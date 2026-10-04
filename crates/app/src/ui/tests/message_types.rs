//! Every kind of message in the real window: each tile inside its bubble
//! and the bubble inside the pane, in both themes and both directions, and
//! what a click on one does.

use super::*;
use crate::ui::tiles;
use client_provider::{
    AccountId, CalendarEvent, ChatId, ContactCard, ContactId, EventCall, EventPlace, GeoPoint,
    LinkPreview, Location, Media, MediaKind, MediaRef, Mention, Message, MessageExtras, MessageId,
    Party, Poll, PollOption, ProviderEvent, SystemEvent, SystemKind, Timestamp,
};
use provider_mock::{SHOWCASE_ACCOUNT, SHOWCASE_CHAT};
use std::cell::Cell;

thread_local! {
    /// Each pushed message is a little newer than the one before.
    static CLOCK: Cell<i64> = const { Cell::new(0) };
}

/// Puts a message at the end of the open chat, as if it had just arrived
/// (or, with `outgoing`, been sent from the phone).
fn push(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    content: MessageContent,
    extras: MessageExtras,
    outgoing: bool,
) -> Message {
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let tick = CLOCK.with(|clock| {
        clock.set(clock.get() + 1);
        clock.get()
    });
    let message = Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: chat.account_id.clone(),
        chat_id: chat.id.clone(),
        sender: ContactId::new(if outgoing { "me" } else { "them" }),
        sender_name: None,
        direction: if outgoing {
            Direction::Outgoing
        } else {
            Direction::Incoming
        },
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000 + tick),
        content,
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras,
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message.clone()))
        .unwrap();
    cx.run_until_parked();
    message
}

fn lisbon() -> GeoPoint {
    GeoPoint::new(38.706_93, -9.145_77).unwrap()
}

fn poll(max_choices: u32, options: &[(&str, u32)], voters: u32) -> Poll {
    Poll {
        question: "Where do we stay, all things considered, when we finally get there?".into(),
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

fn dinner() -> CalendarEvent {
    CalendarEvent {
        title: "Dinner at Ramiro with everyone who makes it on time".into(),
        description: Some(
            "Table for five, under Valentina. Bring cash, they take no cards.".into(),
        ),
        starts_at: Some(Timestamp::from_millis(1_791_054_000_000)),
        ends_at: Some(Timestamp::from_millis(1_791_063_000_000)),
        place: Some(EventPlace {
            name: Some("Cervejaria Ramiro".into()),
            address: Some("Av. Almirante Reis 1, 1150-007 Lisboa".into()),
            point: Some(lisbon()),
        }),
        call: Some(EventCall::Video),
        join_url: Some("https://call.example/abc".into()),
        cancelled: false,
    }
}

fn cards(count: usize) -> Vec<ContactCard> {
    (0..count)
        .map(|index| ContactCard {
            name: format!("Contact number {index} with a rather long name to fit"),
            phones: vec![format!("+35191234567{index}")],
        })
        .collect()
}

/// A kind of message to draw: its content, what rides along, the tile it
/// is drawn as and the parts that must stay inside that tile.
struct Kind {
    name: &'static str,
    content: MessageContent,
    extras: MessageExtras,
    tile: &'static str,
    parts: &'static [&'static str],
}

fn kinds() -> Vec<Kind> {
    let plain = MessageExtras::default;
    let mut photo = Media::new(MediaKind::Image);
    photo.source = Some(MediaRef::new("https://media.example/once.jpg"));
    vec![
        Kind {
            name: "location",
            content: MessageContent::Location(Location {
                point: lisbon(),
                name: Some("Time Out Market, the big hall by the river".into()),
                address: Some("Av. 24 de Julho 49, 1200-479 Lisboa, Portugal".into()),
                live: false,
            }),
            extras: plain(),
            tile: "location-tile",
            parts: &[
                "tile-icon",
                "tile-label",
                "location-coordinates",
                "location-open",
            ],
        },
        Kind {
            name: "live location",
            content: MessageContent::Location(Location {
                point: lisbon(),
                name: None,
                address: None,
                live: true,
            }),
            extras: plain(),
            tile: "location-tile",
            parts: &["tile-icon", "tile-label", "location-open"],
        },
        Kind {
            name: "contact card",
            content: MessageContent::Contacts { cards: cards(1) },
            extras: plain(),
            tile: "contact-tile",
            parts: &[
                "contact-avatar",
                "contact-label",
                "contact-message",
                "contact-copy",
            ],
        },
        Kind {
            name: "contact cards",
            content: MessageContent::Contacts { cards: cards(5) },
            extras: plain(),
            tile: "contact-tile",
            parts: &[
                "contact-avatar",
                "contact-label",
                "contact-copy",
                "contacts-view-all",
            ],
        },
        Kind {
            name: "poll",
            content: MessageContent::Poll(poll(
                1,
                &[("Airbnb near the river", 3), ("Hotel in Baixa", 1)],
                4,
            )),
            extras: plain(),
            tile: "poll-tile",
            parts: &[
                "poll-question",
                "poll-rule",
                "poll-option",
                "poll-bar",
                "poll-count",
                "poll-voters",
            ],
        },
        Kind {
            name: "multiple-choice poll",
            content: MessageContent::Poll(poll(
                0,
                &[
                    ("An option whose name goes on for long enough to wrap", 120),
                    ("Short", 7),
                    ("None", 0),
                ],
                120,
            )),
            extras: plain(),
            tile: "poll-tile",
            parts: &["poll-question", "poll-option", "poll-bar", "poll-count"],
        },
        Kind {
            name: "calendar event",
            content: MessageContent::Event(dinner()),
            extras: plain(),
            tile: "event-tile",
            parts: &[
                "tile-icon",
                "tile-label",
                "event-time",
                "event-add",
                "event-map",
                "event-join",
            ],
        },
        Kind {
            name: "cancelled event",
            content: MessageContent::Event(CalendarEvent {
                cancelled: true,
                place: None,
                ..dinner()
            }),
            extras: plain(),
            tile: "event-tile",
            parts: &["tile-icon", "tile-label", "event-cancelled"],
        },
        Kind {
            name: "link preview",
            content: MessageContent::text("Details at https://www.timeoutmarket.com/lisboa"),
            extras: MessageExtras {
                link: Some(LinkPreview {
                    url: "https://www.timeoutmarket.com/lisboa".into(),
                    title: Some(
                        "Time Out Market Lisboa: the best of the city under one roof".into(),
                    ),
                    description: Some(
                        "Food, drinks and culture, curated by the editors of the magazine.".into(),
                    ),
                    thumbnail: None,
                }),
                ..plain()
            },
            tile: "link-card",
            parts: &["link-title", "link-host"],
        },
        Kind {
            name: "view once",
            content: MessageContent::Media(photo),
            extras: MessageExtras {
                view_once: true,
                ..plain()
            },
            tile: "view-once-tile",
            parts: &["tile-icon", "tile-label"],
        },
        Kind {
            name: "unsupported",
            content: MessageContent::Unsupported {
                description: "a_type_with_a_name_so_long_that_it_cannot_fit_on_one_line_of_a_tile"
                    .into(),
            },
            extras: plain(),
            tile: "unsupported-tile",
            parts: &["tile-icon", "tile-label", "unsupported-type"],
        },
        Kind {
            name: "formatted text",
            content: MessageContent::text(
                "*Plan* for _Saturday_ with ~no~ `code`\n- tram 28 to Alfama and then a long \
                 walk down to the river before lunch\n1. lunch\n> bring comfortable \
                 shoes\n```\nLX-4471\n```\nsee https://example.com/a_very/long/path/that/goes/on",
            ),
            extras: MessageExtras {
                forwarded: true,
                starred: true,
                ..plain()
            },
            tile: "message-text",
            parts: &["text-bullet", "text-quote", "text-code"],
        },
        Kind {
            name: "emoji",
            content: MessageContent::text("🎉✈️"),
            extras: plain(),
            tile: "emoji-text",
            parts: &[],
        },
    ]
}

/// The tile and its parts are inside the bubble, and the bubble inside
/// the pane.
fn assert_laid_out(harness: &Harness, cx: &mut TestAppContext, kind: &Kind, where_: &str) {
    let thread = bounds(harness.window, "thread", cx);
    let bubble = bounds(harness.window, "bubble", cx);
    let tile = bounds(harness.window, kind.tile, cx);
    let what = format!("{} ({where_})", kind.name);
    assert!(
        within(bubble, thread),
        "{what}: bubble {bubble:?} outside the pane {thread:?}"
    );
    assert!(
        within(tile, bubble),
        "{what}: {} {tile:?} outside the bubble {bubble:?}",
        kind.tile
    );
    assert!(
        tile.size.width > px(40.) && tile.size.height > px(10.),
        "{what}: the tile has a size, {tile:?}"
    );
    for part in kind.parts {
        let part_bounds = bounds(harness.window, part, cx);
        assert!(
            within(part_bounds, tile),
            "{what}: {part} {part_bounds:?} outside {} {tile:?}",
            kind.tile
        );
        assert!(
            part_bounds.size.width > px(0.) && part_bounds.size.height > px(0.),
            "{what}: {part} is drawn, {part_bounds:?}"
        );
    }
}

fn every_tile_is_laid_out(cx: &mut TestAppContext, theme: ThemeChoice) {
    cx.update(|cx| {
        prepare(cx, None);
        settings::update(cx, |settings| settings.theme = theme);
    });
    let harness = open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    assert_eq!(
        cx.update(|cx| theme::palette(cx).is_dark()),
        theme == ThemeChoice::Dark
    );
    let mut serial = 0;
    for narrow in [false, true] {
        if narrow {
            // Narrower than a bubble may get: the pane decides.
            let visual = VisualTestContext::from_window(harness.window.into(), cx);
            visual.simulate_resize(size(px(660.), px(800.)));
            visual.run_until_parked();
        }
        for outgoing in [false, true] {
            for kind in kinds() {
                serial += 1;
                push(
                    &harness,
                    cx,
                    &format!("kind-{serial}"),
                    kind.content.clone(),
                    kind.extras.clone(),
                    outgoing,
                );
                let where_ = format!(
                    "{theme:?}, {}, {}",
                    if outgoing { "outgoing" } else { "incoming" },
                    if narrow { "narrow" } else { "wide" }
                );
                assert_laid_out(&harness, cx, &kind, &where_);
                if kind.extras.forwarded {
                    let bubble = bounds(harness.window, "bubble", cx);
                    for mark in ["forwarded-label", "starred-mark"] {
                        let mark_bounds = bounds(harness.window, mark, cx);
                        assert!(within(mark_bounds, bubble), "{where_}: {mark}");
                    }
                }
            }
            // A notice is a line on the page: centred, and no bubble of
            // its own.
            serial += 1;
            push(
                &harness,
                cx,
                &format!("kind-{serial}"),
                MessageContent::System(SystemEvent {
                    kind: SystemKind::SubjectChanged,
                    actor: Some(Party {
                        id: ContactId::new("ana"),
                        name: Some("Ana".into()),
                    }),
                    targets: Vec::new(),
                    detail: Some("A group name long enough to need more than one line ✈️".into()),
                }),
                MessageExtras::default(),
                outgoing,
            );
            let thread = bounds(harness.window, "thread", cx);
            let line = bounds(harness.window, "system-line", cx);
            assert!(within(line, thread), "{line:?} outside {thread:?}");
            let (left, right) = (line.left() - thread.left(), thread.right() - line.right());
            assert!(
                (left - right).abs() <= px(16.),
                "centred: {left:?} / {right:?}"
            );
        }
    }
}

#[gpui_kit::test]
fn every_tile_stays_inside_its_bubble_and_the_pane_in_the_light_theme(cx: &mut TestAppContext) {
    every_tile_is_laid_out(cx, ThemeChoice::Light);
}

#[gpui_kit::test]
fn every_tile_stays_inside_its_bubble_and_the_pane_in_the_dark_theme(cx: &mut TestAppContext) {
    every_tile_is_laid_out(cx, ThemeChoice::Dark);
}

fn open_chat_one(cx: &mut TestAppContext) -> Harness {
    open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    )
}

/// The newest poll of the open chat.
fn shown_poll(harness: &Harness, cx: &mut TestAppContext) -> Poll {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let rows = &shell.open.as_ref().unwrap().rows;
        rows.iter()
            .rev()
            .find_map(|row| match row {
                Row::Message(row) => match &row.stored.message.content {
                    MessageContent::Poll(poll) => Some(poll.clone()),
                    _ => None,
                },
                Row::Day(_) => None,
            })
            .expect("a poll is in the open chat")
    })
}

/// A chat whose newest message is a poll the provider knows: sent from
/// here, through the outbox.
fn open_with_poll(cx: &mut TestAppContext) -> (Harness, Message) {
    let harness = open_chat_one(cx);
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    harness
        .engine
        .send(client_provider::OutgoingMessage {
            client_id: client_core::new_client_id(),
            account_id: chat.account_id.clone(),
            chat_id: chat.id.clone(),
            content: OutgoingContent::Poll {
                question: "Where do we stay?".into(),
                options: vec!["River".into(), "Baixa".into(), "Bairro Alto".into()],
                max_choices: 1,
            },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: false,
        })
        .unwrap();
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 1);
    cx.run_until_parked();
    let poll = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        match shell.open.as_ref().unwrap().rows.last() {
            Some(Row::Message(row)) => row.stored.message.clone(),
            _ => panic!("the poll is the newest row"),
        }
    });
    assert!(matches!(poll.content, MessageContent::Poll(_)));
    assert_eq!(poll.status, DeliveryStatus::Sent);
    (harness, poll)
}

#[gpui_kit::test]
fn the_demo_chat_shows_one_of_every_kind_and_names_its_mentions(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = AccountId::new(SHOWCASE_ACCOUNT);
    // The address book arrives: the mention reads as the saved name.
    harness
        .runtime
        .block_on(harness.engine.sync_contacts(&account))
        .unwrap();
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_chat(cx);
            shell.open_chat(ChatId::new(SHOWCASE_CHAT), None, cx);
        })
    });
    cx.run_until_parked();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let rows = &shell.open.as_ref().unwrap().rows;
        let messages: Vec<&Message> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Message(row) => Some(&row.stored.message),
                Row::Day(_) => None,
            })
            .collect();
        use MessageContent as C;
        for (name, found) in [
            (
                "location",
                messages.iter().any(|m| matches!(m.content, C::Location(_))),
            ),
            (
                "cards",
                messages
                    .iter()
                    .any(|m| matches!(m.content, C::Contacts { .. })),
            ),
            (
                "poll",
                messages.iter().any(|m| matches!(m.content, C::Poll(_))),
            ),
            (
                "event",
                messages.iter().any(|m| matches!(m.content, C::Event(_))),
            ),
            (
                "notice",
                messages.iter().any(|m| matches!(m.content, C::System(_))),
            ),
            (
                "unknown",
                messages
                    .iter()
                    .any(|m| matches!(m.content, C::Unsupported { .. })),
            ),
            ("view once", messages.iter().any(|m| m.extras.view_once)),
            ("forwarded", messages.iter().any(|m| m.extras.forwarded)),
            ("link", messages.iter().any(|m| m.extras.link.is_some())),
        ] {
            assert!(found, "the demo chat has no {name}");
        }
        let mention = messages
            .iter()
            .find_map(|m| m.extras.mentions.first())
            .expect("a mention");
        assert_eq!(mention.name.as_deref(), Some("Noah Fischer"));
    });
    // Drawn without a hitch, examples included.
    assert!(shows(harness.window, "bubble", cx));
}

#[gpui_kit::test]
fn a_vote_shows_at_once_reaches_the_provider_and_can_be_changed(cx: &mut TestAppContext) {
    let (harness, poll) = open_with_poll(cx);
    let MessageContent::Poll(before) = &poll.content else {
        unreachable!()
    };
    assert!(shows(harness.window, "poll-tile", cx));
    assert!(!shows(harness.window, "poll-chosen", cx), "no vote yet");

    // The last option is the one painted last: a click on it votes.
    let last = before.options.last().unwrap().clone();
    click(harness.window, "poll-option", cx);
    let voted = shown_poll(&harness, cx);
    assert_eq!(voted.chosen, Some(vec![last.name.clone()]), "at once");
    assert_eq!(voted.options.last().unwrap().votes, last.votes + 1);
    assert_eq!(voted.voters, before.voters + 1);
    assert!(shows(harness.window, "poll-chosen", cx));
    assert!(harness.mock.votes().is_empty(), "the network comes later");

    harness.settle(cx);
    assert_eq!(
        harness.mock.votes(),
        [(poll.id.clone(), vec![last.name.clone()])]
    );
    assert_eq!(
        shown_poll(&harness, cx).chosen,
        Some(vec![last.name.clone()])
    );

    // A click on the chosen option of a single-choice poll takes it back.
    click(harness.window, "poll-option", cx);
    harness.settle(cx);
    let back = shown_poll(&harness, cx);
    assert_eq!(back.chosen, Some(Vec::new()));
    assert_eq!(back.options.last().unwrap().votes, last.votes);
    assert_eq!(harness.mock.votes().len(), 2);
    assert!(!shows(harness.window, "poll-chosen", cx));
}

#[gpui_kit::test]
fn a_vote_the_provider_refuses_is_taken_back_and_said(cx: &mut TestAppContext) {
    let (harness, poll) = open_with_poll(cx);
    let MessageContent::Poll(before) = &poll.content else {
        unreachable!()
    };
    harness
        .mock
        .fail_next_votes([rejected("poll_closed", "The poll is closed.")]);
    click(harness.window, "poll-option", cx);
    assert!(shows(harness.window, "poll-chosen", cx));
    harness.settle(cx);

    let after = shown_poll(&harness, cx);
    assert_eq!(after.options, before.options, "the tally is as it was");
    assert_eq!(after.chosen, Some(Vec::new()));
    assert!(!shows(harness.window, "poll-chosen", cx));
    let said = cx.update(|cx| harness.shell.read(cx).problem.clone());
    let said = said.expect("the refusal is said").to_string();
    assert!(said.contains("not recorded"), "{said}");
}

#[gpui_kit::test]
fn a_poll_that_has_not_been_sent_yet_cannot_be_voted_on(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let mut pending = push(
        &harness,
        cx,
        "local:abc",
        MessageContent::Poll(poll(1, &[("Yes", 0), ("No", 0)], 0)),
        MessageExtras::default(),
        true,
    );
    click(harness.window, "poll-option", cx);
    harness.settle(cx);
    assert!(harness.mock.votes().is_empty());
    assert_eq!(shown_poll(&harness, cx).chosen, Some(Vec::new()));

    // A multiple-choice poll that takes two answers refuses a third.
    pending.id = MessageId::new("m-two");
    pending.content = MessageContent::Poll(Poll {
        chosen: Some(vec!["A".into(), "B".into()]),
        ..poll(2, &[("A", 1), ("B", 1), ("C", 0)], 1)
    });
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(pending))
        .unwrap();
    cx.run_until_parked();
    click(harness.window, "poll-option", cx);
    harness.settle(cx);
    assert!(harness.mock.votes().is_empty(), "nothing was asked");
    let said = cx.update(|cx| harness.shell.read(cx).problem.clone());
    assert!(said.unwrap().contains("2 answers"));
}

#[gpui_kit::test]
fn a_contact_card_copies_its_number_and_opens_the_chat_with_it(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let account = stored_accounts(&harness)[0].id.clone();
    push(
        &harness,
        cx,
        "card-1",
        MessageContent::Contacts {
            cards: vec![ContactCard {
                name: "Rui (driver)".into(),
                phones: vec!["+351 912 345 678".into()],
            }],
        },
        MessageExtras::default(),
        false,
    );

    click(harness.window, "contact-copy", cx);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("+351 912 345 678".to_owned())
    );
    // Said for a moment, then the label is back.
    let copied = |cx: &mut TestAppContext| {
        cx.update(|cx| harness.shell.read(cx).tile_context().copied_key())
    };
    assert_eq!(copied(cx).as_deref(), Some("card-1/0"));
    cx.executor().advance_clock(Duration::from_secs(3));
    cx.run_until_parked();
    assert_eq!(copied(cx), None);

    // "Message" asks the provider about the number, then opens the chat.
    let checks = harness.mock.check_calls();
    click(harness.window, "contact-message", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.check_calls(), checks + 1);
    let open = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    assert_eq!(open.id.as_str(), "+351912345678");
    assert!(harness
        .engine
        .store()
        .chat(&account, &open.id)
        .unwrap()
        .is_some());
    assert_eq!(harness.mock.send_calls(), 0, "nothing was sent");
}

#[gpui_kit::test]
fn a_card_of_someone_in_the_address_book_opens_their_chat_without_asking(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let account = stored_accounts(&harness)[0].id.clone();
    let mut ana = client_provider::Contact::new(account.clone(), ContactId::new("+584140000001"));
    ana.phone = Some("+584140000001".into());
    ana.saved_name = Some("Ana Pérez".into());
    harness.mock.set_contacts(&account, vec![ana]);
    harness
        .runtime
        .block_on(harness.engine.sync_contacts(&account))
        .unwrap();
    cx.run_until_parked();

    push(
        &harness,
        cx,
        "card-2",
        MessageContent::Contacts {
            cards: vec![ContactCard {
                name: "Ana".into(),
                phones: vec!["+58 414-000 0001".into()],
            }],
        },
        MessageExtras::default(),
        true,
    );
    let checks = harness.mock.check_calls();
    click(harness.window, "contact-message", cx);
    harness.settle(cx);
    let open = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    assert_eq!(open.id.as_str(), "+584140000001");
    assert_eq!(open.title, "Ana Pérez");
    assert_eq!(harness.mock.check_calls(), checks, "the contact was known");

    // A number that has no WhatsApp opens nothing, and says so.
    harness.mock.without_whatsapp("+351900000000");
    push(
        &harness,
        cx,
        "card-3",
        MessageContent::Contacts {
            cards: vec![ContactCard {
                name: "Nobody".into(),
                phones: vec!["+351900000000".into()],
            }],
        },
        MessageExtras::default(),
        false,
    );
    click(harness.window, "contact-message", cx);
    harness.settle(cx);
    let (open, said) = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        (
            shell.open.as_ref().unwrap().chat.id.clone(),
            shell.problem.clone(),
        )
    });
    assert_eq!(open.as_str(), "+584140000001", "still the same chat");
    assert!(said.unwrap().contains("not on WhatsApp"));
}

#[gpui_kit::test]
fn several_cards_are_a_short_list_until_view_all(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    push(
        &harness,
        cx,
        "cards-1",
        MessageContent::Contacts { cards: cards(5) },
        MessageExtras::default(),
        false,
    );
    let short = bounds(harness.window, "contact-tile", cx);
    click(harness.window, "contacts-view-all", cx);
    let all = bounds(harness.window, "contact-tile", cx);
    assert!(
        all.size.height > short.size.height + px(40.),
        "two more cards are listed: {short:?} then {all:?}"
    );
    // Still inside its bubble, and the bubble still above the composer.
    let bubble = bounds(harness.window, "bubble", cx);
    let thread = bounds(harness.window, "thread", cx);
    assert!(within(all, bubble) && within(bubble, thread));
    click(harness.window, "contacts-view-all", cx);
    let again = bounds(harness.window, "contact-tile", cx);
    assert_eq!(again.size.height, short.size.height);
}

#[gpui_kit::test]
fn a_place_and_a_link_open_in_the_browser_only_on_a_click(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let media = harness.mock.media_calls();
    push(
        &harness,
        cx,
        "place-1",
        MessageContent::Location(Location {
            point: lisbon(),
            name: Some("Time Out Market".into()),
            address: None,
            live: false,
        }),
        MessageExtras::default(),
        false,
    );
    harness.settle(cx);
    // Drawing it asked nothing of anyone.
    assert_eq!(cx.opened_url(), None);
    assert_eq!(harness.mock.media_calls(), media);

    click(harness.window, "location-open", cx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some(
            "https://www.openstreetmap.org/?mlat=38.706930&mlon=-9.145770\
             #map=16/38.706930/-9.145770"
        )
    );

    // A link's preview comes from the message; its host opens the link.
    push(
        &harness,
        cx,
        "link-1",
        MessageContent::text("look"),
        MessageExtras {
            link: Some(LinkPreview {
                url: "https://www.example.com/launch?x=1".into(),
                title: Some("Launch".into()),
                description: None,
                thumbnail: None,
            }),
            ..Default::default()
        },
        true,
    );
    harness.settle(cx);
    assert_eq!(
        harness.mock.media_calls(),
        media,
        "the page is never fetched"
    );
    click(harness.window, "link-host", cx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://www.example.com/launch?x=1")
    );

    // A preview of something a browser does not open has no link at all.
    push(
        &harness,
        cx,
        "link-2",
        MessageContent::text("look"),
        MessageExtras {
            link: Some(LinkPreview {
                url: "file:///etc/passwd".into(),
                title: Some("Launch".into()),
                description: None,
                thumbnail: None,
            }),
            ..Default::default()
        },
        false,
    );
    assert!(shows(harness.window, "link-card", cx));
    let card = bounds(harness.window, "link-card", cx);
    let host = VisualTestContext::from_window(harness.window.into(), cx).debug_bounds("link-host");
    assert!(
        host.is_none_or(|host| !within(host, card)),
        "no clickable host on that card"
    );

    // A link in the text itself is clickable.
    push(
        &harness,
        cx,
        "link-3",
        MessageContent::text("https://wuapi.dev/docs"),
        MessageExtras::default(),
        false,
    );
    click(harness.window, "message-text", cx);
    assert_eq!(cx.opened_url().as_deref(), Some("https://wuapi.dev/docs"));
}

#[gpui_kit::test]
fn a_view_once_file_is_a_tile_and_is_never_downloaded(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let media = harness.mock.media_calls();
    for (index, kind) in [MediaKind::Image, MediaKind::Video, MediaKind::Voice]
        .into_iter()
        .enumerate()
    {
        let mut file = Media::new(kind);
        file.source = Some(MediaRef::new(format!("https://media.example/once-{index}")));
        file.caption = Some("for your eyes only".into());
        push(
            &harness,
            cx,
            &format!("once-{index}"),
            MessageContent::Media(file),
            MessageExtras {
                view_once: true,
                ..Default::default()
            },
            index == 1,
        );
        harness.settle(cx);
        let tile = bounds(harness.window, "view-once-tile", cx);
        let bubble = bounds(harness.window, "bubble", cx);
        assert!(within(tile, bubble), "{kind:?}");
    }
    assert_eq!(harness.mock.media_calls(), media, "nothing was fetched");
    // And the chat list does not give its caption away either.
    let preview = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let open = shell.open.as_ref().unwrap().chat.id.clone();
        shell.list_rows.iter().find_map(|row| match row {
            ListRow::Chat(chat) if chat.id == open => {
                chat.last_message.as_ref().map(|last| last.text.clone())
            }
            _ => None,
        })
    });
    assert_eq!(preview.as_deref(), Some("View once voice message"));
}

#[gpui_kit::test]
fn a_mention_reads_as_the_saved_name_and_marks_stay_on_the_bubble(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let account = stored_accounts(&harness)[0].id.clone();
    let mut ana = client_provider::Contact::new(account.clone(), ContactId::new("+584140000001"));
    ana.saved_name = Some("Ana Pérez".into());
    harness.engine.store().upsert_contact(&ana).unwrap();

    let mut message = push(
        &harness,
        cx,
        "mention-1",
        MessageContent::text("@584140000001 can you check?"),
        MessageExtras {
            mentions: vec![Mention {
                id: ContactId::new("+584140000001"),
                handle: "584140000001".into(),
                name: None,
                me: false,
            }],
            forwarded: true,
            starred: true,
            ..Default::default()
        },
        false,
    );
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let rows = &shell.open.as_ref().unwrap().rows;
        let Some(Row::Message(row)) = rows.last() else {
            panic!("a message")
        };
        assert_eq!(
            row.stored.message.extras.mentions[0].name.as_deref(),
            Some("Ana Pérez")
        );
    });
    assert!(shows(harness.window, "forwarded-label", cx));
    assert!(shows(harness.window, "starred-mark", cx));
    assert!(!shows(harness.window, "edited-label", cx));

    // Edited on the phone, unstarred: the row follows.
    message.edited = true;
    message.extras.starred = false;
    message.content = MessageContent::text("@584140000001 can you check today?");
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message.clone()))
        .unwrap();
    cx.run_until_parked();
    assert!(shows(harness.window, "edited-label", cx));
    let bubble = bounds(harness.window, "bubble", cx);
    let star =
        VisualTestContext::from_window(harness.window.into(), cx).debug_bounds("starred-mark");
    assert!(star.is_none_or(|star| !within(star, bubble)), "no star now");

    // Deleted for everyone: a placeholder, and nothing of what it was.
    message.deleted = true;
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    cx.run_until_parked();
    let bubble = bounds(harness.window, "bubble", cx);
    for gone in ["forwarded-label", "message-text", "edited-label"] {
        let found = VisualTestContext::from_window(harness.window.into(), cx).debug_bounds(gone);
        assert!(found.is_none_or(|found| !within(found, bubble)), "{gone}");
    }
}

#[test]
fn an_event_is_written_as_a_private_ics_file() {
    let dir = tempfile::tempdir().unwrap();
    let exports = dir.path().join("opened");
    let path = tiles::export_event(&exports, &dinner(), &MessageId::new("m/../17")).unwrap();
    assert_eq!(path, exports.join("events").join("event-m17.ics"));
    let file = std::fs::read_to_string(&path).unwrap();
    assert!(file.starts_with("BEGIN:VCALENDAR\r\n"));
    assert!(file.contains("DTSTART:20261003T190000Z"));
    assert!(file.contains("UID:m..17@inbox.wuapi.dev"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&exports), 0o700);
        assert_eq!(mode(&exports.join("events")), 0o700);
    }
    // Without a start there is nothing a calendar could take.
    let mut undated = dinner();
    undated.starts_at = None;
    assert!(tiles::export_event(&exports, &undated, &MessageId::new("m18")).is_err());
}

#[test]
fn the_map_link_names_the_place_and_nothing_else() {
    assert_eq!(
        tiles::map_url(GeoPoint::new(-33.45, -70.666_67).unwrap()),
        "https://www.openstreetmap.org/?mlat=-33.450000&mlon=-70.666670\
         #map=16/-33.450000/-70.666670"
    );
}

#[gpui_kit::test]
fn a_poll_is_written_in_its_form_and_sent_through_the_outbox(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    click(harness.window, "chat-menu", cx);
    click(harness.window, "menu-poll", cx);
    assert!(shows(harness.window, "poll-form", cx));
    let form = bounds(harness.window, "poll-form", cx);
    for part in ["poll-form-question", "poll-form-option-2", "poll-form-send"] {
        assert!(within(bounds(harness.window, part, cx), form), "{part}");
    }

    // Nothing typed yet: it says what is missing and stays open.
    click(harness.window, "poll-form-send", cx);
    assert!(shows(harness.window, "poll-form-error", cx));
    assert_eq!(harness.engine.store().outbox_pending().unwrap().len(), 0);

    // The keyboard starts in the question; Enter walks the options.
    type_text(harness.window, "Lunch?", cx);
    assert!(!shows(harness.window, "poll-form-error", cx));
    press(harness.window, "enter", cx);
    type_text(harness.window, "Pizza", cx);
    press(harness.window, "enter", cx);
    type_text(harness.window, "Sushi", cx);
    click(harness.window, "poll-form-multiple", cx);
    click(harness.window, "poll-form-send", cx);
    assert!(!shows(harness.window, "poll-form", cx));

    // The bubble is there before the network.
    let pending = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        match shell.open.as_ref().unwrap().rows.last() {
            Some(Row::Message(row)) => row.stored.message.clone(),
            _ => panic!("the poll is the newest row"),
        }
    });
    assert_eq!(pending.status, DeliveryStatus::Pending);
    let MessageContent::Poll(poll) = &pending.content else {
        panic!("a poll, got {:?}", pending.content)
    };
    assert_eq!(poll.question, "Lunch?");
    let names: Vec<&str> = poll
        .options
        .iter()
        .map(|option| option.name.as_str())
        .collect();
    assert_eq!(names, ["Pizza", "Sushi"]);
    assert!(poll.multiple_choice());
    assert!(shows(harness.window, "poll-tile", cx));
    assert_eq!(harness.mock.send_calls(), 0);

    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(harness.mock.send_calls(), 1);

    // Escape leaves the form without sending anything.
    click(harness.window, "chat-menu", cx);
    click(harness.window, "menu-poll", cx);
    type_text(harness.window, "Dinner?", cx);
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "poll-form", cx));
    assert_eq!(harness.engine.store().outbox_pending().unwrap().len(), 0);
}

/// WCAG 2.x contrast ratio between two opaque colours.
fn contrast(a: gpui_kit::Hsla, b: gpui_kit::Hsla) -> f32 {
    fn luminance(colour: gpui_kit::Hsla) -> f32 {
        let rgba = gpui_kit::Rgba::from(colour);
        let linear = |c: f32| {
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
    }
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

#[test]
fn links_mentions_and_chosen_options_stay_legible_on_both_bubbles() {
    for palette in [theme::Palette::light(), theme::Palette::dark()] {
        let which = palette.appearance;
        // What a tile draws with, per side: (signal, bubble, tile fill).
        for (side, signal, bubble, fill) in [
            (
                "incoming",
                palette.accent,
                palette.bubble_in,
                palette.quote_in,
            ),
            (
                "outgoing",
                palette.signal_out,
                palette.bubble_out,
                palette.quote_out,
            ),
        ] {
            let on_bubble = contrast(signal, bubble);
            assert!(
                on_bubble >= 4.5,
                "{which:?} {side}: a link is {on_bubble:.2}:1 on its bubble"
            );
            let on_tile = contrast(signal, fill);
            assert!(
                on_tile >= 3.0,
                "{which:?} {side}: a chosen option's mark is {on_tile:.2}:1 on its tile"
            );
        }
        // A notice is muted text on the quiet fill.
        assert!(
            contrast(palette.text_muted, palette.muted) >= 4.5,
            "{which:?}"
        );
    }
}

#[gpui_kit::test]
fn a_view_once_voice_note_does_not_follow_the_one_before_it(cx: &mut TestAppContext) {
    let (harness, tape, notes) = with_voice_notes(cx);
    let once = "https://media.example/voice-once.ogg";
    harness
        .mock
        .set_media(once, VOICE_NOTE.to_vec(), "audio/ogg; codecs=opus");
    let mut file = Media::new(MediaKind::Voice);
    file.source = Some(MediaRef::new(once));
    push(
        &harness,
        cx,
        "m-voice-once",
        MessageContent::Media(file),
        MessageExtras {
            view_once: true,
            ..Default::default()
        },
        false,
    );

    // The last ordinary note, played to its end.
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[2].clone(), cx));
    until(&harness, cx, |shell| shell.audio.player.is_playing());
    let downloads = harness.mock.media_calls();
    tape.borrow_mut().advance(Duration::from_secs(10));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    harness.settle(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.audio.player.current(), None, "nothing follows");
        assert!(shell.audio.waiting.is_none());
    });
    assert_eq!(
        harness.mock.media_calls(),
        downloads,
        "and nothing was fetched"
    );
}
