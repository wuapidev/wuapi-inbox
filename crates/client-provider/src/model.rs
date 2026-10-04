//! The neutral model every provider speaks.
//!
//! These types describe WhatsApp concepts without any provider's wire format.
//! An adapter's job is to translate its backend into them, and nothing else:
//! persistence, search, retries and rendering all live on the client side.

use crate::content::{CalendarEvent, ContactCard, Location, MessageExtras, Poll, SystemEvent};
use crate::ids::{AccountId, ChatId, ClientMessageId, ContactId, Cursor, MediaRef, MessageId};
use serde::{Deserialize, Serialize};

/// A point in time, in milliseconds since the Unix epoch (UTC).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Timestamp(pub i64);

impl Timestamp {
    /// Builds a timestamp from milliseconds since the Unix epoch.
    pub const fn from_millis(ms: i64) -> Self {
        Self(ms)
    }

    /// Milliseconds since the Unix epoch.
    pub const fn as_millis(self) -> i64 {
        self.0
    }

    /// The current wall-clock time.
    pub fn now() -> Self {
        let since_epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Self(since_epoch.as_millis() as i64)
    }
}

/// Whether an account can currently talk to WhatsApp.
///
/// Connections drop and come back all the time. Report `Reconnecting` for a
/// drop the provider expects to recover from on its own and reserve
/// `Disconnected` / `LoggedOut` for states that need the user: the client
/// keeps queued messages waiting through the former and only surfaces the
/// latter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ConnectionState {
    /// The session is starting or linking.
    Connecting,
    /// Linked and online: sends go out, events come in.
    Connected,
    /// Dropped, recovering on its own. Not an error.
    Reconnecting,
    /// Offline and not coming back without intervention.
    Disconnected {
        /// Provider-specific explanation, shown to the user as is.
        reason: Option<String>,
    },
    /// The device was unlinked or banned. Terminal: the account has to be
    /// linked again.
    LoggedOut,
}

impl ConnectionState {
    /// True while messages can be sent right now.
    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected)
    }

    /// True when the account will not recover without the user.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::LoggedOut)
    }
}

impl Account {
    /// A number that was created and never linked to a phone.
    pub fn never_linked(&self) -> bool {
        self.settings.ever_linked == Some(false) && !self.connection.is_connected()
    }
}

/// One linked WhatsApp number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    /// Stable id of the account.
    pub id: AccountId,
    /// Name to show in the account rail: the WhatsApp profile name or the
    /// user's own label for the number.
    pub display_name: String,
    /// The linked number in E.164 (`+584245550199`), when known.
    pub phone: Option<String>,
    /// The account's own contact id, i.e. the `sender` of its outgoing
    /// messages, when known.
    pub self_contact: Option<ContactId>,
    /// Current connection state.
    pub connection: ConnectionState,
    /// What the provider does on its side for this number.
    #[serde(default)]
    pub settings: AccountSettings,
}

/// Whether the provider imports the chats a phone hands over when a number
/// is linked. WhatsApp sends that history once, right after linking.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryImport {
    /// Nothing is imported: only what arrives after linking is kept.
    Off,
    /// The recent chats the phone sends are imported.
    Recent,
}

/// Which received files the provider downloads by itself, before anyone
/// asks for them. The rest stays on WhatsApp until requested.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerMedia {
    /// Nothing up front: every file is fetched when it is first wanted.
    OnDemand,
    /// Every file, as it arrives.
    Everything,
    /// Only some kinds, up to a size.
    Some {
        /// The largest file downloaded up front, in bytes.
        max_bytes: u64,
        /// The kinds downloaded up front, as the provider names them.
        kinds: Vec<String>,
    },
}

/// Provider-side settings of a number. `None` means the provider does not
/// say, or has no such setting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSettings {
    /// What happens to the phone's history at the next link.
    pub history_import: Option<HistoryImport>,
    /// Which received files are downloaded up front.
    pub server_media: Option<ServerMedia>,
    /// Whether a phone has ever been linked to this number. `Some(false)`
    /// is a number that was created and never linked: it holds nothing,
    /// and is not a session that broke.
    #[serde(default)]
    pub ever_linked: Option<bool>,
}

/// A place a number can connect to WhatsApp from, for providers that let
/// the user choose (a proxy exit, a region). Shown as "city, country".
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LinkPlace {
    /// ISO 3166-1 alpha-2 country code, uppercase.
    pub country: String,
    /// The country's name, for display.
    pub country_name: String,
    /// The provider's code of the city.
    pub city: String,
    /// The city's name, for display.
    pub city_name: String,
}

/// A number to link.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAccount {
    /// The user's label for the number.
    pub name: Option<String>,
    /// Where it connects from. Required by providers that list
    /// [`Provider::link_places`](crate::Provider::link_places).
    pub place: Option<LinkPlace>,
    /// Link by typing a code on the phone instead of scanning: the number
    /// to link, as the user typed it.
    pub pairing_phone: Option<String>,
    /// Whether the phone's recent chats are imported when it links.
    pub history: HistoryImport,
    /// Made once per attempt by the client and sent again on every retry,
    /// so that a request repeated after a dropped connection creates one
    /// number, not two.
    pub request_id: String,
}

/// Where linking a number stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkStep {
    /// The session is starting: nothing to show yet.
    Starting,
    /// A QR code to scan with the phone, as a PNG image.
    Scan {
        /// The image.
        png: Vec<u8>,
    },
    /// A code to type on the phone.
    TypeCode {
        /// The code, as shown (`ABCD-1234`).
        code: String,
        /// When it stops working, if the provider says.
        expires_at: Option<Timestamp>,
    },
    /// The phone accepted; the link is being finished.
    Finishing,
    /// Linked and connected.
    Linked,
    /// Linking stopped and will not go on by itself.
    Stopped {
        /// Why, in words for the user.
        reason: String,
    },
}

/// A number and where its linking stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkStatus {
    /// The number, as the provider has it now.
    pub account: Account,
    /// The step it is at.
    pub step: LinkStep,
    /// Whether a phone has ever been linked to it. A number that never
    /// was holds nothing, and can be deleted when linking is cancelled.
    pub was_linked: bool,
}

/// A change to a number's own settings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountChange {
    /// The user's label for the number.
    Rename(String),
    /// Whether history is imported at the next link.
    History(HistoryImport),
}

/// What kind of conversation a chat is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatKind {
    /// A conversation with one contact.
    Direct,
    /// A group conversation.
    Group,
}

/// A conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chat {
    /// Stable id of the chat, unique within its account.
    pub id: ChatId,
    /// The account the chat belongs to.
    pub account_id: AccountId,
    /// 1:1 or group.
    pub kind: ChatKind,
    /// Name to show: the contact's name or the group subject. Providers that
    /// cannot resolve one should fall back to the phone number.
    pub title: String,
    /// Handle to the chat's picture, if it has one.
    pub avatar: Option<MediaRef>,
    /// Messages the user has not read yet, as far as the provider knows.
    pub unread_count: u32,
    /// Pinned to the top of the chat list.
    pub pinned: bool,
    /// Notifications silenced.
    pub muted: bool,
    /// Moved to the archive.
    pub archived: bool,
    /// The most recent message, when the provider's chat listing includes
    /// it. Lets the client show a preview before any history is fetched.
    pub last_message: Option<Message>,
    /// Which of `pinned`, `muted` and `archived` the provider does not
    /// actually know. The client keeps what it has for those instead of
    /// taking the placeholder beside them.
    pub unknown: ChatUnknown,
    /// The id of the chat's picture, when the chat listing carries it. It
    /// changes when the picture does, so the client asks for the picture
    /// only when this differs from the one it holds. `None`: no picture
    /// the provider knows of, or the provider does not say (see
    /// [`ChatUnknown::picture`]).
    pub picture_id: Option<String>,
    /// When the chat was pinned, when the provider says. Pinned chats are
    /// listed the last one pinned first; one without a time comes after
    /// those that have one. `None`: not pinned, or the time is not known.
    #[serde(default)]
    pub pinned_at: Option<Timestamp>,
}

/// The parts of a chat's state a provider has no knowledge of.
///
/// Some backends learn a chat's pin, mute or archive state only when it
/// changes after linking; until then they cannot say. A provider sets the
/// flag for what it does not know (and leaves the field itself `false`),
/// and the client then keeps its own value: a chat the user just pinned
/// stays pinned when the provider's list still has nothing to say about
/// it. The default is "everything is known".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatUnknown {
    /// `pinned` is a placeholder.
    pub pinned: bool,
    /// `muted` is a placeholder.
    pub muted: bool,
    /// `archived` is a placeholder.
    pub archived: bool,
    /// `picture_id` says nothing: the listing this chat came from does
    /// not carry picture ids at all.
    pub picture: bool,
    /// `unread_count` is a placeholder: the provider does not know how
    /// many messages of this chat are unread. The client then keeps its
    /// own count, and counts what arrives.
    pub unread: bool,
}

/// A change to a chat's state, asked for by the user.
///
/// Every variant sets a value rather than toggling one, so applying a
/// change twice is the same as applying it once: the client repeats a
/// change whose answer was lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatChange {
    /// Pin to, or unpin from, the top of the chat list.
    Pinned(bool),
    /// Silence, or restore, the chat's notifications.
    Muted(bool),
    /// Silence the chat for this many seconds; the provider restores its
    /// notifications after. A provider that cannot time a mute mutes
    /// until it is unmuted.
    MutedFor(u64),
    /// Move to, or out of, the archive.
    Archived(bool),
    /// Flag the chat as unread without a new message. Reading it again is
    /// [`Provider::mark_read`](crate::Provider::mark_read).
    MarkedUnread,
}

/// A person, as an account sees them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contact {
    /// Stable id of the contact.
    pub id: ContactId,
    /// The account that sees this contact.
    pub account_id: AccountId,
    /// Best name available: address-book name, else WhatsApp profile name.
    pub name: Option<String>,
    /// Phone number in E.164, when the contact does not hide it.
    pub phone: Option<String>,
    /// Handle to the profile picture, if visible.
    pub avatar: Option<MediaRef>,
    /// The name the account saved the contact under in its address book.
    #[serde(default)]
    pub saved_name: Option<String>,
    /// The name the contact gave themselves on WhatsApp.
    #[serde(default)]
    pub profile_name: Option<String>,
    /// The verified name of a business.
    #[serde(default)]
    pub business_name: Option<String>,
    /// The contact's WhatsApp username, without the `@`.
    #[serde(default)]
    pub username: Option<String>,
    /// The contact's "About" text, when known.
    #[serde(default)]
    pub about: Option<String>,
    /// The id of the contact's picture, when the provider's contact list
    /// carries it (see [`Chat::picture_id`]).
    #[serde(default)]
    pub picture_id: Option<String>,
    /// Other ids the same person goes by on this account: the number's
    /// id for a contact listed by a hidden-number id, and the other way
    /// round. Messages, mentions and group participants may name the
    /// person by any of them; the client keeps them together.
    #[serde(default)]
    pub alt_ids: Vec<ContactId>,
}

impl Contact {
    /// A contact about which only the id is known.
    pub fn new(account_id: AccountId, id: ContactId) -> Self {
        Self {
            id,
            account_id,
            name: None,
            phone: None,
            avatar: None,
            saved_name: None,
            profile_name: None,
            business_name: None,
            username: None,
            about: None,
            picture_id: None,
            alt_ids: Vec::new(),
        }
    }

    /// The name to show: the saved one, else the business's, else the
    /// profile's, else whatever `name` holds, else the number, else the id.
    pub fn display_name(&self) -> String {
        [
            &self.saved_name,
            &self.business_name,
            &self.profile_name,
            &self.name,
            &self.phone,
        ]
        .into_iter()
        .flatten()
        .find(|text| !text.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| self.id.to_string())
    }
}

/// Whether a phone number has WhatsApp.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NumberCheck {
    /// The number, in E.164.
    pub phone: String,
    /// It has WhatsApp.
    pub on_whatsapp: bool,
    /// The chat to write to it, when it has.
    pub chat: Option<ChatId>,
    /// The verified name of a business, when it is one.
    pub business_name: Option<String>,
}

/// Who sent a message, relative to the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Received from someone else.
    Incoming,
    /// Sent by the account, from this client or any other of its devices.
    Outgoing,
}

/// How far an outgoing message got. Incoming messages are always
/// [`DeliveryStatus::Delivered`] or [`DeliveryStatus::Read`] (read by the
/// user).
///
/// The statuses are ordered: a message only moves forward
/// (`Pending < Accepted < Sent < Delivered < Read`). `Failed` is terminal
/// and must only be reported for errors that retrying cannot fix.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DeliveryStatus {
    /// Written on this device and not handed to the provider yet, or not
    /// known to have arrived there (clock icon).
    Pending,
    /// The provider has it and will put it on WhatsApp: queued, paced or
    /// waiting for the number to reconnect. From here on the message is
    /// safe even if this device goes away (a faint tick).
    Accepted,
    /// WhatsApp's servers have it (one tick).
    Sent,
    /// It reached the recipient's phone (two ticks).
    Delivered,
    /// The recipient read it (two blue ticks).
    Read,
    /// It was not sent and never will be.
    Failed {
        /// Why, in words the user can read.
        reason: String,
    },
}

impl DeliveryStatus {
    /// Position in the forward-only progression; `Failed` ranks lowest so a
    /// late success can still replace it.
    pub fn rank(&self) -> u8 {
        match self {
            Self::Failed { .. } => 0,
            Self::Pending => 1,
            Self::Accepted => 2,
            Self::Sent => 3,
            Self::Delivered => 4,
            Self::Read => 5,
        }
    }

    /// True when `next` may replace `self`: statuses never move backwards.
    /// Status updates arrive out of order on flaky links, so every consumer
    /// should apply them through this check.
    pub fn can_advance_to(&self, next: &DeliveryStatus) -> bool {
        match (self, next) {
            // A provider may fail a message it had only queued.
            (Self::Pending | Self::Accepted, Self::Failed { .. }) => true,
            (_, Self::Failed { .. }) => false,
            _ => next.rank() > self.rank(),
        }
    }
}

/// The broad type of a media attachment; picks the bubble layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A photo.
    Image,
    /// A video.
    Video,
    /// An audio file.
    Audio,
    /// A voice note.
    Voice,
    /// Any other file.
    Document,
    /// A sticker.
    Sticker,
}

/// A media attachment. The bytes are not included: fetch them on demand with
/// [`Provider::download_media`](crate::Provider::download_media).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Media {
    /// Photo, video, voice note...
    pub kind: MediaKind,
    /// Handle to download the payload. `None` when the provider knows the
    /// attachment exists but cannot serve it (expired, not yet uploaded).
    pub source: Option<MediaRef>,
    /// MIME type, when known.
    pub mime_type: Option<String>,
    /// Original file name, for documents.
    pub file_name: Option<String>,
    /// Caption shown under the media.
    pub caption: Option<String>,
    /// Payload size in bytes, when known.
    pub size_bytes: Option<u64>,
    /// Pixel width, for images and videos, when known.
    pub width: Option<u32>,
    /// Pixel height, for images and videos, when known.
    pub height: Option<u32>,
    /// Duration in seconds, for audio and video, when known.
    pub duration_secs: Option<u32>,
    /// A video meant to be played as a GIF: short, silent, looping.
    /// `false` when the provider cannot tell.
    #[serde(default)]
    pub gif: bool,
}

impl Media {
    /// A media descriptor with only its kind set.
    pub fn new(kind: MediaKind) -> Self {
        Self {
            kind,
            source: None,
            mime_type: None,
            file_name: None,
            caption: None,
            size_bytes: None,
            width: None,
            height: None,
            duration_secs: None,
            gif: false,
        }
    }
}

/// What a message carries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MessageContent {
    /// Plain text.
    Text {
        /// The text, with WhatsApp's inline formatting left as typed.
        body: String,
    },
    /// A photo, video, audio, document or sticker.
    Media(Media),
    /// A reaction to another message. Reactions travel as messages because
    /// that is what they are on the wire; the client folds them into the
    /// target's reaction chips and never shows them as bubbles.
    Reaction {
        /// The message reacted to.
        target: MessageId,
        /// One emoji. Empty removes the sender's reaction.
        emoji: String,
    },
    /// A place, or where the sender is right now.
    Location(Location),
    /// One or more contact cards.
    Contacts {
        /// The cards, in the order they were sent.
        cards: Vec<ContactCard>,
    },
    /// A poll with its tally.
    Poll(Poll),
    /// A calendar event.
    Event(CalendarEvent),
    /// A notice from the chat itself (somebody joined, a missed call).
    /// Shown as a centred line, not as a bubble.
    System(SystemEvent),
    /// Something the provider or this client does not model. Rendered as
    /// a neutral tile that names the type: never dropped, since a gap in a
    /// conversation is worse than a placeholder.
    Unsupported {
        /// What the provider calls it, e.g. `"hologram"`. Shown as is.
        description: String,
    },
}

impl MessageContent {
    /// Shorthand for a text body.
    pub fn text(body: impl Into<String>) -> Self {
        Self::Text { body: body.into() }
    }
}

/// A pointer to the message another message quotes.
///
/// `sender_name` and `preview` are a snapshot for rendering the quote when
/// the quoted message is not in the local store. Providers that only know
/// the id leave them `None` and the client resolves the quote locally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyRef {
    /// The quoted message.
    pub message_id: MessageId,
    /// Display name of the quoted message's author, if known.
    pub sender_name: Option<String>,
    /// A short excerpt of the quoted message, if known.
    pub preview: Option<String>,
}

/// One message in a chat.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// The provider's id for the message.
    pub id: MessageId,
    /// The id this client generated when it sent the message, echoed back.
    ///
    /// Providers must return it on every copy of a message that was sent
    /// through [`Provider::send`](crate::Provider::send) (in the receipt's
    /// follow-up events and in history), so the client can match the server
    /// copy with the bubble it is already showing instead of duplicating it.
    /// `None` for messages that did not originate from this client.
    pub client_id: Option<ClientMessageId>,
    /// The account the message belongs to.
    pub account_id: AccountId,
    /// The chat the message belongs to.
    pub chat_id: ChatId,
    /// Who wrote it. For outgoing messages, the account's own contact id.
    pub sender: ContactId,
    /// The author's display name at the time, when known. Shown above
    /// incoming bubbles in groups.
    pub sender_name: Option<String>,
    /// Incoming or outgoing.
    pub direction: Direction,
    /// When WhatsApp says the message was sent.
    pub timestamp: Timestamp,
    /// The payload.
    pub content: MessageContent,
    /// The message this one quotes, if it is a reply.
    pub reply_to: Option<ReplyRef>,
    /// Delivery progress.
    pub status: DeliveryStatus,
    /// The text was edited after sending.
    pub edited: bool,
    /// Deleted for everyone; the content is gone.
    pub deleted: bool,
    /// What rides along with it: forwarded, starred, view once, mentions,
    /// a link preview.
    #[serde(default)]
    pub extras: MessageExtras,
}

/// What the user is sending.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OutgoingContent {
    /// Plain text.
    Text {
        /// The text to send.
        body: String,
    },
    /// A file. Only offered by the UI when
    /// [`Capabilities::media_upload`](crate::Capabilities) is set.
    ///
    /// The client keeps the bytes itself and hands them to
    /// [`Provider::upload_media`](crate::Provider::upload_media) first;
    /// what [`Provider::send`](crate::Provider::send) receives is the
    /// reference that upload answered.
    Media {
        /// Photo, video, voice note, document...
        kind: MediaKind,
        /// The uploaded file, as `upload_media` named it. (In the client's
        /// own queue, before the upload: its local copy.)
        media: MediaRef,
        /// MIME type, when known.
        mime_type: Option<String>,
        /// Caption to send with it.
        caption: Option<String>,
        /// The file's name, for documents.
        #[serde(default)]
        file_name: Option<String>,
        /// A video to be played as a GIF: silent, looping. Only for
        /// [`MediaKind::Video`]; a provider that cannot flag it sends the
        /// video as it is.
        #[serde(default)]
        gif: bool,
    },
    /// A reaction. Only offered when
    /// [`Capabilities::reactions`](crate::Capabilities) is set.
    Reaction {
        /// The message to react to.
        target: MessageId,
        /// One emoji. Empty removes the reaction.
        emoji: String,
    },
    /// An existing message passed on, whatever it carries. Only offered
    /// when [`Capabilities::forward_any`](crate::Capabilities) is set,
    /// and never given to [`Provider::send`](crate::Provider::send): the
    /// client hands it to
    /// [`Provider::forward_messages`](crate::Provider::forward_messages),
    /// which answers like a send.
    Forward {
        /// The message to pass on, by the provider's id.
        source: MessageId,
    },
    /// A new poll. Only offered when
    /// [`Capabilities::polls`](crate::Capabilities) is set.
    Poll {
        /// The question.
        question: String,
        /// The answers: at least two, all different.
        options: Vec<String>,
        /// How many answers a voter may pick; 0 means any number.
        max_choices: u32,
    },
}

/// One message to pass on to one chat
/// ([`Provider::forward_messages`](crate::Provider::forward_messages)).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForwardItem {
    /// Idempotency key of this copy, as for
    /// [`OutgoingMessage::client_id`]: the client repeats the identical
    /// item until the provider answers, and the provider must make one
    /// message of it.
    pub client_id: ClientMessageId,
    /// The message to pass on.
    pub message: MessageId,
    /// The chat it goes to.
    pub to: ChatId,
}

/// A file to hand to a provider before a message can refer to it.
#[derive(Clone, PartialEq, Eq)]
pub struct MediaUpload {
    /// The same for every attempt at uploading this file for this
    /// message: a provider uses it so that an upload repeated after a
    /// dropped connection is one upload, not two.
    pub key: String,
    /// What it will be sent as.
    pub kind: MediaKind,
    /// The bytes.
    pub bytes: std::sync::Arc<Vec<u8>>,
    /// Its MIME type.
    pub mime_type: String,
    /// Its name, when it has one.
    pub file_name: Option<String>,
}

impl std::fmt::Debug for MediaUpload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the bytes.
        f.debug_struct("MediaUpload")
            .field("key", &self.key)
            .field("kind", &self.kind)
            .field("bytes", &self.bytes.len())
            .field("mime_type", &self.mime_type)
            .finish_non_exhaustive()
    }
}

/// Told how many bytes of an upload have gone out so far. May be called
/// from any thread, often.
pub type UploadProgress = std::sync::Arc<dyn Fn(u64) + Send + Sync>;

/// A message on its way out.
///
/// The client writes it to its outbox before the first attempt and resubmits
/// the identical value, same `client_id` included, until the provider either
/// accepts it or rejects it for good.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutgoingMessage {
    /// Idempotency key. See [`ClientMessageId`].
    pub client_id: ClientMessageId,
    /// The account to send from.
    pub account_id: AccountId,
    /// The chat to send to.
    pub chat_id: ChatId,
    /// The payload.
    pub content: OutgoingContent,
    /// The message to quote, when replying.
    pub reply_to: Option<MessageId>,
    /// The people the text mentions: the text names each of them as `@`
    /// followed by the mention's `handle`, which is what
    /// [`Provider::mention_handle`](crate::Provider::mention_handle)
    /// answered for their id. Only filled when
    /// [`Capabilities::mentions`](crate::Capabilities) is set.
    #[serde(default)]
    pub mentions: Vec<crate::Mention>,
    /// The message is somebody's, passed on: it goes out with WhatsApp's
    /// "Forwarded" mark. Only set when
    /// [`Capabilities::forwards`](crate::Capabilities) is.
    #[serde(default)]
    pub forwarded: bool,
}

/// The provider's answer to a successful [`Provider::send`](crate::Provider::send).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendReceipt {
    /// The provider's id for the message. From here on the message is known
    /// by this id; later events refer to it.
    pub message_id: MessageId,
    /// Where the message stands: [`DeliveryStatus::Accepted`] if the
    /// provider only queued it, [`DeliveryStatus::Sent`] if WhatsApp
    /// already has it.
    pub status: DeliveryStatus,
    /// The provider's timestamp for the message, if it assigned one.
    pub timestamp: Option<Timestamp>,
}

/// One page of a listing, plus where the next one starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page<T> {
    /// The items of this page, in the listing's order.
    pub items: Vec<T>,
    /// Pass to the same call to get the next page. `None` on the last page.
    pub next_cursor: Option<Cursor>,
}

impl<T> Page<T> {
    /// A final page.
    pub fn last(items: Vec<T>) -> Self {
        Self {
            items,
            next_cursor: None,
        }
    }
}

/// A downloaded media payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaData {
    /// The raw bytes.
    pub bytes: Vec<u8>,
    /// MIME type reported by the source, when it reported one.
    pub mime_type: Option<String>,
}

/// A profile or group picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Avatar {
    /// The provider's id for this picture. It changes when the picture
    /// does, so the client can tell without downloading it again.
    pub id: String,
    /// The image, as the provider serves it (a small preview is enough:
    /// the client shows it at avatar size).
    pub bytes: Vec<u8>,
    /// MIME type, when the source reported one.
    pub mime_type: Option<String>,
}

/// The answer to [`Provider::fetch_avatar`](crate::Provider::fetch_avatar).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AvatarAnswer {
    /// The picture is the one the client already has (same id): nothing
    /// was downloaded.
    Unchanged,
    /// There is no picture the account can see: none set, hidden by
    /// privacy settings, or the provider cannot read one for this subject.
    None,
    /// A picture the client does not have yet.
    New(Avatar),
}

/// What the client is prepared to receive from
/// [`Provider::fetch_media`](crate::Provider::fetch_media).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaLimit {
    /// The largest payload to accept, in bytes. A provider should refuse
    /// from the response headers when it can, and stop reading at this
    /// size when it cannot.
    pub max_bytes: u64,
    /// Accept only images (by the source's content type).
    pub images_only: bool,
}

/// What a contact is doing in a chat right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresenceState {
    /// Typing a message.
    Typing,
    /// Recording a voice note.
    Recording,
    /// Online, not composing.
    Online,
    /// Stopped composing, or went offline.
    Idle,
}
