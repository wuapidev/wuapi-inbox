//! [`WuapiProvider`]: the [`Provider`] implementation.

use crate::client::{Held, Quoted, WuapiClient, MAX_PAGE};
use crate::config::{ApiKey, LiveTransport, WuapiConfig};
use crate::events::{EventSource, PollingEventSource};
use crate::identity::AuthContext;
use crate::live::LiveEventSource;
use crate::mapping::{self, OnDemand, PICTURE_PREFIX};
use crate::stream::{ReqwestStreamTransport, StreamEventSource, StreamTransport};
use crate::uploads::MediaSend;
use async_trait::async_trait;
use client_provider::{
    Account, AccountChange, AccountId, Avatar, AvatarAnswer, Capabilities, Chat, ChatChange,
    ChatId, Contact, Cursor, DeliveryStatus, EventStream, Feature, HistoryImport, LinkPlace,
    LinkStatus, LiveUpdates, MediaData, MediaLimit, MediaRef, Message, MessageId, NewAccount,
    NumberCheck, OutgoingContent, OutgoingMessage, Page, PollingReason, Provider, ProviderError,
    ProviderResult, SendReceipt, Timestamp,
};
use client_provider::{
    BusinessProfile, ContactId, Group, GroupChange, JoinRequest, NewGroup, OwnProfile,
    ParticipantChange, ParticipantOutcome, ProfileChange,
};
use std::sync::Arc;
use wuapi::types as api;

/// Error codes of the picture endpoint that mean "nothing to show": no
/// picture or hidden by privacy, a contact WhatsApp does not know, a chat
/// the account may not look at, an id the route does not take.
const NO_PICTURE: [&str; 6] = [
    "picture_not_found",
    "whatsapp_not_found",
    "whatsapp_forbidden",
    "not_found",
    "not_supported",
    "invalid_request",
];

/// A preview is a few kilobytes; nothing calling itself a profile picture
/// needs more than this.
const AVATAR_LIMIT: MediaLimit = MediaLimit {
    max_bytes: 2 * 1024 * 1024,
    images_only: true,
};

/// The wuapi adapter.
///
/// Holds an API key and the wuapi SDK's client; everything else is stateless, as
/// the [`Provider`] contract asks.
pub struct WuapiProvider {
    client: Arc<WuapiClient>,
    events: Arc<dyn EventSource>,
    /// What the stream report connects with.
    pub(crate) config: WuapiConfig,
    pub(crate) key: ApiKey,
}

impl WuapiProvider {
    /// Creates the provider. Does not touch the network.
    pub fn new(config: WuapiConfig, api_key: ApiKey) -> ProviderResult<Self> {
        let client = Arc::new(WuapiClient::new(&config, api_key.clone())?);
        let kept = api_key.clone();
        let polling = PollingEventSource::new(client.clone(), config.poll_interval);
        let events: Arc<dyn EventSource> = match config.live {
            // Exactly as before the stream: no stream code is built.
            LiveTransport::Polling => Arc::new(polling),
            // The stream or nothing: an address that cannot be used is an
            // error now, not a silent switch to polling.
            LiveTransport::Stream => Arc::new(StreamEventSource::new(
                client.clone(),
                Arc::new(ReqwestStreamTransport::new(&config, api_key)?),
                config.tuning.clone(),
            )),
            // An address that cannot be used leaves polling, with a reason.
            LiveTransport::Auto => {
                let transport = match ReqwestStreamTransport::new(&config, api_key) {
                    Ok(transport) => Some(Arc::new(transport) as Arc<dyn StreamTransport>),
                    Err(error) => {
                        tracing::info!(%error, "no usable event stream address; polling");
                        None
                    }
                };
                Arc::new(LiveEventSource::new(
                    client.clone(),
                    polling,
                    transport,
                    config.tuning.clone(),
                ))
            }
        };
        Ok(Self {
            client,
            events,
            config,
            key: kept,
        })
    }

    /// The provider together with its polling event source, so a test
    /// can step the poller by hand.
    #[cfg(test)]
    pub(crate) fn with_polling(
        config: WuapiConfig,
        api_key: ApiKey,
    ) -> ProviderResult<(Self, Arc<PollingEventSource>)> {
        let client = Arc::new(WuapiClient::new(&config, api_key.clone())?);
        let events = Arc::new(PollingEventSource::new(
            client.clone(),
            config.poll_interval,
        ));
        Ok((
            Self {
                events: events.clone(),
                client,
                config,
                key: api_key,
            },
            events,
        ))
    }

    /// The provider with the strict stream source over a given transport,
    /// for the tests of the stream.
    #[cfg(test)]
    pub(crate) fn with_stream(
        config: WuapiConfig,
        api_key: ApiKey,
        transport: Arc<dyn crate::stream::StreamTransport>,
    ) -> ProviderResult<Self> {
        let client = Arc::new(WuapiClient::new(&config, api_key.clone())?);
        Ok(Self {
            events: Arc::new(crate::stream::StreamEventSource::new(
                client.clone(),
                transport,
                config.tuning.clone(),
            )),
            client,
            config,
            key: api_key,
        })
    }

    /// The provider with the `Auto` source over a given transport, and that
    /// source, for the tests of the live driver.
    #[cfg(test)]
    pub(crate) fn with_live(
        config: WuapiConfig,
        api_key: ApiKey,
        transport: Arc<dyn crate::stream::StreamTransport>,
    ) -> ProviderResult<(Self, Arc<crate::live::LiveEventSource>)> {
        let client = Arc::new(WuapiClient::new(&config, api_key.clone())?);
        let polling = PollingEventSource::new(client.clone(), config.poll_interval);
        let live = Arc::new(crate::live::LiveEventSource::new(
            client.clone(),
            polling,
            Some(transport),
            config.tuning.clone(),
        ));
        Ok((
            Self {
                events: live.clone(),
                client,
                config,
                key: api_key,
            },
            live,
        ))
    }

    /// The client under the adapter, for the diagnosis.
    pub(crate) fn client(&self) -> &WuapiClient {
        &self.client
    }

    /// The API this provider talks to.
    pub fn base_url(&self) -> &str {
        self.client.base_url()
    }

    /// Who the API key belongs to (`GET /v1/me`). Useful to check a key and
    /// to label the login in the UI.
    pub async fn me(&self) -> ProviderResult<AuthContext> {
        self.client.me().await
    }

    /// What a media ref that names something the API holds a file for
    /// stands for, with the account it belongs to.
    fn held<'a>(account: &'a AccountId, named: OnDemand<'a>) -> Held<'a> {
        match named {
            OnDemand::Message(id) => Held::Message(id),
            OnDemand::Story(id) => Held::Story {
                account: account.as_str(),
                id,
            },
            OnDemand::Favorite(id) => Held::Favorite {
                account: account.as_str(),
                id,
            },
        }
    }

    /// The receipt of a message the API just accepted, which is followed
    /// from here on.
    fn receipt(&self, sent: &api::Message, what: &str) -> ProviderResult<SendReceipt> {
        let mapped = mapping::message(sent).ok_or_else(|| {
            ProviderError::Protocol(format!("the sent {what} came back unreadable"))
        })?;
        self.events.accepted(sent);
        Ok(SendReceipt {
            message_id: mapped.id,
            status: mapped.status,
            timestamp: Some(mapped.timestamp),
        })
    }
}

fn mention_ids(message: &OutgoingMessage) -> Vec<String> {
    message
        .mentions
        .iter()
        .map(|mention| mention.id.to_string())
        .collect()
}

fn not_uploaded() -> ProviderError {
    ProviderError::Rejected {
        code: "invalid_media".into(),
        message: "The file was not uploaded first.".into(),
    }
}

#[async_trait]
impl Provider for WuapiProvider {
    fn id(&self) -> &'static str {
        "wuapi"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // `GET /v1/accounts/{accountId}/chats`: every chat, with
            // wuapi's unread state, pins, mutes, the archive and the names
            // saved in the phone's address book.
            chat_list: true,
            // Pin, mute, archive and mark-unread endpoints, and a direct
            // chat's id is the contact's number, so one can be started.
            chat_state: true,
            start_chat: true,
            // `POST /v1/accounts`, by QR code or by pairing code, and the
            // rename, reconnect, logout and delete routes.
            link_accounts: true,
            link_by_code: true,
            // TODO(wuapi-api): the API has no way back. Once
            // `POST .../pairing-code` was asked for, the account keeps its
            // `pairingPhone` and hides `qrCodeUrl` until it links or is
            // logged out, and `reconnect` asks for another code. Logging
            // the number out and starting it again would imitate a way
            // back with a second session: not done.
            link_back_to_scan: false,
            manage_accounts: true,
            // `historySync`, per account, applied at the next link.
            history_import: true,
            // `GET /v1/accounts/{accountId}/contacts` and
            // `POST .../contacts/check`.
            contacts: true,
            number_check: true,
            contact_names: true,
            groups: true,
            // The event stream pushes; only `Polling` (chosen) polls alone.
            // `Auto` counts as push while it falls back, because it goes
            // back to the stream by itself.
            realtime_push: self.config.live != LiveTransport::Polling,
            // TODO(wuapi-api): GET /v1/messages has no since/after filter.
            incremental_sync: false,
            replies: true,
            reactions: true,
            // `PATCH /v1/messages/{messageId}` and `DELETE` (for everyone,
            // or with `forEveryone=false` on the linked devices only):
            // the account's own messages. The API has no way to delete a
            // message somebody else sent.
            edits: true,
            deletes: true,
            delete_for_me: true,
            delete_received: false,
            // `POST /v1/messages/{messageId}/star` and `/unstar`.
            stars: true,
            // `forwarded` on `POST /v1/messages` labels a new message; a
            // stored one is passed on by naming it (`forward_any`).
            forwards: true,
            // `POST /v1/messages` with `type: poll`, and
            // `POST /v1/messages/{messageId}/vote`.
            polls: true,
            poll_votes: true,
            media_download: true,
            // Contacts for sure; groups through the same path (see
            // `fetch_avatar`), and the group icon stays where that fails.
            avatars: true,
            // `POST /v1/uploads` and `media: {uploadId}`. Whether this
            // deployment has them yet is `media_upload_ready`.
            media_upload: true,
            read_receipts: true,
            // `mark-read` without `read`: the badge goes, no blue ticks.
            quiet_read: true,
            presence: false,
            // `POST …/contacts/lookup`, `GET …/business-profile`, the
            // block, unblock and blocklist routes.
            contact_lookup: true,
            business_profiles: true,
            blocking: true,
            // `PATCH …/profile` and `PUT`/`DELETE …/profile/picture`.
            profile_edit: true,
            // The groups routes: read, create, manage, invite link, join
            // requests, leave.
            group_info: true,
            group_create: true,
            group_manage: true,
            group_invites: true,
            group_join_requests: true,
            group_leave: true,
            // `POST`/`DELETE …/groups/{groupId}/subgroups`, `community` and
            // `communityId` on `POST …/groups`, and
            // `GET …/groups/{groupId}/community-participants`.
            community_manage: true,
            community_members: true,
            // `mentions` on `POST /v1/messages`: contact ids.
            mentions: true,
            // `GET/POST/DELETE …/stickers/favorites`. Turned on in the
            // engine one number at a time: where it is off, `unavailable`
            // says so once a change was refused.
            sticker_favorites: true,
            // `POST /v1/messages/{messageId}/forward`. Polls and calendar
            // events answer `400 not_forwardable`, so they are not offered.
            forward_any: true,
            forward_polls: false,
            forward_events: false,
            // `POST …/stories`, `GET …/stories/own` (the messages of the
            // chat `stories` on a deployment without it), `DELETE
            // /v1/messages/{id}`, `GET …/privacy/stories`.
            story_list: true,
            story_post: true,
            story_delete: true,
            story_privacy: true,
            // `GET …/stories`, for a number that has stories turned on in
            // the engine (an empty list until then), and their files on
            // demand.
            story_contacts: true,
            // `POST …/stories/{id}/view`: the receipt, and only that call.
            story_view: true,
            // `GET …/stories/{id}/viewers`, and `viewCount` on the own.
            story_viewers: true,
            // `replyToStoryId` on `POST /v1/messages`.
            story_reply: true,
            // `POST …/stories/{id}/react`.
            story_react: true,
            // TODO(wuapi-api: stories mute write): `story_group.muted` is
            // readable (the poll passes a mute made on the phone on), but
            // no route mutes or unmutes an author, and this flag means
            // both. Mutes made here stay on this computer.
            story_mute: false,
            // TODO(wuapi-api: stories privacy edit): only the setting
            // (`stories` in `PATCH …/privacy`), not its lists.
            story_privacy_edit: false,
        }
    }

    fn live_updates(&self) -> Option<LiveUpdates> {
        match self.events.live() {
            Some(status) => Some(status.into()),
            None if self.config.live == LiveTransport::Polling => {
                Some(LiveUpdates::Polling(PollingReason::Chosen))
            }
            None => None,
        }
    }

    fn unavailable(&self, account: &AccountId, feature: Feature) -> bool {
        self.client.missing.is(feature, account.as_str())
    }

    fn recheck(&self, account: &AccountId, feature: Feature) {
        self.client.missing.forget(feature, account.as_str());
    }

    async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
        let accounts = self.client.accounts().await?;
        Ok(accounts.iter().map(mapping::account).collect())
    }

    async fn list_chats(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Chat>> {
        let page = self
            .client
            .chats(
                account.as_str(),
                cursor.as_ref().map(Cursor::as_str),
                MAX_PAGE,
            )
            .await?;
        // The API's cursor is passed through: the engine follows it until
        // it is `None`, however short a page is.
        Ok(Page {
            items: page.items.iter().filter_map(mapping::chat).collect(),
            next_cursor: page.next_cursor.map(Cursor::new),
        })
    }

    async fn fetch_messages(
        &self,
        account: &AccountId,
        chat: &ChatId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> ProviderResult<Page<Message>> {
        let page = self
            .client
            .messages(
                Some(account.as_str()),
                Some(chat.as_str()),
                cursor.as_ref().map(Cursor::as_str),
                limit,
            )
            .await?;
        // The account's own messages of this page whose ticks can still
        // move are worth asking about again.
        self.events.listed(&page.items);
        Ok(Page {
            items: page.items.iter().filter_map(mapping::message).collect(),
            next_cursor: page.next_cursor.map(Cursor::new),
        })
    }

    async fn edit_message(
        &self,
        _account: &AccountId,
        _chat: &ChatId,
        message: &MessageId,
        text: &str,
    ) -> ProviderResult<()> {
        self.client.edit(message.as_str(), text).await
    }

    async fn delete_message(
        &self,
        _account: &AccountId,
        _chat: &ChatId,
        message: &MessageId,
        for_everyone: bool,
    ) -> ProviderResult<()> {
        self.client.delete(message.as_str(), for_everyone).await
    }

    async fn star_message(
        &self,
        _account: &AccountId,
        _chat: &ChatId,
        message: &MessageId,
        starred: bool,
    ) -> ProviderResult<()> {
        self.client.star(message.as_str(), starred).await
    }

    async fn send(&self, message: OutgoingMessage) -> ProviderResult<SendReceipt> {
        let client_id = message.client_id.as_str();
        match &message.content {
            OutgoingContent::Text { body } => {
                let sent = self
                    .client
                    .send_text(
                        message.account_id.as_str(),
                        message.chat_id.as_str(),
                        body,
                        message
                            .reply_to
                            .as_ref()
                            .map(|quoted| Quoted::Message(quoted.as_str())),
                        &mention_ids(&message),
                        message.forwarded,
                        client_id,
                    )
                    .await?;
                self.receipt(&sent, "message")
            }
            OutgoingContent::Reaction { target, emoji } => {
                self.client.react(target.as_str(), emoji, client_id).await?;
                // The endpoint answers 204 without a message, so the
                // receipt's id is derived from the client id: the same on
                // every retry, as the contract requires.
                Ok(SendReceipt {
                    message_id: MessageId::new(format!("reaction:{client_id}")),
                    status: DeliveryStatus::Sent,
                    timestamp: Some(Timestamp::now()),
                })
            }
            OutgoingContent::Media {
                kind,
                media,
                mime_type,
                caption,
                file_name,
                gif,
            } => {
                let upload_id = media
                    .as_str()
                    .strip_prefix(crate::uploads::UPLOAD_PREFIX)
                    .ok_or_else(not_uploaded)?;
                let sent = self
                    .client
                    .send_media(MediaSend {
                        account: message.account_id.as_str(),
                        to: message.chat_id.as_str(),
                        kind: *kind,
                        upload_id,
                        mime_type: mime_type.as_deref(),
                        file_name: file_name.as_deref(),
                        caption: caption.as_deref(),
                        reply_to: message.reply_to.as_ref().map(MessageId::as_str),
                        mentions: &mention_ids(&message),
                        gif: *gif,
                        client_id,
                    })
                    .await?;
                self.receipt(&sent, "file")
            }
            OutgoingContent::Forward { .. } => Err(ProviderError::Rejected {
                code: "invalid_request".into(),
                message: "A forward goes through forward_messages.".into(),
            }),
            OutgoingContent::Poll {
                question,
                options,
                max_choices,
            } => {
                let mut poll = api::SendPoll::new(question.clone(), options.clone());
                poll.selectable_count = Some(i64::from(*max_choices));
                let sent = self
                    .client
                    .send_poll(
                        message.account_id.as_str(),
                        message.chat_id.as_str(),
                        poll,
                        message.reply_to.as_ref().map(MessageId::as_str),
                        client_id,
                    )
                    .await?;
                self.receipt(&sent, "poll")
            }
        }
    }

    async fn forward_messages(
        &self,
        account: &AccountId,
        items: &[client_provider::ForwardItem],
    ) -> ProviderResult<Vec<ProviderResult<SendReceipt>>> {
        crate::forward::forward(&self.client, self.events.as_ref(), account, items).await
    }

    async fn list_favorite_stickers(
        &self,
        account: &AccountId,
    ) -> ProviderResult<Vec<client_provider::FavoriteSticker>> {
        self.client.favorite_stickers(account.as_str()).await
    }

    async fn add_favorite_sticker(
        &self,
        account: &AccountId,
        sticker: client_provider::StickerFile,
    ) -> ProviderResult<String> {
        self.client.star_sticker(account.as_str(), &sticker).await
    }

    async fn remove_favorite_sticker(&self, account: &AccountId, id: &str) -> ProviderResult<()> {
        self.client.unstar_sticker(account.as_str(), id).await
    }

    async fn vote_poll(
        &self,
        _account: &AccountId,
        _chat: &ChatId,
        poll: &MessageId,
        choices: &[String],
    ) -> ProviderResult<Option<Message>> {
        let voted = self.client.vote(poll.as_str(), choices).await?;
        // The answer is the poll with its tally, the account's vote
        // counted in. It does not say which options that vote was.
        Ok(mapping::message(&voted))
    }

    async fn mark_read(
        &self,
        account: &AccountId,
        chat: &ChatId,
        _up_to: Option<&MessageId>,
    ) -> ProviderResult<()> {
        // wuapi marks the whole chat; `up_to` cannot be honoured more
        // precisely without knowing every unread message id. Receipts
        // first (the other side's blue ticks), then the badge on the
        // user's own devices. Both are safe to repeat.
        self.client
            .send_read_receipts(account.as_str(), chat.as_str())
            .await?;
        self.client
            .mark_chat_read(account.as_str(), chat.as_str())
            .await
    }

    async fn mark_read_quietly(&self, account: &AccountId, chat: &ChatId) -> ProviderResult<()> {
        // The badge only: `mark-read` sends no receipts.
        self.client
            .mark_chat_read(account.as_str(), chat.as_str())
            .await
    }

    async fn update_chat(
        &self,
        account: &AccountId,
        chat: &ChatId,
        change: ChatChange,
    ) -> ProviderResult<()> {
        self.client
            .chat_state(account.as_str(), chat.as_str(), change)
            .await
    }

    async fn start_chat(&self, account: &AccountId, phone: &str) -> ProviderResult<Chat> {
        // The id of a direct chat is the contact's number in E.164, so
        // there is nothing to ask the API: the first message creates the
        // chat there. A number without WhatsApp is refused when it is sent.
        let number = mapping::e164(phone).ok_or_else(|| ProviderError::Rejected {
            code: "invalid_phone".into(),
            message: "Enter the number with its country code, like +58 424 555 0199.".into(),
        })?;
        Ok(mapping::new_direct_chat(account, &number))
    }

    async fn list_stories(
        &self,
        account: &AccountId,
    ) -> ProviderResult<Vec<client_provider::Story>> {
        let listed = self
            .client
            .stories(account.as_str(), Timestamp::now())
            .await?;
        Ok(listed.stories)
    }

    async fn post_story(
        &self,
        new: client_provider::NewStory,
    ) -> ProviderResult<client_provider::Story> {
        use client_provider::NewStoryContent as C;
        let client_id = new.client_id.as_str();
        let wire = match &new.content {
            C::Text { text, style } => {
                self.client
                    .post_text_story(new.account_id.as_str(), text, style, client_id)
                    .await?
            }
            C::Media {
                kind,
                media,
                mime_type,
                caption,
            } => {
                let upload_id = media
                    .as_str()
                    .strip_prefix(crate::uploads::UPLOAD_PREFIX)
                    .ok_or_else(not_uploaded)?;
                self.client
                    .post_media_story(
                        new.account_id.as_str(),
                        *kind,
                        upload_id,
                        mime_type.as_deref(),
                        caption.as_deref(),
                        client_id,
                    )
                    .await?
            }
        };
        crate::stories::posted(&wire, &new)
    }

    async fn delete_story(&self, _account: &AccountId, story: &MessageId) -> ProviderResult<()> {
        match self.client.delete(story.as_str(), true).await {
            // Already gone is what was asked for.
            Err(ProviderError::Rejected { code, .. }) if code.contains("not_found") => Ok(()),
            other => other,
        }
    }

    async fn view_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        _author: &ContactId,
    ) -> ProviderResult<()> {
        self.client
            .view_story(account.as_str(), story.as_str())
            .await
    }

    async fn story_viewers(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> ProviderResult<Vec<client_provider::StoryViewer>> {
        self.client
            .story_viewers(account.as_str(), story.as_str())
            .await
    }

    async fn reply_to_story(
        &self,
        story: &client_provider::Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        let OutgoingContent::Text { body } = &message.content else {
            return Err(ProviderError::Rejected {
                code: "invalid_request".into(),
                message: "A story is answered with a text.".into(),
            });
        };
        // An ordinary message for the author's chat that quotes the
        // story, under the client's id like any send.
        let sent = self
            .client
            .send_text(
                message.account_id.as_str(),
                message.chat_id.as_str(),
                body,
                Some(Quoted::Story(story.id.as_str())),
                &mention_ids(&message),
                false,
                message.client_id.as_str(),
            )
            .await?;
        self.receipt(&sent, "reply")
    }

    async fn react_to_story(
        &self,
        story: &client_provider::Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        let OutgoingContent::Reaction { emoji, .. } = &message.content else {
            return Err(ProviderError::Rejected {
                code: "invalid_request".into(),
                message: "A reaction to a story is an emoji.".into(),
            });
        };
        let client_id = message.client_id.as_str();
        self.client
            .react_to_story(
                message.account_id.as_str(),
                story.id.as_str(),
                emoji,
                client_id,
            )
            .await?;
        // 204 without a message: the id is derived from the client id,
        // the same on every retry, as for a reaction to a message.
        Ok(SendReceipt {
            message_id: MessageId::new(format!("reaction:{client_id}")),
            status: DeliveryStatus::Sent,
            timestamp: Some(Timestamp::now()),
        })
    }

    async fn story_privacy(
        &self,
        account: &AccountId,
    ) -> ProviderResult<client_provider::StoryPrivacy> {
        let wire = self.client.story_privacy(account.as_str()).await?;
        Ok(crate::stories::privacy(&wire))
    }

    async fn upload_media(
        &self,
        _account: &AccountId,
        upload: client_provider::MediaUpload,
        progress: client_provider::UploadProgress,
    ) -> ProviderResult<MediaRef> {
        let id = self.client.upload(&upload, &progress).await?;
        Ok(MediaRef::new(format!(
            "{}{id}",
            crate::uploads::UPLOAD_PREFIX
        )))
    }

    fn media_upload_limit(&self) -> Option<u64> {
        Some(crate::uploads::UPLOAD_LIMIT)
    }

    async fn media_upload_ready(&self) -> bool {
        self.client.uploads_ready().await
    }

    async fn download_media(
        &self,
        account: &AccountId,
        media: &MediaRef,
    ) -> ProviderResult<MediaData> {
        if let Some((size, named)) = mapping::on_demand(media.as_str()) {
            let limit = MediaLimit {
                max_bytes: u64::MAX,
                images_only: false,
            };
            return self
                .client
                .download_on_demand(Self::held(account, named), size, limit, None)
                .await;
        }
        match media.as_str().strip_prefix(PICTURE_PREFIX) {
            // Profile pictures are looked up only when wanted: their URLs
            // expire, and most are never shown.
            Some(contact) => {
                let url = self
                    .client
                    .contact_picture(account.as_str(), contact)
                    .await?
                    .url
                    .ok_or_else(|| ProviderError::Rejected {
                        code: "no_picture".into(),
                        message: "This contact has no visible profile picture.".into(),
                    })?;
                self.client.download(&url).await
            }
            None => self.client.download(media.as_str()).await,
        }
    }

    async fn fetch_media(
        &self,
        account: &AccountId,
        media: &MediaRef,
        limit: MediaLimit,
    ) -> ProviderResult<MediaData> {
        // Decided when the message, the story or the favorite was mapped,
        // from `media.downloaded`: a file wuapi already stores is fetched
        // from its URL; one still on WhatsApp is asked for through the
        // API first.
        match mapping::on_demand(media.as_str()) {
            Some((size, named)) => {
                self.client
                    .download_on_demand(Self::held(account, named), size, limit, None)
                    .await
            }
            None => self.client.download_limited(media.as_str(), limit).await,
        }
    }

    async fn fetch_media_reporting(
        &self,
        account: &AccountId,
        media: &MediaRef,
        limit: MediaLimit,
        progress: client_provider::UploadProgress,
    ) -> ProviderResult<MediaData> {
        match mapping::on_demand(media.as_str()) {
            Some((size, named)) => {
                self.client
                    .download_on_demand(Self::held(account, named), size, limit, Some(&progress))
                    .await
            }
            None => {
                self.client
                    .download_reporting(media.as_str(), limit, Some(&progress))
                    .await
            }
        }
    }

    async fn list_contacts(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Contact>> {
        let page = self
            .client
            .contacts(account.as_str(), cursor.as_ref().map(Cursor::as_str))
            .await?;
        Ok(Page {
            items: page.items.iter().map(mapping::contact).collect(),
            next_cursor: page.next_cursor.map(Cursor::new),
        })
    }

    async fn check_numbers(
        &self,
        account: &AccountId,
        phones: &[String],
    ) -> ProviderResult<Vec<NumberCheck>> {
        let mut numbers = Vec::new();
        for typed in phones {
            numbers.push(mapping::e164(typed).ok_or_else(invalid_phone)?);
        }
        let mut checks = Vec::new();
        // The route takes 50 numbers at a time.
        for batch in numbers.chunks(50) {
            let answered = self
                .client
                .check_numbers(account.as_str(), batch.to_vec())
                .await?;
            checks.extend(answered.into_iter().map(|check| {
                NumberCheck {
                    phone: check.phone,
                    on_whatsapp: check.on_whats_app,
                    chat: check
                        .contact_id
                        .filter(|_| check.on_whats_app)
                        .map(ChatId::new),
                    business_name: check.business_name.filter(|name| !name.trim().is_empty()),
                }
            }));
        }
        Ok(checks)
    }

    async fn lookup_contact(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Contact> {
        self.client.lookup_contact(account, contact).await
    }

    async fn business_profile(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Option<BusinessProfile>> {
        self.client.business_profile(account, contact).await
    }

    async fn list_blocked(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        self.client.blocked(account).await
    }

    async fn set_blocked(
        &self,
        account: &AccountId,
        contact: &ContactId,
        blocked: bool,
        request_id: &str,
    ) -> ProviderResult<()> {
        self.client
            .set_blocked(account, contact, blocked, request_id)
            .await
    }

    async fn own_profile(&self, account: &AccountId) -> ProviderResult<OwnProfile> {
        self.client.own_profile(account).await
    }

    async fn update_profile(
        &self,
        account: &AccountId,
        change: &ProfileChange,
    ) -> ProviderResult<()> {
        self.client.update_profile(account, change).await
    }

    async fn set_profile_picture(
        &self,
        account: &AccountId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        self.client.set_profile_picture(account, jpeg).await
    }

    async fn list_groups(&self, account: &AccountId) -> ProviderResult<Vec<Group>> {
        // One listing carries every group's participants. Read live from
        // WhatsApp, so it fails (in passing) while the number is offline.
        let groups = self.client.groups(account.as_str()).await?;
        Ok(groups
            .iter()
            .map(|wire| crate::social::group(wire, Vec::new()))
            .collect())
    }

    async fn fetch_group(&self, account: &AccountId, group: &ChatId) -> ProviderResult<Group> {
        self.client.group(account, group).await
    }

    async fn create_group(&self, account: &AccountId, new: &NewGroup) -> ProviderResult<Group> {
        self.client.create_group(account, new).await
    }

    async fn update_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: &GroupChange,
    ) -> ProviderResult<()> {
        self.client.update_group(account, group, change).await
    }

    async fn change_participants(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: ParticipantChange,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        self.client
            .change_participants(account, group, change, contacts, request_id)
            .await
    }

    async fn set_group_picture(
        &self,
        account: &AccountId,
        group: &ChatId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        self.client.set_group_picture(account, group, jpeg).await
    }

    async fn group_invite_link(
        &self,
        account: &AccountId,
        group: &ChatId,
        reset: bool,
        request_id: &str,
    ) -> ProviderResult<String> {
        self.client
            .invite_link(account, group, reset, request_id)
            .await
    }

    async fn join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> ProviderResult<Vec<JoinRequest>> {
        self.client.join_requests(account, group).await
    }

    async fn answer_join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
        approve: bool,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        self.client
            .answer_join_requests(account, group, approve, contacts, request_id)
            .await
    }

    async fn link_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
        request_id: &str,
    ) -> ProviderResult<()> {
        self.client
            .link_subgroup(account, community, group, request_id)
            .await
    }

    async fn unlink_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
    ) -> ProviderResult<()> {
        self.client.unlink_subgroup(account, community, group).await
    }

    async fn community_participants(
        &self,
        account: &AccountId,
        community: &ChatId,
    ) -> ProviderResult<Vec<ContactId>> {
        self.client.community_participants(account, community).await
    }

    async fn leave_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        request_id: &str,
    ) -> ProviderResult<()> {
        self.client.leave_group(account, group, request_id).await
    }

    async fn link_places(&self) -> ProviderResult<Vec<LinkPlace>> {
        let places = self.client.proxy_locations().await?;
        Ok(places.iter().map(mapping::link_place).collect())
    }

    async fn create_account(&self, new: &NewAccount) -> ProviderResult<LinkStatus> {
        let place = new.place.as_ref().ok_or_else(|| ProviderError::Rejected {
            code: "place_required".into(),
            message: "Choose where the number connects from.".into(),
        })?;
        let mut params = api::AccountCreateRequest::new(api::ProxyLocationInput::new(
            place.country.clone(),
            place.city.clone(),
        ));
        params.name = new.name.clone().filter(|name| !name.trim().is_empty());
        params.pairing_phone = match &new.pairing_phone {
            Some(typed) => Some(mapping::e164(typed).ok_or_else(invalid_phone)?),
            None => None,
        };
        // Sent either way: the API's default is no history, and the
        // user's choice should not depend on it.
        params.history_sync = Some(history_sync(new.history));
        let created = self.client.create_account(params, &new.request_id).await?;
        Ok(mapping::link_status(&created))
    }

    async fn link_status(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        let wire = self.client.account(account.as_str()).await?;
        Ok(mapping::link_status(&wire))
    }

    async fn pairing_code(&self, account: &AccountId, phone: &str) -> ProviderResult<LinkStatus> {
        let number = mapping::e164(phone).ok_or_else(invalid_phone)?;
        self.client.pairing_code(account.as_str(), &number).await?;
        self.link_status(account).await
    }

    async fn update_account(
        &self,
        account: &AccountId,
        change: AccountChange,
    ) -> ProviderResult<Account> {
        let mut params = api::AccountUpdateRequest::default();
        match change {
            AccountChange::Rename(name) => {
                let name = name.trim().to_owned();
                if name.is_empty() || name.chars().count() > 100 {
                    return Err(ProviderError::Rejected {
                        code: "invalid_name".into(),
                        message: "A name has between 1 and 100 characters.".into(),
                    });
                }
                params.name = Some(name);
            }
            AccountChange::History(history) => params.history_sync = Some(history_sync(history)),
        }
        let updated = self.client.update_account(account.as_str(), params).await?;
        Ok(mapping::account(&updated))
    }

    async fn reconnect_account(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        let wire = self.client.reconnect_account(account.as_str()).await?;
        Ok(mapping::link_status(&wire))
    }

    async fn unlink_account(&self, account: &AccountId) -> ProviderResult<Account> {
        let wire = self.client.logout_account(account.as_str()).await?;
        Ok(mapping::account(&wire))
    }

    async fn delete_account(&self, account: &AccountId) -> ProviderResult<()> {
        self.client.delete_account(account.as_str()).await
    }

    async fn fetch_avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
        known: Option<&str>,
    ) -> ProviderResult<AvatarAnswer> {
        // One path for contacts and groups: the route takes any chat id
        // (the documentation only names contacts). What it cannot show,
        // for whatever reason WhatsApp or the API has, is "no picture".
        let picture = match self
            .client
            .contact_picture(account.as_str(), subject.as_str())
            .await
        {
            Ok(picture) => picture,
            Err(ProviderError::Rejected { code, .. }) if NO_PICTURE.contains(&code.as_str()) => {
                return Ok(AvatarAnswer::None)
            }
            Err(error) => return Err(error),
        };
        let Some(url) = picture.url else {
            return Ok(AvatarAnswer::None);
        };
        // The URL is short-lived and changes on every call; the id is what
        // says whether the picture changed. Without one, the path of the
        // URL (its query carries the expiry) is the next best thing.
        let id = picture
            .id
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| url.split('?').next().unwrap_or(&url).to_owned());
        if known == Some(id.as_str()) {
            return Ok(AvatarAnswer::Unchanged);
        }
        let data = self.client.download_limited(&url, AVATAR_LIMIT).await?;
        Ok(AvatarAnswer::New(Avatar {
            id,
            bytes: data.bytes,
            mime_type: data.mime_type,
        }))
    }

    async fn subscribe(&self) -> ProviderResult<EventStream> {
        self.events.open().await
    }
}

fn invalid_phone() -> ProviderError {
    ProviderError::Rejected {
        code: "invalid_phone".into(),
        message: "Enter the number with its country code, like +58 424 555 0199.".into(),
    }
}

fn history_sync(history: HistoryImport) -> api::HistorySyncSetting {
    match history {
        HistoryImport::Off => api::HistorySyncSetting::None,
        HistoryImport::Recent => api::HistorySyncSetting::Recent,
    }
}
