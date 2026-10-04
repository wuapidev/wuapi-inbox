//! One example of every kind of message, in one chat of the demo data, so
//! that each of them can be looked at without a real account.

use crate::seed::{contact_for, self_contact, World};
use client_provider::{
    AccountId, CalendarEvent, ChatId, ContactCard, DeliveryStatus, Direction, EventPlace, GeoPoint,
    LinkPreview, Location, Media, MediaKind, MediaRef, Mention, Message, MessageContent,
    MessageExtras, MessageId, Party, Poll, PollOption, SystemEvent, SystemKind, Timestamp,
};

/// The account whose demo data holds the examples.
pub const SHOWCASE_ACCOUNT: &str = "acc_personal";
/// The chat that holds them: the group "Lisbon trip".
pub const SHOWCASE_CHAT: &str = "group:lisbontrip";

/// The animated sticker among the examples.
pub const SHOWCASE_STICKER: &str = "mock://sticker/904/160";

/// The handle a mention of this demo contact is written with.
fn handle_of(name: &str) -> String {
    handle_of_id(&contact_for(name))
}

/// The handle a mention of the demo contact `id` is written with.
pub(crate) fn handle_of_id(id: &client_provider::ContactId) -> String {
    // The demo's contacts have no numbers; any digits stand in for one.
    let digits = id.as_str().bytes().fold(5_841_200_000_u64, |sum, byte| {
        sum.wrapping_mul(31).wrapping_add(u64::from(byte) * 7_919)
    });
    (digits % 1_000_000_000_000).to_string()
}

fn party(name: &str) -> Party {
    Party {
        id: contact_for(name),
        name: Some(name.to_owned()),
    }
}

fn poll(question: &str, options: &[(&str, u32)], max_choices: u32, voters: u32) -> Poll {
    Poll {
        question: question.to_owned(),
        options: options
            .iter()
            .map(|(name, votes)| PollOption {
                name: (*name).to_owned(),
                votes: *votes,
            })
            .collect(),
        max_choices,
        voters,
        chosen: Some(Vec::new()),
    }
}

/// A small poll for the threads the generator writes.
pub(crate) fn any_poll() -> Poll {
    poll(
        "Which day works best?",
        &[("Friday", 2), ("Saturday", 3), ("Sunday", 1)],
        1,
        6,
    )
}

/// Every example, oldest first: who sent it (`None` for the account), its
/// content and what rides along.
fn examples(day: i64) -> Vec<(Option<&'static str>, MessageContent, MessageExtras)> {
    let plain = MessageExtras::default;
    let lisbon = |latitude, longitude| GeoPoint::new(latitude, longitude).expect("a real place");
    let hour = 60 * 60 * 1000;
    let mut view_once = Media::new(MediaKind::Image);
    view_once.mime_type = Some("image/jpeg".into());
    view_once.source = Some(MediaRef::new("mock://image/901/320x240"));
    // A sticker that moves, and a short looping video sent as a GIF.
    let mut sticker = Media::new(MediaKind::Sticker);
    sticker.mime_type = Some("image/gif".into());
    sticker.source = Some(MediaRef::new(SHOWCASE_STICKER));
    let mut gif = Media::new(MediaKind::Video);
    gif.mime_type = Some("video/mp4".into());
    gif.source = Some(MediaRef::new("mock://media/903"));
    gif.size_bytes = Some(412_000);
    gif.duration_secs = Some(3);
    gif.gif = true;
    vec![
        (
            None,
            MessageContent::System(SystemEvent {
                kind: SystemKind::Added,
                actor: Some(party("Camila Duarte")),
                targets: vec![party("Noah Fischer")],
                detail: None,
            }),
            plain(),
        ),
        (
            None,
            MessageContent::System(SystemEvent {
                kind: SystemKind::SubjectChanged,
                actor: Some(party("Valentina Rojas")),
                targets: Vec::new(),
                detail: Some("Lisbon trip ✈️".into()),
            }),
            plain(),
        ),
        (
            Some("Valentina Rojas"),
            MessageContent::text(
                "*Plan for Saturday*\n\
                 - _Morning_: tram 28 to Alfama\n\
                 - Lunch at ~the usual place~ Time Out Market\n\
                 - Sunset at the viewpoint\n\
                 > Bring comfortable shoes\n\
                 Booking code: `LX-4471`",
            ),
            plain(),
        ),
        (
            Some("Camila Duarte"),
            MessageContent::text(format!(
                "@{} can you book the table? Details at https://www.timeoutmarket.com/lisboa",
                handle_of("Noah Fischer")
            )),
            MessageExtras {
                mentions: vec![Mention {
                    id: contact_for("Noah Fischer"),
                    handle: handle_of("Noah Fischer"),
                    name: None,
                    me: false,
                }],
                link: Some(LinkPreview {
                    url: "https://www.timeoutmarket.com/lisboa".into(),
                    title: Some("Time Out Market Lisboa".into()),
                    description: Some(
                        "The best of the city under one roof: food, drinks and culture.".into(),
                    ),
                    thumbnail: Some(MediaRef::new("mock://image/902/160x160")),
                }),
                ..plain()
            },
        ),
        (
            Some("Noah Fischer"),
            MessageContent::Location(Location {
                point: lisbon(38.706_93, -9.145_77),
                name: Some("Time Out Market".into()),
                address: Some("Av. 24 de Julho 49, 1200-479 Lisboa".into()),
                live: false,
            }),
            plain(),
        ),
        (
            None,
            MessageContent::Location(Location {
                point: lisbon(38.713_91, -9.133_48),
                name: None,
                address: None,
                live: true,
            }),
            plain(),
        ),
        (
            Some("Emma Lindqvist"),
            MessageContent::Contacts {
                cards: vec![ContactCard {
                    name: "Rui (driver)".into(),
                    phones: vec!["+351912345678".into()],
                }],
            },
            plain(),
        ),
        (
            None,
            MessageContent::Contacts {
                cards: [
                    ("Hotel reception", "+351213456789"),
                    ("Ana (guide)", "+351934567890"),
                    ("Airport taxi", "+351961234567"),
                    ("Surf school", "+351927654321"),
                ]
                .into_iter()
                .map(|(name, phone)| ContactCard {
                    name: name.into(),
                    phones: vec![phone.into()],
                })
                .collect(),
            },
            plain(),
        ),
        (
            Some("Camila Duarte"),
            MessageContent::Poll(poll(
                "Where do we stay?",
                &[
                    ("Airbnb near the river", 3),
                    ("Hotel in Baixa", 1),
                    ("Hostel in Bairro Alto", 0),
                ],
                1,
                4,
            )),
            plain(),
        ),
        (
            None,
            MessageContent::Poll(poll(
                "What should we not miss?",
                &[
                    ("Sintra", 3),
                    ("Belém", 2),
                    ("Cascais", 1),
                    ("Fado night", 4),
                ],
                0,
                4,
            )),
            plain(),
        ),
        (
            Some("Valentina Rojas"),
            MessageContent::Event(CalendarEvent {
                title: "Dinner at Ramiro".into(),
                description: Some("Table for five, under Valentina.".into()),
                starts_at: Some(Timestamp::from_millis(day + 20 * hour)),
                ends_at: Some(Timestamp::from_millis(day + 22 * hour)),
                place: Some(EventPlace {
                    name: Some("Cervejaria Ramiro".into()),
                    address: Some("Av. Almirante Reis 1, Lisboa".into()),
                    point: Some(lisbon(38.720_37, -9.135_57)),
                }),
                call: None,
                join_url: None,
                cancelled: false,
            }),
            plain(),
        ),
        (
            Some("Noah Fischer"),
            MessageContent::Media(view_once),
            MessageExtras {
                view_once: true,
                ..plain()
            },
        ),
        (
            Some("Emma Lindqvist"),
            MessageContent::text("Flights are cheaper if we leave on the 14th"),
            MessageExtras {
                forwarded: true,
                starred: true,
                ..plain()
            },
        ),
        (None, MessageContent::text("🎉✈️"), plain()),
        (
            Some("Emma Lindqvist"),
            MessageContent::Media(sticker),
            plain(),
        ),
        (Some("Noah Fischer"), MessageContent::Media(gif), plain()),
        (
            Some("Camila Duarte"),
            MessageContent::Unsupported {
                description: "payment_request".into(),
            },
            plain(),
        ),
        (
            None,
            MessageContent::System(SystemEvent {
                kind: SystemKind::MissedVoiceCall,
                actor: Some(party("Noah Fischer")),
                targets: Vec::new(),
                detail: None,
            }),
            plain(),
        ),
    ]
}

/// Puts the examples into the demo's "Lisbon trip" group, just before its
/// last few messages, so the chat still ends with ordinary conversation.
pub(crate) fn add(world: &mut World) {
    let account = AccountId::new(SHOWCASE_ACCOUNT);
    let chat = ChatId::new(SHOWCASE_CHAT);
    let Some(thread) = world.messages.get_mut(&(account.clone(), chat.clone())) else {
        return;
    };
    // After the message that precedes the last three (reactions aside):
    // consecutive messages are at least a minute apart, and the examples
    // are a second apart from each other.
    let at = thread
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, message)| !matches!(message.content, MessageContent::Reaction { .. }))
        .nth(3)
        .map_or(0, |(index, _)| index + 1);
    let base = match at.checked_sub(1).and_then(|before| thread.get(before)) {
        Some(before) => before.timestamp.as_millis(),
        None => thread
            .first()
            .map_or(0, |first| first.timestamp.as_millis() - 60_000),
    };
    let me = self_contact(&account);
    let day = base - base.rem_euclid(24 * 60 * 60 * 1000) + 2 * 24 * 60 * 60 * 1000;
    let examples: Vec<Message> = examples(day)
        .into_iter()
        .enumerate()
        .map(|(index, (sender, content, extras))| {
            let id = world.next_message;
            world.next_message += 1;
            Message {
                id: MessageId::new(format!("m{id}")),
                client_id: None,
                account_id: account.clone(),
                chat_id: chat.clone(),
                sender: sender.map_or_else(|| me.clone(), contact_for),
                sender_name: sender.map(str::to_owned),
                direction: match sender {
                    Some(_) => Direction::Incoming,
                    None => Direction::Outgoing,
                },
                timestamp: Timestamp::from_millis(base + 1_000 * (index as i64 + 1)),
                content,
                reply_to: None,
                status: DeliveryStatus::Read,
                edited: false,
                deleted: false,
                extras,
            }
        })
        .collect();
    thread.splice(at..at, examples);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MockProvider;
    use client_provider::{
        ClientMessageId, OutgoingContent, OutgoingMessage, Provider, ProviderError,
    };

    async fn thread(mock: &MockProvider) -> Vec<Message> {
        let (account, chat) = (AccountId::new(SHOWCASE_ACCOUNT), ChatId::new(SHOWCASE_CHAT));
        let mut messages = Vec::new();
        let mut cursor = None;
        loop {
            let page = mock
                .fetch_messages(&account, &chat, cursor, 100)
                .await
                .unwrap();
            messages.extend(page.items);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        messages.reverse();
        messages
    }

    #[tokio::test]
    async fn the_demo_data_holds_one_of_every_kind_of_message() {
        let mock = MockProvider::quiet();
        let thread = thread(&mock).await;
        let has = |test: &dyn Fn(&Message) -> bool| thread.iter().any(test);
        use MessageContent as C;
        assert!(has(
            &|m| matches!(&m.content, C::Location(place) if !place.live)
        ));
        assert!(has(
            &|m| matches!(&m.content, C::Location(place) if place.live)
        ));
        assert!(has(
            &|m| matches!(&m.content, C::Contacts { cards } if cards.len() == 1)
        ));
        assert!(has(
            &|m| matches!(&m.content, C::Contacts { cards } if cards.len() > 3)
        ));
        assert!(has(
            &|m| matches!(&m.content, C::Poll(poll) if !poll.multiple_choice())
        ));
        assert!(has(
            &|m| matches!(&m.content, C::Poll(poll) if poll.multiple_choice())
        ));
        assert!(has(&|m| matches!(&m.content, C::Event(_))));
        assert!(has(&|m| matches!(&m.content, C::Unsupported { .. })));
        assert!(has(&|m| matches!(&m.content, C::System(event)
            if event.kind == SystemKind::Added)));
        assert!(has(&|m| matches!(&m.content, C::System(event)
            if event.kind == SystemKind::MissedVoiceCall)));
        assert!(has(&|m| m.extras.view_once));
        assert!(has(&|m| matches!(&m.content, C::Media(media)
            if media.kind == MediaKind::Sticker)));
        assert!(has(&|m| matches!(&m.content, C::Media(media)
            if media.kind == MediaKind::Video && media.gif)));
        assert!(has(&|m| m.extras.forwarded && m.extras.starred));
        assert!(has(&|m| m.extras.link.is_some()));
        // The mention is written in the text the way its handle says.
        let mentioning = thread
            .iter()
            .find(|m| !m.extras.mentions.is_empty())
            .expect("a mention");
        let C::Text { body } = &mentioning.content else {
            panic!("a text")
        };
        assert!(body.contains(&format!("@{}", mentioning.extras.mentions[0].handle)));
        assert!(has(
            &|m| matches!(&m.content, C::Text { body } if body.contains("*Plan"))
        ));

        // In order, and the chat still ends with ordinary conversation.
        assert!(thread
            .windows(2)
            .all(|pair| pair[0].timestamp <= pair[1].timestamp));
        let chat = mock
            .chat(
                &AccountId::new(SHOWCASE_ACCOUNT),
                &ChatId::new(SHOWCASE_CHAT),
            )
            .unwrap();
        let last = chat.last_message.unwrap();
        assert!(matches!(
            last.content,
            C::Text { .. } | C::Media(_) | C::Poll(_)
        ));
        assert!(thread.iter().rev().take(3).any(|m| m.id == last.id));
    }

    #[tokio::test]
    async fn a_vote_sets_the_accounts_choice_and_can_be_repeated() {
        let mock = MockProvider::quiet();
        let (account, chat) = (AccountId::new(SHOWCASE_ACCOUNT), ChatId::new(SHOWCASE_CHAT));
        let poll = thread(&mock)
            .await
            .into_iter()
            .find(|m| matches!(&m.content, MessageContent::Poll(poll) if poll.max_choices == 1))
            .unwrap();
        let MessageContent::Poll(before) = &poll.content else {
            unreachable!()
        };
        let choice = vec![before.options[1].name.clone()];

        for _ in 0..2 {
            let answer = mock
                .vote_poll(&account, &chat, &poll.id, &choice)
                .await
                .unwrap()
                .unwrap();
            let MessageContent::Poll(after) = answer.content else {
                panic!("a poll")
            };
            assert_eq!(after.options[1].votes, before.options[1].votes + 1);
            assert_eq!(after.voters, before.voters + 1);
            assert_eq!(after.chosen.as_deref(), Some(choice.as_slice()));
        }
        assert_eq!(mock.message(&account, &poll.id).unwrap().id, poll.id);

        // What is not an option, or not a poll, is refused.
        let stray = mock
            .vote_poll(&account, &chat, &poll.id, &["Moon".to_owned()])
            .await
            .unwrap_err();
        assert!(matches!(stray, ProviderError::Rejected { .. }));
        let missing = mock
            .vote_poll(&account, &chat, &MessageId::new("nope"), &choice)
            .await
            .unwrap_err();
        assert!(matches!(missing, ProviderError::Rejected { .. }));
    }

    #[tokio::test]
    async fn a_poll_is_sent_once_however_often_it_is_submitted() {
        let mock = MockProvider::quiet();
        let poll = |options: &[&str]| OutgoingMessage {
            client_id: ClientMessageId::new("poll-1"),
            account_id: AccountId::new(SHOWCASE_ACCOUNT),
            chat_id: ChatId::new(SHOWCASE_CHAT),
            content: OutgoingContent::Poll {
                question: "Lunch?".into(),
                options: options.iter().map(|name| (*name).to_owned()).collect(),
                max_choices: 0,
            },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: false,
        };
        let first = mock.send(poll(&["Yes", "No"])).await.unwrap();
        let again = mock.send(poll(&["Yes", "No"])).await.unwrap();
        assert_eq!(first.message_id, again.message_id);
        let sent = mock
            .message(&AccountId::new(SHOWCASE_ACCOUNT), &first.message_id)
            .unwrap();
        assert!(matches!(sent.content, MessageContent::Poll(poll)
            if poll.multiple_choice() && poll.options.len() == 2 && poll.voters == 0));

        let mut lonely = poll(&["Yes"]);
        lonely.client_id = ClientMessageId::new("poll-2");
        assert!(matches!(
            mock.send(lonely).await.unwrap_err(),
            ProviderError::Rejected { .. }
        ));
    }
}
