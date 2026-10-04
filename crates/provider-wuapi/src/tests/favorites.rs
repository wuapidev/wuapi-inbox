//! Favorite stickers against a mock API on localhost: the list a page at
//! a time, a file on demand with the key kept to the API, starring by
//! message or by upload, and "not available yet for this number".

use super::{header, on, provider, reply, requests, target, ACCOUNT, KEY};
use crate::availability::RECHECK;
use client_provider::{
    AccountId, Feature, MediaLimit, MediaRef, MessageId, Provider, ProviderError, StickerFile,
};
use std::sync::Arc;
use wiremock::MockServer;

fn account() -> AccountId {
    AccountId::new(ACCOUNT)
}

fn route() -> String {
    format!("/v1/accounts/{ACCOUNT}/stickers/favorites")
}

fn favorite(id: &str, change: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value {
    let mut wire = serde_json::json!({
        "object": "favorite_sticker", "id": id, "accountId": ACCOUNT,
        "mimeType": "image/webp", "animated": null, "lottie": false,
        "width": 512, "height": 512, "size": 20480, "emojis": null,
        "favoritedAt": "2026-09-30T10:00:00.000Z",
        "media": {
            "url": format!("https://api.wuapi.dev/v1/accounts/{ACCOUNT}/stickers/favorites/{id}/media"),
            "downloaded": false,
        },
    });
    change(&mut wire);
    wire
}

fn page(items: Vec<serde_json::Value>, next: Option<&str>) -> String {
    serde_json::json!({"object": "list", "items": items, "nextCursor": next}).to_string()
}

fn sticker(message: Option<&str>) -> StickerFile {
    StickerFile {
        id: "hash-of-the-file".into(),
        bytes: Arc::new(b"RIFF....WEBP".to_vec()),
        mime_type: "image/webp".into(),
        message: message.map(MessageId::new),
    }
}

const NOT_SUPPORTED: &str = r#"{"code":"not_supported","message":"Favorite stickers are not turned on for this account yet."}"#;
const NO_ROUTE: &str = r#"{"code":"not_found","message":"Route not found."}"#;

fn missing(provider: &crate::provider::WuapiProvider) -> bool {
    provider.unavailable(&account(), Feature::StickerFavorites)
}

fn body(request: &wiremock::Request) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null)
}

#[tokio::test]
async fn the_favorites_are_listed_a_page_at_a_time_and_name_their_files() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &route(),
        vec![
            reply(
                200,
                &page(
                    vec![
                        favorite("stk_1", |_| {}),
                        // A vector sticker cannot be drawn here.
                        favorite("stk_lottie", |wire| {
                            wire["lottie"] = true.into();
                            wire["mimeType"] = "application/was".into();
                        }),
                    ],
                    Some("next"),
                ),
            ),
            reply(
                200,
                &page(
                    vec![
                        // Stored already: asked for through its route all
                        // the same, because its file goes with the star.
                        favorite("stk_2", |wire| {
                            wire["media"] = serde_json::json!({
                                "url": "https://files.example/stk_2.webp", "downloaded": true,
                            });
                            wire["width"] = serde_json::Value::Null;
                            wire["height"] = serde_json::Value::Null;
                            wire["size"] = serde_json::Value::Null;
                        }),
                        // WhatsApp no longer has its file. It is still a
                        // favorite on the phone, so it is still listed.
                        favorite("stk_gone", |wire| {
                            wire["media"]["url"] = serde_json::Value::Null;
                        }),
                    ],
                    None,
                ),
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    assert!(provider.capabilities().sticker_favorites);
    let listed = provider.list_favorite_stickers(&account()).await.unwrap();
    let ids: Vec<&str> = listed.iter().map(|sticker| sticker.id.as_str()).collect();
    assert_eq!(ids, ["stk_1", "stk_2", "stk_gone"]);
    assert_eq!(
        listed[0].source,
        MediaRef::new("wuapi-favorite:20480:stk_1"),
        "the favorite is named, not the API's URL"
    );
    assert_eq!((listed[0].width, listed[0].height), (Some(512), Some(512)));
    assert_eq!(listed[0].size_bytes, Some(20480));
    assert_eq!(listed[0].mime_type.as_deref(), Some("image/webp"));
    assert_eq!(listed[1].source, MediaRef::new("wuapi-favorite::stk_2"));
    assert_eq!((listed[1].width, listed[1].height), (None, None));
    let asked: Vec<String> = requests(&server).await.iter().map(target).collect();
    assert_eq!(
        asked,
        [
            format!("{}?limit=100", route()),
            format!("{}?limit=100&cursor=next", route()),
        ],
        "the list, and not one file"
    );
    assert!(!missing(&provider));
}

#[tokio::test]
async fn a_favorites_file_is_fetched_when_wanted_and_the_key_stays_with_the_api() {
    let server = MockServer::start().await;
    let storage = MockServer::start().await;
    on(
        &server,
        "GET",
        &format!("{}/stk_1/media", route()),
        vec![
            reply(
                200,
                &serde_json::json!({
                    "object": "media", "stickerId": "stk_1",
                    "url": format!("{}/files/stk_1.webp", storage.uri()),
                    "mimeType": "image/webp", "size": 4,
                })
                .to_string(),
            ),
            reply(
                410,
                r#"{"code":"media_expired","message":"WhatsApp no longer has this sticker's file."}"#,
            ),
        ],
    )
    .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/files/stk_1.webp"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_raw(vec![9u8, 8, 7, 6], "image/webp"),
        )
        .mount(&storage)
        .await;
    let provider = provider(&server);
    let limit = MediaLimit {
        max_bytes: 2 * 1024 * 1024,
        images_only: true,
    };
    let reference = MediaRef::new("wuapi-favorite:4:stk_1");
    let data = provider
        .fetch_media(&account(), &reference, limit)
        .await
        .unwrap();
    assert_eq!(data.bytes, [9, 8, 7, 6]);
    let api = requests(&server).await;
    assert_eq!(
        target(&api[0]),
        format!("{}/stk_1/media?redirect=false", route())
    );
    assert_eq!(
        header(&api[0], "authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    let fetched = requests(&storage).await;
    assert_eq!(fetched.len(), 1);
    assert_eq!(header(&fetched[0], "authorization"), None);
    for (name, value) in fetched[0].headers.iter() {
        assert!(
            !value.to_str().unwrap_or_default().contains(KEY),
            "the key travelled to the file host in `{name}`"
        );
    }
    // A file WhatsApp let go is terminal, and says so. The favorite stays
    // listed (it is still starred on the phone), so it is asked for once
    // and not again.
    for _ in 0..3 {
        let gone = provider
            .fetch_media(&account(), &reference, limit)
            .await
            .unwrap_err();
        assert!(!gone.is_transient());
        assert_eq!(gone.to_string(), "WhatsApp no longer has this file.");
    }
    assert_eq!(
        requests(&server).await.len(),
        2,
        "the file, then the refusal"
    );
}

#[tokio::test]
async fn a_sticker_seen_in_a_message_is_starred_by_naming_the_message() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &route(),
        vec![
            reply(
                503,
                r#"{"code":"engine_unavailable","message":"Retry shortly."}"#,
            ),
            reply(201, &favorite("stk_new", |_| {}).to_string()),
        ],
    )
    .await;
    let provider = provider(&server);
    // A drop first: the identical request goes again.
    assert!(provider
        .add_favorite_sticker(&account(), sticker(Some("m_sticker")))
        .await
        .unwrap_err()
        .is_transient());
    let listed_as = provider
        .add_favorite_sticker(&account(), sticker(Some("m_sticker")))
        .await
        .unwrap();
    assert_eq!(listed_as, "stk_new", "the id the API lists it under");
    let asked = requests(&server).await;
    assert_eq!(asked.len(), 2, "no upload: no file travelled");
    assert_eq!(
        body(&asked[0]),
        serde_json::json!({"messageId": "m_sticker"})
    );
    assert_eq!(body(&asked[0]), body(&asked[1]));
    assert_eq!(
        header(&asked[0], "idempotency-key"),
        header(&asked[1], "idempotency-key")
    );
    assert!(header(&asked[0], "idempotency-key").is_some_and(|key| !key.is_empty()));
}

#[tokio::test]
async fn a_sticker_from_nowhere_or_whose_message_is_gone_is_starred_by_its_file() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &route(),
        vec![
            // The message is not one the API holds.
            reply(
                404,
                r#"{"code":"not_found","message":"Message not found.","details":{"field":"messageId"}}"#,
            ),
            reply(201, &favorite("stk_up", |_| {}).to_string()),
            reply(201, &favorite("stk_up", |_| {}).to_string()),
        ],
    )
    .await;
    on(
        &server,
        "POST",
        "/v1/uploads",
        vec![
            reply(
                201,
                r#"{"object":"upload","id":"up_s","projectId":null,"status":"ready","mimeType":"image/webp","filename":null,"size":12,"uploadUrl":null,"expiresAt":"2026-10-02T04:00:00.000Z","createdAt":"2026-10-01T04:00:00.000Z"}"#,
            ),
            reply(
                201,
                r#"{"object":"upload","id":"up_s","projectId":null,"status":"ready","mimeType":"image/webp","filename":null,"size":12,"uploadUrl":null,"expiresAt":"2026-10-02T04:00:00.000Z","createdAt":"2026-10-01T04:00:00.000Z"}"#,
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let first = provider
        .add_favorite_sticker(&account(), sticker(Some("m_gone")))
        .await
        .unwrap();
    assert_eq!(first, "stk_up");
    // One the client made or imported: by its file from the start.
    let second = provider
        .add_favorite_sticker(&account(), sticker(None))
        .await
        .unwrap();
    assert_eq!(second, "stk_up", "starring twice is one star");

    let asked = requests(&server).await;
    let seen: Vec<(String, serde_json::Value)> = asked
        .iter()
        .map(|request| (target(request), body(request)))
        .collect();
    assert_eq!(seen.len(), 5);
    assert_eq!(
        seen[0],
        (route(), serde_json::json!({"messageId": "m_gone"}))
    );
    assert_eq!(seen[1].0, "/v1/uploads");
    assert_eq!(seen[1].1["mimeType"], "image/webp");
    assert!(seen[1].1["base64"].is_string());
    assert_eq!(seen[2], (route(), serde_json::json!({"uploadId": "up_s"})));
    assert_eq!(seen[3].0, "/v1/uploads");
    assert_eq!(seen[4], (route(), serde_json::json!({"uploadId": "up_s"})));
    // The same file is the same upload and the same request, whenever.
    assert_eq!(
        header(&asked[1], "idempotency-key"),
        header(&asked[3], "idempotency-key")
    );
    assert_eq!(
        header(&asked[2], "idempotency-key"),
        header(&asked[4], "idempotency-key")
    );
    // Naming the message and sending the file are two requests: they
    // never share a key (the API would answer a conflict).
    assert_ne!(
        header(&asked[0], "idempotency-key"),
        header(&asked[2], "idempotency-key")
    );
}

#[tokio::test]
async fn a_star_is_taken_off_and_one_already_gone_is_done() {
    let server = MockServer::start().await;
    on(
        &server,
        "DELETE",
        &format!("{}/stk_1", route()),
        vec![
            wiremock::ResponseTemplate::new(204),
            reply(
                404,
                r#"{"code":"not_found","message":"Favorite not found."}"#,
            ),
            reply(
                409,
                r#"{"code":"account_not_ready","message":"Reconnecting."}"#,
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    provider
        .remove_favorite_sticker(&account(), "stk_1")
        .await
        .unwrap();
    provider
        .remove_favorite_sticker(&account(), "stk_1")
        .await
        .unwrap();
    assert!(provider
        .remove_favorite_sticker(&account(), "stk_1")
        .await
        .unwrap_err()
        .is_transient());
    assert!(!missing(&provider));
}

/// The engine has favorites turned off for this number: the list is
/// empty, a change answers `400 not_supported`. That is "not available
/// yet for this number": said once, not asked again for ten minutes, and
/// never an error of the sticker.
#[tokio::test]
async fn a_number_without_favorites_says_not_yet_and_is_asked_again_later() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &route(),
        vec![
            reply(200, &page(Vec::new(), None)),
            reply(200, &page(Vec::new(), None)),
        ],
    )
    .await;
    on(
        &server,
        "POST",
        &route(),
        vec![
            reply(400, NOT_SUPPORTED),
            reply(201, &favorite("stk_on", |_| {}).to_string()),
        ],
    )
    .await;
    on(
        &server,
        "DELETE",
        &format!("{}/stk_1", route()),
        vec![reply(400, NOT_SUPPORTED)],
    )
    .await;
    let provider = provider(&server);
    // Reading works and finds nothing: nothing is known yet.
    assert!(provider
        .list_favorite_stickers(&account())
        .await
        .unwrap()
        .is_empty());
    assert!(!missing(&provider));

    let refused = provider
        .add_favorite_sticker(&account(), sticker(Some("m_sticker")))
        .await;
    assert!(
        matches!(refused, Err(ProviderError::Unsupported(_))),
        "{refused:?}"
    );
    assert!(missing(&provider));
    assert!(
        !provider.unavailable(&AccountId::new("another"), Feature::StickerFavorites),
        "it is this number that does not have them"
    );
    assert!(!provider.unavailable(&account(), Feature::ForwardAny));

    // Meanwhile: changes are not sent, the list is still read.
    let before = requests(&server).await.len();
    for result in [
        provider
            .add_favorite_sticker(&account(), sticker(None))
            .await
            .map(|_| ()),
        provider.remove_favorite_sticker(&account(), "stk_1").await,
    ] {
        assert!(
            matches!(result, Err(ProviderError::Unsupported(_))),
            "{result:?}"
        );
    }
    assert_eq!(requests(&server).await.len(), before, "nothing was asked");
    assert!(provider
        .list_favorite_stickers(&account())
        .await
        .unwrap()
        .is_empty());

    // Later it is asked again; by then the number has them.
    provider.client().missing.age(RECHECK);
    assert!(!missing(&provider));
    let listed_as = provider
        .add_favorite_sticker(&account(), sticker(Some("m_sticker")))
        .await
        .unwrap();
    assert_eq!(listed_as, "stk_on");
    assert!(!missing(&provider));

    // A removal learns it the same way.
    let other = MockServer::start().await;
    on(
        &other,
        "DELETE",
        &format!("{}/stk_1", route()),
        vec![reply(400, NOT_SUPPORTED)],
    )
    .await;
    let provider = super::provider(&other);
    let refused = provider.remove_favorite_sticker(&account(), "stk_1").await;
    assert!(matches!(refused, Err(ProviderError::Unsupported(_))));
    assert!(missing(&provider));
}

/// A backend from before the routes: every number, the list included.
#[tokio::test]
async fn a_deployment_without_the_favorites_routes_says_not_yet_for_every_number() {
    let server = MockServer::start().await;
    on(&server, "GET", &route(), vec![reply(404, NO_ROUTE)]).await;
    let provider = provider(&server);
    let listed = provider.list_favorite_stickers(&account()).await;
    assert!(
        matches!(listed, Err(ProviderError::Unsupported(_))),
        "{listed:?}"
    );
    assert!(missing(&provider));
    assert!(provider.unavailable(&AccountId::new("another"), Feature::StickerFavorites));
    // Not asked again, for any of it, until later.
    for result in [
        provider
            .list_favorite_stickers(&account())
            .await
            .map(|_| ()),
        provider
            .add_favorite_sticker(&account(), sticker(None))
            .await
            .map(|_| ()),
        provider.remove_favorite_sticker(&account(), "x").await,
    ] {
        assert!(matches!(result, Err(ProviderError::Unsupported(_))));
    }
    assert_eq!(requests(&server).await.len(), 1);
}
