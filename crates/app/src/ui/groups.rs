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
    action_row, badge, block, hero, hint, paragraph, question, toggle_row, CommunityPeople,
    Confirm, EditWhat, GroupKind, GroupTab, People, PictureFor, Target, PEOPLE_SHOWN,
};
use super::widgets::{avatar_or, icon_button, label, mono, segmented, text_button, AvatarKind};
use crate::format;
use crate::i18n;
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::{StoredGroup, StoredParticipant};
use client_provider::{AccountId, ChatId, ContactId, GroupChange, GroupRole, ParticipantChange};
use gpui_kit::prelude::*;
use gpui_kit::{div, img, Context, Div, ObjectFit, SharedString, Stateful, StyledImage};

/// The line under a group's name in its panel. A community (`groups` is
/// how many groups it links) says that, not how many people its own group
/// holds, so that it is told apart from a group.
pub(super) fn title_line(kind: &str, groups: Option<usize>, participants: Option<usize>) -> String {
    match (groups, participants) {
        (Some(0), _) => kind.to_owned(),
        (Some(groups), _) => i18n::plural(
            "groups.group.title_groups",
            groups as u64,
            &[("kind", &kind)],
        ),
        (None, Some(count)) => i18n::plural(
            "groups.group.title_participants",
            count as u64,
            &[("kind", &kind)],
        ),
        (None, None) => kind.to_owned(),
    }
}

/// A group of rows of "Manage": one raised surface with a hairline
/// around it, its rows parted by hairlines.
fn card(palette: &Palette) -> Div {
    div()
        .rounded(metrics::PANEL_RADIUS())
        .border_1()
        .border_color(palette.border)
        .bg(palette.surface)
        .overflow_hidden()
        .flex()
        .flex_col()
}

/// One row of a [`card`]: what it is on the left, what is done with it on
/// the right. `parted` draws the hairline above every row but the first.
fn card_row(parted: bool, palette: &Palette) -> Div {
    div()
        .px_3()
        .py_3()
        .flex()
        .items_center()
        .gap_3()
        .when(parted, |this| {
            this.border_t_1().border_color(palette.border)
        })
}

/// A card under the label that says what it groups.
fn section(title: &str, card: Div, palette: &Palette) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(div().px_1().child(label(title, palette)))
        .child(card)
}

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
            Some(stored) if stored.group.community => i18n::t("groups.group.kind_community"),
            _ => i18n::t("groups.group.kind_group"),
        };
        // A community says how many groups it links, not how many people
        // its own group holds, so that it is told apart from a group.
        let summary = title_line(
            kind,
            stored
                .as_ref()
                .filter(|stored| stored.group.community)
                .map(|stored| stored.group.subgroups.len()),
            count,
        );
        let lines = vec![hint(summary, palette).debug_selector(|| "group-count".into())];
        let picture = self.openable_picture(picture, account, group, &subject, cx);
        let mut panel = div()
            .debug_selector(|| "group-info".into())
            .flex()
            .flex_col()
            .child(hero(picture, subject.clone().into(), lines, palette));

        let Some(stored) = stored else {
            // Never read: say so instead of showing an empty group.
            return panel.child(div().px_4().py_4().child(hint(
                if self.caps_here().group_info {
                    i18n::t("groups.group.reading")
                } else {
                    i18n::t("groups.group.no_details")
                },
                palette,
            )));
        };
        let can_manage = self.can_manage(&stored);

        if let Some(people) = &self.social.adding {
            return panel.child(self.render_adding(account, group, people, palette, cx));
        }
        if self.social.linking {
            return panel.child(self.render_linking(account, palette, cx));
        }

        let view = cx.entity().downgrade();
        let community = stored.group.community;
        let mut tabs = vec![
            (GroupTab::Overview, i18n::t("groups.group.tab_overview")),
            (
                GroupTab::People,
                if community {
                    i18n::t("groups.group.tab_members")
                } else {
                    i18n::t("groups.group.tab_participants")
                },
            ),
        ];
        if can_manage {
            tabs.push((GroupTab::Manage, i18n::t("groups.group.tab_manage")));
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
                            if community && tab == GroupTab::People {
                                this.load_community_people(cx);
                            }
                            cx.notify();
                        })
                        .ok();
                    },
                )),
        );
        panel.child(match tab {
            GroupTab::Overview => self.render_group_overview(&stored, palette, cx),
            GroupTab::People if community => self.render_community_members(&stored, palette, cx),
            GroupTab::People => self.render_group_people(&stored, can_manage, palette, cx),
            GroupTab::Manage => self.render_group_manage(&stored, palette, cx),
        })
    }

    /// Whether what only admins can do is offered: to an admin, and to a
    /// number whose role is not known (WhatsApp then decides).
    fn can_manage(&self, stored: &StoredGroup) -> bool {
        self.caps_here().group_manage
            && !stored.departed
            && stored.my_role.is_none_or(GroupRole::is_admin)
    }

    fn render_group_overview(
        &self,
        stored: &StoredGroup,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.caps_here();
        let details = &stored.group;
        let (account, group) = (&details.account_id, &details.id);
        let store = self.engine.store();
        let chat = store.chat(account, group).ok().flatten();
        let mut page = div().flex().flex_col();

        if let Some(description) = &details.description {
            page = page.child(
                block(i18n::t("groups.group.description"), palette)
                    .child(paragraph(description.clone(), palette))
                    .debug_selector(|| "group-description".into()),
            );
        }

        let mut facts = div().px_4().py_1().flex().flex_col();
        let role: SharedString = match (stored.departed, stored.my_role) {
            (true, _) => i18n::t("groups.group.role_departed").into(),
            (_, Some(GroupRole::Owner)) => i18n::t("groups.group.role_owner").into(),
            (_, Some(GroupRole::Admin)) => i18n::t("groups.group.role_admin").into(),
            (_, Some(GroupRole::Member)) => i18n::t("groups.group.role_member").into(),
            // TODO(wuapi-api): a group may list this number under a
            // hidden-number id the account object does not carry.
            (_, None) => i18n::t("groups.group.role_unknown").into(),
        };
        facts = facts.child(div().debug_selector(|| "group-role".into()).child(fact(
            i18n::t("groups.group.your_role"),
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
                Some(by) => i18n::tr(
                    "groups.group.created_by",
                    &[("date", &format::date(created)), ("by", &by)],
                ),
                None => format::date(created),
            };
            facts = facts.child(fact(i18n::t("groups.group.created"), text.into(), palette));
        }
        if details.announce {
            facts = facts.child(fact(
                i18n::t("groups.group.messages"),
                i18n::t("groups.group.only_admins_send").into(),
                palette,
            ));
        }
        page = page.child(facts);

        // The community this group is in, which opens in this panel, and
        // whether the group is its announcement group.
        if let Some(community) = &details.community_id {
            let name = store
                .group(account, community)
                .ok()
                .flatten()
                .map(|stored| stored.group.subject)
                .filter(|subject| !subject.is_empty())
                .unwrap_or_else(|| i18n::t("groups.group.community").to_owned());
            let target = Target::Group {
                account: account.clone(),
                group: community.clone(),
            };
            let line = block(i18n::t("groups.group.community"), palette).child(
                action_row("group-community", IconName::Users, name, false, palette)
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                            "groups.group.open_community",
                        ))
                        .build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.push_info(target.clone(), window, cx);
                    }))
                    .when(details.announcements, |this| {
                        this.child(
                            badge(i18n::t("groups.group.announcements"), palette)
                                .debug_selector(|| "group-announcements".into()),
                        )
                    })
                    .child(icon(IconName::ChevronRight, px(16.), palette.text_faint)),
            );
            page = page.child(line);
        }

        // A community's groups: the ones with a chat here open it, and
        // an admin adds groups to it and takes them out.
        let can_link = details.community && caps.community_manage && self.can_manage(stored);
        if details.community && (!details.subgroups.is_empty() || can_link) {
            let mut linked = block(i18n::t("groups.group.community_groups"), palette)
                .debug_selector(|| "group-subgroups".into());
            if can_link {
                let (community, name) = (group.clone(), details.subject.clone());
                linked = linked.child(
                    div()
                        .px_2()
                        .py_2()
                        .flex()
                        .flex_col()
                        .child(
                            action_row(
                                "community-add-group",
                                IconName::Users,
                                i18n::t("groups.group.add_existing"),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.begin_linking(cx);
                            })),
                        )
                        .child(
                            action_row(
                                "community-new-group",
                                IconName::UserPlus,
                                i18n::t("groups.group.new_in_community"),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_new_group(
                                        GroupKind::InCommunity {
                                            community: community.clone(),
                                            name: name.clone(),
                                        },
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        ),
                );
            }
            for (index, subgroup) in details.subgroups.iter().enumerate() {
                let has_chat = store.chat(account, &subgroup.id).ok().flatten().is_some();
                let opens = subgroup.id.clone();
                let removes = subgroup.id.clone();
                linked = linked.child(
                    div()
                        .id(("subgroup", index))
                        .debug_selector(move || format!("subgroup-{index}"))
                        .py_1()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .when(has_chat, |this| {
                            this.cursor_pointer().on_click(cx.listener(
                                move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_overlay(window, cx);
                                    this.open_chat(opens.clone(), Some(window), cx);
                                },
                            ))
                        })
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(metrics::TEXT_BODY())
                                .child(SharedString::from(subgroup.subject.clone())),
                        )
                        .child(
                            div()
                                .flex_none()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(subgroup.announcements, |this| {
                                    this.child(
                                        badge(i18n::t("groups.group.announcements"), palette)
                                            .debug_selector(move || {
                                                format!("subgroup-announcements-{index}")
                                            }),
                                    )
                                })
                                .when(!has_chat, |this| {
                                    this.child(
                                        badge(i18n::t("groups.group.not_joined"), palette)
                                            .debug_selector(move || {
                                                format!("subgroup-unjoined-{index}")
                                            }),
                                    )
                                })
                                // The announcement group stays in its
                                // community.
                                .when(can_link && !subgroup.announcements, |this| {
                                    this.child(
                                        small_button(
                                            ("subgroup-remove", index),
                                            format!("subgroup-remove-{index}"),
                                            i18n::t("groups.group.remove"),
                                            palette,
                                        )
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                cx.stop_propagation();
                                                this.ask(Confirm::Unlink(removes.clone()), cx);
                                            }),
                                        ),
                                    )
                                }),
                        ),
                );
                if self.social.confirm == Some(Confirm::Unlink(subgroup.id.clone())) {
                    let name = subgroup.subject.clone();
                    let busy = self.social.busy;
                    linked = linked.child(
                        question(
                            "subgroup-unlink-question",
                            i18n::tr(
                                "groups.group.unlink_question",
                                &[("name", &name), ("community", &details.subject)],
                            ),
                            palette,
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    text_button(
                                        "subgroup-unlink-cancel",
                                        i18n::t("groups.group.keep_it"),
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
                                        "subgroup-unlink-confirm",
                                        if busy {
                                            i18n::t("groups.group.removing")
                                        } else {
                                            i18n::t("groups.group.remove")
                                        },
                                        None,
                                        false,
                                        palette,
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.confirmed(cx);
                                        },
                                    )),
                                ),
                        ),
                    );
                }
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
                        i18n::t("groups.group.unmute")
                    } else {
                        i18n::t("groups.group.mute")
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
            actions = actions.children(self.gallery_row(account, group, palette, cx));
            let search_in = group.clone();
            actions = actions.child(
                action_row(
                    "group-search",
                    IconName::Search,
                    i18n::t("groups.group.search_in_chat"),
                    false,
                    palette,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.search_from_panel(search_in.clone(), window, cx);
                })),
            );
        }
        // Leaving a community is not offered here.
        if caps.group_leave && !stored.departed && !details.community {
            actions = actions.child(
                action_row(
                    "group-leave",
                    IconName::LogOut,
                    i18n::t("groups.group.leave"),
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
                    i18n::tr("groups.group.leave_question", &[("name", &details.subject)]),
                    palette,
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            text_button(
                                "group-leave-cancel",
                                i18n::t("groups.group.stay"),
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
                                "group-leave-confirm",
                                if busy {
                                    i18n::t("groups.group.leaving")
                                } else {
                                    i18n::t("groups.group.leave")
                                },
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
        let caps = self.caps_here();
        let (account, group) = (&stored.group.account_id, &stored.group.id);
        let store = self.engine.store();
        let mut page = div().flex().flex_col();

        // Who is waiting to be let in.
        if can_manage && caps.group_join_requests {
            self.engine.want_join_requests(account, group);
            let pending = store.join_requests(account, group).unwrap_or_default();
            if !pending.is_empty() {
                let mut waiting = block(i18n::t("groups.group.asking_to_join"), palette)
                    .debug_selector(|| "join-requests".into());
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
                                    i18n::t("groups.group.approve"),
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
                                    i18n::t("groups.group.reject"),
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
                    i18n::t("groups.group.add_participants"),
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
                    i18n::t("groups.group.no_list")
                } else {
                    i18n::t("groups.group.no_match")
                },
                palette,
            )));
        }
        if more {
            list = list.child(
                div().px_2().pt_1().child(
                    mono(i18n::t("groups.group.more_participants"))
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
            i18n::t("groups.group.you").to_owned()
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
                cx,
            )
            .when(expanded, |this| this.bg(palette.muted))
            .when(person.role.is_admin(), |this| {
                this.child(
                    div()
                        .debug_selector(move || format!("participant-admin-{index}"))
                        .child(badge(
                            if person.role == GroupRole::Owner {
                                i18n::t("groups.group.badge_owner")
                            } else {
                                i18n::t("groups.group.badge_admin")
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
                i18n::t("groups.group.view_profile"),
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
                    i18n::t("groups.group.make_admin"),
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
                    i18n::t("groups.group.dismiss_admin"),
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
                    i18n::t("groups.group.remove"),
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
                    i18n::tr(
                        "groups.group.remove_question",
                        &[("name", &name), ("group", &stored.group.subject)],
                    ),
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
                                i18n::t("groups.group.cancel"),
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
                                i18n::t("groups.group.remove"),
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

    /// "Manage": what an admin changes, in groups of rows on cards. What
    /// the group is (its name, description and picture), how people get
    /// in (the invite link), and what participants may do.
    fn render_group_manage(
        &self,
        stored: &StoredGroup,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.caps_here();
        let details = &stored.group;
        let (account, group) = (&details.account_id, &details.id);
        let has_picture = self
            .engine
            .store()
            .avatar(account, group)
            .ok()
            .flatten()
            .is_some_and(|picture| picture.image.is_some());
        let mut page = div().px_4().pt_4().pb_5().flex().flex_col().gap_5();

        // Name and description, each edited in place.
        let mut about = card(palette);
        for (index, (what, title, value, none, button, tip)) in [
            (
                EditWhat::Subject,
                i18n::t("groups.group.name"),
                details.subject.clone(),
                i18n::t("groups.group.no_name"),
                "group-edit-subject",
                i18n::t("groups.group.edit_name"),
            ),
            (
                EditWhat::Description,
                i18n::t("groups.group.description"),
                details.description.clone().unwrap_or_default(),
                i18n::t("groups.group.no_description"),
                "group-edit-description",
                i18n::t("groups.group.edit_description"),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let row = card_row(index > 0, palette);
            about = about.child(
                match self.social.edit.as_ref().filter(|edit| edit.what == what) {
                    Some(edit) => row
                        .flex_col()
                        .items_stretch()
                        .gap_2()
                        .child(label(title, palette))
                        .child(input_box("edit-input", &edit.input, false, palette))
                        .children(
                            edit.error
                                .clone()
                                .map(|error| error_line("edit-error", error, palette)),
                        )
                        .child(edit_buttons(palette, cx)),
                    None => {
                        let current = value.clone();
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(label(title, palette))
                                .child(if value.is_empty() {
                                    hint(none, palette)
                                } else {
                                    paragraph(value, palette)
                                }),
                        )
                        .child(
                            icon_button(button, IconName::Pencil, palette)
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(tip)
                                        .build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.begin_edit(what, current.clone(), window, cx);
                                })),
                        )
                    }
                },
            );
        }

        // The picture, beside what is done with it.
        let picture_for = PictureFor::Group(account.clone(), group.clone());
        about = about.child(
            card_row(true, palette)
                .child(avatar_or(
                    self.media.avatar(account, group),
                    &details.subject,
                    AvatarKind::Group,
                    px(40.),
                    palette,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(label(i18n::t("groups.group.picture"), palette))
                        .child(hint(
                            if has_picture {
                                i18n::t("groups.group.picture_seen")
                            } else {
                                i18n::t("groups.group.picture_none")
                            },
                            palette,
                        )),
                )
                .child(
                    text_button(
                        "group-picture-change",
                        if has_picture {
                            i18n::t("groups.group.change")
                        } else {
                            i18n::t("groups.group.add")
                        },
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
                        icon_button("group-picture-remove", IconName::Trash, palette)
                            .tooltip(|window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                                    "groups.group.remove_picture",
                                ))
                                .build(window, cx)
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.ask(Confirm::RemovePicture, cx);
                            })),
                    )
                }),
        );
        if self.social.confirm == Some(Confirm::RemovePicture) {
            about = about.child(card_row(true, palette).child(self.yes_no(
                "group-picture-question",
                i18n::t("groups.group.remove_picture_question").into(),
                i18n::t("groups.group.remove"),
                palette,
                cx,
            )));
        }
        page = page.child(section(i18n::t("groups.group.about"), about, palette));

        if caps.group_invites {
            page = page.child(section(
                i18n::t("groups.group.invite_link"),
                self.render_invite_link(stored, palette, cx),
                palette,
            ));
        }

        // The settings the provider exposes.
        let mut settings = card(palette);
        let unknown = i18n::t("groups.group.setting_unknown");
        for (index, (id, title, detail, on, change)) in [
            (
                "setting-announce",
                i18n::t("groups.group.setting_announce"),
                i18n::t("groups.group.setting_announce_detail"),
                Some(details.announce),
                GroupChange::Announce(!details.announce),
            ),
            (
                "setting-locked",
                i18n::t("groups.group.setting_locked"),
                i18n::t("groups.group.setting_locked_detail"),
                Some(details.locked),
                GroupChange::Locked(!details.locked),
            ),
            (
                "setting-approval",
                i18n::t("groups.group.setting_approval"),
                i18n::t("groups.group.setting_approval_detail"),
                details.join_approval,
                GroupChange::JoinApproval(!details.join_approval.unwrap_or(false)),
            ),
            (
                "setting-members-add",
                i18n::t("groups.group.setting_members_add"),
                i18n::t("groups.group.setting_members_add_detail"),
                details.members_can_add,
                GroupChange::MembersCanAdd(!details.members_can_add.unwrap_or(false)),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            // TODO(wuapi-api): `joinApproval` and `memberAddMode` cannot
            // be read back, so those two start as "not known".
            let (words, switch) = toggle_row(
                id,
                title,
                if on.is_some() { detail } else { unknown },
                on.unwrap_or(false),
                palette,
            );
            settings = settings.child(card_row(index > 0, palette).gap_4().child(words).child(
                switch.on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.change_group(change.clone(), cx);
                })),
            ));
        }
        page.child(section(
            i18n::t("groups.group.permissions"),
            settings,
            palette,
        ))
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
        let about = hint(i18n::t("groups.group.invite_about"), palette);
        let Some(link) = stored.invite_link.clone() else {
            return card(palette).child(
                card_row(false, palette)
                    .gap_4()
                    .child(div().flex_1().min_w_0().child(about))
                    .child(
                        text_button(
                            "invite-show",
                            if self.social.busy {
                                i18n::t("groups.group.reading_link")
                            } else {
                                i18n::t("groups.group.show_link")
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
        let mut shown = card(palette).child(
            card_row(false, palette)
                .flex_col()
                .items_stretch()
                .gap_2()
                .child(
                    // The link in a well of its own, with what is done
                    // with it at its end.
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
                                mono(link)
                                    .debug_selector(|| "invite-link".into())
                                    .text_color(palette.text),
                            ),
                        )
                        .child(
                            icon_button("invite-copy", IconName::Copy, palette)
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                                        "groups.group.copy_link",
                                    ))
                                    .build(window, cx)
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.copy_from_panel(
                                        copied.clone(),
                                        i18n::t("groups.group.link_copied"),
                                        cx,
                                    );
                                })),
                        )
                        .child(
                            icon_button("invite-reset", IconName::RotateCw, palette)
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                                        "groups.group.reset_link",
                                    ))
                                    .build(window, cx)
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.ask(Confirm::ResetLink, cx);
                                })),
                        ),
                )
                .child(about),
        );
        if self.social.confirm == Some(Confirm::ResetLink) {
            shown = shown.child(card_row(true, palette).child(self.yes_no(
                "invite-reset-question",
                i18n::t("groups.group.reset_question").into(),
                if self.social.busy {
                    i18n::t("groups.group.resetting")
                } else {
                    i18n::t("groups.group.reset_link")
                },
                palette,
                cx,
            )));
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
        question(selector, words, palette)
            .mx_0()
            .my_0()
            .flex_1()
            // In a row of a card: no wider than the row, its words wrapped.
            .min_w_0()
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        small_button(
                            "question-cancel",
                            format!("{selector}-cancel"),
                            i18n::t("groups.group.cancel"),
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
                        text_button(
                            "people-cancel",
                            i18n::t("groups.group.cancel"),
                            None,
                            false,
                            palette,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.info_back(window, cx);
                        })),
                    )
                    .child(
                        text_button(
                            "people-submit",
                            if self.social.busy {
                                i18n::t("groups.group.adding")
                            } else {
                                i18n::t("groups.group.add")
                            },
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
                        &i18n::plural("groups.group.chosen", names.len() as u64, &[]),
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
                    cx,
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
                        i18n::t("groups.group.no_contact_match")
                    } else if synced {
                        i18n::t("groups.group.no_contacts")
                    } else {
                        i18n::t("groups.group.contacts_syncing")
                    },
                    palette,
                )
                .debug_selector(|| "people-empty".into()),
            );
        }
        picker
    }

    // ----- a community's groups and members -------------------------------

    /// "Add an existing group": the groups the number could link to the
    /// community in the panel.
    fn render_linking(
        &self,
        account: &AccountId,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let choices = self
            .engine
            .store()
            .groups_to_link(account)
            .unwrap_or_default();
        let mut list = div()
            .debug_selector(|| "group-linking".into())
            .px_4()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(super::widgets::label(
                i18n::t("groups.group.add_a_group"),
                palette,
            ))
            .child(hint(i18n::t("groups.group.add_a_group_help"), palette));
        if choices.is_empty() {
            list = list.child(
                hint(i18n::t("groups.group.no_group_to_add"), palette)
                    .debug_selector(|| "link-none".into()),
            );
        }
        for (index, (id, subject)) in choices.into_iter().take(PEOPLE_SHOWN).enumerate() {
            list = list.child(
                div()
                    .id(("link-choice", index))
                    .debug_selector(move || format!("link-choice-{index}"))
                    .py_1()
                    .cursor_pointer()
                    .text_size(metrics::TEXT_BODY())
                    .truncate()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.link_group(id.clone(), cx);
                    }))
                    .child(SharedString::from(subject)),
            );
        }
        list.child(
            div().flex().justify_end().child(
                text_button(
                    "link-cancel",
                    i18n::t("groups.group.cancel"),
                    None,
                    false,
                    palette,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.info_back(window, cx);
                })),
            ),
        )
    }

    /// The people in all of a community's groups, as the provider listed
    /// them when the page was opened.
    fn render_community_members(
        &self,
        stored: &StoredGroup,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let account = &stored.group.account_id;
        let mut page = block(i18n::t("groups.group.community_members"), palette);
        match &self.social.community_people {
            None | Some(CommunityPeople::Loading) => {
                page = page.child(
                    hint(i18n::t("groups.group.reading_members"), palette)
                        .debug_selector(|| "community-members-loading".into()),
                );
            }
            Some(CommunityPeople::Failed(sentence)) => {
                page = page
                    .child(error_line(
                        "community-members-error",
                        sentence.clone(),
                        palette,
                    ))
                    .child(
                        div().flex().child(
                            small_button(
                                "community-members-retry",
                                "community-members-retry".into(),
                                i18n::t("groups.group.try_again"),
                                palette,
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.social.community_people = None;
                                this.load_community_people(cx);
                            })),
                        ),
                    );
            }
            Some(CommunityPeople::Loaded(people)) if people.is_empty() => {
                page = page.child(
                    hint(i18n::t("groups.group.no_members"), palette)
                        .debug_selector(|| "community-members-none".into()),
                );
            }
            Some(CommunityPeople::Loaded(people)) => {
                let store = self.engine.store();
                for (index, person) in people.iter().take(PEOPLE_SHOWN).enumerate() {
                    let name = store
                        .contact(account, person)
                        .ok()
                        .flatten()
                        .map(|known| known.display_name())
                        .unwrap_or_else(|| format::phone(person.as_str()));
                    page = page.child(
                        div()
                            .debug_selector(move || format!("community-member-{index}"))
                            .py_1()
                            .truncate()
                            .text_size(metrics::TEXT_BODY())
                            .child(SharedString::from(name)),
                    );
                }
                if people.len() > PEOPLE_SHOWN {
                    page = page.child(hint(
                        i18n::tr(
                            "groups.group.and_more",
                            &[("n", &(people.len() - PEOPLE_SHOWN))],
                        ),
                        palette,
                    ));
                }
            }
        }
        page
    }

    // ----- a new group ----------------------------------------------------

    /// The "New group" form.
    pub(super) fn render_new_group(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let form = self.social.new_group.as_ref()?;
        let (title, create, busy) = match &form.kind {
            GroupKind::Group => (
                i18n::t("groups.group.new_group").to_owned(),
                i18n::t("groups.group.create_group"),
                i18n::t("groups.group.creating"),
            ),
            GroupKind::Community => (
                i18n::t("groups.group.new_community").to_owned(),
                i18n::t("groups.group.create_community"),
                i18n::t("groups.group.creating"),
            ),
            GroupKind::InCommunity { name, .. } => (
                i18n::tr("groups.group.new_group_in", &[("name", name)]),
                i18n::t("groups.group.create_group"),
                i18n::t("groups.group.creating"),
            ),
        };
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
                    &title,
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
                                                    i18n::t("groups.group.change_picture")
                                                } else {
                                                    i18n::t("groups.group.add_picture")
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
                        .when(form.kind != GroupKind::Community, |this| {
                            this.child(super::widgets::label(
                                i18n::t("groups.group.tab_participants"),
                                palette,
                            ))
                            .child(self.render_people(
                                &form.account,
                                &form.people,
                                &[],
                                palette,
                                cx,
                            ))
                        })
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
                            text_button(
                                "new-group-cancel",
                                i18n::t("groups.group.cancel"),
                                None,
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.leave_new_group(window, cx);
                                },
                            )),
                        )
                        .child(
                            text_button(
                                "new-group-create",
                                if form.busy { busy } else { create },
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
