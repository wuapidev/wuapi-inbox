//! The round button on a file that is not here yet: download it, see how
//! far it is and stop it, or try again.
//!
//! One control for every kind of attachment, so that a picture, a video
//! and a document say the same thing the same way.

use crate::format::file_size;
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::Palette;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, div, point, ClickEvent, Div, ElementId, Hsla, PathBuilder, Pixels, SharedString,
    Stateful, Window,
};
use std::f32::consts::TAU;

/// What the button offers.
#[derive(Clone, Debug, PartialEq)]
pub enum Transfer {
    /// Not here, and not coming by itself: a click downloads it.
    Download {
        /// The file's size, when the message says.
        size: Option<u64>,
    },
    /// On its way. `fraction` is how much has arrived, when the size is
    /// known; a click stops it.
    Loading {
        /// From 0 to 1.
        fraction: Option<f32>,
    },
    /// It did not arrive, with the reason: a click tries again.
    Retry(SharedString),
}

impl Transfer {
    /// The name the button goes by in tests.
    pub fn selector(&self) -> &'static str {
        match self {
            Self::Download { .. } => "media-download",
            Self::Loading { .. } => "media-progress",
            Self::Retry(_) => "media-retry",
        }
    }

    /// What it says under the pointer.
    fn hint(&self) -> SharedString {
        match self {
            Self::Download { size: Some(size) } => {
                format!("Download ({})", file_size(*size)).into()
            }
            Self::Download { size: None } => "Download".into(),
            Self::Loading {
                fraction: Some(fraction),
            } => format!("Downloading, {:.0}%: click to stop", fraction * 100.).into(),
            Self::Loading { fraction: None } => "Downloading: click to stop".into(),
            Self::Retry(reason) => format!("{reason} Click to try again.").into(),
        }
    }
}

/// The side of the button.
pub fn button_side() -> Pixels {
    px(40.)
}

/// A ring around the button: all of it faint, and `fraction` of it (from
/// the top, clockwise) in ink. Without a fraction, a quarter of it, which
/// turns while motion is allowed.
pub(super) fn ring(fraction: Option<f32>, track: Hsla, ink: Hsla, still: bool) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let stroke = gpui_kit::px(2.5);
            let radius = bounds.size.width / 2. - stroke;
            let centre = bounds.center();
            let at = |turn: f32| {
                let angle = turn * TAU;
                point(
                    centre.x + radius * angle.sin(),
                    centre.y - radius * angle.cos(),
                )
            };
            let arc = |window: &mut Window, from: f32, to: f32, colour: Hsla| {
                // An arc of a whole turn has no direction: two halves.
                let span = (to - from).clamp(0., 1.);
                if span <= 0.001 {
                    return;
                }
                let mut path = PathBuilder::stroke(stroke);
                path.move_to(at(from));
                let middle = from + span / 2.;
                for end in [middle, from + span] {
                    path.arc_to(
                        point(radius, radius),
                        gpui_kit::px(0.),
                        false,
                        true,
                        at(end),
                    );
                }
                if let Ok(path) = path.build() {
                    window.paint_path(path, colour);
                }
            };
            arc(window, 0., 1., track);
            match fraction {
                Some(fraction) => arc(window, 0., fraction.clamp(0.02, 1.), ink),
                None => {
                    let turn = if still {
                        0.
                    } else {
                        // Once round in a little under a second.
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_or(0, |since| since.as_millis());
                        window.request_animation_frame();
                        (now % 900) as f32 / 900.
                    };
                    arc(window, turn, turn + 0.25, ink);
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The button. `still` holds the turning ring where it is (reduced
/// motion). Reachable with Tab; Enter or Space is the click.
pub fn transfer_button(
    id: impl Into<ElementId>,
    state: &Transfer,
    palette: &Palette,
    still: bool,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> Stateful<Div> {
    let selector = state.selector();
    let hint = state.hint();
    let (fill, ink, focus) = (
        palette.media_button,
        palette.on_media_button,
        palette.accent,
    );
    let glyph = match state {
        Transfer::Download { .. } => IconName::ArrowDownToLine,
        Transfer::Loading { .. } => IconName::X,
        Transfer::Retry(_) => IconName::RotateCw,
    };
    div()
        .id(id)
        .debug_selector(move || selector.into())
        .relative()
        .flex_none()
        .size(button_side())
        .rounded_full()
        .bg(fill)
        .border_2()
        .border_color(gpui_kit::transparent_black())
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .focus_visible(move |style| style.border_color(focus))
        .hover(|style| style.opacity(0.85))
        .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            on_click(event, window, cx);
        })
        .child(icon(glyph, px(17.), ink))
        .when_some(
            match state {
                Transfer::Loading { fraction } => Some(*fraction),
                _ => None,
            },
            |this, fraction| this.child(ring(fraction, ink.opacity(0.3), ink, still)),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_button_says_what_a_click_does() {
        assert_eq!(
            Transfer::Download {
                size: Some(1_258_291)
            }
            .hint()
            .as_ref(),
            "Download (1.2 MB)"
        );
        assert_eq!(
            Transfer::Download { size: None }.hint().as_ref(),
            "Download"
        );
        assert_eq!(
            Transfer::Loading {
                fraction: Some(0.42)
            }
            .hint()
            .as_ref(),
            "Downloading, 42%: click to stop"
        );
        assert!(Transfer::Retry("The connection dropped.".into())
            .hint()
            .contains("try again"));
        assert_eq!(
            Transfer::Download { size: None }.selector(),
            "media-download"
        );
        assert_eq!(
            Transfer::Loading { fraction: None }.selector(),
            "media-progress"
        );
    }
}
