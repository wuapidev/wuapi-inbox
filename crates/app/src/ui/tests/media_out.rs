//! Pictures and files leaving the application: a picture copied to the
//! clipboard, a file saved where the user said. Always the original, as
//! the provider has it, fetched first when only a thumbnail is here.

use super::*;
use crate::clipboard::tests::FakeClipboard;
use crate::ui::media_out::MediaOut;
use client_provider::MediaKind;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// What a test hands the shell in place of the system's clipboard and
/// dialogs, and what they were asked.
pub(super) struct Outside {
    clipboard: Arc<FakeClipboard>,
    /// Where the next "Save as…" answers; `None` dismisses the dialog.
    answer: Rc<RefCell<Option<PathBuf>>>,
    /// The folder and the name each "Save as…" was opened with.
    asked: Rc<RefCell<Vec<(PathBuf, String)>>>,
    /// The folder "Save N items" answers.
    folder: Rc<RefCell<Option<PathBuf>>>,
    downloads: tempfile::TempDir,
    chosen: tempfile::TempDir,
    settings: tempfile::TempDir,
}

pub(super) fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, 40, (y % 256) as u8])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(
            &mut std::io::Cursor::new(&mut out),
            image::ImageFormat::Jpeg,
        )
        .unwrap();
    out
}

pub(super) fn open_with_outside(cx: &mut TestAppContext) -> (Harness, Outside) {
    let outside = Outside {
        clipboard: Arc::new(FakeClipboard::default()),
        answer: Rc::default(),
        asked: Rc::default(),
        folder: Rc::default(),
        downloads: tempfile::tempdir().unwrap(),
        chosen: tempfile::tempdir().unwrap(),
        settings: tempfile::tempdir().unwrap(),
    };
    let (answer, asked, folder) = (
        outside.answer.clone(),
        outside.asked.clone(),
        outside.folder.clone(),
    );
    let out = MediaOut {
        clipboard: outside.clipboard.clone(),
        save_as: Rc::new(move |folder, name, _| {
            asked
                .borrow_mut()
                .push((folder.to_owned(), name.to_owned()));
            gpui_kit::Task::ready(answer.borrow().clone())
        }),
        pick_folder: Rc::new(move |_| gpui_kit::Task::ready(folder.borrow().clone())),
        downloads: outside.downloads.path().to_owned(),
    };
    let file = outside.settings.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file)));
    let harness = open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            out: Some(out),
            ..Default::default()
        },
    );
    (harness, outside)
}

/// A picture in the open chat whose thumbnail is here and whose original
/// is the provider's alone.
pub(super) fn picture(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &'static str,
    original: &[u8],
    mime: &str,
) -> String {
    let url = format!("https://media.example/{id}");
    harness.mock.set_media(&url, original.to_vec(), mime);
    push_media(harness, cx, id, MediaKind::Image, Some(&url), false);
    cache_thumbnail(harness, cx, &url, (120, 90));
    url
}

fn notice(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .leaving
            .notice
            .as_ref()
            .map(|notice| notice.text.to_string())
    })
}

fn problem(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .problem
            .clone()
            .map(|p| p.to_string())
    })
}

/// Lets the download, and the work off the interface's thread after it,
/// finish: until `done`.
pub(super) fn wait(
    harness: &Harness,
    cx: &mut TestAppContext,
    what: &str,
    done: impl Fn() -> bool,
) {
    for _ in 0..400 {
        harness.settle(cx);
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("never: {what}");
}

fn focus_newest(harness: &Harness, cx: &mut TestAppContext) {
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness
            .shell
            .update(cx, |shell, cx| shell.focus_newest(window, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

// ----- the clipboard ------------------------------------------------------------

#[gpui_kit::test]
fn copy_image_fetches_the_original_and_offers_it_as_png_and_as_it_came(cx: &mut TestAppContext) {
    let (harness, outside) = open_with_outside(cx);
    let original = jpeg(640, 480);
    picture(&harness, cx, "holiday", &original, "image/jpeg");
    let downloads = harness.mock.media_calls();
    focus_newest(&harness, cx);

    // Only the thumbnail is here: the original is fetched first, and the
    // window says so. Nothing is copied meanwhile.
    press(harness.window, "ctrl-shift-c", cx);
    assert!(notice(&harness, cx)
        .unwrap()
        .contains("Downloading the original"));
    assert!(outside.clipboard.copied.lock().unwrap().is_empty());
    wait(&harness, cx, "the picture is copied", || {
        !outside.clipboard.copied.lock().unwrap().is_empty()
    });
    assert_eq!(harness.mock.media_calls(), downloads + 1);
    {
        let copied = outside.clipboard.copied.lock().unwrap();
        assert_eq!(copied.len(), 1);
        // The original, untouched, beside a PNG made from it at its size.
        assert_eq!(copied[0].types(), ["image/png", "image/jpeg"]);
        assert_eq!(
            copied[0].original.as_ref().unwrap().1.as_slice(),
            original.as_slice()
        );
        let as_png =
            image::load_from_memory_with_format(&copied[0].png, image::ImageFormat::Png).unwrap();
        assert_eq!(
            (as_png.width(), as_png.height()),
            (640, 480),
            "not the thumbnail"
        );
    }
    assert_eq!(notice(&harness, cx).as_deref(), Some("Image copied"));
    assert!(shows(harness.window, "notice", cx));

    // Now that it is here, a copy is at once; plain Ctrl+C on a picture
    // with nothing written under it copies the picture too.
    press(harness.window, "ctrl-c", cx);
    wait(&harness, cx, "copied again", || {
        outside.clipboard.copied.lock().unwrap().len() == 2
    });
    assert_eq!(
        harness.mock.media_calls(),
        downloads + 1,
        "no second download"
    );

    // A PNG is offered as it is.
    let exact = png(64, 48);
    picture(&harness, cx, "diagram", &exact, "image/png");
    focus_newest(&harness, cx);
    press(harness.window, "ctrl-shift-c", cx);
    wait(&harness, cx, "the PNG is copied", || {
        outside.clipboard.copied.lock().unwrap().len() == 3
    });
    {
        let copied = outside.clipboard.copied.lock().unwrap();
        assert_eq!(copied[2].types(), ["image/png"]);
        assert_eq!(copied[2].png.as_slice(), exact.as_slice());
    }

    // No clipboard: said in words, nothing pretended.
    *outside.clipboard.refuse.lock().unwrap() = Some("no clipboard was found".into());
    press(harness.window, "ctrl-shift-c", cx);
    for _ in 0..400 {
        harness.settle(cx);
        if problem(&harness, cx).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let said = problem(&harness, cx).expect("the failure is said");
    assert!(
        said.contains("could not be copied") && said.contains("no clipboard"),
        "{said}"
    );
    assert_eq!(notice(&harness, cx), None);
}

#[gpui_kit::test]
fn a_file_that_cannot_be_fetched_is_not_copied_and_says_why(cx: &mut TestAppContext) {
    let (harness, outside) = open_with_outside(cx);
    // The provider has no such file.
    let url = "https://media.example/gone";
    push_media(&harness, cx, "gone", MediaKind::Image, Some(url), false);
    cache_thumbnail(&harness, cx, url, (120, 90));
    focus_newest(&harness, cx);
    press(harness.window, "ctrl-shift-c", cx);
    for _ in 0..400 {
        harness.settle(cx);
        if problem(&harness, cx).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Whatever the provider answered, it was not the picture: that is
    // said, and the thumbnail is not copied in its place.
    let said = problem(&harness, cx).expect("the failure is said");
    assert!(
        said.contains("could not be downloaded") || said.contains("could not be copied"),
        "{said}"
    );
    assert!(
        outside.clipboard.copied.lock().unwrap().is_empty(),
        "no thumbnail instead"
    );
}

// ----- saving ---------------------------------------------------------------------

#[gpui_kit::test]
fn save_as_writes_the_providers_bytes_where_the_dialog_said(cx: &mut TestAppContext) {
    let (harness, outside) = open_with_outside(cx);
    let original = jpeg(320, 240);
    picture(&harness, cx, "photo", &original, "image/jpeg");
    let exports_before = std::fs::read_dir(crate::ui::media::exports_dir())
        .map(|dir| dir.count())
        .unwrap_or(0);
    focus_newest(&harness, cx);

    // Ctrl+S: the dialog opens in Downloads with a name made of the chat
    // and the time, and the extension of what the file is.
    let target = outside.chosen.path().join("kept.jpg");
    *outside.answer.borrow_mut() = Some(target.clone());
    press(harness.window, "ctrl-s", cx);
    {
        let asked = outside.asked.borrow();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].0, outside.downloads.path());
        let chat = cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .open
                .as_ref()
                .unwrap()
                .chat
                .title
                .clone()
        });
        assert!(asked[0].1.starts_with(chat.as_str()), "{}", asked[0].1);
        assert!(asked[0].1.ends_with(".jpg"), "{}", asked[0].1);
    }
    wait(&harness, cx, "the file is written", || target.exists());
    // The provider's bytes, exactly: fetched, not re-encoded.
    assert_eq!(std::fs::read(&target).unwrap(), original);
    assert!(notice(&harness, cx).unwrap().contains("kept.jpg"));
    // Nothing else was written anywhere: not beside it, not in the
    // folder files are opened from.
    assert_eq!(std::fs::read_dir(outside.chosen.path()).unwrap().count(), 1);
    assert_eq!(
        std::fs::read_dir(outside.downloads.path()).unwrap().count(),
        0
    );
    let exports_after = std::fs::read_dir(crate::ui::media::exports_dir())
        .map(|dir| dir.count())
        .unwrap_or(0);
    assert_eq!(exports_after, exports_before, "no copy left elsewhere");

    // "Show in folder" leads to it.
    click(harness.window, "notice-reveal", cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).leaving.revealed.clone()),
        Some(target.clone())
    );

    // The next dialog starts in the folder that was used.
    *outside.answer.borrow_mut() = None;
    press(harness.window, "ctrl-s", cx);
    assert_eq!(outside.asked.borrow()[1].0, outside.chosen.path());
    // Dismissed: nothing is written.
    harness.settle(cx);
    assert_eq!(std::fs::read_dir(outside.chosen.path()).unwrap().count(), 1);
}

#[gpui_kit::test]
fn save_to_downloads_asks_nothing_and_never_overwrites(cx: &mut TestAppContext) {
    let (harness, outside) = open_with_outside(cx);
    // A document the provider still holds, with a name of its own.
    let url = "https://media.example/contract";
    let bytes = b"%PDF-1.7 the contract".to_vec();
    harness
        .mock
        .set_media(url, bytes.clone(), "application/pdf");
    push_described_media(&harness, cx, "doc", MediaKind::Document, url, |media| {
        media.file_name = Some("contract.pdf".into());
    });
    focus_newest(&harness, cx);
    let downloads = harness.mock.media_calls();

    press(harness.window, "ctrl-shift-s", cx);
    let first = outside.downloads.path().join("contract.pdf");
    wait(&harness, cx, "the document is saved", || first.exists());
    assert_eq!(std::fs::read(&first).unwrap(), bytes);
    assert!(outside.asked.borrow().is_empty(), "no dialog");
    assert_eq!(
        harness.mock.media_calls(),
        downloads + 1,
        "fetched on demand"
    );

    // Again: a name that is free, the first file untouched.
    std::fs::write(&first, b"mine now").unwrap();
    press(harness.window, "ctrl-shift-s", cx);
    let second = outside.downloads.path().join("contract (1).pdf");
    wait(&harness, cx, "saved under another name", || second.exists());
    assert_eq!(std::fs::read(&second).unwrap(), bytes);
    assert_eq!(std::fs::read(&first).unwrap(), b"mine now");
    assert_eq!(
        notice(&harness, cx).as_deref(),
        Some("Saved contract (1).pdf")
    );

    // The tile's own "Save as…" asks, with the file's name.
    *outside.answer.borrow_mut() = Some(outside.chosen.path().join("elsewhere.pdf"));
    click(harness.window, "media-save", cx);
    assert_eq!(outside.asked.borrow()[0].1, "contract.pdf");
    wait(&harness, cx, "saved through the tile", || {
        outside.chosen.path().join("elsewhere.pdf").exists()
    });
    // The message menu offers both.
    press(harness.window, "m", cx);
    let items = cx.update(|cx| harness.shell.read(cx).menu_items_now());
    for id in ["message-save", "message-save-downloads"] {
        let item = items.iter().find(|item| item.id == id).expect(id);
        assert!(item.action.is_some(), "{id}");
    }
    assert!(!items.iter().any(|item| item.id == "message-copy-image"));
}

#[gpui_kit::test]
fn several_files_are_saved_into_one_folder(cx: &mut TestAppContext) {
    let (harness, outside) = open_with_outside(cx);
    let (one, two) = (jpeg(40, 30), png(20, 10));
    picture(&harness, cx, "one", &one, "image/jpeg");
    picture(&harness, cx, "two", &two, "image/png");
    focus_newest(&harness, cx);
    press(harness.window, "shift-up", cx);
    assert!(shows(harness.window, "selection-save", cx));

    *outside.folder.borrow_mut() = Some(outside.chosen.path().to_owned());
    click(harness.window, "selection-save", cx);
    wait(&harness, cx, "both are saved", || {
        std::fs::read_dir(outside.chosen.path()).unwrap().count() == 2
    });
    let mut saved: Vec<Vec<u8>> = std::fs::read_dir(outside.chosen.path())
        .unwrap()
        .map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
        .collect();
    saved.sort_by_key(Vec::len);
    let mut wanted = vec![one, two];
    wanted.sort_by_key(Vec::len);
    assert_eq!(saved, wanted);
    assert!(
        !shows(harness.window, "selection-bar", cx),
        "the selection is over"
    );
}

// ----- the viewer and the buttons on a picture --------------------------------------

#[gpui_kit::test]
fn the_viewer_has_a_toolbar_walks_the_pictures_and_zooms(cx: &mut TestAppContext) {
    let (harness, outside) = open_with_outside(cx);
    let first = picture(&harness, cx, "first", &jpeg(200, 100), "image/jpeg");
    let second = picture(&harness, cx, "second", &png(100, 200), "image/png");
    let shown = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .viewing
                .as_ref()
                .and_then(|media| media.source.as_ref().map(|source| source.to_string()))
        })
    };
    click(harness.window, "media-image", cx);
    assert_eq!(shown(cx).as_deref(), Some(second.as_str()));

    // The toolbar: inside the viewer, centred, every tool inside it.
    let viewer = bounds(harness.window, "viewer", cx);
    let toolbar = bounds(harness.window, "viewer-toolbar", cx);
    assert!(within(toolbar, viewer));
    let tools = [
        "viewer-previous",
        "viewer-next",
        "viewer-zoom-out",
        "viewer-zoom-in",
        "viewer-fit",
        "viewer-actual",
        "viewer-copy",
        "viewer-save",
        "viewer-open",
        "viewer-close",
    ];
    let mut last_right = px(0.);
    for tool in tools {
        let button = bounds(harness.window, tool, cx);
        assert!(within(button, toolbar), "{tool}");
        assert_eq!(button.size.height, theme::metrics::CONTROL(), "{tool}");
        assert!(button.left() >= last_right, "{tool}: in a row");
        last_right = button.right();
    }
    let (left, right) = (
        bounds(harness.window, "viewer-previous", cx).left(),
        bounds(harness.window, "viewer-close", cx).right(),
    );
    assert!(
        ((left + right) / 2. - viewer.center().x).abs() <= px(2.),
        "centred"
    );
    // The picture is under the toolbar, not behind it.
    let picture_box = bounds(harness.window, "viewer-picture", cx);
    assert!(picture_box.top() >= bounds(harness.window, "viewer-previous", cx).bottom());

    // Left and Right walk the chat's pictures; the ends are the ends.
    press(harness.window, "left", cx);
    assert_eq!(shown(cx).as_deref(), Some(first.as_str()));
    press(harness.window, "right", cx);
    assert_eq!(shown(cx).as_deref(), Some(second.as_str()));
    press(harness.window, "right", cx);
    assert_eq!(shown(cx).as_deref(), Some(second.as_str()));
    click(harness.window, "viewer-previous", cx);
    assert_eq!(shown(cx).as_deref(), Some(first.as_str()));

    // "+" comes closer, "-" goes back, "0" fits; the toolbar stays put.
    let scale = |cx: &mut TestAppContext| {
        cx.update_window(harness.window.into(), |_, window, cx| {
            harness
                .shell
                .read(cx)
                .viewer_frame(window.viewport_size())
                .scale
        })
        .unwrap()
    };
    super::viewer::decoded(&harness, cx);
    let picture_box = bounds(harness.window, "viewer-picture", cx);
    let fitted = scale(cx).unwrap();
    type_text(harness.window, "+", cx);
    type_text(harness.window, "+", cx);
    assert!(scale(cx).unwrap() > fitted);
    let closer = bounds(harness.window, "viewer-picture", cx);
    assert!(closer.size.width > picture_box.size.width);
    assert_eq!(bounds(harness.window, "viewer-toolbar", cx), toolbar);
    press(harness.window, "-", cx);
    assert!(bounds(harness.window, "viewer-picture", cx).size.width < closer.size.width);
    press(harness.window, "0", cx);
    assert_eq!(scale(cx), Some(fitted));
    assert_eq!(bounds(harness.window, "viewer-picture", cx), picture_box);
    click(harness.window, "viewer-zoom-in", cx);
    assert!(scale(cx).unwrap() > fitted);
    click(harness.window, "viewer-fit", cx);
    assert_eq!(scale(cx), Some(fitted));
    click(harness.window, "viewer-actual", cx);
    assert_eq!(scale(cx), Some(1.));
    click(harness.window, "viewer-fit", cx);

    // Ctrl+C copies the picture shown; Ctrl+S asks where to save it.
    press(harness.window, "ctrl-c", cx);
    wait(&harness, cx, "copied from the viewer", || {
        !outside.clipboard.copied.lock().unwrap().is_empty()
    });
    assert_eq!(
        outside.clipboard.copied.lock().unwrap()[0].types(),
        ["image/png", "image/jpeg"]
    );
    *outside.answer.borrow_mut() = Some(outside.chosen.path().join("from-viewer.jpg"));
    click(harness.window, "viewer-save", cx);
    wait(&harness, cx, "saved from the viewer", || {
        outside.chosen.path().join("from-viewer.jpg").exists()
    });
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::Viewer));
    click(harness.window, "viewer-close", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
}

#[gpui_kit::test]
fn a_picture_has_its_buttons_in_its_corner_at_every_interface_size(cx: &mut TestAppContext) {
    let (harness, outside) = open_with_outside(cx);
    picture(&harness, cx, "corner", &jpeg(300, 200), "image/jpeg");
    for step in theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        cx.run_until_parked();
        let frame = bounds(harness.window, "media-box", cx);
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_mouse_move(frame.center(), None, Modifiers::none());
        visual.run_until_parked();
        cx.update_window(harness.window.into(), |_, window, _| window.refresh())
            .unwrap();
        cx.run_until_parked();
        let buttons = bounds(harness.window, "picture-buttons", cx);
        assert!(
            within(buttons, frame),
            "{step}%: {buttons:?} outside {frame:?}"
        );
        assert!(
            frame.right() - buttons.right() <= theme::px(8.),
            "{step}%: the right corner"
        );
        assert!(
            buttons.top() - frame.top() <= theme::px(8.),
            "{step}%: the top corner"
        );
        let mut last_right = px(0.);
        for name in ["picture-save", "picture-copy", "picture-open"] {
            let button = bounds(harness.window, name, cx);
            assert!(within(button, buttons), "{step}%: {name}");
            assert_eq!(button.size.width, button.size.height, "{step}%: {name}");
            assert!(button.left() >= last_right, "{step}%: {name}");
            last_right = button.right();
        }
    }
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
    cx.run_until_parked();

    // The copy button copies; the picture is not opened by that click.
    let hover = |cx: &mut TestAppContext| {
        let frame = bounds(harness.window, "media-box", cx);
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_mouse_move(frame.center(), None, Modifiers::none());
        visual.run_until_parked();
        cx.update_window(harness.window.into(), |_, window, _| window.refresh())
            .unwrap();
        cx.run_until_parked();
    };
    // Away from the picture they are not there at all.
    let thread = bounds(harness.window, "thread", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_move(thread.origin, None, Modifiers::none());
    visual.run_until_parked();
    cx.update_window(harness.window.into(), |_, window, _| window.refresh())
        .unwrap();
    cx.run_until_parked();
    assert!(!shows(harness.window, "picture-copy", cx));
    hover(cx);
    let at = bounds(harness.window, "picture-copy", cx).center();
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_move(at, None, Modifiers::none());
    visual.simulate_click(at, Modifiers::none());
    visual.run_until_parked();
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
    wait(&harness, cx, "copied from the corner", || {
        !outside.clipboard.copied.lock().unwrap().is_empty()
    });
    // The open button opens the viewer.
    hover(cx);
    let at = bounds(harness.window, "picture-open", cx).center();
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_move(at, None, Modifiers::none());
    visual.simulate_click(at, Modifiers::none());
    visual.run_until_parked();
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::Viewer));
}
