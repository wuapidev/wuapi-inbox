//! The driver that chooses between the stream and polling, and what the
//! poller shares with the stream.

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

use crate::client::WuapiClient;
use crate::config::ApiKey;
use crate::envelope::Mapper;
use crate::events::{PollState, PollingEventSource};
use crate::follow::Now;
use crate::tests::envelope::{frames, say};
use crate::tests::{config, on, reply, KEY};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wiremock::MockServer;

/// The message object of the n-th frame of `ticks.sse`: `sent`, then
/// `delivered`, then `read`.
fn tick(n: usize) -> serde_json::Value {
    let data = &frames("ticks.sse")[n].1;
    serde_json::from_str::<serde_json::Value>(data).unwrap()["data"]["object"].clone()
}

fn page(items: &[serde_json::Value]) -> String {
    serde_json::json!({"object": "list", "items": items, "nextCursor": null}).to_string()
}

/// A mock API with no accounts (so no chat or story reads) and these pages
/// of messages, one per poll.
async fn api(pages: &[String]) -> MockServer {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/v1/accounts",
        (0..pages.len())
            .map(|_| reply(200, r#"{"object":"list","items":[],"nextCursor":null}"#))
            .collect(),
    )
    .await;
    on(
        &server,
        "GET",
        "/v1/messages",
        pages.iter().map(|body| reply(200, body)).collect(),
    )
    .await;
    server
}

fn source(server: &MockServer) -> PollingEventSource {
    let config = config(server);
    let client = Arc::new(WuapiClient::new(&config, ApiKey::new(KEY)).unwrap());
    PollingEventSource::new(client, Duration::from_millis(20))
}

#[tokio::test]
async fn poller_shares_state_with_mapper() {
    let server = api(&[page(&[tick(0)]), page(&[tick(1)])]).await;
    let source = source(&server);
    let state = Arc::new(Mutex::new(PollState::default()));
    let mut poller = source.poller_with(state.clone());
    poller.poll(Now::real()).await.unwrap();

    // The stream says `delivered` before the poll finds it.
    let mut mapper = Mapper::new(state, Arc::default(), 16);
    let pushed = mapper.map(&frames("ticks.sse")[1].1, Now::real());
    assert_eq!(
        pushed.events.iter().map(say).collect::<Vec<_>>(),
        ["message m_tick delivered"]
    );

    // The poll reads that same version: it is not said a second time.
    let polled = poller.poll(Now::real()).await.unwrap();
    assert!(
        polled.iter().map(say).collect::<Vec<_>>().is_empty(),
        "{polled:?}"
    );
}

#[tokio::test]
async fn poller_with_state_unchanged_when_polling() {
    let server = api(&[page(&[tick(0)]), page(&[tick(1)])]).await;
    let source = source(&server);
    // Nothing shared: the poller keeps its own table, as it always did.
    let mut poller = source.poller();
    poller.poll(Now::real()).await.unwrap();
    let polled = poller.poll(Now::real()).await.unwrap();
    assert_eq!(
        polled.iter().map(say).collect::<Vec<_>>(),
        ["message m_tick delivered"]
    );
}

#[test]
fn a_message_recorded_silently_is_said_once_when_pushed() {
    use wuapi::types as api;
    let live = &frames("live.sse")[0].1;
    let wire: api::Message = serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(live).unwrap()["data"]["object"].clone(),
    )
    .unwrap();
    let state = Arc::new(Mutex::new(PollState::default()));
    {
        let mut state = state.lock().unwrap();
        // A priming poll listed it and dropped it as history.
        let (listed, _) = state.observe_messages(std::slice::from_ref(&wire));
        assert_eq!(listed.len(), 1);
        state.recorded_silently([wire.id.clone()]);
    }
    let mut mapper = Mapper::new(state, Arc::default(), 16);
    let first = mapper.map(live, Now::real());
    assert_eq!(
        first.events.iter().map(say).collect::<Vec<_>>(),
        ["message m_live delivered"]
    );
    // The same version again, under another event id: said already.
    let again = mapper.map(&live.replace("evt_live1", "evt_live2"), Now::real());
    assert!(again.events.is_empty(), "{:?}", again.events);
}

#[test]
fn a_poll_that_says_a_silently_recorded_message_ends_its_silence() {
    use wuapi::types as api;
    let live = &frames("live.sse")[0].1;
    let wire: api::Message = serde_json::from_value(
        serde_json::from_str::<serde_json::Value>(live).unwrap()["data"]["object"].clone(),
    )
    .unwrap();
    let mut newer = wire.clone();
    newer.updated_at = "2026-09-24T08:11:00.000Z".into();
    let state = Arc::new(Mutex::new(PollState::default()));
    {
        let mut state = state.lock().unwrap();
        state.observe_messages(std::slice::from_ref(&wire));
        state.recorded_silently([wire.id.clone()]);
        // A later poll finds a newer version and says it.
        let (said, _) = state.observe_messages(std::slice::from_ref(&newer));
        assert_eq!(said.len(), 1);
        // The stream then pushes that same version: said already.
        assert!(state.observe_pushed(&newer).is_none());
    }
}

// ----- Auto: what each answer to the first connect leads to --------------------------------------

mod auto {
    use super::{page, tick};
    use crate::config::{ApiKey, LiveTransport, StreamTuning, WuapiConfig};
    use crate::live::{LiveEventSource, Status, Why};
    use crate::provider::WuapiProvider;
    use crate::tests::envelope::say;
    use crate::tests::fake_stream::{refused, Attempt, ScriptedTransport, Step};
    use crate::tests::{config, on, reply, KEY};
    use client_provider::{EventStream, Provider};
    use futures::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;
    use wiremock::MockServer;

    pub(super) fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn quiet() -> f64 {
        0.0
    }

    /// The design's tuning, with every wait short enough to sit through on
    /// the real clock. The mock API is a real socket: a paused clock would
    /// jump ahead while it answers and fire timeouts at random.
    pub(super) fn fast() -> StreamTuning {
        StreamTuning {
            jitter: quiet,
            idle: ms(400),
            open: ms(2000),
            backoff_base: ms(5),
            backoff_cap: ms(20),
            min_gap: ms(5),
            budget: 1000,
            stable_after: ms(100),
            fallback_after: ms(300),
            safety_poll: ms(150),
            reprobe: ms(300),
            ..StreamTuning::default()
        }
    }

    pub(super) fn api_error(code: &str) -> String {
        format!(r#"{{"code":"{code}","message":"words"}}"#)
    }

    /// A mock API with one connected account, and these pages of messages
    /// for the first polls (the first one primes).
    pub(super) async fn world(pages: &[String]) -> MockServer {
        let server = MockServer::start().await;
        let account = format!(
            r#"{{"object":"list","items":[{}],"nextCursor":null}}"#,
            fixture!("account")
        );
        on(
            &server,
            "GET",
            "/v1/accounts",
            (0..400).map(|_| reply(200, &account)).collect(),
        )
        .await;
        on(
            &server,
            "GET",
            "/v1/messages",
            pages.iter().map(|body| reply(200, body)).collect(),
        )
        .await;
        server
    }

    pub(super) struct Rig {
        pub server: MockServer,
        pub transport: ScriptedTransport,
        pub provider: WuapiProvider,
        pub live: Arc<LiveEventSource>,
    }

    pub(super) async fn rig_with(
        server: MockServer,
        attempts: Vec<Attempt>,
        tuning: StreamTuning,
    ) -> Rig {
        rig_polling_every(server, attempts, tuning, ms(20)).await
    }

    /// The same, with the poller's own interval (what the full cadence of
    /// a fallback waits between polls) given.
    pub(super) async fn rig_polling_every(
        server: MockServer,
        attempts: Vec<Attempt>,
        tuning: StreamTuning,
        poll_interval: Duration,
    ) -> Rig {
        let transport = ScriptedTransport::new(attempts);
        let config = WuapiConfig {
            live: LiveTransport::Auto,
            tuning,
            poll_interval,
            ..config(&server)
        };
        let (provider, live) =
            WuapiProvider::with_live(config, ApiKey::new(KEY), Arc::new(transport.clone()))
                .unwrap();
        Rig {
            server,
            transport,
            provider,
            live,
        }
    }

    pub(super) async fn rig(attempts: Vec<Attempt>, tuning: StreamTuning) -> Rig {
        // The first poll primes with nothing; the second finds a tick.
        let server = world(&[page(&[]), page(&[tick(0)])]).await;
        rig_with(server, attempts, tuning).await
    }

    pub(super) async fn next(events: &mut EventStream) -> client_provider::ProviderEvent {
        tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("no event within 5 s")
            .expect("the stream ended")
    }

    /// Keeps the stream polled (the driver lives in it) until `done` holds.
    pub(super) async fn until(
        events: &mut EventStream,
        what: &str,
        done: impl Fn() -> bool,
    ) -> Vec<String> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut said = Vec::new();
        while !done() {
            assert!(tokio::time::Instant::now() < deadline, "timed out: {what}");
            if let Ok(Some(event)) = tokio::time::timeout(ms(20), events.next()).await {
                said.push(say(&event));
            }
        }
        said
    }

    // One answer to the probe, then polling serves, and the stream is asked
    // for again after the re-probe time.
    async fn polls_then_reprobes(answer: Attempt, why: Why) {
        let r = rig(vec![answer, Attempt::Stream(vec![Step::Hang])], fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        assert_eq!(r.live.status(), Status::Polling(why));
        // Polling serves: the tick comes from the second poll.
        assert_eq!(say(&next(&mut events).await), "message m_tick sent");
        let transport = r.transport.clone();
        until(&mut events, "the re-probe", || {
            transport.connects().len() >= 2
        })
        .await;
        let connects = r.transport.connects();
        assert_eq!(connects[1].0, None, "a re-probe starts without a cursor");
        assert!(
            connects[1].1 - connects[0].1 >= ms(300),
            "{:?}",
            connects[1].1 - connects[0].1
        );
    }

    #[tokio::test]
    async fn auto_403_polls_reprobe_10min() {
        polls_then_reprobes(
            refused(403, &api_error("organization_suspended"), None),
            Why::Refused,
        )
        .await;
    }

    #[tokio::test]
    async fn auto_400_polls_reprobe_10min() {
        polls_then_reprobes(
            refused(400, &api_error("invalid_request"), None),
            Why::Unavailable,
        )
        .await;
    }

    #[tokio::test]
    async fn auto_200_not_stream_polls_reprobe_10min() {
        polls_then_reprobes(refused(200, "<html></html>", None), Why::Unavailable).await;
    }

    #[tokio::test]
    async fn auto_non_api_401_polls_reprobe_10min() {
        polls_then_reprobes(
            refused(401, "<html>Unauthorized</html>", None),
            Why::Unavailable,
        )
        .await;
    }

    async fn limited(code: &str, why: Why) {
        let r = rig(
            vec![
                refused(429, &api_error(code), Some(1)),
                Attempt::Stream(vec![Step::Hang]),
            ],
            fast(),
        )
        .await;
        let mut events = r.provider.subscribe().await.unwrap();
        assert_eq!(r.live.status(), Status::Polling(why));
        assert_eq!(say(&next(&mut events).await), "message m_tick sent");
        let transport = r.transport.clone();
        until(&mut events, "the retry", || transport.connects().len() >= 2).await;
        let connects = r.transport.connects();
        let waited = connects[1].1 - connects[0].1;
        assert!(waited >= Duration::from_secs(1), "{waited:?}");
        assert!(waited < Duration::from_secs(4), "{waited:?}");
    }

    #[tokio::test]
    async fn auto_429_limit_reprobes_at_retry_after_default_30s() {
        // The default of 30 s (60 s for any other 429) is checked on the
        // paused clock, in the link's tests; here `Retry-After: 1` is
        // honoured on the real one.
        limited("stream_connection_limit", Why::ConnectionLimit).await;
    }

    #[tokio::test]
    async fn auto_429_other_default_60s() {
        limited("rate_limited", Why::Failing).await;
    }

    #[tokio::test]
    async fn auto_503_backs_off_polls() {
        let r = rig(
            vec![
                refused(503, &api_error("service_unavailable"), None),
                refused(503, &api_error("service_unavailable"), None),
                Attempt::Stream(vec![Step::Hang]),
            ],
            fast(),
        )
        .await;
        let mut events = r.provider.subscribe().await.unwrap();
        assert_eq!(r.live.status(), Status::Polling(Why::Failing));
        assert_eq!(say(&next(&mut events).await), "message m_tick sent");
        let transport = r.transport.clone();
        until(&mut events, "the backoff", || {
            transport.connects().len() >= 3
        })
        .await;
        let connects = r.transport.connects();
        // Backing off from 5 ms to the 20 ms cap: far sooner than a re-probe.
        assert!(connects[2].1 - connects[0].1 < ms(300));
    }

    #[tokio::test]
    async fn auto_probes_at_most_the_budget_across_opens() {
        // The engine opens again after each end of the stream; the refused
        // connects of all those opens count against one budget.
        let pages: Vec<String> = (0..20).map(|_| page(&[])).collect();
        let server = world(&pages).await;
        let attempts = (0..20)
            .map(|_| refused(503, &api_error("service_unavailable"), None))
            .collect();
        let tuning = StreamTuning {
            budget: 3,
            budget_window: Duration::from_secs(30),
            ..fast()
        };
        let r = rig_with(server, attempts, tuning).await;
        for _ in 0..8 {
            let _events = r.provider.subscribe().await.expect("polling serves");
            assert_eq!(r.live.status(), Status::Polling(Why::Failing));
        }
        assert_eq!(r.transport.connects().len(), 3);
    }

    #[tokio::test]
    async fn refusal_remembered_next_open_skips_probe() {
        // Each open primes with a page of its own; the tick comes after.
        let server = super::auto::world(&[page(&[]), page(&[]), page(&[tick(0)])]).await;
        let r = rig_with(
            server,
            vec![refused(404, &api_error("not_found"), None)],
            StreamTuning {
                reprobe: Duration::from_secs(600),
                ..fast()
            },
        )
        .await;
        let _first = r.provider.subscribe().await.unwrap();
        assert_eq!(r.transport.connects().len(), 1, "one probe");
        let mut second = r.provider.subscribe().await.unwrap();
        assert_eq!(r.transport.connects().len(), 1, "the refusal is remembered");
        assert_eq!(r.live.status(), Status::Polling(Why::Unavailable));
        // And that open polled: it was primed, and serves.
        assert_eq!(say(&next(&mut second).await), "message m_tick sent");
    }

    #[tokio::test]
    async fn auto_404_matches_polling_summary() {
        let server = MockServer::start().await;
        let accounts = format!(
            r#"{{"object":"list","items":[{}],"nextCursor":null}}"#,
            fixture!("account")
        );
        let primed = format!(
            r#"{{"object":"list","items":[{},{}],"nextCursor":"older"}}"#,
            fixture!("message"),
            fixture!("message_inbound")
        );
        on(
            &server,
            "GET",
            "/v1/accounts",
            vec![
                reply(200, &accounts),
                reply(
                    503,
                    r#"{"code":"engine_unavailable","message":"Retry shortly."}"#,
                ),
                reply(200, &accounts),
                reply(401, fixture!("error_unauthorized")),
            ],
        )
        .await;
        on(
            &server,
            "GET",
            "/v1/messages",
            vec![reply(200, &primed), reply(200, fixture!("message_list"))],
        )
        .await;
        let r = rig_with(
            server,
            vec![refused(404, &api_error("not_found"), None)],
            StreamTuning {
                reprobe: Duration::from_secs(600),
                ..fast()
            },
        )
        .await;
        let mut events = r.provider.subscribe().await.unwrap();
        let mut seen = Vec::new();
        while let Some(event) = events.next().await {
            seen.push(event);
        }
        let summary: Vec<String> = seen
            .iter()
            .map(|e| match e {
                client_provider::ProviderEvent::ChatUpdated(c) => format!("chat {}", c.title),
                client_provider::ProviderEvent::MessageUpserted(m) => format!("message {}", m.id),
                other => format!("{other:?}"),
            })
            .collect();
        // What `the_event_stream_primes_then_emits_and_survives_bad_ticks`
        // asserts for `Polling`, unchanged.
        assert_eq!(
            summary,
            [
                "message m_sent_by_client",
                "message m_reaction",
                "message m_group_image",
            ]
        );
        let targets: Vec<_> = crate::tests::requests(&r.server)
            .await
            .iter()
            .map(crate::tests::target)
            .collect();
        assert_eq!(targets.len(), 9, "{targets:?}");
        assert_eq!(r.transport.connects().len(), 1, "one probe, no more");
    }
}

// ----- Auto: the 401 guard, falling back and coming back ----------------------------------------------

mod switching {
    use super::auto::{api_error, fast, ms, next, rig, rig_polling_every, rig_with, until, world};
    use super::{page, tick};
    use crate::config::StreamTuning;
    use crate::live::{Status, Why};
    use crate::tests::envelope::say;
    use crate::tests::fake_stream::{refused, write, Attempt, Step};
    use crate::tests::{on, reply, target};
    use client_provider::{Provider, ProviderError};
    use std::time::Duration;

    #[tokio::test]
    async fn auto_401_confirmed_by_me_unauthorized() {
        let r = rig(vec![refused(401, &api_error("unauthorized"), None)], fast()).await;
        on(
            &r.server,
            "GET",
            "/v1/me",
            vec![reply(401, fixture!("error_unauthorized"))],
        )
        .await;
        let error = r.provider.subscribe().await.err().expect("open failed");
        assert!(matches!(error, ProviderError::Unauthorized(_)), "{error:?}");
        // The API was asked, and the poller never started.
        let asked: Vec<String> = crate::tests::requests(&r.server)
            .await
            .iter()
            .map(target)
            .collect();
        assert_eq!(asked, ["/v1/me"]);
    }

    #[tokio::test]
    async fn auto_401_not_confirmed_polls() {
        let r = rig(
            vec![
                refused(401, &api_error("unauthorized"), None),
                Attempt::Stream(vec![Step::Hang]),
            ],
            fast(),
        )
        .await;
        on(
            &r.server,
            "GET",
            "/v1/me",
            vec![reply(200, fixture!("auth_context"))],
        )
        .await;
        let mut events = r.provider.subscribe().await.unwrap();
        // Nobody is signed out for it: polling serves, and says so.
        assert_eq!(r.live.status(), Status::Polling(Why::Unavailable));
        assert_eq!(say(&next(&mut events).await), "message m_tick sent");
    }

    /// A stream that gives one event and drops, then fails to reconnect
    /// `failures` times, then stays up.
    fn drops_then_fails(failures: usize) -> Vec<Attempt> {
        let mut attempts = vec![Attempt::Stream(vec![
            write("id: C1\ndata: {}\n\n"),
            Step::Close,
        ])];
        attempts.extend((0..failures).map(|_| Attempt::Fail("could not connect".into())));
        attempts.push(Attempt::Stream(vec![Step::Hang]));
        attempts
    }

    #[tokio::test]
    async fn auto_fallback_after_3_failures_or_20s() {
        // Three failures in a row (the dropped connection is the first).
        let r = rig(drops_then_fails(2), fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        assert_eq!(r.live.status(), Status::Stream);
        let live = r.live.clone();
        let mut said = until(&mut events, "polling to take over", || {
            live.status() == Status::Polling(Why::Failing)
        })
        .await;
        // The full poll that came with it found the tick.
        if said.is_empty() {
            said.push(say(&next(&mut events).await));
        }
        assert_eq!(said, ["message m_tick sent"]);

        // Or the time: few failures, but too long down. The wait between
        // connects (200 ms) is long against the 300 ms limit.
        let slow = crate::config::StreamTuning {
            backoff_base: ms(200),
            backoff_cap: ms(200),
            min_gap: ms(200),
            fallback_failures: 100,
            ..fast()
        };
        let r = rig(drops_then_fails(10), slow).await;
        let mut events = r.provider.subscribe().await.unwrap();
        assert_eq!(r.live.status(), Status::Stream);
        let live = r.live.clone();
        let started = tokio::time::Instant::now();
        until(&mut events, "polling to take over", || {
            live.status() == Status::Polling(Why::Failing)
        })
        .await;
        let waited = started.elapsed();
        assert!(waited >= ms(250) && waited < ms(2000), "{waited:?}");
        assert!(r.transport.connects().len() < 5, "not by counting failures");
    }

    #[tokio::test]
    async fn fallback_clears_cursor_full_poll() {
        let r = rig(drops_then_fails(3), fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let live = r.live.clone();
        let mut said = until(&mut events, "polling to take over", || {
            live.status() == Status::Polling(Why::Failing)
        })
        .await;
        let transport = r.transport.clone();
        said.extend(
            until(&mut events, "a re-probe", || {
                transport.connects().len() >= 5
            })
            .await,
        );
        // The full poll ran at once: it is the second one the API saw, and
        // it found the tick.
        assert_eq!(said, ["message m_tick sent"]);
        let ids: Vec<Option<String>> = r
            .transport
            .connects()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(ids[0], None);
        assert_eq!(ids[1].as_deref(), Some("C1"), "a short drop resumes");
        // Once polling took over, nothing resumes: the poller covers the gap.
        assert_eq!(ids.last().unwrap(), &None, "{ids:?}");
    }

    fn quick(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/stream")
                .join(name),
        )
        .unwrap()
        .replace("retry: 3000", "retry: 20")
    }

    #[tokio::test]
    async fn auto_reset_on_a_resume_ends_the_stream_and_the_next_open_has_no_cursor() {
        // A reset is the gateway saying the cursor is gone: whatever the
        // reason, the stream ends (the engine refreshes), and nothing
        // resumes from a cursor that is not valid.
        for reset in ["reset.sse", "reset_unknown.sse", "reset_not_logged.sse"] {
            let pages: Vec<String> = (0..60).map(|_| page(&[])).collect();
            let server = world(&pages).await;
            let r = rig_with(
                server,
                vec![
                    Attempt::Stream(vec![write(&quick("resume_before.sse")), Step::Close]),
                    Attempt::Stream(vec![write(&quick(reset)), Step::Hang]),
                    Attempt::Stream(vec![write(&quick("live.sse")), Step::Hang]),
                ],
                fast(),
            )
            .await;
            let mut events = r.provider.subscribe().await.unwrap();
            let mut said = Vec::new();
            loop {
                let next = tokio::time::timeout(
                    Duration::from_secs(3),
                    futures::StreamExt::next(&mut events),
                )
                .await
                .unwrap_or_else(|_| {
                    panic!("{reset}: the stream did not end on the reset: {said:?}")
                });
                match next {
                    Some(event) => said.push(say(&event)),
                    None => break,
                }
            }
            // What came before the reset arrived, and nothing after it.
            said.sort();
            assert_eq!(
                said,
                ["message m_res_1 delivered", "message m_res_2 delivered"],
                "{reset}"
            );
            let connects = r.transport.connects();
            assert_eq!(connects.len(), 2, "{reset}: nothing reconnects by itself");
            assert_eq!(connects[0].0, None);
            assert_eq!(connects[1].0.as_deref(), Some("c1.mfx2ka002"), "{reset}");

            // The engine opens again: no cursor.
            let _next = r.provider.subscribe().await.unwrap();
            let connects = r.transport.connects();
            assert_eq!(connects.len(), 3, "{reset}");
            assert_eq!(connects[2].0, None, "{reset}: a new open never resumes");
        }
    }

    /// The safety poll and the poller's own interval are minutes away: only
    /// the polls the driver asks for by itself can find anything.
    fn far_polls() -> StreamTuning {
        StreamTuning {
            safety_poll: Duration::from_secs(10),
            ..fast()
        }
    }

    #[tokio::test]
    async fn the_seam_poll_after_the_stream_returns_finds_what_only_the_api_has() {
        // The first safety poll primes (empty), the fallback's full poll
        // finds nothing, and the stream that comes back never says the
        // tick: it exists only in the API, and the poll that closes the
        // seam is the only way to it.
        let server = world(&[page(&[]), page(&[]), page(&[tick(0)])]).await;
        let mut attempts = vec![Attempt::Stream(vec![
            write("id: C1\ndata: {}\n\n"),
            Step::Close,
        ])];
        attempts.extend((0..2).map(|_| Attempt::Fail("could not connect".into())));
        attempts.push(Attempt::Stream(vec![Step::Hang]));
        let r = rig_polling_every(server, attempts, far_polls(), Duration::from_secs(10)).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let heard = tokio::time::timeout(Duration::from_secs(2), async {
            // Keeps the stream polled: the driver lives in it.
            futures::StreamExt::next(&mut events).await
        })
        .await
        .expect("the seam poll did not find the tick within 2 s");
        assert_eq!(say(&heard.unwrap()), "message m_tick sent");
        assert_eq!(r.live.status(), Status::Stream);
        assert_eq!(r.transport.connects().len(), 4);
    }

    #[tokio::test]
    async fn falling_back_polls_at_once_whatever_the_poll_interval() {
        // Not live at open (a 503): the poll that primes settles the
        // poller 10 s ahead. The stream then comes up for a moment, drops
        // and fails for good: polling takes over, and its first poll is
        // not 10 s away.
        let server = world(&[page(&[]), page(&[]), page(&[tick(0)])]).await;
        let mut attempts = vec![
            refused(503, &api_error("service_unavailable"), None),
            Attempt::Stream(vec![Step::Sleep(ms(200)), Step::Close]),
        ];
        // The stream never comes back (the script ends): only the poll that
        // polling takes over with can find the tick.
        attempts.extend((0..3).map(|_| Attempt::Fail("could not connect".into())));
        let r = rig_polling_every(server, attempts, far_polls(), Duration::from_secs(10)).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let live = r.live.clone();
        let mut said = until(&mut events, "polling to take over", || {
            live.status() == Status::Polling(Why::Failing)
        })
        .await;
        if said.is_empty() {
            said.push(say(&tokio::time::timeout(
                Duration::from_secs(2),
                futures::StreamExt::next(&mut events),
            )
            .await
            .expect("the fallback did not poll within 2 s")
            .unwrap()));
        }
        assert_eq!(said, ["message m_tick sent"]);
    }

    #[tokio::test]
    async fn recovery_returns_to_stream_one_seam_poll_each_event_once() {
        let ticks_text = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/stream/ticks.sse"),
        )
        .unwrap();
        // The first safety poll primes (empty); the full poll of the fallback
        // finds `sent`; the seam poll finds `delivered`.
        let server = world(&[page(&[]), page(&[tick(0)]), page(&[tick(1)])]).await;
        let mut attempts = vec![Attempt::Stream(vec![
            write("id: C1\ndata: {}\n\n"),
            Step::Close,
        ])];
        attempts.extend((0..2).map(|_| Attempt::Fail("could not connect".into())));
        // The stream that comes back says all three ticks again.
        // (After the seam poll: a poll page fetched before a push may say an
        // older version once, which the design accepts.)
        attempts.push(Attempt::Stream(vec![
            Step::Sleep(ms(150)),
            write(&ticks_text),
            Step::Hang,
        ]));
        let r = rig_with(server, attempts, fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let mut said = Vec::new();
        while said.len() < 3 {
            said.push(say(&next(&mut events).await));
        }
        // Each version once, whichever way it came.
        said.sort();
        assert_eq!(
            said,
            [
                "message m_tick delivered",
                "message m_tick read",
                "message m_tick sent"
            ]
        );
        assert_eq!(r.live.status(), Status::Stream);
        let more = tokio::time::timeout(
            Duration::from_millis(400),
            futures::StreamExt::next(&mut events),
        )
        .await;
        assert!(
            more.is_err(),
            "nothing is said twice: {:?}",
            more.map(|event| event.map(|event| say(&event)))
        );
    }
}

// ----- Auto while the stream is up: the safety poll --------------------------------------------------

mod safety {
    use super::auto::{fast, ms, next, rig_with, world};
    use super::{page, tick};
    use crate::events::EventSource;
    use crate::live::Status;
    use crate::tests::envelope::say;
    use crate::tests::fake_stream::{write, Attempt, Step};
    use crate::tests::{reply, requests, target};
    use client_provider::Provider;
    use std::time::Duration;
    use wiremock::MockServer;

    /// A stream that sends only pings, every 100 ms, for `n` of them.
    fn pings(n: usize) -> Vec<Step> {
        (0..n)
            .flat_map(|_| [Step::Sleep(ms(100)), write(": ping\n\n")])
            .chain([Step::Hang])
            .collect()
    }

    /// `n` pages, one per poll, all of them without news.
    fn quiet_pages(n: usize) -> Vec<String> {
        (0..n).map(|_| page(&[])).collect()
    }

    async fn targets(server: &MockServer) -> Vec<String> {
        requests(server).await.iter().map(target).collect()
    }

    /// The general polls seen so far: the requests for the newest messages.
    fn polls(seen: &[String]) -> usize {
        seen.iter()
            .filter(|t| t.starts_with("/v1/messages?"))
            .count()
    }

    #[tokio::test]
    async fn safety_poll_every_75s_sweep_5min_stories_10min() {
        let server = world(&quiet_pages(40)).await;
        let r = rig_with(server, vec![Attempt::Stream(pings(60))], fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let server = &r.server;
        // Ten polls, a safety interval apart.
        let mut seen = Vec::new();
        for _ in 0..200 {
            let _ = tokio::time::timeout(ms(20), futures::StreamExt::next(&mut events)).await;
            seen = targets(server).await;
            if polls(&seen) >= 10 {
                break;
            }
        }
        assert!(polls(&seen) >= 10, "{seen:?}");
        // Which polls (counting from 1) also read chats, and stories.
        let mut poll = 0;
        let (mut chats, mut stories) = (Vec::new(), Vec::new());
        for request in &seen {
            if request.starts_with("/v1/messages?") {
                poll += 1;
            } else if request.contains("/chats") {
                chats.push(poll);
            } else if request.contains("/stories") {
                stories.push(poll);
            }
        }
        // The first poll reads everything once; then chats every fourth poll
        // (75 s x 4 = 5 minutes) and stories every eighth (10 minutes).
        assert_eq!(chats, [1, 5, 9], "{seen:?}");
        assert_eq!(stories, [1, 9], "{seen:?}");
    }

    #[tokio::test]
    async fn no_follow_ups_while_live() {
        let server = world(&quiet_pages(40)).await;
        let r = rig_with(server, vec![Attempt::Stream(pings(60))], fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let sent: wuapi::types::Message = serde_json::from_str(fixture!("message")).unwrap();
        r.live.accepted(&sent);
        // A follow-up would be due after 0.5 s: the stream tells instead.
        let _ = tokio::time::timeout(ms(1200), futures::StreamExt::next(&mut events)).await;
        let seen = targets(&r.server).await;
        assert!(polls(&seen) >= 2, "it was polling: {seen:?}");
        assert!(
            !seen
                .iter()
                .any(|t| t.starts_with(&format!("/v1/messages/{}", sent.id))),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn follow_ups_run_in_fallback() {
        // The same send, with polling serving: the follow-up comes.
        let server = world(&quiet_pages(40)).await;
        let r = rig_with(
            server,
            vec![crate::tests::fake_stream::refused(404, "{}", None)],
            fast(),
        )
        .await;
        let mut events = r.provider.subscribe().await.unwrap();
        let sent: wuapi::types::Message = serde_json::from_str(fixture!("message")).unwrap();
        r.live.accepted(&sent);
        let _ = tokio::time::timeout(ms(1200), futures::StreamExt::next(&mut events)).await;
        let seen = targets(&r.server).await;
        assert!(
            seen.iter()
                .any(|t| t.starts_with(&format!("/v1/messages/{}", sent.id))),
            "{seen:?}"
        );
    }

    #[tokio::test]
    async fn first_safety_poll_not_awaited() {
        let server = MockServer::start().await;
        // Every answer takes a second and a half.
        let slow = |body: String| reply(200, &body).set_delay(Duration::from_millis(1500));
        crate::tests::on(
            &server,
            "GET",
            "/v1/accounts",
            vec![slow(
                r#"{"object":"list","items":[],"nextCursor":null}"#.into(),
            )],
        )
        .await;
        crate::tests::on(&server, "GET", "/v1/messages", vec![slow(page(&[]))]).await;
        let r = rig_with(server, vec![Attempt::Stream(pings(60))], fast()).await;
        let started = tokio::time::Instant::now();
        let mut events = r.provider.subscribe().await.unwrap();
        // `open` came back with the stream, not with the poll.
        assert!(started.elapsed() < ms(600), "{:?}", started.elapsed());
        assert_eq!(r.live.status(), Status::Stream);
        // And the poll does run, meanwhile.
        let server = &r.server;
        for _ in 0..100 {
            let _ = tokio::time::timeout(ms(20), futures::StreamExt::next(&mut events)).await;
            if !targets(server).await.is_empty() {
                break;
            }
        }
        assert!(targets(server)
            .await
            .iter()
            .any(|t| t.starts_with("/v1/accounts")));
    }

    /// Keeps the stream polled until `millis` after `started`, and counts
    /// the polls made by then.
    async fn count_at(
        events: &mut client_provider::EventStream,
        server: &MockServer,
        started: tokio::time::Instant,
        millis: u64,
    ) -> usize {
        while started.elapsed() < ms(millis) {
            let _ = tokio::time::timeout(ms(10), futures::StreamExt::next(events)).await;
        }
        polls(&targets(server).await)
    }

    /// `n` versions of one message, newer each time: a backend that goes
    /// on changing while the stream says nothing.
    fn versions(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| {
                let mut message = tick(0);
                message["updatedAt"] = format!("2026-09-24T08:{:02}:00.000Z", 20 + i).into();
                page(&[message])
            })
            .collect()
    }

    /// The text of `live.sse`: one inbound message, `m_live`.
    fn live_text() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stream/live.sse"),
        )
        .unwrap()
    }

    /// `m_live` as the API lists it.
    fn inbound() -> serde_json::Value {
        let data = &crate::tests::envelope::frames("live.sse")[0].1;
        serde_json::from_str::<serde_json::Value>(data).unwrap()["data"]["object"].clone()
    }

    /// What the engine hears for 1.5 s when the first safety poll (the
    /// priming one, run after `open` returned) does or does not already
    /// list the message the stream pushes 500 ms in.
    async fn hear(first_poll_lists_it: bool) -> Vec<String> {
        let mut pages = vec![if first_poll_lists_it {
            page(&[inbound()])
        } else {
            page(&[])
        }];
        // Every later poll lists it too, unchanged.
        pages.extend((0..60).map(|_| page(&[inbound()])));
        let server = world(&pages).await;
        let mut steps = vec![Step::Sleep(ms(250)), write(": ping\n\n")];
        steps.extend([Step::Sleep(ms(250)), write(&live_text())]);
        steps.extend((0..4).flat_map(|_| [Step::Sleep(ms(250)), write(": ping\n\n")]));
        steps.push(Step::Hang);
        let r = rig_with(server, vec![Attempt::Stream(steps)], fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let end = tokio::time::Instant::now() + ms(1500);
        let mut said = Vec::new();
        while tokio::time::Instant::now() < end {
            if let Ok(Some(event)) =
                tokio::time::timeout(ms(20), futures::StreamExt::next(&mut events)).await
            {
                said.push(say(&event));
            }
        }
        said
    }

    #[tokio::test]
    async fn a_push_for_what_the_priming_poll_listed_is_still_said() {
        // The poll that runs right after `open` lists a message that did
        // not exist when the engine refreshed; it is dropped as history, and
        // the stream pushes it later. Priming has recorded it silently, not
        // said it: the push is news.
        let said = hear(true).await;
        assert_eq!(said, ["message m_live delivered"], "{said:?}");
    }

    #[tokio::test]
    async fn a_push_for_a_message_no_poll_listed_is_said_once() {
        // Triangulation: the same push when the first poll lists nothing,
        // and every later poll lists the message unchanged.
        let said = hear(false).await;
        assert_eq!(said, ["message m_live delivered"], "{said:?}");
    }

    #[tokio::test]
    async fn silent_kill_switch_change_reaches_ui() {
        // Pings only, and the backend has news.
        let mut pages = vec![page(&[])];
        pages.extend(versions(3));
        let server = world(&pages).await;
        let r = rig_with(server, vec![Attempt::Stream(pings(60))], fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let said = say(&next(&mut events).await);
        assert_eq!(said, "message m_tick sent");
        // The connection is kept: this is not a fallback.
        assert_eq!(r.live.status(), Status::Stream);
    }

    #[tokio::test]
    async fn silent_stream_two_strikes_full_cadence() {
        let mut pages = vec![page(&[])];
        pages.extend(versions(30));
        let server = world(&pages).await;
        let r = rig_with(server, vec![Attempt::Stream(pings(60))], fast()).await;
        let mut events = r.provider.subscribe().await.unwrap();
        let mut at = Vec::new();
        for _ in 0..6 {
            next(&mut events).await;
            at.push(tokio::time::Instant::now());
        }
        // The first two arrive a safety interval apart (150 ms); after the
        // second strike the poller asks every 20 ms.
        assert!(at[1] - at[0] >= ms(100), "{:?}", at[1] - at[0]);
        assert!(at[5] - at[2] < ms(150), "{:?}", at[5] - at[2]);
        assert_eq!(r.live.status(), Status::Stream, "the connection is kept");
    }

    #[tokio::test]
    async fn envelope_returns_to_safety_cadence() {
        // Two changes the stream does not say put the poller on full cadence;
        // after them the backend is quiet, so only the stream's envelope
        // decides when the cadence relaxes.
        let mut pages = vec![page(&[])];
        pages.extend(versions(2));
        pages.extend(quiet_pages(100));
        let server = world(&pages).await;
        // Pings for 1.5 s, then one envelope, then pings again.
        let first = &crate::tests::envelope::frames("ticks.sse")[0].1;
        let mut steps: Vec<Step> = pings(15);
        steps.pop();
        steps.push(write(&format!(
            "id: C1\nevent: message.sent\ndata: {first}\n\n"
        )));
        steps.extend(pings(40));
        let r = rig_with(server, vec![Attempt::Stream(steps)], fast()).await;
        let started = tokio::time::Instant::now();
        let mut events = r.provider.subscribe().await.unwrap();
        let server = &r.server;
        // Two strikes by 300 ms: full cadence, a poll every 20 ms or so.
        let (a, b) = (
            count_at(&mut events, server, started, 600).await,
            count_at(&mut events, server, started, 1000).await,
        );
        assert!(b - a >= 10, "full cadence: {} polls in 400 ms", b - a);
        // The envelope at 1.5 s: back to a poll every 150 ms.
        let (c, d) = (
            count_at(&mut events, server, started, 1800).await,
            count_at(&mut events, server, started, 2300).await,
        );
        assert!(d - c <= 5, "safety cadence: {} polls in 500 ms", d - c);
        assert!(d - c >= 2, "and still polling: {}", d - c);
        assert_eq!(r.live.status(), Status::Stream);
    }
}

// ----- which source `WuapiProvider::new` builds ----------------------------------------------------------

mod wiring {
    use crate::config::{ApiKey, LiveTransport, WuapiConfig};
    use crate::provider::WuapiProvider;
    use crate::tests::KEY;
    use client_provider::ProviderError;

    fn config(live: LiveTransport, api: &str) -> WuapiConfig {
        WuapiConfig {
            base_url: api.to_owned(),
            live,
            ..WuapiConfig::new("test-agent/1.0")
        }
    }

    #[test]
    fn strict_mode_refuses_an_address_it_cannot_use() {
        // The API is on another machine and plain http: the key would travel
        // in the clear.
        let built = WuapiProvider::new(
            config(LiveTransport::Stream, "http://192.168.1.5"),
            ApiKey::new(KEY),
        );
        assert!(
            matches!(&built, Err(ProviderError::Rejected { code, .. }) if code == "invalid_stream_url"),
            "{:?}",
            built.map(|_| ())
        );
    }

    #[test]
    fn strict_mode_builds_with_a_usable_address() {
        let built = WuapiProvider::new(
            config(LiveTransport::Stream, "http://127.0.0.1:9"),
            ApiKey::new(KEY),
        );
        assert!(built.is_ok());
    }

    #[tokio::test]
    async fn auto_with_an_unusable_address_polls_and_says_why() {
        use client_provider::Provider;
        let server = super::auto::world(&[super::page(&[])]).await;
        // The stream address derived from this API is refused, but the API
        // itself is the mock on localhost: only the stream is unusable.
        let config = WuapiConfig {
            stream_url: Some("http://192.168.1.5".to_owned()),
            ..config(LiveTransport::Auto, &server.uri())
        };
        let provider = WuapiProvider::new(config, ApiKey::new(KEY)).unwrap();
        let _events = provider.subscribe().await.unwrap();
        assert_eq!(
            provider.live_updates(),
            Some(client_provider::LiveUpdates::Polling(
                client_provider::PollingReason::Unavailable
            ))
        );
        // And it was polling: the accounts were read once to prime.
        let seen = crate::tests::requests(&server).await;
        assert!(seen.iter().any(|r| r.url.path() == "/v1/accounts"));
    }
}

// ----- what the provider says about its transport ------------------------------------------------------------

mod saying {
    use super::auto::{fast, rig};
    use crate::config::{ApiKey, LiveTransport, StreamTuning, WuapiConfig};
    use crate::provider::WuapiProvider;
    use crate::tests::fake_stream::{refused, write, Attempt, ScriptedTransport, Step};
    use crate::tests::KEY;
    use client_provider::{LiveUpdates, PollingReason, Provider};
    use futures::StreamExt;
    use std::sync::Arc;

    #[tokio::test]
    async fn polling_mode_says_chosen() {
        let server = wiremock::MockServer::start().await;
        let config = WuapiConfig {
            base_url: server.uri(),
            live: LiveTransport::Polling,
            ..WuapiConfig::new("t/1")
        };
        let provider = WuapiProvider::new(config, ApiKey::new(KEY)).unwrap();
        assert_eq!(
            provider.live_updates(),
            Some(LiveUpdates::Polling(PollingReason::Chosen))
        );
    }

    #[tokio::test]
    async fn auto_says_where_it_is() {
        let r = rig(
            vec![refused(404, r#"{"code":"not_found","message":"x"}"#, None)],
            fast(),
        )
        .await;
        let _events = r.provider.subscribe().await.unwrap();
        assert_eq!(
            r.provider.live_updates(),
            Some(LiveUpdates::Polling(PollingReason::Unavailable))
        );
        // No fallback for the length of the test: the state after the drop
        // is `Reconnecting` for good (the script has no second connect),
        // not for the few milliseconds a sampler might miss under load.
        let steady = StreamTuning {
            fallback_failures: 100,
            fallback_after: std::time::Duration::from_secs(600),
            ..fast()
        };
        let r = rig(vec![Attempt::Stream(vec![Step::Hang])], steady).await;
        let mut events = r.provider.subscribe().await.unwrap();
        assert_eq!(r.provider.live_updates(), Some(LiveUpdates::Stream));
        // Quiet for longer than the idle limit: a drop, and a reconnect.
        let provider = &r.provider;
        super::auto::until(&mut events, "the drop", || {
            provider.live_updates() == Some(LiveUpdates::Reconnecting)
        })
        .await;
    }

    /// A strict provider over a scripted stream, and the transport.
    fn strict(attempts: Vec<Attempt>) -> (WuapiProvider, ScriptedTransport) {
        let transport = ScriptedTransport::new(attempts);
        let config = WuapiConfig {
            base_url: "http://127.0.0.1:9".into(),
            live: LiveTransport::Stream,
            tuning: StreamTuning {
                jitter: || 0.0,
                ..StreamTuning::default()
            },
            ..WuapiConfig::new("t/1")
        };
        let provider =
            WuapiProvider::with_stream(config, ApiKey::new(KEY), Arc::new(transport.clone()))
                .unwrap();
        (provider, transport)
    }

    #[tokio::test(start_paused = true)]
    async fn strict_says_reconnecting_once_its_stream_has_ended_on_a_reset() {
        // The stream ends and the engine is about to open another: for
        // that time the provider is not "Stream".
        let (provider, _) = strict(vec![Attempt::Stream(vec![
            write("event: reset\ndata: {\"reason\":\"cursor_expired\"}\n\n"),
            Step::Hang,
        ])]);
        let mut events = provider.subscribe().await.unwrap();
        assert_eq!(provider.live_updates(), Some(LiveUpdates::Stream));
        assert!(events.next().await.is_none(), "the reset ends the stream");
        assert_eq!(provider.live_updates(), Some(LiveUpdates::Reconnecting));
    }

    #[tokio::test(start_paused = true)]
    async fn strict_says_reconnecting_once_it_has_given_up() {
        let mut attempts = vec![Attempt::Stream(vec![
            write("id: C1\ndata: {}\n\n"),
            Step::Close,
        ])];
        attempts.extend((0..500).map(|_| Attempt::Fail("could not connect".into())));
        let (provider, _) = strict(attempts);
        let mut events = provider.subscribe().await.unwrap();
        assert!(events.next().await.is_none(), "30 minutes down is the end");
        assert_eq!(provider.live_updates(), Some(LiveUpdates::Reconnecting));
    }

    #[tokio::test(start_paused = true)]
    async fn strict_says_reconnecting_once_a_reconnect_is_refused_for_good() {
        let mut attempts = vec![Attempt::Stream(vec![
            write("id: C1\ndata: {}\n\n"),
            Step::Close,
        ])];
        attempts.push(crate::tests::fake_stream::refused(
            404,
            r#"{"code":"not_found","message":"x"}"#,
            None,
        ));
        let (provider, _) = strict(attempts);
        let mut events = provider.subscribe().await.unwrap();
        assert!(events.next().await.is_none());
        assert_eq!(provider.live_updates(), Some(LiveUpdates::Reconnecting));
    }

    #[tokio::test(start_paused = true)]
    async fn strict_says_stream_then_reconnecting() {
        let server = wiremock::MockServer::start().await;
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write("id: C1\ndata: {}\n\n"), Step::Close]),
            Attempt::Fail("could not connect".into()),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let config = WuapiConfig {
            base_url: server.uri(),
            live: LiveTransport::Stream,
            tuning: StreamTuning {
                jitter: || 0.0,
                ..StreamTuning::default()
            },
            ..WuapiConfig::new("t/1")
        };
        let provider =
            WuapiProvider::with_stream(config, ApiKey::new(KEY), Arc::new(transport.clone()))
                .unwrap();
        let mut events = provider.subscribe().await.unwrap();
        assert_eq!(provider.live_updates(), Some(LiveUpdates::Stream));
        // The drop: the stream says nothing, and the provider says it is
        // reconnecting until the connection is back.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(100), events.next()).await;
        assert_eq!(provider.live_updates(), Some(LiveUpdates::Reconnecting));
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), events.next()).await;
        assert_eq!(provider.live_updates(), Some(LiveUpdates::Stream));
    }
}
