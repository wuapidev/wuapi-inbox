//! A chat's incoming webhooks: URLs that send a WhatsApp message to it
//! when something posts to them, as the dashboard makes under
//! Integrations. Opened from a chat's menu, for a number whose provider
//! has them (Wuapi Cloud).
//!
//! The hooks are read when the sheet opens and held only while it is
//! open: a URL is a credential, so it is never written to the store.

use super::menus::panel_header;
use super::numbers::{error_line, input_box, said, small_button};
use super::shell::{Overlay, Shell};
use super::social::{hint, question};
use super::widgets::{icon_button, label, mono, text_button};
use crate::i18n;
use crate::icons::IconName;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::{ChatSummary, SyncError};
use client_provider::{AccountId, ChatId, IncomingWebhook, IncomingWebhookChange, ProviderError};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, ClipboardItem, Context, Div, Entity, FontWeight, SharedString, Stateful, Subscription,
    Task, Window,
};

/// What is asked before it is done: both end a URL someone may be using.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum HookAsk {
    NewUrl,
    Delete,
}

/// What is asked of the provider.
enum HookJob {
    List,
    Create(String),
    Change(String, IncomingWebhookChange),
    Delete(String),
}

/// What came back.
enum HookDone {
    Listed(Vec<IncomingWebhook>),
    Made(IncomingWebhook),
    Changed(IncomingWebhook),
    Deleted(String),
}

/// A hook's name being changed, in a field of its own.
struct Renaming {
    id: String,
    field: Entity<InputState>,
    _subscription: Subscription,
}

/// The sheet of a chat's incoming webhooks.
pub(in crate::ui) struct HooksSheet {
    account: AccountId,
    chat: ChatId,
    title: String,
    /// `None` until the provider has answered.
    hooks: Option<Vec<IncomingWebhook>>,
    /// The name of the next one.
    name: Entity<InputState>,
    /// The hook a question is open on, and the question.
    ask: Option<(String, HookAsk)>,
    /// The hook whose name is being changed.
    renaming: Option<Renaming>,
    busy: bool,
    error: Option<SharedString>,
    /// What was just done ("URL copied.").
    note: Option<SharedString>,
    _subscription: Subscription,
}

/// Whether a chat can be given incoming webhooks.
pub(in crate::ui) fn hooks_offered(shell: &Shell, chat: &ChatSummary) -> bool {
    shell
        .engine
        .capabilities_of(&chat.account_id)
        .incoming_webhooks
}

impl Shell {
    /// Opens the sheet of `chat`'s incoming webhooks, and reads them.
    pub(in crate::ui) fn open_hooks(
        &mut self,
        chat: &ChatSummary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = cx
            .new(|cx| InputState::new(window, cx).placeholder(i18n::t("chat.hooks.name_example")));
        let subscription =
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.create_hook(window, cx);
                }
            });
        self.hooks = Some(HooksSheet {
            account: chat.account_id.clone(),
            chat: chat.id.clone(),
            title: chat.title.clone(),
            hooks: None,
            name: name.clone(),
            ask: None,
            renaming: None,
            busy: false,
            error: None,
            note: None,
            _subscription: subscription,
        });
        self.overlay = Overlay::Hooks;
        name.update(cx, |field, cx| field.focus(window, cx));
        self.hook_work(HookJob::List, window, cx);
    }

    pub(in crate::ui) fn close_hooks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // `close_overlay` lets the URLs go.
        self.close_overlay(window, cx);
    }

    /// "Create webhook": one more URL for the chat, named as typed.
    pub(in crate::ui) fn create_hook(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.hooks.as_mut() else {
            return;
        };
        let typed = sheet.name.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            sheet.error = Some(i18n::t("chat.hooks.name_missing").into());
            return cx.notify();
        }
        if typed.chars().count() > 60 {
            sheet.error = Some(i18n::t("chat.hooks.name_long").into());
            return cx.notify();
        }
        self.hook_work(HookJob::Create(typed), window, cx);
    }

    fn change_hook(
        &mut self,
        id: String,
        change: IncomingWebhookChange,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.hook_work(HookJob::Change(id, change), window, cx);
    }

    /// "Rename": the hook's name turns into a field, Enter saves it.
    fn rename_hook(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.hooks.as_mut() else {
            return;
        };
        let current = sheet
            .hooks
            .iter()
            .flatten()
            .find(|hook| hook.id == id)
            .map(|hook| hook.name.clone())
            .unwrap_or_default();
        let field = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::t("chat.hooks.name_placeholder"))
        });
        field.update(cx, |field, cx| {
            field.set_value(current, window, cx);
            field.focus(window, cx);
        });
        let subscription =
            cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_rename(window, cx);
                }
            });
        sheet.ask = None;
        sheet.error = None;
        sheet.note = None;
        sheet.renaming = Some(Renaming {
            id,
            field,
            _subscription: subscription,
        });
        cx.notify();
    }

    fn save_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.hooks.as_mut() else {
            return;
        };
        let Some(renaming) = sheet.renaming.as_ref() else {
            return;
        };
        let typed = renaming.field.read(cx).value().trim().to_owned();
        let id = renaming.id.clone();
        if typed.is_empty() {
            sheet.error = Some(i18n::t("chat.hooks.name_missing").into());
            return cx.notify();
        }
        if typed.chars().count() > 60 {
            sheet.error = Some(i18n::t("chat.hooks.name_long").into());
            return cx.notify();
        }
        let unchanged = sheet
            .hooks
            .iter()
            .flatten()
            .any(|hook| hook.id == id && hook.name == typed);
        if unchanged {
            sheet.renaming = None;
            return cx.notify();
        }
        self.change_hook(id, IncomingWebhookChange::Rename(typed), window, cx);
    }

    fn cancel_rename(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = self.hooks.as_mut() {
            sheet.renaming = None;
            sheet.error = None;
            cx.notify();
        }
    }

    fn ask_hook(&mut self, id: String, ask: HookAsk, cx: &mut Context<Self>) {
        if let Some(sheet) = self.hooks.as_mut() {
            sheet.ask = Some((id, ask));
            sheet.note = None;
            cx.notify();
        }
    }

    fn answer_hook(&mut self, yes: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((id, ask)) = self.hooks.as_mut().and_then(|sheet| sheet.ask.take()) else {
            return;
        };
        match (yes, ask) {
            (false, _) => cx.notify(),
            (true, HookAsk::NewUrl) => {
                self.change_hook(id, IncomingWebhookChange::RotateUrl, window, cx)
            }
            (true, HookAsk::Delete) => self.hook_work(HookJob::Delete(id), window, cx),
        }
    }

    fn copy_hook_url(&mut self, url: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(url));
        if let Some(sheet) = self.hooks.as_mut() {
            sheet.note = Some(i18n::t("chat.hooks.copied").into());
            sheet.error = None;
        }
        cx.notify();
    }

    /// Asks the provider, off this thread, and puts the answer in the
    /// sheet if it is still open on the same chat.
    fn hook_work(&mut self, job: HookJob, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.hooks.as_mut() else {
            return;
        };
        if sheet.busy {
            return;
        }
        sheet.busy = true;
        sheet.error = None;
        sheet.note = None;
        let (account, chat) = (sheet.account.clone(), sheet.chat.clone());
        let engine = self.engine.clone();
        let (asked_account, asked_chat) = (account.clone(), chat.clone());
        let work = self.engine.runtime().spawn(async move {
            match job {
                HookJob::List => engine
                    .incoming_webhooks(&account, &chat)
                    .await
                    .map(HookDone::Listed),
                HookJob::Create(name) => engine
                    .create_incoming_webhook(&account, &chat, &name)
                    .await
                    .map(HookDone::Made),
                HookJob::Change(id, change) => engine
                    .update_incoming_webhook(&account, &id, change)
                    .await
                    .map(HookDone::Changed),
                HookJob::Delete(id) => engine
                    .delete_incoming_webhook(&account, &id)
                    .await
                    .map(|()| HookDone::Deleted(id)),
            }
        });
        let task: Task<()> = cx.spawn_in(window, async move |this, cx| {
            let outcome = work.await;
            this.update_in(cx, |this, window, cx| {
                let Some(sheet) = this
                    .hooks
                    .as_mut()
                    .filter(|sheet| sheet.account == asked_account && sheet.chat == asked_chat)
                else {
                    return;
                };
                sheet.busy = false;
                match outcome {
                    Ok(Ok(done)) => {
                        let hooks = sheet.hooks.get_or_insert_with(Vec::new);
                        match done {
                            HookDone::Listed(listed) => *hooks = listed,
                            HookDone::Made(hook) => {
                                hooks.push(hook);
                                sheet.note = Some(i18n::t("chat.hooks.ready").into());
                                sheet
                                    .name
                                    .update(cx, |field, cx| field.set_value("", window, cx));
                            }
                            HookDone::Changed(hook) => {
                                // A name that was saved closes its field.
                                if sheet
                                    .renaming
                                    .as_ref()
                                    .is_some_and(|renaming| renaming.id == hook.id)
                                {
                                    sheet.renaming = None;
                                }
                                if let Some(known) =
                                    hooks.iter_mut().find(|known| known.id == hook.id)
                                {
                                    *known = hook;
                                }
                            }
                            HookDone::Deleted(id) => hooks.retain(|known| known.id != id),
                        }
                    }
                    Ok(Err(SyncError::Provider(ProviderError::Unsupported(_)))) => {
                        sheet.error = Some(i18n::t("chat.hooks.unsupported").into())
                    }
                    Ok(Err(error)) => sheet.error = Some(said(&error)),
                    Err(error) => sheet.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        });
        self._hooks_work = Some(task);
        cx.notify();
    }

    pub(in crate::ui) fn render_hooks(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let sheet = self.hooks.as_ref()?;
        let about = hint(
            i18n::tr("chat.hooks.about", &[("title", &sheet.title)]),
            palette,
        );
        let list: Div = match &sheet.hooks {
            None if sheet.busy => hint(i18n::t("chat.hooks.reading"), palette),
            None => hint(i18n::t("chat.hooks.unreadable"), palette),
            Some(hooks) if hooks.is_empty() => {
                hint(i18n::t("chat.hooks.none"), palette).debug_selector(|| "hooks-none".into())
            }
            Some(hooks) => {
                let mut rows = div()
                    .rounded(metrics::PANEL_RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.surface)
                    .overflow_hidden()
                    .flex()
                    .flex_col();
                for (index, hook) in hooks.iter().enumerate() {
                    rows = rows.child(self.render_hook(index, hook, sheet, palette, cx));
                }
                rows
            }
        };
        let create = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(label(i18n::t("chat.hooks.new"), palette))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(input_box(
                        "hook-name",
                        &sheet.name,
                        false,
                        palette,
                    )))
                    .child(
                        text_button(
                            "hook-create",
                            if sheet.busy {
                                i18n::t("chat.hooks.working")
                            } else {
                                i18n::t("chat.hooks.create")
                            },
                            Some(IconName::Plus),
                            true,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.create_hook(window, cx);
                        })),
                    ),
            );
        Some(
            self.card("hooks", palette)
                .w(px(480.))
                .max_h(px(620.))
                .flex()
                .flex_col()
                .child(panel_header(
                    i18n::t("chat.hooks.title"),
                    palette,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_hooks(window, cx);
                    }),
                ))
                .child(
                    div()
                        .id("hooks-body")
                        .overflow_y_scroll()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(about)
                        .child(list)
                        .child(create)
                        .children(
                            sheet
                                .error
                                .clone()
                                .map(|error| error_line("hooks-error", error, palette)),
                        )
                        .children(sheet.note.clone().map(|note| {
                            hint(note, palette).debug_selector(|| "hooks-note".into())
                        })),
                ),
        )
    }

    /// One hook: its name and state, its URL with Copy, and what can be
    /// done to it.
    fn render_hook(
        &self,
        index: usize,
        hook: &IncomingWebhook,
        sheet: &HooksSheet,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let (id, url, enabled) = (hook.id.clone(), hook.url.clone(), hook.enabled);
        let (pause_id, rotate_id, delete_id) = (id.clone(), id.clone(), id.clone());
        let rename_id = id.clone();
        let renaming = sheet.renaming.as_ref().filter(|renaming| renaming.id == id);
        let name: Div = match renaming {
            // The name in a field, with what saves it and what leaves it.
            Some(renaming) => div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().min_w_0().child(input_box(
                    "hook-rename-field",
                    &renaming.field,
                    false,
                    palette,
                )))
                .child(
                    small_button(
                        ("hook-rename-save", index),
                        format!("hook-{index}-rename-save"),
                        i18n::t("chat.hooks.save"),
                        palette,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.save_rename(window, cx);
                    })),
                )
                .child(
                    small_button(
                        ("hook-rename-cancel", index),
                        format!("hook-{index}-rename-cancel"),
                        i18n::t("chat.hooks.cancel"),
                        palette,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.cancel_rename(cx);
                    })),
                ),
            None => div()
                .debug_selector(move || format!("hook-{index}-name"))
                .flex_1()
                .min_w_0()
                .font_weight(FontWeight::MEDIUM)
                .text_size(metrics::TEXT_BODY())
                .child(hook.name.clone()),
        };
        let mut row = div()
            .debug_selector(move || format!("hook-{index}"))
            .px_3()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .when(index > 0, |this| {
                this.border_t_1().border_color(palette.border)
            })
            .child(
                div().flex().items_center().gap_2().child(name).child(
                    mono(if enabled {
                        i18n::t("chat.hooks.active")
                    } else {
                        i18n::t("chat.hooks.paused")
                    })
                    .debug_selector(move || format!("hook-{index}-state"))
                    .text_color(if enabled {
                        palette.text_muted
                    } else {
                        palette.danger
                    }),
                ),
            )
            .child(
                // The URL in a well of its own, Copy at its end.
                div()
                    .pl_3()
                    .pr_1()
                    .py_1()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.background)
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div().flex_1().min_w_0().overflow_hidden().child(
                            mono(url.clone())
                                .debug_selector(move || format!("hook-{index}-url"))
                                .text_color(palette.text),
                        ),
                    )
                    .child(
                        icon_button("hook-copy", IconName::Copy, palette)
                            .id(("hook-copy", index))
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                                    "chat.hooks.copy_url",
                                ))
                                .build(window, cx)
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.copy_hook_url(url.clone(), cx);
                            })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        small_button(
                            ("hook-rename", index),
                            format!("hook-{index}-rename"),
                            i18n::t("chat.hooks.rename"),
                            palette,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.rename_hook(rename_id.clone(), window, cx);
                            },
                        )),
                    )
                    .child(
                        small_button(
                            ("hook-pause", index),
                            format!("hook-{index}-pause"),
                            if enabled {
                                i18n::t("chat.hooks.pause")
                            } else {
                                i18n::t("chat.hooks.resume")
                            },
                            palette,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.change_hook(
                                    pause_id.clone(),
                                    IncomingWebhookChange::Enabled(!enabled),
                                    window,
                                    cx,
                                );
                            },
                        )),
                    )
                    .child(
                        small_button(
                            ("hook-rotate", index),
                            format!("hook-{index}-rotate"),
                            i18n::t("chat.hooks.new_url"),
                            palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.ask_hook(rotate_id.clone(), HookAsk::NewUrl, cx);
                        })),
                    )
                    .child(
                        small_button(
                            ("hook-delete", index),
                            format!("hook-{index}-delete"),
                            i18n::t("chat.hooks.delete"),
                            palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.ask_hook(delete_id.clone(), HookAsk::Delete, cx);
                        })),
                    ),
            );
        if let Some((_, ask)) = sheet.ask.as_ref().filter(|(asked, _)| *asked == id) {
            let (words, yes) = match ask {
                HookAsk::NewUrl => (
                    i18n::t("chat.hooks.ask_new_url"),
                    i18n::t("chat.hooks.new_url"),
                ),
                HookAsk::Delete => (
                    i18n::t("chat.hooks.ask_delete"),
                    i18n::t("chat.hooks.delete"),
                ),
            };
            row = row.child(
                question("hook-question", words, palette)
                    .mx_0()
                    .my_0()
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap_2()
                            .child(
                                small_button(
                                    "hook-no",
                                    "hook-question-cancel".into(),
                                    i18n::t("chat.hooks.cancel"),
                                    palette,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.answer_hook(false, window, cx);
                                    },
                                )),
                            )
                            .child(
                                small_button(
                                    "hook-yes",
                                    "hook-question-confirm".into(),
                                    yes,
                                    palette,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.answer_hook(true, window, cx);
                                    },
                                )),
                            ),
                    ),
            );
        }
        row
    }
}
