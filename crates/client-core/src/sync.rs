//! The sync engine: drives a [`Provider`] into the [`Store`].
//!
//! It owns every interaction with the network so that nothing else has to:
//!
//! * a **refresh** copies accounts and chat lists into the store and
//!   fetches the newest page of any chat with news;
//! * the **event loop** applies the provider's live updates, resubscribing
//!   with backoff when the stream ends;
//! * the **outbox worker** hands queued messages to the provider;
//! * **history** is paged in on demand as the user scrolls back.
//!
//! The UI calls the synchronous methods ([`send_text`](SyncEngine::send_text),
//! [`mark_read`](SyncEngine::mark_read), [`open_chat`](SyncEngine::open_chat)),
//! which only write to the store or schedule background work, and returns
//! immediately. It learns about results through the store's
//! [`StoreChange`] notifications.

use crate::outbox::OutboxConfig;
use crate::store::{HistoryState, Store, StoreChange, StoreError, Upsert};
use client_provider::MediaKind;
use client_provider::{
    Account, AccountChange, AccountId, Capabilities, Chat, ChatChange, ChatId, ClientMessageId,
    Contact, ContactId, Direction, Feature, HistoryImport, LinkPlace, LinkStatus, LinkStep,
    Message, MessageContent, MessageId, NewAccount, NumberCheck, OutgoingContent, OutgoingMessage,
    PresenceState, Provider, ProviderError, ProviderEvent, Timestamp,
};
use futures::StreamExt;
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

mod actions;
pub use actions::{ForwardError, ForwardRefusal};
mod library;
mod library_index;
pub use library::{LibraryError, LibrarySend, LIBRARY_BUDGET, RECENT_LIMIT};
pub use library_index::HEARD_LIMIT;
mod social;
mod stories;
pub use social::{failure_sentence, outcome_sentence, own_picture_subject, CreatedGroup};
pub use stories::{
    ReceiptPass, StoryListing, StoryPass, StoryPostError, StoryReplyError, STORIES_FRESH,
    STORIES_OPENED,
};

/// How far back a refresh looks for messages of the account's own whose
/// ticks may still move.
const RECEIPTS_WINDOW: Duration = Duration::from_secs(6 * 60 * 60);
/// How many chats per account a refresh reads again for their ticks.
const RECEIPTS_CHATS: usize = 20;
/// The longest the event loop obeys a `Retry-After` on a failed subscribe:
/// a server's number is a request, not a licence to stall the engine.
const RETRY_AFTER_MAX: Duration = Duration::from_secs(5 * 60);

/// Why a sync operation failed.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The provider failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The local store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl SyncError {
    /// True when trying again later can succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Provider(error) if error.is_transient())
    }
}

/// How much history the engine fetches without being asked.
///
/// The chat list never depends on it: chats come with their last message,
/// so the list is complete after the accounts and the chat pages, whatever
/// the mode. A chat that is opened always loads, at once, on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryMode {
    /// Nothing ahead of time: a chat's history is fetched when it is
    /// opened, and further back as the user scrolls.
    OnOpen,
    /// The newest page of this many of each account's most recent chats.
    Recent(usize),
    /// Every page of every chat.
    Everything,
}

/// Tuning of the engine.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    /// Deadline of every provider call other than sends. A call that
    /// overruns counts as a transient failure.
    pub call_timeout: Duration,
    /// Messages per history page.
    pub page_size: u32,
    /// Shortest wait before resubscribing or refreshing after a failure.
    pub retry_base: Duration,
    /// Longest such wait.
    pub retry_max: Duration,
    /// How long a typing indicator stays up without being refreshed.
    pub presence_ttl: Duration,
    /// How much history to fetch in the background.
    pub history: HistoryMode,
    /// How many history requests may be in flight at once while
    /// preloading.
    pub preload_concurrency: usize,
    /// The least time between the starts of two preload requests. With the
    /// default (250 ms) preloading stays at or under 240 requests a minute,
    /// well below wuapi's 600 a minute per key, leaving the rest for the
    /// chat the user is looking at.
    pub preload_pace: Duration,
    /// How many bytes of media (thumbnails and downloaded files) the cache
    /// may hold. Beyond it, what was used longest ago is dropped.
    pub media_budget: u64,
    /// How many times a change to a chat's state (pin, mute, archive) is
    /// offered to the provider before the engine gives up on it.
    pub chat_change_attempts: u32,
    /// Outbox tuning.
    pub outbox: OutboxConfig,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            call_timeout: Duration::from_secs(30),
            page_size: 50,
            retry_base: Duration::from_secs(1),
            retry_max: Duration::from_secs(60),
            presence_ttl: Duration::from_secs(12),
            history: HistoryMode::OnOpen,
            preload_concurrency: 4,
            preload_pace: Duration::from_millis(250),
            media_budget: 500 * 1024 * 1024,
            chat_change_attempts: 8,
            outbox: OutboxConfig::default(),
        }
    }
}

struct PresenceEntry {
    contact: ContactId,
    state: PresenceState,
    at: Instant,
}

struct Inner {
    store: Arc<Store>,
    provider: Arc<dyn Provider>,
    capabilities: Capabilities,
    config: SyncConfig,
    runtime: Handle,
    /// Wakes the outbox worker: something was queued or an account is back.
    outbox_wake: Notify,
    /// When the last message was queued, in milliseconds: messages queued
    /// together keep the order they were queued in.
    last_queued: std::sync::atomic::AtomicI64,
    presence: Mutex<HashMap<(AccountId, ChatId), PresenceEntry>>,
    /// Chats whose history is being fetched right now, to avoid doing it
    /// twice when the user scrolls impatiently.
    fetching: Mutex<HashSet<(AccountId, ChatId)>>,
    /// The newest change asked for per chat and kind of change. A retry
    /// that is no longer the newest stops: the user changed their mind.
    chat_changes: Mutex<HashMap<(AccountId, ChatId, u8), u64>>,
    /// What the user set and the provider has not been seen to agree with
    /// yet. Until it does (or refuses, or enough time passes after it
    /// accepted), these win over what the provider's chat list says.
    pending_state: Mutex<HashMap<(AccountId, ChatId, u8), PendingState>>,
    /// The newest vote asked for per poll. A retry or an undo that is no
    /// longer the newest stops: the user voted again.
    poll_votes: Mutex<HashMap<(AccountId, MessageId), u64>>,
    /// The newest edit, deletion or star asked for per message (see
    /// `sync/actions.rs`).
    message_ops: Mutex<HashMap<(AccountId, MessageId, u8), u64>>,
    history: Mutex<HistoryMode>,
    /// Chats the store had history of that now have a newer last message:
    /// their newest page is fetched in the background, whatever the mode,
    /// so a conversation that was read before has no hole in it.
    gaps: Mutex<Vec<(AccountId, ChatId)>>,
    /// Wakes the preloader: a refresh finished, an account is back, the
    /// mode changed.
    preload_wake: Notify,
    /// When the next preload request may start.
    next_preload: Mutex<Option<tokio::time::Instant>>,
    /// `Some(reason)` once the provider has refused the credentials. From
    /// then on the engine asks nothing more.
    auth_lost: tokio::sync::watch::Sender<Option<String>>,
    /// When each chat's picture is next worth asking about.
    avatar_due: Mutex<HashMap<(AccountId, ChatId), Instant>>,
    /// The picture id each chat's listing last carried, and the one the
    /// provider was last asked about: a picture is asked for again as
    /// soon as its id changes, and not twice for the same id.
    picture_hints: Mutex<HashMap<(AccountId, ChatId), PictureHint>>,
    /// Where copying each account's address book stands.
    contact_sync: Mutex<HashMap<AccountId, ContactSync>>,
    /// The files being uploaded, and how far each is.
    uploads: Mutex<HashMap<ClientMessageId, Upload>>,
    upload_slots: tokio::sync::Semaphore,
    /// Whether the provider can take files right now, and when it said.
    upload_ready: Mutex<Option<(bool, Instant)>>,
    upload_checking: Mutex<bool>,
    /// The chats whose unread count the provider does not know: the
    /// engine counts for those.
    uncounted: Mutex<HashSet<(AccountId, ChatId)>>,
    /// Tell the other side when their messages have been read.
    read_receipts: std::sync::atomic::AtomicBool,
    contacts_wake: Notify,
    /// Profiles and groups: what is fresh, and what is on its way.
    social: social::Social,
    /// The sticker and GIF library: which accounts' favorites are being
    /// synced, and when each is next due.
    library: library::LibrarySync,
    /// Stories: what is listed, owed and on its way out.
    stories: stories::Stories,
    /// How many picture requests may be in flight.
    avatar_slots: tokio::sync::Semaphore,
    media: Mutex<MediaQueue>,
    /// Bytes received so far of each download under way, and when the
    /// view was last told.
    media_progress: Mutex<HashMap<String, (u64, Option<Instant>)>>,
    media_cancel: Notify,
    /// Set by `shutdown`: nothing new is started.
    stopped: std::sync::atomic::AtomicBool,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

/// The engine. Cheap to clone; clones share the same state.
#[derive(Clone)]
pub struct SyncEngine {
    inner: Arc<Inner>,
}

impl SyncEngine {
    /// Creates an engine without starting any background work. Tests drive
    /// it step by step; applications call [`start`](Self::start).
    ///
    /// `runtime` is the Tokio runtime the engine's background work runs on.
    pub fn new(
        store: Arc<Store>,
        provider: Arc<dyn Provider>,
        config: SyncConfig,
        runtime: Handle,
    ) -> Self {
        let config_history = config.history;
        let slots = config.preload_concurrency.max(1);
        Self {
            inner: Arc::new(Inner {
                capabilities: provider.capabilities(),
                store,
                provider,
                config,
                runtime,
                outbox_wake: Notify::new(),
                last_queued: std::sync::atomic::AtomicI64::new(0),
                presence: Mutex::new(HashMap::new()),
                fetching: Mutex::new(HashSet::new()),
                chat_changes: Mutex::new(HashMap::new()),
                pending_state: Mutex::new(HashMap::new()),
                poll_votes: Mutex::new(HashMap::new()),
                message_ops: Mutex::new(HashMap::new()),
                history: Mutex::new(config_history),
                gaps: Mutex::new(Vec::new()),
                preload_wake: Notify::new(),
                next_preload: Mutex::new(None),
                auth_lost: tokio::sync::watch::channel(None).0,
                avatar_due: Mutex::new(HashMap::new()),
                picture_hints: Mutex::new(HashMap::new()),
                contact_sync: Mutex::new(HashMap::new()),
                uploads: Mutex::new(HashMap::new()),
                upload_slots: tokio::sync::Semaphore::new(2),
                upload_ready: Mutex::new(None),
                upload_checking: Mutex::new(false),
                uncounted: Mutex::new(HashSet::new()),
                read_receipts: std::sync::atomic::AtomicBool::new(true),
                contacts_wake: Notify::new(),
                social: social::Social::default(),
                library: library::LibrarySync::default(),
                stories: stories::Stories::default(),
                avatar_slots: tokio::sync::Semaphore::new(slots),
                media: Mutex::new(MediaQueue::default()),
                media_progress: Mutex::new(HashMap::new()),
                media_cancel: Notify::new(),
                stopped: std::sync::atomic::AtomicBool::new(false),
                tasks: Mutex::new(Vec::new()),
            }),
        }
    }

    /// Starts the background work: the outbox worker and the loop that
    /// refreshes the store and follows the provider's events.
    pub fn start(&self) {
        if let Err(error) = self.inner.store.outbox_recover() {
            tracing::error!(%error, "could not recover the outbox");
        }
        let outbox = self.clone();
        let events = self.clone();
        let preload = self.clone();
        let contacts = self.clone();
        let stickers = self.clone();
        let stories = self.clone();
        let mut tasks = self.inner.tasks.lock().expect("tasks lock");
        tasks.push(
            self.inner
                .runtime
                .spawn(async move { contacts.run_contacts().await }),
        );
        tasks.push(
            self.inner
                .runtime
                .spawn(async move { stories.run_stories().await }),
        );
        tasks.push(
            self.inner
                .runtime
                .spawn(async move { preload.run_preload().await }),
        );
        tasks.push(
            self.inner
                .runtime
                .spawn(async move { stickers.run_library_index().await }),
        );
        tasks.push(
            self.inner
                .runtime
                .spawn(async move { outbox.run_outbox().await }),
        );
        tasks.push(
            self.inner
                .runtime
                .spawn(async move { events.run_events().await }),
        );
    }

    /// Stops the background work. Queued messages stay in the store and go
    /// out the next time an engine starts.
    pub fn shutdown(&self) {
        self.inner
            .stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.cancel_media();
        for task in self.inner.tasks.lock().expect("tasks lock").drain(..) {
            task.abort();
        }
    }

    /// The id of the provider this engine runs on.
    pub fn provider_id(&self) -> &'static str {
        self.inner.provider.id()
    }

    /// How the provider's live updates reach the client right now, when it
    /// says. For the diagnostics; nothing else in the engine depends on it.
    pub fn live_updates(&self) -> Option<client_provider::LiveUpdates> {
        self.inner.provider.live_updates()
    }

    /// The store this engine writes to.
    pub fn store(&self) -> &Arc<Store> {
        &self.inner.store
    }

    /// What the provider supports.
    pub fn capabilities(&self) -> Capabilities {
        self.inner.capabilities
    }

    /// Whether a part the capabilities promise is missing for now for
    /// this account: the provider's backend does not have it yet, or has
    /// not turned it on for the number. The place that offers it then says
    /// "not available yet" instead; what works is not touched, and the
    /// provider asks again by itself later. Cheap: asked when drawing.
    pub fn feature_unavailable(&self, account: &AccountId, feature: Feature) -> bool {
        self.inner.provider.unavailable(account, feature)
    }

    /// What the provider can do for `account` right now: its
    /// capabilities, less the parts that are missing for now. What a view
    /// of one number asks, so that it offers nothing that would answer
    /// "not available yet".
    pub fn capabilities_for(&self, account: &AccountId) -> Capabilities {
        let mut capabilities = self.inner.capabilities;
        let missing = |feature| self.feature_unavailable(account, feature);
        if missing(Feature::ForwardAny) {
            capabilities.forward_any = false;
        }
        if missing(Feature::StickerFavorites) {
            capabilities.sticker_favorites = false;
        }
        if missing(Feature::Stories) {
            capabilities.story_contacts = false;
            capabilities.story_view = false;
            capabilities.story_viewers = false;
            capabilities.story_reply = false;
            capabilities.story_react = false;
        }
        capabilities
    }

    /// Follows whether the provider still accepts the credentials. The
    /// value becomes `Some(reason)` when it answers that it does not (a
    /// key revoked or deleted); the engine has then stopped for good and
    /// the application should ask the user to sign in. A network failure,
    /// a timeout or a 5xx never ends up here: those are retried.
    pub fn auth_lost(&self) -> tokio::sync::watch::Receiver<Option<String>> {
        self.inner.auth_lost.subscribe()
    }

    /// True once the provider has refused the credentials.
    pub fn is_auth_lost(&self) -> bool {
        self.inner.auth_lost.borrow().is_some()
    }

    /// Records a refusal of the credentials, if `error` is one. Returns
    /// whether it was.
    fn note_unauthorized(&self, error: &ProviderError) -> bool {
        match error {
            ProviderError::Unauthorized(reason) => {
                tracing::warn!(%reason, "the provider refused the credentials; sync stops");
                self.inner.auth_lost.send_replace(Some(reason.clone()));
                true
            }
            _ => false,
        }
    }

    /// Changes how much history is fetched in the background, from now on.
    pub fn set_history(&self, mode: HistoryMode) {
        *self.inner.history.lock().expect("history lock") = mode;
        self.inner.preload_wake.notify_one();
    }

    /// How much history is fetched in the background.
    pub fn history(&self) -> HistoryMode {
        *self.inner.history.lock().expect("history lock")
    }

    /// The Tokio runtime the engine works on, for callers that want to
    /// await one of its `async` steps off their own thread.
    pub fn runtime(&self) -> &Handle {
        &self.inner.runtime
    }

    // ----- called by the UI: never blocks on the network ---------------

    /// Queues a text message and returns its client id. The message shows
    /// up in the store immediately, pending, and is sent in the background.
    pub fn send_text(
        &self,
        account: &AccountId,
        chat: &ChatId,
        body: impl Into<String>,
        reply_to: Option<MessageId>,
    ) -> Result<ClientMessageId, StoreError> {
        self.send_text_mentioning(account, chat, body, reply_to, Vec::new())
    }

    /// What follows the `@` where a text sent from here mentions
    /// `contact` (the provider's way of writing it).
    pub fn mention_handle(&self, contact: &ContactId) -> String {
        self.inner.provider.mention_handle(contact)
    }

    /// The mentions of people, as a message carries them.
    fn mentions_of(&self, people: Vec<ContactId>) -> Vec<client_provider::Mention> {
        people
            .into_iter()
            .map(|id| client_provider::Mention {
                handle: self.mention_handle(&id),
                id,
                name: None,
                me: false,
            })
            .collect()
    }

    /// [`send_text`](Self::send_text) with the people the text mentions:
    /// `body` names each of them as `@` and
    /// [`mention_handle`](Self::mention_handle) of their id.
    pub fn send_text_mentioning(
        &self,
        account: &AccountId,
        chat: &ChatId,
        body: impl Into<String>,
        reply_to: Option<MessageId>,
        mentions: Vec<ContactId>,
    ) -> Result<ClientMessageId, StoreError> {
        let mentions = self.mentions_of(mentions);
        self.send(OutgoingMessage {
            client_id: new_client_id(),
            account_id: account.clone(),
            chat_id: chat.clone(),
            content: OutgoingContent::Text { body: body.into() },
            reply_to,
            mentions,
            forwarded: false,
        })
    }

    /// The time to queue a message at: now, and never the same
    /// millisecond as another message queued here (the outbox orders a
    /// chat's messages by it), so a batch queued in a loop goes out in
    /// the order it was queued.
    pub(crate) fn queue_time(&self) -> Timestamp {
        use std::sync::atomic::Ordering;
        let now = Timestamp::now().as_millis();
        let mut last = self.inner.last_queued.load(Ordering::SeqCst);
        loop {
            let next = now.max(last + 1);
            match self.inner.last_queued.compare_exchange(
                last,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return Timestamp::from_millis(next),
                Err(seen) => last = seen,
            }
        }
    }

    /// Queues any outgoing message. See [`send_text`](Self::send_text).
    pub fn send(&self, message: OutgoingMessage) -> Result<ClientMessageId, StoreError> {
        self.inner.store.enqueue(
            &message,
            self.queue_time(),
            self.inner.config.outbox.max_age,
        )?;
        self.inner.outbox_wake.notify_one();
        Ok(message.client_id)
    }

    /// Whether read receipts are sent when a chat is read.
    pub fn read_receipts(&self) -> bool {
        self.inner
            .read_receipts
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Turns read receipts on or off, from now on.
    pub fn set_read_receipts(&self, send: bool) {
        self.inner
            .read_receipts
            .store(send, std::sync::atomic::Ordering::Relaxed);
    }

    /// Clears a chat's unread count locally and tells the provider in the
    /// background.
    pub fn mark_read(&self, account: &AccountId, chat: &ChatId) {
        if let Err(error) = self.inner.store.mark_chat_read(account, chat) {
            tracing::error!(%error, "could not mark the chat read");
        }
        let this = self.clone();
        let (account, chat) = (account.clone(), chat.clone());
        // Without read receipts the provider is still told, so that the
        // badge goes on the account's other devices: quietly. A provider
        // that can only do both is asked for both.
        let quietly = !self.read_receipts() && self.inner.capabilities.quiet_read;
        self.inner.runtime.spawn(async move {
            let provider = &this.inner.provider;
            let call = async {
                if quietly {
                    provider.mark_read_quietly(&account, &chat).await
                } else {
                    provider.mark_read(&account, &chat, None).await
                }
            };
            // Best effort: the next refresh brings the provider's unread
            // count back if this did not get through.
            if let Err(error) = this.bounded(call).await {
                this.note_unauthorized(&error);
                tracing::debug!(%error, "mark_read did not reach the provider");
            }
        });
    }

    /// Pins, mutes, archives or marks a chat as unread. The store shows
    /// the new state at once; the provider is told in the background.
    ///
    /// Changes set a value, so they are safe to repeat: a transient
    /// failure is retried with backoff, quietly. Only a refusal by the
    /// provider (or running out of attempts) puts the old state back. The
    /// retries live in memory: after a restart, the next refresh shows
    /// whatever the provider has.
    pub fn update_chat(&self, account: &AccountId, chat: &ChatId, change: ChatChange) {
        let inner = &self.inner;
        if let Err(error) = inner.store.apply_chat_change(account, chat, change) {
            tracing::error!(%error, "could not store the chat change");
            return;
        }
        let key = (account.clone(), chat.clone(), change_kind(change));
        if let Some(value) = value_of(change) {
            let mut pending = inner.pending_state.lock().expect("pending lock");
            pending.insert(
                key.clone(),
                PendingState {
                    value,
                    accepted_at: None,
                },
            );
        }
        let generation = {
            let mut changes = inner.chat_changes.lock().expect("chat changes lock");
            let generation = changes.get(&key).copied().unwrap_or(0) + 1;
            changes.insert(key.clone(), generation);
            generation
        };
        let this = self.clone();
        inner.runtime.spawn(async move {
            let (account, chat, _) = &key;
            let is_newest = |this: &SyncEngine| {
                let changes = this.inner.chat_changes.lock().expect("chat changes lock");
                changes.get(&key) == Some(&generation)
            };
            let mut attempt = 0;
            let failure = loop {
                attempt += 1;
                let call = this.inner.provider.update_chat(account, chat, change);
                match this.bounded(call).await {
                    Ok(()) => {
                        // Accepted. The provider's lists may lag behind:
                        // the user's value stands until they catch up.
                        let mut pending = this.inner.pending_state.lock().expect("pending lock");
                        if let Some(state) = pending.get_mut(&key) {
                            state.accepted_at = Some(Instant::now());
                        }
                        return;
                    }
                    Err(error)
                        if error.is_transient()
                            && attempt < this.inner.config.chat_change_attempts =>
                    {
                        tracing::debug!(%error, "chat change: will be retried");
                        let wait = error.retry_after().unwrap_or(this.backoff(attempt));
                        tokio::time::sleep(wait).await;
                        if !is_newest(&this) {
                            return;
                        }
                    }
                    Err(error) => break error,
                }
            };
            if this.note_unauthorized(&failure) {
                // Not refused: not asked. The change stays for after the
                // next sign-in's refresh to confirm or correct.
                return;
            }
            tracing::warn!(error = %failure, "the chat change did not reach the provider");
            if is_newest(&this) {
                this.inner
                    .pending_state
                    .lock()
                    .expect("pending lock")
                    .remove(&key);
                if let Some(undo) = undo_of(change) {
                    if let Err(error) = this.inner.store.apply_chat_change(account, chat, undo) {
                        tracing::error!(%error, "could not undo the chat change");
                    }
                }
                // Put back, and said: a change that silently undoes itself
                // looks like a button that does not work.
                let what = match change {
                    ChatChange::Pinned(true) => "pinned",
                    ChatChange::Pinned(false) => "unpinned",
                    ChatChange::Muted(true) | ChatChange::MutedFor(_) => "muted",
                    ChatChange::Muted(false) => "unmuted",
                    ChatChange::Archived(true) => "archived",
                    ChatChange::Archived(false) => "unarchived",
                    ChatChange::MarkedUnread => "marked as unread",
                };
                let why = if failure.is_transient() {
                    "the connection kept failing".to_owned()
                } else {
                    failure.to_string()
                };
                this.inner.store.notify(StoreChange::Problem {
                    message: format!("The chat could not be {what}: {why}"),
                });
            }
        });
    }

    /// Casts the account's vote in a poll: `choices` are all the options
    /// it now stands for (none takes the vote back). The store shows the
    /// vote and the moved tally at once; the provider is told in the
    /// background.
    ///
    /// A vote sets a value, so it is safe to repeat: a transient failure
    /// is retried with backoff, quietly. Only a refusal by the provider
    /// (or running out of attempts) takes the vote back off the screen,
    /// and says so.
    pub fn vote_poll(
        &self,
        account: &AccountId,
        chat: &ChatId,
        poll: &MessageId,
        choices: Vec<String>,
    ) {
        let inner = &self.inner;
        let current = match inner.store.message(account, poll) {
            Ok(Some(Message {
                content: MessageContent::Poll(current),
                ..
            })) => current,
            Ok(_) => return,
            Err(error) => {
                tracing::error!(%error, "could not read the poll");
                return;
            }
        };
        let before = current.chosen.clone().unwrap_or_default();
        let voted = current.with_vote(&choices);
        let choices = voted.chosen.clone().unwrap_or_default();
        if let Err(error) = inner.store.set_poll(account, poll, &voted) {
            tracing::error!(%error, "could not store the vote");
            return;
        }
        let key = (account.clone(), poll.clone());
        let generation = {
            let mut votes = inner.poll_votes.lock().expect("poll votes lock");
            let generation = votes.get(&key).copied().unwrap_or(0) + 1;
            votes.insert(key.clone(), generation);
            generation
        };
        let this = self.clone();
        let chat = chat.clone();
        inner.runtime.spawn(async move {
            let (account, poll) = &key;
            let is_newest = |this: &SyncEngine| {
                let votes = this.inner.poll_votes.lock().expect("poll votes lock");
                votes.get(&key) == Some(&generation)
            };
            let mut attempt = 0;
            let failure = loop {
                attempt += 1;
                let call = this
                    .inner
                    .provider
                    .vote_poll(account, &chat, poll, &choices);
                match this.bounded(call).await {
                    Ok(answer) => {
                        // The provider's tally, when it sent one and no
                        // newer vote is on its way. The vote itself stays
                        // the one stored: a provider that cannot say how
                        // the account voted does not wipe it.
                        if let (Some(message), true) = (answer, is_newest(&this)) {
                            if let Err(error) = this.inner.store.upsert_message(&message) {
                                tracing::error!(%error, "could not store the poll's tally");
                            }
                        }
                        return;
                    }
                    Err(error)
                        if error.is_transient()
                            && attempt < this.inner.config.chat_change_attempts =>
                    {
                        tracing::debug!(%error, "vote: will be retried");
                        let wait = error.retry_after().unwrap_or(this.backoff(attempt));
                        tokio::time::sleep(wait).await;
                        if !is_newest(&this) {
                            return;
                        }
                    }
                    Err(error) => break error,
                }
            };
            if this.note_unauthorized(&failure) {
                return;
            }
            tracing::warn!(error = %failure, "the vote did not reach the provider");
            if !is_newest(&this) {
                return;
            }
            // Undone on the tally as it stands now, which may have moved
            // meanwhile: the previous vote is put back, nothing else.
            if let Ok(Some(Message {
                content: MessageContent::Poll(now),
                ..
            })) = this.inner.store.message(account, poll)
            {
                if let Err(error) =
                    this.inner
                        .store
                        .set_poll(account, poll, &now.with_vote(&before))
                {
                    tracing::error!(%error, "could not undo the vote");
                }
            }
            let why = if failure.is_transient() {
                "the connection kept failing".to_owned()
            } else {
                failure.to_string()
            };
            this.inner.store.notify(StoreChange::Problem {
                message: format!("Your vote was not recorded: {why}"),
            });
        });
    }

    /// A chat as the provider reports it, with what the user has set and
    /// the provider has not caught up with put back in.
    ///
    /// A pending value is dropped when the provider's report agrees with
    /// it (confirmed), and [`PENDING_GRACE`] after the provider accepted
    /// the change (from then on its word is the truth again, so a change
    /// made on the phone later is not masked for ever).
    fn with_pending_state(&self, mut chat: Chat) -> Chat {
        let mut pending = self.inner.pending_state.lock().expect("pending lock");
        if pending.is_empty() {
            return chat;
        }
        let mut settle = |kind: u8, value: &mut bool, unknown: &mut bool| {
            let key = (chat.account_id.clone(), chat.id.clone(), kind);
            let Some(state) = pending.get(&key).copied() else {
                return;
            };
            let confirmed = !*unknown && *value == state.value;
            let lapsed = state
                .accepted_at
                .is_some_and(|at| at.elapsed() >= PENDING_GRACE);
            if confirmed || (lapsed && !*unknown) {
                pending.remove(&key);
            } else {
                *value = state.value;
                *unknown = false;
            }
        };
        settle(0, &mut chat.pinned, &mut chat.unknown.pinned);
        settle(1, &mut chat.muted, &mut chat.unknown.muted);
        settle(2, &mut chat.archived, &mut chat.unknown.archived);
        chat
    }

    /// The chat with a phone number, created in the store if it is new.
    /// Asks the provider (which normalises the number), so await it off
    /// the UI thread.
    pub async fn start_chat(&self, account: &AccountId, phone: &str) -> Result<ChatId, SyncError> {
        let inner = &self.inner;
        let chat = self
            .bounded(inner.provider.start_chat(account, phone))
            .await?;
        // A chat the store already has keeps its name, pin and unread count.
        if inner.store.chat(account, &chat.id)?.is_none() {
            inner.store.upsert_chat(&chat, true)?;
        }
        Ok(chat.id)
    }

    // ----- contacts -----------------------------------------------------

    /// Copies an account's address book into the store: a page at a time,
    /// paced like history preloading, at most [`CONTACT_PAGES`] pages.
    ///
    /// A failure leaves the pass where it is: the next call goes on from
    /// the page that failed, so a dropped connection costs one page, not
    /// the book. When the last page is in, the contacts the pass did not
    /// meet (deleted on the phone) are removed. Returns how many contacts
    /// this call stored.
    pub async fn sync_contacts(&self, account: &AccountId) -> Result<usize, SyncError> {
        let inner = &self.inner;
        if !inner.capabilities.contacts {
            return Ok(0);
        }
        let (mut cursor, pass, mut pages) = {
            let mut all = inner.contact_sync.lock().expect("contacts lock");
            let state = all.entry(account.clone()).or_default();
            if state.running {
                // A pass is under way: its pages are the same pages.
                return Ok(0);
            }
            state.running = true;
            // Each pass is marked later than the one before, even when
            // the clock has not moved: its mark is what tells the rows
            // it met from those it did not.
            let pass = match state.pass {
                Some(pass) => pass,
                None => {
                    let mark = Timestamp::now().as_millis().max(state.last_pass + 1);
                    state.last_pass = mark;
                    *state.pass.insert(Timestamp::from_millis(mark))
                }
            };
            (state.cursor.clone(), pass, state.pages)
        };
        let mut stored = 0;
        let outcome = loop {
            if pages >= CONTACT_PAGES {
                tracing::warn!("the address book is longer than this client reads");
                break Ok(true);
            }
            let page = self
                .paced(|| async {
                    let call = inner.provider.list_contacts(account, cursor.clone());
                    self.bounded(call).await.map_err(SyncError::from)
                })
                .await;
            let page = match page {
                Ok(page) => page,
                Err(error) => break Err(error),
            };
            if let Err(error) = inner
                .store
                .upsert_listed_contacts(account, &page.items, pass)
            {
                break Err(error.into());
            }
            stored += page.items.len();
            pages += 1;
            cursor = page.next_cursor;
            {
                // Remembered page by page: what interrupts the pass does
                // not send it back to the start.
                let mut all = inner.contact_sync.lock().expect("contacts lock");
                let state = all.entry(account.clone()).or_default();
                state.cursor = cursor.clone();
                state.pages = pages;
            }
            if cursor.is_none() {
                break Ok(false);
            }
        };
        let mut all = inner.contact_sync.lock().expect("contacts lock");
        let state = all.entry(account.clone()).or_default();
        state.running = false;
        match outcome {
            Ok(cut_short) => {
                state.cursor = None;
                state.pass = None;
                state.pages = 0;
                state.finished = Some(Instant::now());
                drop(all);
                if !cut_short {
                    inner.store.prune_contacts(account, pass)?;
                }
                Ok(stored)
            }
            Err(error) => {
                if let SyncError::Provider(provider) = &error {
                    if !self.note_unauthorized(provider) && !provider.is_transient() {
                        // The provider refused this page for good (a
                        // cursor it no longer knows): from the top next
                        // time, in a new pass.
                        state.cursor = None;
                        state.pass = None;
                        state.pages = 0;
                    }
                }
                Err(error)
            }
        }
    }

    /// Says the address book of `account` is about to be shown, or may
    /// have changed: it is copied again if the last copy is older than
    /// `within`. Cheap; the work happens in the background.
    pub fn want_contacts(&self, account: &AccountId, within: Duration) {
        {
            let mut all = self.inner.contact_sync.lock().expect("contacts lock");
            let state = all.entry(account.clone()).or_default();
            state.within = Some(state.within.map_or(within, |known| known.min(within)));
        }
        self.inner.contacts_wake.notify_one();
    }

    /// Whether the address book of `account` has been copied at least
    /// once since this engine started.
    pub fn contacts_synced(&self, account: &AccountId) -> bool {
        self.inner
            .contact_sync
            .lock()
            .expect("contacts lock")
            .get(account)
            .is_some_and(|state| state.finished.is_some())
    }

    /// Whether a phone number has WhatsApp. Asks the provider: await it
    /// off the UI thread.
    pub async fn check_number(
        &self,
        account: &AccountId,
        phone: &str,
    ) -> Result<NumberCheck, SyncError> {
        let phones = [phone.to_owned()];
        let checked = self
            .bounded(self.inner.provider.check_numbers(account, &phones))
            .await;
        let checked = self.noted(checked)?;
        checked.into_iter().next().ok_or_else(|| {
            SyncError::Provider(ProviderError::Protocol(
                "the provider did not answer for the number".into(),
            ))
        })
    }

    /// The chat with a contact of the address book, created in the store
    /// if the account has none with them yet. Asks nobody: the contact's
    /// id is its chat's id.
    pub fn chat_with_contact(&self, contact: &Contact) -> Result<ChatId, StoreError> {
        let chat_id = ChatId::new(contact.id.as_str());
        let store = &self.inner.store;
        if store.chat(&contact.account_id, &chat_id)?.is_none() {
            store.upsert_chat(
                &Chat {
                    id: chat_id.clone(),
                    account_id: contact.account_id.clone(),
                    kind: client_provider::ChatKind::Direct,
                    title: contact.display_name(),
                    avatar: contact.avatar.clone(),
                    unread_count: 0,
                    pinned: false,
                    muted: false,
                    archived: false,
                    last_message: None,
                    // Nothing is known of its state: the provider's list
                    // says, once the chat exists there.
                    unknown: client_provider::ChatUnknown {
                        pinned: true,
                        muted: true,
                        archived: true,
                        picture: contact.picture_id.is_none(),
                        unread: false,
                    },
                    picture_id: contact.picture_id.clone(),
                    pinned_at: None,
                },
                true,
            )?;
        }
        Ok(chat_id)
    }

    /// The chat with a number someone typed, after asking whether the
    /// number has WhatsApp (when the provider can tell). When it cannot
    /// be asked just now the chat opens all the same: a message to a
    /// number without WhatsApp fails when it is sent, and says so.
    pub async fn start_chat_checked(
        &self,
        account: &AccountId,
        phone: &str,
    ) -> Result<NewChat, SyncError> {
        if self.inner.capabilities.number_check {
            match self.check_number(account, phone).await {
                Ok(check) if !check.on_whatsapp => return Ok(NewChat::NotOnWhatsApp(check.phone)),
                Ok(_) => {}
                Err(error) if error.is_transient() => {}
                Err(error) => return Err(error),
            }
        }
        self.start_chat(account, phone).await.map(NewChat::Open)
    }

    /// Keeps the address books current: once at start, every
    /// [`CONTACTS_TTL`], and when [`want_contacts`](Self::want_contacts)
    /// says so (the picker opened, a number reconnected).
    async fn run_contacts(self) {
        let mut failures = 0u32;
        loop {
            if self.is_auth_lost() {
                return;
            }
            let accounts = self.inner.store.accounts().unwrap_or_default();
            let mut failed = false;
            for account in accounts {
                let due = {
                    let mut all = self.inner.contact_sync.lock().expect("contacts lock");
                    let state = all.entry(account.id.clone()).or_default();
                    let within = state.within.take().unwrap_or(CONTACTS_TTL);
                    state
                        .finished
                        .is_none_or(|finished| finished.elapsed() >= within)
                };
                if !due {
                    continue;
                }
                match self.sync_contacts(&account.id).await {
                    Ok(_) => {}
                    Err(SyncError::Provider(ProviderError::Unauthorized(_))) => return,
                    Err(error) => {
                        failed = true;
                        tracing::debug!(%error, "copying contacts paused; will resume");
                    }
                }
            }
            failures = if failed { failures + 1 } else { 0 };
            let wait = if failed {
                self.backoff(failures)
            } else {
                CONTACTS_CHECK
            };
            tokio::select! {
                _ = self.inner.contacts_wake.notified() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    // ----- numbers ------------------------------------------------------
    //
    // Linking and managing numbers. These are asked for by a screen that
    // is waiting, so they are awaited (off the UI thread) and their
    // failures go back to that screen; nothing here is retried behind the
    // user's back. The store follows every answer, so the rail and the
    // settings show the number as the provider has it.

    /// Where a new number can connect from. Empty when the provider has
    /// nothing to choose.
    pub async fn link_places(&self) -> Result<Vec<LinkPlace>, SyncError> {
        let places = self.bounded(self.inner.provider.link_places()).await;
        Ok(self.noted(places)?)
    }

    /// Starts linking a number. Safe to repeat with the same
    /// [`NewAccount::request_id`].
    pub async fn create_account(&self, new: &NewAccount) -> Result<LinkStatus, SyncError> {
        let status = self.bounded(self.inner.provider.create_account(new)).await;
        self.linking(status)
    }

    /// Where linking `account` stands now. When it has just linked, its
    /// chats are fetched.
    pub async fn link_status(&self, account: &AccountId) -> Result<LinkStatus, SyncError> {
        let status = self.bounded(self.inner.provider.link_status(account)).await;
        self.linking(status)
    }

    /// Asks for a code to type on the phone.
    pub async fn pairing_code(
        &self,
        account: &AccountId,
        phone: &str,
    ) -> Result<LinkStatus, SyncError> {
        let status = self
            .bounded(self.inner.provider.pairing_code(account, phone))
            .await;
        self.linking(status)
    }

    /// Goes back from the code to type to the QR code, for the same number.
    pub async fn scan_instead(&self, account: &AccountId) -> Result<LinkStatus, SyncError> {
        let status = self
            .bounded(self.inner.provider.scan_instead(account))
            .await;
        self.linking(status)
    }

    /// Restarts a number's session.
    pub async fn reconnect_account(&self, account: &AccountId) -> Result<LinkStatus, SyncError> {
        let status = self
            .bounded(self.inner.provider.reconnect_account(account))
            .await;
        self.linking(status)
    }

    /// Renames a number or changes one of its settings.
    pub async fn update_account(
        &self,
        account: &AccountId,
        change: AccountChange,
    ) -> Result<Account, SyncError> {
        let updated = self
            .bounded(self.inner.provider.update_account(account, change))
            .await;
        let updated = self.noted(updated)?;
        self.inner
            .store
            .upsert_account(self.inner.provider.id(), &updated)?;
        Ok(updated)
    }

    /// Unlinks the phone from a number, keeping the number and what is
    /// stored for it.
    pub async fn unlink_account(&self, account: &AccountId) -> Result<Account, SyncError> {
        let updated = self
            .bounded(self.inner.provider.unlink_account(account))
            .await;
        let updated = self.noted(updated)?;
        self.inner
            .store
            .upsert_account(self.inner.provider.id(), &updated)?;
        Ok(updated)
    }

    /// Deletes a number, at the provider and then here.
    pub async fn delete_account(&self, account: &AccountId) -> Result<(), SyncError> {
        let deleted = self
            .bounded(self.inner.provider.delete_account(account))
            .await;
        self.noted(deleted)?;
        self.inner.store.remove_account(account)?;
        Ok(())
    }

    /// Notes a refused key on the way through.
    fn noted<T>(&self, result: Result<T, ProviderError>) -> Result<T, ProviderError> {
        if let Err(error) = &result {
            self.note_unauthorized(error);
        }
        result
    }

    /// Stores the number of a linking answer, and wakes the sync when it
    /// has just become usable.
    fn linking(&self, status: Result<LinkStatus, ProviderError>) -> Result<LinkStatus, SyncError> {
        let status = self.noted(status)?;
        let was_connected = self
            .inner
            .store
            .accounts()?
            .iter()
            .any(|known| known.id == status.account.id && known.connection.is_connected());
        self.inner
            .store
            .upsert_account(self.inner.provider.id(), &status.account)?;
        if status.step == LinkStep::Linked && !was_connected {
            let imports = status.account.settings.history_import == Some(HistoryImport::Recent);
            let (this, account) = (self.clone(), status.account.id.clone());
            self.inner.runtime.spawn(async move {
                let refreshed = this.refresh().await;
                this.log_failure("refresh after linking", refreshed);
                if !imports {
                    return;
                }
                // The phone hands its history over during the minutes
                // after the link, and nothing announces that it is done
                // (the provider's "history synced" is a webhook, which a
                // desktop cannot receive). So the chat list is read again
                // a few times, further apart, and each chat is taken as
                // unread history: older messages may have appeared under
                // the ones already here.
                for wait in HISTORY_ARRIVES {
                    tokio::time::sleep(wait).await;
                    if this.is_stopped() {
                        return;
                    }
                    if let Err(error) = this.inner.store.forget_history_progress(&account) {
                        tracing::error!(%error, "could not reset history progress");
                    }
                    let refreshed = this.refresh().await;
                    this.log_failure("refresh while history is imported", refreshed);
                }
            });
        }
        Ok(status)
    }

    /// Makes sure the newest page of a chat's history is in the store.
    /// Call when the user opens a chat.
    pub fn open_chat(&self, account: &AccountId, chat: &ChatId) {
        let this = self.clone();
        let (account, chat) = (account.clone(), chat.clone());
        self.inner.runtime.spawn(async move {
            let loaded = this
                .inner
                .store
                .history_state(&account, &chat)
                .map(|state| state.loaded)
                .unwrap_or(false);
            // A group's participants and settings, behind the messages.
            let is_group = matches!(
                this.inner.store.chat(&account, &chat),
                Ok(Some(known)) if known.kind == client_provider::ChatKind::Group
            );
            if is_group {
                this.want_group(&account, &chat);
            }
            if !loaded {
                this.log_failure("history", this.fetch_latest(&account, &chat).await);
            }
        });
    }

    /// Fetches one more page of older history in the background. Call when
    /// the user scrolls to the top of what is loaded.
    pub fn request_older(&self, account: &AccountId, chat: &ChatId) {
        let this = self.clone();
        let (account, chat) = (account.clone(), chat.clone());
        self.inner.runtime.spawn(async move {
            this.log_failure("older history", this.fetch_older(&account, &chat).await);
        });
    }

    /// What somebody in the chat is doing right now, if anything.
    pub fn presence(&self, account: &AccountId, chat: &ChatId) -> Option<PresenceState> {
        let presence = self.inner.presence.lock().expect("presence lock");
        presence
            .get(&(account.clone(), chat.clone()))
            .filter(|entry| entry.at.elapsed() < self.inner.config.presence_ttl)
            .map(|entry| entry.state)
            .filter(|state| !matches!(state, PresenceState::Idle))
    }

    // ----- sync steps: used by the background loops and by tests -------

    /// Copies accounts and chat lists into the store: one request for the
    /// accounts and one per page of each account's chats, and nothing per
    /// chat. Chats arrive with their last message, so the list is complete
    /// when this returns; history is the preloader's business
    /// ([`preload`](Self::preload)) and the open chat's.
    pub async fn refresh(&self) -> Result<(), SyncError> {
        let inner = &self.inner;
        let accounts = self.bounded(inner.provider.list_accounts()).await?;
        // A number the provider no longer lists was deleted somewhere
        // else: it goes from here too, with everything stored for it.
        // Only after a listing that answered in full, never on a failure.
        for known in inner.store.accounts()? {
            if !accounts.iter().any(|listed| listed.id == known.id) {
                tracing::info!(account = %known.id, "a number was deleted elsewhere; removing it");
                inner.store.remove_account(&known.id)?;
            }
        }
        inner
            .store
            .upsert_accounts(inner.provider.id(), &accounts)?;

        for account in &accounts {
            let mut cursor = None;
            loop {
                let page = self
                    .bounded(inner.provider.list_chats(&account.id, cursor))
                    .await?;
                for chat in &page.items {
                    let keep_unread = !inner.capabilities.chat_list;
                    let chat = &self.with_pending_state(chat.clone());
                    self.note_picture(chat);
                    let news = inner.store.upsert_chat(chat, keep_unread)?;
                    let loaded = inner.store.history_state(&account.id, &chat.id)?.loaded;
                    if news && loaded {
                        // Read before, and something newer than what is
                        // stored: fill the hole in the background.
                        let mut gaps = inner.gaps.lock().expect("gaps lock");
                        let gap = (account.id.clone(), chat.id.clone());
                        if !gaps.contains(&gap) {
                            gaps.push(gap);
                        }
                    }
                }
                match page.next_cursor {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            // Ticks that may have moved while nobody was listening (the
            // application was closed, the event stream was down): the
            // conversations read here before, with recent messages of the
            // account's own that are not read yet, get their newest page
            // read again.
            let since = Timestamp::from_millis(
                Timestamp::now().as_millis() - RECEIPTS_WINDOW.as_millis() as i64,
            );
            let awaiting =
                inner
                    .store
                    .chats_awaiting_receipts(&account.id, since, RECEIPTS_CHATS)?;
            for chat in awaiting {
                // Only a conversation that was read here before: one that
                // was not is read whole when it is opened.
                if !inner.store.history_state(&account.id, &chat)?.loaded {
                    continue;
                }
                let gap = (account.id.clone(), chat);
                let mut gaps = inner.gaps.lock().expect("gaps lock");
                if !gaps.contains(&gap) {
                    gaps.push(gap);
                }
            }
        }
        inner.preload_wake.notify_one();
        Ok(())
    }

    /// One pass of background history loading, for the current
    /// [`HistoryMode`]: the holes left by a refresh first, then the newest
    /// chats.
    ///
    /// At most `preload_concurrency` requests are in flight and their
    /// starts are `preload_pace` apart. A rate limit is waited out (for
    /// the provider's `Retry-After` when it gives one) and the request
    /// repeated. Any other failure ends the pass with what is done kept:
    /// the background loop runs it again later, so a dropped connection
    /// pauses preloading and a reconnect resumes it.
    pub async fn preload(&self) -> Result<(), SyncError> {
        let inner = &self.inner;
        let mode = self.history();
        // (account, chat, every page?)
        let mut jobs: Vec<(AccountId, ChatId, bool)> = {
            let mut gaps = inner.gaps.lock().expect("gaps lock");
            gaps.drain(..).map(|(a, c)| (a, c, false)).collect()
        };
        let mut seen: HashSet<(AccountId, ChatId)> = jobs
            .iter()
            .map(|(a, c, _)| (a.clone(), c.clone()))
            .collect();
        if mode != HistoryMode::OnOpen {
            for account in inner.store.accounts()? {
                let mut chats = inner.store.chats(&account.id, None)?;
                // Newest first; the list's own order puts pins on top.
                chats.sort_by_key(|chat| {
                    std::cmp::Reverse(chat.last_message.as_ref().map(|m| m.timestamp))
                });
                let (take, all) = match mode {
                    HistoryMode::Recent(n) => (n, false),
                    _ => (usize::MAX, true),
                };
                for chat in chats.into_iter().take(take) {
                    let state = inner.store.history_state(&account.id, &chat.id)?;
                    let wanted = if all { !state.complete } else { !state.loaded };
                    if wanted && seen.insert((account.id.clone(), chat.id.clone())) {
                        jobs.push((account.id.clone(), chat.id, all));
                    }
                }
            }
        }

        let concurrency = inner.config.preload_concurrency.max(1);
        let mut results = futures::stream::iter(jobs.into_iter().map(|job| {
            let this = self.clone();
            async move {
                let result = this.preload_chat(&job.0, &job.1, job.2).await;
                (job, result)
            }
        }))
        .buffer_unordered(concurrency);

        let mut failure = None;
        while let Some(((account, chat, _), result)) = results.next().await {
            if let Err(error) = result {
                if matches!(error, SyncError::Provider(ref e) if e.is_transient() || matches!(e, ProviderError::Unauthorized(_)))
                {
                    // Not done: a hole stays a hole until it is filled.
                    let mut gaps = inner.gaps.lock().expect("gaps lock");
                    gaps.push((account, chat));
                    failure.get_or_insert(error);
                } else {
                    // This chat cannot be read (it is gone, say). The
                    // others can.
                    self.log_failure::<()>("history", Err(error));
                }
            }
            if failure.is_some() {
                // Stop starting new requests; the ones in flight finish.
                break;
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// The history of one chat, for the preloader: its newest page, and
    /// with `all` every older one.
    async fn preload_chat(
        &self,
        account: &AccountId,
        chat: &ChatId,
        all: bool,
    ) -> Result<(), SyncError> {
        self.paced(|| self.fetch_latest(account, chat)).await?;
        while all && self.paced(|| self.fetch_older(account, chat)).await? {}
        Ok(())
    }

    /// Runs one preload request in its turn, repeating it after a rate
    /// limit.
    async fn paced<T, F>(&self, request: impl Fn() -> F) -> Result<T, SyncError>
    where
        F: Future<Output = Result<T, SyncError>>,
    {
        let mut limited = 0u32;
        loop {
            let wait = {
                let mut next = self.inner.next_preload.lock().expect("pace lock");
                let now = tokio::time::Instant::now();
                let at = next.map_or(now, |at| at.max(now));
                *next = Some(at + self.inner.config.preload_pace);
                at - now
            };
            tokio::time::sleep(wait).await;
            match request().await {
                Err(SyncError::Provider(ProviderError::RateLimited { retry_after }))
                    if limited < 5 =>
                {
                    limited += 1;
                    let wait = retry_after.unwrap_or(self.backoff(limited));
                    tracing::debug!(?wait, "rate limited while preloading; waiting");
                    // Everyone waits, not only this request.
                    {
                        let mut next = self.inner.next_preload.lock().expect("pace lock");
                        let until = tokio::time::Instant::now() + wait;
                        *next = Some(next.map_or(until, |at| at.max(until)));
                    }
                }
                other => return other,
            }
        }
    }

    /// Fetches the newest page of a chat.
    pub async fn fetch_latest(&self, account: &AccountId, chat: &ChatId) -> Result<(), SyncError> {
        let inner = &self.inner;
        let page = self
            .bounded(
                inner
                    .provider
                    .fetch_messages(account, chat, None, inner.config.page_size),
            )
            .await?;
        inner.store.upsert_messages(&page.items)?;
        let mut state = inner.store.history_state(account, chat)?;
        if !state.loaded {
            // First visit: remember where older history continues. On later
            // visits the stored cursor already points further back.
            state = HistoryState {
                loaded: true,
                complete: page.next_cursor.is_none(),
                cursor: page.next_cursor,
            };
            inner.store.set_history_state(account, chat, &state)?;
        }
        Ok(())
    }

    /// Fetches the next older page of a chat. Returns `false` when there is
    /// nothing older (or another fetch for the chat is already running).
    pub async fn fetch_older(&self, account: &AccountId, chat: &ChatId) -> Result<bool, SyncError> {
        let inner = &self.inner;
        let key = (account.clone(), chat.clone());
        if !inner
            .fetching
            .lock()
            .expect("fetching lock")
            .insert(key.clone())
        {
            return Ok(false);
        }
        let result = async {
            let state = inner.store.history_state(account, chat)?;
            if !state.loaded {
                self.fetch_latest(account, chat).await?;
                return Ok(true);
            }
            if state.complete {
                return Ok(false);
            }
            let page = self
                .bounded(inner.provider.fetch_messages(
                    account,
                    chat,
                    state.cursor,
                    inner.config.page_size,
                ))
                .await?;
            inner.store.upsert_messages(&page.items)?;
            inner.store.set_history_state(
                account,
                chat,
                &HistoryState {
                    loaded: true,
                    complete: page.next_cursor.is_none(),
                    cursor: page.next_cursor,
                },
            )?;
            // Tell the view even if every message was already known, so it
            // stops showing a spinner.
            inner.store.notify(StoreChange::Messages {
                account_id: account.clone(),
                chat_id: chat.clone(),
            });
            Ok(true)
        }
        .await;
        inner.fetching.lock().expect("fetching lock").remove(&key);
        result
    }

    /// Applies one live update to the store.
    pub fn apply_event(&self, event: ProviderEvent) -> Result<(), StoreError> {
        let inner = &self.inner;
        match event {
            ProviderEvent::MessageUpserted(message) => {
                let outcome = inner.store.upsert_message(&message)?;
                if message.direction == Direction::Outgoing && message.client_id.is_some() {
                    // The provider's copy of a message sent from here may
                    // have taken it out of the queue: what waited behind
                    // it in its chat can go now.
                    inner.outbox_wake.notify_one();
                }
                let counts_as_unread = outcome == Upsert::Inserted
                    && message.direction == Direction::Incoming
                    && !matches!(
                        message.content,
                        MessageContent::Reaction { .. } | MessageContent::System(_)
                    );
                // Providers with a real chat list report unread counts
                // themselves; for the others, and for the chats such a
                // list has no count for, the engine keeps count.
                let uncounted = !inner.capabilities.chat_list
                    || inner
                        .uncounted
                        .lock()
                        .expect("uncounted lock")
                        .contains(&(message.account_id.clone(), message.chat_id.clone()));
                if counts_as_unread && uncounted {
                    inner
                        .store
                        .bump_unread(&message.account_id, &message.chat_id)?;
                }
                if message.direction == Direction::Incoming {
                    self.set_presence(
                        &message.account_id,
                        &message.chat_id,
                        &message.sender,
                        PresenceState::Idle,
                    );
                }
            }
            ProviderEvent::MessageStatusChanged {
                account_id,
                message_id,
                status,
                ..
            } => {
                inner
                    .store
                    .set_message_status(&account_id, &message_id, &status)?;
            }
            ProviderEvent::ChatUpdated(chat) => {
                let chat = self.with_pending_state(chat);
                self.note_picture(&chat);
                self.note_group_chat(&chat);
                inner
                    .store
                    .upsert_chat(&chat, !inner.capabilities.chat_list)?;
            }
            ProviderEvent::ContactUpdated(contact) => {
                inner.store.upsert_contact(&contact)?;
            }
            ProviderEvent::GroupChanged {
                account_id,
                group_id,
            } => self.group_changed(&account_id, &group_id),
            ProviderEvent::Presence {
                account_id,
                chat_id,
                contact_id,
                state,
            } => self.set_presence(&account_id, &chat_id, &contact_id, state),
            ProviderEvent::ConnectionChanged { account_id, state } => {
                inner.store.set_connection(&account_id, &state)?;
                if state.is_connected() {
                    // The account is back: send what was waiting for it now.
                    inner
                        .store
                        .outbox_flush_account(&account_id, Timestamp::now())?;
                    inner.outbox_wake.notify_one();
                    // And go on loading history where it stopped.
                    inner.preload_wake.notify_one();
                    // The address book may have changed meanwhile.
                    self.want_contacts(&account_id, CONTACTS_AFTER_RECONNECT);
                    // So may the stickers starred on the phone.
                    self.want_favorite_stickers(&account_id);
                    // And so may the stories; what waited to be posted goes.
                    self.stories_reconnected(&account_id);
                }
            }
            event @ (ProviderEvent::StoryUpserted(_)
            | ProviderEvent::StoryRemoved { .. }
            | ProviderEvent::StoryViewed { .. }
            | ProviderEvent::StoryMuteChanged { .. }) => self.apply_story_event(event)?,
        }
        Ok(())
    }

    /// Runs the outbox once, for everything due now.
    pub async fn flush_outbox(&self) -> Result<crate::OutboxPass, StoreError> {
        crate::outbox::run_outbox_pass_with(
            &self.inner.store,
            self.inner.provider.as_ref(),
            &self.inner.config.outbox,
            Timestamp::now(),
            &self.upload_hooks(false),
        )
        .await
    }

    // ----- files on their way out -----------------------------------------

    /// Queues a file for a chat. The bytes are copied into the store with
    /// the message, so the send survives a restart and no longer depends
    /// on where the file came from; the bubble shows that copy at once.
    ///
    /// A file larger than the provider takes is refused here, before
    /// anything is stored or uploaded. This is [`prepare_media`]
    /// (Self::prepare_media) and [`send_prepared`](Self::send_prepared) in
    /// one call: it decodes an image, so an interface calls the two
    /// itself, the first off its thread.
    pub fn send_media(
        &self,
        account: &AccountId,
        chat: &ChatId,
        file: NewMedia,
    ) -> Result<ClientMessageId, SendMediaError> {
        let prepared = self.prepare_media(file)?;
        self.send_prepared(account, chat, prepared)
    }

    /// Checks a file against what the provider takes and, for an image,
    /// makes the thumbnail its bubble will show. Touches no store and no
    /// network; decoding a large picture takes a moment, so call it off
    /// the UI thread.
    pub fn prepare_media(&self, file: NewMedia) -> Result<PreparedMedia, SendMediaError> {
        let inner = &self.inner;
        if !inner.capabilities.media_upload {
            return Err(SendMediaError::NotAvailable);
        }
        let size = file.bytes.len() as u64;
        if let Some(limit) = inner.provider.media_upload_limit() {
            if size > limit {
                return Err(SendMediaError::TooLarge { size, limit });
            }
        }
        if size == 0 {
            return Err(SendMediaError::Empty);
        }
        let thumbnail = matches!(file.kind, MediaKind::Image | MediaKind::Sticker)
            .then(|| crate::thumbnail(&file.bytes, THUMBNAIL_SIDE).ok())
            .flatten();
        Ok(PreparedMedia { file, thumbnail })
    }

    /// Queues a prepared file: its copy, its thumbnail and its pending
    /// bubble go into the store, and the outbox takes it from there.
    pub fn send_prepared(
        &self,
        account: &AccountId,
        chat: &ChatId,
        prepared: PreparedMedia,
    ) -> Result<ClientMessageId, SendMediaError> {
        self.queue_prepared(account, chat, prepared, false)
    }

    /// [`send_prepared`](Self::send_prepared), with a video marked to
    /// play as a GIF when `gif` is set.
    pub(crate) fn queue_prepared(
        &self,
        account: &AccountId,
        chat: &ChatId,
        prepared: PreparedMedia,
        gif: bool,
    ) -> Result<ClientMessageId, SendMediaError> {
        let inner = &self.inner;
        let PreparedMedia { file, thumbnail } = prepared;
        let client_id = new_client_id();
        let local = crate::outbox::local_media_ref(&client_id);
        // An image shows from the copy in hand: the bubble never waits
        // for the network to show what is being sent.
        let mut pixels = None;
        if let Some(small) = thumbnail {
            pixels = Some((small.width, small.height));
            let cached = crate::CachedMedia {
                size: Some((small.width, small.height)),
                mime: Some(small.mime.to_owned()),
                bytes: small.bytes,
            };
            inner.store.put_media(
                &thumbnail_key(local.as_str()),
                &cached,
                Timestamp::now(),
                inner.config.media_budget,
            )?;
        }
        let message = OutgoingMessage {
            client_id: client_id.clone(),
            account_id: account.clone(),
            chat_id: chat.clone(),
            content: OutgoingContent::Media {
                kind: file.kind,
                media: local,
                mime_type: Some(file.mime_type.clone()),
                caption: file.caption.filter(|caption| !caption.trim().is_empty()),
                file_name: file.file_name.clone(),
                gif: gif && file.kind == MediaKind::Video,
            },
            reply_to: file.reply_to,
            mentions: self.mentions_of(file.mentions),
            forwarded: false,
        };
        inner.store.enqueue_with_file(
            &message,
            Some(&crate::outbox::OutboxFile {
                bytes: std::sync::Arc::new(file.bytes),
                mime: file.mime_type,
                file_name: file.file_name,
            }),
            pixels,
            self.queue_time(),
            inner.config.outbox.max_age,
        )?;
        inner.outbox_wake.notify_one();
        Ok(client_id)
    }

    /// The largest file the provider takes, when it has a limit.
    pub fn upload_limit(&self) -> Option<u64> {
        self.inner.provider.media_upload_limit()
    }

    /// How far the upload of a queued message's file is: bytes out, bytes
    /// in all. `None` when it is not being uploaded right now.
    pub fn upload_progress(&self, client_id: &ClientMessageId) -> Option<(u64, u64)> {
        self.inner
            .uploads
            .lock()
            .expect("uploads lock")
            .get(client_id)
            .map(|upload| (upload.sent, upload.total))
    }

    /// Takes a queued message back: its file is not uploaded (or the
    /// upload under way is dropped) and its bubble goes. Returns false
    /// when it is too late, because the message is being sent or was.
    pub fn cancel_send(&self, client_id: &ClientMessageId) -> Result<bool, StoreError> {
        let cancelled = self.inner.store.outbox_cancel(client_id)?;
        if cancelled {
            // An upload under way notices at its next step.
            if let Some(upload) = self
                .inner
                .uploads
                .lock()
                .expect("uploads lock")
                .remove(client_id)
            {
                upload.cancel.notify_waiters();
            }
            self.inner.outbox_wake.notify_one();
        }
        Ok(cancelled)
    }

    /// Whether files can be sent right now, as far as is known: `None`
    /// until the provider has been asked. Asks again behind the scenes
    /// when the answer is old (a "no" is asked about again after ten
    /// minutes; a "yes" holds for the session).
    pub fn uploads_available(&self) -> Option<bool> {
        if !self.inner.capabilities.media_upload {
            return Some(false);
        }
        let known = *self.inner.upload_ready.lock().expect("upload lock");
        let stale = match known {
            Some((true, _)) => false,
            Some((false, at)) => at.elapsed() >= UPLOADS_RECHECK,
            None => true,
        };
        if stale && !self.is_stopped() {
            self.check_uploads();
        }
        known.map(|(ready, _)| ready)
    }

    /// Asks the provider whether files can be sent, once at a time.
    pub fn check_uploads(&self) {
        {
            let mut checking = self.inner.upload_checking.lock().expect("upload lock");
            if *checking {
                return;
            }
            *checking = true;
        }
        let this = self.clone();
        self.inner.runtime.spawn(async move {
            let ready = this
                .bounded(async { Ok(this.inner.provider.media_upload_ready().await) })
                .await
                .ok();
            if let Some(ready) = ready {
                *this.inner.upload_ready.lock().expect("upload lock") =
                    Some((ready, Instant::now()));
                // The attach button may have a new answer to show.
                this.inner.store.notify(StoreChange::Accounts);
            }
            *this.inner.upload_checking.lock().expect("upload lock") = false;
        });
    }

    /// What the outbox does about files: progress is kept for the bubbles
    /// to show, and (with `spawn`) each upload runs on its own, two at a
    /// time, so that a large file does not hold up the other chats.
    fn upload_hooks(&self, spawn: bool) -> crate::outbox::UploadHooks {
        let progress_of = self.clone();
        let progress: crate::outbox::UploadReport =
            std::sync::Arc::new(move |client_id, sent, total| {
                progress_of.note_upload(client_id, sent, total);
            });
        let start = spawn.then(|| {
            let this = self.clone();
            let start: crate::outbox::UploadStart =
                std::sync::Arc::new(move |entry| this.start_upload(entry));
            start
        });
        crate::outbox::UploadHooks {
            progress: Some(progress),
            start,
        }
    }

    /// Records how far an upload is, and tells the view now and then.
    fn note_upload(&self, client_id: &ClientMessageId, sent: u64, total: u64) {
        let tell = {
            let mut uploads = self.inner.uploads.lock().expect("uploads lock");
            let upload = uploads.entry(client_id.clone()).or_default();
            upload.sent = sent;
            upload.total = total;
            let due = upload
                .told
                .is_none_or(|told| told.elapsed() >= Duration::from_millis(150));
            if due || sent >= total {
                upload.told = Some(Instant::now());
            }
            let tell = due || sent >= total;
            // An upload made within a pass has nobody to tidy up after
            // it: done is done.
            if sent >= total && !upload.running {
                uploads.remove(client_id);
            }
            tell
        };
        if tell {
            if let Ok(Some(chat)) = self.inner.store.outbox_chat(client_id) {
                self.inner.store.notify(StoreChange::Messages {
                    account_id: chat.0,
                    chat_id: chat.1,
                });
            }
        }
    }

    /// Uploads one queued file on its own task, unless it already is.
    fn start_upload(&self, entry: crate::OutboxEntry) {
        let client_id = entry.message.client_id.clone();
        let cancel = {
            let mut uploads = self.inner.uploads.lock().expect("uploads lock");
            let upload = uploads.entry(client_id.clone()).or_default();
            if upload.running {
                return;
            }
            upload.running = true;
            upload.cancel.clone()
        };
        let this = self.clone();
        self.inner.runtime.spawn(async move {
            let inner = &this.inner;
            let work = async {
                let Ok(_slot) = inner.upload_slots.acquire().await else {
                    return;
                };
                let outcome = crate::outbox::upload_entry(
                    &inner.store,
                    inner.provider.as_ref(),
                    &inner.config.outbox,
                    &entry,
                    Timestamp::now(),
                    &this.upload_hooks(false),
                )
                .await;
                match outcome {
                    Ok(crate::outbox::UploadOutcome::Unauthorized(reason)) => {
                        this.note_unauthorized(&ProviderError::Unauthorized(reason));
                    }
                    Ok(_) => {}
                    Err(error) => tracing::error!(%error, "could not record an upload"),
                }
            };
            // A cancelled upload is dropped where it stands.
            tokio::select! {
                _ = work => {}
                _ = cancel.notified() => {}
            }
            inner
                .uploads
                .lock()
                .expect("uploads lock")
                .remove(&client_id);
            if let Ok(Some(chat)) = inner.store.outbox_chat(&client_id) {
                inner.store.notify(StoreChange::Messages {
                    account_id: chat.0,
                    chat_id: chat.1,
                });
            }
            inner.outbox_wake.notify_one();
        });
    }

    // ----- internals ---------------------------------------------------

    fn set_presence(
        &self,
        account: &AccountId,
        chat: &ChatId,
        contact: &ContactId,
        state: PresenceState,
    ) {
        let key = (account.clone(), chat.clone());
        let changed = {
            let mut presence = self.inner.presence.lock().expect("presence lock");
            match state {
                PresenceState::Idle => match presence.get(&key) {
                    Some(entry) if &entry.contact == contact => presence.remove(&key).is_some(),
                    _ => false,
                },
                _ => {
                    presence.insert(
                        key,
                        PresenceEntry {
                            contact: contact.clone(),
                            state,
                            at: Instant::now(),
                        },
                    );
                    true
                }
            }
        };
        if changed {
            self.inner.store.notify(StoreChange::Presence {
                account_id: account.clone(),
                chat_id: chat.clone(),
            });
            if !matches!(state, PresenceState::Idle) {
                // Indicators expire on their own: tell the view to look
                // again once this one is stale.
                let this = self.clone();
                let (account, chat) = (account.clone(), chat.clone());
                let ttl = self.inner.config.presence_ttl;
                self.inner.runtime.spawn(async move {
                    tokio::time::sleep(ttl).await;
                    this.inner.store.notify(StoreChange::Presence {
                        account_id: account,
                        chat_id: chat,
                    });
                });
            }
        }
    }

    /// Bounds a provider call: an overrun is a transient failure.
    async fn bounded<T>(
        &self,
        call: impl Future<Output = Result<T, ProviderError>>,
    ) -> Result<T, ProviderError> {
        match tokio::time::timeout(self.inner.config.call_timeout, call).await {
            Ok(result) => result,
            Err(_) => Err(ProviderError::Transient("the request timed out".to_owned())),
        }
    }

    fn log_failure<T>(&self, what: &str, result: Result<T, SyncError>) {
        if let Err(error) = result {
            if let SyncError::Provider(provider) = &error {
                if self.note_unauthorized(provider) {
                    return;
                }
            }
            if error.is_transient() {
                tracing::debug!(%error, "{what}: will be retried");
            } else {
                tracing::warn!(%error, "{what} failed");
            }
        }
    }

    fn backoff(&self, failures: u32) -> Duration {
        let factor = 1u32 << failures.saturating_sub(1).min(16);
        self.inner
            .config
            .retry_base
            .saturating_mul(factor)
            .min(self.inner.config.retry_max)
    }

    /// Follows the provider: subscribe, refresh, apply events, and start
    /// over (with backoff) whenever the stream ends. Ends for good when the
    /// provider refuses the credentials: that is not weather, and asking
    /// again would only get the same answer.
    async fn run_events(self) {
        let mut failures = 0u32;
        // Subscribes that failed in a row. A refresh that works says the
        // REST side is healthy, not that the stream is: it does not clear
        // this, or a stream that refuses would be asked every `retry_base`.
        let mut subscribe_failures = 0u32;
        loop {
            let mut asked_to_wait = Duration::ZERO;
            if self.is_auth_lost() {
                return;
            }
            // Subscribe before refreshing so nothing that happens during
            // the refresh is missed.
            let subscription = self.bounded(self.inner.provider.subscribe()).await;
            if matches!(&subscription, Err(error) if self.note_unauthorized(error)) {
                return;
            }
            match self.refresh().await {
                Ok(()) => failures = 0,
                Err(SyncError::Provider(error)) if self.note_unauthorized(&error) => return,
                Err(error) => {
                    failures += 1;
                    self.log_failure::<()>("refresh", Err(error));
                }
            }
            match subscription {
                Ok(mut events) if failures == 0 => {
                    subscribe_failures = 0;
                    while let Some(event) = events.next().await {
                        if let Err(error) = self.apply_event(event) {
                            tracing::error!(%error, "could not store an event");
                        }
                    }
                    tracing::debug!("the event stream ended; resubscribing");
                    failures = 1;
                }
                Ok(_) => subscribe_failures = 0,
                Err(error) => {
                    subscribe_failures += 1;
                    failures = failures.max(subscribe_failures);
                    tracing::debug!(%error, "could not subscribe to events");
                    if let ProviderError::RateLimited {
                        retry_after: Some(asked),
                    } = error
                    {
                        // Asked to wait: never less than that.
                        asked_to_wait = asked.min(RETRY_AFTER_MAX);
                    }
                }
            }
            tokio::time::sleep(self.backoff(failures).max(asked_to_wait)).await;
        }
    }

    /// Loads history in the background: a pass whenever there is reason to
    /// (a refresh, a reconnect, a new mode), again with backoff while the
    /// connection is down.
    async fn run_preload(self) {
        loop {
            self.inner.preload_wake.notified().await;
            let mut failures = 0u32;
            loop {
                if self.is_auth_lost() {
                    return;
                }
                match self.preload().await {
                    Ok(()) => break,
                    Err(SyncError::Provider(error)) if self.note_unauthorized(&error) => return,
                    Err(error) if error.is_transient() => {
                        failures += 1;
                        tracing::debug!(%error, "preloading paused; will resume");
                        tokio::select! {
                            _ = self.inner.preload_wake.notified() => {}
                            _ = tokio::time::sleep(self.backoff(failures)) => {}
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "preloading failed");
                        break;
                    }
                }
            }
        }
    }

    /// Sends queued messages whenever something is due.
    async fn run_outbox(self) {
        loop {
            // In the background each file is uploaded on its own task, so
            // one large file does not hold up the other chats.
            let pass = crate::outbox::run_outbox_pass_with(
                &self.inner.store,
                self.inner.provider.as_ref(),
                &self.inner.config.outbox,
                Timestamp::now(),
                &self.upload_hooks(true),
            )
            .await;
            match pass {
                Ok(pass) => {
                    if let Some(reason) = pass.unauthorized {
                        self.note_unauthorized(&ProviderError::Unauthorized(reason));
                        // What is queued stays queued, for after the next
                        // sign-in.
                        return;
                    }
                }
                Err(error) => tracing::error!(%error, "outbox pass failed"),
            }
            // Sleep until the next retry or expiry is due, or until
            // something is queued or an account reconnects.
            let wait = match self.inner.store.outbox_next_wakeup() {
                Ok(Some(at)) => {
                    let millis = (at.as_millis() - Timestamp::now().as_millis()).max(0) as u64;
                    Some(Duration::from_millis(millis))
                }
                Ok(None) => None,
                Err(_) => Some(self.inner.config.retry_max),
            };
            match wait {
                Some(wait) => {
                    tokio::select! {
                        _ = self.inner.outbox_wake.notified() => {}
                        _ = tokio::time::sleep(wait) => {}
                    }
                }
                None => self.inner.outbox_wake.notified().await,
            }
        }
    }
}

// ----- pictures and media ---------------------------------------------------

/// How long a chat's picture is taken to be current before the provider is
/// asked again.
pub const AVATAR_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// How long to wait before asking again for a picture that could not be
/// fetched (offline, a hiccup).
const AVATAR_RETRY: Duration = Duration::from_secs(60);
/// Pictures are kept at this many pixels per side: twice the largest
/// avatar on screen, for HiDPI.
const AVATAR_SIDE: u32 = 96;
/// Message images are kept as thumbnails of at most this many pixels per
/// side: twice the widest an image is shown in a bubble.
const THUMBNAIL_SIDE: u32 = 720;
/// The largest file fetched without being asked (an image for a bubble).
pub const AUTO_MEDIA_LIMIT: u64 = 25 * 1024 * 1024;
/// The largest file fetched when the user asks for it.
pub const MANUAL_MEDIA_LIMIT: u64 = 200 * 1024 * 1024;
/// How many media downloads run at once.
const MEDIA_WORKERS: usize = 3;
/// How many media downloads may be waiting. Beyond it the oldest request
/// is forgotten: it was for rows that have scrolled away.
const MEDIA_QUEUE: usize = 24;
/// How long one media download may take.
const MEDIA_TIMEOUT: Duration = Duration::from_secs(120);
/// How long to wait before trying again a download that failed for a
/// reason that may pass.
const MEDIA_RETRY: Duration = Duration::from_secs(30);

/// The cache key of the thumbnail of the image at `url`.
pub fn thumbnail_key(url: &str) -> String {
    format!("thumb:{url}")
}

/// The cache key of the animated original of the image at `url`: the
/// file as it came, when it is an animated WebP or GIF small enough to
/// keep, or an empty entry that says "it does not move" (so the question
/// is asked once).
pub fn animation_key(url: &str) -> String {
    format!("anim:{url}")
}

/// The largest animated image that is kept as it came. WhatsApp's
/// animated stickers are well under a megabyte.
pub const ANIMATION_KEEP_LIMIT: usize = 8 * 1024 * 1024;

/// The cache key of the whole file at `url`.
pub fn file_key(url: &str) -> String {
    format!("file:{url}")
}

/// Where a media payload stands, when it is not in the cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaState {
    /// Nobody asked for it, or it will be tried again.
    Idle,
    /// Being downloaded, or waiting its turn.
    Loading,
    /// It could not be had, with the reason: too large, not an image,
    /// refused. Asking again may work.
    Unavailable(String),
    /// It is gone for good (WhatsApp no longer has the file): there is
    /// nothing to ask again for.
    Expired(String),
}

/// How long a "files cannot be sent" answer is good for.
const UPLOADS_RECHECK: Duration = Duration::from_secs(10 * 60);

/// One file on its way to the provider.
#[derive(Default)]
struct Upload {
    running: bool,
    sent: u64,
    total: u64,
    /// When the view was last told.
    told: Option<Instant>,
    cancel: std::sync::Arc<Notify>,
}

/// A file the user wants to send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMedia {
    /// What it is sent as.
    pub kind: MediaKind,
    /// The bytes.
    pub bytes: Vec<u8>,
    /// Its MIME type.
    pub mime_type: String,
    /// Its name, when it has one.
    pub file_name: Option<String>,
    /// The text under it.
    pub caption: Option<String>,
    /// The message it answers.
    pub reply_to: Option<MessageId>,
    /// The people the caption mentions: it names each of them as `@` and
    /// [`SyncEngine::mention_handle`] of their id.
    pub mentions: Vec<ContactId>,
}

/// A file checked and ready to be queued
/// ([`SyncEngine::prepare_media`]).
#[derive(Clone, Debug)]
pub struct PreparedMedia {
    file: NewMedia,
    thumbnail: Option<crate::imaging::Thumbnail>,
}

/// Why a file was not queued.
#[derive(Debug, thiserror::Error)]
pub enum SendMediaError {
    /// The provider cannot take files.
    #[error("Sending files is not available with this provider.")]
    NotAvailable,
    /// Larger than the provider takes.
    #[error("The file is larger than can be sent.")]
    TooLarge {
        /// The file's size, in bytes.
        size: u64,
        /// The most the provider takes, in bytes.
        limit: u64,
    },
    /// Nothing in it.
    #[error("The file is empty.")]
    Empty,
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What asking for a chat with a typed number gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NewChat {
    /// The chat, in the store.
    Open(ChatId),
    /// The number (in E.164) has no WhatsApp: there is no chat to open.
    NotOnWhatsApp(String),
}

/// How long a copy of an address book is good for.
const CONTACTS_TTL: Duration = Duration::from_secs(6 * 3600);
/// How old a copy may be when its number reconnects before it is taken
/// again.
const CONTACTS_AFTER_RECONNECT: Duration = Duration::from_secs(15 * 60);
/// How often the copies' ages are looked at.
const CONTACTS_CHECK: Duration = Duration::from_secs(10 * 60);
/// The most pages one pass reads: 50,000 contacts at 100 a page.
const CONTACT_PAGES: usize = 500;

/// Where copying one account's address book stands.
#[derive(Default)]
struct ContactSync {
    /// A pass is running.
    running: bool,
    /// Where the pass under way continues; `None` at its start.
    cursor: Option<client_provider::Cursor>,
    /// When the pass under way began: what its rows are marked with.
    pass: Option<Timestamp>,
    /// The mark of the newest pass begun.
    last_pass: i64,
    /// How many pages the pass under way has read.
    pages: usize,
    /// When the last complete pass ended.
    finished: Option<Instant>,
    /// Someone wants a copy no older than this.
    within: Option<Duration>,
}

/// When the chat list is read again after a number links with history
/// import on: 20 seconds, then 1, 3 and 5 minutes after that.
const HISTORY_ARRIVES: [Duration; 4] = [
    Duration::from_secs(20),
    Duration::from_secs(60),
    Duration::from_secs(180),
    Duration::from_secs(300),
];

/// What is known about the id of a chat's picture.
#[derive(Default)]
struct PictureHint {
    /// The id the chat list last carried (`Some(None)`: it carried none).
    seen: Option<Option<String>>,
    /// The id the provider was last asked about, and answered.
    asked: Option<String>,
}

/// What of a file is wanted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MediaWant {
    Thumbnail,
    Whole,
    Animation,
}

struct MediaJob {
    account: AccountId,
    url: String,
    key: String,
    /// The whole file, on request; otherwise a thumbnail of an image.
    whole: bool,
    /// The image is wanted for whether it moves: its thumbnail is already
    /// here, from before animations were kept.
    animation: bool,
    /// The user asked for this one: the larger limit applies.
    asked: bool,
}

enum MediaSlot {
    Loading,
    RetryAt(Instant),
    Failed(String),
    /// The file no longer exists anywhere.
    Gone(String),
}

#[derive(Default)]
struct MediaQueue {
    /// Newest request first.
    waiting: std::collections::VecDeque<MediaJob>,
    slots: HashMap<String, MediaSlot>,
    workers: usize,
    /// Downloads under way that the user cancelled.
    cancelled: HashSet<String>,
}

impl SyncEngine {
    fn is_stopped(&self) -> bool {
        self.inner.stopped.load(std::sync::atomic::Ordering::SeqCst) || self.is_auth_lost()
    }

    /// The copy kept of a file sent from here: in the outbox while it is
    /// queued, in the media cache once it has gone.
    fn local_media(&self, client_id: &ClientMessageId) -> Option<client_provider::MediaData> {
        let store = &self.inner.store;
        if let Ok(Some(file)) = store.outbox_file(client_id) {
            return Some(client_provider::MediaData {
                bytes: file.bytes.as_ref().clone(),
                mime_type: Some(file.mime),
            });
        }
        let key = file_key(crate::outbox::local_media_ref(client_id).as_str());
        store
            .media(&key, Timestamp::now())
            .ok()
            .flatten()
            .map(|cached| client_provider::MediaData {
                bytes: cached.bytes,
                mime_type: cached.mime,
            })
    }

    /// Notes the picture id a chat arrived with. When it is not the one
    /// seen before, the picture is worth asking for now, whenever it was
    /// last checked.
    fn note_picture(&self, chat: &Chat) {
        {
            // Along the way: whether the provider counts this chat's
            // unread messages, or leaves that to the client.
            let key = (chat.account_id.clone(), chat.id.clone());
            let mut uncounted = self.inner.uncounted.lock().expect("uncounted lock");
            if chat.unknown.unread {
                uncounted.insert(key);
            } else {
                uncounted.remove(&key);
            }
        }
        if chat.unknown.picture {
            return;
        }
        let key = (chat.account_id.clone(), chat.id.clone());
        let mut hints = self.inner.picture_hints.lock().expect("picture lock");
        let hint = hints.entry(key.clone()).or_default();
        if hint.seen == Some(chat.picture_id.clone()) {
            return;
        }
        let first = hint.seen.is_none();
        hint.seen = Some(chat.picture_id.clone());
        drop(hints);
        if !first {
            self.inner
                .avatar_due
                .lock()
                .expect("avatar lock")
                .remove(&key);
        }
    }

    /// Says that a chat's picture is about to be shown. Cheap enough to
    /// call on every frame for every visible row: the provider is asked at
    /// most once a day per chat (sooner after a failure), a few at a time,
    /// within the same request budget as history preloading, and not while
    /// the account is offline. The picture arrives in the store
    /// ([`StoreChange::Avatar`]).
    pub fn want_avatar(&self, account: &AccountId, subject: &ChatId) {
        if !self.inner.capabilities.avatars || self.is_stopped() {
            return;
        }
        let key = (account.clone(), subject.clone());
        {
            let mut due = self.inner.avatar_due.lock().expect("avatar lock");
            let now = Instant::now();
            if due.get(&key).is_some_and(|at| now < *at) {
                return;
            }
            due.insert(key.clone(), now + AVATAR_RETRY);
        }
        let this = self.clone();
        self.inner.runtime.spawn(async move {
            let next = this.load_avatar(&key.0, &key.1).await;
            let mut due = this.inner.avatar_due.lock().expect("avatar lock");
            due.insert(key, Instant::now() + next);
        });
    }

    /// Makes sure the store's copy of a chat's picture is current.
    /// Returns how long until it is worth looking again.
    pub async fn load_avatar(&self, account: &AccountId, subject: &ChatId) -> Duration {
        let inner = &self.inner;
        let stored = inner.store.avatar(account, subject).ok().flatten();
        let now = Timestamp::now();
        let key = (account.clone(), subject.clone());
        // The chat list says which picture the chat has: when it is the
        // one held here there is nothing to ask, however old the copy,
        // and when it is another one it is asked for now. Without an id
        // (the provider does not give one, or knows of no picture) the
        // picture is checked once a day.
        let hint = inner.store.chat_picture_id(account, subject).ok().flatten();
        let held = stored
            .as_ref()
            .filter(|stored| stored.image.is_some())
            .and_then(|stored| stored.picture_id.clone());
        let asked_already = {
            let hints = inner.picture_hints.lock().expect("picture lock");
            hints
                .get(&key)
                .is_some_and(|known| known.asked.is_some() && known.asked == hint)
        };
        match &hint {
            Some(id) if held.as_ref() == Some(id) => return AVATAR_TTL,
            // Another picture than the one held, not asked about yet.
            Some(_) if !asked_already => {}
            _ => {
                if let Some(stored) = &stored {
                    let age = (now.as_millis() - stored.checked_at.as_millis()).max(0) as u64;
                    if age < AVATAR_TTL.as_millis() as u64 {
                        return AVATAR_TTL - Duration::from_millis(age);
                    }
                }
            }
        }
        // Not while the number is offline: the provider would only say so.
        let connected = inner.store.accounts().is_ok_and(|accounts| {
            accounts
                .iter()
                .any(|a| &a.id == account && a.connection.is_connected())
        });
        if !connected {
            return AVATAR_RETRY;
        }
        let Ok(_slot) = inner.avatar_slots.acquire().await else {
            return AVATAR_RETRY;
        };
        let known = stored.and_then(|stored| stored.picture_id);
        let answer = self
            .paced(|| async {
                let call = inner
                    .provider
                    .fetch_avatar(account, subject, known.as_deref());
                self.bounded(call).await.map_err(SyncError::from)
            })
            .await;
        if answer.is_ok() {
            // Asked, and answered: not again for this id, whatever came.
            let mut hints = inner.picture_hints.lock().expect("picture lock");
            hints.entry(key).or_default().asked = hint.clone();
        }
        let stored = match answer {
            Ok(client_provider::AvatarAnswer::Unchanged) => {
                inner.store.touch_avatar(account, subject, now)
            }
            Ok(client_provider::AvatarAnswer::None) => {
                inner.store.put_avatar(account, subject, None, now)
            }
            Ok(client_provider::AvatarAnswer::New(avatar)) => {
                let bytes = avatar.bytes;
                let small =
                    tokio::task::spawn_blocking(move || crate::thumbnail(&bytes, AVATAR_SIDE))
                        .await;
                match small {
                    Ok(Ok(small)) => inner.store.put_avatar(
                        account,
                        subject,
                        Some((&avatar.id, &small.bytes)),
                        now,
                    ),
                    // Not an image after all: as good as no picture.
                    _ => inner.store.put_avatar(account, subject, None, now),
                }
            }
            Err(SyncError::Provider(error)) => {
                if self.note_unauthorized(&error) || error.is_transient() {
                    return AVATAR_RETRY;
                }
                tracing::debug!(%error, "a picture could not be fetched");
                return AVATAR_RETRY * 60;
            }
            Err(SyncError::Store(error)) => Err(error),
        };
        if let Err(error) = stored {
            tracing::error!(%error, "could not store a picture");
            return AVATAR_RETRY;
        }
        AVATAR_TTL
    }

    /// Says that the image at `url` is about to be shown in a bubble: its
    /// thumbnail is downloaded (images only, at most
    /// [`AUTO_MEDIA_LIMIT`]), made and cached, a few at a time, newest
    /// request first. It arrives in the store ([`StoreChange::Media`],
    /// under [`thumbnail_key`]); what cannot be had is reported by
    /// [`media_state`](Self::media_state).
    pub fn want_thumbnail(&self, account: &AccountId, url: &str) {
        self.want_media(account, url, MediaWant::Thumbnail, false);
    }

    /// [`want_thumbnail`](Self::want_thumbnail) because the user asked
    /// for this one: the picture may be as large as a file the user asks
    /// for ([`MANUAL_MEDIA_LIMIT`]), not only as large as one fetched
    /// unasked.
    pub fn want_thumbnail_asked(&self, account: &AccountId, url: &str) {
        self.want_media(account, url, MediaWant::Thumbnail, true);
    }

    /// Fetches the whole file at `url` because the user asked for it (to
    /// look at an image full size, to open a document): any type, at most
    /// [`MANUAL_MEDIA_LIMIT`]. It arrives under [`file_key`].
    pub fn want_file(&self, account: &AccountId, url: &str) {
        self.want_media(account, url, MediaWant::Whole, true);
    }

    /// Asks whether the image at `url`, whose thumbnail is already
    /// cached, moves: it is fetched again and, when it is an animated
    /// WebP or GIF, kept as it came under [`animation_key`]; otherwise an
    /// empty entry is kept there. An image fetched from now on answers
    /// this with its thumbnail, in one download.
    pub fn want_animation(&self, account: &AccountId, url: &str) {
        self.want_media(account, url, MediaWant::Animation, false);
    }

    fn want_media(&self, account: &AccountId, url: &str, want: MediaWant, asked: bool) {
        if self.is_stopped() {
            return;
        }
        let whole = want == MediaWant::Whole;
        let key = match want {
            MediaWant::Whole => file_key(url),
            MediaWant::Thumbnail => thumbnail_key(url),
            MediaWant::Animation => animation_key(url),
        };
        let mut media = self.inner.media.lock().expect("media lock");
        match media.slots.get(&key) {
            Some(MediaSlot::RetryAt(at)) if Instant::now() >= *at => {}
            Some(_) => return,
            None => {}
        }
        media.slots.insert(key.clone(), MediaSlot::Loading);
        media.waiting.push_front(MediaJob {
            account: account.clone(),
            url: url.to_owned(),
            key,
            whole,
            animation: want == MediaWant::Animation,
            asked,
        });
        // Requests nobody is waiting for any more: rows that scrolled away.
        while media.waiting.len() > MEDIA_QUEUE {
            if let Some(dropped) = media.waiting.pop_back() {
                media.slots.remove(&dropped.key);
            }
        }
        if media.workers < MEDIA_WORKERS {
            media.workers += 1;
            let this = self.clone();
            self.inner
                .runtime
                .spawn(async move { this.run_media().await });
        }
    }

    /// Where the payload under `key` stands, when it is not in the cache.
    pub fn media_state(&self, key: &str) -> MediaState {
        let media = self.inner.media.lock().expect("media lock");
        match media.slots.get(key) {
            Some(MediaSlot::Loading) => MediaState::Loading,
            Some(MediaSlot::Failed(reason)) => MediaState::Unavailable(reason.clone()),
            Some(MediaSlot::Gone(reason)) => MediaState::Expired(reason.clone()),
            Some(MediaSlot::RetryAt(_)) | None => MediaState::Idle,
        }
    }

    /// How many bytes of the payload under `key` have arrived, while it
    /// is being downloaded.
    pub fn media_progress(&self, key: &str) -> Option<u64> {
        self.inner
            .media_progress
            .lock()
            .expect("media lock")
            .get(key)
            .map(|progress| progress.0)
    }

    /// Stops one download: the user changed their mind. One that has not
    /// started is forgotten; one under way is dropped where it stands.
    /// Asking for it again starts over.
    pub fn cancel_media_key(&self, key: &str) {
        {
            let mut media = self.inner.media.lock().expect("media lock");
            if !matches!(media.slots.get(key), Some(MediaSlot::Loading)) {
                return;
            }
            media.slots.remove(key);
            let waiting = media.waiting.len();
            media.waiting.retain(|job| job.key != key);
            if media.waiting.len() == waiting {
                // Not in the queue any more: it is under way.
                media.cancelled.insert(key.to_owned());
            }
        }
        self.inner
            .media_progress
            .lock()
            .expect("media lock")
            .remove(key);
        self.inner.media_cancel.notify_waiters();
        self.inner.store.notify(StoreChange::Media {
            key: key.to_owned(),
        });
    }

    /// Forgets that a download failed, so that asking again tries again:
    /// the user pressed "retry".
    pub fn retry_media(&self, key: &str) {
        let mut media = self.inner.media.lock().expect("media lock");
        if !matches!(
            media.slots.get(key),
            Some(MediaSlot::Loading | MediaSlot::Gone(_))
        ) {
            media.slots.remove(key);
        }
    }

    /// Forgets the downloads that have not started: the chat they were for
    /// was closed. Those in flight finish and are cached.
    pub fn cancel_media(&self) {
        let mut media = self.inner.media.lock().expect("media lock");
        let MediaQueue { waiting, slots, .. } = &mut *media;
        for job in waiting.drain(..) {
            slots.remove(&job.key);
        }
    }

    async fn run_media(self) {
        loop {
            let job = {
                let mut media = self.inner.media.lock().expect("media lock");
                match media.waiting.pop_front() {
                    Some(job) if !self.is_stopped() => job,
                    _ => {
                        media.workers -= 1;
                        return;
                    }
                }
            };
            // Dropped where it stands if the user cancels it meanwhile.
            let outcome = tokio::select! {
                outcome = self.load_media(&job) => Some(outcome),
                _ = self.media_cancelled(&job.key) => None,
            };
            self.inner
                .media_progress
                .lock()
                .expect("media lock")
                .remove(&job.key);
            let Some(outcome) = outcome else {
                continue;
            };
            {
                let mut media = self.inner.media.lock().expect("media lock");
                if media.cancelled.remove(&job.key) {
                    // Cancelled as it finished: what arrived is kept, the
                    // slot stays as the cancel left it.
                    continue;
                }
                match outcome {
                    Err(MediaFailure::Gone(reason)) => {
                        media.slots.insert(job.key.clone(), MediaSlot::Gone(reason))
                    }
                    Ok(()) => media.slots.remove(&job.key),
                    Err(MediaFailure::Later) => media.slots.insert(
                        job.key.clone(),
                        MediaSlot::RetryAt(Instant::now() + MEDIA_RETRY),
                    ),
                    Err(MediaFailure::Never(reason)) => media
                        .slots
                        .insert(job.key.clone(), MediaSlot::Failed(reason)),
                };
            }
            // Either way the view has something new to show.
            self.inner.store.notify(StoreChange::Media { key: job.key });
        }
    }

    /// Resolves when the download under `key` is cancelled.
    async fn media_cancelled(&self, key: &str) {
        loop {
            let cancelled = self.inner.media_cancel.notified();
            if self
                .inner
                .media
                .lock()
                .expect("media lock")
                .cancelled
                .remove(key)
            {
                return;
            }
            cancelled.await;
        }
    }

    /// Records how much of a download has arrived, and tells the view
    /// now and then.
    fn note_download(&self, key: &str, received: u64) {
        let tell = {
            let mut all = self.inner.media_progress.lock().expect("media lock");
            let entry = all.entry(key.to_owned()).or_insert((0, None));
            entry.0 = received;
            let due = entry
                .1
                .is_none_or(|told: Instant| told.elapsed() >= Duration::from_millis(150));
            if due {
                entry.1 = Some(Instant::now());
            }
            due
        };
        if tell {
            self.inner.store.notify(StoreChange::Media {
                key: key.to_owned(),
            });
        }
    }

    async fn load_media(&self, job: &MediaJob) -> Result<(), MediaFailure> {
        let inner = &self.inner;
        if matches!(inner.store.media_size(&job.key), Ok(Some(_)))
            || matches!(inner.store.media(&job.key, Timestamp::now()), Ok(Some(_)))
        {
            return Ok(());
        }
        let limit = client_provider::MediaLimit {
            max_bytes: if job.whole || job.asked {
                MANUAL_MEDIA_LIMIT
            } else {
                AUTO_MEDIA_LIMIT
            },
            images_only: !job.whole,
        };
        let media = client_provider::MediaRef::new(job.url.clone());
        // A file of the user's own, queued or sent from here: the copy is
        // in the store, and no provider knows the reference.
        let local = job
            .url
            .strip_prefix(crate::outbox::LOCAL_MEDIA)
            .map(|client_id| self.local_media(&ClientMessageId::new(client_id)));
        let fetched = match local {
            Some(Some(data)) => Ok(Ok(data)),
            Some(None) => {
                return Err(MediaFailure::Never(
                    "The file is no longer on this computer.".to_owned(),
                ))
            }
            None => {
                let (this, key) = (self.clone(), job.key.clone());
                let progress: client_provider::UploadProgress =
                    std::sync::Arc::new(move |received| this.note_download(&key, received));
                let fetch =
                    inner
                        .provider
                        .fetch_media_reporting(&job.account, &media, limit, progress);
                tokio::time::timeout(MEDIA_TIMEOUT, fetch).await
            }
        };
        let data = match fetched {
            Ok(Ok(data)) => data,
            Ok(Err(error)) => {
                if self.note_unauthorized(&error) || error.is_transient() {
                    return Err(MediaFailure::Later);
                }
                // WhatsApp no longer has the file: nothing to retry.
                if matches!(&error, ProviderError::Rejected { code, .. } if code == "media_expired")
                {
                    return Err(MediaFailure::Gone(error.to_string()));
                }
                return Err(MediaFailure::Never(error.to_string()));
            }
            Err(_) => return Err(MediaFailure::Later),
        };
        let put = |key: &str, cached: &crate::CachedMedia| {
            inner
                .store
                .put_media(key, cached, Timestamp::now(), inner.config.media_budget)
                .map_err(|error| MediaFailure::Never(error.to_string()))
        };
        if job.whole {
            let cached = crate::CachedMedia {
                bytes: data.bytes,
                mime: data.mime_type,
                size: None,
            };
            put(&job.key, &cached)?;
            return Ok(());
        }
        // An image: its thumbnail, and whether it moves. An animated
        // sticker or GIF is kept as it came, next to its first frame.
        let bytes = data.bytes;
        let read = tokio::task::spawn_blocking(move || {
            let small = crate::thumbnail(&bytes, THUMBNAIL_SIDE);
            let moving = crate::animated_format(&bytes)
                .filter(|_| bytes.len() <= ANIMATION_KEEP_LIMIT)
                .map(|format| (format, bytes));
            (small, moving)
        })
        .await;
        let Ok((Ok(small), moving)) = read else {
            return Err(MediaFailure::Never(
                "The image could not be read.".to_owned(),
            ));
        };
        let thumbnail = crate::CachedMedia {
            size: Some((small.width, small.height)),
            mime: Some(small.mime.to_owned()),
            bytes: small.bytes,
        };
        let animation = match moving {
            Some((format, bytes)) => crate::CachedMedia {
                bytes,
                mime: Some(format.mime().to_owned()),
                size: None,
            },
            // It does not move: remembered, so it is not asked again.
            None => crate::CachedMedia {
                bytes: Vec::new(),
                mime: None,
                size: None,
            },
        };
        let thumbnail_key = thumbnail_key(&job.url);
        if !job.animation || !matches!(inner.store.media_size(&thumbnail_key), Ok(Some(_))) {
            put(&thumbnail_key, &thumbnail)?;
        }
        put(&animation_key(&job.url), &animation)?;
        Ok(())
    }
}

enum MediaFailure {
    /// Try again in a while.
    Later,
    /// Not this one, with the reason.
    Never(String),
    /// Not ever: it no longer exists.
    Gone(String),
}

/// How long after the provider accepted a change the user's value still
/// wins over a chat list that says otherwise.
const PENDING_GRACE: Duration = Duration::from_secs(120);

/// A value the user set, waiting for the provider to agree.
#[derive(Clone, Copy)]
struct PendingState {
    value: bool,
    /// When the provider accepted the change. `None`: still being sent.
    accepted_at: Option<Instant>,
}

/// The value a change sets, for the changes that set one.
fn value_of(change: ChatChange) -> Option<bool> {
    match change {
        ChatChange::Pinned(on) | ChatChange::Muted(on) | ChatChange::Archived(on) => Some(on),
        ChatChange::MutedFor(_) => Some(true),
        ChatChange::MarkedUnread => None,
    }
}

/// Which state a change touches: changes of the same kind replace each
/// other, changes of different kinds do not.
fn change_kind(change: ChatChange) -> u8 {
    match change {
        ChatChange::Pinned(_) => 0,
        ChatChange::Muted(_) | ChatChange::MutedFor(_) => 1,
        ChatChange::Archived(_) => 2,
        ChatChange::MarkedUnread => 3,
    }
}

/// The change that puts the previous state back, when there is one.
fn undo_of(change: ChatChange) -> Option<ChatChange> {
    match change {
        ChatChange::Pinned(on) => Some(ChatChange::Pinned(!on)),
        ChatChange::Muted(on) => Some(ChatChange::Muted(!on)),
        ChatChange::MutedFor(_) => Some(ChatChange::Muted(false)),
        ChatChange::Archived(on) => Some(ChatChange::Archived(!on)),
        ChatChange::MarkedUnread => None,
    }
}

/// A fresh, globally unique client message id.
pub fn new_client_id() -> ClientMessageId {
    ClientMessageId::new(uuid::Uuid::new_v4().simple().to_string())
}

#[cfg(test)]
mod live_updates_tests {
    use super::*;
    use async_trait::async_trait;
    use client_provider::{
        Account, Capabilities, Chat, ChatChange, Cursor, EventStream, LiveUpdates, MediaData,
        MediaRef, Message, OutgoingMessage, Page, PollingReason, Provider, ProviderResult,
        SendReceipt,
    };
    use provider_mock::MockProvider;

    /// The mock, saying how its updates arrive.
    struct Saying(MockProvider, Option<LiveUpdates>);

    #[async_trait]
    impl Provider for Saying {
        fn id(&self) -> &'static str {
            "saying"
        }
        fn capabilities(&self) -> Capabilities {
            self.0.capabilities()
        }
        fn live_updates(&self) -> Option<LiveUpdates> {
            self.1
        }
        async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
            self.0.list_accounts().await
        }
        async fn list_chats(&self, a: &AccountId, c: Option<Cursor>) -> ProviderResult<Page<Chat>> {
            self.0.list_chats(a, c).await
        }
        async fn fetch_messages(
            &self,
            a: &AccountId,
            c: &ChatId,
            k: Option<Cursor>,
            n: u32,
        ) -> ProviderResult<Page<Message>> {
            self.0.fetch_messages(a, c, k, n).await
        }
        async fn send(&self, m: OutgoingMessage) -> ProviderResult<SendReceipt> {
            self.0.send(m).await
        }
        async fn mark_read(
            &self,
            a: &AccountId,
            c: &ChatId,
            up_to: Option<&MessageId>,
        ) -> ProviderResult<()> {
            self.0.mark_read(a, c, up_to).await
        }
        async fn update_chat(
            &self,
            a: &AccountId,
            c: &ChatId,
            change: ChatChange,
        ) -> ProviderResult<()> {
            self.0.update_chat(a, c, change).await
        }
        async fn download_media(&self, a: &AccountId, m: &MediaRef) -> ProviderResult<MediaData> {
            self.0.download_media(a, m).await
        }
        async fn subscribe(&self) -> ProviderResult<EventStream> {
            self.0.subscribe().await
        }
    }

    fn engine_on(live: Option<LiveUpdates>) -> SyncEngine {
        SyncEngine::new(
            Arc::new(Store::open_in_memory().unwrap()),
            Arc::new(Saying(MockProvider::default(), live)),
            SyncConfig::default(),
            Handle::current(),
        )
    }

    #[tokio::test]
    async fn engine_passes_live_updates_through() {
        // Whatever the provider says is what the diagnostics get, as it
        // changes.
        for said in [
            Some(LiveUpdates::Stream),
            Some(LiveUpdates::Reconnecting),
            Some(LiveUpdates::Polling(PollingReason::Failing)),
            None,
        ] {
            assert_eq!(engine_on(said).live_updates(), said);
        }
    }
}

/// How the event loop paces itself when the provider cannot be subscribed to
/// while the rest of the API is healthy (a refresh that works must not wipe
/// the memory of the failed subscribes).
#[cfg(test)]
mod subscribe_pacing_tests {
    use super::*;
    use async_trait::async_trait;
    use client_provider::{
        Account, Capabilities, Chat, ChatChange, Cursor, EventStream, MediaData, MediaRef,
        OutgoingMessage, Page, ProviderResult, SendReceipt,
    };
    use provider_mock::MockProvider;

    /// What `subscribe` answers, call by call; the last answer repeats.
    type Answers = Vec<Result<(), ProviderError>>;

    /// The mock, whose `subscribe` is scripted and whose calls are logged.
    struct Scripted {
        inner: MockProvider,
        answers: Mutex<Answers>,
        subscribes: Arc<Mutex<Vec<tokio::time::Instant>>>,
        refreshes: Arc<Mutex<Vec<tokio::time::Instant>>>,
    }

    #[async_trait]
    impl Provider for Scripted {
        fn id(&self) -> &'static str {
            "scripted"
        }
        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }
        async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
            self.refreshes
                .lock()
                .unwrap()
                .push(tokio::time::Instant::now());
            self.inner.list_accounts().await
        }
        async fn list_chats(&self, a: &AccountId, c: Option<Cursor>) -> ProviderResult<Page<Chat>> {
            self.inner.list_chats(a, c).await
        }
        async fn fetch_messages(
            &self,
            a: &AccountId,
            c: &ChatId,
            k: Option<Cursor>,
            n: u32,
        ) -> ProviderResult<Page<Message>> {
            self.inner.fetch_messages(a, c, k, n).await
        }
        async fn send(&self, m: OutgoingMessage) -> ProviderResult<SendReceipt> {
            self.inner.send(m).await
        }
        async fn mark_read(
            &self,
            a: &AccountId,
            c: &ChatId,
            up_to: Option<&MessageId>,
        ) -> ProviderResult<()> {
            self.inner.mark_read(a, c, up_to).await
        }
        async fn update_chat(
            &self,
            a: &AccountId,
            c: &ChatId,
            change: ChatChange,
        ) -> ProviderResult<()> {
            self.inner.update_chat(a, c, change).await
        }
        async fn download_media(&self, a: &AccountId, m: &MediaRef) -> ProviderResult<MediaData> {
            self.inner.download_media(a, m).await
        }
        async fn subscribe(&self) -> ProviderResult<EventStream> {
            self.subscribes
                .lock()
                .unwrap()
                .push(tokio::time::Instant::now());
            let mut answers = self.answers.lock().unwrap();
            let answer = if answers.len() > 1 {
                answers.remove(0)
            } else {
                answers[0].clone()
            };
            // A stream that has nothing to say and ends at once.
            answer.map(|()| futures::stream::empty().boxed())
        }
    }

    struct Run {
        engine: SyncEngine,
        subscribes: Arc<Mutex<Vec<tokio::time::Instant>>>,
        refreshes: Arc<Mutex<Vec<tokio::time::Instant>>>,
        started: tokio::time::Instant,
    }

    impl Run {
        /// The seconds, from the start, of each subscribe.
        fn subscribe_secs(&self) -> Vec<u64> {
            self.subscribes
                .lock()
                .unwrap()
                .iter()
                .map(|at| at.duration_since(self.started).as_secs())
                .collect()
        }
    }

    /// The default pacing, a provider whose subscribe answers as scripted
    /// and whose REST side (the mock) is healthy.
    fn run(answers: Answers) -> Run {
        let subscribes = Arc::new(Mutex::new(Vec::new()));
        let refreshes = Arc::new(Mutex::new(Vec::new()));
        let provider = Scripted {
            inner: MockProvider::default(),
            answers: Mutex::new(answers),
            subscribes: subscribes.clone(),
            refreshes: refreshes.clone(),
        };
        let engine = SyncEngine::new(
            Arc::new(Store::open_in_memory().unwrap()),
            Arc::new(provider),
            SyncConfig::default(),
            Handle::current(),
        );
        let started = tokio::time::Instant::now();
        engine.start();
        Run {
            engine,
            subscribes,
            refreshes,
            started,
        }
    }

    fn transient() -> ProviderError {
        ProviderError::Transient("the stream is down".into())
    }

    fn limited(seconds: Option<u64>) -> ProviderError {
        ProviderError::RateLimited {
            retry_after: seconds.map(Duration::from_secs),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_refuses_is_asked_at_most_six_times_a_minute() {
        // Every subscribe fails, the REST side is fine: the refresh that
        // follows each attempt works and must not reset the pacing.
        let run = run(vec![Err(transient())]);
        tokio::time::sleep(Duration::from_secs(60)).await;
        let secs = run.subscribe_secs();
        assert!(secs.len() <= 6, "subscribes at seconds {secs:?}");
        // Triangulation: it does go on asking, with growing gaps.
        tokio::time::sleep(Duration::from_secs(240)).await;
        let later = run.subscribe_secs();
        assert!(later.len() > secs.len(), "{later:?}");
        // Any seven in a row span a full minute: at most six per rolling
        // minute.
        for seven in later.windows(7) {
            assert!(seven[6] - seven[0] >= 60, "subscribes at seconds {later:?}");
        }
        run.engine.shutdown();
    }

    #[tokio::test(start_paused = true)]
    async fn a_stream_that_refuses_costs_one_refresh_per_attempt_not_a_storm() {
        let run = run(vec![Err(transient())]);
        tokio::time::sleep(Duration::from_secs(60)).await;
        let refreshes = run.refreshes.lock().unwrap().len();
        let subscribes = run.subscribes.lock().unwrap().len();
        assert!(subscribes >= 2, "the loop ran: {subscribes}");
        assert!(refreshes <= 6, "{refreshes} refreshes in a minute");
        run.engine.shutdown();
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_waits_at_least_what_the_server_said() {
        let run = run(vec![Err(limited(Some(30)))]);
        tokio::time::sleep(Duration::from_secs(29)).await;
        assert_eq!(run.subscribe_secs(), [0], "nothing before 30 s");
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert_eq!(run.subscribe_secs(), [0, 30]);
        run.engine.shutdown();
    }

    #[tokio::test(start_paused = true)]
    async fn rate_limited_without_a_wait_uses_the_engines_own_backoff() {
        let run = run(vec![Err(limited(None))]);
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert_eq!(run.subscribe_secs(), [0, 1], "1 s, then 2 s");
        run.engine.shutdown();
    }

    #[tokio::test(start_paused = true)]
    async fn a_huge_retry_after_is_clamped() {
        let run = run(vec![Err(limited(Some(86_400)))]);
        tokio::time::sleep(Duration::from_secs(301)).await;
        assert_eq!(run.subscribe_secs(), [0, 300]);
        run.engine.shutdown();
    }

    #[tokio::test(start_paused = true)]
    async fn a_subscribe_that_works_forgets_the_failures_before_it() {
        // Three failures, one stream that ends, then failures again: the
        // wait after the first of the new failures is the first step.
        let run = run(vec![
            Err(transient()),
            Err(transient()),
            Err(transient()),
            Ok(()),
            Err(transient()),
        ]);
        // 0, +1, +2, +4 = 7 s: the stream that works ends at once; +1 s;
        // then the failure, then +1 s.
        tokio::time::sleep(Duration::from_millis(9500)).await;
        assert_eq!(run.subscribe_secs(), [0, 1, 3, 7, 8, 9]);
        run.engine.shutdown();
    }
}
