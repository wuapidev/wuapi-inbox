//! The centred pills of a conversation's page: the day, and how many
//! messages were unread when the chat was opened.
//!
//! They stand on the wallpaper, so each is a small surface of its own with
//! a hairline around it: what they say never depends on what is behind.

use super::widgets::mono;
use crate::theme::px;
use crate::theme::{hairline, metrics, Palette};
use gpui_kit::prelude::*;
use gpui_kit::{div, Div, Hsla, Pixels, SharedString};

/// A pill: mono capitals on the raised surface.
fn pill(text: SharedString, ink: Hsla, palette: &Palette) -> Div {
    div()
        .flex_none()
        .h(metrics::PILL())
        .px(px(11.))
        .rounded_full()
        .border_1()
        .border_color(palette.border)
        .bg(palette.surface)
        .flex()
        .items_center()
        .text_color(ink)
        .child(mono(text))
}

/// "Today", "Yesterday" or a date, above the first message of that day.
pub fn day(text: SharedString, palette: &Palette) -> Div {
    div()
        .w_full()
        .pt(px(16.))
        .pb(px(4.))
        .flex()
        .justify_center()
        .child(
            pill(text.to_uppercase().into(), palette.text_muted, palette)
                .debug_selector(|| "day-pill".into()),
        )
}

/// What the unread divider says.
pub fn unread_label(count: u32) -> String {
    match count {
        1 => "1 unread message".to_owned(),
        n => format!("{n} unread messages"),
    }
}

/// "3 unread messages", in the accent, on a rule across the thread.
pub fn unread(count: u32, gutter: Pixels, palette: &Palette) -> Div {
    let rule = || div().flex_1().h(hairline()).bg(palette.border);
    div()
        .debug_selector(|| "unread-divider".into())
        .w_full()
        .px(gutter)
        .pt(px(14.))
        .pb(px(4.))
        .flex()
        .items_center()
        .gap(px(10.))
        .child(rule())
        .child(
            pill(
                unread_label(count).to_uppercase().into(),
                palette.accent,
                palette,
            )
            .debug_selector(|| "unread-pill".into()),
        )
        .child(rule())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_divider_counts_in_words() {
        assert_eq!(unread_label(1), "1 unread message");
        assert_eq!(unread_label(2), "2 unread messages");
        assert_eq!(unread_label(120), "120 unread messages");
    }
}
