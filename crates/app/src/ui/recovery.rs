//! The screen for a local database that cannot be opened: it is encrypted
//! and its key is no longer in the system keychain.
//!
//! The database is a copy of what the provider has, so the way forward is
//! to delete it and sync again. That is asked, never done unasked: messages
//! that were still waiting to be sent are in that file and are lost with
//! it.

use super::conversation::{colophon, mark_frame};
use super::widgets::{grid_mark, screen_header, screen_rail, tag, text_button, MarkPlay};
use crate::settings;
use crate::theme::px;
use crate::theme::{fonts, metrics, palette};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, Context, EventEmitter, FocusHandle, FontFeatures, FontWeight, SharedString, Window,
};
use std::sync::Arc;

/// What the screen tells its owner.
pub enum RecoveryEvent {
    /// The person chose to delete the local database and sync again.
    Reset,
}

/// The screen.
pub struct RecoveryScreen {
    /// Why starting over did not work, when it did not.
    pub(super) error: Option<SharedString>,
    focus: FocusHandle,
}

impl EventEmitter<RecoveryEvent> for RecoveryScreen {}

impl RecoveryScreen {
    /// Builds the screen.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        Self { error: None, focus }
    }
}

impl Render for RecoveryScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let features = FontFeatures(Arc::new(
            fonts::SANS_FEATURES
                .iter()
                .map(|(tag, value)| ((*tag).to_owned(), *value))
                .collect(),
        ));
        // Nothing to celebrate here: the mark stays still.
        let play = MarkPlay {
            paused: true,
            reduced: settings::reduce_motion(cx),
            intro: false,
            frozen: None,
        };
        div()
            .id("recovery")
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .bg(palette.background)
            .text_color(palette.text)
            .font_family(fonts::SANS)
            .font_features(features)
            .child(screen_rail(&palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(screen_header("Local data", 1., &palette))
                    .child(
                        div()
                            .flex_1()
                            .min_h_0()
                            .px_6()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .debug_selector(|| "recovery-form".into())
                                    .w_full()
                                    .max_w(metrics::FORM_WIDTH())
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .child(mark_frame("recovery-mark", play, 1., &palette))
                                    .child(
                                        div()
                                            .pt_6()
                                            .text_center()
                                            .text_size(metrics::TEXT_DISPLAY())
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child("The local database cannot be opened"),
                                    )
                                    .child(
                                        div()
                                            .pt_2()
                                            .text_center()
                                            .text_size(metrics::TEXT_BODY())
                                            .line_height(px(22.))
                                            .text_color(palette.text_muted)
                                            .child(
                                                "It is encrypted, and its key is no longer in \
                                                 your system keychain. Resetting deletes the \
                                                 copy on this computer and downloads your chats \
                                                 again.",
                                            ),
                                    )
                                    .child(div().pt_4().child(tag(
                                        "Messages waiting to be sent are lost",
                                        Some(palette.danger),
                                        &palette,
                                    )))
                                    .children(self.error.clone().map(|error| {
                                        div()
                                            .debug_selector(|| "recovery-error".into())
                                            .pt_3()
                                            .text_center()
                                            .text_size(metrics::TEXT_SMALL())
                                            .line_height(px(19.))
                                            .text_color(palette.danger)
                                            .child(error)
                                    }))
                                    .child(
                                        div()
                                            .pt_6()
                                            .flex()
                                            .gap_2()
                                            .child(
                                                text_button(
                                                    "recovery-quit",
                                                    "Quit",
                                                    None,
                                                    false,
                                                    &palette,
                                                )
                                                .on_click(|_, _, cx| cx.quit()),
                                            )
                                            // Not on Enter: deleting takes a
                                            // deliberate click (or Tab to it).
                                            .child(
                                                text_button(
                                                    "reset-database",
                                                    "Reset local data",
                                                    None,
                                                    true,
                                                    &palette,
                                                )
                                                .on_click(cx.listener(|_, _, _, cx| {
                                                    cx.emit(RecoveryEvent::Reset)
                                                })),
                                            ),
                                    ),
                            ),
                    )
                    .child(colophon(&palette)),
            )
            .child(grid_mark(
                metrics::RAIL_WIDTH(),
                metrics::HEADER_HEIGHT(),
                &palette,
            ))
    }
}
