//! The picture viewer: every picture opens whole, the percentage is the
//! real scale, and zooming and moving keep within their limits.
//!
//! What is drawn is compared within three quarters of a pixel: layout
//! snaps to the device's pixels.

use super::media_out::{jpeg, open_with_outside, picture};
use super::*;
use crate::ui::viewer::{self, Frame};
use gpui_kit::{point, MouseButton, MouseDownEvent, MouseUpEvent, ScrollDelta, ScrollWheelEvent};

/// The pictures: the owner's tall screenshot, the same on its side, a
/// large square, a tiny one, and a very tall strip.
const PICTURES: [(&str, u32, u32); 5] = [
    ("tall", 1080, 2340),
    ("wide", 2340, 1080),
    ("square", 4000, 4000),
    ("tiny", 64, 64),
    ("strip", 300, 3000),
];
const WINDOWS: [(f32, f32); 2] = [(1280., 800.), (800., 1200.)];

/// The same JPEG with an EXIF note saying how to turn it: an APP1
/// segment after the start marker with one entry, the orientation.
fn turned(jpeg: &[u8], orientation: u8) -> Vec<u8> {
    let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0".to_vec();
    exif.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, orientation, 0, 0, 0]);
    exif.extend_from_slice(&[0, 0, 0, 0]);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&exif);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// Waits until the original of the picture in the viewer is decoded.
pub(super) fn decoded(harness: &Harness, cx: &mut TestAppContext) {
    for _ in 0..400 {
        harness.settle(cx);
        if cx.update(|cx| harness.shell.read(cx).viewer_shown().is_some()) {
            return cx.run_until_parked();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the original was never decoded");
}

fn frame(harness: &Harness, cx: &mut TestAppContext) -> Frame {
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.read(cx).viewer_frame(window.viewport_size())
    })
    .unwrap()
}

fn resize(harness: &Harness, cx: &mut TestAppContext, (width, height): (f32, f32)) {
    let visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_resize(size(px(width), px(height)));
    visual.run_until_parked();
}

/// Opens the viewer on the picture at `url` and waits for its original.
fn view(harness: &Harness, cx: &mut TestAppContext, url: &str) {
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.close_overlay(window, cx);
            let media = shell
                .pictures()
                .into_iter()
                .find(|media| media.source.as_ref().is_some_and(|at| at.as_str() == url))
                .expect("the picture is in the chat");
            shell.begin_viewing(media, window, cx);
        })
    })
    .unwrap();
    decoded(harness, cx);
    cx.run_until_parked();
}

/// Drags the picture across its room, over and over: towards the bottom
/// right, or towards the top left. The pointer stays inside the window.
fn drag(harness: &Harness, cx: &mut TestAppContext, area: Bounds<gpui_kit::Pixels>, down: bool) {
    let inset = point(px(10.), px(10.));
    let (mut from, mut to) = (area.origin + inset, area.bottom_right() - inset);
    if !down {
        std::mem::swap(&mut from, &mut to);
    }
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    for _ in 0..12 {
        visual.simulate_mouse_move(from, None, Modifiers::none());
        visual.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    }
    visual.simulate_mouse_move(from, None, Modifiers::none());
    visual.run_until_parked();
}

fn label(harness: &Harness, cx: &mut TestAppContext) -> String {
    format!("{:.0}%", frame(harness, cx).scale.unwrap() * 100.)
}

fn close(a: f32, b: f32, tolerance: f32) -> bool {
    (a - b).abs() <= tolerance
}

#[gpui_kit::test]
fn every_picture_opens_whole_and_zooms_within_its_limits(cx: &mut TestAppContext) {
    let (harness, _outside) = open_with_outside(cx);
    let urls: Vec<String> = PICTURES
        .iter()
        .map(|(id, width, height)| picture(&harness, cx, id, &jpeg(*width, *height), "image/jpeg"))
        .collect();
    for window in WINDOWS {
        resize(&harness, cx, window);
        for ((id, width, height), url) in PICTURES.iter().zip(&urls) {
            let what = format!("{id} in {window:?}");
            let natural = (*width as f32, *height as f32);
            view(&harness, cx, url);
            let natural_seen =
                cx.update(|cx| harness.shell.read(cx).viewer_shown().unwrap().natural);
            assert_eq!(natural_seen, (*width, *height), "{what}");

            // On opening: whole, inside the area under the toolbar, in
            // its proportions, in the middle, not enlarged.
            let opened = frame(&harness, cx);
            let drawn = bounds(harness.window, "viewer-picture", cx);
            let area = bounds(harness.window, "viewer-area", cx);
            assert_eq!(area, opened.area, "{what}");
            assert!(
                area.top() >= bounds(harness.window, "viewer-toolbar", cx).bottom(),
                "{what}: under the toolbar"
            );
            assert!(within(area, bounds(harness.window, "viewer", cx)), "{what}");
            assert!(
                close(drawn.left().as_f32(), opened.rect.left().as_f32(), 0.75)
                    && close(
                        drawn.size.height.as_f32(),
                        opened.rect.size.height.as_f32(),
                        0.75
                    ),
                "{what}: drawn {drawn:?}, the state says {:?}",
                opened.rect
            );
            let (shown_w, shown_h) = (drawn.size.width.as_f32(), drawn.size.height.as_f32());
            assert!(
                drawn.left() >= area.left() - px(0.75)
                    && drawn.right() <= area.right() + px(0.75)
                    && drawn.top() >= area.top() - px(0.75)
                    && drawn.bottom() <= area.bottom() + px(0.75),
                "{what}: {drawn:?} not whole inside {area:?}"
            );
            assert!(
                close(shown_w / shown_h, natural.0 / natural.1, 0.01),
                "{what}: proportions"
            );
            assert!(
                close(drawn.center().x.as_f32(), area.center().x.as_f32(), 0.75)
                    && close(drawn.center().y.as_f32(), area.center().y.as_f32(), 0.75),
                "{what}: centred"
            );
            let scale = opened.scale.unwrap();
            assert!(scale <= 1., "{what}: enlarged to {scale}");
            assert!(close(scale, shown_w / natural.0, 2e-3), "{what}");
            // As large as it can be: it touches the area on one axis,
            // unless it is shown at its own size.
            let room = (area.size.width.as_f32(), area.size.height.as_f32());
            assert!(
                scale == 1. || close(shown_w, room.0, 0.75) || close(shown_h, room.1, 0.75),
                "{what}: smaller than it could be"
            );
            // The toolbar says that scale, of the picture's own pixels.
            assert_eq!(
                label(&harness, cx),
                format!("{:.0}%", opened.rect.size.width.as_f32() / natural.0 * 100.),
                "{what}"
            );

            // "-" goes below the fit, down to the least, and stops.
            let least = viewer::least(opened.fit);
            for _ in 0..12 {
                press(harness.window, "-", cx);
            }
            assert!(
                close(frame(&harness, cx).scale.unwrap(), least, 1e-5),
                "{what}"
            );
            assert!(least <= 0.10 && least <= opened.fit, "{what}");
            let smallest = bounds(harness.window, "viewer-picture", cx);
            assert!(
                close(smallest.size.width.as_f32(), natural.0 * least, 0.75),
                "{what}"
            );
            assert!(
                close(smallest.center().x.as_f32(), area.center().x.as_f32(), 0.75)
                    && close(smallest.center().y.as_f32(), area.center().y.as_f32(), 0.75),
                "{what}: a small picture stays in the middle"
            );
            // "+" goes up to 800 % and stops.
            for _ in 0..20 {
                type_text(harness.window, "+", cx);
            }
            assert_eq!(frame(&harness, cx).scale, Some(viewer::MOST), "{what}");
            assert_eq!(label(&harness, cx), "800%", "{what}");
            // "1" is one pixel of the picture on one of the window; "0"
            // is whole again, exactly as it opened.
            press(harness.window, "1", cx);
            assert_eq!(frame(&harness, cx).scale, Some(1.), "{what}");
            assert!(
                close(
                    bounds(harness.window, "viewer-picture", cx)
                        .size
                        .width
                        .as_f32(),
                    natural.0,
                    0.75
                ),
                "{what}"
            );
            press(harness.window, "0", cx);
            assert_eq!(frame(&harness, cx), opened, "{what}");

            // Moving: at 200 % the picture is larger than its room; it
            // goes with the pointer until its edge meets the area's, and
            // no further.
            click(harness.window, "viewer-actual", cx);
            for _ in 0..3 {
                click(harness.window, "viewer-zoom-in", cx);
            }
            assert_eq!(frame(&harness, cx).scale, Some(2.), "{what}");
            drag(&harness, cx, area, true);
            let moved = bounds(harness.window, "viewer-picture", cx);
            for (shown, room, edge, limit) in [
                (moved.size.width, area.size.width, moved.left(), area.left()),
                (moved.size.height, area.size.height, moved.top(), area.top()),
            ] {
                if shown > room {
                    assert!(
                        close(edge.as_f32(), limit.as_f32(), 0.75),
                        "{what}: clamped"
                    );
                }
            }
            if moved.size.width <= area.size.width {
                assert!(
                    close(moved.center().x.as_f32(), area.center().x.as_f32(), 0.75),
                    "{what}: centred where it is not larger"
                );
            }

            // The wheel zooms around the pointer: the point of the
            // picture under it stays under it.
            press(harness.window, "1", cx);
            press(harness.window, "0", cx);
            for _ in 0..2 {
                click(harness.window, "viewer-actual", cx);
                click(harness.window, "viewer-zoom-in", cx);
                click(harness.window, "viewer-zoom-in", cx);
                click(harness.window, "viewer-zoom-in", cx);
            }
            let before = bounds(harness.window, "viewer-picture", cx);
            let before_scale = frame(&harness, cx).scale.unwrap();
            let pointer = area.center() + point(px(60.), px(-40.));
            let under = |rect: Bounds<gpui_kit::Pixels>, scale: f32| {
                (
                    (pointer.x - rect.left()).as_f32() / scale,
                    (pointer.y - rect.top()).as_f32() / scale,
                )
            };
            let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
            visual.simulate_event(ScrollWheelEvent {
                position: pointer,
                delta: ScrollDelta::Lines(point(0., 1.)),
                ..Default::default()
            });
            visual.run_until_parked();
            let after = bounds(harness.window, "viewer-picture", cx);
            let after_scale = frame(&harness, cx).scale.unwrap();
            assert!(
                close(after_scale, before_scale * viewer::WHEEL, 1e-4),
                "{what}"
            );
            let (was, is) = (under(before, before_scale), under(after, after_scale));
            // Only on the axes where the picture is larger than its room
            // (elsewhere it stays in the middle).
            if before.size.width > area.size.width {
                assert!(close(was.0, is.0, 1.), "{what}: {was:?} → {is:?}");
            }
            if before.size.height > area.size.height {
                assert!(close(was.1, is.1, 1.), "{what}: {was:?} → {is:?}");
            }
            visual.simulate_event(ScrollWheelEvent {
                position: pointer,
                delta: ScrollDelta::Lines(point(0., -1.)),
                ..Default::default()
            });
            visual.run_until_parked();
            assert!(
                close(frame(&harness, cx).scale.unwrap(), before_scale, 1e-4),
                "{what}: the wheel towards the user goes back"
            );
            press(harness.window, "0", cx);
        }
    }
}

#[gpui_kit::test]
fn the_window_refits_a_fitted_picture_and_keeps_a_zoomed_one(cx: &mut TestAppContext) {
    let (harness, _outside) = open_with_outside(cx);
    let url = picture(&harness, cx, "tall", &jpeg(1080, 2340), "image/jpeg");
    resize(&harness, cx, WINDOWS[0]);
    view(&harness, cx, &url);
    let wide = frame(&harness, cx);
    // Fitted: another window, another fit, whole again.
    resize(&harness, cx, WINDOWS[1]);
    let tall = frame(&harness, cx);
    assert!(tall.scale.unwrap() > wide.scale.unwrap());
    let drawn = bounds(harness.window, "viewer-picture", cx);
    let area = bounds(harness.window, "viewer-area", cx);
    assert!(
        within(drawn, area) || drawn == area,
        "{drawn:?} in {area:?}"
    );
    assert!(close(
        drawn.size.height.as_f32(),
        area.size.height.as_f32().min(2340. * tall.scale.unwrap()),
        0.75
    ));
    // Zoomed and moved to a corner in the wide window: the scale stays
    // through a resize, and the picture is brought back within the new,
    // taller room.
    resize(&harness, cx, WINDOWS[0]);
    press(harness.window, "1", cx);
    let area = bounds(harness.window, "viewer-area", cx);
    drag(&harness, cx, area, false);
    let drawn = bounds(harness.window, "viewer-picture", cx);
    assert!(close(drawn.bottom().as_f32(), area.bottom().as_f32(), 0.75));
    // Narrower than this room: in the middle, however far it was dragged.
    assert!(close(
        drawn.center().x.as_f32(),
        area.center().x.as_f32(),
        0.75
    ));
    resize(&harness, cx, WINDOWS[1]);
    assert_eq!(frame(&harness, cx).scale, Some(1.));
    let drawn = bounds(harness.window, "viewer-picture", cx);
    let area = bounds(harness.window, "viewer-area", cx);
    // Its bottom edge is at the new room's, not above it.
    assert!(
        close(drawn.bottom().as_f32(), area.bottom().as_f32(), 0.75),
        "{drawn:?} in {area:?}"
    );
    // Wider than this room: it covers it from side to side.
    assert!(drawn.left() <= area.left() + px(0.75) && drawn.right() >= area.right() - px(0.75));
}

#[gpui_kit::test]
fn two_clicks_toggle_actual_size_and_the_next_picture_opens_whole(cx: &mut TestAppContext) {
    let (harness, _outside) = open_with_outside(cx);
    let first = picture(&harness, cx, "tall", &jpeg(1080, 2340), "image/jpeg");
    let second = picture(&harness, cx, "wide", &jpeg(2340, 1080), "image/jpeg");
    resize(&harness, cx, WINDOWS[0]);
    view(&harness, cx, &first);
    let opened = frame(&harness, cx);
    let at = opened.rect.center() + point(px(20.), px(90.));
    let twice = |cx: &mut TestAppContext| {
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        for count in [1, 2] {
            visual.simulate_event(MouseDownEvent {
                position: at,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
                click_count: count,
                first_mouse: false,
            });
            visual.simulate_event(MouseUpEvent {
                position: at,
                button: MouseButton::Left,
                modifiers: Modifiers::none(),
                click_count: count,
            });
        }
        visual.run_until_parked();
    };
    // Two clicks on the picture: actual size, with what was clicked
    // still under the pointer; the viewer stays open.
    let under = (at.y - opened.rect.top()).as_f32() / opened.scale.unwrap();
    twice(cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::Viewer));
    let actual = frame(&harness, cx);
    assert_eq!(actual.scale, Some(1.));
    assert!(close((at.y - actual.rect.top()).as_f32(), under, 0.5));
    // Two more: whole again.
    twice(cx);
    assert_eq!(frame(&harness, cx), opened);

    // Zoomed in, the next picture still opens whole; and so does this
    // one when coming back to it.
    press(harness.window, "1", cx);
    press(harness.window, "right", cx);
    decoded(&harness, cx);
    let next = frame(&harness, cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).viewer_shown().unwrap().natural),
        (2340, 1080)
    );
    assert!(close(next.scale.unwrap(), next.fit, 1e-6) && next.fit < 1.);
    assert!(close(
        next.rect.size.width.as_f32(),
        next.area.size.width.as_f32(),
        0.75
    ));
    press(harness.window, "left", cx);
    decoded(&harness, cx);
    assert_eq!(frame(&harness, cx), opened);
    let _ = second;
}

#[gpui_kit::test]
fn a_photo_taken_upright_is_shown_upright_and_the_thumbnail_stands_in(cx: &mut TestAppContext) {
    let (harness, _outside) = open_with_outside(cx);
    // Stored 1200x800 with a note to turn it a quarter: 800x1200 to look at.
    let photo = turned(&jpeg(1200, 800), 6);
    let url = picture(&harness, cx, "photo", &photo, "image/jpeg");
    resize(&harness, cx, WINDOWS[0]);
    // Until the original is here the thumbnail is shown, whole, in the
    // same room, and the toolbar claims no scale.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            let media = shell.pictures().remove(0);
            shell.begin_viewing(media, window, cx);
        })
    })
    .unwrap();
    let waiting = frame(&harness, cx);
    if waiting.scale.is_none() {
        let drawn = waiting.rect;
        assert!(
            close(
                drawn.size.height.as_f32(),
                waiting.area.size.height.as_f32(),
                0.75
            ) || close(
                drawn.size.width.as_f32(),
                waiting.area.size.width.as_f32(),
                0.75
            )
        );
    }
    view(&harness, cx, &url);
    let shown = frame(&harness, cx);
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).viewer_shown().unwrap().natural),
        (800, 1200)
    );
    // Portrait: its height is what fills the room.
    assert!(close(
        shown.rect.size.height.as_f32(),
        shown.area.size.height.as_f32(),
        0.75
    ));
    assert!(close(
        shown.rect.size.width.as_f32() / shown.rect.size.height.as_f32(),
        800. / 1200.,
        0.001
    ));
}
