//! Tests against JSON fixtures taken from `openapi.json`. Nothing here
//! touches the network: the calls that go through the wuapi SDK are
//! answered by a mock API on localhost, and the hand-written login talks
//! to a fake transport that records requests and replays canned answers.

use crate::config::{
    check_stream_url, ApiKey, LiveTransport, WuapiConfig, DEFAULT_BASE_URL, DEFAULT_STREAM_URL,
};
use crate::error;
use crate::events::PollState;
use crate::http::{self, ApiRequest, ApiResponse, Method, NetworkError, Transport};
use crate::identity::AuthContext;
use crate::login::{DeviceLogin, LoginError, PollOutcome};
use crate::mapping;
use crate::provider::WuapiProvider;
use async_trait::async_trait;
use client_provider::{
    AccountId, ChatChange, ChatId, ChatKind, ClientMessageId, ConnectionState, DeliveryStatus,
    Direction, MediaKind, MediaRef, MessageContent, MessageId, OutgoingContent, OutgoingMessage,
    Provider, ProviderError, ProviderEvent,
};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Respond, ResponseTemplate};
use wuapi::types as api;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../tests/fixtures/", $name, ".json"))
    };
}

/// Parses a fixture into the SDK's type. Every fixture is an example from
/// the spec, so this also checks the generated types against it.
fn parse<T: serde::de::DeserializeOwned>(json: &str) -> T {
    serde_json::from_str(json).expect("the fixture matches the SDK's type")
}

const ACCOUNT: &str = "k57a8m2x9d3f0q1wjh6ypc4n2d7s0vbr";
const GROUP: &str = "120363041234567890@g.us";
const KEY: &str = "wu_live_0123456789abcdef0123456789abcdef0123456789abcdef";

// ----- the mock API -------------------------------------------------------

/// Answers one endpoint with the queued replies, in order. A request
/// beyond the last one gets a 599, which no test expects.
struct Replies(Mutex<VecDeque<ResponseTemplate>>);

impl Respond for Replies {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| ResponseTemplate::new(599))
    }
}

/// A reply with a status and a body, JSON unless the test says otherwise.
fn reply(status: u16, body: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_raw(body.as_bytes().to_vec(), "application/json")
}

/// Queues `replies` for `verb path` on the mock API.
async fn on(server: &MockServer, verb: &str, at: &str, replies: Vec<ResponseTemplate>) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(Replies(Mutex::new(replies.into())))
        .mount(server)
        .await;
}

/// What the mock API was asked, in order.
async fn requests(server: &MockServer) -> Vec<wiremock::Request> {
    server.received_requests().await.unwrap_or_default()
}

/// Path and query of a request, as they travelled.
fn target(request: &wiremock::Request) -> String {
    match request.url.query() {
        Some(query) => format!("{}?{query}", request.url.path()),
        None => request.url.path().to_owned(),
    }
}

fn header<'a>(request: &'a wiremock::Request, name: &str) -> Option<&'a str> {
    request.headers.get(name).and_then(|v| v.to_str().ok())
}

fn config(server: &MockServer) -> WuapiConfig {
    WuapiConfig {
        base_url: server.uri(),
        // Polling tests run on the real clock.
        poll_interval: Duration::from_millis(20),
        // These tests drive the poller; the live transport is its own suite.
        live: LiveTransport::Polling,
        ..WuapiConfig::new("test-agent/1.0")
    }
}

fn provider(server: &MockServer) -> WuapiProvider {
    WuapiProvider::new(config(server), ApiKey::new(KEY)).unwrap()
}

// ----- mapping ------------------------------------------------------------

#[test]
fn maps_the_spec_example_message() {
    let wire: api::Message = parse(fixture!("message"));
    let message = mapping::message(&wire).unwrap();
    assert_eq!(message.id.as_str(), "m17d0a9w2sqc7k3v8x1n5ybr6t4hjp2e");
    assert_eq!(message.account_id.as_str(), ACCOUNT);
    assert_eq!(message.chat_id.as_str(), "+584245550199");
    assert_eq!(message.sender.as_str(), "+584121234567");
    assert_eq!(message.direction, Direction::Outgoing);
    assert_eq!(
        message.content,
        MessageContent::text("Your order has shipped.")
    );
    assert_eq!(
        message.status,
        DeliveryStatus::Accepted,
        "queued: wuapi has it, WhatsApp not yet"
    );
    assert_eq!(
        message.client_id, None,
        "foreign metadata is not a client id"
    );
    assert_eq!(message.reply_to, None);
    // No sentAt yet: createdAt, 2026-09-24T08:15:40Z.
    assert_eq!(message.timestamp.as_millis(), 1_790_237_740_000);
    assert!(!message.edited && !message.deleted);
}

#[test]
fn maps_client_id_reply_and_sent_time() {
    let wire: api::Message = parse(fixture!("message_sent_by_client"));
    let message = mapping::message(&wire).unwrap();
    assert_eq!(message.client_id, Some(ClientMessageId::new("c0ffee")));
    assert_eq!(message.status, DeliveryStatus::Delivered);
    let reply = message.reply_to.unwrap();
    assert_eq!(reply.message_id.as_str(), "m_in_1");
    assert_eq!(reply.preview, None);
    // sentAt wins over createdAt, to the millisecond.
    assert_eq!(message.timestamp.as_millis(), 1_790_237_741_500);
}

#[test]
fn maps_inbound_group_media() {
    let wire: api::Message = parse(fixture!("message_group_image"));
    let message = mapping::message(&wire).unwrap();
    assert_eq!(message.chat_id.as_str(), GROUP);
    assert_eq!(message.direction, Direction::Incoming);
    assert_eq!(message.sender_name.as_deref(), Some("Maria"));
    assert_eq!(message.status, DeliveryStatus::Delivered, "received");
    let MessageContent::Media(media) = message.content else {
        panic!("an image is media");
    };
    assert_eq!(media.kind, MediaKind::Image);
    assert_eq!(media.caption.as_deref(), Some("Launch day"));
    assert_eq!(media.mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(
        media.source,
        Some(MediaRef::new("https://api.wuapi.dev/v1/media/abc123"))
    );
}

#[test]
fn maps_reactions_failures_and_unmodelled_types() {
    let reaction = mapping::message(&parse(fixture!("message_reaction"))).unwrap();
    assert_eq!(
        reaction.content,
        MessageContent::Reaction {
            target: MessageId::new("m17d0a9w2sqc7k3v8x1n5ybr6t4hjp2e"),
            emoji: "👍".into()
        }
    );
    assert_eq!(reaction.reply_to, None, "the target is not a quote");

    // An emptied reaction is a removal; one without a target is dropped.
    let mut removed: api::Message = parse(fixture!("message_reaction"));
    removed.text = None;
    assert!(matches!(
        mapping::message(&removed).unwrap().content,
        MessageContent::Reaction { emoji, .. } if emoji.is_empty()
    ));
    removed.reply_to_message_id = None;
    assert!(mapping::message(&removed).is_none());

    let failed = mapping::message(&parse(fixture!("message_failed"))).unwrap();
    assert_eq!(
        failed.status,
        DeliveryStatus::Failed {
            reason: "The recipient has no WhatsApp.".into()
        }
    );

    // A type the API itself calls unknown is kept, by that name.
    assert_eq!(
        mapping::message(&parse(fixture!("message_unknown")))
            .unwrap()
            .content,
        MessageContent::Unsupported {
            description: "unknown".into()
        }
    );
}

#[test]
fn skips_stories_and_channels() {
    let list: api::MessageList = parse(fixture!("message_list"));
    let story = list
        .items
        .iter()
        .find(|m| m.chat_type == api::ChatType::Story)
        .unwrap();
    assert!(mapping::message(story).is_none());
    let mut channel = story.clone();
    channel.chat_type = "channel".into();
    assert!(mapping::message(&channel).is_none());
    // A chat type newer than the SDK is skipped too, not misfiled.
    channel.chat_type = "broadcast".into();
    assert!(mapping::message(&channel).is_none());
}

#[test]
fn maps_accounts_and_their_connection_states() {
    let wire: api::Account = parse(fixture!("account"));
    let account = mapping::account(&wire);
    assert_eq!(account.id.as_str(), ACCOUNT);
    assert_eq!(account.display_name, "Support line");
    assert_eq!(account.phone.as_deref(), Some("+584121234567"));
    assert_eq!(account.self_contact.unwrap().as_str(), "+584121234567");
    assert_eq!(account.connection, ConnectionState::Connected);

    let with = |status: &str, reconnecting: bool, reason: Option<&str>| {
        let mut w = wire.clone();
        w.status = status.into();
        w.reconnecting = reconnecting;
        w.disconnect_reason = reason.map(str::to_owned);
        mapping::connection(&w)
    };
    assert_eq!(with("qr_ready", false, None), ConnectionState::Connecting);
    assert_eq!(
        with("authenticating", false, None),
        ConnectionState::Connecting
    );
    // A drop wuapi is recovering from is not an error, whatever the status.
    assert_eq!(
        with("disconnected", true, None),
        ConnectionState::Reconnecting
    );
    assert_eq!(
        with("initializing", true, None),
        ConnectionState::Reconnecting
    );
    assert_eq!(
        with("disconnected", false, Some("logged_out")),
        ConnectionState::LoggedOut
    );
    assert_eq!(
        with("failed", false, Some("proxy_paused")),
        ConnectionState::Disconnected {
            reason: Some("proxy_paused".into())
        }
    );

    // Without a label, the profile name is the display name.
    let list: api::AccountList = parse(fixture!("account_list_last"));
    assert_eq!(mapping::account(&list.items[0]).display_name, "Second");
}

#[test]
fn values_newer_than_the_sdk_get_the_least_committal_mapping() {
    let mut wire: api::Message = parse(fixture!("message_inbound"));
    wire.status = "snoozed".into();
    wire.r#type = "hologram".into();
    let message = mapping::message(&wire).unwrap();
    assert_eq!(message.status, DeliveryStatus::Delivered, "it is inbound");
    assert_eq!(
        message.content,
        MessageContent::Unsupported {
            description: "hologram".into()
        },
        "a type newer than the SDK is named, not guessed"
    );
    wire.direction = api::MessageDirection::Outbound;
    assert_eq!(
        mapping::message(&wire).unwrap().status,
        DeliveryStatus::Accepted,
        "whatever the status is, the API has the message"
    );

    let mut account: api::Account = parse(fixture!("account"));
    account.status = "hibernating".into();
    assert_eq!(
        mapping::connection(&account),
        ConnectionState::Disconnected { reason: None }
    );
}

#[test]
fn decodes_the_remaining_spec_examples() {
    let me: AuthContext = parse::<api::AuthContext>(fixture!("auth_context")).into();
    assert_eq!(me.organization.name, "Acme");
    assert_eq!(me.api_key.key_prefix, "wu_live_9c9c");
    assert_eq!(me.project.unwrap().name, "Northwind Dental");
    let group: api::Group = parse(fixture!("group"));
    assert_eq!(
        (group.id.as_str(), group.name.as_str()),
        (GROUP, "Launch team")
    );
}

// ----- errors -----------------------------------------------------------

#[test]
fn api_answers_become_the_right_errors() {
    let error = |status: u16, body: &str, retry_after: Option<u64>| {
        error::from_response(ApiResponse {
            status,
            retry_after,
            content_type: None,
            body: body.as_bytes().to_vec(),
        })
    };

    assert!(matches!(error(500, "", None), ProviderError::Transient(_)));
    assert!(matches!(
        error(503, "<html>", None),
        ProviderError::Transient(_)
    ));
    assert!(matches!(error(408, "", None), ProviderError::Transient(_)));
    assert!(matches!(
        error(429, "{}", Some(7)),
        ProviderError::RateLimited { retry_after: Some(d) } if d == Duration::from_secs(7)
    ));
    assert!(matches!(
        error(401, fixture!("error_unauthorized"), None),
        ProviderError::Unauthorized(_)
    ));
    // The account is reconnecting: wait, do not fail the message.
    assert!(error(409, fixture!("error_not_ready"), None).is_transient());
    for code in ["idempotency_conflict", "state_resyncing", "message_sending"] {
        let body = format!(r#"{{"code":"{code}","message":"Not now."}}"#);
        assert!(error(409, &body, None).is_transient(), "{code}");
    }
    match error(400, fixture!("error_invalid"), None) {
        ProviderError::Rejected { code, message } => {
            assert_eq!(code, "invalid_request");
            assert_eq!(message, "text must not be blank.");
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    for status in [402, 403, 404, 409, 422] {
        assert!(!error(status, r#"{"code":"x","message":"y"}"#, None).is_transient());
    }
    // The waiting codes only mean waiting on a 409.
    assert!(!error(400, r#"{"code":"account_not_ready","message":"y"}"#, None).is_transient());

    let network: ProviderError = NetworkError("connection reset".into()).into();
    assert!(network.is_transient());
}

#[test]
fn sdk_errors_that_never_reached_the_api_are_classified() {
    assert!(error::from_sdk(wuapi::Error::Timeout {
        timeout: Duration::from_secs(20)
    })
    .is_transient());
    assert!(matches!(
        error::from_sdk(wuapi::Error::Encode("a map with a non-string key".into())),
        ProviderError::Protocol(_)
    ));
    assert!(matches!(
        error::from_sdk(wuapi::Error::Config("bad project".into())),
        ProviderError::Protocol(_)
    ));
    // No key is a reason to sign in, and it is known before any request.
    assert!(matches!(
        WuapiProvider::new(WuapiConfig::new("test-agent/1.0"), ApiKey::new("  ")),
        Err(ProviderError::Unauthorized(_))
    ));
}

/// The whole path: the mock API answers, the SDK turns the answer into its
/// error enum, and the adapter maps that to a `ProviderError`.
#[tokio::test]
async fn sdk_errors_from_real_answers_are_classified() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/v1/me",
        vec![
            reply(500, r#"{"code":"internal_error","message":"Oops."}"#),
            ResponseTemplate::new(503).set_body_string("<html>bad gateway</html>"),
            reply(429, r#"{"code":"rate_limited","message":"Slow down."}"#)
                .insert_header("retry-after", "7"),
            reply(401, fixture!("error_unauthorized")),
            reply(409, fixture!("error_not_ready")),
            reply(400, fixture!("error_invalid")),
            reply(404, r#"{"code":"not_found","message":"No such route."}"#),
            reply(200, "not json"),
            reply(200, r#"{"object":"auth_context"}"#),
        ],
    )
    .await;
    let provider = provider(&server);
    let mut outcomes = Vec::new();
    for _ in 0..9 {
        outcomes.push(provider.me().await.unwrap_err());
    }

    assert!(matches!(&outcomes[0], ProviderError::Transient(m) if m.contains("internal_error")));
    assert!(matches!(&outcomes[1], ProviderError::Transient(m) if m.contains("http_503")));
    assert!(matches!(
        &outcomes[2],
        ProviderError::RateLimited { retry_after: Some(d) } if *d == Duration::from_secs(7)
    ));
    assert!(matches!(&outcomes[3], ProviderError::Unauthorized(_)));
    assert!(matches!(&outcomes[4], ProviderError::Transient(m) if m.contains("account_not_ready")));
    assert!(matches!(
        &outcomes[5],
        ProviderError::Rejected { code, message }
            if code == "invalid_request" && message == "text must not be blank."
    ));
    assert!(matches!(&outcomes[6], ProviderError::Rejected { code, .. } if code == "not_found"));
    // A 2xx that is not what the spec promises.
    assert!(matches!(&outcomes[7], ProviderError::Protocol(_)));
    assert!(matches!(&outcomes[8], ProviderError::Protocol(_)));

    // One request per call: retrying is the engine's job, not the SDK's.
    assert_eq!(requests(&server).await.len(), 9);
}

#[tokio::test]
async fn timeouts_and_dropped_connections_are_transient() {
    // Slower than the deadline.
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/v1/me",
        vec![reply(200, fixture!("auth_context")).set_delay(Duration::from_secs(5))],
    )
    .await;
    let slow = WuapiProvider::new(
        WuapiConfig {
            request_timeout: Duration::from_millis(150),
            ..config(&server)
        },
        ApiKey::new(KEY),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let error = slow.me().await.unwrap_err();
    assert!(matches!(&error, ProviderError::Transient(_)), "{error:?}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "the call is bounded"
    );

    // Nobody listening: the port was free a moment ago.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let unreachable = WuapiProvider::new(
        WuapiConfig {
            base_url: format!("http://127.0.0.1:{port}"),
            ..WuapiConfig::new("test-agent/1.0")
        },
        ApiKey::new(KEY),
    )
    .unwrap();
    let error = unreachable.list_accounts().await.unwrap_err();
    assert!(matches!(&error, ProviderError::Transient(_)), "{error:?}");
    // The message names no URL: query strings carry phone numbers.
    assert!(!error.to_string().contains("127.0.0.1"), "{error}");
}

#[test]
fn origins_compare_safely() {
    assert_eq!(
        http::origin("https://API.wuapi.dev/v1/media/1?x=1").as_deref(),
        Some("https://api.wuapi.dev")
    );
    assert_ne!(
        http::origin("https://api.wuapi.dev.evil.example/x"),
        http::origin("https://api.wuapi.dev")
    );
    assert_eq!(http::origin("https://api.wuapi.dev@evil.example/x"), None);
}

#[test]
fn the_api_key_is_never_printed() {
    let key = ApiKey::new(KEY);
    let request = ApiRequest::get("https://api.wuapi.dev/v1/me").bearer(&key);
    let printed = format!("{key:?} {request:?}");
    assert!(printed.contains("wu_live_0123"));
    assert!(!printed.contains(&KEY[12..]));
}

// ----- requests -----------------------------------------------------------

#[tokio::test]
async fn sends_text_with_an_idempotency_key_and_echoed_client_id() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/v1/messages",
        vec![
            reply(202, fixture!("message")),
            reply(202, fixture!("message")),
        ],
    )
    .await;
    let provider = provider(&server);
    let outgoing = OutgoingMessage {
        client_id: ClientMessageId::new("c0ffee"),
        account_id: AccountId::new(ACCOUNT),
        chat_id: ChatId::new("+584245550199"),
        content: OutgoingContent::Text {
            body: "Your order has shipped.".into(),
        },
        reply_to: Some(MessageId::new("m_in_1")),
        mentions: Vec::new(),
        forwarded: false,
    };

    let receipt = provider.send(outgoing.clone()).await.unwrap();
    assert_eq!(
        receipt.message_id.as_str(),
        "m17d0a9w2sqc7k3v8x1n5ybr6t4hjp2e"
    );
    assert_eq!(receipt.status, DeliveryStatus::Accepted);
    // A retry is the same request with the same key, so wuapi replays its
    // first answer instead of sending again.
    let retry = provider.send(outgoing).await.unwrap();
    assert_eq!(retry, receipt);

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(target(request), "/v1/messages");
        assert_eq!(header(request, "idempotency-key"), Some("c0ffee"));
        assert_eq!(
            header(request, "authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            serde_json::json!({
                "accountId": ACCOUNT,
                "to": "+584245550199",
                "type": "text",
                "text": "Your order has shipped.",
                "replyToMessageId": "m_in_1",
                "metadata": { "clientMessageId": "c0ffee" },
            })
        );
    }
    assert_eq!(requests[0].body, requests[1].body);
}

#[tokio::test]
async fn a_send_that_fails_keeps_its_classification() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/v1/messages",
        vec![
            reply(
                502,
                r#"{"code":"whatsapp_error","message":"WhatsApp failed."}"#,
            ),
            reply(409, fixture!("error_not_ready")),
            reply(400, fixture!("error_invalid")),
        ],
    )
    .await;
    let provider = provider(&server);
    let outgoing = OutgoingMessage {
        client_id: ClientMessageId::new("c1"),
        account_id: AccountId::new(ACCOUNT),
        chat_id: ChatId::new("+584245550199"),
        content: OutgoingContent::Text { body: "hi".into() },
        reply_to: None,
        mentions: Vec::new(),
        forwarded: false,
    };
    assert!(provider
        .send(outgoing.clone())
        .await
        .unwrap_err()
        .is_transient());
    assert!(provider
        .send(outgoing.clone())
        .await
        .unwrap_err()
        .is_transient());
    assert!(!provider
        .send(outgoing.clone())
        .await
        .unwrap_err()
        .is_transient());

    // A file that was not uploaded first is refused before any request.
    let media = OutgoingMessage {
        content: OutgoingContent::Media {
            kind: MediaKind::Image,
            media: MediaRef::new("local:c1"),
            mime_type: None,
            caption: None,
            file_name: None,
            gif: false,
        },
        ..outgoing
    };
    assert!(matches!(
        provider.send(media).await,
        Err(ProviderError::Rejected { code, .. }) if code == "invalid_media"
    ));
    // Three attempts, three requests, every one with the client's key: the
    // SDK did not retry behind the engine's back.
    let requests = requests(&server).await;
    assert_eq!(requests.len(), 3);
    assert!(requests
        .iter()
        .all(|r| header(r, "idempotency-key") == Some("c1")));
    assert!(provider.capabilities().media_upload);
}

#[tokio::test]
async fn reactions_use_the_react_endpoint() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/v1/messages/m%2F1/react",
        vec![ResponseTemplate::new(204), ResponseTemplate::new(204)],
    )
    .await;
    let provider = provider(&server);
    let outgoing = OutgoingMessage {
        client_id: ClientMessageId::new("r1"),
        account_id: AccountId::new(ACCOUNT),
        chat_id: ChatId::new(GROUP),
        content: OutgoingContent::Reaction {
            target: MessageId::new("m/1"),
            emoji: "❤️".into(),
        },
        reply_to: None,
        mentions: Vec::new(),
        forwarded: false,
    };
    let first = provider.send(outgoing.clone()).await.unwrap();
    let second = provider.send(outgoing).await.unwrap();
    assert_eq!(first.message_id, second.message_id);

    let request = &requests(&server).await[0];
    assert_eq!(target(request), "/v1/messages/m%2F1/react");
    assert_eq!(header(request, "idempotency-key"), Some("r1"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
        serde_json::json!({ "emoji": "❤️" })
    );
}

#[tokio::test]
async fn fetches_history_newest_first_with_cursor_paging() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/v1/messages",
        vec![
            reply(200, fixture!("message_list")),
            reply(200, fixture!("message_list_last")),
        ],
    )
    .await;
    let provider = provider(&server);
    let account = AccountId::new(ACCOUNT);
    let chat = ChatId::new("+584245550199");

    let first = provider
        .fetch_messages(&account, &chat, None, 500)
        .await
        .unwrap();
    // Six on the wire, the story is dropped.
    assert_eq!(first.items.len(), 5);
    let cursor = first.next_cursor.expect("there is another page");
    assert_eq!(cursor.as_str(), "cur_2");
    let second = provider
        .fetch_messages(&account, &chat, Some(cursor), 0)
        .await
        .unwrap();
    assert_eq!(second.items.len(), 2);
    assert_eq!(second.next_cursor, None);

    let requests = requests(&server).await;
    assert_eq!(
        target(&requests[0]),
        format!("/v1/messages?accountId={ACCOUNT}&chatId=%2B584245550199&limit=100")
    );
    assert_eq!(
        target(&requests[1]),
        format!("/v1/messages?accountId={ACCOUNT}&chatId=%2B584245550199&limit=1&cursor=cur_2")
    );
}

#[tokio::test]
async fn lists_accounts_across_pages() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/v1/accounts",
        vec![
            reply(200, fixture!("account_list")),
            reply(200, fixture!("account_list_last")),
        ],
    )
    .await;
    let accounts = provider(&server).list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].id.as_str(), ACCOUNT);
    assert_eq!(accounts[0].connection, ConnectionState::Connected);
    assert_eq!(accounts[1].connection, ConnectionState::LoggedOut);
    let requests = requests(&server).await;
    assert_eq!(target(&requests[0]), "/v1/accounts?limit=100");
    assert_eq!(
        target(&requests[1]),
        "/v1/accounts?limit=100&cursor=acc_cur"
    );
    assert_eq!(requests.len(), 2);
}

#[tokio::test]
async fn a_page_that_fails_fails_the_listing() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/v1/accounts",
        vec![
            reply(200, fixture!("account_list")),
            reply(
                503,
                r#"{"code":"engine_unavailable","message":"Retry shortly."}"#,
            ),
        ],
    )
    .await;
    // Half a list would look like accounts that disappeared.
    let error = provider(&server).list_accounts().await.unwrap_err();
    assert!(error.is_transient(), "{error:?}");
}

#[tokio::test]
async fn mark_read_sends_receipts_then_clears_the_badge() {
    let server = MockServer::start().await;
    let chat = format!("/v1/accounts/{ACCOUNT}/chats/120363041234567890%40g.us");
    on(
        &server,
        "POST",
        &format!("{chat}/read"),
        vec![reply(200, fixture!("chat_read"))],
    )
    .await;
    on(
        &server,
        "POST",
        &format!("{chat}/mark-read"),
        vec![
            ResponseTemplate::new(204),
            // Right after linking, chat state is still syncing.
            reply(409, r#"{"code":"state_resyncing","message":"Resyncing."}"#)
                .insert_header("retry-after", "3"),
        ],
    )
    .await;
    let provider = provider(&server);
    let (account, group) = (AccountId::new(ACCOUNT), ChatId::new(GROUP));
    provider.mark_read(&account, &group, None).await.unwrap();
    let targets: Vec<_> = requests(&server).await.iter().map(target).collect();
    assert_eq!(
        targets,
        [format!("{chat}/read"), format!("{chat}/mark-read")]
    );

    on(
        &server,
        "POST",
        &format!("{chat}/read"),
        vec![reply(200, fixture!("chat_read"))],
    )
    .await;
    let error = provider
        .mark_read(&account, &group, None)
        .await
        .unwrap_err();
    assert!(error.is_transient(), "worth trying again: {error:?}");
}

#[tokio::test]
async fn chat_changes_call_their_endpoints_and_say_whether_to_retry() {
    let server = MockServer::start().await;
    let chat = format!("/v1/accounts/{ACCOUNT}/chats/120363041234567890%40g.us");
    for action in [
        "pin",
        "unpin",
        "mute",
        "unmute",
        "archive",
        "unarchive",
        "mark-unread",
    ] {
        on(
            &server,
            "POST",
            &format!("{chat}/{action}"),
            vec![
                ResponseTemplate::new(204),
                reply(409, r#"{"code":"state_resyncing","message":"Resyncing."}"#),
                reply(403, r#"{"code":"forbidden","message":"Not yours."}"#),
            ],
        )
        .await;
    }
    let provider = provider(&server);
    assert!(provider.capabilities().chat_state);
    let (account, group) = (AccountId::new(ACCOUNT), ChatId::new(GROUP));
    for change in [
        ChatChange::Pinned(true),
        ChatChange::Pinned(false),
        ChatChange::Muted(true),
        ChatChange::Muted(false),
        ChatChange::Archived(true),
        ChatChange::Archived(false),
        ChatChange::MarkedUnread,
    ] {
        provider
            .update_chat(&account, &group, change)
            .await
            .unwrap();
    }
    let asked = requests(&server).await;
    let targets: Vec<_> = asked.iter().map(target).collect();
    assert_eq!(
        targets,
        [
            format!("{chat}/pin"),
            format!("{chat}/unpin"),
            format!("{chat}/mute"),
            format!("{chat}/unmute"),
            format!("{chat}/archive"),
            format!("{chat}/unarchive"),
            format!("{chat}/mark-unread"),
        ]
    );
    // Muted until unmuted: no duration is sent.
    assert_eq!(asked[2].body, b"{}");

    // Chat state still syncing with the phone: worth repeating. A refusal
    // is not.
    let again = provider.update_chat(&account, &group, ChatChange::Pinned(true));
    assert!(again.await.unwrap_err().is_transient());
    assert!(matches!(
        provider.update_chat(&account, &group, ChatChange::Pinned(true)).await,
        Err(ProviderError::Rejected { code, .. }) if code == "forbidden"
    ));
}

#[tokio::test]
async fn a_chat_with_a_number_starts_without_asking_the_api() {
    let server = MockServer::start().await;
    let provider = provider(&server);
    assert!(provider.capabilities().start_chat);
    let account = AccountId::new(ACCOUNT);

    for typed in ["+58 424 555 0199", "0058 (424) 555-0199", "58.424.555.0199"] {
        let chat = provider.start_chat(&account, typed).await.unwrap();
        assert_eq!(chat.id.as_str(), "+584245550199", "from {typed:?}");
        assert_eq!(chat.kind, ChatKind::Direct);
        assert_eq!(chat.title, "+584245550199");
        assert_eq!(chat.avatar, Some(MediaRef::new("picture:+584245550199")));
    }
    for typed in [
        "",
        "maria",
        "12345",
        "0424 555 0199",
        "+58 424 555 0199 ext 4",
    ] {
        assert!(
            matches!(
                provider.start_chat(&account, typed).await,
                Err(ProviderError::Rejected { code, .. }) if code == "invalid_phone"
            ),
            "{typed:?} is not a full number"
        );
    }
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn me_reads_the_auth_context() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        "/v1/me",
        vec![
            reply(200, fixture!("auth_context")),
            reply(401, fixture!("error_unauthorized")),
        ],
    )
    .await;
    let provider = provider(&server);
    let me = provider.me().await.unwrap();
    assert_eq!(me.organization.name, "Acme");
    assert_eq!(me.api_key.key_prefix, "wu_live_9c9c");
    assert!(matches!(
        provider.me().await,
        Err(ProviderError::Unauthorized(_))
    ));
    assert_eq!(target(&requests(&server).await[0]), "/v1/me");
}

#[tokio::test]
async fn media_downloads_only_share_the_key_with_the_api() {
    let server = MockServer::start().await;
    // Another origin: WhatsApp's CDN, say.
    let elsewhere = MockServer::start().await;
    on(
        &server,
        "GET",
        "/v1/media/abc123",
        vec![ResponseTemplate::new(200).set_body_raw(b"own".to_vec(), "image/jpeg")],
    )
    .await;
    let picture = format!(
        r#"{{"object":"picture","id":"1727164540","url":"{}/v/t61/example.jpg","preview":true}}"#,
        elsewhere.uri()
    );
    on(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/contacts/%2B584245550199/picture"),
        vec![
            reply(200, &picture),
            reply(
                404,
                r#"{"code":"picture_not_found","message":"No picture."}"#,
            ),
        ],
    )
    .await;
    on(
        &elsewhere,
        "GET",
        "/d/f/abc.enc",
        vec![
            ResponseTemplate::new(200).set_body_string("foreign"),
            ResponseTemplate::new(503),
        ],
    )
    .await;
    on(
        &elsewhere,
        "GET",
        "/v/t61/example.jpg",
        vec![ResponseTemplate::new(200).set_body_string("jpeg")],
    )
    .await;
    let provider = provider(&server);
    let account = AccountId::new(ACCOUNT);

    let own = MediaRef::new(format!("{}/v1/media/abc123", server.uri()));
    let data = provider.download_media(&account, &own).await.unwrap();
    assert_eq!(data.bytes, b"own");
    assert_eq!(data.mime_type.as_deref(), Some("image/jpeg"));
    let foreign = MediaRef::new(format!("{}/d/f/abc.enc", elsewhere.uri()));
    provider.download_media(&account, &foreign).await.unwrap();
    // A profile picture is resolved to its URL first.
    let picture = MediaRef::new("picture:+584245550199");
    assert_eq!(
        provider
            .download_media(&account, &picture)
            .await
            .unwrap()
            .bytes,
        b"jpeg"
    );

    let own_requests = requests(&server).await;
    assert_eq!(target(&own_requests[0]), "/v1/media/abc123");
    assert!(header(&own_requests[0], "authorization").is_some());
    assert_eq!(
        header(&own_requests[0], "user-agent"),
        Some("test-agent/1.0")
    );
    assert_eq!(
        target(&own_requests[1]),
        format!("/v1/accounts/{ACCOUNT}/contacts/%2B584245550199/picture?preview=true")
    );
    let foreign_requests = requests(&elsewhere).await;
    assert_eq!(foreign_requests.len(), 2);
    assert!(
        foreign_requests
            .iter()
            .all(|r| header(r, "authorization").is_none()),
        "third parties never get the key"
    );

    // A download that fails says whether trying again can help.
    assert!(provider
        .download_media(&account, &foreign)
        .await
        .unwrap_err()
        .is_transient());
    assert!(matches!(
        provider.download_media(&account, &picture).await,
        Err(ProviderError::Rejected { code, .. }) if code == "picture_not_found"
    ));
    assert!(provider
        .download_media(&account, &MediaRef::new("file:///etc/passwd"))
        .await
        .is_err());
}

// ----- the chat list ------------------------------------------------------

#[test]
fn maps_chats_and_treats_unobserved_state_as_quiet() {
    let list: api::ChatList = parse(fixture!("chat_list"));
    let last: api::ChatList = parse(fixture!("chat_list_last"));

    let group = mapping::chat(&list.items[0]).unwrap();
    assert_eq!(group.kind, ChatKind::Group);
    assert_eq!(group.title, "Launch team");
    assert_eq!(group.avatar, None);
    assert_eq!(group.unread_count, 3);
    assert!(group.pinned && group.muted && !group.archived);
    assert_eq!(group.last_message.unwrap().id.as_str(), "m_group_image");

    // The saved name wins over the profile name, and a chat marked as
    // unread (unread, count 0) still shows a badge.
    let direct = mapping::chat(&list.items[1]).unwrap();
    assert_eq!(direct.kind, ChatKind::Direct);
    assert_eq!(direct.title, "Maria Fernanda");
    assert_eq!(direct.avatar, Some(MediaRef::new("picture:+584245550199")));
    assert_eq!(direct.unread_count, 1);
    assert!(!direct.pinned && !direct.muted && !direct.archived);

    // Channels are not conversations the client shows.
    assert_eq!(mapping::chat(&list.items[2]), None);

    // What was observed is known; `muteExpiresAt` aside, nothing is unknown.
    assert_eq!(group.unknown, client_provider::ChatUnknown::default());

    // Nothing observed yet: every state is null. It reads as "not", and is
    // marked unknown so the client keeps what it has (a pin made there).
    let unknown = mapping::chat(&last.items[0]).unwrap();
    assert!(unknown.unknown.pinned && unknown.unknown.muted && unknown.unknown.archived);
    assert_eq!(unknown.title, "+584149998877", "no name: the number");
    assert_eq!(unknown.unread_count, 0);
    assert!(!unknown.pinned && !unknown.muted && !unknown.archived);
    assert_eq!(unknown.last_message, None);

    let archived = mapping::chat(&last.items[1]).unwrap();
    assert_eq!(archived.title, "Pedro");
    assert!(archived.archived && !archived.pinned && !archived.muted);
    assert!(!archived.unknown.archived && archived.unknown.pinned);

    // A count without the flag still counts; a negative one does not.
    let mut odd = list.items[0].clone();
    odd.unread = None;
    odd.unread_count = Some(7);
    assert_eq!(mapping::chat(&odd).unwrap().unread_count, 7);
    odd.unread_count = Some(-1);
    assert_eq!(mapping::chat(&odd).unwrap().unread_count, 0);
}

#[tokio::test]
async fn list_chats_reads_the_chat_list_and_follows_the_cursor() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/chats"),
        vec![
            reply(200, fixture!("chat_list")),
            // An empty page that is not the last one: only the cursor says
            // when the listing ends.
            reply(
                200,
                r#"{"object":"list","items":[],"nextCursor":"chats_3"}"#,
            ),
            reply(200, fixture!("chat_list_last")),
        ],
    )
    .await;
    let provider = provider(&server);
    let account = AccountId::new(ACCOUNT);
    assert!(provider.capabilities().chat_list);
    assert!(provider.capabilities().contact_names);

    // Paged the way the engine does: until there is no cursor.
    let mut titles = Vec::new();
    let mut cursor = None;
    loop {
        let page = provider.list_chats(&account, cursor).await.unwrap();
        titles.extend(page.items.into_iter().map(|chat| chat.title));
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    assert_eq!(
        titles,
        ["Launch team", "Maria Fernanda", "+584149998877", "Pedro"]
    );

    let targets: Vec<_> = requests(&server).await.iter().map(target).collect();
    assert_eq!(
        targets,
        [
            format!("/v1/accounts/{ACCOUNT}/chats?limit=100"),
            format!("/v1/accounts/{ACCOUNT}/chats?limit=100&cursor=chats_2"),
            format!("/v1/accounts/{ACCOUNT}/chats?limit=100&cursor=chats_3"),
        ],
        "no messages are paged and no groups are listed"
    );
}

#[tokio::test]
async fn a_chat_list_that_fails_says_whether_to_retry() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/chats"),
        vec![
            reply(503, r#"{"code":"unavailable","message":"Retry shortly."}"#),
            reply(401, fixture!("error_unauthorized")),
        ],
    )
    .await;
    let provider = provider(&server);
    let account = AccountId::new(ACCOUNT);
    // Neither is "the route is missing": no fallback, no extra requests.
    assert!(provider
        .list_chats(&account, None)
        .await
        .unwrap_err()
        .is_transient());
    assert!(matches!(
        provider.list_chats(&account, None).await,
        Err(ProviderError::Unauthorized(_))
    ));
    assert_eq!(requests(&server).await.len(), 2);
}

// ----- polling ------------------------------------------------------------

#[test]
fn polling_reports_only_what_changed() {
    let list: api::MessageList = parse(fixture!("message_list"));
    let mut state = PollState::default();

    // The first observation primes: its events are history, discarded by
    // the caller. Everything on the page was new.
    let (primed, all_new) = state.observe_messages(&list.items[2..]);
    assert!(all_new);
    assert!(!primed.is_empty());

    // Same page again: nothing.
    let (events, all_new) = state.observe_messages(&list.items[2..]);
    assert!(events.is_empty());
    assert!(!all_new);

    // A message changes status and two arrive, one in a new (group) chat.
    let mut page = list.items.clone();
    page[2].status = "read".into();
    page[2].updated_at = "2026-09-24T08:30:00.000Z".into();
    let (events, all_new) = state.observe_messages(&page);
    assert!(!all_new);

    let summary: Vec<String> = events
        .iter()
        .map(|e| match e {
            ProviderEvent::ChatUpdated(c) => format!("chat {}", c.title),
            ProviderEvent::MessageUpserted(m) => format!("message {} {:?}", m.id, m.status),
            other => format!("{other:?}"),
        })
        .collect();
    // Oldest first: the status change, the reaction, then the message of
    // the group. Chats are not made up from messages: they come from the
    // chat list.
    assert_eq!(
        summary,
        [
            "message m_sent_by_client Read",
            "message m_reaction Delivered",
            "message m_group_image Delivered",
        ]
    );
}

#[test]
fn polling_reports_connection_changes() {
    let mut account: api::Account = parse(fixture!("account"));
    let mut state = PollState::default();
    assert_eq!(
        state.observe_accounts(std::slice::from_ref(&account)).len(),
        1
    );
    assert!(state
        .observe_accounts(std::slice::from_ref(&account))
        .is_empty());

    account.status = "disconnected".into();
    account.reconnecting = true;
    assert_eq!(
        state.observe_accounts(&[account]),
        [ProviderEvent::ConnectionChanged {
            account_id: AccountId::new(ACCOUNT),
            state: ConnectionState::Reconnecting
        }]
    );
}

#[tokio::test]
async fn the_event_stream_primes_then_emits_and_survives_bad_ticks() {
    use futures::StreamExt;
    let server = MockServer::start().await;
    // A connected account: only those are asked for their chats.
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
            // Prime.
            reply(200, &accounts),
            // Tick 1: the backend hiccups. Skipped.
            reply(
                503,
                r#"{"code":"engine_unavailable","message":"Retry shortly."}"#,
            ),
            // Tick 2: the same account.
            reply(200, &accounts),
            // Tick 3: the key was revoked. The stream ends.
            reply(401, fixture!("error_unauthorized")),
        ],
    )
    .await;
    on(
        &server,
        "GET",
        "/v1/messages",
        vec![
            // Prime: the newest page as it is right now.
            reply(200, &primed),
            // Tick 2: a page with news.
            reply(200, fixture!("message_list")),
        ],
    )
    .await;
    let provider = provider(&server);

    let mut events = provider.subscribe().await.unwrap();
    let mut seen = Vec::new();
    while let Some(event) = events.next().await {
        seen.push(event);
    }
    // What existed when the stream opened is history, not news.
    let summary: Vec<String> = seen
        .iter()
        .map(|e| match e {
            ProviderEvent::ChatUpdated(c) => format!("chat {}", c.title),
            ProviderEvent::MessageUpserted(m) => format!("message {}", m.id),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(
        summary,
        [
            "message m_sent_by_client",
            "message m_reaction",
            "message m_group_image",
        ]
    );
    // The accounts four times and the messages twice; the chats of the
    // account when it was primed and when it had news; its stories once,
    // while priming. No group is looked up to name a chat.
    let targets: Vec<_> = requests(&server).await.iter().map(target).collect();
    assert_eq!(targets.len(), 9, "{targets:?}");
    assert!(!targets.iter().any(|target| target.contains("/groups")));
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.contains("/stories"))
            .count(),
        1
    );
    assert_eq!(
        targets
            .iter()
            .filter(|target| target.contains("/chats"))
            .count(),
        2
    );
}

#[tokio::test]
async fn the_event_stream_reports_chats_with_the_providers_unread_counts() {
    use futures::StreamExt;
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
            reply(200, &accounts),
            // The key was revoked: the stream ends.
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
    on(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/chats"),
        vec![
            // Prime: what the chats look like now is history.
            reply(200, fixture!("chat_list_last")),
            // The account has new messages: its chats are read again.
            reply(200, fixture!("chat_list")),
        ],
    )
    .await;
    let provider = provider(&server);

    let mut events = provider.subscribe().await.unwrap();
    let mut seen = Vec::new();
    while let Some(event) = events.next().await {
        seen.push(event);
    }
    let summary: Vec<String> = seen
        .iter()
        .map(|e| match e {
            ProviderEvent::ChatUpdated(c) => format!("chat {} unread {}", c.title, c.unread_count),
            ProviderEvent::MessageUpserted(m) => format!("message {}", m.id),
            other => format!("{other:?}"),
        })
        .collect();
    // Chats first, with wuapi's own names and counts, then the messages.
    // No chat is made up from a message, and the channel is left out.
    assert_eq!(
        summary,
        [
            "chat Launch team unread 3",
            "chat Maria Fernanda unread 1",
            "message m_sent_by_client",
            "message m_reaction",
            "message m_group_image",
        ]
    );
    let targets: Vec<_> = requests(&server).await.iter().map(target).collect();
    assert_eq!(
        targets,
        [
            "/v1/accounts?limit=100".to_owned(),
            "/v1/messages?limit=100".to_owned(),
            format!("/v1/accounts/{ACCOUNT}/chats?limit=50"),
            // The stories, on the first poll and then every few rounds.
            format!("/v1/accounts/{ACCOUNT}/stories/own?limit=100"),
            "/v1/accounts?limit=100".to_owned(),
            "/v1/messages?limit=100".to_owned(),
            format!("/v1/accounts/{ACCOUNT}/chats?limit=50"),
            "/v1/accounts?limit=100".to_owned(),
        ],
        "no group lookups and no extra pages of messages"
    );
}

#[test]
fn polling_reports_a_chat_only_when_it_changed() {
    let list: api::ChatList = parse(fixture!("chat_list"));
    let mut state = PollState::default();
    // Two conversations; the channel is not one.
    assert_eq!(state.observe_chats(&list.items).len(), 2);
    assert!(state.observe_chats(&list.items).is_empty());

    // Read on the phone: the count drops with no message involved.
    let mut read = list.items.clone();
    read[0].unread = Some(false);
    read[0].unread_count = Some(0);
    let events = state.observe_chats(&read);
    assert!(matches!(
        events.as_slice(),
        [ProviderEvent::ChatUpdated(chat)] if chat.unread_count == 0 && chat.title == "Launch team"
    ));

    // A new last message moves the chat too.
    let mut moved = read.clone();
    moved[1].last_message.as_mut().unwrap().id = "m_newer".into();
    let events = state.observe_chats(&moved);
    assert!(matches!(
        events.as_slice(),
        [ProviderEvent::ChatUpdated(chat)] if chat.title == "Maria Fernanda"
    ));
}

/// Replays queued answers and records what was asked: the transport of the
/// hand-written login requests.
#[derive(Default)]
struct FakeTransport {
    answers: Mutex<VecDeque<Result<ApiResponse, NetworkError>>>,
    requests: Mutex<Vec<ApiRequest>>,
}

impl FakeTransport {
    fn answer(&self, status: u16, body: &str) -> &Self {
        self.answers.lock().unwrap().push_back(Ok(ApiResponse {
            status,
            body: body.as_bytes().to_vec(),
            content_type: Some("application/json".into()),
            retry_after: None,
        }));
        self
    }

    fn fail(&self, reason: &str) -> &Self {
        self.answers
            .lock()
            .unwrap()
            .push_back(Err(NetworkError(reason.into())));
        self
    }

    fn requests(&self) -> Vec<ApiRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Transport for FakeTransport {
    async fn execute(&self, request: ApiRequest) -> Result<ApiResponse, NetworkError> {
        self.requests.lock().unwrap().push(request);
        self.answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("the test queued an answer for every request")
    }
}

// ----- device login -------------------------------------------------------

fn login(transport: &Arc<FakeTransport>) -> DeviceLogin {
    DeviceLogin::with_transport(&WuapiConfig::new("test-agent/1.0"), transport.clone())
}

#[tokio::test(start_paused = true)]
async fn device_login_requests_a_code_and_retries_hiccups() {
    let transport = Arc::new(FakeTransport::default());
    transport
        .fail("timed out")
        .answer(502, "")
        .answer(200, fixture!("device_code"));
    let code = login(&transport).request_code("my-laptop").await.unwrap();
    assert_eq!(code.user_code, "WXYZ-1234");
    assert_eq!(code.interval, 5);
    assert_eq!(code.expires_in, 600);
    assert!(code
        .verification_uri_complete
        .ends_with("user_code=WXYZ-1234"));
    assert!(!format!("{code:?}").contains(&code.device_code));

    let request = &transport.requests()[2];
    assert_eq!(request.method, Method::Post);
    assert_eq!(request.url, "https://api.wuapi.dev/cli/device/code");
    assert_eq!(
        request.body,
        Some(serde_json::json!({ "clientName": "my-laptop" }))
    );
    assert_eq!(request.bearer, None, "logging in needs no key");

    // A 4xx is final.
    transport.answer(
        429,
        r#"{"error":"rate_limited","message":"Too many logins started."}"#,
    );
    assert_eq!(
        login(&transport).request_code("x").await,
        Err(LoginError::Unexpected("rate_limited".into()))
    );
}

#[tokio::test]
async fn device_login_poll_steps() {
    let transport = Arc::new(FakeTransport::default());
    let code = parse(fixture!("device_code"));
    let login = login(&transport);
    let pending = r#"{"error":"authorization_pending"}"#;

    transport
        .answer(400, pending)
        .answer(400, r#"{"error":"slow_down"}"#)
        .answer(429, "")
        .answer(503, "")
        .fail("eof")
        .answer(200, fixture!("device_token"));
    assert_eq!(login.poll_once(&code).await, Ok(PollOutcome::Pending));
    assert_eq!(login.poll_once(&code).await, Ok(PollOutcome::SlowDown));
    assert_eq!(login.poll_once(&code).await, Ok(PollOutcome::SlowDown));
    for _ in 0..2 {
        assert!(matches!(
            login.poll_once(&code).await,
            Ok(PollOutcome::Unreachable(_))
        ));
    }
    let Ok(PollOutcome::Granted(grant)) = login.poll_once(&code).await else {
        panic!("the login was approved");
    };
    assert!(grant.api_key.expose().starts_with("wu_live_"));
    assert_eq!(grant.key_prefix.as_deref(), Some("wu_live_9c9c"));
    assert_eq!(grant.organization.name, "Acme");
    assert_eq!(grant.project, None);
    assert!(!format!("{grant:?}").contains(&grant.api_key.expose()[12..]));

    let request = &transport.requests()[0];
    assert_eq!(request.url, "https://api.wuapi.dev/cli/device/token");
    assert_eq!(
        request.body,
        Some(serde_json::json!({ "deviceCode": "a".repeat(64) }))
    );

    transport
        .answer(400, r#"{"error":"expired_token"}"#)
        .answer(400, r#"{"error":"access_denied"}"#)
        .answer(400, r#"{"error":"invalid_grant"}"#)
        .answer(
            400,
            r#"{"error":"invalid_request","message":"deviceCode is required."}"#,
        );
    assert_eq!(login.poll_once(&code).await, Err(LoginError::Expired));
    assert_eq!(login.poll_once(&code).await, Err(LoginError::Denied));
    assert_eq!(login.poll_once(&code).await, Err(LoginError::InvalidGrant));
    assert!(matches!(
        login.poll_once(&code).await,
        Err(LoginError::Unexpected(_))
    ));
}

#[tokio::test(start_paused = true)]
async fn device_login_waits_until_approved_or_expired() {
    let transport = Arc::new(FakeTransport::default());
    let code = parse(fixture!("device_code"));
    transport
        .answer(400, r#"{"error":"authorization_pending"}"#)
        .fail("eof")
        .answer(400, r#"{"error":"slow_down"}"#)
        .answer(200, fixture!("device_token"));
    let started = tokio::time::Instant::now();
    let grant = login(&transport).wait_for_token(&code).await.unwrap();
    assert_eq!(grant.organization.id, "w82t6y1u5i9o3p7a2s6d0f4g8h2j6k1l");
    // 5 + 5 + 5, then 10 after being told to slow down.
    assert_eq!(started.elapsed(), Duration::from_secs(25));

    // Nobody approves: the wait ends when the code does, on its own.
    let mut short = code.clone();
    short.expires_in = 12;
    for _ in 0..2 {
        transport.answer(400, r#"{"error":"authorization_pending"}"#);
    }
    assert_eq!(
        login(&transport).wait_for_token(&short).await,
        Err(LoginError::Expired)
    );
}

// ----- what a start costs, with the sync engine on top ----------------------

mod with_the_engine {
    use super::*;
    use client_core::{HistoryMode, Store, SyncConfig, SyncEngine};

    /// An API with one account and its chats on one page; every listing
    /// answers as often as it is asked.
    async fn api() -> MockServer {
        let server = MockServer::start().await;
        let accounts = format!(
            r#"{{"object":"list","items":[{}],"nextCursor":null}}"#,
            fixture!("account")
        );
        let mut chats: serde_json::Value = serde_json::from_str(fixture!("chat_list")).unwrap();
        chats["nextCursor"] = serde_json::Value::Null;
        for (at, body) in [
            ("/v1/accounts".to_owned(), accounts),
            (format!("/v1/accounts/{ACCOUNT}/chats"), chats.to_string()),
            (
                "/v1/messages".to_owned(),
                fixture!("message_list_last").to_owned(),
            ),
        ] {
            Mock::given(method("GET"))
                .and(path(at))
                .respond_with(reply(200, &body))
                .mount(&server)
                .await;
        }
        server
    }

    fn engine(server: &MockServer, history: HistoryMode) -> SyncEngine {
        SyncEngine::new(
            Arc::new(Store::open_in_memory().unwrap()),
            Arc::new(provider(server)),
            SyncConfig {
                history,
                preload_pace: Duration::from_millis(1),
                ..SyncConfig::default()
            },
            tokio::runtime::Handle::current(),
        )
    }

    async fn asked(server: &MockServer) -> Vec<String> {
        requests(server).await.iter().map(target).collect()
    }

    #[tokio::test]
    async fn the_chat_list_costs_two_requests_whatever_the_history_mode() {
        for mode in [
            HistoryMode::OnOpen,
            HistoryMode::Recent(1),
            HistoryMode::Everything,
        ] {
            let server = api().await;
            let engine = engine(&server, mode);
            engine.refresh().await.unwrap();

            // The accounts and the first (here, only) page of chats: the
            // list is on screen after these two, with names, previews and
            // unread counts. Nothing was asked per chat.
            assert_eq!(
                asked(&server).await,
                [
                    "/v1/accounts?limit=100".to_owned(),
                    format!("/v1/accounts/{ACCOUNT}/chats?limit=100"),
                ],
                "{mode:?}"
            );
            let account = AccountId::new(ACCOUNT);
            let chats = engine.store().chats(&account, None).unwrap();
            assert_eq!(chats.len(), 2);
            assert!(chats.iter().all(|chat| chat.last_message.is_some()));
            assert_eq!(chats[0].unread_count, 3, "the provider's count");

            // History, afterwards and in the background, as the mode says.
            engine.preload().await.unwrap();
            let history: Vec<_> = asked(&server).await.split_off(2);
            let expected = match mode {
                HistoryMode::OnOpen => 0,
                HistoryMode::Recent(_) => 1,
                HistoryMode::Everything => 2,
            };
            assert_eq!(history.len(), expected, "{mode:?}: {history:?}");
            assert!(
                history
                    .iter()
                    .all(|request| request.starts_with("/v1/messages?")
                        && request.contains("chatId="))
            );
        }
    }

    #[tokio::test]
    async fn opening_a_chat_is_one_request_for_that_chat() {
        let server = api().await;
        let engine = engine(&server, HistoryMode::OnOpen);
        engine.refresh().await.unwrap();
        let (account, chat) = (AccountId::new(ACCOUNT), ChatId::new("+584245550199"));
        engine.fetch_latest(&account, &chat).await.unwrap();
        let history: Vec<_> = asked(&server).await.split_off(2);
        assert_eq!(
            history,
            [format!(
                "/v1/messages?accountId={ACCOUNT}&chatId=%2B584245550199&limit=50"
            )]
        );
    }

    #[tokio::test]
    async fn a_revoked_key_is_asked_once_and_a_bad_gateway_is_asked_again() {
        // 401: the engine stops and says so.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(reply(401, fixture!("error_unauthorized")))
            .mount(&server)
            .await;
        let revoked = engine(&server, HistoryMode::OnOpen);
        let mut auth = revoked.auth_lost();
        revoked.start();
        tokio::time::timeout(Duration::from_secs(5), auth.changed())
            .await
            .expect("the engine noticed")
            .unwrap();
        assert!(revoked.is_auth_lost());
        let asked_once = requests(&server).await.len();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(requests(&server).await.len(), asked_once, "no retries");
        assert_eq!(asked_once, 1);
        revoked.shutdown();

        // 502: weather. Nobody is signed out, and it is asked again.
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(reply(
                502,
                r#"{"code":"bad_gateway","message":"Try again."}"#,
            ))
            .mount(&server)
            .await;
        let flaky = engine(&server, HistoryMode::OnOpen);
        flaky.start();
        tokio::time::sleep(Duration::from_millis(2500)).await;
        assert!(!flaky.is_auth_lost());
        assert!(requests(&server).await.len() >= 2, "it keeps trying");
        flaky.shutdown();
    }
}

// ----- pictures and media ---------------------------------------------------

mod pictures_and_media {
    use super::*;
    use client_provider::{AvatarAnswer, MediaLimit};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n-not-decoded-here";

    fn picture(server: &MockServer, id: &str) -> String {
        format!(
            r#"{{"object":"picture","id":"{id}","url":"{}/cdn/{id}.jpg?oe=expires","preview":true}}"#,
            server.uri()
        )
    }

    async fn serve(server: &MockServer, at: &str, body: &[u8], content_type: &str) {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.to_vec(), content_type))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_profile_picture_is_looked_up_then_downloaded_and_not_twice() {
        let server = MockServer::start().await;
        let route = format!("/v1/accounts/{ACCOUNT}/contacts/%2B584245550199/picture");
        on(
            &server,
            "GET",
            &route,
            vec![
                reply(200, &picture(&server, "pic-1")),
                reply(200, &picture(&server, "pic-1")),
                reply(200, &picture(&server, "pic-2")),
            ],
        )
        .await;
        serve(&server, "/cdn/pic-1.jpg", PNG, "image/jpeg").await;
        serve(&server, "/cdn/pic-2.jpg", b"second", "image/jpeg").await;
        let provider = provider(&server);
        assert!(provider.capabilities().avatars);
        let (account, contact) = (AccountId::new(ACCOUNT), ChatId::new("+584245550199"));

        // Nothing known: the small preview is asked for, and its bytes.
        let first = provider
            .fetch_avatar(&account, &contact, None)
            .await
            .unwrap();
        let AvatarAnswer::New(avatar) = first else {
            panic!("expected a picture, got {first:?}");
        };
        assert_eq!(
            (avatar.id.as_str(), avatar.bytes.as_slice()),
            ("pic-1", PNG)
        );

        // Same id as the client has: the lookup only, no download. (The
        // URL is different every time; the id is what counts.)
        let same = provider
            .fetch_avatar(&account, &contact, Some("pic-1"))
            .await
            .unwrap();
        assert_eq!(same, AvatarAnswer::Unchanged);

        // A new id: downloaded.
        let changed = provider
            .fetch_avatar(&account, &contact, Some("pic-1"))
            .await
            .unwrap();
        assert!(matches!(changed, AvatarAnswer::New(avatar) if avatar.id == "pic-2"));

        let targets: Vec<_> = requests(&server).await.iter().map(target).collect();
        assert_eq!(
            targets,
            [
                format!("{route}?preview=true"),
                "/cdn/pic-1.jpg?oe=expires".to_owned(),
                format!("{route}?preview=true"),
                format!("{route}?preview=true"),
                "/cdn/pic-2.jpg?oe=expires".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn no_picture_is_an_answer_and_a_rate_limit_is_not() {
        let server = MockServer::start().await;
        let contact_route = format!("/v1/accounts/{ACCOUNT}/contacts/%2B584245550199/picture");
        on(
            &server,
            "GET",
            &contact_route,
            vec![
                reply(
                    404,
                    r#"{"code":"picture_not_found","message":"No picture."}"#,
                ),
                reply(
                    200,
                    r#"{"object":"picture","id":null,"url":null,"preview":true}"#,
                ),
                reply(429, r#"{"code":"rate_limited","message":"Slow down."}"#)
                    .insert_header("retry-after", "7"),
                reply(
                    503,
                    r#"{"code":"engine_unavailable","message":"Retry shortly."}"#,
                ),
                reply(409, fixture!("error_not_ready")),
                reply(401, fixture!("error_unauthorized")),
            ],
        )
        .await;
        let provider = provider(&server);
        let (account, contact) = (AccountId::new(ACCOUNT), ChatId::new("+584245550199"));
        let ask = || provider.fetch_avatar(&account, &contact, None);

        // Hidden by privacy or not set: nothing to show, and not an error.
        assert_eq!(ask().await.unwrap(), AvatarAnswer::None);
        assert_eq!(ask().await.unwrap(), AvatarAnswer::None);
        // Told to slow down: says for how long.
        assert!(matches!(
            ask().await,
            Err(ProviderError::RateLimited { retry_after: Some(wait) }) if wait == Duration::from_secs(7)
        ));
        // The engine or the number is away: worth another try, later.
        assert!(ask().await.unwrap_err().is_transient());
        assert!(ask().await.unwrap_err().is_transient());
        // A revoked key is a revoked key here too.
        assert!(matches!(ask().await, Err(ProviderError::Unauthorized(_))));
    }

    #[tokio::test]
    async fn a_group_is_asked_on_the_same_path_and_keeps_its_icon_if_refused() {
        let server = MockServer::start().await;
        let route = format!("/v1/accounts/{ACCOUNT}/contacts/120363041234567890%40g.us/picture");
        on(
            &server,
            "GET",
            &route,
            vec![
                reply(200, &picture(&server, "grp-1")),
                // What a deployment that only takes contacts there says.
                reply(
                    400,
                    r#"{"code":"invalid_request","message":"`contactId` must be a contact."}"#,
                ),
                reply(404, r#"{"code":"whatsapp_not_found","message":"Unknown."}"#),
                reply(
                    403,
                    r#"{"code":"whatsapp_forbidden","message":"Not a member."}"#,
                ),
            ],
        )
        .await;
        serve(&server, "/cdn/grp-1.jpg", PNG, "image/jpeg").await;
        let provider = provider(&server);
        let (account, group) = (AccountId::new(ACCOUNT), ChatId::new(GROUP));

        let first = provider.fetch_avatar(&account, &group, None).await.unwrap();
        assert!(matches!(first, AvatarAnswer::New(avatar) if avatar.id == "grp-1"));
        for _ in 0..3 {
            let refused = provider.fetch_avatar(&account, &group, None).await.unwrap();
            assert_eq!(refused, AvatarAnswer::None, "the group icon stays");
        }
    }

    #[tokio::test]
    async fn media_is_refused_from_its_headers_before_it_is_read() {
        let server = MockServer::start().await;
        serve(&server, "/media/photo", PNG, "image/png; charset=binary").await;
        serve(&server, "/media/contract", b"%PDF-1.7", "application/pdf").await;
        serve(&server, "/media/big", &vec![7u8; 4096], "image/jpeg").await;
        Mock::given(method("GET"))
            .and(path("/media/gone"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let provider = provider(&server);
        let account = AccountId::new(ACCOUNT);
        let url = |name: &str| MediaRef::new(format!("{}/media/{name}", server.uri()));
        let images = MediaLimit {
            max_bytes: 1024,
            images_only: true,
        };
        let anything = MediaLimit {
            max_bytes: 1024 * 1024,
            images_only: false,
        };

        let photo = provider
            .fetch_media(&account, &url("photo"), images)
            .await
            .unwrap();
        assert_eq!(photo.bytes, PNG);
        assert_eq!(photo.mime_type.as_deref(), Some("image/png"));

        // Not an image: refused for a bubble, fetched when asked for.
        assert!(matches!(
            provider.fetch_media(&account, &url("contract"), images).await,
            Err(ProviderError::Rejected { code, .. }) if code == "not_an_image"
        ));
        let contract = provider
            .fetch_media(&account, &url("contract"), anything)
            .await
            .unwrap();
        assert_eq!(contract.bytes, b"%PDF-1.7");

        // Larger than the limit: refused, and not a reason to retry.
        let too_large = provider.fetch_media(&account, &url("big"), images).await;
        assert!(matches!(
            &too_large,
            Err(ProviderError::Rejected { code, .. }) if code == "too_large"
        ));
        assert!(!too_large.unwrap_err().is_transient());

        // Deleted with its message: gone for good.
        assert!(matches!(
            provider.fetch_media(&account, &url("gone"), anything).await,
            Err(ProviderError::Rejected { .. })
        ));
        // Only http(s) is ever fetched.
        for bad in [
            "file:///etc/passwd",
            "ftp://example.com/x",
            "javascript:alert(1)",
        ] {
            assert!(provider
                .fetch_media(&account, &MediaRef::new(bad), anything)
                .await
                .is_err());
        }
    }

    #[test]
    fn a_sticker_sent_from_the_phone_keeps_its_download_url() {
        // Outbound, but not sent through this client or the API: the phone
        // sent it, and wuapi stored the file. Its URL is wuapi's, like an
        // inbound one, and it is a sticker, not a document.
        let wire: api::Message = parse(fixture!("message_sticker_phone"));
        let message = mapping::message(&wire).expect("a message");
        assert_eq!(message.direction, Direction::Outgoing);
        assert_eq!(message.client_id, None, "not one of ours");
        let MessageContent::Media(media) = &message.content else {
            panic!("expected media, got {:?}", message.content);
        };
        assert_eq!(media.kind, MediaKind::Sticker);
        assert_eq!(media.mime_type.as_deref(), Some("image/webp"));
        assert_eq!(
            media.source,
            Some(MediaRef::new("https://api.wuapi.dev/v1/media/stk123"))
        );
        assert_eq!(media.caption, None);

        // History-imported: the API has no file for it.
        let mut imported = wire.clone();
        imported.media.as_mut().unwrap().url = None;
        let message = mapping::message(&imported).unwrap();
        assert!(matches!(
            &message.content,
            MessageContent::Media(media) if media.source.is_none() && media.kind == MediaKind::Sticker
        ));
    }

    // ----- media on demand ----------------------------------------------

    const MESSAGE: &str = "m_group_image";
    const ROUTE: &str = "/v1/messages/m_group_image/media";
    const ANY: MediaLimit = MediaLimit {
        max_bytes: 1024 * 1024,
        images_only: false,
    };

    /// The spec's image message, with its file still on WhatsApp.
    fn on_demand_message(size: Option<i64>) -> client_provider::Media {
        let mut wire: api::Message = parse(fixture!("message_group_image"));
        let media = wire.media.as_mut().unwrap();
        media.downloaded = false;
        media.url = Some(format!("https://api.wuapi.dev{ROUTE}"));
        media.size = size;
        match mapping::message(&wire).unwrap().content {
            MessageContent::Media(media) => media,
            other => panic!("expected media, got {other:?}"),
        }
    }

    fn media_file(storage: &MockServer) -> String {
        format!(
            r#"{{"object":"media","messageId":"{MESSAGE}","url":"{}/files/abc.jpg","mimeType":"image/jpeg","filename":null,"size":5}}"#,
            storage.uri()
        )
    }

    /// A provider that does not wait long between its attempts.
    fn quick(server: &MockServer) -> WuapiProvider {
        WuapiProvider::new(
            WuapiConfig {
                media_backoff: vec![Duration::from_millis(10), Duration::from_millis(20)],
                ..config(server)
            },
            ApiKey::new(KEY),
        )
        .unwrap()
    }

    fn bearers(requests: &[wiremock::Request]) -> Vec<Option<String>> {
        requests
            .iter()
            .map(|request| header(request, "authorization").map(str::to_owned))
            .collect()
    }

    #[test]
    fn what_the_api_knows_of_a_file_and_of_a_picture_is_kept() {
        // Size, pixels and length, when the API gives them.
        let mut wire: api::Message = parse(fixture!("message_group_image"));
        let media = wire.media.as_mut().unwrap();
        (media.size, media.width, media.height) = (Some(2048), Some(1280), Some(720));
        media.duration_seconds = Some(12);
        let MessageContent::Media(known) = mapping::message(&wire).unwrap().content else {
            panic!("expected media");
        };
        assert_eq!((known.width, known.height), (Some(1280), Some(720)));
        assert_eq!(known.duration_secs, Some(12));
        assert_eq!(known.size_bytes, Some(2048));
        // Half a size is no size, and nothing is made up.
        let media = wire.media.as_mut().unwrap();
        (media.width, media.height, media.duration_seconds) = (Some(1280), None, None);
        let MessageContent::Media(partial) = mapping::message(&wire).unwrap().content else {
            panic!("expected media");
        };
        assert_eq!((partial.width, partial.height), (None, None));
        assert_eq!(partial.duration_secs, None);

        // A chat says which picture it has, or that it knows of none.
        let list: api::ChatList = parse(fixture!("chat_list"));
        let mut chat = list.items[0].clone();
        chat.picture_id = Some("1727164540".into());
        let mapped = mapping::chat(&chat).unwrap();
        assert_eq!(mapped.picture_id.as_deref(), Some("1727164540"));
        assert!(!mapped.unknown.picture);
        chat.picture_id = None;
        let mapped = mapping::chat(&chat).unwrap();
        assert_eq!(mapped.picture_id, None);
        assert!(
            !mapped.unknown.picture,
            "the list was asked; it knows of none"
        );
    }

    #[test]
    fn where_a_file_is_fetched_from_is_decided_by_downloaded() {
        // Stored by wuapi: the URL is the file, and the size is kept.
        let mut wire: api::Message = parse(fixture!("message_group_image"));
        wire.media.as_mut().unwrap().size = Some(2048);
        let MessageContent::Media(stored) = mapping::message(&wire).unwrap().content else {
            panic!("expected media");
        };
        assert_eq!(
            stored.source,
            Some(MediaRef::new("https://api.wuapi.dev/v1/media/abc123"))
        );
        assert_eq!(stored.size_bytes, Some(2048));

        // Still on WhatsApp: the ref names the message, not the URL, so
        // nothing downstream can mistake it for a file to fetch directly.
        let waiting = on_demand_message(Some(2048));
        let source = waiting.source.clone().unwrap();
        assert_eq!(source.as_str(), "wuapi-media:2048:m_group_image");
        assert_eq!(
            mapping::on_demand(source.as_str()),
            Some((Some(2048), mapping::OnDemand::Message(MESSAGE)))
        );
        assert_eq!(waiting.size_bytes, Some(2048));
        let unsized_ = on_demand_message(None).source.unwrap();
        assert_eq!(
            mapping::on_demand(unsized_.as_str()),
            Some((None, mapping::OnDemand::Message(MESSAGE)))
        );
        assert_eq!(
            mapping::on_demand("https://api.wuapi.dev/v1/media/abc123"),
            None
        );

        // Nothing to fetch it with.
        wire.media.as_mut().unwrap().downloaded = false;
        wire.media.as_mut().unwrap().url = None;
        let MessageContent::Media(gone) = mapping::message(&wire).unwrap().content else {
            panic!("expected media");
        };
        assert_eq!(gone.source, None);
    }

    #[tokio::test]
    async fn a_stored_file_is_fetched_from_its_url_without_asking_the_api() {
        let server = MockServer::start().await;
        let storage = MockServer::start().await;
        serve(&storage, "/files/abc.jpg", b"bytes", "image/jpeg").await;
        let provider = quick(&server);
        let stored = MediaRef::new(format!("{}/files/abc.jpg", storage.uri()));
        let data = provider
            .fetch_media(&AccountId::new(ACCOUNT), &stored, ANY)
            .await
            .unwrap();
        assert_eq!(data.bytes, b"bytes");
        assert!(requests(&server).await.is_empty(), "the API was not asked");
        assert_eq!(bearers(&requests(&storage).await), vec![None]);
    }

    #[tokio::test]
    async fn a_file_still_on_whatsapp_is_asked_for_then_fetched_and_the_key_stays_with_the_api() {
        let server = MockServer::start().await;
        let storage = MockServer::start().await;
        on(
            &server,
            "GET",
            ROUTE,
            vec![
                reply(200, &media_file(&storage)),
                reply(200, &media_file(&storage)),
            ],
        )
        .await;
        serve(&storage, "/files/abc.jpg", b"bytes", "image/jpeg").await;
        let provider = quick(&server);
        let source = on_demand_message(Some(5)).source.unwrap();
        let data = provider
            .fetch_media(&AccountId::new(ACCOUNT), &source, ANY)
            .await
            .unwrap();
        assert_eq!(data.bytes, b"bytes");
        assert_eq!(data.mime_type.as_deref(), Some("image/jpeg"));

        // The API was asked for JSON, with the key.
        let asked = requests(&server).await;
        assert_eq!(asked.len(), 1);
        assert_eq!(target(&asked[0]), format!("{ROUTE}?redirect=false"));
        assert_eq!(
            header(&asked[0], "authorization"),
            Some(format!("Bearer {KEY}").as_str())
        );
        // The storage host was asked for the file, without it.
        let fetched = requests(&storage).await;
        assert_eq!(fetched.len(), 1);
        assert_eq!(bearers(&fetched), vec![None]);
        for request in &fetched {
            for (name, value) in request.headers.iter() {
                assert!(
                    !value.to_str().unwrap_or_default().contains(KEY),
                    "the key travelled in `{name}`"
                );
            }
            assert!(!request.url.as_str().contains(KEY));
        }

        // The same through the unlimited path.

        let data = provider
            .download_media(&AccountId::new(ACCOUNT), &source)
            .await
            .unwrap();
        assert_eq!(data.bytes, b"bytes");
        assert!(bearers(&requests(&storage).await)
            .iter()
            .all(Option::is_none));
    }

    #[tokio::test]
    async fn the_key_does_not_follow_a_redirect_off_the_api() {
        // A stored file whose URL is on the API and answers a redirect to
        // the storage host: the key goes to the API only.
        let server = MockServer::start().await;
        let storage = MockServer::start().await;
        on(
            &server,
            "GET",
            "/v1/media/abc123",
            vec![ResponseTemplate::new(302)
                .insert_header("location", format!("{}/files/abc.jpg", storage.uri()))],
        )
        .await;
        serve(&storage, "/files/abc.jpg", b"bytes", "image/jpeg").await;
        let provider = quick(&server);
        let own = MediaRef::new(format!("{}/v1/media/abc123", server.uri()));
        let data = provider
            .fetch_media(&AccountId::new(ACCOUNT), &own, ANY)
            .await
            .unwrap();
        assert_eq!(data.bytes, b"bytes");
        assert_eq!(
            bearers(&requests(&server).await),
            vec![Some(format!("Bearer {KEY}"))]
        );
        assert_eq!(bearers(&requests(&storage).await), vec![None]);
    }

    #[tokio::test]
    async fn a_fetch_that_fails_for_now_is_tried_again_and_then_works() {
        let server = MockServer::start().await;
        let storage = MockServer::start().await;
        on(
            &server,
            "GET",
            ROUTE,
            vec![
                reply(502, r#"{"code":"bad_gateway","message":"Try again."}"#),
                reply(409, fixture!("error_not_ready")),
                reply(200, &media_file(&storage)),
            ],
        )
        .await;
        serve(&storage, "/files/abc.jpg", b"bytes", "image/jpeg").await;
        let provider = quick(&server);
        let source = on_demand_message(None).source.unwrap();
        let data = provider
            .fetch_media(&AccountId::new(ACCOUNT), &source, ANY)
            .await
            .unwrap();
        assert_eq!(data.bytes, b"bytes");
        assert_eq!(requests(&server).await.len(), 3);
    }

    #[tokio::test]
    async fn a_fetch_that_keeps_failing_stays_transient_and_is_bounded() {
        let server = MockServer::start().await;
        on(
            &server,
            "GET",
            ROUTE,
            vec![
                reply(503, r#"{"code":"unavailable","message":"Try again."}"#),
                // Slower than the deadline of one attempt.
                reply(200, "{}").set_delay(Duration::from_secs(5)),
                reply(503, r#"{"code":"unavailable","message":"Try again."}"#),
                reply(200, "{}"),
            ],
        )
        .await;
        let provider = WuapiProvider::new(
            WuapiConfig {
                media_timeout: Duration::from_millis(150),
                media_backoff: vec![Duration::from_millis(10), Duration::from_millis(20)],
                ..config(&server)
            },
            ApiKey::new(KEY),
        )
        .unwrap();
        let source = on_demand_message(None).source.unwrap();
        let started = std::time::Instant::now();
        let error = provider
            .fetch_media(&AccountId::new(ACCOUNT), &source, ANY)
            .await
            .unwrap_err();
        // Never terminal: the client tries again later.
        assert!(error.is_transient(), "{error:?}");
        assert_eq!(requests(&server).await.len(), 3, "three attempts, no more");
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[tokio::test]
    async fn a_file_whatsapp_no_longer_has_is_terminal_and_says_so() {
        let server = MockServer::start().await;
        on(
            &server,
            "GET",
            ROUTE,
            vec![reply(
                410,
                r#"{"code":"media_expired","message":"The file is no longer on WhatsApp."}"#,
            )],
        )
        .await;
        let provider = quick(&server);
        let source = on_demand_message(None).source.unwrap();
        let error = provider
            .fetch_media(&AccountId::new(ACCOUNT), &source, ANY)
            .await
            .unwrap_err();
        assert!(!error.is_transient());
        assert!(
            matches!(&error, ProviderError::Rejected { code, .. } if code == "media_expired"),
            "{error:?}"
        );
        assert_eq!(error.to_string(), "WhatsApp no longer has this file.");
        assert_eq!(requests(&server).await.len(), 1, "asked once");
    }

    #[tokio::test]
    async fn a_file_over_the_limit_is_refused_before_the_api_fetches_it() {
        let server = MockServer::start().await;
        let storage = MockServer::start().await;
        let provider = quick(&server);
        let account = AccountId::new(ACCOUNT);
        let tiny = MediaLimit {
            max_bytes: 4,
            images_only: false,
        };
        // The message said its size: nothing is asked at all.
        let source = on_demand_message(Some(5)).source.unwrap();
        let error = provider
            .fetch_media(&account, &source, tiny)
            .await
            .unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "too_large"));
        assert!(requests(&server).await.is_empty());

        // It did not: the API's answer says it, and the file is not fetched.
        on(
            &server,
            "GET",
            ROUTE,
            vec![reply(200, &media_file(&storage))],
        )
        .await;
        let source = on_demand_message(None).source.unwrap();
        let error = provider
            .fetch_media(&account, &source, tiny)
            .await
            .unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "too_large"));
        assert!(requests(&storage).await.is_empty());
    }

    #[test]
    fn an_account_says_which_media_the_server_downloads_up_front() {
        use client_provider::{HistoryImport, ServerMedia};
        let mut wire: api::Account = parse(fixture!("account"));
        let mapped = mapping::account(&wire);
        assert_eq!(mapped.settings.server_media, Some(ServerMedia::Everything));
        wire.media_auto_download = serde_json::from_str(r#""none""#).unwrap();
        wire.history_sync = api::HistorySyncSetting::Recent;
        let mapped = mapping::account(&wire);
        assert_eq!(mapped.settings.server_media, Some(ServerMedia::OnDemand));
        assert_eq!(mapped.settings.history_import, Some(HistoryImport::Recent));
        wire.media_auto_download =
            serde_json::from_str(r#"{"maxBytes":5242880,"types":["image","sticker"]}"#).unwrap();
        assert_eq!(
            mapping::account(&wire).settings.server_media,
            Some(ServerMedia::Some {
                max_bytes: 5_242_880,
                kinds: vec!["image".into(), "sticker".into()],
            })
        );
        // A shape newer than the SDK: not said, never guessed.
        wire.media_auto_download = serde_json::from_str(r#"["later"]"#).unwrap();
        assert_eq!(mapping::account(&wire).settings.server_media, None);
    }
}

#[test]
fn imported_history_is_read_like_any_other_message() {
    // `source: history`: imported from the phone when the number linked.
    // No event announces these; they are in the chat list and in each
    // chat's history, dated when they were sent.
    let mut wire: api::Message = parse(fixture!("message_inbound"));
    wire.source = api::MessageSource::History;
    wire.sent_at = Some("2025-01-05T10:00:00.000Z".into());
    let message = mapping::message(&wire).expect("history is not skipped");
    assert_eq!(message.direction, Direction::Incoming);
    assert_eq!(message.client_id, None);
    assert_eq!(
        message.timestamp,
        mapping::timestamp("2025-01-05T10:00:00.000Z").unwrap(),
        "dated when it was sent, not when it was imported"
    );
    // One the account sent from its phone, before it was linked here.
    wire.direction = api::MessageDirection::Outbound;
    let message = mapping::message(&wire).unwrap();
    assert_eq!(message.direction, Direction::Outgoing);
    assert!(!matches!(message.status, DeliveryStatus::Failed { .. }));
}

mod numbers {
    use super::*;
    use client_provider::{
        AccountChange, HistoryImport, LinkPlace, LinkStatus, LinkStep, NewAccount,
    };

    const NEW: &str = "acc_new";
    /// A real, tiny PNG (1x1), as a QR code would arrive.
    const QR: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

    /// The spec's account, changed where a test needs it.
    fn account_with(change: impl FnOnce(&mut serde_json::Value)) -> String {
        let mut value: serde_json::Value = serde_json::from_str(fixture!("account")).unwrap();
        value["id"] = NEW.into();
        change(&mut value);
        value.to_string()
    }

    fn linking(status: &str) -> String {
        account_with(|account| {
            account["status"] = status.into();
            account["phone"] = serde_json::Value::Null;
            account["linkedAt"] = serde_json::Value::Null;
            account["billable"] = false.into();
        })
    }

    fn caracas() -> LinkPlace {
        LinkPlace {
            country: "VE".into(),
            country_name: "Venezuela".into(),
            city: "caracas".into(),
            city_name: "Caracas".into(),
        }
    }

    fn step(json: &str) -> LinkStatus {
        mapping::link_status(&parse::<api::Account>(json))
    }

    #[test]
    fn an_account_says_where_its_linking_stands() {
        assert_eq!(step(&linking("initializing")).step, LinkStep::Starting);
        assert!(!step(&linking("initializing")).was_linked);

        // The QR code is a PNG data URL, decoded here.
        let scan = step(&account_with(|account| {
            account["status"] = "qr_ready".into();
            account["qrCodeUrl"] = QR.into();
            account["linkedAt"] = serde_json::Value::Null;
        }));
        match scan.step {
            LinkStep::Scan { png } => assert!(png.starts_with(b"\x89PNG\r\n\x1a\n")),
            other => panic!("expected a code to scan, got {other:?}"),
        }
        // Anything else in that field is not drawn and not followed.
        for bad in [
            "https://example.com/qr.png",
            "data:image/svg+xml;base64,PHN2Zy8+",
            "data:image/png;base64,bm90IGEgcG5n",
            "data:image/png;base64,***",
        ] {
            let status = step(&account_with(|account| {
                account["status"] = "qr_ready".into();
                account["qrCodeUrl"] = bad.into();
            }));
            assert_eq!(status.step, LinkStep::Starting, "{bad}");
        }

        // A pairing code wins over a QR code.
        let code = step(&account_with(|account| {
            account["status"] = "qr_ready".into();
            account["pairingCode"] = "ABCD-1234".into();
            account["pairingCodeExpiresAt"] = "2026-09-24T08:18:20.000Z".into();
        }));
        assert!(matches!(
            &code.step,
            LinkStep::TypeCode { code, expires_at: Some(_) } if code == "ABCD-1234"
        ));

        assert_eq!(step(&linking("authenticating")).step, LinkStep::Finishing);
        let linked = step(fixture!("account"));
        assert_eq!(linked.step, LinkStep::Linked);
        assert!(linked.was_linked);

        // Stopped, with the reason in words.
        let late = step(&account_with(|account| {
            account["status"] = "disconnected".into();
            account["disconnectReason"] = "link_timeout".into();
            account["linkedAt"] = serde_json::Value::Null;
        }));
        assert_eq!(
            late.step,
            LinkStep::Stopped {
                reason: "The number was not linked in time.".into()
            }
        );
        assert!(!late.was_linked);
        // Never linked: said on the number itself, so it is not shown as
        // a session that broke.
        assert_eq!(late.account.settings.ever_linked, Some(false));
        assert!(late.account.never_linked());
        assert!(!linked.account.never_linked());
        let failed = step(&account_with(|account| {
            account["status"] = "failed".into();
            account["lastError"] = "proxy unreachable".into();
        }));
        assert_eq!(
            failed.step,
            LinkStep::Stopped {
                reason: "proxy unreachable".into()
            }
        );
        // A number that was ready and dropped is on its way back.
        let dropped = step(&account_with(|account| {
            account["status"] = "disconnected".into();
            account["reconnecting"] = true.into();
        }));
        assert_eq!(dropped.step, LinkStep::Starting);
    }

    #[tokio::test]
    async fn the_places_are_the_proxy_locations() {
        let server = MockServer::start().await;
        on(
            &server,
            "GET",
            "/v1/proxy-locations",
            vec![
                reply(
                    200,
                    r#"{"object":"list","items":[{"object":"proxy_location","country":"VE","countryName":"Venezuela","city":"caracas","cityName":"Caracas"}],"nextCursor":"p2"}"#,
                ),
                reply(
                    200,
                    r#"{"object":"list","items":[{"object":"proxy_location","country":"CO","countryName":"Colombia","city":"bogotá","cityName":"Bogotá"}],"nextCursor":null}"#,
                ),
            ],
        )
        .await;
        let places = provider(&server).link_places().await.unwrap();
        assert_eq!(places.len(), 2);
        assert_eq!(places[0], caracas());
        assert_eq!(places[1].city, "bogotá");
        let asked = requests(&server).await;
        assert_eq!(target(&asked[1]), "/v1/proxy-locations?limit=100&cursor=p2");
    }

    #[tokio::test]
    async fn creating_a_number_sends_one_request_id_however_often_it_is_sent() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/accounts",
            vec![
                reply(503, r#"{"code":"unavailable","message":"Try again."}"#),
                reply(201, &linking("initializing")),
            ],
        )
        .await;
        let provider = provider(&server);
        let new = NewAccount {
            name: Some("Support".into()),
            place: Some(caracas()),
            pairing_phone: Some("+58 412 123 4567".into()),
            history: HistoryImport::Recent,
            request_id: "attempt-1".into(),
        };
        let error = provider.create_account(&new).await.unwrap_err();
        assert!(error.is_transient(), "{error:?}");
        let status = provider.create_account(&new).await.unwrap();
        assert_eq!(status.account.id.as_str(), NEW);
        assert_eq!(status.step, LinkStep::Starting);
        assert!(!status.was_linked);

        let asked = requests(&server).await;
        assert_eq!(asked.len(), 2);
        for request in &asked {
            assert_eq!(header(request, "idempotency-key"), Some("attempt-1"));
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(
                body,
                serde_json::json!({
                    "name": "Support",
                    "proxyLocation": {"country": "VE", "city": "caracas"},
                    "pairingPhone": "+584121234567",
                    "historySync": "recent",
                })
            );
        }
    }

    #[tokio::test]
    async fn what_is_wrong_with_a_new_number_is_said_before_or_by_the_api() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/accounts",
            vec![
                reply(
                    402,
                    r#"{"code":"upgrade_required","message":"The Free plan includes 1 account. Upgrade to connect more."}"#,
                ),
                reply(
                    403,
                    r#"{"code":"project_limit_reached","message":"This project already has its maximum of 2 account(s)."}"#,
                ),
            ],
        )
        .await;
        let provider = provider(&server);
        let mut new = NewAccount {
            name: None,
            place: None,
            pairing_phone: None,
            history: HistoryImport::Off,
            request_id: "attempt-2".into(),
        };
        // No place, or a number that is not one: nothing is sent.
        let error = provider.create_account(&new).await.unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "place_required"));
        new.place = Some(caracas());
        new.pairing_phone = Some("maria".into());
        let error = provider.create_account(&new).await.unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "invalid_phone"));
        assert!(requests(&server).await.is_empty());

        // The plan's limits arrive in the API's own words, and are final.
        new.pairing_phone = None;
        for words in [
            "The Free plan includes 1 account. Upgrade to connect more.",
            "This project already has its maximum of 2 account(s).",
        ] {
            let error = provider.create_account(&new).await.unwrap_err();
            assert!(!error.is_transient());
            assert_eq!(error.to_string(), words);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&requests(&server).await[0].body).unwrap();
        assert_eq!(body["historySync"], "none", "the choice is always sent");
        assert!(body.get("pairingPhone").is_none());
    }

    #[tokio::test]
    async fn a_number_is_followed_renamed_reconnected_logged_out_and_deleted() {
        let server = MockServer::start().await;
        let at = format!("/v1/accounts/{NEW}");
        on(
            &server,
            "GET",
            &at,
            vec![
                reply(
                    200,
                    &account_with(|account| {
                        account["status"] = "qr_ready".into();
                        account["qrCodeUrl"] = QR.into();
                    }),
                ),
                reply(
                    200,
                    &account_with(|account| {
                        account["status"] = "qr_ready".into();
                        account["pairingCode"] = "ABCD-1234".into();
                    }),
                ),
            ],
        )
        .await;
        on(
            &server,
            "PATCH",
            &at,
            vec![
                reply(
                    200,
                    &account_with(|account| account["name"] = "Ventas".into()),
                ),
                reply(
                    200,
                    &account_with(|account| account["historySync"] = "recent".into()),
                ),
            ],
        )
        .await;
        on(
            &server,
            "POST",
            &format!("{at}/pairing-code"),
            vec![reply(
                200,
                r#"{"object":"pairing_code","accountId":"acc_new","code":"ABCD-1234","expiresAt":null}"#,
            )],
        )
        .await;
        on(
            &server,
            "POST",
            &format!("{at}/reconnect"),
            vec![reply(200, &linking("initializing"))],
        )
        .await;
        on(
            &server,
            "POST",
            &format!("{at}/logout"),
            vec![reply(
                200,
                &account_with(|account| {
                    account["status"] = "disconnected".into();
                    account["disconnectReason"] = "logged_out".into();
                }),
            )],
        )
        .await;
        on(
            &server,
            "DELETE",
            &at,
            vec![
                ResponseTemplate::new(204),
                reply(404, r#"{"code":"not_found","message":"No such account."}"#),
            ],
        )
        .await;
        let provider = provider(&server);
        let account = AccountId::new(NEW);

        let status = provider.link_status(&account).await.unwrap();
        assert!(matches!(status.step, LinkStep::Scan { .. }));
        let status = provider
            .pairing_code(&account, "+58 412 123 4567")
            .await
            .unwrap();
        assert!(matches!(status.step, LinkStep::TypeCode { .. }));
        // The API has no way from the code back to the QR code: the
        // client is told, and nothing is sent that would imitate one.
        assert!(provider.capabilities().link_by_code);
        assert!(!provider.capabilities().link_back_to_scan);
        let error = provider.scan_instead(&account).await.unwrap_err();
        assert!(matches!(error, ProviderError::Unsupported(_)));

        let renamed = provider
            .update_account(&account, AccountChange::Rename("  Ventas ".into()))
            .await
            .unwrap();
        assert_eq!(renamed.display_name, "Ventas");
        let error = provider
            .update_account(&account, AccountChange::Rename("  ".into()))
            .await
            .unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "invalid_name"));
        let imported = provider
            .update_account(&account, AccountChange::History(HistoryImport::Recent))
            .await
            .unwrap();
        assert_eq!(
            imported.settings.history_import,
            Some(HistoryImport::Recent)
        );

        let again = provider.reconnect_account(&account).await.unwrap();
        assert_eq!(again.step, LinkStep::Starting);
        let out = provider.unlink_account(&account).await.unwrap();
        assert_eq!(out.connection, ConnectionState::LoggedOut);
        provider.delete_account(&account).await.unwrap();
        // Already gone is done.
        provider.delete_account(&account).await.unwrap();

        let asked = requests(&server).await;
        let bodies: Vec<serde_json::Value> = asked
            .iter()
            .filter(|request| request.method.as_str() == "PATCH")
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        assert_eq!(
            bodies,
            vec![
                serde_json::json!({"name": "Ventas"}),
                serde_json::json!({"historySync": "recent"}),
            ]
        );
        let pairing = asked
            .iter()
            .find(|request| request.url.path().ends_with("/pairing-code"))
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&pairing.body).unwrap(),
            serde_json::json!({"phone": "+584121234567"})
        );
    }
}

mod contacts {
    use super::*;

    fn contact_json(id: &str, saved: Option<&str>, profile: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "object": "contact",
            "id": id,
            "accountId": ACCOUNT,
            "phone": id.starts_with('+').then_some(id),
            "lid": (!id.starts_with('+')).then_some(id),
            "savedName": saved,
            "profileName": profile,
            "username": null,
            "about": null,
            "pictureId": null,
            "businessName": null,
            "deviceCount": null,
        })
    }

    fn page(items: Vec<serde_json::Value>, next: Option<&str>) -> String {
        serde_json::json!({"object": "list", "items": items, "nextCursor": next}).to_string()
    }

    #[test]
    fn a_contact_keeps_both_of_its_ids() {
        // Listed by number, with the hidden-number id WhatsApp gave.
        let mut wire = contact_json("+584140000001", Some("Ana"), None);
        wire["lid"] = "lid:200055501000001".into();
        let contact = mapping::contact(&parse::<api::Contact>(&wire.to_string()));
        assert_eq!(contact.id.as_str(), "+584140000001");
        assert_eq!(
            contact.alt_ids,
            vec![client_provider::ContactId::new("lid:200055501000001")]
        );
        // Known only by the hidden-number id: there is no other id.
        let hidden = contact_json("lid:9911", Some("Hidden"), None);
        let contact = mapping::contact(&parse::<api::Contact>(&hidden.to_string()));
        assert!(contact.alt_ids.is_empty());
        assert_eq!(contact.phone, None);
    }

    #[tokio::test]
    async fn the_address_book_is_read_a_page_at_a_time() {
        let server = MockServer::start().await;
        let route = format!("/v1/accounts/{ACCOUNT}/contacts");
        let mut business = contact_json("+584140000002", None, Some("Pan"));
        business["businessName"] = "Panadería Central".into();
        business["pictureId"] = "1727164540".into();
        business["username"] = "pan_central".into();
        on(
            &server,
            "GET",
            &route,
            vec![
                reply(
                    200,
                    &page(
                        vec![
                            contact_json("+584140000001", Some("Ana"), Some("Ani")),
                            business,
                        ],
                        Some("p2"),
                    ),
                ),
                reply(
                    200,
                    &page(vec![contact_json("lid:9911", Some("Hidden"), None)], None),
                ),
            ],
        )
        .await;
        let provider = provider(&server);
        assert!(provider.capabilities().contacts);
        let account = AccountId::new(ACCOUNT);

        let first = provider.list_contacts(&account, None).await.unwrap();
        assert_eq!(first.items.len(), 2);
        let ana = &first.items[0];
        assert_eq!(ana.id.as_str(), "+584140000001");
        assert_eq!(ana.phone.as_deref(), Some("+584140000001"));
        assert_eq!(ana.saved_name.as_deref(), Some("Ana"));
        assert_eq!(ana.profile_name.as_deref(), Some("Ani"));
        assert_eq!(ana.display_name(), "Ana", "the saved name wins");
        let shop = &first.items[1];
        assert_eq!(shop.display_name(), "Panadería Central");
        assert_eq!(shop.picture_id.as_deref(), Some("1727164540"));
        assert_eq!(shop.username.as_deref(), Some("pan_central"));
        let next = first.next_cursor.expect("another page");

        let second = provider.list_contacts(&account, Some(next)).await.unwrap();
        assert_eq!(second.next_cursor, None);
        // A contact whose number WhatsApp hides is known by its id.
        assert_eq!(second.items[0].id.as_str(), "lid:9911");
        assert_eq!(second.items[0].phone, None);

        let asked = requests(&server).await;
        assert_eq!(target(&asked[0]), format!("{route}?limit=100"));
        assert_eq!(target(&asked[1]), format!("{route}?limit=100&cursor=p2"));
    }

    /// The contacts route is the address book: nothing stands in for it.
    /// A failure is said as it is and the chat list is not asked instead.
    #[tokio::test]
    async fn an_address_book_that_cannot_be_read_says_so_and_asks_nothing_else() {
        let server = MockServer::start().await;
        on(
            &server,
            "GET",
            &format!("/v1/accounts/{ACCOUNT}/contacts"),
            vec![
                reply(
                    404,
                    r#"{"code":"not_found","message":"Account not found."}"#,
                ),
                reply(
                    503,
                    r#"{"code":"engine_unavailable","message":"Try again."}"#,
                ),
            ],
        )
        .await;
        let provider = provider(&server);
        let account = AccountId::new(ACCOUNT);
        let error = provider.list_contacts(&account, None).await.unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "not_found"));
        assert!(provider
            .list_contacts(&account, None)
            .await
            .unwrap_err()
            .is_transient());
        let asked = requests(&server).await;
        assert_eq!(asked.len(), 2);
        assert!(asked
            .iter()
            .all(|request| request.url.path().ends_with("/contacts")));
    }

    #[tokio::test]
    async fn a_number_is_checked_before_it_is_written_to() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            &format!("/v1/accounts/{ACCOUNT}/contacts/check"),
            vec![reply(
                200,
                r#"{"object":"list","items":[
                    {"object":"contact_check","phone":"+584140000001","onWhatsApp":true,"contactId":"+584140000001","businessName":null,"username":null},
                    {"object":"contact_check","phone":"+584140000002","onWhatsApp":false,"contactId":null,"businessName":null,"username":null}
                ],"nextCursor":null}"#,
            )],
        )
        .await;
        let provider = provider(&server);
        assert!(provider.capabilities().number_check);
        let account = AccountId::new(ACCOUNT);
        // However they were typed, the numbers travel in E.164.
        let checks = provider
            .check_numbers(
                &account,
                &[
                    "+58 414 000 0001".to_owned(),
                    "0058 414-000-0002".to_owned(),
                ],
            )
            .await
            .unwrap();
        assert_eq!(checks.len(), 2);
        assert!(checks[0].on_whatsapp);
        assert_eq!(checks[0].chat.as_ref().unwrap().as_str(), "+584140000001");
        assert!(!checks[1].on_whatsapp);
        assert_eq!(checks[1].chat, None);
        let body: serde_json::Value =
            serde_json::from_slice(&requests(&server).await[0].body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({"phones": ["+584140000001", "+584140000002"]})
        );
        // What cannot be read as a number is refused before anything is
        // sent.
        let error = provider
            .check_numbers(&account, &["maria".to_owned()])
            .await
            .unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "invalid_phone"));
        assert_eq!(requests(&server).await.len(), 1);
    }
}

#[tokio::test]
async fn a_chat_is_marked_read_without_receipts_when_asked_quietly() {
    let server = MockServer::start().await;
    let chat = format!("/v1/accounts/{ACCOUNT}/chats/120363041234567890%40g.us");
    on(
        &server,
        "POST",
        &format!("{chat}/mark-read"),
        vec![ResponseTemplate::new(204)],
    )
    .await;
    let provider = provider(&server);
    assert!(provider.capabilities().quiet_read);
    provider
        .mark_read_quietly(&AccountId::new(ACCOUNT), &ChatId::new(GROUP))
        .await
        .unwrap();
    let targets: Vec<_> = requests(&server).await.iter().map(target).collect();
    assert_eq!(
        targets,
        [format!("{chat}/mark-read")],
        "no receipts were sent"
    );
}

#[test]
fn an_unread_count_the_api_does_not_know_is_said_to_be_unknown() {
    let list: api::ChatList = parse(fixture!("chat_list"));
    let mut chat = list.items[0].clone();
    // Counted by wuapi.
    (chat.unread, chat.unread_count) = (Some(true), Some(4));
    let mapped = mapping::chat(&chat).unwrap();
    assert_eq!(mapped.unread_count, 4);
    assert!(!mapped.unknown.unread);
    // Marked as unread, nothing to count: the least that can be true.
    (chat.unread, chat.unread_count) = (Some(true), Some(0));
    assert_eq!(mapping::chat(&chat).unwrap().unread_count, 1);
    (chat.unread, chat.unread_count) = (Some(true), None);
    let mapped = mapping::chat(&chat).unwrap();
    assert_eq!(mapped.unread_count, 1);
    assert!(!mapped.unknown.unread);
    // Read.
    (chat.unread, chat.unread_count) = (Some(false), Some(0));
    let mapped = mapping::chat(&chat).unwrap();
    assert_eq!(mapped.unread_count, 0);
    assert!(!mapped.unknown.unread);
    // Neither field: wuapi does not know (a chat from before it counted,
    // or imported history). That is not "read": the client keeps its own
    // count instead of taking a zero.
    (chat.unread, chat.unread_count) = (None, None);
    let mapped = mapping::chat(&chat).unwrap();
    assert_eq!(mapped.unread_count, 0);
    assert!(mapped.unknown.unread);
}

mod diagnosis {
    use super::*;
    use crate::diagnose::{chat_line, short_id};

    #[test]
    fn ids_are_cut_down_to_their_tails() {
        assert_eq!(short_id("+584245550199"), "phone:..0199");
        assert_eq!(short_id("120363041234567890@g.us"), "group:..7890");
        assert_eq!(short_id("lid:99887766554433"), "lid:..4433");
        assert_eq!(short_id("k57a8m2x9d3f0q1wjh6ypc4n2d7s0vbr"), "id:..0vbr");
        assert_eq!(short_id("ab"), "id:..ab");
    }

    #[test]
    fn a_chat_line_says_the_state_and_nothing_personal() {
        let list: api::ChatList = parse(fixture!("chat_list"));
        let mut chat = list.items[0].clone();
        chat.id = "+584245550199".into();
        chat.r#type = api::ChatType::Direct;
        chat.name = Some("María Secret".into());
        chat.saved_name = Some("Mamá".into());
        chat.unread = None;
        chat.unread_count = None;
        chat.pinned = Some(true);
        chat.archived = Some(false);
        chat.muted = None;
        chat.picture_id = Some("1727164540".into());
        chat.last_message_at = "2026-09-24T09:00:00.000Z".into();
        let line = chat_line(&chat);
        assert_eq!(
            line,
            "chat phone:..0199 type=direct unread=null unreadCount=null pinned=true \
             archived=false muted=null pictureId=yes lastMessageAt=2026-09-24T09:00:00.000Z \
             source=endpoint"
        );
        // Nothing of the person or the conversation.
        for secret in ["María", "Mamá", "+58424", "1727164540"] {
            assert!(!line.contains(secret), "{secret} in {line}");
        }
        if let Some(message) = &chat.last_message {
            if let Some(text) = message.text.as_deref().filter(|text| text.len() > 3) {
                assert!(!line.contains(text));
            }
        }
        chat.unread = Some(true);
        chat.unread_count = Some(3);
        chat.picture_id = None;
        let line = chat_line(&chat);
        assert!(line.contains("unread=true unreadCount=3"));
        assert!(line.contains("pictureId=no"));
    }

    #[tokio::test]
    async fn the_report_reads_the_first_page_of_each_account() {
        let server = MockServer::start().await;
        on(
            &server,
            "GET",
            "/v1/accounts",
            vec![reply(200, fixture!("account_list_last"))],
        )
        .await;
        Mock::given(method("GET"))
            .and(wiremock::matchers::path_regex(
                r"^/v1/accounts/[^/]+/chats$",
            ))
            .respond_with(reply(200, fixture!("chat_list_last")))
            .mount(&server)
            .await;
        let provider = provider(&server);
        let lines = provider.diagnose_chats().await.unwrap();
        assert_eq!(lines[0], format!("sdk wuapi {}", wuapi::VERSION));
        assert_eq!(lines[1], format!("api {}", server.uri()));
        assert!(lines[2].starts_with("accounts "));
        assert!(lines
            .iter()
            .any(|line| line.starts_with("account ") && line.contains("source=endpoint")));
        assert!(lines.iter().any(|line| line.starts_with("chat ")));
        // The key is in none of them.
        assert!(lines.iter().all(|line| !line.contains(KEY)));
        // Only the first page was asked for.
        let chat_asks = requests(&server)
            .await
            .iter()
            .filter(|request| request.url.path().ends_with("/chats"))
            .filter(|request| request.url.query().is_some_and(|q| q.contains("cursor")))
            .count();
        assert_eq!(chat_asks, 0);
    }
}

mod uploads {
    use super::*;
    use client_provider::{MediaUpload, UploadProgress};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn file(bytes: usize, key: &str) -> MediaUpload {
        MediaUpload {
            key: key.into(),
            kind: MediaKind::Document,
            bytes: Arc::new((0..bytes).map(|i| (i % 251) as u8).collect()),
            mime_type: "application/pdf".into(),
            file_name: Some("report.pdf".into()),
        }
    }

    fn upload_json(id: &str, status: &str, url: Option<&str>) -> String {
        serde_json::json!({
            "object": "upload", "id": id, "projectId": null, "status": status,
            "mimeType": "application/pdf", "filename": "report.pdf", "size": 1,
            "uploadUrl": url, "expiresAt": "2026-10-01T05:00:00.000Z",
            "createdAt": "2026-10-01T04:00:00.000Z",
        })
        .to_string()
    }

    fn counting() -> (UploadProgress, Arc<AtomicU64>) {
        let seen = Arc::new(AtomicU64::new(0));
        let report = seen.clone();
        (
            Arc::new(move |sent| {
                report.fetch_max(sent, Ordering::SeqCst);
            }),
            seen,
        )
    }

    const BIG: usize = 4 * 1024 * 1024;

    #[tokio::test]
    async fn a_file_is_created_stored_and_completed_and_the_key_stays_with_the_api() {
        let server = MockServer::start().await;
        let storage = MockServer::start().await;
        let url = format!("{}/store/abc?token=signed", storage.uri());
        on(
            &server,
            "POST",
            "/v1/uploads",
            vec![reply(201, &upload_json("up_1", "pending", Some(&url)))],
        )
        .await;
        on(
            &storage,
            "POST",
            "/store/abc",
            vec![reply(200, r#"{"storageId":"st_9"}"#)],
        )
        .await;
        on(
            &server,
            "POST",
            "/v1/uploads/up_1/complete",
            vec![reply(200, &upload_json("up_1", "ready", None))],
        )
        .await;
        let provider = provider(&server);
        assert!(provider.capabilities().media_upload);
        assert_eq!(provider.media_upload_limit(), Some(100 * 1024 * 1024));
        let (progress, seen) = counting();
        let upload = file(BIG, "c1:0");
        let bytes = upload.bytes.clone();
        let reference = provider
            .upload_media(&AccountId::new(ACCOUNT), upload, progress)
            .await
            .unwrap();
        assert_eq!(reference.as_str(), "wuapi-upload:up_1");
        assert_eq!(
            seen.load(Ordering::SeqCst),
            BIG as u64,
            "progress reached the end"
        );

        // The API: created under the caller's key, with the size, then
        // completed with what storage answered.
        let api = requests(&server).await;
        assert_eq!(api.len(), 2);
        assert_eq!(header(&api[0], "idempotency-key"), Some("c1:0"));
        let created: serde_json::Value = serde_json::from_slice(&api[0].body).unwrap();
        assert_eq!(
            created,
            serde_json::json!({"mimeType": "application/pdf", "filename": "report.pdf", "size": BIG})
        );
        let completed: serde_json::Value = serde_json::from_slice(&api[1].body).unwrap();
        assert_eq!(completed, serde_json::json!({"storageId": "st_9"}));
        for request in &api {
            assert_eq!(
                header(request, "authorization"),
                Some(format!("Bearer {KEY}").as_str())
            );
        }
        // Storage: the bytes and their type, and no trace of the key.
        let stored = requests(&storage).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].body, *bytes);
        assert_eq!(header(&stored[0], "content-type"), Some("application/pdf"));
        assert_eq!(header(&stored[0], "authorization"), None);
        for (name, value) in stored[0].headers.iter() {
            assert!(
                !value.to_str().unwrap_or_default().contains(KEY),
                "the key travelled to storage in `{name}`"
            );
        }
        assert!(!stored[0].url.as_str().contains(KEY));
    }

    #[tokio::test]
    async fn a_small_file_goes_in_one_request() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/uploads",
            vec![
                reply(201, &upload_json("up_2", "ready", None)),
                reply(201, &upload_json("up_2", "ready", None)),
            ],
        )
        .await;
        let provider = provider(&server);
        let account = AccountId::new(ACCOUNT);
        let (progress, seen) = counting();
        let reference = provider
            .upload_media(&account, file(2000, "c2:0"), progress.clone())
            .await
            .unwrap();
        assert_eq!(reference.as_str(), "wuapi-upload:up_2");
        assert_eq!(seen.load(Ordering::SeqCst), 2000);
        // Repeated (the answer was lost): the same key, the same upload.
        let again = provider
            .upload_media(&account, file(2000, "c2:0"), progress)
            .await
            .unwrap();
        assert_eq!(again, reference);
        let asked = requests(&server).await;
        assert_eq!(asked.len(), 2);
        assert!(asked
            .iter()
            .all(|request| header(request, "idempotency-key") == Some("c2:0")));
        let body: serde_json::Value = serde_json::from_slice(&asked[0].body).unwrap();
        assert!(body.get("size").is_none());
        let decoded = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(body["base64"].as_str().unwrap())
                .unwrap()
        };
        assert_eq!(decoded, *file(2000, "x").bytes);
    }

    #[tokio::test]
    async fn refusals_are_final_and_weather_is_not() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/uploads",
            vec![
                reply(
                    413,
                    r#"{"code":"media_too_large","message":"Too large.","details":{"maxBytes":104857600}}"#,
                ),
                reply(429, r#"{"code":"rate_limited","message":"Slow down."}"#)
                    .insert_header("retry-after", "7"),
                reply(400, r#"{"code":"invalid_request","message":"This type cannot be sent."}"#),
                reply(503, r#"{"code":"unavailable","message":"Try again."}"#),
            ],
        )
        .await;
        let provider = provider(&server);
        let account = AccountId::new(ACCOUNT);
        let attempt = |key: &'static str| {
            let (progress, _) = counting();
            provider.upload_media(&account, file(100, key), progress)
        };
        let error = attempt("a").await.unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "too_large"));
        let error = attempt("b").await.unwrap_err();
        assert_eq!(error.retry_after(), Some(Duration::from_secs(7)));
        assert!(error.is_transient());
        let error = attempt("c").await.unwrap_err();
        assert!(!error.is_transient());
        assert_eq!(error.to_string(), "This type cannot be sent.");
        assert!(attempt("d").await.unwrap_err().is_transient());

        // Over the limit: refused here, nothing sent.
        let before = requests(&server).await.len();
        let huge = MediaUpload {
            bytes: Arc::new(vec![0; 100 * 1024 * 1024 + 1]),
            ..file(1, "e")
        };
        let (progress, _) = counting();
        let error = provider
            .upload_media(&account, huge, progress)
            .await
            .unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "too_large"));
        assert_eq!(requests(&server).await.len(), before);
    }

    #[tokio::test]
    async fn a_deployment_without_the_routes_says_not_yet_and_is_asked_again_later() {
        // TEMPORARY(uploads-rollout)
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/uploads",
            vec![reply(404, "Not found"), reply(404, "Not found")],
        )
        .await;
        let provider = provider(&server);
        assert!(!provider.media_upload_ready().await);
        assert!(!provider.media_upload_ready().await);
        assert_eq!(
            requests(&server).await.len(),
            1,
            "remembered, not asked each time"
        );
        // A file queued anyway waits; it is not failed.
        let (progress, _) = counting();
        let error = provider
            .upload_media(&AccountId::new(ACCOUNT), file(10, "k"), progress)
            .await
            .unwrap_err();
        assert!(error.is_transient(), "{error:?}");

        // Where the routes exist, the probe is refused for its empty body.
        let deployed = MockServer::start().await;
        on(
            &deployed,
            "POST",
            "/v1/uploads",
            vec![reply(
                400,
                r#"{"code":"invalid_request","message":"mimeType is required."}"#,
            )],
        )
        .await;
        let provider = super::provider(&deployed);
        assert!(provider.media_upload_ready().await);
        assert!(provider.media_upload_ready().await);
        let asked = requests(&deployed).await;
        assert_eq!(asked.len(), 1);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&asked[0].body).unwrap(),
            serde_json::json!({}),
            "the probe creates nothing"
        );
    }

    #[tokio::test]
    async fn an_upload_that_expired_midway_is_started_again() {
        let server = MockServer::start().await;
        let storage = MockServer::start().await;
        let url = format!("{}/store/abc", storage.uri());
        on(
            &server,
            "POST",
            "/v1/uploads",
            vec![
                reply(201, &upload_json("up_old", "pending", Some(&url))),
                reply(201, &upload_json("up_new", "pending", Some(&url))),
            ],
        )
        .await;
        on(
            &storage,
            "POST",
            "/store/abc",
            vec![
                reply(200, r#"{"storageId":"st_1"}"#),
                reply(200, r#"{"storageId":"st_2"}"#),
            ],
        )
        .await;
        on(
            &server,
            "POST",
            "/v1/uploads/up_old/complete",
            vec![reply(
                404,
                r#"{"code":"not_found","message":"Upload not found."}"#,
            )],
        )
        .await;
        on(
            &server,
            "POST",
            "/v1/uploads/up_new/complete",
            vec![reply(200, &upload_json("up_new", "ready", None))],
        )
        .await;
        let provider = provider(&server);
        let (progress, _) = counting();
        let reference = provider
            .upload_media(&AccountId::new(ACCOUNT), file(BIG, "c9:0"), progress)
            .await
            .unwrap();
        assert_eq!(reference.as_str(), "wuapi-upload:up_new");
        let keys: Vec<String> = requests(&server)
            .await
            .iter()
            .filter(|request| request.url.path() == "/v1/uploads")
            .map(|request| header(request, "idempotency-key").unwrap().to_owned())
            .collect();
        assert_eq!(
            keys,
            ["c9:0", "c9:0~again"],
            "a new upload, not the expired one replayed"
        );
    }

    #[tokio::test]
    async fn a_message_refers_to_its_upload_and_an_upload_that_is_gone_is_said() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/messages",
            vec![
                reply(201, fixture!("message_sent_by_client")),
                reply(
                    404,
                    r#"{"code":"not_found","message":"Upload not found or expired."}"#,
                ),
                reply(
                    404,
                    r#"{"code":"not_found","message":"Account not found."}"#,
                ),
            ],
        )
        .await;
        let provider = provider(&server);
        let outgoing = OutgoingMessage {
            client_id: ClientMessageId::new("c1"),
            account_id: AccountId::new(ACCOUNT),
            chat_id: ChatId::new("+584245550199"),
            content: OutgoingContent::Media {
                kind: MediaKind::Document,
                media: MediaRef::new("wuapi-upload:up_1"),
                mime_type: Some("application/pdf".into()),
                caption: Some("the report".into()),
                file_name: Some("report.pdf".into()),
                gif: false,
            },
            reply_to: Some(MessageId::new("m_quoted")),
            mentions: Vec::new(),
            forwarded: false,
        };
        let receipt = provider.send(outgoing.clone()).await.unwrap();
        assert!(!receipt.message_id.as_str().is_empty());
        let asked = requests(&server).await;
        assert_eq!(header(&asked[0], "idempotency-key"), Some("c1"));
        let body: serde_json::Value = serde_json::from_slice(&asked[0].body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "accountId": ACCOUNT,
                "to": "+584245550199",
                "type": "document",
                "media": {"uploadId": "up_1", "mimeType": "application/pdf", "filename": "report.pdf"},
                "text": "the report",
                "replyToMessageId": "m_quoted",
                "metadata": {"clientMessageId": "c1"},
            })
        );
        // The upload expired before the send got through: said in the code
        // the client acts on, by uploading again.
        let error = provider.send(outgoing.clone()).await.unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "upload_expired"));
        // Any other 404 is what it is.
        let error = provider.send(outgoing).await.unwrap_err();
        assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "not_found"));
    }

    /// What stickers and GIFs are on the wire: a sticker is a `sticker`
    /// message with no caption; a GIF is a `video` whose media says
    /// `gifPlayback`; the flag goes nowhere else. A `.gif` sent as the
    /// image it is asks not to be re-encoded.
    #[tokio::test]
    async fn a_sticker_and_a_gif_are_sent_as_the_api_takes_them() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/messages",
            vec![
                reply(201, fixture!("message_sent_by_client")),
                reply(201, fixture!("message_sent_by_client")),
                reply(201, fixture!("message_sent_by_client")),
            ],
        )
        .await;
        let provider = provider(&server);
        let send = |client: &str, kind: MediaKind, mime: &str, gif: bool| OutgoingMessage {
            client_id: ClientMessageId::new(client),
            account_id: AccountId::new(ACCOUNT),
            chat_id: ChatId::new("+584245550199"),
            content: OutgoingContent::Media {
                kind,
                media: MediaRef::new("wuapi-upload:up_9"),
                mime_type: Some(mime.into()),
                caption: None,
                file_name: None,
                gif,
            },
            reply_to: Some(MessageId::new("m_quoted")),
            mentions: Vec::new(),
            forwarded: false,
        };
        provider
            .send(send("s1", MediaKind::Sticker, "image/webp", false))
            .await
            .unwrap();
        provider
            .send(send("g1", MediaKind::Video, "video/mp4", true))
            .await
            .unwrap();
        // The flag means nothing on anything but a video, and the API
        // refuses it there: it is not sent.
        provider
            .send(send("i1", MediaKind::Image, "image/gif", true))
            .await
            .unwrap();
        let asked = requests(&server).await;
        let body =
            |at: usize| -> serde_json::Value { serde_json::from_slice(&asked[at].body).unwrap() };
        assert_eq!(
            body(0),
            serde_json::json!({
                "accountId": ACCOUNT,
                "to": "+584245550199",
                "type": "sticker",
                "media": {"uploadId": "up_9", "mimeType": "image/webp"},
                "replyToMessageId": "m_quoted",
                "metadata": {"clientMessageId": "s1"},
            })
        );
        assert_eq!(
            body(1)["media"],
            serde_json::json!({"uploadId": "up_9", "mimeType": "video/mp4", "gifPlayback": true})
        );
        assert_eq!(body(1)["type"], "video");
        // The API re-encodes an image the way the WhatsApp apps do. A GIF
        // file would lose its frames: it goes as it is.
        assert_eq!(
            body(2)["media"],
            serde_json::json!({"uploadId": "up_9", "mimeType": "image/gif", "quality": "original"})
        );
        assert_eq!(header(&asked[1], "idempotency-key"), Some("g1"));
    }

    /// A photo says nothing about its quality: the account's setting
    /// decides, as on the phone.
    #[tokio::test]
    async fn a_photo_is_left_to_the_accounts_image_quality() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/messages",
            vec![reply(201, fixture!("message_sent_by_client"))],
        )
        .await;
        let provider = provider(&server);
        provider
            .send(OutgoingMessage {
                client_id: ClientMessageId::new("p1"),
                account_id: AccountId::new(ACCOUNT),
                chat_id: ChatId::new("+584245550199"),
                content: OutgoingContent::Media {
                    kind: MediaKind::Image,
                    media: MediaRef::new("wuapi-upload:up_3"),
                    mime_type: Some("image/jpeg".into()),
                    caption: None,
                    file_name: Some("photo.jpg".into()),
                    gif: false,
                },
                reply_to: None,
                mentions: Vec::new(),
                forwarded: false,
            })
            .await
            .unwrap();
        let asked = requests(&server).await;
        let body: serde_json::Value = serde_json::from_slice(&asked[0].body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "accountId": ACCOUNT,
                "to": "+584245550199",
                "type": "image",
                "media": {"uploadId": "up_3", "mimeType": "image/jpeg", "filename": "photo.jpg"},
                "metadata": {"clientMessageId": "p1"},
            })
        );
    }

    #[tokio::test]
    async fn a_caption_that_mentions_people_sends_their_ids_with_the_file() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/messages",
            vec![reply(201, fixture!("message_sent_by_client"))],
        )
        .await;
        let provider = provider(&server);
        let mention = |id: &str| client_provider::Mention {
            handle: id.trim_start_matches('+').to_owned(),
            id: client_provider::ContactId::new(id),
            name: None,
            me: false,
        };
        provider
            .send(OutgoingMessage {
                client_id: ClientMessageId::new("c7"),
                account_id: AccountId::new(ACCOUNT),
                chat_id: ChatId::new("120363000000000001@g.us"),
                content: OutgoingContent::Media {
                    kind: MediaKind::Image,
                    media: MediaRef::new("wuapi-upload:up_7"),
                    mime_type: Some("image/png".into()),
                    caption: Some("for @584245550199 and @584149998877".into()),
                    file_name: Some("photo.png".into()),
                    gif: false,
                },
                reply_to: Some(MessageId::new("m_quoted")),
                mentions: vec![mention("+584245550199"), mention("+584149998877")],
                forwarded: false,
            })
            .await
            .unwrap();
        let asked = requests(&server).await;
        let body: serde_json::Value = serde_json::from_slice(&asked[0].body).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "accountId": ACCOUNT,
                "to": "120363000000000001@g.us",
                "type": "image",
                "media": {"uploadId": "up_7", "mimeType": "image/png", "filename": "photo.png"},
                "text": "for @584245550199 and @584149998877",
                "replyToMessageId": "m_quoted",
                "mentions": ["+584245550199", "+584149998877"],
                "metadata": {"clientMessageId": "c7"},
            })
        );
    }

    #[tokio::test]
    async fn three_files_sent_together_each_carry_their_own_caption_and_mentions() {
        let server = MockServer::start().await;
        on(
            &server,
            "POST",
            "/v1/messages",
            vec![
                reply(201, fixture!("message_sent_by_client")),
                reply(201, fixture!("message_sent_by_client")),
                reply(201, fixture!("message_sent_by_client")),
            ],
        )
        .await;
        let provider = provider(&server);
        let mention = |id: &str| client_provider::Mention {
            handle: id.trim_start_matches('+').to_owned(),
            id: client_provider::ContactId::new(id),
            name: None,
            me: false,
        };
        let file = |n: usize, caption: Option<&str>, mentions: Vec<client_provider::Mention>| {
            OutgoingMessage {
                client_id: ClientMessageId::new(format!("batch:{n}")),
                account_id: AccountId::new(ACCOUNT),
                chat_id: ChatId::new("120363000000000001@g.us"),
                content: OutgoingContent::Media {
                    kind: MediaKind::Image,
                    media: MediaRef::new(format!("wuapi-upload:up_{n}")),
                    mime_type: Some("image/png".into()),
                    caption: caption.map(str::to_owned),
                    gif: false,
                    file_name: Some(format!("photo{n}.png")),
                },
                // As the sheet queues them: the reply goes with the first.
                reply_to: (n == 1).then(|| MessageId::new("m_quoted")),
                mentions,
                forwarded: false,
            }
        };
        // In the order of the strip; the outbox sends them one at a time.
        for message in [
            file(
                1,
                Some("first for @584245550199"),
                vec![mention("+584245550199")],
            ),
            file(2, Some("second, nobody"), Vec::new()),
            file(3, None, vec![]),
        ] {
            provider.send(message).await.unwrap();
        }
        let asked = requests(&server).await;
        let bodies: Vec<serde_json::Value> = asked
            .iter()
            .map(|request| serde_json::from_slice(&request.body).unwrap())
            .collect();
        assert_eq!(bodies.len(), 3);
        for (n, body) in bodies.iter().enumerate() {
            let n = n + 1;
            assert_eq!(
                body["media"]["uploadId"],
                format!("up_{n}"),
                "each message refers to its own file"
            );
            assert_eq!(body["metadata"]["clientMessageId"], format!("batch:{n}"));
            assert_eq!(
                header(&asked[n - 1], "idempotency-key"),
                Some(format!("batch:{n}").as_str())
            );
        }
        assert_eq!(bodies[0]["text"], "first for @584245550199");
        assert_eq!(bodies[0]["mentions"], serde_json::json!(["+584245550199"]));
        assert_eq!(bodies[0]["replyToMessageId"], "m_quoted");
        assert_eq!(bodies[1]["text"], "second, nobody");
        assert!(bodies[1].get("mentions").is_none());
        assert!(bodies[1].get("replyToMessageId").is_none());
        assert!(bodies[2].get("text").is_none());
        assert!(bodies[2].get("mentions").is_none());
        assert!(bodies[2].get("replyToMessageId").is_none());
    }

    #[tokio::test]
    async fn a_recorded_voice_note_is_uploaded_as_ogg_opus_and_sent_as_voice() {
        const MIME: &str = "audio/ogg; codecs=opus";
        let server = MockServer::start().await;
        // A note of a few seconds is small: it goes in one request.
        on(
            &server,
            "POST",
            "/v1/uploads",
            vec![reply(201, &upload_json("up_v", "ready", None))],
        )
        .await;
        on(
            &server,
            "POST",
            "/v1/messages",
            vec![reply(201, fixture!("message_sent_by_client"))],
        )
        .await;
        let provider = provider(&server);
        // What the recorder makes: an Ogg stream, with no name.
        let note: Arc<Vec<u8>> = Arc::new([b"OggS".as_slice(), &[3; 6_000]].concat());
        let (progress, _) = counting();
        let reference = provider
            .upload_media(
                &AccountId::new(ACCOUNT),
                MediaUpload {
                    key: "v1:0".into(),
                    kind: MediaKind::Voice,
                    bytes: note.clone(),
                    mime_type: MIME.into(),
                    file_name: None,
                },
                progress,
            )
            .await
            .unwrap();
        assert_eq!(reference.as_str(), "wuapi-upload:up_v");
        provider
            .send(OutgoingMessage {
                client_id: ClientMessageId::new("v1"),
                account_id: AccountId::new(ACCOUNT),
                chat_id: ChatId::new("+584245550199"),
                content: OutgoingContent::Media {
                    kind: MediaKind::Voice,
                    media: reference,
                    mime_type: Some(MIME.into()),
                    caption: None,
                    file_name: None,
                    gif: false,
                },
                reply_to: None,
                mentions: Vec::new(),
                forwarded: false,
            })
            .await
            .unwrap();

        // The API got the note as it was recorded, under its type, and
        // the message is a voice note that refers to it.
        let api = requests(&server).await;
        assert_eq!(api.len(), 2);
        let created: serde_json::Value = serde_json::from_slice(&api[0].body).unwrap();
        assert_eq!(created["mimeType"], MIME);
        assert!(created.get("filename").is_none_or(|name| name.is_null()));
        let sent = {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD
                .decode(created["base64"].as_str().unwrap())
                .unwrap()
        };
        assert_eq!(sent, *note);
        let message: serde_json::Value = serde_json::from_slice(&api.last().unwrap().body).unwrap();
        assert_eq!(
            message,
            serde_json::json!({
                "accountId": ACCOUNT,
                "to": "+584245550199",
                "type": "voice",
                "media": {"uploadId": "up_v", "mimeType": MIME},
                "metadata": {"clientMessageId": "v1"},
            })
        );
    }
}

// ----- the live transport: mode and tuning ---------------------------------

#[test]
fn stream_tuning_defaults_match_design() {
    let t = WuapiConfig::new("test-agent/1.0").tuning;
    let secs = Duration::from_secs;
    assert_eq!(t.idle, secs(45), "three heartbeats");
    assert_eq!(t.open, secs(12), "inside the engine's 30 s bound");
    assert_eq!((t.backoff_base, t.backoff_cap), (secs(1), secs(30)));
    assert_eq!(t.min_gap, secs(1));
    assert_eq!((t.budget, t.budget_window), (6, secs(60)));
    assert_eq!((t.retry_max, t.retry_after_max), (secs(60), secs(300)));
    assert_eq!(t.stable_after, secs(30));
    assert_eq!((t.fallback_failures, t.fallback_after), (3, secs(20)));
    assert_eq!(t.give_up_after, secs(30 * 60));
    assert_eq!(t.safety_poll, secs(75));
    assert_eq!(t.reprobe, secs(600));
    assert_eq!(t.dirty_window, Duration::from_millis(500));
    assert_eq!(t.dirty_gap, secs(2));
    assert_eq!((t.dirty_per_minute, t.dirty_concurrency), (60, 4));
    assert_eq!(
        t.dirty_collapse, 200,
        "pending chats past this become a page"
    );
    assert_eq!((t.dirty_retry, t.dirty_retries), (secs(5), 3));
    assert_eq!(t.evt_lru, 4096);
    assert_eq!(t.max_frame, 1024 * 1024);
}

#[test]
fn default_live_is_auto_after_flip() {
    let config = WuapiConfig::new("test-agent/1.0");
    assert_eq!(config.live, LiveTransport::Auto);
    assert_eq!(config.stream_url, None);
    assert_eq!(DEFAULT_STREAM_URL, "https://stream.wuapi.dev");
}

#[test]
fn realtime_push_true_unless_polling() {
    let with = |live| {
        let config = WuapiConfig {
            live,
            ..WuapiConfig::new("test-agent/1.0")
        };
        WuapiProvider::new(config, ApiKey::new(KEY))
            .unwrap()
            .capabilities()
            .realtime_push
    };
    assert!(with(LiveTransport::Auto));
    assert!(with(LiveTransport::Stream));
    assert!(!with(LiveTransport::Polling));
    // Falling back is still Auto: the flag says what the adapter can do.
    let fallback = WuapiProvider::new(
        WuapiConfig {
            live: LiveTransport::Auto,
            stream_url: Some("not a url".into()),
            ..WuapiConfig::new("test-agent/1.0")
        },
        ApiKey::new(KEY),
    )
    .unwrap();
    assert!(fallback.capabilities().realtime_push);
}

#[test]
fn check_stream_url_takes_https_anywhere() {
    for url in [
        "https://stream.wuapi.dev",
        "https://example.com:8443",
        "https://example.com/prefix",
        "HTTPS://Example.COM",
    ] {
        assert_eq!(check_stream_url(url), Ok(()), "{url}");
    }
}

#[test]
fn check_stream_url_takes_http_only_on_loopback() {
    for url in [
        "http://localhost",
        "http://localhost:8080",
        "http://127.0.0.1",
        "http://127.9.9.9:80",
        "http://[::1]:9000",
    ] {
        assert_eq!(check_stream_url(url), Ok(()), "{url}");
    }
    for url in [
        "http://example.com",
        "http://192.168.1.5",
        "http://localhostx",
        "http://127.example.com",
        "http://128.0.0.1",
        "http://[::2]",
    ] {
        assert!(check_stream_url(url).is_err(), "{url}");
    }
}

#[test]
fn check_stream_url_refuses_user_info_and_junk() {
    for url in [
        "https://user:secret@example.com",
        "http://user@localhost",
        "example.com",
        "ftp://example.com",
        "https://",
        "",
    ] {
        let why = check_stream_url(url).unwrap_err();
        assert!(!why.contains("secret"), "the reason never echoes a secret");
        assert!(!why.is_empty(), "{url}");
    }
}

#[test]
fn check_stream_url_refuses_a_query_or_a_fragment() {
    // The path is appended to a root: a query or a fragment would end up
    // in the middle of the address, and a credential in it in the report.
    for url in [
        "https://example.com/?token=secret",
        "https://example.com?token=secret",
        "https://example.com/prefix?x=1",
        "https://example.com/#frag",
        "https://example.com#frag",
        "http://localhost:9?x=1",
    ] {
        let why = check_stream_url(url).unwrap_err();
        assert!(!why.contains("secret"), "the reason never echoes it: {why}");
        assert!(!why.is_empty(), "{url}");
        assert!(endpoint(DEFAULT_BASE_URL, Some(url)).is_err(), "{url}");
    }
    // Triangulation: the same roots without them are fine.
    for url in [
        "https://example.com/",
        "https://example.com/prefix",
        "http://localhost:9",
    ] {
        assert_eq!(check_stream_url(url), Ok(()), "{url}");
    }
}

fn endpoint(base_url: &str, stream_url: Option<&str>) -> Result<String, String> {
    WuapiConfig {
        base_url: base_url.to_owned(),
        stream_url: stream_url.map(str::to_owned),
        ..WuapiConfig::new("test-agent/1.0")
    }
    .stream_endpoint()
}

#[test]
fn stream_endpoint_production_is_exact() {
    assert_eq!(
        endpoint(DEFAULT_BASE_URL, None).as_deref(),
        Ok("https://stream.wuapi.dev/v1/events/stream")
    );
    assert_eq!(
        endpoint("https://api.wuapi.dev/", None).as_deref(),
        Ok("https://stream.wuapi.dev/v1/events/stream"),
        "a trailing slash is still production"
    );
}

#[test]
fn stream_endpoint_other_apis_use_their_own_origin() {
    assert_eq!(
        endpoint("https://api.example.com", None).as_deref(),
        Ok("https://api.example.com/v1/events/stream")
    );
    assert_eq!(
        endpoint("http://127.0.0.1:8123/ignored/path", None).as_deref(),
        Ok("http://127.0.0.1:8123/v1/events/stream"),
        "only the origin is taken; there is no api. to stream. swap"
    );
    assert_eq!(
        endpoint("https://api.wuapi.dev.evil.example", None).as_deref(),
        Ok("https://api.wuapi.dev.evil.example/v1/events/stream"),
    );
}

#[test]
fn stream_endpoint_stream_url_is_a_root() {
    assert_eq!(
        endpoint(DEFAULT_BASE_URL, Some("https://gw.example.com")).as_deref(),
        Ok("https://gw.example.com/v1/events/stream")
    );
    assert_eq!(
        endpoint(DEFAULT_BASE_URL, Some("http://localhost:9/")).as_deref(),
        Ok("http://localhost:9/v1/events/stream")
    );
}

#[test]
fn stream_endpoint_refuses_what_check_stream_url_refuses() {
    // A derived URL that is plain http off loopback.
    assert!(endpoint("http://192.168.1.5", None).is_err());
    // An explicit one, too.
    assert!(endpoint(DEFAULT_BASE_URL, Some("http://example.com")).is_err());
    assert!(endpoint(DEFAULT_BASE_URL, Some("https://u:p@example.com")).is_err());
}

mod acceptance;
mod actions;
mod chat_reads;
mod chunk_spike;
mod diagnose;
mod dirty;
mod envelope;
mod fake_stream;
mod favorites;
mod forward;
mod live;
mod message_types;
mod older_backend;
mod sending;
mod social;
mod sse;
mod stories;
mod stream;
