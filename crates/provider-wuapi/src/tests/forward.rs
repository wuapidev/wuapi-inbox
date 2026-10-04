//! Forwarding by naming the message, against a mock API on localhost:
//! what is asked, under which key, what each refusal becomes, and what a
//! deployment without the route degrades to.

use super::{header, on, provider, reply, requests, target, ACCOUNT};
use crate::availability::RECHECK;
use client_provider::{
    refusal, AccountId, ChatId, ClientMessageId, DeliveryStatus, Feature, ForwardItem, MessageId,
    Provider, ProviderError,
};
use wiremock::MockServer;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

const ROUTE: &str = "/v1/messages/m_source/forward";

fn account() -> AccountId {
    AccountId::new(ACCOUNT)
}

fn item(client: &str, to: &str) -> ForwardItem {
    ForwardItem {
        client_id: ClientMessageId::new(client),
        message: MessageId::new("m_source"),
        to: ChatId::new(to),
    }
}

/// The API's answer: one queued message for the chat, marked forwarded.
/// It carries no client id: the route takes no metadata.
fn copy(id: &str, to: &str) -> String {
    let mut wire: serde_json::Value = serde_json::from_str(fixture!("message")).unwrap();
    wire["id"] = id.into();
    wire["chatId"] = to.into();
    wire["to"] = to.into();
    wire["forwarded"] = true.into();
    wire["metadata"] = serde_json::json!({});
    serde_json::json!({"object": "list", "items": [wire], "nextCursor": null}).to_string()
}

fn refused(code: &str, reason: Option<&str>) -> String {
    let mut body = serde_json::json!({"code": code, "message": "The API's own words."});
    if let Some(reason) = reason {
        body["details"] = serde_json::json!({ "reason": reason });
    }
    body.to_string()
}

#[tokio::test]
async fn each_copy_is_one_request_under_its_own_key_and_a_repeat_is_the_same_request() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        ROUTE,
        vec![
            reply(202, &copy("m_copy_1", "+584245550199")),
            // The answer to the second was lost on the way.
            reply(504, r#"{"code":"timeout","message":"Try again."}"#),
            reply(202, &copy("m_copy_2", "120363041234567890@g.us")),
        ],
    )
    .await;
    let provider = provider(&server);
    let caps = provider.capabilities();
    assert!(caps.forwards && caps.forward_any);
    assert!(
        !caps.forward_polls && !caps.forward_events,
        "the API refuses those, so they are not offered"
    );
    let items = [
        item("c1", "+584245550199"),
        item("c2", "120363041234567890@g.us"),
    ];
    let answers = provider.forward_messages(&account(), &items).await.unwrap();
    assert_eq!(answers.len(), 2, "one answer per item, in order");
    let first = answers[0].as_ref().unwrap();
    assert_eq!(first.message_id.as_str(), "m_copy_1");
    assert_eq!(first.status, DeliveryStatus::Accepted, "queued by the API");
    // A drop fails that copy alone, for now: the client asks again.
    assert!(answers[1].as_ref().unwrap_err().is_transient());
    let again = provider
        .forward_messages(&account(), &items[1..])
        .await
        .unwrap();
    assert_eq!(again[0].as_ref().unwrap().message_id.as_str(), "m_copy_2");

    let asked = requests(&server).await;
    assert_eq!(asked.len(), 3);
    assert!(asked.iter().all(|request| target(request) == ROUTE));
    let bodies: Vec<serde_json::Value> = asked
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(bodies[0], serde_json::json!({"to": ["+584245550199"]}));
    assert_eq!(
        bodies[1],
        serde_json::json!({"to": ["120363041234567890@g.us"]})
    );
    assert_eq!(bodies[1], bodies[2], "the identical request");
    let keys: Vec<_> = asked
        .iter()
        .map(|request| header(request, "idempotency-key"))
        .collect();
    assert_eq!(keys, [Some("c1"), Some("c2"), Some("c2")]);
    assert!(!provider.unavailable(&account(), Feature::ForwardAny));
}

#[tokio::test]
async fn every_refusal_fails_its_copy_alone_with_the_reason_the_client_words() {
    let server = MockServer::start().await;
    let cases: [(u16, &str, Option<&str>, &str); 13] = [
        (
            400,
            "not_forwardable",
            Some("deleted"),
            refusal::FORWARD_DELETED,
        ),
        (
            400,
            "not_forwardable",
            Some("view_once"),
            refusal::FORWARD_VIEW_ONCE,
        ),
        (
            400,
            "not_forwardable",
            Some("not_sent"),
            refusal::FORWARD_NOT_SENT,
        ),
        (400, "not_forwardable", Some("poll"), refusal::FORWARD_KIND),
        (
            400,
            "not_forwardable",
            Some("calendar_event"),
            refusal::FORWARD_KIND,
        ),
        (
            400,
            "not_forwardable",
            Some("reaction"),
            refusal::FORWARD_KIND,
        ),
        (
            400,
            "not_forwardable",
            Some("unknown"),
            refusal::FORWARD_KIND,
        ),
        (400, "not_forwardable", Some("empty"), refusal::FORWARD_KIND),
        (400, "not_forwardable", Some("story"), refusal::FORWARD_KIND),
        (
            400,
            "not_forwardable",
            Some("media_not_stored"),
            refusal::FORWARD_NO_FILE,
        ),
        // A reason newer than this adapter, and none at all.
        (400, "not_forwardable", None, refusal::FORWARD_KIND),
        (410, "media_expired", None, refusal::FORWARD_FILE_GONE),
        (413, "media_too_large", None, refusal::FORWARD_TOO_LARGE),
    ];
    let mut replies: Vec<_> = cases
        .iter()
        .map(|(status, code, reason, _)| reply(*status, &refused(code, *reason)))
        .collect();
    // After them, one that goes through: a refusal stopped nothing.
    replies.push(reply(202, &copy("m_copy", "+584245550199")));
    on(&server, "POST", ROUTE, replies).await;
    let provider = provider(&server);
    let items: Vec<ForwardItem> = (0..=cases.len())
        .map(|index| item(&format!("c{index}"), "+584245550199"))
        .collect();
    let answers = provider.forward_messages(&account(), &items).await.unwrap();
    assert_eq!(answers.len(), cases.len() + 1);
    for ((_, wire, reason, expected), answer) in cases.iter().zip(&answers) {
        match answer {
            Err(ProviderError::Rejected { code, .. }) => {
                assert_eq!(code, expected, "{wire} {reason:?}")
            }
            other => panic!("{wire} {reason:?}: {other:?}"),
        }
    }
    assert!(answers.last().unwrap().is_ok());
    // None of that says the route is missing.
    assert!(!provider.unavailable(&account(), Feature::ForwardAny));
}

#[tokio::test]
async fn other_answers_keep_their_classification() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        ROUTE,
        vec![
            reply(409, fixture!("error_not_ready")),
            reply(429, r#"{"code":"rate_limited","message":"Slow down."}"#)
                .insert_header("retry-after", "9"),
            // The message is not one the API holds.
            reply(
                404,
                r#"{"code":"not_found","message":"Message not found."}"#,
            ),
            reply(
                402,
                r#"{"code":"free_limit_reached","message":"This month's limit was reached."}"#,
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let items: Vec<ForwardItem> = (0..4)
        .map(|index| item(&format!("c{index}"), "+584245550199"))
        .collect();
    let answers = provider.forward_messages(&account(), &items).await.unwrap();
    assert!(answers[0].as_ref().unwrap_err().is_transient());
    assert_eq!(
        answers[1].as_ref().unwrap_err().retry_after(),
        Some(std::time::Duration::from_secs(9))
    );
    assert!(
        matches!(&answers[2], Err(ProviderError::Rejected { code, .. }) if code == "not_found")
    );
    let limit = answers[3].as_ref().unwrap_err();
    assert_eq!(limit.to_string(), "This month's limit was reached.");
    assert!(
        !provider.unavailable(&account(), Feature::ForwardAny),
        "a message that is not found is not a route that is not there"
    );

    // A revoked key is not about one message: the whole call says so.
    let revoked = MockServer::start().await;
    on(
        &revoked,
        "POST",
        ROUTE,
        vec![reply(401, fixture!("error_unauthorized"))],
    )
    .await;
    let provider = super::provider(&revoked);
    assert!(matches!(
        provider.forward_messages(&account(), &items[..1]).await,
        Err(ProviderError::Unauthorized(_))
    ));
}

/// A backend from before the route: "not available yet", once, without a
/// request for the rest of the session's ten minutes, and asked again
/// after them.
#[tokio::test]
async fn a_deployment_without_the_route_says_not_yet_and_is_asked_again_later() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        ROUTE,
        vec![
            reply(404, r#"{"code":"not_found","message":"Route not found."}"#),
            reply(202, &copy("m_copy", "+584245550199")),
        ],
    )
    .await;
    let provider = provider(&server);
    let items = [item("c1", "+584245550199"), item("c2", "+584140000009")];
    assert!(!provider.unavailable(&account(), Feature::ForwardAny));
    let first = provider.forward_messages(&account(), &items).await;
    assert!(
        matches!(first, Err(ProviderError::Unsupported(_))),
        "{first:?}"
    );
    assert!(provider.unavailable(&account(), Feature::ForwardAny));
    // Every number: the route is the deployment's.
    assert!(provider.unavailable(&AccountId::new("another"), Feature::ForwardAny));
    // Nothing else is said to be missing.
    assert!(!provider.unavailable(&account(), Feature::Stories));
    assert!(!provider.unavailable(&account(), Feature::StickerFavorites));

    let second = provider.forward_messages(&account(), &items).await;
    assert!(matches!(second, Err(ProviderError::Unsupported(_))));
    assert_eq!(
        requests(&server).await.len(),
        1,
        "the first item found out; nothing was asked after that"
    );

    provider.client().missing.age(RECHECK);
    assert!(!provider.unavailable(&account(), Feature::ForwardAny));
    let later = provider
        .forward_messages(&account(), &items[..1])
        .await
        .unwrap();
    assert!(later[0].is_ok());
    assert!(!provider.unavailable(&account(), Feature::ForwardAny));
}

/// A forwarded copy is followed like any message sent from here.
#[tokio::test]
async fn a_forwarded_copy_is_followed_for_its_ticks() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        ROUTE,
        vec![reply(202, &copy("m_copy", "+584245550199"))],
    )
    .await;
    let (provider, polling) =
        crate::provider::WuapiProvider::with_polling(super::config(&server), super::KEY.into())
            .unwrap();
    assert_eq!(polling.following(), 0);
    provider
        .forward_messages(&account(), &[item("c1", "+584245550199")])
        .await
        .unwrap();
    assert_eq!(polling.following(), 1);
}
