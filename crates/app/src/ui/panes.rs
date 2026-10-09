//! The keyboard from pane to pane: the rail, the chat list, the
//! conversation.
//!
//! The conversation has the keyboard when its composer, or one of its
//! messages, does. The rail has it when one of its numbers or groups
//! does (they are focusable on their own). The chat list has it when the
//! shell itself holds the keyboard and says so (`Shell::pane_list`), or
//! when no conversation is open at all: then a row of the list wears the
//! same outline a message in focus does, the arrows move it, and single
//! keys act on that chat without opening it. Moving the outline opens
//! nothing, so nothing is marked as read by walking the list.

use super::shell::{ChatFilter, ListRow, Overlay, Shell};
use crate::keys::Command;
use crate::theme::metrics;
use client_core::ChatSummary;
use client_provider::{ChatChange, ChatId};
use gpui_kit::{point, App, Context, FocusHandle, ScrollStrategy, Window};

/// The filters, in the order Tab walks them.
const FILTERS: [ChatFilter; 5] = [
    ChatFilter::All,
    ChatFilter::Unread,
    ChatFilter::Groups,
    ChatFilter::Communities,
    ChatFilter::Archived,
];

/// The name the rail's keyboard knows a group by.
pub(super) fn rail_group_name(id: u32) -> String {
    format!("group:{id}")
}

impl Shell {
    // ----- the rail -----------------------------------------------------------

    /// What gives a number or a group of the rail the keyboard.
    pub(super) fn rail_handle(&self, name: &str, cx: &mut App) -> FocusHandle {
        self.rail_handles
            .borrow_mut()
            .entry(name.to_owned())
            .or_insert_with(|| cx.focus_handle())
            .clone()
    }

    /// The numbers and groups of the rail, top to bottom, as drawn.
    fn rail_stops(&self) -> Vec<String> {
        use crate::rail::RowKind;
        self.rail_rows
            .borrow()
            .iter()
            .filter_map(|row| match &row.what {
                RowKind::Account(_, key) | RowKind::Member(_, _, key) => Some(key.clone()),
                RowKind::Group(_, id) => Some(rail_group_name(*id)),
                RowKind::GroupEnd(..) => None,
            })
            .collect()
    }

    /// The number or group of the rail that has the keyboard.
    pub(super) fn rail_focused(&self, window: &Window) -> Option<String> {
        self.rail_handles
            .borrow()
            .iter()
            .find(|(_, handle)| handle.is_focused(window))
            .map(|(name, _)| name.clone())
    }

    /// Moves the keyboard up or down the rail.
    fn step_rail(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let stops = self.rail_stops();
        let Some(at) = self
            .rail_focused(window)
            .and_then(|name| stops.iter().position(|stop| *stop == name))
        else {
            return false;
        };
        let next = (at as isize + by).clamp(0, stops.len() as isize - 1) as usize;
        self.rail_handle(&stops[next], cx).focus(window, cx);
        cx.notify();
        true
    }

    /// The keyboard goes to the rail: to the number on screen, else to
    /// the first thing in it.
    pub(super) fn focus_rail(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let stops = self.rail_stops();
        let current = self.account.as_ref().map(|account| self.rail_key(account));
        let Some(stop) = current
            .filter(|key| stops.contains(key))
            .or_else(|| stops.first().cloned())
        else {
            return false;
        };
        self.pane_list = false;
        self.keys.clear();
        self.rail_handle(&stop, cx).focus(window, cx);
        cx.notify();
        true
    }

    // ----- the chat list ------------------------------------------------------

    /// Whether the keys of the chat list apply: the shell holds the
    /// keyboard for the list, with nothing over the panes.
    pub(super) fn list_has_keyboard(&self, window: &Window) -> bool {
        self.overlay == Overlay::None
            && !self.status_active()
            && self.keys.focused.is_none()
            && self.focus.is_focused(window)
            && (self.pane_list || self.open.is_none())
    }

    /// Where the keyboard can stop, with the row each is drawn in: the
    /// chats, and a community's row (which stands for the community's
    /// own group id). A search result's labels are not stops.
    fn list_stops(&self) -> Vec<(usize, ChatId)> {
        self.list_rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match row {
                ListRow::Chat(chat) => Some((index, chat.id.clone())),
                ListRow::Community(community) => Some((index, community.id.clone())),
                _ => None,
            })
            .collect()
    }

    /// The chat the list's outline is on, if it is still in the list and
    /// is a chat.
    pub(super) fn list_cursor_chat(&self) -> Option<ChatSummary> {
        let cursor = self.list_cursor.as_ref()?;
        self.list_rows.iter().find_map(|row| match row {
            ListRow::Chat(chat) if chat.id == *cursor => Some(chat.clone()),
            _ => None,
        })
    }

    /// The community the list's outline is on, if it is on one.
    fn list_cursor_community(&self) -> Option<ChatId> {
        let cursor = self.list_cursor.as_ref()?;
        self.list_rows.iter().find_map(|row| match row {
            ListRow::Community(community) if community.id == *cursor => Some(community.id.clone()),
            _ => None,
        })
    }

    /// Puts the outline on a chat and brings its row on screen.
    fn set_list_cursor(&mut self, chat: ChatId, cx: &mut Context<Self>) {
        if let Some((row, _)) = self
            .list_stops()
            .into_iter()
            .find(|(_, stop)| *stop == chat)
        {
            self.chat_scroll.scroll_to_item(row, ScrollStrategy::Top);
        }
        self.list_cursor = Some(chat);
        cx.notify();
    }

    /// The keyboard goes to the chat list: to the chat the outline was
    /// on, else to the open one, else to the first.
    pub(super) fn focus_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = Overlay::None;
        self.keys.clear();
        self.pane_list = true;
        let stops = self.list_stops();
        let known = |id: &ChatId| stops.iter().any(|(_, stop)| stop == id);
        let cursor = self
            .list_cursor
            .clone()
            .filter(|id| known(id))
            .or_else(|| {
                self.open
                    .as_ref()
                    .map(|open| open.chat.id.clone())
                    .filter(|id| known(id))
            })
            .or_else(|| stops.first().map(|(_, stop)| stop.clone()));
        self.focus.focus(window, cx);
        gpui_kit::base::TextSelection::clear(window, cx);
        match cursor {
            Some(chat) => self.set_list_cursor(chat, cx),
            None => cx.notify(),
        }
    }

    /// Moves the outline: by so many chats, clamped to the ends.
    fn step_list(&mut self, by: isize, cx: &mut Context<Self>) -> bool {
        let stops: Vec<ChatId> = self
            .list_stops()
            .into_iter()
            .map(|(_, stop)| stop)
            .collect();
        if stops.is_empty() {
            return false;
        }
        let at = self
            .list_cursor
            .as_ref()
            .and_then(|cursor| stops.iter().position(|id| id == cursor));
        let last = stops.len() as isize - 1;
        let next = match at {
            Some(at) => (at as isize).saturating_add(by).clamp(0, last),
            // Nothing had it yet: the first chat, or the last going up.
            None if by < 0 => last,
            None => 0,
        };
        self.set_list_cursor(stops[next as usize].clone(), cx);
        true
    }

    /// Puts the outline on the first or the last chat, wherever it was
    /// (it may be on a chat the list no longer shows).
    fn jump_list(&mut self, first: bool, cx: &mut Context<Self>) -> bool {
        let stop = {
            let stops = self.list_stops();
            let stop = if first { stops.first() } else { stops.last() };
            stop.map(|(_, stop)| stop.clone())
        };
        match stop {
            Some(chat) => {
                self.set_list_cursor(chat, cx);
                true
            }
            None => false,
        }
    }

    /// How many chats fit in the list as it is drawn.
    fn list_page(&self) -> isize {
        let height = self.chat_scroll.0.borrow().base_handle.bounds().size.height;
        ((height / metrics::CHAT_ROW_HEIGHT()).floor() as isize - 1).max(1)
    }

    /// The keyboard goes to the conversation: its composer.
    pub(super) fn focus_conversation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.open.is_none() {
            return false;
        }
        self.pane_list = false;
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
        cx.notify();
        true
    }

    /// A character typed on the list starts a search with it.
    pub(super) fn type_into_search(
        &mut self,
        stroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let held = &stroke.modifiers;
        if held.control || held.alt || held.platform || held.function {
            return false;
        }
        let Some(typed) = stroke.key_char.as_deref().filter(|typed| {
            !typed.is_empty() && typed.chars().all(|c| !c.is_control() && c != ' ')
        }) else {
            return false;
        };
        let typed = typed.to_owned();
        self.pane_list = false;
        self.set_search(&typed, window, cx);
        self.search.update(cx, |field, cx| field.focus(window, cx));
        true
    }

    /// Puts `text` in the search field and searches for it. (A value set
    /// from here is not a change the field reports.)
    fn set_search(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |field, cx| field.set_value(text, window, cx));
        self.query = text.trim().to_owned();
        self.reload_chats(cx);
        cx.notify();
    }

    /// Tab on the chat list walks the filters. Seen before the window's
    /// own Tab, which would move the keyboard to the next control.
    pub(super) fn list_tab(
        &mut self,
        stroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let held = &stroke.modifiers;
        if stroke.key != "tab"
            || held.control
            || held.alt
            || held.platform
            || !self.list_has_keyboard(window)
        {
            return false;
        }
        let command = if held.shift {
            Command::ListPreviousFilter
        } else {
            Command::ListNextFilter
        };
        self.run_pane_command(command, window, cx).unwrap_or(false)
    }

    // ----- the commands -------------------------------------------------------

    /// Runs a command of the panes, the chat list or the rail. `None`:
    /// not one of them.
    pub(super) fn run_pane_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        use Command as C;
        let on_rail = self.rail_focused(window).is_some();
        let on_list = self.list_has_keyboard(window);
        Some(match command {
            C::FocusList => {
                self.focus_list(window, cx);
                true
            }
            C::PaneLeft if on_rail => false,
            C::PaneLeft if on_list => self.focus_rail(window, cx),
            C::PaneLeft => {
                self.focus_list(window, cx);
                true
            }
            C::PaneRight | C::RailToList if on_rail => {
                self.focus_list(window, cx);
                true
            }
            C::PaneRight if on_list => self.focus_conversation(window, cx),
            C::PaneRight | C::RailToList => false,
            C::RailUp => self.step_rail(-1, window, cx),
            C::RailDown => self.step_rail(1, window, cx),
            C::LeaveConversation => {
                if self.thread_search.is_some() {
                    self.close_thread_search(window, cx);
                    true
                } else if self.open.is_some()
                    && !on_list
                    && self.composer.read(cx).value().is_empty()
                {
                    // The conversation stays as it is: only the keyboard
                    // leaves it.
                    self.focus_list(window, cx);
                    true
                } else {
                    false
                }
            }
            C::ListUp => self.step_list(-1, cx),
            C::ListDown => self.step_list(1, cx),
            C::ListFirst => self.jump_list(true, cx),
            C::ListLast => self.jump_list(false, cx),
            C::ListPageUp => self.step_list(-self.list_page(), cx),
            C::ListPageDown => self.step_list(self.list_page(), cx),
            C::ListOpen => {
                // A community opens its own screen, over the list.
                if let (Some(community), Some(account)) =
                    (self.list_cursor_community(), self.account.clone())
                {
                    self.open_info(
                        super::social::Target::Group {
                            account,
                            group: community,
                        },
                        window,
                        cx,
                    );
                    return Some(true);
                }
                let Some(chat) = self.list_cursor_chat() else {
                    return Some(false);
                };
                self.open_chat(chat.id, Some(window), cx);
                self.focus_conversation(window, cx)
            }
            C::ListSearch => {
                self.pane_list = false;
                self.search.update(cx, |field, cx| field.focus(window, cx));
                true
            }
            C::ListNextFilter | C::ListPreviousFilter => {
                // The communities are a stop only while some chat is in one.
                let stops: Vec<ChatFilter> = FILTERS
                    .into_iter()
                    .filter(|filter| *filter != ChatFilter::Communities || self.has_communities)
                    .collect();
                let at = stops
                    .iter()
                    .position(|filter| *filter == self.filter)
                    .unwrap_or(0);
                let by = if command == C::ListNextFilter {
                    1
                } else {
                    stops.len() - 1
                };
                self.set_filter(stops[(at + by) % stops.len()], cx);
                true
            }
            C::ListClear => {
                if self.query.is_empty() {
                    return Some(false);
                }
                self.set_search("", window, cx);
                true
            }
            C::ListPin | C::ListArchive | C::ListMute | C::ListToggleRead => {
                let Some(chat) = self.list_cursor_chat() else {
                    return Some(false);
                };
                if command == C::ListToggleRead && chat.unread_count > 0 {
                    self.engine.mark_read(&chat.account_id, &chat.id);
                    return Some(true);
                }
                if !self.engine.capabilities().chat_state {
                    return Some(false);
                }
                let change = match command {
                    C::ListPin => ChatChange::Pinned(!chat.pinned),
                    C::ListArchive => ChatChange::Archived(!chat.archived),
                    C::ListMute => ChatChange::Muted(!chat.muted),
                    _ => ChatChange::MarkedUnread,
                };
                self.engine.update_chat(&chat.account_id, &chat.id, change);
                true
            }
            C::ListMenu => {
                let Some(chat) = self.list_cursor_chat() else {
                    return Some(false);
                };
                // Beside the row, where a right-click would have been.
                let row = self
                    .list_stops()
                    .into_iter()
                    .find(|(_, stop)| *stop == chat.id)
                    .map(|(row, _)| row)
                    .unwrap_or(0);
                let (bounds, offset) = {
                    let state = self.chat_scroll.0.borrow();
                    (state.base_handle.bounds(), state.base_handle.offset())
                };
                let top = bounds.origin.y + offset.y + metrics::CHAT_ROW_HEIGHT() * row as f32;
                let at = point(
                    metrics::RAIL_WIDTH() + self.list_px - metrics::CHAT_ROW_HEIGHT(),
                    (top + metrics::CHAT_ROW_HEIGHT() / 2.)
                        .clamp(bounds.origin.y, bounds.origin.y + bounds.size.height),
                );
                self.open_row_menu(chat, at, window, cx);
                true
            }
            _ => return None,
        })
    }
}
