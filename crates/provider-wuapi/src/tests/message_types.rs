//! Every message `type` of the API, from a fixture shaped like the spec's
//! `Message`, into the neutral model; and the two calls a poll needs.

use super::*;
use client_provider::{
    CalendarEvent, ContactCard, ContactId, EventCall, EventPlace, GeoPoint, Location, Mention,
    MessageExtras, Poll, PollOption, Timestamp,
};

macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!("../../tests/fixtures/", $name, ".json"))
    };
}

fn mapped(json: &str) -> client_provider::Message {
    mapping::message(&parse::<api::Message>(json)).expect("a message the client shows")
}

fn lunch() -> Poll {
    Poll {
        question: "Lunch?".into(),
        options: [("Pizza", 1), ("Sushi", 2), ("Tacos", 0)]
            .into_iter()
            .map(|(name, votes)| PollOption {
                name: name.into(),
                votes,
            })
            .collect(),
        max_choices: 1,
        voters: 3,
        chosen: None,
    }
}

#[test]
fn maps_a_location() {
    let message = mapped(fixture!("message_location"));
    assert_eq!(
        message.content,
        MessageContent::Location(Location {
            point: GeoPoint::new(10.4806, -66.9036).unwrap(),
            name: Some("Caracas office".into()),
            address: Some("Av. Francisco de Miranda".into()),
            // The API does not say whether a location is live.
            live: false,
        })
    );
    assert!(message.extras.is_empty());

    // Without a name or address the coordinates are all there is.
    let mut bare: api::Message = parse(fixture!("message_location"));
    let place = bare.location.as_mut().unwrap();
    (place.name, place.address) = (Some("  ".into()), None);
    assert!(matches!(
        mapping::message(&bare).unwrap().content,
        MessageContent::Location(Location {
            name: None,
            address: None,
            ..
        })
    ));

    // Coordinates that are no place, or no `location` at all (a message
    // imported with the history): the type's name is what is left.
    bare.location.as_mut().unwrap().latitude = 123.0;
    let unreadable = MessageContent::Unsupported {
        description: "location".into(),
    };
    assert_eq!(mapping::message(&bare).unwrap().content, unreadable);
    bare.location = None;
    assert_eq!(mapping::message(&bare).unwrap().content, unreadable);
}

#[test]
fn maps_one_contact_card_and_several() {
    let sales = ContactCard {
        name: "Sales".into(),
        phones: vec!["+584121111111".into()],
    };
    assert_eq!(
        mapped(fixture!("message_contact")).content,
        MessageContent::Contacts {
            cards: vec![sales.clone()]
        }
    );
    assert_eq!(
        mapped(fixture!("message_contacts")).content,
        MessageContent::Contacts {
            cards: vec![
                sales.clone(),
                ContactCard {
                    name: "Support".into(),
                    phones: vec!["+584122222222".into()],
                }
            ]
        }
    );

    // `contact` alone is enough, and a card without a number has none.
    let mut wire: api::Message = parse(fixture!("message_contact"));
    wire.contacts = None;
    wire.contact.as_mut().unwrap().phone = String::new();
    assert_eq!(
        mapping::message(&wire).unwrap().content,
        MessageContent::Contacts {
            cards: vec![ContactCard {
                name: "Sales".into(),
                phones: Vec::new(),
            }]
        }
    );
    wire.contact = None;
    assert_eq!(
        mapping::message(&wire).unwrap().content,
        MessageContent::Unsupported {
            description: "contact".into()
        }
    );
}

#[test]
fn maps_a_poll_with_its_tally_and_no_word_on_the_accounts_vote() {
    let message = mapped(fixture!("message_poll"));
    assert_eq!(message.content, MessageContent::Poll(lunch()));
    assert_eq!(message.chat_id.as_str(), GROUP);

    // `selectableCount: 0` is any number of answers; a negative count
    // (which the API should never send) is none, not a huge number.
    let mut wire: api::Message = parse(fixture!("message_poll"));
    let poll = wire.poll.as_mut().unwrap();
    poll.selectable_count = 0;
    poll.options[0].vote_count = -4;
    let MessageContent::Poll(poll) = mapping::message(&wire).unwrap().content else {
        panic!("a poll")
    };
    assert!(poll.multiple_choice());
    assert_eq!(poll.options[0].votes, 0);

    wire.poll = None;
    assert_eq!(
        mapping::message(&wire).unwrap().content,
        MessageContent::Unsupported {
            description: "poll".into()
        }
    );
}

#[test]
fn maps_a_calendar_event() {
    let message = mapped(fixture!("message_calendar_event"));
    assert_eq!(
        message.content,
        MessageContent::Event(CalendarEvent {
            title: "Launch review".into(),
            description: Some("Final checks before the release.".into()),
            // 2026-10-01T15:00:00Z and an hour later.
            starts_at: Some(Timestamp::from_millis(1_790_866_800_000)),
            ends_at: Some(Timestamp::from_millis(1_790_870_400_000)),
            place: Some(EventPlace {
                name: Some("Caracas office".into()),
                address: Some("Av. Francisco de Miranda".into()),
                point: GeoPoint::new(10.4806, -66.9036),
            }),
            call: Some(EventCall::Video),
            join_url: Some("https://call.whatsapp.com/video/AbCdEf123".into()),
            cancelled: false,
        })
    );

    // What is not there is not invented, and a join link that is not
    // https is not kept.
    let mut wire: api::Message = parse(fixture!("message_calendar_event"));
    let event = wire.calendar_event.as_mut().unwrap();
    event.cancelled = true;
    event.call_type = Some("hologram".into());
    event.join_url = Some("javascript:alert(1)".into());
    event.ends_at = None;
    event.description = None;
    let place = event.location.as_mut().unwrap();
    (place.name, place.address, place.latitude) = (None, None, None);
    let MessageContent::Event(event) = mapping::message(&wire).unwrap().content else {
        panic!("an event")
    };
    assert!(event.cancelled);
    assert_eq!(
        (event.call, event.join_url, event.ends_at, event.place),
        (None, None, None, None)
    );
}

#[test]
fn maps_view_once_forwarded_starred_edited_and_mentions() {
    let once = mapped(fixture!("message_view_once"));
    assert!(once.extras.view_once);
    assert!(matches!(&once.content, MessageContent::Media(media)
        if media.kind == MediaKind::Image));

    let marked = mapped(fixture!("message_marked"));
    assert_eq!(
        marked.extras,
        MessageExtras {
            forwarded: true,
            forwarded_many: false,
            starred: true,
            view_once: false,
            mentions: vec![
                Mention {
                    id: ContactId::new("+584121234567"),
                    handle: "584121234567".into(),
                    name: None,
                    me: false,
                },
                Mention {
                    id: ContactId::new("lid:77123456789012"),
                    handle: "77123456789012".into(),
                    name: None,
                    me: false,
                },
            ],
            // The API carries no link preview on a message.
            link: None,
            sender_username: None,
            story_reply: None,
        }
    );
    assert!(marked.edited && !marked.deleted);
    // The text is kept as typed: the handles are what it names people by.
    assert_eq!(
        marked.content,
        MessageContent::text("@584121234567 and @77123456789012 see https://example.com/launch")
    );
}

#[test]
fn a_mention_lists_the_id_and_the_text_may_carry_another_one() {
    // What the API does with a mention made by hidden-number id: the id
    // in `mentions` is already the number, the text still has the digits
    // of the hidden-number id. The mapping passes both on as they are;
    // tying them together is the client's, which knows both ids of a
    // person from the contact list.
    let mut wire: api::Message = parse(fixture!("message_marked"));
    wire.text = Some("@200055501000001 are you in?".into());
    wire.mentions = vec!["+584121234567".into(), "  ".into()];
    wire.username = Some("maria.r".into());
    let message = mapping::message(&wire).unwrap();
    assert_eq!(
        message.extras.mentions,
        vec![Mention {
            id: ContactId::new("+584121234567"),
            handle: "584121234567".into(),
            name: None,
            me: false,
        }]
    );
    assert_eq!(
        message.content,
        MessageContent::text("@200055501000001 are you in?")
    );
    // The sender's username rides along, for a sender without a number.
    assert_eq!(message.extras.sender_username.as_deref(), Some("maria.r"));
    assert_eq!(message.sender_name.as_deref(), Some("Maria"));
}

#[test]
fn a_gif_arrives_as_a_video_with_nothing_that_says_gif() {
    // What the API answers for a GIF sent on WhatsApp: `video`,
    // `video/mp4`, and no field that tells it from any other video.
    let mut wire: api::Message = parse(fixture!("message_group_image"));
    wire.r#type = api::MessageType::Video;
    let media = wire.media.as_mut().expect("the fixture has a file");
    media.mime_type = Some("video/mp4".into());
    let MessageContent::Media(media) = mapping::message(&wire).unwrap().content else {
        panic!("a file")
    };
    assert_eq!(media.kind, MediaKind::Video);
    assert!(!media.gif, "the API cannot say");

    // A vector sticker comes as a sticker whose file is not an image.
    let mut wire: api::Message = parse(fixture!("message_sticker_phone"));
    wire.media
        .as_mut()
        .expect("the fixture has a file")
        .mime_type = Some("application/was".into());
    let MessageContent::Media(media) = mapping::message(&wire).unwrap().content else {
        panic!("a file")
    };
    assert_eq!(media.kind, MediaKind::Sticker);
    assert_eq!(media.mime_type.as_deref(), Some("application/was"));
}

#[tokio::test]
async fn a_text_is_sent_with_the_ids_it_mentions() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/v1/messages",
        vec![reply(202, fixture!("message"))],
    )
    .await;
    let provider = provider(&server);
    assert!(provider.capabilities().mentions);
    let mention = |id: &str| {
        let id = ContactId::new(id);
        Mention {
            handle: provider.mention_handle(&id),
            id,
            name: None,
            me: false,
        }
    };
    let outgoing = OutgoingMessage {
        client_id: ClientMessageId::new("c0ffee"),
        account_id: AccountId::new(ACCOUNT),
        chat_id: ChatId::new(GROUP),
        content: OutgoingContent::Text {
            body: "@584121234567 and @77123456789012: dinner?".into(),
        },
        reply_to: None,
        mentions: vec![mention("+584121234567"), mention("lid:77123456789012")],
        forwarded: false,
    };
    assert_eq!(outgoing.mentions[1].handle, "77123456789012");
    provider.send(outgoing).await.unwrap();

    let requests = requests(&server).await;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&requests[0].body).unwrap(),
        serde_json::json!({
            "accountId": ACCOUNT,
            "to": GROUP,
            "type": "text",
            "text": "@584121234567 and @77123456789012: dinner?",
            "mentions": ["+584121234567", "lid:77123456789012"],
            "metadata": { "clientMessageId": "c0ffee" },
        })
    );
}

#[test]
fn every_type_of_the_spec_maps_to_something_the_client_draws() {
    for name in [
        "text",
        "image",
        "video",
        "audio",
        "voice",
        "document",
        "sticker",
        "location",
        "contact",
        "contacts",
        "poll",
        "calendar_event",
        "unknown",
    ] {
        // The type alone, with none of its fields: still a message.
        let mut wire: api::Message = parse(fixture!("message_inbound"));
        wire.r#type = name.into();
        wire.text = None;
        let message = mapping::message(&wire).unwrap_or_else(|| panic!("{name} was dropped"));
        match name {
            "text" => assert_eq!(message.content, MessageContent::text("")),
            "image" | "video" | "audio" | "voice" | "document" | "sticker" => {
                assert!(
                    matches!(message.content, MessageContent::Media(_)),
                    "{name}"
                )
            }
            _ => assert_eq!(
                message.content,
                MessageContent::Unsupported {
                    description: name.into()
                },
                "{name}"
            ),
        }
    }
    // A reaction needs its target; the fixture has one.
    assert!(matches!(
        mapped(fixture!("message_reaction")).content,
        MessageContent::Reaction { .. }
    ));
}

#[tokio::test]
async fn a_vote_names_every_chosen_option_and_brings_the_tally_back() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/v1/messages/m_poll/vote",
        vec![
            reply(200, fixture!("message_poll")),
            reply(200, fixture!("message_poll")),
            reply(409, fixture!("error_not_ready")),
            reply(400, fixture!("error_invalid")),
        ],
    )
    .await;
    let provider = provider(&server);
    let (account, chat, poll) = (
        AccountId::new(ACCOUNT),
        ChatId::new(GROUP),
        MessageId::new("m_poll"),
    );

    let answer = provider
        .vote_poll(&account, &chat, &poll, &["Sushi".to_owned()])
        .await
        .unwrap()
        .expect("the poll comes back");
    assert_eq!(answer.id, poll);
    assert_eq!(answer.content, MessageContent::Poll(lunch()));

    // Taking the vote back is a vote for nothing.
    provider
        .vote_poll(&account, &chat, &poll, &[])
        .await
        .unwrap();
    // A number that is reconnecting is weather; a bad request is not.
    let later = provider
        .vote_poll(&account, &chat, &poll, &["Sushi".to_owned()])
        .await
        .unwrap_err();
    assert!(later.is_transient(), "{later:?}");
    let refused = provider
        .vote_poll(&account, &chat, &poll, &["Moon".to_owned()])
        .await
        .unwrap_err();
    assert!(
        matches!(refused, ProviderError::Rejected { .. }),
        "{refused:?}"
    );

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 4);
    let bodies: Vec<serde_json::Value> = requests
        .iter()
        .map(|request| serde_json::from_slice(&request.body).unwrap())
        .collect();
    assert_eq!(bodies[0], serde_json::json!({ "options": ["Sushi"] }));
    assert_eq!(bodies[1], serde_json::json!({ "options": [] }));
    for request in &requests {
        assert_eq!(target(request), "/v1/messages/m_poll/vote");
        assert!(header(request, "idempotency-key").is_some());
    }
    assert!(provider.capabilities().poll_votes && provider.capabilities().polls);
}

#[tokio::test]
async fn a_poll_is_sent_with_an_idempotency_key_and_echoed_client_id() {
    let server = MockServer::start().await;
    on(
        &server,
        "POST",
        "/v1/messages",
        vec![
            reply(202, fixture!("message_poll")),
            reply(202, fixture!("message_poll")),
        ],
    )
    .await;
    let provider = provider(&server);
    let outgoing = OutgoingMessage {
        client_id: ClientMessageId::new("c0ffee"),
        account_id: AccountId::new(ACCOUNT),
        chat_id: ChatId::new(GROUP),
        content: OutgoingContent::Poll {
            question: "Lunch?".into(),
            options: vec!["Pizza".into(), "Sushi".into(), "Tacos".into()],
            max_choices: 1,
        },
        reply_to: None,
        mentions: Vec::new(),
        forwarded: false,
    };
    let receipt = provider.send(outgoing.clone()).await.unwrap();
    assert_eq!(receipt.message_id.as_str(), "m_poll");
    assert_eq!(provider.send(outgoing).await.unwrap(), receipt);

    let requests = requests(&server).await;
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(header(request, "idempotency-key"), Some("c0ffee"));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            serde_json::json!({
                "accountId": ACCOUNT,
                "to": GROUP,
                "type": "poll",
                "poll": {
                    "name": "Lunch?",
                    "options": ["Pizza", "Sushi", "Tacos"],
                    "selectableCount": 1,
                },
                "metadata": { "clientMessageId": "c0ffee" },
            })
        );
    }
}

#[test]
fn polling_reports_a_poll_again_when_its_tally_moves() {
    let mut state = PollState::default();
    let wire: api::Message = parse(fixture!("message_poll"));
    let (first, _) = state.observe_messages(std::slice::from_ref(&wire));
    assert_eq!(first.len(), 1);
    let (same, _) = state.observe_messages(std::slice::from_ref(&wire));
    assert!(same.is_empty(), "nothing changed");

    // Somebody voted: the API's copy has a new tally and a new
    // `updatedAt`, and the client gets the message again.
    let mut voted = wire.clone();
    voted.poll.as_mut().unwrap().options[2].vote_count = 1;
    voted.poll.as_mut().unwrap().voter_count = 4;
    voted.updated_at = "2026-09-24T09:30:00.000Z".into();
    let (events, _) = state.observe_messages(&[voted]);
    let [ProviderEvent::MessageUpserted(message)] = events.as_slice() else {
        panic!("one update, got {events:?}")
    };
    let MessageContent::Poll(poll) = &message.content else {
        panic!("a poll")
    };
    assert_eq!((poll.options[2].votes, poll.voters), (1, 4));
}

// ----- what SDK 0.9.0 to 0.12.0 added to a message and a chat -----------------

/// A received GIF is a `video` whose media says `gifPlayback`: it gets the
/// GIF flag, so its tile wears the badge and it is saved as a GIF.
#[test]
fn a_video_that_plays_as_a_gif_is_a_gif_and_nothing_else_is() {
    let mut wire: api::Message = parse(fixture!("message_group_image"));
    wire.r#type = api::MessageType::Video;
    let media = wire.media.as_mut().unwrap();
    media.mime_type = Some("video/mp4".into());
    media.gif_playback = true;
    media.duration_seconds = Some(4);
    media.size = Some(81_234);
    let MessageContent::Media(gif) = mapping::message(&wire).unwrap().content else {
        panic!("a video is media");
    };
    assert!(gif.gif);
    assert_eq!(gif.kind, MediaKind::Video);
    assert_eq!(gif.duration_secs, Some(4));
    assert_eq!(gif.size_bytes, Some(81_234));

    // A plain video is not one.
    wire.media.as_mut().unwrap().gif_playback = false;
    let MessageContent::Media(video) = mapping::message(&wire).unwrap().content else {
        panic!("a video is media");
    };
    assert!(!video.gif);
    // The flag means nothing on a picture.
    wire.r#type = api::MessageType::Image;
    wire.media.as_mut().unwrap().gif_playback = true;
    let MessageContent::Media(image) = mapping::message(&wire).unwrap().content else {
        panic!("an image is media");
    };
    assert!(!image.gif);
}

/// A received sticker or picture comes with its size in pixels when the
/// engine reports it: that is the box it is drawn in before its file is
/// here. Half a size is no size.
#[test]
fn a_received_file_carries_the_box_it_is_drawn_in() {
    let mut wire: api::Message = parse(fixture!("message_sticker_phone"));
    {
        let media = wire.media.as_mut().unwrap();
        media.width = Some(512);
        media.height = Some(384);
    }
    let MessageContent::Media(sticker) = mapping::message(&wire).unwrap().content else {
        panic!("a sticker is media");
    };
    assert_eq!(sticker.kind, MediaKind::Sticker);
    assert_eq!((sticker.width, sticker.height), (Some(512), Some(384)));

    wire.media.as_mut().unwrap().height = None;
    let MessageContent::Media(half) = mapping::message(&wire).unwrap().content else {
        panic!("a sticker is media");
    };
    assert_eq!((half.width, half.height), (None, None));
    wire.media.as_mut().unwrap().height = Some(0);
    let MessageContent::Media(zero) = mapping::message(&wire).unwrap().content else {
        panic!("a sticker is media");
    };
    assert_eq!((zero.width, zero.height), (None, None));
}

#[test]
fn forwarded_many_times_is_said_and_is_a_forward() {
    let mut wire: api::Message = parse(fixture!("message_inbound"));
    assert!(!mapping::message(&wire).unwrap().extras.forwarded);
    wire.forwarded = true;
    let once = mapping::message(&wire).unwrap().extras;
    assert!(once.forwarded && !once.forwarded_many);
    wire.forwarded_many_times = true;
    let many = mapping::message(&wire).unwrap().extras;
    assert!(many.forwarded && many.forwarded_many);
    // Whatever the first flag says.
    wire.forwarded = false;
    let many = mapping::message(&wire).unwrap().extras;
    assert!(many.forwarded && many.forwarded_many);
}

/// `replyToStoryId`: somebody answered a story of the account's. What the
/// story showed is not on the message; the client fills that in.
#[test]
fn a_message_that_answers_a_story_says_which() {
    let mut wire: api::Message = parse(fixture!("message_inbound"));
    assert_eq!(mapping::message(&wire).unwrap().extras.story_reply, None);
    wire.reply_to_story_id = Some("m_story".into());
    let message = mapping::message(&wire).unwrap();
    let mark = message.extras.story_reply.expect("it answers a story");
    assert_eq!(mark.story.as_str(), "m_story");
    assert!(
        mark.of_mine,
        "an inbound answer is to a story of the account's"
    );
    assert_eq!(mark.preview, None);
    assert_eq!(
        message.reply_to, None,
        "a story is not a message of the chat"
    );
}

/// `pinnedAt` orders the pinned chats. It says nothing for a chat that is
/// not pinned, and a pin whose time WhatsApp did not say has none.
#[test]
fn a_pinned_chat_says_when_it_was_pinned() {
    let list: api::ChatList = parse(fixture!("chat_list"));
    let mut wire = list.items[0].clone();
    wire.pinned = Some(true);
    wire.pinned_at = Some("2026-09-24T08:15:40.000Z".into());
    let chat = mapping::chat(&wire).unwrap();
    assert!(chat.pinned);
    assert_eq!(
        chat.pinned_at,
        Some(client_provider::Timestamp::from_millis(1_790_237_740_000))
    );
    wire.pinned_at = None;
    let chat = mapping::chat(&wire).unwrap();
    assert!(chat.pinned && chat.pinned_at.is_none());
    // A time without a pin is not a pin.
    wire.pinned = Some(false);
    wire.pinned_at = Some("2026-09-24T08:15:40.000Z".into());
    assert_eq!(mapping::chat(&wire).unwrap().pinned_at, None);
    wire.pinned = None;
    let unknown = mapping::chat(&wire).unwrap();
    assert!(unknown.unknown.pinned && unknown.pinned_at.is_none());
}
