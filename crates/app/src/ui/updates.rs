//! Updates in the window: the About section (the version, the channel,
//! when the source was last asked, what stands ready), the small notice
//! in the rail when a restart would install a new version, the two
//! commands ("Check for updates", "Restart to update"), and the question
//! asked of a copy that cannot update itself where it was opened from:
//! whether to move to the Applications folder.
//!
//! The window only shows what the updater last said (`crate::update`); it
//! never waits for it and nothing here opens anything by itself.

use super::menus::fact;
use super::panels::{choice_row, section};
use super::shell::{Overlay, SettingsSection, Shell};
use super::voice_record::Phase as VoicePhase;
use super::widgets::{switch, text_button};
use crate::icons::{icon, IconName};
use crate::product::{LICENCE, MAKER, PRODUCT_NAME, REPOSITORY, VERSION};
use crate::theme::{metrics, px, Palette};
use crate::update::{self, UrlFrom};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, Entity, FontWeight, SharedString, Stateful, Task, Window};
use updater::{Manual, Phase};

/// What the About section holds beside what the updater says.
pub(super) struct UpdateUi {
    /// Where another update source is typed.
    pub(super) url_input: Entity<InputState>,
    /// What saving the source said.
    pub(super) said: Option<SharedString>,
    /// "Restart now" was pressed with work under way: the warning is
    /// showing, and the next press restarts.
    pub(super) warned: bool,
    /// The advanced part (the update source) is unfolded.
    pub(super) advanced: bool,
    /// Where the move to the Applications folder stands.
    moving: Moving,
    _moving: Option<Task<()>>,
}

/// The move to the Applications folder, as the window follows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Moving {
    /// Not asked for.
    #[default]
    No,
    /// The application is being copied.
    Working,
    /// It was not moved, and why.
    Failed(SharedString),
}

impl UpdateUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let (base_url, from) = update::base_url(cx);
        let url_input = cx.new(|cx| {
            let mut field = InputState::new(window, cx).placeholder(updater::DEFAULT_BASE_URL);
            if from != UrlFrom::Default {
                field.set_value(base_url, window, cx);
            }
            field
        });
        Self {
            url_input,
            said: None,
            warned: false,
            advanced: from != UrlFrom::Default,
            moving: Moving::No,
            _moving: None,
        }
    }
}

/// An Applications folder, in words: the system's, or the user's own.
fn folder_words(folder: &std::path::Path) -> &'static str {
    if folder == std::path::Path::new("/Applications") {
        "the Applications folder"
    } else {
        "the Applications folder of your home folder"
    }
}

/// A paragraph of small, quiet text.
fn quiet(text: impl Into<SharedString>, palette: &Palette) -> Div {
    div()
        .text_size(metrics::TEXT_SMALL())
        .line_height(px(19.))
        .text_color(palette.text_muted)
        .child(text.into())
}

impl Shell {
    /// What a restart would interrupt, in words. `None` when nothing is
    /// under way.
    pub(super) fn restart_interrupts(&self) -> Option<String> {
        let mut busy = Vec::new();
        match self.unsent() {
            0 => {}
            1 => busy.push("1 message is still on its way out".to_owned()),
            n => busy.push(format!("{n} messages are still on their way out")),
        }
        if matches!(
            self.voice.phase,
            VoicePhase::Recording | VoicePhase::Preview | VoicePhase::Sending
        ) {
            busy.push("a voice note is being recorded".to_owned());
        }
        if busy.is_empty() {
            return None;
        }
        Some(format!(
            "{}. Messages that are waiting are kept and sent after the restart; a recording \
             that was not sent is lost.",
            {
                let mut said = busy.join(", and ");
                if let Some(first) = said.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                said
            }
        ))
    }

    /// "Restart to update": restarts, unless something is under way, in
    /// which case it says what and asks once more (in Settings > About).
    pub(super) fn restart_to_update(
        &mut self,
        anyway: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if update::ready_version(cx).is_none() {
            return;
        }
        if !anyway && self.restart_interrupts().is_some() {
            self.updates.warned = true;
            self.open_about(window, cx);
            return;
        }
        if let Err(error) = update::restart(cx) {
            self.show_problem(
                format!("{PRODUCT_NAME} could not be restarted: {error}"),
                cx,
            );
        }
    }

    /// Why the move to the Applications folder was not made, when it was
    /// asked for and failed.
    pub(super) fn move_failure(&self) -> Option<SharedString> {
        match &self.updates.moving {
            Moving::Failed(why) => Some(why.clone()),
            _ => None,
        }
    }

    /// "Move to Applications": the application is copied there off this
    /// thread, the copy is started, and this one quits. A move that
    /// fails says why, and has changed nothing.
    pub(super) fn move_to_applications(&mut self, cx: &mut Context<Self>) {
        let Some(offer) = update::relocation(cx) else {
            return;
        };
        if self.updates.moving == Moving::Working {
            return;
        }
        self.updates.moving = Moving::Working;
        cx.notify();
        self.updates._moving = Some(cx.spawn(async move |this, cx| {
            let run = offer.run.clone();
            let moved = cx.background_spawn(async move { run() }).await;
            this.update(cx, |this, cx| match moved {
                // The copy is waiting for this instance to end.
                Ok(()) => cx.quit(),
                Err(why) => {
                    tracing::warn!(%why, "the application was not moved to the Applications folder");
                    this.updates.moving = Moving::Failed(
                        format!(
                            "{PRODUCT_NAME} could not be moved: {}. Nothing was changed.",
                            why.trim().trim_end_matches('.')
                        )
                        .into(),
                    );
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// The question at the top of the chats, for a copy that was opened
    /// from the disk image or from where it was downloaded to: there it
    /// cannot replace itself with a new version, and in the Applications
    /// folder it can. Asked until it is answered; "Not now" is remembered.
    pub(super) fn render_move_prompt(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let offer = update::relocation(cx)?;
        if crate::settings::get(cx).move_prompt_done {
            return None;
        }
        let working = self.updates.moving == Moving::Working;
        Some(
            div()
                .flex_none()
                .debug_selector(|| "move-prompt".into())
                .px_4()
                .py_3()
                .border_b_1()
                .border_color(palette.border)
                .bg(palette.muted)
                .flex()
                .flex_col()
                .gap_2()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(18.))
                .text_color(palette.text)
                .child(SharedString::from(format!(
                    "Move {PRODUCT_NAME} to {}? From there it updates by itself.",
                    folder_words(&offer.folder)
                )))
                .children(self.move_failure().map(|why| {
                    div()
                        .debug_selector(|| "move-failed".into())
                        .text_color(palette.danger)
                        .child(why)
                }))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            text_button(
                                "move-accept",
                                if working {
                                    "Moving…"
                                } else {
                                    "Move to Applications"
                                },
                                None,
                                true,
                                palette,
                            )
                            .when(working, |this| this.opacity(0.6))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.move_to_applications(cx);
                            })),
                        )
                        .when(!working, |this| {
                            this.child(
                                text_button("move-not-now", "Not now", None, false, palette)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.stop_propagation();
                                        crate::settings::update(cx, |settings| {
                                            settings.move_prompt_done = true
                                        });
                                        cx.notify();
                                    })),
                            )
                        }),
                ),
        )
    }

    /// The same offer in Settings > About, where it stays after "Not
    /// now": in place of a link to the download page.
    fn render_move_offer(
        &self,
        offer: &update::Move,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let working = self.updates.moving == Moving::Working;
        div()
            .p_3()
            .rounded(metrics::RADIUS())
            .border_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(palette.text)
                            .child(SharedString::from(format!(
                                "Where it was opened from, {PRODUCT_NAME} cannot replace itself \
                                 with a new version. In {} it updates by itself.",
                                folder_words(&offer.folder)
                            ))),
                    )
                    .child(
                        text_button(
                            "update-move",
                            if working {
                                "Moving…"
                            } else {
                                "Move to Applications"
                            },
                            None,
                            true,
                            palette,
                        )
                        .when(working, |this| this.opacity(0.6))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.move_to_applications(cx);
                        })),
                    ),
            )
            .children(self.move_failure().map(|why| {
                div()
                    .debug_selector(|| "update-move-failed".into())
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(19.))
                    .text_color(palette.danger)
                    .child(why)
            }))
    }

    /// Settings, on About.
    pub(super) fn open_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay != Overlay::Settings {
            self.open_overlay(Overlay::Settings, window, cx);
        }
        self.settings_section = SettingsSection::About;
        cx.notify();
    }

    /// "Check for updates": asks now, and shows where the answer will be.
    pub(super) fn check_for_updates(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        update::check_now(cx);
        self.open_about(window, cx);
    }

    fn save_update_url(&mut self, cx: &mut Context<Self>) {
        let typed = self.updates.url_input.read(cx).value().trim().to_owned();
        self.updates.said = Some(match update::set_base_url(cx, &typed) {
            Ok(()) if typed.is_empty() => "Updates come from the project's releases.".into(),
            Ok(()) => "Saved. Updates are taken from this address.".into(),
            Err(why) => format!("Not saved: {why}.").into(),
        });
        cx.notify();
    }

    fn reset_update_url(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.updates
            .url_input
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.save_update_url(cx);
    }

    /// The notice in the rail: there while a restart would install a new
    /// version, and nothing otherwise. A click shows the version and
    /// what is new in it.
    pub(super) fn render_update_notice(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let version = update::ready_version(cx)?;
        let (hover, ring) = (palette.muted, palette.accent);
        let hint: SharedString = format!("Restart to update to {version}").into();
        Some(
            div()
                .id("update-ready")
                .debug_selector(|| "update-ready".into())
                .flex_none()
                .size(metrics::CONTROL())
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .tab_index(0)
                .hover(move |style| style.bg(hover))
                .focus_visible(move |style| style.border_color(ring))
                .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
                .child(icon(IconName::ArrowDownToLine, px(18.), palette.accent))
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_about(window, cx);
                })),
        )
    }

    /// Settings > About.
    pub(super) fn render_about(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let snapshot = update::snapshot(cx);
        let (base_url, url_from) = update::base_url(cx);
        let disabled = matches!(snapshot.phase, Phase::Disabled(_));
        let busy = matches!(snapshot.phase, Phase::Checking | Phase::Downloading { .. });
        let status = update::status_line(&snapshot);
        let checked = update::last_checked(&snapshot, updater::stage::now());
        // A copy that can move to the Applications folder is offered
        // that, and never the download page: the new version then comes
        // by itself.
        let relocation = update::relocation(cx);
        let (notes, ready, download) = match &snapshot.phase {
            Phase::Ready { notes, .. } => (notes.clone(), true, false),
            Phase::Available { notes, why, .. } => {
                let by_hand = match why {
                    Manual::NoBuild => false,
                    Manual::Install(_) => relocation.is_none(),
                    Manual::TooOld => true,
                };
                (notes.clone(), false, by_hand)
            }
            _ => (String::new(), false, false),
        };
        let interrupts = self.restart_interrupts();
        let warned = self.updates.warned && interrupts.is_some();

        let mut about = section("About", palette)
            .child(fact(
                "Version",
                format!("{PRODUCT_NAME} {VERSION}").into(),
                palette,
            ))
            .child(
                fact("Channel", update::CHANNEL.name().into(), palette)
                    .debug_selector(|| "about-channel".into()),
            )
            .child(
                fact("Last checked for updates", checked.into(), palette)
                    .debug_selector(|| "about-last-check".into()),
            )
            .child(fact("Licence", LICENCE.into(), palette))
            .child(fact("Built by", MAKER.into(), palette))
            .child(fact(
                "Source",
                REPOSITORY.trim_start_matches("https://").into(),
                palette,
            ));

        // ----- updates
        let mut updates = div()
            .debug_selector(|| "about-updates".into())
            .pt_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(
                        div()
                            .debug_selector(|| "update-status".into())
                            .flex_1()
                            .min_w_0()
                            .text_size(metrics::TEXT_BODY())
                            .line_height(px(21.))
                            .font_weight(if ready {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(palette.text)
                            .child(SharedString::from(status)),
                    )
                    .when(!disabled && !ready, |this| {
                        this.child(
                            text_button(
                                "update-check",
                                if busy { "Checking…" } else { "Check now" },
                                Some(IconName::RotateCw),
                                false,
                                palette,
                            )
                            .when(busy, |this| this.opacity(0.6))
                            .on_click(cx.listener(
                                move |_, _, _, cx| {
                                    cx.stop_propagation();
                                    if !busy {
                                        update::check_now(cx);
                                    }
                                },
                            )),
                        )
                    })
                    .when(ready, |this| {
                        this.child(
                            text_button(
                                "update-restart",
                                if warned {
                                    "Restart anyway"
                                } else {
                                    "Restart now"
                                },
                                Some(IconName::RotateCw),
                                true,
                                palette,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.restart_to_update(warned, window, cx);
                                },
                            )),
                        )
                    })
                    .when(download, |this| {
                        this.child(
                            text_button(
                                "update-download",
                                "Download",
                                Some(IconName::ExternalLink),
                                false,
                                palette,
                            )
                            .on_click(|_, _, cx| {
                                cx.stop_propagation();
                                cx.open_url(&update::download_page());
                            }),
                        )
                    }),
            );
        if let Some(offer) = &relocation {
            updates = updates.child(self.render_move_offer(offer, palette, cx));
        }
        if warned {
            updates = updates.child(
                div()
                    .debug_selector(|| "update-restart-warning".into())
                    .p_3()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .flex()
                    .gap_3()
                    .child(icon(IconName::TriangleAlert, px(16.), palette.warning))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(palette.text)
                            .child(SharedString::from(interrupts.unwrap_or_default())),
                    ),
            );
        }
        if !notes.trim().is_empty() {
            updates = updates.child(
                div()
                    .debug_selector(|| "update-notes".into())
                    .p_3()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .children(
                        notes
                            .lines()
                            .take(12)
                            .map(|line| quiet(line.to_owned(), palette)),
                    ),
            );
        }
        if let Some(notice) = snapshot.notice.clone() {
            updates = updates.child(
                div()
                    .debug_selector(|| "update-notice".into())
                    .p_3()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(icon(IconName::Info, px(16.), palette.icon))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(palette.text)
                            .child(SharedString::from(notice)),
                    )
                    .child(
                        text_button("update-notice-ok", "OK", None, false, palette).on_click(
                            |_, _, cx| {
                                cx.stop_propagation();
                                update::dismiss_notice(cx);
                                cx.refresh_windows();
                            },
                        ),
                    ),
            );
        }
        about = about.child(updates);

        if !disabled {
            let automatic = snapshot.automatic;
            about = about.child(choice_row(
                "Check for updates automatically",
                "A new version is downloaded in the background, checked against the \
                 project's signature, and installed the next time the application starts.",
                switch("update-automatic", automatic, palette).on_click(move |_, _, cx| {
                    cx.stop_propagation();
                    update::set_automatic(cx, !automatic);
                }),
                palette,
            ));
            about = about.child(self.render_update_source(base_url, url_from, palette, cx));
        }

        about.child(
            div()
                .pt_3()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(19.))
                        .text_color(palette.text_muted)
                        .child(
                            "An independent, open-source client. Not affiliated with \
                             WhatsApp.",
                        ),
                )
                .child(
                    text_button(
                        "open-repository",
                        "Source code",
                        Some(IconName::ExternalLink),
                        false,
                        palette,
                    )
                    .on_click(|_, _, cx| {
                        cx.stop_propagation();
                        cx.open_url(REPOSITORY);
                    }),
                ),
        )
    }

    /// The advanced part of About: where updates are taken from.
    fn render_update_source(
        &self,
        base_url: String,
        url_from: UrlFrom,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.updates.advanced;
        let head = div()
            .id("update-advanced")
            .debug_selector(|| "update-advanced".into())
            .py_2()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .text_size(metrics::TEXT_SMALL())
            .text_color(palette.text_muted)
            .child(icon(
                if open {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                },
                px(14.),
                palette.icon,
            ))
            .child("Advanced")
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.updates.advanced = !this.updates.advanced;
                cx.notify();
            }));
        let mut block = div().flex().flex_col().child(head);
        if !open {
            return block;
        }
        let state: SharedString = match (&self.updates.said, url_from) {
            (_, UrlFrom::CommandLine) => {
                format!("Set with --update-url for this run: {base_url}").into()
            }
            (Some(said), _) => said.clone(),
            (None, UrlFrom::Setting) => "Updates are taken from this address.".into(),
            (None, UrlFrom::Default) => {
                "Updates come from the project's releases. Another address (https) can be \
                 a mirror; it must serve the same signed files."
                    .into()
            }
        };
        block = block
            .child(
                div()
                    .text_size(metrics::TEXT_BODY())
                    .font_weight(FontWeight::MEDIUM)
                    .child("Update source"),
            )
            .child(
                div()
                    .pt_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .debug_selector(|| "update-url".into())
                            .flex_1()
                            .min_w_0()
                            .h(metrics::BUTTON())
                            .px_2()
                            .rounded(metrics::RADIUS())
                            .border_1()
                            .border_color(palette.border)
                            .bg(palette.surface)
                            .flex()
                            .items_center()
                            .child(Input::new(&self.updates.url_input).appearance(false)),
                    )
                    .when(url_from != UrlFrom::CommandLine, |this| {
                        this.child(
                            text_button("update-url-save", "Save", None, false, palette).on_click(
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.save_update_url(cx);
                                }),
                            ),
                        )
                    })
                    .when(url_from == UrlFrom::Setting, |this| {
                        this.child(
                            text_button("update-url-reset", "Reset", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.reset_update_url(window, cx);
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "update-url-state".into())
                    .pt_2()
                    .child(quiet(state, palette)),
            );
        block
    }
}
