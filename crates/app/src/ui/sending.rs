//! Files on their way into a chat, before they are messages: a bubble
//! the moment "Send" is pressed, while the picture is read for its
//! thumbnail and the file is copied into the store, until the message it
//! becomes is in the conversation. With a large photo or a video that
//! takes a moment, and nothing on screen in the meantime read as nothing
//! happening.

use super::shell::Shell;
use super::transfer::{transfer_button, Transfer};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_provider::{AccountId, ChatId, MediaKind};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, SharedString};

/// A file being made ready to send.
pub(super) struct Preparing {
    pub(super) id: u64,
    pub(super) account: AccountId,
    pub(super) chat: ChatId,
    pub(super) kind: MediaKind,
    pub(super) name: String,
    pub(super) caption: Option<String>,
}

/// The side of the picture's place, while there is no picture yet.
const SIDE: f32 = 220.;

impl Shell {
    /// Shows a file in its chat at once, until [`done_preparing`]
    /// (Self::done_preparing) says it became a message, or did not.
    pub(super) fn begin_preparing(
        &mut self,
        account: &AccountId,
        chat: &ChatId,
        kind: MediaKind,
        name: String,
        caption: Option<String>,
    ) -> u64 {
        self.preparing_next += 1;
        let id = self.preparing_next;
        self.preparing.push(Preparing {
            id,
            account: account.clone(),
            chat: chat.clone(),
            kind,
            name,
            caption,
        });
        id
    }

    /// The file is a message in the store now, or will not be one.
    pub(super) fn done_preparing(&mut self, id: u64, cx: &mut Context<Self>) {
        self.preparing.retain(|preparing| preparing.id != id);
        if let Some(open) = &self.open {
            open.list.scroll_to_end();
        }
        cx.notify();
    }

    /// The bubbles of the open chat's files being made ready, under its
    /// newest message.
    pub(super) fn render_preparing(&self, palette: &Palette, still: bool) -> Option<Div> {
        let open = self.open.as_ref()?;
        let here: Vec<&Preparing> = self
            .preparing
            .iter()
            .filter(|preparing| {
                preparing.chat == open.chat.id && preparing.account == open.chat.account_id
            })
            .collect();
        if here.is_empty() {
            return None;
        }
        let mut column = div()
            .debug_selector(|| "preparing".into())
            .flex_none()
            .px_4()
            .pt_2()
            .flex()
            .flex_col()
            .items_end()
            .gap_2();
        for (index, preparing) in here.into_iter().enumerate() {
            let picture = matches!(
                preparing.kind,
                MediaKind::Image | MediaKind::Video | MediaKind::Sticker
            );
            // Where the picture will be: an empty place with the wheel
            // that turns while a file is on its way.
            let wheel = transfer_button(
                ("preparing-wheel", index),
                &Transfer::Loading { fraction: None },
                palette,
                still,
                |_, _, _| {},
            );
            let place = if picture {
                div()
                    .w(px(SIDE))
                    .h(px(SIDE * 0.75))
                    .rounded(px(6.))
                    .bg(palette.quote_out)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(wheel)
            } else {
                div()
                    .w(px(SIDE))
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(wheel)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_BODY())
                            .child(SharedString::from(preparing.name.clone())),
                    )
            };
            column = column.child(
                div()
                    .debug_selector(move || format!("preparing-{index}"))
                    .max_w(metrics::BUBBLE_MAX_WIDTH())
                    .p(px(4.))
                    .rounded(metrics::BUBBLE_CORNER())
                    .border_1()
                    .border_color(palette.bubble_out_border)
                    .bg(palette.bubble_out)
                    .text_color(palette.on_bubble_out)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(place)
                    .children(preparing.caption.clone().map(|caption| {
                        div()
                            .px_1()
                            .w(px(SIDE))
                            .text_size(metrics::TEXT_BODY())
                            .child(SharedString::from(caption))
                    }))
                    .child(
                        div()
                            .px_1()
                            .text_size(metrics::TEXT_META())
                            .text_color(palette.meta_out)
                            .child(crate::i18n::t("chat.attach_sheet.preparing")),
                    ),
            );
        }
        Some(column)
    }
}
