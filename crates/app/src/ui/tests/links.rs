//! WhatsApp links, followed in the window: the chat they name opens with
//! their text ready to send, an invite shows its group and joins it, and
//! Settings says who opens them.

use super::*;
use crate::link_handler::Handler;
use crate::ui::links::LinkSystem;
use client_provider::{AccountId, ChatId, ChatKind};

fn receive(harness: &Harness, link: &str, cx: &mut TestAppContext) {
    cx.update(|cx| crate::ui::receive_link(Some(link.to_owned()), cx));
    // The link is followed at the next frame; the number is checked off
    // the window's thread.
    settle_a_while(harness, cx);
}

fn settle_a_while(harness: &Harness, cx: &mut TestAppContext) {
    for _ in 0..4 {
        harness.settle(cx);
    }
}

fn composed(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell.composer.read(cx).value().to_string()
    })
}

#[gpui_kit::test]
fn a_link_opens_the_chat_it_names_with_its_text_ready_and_unsent(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    receive(
        &harness,
        "https://wa.me/584245550199?text=Hola%2C%20%C2%BFc%C3%B3mo%20est%C3%A1s%3F",
        cx,
    );
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        let open = shell.open.as_ref().expect("the conversation is open");
        assert_eq!(open.chat.id.as_str(), "+584245550199");
    });
    assert_eq!(composed(&harness, cx), "Hola, ¿cómo estás?");
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 0, "nothing is sent by itself");
}

#[gpui_kit::test]
fn a_link_with_only_a_text_lets_the_person_be_chosen(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    receive(&harness, "whatsapp://send?text=look%20at%20this", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::NewChat
    );
    // Chosen in the panel: the text goes with them.
    type_text(harness.window, "+58 424 555 0199", cx);
    click(harness.window, "new-chat-open", cx);
    settle_a_while(&harness, cx);
    assert!(cx.update(|cx| harness.shell.read(cx).open.is_some()));
    assert_eq!(composed(&harness, cx), "look at this");
}

#[gpui_kit::test]
fn a_text_is_dropped_when_the_panel_is_closed_without_a_chat(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    receive(&harness, "whatsapp://send?text=never%20mind", cx);
    press(harness.window, "escape", cx);
    settle_a_while(&harness, cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::None
    );
    cx.update(|cx| assert!(harness.shell.read(cx).links.text.is_none()));
    assert_eq!(composed(&harness, cx), "");
}

#[gpui_kit::test]
fn a_whatsapp_link_in_a_message_is_followed_here_not_in_the_browser(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_link("https://wa.me/584245550199", cx)
        })
    });
    settle_a_while(&harness, cx);
    assert!(cx.opened_url().is_none(), "the browser was not asked");
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let open = shell.open.as_ref().expect("the conversation is open");
        assert_eq!(open.chat.id.as_str(), "+584245550199");
    });
}

/// The account, and the first of its groups.
fn a_group(harness: &Harness) -> (AccountId, ChatId) {
    let store = harness.engine.store();
    let account = store.accounts().unwrap()[0].id.clone();
    let group = store
        .chats(&account, None)
        .unwrap()
        .into_iter()
        .find(|chat| chat.kind == ChatKind::Group)
        .expect("the mock has groups")
        .id;
    (account, group)
}

fn invite_of(harness: &Harness, cx: &mut TestAppContext) -> (bool, Option<String>, bool) {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let invite = shell.links.invite.as_ref().expect("the invite is shown");
        (
            invite.member,
            invite.group.as_ref().map(|group| group.subject.clone()),
            invite.error.is_some(),
        )
    })
}

#[gpui_kit::test]
fn a_group_invite_shows_the_group_and_joins_it_with_a_click(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let (account, group) = a_group(&harness);
    let subject = harness.mock.group(&account, &group).unwrap().subject;
    // Out of the group first: the invite is the way back in.
    harness
        .runtime
        .block_on(harness.engine.leave_group(&account, &group, "left"))
        .unwrap();
    harness.mock.add_invite("AbC123xyz", &account, &group);

    // The query WhatsApp adds to a shared link is not part of the code.
    receive(
        &harness,
        "https://chat.whatsapp.com/AbC123xyz?s=cl&p=i&mlu=4&ilr=4&iam=0",
        cx,
    );
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::JoinGroup
    );
    assert_eq!(invite_of(&harness, cx), (false, Some(subject), false));
    assert!(shows(harness.window, "invite-subject", cx));
    assert_eq!(harness.mock.social_call_count("join_group"), 0, "not yet");

    click(harness.window, "invite-join", cx);
    settle_a_while(&harness, cx);
    assert_eq!(harness.mock.social_call_count("join_group"), 1);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert!(shell.links.invite.is_none());
        let open = shell.open.as_ref().expect("the group is open");
        assert_eq!(open.chat.id, group);
    });
    let stored = harness.engine.store().group(&account, &group).unwrap();
    assert!(!stored.unwrap().departed, "in again");
}

#[gpui_kit::test]
fn an_invite_to_a_group_the_account_is_in_opens_it(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let (account, group) = a_group(&harness);
    harness.mock.add_invite("Mine42", &account, &group);
    receive(&harness, "whatsapp://chat?code=Mine42", cx);
    assert!(invite_of(&harness, cx).0, "already a member");
    press(harness.window, "enter", cx);
    settle_a_while(&harness, cx);
    assert_eq!(harness.mock.social_call_count("join_group"), 0);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.open.as_ref().expect("open").chat.id, group);
    });
}

#[gpui_kit::test]
fn an_invite_that_was_reset_is_said_so_and_nothing_is_joined(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    receive(&harness, "https://chat.whatsapp.com/Gone000", cx);
    assert_eq!(invite_of(&harness, cx), (false, None, true));
    assert!(shows(harness.window, "invite-error", cx));
    click(harness.window, "invite-join", cx);
    settle_a_while(&harness, cx);
    assert_eq!(harness.mock.social_call_count("join_group"), 0);
    press(harness.window, "escape", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert!(shell.links.invite.is_none());
    });
}

#[gpui_kit::test]
fn settings_say_who_opens_the_links_and_make_it_this_application(cx: &mut TestAppContext) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static MADE: AtomicBool = AtomicBool::new(false);
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        cx.set_global(LinkSystem {
            current: || {
                if MADE.load(Ordering::SeqCst) {
                    Handler::Us
                } else {
                    Handler::Other("WhatsApp".into())
                }
            },
            make_default: || {
                MADE.store(true, Ordering::SeqCst);
                Ok("WhatsApp links open here now.".into())
            },
        })
    });
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_overlay(Overlay::Settings, window, cx);
            shell.settings_section = crate::ui::shell::SettingsSection::About;
            cx.notify();
        })
    })
    .unwrap();
    settle_a_while(&harness, cx);
    let handler =
        |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).links.handler.clone());
    assert_eq!(handler(cx), Some(Handler::Other("WhatsApp".into())));
    assert!(shows(harness.window, "links-handler", cx));
    click(harness.window, "links-make-default", cx);
    settle_a_while(&harness, cx);
    assert_eq!(handler(cx), Some(Handler::Us));
    assert!(shows(harness.window, "links-said", cx));
    assert!(!shows(harness.window, "links-make-default", cx));
}
