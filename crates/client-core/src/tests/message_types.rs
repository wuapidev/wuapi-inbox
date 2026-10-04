//! Messages that are more than text or a file: what the store keeps of
//! them, how they read in one line, and how a vote in a poll travels.

use super::*;
use client_provider::{
    CalendarEvent, Contact, ContactCard, GeoPoint, LinkPreview, Location, MediaRef, Mention,
    MessageExtras, Party, Poll, PollOption, SystemEvent, SystemKind,
};
use provider_mock::{SHOWCASE_ACCOUNT, SHOWCASE_CHAT};

fn poll(chosen: Option<&[&str]>) -> Poll {
    Poll {
        question: "Where do we stay?".into(),
        options: [("River", 3), ("Baixa", 1)]
            .into_iter()
            .map(|(name, votes)| PollOption {
                name: name.into(),
                votes,
            })
            .collect(),
        max_choices: 1,
        voters: 4,
        chosen: chosen.map(|names| names.iter().map(|name| (*name).to_owned()).collect()),
    }
}

fn with(id: &str, ts: i64, content: MessageContent) -> Message {
    let mut message = text(id, "chat", ts, "", Direction::Incoming);
    message.content = content;
    message
}

fn store() -> Store {
    let store = Store::open_in_memory().unwrap();
    seed_account(&store);
    store.upsert_chat(&chat("chat", "Ana"), false).unwrap();
    store
}

/// One of every kind, with the line each is known by.
fn every_kind() -> Vec<(MessageContent, MessageExtras, &'static str)> {
    let plain = MessageExtras::default;
    let party = |name: &str| Party {
        id: ContactId::new(name.to_lowercase()),
        name: Some(name.to_owned()),
    };
    let mut photo = Media::new(MediaKind::Image);
    photo.caption = Some("the view".into());
    vec![
        (
            MessageContent::Location(Location {
                point: GeoPoint::new(38.7, -9.1).unwrap(),
                name: Some("Time Out Market".into()),
                address: Some("Av. 24 de Julho".into()),
                live: false,
            }),
            plain(),
            "Location · Time Out Market",
        ),
        (
            MessageContent::Location(Location {
                point: GeoPoint::new(38.7, -9.1).unwrap(),
                name: None,
                address: None,
                live: true,
            }),
            plain(),
            "Live location",
        ),
        (
            MessageContent::Contacts {
                cards: vec![ContactCard {
                    name: "Rui".into(),
                    phones: vec!["+351912345678".into()],
                }],
            },
            plain(),
            "Contact · Rui",
        ),
        (
            MessageContent::Contacts {
                cards: ["Rui", "Ana", "Eva"]
                    .into_iter()
                    .map(|name| ContactCard {
                        name: name.into(),
                        phones: Vec::new(),
                    })
                    .collect(),
            },
            plain(),
            "Contacts · Rui and 2 more",
        ),
        (
            MessageContent::Poll(poll(None)),
            plain(),
            "Poll · Where do we stay?",
        ),
        (
            MessageContent::Event(CalendarEvent {
                title: "Dinner".into(),
                description: None,
                starts_at: Some(Timestamp::from_millis(1)),
                ends_at: None,
                place: None,
                call: None,
                join_url: None,
                cancelled: false,
            }),
            plain(),
            "Event · Dinner",
        ),
        (
            MessageContent::System(SystemEvent {
                kind: SystemKind::Added,
                actor: Some(party("Ana")),
                targets: vec![party("Luis"), party("Eva")],
                detail: None,
            }),
            plain(),
            "Ana added Luis and Eva",
        ),
        (
            MessageContent::Unsupported {
                description: "hologram".into(),
            },
            plain(),
            "Unsupported message",
        ),
        (
            MessageContent::Media(photo),
            MessageExtras {
                view_once: true,
                ..plain()
            },
            "View once photo",
        ),
        (
            MessageContent::text("hello @584245550199"),
            MessageExtras {
                mentions: vec![Mention {
                    id: ContactId::new("+584245550199"),
                    handle: "584245550199".into(),
                    name: Some("Ana Rojas".into()),
                    me: false,
                }],
                ..plain()
            },
            "hello @Ana Rojas",
        ),
    ]
}

#[test]
fn every_kind_of_message_survives_the_store_and_reads_in_one_line() {
    let store = store();
    let kinds = every_kind();
    for (index, (content, extras, _)) in kinds.iter().enumerate() {
        let mut message = with(&format!("m{index}"), 1_000 + index as i64, content.clone());
        message.extras = extras.clone();
        // And each one quoted by a reply, which shows the same line.
        let mut reply = text(
            &format!("r{index}"),
            "chat",
            5_000 + index as i64,
            "re",
            Direction::Outgoing,
        );
        reply.reply_to = Some(ReplyRef {
            message_id: message.id.clone(),
            sender_name: None,
            preview: None,
        });
        store.upsert_messages(&[message, reply]).unwrap();
    }

    let stored = store.messages(&account(), &chat_id(), 100).unwrap();
    assert_eq!(stored.len(), kinds.len() * 2);
    for (index, (content, extras, line)) in kinds.iter().enumerate() {
        let message = &stored[index].message;
        assert_eq!(&message.content, content, "{line}");
        assert_eq!(&message.extras, extras, "{line}");
        assert_eq!(message_preview(message), *line);
        let reply = &stored[kinds.len() + index].message;
        assert_eq!(
            reply.reply_to.as_ref().unwrap().preview.as_deref(),
            Some(*line),
            "the quote of it"
        );
    }

    // The chat list's preview is the line of the newest message.
    let last = with("last", 9_000, kinds[4].0.clone());
    store.upsert_message(&last).unwrap();
    let row = store.chat(&account(), &chat_id()).unwrap().unwrap();
    assert_eq!(row.last_message.unwrap().text, "Poll · Where do we stay?");

    // A deleted one says so, whatever it was.
    let mut gone = last;
    gone.deleted = true;
    assert_eq!(message_preview(&gone), "This message was deleted");
}

#[test]
fn polls_places_cards_and_events_are_found_by_their_words() {
    let store = store();
    let kinds = every_kind();
    for (index, (content, _, _)) in kinds.iter().enumerate() {
        store
            .upsert_message(&with(&format!("m{index}"), index as i64, content.clone()))
            .unwrap();
    }
    for (query, expected) in [
        ("market", 1),
        ("rui", 2),
        ("stay", 1),
        ("baixa", 1),
        ("dinner", 1),
        ("hologram", 0),
    ] {
        let hits = store.search_messages(&account(), query, 10).unwrap();
        assert_eq!(hits.len(), expected, "{query}");
    }
}

#[test]
fn what_rides_along_is_kept_and_follows_the_provider() {
    let store = store();
    let mut message = text(
        "m1",
        "chat",
        1,
        "see https://example.com",
        Direction::Incoming,
    );
    message.extras = MessageExtras {
        forwarded: true,
        forwarded_many: false,
        starred: true,
        view_once: false,
        mentions: Vec::new(),
        link: Some(LinkPreview {
            url: "https://example.com".into(),
            title: Some("Example".into()),
            description: None,
            thumbnail: Some(MediaRef::new("mock://image/1/40x40")),
        }),
        sender_username: None,
        story_reply: None,
    };
    assert_eq!(store.upsert_message(&message).unwrap(), Upsert::Inserted);
    assert_eq!(store.upsert_message(&message).unwrap(), Upsert::Unchanged);
    let read = store.message(&account(), &message.id).unwrap().unwrap();
    assert_eq!(read.extras, message.extras);

    // Unstarred on the phone: the provider's word replaces the stored one.
    message.extras.starred = false;
    assert_eq!(store.upsert_message(&message).unwrap(), Upsert::Updated);
    let read = store.message(&account(), &message.id).unwrap().unwrap();
    assert!(!read.extras.starred && read.extras.forwarded);

    // A message without any stores nothing for them.
    let plain = text("m2", "chat", 2, "plain", Direction::Incoming);
    store.upsert_message(&plain).unwrap();
    let read = store.message(&account(), &plain.id).unwrap().unwrap();
    assert!(read.extras.is_empty());
}

#[test]
fn a_mention_is_named_from_the_address_book() {
    let store = store();
    let ana = ContactId::new("+584245550199");
    let mut message = text("m1", "chat", 1, "ask @584245550199", Direction::Incoming);
    message.extras.mentions = vec![Mention {
        id: ana.clone(),
        handle: "584245550199".into(),
        name: None,
        me: false,
    }];
    store.upsert_message(&message).unwrap();
    let read = &store.messages(&account(), &chat_id(), 10).unwrap()[0].message;
    assert_eq!(
        read.extras.mentions[0].name.as_deref(),
        Some("+58 424 555 0199"),
        "nobody by that id yet: the number, as people write it"
    );
    assert_eq!(message_preview(read), "ask @+58 424 555 0199");

    let mut contact = Contact::new(account(), ana);
    contact.saved_name = Some("Ana Rojas".into());
    contact.profile_name = Some("ana.r".into());
    store.upsert_contact(&contact).unwrap();
    let read = &store.messages(&account(), &chat_id(), 10).unwrap()[0].message;
    assert_eq!(read.extras.mentions[0].name.as_deref(), Some("Ana Rojas"));
    assert_eq!(message_preview(read), "ask @Ana Rojas");
    // What is stored is still what the provider sent.
    assert_eq!(with_mention_names("x", &message.extras.mentions), "x");
}

#[test]
fn a_tally_from_the_provider_does_not_wipe_the_vote_the_client_knows() {
    let store = store();
    let message = with("p1", 1, MessageContent::Poll(poll(None)));
    store.upsert_message(&message).unwrap();
    assert!(store
        .set_poll(
            &account(),
            &message.id,
            &poll(None).with_vote(&["Baixa".into()])
        )
        .unwrap());

    // The provider's copy arrives: a new tally, and no word on the vote.
    let mut tally = poll(None);
    tally.options[1].votes = 5;
    tally.voters = 8;
    store
        .upsert_message(&with("p1", 1, MessageContent::Poll(tally)))
        .unwrap();
    let MessageContent::Poll(read) = store
        .message(&account(), &message.id)
        .unwrap()
        .unwrap()
        .content
    else {
        panic!("still a poll")
    };
    assert_eq!(read.options[1].votes, 5);
    assert_eq!(read.voters, 8);
    assert_eq!(
        read.chosen,
        Some(vec!["Baixa".to_owned()]),
        "the vote stays"
    );

    // A provider that does say how the account voted is believed.
    store
        .upsert_message(&with("p1", 1, MessageContent::Poll(poll(Some(&["River"])))))
        .unwrap();
    let MessageContent::Poll(read) = store
        .message(&account(), &message.id)
        .unwrap()
        .unwrap()
        .content
    else {
        panic!("still a poll")
    };
    assert_eq!(read.chosen, Some(vec!["River".to_owned()]));

    // Only a poll can take a poll.
    let plain = text("t1", "chat", 2, "hi", Direction::Incoming);
    store.upsert_message(&plain).unwrap();
    assert!(!store.set_poll(&account(), &plain.id, &poll(None)).unwrap());
    assert!(!store
        .set_poll(&account(), &MessageId::new("nope"), &poll(None))
        .unwrap());
}

/// The poll of the demo data that takes one answer, and where it is.
async fn demo_poll(engine: &SyncEngine) -> (AccountId, ChatId, MessageId, Poll) {
    engine.refresh().await.unwrap();
    let (account, chat) = (AccountId::new(SHOWCASE_ACCOUNT), ChatId::new(SHOWCASE_CHAT));
    engine.fetch_latest(&account, &chat).await.unwrap();
    let found = engine
        .store()
        .messages(&account, &chat, 200)
        .unwrap()
        .into_iter()
        .find_map(|stored| match stored.message.content {
            MessageContent::Poll(poll) if poll.question == "Where do we stay?" => {
                Some((stored.message.id, poll))
            }
            _ => None,
        })
        .expect("the demo data has that poll");
    (account, chat, found.0, found.1)
}

fn stored_poll(engine: &SyncEngine, account: &AccountId, id: &MessageId) -> Poll {
    match engine
        .store()
        .message(account, id)
        .unwrap()
        .unwrap()
        .content
    {
        MessageContent::Poll(poll) => poll,
        other => panic!("not a poll: {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_vote_shows_at_once_and_survives_a_bad_connection() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat, id, before) = demo_poll(&engine).await;
    let hotel = before.options[1].name.clone();

    // The connection drops twice. The user sees the vote regardless.
    mock.fail_next_votes([
        ProviderError::Transient("eof".into()),
        ProviderError::RateLimited { retry_after: None },
    ]);
    engine.vote_poll(&account, &chat, &id, vec![hotel.clone()]);
    let shown = stored_poll(&engine, &account, &id);
    assert_eq!(shown.chosen, Some(vec![hotel.clone()]));
    assert_eq!(shown.options[1].votes, before.options[1].votes + 1);
    assert_eq!(shown.voters, before.voters + 1);
    assert!(mock.votes().is_empty(), "nothing got through yet");

    settle().await;
    assert_eq!(mock.votes(), [(id.clone(), vec![hotel.clone()])]);
    let settled = stored_poll(&engine, &account, &id);
    assert_eq!(settled, shown, "the provider's tally agrees");

    // Taking the vote back is a vote for nothing.
    engine.vote_poll(&account, &chat, &id, Vec::new());
    settle().await;
    let back = stored_poll(&engine, &account, &id);
    assert_eq!(back.chosen, Some(Vec::new()));
    assert_eq!(back.options[1].votes, before.options[1].votes);
    assert_eq!(back.voters, before.voters);
    assert_eq!(mock.votes().len(), 2);
}

#[tokio::test(start_paused = true)]
async fn a_vote_the_provider_refuses_is_taken_back_and_said() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    let (account, chat, id, before) = demo_poll(&engine).await;
    let mut changes = engine.store().subscribe();

    mock.fail_next_votes([ProviderError::Rejected {
        code: "poll_closed".into(),
        message: "The poll is closed.".into(),
    }]);
    engine.vote_poll(&account, &chat, &id, vec![before.options[0].name.clone()]);
    assert_eq!(
        stored_poll(&engine, &account, &id).options[0].votes,
        before.options[0].votes + 1
    );
    settle().await;
    let after = stored_poll(&engine, &account, &id);
    assert_eq!(after.options, before.options, "the tally is as it was");
    assert_eq!(after.voters, before.voters);
    assert_eq!(after.chosen, Some(Vec::new()));
    assert!(mock.votes().is_empty());

    let mut said = None;
    while let Some(change) = changes.try_next() {
        if let StoreChange::Problem { message } = change {
            said = Some(message);
        }
    }
    let said = said.expect("the refusal is said");
    assert!(
        said.contains("not recorded") && said.contains("closed"),
        "{said}"
    );

    // The user votes again while the first vote is still being retried:
    // the newer one stands, whatever happens to the older.
    mock.fail_next_votes([ProviderError::Transient("eof".into())]);
    engine.vote_poll(&account, &chat, &id, vec![before.options[0].name.clone()]);
    engine.vote_poll(&account, &chat, &id, vec![before.options[2].name.clone()]);
    settle().await;
    let last = stored_poll(&engine, &account, &id);
    assert_eq!(last.chosen, Some(vec![before.options[2].name.clone()]));
    assert_eq!(last.options[0].votes, before.options[0].votes);
    assert_eq!(last.options[2].votes, before.options[2].votes + 1);
    assert_eq!(
        mock.votes().last().unwrap().1,
        vec![before.options[2].name.clone()]
    );
}

#[tokio::test]
async fn a_new_poll_goes_through_the_outbox_like_any_message() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store.chats(&account, None).unwrap().remove(0).id;

    let client_id = engine
        .send(OutgoingMessage {
            client_id: new_client_id(),
            account_id: account.clone(),
            chat_id: chat.clone(),
            content: OutgoingContent::Poll {
                question: "Lunch?".into(),
                options: vec!["Yes".into(), "No".into()],
                max_choices: 1,
            },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: false,
        })
        .unwrap();
    // The bubble is there before the network: a poll nobody voted in.
    let pending = store
        .message(&account, &local_message_id(&client_id))
        .unwrap()
        .unwrap();
    assert_eq!(pending.status, DeliveryStatus::Pending);
    assert_eq!(message_preview(&pending), "Poll · Lunch?");

    assert_eq!(engine.flush_outbox().await.unwrap().sent, 1);
    let sent = store
        .messages(&account, &chat, 5)
        .unwrap()
        .pop()
        .unwrap()
        .message;
    assert_eq!(sent.client_id, Some(client_id));
    assert_eq!(sent.status, DeliveryStatus::Sent);
    let MessageContent::Poll(poll) = sent.content else {
        panic!("a poll")
    };
    assert_eq!(poll.options.len(), 2);
    assert_eq!(poll.voters, 0);
}

#[test]
fn notices_read_as_sentences() {
    let party = |name: &str| Party {
        id: ContactId::new(name.to_lowercase()),
        name: Some(name.to_owned()),
    };
    let line = |kind, actor: Option<&str>, targets: &[&str], detail: Option<&str>| {
        system_line(&SystemEvent {
            kind,
            actor: actor.map(party),
            targets: targets.iter().map(|name| party(name)).collect(),
            detail: detail.map(str::to_owned),
        })
    };
    use SystemKind as K;
    assert_eq!(line(K::Joined, None, &["Ana"], None), "Ana joined");
    assert_eq!(line(K::Left, None, &["Ana"], None), "Ana left");
    assert_eq!(
        line(K::Added, Some("Ana"), &["Luis"], None),
        "Ana added Luis"
    );
    assert_eq!(
        line(K::Added, None, &["Luis", "Eva"], None),
        "Luis and Eva were added"
    );
    assert_eq!(
        line(
            K::Removed,
            Some("Ana"),
            &["Luis", "Eva", "Rui", "Ivo"],
            None
        ),
        "Ana removed Luis, Eva and 2 more"
    );
    assert_eq!(
        line(K::SubjectChanged, Some("Ana"), &[], Some("Lisbon")),
        "Ana changed the group name to “Lisbon”"
    );
    assert_eq!(
        line(K::PictureChanged, None, &[], None),
        "The group picture changed"
    );
    assert_eq!(
        line(K::Promoted, None, &["Luis"], None),
        "Luis is now an admin"
    );
    assert_eq!(
        line(K::Demoted, None, &["Luis", "Eva"], None),
        "Luis and Eva are no longer admins"
    );
    assert_eq!(
        line(K::GroupCreated, Some("Ana"), &[], Some("Trip")),
        "Ana created the group “Trip”"
    );
    assert_eq!(
        line(K::MissedVoiceCall, None, &[], None),
        "Missed voice call"
    );
    assert_eq!(line(K::VideoCall, None, &[], None), "Video call");
    // Someone without a name is their id.
    assert_eq!(
        system_line(&SystemEvent {
            kind: K::Joined,
            actor: None,
            targets: vec![Party {
                id: ContactId::new("+351912345678"),
                name: None
            }],
            detail: None,
        }),
        "+351912345678 joined"
    );
}

#[tokio::test]
async fn a_notice_is_not_an_unread_message() {
    let mock = MockProvider::quiet();
    let engine = engine_for(&mock);
    engine.refresh().await.unwrap();
    let store = engine.store().clone();
    let account = store.accounts().unwrap().remove(0).id;
    let chat = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.unread_count == 0)
        .unwrap();
    // A provider whose chat list has no count for this chat: the engine
    // counts what arrives.
    mock.unread_unknown(&chat.id);
    engine.refresh().await.unwrap();

    let mut notice = incoming(&account, &chat.id, "notice-1");
    notice.content = MessageContent::System(SystemEvent {
        kind: SystemKind::MissedVoiceCall,
        actor: None,
        targets: Vec::new(),
        detail: None,
    });
    engine
        .apply_event(ProviderEvent::MessageUpserted(notice))
        .unwrap();
    assert_eq!(
        store
            .chat(&account, &chat.id)
            .unwrap()
            .unwrap()
            .unread_count,
        0
    );
    engine
        .apply_event(ProviderEvent::MessageUpserted(incoming(
            &account, &chat.id, "text-1",
        )))
        .unwrap();
    assert_eq!(
        store
            .chat(&account, &chat.id)
            .unwrap()
            .unwrap()
            .unread_count,
        1
    );
}
