//! The [`Provider`] trait.

use crate::capabilities::Capabilities;
use crate::error::ProviderError;
use crate::error::ProviderResult;
use crate::event::EventStream;
use crate::ids::ContactId;
use crate::ids::{AccountId, ChatId, Cursor, MediaRef, MessageId};
use crate::model::{
    Account, AccountChange, AvatarAnswer, Chat, ChatChange, Contact, LinkPlace, LinkStatus,
    MediaData, MediaLimit, MediaUpload, Message, NewAccount, NumberCheck, OutgoingMessage, Page,
    SendReceipt, UploadProgress,
};
use crate::social::{
    BusinessProfile, Group, GroupChange, JoinRequest, NewGroup, OwnProfile, ParticipantChange,
    ParticipantOutcome, ProfileChange,
};
use crate::stories::{NewStory, Story, StoryPrivacy, StoryViewer};
use async_trait::async_trait;
use std::time::Duration;

/// How long a provider must remember a [`ClientMessageId`](crate::ClientMessageId).
///
/// [`Provider::send`] has to be idempotent for at least this long after the
/// first attempt. The client stops retrying a message well before the window
/// closes (and marks it failed), so a retry can never land after the provider
/// has forgotten the id.
pub const IDEMPOTENCY_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// How a provider's live updates reach the client right now. Said for
/// the diagnostics, never for the main window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveUpdates {
    /// Pushed over a stream the provider holds open.
    Stream,
    /// The stream dropped and is being opened again; events keep coming
    /// meanwhile where the provider can.
    Reconnecting,
    /// Asked for at an interval, and why not pushed.
    Polling(PollingReason),
}

/// Why updates are polled and not pushed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollingReason {
    /// Polling is what was asked for.
    Chosen,
    /// The backend has no stream yet.
    Unavailable,
    /// The backend allows no more open streams.
    ConnectionLimit,
    /// The backend's stream refused this key.
    Refused,
    /// The stream keeps failing.
    Failing,
}

/// A WhatsApp backend.
///
/// Implement this trait to plug a service into the client: a hosted REST
/// API, a self-hosted gateway, a library that speaks the protocol directly.
/// The client's sync engine is the only caller. It copies what the provider
/// returns into a local database and the UI reads from there, so a provider
/// never has to be fast, cache anything, or keep state between calls beyond
/// its own connection.
///
/// # Contract
///
/// * **Stateless from the caller's side.** Any method may be called at any
///   time, concurrently, in any order, and again after it failed.
/// * **Bounded.** Every call must finish in bounded time. Put a timeout on
///   each network request and return [`ProviderError::Transient`] when it
///   fires; the engine also wraps calls in its own deadline and treats an
///   overrun the same way.
/// * **Honest errors.** Use [`ProviderError::Transient`] /
///   [`ProviderError::RateLimited`] only for failures a retry can fix. The
///   engine retries those silently and reports the rest to the user.
/// * **Idempotent sends.** See [`send`](Self::send).
///
/// The trait is object-safe; the client holds providers as
/// `Arc<dyn Provider>`.
///
/// [`ProviderError::Transient`]: crate::ProviderError::Transient
/// [`ProviderError::RateLimited`]: crate::ProviderError::RateLimited
#[async_trait]
pub trait Provider: Send + Sync + 'static {
    /// A short, stable, lowercase name for this kind of provider, such as
    /// `"wuapi"`. Stored next to each account so the client knows which
    /// provider owns it. Must not change between releases.
    fn id(&self) -> &'static str;

    /// What this provider supports. Must be cheap and must not touch the
    /// network; the answer may be cached for the lifetime of the provider.
    fn capabilities(&self) -> Capabilities;

    /// Whether a part the capabilities promise is known not to work right
    /// now for `account`: the backend does not have its routes yet, or it
    /// is not turned on for this number. The default is `false`.
    ///
    /// A provider learns it from its backend's answers and forgets it
    /// after a while, so that the part is tried again. Calls made
    /// meanwhile answer [`ProviderError::Unsupported`], which the client
    /// takes as "not available yet" and never shows as a failure. Cheap
    /// and without I/O: the client asks whenever it draws.
    fn unavailable(&self, account: &AccountId, feature: crate::Feature) -> bool {
        let _ = (account, feature);
        false
    }

    /// How live updates reach the client right now, for a provider that has
    /// more than one way. Cheap and without I/O: the diagnostics ask when
    /// they open. `None`, the default, is a provider with no such choice.
    fn live_updates(&self) -> Option<LiveUpdates> {
        None
    }

    /// The user asked for `feature` again for `account` (they opened the
    /// place that offers it, or pressed "Check again"): whatever was
    /// remembered about it being unavailable is forgotten, so the next
    /// call asks the backend instead of answering from memory. Cheap and
    /// without I/O. The default does nothing, which is right for a
    /// provider that remembers nothing.
    fn recheck(&self, account: &AccountId, feature: crate::Feature) {
        let _ = (account, feature);
    }

    /// What follows the `@` where a text sent through this provider
    /// mentions `contact`. On WhatsApp it is the digits of the id, which
    /// is what the default answers; a backend whose ids are not numbers
    /// says what it writes instead. Must not touch the network.
    fn mention_handle(&self, contact: &ContactId) -> String {
        crate::mention_handle(contact)
    }

    /// Every account the current credentials can use.
    ///
    /// Called at startup and whenever the client wants to refresh
    /// connection states.
    async fn list_accounts(&self) -> ProviderResult<Vec<Account>>;

    /// One page of an account's chats, most recently active first.
    ///
    /// Pass `None` for the first page and the previous page's
    /// [`Page::next_cursor`] for the following ones.
    ///
    /// Providers without a native chat listing should reconstruct the best
    /// list they can (and leave [`Capabilities::chat_list`] `false`) rather
    /// than return an error: a partial chat list is far more useful than
    /// none.
    async fn list_chats(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Chat>>;

    /// One page of a chat's history, **newest first**.
    ///
    /// Pass `None` for the most recent page and the previous page's
    /// [`Page::next_cursor`] to walk back in time. `limit` is the page size
    /// the caller would like; a provider may return fewer or cap it.
    ///
    /// Reactions are returned as messages with
    /// [`MessageContent::Reaction`](crate::MessageContent::Reaction).
    async fn fetch_messages(
        &self,
        account: &AccountId,
        chat: &ChatId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> ProviderResult<Page<Message>>;

    /// Sends a message.
    ///
    /// # Idempotency
    ///
    /// `message.client_id` is an idempotency key. The client stores every
    /// outgoing message in a persistent outbox and calls `send` again with
    /// the identical value after a timeout, a dropped connection, or a
    /// restart, because it cannot know whether the first attempt went
    /// through. **Calling `send` any number of times with the same
    /// `client_id` must put at most one message on WhatsApp**, and each
    /// successful call must return the same [`SendReceipt::message_id`].
    ///
    /// The guarantee must hold for at least [`IDEMPOTENCY_WINDOW`] after
    /// the first attempt.
    ///
    /// How to get there depends on the backend: forward the id as an
    /// idempotency key if the API has one, derive the WhatsApp message id
    /// from it deterministically, or keep a table of ids already sent. A
    /// provider that cannot guarantee it must not be used for sending.
    ///
    /// # Result
    ///
    /// Return as soon as the backend has durably accepted the message;
    /// later progress arrives as [`ProviderEvent::MessageStatusChanged`].
    /// The message itself, when it shows up in events or history, must
    /// carry the `client_id` in [`Message::client_id`].
    ///
    /// [`ProviderEvent::MessageStatusChanged`]: crate::ProviderEvent::MessageStatusChanged
    async fn send(&self, message: OutgoingMessage) -> ProviderResult<SendReceipt>;

    /// Marks a chat as read up to and including `up_to` (the whole chat when
    /// `None`), clearing its unread count. Sends read receipts to the other
    /// side when [`Capabilities::read_receipts`] is set.
    ///
    /// Must be safe to repeat.
    async fn mark_read(
        &self,
        account: &AccountId,
        chat: &ChatId,
        up_to: Option<&MessageId>,
    ) -> ProviderResult<()>;

    /// Clears a chat's unread count, on the account's own devices too,
    /// without telling the other side: no read receipts. For users who
    /// turned receipts off.
    ///
    /// Only called when [`Capabilities::quiet_read`] is set; the default
    /// answers [`ProviderError::Unsupported`]. Must be safe to repeat.
    async fn mark_read_quietly(&self, account: &AccountId, chat: &ChatId) -> ProviderResult<()> {
        let _ = (account, chat);
        Err(ProviderError::Unsupported("marking a chat read quietly"))
    }

    /// Pins, mutes, archives or marks a chat as unread.
    ///
    /// Only called when [`Capabilities::chat_state`] is set; the default
    /// answers [`ProviderError::Unsupported`]. Every [`ChatChange`] sets a
    /// value, so the call must be safe to repeat: the client shows the new
    /// state at once and repeats the call until it gets through.
    async fn update_chat(
        &self,
        account: &AccountId,
        chat: &ChatId,
        change: ChatChange,
    ) -> ProviderResult<()> {
        let _ = (account, chat, change);
        Err(ProviderError::Unsupported("changing a chat's state"))
    }

    /// The chat with a phone number, for starting a conversation the
    /// account has not had yet. `phone` is what the user typed; the
    /// provider normalises it and rejects what cannot be a number.
    ///
    /// Only called when [`Capabilities::start_chat`] is set; the default
    /// answers [`ProviderError::Unsupported`]. It need not check that the
    /// number is on WhatsApp: a message to one that is not fails when it
    /// is sent. Must be safe to repeat.
    async fn start_chat(&self, account: &AccountId, phone: &str) -> ProviderResult<Chat> {
        let _ = (account, phone);
        Err(ProviderError::Unsupported("starting a chat with a number"))
    }

    /// Downloads a media payload previously announced in a
    /// [`Media::source`](crate::Media::source) or an avatar field.
    ///
    /// Only called when [`Capabilities::media_download`] is set.
    async fn download_media(
        &self,
        account: &AccountId,
        media: &MediaRef,
    ) -> ProviderResult<MediaData>;

    /// [`fetch_media`](Self::fetch_media), saying how many bytes have
    /// arrived so far, as often as the provider can. The client shows a
    /// download's progress from it. The default says nothing until the
    /// end.
    async fn fetch_media_reporting(
        &self,
        account: &AccountId,
        media: &MediaRef,
        limit: MediaLimit,
        progress: UploadProgress,
    ) -> ProviderResult<MediaData> {
        let data = self.fetch_media(account, media, limit).await?;
        progress(data.bytes.len() as u64);
        Ok(data)
    }

    /// Takes a file the user wants to send, and answers the reference a
    /// message can then carry
    /// ([`OutgoingContent::Media`](crate::OutgoingContent)).
    ///
    /// Only called when [`Capabilities::media_upload`] is set; the default
    /// answers [`ProviderError::Unsupported`]. The contract mirrors
    /// [`send`](Self::send): it must be safe to repeat with the same
    /// [`MediaUpload::key`] (the client repeats it after every failure
    /// that may pass, with backoff, and never tells the user), and only a
    /// failure that will not pass is [`ProviderError::Rejected`]: too
    /// large (`too_large`), a type the backend does not take
    /// (`unsupported_type`). `progress` is told how many bytes have gone
    /// out, as often as the provider can say.
    ///
    /// The reference may stop working after a while. A `send` that finds
    /// it gone answers `Rejected` with code `upload_expired`, and the
    /// client uploads again: it still has the file.
    async fn upload_media(
        &self,
        account: &AccountId,
        upload: MediaUpload,
        progress: UploadProgress,
    ) -> ProviderResult<MediaRef> {
        let _ = (account, upload, progress);
        Err(ProviderError::Unsupported("sending files"))
    }

    /// The largest file [`upload_media`](Self::upload_media) takes, in
    /// bytes, when there is a limit. The client refuses a larger file
    /// before anything is uploaded.
    fn media_upload_limit(&self) -> Option<u64> {
        None
    }

    /// Whether files can be sent right now. A provider whose backend may
    /// not have the upload routes yet finds out here, cheaply, so that
    /// the client says "not available yet" before the user picks a file
    /// instead of failing afterwards. The default is
    /// [`Capabilities::media_upload`].
    async fn media_upload_ready(&self) -> bool {
        self.capabilities().media_upload
    }

    /// Downloads a media payload within a limit.
    ///
    /// What the client calls for message media: it never wants more than
    /// `limit.max_bytes`, and for payloads it loads without being asked it
    /// wants images only. Too large or of the wrong type is
    /// [`ProviderError::Rejected`] (codes `too_large`, `not_an_image`),
    /// which the client shows as "not available" rather than retrying.
    ///
    /// The default downloads with [`download_media`](Self::download_media)
    /// and checks afterwards. A provider that fetches over HTTP should
    /// override it to refuse from the headers and to stop reading at the
    /// limit, so that an oversized or mislabelled file is never pulled in
    /// whole.
    async fn fetch_media(
        &self,
        account: &AccountId,
        media: &MediaRef,
        limit: MediaLimit,
    ) -> ProviderResult<MediaData> {
        let data = self.download_media(account, media).await?;
        if data.bytes.len() as u64 > limit.max_bytes {
            return Err(ProviderError::Rejected {
                code: "too_large".into(),
                message: "The file is larger than the download limit.".into(),
            });
        }
        let is_image = data
            .mime_type
            .as_deref()
            .is_some_and(|mime| mime.starts_with("image/"));
        if limit.images_only && !is_image {
            return Err(ProviderError::Rejected {
                code: "not_an_image".into(),
                message: "The file is not an image.".into(),
            });
        }
        Ok(data)
    }

    /// The picture of a chat: a contact's profile picture or a group's.
    ///
    /// `subject` is the chat's id (for a direct chat, the contact).
    /// `known` is the id of the picture the client already has, if any: a
    /// provider that can tell the picture has not changed answers
    /// [`AvatarAnswer::Unchanged`] without downloading it.
    /// [`AvatarAnswer::None`] covers every "there is nothing to show" (no
    /// picture, hidden by privacy, not readable for this kind of chat);
    /// keep errors for failures worth knowing about.
    ///
    /// Only called when [`Capabilities::avatars`] is set; the default
    /// answers [`ProviderError::Unsupported`]. Called lazily, for chats on
    /// screen, a few at a time; never on the way of a send.
    async fn fetch_avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
        known: Option<&str>,
    ) -> ProviderResult<AvatarAnswer> {
        let _ = (account, subject, known);
        Err(ProviderError::Unsupported("profile pictures"))
    }

    /// Casts the account's vote in a poll: `choices` are the names of the
    /// options it now stands for, all of them (an empty list takes the
    /// vote back). The answer is the poll with its new tally when the
    /// backend returns it, `None` when the tally arrives as an event.
    ///
    /// Only called when [`Capabilities::poll_votes`] is set; the default
    /// answers [`ProviderError::Unsupported`]. A vote sets a value, so the
    /// call must be safe to repeat: the client shows the vote at once and
    /// repeats the call until it gets through, and takes the vote back off
    /// the screen only when the provider refuses it.
    async fn vote_poll(
        &self,
        account: &AccountId,
        chat: &ChatId,
        poll: &MessageId,
        choices: &[String],
    ) -> ProviderResult<Option<Message>> {
        let _ = (account, chat, poll, choices);
        Err(ProviderError::Unsupported("voting in a poll"))
    }

    /// Replaces the text of a message the account sent.
    ///
    /// Only called when [`Capabilities::edits`] is set, for the account's
    /// own text messages; the default answers
    /// [`ProviderError::Unsupported`]. WhatsApp allows it for a while
    /// after sending only: answer its refusal as
    /// [`ProviderError::Rejected`], which the client shows as it is. An
    /// edit sets a value, so the call must be safe to repeat: the client
    /// shows the new text at once, repeats the call until it gets through
    /// and puts the old text back only when the provider refuses.
    async fn edit_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        text: &str,
    ) -> ProviderResult<()> {
        let _ = (account, chat, message, text);
        Err(ProviderError::Unsupported("editing a message"))
    }

    /// Deletes a message: for everyone (the account's own messages, when
    /// [`Capabilities::deletes`] is set) or for the account alone
    /// ([`Capabilities::delete_for_me`] for its own messages,
    /// [`Capabilities::delete_received`] for other people's).
    ///
    /// The default answers [`ProviderError::Unsupported`]. Safe to
    /// repeat: deleting what is already deleted is a success.
    async fn delete_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        for_everyone: bool,
    ) -> ProviderResult<()> {
        let _ = (account, chat, message, for_everyone);
        Err(ProviderError::Unsupported("deleting a message"))
    }

    /// Stars or unstars a message.
    ///
    /// Only called when [`Capabilities::stars`] is set; the default
    /// answers [`ProviderError::Unsupported`]. It sets a value, so it is
    /// safe to repeat.
    async fn star_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        starred: bool,
    ) -> ProviderResult<()> {
        let _ = (account, chat, message, starred);
        Err(ProviderError::Unsupported("starring a message"))
    }

    // ----- forwarding ----------------------------------------------------

    /// Passes messages on to chats the way WhatsApp does: the original is
    /// forwarded (the provider reuses what it holds, a media reference
    /// included, so no file passes through this client) and each copy is
    /// marked as forwarded. One answer per item, in the order given: the
    /// copy that was accepted, or why this one cannot be (a file
    /// WhatsApp no longer has, a message that is view-once, a kind that
    /// cannot be forwarded: `Rejected`); an item that failed does not
    /// stop the others. A failure of the whole call is the `Err`.
    ///
    /// The message named may be a story that is up ([`Story::id`](crate::Story)
    /// is a message id): that is how a picture or a video of Status is
    /// passed on to a chat.
    ///
    /// Only called when [`Capabilities::forward_any`] is set; the default
    /// answers [`ProviderError::Unsupported`]. The contract mirrors
    /// [`send`](Self::send): every [`ForwardItem::client_id`] is an
    /// idempotency key, so the client repeats an item after a failure
    /// that may pass and never produces a second copy.
    async fn forward_messages(
        &self,
        account: &AccountId,
        items: &[crate::ForwardItem],
    ) -> ProviderResult<Vec<ProviderResult<SendReceipt>>> {
        let _ = (account, items);
        Err(ProviderError::Unsupported("forwarding messages"))
    }

    // ----- stickers -----------------------------------------------------

    /// The stickers starred on the account, all of them.
    ///
    /// Only called when [`Capabilities::sticker_favorites`] is set; the
    /// default answers [`ProviderError::Unsupported`]. The client copies
    /// them into its own library in the background and reflects changes
    /// both ways; each file is fetched with
    /// [`fetch_media`](Self::fetch_media) on its [`source`]
    /// (crate::FavoriteSticker::source).
    async fn list_favorite_stickers(
        &self,
        account: &AccountId,
    ) -> ProviderResult<Vec<crate::FavoriteSticker>> {
        let _ = account;
        Err(ProviderError::Unsupported("favorite stickers"))
    }

    /// Stars a sticker on the account, and answers the id it is listed
    /// under from now on: the [`FavoriteSticker::id`](crate::FavoriteSticker::id)
    /// that [`list_favorite_stickers`](Self::list_favorite_stickers) gives
    /// it and a removal names. A provider whose backend has no id of its
    /// own answers [`StickerFile::id`](crate::StickerFile::id).
    ///
    /// Only called when [`Capabilities::sticker_favorites`] is set. It
    /// sets a value keyed by [`StickerFile::id`](crate::StickerFile::id),
    /// so repeating it is a success and answers the same id. A sticker
    /// that was seen in a message names it
    /// ([`StickerFile::message`](crate::StickerFile::message)): a
    /// provider that can star by naming the message does, and no file
    /// travels.
    async fn add_favorite_sticker(
        &self,
        account: &AccountId,
        sticker: crate::StickerFile,
    ) -> ProviderResult<String> {
        let _ = (account, sticker);
        Err(ProviderError::Unsupported("favorite stickers"))
    }

    /// Takes a star off a sticker: `id` is the
    /// [`FavoriteSticker::id`](crate::FavoriteSticker::id) it was listed
    /// under. Removing what is not there is a success.
    async fn remove_favorite_sticker(&self, account: &AccountId, id: &str) -> ProviderResult<()> {
        let _ = (account, id);
        Err(ProviderError::Unsupported("favorite stickers"))
    }

    // ----- contacts -----------------------------------------------------

    /// One page of the account's address book, in a stable order.
    ///
    /// Only called when [`Capabilities::contacts`] is set; the default
    /// answers [`ProviderError::Unsupported`]. The client copies the whole
    /// book into its store in the background, a page at a time, and reads
    /// it from there: this is never on the way of a click. It should not
    /// need the number to be connected.
    async fn list_contacts(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Contact>> {
        let _ = (account, cursor);
        Err(ProviderError::Unsupported("listing contacts"))
    }

    /// Says which of `phones` (as typed) have WhatsApp. One answer per
    /// number that could be read as one; a number that cannot is
    /// [`ProviderError::Rejected`] with code `invalid_phone`.
    ///
    /// Only called when [`Capabilities::number_check`] is set. Read-only
    /// and safe to repeat.
    async fn check_numbers(
        &self,
        account: &AccountId,
        phones: &[String],
    ) -> ProviderResult<Vec<NumberCheck>> {
        let _ = (account, phones);
        Err(ProviderError::Unsupported("checking numbers"))
    }

    // ----- profiles -----------------------------------------------------

    /// Asks WhatsApp about one contact: the "About" text, the username,
    /// the picture id and a business's verified name. Fields the answer
    /// does not carry are `None`; the client keeps what it has for those.
    ///
    /// Only called when [`Capabilities::contact_lookup`] is set. Read-only
    /// and safe to repeat; asked when a profile is opened, never in bulk.
    async fn lookup_contact(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Contact> {
        let _ = (account, contact);
        Err(ProviderError::Unsupported("looking a contact up"))
    }

    /// What a business says about itself. `None` when the contact is not
    /// a business or has no profile.
    ///
    /// Only called when [`Capabilities::business_profiles`] is set.
    async fn business_profile(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Option<BusinessProfile>> {
        let _ = (account, contact);
        Err(ProviderError::Unsupported("business profiles"))
    }

    /// The contacts the account has blocked, under every id the provider
    /// knows each one by (a number, a hidden-number id).
    ///
    /// Only called when [`Capabilities::blocking`] is set.
    async fn list_blocked(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        let _ = account;
        Err(ProviderError::Unsupported("the blocklist"))
    }

    /// Blocks or unblocks a contact. Sets a value: safe to repeat.
    /// `request_id` is the same on every retry of one user action.
    async fn set_blocked(
        &self,
        account: &AccountId,
        contact: &ContactId,
        blocked: bool,
        request_id: &str,
    ) -> ProviderResult<()> {
        let _ = (account, contact, blocked, request_id);
        Err(ProviderError::Unsupported("blocking a contact"))
    }

    /// The account's own profile, as far as it can be read. A provider
    /// that can read nothing answers a profile of `None`s.
    ///
    /// Only called when [`Capabilities::profile_edit`] is set.
    async fn own_profile(&self, account: &AccountId) -> ProviderResult<OwnProfile> {
        let _ = account;
        Err(ProviderError::Unsupported("reading the own profile"))
    }

    /// Changes the account's own name or "About" text. Sets a value: safe
    /// to repeat.
    async fn update_profile(
        &self,
        account: &AccountId,
        change: &ProfileChange,
    ) -> ProviderResult<()> {
        let _ = (account, change);
        Err(ProviderError::Unsupported("changing the own profile"))
    }

    /// Sets the account's own picture (`jpeg`: a square JPEG the client
    /// prepared) or removes it (`None`). Answers the new picture's id when
    /// the backend gives one. Safe to repeat.
    async fn set_profile_picture(
        &self,
        account: &AccountId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        let _ = (account, jpeg);
        Err(ProviderError::Unsupported("changing the own picture"))
    }

    // ----- groups -------------------------------------------------------

    /// Every group the account is in, with its participants.
    ///
    /// Only called when [`Capabilities::group_info`] is set; in the
    /// background, to know which groups a contact shares with the account.
    /// A provider that cannot list them all at a reasonable cost keeps the
    /// default, which answers [`ProviderError::Unsupported`].
    async fn list_groups(&self, account: &AccountId) -> ProviderResult<Vec<Group>> {
        let _ = account;
        Err(ProviderError::Unsupported("listing groups"))
    }

    /// A group's details and participants.
    ///
    /// Only called when [`Capabilities::group_info`] is set.
    async fn fetch_group(&self, account: &AccountId, group: &ChatId) -> ProviderResult<Group> {
        let _ = (account, group);
        Err(ProviderError::Unsupported("group details"))
    }

    /// Creates a group. Must be safe to repeat with the same
    /// [`NewGroup::request_id`]. Participants who could not be added (their
    /// privacy settings) are simply missing from the answer.
    ///
    /// Only called when [`Capabilities::group_create`] is set.
    async fn create_group(&self, account: &AccountId, new: &NewGroup) -> ProviderResult<Group> {
        let _ = (account, new);
        Err(ProviderError::Unsupported("creating a group"))
    }

    /// Changes a group's subject, description or one of its settings.
    /// Admins only: anyone else gets [`ProviderError::Rejected`] with
    /// [`refusal::NOT_ADMIN`](crate::refusal::NOT_ADMIN). Sets a value:
    /// safe to repeat.
    ///
    /// Only called when [`Capabilities::group_manage`] is set, as are the
    /// two methods after it.
    async fn update_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: &GroupChange,
    ) -> ProviderResult<()> {
        let _ = (account, group, change);
        Err(ProviderError::Unsupported("changing a group"))
    }

    /// Adds, removes, promotes or demotes participants. The answer has one
    /// outcome per contact; a refusal of the whole request (not an admin)
    /// is an error. `request_id` is the same on every retry of one user
    /// action, so a repeat after a lost answer does not act twice.
    async fn change_participants(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: ParticipantChange,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        let _ = (account, group, change, contacts, request_id);
        Err(ProviderError::Unsupported(
            "changing a group's participants",
        ))
    }

    /// Sets a group's picture (a square JPEG the client prepared) or
    /// removes it. Answers the new picture's id when the backend gives
    /// one. Safe to repeat.
    async fn set_group_picture(
        &self,
        account: &AccountId,
        group: &ChatId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        let _ = (account, group, jpeg);
        Err(ProviderError::Unsupported("changing a group's picture"))
    }

    /// A group's invite link. With `reset`, the current link stops working
    /// and a new one is answered (`request_id` as in
    /// [`change_participants`](Self::change_participants)).
    ///
    /// Only called when [`Capabilities::group_invites`] is set.
    async fn group_invite_link(
        &self,
        account: &AccountId,
        group: &ChatId,
        reset: bool,
        request_id: &str,
    ) -> ProviderResult<String> {
        let _ = (account, group, reset, request_id);
        Err(ProviderError::Unsupported("group invite links"))
    }

    /// Who is waiting to be let into a group.
    ///
    /// Only called when [`Capabilities::group_join_requests`] is set.
    async fn join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> ProviderResult<Vec<JoinRequest>> {
        let _ = (account, group);
        Err(ProviderError::Unsupported("requests to join a group"))
    }

    /// Approves or rejects requests to join. One outcome per contact.
    async fn answer_join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
        approve: bool,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        let _ = (account, group, approve, contacts, request_id);
        Err(ProviderError::Unsupported("answering requests to join"))
    }

    /// The account leaves a group. Leaving one it is no longer in is a
    /// success.
    ///
    /// Only called when [`Capabilities::group_leave`] is set.
    async fn leave_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        request_id: &str,
    ) -> ProviderResult<()> {
        let _ = (account, group, request_id);
        Err(ProviderError::Unsupported("leaving a group"))
    }

    /// Links an existing group to a community. Admins of both only:
    /// anyone else gets [`ProviderError::Rejected`] with
    /// [`refusal::NOT_ADMIN`](crate::refusal::NOT_ADMIN). Linking a group
    /// that is already linked there is a success. `request_id` is the same
    /// on every retry of one user action.
    ///
    /// Only called when [`Capabilities::community_manage`] is set.
    async fn link_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
        request_id: &str,
    ) -> ProviderResult<()> {
        let _ = (account, community, group, request_id);
        Err(ProviderError::Unsupported("linking a group to a community"))
    }

    /// Takes a group out of a community. Admins only. Unlinking a group
    /// that is not linked is a success. Safe to repeat.
    ///
    /// Only called when [`Capabilities::community_manage`] is set.
    async fn unlink_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
    ) -> ProviderResult<()> {
        let _ = (account, community, group);
        Err(ProviderError::Unsupported(
            "taking a group out of a community",
        ))
    }

    /// The people in all of a community's groups, as contact ids.
    ///
    /// Only called when [`Capabilities::community_members`] is set.
    async fn community_participants(
        &self,
        account: &AccountId,
        community: &ChatId,
    ) -> ProviderResult<Vec<ContactId>> {
        let _ = (account, community);
        Err(ProviderError::Unsupported("a community's participants"))
    }

    // ----- stories -------------------------------------------------------

    /// The stories the account can see and its own, the ones that have not
    /// expired. Called when the Status view is looked at and after a
    /// reconnect; what changes in between arrives as
    /// [`ProviderEvent::StoryUpserted`](crate::ProviderEvent::StoryUpserted)
    /// and friends. Only called when [`Capabilities::story_list`] is set.
    async fn list_stories(&self, account: &AccountId) -> ProviderResult<Vec<Story>> {
        let _ = account;
        Err(ProviderError::Unsupported("listing stories"))
    }

    /// Posts a story. `story.client_id` is an idempotency key: any number
    /// of calls with the same one put at most one story on WhatsApp and
    /// answer the same story. A picture or a video was uploaded first with
    /// [`upload_media`](Self::upload_media). Only called when
    /// [`Capabilities::story_post`] is set.
    async fn post_story(&self, story: NewStory) -> ProviderResult<Story> {
        let _ = story;
        Err(ProviderError::Unsupported("posting a story"))
    }

    /// Takes one of the account's own stories down, for everyone. Safe to
    /// repeat: a story that is gone is not an error. Only called when
    /// [`Capabilities::story_delete`] is set.
    async fn delete_story(&self, account: &AccountId, story: &MessageId) -> ProviderResult<()> {
        let _ = (account, story);
        Err(ProviderError::Unsupported("deleting a story"))
    }

    /// Tells `author` that the account saw their story (the view receipt).
    ///
    /// The client calls it only for a story that was actually shown, once,
    /// and not at all while the user keeps read receipts off. Must be safe
    /// to repeat. Only called when [`Capabilities::story_view`] is set.
    async fn view_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        author: &ContactId,
    ) -> ProviderResult<()> {
        let _ = (account, story, author);
        Err(ProviderError::Unsupported("marking a story as seen"))
    }

    /// Who saw one of the account's own stories, newest first. Only called
    /// when [`Capabilities::story_viewers`] is set.
    async fn story_viewers(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> ProviderResult<Vec<StoryViewer>> {
        let _ = (account, story);
        Err(ProviderError::Unsupported("who saw a story"))
    }

    /// Sends a reply to a story: `message` is an ordinary text for the
    /// author's chat whose `reply_to` is the story's id, and it must reach
    /// them as a reply to that story. `message.client_id` is an
    /// idempotency key, as for [`send`](Self::send). Only called when
    /// [`Capabilities::story_reply`] is set.
    async fn reply_to_story(
        &self,
        story: &Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        let _ = (story, message);
        Err(ProviderError::Unsupported("replying to a story"))
    }

    /// Reacts to a story: `message` carries an
    /// [`OutgoingContent::Reaction`](crate::OutgoingContent::Reaction)
    /// whose target is the story's id (an empty emoji takes the reaction
    /// back). Idempotent on `message.client_id`. Only called when
    /// [`Capabilities::story_react`] is set.
    async fn react_to_story(
        &self,
        story: &Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        let _ = (story, message);
        Err(ProviderError::Unsupported("reacting to a story"))
    }

    /// The authors whose stories the account muted. Only called when
    /// [`Capabilities::story_mute`] is set.
    async fn muted_story_authors(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        let _ = account;
        Err(ProviderError::Unsupported("muted stories"))
    }

    /// Mutes or unmutes an author's stories. Sets a value, so it is safe
    /// to repeat. Only called when [`Capabilities::story_mute`] is set.
    async fn set_story_muted(
        &self,
        account: &AccountId,
        author: &ContactId,
        muted: bool,
    ) -> ProviderResult<()> {
        let _ = (account, author, muted);
        Err(ProviderError::Unsupported("muting stories"))
    }

    /// Who sees the account's stories. Only called when
    /// [`Capabilities::story_privacy`] is set.
    async fn story_privacy(&self, account: &AccountId) -> ProviderResult<StoryPrivacy> {
        let _ = account;
        Err(ProviderError::Unsupported("story privacy"))
    }

    /// Changes who sees the account's stories, lists included. Sets a
    /// value, so it is safe to repeat. Only called when
    /// [`Capabilities::story_privacy_edit`] is set.
    async fn set_story_privacy(
        &self,
        account: &AccountId,
        privacy: &StoryPrivacy,
    ) -> ProviderResult<()> {
        let _ = (account, privacy);
        Err(ProviderError::Unsupported("changing who sees stories"))
    }

    // ----- numbers ------------------------------------------------------

    /// The places a new number can connect from, when the provider lets
    /// the user choose. Empty (the default) when there is nothing to
    /// choose.
    async fn link_places(&self) -> ProviderResult<Vec<LinkPlace>> {
        Ok(Vec::new())
    }

    /// Starts linking a new number. Answers at once, usually at
    /// [`LinkStep::Starting`]; the client then asks
    /// [`link_status`](Self::link_status) until the number is linked.
    ///
    /// Only called when [`Capabilities::link_accounts`] is set. Must be
    /// safe to repeat with the same [`NewAccount::request_id`]: a request
    /// that is sent again after a dropped connection creates one number.
    /// A refusal the user can act on (a plan limit, no subscription) is
    /// [`ProviderError::Rejected`] with a message that says so.
    async fn create_account(&self, new: &NewAccount) -> ProviderResult<LinkStatus> {
        let _ = new;
        Err(ProviderError::Unsupported("linking a number"))
    }

    /// Where linking `account` stands: the QR code or the code to show
    /// now, or that it is done. Asked every few seconds while the linking
    /// screen is open; safe to repeat.
    async fn link_status(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        let _ = account;
        Err(ProviderError::Unsupported("linking a number"))
    }

    /// Asks for a code to type on the phone `phone`, for a number that is
    /// not linked. Only called when [`Capabilities::link_by_code`] is set.
    async fn pairing_code(&self, account: &AccountId, phone: &str) -> ProviderResult<LinkStatus> {
        let _ = (account, phone);
        Err(ProviderError::Unsupported("linking with a code"))
    }

    /// Stops linking `account` with a code: the same number, on the session
    /// it has, shows the QR code again. Safe to repeat. Only called when
    /// [`Capabilities::link_back_to_scan`] is set.
    async fn scan_instead(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        let _ = account;
        Err(ProviderError::Unsupported("going back to the QR code"))
    }

    /// Renames a number or changes one of its settings, and answers the
    /// number as it is afterwards. Every change sets a value: safe to
    /// repeat. Only called when [`Capabilities::manage_accounts`] is set
    /// (and, for [`AccountChange::History`],
    /// [`Capabilities::history_import`]).
    async fn update_account(
        &self,
        account: &AccountId,
        change: AccountChange,
    ) -> ProviderResult<Account> {
        let _ = (account, change);
        Err(ProviderError::Unsupported("changing a number"))
    }

    /// Restarts a number's session. One that is no longer linked comes
    /// back with a fresh code to link it again
    /// ([`link_status`](Self::link_status) follows it from there).
    async fn reconnect_account(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        let _ = account;
        Err(ProviderError::Unsupported("reconnecting a number"))
    }

    /// Unlinks the phone from a number, keeping the number and its
    /// messages. The client asks the user first. Safe to repeat.
    async fn unlink_account(&self, account: &AccountId) -> ProviderResult<Account> {
        let _ = account;
        Err(ProviderError::Unsupported("logging a number out"))
    }

    /// Deletes a number. The client only does this by itself for a number
    /// that was never linked (linking was cancelled), and asks first.
    /// Deleting one that is already gone is a success.
    async fn delete_account(&self, account: &AccountId) -> ProviderResult<()> {
        let _ = account;
        Err(ProviderError::Unsupported("deleting a number"))
    }

    /// Opens the stream of live updates for every account.
    ///
    /// The engine calls this once at startup and again, with backoff, each
    /// time the returned stream ends. Providers without a push channel
    /// should poll internally and emit what they find (and leave
    /// [`Capabilities::realtime_push`] `false`).
    async fn subscribe(&self) -> ProviderResult<EventStream>;
}
