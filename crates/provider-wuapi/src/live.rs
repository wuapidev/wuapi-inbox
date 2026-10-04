//! `Auto`: the stream when the deployment has it, polling when it does not,
//! and back.
//!
//! One driver, one `EventStream`. The link to the stream, the chat reads
//! and the poller are all futures this driver owns and races in one
//! `select!`, so the stream's body is polled all the time and no REST call
//! is awaited in line with it. The poller and the stream's mapper write to
//! one table of what was said, so a change that arrives by both ways is
//! said once.

use crate::client::WuapiClient;
use crate::config::StreamTuning;
use crate::envelope::{ChatReads, Mapper};
use crate::events::{EventSource, PollState, Poller, PollingEventSource, Shared};
use crate::follow::Now;
use crate::stream::{Gate, Limits, Link, LinkEvent, Refusal, StreamTransport};
use async_trait::async_trait;
use client_provider::{EventStream, ProviderError, ProviderEvent, ProviderResult};
use futures::future::BoxFuture;
use futures::StreamExt;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;
use wuapi::types as api;

/// Which transport serves events right now, and why not the stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Stream,
    /// The stream dropped and is being re-opened.
    Reconnecting,
    Polling(Why),
}

/// Why polling serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Why {
    /// The deployment has no stream (yet).
    Unavailable,
    /// The organization has all the stream connections it may open.
    ConnectionLimit,
    /// The stream refused this key.
    Refused,
    /// The stream keeps failing.
    Failing,
}

impl From<Status> for client_provider::LiveUpdates {
    fn from(status: Status) -> Self {
        use client_provider::{LiveUpdates, PollingReason};
        match status {
            Status::Stream => LiveUpdates::Stream,
            Status::Reconnecting => LiveUpdates::Reconnecting,
            Status::Polling(why) => LiveUpdates::Polling(match why {
                Why::Unavailable => PollingReason::Unavailable,
                Why::ConnectionLimit => PollingReason::ConnectionLimit,
                Why::Refused => PollingReason::Refused,
                Why::Failing => PollingReason::Failing,
            }),
        }
    }
}

/// A message the safety poll finds that is this old, and the stream never
/// said, is a sign the stream is silent.
const SILENT_AFTER_MS: i64 = 10_000;

/// Safety polls in a row that find such a message before the poller goes
/// back to full cadence.
const STRIKES: u32 = 2;

/// What is remembered between two `open`s.
#[derive(Default)]
struct Memory {
    /// The stream was refused until then, for this reason: the next `open`
    /// does not probe it.
    refusal: Option<(Instant, Why)>,
    /// The API said the key is dead.
    revoked: Option<String>,
}

pub(crate) struct LiveEventSource {
    polling: PollingEventSource,
    client: Arc<WuapiClient>,
    /// `None` when there is no usable stream address.
    transport: Option<Arc<dyn StreamTransport>>,
    tuning: StreamTuning,
    status: Arc<Mutex<Status>>,
    memory: Arc<Mutex<Memory>>,
    /// The connect budget, for every open.
    gate: Gate,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What a refusal of the stream comes to.
struct Plan {
    why: Why,
    /// The link connects again this long from now, instead of by itself.
    restart: Option<Duration>,
    /// The next `open` does not probe for this long.
    remember: Option<Duration>,
    /// Polling takes over now, not after the failures or the time.
    at_once: bool,
}

/// What a refusal means. `Err` is a key the API itself says is dead.
async fn plan(
    client: &WuapiClient,
    tuning: &StreamTuning,
    refusal: &Refusal,
) -> ProviderResult<Plan> {
    let reprobe = tuning.reprobe;
    let waits = |named: Option<Duration>, default: u64| {
        named
            .unwrap_or(Duration::from_secs(default))
            .min(tuning.retry_after_max)
    };
    Ok(match refusal.status {
        // The API says the key is dead; the host in front of it might be
        // lying, so the API's own `/v1/me` has the last word.
        401 if refusal.api_shaped => match client.me().await {
            Err(error @ ProviderError::Unauthorized(_)) => return Err(error),
            _ => Plan::reprobe(Why::Unavailable, reprobe),
        },
        // Somebody else's page.
        401 => Plan::reprobe(Why::Unavailable, reprobe),
        403 => Plan::reprobe(Why::Refused, reprobe),
        429 if refusal.code.as_deref() == Some("stream_connection_limit") => Plan {
            why: Why::ConnectionLimit,
            restart: None,
            remember: Some(waits(refusal.retry_after, 30)),
            at_once: true,
        },
        429 => Plan {
            why: Why::Failing,
            restart: None,
            remember: Some(waits(refusal.retry_after, 60)),
            at_once: false,
        },
        408 | 500..=599 => Plan {
            why: Why::Failing,
            restart: None,
            remember: None,
            at_once: false,
        },
        _ => Plan::reprobe(Why::Unavailable, reprobe),
    })
}

impl Plan {
    /// The stream is not there for now: ask again after `wait`.
    fn reprobe(why: Why, wait: Duration) -> Self {
        Self {
            why,
            restart: Some(wait),
            remember: Some(wait),
            at_once: true,
        }
    }

    /// Carries the plan out on the link and in memory.
    fn apply(&self, link: &mut Link, memory: &Mutex<Memory>) {
        let now = Instant::now();
        if let Some(wait) = self.restart {
            link.restart_at(now + wait, false);
        }
        if let Some(wait) = self.remember {
            locked(memory).refusal = Some((now + wait, self.why));
        }
        if self.at_once {
            link.clear_cursor();
        }
    }
}

impl LiveEventSource {
    pub(crate) fn new(
        client: Arc<WuapiClient>,
        polling: PollingEventSource,
        transport: Option<Arc<dyn StreamTransport>>,
        tuning: StreamTuning,
    ) -> Self {
        Self {
            polling,
            client,
            transport,
            gate: Gate::new(&tuning),
            tuning,
            status: Arc::new(Mutex::new(Status::Polling(Why::Unavailable))),
            memory: Arc::default(),
        }
    }

    /// How events reach the client right now.
    pub(crate) fn status(&self) -> Status {
        *locked(&self.status)
    }

    /// A link over the source's shared budget.
    fn link(&self, transport: &Arc<dyn StreamTransport>) -> Link {
        Link::with_budget(
            transport.clone(),
            &self.tuning,
            self.limits(),
            self.gate.budget(),
        )
    }

    fn limits(&self) -> Limits {
        Limits {
            give_up: None,
            fallback: Some((self.tuning.fallback_failures, self.tuning.fallback_after)),
        }
    }
}

#[async_trait]
impl EventSource for LiveEventSource {
    async fn open(&self) -> ProviderResult<EventStream> {
        if let Some(message) = locked(&self.memory).revoked.clone() {
            return Err(ProviderError::Unauthorized(message));
        }
        let state = Arc::new(Mutex::new(PollState::default()));
        let shared = self.polling.shared();
        let mut poller = self.polling.poller_with(state.clone());
        let mut queue = VecDeque::new();

        let remembered = locked(&self.memory)
            .refusal
            .filter(|(until, _)| *until > Instant::now());
        let mut link = None;
        let mut live = false;
        let mut why = Why::Unavailable;
        match (&self.transport, remembered) {
            (None, _) => {}
            // Refused not long ago: no probe, polling at once.
            (Some(transport), Some((until, reason))) => {
                let mut stopped = self.link(transport);
                stopped.restart_at(until, false);
                why = reason;
                link = Some(stopped);
            }
            // Probed too often, over all the opens so far: no probe now,
            // polling at once, and the link tries when the budget allows.
            (Some(transport), None) if self.gate.closed().is_some() => {
                let mut stopped = self.link(transport);
                stopped.restart_at(self.gate.next_slot(Instant::now()), false);
                why = Why::Failing;
                link = Some(stopped);
            }
            (Some(transport), None) => {
                let mut probing = self.link(transport);
                loop {
                    match probing.next().await {
                        LinkEvent::Up => {
                            live = true;
                            break;
                        }
                        LinkEvent::Refused(refusal) => {
                            let plan = plan(&self.client, &self.tuning, &refusal).await?;
                            plan.apply(&mut probing, &self.memory);
                            why = plan.why;
                            break;
                        }
                        LinkEvent::Unreachable => {
                            why = Why::Failing;
                            break;
                        }
                        _ => {}
                    }
                }
                link = Some(probing);
            }
        }
        if live {
            locked(&self.memory).refusal = None;
            *locked(&self.status) = Status::Stream;
        } else {
            // Polling serves: prime it now, as the polling source does.
            // What exists at this moment is history, which the client
            // reads through `list_chats` / `fetch_messages`.
            *locked(&self.status) = Status::Polling(why);
            queue.extend(poller.poll(Now::real()).await?);
            poller.settle();
        }

        let run = Auto {
            link,
            mapper: Mapper::new(state.clone(), shared.clone(), self.tuning.evt_lru),
            reads: ChatReads::new(self.client.clone(), state, &self.tuning),
            queue,
            poller: Some(poller),
            job: None,
            shared,
            live,
            full_cadence: false,
            strikes: 0,
            first_safety: live,
            poll_asap: false,
            // The first safety poll runs right away, not awaited by `open`.
            next_safety: Instant::now(),
            source: Source {
                client: self.client.clone(),
                tuning: self.tuning.clone(),
                status: self.status.clone(),
                memory: self.memory.clone(),
            },
        };
        let stream = futures::stream::unfold(run, |mut run| async move {
            let event = run.next().await?;
            Some((event, run))
        });
        Ok(stream.boxed())
    }

    fn accepted(&self, message: &api::Message) {
        self.polling.accepted(message);
    }

    fn listed(&self, messages: &[api::Message]) {
        self.polling.listed(messages);
    }

    fn live(&self) -> Option<Status> {
        Some(self.status())
    }
}

/// What a poll job returns: the poller, back, and what it found.
type Job = BoxFuture<'static, (Poller, ProviderResult<Vec<ProviderEvent>>)>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// One general poll, on the safety cadence.
    Safety,
    /// Follow-ups and the general poll, on the full cadence.
    Full,
}

/// The shared parts the running driver needs from its source.
struct Source {
    client: Arc<WuapiClient>,
    tuning: StreamTuning,
    status: Arc<Mutex<Status>>,
    memory: Arc<Mutex<Memory>>,
}

/// One open stream of events.
struct Auto {
    link: Option<Link>,
    mapper: Mapper,
    reads: ChatReads,
    queue: VecDeque<ProviderEvent>,
    /// `None` while a job has it.
    poller: Option<Poller>,
    job: Option<(Kind, Job)>,
    shared: Arc<Shared>,
    /// The stream serves; otherwise polling does.
    live: bool,
    /// The stream is up but silent: the poller asks as often as it does
    /// in fallback.
    full_cadence: bool,
    strikes: u32,
    /// The next safety poll is the first: it primes, and finds history.
    first_safety: bool,
    /// A full poll is wanted now.
    poll_asap: bool,
    next_safety: Instant,
    source: Source,
}

/// What one turn of the driver's wait came to.
enum Got {
    Link(LinkEvent),
    Read(ProviderResult<Vec<ProviderEvent>>),
    Job((Poller, ProviderResult<Vec<ProviderEvent>>)),
    Tick,
}

async fn next_link(link: &mut Option<Link>) -> LinkEvent {
    match link {
        Some(link) => link.next().await,
        None => std::future::pending().await,
    }
}

async fn next_job(job: &mut Option<(Kind, Job)>) -> (Poller, ProviderResult<Vec<ProviderEvent>>) {
    match job {
        Some((_, job)) => job.await,
        None => std::future::pending().await,
    }
}

impl Auto {
    fn set_status(&self, status: Status) {
        *locked(&self.source.status) = status;
    }

    fn revoke(&self, message: String) {
        locked(&self.source.memory).revoked = Some(message);
    }

    /// Whether the poller asks about followed messages: only when the
    /// stream does not tell.
    fn follows(&self) -> bool {
        !self.live || self.full_cadence
    }

    /// When the poller next has something to do.
    fn due(&self, now: Instant) -> Option<Instant> {
        let poller = self.poller.as_ref()?;
        if self.poll_asap {
            return Some(now);
        }
        Some(if self.follows() {
            poller.next_wake(now)
        } else {
            self.next_safety
        })
    }

    fn start_job(&mut self) {
        let Some(mut poller) = self.poller.take() else {
            return;
        };
        let now = Now::real();
        if self.follows() {
            if std::mem::take(&mut self.poll_asap) {
                poller.schedule(now.at);
            }
            self.job = Some((
                Kind::Full,
                Box::pin(async move {
                    let result = poller.step(now).await.map(|()| poller.drain());
                    (poller, result)
                }),
            ));
        } else {
            self.job = Some((
                Kind::Safety,
                Box::pin(async move {
                    let result = poller.poll(now).await;
                    (poller, result)
                }),
            ));
        }
    }

    /// Polling takes over from the stream.
    fn enter_fallback(&mut self, why: Why) {
        if self.live {
            tracing::info!(?why, "the event stream is not serving; polling instead");
        }
        self.live = false;
        self.full_cadence = false;
        self.poll_asap = true;
        self.set_status(Status::Polling(why));
        if let Some(link) = &mut self.link {
            // What the poller reads covers the gap: the next connect starts
            // fresh.
            link.clear_cursor();
        }
    }

    /// Waits for the next event; `None` is the end of the stream.
    async fn next(&mut self) -> Option<ProviderEvent> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Some(event);
            }
            let now = Instant::now();
            let due = if self.job.is_none() {
                self.due(now)
            } else {
                None
            };
            if due.is_some_and(|due| due <= now) {
                self.start_job();
                continue;
            }
            let follows = self.follows() && self.job.is_none();
            let got = {
                let Auto {
                    link,
                    reads,
                    job,
                    shared,
                    ..
                } = &mut *self;
                tokio::select! {
                    event = next_link(link) => Got::Link(event),
                    read = reads.next() => Got::Read(read),
                    done = next_job(job), if job.is_some() => Got::Job(done),
                    () = tokio::time::sleep_until(due.unwrap_or(now)), if due.is_some() => Got::Tick,
                    // A send was accepted: a follow-up may be due sooner.
                    () = shared.rung(), if follows => Got::Tick,
                }
            };
            match got {
                Got::Tick => {}
                Got::Read(Ok(events)) => self.queue.extend(events),
                Got::Read(Err(error)) => {
                    if let ProviderError::Unauthorized(message) = error {
                        self.revoke(message);
                    }
                    return None;
                }
                Got::Job((poller, result)) => {
                    self.poller = Some(poller);
                    let kind = self.job.take().map(|(kind, _)| kind)?;
                    if !self.finish_job(kind, result) {
                        return None;
                    }
                }
                Got::Link(event) => {
                    if !self.on_link(event).await {
                        return None;
                    }
                }
            }
        }
    }

    /// A poll came back. `false` ends the stream.
    fn finish_job(&mut self, kind: Kind, result: ProviderResult<Vec<ProviderEvent>>) -> bool {
        let tuning = &self.source.tuning;
        let again = Instant::now() + tuning.safety_poll;
        match result {
            Err(ProviderError::Unauthorized(message)) => {
                tracing::warn!("wuapi rejected the API key; event polling stops");
                self.revoke(message);
                return false;
            }
            Ok(events) => {
                if kind == Kind::Safety {
                    self.next_safety = again;
                    self.check_silence(&events);
                }
                self.queue.extend(events);
            }
            Err(ProviderError::RateLimited { retry_after }) if kind == Kind::Safety => {
                let wait = retry_after.unwrap_or(tuning.safety_poll);
                self.next_safety = Instant::now() + wait.min(tuning.retry_after_max);
            }
            Err(error) => {
                tracing::debug!(%error, "a poll failed; trying again later");
                if kind == Kind::Safety {
                    self.next_safety = again;
                }
            }
        }
        true
    }

    /// A safety poll that finds a message the stream never said, and that is
    /// not new, says the stream is silent while its pings go on (the
    /// backend's kill switch). Twice in a row and the poller asks as often
    /// as it does in fallback, until the stream says something.
    fn check_silence(&mut self, found: &[ProviderEvent]) {
        if std::mem::take(&mut self.first_safety) {
            return;
        }
        let now = client_provider::Timestamp::now().as_millis();
        let stale = found.iter().any(|event| {
            matches!(event, ProviderEvent::MessageUpserted(message)
                if now - message.timestamp.as_millis() > SILENT_AFTER_MS)
        });
        if !stale {
            self.strikes = 0;
            return;
        }
        self.strikes += 1;
        if self.strikes >= STRIKES && !self.full_cadence {
            tracing::info!("the event stream is silent; polling at full cadence meanwhile");
            self.full_cadence = true;
            self.poll_asap = true;
        }
    }

    /// The link said something. `false` ends the stream.
    async fn on_link(&mut self, event: LinkEvent) -> bool {
        match event {
            LinkEvent::Up => {
                locked(&self.source.memory).refusal = None;
                self.set_status(Status::Stream);
                if !self.live {
                    // The stream is back: one more poll closes the seam, then
                    // the poller drops to the safety cadence.
                    self.live = true;
                    self.full_cadence = false;
                    self.strikes = 0;
                    self.first_safety = false;
                    self.next_safety = Instant::now();
                }
            }
            LinkEvent::Data(data) => {
                let mapped = self.mapper.map(&data, Now::real());
                self.queue.extend(mapped.events);
                for mark in mapped.marks {
                    self.reads.mark(mark);
                }
                if self.full_cadence {
                    // It spoke: no kill switch after all.
                    self.full_cadence = false;
                    self.strikes = 0;
                    self.next_safety = Instant::now() + self.source.tuning.safety_poll;
                }
            }
            LinkEvent::Dropped | LinkEvent::Unreachable => {
                if self.live {
                    self.set_status(Status::Reconnecting);
                }
            }
            LinkEvent::Fallback | LinkEvent::GaveUp => self.enter_fallback(Why::Failing),
            // The cursor is gone: the engine subscribes again and refreshes.
            LinkEvent::Reset => return false,
            LinkEvent::Refused(refusal) => return self.on_refused(&refusal).await,
        }
        true
    }

    async fn on_refused(&mut self, refusal: &Refusal) -> bool {
        let plan = match plan(&self.source.client, &self.source.tuning, refusal).await {
            Ok(plan) => plan,
            Err(ProviderError::Unauthorized(message)) => {
                self.revoke(message);
                return false;
            }
            Err(_) => return true,
        };
        if let Some(link) = &mut self.link {
            plan.apply(link, &self.source.memory);
        }
        if self.live {
            if plan.at_once {
                self.enter_fallback(plan.why);
            } else {
                self.set_status(Status::Reconnecting);
            }
        } else {
            self.set_status(Status::Polling(plan.why));
        }
        true
    }
}
