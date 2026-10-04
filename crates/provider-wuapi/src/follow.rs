//! Which outgoing messages are worth asking about again, and when.
//!
//! Without a push channel, a message's ticks only move here when somebody
//! asks. The general poll reads the newest page of the whole organization,
//! which is late (one interval) and blind to a message that newer ones
//! have pushed off that page. So the messages whose status can still move
//! are kept here, each with the time it is next worth a request:
//!
//! * **Just accepted** (a send from this client, or any outgoing message
//!   seen within its first two minutes): asked about 0.5 s later, then 1,
//!   2, 4, 8, 16 and every 30 s apart, until it is delivered, read or
//!   failed, or [`FAST_FOR`] has passed.
//! * **After that**, for as long as it is not read and not older than
//!   [`WATCH_FOR`]: every eighth of its age, never more often than
//!   [`SLOW_MIN`] (half a minute while it is not delivered) nor more
//!   rarely than [`SLOW_MAX`]. A message the general poll still sees on
//!   its page is checked by that poll for free and never gets a request
//!   of its own.
//!
//! Requests are shared: several messages of one chat that are due are
//! answered by one read of that chat's newest page. They are bounded
//! ([`PER_MINUTE`]), and a `429` stops all of them for its `Retry-After`.
//!
//! Pure state, no I/O: the poller in `events.rs` makes the requests this
//! plans and reports back what it read. A push transport needs none of it.

use crate::mapping;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;
use tokio::time::Instant;
use wuapi::types as api;

/// The first check after a message was accepted.
pub(crate) const FAST_FIRST: Duration = Duration::from_millis(500);
/// The longest wait between two checks of a message just accepted.
pub(crate) const FAST_MAX: Duration = Duration::from_secs(30);
/// How long a message that is not delivered yet is followed closely.
pub(crate) const FAST_FOR: Duration = Duration::from_secs(120);
/// The shortest wait between two checks after that.
pub(crate) const SLOW_MIN: Duration = Duration::from_secs(15);
/// The longest.
pub(crate) const SLOW_MAX: Duration = Duration::from_secs(5 * 60);
/// A message is checked again after this share of its age.
const SLOW_AGE_SHARE: u32 = 8;
/// How long an outgoing message that is not read is watched at all.
pub(crate) const WATCH_FOR: Duration = Duration::from_secs(6 * 60 * 60);
/// How many messages are watched. Beyond it the oldest are let go.
pub(crate) const MAX_WATCHED: usize = 300;
/// How many follow-up requests may be made in any minute: a fifth of the
/// 600 a key is allowed, whatever is being sent.
pub(crate) const PER_MINUTE: usize = 120;
/// How many requests one step makes at most.
pub(crate) const PER_STEP: usize = 8;
/// How long everything waits after a `429` that does not say.
const DEFAULT_PAUSE: Duration = Duration::from_secs(5);
/// The longest such wait, whatever it says.
const MAX_PAUSE: Duration = Duration::from_secs(60);
/// The window of [`PER_MINUTE`].
const MINUTE: Duration = Duration::from_secs(60);

/// A moment, on both clocks: the one that only moves forward, for waits,
/// and the calendar's, for the age of a message. Tests make their own.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Now {
    pub(crate) at: Instant,
    pub(crate) epoch_ms: i64,
}

impl Now {
    pub(crate) fn real() -> Self {
        Self {
            at: Instant::now(),
            epoch_ms: client_provider::Timestamp::now().as_millis(),
        }
    }

    /// This moment, `later` on.
    #[cfg(test)]
    pub(crate) fn plus(self, later: Duration) -> Self {
        Self {
            at: self.at + later,
            epoch_ms: self.epoch_ms + later.as_millis() as i64,
        }
    }
}

/// A fingerprint that changes whenever the message does.
pub(crate) fn version(message: &api::Message) -> String {
    format!("{}|{}", message.updated_at, message.status)
}

/// One request the poller should make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Fetch {
    /// `GET /v1/messages/{id}`.
    One {
        /// The message.
        id: String,
    },
    /// The newest page of a chat, for several of its messages at once.
    Chat {
        /// The account.
        account: String,
        /// The chat.
        chat: String,
        /// The messages it is read for.
        ids: Vec<String>,
    },
}

impl Fetch {
    /// The messages this request is for.
    pub(crate) fn ids(&self) -> Vec<String> {
        match self {
            Self::One { id } => vec![id.clone()],
            Self::Chat { ids, .. } => ids.clone(),
        }
    }
}

struct Watched {
    account: String,
    chat: String,
    /// The version of the message the client was last told.
    told: String,
    /// When it came under watch, and how old it was then.
    added: Instant,
    age_then: Duration,
    /// It was accepted a moment ago: followed closely until delivered.
    close: bool,
    delivered: bool,
    /// Its chat's newest page no longer shows it: it is asked for by id.
    off_page: bool,
    due: Instant,
}

impl Watched {
    fn age(&self, now: Instant) -> Duration {
        self.age_then + now.saturating_duration_since(self.added)
    }

    /// How long until it is next worth a request.
    fn wait(&self, now: Instant) -> Duration {
        let age = self.age(now);
        if self.close && !self.delivered {
            if let Some(wait) = close_wait(age) {
                return wait;
            }
            // Past its close checks and still not delivered: never
            // sooner than the last of them were apart.
            return (age / SLOW_AGE_SHARE).clamp(FAST_MAX, SLOW_MAX);
        }
        (age / SLOW_AGE_SHARE).clamp(SLOW_MIN, SLOW_MAX)
    }
}

/// The moments, counted from its acceptance, at which a message just
/// accepted is asked about: 0.5 s, then gaps of 1, 2, 4, 8, 16 and 30 s.
pub(crate) fn close_points() -> Vec<Duration> {
    let mut points = Vec::new();
    let (mut at, mut gap) = (FAST_FIRST, FAST_FIRST * 2);
    while at < FAST_FOR {
        points.push(at);
        at += gap;
        gap = (gap * 2).min(FAST_MAX);
    }
    points
}

/// The wait until the next of [`close_points`] for a message of this age.
fn close_wait(age: Duration) -> Option<Duration> {
    // A check that lands a hair before its point counts as that point.
    let slack = Duration::from_millis(50);
    close_points()
        .into_iter()
        .find(|point| *point > age + slack)
        .map(|point| point - age)
}

/// True for a message whose ticks can still move: one of the account's
/// own, in a chat the client shows, that is not read and has not failed.
fn can_still_move(wire: &api::Message) -> bool {
    use api::MessageStatus as S;
    wire.direction == api::MessageDirection::Outbound
        && matches!(wire.chat_type, api::ChatType::Direct | api::ChatType::Group)
        && wire.r#type != api::MessageType::Reaction
        && matches!(wire.status, S::Queued | S::Sent | S::Delivered)
}

/// The messages under watch. See the module documentation.
#[derive(Default)]
pub(crate) struct Follow {
    watched: HashMap<String, Watched>,
    /// When each follow-up request of the last minute was made.
    spent: VecDeque<Instant>,
    /// A `429` said to wait until then.
    paused_until: Option<Instant>,
}

impl Follow {
    /// How many messages are under watch.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.watched.len()
    }

    /// Whether this message is under watch.
    pub(crate) fn watches(&self, id: &str) -> bool {
        self.watched.contains_key(id)
    }

    fn age_of(wire: &api::Message, now: Now) -> Option<Duration> {
        let created = mapping::timestamp(&wire.created_at)?.as_millis();
        Some(Duration::from_millis((now.epoch_ms - created).max(0) as u64))
    }

    /// The API accepted a message sent from here: it is followed closely
    /// from now on. The client knows this version of it (the answer to
    /// its send says it).
    pub(crate) fn accept(&mut self, wire: &api::Message, now: Now) {
        if !can_still_move(wire) {
            return;
        }
        // Counted from this moment on this machine's own clock, not from
        // the API's `createdAt`: the two clocks need not agree, and half a
        // second is less than they usually differ by.
        let entry = self
            .watched
            .entry(wire.id.clone())
            .or_insert_with(|| Watched {
                account: wire.account_id.clone(),
                chat: wire.chat_id.clone(),
                told: version(wire),
                added: now.at,
                age_then: Duration::ZERO,
                close: true,
                delivered: wire.status == api::MessageStatus::Delivered,
                off_page: false,
                due: now.at,
            });
        entry.close = true;
        entry.due = now.at + entry.wait(now.at);
        self.trim(now.at);
    }

    /// The message was just read from the API, by whatever request.
    /// Returns whether it is under watch and the client has not been told
    /// this version of it: the caller reports it then. A message whose
    /// ticks can still move comes under watch; one that is read, failed or
    /// too old leaves it.
    pub(crate) fn observe(&mut self, wire: &api::Message, now: Now) -> bool {
        let version = version(wire);
        let age = Self::age_of(wire, now);
        let keep = can_still_move(wire) && age.is_some_and(|age| age < WATCH_FOR);
        match self.watched.get_mut(&wire.id) {
            Some(entry) => {
                let news = entry.told != version;
                entry.told = version;
                if keep {
                    entry.delivered = wire.status == api::MessageStatus::Delivered;
                    entry.due = now.at + entry.wait(now.at);
                } else {
                    self.watched.remove(&wire.id);
                }
                news
            }
            None => {
                if let (true, Some(age)) = (keep, age) {
                    let mut entry = Watched {
                        account: wire.account_id.clone(),
                        chat: wire.chat_id.clone(),
                        told: version,
                        added: now.at,
                        age_then: age,
                        // Sent a moment ago, from wherever: its first
                        // ticks are about to come.
                        close: age < FAST_FOR,
                        delivered: wire.status == api::MessageStatus::Delivered,
                        off_page: false,
                        due: now.at,
                    };
                    entry.due = now.at + entry.wait(now.at);
                    self.watched.insert(wire.id.clone(), entry);
                    self.trim(now.at);
                }
                false
            }
        }
    }

    /// Lets the oldest go when there are too many.
    fn trim(&mut self, now: Instant) {
        while self.watched.len() > MAX_WATCHED {
            let oldest = self
                .watched
                .iter()
                .max_by_key(|(_, entry)| entry.age(now))
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => self.watched.remove(&id),
                None => break,
            };
        }
    }

    fn budget_left(&mut self, now: Instant) -> usize {
        while self
            .spent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= MINUTE)
        {
            self.spent.pop_front();
        }
        PER_MINUTE.saturating_sub(self.spent.len())
    }

    /// The requests worth making now, at most [`PER_STEP`] and within the
    /// minute's budget. Each message planned is given its next time at
    /// once, so a request that fails for now simply leaves it there.
    pub(crate) fn plan(&mut self, now: Instant) -> Vec<Fetch> {
        self.watched.retain(|_, entry| entry.age(now) < WATCH_FOR);
        if self.paused_until.is_some_and(|until| now < until) {
            return Vec::new();
        }
        self.paused_until = None;
        // (due, id, account, chat, off the page)
        let mut due: Vec<(Instant, String, String, String, bool)> = self
            .watched
            .iter()
            .filter(|(_, entry)| entry.due <= now)
            .map(|(id, entry)| {
                (
                    entry.due,
                    id.clone(),
                    entry.account.clone(),
                    entry.chat.clone(),
                    entry.off_page,
                )
            })
            .collect();
        due.sort();

        // Several of one chat share a read of its newest page; one alone,
        // or one that page no longer shows, is asked for by id.
        let mut together: HashMap<(String, String), usize> = HashMap::new();
        for (_, _, account, chat, off_page) in &due {
            if !off_page {
                *together.entry((account.clone(), chat.clone())).or_default() += 1;
            }
        }
        let mut fetches: Vec<Fetch> = Vec::new();
        let mut pages: HashMap<(String, String), usize> = HashMap::new();
        for (_, id, account, chat, off_page) in due {
            let key = (account, chat);
            if off_page || together[&key] < 2 {
                fetches.push(Fetch::One { id });
                continue;
            }
            match pages.get(&key) {
                Some(index) => {
                    if let Fetch::Chat { ids, .. } = &mut fetches[*index] {
                        ids.push(id);
                    }
                }
                None => {
                    pages.insert(key.clone(), fetches.len());
                    fetches.push(Fetch::Chat {
                        account: key.0,
                        chat: key.1,
                        ids: vec![id],
                    });
                }
            }
        }

        let allowed = self.budget_left(now).min(PER_STEP);
        fetches.truncate(allowed);
        for fetch in &fetches {
            self.spent.push_back(now);
            for id in fetch.ids() {
                if let Some(entry) = self.watched.get_mut(&id) {
                    entry.due = now + entry.wait(now);
                }
            }
        }
        fetches
    }

    /// The chat's newest page does not show the message any more: from
    /// now on it is asked for by id, starting at once.
    pub(crate) fn not_on_page(&mut self, id: &str, now: Instant) {
        if let Some(entry) = self.watched.get_mut(id) {
            entry.off_page = true;
            entry.due = now;
        }
    }

    /// The API no longer has the message.
    pub(crate) fn gone(&mut self, id: &str) {
        self.watched.remove(id);
    }

    /// The API said to slow down: nothing is asked until `retry_after` has
    /// passed, and the messages of the request it refused are first then.
    pub(crate) fn pause(
        &mut self,
        now: Instant,
        retry_after: Option<Duration>,
        refused: &[String],
    ) {
        let wait = retry_after
            .unwrap_or(DEFAULT_PAUSE)
            .clamp(Duration::from_secs(1), MAX_PAUSE);
        let until = now + wait;
        self.paused_until = Some(self.paused_until.map_or(until, |known| known.max(until)));
        for id in refused {
            if let Some(entry) = self.watched.get_mut(id) {
                entry.due = until;
            }
        }
    }

    /// When the next request is worth making, if any message is watched.
    pub(crate) fn next_wake(&self, now: Instant) -> Option<Instant> {
        let mut wake = self.watched.values().map(|entry| entry.due).min()?;
        if let Some(until) = self.paused_until {
            wake = wake.max(until);
        }
        // Out of budget: when the oldest request of this minute leaves it.
        let recent = self
            .spent
            .iter()
            .filter(|at| now.saturating_duration_since(**at) < MINUTE)
            .count();
        if recent >= PER_MINUTE {
            if let Some(oldest) = self.spent.front() {
                wake = wake.max(*oldest + MINUTE);
            }
        }
        Some(wake)
    }
}
