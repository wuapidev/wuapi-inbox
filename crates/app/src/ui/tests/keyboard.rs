//! The keyboard-first layer over a conversation, in the real window:
//! selecting and copying text, moving through the messages and acting on
//! the one in focus, the message menu, the command palette and the
//! shortcuts themselves.

use super::*;
use client_provider::{AccountId, ChatId, ContactId, Message, MessageId, ProviderEvent, Timestamp};
use gpui_kit::point;

/// A text message for the open chat, `at` seconds from now; from the
/// other side unless `mine`.
pub(super) fn text_message(
    account: &AccountId,
    chat: &ChatId,
    id: &str,
    mine: bool,
    body: &str,
    at: i64,
) -> Message {
    Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: account.clone(),
        chat_id: chat.clone(),
        sender: ContactId::new(if mine { "me" } else { "them" }),
        sender_name: None,
        direction: if mine {
            Direction::Outgoing
        } else {
            Direction::Incoming
        },
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 120_000 + at * 1_000),
        content: MessageContent::text(body),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: Default::default(),
    }
}

pub(super) fn open_chat_one(cx: &mut TestAppContext) -> Harness {
    open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    )
}

pub(super) fn open_ids(harness: &Harness, cx: &mut TestAppContext) -> (AccountId, ChatId) {
    cx.update(|cx| {
        let chat = &harness.shell.read(cx).open.as_ref().unwrap().chat;
        (chat.account_id.clone(), chat.id.clone())
    })
}

/// A message arrives in (or is sent from) the open chat.
pub(super) fn put(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    mine: bool,
    body: &str,
    at: i64,
) {
    let (account, chat) = open_ids(harness, cx);
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(text_message(
            &account, &chat, id, mine, body, at,
        )))
        .unwrap();
    cx.run_until_parked();
}

fn drag(
    harness: &Harness,
    cx: &mut TestAppContext,
    from: Point<gpui_kit::Pixels>,
    to: Point<gpui_kit::Pixels>,
) {
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.drag(from, to, cx)
    })
    .unwrap();
    cx.run_until_parked();
}

fn clipboard(cx: &mut TestAppContext) -> String {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default()
}

// ----- selecting and copying text ---------------------------------------------

#[gpui_kit::test]
fn part_of_a_messages_text_is_selected_with_the_mouse_and_copied(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let written = "alpha bravo charlie delta echo";
    put(&harness, cx, "s1", false, written, 1);
    let body = bounds(harness.window, "message-text", cx);
    let bubble = bounds(harness.window, "bubble", cx);
    let middle = body.top() + body.size.height / 2.;

    // A drag over the first half of the line selects that part: Ctrl+C
    // copies it, and only it.
    drag(
        &harness,
        cx,
        point(body.left() + px(1.), middle),
        point(body.left() + body.size.width / 2., middle),
    );
    press(harness.window, "ctrl-c", cx);
    let part = clipboard(cx);
    assert!(
        !part.is_empty() && part.len() < written.len() && written.starts_with(&part),
        "copied {part:?}"
    );
    // Selecting moved nothing.
    assert_eq!(bounds(harness.window, "bubble", cx), bubble);
    assert_eq!(bounds(harness.window, "message-text", cx), body);

    // A drag over all of it copies all of it.
    drag(
        &harness,
        cx,
        point(body.left() + px(1.), middle),
        point(body.right() + px(20.), middle),
    );
    press(harness.window, "ctrl-c", cx);
    assert_eq!(clipboard(cx), written);
}

#[gpui_kit::test]
fn a_copy_is_the_text_as_it_was_written_and_reaches_across_messages(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(&harness, cx, "m1", false, "first *bold* line", 1);
    let first = bounds(harness.window, "message-text", cx);
    put(&harness, cx, "m2", true, "second _one_ here", 2);
    let second = bounds(harness.window, "message-text", cx);
    let first = Bounds {
        // The first message moved up when the second arrived.
        origin: point(
            first.left(),
            bounds(harness.window, "row-m1", cx).bottom() - px(8.) - first.size.height,
        ),
        size: first.size,
    };

    // One message, with its markup back around what it styles.
    let line = second.top() + second.size.height / 2.;
    drag(
        &harness,
        cx,
        point(second.left() + px(1.), line),
        point(second.right() + px(10.), line),
    );
    press(harness.window, "ctrl-c", cx);
    assert_eq!(clipboard(cx), "second _one_ here");

    // From inside the first message to the end of the second: both, in
    // order, one per line.
    drag(
        &harness,
        cx,
        point(first.left() + px(1.), first.top() + first.size.height / 2.),
        point(second.right() + px(10.), line),
    );
    press(harness.window, "ctrl-c", cx);
    assert_eq!(clipboard(cx), "first *bold* line\nsecond _one_ here");

    // A click elsewhere lets the selection go: nothing new is copied.
    let thread = bounds(harness.window, "thread", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_click(
        point(thread.left() + px(4.), thread.top() + px(4.)),
        Modifiers::none(),
    );
    visual.run_until_parked();
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("untouched".into()));
    press(harness.window, "ctrl-c", cx);
    assert_eq!(clipboard(cx), "untouched");
}

// ----- which message has the keyboard ------------------------------------------

/// The message in focus, by id.
pub(super) fn focused(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .focused_message()
            .map(|message| message.id.to_string())
    })
}

fn composer_text(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| harness.shell.read(cx).composer.read(cx).value().to_string())
}

pub(super) fn overlay(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

/// A stored message of the open chat, as the window holds it.
pub(super) fn row(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
) -> Option<client_core::StoredMessage> {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell.open.as_ref()?.rows.iter().find_map(|row| match row {
            Row::Message(row) if row.stored.message.id.as_str() == id => Some(row.stored.clone()),
            _ => None,
        })
    })
}

/// A text of the account's own that went through the outbox and the
/// provider, so it has an id there. Returns that id.
fn send(harness: &Harness, cx: &mut TestAppContext, body: &str) -> String {
    let (account, chat) = open_ids(harness, cx);
    harness
        .engine
        .send_text(&account, &chat, body.to_owned(), None)
        .unwrap();
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .rev()
            .find_map(|row| match row {
                Row::Message(row) if row.stored.message.direction == Direction::Outgoing => {
                    Some(row.stored.message.id.to_string())
                }
                _ => None,
            })
            .expect("the sent message")
    })
}

#[gpui_kit::test]
fn the_arrows_move_through_the_messages_and_escape_returns_to_the_composer(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    for (n, mine) in [(1, false), (2, true), (3, false), (4, false), (5, true)] {
        put(
            &harness,
            cx,
            &format!("k{n}"),
            mine,
            &format!("message {n}"),
            n,
        );
    }
    focus_composer(&harness, cx);
    assert_eq!(focused(&harness, cx), None);
    let newest_row = bounds(harness.window, "row-k5", cx);
    assert!(!shows(harness.window, "message-focus", cx));

    // Up in an empty composer: the newest message has the keyboard, with
    // an outline around its bubble that moved nothing.
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("k5"));
    let ring = bounds(harness.window, "message-focus", cx);
    let bubble = bounds(harness.window, "bubble", cx);
    assert!(within(bubble, ring) && ring.size.width > bubble.size.width);
    assert!(ring.size.width < bubble.size.width + px(12.));
    assert_eq!(bounds(harness.window, "row-k5", cx), newest_row);

    // Up and down walk the messages; Home and End jump.
    press(harness.window, "up", cx);
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("k3"));
    press(harness.window, "down", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("k4"));
    press(harness.window, "end", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("k5"));
    press(harness.window, "down", cx);
    assert_eq!(
        focused(&harness, cx).as_deref(),
        Some("k5"),
        "the end is the end"
    );
    press(harness.window, "home", cx);
    let oldest = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .find_map(|row| match row {
                Row::Message(row) => Some(row.stored.message.id.to_string()),
                _ => None,
            })
    });
    assert_eq!(focused(&harness, cx), oldest);
    // The list went there with it.
    assert!(shows(harness.window, "message-focus", cx));

    // Escape: the composer has the keyboard again, and typing types.
    press(harness.window, "end", cx);
    press(harness.window, "escape", cx);
    assert_eq!(focused(&harness, cx), None);
    assert!(!shows(harness.window, "message-focus", cx));
    assert!(
        cx.update(|cx| harness.shell.read(cx).open.is_some()),
        "the chat stays open"
    );
    type_text(harness.window, "hello", cx);
    assert_eq!(composer_text(&harness, cx), "hello");

    // With something typed, Up is the text field's; Alt+Up still goes to
    // the messages, and what was typed waits.
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx), None);
    press(harness.window, "alt-up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("k5"));
    assert_eq!(composer_text(&harness, cx), "hello");

    // A press on a bubble's empty room gives that message the keyboard.
    press(harness.window, "escape", cx);
    let bubble = bounds(harness.window, "bubble", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_click(
        point(bubble.right() - px(30.), bubble.bottom() - px(3.)),
        Modifiers::none(),
    );
    visual.run_until_parked();
    assert_eq!(focused(&harness, cx).as_deref(), Some("k5"));
}

#[gpui_kit::test]
fn single_keys_do_nothing_while_typing(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let mine = send(&harness, cx, "a message of my own");
    focus_composer(&harness, cx);
    // Every single key of the registry, typed into the composer: letters.
    let keys: Vec<&str> = crate::keys::BINDINGS
        .iter()
        .filter(|binding| binding.when == crate::keys::When::Message)
        .flat_map(|binding| binding.chords.iter())
        .filter(|chord| !chord.secondary && !chord.shift && chord.key.chars().count() == 1)
        .map(|chord| chord.key)
        .collect();
    assert!(
        keys.contains(&"e") && keys.contains(&"r") && keys.len() > 8,
        "{keys:?}"
    );
    for key in &keys {
        type_text(harness.window, key, cx);
    }
    assert_eq!(composer_text(&harness, cx), keys.concat());
    // Nothing happened to any message, and nothing opened.
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(focused(&harness, cx), None);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(
            shell.acting.compose,
            crate::ui::message_actions::Compose::New
        );
        assert!(shell.acting.selecting.is_none());
    });
    let stored = row(&harness, cx, &mine).unwrap();
    assert!(!stored.message.edited && !stored.message.deleted && !stored.message.extras.starred);
    assert!(harness.mock.actions().is_empty());
    // Backspace is the text field's too.
    press(harness.window, "backspace", cx);
    assert_eq!(composer_text(&harness, cx).len(), keys.concat().len() - 1);
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

#[gpui_kit::test]
fn the_focused_message_is_copied_whole_or_as_its_link(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(
        &harness,
        cx,
        "c1",
        false,
        "read *this* at https://wuapi.dev/docs today",
        1,
    );
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);

    press(harness.window, "c", cx);
    assert_eq!(clipboard(cx), "read *this* at https://wuapi.dev/docs today");
    press(harness.window, "shift-c", cx);
    assert_eq!(clipboard(cx), "https://wuapi.dev/docs");
    // Ctrl+A selects the message's text; Ctrl+C copies it.
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(String::new()));
    press(harness.window, "ctrl-a", cx);
    assert!(cx.update(|cx| harness.shell.read(cx).keys.whole));
    press(harness.window, "ctrl-c", cx);
    assert_eq!(clipboard(cx), "read *this* at https://wuapi.dev/docs today");
    // Moving on lets the selection go.
    put(&harness, cx, "c2", false, "next", 2);
    press(harness.window, "down", cx);
    assert!(!cx.update(|cx| harness.shell.read(cx).keys.whole));
}

// ----- the message menu -----------------------------------------------------------

#[gpui_kit::test]
fn the_message_menu_lists_what_can_be_done_with_its_keys(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    // (Sent a few minutes ago, so that what is sent now comes after it.)
    put(&harness, cx, "theirs", false, "from the other side", -300);
    let mine = send(&harness, cx, "from me");

    // A right-click on a bubble: its menu, inside the window, with the
    // message in focus.
    right_click(harness.window, "bubble", cx);
    assert_eq!(overlay(&harness, cx), Overlay::MessageMenu);
    assert_eq!(focused(&harness, cx).as_deref(), Some(mine.as_str()));
    let menu = bounds(harness.window, "menu", cx);
    let window = cx.update(|cx| harness.shell.read(cx).viewport);
    assert!(menu.left() >= px(0.) && menu.right() <= window.width);
    assert!(
        menu.top() >= px(0.) && menu.bottom() <= window.height,
        "{menu:?}"
    );
    // Every entry that can be run shows its keys, as the registry has them.
    let items = cx.update(|cx| harness.shell.read(cx).menu_items_now());
    let ids: Vec<&str> = items.iter().map(|item| item.id).collect();
    assert_eq!(
        ids,
        [
            "message-reply",
            "message-react",
            "message-copy",
            "message-forward",
            "message-star",
            "message-edit",
            "message-info",
            "message-select",
            "message-delete"
        ]
    );
    for item in &items {
        assert!(
            item.action.is_some(),
            "{} can be run on one's own text",
            item.id
        );
        let command = item.keys.expect("a command");
        let keys = crate::keys::keys_label(command).expect("keys");
        let leaked: &'static str = Box::leak(format!("{}-keys", item.id).into_boxed_str());
        let label = bounds(harness.window, leaked, cx);
        let entry = bounds_of(harness.window, item.id, cx);
        assert!(within(label, entry), "{keys} beside {}", item.id);
        assert!(label.right() > entry.left() + entry.size.width / 2.);
    }
    // An entry's own key runs it from the menu.
    press(harness.window, "e", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(shows(harness.window, "compose-edit", cx));
    press(harness.window, "escape", cx);

    // Somebody else's message: Edit is there, off, and says why.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("theirs"));
    press(harness.window, "m", cx);
    assert_eq!(overlay(&harness, cx), Overlay::MessageMenu);
    let items = cx.update(|cx| harness.shell.read(cx).menu_items_now());
    let edit = items.iter().find(|item| item.id == "message-edit").unwrap();
    assert_eq!(edit.action, None);
    assert_eq!(edit.hint, Some("Only your own messages"));
    press(harness.window, "e", cx);
    assert_eq!(
        overlay(&harness, cx),
        Overlay::MessageMenu,
        "a disabled entry does nothing"
    );
    // Escape closes the menu, and the message keeps the keyboard.
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(focused(&harness, cx).as_deref(), Some("theirs"));
    press(harness.window, "e", cx);
    assert!(
        !shows(harness.window, "compose-edit", cx),
        "nor does its key"
    );
}

// ----- edit and reply ----------------------------------------------------------------

#[gpui_kit::test]
fn up_then_e_edits_a_message_and_a_refusal_puts_it_back(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let id = send(&harness, cx, "see you at 8");
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "e", cx);

    // The composer is editing: it says so, shows the original and holds
    // its text.
    assert!(shows(harness.window, "compose-edit", cx));
    let bar = bounds(harness.window, "compose-edit", cx);
    let field = bounds(harness.window, "composer-field-focused", cx);
    assert!(bar.bottom() <= field.top() && within(bar, bounds(harness.window, "composer", cx)));
    assert_eq!(composer_text(&harness, cx), "see you at 8");
    assert_eq!(focused(&harness, cx), None, "typing is typing again");

    // Enter saves: shown at once as edited, then told to the provider.
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "see you at 9", cx);
    press(harness.window, "enter", cx);
    assert!(!shows(harness.window, "compose-edit", cx));
    assert_eq!(composer_text(&harness, cx), "");
    let stored = row(&harness, cx, &id).unwrap().message;
    assert_eq!(stored.content, MessageContent::text("see you at 9"));
    assert!(stored.edited);
    assert!(shows(harness.window, "edited-label", cx));
    assert!(
        harness.mock.actions().is_empty(),
        "nothing waited for the network"
    );
    harness.settle(cx);
    assert_eq!(harness.mock.actions(), [format!("edit {id}")]);
    assert_eq!(
        harness
            .mock
            .message(&open_ids(&harness, cx).0, &MessageId::new(id.clone()))
            .unwrap()
            .content,
        MessageContent::text("see you at 9")
    );
    // No second message was sent.
    assert_eq!(harness.mock.delivered_count(), 1);

    // Escape gives an edit up: the text is not changed and the composer
    // is empty again.
    press(harness.window, "up", cx);
    press(harness.window, "e", cx);
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "never mind", cx);
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "compose-edit", cx));
    assert_eq!(composer_text(&harness, cx), "");
    assert!(cx.update(|cx| harness.shell.read(cx).open.is_some()));
    assert_eq!(
        row(&harness, cx, &id).unwrap().message.content,
        MessageContent::text("see you at 9")
    );

    // WhatsApp refuses (the window to edit has closed): the old text is
    // back, and the window says why.
    harness.mock.fail_next_actions([rejected(
        "edit_window_closed",
        "It is too late to edit this message.",
    )]);
    press(harness.window, "up", cx);
    press(harness.window, "e", cx);
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "see you at 10", cx);
    press(harness.window, "enter", cx);
    assert_eq!(
        row(&harness, cx, &id).unwrap().message.content,
        MessageContent::text("see you at 10"),
        "shown at once"
    );
    harness.settle(cx);
    let stored = row(&harness, cx, &id).unwrap().message;
    assert_eq!(stored.content, MessageContent::text("see you at 9"));
    let problem = cx
        .update(|cx| harness.shell.read(cx).problem.clone())
        .expect("the refusal is said");
    assert!(
        problem.contains("not edited") && problem.contains("too late"),
        "{problem}"
    );
}

#[gpui_kit::test]
fn a_message_that_has_not_left_yet_is_edited_in_the_outbox(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let (account, chat) = open_ids(&harness, cx);
    harness
        .engine
        .send_text(&account, &chat, "frist".to_owned(), None)
        .unwrap();
    cx.run_until_parked();
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "e", cx);
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "first", cx);
    press(harness.window, "enter", cx);

    // What will be sent is the new text; nobody saw the old one, so it
    // is not marked as edited, and the provider was not asked to edit.
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].message.content,
        OutgoingContent::Text {
            body: "first".into()
        }
    );
    cx.update(|cx| {
        assert_eq!(
            newest_message(harness.shell.read(cx)),
            Some(("first".to_owned(), DeliveryStatus::Pending, true))
        );
    });
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    cx.run_until_parked();
    assert!(harness.mock.actions().is_empty());
    assert_eq!(harness.mock.delivered_count(), 1);
    let sent = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .rev()
            .find_map(|row| match row {
                Row::Message(row) => Some(row.stored.message.clone()),
                _ => None,
            })
    });
    let sent = sent.unwrap();
    assert_eq!(sent.content, MessageContent::text("first"));
    assert!(!sent.edited);
}

#[gpui_kit::test]
fn a_reply_names_what_it_answers_and_its_quote_leads_back(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(
        &harness,
        cx,
        "q1",
        false,
        "shall we meet at the usual place?",
        -300,
    );
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("q1"));

    // R (or Enter): the composer says what is being answered.
    press(harness.window, "r", cx);
    assert!(shows(harness.window, "compose-reply", cx));
    assert_eq!(focused(&harness, cx), None);
    // Escape lets the reply go; the chat stays.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "compose-reply", cx));
    assert!(cx.update(|cx| harness.shell.read(cx).open.is_some()));

    press(harness.window, "up", cx);
    press(harness.window, "enter", cx);
    assert!(shows(harness.window, "compose-reply", cx));
    type_text(harness.window, "yes, at noon", cx);
    press(harness.window, "enter", cx);
    assert!(!shows(harness.window, "compose-reply", cx));

    // It went out as an answer to that message.
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message.reply_to, Some(MessageId::new("q1")));
    let quote = bounds(harness.window, "quote", cx);
    assert!(within(quote, bounds(harness.window, "bubble", cx)));

    // The next message is not a reply.
    type_text(harness.window, "see you", cx);
    press(harness.window, "enter", cx);
    assert_eq!(
        harness.engine.store().outbox_pending().unwrap()[1]
            .message
            .reply_to,
        None
    );

    // A click on the quote goes to the message it quotes and lights it,
    // for a moment.
    click(harness.window, "quote", cx);
    assert!(shows(harness.window, "message-flash", cx));
    let lit = bounds(harness.window, "message-flash", cx);
    assert_eq!(lit, bounds(harness.window, "row-q1", cx));
    cx.executor()
        .advance_clock(crate::motion::FLASH + Duration::from_millis(50));
    cx.run_until_parked();
    assert!(!shows(harness.window, "message-flash", cx));
}

// ----- react, star, delete ---------------------------------------------------------

#[gpui_kit::test]
fn a_reaction_is_picked_shows_as_ones_own_and_is_taken_back(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(&harness, cx, "r1", false, "we won!", 1);
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);

    // "+" opens the six; a digit picks.
    type_text(harness.window, "+", cx);
    assert_eq!(overlay(&harness, cx), Overlay::React);
    let sheet = bounds(harness.window, "react-sheet", cx);
    for index in 0..6 {
        let emoji = bounds_of(harness.window, &format!("reaction-{index}"), cx);
        assert!(within(emoji, sheet), "{index}");
    }
    assert!(within(bounds(harness.window, "reaction-input", cx), sheet));
    press(harness.window, "2", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    let reactions = row(&harness, cx, "r1").unwrap().reactions;
    assert_eq!(reactions.len(), 1);
    assert_eq!(
        (reactions[0].emoji.as_str(), reactions[0].from_me),
        ("❤️", true)
    );
    assert!(shows(harness.window, "reaction-mine", cx));
    // It went through the outbox, like a message.
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(
        pending[0].message.content,
        OutgoingContent::Reaction {
            target: MessageId::new("r1"),
            emoji: "❤️".into()
        }
    );
    assert_eq!(
        focused(&harness, cx).as_deref(),
        Some("r1"),
        "the message keeps the keyboard"
    );

    // Another one replaces it: the arrows and Enter pick too.
    type_text(harness.window, ":", cx);
    assert!(shows(harness.window, "reaction-remove", cx));
    press(harness.window, "right", cx);
    press(harness.window, "right", cx);
    press(harness.window, "enter", cx);
    let reactions = row(&harness, cx, "r1").unwrap().reactions;
    assert_eq!(reactions.len(), 1);
    assert_eq!(reactions[0].emoji, "😂");

    // A click on one's own chip says who reacted; one's own line there
    // takes it back.
    click(harness.window, "reaction-mine", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Reactors);
    click(harness.window, "reactor-0", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(row(&harness, cx, "r1").unwrap().reactions.is_empty());
    assert!(!shows(harness.window, "reaction-mine", cx));
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(
        pending.last().unwrap().message.content,
        OutgoingContent::Reaction {
            target: MessageId::new("r1"),
            emoji: String::new()
        }
    );
}

#[gpui_kit::test]
fn s_stars_and_unstars_the_message(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let id = send(&harness, cx, "keep this");
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "s", cx);
    assert!(row(&harness, cx, &id).unwrap().message.extras.starred);
    assert!(shows(harness.window, "starred-mark", cx));
    harness.settle(cx);
    assert_eq!(harness.mock.actions(), [format!("star {id}")]);
    press(harness.window, "s", cx);
    assert!(!row(&harness, cx, &id).unwrap().message.extras.starred);
    harness.settle(cx);
    assert_eq!(harness.mock.actions()[1], format!("unstar {id}"));

    // A refusal takes the star back off, and says so.
    harness
        .mock
        .fail_next_actions([rejected("forbidden", "Not allowed.")]);
    press(harness.window, "s", cx);
    assert!(row(&harness, cx, &id).unwrap().message.extras.starred);
    harness.settle(cx);
    assert!(!row(&harness, cx, &id).unwrap().message.extras.starred);
    let problem = cx
        .update(|cx| harness.shell.read(cx).problem.clone())
        .unwrap();
    assert!(problem.contains("not starred"), "{problem}");
}

#[gpui_kit::test]
fn a_message_is_deleted_for_everyone_or_for_oneself_after_being_asked(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(&harness, cx, "theirs", false, "from them", -300);
    let first = send(&harness, cx, "one of mine");
    let second = send(&harness, cx, "another of mine");
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some(second.as_str()));

    // Backspace asks. The keyboard starts on Cancel: Enter deletes nothing.
    press(harness.window, "backspace", cx);
    assert_eq!(overlay(&harness, cx), Overlay::DeleteMessage);
    for choice in ["delete-for-everyone", "delete-for-me", "delete-cancel"] {
        let leaked: &'static str = choice;
        assert!(shows(harness.window, leaked, cx), "{choice}");
    }
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(!row(&harness, cx, &second).unwrap().message.deleted);
    assert!(harness.mock.actions().is_empty());

    // For everyone: the placeholder takes its place, at once.
    press(harness.window, "delete", cx);
    click(harness.window, "delete-for-everyone", cx);
    assert!(row(&harness, cx, &second).unwrap().message.deleted);
    harness.settle(cx);
    assert_eq!(harness.mock.actions(), [format!("delete-all {second}")]);

    // For oneself: it goes from the conversation. The keyboard picks the
    // way with the arrows.
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some(first.as_str()));
    press(harness.window, "backspace", cx);
    press(harness.window, "up", cx);
    press(harness.window, "enter", cx);
    assert!(
        row(&harness, cx, &first).is_none(),
        "gone from the conversation"
    );
    harness.settle(cx);
    assert_eq!(harness.mock.actions()[1], format!("delete-me {first}"));

    // Somebody else's message can only be deleted for oneself.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "up", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("theirs"));
    press(harness.window, "backspace", cx);
    assert!(!shows(harness.window, "delete-for-everyone", cx));
    assert!(shows(harness.window, "delete-for-me", cx));
    press(harness.window, "escape", cx);
    assert!(row(&harness, cx, "theirs").is_some());

    // A refusal brings the message back.
    harness
        .mock
        .fail_next_actions([rejected("forbidden", "Too late to delete.")]);
    press(harness.window, "backspace", cx);
    click(harness.window, "delete-for-me", cx);
    assert!(row(&harness, cx, "theirs").is_none());
    harness.settle(cx);
    assert!(
        row(&harness, cx, "theirs").is_some(),
        "back after the refusal"
    );
    let problem = cx
        .update(|cx| harness.shell.read(cx).problem.clone())
        .unwrap();
    assert!(
        problem.contains("not deleted") && problem.contains("Too late"),
        "{problem}"
    );
}

// ----- forward and selecting several ------------------------------------------------

#[gpui_kit::test]
fn a_message_is_forwarded_to_two_chats(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(
        &harness,
        cx,
        "f1",
        false,
        "the address is *Rua Augusta 12*",
        1,
    );
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "f", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Forward);

    // Nothing picked: nothing to send.
    click(harness.window, "forward-send", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Forward);
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());

    // Two chats: one with a click, one with the keyboard.
    let rows = cx.update(|cx| harness.shell.read(cx).forward_rows(cx));
    assert!(rows.len() > 5);
    click(harness.window, "forward-chat-0", cx);
    press(harness.window, "down", cx);
    press(harness.window, "down", cx);
    press(harness.window, "enter", cx);
    let picked = cx.update(|cx| harness.shell.read(cx).acting.forward_to.clone());
    assert_eq!(picked, [rows[0].0.clone(), rows[2].0.clone()]);
    // A third, and letting it go again.
    click(harness.window, "forward-chat-1", cx);
    click(harness.window, "forward-chat-1", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).acting.forward_to.len()),
        2
    );

    click(harness.window, "forward-send", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 2, "one message per chat");
    for (entry, chat) in pending.iter().zip(&picked) {
        assert_eq!(&entry.message.chat_id, chat);
        assert!(entry.message.forwarded, "marked as forwarded");
        assert_eq!(
            entry.message.content,
            OutgoingContent::Text {
                body: "the address is *Rua Augusta 12*".into()
            }
        );
    }
    // And the provider gets them so.
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    let account = open_ids(&harness, cx).0;
    let delivered = harness
        .runtime
        .block_on(async {
            use client_provider::Provider as _;
            harness
                .mock
                .fetch_messages(&account, &picked[0], None, 1)
                .await
        })
        .unwrap();
    assert!(delivered.items[0].extras.forwarded);

    // No more than five chats at once.
    press(harness.window, "f", cx);
    for index in 0..7 {
        click_dynamic(&harness, cx, &format!("forward-chat-{index}"));
    }
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).acting.forward_to.len()),
        crate::ui::message_actions::FORWARD_LIMIT
    );
}

fn click_dynamic(harness: &Harness, cx: &mut TestAppContext, selector: &str) {
    let leaked: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    click(harness.window, leaked, cx);
}

#[gpui_kit::test]
fn several_messages_are_selected_with_shift_and_acted_on_together(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    for n in 1..=4 {
        put(
            &harness,
            cx,
            &format!("s{n}"),
            n % 2 == 0,
            &format!("line {n}"),
            n,
        );
    }
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    assert!(shows(harness.window, "composer", cx));

    // Shift+Up twice: three messages, and the bar takes the composer's
    // place.
    press(harness.window, "shift-up", cx);
    press(harness.window, "shift-up", cx);
    let picked = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .subjects()
                .iter()
                .map(|message| message.id.to_string())
                .collect::<Vec<_>>()
        })
    };
    assert_eq!(picked(cx), ["s2", "s3", "s4"]);
    assert!(shows(harness.window, "selection-bar", cx));
    assert!(!shows(harness.window, "composer", cx));
    assert!(shows(harness.window, "message-check", cx));
    assert!(shows(harness.window, "message-picked", cx));
    // The marks took no room: the rows are where they were.
    let thread = bounds(harness.window, "thread", cx);
    let check = bounds(harness.window, "message-check", cx);
    assert!(check.right() <= thread.right() && check.left() > thread.right() - px(40.));

    // Shift+Down lets nothing go but moves on; X lets the one in focus go.
    press(harness.window, "x", cx);
    assert_eq!(picked(cx), ["s3", "s4"]);
    press(harness.window, "x", cx);
    assert_eq!(picked(cx), ["s2", "s3", "s4"]);

    // Copy: every picked message, one per line, with who and when.
    press(harness.window, "c", cx);
    let copied = clipboard(cx);
    let lines: Vec<&str> = copied.lines().collect();
    assert_eq!(lines.len(), 3, "{copied}");
    assert!(
        lines[0].ends_with("You: line 2") && lines[1].ends_with(": line 3"),
        "{copied}"
    );
    assert!(lines[0].starts_with('['), "{copied}");

    // Star: all of them.
    press(harness.window, "s", cx);
    for id in ["s2", "s3", "s4"] {
        assert!(
            row(&harness, cx, id).unwrap().message.extras.starred,
            "{id}"
        );
    }
    assert!(!row(&harness, cx, "s1").unwrap().message.extras.starred);

    // Forward: the sheet is about the three.
    click(harness.window, "selection-forward", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Forward);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).acting.subjects.len()),
        3
    );
    press(harness.window, "escape", cx);

    // Delete: asked once for all of them. Theirs are among them, so not
    // for everyone.
    click(harness.window, "selection-delete", cx);
    assert_eq!(overlay(&harness, cx), Overlay::DeleteMessage);
    assert!(!shows(harness.window, "delete-for-everyone", cx));
    click(harness.window, "delete-for-me", cx);
    for id in ["s2", "s3", "s4"] {
        assert!(row(&harness, cx, id).is_none(), "{id}");
    }
    assert!(row(&harness, cx, "s1").is_some());
    // The selection is over and the composer is back.
    assert!(!shows(harness.window, "selection-bar", cx));
    assert!(shows(harness.window, "composer", cx));

    // Escape leaves a selection before it leaves the messages.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "x", cx);
    assert!(shows(harness.window, "selection-bar", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "selection-bar", cx));
    assert_eq!(focused(&harness, cx).as_deref(), Some("s1"));
    press(harness.window, "escape", cx);
    assert_eq!(focused(&harness, cx), None);
}

// ----- the palette, the search in a conversation, the shortcuts ---------------------

use crate::ui::palette::Item;

/// The rows of the palette that are not headings, as (kind, title).
pub(super) fn palette_rows(
    harness: &Harness,
    cx: &mut TestAppContext,
) -> Vec<(&'static str, String)> {
    use crate::ui::palette::MODES;
    use crate::ui::palette_steps::StepKind;
    cx.update(|cx| {
        let palette = &harness.shell.read(cx).palette;
        palette
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Section(_) | Item::Line(..) => None,
                Item::Chat(chat) => Some(("chat", chat.title.clone())),
                Item::Person(contact) => Some(("person", contact.display_name())),
                Item::Message(hit) => Some(("message", client_core::message_preview(&hit.message))),
                Item::Command(command) => {
                    Some(("command", crate::keys::label(*command).to_owned()))
                }
                Item::Unavailable(command, _) => {
                    Some(("unavailable", crate::keys::label(*command).to_owned()))
                }
                Item::Number(number) => Some(("number", number.clone())),
                Item::Choice(place) => match palette.steps.last().map(|step| &step.kind) {
                    Some(StepKind::Choices(choices)) => {
                        Some(("choice", choices[*place].label.to_string()))
                    }
                    _ => Some(("mode", MODES[*place].0.to_owned())),
                },
            })
            .collect()
    })
}

/// Lets the palette's wait after a keystroke pass, and its read finish.
pub(super) fn settle_search(harness: &Harness, cx: &mut TestAppContext) {
    cx.executor()
        .advance_clock(crate::motion::SEARCH_DEBOUNCE + Duration::from_millis(10));
    cx.run_until_parked();
    harness.settle(cx);
}

#[gpui_kit::test]
fn the_palette_finds_a_message_opens_its_chat_and_goes_to_it(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    assert!(cx.update(|cx| harness.shell.read(cx).open.is_none()));

    // Ctrl+K, from anywhere: with nothing typed, chats and commands.
    press(harness.window, "ctrl-k", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
    let palette = bounds(harness.window, "palette", cx);
    let window = cx.update(|cx| harness.shell.read(cx).viewport);
    assert!(
        (palette.center().x - window.width / 2.).abs() <= px(1.),
        "centred"
    );
    assert!(palette.top() > px(40.) && palette.bottom() < window.height);
    let rows = palette_rows(&harness, cx);
    assert!(rows.iter().any(|(kind, _)| *kind == "chat"));
    assert!(rows.iter().any(|(kind, _)| *kind == "command"));

    // A word that is only in messages: the store is asked a moment after
    // the typing stops, not per letter.
    type_text(harness.window, "dentist", cx);
    assert!(
        !palette_rows(&harness, cx)
            .iter()
            .any(|(kind, _)| *kind == "message"),
        "not before the wait is over"
    );
    settle_search(&harness, cx);
    let rows = palette_rows(&harness, cx);
    let hits: Vec<_> = rows.iter().filter(|(kind, _)| *kind == "message").collect();
    assert!(!hits.is_empty(), "{rows:?}");
    assert!(hits[0].1.to_lowercase().contains("dentist"), "{hits:?}");
    let wanted = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell.palette.items.iter().find_map(|item| match item {
            Item::Message(hit) => Some((hit.message.chat_id.clone(), hit.message.id.clone())),
            _ => None,
        })
    });
    let (chat, message) = wanted.unwrap();

    // Down to the message, Enter: its chat opens, and the message is on
    // screen, lit for a moment.
    let at = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .palette
            .items
            .iter()
            .position(|item| matches!(item, Item::Message(_)))
            .unwrap()
    });
    for _ in 0..40 {
        if cx.update(|cx| harness.shell.read(cx).palette.cursor) == at {
            break;
        }
        press(harness.window, "down", cx);
    }
    assert_eq!(cx.update(|cx| harness.shell.read(cx).palette.cursor), at);
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(
            shell.open.as_ref().map(|open| open.chat.id.clone()),
            Some(chat.clone())
        );
    });
    assert!(shows(harness.window, "message-flash", cx));
    let lit = bounds(harness.window, "message-flash", cx);
    assert_eq!(
        lit,
        bounds_of(harness.window, &format!("row-{message}"), cx)
    );
    assert!(within(lit, bounds(harness.window, "thread", cx)));

    // The chat that was opened is the first thing it offers next time.
    press(harness.window, "ctrl-k", cx);
    let first = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .palette
            .items
            .iter()
            .find_map(|item| match item {
                Item::Chat(chat) => Some(chat.id.clone()),
                _ => None,
            })
    });
    assert_eq!(first, Some(chat));
    // Ctrl+K again closes it.
    press(harness.window, "ctrl-k", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

#[gpui_kit::test]
fn the_palette_narrows_by_prefix_and_runs_commands_with_their_keys(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    press(harness.window, "ctrl-k", cx);

    // ">" is commands only, ranked as they are typed, each with its keys.
    type_text(harness.window, ">settings key", cx);
    let rows = palette_rows(&harness, cx);
    assert!(rows.iter().all(|(kind, _)| *kind == "command"), "{rows:?}");
    assert_eq!(rows[0].1, "Settings: Keyboard");
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Settings);
    assert!(shows(harness.window, "keyboard-show-all", cx));
    // Settings > Keyboard leads to the list of every shortcut.
    click(harness.window, "keyboard-show-all", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Shortcuts);
    press(harness.window, "escape", cx);

    // A command shows its keys, as the registry has them.
    press(harness.window, "ctrl-k", cx);
    type_text(harness.window, ">new chat", cx);
    let at = cx.update(|cx| harness.shell.read(cx).palette.cursor);
    let keys = bounds_of(harness.window, &format!("palette-keys-{at}"), cx);
    let item = bounds_of(harness.window, &format!("palette-item-{at}"), cx);
    assert!(within(keys, item) && keys.right() > item.center().x);
    // What was run is offered first the next time, with nothing typed.
    // "New chat" asks who with, in place; Escape goes back, then closes.
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
    assert!(shows(harness.window, "palette-step", cx));
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
    assert!(!shows(harness.window, "palette-step", cx));
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    press(harness.window, "ctrl-k", cx);
    let commands: Vec<String> = palette_rows(&harness, cx)
        .into_iter()
        .filter(|(kind, _)| *kind == "command")
        .map(|(_, title)| title)
        .collect();
    assert_eq!(commands[0], "New chat");
    assert_eq!(commands[1], "Settings: Keyboard");

    // "#" is groups, "@" people of the address book.
    type_text(harness.window, "#lisbon", cx);
    settle_search(&harness, cx);
    let rows = palette_rows(&harness, cx);
    assert_eq!(rows, [("chat", "Lisbon trip ✈️".to_owned())]);
    let account = open_ids(&harness, cx).0;
    harness
        .runtime
        .block_on(harness.engine.sync_contacts(&account))
        .unwrap();
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "@a", cx);
    settle_search(&harness, cx);
    let rows = palette_rows(&harness, cx);
    assert!(
        !rows.is_empty() && rows.iter().all(|(kind, _)| *kind == "person"),
        "{rows:?}"
    );

    // "in:" looks through the open conversation only.
    put(
        &harness,
        cx,
        "zz1",
        false,
        "the zebra crossing is closed",
        1,
    );
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "in: zebra", cx);
    settle_search(&harness, cx);
    let rows = palette_rows(&harness, cx);
    assert_eq!(
        rows,
        [("message", "the zebra crossing is closed".to_owned())]
    );

    // Ctrl+Shift+F is the palette over messages.
    press(harness.window, "escape", cx);
    press(harness.window, "ctrl-shift-f", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
    type_text(harness.window, "zebra", cx);
    settle_search(&harness, cx);
    let rows = palette_rows(&harness, cx);
    assert!(
        !rows.is_empty() && rows.iter().all(|(kind, _)| *kind == "message"),
        "{rows:?}"
    );
    // Something nothing answers says so.
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "qqqqzzzz", cx);
    settle_search(&harness, cx);
    assert!(shows(harness.window, "palette-empty", cx));
}

#[gpui_kit::test]
fn the_search_in_a_conversation_steps_through_its_matches(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    for (n, body) in [
        (1, "the walrus is here"),
        (2, "nothing to see"),
        (3, "a second walrus"),
        (4, "and a third Walrus, the last"),
    ] {
        put(&harness, cx, &format!("w{n}"), n % 2 == 0, body, n);
    }
    press(harness.window, "ctrl-f", cx);
    assert!(shows(harness.window, "thread-search", cx));
    type_text(harness.window, "walrus", cx);

    let state = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            let search = shell.thread_search.as_ref().unwrap();
            (
                search.hits.len(),
                search.current,
                search.hits[search.current].message.id.to_string(),
                shell.acting.flash.clone(),
            )
        })
    };
    // Three matches; the conversation is on the newest, which is lit.
    assert_eq!(state(cx), (3, 0, "w4".into(), Some("m:w4".into())));
    assert_eq!(
        bounds(harness.window, "message-flash", cx),
        bounds(harness.window, "row-w4", cx)
    );
    // Enter: the next one back in time. Shift+Enter: forward again.
    press(harness.window, "enter", cx);
    assert_eq!(state(cx).2, "w3");
    press(harness.window, "enter", cx);
    assert_eq!(state(cx).2, "w1");
    press(harness.window, "enter", cx);
    assert_eq!(state(cx).2, "w4", "round again");
    press(harness.window, "shift-enter", cx);
    assert_eq!(state(cx).2, "w1");
    // The buttons and Ctrl+G do the same.
    click(harness.window, "thread-search-previous", cx);
    assert_eq!(state(cx).2, "w3");
    press(harness.window, "ctrl-g", cx);
    assert_eq!(state(cx).2, "w1");
    press(harness.window, "ctrl-shift-g", cx);
    assert_eq!(state(cx).2, "w3");
    assert_eq!(
        bounds(harness.window, "message-flash", cx),
        bounds(harness.window, "row-w3", cx)
    );
    assert!(shows(harness.window, "thread-search-count", cx));

    // Escape closes the search, and nothing stays lit.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "thread-search", cx));
    assert!(!shows(harness.window, "message-flash", cx));
    assert!(cx.update(|cx| harness.shell.read(cx).open.is_some()));
}

#[gpui_kit::test]
fn every_shortcut_is_listed_on_the_sheet_and_chats_are_walked_by_key(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    // "?" with nothing being typed, or Ctrl+/ from anywhere.
    type_text(harness.window, "?", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Shortcuts);
    let sheet = bounds(harness.window, "shortcuts", cx);
    let rows: usize = crate::ui::palette::shortcut_rows()
        .iter()
        .map(|(_, rows)| rows.len())
        .sum();
    assert_eq!(
        rows,
        crate::keys::BINDINGS
            .iter()
            .filter(|binding| !binding.chords.is_empty())
            .count()
    );
    for index in 0..rows {
        let row = bounds_of(harness.window, &format!("shortcut-{index}"), cx);
        assert!(
            row.left() >= sheet.left() && row.right() <= sheet.right(),
            "{index}"
        );
    }
    press(harness.window, "escape", cx);
    press(harness.window, "ctrl-/", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Shortcuts);
    press(harness.window, "ctrl-/", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);

    // Ctrl+Tab walks the chats down the list, Ctrl+Shift+Tab back up.
    let chats: Vec<String> = cx.update(|cx| {
        chat_rows(harness.shell.read(cx))
            .into_iter()
            .map(|(id, _, _)| id)
            .collect()
    });
    let open_now = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .open
                .as_ref()
                .map(|open| open.chat.id.to_string())
        })
    };
    press(harness.window, "ctrl-tab", cx);
    assert_eq!(open_now(cx).as_deref(), Some(chats[0].as_str()));
    press(harness.window, "ctrl-tab", cx);
    assert_eq!(open_now(cx).as_deref(), Some(chats[1].as_str()));
    press(harness.window, "ctrl-shift-tab", cx);
    assert_eq!(open_now(cx).as_deref(), Some(chats[0].as_str()));
    // In the composer "?" is a question mark.
    focus_composer(&harness, cx);
    type_text(harness.window, "?", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert_eq!(composer_text(&harness, cx), "?");
    // Ctrl+2 is the second number of the rail; Ctrl+Shift+U the next
    // chat with something unread.
    let accounts = stored_accounts(&harness);
    press(harness.window, "ctrl-2", cx);
    cx.update(|cx| {
        assert_eq!(
            harness.shell.read(cx).account.as_ref(),
            Some(&accounts[1].id)
        );
    });
    press(harness.window, "ctrl-1", cx);
    let unread: Vec<String> = cx.update(|cx| {
        chat_rows(harness.shell.read(cx))
            .into_iter()
            .filter(|(_, _, unread)| *unread > 0)
            .map(|(id, _, _)| id)
            .collect()
    });
    press(harness.window, "ctrl-shift-u", cx);
    assert_eq!(open_now(cx).as_deref(), Some(unread[0].as_str()));
}

#[gpui_kit::test]
fn the_palette_is_centred_in_the_window_over_a_veil_and_only_grows_downward(
    cx: &mut TestAppContext,
) {
    let harness = open_chat_one(cx);
    focus_composer(&harness, cx);
    let mut measured = Vec::new();
    for (width, height, list) in [
        (1240., 800., None),
        (1240., 800., Some(480u16)),
        (900., 640., Some(260)),
        (1600., 1000., None),
        (700., 520., None),
    ] {
        cx.update(|cx| settings::update(cx, |settings| settings.list_width = list));
        let visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_resize(size(px(width), px(height)));
        visual.run_until_parked();
        press(harness.window, "ctrl-k", cx);
        let what = format!("{width}x{height}, list {list:?}");

        // The veil covers the whole window; the palette is centred in the
        // window (not in the conversation), its top a fixed share down.
        let veil = bounds(harness.window, "overlay", cx);
        assert_eq!(veil.origin, point(px(0.), px(0.)), "{what}");
        assert_eq!(veil.size, size(px(width), px(height)), "{what}");
        let palette = bounds(harness.window, "palette", cx);
        assert!(
            (palette.center().x - px(width / 2.)).abs() <= px(1.),
            "{what}: {palette:?}"
        );
        assert_eq!(
            palette.top(),
            crate::ui::palette::palette_top(px(height)),
            "{what}"
        );
        assert!(
            palette.top() >= px(height * 0.16) && palette.top() <= px(height * 0.2),
            "{what}"
        );
        // A fixed width, given up only to a window too narrow for it, and
        // never out of the window.
        let wanted = theme::metrics::PALETTE_WIDTH().min(px(width) - theme::px(32.));
        assert!(
            (palette.size.width - wanted).abs() <= px(1.),
            "{what}: {palette:?}"
        );
        assert!(
            palette.left() >= px(0.) && palette.right() <= px(width),
            "{what}"
        );
        assert!(palette.bottom() <= px(height), "{what}: {palette:?}");
        // The rule of the accent along its top, the row the keyboard is
        // on with its bar, and the hints at its foot, all inside it.
        for part in ["palette-rule", "palette-cursor", "palette-hints"] {
            assert!(
                within(bounds(harness.window, part, cx), palette),
                "{what}: {part}"
            );
        }
        let cursor = bounds(harness.window, "palette-cursor", cx);
        let at = cx.update(|cx| harness.shell.read(cx).palette.cursor);
        let row = bounds_of(harness.window, &format!("palette-item-{at}"), cx);
        assert_eq!(
            cursor.left(),
            row.left(),
            "{what}: the bar is on the row's left"
        );

        // With nothing found it is shorter, and nowhere else.
        type_text(harness.window, ">qqqqzzzz", cx);
        let empty = bounds(harness.window, "palette", cx);
        assert_eq!(empty.origin, palette.origin, "{what}");
        assert_eq!(empty.size.width, palette.size.width, "{what}");
        assert!(empty.size.height < palette.size.height, "{what}");
        // And with many results it scrolls inside.
        press(harness.window, "ctrl-a", cx);
        type_text(harness.window, ">", cx);
        let full = bounds(harness.window, "palette", cx);
        assert_eq!(full.origin, palette.origin, "{what}");
        assert!(full.bottom() <= px(height), "{what}: {full:?}");
        measured.push(palette.size.width);

        // A click on the veil closes it, and the keyboard is back where
        // it was: in the composer.
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_click(point(px(6.), px(height - 6.)), Modifiers::none());
        visual.run_until_parked();
        assert_eq!(overlay(&harness, cx), Overlay::None, "{what}");
        type_text(harness.window, "x", cx);
        assert!(composer_text(&harness, cx).ends_with('x'), "{what}");
    }
    assert_eq!(
        measured[0], measured[3],
        "the same width in any roomy window"
    );
}

#[gpui_kit::test]
fn sheets_and_menus_are_lifted_over_a_veil_or_stand_on_their_own_outline(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    put(&harness, cx, "v1", false, "a message", 1);
    let window = cx.update(|cx| harness.shell.read(cx).viewport);
    let centred = |cx: &mut TestAppContext, card: &'static str| {
        let veil = bounds(harness.window, "overlay", cx);
        assert_eq!(veil.size, window, "{card}: the veil covers the window");
        let card_bounds = bounds(harness.window, card, cx);
        assert!(
            (card_bounds.center().x - window.width / 2.).abs() <= px(1.),
            "{card}: {card_bounds:?}"
        );
        assert!(
            (card_bounds.center().y - window.height / 2.).abs() <= px(1.),
            "{card}"
        );
    };
    // The sheets: centred in the window.
    press(harness.window, "ctrl-/", cx);
    centred(cx, "shortcuts");
    press(harness.window, "escape", cx);
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "f", cx);
    centred(cx, "forward-sheet");
    press(harness.window, "escape", cx);
    press(harness.window, "i", cx);
    centred(cx, "message-info");
    press(harness.window, "escape", cx);
    press(harness.window, "backspace", cx);
    centred(cx, "delete-message");
    press(harness.window, "escape", cx);
    // A menu has no veil over the window's content: it is where it was
    // asked for, inside the window.
    press(harness.window, "m", cx);
    let menu = bounds(harness.window, "menu", cx);
    assert!(menu.left() >= px(0.) && menu.right() <= window.width);
    assert!(menu.bottom() <= window.height);
}

// ----- the newest message, whole above the composer -----------------------------

/// A file message in the open chat, `at` seconds from now.
fn put_media(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    kind: client_provider::MediaKind,
    url: &str,
    at: i64,
) {
    use client_provider::{Media, MediaRef};
    let (account, chat) = open_ids(harness, cx);
    let mut message = text_message(&account, &chat, id, false, "", at);
    let mut media = Media::new(kind);
    media.source = Some(MediaRef::new(url));
    message.content = MessageContent::Media(media);
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    cx.run_until_parked();
}

/// The newest message has the keyboard: its outline, and whatever hangs
/// under its bubble, are whole inside the list (which paints nothing
/// outside itself), and the row is where the newest row always is.
fn whole_at_the_end(harness: &Harness, cx: &mut TestAppContext, id: &str, when: &str) {
    use theme::metrics;
    assert_eq!(focused(harness, cx).as_deref(), Some(id), "{when}");
    let thread = bounds(harness.window, "thread", cx);
    let list_bottom = thread.bottom() - (metrics::THREAD_INSET() - metrics::FOCUS_ROOM());
    let ring = bounds(harness.window, "message-focus", cx);
    let row = bounds_of(harness.window, &format!("row-{id}"), cx);
    assert!(
        (row.bottom() - (thread.bottom() - metrics::THREAD_INSET())).abs() <= px(1.),
        "{when}: the row ends at {:?}, the pane at {:?}",
        row.bottom(),
        thread.bottom()
    );
    assert!(
        ring.bottom() <= list_bottom + px(0.5) && ring.top() >= thread.top(),
        "{when}: the outline {ring:?} is cut by the list, which ends at {list_bottom:?}"
    );
    assert!(
        ring.bottom() <= row.bottom() + metrics::FOCUS_OUT() + px(0.5),
        "{when}: {ring:?} stands out of {row:?}"
    );
    for chip in ["reaction-mine", "reaction-chip", "sending-line"] {
        if let Some(chip_bounds) =
            VisualTestContext::from_window(harness.window.into(), cx).debug_bounds(chip)
        {
            assert!(
                chip_bounds.bottom() <= row.bottom() + px(0.5),
                "{when}: {chip}"
            );
        }
    }
    // And all of it above whatever is under the thread.
    let under = if shows(harness.window, "selection-bar", cx) {
        bounds(harness.window, "selection-bar", cx)
    } else {
        bounds(harness.window, "composer", cx)
    };
    assert!(
        ring.bottom() + px(3.) <= under.top(),
        "{when}: the outline ends at {:?}, what is under the thread starts at {:?}",
        ring.bottom(),
        under.top()
    );
}

#[gpui_kit::test]
fn the_newest_message_and_its_outline_are_whole_above_the_composer(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open_chat_one(cx);
    let tape = std::rc::Rc::new(std::cell::RefCell::new(crate::audio::tests::Tape::default()));
    let output = Box::new(crate::audio::tests::FakeOutput(tape.clone()));
    harness
        .shell
        .update(cx, |shell, _| shell.set_audio_output(output));
    let mut at = 0;
    for step in theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        cx.run_until_parked();
        let id = |kind: &str| format!("{kind}-{step}");
        let mut next = || {
            at += 10;
            at
        };

        // A text, then with a reaction under it: the row grew while it
        // had the keyboard, and is still whole.
        let text = id("text");
        put(&harness, cx, &text, false, "see you there", next());
        focus_composer(&harness, cx);
        press(harness.window, "up", cx);
        whole_at_the_end(&harness, cx, &text, &format!("{step}%: a text"));
        // The other side reacts: nothing here moved the keyboard, the
        // row just grew.
        let (account, chat) = open_ids(&harness, cx);
        let mut reaction = text_message(&account, &chat, &id("reaction"), false, "", next());
        reaction.content = MessageContent::Reaction {
            target: MessageId::new(text.clone()),
            emoji: "👍".to_owned(),
        };
        let before = bounds_of(harness.window, &format!("row-{text}"), cx);
        harness
            .engine
            .apply_event(ProviderEvent::MessageUpserted(reaction))
            .unwrap();
        cx.run_until_parked();
        assert!(shows(harness.window, "reaction-chip", cx));
        assert!(
            bounds_of(harness.window, &format!("row-{text}"), cx)
                .size
                .height
                > before.size.height
        );
        whole_at_the_end(
            &harness,
            cx,
            &text,
            &format!("{step}%: a text someone reacted to"),
        );
        type_text(harness.window, "+", cx);
        press(harness.window, "2", cx);
        harness.settle(cx);
        assert!(shows(harness.window, "reaction-mine", cx));
        whole_at_the_end(
            &harness,
            cx,
            &text,
            &format!("{step}%: a text with a reaction"),
        );

        // A picture, and a sticker (which has no bubble).
        for (kind, name) in [(MediaKind::Image, "image"), (MediaKind::Sticker, "sticker")] {
            let (message, url) = (id(name), format!("https://media.example/{name}-{step}"));
            harness.mock.set_media(&url, png(300, 200), "image/png");
            put_media(&harness, cx, &message, kind, &url, next());
            cache_thumbnail(&harness, cx, &url, (120, 90));
            press(harness.window, "end", cx);
            whole_at_the_end(&harness, cx, &message, &format!("{step}%: a {name}"));
            harness.settle(cx);
            whole_at_the_end(
                &harness,
                cx,
                &message,
                &format!("{step}%: a {name}, loaded"),
            );
        }

        // A voice note: before it is fetched, while it is on its way, and
        // playing.
        let (voice, url) = (
            id("voice"),
            format!("https://media.example/voice-{step}.ogg"),
        );
        harness.mock.set_media(
            &url,
            crate::audio::tests::VOICE_NOTE.to_vec(),
            "audio/ogg; codecs=opus",
        );
        put_media(&harness, cx, &voice, MediaKind::Voice, &url, next());
        press(harness.window, "end", cx);
        whole_at_the_end(&harness, cx, &voice, &format!("{step}%: a voice note"));
        harness.mock.set_media_latency(Duration::from_millis(60));
        press(harness.window, "o", cx);
        cx.run_until_parked();
        whole_at_the_end(
            &harness,
            cx,
            &voice,
            &format!("{step}%: a voice note on its way"),
        );
        until(&harness, cx, |shell| shell.audio.player.is_playing());
        whole_at_the_end(
            &harness,
            cx,
            &voice,
            &format!("{step}%: a voice note playing"),
        );
        harness.mock.set_media_latency(Duration::ZERO);
        // "O" again pauses it.
        press(harness.window, "o", cx);
        cx.run_until_parked();

        // Picked, with the selection's bar in the composer's place.
        press(harness.window, "x", cx);
        assert!(shows(harness.window, "selection-bar", cx));
        whole_at_the_end(&harness, cx, &voice, &format!("{step}%: picked"));
        press(harness.window, "escape", cx);

        // The composer grown to several lines: the thread gives way.
        focus_composer(&harness, cx);
        for line in ["a long", "message", "in", "several", "lines"] {
            type_text(harness.window, line, cx);
            press(harness.window, "shift-enter", cx);
        }
        press(harness.window, "alt-up", cx);
        whole_at_the_end(&harness, cx, &voice, &format!("{step}%: a tall composer"));
        focus_composer(&harness, cx);
        press(harness.window, "ctrl-a", cx);
        press(harness.window, "backspace", cx);

        // The oldest message loaded: its outline is whole under the header.
        press(harness.window, "up", cx);
        press(harness.window, "home", cx);
        let (ring, thread) = (
            bounds(harness.window, "message-focus", cx),
            bounds(harness.window, "thread", cx),
        );
        assert!(
            ring.top() >= thread.top(),
            "{step}%: {ring:?} under the header"
        );
        press(harness.window, "end", cx);
    }
}

// ----- a bubble is as large as what it says ------------------------------------

/// The bounds of the bubble in the row of message `id`: the bubble
/// painted inside that row.
fn bubble_of(harness: &Harness, cx: &mut TestAppContext, id: &str) -> Bounds<gpui_kit::Pixels> {
    bounds_of(harness.window, &format!("bubble-{id}"), cx)
}

const ONE_LINE: &str = "So I built this, maybe it brings people.";
const THREE_LINES: &str = "So I built this, maybe it brings people. It took a while, and \
     there is more to do, but the part that matters is there and it works well enough to \
     show around, which is what I wanted before the weekend anyway.";

/// The message `newest` is the newest of the open chat: its bubble is as
/// large as what it says, with the keyboard on it and without, and stays
/// that size when `then` makes it one message among others.
fn as_large_as_any(
    harness: &Harness,
    cx: &mut TestAppContext,
    newest: &str,
    one_line: bool,
    what: &str,
    then: impl FnOnce(&mut TestAppContext),
) {
    let row = bounds_of(harness.window, &format!("row-{newest}"), cx);
    let as_newest = bubble_of(harness, cx, newest);
    // With the keyboard on it.
    focus_composer(harness, cx);
    press(harness.window, "up", cx);
    assert_eq!(focused(harness, cx).as_deref(), Some(newest), "{what}");
    let in_focus = bubble_of(harness, cx, newest);
    focus_composer(harness, cx);
    // Then it is not the newest any more.
    then(cx);
    let as_other = bubble_of(harness, cx, newest);
    for (name, bubble) in [("newest", as_newest), ("in focus", in_focus)] {
        assert!(
            (bubble.size.height - as_other.size.height).abs() <= px(1.)
                && (bubble.size.width - as_other.size.width).abs() <= px(1.),
            "{what}, {name}: {bubble:?}, and {as_other:?} once it is not the newest"
        );
    }
    // The row is its bubble and the room above it: the room of the
    // outline, and any free space of the pane, is outside both.
    assert!(
        row.size.height <= as_newest.size.height + theme::metrics::RUN_GAP() + px(3.),
        "{what}: the row {row:?} around {as_newest:?}"
    );
    // One line is one line; no bubble is as wide as the pane.
    let thread = bounds(harness.window, "thread", cx);
    assert!(as_newest.size.width < thread.size.width * 0.8, "{what}");
    // One line of text and the bubble's padding, at any interface size.
    let line = theme::px(21.) + theme::px(14.) + px(4.);
    if one_line {
        assert!(as_newest.size.height <= line, "{what}: {as_newest:?}");
    } else {
        assert!(as_newest.size.height > line, "{what}: {as_newest:?}");
    }
}

#[gpui_kit::test]
fn the_newest_bubble_is_as_large_as_any_other(cx: &mut TestAppContext) {
    let harness = open_chat_one(cx);
    let mut at = 0;
    for step in theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        cx.run_until_parked();
        for (mine, body) in [
            (true, ONE_LINE),
            (false, ONE_LINE),
            (true, THREE_LINES),
            (false, THREE_LINES),
        ] {
            at += 10;
            let (newest, after) = (format!("n{at}"), format!("a{at}"));
            put(&harness, cx, &newest, mine, body, at);
            let what = format!("{step}%, mine: {mine}, {} characters", body.len());
            as_large_as_any(&harness, cx, &newest, body == ONE_LINE, &what, |cx| {
                put(&harness, cx, &after, !mine, "ok", at + 5)
            });
        }
    }

    // Right after a send, while it goes from waiting to sent: the same
    // bubble all along.
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
    cx.run_until_parked();
    let (account, chat) = open_ids(&harness, cx);
    harness
        .engine
        .send_text(&account, &chat, ONE_LINE.to_owned(), None)
        .unwrap();
    cx.run_until_parked();
    let waiting = bounds(harness.window, "bubble", cx);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    cx.run_until_parked();
    harness.settle(cx);
    let sent = bounds(harness.window, "bubble", cx);
    assert!(
        (waiting.size.height - sent.size.height).abs() <= px(1.) && sent.size.height < px(40.),
        "{waiting:?} while it waits, {sent:?} once sent"
    );
    assert!((waiting.size.width - sent.size.width).abs() <= px(1.));
}

#[gpui_kit::test]
fn the_only_message_of_a_chat_is_as_large_as_what_it_says(cx: &mut TestAppContext) {
    // A new conversation: nothing in it, so what arrives is the newest
    // and the oldest message at once, in a pane that is mostly free.
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "new-chat", cx);
    type_text(harness.window, "+58 424 555 0199", cx);
    click(harness.window, "new-chat-open", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let open = harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .map(|open| open.rows.len());
        assert_eq!(open, Some(0), "an empty conversation");
    });
    for (n, (mine, body)) in [
        (true, ONE_LINE),
        (false, THREE_LINES),
        (false, ONE_LINE),
        (true, THREE_LINES),
    ]
    .into_iter()
    .enumerate()
    {
        let at = n as i64 * 10;
        let (newest, after) = (format!("only{n}"), format!("next{n}"));
        put(&harness, cx, &newest, mine, body, at);
        // The pane has room to spare, and none of it is in the bubble.
        let thread = bounds(harness.window, "thread", cx);
        let bubble = bubble_of(&harness, cx, &newest);
        assert!(bubble.size.height < thread.size.height / 3., "{bubble:?}");
        let what = format!("message {n} of a new chat");
        as_large_as_any(&harness, cx, &newest, body == ONE_LINE, &what, |cx| {
            put(&harness, cx, &after, !mine, "ok", at + 5)
        });
    }
    // The oldest message, with the keyboard on it: the same bubble.
    let oldest = bubble_of(&harness, cx, "only0");
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "home", cx);
    assert_eq!(focused(&harness, cx).as_deref(), Some("only0"));
    assert_eq!(bubble_of(&harness, cx, "only0").size, oldest.size);
    let (ring, thread) = (
        bounds(harness.window, "message-focus", cx),
        bounds(harness.window, "thread", cx),
    );
    assert!(ring.top() >= thread.top() && ring.bottom() <= thread.bottom());
}
