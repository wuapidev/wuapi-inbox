//! A chat's incoming webhooks in the real window: offered in the chat's
//! menu only where the number's provider has them, then made, renamed,
//! copied, paused, given a new URL and deleted from their sheet.

use super::*;
use crate::ui::shell::Overlay;
use client_provider::{Capabilities, ChatId};

fn first_chat(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone())
}

fn clipboard(cx: &mut TestAppContext) -> String {
    cx.read_from_clipboard()
        .and_then(|item| item.text())
        .unwrap_or_default()
}

#[gpui_kit::test]
fn a_chats_menu_makes_and_manages_its_incoming_webhooks(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    harness.settle(cx);
    let chat = first_chat(&harness, cx);

    right_click(harness.window, &format!("chat-{chat}"), cx);
    assert!(shows(harness.window, "menu-hooks", cx));
    click(harness.window, "menu-hooks", cx);
    harness.settle(cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::Hooks));
    assert!(shows(harness.window, "hooks-none", cx));

    // Its name is typed, Enter makes it.
    type_text(harness.window, "Signups", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    let held = harness.mock.incoming_webhooks_held();
    assert_eq!(held.len(), 1);
    assert_eq!(
        (held[0].name.as_str(), &held[0].to),
        ("Signups", &ChatId::new(chat.clone()))
    );
    assert!(shows(harness.window, "hook-0", cx) && !shows(harness.window, "hooks-none", cx));
    assert!(
        shows(harness.window, "hooks-note", cx),
        "says to keep it private"
    );

    // Renamed in place: the field starts with the name, Enter saves it.
    click(harness.window, "hook-0-rename", cx);
    assert!(shows(harness.window, "hook-rename-field", cx));
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "New signups", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.incoming_webhooks_held()[0].name, "New signups");
    assert!(!shows(harness.window, "hook-rename-field", cx));
    assert!(shows(harness.window, "hook-0-name", cx));

    // Cancel leaves it as it was.
    click(harness.window, "hook-0-rename", cx);
    type_text(harness.window, " and more", cx);
    click(harness.window, "hook-0-rename-cancel", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.incoming_webhooks_held()[0].name, "New signups");
    assert!(!shows(harness.window, "hook-rename-field", cx));

    click(harness.window, "hook-copy", cx);
    assert_eq!(clipboard(cx), held[0].url);

    click(harness.window, "hook-0-pause", cx);
    harness.settle(cx);
    assert!(!harness.mock.incoming_webhooks_held()[0].enabled);

    // A new URL is asked about first: the old one stops working.
    click(harness.window, "hook-0-rotate", cx);
    assert!(shows(harness.window, "hook-question", cx));
    assert_eq!(harness.mock.incoming_webhooks_held()[0].url, held[0].url);
    click(harness.window, "hook-question-confirm", cx);
    harness.settle(cx);
    assert_ne!(harness.mock.incoming_webhooks_held()[0].url, held[0].url);

    click(harness.window, "hook-0-delete", cx);
    click(harness.window, "hook-question-confirm", cx);
    harness.settle(cx);
    assert!(harness.mock.incoming_webhooks_held().is_empty());
    assert!(shows(harness.window, "hooks-none", cx));

    press(harness.window, "escape", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert!(shell.hooks.is_none(), "the URLs are let go with the sheet");
    });
}

#[gpui_kit::test]
fn a_nameless_webhook_is_not_made(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    harness.settle(cx);
    let chat = first_chat(&harness, cx);
    right_click(harness.window, &format!("chat-{chat}"), cx);
    click(harness.window, "menu-hooks", cx);
    harness.settle(cx);

    click(harness.window, "hook-create", cx);
    harness.settle(cx);

    assert!(shows(harness.window, "hooks-error", cx));
    assert!(harness.mock.incoming_webhooks_held().is_empty());
}

#[gpui_kit::test]
fn a_number_without_them_is_not_offered_incoming_webhooks(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let mock = MockProvider::new(provider_mock::MockConfig {
        capabilities: Some(Capabilities {
            incoming_webhooks: false,
            ..Capabilities::all()
        }),
        ..provider_mock::MockConfig::quiet()
    });
    let harness = open_over(cx, ShellOptions::default(), mock);
    harness.settle(cx);
    let chat = first_chat(&harness, cx);

    right_click(harness.window, &format!("chat-{chat}"), cx);

    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::RowMenu));
    assert!(shows(harness.window, "menu-pin", cx));
    assert!(!shows(harness.window, "menu-hooks", cx));
}
