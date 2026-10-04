//! The owner's acceptance criteria for the event stream, as tests against
//! the local stand-ins: a mock API, a [`FakeStream`] on a real socket (the
//! real transport talks to it) and, where it says so, the real sync engine.
//!
//! Waits are shortened by rewriting the `retry:` lines of the fixtures; the
//! exact 3 s and 7 s are proven on the paused clock in `drain_*`.

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

use crate::config::{ApiKey, LiveTransport, StreamTuning, WuapiConfig};
use crate::provider::WuapiProvider;
use crate::tests::envelope::say;
use crate::tests::fake_stream::{write, FakeStream, Script, Step};
use crate::tests::{on, reply, requests, target, ACCOUNT, KEY};
use client_core::{HistoryMode, Store, SyncConfig, SyncEngine};
use client_provider::{AccountId, ChatId, DeliveryStatus, LiveUpdates, PollingReason, Provider};
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer};

const CHAT: &str = "+584245550199";
const EMPTY: &str = r#"{"object":"list","items":[],"nextCursor":null}"#;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn quiet() -> f64 {
    0.0
}

/// The design's tuning with the reconnect waits cut down to real-clock size.
fn tuning() -> StreamTuning {
    StreamTuning {
        jitter: quiet,
        backoff_base: ms(5),
        backoff_cap: ms(20),
        min_gap: ms(5),
        budget: 1000,
        ..StreamTuning::default()
    }
}

fn read(name: &str) -> String {
    std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/stream")
            .join(name),
    )
    .unwrap()
}

/// A fixture whose `retry:` lines are 20 ms, so a reconnect is not a wait
/// of seconds on the real clock.
fn quick(name: &str) -> String {
    read(name)
        .replace("retry: 3000", "retry: 20")
        .replace("retry: 7000", "retry: 20")
}

/// One account, no chats, an empty message listing: the engine's refresh
/// and the poller both find nothing.
async fn api() -> MockServer {
    let server = MockServer::start().await;
    let accounts = format!(
        r#"{{"object":"list","items":[{}],"nextCursor":null}}"#,
        fixture!("account")
    );
    for (at, body) in [
        ("/v1/accounts".to_owned(), accounts),
        (format!("/v1/accounts/{ACCOUNT}/chats"), EMPTY.to_owned()),
        ("/v1/messages".to_owned(), EMPTY.to_owned()),
    ] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(reply(200, &body))
            .mount(&server)
            .await;
    }
    server
}

/// The poller's listings: `GET /v1/messages` for the newest messages of
/// every chat. The engine's history reads name a chat and are not polls.
async fn polls(server: &MockServer) -> usize {
    requests(server)
        .await
        .iter()
        .map(target)
        .filter(|t| t.starts_with("/v1/messages?") && !t.contains("chatId="))
        .count()
}

/// How often the engine (or the poller) listed the accounts.
async fn account_listings(server: &MockServer) -> usize {
    requests(server)
        .await
        .iter()
        .map(target)
        .filter(|t| t.starts_with("/v1/accounts?"))
        .count()
}

struct Rig {
    api: MockServer,
    fake: FakeStream,
    engine: SyncEngine,
}

/// A real provider, over the real transport, aimed at `fake`, under a real
/// engine that has been started.
async fn rig(live: LiveTransport, scripts: Vec<Script>) -> Rig {
    rig_paced(live, scripts, tuning(), (ms(20), ms(50))).await
}

/// The same with the stream's tuning and the engine's `(retry_base,
/// retry_max)` given.
async fn rig_paced(
    live: LiveTransport,
    scripts: Vec<Script>,
    tuning: StreamTuning,
    (retry_base, retry_max): (Duration, Duration),
) -> Rig {
    let api = api().await;
    let fake = FakeStream::start(scripts).await;
    let config = WuapiConfig {
        base_url: api.uri(),
        stream_url: Some(fake.uri()),
        live,
        // A poller, if one were built, would ask every 20 ms.
        poll_interval: ms(20),
        tuning,
        ..WuapiConfig::new("test-agent/1.0")
    };
    let provider = WuapiProvider::new(config, ApiKey::new(KEY)).unwrap();
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(provider),
        SyncConfig {
            history: HistoryMode::OnOpen,
            retry_base,
            retry_max,
            preload_pace: ms(1),
            ..SyncConfig::default()
        },
        tokio::runtime::Handle::current(),
    );
    engine.start();
    Rig { api, fake, engine }
}

impl Rig {
    /// The bubbles of the chat, oldest first: (id, status).
    fn bubbles(&self) -> Vec<(String, DeliveryStatus)> {
        let mut found: Vec<_> = self
            .engine
            .store()
            .messages(&AccountId::new(ACCOUNT), &ChatId::new(CHAT), 100)
            .unwrap()
            .into_iter()
            .map(|m| (m.message.id.to_string(), m.message.status))
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        found
    }

    fn ids(&self) -> Vec<String> {
        self.bubbles().into_iter().map(|(id, _)| id).collect()
    }

    /// Waits until `done` holds, up to five seconds.
    async fn until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !done(self) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: {what}; stored {:?}, {} stream connections",
                self.bubbles(),
                self.fake.heads().len()
            );
            tokio::time::sleep(ms(10)).await;
        }
    }
}

fn last_event_id(head: &str) -> Option<String> {
    head.lines().find_map(|line| {
        line.to_ascii_lowercase()
            .strip_prefix("last-event-id:")
            .map(|v| v.trim().to_owned())
    })
}

// ----- strict mode: no poll traffic -----------------------------------------------------

#[tokio::test]
async fn strict_stream_zero_poll_requests() {
    // A new message, then the three ticks of another, then silence with
    // pings: all of it over the stream.
    let rig = rig(
        LiveTransport::Stream,
        vec![Script::stream(vec![
            write(&quick("live.sse")),
            write(&quick("ticks.sse")),
            Step::Sleep(ms(300)),
            write(": ping\n\n"),
            Step::Hang,
        ])],
    )
    .await;
    rig.until("the message and the last tick", |r| {
        r.bubbles()
            .iter()
            .any(|(id, status)| id == "m_tick" && *status == DeliveryStatus::Read)
    })
    .await;
    assert_eq!(rig.ids(), ["m_live", "m_tick"]);

    // Long enough for a poller on a 20 ms interval to have asked many times.
    tokio::time::sleep(ms(600)).await;
    assert_eq!(polls(&rig.api).await, 0, "no poll request in strict mode");
    assert_eq!(rig.fake.heads().len(), 1, "one stream, held");
    // The engine's own refresh is the only thing that listed accounts.
    assert_eq!(account_listings(&rig.api).await, 1);
    rig.engine.shutdown();
}

#[tokio::test]
async fn the_poll_counter_sees_polling_mode() {
    // The same API, the same engine, polling chosen: the helper that counts
    // zero above counts many here, so the zero is a measurement.
    let rig = rig(LiveTransport::Polling, Vec::new()).await;
    tokio::time::sleep(ms(600)).await;
    assert!(polls(&rig.api).await >= 3, "{}", polls(&rig.api).await);
    assert!(
        rig.fake.heads().is_empty(),
        "polling never asks for the stream"
    );
    rig.engine.shutdown();
}

// ----- drops: resume without a gap ------------------------------------------------------

#[tokio::test]
async fn drop_under_30_min_resumes_once_no_resync() {
    // The first connection brings two messages and breaks; the gateway,
    // asked from the last id, sends on from there (and, as it may, the
    // last one again).
    let rig = rig(
        LiveTransport::Stream,
        vec![
            Script::stream(vec![write(&quick("resume_before.sse")), Step::Close]),
            Script::stream(vec![write(&quick("resume_after.sse")), Step::Hang]),
        ],
    )
    .await;
    rig.until("all four messages", |r| r.ids().len() == 4).await;
    assert_eq!(rig.ids(), ["m_res_1", "m_res_2", "m_res_3", "m_res_4"]);

    let heads = rig.fake.heads();
    assert_eq!(heads.len(), 2, "one reconnect");
    assert_eq!(last_event_id(&heads[0]), None);
    assert_eq!(last_event_id(&heads[1]).as_deref(), Some("c1.mfx2ka002"));

    // The provider's event stream never ended, so the engine did not
    // subscribe again and did not refresh again: one listing in all.
    tokio::time::sleep(ms(500)).await;
    assert_eq!(account_listings(&rig.api).await, 1);
    assert_eq!(rig.fake.heads().len(), 2);
    rig.engine.shutdown();
}

#[tokio::test]
async fn reset_after_long_drop_one_extra_refresh_no_last_event_id() {
    // Past the replay window the gateway answers a resume with `reset`
    // (and closes). The stream ends, the engine starts over: one refresh
    // more, and the new connection carries no `Last-Event-ID`.
    let rig = rig(
        LiveTransport::Stream,
        vec![
            Script::stream(vec![write(&quick("resume_before.sse")), Step::Close]),
            Script::stream(vec![write(&quick("reset.sse")), Step::Hang]),
            Script::stream(vec![write(&quick("live.sse")), Step::Hang]),
        ],
    )
    .await;
    rig.until("the message after the reset", |r| {
        r.ids().contains(&"m_live".to_owned())
    })
    .await;
    assert_eq!(rig.ids(), ["m_live", "m_res_1", "m_res_2"]);

    let heads = rig.fake.heads();
    assert_eq!(heads.len(), 3);
    assert_eq!(last_event_id(&heads[1]).as_deref(), Some("c1.mfx2ka002"));
    assert_eq!(
        last_event_id(&heads[2]),
        None,
        "the cursor went with the reset"
    );

    // The refresh at the start and exactly one after the reset.
    tokio::time::sleep(ms(500)).await;
    assert_eq!(account_listings(&rig.api).await, 2);
    assert_eq!(rig.fake.heads().len(), 3, "and the stream is held");
    rig.engine.shutdown();
}

// ----- a gateway drain ------------------------------------------------------------------

mod drain {
    use super::*;
    use crate::tests::fake_stream::{Attempt, ScriptedTransport};

    async fn run(drain: String) -> (Vec<String>, ScriptedTransport) {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write(&drain), Step::Close]),
            Attempt::Stream(vec![write(&read("resume_after.sse")), Step::Hang]),
        ]);
        let config = WuapiConfig {
            base_url: server.uri(),
            live: LiveTransport::Stream,
            tuning: tuning(),
            ..WuapiConfig::new("test-agent/1.0")
        };
        let provider =
            WuapiProvider::with_stream(config, ApiKey::new(KEY), Arc::new(transport.clone()))
                .unwrap();
        let mut events = provider.subscribe().await.unwrap();
        let mut said = Vec::new();
        while said.len() < 4 {
            said.push(say(&events.next().await.expect("the stream does not end")));
        }
        (said, transport)
    }

    const ALL_FOUR: [&str; 4] = [
        "message m_res_1 delivered",
        "message m_res_2 delivered",
        "message m_res_3 delivered",
        "message m_res_4 delivered",
    ];

    #[tokio::test(start_paused = true)]
    async fn drain_loses_nothing_and_waits_the_retry_it_was_told() {
        // `drain.sse` says `retry: 7000` before it closes.
        let (said, transport) = run(read("drain.sse")).await;
        assert_eq!(said, ALL_FOUR);
        let connects = transport.connects();
        assert_eq!(connects.len(), 2);
        assert_eq!(connects[1].0.as_deref(), Some("c1.mfx2kc001"));
        assert_eq!(connects[1].1 - connects[0].1, Duration::from_secs(7));
    }

    #[tokio::test(start_paused = true)]
    async fn drain_loses_nothing_waits_3s() {
        // A drain that closes under the leading `retry: 3000` and no other.
        let (said, transport) = run(read("drain.sse").replace("retry: 7000\n\n", "")).await;
        assert_eq!(said, ALL_FOUR);
        let connects = transport.connects();
        assert_eq!(connects[1].0.as_deref(), Some("c1.mfx2kc001"));
        assert_eq!(connects[1].1 - connects[0].1, Duration::from_secs(3));
    }
}

// ----- Auto against a backend with no gateway ------------------------------------------

mod without_a_gateway {
    use super::*;
    use crate::tests::envelope::frames;
    use client_provider::EventStream;

    /// The message object of the n-th tick of `ticks.sse`.
    fn tick(n: usize) -> serde_json::Value {
        let data = &frames("ticks.sse")[n].1;
        serde_json::from_str::<serde_json::Value>(data).unwrap()["data"]["object"].clone()
    }

    fn page(items: &[serde_json::Value]) -> String {
        serde_json::json!({"object": "list", "items": items, "nextCursor": null}).to_string()
    }

    /// A mock API with no accounts (so no chat or story reads) and one page
    /// of messages per poll.
    async fn world(pages: &[String]) -> MockServer {
        let server = MockServer::start().await;
        on(
            &server,
            "GET",
            "/v1/accounts",
            (0..pages.len()).map(|_| reply(200, EMPTY)).collect(),
        )
        .await;
        on(
            &server,
            "GET",
            "/v1/messages",
            pages.iter().map(|b| reply(200, b)).collect(),
        )
        .await;
        server
    }

    async fn first_events(
        live: LiveTransport,
        stream: Option<Script>,
    ) -> (Vec<String>, Option<LiveUpdates>) {
        // The first poll primes with nothing, the next two find a tick each.
        let server = world(&[page(&[]), page(&[tick(0)]), page(&[tick(0), tick(1)])]).await;
        let fake = FakeStream::start(stream.into_iter().collect()).await;
        let config = WuapiConfig {
            base_url: server.uri(),
            stream_url: Some(fake.uri()),
            live,
            poll_interval: ms(20),
            tuning: tuning(),
            ..WuapiConfig::new("test-agent/1.0")
        };
        let provider = WuapiProvider::new(config, ApiKey::new(KEY)).unwrap();
        let mut events: EventStream = provider.subscribe().await.unwrap();
        let mut said = Vec::new();
        while said.len() < 2 {
            let next = tokio::time::timeout(Duration::from_secs(5), events.next()).await;
            said.push(say(&next
                .expect("an event in time")
                .expect("the stream goes on")));
        }
        (said, provider.live_updates())
    }

    #[tokio::test]
    async fn auto_on_404_equals_polling_today() {
        let (today, _) = first_events(LiveTransport::Polling, None).await;
        assert_eq!(today, ["message m_tick sent", "message m_tick delivered"]);

        // A host with no gateway: the stream path is a 404 page (or a
        // 200 page that is not a stream). Same events, by polling.
        for answer in [
            Script::answer(404, "text/html", "<html>Not Found</html>"),
            Script::answer(200, "text/html", "<html>Welcome</html>"),
        ] {
            let (said, status) = first_events(LiveTransport::Auto, Some(answer)).await;
            assert_eq!(said, today);
            assert_eq!(
                status,
                Some(LiveUpdates::Polling(PollingReason::Unavailable))
            );
        }
    }
}

// ----- where the key goes ---------------------------------------------------------------

#[tokio::test]
async fn key_origins_only_api_and_stream() {
    let api = api().await;
    let spy = FakeStream::start(vec![Script::stream(vec![Step::Hang])]).await;
    // The stream host sends a client elsewhere: the key must not follow.
    let fake = FakeStream::start(vec![Script::answer(302, "text/html", "")
        .header("Location", &format!("{}/v1/events/stream", spy.uri()))])
    .await;
    let config = WuapiConfig {
        base_url: api.uri(),
        stream_url: Some(fake.uri()),
        live: LiveTransport::Auto,
        poll_interval: ms(20),
        tuning: tuning(),
        ..WuapiConfig::new("test-agent/1.0")
    };
    let provider = WuapiProvider::new(config, ApiKey::new(KEY)).unwrap();
    let mut events = provider.subscribe().await.unwrap();
    // Polling serves meanwhile (priming, then nothing to say): keep it driven.
    let _ = tokio::time::timeout(ms(400), events.next()).await;

    let bearer = format!("authorization: bearer {}", KEY.to_ascii_lowercase());
    let heads = fake.heads();
    assert_eq!(heads.len(), 1, "the stream origin was asked once");
    assert!(
        heads[0].to_ascii_lowercase().contains(&bearer),
        "it gets the key"
    );
    let first_line = heads[0].lines().next().unwrap();
    assert!(!first_line.contains(KEY), "never in the URL: {first_line}");

    let seen = requests(&api).await;
    assert!(!seen.is_empty(), "the API was asked");
    for request in &seen {
        let header = request
            .headers
            .get("authorization")
            .map(|v| v.to_str().unwrap());
        assert_eq!(
            header,
            Some(format!("Bearer {KEY}").as_str()),
            "{request:?}"
        );
        assert!(!request.url.as_str().contains(KEY));
    }

    assert!(spy.heads().is_empty(), "nothing was sent to the other host");
}

// ----- a stream that refuses while the API is healthy -----------------------------------------

/// The default pacing, 20 times faster: the engine backs off from 50 ms to
/// 400 ms (1 s to 8 s there), and the source allows 3 connects in a second
/// (6 in a minute there).
fn refusing_tuning() -> StreamTuning {
    StreamTuning {
        budget: 3,
        budget_window: Duration::from_secs(1),
        ..tuning()
    }
}

async fn refusing(scripts: Vec<Script>) -> Rig {
    rig_paced(
        LiveTransport::Stream,
        scripts,
        refusing_tuning(),
        (ms(50), ms(400)),
    )
    .await
}

fn refusals(status: u16, body: &str) -> Vec<Script> {
    (0..400)
        .map(|_| Script::answer(status, "application/json", body))
        .collect()
}

/// Strict mode, a stream that refuses every connect, a healthy API: the
/// engine opens again and again, and neither the connects nor the full
/// refreshes that go with each open may run away.
async fn refusal_is_paced(rig: Rig) {
    tokio::time::sleep(ms(900)).await;
    let connects = rig.fake.heads().len();
    let listings = account_listings(&rig.api).await;
    assert!(
        (1..=3).contains(&connects),
        "{connects} connects in 900 ms against a budget of 3 a second"
    );
    // One refresh per open: at 0, 50, 150, 350 and 750 ms. Without the
    // engine's memory of the failed subscribes it is one every 50 ms.
    assert!(listings <= 6, "{listings} refreshes in 900 ms");
    // And it goes on asking, once the window lets it.
    rig.until("a connect after the first second", |r| {
        r.fake.heads().len() > connects
    })
    .await;
    rig.engine.shutdown();
}

#[tokio::test]
async fn strict_503_is_not_a_storm_when_the_api_is_healthy() {
    refusal_is_paced(
        refusing(refusals(
            503,
            r#"{"code":"service_unavailable","message":"x"}"#,
        ))
        .await,
    )
    .await;
}

#[tokio::test]
async fn strict_404_is_not_a_storm_when_the_api_is_healthy() {
    refusal_is_paced(refusing(refusals(404, r#"{"code":"not_found","message":"x"}"#)).await).await;
}

#[tokio::test]
async fn strict_unreachable_is_not_a_storm_when_the_api_is_healthy() {
    // The server accepts and closes without an answer.
    refusal_is_paced(refusing(Vec::new()).await).await;
}

#[tokio::test]
async fn strict_429_waits_the_retry_after_when_the_api_is_healthy() {
    let scripts = (0..400)
        .map(|_| {
            Script::answer(
                429,
                "application/json",
                r#"{"code":"rate_limited","message":"x"}"#,
            )
            .header("Retry-After", "1")
        })
        .collect();
    let rig = refusing(scripts).await;
    tokio::time::sleep(ms(800)).await;
    assert_eq!(rig.fake.heads().len(), 1, "asked to wait a second");
    assert!(account_listings(&rig.api).await <= 2);
    rig.until("the second connect", |r| r.fake.heads().len() == 2)
        .await;
    rig.engine.shutdown();
}
