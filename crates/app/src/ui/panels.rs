//! The settings panel: account, appearance, notifications, about.
//!
//! Everything in it is real or says that it is not: the account comes from
//! the provider and the store, the choices are saved to `settings.json`
//! (see `settings.rs`) the moment they are made, and the notifications
//! section says that nothing is delivered yet.

use super::connection::connection_words;
use super::menus::{fact, panel_header};
use super::shell::{CloudOffer, Home, IdentityState, Overlay, SessionKind, SettingsSection, Shell};
use super::widgets::{label, led, mono, segmented, status_colour, switch, text_button};
use crate::i18n::{self, Language, LanguageChoice};
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
            SettingsSection::About => div()
                .flex()
                .flex_col()
                .child(self.render_about(palette, cx))
                .child(self.render_link_setting(palette, cx)),
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
            (
                SettingsSection::Account,
                i18n::t("accounts.settings.section.account"),
                "settings-account",
            ),
            (
                SettingsSection::Sync,
                i18n::t("accounts.settings.section.sync"),
                "settings-sync",
            ),
            (
                SettingsSection::Appearance,
                i18n::t("accounts.settings.section.appearance"),
                "settings-appearance",
            ),
            (
                SettingsSection::Notifications,
                i18n::t("accounts.settings.section.notifications"),
                "settings-notifications",
            ),
            (
                SettingsSection::Audio,
                i18n::t("accounts.settings.section.audio"),
                "settings-audio",
            ),
            (
                SettingsSection::Stickers,
                i18n::t("accounts.settings.section.stickers"),
                "settings-stickers",
            ),
            (
                SettingsSection::Keyboard,
                i18n::t("accounts.settings.section.keyboard"),
                "settings-keyboard",
            ),
            (
                SettingsSection::Status,
                i18n::t("accounts.settings.section.status"),
                "settings-status",
            ),
            (
                SettingsSection::About,
                i18n::t("accounts.settings.section.about"),
                "settings-about",
            ),
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
            .h(px(780.))
            .flex()
            .flex_col()
            .child(panel_header(
                i18n::t("accounts.settings.title"),
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
            HistoryChoice::OnOpen => i18n::t("accounts.settings.sync.on_open"),
            HistoryChoice::Recent => i18n::t("accounts.settings.sync.recent"),
            HistoryChoice::Everything => i18n::t("accounts.settings.sync.everything"),
        };
        section(i18n::t("accounts.settings.section.sync"), palette)
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
                            .child(i18n::t("accounts.settings.sync.history")),
                    )
                    .child(div().flex().child(segmented(
                        "history",
                        &[
                            (
                                HistoryChoice::OnOpen,
                                i18n::t("accounts.settings.sync.history_on_open"),
                            ),
                            (
                                HistoryChoice::Recent,
                                i18n::t("accounts.settings.sync.history_recent"),
                            ),
                            (
                                HistoryChoice::Everything,
                                i18n::t("accounts.settings.sync.history_everything"),
                            ),
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
                i18n::t("accounts.settings.sync.media"),
                i18n::t("accounts.settings.sync.media_text"),
                segmented(
                    "media",
                    &[
                        (
                            MediaChoice::Automatic,
                            i18n::t("accounts.settings.sync.media_automatic"),
                        ),
                        (
                            MediaChoice::Images,
                            i18n::t("accounts.settings.sync.media_images"),
                        ),
                        (
                            MediaChoice::Never,
                            i18n::t("accounts.settings.sync.media_never"),
                        ),
                        (
                            MediaChoice::Everything,
                            i18n::t("accounts.settings.sync.history_everything"),
                        ),
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
                    i18n::t("accounts.settings.sync.recent_count"),
                    i18n::t("accounts.settings.sync.recent_count_text"),
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
            .when(self.caps_here().quiet_read, |this| {
                this.child(choice_row(
                    i18n::t("accounts.settings.sync.read_receipts"),
                    i18n::t("accounts.settings.sync.read_receipts_text"),
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
            .when(self.caps_here().story_view, |this| {
                this.child(choice_row(
                    i18n::t("accounts.settings.sync.story_receipts"),
                    if chosen.read_receipts {
                        i18n::t("accounts.settings.sync.story_receipts_on")
                    } else {
                        i18n::t("accounts.settings.sync.story_receipts_off")
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
        let caps = self.caps_here();
        let (audience, count) = self.audience_summary();
        let summary: SharedString = match count {
            Some(n) => format!("{audience} ({n})").into(),
            None => audience,
        };
        section(i18n::t("accounts.settings.section.status"), palette)
            .child(choice_row(
                i18n::t("accounts.settings.status.who_sees"),
                if caps.story_privacy_edit {
                    i18n::t("accounts.settings.status.who_sees_edit")
                } else if caps.story_privacy {
                    i18n::t("accounts.settings.status.who_sees_read_only")
                } else {
                    i18n::t("accounts.settings.status.who_sees_none")
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
                            text_button(
                                "status-settings-edit",
                                i18n::t("accounts.settings.status.view"),
                                None,
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_audience(window, cx);
                                },
                            )),
                        )
                    }),
                palette,
            ))
            .child(choice_row(
                i18n::t("accounts.settings.status.notify"),
                i18n::t("accounts.settings.status.notify_text"),
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
            SessionKind::Demo => i18n::t("accounts.settings.account.demo").into(),
            SessionKind::Keychain | SessionKind::Unsaved | SessionKind::Environment => MAKER.into(),
            #[cfg(feature = "provider-native")]
            SessionKind::Local if self.cloud == CloudOffer::Connected => {
                i18n::t("accounts.settings.account.local_cloud").into()
            }
            #[cfg(feature = "provider-native")]
            SessionKind::Local => i18n::t("accounts.settings.account.local").into(),
        };
        let mut account = section(i18n::t("accounts.settings.section.account"), palette)
            .child(fact(
                i18n::t("accounts.settings.account.provider"),
                provider,
                palette,
            ))
            .child(fact(
                i18n::t("accounts.settings.account.database"),
                match (&self.storage_note, self.session) {
                    (Some(_), _) => i18n::t("accounts.settings.account.database_memory"),
                    (None, SessionKind::Demo) => i18n::t("accounts.settings.account.database_demo"),
                    (None, _) => i18n::t("accounts.settings.account.database_encrypted"),
                }
                .into(),
                palette,
            ));

        // The key itself is never shown, here or anywhere: only its prefix.
        account = match &self.identity {
            IdentityState::Known(identity) => account
                .child(fact(
                    i18n::t("accounts.settings.account.organization"),
                    identity.organization.clone().into(),
                    palette,
                ))
                .child(fact(
                    i18n::t("accounts.settings.account.project"),
                    identity
                        .project
                        .clone()
                        .unwrap_or_else(|| {
                            i18n::t("accounts.settings.account.all_projects").to_owned()
                        })
                        .into(),
                    palette,
                ))
                .child(fact(
                    i18n::t("accounts.settings.account.api_key"),
                    match &identity.key_prefix {
                        Some(prefix) => format!("{prefix}…").into(),
                        None => i18n::t("accounts.settings.account.keychain").into(),
                    },
                    palette,
                )),
            IdentityState::Loading => account.child(fact(
                i18n::t("accounts.settings.account.organization"),
                i18n::t("accounts.settings.account.loading").into(),
                palette,
            )),
            IdentityState::Unavailable => account.child(fact(
                i18n::t("accounts.settings.account.organization"),
                i18n::t("accounts.settings.account.unavailable").into(),
                palette,
            )),
            IdentityState::Unknown => account,
        };

        // Above the numbers, so that it stays in reach however many there are.
        let (note, can_sign_out): (&str, bool) = match self.session {
            SessionKind::Demo => (i18n::t("accounts.settings.account.note_demo"), false),
            SessionKind::Keychain => (i18n::t("accounts.settings.account.note_keychain"), true),
            SessionKind::Unsaved => (i18n::t("accounts.settings.account.note_unsaved"), true),
            SessionKind::Environment => {
                (i18n::t("accounts.settings.account.note_environment"), false)
            }
            #[cfg(feature = "provider-native")]
            SessionKind::Local if self.cloud == CloudOffer::Connected => {
                (i18n::t("accounts.settings.account.note_cloud"), true)
            }
            #[cfg(feature = "provider-native")]
            SessionKind::Local if self.cloud == CloudOffer::Offered => {
                (i18n::t("accounts.settings.account.note_offered"), false)
            }
            #[cfg(feature = "provider-native")]
            SessionKind::Local => (i18n::t("accounts.settings.account.note_local"), false),
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
                .when(self.cloud == CloudOffer::Offered, |this| {
                    this.child(
                        text_button(
                            "connect-cloud",
                            i18n::t("accounts.settings.account.connect_cloud"),
                            Some(IconName::Globe),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.connect_cloud(cx);
                        })),
                    )
                })
                .when(can_sign_out, |this| {
                    this.child(
                        text_button(
                            "sign-out",
                            i18n::t("accounts.settings.account.sign_out"),
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
                .child(label(
                    i18n::t("accounts.settings.account.linked_numbers"),
                    palette,
                ))
                .when(self.caps_here().link_accounts, |this| {
                    this.child(
                        text_button(
                            "add-number-settings",
                            i18n::t("accounts.settings.account.add_number"),
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
                    .child(i18n::t("accounts.settings.account.no_numbers")),
            );
        }
        for (index, number) in self.accounts.iter().enumerate() {
            let words = connection_words(number);
            let state = words.label.to_uppercase();
            let home = self.home_of(&number.id);
            account = account.child(
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
                                        div()
                                            .truncate()
                                            .text_size(metrics::TEXT_BODY())
                                            .child(SharedString::from(number.display_name.clone())),
                                    )
                                    .children(
                                        number.phone.clone().map(|phone| {
                                            mono(phone).text_color(palette.text_muted)
                                        }),
                                    )
                                    // Where it is connected from, and
                                    // what that means for it.
                                    .children(home.map(|home| {
                                        div()
                                            .debug_selector(move || format!("number-home-{index}"))
                                            .text_size(metrics::TEXT_SMALL())
                                            .line_height(px(18.))
                                            .text_color(palette.text_muted)
                                            .child(match home {
                                                Home::Local => {
                                                    i18n::t("accounts.settings.account.home_local")
                                                }
                                                Home::Cloud => {
                                                    i18n::t("accounts.settings.account.home_cloud")
                                                }
                                            })
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
        ServerMedia::OnDemand => i18n::t("accounts.settings.server_media.on_demand").to_owned(),
        ServerMedia::Everything => i18n::t("accounts.settings.server_media.everything").to_owned(),
        ServerMedia::Some { max_bytes, kinds } => {
            let kinds = if kinds.is_empty() {
                i18n::t("accounts.settings.server_media.no_types").to_owned()
            } else {
                kinds.join(", ")
            };
            i18n::tr(
                "accounts.settings.server_media.some",
                &[
                    ("kinds", &kinds),
                    ("size", &crate::format::file_size(*max_bytes)),
                ],
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
    section(i18n::t("accounts.settings.section.appearance"), palette)
        .child(choice_row(
            i18n::t("accounts.settings.appearance.theme"),
            i18n::t("accounts.settings.appearance.theme_text"),
            segmented(
                "theme",
                &[
                    (
                        ThemeChoice::Light,
                        i18n::t("accounts.settings.appearance.light"),
                    ),
                    (
                        ThemeChoice::Dark,
                        i18n::t("accounts.settings.appearance.dark"),
                    ),
                    (
                        ThemeChoice::System,
                        i18n::t("accounts.settings.appearance.system"),
                    ),
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
            i18n::t("accounts.settings.appearance.scale"),
            i18n::t("accounts.settings.appearance.scale_text"),
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
            i18n::t("accounts.settings.appearance.bubbles"),
            i18n::t("accounts.settings.appearance.bubbles_text"),
            segmented(
                "bubbles",
                &[
                    (
                        BubbleStyle::Brand,
                        i18n::t("accounts.settings.appearance.brand"),
                    ),
                    (
                        BubbleStyle::Lime,
                        i18n::t("accounts.settings.appearance.lime"),
                    ),
                    (
                        BubbleStyle::Neutral,
                        i18n::t("accounts.settings.appearance.neutral"),
                    ),
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
            i18n::t("accounts.settings.appearance.wallpaper"),
            i18n::t("accounts.settings.appearance.wallpaper_text"),
            segmented(
                "wallpaper",
                &[
                    (
                        WallpaperChoice::Pattern,
                        i18n::t("accounts.settings.appearance.pattern"),
                    ),
                    (
                        WallpaperChoice::Plain,
                        i18n::t("accounts.settings.appearance.plain"),
                    ),
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
            i18n::t("accounts.settings.appearance.motion"),
            i18n::t("accounts.settings.appearance.motion_text"),
            segmented(
                "motion",
                &[
                    (
                        MotionChoice::System,
                        i18n::t("accounts.settings.appearance.system"),
                    ),
                    (MotionChoice::On, i18n::t("accounts.settings.appearance.on")),
                    (
                        MotionChoice::Off,
                        i18n::t("accounts.settings.appearance.off"),
                    ),
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
        .child(choice_row(
            i18n::t("accounts.settings.language.title"),
            i18n::t("accounts.settings.language.text"),
            segmented(
                "language",
                &[
                    (LanguageChoice::System, i18n::t("common.language.system")),
                    (
                        LanguageChoice::Lang(Language::En),
                        Language::En.native_name(),
                    ),
                    (
                        LanguageChoice::Lang(Language::Es),
                        Language::Es.native_name(),
                    ),
                ],
                chosen.language,
                palette,
                |language, _, cx| {
                    cx.stop_propagation();
                    settings::update(cx, |settings| settings.language = language);
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
    let mut rows = section(i18n::t("accounts.settings.section.keyboard"), palette).child(
        div()
            .pb_2()
            .text_size(metrics::TEXT_SMALL())
            .line_height(px(19.))
            .text_color(palette.text_muted)
            .child(i18n::t("accounts.settings.keyboard.text")),
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
    rows.child(
        div().pt_3().flex().child(
            text_button(
                "keyboard-show-all",
                i18n::t("accounts.settings.keyboard.all"),
                None,
                false,
                palette,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.open_overlay(Overlay::Shortcuts, window, cx);
            })),
        ),
    )
}

fn render_notifications(palette: &Palette, cx: &mut Context<Shell>) -> Div {
    let chosen = settings::get(cx);
    section(i18n::t("accounts.settings.section.notifications"), palette)
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
                        .child(i18n::t("accounts.settings.notifications.note")),
                ),
        )
        .child(choice_row(
            i18n::t("accounts.settings.notifications.desktop"),
            i18n::t("accounts.settings.notifications.desktop_text"),
            switch("notify-desktop", chosen.desktop_notifications, palette).on_click(|_, _, cx| {
                cx.stop_propagation();
                settings::update(cx, |settings| {
                    settings.desktop_notifications = !settings.desktop_notifications;
                });
            }),
            palette,
        ))
        .child(choice_row(
            i18n::t("accounts.settings.notifications.previews"),
            i18n::t("accounts.settings.notifications.previews_text"),
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
