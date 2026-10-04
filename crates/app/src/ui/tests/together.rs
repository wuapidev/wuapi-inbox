//! Where message types, profiles and groups, and attachments meet: a name
//! in a conversation leads to that person's profile, and a form that is
//! being filled in is not swept away by a file dropped behind it.

use super::*;
use crate::ui::social::Target;
use client_provider::{
    ChatId, ContactId, Message, MessageExtras, MessageId, ProviderEvent, Timestamp,
};
use provider_mock::SHOWCASE_CHAT;

/// A message in the open chat from somebody with a name, newer than
/// everything else in it.
fn said_by(harness: &Harness, cx: &mut TestAppContext, sender: &str, name: &str) -> Message {
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let message = Message {
        id: MessageId::new(format!("from-{sender}")),
        client_id: None,
        account_id: chat.account_id.clone(),
        chat_id: chat.id.clone(),
        sender: ContactId::new(sender),
        sender_name: Some(name.to_owned()),
        direction: Direction::Incoming,
        timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000),
        content: MessageContent::text("See you all there"),
        reply_to: None,
        status: DeliveryStatus::Delivered,
        edited: false,
        deleted: false,
        extras: MessageExtras::default(),
    };
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(message.clone()))
        .unwrap();
    cx.run_until_parked();
    message
}

#[gpui_kit::test]
fn the_senders_name_in_a_group_opens_that_persons_profile(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_chat(ChatId::new(SHOWCASE_CHAT), None, cx)
        })
    });
    cx.run_until_parked();
    let message = said_by(&harness, cx, "+351910000042", "Zacarias Nunes");
    assert!(shows(harness.window, "sender-name", cx));
    // Whichever name on screen is clicked, the profile is its sender's.
    let senders: Vec<ContactId> = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .filter_map(|row| match row {
                Row::Message(row) if row.stored.message.direction == Direction::Incoming => {
                    Some(row.stored.message.sender.clone())
                }
                _ => None,
            })
            .collect()
    });
    assert!(senders.contains(&message.sender));
    click(harness.window, "sender-name", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::ContactInfo);
        match shell.social.target() {
            Some(Target::Contact { account, contact }) => {
                assert_eq!(account, &message.account_id);
                assert!(senders.contains(contact), "the profile of a sender");
            }
            other => panic!("a contact's profile, not {other:?}"),
        }
    });
    // Escape closes it and the conversation is still the one behind.
    press(harness.window, "escape", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert_eq!(
            shell.open.as_ref().unwrap().chat.id,
            ChatId::new(SHOWCASE_CHAT)
        );
    });
}

#[test]
fn a_mention_knows_whose_profile_it_opens() {
    use crate::markup::{parse, Block, Handle};
    // Two people nobody has a name for read alike; each stretch still
    // knows which of the message's mentions it is.
    let handles = [
        Handle {
            handle: "111".into(),
            name: Some("Ana".into()),
            me: false,
            who: 0,
        },
        Handle {
            handle: "222".into(),
            name: None,
            me: false,
            who: 1,
        },
        Handle {
            handle: "333".into(),
            name: None,
            me: false,
            who: 2,
        },
    ];
    let blocks = parse("@111, @333 and @222", &handles);
    let Block::Paragraph(line) = &blocks[0] else {
        panic!("a paragraph")
    };
    assert_eq!(line.text, "@Ana, @someone and @someone");
    let who: Vec<Option<usize>> = line
        .spans
        .iter()
        .map(|span| span.style.mention_of)
        .collect();
    assert_eq!(who, [Some(0), Some(2), Some(1)]);
}

#[gpui_kit::test]
fn a_file_dropped_behind_a_poll_being_written_does_not_take_its_place(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (harness, picture, _) = open_for_attaching(cx, dir.path());
    click(harness.window, "chat-menu", cx);
    click(harness.window, "menu-poll", cx);
    assert!(shows(harness.window, "poll-form", cx));
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.attach_paths(vec![picture.clone()], cx)
        })
    });
    // The file is read off this thread: give it the time it takes.
    for _ in 0..30 {
        harness.settle(cx);
        std::thread::sleep(Duration::from_millis(10));
    }
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::NewPoll, "the form stays");
        assert!(shell.attach.is_none(), "and no sheet waits behind it");
    });
    assert!(shows(harness.window, "poll-form", cx));
    // With the form closed, the same drop opens the sheet; Escape there
    // closes the sheet only.
    press(harness.window, "escape", cx);
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.attach_paths(vec![picture.clone()], cx)
        })
    });
    until(&harness, cx, sheet_open);
    assert!(shows(harness.window, "attach-sheet", cx));
    press(harness.window, "escape", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert!(shell.attach.is_none());
        assert!(shell.open.is_some(), "the conversation is still open");
    });
}
