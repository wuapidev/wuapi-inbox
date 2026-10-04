//! The keyboard from pane to pane: the conversation, the chat list, the
//! rail, and what the keys do while the chat list has it.

use super::*;

fn open_one(cx: &mut TestAppContext) -> Harness {
    open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    )
}

fn open_id(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .map(|open| open.chat.id.to_string())
    })
}

fn cursor(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_cursor
            .as_ref()
            .map(|chat| chat.to_string())
    })
}

/// Whether the chat list has the keyboard.
fn on_list(harness: &Harness, cx: &mut TestAppContext) -> bool {
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.read(cx).list_has_keyboard(window)
    })
    .unwrap()
}

fn rows(harness: &Harness, cx: &mut TestAppContext) -> Vec<(String, bool, u32)> {
    cx.update(|cx| chat_rows(harness.shell.read(cx)))
}

fn composer(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| harness.shell.read(cx).composer.read(cx).value().to_string())
}

fn summary(harness: &Harness, cx: &mut TestAppContext, id: &str) -> client_core::ChatSummary {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_rows
            .iter()
            .find_map(|row| match row {
                ListRow::Chat(chat) if chat.id.as_str() == id => Some(chat.clone()),
                _ => None,
            })
            .expect("the chat is in the list")
    })
}

#[gpui_kit::test]
fn escape_reaches_the_list_the_arrows_walk_it_and_enter_opens(cx: &mut TestAppContext) {
    let harness = open_one(cx);
    let all = rows(&harness, cx);
    let opened = open_id(&harness, cx).unwrap();
    let at = all.iter().position(|row| row.0 == opened).unwrap();
    focus_composer(&harness, cx);
    assert!(!on_list(&harness, cx) && !shows(harness.window, "list-focus", cx));

    // Escape in an empty composer: the keyboard is the chat list's, on
    // the open chat's row. The conversation stays open.
    press(harness.window, "escape", cx);
    assert!(on_list(&harness, cx));
    assert_eq!(open_id(&harness, cx).as_deref(), Some(opened.as_str()));
    assert_eq!(cursor(&harness, cx).as_deref(), Some(opened.as_str()));
    let ring = bounds(harness.window, "list-focus", cx);
    let row = bounds_of(harness.window, &format!("chat-{opened}"), cx);
    assert!(within(ring, row), "{ring:?} in {row:?}");
    // Escape again does not close it either.
    press(harness.window, "escape", cx);
    assert_eq!(open_id(&harness, cx).as_deref(), Some(opened.as_str()));

    // Down and up (and J and K) move the outline, and only the outline:
    // nothing is opened, so nothing is read.
    let unread_before: Vec<u32> = all.iter().map(|row| row.2).collect();
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx), Some(all[at + 1].0.clone()));
    type_text(harness.window, "j", cx);
    assert_eq!(cursor(&harness, cx), Some(all[at + 2].0.clone()));
    type_text(harness.window, "k", cx);
    press(harness.window, "up", cx);
    assert_eq!(cursor(&harness, cx), Some(all[at].0.clone()));
    press(harness.window, "end", cx);
    assert_eq!(cursor(&harness, cx), Some(all.last().unwrap().0.clone()));
    press(harness.window, "down", cx);
    assert_eq!(cursor(&harness, cx), Some(all.last().unwrap().0.clone()));
    press(harness.window, "home", cx);
    assert_eq!(cursor(&harness, cx), Some(all[0].0.clone()));
    press(harness.window, "pagedown", cx);
    let paged = cursor(&harness, cx).unwrap();
    assert!(all.iter().position(|row| row.0 == paged).unwrap() > 1);
    press(harness.window, "pageup", cx);
    assert_eq!(cursor(&harness, cx), Some(all[0].0.clone()));
    harness.settle(cx);
    assert_eq!(open_id(&harness, cx).as_deref(), Some(opened.as_str()));
    let unread_after: Vec<u32> = rows(&harness, cx).iter().map(|row| row.2).collect();
    assert_eq!(unread_after, unread_before);
    // The outline is on screen wherever it went.
    press(harness.window, "end", cx);
    assert!(shows(harness.window, "list-focus", cx));

    // Enter opens that chat, and typing goes to its composer.
    let last = all.last().unwrap().0.clone();
    press(harness.window, "enter", cx);
    assert_eq!(open_id(&harness, cx), Some(last.clone()));
    assert!(!on_list(&harness, cx) && !shows(harness.window, "list-focus", cx));
    type_text(harness.window, "hello", cx);
    assert_eq!(composer(&harness, cx), "hello");

    // With something typed, Escape leaves the keyboard where it is, and
    // the single keys of the list are letters.
    press(harness.window, "escape", cx);
    assert!(!on_list(&harness, cx));
    type_text(harness.window, "p", cx);
    assert_eq!(composer(&harness, cx), "hellop");
    assert_eq!(rows(&harness, cx), all, "nothing was pinned by typing a p");
    // Ctrl+L goes to the list all the same, and Right comes back.
    press(harness.window, "ctrl-l", cx);
    assert!(on_list(&harness, cx));
    assert_eq!(cursor(&harness, cx), Some(last.clone()));
    press(harness.window, "right", cx);
    assert!(!on_list(&harness, cx));
    type_text(harness.window, "!", cx);
    assert_eq!(composer(&harness, cx), "hellop!");
}

#[gpui_kit::test]
fn single_keys_act_on_the_chat_in_focus_without_opening_it(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "list-menu", cx);
    click(harness.window, "menu-read-all", cx);
    harness.settle(cx);
    cx.update(|cx| harness.shell.update(cx, |shell, cx| shell.close_chat(cx)));
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness
            .shell
            .update(cx, |shell, cx| shell.focus_list(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
    // With no conversation open the keyboard is the list's.
    assert!(on_list(&harness, cx));
    press(harness.window, "home", cx);
    press(harness.window, "down", cx);
    press(harness.window, "down", cx);
    let chat = cursor(&harness, cx).unwrap();
    let before = summary(&harness, cx, &chat);

    // P pins, M mutes, U marks unread and read again; the chat is not
    // opened by any of them.
    type_text(harness.window, "p", cx);
    harness.settle(cx);
    assert_eq!(summary(&harness, cx, &chat).pinned, !before.pinned);
    // The outline stays on the chat, wherever the list moved it to.
    assert_eq!(cursor(&harness, cx).as_deref(), Some(chat.as_str()));
    assert!(shows(harness.window, "list-focus", cx));
    type_text(harness.window, "p", cx);
    type_text(harness.window, "m", cx);
    harness.settle(cx);
    assert_eq!(summary(&harness, cx, &chat).pinned, before.pinned);
    assert_eq!(summary(&harness, cx, &chat).muted, !before.muted);
    assert_eq!(before.unread_count, 0);
    type_text(harness.window, "u", cx);
    harness.settle(cx);
    assert!(summary(&harness, cx, &chat).unread_count > 0);
    type_text(harness.window, "u", cx);
    harness.settle(cx);
    assert_eq!(summary(&harness, cx, &chat).unread_count, 0);
    assert_eq!(open_id(&harness, cx), None);

    // A message arrives in another chat and the list is drawn again: the
    // outline is still on the same chat.
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    assert_eq!(cursor(&harness, cx).as_deref(), Some(chat.as_str()));
    let ring = bounds(harness.window, "list-focus", cx);
    assert!(within(
        ring,
        bounds_of(harness.window, &format!("chat-{chat}"), cx)
    ));

    // The menu key opens the row's menu, beside the row; Escape gives the
    // keyboard back to the list.
    press(harness.window, "shift-f10", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::RowMenu);
        assert_eq!(
            shell.menu_target.as_ref().map(|chat| chat.id.to_string()),
            Some(chat.clone())
        );
    });
    press(harness.window, "escape", cx);
    assert!(on_list(&harness, cx));
    assert_eq!(cursor(&harness, cx).as_deref(), Some(chat.as_str()));

    // A archives: the chat leaves this list; Tab walks the filters to
    // where it went.
    type_text(harness.window, "a", cx);
    harness.settle(cx);
    assert!(!rows(&harness, cx).iter().any(|row| row.0 == chat));
    let filter = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).filter);
    assert_eq!(filter(cx), ChatFilter::All);
    press(harness.window, "tab", cx);
    assert_eq!(filter(cx), ChatFilter::Unread);
    press(harness.window, "shift-tab", cx);
    assert_eq!(filter(cx), ChatFilter::All);
    for _ in 0..3 {
        press(harness.window, "tab", cx);
    }
    assert_eq!(filter(cx), ChatFilter::Archived);
    assert!(on_list(&harness, cx));
    assert!(rows(&harness, cx).iter().any(|row| row.0 == chat));
    press(harness.window, "tab", cx);
    assert_eq!(filter(cx), ChatFilter::All);
}

#[gpui_kit::test]
fn typing_on_the_list_searches_and_the_panes_are_a_key_apart(cx: &mut TestAppContext) {
    let harness = open_one(cx);
    focus_composer(&harness, cx);
    press(harness.window, "escape", cx);
    assert!(on_list(&harness, cx));

    // A character that is not a key of the list starts a search with it;
    // what follows goes on in the field.
    let query = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).query.clone());
    type_text(harness.window, "e", cx);
    cx.run_until_parked();
    assert!(!on_list(&harness, cx));
    type_text(harness.window, "l", cx);
    harness.settle(cx);
    assert_eq!(query(cx), "el");
    // Back on the list, Escape empties it.
    press(harness.window, "ctrl-l", cx);
    assert!(on_list(&harness, cx));
    press(harness.window, "escape", cx);
    harness.settle(cx);
    assert_eq!(query(cx), "");
    // "/" goes to the field without typing anything.
    type_text(harness.window, "/", cx);
    assert!(!on_list(&harness, cx));
    type_text(harness.window, "an", cx);
    harness.settle(cx);
    assert_eq!(query(cx), "an");
    press(harness.window, "ctrl-a", cx);
    press(harness.window, "backspace", cx);
    harness.settle(cx);

    // Alt+Left and Alt+Right: conversation, list, rail, and back.
    let rail = |cx: &mut TestAppContext| {
        cx.update_window(harness.window.into(), |_, window, cx| {
            harness.shell.read(cx).rail_focused(window)
        })
        .unwrap()
    };
    focus_composer(&harness, cx);
    press(harness.window, "alt-left", cx);
    assert!(on_list(&harness, cx) && rail(cx).is_none());
    press(harness.window, "alt-left", cx);
    assert!(!on_list(&harness, cx));
    let number = rail(cx).expect("a number of the rail has the keyboard");
    // It is the number on screen.
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(number, shell.rail_key(shell.account.as_ref().unwrap()));
    });
    // Further left there is nothing.
    press(harness.window, "alt-left", cx);
    assert_eq!(rail(cx), Some(number.clone()));
    // Right, or Alt+Right: the list; again: the conversation.
    press(harness.window, "right", cx);
    assert!(on_list(&harness, cx) && rail(cx).is_none());
    press(harness.window, "alt-left", cx);
    assert_eq!(rail(cx), Some(number));
    press(harness.window, "alt-right", cx);
    assert!(on_list(&harness, cx));
    press(harness.window, "alt-right", cx);
    assert!(!on_list(&harness, cx));
    type_text(harness.window, "back", cx);
    assert_eq!(composer(&harness, cx), "back");
}

#[gpui_kit::test]
fn the_rails_arrows_walk_its_numbers_and_enter_selects(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let harness = open_with_numbers(cx, 2);
    let rail = |cx: &mut TestAppContext| {
        cx.update_window(harness.window.into(), |_, window, cx| {
            harness.shell.read(cx).rail_focused(window)
        })
        .unwrap()
    };
    let account = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            shell.rail_key(shell.account.as_ref().unwrap())
        })
    };
    press(harness.window, "ctrl-l", cx);
    press(harness.window, "alt-left", cx);
    let first = rail(cx).unwrap();
    assert_eq!(first, account(cx));
    press(harness.window, "down", cx);
    let second = rail(cx).unwrap();
    assert_ne!(second, first);
    // Moving the keyboard selects nothing; Enter does.
    assert_eq!(account(cx), first);
    press(harness.window, "enter", cx);
    cx.run_until_parked();
    assert_eq!(account(cx), second);
    press(harness.window, "up", cx);
    assert_eq!(rail(cx), Some(first.clone()));
    press(harness.window, "up", cx);
    assert_eq!(rail(cx), Some(first));
    // Right: the chats of the number that was selected.
    press(harness.window, "right", cx);
    assert!(on_list(&harness, cx));
    press(harness.window, "down", cx);
    assert!(shows(harness.window, "list-focus", cx));
}
