//! An in-memory [`Provider`] for development and tests.
//!
//! It starts with a deterministic, realistic-looking world (two accounts, a
//! few dozen chats, groups, long threads, replies, reactions, media
//! placeholders, mixed delivery statuses) and can keep it alive: sent
//! messages advance from sent to delivered to read, contacts type and
//! answer, new messages arrive in other chats.
//!
//! It also doubles as the reference for the trickier parts of the provider
//! contract. In particular [`send`](Provider::send) is idempotent on the
//! client message id, and tests can inject failures to check that a caller
//! copes with them.

#![warn(missing_docs)]

mod forward;
mod seed;
mod showcase;
mod social;
mod stickers;
mod stories;

pub use showcase::{SHOWCASE_ACCOUNT, SHOWCASE_CHAT, SHOWCASE_STICKER};

use async_trait::async_trait;
use client_provider::{
    Account, AccountChange, AccountId, AccountSettings, Avatar, AvatarAnswer, Capabilities, Chat,
    ChatChange, ChatId, ChatKind, ChatUnknown, ClientMessageId, ConnectionState, Contact, Cursor,
    DeliveryStatus, Direction, EventStream, LinkPlace, LinkStatus, LinkStep, MediaData, MediaRef,
    Message, MessageContent, MessageId, NewAccount, NumberCheck, OutgoingContent, OutgoingMessage,
    Page, PresenceState, Provider, ProviderError, ProviderEvent, ProviderResult, ReplyRef,
    SendReceipt, Timestamp,
};
use client_provider::{
    BusinessProfile, ContactId, Group, GroupChange, JoinRequest, NewGroup, OwnProfile,
    ParticipantChange, ParticipantOutcome, ProfileChange,
};
use futures::StreamExt;
use seed::{Rng, World};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::broadcast;

const CHAT_PAGE: usize = 20;
const CONTACT_PAGE: usize = 5;

/// How the mock behaves.
#[derive(Clone, Debug)]
pub struct MockConfig {
    /// Seed of the generated world. The same seed gives the same chats and
    /// messages (timestamps are relative to the moment of creation).
    pub seed: u64,
    /// How long [`Provider::send`] takes to answer. The message is recorded
    /// before the wait, so a caller that times out has still sent it.
    pub send_latency: Duration,
    /// Advance sent messages to delivered and read, and have contacts
    /// answer.
    pub simulate_replies: bool,
    /// Make messages arrive on their own every so often.
    pub ambient_traffic: Option<Duration>,
    /// Scales every simulated delay other than `send_latency`. `1.0` feels
    /// like a real conversation; tests use something small.
    pub time_scale: f32,
    /// A number being linked links by itself after this many status
    /// checks, as if someone had scanned the code. `None`: only when
    /// [`MockProvider::complete_link`] says so.
    pub link_after: Option<u32>,
    /// What the provider says it can do, instead of everything: for
    /// testing how an interface treats a provider that cannot, say, post
    /// stories or reply to them. `None`: all of it.
    pub capabilities: Option<Capabilities>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            seed: 7,
            send_latency: Duration::from_millis(700),
            simulate_replies: true,
            ambient_traffic: Some(Duration::from_secs(9)),
            time_scale: 1.0,
            link_after: Some(5),
            capabilities: None,
        }
    }
}

impl MockConfig {
    /// No latency and nothing happening on its own: every effect is the
    /// direct result of a call. What unit tests want.
    pub fn quiet() -> Self {
        Self {
            seed: 7,
            send_latency: Duration::ZERO,
            simulate_replies: false,
            ambient_traffic: None,
            time_scale: 0.0,
            link_after: None,
            capabilities: None,
        }
    }
}

struct State {
    world: World,
    /// Receipts by client id: the idempotency table of `send`.
    sent: HashMap<ClientMessageId, SendReceipt>,
    /// Failures to return from upcoming `send` calls, in order.
    send_failures: VecDeque<ProviderError>,
    /// How many times `send` was called, successful or not.
    send_calls: usize,
    /// Every message it was asked to send, in order.
    asked: Vec<OutgoingMessage>,
    /// Failures to return from upcoming `update_chat` calls, in order.
    chat_failures: VecDeque<ProviderError>,
    /// Failures to return from upcoming `vote_poll` calls, in order.
    vote_failures: VecDeque<ProviderError>,
    /// Failures to return from upcoming edits, deletions and stars.
    action_failures: VecDeque<ProviderError>,
    /// What was done to messages, in order: "edit m12", "delete-all m12".
    actions: Vec<String>,
    /// The votes that were cast, in order: the poll and the choices.
    votes: Vec<(MessageId, Vec<String>)>,
    /// Failures to return from upcoming `list_accounts` calls, in order.
    account_failures: VecDeque<ProviderError>,
    /// Failures to return from upcoming `fetch_messages` calls, in order.
    history_failures: VecDeque<ProviderError>,
    /// Report every chat's pin, mute and archive as not observed, the
    /// way a backend does that only learns them when they change.
    blind_to_chat_state: bool,
    /// The chat list carries the id of each chat's picture.
    picture_ids: bool,
    /// The chats whose unread count was cleared, in order, and those for
    /// which read receipts went out as well.
    marked_read: Vec<ChatId>,
    receipts_sent: Vec<ChatId>,
    /// Chats (by id) whose unread count the chat list does not know.
    unread_unknown: Vec<ChatId>,
    /// Pictures by (account, chat): id and bytes.
    avatars: HashMap<(AccountId, ChatId), (String, Vec<u8>)>,
    /// Failures to return from upcoming `fetch_avatar` calls, in order.
    avatar_failures: VecDeque<ProviderError>,
    avatar_calls: usize,
    /// Media payloads by reference: bytes and MIME type.
    media: HashMap<String, (Vec<u8>, String)>,
    media_calls: usize,
    /// How many times `list_accounts` and `fetch_messages` were called.
    account_calls: usize,
    history_calls: usize,
    /// Failures to return from upcoming media downloads, in order.
    media_failures: VecDeque<ProviderError>,
    /// How long a media download takes.
    media_latency: Duration,
    /// Uploaded files by the key they were uploaded under: the reference
    /// that was answered.
    uploads: HashMap<String, String>,
    /// The chats muted for a time, in order: the chat and the seconds.
    timed_mutes: Vec<(ChatId, u64)>,
    /// What was uploaded, in order: the bytes and their type.
    uploaded: Vec<(Vec<u8>, String)>,
    /// Failures to return from upcoming `upload_media` calls, in order.
    upload_failures: VecDeque<ProviderError>,
    upload_calls: usize,
    /// How long an upload takes, on top of nothing.
    upload_latency: Duration,
    /// Uploaded references that have "expired": a send with one is
    /// refused once.
    expired_uploads: Vec<String>,
    /// The largest file that can be uploaded.
    upload_limit: u64,
    /// Uploads are not available (the backend does not have them yet).
    uploads_off: bool,
    /// What the capabilities promise and is missing for now: for one
    /// account, or (`None`) for all.
    unavailable: std::collections::HashSet<(Option<AccountId>, client_provider::Feature)>,
    /// The parts whose "missing" is only remembered: asking again
    /// ([`Provider::recheck`]) finds them.
    restored_by_recheck: std::collections::HashSet<client_provider::Feature>,
    /// Address books a test set, by account. Without one, the people of
    /// the account's direct chats are its contacts.
    contacts: HashMap<AccountId, Vec<Contact>>,
    /// Failures to return from upcoming `list_contacts` calls, in order.
    contact_failures: VecDeque<ProviderError>,
    /// How many `list_contacts` calls succeed before those failures.
    contact_failures_after: usize,
    contact_calls: usize,
    /// The cursors `list_contacts` was called with, in order.
    contact_cursors: Vec<Option<String>>,
    /// Numbers (E.164) that are not on WhatsApp.
    without_whatsapp: Vec<String>,
    check_calls: usize,
    /// Numbers being linked.
    links: HashMap<AccountId, Link>,
    /// The number each `create_account` request made, by request id.
    created: HashMap<String, AccountId>,
    /// Failures to return from upcoming `create_account` calls, in order.
    link_failures: VecDeque<ProviderError>,
    /// Failures to return from upcoming `update_account` calls, in order.
    account_update_failures: VecDeque<ProviderError>,
    /// Failures to return from upcoming `link_status` calls, in order.
    link_check_failures: VecDeque<ProviderError>,
    /// Failures to return from upcoming `pairing_code` calls, in order.
    pairing_failures: VecDeque<ProviderError>,
    /// How many times `pairing_code` was called.
    pairing_calls: usize,
    /// How many times `create_account` and `link_status` were called.
    create_calls: usize,
    link_checks: usize,
    /// Numbers that were deleted.
    deleted: Vec<AccountId>,
    /// Profiles and groups.
    social: social::Social,
    /// Favorite stickers.
    stickers: stickers::Stickers,
    forwards: forward::Forwards,
    /// Stories.
    stories: stories::MockStories,
    rng: Rng,
}

/// A number on its way to being linked.
struct Link {
    /// How many times its status was asked.
    checks: u32,
    /// The number to link by code, when not by QR code.
    phone: Option<String>,
    was_linked: bool,
    done: bool,
    stopped: Option<String>,
    /// How many codes were asked for after the first: each is another.
    codes: u32,
    /// The code it has is past its time.
    code_expired: bool,
    /// The QR code it shows is one that cannot be read.
    unreadable: bool,
}

struct Inner {
    config: MockConfig,
    state: Mutex<State>,
    events: broadcast::Sender<ProviderEvent>,
    ambient_started: AtomicBool,
}

/// The in-memory provider. Cheap to clone; clones share the same world.
#[derive(Clone)]
pub struct MockProvider {
    inner: Arc<Inner>,
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new(MockConfig::default())
    }
}

impl MockProvider {
    /// Does something to a message of the world: `change` alters it and
    /// says whether it stays (`false` takes it out of its thread). Safe
    /// to repeat: a message that is no longer there is a success.
    fn act(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        what: &str,
        change: impl FnOnce(&mut Message) -> ProviderResult<bool>,
    ) -> ProviderResult<()> {
        let updated = {
            let mut state = self.state();
            if let Some(error) = state.action_failures.pop_front() {
                return Err(error);
            }
            let thread = state
                .world
                .messages
                .entry((account.clone(), chat.clone()))
                .or_default();
            let Some(at) = thread.iter().position(|found| &found.id == message) else {
                return Ok(());
            };
            let keep = change(&mut thread[at])?;
            let updated = keep.then(|| thread[at].clone());
            if !keep {
                thread.remove(at);
            }
            state.actions.push(format!("{what} {message}"));
            updated
        };
        if let Some(updated) = updated {
            self.emit(ProviderEvent::MessageUpserted(updated));
        }
        Ok(())
    }

    /// Builds a provider with a freshly seeded world.
    pub fn new(config: MockConfig) -> Self {
        let world = seed::build(config.seed, Timestamp::now());
        let story_state = stories::seed(&world, Timestamp::now());
        let (events, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    world,
                    sent: HashMap::new(),
                    send_failures: VecDeque::new(),
                    send_calls: 0,
                    asked: Vec::new(),
                    chat_failures: VecDeque::new(),
                    vote_failures: VecDeque::new(),
                    action_failures: VecDeque::new(),
                    actions: Vec::new(),
                    votes: Vec::new(),
                    account_failures: VecDeque::new(),
                    history_failures: VecDeque::new(),
                    blind_to_chat_state: false,
                    picture_ids: false,
                    marked_read: Vec::new(),
                    receipts_sent: Vec::new(),
                    unread_unknown: Vec::new(),
                    avatars: HashMap::new(),
                    avatar_failures: VecDeque::new(),
                    avatar_calls: 0,
                    media: HashMap::new(),
                    media_calls: 0,
                    account_calls: 0,
                    history_calls: 0,
                    media_failures: VecDeque::new(),
                    media_latency: Duration::ZERO,
                    uploads: HashMap::new(),
                    uploaded: Vec::new(),
                    timed_mutes: Vec::new(),
                    upload_failures: VecDeque::new(),
                    upload_calls: 0,
                    upload_latency: Duration::ZERO,
                    expired_uploads: Vec::new(),
                    upload_limit: 100 * 1024 * 1024,
                    uploads_off: false,
                    unavailable: Default::default(),
                    restored_by_recheck: Default::default(),
                    contacts: HashMap::new(),
                    contact_failures: VecDeque::new(),
                    contact_failures_after: 0,
                    contact_calls: 0,
                    contact_cursors: Vec::new(),
                    without_whatsapp: Vec::new(),
                    check_calls: 0,
                    links: HashMap::new(),
                    created: HashMap::new(),
                    link_failures: VecDeque::new(),
                    link_check_failures: VecDeque::new(),
                    pairing_failures: VecDeque::new(),
                    pairing_calls: 0,
                    account_update_failures: VecDeque::new(),
                    create_calls: 0,
                    link_checks: 0,
                    deleted: Vec::new(),
                    social: social::Social::default(),
                    stickers: stickers::Stickers::default(),
                    forwards: forward::Forwards::default(),
                    stories: story_state,
                    rng: Rng::new(config.seed ^ 0xA5A5),
                }),
                config,
                events,
                ambient_started: AtomicBool::new(false),
            }),
        }
    }

    /// A provider for unit tests: see [`MockConfig::quiet`].
    pub fn quiet() -> Self {
        Self::new(MockConfig::quiet())
    }

    /// Makes the next calls to [`Provider::send`] fail with the given
    /// errors, one per call, before sends work again.
    pub fn fail_next_sends(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().send_failures.extend(errors);
    }

    /// Makes the next `update_chat` calls fail with the given errors, in
    /// order.
    pub fn fail_next_chat_updates(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().chat_failures.extend(errors);
    }

    /// Makes the next `vote_poll` calls fail with the given errors, in
    /// order.
    pub fn fail_next_votes(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().vote_failures.extend(errors);
    }

    /// Makes the next edits, deletions and stars of messages fail with the
    /// given errors, in order.
    pub fn fail_next_actions(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().action_failures.extend(errors);
    }

    /// What was done to messages, in order: `edit <id>`, `delete-all
    /// <id>`, `delete-me <id>`, `star <id>`, `unstar <id>`.
    pub fn actions(&self) -> Vec<String> {
        self.state().actions.clone()
    }

    /// The votes the provider accepted, in order: the poll and the choices.
    pub fn votes(&self) -> Vec<(MessageId, Vec<String>)> {
        self.state().votes.clone()
    }

    /// A message of the world, by id.
    pub fn message(&self, account: &AccountId, id: &MessageId) -> Option<Message> {
        self.state()
            .world
            .messages
            .iter()
            .filter(|((owner, _), _)| owner == account)
            .flat_map(|(_, thread)| thread.iter())
            .find(|message| &message.id == id)
            .cloned()
    }

    /// Makes the next `list_accounts` calls fail with the given errors, in
    /// order.
    pub fn fail_next_account_lists(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().account_failures.extend(errors);
    }

    /// Makes the next `fetch_messages` calls fail with the given errors,
    /// in order.
    pub fn fail_next_history(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().history_failures.extend(errors);
    }

    /// Makes the chat list report pin, mute and archive as "not observed"
    /// (false), whatever they are: what a backend does that has not seen
    /// WhatsApp report a change for the chat yet.
    pub fn observe_no_chat_state(&self, blind: bool) {
        self.state().blind_to_chat_state = blind;
    }

    /// Gives a chat a picture (or, with `None`, takes it away).
    pub fn set_avatar(&self, account: &AccountId, chat: &ChatId, picture: Option<(&str, Vec<u8>)>) {
        let key = (account.clone(), chat.clone());
        let mut state = self.state();
        match picture {
            Some((id, bytes)) => state.avatars.insert(key, (id.to_owned(), bytes)),
            None => state.avatars.remove(&key),
        };
    }

    /// The chats whose unread count was cleared through the provider, in
    /// order.
    pub fn marked_read(&self) -> Vec<ChatId> {
        self.state().marked_read.clone()
    }

    /// The chats for which read receipts were sent, in order.
    pub fn receipts_sent(&self) -> Vec<ChatId> {
        self.state().receipts_sent.clone()
    }

    /// Makes the chat list say nothing about how many messages of `chat`
    /// are unread, the way a backend does that never counted them.
    pub fn unread_unknown(&self, chat: &ChatId) {
        self.state().unread_unknown.push(chat.clone());
    }

    /// Sets what the provider says a chat's unread count is.
    pub fn set_unread(&self, account: &AccountId, chat: &ChatId, count: u32) {
        let updated = {
            let mut state = self.state();
            state
                .world
                .chats
                .iter_mut()
                .find(|c| &c.id == chat && &c.account_id == account)
                .map(|c| {
                    c.unread_count = count;
                    c.clone()
                })
        };
        if let Some(chat) = updated {
            self.emit(ProviderEvent::ChatUpdated(chat));
        }
    }

    /// Makes the chat list carry the id of each chat's picture, the way a
    /// backend does that knows it.
    pub fn list_picture_ids(&self, on: bool) {
        self.state().picture_ids = on;
    }

    /// Makes the next `fetch_avatar` calls fail with the given errors.
    pub fn fail_next_avatars(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().avatar_failures.extend(errors);
    }

    /// How many times `fetch_avatar` has been called.
    pub fn avatar_calls(&self) -> usize {
        self.state().avatar_calls
    }

    /// Serves `bytes` for the media reference `media`.
    pub fn set_media(&self, media: &str, bytes: Vec<u8>, mime: &str) {
        self.state()
            .media
            .insert(media.to_owned(), (bytes, mime.to_owned()));
    }

    /// Makes the next media downloads fail with the given errors, one
    /// per download.
    pub fn fail_next_media(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().media_failures.extend(errors);
    }

    /// Makes every media download take this long, reporting half of it
    /// on the way.
    pub fn set_media_latency(&self, latency: Duration) {
        self.state().media_latency = latency;
    }

    /// How many times media was downloaded.
    pub fn media_calls(&self) -> usize {
        self.state().media_calls
    }

    /// How many times `list_accounts` has been called.
    pub fn account_calls(&self) -> usize {
        self.state().account_calls
    }

    /// How many times `fetch_messages` has been called.
    pub fn history_calls(&self) -> usize {
        self.state().history_calls
    }

    /// Makes the next calls to [`Provider::upload_media`] fail with the
    /// given errors, one per call.
    pub fn fail_next_uploads(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().upload_failures.extend(errors);
    }

    /// How many times `upload_media` was called.
    pub fn upload_calls(&self) -> usize {
        self.state().upload_calls
    }

    /// The chats that were muted for a time, in order, with the seconds.
    pub fn timed_mutes(&self) -> Vec<(ChatId, u64)> {
        self.state().timed_mutes.clone()
    }

    /// The files that were uploaded, in order: their bytes and types.
    pub fn uploaded(&self) -> Vec<(Vec<u8>, String)> {
        self.state().uploaded.clone()
    }

    /// How many distinct files were uploaded.
    pub fn uploaded_count(&self) -> usize {
        self.state().uploads.len()
    }

    /// Makes every upload take this long.
    pub fn set_upload_latency(&self, latency: Duration) {
        self.state().upload_latency = latency;
    }

    /// Makes every reference uploaded so far stop working, the way an
    /// upload nobody used in time does.
    pub fn expire_uploads(&self) {
        let mut state = self.state();
        let all: Vec<String> = state.uploads.values().cloned().collect();
        state.expired_uploads.extend(all);
    }

    /// Sets the largest file that can be uploaded.
    pub fn set_upload_limit(&self, bytes: u64) {
        self.state().upload_limit = bytes;
    }

    /// Makes uploads unavailable, as on a backend that does not have them
    /// yet.
    pub fn set_uploads_available(&self, available: bool) {
        self.state().uploads_off = !available;
    }

    /// Sets the address book of an account.
    pub fn set_contacts(&self, account: &AccountId, contacts: Vec<Contact>) {
        self.state().contacts.insert(account.clone(), contacts);
    }

    /// Makes the next calls to [`Provider::list_contacts`] fail with the
    /// given errors, one per call.
    pub fn fail_next_contact_pages(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().contact_failures.extend(errors);
    }

    /// As [`fail_next_contact_pages`](Self::fail_next_contact_pages),
    /// after `pages` calls that work: a listing that breaks midway.
    pub fn fail_contact_pages_after(
        &self,
        pages: usize,
        errors: impl IntoIterator<Item = ProviderError>,
    ) {
        let mut state = self.state();
        state.contact_failures_after = pages;
        state.contact_failures.extend(errors);
    }

    /// How many times `list_contacts` was called.
    pub fn contact_calls(&self) -> usize {
        self.state().contact_calls
    }

    /// The cursors `list_contacts` was called with, in order.
    pub fn contact_cursors(&self) -> Vec<Option<String>> {
        self.state().contact_cursors.clone()
    }

    /// Says that a number (E.164) is not on WhatsApp.
    pub fn without_whatsapp(&self, phone: &str) {
        self.state().without_whatsapp.push(phone.to_owned());
    }

    /// How many times `check_numbers` was called.
    pub fn check_calls(&self) -> usize {
        self.state().check_calls
    }

    /// Makes the next calls to [`Provider::create_account`] fail with the
    /// given errors, one per call.
    pub fn fail_next_links(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().link_failures.extend(errors);
    }

    /// Makes the next calls to [`Provider::link_status`] fail with the
    /// given errors, one per call.
    pub fn fail_next_link_checks(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().link_check_failures.extend(errors);
    }

    /// Makes the next calls to [`Provider::update_account`] fail with the
    /// given errors, one per call.
    pub fn fail_next_account_updates(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().account_update_failures.extend(errors);
    }

    /// Makes the next calls to [`Provider::pairing_code`] fail with the
    /// given errors, one per call.
    pub fn fail_next_pairing_codes(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().pairing_failures.extend(errors);
    }

    /// How many times `pairing_code` was called.
    pub fn pairing_calls(&self) -> usize {
        self.state().pairing_calls
    }

    /// The code `account` shows is past its time, until another is asked
    /// for.
    pub fn expire_code(&self, account: &AccountId) {
        if let Some(link) = self.state().links.get_mut(account) {
            link.code_expired = true;
        }
    }

    /// The session of `account` starts over: nothing to show until its
    /// status is asked again, as when a QR code ran out.
    pub fn restart_link(&self, account: &AccountId) {
        if let Some(link) = self.state().links.get_mut(account) {
            link.checks = 0;
        }
    }

    /// The QR code of `account` is, or is no longer, one that cannot be
    /// read.
    pub fn garble_code(&self, account: &AccountId, garbled: bool) {
        if let Some(link) = self.state().links.get_mut(account) {
            link.unreadable = garbled;
        }
    }

    /// The phone scanned the code (or the code was typed): the number is
    /// linked at its next status check.
    pub fn complete_link(&self, account: &AccountId) {
        if let Some(link) = self.state().links.get_mut(account) {
            link.done = true;
        }
    }

    /// Linking `account` stops for `reason`.
    pub fn stop_link(&self, account: &AccountId, reason: &str) {
        if let Some(link) = self.state().links.get_mut(account) {
            link.stopped = Some(reason.to_owned());
        }
    }

    /// How many times `create_account` was called.
    pub fn create_calls(&self) -> usize {
        self.state().create_calls
    }

    /// How many times `link_status` was called.
    pub fn link_checks(&self) -> usize {
        self.state().link_checks
    }

    /// The numbers that were deleted, in order.
    pub fn deleted_accounts(&self) -> Vec<AccountId> {
        self.state().deleted.clone()
    }

    /// The provider's own copy of a number.
    pub fn account(&self, account: &AccountId) -> Option<Account> {
        self.state()
            .world
            .accounts
            .iter()
            .find(|a| &a.id == account)
            .cloned()
    }

    /// Where linking `account` stands, advancing it one check.
    fn link_step(&self, account: &AccountId, count: bool) -> ProviderResult<LinkStatus> {
        let mut state = self.state();
        let link_after = self.inner.config.link_after;
        let State { world, links, .. } = &mut *state;
        let Some(entry) = world.accounts.iter_mut().find(|a| &a.id == account) else {
            return Err(ProviderError::Rejected {
                code: "not_found".into(),
                message: "There is no such number.".into(),
            });
        };
        let Some(link) = links.get_mut(account) else {
            // Not being linked: it is whatever its connection says.
            let step = match &entry.connection {
                ConnectionState::Connected | ConnectionState::Reconnecting => LinkStep::Linked,
                ConnectionState::Connecting => LinkStep::Starting,
                ConnectionState::Disconnected { reason } => LinkStep::Stopped {
                    reason: reason.clone().unwrap_or_else(|| "Not connected.".into()),
                },
                ConnectionState::LoggedOut => LinkStep::Stopped {
                    reason: "The phone unlinked this device.".into(),
                },
            };
            return Ok(LinkStatus {
                account: entry.clone(),
                step,
                was_linked: true,
            });
        };
        if count {
            link.checks += 1;
        }
        if link_after.is_some_and(|after| link.checks >= after) {
            link.done = true;
        }
        let step = if let Some(reason) = &link.stopped {
            entry.connection = ConnectionState::Disconnected {
                reason: Some(reason.clone()),
            };
            LinkStep::Stopped {
                reason: reason.clone(),
            }
        } else if link.done {
            link.was_linked = true;
            entry.settings.ever_linked = Some(true);
            entry.connection = ConnectionState::Connected;
            if entry.phone.is_none() {
                let phone = link
                    .phone
                    .clone()
                    .unwrap_or_else(|| "+584125550100".to_owned());
                entry.self_contact = Some(client_provider::ContactId::new(phone.clone()));
                entry.phone = Some(phone);
            }
            LinkStep::Linked
        } else if link.checks == 0 {
            LinkStep::Starting
        } else if link.phone.is_some() {
            let life = if link.code_expired { -1_000 } else { 160_000 };
            LinkStep::TypeCode {
                code: format!("WUAP-{}", 1234 + link.codes),
                expires_at: Some(Timestamp::from_millis(Timestamp::now().as_millis() + life)),
            }
        } else if link.unreadable {
            LinkStep::Scan {
                png: b"not a picture".to_vec(),
            }
        } else {
            LinkStep::Scan {
                png: seed::picture("mock://image/7/240x240").unwrap_or_default(),
            }
        };
        let status = LinkStatus {
            account: entry.clone(),
            step,
            was_linked: link.was_linked,
        };
        if link.done && link.stopped.is_none() {
            links.remove(account);
        }
        Ok(status)
    }

    /// The provider's own copy of a chat.
    pub fn chat(&self, account: &AccountId, chat: &ChatId) -> Option<Chat> {
        self.state()
            .world
            .chats
            .iter()
            .find(|c| &c.id == chat && &c.account_id == account)
            .cloned()
    }

    /// Every message it was asked to send, in order, as it was asked.
    pub fn sent(&self) -> Vec<OutgoingMessage> {
        self.state().asked.clone()
    }

    /// How many times [`Provider::send`] has been called.
    pub fn send_calls(&self) -> usize {
        self.state().send_calls
    }

    /// How many distinct messages were actually sent through
    /// [`Provider::send`]. With idempotent sends this never exceeds the
    /// number of distinct client ids.
    pub fn delivered_count(&self) -> usize {
        self.state().sent.len()
    }

    /// Injects an event as if the backend had pushed it. Message and chat
    /// events also update the mock's own world, so history stays consistent
    /// with what was pushed.
    pub fn push_event(&self, event: ProviderEvent) {
        {
            let mut state = self.state();
            match &event {
                ProviderEvent::MessageUpserted(message) => {
                    let key = (message.account_id.clone(), message.chat_id.clone());
                    let thread = state.world.messages.entry(key).or_default();
                    match thread.iter_mut().find(|m| m.id == message.id) {
                        Some(existing) => *existing = message.clone(),
                        None => thread.push(message.clone()),
                    }
                    // The chat list shows the newest message of each chat.
                    if let Some(chat) = state
                        .world
                        .chats
                        .iter_mut()
                        .find(|c| c.id == message.chat_id && c.account_id == message.account_id)
                    {
                        let newer = chat
                            .last_message
                            .as_ref()
                            .is_none_or(|last| last.timestamp <= message.timestamp);
                        if newer && !matches!(message.content, MessageContent::Reaction { .. }) {
                            chat.last_message = Some(message.clone());
                        }
                    }
                }
                ProviderEvent::MessageStatusChanged {
                    account_id,
                    chat_id,
                    message_id,
                    status,
                } => {
                    let key = (account_id.clone(), chat_id.clone());
                    if let Some(m) = state
                        .world
                        .messages
                        .get_mut(&key)
                        .and_then(|t| t.iter_mut().find(|m| &m.id == message_id))
                    {
                        m.status = status.clone();
                    }
                }
                ProviderEvent::ChatUpdated(chat) => {
                    match state
                        .world
                        .chats
                        .iter_mut()
                        .find(|c| c.id == chat.id && c.account_id == chat.account_id)
                    {
                        Some(existing) => *existing = chat.clone(),
                        None => state.world.chats.push(chat.clone()),
                    }
                }
                ProviderEvent::ConnectionChanged {
                    account_id,
                    state: s,
                } => {
                    if let Some(a) = state
                        .world
                        .accounts
                        .iter_mut()
                        .find(|a| &a.id == account_id)
                    {
                        a.connection = s.clone();
                    }
                }
                ProviderEvent::Presence { .. }
                | ProviderEvent::ContactUpdated(_)
                | ProviderEvent::GroupChanged { .. }
                | ProviderEvent::StoryUpserted(_)
                | ProviderEvent::StoryRemoved { .. }
                | ProviderEvent::StoryViewed { .. }
                | ProviderEvent::StoryMuteChanged { .. } => {}
            }
        }
        self.emit(event);
    }

    /// Makes a part the capabilities promise missing for now, for one
    /// account or (`None`) for all of them, or puts it back: what a
    /// backend without the routes, or a number the part is not turned on
    /// for, looks like. Unlike a capability it can change at any time.
    pub fn set_unavailable(
        &self,
        account: Option<&AccountId>,
        feature: client_provider::Feature,
        unavailable: bool,
    ) {
        let key = (account.cloned(), feature);
        let mut state = self.state();
        if unavailable {
            state.unavailable.insert(key);
        } else {
            state.unavailable.remove(&key);
        }
    }

    /// What a provider that remembers a missing part looks like once its
    /// backend has it: `feature` stays missing until the client asks for
    /// it again ([`Provider::recheck`]), and is there from then on.
    pub fn restore_at_recheck(&self, feature: client_provider::Feature) {
        self.state().restored_by_recheck.insert(feature);
    }

    pub(crate) fn is_unavailable(
        &self,
        account: &AccountId,
        feature: client_provider::Feature,
    ) -> bool {
        let state = self.state();
        state.unavailable.contains(&(None, feature))
            || state
                .unavailable
                .contains(&(Some(account.clone()), feature))
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().expect("mock state poisoned")
    }

    fn emit(&self, event: ProviderEvent) {
        // No subscriber is fine: events are hints, history is the truth.
        let _ = self.inner.events.send(event);
    }

    fn scaled(&self, millis: u64) -> Duration {
        Duration::from_millis((millis as f32 * self.inner.config.time_scale) as u64)
    }

    /// Old messages appear in a chat the way imported history does:
    /// dated in the past, oldest first, and with no event for any of
    /// them. The chat is created if the number has none with `phone`.
    pub fn import_history(&self, account: &AccountId, phone: &str, texts: &[&str]) -> ChatId {
        let chat_id = ChatId::new(phone);
        let mut state = self.state();
        let key = (account.clone(), chat_id.clone());
        let day = 86_400_000;
        let start = Timestamp::now().as_millis() - day * (texts.len() as i64 + 1);
        let mut imported = Vec::new();
        for (index, text) in texts.iter().enumerate() {
            let id = state.world.next_message;
            state.world.next_message += 1;
            imported.push(Message {
                id: MessageId::new(format!("m{id}")),
                client_id: None,
                account_id: account.clone(),
                chat_id: chat_id.clone(),
                sender: client_provider::ContactId::new(phone),
                sender_name: None,
                direction: Direction::Incoming,
                timestamp: Timestamp::from_millis(start + day * index as i64),
                content: MessageContent::Text {
                    body: (*text).to_owned(),
                },
                reply_to: None,
                status: DeliveryStatus::Delivered,
                edited: false,
                deleted: false,
                extras: Default::default(),
            });
        }
        let history = state.world.messages.entry(key).or_default();
        // Under what is already there.
        imported.append(history);
        *history = imported;
        let newest = history.last().cloned();
        match state
            .world
            .chats
            .iter_mut()
            .find(|c| c.id == chat_id && &c.account_id == account)
        {
            Some(chat) => chat.last_message = newest,
            None => state.world.chats.push(Chat {
                id: chat_id.clone(),
                account_id: account.clone(),
                kind: ChatKind::Direct,
                title: phone.to_owned(),
                avatar: None,
                unread_count: 0,
                pinned: false,
                muted: false,
                archived: false,
                last_message: newest,
                unknown: Default::default(),
                picture_id: None,
                pinned_at: None,
            }),
        }
        chat_id
    }

    /// Appends an incoming message to a chat and emits the matching events.
    fn receive(&self, account: &AccountId, chat: &ChatId, content: MessageContent) {
        let (message, chat_snapshot) = {
            let mut state = self.state();
            let key = (account.clone(), chat.clone());
            let members = state.world.members.get(&key).cloned().unwrap_or_default();
            let name = if members.is_empty() {
                None
            } else {
                let i = state.rng.below(members.len() as u64) as usize;
                Some(members[i].clone())
            };
            let id = state.world.next_message;
            state.world.next_message += 1;
            let message = Message {
                id: MessageId::new(format!("m{id}")),
                client_id: None,
                account_id: account.clone(),
                chat_id: chat.clone(),
                sender: name
                    .as_deref()
                    .map(seed::contact_for)
                    .unwrap_or_else(|| client_provider::ContactId::new(chat.as_str())),
                sender_name: name,
                direction: Direction::Incoming,
                timestamp: Timestamp::now(),
                content,
                reply_to: None,
                status: DeliveryStatus::Delivered,
                edited: false,
                deleted: false,
                extras: Default::default(),
            };
            state
                .world
                .messages
                .entry(key)
                .or_default()
                .push(message.clone());
            let chat_snapshot = state
                .world
                .chats
                .iter_mut()
                .find(|c| &c.id == chat && &c.account_id == account)
                .map(|c| {
                    c.unread_count += 1;
                    c.last_message = Some(message.clone());
                    c.clone()
                });
            (message, chat_snapshot)
        };
        self.emit(ProviderEvent::MessageUpserted(message));
        if let Some(chat) = chat_snapshot {
            self.emit(ProviderEvent::ChatUpdated(chat));
        }
    }

    fn set_status(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        status: DeliveryStatus,
    ) {
        {
            let mut state = self.state();
            let key = (account.clone(), chat.clone());
            if let Some(m) = state
                .world
                .messages
                .get_mut(&key)
                .and_then(|t| t.iter_mut().find(|m| &m.id == message))
            {
                m.status = status.clone();
            }
        }
        self.emit(ProviderEvent::MessageStatusChanged {
            account_id: account.clone(),
            chat_id: chat.clone(),
            message_id: message.clone(),
            status,
        });
    }

    /// Plays the other side of a conversation after a message was sent.
    fn spawn_aftermath(
        &self,
        account: AccountId,
        chat: ChatId,
        message: MessageId,
        kind: ChatKind,
    ) {
        let this = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(this.scaled(900)).await;
            this.set_status(&account, &chat, &message, DeliveryStatus::Delivered);
            tokio::time::sleep(this.scaled(1600)).await;
            this.set_status(&account, &chat, &message, DeliveryStatus::Read);

            tokio::time::sleep(this.scaled(700)).await;
            let (contact, line) = {
                let mut state = this.state();
                let line = match kind {
                    ChatKind::Direct => seed::their_line(&mut state.rng),
                    ChatKind::Group => seed::group_line(&mut state.rng),
                };
                (client_provider::ContactId::new(chat.as_str()), line)
            };
            this.emit(ProviderEvent::Presence {
                account_id: account.clone(),
                chat_id: chat.clone(),
                contact_id: contact.clone(),
                state: PresenceState::Typing,
            });
            tokio::time::sleep(this.scaled(2200)).await;
            this.emit(ProviderEvent::Presence {
                account_id: account.clone(),
                chat_id: chat.clone(),
                contact_id: contact,
                state: PresenceState::Idle,
            });
            this.receive(&account, &chat, MessageContent::text(line));
        });
    }

    /// Starts the background traffic the first time somebody subscribes.
    fn start_ambient(&self) {
        let Some(interval) = self.inner.config.ambient_traffic else {
            return;
        };
        if self.inner.ambient_started.swap(true, Ordering::SeqCst) {
            return;
        }
        // Hold only a weak reference so the task ends with the provider.
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let Some(inner) = weak.upgrade() else { break };
                let this = MockProvider { inner };
                let target = {
                    let mut state = this.state();
                    let count = state.world.chats.len() as u64;
                    // Skip the first chats so the one being looked at stays calm.
                    let i = state.rng.range(2, count.saturating_sub(1).max(2)) as usize;
                    let line_kind = state.world.chats.get(i).map(|c| c.kind);
                    let line = match line_kind {
                        Some(ChatKind::Group) => seed::group_line(&mut state.rng),
                        _ => seed::their_line(&mut state.rng),
                    };
                    state
                        .world
                        .chats
                        .get(i)
                        .map(|c| (c.account_id.clone(), c.id.clone(), line))
                };
                if let Some((account, chat, line)) = target {
                    this.receive(&account, &chat, MessageContent::text(line));
                }
                // And now and then somebody posts a status.
                if this.state().rng.chance(25) {
                    this.ambient_story();
                }
            }
        });
    }
}

fn offset_of(cursor: Option<Cursor>) -> ProviderResult<usize> {
    match cursor {
        None => Ok(0),
        Some(c) => c.as_str().parse().map_err(|_| ProviderError::Rejected {
            code: "invalid_cursor".into(),
            message: "The cursor is not valid.".into(),
        }),
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn mention_handle(&self, contact: &client_provider::ContactId) -> String {
        // The demo's ids are not numbers: digits made from them stand in.
        showcase::handle_of_id(contact)
    }

    fn capabilities(&self) -> Capabilities {
        if let Some(chosen) = self.inner.config.capabilities {
            return chosen;
        }
        Capabilities {
            // Files are kept in memory and echoed back.
            media_upload: true,
            // Pictures are whatever a test hands it; none by default.
            avatars: true,
            sticker_favorites: self.favorites_available(),
            forward_any: self.forward_available(),
            ..Capabilities::all()
        }
    }

    fn unavailable(&self, account: &AccountId, feature: client_provider::Feature) -> bool {
        self.is_unavailable(account, feature)
    }

    fn recheck(&self, _account: &AccountId, feature: client_provider::Feature) {
        // The mock remembers nothing: what is missing is what a test
        // said is missing. That it was asked is recorded.
        if feature == client_provider::Feature::Stories {
            self.note_story_recheck();
        }
        // A provider that only remembered the part as missing finds it
        // when it asks again.
        let mut state = self.state();
        if state.restored_by_recheck.remove(&feature) {
            state.unavailable.retain(|(_, missing)| *missing != feature);
        }
    }

    async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
        let mut state = self.state();
        state.account_calls += 1;
        if let Some(error) = state.account_failures.pop_front() {
            return Err(error);
        }
        Ok(state.world.accounts.clone())
    }

    async fn list_chats(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Chat>> {
        let offset = offset_of(cursor)?;
        let state = self.state();
        let mut chats: Vec<&Chat> = state
            .world
            .chats
            .iter()
            .filter(|c| &c.account_id == account)
            .collect();
        chats.sort_by_key(|c| {
            std::cmp::Reverse(
                c.last_message
                    .as_ref()
                    .map(|m| m.timestamp)
                    .unwrap_or_default(),
            )
        });
        let blind = state.blind_to_chat_state;
        let items: Vec<Chat> = chats
            .iter()
            .skip(offset)
            .take(CHAT_PAGE)
            .map(|c| {
                let mut chat = (*c).clone();
                if blind {
                    chat.pinned = false;
                    chat.muted = false;
                    chat.archived = false;
                    chat.unknown = ChatUnknown {
                        pinned: true,
                        muted: true,
                        archived: true,
                        picture: chat.unknown.picture,
                        unread: false,
                    };
                }
                if state.unread_unknown.contains(&chat.id) {
                    chat.unread_count = 0;
                    chat.unknown.unread = true;
                }
                if state.picture_ids {
                    // A backend whose chat list says which picture each
                    // chat has.
                    let key = (chat.account_id.clone(), chat.id.clone());
                    chat.picture_id = state.avatars.get(&key).map(|(id, _)| id.clone());
                    chat.unknown.picture = false;
                }
                chat
            })
            .collect();
        let next = offset + items.len();
        Ok(Page {
            next_cursor: (next < chats.len()).then(|| Cursor::new(next.to_string())),
            items,
        })
    }

    async fn fetch_messages(
        &self,
        account: &AccountId,
        chat: &ChatId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> ProviderResult<Page<Message>> {
        let mut state = self.state();
        state.history_calls += 1;
        if let Some(error) = state.history_failures.pop_front() {
            return Err(error);
        }
        let key = (account.clone(), chat.clone());
        let Some(thread) = state.world.messages.get(&key) else {
            return Err(ProviderError::Rejected {
                code: "not_found".into(),
                message: "No such chat.".into(),
            });
        };
        // The cursor is the index of the oldest message already returned.
        // Threads only grow at the end, so it stays valid as messages arrive.
        let end = match cursor {
            None => thread.len(),
            Some(c) => offset_of(Some(c))?.min(thread.len()),
        };
        let start = end.saturating_sub(limit.clamp(1, 200) as usize);
        let items: Vec<Message> = thread[start..end].iter().rev().cloned().collect();
        Ok(Page {
            items,
            next_cursor: (start > 0).then(|| Cursor::new(start.to_string())),
        })
    }

    async fn send(&self, outgoing: OutgoingMessage) -> ProviderResult<SendReceipt> {
        let (receipt, kind) = {
            let mut state = self.state();
            state.send_calls += 1;
            state.asked.push(outgoing.clone());
            if let Some(error) = state.send_failures.pop_front() {
                return Err(error);
            }
            // Idempotency: a client id already sent returns its first receipt.
            if let Some(receipt) = state.sent.get(&outgoing.client_id) {
                return Ok(receipt.clone());
            }
            let key = (outgoing.account_id.clone(), outgoing.chat_id.clone());
            let Some(kind) = state
                .world
                .chats
                .iter()
                .find(|c| c.id == outgoing.chat_id && c.account_id == outgoing.account_id)
                .map(|c| c.kind)
            else {
                return Err(ProviderError::Rejected {
                    code: "not_found".into(),
                    message: "No such chat.".into(),
                });
            };
            let content = match &outgoing.content {
                OutgoingContent::Text { body } if body.trim().is_empty() => {
                    return Err(ProviderError::Rejected {
                        code: "invalid_request".into(),
                        message: "The message is empty.".into(),
                    });
                }
                OutgoingContent::Text { body } => MessageContent::text(body.clone()),
                OutgoingContent::Reaction { target, emoji } => MessageContent::Reaction {
                    target: target.clone(),
                    emoji: emoji.clone(),
                },
                OutgoingContent::Media {
                    kind,
                    media,
                    mime_type,
                    caption,
                    file_name,
                    gif,
                } => {
                    if let Some(at) = state
                        .expired_uploads
                        .iter()
                        .position(|gone| gone == media.as_str())
                    {
                        state.expired_uploads.remove(at);
                        state.uploads.retain(|_, known| known != media.as_str());
                        state.media.remove(media.as_str());
                        return Err(ProviderError::Rejected {
                            code: "upload_expired".into(),
                            message: "The uploaded file is no longer there.".into(),
                        });
                    }
                    let Some((bytes, _)) = state.media.get(media.as_str()) else {
                        return Err(ProviderError::Rejected {
                            code: "upload_expired".into(),
                            message: "The uploaded file is no longer there.".into(),
                        });
                    };
                    // The message echoes what was uploaded.
                    let mut sent = client_provider::Media::new(*kind);
                    sent.size_bytes = Some(bytes.len() as u64);
                    sent.source = Some(media.clone());
                    sent.mime_type = mime_type.clone();
                    sent.caption = caption.clone();
                    sent.file_name = file_name.clone();
                    sent.gif = *gif && *kind == client_provider::MediaKind::Video;
                    MessageContent::Media(sent)
                }
                OutgoingContent::Forward { .. } => {
                    return Err(ProviderError::Rejected {
                        code: "invalid_request".into(),
                        message: "A forward goes through forward_messages.".into(),
                    });
                }
                OutgoingContent::Poll {
                    question, options, ..
                } if question.trim().is_empty() || options.len() < 2 => {
                    return Err(ProviderError::Rejected {
                        code: "invalid_request".into(),
                        message: "A poll needs a question and at least two options.".into(),
                    });
                }
                OutgoingContent::Poll {
                    question,
                    options,
                    max_choices,
                } => MessageContent::Poll(client_provider::Poll {
                    question: question.clone(),
                    options: options
                        .iter()
                        .map(|name| client_provider::PollOption {
                            name: name.clone(),
                            votes: 0,
                        })
                        .collect(),
                    max_choices: *max_choices,
                    voters: 0,
                    chosen: Some(Vec::new()),
                }),
            };
            let reply_to = outgoing.reply_to.as_ref().map(|id| {
                let quoted = state
                    .world
                    .messages
                    .get(&key)
                    .and_then(|t| t.iter().find(|m| &m.id == id));
                ReplyRef {
                    message_id: id.clone(),
                    sender_name: quoted.and_then(|m| m.sender_name.clone()),
                    preview: quoted.and_then(|m| match &m.content {
                        MessageContent::Text { body } => Some(body.chars().take(80).collect()),
                        _ => None,
                    }),
                }
            });
            let n = state.world.next_message;
            state.world.next_message += 1;
            let now = Timestamp::now();
            let message = Message {
                id: MessageId::new(format!("m{n}")),
                client_id: Some(outgoing.client_id.clone()),
                account_id: outgoing.account_id.clone(),
                chat_id: outgoing.chat_id.clone(),
                sender: seed::self_contact(&outgoing.account_id),
                sender_name: None,
                direction: Direction::Outgoing,
                timestamp: now,
                content,
                reply_to,
                status: DeliveryStatus::Sent,
                edited: false,
                deleted: false,
                extras: client_provider::MessageExtras {
                    mentions: outgoing.mentions.clone(),
                    forwarded: outgoing.forwarded,
                    ..Default::default()
                },
            };
            let receipt = SendReceipt {
                message_id: message.id.clone(),
                status: DeliveryStatus::Sent,
                timestamp: Some(now),
            };
            state
                .sent
                .insert(outgoing.client_id.clone(), receipt.clone());
            let is_reaction = matches!(message.content, MessageContent::Reaction { .. });
            state
                .world
                .messages
                .entry(key)
                .or_default()
                .push(message.clone());
            if !is_reaction {
                if let Some(chat) = state
                    .world
                    .chats
                    .iter_mut()
                    .find(|c| c.id == outgoing.chat_id && c.account_id == outgoing.account_id)
                {
                    chat.last_message = Some(message);
                }
            }
            (receipt, (!is_reaction).then_some(kind))
        };

        // The message is on "WhatsApp" now; the answer takes a while to
        // travel back. A caller that gives up waiting here has still sent
        // the message, which is exactly the case idempotency exists for.
        if !self.inner.config.send_latency.is_zero() {
            tokio::time::sleep(self.inner.config.send_latency).await;
        }

        if let (true, Some(kind)) = (self.inner.config.simulate_replies, kind) {
            self.spawn_aftermath(
                outgoing.account_id,
                outgoing.chat_id,
                receipt.message_id.clone(),
                kind,
            );
        }
        Ok(receipt)
    }

    async fn forward_messages(
        &self,
        account: &AccountId,
        items: &[client_provider::ForwardItem],
    ) -> ProviderResult<Vec<ProviderResult<SendReceipt>>> {
        if self.is_unavailable(account, client_provider::Feature::ForwardAny) {
            self.note_forward_refused();
            return Err(ProviderError::Unsupported("forwarding messages"));
        }
        let answer = self.forward_items(account, items)?;
        // The copies are on "WhatsApp"; the answer takes a while to come
        // back, as a send's does.
        if !self.inner.config.send_latency.is_zero() {
            tokio::time::sleep(self.inner.config.send_latency).await;
        }
        Ok(answer)
    }

    async fn mark_read(
        &self,
        account: &AccountId,
        chat: &ChatId,
        _up_to: Option<&MessageId>,
    ) -> ProviderResult<()> {
        self.state().receipts_sent.push(chat.clone());
        self.mark_read_quietly(account, chat).await
    }

    async fn mark_read_quietly(&self, account: &AccountId, chat: &ChatId) -> ProviderResult<()> {
        let updated = {
            let mut state = self.state();
            state.marked_read.push(chat.clone());
            state
                .world
                .chats
                .iter_mut()
                .find(|c| &c.id == chat && &c.account_id == account)
                .filter(|c| c.unread_count > 0)
                .map(|c| {
                    c.unread_count = 0;
                    c.clone()
                })
        };
        if let Some(chat) = updated {
            self.emit(ProviderEvent::ChatUpdated(chat));
        }
        Ok(())
    }

    async fn vote_poll(
        &self,
        account: &AccountId,
        chat: &ChatId,
        poll: &MessageId,
        choices: &[String],
    ) -> ProviderResult<Option<Message>> {
        let updated = {
            let mut state = self.state();
            if let Some(error) = state.vote_failures.pop_front() {
                return Err(error);
            }
            let found = state
                .world
                .messages
                .get_mut(&(account.clone(), chat.clone()))
                .and_then(|thread| thread.iter_mut().find(|message| &message.id == poll));
            let Some(message) = found else {
                return Err(ProviderError::Rejected {
                    code: "not_found".into(),
                    message: "No such poll.".into(),
                });
            };
            let MessageContent::Poll(current) = &message.content else {
                return Err(ProviderError::Rejected {
                    code: "invalid_request".into(),
                    message: "That message is not a poll.".into(),
                });
            };
            if let Some(stray) = choices
                .iter()
                .find(|choice| !current.options.iter().any(|option| &option.name == *choice))
            {
                return Err(ProviderError::Rejected {
                    code: "invalid_request".into(),
                    message: format!("“{stray}” is not an option of this poll."),
                });
            }
            // Setting the same vote twice changes nothing: safe to repeat.
            message.content = MessageContent::Poll(current.with_vote(choices));
            let updated = message.clone();
            state.votes.push((poll.clone(), choices.to_vec()));
            updated
        };
        self.emit(ProviderEvent::MessageUpserted(updated.clone()));
        Ok(Some(updated))
    }

    async fn edit_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        text: &str,
    ) -> ProviderResult<()> {
        let text = text.to_owned();
        self.act(account, chat, message, "edit", move |found| {
            if found.direction != Direction::Outgoing {
                return Err(ProviderError::Rejected {
                    code: "invalid_request".into(),
                    message: "Only your own messages can be edited.".into(),
                });
            }
            if !matches!(found.content, MessageContent::Text { .. }) {
                return Err(ProviderError::Rejected {
                    code: "invalid_request".into(),
                    message: "Only text messages can be edited.".into(),
                });
            }
            // WhatsApp's window: a quarter of an hour.
            if Timestamp::now().as_millis() - found.timestamp.as_millis() > 15 * 60 * 1000 {
                return Err(ProviderError::Rejected {
                    code: "edit_window_closed".into(),
                    message: "It is too late to edit this message.".into(),
                });
            }
            found.content = MessageContent::text(text);
            found.edited = true;
            Ok(true)
        })
    }

    async fn delete_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        for_everyone: bool,
    ) -> ProviderResult<()> {
        let what = if for_everyone {
            "delete-all"
        } else {
            "delete-me"
        };
        self.act(account, chat, message, what, move |found| {
            if for_everyone && found.direction != Direction::Outgoing {
                return Err(ProviderError::Rejected {
                    code: "invalid_request".into(),
                    message: "Only your own messages can be deleted for everyone.".into(),
                });
            }
            if for_everyone {
                found.deleted = true;
                found.content = MessageContent::text("");
            }
            // For the account alone: the message goes from its thread.
            Ok(for_everyone)
        })
    }

    async fn star_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        starred: bool,
    ) -> ProviderResult<()> {
        let what = if starred { "star" } else { "unstar" };
        self.act(account, chat, message, what, move |found| {
            found.extras.starred = starred;
            Ok(true)
        })
    }

    async fn update_chat(
        &self,
        account: &AccountId,
        chat: &ChatId,
        change: ChatChange,
    ) -> ProviderResult<()> {
        let mut timed = None;
        let updated = {
            let mut state = self.state();
            if let Some(error) = state.chat_failures.pop_front() {
                return Err(error);
            }
            let Some(found) = state
                .world
                .chats
                .iter_mut()
                .find(|c| &c.id == chat && &c.account_id == account)
            else {
                return Err(ProviderError::Rejected {
                    code: "not_found".into(),
                    message: "No such chat.".into(),
                });
            };
            match change {
                ChatChange::Pinned(on) => {
                    found.pinned = on;
                    found.pinned_at = on.then(Timestamp::now);
                }
                ChatChange::Muted(on) => found.muted = on,
                ChatChange::MutedFor(seconds) => {
                    found.muted = true;
                    timed = Some(seconds);
                }
                ChatChange::Archived(on) => found.archived = on,
                ChatChange::MarkedUnread => found.unread_count = found.unread_count.max(1),
            }
            found.clone()
        };
        if let Some(seconds) = timed {
            self.state().timed_mutes.push((chat.clone(), seconds));
        }
        self.emit(ProviderEvent::ChatUpdated(updated));
        Ok(())
    }

    async fn start_chat(&self, account: &AccountId, phone: &str) -> ProviderResult<Chat> {
        let digits: String = phone.chars().filter(char::is_ascii_digit).collect();
        if !(7..=15).contains(&digits.len()) {
            return Err(ProviderError::Rejected {
                code: "invalid_phone".into(),
                message: "That does not look like a phone number.".into(),
            });
        }
        let id = ChatId::new(format!("+{digits}"));
        let mut state = self.state();
        if let Some(existing) = state
            .world
            .chats
            .iter()
            .find(|c| c.id == id && &c.account_id == account)
        {
            return Ok(existing.clone());
        }
        let chat = Chat {
            id: id.clone(),
            account_id: account.clone(),
            kind: ChatKind::Direct,
            title: id.to_string(),
            avatar: None,
            unread_count: 0,
            pinned: false,
            muted: false,
            archived: false,
            last_message: None,
            unknown: Default::default(),
            picture_id: None,
            pinned_at: None,
        };
        state.world.chats.push(chat.clone());
        state
            .world
            .messages
            .insert((account.clone(), id), Vec::new());
        Ok(chat)
    }

    async fn fetch_media_reporting(
        &self,
        account: &AccountId,
        media: &MediaRef,
        limit: client_provider::MediaLimit,
        progress: client_provider::UploadProgress,
    ) -> ProviderResult<MediaData> {
        let (failure, latency) = {
            let mut state = self.state();
            (state.media_failures.pop_front(), state.media_latency)
        };
        if let Some(error) = failure {
            self.state().media_calls += 1;
            return Err(error);
        }
        let data = self.fetch_media(account, media, limit).await?;
        let total = data.bytes.len() as u64;
        if !latency.is_zero() {
            progress(total / 2);
            tokio::time::sleep(latency).await;
        }
        progress(total);
        Ok(data)
    }

    async fn upload_media(
        &self,
        _account: &AccountId,
        upload: client_provider::MediaUpload,
        progress: client_provider::UploadProgress,
    ) -> ProviderResult<MediaRef> {
        let latency = {
            let mut state = self.state();
            state.upload_calls += 1;
            if state.uploads_off {
                return Err(ProviderError::Unsupported("sending files"));
            }
            if let Some(error) = state.upload_failures.pop_front() {
                return Err(error);
            }
            if upload.bytes.len() as u64 > state.upload_limit {
                return Err(ProviderError::Rejected {
                    code: "too_large".into(),
                    message: "The file is larger than this provider takes.".into(),
                });
            }
            // The same key again: the same upload.
            if let Some(known) = state.uploads.get(&upload.key) {
                return Ok(MediaRef::new(known.clone()));
            }
            state.upload_latency
        };
        let total = upload.bytes.len() as u64;
        progress(total / 2);
        if !latency.is_zero() {
            tokio::time::sleep(latency).await;
        }
        progress(total);
        let mut state = self.state();
        let reference = format!("mock://upload/{}", state.uploads.len() + 1);
        state.uploads.insert(upload.key.clone(), reference.clone());
        state
            .uploaded
            .push((upload.bytes.to_vec(), upload.mime_type.clone()));
        state.media.insert(
            reference.clone(),
            (upload.bytes.as_ref().clone(), upload.mime_type.clone()),
        );
        Ok(MediaRef::new(reference))
    }

    fn media_upload_limit(&self) -> Option<u64> {
        Some(self.state().upload_limit)
    }

    async fn media_upload_ready(&self) -> bool {
        !self.state().uploads_off
    }

    async fn list_favorite_stickers(
        &self,
        account: &AccountId,
    ) -> ProviderResult<Vec<client_provider::FavoriteSticker>> {
        self.favorites_of(account)
    }

    async fn add_favorite_sticker(
        &self,
        account: &AccountId,
        sticker: client_provider::StickerFile,
    ) -> ProviderResult<String> {
        self.favorite_added(account, sticker)
    }

    async fn remove_favorite_sticker(&self, account: &AccountId, id: &str) -> ProviderResult<()> {
        self.favorite_removed(account, id)
    }

    async fn download_media(
        &self,
        _account: &AccountId,
        media: &MediaRef,
    ) -> ProviderResult<MediaData> {
        let mut state = self.state();
        state.media_calls += 1;
        if let Some((bytes, mime)) = state.media.get(media.as_str()) {
            return Ok(MediaData {
                bytes: bytes.clone(),
                mime_type: Some(mime.clone()),
            });
        }
        // The demo's photos are drawn on the spot.
        if let Some(png) = seed::picture(media.as_str()) {
            return Ok(MediaData {
                bytes: png,
                mime_type: Some("image/png".into()),
            });
        }
        // And so are its animated stickers.
        if let Some(gif) = seed::sticker(media.as_str()) {
            return Ok(MediaData {
                bytes: gif,
                mime_type: Some("image/gif".into()),
            });
        }
        // The mock has no other real files; hand back a recognisable
        // placeholder.
        Ok(MediaData {
            bytes: media.as_str().as_bytes().to_vec(),
            mime_type: Some("application/octet-stream".into()),
        })
    }

    async fn fetch_avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
        known: Option<&str>,
    ) -> ProviderResult<AvatarAnswer> {
        let mut state = self.state();
        state.avatar_calls += 1;
        if let Some(error) = state.avatar_failures.pop_front() {
            return Err(error);
        }
        Ok(
            match state.avatars.get(&(account.clone(), subject.clone())) {
                None => AvatarAnswer::None,
                Some((id, _)) if known == Some(id.as_str()) => AvatarAnswer::Unchanged,
                Some((id, bytes)) => AvatarAnswer::New(Avatar {
                    id: id.clone(),
                    bytes: bytes.clone(),
                    mime_type: Some("image/png".to_owned()),
                }),
            },
        )
    }

    async fn list_contacts(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Contact>> {
        let mut state = self.state();
        state.contact_calls += 1;
        state
            .contact_cursors
            .push(cursor.as_ref().map(|cursor| cursor.as_str().to_owned()));
        if state.contact_failures_after > 0 {
            state.contact_failures_after -= 1;
        } else if let Some(error) = state.contact_failures.pop_front() {
            return Err(error);
        }
        let offset = offset_of(cursor)?;
        let mut book = match state.contacts.get(account) {
            Some(book) => book.clone(),
            // The people the account talks to, under the names of their
            // chats.
            None => state
                .world
                .chats
                .iter()
                .filter(|chat| &chat.account_id == account && chat.kind == ChatKind::Direct)
                .map(|chat| {
                    let mut contact = Contact::new(
                        account.clone(),
                        client_provider::ContactId::new(chat.id.as_str()),
                    );
                    contact.phone = Some(chat.id.to_string());
                    contact.saved_name = Some(chat.title.clone());
                    contact.name = Some(chat.title.clone());
                    contact
                })
                .collect(),
        };
        book.sort_by_key(|contact| contact.display_name().to_lowercase());
        let items: Vec<Contact> = book
            .iter()
            .skip(offset)
            .take(CONTACT_PAGE)
            .cloned()
            .collect();
        let next = offset + items.len();
        Ok(Page {
            next_cursor: (next < book.len()).then(|| Cursor::new(next.to_string())),
            items,
        })
    }

    async fn check_numbers(
        &self,
        _account: &AccountId,
        phones: &[String],
    ) -> ProviderResult<Vec<NumberCheck>> {
        let mut checks = Vec::new();
        for typed in phones {
            let phone = normalize_phone(typed)?;
            let mut state = self.state();
            state.check_calls += 1;
            let on_whatsapp = !state.without_whatsapp.contains(&phone);
            checks.push(NumberCheck {
                chat: on_whatsapp.then(|| ChatId::new(phone.clone())),
                phone,
                on_whatsapp,
                business_name: None,
            });
        }
        Ok(checks)
    }

    async fn lookup_contact(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Contact> {
        self.mock_lookup_contact(account, contact)
    }

    async fn business_profile(
        &self,
        _account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Option<BusinessProfile>> {
        self.mock_business_profile(contact)
    }

    async fn list_blocked(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        self.mock_list_blocked(account)
    }

    async fn set_blocked(
        &self,
        account: &AccountId,
        contact: &ContactId,
        blocked: bool,
        _request_id: &str,
    ) -> ProviderResult<()> {
        self.mock_set_blocked(account, contact, blocked)
    }

    async fn own_profile(&self, account: &AccountId) -> ProviderResult<OwnProfile> {
        self.mock_own_profile(account)
    }

    async fn update_profile(
        &self,
        account: &AccountId,
        change: &ProfileChange,
    ) -> ProviderResult<()> {
        self.mock_update_profile(account, change)
    }

    async fn set_profile_picture(
        &self,
        account: &AccountId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        let subject = ChatId::new(
            self.account(account)
                .and_then(|known| known.phone)
                .unwrap_or_else(|| account.to_string()),
        );
        self.mock_set_picture("set_profile_picture", account, subject, false, jpeg)
    }

    async fn list_groups(&self, account: &AccountId) -> ProviderResult<Vec<Group>> {
        self.mock_list_groups(account)
    }

    async fn fetch_group(&self, account: &AccountId, group: &ChatId) -> ProviderResult<Group> {
        self.mock_fetch_group(account, group)
    }

    async fn create_group(&self, account: &AccountId, new: &NewGroup) -> ProviderResult<Group> {
        self.mock_create_group(account, new)
    }

    async fn update_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: &GroupChange,
    ) -> ProviderResult<()> {
        self.mock_update_group(account, group, change)
    }

    async fn change_participants(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: ParticipantChange,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        self.mock_change_participants(account, group, change, contacts, request_id)
    }

    async fn set_group_picture(
        &self,
        account: &AccountId,
        group: &ChatId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        self.mock_set_picture("set_group_picture", account, group.clone(), true, jpeg)
    }

    async fn group_invite_link(
        &self,
        account: &AccountId,
        group: &ChatId,
        reset: bool,
        _request_id: &str,
    ) -> ProviderResult<String> {
        self.mock_invite_link(account, group, reset)
    }

    async fn join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> ProviderResult<Vec<JoinRequest>> {
        self.mock_join_requests(account, group)
    }

    async fn answer_join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
        approve: bool,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        self.mock_answer_join_requests(account, group, approve, contacts, request_id)
    }

    async fn leave_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        _request_id: &str,
    ) -> ProviderResult<()> {
        self.mock_leave_group(account, group)
    }

    async fn link_places(&self) -> ProviderResult<Vec<LinkPlace>> {
        let place = |country: &str, country_name: &str, city: &str, city_name: &str| LinkPlace {
            country: country.into(),
            country_name: country_name.into(),
            city: city.into(),
            city_name: city_name.into(),
        };
        Ok(vec![
            place("CL", "Chile", "santiago", "Santiago"),
            place("CO", "Colombia", "bogotá", "Bogotá"),
            place("US", "United States", "miami", "Miami"),
            place("US", "United States", "newyorkcity", "New York City"),
            place("VE", "Venezuela", "caracas", "Caracas"),
            place("VE", "Venezuela", "maracaibo", "Maracaibo"),
        ])
    }

    async fn create_account(&self, new: &NewAccount) -> ProviderResult<LinkStatus> {
        let id = {
            let mut state = self.state();
            state.create_calls += 1;
            if let Some(error) = state.link_failures.pop_front() {
                return Err(error);
            }
            match state.created.get(&new.request_id) {
                // The same request again: the same number.
                Some(id) => id.clone(),
                None => {
                    let phone = match &new.pairing_phone {
                        Some(typed) => Some(normalize_phone(typed)?),
                        None => None,
                    };
                    let id = AccountId::new(format!("acc_new_{}", state.created.len() + 1));
                    state.world.accounts.push(Account {
                        id: id.clone(),
                        display_name: new
                            .name
                            .clone()
                            .filter(|name| !name.trim().is_empty())
                            .unwrap_or_else(|| "New number".to_owned()),
                        phone: None,
                        self_contact: None,
                        connection: ConnectionState::Connecting,
                        settings: AccountSettings {
                            history_import: Some(new.history),
                            server_media: Some(client_provider::ServerMedia::OnDemand),
                            ever_linked: Some(false),
                        },
                    });
                    state.links.insert(
                        id.clone(),
                        Link {
                            checks: 0,
                            phone,
                            was_linked: false,
                            done: false,
                            stopped: None,
                            codes: 0,
                            code_expired: false,
                            unreadable: false,
                        },
                    );
                    state.created.insert(new.request_id.clone(), id.clone());
                    id
                }
            }
        };
        self.link_step(&id, false)
    }

    async fn link_status(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        {
            let mut state = self.state();
            state.link_checks += 1;
            if let Some(error) = state.link_check_failures.pop_front() {
                return Err(error);
            }
        }
        self.link_step(account, true)
    }

    async fn pairing_code(&self, account: &AccountId, phone: &str) -> ProviderResult<LinkStatus> {
        {
            let mut state = self.state();
            state.pairing_calls += 1;
            if let Some(error) = state.pairing_failures.pop_front() {
                return Err(error);
            }
            let phone = normalize_phone(phone)?;
            if let Some(link) = state.links.get_mut(account) {
                // Asked again: another code, with its own time.
                if link.phone.is_some() {
                    link.codes += 1;
                }
                link.code_expired = false;
                link.phone = Some(phone);
                link.checks = link.checks.max(1);
            }
        }
        self.link_step(account, false)
    }

    async fn scan_instead(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        if let Some(link) = self.state().links.get_mut(account) {
            link.phone = None;
            link.code_expired = false;
        }
        self.link_step(account, false)
    }

    async fn update_account(
        &self,
        account: &AccountId,
        change: AccountChange,
    ) -> ProviderResult<Account> {
        let mut state = self.state();
        if let Some(error) = state.account_update_failures.pop_front() {
            return Err(error);
        }
        let Some(entry) = state.world.accounts.iter_mut().find(|a| &a.id == account) else {
            return Err(ProviderError::Rejected {
                code: "not_found".into(),
                message: "There is no such number.".into(),
            });
        };
        match change {
            AccountChange::Rename(name) if name.trim().is_empty() => {
                return Err(ProviderError::Rejected {
                    code: "invalid_name".into(),
                    message: "A name has between 1 and 100 characters.".into(),
                })
            }
            AccountChange::Rename(name) => entry.display_name = name.trim().to_owned(),
            AccountChange::History(history) => entry.settings.history_import = Some(history),
        }
        Ok(entry.clone())
    }

    async fn reconnect_account(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        {
            let mut state = self.state();
            let State { world, links, .. } = &mut *state;
            if let Some(entry) = world.accounts.iter_mut().find(|a| &a.id == account) {
                match entry.connection {
                    // No longer linked: it has to be linked again.
                    ConnectionState::LoggedOut => {
                        entry.connection = ConnectionState::Connecting;
                        links.insert(
                            account.clone(),
                            Link {
                                checks: 0,
                                phone: None,
                                was_linked: true,
                                done: false,
                                stopped: None,
                                codes: 0,
                                code_expired: false,
                                unreadable: false,
                            },
                        );
                    }
                    _ if links.contains_key(account) => {
                        if let Some(link) = links.get_mut(account) {
                            link.stopped = None;
                            link.checks = 0;
                        }
                        entry.connection = ConnectionState::Connecting;
                    }
                    _ => entry.connection = ConnectionState::Connected,
                }
            }
        }
        self.link_step(account, false)
    }

    async fn unlink_account(&self, account: &AccountId) -> ProviderResult<Account> {
        let mut state = self.state();
        state.links.remove(account);
        let Some(entry) = state.world.accounts.iter_mut().find(|a| &a.id == account) else {
            return Err(ProviderError::Rejected {
                code: "not_found".into(),
                message: "There is no such number.".into(),
            });
        };
        entry.connection = ConnectionState::LoggedOut;
        Ok(entry.clone())
    }

    async fn delete_account(&self, account: &AccountId) -> ProviderResult<()> {
        let mut state = self.state();
        state.links.remove(account);
        let before = state.world.accounts.len();
        state.world.accounts.retain(|a| &a.id != account);
        if state.world.accounts.len() < before {
            state.world.chats.retain(|c| &c.account_id != account);
            state.deleted.push(account.clone());
        }
        Ok(())
    }

    async fn list_stories(
        &self,
        account: &AccountId,
    ) -> ProviderResult<Vec<client_provider::Story>> {
        let answer = self.story_list(account);
        self.story_wait().await;
        answer
    }

    async fn post_story(
        &self,
        story: client_provider::NewStory,
    ) -> ProviderResult<client_provider::Story> {
        let answer = self.story_post(story);
        self.story_wait().await;
        answer
    }

    async fn delete_story(&self, account: &AccountId, story: &MessageId) -> ProviderResult<()> {
        let answer = self.story_delete(account, story);
        self.story_wait().await;
        answer
    }

    async fn view_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        author: &ContactId,
    ) -> ProviderResult<()> {
        self.story_view(account, story, author)
    }

    async fn story_viewers(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> ProviderResult<Vec<client_provider::StoryViewer>> {
        self.story_viewers_of(account, story)
    }

    async fn reply_to_story(
        &self,
        story: &client_provider::Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        self.story_answer(story, message, false).await
    }

    async fn react_to_story(
        &self,
        story: &client_provider::Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        self.story_answer(story, message, true).await
    }

    async fn muted_story_authors(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        self.story_muted_list(account)
    }

    async fn set_story_muted(
        &self,
        account: &AccountId,
        author: &ContactId,
        muted: bool,
    ) -> ProviderResult<()> {
        self.story_set_muted(account, author, muted)
    }

    async fn story_privacy(
        &self,
        account: &AccountId,
    ) -> ProviderResult<client_provider::StoryPrivacy> {
        self.story_privacy_of(account)
    }

    async fn set_story_privacy(
        &self,
        account: &AccountId,
        privacy: &client_provider::StoryPrivacy,
    ) -> ProviderResult<()> {
        self.story_set_privacy(account, privacy)
    }

    async fn subscribe(&self) -> ProviderResult<EventStream> {
        self.start_ambient();
        let receiver = self.inner.events.subscribe();
        let stream = futures::stream::unfold(receiver, |mut receiver| async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => return Some((event, receiver)),
                    // A slow consumer skipped events. They are hints, so
                    // carrying on is correct.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        });
        Ok(stream.boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outgoing(provider_chat: &Chat, client_id: &str, body: &str) -> OutgoingMessage {
        OutgoingMessage {
            client_id: ClientMessageId::new(client_id),
            account_id: provider_chat.account_id.clone(),
            chat_id: provider_chat.id.clone(),
            content: OutgoingContent::Text { body: body.into() },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: false,
        }
    }

    #[tokio::test]
    async fn seeds_a_realistic_world() {
        let mock = MockProvider::quiet();
        let accounts = mock.list_accounts().await.unwrap();
        assert_eq!(accounts.len(), 2);

        let mut chats = Vec::new();
        let mut cursor = None;
        loop {
            let page = mock.list_chats(&accounts[0].id, cursor).await.unwrap();
            chats.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert!(chats.len() >= 24, "a few dozen chats, got {}", chats.len());
        assert!(chats.iter().any(|c| c.kind == ChatKind::Group));
        assert!(chats.iter().any(|c| c.unread_count > 0));
        assert!(chats.iter().any(|c| c.pinned));
        // Most recently active first.
        let times: Vec<_> = chats
            .iter()
            .map(|c| c.last_message.as_ref().unwrap().timestamp)
            .collect();
        assert!(times.windows(2).all(|w| w[0] >= w[1]));
    }

    #[tokio::test]
    async fn history_pages_newest_first_without_gaps() {
        let mock = MockProvider::quiet();
        let account = mock.list_accounts().await.unwrap().remove(0).id;
        let chat = mock
            .list_chats(&account, None)
            .await
            .unwrap()
            .items
            .remove(0);

        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = mock
                .fetch_messages(&account, &chat.id, cursor, 100)
                .await
                .unwrap();
            assert!(page.items.len() <= 100);
            seen.extend(page.items.into_iter().map(|m| m.id));
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        assert!(seen.len() >= 640, "long thread, got {}", seen.len());
        let unique: std::collections::HashSet<_> = seen.iter().collect();
        assert_eq!(unique.len(), seen.len(), "no message returned twice");
    }

    #[tokio::test]
    async fn send_is_idempotent_on_the_client_id() {
        let mock = MockProvider::quiet();
        let account = mock.list_accounts().await.unwrap().remove(0).id;
        let chat = mock
            .list_chats(&account, None)
            .await
            .unwrap()
            .items
            .remove(0);
        let before = mock
            .fetch_messages(&account, &chat.id, None, 5)
            .await
            .unwrap()
            .items[0]
            .id
            .clone();

        let first = mock.send(outgoing(&chat, "c-1", "hello")).await.unwrap();
        let second = mock.send(outgoing(&chat, "c-1", "hello")).await.unwrap();
        assert_eq!(first, second);
        assert_eq!(mock.send_calls(), 2);
        assert_eq!(mock.delivered_count(), 1);

        let latest = mock
            .fetch_messages(&account, &chat.id, None, 5)
            .await
            .unwrap()
            .items;
        assert_eq!(latest[0].id, first.message_id);
        assert_eq!(latest[0].client_id, Some(ClientMessageId::new("c-1")));
        assert_eq!(latest[1].id, before, "exactly one message was appended");
    }

    #[tokio::test]
    async fn injected_failures_come_first() {
        let mock = MockProvider::quiet();
        let account = mock.list_accounts().await.unwrap().remove(0).id;
        let chat = mock
            .list_chats(&account, None)
            .await
            .unwrap()
            .items
            .remove(0);
        mock.fail_next_sends([ProviderError::Transient("eof".into())]);
        assert!(mock
            .send(outgoing(&chat, "c-2", "hi"))
            .await
            .unwrap_err()
            .is_transient());
        assert_eq!(mock.delivered_count(), 0);
        mock.send(outgoing(&chat, "c-2", "hi")).await.unwrap();
        assert_eq!(mock.delivered_count(), 1);
    }

    #[tokio::test]
    async fn live_events_follow_a_send() {
        let mock = MockProvider::new(MockConfig {
            simulate_replies: true,
            time_scale: 0.001,
            ..MockConfig::quiet()
        });
        let account = mock.list_accounts().await.unwrap().remove(0).id;
        let chat = mock
            .list_chats(&account, None)
            .await
            .unwrap()
            .items
            .remove(0);
        let mut events = mock.subscribe().await.unwrap();
        let receipt = mock.send(outgoing(&chat, "c-3", "ping")).await.unwrap();

        let mut statuses = Vec::new();
        let mut got_reply = false;
        while !got_reply {
            let event = tokio::time::timeout(Duration::from_secs(5), events.next())
                .await
                .expect("events keep coming")
                .expect("stream stays open");
            match event {
                ProviderEvent::MessageStatusChanged {
                    message_id, status, ..
                } if message_id == receipt.message_id => statuses.push(status),
                ProviderEvent::MessageUpserted(m) if m.direction == Direction::Incoming => {
                    got_reply = true;
                }
                _ => {}
            }
        }
        assert_eq!(
            statuses,
            vec![DeliveryStatus::Delivered, DeliveryStatus::Read]
        );
    }
}

#[cfg(test)]
mod picture_tests {
    use super::*;

    #[tokio::test]
    async fn the_demo_photos_are_real_images() {
        let mock = MockProvider::quiet();
        let account = mock.list_accounts().await.unwrap().remove(0).id;
        let data = mock
            .download_media(&account, &MediaRef::new("mock://image/42/320x180"))
            .await
            .unwrap();
        assert_eq!(data.mime_type.as_deref(), Some("image/png"));
        assert!(data.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
        // Width and height are where a PNG says they are.
        assert_eq!(&data.bytes[16..24], &[0, 0, 1, 64, 0, 0, 0, 180]);
        assert!(seed::picture("mock://image/1/99999x10").is_none());
        assert!(seed::picture("mock://media/1").is_none());
    }
}

/// A typed phone number as E.164, or the refusal for what cannot be one.
fn normalize_phone(typed: &str) -> ProviderResult<String> {
    let digits: String = typed.chars().filter(char::is_ascii_digit).collect();
    if !(7..=15).contains(&digits.len()) {
        return Err(ProviderError::Rejected {
            code: "invalid_phone".into(),
            message: "That does not look like a phone number.".into(),
        });
    }
    Ok(format!("+{digits}"))
}
