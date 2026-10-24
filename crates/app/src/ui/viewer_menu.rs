//! The picture viewer's menu: a right click on the picture opens it at
//! the pointer, with what the toolbar does and what can be done with the
//! message the picture came in.
//!
//! It lives inside the viewer, not as an overlay of its own, so that the
//! picture stays behind it. Escape, or a click beside it, closes the menu
//! before the viewer.

use super::shell::Shell;
use crate::i18n;
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, Pixels, Point, Stateful, Window};

/// What an entry of the menu does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ViewerAction {
    Copy,
    SaveAs,
    SaveToDownloads,
    OpenWith,
    /// Toggles between the whole picture and its actual size.
    Zoom,
    /// Forwards the message the picture came in.
    Forward,
    /// Closes the viewer on the message the picture came in.
    GoToMessage,
}

/// How wide the menu is.
const WIDTH: f32 = 232.;
/// How tall an entry is.
const ROW: f32 = 32.;

impl Shell {
    /// Opens the menu at the pointer.
    pub(super) fn open_viewer_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.viewer.menu = Some(at);
        cx.notify();
    }

    /// The row of the open chat whose message the viewer's picture came
    /// in, if it came in one.
    fn viewed_message_key(&self) -> Option<String> {
        if self.viewed_picture().is_some() {
            return None;
        }
        let source = self.viewing.as_ref()?.source.as_ref()?;
        self.open.as_ref()?.rows.iter().find_map(|row| match row {
            super::bubble::Row::Message(row)
                if super::media_out::media_of(&row.stored.message)
                    .is_some_and(|media| media.source.as_ref() == Some(source)) =>
            {
                Some(row.key.clone())
            }
            _ => None,
        })
    }

    /// The menu's entries, in order.
    pub(super) fn viewer_menu_entries(&self) -> Vec<(ViewerAction, &'static str, IconName)> {
        let whole = matches!(self.viewer.zoom, super::viewer::Zoom::Fit);
        let mut entries = vec![
            (
                ViewerAction::Copy,
                i18n::t("common.viewer.copy"),
                IconName::Copy,
            ),
            (
                ViewerAction::SaveAs,
                i18n::t("common.viewer.save"),
                IconName::ArrowDownToLine,
            ),
            (
                ViewerAction::SaveToDownloads,
                i18n::t("common.viewer.save_downloads"),
                IconName::ArrowDownToLine,
            ),
            (
                ViewerAction::OpenWith,
                i18n::t("common.viewer.open_other"),
                IconName::ExternalLink,
            ),
            (
                ViewerAction::Zoom,
                if whole {
                    i18n::t("common.viewer.actual")
                } else {
                    i18n::t("common.viewer.fit")
                },
                if whole {
                    IconName::Scan
                } else {
                    IconName::Maximize
                },
            ),
        ];
        if self.viewed_message_key().is_some() {
            entries.extend([
                (
                    ViewerAction::Forward,
                    i18n::t("common.viewer.forward"),
                    IconName::Forward,
                ),
                (
                    ViewerAction::GoToMessage,
                    i18n::t("common.viewer.go_to_message"),
                    IconName::MessageSquareText,
                ),
            ]);
        }
        entries
    }

    /// Does what an entry says, and closes the menu.
    pub(super) fn run_viewer_action(
        &mut self,
        action: ViewerAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.viewer.menu = None;
        match action {
            ViewerAction::Copy => {
                self.copy_image(cx);
            }
            ViewerAction::SaveAs => {
                self.save_as(None, cx);
            }
            ViewerAction::SaveToDownloads => {
                self.save_to_downloads(None, cx);
            }
            ViewerAction::OpenWith => self.open_viewed(cx),
            ViewerAction::Zoom => {
                if matches!(self.viewer.zoom, super::viewer::Zoom::Fit) {
                    self.zoom_viewer_to(super::viewer::Zoom::At(1.), None, window, cx);
                } else {
                    self.zoom_viewer(None, window, cx);
                }
            }
            ViewerAction::Forward | ViewerAction::GoToMessage => {
                let Some(key) = self.viewed_message_key() else {
                    return cx.notify();
                };
                self.close_overlay(window, cx);
                self.focus_message(key.clone(), window, cx);
                if action == ViewerAction::Forward {
                    self.begin_forward(window, cx);
                } else {
                    self.flash_row(key, cx);
                }
            }
        }
        cx.notify();
    }

    /// The menu, at the pointer and inside the window, when it is open.
    pub(super) fn render_viewer_menu(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let at = self.viewer.menu?;
        let entries = self.viewer_menu_entries();
        let height = px(ROW) * entries.len() as f32 + px(10.);
        let left =
            at.x.min(self.viewport.width - px(WIDTH) - px(8.))
                .max(px(8.));
        let top = at.y.min(self.viewport.height - height - px(8.)).max(px(8.));
        let mut menu = self
            .card("viewer-menu", palette)
            .absolute()
            .left(left)
            .top(top)
            .w(px(WIDTH))
            .p_1()
            .flex()
            .flex_col()
            .on_mouse_down(gpui_kit::MouseButton::Right, |_, _, cx| {
                cx.stop_propagation()
            });
        let hover = palette.elevated_selected;
        for (index, (action, label, glyph)) in entries.into_iter().enumerate() {
            let name = format!("viewer-menu-{action:?}").to_lowercase();
            menu = menu.child(
                div()
                    .id(("viewer-menu-item", index))
                    .debug_selector(move || name.clone())
                    .h(px(ROW))
                    .px_2()
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .text_size(metrics::TEXT_BODY())
                    .text_color(palette.text)
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.run_viewer_action(action, window, cx);
                    }))
                    .child(icon(glyph, px(16.), palette.icon))
                    .child(label),
            );
        }
        Some(menu)
    }
}
