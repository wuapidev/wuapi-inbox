//! What one `data:` line of the event stream says, and what the client
//! does about it. Pure: no I/O, no clock.

use crate::client::WuapiClient;
use crate::compat;
use crate::config::StreamTuning;
use crate::events::{PollState, Shared, CHAT_POLL_PAGE};
use crate::follow::Now;
use crate::{mapping, stories};
use client_provider::{
    AccountId, ChatId, MessageId, ProviderError, ProviderEvent, ProviderResult, Timestamp,
};
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::StreamExt;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use wuapi::types as api;

/// One `data:` line, read.
#[derive(Debug)]
pub(crate) struct Decoded {
    /// The envelope's `evt_` id, read before anything else is.
    pub id: Option<String>,
    /// The envelope's `type`, empty when it has none.
    pub kind: String,
    pub body: Body,
}

/// What the envelope carries, for the types the client acts on.
#[derive(Debug)]
pub(crate) enum Body {
    /// `message.*` but `message.edited`.
    Message(Box<api::Message>),
    /// `message.edited`.
    Edited(Box<api::Message>),
    /// `account.*`.
    Account(Box<api::Account>),
    /// `story.received` and `story.deleted`.
    Story(Box<api::Story>),
    /// `story.viewed` and `story.reacted`.
    Viewer(Box<api::StoryViewer>),
    /// `contact.updated`.
    Contact(Box<api::Contact>),
    /// `group.updated`.
    Group(Box<api::GroupChange>),
    /// `group.joined`.
    Joined(Box<api::Group>),
    /// An event that says a chat changed and not how: `chat.updated`,
    /// `contact.picture_updated`.
    Chat { account: String, chat: String },
    /// `history.synced`.
    History { account: String },
    /// `poll.voted`, with the poll message when the envelope has it.
    Poll(Option<Box<api::Message>>),
    /// A type the client has no use for, or does not know.
    Ignored,
    /// A handled type that could not be read. `chat` is the chat of a
    /// `message.*` whose ids could still be found. `why` says nothing of
    /// the payload.
    Broken {
        chat: Option<(String, String)>,
        why: String,
    },
}

/// Reads one `data:` line.
///
/// The id and the type come first, from the bare JSON: a payload this
/// client cannot read must not cost it the id (the replay dedupe) or, for a
/// message, the chat to read again. Types the client has no use for are not
/// decoded at all, so a newer backend's changes to them cost nothing.
pub(crate) fn decode(data: &str) -> Decoded {
    let mut value: Value = match serde_json::from_str(data) {
        Ok(value) => value,
        Err(error) => {
            return Decoded {
                id: None,
                kind: String::new(),
                body: Body::Broken {
                    chat: None,
                    why: reason(&error),
                },
            }
        }
    };
    let text = |field: &Value| field.as_str().map(str::to_owned);
    let id = text(&value["id"]);
    let kind = text(&value["type"]).unwrap_or_default();
    let body = if handled(&kind) {
        read(&kind, &mut value)
    } else {
        Body::Ignored
    };
    Decoded { id, kind, body }
}

/// What is wrong with a payload, without quoting any of it: serde's
/// messages carry the offending value (`invalid type: integer \`12345\``),
/// and a value is somebody's message, number or name.
fn reason(error: &serde_json::Error) -> String {
    use serde_json::error::Category;
    match error.classify() {
        Category::Syntax | Category::Eof => "not valid JSON".to_owned(),
        Category::Io => "unreadable".to_owned(),
        Category::Data => {
            let text = error.to_string();
            if let Some(rest) = text.strip_prefix("missing field `") {
                // The name is the schema's.
                let name = rest.split('`').next().unwrap_or_default();
                format!("missing field `{name}`")
            } else if text.starts_with("invalid type") {
                match text.split_once(", expected ") {
                    Some((_, wanted)) => {
                        let wanted = wanted.split(" at line").next().unwrap_or_default();
                        format!("invalid type, expected {wanted}")
                    }
                    None => "invalid type".to_owned(),
                }
            } else {
                "invalid value".to_owned()
            }
        }
    }
}

/// Whether the client does anything with this type.
fn handled(kind: &str) -> bool {
    kind.starts_with("message.")
        || kind.starts_with("account.")
        || kind.starts_with("story.")
        || matches!(
            kind,
            "contact.updated"
                | "contact.picture_updated"
                | "group.updated"
                | "group.joined"
                | "chat.updated"
                | "history.synced"
                | "poll.voted"
        )
}

/// The account and chat a `message.*` names, when its ids are readable
/// even though the message is not.
fn chat_of(value: &Value) -> Option<(String, String)> {
    let object = &value["data"]["object"];
    Some((
        object["accountId"].as_str()?.to_owned(),
        object["chatId"].as_str()?.to_owned(),
    ))
}

fn read(kind: &str, value: &mut Value) -> Body {
    // Stories are read as the story lists are, one at a time and leniently.
    if matches!(kind, "story.received" | "story.deleted") {
        let object = value["data"]["object"].take();
        // What `read_story` says about a story quotes its values; this
        // client only says that it could not.
        let mut unread = Vec::new();
        return match compat::read_story(object, None, &mut unread) {
            Some(story) => Body::Story(Box::new(story)),
            None => Body::Broken {
                chat: None,
                why: "the story could not be read".to_owned(),
            },
        };
    }
    let chat = if kind.starts_with("message.") {
        chat_of(value)
    } else {
        None
    };
    compat::fill(value);
    let event: api::Event = match serde_json::from_value(value.take()) {
        Ok(event) => event,
        Err(error) => {
            return Body::Broken {
                chat,
                why: reason(&error),
            }
        }
    };
    match event {
        api::Event::Message(event) => Body::Message(Box::new(event.data.object)),
        api::Event::MessageEdited(event) => Body::Edited(Box::new(event.data.object)),
        api::Event::Account(event) => Body::Account(Box::new(event.data.object)),
        api::Event::StoryViewer(event) => Body::Viewer(Box::new(event.data.object)),
        api::Event::ContactUpdated(event) => Body::Contact(Box::new(event.data.object)),
        api::Event::GroupUpdated(event) => Body::Group(Box::new(event.data.object)),
        api::Event::ChatUpdated(event) => Body::Chat {
            account: event.data.object.account_id,
            chat: event.data.object.chat_id,
        },
        api::Event::ContactPictureUpdated(event) => Body::Chat {
            account: event.data.object.account_id,
            chat: event.data.object.chat_id,
        },
        api::Event::GroupJoined(event) => Body::Joined(Box::new(event.data.object)),
        api::Event::HistorySynced(event) => Body::History {
            account: event.data.object.account_id,
        },
        api::Event::PollVoted(event) => Body::Poll(event.data.object.poll.map(Box::new)),
        _ => Body::Ignored,
    }
}

/// The recent `evt_` ids, to drop what a replay repeats. Least recently
/// used goes first.
pub(crate) struct EvtLru {
    capacity: usize,
    /// Id -> when it was last seen.
    stamps: HashMap<String, u64>,
    /// When -> id, oldest first.
    order: BTreeMap<u64, String>,
    clock: u64,
}

impl EvtLru {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            stamps: HashMap::new(),
            order: BTreeMap::new(),
            clock: 0,
        }
    }

    /// Whether `id` was already seen; records it either way.
    pub(crate) fn seen(&mut self, id: &str) -> bool {
        self.clock += 1;
        let known = match self.stamps.insert(id.to_owned(), self.clock) {
            Some(before) => {
                self.order.remove(&before);
                true
            }
            None => false,
        };
        self.order.insert(self.clock, id.to_owned());
        while self.stamps.len() > self.capacity {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            self.stamps.remove(&oldest);
        }
        known
    }
}

/// A chat or an account the stream says is worth reading again.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Dirty {
    Chat { account: String, chat: String },
    Account(String),
}

/// What one envelope came to.
#[derive(Debug, Default)]
pub(crate) struct Mapped {
    pub events: Vec<ProviderEvent>,
    pub marks: Vec<Dirty>,
}

/// Turns envelopes into events, and into marks for chats worth reading.
pub(crate) struct Mapper {
    state: Arc<Mutex<PollState>>,
    shared: Arc<Shared>,
    lru: EvtLru,
}

impl Mapper {
    pub(crate) fn new(state: Arc<Mutex<PollState>>, shared: Arc<Shared>, evt_lru: usize) -> Self {
        Self {
            state,
            shared,
            lru: EvtLru::new(evt_lru),
        }
    }

    /// One `data:` line.
    pub(crate) fn map(&mut self, data: &str, now: Now) -> Mapped {
        let decoded = decode(data);
        // A replay repeats ids: the second time is nothing.
        if let Some(id) = &decoded.id {
            if self.lru.seen(id) {
                return Mapped::default();
            }
        }
        let mut out = Mapped::default();
        match decoded.body {
            Body::Message(wire) => {
                self.pushed(&wire, false, now, &mut out);
                if decoded.kind == "message.received" {
                    out.marks.push(Dirty::Chat {
                        account: wire.account_id,
                        chat: wire.chat_id,
                    });
                }
            }
            Body::Edited(wire) => self.pushed(&wire, true, now, &mut out),
            Body::Poll(Some(wire)) => self.pushed(&wire, false, now, &mut out),
            Body::Poll(None) => {}
            Body::Account(wire) => {
                let events = locked(&self.state).observe_accounts(std::slice::from_ref(&wire));
                out.events.extend(events);
            }
            Body::Story(wire) if decoded.kind == "story.deleted" => {
                let event = locked(&self.state).forget_story(&wire.account_id, &wire.id);
                out.events.push(event);
            }
            Body::Story(wire) => {
                let at = Timestamp::from_millis(now.epoch_ms);
                if let Some(story) = stories::story(&wire, at) {
                    out.events.extend(locked(&self.state).observe_story(story));
                }
            }
            Body::Viewer(wire) => {
                if let Some(viewer) = stories::viewer(&wire) {
                    out.events.push(ProviderEvent::StoryViewed {
                        account_id: AccountId::new(wire.account_id.clone()),
                        story_id: MessageId::new(wire.story_id.clone()),
                        viewer,
                    });
                }
            }
            Body::Contact(wire) => out
                .events
                .push(ProviderEvent::ContactUpdated(mapping::contact(&wire))),
            Body::Group(wire) => {
                // WhatsApp reports a link on the community or on the
                // subgroup: `communityId` names the community either way.
                let relinked: Vec<ChatId> = wire
                    .linked
                    .iter()
                    .chain(&wire.unlinked)
                    .map(|group| ChatId::new(group.clone()))
                    .collect();
                // A link alone changes nothing else about `groupId`.
                let only_relinked = !relinked.is_empty()
                    && wire.added.is_empty()
                    && wire.removed.is_empty()
                    && wire.promoted.is_empty()
                    && wire.demoted.is_empty()
                    && wire.name.is_none()
                    && wire.description.is_none()
                    && wire.locked.is_none()
                    && wire.announce.is_none();
                if !only_relinked {
                    out.events.push(ProviderEvent::GroupChanged {
                        account_id: AccountId::new(wire.account_id.clone()),
                        group_id: ChatId::new(wire.group_id.clone()),
                    });
                }
                if !relinked.is_empty() {
                    let community = wire.community_id.as_ref().unwrap_or(&wire.group_id);
                    out.events.push(ProviderEvent::CommunityChanged {
                        account_id: AccountId::new(wire.account_id.clone()),
                        community_id: ChatId::new(community.clone()),
                        groups: relinked,
                    });
                }
                if wire.name.is_some() {
                    out.marks.push(Dirty::Chat {
                        account: wire.account_id,
                        chat: wire.group_id,
                    });
                }
            }
            Body::Joined(wire) => {
                // Joined inside a community: the group is read with it,
                // so the chat list knows where the new chat belongs.
                if let Some(community) = wire.community_id.as_ref().filter(|id| !id.is_empty()) {
                    out.events.push(ProviderEvent::CommunityChanged {
                        account_id: AccountId::new(wire.account_id.clone()),
                        community_id: ChatId::new(community.clone()),
                        groups: vec![ChatId::new(wire.id.clone())],
                    });
                }
                out.marks.push(Dirty::Chat {
                    account: wire.account_id,
                    chat: wire.id,
                });
            }
            Body::Chat { account, chat } => out.marks.push(Dirty::Chat { account, chat }),
            Body::History { account } => out.marks.push(Dirty::Account(account)),
            Body::Ignored => tracing::debug!(kind = %decoded.kind, "stream event ignored"),
            Body::Broken { chat, why } => {
                // The type and the reason only: what the event said stays
                // out of the log, it is somebody's message.
                tracing::debug!(kind = %decoded.kind, %why, "a stream event could not be read");
                if let Some((account, chat)) = chat {
                    out.marks.push(Dirty::Chat { account, chat });
                }
            }
        }
        out
    }

    /// A message the stream pushed: said once, and no longer followed.
    fn pushed(&self, wire: &api::Message, edited: bool, now: Now, out: &mut Mapped) {
        let event = locked(&self.state).observe_pushed(wire);
        // The stream told what a follow-up would have asked.
        self.shared.follow().observe(wire, now);
        out.events.extend(event.map(|mut event| {
            if let ProviderEvent::MessageUpserted(message) = &mut event {
                // An edit is an edit even when the message does not say so.
                message.edited |= edited;
            }
            event
        }));
    }
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // A panic while holding it leaves nothing half-done worth losing the
    // stream for.
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One read the stream's marks come to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Read {
    /// `GET /v1/accounts/{account}/chats/{chat}`.
    Chat { account: String, chat: String },
    /// The newest page of an account's chats.
    Page { account: String },
}

/// How a read ended, as far as the marks are concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Done,
    /// Weather: ask again shortly.
    Transient,
    /// Every read waits this long.
    RateLimited(Duration),
    /// The chat is gone: nothing to read.
    NotFound,
}

/// The chats the stream says changed, waiting to be read, and when each
/// read may start. Pure: the caller brings the time and does the reading.
///
/// A mark opens a window and every mark inside it is read together, a chat
/// once however often it was marked. Reads are limited: a few at once, one
/// per chat every `dirty_gap`, so many a minute. A mark that comes while
/// the chat is being read is kept for after.
pub(crate) struct DirtyChats {
    window: Duration,
    gap: Duration,
    per_minute: usize,
    concurrency: usize,
    collapse: usize,
    retry: Duration,
    retries: u32,
    /// Marked, not started.
    pending: HashMap<Read, Waiting>,
    /// Started, not finished.
    reading: HashMap<Read, Flight>,
    /// When the open window closes.
    window_closes: Option<Instant>,
    /// When each chat was last started.
    last_start: HashMap<Read, Instant>,
    /// Starts within the last minute, oldest first.
    starts: VecDeque<Instant>,
    /// Nothing starts before this: a `429` said so.
    paused_until: Option<Instant>,
    seq: u64,
}

struct Waiting {
    ready_at: Instant,
    seq: u64,
    /// Retries made so far.
    tries: u32,
}

struct Flight {
    tries: u32,
    /// Marked again while being read.
    remarked: bool,
}

const MINUTE: Duration = Duration::from_secs(60);

impl DirtyChats {
    pub(crate) fn new(tuning: &StreamTuning) -> Self {
        Self {
            window: tuning.dirty_window,
            gap: tuning.dirty_gap,
            per_minute: tuning.dirty_per_minute,
            concurrency: tuning.dirty_concurrency,
            collapse: tuning.dirty_collapse,
            retry: tuning.dirty_retry,
            retries: tuning.dirty_retries,
            pending: HashMap::new(),
            reading: HashMap::new(),
            window_closes: None,
            last_start: HashMap::new(),
            starts: VecDeque::new(),
            paused_until: None,
            seq: 0,
        }
    }

    /// A chat (or an account's chats) is worth reading.
    pub(crate) fn mark(&mut self, mark: Dirty, now: Instant) {
        let read = match mark {
            Dirty::Chat { account, chat } => Read::Chat { account, chat },
            Dirty::Account(account) => Read::Page { account },
        };
        if let Some(flight) = self.reading.get_mut(&read) {
            flight.remarked = true;
            return;
        }
        if self.pending.contains_key(&read) {
            return;
        }
        self.enqueue(read, 0, now);
        self.collapse_if_crowded();
    }

    /// Puts a read on the list, ready when the window of `now` closes.
    fn enqueue(&mut self, read: Read, tries: u32, now: Instant) {
        let ready_at = match self.window_closes {
            Some(closes) if closes > now => closes,
            _ => {
                let closes = now + self.window;
                self.window_closes = Some(closes);
                closes
            }
        };
        self.push(read, ready_at, tries);
    }

    fn push(&mut self, read: Read, ready_at: Instant, tries: u32) {
        self.seq += 1;
        let seq = self.seq;
        self.pending.insert(
            read,
            Waiting {
                ready_at,
                seq,
                tries,
            },
        );
    }

    /// More chats waiting than are worth asking about one by one: ask for
    /// each account's newest chats instead.
    fn collapse_if_crowded(&mut self) {
        let crowded = self
            .pending
            .keys()
            .filter(|read| matches!(read, Read::Chat { .. }))
            .count()
            > self.collapse;
        if !crowded {
            return;
        }
        let chats: Vec<Read> = self
            .pending
            .keys()
            .filter(|read| matches!(read, Read::Chat { .. }))
            .cloned()
            .collect();
        let mut gone: Vec<(Read, Waiting)> = chats
            .into_iter()
            .filter_map(|read| self.pending.remove(&read).map(|waiting| (read, waiting)))
            .collect();
        gone.sort_by_key(|(_, waiting)| waiting.seq);
        for (read, waiting) in gone {
            let Read::Chat { account, .. } = read else {
                continue;
            };
            let page = Read::Page { account };
            match self.pending.get_mut(&page) {
                Some(have) => have.ready_at = have.ready_at.min(waiting.ready_at),
                None => self.push(page, waiting.ready_at, 0),
            }
        }
    }

    /// The earliest `read` may start, whatever else is going on.
    fn earliest(&self, read: &Read, waiting: &Waiting) -> Instant {
        let mut at = waiting.ready_at;
        if let Some(last) = self.last_start.get(read) {
            at = at.max(*last + self.gap);
        }
        if let Some(until) = self.paused_until {
            at = at.max(until);
        }
        at
    }

    fn forget_old_starts(&mut self, now: Instant) {
        while self
            .starts
            .front()
            .is_some_and(|start| now.saturating_duration_since(*start) >= MINUTE)
        {
            self.starts.pop_front();
        }
        self.last_start
            .retain(|_, start| now.saturating_duration_since(*start) < self.gap);
    }

    /// The reads that may start at `now`.
    pub(crate) fn due(&mut self, now: Instant) -> Vec<Read> {
        self.forget_old_starts(now);
        let mut ready: Vec<(Instant, u64, Read)> = self
            .pending
            .iter()
            .filter(|(read, waiting)| self.earliest(read, waiting) <= now)
            .map(|(read, waiting)| (waiting.ready_at, waiting.seq, read.clone()))
            .collect();
        ready.sort_by_key(|(at, seq, _)| (*at, *seq));
        let mut started = Vec::new();
        for (_, _, read) in ready {
            if self.reading.len() >= self.concurrency || self.starts.len() >= self.per_minute {
                break;
            }
            let Some(waiting) = self.pending.remove(&read) else {
                continue;
            };
            self.reading.insert(
                read.clone(),
                Flight {
                    tries: waiting.tries,
                    remarked: false,
                },
            );
            self.starts.push_back(now);
            self.last_start.insert(read.clone(), now);
            started.push(read);
        }
        started
    }

    /// A read that `due` returned is over.
    pub(crate) fn finished(&mut self, read: &Read, outcome: Outcome, now: Instant) {
        let Some(flight) = self.reading.remove(read) else {
            return;
        };
        match outcome {
            Outcome::NotFound => {}
            Outcome::Done => {
                if flight.remarked {
                    self.enqueue(read.clone(), 0, now);
                }
            }
            Outcome::Transient if flight.remarked => self.enqueue(read.clone(), 0, now),
            Outcome::Transient if flight.tries < self.retries => {
                self.push(read.clone(), now + self.retry, flight.tries + 1);
            }
            // Out of retries: the next mark of the chat starts over.
            Outcome::Transient => {}
            Outcome::RateLimited(wait) => {
                self.paused_until = Some(now + wait);
                // Still owed, and first in line once the pause is over.
                self.push(read.clone(), now, flight.tries);
            }
        }
    }

    /// When `due` next has something to say, if anything is waiting.
    pub(crate) fn next_wake(&self, _now: Instant) -> Option<Instant> {
        let mut wake = self
            .pending
            .iter()
            .map(|(read, waiting)| self.earliest(read, waiting))
            .min()?;
        if self.starts.len() >= self.per_minute {
            if let Some(oldest) = self.starts.front() {
                wake = wake.max(*oldest + MINUTE);
            }
        }
        Some(wake)
    }
}

/// How long reads wait after a `429` that does not say.
const DEFAULT_PAUSE: Duration = Duration::from_secs(5);

type Finished = (Read, ProviderResult<Vec<api::Chat>>);

/// Runs the reads [`DirtyChats`] plans, against the API.
///
/// The reads are futures this struct owns, so they go on while the caller
/// does something else, and dropping [`next`](Self::next) loses nothing.
/// Call `next` afresh after a [`mark`](Self::mark): what it waits for may
/// have changed.
pub(crate) struct ChatReads {
    client: Arc<WuapiClient>,
    state: Arc<Mutex<PollState>>,
    dirty: DirtyChats,
    retry_after_max: Duration,
    flying: FuturesUnordered<BoxFuture<'static, Finished>>,
}

impl ChatReads {
    pub(crate) fn new(
        client: Arc<WuapiClient>,
        state: Arc<Mutex<PollState>>,
        tuning: &StreamTuning,
    ) -> Self {
        Self {
            client,
            state,
            dirty: DirtyChats::new(tuning),
            retry_after_max: tuning.retry_after_max,
            flying: FuturesUnordered::new(),
        }
    }

    /// A chat is worth reading.
    pub(crate) fn mark(&mut self, mark: Dirty) {
        self.dirty.mark(mark, Instant::now());
    }

    fn start(&mut self, read: Read) {
        let client = self.client.clone();
        self.flying.push(Box::pin(async move {
            let result = match &read {
                Read::Chat { account, chat } => client.chat(account, chat).await.map(|c| vec![c]),
                Read::Page { account } => client
                    .chats(account, None, CHAT_POLL_PAGE)
                    .await
                    .map(|page| page.items),
            };
            (read, result)
        }));
    }

    /// Waits for reads to finish until one has news, and returns it. An
    /// `Err` is a revoked key: the stream is over. Never returns while
    /// nothing is marked.
    pub(crate) async fn next(&mut self) -> ProviderResult<Vec<ProviderEvent>> {
        loop {
            let now = Instant::now();
            for read in self.dirty.due(now) {
                self.start(read);
            }
            let wake = self.dirty.next_wake(now);
            let done = tokio::select! {
                Some(done) = self.flying.next(), if !self.flying.is_empty() => Some(done),
                () = tokio::time::sleep_until(wake.unwrap_or(now)), if wake.is_some() => None,
                else => std::future::pending().await,
            };
            let Some((read, result)) = done else {
                continue;
            };
            let events = self.finish(&read, result)?;
            if !events.is_empty() {
                return Ok(events);
            }
        }
    }

    fn finish(
        &mut self,
        read: &Read,
        result: ProviderResult<Vec<api::Chat>>,
    ) -> ProviderResult<Vec<ProviderEvent>> {
        let now = Instant::now();
        let outcome = match result {
            Ok(chats) => {
                self.dirty.finished(read, Outcome::Done, now);
                return Ok(locked(&self.state).observe_chats(&chats));
            }
            Err(error @ ProviderError::Unauthorized(_)) => return Err(error),
            Err(ProviderError::RateLimited { retry_after }) => Outcome::RateLimited(
                retry_after
                    .unwrap_or(DEFAULT_PAUSE)
                    .min(self.retry_after_max),
            ),
            Err(ProviderError::Rejected { code, .. })
                if matches!(code.as_str(), "not_found" | "chat_not_found" | "http_404") =>
            {
                Outcome::NotFound
            }
            Err(error) => {
                tracing::debug!(%error, "a chat could not be read; trying again shortly");
                Outcome::Transient
            }
        };
        self.dirty.finished(read, outcome, now);
        Ok(Vec::new())
    }
}
