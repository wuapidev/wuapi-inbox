//! WhatsApp links, followed in the window.
//!
//! A link reaches the window from the system (the command line, or
//! macOS's "open this URL"), from another start of the application
//! (`instance.rs`), or from a click on one in a message. It waits here
//! until there is a number to write from, then opens the conversation
//! with the number it names, with its text in the composer: ready to
//! send, never sent by itself. A link with a text and no number opens
//! the new chat panel, and the text goes to whoever is chosen there. A
//! group's invite shows the group first; it is joined with a click, and
//! its chat opens (a group that lets people in after an admin approves
//! says the request waits).
//!
//! Settings > About also says who opens `whatsapp:` links on this
//! computer, and makes it this application (`link_handler.rs`).

use super::menus::{fact, panel_header};
use super::panels::section;
use super::shell::{ChatFilter, Overlay, SettingsSection, Shell};
use super::widgets::text_button;
use crate::i18n;
use crate::link_handler::Handler;
use crate::theme::{metrics, px, Palette};
use crate::wa_link::{self, WaLink};
use client_provider::{AccountId, ChatId, Group, JoinOutcome};
use gpui_kit::prelude::*;
use gpui_kit::{div, App, Context, Div, FontWeight, Global, SharedString, Stateful, Task, Window};

/// What the shell keeps of the links.
#[derive(Default)]
pub(in crate::ui) struct Links {
    /// How many times a chat was opened: a link's text goes to a chat
    /// opened after it came, and to none if the panel was closed instead.
    pub(in crate::ui) chats_opened: u64,
    /// A link's text, and [`chats_opened`](Self::chats_opened) when it came.
    pub(in crate::ui) text: Option<(String, u64)>,
    /// Who opens the links, once the system was asked.
    pub(in crate::ui) handler: Option<Handler>,
    /// What the last "make it the default" said.
    pub(in crate::ui) said: Option<Result<String, String>>,
    /// The group invite on screen, if one is.
    pub(in crate::ui) invite: Option<Invite>,
    asking: bool,
    _asking: Option<Task<()>>,
    _invite: Option<Task<()>>,
}

/// A group's invite, shown before it is joined.
pub(in crate::ui) struct Invite {
    account: AccountId,
    code: String,
    /// The group, once WhatsApp said which it is.
    pub(in crate::ui) group: Option<Group>,
    /// The account is in it already: the button opens its chat.
    pub(in crate::ui) member: bool,
    /// Why it could not be read, or joined.
    pub(in crate::ui) error: Option<SharedString>,
    joining: bool,
    /// The request to join waits for an admin.
    pub(in crate::ui) pending: bool,
    /// The same for every attempt to join with this invite.
    request_id: String,
}

/// Links that came before there was a number to follow them with.
#[derive(Default)]
struct Waiting(Vec<String>);

impl Global for Waiting {}

/// What asks the system who opens the links, and makes it this
/// application. Tests put their own in place, so that they never change
/// the computer they run on.
#[derive(Clone, Copy)]
pub(in crate::ui) struct LinkSystem {
    pub(in crate::ui) current: fn() -> Handler,
    pub(in crate::ui) make_default: fn() -> Result<String, String>,
}

impl Global for LinkSystem {}

fn system(cx: &App) -> LinkSystem {
    if let Some(system) = cx.try_global::<LinkSystem>() {
        return *system;
    }
    #[cfg(not(test))]
    return LinkSystem {
        current: crate::link_handler::current,
        make_default: crate::link_handler::make_default,
    };
    #[cfg(test)]
    LinkSystem {
        current: || Handler::Unknown("not asked in tests".into()),
        make_default: || Err("not in tests".into()),
    }
}

/// A link for the window, or a start without one: either way the window
/// comes to the front. The link is followed at its next frame, or once
/// there is a number to follow it with.
pub fn receive_link(link: Option<String>, cx: &mut App) {
    if let Some(link) = link {
        cx.default_global::<Waiting>().0.push(link);
    }
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, _| window.activate_window());
    }
    cx.activate(true);
    cx.refresh_windows();
}

impl Shell {
    /// At each frame: links that wait are followed, a link's text goes
    /// to the chat it opened, and Settings > About asks who opens links.
    pub(super) fn tend_links(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let waiting = self.account.is_some()
            && cx
                .try_global::<Waiting>()
                .is_some_and(|waiting| !waiting.0.is_empty());
        if waiting {
            let links = std::mem::take(&mut cx.global_mut::<Waiting>().0);
            cx.defer_in(window, move |this, window, cx| {
                for link in links {
                    this.follow_wa_link(&link, window, cx);
                }
            });
        }
        if self.links.text.is_some() && self.overlay == Overlay::None {
            cx.defer_in(window, |this, window, cx| this.place_link_text(window, cx));
        }
        if self.overlay == Overlay::Settings
            && self.settings_section == SettingsSection::About
            && self.links.handler.is_none()
            && !self.links.asking
        {
            self.ask_link_handler(cx);
        }
    }

    /// Follows one link.
    pub(super) fn follow_wa_link(
        &mut self,
        link: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match wa_link::parse(link) {
            None => self.show_problem(crate::i18n::t("chat.links.cannot_open").into(), cx),
            Some(WaLink::Invite(code)) => match self.account.clone() {
                Some(account) if self.caps_here().group_join => {
                    self.read_invite(account, code, window, cx)
                }
                _ => self.show_problem(crate::i18n::t("chat.links.invite").into(), cx),
            },
            Some(WaLink::Chat { phone, text }) => {
                self.open_overlay(Overlay::NewChat, window, cx);
                self.links.text = text.map(|text| (text, self.links.chats_opened));
                // The number is checked as if typed in the panel: what
                // goes wrong is said there.
                if let Some(phone) = phone {
                    let typed = format!("+{phone}");
                    self.new_chat
                        .update(cx, |field, cx| field.set_value(typed.clone(), window, cx));
                    self.open_number(typed, cx);
                }
            }
        }
    }

    /// Shows the group an invite is for, read from WhatsApp.
    fn read_invite(
        &mut self,
        account: AccountId,
        code: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.links.invite = Some(Invite {
            account: account.clone(),
            code: code.clone(),
            group: None,
            member: false,
            error: None,
            joining: false,
            pending: false,
            request_id: client_core::new_client_id().to_string(),
        });
        self.open_overlay(Overlay::JoinGroup, window, cx);
        let engine = self.engine.clone();
        let work = self.engine.runtime().spawn(async move {
            let group = engine.invite_info(&account, &code).await?;
            let store = engine.store();
            let chat = store.chat(&account, &group.id).ok().flatten();
            let departed = store
                .group(&account, &group.id)
                .ok()
                .flatten()
                .is_some_and(|stored| stored.departed);
            Ok::<_, client_core::SyncError>((group, chat.is_some() && !departed))
        });
        self.links._invite = Some(cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                let Some(invite) = this.links.invite.as_mut() else {
                    return;
                };
                match outcome {
                    Ok(Ok((group, member))) => {
                        invite.group = Some(group);
                        invite.member = member;
                    }
                    Ok(Err(error)) => {
                        invite.error = Some(client_core::failure_sentence(&error).into())
                    }
                    Err(error) => invite.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// "Join" on the invite's card: joins the group and opens its chat,
    /// or opens it at once when the account is in it already.
    pub(super) fn join_invite(&mut self, cx: &mut Context<Self>) {
        let Some(invite) = self.links.invite.as_mut() else {
            return;
        };
        let Some(group) = invite.group.clone() else {
            return;
        };
        if invite.joining || invite.pending {
            return;
        }
        if invite.member {
            return self.enter_group(group.id, cx);
        }
        invite.joining = true;
        invite.error = None;
        let engine = self.engine.clone();
        let (account, code, request_id) = (
            invite.account.clone(),
            invite.code.clone(),
            invite.request_id.clone(),
        );
        let work = self.engine.runtime().spawn(async move {
            engine
                .join_group(&account, &code, &group.subject, &request_id)
                .await
        });
        self.links._invite = Some(cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                let Some(invite) = this.links.invite.as_mut() else {
                    return;
                };
                invite.joining = false;
                match outcome {
                    Ok(Ok(JoinOutcome::Joined(chat))) => this.enter_group(chat, cx),
                    Ok(Ok(JoinOutcome::Pending)) => invite.pending = true,
                    Ok(Err(error)) => {
                        invite.error = Some(client_core::failure_sentence(&error).into())
                    }
                    Err(error) => invite.error = Some(error.to_string().into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// The card closes and the group's chat opens.
    fn enter_group(&mut self, chat: ChatId, cx: &mut Context<Self>) {
        self.links.invite = None;
        self.overlay = Overlay::None;
        self.filter = ChatFilter::All;
        self.reload_chats(cx);
        self.open_chat(chat, None, cx);
        cx.notify();
    }

    /// The invite's card: the group, and the button that joins it.
    pub(super) fn render_join_group(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let invite = self.links.invite.as_ref()?;
        let muted = |text: SharedString| {
            div()
                .text_size(metrics::TEXT_BODY())
                .line_height(px(21.))
                .text_color(palette.text_muted)
                .child(text)
        };
        let mut body = div().p_4().flex().flex_col().gap_2();
        if let Some(group) = &invite.group {
            body = body
                .child(
                    div()
                        .debug_selector(|| "invite-subject".into())
                        .text_size(metrics::TEXT_NAME())
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(SharedString::from(group.subject.clone())),
                )
                .when(!group.participants.is_empty(), |this| {
                    this.child(muted(
                        i18n::plural(
                            "chat.links.join_members",
                            group.participants.len() as u64,
                            &[],
                        )
                        .into(),
                    ))
                })
                .when_some(group.description.clone(), |this, description| {
                    this.child(muted(description.into()).line_clamp(4))
                });
        } else if invite.error.is_none() {
            body = body.child(muted(i18n::t("chat.links.join_reading").into()));
        }
        if invite.pending {
            body = body.child(
                div()
                    .debug_selector(|| "invite-pending".into())
                    .text_color(palette.text)
                    .child(i18n::t("chat.links.join_pending")),
            );
        }
        if let Some(error) = &invite.error {
            body = body.child(
                div()
                    .debug_selector(|| "invite-error".into())
                    .text_color(palette.danger)
                    .child(error.clone()),
            );
        }
        let ready = invite.group.is_some() && !invite.joining && !invite.pending;
        let mut buttons = div()
            .p_4()
            .border_t_1()
            .border_color(palette.border)
            .flex()
            .justify_end()
            .gap_2()
            .child(
                text_button(
                    "invite-cancel",
                    if invite.pending {
                        i18n::t("chat.links.join_close")
                    } else {
                        i18n::t("chat.links.join_cancel")
                    },
                    None,
                    false,
                    palette,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_overlay(window, cx);
                })),
            );
        if !invite.pending {
            buttons = buttons.child(
                text_button(
                    "invite-join",
                    if invite.member {
                        i18n::t("chat.links.join_open")
                    } else {
                        i18n::t("chat.links.join")
                    },
                    None,
                    true,
                    palette,
                )
                .when(!ready, |this| this.opacity(0.5))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.join_invite(cx);
                })),
            );
        }
        Some(
            self.card("join-group", palette)
                .w(px(420.))
                .flex()
                .flex_col()
                .child(panel_header(
                    i18n::t("chat.links.join_title"),
                    palette,
                    cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
                ))
                .child(body)
                .child(buttons),
        )
    }

    /// The panel closed: its text goes to the chat it opened, if one was.
    fn place_link_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay != Overlay::None {
            return;
        }
        let Some((text, at)) = self.links.text.take() else {
            return;
        };
        if self.links.chats_opened == at || self.open.is_none() {
            return;
        }
        self.composer.update(cx, |composer, cx| {
            composer.set_value(text, window, cx);
            composer.focus(window, cx);
        });
        cx.notify();
    }

    fn ask_link_handler(&mut self, cx: &mut Context<Self>) {
        let system = system(cx);
        self.links.asking = true;
        let asked = cx.background_spawn(async move { (system.current)() });
        self.links._asking = Some(cx.spawn(async move |this, cx| {
            let handler = asked.await;
            this.update(cx, |this, cx| {
                this.links.asking = false;
                this.links.handler = Some(handler);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Makes this application the one that opens WhatsApp links.
    pub(super) fn make_link_default(&mut self, cx: &mut Context<Self>) {
        if self.links.asking {
            return;
        }
        let system = system(cx);
        self.links.asking = true;
        self.links.said = None;
        let made = cx.background_spawn(async move {
            let said = (system.make_default)();
            (said, (system.current)())
        });
        self.links._asking = Some(cx.spawn(async move |this, cx| {
            let (said, handler) = made.await;
            this.update(cx, |this, cx| {
                this.links.asking = false;
                this.links.handler = Some(handler);
                this.links.said = Some(said);
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Settings > About: who opens WhatsApp links, and the button.
    pub(super) fn render_link_setting(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let line: SharedString = match &self.links.handler {
            _ if self.links.asking => crate::i18n::t("chat.links.asking").into(),
            Some(handler) => handler.line().into(),
            None => crate::i18n::t("chat.links.asking").into(),
        };
        let ours = self.links.handler == Some(Handler::Us);
        let mut part = section(crate::i18n::t("chat.links.section"), palette)
            .debug_selector(|| "links-setting".into())
            .child(
                fact(crate::i18n::t("chat.links.opened_with"), line, palette)
                    .debug_selector(|| "links-handler".into()),
            );
        if let Some(said) = &self.links.said {
            let (text, colour) = match said {
                Ok(text) => (text.clone(), palette.text_muted),
                Err(text) => (text.clone(), palette.danger),
            };
            part = part.child(
                div()
                    .debug_selector(|| "links-said".into())
                    .pt_2()
                    .text_color(colour)
                    .child(SharedString::from(text)),
            );
        }
        if !ours {
            part = part.child(
                div().pt_3().flex().child(
                    text_button(
                        "links-make-default",
                        crate::i18n::t("chat.links.make_default"),
                        None,
                        false,
                        palette,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.make_link_default(cx);
                    })),
                ),
            );
        }
        part
    }
}
