//! Adapter configuration and the API key wrapper.

use std::fmt;
use std::time::Duration;

/// The production API.
pub const DEFAULT_BASE_URL: &str = "https://api.wuapi.dev";

/// Root of the production event stream; the path `/v1/events/stream` is
/// appended to it.
pub const DEFAULT_STREAM_URL: &str = "https://stream.wuapi.dev";

/// How live updates reach the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LiveTransport {
    /// The stream when the deployment has it, polling when it does not.
    Auto,
    /// The stream only: an unavailable stream is an error.
    Stream,
    /// Polling only.
    Polling,
}

/// How the adapter talks to wuapi.
///
/// Nothing here names the application: the host passes its own
/// `user_agent` (and, for the keychain, its own service name).
#[derive(Clone, Debug)]
pub struct WuapiConfig {
    /// Root of the API, without a trailing slash.
    pub base_url: String,
    /// `User-Agent` of the application, e.g. `"myclient/0.1.0"`. Sent with
    /// the login and media requests; calls to `/v1` go through the wuapi
    /// SDK, which sends its own.
    pub user_agent: String,
    /// Deadline of one whole request, body included. Nothing is retried
    /// inside the adapter: an overrun is reported as transient.
    pub request_timeout: Duration,
    /// Deadline for establishing the connection.
    pub connect_timeout: Duration,
    /// Deadline of one request for a file that is still on WhatsApp: the
    /// API downloads it first, through the number's proxy, so it takes
    /// longer than any other call.
    pub media_timeout: Duration,
    /// Deadline for sending one file's bytes to storage.
    pub upload_timeout: Duration,
    /// The waits between the attempts at such a request, when it fails for
    /// a reason that may pass. One more attempt than entries.
    pub media_backoff: Vec<Duration>,
    /// How often the event stream polls for news.
    pub poll_interval: Duration,
    /// How live updates are fetched. `Auto` by default: the stream when the
    /// deployment has it, polling when it does not.
    pub live: LiveTransport,
    /// Root of the event stream, without the path. `None` derives it from
    /// `base_url`.
    pub stream_url: Option<String>,
    /// Timings and limits of the stream; tests shorten them.
    pub(crate) tuning: StreamTuning,
}

/// Every timing and limit of the event stream, in one place so tests can
/// shorten them.
#[derive(Clone, Debug)]
pub(crate) struct StreamTuning {
    /// Silence after which a connection is dead: three heartbeats.
    pub idle: Duration,
    /// Bound on getting the response headers of a connect.
    pub open: Duration,
    /// First step of the reconnect backoff.
    pub backoff_base: Duration,
    /// Ceiling of the reconnect backoff.
    pub backoff_cap: Duration,
    /// Smallest wait between two connects: full jitter may draw zero.
    pub min_gap: Duration,
    /// Connects allowed in one `budget_window`.
    pub budget: usize,
    /// The rolling window of `budget`.
    pub budget_window: Duration,
    /// Clamp on a server-sent `retry:`.
    pub retry_max: Duration,
    /// Clamp on a server-sent `Retry-After`.
    pub retry_after_max: Duration,
    /// A connection that lived this long resets the failure count.
    pub stable_after: Duration,
    /// Failures after which `Auto` falls back to polling.
    pub fallback_failures: u32,
    /// Time down after which `Auto` falls back to polling.
    pub fallback_after: Duration,
    /// Time down after which a strict stream ends: the replay window.
    pub give_up_after: Duration,
    /// Period of the safety poll while the stream is live.
    pub safety_poll: Duration,
    /// How long a refusal is remembered before the stream is tried again.
    pub reprobe: Duration,
    /// Marks of one chat arriving this close are read once.
    pub dirty_window: Duration,
    /// Smallest gap between two reads of one chat.
    pub dirty_gap: Duration,
    /// Chat reads allowed per minute.
    pub dirty_per_minute: usize,
    /// Chat reads in flight at once.
    pub dirty_concurrency: usize,
    /// Pending chats above this many are read as one page per account.
    pub dirty_collapse: usize,
    /// Wait before a chat read that failed is tried again.
    pub dirty_retry: Duration,
    /// Retries of a failed chat read, after the first attempt.
    pub dirty_retries: u32,
    /// Event ids remembered to drop replays.
    pub evt_lru: usize,
    /// Largest SSE frame accepted, in bytes.
    pub max_frame: usize,
    /// How long the stream report listens.
    pub diagnose_window: Duration,
    /// A number in `[0, 1)` for the backoff's jitter; tests fix it.
    pub jitter: fn() -> f64,
}

/// A number in `[0, 1)` from the standard library's randomly seeded hasher,
/// so the backoff needs no random-number crate.
fn random_unit() -> f64 {
    use std::hash::{BuildHasher, Hasher};
    let bits = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

impl Default for StreamTuning {
    fn default() -> Self {
        let secs = Duration::from_secs;
        Self {
            idle: secs(45),
            open: secs(12),
            backoff_base: secs(1),
            backoff_cap: secs(30),
            min_gap: secs(1),
            budget: 6,
            budget_window: secs(60),
            retry_max: secs(60),
            retry_after_max: secs(5 * 60),
            stable_after: secs(30),
            fallback_failures: 3,
            fallback_after: secs(20),
            give_up_after: secs(30 * 60),
            safety_poll: secs(75),
            reprobe: secs(10 * 60),
            dirty_window: Duration::from_millis(500),
            dirty_gap: secs(2),
            dirty_per_minute: 60,
            dirty_concurrency: 4,
            dirty_collapse: 200,
            dirty_retry: secs(5),
            dirty_retries: 3,
            evt_lru: 4096,
            max_frame: 1024 * 1024,
            diagnose_window: secs(20),
            jitter: random_unit,
        }
    }
}

impl WuapiConfig {
    /// The defaults, with the given `User-Agent`.
    pub fn new(user_agent: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_owned(),
            user_agent: user_agent.into(),
            request_timeout: Duration::from_secs(20),
            connect_timeout: Duration::from_secs(10),
            media_timeout: Duration::from_secs(45),
            upload_timeout: Duration::from_secs(9 * 60),
            media_backoff: vec![Duration::from_secs(1), Duration::from_secs(4)],
            poll_interval: Duration::from_secs(4),
            live: LiveTransport::Auto,
            stream_url: None,
            tuning: StreamTuning::default(),
        }
    }

    /// The full URL of the event stream, or why there is none.
    ///
    /// `stream_url` is a root and gets the path appended. Without it, the
    /// production API has its own gateway host and any other API serves the
    /// stream from its own origin. There is no generic `api.` to `stream.`
    /// swap. A URL that fails [`check_stream_url`] is refused.
    pub(crate) fn stream_endpoint(&self) -> Result<String, String> {
        let root = match &self.stream_url {
            Some(root) => root.trim_end_matches('/').to_owned(),
            None if self.base().eq_ignore_ascii_case(DEFAULT_BASE_URL) => {
                DEFAULT_STREAM_URL.to_owned()
            }
            None => crate::http::origin(self.base())
                .ok_or_else(|| "the API address has no usable origin".to_owned())?,
        };
        check_stream_url(&root)?;
        Ok(format!("{root}{STREAM_PATH}"))
    }

    pub(crate) fn base(&self) -> &str {
        self.base_url.trim_end_matches('/')
    }
}

/// Where the event stream lives on its host.
const STREAM_PATH: &str = "/v1/events/stream";

/// Whether the event stream can be read from `url`: `Err` says why not.
///
/// The API key travels to it, so: `https` anywhere, `http` only on this
/// machine (`localhost`, `127.0.0.0/8`, `[::1]`), and no user info, query or
/// fragment (the stream's path is appended to what is given).
pub fn check_stream_url(url: &str) -> Result<(), String> {
    let (scheme, rest) = url.split_once("://").ok_or("it must start with https://")?;
    if rest.contains(['?', '#']) {
        return Err("it must not carry a query or a fragment".to_owned());
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return Err("it must not carry a user name or password".to_owned());
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(inner) => {
            let (host, after) = inner.split_once(']').ok_or("the address is not valid")?;
            (format!("[{host}]"), after.strip_prefix(':'))
        }
        None => match authority.rsplit_once(':') {
            Some((host, port)) => (host.to_owned(), Some(port)),
            None => (authority.to_owned(), None),
        },
    };
    if host.is_empty() || host == "[]" {
        return Err("it has no host".to_owned());
    }
    if port.is_some_and(|port| port.parse::<u16>().is_err()) {
        return Err("the port is not valid".to_owned());
    }
    match scheme.to_ascii_lowercase().as_str() {
        "https" => Ok(()),
        "http" if is_loopback(&host) => Ok(()),
        "http" => Err("plain http is only for this machine; use https://".to_owned()),
        _ => Err("it must start with https://".to_owned()),
    }
}

/// `localhost`, `127.0.0.0/8` or `[::1]`.
fn is_loopback(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host == "localhost" {
        return true;
    }
    match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(v6) => v6
            .parse::<std::net::Ipv6Addr>()
            .is_ok_and(|a| a.is_loopback()),
        None => host
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|a| a.is_loopback()),
    }
}

/// A wuapi API key (`wu_live_...`).
///
/// Wrapped so it cannot end up in a log by accident: `Debug` prints only
/// the public prefix.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key.
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key itself. Only for building the `Authorization` header or
    /// handing it to the keychain.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix: String = self.0.chars().take(12).collect();
        write!(f, "ApiKey({prefix}…)")
    }
}

impl From<String> for ApiKey {
    fn from(key: String) -> Self {
        Self(key)
    }
}

impl From<&str> for ApiKey {
    fn from(key: &str) -> Self {
        Self(key.to_owned())
    }
}
