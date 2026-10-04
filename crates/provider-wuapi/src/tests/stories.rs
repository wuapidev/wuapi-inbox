//! Stories against a mock API on localhost: posting, the account's own
//! stories and its contacts', their files on demand, the view receipt as
//! the one explicit call, viewers, replies and reactions, what a
//! deployment without the routes degrades to, and the proof that what the
//! API cannot do answers `Unsupported` with its capability off.

use super::{header, on, parse, provider, reply, requests, target, ACCOUNT, KEY};
use crate::availability::RECHECK;
use crate::events::PollState;
use crate::mapping;
use crate::provider::WuapiProvider;
use crate::stories;
use client_provider::{
    AccountId, ClientMessageId, ContactId, Feature, MediaKind, MediaLimit, MediaRef, MessageId,
    NewStory, NewStoryContent, OutgoingContent, OutgoingMessage, Provider, ProviderError,
    ProviderEvent, StoryAudience, StoryBody, StoryFont, StoryPrivacy, StoryStyle, Timestamp,
};
use wiremock::MockServer;
use wuapi::types as api;

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

fn account() -> AccountId {
    AccountId::new(ACCOUNT)
}

fn at(tail: &str) -> String {
    format!("/v1/accounts/{ACCOUNT}{tail}")
}

fn body(request: &wiremock::Request) -> serde_json::Value {
    serde_json::from_slice(&request.body).unwrap_or(serde_json::Value::Null)
}

/// The spec's story message: outbound, of the chat `stories`.
fn story_message() -> api::Message {
    let list: api::MessageList = parse(fixture!("message_list"));
    list.items
        .into_iter()
        .find(|message| message.chat_type == api::ChatType::Story)
        .expect("the example has a story")
}

fn story_json(id: &str, created: &str, kind: &str) -> String {
    let mut wire = story_message();
    wire.id = id.into();
    wire.created_at = created.into();
    wire.sent_at = Some(created.into());
    wire.status = api::MessageStatus::Sent;
    let mut json = serde_json::to_value(&wire).unwrap();
    json["type"] = kind.into();
    json.to_string()
}

fn page(items: &[String], next: Option<&str>) -> String {
    format!(
        r#"{{"object":"list","items":[{}],"nextCursor":{}}}"#,
        items.join(","),
        next.map_or("null".to_owned(), |cursor| format!("\"{cursor}\""))
    )
}

fn now_minus(minutes: i64) -> String {
    let at = chrono::Utc::now() - chrono::Duration::minutes(minutes);
    at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ----- mapping --------------------------------------------------------------

#[test]
fn an_outbound_story_message_is_the_accounts_own_story() {
    let wire = story_message();
    let story = stories::story_of_message(&wire).expect("a story");
    assert_eq!(story.id.as_str(), "m_story");
    assert!(story.mine && story.viewed);
    assert_eq!(story.author.as_str(), "+584121234567");
    assert_eq!(
        story.body,
        StoryBody::Text {
            text: "Your order has shipped.".into(),
            style: StoryStyle::default()
        }
    );
    assert_eq!(
        story.posted_at,
        Timestamp::from_millis(1_790_237_740_000),
        "WhatsApp's time, else the one the row was made"
    );
    // It expires a day after it was posted.
    assert_eq!(
        story.expiry().as_millis(),
        story.posted_at.as_millis() + 86_400_000
    );
    // Not a story: a direct message, and a story that failed.
    let list: api::MessageList = parse(fixture!("message_list"));
    assert!(list
        .items
        .iter()
        .filter(|message| message.chat_type != api::ChatType::Story)
        .all(|message| stories::story_of_message(message).is_none()));
    let mut failed = wire.clone();
    failed.status = api::MessageStatus::Failed;
    assert!(stories::story_of_message(&failed).is_none());
    let mut inbound = wire.clone();
    inbound.direction = api::MessageDirection::Inbound;
    assert!(stories::story_of_message(&inbound).is_none());
    // The chat list still shows no stories as a chat.
    assert!(mapping::message(&wire).is_none());
}

#[test]
fn the_audience_is_the_list_in_use_and_both_lists_are_kept() {
    let wire: api::StoryPrivacy = serde_json::from_str(
        r#"{"object":"story_privacy","lists":[
            {"type":"contacts","contactIds":[],"default":false},
            {"type":"blacklist","contactIds":["+584245550199","lid:77"],"default":true},
            {"type":"whitelist","contactIds":["+584140000009"],"default":false}]}"#,
    )
    .unwrap();
    let privacy = stories::privacy(&wire);
    assert_eq!(privacy.audience, StoryAudience::ContactsExcept);
    assert_eq!(
        privacy.except,
        [ContactId::new("+584245550199"), ContactId::new("lid:77")]
    );
    assert_eq!(privacy.only, [ContactId::new("+584140000009")]);
    assert_eq!(privacy.listed().len(), 2);

    let everyone: api::StoryPrivacy = serde_json::from_str(
        r#"{"object":"story_privacy","lists":[{"type":"contacts","contactIds":[],"default":true}]}"#,
    )
    .unwrap();
    assert_eq!(stories::privacy(&everyone), StoryPrivacy::default());
    let only: api::StoryPrivacy = serde_json::from_str(
        r#"{"object":"story_privacy","lists":[{"type":"whitelist","contactIds":["+1"],"default":true}]}"#,
    )
    .unwrap();
    assert_eq!(
        stories::privacy(&only).audience,
        StoryAudience::OnlyShareWith
    );
}

// ----- capabilities -----------------------------------------------------------

#[tokio::test]
async fn what_the_api_does_is_offered_and_the_rest_says_so() {
    let server = MockServer::start().await;
    let provider = provider(&server);
    let caps = provider.capabilities();
    assert!(caps.story_list && caps.story_post && caps.story_delete && caps.story_privacy);
    // Since SDK 0.12.0: contacts' stories and what is done with them.
    assert!(caps.story_contacts);
    assert!(caps.story_view && caps.story_viewers);
    assert!(caps.story_reply && caps.story_react);
    // Still not there: a mute cannot be written, the lists not changed.
    assert!(!caps.story_mute && !caps.story_privacy_edit);
    let account = account();
    let author = ContactId::new("+584245550199");
    let unsupported = |result: Result<(), ProviderError>| {
        assert!(
            matches!(result, Err(ProviderError::Unsupported(_))),
            "{result:?}"
        )
    };
    unsupported(provider.muted_story_authors(&account).await.map(|_| ()));
    unsupported(provider.set_story_muted(&account, &author, true).await);
    unsupported(
        provider
            .set_story_privacy(&account, &StoryPrivacy::default())
            .await,
    );
    // Not one of them touched the network.
    assert!(requests(&server).await.is_empty());
}

// ----- posting ------------------------------------------------------------------

#[tokio::test]
async fn a_text_story_is_posted_with_its_colour_font_and_the_client_id_as_the_key() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &at("/stories"),
        vec![reply(
            202,
            &story_json("m_new", &now_minus(0), "text")
                .replace("Your order has shipped.", "Hello, world"),
        )],
    )
    .await;
    let provider = provider(&server);
    let style = StoryStyle {
        background: 0x0B6E4F,
        font: StoryFont(7),
    };
    let new = NewStory {
        client_id: ClientMessageId::new("story-1"),
        account_id: account(),
        content: NewStoryContent::Text {
            text: "Hello, world".into(),
            style,
        },
    };
    let story = provider.post_story(new).await.unwrap();
    assert_eq!(story.id.as_str(), "m_new");
    assert_eq!(story.client_id, Some(ClientMessageId::new("story-1")));
    // The API's message has no style: the one asked for is kept.
    assert_eq!(
        story.body,
        StoryBody::Text {
            text: "Your order has shipped.".replace("Your order has shipped.", "Hello, world"),
            style
        }
    );
    let sent = requests(&server).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(target(&sent[0]), at("/stories"));
    assert_eq!(header(&sent[0], "idempotency-key"), Some("story-1"));
    assert_eq!(
        body(&sent[0]),
        serde_json::json!({
            "type": "text",
            "text": "Hello, world",
            "backgroundColor": "#0B6E4F",
            "font": 7
        })
    );
}

#[tokio::test]
async fn a_picture_story_refers_to_its_upload_and_a_missing_upload_is_refused() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &at("/stories"),
        vec![reply(202, &story_json("m_pic", &now_minus(0), "image"))],
    )
    .await;
    let provider = provider(&server);
    let new = |media: &str| NewStory {
        client_id: ClientMessageId::new("story-2"),
        account_id: account(),
        content: NewStoryContent::Media {
            kind: MediaKind::Image,
            media: MediaRef::new(media),
            mime_type: Some("image/jpeg".into()),
            caption: Some("The view".into()),
        },
    };
    let refused = provider.post_story(new("not an upload")).await.unwrap_err();
    assert!(matches!(refused, ProviderError::Rejected { .. }));
    assert!(requests(&server).await.is_empty());

    let story = provider
        .post_story(new("wuapi-upload:upl_123"))
        .await
        .unwrap();
    assert_eq!(story.client_id, Some(ClientMessageId::new("story-2")));
    let sent = requests(&server).await;
    assert_eq!(header(&sent[0], "idempotency-key"), Some("story-2"));
    assert_eq!(
        body(&sent[0]),
        serde_json::json!({
            "type": "image",
            "media": {"uploadId": "upl_123", "mimeType": "image/jpeg"},
            "text": "The view"
        })
    );
}

#[tokio::test]
async fn a_post_that_met_a_dropped_connection_is_transient_and_a_refusal_is_not() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &at("/stories"),
        vec![
            reply(502, r#"{"code":"bad_gateway","message":"try again"}"#),
            reply(
                400,
                r#"{"code":"no_recipients","message":"The story has no recipients."}"#,
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let new = NewStory {
        client_id: ClientMessageId::new("story-3"),
        account_id: account(),
        content: NewStoryContent::Text {
            text: "x".into(),
            style: StoryStyle::default(),
        },
    };
    assert!(provider
        .post_story(new.clone())
        .await
        .unwrap_err()
        .is_transient());
    let refused = provider.post_story(new).await.unwrap_err();
    assert!(!refused.is_transient());
    assert!(refused.to_string().contains("no recipients"));
}

// ----- the account's own stories ----------------------------------------------

/// A `story` as the story routes answer it.
fn story_wire(id: &str, change: impl FnOnce(&mut serde_json::Value)) -> serde_json::Value {
    let posted = now_minus(30);
    let mut wire = serde_json::json!({
        "object": "story", "id": id, "projectId": null, "accountId": ACCOUNT,
        "contactId": "+584245550199", "own": false, "profileName": "Maria", "username": null,
        "type": "text", "text": "Good morning", "backgroundColor": "#0B6E4F", "font": 7,
        "media": null, "status": "received", "viewedAt": null, "authorNotified": null,
        "reaction": null, "viewCount": null, "postedAt": posted,
        "expiresAt": now_minus(30 - 24 * 60), "deletedAt": null, "createdAt": posted,
    });
    change(&mut wire);
    wire
}

fn own_wire(id: &str, views: i64) -> serde_json::Value {
    story_wire(id, |wire| {
        wire["own"] = true.into();
        wire["contactId"] = "+584121234567".into();
        wire["profileName"] = serde_json::Value::Null;
        wire["status"] = "read".into();
        wire["viewCount"] = views.into();
    })
}

fn picture_wire(id: &str, downloaded: bool) -> serde_json::Value {
    story_wire(id, |wire| {
        wire["type"] = "image".into();
        wire["text"] = "The view".into();
        wire["backgroundColor"] = serde_json::Value::Null;
        wire["font"] = serde_json::Value::Null;
        wire["media"] = serde_json::json!({
            "url": if downloaded {
                "https://files.example/story.jpg".to_owned()
            } else {
                format!("https://api.wuapi.dev/v1/accounts/{ACCOUNT}/stories/{id}/media")
            },
            "mimeType": "image/jpeg", "filename": null, "size": 4096, "width": 1080,
            "height": 1920, "durationSeconds": null, "gifPlayback": false,
            "downloaded": downloaded,
        });
    })
}

fn group_wire(contact: &str, muted: bool, stories: Vec<serde_json::Value>) -> serde_json::Value {
    let stories: Vec<serde_json::Value> = stories
        .into_iter()
        .map(|mut story| {
            story["contactId"] = contact.into();
            story
        })
        .collect();
    serde_json::json!({
        "object": "story_group", "accountId": ACCOUNT, "contactId": contact,
        "profileName": "Maria", "username": null, "muted": muted,
        "storyCount": stories.len(), "unviewedCount": stories.len(),
        "lastPostedAt": now_minus(30), "stories": stories,
    })
}

fn list_of(items: Vec<serde_json::Value>, next: Option<&str>) -> String {
    serde_json::json!({"object": "list", "items": items, "nextCursor": next}).to_string()
}

const NO_ROUTE: &str = r#"{"code":"not_found","message":"Route not found."}"#;

fn missing(provider: &WuapiProvider) -> bool {
    provider.unavailable(&account(), Feature::Stories)
}

#[tokio::test]
async fn the_stories_are_the_accounts_own_and_its_contacts_by_author_a_page_at_a_time() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &at("/stories/own"),
        vec![
            reply(200, &list_of(vec![own_wire("m_mine", 3)], Some("own_2"))),
            reply(
                200,
                &list_of(
                    vec![
                        // Still queued: no time of its own yet, listed.
                        story_wire("m_queued", |wire| {
                            wire["own"] = true.into();
                            wire["status"] = "queued".into();
                            wire["postedAt"] = serde_json::Value::Null;
                            wire["expiresAt"] = serde_json::Value::Null;
                            wire["createdAt"] = now_minus(1).into();
                        }),
                        // Not stories: one that failed, one taken down.
                        story_wire("m_failed", |wire| {
                            wire["own"] = true.into();
                            wire["status"] = "failed".into();
                        }),
                        story_wire("m_deleted", |wire| {
                            wire["own"] = true.into();
                            wire["deletedAt"] = now_minus(5).into();
                        }),
                    ],
                    None,
                ),
            ),
        ],
    )
    .await;
    on(
        &server,
        "GET",
        &at("/stories"),
        vec![
            reply(
                200,
                &list_of(
                    vec![group_wire(
                        "+584245550199",
                        false,
                        vec![
                            story_wire("s_text", |_| {}),
                            picture_wire("s_waiting", false),
                            picture_wire("s_stored", true),
                        ],
                    )],
                    Some("groups_2"),
                ),
            ),
            reply(
                200,
                &list_of(
                    vec![group_wire(
                        "lid:77123456789012",
                        true,
                        vec![
                            story_wire("s_seen", |wire| {
                                wire["viewedAt"] = now_minus(2).into();
                            }),
                            story_wire("s_gif", |wire| {
                                wire["type"] = "video".into();
                                wire["media"] = serde_json::json!({
                                    "url": "https://files.example/loop.mp4",
                                    "mimeType": "video/mp4", "filename": null, "size": 900,
                                    "width": 320, "height": 240, "durationSeconds": 3,
                                    "gifPlayback": true, "downloaded": true,
                                });
                            }),
                            // Past its day: not listed, whatever the API says.
                            story_wire("s_expired", |wire| {
                                wire["postedAt"] = now_minus(25 * 60).into();
                                wire["expiresAt"] = now_minus(60).into();
                            }),
                            // A kind WhatsApp added since.
                            story_wire("s_new_kind", |wire| {
                                wire["type"] = "hologram".into();
                            }),
                        ],
                    )],
                    None,
                ),
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let listed = provider.list_stories(&account()).await.unwrap();
    let ids: Vec<&str> = listed.iter().map(|story| story.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "m_mine",
            "m_queued",
            "s_text",
            "s_waiting",
            "s_stored",
            "s_seen",
            "s_gif"
        ]
    );
    let story = |id: &str| listed.iter().find(|story| story.id.as_str() == id).unwrap();

    // The account's own: seen by definition, with how many saw it.
    let mine = story("m_mine");
    assert!(mine.mine && mine.viewed);
    assert_eq!(mine.view_count, Some(3));
    assert_eq!(mine.author.as_str(), "+584121234567");

    // A contact's text story, with the colour and the font it was posted in.
    let text = story("s_text");
    assert!(!text.mine && !text.viewed);
    assert_eq!(text.view_count, None);
    assert_eq!(text.author.as_str(), "+584245550199");
    assert_eq!(text.author_name.as_deref(), Some("Maria"));
    assert_eq!(
        text.body,
        StoryBody::Text {
            text: "Good morning".into(),
            style: StoryStyle {
                background: 0x0B6E4F,
                font: StoryFont(7)
            }
        }
    );
    assert!(text.expires_at.is_some());

    // A picture still on WhatsApp names the story, never the API's URL; a
    // stored one is its file. Both carry the box they are drawn in.
    let StoryBody::Media(waiting) = &story("s_waiting").body else {
        panic!("a picture is media");
    };
    assert_eq!(waiting.kind, MediaKind::Image);
    assert_eq!(
        waiting.source,
        Some(MediaRef::new("wuapi-story:4096:s_waiting"))
    );
    assert_eq!((waiting.width, waiting.height), (Some(1080), Some(1920)));
    assert_eq!(waiting.caption.as_deref(), Some("The view"));
    let StoryBody::Media(stored) = &story("s_stored").body else {
        panic!("a picture is media");
    };
    assert_eq!(
        stored.source,
        Some(MediaRef::new("https://files.example/story.jpg"))
    );

    // Seen on the phone, or through the API.
    assert!(story("s_seen").viewed);
    assert_eq!(story("s_seen").author.as_str(), "lid:77123456789012");
    // A video story that plays as a GIF.
    let StoryBody::Media(gif) = &story("s_gif").body else {
        panic!("a video is media");
    };
    assert!(gif.gif && gif.kind == MediaKind::Video);
    assert_eq!(gif.duration_secs, Some(3));

    // Both listings were followed to their last page, a hundred at a time.
    let asked: Vec<String> = requests(&server).await.iter().map(target).collect();
    assert_eq!(
        asked,
        [
            at("/stories/own?limit=100"),
            at("/stories/own?limit=100&cursor=own_2"),
            at("/stories?limit=100"),
            at("/stories?limit=100&cursor=groups_2"),
        ]
    );
    // Listing told nobody anything: reads only, and no view receipt.
    assert!(requests(&server)
        .await
        .iter()
        .all(|request| request.method.as_str() == "GET"));
    assert!(!missing(&provider));
}

#[tokio::test]
async fn a_storys_file_is_asked_for_through_the_api_and_the_key_stays_there() {
    let server = MockServer::start().await;
    let storage = MockServer::start().await;
    on(
        &server,
        "GET",
        &at("/stories/s_waiting/media"),
        vec![reply(
            200,
            &serde_json::json!({
                "object": "media", "storyId": "s_waiting",
                "url": format!("{}/files/story.jpg", storage.uri()),
                "mimeType": "image/jpeg", "filename": null, "size": 4,
            })
            .to_string(),
        )],
    )
    .await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/files/story.jpg"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_raw(vec![1u8, 2, 3, 4], "image/jpeg"),
        )
        .mount(&storage)
        .await;
    let provider = provider(&server);
    let limit = MediaLimit {
        max_bytes: 1024,
        images_only: true,
    };
    let data = provider
        .fetch_media(&account(), &MediaRef::new("wuapi-story:4:s_waiting"), limit)
        .await
        .unwrap();
    assert_eq!(data.bytes, [1, 2, 3, 4]);

    // The API was asked for JSON, with the key; the file host never saw it.
    let api = requests(&server).await;
    assert_eq!(api.len(), 1, "one request, and it is not a view receipt");
    assert_eq!(
        target(&api[0]),
        at("/stories/s_waiting/media?redirect=false")
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

    // A file the story says is over the limit is refused before the API
    // is asked to fetch it.
    let error = provider
        .fetch_media(&account(), &MediaRef::new("wuapi-story:4096:s_big"), limit)
        .await
        .unwrap_err();
    assert!(matches!(&error, ProviderError::Rejected { code, .. } if code == "too_large"));
    assert_eq!(requests(&server).await.len(), 1);
}

#[tokio::test]
async fn seeing_a_story_is_one_explicit_request_and_a_repeat_is_the_same_one() {
    let server = MockServer::start().await;
    let seen = story_wire("s_text", |wire| {
        wire["viewedAt"] = now_minus(0).into();
        wire["authorNotified"] = true.into();
    })
    .to_string();
    on(
        &server,
        "POST",
        &at("/stories/s_text/view"),
        vec![
            reply(
                503,
                r#"{"code":"engine_unavailable","message":"Retry shortly."}"#,
            ),
            reply(200, &seen),
            reply(404, r#"{"code":"not_found","message":"Story not found."}"#),
        ],
    )
    .await;
    let provider = provider(&server);
    let author = ContactId::new("+584245550199");
    let story = MessageId::new("s_text");
    // A drop is weather: the receipt is owed and asked again.
    assert!(provider
        .view_story(&account(), &story, &author)
        .await
        .unwrap_err()
        .is_transient());
    provider
        .view_story(&account(), &story, &author)
        .await
        .unwrap();
    // The story went meanwhile: a refusal, which the client drops.
    let gone = provider
        .view_story(&account(), &story, &author)
        .await
        .unwrap_err();
    assert!(matches!(&gone, ProviderError::Rejected { code, .. } if code == "not_found"));
    assert!(
        !missing(&provider),
        "a story that is gone is not a missing route"
    );

    let asked = requests(&server).await;
    assert_eq!(asked.len(), 3);
    assert!(asked.iter().all(|request| request.body.is_empty()));
    // The same key every time: one receipt however often it is asked.
    assert!(asked
        .iter()
        .all(|request| header(request, "idempotency-key") == Some("view:s_text")));
}

#[tokio::test]
async fn who_saw_a_story_is_read_a_page_at_a_time_with_their_reactions() {
    let server = MockServer::start().await;
    let viewer = |contact: &str, reaction: Option<&str>| {
        serde_json::json!({
            "object": "story_viewer", "accountId": ACCOUNT, "storyId": "m_mine",
            "contactId": contact, "viewedAt": "2026-09-24T08:15:40.000Z",
            "reaction": reaction, "reactedAt": reaction.map(|_| "2026-09-24T08:16:00.000Z"),
        })
    };
    on(
        &server,
        "GET",
        &at("/stories/m_mine/viewers"),
        vec![
            reply(
                200,
                &list_of(vec![viewer("+584245550199", Some("💚"))], Some("more")),
            ),
            reply(
                200,
                &list_of(vec![viewer("lid:77123456789012", None)], None),
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let viewers = provider
        .story_viewers(&account(), &MessageId::new("m_mine"))
        .await
        .unwrap();
    assert_eq!(viewers.len(), 2);
    assert_eq!(viewers[0].contact.as_str(), "+584245550199");
    assert_eq!(viewers[0].reaction.as_deref(), Some("💚"));
    assert_eq!(viewers[0].viewed_at.as_millis(), 1_790_237_740_000);
    assert_eq!(viewers[1].reaction, None);
    let asked: Vec<String> = requests(&server).await.iter().map(target).collect();
    assert_eq!(
        asked,
        [
            at("/stories/m_mine/viewers?limit=100"),
            at("/stories/m_mine/viewers?limit=100&cursor=more"),
        ]
    );
}

fn contact_story() -> client_provider::Story {
    stories::story(
        &serde_json::from_value(story_wire("s_text", |_| {})).unwrap(),
        Timestamp::now(),
    )
    .unwrap()
}

fn answer(content: OutgoingContent) -> OutgoingMessage {
    OutgoingMessage {
        client_id: ClientMessageId::new("c-answer"),
        account_id: account(),
        chat_id: client_provider::ChatId::new("+584245550199"),
        content,
        reply_to: Some(MessageId::new("s_text")),
        mentions: Vec::new(),
        forwarded: false,
    }
}

#[tokio::test]
async fn a_reply_to_a_story_is_a_message_to_its_author_that_names_the_story() {
    let server = MockServer::start().await;
    let sent = {
        let mut wire: serde_json::Value =
            serde_json::from_str(fixture!("message_sent_by_client")).unwrap();
        wire["replyToMessageId"] = serde_json::Value::Null;
        wire["replyToStoryId"] = "s_text".into();
        wire.to_string()
    };
    on(
        &server,
        "POST",
        "/v1/messages",
        vec![
            reply(504, r#"{"code":"timeout","message":"Try again."}"#),
            reply(202, &sent),
        ],
    )
    .await;
    let provider = provider(&server);
    let story = contact_story();
    let message = answer(OutgoingContent::Text {
        body: "Lovely".into(),
    });
    // A drop, then the same request again: one reply.
    assert!(provider
        .reply_to_story(&story, message.clone())
        .await
        .unwrap_err()
        .is_transient());
    let receipt = provider.reply_to_story(&story, message).await.unwrap();
    assert!(!receipt.message_id.as_str().is_empty());
    let asked = requests(&server).await;
    assert_eq!(asked.len(), 2);
    assert_eq!(body(&asked[0]), body(&asked[1]));
    assert!(asked
        .iter()
        .all(|request| header(request, "idempotency-key") == Some("c-answer")));
    assert_eq!(
        body(&asked[1]),
        serde_json::json!({
            "accountId": ACCOUNT,
            "to": "+584245550199",
            "type": "text",
            "text": "Lovely",
            "replyToStoryId": "s_text",
            "metadata": {"clientMessageId": "c-answer"},
        }),
        "the story is named as a story, not as a message"
    );
    // The copy that comes back says it answers a story.
    let wire: api::Message = serde_json::from_str(&sent).unwrap();
    let mapped = mapping::message(&wire).unwrap();
    let mark = mapped.extras.story_reply.expect("it answers a story");
    assert_eq!(mark.story.as_str(), "s_text");
    assert!(!mark.of_mine, "the account answered somebody's");
    assert_eq!(mapped.reply_to, None);
}

#[tokio::test]
async fn a_reaction_to_a_story_goes_to_its_route_and_a_refusal_is_said_in_the_apis_words() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &at("/stories/s_text/react"),
        vec![
            wiremock::ResponseTemplate::new(204),
            reply(
                400,
                r#"{"code":"not_supported","message":"Your status privacy leaves this contact out, so a reaction cannot reach them alone."}"#,
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let story = contact_story();
    let reaction = || {
        answer(OutgoingContent::Reaction {
            target: MessageId::new("s_text"),
            emoji: "💚".into(),
        })
    };
    let receipt = provider.react_to_story(&story, reaction()).await.unwrap();
    assert_eq!(receipt.message_id.as_str(), "reaction:c-answer");
    let refused = provider
        .react_to_story(&story, reaction())
        .await
        .unwrap_err();
    assert!(!refused.is_transient());
    assert!(refused.to_string().contains("status privacy"), "{refused}");
    // That refusal is about the audience, not about stories being there.
    assert!(!missing(&provider));
    let asked = requests(&server).await;
    assert_eq!(body(&asked[0]), serde_json::json!({"emoji": "💚"}));
    assert_eq!(header(&asked[0], "idempotency-key"), Some("c-answer"));
}

/// A backend from before the story routes: the account's own stories are
/// read the way they were, contacts' are "not available yet", nothing
/// fails, and the routes are asked about again later.
#[tokio::test]
async fn a_deployment_without_the_story_routes_keeps_the_own_stories_and_says_not_yet() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &at("/stories/own"),
        vec![
            reply(404, NO_ROUTE),
            reply(200, &list_of(vec![own_wire("m_mine", 0)], None)),
        ],
    )
    .await;
    on(
        &server,
        "GET",
        &at("/stories"),
        vec![reply(200, &list_of(Vec::new(), None))],
    )
    .await;
    let fresh = story_json("m_fresh", &now_minus(30), "text");
    let day_old = story_json("m_old", &now_minus(25 * 60), "text");
    let failed = story_json("m_failed", &now_minus(10), "text")
        .replace(r#""status":"sent""#, r#""status":"failed""#);
    on(
        &server,
        "GET",
        "/v1/messages",
        vec![
            reply(200, &page(&[fresh.clone(), failed, day_old], Some("cur_2"))),
            reply(200, &page(&[fresh], None)),
        ],
    )
    .await;
    let provider = provider(&server);
    assert!(!missing(&provider));
    let own = provider.list_stories(&account()).await.unwrap();
    let ids: Vec<&str> = own.iter().map(|story| story.id.as_str()).collect();
    assert_eq!(
        ids,
        ["m_fresh"],
        "a day old and failed ones are not stories"
    );
    assert!(missing(&provider), "contacts' stories: not available yet");

    // While that is believed, the routes are not asked again and what
    // needs them says so without a request.
    provider.list_stories(&account()).await.unwrap();
    let author = ContactId::new("+584245550199");
    let story = MessageId::new("m_fresh");
    for result in [
        provider.view_story(&account(), &story, &author).await,
        provider.story_viewers(&account(), &story).await.map(|_| ()),
    ] {
        assert!(
            matches!(result, Err(ProviderError::Unsupported(_))),
            "{result:?}"
        );
    }
    let asked: Vec<String> = requests(&server).await.iter().map(target).collect();
    assert_eq!(asked.len(), 3, "{asked:?}");
    assert_eq!(asked[0], at("/stories/own?limit=100"));
    // It stopped at the page that reached back a day: no second page.
    assert!(asked[1].contains("chatId=stories") && asked[1].contains(ACCOUNT));
    assert!(asked[2].contains("chatId=stories"));

    // Later it is asked again, and the deployment has them now.
    provider.client().missing.age(RECHECK);
    let listed = provider.list_stories(&account()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].view_count, Some(0));
    assert!(!missing(&provider));
}

// ----- what the poll passes on ------------------------------------------------

#[test]
fn the_poll_says_which_stories_came_changed_and_went_and_who_was_muted_on_the_phone() {
    let now = Timestamp::now();
    let read = |wire: serde_json::Value| -> client_provider::Story {
        stories::story(&serde_json::from_value(wire).unwrap(), now).unwrap()
    };
    let maria = ContactId::new("+584245550199");
    let pedro = ContactId::new("+584140000009");
    let listed =
        |stories: Vec<client_provider::Story>, muted: Vec<(ContactId, bool)>| stories::Listed {
            stories,
            muted,
            wires: Vec::new(),
            unread: Vec::new(),
        };
    let mut state = PollState::default();
    let summary = |events: Vec<ProviderEvent>| -> Vec<String> {
        events
            .iter()
            .map(|event| match event {
                ProviderEvent::StoryUpserted(story) => format!("story {}", story.id),
                ProviderEvent::StoryRemoved { story_id, .. } => format!("gone {story_id}"),
                ProviderEvent::StoryMuteChanged { contact, muted, .. } => {
                    format!("muted {contact} {muted}")
                }
                other => format!("{other:?}"),
            })
            .collect()
    };

    // The first time everything that is up is said, and a mute made on
    // the phone. "Not muted" is not: a mute made here cannot be written
    // to the API, and its silence must not undo it.
    // Read once: a story read twice would differ by the moment it was
    // read at, which is not a change.
    let mine = read(own_wire("m_mine", 0));
    let text = read(story_wire("s_text", |_| {}));
    let first = state.observe_stories(
        ACCOUNT,
        &listed(
            vec![mine.clone(), text.clone()],
            vec![(maria.clone(), false), (pedro.clone(), true)],
        ),
    );
    assert_eq!(
        summary(first),
        [
            "story m_mine",
            "story s_text",
            format!("muted {pedro} true").as_str()
        ]
    );
    // The same again: nothing.
    let same = state.observe_stories(
        ACCOUNT,
        &listed(
            vec![mine.clone(), text],
            vec![(maria.clone(), false), (pedro.clone(), true)],
        ),
    );
    assert!(same.is_empty());
    // Somebody saw the account's story, a contact's went, another came,
    // and both mutes were changed on the phone.
    let later = state.observe_stories(
        ACCOUNT,
        &listed(
            vec![
                client_provider::Story {
                    view_count: Some(2),
                    ..mine
                },
                read(story_wire("s_new", |_| {})),
            ],
            vec![(maria.clone(), true), (pedro.clone(), false)],
        ),
    );
    assert_eq!(
        summary(later),
        [
            "story m_mine",
            "story s_new",
            "gone s_text",
            format!("muted {maria} true").as_str(),
            format!("muted {pedro} false").as_str(),
        ]
    );
    // Another account's stories are its own business.
    let other = state.observe_stories("other", &listed(Vec::new(), Vec::new()));
    assert!(other.is_empty());
}

#[tokio::test]
async fn a_story_is_taken_down_for_everyone_and_one_already_gone_is_fine() {
    let server = MockServer::start().await;
    on(
        &server,
        "DELETE",
        "/v1/messages/m_story",
        vec![
            wiremock::ResponseTemplate::new(204),
            reply(
                404,
                r#"{"code":"message_not_found","message":"No such message."}"#,
            ),
        ],
    )
    .await;
    let provider = provider(&server);
    let id = MessageId::new("m_story");
    provider.delete_story(&account(), &id).await.unwrap();
    provider.delete_story(&account(), &id).await.unwrap();
    let sent = requests(&server).await;
    assert!(
        target(&sent[0]).contains("forEveryone=true"),
        "{}",
        target(&sent[0])
    );
}

// ----- who sees them ------------------------------------------------------------

#[tokio::test]
async fn the_audience_is_read_from_the_privacy_route() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &at("/privacy/stories"),
        vec![reply(
            200,
            r#"{"object":"story_privacy","lists":[
                {"type":"contacts","contactIds":[],"default":false},
                {"type":"blacklist","contactIds":["+584245550199"],"default":true},
                {"type":"whitelist","contactIds":[],"default":false}]}"#,
        )],
    )
    .await;
    let provider = provider(&server);
    let privacy = provider.story_privacy(&account()).await.unwrap();
    assert_eq!(privacy.audience, StoryAudience::ContactsExcept);
    assert_eq!(privacy.except, [ContactId::new("+584245550199")]);
}
async fn always(server: &MockServer, at: &str, answer: wiremock::ResponseTemplate) {
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(at))
        .respond_with(answer)
        .mount(server)
        .await;
}

const EMPTY: &str = r#"{"object":"list","items":[],"nextCursor":null}"#;

async fn quiet_api(server: &MockServer) {
    let accounts = format!(
        r#"{{"object":"list","items":[{}],"nextCursor":null}}"#,
        fixture!("account")
    );
    always(server, "/v1/accounts", reply(200, &accounts)).await;
    always(server, "/v1/messages", reply(200, EMPTY)).await;
    always(server, &at("/chats"), reply(200, EMPTY)).await;
}

async fn story_asks(server: &MockServer) -> usize {
    requests(server)
        .await
        .iter()
        .filter(|request| request.url.path().contains("/stories"))
        .count()
}

/// The story events are webhooks, which a desktop application cannot
/// receive: the poll reads the lists instead, on its first round (so that
/// what is up when the application starts is there) and then every
/// few rounds, and passes on what changed.
#[tokio::test]
async fn the_poll_reads_the_stories_on_its_first_round_and_then_now_and_then() {
    use crate::follow::Now;
    let server = MockServer::start().await;
    quiet_api(&server).await;
    always(
        &server,
        &at("/stories/own"),
        reply(200, &list_of(vec![own_wire("m_mine", 1)], None)),
    )
    .await;
    always(
        &server,
        &at("/stories"),
        reply(
            200,
            &list_of(
                vec![group_wire(
                    "+584245550199",
                    true,
                    vec![story_wire("s_text", |_| {})],
                )],
                None,
            ),
        ),
    )
    .await;
    let (_provider, polling) =
        WuapiProvider::with_polling(super::config(&server), KEY.into()).unwrap();
    let mut poller = polling.poller();
    let first = poller.poll(Now::real()).await.unwrap();
    let told: Vec<String> = first
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::StoryUpserted(story) => Some(format!("story {}", story.id)),
            ProviderEvent::StoryMuteChanged { contact, muted, .. } => {
                Some(format!("muted {contact} {muted}"))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        told,
        ["story m_mine", "story s_text", "muted +584245550199 true"],
        "what is up when the stream opens is said, though it is not news"
    );
    assert_eq!(story_asks(&server).await, 2);
    // Quiet rounds, then the stories again: nothing changed.
    for _ in 0..crate::events::STORY_SWEEP_POLLS - 1 {
        let events = poller.poll(Now::real()).await.unwrap();
        assert!(events.is_empty(), "{events:?}");
    }
    assert_eq!(story_asks(&server).await, 2);
    assert!(poller.poll(Now::real()).await.unwrap().is_empty());
    assert_eq!(story_asks(&server).await, 4);
    // Reads only: the poll never tells an author anything.
    assert!(requests(&server)
        .await
        .iter()
        .all(|request| request.method.as_str() == "GET"));
}

/// Without the routes the poll asks once, finds out, and leaves the
/// stories alone (the own ones are read when Status is looked at, the way
/// they were), until it is worth asking again. The rest of the poll is
/// not touched.
#[tokio::test]
async fn the_poll_leaves_stories_alone_on_a_deployment_without_their_routes() {
    use crate::follow::Now;
    let server = MockServer::start().await;
    quiet_api(&server).await;
    always(&server, &at("/stories/own"), reply(404, NO_ROUTE)).await;
    let (provider, polling) =
        WuapiProvider::with_polling(super::config(&server), KEY.into()).unwrap();
    let mut poller = polling.poller();
    for _ in 0..2 * crate::events::STORY_SWEEP_POLLS + 1 {
        let events = poller.poll(Now::real()).await.unwrap();
        assert!(events
            .iter()
            .all(|event| !matches!(event, ProviderEvent::StoryUpserted(_))));
    }
    assert!(missing(&provider));
    assert_eq!(
        story_asks(&server).await,
        1,
        "three story rounds, one question"
    );
    provider.client().missing.age(RECHECK);
    for _ in 0..crate::events::STORY_SWEEP_POLLS {
        poller.poll(Now::real()).await.unwrap();
    }
    assert_eq!(story_asks(&server).await, 2, "asked again later");
}

// ----- what the API really answers ----------------------------------------------

/// The spec's own examples of both listings, with their times moved to
/// now: a story is only one for a day.
fn spec_list(fixture: &str) -> String {
    fixture
        .replace("2026-09-24T08:15:38.000Z", &now_minus(31))
        .replace("2026-09-24T08:15:40.000Z", &now_minus(30))
        .replace("2026-09-24T08:15:41.000Z", &now_minus(29))
        .replace("2026-09-25T08:15:40.000Z", &now_minus(30 - 24 * 60))
        .replace("k57a8m2x9d3f0q1wjh6ypc4n2d7s0vbr", ACCOUNT)
}

#[tokio::test]
async fn the_specs_examples_of_both_listings_are_read_as_they_are() {
    let server = MockServer::start().await;
    always(
        &server,
        &at("/stories/own"),
        reply(200, &spec_list(fixture!("story_list"))),
    )
    .await;
    always(
        &server,
        &at("/stories"),
        reply(200, &spec_list(fixture!("story_group_list"))),
    )
    .await;
    let provider = provider(&server);
    let listed = provider.list_stories(&account()).await.unwrap();
    assert_eq!(listed.len(), 2, "{listed:?}");
    let (own, theirs) = (&listed[0], &listed[1]);
    assert!(own.mine && own.viewed);
    assert_eq!(own.view_count, Some(12));
    assert!(matches!(&own.body, StoryBody::Text { text, style }
        if text.starts_with("New hours") && style.background == 0x0F766E && style.font == StoryFont(1)));
    assert!(!theirs.mine && !theirs.viewed);
    assert_eq!(theirs.author.as_str(), "+584245550199");
    assert_eq!(theirs.author_name.as_deref(), Some("Maria G."));
    let StoryBody::Media(picture) = &theirs.body else {
        panic!("the example is a picture");
    };
    assert_eq!(picture.kind, MediaKind::Image);
    assert_eq!(picture.caption.as_deref(), Some("New menu this week"));
    // Still on WhatsApp: the story is named, the API's URL is not followed.
    assert!(picture
        .source
        .as_ref()
        .is_some_and(|source| source.as_str().starts_with("wuapi-story:184320:")));
    assert!(!missing(&provider));
}

#[tokio::test]
async fn nobody_with_a_story_up_is_an_empty_list_and_not_a_missing_feature() {
    let server = MockServer::start().await;
    always(&server, &at("/stories/own"), reply(200, EMPTY)).await;
    always(&server, &at("/stories"), reply(200, EMPTY)).await;
    let provider = provider(&server);
    assert!(provider.list_stories(&account()).await.unwrap().is_empty());
    assert!(!missing(&provider));
}

/// Several authors, one of them known by a hidden-number id only, with a
/// text, a picture and a video, one seen on the phone: each story keeps
/// its author's id as the API gives it, `lid:` included.
#[tokio::test]
async fn several_authors_are_listed_each_under_the_id_the_api_gives_hidden_numbers_included() {
    let server = MockServer::start().await;
    always(&server, &at("/stories/own"), reply(200, EMPTY)).await;
    let video = |id: &str| {
        story_wire(id, |wire| {
            wire["type"] = "video".into();
            wire["text"] = serde_json::Value::Null;
            wire["media"] = serde_json::json!({
                "url": format!("https://api.wuapi.dev/v1/accounts/{ACCOUNT}/stories/{id}/media"),
                "mimeType": "video/mp4", "filename": null, "size": 2_000_000, "width": 720,
                "height": 1280, "durationSeconds": 14, "gifPlayback": false,
                "downloaded": false,
            });
        })
    };
    always(
        &server,
        &at("/stories"),
        reply(
            200,
            &list_of(
                vec![
                    group_wire(
                        "lid:77123456789012",
                        false,
                        vec![story_wire("s_lid_text", |_| {}), video("s_lid_video")],
                    ),
                    group_wire(
                        "+584140000009",
                        false,
                        vec![
                            picture_wire("s_pic", false),
                            story_wire("s_seen", |wire| {
                                wire["viewedAt"] = now_minus(3).into();
                                wire["authorNotified"] = true.into();
                                wire["reaction"] = "💚".into();
                            }),
                        ],
                    ),
                ],
                None,
            ),
        ),
    )
    .await;
    let provider = provider(&server);
    let listed = provider.list_stories(&account()).await.unwrap();
    let told: Vec<(String, String, bool)> = listed
        .iter()
        .map(|story| (story.id.to_string(), story.author.to_string(), story.viewed))
        .collect();
    assert_eq!(
        told,
        [
            (
                "s_lid_text".to_owned(),
                "lid:77123456789012".to_owned(),
                false
            ),
            (
                "s_lid_video".to_owned(),
                "lid:77123456789012".to_owned(),
                false
            ),
            ("s_pic".to_owned(), "+584140000009".to_owned(), false),
            ("s_seen".to_owned(), "+584140000009".to_owned(), true),
        ]
    );
    let StoryBody::Media(video) = &listed[1].body else {
        panic!("a video is media");
    };
    assert_eq!(video.kind, MediaKind::Video);
    assert_eq!(video.duration_secs, Some(14));
}

/// A story the adapter cannot read (a field of another type than the
/// spec's, a field a backend from before it leaves out) is one story
/// less, never a list that cannot be read: the others are listed.
#[tokio::test]
async fn a_story_that_cannot_be_read_does_not_hide_the_others() {
    let server = MockServer::start().await;
    always(
        &server,
        &at("/stories/own"),
        reply(
            200,
            &list_of(
                vec![
                    own_wire("m_mine", 1),
                    // A backend from before `username`, `authorNotified`
                    // and `reaction`.
                    story_wire("m_older", |wire| {
                        wire["own"] = true.into();
                        let fields = wire.as_object_mut().unwrap();
                        fields.remove("username");
                        fields.remove("authorNotified");
                        fields.remove("reaction");
                        fields.remove("projectId");
                    }),
                ],
                None,
            ),
        ),
    )
    .await;
    always(
        &server,
        &at("/stories"),
        reply(
            200,
            &list_of(
                vec![
                    group_wire(
                        "+584245550199",
                        false,
                        vec![
                            story_wire("s_good", |_| {}),
                            // Not what the spec says a story is.
                            story_wire("s_odd", |wire| {
                                wire["postedAt"] = 1_790_000_000_000_i64.into();
                                wire["own"] = "no".into();
                            }),
                            // A file from before `gifPlayback`, with a
                            // length that is not a whole number.
                            {
                                let mut wire = picture_wire("s_older_file", false);
                                let media = wire["media"].as_object_mut().unwrap();
                                media.remove("gifPlayback");
                                media.insert("durationSeconds".into(), 12.5.into());
                                wire
                            },
                        ],
                    ),
                    {
                        // A group from before `username` and `muted`.
                        let mut group =
                            group_wire("+584140000009", false, vec![story_wire("s_other", |_| {})]);
                        let fields = group.as_object_mut().unwrap();
                        fields.remove("username");
                        fields.remove("muted");
                        group
                    },
                ],
                None,
            ),
        ),
    )
    .await;
    let provider = provider(&server);
    let listed = provider.list_stories(&account()).await.unwrap();
    let ids: Vec<&str> = listed.iter().map(|story| story.id.as_str()).collect();
    assert_eq!(
        ids,
        ["m_mine", "m_older", "s_good", "s_older_file", "s_other"]
    );
    assert!(!missing(&provider));
}

/// A story inside an author's group that does not name its author (or
/// names another id of the same person) is that author's: the group says
/// who posted what it holds.
#[tokio::test]
async fn a_story_is_its_groups_authors_whatever_the_story_itself_says() {
    let server = MockServer::start().await;
    always(&server, &at("/stories/own"), reply(200, EMPTY)).await;
    let mut group = group_wire(
        "lid:77123456789012",
        false,
        vec![story_wire("s_one", |_| {}), story_wire("s_two", |_| {})],
    );
    group["stories"][0]["contactId"] = serde_json::Value::Null;
    group["stories"][1]["contactId"] = "+584245550199".into();
    always(
        &server,
        &at("/stories"),
        reply(200, &list_of(vec![group], None)),
    )
    .await;
    let provider = provider(&server);
    let listed = provider.list_stories(&account()).await.unwrap();
    let authors: Vec<&str> = listed.iter().map(|story| story.author.as_str()).collect();
    assert_eq!(authors, ["lid:77123456789012", "lid:77123456789012"]);
}

/// What tells the adapter that stories are missing is the deployment
/// having no route. A story that is gone, or a number whose engine does
/// not send view receipts or reactions yet, is that request refused: the
/// list goes on being read.
#[tokio::test]
async fn a_story_that_is_gone_or_a_refused_receipt_does_not_turn_stories_off() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        &at("/stories/s_gone/view"),
        vec![reply(
            404,
            r#"{"code":"not_found","message":"Story not found."}"#,
        )],
    )
    .await;
    on(
        &server,
        "POST",
        &at("/stories/s_text/view"),
        vec![reply(
            400,
            r#"{"code":"not_supported","message":"Story views are not available for this account yet."}"#,
        )],
    )
    .await;
    on(
        &server,
        "GET",
        &at("/stories/s_gone/viewers"),
        vec![reply(
            404,
            r#"{"code":"not_found","message":"Story not found."}"#,
        )],
    )
    .await;
    let provider = provider(&server);
    let author = ContactId::new("+584245550199");
    for story in ["s_gone", "s_text"] {
        let refused = provider
            .view_story(&account(), &MessageId::new(story), &author)
            .await
            .unwrap_err();
        assert!(
            matches!(refused, ProviderError::Rejected { .. }),
            "{refused:?}"
        );
    }
    assert!(provider
        .story_viewers(&account(), &MessageId::new("s_gone"))
        .await
        .is_err());
    assert!(!missing(&provider));
}

/// Stories are stored rows: they are read for a number that is
/// reconnecting too (connections drop every few minutes), not only for
/// one that is `ready` at the moment of the poll.
#[tokio::test]
async fn the_poll_reads_the_stories_of_a_number_that_is_reconnecting() {
    use crate::follow::Now;
    let server = MockServer::start().await;
    let accounts = format!(
        r#"{{"object":"list","items":[{}],"nextCursor":null}}"#,
        fixture!("account")
            .replace(r#""status": "ready""#, r#""status": "disconnected""#)
            .replace(r#""reconnecting": false"#, r#""reconnecting": true"#)
    );
    assert!(
        accounts.contains("disconnected") && accounts.contains(r#""reconnecting": true"#),
        "the fixture says `ready` and not reconnecting"
    );
    always(&server, "/v1/accounts", reply(200, &accounts)).await;
    always(&server, "/v1/messages", reply(200, EMPTY)).await;
    always(&server, &at("/chats"), reply(200, EMPTY)).await;
    always(&server, &at("/stories/own"), reply(200, EMPTY)).await;
    always(
        &server,
        &at("/stories"),
        reply(
            200,
            &list_of(
                vec![group_wire(
                    "+584245550199",
                    false,
                    vec![story_wire("s_text", |_| {})],
                )],
                None,
            ),
        ),
    )
    .await;
    let (_provider, polling) =
        WuapiProvider::with_polling(super::config(&server), KEY.into()).unwrap();
    let mut poller = polling.poller();
    let first = poller.poll(Now::real()).await.unwrap();
    assert!(
        first
            .iter()
            .any(|event| matches!(event, ProviderEvent::StoryUpserted(story) if story.id.as_str() == "s_text")),
        "{first:?}"
    );
}

/// A listing that failed in passing is asked again at the next poll, not
/// a sweep later: the first round is what fills Status at startup.
#[tokio::test]
async fn stories_that_could_not_be_read_are_asked_for_again_at_the_next_poll() {
    use crate::follow::Now;
    let server = MockServer::start().await;
    quiet_api(&server).await;
    on(
        &server,
        "GET",
        &at("/stories/own"),
        vec![
            reply(
                503,
                r#"{"code":"engine_unavailable","message":"Try again."}"#,
            ),
            reply(200, EMPTY),
        ],
    )
    .await;
    always(
        &server,
        &at("/stories"),
        reply(
            200,
            &list_of(
                vec![group_wire(
                    "+584245550199",
                    false,
                    vec![story_wire("s_text", |_| {})],
                )],
                None,
            ),
        ),
    )
    .await;
    let (_provider, polling) =
        WuapiProvider::with_polling(super::config(&server), KEY.into()).unwrap();
    let mut poller = polling.poller();
    let stories = |events: &[ProviderEvent]| {
        events
            .iter()
            .filter(|event| matches!(event, ProviderEvent::StoryUpserted(_)))
            .count()
    };
    assert_eq!(stories(&poller.poll(Now::real()).await.unwrap()), 0);
    assert_eq!(
        stories(&poller.poll(Now::real()).await.unwrap()),
        1,
        "the very next poll reads them"
    );
}

/// "Check again": what was remembered about the routes is forgotten, so
/// the very next listing asks the API instead of answering from memory
/// for ten minutes.
#[tokio::test]
async fn a_recheck_forgets_that_stories_were_missing_and_the_api_is_asked_at_once() {
    let server = MockServer::start().await;
    on(
        &server,
        "GET",
        &at("/stories/own"),
        vec![reply(404, NO_ROUTE), reply(200, EMPTY)],
    )
    .await;
    always(
        &server,
        &at("/stories"),
        reply(
            200,
            &list_of(
                vec![group_wire(
                    "+584245550199",
                    false,
                    vec![story_wire("s_text", |_| {})],
                )],
                None,
            ),
        ),
    )
    .await;
    always(&server, "/v1/messages", reply(200, EMPTY)).await;
    let provider = provider(&server);
    assert!(provider.list_stories(&account()).await.unwrap().is_empty());
    assert!(missing(&provider));
    // Believed: nothing is asked of the story routes.
    assert!(provider.list_stories(&account()).await.unwrap().is_empty());
    assert_eq!(story_asks(&server).await, 1);

    provider.recheck(&account(), Feature::Stories);
    assert!(!missing(&provider));
    let listed = provider.list_stories(&account()).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!missing(&provider));
    // Another feature's memory is not touched by it.
    provider.client().missing.no_route(Feature::ForwardAny);
    provider.recheck(&account(), Feature::Stories);
    assert!(provider.unavailable(&account(), Feature::ForwardAny));
}

// ----- `--diagnose stories` -----------------------------------------------------

mod diagnosis {
    use super::*;
    use crate::diagnose::{error_line, story_line, story_lines};
    use crate::stories::ListError;

    fn wire(value: serde_json::Value) -> api::Story {
        serde_json::from_value(value).unwrap()
    }

    /// Everything a person wrote or is called, in every fixture below.
    const PRIVATE: [&str; 9] = [
        "Maria",
        "Good morning",
        "The view",
        "files.example",
        "api.wuapi.dev",
        "story.jpg",
        "+584245550199",
        "584245550199",
        KEY,
    ];

    fn says_nothing_private(lines: &[String]) {
        for line in lines {
            for private in PRIVATE {
                assert!(!line.contains(private), "`{private}` in `{line}`");
            }
        }
    }

    #[test]
    fn a_story_line_says_who_cut_down_what_when_and_where_its_file_is() {
        let now = Timestamp::now();
        let text = wire(story_wire("s97fd2k4w8qc1n5x7v3b9yt6r0hjm2ea", |wire| {
            wire["postedAt"] = "2026-10-02T08:15:40.000Z".into();
            wire["expiresAt"] = "2099-10-03T08:15:40.000Z".into();
        }));
        let line = story_line(&text, now);
        assert_eq!(
            line,
            "story id:..m2ea author=phone:..0199 own=false type=text status=received \
             posted=2026-10-02T08:15:40.000Z expires=2099-10-03T08:15:40.000Z viewed=false \
             media=none shown=true"
        );

        let waiting = story_line(&wire(picture_wire("s_waiting", false)), now);
        assert!(waiting.contains("type=image") && waiting.contains("media=on_whatsapp"));
        let stored = story_line(&wire(picture_wire("s_stored", true)), now);
        assert!(stored.contains("media=downloaded"), "{stored}");
        let lost = story_line(
            &wire({
                let mut wire = picture_wire("s_lost", false);
                wire["media"]["url"] = serde_json::Value::Null;
                wire
            }),
            now,
        );
        assert!(lost.contains("media=missing"), "{lost}");

        // Seen, by a hidden-number id; one the account posted; one of a
        // kind the client does not show.
        let seen = story_line(
            &wire(story_wire("s_seen", |wire| {
                wire["contactId"] = "lid:77123456789012".into();
                wire["viewedAt"] = now_minus(1).into();
            })),
            now,
        );
        assert!(seen.contains("author=lid:..9012") && seen.contains("viewed=true"));
        let mine = story_line(&wire(own_wire("m_mine", 3)), now);
        assert!(mine.contains("own=true") && mine.contains("status=read"));
        let odd = story_line(
            &wire(story_wire("s_odd", |wire| {
                wire["type"] = "hologram".into();
            })),
            now,
        );
        assert!(odd.contains("shown=false"), "{odd}");
        let queued = story_line(
            &wire(story_wire("m_queued", |wire| {
                wire["own"] = true.into();
                wire["postedAt"] = serde_json::Value::Null;
                wire["expiresAt"] = serde_json::Value::Null;
            })),
            now,
        );
        assert!(queued.contains("posted=null expires=null"), "{queued}");
        says_nothing_private(&[line, waiting, stored, lost, seen, mine, odd, queued]);
    }

    #[test]
    fn an_error_is_said_by_its_status_and_code_and_never_by_its_words() {
        let api = ListError::Api(Box::new(wuapi::Error::Api {
            status: 404,
            code: "not_found".into(),
            message: "Account of Maria not found.".into(),
            details: None,
            request_id: None,
            retry_after: None,
        }));
        assert_eq!(error_line(&api), "error status=404 code=not_found");
        let timeout = ListError::Api(Box::new(wuapi::Error::Timeout {
            timeout: std::time::Duration::from_secs(20),
        }));
        assert_eq!(error_line(&timeout), "error status=none code=timeout");
        let older = ListError::Messages(ProviderError::Rejected {
            code: "account_not_found".into(),
            message: "Maria".into(),
        });
        assert_eq!(
            error_line(&older),
            "error status=none code=account_not_found"
        );
        let lines = story_lines(
            "account id:..0vbr status=ready",
            true,
            &Err(api),
            Timestamp::now(),
        );
        assert_eq!(
            lines,
            [
                "account id:..0vbr status=ready stories=available own=? contacts=? \
              error status=404 code=not_found"
            ]
        );
    }

    /// The whole report against a mock API: the same listing the
    /// application makes, counted, a line per story, what could not be
    /// read said without its content, reads only.
    #[tokio::test]
    async fn the_report_counts_the_stories_lists_each_and_tells_nobody_anything() {
        let server = MockServer::start().await;
        quiet_api(&server).await;
        always(
            &server,
            &at("/stories/own"),
            reply(200, &list_of(vec![own_wire("m_mine", 2)], None)),
        )
        .await;
        always(
            &server,
            &at("/stories"),
            reply(
                200,
                &list_of(
                    vec![
                        group_wire(
                            "+584245550199",
                            false,
                            vec![
                                story_wire("s_text", |_| {}),
                                picture_wire("s_pic", false),
                                story_wire("s_broken", |wire| {
                                    wire["own"] = "Maria".into();
                                }),
                            ],
                        ),
                        group_wire(
                            "lid:77123456789012",
                            true,
                            vec![story_wire("s_seen", |wire| {
                                wire["viewedAt"] = now_minus(2).into();
                            })],
                        ),
                    ],
                    None,
                ),
            ),
        )
        .await;
        let provider = provider(&server);
        let lines = provider.diagnose_stories().await.unwrap();
        assert!(lines[0].starts_with("sdk wuapi "));
        assert!(lines[1].starts_with("api http://127.0.0.1"));
        assert_eq!(lines[2], "accounts 1");
        assert!(
            lines[3].starts_with("account id:..")
                && lines[3].ends_with(
                    "stories=available own=1 contacts=3 unviewed=2 authors=2 muted_authors=1 \
                     answered=4 unread=1"
                ),
            "{}",
            lines[3]
        );
        let stories: Vec<&String> = lines
            .iter()
            .filter(|line| line.starts_with("story "))
            .collect();
        assert_eq!(stories.len(), 4);
        assert!(stories[0].contains("own=true"));
        assert!(stories[3].contains("author=lid:..9012") && stories[3].contains("viewed=true"));
        let unread: Vec<&String> = lines
            .iter()
            .filter(|line| line.starts_with("unread "))
            .collect();
        assert_eq!(unread.len(), 1);
        assert!(unread[0].contains("invalid type"), "{}", unread[0]);
        says_nothing_private(&lines);
        // Reads only: nothing was marked as seen, nobody was told.
        assert!(requests(&server)
            .await
            .iter()
            .all(|request| request.method.as_str() == "GET"));
    }

    /// A deployment without the routes, and a listing that fails: each is
    /// said as what it is, with the status and the code.
    #[tokio::test]
    async fn the_report_says_when_stories_are_not_available_and_why_a_listing_failed() {
        let server = MockServer::start().await;
        quiet_api(&server).await;
        always(&server, &at("/stories/own"), reply(404, NO_ROUTE)).await;
        let provider = provider(&server);
        let lines = provider.diagnose_stories().await.unwrap();
        assert!(
            lines[3].contains("stories=not_available own=0 contacts=0"),
            "{}",
            lines[3]
        );

        let server = MockServer::start().await;
        quiet_api(&server).await;
        always(
            &server,
            &at("/stories/own"),
            reply(
                403,
                r#"{"code":"forbidden","message":"This key cannot read Maria's stories."}"#,
            ),
        )
        .await;
        let provider = super::provider(&server);
        let lines = provider.diagnose_stories().await.unwrap();
        assert!(
            lines[3]
                .ends_with("stories=available own=? contacts=? error status=403 code=forbidden"),
            "{}",
            lines[3]
        );
        says_nothing_private(&lines);
    }
}
