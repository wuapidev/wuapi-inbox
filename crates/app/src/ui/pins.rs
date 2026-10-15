//! Messages pinned at the top of a chat, as on WhatsApp: for everyone in
//! it, for a day, a week or thirty days, three at most.
//!
//! "Pin" on a message asks for how long; on a pinned one it unpins at
//! once. The bar under the chat's header shows the pinned message,
//! newest pin first: a click goes to it and brings the next one up, the
//! way the phone does it.

use super::menus::panel_header;
use super::shell::{Overlay, Shell};
use super::widgets::{mono, text_button};
use crate::icons::{icon, IconName};
use crate::keys::Command;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::message_preview;
use client_provider::{Message, PinFor, Timestamp};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, SharedString, Stateful, Window};

/// A choice of the pin sheet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PinChoice {
    For(PinFor),
    Cancel,
}

pub(super) const PIN_CHOICES: [PinChoice; 4] = [
    PinChoice::For(PinFor::Day),
    PinChoice::For(PinFor::Week),
    PinChoice::For(PinFor::Month),
    PinChoice::Cancel,
];

impl PinChoice {
    fn label(self) -> &'static str {
        match self {
            Self::For(PinFor::Day) => "24 hours",
            Self::For(PinFor::Week) => "7 days",
            Self::For(PinFor::Month) => "30 days",
            Self::Cancel => "Cancel",
        }
    }

    fn selector(self) -> &'static str {
        match self {
            Self::For(PinFor::Day) => "pin-day",
            Self::For(PinFor::Week) => "pin-week",
            Self::For(PinFor::Month) => "pin-month",
            Self::Cancel => "pin-cancel",
        }
    }
}

impl Shell {
    /// "Pin" on the message in focus: unpins a pinned one at once, and
    /// asks for how long otherwise.
    pub(super) fn begin_pin(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.focused_message() else {
            return false;
        };
        if self.command_state(Command::Pin, &message) != super::message_menu::CommandState::Enabled
        {
            return false;
        }
        if message.extras.pinned_at(Timestamp::now()) {
            self.pin(&message, None);
            cx.notify();
            return true;
        }
        self.acting.subjects = vec![message];
        // WhatsApp's own choice when none is made: a week.
        self.acting.pin_cursor = 1;
        self.open_overlay(Overlay::PinMessage, window, cx);
        true
    }

    fn pin(&self, message: &Message, pin: Option<PinFor>) {
        if let Err(error) =
            self.engine
                .pin_message(&message.account_id, &message.chat_id, &message.id, pin)
        {
            tracing::error!(%error, "could not store the pin");
        }
    }

    /// A choice of the pin sheet was made.
    pub(super) fn pin_chosen(
        &mut self,
        choice: PinChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let PinChoice::For(pin) = choice {
            for message in std::mem::take(&mut self.acting.subjects) {
                self.pin(&message, Some(pin));
            }
        }
        self.acting.subjects.clear();
        self.close_overlay(window, cx);
        cx.notify();
    }

    /// The keys of the pin sheet. Answers whether one was used.
    pub(super) fn pin_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count = PIN_CHOICES.len();
        match key {
            "down" | "tab" => self.acting.pin_cursor = (self.acting.pin_cursor + 1) % count,
            "up" => self.acting.pin_cursor = (self.acting.pin_cursor + count - 1) % count,
            "enter" | "space" => {
                let choice = PIN_CHOICES[self.acting.pin_cursor.min(count - 1)];
                self.pin_chosen(choice, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn render_pin_sheet(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let mut buttons = div().flex().flex_col().gap_2();
        for (index, choice) in PIN_CHOICES.into_iter().enumerate() {
            let on = index == self.acting.pin_cursor;
            buttons = buttons.child(
                text_button(
                    choice.selector(),
                    choice.label(),
                    (choice != PinChoice::Cancel).then_some(IconName::Pin),
                    false,
                    palette,
                )
                .when(on, |this| this.border_color(palette.focus_ring))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.pin_chosen(choice, window, cx);
                })),
            );
        }
        self.card("pin-message", palette)
            .w(px(360.))
            .flex()
            .flex_col()
            .child(panel_header(
                "Pin for how long?",
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
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(palette.text_muted)
                            .child(format!(
                                "Everyone in the chat sees it at the top. A chat keeps {} \
                                 pinned messages: one more replaces the one that ends first. \
                                 You can unpin it at any time.",
                                client_core::PINS_PER_CHAT
                            )),
                    )
                    .child(buttons),
            )
    }

    /// The pinned messages of the open chat that are still pinned.
    fn pinned_now(&self) -> Vec<&Message> {
        let now = Timestamp::now();
        self.open
            .as_ref()
            .map(|open| {
                open.pinned
                    .iter()
                    .filter(|message| message.extras.pinned_at(now))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Goes to the pinned message on the bar, and puts the next one there.
    fn pinned_clicked(&mut self, cx: &mut Context<Self>) {
        let pinned: Vec<Message> = self.pinned_now().into_iter().cloned().collect();
        if pinned.is_empty() {
            return;
        }
        let Some(open) = self.open.as_mut() else {
            return;
        };
        let at = open.pinned_shown % pinned.len();
        open.pinned_shown = (at + 1) % pinned.len();
        self.jump_and_flash(pinned[at].id.clone(), cx);
    }

    /// The bar under the header with the chat's pinned message, while it
    /// has one.
    pub(super) fn render_pinned_bar(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let pinned = self.pinned_now();
        let open = self.open.as_ref()?;
        let count = pinned.len();
        let message = *pinned.get(open.pinned_shown % count.max(1))?;
        let at = open.pinned_shown % count;
        let who = match message.direction {
            client_provider::Direction::Outgoing => "You".to_owned(),
            client_provider::Direction::Incoming => message
                .sender_name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| message.sender.to_string()),
        };
        let line = format!("{who}: {}", message_preview(message));
        let caps_pins = self.caps_here().pins;
        let unpin = message.clone();
        let hover = palette.hover;
        Some(
            div()
                .debug_selector(|| "pinned-bar".into())
                .flex_none()
                .px_4()
                .py(px(6.))
                .border_b_1()
                .border_color(palette.border)
                .bg(palette.surface)
                .flex()
                .items_center()
                .gap_3()
                .child(icon(IconName::Pin, px(14.), palette.accent))
                .child(
                    div()
                        .id("pinned-open")
                        .debug_selector(|| "pinned-open".into())
                        .flex_1()
                        .min_w_0()
                        .cursor_pointer()
                        .rounded(metrics::RADIUS())
                        .hover(move |style| style.bg(hover))
                        .flex()
                        .flex_col()
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.pinned_clicked(cx);
                        }))
                        .child(
                            mono(if count > 1 {
                                format!("PINNED · {} OF {count}", at + 1)
                            } else {
                                "PINNED".to_owned()
                            })
                            .text_color(palette.text_muted),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(metrics::TEXT_SMALL())
                                .text_color(palette.text)
                                .child(SharedString::from(line)),
                        ),
                )
                .when(caps_pins, |this| {
                    this.child(
                        super::widgets::icon_button("pinned-unpin", IconName::X, palette).on_click(
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.pin(&unpin, None);
                                cx.notify();
                            }),
                        ),
                    )
                }),
        )
    }
}

/// The pin of a pinned message, for the line with the time.
pub(super) fn pinned_mark(colour: gpui_kit::Hsla) -> Div {
    div()
        .debug_selector(|| "pinned-mark".into())
        .flex_none()
        .child(icon(IconName::Pin, px(11.), colour))
}
