//! The group panels: a group's details, participants and settings, and
//! the form that creates a new one.
//!
//! What an admin can change is only offered to an admin, and only where
//! the provider says it can do it (`Capabilities::group_*`). When the
//! account's own role is not known (the provider names it by an id the
//! account does not know as its own) the controls are offered and
//! WhatsApp decides: a refusal is said in words.

use super::menus::{fact, panel_header};
use super::numbers::{error_line, input_box, small_button};
use super::profiles::{edit_buttons, HERO_PICTURE};
use super::shell::Shell;
use super::social::{
    action_row, badge, block, hero, hint, paragraph, question, toggle_row, Confirm, EditWhat,
    GroupTab, People, PictureFor, Target, PEOPLE_SHOWN,
};
use super::widgets::{avatar_or, mono, segmented, text_button, AvatarKind};
use crate::format;
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::{StoredGroup, StoredParticipant};
use client_provider::{AccountId, ChatId, ContactId, GroupChange, GroupRole, ParticipantChange};
use gpui_kit::prelude::*;
use gpui_kit::{div, img, Context, Div, ObjectFit, SharedString, Stateful, StyledImage};

impl Shell {
    pub(super) fn render_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        // Looking is what refreshes: read again when the copy is not fresh.
        self.engine.want_group(account, group);
        let store = self.engine.store();
        let chat = store.chat(account, group).ok().flatten();
        let stored = store.group(account, group).ok().flatten();
        let subject = match (&stored, &chat) {
            (Some(stored), _) => stored.group.subject.clone(),
            (None, Some(chat)) => chat.title.clone(),
            (None, None) => group.to_string(),
        };
        let picture = avatar_or(
            self.media.avatar(account, group),
            &subject,
            AvatarKind::Group,
            HERO_PICTURE(),
            palette,
        );
        let count = stored.as_ref().map(|stored| stored.participant_count);
        let kind = match &stored {
            Some(stored) if stored.group.community => "Community",
            _ => "Group",
        };
        let lines = vec![hint(
            match count {
                Some(1) => format!("{kind} · 1 participant"),
                Some(count) => format!("{kind} · {count} participants"),
                None => kind.to_owned(),
            },
            palette,
        )
        .debug_selector(|| "group-count".into())];
        let mut panel = div()
            .debug_selector(|| "group-info".into())
            .flex()
            .flex_col()
            .child(hero(picture, subject.clone().into(), lines, palette));

        let Some(stored) = stored else {
            // Never read: say so instead of showing an empty group.
            return panel.child(div().px_4().py_4().child(hint(
                if self.engine.capabilities().group_info {
                    "Reading the group from WhatsApp…"
                } else {
                    "This provider does not give a group's details."
                },
                palette,
            )));
        };
        let can_manage = self.can_manage(&stored);

        if let Some(people) = &self.social.adding {
            return panel.child(self.render_adding(account, group, people, palette, cx));
        }

        let view = cx.entity().downgrade();
        let mut tabs = vec![
            (GroupTab::Overview, "Overview"),
            (GroupTab::People, "Participants"),
        ];
        if can_manage {
            tabs.push((GroupTab::Manage, "Manage"));
        }
        let tab = match self.social.tab {
            GroupTab::Manage if !can_manage => GroupTab::Overview,
            tab => tab,
        };
        panel = panel.child(
            div()
                .px_4()
                .py_2()
                .flex()
                .justify_center()
                .border_b_1()
                .border_color(palette.border)
                .child(segmented(
                    "group-tab",
                    &tabs,
                    tab,
                    palette,
                    move |tab, _, cx| {
                        cx.stop_propagation();
                        view.update(cx, |this, cx| {
                            this.social.tab = tab;
                            this.social.confirm = None;
                            this.social.edit = None;
                            this.social.expanded = None;
                            cx.notify();
                        })
                        .ok();
                    },
                )),
        );
        panel.child(match tab {
            GroupTab::Overview => self.render_group_overview(&stored, palette, cx),
            GroupTab::People => self.render_group_people(&stored, can_manage, palette, cx),
            GroupTab::Manage => self.render_group_manage(&stored, palette, cx),
        })
    }

    /// Whether what only admins can do is offered: to an admin, and to a
    /// number whose role is not known (WhatsApp then decides).
    fn can_manage(&self, stored: &StoredGroup) -> bool {
        self.engine.capabilities().group_manage
            && !stored.departed
            && stored.my_role.is_none_or(GroupRole::is_admin)
    }

    fn render_group_overview(
        &self,
        stored: &StoredGroup,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.engine.capabilities();
        let details = &stored.group;
        let (account, group) = (&details.account_id, &details.id);
        let store = self.engine.store();
        let chat = store.chat(account, group).ok().flatten();
        let mut page = div().flex().flex_col();

        if let Some(description) = &details.description {
            page = page.child(
                block("Description", palette)
                    .child(paragraph(description.clone(), palette))
                    .debug_selector(|| "group-description".into()),
            );
        }

        let mut facts = div().px_4().py_1().flex().flex_col();
        let role: SharedString = match (stored.departed, stored.my_role) {
            (true, _) => "You are no longer in this group".into(),
            (_, Some(GroupRole::Owner)) => "You created this group".into(),
            (_, Some(GroupRole::Admin)) => "You are an admin".into(),
            (_, Some(GroupRole::Member)) => "You are a participant".into(),
            // TODO(wuapi-api): a group may list this number under a
            // hidden-number id the account object does not carry.
            (_, None) => "Not known here".into(),
        };
        facts = facts.child(div().debug_selector(|| "group-role".into()).child(fact(
            "Your role",
            role,
            palette,
        )));
        if let Some(created) = details.created_at {
            let by = details.owner.as_ref().map(|owner| {
                store
                    .contact(account, owner)
                    .ok()
                    .flatten()
                    .map(|known| known.display_name())
                    .unwrap_or_else(|| format::phone(owner.as_str()))
            });
            let text = match by {
                Some(by) => format!("{} by {by}", format::date(created)),
                None => format::date(created),
            };
            facts = facts.child(fact("Created", text.into(), palette));
        }
        if details.announce {
            facts = facts.child(fact("Messages", "Only admins can send".into(), palette));
        }
        page = page.child(facts);

        // A community's groups: shown, not managed from here.
        if !details.subgroups.is_empty() {
            let mut linked = block("Groups in this community", palette)
                .debug_selector(|| "group-subgroups".into());
            for subgroup in &details.subgroups {
                linked = linked.child(
                    div()
                        .py_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(metrics::TEXT_BODY())
                                .child(SharedString::from(subgroup.subject.clone())),
                        )
                        .when(subgroup.announcements, |this| {
                            this.child(badge("Announcements", palette))
                        }),
                );
            }
            page = page.child(linked);
        }

        let mut actions = div().px_2().py_2().flex().flex_col();
        if let Some(chat) = chat.as_ref().filter(|_| caps.chat_state) {
            let (of_account, of_chat, muted) = (account.clone(), chat.id.clone(), chat.muted);
            actions = actions.child(
                action_row(
                    "group-mute",
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
            let search_in = group.clone();
            actions = actions.child(
                action_row(
                    "group-search",
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
        if caps.group_leave && !stored.departed {
            actions = actions.child(
                action_row(
                    "group-leave",
                    IconName::LogOut,
                    "Leave group",
                    true,
                    palette,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.ask(Confirm::Leave, cx);
                })),
            );
        }
        page = page.child(actions);

        if self.social.confirm == Some(Confirm::Leave) {
            let busy = self.social.busy;
            page = page.child(
                question(
                    "group-leave-question",
                    format!(
                        "Leave \"{}\"? You will stop receiving its messages, and only an admin \
                         can add you back. The chat stays in your list.",
                        details.subject
                    ),
                    palette,
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            text_button("group-leave-cancel", "Stay", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.info_back(window, cx);
                                })),
                        )
                        .child(
                            text_button(
                                "group-leave-confirm",
                                if busy { "Leaving…" } else { "Leave group" },
                                Some(IconName::LogOut),
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
        page
    }

    fn render_group_people(
        &self,
        stored: &StoredGroup,
        can_manage: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.engine.capabilities();
        let (account, group) = (&stored.group.account_id, &stored.group.id);
        let store = self.engine.store();
        let mut page = div().flex().flex_col();

        // Who is waiting to be let in.
        if can_manage && caps.group_join_requests {
            self.engine.want_join_requests(account, group);
            let pending = store.join_requests(account, group).unwrap_or_default();
            if !pending.is_empty() {
                let mut waiting =
                    block("Asking to join", palette).debug_selector(|| "join-requests".into());
                for (index, request) in pending.into_iter().enumerate() {
                    let name = store
                        .contact(account, &request.contact)
                        .ok()
                        .flatten()
                        .map(|known| known.display_name())
                        .unwrap_or_else(|| format::phone(request.contact.as_str()));
                    let (approve, reject) = (request.clone(), request.clone());
                    waiting = waiting.child(
                        div()
                            .py_1()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(metrics::TEXT_BODY())
                                    .child(SharedString::from(name)),
                            )
                            .child(
                                small_button(
                                    ("join-approve", index),
                                    format!("join-approve-{index}"),
                                    "Approve",
                                    palette,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.answer_request(approve.clone(), true, cx);
                                    },
                                )),
                            )
                            .child(
                                small_button(
                                    ("join-reject", index),
                                    format!("join-reject-{index}"),
                                    "Reject",
                                    palette,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.answer_request(reject.clone(), false, cx);
                                    },
                                )),
                            ),
                    );
                }
                page = page.child(waiting);
            }
        }

        let typed = self
            .social
            .member_search
            .as_ref()
            .map(|(search, _)| search.read(cx).value().trim().to_owned())
            .unwrap_or_default();
        let people = store
            .group_participants(account, group, Some(&typed), PEOPLE_SHOWN + 1)
            .unwrap_or_default();
        let mut list = div().px_2().py_2().flex().flex_col();
        if can_manage {
            list = list.child(
                action_row(
                    "group-add-people",
                    IconName::UserPlus,
                    "Add participants",
                    false,
                    palette,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.begin_adding(window, cx);
                })),
            );
        }
        if let Some((search, _)) = self
            .social
            .member_search
            .as_ref()
            .filter(|_| stored.participant_count > 8)
        {
            list = list.child(div().px_2().pb_2().child(input_box(
                "participant-search",
                search,
                false,
                palette,
            )));
        }
        let more = people.len() > PEOPLE_SHOWN;
        for (index, person) in people.iter().take(PEOPLE_SHOWN).enumerate() {
            list =
                list.child(self.render_participant(index, stored, person, can_manage, palette, cx));
        }
        if people.is_empty() {
            list = list.child(div().px_2().child(hint(
                if typed.is_empty() {
                    "WhatsApp has not listed this group's participants."
                } else {
                    "No participant matches."
                },
                palette,
            )));
        }
        if more {
            list = list.child(
                div().px_2().pt_1().child(
                    mono("MORE PARTICIPANTS: TYPE TO NARROW")
                        .text_size(px(9.5))
                        .text_color(palette.text_faint),
                ),
            );
        }
        page = page.child(list);

        page
    }

    /// One participant: who, their badge, and (when their row was picked)
    /// what can be done with them.
    fn render_participant(
        &self,
        index: usize,
        stored: &StoredGroup,
        person: &StoredParticipant,
        can_manage: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let (account, group) = (&stored.group.account_id, &stored.group.id);
        let _ = group;
        let name = if person.me {
            "You".to_owned()
        } else {
            person.name.clone()
        };
        let detail = person
            .phone
            .as_ref()
            .map(|phone| format::phone(phone))
            .filter(|phone| *phone != name);
        let toggled = person.contact.clone();
        let expanded = self.social.expanded.as_ref() == Some(&person.contact);
        let row = self
            .person_row(
                ("participant", index),
                format!("participant-{index}"),
                account,
                &ChatId::new(person.contact.as_str()),
                &name,
                detail,
                AvatarKind::Person,
                palette,
            )
            .when(expanded, |this| this.bg(palette.muted))
            .when(person.role.is_admin(), |this| {
                this.child(
                    div()
                        .debug_selector(move || format!("participant-admin-{index}"))
                        .child(badge(
                            if person.role == GroupRole::Owner {
                                "Owner"
                            } else {
                                "Admin"
                            },
                            palette,
                        )),
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                let same = this.social.expanded.as_ref() == Some(&toggled);
                this.social.expanded = (!same).then(|| toggled.clone());
                this.social.confirm = None;
                cx.notify();
            }));
        let mut shown = div().flex().flex_col().child(row);
        if !expanded || person.me {
            return shown;
        }
        let mut actions = div().pl(px(52.)).pb_2().flex().flex_wrap().gap_2();
        let target = Target::Contact {
            account: account.clone(),
            contact: person.contact.clone(),
        };
        actions = actions.child(
            small_button(
                "participant-profile",
                "participant-profile".into(),
                "View profile",
                palette,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.push_info(target.clone(), window, cx);
            })),
        );
        if can_manage && person.role != GroupRole::Owner {
            let (promote, demote, remove) = (
                person.contact.clone(),
                person.contact.clone(),
                person.contact.clone(),
            );
            actions = actions.child(if person.role == GroupRole::Member {
                small_button(
                    "participant-promote",
                    "participant-promote".into(),
                    "Make admin",
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.change_participant(promote.clone(), ParticipantChange::Promote, cx);
                }))
            } else {
                small_button(
                    "participant-demote",
                    "participant-demote".into(),
                    "Dismiss as admin",
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.change_participant(demote.clone(), ParticipantChange::Demote, cx);
                }))
            });
            actions = actions.child(
                small_button(
                    "participant-remove",
                    "participant-remove".into(),
                    "Remove",
                    palette,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.ask(Confirm::Remove(remove.clone()), cx);
                })),
            );
        }
        shown = shown.child(actions);
        // The question sits under the person it is about.
        if self.social.confirm == Some(Confirm::Remove(person.contact.clone())) {
            shown = shown.child(
                question(
                    "participant-remove-question",
                    format!("Remove {name} from \"{}\"?", stored.group.subject),
                    palette,
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            text_button(
                                "participant-remove-cancel",
                                "Cancel",
                                None,
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.info_back(window, cx);
                                },
                            )),
                        )
                        .child(
                            text_button(
                                "participant-remove-confirm",
                                "Remove",
                                Some(IconName::Trash),
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
        shown
    }

    fn render_group_manage(
        &self,
        stored: &StoredGroup,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.engine.capabilities();
        let details = &stored.group;
        let (account, group) = (&details.account_id, &details.id);
        let has_picture = self
            .engine
            .store()
            .avatar(account, group)
            .ok()
            .flatten()
            .is_some_and(|picture| picture.image.is_some());
        let mut page = div().flex().flex_col();

        // Name and description, each edited in place.
        for (what, title, value, button) in [
            (
                EditWhat::Subject,
                "Name",
                details.subject.clone(),
                "group-edit-subject",
            ),
            (
                EditWhat::Description,
                "Description",
                details.description.clone().unwrap_or_default(),
                "group-edit-description",
            ),
        ] {
            let mut field = block(title, palette);
            match self.social.edit.as_ref().filter(|edit| edit.what == what) {
                Some(edit) => {
                    field = field
                        .child(input_box("edit-input", &edit.input, false, palette))
                        .children(
                            edit.error
                                .clone()
                                .map(|error| error_line("edit-error", error, palette)),
                        )
                        .child(edit_buttons(palette, cx));
                }
                None => {
                    let current = value.clone();
                    field = field.child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(div().flex_1().min_w_0().child(if value.is_empty() {
                                hint("None.", palette)
                            } else {
                                paragraph(value, palette)
                            }))
                            .child(
                                text_button(button, "Edit", Some(IconName::Pencil), false, palette)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.begin_edit(what, current.clone(), window, cx);
                                    })),
                            ),
                    );
                }
            }
            page = page.child(field);
        }

        let picture_for = PictureFor::Group(account.clone(), group.clone());
        page = page.child(
            block("Picture", palette).child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        text_button(
                            "group-picture-change",
                            "Change picture",
                            Some(IconName::Camera),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.choose_picture(picture_for.clone(), cx);
                        })),
                    )
                    .when(has_picture, |this| {
                        this.child(
                            text_button(
                                "group-picture-remove",
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
            ),
        );
        if self.social.confirm == Some(Confirm::RemovePicture) {
            page = page.child(self.yes_no(
                "group-picture-question",
                "Remove the group's picture?".into(),
                "Remove",
                palette,
                cx,
            ));
        }

        if caps.group_invites {
            page = page.child(self.render_invite_link(stored, palette, cx));
        }

        // The settings the provider exposes.
        let mut settings = block("Settings", palette);
        let unknown = "WhatsApp does not say what this is set to; switching sets it.";
        for (id, title, detail, on, change) in [
            (
                "setting-announce",
                "Only admins send messages",
                "Everyone else can read.",
                Some(details.announce),
                GroupChange::Announce(!details.announce),
            ),
            (
                "setting-locked",
                "Only admins edit group info",
                "The name, description and picture.",
                Some(details.locked),
                GroupChange::Locked(!details.locked),
            ),
            (
                "setting-approval",
                "Approve new participants",
                "An admin lets in whoever asks to join.",
                details.join_approval,
                GroupChange::JoinApproval(!details.join_approval.unwrap_or(false)),
            ),
            (
                "setting-members-add",
                "Participants can add others",
                "Off: only admins add people.",
                details.members_can_add,
                GroupChange::MembersCanAdd(!details.members_can_add.unwrap_or(false)),
            ),
        ] {
            // TODO(wuapi-api): `joinApproval` and `memberAddMode` cannot
            // be read back, so those two start as "not known".
            let (words, switch) = toggle_row(
                id,
                title,
                if on.is_some() { detail } else { unknown },
                on.unwrap_or(false),
                palette,
            );
            settings = settings.child(
                div()
                    .py_1()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(words)
                    .child(switch.on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.change_group(change.clone(), cx);
                    }))),
            );
        }
        page = page.child(settings);
        page
    }

    /// The invite link: asked for on a click, then shown with what can be
    /// done with it.
    fn render_invite_link(
        &self,
        stored: &StoredGroup,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let (account, group) = (stored.group.account_id.clone(), stored.group.id.clone());
        let mut shown = block("Invite link", palette);
        let Some(link) = stored.invite_link.clone() else {
            return shown
                .child(hint(
                    "Anyone with the link can join the group, or ask to when new \
                     participants need approval.",
                    palette,
                ))
                .child(
                    div().flex().child(
                        text_button(
                            "invite-show",
                            if self.social.busy {
                                "Reading…"
                            } else {
                                "Show invite link"
                            },
                            Some(IconName::Link),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.invite_link(account.clone(), group.clone(), false, cx);
                        })),
                    ),
                );
        };
        let copied = link.clone();
        shown = shown
            .child(
                mono(link)
                    .debug_selector(|| "invite-link".into())
                    .text_color(palette.text),
            )
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        text_button("invite-copy", "Copy", Some(IconName::Copy), false, palette)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.copy_from_panel(copied.clone(), "Invite link copied.", cx);
                            })),
                    )
                    .child(
                        text_button(
                            "invite-reset",
                            "Reset link",
                            Some(IconName::RotateCw),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.ask(Confirm::ResetLink, cx);
                        })),
                    ),
            );
        if self.social.confirm == Some(Confirm::ResetLink) {
            shown = shown.child(self.yes_no(
                "invite-reset-question",
                "Reset the invite link? The current link stops working for everyone who has it."
                    .into(),
                if self.social.busy {
                    "Resetting…"
                } else {
                    "Reset link"
                },
                palette,
                cx,
            ));
        }
        shown
    }

    /// A question with Cancel and one button that goes on. Its buttons
    /// are named after `selector` (`…-cancel`, `…-confirm`).
    fn yes_no(
        &self,
        selector: &'static str,
        words: SharedString,
        yes: &'static str,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        question(selector, words, palette).mx_0().child(
            div()
                .flex()
                .justify_end()
                .gap_2()
                .child(
                    small_button(
                        "question-cancel",
                        format!("{selector}-cancel"),
                        "Cancel",
                        palette,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.info_back(window, cx);
                    })),
                )
                .child(
                    small_button(
                        "question-confirm",
                        format!("{selector}-confirm"),
                        yes,
                        palette,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.confirmed(cx);
                    })),
                ),
        )
    }

    /// "Add participants": the address book without who is already in.
    fn render_adding(
        &self,
        account: &AccountId,
        group: &ChatId,
        people: &People,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let inside: Vec<ContactId> = self
            .engine
            .store()
            .group_participants(account, group, None, 2048)
            .unwrap_or_default()
            .into_iter()
            .map(|person| person.contact)
            .collect();
        div()
            .debug_selector(|| "group-adding".into())
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(self.render_people(account, people, &inside, palette, cx))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("people-cancel", "Cancel", None, false, palette).on_click(
                            cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.info_back(window, cx);
                            }),
                        ),
                    )
                    .child(
                        text_button(
                            "people-submit",
                            if self.social.busy { "Adding…" } else { "Add" },
                            Some(IconName::UserPlus),
                            true,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.submit_adding(cx);
                        })),
                    ),
            )
    }

    /// A people picker: who is chosen, a search field, and the contacts
    /// that match, ticked when chosen.
    fn render_people(
        &self,
        account: &AccountId,
        people: &People,
        without: &[ContactId],
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let rows = self.picker_rows(account, people, without, cx);
        let typed = !people.search.read(cx).value().trim().is_empty();
        let mut picker = div().flex().flex_col().gap_2();
        if !people.chosen.is_empty() {
            let names: Vec<String> = people.chosen.iter().map(|c| c.display_name()).collect();
            picker = picker.child(
                div()
                    .debug_selector(|| "people-chosen".into())
                    .flex()
                    .flex_col()
                    .child(badge(
                        &match names.len() {
                            1 => "1 chosen".to_owned(),
                            count => format!("{count} chosen"),
                        },
                        palette,
                    ))
                    .child(hint(names.join(", "), palette)),
            );
        }
        picker = picker.child(input_box("people-search", &people.search, false, palette));
        let mut list = div().flex().flex_col();
        for (index, contact) in rows.iter().enumerate() {
            let name = contact.display_name();
            let detail = contact
                .phone
                .as_ref()
                .map(|phone| format::phone(phone))
                .filter(|phone| *phone != name);
            let ticked = people.chosen.iter().any(|known| known.id == contact.id);
            let pick = contact.clone();
            list = list.child(
                self.person_row(
                    ("people-contact", index),
                    format!("people-contact-{index}"),
                    account,
                    &ChatId::new(contact.id.as_str()),
                    &name,
                    detail,
                    AvatarKind::Person,
                    palette,
                )
                .when(ticked, |this| {
                    this.bg(palette.muted)
                        .child(icon(IconName::Check, px(16.), palette.accent))
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_pick(pick.clone(), cx);
                })),
            );
        }
        picker = picker.child(list);
        if rows.is_empty() {
            let synced = self.engine.contacts_synced(account);
            picker = picker.child(
                hint(
                    if typed {
                        "No contact matches."
                    } else if synced {
                        "No contacts to choose from."
                    } else {
                        "Your contacts appear here once the phone has synced them."
                    },
                    palette,
                )
                .debug_selector(|| "people-empty".into()),
            );
        }
        picker
    }

    // ----- a new group ----------------------------------------------------

    /// The "New group" form.
    pub(super) fn render_new_group(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let form = self.social.new_group.as_ref()?;
        let picture: Div = match &form.picture {
            Some((_, drawn)) => div()
                .debug_selector(|| "new-group-picture-shown".into())
                .flex_none()
                .size(px(56.))
                .rounded_full()
                .overflow_hidden()
                .border_1()
                .border_color(palette.border)
                .child(
                    img(drawn.clone())
                        .size_full()
                        .rounded_full()
                        .object_fit(ObjectFit::Cover),
                ),
            None => super::widgets::avatar("", AvatarKind::Group, px(56.), palette),
        };
        Some(
            self.card("new-group", palette)
                .w(px(440.))
                .max_h(px(700.))
                .flex()
                .flex_col()
                .child(panel_header(
                    "New group",
                    palette,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.leave_new_group(window, cx);
                    }),
                ))
                .child(
                    div()
                        .id("new-group-scroll")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div().flex().items_center().gap_3().child(picture).child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(input_box(
                                        "new-group-subject",
                                        &form.subject,
                                        false,
                                        palette,
                                    ))
                                    .child(
                                        div().flex().child(
                                            small_button(
                                                "new-group-picture",
                                                "new-group-picture".into(),
                                                if form.picture.is_some() {
                                                    "Change picture"
                                                } else {
                                                    "Add a picture (optional)"
                                                },
                                                palette,
                                            )
                                            .on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    cx.stop_propagation();
                                                    this.choose_picture(PictureFor::NewGroup, cx);
                                                }),
                                            ),
                                        ),
                                    ),
                            ),
                        )
                        .child(super::widgets::label("Participants", palette))
                        .child(self.render_people(&form.account, &form.people, &[], palette, cx))
                        .children(
                            form.error
                                .clone()
                                .map(|error| error_line("new-group-error", error, palette)),
                        ),
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
                            text_button("new-group-cancel", "Cancel", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.leave_new_group(window, cx);
                                })),
                        )
                        .child(
                            text_button(
                                "new-group-create",
                                if form.busy {
                                    "Creating…"
                                } else {
                                    "Create group"
                                },
                                Some(IconName::Users),
                                true,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.submit_new_group(window, cx);
                                },
                            )),
                        ),
                ),
        )
    }
}
