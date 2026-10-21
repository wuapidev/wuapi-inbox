//! Incoming webhooks against a mock API on localhost: the ones of a chat
//! picked from the key's list, made, paused, given a new URL and deleted,
//! and a deployment without the routes.

use super::{on, provider, reply, requests, target, ACCOUNT};
use client_provider::{AccountId, ChatId, IncomingWebhookChange, Provider, ProviderError};
use wiremock::MockServer;

const CHAT: &str = "+584245550199";
const ROUTE: &str = "/v1/incoming-webhooks";

fn hook(
    id: &str,
    account: &str,
    to: &str,
    change: impl FnOnce(&mut serde_json::Value),
) -> serde_json::Value {
    let mut wire = serde_json::json!({
        "object": "incoming_webhook", "id": id, "accountId": account, "projectId": null,
        "name": "Signups", "to": to, "enabled": true,
        "url": format!("https://api.wuapi.dev/hooks/{id}-secret"),
        "createdAt": "2026-10-12T10:00:00.000Z", "updatedAt": "2026-10-12T10:00:00.000Z",
    });
    change(&mut wire);
    wire
}

fn page(items: Vec<serde_json::Value>, next: Option<&str>) -> String {
    serde_json::json!({"object": "list", "items": items, "nextCursor": next}).to_string()
}

fn body(request: &wiremock::Request) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn the_hooks_of_a_chat_are_picked_from_every_page_of_the_key() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        ROUTE,
        vec![
            reply(
                200,
                &page(
                    vec![
                        hook("ih_1", ACCOUNT, CHAT, |_| {}),
                        // Another chat of the same number.
                        hook("ih_2", ACCOUNT, "+584245550100", |_| {}),
                    ],
                    Some("next"),
                ),
            ),
            reply(
                200,
                &page(
                    vec![
                        // The same chat, from another number.
                        hook("ih_3", "acc_other", CHAT, |_| {}),
                        hook("ih_4", ACCOUNT, CHAT, |wire| wire["enabled"] = false.into()),
                    ],
                    None,
                ),
            ),
        ],
    )
    .await;

    let hooks = provider(&server)
        .list_incoming_webhooks(&AccountId::new(ACCOUNT), &ChatId::new(CHAT))
        .await
        .unwrap();

    let ids: Vec<_> = hooks.iter().map(|hook| hook.id.as_str()).collect();
    assert_eq!(ids, ["ih_1", "ih_4"]);
    assert_eq!(hooks[0].url, "https://api.wuapi.dev/hooks/ih_1-secret");
    assert_eq!(hooks[0].to, ChatId::new(CHAT));
    assert!(hooks[0].enabled && !hooks[1].enabled);
    let asked: Vec<_> = requests(&server).await.iter().map(target).collect();
    assert_eq!(
        asked,
        [
            "/v1/incoming-webhooks?limit=100",
            "/v1/incoming-webhooks?limit=100&cursor=next"
        ]
    );
}

#[tokio::test]
async fn a_hook_is_made_for_the_chat_with_an_idempotency_key() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        ROUTE,
        vec![reply(201, &hook("ih_1", ACCOUNT, CHAT, |_| {}).to_string())],
    )
    .await;

    let made = provider(&server)
        .create_incoming_webhook(&AccountId::new(ACCOUNT), &ChatId::new(CHAT), "Signups")
        .await
        .unwrap();

    assert_eq!(made.id, "ih_1");
    let sent = &requests(&server).await[0];
    assert_eq!(
        body(sent),
        serde_json::json!({"accountId": ACCOUNT, "name": "Signups", "to": CHAT})
    );
    assert!(super::header(sent, "idempotency-key").is_some());
}

#[tokio::test]
async fn a_change_sends_one_field_and_answers_the_hook_as_it_is_now() {
    let server = MockServer::start().await;
    let at = format!("{ROUTE}/ih_1");
    on(
        &server,
        "PATCH",
        &at,
        vec![
            reply(
                200,
                &hook("ih_1", ACCOUNT, CHAT, |wire| wire["enabled"] = false.into()).to_string(),
            ),
            reply(
                200,
                &hook("ih_1", ACCOUNT, CHAT, |wire| {
                    wire["url"] = "https://api.wuapi.dev/hooks/new".into()
                })
                .to_string(),
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let account = AccountId::new(ACCOUNT);

    let paused = provider
        .update_incoming_webhook(&account, "ih_1", IncomingWebhookChange::Enabled(false))
        .await
        .unwrap();
    let rotated = provider
        .update_incoming_webhook(&account, "ih_1", IncomingWebhookChange::RotateUrl)
        .await
        .unwrap();

    assert!(!paused.enabled);
    assert_eq!(rotated.url, "https://api.wuapi.dev/hooks/new");
    let sent: Vec<_> = requests(&server).await.iter().map(body).collect();
    assert_eq!(
        sent,
        [
            serde_json::json!({"enabled": false}),
            serde_json::json!({"rotateUrl": true})
        ]
    );
}

#[tokio::test]
async fn deleting_a_hook_that_is_gone_is_done() {
    let server = MockServer::start().await;
    on(
        &server,
        "DELETE",
        &format!("{ROUTE}/ih_1"),
        vec![reply(
            404,
            r#"{"code":"not_found","message":"Incoming webhook not found."}"#,
        )],
    )
    .await;

    provider(&server)
        .delete_incoming_webhook(&AccountId::new(ACCOUNT), "ih_1")
        .await
        .unwrap();
}

#[tokio::test]
async fn a_deployment_without_the_routes_does_not_have_them() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        ROUTE,
        vec![reply(
            404,
            r#"{"code":"not_found","message":"Route not found."}"#,
        )],
    )
    .await;

    let answer = provider(&server)
        .list_incoming_webhooks(&AccountId::new(ACCOUNT), &ChatId::new(CHAT))
        .await;

    assert!(
        matches!(answer, Err(ProviderError::Unsupported(_))),
        "{answer:?}"
    );
}

#[tokio::test]
async fn too_many_hooks_is_a_refusal_the_user_can_read() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        ROUTE,
        vec![reply(400, r#"{"code":"limit_reached","message":"An organization can have up to 50 incoming webhooks."}"#)],
    )
    .await;

    let answer = provider(&server)
        .create_incoming_webhook(&AccountId::new(ACCOUNT), &ChatId::new(CHAT), "One more")
        .await;

    match answer {
        Err(ProviderError::Rejected { code, message }) => {
            assert_eq!(code, "limit_reached");
            assert!(message.contains("50"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}
