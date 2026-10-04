//! The ring around the picture of somebody with a status.
//!
//! One piece per story, oldest first from the top and clockwise, with a
//! gap between pieces: the ring says how many stories there are and which
//! are new. The new ones are in the accent (the brand's signal, the same
//! colour as an unread count), the ones seen in the hairline's colour.
//! The accent is used rather than the person's own colour on purpose:
//! people's colours say who wrote what, the accent says something is
//! waiting, and a ring that did both could not be read.

use crate::stories::{ring_segments, RingSegment};
use crate::theme::{story, Palette};
use client_core::StoryRing;
use gpui_kit::prelude::*;
use gpui_kit::{canvas, div, point, Div, Hsla, PathBuilder, Pixels, Window};
use std::f32::consts::TAU;

/// The ring's pieces for somebody with `ring.total` stories of which
/// `ring.unviewed` are new. The newest stories are the ones not seen, as
/// they are posted: the seen ones come first round the ring.
pub fn segments_of(ring: StoryRing) -> Vec<RingSegment> {
    let unviewed = ring.unviewed.min(ring.total);
    let viewed: Vec<bool> = (0..ring.total)
        .map(|index| index < ring.total - unviewed)
        .collect();
    ring_segments(&viewed)
}

/// Strokes the pieces into `bounds`' circle.
fn paint(
    pieces: &[RingSegment],
    seen: Hsla,
    new: Hsla,
    stroke: Pixels,
    bounds: gpui_kit::Bounds<Pixels>,
    window: &mut Window,
) {
    let radius = bounds.size.width / 2. - stroke / 2.;
    let centre = bounds.center();
    let at = |turn: f32| {
        let angle = turn * TAU;
        point(
            centre.x + radius * angle.sin(),
            centre.y - radius * angle.cos(),
        )
    };
    for piece in pieces {
        let colour = if piece.viewed { seen } else { new };
        let span = (piece.to - piece.from).clamp(0., 1.);
        if span <= 0.001 {
            continue;
        }
        let mut path = PathBuilder::stroke(stroke);
        path.move_to(at(piece.from));
        // A whole turn has no direction: drawn in two halves, like every
        // arc here.
        let steps = if span > 0.5 { 2 } else { 1 };
        for step in 1..=steps {
            path.arc_to(
                point(radius, radius),
                gpui_kit::px(0.),
                false,
                true,
                at(piece.from + span * step as f32 / steps as f32),
            );
        }
        if let Ok(path) = path.build() {
            window.paint_path(path, colour);
        }
    }
}

/// The ring drawn over the whole of its box.
fn ring(ring: StoryRing, palette: &Palette) -> impl IntoElement {
    let pieces = segments_of(ring);
    let (seen, new) = (palette.grid_mark, palette.accent);
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| paint(&pieces, seen, new, story::RING_STROKE(), bounds, window),
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The ring drawn outside a picture of `side` pixels, without changing
/// the box the picture is in (the chat list's rows and the conversation's
/// header keep their layout): an overlay that reaches past the picture by
/// the gap and the stroke.
pub fn outset(stories: StoryRing, side: Pixels, palette: &Palette) -> Div {
    let room = story::RING_GAP() + story::RING_STROKE();
    div()
        .debug_selector(move || format!("story-ring-{}-{}", stories.total, stories.unviewed))
        .absolute()
        .top(-room)
        .left(-room)
        .size(side + room * 2.)
        .child(ring(stories, palette))
}

/// A picture with its ring: the picture's size with the gap and the
/// stroke around it. Without stories the picture is drawn in the same box,
/// so nothing moves when somebody posts one.
pub fn with_ring(face: Div, stories: Option<StoryRing>, palette: &Palette) -> Div {
    let room = story::RING_GAP() + story::RING_STROKE();
    let outer = div()
        .flex_none()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .p(room);
    match stories {
        Some(stories) if stories.total > 0 => outer
            .debug_selector(move || format!("story-ring-{}-{}", stories.total, stories.unviewed))
            .child(ring(stories, palette))
            .child(face),
        _ => outer.child(face),
    }
}
