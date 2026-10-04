//! Shows the mark on a dark and a light background.
//!
//! ```text
//! cargo run -p brand-mark --example preview
//! ```
//!
//! By default the marks are alive, at a few sizes. Environment:
//!
//! - `BRAND_MARK_SHEET=1` shows the key poses of the timeline side by side,
//!   frozen, instead: the entrance, a blink, each look, and the pointer's
//!   gesture.
//! - `BRAND_MARK_FOREVER=1` never rests.
//! - `BRAND_MARK_STILL=1` pauses the animation.
//! - `BRAND_MARK_REDUCED=1` turns reduced motion on.
//! - `BRAND_MARK_TRACE=1` prints every redraw of the view, with its time.

use std::time::{Duration, Instant};

use brand_mark::{AnimatedMark, Pose, Repeat, Timeline};
use gpui_kit::{
    div, prelude::*, px, rgb, size, App, Bounds, Context, Pixels, Render, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions,
};

/// The window class, so a compositor rule can place the preview.
const APP_ID: &str = "dev.wuapi.brand-mark-preview";

const SIZES: [f32; 4] = [24., 48., 96., 160.];
// The app's page colours: near-black and paper.
const DARK: u32 = 0x0a0a0a;
const LIGHT: u32 = 0xfafaf9;

/// Seconds on [`Timeline::site`] that show each pose: the bubble popping,
/// the eyes opening, the w half drawn, rest, a blink (while looking right),
/// then looking right, left, up and up-right.
const SHEET: [f64; 9] = [0.15, 0.40, 0.80, 1.50, 4.11, 4.55, 6.53, 7.96, 9.72];
/// Seconds after the pointer arrives: the first of the two blinks, the w
/// running off, the glance held while the w draws again.
const SHEET_POINTED: [f64; 3] = [0.07, 0.20, 0.50];
const SHEET_SIDE: f32 = 96.;
const SHEET_GAP: f32 = 16.;

fn flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty() && value != "0")
}

fn sheet_poses() -> Vec<Pose> {
    let timeline = Timeline::site();
    let pointed = Duration::from_secs(30);
    let idle = SHEET.map(|at| timeline.pose(Duration::from_secs_f64(at)));
    let gesture = SHEET_POINTED.map(|after| {
        let at = pointed + Duration::from_secs_f64(after);
        timeline.frame(at, Some(pointed)).pose
    });
    idle.into_iter().chain(gesture).collect()
}

struct Preview {
    opened: Instant,
    renders: usize,
}

impl Preview {
    fn live(&self, side: f32, tile: bool) -> AnimatedMark {
        let mark = AnimatedMark::new(px(side))
            .id(SharedString::from(format!("live-{side}-{tile}")))
            .tile(tile)
            .animate(!flag("BRAND_MARK_STILL"))
            .reduced_motion(flag("BRAND_MARK_REDUCED"));
        if flag("BRAND_MARK_FOREVER") {
            mark.repeat(Repeat::Forever)
        } else {
            mark
        }
    }

    fn row(&self, background: u32, tile: bool) -> impl IntoElement {
        let row = div()
            .flex()
            .flex_1()
            .items_center()
            .justify_center()
            .bg(rgb(background));
        if flag("BRAND_MARK_SHEET") {
            let poses = sheet_poses().into_iter().enumerate();
            row.gap(px(SHEET_GAP)).children(poses.map(|(index, pose)| {
                AnimatedMark::new(px(SHEET_SIDE))
                    .id(SharedString::from(format!("pose-{index}-{tile}")))
                    .tile(tile)
                    .pose(pose)
            }))
        } else {
            row.gap(px(48.))
                .children(SIZES.map(|side| self.live(side, tile)))
        }
    }
}

impl Render for Preview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        // How often the view is redrawn is what the animation costs.
        self.renders += 1;
        if flag("BRAND_MARK_TRACE") {
            let at = self.opened.elapsed().as_secs_f64();
            eprintln!("render {} at {at:.3}", self.renders);
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(self.row(DARK, false))
            .child(self.row(LIGHT, true))
    }
}

fn main() {
    gpui_kit::application().run(|cx: &mut App| {
        let extent: gpui_kit::Size<Pixels> = if flag("BRAND_MARK_SHEET") {
            let poses = (SHEET.len() + SHEET_POINTED.len()) as f32;
            size(px(poses * (SHEET_SIDE + SHEET_GAP) + 56.), px(300.))
        } else {
            size(px(720.), px(480.))
        };
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, extent, cx))),
            titlebar: Some(TitlebarOptions {
                title: Some("wuapi mark".into()),
                ..Default::default()
            }),
            app_id: Some(APP_ID.into()),
            ..Default::default()
        };
        cx.open_window(options, |_, cx| {
            cx.new(|_| Preview {
                opened: Instant::now(),
                renders: 0,
            })
        })
        .expect("the preview window opens");
    });
}
