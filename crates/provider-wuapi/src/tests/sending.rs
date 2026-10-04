//! A message sent from here, from the API's `202` to its ticks: what is
//! asked, when, and how often. The mock API answers on localhost; the
//! clock is the test's own, handed to the poller step by step, so two
//! minutes of following take no time and the moments are exact.

use super::*;
use crate::events::{Poller, PollingEventSource};
use crate::follow::{self, Fetch, Follow, Now};
use client_core::{run_outbox_pass, OutboxConfig, Store, SyncConfig, SyncEngine};
use wiremock::matchers::query_param;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

const CHAT: &str = "+584245550199";
const EMPTY: &str = r#"{"object":"list","items":[],"nextCursor":null}"#;

fn iso(epoch_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(epoch_ms)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A text of the account's own, as the API returns it: created at
/// `created`, last changed `changed` milliseconds later.
fn wire(id: &str, status: &str, created: i64, changed: i64) -> serde_json::Value {
    let mut message: serde_json::Value = serde_json::from_str(fixture!("message")).unwrap();
    message["id"] = id.into();
    message["status"] = status.into();
    message["createdAt"] = iso(created).into();
    message["updatedAt"] = iso(created + changed).into();
    if status != "queued" && status != "failed" {
        message["sentAt"] = iso(created + 300).into();
    }
    message["metadata"] = serde_json::json!({});
    message
}

fn parsed(message: &serde_json::Value) -> api::Message {
    serde_json::from_value(message.clone()).expect("the test's message is a Message")
}

fn list(items: &[serde_json::Value]) -> String {
    serde_json::json!({"object": "list", "items": items, "nextCursor": null}).to_string()
}

/// Answers `verb at` the same way every time it is asked.
async fn always(server: &MockServer, verb: &str, at: &str, answer: ResponseTemplate) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(answer)
        .mount(server)
        .await;
}

/// `POST /v1/messages`: accepts whatever is sent as `m_<client id>`, in
/// `status`, with the client id echoed in the metadata as the API does.
struct Accepts {
    status: &'static str,
    created: i64,
    delay: Mutex<VecDeque<Duration>>,
}

impl Accepts {
    fn new(status: &'static str, created: i64) -> Self {
        Self {
            status,
            created,
            delay: Mutex::new(VecDeque::new()),
        }
    }

    /// The first answers are this slow, in order; the rest come at once.
    fn slow(self, delays: impl IntoIterator<Item = Duration>) -> Self {
        *self.delay.lock().unwrap() = delays.into_iter().collect();
        self
    }
}

impl Respond for Accepts {
    fn respond(&self, request: &wiremock::Request) -> ResponseTemplate {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let client_id = body["metadata"]["clientMessageId"].as_str().unwrap();
        let mut message = wire(&format!("m_{client_id}"), self.status, self.created, 0);
        message["metadata"] = serde_json::json!({ "clientMessageId": client_id });
        let answer = reply(202, &message.to_string());
        match self.delay.lock().unwrap().pop_front() {
            Some(delay) => answer.set_delay(delay),
            None => answer,
        }
    }
}

/// One connected account, no chats, no messages, unless a test mounts
/// something more specific first.
async fn quiet_api(server: &MockServer) {
    let accounts = format!(
        r#"{{"object":"list","items":[{}],"nextCursor":null}}"#,
        fixture!("account")
    );
    always(server, "GET", "/v1/accounts", reply(200, &accounts)).await;
    always(
        server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/chats"),
        reply(200, EMPTY),
    )
    .await;
    always(server, "GET", "/v1/messages", reply(200, EMPTY)).await;
}

struct Rig {
    provider: Arc<WuapiProvider>,
    source: Arc<PollingEventSource>,
    poller: Poller,
    engine: SyncEngine,
}

fn rig(server: &MockServer) -> Rig {
    rig_with(config(server))
}

fn rig_with(config: WuapiConfig) -> Rig {
    let (provider, source) = WuapiProvider::with_polling(config, ApiKey::new(KEY)).unwrap();
    let provider = Arc::new(provider);
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        provider.clone(),
        SyncConfig::default(),
        tokio::runtime::Handle::current(),
    );
    Rig {
        poller: source.poller(),
        provider,
        source,
        engine,
    }
}

impl Rig {
    fn account(&self) -> AccountId {
        AccountId::new(ACCOUNT)
    }

    fn chat(&self) -> ChatId {
        ChatId::new(CHAT)
    }

    /// The bubbles of the chat: (id, status).
    fn bubbles(&self) -> Vec<(String, DeliveryStatus)> {
        self.engine
            .store()
            .messages(&self.account(), &self.chat(), 100)
            .unwrap()
            .into_iter()
            .map(|m| (m.message.id.to_string(), m.message.status))
            .collect()
    }

    /// The tick of the one bubble there is.
    fn tick(&self) -> DeliveryStatus {
        let bubbles = self.bubbles();
        assert_eq!(bubbles.len(), 1, "one bubble: {bubbles:?}");
        bubbles[0].1.clone()
    }

    fn apply(&self, events: Vec<ProviderEvent>) -> usize {
        let count = events.len();
        for event in events {
            self.engine.apply_event(event).unwrap();
        }
        count
    }

    /// Follows up at `now` and hands what it finds to the engine. Returns
    /// how many events that was.
    async fn follow_up(&mut self, now: Now) -> usize {
        let events = self.poller.follow_up(now).await.unwrap();
        self.apply(events)
    }

    async fn poll(&mut self, now: Now) -> usize {
        let events = self.poller.poll(now).await.unwrap();
        self.apply(events)
    }

    fn text(&self, client_id: &str) -> OutgoingMessage {
        OutgoingMessage {
            client_id: ClientMessageId::new(client_id),
            account_id: self.account(),
            chat_id: self.chat(),
            content: OutgoingContent::Text {
                body: "Esta bien.".into(),
            },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: false,
        }
    }
}

/// The requests for one message by its id.
async fn asked_by_id(server: &MockServer, id: &str) -> usize {
    let at = format!("/v1/messages/{id}");
    requests(server)
        .await
        .iter()
        .filter(|request| request.url.path() == at)
        .count()
}

fn secs(seconds: f64) -> Duration {
    Duration::from_secs_f64(seconds)
}

// ----- from the 202 to the ticks ------------------------------------------

#[tokio::test]
async fn an_accepted_message_leaves_the_clock_and_its_ticks_follow_within_moments() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("queued", created))
        .mount(&server)
        .await;
    let step = |status: &str, changed: i64| {
        reply(200, &wire("m_c1", status, created, changed).to_string())
    };
    on(
        &server,
        "GET",
        "/v1/messages/m_c1",
        vec![
            step("queued", 0),
            step("sent", 900),
            step("delivered", 2_400),
            step("read", 15_000),
        ],
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    rig.poll(Now::real()).await;

    // Enter: the bubble is there before any request, with the clock.
    let mut ticks = Vec::new();
    rig.engine
        .store()
        .enqueue(
            &rig.text("c1"),
            client_provider::Timestamp::now(),
            Duration::from_secs(60),
        )
        .unwrap();
    ticks.push(rig.tick());
    // The API answers 202: the clock goes at once, on the same row, which
    // now carries the API's id.
    rig.engine.flush_outbox().await.unwrap();
    ticks.push(rig.tick());
    assert_eq!(rig.bubbles()[0].0, "m_c1");
    assert_eq!(rig.source.following(), 1);
    let sent = Now::real();

    // Nothing is asked before the first follow-up is due, half a second
    // after the API accepted the message.
    assert_eq!(rig.follow_up(sent.plus(secs(0.3))).await, 0);
    assert_eq!(asked_by_id(&server, "m_c1").await, 0);
    // 0.5 s: still queued. Asked, and nothing to say.
    assert_eq!(rig.follow_up(sent.plus(secs(0.6))).await, 0);
    assert_eq!(asked_by_id(&server, "m_c1").await, 1);
    ticks.push(rig.tick());
    // Not again before 1.5 s.
    assert_eq!(rig.follow_up(sent.plus(secs(1.2))).await, 0);
    assert_eq!(asked_by_id(&server, "m_c1").await, 1);
    // 1.5 s: WhatsApp has it. One tick.
    assert_eq!(rig.follow_up(sent.plus(secs(1.6))).await, 1);
    ticks.push(rig.tick());
    // 3.5 s: it reached the phone. Two ticks.
    assert_eq!(rig.follow_up(sent.plus(secs(2.5))).await, 0);
    assert_eq!(rig.follow_up(sent.plus(secs(3.6))).await, 1);
    ticks.push(rig.tick());
    assert_eq!(asked_by_id(&server, "m_c1").await, 3);
    // Delivered: no hurry any more. Asked again a quarter of a minute
    // later, when it has been read.
    assert_eq!(rig.follow_up(sent.plus(secs(10.))).await, 0);
    assert_eq!(asked_by_id(&server, "m_c1").await, 3);
    assert_eq!(rig.follow_up(sent.plus(secs(19.))).await, 1);
    ticks.push(rig.tick());
    // Read: there is nothing left to follow, ever.
    assert_eq!(rig.source.following(), 0);
    assert_eq!(rig.follow_up(sent.plus(secs(3_600.))).await, 0);

    assert_eq!(
        ticks,
        [
            DeliveryStatus::Pending,
            DeliveryStatus::Accepted,
            DeliveryStatus::Accepted,
            DeliveryStatus::Sent,
            DeliveryStatus::Delivered,
            DeliveryStatus::Read,
        ]
    );
    assert_eq!(
        asked_by_id(&server, "m_c1").await,
        4,
        "four requests in all"
    );
    let posts = requests(&server)
        .await
        .iter()
        .filter(|request| request.method.as_str() == "POST")
        .count();
    assert_eq!(posts, 1);
    // The times of it are in the store, for `--diagnose sends`.
    let timing = rig.engine.store().send_timings(1).unwrap().remove(0);
    assert!(timing.accepted_at.is_some() && timing.read_at.is_some());
}

#[tokio::test]
async fn a_message_that_never_arrives_is_asked_about_eight_times_in_its_first_two_minutes() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("sent", created))
        .mount(&server)
        .await;
    // The other phone is off: sent, and nothing more.
    always(
        &server,
        "GET",
        "/v1/messages/m_c1",
        reply(200, &wire("m_c1", "sent", created, 0).to_string()),
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    rig.provider.send(rig.text("c1")).await.unwrap();
    let sent = Now::real();

    // Every 100 ms for two minutes: as often as the stream could wake.
    let mut found = 0;
    for tenth in 1..=1_200 {
        found += rig.follow_up(sent.plus(secs(f64::from(tenth) / 10.))).await;
    }
    assert_eq!(found, 0, "nothing changed, nothing reported");
    assert_eq!(
        asked_by_id(&server, "m_c1").await,
        follow::close_points().len()
    );
    assert_eq!(follow::close_points().len(), 8);
    // From then on at the slow pace: an eighth of its age apart.
    for second in 121..=600 {
        rig.follow_up(sent.plus(secs(f64::from(second)))).await;
    }
    let slow = asked_by_id(&server, "m_c1").await - 8;
    assert!((8..=14).contains(&slow), "{slow} requests in eight minutes");
    assert_eq!(rig.source.following(), 1, "still watched, for hours");
}

#[tokio::test]
async fn several_messages_in_flight_in_one_chat_share_one_request() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("queued", created))
        .mount(&server)
        .await;
    // The chat's newest page shows two of the three, both sent by now.
    let page = list(&[
        wire("m_c3", "sent", created, 700),
        wire("m_c2", "sent", created, 600),
    ]);
    Mock::given(method("GET"))
        .and(path("/v1/messages"))
        .and(query_param("chatId", CHAT))
        .respond_with(reply(200, &page))
        .mount(&server)
        .await;
    always(
        &server,
        "GET",
        "/v1/messages/m_c1",
        reply(200, &wire("m_c1", "delivered", created, 800).to_string()),
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    for client_id in ["c1", "c2", "c3"] {
        rig.provider.send(rig.text(client_id)).await.unwrap();
    }
    assert_eq!(rig.source.following(), 3);
    let sent = Now::real();
    let before = requests(&server).await.len();

    // Three are due: one request, the chat's newest page.
    assert_eq!(rig.follow_up(sent.plus(secs(0.7))).await, 2);
    let asked: Vec<String> = requests(&server).await[before..]
        .iter()
        .map(target)
        .collect();
    assert_eq!(
        asked,
        [format!(
            "/v1/messages?accountId={ACCOUNT}&chatId=%2B584245550199&limit=100"
        )]
    );
    // The one that page did not show is asked for by its id, at once, and
    // from then on.
    assert_eq!(rig.follow_up(sent.plus(secs(0.8))).await, 1);
    assert_eq!(asked_by_id(&server, "m_c1").await, 1);
    assert_eq!(requests(&server).await.len(), before + 2);
}

#[tokio::test]
async fn the_stream_wakes_for_a_follow_up_without_waiting_for_the_next_poll() {
    use futures::StreamExt;
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("queued", created))
        .mount(&server)
        .await;
    always(
        &server,
        "GET",
        "/v1/messages/m_c1",
        reply(200, &wire("m_c1", "sent", created, 300).to_string()),
    )
    .await;
    quiet_api(&server).await;
    // On the real clock, with a general poll that is a minute away.
    let rig = rig_with(WuapiConfig {
        poll_interval: Duration::from_secs(60),
        ..config(&server)
    });
    let mut events = rig.provider.subscribe().await.unwrap();
    let started = std::time::Instant::now();
    rig.provider.send(rig.text("c1")).await.unwrap();

    let event = tokio::time::timeout(Duration::from_secs(5), events.next())
        .await
        .expect("the follow-up comes by itself")
        .unwrap();
    let took = started.elapsed();
    match event {
        ProviderEvent::MessageUpserted(message) => {
            assert_eq!(message.id.as_str(), "m_c1");
            assert_eq!(message.status, DeliveryStatus::Sent);
        }
        other => panic!("{other:?}"),
    }
    assert!(
        took >= Duration::from_millis(450) && took < Duration::from_secs(3),
        "half a second after the API accepted it; took {took:?}"
    );
    // One poll to prime, one request by id, and no poll since.
    let asked: Vec<String> = requests(&server)
        .await
        .iter()
        .filter(|request| request.method.as_str() == "GET")
        .map(target)
        .collect();
    assert_eq!(
        asked,
        [
            "/v1/accounts?limit=100".to_owned(),
            "/v1/messages?limit=100".to_owned(),
            format!("/v1/accounts/{ACCOUNT}/chats?limit=50"),
            format!("/v1/accounts/{ACCOUNT}/stories/own?limit=100"),
            "/v1/messages/m_c1".to_owned(),
        ]
    );
}

// ----- the slow ways a send can go ----------------------------------------

#[tokio::test]
async fn a_poll_that_sees_the_message_before_its_post_returns_makes_one_bubble() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    // The API takes the message at once and answers a second late.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("queued", created).slow([Duration::from_millis(700)]))
        .mount(&server)
        .await;
    let mut seen = wire("m_c1", "sent", created, 400);
    seen["metadata"] = serde_json::json!({ "clientMessageId": "c1" });
    on(
        &server,
        "GET",
        "/v1/messages",
        vec![reply(200, EMPTY), reply(200, &list(&[seen]))],
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    rig.poll(Now::real()).await;
    rig.engine
        .store()
        .enqueue(
            &rig.text("c1"),
            client_provider::Timestamp::now(),
            Duration::from_secs(60),
        )
        .unwrap();

    let flush = {
        let engine = rig.engine.clone();
        tokio::spawn(async move { engine.flush_outbox().await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rig.tick(),
        DeliveryStatus::Pending,
        "the answer is not here yet"
    );

    // The general poll comes by first, and already finds it sent.
    assert_eq!(rig.poll(Now::real()).await, 1);
    assert_eq!(rig.bubbles(), [("m_c1".to_owned(), DeliveryStatus::Sent)]);
    assert!(
        rig.engine.store().outbox_pending().unwrap().is_empty(),
        "the API has it: nothing is left to send"
    );
    assert_eq!(rig.source.following(), 1, "and its ticks are followed");

    // Then the answer to the request arrives, older than what is shown.
    let pass = flush.await.unwrap().unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(
        rig.bubbles(),
        [("m_c1".to_owned(), DeliveryStatus::Sent)],
        "still one bubble, and its tick did not go back"
    );
}

#[tokio::test]
async fn a_post_that_times_out_stays_pending_and_its_repeat_is_the_same_message() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    // The first answer comes after the client gave up; the repeat is
    // answered at once, as the API replays what it has under that key.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("sent", created).slow([Duration::from_millis(1_500)]))
        .mount(&server)
        .await;
    quiet_api(&server).await;
    let rig = rig_with(WuapiConfig {
        request_timeout: Duration::from_millis(250),
        ..config(&server)
    });
    let store = rig.engine.store().clone();
    let outbox = OutboxConfig::default();
    let t0 = client_provider::Timestamp::now();
    store
        .enqueue(&rig.text("c1"), t0, Duration::from_secs(60))
        .unwrap();

    let pass = run_outbox_pass(&store, rig.provider.as_ref(), &outbox, t0)
        .await
        .unwrap();
    assert_eq!((pass.sent, pass.retried, pass.failed), (0, 1, 0));
    assert_eq!(
        rig.bubbles(),
        [("local:c1".to_owned(), DeliveryStatus::Pending)],
        "not known to have arrived: the clock stays, and nothing failed"
    );
    assert_eq!(rig.source.following(), 0);

    // The retry is due two seconds later.
    let retry = client_provider::Timestamp::from_millis(
        t0.as_millis() + outbox.backoff(1).as_millis() as i64,
    );
    assert_eq!(outbox.backoff(1), Duration::from_secs(2));
    let pass = run_outbox_pass(&store, rig.provider.as_ref(), &outbox, retry)
        .await
        .unwrap();
    assert_eq!((pass.sent, pass.retried, pass.failed), (1, 0, 0));
    assert_eq!(rig.bubbles(), [("m_c1".to_owned(), DeliveryStatus::Sent)]);
    assert_eq!(rig.source.following(), 1);

    let posts: Vec<_> = requests(&server)
        .await
        .into_iter()
        .filter(|request| request.method.as_str() == "POST")
        .collect();
    assert_eq!(posts.len(), 2);
    assert_eq!(header(&posts[0], "idempotency-key"), Some("c1"));
    assert_eq!(header(&posts[1], "idempotency-key"), Some("c1"));
    assert_eq!(posts[0].body, posts[1].body, "the identical request");
}

#[tokio::test]
async fn a_receipt_that_comes_minutes_later_is_found_off_the_newest_page() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("queued", created))
        .mount(&server)
        .await;
    let step = |status: &str, changed: i64| {
        reply(200, &wire("m_c1", status, created, changed).to_string())
    };
    on(
        &server,
        "GET",
        "/v1/messages/m_c1",
        vec![
            step("sent", 500),
            step("sent", 500),
            step("delivered", 400_000),
        ],
    )
    .await;
    // The organization is busy: by the time of the next general poll its
    // newest page is somebody else's messages, and stays that way.
    on(
        &server,
        "GET",
        "/v1/messages",
        vec![
            reply(200, EMPTY),
            reply(200, fixture!("message_list_last")),
            reply(200, fixture!("message_list_last")),
        ],
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    rig.poll(Now::real()).await;
    rig.engine
        .store()
        .enqueue(
            &rig.text("c1"),
            client_provider::Timestamp::now(),
            Duration::from_secs(60),
        )
        .unwrap();
    rig.engine.flush_outbox().await.unwrap();
    let sent = Now::real();
    assert_eq!(rig.follow_up(sent.plus(secs(0.6))).await, 1);
    assert_eq!(rig.tick(), DeliveryStatus::Sent);

    // Five minutes on, the phone of the other side is still off. The
    // general poll does not show the message at all.
    let later = sent.plus(secs(300.));
    rig.poll(later).await;
    assert_eq!(
        rig.engine
            .store()
            .message(&rig.account(), &MessageId::new("m_c1"))
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Sent
    );
    assert_eq!(rig.follow_up(later).await, 0);
    assert_eq!(asked_by_id(&server, "m_c1").await, 2);
    // Not asked again within the same half minute...
    assert_eq!(rig.follow_up(later.plus(secs(20.))).await, 0);
    assert_eq!(asked_by_id(&server, "m_c1").await, 2);
    // ...and a minute later the phone is back: delivered, found by id.
    rig.poll(later.plus(secs(60.))).await;
    assert_eq!(rig.follow_up(later.plus(secs(60.))).await, 1);
    assert_eq!(asked_by_id(&server, "m_c1").await, 3);
    assert_eq!(
        rig.engine
            .store()
            .message(&rig.account(), &MessageId::new("m_c1"))
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );
}

#[tokio::test]
async fn a_rate_limit_during_the_follow_up_is_waited_out() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("queued", created))
        .mount(&server)
        .await;
    on(
        &server,
        "GET",
        "/v1/messages/m_c1",
        vec![
            reply(429, r#"{"code":"rate_limited","message":"Slow down."}"#)
                .insert_header("Retry-After", "7"),
            reply(200, &wire("m_c1", "delivered", created, 900).to_string()),
        ],
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    rig.engine
        .store()
        .enqueue(
            &rig.text("c1"),
            client_provider::Timestamp::now(),
            Duration::from_secs(60),
        )
        .unwrap();
    rig.engine.flush_outbox().await.unwrap();
    let sent = Now::real();

    assert_eq!(rig.follow_up(sent.plus(secs(0.6))).await, 0);
    assert_eq!(asked_by_id(&server, "m_c1").await, 1);
    assert_eq!(rig.tick(), DeliveryStatus::Accepted, "nothing failed");
    // For seven seconds nothing is asked, though follow-ups were due.
    for at in [1.6, 3.6, 7.0] {
        assert_eq!(rig.follow_up(sent.plus(secs(at))).await, 0);
    }
    assert_eq!(asked_by_id(&server, "m_c1").await, 1);
    // Then at once.
    assert_eq!(rig.follow_up(sent.plus(secs(7.7))).await, 1);
    assert_eq!(asked_by_id(&server, "m_c1").await, 2);
    assert_eq!(rig.tick(), DeliveryStatus::Delivered);
}

#[tokio::test]
async fn a_message_the_api_could_not_send_shows_failed_with_its_reason() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(Accepts::new("queued", created))
        .mount(&server)
        .await;
    let mut failed = wire("m_c1", "failed", created, 800);
    failed["error"] = serde_json::json!({
        "code": "not_on_whatsapp",
        "message": "The recipient has no WhatsApp."
    });
    always(
        &server,
        "GET",
        "/v1/messages/m_c1",
        reply(200, &failed.to_string()),
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    rig.engine
        .store()
        .enqueue(
            &rig.text("c1"),
            client_provider::Timestamp::now(),
            Duration::from_secs(60),
        )
        .unwrap();
    rig.engine.flush_outbox().await.unwrap();
    assert_eq!(rig.tick(), DeliveryStatus::Accepted);
    let sent = Now::real();

    assert_eq!(rig.follow_up(sent.plus(secs(0.6))).await, 1);
    assert_eq!(
        rig.tick(),
        DeliveryStatus::Failed {
            reason: "The recipient has no WhatsApp.".into()
        }
    );
    assert_eq!(rig.source.following(), 0, "failed is final");
    assert_eq!(rig.follow_up(sent.plus(secs(60.))).await, 0);
    assert_eq!(asked_by_id(&server, "m_c1").await, 1);
}

// ----- the general poll ---------------------------------------------------

/// An account of the fixture's shape, with its own id and status.
fn account(id: &str, status: &str) -> serde_json::Value {
    let mut account: serde_json::Value = serde_json::from_str(fixture!("account")).unwrap();
    account["id"] = id.into();
    account["status"] = status.into();
    account
}

#[tokio::test]
async fn numbers_that_are_not_linked_cost_a_poll_nothing() {
    let server = MockServer::start().await;
    let accounts = list(&[
        account(ACCOUNT, "ready"),
        account("never_linked_1", "qr_ready"),
        account("never_linked_2", "initializing"),
        account("logged_out", "disconnected"),
    ]);
    always(&server, "GET", "/v1/accounts", reply(200, &accounts)).await;
    always(&server, "GET", "/v1/messages", reply(200, EMPTY)).await;
    always(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/chats"),
        reply(200, EMPTY),
    )
    .await;
    // Asking any of the others would take seconds, if it were done.
    for id in ["never_linked_1", "never_linked_2", "logged_out"] {
        always(
            &server,
            "GET",
            &format!("/v1/accounts/{id}/chats"),
            reply(200, EMPTY).set_delay(Duration::from_secs(3)),
        )
        .await;
    }
    let mut rig = rig(&server);

    let started = std::time::Instant::now();
    // Five polls: the first and the fifth read every account's chats.
    for _ in 0..5 {
        rig.poll(Now::real()).await;
    }
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "five polls took {:?}",
        started.elapsed()
    );
    let asked: Vec<String> = requests(&server).await.iter().map(target).collect();
    let sweep = [
        "/v1/accounts?limit=100".to_owned(),
        "/v1/messages?limit=100".to_owned(),
        format!("/v1/accounts/{ACCOUNT}/chats?limit=50"),
    ];
    let quiet = &sweep[..2];
    // The stories of the connected number are read on the first poll (and
    // then every few rounds), never those of the others.
    let stories = [format!("/v1/accounts/{ACCOUNT}/stories/own?limit=100")];
    let expected: Vec<String> =
        [&sweep[..], &stories[..], quiet, quiet, quiet, &sweep[..]].concat();
    assert_eq!(asked, expected, "as many requests as with one number");
}

#[tokio::test]
async fn the_chats_of_several_numbers_are_read_at_the_same_time() {
    let server = MockServer::start().await;
    let ids = ["one", "two", "three", "four"];
    let accounts = list(&ids.map(|id| account(id, "ready")));
    always(&server, "GET", "/v1/accounts", reply(200, &accounts)).await;
    always(&server, "GET", "/v1/messages", reply(200, EMPTY)).await;
    for id in ids {
        always(
            &server,
            "GET",
            &format!("/v1/accounts/{id}/chats"),
            reply(200, EMPTY).set_delay(Duration::from_millis(400)),
        )
        .await;
    }
    let mut rig = rig(&server);
    let started = std::time::Instant::now();
    rig.poll(Now::real()).await;
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(1_200),
        "four numbers one after the other would take 1.6 s; took {took:?}"
    );
    // Accounts, messages, four chat lists, and each number's stories.
    assert_eq!(requests(&server).await.len(), 10);
}

#[tokio::test]
async fn a_number_whose_chats_cannot_be_read_does_not_hold_up_the_others() {
    let server = MockServer::start().await;
    let accounts = list(&[account(ACCOUNT, "ready"), account("broken", "ready")]);
    always(&server, "GET", "/v1/accounts", reply(200, &accounts)).await;
    on(
        &server,
        "GET",
        "/v1/messages",
        vec![
            reply(200, EMPTY),
            reply(200, fixture!("message_list_last")),
            reply(200, fixture!("message_list_last")),
        ],
    )
    .await;
    always(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/chats"),
        reply(200, fixture!("chat_list_last")),
    )
    .await;
    always(
        &server,
        "GET",
        "/v1/accounts/broken/chats",
        reply(
            503,
            r#"{"code":"engine_unavailable","message":"Retry shortly."}"#,
        ),
    )
    .await;
    let mut rig = rig(&server);
    rig.poll(Now::real()).await;
    // The poll with news goes through, with the messages and the chats
    // of the number that answers.
    let events = rig.poller.poll(Now::real()).await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, ProviderEvent::MessageUpserted(_))),
        "{events:?}"
    );
    // And the other number is asked again each time, until it answers.
    rig.poller.poll(Now::real()).await.unwrap();
    let broken = requests(&server)
        .await
        .iter()
        .filter(|request| request.url.path() == "/v1/accounts/broken/chats")
        .count();
    assert_eq!(broken, 3);
}

#[tokio::test]
async fn a_message_on_the_newest_page_is_followed_by_the_general_poll_for_free() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms - 10 * 60 * 1_000;
    // Sent ten minutes ago from the phone; delivered at the third poll.
    let page = |status: &str, changed: i64| {
        reply(200, &list(&[wire("m_phone", status, created, changed)]))
    };
    on(
        &server,
        "GET",
        "/v1/messages",
        vec![page("sent", 0), page("sent", 0), page("delivered", 590_000)],
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    let start = Now::real();
    // Priming says the account's own recent messages once: their ticks
    // may have moved while nobody was listening.
    assert_eq!(rig.poll(start).await, 1);
    assert_eq!(rig.source.following(), 1);
    for (n, expected) in [(1, 0), (2, 1)] {
        let now = start.plus(secs(4. * f64::from(n)));
        assert_eq!(rig.follow_up(now).await, 0);
        assert_eq!(rig.poll(now).await, expected);
    }
    assert_eq!(
        rig.engine
            .store()
            .message(&rig.account(), &MessageId::new("m_phone"))
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );
    assert_eq!(
        asked_by_id(&server, "m_phone").await,
        0,
        "no request of its own"
    );
}

#[tokio::test]
async fn history_that_is_read_puts_its_unsettled_messages_under_watch() {
    let server = MockServer::start().await;
    let created = Now::real().epoch_ms - 30 * 60 * 1_000;
    let page = list(&[
        wire("m_recent", "delivered", created, 1_000),
        wire("m_read", "read", created - 1_000, 5_000),
        wire("m_old", "sent", created - 7 * 60 * 60 * 1_000, 0),
    ]);
    Mock::given(method("GET"))
        .and(path("/v1/messages"))
        .and(query_param("chatId", CHAT))
        .respond_with(reply(200, &page))
        .mount(&server)
        .await;
    always(
        &server,
        "GET",
        "/v1/messages/m_recent",
        reply(
            200,
            &wire("m_recent", "read", created, 1_900_000).to_string(),
        ),
    )
    .await;
    quiet_api(&server).await;
    let mut rig = rig(&server);
    rig.poll(Now::real()).await;
    rig.engine
        .fetch_latest(&rig.account(), &rig.chat())
        .await
        .unwrap();
    // Only the one whose ticks can still move: not the read one, and not
    // the one from seven hours ago.
    assert_eq!(rig.source.following(), 1);
    let now = Now::real();
    assert_eq!(rig.follow_up(now.plus(secs(10.))).await, 0);
    assert_eq!(asked_by_id(&server, "m_recent").await, 0);
    // Half an hour old: asked about an eighth of that later.
    assert_eq!(rig.follow_up(now.plus(secs(240.))).await, 1);
    assert_eq!(asked_by_id(&server, "m_recent").await, 1);
    assert_eq!(
        rig.engine
            .store()
            .message(&rig.account(), &MessageId::new("m_recent"))
            .unwrap()
            .unwrap()
            .status,
        DeliveryStatus::Read
    );
}

// ----- the plan, without a network ------------------------------------------

fn now_at(base: Now, seconds: f64) -> Now {
    base.plus(secs(seconds))
}

#[test]
fn a_message_just_accepted_is_asked_about_at_doubling_intervals() {
    let points: Vec<f64> = follow::close_points()
        .iter()
        .map(Duration::as_secs_f64)
        .collect();
    assert_eq!(points, [0.5, 1.5, 3.5, 7.5, 15.5, 31.5, 61.5, 91.5]);
}

#[tokio::test]
async fn following_stays_within_its_share_of_the_rate_limit() {
    let base = Now::real();
    let mut follow = Follow::default();
    // Two hundred messages just sent, each to another chat.
    for n in 0..200 {
        let mut message = wire(&format!("m{n}"), "sent", base.epoch_ms, 0);
        message["chatId"] = format!("+58424{n:07}").into();
        follow.accept(&parsed(&message), base);
    }
    assert_eq!(follow.len(), 200);
    // Whoever drives it asks as fast as it is allowed to, for a minute.
    let mut asked = 0;
    let mut at = 0.0;
    while at < 60.0 {
        let plan = follow.plan(now_at(base, at).at);
        assert!(plan.len() <= follow::PER_STEP);
        asked += plan.len();
        at += 0.05;
    }
    assert_eq!(asked, follow::PER_MINUTE, "a fifth of the key's 600");
    // And it knows when it may ask again, instead of spinning.
    let wake = follow.next_wake(now_at(base, 60.0).at).unwrap();
    assert!(wake >= now_at(base, 60.0).at);
    assert!(!follow.plan(now_at(base, 61.0).at).is_empty());
}

#[tokio::test]
async fn a_message_is_watched_for_six_hours_and_only_while_its_ticks_can_move() {
    let base = Now::real();
    let mut follow = Follow::default();
    let sent = parsed(&wire("m1", "sent", base.epoch_ms, 0));
    follow.accept(&sent, base);
    // An inbound message, a reaction and a read one are not watched.
    let mut inbound = wire("m2", "received", base.epoch_ms, 0);
    inbound["direction"] = "inbound".into();
    assert!(!follow.observe(&parsed(&inbound), base));
    assert!(!follow.observe(&parsed(&wire("m3", "read", base.epoch_ms, 0)), base));
    assert_eq!(follow.len(), 1);

    // The same version again is not news; a new one is, once.
    assert!(!follow.observe(&sent, now_at(base, 1.0)));
    let delivered = parsed(&wire("m1", "delivered", base.epoch_ms, 2_000));
    assert!(follow.observe(&delivered, now_at(base, 2.0)));
    assert!(!follow.observe(&delivered, now_at(base, 3.0)));

    // Delivered and never read: asked about ever more rarely, never more
    // than every five minutes apart, and let go after six hours.
    let mut asked = 0;
    let mut second = 3.0;
    while second < 7.0 * 3_600.0 {
        for fetch in follow.plan(now_at(base, second).at) {
            assert_eq!(fetch, Fetch::One { id: "m1".into() });
            asked += 1;
        }
        second += 1.0;
    }
    assert!(!follow.watches("m1"));
    assert!((80..=110).contains(&asked), "{asked} requests in six hours");
    assert_eq!(follow.next_wake(now_at(base, second).at), None);
}
