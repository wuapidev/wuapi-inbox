//! The PDF reader, and the list of people behind an `@`: a PDF of a
//! message opens here, its pages drawn as they come into view and let go
//! when they leave; the arrows move in the list of people although the
//! text field has bindings of its own for them.

use super::media_out::open_with_outside;
use super::*;
use client_provider::{ChatId, MediaKind};
use provider_mock::SHOWCASE_CHAT;

fn overlay(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

/// Waits for something that is made off the interface's thread.
fn until(
    harness: &Harness,
    cx: &mut TestAppContext,
    what: &str,
    done: impl Fn(&mut TestAppContext) -> bool,
) {
    for _ in 0..400 {
        harness.settle(cx);
        if done(cx) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("never: {what}");
}

/// A document of the open chat that the provider still holds, in focus.
fn document(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &'static str,
    name: &str,
    mime: &str,
    bytes: Vec<u8>,
) {
    let url = format!("https://media.example/{id}");
    harness.mock.set_media(&url, bytes, mime);
    push_described_media(harness, cx, id, MediaKind::Document, &url, |media| {
        media.file_name = Some(name.to_owned());
        media.mime_type = Some(mime.to_owned());
    });
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness
            .shell
            .update(cx, |shell, cx| shell.focus_newest(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_pdf_is_read_here_page_by_page(cx: &mut TestAppContext) {
    let (harness, _outside) = open_with_outside(cx);
    let pages = 40;
    document(
        &harness,
        cx,
        "report",
        "Report 2026.pdf",
        "application/pdf",
        crate::pdf::tests::sample(pages, (300, 400)),
    );
    let reader = |cx: &mut TestAppContext, read: &dyn Fn(&crate::ui::reader::Reader) -> bool| {
        cx.update(|cx| read(&harness.shell.read(cx).reader))
    };

    // O opens what the message carries: a PDF, here. It is fetched, read
    // and its first pages drawn; the ones far down are not.
    press(harness.window, "o", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Reader);
    assert!(shows(harness.window, "reader-toolbar", cx));
    until(&harness, cx, "the PDF is read", |cx| {
        reader(cx, &|reader| reader.page_count() == Some(pages))
    });
    until(&harness, cx, "the first page is drawn", |cx| {
        reader(cx, &|reader| reader.pages_drawn().contains(&0))
    });
    assert!(reader(cx, &|reader| reader.pages_drawn().len() < 8));
    assert!(shows(harness.window, "reader-page-0", cx));

    // The pages are as wide as each other, centred, under the toolbar.
    let (toolbar, first) = (
        bounds(harness.window, "reader-toolbar", cx),
        bounds(harness.window, "reader-page-0", cx),
    );
    let window = bounds(harness.window, "reader", cx);
    assert!(first.top() >= toolbar.bottom());
    assert!(
        (first.center().x - window.center().x).abs() <= px(1.),
        "{first:?} in {window:?}"
    );
    assert!((first.size.height / first.size.width - 400. / 300.).abs() < 0.01);

    // Right goes to the next page, End to the last: the pages there are
    // drawn, and the first ones let go.
    press(harness.window, "right", cx);
    let second = bounds(harness.window, "reader-page-1", cx);
    assert!((second.top() - first.top()).abs() <= px(1.), "{second:?}");
    press(harness.window, "end", cx);
    until(&harness, cx, "the last page is drawn", |cx| {
        reader(cx, &|reader| reader.pages_drawn().contains(&(pages - 1)))
    });
    assert!(reader(cx, &|reader| !reader.pages_drawn().contains(&0)));
    assert!(reader(cx, &|reader| reader.pages_drawn().len() < 8));

    // "+" makes the pages larger; the fit button puts them back.
    press(harness.window, "home", cx);
    let fitted = bounds(harness.window, "reader-page-0", cx).size.width;
    click(harness.window, "reader-zoom-in", cx);
    let closer = bounds(harness.window, "reader-page-0", cx).size.width;
    assert!(
        (closer / fitted - 1.25).abs() < 0.01,
        "{closer:?} {fitted:?}"
    );
    click(harness.window, "reader-fit", cx);
    assert_eq!(
        bounds(harness.window, "reader-page-0", cx).size.width,
        fitted
    );

    // Escape closes it, and nothing of the file is held any more.
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    assert!(reader(cx, &|reader| reader.media.is_none()
        && reader.pages_drawn().is_empty()));
}

#[gpui_kit::test]
fn what_is_not_a_pdf_is_said_and_other_files_open_outside(cx: &mut TestAppContext) {
    let (harness, _outside) = open_with_outside(cx);
    // Named a PDF, and not one: the reader says so and offers the way out.
    document(
        &harness,
        cx,
        "broken",
        "broken.pdf",
        "application/pdf",
        b"not a pdf at all".to_vec(),
    );
    press(harness.window, "o", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Reader);
    until(&harness, cx, "it is said", |cx| {
        shows(harness.window, "reader-open-instead", cx)
    });
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);

    // A spreadsheet is the system's to open: no reader.
    document(
        &harness,
        cx,
        "sheet",
        "budget.xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        b"PK".to_vec(),
    );
    press(harness.window, "o", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

#[gpui_kit::test]
fn the_arrows_move_in_the_list_of_people_and_tab_picks(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_chat(cx);
            shell.open_chat(ChatId::new(SHOWCASE_CHAT), None, cx);
        })
    });
    harness.settle(cx);
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell
                .composer
                .update(cx, |composer, cx| composer.focus(window, cx))
        })
    })
    .unwrap();
    let cursor = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).mentions().cursor);
    let people = |cx: &mut TestAppContext| {
        cx.update(|cx| harness.shell.read(cx).mention_matches(cx))
            .into_iter()
            .map(|person| crate::format::phone(&person.name))
            .collect::<Vec<_>>()
    };
    let text = |cx: &mut TestAppContext| {
        cx.update(|cx| harness.shell.read(cx).composer.read(cx).value().to_string())
    };

    type_text(harness.window, "hi @", cx);
    assert!(shows(harness.window, "mention-picker", cx));
    let listed = people(cx);
    assert!(listed.len() >= 3, "{listed:?}");
    assert_eq!(cursor(cx), 0);

    // Down and Up are the list's, not the caret's; they go round.
    press(harness.window, "down", cx);
    assert_eq!(cursor(cx), 1);
    press(harness.window, "down", cx);
    assert_eq!(cursor(cx), 2);
    press(harness.window, "up", cx);
    assert_eq!(cursor(cx), 1);
    press(harness.window, "up", cx);
    press(harness.window, "up", cx);
    assert_eq!(cursor(cx), listed.len() - 1);
    press(harness.window, "down", cx);
    press(harness.window, "down", cx);
    assert_eq!(cursor(cx), 1);
    assert_eq!(text(cx), "hi @", "the text was not touched");

    // Tab picks the one the keyboard is on; nothing was indented.
    press(harness.window, "tab", cx);
    assert_eq!(text(cx), format!("hi @{} ", listed[1]));
    assert!(!shows(harness.window, "mention-picker", cx));

    // Enter picks too, and with no list the arrows are the field's again.
    type_text(harness.window, "@", cx);
    press(harness.window, "down", cx);
    press(harness.window, "enter", cx);
    assert_eq!(text(cx), format!("hi @{} @{} ", listed[1], listed[1]));
}

#[gpui_kit::test]
fn far_up_a_conversation_a_button_goes_back_to_the_newest_message(cx: &mut TestAppContext) {
    let (harness, _outside) = open_with_outside(cx);
    // Enough messages to scroll through.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell
                .composer
                .update(cx, |composer, cx| composer.focus(window, cx))
        })
    })
    .unwrap();
    for line in 0..60 {
        type_text(harness.window, &format!("line {line}"), cx);
        press(harness.window, "enter", cx);
    }
    harness.settle(cx);
    let away = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            let shell = harness.shell.read(cx);
            crate::ui::conversation::away_from_newest(&shell.open.as_ref().unwrap().list)
        })
    };
    let wheel = |cx: &mut TestAppContext, by: f32| {
        let over = bounds(harness.window, "thread", cx).center();
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_event(gpui_kit::ScrollWheelEvent {
            position: over,
            delta: gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), gpui_kit::px(by))),
            ..Default::default()
        });
        visual.run_until_parked();
    };
    // At the newest message, and a little above it: nothing is offered.
    assert!(!shows(harness.window, "jump-to-newest", cx));
    wheel(cx, 120.);
    assert!(away(cx) > 0.);
    assert!(!shows(harness.window, "jump-to-newest", cx));

    // Far up: the button is there, inside the conversation.
    wheel(cx, 900.);
    assert!(shows(harness.window, "jump-to-newest", cx));
    let (button, thread) = (
        bounds(harness.window, "jump-to-newest", cx),
        bounds(harness.window, "thread", cx),
    );
    assert!(button.right() <= thread.right() && button.bottom() <= thread.bottom());

    // A click is at the newest message at once, and the button is gone.
    click(harness.window, "jump-to-newest", cx);
    assert_eq!(away(cx), 0.);
    assert!(!shows(harness.window, "jump-to-newest", cx));
}
