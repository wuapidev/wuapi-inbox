//! The attach sheet as a carousel: one caption for each file, with its own
//! mentions, caret and undo history; switching, removing, adding and
//! reordering files; what is sent, and in which order; and that the sheet
//! fits at every interface size with many files.

use super::*;
use crate::ui::message_actions::Compose;
use client_provider::{ChatId, MediaKind};
use provider_mock::SHOWCASE_CHAT;
use std::path::PathBuf;

/// `count` pictures, each of a different colour so they differ.
fn pictures(dir: &std::path::Path, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|n| {
            let path = dir.join(format!("photo{n}.png"));
            image::RgbaImage::from_pixel(
                160 + n as u32,
                90,
                image::Rgba([(n * 20 % 255) as u8, 120, 200, 255]),
            )
            .save(&path)
            .unwrap();
            path
        })
        .collect()
}

/// The group chat of the demo (so there are people to mention) open, and a
/// file dialog that answers with `files`.
fn group_for_attaching(cx: &mut TestAppContext, files: Vec<PathBuf>) -> Harness {
    let harness = open(
        cx,
        ShellOptions {
            pick_files: Some(std::rc::Rc::new(move |_, _| {
                gpui_kit::Task::ready(Some(files.clone()))
            })),
            ..Default::default()
        },
    );
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_chat(cx);
            shell.open_chat(ChatId::new(SHOWCASE_CHAT), None, cx);
        })
    });
    harness.settle(cx);
    harness
}

fn open_sheet(harness: &Harness, cx: &mut TestAppContext, count: usize) {
    click(harness.window, "attach", cx);
    until(harness, cx, sheet_open);
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert_eq!(draft.items.len(), count);
    });
    cx.run_until_parked();
}

fn selected(harness: &Harness, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| harness.shell.read(cx).attach.as_ref().unwrap().selected)
}

/// The text of the caption of file `index`.
fn caption_of(harness: &Harness, index: usize, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        draft.items[index]
            .caption
            .as_ref()
            .expect("it has a field")
            .read(cx)
            .value()
            .to_string()
    })
}

fn mentioned(harness: &Harness, index: usize, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        draft.items[index].mentioning.picked.len()
    })
}

fn noah(harness: &Harness) -> client_provider::ContactId {
    let (account, chat) = (
        client_provider::AccountId::new(provider_mock::SHOWCASE_ACCOUNT),
        ChatId::new(SHOWCASE_CHAT),
    );
    harness
        .engine
        .store()
        .group_participants(&account, &chat, None, 50)
        .unwrap()
        .into_iter()
        .find(|person| person.name == "Noah Fischer")
        .expect("Noah is in the group")
        .contact
}

/// Types `text` and picks the first person the `@` offers.
fn mention_noah(harness: &Harness, text: &str, cx: &mut TestAppContext) {
    type_text(harness.window, text, cx);
    assert!(shows(harness.window, "mention-picker", cx), "{text}");
    press(harness.window, "enter", cx);
}

/// What a queued file says: its kind, caption, mentions and reply.
type Queued = (
    MediaKind,
    Option<String>,
    Vec<client_provider::ContactId>,
    Option<client_provider::MessageId>,
);

fn queued(harness: &Harness) -> Vec<Queued> {
    harness
        .engine
        .store()
        .outbox_pending()
        .unwrap()
        .into_iter()
        .map(|entry| {
            let OutgoingContent::Media { kind, caption, .. } = entry.message.content else {
                panic!("a file");
            };
            (
                kind,
                caption,
                entry.message.mentions.into_iter().map(|m| m.id).collect(),
                entry.message.reply_to,
            )
        })
        .collect()
}

#[gpui_kit::test]
fn each_of_three_files_has_its_own_caption_and_mention_and_goes_as_its_own_message(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 3));
    let noah = noah(&harness);
    let handle = harness.engine.mention_handle(&noah);
    open_sheet(&harness, cx, 3);
    assert_eq!(selected(&harness, cx), 0);

    // The first file: a caption with a mention.
    mention_noah(&harness, "one @noa", cx);
    type_text(harness.window, "alpha", cx);
    assert_eq!(caption_of(&harness, 0, cx), "one @Noah Fischer alpha");

    // Ctrl+PageDown shows the next file with the caption still the
    // keyboard's: the first one's text did not come along.
    press(harness.window, "ctrl-pagedown", cx);
    assert_eq!(selected(&harness, cx), 1);
    assert_eq!(caption_of(&harness, 1, cx), "");
    type_text(harness.window, "two, nobody", cx);
    // The caret keys are the caption's: Left moves the caret, not the file.
    press(harness.window, "left", cx);
    assert_eq!(selected(&harness, cx), 1);
    press(harness.window, "ctrl-pagedown", cx);
    assert_eq!(selected(&harness, cx), 2);
    mention_noah(&harness, "three @noa", cx);

    // Back and forth: each caption, its mentions and its caret are kept.
    press(harness.window, "ctrl-pageup", cx);
    press(harness.window, "ctrl-pageup", cx);
    assert_eq!(selected(&harness, cx), 0);
    assert_eq!(caption_of(&harness, 0, cx), "one @Noah Fischer alpha");
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        let first = draft.items[0].caption.as_ref().unwrap().read(cx);
        assert_eq!(first.cursor(), first.value().len(), "the caret stayed");
        let second = draft.items[1].caption.as_ref().unwrap().read(cx);
        assert_eq!(
            second.cursor(),
            second.value().len() - 1,
            "the caret of the second is where Left put it"
        );
    });
    assert_eq!(
        [
            mentioned(&harness, 0, cx),
            mentioned(&harness, 1, cx),
            mentioned(&harness, 2, cx)
        ],
        [1, 0, 1]
    );
    // A click on a thumbnail shows that file, and its caption has the
    // keyboard.
    click(harness.window, "attach-item-1", cx);
    assert_eq!(selected(&harness, cx), 1);
    type_text(harness.window, "!", cx);
    assert_eq!(caption_of(&harness, 1, cx), "two, nobod!y");
    // Each file with something to say is marked.
    for mark in [
        "attach-caption-mark-0",
        "attach-caption-mark-1",
        "attach-caption-mark-2",
    ] {
        assert!(shows(harness.window, mark, cx), "{mark}");
    }
    // The composer under the sheet was not written in.
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.composer.read(cx).value().is_empty());
        assert!(shell.mentioning.picked.is_empty());
    });

    // Enter sends all, in the order of the strip.
    press(harness.window, "enter", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).attach.is_none()));
    until(&harness, cx, |shell| own_media(shell) == 3);
    let sent = queued(&harness);
    assert_eq!(sent.len(), 3);
    let said: Vec<_> = sent.iter().map(|file| file.1.clone()).collect();
    assert_eq!(
        said,
        [
            Some(format!("one @{handle} alpha")),
            Some("two, nobod!y".to_owned()),
            Some(format!("three @{handle} ")).map(|text| text.trim().to_owned()),
        ]
    );
    assert_eq!(sent[0].2, std::slice::from_ref(&noah));
    assert!(sent[1].2.is_empty());
    assert_eq!(sent[2].2, std::slice::from_ref(&noah));
    assert!(sent.iter().all(|file| file.3.is_none()), "no reply here");
    assert!(sent.iter().all(|file| file.0 == MediaKind::Image));

    // Through the outbox to the provider: three messages in order, each
    // with its own caption and mentions.
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 3);
    let delivered = harness.mock.sent();
    assert_eq!(delivered.len(), 3);
    for (message, expected) in delivered.iter().zip(&sent) {
        let OutgoingContent::Media { caption, .. } = &message.content else {
            panic!("a file");
        };
        assert_eq!(caption, &expected.1);
        let ids: Vec<_> = message.mentions.iter().map(|m| m.id.clone()).collect();
        assert_eq!(ids, expected.2);
    }
    let ids: std::collections::HashSet<_> = delivered
        .iter()
        .map(|message| message.client_id.clone())
        .collect();
    assert_eq!(ids.len(), 3, "three client message ids");
}

#[gpui_kit::test]
fn each_file_keeps_its_own_undo_history(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 2));
    open_sheet(&harness, cx, 2);
    type_text(harness.window, "first ", cx);
    press(harness.window, "space", cx);
    type_text(harness.window, "words", cx);
    press(harness.window, "ctrl-pagedown", cx);
    type_text(harness.window, "other", cx);
    press(harness.window, "ctrl-pageup", cx);
    let before = caption_of(&harness, 0, cx);
    assert!(before.starts_with("first"));
    // Undo in the first file takes back what was typed there, and leaves
    // the second one's text alone.
    press(harness.window, "ctrl-z", cx);
    let after = caption_of(&harness, 0, cx);
    assert_ne!(after, before, "the undo reached the first file's text");
    assert!(before.starts_with(&after) || after.is_empty());
    assert_eq!(caption_of(&harness, 1, cx), "other");
}

#[gpui_kit::test]
fn a_file_is_taken_out_of_the_middle_and_the_rest_keep_their_order_and_captions(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 3));
    open_sheet(&harness, cx, 3);
    type_text(harness.window, "A", cx);
    press(harness.window, "ctrl-pagedown", cx);
    type_text(harness.window, "B", cx);
    press(harness.window, "ctrl-pagedown", cx);
    type_text(harness.window, "C", cx);
    click(harness.window, "attach-remove-1", cx);
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        let names: Vec<_> = draft
            .items
            .iter()
            .map(|item| item.file.name.clone())
            .collect();
        assert_eq!(names, ["photo0.png", "photo2.png"]);
        assert_eq!(draft.selected, 1, "still the file that was on show");
    });
    assert_eq!(caption_of(&harness, 0, cx), "A");
    assert_eq!(caption_of(&harness, 1, cx), "C");
    assert!(!shows(harness.window, "attach-item-2", cx));
    // The keyboard is in the caption of the file on show.
    type_text(harness.window, "!", cx);
    assert_eq!(caption_of(&harness, 1, cx), "C!");

    // Taking out the file on show shows its neighbour.
    click(harness.window, "attach-remove-1", cx);
    assert_eq!(selected(&harness, cx), 0);
    type_text(harness.window, "?", cx);
    assert_eq!(caption_of(&harness, 0, cx), "A?");
    press(harness.window, "enter", cx);
    until(&harness, cx, |shell| own_media(shell) == 1);
    let sent = queued(&harness);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1.as_deref(), Some("A?"));
}

#[gpui_kit::test]
fn files_move_earlier_and_later_and_go_in_the_new_order(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 3));
    open_sheet(&harness, cx, 3);
    type_text(harness.window, "zero", cx);
    press(harness.window, "ctrl-shift-pagedown", cx);
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        let names: Vec<_> = draft
            .items
            .iter()
            .map(|item| item.file.name.clone())
            .collect();
        assert_eq!(names, ["photo1.png", "photo0.png", "photo2.png"]);
        assert_eq!(draft.selected, 1, "the file moved, and is still on show");
    });
    assert_eq!(
        caption_of(&harness, 1, cx),
        "zero",
        "its caption went with it"
    );
    // At the end it stays.
    press(harness.window, "ctrl-shift-pagedown", cx);
    press(harness.window, "ctrl-shift-pagedown", cx);
    assert_eq!(selected(&harness, cx), 2);
    press(harness.window, "ctrl-shift-pageup", cx);
    assert_eq!(selected(&harness, cx), 1);
    press(harness.window, "enter", cx);
    until(&harness, cx, |shell| own_media(shell) == 3);
    let said: Vec<_> = queued(&harness).into_iter().map(|file| file.1).collect();
    assert_eq!(said, [None, Some("zero".to_owned()), None]);
}

#[gpui_kit::test]
fn the_strip_has_the_keyboard_when_tab_gets_there_and_the_arrows_walk_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 3));
    open_sheet(&harness, cx, 3);
    // Caption, then the strip, then the buttons.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            assert!(shell.caption_has_keyboard(window, cx));
        });
    })
    .unwrap();
    // (The caption's emoji button is part of the caption.)
    press(harness.window, "tab", cx);
    press(harness.window, "tab", cx);
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            assert!(!shell.caption_has_keyboard(window, cx));
            assert!(
                shell.attach_strip_focus.is_focused(window),
                "the strip is next"
            );
        });
    })
    .unwrap();
    // The arrows are the strip's now, and keep the keyboard there.
    press(harness.window, "right", cx);
    assert_eq!(selected(&harness, cx), 1);
    press(harness.window, "right", cx);
    press(harness.window, "right", cx);
    assert_eq!(selected(&harness, cx), 2, "the end stays the end");
    press(harness.window, "left", cx);
    assert_eq!(selected(&harness, cx), 1);
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, _| {
            assert!(shell.attach_strip_focus.is_focused(window));
        });
    })
    .unwrap();
    // Delete takes the file on show out.
    press(harness.window, "delete", cx);
    cx.update(|cx| {
        assert_eq!(
            harness.shell.read(cx).attach.as_ref().unwrap().items.len(),
            2
        )
    });
    // Enter, with the keyboard on the strip, sends all.
    cx.update_window(harness.window.into(), |_, window, cx| {
        let strip = harness.shell.read(cx).attach_strip_focus.clone();
        strip.focus(window, cx);
    })
    .unwrap();
    press(harness.window, "enter", cx);
    until(&harness, cx, |shell| own_media(shell) == 2);
}

#[gpui_kit::test]
fn a_file_pasted_while_the_sheet_is_open_joins_it(cx: &mut TestAppContext) {
    use gpui_kit::{ClipboardEntry, ClipboardItem, ExternalPaths};
    let dir = tempfile::tempdir().unwrap();
    let files = pictures(dir.path(), 3);
    let harness = group_for_attaching(cx, files[..2].to_vec());
    open_sheet(&harness, cx, 2);
    type_text(harness.window, "kept", cx);
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
                vec![files[2].clone()].into(),
            ))],
        })
    });
    press(harness.window, "ctrl-v", cx);
    until(&harness, cx, |shell| {
        shell
            .attach
            .as_ref()
            .is_some_and(|draft| draft.items.len() == 3)
    });
    // The new file is on show, appended, with a caption of its own; the
    // caption of the first is as it was.
    assert_eq!(selected(&harness, cx), 2);
    assert_eq!(caption_of(&harness, 0, cx), "kept");
    assert!(shows(harness.window, "attach-item-2", cx));
    type_text(harness.window, "new", cx);
    assert_eq!(caption_of(&harness, 2, cx), "new");
    assert_eq!(caption_of(&harness, 0, cx), "kept");

    // "Add more files" (its button and its key) appends the dialog's
    // files too.
    click(harness.window, "attach-add", cx);
    until(&harness, cx, |shell| {
        shell
            .attach
            .as_ref()
            .is_some_and(|draft| draft.items.len() == 5)
    });
    assert_eq!(selected(&harness, cx), 3);
    press(harness.window, "ctrl-o", cx);
    until(&harness, cx, |shell| {
        shell
            .attach
            .as_ref()
            .is_some_and(|draft| draft.items.len() == 7)
    });
}

#[gpui_kit::test]
fn files_dropped_on_the_open_sheet_join_it(cx: &mut TestAppContext) {
    use gpui_kit::{ExternalPaths, FileDropEvent};
    let dir = tempfile::tempdir().unwrap();
    let files = pictures(dir.path(), 3);
    let harness = group_for_attaching(cx, files[..1].to_vec());
    open_sheet(&harness, cx, 1);
    let over = bounds(harness.window, "attach-sheet", cx).center();
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_event(FileDropEvent::Entered {
        position: over,
        paths: ExternalPaths(files[1..].to_vec().into()),
    });
    visual.simulate_event(FileDropEvent::Pending { position: over });
    visual.simulate_event(FileDropEvent::Submit { position: over });
    visual.run_until_parked();
    until(&harness, cx, |shell| {
        shell
            .attach
            .as_ref()
            .is_some_and(|draft| draft.items.len() == 3)
    });
}

#[gpui_kit::test]
fn a_file_over_the_limit_holds_back_only_itself(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let mut files = pictures(dir.path(), 3);
    // The middle one is a large document.
    let big = dir.path().join("big.pdf");
    std::fs::write(&big, [b"%PDF-1.7 ".as_slice(), &[7; 5000]].concat()).unwrap();
    files.insert(1, big);
    let harness = group_for_attaching(cx, files);
    harness.mock.set_upload_limit(3000);
    open_sheet(&harness, cx, 4);
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        let blocked: Vec<_> = draft
            .items
            .iter()
            .map(|item| item.blocked.is_some())
            .collect();
        assert_eq!(blocked, [false, true, false, false]);
    });
    type_text(harness.window, "fine", cx);
    click(harness.window, "attach-item-1", cx);
    // Said on the file itself, readably; nothing to caption.
    assert!(shows(harness.window, "attach-blocked", cx));
    assert!(!shows(harness.window, "attach-caption", cx));
    assert!(shows(harness.window, "attach-blocked-mark-1", cx));
    assert!(!shows(harness.window, "attach-blocked-mark-0", cx));
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        let why = draft.items[1].blocked.as_ref().unwrap();
        assert!(
            why.contains("Too large") && why.contains("most that can be sent"),
            "{why}"
        );
    });
    // The total counts what goes.
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert_eq!(draft.items.len(), 4);
    });
    // The Left and Right of the strip work from here, and Enter sends the
    // other three.
    click(harness.window, "attach-send", cx);
    until(&harness, cx, |shell| own_media(shell) == 3);
    let sent = queued(&harness);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[0].1.as_deref(), Some("fine"));
    assert_eq!(harness.mock.upload_calls(), 0);
    cx.update(|cx| {
        let said = harness.shell.read(cx).problem.clone().expect("it was said");
        assert!(said.contains("too large"), "{said}");
    });
}

#[gpui_kit::test]
fn the_reply_goes_with_the_first_file_only_and_the_sheet_says_so(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 3));
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
                    Row::Message(row) if !row.stored.message.deleted => {
                        Some(row.stored.message.clone())
                    }
                    _ => None,
                })
                .expect("a message to answer");
            shell.acting.compose = Compose::Reply(Box::new(message.clone()));
            message.id
        })
    });
    open_sheet(&harness, cx, 3);
    assert!(shows(harness.window, "compose-reply", cx));
    assert!(shows(harness.window, "attach-reply-note", cx));
    // On whichever file is on show.
    press(harness.window, "ctrl-pagedown", cx);
    assert!(shows(harness.window, "attach-reply-note", cx));
    press(harness.window, "enter", cx);
    until(&harness, cx, |shell| own_media(shell) == 3);
    let sent = queued(&harness);
    assert_eq!(sent[0].3.as_ref(), Some(&answered));
    assert_eq!(sent[1].3, None);
    assert_eq!(sent[2].3, None);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).acting.compose, Compose::New));
    // And so it reaches the provider.
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    let delivered = harness.mock.sent();
    assert_eq!(delivered[0].reply_to.as_ref(), Some(&answered));
    assert!(delivered[1].reply_to.is_none() && delivered[2].reply_to.is_none());
}

#[gpui_kit::test]
fn how_a_file_is_sent_is_chosen_for_each_and_can_be_applied_to_all(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 3));
    open_sheet(&harness, cx, 3);
    // Only the second one as a document.
    press(harness.window, "ctrl-pagedown", cx);
    click(harness.window, "attach-as-document", cx);
    press(harness.window, "enter", cx);
    until(&harness, cx, |shell| own_media(shell) == 3);
    let kinds: Vec<_> = queued(&harness).into_iter().map(|file| file.0).collect();
    assert_eq!(
        kinds,
        [MediaKind::Image, MediaKind::Document, MediaKind::Image]
    );

    // "Apply to all" copies the choice of the file on show.
    open_sheet(&harness, cx, 3);
    click(harness.window, "attach-as-document", cx);
    click(harness.window, "attach-apply-all", cx);
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert!(draft.items.iter().all(|item| item.as_document));
    });
    click(harness.window, "attach-as-document", cx);
    click(harness.window, "attach-apply-all", cx);
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert!(draft.items.iter().all(|item| !item.as_document));
    });
}

#[gpui_kit::test]
fn leaving_asks_when_there_is_something_to_lose_and_escape_closes_lists_first(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let files = pictures(dir.path(), 2);
    let harness = group_for_attaching(cx, files[..1].to_vec());

    // One file, nothing written: Escape just closes.
    open_sheet(&harness, cx, 1);
    press(harness.window, "escape", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).attach.is_none()));

    // A caption with text: it asks, and a click on "Keep editing" goes on
    // where it was.
    open_sheet(&harness, cx, 1);
    type_text(harness.window, "an @noa", cx);
    // The list of people closes first; the sheet is still asking nothing.
    assert!(shows(harness.window, "mention-picker", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "mention-picker", cx));
    assert!(!shows(harness.window, "attach-discard-confirm", cx));
    // Then the question.
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "attach-discard-confirm", cx));
    let sheet = bounds(harness.window, "attach-sheet", cx);
    assert!(within(
        bounds(harness.window, "attach-discard-confirm", cx),
        sheet
    ));
    // Enter does not send while it is asked: it goes on with the files.
    press(harness.window, "enter", cx);
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
    assert!(!shows(harness.window, "attach-discard-confirm", cx));
    cx.update(|cx| assert!(harness.shell.read(cx).attach.is_some()));
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "attach-discard-confirm", cx));
    click(harness.window, "attach-keep", cx);
    assert!(!shows(harness.window, "attach-discard-confirm", cx));
    assert_eq!(caption_of(&harness, 0, cx), "an @noa");
    // The caption has the keyboard back.
    type_text(harness.window, "!", cx);
    assert_eq!(caption_of(&harness, 0, cx), "an @noa!");
    // Cancel asks as well; Discard throws everything away.
    click(harness.window, "attach-cancel", cx);
    assert!(shows(harness.window, "attach-discard-confirm", cx));
    click(harness.window, "attach-discard", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.attach.is_none());
        assert_eq!(shell.overlay, Overlay::None);
    });
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());

    // Several files, nothing written: it asks too; Escape again means it.
    let harness = group_for_attaching(cx, files);
    open_sheet(&harness, cx, 2);
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "attach-discard-confirm", cx));
    press(harness.window, "escape", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).attach.is_none()));
    assert!(harness.engine.store().outbox_pending().unwrap().is_empty());
}

#[gpui_kit::test]
fn the_emoji_picker_gives_the_sheet_back_on_the_same_file_and_caret(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 3));
    open_sheet(&harness, cx, 3);
    type_text(harness.window, "zero", cx);
    press(harness.window, "ctrl-pagedown", cx);
    type_text(harness.window, "one two", cx);
    // The caret in the middle.
    for _ in 0..4 {
        press(harness.window, "left", cx);
    }
    let caret = cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        draft.items[1].caption.as_ref().unwrap().read(cx).cursor()
    });
    assert_eq!(caret, 3);

    press(harness.window, "ctrl-e", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::EmojiPicker
    );
    assert!(
        !shows(harness.window, "attach-sheet", cx),
        "one overlay at a time"
    );
    press(harness.window, "escape", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::AttachSheet
    );
    assert_eq!(selected(&harness, cx), 1);
    let again = cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        draft.items[1].caption.as_ref().unwrap().read(cx).cursor()
    });
    assert_eq!(again, caret, "the caret did not move");
    // And the caption has the keyboard: what is typed lands at the caret.
    type_text(harness.window, "_", cx);
    assert_eq!(caption_of(&harness, 1, cx), "one_ two");
    assert_eq!(caption_of(&harness, 0, cx), "zero");

    // A pick lands in the caption of that file, at the caret.
    click(harness.window, "caption-emoji", cx);
    type_text(harness.window, "pizza", cx);
    cx.run_until_parked();
    cx.executor()
        .advance_clock(crate::motion::SEARCH_DEBOUNCE + std::time::Duration::from_millis(10));
    cx.run_until_parked();
    press(harness.window, "enter", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).overlay),
        Overlay::AttachSheet
    );
    assert_eq!(selected(&harness, cx), 1);
    assert_eq!(caption_of(&harness, 1, cx), "one_🍕 two");
    assert_eq!(caption_of(&harness, 0, cx), "zero");
}

#[gpui_kit::test]
fn the_sheet_fits_at_every_interface_size_with_ten_files_and_the_strip_scrolls(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 10));
    // The tallest the sheet gets: a reply under way, a long caption.
    cx.update(|cx| {
        harness.shell.update(cx, |shell, _| {
            let message = shell
                .open
                .as_ref()
                .unwrap()
                .rows
                .iter()
                .rev()
                .find_map(|row| match row {
                    Row::Message(row) if !row.stored.message.deleted => {
                        Some(row.stored.message.clone())
                    }
                    _ => None,
                })
                .expect("a message to answer");
            shell.acting.compose = Compose::Reply(Box::new(message));
        })
    });
    open_sheet(&harness, cx, 10);
    type_text(harness.window, "a caption", cx);
    for _ in 0..3 {
        press(harness.window, "shift-enter", cx);
        type_text(harness.window, "with several lines", cx);
    }
    let window = bounds(harness.window, "overlay", cx);
    for step in theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        cx.run_until_parked();
        // On the first and on the last file.
        for last in [false, true] {
            // By key: a thumbnail scrolled out of the strip is not there
            // to be clicked.
            for _ in 0..9 {
                press(
                    harness.window,
                    if last { "ctrl-pagedown" } else { "ctrl-pageup" },
                    cx,
                );
            }
            cx.run_until_parked();
            cx.run_until_parked();
            let sheet = bounds(harness.window, "attach-sheet", cx);
            let what = format!("{step}%, last: {last}");
            assert!(within(sheet, window), "{what}: the sheet is in the window");
            for part in [
                "attach-preview",
                "attach-file-info",
                "attach-strip",
                "attach-options",
                "attach-total",
                "attach-send",
                "attach-cancel",
                "attach-reply-note",
                "compose-reply",
            ] {
                assert!(
                    within(bounds_of(harness.window, part, cx), sheet),
                    "{what}: {part} is inside the sheet"
                );
            }
            if !last {
                // The caption belongs to the first file, which has text.
                assert!(
                    within(bounds_of(harness.window, "attach-caption", cx), sheet),
                    "{what}: the caption is inside the sheet"
                );
            }
            // The caption stays under the preview and above the strip.
            let preview = bounds_of(harness.window, "attach-preview", cx);
            let strip = bounds_of(harness.window, "attach-strip", cx);
            assert!(
                preview.bottom() <= strip.top(),
                "{what}: preview above strip"
            );
            assert!(preview.size.height >= px(60.), "{what}: {preview:?}");
            if shows(harness.window, "attach-caption", cx) {
                let caption = bounds_of(harness.window, "attach-caption", cx);
                assert!(preview.bottom() <= caption.top(), "{what}: caption below");
                assert!(
                    caption.bottom() <= strip.top(),
                    "{what}: caption above strip"
                );
            }
            // The strip scrolls: ten files are wider than it. The one on
            // show is in it, whole.
            let on_show = bounds_of(
                harness.window,
                &format!("attach-item-{}", if last { 9 } else { 0 }),
                cx,
            );
            assert!(
                on_show.left() >= strip.left() - px(1.)
                    && on_show.right() <= strip.right() + px(1.),
                "{what}: the file on show is in view: {on_show:?} in {strip:?}"
            );
            let first = bounds_of(harness.window, "attach-item-0", cx);
            let tenth = bounds_of(harness.window, "attach-item-9", cx);
            assert!(
                tenth.right() - first.left() > strip.size.width,
                "{what}: the strip is longer than what it shows"
            );
        }
    }
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
}

// ----- the sheet and the sticker and GIF picker ---------------------------------------------

#[gpui_kit::test]
fn stickers_and_gifs_wait_while_files_are_being_attached(cx: &mut TestAppContext) {
    use crate::ui::picker::Tab;
    let dir = tempfile::tempdir().unwrap();
    let harness = group_for_attaching(cx, pictures(dir.path(), 3));
    harness
        .engine
        .library_save_sticker(
            client_core::fixtures::still_webp(40, 40),
            "image/webp",
            client_core::LibrarySource::Received,
            false,
        )
        .unwrap();
    open_sheet(&harness, cx, 3);
    type_text(harness.window, "zero", cx);
    let overlay = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).overlay);

    // The shortcuts of the Stickers and GIFs tabs do nothing here.
    for keys in ["ctrl-shift-e", "ctrl-shift-j"] {
        press(harness.window, keys, cx);
        cx.run_until_parked();
        assert_eq!(overlay(cx), Overlay::AttachSheet, "{keys}");
    }

    // The caption's picker is emoji only: no tabs to change to, and the
    // other tabs' shortcuts neither change it nor close it.
    press(harness.window, "ctrl-e", cx);
    cx.run_until_parked();
    assert_eq!(overlay(cx), Overlay::EmojiPicker);
    assert!(!shows(harness.window, "picker-tabs", cx));
    for keys in ["ctrl-shift-e", "ctrl-shift-j"] {
        press(harness.window, keys, cx);
        cx.run_until_parked();
        assert_eq!(overlay(cx), Overlay::EmojiPicker, "{keys}");
        assert_eq!(
            cx.update(|cx| harness.shell.read(cx).picker.tab),
            Tab::Emoji,
            "{keys}"
        );
    }

    // Even asked directly, a sticker is not sent from under the sheet.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.picker.tab = Tab::Stickers;
            shell.picker_load(cx);
            assert!(!shell.picker.grid().tiles.is_empty());
            shell.picker_send(0, false, window, cx);
            shell.picker.tab = Tab::Emoji;
        })
    })
    .unwrap();
    for _ in 0..20 {
        harness.settle(cx);
        cx.run_until_parked();
    }
    assert!(queued(&harness).is_empty(), "nothing was sent");
    assert_eq!(overlay(cx), Overlay::EmojiPicker);

    // The sheet comes back as it was.
    press(harness.window, "escape", cx);
    assert_eq!(overlay(cx), Overlay::AttachSheet);
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert_eq!(draft.items.len(), 3);
    });
    assert_eq!(caption_of(&harness, 0, cx), "zero");
}

#[gpui_kit::test]
fn a_gif_from_disk_goes_through_the_sheet_with_its_own_caption(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let gif = dir.path().join("loop.gif");
    std::fs::write(&gif, client_core::fixtures::animated_gif(48, 32, 3, 80)).unwrap();
    let mut files = vec![gif];
    files.extend(pictures(dir.path(), 1));
    let harness = group_for_attaching(cx, files);
    open_sheet(&harness, cx, 2);
    type_text(harness.window, "a loop", cx);
    press(harness.window, "ctrl-pagedown", cx);
    type_text(harness.window, "a photo", cx);
    press(harness.window, "enter", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).attach.is_none()));
    until(&harness, cx, |shell| own_media(shell) == 2);

    let pending = harness.engine.store().outbox_pending().unwrap();
    let said: Vec<_> = pending
        .iter()
        .map(|entry| match &entry.message.content {
            OutgoingContent::Media {
                kind, caption, gif, ..
            } => (*kind, caption.clone(), *gif),
            _ => panic!("a file"),
        })
        .collect();
    assert_eq!(
        said,
        [
            (MediaKind::Image, Some("a loop".to_owned()), false),
            (MediaKind::Image, Some("a photo".to_owned()), false),
        ]
    );
    // An attachment is not kept in the sticker and GIF library.
    let store = harness.engine.store();
    assert!(store
        .library_items(client_core::LibraryKind::Gif)
        .unwrap()
        .is_empty());
}

fn names(harness: &Harness, cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        let mut names: Vec<String> = harness
            .shell
            .read(cx)
            .attach
            .as_ref()
            .map(|draft| draft.items.iter().map(|i| i.file.name.clone()).collect())
            .unwrap_or_default();
        names.sort();
        names
    })
}

#[gpui_kit::test]
fn files_that_arrive_while_others_are_still_being_read_all_reach_the_sheet(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let files = pictures(dir.path(), 7);
    let harness = group_for_attaching(cx, files[5..].to_vec());
    // A drop, and another before the first is read.
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.attach_paths(files[..2].to_vec(), cx);
            shell.attach_paths(files[2..5].to_vec(), cx);
        })
    });
    until(&harness, cx, |shell| {
        shell
            .attach
            .as_ref()
            .is_some_and(|draft| draft.items.len() == 5)
    });
    assert_eq!(
        names(&harness, cx),
        [
            "photo0.png",
            "photo1.png",
            "photo2.png",
            "photo3.png",
            "photo4.png"
        ]
    );

    // The file dialog answering, and a paste right behind it: both.
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.pick_attachments(cx);
            shell.attach_paths(files[..1].to_vec(), cx);
        })
    });
    until(&harness, cx, |shell| {
        shell
            .attach
            .as_ref()
            .is_some_and(|draft| draft.items.len() == 8)
    });
    let all = names(&harness, cx);
    assert!(all.contains(&"photo5.png".to_owned()) && all.contains(&"photo6.png".to_owned()));
}

#[gpui_kit::test]
fn a_file_refused_as_it_is_sent_is_said_even_when_more_files_are_attached_at_once(
    cx: &mut TestAppContext,
) {
    let dir = tempfile::tempdir().unwrap();
    let files = pictures(dir.path(), 2);
    let harness = group_for_attaching(cx, files[..1].to_vec());
    open_sheet(&harness, cx, 1);
    // The limit came down after the file was put in the sheet.
    harness.mock.set_upload_limit(10);
    let more = files[1..].to_vec();
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.send_attachments(window, cx);
            shell.attach_paths(more, cx);
        })
    })
    .unwrap();
    until(&harness, cx, |shell| {
        shell
            .problem
            .as_ref()
            .is_some_and(|said| said.contains("was not sent"))
    });
    assert!(queued(&harness).is_empty());
    // And the files attached meanwhile are in a sheet of their own.
    until(&harness, cx, |shell| {
        shell
            .attach
            .as_ref()
            .is_some_and(|draft| draft.items.len() == 1)
    });
}
