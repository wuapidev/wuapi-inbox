//! The provider: the client's questions, answered from the archive and
//! passed on to a session.

use crate::archive::{Archive, ArchiveError, StoredAccount, StoredChat, StoredMessage};
use crate::convert::{self, DELIVERED, READ, SENT};
use crate::ids::{self, id_of, jid_of, message_id_for, STATUS_CHAT};
use crate::mapping::{self, MediaPointer};
use crate::session::{OpenOptions, Session, SessionError, Sessions, Sink, StatusPost, Target};
use async_trait::async_trait;
use client_provider::{
    refusal, Account, AccountChange, AccountId, AccountSettings, AvatarAnswer, BusinessProfile,
    CallEvent, CallState, Capabilities, Chat, ChatChange, ChatId, ClientMessageId, ConnectionState,
    Contact, ContactId, Cursor, DeliveryStatus, EventStream, ForwardItem, Group, GroupChange,
    HistoryImport, JoinRequest, LinkStatus, LinkStep, MediaData, MediaRef, MediaUpload, Message,
    MessageId, NewAccount, NewGroup, NewStory, NewStoryContent, NumberCheck, OutgoingContent,
    OutgoingMessage, OwnProfile, Page, ParticipantChange, ParticipantOutcome, ProfileChange,
    Provider, ProviderError, ProviderEvent, ProviderResult, SendReceipt, ServerMedia, Story,
    StoryAudience, StoryPrivacy, StoryViewer, Timestamp, UploadProgress, STORY_LIFETIME,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc, Notify};
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::Jid;

/// How long a page of older history waits for the phone to hand it over.
const HISTORY_WAIT: Duration = Duration::from_secs(8);
/// How many messages the phone is asked for at a time.
const HISTORY_BATCH: u32 = 50;
/// How many chats a page of the chat list holds.
const CHAT_PAGE: u32 = 200;
/// How many read receipts one call sends at most.
const READ_BATCH: u32 = 300;
/// How many events a slow listener may fall behind by.
const EVENT_BUFFER: usize = 4096;

/// A number, while the application runs.
#[derive(Clone)]
pub(crate) struct Live {
    /// Its session, once it started.
    pub session: Option<Arc<dyn Session>>,
    /// The session's number in the protocol store.
    pub device: i32,
    /// Where its connection is.
    pub connection: ConnectionState,
    /// Where its linking is.
    pub step: LinkStep,
    /// The last QR code, to go back to from a typed code.
    pub qr: Option<String>,
}

pub(crate) struct Inner {
    pub sessions: Arc<dyn Sessions>,
    pub archive: Archive,
    pub live: Mutex<HashMap<String, Live>>,
    pub events: broadcast::Sender<ProviderEvent>,
    pub sink: Sink,
    /// Woken when the phone hands over history that was asked for.
    pub history_news: Notify,
    /// The calls that are ringing or on, by id.
    pub calls: Mutex<HashMap<String, HeldCall>>,
    /// What sessions said before their number was written down.
    pub early: Mutex<Vec<(i32, crate::session::Inbound)>>,
}

/// A call that is ringing or on.
#[derive(Clone)]
pub(crate) struct HeldCall {
    /// With video.
    pub video: bool,
    /// It was answered, here or there.
    pub answered: bool,
    /// Somebody called the account; `false` when the account called.
    pub incoming: bool,
    /// When both sides started hearing each other.
    pub connected_ms: Option<i64>,
}

/// The provider that speaks WhatsApp's own protocol from this machine.
#[derive(Clone)]
pub struct NativeProvider {
    pub(crate) inner: Arc<Inner>,
}

pub(crate) fn now_ms() -> i64 {
    Timestamp::now().as_millis()
}

pub(crate) fn fail(error: SessionError) -> ProviderError {
    match error {
        SessionError::NotNow(why) => ProviderError::Transient(why),
        SessionError::SlowDown => ProviderError::RateLimited { retry_after: None },
        SessionError::Refused { code, message } => ProviderError::Rejected { code, message },
        SessionError::NotLinked => ProviderError::Rejected {
            code: "logged_out".into(),
            message: "This number is not linked to WhatsApp any more. Link it again.".into(),
        },
        SessionError::Unsupported(what) => ProviderError::Unsupported(what),
    }
}

pub(crate) fn db(error: ArchiveError) -> ProviderError {
    ProviderError::Protocol(error.to_string())
}

fn rejected(code: &str, message: &str) -> ProviderError {
    ProviderError::Rejected {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

fn account_id(number: i32) -> String {
    format!("native:{number}")
}

impl Inner {
    pub(crate) fn emit(&self, event: ProviderEvent) {
        // Nobody listening is fine: events are hints.
        let _ = self.events.send(event);
    }

    pub(crate) fn live_of(&self, account: &str) -> Option<Live> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(account)
            .cloned()
    }

    pub(crate) fn change_live(&self, account: &str, change: impl FnOnce(&mut Live)) {
        let mut live = self
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = live.get_mut(account) {
            change(entry);
        }
    }

    pub(crate) fn account_of_device(&self, device: i32) -> Option<String> {
        self.live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .find(|(_, live)| live.device == device)
            .map(|(id, _)| id.clone())
    }

    fn row(&self, account: &AccountId) -> ProviderResult<StoredAccount> {
        self.archive
            .account(account.as_str())
            .map_err(db)?
            .ok_or_else(|| rejected(refusal::NOT_FOUND, "This number is not on this computer."))
    }

    /// The session of a number, whatever its connection.
    fn session(&self, account: &AccountId) -> ProviderResult<Arc<dyn Session>> {
        let live = self
            .live_of(account.as_str())
            .ok_or_else(|| rejected(refusal::NOT_FOUND, "This number is not on this computer."))?;
        if live.connection == ConnectionState::LoggedOut {
            return Err(fail(SessionError::NotLinked));
        }
        live.session
            .ok_or_else(|| ProviderError::Transient("the number is still starting".into()))
    }

    /// The session of a number that is online.
    fn online(&self, account: &AccountId) -> ProviderResult<Arc<dyn Session>> {
        let session = self.session(account)?;
        match self.live_of(account.as_str()).map(|live| live.connection) {
            Some(ConnectionState::Connected) => Ok(session),
            _ => Err(ProviderError::Transient("the number is offline".into())),
        }
    }

    pub(crate) fn to_account(&self, row: &StoredAccount) -> Account {
        let live = self.live_of(&row.id);
        let named = |name: &Option<String>| name.clone().filter(|text| !text.trim().is_empty());
        Account {
            id: AccountId::new(row.id.clone()),
            display_name: named(&row.label)
                .or_else(|| named(&row.push_name))
                .or_else(|| row.phone.clone())
                .unwrap_or_else(|| "WhatsApp".to_owned()),
            phone: row.phone.clone(),
            self_contact: row
                .phone
                .clone()
                .or_else(|| row.lid.clone())
                .map(ContactId::new),
            connection: live
                .map(|live| live.connection)
                .unwrap_or(ConnectionState::Connecting),
            settings: AccountSettings {
                history_import: Some(if row.history {
                    HistoryImport::Recent
                } else {
                    HistoryImport::Off
                }),
                // Nothing is fetched before it is asked for: files stay on
                // WhatsApp until the client wants them.
                server_media: Some(ServerMedia::OnDemand),
                ever_linked: Some(row.ever_linked),
            },
        }
    }

    fn link_status(&self, row: &StoredAccount) -> LinkStatus {
        let live = self.live_of(&row.id);
        LinkStatus {
            account: self.to_account(row),
            step: live.map(|live| live.step).unwrap_or(LinkStep::Starting),
            was_linked: row.ever_linked,
        }
    }

    /// The id a chat or a person is kept under: their number when this
    /// device knows it, whichever address the wire used.
    pub(crate) fn canonical(&self, account: &str, jid: &Jid, alt: Option<&Jid>) -> String {
        let id = id_of(jid);
        if !jid.server.is_lid_family() {
            if let Some(alt) = alt.filter(|alt| alt.server.is_lid_family()) {
                self.learn_alias(account, &id_of(alt), &id);
            }
            return id;
        }
        if let Some(alt) = alt.filter(|alt| alt.server.is_pn_family()) {
            let number = id_of(alt);
            self.learn_alias(account, &id, &number);
            return number;
        }
        self.archive
            .number_of(account, &id)
            .ok()
            .flatten()
            .unwrap_or(id)
    }

    /// Records that two ids are one person, and brings what was kept
    /// under the hidden one over to the number.
    pub(crate) fn learn_alias(&self, account: &str, lid: &str, pn: &str) {
        if !matches!(self.archive.link_ids(account, lid, pn), Ok(true)) {
            return;
        }
        let _ = self.archive.merge_chat(account, lid, pn);
        let id = AccountId::new(account);
        let known = self
            .archive
            .contact(account, pn)
            .ok()
            .flatten()
            .unwrap_or_else(|| crate::archive::StoredContact {
                id: pn.to_owned(),
                ..Default::default()
            });
        self.emit(ProviderEvent::ContactUpdated(convert::to_contact(
            &id,
            &known,
            Some(lid.to_owned()),
        )));
    }

    fn jid(&self, id: &str) -> ProviderResult<Jid> {
        jid_of(id).ok_or_else(|| rejected(refusal::NOT_FOUND, "That is not a WhatsApp address."))
    }

    pub(crate) fn message_model(
        &self,
        account: &AccountId,
        stored: &StoredMessage,
    ) -> Option<Message> {
        let votes = convert::is_poll(stored)
            .then(|| {
                self.archive
                    .votes(account.as_str(), &stored.chat, &stored.id)
                    .ok()
            })
            .flatten();
        let mut message = convert::to_message(account, stored, votes.as_ref())?;
        if let Ok(Some(until)) = self
            .archive
            .pin_until(account.as_str(), &stored.chat, &stored.id)
        {
            message.extras.pinned_until = Some(Timestamp::from_millis(until));
        }
        Some(message)
    }

    pub(crate) fn chat_model(&self, account: &AccountId, stored: &StoredChat) -> Chat {
        let person = (!ids::is_group(&stored.id))
            .then(|| {
                self.archive
                    .contact(account.as_str(), &stored.id)
                    .ok()
                    .flatten()
            })
            .flatten();
        // The newest thing a conversation shows: a reaction is not one.
        let last = self
            .archive
            .page(account.as_str(), &stored.id, None, 8)
            .unwrap_or_default()
            .iter()
            .filter_map(|row| self.message_model(account, row))
            .find(|message| {
                !matches!(
                    message.content,
                    client_provider::MessageContent::Reaction { .. }
                )
            });
        convert::to_chat(account, stored, person.as_ref(), last, now_ms())
    }

    pub(crate) fn chat_changed(&self, account: &str, chat: &StoredChat) {
        if chat.id == STATUS_CHAT {
            return;
        }
        let id = AccountId::new(account);
        self.emit(ProviderEvent::ChatUpdated(self.chat_model(&id, chat)));
    }

    /// A message of the archive with what WhatsApp needs to act on it.
    fn target(
        &self,
        account: &AccountId,
        chat: Option<&ChatId>,
        message: &MessageId,
    ) -> ProviderResult<(StoredMessage, Target)> {
        let stored = match chat {
            Some(chat) => self
                .archive
                .message(account.as_str(), chat.as_str(), message.as_str())
                .map_err(db)?,
            None => None,
        };
        let stored = match stored {
            Some(stored) => stored,
            None => self
                .archive
                .find_message(account.as_str(), message.as_str())
                .map_err(db)?
                .ok_or_else(|| {
                    rejected(
                        refusal::NOT_FOUND,
                        "This message is not on this computer any more.",
                    )
                })?,
        };
        let target = self.target_of(&stored)?;
        Ok((stored, target))
    }

    fn target_of(&self, stored: &StoredMessage) -> ProviderResult<Target> {
        Ok(Target {
            chat: self.jid(&stored.chat)?,
            id: stored.id.clone(),
            sender: if stored.from_me {
                None
            } else {
                jid_of(&stored.sender)
            },
            from_me: stored.from_me,
            timestamp_ms: stored.timestamp_ms,
        })
    }

    fn newest_target(&self, account: &AccountId, chat: &ChatId) -> Option<Target> {
        let stored = self
            .archive
            .newest(account.as_str(), chat.as_str())
            .ok()
            .flatten()?;
        self.target_of(&stored).ok()
    }

    fn own_id(&self, account: &AccountId) -> String {
        self.archive
            .account(account.as_str())
            .ok()
            .flatten()
            .and_then(|row| row.phone.or(row.lid))
            .unwrap_or_else(|| account.to_string())
    }

    /// Writes a message that went out from here, so that it is listed and
    /// can be answered, edited or taken back.
    fn keep_sent(
        &self,
        account: &AccountId,
        chat: &str,
        id: &str,
        client_id: &ClientMessageId,
        message: &wa::Message,
        at_ms: i64,
    ) -> ProviderResult<()> {
        let stored = StoredMessage {
            chat: chat.to_owned(),
            id: id.to_owned(),
            sender: self.own_id(account),
            sender_name: None,
            from_me: true,
            timestamp_ms: at_ms,
            status: SENT,
            starred: false,
            edited: false,
            deleted: false,
            client_id: Some(client_id.to_string()),
            proto: convert::encode(message),
        };
        self.archive
            .put_message(account.as_str(), &stored)
            .map_err(db)?;
        self.archive
            .record_send(account.as_str(), client_id.as_str(), id, at_ms)
            .map_err(db)?;
        if chat != STATUS_CHAT {
            self.archive
                .update_chat(account.as_str(), chat, |chat| {
                    chat.last_ms = chat.last_ms.max(at_ms);
                    chat.unread = 0;
                    chat.marked_unread = false;
                })
                .map_err(db)?;
        }
        Ok(())
    }

    /// The receipt of a key that already went out, when it did.
    fn already_sent(
        &self,
        account: &AccountId,
        client_id: &ClientMessageId,
    ) -> ProviderResult<Option<SendReceipt>> {
        Ok(self
            .archive
            .sent_as(account.as_str(), client_id.as_str())
            .map_err(db)?
            .map(|(id, at_ms)| SendReceipt {
                message_id: MessageId::new(id),
                status: DeliveryStatus::Sent,
                timestamp: Some(Timestamp::from_millis(at_ms)),
            }))
    }

    /// Sends a message under the id its key always has, and keeps it.
    async fn deliver(
        &self,
        account: &AccountId,
        chat: &str,
        client_id: &ClientMessageId,
        message: wa::Message,
    ) -> ProviderResult<SendReceipt> {
        if let Some(receipt) = self.already_sent(account, client_id)? {
            return Ok(receipt);
        }
        let session = self.online(account)?;
        let to = self.jid(chat)?;
        let id = message_id_for(client_id);
        session
            .send(&to, message.clone(), &id)
            .await
            .map_err(fail)?;
        let at_ms = now_ms();
        self.keep_sent(account, chat, &id, client_id, &message, at_ms)?;
        Ok(SendReceipt {
            message_id: MessageId::new(id),
            status: DeliveryStatus::Sent,
            timestamp: Some(Timestamp::from_millis(at_ms)),
        })
    }

    /// The people a story of the account is shown to: its address book,
    /// as the audience setting cuts it.
    fn story_recipients(&self, account: &AccountId) -> ProviderResult<Vec<Jid>> {
        let privacy = self.story_privacy(account)?;
        let listed = |ids: &[ContactId]| -> Vec<Jid> {
            ids.iter().filter_map(|id| jid_of(id.as_str())).collect()
        };
        if privacy.audience == StoryAudience::OnlyShareWith {
            return Ok(listed(&privacy.only));
        }
        let except: Vec<String> = match privacy.audience {
            StoryAudience::ContactsExcept => {
                privacy.except.iter().map(|id| id.to_string()).collect()
            }
            _ => Vec::new(),
        };
        Ok(self
            .archive
            .contacts(account.as_str())
            .map_err(db)?
            .into_iter()
            .filter(|contact| contact.saved_name.is_some() && contact.id.starts_with('+'))
            .filter(|contact| !except.contains(&contact.id))
            .filter_map(|contact| jid_of(&contact.id))
            .collect())
    }

    fn story_privacy(&self, account: &AccountId) -> ProviderResult<StoryPrivacy> {
        Ok(self
            .archive
            .value(account.as_str(), "story_privacy")
            .map_err(db)?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default())
    }

    fn stored_story(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> ProviderResult<StoredMessage> {
        self.archive
            .message(account.as_str(), STATUS_CHAT, story.as_str())
            .map_err(db)?
            .ok_or_else(|| rejected(refusal::NOT_FOUND, "This status is no longer available."))
    }

    pub(crate) fn story_model(&self, account: &AccountId, stored: &StoredMessage) -> Option<Story> {
        let views = stored.from_me.then(|| {
            self.archive
                .views(account.as_str(), &stored.id)
                .map(|views| views.len() as u32)
                .unwrap_or(0)
        });
        convert::to_story(account, stored, views)
    }

    /// Starts the session of a stored number.
    pub(crate) async fn open(self: &Arc<Self>, row: &StoredAccount, device: Option<i32>) {
        let options = OpenOptions {
            history: row.history,
        };
        match self.sessions.open(device, options, self.sink.clone()).await {
            Ok((number, session)) => {
                if number != row.device {
                    let _ = self.archive.put_account(&StoredAccount {
                        device: number,
                        ..row.clone()
                    });
                }
                self.change_live(&row.id, |live| {
                    live.device = number;
                    live.session = Some(session);
                });
                self.replay_early(number);
            }
            Err(error) => {
                tracing::warn!(account = %row.id, "the session did not start: {error}");
                let state = ConnectionState::Disconnected {
                    reason: Some(error.to_string()),
                };
                self.change_live(&row.id, |live| {
                    live.connection = state.clone();
                    live.step = LinkStep::Stopped {
                        reason: error.to_string(),
                    };
                });
                self.emit(ProviderEvent::ConnectionChanged {
                    account_id: AccountId::new(row.id.clone()),
                    state,
                });
            }
        }
    }
}

impl NativeProvider {
    /// A provider over the sessions and the archive of this machine. The
    /// numbers that were linked before start connecting at once, on
    /// `runtime`.
    pub fn new(sessions: Arc<dyn Sessions>, archive: Archive, runtime: &Handle) -> Self {
        crate::call_video::look_ahead();
        let (sink, mut inbox) = mpsc::unbounded_channel();
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        let stored = archive.accounts().unwrap_or_default();
        let live = stored
            .iter()
            .map(|row| {
                let entry = Live {
                    session: None,
                    device: row.device,
                    connection: ConnectionState::Connecting,
                    step: LinkStep::Starting,
                    qr: None,
                };
                (row.id.clone(), entry)
            })
            .collect();
        let inner = Arc::new(Inner {
            sessions,
            archive,
            live: Mutex::new(live),
            events,
            sink,
            history_news: Notify::new(),
            calls: Mutex::new(HashMap::new()),
            early: Mutex::new(Vec::new()),
        });
        let listener = inner.clone();
        runtime.spawn(async move {
            while let Some((device, inbound)) = inbox.recv().await {
                listener.apply(device, inbound);
            }
        });
        let starter = inner.clone();
        runtime.spawn(async move {
            for row in stored {
                starter.open(&row, Some(row.device)).await;
            }
        });
        Self { inner }
    }
}

/// A QR code as a PNG image: black on white, with the quiet margin a
/// phone's camera needs.
pub(crate) fn qr_png(text: &str) -> Option<Vec<u8>> {
    const MODULE: u32 = 8;
    const MARGIN: u32 = 4;
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let width = code.width() as u32;
    let colors = code.to_colors();
    let side = (width + 2 * MARGIN) * MODULE;
    let image = image::GrayImage::from_fn(side, side, |x, y| {
        let (column, row) = (x / MODULE, y / MODULE);
        let inside =
            (MARGIN..MARGIN + width).contains(&column) && (MARGIN..MARGIN + width).contains(&row);
        let dark = inside
            && colors[((row - MARGIN) * width + (column - MARGIN)) as usize] == qrcode::Color::Dark;
        image::Luma([if dark { 0 } else { 255 }])
    });
    let mut png = Vec::new();
    image::DynamicImage::ImageLuma8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .ok()?;
    Some(png)
}

/// A message as it is passed on: marked as forwarded, without what was
/// its own (the quote it answered, its secret). `Err` with the neutral
/// code of why it cannot be.
fn forwardable(stored: &StoredMessage) -> Result<wa::Message, &'static str> {
    if stored.deleted {
        return Err(refusal::FORWARD_DELETED);
    }
    let message = convert::decode(&stored.proto).ok_or(refusal::FORWARD_KIND)?;
    let mapped = match mapping::effect(&message, None) {
        mapping::Effect::Content(mapped) => mapped,
        _ => return Err(refusal::FORWARD_KIND),
    };
    if mapped.extras.view_once {
        return Err(refusal::FORWARD_VIEW_ONCE);
    }
    let score = if mapped.extras.forwarded_many { 5 } else { 1 };
    let context = wa::ContextInfo {
        is_forwarded: Some(true),
        forwarding_score: Some(score),
        ..Default::default()
    };
    use client_provider::MessageContent as Content;
    match &mapped.content {
        Content::Text { body } => Ok(convert::text_message(body, Some(context))),
        Content::Media(media) => {
            let pointer = media
                .source
                .as_ref()
                .and_then(MediaPointer::parse)
                .ok_or(refusal::FORWARD_NO_FILE)?;
            let built = convert::media_message(
                &pointer,
                media.kind,
                media.mime_type.as_deref(),
                media.caption.as_deref(),
                media.file_name.as_deref(),
                media.gif,
                Some(context),
            );
            // A document passed on keeps the picture it came with.
            let preview = client_provider::FilePreview {
                jpeg: media
                    .thumbnail
                    .as_ref()
                    .and_then(mapping::inline_bytes)
                    .unwrap_or_default(),
                width: None,
                height: None,
                pages: media.pages,
            };
            Ok(convert::with_preview(built, Some(&preview)))
        }
        Content::Location(_) | Content::Contacts { .. } => {
            let mut passed = mapping::unwrap(&message).clone();
            passed.message_context_info = Default::default();
            let field = whatsapp_rust::buffa::MessageField::some(context);
            if let Some(place) = passed.location_message.as_option_mut() {
                place.context_info = field;
            } else if let Some(place) = passed.live_location_message.as_option_mut() {
                place.context_info = field;
            } else if let Some(card) = passed.contact_message.as_option_mut() {
                card.context_info = field;
            } else if let Some(cards) = passed.contacts_array_message.as_option_mut() {
                cards.context_info = field;
            }
            Ok(passed)
        }
        _ => Err(refusal::FORWARD_KIND),
    }
}

fn forward_refusal(code: &str) -> ProviderError {
    let message = match code {
        refusal::FORWARD_DELETED => "This message was deleted.",
        refusal::FORWARD_VIEW_ONCE => "A view-once message cannot be forwarded.",
        refusal::FORWARD_NO_FILE => "WhatsApp no longer has this file.",
        _ => "This kind of message cannot be forwarded.",
    };
    rejected(code, message)
}

#[async_trait]
impl Provider for NativeProvider {
    fn id(&self) -> &'static str {
        "native"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // The phone hands the chats over with their unread counts,
            // pins, mutes and the archive, and every later change is heard.
            chat_list: true,
            chat_state: true,
            start_chat: true,
            link_accounts: true,
            link_by_code: true,
            // Giving up on a code leaves the QR codes running.
            link_back_to_scan: true,
            manage_accounts: true,
            history_import: true,
            contacts: true,
            number_check: true,
            contact_names: true,
            groups: true,
            realtime_push: true,
            // A linked device is told what it missed when it reconnects,
            // but nothing here answers "everything since X": catching up
            // is reading the newest pages again, which cost nothing.
            incremental_sync: false,
            replies: true,
            private_replies: true,
            reactions: true,
            edits: true,
            deletes: true,
            delete_for_me: true,
            delete_received: true,
            stars: true,
            pins: true,
            forwards: true,
            polls: true,
            poll_votes: true,
            media_download: true,
            avatars: true,
            media_upload: true,
            read_receipts: true,
            quiet_read: true,
            presence: true,
            contact_lookup: true,
            business_profiles: true,
            blocking: true,
            profile_edit: true,
            group_info: true,
            group_create: true,
            group_manage: true,
            group_invites: true,
            group_join_requests: true,
            group_leave: true,
            community_manage: true,
            community_members: true,
            mentions: true,
            // TODO(native): the starred stickers are heard (app state) and
            // not written yet; favorites stay on this computer.
            sticker_favorites: false,
            forward_any: true,
            // A poll passed on needs a secret of its own, and an event its
            // RSVPs: not done.
            forward_polls: false,
            forward_events: false,
            story_list: true,
            story_contacts: true,
            story_post: true,
            story_view: true,
            story_viewers: true,
            story_reply: true,
            story_react: true,
            story_mute: true,
            story_privacy: true,
            // TODO(native): the audience is read from the phone and not
            // written back.
            story_privacy_edit: false,
            story_delete: true,
            calls: true,
            // TODO(native): video needs a camera and an H.264 encoder.
            // With video where there is something to encode it with,
            // once that is known (see `call_video::look_ahead`).
            video_calls: crate::call_video::available_now(),
            // A URL that sends a message needs a server to receive it:
            // Wuapi Cloud has them, a number on this computer does not.
            incoming_webhooks: false,
        }
    }

    async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
        let inner = &self.inner;
        Ok(inner
            .archive
            .accounts()
            .map_err(db)?
            .iter()
            .map(|row| inner.to_account(row))
            .collect())
    }

    async fn list_chats(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Chat>> {
        let inner = &self.inner;
        let offset: u32 = cursor
            .and_then(|cursor| cursor.as_str().parse().ok())
            .unwrap_or(0);
        let stored = inner
            .archive
            .chats(account.as_str(), offset, CHAT_PAGE)
            .map_err(db)?;
        let full = stored.len() as u32 == CHAT_PAGE;
        let items = stored
            .iter()
            .filter(|chat| chat.id != STATUS_CHAT)
            .map(|chat| inner.chat_model(account, chat))
            .collect();
        Ok(Page {
            items,
            next_cursor: full.then(|| Cursor::new((offset + CHAT_PAGE).to_string())),
        })
    }

    async fn fetch_messages(
        &self,
        account: &AccountId,
        chat: &ChatId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> ProviderResult<Page<Message>> {
        let inner = &self.inner;
        let before = cursor.as_ref().and_then(|cursor| {
            let (ts, id) = cursor.as_str().split_once(':')?;
            Some((ts.parse::<i64>().ok()?, id.to_owned()))
        });
        let limit = limit.clamp(1, 500);
        let read = || {
            inner
                .archive
                .page(
                    account.as_str(),
                    chat.as_str(),
                    before.as_ref().map(|(ts, id)| (*ts, id.as_str())),
                    limit,
                )
                .map_err(db)
        };
        let mut rows = read()?;
        if cursor.is_none() {
            // Somebody is looking at this chat: ask to be told when its
            // person types. Nothing depends on the answer.
            if let (Ok(session), Some(who)) = (inner.online(account), jid_of(chat.as_str())) {
                if !ids::is_group(chat.as_str()) {
                    let _ = session.watch(&who).await;
                }
            }
        }
        if rows.is_empty() && before.is_some() {
            // This device holds nothing older: the phone does. Ask it
            // once for what comes before the oldest message here.
            let oldest = inner
                .archive
                .oldest(account.as_str(), chat.as_str())
                .map_err(db)?;
            let asked_key = format!("history:{chat}");
            let asked = inner
                .archive
                .value(account.as_str(), &asked_key)
                .map_err(db)?;
            if let (Some(oldest), Ok(session)) = (oldest, inner.online(account)) {
                if asked.as_deref() != Some(oldest.id.as_str()) {
                    let target = inner.target_of(&oldest)?;
                    let news = inner.history_news.notified();
                    // Asking and waiting together stay well under the
                    // engine's own deadline for a call.
                    let asking = session.request_history(&target, HISTORY_BATCH);
                    let answer = tokio::time::timeout(HISTORY_WAIT, asking).await;
                    if matches!(answer, Ok(Ok(()))) {
                        let _ = tokio::time::timeout(HISTORY_WAIT, news).await;
                        rows = read()?;
                        if rows.is_empty() {
                            // The phone had nothing, or is not there: do
                            // not ask for the same page in a loop.
                            let _ = inner.archive.set_value(
                                account.as_str(),
                                &asked_key,
                                Some(&oldest.id),
                            );
                        }
                    }
                }
            }
        }
        let next_cursor = rows
            .last()
            .map(|row| Cursor::new(format!("{}:{}", row.timestamp_ms, row.id)));
        let items = rows
            .iter()
            .filter_map(|row| inner.message_model(account, row))
            .collect();
        Ok(Page { items, next_cursor })
    }

    async fn send(&self, message: OutgoingMessage) -> ProviderResult<SendReceipt> {
        let inner = &self.inner;
        let account = &message.account_id;
        if let Some(receipt) = inner.already_sent(account, &message.client_id)? {
            return Ok(receipt);
        }
        let chat = message.chat_id.as_str();
        let mut quoted = match &message.reply_to {
            Some(id) => inner
                .archive
                .message(account.as_str(), chat, id.as_str())
                .map_err(db)?,
            None => None,
        };
        // An answer in private: what it quotes is in a group, which the
        // quote names so that the other side shows where it is from.
        let mut quoted_chat = None;
        if let (None, Some(id)) = (&quoted, &message.reply_to) {
            let elsewhere = inner
                .archive
                .find_message(account.as_str(), id.as_str())
                .map_err(db)?
                .filter(|stored| !stored.from_me && crate::ids::is_group(&stored.chat));
            if let Some(stored) = elsewhere {
                if let Some(group) = jid_of(&stored.chat) {
                    quoted_chat = Some(group);
                    quoted = Some(stored);
                }
            }
        }
        let quoted_author = quoted.as_ref().and_then(|stored| {
            if stored.from_me {
                inner
                    .session(account)
                    .ok()
                    .and_then(|session| session.identity().pn)
            } else {
                jid_of(&stored.sender)
            }
        });
        let mentions: Vec<Jid> = message
            .mentions
            .iter()
            .filter_map(|mention| jid_of(mention.id.as_str()))
            .collect();
        let context = convert::context(
            quoted
                .as_ref()
                .map(|stored| (stored, quoted_author.as_ref())),
            quoted_chat.as_ref(),
            &mentions,
            message.forwarded,
        );
        let built = match &message.content {
            OutgoingContent::Text { body } => {
                if body.trim().is_empty() {
                    return Err(rejected("empty", "A message needs some text."));
                }
                convert::text_message(body, context)
            }
            OutgoingContent::Media {
                kind,
                media,
                mime_type,
                caption,
                file_name,
                gif,
                preview,
            } => {
                let pointer = MediaPointer::parse(media).ok_or_else(|| {
                    rejected("not_uploaded", "The file was not uploaded to WhatsApp.")
                })?;
                let built = convert::media_message(
                    &pointer,
                    *kind,
                    mime_type.as_deref(),
                    caption.as_deref(),
                    file_name.as_deref(),
                    *gif,
                    context,
                );
                convert::with_preview(built, preview.as_ref())
            }
            OutgoingContent::Reaction { target, emoji } => {
                let (_, target) = inner.target(account, Some(&message.chat_id), target)?;
                let session = inner.online(account)?;
                let id = session.react(&target, emoji).await.map_err(fail)?;
                let reaction = wa::Message {
                    reaction_message: whatsapp_rust::buffa::MessageField::some(
                        wa::message::ReactionMessage {
                            key: whatsapp_rust::buffa::MessageField::some(wa::MessageKey {
                                remote_jid: Some(convert::wire(&target.chat)),
                                from_me: Some(target.from_me),
                                id: Some(target.id.clone()),
                                participant: target.sender.as_ref().map(convert::wire),
                            }),
                            text: Some(emoji.clone()),
                            ..Default::default()
                        },
                    ),
                    ..Default::default()
                };
                let at_ms = now_ms();
                inner.keep_sent(account, chat, &id, &message.client_id, &reaction, at_ms)?;
                return Ok(SendReceipt {
                    message_id: MessageId::new(id),
                    status: DeliveryStatus::Sent,
                    timestamp: Some(Timestamp::from_millis(at_ms)),
                });
            }
            OutgoingContent::Poll {
                question,
                options,
                max_choices,
            } => {
                if options.len() < 2 {
                    return Err(rejected("invalid", "A poll needs at least two answers."));
                }
                convert::poll_message(question, options, *max_choices, &message.client_id)
            }
            OutgoingContent::Forward { .. } => {
                return Err(rejected("invalid", "A forward is not sent as a message."))
            }
        };
        inner
            .deliver(account, chat, &message.client_id, built)
            .await
    }

    async fn mark_read(
        &self,
        account: &AccountId,
        chat: &ChatId,
        up_to: Option<&MessageId>,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let mut unread = inner
            .archive
            .unread_incoming(account.as_str(), chat.as_str(), READ_BATCH)
            .map_err(db)?;
        if let Some(up_to) = up_to {
            let limit = inner
                .archive
                .message(account.as_str(), chat.as_str(), up_to.as_str())
                .map_err(db)?
                .map(|stored| stored.timestamp_ms);
            if let Some(limit) = limit {
                unread.retain(|stored| stored.timestamp_ms <= limit);
            }
        }
        if !unread.is_empty() {
            let session = inner.online(account)?;
            let to = inner.jid(chat.as_str())?;
            // A receipt names one sender: in a group, one call for each.
            let mut by_sender: HashMap<String, Vec<String>> = HashMap::new();
            for stored in &unread {
                by_sender
                    .entry(stored.sender.clone())
                    .or_default()
                    .push(stored.id.clone());
            }
            for (sender, ids) in by_sender {
                let sender = ids::is_group(chat.as_str())
                    .then(|| jid_of(&sender))
                    .flatten();
                session
                    .mark_read(&to, sender.as_ref(), &ids)
                    .await
                    .map_err(fail)?;
            }
            for stored in &unread {
                let _ = inner
                    .archive
                    .advance(account.as_str(), chat.as_str(), &stored.id, READ);
            }
        }
        inner
            .archive
            .update_chat(account.as_str(), chat.as_str(), |chat| {
                chat.unread = 0;
                chat.marked_unread = false;
            })
            .map_err(db)?;
        Ok(())
    }

    async fn mark_read_quietly(&self, account: &AccountId, chat: &ChatId) -> ProviderResult<()> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        let to = inner.jid(chat.as_str())?;
        let last = inner.newest_target(account, chat);
        session
            .set_chat_read(&to, true, last.as_ref())
            .await
            .map_err(fail)?;
        inner
            .archive
            .update_chat(account.as_str(), chat.as_str(), |chat| {
                chat.unread = 0;
                chat.marked_unread = false;
            })
            .map_err(db)?;
        Ok(())
    }

    async fn update_chat(
        &self,
        account: &AccountId,
        chat: &ChatId,
        change: ChatChange,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        let to = inner.jid(chat.as_str())?;
        let last = inner.newest_target(account, chat);
        if change == ChatChange::MarkedUnread {
            session
                .set_chat_read(&to, false, last.as_ref())
                .await
                .map_err(fail)?;
        } else {
            session
                .change_chat(&to, change, last.as_ref())
                .await
                .map_err(fail)?;
        }
        let now = now_ms();
        inner
            .archive
            .update_chat(account.as_str(), chat.as_str(), |chat| match change {
                ChatChange::Pinned(true) => chat.pinned_at_ms = Some(now),
                ChatChange::Pinned(false) => chat.pinned_at_ms = None,
                ChatChange::Muted(true) => chat.muted_until_ms = Some(-1),
                ChatChange::Muted(false) => chat.muted_until_ms = None,
                ChatChange::MutedFor(seconds) => {
                    chat.muted_until_ms = Some(now.saturating_add(seconds as i64 * 1000));
                }
                ChatChange::Archived(archived) => chat.archived = archived,
                ChatChange::MarkedUnread => chat.marked_unread = true,
            })
            .map_err(db)?;
        Ok(())
    }

    async fn start_chat(&self, account: &AccountId, phone: &str) -> ProviderResult<Chat> {
        let inner = &self.inner;
        let id = ids::id_of_phone(phone)
            .ok_or_else(|| rejected("invalid", "That is not a phone number."))?;
        let session = inner.online(account)?;
        let digits = id.trim_start_matches('+').to_owned();
        let found = session.on_whatsapp(&[digits]).await.map_err(fail)?;
        let jid = found
            .into_iter()
            .find_map(|number| number.jid)
            .ok_or_else(|| rejected(refusal::NOT_ON_WHATSAPP, "This number is not on WhatsApp."))?;
        let chat = inner
            .archive
            .update_chat(account.as_str(), &id_of(&jid), |_| {})
            .map_err(db)?;
        Ok(inner.chat_model(account, &chat))
    }

    async fn download_media(
        &self,
        account: &AccountId,
        media: &MediaRef,
    ) -> ProviderResult<MediaData> {
        if let Some(bytes) = mapping::inline_bytes(media) {
            return Ok(MediaData {
                bytes,
                mime_type: Some("image/jpeg".into()),
            });
        }
        let pointer = MediaPointer::parse(media)
            .ok_or_else(|| rejected(refusal::NOT_FOUND, "This file is not on WhatsApp."))?;
        let session = self.inner.online(account)?;
        let bytes = session.download(&pointer).await.map_err(fail)?;
        Ok(MediaData {
            bytes,
            mime_type: pointer.mime_type,
        })
    }

    async fn fetch_media(
        &self,
        account: &AccountId,
        media: &MediaRef,
        limit: client_provider::MediaLimit,
    ) -> ProviderResult<MediaData> {
        // The pointer says the size and the type before a byte is fetched.
        if let Some(pointer) = MediaPointer::parse(media) {
            if pointer.file_length > limit.max_bytes {
                return Err(rejected(
                    "too_large",
                    "The file is larger than the download limit.",
                ));
            }
            let image = pointer.class == "image" || pointer.class == "sticker";
            if limit.images_only && !image {
                return Err(rejected("not_an_image", "The file is not an image."));
            }
        }
        self.download_media(account, media).await
    }

    async fn upload_media(
        &self,
        account: &AccountId,
        upload: MediaUpload,
        progress: UploadProgress,
    ) -> ProviderResult<MediaRef> {
        let inner = &self.inner;
        let key = format!("upload:{}", upload.key);
        if let Some(held) = inner.archive.value(account.as_str(), &key).map_err(db)? {
            return Ok(MediaRef::new(held));
        }
        let session = inner.online(account)?;
        let size = upload.bytes.len() as u64;
        let mut pointer = session
            .upload(
                upload.bytes.as_ref().clone(),
                convert::class_of(upload.kind),
            )
            .await
            .map_err(fail)?;
        pointer.mime_type = Some(upload.mime_type.clone());
        progress(size);
        let media = pointer.to_ref();
        inner
            .archive
            .set_value(account.as_str(), &key, Some(media.as_str()))
            .map_err(db)?;
        Ok(media)
    }

    async fn fetch_avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
        known: Option<&str>,
    ) -> ProviderResult<AvatarAnswer> {
        let session = self.inner.online(account)?;
        let whose = self.inner.jid(subject.as_str())?;
        session.picture(&whose, known).await.map_err(fail)
    }

    async fn vote_poll(
        &self,
        account: &AccountId,
        chat: &ChatId,
        poll: &MessageId,
        choices: &[String],
    ) -> ProviderResult<Option<Message>> {
        let inner = &self.inner;
        let (stored, target) = inner.target(account, Some(chat), poll)?;
        let message = convert::decode(&stored.proto)
            .filter(|_| convert::is_poll(&stored))
            .ok_or_else(|| rejected("invalid", "This message is not a poll."))?;
        let session = inner.online(account)?;
        session
            .vote(&target, &message, choices)
            .await
            .map_err(fail)?;
        let hashes: Vec<Vec<u8>> = choices
            .iter()
            .map(|name| mapping::option_hash(name))
            .collect();
        inner
            .archive
            .put_vote(
                account.as_str(),
                &crate::archive::Vote {
                    chat: &stored.chat,
                    poll: &stored.id,
                    voter: &inner.own_id(account),
                    mine: true,
                    hashes: &hashes,
                    timestamp_ms: now_ms(),
                },
            )
            .map_err(db)?;
        Ok(inner.message_model(account, &stored))
    }

    async fn edit_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        text: &str,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let (stored, target) = inner.target(account, Some(chat), message)?;
        let replacement = convert::text_message(text, None);
        let session = inner.online(account)?;
        session
            .edit(&target, replacement.clone())
            .await
            .map_err(fail)?;
        inner
            .archive
            .replace(
                account.as_str(),
                &stored.chat,
                &stored.id,
                &convert::encode(&replacement),
            )
            .map_err(db)?;
        Ok(())
    }

    async fn delete_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        for_everyone: bool,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        // A message that is gone already was deleted: the client repeats
        // a call whose answer was lost.
        let Ok((stored, target)) = inner.target(account, Some(chat), message) else {
            return Ok(());
        };
        let session = inner.online(account)?;
        if for_everyone {
            if !stored.deleted {
                session.revoke(&target).await.map_err(fail)?;
            }
            inner
                .archive
                .mark_deleted(account.as_str(), &stored.chat, &stored.id)
                .map_err(db)?;
        } else {
            session.delete_for_me(&target).await.map_err(fail)?;
            inner
                .archive
                .remove_message(account.as_str(), &stored.chat, &stored.id)
                .map_err(db)?;
        }
        Ok(())
    }

    async fn star_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        starred: bool,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let (stored, target) = inner.target(account, Some(chat), message)?;
        let session = inner.online(account)?;
        session.star(&target, starred).await.map_err(fail)?;
        inner
            .archive
            .set_starred(account.as_str(), &stored.chat, &stored.id, starred)
            .map_err(db)?;
        Ok(())
    }

    async fn pin_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        pin: Option<client_provider::PinFor>,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let (stored, target) = inner.target(account, Some(chat), message)?;
        let session = inner.online(account)?;
        session.pin(&target, pin).await.map_err(fail)?;
        let until = pin.map(|pin| now_ms() + pin.duration().as_millis() as i64);
        inner
            .archive
            .set_pin(account.as_str(), &stored.chat, &stored.id, until)
            .map_err(db)?;
        Ok(())
    }

    async fn forward_messages(
        &self,
        account: &AccountId,
        items: &[ForwardItem],
    ) -> ProviderResult<Vec<ProviderResult<SendReceipt>>> {
        let inner = &self.inner;
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            let result = async {
                if let Some(receipt) = inner.already_sent(account, &item.client_id)? {
                    return Ok(receipt);
                }
                let (stored, _) = inner.target(account, None, &item.message)?;
                let passed = forwardable(&stored).map_err(forward_refusal)?;
                inner
                    .deliver(account, item.to.as_str(), &item.client_id, passed)
                    .await
            }
            .await;
            results.push(result);
        }
        Ok(results)
    }

    async fn list_contacts(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Contact>> {
        let inner = &self.inner;
        if cursor.is_some() {
            return Ok(Page::last(Vec::new()));
        }
        let contacts = inner
            .archive
            .contacts(account.as_str())
            .map_err(db)?
            .iter()
            .map(|known| {
                let alt = if known.id.starts_with('+') {
                    inner
                        .archive
                        .hidden_id_of(account.as_str(), &known.id)
                        .ok()
                        .flatten()
                } else {
                    inner
                        .archive
                        .number_of(account.as_str(), &known.id)
                        .ok()
                        .flatten()
                };
                convert::to_contact(account, known, alt)
            })
            .collect();
        Ok(Page::last(contacts))
    }

    async fn check_numbers(
        &self,
        account: &AccountId,
        phones: &[String],
    ) -> ProviderResult<Vec<NumberCheck>> {
        let session = self.inner.online(account)?;
        let digits: Vec<String> = phones
            .iter()
            .map(|phone| phone.chars().filter(char::is_ascii_digit).collect())
            .collect();
        let found = session.on_whatsapp(&digits).await.map_err(fail)?;
        Ok(phones
            .iter()
            .zip(&digits)
            .map(|(phone, digits)| {
                let jid = found
                    .iter()
                    .find(|number| &number.digits == digits)
                    .and_then(|number| number.jid.clone());
                NumberCheck {
                    phone: phone.clone(),
                    on_whatsapp: jid.is_some(),
                    chat: jid.as_ref().map(ids::chat_id),
                    business_name: None,
                }
            })
            .collect())
    }

    async fn lookup_contact(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Contact> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        let who = inner.jid(contact.as_str())?;
        let found = session.lookup(&who).await.map_err(fail)?;
        if let Some(alt) = &found.alt {
            let (lid, pn) = if who.server.is_lid_family() {
                (id_of(&who), id_of(alt))
            } else {
                (id_of(alt), id_of(&who))
            };
            inner.learn_alias(account.as_str(), &lid, &pn);
        }
        let (known, _) = inner
            .archive
            .learn_contact(
                account.as_str(),
                &crate::archive::StoredContact {
                    id: contact.to_string(),
                    business_name: found.business_name.clone(),
                    ..Default::default()
                },
            )
            .map_err(db)?;
        Ok(Contact {
            about: found.about,
            picture_id: found.picture_id,
            ..convert::to_contact(account, &known, found.alt.as_ref().map(id_of))
        })
    }

    async fn business_profile(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Option<BusinessProfile>> {
        let session = self.inner.online(account)?;
        let who = self.inner.jid(contact.as_str())?;
        session.business(&who).await.map_err(fail)
    }

    async fn list_blocked(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        let session = self.inner.online(account)?;
        let blocked = session.blocked().await.map_err(fail)?;
        Ok(blocked
            .iter()
            .map(|jid| ContactId::new(self.inner.canonical(account.as_str(), jid, None)))
            .collect())
    }

    async fn set_blocked(
        &self,
        account: &AccountId,
        contact: &ContactId,
        blocked: bool,
        _request_id: &str,
    ) -> ProviderResult<()> {
        let session = self.inner.online(account)?;
        let who = self.inner.jid(contact.as_str())?;
        session.set_blocked(&who, blocked).await.map_err(fail)
    }

    async fn own_profile(&self, account: &AccountId) -> ProviderResult<OwnProfile> {
        let session = self.inner.online(account)?;
        let name = Some(session.identity().push_name).filter(|name| !name.is_empty());
        // The About may be hidden from the account's own lookups by its
        // privacy setting; that is "not known", not a failure.
        let about = session.own_about().await.ok().flatten();
        Ok(OwnProfile { name, about })
    }

    async fn update_profile(
        &self,
        account: &AccountId,
        change: &ProfileChange,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        match change {
            ProfileChange::Name(name) => {
                session.set_name(name).await.map_err(fail)?;
                let mut row = inner.row(account)?;
                row.push_name = Some(name.clone());
                inner.archive.put_account(&row).map_err(db)?;
            }
            ProfileChange::About(about) => session.set_about(about).await.map_err(fail)?,
        }
        Ok(())
    }

    async fn set_profile_picture(
        &self,
        account: &AccountId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        let session = self.inner.online(account)?;
        session.set_picture(None, jpeg).await.map_err(fail)
    }

    async fn list_groups(&self, account: &AccountId) -> ProviderResult<Vec<Group>> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        let mut groups = session.groups().await.map_err(fail)?;
        for group in &mut groups {
            inner.adopt_group(account, group);
        }
        Ok(groups)
    }

    async fn fetch_group(&self, account: &AccountId, group: &ChatId) -> ProviderResult<Group> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        let mut found = session
            .group(&inner.jid(group.as_str())?)
            .await
            .map_err(fail)?;
        inner.adopt_group(account, &mut found);
        Ok(found)
    }

    async fn create_group(&self, account: &AccountId, new: &NewGroup) -> ProviderResult<Group> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        // A request repeated after a lost answer is the group it made.
        let key = format!("request:{}", new.request_id);
        if let Some(made) = inner.archive.value(account.as_str(), &key).map_err(db)? {
            if let Some(jid) = jid_of(&made) {
                let mut group = session.group(&jid).await.map_err(fail)?;
                inner.adopt_group(account, &mut group);
                return Ok(group);
            }
        }
        let participants: Vec<Jid> = new
            .participants
            .iter()
            .filter_map(|contact| jid_of(contact.as_str()))
            .collect();
        let parent = match &new.in_community {
            Some(community) => Some(inner.jid(community.as_str())?),
            None => None,
        };
        let mut group = session
            .create_group(new, &participants, parent.as_ref())
            .await
            .map_err(fail)?;
        inner
            .archive
            .set_value(account.as_str(), &key, Some(group.id.as_str()))
            .map_err(db)?;
        inner.adopt_group(account, &mut group);
        Ok(group)
    }

    async fn update_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: &GroupChange,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        session
            .change_group(&inner.jid(group.as_str())?, change)
            .await
            .map_err(fail)?;
        if let GroupChange::Subject(subject) = change {
            let chat = inner
                .archive
                .update_chat(account.as_str(), group.as_str(), |chat| {
                    chat.name = Some(subject.clone());
                })
                .map_err(db)?;
            inner.chat_changed(account.as_str(), &chat);
        }
        Ok(())
    }

    async fn change_participants(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: ParticipantChange,
        contacts: &[ContactId],
        _request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        let who: Vec<Jid> = contacts
            .iter()
            .filter_map(|contact| jid_of(contact.as_str()))
            .collect();
        session
            .change_participants(&inner.jid(group.as_str())?, change, &who)
            .await
            .map_err(fail)
    }

    async fn set_group_picture(
        &self,
        account: &AccountId,
        group: &ChatId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        let session = self.inner.online(account)?;
        let group = self.inner.jid(group.as_str())?;
        session.set_picture(Some(&group), jpeg).await.map_err(fail)
    }

    async fn group_invite_link(
        &self,
        account: &AccountId,
        group: &ChatId,
        reset: bool,
        request_id: &str,
    ) -> ProviderResult<String> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        // A reset repeated after a lost answer must not reset twice.
        let key = format!("request:{request_id}");
        if reset {
            if let Some(link) = inner.archive.value(account.as_str(), &key).map_err(db)? {
                return Ok(link);
            }
        }
        let link = session
            .invite_link(&inner.jid(group.as_str())?, reset)
            .await
            .map_err(fail)?;
        if reset {
            inner
                .archive
                .set_value(account.as_str(), &key, Some(&link))
                .map_err(db)?;
        }
        Ok(link)
    }

    async fn join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> ProviderResult<Vec<JoinRequest>> {
        let session = self.inner.online(account)?;
        session
            .join_requests(&self.inner.jid(group.as_str())?)
            .await
            .map_err(fail)
    }

    async fn answer_join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
        approve: bool,
        contacts: &[ContactId],
        _request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        let session = self.inner.online(account)?;
        let who: Vec<Jid> = contacts
            .iter()
            .filter_map(|contact| jid_of(contact.as_str()))
            .collect();
        session
            .answer_join_requests(&self.inner.jid(group.as_str())?, approve, &who)
            .await
            .map_err(fail)
    }

    async fn leave_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        _request_id: &str,
    ) -> ProviderResult<()> {
        let session = self.inner.online(account)?;
        match session.leave_group(&self.inner.jid(group.as_str())?).await {
            // Leaving a group the account is not in is having left it.
            Err(SessionError::Refused { code, .. }) if code == refusal::NOT_MEMBER => Ok(()),
            other => other.map_err(fail),
        }
    }

    async fn link_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
        _request_id: &str,
    ) -> ProviderResult<()> {
        let session = self.inner.online(account)?;
        session
            .link_subgroup(
                &self.inner.jid(community.as_str())?,
                &self.inner.jid(group.as_str())?,
            )
            .await
            .map_err(fail)
    }

    async fn unlink_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
    ) -> ProviderResult<()> {
        let session = self.inner.online(account)?;
        session
            .unlink_subgroup(
                &self.inner.jid(community.as_str())?,
                &self.inner.jid(group.as_str())?,
            )
            .await
            .map_err(fail)
    }

    async fn community_participants(
        &self,
        account: &AccountId,
        community: &ChatId,
    ) -> ProviderResult<Vec<ContactId>> {
        let session = self.inner.online(account)?;
        let people = session
            .community_participants(&self.inner.jid(community.as_str())?)
            .await
            .map_err(fail)?;
        Ok(people
            .iter()
            .map(|jid| ContactId::new(self.inner.canonical(account.as_str(), jid, None)))
            .collect())
    }

    async fn list_stories(&self, account: &AccountId) -> ProviderResult<Vec<Story>> {
        let inner = &self.inner;
        let day = STORY_LIFETIME.as_millis() as i64;
        let since = now_ms() - day;
        // What is past its day goes, here as on WhatsApp.
        let _ = inner
            .archive
            .forget_before(account.as_str(), STATUS_CHAT, since);
        Ok(inner
            .archive
            .since(account.as_str(), STATUS_CHAT, since)
            .map_err(db)?
            .iter()
            .filter_map(|stored| inner.story_model(account, stored))
            .collect())
    }

    async fn post_story(&self, story: NewStory) -> ProviderResult<Story> {
        let inner = &self.inner;
        let account = &story.account_id;
        let id = message_id_for(&story.client_id);
        if inner.already_sent(account, &story.client_id)?.is_none() {
            let message = match &story.content {
                NewStoryContent::Text { text, style } => convert::story_text(text, *style),
                NewStoryContent::Media {
                    kind,
                    media,
                    mime_type,
                    caption,
                } => {
                    let pointer = MediaPointer::parse(media).ok_or_else(|| {
                        rejected("not_uploaded", "The file was not uploaded to WhatsApp.")
                    })?;
                    convert::media_message(
                        &pointer,
                        *kind,
                        mime_type.as_deref(),
                        caption.as_deref(),
                        None,
                        false,
                        None,
                    )
                }
            };
            let session = inner.online(account)?;
            let recipients = inner.story_recipients(account)?;
            session
                .post_status(StatusPost {
                    message: message.clone(),
                    recipients: recipients.clone(),
                    id: id.clone(),
                })
                .await
                .map_err(fail)?;
            inner.keep_sent(
                account,
                STATUS_CHAT,
                &id,
                &story.client_id,
                &message,
                now_ms(),
            )?;
            // Taking it down later names the people it was shown to.
            let shown: Vec<String> = recipients.iter().map(convert::wire).collect();
            let _ = inner.archive.set_value(
                account.as_str(),
                &format!("story_shown:{id}"),
                serde_json::to_string(&shown).ok().as_deref(),
            );
        }
        let stored = inner.stored_story(account, &MessageId::new(id))?;
        inner.story_model(account, &stored).ok_or_else(|| {
            ProviderError::Protocol("the story that was posted cannot be read back".into())
        })
    }

    async fn delete_story(&self, account: &AccountId, story: &MessageId) -> ProviderResult<()> {
        let inner = &self.inner;
        let Ok(stored) = inner.stored_story(account, story) else {
            return Ok(());
        };
        if stored.deleted {
            return Ok(());
        }
        let session = inner.online(account)?;
        let shown: Vec<Jid> = inner
            .archive
            .value(account.as_str(), &format!("story_shown:{story}"))
            .map_err(db)?
            .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
            .map(|wires| wires.iter().filter_map(|wire| wire.parse().ok()).collect())
            .unwrap_or_default();
        let recipients = if shown.is_empty() {
            inner.story_recipients(account)?
        } else {
            shown
        };
        session
            .revoke_status(story.as_str(), &recipients)
            .await
            .map_err(fail)?;
        inner
            .archive
            .mark_deleted(account.as_str(), STATUS_CHAT, story.as_str())
            .map_err(db)?;
        Ok(())
    }

    async fn view_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        author: &ContactId,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        let status: Jid = inner.jid(STATUS_CHAT)?;
        let author = inner.jid(author.as_str())?;
        session
            .mark_read(&status, Some(&author), &[story.to_string()])
            .await
            .map_err(fail)?;
        let _ = inner
            .archive
            .advance(account.as_str(), STATUS_CHAT, story.as_str(), READ);
        Ok(())
    }

    async fn story_viewers(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> ProviderResult<Vec<StoryViewer>> {
        Ok(self
            .inner
            .archive
            .views(account.as_str(), story.as_str())
            .map_err(db)?
            .into_iter()
            .map(|view| StoryViewer {
                contact: ContactId::new(view.viewer),
                name: None,
                viewed_at: Timestamp::from_millis(view.at_ms),
                reaction: view.reaction,
            })
            .collect())
    }

    async fn reply_to_story(
        &self,
        story: &Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        let inner = &self.inner;
        let account = &message.account_id;
        let OutgoingContent::Text { body } = &message.content else {
            return Err(rejected("invalid", "A reply to a status is a text."));
        };
        let stored = inner.stored_story(account, &story.id)?;
        let author = inner.jid(story.author.as_str())?;
        let status = inner.jid(STATUS_CHAT)?;
        let context = convert::context(Some((&stored, Some(&author))), Some(&status), &[], false);
        inner
            .deliver(
                account,
                message.chat_id.as_str(),
                &message.client_id,
                convert::text_message(body, context),
            )
            .await
    }

    async fn react_to_story(
        &self,
        story: &Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        let inner = &self.inner;
        let account = &message.account_id;
        let OutgoingContent::Reaction { emoji, .. } = &message.content else {
            return Err(rejected("invalid", "A reaction to a status is an emoji."));
        };
        if let Some(receipt) = inner.already_sent(account, &message.client_id)? {
            return Ok(receipt);
        }
        let stored = inner.stored_story(account, &story.id)?;
        let target = inner.target_of(&stored)?;
        let session = inner.online(account)?;
        let id = session.react(&target, emoji).await.map_err(fail)?;
        let at_ms = now_ms();
        inner
            .archive
            .record_send(account.as_str(), message.client_id.as_str(), &id, at_ms)
            .map_err(db)?;
        Ok(SendReceipt {
            message_id: MessageId::new(id),
            status: DeliveryStatus::Sent,
            timestamp: Some(Timestamp::from_millis(at_ms)),
        })
    }

    async fn muted_story_authors(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        Ok(self
            .inner
            .archive
            .values(account.as_str(), "story_mute:")
            .map_err(db)?
            .into_iter()
            .filter_map(|(key, _)| key.strip_prefix("story_mute:").map(ContactId::new))
            .collect())
    }

    async fn set_story_muted(
        &self,
        account: &AccountId,
        author: &ContactId,
        muted: bool,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let session = inner.online(account)?;
        session
            .mute_stories(&inner.jid(author.as_str())?, muted)
            .await
            .map_err(fail)?;
        inner
            .archive
            .set_value(
                account.as_str(),
                &format!("story_mute:{author}"),
                muted.then_some("1"),
            )
            .map_err(db)
    }

    async fn story_privacy(&self, account: &AccountId) -> ProviderResult<StoryPrivacy> {
        self.inner.story_privacy(account)
    }

    async fn create_account(&self, new: &NewAccount) -> ProviderResult<LinkStatus> {
        let inner = &self.inner;
        // A request repeated after a lost answer is the number it made.
        let key = format!("request:{}", new.request_id);
        if let Some(id) = inner.archive.value("", &key).map_err(db)? {
            if let Some(row) = inner.archive.account(&id).map_err(db)? {
                let status = inner.link_status(&row);
                // The number was made and its code was not given yet (the
                // session was still starting): this is the call that asks.
                let coded = matches!(status.step, LinkStep::TypeCode { .. } | LinkStep::Linked);
                return match &new.pairing_phone {
                    Some(phone) if !coded && !row.ever_linked => {
                        self.pairing_code(&AccountId::new(row.id), phone).await
                    }
                    _ => Ok(status),
                };
            }
        }
        let number = inner
            .archive
            .accounts()
            .map_err(db)?
            .iter()
            .filter_map(|row| row.id.strip_prefix("native:")?.parse::<i32>().ok())
            .max()
            .unwrap_or(0)
            + 1;
        let row = StoredAccount {
            id: account_id(number),
            device: 0,
            label: new.name.clone().filter(|name| !name.trim().is_empty()),
            history: new.history == HistoryImport::Recent,
            ..StoredAccount::default()
        };
        inner.archive.put_account(&row).map_err(db)?;
        inner
            .archive
            .set_value("", &key, Some(&row.id))
            .map_err(db)?;
        inner
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                row.id.clone(),
                Live {
                    session: None,
                    device: 0,
                    connection: ConnectionState::Connecting,
                    step: LinkStep::Starting,
                    qr: None,
                },
            );
        inner.open(&row, None).await;
        let row = inner.row(&AccountId::new(row.id))?;
        if let Some(phone) = &new.pairing_phone {
            return self
                .pairing_code(&AccountId::new(row.id.clone()), phone)
                .await;
        }
        Ok(inner.link_status(&row))
    }

    async fn link_status(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        let row = self.inner.row(account)?;
        Ok(self.inner.link_status(&row))
    }

    async fn pairing_code(&self, account: &AccountId, phone: &str) -> ProviderResult<LinkStatus> {
        let inner = &self.inner;
        let row = inner.row(account)?;
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        if digits.len() < 5 {
            return Err(rejected("invalid", "That is not a phone number."));
        }
        let session = inner.session(account)?;
        let code = session.pair_code(&digits).await.map_err(fail)?;
        // WhatsApp shows the code in two halves.
        let shown = match code.len() {
            8 => format!("{}-{}", &code[..4], &code[4..]),
            _ => code,
        };
        inner.change_live(account.as_str(), |live| {
            live.step = LinkStep::TypeCode {
                code: shown,
                expires_at: None,
            };
        });
        Ok(inner.link_status(&row))
    }

    async fn scan_instead(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        let inner = &self.inner;
        let row = inner.row(account)?;
        let session = inner.session(account)?;
        session.cancel_pair_code().await.map_err(fail)?;
        inner.change_live(account.as_str(), |live| {
            live.step = match live.qr.as_deref().and_then(qr_png) {
                Some(png) => LinkStep::Scan { png },
                None => LinkStep::Starting,
            };
        });
        Ok(inner.link_status(&row))
    }

    async fn update_account(
        &self,
        account: &AccountId,
        change: AccountChange,
    ) -> ProviderResult<Account> {
        let inner = &self.inner;
        let mut row = inner.row(account)?;
        match change {
            AccountChange::Rename(name) => {
                row.label = Some(name).filter(|name| !name.trim().is_empty())
            }
            AccountChange::History(history) => row.history = history == HistoryImport::Recent,
        }
        inner.archive.put_account(&row).map_err(db)?;
        Ok(inner.to_account(&row))
    }

    async fn reconnect_account(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        let inner = &self.inner;
        let row = inner.row(account)?;
        let live = inner.live_of(account.as_str());
        let logged_out = live
            .as_ref()
            .is_some_and(|live| live.connection == ConnectionState::LoggedOut);
        match live.and_then(|live| live.session) {
            Some(session) if !logged_out => session.reconnect().await,
            stale => {
                // The link is gone: the number is linked again, from
                // nothing, and keeps its place and its chats here.
                if let Some(session) = stale {
                    session.shutdown().await;
                }
                if row.device != 0 {
                    let _ = inner.sessions.remove(row.device).await;
                }
                inner.change_live(account.as_str(), |live| {
                    live.session = None;
                    live.connection = ConnectionState::Connecting;
                    live.step = LinkStep::Starting;
                    live.qr = None;
                });
                inner.open(&row, None).await;
            }
        }
        let row = inner.row(account)?;
        Ok(inner.link_status(&row))
    }

    async fn unlink_account(&self, account: &AccountId) -> ProviderResult<Account> {
        let inner = &self.inner;
        let row = inner.row(account)?;
        if let Some(session) = inner
            .live_of(account.as_str())
            .and_then(|live| live.session)
        {
            // A number that is offline cannot tell the phone; the session's
            // keys go from this machine either way.
            let _ = session.logout().await;
            session.shutdown().await;
        }
        inner.change_live(account.as_str(), |live| {
            live.connection = ConnectionState::LoggedOut;
            live.step = LinkStep::Stopped {
                reason: "This number was logged out.".into(),
            };
        });
        inner.emit(ProviderEvent::ConnectionChanged {
            account_id: account.clone(),
            state: ConnectionState::LoggedOut,
        });
        Ok(inner.to_account(&row))
    }

    async fn delete_account(&self, account: &AccountId) -> ProviderResult<()> {
        let inner = &self.inner;
        let Ok(row) = inner.row(account) else {
            return Ok(());
        };
        let live = inner
            .live
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(account.as_str());
        if let Some(session) = live.and_then(|live| live.session) {
            if row.ever_linked {
                let _ = session.logout().await;
            }
            session.shutdown().await;
        }
        if row.device != 0 {
            inner.sessions.remove(row.device).await.map_err(fail)?;
        }
        inner
            .archive
            .clear_account(account.as_str(), true)
            .map_err(db)?;
        Ok(())
    }

    async fn start_call(
        &self,
        account: &AccountId,
        chat: &ChatId,
        video: bool,
    ) -> ProviderResult<String> {
        let inner = &self.inner;
        if video && !crate::call_video::available() {
            return Err(ProviderError::Unsupported("video calls"));
        }
        let session = inner.online(account)?;
        let peer = inner.jid(chat.as_str())?;
        let call = session.start_call(&peer, video).await.map_err(fail)?;
        inner
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                call.clone(),
                HeldCall {
                    video,
                    answered: false,
                    incoming: false,
                    connected_ms: None,
                },
            );
        inner.emit(ProviderEvent::Call(CallEvent {
            account_id: account.clone(),
            call_id: call.clone(),
            chat_id: chat.clone(),
            peer: ContactId::new(chat.as_str()),
            video,
            state: CallState::Ringing {
                incoming: false,
                group: false,
            },
            at: Timestamp::now(),
        }));
        Ok(call)
    }

    async fn accept_call(&self, account: &AccountId, call: &str) -> ProviderResult<()> {
        let inner = &self.inner;
        let held = inner
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(call)
            .cloned();
        match held {
            // Answering a call that is on already is having answered it.
            Some(held) if held.answered => Ok(()),
            Some(_) => inner.online(account)?.accept_call(call).await.map_err(fail),
            None => Err(rejected(refusal::NOT_FOUND, "This call is over.")),
        }
    }

    async fn end_call(&self, account: &AccountId, call: &str) -> ProviderResult<()> {
        let inner = &self.inner;
        let held = inner
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(call);
        if !held {
            // A call that is over was ended.
            return Ok(());
        }
        inner.session(account)?.end_call(call).await.map_err(fail)
    }

    async fn mute_call(&self, account: &AccountId, call: &str, muted: bool) -> ProviderResult<()> {
        self.inner
            .session(account)?
            .mute_call(call, muted)
            .await
            .map_err(fail)
    }

    async fn invite_to_call(
        &self,
        account: &AccountId,
        call: &str,
        contact: &ContactId,
    ) -> ProviderResult<()> {
        let inner = &self.inner;
        let guest = inner.jid(contact.as_str())?;
        inner
            .session(account)?
            .invite_to_call(call, &guest)
            .await
            .map_err(fail)
    }

    async fn set_call_camera(
        &self,
        account: &AccountId,
        call: &str,
        on: bool,
    ) -> ProviderResult<()> {
        self.inner
            .session(account)?
            .set_call_camera(call, on)
            .await
            .map_err(fail)
    }

    fn call_video(&self, account: &AccountId, call: &str) -> Option<client_provider::CallVideo> {
        self.inner.session(account).ok()?.call_video(call)
    }

    fn set_call_devices(&self, devices: client_provider::CallDevices) {
        crate::call_audio::choose(devices);
    }

    fn call_cameras(&self) -> Vec<client_provider::CallCamera> {
        crate::call_video::cameras()
    }

    async fn subscribe(&self) -> ProviderResult<EventStream> {
        let events = self.inner.events.subscribe();
        // What happened before anybody listened is said again: a number
        // that connected while the engine was starting is connected.
        let now: Vec<ProviderEvent> = {
            let live = self
                .inner
                .live
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            live.iter()
                .map(|(id, live)| ProviderEvent::ConnectionChanged {
                    account_id: AccountId::new(id.clone()),
                    state: live.connection.clone(),
                })
                .collect()
        };
        // A listener that fell behind missed events; ending the stream
        // makes the engine subscribe again and read everything once.
        let later = futures::stream::unfold(events, |mut events| async move {
            match events.recv().await {
                Ok(event) => Some((event, events)),
                Err(_) => None,
            }
        });
        Ok(futures::stream::iter(now).chain(later).boxed())
    }
}

impl Inner {
    /// Takes a group as the session read it: gives it its account, and
    /// keeps its name for the chat list.
    fn adopt_group(&self, account: &AccountId, group: &mut Group) {
        group.account_id = account.clone();
        if group.subject.trim().is_empty() {
            return;
        }
        let before = self
            .archive
            .chat(account.as_str(), group.id.as_str())
            .ok()
            .flatten();
        let renamed = before
            .as_ref()
            .is_some_and(|chat| chat.name.as_deref() != Some(group.subject.as_str()));
        if renamed {
            if let Ok(chat) =
                self.archive
                    .update_chat(account.as_str(), group.id.as_str(), |chat| {
                        chat.name = Some(group.subject.clone());
                    })
            {
                self.chat_changed(account.as_str(), &chat);
            }
        }
    }

    pub(crate) fn delivered_rank(read: bool) -> u8 {
        if read {
            READ
        } else {
            DELIVERED
        }
    }
}
