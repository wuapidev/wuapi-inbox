//! Where the QR code is drawn while a number links.
//!
//! The frame has one size from the moment the screen opens: a placeholder
//! in the shape of a QR code waits in it, and the code takes its place
//! without anything around it moving. The placeholder is drawn here, from
//! the pattern `linking::skeleton_module` decides: it has nothing in it to
//! scan. Nothing is ever drawn over the real code, whose error correction
//! is the provider's and is not known here.

use super::widgets::{brand_mark, MarkPlay};
use crate::linking::{skeleton_module, SkeletonModule, SKELETON_SIDE};
use crate::motion;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use gpui_kit::prelude::*;
use gpui_kit::{canvas, div, fill, img, point, size, Bounds, Div, Image, ObjectFit, StyledImage};
use std::sync::Arc;

/// The side of the frame, and so of the QR code on its paper.
pub(super) const SIDE: f32 = 232.;
/// The paper around the code.
const PAD: f32 = 8.;
/// The plate in the middle of the placeholder, and the mark on it.
const PLATE: f32 = 64.;
const MARK: f32 = 36.;

/// What the frame shows right now.
pub(super) struct QrLook {
    /// The code, once there is one.
    pub image: Option<Arc<Image>>,
    /// How far the code has come in over the placeholder, from 0 to 1.
    pub shown: f32,
    /// Where the light is on the placeholder; `None` when it rests.
    pub light: Option<f32>,
    /// Something is still moving: the next frame is asked for.
    pub moving: bool,
    /// How the mark on the plate behaves.
    pub mark: MarkPlay,
}

/// The frame: the placeholder, the code, or one fading into the other.
pub(super) fn frame(look: QrLook, palette: &Palette) -> Div {
    let QrLook {
        image,
        shown,
        light,
        moving,
        mark,
    } = look;
    let shown = if image.is_some() { shown } else { 0. };
    let layer = || {
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .rounded(metrics::RADIUS())
    };
    div()
        .debug_selector(|| "link-qr-frame".into())
        .relative()
        .flex_none()
        .size(px(SIDE))
        .when(shown < 1., |this| {
            this.child(
                layer()
                    .debug_selector(|| "link-qr-skeleton".into())
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.muted)
                    .opacity(1. - shown)
                    .child(modules(light, moving, palette))
                    .child(plate(mark, palette)),
            )
        })
        .when_some(image, |this, image| {
            this.child(
                layer()
                    .debug_selector(|| "link-qr".into())
                    .p(px(PAD))
                    // A QR code is read on white, whatever the theme.
                    .bg(palette.qr_paper)
                    .opacity(shown)
                    .child(img(image).size_full().object_fit(ObjectFit::Contain)),
            )
        })
}

/// The placeholder's modules, with the light where it is.
fn modules(light: Option<f32>, moving: bool, palette: &Palette) -> impl IntoElement {
    let ink = palette.text_faint;
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<gpui_kit::Pixels>, _, window, _| {
            if moving {
                window.request_animation_frame();
            }
            let pad = px(PAD);
            let module = (bounds.size.width - pad * 2.) / SKELETON_SIDE as f32;
            let origin = bounds.origin + point(pad, pad);
            // A dot is a little smaller than its module, so the pattern
            // reads as dots; a finder square is drawn whole.
            let gap = module * 0.1;
            for y in 0..SKELETON_SIDE {
                for x in 0..SKELETON_SIDE {
                    let at = origin + point(module * x as f32, module * y as f32);
                    match skeleton_module(x, y) {
                        SkeletonModule::Clear => {}
                        SkeletonModule::Finder => window.paint_quad(fill(
                            Bounds::new(at, size(module, module)),
                            ink.opacity(motion::link::LIT),
                        )),
                        SkeletonModule::Dot => {
                            let along = (x + y) as f32 / (2 * (SKELETON_SIDE - 1)) as f32;
                            window.paint_quad(
                                fill(
                                    Bounds::new(
                                        at + point(gap, gap),
                                        size(module - gap * 2., module - gap * 2.),
                                    ),
                                    ink.opacity(motion::link::glow(along, light)),
                                )
                                .corner_radii(module * 0.2),
                            );
                        }
                    }
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The wuapi mark in the middle of the placeholder, on its plate. The mark
/// is the lime one on every theme: bare on a dark plate, on its ink tile on
/// a light one (`widgets::brand_mark`).
fn plate(mark: MarkPlay, palette: &Palette) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .debug_selector(|| "link-qr-plate".into())
                .flex_none()
                .size(px(PLATE))
                .rounded(px(14.))
                .flex()
                .items_center()
                .justify_center()
                .when(palette.is_dark(), |this| {
                    this.border_1()
                        .border_color(palette.border)
                        .bg(palette.background)
                })
                .child(brand_mark("link-qr-mark", px(MARK), mark, palette)),
        )
}
