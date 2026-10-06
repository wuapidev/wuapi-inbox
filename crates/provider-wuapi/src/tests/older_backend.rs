//! A backend older than the SDK: one that does not send the response
//! fields the API added since (`forwardedManyTimes`, `media.gifPlayback`,
//! `replyToStoryId`, `imageQuality`, `pinnedAt`) and has none of the newer
//! routes. Everything that worked before the SDK was raised still works,
//! and what is new says "not available yet".
//!
//! TEMPORARY(response-compat): remove with `compat.rs`.

use super::{config, parse, provider, reply, requests, target, ACCOUNT, KEY};
use crate::follow::Now;
use crate::provider::WuapiProvider;
use client_provider::{
    AccountId, ChatId, ClientMessageId, Feature, MessageContent, MessageId, NewStory,
    NewStoryContent, OutgoingContent, OutgoingMessage, Provider, ProviderError, ProviderEvent,
    StoryStyle,
};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer};
use wuapi::types as api;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

/// What the API has added to its answers since SDK 0.8.0.
const NEWER: [&str; 5] = [
    "forwardedManyTimes",
    "gifPlayback",
    "replyToStoryId",
    "imageQuality",
    "pinnedAt",
];

/// A fixture as a backend from before those fields answers it.
fn older(json: &str) -> String {
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            serde_json::Value::Object(map) => {
                for field in NEWER {
                    map.remove(field);
                }
                map.values_mut().for_each(strip);
            }
            _ => {}
        }
    }
    let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
    strip(&mut value);
    value.to_string()
}

async fn always(server: &MockServer, verb: &str, at: &str, body: String, status: u16) {
    Mock::given(method(verb))
        .and(path(at))
        .respond_with(reply(status, &body))
        .mount(server)
        .await;
}

const NO_ROUTE: &str = r#"{"code":"not_found","message":"Route not found."}"#;

/// The API as it was: its answers without the newer fields, and "Route
/// not found." for every route that came since.
async fn older_api() -> MockServer {
    let server = MockServer::start().await;
    // A connected account: only those are asked for their chats.
    let accounts = format!(
        r#"{{"object":"list","items":[{}],"nextCursor":null}}"#,
        fixture!("account")
    );
    always(&server, "GET", "/v1/accounts", older(&accounts), 200).await;
    always(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}"),
        older(fixture!("account")),
        200,
    )
    .await;
    // The chats with their last messages, as one page.
    let chats = {
        let mut list: serde_json::Value = serde_json::from_str(fixture!("chat_list")).unwrap();
        list["nextCursor"] = serde_json::Value::Null;
        list.to_string()
    };
    always(
        &server,
        "GET",
        &format!("/v1/accounts/{ACCOUNT}/chats"),
        older(&chats),
        200,
    )
    .await;
    always(
        &server,
        "GET",
        "/v1/messages",
        older(fixture!("message_list_last")),
        200,
    )
    .await;
    always(
        &server,
        "POST",
        "/v1/messages",
        older(fixture!("message_sent_by_client")),
        202,
    )
    .await;
    always(
        &server,
        "GET",
        "/v1/messages/m_group_image",
        older(fixture!("message_group_image")),
        200,
    )
    .await;
    always(
        &server,
        "POST",
        "/v1/messages/m_poll/vote",
        older(fixture!("message_poll")),
        200,
    )
    .await;
    always(
        &server,
        "PATCH",
        "/v1/messages/m_text",
        older(fixture!("message")),
        200,
    )
    .await;
    always(
        &server,
        "POST",
        "/v1/messages/m_text/star",
        older(fixture!("message")),
        200,
    )
    .await;
    let story = {
        let list: api::MessageList = parse(fixture!("message_list"));
        let story = list
            .items
            .into_iter()
            .find(|message| message.chat_type == api::ChatType::Story)
            .unwrap();
        older(&serde_json::to_string(&story).unwrap())
    };
    always(
        &server,
        "POST",
        &format!("/v1/accounts/{ACCOUNT}/stories"),
        story,
        202,
    )
    .await;
    // Everything that came with 0.12.0 is not there.
    for route in [
        r"^/v1/messages/[^/]+/forward$",
        r"^/v1/accounts/[^/]+/stickers/favorites.*$",
        r"^/v1/accounts/[^/]+/stories/.+$",
    ] {
        Mock::given(path_regex(route))
            .respond_with(reply(404, NO_ROUTE))
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path(format!("/v1/accounts/{ACCOUNT}/stories")))
        .respond_with(reply(404, NO_ROUTE))
        .mount(&server)
        .await;
    server
}

/// Why this is needed at all: the SDK's own types refuse those answers.
#[test]
fn the_sdk_cannot_read_an_answer_from_before_a_field_it_requires() {
    let message = older(fixture!("message"));
    let refused = serde_json::from_str::<api::Message>(&message).unwrap_err();
    assert!(
        refused.to_string().contains("forwardedManyTimes"),
        "{refused}"
    );
    let media = older(fixture!("message_group_image"));
    let mut with_flag: serde_json::Value = serde_json::from_str(&media).unwrap();
    with_flag["forwardedManyTimes"] = false.into();
    let refused = serde_json::from_value::<api::Message>(with_flag).unwrap_err();
    assert!(refused.to_string().contains("gifPlayback"), "{refused}");
    let refused = serde_json::from_str::<api::Account>(&older(fixture!("account"))).unwrap_err();
    assert!(refused.to_string().contains("imageQuality"), "{refused}");

    // Read leniently, they are what their absence means.
    let read: crate::compat::Lenient<api::Message> = serde_json::from_str(&media).unwrap();
    assert!(!read.0.forwarded_many_times);
    assert!(!read.0.media.as_ref().unwrap().gif_playback);
    assert_eq!(read.0.reply_to_story_id, None);
    let read: crate::compat::Lenient<api::Account> =
        serde_json::from_str(&older(fixture!("account"))).unwrap();
    assert_eq!(read.0.image_quality, api::ImageQualitySetting::Standard);
    // A field that is there is left as it is.
    let mut value: serde_json::Value = serde_json::from_str(fixture!("message")).unwrap();
    value["forwardedManyTimes"] = true.into();
    let read: crate::compat::Lenient<api::Message> = serde_json::from_value(value).unwrap();
    assert!(read.0.forwarded_many_times);
}

/// A group as a backend from before communities had a parent answers
/// it: without `communityId` and `default`.
fn before_communities(json: &str) -> String {
    fn strip(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Array(items) => items.iter_mut().for_each(strip),
            serde_json::Value::Object(map) => {
                if map.get("object").and_then(|object| object.as_str()) == Some("group") {
                    map.remove("communityId");
                    map.remove("default");
                }
                map.values_mut().for_each(strip);
            }
            _ => {}
        }
    }
    let mut value: serde_json::Value = serde_json::from_str(json).unwrap();
    strip(&mut value);
    value.to_string()
}

/// Groups are read, listed and created against a backend that does not
/// say which community a group is linked to: it is then in none.
#[tokio::test]
async fn groups_are_still_read_from_a_backend_from_before_communities() {
    let one = before_communities(fixture!("group"));
    let refused = serde_json::from_str::<api::Group>(&one).unwrap_err();
    assert!(refused.to_string().contains("default"), "{refused}");

    let server = MockServer::start().await;
    let group = "120363041234567890@g.us";
    let groups = format!("/v1/accounts/{ACCOUNT}/groups");
    let at = format!("{groups}/{}", group.replace('@', "%40"));
    always(&server, "GET", &at, one.clone(), 200).await;
    always(
        &server,
        "GET",
        &groups,
        before_communities(fixture!("group_list")),
        200,
    )
    .await;
    always(&server, "POST", &groups, one.clone(), 201).await;
    always(&server, "PATCH", &at, one, 200).await;
    let provider = provider(&server);
    let account = AccountId::new(ACCOUNT);

    let read = provider
        .fetch_group(&account, &ChatId::new(group))
        .await
        .unwrap();
    assert_eq!(read.subject, "Launch team");
    assert_eq!((read.community_id, read.announcements), (None, false));
    let listed = provider.list_groups(&account).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].community_id, None);
    let made = provider
        .create_group(
            &account,
            &client_provider::NewGroup {
                subject: "Launch team".into(),
                participants: vec![client_provider::ContactId::new("+584245550199")],
                request_id: "req-1".into(),
                community: false,
                in_community: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(made.id.as_str(), group);
    provider
        .update_group(
            &account,
            &ChatId::new(group),
            &client_provider::GroupChange::Subject("Launch crew".into()),
        )
        .await
        .unwrap();

    // The requests are the ones the SDK's own methods make.
    let asked: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|request| format!("{} {}", request.method, target(request)))
        .collect();
    assert_eq!(
        asked,
        [
            format!("GET {at}"),
            format!("GET {groups}?limit=100"),
            format!("POST {groups}"),
            format!("PATCH {at}"),
        ]
    );
}

/// Accounts, chats, history, sends, votes, edits, stars, posting a story:
/// all of it goes on against the older backend.
#[tokio::test]
async fn everything_that_worked_before_still_works_against_an_older_backend() {
    let server = older_api().await;
    let provider = provider(&server);
    let account = AccountId::new(ACCOUNT);
    let chat = ChatId::new("+584245550199");

    let accounts = provider.list_accounts().await.unwrap();
    assert!(!accounts.is_empty());
    provider.link_status(&account).await.unwrap();
    let chats = provider.list_chats(&account, None).await.unwrap();
    assert!(!chats.items.is_empty());
    assert!(
        chats.items.iter().any(|chat| chat.last_message.is_some()),
        "a chat's last message is read too"
    );
    assert!(chats.items.iter().all(|chat| chat.pinned_at.is_none()));
    let history = provider
        .fetch_messages(&account, &chat, None, 50)
        .await
        .unwrap();
    assert!(!history.items.is_empty());
    assert!(history
        .items
        .iter()
        .all(|message| !message.extras.forwarded_many && message.extras.story_reply.is_none()));

    let text = OutgoingMessage {
        client_id: ClientMessageId::new("c0ffee"),
        account_id: account.clone(),
        chat_id: chat.clone(),
        content: OutgoingContent::Text {
            body: "hello".into(),
        },
        reply_to: None,
        mentions: Vec::new(),
        forwarded: false,
    };
    let receipt = provider.send(text.clone()).await.unwrap();
    assert!(!receipt.message_id.as_str().is_empty());
    let file = OutgoingMessage {
        content: OutgoingContent::Media {
            kind: client_provider::MediaKind::Image,
            media: client_provider::MediaRef::new("wuapi-upload:up_1"),
            mime_type: Some("image/jpeg".into()),
            caption: None,
            file_name: None,
            gif: false,
        },
        ..text
    };
    provider.send(file).await.unwrap();

    let voted = provider
        .vote_poll(
            &account,
            &chat,
            &MessageId::new("m_poll"),
            &["Sushi".into()],
        )
        .await
        .unwrap();
    assert!(matches!(
        voted.map(|message| message.content),
        Some(MessageContent::Poll(_))
    ));
    provider
        .edit_message(&account, &chat, &MessageId::new("m_text"), "new words")
        .await
        .unwrap();
    provider
        .star_message(&account, &chat, &MessageId::new("m_text"), true)
        .await
        .unwrap();
    let story = provider
        .post_story(NewStory {
            client_id: ClientMessageId::new("story-1"),
            account_id: account.clone(),
            content: NewStoryContent::Text {
                text: "Hello".into(),
                style: StoryStyle::default(),
            },
        })
        .await
        .unwrap();
    assert!(story.mine);

    // The requests are the ones the SDK's own methods make.
    let asked: Vec<String> = requests(&server)
        .await
        .iter()
        .map(|request| format!("{} {}", request.method, target(request)))
        .collect();
    assert_eq!(
        asked,
        [
            "GET /v1/accounts?limit=100".to_owned(),
            format!("GET /v1/accounts/{ACCOUNT}"),
            format!("GET /v1/accounts/{ACCOUNT}/chats?limit=100"),
            format!("GET /v1/messages?accountId={ACCOUNT}&chatId=%2B584245550199&limit=50"),
            "POST /v1/messages".to_owned(),
            "POST /v1/messages".to_owned(),
            "POST /v1/messages/m_poll/vote".to_owned(),
            "PATCH /v1/messages/m_text".to_owned(),
            "POST /v1/messages/m_text/star".to_owned(),
            format!("POST /v1/accounts/{ACCOUNT}/stories"),
        ]
    );
}

/// What is new says "not available yet" against it, each part on its
/// own, and the poll keeps bringing messages and chats.
#[tokio::test]
async fn what_is_new_says_not_yet_against_an_older_backend_and_the_poll_goes_on() {
    let server = older_api().await;
    let (provider, polling) = WuapiProvider::with_polling(config(&server), KEY.into()).unwrap();
    let account = AccountId::new(ACCOUNT);
    let unsupported =
        |result: Result<(), ProviderError>| matches!(result, Err(ProviderError::Unsupported(_)));

    let item = client_provider::ForwardItem {
        client_id: ClientMessageId::new("f1"),
        message: MessageId::new("m_group_image"),
        to: ChatId::new("+584245550199"),
    };
    assert!(unsupported(
        provider
            .forward_messages(&account, &[item])
            .await
            .map(|_| ())
    ));
    assert!(unsupported(
        provider.list_favorite_stickers(&account).await.map(|_| ())
    ));
    // The own stories are read the old way; contacts' are none.
    let stories = provider.list_stories(&account).await.unwrap();
    assert!(stories.iter().all(|story| story.mine));
    for feature in [
        Feature::ForwardAny,
        Feature::StickerFavorites,
        Feature::Stories,
    ] {
        assert!(provider.unavailable(&account, feature), "{feature:?}");
    }

    // The poll reads accounts, messages and chats as it did, and leaves
    // the stories alone.
    let mut poller = polling.poller();
    let before = requests(&server).await.len();
    poller.poll(Now::real()).await.unwrap();
    let news = poller.poll(Now::real()).await.unwrap();
    assert!(news
        .iter()
        .all(|event| !matches!(event, ProviderEvent::StoryUpserted(_))));
    let asked: Vec<String> = requests(&server).await[before..]
        .iter()
        .map(target)
        .collect();
    assert!(
        asked.iter().all(|target| !target.contains("/stories")),
        "{asked:?}"
    );
    assert!(asked.iter().any(|target| target.contains("/chats")));
}
