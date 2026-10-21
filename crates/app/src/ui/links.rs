//! WhatsApp links, followed in the window.
//!
//! A link reaches the window from the system (the command line, or
//! macOS's "open this URL"), from another start of the application
//! (`instance.rs`), or from a click on one in a message. It waits here
//! until there is a number to write from, then opens the conversation
//! with the number it names, with its text in the composer: ready to
//! send, never sent by itself. A link with a text and no number opens
//! the new chat panel, and the text goes to whoever is chosen there. A
//! group's invite is said to be not followed: joining by link is not
//! something this application does yet.
//!
//! Settings > About also says who opens `whatsapp:` links on this
//! computer, and makes it this application (`link_handler.rs`).

use super::menus::fact;
use super::panels::section;
use super::shell::{Overlay, SettingsSection, Shell};
use super::widgets::text_button;
use crate::link_handler::Handler;
use crate::theme::Palette;
use crate::wa_link::{self, WaLink};
use gpui_kit::prelude::*;
use gpui_kit::{div, App, Context, Div, Global, SharedString, Task, Window};

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
    asking: bool,
    _asking: Option<Task<()>>,
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
            Some(WaLink::Invite(_)) => {
                self.show_problem(crate::i18n::t("chat.links.invite").into(), cx)
            }
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
