//! Live updates.
//!
//! wuapi Streams pushes events over a connection the client opens itself
//! (server-sent events, see `stream.rs`), so a desktop application hears
//! them without a public endpoint. [`EventSource`] is the seam both
//! transports implement: [`PollingEventSource`] here, `StreamEventSource`
//! and `LiveEventSource` for the stream. Polling stays for what the stream
//! does not carry: it is the fallback when a deployment has no stream, and
//! in `Auto` mode it runs as a slow safety poll beside the stream.
//!
//! Polling has two parts. The general poll reads the accounts, the newest
//! page of messages and the most recently active chats, every
//! `poll_interval`, and the stories now and then (the stream carries
//! the story events; polling needs them where there is no stream). The follow-up (`follow.rs`) asks again, sooner and by
//! id, about the outgoing messages whose ticks can still move: a message
//! just sent is not left waiting for the next general poll, and one that
//! newer messages pushed off the newest page is not forgotten. While the
//! stream is live the second part is unnecessary: `message.sent`,
//! `message.delivered` and `message.read` arrive by themselves, so the
//! follow-ups run only when the stream is not.

use crate::client::{WuapiClient, MAX_PAGE};
use crate::follow::{version, Fetch, Follow, Now, WATCH_FOR};
use crate::mapping;
use async_trait::async_trait;
use client_provider::{
    AccountId, Chat, ConnectionState, EventStream, Feature, MessageId, ProviderError,
    ProviderEvent, ProviderResult, Story, Timestamp,
};
use futures::StreamExt;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio::time::Instant;
use wuapi::types as api;

/// Where live updates come from.
#[async_trait]
pub(crate) trait EventSource: Send + Sync {
    /// Opens a stream of updates starting now.
    async fn open(&self) -> ProviderResult<EventStream>;

    /// The API accepted a message sent from here, and answered this. A
    /// source that is pushed every change of a message has nothing to do;
    /// one that has to ask starts asking about this message.
    fn accepted(&self, _message: &api::Message) {}

    /// These messages were just read as history and handed to the client.
    /// Again only of interest to a source that has to ask: the ones whose
    /// ticks can still move are worth asking about later.
    fn listed(&self, _messages: &[api::Message]) {}

    /// How events reach the client right now, for a source that knows.
    fn live(&self) -> Option<crate::live::Status> {
        None
    }
}

/// How many message versions the poller remembers.
const MAX_SEEN: usize = 5_000;

/// How many pages one poll reads when everything it sees is new.
const MAX_CATCH_UP_PAGES: usize = 3;

/// How many accounts' chats are read at once.
const CHAT_POLL_CONCURRENCY: usize = 4;

/// How many chats one poll reads per account: the most recently active
/// ones. Changes to a chat further down (read on the phone, pinned, muted)
/// show up at the next full refresh.
pub(crate) const CHAT_POLL_PAGE: u32 = 50;

/// Every this many polls, the chats of every account are read, not only of
/// the accounts with new messages: that is how changes that come without a
/// message (read on the phone, pinned, muted, archived) are noticed.
const CHAT_SWEEP_POLLS: u32 = 4;

/// Every this many polls the stories of every connected account are read:
/// its contacts' (which carry no message the general poll would see) and
/// its own (whose view counts move). Two small requests per account, of
/// stored rows: nothing is asked of WhatsApp and nobody is told anything.
/// Not read at all while the deployment is known not to have the routes.
/// At the default interval that is about every half minute, which is how
/// late a contact's new story can be until the API pushes its events.
pub(crate) const STORY_SWEEP_POLLS: u32 = 8;

/// What the poller has already reported, so that each poll yields only the
/// difference. Pure state: no I/O, fully unit-tested.
#[derive(Default)]
pub(crate) struct PollState {
    connections: HashMap<String, ConnectionState>,
    /// Message id -> the version last reported.
    seen: HashMap<String, String>,
    /// Ids in the order they were first seen, to evict the oldest.
    seen_order: VecDeque<String>,
    /// Messages a priming poll recorded in `seen` and dropped as history:
    /// recorded, but never said. A push for one of them is news, because
    /// the client was never told, and it is said once.
    silent: HashSet<String>,
    /// The chats as last reported, without their last message, by
    /// (account, chat).
    known_chats: HashMap<(String, String), Chat>,
    /// The id of each known chat's last message.
    last_messages: HashMap<(String, String), client_provider::MessageId>,
    /// The stories as last reported, by (account, story).
    stories: HashMap<(String, String), Story>,
    /// Whether each author's stories were muted when last read, by
    /// (account, author).
    story_mutes: HashMap<(String, String), bool>,
}

impl PollState {
    /// Records the accounts' states and returns a `ConnectionChanged` for
    /// each one that differs from the last poll.
    pub(crate) fn observe_accounts(&mut self, accounts: &[api::Account]) -> Vec<ProviderEvent> {
        let mut events = Vec::new();
        for account in accounts {
            let state = mapping::connection(account);
            let previous = self.connections.insert(account.id.clone(), state.clone());
            if previous.as_ref() != Some(&state) {
                events.push(ProviderEvent::ConnectionChanged {
                    account_id: AccountId::new(account.id.clone()),
                    state,
                });
            }
        }
        events
    }

    /// Records a page of chats and returns a `ChatUpdated` for each one
    /// that is new or whose state (name, unread count, pin, mute, archive,
    /// last message) differs from the last poll. The provider's values win:
    /// the client stores the unread count it is given.
    pub(crate) fn observe_chats(&mut self, page: &[api::Chat]) -> Vec<ProviderEvent> {
        let mut events = Vec::new();
        for chat in page.iter().filter_map(mapping::chat) {
            let key = (chat.account_id.to_string(), chat.id.to_string());
            // Compared without the message, which has its own events; its
            // id is enough to tell that the chat moved.
            let mut state = chat.clone();
            let last = state.last_message.take().map(|m| m.id);
            let previous = self.known_chats.get(&key);
            if previous != Some(&state) || self.last_messages.get(&key) != last.as_ref() {
                self.known_chats.insert(key.clone(), state);
                match last {
                    Some(id) => self.last_messages.insert(key, id),
                    None => self.last_messages.remove(&key),
                };
                events.push(ProviderEvent::ChatUpdated(chat));
            }
        }
        events
    }

    /// Records a message the stream pushed and returns its event, unless
    /// the client has been told this version already (by a poll, or by an
    /// earlier push) or a newer one: a replay can arrive behind a poll that
    /// has seen more. `updatedAt` is an ISO 8601 time in UTC with fixed
    /// width, which orders as text.
    pub(crate) fn observe_pushed(&mut self, wire: &api::Message) -> Option<ProviderEvent> {
        let version = version(wire);
        let silent = self.silent.contains(&wire.id);
        match self.seen.insert(wire.id.clone(), version.clone()) {
            Some(previous) if previous == version && !silent => return None,
            // Seen only while priming, never said: say it.
            Some(previous) if previous == version => {}
            Some(previous) => {
                let seen_at = previous.split('|').next().unwrap_or_default();
                if wire.updated_at.as_str() < seen_at {
                    // Put back what was known: this is not news.
                    self.seen.insert(wire.id.clone(), previous);
                    return None;
                }
            }
            None => self.seen_order.push_back(wire.id.clone()),
        }
        self.silent.remove(&wire.id);
        self.evict_seen();
        mapping::message(wire).map(ProviderEvent::MessageUpserted)
    }

    /// Notes that these messages were recorded but not said (a priming poll
    /// dropped them as history).
    pub(crate) fn recorded_silently(&mut self, ids: impl IntoIterator<Item = String>) {
        self.silent.extend(ids);
    }

    /// Records a story the stream pushed and returns its event unless the
    /// client has it as it is.
    pub(crate) fn observe_story(&mut self, story: Story) -> Option<ProviderEvent> {
        let key = (story.account_id.to_string(), story.id.to_string());
        if self.stories.get(&key) == Some(&story) {
            return None;
        }
        self.stories.insert(key, story.clone());
        Some(ProviderEvent::StoryUpserted(story))
    }

    /// Forgets a story the stream says is gone. The event is returned even
    /// for one never seen: the client may have it from a list.
    pub(crate) fn forget_story(&mut self, account: &str, story: &str) -> ProviderEvent {
        self.stories.remove(&(account.to_owned(), story.to_owned()));
        ProviderEvent::StoryRemoved {
            account_id: AccountId::new(account),
            story_id: MessageId::new(story),
        }
    }

    fn evict_seen(&mut self) {
        while self.seen_order.len() > MAX_SEEN {
            if let Some(oldest) = self.seen_order.pop_front() {
                self.seen.remove(&oldest);
                self.silent.remove(&oldest);
            }
        }
    }

    /// True when the poller has not reported this version of the message.
    fn is_news(&self, message: &api::Message) -> bool {
        self.seen.get(&message.id) != Some(&version(message))
    }

    /// Records the stories of one account as they were just listed and
    /// returns what changed since the last time: a story that is new or
    /// differs (seen on the phone, another view of an own one) is
    /// upserted, one that is no longer listed is removed. An author muted
    /// on the phone is said the first time, and after that whenever it
    /// changes; "not muted" is never said first, because a mute made in
    /// the client cannot be written to the API and must not be undone by
    /// its silence.
    pub(crate) fn observe_stories(
        &mut self,
        account: &str,
        listed: &crate::stories::Listed,
    ) -> Vec<ProviderEvent> {
        let mut events = Vec::new();
        let mut present = HashSet::new();
        for story in &listed.stories {
            let key = (account.to_owned(), story.id.to_string());
            present.insert(story.id.to_string());
            if self.stories.get(&key) != Some(story) {
                self.stories.insert(key, story.clone());
                events.push(ProviderEvent::StoryUpserted(story.clone()));
            }
        }
        let gone: Vec<(String, String)> = self
            .stories
            .keys()
            .filter(|(owner, id)| owner == account && !present.contains(id))
            .cloned()
            .collect();
        for key in gone {
            self.stories.remove(&key);
            events.push(ProviderEvent::StoryRemoved {
                account_id: AccountId::new(account),
                story_id: MessageId::new(key.1),
            });
        }
        for (contact, muted) in &listed.muted {
            let key = (account.to_owned(), contact.to_string());
            let before = self.story_mutes.insert(key, *muted);
            if before != Some(*muted) && (before.is_some() || *muted) {
                events.push(ProviderEvent::StoryMuteChanged {
                    account_id: AccountId::new(account),
                    contact: contact.clone(),
                    muted: *muted,
                });
            }
        }
        events
    }

    /// Records a page of messages (newest first, as the API returns it) and
    /// returns the events for what is new or changed, oldest first.
    ///
    /// The second value is `true` when every message of the page was news,
    /// which means the next page may hold more.
    pub(crate) fn observe_messages(&mut self, page: &[api::Message]) -> (Vec<ProviderEvent>, bool) {
        let mut events = Vec::new();
        let mut all_new = !page.is_empty();
        for wire in page.iter().rev() {
            let version = version(wire);
            match self.seen.insert(wire.id.clone(), version.clone()) {
                Some(previous) if previous == version => {
                    all_new = false;
                    continue;
                }
                Some(_) => all_new = false,
                None => self.seen_order.push_back(wire.id.clone()),
            }
            let Some(message) = mapping::message(wire) else {
                continue;
            };
            // Said now: no longer only recorded.
            self.silent.remove(&wire.id);
            events.push(ProviderEvent::MessageUpserted(message));
        }
        self.evict_seen();
        (events, all_new)
    }
}

/// Polls the accounts, the newest messages and the most recently active
/// chats on a fixed interval, and in between asks about the outgoing
/// messages whose ticks can still move (see `follow.rs`).
///
/// The chats of an account are read when it has new messages, and of every
/// account each [`CHAT_SWEEP_POLLS`] polls: one small request per account
/// that is `ready`, several accounts at a time, which is what carries
/// unread counts, pins, mutes and names. An account that is not linked or
/// not connected is not asked: its chats cannot have changed, so it costs
/// a poll nothing.
///
/// Limits, all consequences of polling a listing without a `since` filter:
/// there is no presence, incoming messages are up to one interval late,
/// and so is an edit or a deletion of a message newer ones have pushed off
/// the newest page (until the client re-reads that chat). The status of an
/// outgoing message is not among them: those are followed by id.
pub(crate) struct PollingEventSource {
    client: Arc<WuapiClient>,
    interval: Duration,
    /// What is followed, shared with every stream this source opens: a
    /// send made while no stream is open is still followed by the next.
    shared: Arc<Shared>,
}

/// The messages under watch, and the bell that says one was added.
#[derive(Default)]
pub(crate) struct Shared {
    follow: Mutex<Follow>,
    wake: Notify,
}

impl Shared {
    /// Completes when a message was accepted: a follow-up may be due before
    /// anything that was planned.
    pub(crate) async fn rung(&self) {
        self.wake.notified().await;
    }

    pub(crate) fn follow(&self) -> std::sync::MutexGuard<'_, Follow> {
        // A panic while holding it leaves nothing half-done worth losing
        // the stream for.
        self.follow
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl PollingEventSource {
    pub(crate) fn new(client: Arc<WuapiClient>, interval: Duration) -> Self {
        Self {
            client,
            interval,
            shared: Arc::default(),
        }
    }

    /// A poller on this source's watch list, not started: [`open`]
    /// (EventSource::open) drives one by the clock, tests step by step.
    pub(crate) fn poller(&self) -> Poller {
        self.poller_with(Arc::default())
    }

    /// Like [`poller`](Self::poller), over a table of what was said that
    /// somebody else writes to as well.
    pub(crate) fn poller_with(&self, state: Arc<Mutex<PollState>>) -> Poller {
        Poller {
            client: self.client.clone(),
            interval: self.interval,
            state,
            queue: VecDeque::new(),
            primed: false,
            until_sweep: 0,
            until_stories: 0,
            shared: self.shared.clone(),
            next_poll: Instant::now(),
            owed_chats: HashSet::new(),
            owed_stories: HashSet::new(),
        }
    }

    /// What is followed, for the stream's mapper to retire from.
    pub(crate) fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// How many messages are being followed.
    #[cfg(test)]
    pub(crate) fn following(&self) -> usize {
        self.shared.follow().len()
    }
}

pub(crate) struct Poller {
    client: Arc<WuapiClient>,
    interval: Duration,
    /// Shared with the stream's mapper when there is one: one table
    /// answers "was this version said".
    state: Arc<Mutex<PollState>>,
    queue: VecDeque<ProviderEvent>,
    /// Set once the first poll succeeded.
    primed: bool,
    /// Polls left until the next one that reads every account's chats.
    until_sweep: u32,
    /// Polls left until the next one that reads the stories.
    until_stories: u32,
    shared: Arc<Shared>,
    /// When the next general poll is due.
    next_poll: Instant,
    /// Accounts whose chats could not be read when they were wanted: they
    /// are asked again at the next poll.
    owed_chats: HashSet<String>,
    /// Accounts whose stories could not be read when they were wanted:
    /// they are asked again at the next poll, not a sweep later.
    owed_stories: HashSet<String>,
}

impl Poller {
    fn state(&self) -> std::sync::MutexGuard<'_, PollState> {
        // A panic while holding it leaves nothing half-done worth losing
        // the stream for.
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// One general poll. Returns the events it found, in order.
    ///
    /// The accounts and the messages are fetched before anything is
    /// recorded, so a request that fails there loses nothing: the next
    /// poll sees the same difference again.
    pub(crate) async fn poll(&mut self, now: Now) -> ProviderResult<Vec<ProviderEvent>> {
        let accounts = self.client.accounts().await?;

        let mut pages = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_CATCH_UP_PAGES {
            let page = self
                .client
                .messages(None, None, cursor.as_deref(), MAX_PAGE)
                .await?;
            cursor = page.next_cursor;
            // Peek without recording: is this whole page unknown?
            let all_new = page
                .items
                .iter()
                .all(|m| !self.state().seen.contains_key(&m.id));
            pages.push(page.items);
            // While priming everything is unknown by definition, and none
            // of it is news: one page is enough to know where "now" is.
            if !self.primed || !all_new || cursor.is_none() {
                break;
            }
        }

        let chat_pages = self.poll_chats(&accounts, &pages).await?;

        let story_lists = self.poll_stories(&accounts, now).await?;

        // Nothing below can fail. Chats come before their messages.
        let mut events = self.state().observe_accounts(&accounts);
        for page in &chat_pages {
            events.extend(self.state().observe_chats(page));
        }
        // Oldest page first, so events come out in chronological order.
        for page in pages.iter().rev() {
            events.extend(self.digest(page, now));
        }
        if !self.primed {
            // What exists when the stream opens is history, which the
            // client reads through `list_chats` / `fetch_messages`. All
            // but one thing: the ticks of the account's own recent
            // messages may have moved while nobody was listening, and
            // history is not read again for that. They are said once.
            let mut dropped = Vec::new();
            events.retain(|event| {
                let keep = matches!(event, ProviderEvent::MessageUpserted(message)
                    if message.direction == client_provider::Direction::Outgoing
                        && now.epoch_ms - message.timestamp.as_millis()
                            < WATCH_FOR.as_millis() as i64);
                if let (false, ProviderEvent::MessageUpserted(message)) = (keep, event) {
                    dropped.push(message.id.to_string());
                }
                keep
            });
            // Recorded as seen, not said: a push for one is still news.
            self.state().recorded_silently(dropped);
        }
        // Stories are said in full the first time: nothing else brings
        // the ones that were up before the stream opened, and repeating
        // what the client has changes nothing.
        for (account, listed) in &story_lists {
            events.extend(self.state().observe_stories(account, listed));
        }
        self.primed = true;
        self.until_sweep = match self.until_sweep {
            0 => CHAT_SWEEP_POLLS - 1,
            left => left - 1,
        };
        self.until_stories = match self.until_stories {
            0 => STORY_SWEEP_POLLS - 1,
            left => left - 1,
        };
        Ok(events)
    }

    /// Records messages just read from the API (newest first, as it
    /// returns them) and returns the events for what the client has not
    /// been told: what is new or changed since the last poll, and what a
    /// followed message has become.
    fn digest(&mut self, page: &[api::Message], now: Now) -> Vec<ProviderEvent> {
        let (mut events, _) = self.state().observe_messages(page);
        // For a followed message, what the client was last told decides,
        // not what this stream happens to have seen: the answer to its
        // send told it the first version, and a stream opened later has
        // seen none.
        let mut known = HashSet::new();
        let mut owed = HashMap::new();
        {
            let mut follow = self.shared.follow();
            for wire in page.iter().rev() {
                let followed = follow.watches(&wire.id);
                if follow.observe(wire, now) {
                    owed.insert(wire.id.as_str(), wire);
                } else if followed {
                    known.insert(wire.id.as_str());
                }
            }
        }
        events.retain(|event| match event {
            ProviderEvent::MessageUpserted(message) => {
                owed.remove(message.id.as_str());
                !known.contains(message.id.as_str())
            }
            _ => true,
        });
        // Oldest first, like the rest.
        for wire in page.iter().rev() {
            if owed.contains_key(wire.id.as_str()) {
                events.extend(mapping::message(wire).map(ProviderEvent::MessageUpserted));
            }
        }
        events
    }

    /// The newest chats of the accounts worth asking about in this poll:
    /// those with news among `pages`, and all of them on a sweep. Only
    /// accounts that are `ready`, several at a time. An account whose
    /// chats cannot be read does not hold up the others, nor the
    /// messages: it is asked again at the next poll.
    async fn poll_chats(
        &mut self,
        accounts: &[api::Account],
        pages: &[Vec<api::Message>],
    ) -> ProviderResult<Vec<Vec<api::Chat>>> {
        let sweep = self.until_sweep == 0;
        let with_news: HashSet<String> = {
            let state = self.state();
            pages
                .iter()
                .flatten()
                .filter(|message| state.is_news(message))
                .map(|message| message.account_id.clone())
                .collect()
        };
        let wanted: Vec<String> = accounts
            .iter()
            .filter(|account| account.status == api::AccountStatus::Ready)
            .filter(|account| {
                sweep || with_news.contains(&account.id) || self.owed_chats.contains(&account.id)
            })
            .map(|account| account.id.clone())
            .collect();
        let client = self.client.clone();
        let answers: Vec<(String, ProviderResult<api::ChatList>)> =
            futures::stream::iter(wanted.into_iter().map(|account| {
                let client = client.clone();
                async move {
                    let answer = client.chats(&account, None, CHAT_POLL_PAGE).await;
                    (account, answer)
                }
            }))
            .buffered(CHAT_POLL_CONCURRENCY)
            .collect()
            .await;

        let mut chat_pages = Vec::new();
        for (account, answer) in answers {
            match answer {
                Ok(page) => {
                    self.owed_chats.remove(&account);
                    chat_pages.push(page.items);
                }
                // A revoked key: this poll is void, and the last one.
                Err(error @ ProviderError::Unauthorized(_)) => return Err(error),
                // Weather: this account's chats are read at the next poll.
                Err(error) if error.is_transient() => {
                    tracing::debug!(%error, "an account's chats could not be read; next poll");
                    self.owed_chats.insert(account);
                }
                // This account's chats cannot be read (it was just
                // removed, say). Its messages are still worth reporting.
                Err(error) => tracing::debug!(%error, "an account's chats are not available"),
            }
        }
        Ok(chat_pages)
    }

    /// The stories of every connected account, on the polls that read
    /// them, and at the next poll for an account whose stories could not
    /// be read. They are stored rows: an account that is reconnecting
    /// (connections drop every few minutes) is read like one that is
    /// `ready`; one that is not linked, or logged out, is not asked. A deployment
    /// without the story routes is not asked at all until that is worth
    /// asking again.
    async fn poll_stories(
        &mut self,
        accounts: &[api::Account],
        now: Now,
    ) -> ProviderResult<Vec<(String, crate::stories::Listed)>> {
        let mut lists = Vec::new();
        let sweep = self.until_stories == 0;
        if !sweep && self.owed_stories.is_empty() {
            return Ok(lists);
        }
        let at = Timestamp::from_millis(now.epoch_ms);
        let wanted: Vec<String> = accounts
            .iter()
            .filter(|account| account.status == api::AccountStatus::Ready || account.reconnecting)
            .filter(|account| sweep || self.owed_stories.contains(&account.id))
            .filter(|account| !self.client.missing.is(Feature::Stories, &account.id))
            .map(|account| account.id.clone())
            .collect();
        // What is owed is asked for now, or was for an account that is
        // gone.
        self.owed_stories.clear();
        let client = self.client.clone();
        let answers: Vec<(String, ProviderResult<crate::stories::Listed>)> =
            futures::stream::iter(wanted.into_iter().map(|account| {
                let client = client.clone();
                async move {
                    let answer = client.stories(&account, at).await;
                    (account, answer)
                }
            }))
            .buffered(CHAT_POLL_CONCURRENCY)
            .collect()
            .await;
        for (account, answer) in answers {
            match answer {
                // The older way of reading the own stories has nothing a
                // poll is needed for.
                Ok(_) if self.client.missing.is(Feature::Stories, &account) => {}
                Ok(listed) => lists.push((account, listed)),
                Err(error @ ProviderError::Unauthorized(_)) => return Err(error),
                // Weather: this account's stories are read at the next
                // poll.
                Err(error) if error.is_transient() => {
                    tracing::debug!(%error, "an account's stories could not be read; next poll");
                    self.owed_stories.insert(account);
                }
                Err(error) => tracing::debug!(%error, "an account's stories could not be read"),
            }
        }
        Ok(lists)
    }

    /// Asks about the followed messages that are due. Returns the events
    /// for the ones that changed. Only a refused key is an error: weather
    /// leaves a message where it is, to be asked about at its next time,
    /// and a `429` stops every follow-up for as long as it says.
    pub(crate) async fn follow_up(&mut self, now: Now) -> ProviderResult<Vec<ProviderEvent>> {
        let plan = self.shared.follow().plan(now.at);
        let mut events = Vec::new();
        for (index, fetch) in plan.iter().enumerate() {
            let read = match fetch {
                Fetch::One { id } => self.client.message(id).await.map(|message| vec![message]),
                Fetch::Chat { account, chat, .. } => self
                    .client
                    .messages(Some(account), Some(chat), None, MAX_PAGE)
                    .await
                    .map(|page| page.items),
            };
            match read {
                Ok(messages) => {
                    events.extend(self.digest(&messages, now));
                    if let Fetch::Chat { ids, .. } = fetch {
                        let mut follow = self.shared.follow();
                        for id in ids {
                            if !messages.iter().any(|message| &message.id == id) {
                                follow.not_on_page(id, now.at);
                            }
                        }
                    }
                }
                Err(ProviderError::RateLimited { retry_after }) => {
                    // Everything planned and not asked yet waits too.
                    let refused: Vec<String> = plan[index..].iter().flat_map(Fetch::ids).collect();
                    tracing::debug!(?retry_after, "rate limited while following sends; waiting");
                    self.shared.follow().pause(now.at, retry_after, &refused);
                    break;
                }
                Err(error @ ProviderError::Unauthorized(_)) => return Err(error),
                Err(ProviderError::Rejected { code, .. }) if code == "not_found" => {
                    let mut follow = self.shared.follow();
                    for id in fetch.ids() {
                        follow.gone(&id);
                    }
                }
                Err(error) => tracing::debug!(%error, "a follow-up failed; asking again later"),
            }
        }
        Ok(events)
    }

    /// When there is next something to do: the general poll, or a
    /// followed message that is due before it.
    /// The poller was just primed: the next poll is one interval away.
    pub(crate) fn settle(&mut self) {
        self.next_poll = Instant::now() + self.interval;
    }

    /// The next general poll is due at `at`.
    pub(crate) fn schedule(&mut self, at: Instant) {
        self.next_poll = at;
    }

    /// The events a [`step`](Self::step) queued.
    pub(crate) fn drain(&mut self) -> Vec<ProviderEvent> {
        self.queue.drain(..).collect()
    }

    pub(crate) fn next_wake(&self, now: Instant) -> Instant {
        match self.shared.follow().next_wake(now) {
            Some(follow) => follow.min(self.next_poll),
            None => self.next_poll,
        }
    }

    /// Does what is due at `now` and queues the events it finds.
    pub(crate) async fn step(&mut self, now: Now) -> ProviderResult<()> {
        let events = self.follow_up(now).await?;
        self.queue.extend(events);
        if now.at < self.next_poll {
            return Ok(());
        }
        let polled = self.poll(now).await;
        self.next_poll = Instant::now() + self.interval;
        match polled {
            Ok(events) => self.queue.extend(events),
            Err(error @ ProviderError::Unauthorized(_)) => return Err(error),
            Err(ProviderError::RateLimited { retry_after }) => {
                // The general poll waits it out too, and so do the
                // follow-ups: one budget, one key.
                tracing::debug!(?retry_after, "rate limited while polling; waiting");
                self.shared.follow().pause(now.at, retry_after, &[]);
                if let Some(wait) = retry_after {
                    self.next_poll = self.next_poll.max(Instant::now() + wait);
                }
            }
            // Anything else is weather. Skip this tick.
            Err(error) => tracing::debug!(%error, "poll failed; trying again next tick"),
        }
        Ok(())
    }
}

#[async_trait]
impl EventSource for PollingEventSource {
    async fn open(&self) -> ProviderResult<EventStream> {
        let mut poller = self.poller();
        // Prime now: whatever exists at this moment is history, which the
        // client reads through `list_chats` / `fetch_messages`. Only what
        // changes from here on is an event (and the ticks that may have
        // moved while nobody listened, see `poll`).
        let primed = poller.poll(Now::real()).await?;
        poller.queue.extend(primed);
        poller.next_poll = Instant::now() + poller.interval;

        let stream = futures::stream::unfold(poller, |mut poller| async move {
            loop {
                if let Some(event) = poller.queue.pop_front() {
                    return Some((event, poller));
                }
                let wake = poller.next_wake(Instant::now());
                tokio::select! {
                    _ = tokio::time::sleep_until(wake) => {}
                    // A message was accepted: its first follow-up may be
                    // due before anything that was planned.
                    _ = poller.shared.wake.notified() => continue,
                }
                // The key was revoked: end the stream. The client will
                // try to subscribe again and get the same answer.
                if let Err(error) = poller.step(Now::real()).await {
                    tracing::warn!(%error, "wuapi rejected the API key; event polling stops");
                    return None;
                }
            }
        });
        Ok(stream.boxed())
    }

    fn accepted(&self, message: &api::Message) {
        self.shared.follow().accept(message, Now::real());
        self.shared.wake.notify_one();
    }

    fn listed(&self, messages: &[api::Message]) {
        let now = Now::real();
        let mut follow = self.shared.follow();
        for message in messages {
            // The client has this version: only the watching matters.
            follow.observe(message, now);
        }
    }
}
