//! The connection to the event stream: reconnecting, resuming and pacing.

use crate::client::WuapiClient;
use crate::config::{ApiKey, StreamTuning, WuapiConfig};
use crate::envelope::{ChatReads, Mapper};
use crate::events::{EventSource, PollState};
use crate::follow::Now;
use crate::live::Status;
use crate::sse::{SseItem, SseParser};
use async_trait::async_trait;
use client_provider::{EventStream, ProviderError, ProviderEvent, ProviderResult};
use futures::future::BoxFuture;
use futures::StreamExt;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

pub(crate) fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// An answer to a connect that is not the stream: its status and what
/// helps to tell whose it is. The body is capped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Refused {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub retry_after: Option<Duration>,
}

/// What a connect came to, when it got an answer.
pub(crate) enum Answer {
    /// `200` with `Content-Type: text/event-stream`.
    Stream(Box<dyn StreamBody>),
    /// Anything else, a `200` of another type included.
    Refused(Refused),
}

/// The body of an open stream.
///
/// `chunk` must lose nothing when its future is dropped: the link polls it
/// against timers.
#[async_trait]
pub(crate) trait StreamBody: Send {
    /// The next bytes, `None` at a clean end, `Err` when the connection
    /// broke or went quiet.
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String>;
}

/// Opens the stream. `Err` is a connect that got no answer: network, TLS,
/// timeout.
pub(crate) trait StreamTransport: Send + Sync {
    /// One connect, with `Last-Event-ID` when given. The future owns what it
    /// needs, so the link can hold it across polls.
    fn connect(&self, last_event_id: Option<String>) -> BoxFuture<'static, Result<Answer, String>>;
}

/// How much of an error body is read: a `{code, message}` is a few hundred
/// bytes.
const REFUSED_BODY_MAX: usize = 64 * 1024;

/// The real transport: its own HTTP client, not the SDK's pool, because
/// the API client's total timeout would kill a stream.
pub(crate) struct ReqwestStreamTransport {
    client: reqwest::Client,
    endpoint: String,
    key: ApiKey,
    open: Duration,
}

impl ReqwestStreamTransport {
    pub(crate) fn new(config: &WuapiConfig, key: ApiKey) -> ProviderResult<Self> {
        let endpoint = config
            .stream_endpoint()
            .map_err(|why| ProviderError::Rejected {
                code: "invalid_stream_url".into(),
                message: format!("The event stream address is not usable: {why}"),
            })?;
        let client = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(config.connect_timeout)
            // The body is read for as long as it lasts: no `.timeout()`,
            // which would cut a healthy stream. Silence is the idle limit.
            .read_timeout(config.tuning.idle)
            // A redirect is an answer, never a second request: the key
            // goes to the endpoint's origin and nowhere else.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| ProviderError::Protocol(format!("could not set up HTTP: {e}")))?;
        Ok(Self {
            client,
            endpoint,
            key,
            open: config.tuning.open,
        })
    }
}

impl ReqwestStreamTransport {
    /// The request for `url`, with the key only if `url` is of the origin
    /// the endpoint was fixed to.
    pub(crate) fn request(
        &self,
        url: &str,
        last_event_id: Option<String>,
    ) -> reqwest::RequestBuilder {
        let mut request = self
            .client
            .get(url)
            .header("Accept", "text/event-stream")
            .header("Cache-Control", "no-cache");
        if attaches_bearer(&self.endpoint, url) {
            request = request.bearer_auth(self.key.expose());
        }
        if let Some(id) = last_event_id {
            request = request.header("Last-Event-ID", id);
        }
        request
    }
}

impl StreamTransport for ReqwestStreamTransport {
    fn connect(&self, last_event_id: Option<String>) -> BoxFuture<'static, Result<Answer, String>> {
        let request = self.request(&self.endpoint, last_event_id);
        let open = self.open;
        Box::pin(async move {
            // Bounded up to the headers, and a refusal's small body.
            let first = tokio::time::timeout(open, async {
                let response = request
                    .send()
                    .await
                    .map_err(|e| crate::http::describe(e.without_url()))?;
                let status = response.status().as_u16();
                let header = |name: &str| {
                    response
                        .headers()
                        .get(name)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned)
                };
                let content_type = header("content-type");
                let is_stream = content_type.as_deref().is_some_and(|value| {
                    value
                        .trim_start()
                        .to_ascii_lowercase()
                        .starts_with("text/event-stream")
                });
                if status == 200 && is_stream {
                    return Ok(Answer::Stream(Box::new(ReqwestBody(response))));
                }
                let retry_after = header("retry-after")
                    .and_then(|v| v.trim().parse().ok())
                    .map(Duration::from_secs);
                let mut response = response;
                let mut body = Vec::new();
                while body.len() < REFUSED_BODY_MAX {
                    match response.chunk().await {
                        Ok(Some(bytes)) => body.extend_from_slice(&bytes),
                        _ => break,
                    }
                }
                body.truncate(REFUSED_BODY_MAX);
                Ok(Answer::Refused(Refused {
                    status,
                    content_type,
                    body,
                    retry_after,
                }))
            })
            .await;
            first.unwrap_or_else(|_| Err("the request timed out".to_owned()))
        })
    }
}

struct ReqwestBody(reqwest::Response);

#[async_trait]
impl StreamBody for ReqwestBody {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, String> {
        self.0
            .chunk()
            .await
            .map(|bytes| bytes.map(|bytes| bytes.to_vec()))
            .map_err(|e| crate::http::describe(e.without_url()))
    }
}

/// Whether the key may go to `url`: only to the origin the endpoint was
/// fixed to.
pub(crate) fn attaches_bearer(endpoint: &str, url: &str) -> bool {
    match (crate::http::origin(endpoint), crate::http::origin(url)) {
        (Some(fixed), Some(asked)) => fixed == asked,
        _ => false,
    }
}

/// The wait before the next connect, from the failures so far.
pub(crate) struct Backoff {
    base: Duration,
    cap: Duration,
    min_gap: Duration,
    stable_after: Duration,
    retry_max: Duration,
    retry_after_max: Duration,
    jitter: fn() -> f64,
    failures: u32,
}

impl Backoff {
    pub(crate) fn new(tuning: &StreamTuning) -> Self {
        Self {
            base: tuning.backoff_base,
            cap: tuning.backoff_cap,
            min_gap: tuning.min_gap,
            stable_after: tuning.stable_after,
            retry_max: tuning.retry_max,
            retry_after_max: tuning.retry_after_max,
            jitter: tuning.jitter,
            failures: 0,
        }
    }

    /// A connect did not reach a stream.
    pub(crate) fn connect_failed(&mut self) {
        self.failures = self.failures.saturating_add(1);
    }

    /// A connection that reached a stream ended after `lived`.
    pub(crate) fn connection_ended(&mut self, lived: Duration) {
        if lived >= self.stable_after {
            self.failures = 0;
        } else {
            self.connect_failed();
        }
    }

    /// The consecutive failures counted so far.
    pub(crate) fn failures(&self) -> u32 {
        self.failures
    }

    /// The floor a server asked for, clamped: the larger of its `retry:` and
    /// its `Retry-After`.
    pub(crate) fn floor(&self, retry: Option<Duration>, retry_after: Option<Duration>) -> Duration {
        let retry = retry.map(|wait| wait.min(self.retry_max));
        let retry_after = retry_after.map(|wait| wait.min(self.retry_after_max));
        retry
            .unwrap_or_default()
            .max(retry_after.unwrap_or_default())
    }

    /// How long to wait before the next connect: the floor, plus a random
    /// part of the current step.
    pub(crate) fn wait(&self, floor: Duration) -> Duration {
        // The first failure is the first step, also after a stable
        // connection (a recycle or a drain is a close with n = 1).
        let n = self.failures.max(1);
        let ceiling = self
            .base
            .saturating_mul(1u32.checked_shl(n - 1).unwrap_or(u32::MAX))
            .min(self.cap);
        let draw = ((self.jitter)().clamp(0.0, 1.0)) * ceiling.as_secs_f64();
        // The floor is a least, and the draw comes on top: clients that all
        // got the same `retry:` must not all come back at the same instant.
        (floor + Duration::from_secs_f64(draw)).max(self.min_gap)
    }
}

/// How many connects a rolling window allows.
pub(crate) struct ConnectBudget {
    limit: usize,
    window: Duration,
    starts: VecDeque<Instant>,
}

impl ConnectBudget {
    pub(crate) fn new(tuning: &StreamTuning) -> Self {
        Self {
            limit: tuning.budget,
            window: tuning.budget_window,
            starts: VecDeque::new(),
        }
    }

    /// A connect starts at `now`.
    pub(crate) fn record(&mut self, now: Instant) {
        self.forget(now);
        self.starts.push_back(now);
    }

    /// The earliest a connect may start, given `now`. It only reads: `now`
    /// may be a later instant a waiting link planned for, and the connects
    /// that leave the window only by then still count for everyone else.
    pub(crate) fn next_slot(&self, now: Instant) -> Instant {
        let in_window: Vec<Instant> = self
            .starts
            .iter()
            .copied()
            .filter(|start| now.saturating_duration_since(*start) < self.window)
            .collect();
        if in_window.len() < self.limit {
            return now;
        }
        // The connect that frees a slot is the one `limit` back.
        let freeing = in_window[in_window.len() - self.limit];
        (freeing + self.window).max(now)
    }

    fn forget(&mut self, now: Instant) {
        while self
            .starts
            .front()
            .is_some_and(|start| now.saturating_duration_since(*start) >= self.window)
        {
            self.starts.pop_front();
        }
    }
}

/// Why a connect did not reach the stream, as far as the drivers care.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Refusal {
    pub status: u16,
    /// The API's `code`, when the body is the API's error.
    pub code: Option<String>,
    /// The API's `message`: what to tell the caller, not what to log.
    pub message: Option<String>,
    /// The body decodes as the API's error: it is the API that answered.
    pub api_shaped: bool,
    pub retry_after: Option<Duration>,
    /// The link tries again by itself; otherwise it has stopped and waits
    /// for its driver.
    pub retrying: bool,
}

/// What the link tells its driver.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LinkEvent {
    /// A connection reached the stream.
    Up,
    /// The `data:` of one event, `reset` apart.
    Data(String),
    /// The connection ended; the link reconnects.
    Dropped,
    /// A connect got an answer that is not the stream.
    Refused(Refusal),
    /// A connect got no answer.
    Unreachable,
    /// `event: reset`: the cursor is gone and the link has stopped.
    Reset,
    /// Down for the give-up time (strict mode); the link has stopped.
    GaveUp,
    /// Down for the failures or the time fallback asks (`Auto`); the link
    /// goes on trying, without a cursor.
    Fallback,
}

/// When the link says it is time to stop or to fall back.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Limits {
    /// Give up after being down this long.
    pub give_up: Option<Duration>,
    /// Fall back after this many failures in a row, or this long down.
    pub fallback: Option<(u32, Duration)>,
}

enum State {
    /// Waits until the instant, then connects.
    Waiting(Instant),
    Connecting {
        attempt: BoxFuture<'static, Result<Answer, String>>,
        deadline: Instant,
    },
    Live {
        body: Box<dyn StreamBody>,
        since: Instant,
        last_byte: Instant,
    },
    /// Not connecting: it ended, or waits for its driver.
    Stopped,
}

/// One logical connection to the stream: connects, reads, and reconnects
/// by itself with the cursor, backoff and budget the design lays down. It
/// never ends on its own except on `reset`, give-up, and an answer that
/// says to stop; those are events, and the link then waits for its driver.
///
/// `next` loses nothing when its future is dropped, so a driver can race
/// it against anything.
pub(crate) struct Link {
    transport: Arc<dyn StreamTransport>,
    limits: Limits,
    idle: Duration,
    open: Duration,
    max_frame: usize,
    backoff: Backoff,
    /// Shared with every other link of the same source: the budget is the
    /// source's, not the connection's.
    budget: Arc<Mutex<ConnectBudget>>,
    parser: SseParser,
    cursor: Option<String>,
    /// The last `retry:` the server sent.
    retry: Option<Duration>,
    state: State,
    queue: VecDeque<LinkEvent>,
    /// Since when no connection is up; `None` while one is.
    down_since: Option<Instant>,
    /// `Fallback` was said for this time down.
    fell_back: bool,
}

impl Link {
    /// A link with a budget of its own that connects at once.
    #[cfg(test)]
    pub(crate) fn new(
        transport: Arc<dyn StreamTransport>,
        tuning: &StreamTuning,
        limits: Limits,
    ) -> Self {
        let budget = Arc::new(Mutex::new(ConnectBudget::new(tuning)));
        Self::with_budget(transport, tuning, limits, budget)
    }

    /// A link that connects as soon as `budget` has a slot.
    pub(crate) fn with_budget(
        transport: Arc<dyn StreamTransport>,
        tuning: &StreamTuning,
        limits: Limits,
        budget: Arc<Mutex<ConnectBudget>>,
    ) -> Self {
        let now = Instant::now();
        let first = locked(&budget).next_slot(now);
        Self {
            transport,
            limits,
            idle: tuning.idle,
            open: tuning.open,
            max_frame: tuning.max_frame,
            backoff: Backoff::new(tuning),
            budget,
            parser: SseParser::new(tuning.max_frame),
            cursor: None,
            retry: None,
            state: State::Waiting(first),
            queue: VecDeque::new(),
            down_since: Some(now),
            fell_back: false,
        }
    }

    /// The last cursor, `None` when there is none.
    #[cfg(test)]
    pub(crate) fn cursor(&self) -> Option<&str> {
        self.cursor.as_deref()
    }

    /// A stopped link connects again at `at`, with its cursor or without.
    pub(crate) fn restart_at(&mut self, at: Instant, keep_cursor: bool) {
        if !keep_cursor {
            self.cursor = None;
        }
        let at = locked(&self.budget).next_slot(at);
        self.state = State::Waiting(at);
    }

    /// Forgets the cursor: what it covered is read some other way.
    pub(crate) fn clear_cursor(&mut self) {
        self.cursor = None;
    }

    /// Whether the link has stopped and waits for its driver.
    pub(crate) fn is_stopped(&self) -> bool {
        matches!(self.state, State::Stopped)
    }

    /// The next thing worth telling.
    pub(crate) async fn next(&mut self) -> LinkEvent {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return event;
            }
            if let Some(event) = self.limit_event() {
                return event;
            }
            let limit = self.limit_at();
            match &mut self.state {
                State::Stopped => std::future::pending().await,
                State::Waiting(at) => {
                    let at = *at;
                    tokio::time::sleep_until(limit.map_or(at, |limit| limit.min(at))).await;
                    if Instant::now() >= at {
                        self.start_connect();
                    }
                }
                State::Connecting { attempt, deadline } => {
                    let deadline = *deadline;
                    let answer = tokio::select! {
                        answer = attempt.as_mut() => Some(answer),
                        () = tokio::time::sleep_until(
                            limit.map_or(deadline, |limit| limit.min(deadline))
                        ) => None,
                    };
                    match answer {
                        Some(answer) => self.answered(answer),
                        None if Instant::now() >= deadline => {
                            self.answered(Err("the connection took too long".to_owned()))
                        }
                        None => {}
                    }
                }
                State::Live {
                    body, last_byte, ..
                } => {
                    let idle_at = *last_byte + self.idle;
                    let read = tokio::select! {
                        read = body.chunk() => Some(read),
                        () = tokio::time::sleep_until(idle_at) => None,
                    };
                    match read {
                        Some(Ok(Some(bytes))) => {
                            if let State::Live { last_byte, .. } = &mut self.state {
                                *last_byte = Instant::now();
                            }
                            self.feed(&bytes);
                        }
                        // A clean end, a broken connection and silence are
                        // the same thing: a drop.
                        Some(Ok(None) | Err(_)) | None => self.dropped(),
                    }
                }
            }
        }
    }

    fn start_connect(&mut self) {
        let now = Instant::now();
        locked(&self.budget).record(now);
        self.state = State::Connecting {
            attempt: self.transport.connect(self.cursor.clone()),
            deadline: now + self.open,
        };
    }

    /// Plans the next connect after a failure or a drop.
    fn schedule(&mut self, retry_after: Option<Duration>) {
        let floor = self.backoff.floor(self.retry, retry_after);
        let at = Instant::now() + self.backoff.wait(floor);
        self.state = State::Waiting(locked(&self.budget).next_slot(at));
    }

    fn answered(&mut self, answer: Result<Answer, String>) {
        let now = Instant::now();
        match answer {
            Ok(Answer::Stream(body)) => {
                self.queue.push_back(LinkEvent::Up);
                self.down_since = None;
                self.fell_back = false;
                self.parser = SseParser::new(self.max_frame);
                self.state = State::Live {
                    body,
                    since: now,
                    last_byte: now,
                };
            }
            Ok(Answer::Refused(refused)) => self.refused(refused),
            Err(why) => {
                tracing::debug!(%why, "the event stream could not be reached");
                self.backoff.connect_failed();
                self.queue.push_back(LinkEvent::Unreachable);
                self.schedule(None);
            }
        }
    }

    fn refused(&mut self, refused: Refused) {
        let parsed: Option<wuapi::types::ApiError> = serde_json::from_slice(&refused.body).ok();
        let api_shaped = parsed.is_some();
        let (code, message) = match parsed {
            Some(error) => (Some(error.code), Some(error.message)),
            None => (None, None),
        };
        let retrying = match refused.status {
            408 | 429 | 500..=599 => true,
            // A 401 that is not the API's is somebody else's page.
            401 => !api_shaped,
            _ => false,
        };
        self.queue.push_back(LinkEvent::Refused(Refusal {
            status: refused.status,
            code: code.clone(),
            message,
            api_shaped,
            retry_after: refused.retry_after,
            retrying,
        }));
        if !retrying {
            self.state = State::Stopped;
            return;
        }
        self.backoff.connect_failed();
        let floor = asked_wait(refused.status, code.as_deref(), refused.retry_after);
        self.schedule(floor);
    }

    /// The connection ended.
    fn dropped(&mut self) {
        let now = Instant::now();
        if let State::Live { since, .. } = &self.state {
            self.backoff
                .connection_ended(now.saturating_duration_since(*since));
        }
        self.queue.push_back(LinkEvent::Dropped);
        self.down_since = Some(now);
        self.fell_back = false;
        self.schedule(None);
    }

    fn feed(&mut self, bytes: &[u8]) {
        for item in self.parser.feed(bytes) {
            match item {
                SseItem::Event { event, data, id } => {
                    if event == "reset" {
                        // Everything before it is already queued. Nothing
                        // after it is read, and the cursor dies.
                        self.cursor = None;
                        self.queue.push_back(LinkEvent::Reset);
                        self.state = State::Stopped;
                        return;
                    }
                    self.queue.push_back(LinkEvent::Data(data));
                    self.set_cursor(id);
                }
                SseItem::Cursor(id) => self.set_cursor(Some(id)),
                SseItem::Retry(wait) => self.retry = Some(wait),
                SseItem::Comment => {}
                SseItem::Oversized => tracing::debug!("a stream frame over the cap was dropped"),
            }
        }
    }

    /// An empty id clears the cursor, as the SSE rules say.
    fn set_cursor(&mut self, id: Option<String>) {
        match id {
            Some(id) if id.is_empty() => self.cursor = None,
            Some(id) => self.cursor = Some(id),
            None => {}
        }
    }

    fn limit_event(&mut self) -> Option<LinkEvent> {
        if self.is_stopped() {
            return None;
        }
        let since = self.down_since?;
        let down = Instant::now().saturating_duration_since(since);
        if self.limits.give_up.is_some_and(|limit| down >= limit) {
            self.state = State::Stopped;
            return Some(LinkEvent::GaveUp);
        }
        if let Some((failures, after)) = self.limits.fallback {
            if !self.fell_back && (self.backoff.failures() >= failures || down >= after) {
                self.fell_back = true;
                // The poller covers the gap: the next connect starts fresh.
                self.cursor = None;
                return Some(LinkEvent::Fallback);
            }
        }
        None
    }

    /// The next instant a limit may be reached by time alone.
    fn limit_at(&self) -> Option<Instant> {
        if self.is_stopped() {
            return None;
        }
        let since = self.down_since?;
        let give_up = self.limits.give_up.map(|limit| since + limit);
        let fall_back = match self.limits.fallback {
            Some((_, after)) if !self.fell_back => Some(since + after),
            _ => None,
        };
        give_up.into_iter().chain(fall_back).min()
    }
}

/// How long a refusal asks for before the next connect: its `Retry-After`,
/// else the spec's default for a 429 (30 s at the connection limit, 60 s
/// otherwise).
pub(crate) fn asked_wait(
    status: u16,
    code: Option<&str>,
    retry_after: Option<Duration>,
) -> Option<Duration> {
    match status {
        429 => Some(
            retry_after.unwrap_or(if code == Some("stream_connection_limit") {
                Duration::from_secs(30)
            } else {
                Duration::from_secs(60)
            }),
        ),
        _ => retry_after,
    }
}

/// What a refusal of the very first connect is to the caller, in strict
/// mode.
fn refusal_error(refusal: &Refusal) -> ProviderError {
    if refusal.status == 401 && !refusal.api_shaped {
        // Not the API's answer: nobody is signed out for it.
        return ProviderError::Transient(
            "the stream host answered 401, but it is not the API".into(),
        );
    }
    crate::error::classify(
        refusal.status,
        refusal
            .code
            .clone()
            .unwrap_or_else(|| format!("http_{}", refusal.status)),
        refusal
            .message
            .clone()
            .unwrap_or_else(|| format!("HTTP {}", refusal.status)),
        refusal.retry_after,
    )
}

/// What every `open()` of one source shares: the connect budget and the last
/// failure. The engine opens again each time a stream ends or fails; without
/// this, each open would be a fresh start and the budget would never bite.
pub(crate) struct Gate {
    budget: Arc<Mutex<ConnectBudget>>,
    retry_after_max: Duration,
    held: Mutex<Held>,
}

#[derive(Default)]
struct Held {
    /// No connect before this instant: the server asked to wait.
    until: Option<Instant>,
    /// What the last open said.
    error: Option<ProviderError>,
}

impl Gate {
    pub(crate) fn new(tuning: &StreamTuning) -> Self {
        Self {
            budget: Arc::new(Mutex::new(ConnectBudget::new(tuning))),
            retry_after_max: tuning.retry_after_max,
            held: Mutex::default(),
        }
    }

    /// The budget every link of the source counts its connects in.
    pub(crate) fn budget(&self) -> Arc<Mutex<ConnectBudget>> {
        self.budget.clone()
    }

    /// The earliest the budget lets a connect start.
    pub(crate) fn next_slot(&self, now: Instant) -> Instant {
        locked(&self.budget).next_slot(now)
    }

    /// Why no connect may start now, if so: the last error, without
    /// connecting.
    pub(crate) fn closed(&self) -> Option<ProviderError> {
        let now = Instant::now();
        let held = locked(&self.held);
        let asked_to_wait = held.until.is_some_and(|until| until > now);
        if !asked_to_wait && self.next_slot(now) <= now {
            return None;
        }
        Some(held.error.clone().unwrap_or_else(|| {
            ProviderError::Transient("the event stream was opened too often; waiting".into())
        }))
    }

    /// An open failed with `error`; no connect for `pause` (clamped).
    pub(crate) fn failed(&self, error: ProviderError, pause: Duration) {
        *locked(&self.held) = Held {
            until: Some(Instant::now() + pause.min(self.retry_after_max)),
            error: Some(error),
        };
    }

    /// An open reached the stream.
    pub(crate) fn worked(&self) {
        *locked(&self.held) = Held::default();
    }
}

/// The strict source: the stream and nothing else. No poller, no fallback.
pub(crate) struct StreamEventSource {
    client: Arc<WuapiClient>,
    transport: Arc<dyn StreamTransport>,
    tuning: StreamTuning,
    /// The API said the key is dead, mid-stream: the next `open` says so
    /// without connecting.
    revoked: Arc<Mutex<Option<String>>>,
    status: Arc<Mutex<Status>>,
    /// The budget and the last failure, for every open.
    gate: Gate,
}

impl StreamEventSource {
    pub(crate) fn new(
        client: Arc<WuapiClient>,
        transport: Arc<dyn StreamTransport>,
        tuning: StreamTuning,
    ) -> Self {
        Self {
            client,
            transport,
            gate: Gate::new(&tuning),
            tuning,
            revoked: Arc::default(),
            status: Arc::new(Mutex::new(Status::Reconnecting)),
        }
    }
}

/// An open stream: the link, what it says turned into events, and the
/// chat reads those events ask for.
struct Running {
    link: Link,
    mapper: Mapper,
    reads: ChatReads,
    queue: VecDeque<ProviderEvent>,
    revoked: Arc<Mutex<Option<String>>>,
    status: Arc<Mutex<Status>>,
}

impl Running {
    fn say(&self, status: Status) {
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = status;
    }

    /// Remembers that the key was refused.
    fn revoke(&self, message: String) {
        *self.revoked.lock().unwrap_or_else(|p| p.into_inner()) = Some(message);
    }
}

#[async_trait]
impl EventSource for StreamEventSource {
    fn live(&self) -> Option<Status> {
        Some(*self.status.lock().unwrap_or_else(|p| p.into_inner()))
    }

    async fn open(&self) -> ProviderResult<EventStream> {
        let revoked = self
            .revoked
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(message) = revoked {
            return Err(ProviderError::Unauthorized(message));
        }
        // Opened again and again by the engine: the budget and a server's
        // `Retry-After` hold across opens, not only inside one.
        if let Some(error) = self.gate.closed() {
            return Err(error);
        }
        let limits = Limits {
            give_up: Some(self.tuning.give_up_after),
            fallback: None,
        };
        let mut link = Link::with_budget(
            self.transport.clone(),
            &self.tuning,
            limits,
            self.gate.budget(),
        );
        // Only once connected: what the first connect says is the answer.
        loop {
            match link.next().await {
                LinkEvent::Up => {
                    self.gate.worked();
                    break;
                }
                LinkEvent::Refused(refusal) => {
                    let error = refusal_error(&refusal);
                    let pause =
                        asked_wait(refusal.status, refusal.code.as_deref(), refusal.retry_after);
                    self.gate.failed(error.clone(), pause.unwrap_or_default());
                    return Err(error);
                }
                LinkEvent::Unreachable => {
                    let error = ProviderError::Transient("could not reach the event stream".into());
                    self.gate.failed(error.clone(), Duration::ZERO);
                    return Err(error);
                }
                LinkEvent::GaveUp => {
                    let error = ProviderError::Transient("the event stream is down".into());
                    self.gate.failed(error.clone(), Duration::ZERO);
                    return Err(error);
                }
                _ => {}
            }
        }
        // One table of what was said for this stream, shared with the chat
        // reads. There is no follow list to retire: nothing follows in
        // strict mode.
        let state = Arc::new(Mutex::new(PollState::default()));
        let running = Running {
            link,
            mapper: Mapper::new(state.clone(), Arc::default(), self.tuning.evt_lru),
            reads: ChatReads::new(self.client.clone(), state, &self.tuning),
            queue: VecDeque::new(),
            revoked: self.revoked.clone(),
            status: self.status.clone(),
        };
        *self.status.lock().unwrap_or_else(|p| p.into_inner()) = Status::Stream;
        let stream = futures::stream::unfold(running, |mut run| async move {
            loop {
                if let Some(event) = run.queue.pop_front() {
                    return Some((event, run));
                }
                tokio::select! {
                    said = run.link.next() => match said {
                        LinkEvent::Data(data) => {
                            let mapped = run.mapper.map(&data, Now::real());
                            run.queue.extend(mapped.events);
                            for mark in mapped.marks {
                                run.reads.mark(mark);
                            }
                        }
                        LinkEvent::Up => run.say(Status::Stream),
                        LinkEvent::Dropped | LinkEvent::Unreachable => run.say(Status::Reconnecting),
                        // The cursor is gone, or the stream has been down for
                        // the whole replay window: the engine starts over.
                        LinkEvent::Reset | LinkEvent::GaveUp => {
                            run.say(Status::Reconnecting);
                            return None;
                        }
                        LinkEvent::Refused(refusal) if refusal.retrying => {
                            run.say(Status::Reconnecting)
                        }
                        LinkEvent::Refused(refusal) => {
                            if refusal.status == 401 && refusal.api_shaped {
                                run.revoke(refusal.message.unwrap_or_default());
                            }
                            run.say(Status::Reconnecting);
                            return None;
                        }
                        _ => {}
                    },
                    read = run.reads.next() => match read {
                        Ok(events) => run.queue.extend(events),
                        Err(error) => {
                            if let ProviderError::Unauthorized(message) = error {
                                run.revoke(message);
                            }
                            run.say(Status::Reconnecting);
                            return None;
                        }
                    },
                }
            }
        });
        Ok(stream.boxed())
    }
}
