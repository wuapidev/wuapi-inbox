//! Reading the chats the stream marked, against a mock API.

use super::{config, on, reply, requests, target, ACCOUNT, GROUP, KEY};
use crate::client::WuapiClient;
use crate::compat;
use crate::config::{ApiKey, StreamTuning, WuapiConfig};
use crate::envelope::{ChatReads, Dirty};
use crate::events::PollState;
use client_provider::{ProviderError, ProviderEvent};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use wiremock::MockServer;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

const CHAT: &str = "120363041234567890@g.us";
const CHAT_PATH: &str =
    "/v1/accounts/k57a8m2x9d3f0q1wjh6ypc4n2d7s0vbr/chats/120363041234567890%40g.us";

fn client(server: &MockServer) -> Arc<WuapiClient> {
    Arc::new(WuapiClient::new(&config(server), ApiKey::new(KEY)).unwrap())
}

/// Windows and waits short enough for the real clock.
fn quick() -> StreamTuning {
    StreamTuning {
        dirty_window: Duration::from_millis(10),
        dirty_gap: Duration::ZERO,
        dirty_retry: Duration::from_millis(40),
        ..StreamTuning::default()
    }
}

fn rig(server: &MockServer) -> (ChatReads, Arc<Mutex<PollState>>) {
    let state = Arc::new(Mutex::new(PollState::default()));
    (
        ChatReads::new(client(server), state.clone(), &quick()),
        state,
    )
}

fn mark_group(reads: &mut ChatReads) {
    reads.mark(Dirty::Chat {
        account: ACCOUNT.to_owned(),
        chat: GROUP.to_owned(),
    });
}

/// The unread counts of the `ChatUpdated` events.
fn unread(events: &[ProviderEvent]) -> Vec<u32> {
    events
        .iter()
        .map(|event| match event {
            ProviderEvent::ChatUpdated(chat) => chat.unread_count,
            other => panic!("{other:?}"),
        })
        .collect()
}

/// `chat.json` with its unread count changed.
fn chat_with_unread(count: i64) -> String {
    let mut value: serde_json::Value = serde_json::from_str(fixture!("chat")).unwrap();
    value["unreadCount"] = count.into();
    value.to_string()
}

#[tokio::test]
async fn compat_chat_request_parts() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        CHAT_PATH,
        vec![reply(200, fixture!("chat"))],
    )
    .await;
    let http = WuapiConfig {
        base_url: server.uri(),
        ..WuapiConfig::new("test-agent/1.0")
    };
    let client = WuapiClient::new(&http, ApiKey::new(KEY)).unwrap();
    let chat = compat::chat(client.sdk().http(), ACCOUNT, CHAT)
        .await
        .unwrap();
    assert_eq!(chat.0.id, CHAT);
    let seen = requests(&server).await;
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].method.as_str(), "GET");
    assert_eq!(target(&seen[0]), CHAT_PATH, "the id is encoded, no query");
    assert_eq!(
        seen[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap(),
        format!("Bearer {KEY}")
    );
}

#[tokio::test]
async fn client_chat_reads_one_chat() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        CHAT_PATH,
        vec![reply(200, fixture!("chat"))],
    )
    .await;
    let chat = client(&server).chat(ACCOUNT, CHAT).await.unwrap();
    assert_eq!(
        (chat.id.as_str(), chat.account_id.as_str()),
        (CHAT, ACCOUNT)
    );
    assert_eq!(chat.unread_count, Some(3));
    assert!(
        chat.last_message.is_some(),
        "the last message comes with it"
    );
}

#[tokio::test]
async fn chat_read_provider_wins_unread_4() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        CHAT_PATH,
        vec![
            reply(200, &chat_with_unread(1)),
            reply(200, &chat_with_unread(4)),
        ],
    )
    .await;
    let (mut reads, state) = rig(&server);
    // The event-derived view said 1.
    mark_group(&mut reads);
    assert_eq!(unread(&reads.next().await.unwrap()), [1]);
    // The provider says 4, and that is what the client is told.
    mark_group(&mut reads);
    assert_eq!(unread(&reads.next().await.unwrap()), [4]);
    drop(state);
}

#[tokio::test]
async fn chat_read_failure_retries_5s_3_times() {
    let server = MockServer::start().await;
    let down = || {
        reply(
            503,
            r#"{"code":"engine_unavailable","message":"Retry shortly."}"#,
        )
    };
    on(
        &server,
        "GET",
        CHAT_PATH,
        vec![down(), down(), down(), reply(200, &chat_with_unread(2))],
    )
    .await;
    let (mut reads, _) = rig(&server);
    mark_group(&mut reads);
    let started = Instant::now();
    assert_eq!(unread(&reads.next().await.unwrap()), [2]);
    assert!(
        started.elapsed() >= Duration::from_millis(120),
        "three waits of the retry delay: {:?}",
        started.elapsed()
    );
    assert_eq!(
        requests(&server).await.len(),
        4,
        "the first try and three retries"
    );

    // A fourth failure in a row is the last: the chat stays as it was.
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        CHAT_PATH,
        vec![down(), down(), down(), down()],
    )
    .await;
    let (mut reads, _) = rig(&server);
    mark_group(&mut reads);
    let waited = tokio::time::timeout(Duration::from_millis(600), reads.next()).await;
    assert!(waited.is_err(), "no event, and no end: {waited:?}");
    assert_eq!(requests(&server).await.len(), 4);
}

#[tokio::test]
async fn chat_429_pauses_for_retry_after() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        CHAT_PATH,
        vec![
            reply(429, r#"{"code":"rate_limited","message":"Slow down."}"#)
                .insert_header("retry-after", "1"),
            reply(200, &chat_with_unread(2)),
        ],
    )
    .await;
    let (mut reads, _) = rig(&server);
    mark_group(&mut reads);
    let started = Instant::now();
    assert_eq!(unread(&reads.next().await.unwrap()), [2]);
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "it waited what the API said: {:?}",
        started.elapsed()
    );
    assert_eq!(requests(&server).await.len(), 2);
}

#[tokio::test]
async fn chat_404_drops_mark() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        CHAT_PATH,
        vec![reply(
            404,
            r#"{"code":"not_found","message":"No such chat."}"#,
        )],
    )
    .await;
    let (mut reads, _) = rig(&server);
    mark_group(&mut reads);
    let waited = tokio::time::timeout(Duration::from_millis(400), reads.next()).await;
    assert!(waited.is_err(), "nothing to say, and not over: {waited:?}");
    assert_eq!(
        requests(&server).await.len(),
        1,
        "and it was not asked again"
    );
}

#[tokio::test]
async fn chat_401_ends_stream() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        CHAT_PATH,
        vec![reply(401, fixture!("error_unauthorized"))],
    )
    .await;
    let (mut reads, _) = rig(&server);
    mark_group(&mut reads);
    let error = reads.next().await.unwrap_err();
    assert!(matches!(error, ProviderError::Unauthorized(_)), "{error:?}");
}

#[tokio::test]
async fn account_page_read_updates_the_accounts_chats() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/chats"),
        vec![reply(200, fixture!("chat_list"))],
    )
    .await;
    let (mut reads, _) = rig(&server);
    reads.mark(Dirty::Account(ACCOUNT.to_owned()));
    let events = reads.next().await.unwrap();
    assert!(!events.is_empty());
    assert!(events
        .iter()
        .all(|e| matches!(e, ProviderEvent::ChatUpdated(_))));
    let seen = requests(&server).await;
    assert_eq!(
        target(&seen[0]),
        format!("/v1/accounts/{ACCOUNT}/chats?limit=50")
    );
}
