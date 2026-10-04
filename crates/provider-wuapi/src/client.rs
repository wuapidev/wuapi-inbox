//! Access to the wuapi endpoints the adapter uses, through the generated
//! SDK (the `wuapi` crate).
//!
//! One method per endpoint, each returning the SDK's types with the SDK's
//! error already translated. No mapping to the neutral model happens here;
//! see `mapping.rs`.
//!
//! The SDK is configured to fit the provider contract:
//!
//! * every attempt is bounded by `request_timeout` (and the connection by
//!   `connect_timeout`, through the shared reqwest client);
//! * the SDK's own retries are off. A provider does not retry: the sync
//!   engine does, with its own backoff, and it must hear about a failure
//!   right away to keep the order of a chat's messages.

use crate::availability::Missing;
use crate::compat;
use crate::config::{ApiKey, WuapiConfig};
use crate::error::{from_response, from_sdk};
use crate::http::{origin, ApiRequest, ReqwestTransport, Transport};
use crate::identity::AuthContext;
use crate::mapping::CLIENT_ID_KEY;
use client_provider::{ChatChange, MediaData, MediaLimit, ProviderError, ProviderResult};
use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use wuapi::types as api;
use wuapi::{RetryPolicy, Wuapi};

/// The largest page wuapi serves.
pub(crate) const MAX_PAGE: u32 = 100;

/// A guard against a listing that never ends: 50 pages.
const MAX_ITEMS: usize = 50 * MAX_PAGE as usize;

/// The longest wait between two attempts at fetching a file, whatever
/// `Retry-After` says: the caller has its own, longer retry.
const MAX_MEDIA_WAIT: Duration = Duration::from_secs(10);

/// What a message answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Quoted<'a> {
    /// Another message of the chat.
    Message(&'a str),
    /// A story of the person the chat is with.
    Story(&'a str),
}

/// What the API holds a file for, and fetches from WhatsApp the first
/// time it is asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Held<'a> {
    /// A message's file.
    Message(&'a str),
    /// A story's file, of this account.
    Story {
        /// The account.
        account: &'a str,
        /// The story.
        id: &'a str,
    },
    /// A favorite sticker's file, of this account.
    Favorite {
        /// The account.
        account: &'a str,
        /// The favorite.
        id: &'a str,
    },
}

/// Where a file is, as the API answered.
pub(crate) struct FileAt {
    /// The URL to download it from.
    pub(crate) url: String,
    /// Its size in bytes, when the API says.
    pub(crate) size: Option<i64>,
}

pub(crate) struct WuapiClient {
    api: Wuapi,
    /// What this deployment, or one of its numbers, does not have yet.
    pub(crate) missing: Missing,
    /// Favorite stickers whose file WhatsApp no longer has, by account
    /// and id: asked for once.
    gone_files: Mutex<std::collections::HashSet<String>>,
    /// For media bytes, which are not JSON and not always on the API host.
    transport: ReqwestTransport,
    base: String,
    key: ApiKey,
    media_timeout: Duration,
    media_backoff: Vec<Duration>,
    upload_timeout: Duration,
    /// TEMPORARY(uploads-rollout): whether the upload routes exist here,
    /// and when that was learned.
    pub(crate) uploads_state: Mutex<Option<(bool, Instant)>>,
}

impl WuapiClient {
    /// Sets up the SDK. Does not touch the network.
    pub(crate) fn new(config: &WuapiConfig, key: ApiKey) -> ProviderResult<Self> {
        if key.expose().trim().is_empty() {
            return Err(ProviderError::Unauthorized("there is no API key".into()));
        }
        let transport = ReqwestTransport::new(config)?;
        // TODO(wuapi-sdk): the SDK sends its own `User-Agent` on every
        // request, so `config.user_agent` only reaches the API on the login
        // and media requests.
        let api = Wuapi::builder()
            .api_key(key.expose())
            .base_url(config.base())
            .timeout(config.request_timeout)
            .retry(RetryPolicy::none())
            .http_client(transport.client())
            .build()
            .map_err(from_sdk)?;
        Ok(Self {
            api,
            missing: Missing::default(),
            gone_files: Mutex::default(),
            transport,
            base: config.base().to_owned(),
            key,
            media_timeout: config.media_timeout,
            media_backoff: config.media_backoff.clone(),
            upload_timeout: config.upload_timeout,
            uploads_state: Mutex::new(None),
        })
    }

    /// The SDK, for the modules that add calls of their own.
    pub(crate) fn sdk(&self) -> &Wuapi {
        &self.api
    }

    /// The HTTP client the SDK shares, for what the SDK does not cover.
    pub(crate) fn http(&self) -> reqwest::Client {
        self.transport.client()
    }

    pub(crate) fn media_timeout(&self) -> Duration {
        self.media_timeout
    }

    pub(crate) fn upload_timeout(&self) -> Duration {
        self.upload_timeout
    }

    /// The API's root, without a trailing slash.
    pub(crate) fn base_url(&self) -> &str {
        &self.base
    }

    /// `GET /v1/me`
    pub(crate) async fn me(&self) -> ProviderResult<AuthContext> {
        let context = self.api.me().await.map_err(from_sdk)?;
        Ok(context.into())
    }

    /// `GET /v1/accounts`, every page.
    pub(crate) async fn accounts(&self) -> ProviderResult<Vec<api::Account>> {
        compat::accounts(self.api.http(), i64::from(MAX_PAGE))
            .to_vec_max(MAX_ITEMS)
            .await
            .map_err(from_sdk)
    }

    /// `GET /v1/proxy-locations`, every page: where a number's exit can be.
    pub(crate) async fn proxy_locations(&self) -> ProviderResult<Vec<api::ProxyLocationItem>> {
        let params = api::ProxyLocationsListParams {
            limit: Some(i64::from(MAX_PAGE)),
            ..Default::default()
        };
        self.api
            .proxy_locations()
            .list(params)
            .to_vec_max(MAX_ITEMS)
            .await
            .map_err(from_sdk)
    }

    /// `POST /v1/accounts`. `request_id` is sent as the idempotency key,
    /// so the same attempt sent twice creates one account.
    pub(crate) async fn create_account(
        &self,
        params: api::AccountsCreateParams,
        request_id: &str,
    ) -> ProviderResult<api::Account> {
        compat::create_account(self.api.http(), &params)
            .idempotency_key(request_id)
            .await
            .map(|account| account.0)
            .map_err(from_sdk)
    }

    /// `GET /v1/accounts/{accountId}`
    pub(crate) async fn account(&self, account: &str) -> ProviderResult<api::Account> {
        compat::account(self.api.http(), account)
            .await
            .map(|account| account.0)
            .map_err(from_sdk)
    }

    /// `PATCH /v1/accounts/{accountId}`
    pub(crate) async fn update_account(
        &self,
        account: &str,
        params: api::AccountsUpdateParams,
    ) -> ProviderResult<api::Account> {
        compat::update_account(self.api.http(), account, &params)
            .await
            .map(|account| account.0)
            .map_err(from_sdk)
    }

    /// `POST /v1/accounts/{accountId}/reconnect`
    pub(crate) async fn reconnect_account(&self, account: &str) -> ProviderResult<api::Account> {
        compat::reconnect(self.api.http(), account)
            .await
            .map(|account| account.0)
            .map_err(from_sdk)
    }

    /// `POST /v1/accounts/{accountId}/logout`
    pub(crate) async fn logout_account(&self, account: &str) -> ProviderResult<api::Account> {
        compat::logout(self.api.http(), account)
            .await
            .map(|account| account.0)
            .map_err(from_sdk)
    }

    /// `POST /v1/accounts/{accountId}/pairing-code`
    pub(crate) async fn pairing_code(&self, account: &str, phone: &str) -> ProviderResult<()> {
        self.api
            .accounts()
            .create_pairing_code(account, api::PairingCodeRequest::new(phone))
            .await
            .map(|_| ())
            .map_err(from_sdk)
    }

    /// `DELETE /v1/accounts/{accountId}`. Already gone is done.
    pub(crate) async fn delete_account(&self, account: &str) -> ProviderResult<()> {
        match self.api.accounts().delete(account).await.map_err(from_sdk) {
            Err(ProviderError::Rejected { code, .. }) if code == "not_found" => Ok(()),
            other => other,
        }
    }

    /// `GET /v1/accounts/{accountId}/chats`: one page, the chat with the
    /// newest message first.
    ///
    /// No filter is sent: the client wants every chat. A page may be
    /// shorter than `limit` while `nextCursor` is still set (the API says
    /// so for combined filters), so callers follow the cursor until it is
    /// `null` and never stop at a short page.
    pub(crate) async fn chats(
        &self,
        account: &str,
        cursor: Option<&str>,
        limit: u32,
    ) -> ProviderResult<api::ChatList> {
        compat::chats(
            self.api.http(),
            account,
            i64::from(limit.clamp(1, MAX_PAGE)),
            cursor.map(str::to_owned),
        )
        .page()
        .await
        .map(|page| page.0)
        .map_err(from_sdk)
    }

    /// `GET /v1/accounts/{accountId}/chats/{chatId}`: one chat as it stands
    /// now, with the provider's own unread count, pin, mute and archive.
    pub(crate) async fn chat(&self, account: &str, chat: &str) -> ProviderResult<api::Chat> {
        compat::chat(self.api.http(), account, chat)
            .await
            .map(|chat| chat.0)
            .map_err(from_sdk)
    }

    /// `GET /v1/accounts/{accountId}/contacts`: one page of the address
    /// book the phone synced to wuapi, by saved name. Reads stored rows:
    /// cheap, and the account need not be `ready`.
    pub(crate) async fn contacts(
        &self,
        account: &str,
        cursor: Option<&str>,
    ) -> ProviderResult<api::ContactList> {
        let params = api::ContactsListParams {
            limit: Some(i64::from(MAX_PAGE)),
            cursor: cursor.map(str::to_owned),
            ..Default::default()
        };
        self.api
            .contacts()
            .list(account, params)
            .page()
            .await
            .map_err(from_sdk)
    }

    /// `POST /v1/accounts/{accountId}/contacts/check`: which of up to 50
    /// numbers have WhatsApp.
    pub(crate) async fn check_numbers(
        &self,
        account: &str,
        phones: Vec<String>,
    ) -> ProviderResult<Vec<api::ContactCheck>> {
        let list = self
            .api
            .contacts()
            .check(account, api::ContactCheckRequest::new(phones))
            .await
            .map_err(from_sdk)?;
        Ok(list.items)
    }

    /// `GET /v1/messages`: one page, newest first.
    ///
    /// TODO(wuapi-api): the listing has no `since` / `after` filter, so
    /// "what is new" can only be answered by re-reading the newest pages.
    pub(crate) async fn messages(
        &self,
        account: Option<&str>,
        chat: Option<&str>,
        cursor: Option<&str>,
        limit: u32,
    ) -> ProviderResult<api::MessageList> {
        compat::messages(
            self.api.http(),
            account,
            chat,
            i64::from(limit.clamp(1, MAX_PAGE)),
            cursor.map(str::to_owned),
        )
        .page()
        .await
        .map(|page| page.0)
        .map_err(from_sdk)
    }

    /// `GET /v1/messages/{messageId}`: one message as it stands now.
    pub(crate) async fn message(&self, id: &str) -> ProviderResult<api::Message> {
        compat::message(self.api.http(), id)
            .await
            .map(|message| message.0)
            .map_err(from_sdk)
    }

    /// `POST /v1/messages` with a text body.
    ///
    /// `client_id` travels twice: as the `Idempotency-Key` header, which
    /// makes wuapi replay its first answer instead of sending again, and in
    /// `metadata`, which wuapi returns on every copy of the message so the
    /// client can recognise its own.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send_text(
        &self,
        account: &str,
        to: &str,
        text: &str,
        reply_to: Option<Quoted<'_>>,
        mentions: &[String],
        forwarded: bool,
        client_id: &str,
    ) -> ProviderResult<api::Message> {
        let mut request = api::SendTextMessageRequest::new(account, to, text);
        request.r#type = Some("text".to_owned());
        match reply_to {
            // `replyToStoryId`: the message goes to the story's author and
            // quotes the story.
            Some(Quoted::Story(story)) => request.reply_to_story_id = Some(story.to_owned()),
            Some(Quoted::Message(message)) => {
                request.reply_to_message_id = Some(message.to_owned());
            }
            None => {}
        }
        // `forwarded`: WhatsApp's "Forwarded" mark on what is passed on.
        request.forwarded = forwarded.then_some(true);
        // `mentions`: contact ids, at most 256.
        if !mentions.is_empty() {
            request.mentions = Some(mentions.iter().take(256).cloned().collect());
        }
        request.metadata = Some(BTreeMap::from([(
            CLIENT_ID_KEY.to_owned(),
            client_id.to_owned(),
        )]));
        compat::send(self.api.http(), request)
            .idempotency_key(client_id)
            .await
            .map(|message| message.0)
            .map_err(from_sdk)
    }

    /// `POST /v1/messages` with a poll, sent like a text: the client id is
    /// the `Idempotency-Key` and rides in `metadata`.
    pub(crate) async fn send_poll(
        &self,
        account: &str,
        to: &str,
        poll: api::SendPoll,
        reply_to: Option<&str>,
        client_id: &str,
    ) -> ProviderResult<api::Message> {
        let mut request = api::SendPollMessageRequest::new(account, to, poll);
        request.reply_to_message_id = reply_to.map(str::to_owned);
        request.metadata = Some(BTreeMap::from([(
            CLIENT_ID_KEY.to_owned(),
            client_id.to_owned(),
        )]));
        compat::send(self.api.http(), request)
            .idempotency_key(client_id)
            .await
            .map(|message| message.0)
            .map_err(from_sdk)
    }

    /// `POST /v1/messages/{messageId}/vote`: the account's vote, as the
    /// names of every option it stands for (none retracts it). Answers the
    /// poll with its tally. A vote sets a value, so repeating it is
    /// harmless; the SDK adds an `Idempotency-Key` all the same.
    pub(crate) async fn vote(
        &self,
        message: &str,
        options: &[String],
    ) -> ProviderResult<api::Message> {
        compat::vote(
            self.api.http(),
            message,
            &api::VoteRequest::new(options.to_vec()),
        )
        .await
        .map(|message| message.0)
        .map_err(from_sdk)
    }

    /// `POST /v1/messages/{messageId}/react`. An empty emoji removes the
    /// reaction.
    pub(crate) async fn react(
        &self,
        message: &str,
        emoji: &str,
        client_id: &str,
    ) -> ProviderResult<()> {
        self.api
            .messages()
            .react(message, api::ReactRequest::new(emoji))
            .idempotency_key(client_id)
            .await
            .map_err(from_sdk)
    }

    /// `PATCH /v1/messages/{messageId}`: a new text for a message the
    /// account sent, within WhatsApp's edit window (about 15 minutes; its
    /// refusal comes back as it is). A message wuapi still has queued is
    /// sent with the new text instead.
    pub(crate) async fn edit(&self, message: &str, text: &str) -> ProviderResult<()> {
        compat::edit(
            self.api.http(),
            message,
            &api::MessageEditRequest::new(text),
        )
        .await
        .map(drop)
        .map_err(from_sdk)
    }

    /// `DELETE /v1/messages/{messageId}`: an outbound message, for
    /// everyone or (`forEveryone=false`) on the linked devices only.
    pub(crate) async fn delete(&self, message: &str, for_everyone: bool) -> ProviderResult<()> {
        self.api
            .messages()
            .delete(
                message,
                api::MessagesDeleteParams {
                    for_everyone: Some(for_everyone),
                },
            )
            .await
            .map_err(from_sdk)
    }

    /// `POST /v1/messages/{messageId}/star` and `/unstar`.
    pub(crate) async fn star(&self, message: &str, starred: bool) -> ProviderResult<()> {
        compat::star(self.api.http(), message, starred)
            .await
            .map(drop)
            .map_err(from_sdk)
    }

    /// `POST .../chats/{chatId}/read`: read receipts (blue ticks) for every
    /// unread inbound message of the chat.
    pub(crate) async fn send_read_receipts(&self, account: &str, chat: &str) -> ProviderResult<()> {
        self.api
            .chats()
            .send_read_receipts(account, chat, api::ReadReceiptsRequest::default())
            .await
            .map(drop)
            .map_err(from_sdk)
    }

    /// `POST .../chats/{chatId}/mark-read`: clears the unread badge on the
    /// phone and the other devices.
    pub(crate) async fn mark_chat_read(&self, account: &str, chat: &str) -> ProviderResult<()> {
        self.api
            .chats()
            .mark_read(account, chat)
            .await
            .map_err(from_sdk)
    }

    /// `POST .../chats/{chatId}/{pin,unpin,mute,unmute,archive,unarchive,
    /// mark-unread}`. Each one sets a state, so repeating it is harmless;
    /// the SDK adds an `Idempotency-Key` all the same.
    pub(crate) async fn chat_state(
        &self,
        account: &str,
        chat: &str,
        change: ChatChange,
    ) -> ProviderResult<()> {
        let chats = self.api.chats();
        match change {
            ChatChange::Pinned(true) => chats.pin(account, chat).await,
            ChatChange::Pinned(false) => chats.unpin(account, chat).await,
            // No duration: muted until it is unmuted.
            ChatChange::Muted(true) => chats.mute(account, chat, api::MuteRequest::default()).await,
            ChatChange::MutedFor(seconds) => {
                let request = api::MuteRequest {
                    duration_seconds: Some(seconds.min(i64::MAX as u64) as i64),
                };
                chats.mute(account, chat, request).await
            }
            ChatChange::Muted(false) => chats.unmute(account, chat).await,
            ChatChange::Archived(true) => chats.archive(account, chat).await,
            ChatChange::Archived(false) => chats.unarchive(account, chat).await,
            ChatChange::MarkedUnread => chats.mark_unread(account, chat).await,
        }
        .map_err(from_sdk)
    }

    /// `GET /v1/accounts/{accountId}/groups`, every page. Read live from
    /// WhatsApp, so it fails while the account is offline.
    pub(crate) async fn groups(&self, account: &str) -> ProviderResult<Vec<api::Group>> {
        let params = api::GroupsListParams {
            limit: Some(i64::from(MAX_PAGE)),
            ..Default::default()
        };
        self.api
            .groups()
            .list(account, params)
            .to_vec_max(MAX_ITEMS)
            .await
            .map_err(from_sdk)
    }

    /// `GET .../contacts/{contactId}/picture?preview=true`: the download
    /// picture of a contact (or, on the same path, of a group).
    pub(crate) async fn contact_picture(
        &self,
        account: &str,
        contact: &str,
    ) -> ProviderResult<api::Picture> {
        let params = api::ContactsGetPictureParams {
            preview: Some(true),
        };
        self.api
            .contacts()
            .get_picture(account, contact, params)
            .await
            .map_err(from_sdk)
    }

    /// Where a file the API holds for something is, asked for JSON:
    /// `GET /v1/messages/{messageId}/media`, `GET …/stories/{storyId}/media`
    /// or `GET …/stickers/favorites/{stickerId}/media`, each with
    /// `redirect=false`. A file still on WhatsApp is downloaded by the API
    /// first, once, through the number's proxy. None of these marks
    /// anything as read or seen.
    ///
    /// That download is the slow, fragile part, so each attempt has its
    /// own longer deadline and a failure that may pass (a timeout, a
    /// dropped connection, a 5xx, a number that is reconnecting) is tried
    /// again a couple of times, further apart each time. Repeating is
    /// safe: the API keeps the file after the first download. What is
    /// still failing after that is reported as transient, and the caller
    /// tries again later. Only an answer that will not change is
    /// terminal: `410 media_expired`, when WhatsApp no longer has the file.
    ///
    /// A favorite sticker stays listed after its file is gone, and the
    /// client asks for the file of every favorite it does not hold: what
    /// answered `media_expired` is remembered and not asked for again.
    pub(crate) async fn media_file(&self, held: Held<'_>) -> ProviderResult<FileAt> {
        let expired = || ProviderError::Rejected {
            code: "media_expired".into(),
            message: "WhatsApp no longer has this file.".into(),
        };
        let remembered = match held {
            Held::Favorite { account, id } => Some(format!("{account}/{id}")),
            _ => None,
        };
        if let Some(key) = &remembered {
            if self.gone_files.lock().expect("gone lock").contains(key) {
                return Err(expired());
            }
        }
        let mut waits = self.media_backoff.iter();
        loop {
            let answer = match held {
                Held::Message(id) => self
                    .api
                    .messages()
                    .get_media(
                        id,
                        api::MessagesGetMediaParams {
                            redirect: Some(false),
                        },
                    )
                    .timeout(self.media_timeout)
                    .await
                    .map(|file| FileAt {
                        url: file.url,
                        size: file.size,
                    }),
                Held::Story { account, id } => self
                    .api
                    .stories()
                    .get_media(
                        account,
                        id,
                        api::StoriesGetMediaParams {
                            redirect: Some(false),
                        },
                    )
                    .timeout(self.media_timeout)
                    .await
                    .map(|file| FileAt {
                        url: file.url,
                        size: file.size,
                    }),
                Held::Favorite { account, id } => self
                    .api
                    .favorite_stickers()
                    .get_media(
                        account,
                        id,
                        api::FavoriteStickersGetMediaParams {
                            redirect: Some(false),
                        },
                    )
                    .timeout(self.media_timeout)
                    .await
                    .map(|file| FileAt {
                        url: file.url,
                        size: file.size,
                    }),
            }
            .map_err(from_sdk);
            let error = match answer {
                Ok(file) => return Ok(file),
                Err(error) => error,
            };
            if let ProviderError::Rejected { code, .. } = &error {
                if code == "media_expired" {
                    if let Some(key) = remembered {
                        self.gone_files.lock().expect("gone lock").insert(key);
                    }
                    return Err(expired());
                }
            }
            if !error.is_transient() {
                return Err(error);
            }
            let Some(wait) = waits.next() else {
                return Err(error);
            };
            tokio::time::sleep(error.retry_after().unwrap_or(*wait).min(MAX_MEDIA_WAIT)).await;
        }
    }

    /// Fetches a file that is still on WhatsApp: asks the API where it is
    /// (with the key, on the API host), then downloads from the URL it
    /// answers (without the key, unless that URL is on the API host too).
    /// `size` is what the message, the story or the favorite said, when
    /// it said: a file over the limit is refused before the API is asked
    /// to fetch it.
    pub(crate) async fn download_on_demand(
        &self,
        held: Held<'_>,
        size: Option<u64>,
        limit: MediaLimit,
        progress: Option<&client_provider::UploadProgress>,
    ) -> ProviderResult<MediaData> {
        let too_large = || ProviderError::Rejected {
            code: "too_large".into(),
            message: "The file is larger than the download limit.".into(),
        };
        if size.is_some_and(|size| size > limit.max_bytes) {
            return Err(too_large());
        }
        let file = self.media_file(held).await?;
        if file
            .size
            .and_then(|size| u64::try_from(size).ok())
            .is_some_and(|size| size > limit.max_bytes)
        {
            return Err(too_large());
        }
        self.download_reporting(&file.url, limit, progress).await
    }

    /// Downloads a media URL within a limit, refusing early what is too
    /// large or (when only images are wanted) not an image. The API key
    /// goes along only to the API's own host, as in [`download`](Self::download).
    pub(crate) async fn download_limited(
        &self,
        url: &str,
        limit: MediaLimit,
    ) -> ProviderResult<MediaData> {
        self.download_reporting(url, limit, None).await
    }

    /// [`download_limited`](Self::download_limited), saying how many
    /// bytes have arrived.
    pub(crate) async fn download_reporting(
        &self,
        url: &str,
        limit: MediaLimit,
        progress: Option<&client_provider::UploadProgress>,
    ) -> ProviderResult<MediaData> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(ProviderError::Rejected {
                code: "invalid_media".into(),
                message: "The media reference is not a URL.".into(),
            });
        }
        let own = origin(url).is_some() && origin(url) == origin(&self.base);
        self.transport
            .fetch_limited(url, own.then_some(&self.key), limit, progress)
            .await
    }

    /// Downloads a media URL. The API key goes along only when the URL
    /// points at the API itself; media hosted elsewhere never sees it.
    ///
    /// Not an SDK call: the SDK only speaks JSON to the API host.
    pub(crate) async fn download(&self, url: &str) -> ProviderResult<MediaData> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(ProviderError::Rejected {
                code: "invalid_media".into(),
                message: "The media reference is not a URL.".into(),
            });
        }
        let mut request = ApiRequest::get(url);
        if origin(url).is_some() && origin(url) == origin(&self.base) {
            request = request.bearer(&self.key);
        }
        let response = self.transport.execute(request).await?;
        if !response.is_success() {
            return Err(from_response(response));
        }
        Ok(MediaData {
            mime_type: response.content_type,
            bytes: response.body,
        })
    }
}
