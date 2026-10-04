//! What opens on top of the panes: the two menus, "New chat" and the
//! image viewer. The settings panel is in `panels.rs`; the profile and group
//! panels are in `profiles.rs` and `groups.rs`.
//!
//! One overlay at a time (`Shell::overlay`). Each one is drawn over a layer
//! that covers the window: a click on that layer closes the overlay, and
//! while it is open the keyboard belongs to it (arrows and Enter in a menu,
//! Escape everywhere).

use super::shell::{MenuItem, Overlay, Shell};
use super::transfer::{transfer_button, Transfer};
use super::widgets::{avatar_or, icon_button, label, mono, text_button, AvatarKind};
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use gpui_kit::component::input::Input;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, img, AnyElement, ClickEvent, Context, Div, FontWeight, KeyDownEvent, ObjectFit,
    SharedString, Stateful, StyledImage, Window,
};

/// Width of a menu.
#[allow(non_snake_case)]
fn MENU_WIDTH() -> gpui_kit::Pixels {
    px(248.)
}
/// What a second line, the reason an entry is disabled, adds to its row.
#[allow(non_snake_case)]
pub(super) fn REASON_LINE() -> gpui_kit::Pixels {
    px(15.)
}
/// Height of a menu entry.
#[allow(non_snake_case)]
fn MENU_ROW() -> gpui_kit::Pixels {
    px(34.)
}

impl Shell {
    /// The open overlay, if any, with the layer that closes it.
    pub(super) fn render_overlay(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        // Menus drop from their buttons; the header's buttons sit 8 px
        // from the pane's edge.
        let list_right = metrics::RAIL_WIDTH() + self.list_px - px(8.);
        let (content, veil): (AnyElement, bool) = match self.overlay {
            Overlay::None => return None,
            Overlay::ListMenu => (
                self.render_menu(palette, cx)
                    .left(list_right - MENU_WIDTH())
                    .top(metrics::HEADER_HEIGHT() - px(8.))
                    .into_any_element(),
                false,
            ),
            Overlay::RowMenu => (
                self.render_menu(palette, cx)
                    // Where the pointer was, kept inside the window.
                    .left(self.menu_at.x.min(px(1240.) - MENU_WIDTH()))
                    .top(self.menu_at.y)
                    .into_any_element(),
                false,
            ),
            Overlay::MessageMenu => {
                let entries = self.menu_items(cx);
                // A disabled entry says why on a second line.
                let reasons = entries
                    .iter()
                    .filter(|item| item.hint.is_some_and(|hint| !hint.is_empty()))
                    .count() as f32;
                let height = MENU_ROW() * entries.len() as f32 + REASON_LINE() * reasons + px(10.);
                // At the pointer, else beside the message in focus; kept
                // inside the window either way.
                let at = self
                    .message_menu_at
                    .unwrap_or_else(|| self.focused_anchor());
                let left =
                    at.x.min(self.viewport.width - MENU_WIDTH() - px(8.))
                        .max(px(8.));
                let top = at.y.min(self.viewport.height - height - px(8.)).max(px(8.));
                (
                    self.render_menu(palette, cx)
                        .left(left)
                        .top(top)
                        .into_any_element(),
                    false,
                )
            }
            Overlay::React => {
                let at = self.focused_anchor();
                // With every emoji showing it is a good deal larger.
                let size = self.react_sheet_size();
                (
                    self.render_react_sheet(palette, cx)
                        .left(at.x.min(self.viewport.width - size.width).max(px(8.)))
                        .top(at.y.min(self.viewport.height - size.height).max(px(8.)))
                        .into_any_element(),
                    false,
                )
            }
            Overlay::Reactors => {
                let at = self.reactors_origin();
                (
                    self.render_reactors(palette, cx)?
                        .left(at.x)
                        .top(at.y)
                        .into_any_element(),
                    false,
                )
            }
            Overlay::EmojiPicker => (
                self.render_emoji_picker(palette, cx).into_any_element(),
                false,
            ),
            Overlay::Palette => (
                // Centred across the window, its top a fixed share of the
                // window's height down: it grows downward with its
                // results and never moves.
                div()
                    .absolute()
                    .top(super::palette::palette_top(self.viewport.height))
                    .left_0()
                    .w_full()
                    .flex()
                    .justify_center()
                    .child(self.render_palette(palette, cx))
                    .into_any_element(),
                true,
            ),
            Overlay::Shortcuts => (self.render_shortcuts(palette, cx).into_any_element(), true),
            Overlay::DeleteMessage => (
                self.render_delete_sheet(palette, cx).into_any_element(),
                true,
            ),
            Overlay::MessageInfo => (
                self.render_info_sheet(palette, cx)?.into_any_element(),
                true,
            ),
            Overlay::Forward => (
                self.render_forward_sheet(palette, cx).into_any_element(),
                true,
            ),
            Overlay::ChatMenu => (
                self.render_menu(palette, cx)
                    .right(px(8.))
                    .top(metrics::HEADER_HEIGHT() - px(8.))
                    .into_any_element(),
                false,
            ),
            Overlay::NewChat => (
                self.render_new_chat(palette, cx)
                    .left(list_right - px(340.))
                    .top(metrics::HEADER_HEIGHT() - px(8.))
                    .into_any_element(),
                false,
            ),
            Overlay::Settings => (self.render_settings(palette, cx).into_any_element(), true),
            Overlay::ContactInfo => (self.render_info(palette, cx)?.into_any_element(), true),
            Overlay::NewGroup => (self.render_new_group(palette, cx)?.into_any_element(), true),
            Overlay::OwnProfile => (
                self.render_own_profile(palette, cx)?.into_any_element(),
                true,
            ),
            Overlay::Viewer => (
                self.render_viewer(palette, window, cx)?.into_any_element(),
                true,
            ),
            Overlay::NewPoll => (self.render_poll_form(palette, cx)?.into_any_element(), true),
            Overlay::ConfirmSignOut => (
                self.render_confirm_sign_out(palette, cx).into_any_element(),
                true,
            ),
            Overlay::RailMenu => (
                self.render_rail_menu(palette, cx)?.into_any_element(),
                false,
            ),
            Overlay::RailEdit => (self.render_rail_edit(palette, cx)?.into_any_element(), true),
            Overlay::AttachSheet => (
                self.render_attach_sheet(palette, cx)?.into_any_element(),
                true,
            ),
            Overlay::AddNumber => (
                self.render_add_number(palette, cx)?.into_any_element(),
                true,
            ),
            Overlay::NumberAction => (
                self.render_number_action(palette, cx)?.into_any_element(),
                true,
            ),
            Overlay::Status => (
                self.render_status_sheet(palette, cx)?.into_any_element(),
                true,
            ),
        };
        Some(
            div()
                .id("overlay")
                .debug_selector(|| "overlay".into())
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                // Nothing underneath is hovered or clicked through.
                .occlude()
                .when(veil, |this| {
                    this.bg(palette.scrim)
                        .flex()
                        .items_center()
                        .justify_center()
                })
                .track_focus(&self.overlay_focus)
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    this.overlay_key(event, window, cx)
                }))
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    // A click outside the card closes it. Enter or Space
                    // while the layer holds the keyboard is not a click
                    // outside anything.
                    if !matches!(event, ClickEvent::Keyboard(_)) {
                        this.dismiss_overlay(window, cx);
                    }
                }))
                // Files dropped on the open sheet join the ones in it (the
                // layer is over the conversation's own drop target).
                .on_drop(cx.listener(|this, paths: &gpui_kit::ExternalPaths, _, cx| {
                    if this.overlay == Overlay::AttachSheet {
                        this.attach_paths(paths.paths().to_vec(), cx);
                    }
                }))
                .child(content)
                .into_any_element(),
        )
    }

    /// A card on the overlay layer. Clicks inside it stay inside.
    pub(super) fn card(&self, id: &'static str, palette: &Palette) -> Stateful<Div> {
        div()
            .id(id)
            .debug_selector(move || id.into())
            // Lifted over the window: its own surface, an outline stronger
            // than a hairline and a soft shade. The brand is flat, so the
            // shade is slight; the veil and the outline do the parting.
            .rounded(metrics::PANEL_RADIUS())
            .border_1()
            .border_color(palette.elevated_border)
            .bg(palette.elevated)
            .shadow(vec![gpui_kit::BoxShadow {
                color: palette.elevated_shadow,
                offset: gpui_kit::point(px(0.), px(6.)),
                blur_radius: px(18.),
                spread_radius: px(0.),
                inset: false,
            }])
            .text_color(palette.text)
            .on_click(|_, _, cx| cx.stop_propagation())
    }

    fn render_menu(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let items = self.menu_items(cx);
        let mut menu = self
            .card("menu", palette)
            .absolute()
            .w(MENU_WIDTH())
            .p_1()
            .flex()
            .flex_col();
        for (index, item) in items.into_iter().enumerate() {
            menu = menu.child(self.render_menu_item(index, item, palette, cx));
        }
        menu
    }

    fn render_menu_item(
        &self,
        index: usize,
        item: MenuItem,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = item.id;
        let row = div()
            .id(("menu-item", index))
            .debug_selector(move || name.into())
            .min_h(MENU_ROW())
            .px_2()
            .rounded(px(4.))
            .flex()
            .items_center()
            .gap_2()
            .text_size(metrics::TEXT_BODY());
        match item.action {
            Some(action) => {
                let hover = palette.elevated_selected;
                row.cursor_pointer()
                    .text_color(palette.text)
                    .when(index == self.menu_cursor, |this| {
                        this.bg(palette.elevated_selected)
                    })
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.run_menu_action(action, window, cx);
                    }))
                    .child(icon(item.icon, px(16.), palette.icon))
                    .child(
                        div()
                            .debug_selector(move || format!("{name}-label"))
                            .flex_1()
                            .min_w_0()
                            .child(item.label),
                    )
                    // The entry's keys, as the registry has them.
                    .children(item.keys.and_then(crate::keys::keys_label).map(|keys| {
                        mono(keys)
                            .debug_selector(move || format!("{name}-keys"))
                            .text_size(px(10.))
                            .text_color(palette.text_faint)
                    }))
                    .into_any_element()
            }
            // Not there yet: say so on the entry itself, not in a tooltip
            // someone has to find.
            //
            // The whole label first, and the reason under it: the reason
            // never takes the label's room.
            None => {
                let reason = item.hint.filter(|hint| !hint.is_empty());
                let tip: Option<SharedString> = reason.map(SharedString::from);
                row.text_color(palette.text_faint)
                    .py(px(3.))
                    .child(icon(item.icon, px(16.), palette.text_faint))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .debug_selector(move || format!("{name}-label"))
                                    .child(item.label),
                            )
                            .children(reason.map(|reason| {
                                div()
                                    .debug_selector(move || format!("{name}-reason"))
                                    .text_size(px(10.5))
                                    .text_color(palette.text_faint)
                                    .child(reason)
                            })),
                    )
                    .when_some(tip, |this, tip| {
                        this.tooltip(move |window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(tip.clone())
                                .build(window, cx)
                        })
                    })
                    .into_any_element()
            }
        }
    }

    /// "New chat": the address book, searched as it is typed in, and a
    /// row for a number that is in nobody's.
    fn render_new_chat(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let (contacts, number) = self.new_chat_rows(cx);
        let typed = !self.new_chat.read(cx).value().trim().is_empty();
        let hover = palette.hover;
        let mut list = div().flex().flex_col();
        for (index, contact) in contacts.iter().enumerate() {
            let name = contact.display_name();
            let detail = contact
                .phone
                .clone()
                .filter(|phone| *phone != name)
                .or_else(|| contact.username.as_ref().map(|user| format!("@{user}")));
            let chosen = contact.clone();
            let subject = client_provider::ChatId::new(contact.id.as_str());
            list = list.child(
                div()
                    .id(("new-chat-contact", index))
                    .debug_selector(move || format!("new-chat-contact-{index}"))
                    .h(px(44.))
                    .px_2()
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .when(index == self.new_chat_cursor, |this| this.bg(palette.muted))
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_contact(chosen.clone(), window, cx);
                    }))
                    .child(avatar_or(
                        self.media.avatar(&contact.account_id, &subject),
                        &name,
                        AvatarKind::Person,
                        px(28.),
                        palette,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(metrics::TEXT_BODY())
                                    .child(SharedString::from(name.clone())),
                            )
                            .children(
                                detail.map(|detail| mono(detail).text_color(palette.text_muted)),
                            ),
                    ),
            );
        }
        if let Some(number) = number {
            let index = contacts.len();
            let write_to = number.clone();
            list = list.child(
                div()
                    .id("new-chat-open")
                    .debug_selector(|| "new-chat-open".into())
                    .h(px(44.))
                    .px_2()
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .when(index == self.new_chat_cursor, |this| this.bg(palette.muted))
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_number(write_to.clone(), cx);
                    }))
                    .child(icon(IconName::Plus, px(16.), palette.icon))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().text_size(metrics::TEXT_BODY()).child(
                                if self.new_chat_checking {
                                    "Checking the number…"
                                } else {
                                    "New number"
                                },
                            ))
                            .child(mono(number).text_color(palette.text_muted)),
                    ),
            );
        }
        // Why the list is empty, when it is.
        let synced = self
            .account
            .as_ref()
            .is_some_and(|account| self.engine.contacts_synced(account));
        let empty: Option<&'static str> = if !contacts.is_empty() {
            None
        } else if typed {
            Some("No contact matches. To write to a number, type it with its country code.")
        } else if !self.engine.capabilities().contacts {
            Some("Write to a number, with its country code.")
        } else if synced {
            Some("No contacts yet: the phone's address book is empty, or has not been shared.")
        } else {
            Some("Your contacts appear here once the phone has synced them.")
        };
        self.card("new-chat-panel", palette)
            .absolute()
            .w(px(340.))
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(label("New chat", palette))
            .child(
                div()
                    .h(px(36.))
                    .px_2()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.background)
                    .flex()
                    .items_center()
                    .child(Input::new(&self.new_chat).appearance(false)),
            )
            .child(list)
            .children(empty.map(|text| {
                div()
                    .debug_selector(|| "new-chat-empty".into())
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(18.))
                    .text_color(palette.text_muted)
                    .child(text)
            }))
            .children(self.new_chat_error.clone().map(|error| {
                div()
                    .debug_selector(|| "new-chat-error".into())
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(18.))
                    .text_color(palette.danger)
                    .child(error)
            }))
    }

    /// An image at the size of the window. A click anywhere, or Escape,
    /// closes it.
    fn render_viewer(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let media = self.viewing.as_ref()?;
        let account = self.account.as_ref()?;
        let url = media.source.as_ref()?.as_str();
        let image = self.viewer_image()?;
        let frame = self.viewer_frame(window.viewport_size());
        // While the original is on its way (or did not come), the same
        // button as on the bubble says so, in the corner.
        let state = if self.viewer_shown().is_some() {
            None
        } else {
            match self.media.file_state(url) {
                super::media::FileState::Loading => Some(Transfer::Loading {
                    fraction: self.media.file_fraction(url, media),
                }),
                super::media::FileState::Unavailable(_) if self.media.file_expired(url) => None,
                super::media::FileState::Unavailable(reason) => Some(Transfer::Retry(reason)),
                _ => None,
            }
        };
        let button = state.map(|state| {
            let (shelf, account, url) = (self.media.clone(), account.clone(), url.to_owned());
            let stop = matches!(state, Transfer::Loading { .. });
            div().absolute().bottom_4().right_4().child(transfer_button(
                "viewer-transfer",
                &state,
                palette,
                crate::settings::reduce_motion(cx),
                move |_, _, _| {
                    if stop {
                        shelf.cancel_file(&url);
                    } else {
                        shelf.retry_file(&account, &url);
                    }
                },
            ))
        });
        let (before, after) = self.viewer_neighbours();
        // The toolbar: what can be done with the picture, each with the
        // keys the registry gives it.
        let tool = |id: &'static str,
                    glyph: IconName,
                    command: Option<crate::keys::Command>,
                    hint: &'static str| {
            let hint: SharedString = match command.and_then(crate::keys::keys_label) {
                Some(keys) => format!("{hint} ({keys})").into(),
                None => hint.into(),
            };
            icon_button(id, glyph, palette).tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
            })
        };
        use crate::keys::Command as C;
        let toolbar = div()
            .debug_selector(|| "viewer-toolbar".into())
            .absolute()
            .top(px(12.))
            .left_0()
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .id("viewer-tools")
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .p(px(4.))
                    .rounded(metrics::PANEL_RADIUS())
                    .border_1()
                    .border_color(palette.elevated_border)
                    .bg(palette.elevated)
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        tool(
                            "viewer-previous",
                            IconName::ChevronLeft,
                            Some(C::ViewerPrevious),
                            "Previous picture",
                        )
                        .when(!before, |this| this.opacity(0.35))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.step_viewer(false, cx);
                        })),
                    )
                    .child(
                        tool(
                            "viewer-next",
                            IconName::ChevronRight,
                            Some(C::ViewerNext),
                            "Next picture",
                        )
                        .when(!after, |this| this.opacity(0.35))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.step_viewer(true, cx);
                        })),
                    )
                    .child(
                        tool(
                            "viewer-zoom-out",
                            IconName::ZoomOut,
                            Some(C::ZoomOut),
                            "Zoom out",
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.zoom_viewer(Some(false), window, cx);
                        })),
                    )
                    .child(
                        mono(match frame.scale {
                            Some(scale) => format!("{:.0}%", scale * 100.),
                            None => "…".to_owned(),
                        })
                        .debug_selector(|| "viewer-zoom".into())
                        .w(px(44.))
                        .text_center()
                        .text_color(palette.text_muted),
                    )
                    .child(
                        tool(
                            "viewer-zoom-in",
                            IconName::ZoomIn,
                            Some(C::ZoomIn),
                            "Zoom in",
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.zoom_viewer(Some(true), window, cx);
                        })),
                    )
                    .child(
                        tool(
                            "viewer-fit",
                            IconName::Maximize,
                            Some(C::ZoomFit),
                            "Fit the window",
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.zoom_viewer(None, window, cx);
                        })),
                    )
                    .child(
                        tool(
                            "viewer-actual",
                            IconName::Scan,
                            Some(C::ZoomActual),
                            "Actual size",
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.zoom_viewer_to(super::viewer::Zoom::At(1.), None, window, cx);
                        })),
                    )
                    .child(
                        tool("viewer-copy", IconName::Copy, Some(C::Copy), "Copy image").on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.copy_image(cx);
                            }),
                        ),
                    )
                    .child(
                        tool(
                            "viewer-save",
                            IconName::ArrowDownToLine,
                            Some(C::SaveAs),
                            "Save as…",
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.save_as(None, cx);
                        })),
                    )
                    .child(
                        tool(
                            "viewer-open",
                            IconName::ExternalLink,
                            Some(C::Open),
                            "Open with another application",
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.open_viewed(cx);
                        })),
                    )
                    .child(
                        tool("viewer-close", IconName::X, None, "Close (Esc)").on_click(
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_overlay(window, cx);
                            }),
                        ),
                    ),
            );
        Some(
            div()
                .debug_selector(|| "viewer".into())
                .relative()
                .size_full()
                .overflow_hidden()
                // A drag moves the picture, as far as it is larger than
                // its room.
                .on_mouse_move(
                    cx.listener(|this, event: &gpui_kit::MouseMoveEvent, window, cx| {
                        let last = this.viewer.pointer.replace(event.position);
                        if let (true, Some(last)) = (event.dragging(), last) {
                            this.drag_viewer(event.position - last, window, cx);
                        }
                    }),
                )
                // The wheel zooms around the pointer: away from the user
                // is closer.
                .on_scroll_wheel(cx.listener(
                    |this, event: &gpui_kit::ScrollWheelEvent, window, cx| {
                        let factor = match event.delta {
                            gpui_kit::ScrollDelta::Lines(lines) => {
                                super::viewer::WHEEL.powf(lines.y)
                            }
                            gpui_kit::ScrollDelta::Pixels(by) => {
                                super::viewer::WHEEL.powf(by.y.as_f32() / 40.)
                            }
                        };
                        this.zoom_viewer_by(factor, event.position, window, cx);
                        cx.stop_propagation();
                    },
                ))
                .child(
                    div()
                        .debug_selector(|| "viewer-area".into())
                        .absolute()
                        .left(frame.area.origin.x)
                        .top(frame.area.origin.y)
                        .w(frame.area.size.width)
                        .h(frame.area.size.height),
                )
                .child(
                    div()
                        .id("viewer-picture")
                        .debug_selector(|| "viewer-picture".into())
                        .absolute()
                        .left(frame.rect.origin.x)
                        .top(frame.rect.origin.y)
                        .w(frame.rect.size.width)
                        .h(frame.rect.size.height)
                        // A click on the picture is not a click beside
                        // it; two toggle between whole and actual size.
                        .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            if event.click_count() == 2 {
                                this.toggle_viewer_zoom(event.position(), window, cx);
                            }
                        }))
                        .child(
                            img(image)
                                .size_full()
                                .object_fit(ObjectFit::Fill)
                                .rounded(metrics::RADIUS()),
                        ),
                )
                .child(toolbar)
                .children(button)
                .text_color(palette.text),
        )
    }

    /// "Sign out?": what goes with it, and what would be lost.
    fn render_confirm_sign_out(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let unsent = self.unsent();
        self.card("confirm-sign-out", palette)
            .w(px(420.))
            .flex()
            .flex_col()
            .child(panel_header(
                "Sign out?",
                palette,
                cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
            ))
            .child(
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_size(metrics::TEXT_BODY())
                            .line_height(px(21.))
                            .text_color(palette.text_muted)
                            .child(
                                "Signing out removes the API key from your system keychain and \
                                 deletes the chats stored on this computer, with their key. \
                                 Your settings stay, and the chats come back from wuapi when \
                                 you sign in again.",
                            ),
                    )
                    .when(unsent > 0, |this| {
                        this.child(
                            div()
                                .debug_selector(|| "unsent-warning".into())
                                .p_3()
                                .rounded(metrics::RADIUS())
                                .border_1()
                                .border_color(palette.border)
                                .flex()
                                .gap_3()
                                .child(icon(IconName::TriangleAlert, px(16.), palette.danger))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(metrics::TEXT_SMALL())
                                        .line_height(px(19.))
                                        .text_color(palette.text)
                                        .child(SharedString::from(match unsent {
                                            1 => "1 message is still waiting to be sent. It \
                                                  will be lost."
                                                .to_owned(),
                                            n => format!(
                                                "{n} messages are still waiting to be sent. \
                                                 They will be lost."
                                            ),
                                        })),
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .p_4()
                    .border_t_1()
                    .border_color(palette.border)
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("sign-out-cancel", "Cancel", None, false, palette).on_click(
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_overlay(window, cx);
                            }),
                        ),
                    )
                    .child(
                        text_button(
                            "sign-out-confirm",
                            if unsent > 0 {
                                "Sign out and lose them"
                            } else {
                                "Sign out"
                            },
                            Some(IconName::LogOut),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.sign_out(cx);
                        })),
                    ),
            )
    }
}

/// The top strip of a panel: its title and the button that closes it.
pub(super) fn panel_header(
    title: &str,
    palette: &Palette,
    on_close: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> Div {
    div()
        .flex_none()
        .h(px(48.))
        .pl_4()
        .pr_2()
        .border_b_1()
        .border_color(palette.border)
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(metrics::TEXT_NAME())
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(title.to_owned())),
        )
        .child(icon_button("close-panel", IconName::X, palette).on_click(on_close))
}

/// A line of a panel: a mono label on the left, its value on the right,
/// a hairline underneath.
pub(super) fn fact(name: &str, value: SharedString, palette: &Palette) -> Div {
    div()
        .min_h(px(36.))
        .py_2()
        .border_b_1()
        .border_color(palette.border)
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(label(name, palette))
        .child(
            div()
                .min_w_0()
                .truncate()
                .text_size(metrics::TEXT_SMALL())
                .text_color(palette.text)
                .child(value),
        )
}
