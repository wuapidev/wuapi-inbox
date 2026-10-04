//! What stands on and under a bubble and is the way to more: the arrow of
//! its menu, which is over whatever the bubble holds, and its reactions,
//! which say who made them.

use super::super::reactors::{names_line, Shown};
use super::keyboard::{focused, open_chat_one, open_ids, overlay, put, row, text_message};
use super::*;
use client_provider::{Contact, ContactId, MessageId, ProviderEvent, ReplyRef};
use gpui_kit::point;

/// Somebody reacts to a message of the open chat: the account itself when
/// `sender` is "me".
fn react(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    target: &str,
    sender: &str,
    emoji: &str,
    at: i64,
) {
    let (account, chat) = open_ids(harness, cx);
    let mut reaction = text_message(&account, &chat, id, sender == "me", "", at);
    reaction.sender = ContactId::new(sender);
    reaction.content = MessageContent::Reaction {
        target: MessageId::new(target),
        emoji: emoji.to_owned(),
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(reaction))
        .unwrap();
    cx.run_until_parked();
}

/// The address book names somebody.
fn name(harness: &Harness, cx: &mut TestAppContext, id: &str, name: &str) {
    let (account, _) = open_ids(harness, cx);
    let mut contact = Contact::new(account, ContactId::new(id));
    contact.name = Some(name.to_owned());
    contact.saved_name = Some(name.to_owned());
    harness
        .engine
        .apply_event(ProviderEvent::ContactUpdated(contact))
        .unwrap();
    cx.run_until_parked();
}

/// The open chat's name: in a chat of two, what the other one is called.
fn title(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .unwrap()
            .chat
            .title
            .clone()
    })
}

/// The lines of the panel, as it draws them.
fn shown(harness: &Harness, cx: &mut TestAppContext) -> Vec<(String, String, bool)> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .reactors_shown()
            .into_iter()
            .map(
                |Shown {
                     name, emoji, mine, ..
                 }| (name, emoji, mine),
            )
            .collect()
    })
}

/// What the pointer on a reaction of a message says.
fn hint(harness: &Harness, cx: &mut TestAppContext, id: &str, emoji: &str) -> String {
    let stored = row(harness, cx, id).unwrap();
    let reaction = stored
        .reactions
        .iter()
        .find(|reaction| reaction.emoji == emoji)
        .unwrap();
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .reactors_hint(&stored.message, reaction)
    })
}

#[gpui_kit::test]
fn the_arrow_of_a_message_is_over_its_quote(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(&harness, cx, "q1", false, "shall we meet at noon?", 1);
    let (account, chat) = open_ids(&harness, cx);
    let mut answer = text_message(&account, &chat, "a1", false, "then it is settled", 2);
    answer.reply_to = Some(ReplyRef {
        message_id: MessageId::new("q1"),
        sender_name: None,
        preview: Some("shall we meet at noon?".into()),
    });
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(answer))
        .unwrap();
    cx.run_until_parked();

    // The arrow is on the corner of the newest bubble, and so is its
    // quote: they share room.
    let bubble = bounds(harness.window, "bubble-a1", cx);
    let arrow = bounds(harness.window, "message-arrow", cx);
    let quote = bounds(harness.window, "quote", cx);
    assert!(within(arrow, bubble) && within(quote, bubble));
    let shared = arrow.intersect(&quote);
    assert!(
        shared.size.width > px(4.) && shared.size.height > px(4.),
        "the arrow {arrow:?} and the quote {quote:?} do not meet"
    );

    // A click where they meet is the arrow's: the menu opens, and nothing
    // jumps to the message that is quoted.
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_move(shared.center(), None, Modifiers::none());
    visual.run_until_parked();
    visual.simulate_click(shared.center(), Modifiers::none());
    visual.run_until_parked();
    assert_eq!(overlay(&harness, cx), Overlay::MessageMenu);
    assert_eq!(focused(&harness, cx).as_deref(), Some("a1"));
    assert!(!shows(harness.window, "message-flash", cx));
    press(harness.window, "escape", cx);

    // The rest of the quote is still the way to what it quotes.
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_click(
        point(quote.left() + px(12.), quote.center().y),
        Modifiers::none(),
    );
    visual.run_until_parked();
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(shows(harness.window, "message-flash", cx));
}

#[gpui_kit::test]
fn a_click_on_a_reaction_lists_who_reacted(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(&harness, cx, "r1", false, "we won!", 1);
    react(&harness, cx, "x1", "r1", "them", "👍", 2);
    react(&harness, cx, "x2", "r1", "me", "❤️", 3);
    let other = title(&harness, cx);

    // Their reaction: the panel opens by the message, with the emoji that
    // was clicked first. In a chat of two, the other one has the chat's name.
    click(harness.window, "reaction-chip", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Reactors);
    assert_eq!(focused(&harness, cx).as_deref(), Some("r1"));
    assert_eq!(
        shown(&harness, cx),
        vec![
            (other.clone(), "👍".to_owned(), false),
            ("You".to_owned(), "❤️".to_owned(), true),
        ]
    );
    let card = bounds(harness.window, "reactors", cx);
    assert!(within(card, bounds(harness.window, "overlay", cx)));
    for index in 0..2 {
        let line = bounds_of(harness.window, &format!("reactor-{index}"), cx);
        let emoji = bounds_of(harness.window, &format!("reactor-emoji-{index}"), cx);
        assert!(within(line, card) && within(emoji, line), "{index}");
    }
    assert!(!shows(harness.window, "reactor-2", cx));
    // Nothing was sent: looking is not reacting.
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());

    // Escape closes it.
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(!shows(harness.window, "reactors", cx));

    // One's own reaction: that one first.
    click(harness.window, "reaction-mine", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Reactors);
    assert_eq!(
        shown(&harness, cx),
        vec![
            ("You".to_owned(), "❤️".to_owned(), true),
            (other, "👍".to_owned(), false),
        ]
    );

    // A click outside closes it too.
    let layer = bounds(harness.window, "overlay", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_click(
        point(layer.left() + px(4.), layer.top() + px(4.)),
        Modifiers::none(),
    );
    visual.run_until_parked();
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

#[gpui_kit::test]
fn ones_own_line_of_the_panel_takes_the_reaction_back(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(&harness, cx, "r1", false, "we won!", 1);
    react(&harness, cx, "x1", "r1", "them", "👍", 2);
    // The account reacts from here: "+" opens the six, a digit picks.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    type_text(harness.window, "+", cx);
    press(harness.window, "2", cx);
    assert_eq!(row(&harness, cx, "r1").unwrap().reactions.len(), 2);

    // One's own is the first line of its panel.
    click(harness.window, "reaction-mine", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Reactors);
    click(harness.window, "reactor-0", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    let reactions = row(&harness, cx, "r1").unwrap().reactions;
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].emoji, "👍");
    assert_eq!(
        harness
            .engine
            .store()
            .outbox_pending()
            .unwrap()
            .last()
            .unwrap()
            .message
            .content,
        OutgoingContent::Reaction {
            target: MessageId::new("r1"),
            emoji: String::new()
        }
    );
}

#[gpui_kit::test]
fn w_lists_who_reacted_to_the_message_in_focus(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(&harness, cx, "r1", false, "we won!", 1);
    put(&harness, cx, "r2", false, "nobody cares", 2);
    react(&harness, cx, "x1", "r1", "them", "👍", 3);
    focus_composer(&harness, cx);

    // Nobody reacted to the newest one: there is nothing to list.
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("r2"));
    press(harness.window, "w", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);

    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("r1"));
    press(harness.window, "w", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Reactors);
    assert!(shows(harness.window, "reactor-0", cx));
    assert_eq!(shown(&harness, cx).len(), 1);
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(
        focused(&harness, cx).as_deref(),
        Some("r1"),
        "the message keeps the keyboard"
    );

    // The message's menu offers it as well.
    press(harness.window, "m", cx);
    assert!(shows(harness.window, "message-reactors", cx));
    click(harness.window, "message-reactors", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Reactors);
}

#[test]
fn a_hint_names_one_two_three_and_then_counts() {
    let names =
        |names: &[&str]| -> Vec<String> { names.iter().map(|&name| name.to_owned()).collect() };
    assert_eq!(names_line(&names(&["Ana"])), "Ana");
    assert_eq!(names_line(&names(&["Ana", "Luis"])), "Ana and Luis");
    assert_eq!(
        names_line(&names(&["Ana", "Luis", "Eva"])),
        "Ana, Luis and Eva"
    );
    assert_eq!(
        names_line(&names(&["Ana", "Luis", "Eva", "Rui"])),
        "Ana, Luis and 2 more"
    );
    assert_eq!(
        names_line(&names(&["Ana", "Luis", "Eva", "Rui", "Ivo"])),
        "Ana, Luis and 3 more"
    );
    assert_eq!(names_line(&[]), "");
}

#[gpui_kit::test]
fn the_hint_of_a_reaction_names_who_made_it(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let other = title(&harness, cx);
    put(&harness, cx, "r1", false, "we won!", 1);
    name(&harness, cx, "ana", "Ana Lima");
    name(&harness, cx, "luis", "Luis Paz");

    // One: in a chat of two, the other one by the chat's name.
    react(&harness, cx, "x1", "r1", "them", "👍", 2);
    assert_eq!(hint(&harness, cx, "r1", "👍"), other);
    // Two, in the order they reacted; people are called what the address
    // book calls them.
    react(&harness, cx, "x2", "r1", "ana", "👍", 3);
    assert_eq!(
        hint(&harness, cx, "r1", "👍"),
        format!("{other} and Ana Lima")
    );
    // The account itself is "You", and comes first.
    react(&harness, cx, "x3", "r1", "me", "👍", 4);
    assert_eq!(
        hint(&harness, cx, "r1", "👍"),
        format!("You, {other} and Ana Lima")
    );
    // Many: two names and how many more.
    react(&harness, cx, "x4", "r1", "luis", "👍", 5);
    // In a chat of two, an id the store has no name for is the other
    // one's: they go by more than one.
    react(&harness, cx, "x5", "r1", "lid:77", "👍", 6);
    assert_eq!(
        hint(&harness, cx, "r1", "👍"),
        format!("You, {other} and 3 more")
    );
    // Another emoji has its own people.
    react(&harness, cx, "x6", "r1", "luis", "🔥", 7);
    assert_eq!(hint(&harness, cx, "r1", "🔥"), "Luis Paz");
    assert_eq!(
        hint(&harness, cx, "r1", "👍"),
        format!("You, {other} and 2 more")
    );

    // The panel has every one of them, the emoji that was clicked first.
    click(harness.window, "reaction-chip", cx);
    assert_eq!(
        shown(&harness, cx),
        vec![
            ("Luis Paz".to_owned(), "🔥".to_owned(), false),
            ("You".to_owned(), "👍".to_owned(), true),
            (other.clone(), "👍".to_owned(), false),
            ("Ana Lima".to_owned(), "👍".to_owned(), false),
            (other.clone(), "👍".to_owned(), false),
        ]
    );
}

#[gpui_kit::test]
fn in_a_group_people_go_by_their_names_and_strangers_by_their_number(cx: &mut TestAppContext) {
    use provider_mock::{SHOWCASE_ACCOUNT, SHOWCASE_CHAT};
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_chat(cx);
            shell.open_chat(client_provider::ChatId::new(SHOWCASE_CHAT), None, cx);
        })
    });
    // The group's participants are read when its chat opens.
    harness.settle(cx);
    let noah = harness
        .engine
        .store()
        .group_participants(
            &client_provider::AccountId::new(SHOWCASE_ACCOUNT),
            &client_provider::ChatId::new(SHOWCASE_CHAT),
            None,
            100,
        )
        .unwrap()
        .into_iter()
        .find(|person| person.name == "Noah Fischer")
        .expect("Noah is in the group");

    put(&harness, cx, "g1", true, "who is in?", 1);
    react(&harness, cx, "y1", "g1", noah.contact.as_str(), "👍", 2);
    assert_eq!(hint(&harness, cx, "g1", "👍"), "Noah Fischer");
    // Somebody the group does not name: their number, as people write
    // it; and with nothing but an id, the id. Never the group's name.
    react(&harness, cx, "y2", "g1", "+351910000001", "👍", 3);
    react(&harness, cx, "y3", "g1", "lid:999", "👍", 4);
    assert_eq!(
        hint(&harness, cx, "g1", "👍"),
        format!(
            "Noah Fischer, {} and lid:999",
            crate::format::phone("+351910000001")
        )
    );
}
