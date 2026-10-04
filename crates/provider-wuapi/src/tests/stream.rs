//! The link to the event stream: pacing, resuming, ending.

use crate::config::StreamTuning;
use crate::stream::{Backoff, ConnectBudget};
use std::time::Duration;
use tokio::time::Instant;

fn secs(n: u64) -> Duration {
    Duration::from_secs(n)
}

fn draw_max() -> f64 {
    0.999_999
}

fn draw_zero() -> f64 {
    0.0
}

fn draw_half() -> f64 {
    0.5
}

fn tuning(jitter: fn() -> f64) -> StreamTuning {
    StreamTuning {
        jitter,
        ..StreamTuning::default()
    }
}

fn near(got: Duration, want: Duration) {
    let off = got.abs_diff(want);
    assert!(off < Duration::from_millis(5), "{got:?} is not {want:?}");
}

#[test]
fn backoff_range_and_floor() {
    let mut top = Backoff::new(&tuning(draw_max));
    // The first failure draws from 0..1 s, the second 0..2 s, then 4, 8.
    let mut seen = Vec::new();
    for _ in 0..8 {
        top.connect_failed();
        seen.push(top.wait(Duration::ZERO));
    }
    // The ceiling doubles from 1 s to the 30 s cap, and stays there.
    for (got, want) in seen.iter().zip([1, 2, 4, 8, 16, 30, 30, 30]) {
        near(*got, secs(want));
    }
    // A floor is a least: the draw comes on top of it, never inside it.
    let mut one = Backoff::new(&tuning(draw_max));
    one.connect_failed();
    near(one.wait(secs(7)), secs(8));
    near(one.wait(secs(0)), secs(1));
}

#[test]
fn jitter_is_added_to_the_floor_not_swallowed_by_it() {
    // After a gateway crash every client has the same `retry: 3000`: if a
    // draw smaller than the floor vanished, all of them would come back at
    // exactly 3 s.
    let mut spread = Vec::new();
    for draw in [draw_zero, draw_half, draw_max] {
        let mut backoff = Backoff::new(&tuning(draw));
        backoff.connect_failed();
        spread.push(backoff.wait(secs(3)));
    }
    near(spread[0], secs(3));
    near(spread[1], Duration::from_millis(3500));
    near(spread[2], secs(4));
    // The same for a Retry-After, and still at least what was asked.
    let mut limited = Backoff::new(&tuning(draw_half));
    limited.connect_failed();
    limited.connect_failed();
    near(limited.wait(secs(20)), secs(21));
}

#[test]
fn backoff_min_gap_1s() {
    let mut low = Backoff::new(&tuning(draw_zero));
    low.connect_failed();
    // Full jitter may draw zero; a reconnect never comes sooner than the gap.
    near(low.wait(Duration::ZERO), secs(1));
    // A floor still wins over the gap.
    near(low.wait(secs(3)), secs(3));
}

#[test]
fn backoff_resets_after_stable_30s() {
    let mut backoff = Backoff::new(&tuning(draw_max));
    for _ in 0..4 {
        backoff.connect_failed();
    }
    assert_eq!(backoff.failures(), 4);
    // A connection that ended before 30 s is one more failure.
    backoff.connection_ended(secs(29));
    assert_eq!(backoff.failures(), 5);
    near(backoff.wait(Duration::ZERO), secs(16));
    // One that lived 30 s starts the count again: the next wait is the first.
    backoff.connection_ended(secs(30));
    assert_eq!(backoff.failures(), 0);
    near(backoff.wait(Duration::ZERO), secs(1));
}

#[test]
fn retry_after_clamped_5min() {
    let backoff = Backoff::new(&tuning(draw_max));
    // `retry:` is clamped to a minute, `Retry-After` to five.
    assert_eq!(backoff.floor(Some(secs(3)), None), secs(3));
    assert_eq!(backoff.floor(Some(secs(3600)), None), secs(60));
    assert_eq!(backoff.floor(None, Some(secs(20))), secs(20));
    assert_eq!(backoff.floor(None, Some(secs(86_400))), secs(300));
    // The larger of the two wins.
    assert_eq!(backoff.floor(Some(secs(3)), Some(secs(20))), secs(20));
    assert_eq!(backoff.floor(Some(secs(30)), Some(secs(20))), secs(30));
    assert_eq!(backoff.floor(None, None), Duration::ZERO);
}

#[tokio::test(start_paused = true)]
async fn budget_6_per_rolling_minute() {
    let mut budget = ConnectBudget::new(&StreamTuning::default());
    let start = Instant::now();
    // Six connects fit at once.
    for _ in 0..6 {
        assert_eq!(budget.next_slot(Instant::now()), Instant::now());
        budget.record(Instant::now());
    }
    // The seventh waits for the first to leave the window.
    assert_eq!(budget.next_slot(Instant::now()), start + secs(60));
    // Time passes: the slot is free again once the minute is over.
    tokio::time::advance(secs(61)).await;
    assert_eq!(budget.next_slot(Instant::now()), Instant::now());
    budget.record(Instant::now());
    // Only the newest connect still counts after the others left.
    for _ in 0..5 {
        assert_eq!(budget.next_slot(Instant::now()), Instant::now());
        budget.record(Instant::now());
    }
    assert_eq!(budget.next_slot(Instant::now()), Instant::now() + secs(60));
}

#[tokio::test(start_paused = true)]
async fn asking_for_a_later_slot_does_not_make_the_budget_forget() {
    // A link that waits plans its connect for a later instant. Asking
    // about that instant must not drop the connects that only leave the
    // window by then: the budget is shared with whoever asks about now.
    let mut budget = ConnectBudget::new(&StreamTuning::default());
    let start = Instant::now();
    for _ in 0..6 {
        budget.record(Instant::now());
    }
    assert_eq!(budget.next_slot(start + secs(61)), start + secs(61));
    assert_eq!(budget.next_slot(start + secs(1)), start + secs(60));
    assert_eq!(budget.next_slot(start), start + secs(60));
}

// ----- the fakes ---------------------------------------------------------------

mod fakes {
    use crate::tests::fake_stream::{write, FakeStream, Script};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn fake_stream_records_request_head() {
        let fake = FakeStream::start(vec![Script::stream(vec![write("retry: 3000\n\n")])]).await;
        let mut socket = tokio::net::TcpStream::connect(fake.uri().trim_start_matches("http://"))
            .await
            .unwrap();
        socket
            .write_all(b"GET /v1/events/stream HTTP/1.1\r\nLast-Event-ID: c7\r\n\r\n")
            .await
            .unwrap();
        let mut answer = String::new();
        socket.read_to_string(&mut answer).await.unwrap();
        // What the client sent was kept, head only.
        let heads = fake.heads();
        assert_eq!(heads.len(), 1, "{heads:?}");
        assert!(heads[0].starts_with("GET /v1/events/stream HTTP/1.1"));
        assert!(heads[0].contains("Last-Event-ID: c7"));
        // And what was scripted came back: status, type, then the body.
        assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
        assert!(answer.contains("Content-Type: text/event-stream"));
        assert!(answer.ends_with("retry: 3000\n\n"));
    }
}

// ----- the real transport, against a mock API ---------------------------------------

mod http {
    use crate::config::ApiKey;
    use crate::stream::{attaches_bearer, Answer, ReqwestStreamTransport, StreamTransport};
    use crate::tests::{config, header, KEY};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const STREAM_PATH: &str = "/v1/events/stream";

    fn transport(server: &MockServer) -> ReqwestStreamTransport {
        ReqwestStreamTransport::new(&config(server), ApiKey::new(KEY)).unwrap()
    }

    async fn serve(server: &MockServer, response: ResponseTemplate) {
        Mock::given(method("GET"))
            .and(path(STREAM_PATH))
            .respond_with(response)
            .mount(server)
            .await;
    }

    fn events(body: &str) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_raw(body.as_bytes().to_vec(), "text/event-stream")
    }

    #[tokio::test]
    async fn request_has_bearer_accept_no_key_in_url() {
        let server = MockServer::start().await;
        serve(&server, events("retry: 3000\n\n")).await;
        let answer = transport(&server).connect(None).await.unwrap();
        let Answer::Stream(mut body) = answer else {
            panic!("a stream was expected");
        };
        // The body is the server's, so the request really got through.
        assert_eq!(
            body.chunk().await.unwrap().unwrap(),
            b"retry: 3000\n\n".to_vec()
        );
        let seen = server.received_requests().await.unwrap();
        assert_eq!(seen.len(), 1);
        let request = &seen[0];
        assert_eq!(request.method.as_str(), "GET");
        assert_eq!(request.url.path(), STREAM_PATH);
        assert_eq!(
            header(request, "authorization"),
            Some(&*format!("Bearer {KEY}"))
        );
        assert_eq!(header(request, "accept"), Some("text/event-stream"));
        assert_eq!(header(request, "cache-control"), Some("no-cache"));
        assert_eq!(header(request, "user-agent"), Some("test-agent/1.0"));
        // The key is in the header only: no query, no key in the address.
        assert_eq!(request.url.query(), None);
        assert!(!request.url.as_str().contains("wu_live"));
    }

    #[tokio::test]
    async fn no_last_event_id_on_first_connect() {
        let server = MockServer::start().await;
        serve(&server, events(": ping\n\n")).await;
        let transport = transport(&server);
        transport.connect(None).await.unwrap();
        transport.connect(Some("c1.abc".into())).await.unwrap();
        let seen = server.received_requests().await.unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(header(&seen[0], "last-event-id"), None);
        assert_eq!(header(&seen[1], "last-event-id"), Some("c1.abc"));
    }

    #[tokio::test]
    async fn redirect_not_followed_foreign_origin_never_sees_key() {
        let elsewhere = MockServer::start().await;
        let server = MockServer::start().await;
        serve(
            &server,
            ResponseTemplate::new(302).insert_header(
                "location",
                format!("{}{STREAM_PATH}", elsewhere.uri()).as_str(),
            ),
        )
        .await;
        let answer = transport(&server).connect(None).await.unwrap();
        // The 3xx is the answer; nobody follows it.
        let Answer::Refused(refused) = answer else {
            panic!("a refusal was expected");
        };
        assert_eq!(refused.status, 302);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert!(
            elsewhere.received_requests().await.unwrap().is_empty(),
            "the other origin was never asked, so it never saw the key"
        );
    }

    #[test]
    fn the_request_carries_the_key_only_to_the_endpoints_origin() {
        let config = crate::config::WuapiConfig {
            stream_url: Some("https://stream.example".into()),
            ..crate::config::WuapiConfig::new("t/1")
        };
        let transport = ReqwestStreamTransport::new(&config, ApiKey::new(KEY)).unwrap();
        let bearer = |url: &str| {
            transport
                .request(url, None)
                .build()
                .unwrap()
                .headers()
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        // The endpoint itself: the key.
        assert_eq!(
            bearer("https://stream.example/v1/events/stream").as_deref(),
            Some(format!("Bearer {KEY}").as_str())
        );
        // Another host, another scheme, another port: none.
        assert_eq!(bearer("https://elsewhere.example/v1/events/stream"), None);
        assert_eq!(bearer("http://stream.example/v1/events/stream"), None);
        assert_eq!(bearer("https://stream.example:8443/v1/events/stream"), None);
    }

    #[test]
    fn bearer_only_same_origin() {
        let endpoint = "https://stream.wuapi.dev/v1/events/stream";
        assert!(attaches_bearer(endpoint, endpoint));
        assert!(attaches_bearer(endpoint, "HTTPS://STREAM.wuapi.dev/other"));
        assert!(!attaches_bearer(
            endpoint,
            "https://api.wuapi.dev/v1/events/stream"
        ));
        assert!(!attaches_bearer(
            endpoint,
            "http://stream.wuapi.dev/v1/events/stream"
        ));
        assert!(!attaches_bearer(
            endpoint,
            "https://stream.wuapi.dev:8443/x"
        ));
        assert!(!attaches_bearer(
            endpoint,
            "https://stream.wuapi.dev@evil.example/x"
        ));
    }

    #[tokio::test]
    async fn pre_stream_errors_map() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/stream/errors");
        let cases = [
            (400, "400.json", "application/json", None),
            (401, "401.json", "application/json", None),
            (403, "403.json", "application/json", None),
            (404, "404.json", "application/json", None),
            (429, "429_limit.json", "application/json", Some(30)),
            (503, "503.json", "application/json", Some(7)),
            (401, "not_api.html", "text/html", None),
        ];
        for (status, file, content_type, retry_after) in cases {
            let body = std::fs::read(dir.join(file)).unwrap();
            let server = MockServer::start().await;
            let mut response =
                ResponseTemplate::new(status).set_body_raw(body.clone(), content_type);
            if let Some(seconds) = retry_after {
                response = response.insert_header("retry-after", seconds.to_string().as_str());
            }
            serve(&server, response).await;
            let Answer::Refused(refused) = transport(&server).connect(None).await.unwrap() else {
                panic!("{file}: a refusal was expected");
            };
            assert_eq!(refused.status, status, "{file}");
            assert_eq!(
                refused.content_type.as_deref(),
                Some(content_type),
                "{file}"
            );
            assert_eq!(refused.body, body, "{file}");
            assert_eq!(
                refused.retry_after,
                retry_after.map(Duration::from_secs),
                "{file}"
            );
        }
    }

    #[tokio::test]
    async fn a_200_that_is_not_a_stream_is_a_refusal() {
        let server = MockServer::start().await;
        serve(
            &server,
            ResponseTemplate::new(200).set_body_raw(b"<html></html>".to_vec(), "text/html"),
        )
        .await;
        let Answer::Refused(refused) = transport(&server).connect(None).await.unwrap() else {
            panic!("a refusal was expected");
        };
        assert_eq!(refused.status, 200);
        assert_eq!(refused.content_type.as_deref(), Some("text/html"));
    }

    #[test]
    fn a_stream_url_that_fails_the_check_is_a_construction_error() {
        let mut config = crate::config::WuapiConfig::new("t/1");
        config.base_url = "http://192.168.1.5".to_owned();
        let built = ReqwestStreamTransport::new(&config, ApiKey::new(KEY));
        assert!(matches!(
            built,
            Err(client_provider::ProviderError::Rejected { .. })
        ));
    }
}

// ----- a stream held open on a real socket --------------------------------------------

mod held {
    use crate::config::{ApiKey, StreamTuning, WuapiConfig};
    use crate::sse::{SseItem, SseParser};
    use crate::stream::{Answer, ReqwestStreamTransport, StreamBody, StreamTransport};
    use crate::tests::fake_stream::{write, FakeStream, Script, Step};
    use crate::tests::KEY;
    use std::time::Duration;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A config for a fake on localhost, with a total request timeout far
    /// shorter than the test, and a short idle limit.
    fn config(fake: &FakeStream, idle: Duration) -> WuapiConfig {
        WuapiConfig {
            base_url: fake.uri(),
            request_timeout: ms(300),
            tuning: StreamTuning {
                idle,
                ..StreamTuning::default()
            },
            ..WuapiConfig::new("test-agent/1.0")
        }
    }

    async fn open(fake: &FakeStream, idle: Duration) -> Box<dyn StreamBody> {
        let transport = ReqwestStreamTransport::new(&config(fake, idle), ApiKey::new(KEY)).unwrap();
        match transport.connect(None).await.unwrap() {
            Answer::Stream(body) => body,
            Answer::Refused(refused) => panic!("refused: {refused:?}"),
        }
    }

    #[tokio::test]
    async fn no_total_timeout_pings_only() {
        // A ping every 100 ms for 1.2 s: four times the API client's total
        // timeout, three times the idle limit, and no byte-less gap.
        let mut steps = vec![write("retry: 3000\n\n")];
        for _ in 0..12 {
            steps.push(Step::Sleep(ms(100)));
            steps.push(write(": ping\n\n"));
        }
        steps.push(Step::Hang);
        let fake = FakeStream::start(vec![Script::stream(steps)]).await;
        let mut body = open(&fake, ms(400)).await;
        let started = std::time::Instant::now();
        let mut pings = 0;
        let mut text = String::new();
        while pings < 12 {
            let bytes = body
                .chunk()
                .await
                .unwrap()
                .expect("the stream is still open");
            text.push_str(&String::from_utf8(bytes).unwrap());
            pings = text.matches(": ping").count();
        }
        assert!(started.elapsed() > ms(1000), "{:?}", started.elapsed());
    }

    #[tokio::test]
    async fn idle_detection_reconnects() {
        // One frame, then nothing: past the idle limit the read fails, which
        // the link treats as a drop and answers with a reconnect.
        let fake = FakeStream::start(vec![Script::stream(vec![
            write("id: c1\ndata: 1\n\n"),
            Step::Hang,
        ])])
        .await;
        let mut body = open(&fake, ms(250)).await;
        assert!(body.chunk().await.unwrap().is_some());
        let waited = std::time::Instant::now();
        let broken = body.chunk().await;
        assert!(broken.is_err(), "{broken:?}");
        assert!(waited.elapsed() >= ms(200), "{:?}", waited.elapsed());
        assert!(waited.elapsed() < ms(2000), "{:?}", waited.elapsed());
    }

    #[tokio::test]
    async fn chunk_splits_on_wire() {
        // A frame with a multi-byte character, cut inside the character and
        // between the CR and the LF, with gaps so they arrive as separate
        // chunks.
        let frame = "id: c9\r\nevent: message.received\r\ndata: {\"t\":\"é✓\"}\r\n\r\n".as_bytes();
        let cut_in_check = frame.iter().position(|b| *b == 0x9c).unwrap();
        let cut_in_crlf = frame.iter().position(|b| *b == b'\r').unwrap() + 1;
        let mut steps = Vec::new();
        let mut from = 0;
        for cut in [cut_in_crlf, cut_in_check, frame.len()] {
            steps.push(Step::Write(frame[from..cut].to_vec()));
            steps.push(Step::Sleep(ms(30)));
            from = cut;
        }
        steps.push(Step::Close);
        let fake = FakeStream::start(vec![Script::stream(steps)]).await;
        let mut body = open(&fake, ms(2000)).await;
        let mut parser = SseParser::new(1024);
        let mut chunks = 0;
        let mut items = Vec::new();
        while let Some(bytes) = body.chunk().await.unwrap() {
            chunks += 1;
            items.extend(parser.feed(&bytes));
        }
        assert!(chunks >= 3, "the frame really came in pieces: {chunks}");
        assert_eq!(
            items,
            [SseItem::Event {
                event: "message.received".into(),
                data: "{\"t\":\"é✓\"}".into(),
                id: Some("c9".into()),
            }]
        );
    }
}

// ----- the link, on the paused clock ---------------------------------------------------

mod link {
    use crate::config::StreamTuning;
    use crate::stream::{Limits, Link, LinkEvent, Refusal};
    use crate::tests::fake_stream::{refused, write, Attempt, ScriptedTransport, Step};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::time::Instant;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn quiet() -> f64 {
        0.0
    }

    /// Jitter fixed at zero: every wait is its floor, so tests can name it.
    fn tuning() -> StreamTuning {
        StreamTuning {
            jitter: quiet,
            ..StreamTuning::default()
        }
    }

    fn link_with(transport: &ScriptedTransport, tuning: &StreamTuning, limits: Limits) -> Link {
        Link::new(Arc::new(transport.clone()), tuning, limits)
    }

    fn link(transport: &ScriptedTransport) -> Link {
        link_with(transport, &tuning(), Limits::default())
    }

    async fn take(link: &mut Link, n: usize) -> Vec<LinkEvent> {
        let mut events = Vec::new();
        for _ in 0..n {
            events.push(link.next().await);
        }
        events
    }

    fn data(text: &str) -> LinkEvent {
        LinkEvent::Data(text.to_owned())
    }

    /// True when the link has nothing more to say for ten minutes.
    async fn goes_quiet(link: &mut Link) -> bool {
        tokio::time::timeout(secs(600), link.next()).await.is_err()
    }

    fn last_ids(transport: &ScriptedTransport) -> Vec<Option<String>> {
        transport.connects().into_iter().map(|(id, _)| id).collect()
    }

    #[tokio::test(start_paused = true)]
    async fn short_drop_resumes_with_last_event_id_stream_never_ends() {
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write("id: C5\ndata: a\n\n"), Step::Close]),
            Attempt::Stream(vec![write("id: C6\ndata: b\n\n"), Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 5).await;
        // The drop is told, nothing ends, and the second connection goes on
        // where the first stopped.
        assert_eq!(
            events,
            [
                LinkEvent::Up,
                data("a"),
                LinkEvent::Dropped,
                LinkEvent::Up,
                data("b")
            ]
        );
        assert_eq!(last_ids(&transport), [None, Some("C5".to_owned())]);
        let times: Vec<Instant> = transport.connects().into_iter().map(|(_, at)| at).collect();
        assert!(times[1] - times[0] >= secs(1), "the minimum gap holds");
    }

    #[tokio::test(start_paused = true)]
    async fn cursor_moves_only_after_dispatch() {
        let transport = ScriptedTransport::new(vec![
            // The second frame is cut before its blank line.
            Attempt::Stream(vec![
                write("id: C1\ndata: a\n\n"),
                write("id: C2\ndata: b\n"),
                Step::Close,
            ]),
            // A ping that carries an id moves the cursor and says nothing.
            Attempt::Stream(vec![write(": ping\nid: C9\n\n"), Step::Close]),
            // An empty id clears it.
            Attempt::Stream(vec![
                write("id: C10\ndata: c\n\nid:\ndata: d\n\n"),
                Step::Close,
            ]),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 10).await;
        assert_eq!(
            events,
            [
                LinkEvent::Up,
                data("a"),
                LinkEvent::Dropped,
                LinkEvent::Up,
                LinkEvent::Dropped,
                LinkEvent::Up,
                data("c"),
                data("d"),
                LinkEvent::Dropped,
                LinkEvent::Up,
            ]
        );
        // C1, not C2: the half frame was replayed. C9: the ping's id. None:
        // the empty id cleared it.
        assert_eq!(
            last_ids(&transport),
            [None, Some("C1".to_owned()), Some("C9".to_owned()), None]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn drain_waits_retry_floor_3s_loses_nothing() {
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![
                write("id: C1\ndata: a\n\nretry: 3000\n\n"),
                Step::Close,
            ]),
            Attempt::Stream(vec![write("id: C2\ndata: b\n\n"), Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 5).await;
        assert_eq!(
            events,
            [
                LinkEvent::Up,
                data("a"),
                LinkEvent::Dropped,
                LinkEvent::Up,
                data("b")
            ]
        );
        let connects = transport.connects();
        assert_eq!(connects[1].0.as_deref(), Some("C1"));
        // The jitter drew zero, so only the server's `retry:` made it wait.
        assert_eq!(connects[1].1 - connects[0].1, secs(3));
    }

    #[tokio::test(start_paused = true)]
    async fn reconnect_storm_bounded_to_6_per_minute() {
        // Waits of a millisecond: the budget alone holds the link back.
        let fast = StreamTuning {
            backoff_base: Duration::from_millis(1),
            backoff_cap: Duration::from_millis(1),
            min_gap: Duration::ZERO,
            ..tuning()
        };
        let transport =
            ScriptedTransport::new((0..100).map(|_| Attempt::Fail("refused".into())).collect());
        let mut link = link_with(&transport, &fast, Limits::default());
        let _ = tokio::time::timeout(secs(125), async {
            loop {
                link.next().await;
            }
        })
        .await;
        let times: Vec<Instant> = transport.connects().into_iter().map(|(_, at)| at).collect();
        assert!(times.len() >= 12, "it kept trying: {}", times.len());
        for window in times.windows(7) {
            assert!(
                window[6] - window[0] >= secs(60),
                "seven connects inside one minute"
            );
        }
    }

    fn api_error(code: &str) -> String {
        format!(r#"{{"code":"{code}","message":"words"}}"#)
    }

    #[tokio::test(start_paused = true)]
    async fn retry_after_20_honoured() {
        let transport = ScriptedTransport::new(vec![
            refused(429, &api_error("rate_limited"), Some(20)),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 2).await;
        assert_eq!(
            events,
            [
                LinkEvent::Refused(Refusal {
                    status: 429,
                    code: Some("rate_limited".into()),
                    message: Some("words".into()),
                    api_shaped: true,
                    retry_after: Some(secs(20)),
                    retrying: true,
                }),
                LinkEvent::Up
            ]
        );
        let connects = transport.connects();
        assert_eq!(connects[1].1 - connects[0].1, secs(20));
    }

    #[tokio::test(start_paused = true)]
    async fn a_429_without_retry_after_waits_30_or_60() {
        let transport = ScriptedTransport::new(vec![
            refused(429, &api_error("stream_connection_limit"), None),
            refused(429, &api_error("rate_limited"), None),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut link = link(&transport);
        take(&mut link, 3).await;
        let at: Vec<Instant> = transport.connects().into_iter().map(|(_, at)| at).collect();
        assert_eq!(at[1] - at[0], secs(30), "the connection cap");
        assert_eq!(at[2] - at[1], secs(60), "any other 429");
    }

    #[tokio::test(start_paused = true)]
    async fn non_api_401_is_transient() {
        let transport = ScriptedTransport::new(vec![
            refused(401, "<html>Unauthorized</html>", None),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 2).await;
        assert_eq!(
            events,
            [
                LinkEvent::Refused(Refusal {
                    status: 401,
                    code: None,
                    message: None,
                    api_shaped: false,
                    retry_after: None,
                    retrying: true,
                }),
                LinkEvent::Up
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn api_401_ends_stream_next_open_unauthorized() {
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write("id: C1\ndata: a\n\n"), Step::Close]),
            refused(401, &api_error("unauthorized"), None),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 4).await;
        assert_eq!(
            events[3].clone(),
            LinkEvent::Refused(Refusal {
                status: 401,
                code: Some("unauthorized".into()),
                message: Some("words".into()),
                api_shaped: true,
                retry_after: None,
                retrying: false,
            })
        );
        // It does not try again: the key is dead.
        assert!(goes_quiet(&mut link).await);
        assert_eq!(transport.connects().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn reset_ends_stream_once_cursor_gone() {
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![
                write("id: C1\ndata: a\n\n"),
                write("event: reset\ndata: {\"reason\":\"cursor_expired\"}\n\n"),
                write("id: C3\ndata: z\n\n"),
                Step::Hang,
            ]),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 3).await;
        assert_eq!(events, [LinkEvent::Up, data("a"), LinkEvent::Reset]);
        assert_eq!(link.cursor(), None, "the cursor died with the stream");
        // Nothing after it is delivered and nothing reconnects.
        assert!(goes_quiet(&mut link).await);
        assert_eq!(transport.connects().len(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn reset_events_before_still_delivered() {
        // All in one chunk: what came before the reset is not lost with it.
        let transport = ScriptedTransport::new(vec![Attempt::Stream(vec![
            write(
                "id: C1\ndata: a\n\nid: C2\ndata: b\n\nevent: reset\ndata: {}\n\nid: C4\ndata: z\n\n",
            ),
            Step::Hang,
        ])]);
        let mut link = link(&transport);
        let events = take(&mut link, 4).await;
        assert_eq!(
            events,
            [LinkEvent::Up, data("a"), data("b"), LinkEvent::Reset]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn silence_past_the_idle_limit_is_a_drop() {
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write(": ping\n\n"), Step::Hang]),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 3).await;
        assert_eq!(events, [LinkEvent::Up, LinkEvent::Dropped, LinkEvent::Up]);
        let connects = transport.connects();
        // 45 s of silence, then the minimum gap.
        assert_eq!(connects[1].1 - connects[0].1, secs(46));
    }

    #[tokio::test(start_paused = true)]
    async fn a_connect_without_an_answer_is_told_and_retried() {
        let transport = ScriptedTransport::new(vec![
            Attempt::Fail("could not connect".into()),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut link = link(&transport);
        let events = take(&mut link, 2).await;
        assert_eq!(events, [LinkEvent::Unreachable, LinkEvent::Up]);
    }
}

// ----- the strict source ------------------------------------------------------------------

mod strict {
    use super::secs;
    use crate::config::{ApiKey, LiveTransport, StreamTuning, WuapiConfig};
    use crate::provider::WuapiProvider;
    use crate::tests::envelope::say;
    use crate::tests::fake_stream::{refused, write, Attempt, ScriptedTransport, Step};
    use crate::tests::KEY;
    use client_provider::{EventStream, Provider, ProviderError};
    use futures::StreamExt;
    use wiremock::MockServer;

    fn quiet() -> f64 {
        0.0
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/stream")
                .join(name),
        )
        .unwrap()
    }

    fn provider(server: &MockServer, transport: &ScriptedTransport) -> WuapiProvider {
        let config = WuapiConfig {
            base_url: server.uri(),
            live: LiveTransport::Stream,
            tuning: StreamTuning {
                jitter: quiet,
                ..StreamTuning::default()
            },
            ..WuapiConfig::new("test-agent/1.0")
        };
        WuapiProvider::with_stream(
            config,
            ApiKey::new(KEY),
            std::sync::Arc::new(transport.clone()),
        )
        .unwrap()
    }

    async fn open_error(attempt: Attempt) -> ProviderError {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![attempt]);
        match provider(&server, &transport).subscribe().await {
            Ok(_) => panic!("open succeeded"),
            Err(error) => error,
        }
    }

    fn api(code: &str, message: &str) -> String {
        format!(r#"{{"code":"{code}","message":"{message}"}}"#)
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_401() {
        let error = open_error(refused(401, &api("unauthorized", "Bad key."), None)).await;
        assert!(matches!(error, ProviderError::Unauthorized(m) if m == "Bad key."));
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_401_not_api() {
        let error = open_error(refused(401, "<html>Unauthorized</html>", None)).await;
        assert!(matches!(error, ProviderError::Transient(_)), "{error:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_403() {
        let error = open_error(refused(
            403,
            &api("organization_suspended", "Suspended."),
            None,
        ))
        .await;
        assert!(
            matches!(&error, ProviderError::Rejected { code, message }
                if code == "organization_suspended" && message == "Suspended."),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_404() {
        let error = open_error(refused(404, &api("project_not_found", "No project."), None)).await;
        assert!(
            matches!(&error, ProviderError::Rejected { code, .. } if code == "project_not_found"),
            "{error:?}"
        );
        // A 404 that is not the API's has no code of its own.
        let error = open_error(refused(404, "<html>Not Found</html>", None)).await;
        assert!(
            matches!(&error, ProviderError::Rejected { code, .. } if code == "http_404"),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_400() {
        let error = open_error(refused(400, &api("invalid_request", "No."), None)).await;
        assert!(
            matches!(&error, ProviderError::Rejected { code, .. } if code == "invalid_request"),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_3xx() {
        let error = open_error(refused(302, "", None)).await;
        assert!(
            matches!(&error, ProviderError::Rejected { code, .. } if code == "http_302"),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_200_not_stream() {
        let error = open_error(refused(200, "<html></html>", None)).await;
        assert!(
            matches!(&error, ProviderError::Rejected { code, .. } if code == "http_200"),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_429_limit() {
        let error = open_error(refused(
            429,
            &api("stream_connection_limit", "Too many."),
            Some(30),
        ))
        .await;
        assert!(
            matches!(error, ProviderError::RateLimited { retry_after } if retry_after == Some(secs(30))),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_429_other() {
        let error = open_error(refused(429, &api("rate_limited", "Slow."), None)).await;
        assert!(
            matches!(error, ProviderError::RateLimited { retry_after: None }),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_503() {
        let error = open_error(refused(503, &api("service_unavailable", "Later."), Some(7))).await;
        assert!(matches!(error, ProviderError::Transient(_)), "{error:?}");
        // No answer at all is weather too.
        let error = open_error(Attempt::Fail("could not connect".into())).await;
        assert!(matches!(error, ProviderError::Transient(_)), "{error:?}");
    }

    async fn summary(events: &mut EventStream, n: usize) -> Vec<String> {
        let mut said = Vec::new();
        while said.len() < n {
            let event = events.next().await.expect("the stream is still open");
            said.push(say(&event));
        }
        said
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_maps_the_events_of_a_live_stream() {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![Attempt::Stream(vec![
            write(&fixture("ticks.sse")),
            Step::Hang,
        ])]);
        let mut events = provider(&server, &transport).subscribe().await.unwrap();
        assert_eq!(
            summary(&mut events, 3).await,
            [
                "message m_tick sent",
                "message m_tick delivered",
                "message m_tick read"
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn strict_makes_no_poll_request_5min() {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![Attempt::Stream(vec![
            write(&fixture("ticks.sse")),
            Step::Hang,
        ])]);
        let mut events = provider(&server, &transport).subscribe().await.unwrap();
        summary(&mut events, 3).await;
        // Five quiet minutes (pings would only keep it alive).
        let waited = tokio::time::timeout(secs(300), events.next()).await;
        assert!(waited.is_err(), "the stream did not end or say anything");
        let asked = server.received_requests().await.unwrap();
        assert!(asked.is_empty(), "no poll, no read: {asked:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn strict_gives_up_after_30min_down() {
        let server = MockServer::start().await;
        let mut attempts = vec![Attempt::Stream(vec![
            write("id: C1\ndata: {}\n\n"),
            Step::Close,
        ])];
        attempts.extend((0..500).map(|_| Attempt::Fail("could not connect".into())));
        let transport = ScriptedTransport::new(attempts);
        let mut events = provider(&server, &transport).subscribe().await.unwrap();
        let started = tokio::time::Instant::now();
        // `{}` says nothing, then the drop; the stream ends only at give-up.
        assert!(events.next().await.is_none());
        let down = started.elapsed();
        assert!(down >= secs(30 * 60) && down < secs(31 * 60), "{down:?}");
        let connects = transport.connects();
        assert!(connects.len() > 10, "it kept trying: {}", connects.len());
        // All of them resumed from the cursor.
        assert!(connects[1..]
            .iter()
            .all(|(id, _)| id.as_deref() == Some("C1")));
    }

    #[tokio::test(start_paused = true)]
    async fn strict_ends_on_a_404_during_reconnect() {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write("id: C1\ndata: {}\n\n"), Step::Close]),
            refused(404, &api("project_not_found", "No."), None),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let mut events = provider(&server, &transport).subscribe().await.unwrap();
        assert!(events.next().await.is_none());
        assert_eq!(transport.connects().len(), 2, "it did not go on");
    }

    #[tokio::test(start_paused = true)]
    async fn api_401_mid_stream_ends_it_and_the_next_open_says_unauthorized() {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write("id: C1\ndata: {}\n\n"), Step::Close]),
            refused(401, &api("unauthorized", "Key revoked."), None),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let provider = provider(&server, &transport);
        let mut events = provider.subscribe().await.unwrap();
        assert!(events.next().await.is_none());
        let again = provider.subscribe().await;
        assert!(
            matches!(&again, Err(ProviderError::Unauthorized(m)) if m == "Key revoked."),
            "{:?}",
            again.map(|_| ())
        );
        assert_eq!(
            transport.connects().len(),
            2,
            "no connect for the second open"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn short_drops_never_end_the_event_stream() {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write(&fixture("resume_before.sse")), Step::Close]),
            Attempt::Stream(vec![write(&fixture("resume_after.sse")), Step::Hang]),
        ]);
        let mut events = provider(&server, &transport).subscribe().await.unwrap();
        // Messages 1 and 2, then after the resume 2 again (the same event
        // id, so it is not said twice), 3 and 4.
        let said = summary(&mut events, 4).await;
        let ids: Vec<&str> = said
            .iter()
            .map(|line| line.split(' ').nth(1).unwrap())
            .collect();
        assert_eq!(ids, ["m_res_1", "m_res_2", "m_res_3", "m_res_4"]);
        let connects = transport.connects();
        assert_eq!(connects.len(), 2);
        assert!(connects[1].0.is_some(), "the resume sent Last-Event-ID");
    }

    /// Opens the strict provider every second for a minute while every
    /// connect is refused: what the engine does when REST is healthy and
    /// the stream is not, minus the engine's own pacing.
    #[tokio::test(start_paused = true)]
    async fn strict_open_in_a_loop_connects_at_most_six_a_minute() {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(
            (0..200)
                .map(|_| refused(503, &api("service_unavailable", "Later."), None))
                .collect(),
        );
        let provider = provider(&server, &transport);
        for second in 0..60 {
            let error = provider.subscribe().await.err().expect("refused");
            // The error is the one the refusal gave, connected or not.
            assert!(
                matches!(error, ProviderError::Transient(_)),
                "second {second}: {error:?}"
            );
            tokio::time::sleep(secs(1)).await;
        }
        let connects = transport.connects();
        assert_eq!(connects.len(), 6, "at {:?}", connects);
        // The minute has passed: a connect is allowed again.
        assert!(provider.subscribe().await.is_err());
        assert_eq!(transport.connects().len(), 7);
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_after_unreachable_is_gated_too() {
        // Triangulation: no answer at all, not a refusal.
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(Vec::new());
        let provider = provider(&server, &transport);
        for _ in 0..30 {
            let error = provider.subscribe().await.err().expect("no script");
            assert!(matches!(error, ProviderError::Transient(_)), "{error:?}");
            tokio::time::sleep(secs(1)).await;
        }
        assert_eq!(transport.connects().len(), 6);
    }

    async fn not_before(code: &str, retry_after: Option<u64>, wait: u64) {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![
            refused(429, &api(code, "Slow."), retry_after),
            Attempt::Stream(vec![Step::Hang]),
        ]);
        let provider = provider(&server, &transport);
        assert!(matches!(
            provider.subscribe().await.err(),
            Some(ProviderError::RateLimited { .. })
        ));
        tokio::time::sleep(secs(wait - 1)).await;
        // Asked to wait: the answer is the last error, with no connect.
        assert!(matches!(
            provider.subscribe().await.err(),
            Some(ProviderError::RateLimited { .. })
        ));
        assert_eq!(transport.connects().len(), 1, "no connect before {wait} s");
        tokio::time::sleep(secs(2)).await;
        assert!(provider.subscribe().await.is_ok());
        assert_eq!(transport.connects().len(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_does_not_connect_before_retry_after() {
        not_before("rate_limited", Some(20), 20).await;
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_waits_30_s_on_a_connection_limit_without_retry_after() {
        not_before("stream_connection_limit", None, 30).await;
    }

    #[tokio::test(start_paused = true)]
    async fn strict_open_waits_60_s_on_any_other_429_without_retry_after() {
        not_before("rate_limited", None, 60).await;
    }

    #[tokio::test(start_paused = true)]
    async fn duplicate_evt_emitted_once() {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![Attempt::Stream(vec![
            write(&fixture("duplicate.sse")),
            // A marker after it, to know the duplicate was passed over.
            write(&fixture("ticks.sse")),
            Step::Hang,
        ])]);
        let mut events = provider(&server, &transport).subscribe().await.unwrap();
        let said = summary(&mut events, 4).await;
        assert_eq!(
            said.iter()
                .filter(|line| line.starts_with("message m_dup"))
                .count(),
            1,
            "{said:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn reset_ends_stream_once_cursor_gone_and_events_before_still_arrive() {
        let server = MockServer::start().await;
        let transport = ScriptedTransport::new(vec![Attempt::Stream(vec![
            write(&fixture("ticks.sse")),
            write(&fixture("reset.sse")),
            Step::Hang,
        ])]);
        let mut events = provider(&server, &transport).subscribe().await.unwrap();
        let mut said = Vec::new();
        while let Some(event) = events.next().await {
            said.push(say(&event));
        }
        // The three ticks came before the reset; what follows it is not read.
        assert_eq!(said.len(), 3, "{said:?}");
        assert_eq!(transport.connects().len(), 1);
    }
}

// ----- the backend's fixtures ---------------------------------------------------------------

mod contract {
    use crate::config::{ApiKey, LiveTransport, StreamTuning, WuapiConfig};
    use crate::provider::WuapiProvider;
    use crate::stream::{Limits, Link, LinkEvent};
    use crate::tests::fake_stream::{write, Attempt, ScriptedTransport, Step};
    use crate::tests::KEY;
    use client_provider::Provider;
    use futures::StreamExt;
    use std::sync::Arc;
    use std::time::Duration;
    use wiremock::MockServer;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/stream")
                .join(name),
        )
        .unwrap()
    }

    fn quiet() -> f64 {
        0.0
    }

    fn tuning() -> StreamTuning {
        StreamTuning {
            jitter: quiet,
            ..StreamTuning::default()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn drain_retry_is_whatever_the_server_says() {
        // The gateway draws it between 1 and 20 s: 7 s here, after the
        // 3 s every stream starts with. The last one wins.
        let transport = ScriptedTransport::new(vec![
            Attempt::Stream(vec![write(&fixture("drain.sse")), Step::Close]),
            Attempt::Stream(vec![write(&fixture("resume_after.sse")), Step::Hang]),
        ]);
        let mut link = Link::new(Arc::new(transport.clone()), &tuning(), Limits::default());
        let mut said = Vec::new();
        while said.len() < 6 {
            said.push(link.next().await);
        }
        assert_eq!(said[0], LinkEvent::Up);
        assert_eq!(said[2], LinkEvent::Dropped);
        let connects = transport.connects();
        assert_eq!(connects[1].1 - connects[0].1, Duration::from_secs(7));
        assert_eq!(connects[1].0.as_deref(), Some("c1.mfx2kc001"), "it resumed");
        // Every stream starts with `retry: 3000`: the leading one is taken in
        // stride, and a later stream is read the same way.
        assert!(said[3..].contains(&LinkEvent::Up));
    }

    #[tokio::test(start_paused = true)]
    async fn every_reset_reason_ends_the_stream_once() {
        for name in ["reset.sse", "reset_unknown.sse", "reset_not_logged.sse"] {
            let server = MockServer::start().await;
            let transport = ScriptedTransport::new(vec![
                Attempt::Stream(vec![write(&fixture(name)), Step::Hang]),
                Attempt::Stream(vec![Step::Hang]),
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
            // The frame after the reset is not read: the stream ends.
            assert!(events.next().await.is_none(), "{name}");
            assert_eq!(transport.connects().len(), 1, "{name}: no reconnect");
        }
    }
}
