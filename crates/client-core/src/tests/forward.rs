//! Forwarding: any message passed on to other chats by naming it, queued
//! like a send, shown at once as pending with the "Forwarded" mark, and
//! never made twice by a retry.

use super::*;
use client_provider::{ContactCard, GeoPoint, Location, MediaRef, Poll, PollOption};

/// A message of the account's first chat, put in the provider's world
/// and in the store, as if it had been there for a while.
fn put(mock: &MockProvider, engine: &SyncEngine, message: &Message) {
    mock.push_event(ProviderEvent::MessageUpserted(message.clone()));
    engine.store().upsert_message(message).unwrap();
}

fn media_message(
    account: &AccountId,
    chat: &ChatId,
    id: &str,
    ts: i64,
    kind: MediaKind,
    url: &str,
) -> Message {
    let mut media = Media::new(kind);
    media.source = Some(MediaRef::new(url));
    media.mime_type = Some(
        match kind {
            MediaKind::Image => "image/jpeg",
            MediaKind::Voice => "audio/ogg",
            MediaKind::Sticker => "image/webp",
            MediaKind::Document => "application/pdf",
            _ => "video/mp4",
        }
        .to_owned(),
    );
    let mut message = text(id, "x", ts, "", Direction::Incoming);
    message.account_id = account.clone();
    message.chat_id = chat.clone();
    message.content = MessageContent::Media(media);
    message
}

/// A pass over the outbox a little later than the messages were queued:
/// a batch queued in one go has each message a millisecond after the one
/// before it, to keep their order.
async fn flush_later(engine: &SyncEngine, mock: &MockProvider) -> crate::OutboxPass {
    let later = Timestamp::from_millis(Timestamp::now().as_millis() + 5_000);
    crate::run_outbox_pass(engine.store(), mock, &SyncConfig::default().outbox, later)
        .await
        .unwrap()
}

struct World {
    mock: MockProvider,
    engine: SyncEngine,
    account: AccountId,
    to: Vec<ChatId>,
    sources: Vec<Message>,
}

/// An engine over the mock, with one of each kind in the first chat.
async fn world() -> World {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chats = store.chats(&account, None).unwrap();
    let from = chats[0].id.clone();
    let to = vec![chats[1].id.clone(), chats[2].id.clone()];
    let base = Timestamp::now().as_millis() + 1_000_000;
    let mut caption = media_message(
        &account,
        &from,
        "src-image",
        base + 1,
        MediaKind::Image,
        "https://wa/img",
    );
    if let MessageContent::Media(media) = &mut caption.content {
        media.caption = Some("the lake".into());
    }
    let mut doc = media_message(
        &account,
        &from,
        "src-doc",
        base + 3,
        MediaKind::Document,
        "https://wa/doc",
    );
    if let MessageContent::Media(media) = &mut doc.content {
        media.file_name = Some("plan.pdf".into());
    }
    let mut poll = text("src-poll", "x", base + 5, "", Direction::Incoming);
    poll.account_id = account.clone();
    poll.chat_id = from.clone();
    poll.content = MessageContent::Poll(Poll {
        question: "Lunch?".into(),
        options: vec![
            PollOption {
                name: "yes".into(),
                votes: 4,
            },
            PollOption {
                name: "no".into(),
                votes: 1,
            },
        ],
        max_choices: 1,
        voters: 5,
        chosen: Some(vec!["yes".into()]),
    });
    let mut place = text("src-place", "x", base + 6, "", Direction::Incoming);
    place.account_id = account.clone();
    place.chat_id = from.clone();
    place.content = MessageContent::Location(Location {
        point: GeoPoint::new(38.7, -9.1).unwrap(),
        name: Some("Café".into()),
        address: None,
        live: false,
    });
    let mut card = text("src-card", "x", base + 7, "", Direction::Incoming);
    card.account_id = account.clone();
    card.chat_id = from.clone();
    card.content = MessageContent::Contacts {
        cards: vec![ContactCard {
            name: "Ana".into(),
            phones: vec!["+351900000000".into()],
        }],
    };
    let mut words = text("src-text", "x", base + 8, "hola", Direction::Incoming);
    words.account_id = account.clone();
    words.chat_id = from.clone();
    let sources = vec![
        caption,
        media_message(
            &account,
            &from,
            "src-voice",
            base + 2,
            MediaKind::Voice,
            "https://wa/voice",
        ),
        doc,
        media_message(
            &account,
            &from,
            "src-sticker",
            base + 4,
            MediaKind::Sticker,
            "https://wa/st",
        ),
        poll,
        place,
        card,
        words,
    ];
    for message in &sources {
        put(&mock, &engine, message);
    }
    World {
        mock,
        engine,
        account,
        to,
        sources,
    }
}

fn thread(world: &World, chat: &ChatId) -> Vec<Message> {
    world
        .engine
        .store()
        .messages(&world.account, chat, 500)
        .unwrap()
        .into_iter()
        .map(|stored| stored.message)
        .filter(|message| message.extras.forwarded)
        .collect()
}

#[tokio::test(start_paused = true)]
async fn every_kind_is_forwarded_to_two_chats_in_order_without_moving_a_file() {
    let world = world().await;
    let engine = &world.engine;
    // In the order they were written, two chats each.
    let mut ordered = world.sources.clone();
    ordered.sort_by_key(|message| message.timestamp);
    for message in &ordered {
        for chat in &world.to {
            engine
                .forward_message(&world.account, message, chat)
                .unwrap();
        }
    }

    // At once: pending bubbles, marked, with what they pass on, in order.
    for chat in &world.to {
        let shown = thread(&world, chat);
        assert_eq!(shown.len(), ordered.len(), "{chat}");
        for (bubble, source) in shown.iter().zip(&ordered) {
            assert_eq!(bubble.status, DeliveryStatus::Pending);
            assert!(bubble.extras.forwarded);
            assert_eq!(bubble.direction, Direction::Outgoing);
            match (&bubble.content, &source.content) {
                (MessageContent::Poll(copy), MessageContent::Poll(original)) => {
                    assert_eq!(copy.question, original.question);
                    assert_eq!((copy.voters, copy.options[0].votes), (0, 0));
                }
                (copy, original) => assert_eq!(copy, original),
            }
        }
    }

    let pass = flush_later(engine, &world.mock).await;
    assert_eq!(pass.sent, ordered.len() * 2);
    assert_eq!((pass.retried, pass.failed), (0, 0));
    // No file went through here: nothing uploaded, nothing downloaded,
    // nothing sent as a message of its own.
    assert_eq!(world.mock.upload_calls(), 0);
    assert_eq!(world.mock.media_calls(), 0);
    // Nothing is sent again as a message of its own, the text included:
    // the provider holds it, so it is the original that is passed on.
    assert!(world.mock.sent().is_empty());
    // The provider was asked in the order, per chat.
    let named: Vec<&Message> = ordered.iter().collect();
    let asked = world.mock.forward_calls();
    assert_eq!(asked.len(), named.len() * 2);
    for chat in &world.to {
        let names: Vec<&str> = asked
            .iter()
            .filter(|item| &item.to == chat)
            .map(|item| item.message.as_str())
            .collect();
        let expected: Vec<&str> = named.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(names, expected);
    }
    // Each copy is on "WhatsApp", marked, and the bubble took its id.
    for chat in &world.to {
        let on_phone: Vec<Message> = world
            .mock
            .thread(&world.account, chat)
            .into_iter()
            .filter(|message| message.extras.forwarded)
            .collect();
        assert_eq!(on_phone.len(), ordered.len());
        let shown = thread(&world, chat);
        assert!(shown
            .iter()
            .all(|bubble| bubble.status == DeliveryStatus::Sent
                && !bubble.id.as_str().starts_with("local:")));
    }
    assert_eq!(world.mock.forwarded_copies(), named.len() * 2);
}

#[tokio::test(start_paused = true)]
async fn a_forward_that_loses_its_connection_is_repeated_under_the_same_id_and_made_once() {
    let world = world().await;
    let engine = &world.engine;
    let source = &world.sources[0];
    let client_id = engine
        .forward_message(&world.account, source, &world.to[0])
        .unwrap();
    let store = engine.store().clone();
    world
        .mock
        .fail_next_forwards([ProviderError::Transient("eof".into())]);
    let pass = engine.flush_outbox().await.unwrap();
    assert_eq!((pass.sent, pass.retried, pass.failed), (0, 1, 0));
    let bubble = thread(&world, &world.to[0]);
    assert_eq!(bubble.len(), 1);
    assert_eq!(
        bubble[0].status,
        DeliveryStatus::Pending,
        "a drop is not shown"
    );

    // Then it goes through, and once more is asked for what already went:
    // the same copy.
    let at = |hours: i64| Timestamp::from_millis(Timestamp::now().as_millis() + hours * 3_600_000);
    let config = SyncConfig::default().outbox;
    let pass = crate::run_outbox_pass(&store, &world.mock, &config, at(1))
        .await
        .unwrap();
    assert_eq!(pass.sent, 1);
    let asked = world.mock.forward_calls();
    assert_eq!(asked.len(), 2);
    assert!(asked.iter().all(|item| item.client_id == client_id));
    assert_eq!(world.mock.forwarded_copies(), 1);
    // The provider answering the same item again is the same message.
    let again = world
        .mock
        .forward_messages(&world.account, &asked[..1])
        .await
        .unwrap();
    assert_eq!(again.len(), 1);
    assert_eq!(world.mock.forwarded_copies(), 1);
    assert_eq!(thread(&world, &world.to[0]).len(), 1, "one bubble");
}

#[tokio::test(start_paused = true)]
async fn one_that_cannot_be_forwarded_fails_alone_and_says_why() {
    let world = world().await;
    let engine = &world.engine;
    let image = &world.sources[0];
    let voice = &world.sources[1];
    let doc = &world.sources[2];
    world.mock.refuse_forward_of(
        &voice.id,
        "media_expired",
        "WhatsApp no longer has this file.",
    );
    for message in [image, voice, doc] {
        engine
            .forward_message(&world.account, message, &world.to[0])
            .unwrap();
    }
    let pass = flush_later(engine, &world.mock).await;
    assert_eq!((pass.sent, pass.failed), (2, 1));
    let shown = thread(&world, &world.to[0]);
    assert_eq!(shown.len(), 3);
    let of = |kind: MediaKind| {
        shown
            .iter()
            .find(|message| matches!(&message.content, MessageContent::Media(m) if m.kind == kind))
            .unwrap()
    };
    assert_eq!(of(MediaKind::Image).status, DeliveryStatus::Sent);
    match &of(MediaKind::Voice).status {
        DeliveryStatus::Failed { reason } => {
            assert!(reason.contains("no longer has this file"), "{reason}")
        }
        other => panic!("not failed: {other:?}"),
    }
    assert_eq!(
        of(MediaKind::Document).status,
        DeliveryStatus::Sent,
        "the one behind it is not held back"
    );
}

#[tokio::test(start_paused = true)]
async fn what_cannot_be_forwarded_is_refused_before_anything_is_queued() {
    let world = world().await;
    let engine = &world.engine;
    let image = world.sources[0].clone();
    assert_eq!(engine.forward_refusal(&image), None);

    let mut deleted = image.clone();
    deleted.deleted = true;
    let mut once = image.clone();
    once.extras.view_once = true;
    let mut local = image.clone();
    local.id = MessageId::new("local:abc");
    let mut reaction = image.clone();
    reaction.content = MessageContent::Reaction {
        target: MessageId::new("x"),
        emoji: "x".into(),
    };
    use crate::ForwardRefusal as R;
    for (message, why) in [
        (&deleted, R::Deleted),
        (&once, R::ViewOnce),
        (&local, R::NotSent),
        (&reaction, R::Kind),
    ] {
        assert_eq!(engine.forward_refusal(message), Some(why));
        assert!(matches!(
            engine.forward_message(&world.account, message, &world.to[0]),
            Err(crate::ForwardError::Refused(_))
        ));
    }
    // A file WhatsApp has told us is gone.
    let url = "https://wa/img";
    world.mock.fail_next_media([ProviderError::Rejected {
        code: "media_expired".into(),
        message: "gone".into(),
    }]);
    engine.want_thumbnail_asked(&world.account, url);
    settled(engine, &thumbnail_key(url)).await;
    assert_eq!(engine.forward_refusal(&image), Some(R::FileGone));
    assert!(engine.store().outbox_pending().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn without_forwarding_by_name_only_a_text_is_forwarded_as_before() {
    let mock = MockProvider::quiet();
    mock.set_forward_available(false);
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chats = store.chats(&account, None).unwrap();
    let image = media_message(
        &account,
        &chats[0].id,
        "i",
        1,
        MediaKind::Image,
        "https://wa/i",
    );
    let mut words = text("t", "x", 2, "hola", Direction::Incoming);
    words.account_id = account.clone();
    words.chat_id = chats[0].id.clone();

    assert_eq!(
        engine.forward_refusal(&image),
        Some(crate::ForwardRefusal::NotAvailableYet)
    );
    assert!(matches!(
        engine.forward_message(&account, &image, &chats[1].id),
        Err(crate::ForwardError::Refused(_))
    ));
    assert_eq!(engine.forward_refusal(&words), None);
    engine
        .forward_message(&account, &words, &chats[1].id)
        .unwrap();
    engine.flush_outbox().await.unwrap();
    let sent = mock.sent();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].forwarded);
    assert!(matches!(&sent[0].content, OutgoingContent::Text { body } if body == "hola"));
    assert!(mock.forward_calls().is_empty());
}

/// A text the provider does not hold cannot be named: one that only this
/// computer has, or one that never went. It is sent again with the mark,
/// as a text always could be.
#[tokio::test(start_paused = true)]
async fn a_text_the_provider_does_not_hold_is_sent_again_with_the_mark() {
    let world = world().await;
    let engine = &world.engine;
    let words = world.sources.last().unwrap().clone();
    // Not in the store: the words of something that is not a message of a
    // chat (a story's, say).
    let mut elsewhere = words.clone();
    elsewhere.id = MessageId::new("not-a-chat-message");
    // Only here: still on its way out.
    let mut local = words.clone();
    local.id = MessageId::new("local:abc");
    local.status = DeliveryStatus::Pending;
    for source in [&elsewhere, &local] {
        assert_eq!(engine.forward_refusal(source), None);
        engine
            .forward_message(&world.account, source, &world.to[0])
            .unwrap();
    }
    let pass = flush_later(engine, &world.mock).await;
    assert_eq!((pass.sent, pass.failed), (2, 0));
    let sent = world.mock.sent();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|message| message.forwarded
        && matches!(&message.content, OutgoingContent::Text { body } if body == "hola")));
    assert!(world.mock.forward_calls().is_empty());
}

/// The provider is built to forward by naming a message and its backend
/// does not have that yet (or lost it): nothing fails for it. What was
/// already queued is settled the old way when it is a text, and says "not
/// available yet" when it is not; from then on the menu says so before
/// anything is queued, and a text goes the old way from the start. When
/// the backend has it again, everything is named again.
#[tokio::test(start_paused = true)]
async fn forwarding_that_is_missing_for_now_degrades_and_comes_back() {
    use client_provider::Feature;
    let world = world().await;
    let engine = &world.engine;
    let image = world.sources[0].clone();
    let words = world.sources.last().unwrap().clone();
    assert!(engine.capabilities().forward_any);
    assert!(!engine.feature_unavailable(&world.account, Feature::ForwardAny));

    // Queued while everything looked fine.
    let text_copy = engine
        .forward_message(&world.account, &words, &world.to[0])
        .unwrap();
    engine
        .forward_message(&world.account, &image, &world.to[1])
        .unwrap();
    // By the time they go, the backend says it has no such thing.
    world.mock.set_unavailable(None, Feature::ForwardAny, true);
    let pass = flush_later(engine, &world.mock).await;
    assert_eq!((pass.sent, pass.retried, pass.failed), (1, 0, 1));
    // The text went as a text with the mark, under the id it was queued
    // with: one message, whichever way it went.
    let sent = world.mock.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].client_id, text_copy);
    assert!(sent[0].forwarded);
    assert!(matches!(&sent[0].content, OutgoingContent::Text { body } if body == "hola"));
    assert_eq!(thread(&world, &world.to[0])[0].status, DeliveryStatus::Sent);
    // The picture could not go, and says why in the words of the menu.
    match &thread(&world, &world.to[1])[0].status {
        DeliveryStatus::Failed { reason } => assert_eq!(
            reason,
            crate::ForwardRefusal::NotAvailableYet.reason(),
            "{reason}"
        ),
        other => panic!("not failed: {other:?}"),
    }

    // From now on: refused before queueing, and a text is not named.
    assert!(engine.feature_unavailable(&world.account, Feature::ForwardAny));
    assert!(!engine.capabilities_for(&world.account).forward_any);
    assert_eq!(
        engine.forward_refusal(&image),
        Some(crate::ForwardRefusal::NotAvailableYet)
    );
    assert_eq!(engine.forward_refusal(&words), None);
    let before = world.mock.forward_call_count();
    engine
        .forward_message(&world.account, &words, &world.to[0])
        .unwrap();
    flush_later(engine, &world.mock).await;
    assert_eq!(world.mock.sent().len(), 2);
    assert_eq!(
        world.mock.forward_call_count(),
        before,
        "not asked for what it does not have"
    );

    // It is back.
    world.mock.set_unavailable(None, Feature::ForwardAny, false);
    assert_eq!(engine.forward_refusal(&image), None);
    engine
        .forward_message(&world.account, &image, &world.to[1])
        .unwrap();
    engine
        .forward_message(&world.account, &words, &world.to[1])
        .unwrap();
    let pass = flush_later(engine, &world.mock).await;
    assert_eq!((pass.sent, pass.failed), (2, 0));
    assert_eq!(world.mock.sent().len(), 2, "named again, not sent again");
    assert_eq!(world.mock.forwarded_copies(), 2);
}

/// What a provider refuses when the copy is sent reads the way a refusal
/// before queueing does: the neutral codes are worded here, once.
#[tokio::test(start_paused = true)]
async fn a_refusal_from_the_provider_is_said_in_the_menus_own_words() {
    use crate::ForwardRefusal as R;
    use client_provider::refusal;
    let cases = [
        (refusal::FORWARD_DELETED, R::Deleted),
        (refusal::FORWARD_VIEW_ONCE, R::ViewOnce),
        (refusal::FORWARD_KIND, R::Kind),
        (refusal::FORWARD_NOT_SENT, R::NotSent),
        (refusal::FORWARD_NO_FILE, R::NoFile),
        (refusal::FORWARD_FILE_GONE, R::FileGone),
        (refusal::FORWARD_TOO_LARGE, R::TooLarge),
    ];
    for (code, refusal) in cases {
        assert_eq!(R::of_code(code), Some(refusal), "{code}");
    }
    assert_eq!(R::of_code("not_on_whatsapp"), None);

    let world = world().await;
    let engine = &world.engine;
    let image = &world.sources[0];
    let voice = &world.sources[1];
    world.mock.refuse_forward_of(
        &image.id,
        refusal::FORWARD_VIEW_ONCE,
        "the provider's own sentence",
    );
    // A code the client has no words for keeps the provider's.
    world
        .mock
        .refuse_forward_of(&voice.id, "free_limit_reached", "This month's limit.");
    for message in [image, voice] {
        engine
            .forward_message(&world.account, message, &world.to[0])
            .unwrap();
    }
    let pass = flush_later(engine, &world.mock).await;
    assert_eq!(pass.failed, 2);
    let reasons: Vec<String> = thread(&world, &world.to[0])
        .iter()
        .map(|message| match &message.status {
            DeliveryStatus::Failed { reason } => reason.clone(),
            other => panic!("not failed: {other:?}"),
        })
        .collect();
    assert_eq!(
        reasons,
        [
            R::ViewOnce.reason().to_owned(),
            "This month's limit.".to_owned()
        ]
    );
}

/// A backend that does not pass polls or calendar events on says so in
/// its capabilities: they are refused before queueing, as a kind.
#[tokio::test(start_paused = true)]
async fn kinds_a_backend_does_not_pass_on_are_refused_before_queueing() {
    let mock = MockProvider::new(provider_mock::MockConfig {
        capabilities: Some(client_provider::Capabilities {
            forward_polls: false,
            forward_events: false,
            ..client_provider::Capabilities::all()
        }),
        ..provider_mock::MockConfig::quiet()
    });
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let account = engine.store().accounts().unwrap().remove(0).id;
    let chats = engine.store().chats(&account, None).unwrap();
    let mut poll = text("p", "x", 1, "", Direction::Incoming);
    poll.account_id = account.clone();
    poll.chat_id = chats[0].id.clone();
    poll.content = MessageContent::Poll(Poll {
        question: "Lunch?".into(),
        options: Vec::new(),
        max_choices: 1,
        voters: 0,
        chosen: None,
    });
    let mut event = poll.clone();
    event.content = MessageContent::Event(client_provider::CalendarEvent {
        title: "Launch".into(),
        description: None,
        starts_at: None,
        ends_at: None,
        place: None,
        call: None,
        join_url: None,
        cancelled: false,
    });
    for message in [&poll, &event] {
        assert_eq!(
            engine.forward_refusal(message),
            Some(crate::ForwardRefusal::Kind)
        );
    }
    // A picture still goes.
    let image = media_message(
        &account,
        &chats[0].id,
        "i",
        2,
        MediaKind::Image,
        "https://wa/i",
    );
    assert_eq!(engine.forward_refusal(&image), None);
}
