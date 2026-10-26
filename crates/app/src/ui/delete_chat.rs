//! Deleting a chat: the question, then the wait for the provider.
//!
//! Nothing deleted can be put back, so this one is not local first: the
//! sheet stays until the provider has deleted the chat, and says why when
//! it could not. A group the account is still in is left on the way, as
//! WhatsApp keeps the chat of a group its member is in; the sheet says so.

use super::menus::panel_header;
use super::shell::{Overlay, Shell};
use super::widgets::text_button;
use crate::icons::IconName;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::ChatSummary;
use client_provider::ChatKind;
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, SharedString, Stateful, Task, Window};

/// The chat being asked about, and the work under way.
pub(super) struct DeletingChat {
    chat: ChatSummary,
    /// Waiting for the provider.
    busy: bool,
    /// Why it could not be deleted, when it could not.
    pub(super) error: Option<SharedString>,
    _work: Option<Task<()>>,
}

impl Shell {
    /// Asks whether to delete `chat`.
    pub(super) fn begin_delete_chat(
        &mut self,
        chat: ChatSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.deleting_chat = Some(DeletingChat {
            chat,
            busy: false,
            error: None,
            _work: None,
        });
        self.open_overlay(Overlay::DeleteChat, window, cx);
    }

    /// "Delete": the provider is asked, and the chat goes once it says yes.
    pub(super) fn confirm_delete_chat(&mut self, cx: &mut Context<Self>) {
        let Some(deleting) = self.deleting_chat.as_mut() else {
            return;
        };
        if deleting.busy {
            return;
        }
        deleting.busy = true;
        deleting.error = None;
        let (account, chat) = (deleting.chat.account_id.clone(), deleting.chat.id.clone());
        let engine = self.engine.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.delete_chat(&account, &chat).await });
        let chat = deleting.chat.id.clone();
        deleting._work = Some(cx.spawn(async move |this, cx| {
            let outcome = match work.await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(error)) => Err(SharedString::from(client_core::failure_sentence(&error))),
                Err(error) => Err(SharedString::from(error.to_string())),
            };
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(()) => {
                        this.deleting_chat = None;
                        if this.overlay == Overlay::DeleteChat {
                            this.overlay = Overlay::None;
                        }
                        if this.open.as_ref().is_some_and(|open| open.chat.id == chat) {
                            this.close_chat(cx);
                        }
                    }
                    // The question stays: the button is the way to try again.
                    Err(sentence) => {
                        if let Some(deleting) = this.deleting_chat.as_mut() {
                            deleting.busy = false;
                            deleting.error = Some(sentence);
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Cancel, Escape, or a click outside.
    pub(super) fn cancel_delete_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.deleting_chat = None;
        self.close_overlay(window, cx);
    }

    pub(super) fn render_delete_chat(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<gpui_kit::Div>> {
        let deleting = self.deleting_chat.as_ref()?;
        let group = deleting.chat.kind == ChatKind::Group;
        let body = if group {
            crate::i18n::tr(
                "common.delete_chat.body_group",
                &[("name", &deleting.chat.title)],
            )
        } else {
            crate::i18n::tr("common.delete_chat.body", &[("name", &deleting.chat.title)])
        };
        let busy = deleting.busy;
        Some(
            self.card("delete-chat", palette)
                .w(px(400.))
                .flex()
                .flex_col()
                .child(panel_header(
                    crate::i18n::t("common.delete_chat.title"),
                    palette,
                    cx.listener(|this, _, window, cx| this.cancel_delete_chat(window, cx)),
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
                                .child(SharedString::from(body)),
                        )
                        .children(deleting.error.clone().map(|error| {
                            div()
                                .debug_selector(|| "delete-chat-error".into())
                                .text_size(metrics::TEXT_SMALL())
                                .line_height(px(18.))
                                .text_color(palette.danger)
                                .child(error)
                        })),
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
                            text_button(
                                "delete-chat-cancel",
                                crate::i18n::t("common.delete_chat.cancel"),
                                None,
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.cancel_delete_chat(window, cx);
                                },
                            )),
                        )
                        .child({
                            let button = text_button(
                                "delete-chat-confirm",
                                if busy {
                                    crate::i18n::t("common.delete_chat.deleting")
                                } else if group {
                                    crate::i18n::t("common.delete_chat.confirm_group")
                                } else {
                                    crate::i18n::t("common.delete_chat.confirm")
                                },
                                Some(IconName::Trash),
                                false,
                                palette,
                            )
                            .text_color(palette.danger);
                            if busy {
                                button.opacity(0.6)
                            } else {
                                button.on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.confirm_delete_chat(cx);
                                }))
                            }
                        }),
                ),
        )
    }
}
