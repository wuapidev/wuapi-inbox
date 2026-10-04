//! The settings panel: account, appearance, notifications, about.
//!
//! Everything in it is real or says that it is not: the account comes from
//! the provider and the store, the choices are saved to `settings.json`
//! (see `settings.rs`) the moment they are made, and the notifications
//! section says that nothing is delivered yet.

use super::connection::connection_words;
use super::menus::{fact, panel_header};
use super::shell::{IdentityState, Overlay, SessionKind, SettingsSection, Shell};
use super::widgets::{label, led, mono, segmented, status_colour, switch, text_button};
use crate::icons::IconName;
use crate::product::MAKER;
use crate::settings::{
    self, HistoryChoice, MediaChoice, MotionChoice, ThemeChoice, WallpaperChoice,
};
use crate::theme::px;
use crate::theme::BubbleStyle;
use crate::theme::{metrics, Palette};
use client_provider::ServerMedia;
use gpui_kit::prelude::*;
use gpui_kit::{div, App, Context, Div, FontWeight, SharedString, Stateful};

impl Shell {
    pub(super) fn render_settings(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let content = match self.settings_section {
            SettingsSection::Account => self.render_account(palette, cx),
            SettingsSection::Sync => self.render_sync(palette, cx),
            SettingsSection::Appearance => render_appearance(palette, cx),
            SettingsSection::Notifications => render_notifications(palette, cx),
            SettingsSection::Audio => self.render_audio(palette, cx),
            SettingsSection::Stickers => self.render_stickers_settings(palette, cx),
            SettingsSection::Keyboard => render_keyboard(palette, cx),
            SettingsSection::Status => self.render_status_settings(palette, cx),
            SettingsSection::About => self.render_about(palette, cx),
        };
        let mut nav = div()
            .flex_none()
            .w(px(168.))
            .p_2()
            .border_r_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .gap(px(2.));
        for (index, (section, name, selector)) in [
            (SettingsSection::Account, "Account", "settings-account"),
            (SettingsSection::Sync, "Sync", "settings-sync"),
            (
                SettingsSection::Appearance,
                "Appearance",
                "settings-appearance",
            ),
            (
                SettingsSection::Notifications,
                "Notifications",
                "settings-notifications",
            ),
            (SettingsSection::Audio, "Audio", "settings-audio"),
            (
                SettingsSection::Stickers,
                "Stickers and GIFs",
                "settings-stickers",
            ),
            (SettingsSection::Keyboard, "Keyboard", "settings-keyboard"),
            (SettingsSection::Status, "Status", "settings-status"),
            (SettingsSection::About, "About", "settings-about"),
        ]
        .into_iter()
        .enumerate()
        {
            let active = self.settings_section == section;
            let (hover, ring) = (palette.hover, palette.accent);
            nav = nav.child(
                div()
                    .id(("settings-section", index))
                    .debug_selector(move || selector.into())
                    .h(metrics::CONTROL())
                    .px_3()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(metrics::TEXT_BODY())
                    .tab_index(0)
                    .focus_visible(move |style| style.border_color(ring))
                    .map(|this| {
                        if active {
                            this.bg(palette.muted)
                                .text_color(palette.text)
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            this.text_color(palette.text_muted)
                                .hover(move |style| style.bg(hover))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.settings_section = section;
                        cx.notify();
                    }))
                    .child(name),
            );
        }
        self.card("settings-panel", palette)
            .w(px(680.))
            // Tall enough for the longest section, Appearance, without
            // scrolling.
            .h(px(700.))
            .flex()
            .flex_col()
            .child(panel_header(
                "Settings",
                palette,
                cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
            ))
            .child(
                div().flex_1().min_h_0().flex().child(nav).child(
                    div()
                        .id("settings-scroll")
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_y_scroll()
                        .child(content),
                ),
            )
    }

    /// How much message history is fetched without being asked.
    fn render_sync(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let chosen = settings::get(cx);
        let view = cx.entity().downgrade();
        let pick = move |change: Box<dyn FnOnce(&mut settings::Settings)>, cx: &mut App| {
            view.update(cx, |this, cx| this.set_history(change, cx))
                .ok();
        };
        let (pick_mode, pick_count) = (pick.clone(), pick);
        let detail = match chosen.history {
            HistoryChoice::OnOpen => {
                "The chat list loads on its own. A chat's messages are fetched when you \
                 open it, and older ones as you scroll back."
            }
            HistoryChoice::Recent => {
                "The latest messages of your most recent chats are fetched in the \
                 background, so they are there before you open them."
            }
            HistoryChoice::Everything => {
                "Every chat is fetched all the way back, in the background. Slow on a large \
                 account; search then covers everything."
            }
        };
        section("Sync", palette)
            .child(
                div()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(metrics::TEXT_BODY())
                            .font_weight(FontWeight::MEDIUM)
                            .child("Message history"),
                    )
                    .child(div().flex().child(segmented(
                        "history",
                        &[
                            (HistoryChoice::OnOpen, "When I open a chat"),
                            (HistoryChoice::Recent, "Recent chats"),
                            (HistoryChoice::Everything, "Everything"),
                        ],
                        chosen.history,
                        palette,
                        move |history, _, cx| {
                            cx.stop_propagation();
                            pick_mode(Box::new(move |settings| settings.history = history), cx);
                        },
                    )))
                    .child(
                        div()
                            .debug_selector(|| "history-detail".into())
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(palette.text_muted)
                            .child(detail),
                    ),
            )
            .child(choice_row(
                "Download media automatically",
                "Images load as they come into view (up to 25 MB each). Videos, audio and \
                 documents wait for a click unless you choose Everything.",
                segmented(
                    "media",
                    &[
                        (MediaChoice::Images, "Images"),
                        (MediaChoice::Never, "Never"),
                        (MediaChoice::Everything, "Everything"),
                    ],
                    chosen.media,
                    palette,
                    |media, _, cx| {
                        cx.stop_propagation();
                        settings::update(cx, |settings| settings.media = media);
                    },
                ),
                palette,
            ))
            .when(chosen.history == HistoryChoice::Recent, |this| {
                this.child(choice_row(
                    "How many chats",
                    "Per linked number, the most recent first.",
                    segmented(
                        "recent",
                        &[(10u32, "10"), (20, "20"), (50, "50"), (100, "100")],
                        chosen.recent_chats,
                        palette,
                        move |count, _, cx| {
                            cx.stop_propagation();
                            pick_count(Box::new(move |settings| settings.recent_chats = count), cx);
                        },
                    ),
                    palette,
                ))
            })
            .when(self.engine.capabilities().quiet_read, |this| {
                this.child(choice_row(
                    "Send read receipts",
                    "Tells the other side when you have read their messages. Off: a chat you \
                     open is still marked as read on your own devices, without the blue ticks.",
                    switch("read-receipts", chosen.read_receipts, palette).on_click(|_, _, cx| {
                        cx.stop_propagation();
                        settings::update(cx, |settings| {
                            settings.read_receipts = !settings.read_receipts;
                        });
                    }),
                    palette,
                ))
            })
            // The same for status: its own switch, which follows the one
            // above (nothing is sent while that is off).
            .when(self.engine.capabilities().story_view, |this| {
                this.child(choice_row(
                    "Status view receipts",
                    if chosen.read_receipts {
                        "Tells the person who posted a status that you saw it, when it was \
                         shown to you. Off: a status you watch is seen here only."
                    } else {
                        "Off while Send read receipts is off: a status you watch is seen here \
                         only."
                    },
                    switch(
                        "story-receipts",
                        chosen.story_receipts && chosen.read_receipts,
                        palette,
                    )
                    .when(!chosen.read_receipts, |this| this.opacity(0.5))
                    .on_click(|_, _, cx| {
                        cx.stop_propagation();
                        settings::update(cx, |settings| {
                            settings.story_receipts = !settings.story_receipts;
                        });
                    }),
                    palette,
                ))
            })
    }

    /// Who sees the account's status, and whether a contact's new status
    /// is told by a notification.
    fn render_status_settings(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let chosen = settings::get(cx);
        let caps = self.engine.capabilities();
        let (audience, count) = self.audience_summary();
        let summary: SharedString = match count {
            Some(n) => format!("{audience} ({n})").into(),
            None => audience,
        };
        section("Status", palette)
            .child(choice_row(
                "Who sees my status",
                if caps.story_privacy_edit {
                    "Your contacts, your contacts except some of them, or only some of them."
                } else if caps.story_privacy {
                    "How it is set on your phone. Changing it is not available yet from this \
                     provider."
                } else {
                    "Not available yet from this provider."
                },
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .debug_selector(|| "status-settings-audience".into())
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text_muted)
                            .child(summary),
                    )
                    .when(caps.story_privacy, |this| {
                        this.child(
                            text_button("status-settings-edit", "View", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_audience(window, cx);
                                })),
                        )
                    }),
                palette,
            ))
            .child(choice_row(
                "Notify me of new status",
                "A notification when a contact posts a status. Off unless you turn it on; not \
                 delivered yet in this build, like the other notifications.",
                switch("notify-status", chosen.story_notifications, palette).on_click(
                    |_, _, cx| {
                        cx.stop_propagation();
                        settings::update(cx, |settings| {
                            settings.story_notifications = !settings.story_notifications;
                        });
                    },
                ),
                palette,
            ))
    }

    /// Who is signed in, the numbers linked to it, and the way out.
    fn render_account(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let provider: SharedString = match self.session {
            SessionKind::Demo => "Demo data (nothing leaves this computer)".into(),
            SessionKind::Keychain | SessionKind::Unsaved | SessionKind::Environment => MAKER.into(),
        };
        let mut account = section("Account", palette)
            .child(fact("Provider", provider, palette))
            .child(fact(
                "Local database",
                match (&self.storage_note, self.session) {
                    (Some(_), _) => "In memory, not saved",
                    (None, SessionKind::Demo) => "In memory (demo data)",
                    (None, _) => "Encrypted on this computer",
                }
                .into(),
                palette,
            ));

        // The key itself is never shown, here or anywhere: only its prefix.
        account = match &self.identity {
            IdentityState::Known(identity) => account
                .child(fact(
                    "Organization",
                    identity.organization.clone().into(),
                    palette,
                ))
                .child(fact(
                    "Project",
                    identity
                        .project
                        .clone()
                        .unwrap_or_else(|| "All projects".to_owned())
                        .into(),
                    palette,
                ))
                .child(fact(
                    "API key",
                    match &identity.key_prefix {
                        Some(prefix) => format!("{prefix}…").into(),
                        None => "Stored in the system keychain".into(),
                    },
                    palette,
                )),
            IdentityState::Loading => {
                account.child(fact("Organization", "Loading…".into(), palette))
            }
            IdentityState::Unavailable => account.child(fact(
                "Organization",
                "Not available right now".into(),
                palette,
            )),
            IdentityState::Unknown => account,
        };

        // Above the numbers, so that it stays in reach however many there are.
        let (note, can_sign_out): (&str, bool) = match self.session {
            SessionKind::Demo => ("Demo data has no account to sign out of.", false),
            SessionKind::Keychain => (
                "Signing out removes the API key and deletes the chats stored on this \
                 computer.",
                true,
            ),
            SessionKind::Unsaved => (
                "Signed in for this session only: the API key is not saved, and the next \
                 start asks again.",
                true,
            ),
            SessionKind::Environment => (
                "Signed in with WUAPI_API_KEY. Unset it to sign in from here.",
                false,
            ),
        };
        account = account.child(
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
                        .child(note),
                )
                .when(can_sign_out, |this| {
                    this.child(
                        text_button(
                            "sign-out",
                            "Sign out",
                            Some(IconName::LogOut),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            // Never at once: it deletes things.
                            this.open_overlay(Overlay::ConfirmSignOut, window, cx);
                        })),
                    )
                }),
        );

        account = account.child(
            div()
                .pt_3()
                .pb_1()
                .flex()
                .items_center()
                .justify_between()
                .child(label("Linked numbers", palette))
                .when(self.engine.capabilities().link_accounts, |this| {
                    this.child(
                        text_button(
                            "add-number-settings",
                            "Add number",
                            Some(IconName::Plus),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_overlay(Overlay::AddNumber, window, cx);
                        })),
                    )
                }),
        );
        if self.accounts.is_empty() {
            account = account.child(
                div()
                    .py_2()
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_muted)
                    .child("No numbers yet."),
            );
        }
        for (index, number) in self.accounts.iter().enumerate() {
            let words = connection_words(number);
            let state = words.label.to_uppercase();
            account =
                account.child(
                    div()
                        .border_b_1()
                        .border_color(palette.border)
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .min_h(px(40.))
                                .py_2()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(led(if number.never_linked() {
                                    palette.text_faint
                                } else {
                                    status_colour(&number.connection, palette)
                                }))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            div().truncate().text_size(metrics::TEXT_BODY()).child(
                                                SharedString::from(number.display_name.clone()),
                                            ),
                                        )
                                        .children(number.phone.clone().map(|phone| {
                                            mono(phone).text_color(palette.text_muted)
                                        }))
                                        // Why it is off, when the provider
                                        // said.
                                        .children(words.reason.is_some().then(|| {
                                            div()
                                                .debug_selector(move || {
                                                    format!("number-reason-{index}")
                                                })
                                                .text_size(metrics::TEXT_SMALL())
                                                .line_height(px(18.))
                                                .text_color(palette.text_muted)
                                                .child(SharedString::from(words.summary.clone()))
                                        })),
                                )
                                .child(mono(state).text_color(palette.text_muted)),
                        )
                        .child(self.render_number_extras(index, number, palette, cx)),
                );
        }

        account
    }
}

/// One line on which received files the provider downloads by itself for
/// a number. Separate from this app's own "Download media" setting, which
/// decides what this computer fetches.
pub(super) fn server_media_line(setting: &ServerMedia) -> String {
    match setting {
        ServerMedia::OnDemand => {
            "Server media: on demand. A file is fetched from WhatsApp when it is first opened."
                .to_owned()
        }
        ServerMedia::Everything => {
            "Server media: everything. Every received file is stored as it arrives.".to_owned()
        }
        ServerMedia::Some { max_bytes, kinds } => {
            let kinds = if kinds.is_empty() {
                "no types".to_owned()
            } else {
                kinds.join(", ")
            };
            format!(
                "Server media: {kinds} up to {} are stored as they arrive; the rest on demand.",
                crate::format::file_size(*max_bytes)
            )
        }
    }
}

/// A section of the panel: a mono title over its rows.
pub(super) fn section(title: &str, palette: &Palette) -> Div {
    div().px_4().pt_4().pb_4().flex().flex_col().child(
        div()
            .pb_2()
            .child(mono(format!("[ {} ]", title.to_uppercase())).text_color(palette.text_muted)),
    )
}

/// A setting: what it is on the left, the control on the right.
pub(super) fn choice_row(
    title: &'static str,
    detail: &'static str,
    control: impl IntoElement,
    palette: &Palette,
) -> Div {
    div()
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
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
                        .child(title),
                )
                .child(
                    div()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.text_muted)
                        .child(detail),
                ),
        )
        .child(control)
}

fn render_appearance(palette: &Palette, cx: &mut Context<Shell>) -> Div {
    let chosen = settings::get(cx);
    section("Appearance", palette)
        .child(choice_row(
            "Theme",
            "System follows your desktop.",
            segmented(
                "theme",
                &[
                    (ThemeChoice::Light, "Light"),
                    (ThemeChoice::Dark, "Dark"),
                    (ThemeChoice::System, "System"),
                ],
                chosen.theme,
                palette,
                |theme, _, cx| {
                    cx.stop_propagation();
                    settings::update(cx, |settings| settings.theme = theme);
                },
            ),
            palette,
        ))
        .child(choice_row(
            "Interface size",
            "Scales everything together: text, rows, pictures and panes. Ctrl with +, - or \
             0 does the same from anywhere.",
            segmented(
                "scale",
                &[
                    (80u16, "80%"),
                    (90, "90%"),
                    (100, "100%"),
                    (110, "110%"),
                    (125, "125%"),
                ],
                crate::theme::nearest_step(chosen.interface_scale),
                palette,
                |scale, _, cx| {
                    cx.stop_propagation();
                    settings::update(cx, |settings| settings.interface_scale = scale);
                },
            ),
            palette,
        ))
        .child(choice_row(
            "Message bubbles",
            "The colour of your own messages: a quiet lime tint, the lime itself, or none.",
            segmented(
                "bubbles",
                &[
                    (BubbleStyle::Brand, "Brand"),
                    (BubbleStyle::Lime, "Lime"),
                    (BubbleStyle::Neutral, "Neutral"),
                ],
                chosen.bubbles,
                palette,
                |bubbles, _, cx| {
                    cx.stop_propagation();
                    settings::update(cx, |settings| settings.bubbles = bubbles);
                },
            ),
            palette,
        ))
        .child(choice_row(
            "Chat wallpaper",
            "What is behind the messages: a faint pattern, or the plain page.",
            segmented(
                "wallpaper",
                &[
                    (WallpaperChoice::Pattern, "Pattern"),
                    (WallpaperChoice::Plain, "Plain"),
                ],
                chosen.wallpaper,
                palette,
                |wallpaper, _, cx| {
                    cx.stop_propagation();
                    settings::update(cx, |settings| settings.wallpaper = wallpaper);
                },
            ),
            palette,
        ))
        .child(choice_row(
            "Motion",
            "The logo's animation and the entrances. System follows your desktop's \
             reduced-motion setting; where it has none, motion is on.",
            segmented(
                "motion",
                &[
                    (MotionChoice::System, "System"),
                    (MotionChoice::On, "On"),
                    (MotionChoice::Off, "Off"),
                ],
                chosen.motion,
                palette,
                |motion, _, cx| {
                    cx.stop_propagation();
                    settings::update(cx, |settings| settings.motion = motion);
                },
            ),
            palette,
        ))
        .child(super::emoji_picker::language_row(palette, cx))
}

/// The keyboard: the handful of shortcuts everything else follows from,
/// and the way to the full list. All of it comes from the registry
/// (`crate::keys`), like the list itself.
fn render_keyboard(palette: &Palette, cx: &mut Context<Shell>) -> Div {
    use crate::keys::{self, Command};
    let mut rows = section("Keyboard", palette).child(
        div()
            .pb_2()
            .text_size(metrics::TEXT_SMALL())
            .line_height(px(19.))
            .text_color(palette.text_muted)
            .child(
                "Everything can be done from the keyboard: the palette goes anywhere and \
                 runs any action, with its keys beside it. From the composer, the up arrow \
                 goes to the messages; there, single keys act on the message in focus.",
            ),
    );
    for (index, command) in [
        Command::Palette,
        Command::PaletteCommands,
        Command::Find,
        Command::SearchMessages,
        Command::FocusMessages,
        Command::NextChat,
        Command::Shortcuts,
    ]
    .into_iter()
    .enumerate()
    {
        rows = rows.child(
            div()
                .debug_selector(move || format!("keyboard-row-{index}"))
                .py(px(6.))
                .border_b_1()
                .border_color(palette.border)
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .text_size(metrics::TEXT_BODY())
                        .child(keys::label(command)),
                )
                .child(
                    mono(keys::keys_label(command).unwrap_or_default()).text_color(palette.text),
                ),
        );
    }
    rows.child(div().pt_3().flex().child(
        text_button("keyboard-show-all", "All shortcuts", None, false, palette).on_click(
            cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.open_overlay(Overlay::Shortcuts, window, cx);
            }),
        ),
    ))
}

fn render_notifications(palette: &Palette, cx: &mut Context<Shell>) -> Div {
    let chosen = settings::get(cx);
    section("Notifications", palette)
        .child(
            div()
                .debug_selector(|| "notifications-note".into())
                .pb_2()
                .flex()
                .gap_2()
                .child(led(palette.warning).mt(px(6.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(19.))
                        .text_color(palette.text_muted)
                        .child(
                            "Not delivered yet. This build does not show desktop \
                             notifications; your choices are saved and apply when it does.",
                        ),
                ),
        )
        .child(choice_row(
            "Desktop notifications",
            "A notification when a message arrives.",
            switch("notify-desktop", chosen.desktop_notifications, palette).on_click(|_, _, cx| {
                cx.stop_propagation();
                settings::update(cx, |settings| {
                    settings.desktop_notifications = !settings.desktop_notifications;
                });
            }),
            palette,
        ))
        .child(choice_row(
            "Message previews",
            "Show the text of the message in the notification.",
            switch("notify-previews", chosen.message_previews, palette).on_click(|_, _, cx| {
                cx.stop_propagation();
                settings::update(cx, |settings| {
                    settings.message_previews = !settings.message_previews;
                });
            }),
            palette,
        ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_media_setting_reads_as_one_line() {
        assert!(server_media_line(&ServerMedia::OnDemand).contains("on demand"));
        assert!(server_media_line(&ServerMedia::Everything).contains("everything"));
        let some = server_media_line(&ServerMedia::Some {
            max_bytes: 5 * 1024 * 1024,
            kinds: vec!["image".into(), "sticker".into()],
        });
        assert!(
            some.contains("image, sticker") && some.contains("5"),
            "{some}"
        );
    }
}
