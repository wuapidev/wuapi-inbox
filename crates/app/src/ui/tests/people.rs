//! Names in the real window: a mention made by a hidden-number id reads
//! as the person, the account's own mention as "You", a name that
//! arrives later is on the messages already drawn, and `@` in a group's
//! composer mentions for real.

use super::*;
use client_provider::{
    AccountId, ChatId, Contact, ContactId, Mention, Message, MessageExtras, MessageId,
    ProviderEvent, Timestamp,
};
use provider_mock::{SHOWCASE_ACCOUNT, SHOWCASE_CHAT};

const ANA: &str = "+351912345678";
const ANA_LID: &str = "lid:200055501000001";

fn open_demo_group(cx: &mut TestAppContext) -> Harness {
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_chat(cx);
            shell.open_chat(ChatId::new(SHOWCASE_CHAT), None, cx);
        })
    });
    // The group's participants are read when its chat opens.
    harness.settle(cx);
    harness
}

/// The newest message of the open chat as the window holds it.
fn newest(harness: &Harness, cx: &mut TestAppContext) -> Message {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let rows = &shell.open.as_ref().unwrap().rows;
        rows.iter()
            .rev()
            .find_map(|row| match row {
                Row::Message(row) => Some(row.stored.message.clone()),
                Row::Day(_) => None,
            })
            .expect("a message")
    })
}

fn incoming(harness: &Harness, cx: &mut TestAppContext, id: &str, body: &str, who: &[&str]) {
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let message = Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: chat.account_id.clone(),
        chat_id: chat.id.clone(),
        sender: ContactId::new("contact:somebodyelse"),
        sender_name: Some("Somebody".into()),
        direction: Direction::Incoming,
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 120_000),
        content: MessageContent::text(body),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: MessageExtras {
            mentions: who
                .iter()
                .map(|id| {
                    let id = ContactId::new(*id);
                    Mention {
                        handle: client_provider::mention_handle(&id),
                        id,
                        name: None,
                        me: false,
                    }
                })
                .collect(),
            ..Default::default()
        },
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_mention_by_hidden_number_id_reads_as_the_person_once_they_are_known(cx: &mut TestAppContext) {
    let harness = open_demo_group(cx);
    let account = AccountId::new(SHOWCASE_ACCOUNT);
    // As the API delivers it: the number in the list, the digits of the
    // hidden-number id in the text.
    incoming(
        &harness,
        cx,
        "lid-mention",
        "@200055501000001 are you in?",
        &[ANA],
    );
    let before = newest(&harness, cx);
    assert_eq!(before.extras.mentions[0].handle, "200055501000001");
    assert_eq!(
        client_core::message_preview(&before),
        "@+351 912 345 678 are you in?",
        "the number, never the digits of the hidden id"
    );
    assert!(shows(harness.window, "message-text", cx));

    // The address book arrives while the chat is open: the row already on
    // screen says her name, and so does the chat list's line.
    let mut ana = Contact::new(account.clone(), ContactId::new(ANA));
    ana.phone = Some(ANA.into());
    ana.saved_name = Some("Ana Rojas".into());
    ana.alt_ids = vec![ContactId::new(ANA_LID)];
    harness
        .engine
        .store()
        .upsert_listed_contacts(&account, &[ana], Timestamp::now())
        .unwrap();
    cx.run_until_parked();
    let after = newest(&harness, cx);
    assert_eq!(after.extras.mentions[0].name.as_deref(), Some("Ana Rojas"));
    assert_eq!(
        client_core::message_preview(&after),
        "@Ana Rojas are you in?"
    );
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let line = shell
            .list_rows
            .iter()
            .find_map(|row| match row {
                ListRow::Chat(chat) if chat.id.as_str() == SHOWCASE_CHAT => {
                    chat.last_message.as_ref().map(|last| last.text.clone())
                }
                _ => None,
            })
            .expect("the group's row");
        assert_eq!(line, "@Ana Rojas are you in?");
    });

    // A mention nobody can be found for is "someone"; text that only
    // looks like one stays as typed.
    incoming(
        &harness,
        cx,
        "unknown-mention",
        "@99887766554433 the code is @123456",
        &["lid:99887766554433"],
    );
    assert_eq!(
        client_core::message_preview(&newest(&harness, cx)),
        "@someone the code is @123456"
    );
}

#[gpui_kit::test]
fn the_accounts_own_mention_is_you(cx: &mut TestAppContext) {
    let harness = open_demo_group(cx);
    let me = stored_accounts(&harness)
        .into_iter()
        .find(|account| account.id.as_str() == SHOWCASE_ACCOUNT)
        .and_then(|account| account.self_contact)
        .expect("the demo account's own id");
    let handle = harness.engine.mention_handle(&me);
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let mut message = Message {
        id: MessageId::new("you"),
        client_id: None,
        account_id: chat.account_id.clone(),
        chat_id: chat.id.clone(),
        sender: ContactId::new("contact:somebodyelse"),
        sender_name: Some("Somebody".into()),
        direction: Direction::Incoming,
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 120_000),
        content: MessageContent::text(format!("@{handle} can you check?")),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: MessageExtras::default(),
    };
    message.extras.mentions = vec![Mention {
        id: me,
        handle,
        name: None,
        me: false,
    }];
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    cx.run_until_parked();
    let shown = newest(&harness, cx);
    assert!(shown.extras.mentions[0].me);
    assert_eq!(client_core::message_preview(&shown), "@You can you check?");
}

#[gpui_kit::test]
fn an_at_in_a_groups_composer_lists_the_participants_and_mentions_for_real(
    cx: &mut TestAppContext,
) {
    let harness = open_demo_group(cx);
    let (account, chat) = (AccountId::new(SHOWCASE_ACCOUNT), ChatId::new(SHOWCASE_CHAT));
    let people = harness
        .engine
        .store()
        .group_participants(&account, &chat, None, 100)
        .unwrap();
    let noah = people
        .iter()
        .find(|person| person.name == "Noah Fischer")
        .expect("Noah is in the group")
        .clone();
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell
                .composer
                .update(cx, |composer, cx| composer.focus(window, cx))
        })
    })
    .unwrap();

    assert!(!shows(harness.window, "mention-picker", cx));
    type_text(harness.window, "hey @", cx);
    assert!(shows(harness.window, "mention-picker", cx), "everyone");
    cx.update(|cx| {
        let matches = harness.shell.read(cx).mention_matches(cx);
        assert!(matches.len() > 1);
        assert!(matches.iter().all(|person| !person.me), "not oneself");
    });
    type_text(harness.window, "noa", cx);
    cx.update(|cx| {
        let matches = harness.shell.read(cx).mention_matches(cx);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].contact, noah.contact);
    });

    // Escape closes the list until the text changes; Enter then sends
    // nothing by accident, because the list is back with the next letter.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "mention-picker", cx));
    press(harness.window, "backspace", cx);
    assert!(shows(harness.window, "mention-picker", cx));

    // Enter picks: the name is in the text, and nothing was sent.
    let before = harness.engine.store().outbox_pending().unwrap().len();
    press(harness.window, "enter", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(
            shell.composer.read(cx).value().as_ref(),
            "hey @Noah Fischer "
        );
    });
    assert_eq!(
        harness.engine.store().outbox_pending().unwrap().len(),
        before
    );
    assert!(!shows(harness.window, "mention-picker", cx));

    type_text(harness.window, "table for five?", cx);
    press(harness.window, "enter", cx);
    let queued = harness.engine.store().outbox_pending().unwrap();
    let sent = &queued.last().expect("queued").message;
    let handle = harness.engine.mention_handle(&noah.contact);
    assert_eq!(
        sent.content,
        client_provider::OutgoingContent::Text {
            body: format!("hey @{handle} table for five?")
        }
    );
    assert_eq!(sent.mentions.len(), 1);
    assert_eq!(sent.mentions[0].id, noah.contact);
    assert_eq!(sent.mentions[0].handle, handle);

    // The bubble that is already there reads by name.
    let pending = newest(&harness, cx);
    assert_eq!(
        client_core::message_preview(&pending),
        "hey @Noah Fischer table for five?"
    );
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.composer.read(cx).value().is_empty());
        assert!(shell.mentioning.picked.is_empty());
    });
}

#[gpui_kit::test]
fn a_direct_chat_offers_nobody_to_mention(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let kind = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.kind);
    if kind != client_provider::ChatKind::Direct {
        return;
    }
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell
                .composer
                .update(cx, |composer, cx| composer.focus(window, cx))
        })
    })
    .unwrap();
    type_text(harness.window, "@", cx);
    assert!(!shows(harness.window, "mention-picker", cx));
}

// ----- the caption of what is attached -----------------------------------------------

#[gpui_kit::test]
fn the_caption_of_an_attached_file_is_a_composer_of_its_own(cx: &mut TestAppContext) {
    use client_provider::{MediaKind, OutgoingContent};
    let dir = tempfile::tempdir().unwrap();
    let picture = dir.path().join("photo.png");
    image::RgbaImage::from_pixel(320, 160, image::Rgba([30, 120, 200, 255]))
        .save(&picture)
        .unwrap();
    let picked = vec![picture];
    let harness = open(
        cx,
        ShellOptions {
            pick_files: Some(std::rc::Rc::new(move |_, _| {
                gpui_kit::Task::ready(Some(picked.clone()))
            })),
            ..Default::default()
        },
    );
    harness.settle(cx);
    // A group, so that there are people to mention.
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_chat(cx);
            shell.open_chat(ChatId::new(SHOWCASE_CHAT), None, cx);
        })
    });
    harness.settle(cx);
    let (account, chat) = cx.update(|cx| {
        let chat = &harness.shell.read(cx).open.as_ref().unwrap().chat;
        (chat.account_id.clone(), chat.id.clone())
    });
    let noah = harness
        .engine
        .store()
        .group_participants(&account, &chat, None, 50)
        .unwrap()
        .into_iter()
        .find(|person| person.name == "Noah Fischer")
        .expect("Noah is in the group");
    let caption = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .captioned_field()
                .expect("a caption on show")
                .read(cx)
                .value()
                .to_string()
        })
    };
    let overlay = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).overlay);
    let settle_search = |cx: &mut TestAppContext| {
        cx.run_until_parked();
        cx.executor()
            .advance_clock(crate::motion::SEARCH_DEBOUNCE + Duration::from_millis(10));
        cx.run_until_parked();
    };

    // A reply is under way when the file is attached: the sheet shows
    // what is answered.
    let answered = cx.update(|cx| {
        harness.shell.update(cx, |shell, _| {
            let message = shell
                .open
                .as_ref()
                .unwrap()
                .rows
                .iter()
                .rev()
                .find_map(|row| match row {
                    Row::Message(row)
                        if row.stored.message.direction == Direction::Incoming
                            && !row.stored.message.deleted =>
                    {
                        Some(row.stored.message.clone())
                    }
                    _ => None,
                })
                .expect("a message to answer");
            shell.acting.compose =
                crate::ui::message_actions::Compose::Reply(Box::new(message.clone()));
            message.id
        })
    });
    click(harness.window, "attach", cx);
    until(&harness, cx, sheet_open);
    let sheet = bounds(harness.window, "attach-sheet", cx);
    for part in ["attach-caption", "caption-emoji", "compose-about"] {
        assert!(within(bounds(harness.window, part, cx), sheet), "{part}");
    }
    let one_line = bounds(harness.window, "attach-caption", cx);

    // "@" lists the group's people, in the sheet, over the caption.
    type_text(harness.window, "for @noa", cx);
    let list = bounds(harness.window, "mention-picker", cx);
    assert!(
        within(list, sheet) && list.bottom() <= bounds(harness.window, "attach-caption", cx).top()
    );
    cx.update(|cx| {
        let matches = harness.shell.read(cx).mention_matches(cx);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].contact, noah.contact);
    });
    // Escape closes the list, not the sheet; the next letter brings it back.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "mention-picker", cx));
    assert_eq!(overlay(cx), Overlay::AttachSheet);
    press(harness.window, "backspace", cx);
    assert!(shows(harness.window, "mention-picker", cx));
    // Enter picks, and sends nothing.
    press(harness.window, "enter", cx);
    assert_eq!(caption(cx), "for @Noah Fischer ");
    assert_eq!(overlay(cx), Overlay::AttachSheet);
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
    // The composer underneath was not written in.
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.composer.read(cx).value().is_empty());
        assert!(shell.mentioning.picked.is_empty());
        assert_eq!(
            shell.attach.as_ref().unwrap().items[0]
                .mentioning
                .picked
                .len(),
            1
        );
    });

    // A shortcode offers its emoji; Tab takes it.
    type_text(harness.window, ":fir", cx);
    settle_search(cx);
    settle_search(cx);
    assert!(within(
        bounds(harness.window, "emoji-completion", cx),
        sheet
    ));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "emoji-completion", cx));
    assert_eq!(overlay(cx), Overlay::AttachSheet);
    type_text(harness.window, "e", cx);
    settle_search(cx);
    press(harness.window, "tab", cx);
    assert_eq!(caption(cx), "for @Noah Fischer 🔥");

    // Shift+Enter is a new line, and the field grows with it.
    press(harness.window, "shift-enter", cx);
    type_text(harness.window, "*see* the second line", cx);
    assert_eq!(caption(cx), "for @Noah Fischer 🔥\n*see* the second line");
    assert_eq!(overlay(cx), Overlay::AttachSheet);
    let two_lines = bounds(harness.window, "attach-caption", cx);
    assert!(
        two_lines.size.height > one_line.size.height + px(4.),
        "{one_line:?} → {two_lines:?}"
    );
    assert!(within(
        two_lines,
        bounds(harness.window, "attach-sheet", cx)
    ));

    // The emoji picker, from the button and from its key: a pick lands in
    // the caption and the sheet is back, with the keyboard in the caption.
    press(harness.window, "ctrl-e", cx);
    assert_eq!(overlay(cx), Overlay::EmojiPicker);
    cx.update(|cx| assert!(harness.shell.read(cx).attach.is_some(), "the files wait"));
    press(harness.window, "escape", cx);
    assert_eq!(overlay(cx), Overlay::AttachSheet);
    click(harness.window, "caption-emoji", cx);
    assert_eq!(overlay(cx), Overlay::EmojiPicker);
    type_text(harness.window, "pizza", cx);
    settle_search(cx);
    settle_search(cx);
    press(harness.window, "enter", cx);
    assert_eq!(overlay(cx), Overlay::AttachSheet);
    assert_eq!(caption(cx), "for @Noah Fischer 🔥\n*see* the second line🍕");
    type_text(harness.window, "!", cx);
    assert!(caption(cx).ends_with("🍕!"), "the caption has the keyboard");

    // Enter sends: one media message, as a reply, with the mention.
    press(harness.window, "enter", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert!(shell.attach.is_none());
        // The reply was the files': it is done with.
        assert_eq!(
            shell.acting.compose,
            crate::ui::message_actions::Compose::New
        );
    });
    until(&harness, cx, |shell| own_media(shell) == 1);
    let queued = harness.engine.store().outbox_pending().unwrap();
    let sent = &queued.last().expect("queued").message;
    let handle = harness.engine.mention_handle(&noah.contact);
    let OutgoingContent::Media {
        kind,
        caption: said,
        ..
    } = &sent.content
    else {
        panic!("not a file: {:?}", sent.content);
    };
    assert_eq!(*kind, MediaKind::Image);
    assert_eq!(
        said.as_deref(),
        Some(format!("for @{handle} 🔥\n*see* the second line🍕!").as_str())
    );
    assert_eq!(sent.mentions.len(), 1);
    assert_eq!(sent.mentions[0].id, noah.contact);
    assert_eq!(sent.reply_to.as_ref(), Some(&answered));

    // Through the outbox to the provider: uploaded, then sent with all of it.
    let sends = harness.mock.send_calls();
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(harness.mock.send_calls(), sends + 1);
    let delivered = harness.mock.sent().pop().expect("the provider got it");
    assert_eq!(delivered.mentions.len(), 1);
    assert_eq!(delivered.mentions[0].id, noah.contact);
    assert_eq!(delivered.reply_to.as_ref(), Some(&answered));
    // The bubble reads by name, in the mention's colour like any other.
    cx.run_until_parked();
    let shown = newest(&harness, cx);
    assert!(
        client_core::message_preview(&shown).contains("@Noah Fischer"),
        "{}",
        client_core::message_preview(&shown)
    );
}
