//! The profile panels: a contact's, and the account's own.
//!
//! A contact's profile is what the store holds of them: the address book
//! (saved name, the name they gave themselves, the number), what WhatsApp
//! answered when the panel was opened (About, username, a business's
//! profile), the blocklist, and the groups whose participants the store
//! knows. Opening the panel is what asks the engine to refresh those.

use super::menus::{fact, panel_header};
use super::numbers::{error_line, input_box};
use super::shell::Shell;
use super::social::{
    action_row, block, hero, hint, paragraph, question, Confirm, EditWhat, PictureFor, Target,
};
use super::widgets::{avatar_or, icon_button, mono, tag, text_button, AvatarKind};
use crate::format;
use crate::icons::IconName;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_provider::{AccountId, BusinessProfile, ChatId, Contact, ContactId};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, SharedString, Stateful};

/// The side of the picture at the top of a profile.
#[allow(non_snake_case)]
pub(super) fn HERO_PICTURE() -> gpui_kit::Pixels {
    px(96.)
}

impl Shell {
    /// The info panel: a contact's profile or a group's details, by what
    /// was opened.
    pub(super) fn render_info(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let (title, body) = match self.social.target()? {
            Target::Contact { account, contact } => (
                "Contact info",
                self.render_contact(account, contact, palette, cx),
            ),
            Target::Group { account, group } => {
                ("Group info", self.render_group(account, group, palette, cx))
            }
        };
        let header = panel_header(
            title,
            palette,
            cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
        );
        Some(
            self.card("contact-info", palette)
                .w(px(420.))
                .max_h(px(700.))
                .flex()
                .flex_col()
                .child(
                    div()
                        .relative()
                        .child(header)
                        .when(self.info_can_go_back(), |this| {
                            // Over the title's left edge: back to where
                            // this was opened from.
                            this.child(div().absolute().top(px(8.)).right(px(44.)).child(
                                icon_button("info-back", IconName::ArrowLeft, palette).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.info_back(window, cx);
                                    }),
                                ),
                            ))
                        }),
                )
                .children(self.social.error.clone().map(|error| {
                    div()
                        .px_4()
                        .pt_2()
                        .child(error_line("info-error", error, palette))
                }))
                .children(self.social.note.clone().map(|note| {
                    div()
                        .debug_selector(|| "info-note".into())
                        .px_4()
                        .pt_2()
                        .child(hint(note, palette))
                }))
                .child(
                    div()
                        .id("info-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(body),
                ),
        )
    }

    fn render_contact(
        &self,
        account: &AccountId,
        contact: &ContactId,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.engine.capabilities();
        let store = self.engine.store();
        // Looking is what refreshes: WhatsApp is asked about the contact,
        // the blocklist and (for "groups in common") every group, each at
        // most once in a while, in the background.
        self.engine.want_profile(account, contact);
        self.engine.want_groups(account);

        let chat_id = ChatId::new(contact.as_str());
        let chat = store.chat(account, &chat_id).ok().flatten();
        let known = store
            .contact(account, contact)
            .ok()
            .flatten()
            .unwrap_or_else(|| Contact::new(account.clone(), contact.clone()));
        let number = known.phone.clone().or_else(|| {
            contact
                .as_str()
                .starts_with('+')
                .then(|| contact.to_string())
        });
        let name = match (&chat, known.display_name()) {
            // The chat's title is the best name the provider had.
            (Some(chat), fallback) if fallback == contact.as_str() => chat.title.clone(),
            (_, name) => name,
        };
        let blocked = store.is_blocked(account, contact).unwrap_or(false);
        let business = store.business_profile(account, contact).ok().flatten();
        let is_open = self
            .open
            .as_ref()
            .is_some_and(|open| open.chat.id == chat_id && &open.chat.account_id == account);

        let mut lines = Vec::new();
        if let Some(number) = &number {
            lines.push(
                mono(format::phone(number))
                    .debug_selector(|| "profile-number".into())
                    .text_color(palette.text_muted),
            );
        }
        if let Some(profile_name) = known.profile_name.as_ref().filter(|n| **n != name) {
            lines.push(hint(format!("~{profile_name}"), palette));
        }
        if let Some(username) = &known.username {
            lines.push(mono(format!("@{username}")).text_color(palette.text_muted));
        }
        if known.business_name.is_some() {
            lines.push(tag("Business", None, palette));
        }
        if blocked {
            lines.push(
                tag("Blocked", Some(palette.danger), palette)
                    .debug_selector(|| "profile-blocked".into()),
            );
        }
        let picture = super::senders::person_avatar(
            self.media.avatar(account, &chat_id),
            &name,
            HERO_PICTURE(),
            self.senders.of(account, None, contact).tone,
            palette,
        );
        let mut panel = div()
            .debug_selector(|| "profile".into())
            .flex()
            .flex_col()
            .child(hero(picture, name.clone().into(), lines, palette));

        // About: what WhatsApp answered when asked.
        let about = match (&known.about, caps.contact_lookup) {
            (Some(about), _) => Some(paragraph(about.clone(), palette)),
            (None, true) => Some(hint("Not shown to this number, or not set.", palette)),
            (None, false) => None,
        };
        if let Some(about) = about {
            panel = panel.child(
                block("About", palette).child(about.debug_selector(|| "profile-about".into())),
            );
        }
        if let Some(business) = &business {
            panel = panel.child(business_block(business, palette));
        }

        // What can be done.
        let mut actions = div().px_2().py_2().flex().flex_col();
        if !is_open {
            let (to_account, to) = (account.clone(), contact.clone());
            actions = actions.child(
                action_row(
                    "profile-message",
                    IconName::MessageCircle,
                    "Message",
                    false,
                    palette,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.message_from_panel(&to_account, &to, window, cx);
                })),
            );
        }
        if let Some(chat) = chat.as_ref().filter(|_| caps.chat_state) {
            let (of_account, of_chat, muted) = (account.clone(), chat.id.clone(), chat.muted);
            actions = actions.child(
                action_row(
                    "profile-mute",
                    if muted {
                        IconName::Bell
                    } else {
                        IconName::BellOff
                    },
                    if muted {
                        "Unmute notifications"
                    } else {
                        "Mute notifications"
                    },
                    false,
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.mute_from_panel(&of_account, &of_chat, !muted);
                    cx.notify();
                })),
            );
        }
        if chat.is_some() {
            let search_in = chat_id.clone();
            actions = actions.child(
                action_row(
                    "profile-search",
                    IconName::Search,
                    "Search in chat",
                    false,
                    palette,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.search_from_panel(search_in.clone(), window, cx);
                })),
            );
        }
        if let Some(number) = number.clone() {
            actions = actions.child(
                action_row(
                    "profile-copy",
                    IconName::Copy,
                    "Copy number",
                    false,
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.copy_from_panel(number.clone(), "Number copied.", cx);
                })),
            );
        }
        if caps.blocking {
            actions = actions.child(
                action_row(
                    "profile-block",
                    IconName::Ban,
                    if blocked { "Unblock" } else { "Block" },
                    !blocked,
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.ask(Confirm::Block(!blocked), cx);
                })),
            );
        }
        // Their status: muted here, and with the provider when it keeps
        // mutes. Offered wherever the provider has a status at all.
        if self.has_status() {
            let muted = store.story_muted(account, contact).unwrap_or(false);
            let (account, who) = (account.clone(), contact.clone());
            actions = actions.child(
                action_row(
                    "profile-mute-status",
                    if muted {
                        IconName::Bell
                    } else {
                        IconName::BellOff
                    },
                    if muted {
                        "Unmute their status"
                    } else {
                        "Mute their status"
                    },
                    false,
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.engine.set_story_muted(&account, &who, !muted);
                    cx.notify();
                })),
            );
        }
        panel = panel.child(actions.border_b_1().border_color(palette.border));

        if let Some(Confirm::Block(block)) = &self.social.confirm {
            let words = if *block {
                format!(
                    "Block {name}? They will no longer be able to message or call this number, \
                     and will not be told."
                )
            } else {
                format!("Unblock {name}? They will be able to message and call this number again.")
            };
            panel = panel.child(
                question("profile-block-question", words, palette).child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            text_button("profile-block-cancel", "Cancel", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.info_back(window, cx);
                                })),
                        )
                        .child(
                            text_button(
                                "profile-block-confirm",
                                if *block { "Block" } else { "Unblock" },
                                Some(IconName::Ban),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.confirmed(cx);
                            })),
                        ),
                ),
            );
        }

        // The address book's names, when there is more than the title.
        let mut names = div().px_4().py_1().flex().flex_col();
        let mut any = false;
        for (title, value) in [
            ("Saved as", &known.saved_name),
            ("Business", &known.business_name),
            ("Name on WhatsApp", &known.profile_name),
        ] {
            if let Some(value) = value.as_ref().filter(|value| !value.trim().is_empty()) {
                any = true;
                names = names.child(fact(title, value.clone().into(), palette));
            }
        }
        if any {
            panel = panel.child(names);
        }

        if caps.group_info {
            panel = panel.child(self.render_common_groups(account, contact, palette, cx));
        }
        panel
    }

    /// The groups the account and the contact are both in, as far as the
    /// store knows their participants.
    fn render_common_groups(
        &self,
        account: &AccountId,
        contact: &ContactId,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let common = self
            .engine
            .store()
            .groups_in_common(account, contact)
            .unwrap_or_default();
        let mut list = block("Groups in common", palette);
        if common.is_empty() {
            return list.child(
                hint(
                    if self.engine.groups_listed(account) {
                        "None."
                    } else {
                        "Reading this number's groups…"
                    },
                    palette,
                )
                .debug_selector(|| "common-groups-none".into()),
            );
        }
        for (index, (group, subject)) in common.into_iter().enumerate() {
            let target = Target::Group {
                account: account.clone(),
                group: group.clone(),
            };
            list = list.child(
                self.person_row(
                    ("common-group", index),
                    format!("common-group-{index}"),
                    account,
                    &group,
                    &subject,
                    None,
                    AvatarKind::Group,
                    palette,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.push_info(target.clone(), window, cx);
                })),
            );
        }
        list
    }

    // ----- the account's own profile -------------------------------------

    /// The editor of a number's own WhatsApp profile.
    pub(super) fn render_own_profile(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let own = self.social.own.as_ref()?;
        let account = &own.account;
        let number = self.accounts.iter().find(|known| &known.id == account)?;
        // What the provider can read of it, read behind the panel.
        self.engine.want_own_profile(account);
        let profile = self.engine.store().own_profile(account).unwrap_or_default();
        let subject = self.engine.own_subject(account);
        let stored = subject
            .as_ref()
            .and_then(|subject| self.engine.store().avatar(account, subject).ok().flatten());
        let has_picture = stored.is_some_and(|stored| stored.image.is_some());
        let picture = avatar_or(
            subject
                .as_ref()
                .and_then(|subject| self.media.avatar(account, subject)),
            profile.name.as_deref().unwrap_or(&number.display_name),
            AvatarKind::Person,
            HERO_PICTURE(),
            palette,
        );
        let mut lines = vec![hint(
            format!("{} on WhatsApp", number.display_name),
            palette,
        )];
        if let Some(phone) = &number.phone {
            lines.insert(0, mono(format::phone(phone)).text_color(palette.text_muted));
        }
        let (change_for, connected) = (account.clone(), number.connection.is_connected());
        let mut body = div().flex().flex_col().child(hero(
            picture,
            profile
                .name
                .clone()
                .unwrap_or_else(|| number.display_name.clone())
                .into(),
            lines,
            palette,
        ));

        body = body.child(
            div()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(palette.border)
                .flex()
                .justify_center()
                .gap_2()
                .child(
                    text_button(
                        "own-picture-change",
                        "Change picture",
                        Some(IconName::Camera),
                        false,
                        palette,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.choose_picture(PictureFor::Own(change_for.clone()), cx);
                    })),
                )
                .when(has_picture, |this| {
                    this.child(
                        text_button(
                            "own-picture-remove",
                            "Remove",
                            Some(IconName::Trash),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.ask(Confirm::RemovePicture, cx);
                        })),
                    )
                }),
        );
        if self.social.confirm == Some(Confirm::RemovePicture) {
            body = body.child(
                question(
                    "own-picture-question",
                    "Remove this number's profile picture on WhatsApp?",
                    palette,
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            text_button("own-picture-keep", "Keep it", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.leave_own_profile(window, cx);
                                })),
                        )
                        .child(
                            text_button(
                                "own-picture-remove-confirm",
                                "Remove",
                                Some(IconName::Trash),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.remove_own_picture(cx);
                            })),
                        ),
                ),
            );
        }

        body = body
            .child(self.render_own_field(
                EditWhat::OwnName,
                "Name",
                profile.name.clone(),
                "The name on the phone is not known here yet. What you set here replaces it.",
                palette,
                cx,
            ))
            .child(self.render_own_field(
                EditWhat::OwnAbout,
                "About",
                profile.about.clone(),
                // TODO(wuapi-api): no route reads the own About text.
                "WhatsApp did not say what the About text is. What you set here replaces it.",
                palette,
                cx,
            ));
        if !connected {
            body = body.child(
                div().px_4().py_3().child(
                    hint(
                        "This number is not connected. Changes are sent when it is back.",
                        palette,
                    )
                    .debug_selector(|| "own-offline".into()),
                ),
            );
        }
        Some(
            self.card("own-profile", palette)
                .w(px(420.))
                .max_h(px(700.))
                .flex()
                .flex_col()
                .child(panel_header(
                    "WhatsApp profile",
                    palette,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.social.confirm = None;
                        this.social.edit = None;
                        this.leave_own_profile(window, cx);
                    }),
                ))
                .children(self.social.error.clone().map(|error| {
                    div()
                        .px_4()
                        .pt_2()
                        .child(error_line("info-error", error, palette))
                }))
                .child(
                    div()
                        .id("own-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .child(body),
                ),
        )
    }

    /// One text of the own profile: what it is, and the way to change it.
    fn render_own_field(
        &self,
        what: EditWhat,
        title: &'static str,
        value: Option<String>,
        unknown: &'static str,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let (shown, edit_button, input) = match what {
            EditWhat::OwnName => ("own-name", "own-edit-name", "own-name-input"),
            _ => ("own-about", "own-edit-about", "own-about-input"),
        };
        let field = block(title, palette);
        if let Some(edit) = self.social.edit.as_ref().filter(|edit| edit.what == what) {
            return field
                .child(input_box(input, &edit.input, false, palette))
                .children(
                    edit.error
                        .clone()
                        .map(|error| error_line("edit-error", error, palette)),
                )
                .child(edit_buttons(palette, cx));
        }
        let current = value.clone().unwrap_or_default();
        field.child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .debug_selector(move || shown.into())
                        .child(match value {
                            Some(value) => paragraph(value, palette),
                            None => hint(unknown, palette),
                        }),
                )
                .child(
                    text_button(edit_button, "Edit", Some(IconName::Pencil), false, palette)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.begin_edit(what, current.clone(), window, cx);
                        })),
                ),
        )
    }
}

/// Save and Cancel under a field being edited.
pub(super) fn edit_buttons(palette: &Palette, cx: &mut Context<Shell>) -> Div {
    div()
        .pt_1()
        .flex()
        .justify_end()
        .gap_2()
        .child(
            text_button("edit-cancel", "Cancel", None, false, palette).on_click(cx.listener(
                |this, _, window, cx| {
                    cx.stop_propagation();
                    this.social.edit = None;
                    this.overlay_focus.focus(window, cx);
                    cx.notify();
                },
            )),
        )
        .child(
            text_button("edit-save", "Save", None, true, palette).on_click(cx.listener(
                |this, _, window, cx| {
                    cx.stop_propagation();
                    this.save_edit(window, cx);
                },
            )),
        )
}

/// What a business says about itself.
fn business_block(profile: &BusinessProfile, palette: &Palette) -> Div {
    let mut shown = block("Business", palette).debug_selector(|| "profile-business".into());
    if let Some(description) = &profile.description {
        shown = shown.child(paragraph(description.clone(), palette));
    }
    if !profile.categories.is_empty() {
        shown = shown.child(hint(profile.categories.join(" · "), palette));
    }
    for (title, value) in [("Address", &profile.address), ("E-mail", &profile.email)] {
        if let Some(value) = value {
            shown = shown.child(fact(title, value.clone().into(), palette));
        }
    }
    for site in &profile.websites {
        shown = shown.child(fact("Web", site.clone().into(), palette));
    }
    if !profile.hours.is_empty() {
        let mut hours = div().pt_1().flex().flex_col();
        for day in &profile.hours {
            let when: SharedString = match (&day.open, &day.close) {
                (Some(open), Some(close)) => format!("{open} to {close}").into(),
                _ => day.mode.replace('_', " ").into(),
            };
            hours = hours.child(
                div()
                    .flex()
                    .justify_between()
                    .gap_4()
                    .child(mono(day.day.to_uppercase()).text_color(palette.text_muted))
                    .child(
                        div()
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text)
                            .child(when),
                    ),
            );
        }
        if let Some(zone) = &profile.time_zone {
            hours = hours.child(hint(zone.clone(), palette));
        }
        shown = shown.child(hours);
    }
    shown
}
