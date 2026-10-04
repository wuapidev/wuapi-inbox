//! Forwarding in the real window: any kind of message, several at once in
//! the order they were written, only with a provider that can; and the
//! menus that say why something cannot be done without cutting its name.

use super::*;
use crate::ui::shell::Overlay;
use client_provider::{
    AccountId, ChatId, ContactId, DeliveryStatus, Direction, Media, MediaKind, MediaRef, Message,
    MessageContent, MessageId, OutgoingContent, ProviderEvent, Timestamp,
};

fn open_chat(cx: &mut TestAppContext) -> Harness {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    harness.settle(cx);
    harness
}

fn ids(harness: &Harness, cx: &mut TestAppContext) -> (AccountId, ChatId) {
    cx.update(|cx| {
        let chat = &harness.shell.read(cx).open.as_ref().unwrap().chat;
        (chat.account_id.clone(), chat.id.clone())
    })
}

/// A message arrives in the open chat, on "WhatsApp" (the provider's
/// world, which forwarding reads) and here.
fn arrive(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    at: i64,
    content: MessageContent,
    view_once: bool,
) {
    let (account, chat) = ids(harness, cx);
    let mut message = Message {
        id: MessageId::new(id),
        client_id: None,
        account_id: account,
        chat_id: chat,
        sender: ContactId::new("them"),
        sender_name: None,
        direction: Direction::Incoming,
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 120_000 + at * 1_000),
        content,
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: Default::default(),
    };
    message.extras.view_once = view_once;
    harness
        .mock
        .push_event(ProviderEvent::MessageUpserted(message.clone()));
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message))
        .unwrap();
    cx.run_until_parked();
}

fn media(kind: MediaKind, url: &str, caption: Option<&str>) -> MessageContent {
    let mut media = Media::new(kind);
    media.source = Some(MediaRef::new(url));
    media.caption = caption.map(str::to_owned);
    media.mime_type = Some(
        match kind {
            MediaKind::Image => "image/jpeg",
            MediaKind::Voice => "audio/ogg",
            MediaKind::Sticker => "image/webp",
            _ => "application/pdf",
        }
        .to_owned(),
    );
    MessageContent::Media(media)
}

fn picked_rows(harness: &Harness, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .subjects()
            .iter()
            .map(|message| message.id.to_string())
            .collect()
    })
}

fn forwarded_bubbles(harness: &Harness, chat: &ChatId, account: &AccountId) -> Vec<Message> {
    harness
        .engine
        .store()
        .messages(account, chat, 500)
        .unwrap()
        .into_iter()
        .map(|stored| stored.message)
        .filter(|message| message.extras.forwarded)
        .collect()
}

// ----- the sheet ---------------------------------------------------------------------------------

#[test]
fn the_sheet_is_titled_by_how_many_messages_it_is_about() {
    use crate::ui::message_actions::not_forwarded;
    let one = [("Photo", "WhatsApp no longer has this file")];
    assert_eq!(
        not_forwarded(&one),
        "Not forwarded: Photo (whatsapp no longer has this file)"
    );
    let two = [
        ("Photo", "A view-once message cannot be forwarded"),
        ("Poll", "x"),
    ];
    assert!(not_forwarded(&two).starts_with("2 messages not forwarded: Photo ("));
}

#[gpui_kit::test]
fn photos_voice_notes_documents_and_stickers_are_forwarded_with_text_in_the_order_written(
    cx: &mut TestAppContext,
) {
    let harness = open_chat(cx);
    arrive(
        &harness,
        cx,
        "w1",
        1,
        MessageContent::text("see this"),
        false,
    );
    arrive(
        &harness,
        cx,
        "w2",
        2,
        media(MediaKind::Image, "https://wa/i", Some("the lake")),
        false,
    );
    arrive(
        &harness,
        cx,
        "w3",
        3,
        media(MediaKind::Voice, "https://wa/v", None),
        false,
    );
    arrive(
        &harness,
        cx,
        "w4",
        4,
        media(MediaKind::Document, "https://wa/d", None),
        false,
    );
    arrive(
        &harness,
        cx,
        "w5",
        5,
        media(MediaKind::Sticker, "https://wa/s", None),
        false,
    );

    // Five messages picked with the keyboard, the sheet is about them all.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    for _ in 0..4 {
        press(harness.window, "shift-up", cx);
    }
    assert_eq!(picked_rows(&harness, cx), ["w1", "w2", "w3", "w4", "w5"]);
    click(harness.window, "selection-forward", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::Forward
    );
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).acting.subjects.len()),
        5
    );
    assert!(!shows(harness.window, "forward-skipped", cx));

    click(harness.window, "forward-chat-0", cx);
    click(harness.window, "forward-chat-1", cx);
    let targets = cx.update(|cx| harness.shell.read(cx).acting.forward_to.clone());
    assert_eq!(targets.len(), 2);
    // (The demo chats already hold a forwarded message or two.)
    let account = ids(&harness, cx).0;
    let before: Vec<usize> = targets
        .iter()
        .map(|chat| forwarded_bubbles(&harness, chat, &account).len())
        .collect();
    click(harness.window, "forward-send", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::None
    );

    // Queued per chat in the order they were written, as the kind each is,
    // and shown at once, pending and marked.
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 10);
    for chat in &targets {
        let kinds: Vec<String> = pending
            .iter()
            .filter(|entry| &entry.message.chat_id == chat)
            .map(|entry| match &entry.message.content {
                OutgoingContent::Text { body } => format!("text:{body}"),
                OutgoingContent::Forward { source } => format!("forward:{source}"),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            [
                "text:see this",
                "forward:w2",
                "forward:w3",
                "forward:w4",
                "forward:w5"
            ]
        );
        assert!(pending
            .iter()
            .filter(|entry| &entry.message.chat_id == chat)
            .all(|entry| entry.message.forwarded));
        let bubbles: Vec<Message> = forwarded_bubbles(&harness, chat, &account)
            .into_iter()
            .filter(|bubble| bubble.status == DeliveryStatus::Pending)
            .collect();
        assert_eq!(bubbles.len(), 5);
        assert!(bubbles.iter().all(|bubble| bubble.extras.forwarded));
    }

    // Then the provider forwards the originals: no file moves from here.
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(harness.mock.upload_calls(), 0);
    assert_eq!(harness.mock.forward_calls().len(), 8);
    for (chat, before) in targets.iter().zip(before) {
        let bubbles = forwarded_bubbles(&harness, chat, &account);
        assert_eq!(bubbles.len(), before + 5);
        let ours: Vec<&Message> = bubbles
            .iter()
            .filter(|bubble| bubble.client_id.is_some())
            .collect();
        assert_eq!(ours.len(), 5);
        assert!(ours
            .iter()
            .all(|bubble| bubble.status == DeliveryStatus::Sent));
    }
}

#[gpui_kit::test]
fn what_cannot_be_forwarded_is_said_and_the_rest_goes(cx: &mut TestAppContext) {
    let harness = open_chat(cx);
    arrive(&harness, cx, "k1", 1, MessageContent::text("kept"), false);
    arrive(
        &harness,
        cx,
        "k2",
        2,
        media(MediaKind::Image, "https://wa/once", None),
        true,
    );
    arrive(
        &harness,
        cx,
        "k3",
        3,
        media(MediaKind::Sticker, "https://wa/ok", None),
        false,
    );
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "shift-up", cx);
    press(harness.window, "shift-up", cx);
    click(harness.window, "selection-forward", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::Forward
    );
    // The sheet is about the two that can go, and says what it left out.
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).acting.subjects.len()),
        2
    );
    assert!(shows(harness.window, "forward-skipped", cx));
    let skipped = bounds(harness.window, "forward-skipped", cx);
    let sheet = bounds(harness.window, "forward-sheet", cx);
    assert!(within(skipped, sheet));
    click(harness.window, "forward-chat-0", cx);
    click(harness.window, "forward-send", cx);
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 2);
    let said = cx
        .update(|cx| harness.shell.read(cx).problem.clone())
        .unwrap();
    assert!(
        said.contains("Photo") && said.contains("view-once"),
        "{said}"
    );

    // One the provider refuses at run time fails alone; the next goes.
    harness.mock.refuse_forward_of(
        &MessageId::new("k3"),
        "media_expired",
        "WhatsApp no longer has this file.",
    );
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    let account = ids(&harness, cx).0;
    let chat = pending[0].message.chat_id.clone();
    let bubbles = forwarded_bubbles(&harness, &chat, &account);
    assert_eq!(bubbles.len(), 2);
    assert!(bubbles
        .iter()
        .any(|bubble| bubble.status == DeliveryStatus::Sent));
    assert!(bubbles.iter().any(|bubble| matches!(
        &bubble.status,
        DeliveryStatus::Failed { reason } if reason.contains("no longer has this file")
    )));
}

#[gpui_kit::test]
fn without_forwarding_by_name_only_a_text_can_be_forwarded_and_the_menu_says_why(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| prepare(cx, None));
    let mock = provider_mock::MockProvider::quiet();
    mock.set_forward_available(false);
    let harness = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    harness.settle(cx);
    arrive(&harness, cx, "n1", 1, MessageContent::text("hello"), false);
    arrive(
        &harness,
        cx,
        "n2",
        2,
        media(MediaKind::Image, "https://wa/i", None),
        false,
    );

    // The photo: Forward is there, dimmed, with the reason.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    press(harness.window, "m", cx);
    let items = cx.update(|cx| harness.shell.read(cx).message_menu_items());
    let forward = items
        .iter()
        .find(|item| item.id == "message-forward")
        .unwrap();
    assert!(forward.action.is_none());
    assert_eq!(forward.hint, Some("Not available yet from this provider"));
    // The whole label and the reason under it, inside the row.
    let row = bounds(harness.window, "message-forward", cx);
    let label = bounds(harness.window, "message-forward-label", cx);
    let reason = bounds(harness.window, "message-forward-reason", cx);
    assert!(
        within(label, row) && within(reason, row),
        "{label:?} {reason:?} {row:?}"
    );
    assert!(
        reason.top() >= label.bottom(),
        "the reason is on a second line"
    );
    press(harness.window, "escape", cx);
    // The key does nothing for it.
    press(harness.window, "f", cx);
    assert_ne!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::Forward
    );

    // The text goes as before.
    press(harness.window, "up", cx);
    press(harness.window, "f", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::Forward
    );
    click(harness.window, "forward-chat-0", cx);
    click(harness.window, "forward-send", cx);
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].message.forwarded);
    assert!(matches!(
        &pending[0].message.content,
        OutgoingContent::Text { body } if body == "hello"
    ));
}

// ----- menus: the label is whole, the reason under it ----------------------------------------------

/// A row of a menu: its label and its reason are inside it, the reason
/// under the label, and nothing sticks out of the card.
fn assert_whole(
    harness: &Harness,
    cx: &mut TestAppContext,
    card: &'static str,
    id: &str,
    what: &str,
) {
    let row = bounds_of(harness.window, id, cx);
    let label = bounds_of(harness.window, &format!("{id}-label"), cx);
    let reason = bounds_of(harness.window, &format!("{id}-reason"), cx);
    let card = bounds(harness.window, card, cx);
    assert!(
        within(label, row) && within(reason, row) && within(row, card),
        "{what}: {label:?} {reason:?} {row:?} {card:?}"
    );
    assert!(
        reason.top() >= label.bottom(),
        "{what}: the reason is under the label"
    );
    // One line each at these sizes: the label is not cut to half a word.
    let line = crate::theme::metrics::TEXT_BODY() * 2.2;
    assert!(label.size.height <= line, "{what}: {label:?}");
}

#[gpui_kit::test]
fn a_menu_row_keeps_its_whole_label_and_puts_the_reason_under_it_at_every_size(
    cx: &mut TestAppContext,
) {
    let harness = open(cx, ShellOptions::default());
    // Everything read: "Mark all as read" has nothing to do, and says so.
    click(harness.window, "list-menu", cx);
    click(harness.window, "menu-read-all", cx);
    harness.settle(cx);
    let account = cx.update(|cx| harness.shell.read(cx).accounts[0].id.clone());
    for step in crate::theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |s| s.interface_scale = step));
        cx.run_until_parked();

        // The chats' menu.
        click(harness.window, "list-menu", cx);
        assert_whole(
            &harness,
            cx,
            "menu",
            "menu-read-all",
            &format!("{step}% list menu"),
        );
        press(harness.window, "escape", cx);

        // The menu of a chat's row: every label whole, inside its row.
        let first = cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .list_rows
                .iter()
                .find_map(|row| match row {
                    crate::ui::shell::ListRow::Chat(chat) => Some(chat.id.clone()),
                    _ => None,
                })
                .unwrap()
        });
        let at = bounds_of(harness.window, &format!("chat-{first}"), cx);
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_event(gpui_kit::MouseDownEvent {
            position: gpui_kit::point(px(200.), at.center().y),
            modifiers: gpui_kit::Modifiers::none(),
            button: gpui_kit::MouseButton::Right,
            click_count: 1,
            first_mouse: false,
        });
        visual.run_until_parked();
        let items = cx.update(|cx| harness.shell.read(cx).menu_items_now());
        assert!(items.len() > 3);
        let card = bounds(harness.window, "menu", cx);
        for item in items {
            let row = bounds_of(harness.window, item.id, cx);
            let label = bounds_of(harness.window, &format!("{}-label", item.id), cx);
            assert!(
                within(label, row) && within(row, card),
                "{step}% row menu: {} {label:?} {row:?}",
                item.id
            );
        }
        press(harness.window, "escape", cx);

        // The menu of a number of the rail.
        cx.update_window(harness.window.into(), |_, window, cx| {
            harness.shell.update(cx, |shell, cx| {
                shell.open_rail_menu(
                    crate::ui::rail_menu::RailTarget::Account(account.clone()),
                    Default::default(),
                    window,
                    cx,
                );
            })
        })
        .unwrap();
        cx.run_until_parked();
        assert_whole(
            &harness,
            cx,
            "rail-menu",
            "rail-read",
            &format!("{step}% rail menu"),
        );
        press(harness.window, "escape", cx);
    }
    cx.update(|cx| settings::update(cx, |s| s.interface_scale = 100));
}

#[gpui_kit::test]
fn the_message_menu_and_the_row_menu_keep_every_label_whole_at_every_size(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let mock = provider_mock::MockProvider::quiet();
    mock.set_forward_available(false);
    let harness = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    harness.settle(cx);
    arrive(
        &harness,
        cx,
        "m1",
        1,
        media(MediaKind::Image, "https://wa/i", None),
        false,
    );
    for step in crate::theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |s| s.interface_scale = step));
        cx.run_until_parked();
        focus_composer(&harness, cx);
        press(harness.window, "up", cx);
        press(harness.window, "m", cx);
        assert_whole(
            &harness,
            cx,
            "menu",
            "message-forward",
            &format!("{step}% message menu"),
        );
        // Every entry of it: its label is inside its row, whole.
        let items = cx.update(|cx| harness.shell.read(cx).message_menu_items());
        let card = bounds(harness.window, "menu", cx);
        for item in items {
            let row = bounds_of(harness.window, item.id, cx);
            let label = bounds_of(harness.window, &format!("{}-label", item.id), cx);
            assert!(
                within(label, row) && within(row, card),
                "{step}%: {} {label:?} {row:?}",
                item.id
            );
        }
        press(harness.window, "escape", cx);
    }
    cx.update(|cx| settings::update(cx, |s| s.interface_scale = 100));
}

// ----- what the wuapi adapter says it can do --------------------------------------------------

/// The capabilities the wuapi adapter reports, read from the adapter
/// itself (making one touches no network), on the mock provider: the
/// window as it is over wuapi, without a network.
fn mock_with_what_wuapi_can_do() -> provider_mock::MockProvider {
    use client_provider::Provider as _;
    let wuapi = provider_wuapi::WuapiProvider::new(
        provider_wuapi::WuapiConfig::new("wuapi-inbox-tests"),
        provider_wuapi::ApiKey::new("wu_test_not_a_key"),
    )
    .unwrap();
    provider_mock::MockProvider::new(provider_mock::MockConfig {
        capabilities: Some(wuapi.capabilities()),
        ..provider_mock::MockConfig::quiet()
    })
}

fn forward_item(harness: &Harness, cx: &mut TestAppContext) -> (bool, Option<&'static str>) {
    press(harness.window, "m", cx);
    let items = cx.update(|cx| harness.shell.read(cx).message_menu_items());
    let forward = items
        .iter()
        .find(|item| item.id == "message-forward")
        .expect("Forward is in the menu");
    let state = (forward.action.is_some(), forward.hint);
    press(harness.window, "escape", cx);
    state
}

/// Under wuapi a picture is forwarded: by naming it, with no file moving.
/// A poll is not something WhatsApp passes on through it, and says so.
#[gpui_kit::test]
fn under_wuapi_a_picture_is_forwarded_by_naming_it_and_a_poll_says_why_not(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| prepare(cx, None));
    let mock = mock_with_what_wuapi_can_do();
    let harness = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    harness.settle(cx);
    let caps = harness.engine.capabilities();
    assert!(caps.forwards && caps.forward_any);
    arrive(
        &harness,
        cx,
        "n1",
        1,
        media(MediaKind::Image, "https://wa/i", Some("the lake")),
        false,
    );
    arrive(
        &harness,
        cx,
        "n2",
        2,
        MessageContent::Poll(client_provider::Poll {
            question: "Lunch?".into(),
            options: vec![client_provider::PollOption {
                name: "yes".into(),
                votes: 1,
            }],
            max_choices: 1,
            voters: 1,
            chosen: None,
        }),
        false,
    );

    // The poll, the newest: Forward is there, off, with the reason.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    assert_eq!(
        forward_item(&harness, cx),
        (false, Some("This kind of message cannot be forwarded"))
    );
    // The photo above it: Forward is on.
    press(harness.window, "up", cx);
    assert_eq!(forward_item(&harness, cx), (true, None));
    press(harness.window, "f", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::Forward
    );
    click(harness.window, "forward-chat-0", cx);
    click(harness.window, "forward-send", cx);
    // Queued as the naming of the original: no file was read or copied.
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].message.forwarded);
    assert!(matches!(
        &pending[0].message.content,
        OutgoingContent::Forward { source } if source.as_str() == "n1"
    ));
    harness.runtime.block_on(async {
        harness.engine.flush_outbox().await.unwrap();
    });
    assert_eq!(harness.mock.forwarded_copies(), 1);
    assert_eq!(harness.mock.upload_calls(), 0);
    assert!(harness.mock.sent().is_empty());
}

/// The backend behind the provider does not have the forward route yet:
/// the menu says "not available yet" for a picture, a text still goes,
/// and when the route is there the picture goes too, in the same session.
#[gpui_kit::test]
fn forwarding_that_is_not_deployed_yet_says_so_in_the_menu_and_comes_back(cx: &mut TestAppContext) {
    use client_provider::Feature;
    cx.update(|cx| prepare(cx, None));
    let mock = mock_with_what_wuapi_can_do();
    mock.set_unavailable(None, Feature::ForwardAny, true);
    let harness = open_over(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
        mock,
    );
    harness.settle(cx);
    arrive(&harness, cx, "n1", 1, MessageContent::text("hello"), false);
    arrive(
        &harness,
        cx,
        "n2",
        2,
        media(MediaKind::Image, "https://wa/i", None),
        false,
    );
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    assert_eq!(
        forward_item(&harness, cx),
        (false, Some("Not available yet from this provider"))
    );
    // The text above it is forwarded as a text always could be.
    press(harness.window, "up", cx);
    assert_eq!(forward_item(&harness, cx), (true, None));

    // The route arrives (the backend was deployed): no restart needed.
    harness
        .mock
        .set_unavailable(None, Feature::ForwardAny, false);
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    assert_eq!(forward_item(&harness, cx), (true, None));
}

/// WhatsApp's "Forwarded many times" is said on the bubble, where
/// "Forwarded" is.
#[gpui_kit::test]
fn a_message_forwarded_many_times_says_so_on_its_bubble(cx: &mut TestAppContext) {
    let harness = open_chat(cx);
    let (account, chat) = ids(&harness, cx);
    let message = |id: &str, at: i64, many: bool| {
        let mut message = Message {
            id: MessageId::new(id),
            client_id: None,
            account_id: account.clone(),
            chat_id: chat.clone(),
            sender: ContactId::new("them"),
            sender_name: None,
            direction: Direction::Incoming,
            timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 120_000 + at),
            content: MessageContent::text("pass it on"),
            reply_to: None,
            status: DeliveryStatus::Delivered,
            edited: false,
            deleted: false,
            extras: Default::default(),
        };
        message.extras.forwarded = true;
        message.extras.forwarded_many = many;
        message
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message("once", 1, false)))
        .unwrap();
    cx.run_until_parked();
    assert!(shows(harness.window, "forwarded-label", cx));
    assert!(!shows(harness.window, "forwarded-many", cx));
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message("many", 2, true)))
        .unwrap();
    cx.run_until_parked();
    assert!(shows(harness.window, "forwarded-many", cx));
}
