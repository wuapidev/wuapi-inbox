//! Editing, deleting, starring and forwarding: the requests, and what
//! wuapi's refusals become.

use super::*;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

#[tokio::test]
async fn an_edit_is_a_patch_and_whatsapps_refusal_is_not_retried() {
    let server = MockServer::start().await;
    on(
        &server,
        "PATCH",
        "/v1/messages/m%2F1",
        vec![
            reply(200, fixture!("message")),
            reply(
                409,
                r#"{"code":"edit_window_closed","message":"WhatsApp no longer allows editing this message.","details":{}}"#,
            ),
            reply(503, r#"{"code":"unavailable","message":"Try again.","details":{}}"#),
        ],
    )
    .await;
    let provider = provider(&server);
    let (account, chat, message) = (
        AccountId::new(ACCOUNT),
        ChatId::new("+584245550199"),
        MessageId::new("m/1"),
    );
    assert!(provider.capabilities().edits);
    provider
        .edit_message(&account, &chat, &message, "the new text")
        .await
        .unwrap();
    let request = &requests(&server).await[0];
    assert_eq!(request.method.as_str(), "PATCH");
    assert_eq!(target(request), "/v1/messages/m%2F1");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
        serde_json::json!({ "text": "the new text" })
    );

    // WhatsApp's edit window has closed: a refusal, with its words.
    let refused = provider
        .edit_message(&account, &chat, &message, "too late")
        .await
        .unwrap_err();
    assert!(!refused.is_transient(), "{refused:?}");
    assert!(
        refused.to_string().contains("no longer allows"),
        "{refused}"
    );
    // The API being down is weather.
    let down = provider
        .edit_message(&account, &chat, &message, "again")
        .await
        .unwrap_err();
    assert!(down.is_transient(), "{down:?}");
}

#[tokio::test]
async fn a_delete_says_whether_it_is_for_everyone() {
    let server = MockServer::start().await;
    on(
        &server,
        "DELETE",
        "/v1/messages/m1",
        vec![ResponseTemplate::new(204), ResponseTemplate::new(204)],
    )
    .await;
    let provider = provider(&server);
    let (account, chat, message) = (
        AccountId::new(ACCOUNT),
        ChatId::new("+584245550199"),
        MessageId::new("m1"),
    );
    let caps = provider.capabilities();
    assert!(caps.deletes && caps.delete_for_me);
    assert!(
        !caps.delete_received,
        "the API deletes outbound messages only"
    );
    provider
        .delete_message(&account, &chat, &message, true)
        .await
        .unwrap();
    provider
        .delete_message(&account, &chat, &message, false)
        .await
        .unwrap();
    let requests = requests(&server).await;
    assert_eq!(requests[0].method.as_str(), "DELETE");
    assert_eq!(target(&requests[0]), "/v1/messages/m1?forEveryone=true");
    assert_eq!(target(&requests[1]), "/v1/messages/m1?forEveryone=false");
}

#[tokio::test]
async fn a_star_and_its_removal_are_two_endpoints() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/v1/messages/m1/star",
        vec![reply(200, fixture!("message"))],
    )
    .await;
    on(
        &server,
        "POST",
        "/v1/messages/m1/unstar",
        vec![reply(200, fixture!("message"))],
    )
    .await;
    let provider = provider(&server);
    let (account, chat, message) = (
        AccountId::new(ACCOUNT),
        ChatId::new("+584245550199"),
        MessageId::new("m1"),
    );
    assert!(provider.capabilities().stars);
    provider
        .star_message(&account, &chat, &message, true)
        .await
        .unwrap();
    provider
        .star_message(&account, &chat, &message, false)
        .await
        .unwrap();
    let requests = requests(&server).await;
    assert_eq!(target(&requests[0]), "/v1/messages/m1/star");
    assert_eq!(target(&requests[1]), "/v1/messages/m1/unstar");
}

#[tokio::test]
async fn a_forward_is_a_send_with_the_forwarded_mark() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/v1/messages",
        vec![reply(202, fixture!("message"))],
    )
    .await;
    let provider = provider(&server);
    assert!(provider.capabilities().forwards);
    provider
        .send(OutgoingMessage {
            client_id: ClientMessageId::new("f1"),
            account_id: AccountId::new(ACCOUNT),
            chat_id: ChatId::new("+584245550199"),
            content: OutgoingContent::Text {
                body: "passed on".into(),
            },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: true,
        })
        .await
        .unwrap();
    let request = &requests(&server).await[0];
    assert_eq!(header(request, "idempotency-key"), Some("f1"));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
        serde_json::json!({
            "accountId": ACCOUNT,
            "to": "+584245550199",
            "type": "text",
            "text": "passed on",
            "forwarded": true,
            "metadata": { "clientMessageId": "f1" },
        })
    );
}
