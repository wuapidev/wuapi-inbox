//! The rail's context menu and the cards it opens: what can be done to a
//! number or a group without dragging anything.
//!
//! Every move a drag can make is here too (move up or down, into a group,
//! out of it), next to what only a menu can offer: a name, a look, the
//! number's notifications, its session.

use super::numbers::NumberAsk;
use super::rail::ICON_SUBJECT;
use super::shell::{Overlay, PickKind, Shell};
use super::widgets::{label, text_button};
use crate::icons::{icon, IconName};
use crate::rail::{self, Dragged, Drop, Key, COLOURS};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_provider::{AccountId, ChatId, ConnectionState, Timestamp};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, Entity, Pixels, Point, SharedString, Stateful, Window};

/// Width of the rail's menu.
fn menu_width() -> Pixels {
    px(264.)
}

/// What a menu or a card of the rail is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui) enum RailTarget {
    Account(AccountId),
    Group(u32),
}

/// The rail's context menu.
pub(in crate::ui) struct RailMenu {
    pub(in crate::ui) target: RailTarget,
    at: Point<Pixels>,
    pub(in crate::ui) cursor: usize,
}

/// What an entry of the rail's menu does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::ui) enum RailAction {
    Rename,
    Look,
    EditProfile,
    MoveTo(u32),
    GroupWith(Key),
    LeaveGroup,
    MarkRead,
    ToggleMute,
    MoveUp,
    MoveDown,
    Reconnect,
    LogOut,
    Remove,
    GroupEdit,
    GroupToggle,
    Ungroup,
}

/// One entry of the rail's menu. Without an action it is shown disabled,
/// with the reason.
pub(in crate::ui) struct RailEntry {
    /// Name for tests.
    pub(in crate::ui) id: String,
    label: SharedString,
    icon: IconName,
    pub(in crate::ui) action: Option<RailAction>,
    hint: Option<&'static str>,
}

fn entry(
    id: &str,
    label: impl Into<SharedString>,
    icon: IconName,
    action: RailAction,
) -> RailEntry {
    RailEntry {
        id: id.to_owned(),
        label: label.into(),
        icon,
        action: Some(action),
        hint: None,
    }
}

fn disabled(id: &str, label: &'static str, icon: IconName, hint: &'static str) -> RailEntry {
    RailEntry {
        id: id.to_owned(),
        label: label.into(),
        icon,
        action: None,
        hint: Some(hint),
    }
}

/// What the rail's card is editing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ui) enum RailEditKind {
    /// A number's icon and colour.
    Look,
    /// A number's name, kept here only.
    Label,
    /// A group's name and colour.
    Group,
}

/// The card that edits a number's look, its local name, or a group.
pub(in crate::ui) struct RailEdit {
    pub(in crate::ui) target: RailTarget,
    pub(in crate::ui) kind: RailEditKind,
    pub(in crate::ui) input: Entity<InputState>,
    pub(in crate::ui) error: Option<SharedString>,
}

impl Shell {
    // ----- the menu ------------------------------------------------------

    /// Opens the menu of a number or a group, where the pointer is.
    pub(in crate::ui) fn open_rail_menu(
        &mut self,
        target: RailTarget,
        at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rail_menu = Some(RailMenu {
            target,
            at,
            cursor: 0,
        });
        self.overlay = Overlay::RailMenu;
        self.rail_edit = None;
        let first = self
            .rail_entries()
            .iter()
            .position(|entry| entry.action.is_some())
            .unwrap_or(0);
        if let Some(menu) = self.rail_menu.as_mut() {
            menu.cursor = first;
        }
        self.overlay_focus.focus(window, cx);
        cx.notify();
    }

    /// The entries of the open menu.
    pub(in crate::ui) fn rail_entries(&self) -> Vec<RailEntry> {
        let Some(menu) = &self.rail_menu else {
            return Vec::new();
        };
        let caps = self.engine.capabilities();
        let mut entries = Vec::new();
        match &menu.target {
            RailTarget::Account(id) => {
                let Some(account) = self.accounts.iter().find(|known| &known.id == id) else {
                    return entries;
                };
                let key = self.rail_key(id);
                let look = self.rail.look(&key);
                entries.push(entry(
                    "rail-rename",
                    "Rename",
                    IconName::SquarePen,
                    RailAction::Rename,
                ));
                entries.push(entry(
                    "rail-look",
                    "Change icon and colour",
                    IconName::Image,
                    RailAction::Look,
                ));
                if caps.profile_edit {
                    entries.push(entry(
                        "rail-profile",
                        "Edit WhatsApp profile",
                        IconName::Info,
                        RailAction::EditProfile,
                    ));
                }
                // Groups: join one, make one, leave one.
                let own_group = self.rail.group_of(&key);
                for group in self.rail.groups() {
                    if Some(group.id) != own_group {
                        entries.push(entry(
                            &format!("rail-move-{}", group.id),
                            format!("Move to {}", group.name),
                            IconName::Users,
                            RailAction::MoveTo(group.id),
                        ));
                    }
                }
                let others: Vec<Key> = self
                    .rail
                    .items
                    .iter()
                    .filter_map(|item| match item {
                        rail::RailItem::Account(other) if *other != key => Some(other.clone()),
                        _ => None,
                    })
                    .filter(|other| self.rail_account(other).is_some())
                    .collect();
                if others.is_empty() && self.rail.groups().is_empty() {
                    entries.push(disabled(
                        "rail-new-group",
                        "New group",
                        IconName::Users,
                        "Needs another number",
                    ));
                }
                for (place, other) in others.iter().take(4).enumerate() {
                    let name = self
                        .rail_account(other)
                        .map(|account| self.account_name(account))
                        .unwrap_or_default();
                    entries.push(entry(
                        &format!("rail-group-with-{place}"),
                        format!("New group with {name}"),
                        IconName::Plus,
                        RailAction::GroupWith(other.clone()),
                    ));
                }
                if own_group.is_some() {
                    entries.push(entry(
                        "rail-leave-group",
                        "Remove from group",
                        IconName::X,
                        RailAction::LeaveGroup,
                    ));
                }
                entries.push(entry(
                    "rail-up",
                    "Move up",
                    IconName::ChevronDown,
                    RailAction::MoveUp,
                ));
                entries.push(entry(
                    "rail-down",
                    "Move down",
                    IconName::ChevronDown,
                    RailAction::MoveDown,
                ));
                let unread = self.unread.get(id).copied().unwrap_or(0);
                entries.push(if unread > 0 {
                    entry(
                        "rail-read",
                        "Mark all as read",
                        IconName::CheckCheck,
                        RailAction::MarkRead,
                    )
                } else {
                    disabled(
                        "rail-read",
                        "Mark all as read",
                        IconName::CheckCheck,
                        "Nothing unread",
                    )
                });
                entries.push(entry(
                    "rail-mute",
                    if look.muted {
                        "Unmute notifications"
                    } else {
                        "Mute notifications for this number"
                    },
                    if look.muted {
                        IconName::Bell
                    } else {
                        IconName::BellOff
                    },
                    RailAction::ToggleMute,
                ));
                // The session.
                if !caps.manage_accounts {
                    entries.push(disabled(
                        "rail-reconnect",
                        "Reconnect",
                        IconName::RotateCw,
                        "Not with this provider",
                    ));
                } else if account.never_linked() {
                    entries.push(entry(
                        "rail-link",
                        "Link",
                        IconName::Phone,
                        RailAction::Reconnect,
                    ));
                    entries.push(entry(
                        "rail-remove",
                        "Remove",
                        IconName::Trash,
                        RailAction::Remove,
                    ));
                } else {
                    entries.push(entry(
                        "rail-reconnect",
                        if account.connection == ConnectionState::LoggedOut {
                            "Link again"
                        } else {
                            "Reconnect"
                        },
                        IconName::RotateCw,
                        RailAction::Reconnect,
                    ));
                    if account.connection != ConnectionState::LoggedOut {
                        entries.push(entry(
                            "rail-logout",
                            "Log out",
                            IconName::LogOut,
                            RailAction::LogOut,
                        ));
                    }
                }
            }
            RailTarget::Group(id) => {
                let Some(group) = self.rail.group(*id) else {
                    return entries;
                };
                entries.push(entry(
                    "rail-group-toggle",
                    if group.expanded { "Collapse" } else { "Expand" },
                    IconName::ChevronDown,
                    RailAction::GroupToggle,
                ));
                entries.push(entry(
                    "rail-group-edit",
                    "Rename and change colour",
                    IconName::SquarePen,
                    RailAction::GroupEdit,
                ));
                let unread: u32 = group
                    .members
                    .iter()
                    .filter_map(|key| self.rail_account(key))
                    .map(|account| self.unread.get(&account.id).copied().unwrap_or(0))
                    .sum();
                entries.push(if unread > 0 {
                    entry(
                        "rail-read",
                        "Mark all as read",
                        IconName::CheckCheck,
                        RailAction::MarkRead,
                    )
                } else {
                    disabled(
                        "rail-read",
                        "Mark all as read",
                        IconName::CheckCheck,
                        "Nothing unread",
                    )
                });
                entries.push(entry(
                    "rail-up",
                    "Move up",
                    IconName::ChevronDown,
                    RailAction::MoveUp,
                ));
                entries.push(entry(
                    "rail-down",
                    "Move down",
                    IconName::ChevronDown,
                    RailAction::MoveDown,
                ));
                entries.push(entry(
                    "rail-ungroup",
                    "Ungroup",
                    IconName::X,
                    RailAction::Ungroup,
                ));
            }
        }
        entries
    }

    /// Arrows and Enter in the rail's menu.
    pub(in crate::ui) fn rail_menu_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.rail_entries();
        let enabled: Vec<usize> = entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.action.is_some())
            .map(|(index, _)| index)
            .collect();
        let Some(menu) = self.rail_menu.as_mut() else {
            return;
        };
        let Some(place) = enabled.iter().position(|index| *index == menu.cursor) else {
            return;
        };
        match key {
            "down" => menu.cursor = enabled[(place + 1) % enabled.len()],
            "up" => menu.cursor = enabled[(place + enabled.len() - 1) % enabled.len()],
            "home" => menu.cursor = enabled[0],
            "end" => menu.cursor = enabled[enabled.len() - 1],
            "enter" | "space" => {
                if let Some(action) = entries[menu.cursor].action.clone() {
                    return self.run_rail_action(action, window, cx);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// Every unread chat of a number, marked as read.
    fn mark_account_read(&mut self, account: &AccountId) {
        if let Ok(chats) = self.engine.store().chats(account, None) {
            for chat in chats.iter().filter(|chat| chat.unread_count > 0) {
                self.engine.mark_read(account, &chat.id);
            }
        }
    }

    /// Does to a number what an entry of its menu does, without the menu:
    /// for the palette's commands.
    pub(in crate::ui) fn rail_action_on(
        &mut self,
        account: AccountId,
        action: RailAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rail_menu = Some(RailMenu {
            target: RailTarget::Account(account),
            at: Point::default(),
            cursor: 0,
        });
        self.run_rail_action(action, window, cx);
        // An action that opens no card leaves no menu behind.
        self.rail_menu = None;
    }

    /// The same for a group of the rail.
    pub(in crate::ui) fn rail_group_action(
        &mut self,
        group: u32,
        action: RailAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.rail_menu = Some(RailMenu {
            target: RailTarget::Group(group),
            at: Point::default(),
            cursor: 0,
        });
        self.run_rail_action(action, window, cx);
        self.rail_menu = None;
    }

    /// Does what an entry of the rail's menu says.
    pub(in crate::ui) fn run_rail_action(
        &mut self,
        action: RailAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.rail_menu.as_ref().map(|menu| menu.target.clone()) else {
            return;
        };
        let dragged = match &target {
            RailTarget::Account(account) => Dragged::Account(self.rail_key(account)),
            RailTarget::Group(id) => Dragged::Group(*id),
        };
        let caps = self.engine.capabilities();
        match (action, target.clone()) {
            (RailAction::Rename, RailTarget::Account(account)) => {
                self.rail_menu = None;
                return if caps.manage_accounts {
                    // The name is the provider's; if it refuses, the name
                    // is kept here instead (see `confirm_number`).
                    self.ask_number(account, NumberAsk::Rename, window, cx)
                } else {
                    self.open_rail_edit(target, RailEditKind::Label, window, cx)
                };
            }
            (RailAction::Look, _) => {
                self.rail_menu = None;
                return self.open_rail_edit(target, RailEditKind::Look, window, cx);
            }
            (RailAction::GroupEdit, _) => {
                self.rail_menu = None;
                return self.open_rail_edit(target, RailEditKind::Group, window, cx);
            }
            (RailAction::EditProfile, RailTarget::Account(account)) => {
                self.rail_menu = None;
                return self.open_own_profile(account, window, cx);
            }
            (RailAction::Reconnect, RailTarget::Account(account)) => {
                self.rail_menu = None;
                return self.reconnect_number(account, window, cx);
            }
            (RailAction::LogOut, RailTarget::Account(account)) => {
                self.rail_menu = None;
                return self.ask_number(account, NumberAsk::LogOut, window, cx);
            }
            (RailAction::Remove, RailTarget::Account(account)) => {
                self.rail_menu = None;
                return self.ask_number(account, NumberAsk::Remove, window, cx);
            }
            (RailAction::MoveTo(group), _) => {
                self.change_rail(
                    |rail| {
                        rail.apply(&dragged, &Drop::OntoGroup(group));
                    },
                    cx,
                );
            }
            (RailAction::GroupWith(other), RailTarget::Account(account)) => {
                let key = self.rail_key(&account);
                self.change_rail(
                    |rail| {
                        rail.group_with(&key, &other);
                    },
                    cx,
                );
            }
            (RailAction::LeaveGroup, RailTarget::Account(account)) => {
                let key = self.rail_key(&account);
                self.change_rail(
                    |rail| {
                        rail.leave_group(&key);
                    },
                    cx,
                );
            }
            (RailAction::MoveUp, _) => self.change_rail(
                |rail| {
                    rail.nudge(&dragged, true);
                },
                cx,
            ),
            (RailAction::MoveDown, _) => self.change_rail(
                |rail| {
                    rail.nudge(&dragged, false);
                },
                cx,
            ),
            (RailAction::MarkRead, RailTarget::Account(account)) => {
                self.mark_account_read(&account);
            }
            (RailAction::MarkRead, RailTarget::Group(id)) => {
                let members: Vec<AccountId> = self
                    .rail
                    .group(id)
                    .map(|group| {
                        group
                            .members
                            .iter()
                            .filter_map(|key| self.rail_account(key))
                            .map(|account| account.id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                for account in members {
                    self.mark_account_read(&account);
                }
            }
            (RailAction::ToggleMute, RailTarget::Account(account)) => {
                let key = self.rail_key(&account);
                self.change_rail(
                    |rail| rail.set_look(&key, |look| look.muted = !look.muted),
                    cx,
                );
            }
            (RailAction::GroupToggle, RailTarget::Group(id)) => {
                self.toggle_rail_group(id, cx);
            }
            (RailAction::Ungroup, RailTarget::Group(id)) => {
                self.change_rail(
                    |rail| {
                        rail.ungroup(id);
                    },
                    cx,
                );
            }
            _ => {}
        }
        self.close_overlay(window, cx);
    }

    pub(in crate::ui) fn render_rail_menu(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let menu = self.rail_menu.as_ref()?;
        let mut card = self
            .card("rail-menu", palette)
            .absolute()
            .left(menu.at.x.max(metrics::RAIL_WIDTH() - px(12.)))
            .top(menu.at.y)
            .w(menu_width())
            .p_1()
            .flex()
            .flex_col();
        for (index, item) in self.rail_entries().into_iter().enumerate() {
            let name = item.id.clone();
            let row = div()
                .id(("rail-menu-item", index))
                .debug_selector({
                    let name = name.clone();
                    move || name.clone()
                })
                .min_h(px(32.))
                .px_2()
                .rounded(px(4.))
                .flex()
                .items_center()
                .gap_2()
                .text_size(metrics::TEXT_BODY());
            card = card.child(match item.action {
                Some(action) => {
                    let hover = palette.muted;
                    row.cursor_pointer()
                        .text_color(palette.text)
                        .when(index == menu.cursor, |this| this.bg(palette.muted))
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.run_rail_action(action.clone(), window, cx);
                        }))
                        .child(icon(item.icon, px(15.), palette.icon))
                        .child(
                            div()
                                .debug_selector({
                                    let name = name.clone();
                                    move || format!("{name}-label")
                                })
                                .flex_1()
                                .min_w_0()
                                .child(item.label),
                        )
                        .into_any_element()
                }
                // The whole label first, and the reason under it.
                None => {
                    let reason = item.hint.filter(|hint| !hint.is_empty());
                    let tip: Option<gpui_kit::SharedString> =
                        reason.map(gpui_kit::SharedString::from);
                    let (label_name, reason_name) = (name.clone(), name.clone());
                    row.text_color(palette.text_faint)
                        .py(px(3.))
                        .child(icon(item.icon, px(15.), palette.text_faint))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .debug_selector(move || format!("{label_name}-label"))
                                        .child(item.label),
                                )
                                .children(reason.map(|reason| {
                                    div()
                                        .debug_selector(move || format!("{reason_name}-reason"))
                                        .text_size(px(10.))
                                        .text_color(palette.text_faint)
                                        .child(reason)
                                })),
                        )
                        .when_some(tip, |this, tip| {
                            this.tooltip(move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(tip.clone())
                                    .build(window, cx)
                            })
                        })
                        .into_any_element()
                }
            });
        }
        Some(card)
    }

    // ----- the card ------------------------------------------------------

    /// Opens the card that edits a number's look, its local name, or a
    /// group.
    pub(in crate::ui) fn open_rail_edit(
        &mut self,
        target: RailTarget,
        kind: RailEditKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (value, placeholder) = match (&target, kind) {
            (RailTarget::Account(account), RailEditKind::Look) => (
                self.rail
                    .look(&self.rail_key(account))
                    .glyph
                    .unwrap_or_default(),
                "An emoji or two letters",
            ),
            (RailTarget::Account(account), _) => (
                self.accounts
                    .iter()
                    .find(|known| &known.id == account)
                    .map(|known| self.account_name(known))
                    .unwrap_or_default(),
                "A name for this number",
            ),
            (RailTarget::Group(id), _) => (
                self.rail
                    .group(*id)
                    .map(|group| group.name.clone())
                    .unwrap_or_default(),
                "A name for this group",
            ),
        };
        let input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        input.update(cx, |field, cx| {
            field.set_value(value, window, cx);
            field.focus(window, cx);
        });
        self.rail_edit = Some(RailEdit {
            target,
            kind,
            input,
            error: None,
        });
        self.rail_menu = None;
        self.overlay = Overlay::RailEdit;
        cx.notify();
    }

    /// Picks a colour on the card: applied at once.
    fn pick_rail_colour(&mut self, colour: Option<u8>, cx: &mut Context<Self>) {
        let Some(edit) = &self.rail_edit else {
            return;
        };
        match edit.target.clone() {
            RailTarget::Account(account) => {
                let key = self.rail_key(&account);
                self.change_rail(
                    |rail| {
                        rail.set_look(&key, |look| {
                            look.colour = colour;
                            if colour.is_some() {
                                // A colour is the background of a glyph: the
                                // chosen picture steps aside.
                                look.image = false;
                            }
                        })
                    },
                    cx,
                );
            }
            RailTarget::Group(id) => {
                self.change_rail(|rail| rail.colour_group(id, colour.unwrap_or(0)), cx);
            }
        }
    }

    /// "Done", or Enter: what was typed is kept.
    pub(in crate::ui) fn finish_rail_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(edit) = &self.rail_edit else {
            return;
        };
        let typed = edit.input.read(cx).value().trim().to_owned();
        match (edit.target.clone(), edit.kind) {
            (RailTarget::Account(account), RailEditKind::Look) => {
                let key = self.rail_key(&account);
                // An emoji, or a couple of letters: no more fits.
                let glyph: String = typed.chars().take(4).collect();
                self.change_rail(
                    |rail| {
                        rail.set_look(&key, |look| {
                            look.glyph = (!glyph.is_empty()).then_some(glyph);
                            if look.glyph.is_some() {
                                look.image = false;
                            }
                        })
                    },
                    cx,
                );
            }
            (RailTarget::Account(account), _) => {
                let key = self.rail_key(&account);
                let provider_name = self
                    .accounts
                    .iter()
                    .find(|known| known.id == account)
                    .map(|known| known.display_name.clone());
                self.change_rail(
                    |rail| {
                        rail.set_look(&key, |look| {
                            // The provider's own name back: no label to keep.
                            look.label = (!typed.is_empty()
                                && Some(&typed) != provider_name.as_ref())
                            .then_some(typed);
                        })
                    },
                    cx,
                );
            }
            (RailTarget::Group(id), _) => {
                self.change_rail(|rail| rail.rename_group(id, &typed), cx);
            }
        }
        self.close_overlay(window, cx);
    }

    /// "Choose a picture": a file from the system's dialog, made small and
    /// kept in the encrypted store, under the number.
    fn pick_rail_image(&mut self, cx: &mut Context<Self>) {
        let Some(RailTarget::Account(account)) =
            self.rail_edit.as_ref().map(|edit| edit.target.clone())
        else {
            return;
        };
        let picked = (self.pick_files)(PickKind::Image, cx);
        let store = self.engine.store().clone();
        let runtime = self.engine.runtime().clone();
        self._rail_work = Some(cx.spawn(async move |this, cx| {
            let Some(path) = picked.await.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            // Read and scaled off every thread that matters (the file may
            // be large); the store is written from the runtime's own task.
            let (to_store, of, blocking) = (store.clone(), account.clone(), runtime.clone());
            let work = runtime.spawn(async move {
                let small = blocking
                    .spawn_blocking(move || -> Result<Vec<u8>, String> {
                        let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
                        if size > 20 * 1024 * 1024 {
                            return Err("That picture is larger than 20 MB.".to_owned());
                        }
                        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                        client_core::thumbnail(&bytes, 128)
                            .map(|small| small.bytes)
                            .map_err(|_| "That file is not a picture this can show.".to_owned())
                    })
                    .await
                    .map_err(|e| e.to_string())??;
                to_store
                    .put_avatar(
                        &of,
                        &ChatId::new(ICON_SUBJECT),
                        Some(("local", &small)),
                        Timestamp::now(),
                    )
                    .map_err(|e| e.to_string())
            });
            let outcome = work.await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(())) => {
                        this.media
                            .forget_avatar(&account, &ChatId::new(ICON_SUBJECT));
                        let key = this.rail_key(&account);
                        this.change_rail(|rail| rail.set_look(&key, |look| look.image = true), cx);
                    }
                    Ok(Err(reason)) => {
                        if let Some(edit) = this.rail_edit.as_mut() {
                            edit.error = Some(reason.into());
                        }
                    }
                    _ => {}
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Back to the number's own WhatsApp picture and the provider's name.
    fn reset_rail_look(&mut self, cx: &mut Context<Self>) {
        let Some(RailTarget::Account(account)) =
            self.rail_edit.as_ref().map(|edit| edit.target.clone())
        else {
            return;
        };
        let key = self.rail_key(&account);
        self.change_rail(
            |rail| {
                rail.set_look(&key, |look| {
                    look.glyph = None;
                    look.colour = None;
                    look.image = false;
                })
            },
            cx,
        );
    }

    pub(in crate::ui) fn render_rail_edit(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let edit = self.rail_edit.as_ref()?;
        let (title, field, chosen): (&str, &str, Option<u8>) = match (&edit.target, edit.kind) {
            (RailTarget::Account(account), RailEditKind::Look) => (
                "Icon and colour",
                "Emoji or initials",
                self.rail.look(&self.rail_key(account)).colour,
            ),
            (RailTarget::Account(_), _) => ("Name on this computer", "Name", None),
            (RailTarget::Group(id), _) => (
                "Group",
                "Name",
                self.rail.group(*id).map(|group| group.colour),
            ),
        };
        let with_colours = edit.kind != RailEditKind::Label;
        let mut swatches = div().flex().items_center().gap_2();
        for colour in 0..COLOURS {
            let fill = palette.rail[usize::from(colour)];
            let ring = palette.text;
            swatches = swatches.child(
                div()
                    .id(("rail-colour", usize::from(colour)))
                    .debug_selector(move || format!("rail-colour-{colour}"))
                    .size(px(24.))
                    .rounded_full()
                    .bg(fill)
                    .border_2()
                    .border_color(if chosen == Some(colour) {
                        ring
                    } else {
                        palette.surface
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.pick_rail_colour(Some(colour), cx);
                    })),
            );
        }
        let is_look = edit.kind == RailEditKind::Look;
        Some(
            self.card("rail-edit", palette)
                .w(px(380.))
                .flex()
                .flex_col()
                .child(super::menus::panel_header(
                    title,
                    palette,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_overlay(window, cx);
                    }),
                ))
                .child(
                    div()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(label(field, palette))
                                .child(
                                    div()
                                        .debug_selector(|| "rail-edit-input".into())
                                        .h(px(36.))
                                        .px_2()
                                        .rounded(metrics::RADIUS())
                                        .border_1()
                                        .border_color(palette.border)
                                        .bg(palette.background)
                                        .flex()
                                        .items_center()
                                        .child(Input::new(&edit.input).appearance(false)),
                                ),
                        )
                        .when(with_colours, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(label("Colour", palette))
                                    .child(swatches),
                            )
                        })
                        .when(is_look, |this| {
                            this.child(
                                div()
                                    .text_size(metrics::TEXT_SMALL())
                                    .line_height(px(18.))
                                    .text_color(palette.text_muted)
                                    .child(
                                        "Only on this computer. Without any of these the \
                                         number shows its own WhatsApp picture.",
                                    ),
                            )
                        })
                        .children(edit.error.clone().map(|error| {
                            div()
                                .debug_selector(|| "rail-edit-error".into())
                                .text_size(metrics::TEXT_SMALL())
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
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .when(is_look, |this| {
                            this.child(
                                text_button(
                                    "rail-edit-image",
                                    "Choose a picture",
                                    Some(IconName::Image),
                                    false,
                                    palette,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.pick_rail_image(cx);
                                    },
                                )),
                            )
                            .child(
                                text_button("rail-edit-reset", "Reset", None, false, palette)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.reset_rail_look(cx);
                                        this.close_overlay(window, cx);
                                    })),
                            )
                        })
                        .child(
                            text_button("rail-edit-done", "Done", None, true, palette).on_click(
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.finish_rail_edit(window, cx);
                                }),
                            ),
                        ),
                ),
        )
    }
}
