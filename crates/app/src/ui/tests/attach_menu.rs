//! The composer's "+": files, the camera, a poll and an event, each from
//! its entry, and what each one sends.

use super::*;
use crate::ui::camera_ui::{CameraFeed, CameraSource, Cameras};
use client_provider::{CallPicture, CallScreen};
use std::cell::Cell;
use std::rc::Rc;

/// A camera that shows one red picture, and counts how often it is open.
struct FakeCamera {
    opened: Cell<usize>,
    open_now: Rc<Cell<usize>>,
    works: bool,
}

/// Counts down when the camera is let go.
struct Holding(Rc<Cell<usize>>);

impl Drop for Holding {
    fn drop(&mut self) {
        self.0.set(self.0.get() - 1);
    }
}

impl CameraSource for FakeCamera {
    fn available(&self) -> bool {
        true
    }

    fn open(&self, _camera: Option<String>) -> Result<CameraFeed, String> {
        if !self.works {
            return Err("no camera".into());
        }
        self.opened.set(self.opened.get() + 1);
        self.open_now.set(self.open_now.get() + 1);
        let screen = CallScreen::default();
        screen.show(Some(Arc::new(CallPicture {
            width: 32,
            height: 24,
            bgra: [0u8, 0, 255, 255].repeat(32 * 24),
        })));
        Ok(CameraFeed {
            screen,
            problem: Box::new(|| None),
            _open: Box::new(Holding(self.open_now.clone())),
        })
    }
}

fn open_with_camera(cx: &mut TestAppContext, works: bool) -> (Harness, Rc<FakeCamera>) {
    let camera = Rc::new(FakeCamera {
        opened: Cell::new(0),
        open_now: Rc::new(Cell::new(0)),
        works,
    });
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            cameras: Some(Cameras(camera.clone())),
            ..Default::default()
        },
    );
    harness.settle(cx);
    (harness, camera)
}

fn overlay_of(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

fn newest_row(harness: &Harness, cx: &mut TestAppContext) -> client_provider::Message {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        match shell.open.as_ref().unwrap().rows.last() {
            Some(Row::Message(row)) => row.stored.message.clone(),
            _ => panic!("a message is the newest row"),
        }
    })
}

#[gpui_kit::test]
fn the_plus_offers_files_the_camera_a_poll_and_an_event_over_itself(cx: &mut TestAppContext) {
    let (harness, _) = open_with_camera(cx, true);
    let plus = bounds(harness.window, "attach", cx);
    click(harness.window, "attach", cx);
    assert_eq!(overlay_of(&harness, cx), Overlay::AttachMenu);
    let menu = bounds(harness.window, "menu", cx);
    // Above the button, from its left.
    assert!(menu.bottom() <= plus.top(), "{menu:?} {plus:?}");
    for entry in [
        "attach-menu-files",
        "attach-menu-camera",
        "attach-menu-poll",
        "attach-menu-event",
    ] {
        assert!(within(bounds(harness.window, entry, cx), menu), "{entry}");
    }
    // A second click on the "+" closes it.
    click(harness.window, "attach", cx);
    assert_eq!(overlay_of(&harness, cx), Overlay::None);

    // The poll is there too, not only in the chat's menu.
    click(harness.window, "attach", cx);
    click(harness.window, "attach-menu-poll", cx);
    assert!(shows(harness.window, "poll-form", cx));
}

#[gpui_kit::test]
fn files_come_from_the_dialog_through_the_plus(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (harness, _, _) = open_for_attaching(cx, dir.path());
    click(harness.window, "attach", cx);
    click(harness.window, "attach-menu-files", cx);
    until(&harness, cx, sheet_open);
    assert!(shows(harness.window, "attach-sheet", cx));
}

#[gpui_kit::test]
fn an_event_is_written_in_its_form_and_sent_through_the_outbox(cx: &mut TestAppContext) {
    let (harness, _) = open_with_camera(cx, true);
    click(harness.window, "attach", cx);
    click(harness.window, "attach-menu-event", cx);
    assert!(shows(harness.window, "event-form", cx));
    let form = bounds(harness.window, "event-form", cx);
    for part in [
        "event-form-name",
        "event-form-start-date",
        "event-form-start-time",
        "event-form-end-time",
        "event-form-place",
        "event-form-send",
    ] {
        assert!(within(bounds(harness.window, part, cx), form), "{part}");
    }

    // Without a name it says so and stays open.
    click(harness.window, "event-form-send", cx);
    assert!(shows(harness.window, "event-form-error", cx));
    assert_eq!(harness.engine.store().outbox_pending().unwrap().len(), 0);

    // The keyboard is in the name; the start is the next whole hour.
    type_text(harness.window, "Dinner", cx);
    assert!(!shows(harness.window, "event-form-error", cx));
    let place = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .event_form
            .as_ref()
            .unwrap()
            .place
            .clone()
    });
    cx.update_window(harness.window.into(), |_, window, cx| {
        place.update(cx, |field, cx| field.set_value("Home", window, cx));
    })
    .unwrap();
    click(harness.window, "event-form-send", cx);
    assert!(!shows(harness.window, "event-form", cx));

    // The bubble is there before the network.
    let pending = newest_row(&harness, cx);
    assert_eq!(pending.status, DeliveryStatus::Pending);
    let MessageContent::Event(event) = &pending.content else {
        panic!("an event, got {:?}", pending.content)
    };
    assert_eq!(event.title, "Dinner");
    assert_eq!(
        event.place.as_ref().and_then(|place| place.name.as_deref()),
        Some("Home")
    );
    let starts = event.starts_at.unwrap().as_millis();
    let now = client_provider::Timestamp::now().as_millis();
    assert!(starts > now && starts <= now + 3_600_000, "{starts} {now}");
    assert_eq!(starts % 60_000, 0, "on a whole minute");
    assert!(shows(harness.window, "event-tile", cx));

    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 1);
    assert_eq!(harness.mock.send_calls(), 1);
}

#[gpui_kit::test]
fn an_event_that_ends_before_it_starts_is_not_sent(cx: &mut TestAppContext) {
    let (harness, _) = open_with_camera(cx, true);
    click(harness.window, "attach", cx);
    click(harness.window, "attach-menu-event", cx);
    type_text(harness.window, "Dinner", cx);
    let (start_time, end_time) = cx.update(|cx| {
        let form = harness.shell.read(cx).event_form.as_ref().unwrap();
        (form.start_time.clone(), form.end_time.clone())
    });
    // Ending when it starts is ending before it.
    let start = cx.update(|cx| start_time.read(cx).value().to_string());
    cx.update_window(harness.window.into(), |_, window, cx| {
        end_time.update(cx, |field, cx| field.set_value(start, window, cx));
    })
    .unwrap();
    click(harness.window, "event-form-send", cx);
    assert!(shows(harness.window, "event-form-error", cx));
    assert_eq!(harness.engine.store().outbox_pending().unwrap().len(), 0);
    // Escape leaves it, sending nothing.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "event-form", cx));
    assert!(cx.update(|cx| harness.shell.read(cx).event_form.is_none()));
}

#[gpui_kit::test]
fn a_photo_is_taken_with_the_camera_and_goes_to_the_sheet(cx: &mut TestAppContext) {
    let (harness, camera) = open_with_camera(cx, true);
    click(harness.window, "attach", cx);
    click(harness.window, "attach-menu-camera", cx);
    assert!(shows(harness.window, "camera", cx));
    assert!(shows(harness.window, "camera-picture", cx));
    assert_eq!(camera.open_now.get(), 1);

    click(harness.window, "camera-shoot", cx);
    until(&harness, cx, sheet_open);
    // The camera is let go as soon as the picture is taken.
    assert_eq!(camera.open_now.get(), 0);
    assert!(!shows(harness.window, "camera", cx));
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert_eq!(draft.items.len(), 1);
        let file = &draft.items[0].file;
        assert_eq!(file.mime, "image/jpeg");
        assert!(file.name.starts_with("IMG-") && file.name.ends_with(".jpg"));
        let picture = image::load_from_memory(&file.bytes).unwrap();
        assert_eq!((picture.width(), picture.height()), (32, 24));
    });
}

#[gpui_kit::test]
fn a_camera_left_without_a_photo_is_let_go(cx: &mut TestAppContext) {
    let (harness, camera) = open_with_camera(cx, true);
    click(harness.window, "attach", cx);
    click(harness.window, "attach-menu-camera", cx);
    assert_eq!(camera.open_now.get(), 1);
    click(harness.window, "camera-cancel", cx);
    assert_eq!(camera.open_now.get(), 0);
    assert!(!shows(harness.window, "camera", cx));
    // Escape as well.
    click(harness.window, "attach", cx);
    click(harness.window, "attach-menu-camera", cx);
    press(harness.window, "escape", cx);
    assert_eq!(camera.open_now.get(), 0);
    assert_eq!(camera.opened.get(), 2);
    assert!(cx.update(|cx| harness.shell.read(cx).camera.is_none()));
}

#[gpui_kit::test]
fn a_camera_that_does_not_open_says_so(cx: &mut TestAppContext) {
    let (harness, _) = open_with_camera(cx, false);
    click(harness.window, "attach", cx);
    click(harness.window, "attach-menu-camera", cx);
    assert!(shows(harness.window, "camera-problem", cx));
    assert!(!shows(harness.window, "camera-picture", cx));
    // Nothing to take.
    click(harness.window, "camera-shoot", cx);
    assert!(shows(harness.window, "camera", cx));
}
