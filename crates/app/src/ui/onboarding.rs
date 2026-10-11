//! What the window shows while it has no number: what the application
//! is, and the one thing to do first.
//!
//! It takes the place of the start screen (`conversation.rs`) for as long
//! as the list of numbers is empty, and belongs to the same family: the
//! header strip, the mark in its frame, rows split by hairlines. The
//! button opens the linking screen the rail and the settings open
//! (`numbers.rs`); nothing is linked from here.

use super::conversation::{colophon, fact_row, framed_rows, mark_frame};
use super::shell::{CloudOffer, Overlay, SessionKind, Shell, ShellEvent};
use super::widgets::{mono, reveal, tag, text_button, MarkPlay};
use crate::icons::IconName;
use crate::motion;
use crate::product::PRODUCT_NAME;
use crate::settings;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, FontWeight, KeyDownEvent, Window};

/// How a phone links a device, in the order WhatsApp has it.
const STEPS: [(&str, &str); 3] = [
    ("Step 1", "Open WhatsApp on your phone."),
    (
        "Step 2",
        "Open Linked devices, from the menu or from Settings, and tap Link a device.",
    ),
    ("Step 3", "Point the phone at the code this window shows."),
];

impl Shell {
    /// There is no number yet, and this is what the conversation pane
    /// says about it.
    pub(super) fn onboarding_showing(&self) -> bool {
        self.no_numbers() && self.open.is_none() && !self.status_active()
    }

    /// No number is linked. The demo data always has its numbers: a list
    /// that is empty there is one that has not been read yet, and the
    /// start screen stays as it is.
    pub(super) fn no_numbers(&self) -> bool {
        self.accounts.is_empty() && self.session != SessionKind::Demo
    }

    /// A number can be linked from this window: the provider takes new
    /// numbers.
    fn can_link_first_number(&self) -> bool {
        self.engine.capabilities().link_accounts
    }

    /// "Link a WhatsApp number": the linking screen, as the plus in the
    /// rail opens it.
    fn link_first_number(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_overlay(Overlay::AddNumber, window, cx);
        cx.notify();
    }

    /// Enter, while this screen is all there is and no field or button
    /// has the keyboard, is its button. Says whether the key was taken.
    pub(super) fn onboarding_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let stroke = &event.keystroke;
        if stroke.key != "enter"
            || stroke.modifiers.modified()
            || self.overlay != Overlay::None
            || !self.onboarding_showing()
            || !self.can_link_first_number()
            // With a button focused, Enter is that button's.
            || !self.focus.is_focused(window)
        {
            return false;
        }
        self.link_first_number(window, cx);
        true
    }

    /// The pane without a number: a heading, one line, the button that
    /// links one with the steps the phone takes, and the offer of the
    /// cloud where there is one.
    pub(super) fn render_onboarding(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let now = cx.background_executor().now();
        let step = |step: u32| self.entrance.reveal(step, now);
        let play = MarkPlay {
            paused: !window.is_window_active() || self.overlay != Overlay::None,
            reduced: settings::reduce_motion(cx),
            intro: self.mark_intro,
            frozen: motion::frozen_at(cx),
        };

        // As on the start screen: in a narrow pane the rows stack, and in
        // a short one what is not needed steps aside.
        let viewport = window.viewport_size();
        let pane = viewport.width - metrics::RAIL_WIDTH() - self.list_px;
        let narrow = pane < px(560.);
        let can_link = self.can_link_first_number();
        let offered = self.cloud == CloudOffer::Offered;
        // The card takes room of its own.
        let short = viewport.height < if offered { px(860.) } else { px(700.) };
        let cramped = viewport.height < if offered { px(700.) } else { px(560.) };

        let (light, status) = if self.cloud == CloudOffer::Connected {
            (palette.accent, "Wuapi Cloud connected, no numbers yet")
        } else {
            (palette.text_faint, "No numbers yet")
        };
        let line = if can_link {
            format!(
                "{PRODUCT_NAME} is a WhatsApp client for this computer. \
                 Link a number to see its chats here."
            )
        } else {
            format!(
                "{PRODUCT_NAME} is a WhatsApp client for this computer. \
                 There is no number yet, and this provider does not link one from here."
            )
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            // The strip that carries the header rule across the window.
            .child(
                div()
                    .flex_none()
                    .h(metrics::HEADER_HEIGHT())
                    .px_4()
                    .border_b_1()
                    .border_color(palette.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(reveal(
                        tag("Start", None, palette).when(narrow, |this| this.invisible()),
                        step(4),
                    ))
                    .child(reveal(
                        div().debug_selector(|| "start-status".into()).child(tag(
                            status,
                            Some(light),
                            palette,
                        )),
                        step(4),
                    )),
            )
            .child(
                div()
                    .id("onboarding-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .py_4()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .debug_selector(|| "onboarding".into())
                            .w_full()
                            .max_w(metrics::FORM_WIDTH() + px(40.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .when(!short, |this| {
                                this.child(
                                    mark_frame("onboarding-mark", play, step(0), palette).mb_6(),
                                )
                            })
                            .child(reveal(
                                div()
                                    .text_size(metrics::TEXT_DISPLAY())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(palette.text)
                                    .child("Link your first number"),
                                step(1),
                            ))
                            .child(reveal(
                                div()
                                    .pt_2()
                                    .max_w(metrics::FORM_WIDTH())
                                    .text_center()
                                    .text_size(metrics::TEXT_BODY())
                                    .line_height(px(22.))
                                    .text_color(palette.text_muted)
                                    .child(line),
                                step(2),
                            ))
                            .when(can_link, |this| {
                                this.child(reveal(
                                    div()
                                        .pt_6()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            text_button(
                                                "onboarding-link",
                                                "Link a WhatsApp number",
                                                Some(IconName::Plus),
                                                true,
                                                palette,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, window, cx| {
                                                    cx.stop_propagation();
                                                    this.link_first_number(window, cx);
                                                }),
                                            ),
                                        )
                                        .child(
                                            div()
                                                .text_color(palette.text_faint)
                                                .child(mono("Or press Enter")),
                                        ),
                                    step(3),
                                ))
                                // What the phone does, beside the button.
                                .when(!cramped, |this| {
                                    this.child(reveal(
                                        framed_rows(
                                            step(3),
                                            palette,
                                            STEPS.map(|(name, text)| {
                                                let row =
                                                    fact_row(name, text, name != "Step 1", palette);
                                                if narrow {
                                                    row.flex_col().items_start().gap_1()
                                                } else {
                                                    row
                                                }
                                            }),
                                        )
                                        .mt_6(),
                                        step(3),
                                    ))
                                })
                            })
                            .when(offered, |this| {
                                this.child(reveal(
                                    self.render_cloud_offer(narrow, palette, cx).mt_4(),
                                    step(5),
                                ))
                            }),
                    ),
            )
            // Where everything is, for whoever has not found it yet.
            .child(reveal(
                div()
                    .debug_selector(|| "keys-line".into())
                    .flex_none()
                    .pb_3()
                    .flex()
                    .justify_center()
                    .text_color(palette.text_muted)
                    .child(mono(super::hints::keys_line())),
                step(5),
            ))
            .child(reveal(colophon(palette), step(5)))
    }

    /// The card that offers the cloud. It says only what is so: a number
    /// linked to this computer is a linked device, which hears only while
    /// it runs (docs/PROVIDERS.md, "Known gaps in the native adapter").
    fn render_cloud_offer(&self, narrow: bool, palette: &Palette, cx: &mut Context<Self>) -> Div {
        div()
            .w_full()
            .px_4()
            .py_3()
            .border_1()
            .border_color(palette.border)
            .flex()
            .items_center()
            .gap_4()
            .when(narrow, |this| this.flex_col().items_start().gap_3())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(metrics::TEXT_BODY())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(palette.text)
                            .child("Wuapi Cloud"),
                    )
                    .child(
                        div()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(palette.text_muted)
                            .child(
                                "A number linked on this computer only works while this \
                                 application is open. With Wuapi Cloud it stays connected \
                                 when the computer is off, and a team shares the same numbers.",
                            ),
                    ),
            )
            .child(
                text_button(
                    "onboarding-cloud",
                    "Connect Wuapi Cloud",
                    None,
                    false,
                    palette,
                )
                .flex_none()
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.stop_propagation();
                    cx.emit(ShellEvent::ConnectCloud);
                })),
            )
    }

    /// "Connect Wuapi Cloud": the root shows its sign-in.
    pub(super) fn connect_cloud(&mut self, cx: &mut Context<Self>) {
        cx.emit(ShellEvent::ConnectCloud);
    }
}
