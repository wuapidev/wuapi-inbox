//! The forward sheet: the palette's look and keys, with a box on each
//! row. Every chat is there, the ones opened last first, and somebody of
//! the address book with no chat yet; what is typed narrows them the way
//! the palette does. The arrows walk the rows, Enter (or a click) picks
//! one, Cmd or Ctrl with Enter sends to every chat picked.

use super::message_actions::{copy_raw_text, message_kind, not_forwarded, FORWARD_LIMIT};
use super::palette::rank;
use super::shell::{Overlay, Shell};
use super::widgets::{avatar_or, mono, text_button, AvatarKind};
use crate::format::list_time;
use crate::icons::{icon, IconName};
use crate::keys::{self, Command};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::ChatSummary;
use client_provider::{ChatId, ChatKind, Contact, Message, Timestamp};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, AnyElement, Context, Div, FontWeight, KeyDownEvent, SharedString, Stateful, Window,
};

/// How many chats the sheet lists with nothing typed.
const BROWSE: usize = 60;
/// How many chats, and people, it lists for what is typed.
const CHATS: usize = 40;
const PEOPLE: usize = 8;

/// One row of the sheet.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ForwardRow {
    /// A heading.
    Section(&'static str),
    Chat(ChatSummary),
    /// Somebody with no chat yet.
    Person(Contact),
}

impl ForwardRow {
    /// What picking it sends to: the chat, its name and its kind.
    fn target(&self) -> Option<(ChatId, String, ChatKind)> {
        match self {
            Self::Section(_) => None,
            Self::Chat(chat) => Some((chat.id.clone(), chat.title.clone(), chat.kind)),
            Self::Person(contact) => Some((
                ChatId::new(contact.id.as_str()),
                contact.display_name(),
                ChatKind::Direct,
            )),
        }
    }
}

impl Shell {
    /// Reads what the forward sheet lists: with nothing typed, the chats
    /// opened last and then every chat in the list's order; otherwise
    /// the chats and the people that match, best first. Read when the
    /// sheet opens and when the text changes, not while it is drawn.
    pub(super) fn refresh_forward_rows(&mut self, cx: &mut Context<Self>) {
        let typed = self.acting.forward_input.read(cx).value().trim().to_owned();
        let mut rows = Vec::new();
        if let Some(account) = &self.account {
            let store = self.engine.store();
            let section = |rows: &mut Vec<ForwardRow>, title, found: Vec<ForwardRow>| {
                if !found.is_empty() {
                    rows.push(ForwardRow::Section(title));
                    rows.extend(found);
                }
            };
            if typed.is_empty() {
                let chats = store.chats(account, None).unwrap_or_default();
                let recent: Vec<ForwardRow> = self
                    .palette
                    .recent_chats
                    .iter()
                    .filter_map(|id| chats.iter().find(|chat| &chat.id == id))
                    .map(|chat| ForwardRow::Chat(chat.clone()))
                    .collect();
                let rest: Vec<ForwardRow> = chats
                    .iter()
                    .filter(|chat| !self.palette.recent_chats.contains(&chat.id))
                    .take(BROWSE.saturating_sub(recent.len()))
                    .map(|chat| ForwardRow::Chat(chat.clone()))
                    .collect();
                section(
                    &mut rows,
                    crate::i18n::t("commands.palette.section_recent"),
                    recent,
                );
                section(
                    &mut rows,
                    crate::i18n::t("commands.palette.section_chats"),
                    rest,
                );
            } else {
                let chats = store.chats(account, Some(&typed)).unwrap_or_default();
                // Somebody with a chat is listed as that chat already.
                let people: Vec<Contact> = store
                    .contacts(account, Some(&typed), PEOPLE * 4)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|contact| {
                        !chats
                            .iter()
                            .any(|chat| chat.id.as_str() == contact.id.as_str())
                            && store
                                .chat(account, &ChatId::new(contact.id.as_str()))
                                .ok()
                                .flatten()
                                .is_none()
                    })
                    .collect();
                let chats = rank(&typed, chats, |chat| chat.title.clone());
                let people = rank(&typed, people, |contact| contact.display_name());
                section(
                    &mut rows,
                    crate::i18n::t("commands.palette.section_chats"),
                    chats
                        .into_iter()
                        .take(CHATS)
                        .map(ForwardRow::Chat)
                        .collect(),
                );
                section(
                    &mut rows,
                    crate::i18n::t("commands.palette.section_people"),
                    people
                        .into_iter()
                        .take(PEOPLE)
                        .map(ForwardRow::Person)
                        .collect(),
                );
            }
        }
        self.acting.forward_rows = rows;
        self.acting.forward_cursor = 0;
        self.acting.forward_scroll.scroll_to_item(0);
        cx.notify();
    }

    /// What the forward sheet lists that can be picked, in order: the
    /// chat, its name and its kind.
    pub(super) fn forward_rows(&self, _: &gpui_kit::App) -> Vec<(ChatId, String, ChatKind)> {
        self.acting
            .forward_rows
            .iter()
            .filter_map(ForwardRow::target)
            .collect()
    }

    /// Opens the forward sheet for the messages an action is about. What
    /// cannot be forwarded is left out, each with its reason, and the
    /// rest go on: the sheet says what was left out.
    pub(super) fn begin_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut subjects: Vec<Message> = Vec::new();
        let mut skipped: Vec<(&'static str, &'static str)> = Vec::new();
        for message in self.subjects() {
            match self.command_state(Command::Forward, &message) {
                super::message_menu::CommandState::Enabled => subjects.push(message),
                super::message_menu::CommandState::Disabled(why) => {
                    skipped.push((message_kind(&message), why))
                }
                super::message_menu::CommandState::Hidden => {}
            }
        }
        if subjects.is_empty() {
            if !skipped.is_empty() {
                self.show_problem(not_forwarded(&skipped), cx);
            }
            return false;
        }
        self.acting.subjects = subjects;
        self.acting.forward_skipped = skipped;
        self.acting.forward_to.clear();
        self.acting.forward_names.clear();
        self.acting.forward_people.clear();
        self.acting.forward_cursor = 0;
        self.open_overlay(Overlay::Forward, window, cx);
        self.acting.forward_input.update(cx, |field, cx| {
            field.set_value("", window, cx);
            field.focus(window, cx);
        });
        self.refresh_forward_rows(cx);
        true
    }

    /// Picks the row at `index` of those that can be picked, or lets it go.
    pub(super) fn toggle_forward_row(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self
            .acting
            .forward_rows
            .iter()
            .filter(|row| row.target().is_some())
            .nth(index)
            .cloned()
        else {
            return;
        };
        let Some((chat, name, _)) = row.target() else {
            return;
        };
        self.acting.forward_names.insert(chat.clone(), name);
        if let ForwardRow::Person(contact) = row {
            self.acting.forward_people.insert(chat.clone(), contact);
        }
        self.acting.forward_cursor = index;
        self.toggle_forward_to(chat, cx);
    }

    /// Picks a chat of the forward sheet, or lets it go. No more than
    /// [`FORWARD_LIMIT`] at once.
    pub(super) fn toggle_forward_to(&mut self, chat: ChatId, cx: &mut Context<Self>) {
        let picked = &mut self.acting.forward_to;
        match picked.iter().position(|known| *known == chat) {
            Some(at) => {
                picked.remove(at);
            }
            None if picked.len() < FORWARD_LIMIT => picked.push(chat),
            None => {}
        }
        cx.notify();
    }

    /// Sends what the sheet is about to every chat picked, in the order
    /// the messages were written: each copy is a message of its own,
    /// marked as forwarded, queued through the outbox (so a dropped
    /// connection never loses or doubles it) and shown in its chat at
    /// once. Somebody with no chat yet gets one. What cannot be queued is
    /// said, with its reason, and does not stop the others.
    pub(super) fn send_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.acting.forward_to.is_empty() {
            return;
        }
        let Some(account) = self.account.clone() else {
            return;
        };
        let picked = std::mem::take(&mut self.acting.forward_to);
        let people = std::mem::take(&mut self.acting.forward_people);
        let mut subjects = std::mem::take(&mut self.acting.subjects);
        subjects.sort_by_key(|message| message.timestamp);
        let mut refused: Vec<(&'static str, &'static str)> =
            std::mem::take(&mut self.acting.forward_skipped);
        let mut to = Vec::new();
        let mut made = false;
        for chat in picked {
            match people.get(&chat) {
                Some(contact) => match self.engine.chat_with_contact(contact) {
                    Ok(chat) => {
                        made = true;
                        to.push(chat);
                    }
                    Err(error) => {
                        tracing::error!(%error, "could not make the chat to forward to");
                        for message in &subjects {
                            let why = crate::i18n::t("chat.message_actions.forward_save_failed");
                            if !refused.contains(&(message_kind(message), why)) {
                                refused.push((message_kind(message), why));
                            }
                        }
                    }
                },
                None => to.push(chat),
            }
        }
        for message in &subjects {
            for chat in &to {
                let queued = match copy_raw_text(message) {
                    // A text keeps its mentions as they were typed.
                    Some(text) => self
                        .engine
                        .forward_text(&account, chat, text)
                        .map(drop)
                        .map_err(client_core::ForwardError::from),
                    None => self
                        .engine
                        .forward_message(&account, message, chat)
                        .map(drop),
                };
                if let Err(error) = queued {
                    tracing::error!(%error, "could not queue the forward");
                    let why = match error {
                        client_core::ForwardError::Refused(refusal) => refusal.reason(),
                        client_core::ForwardError::Store(_) => {
                            crate::i18n::t("chat.message_actions.forward_save_failed")
                        }
                    };
                    if !refused.contains(&(message_kind(message), why)) {
                        refused.push((message_kind(message), why));
                    }
                }
            }
        }
        self.acting.selecting = None;
        self.acting.forward_names.clear();
        self.close_overlay(window, cx);
        if made {
            self.reload_chats(cx);
        }
        if !refused.is_empty() {
            self.show_problem(not_forwarded(&refused), cx);
        }
    }

    /// The keys of the forward sheet, as the palette's: the arrows and the
    /// page keys walk the rows, Enter picks one, Cmd or Ctrl with Enter
    /// sends.
    pub(super) fn forward_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count = self.forward_rows(cx).len();
        let stroke = &event.keystroke;
        let at = self.acting.forward_cursor.min(count.saturating_sub(1));
        match stroke.key.as_str() {
            "down" if count > 0 => self.acting.forward_cursor = (at + 1) % count,
            "up" if count > 0 => self.acting.forward_cursor = (at + count - 1) % count,
            "pagedown" if count > 0 => self.acting.forward_cursor = (at + 6).min(count - 1),
            "pageup" if count > 0 => self.acting.forward_cursor = at.saturating_sub(6),
            "enter" if stroke.modifiers.secondary() => {
                self.send_forward(window, cx);
                return true;
            }
            "enter" => {
                self.toggle_forward_row(at, cx);
                return true;
            }
            _ => return false,
        }
        self.follow_forward_cursor();
        cx.notify();
        true
    }

    /// The row the keyboard went to is in view; a heading over the first
    /// row comes with it.
    fn follow_forward_cursor(&self) {
        let cursor = self.acting.forward_cursor;
        let shown = if cursor == 0 {
            0
        } else {
            self.acting
                .forward_rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.target().is_some())
                .nth(cursor)
                .map(|(place, _)| place)
                .unwrap_or(0)
        };
        self.acting.forward_scroll.scroll_to_item(shown);
    }

    /// One row: its box, its picture, its name, its last message (or the
    /// number of somebody with no chat), when that was, and what is
    /// unread.
    #[allow(clippy::too_many_arguments)]
    fn forward_row(
        &self,
        index: usize,
        row: &ForwardRow,
        on: bool,
        picked: bool,
        full: bool,
        now: Timestamp,
        colours: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (title, face, second, time, unread) = match row {
            ForwardRow::Section(_) => return div().into_any_element(),
            ForwardRow::Chat(chat) => {
                let kind = match chat.kind {
                    ChatKind::Group => AvatarKind::Group,
                    ChatKind::Direct => AvatarKind::Person,
                };
                (
                    chat.title.clone(),
                    avatar_or(
                        self.media.avatar(&chat.account_id, &chat.id),
                        &chat.title,
                        kind,
                        metrics::AVATAR_LARGE(),
                        colours,
                    ),
                    self.chat_preview(chat, colours),
                    chat.last_message
                        .as_ref()
                        .map(|message| list_time(message.timestamp, now))
                        .unwrap_or_default(),
                    (chat.unread_count > 0).then_some((chat.unread_count, chat.muted)),
                )
            }
            ForwardRow::Person(contact) => {
                let name = contact.display_name();
                let picture = self.account.as_ref().and_then(|account| {
                    self.media
                        .avatar(account, &ChatId::new(contact.id.as_str()))
                });
                let number: SharedString = contact
                    .phone
                    .as_deref()
                    .map(crate::format::phone)
                    .unwrap_or_default()
                    .into();
                (
                    name.clone(),
                    avatar_or(
                        picture,
                        &name,
                        AvatarKind::Person,
                        metrics::AVATAR_LARGE(),
                        colours,
                    ),
                    div().truncate().child(number).into_any_element(),
                    String::new(),
                    None,
                )
            }
        };
        let hover = colours.elevated_selected;
        let name = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_2()
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(metrics::TEXT_BODY())
                    .text_color(colours.text)
                    .font_weight(if unread.is_some() {
                        FontWeight::SEMIBOLD
                    } else {
                        FontWeight::MEDIUM
                    })
                    .child(SharedString::from(title)),
            )
            .child(mono(time).flex_none().text_color(if unread.is_some() {
                colours.accent
            } else {
                colours.text_muted
            }));
        div()
            .id(("forward-chat", index))
            .debug_selector(move || format!("forward-chat-{index}"))
            .relative()
            .flex_none()
            .h(px(52.))
            .pl(px(12.))
            .pr_2()
            .rounded(px(6.))
            .flex()
            .items_center()
            .gap(px(12.))
            .cursor_pointer()
            .when(full && !picked, |this| this.opacity(0.5))
            // The row the keyboard is on: filled, with a bar of the
            // accent on its left, as in the palette.
            .when(on, |this| {
                this.bg(colours.elevated_selected).child(
                    div()
                        .debug_selector(|| "forward-cursor".into())
                        .absolute()
                        .left_0()
                        .top(px(10.))
                        .bottom(px(10.))
                        .w(px(3.))
                        .rounded_full()
                        .bg(colours.focus_ring),
                )
            })
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_forward_row(index, cx);
            }))
            .child(
                div()
                    .flex_none()
                    .debug_selector(move || {
                        if picked {
                            format!("forward-checked-{index}")
                        } else {
                            format!("forward-unchecked-{index}")
                        }
                    })
                    .child(icon(
                        if picked {
                            IconName::SquareCheck
                        } else {
                            IconName::Square
                        },
                        px(17.),
                        if picked { colours.accent } else { colours.icon },
                    )),
            )
            .child(div().flex_none().child(face))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(name)
                    .child(
                        div()
                            .min_w_0()
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(colours.text_muted)
                            .child(second),
                    ),
            )
            .children(unread.map(|(count, muted)| {
                div()
                    .flex_none()
                    .child(super::chat_list::unread_badge(count, muted, colours))
            }))
            .into_any_element()
    }

    pub(super) fn render_forward_sheet(
        &self,
        colours: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let now = Timestamp::now();
        let picked = &self.acting.forward_to;
        let full = picked.len() >= FORWARD_LIMIT;
        let count = self.forward_rows(cx).len();
        let cursor = self.acting.forward_cursor.min(count.saturating_sub(1));
        let mut list = div()
            .id("forward-list")
            .track_scroll(&self.acting.forward_scroll)
            .max_h(
                (self.viewport.height
                    - super::palette::palette_top(self.viewport.height)
                    - px(190.))
                .min(px(440.))
                .max(px(120.)),
            )
            .overflow_y_scroll()
            .p_1()
            .flex()
            .flex_col();
        let mut index = 0;
        for row in &self.acting.forward_rows {
            match row {
                ForwardRow::Section(title) => {
                    list = list.child(
                        div().px_2().pt(px(8.)).pb(px(3.)).child(
                            mono(format!("[ {} ]", title.to_uppercase()))
                                .text_color(colours.text_muted),
                        ),
                    );
                }
                _ => {
                    let on_it = row
                        .target()
                        .is_some_and(|(chat, _, _)| picked.contains(&chat));
                    list = list.child(self.forward_row(
                        index,
                        row,
                        index == cursor,
                        on_it,
                        full,
                        now,
                        colours,
                        cx,
                    ));
                    index += 1;
                }
            }
        }
        // What was picked, as chips: a pick the search no longer lists is
        // still here, and a click lets it go.
        let chips = (!picked.is_empty()).then(|| {
            let mut chips = div()
                .debug_selector(|| "forward-picked".into())
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(colours.border)
                .flex()
                .flex_wrap()
                .gap(px(6.));
            for (place, chat) in picked.iter().enumerate() {
                let name = self
                    .acting
                    .forward_names
                    .get(chat)
                    .cloned()
                    .unwrap_or_else(|| chat.as_str().to_owned());
                let chat = chat.clone();
                chips = chips.child(
                    div()
                        .id(("forward-picked", place))
                        .debug_selector(move || format!("forward-picked-{place}"))
                        .h(px(24.))
                        .pl(px(8.))
                        .pr(px(4.))
                        .rounded_full()
                        .bg(colours.elevated_selected)
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .cursor_pointer()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(colours.text)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_forward_to(chat.clone(), cx);
                        }))
                        .child(
                            div()
                                .max_w(px(160.))
                                .truncate()
                                .child(SharedString::from(name)),
                        )
                        .child(icon(IconName::X, px(12.), colours.text_muted)),
                );
            }
            chips
        });
        let title = crate::i18n::plural(
            "chat.message_actions.forward_title",
            self.acting.subjects.len() as u64,
            &[],
        );
        let skipped = (!self.acting.forward_skipped.is_empty())
            .then(|| not_forwarded(&self.acting.forward_skipped));
        let hints = crate::i18n::tr(
            "chat.message_actions.forward_hints",
            &[(
                "send",
                &keys::Chord::label(&keys::RUN_AND_STAY).to_uppercase(),
            )],
        );
        self.card("forward-sheet", colours)
            .w(metrics::PALETTE_WIDTH())
            .max_w(self.viewport.width - px(32.))
            .overflow_hidden()
            .flex()
            .flex_col()
            // A rule of the accent along its top, as on the palette.
            .child(div().flex_none().h(px(2.5)).bg(colours.accent))
            .child(
                div()
                    .h(px(50.))
                    .px_3()
                    .border_b_1()
                    .border_color(colours.border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(IconName::Search, px(16.), colours.text_muted))
                    // What is being done, before the search.
                    .child(
                        div()
                            .debug_selector(|| "forward-title".into())
                            .flex_none()
                            .max_w(px(260.))
                            .truncate()
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(4.))
                            .bg(colours.elevated_selected)
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(colours.text)
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(icon(IconName::Forward, px(12.), colours.text))
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(super::widgets::field(&self.acting.forward_input)),
                    )
                    .child(
                        div()
                            .id("forward-close")
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_overlay(window, cx);
                            }))
                            .child(mono(keys::key_label("escape")).text_color(colours.text_muted)),
                    ),
            )
            .when_some(skipped, |this, skipped| {
                this.child(
                    div()
                        .debug_selector(|| "forward-skipped".into())
                        .px_3()
                        .pt_2()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(colours.text_muted)
                        .child(SharedString::from(skipped)),
                )
            })
            .children(chips)
            .child(list)
            .when(count == 0, |this| {
                this.child(
                    div()
                        .debug_selector(|| "forward-empty".into())
                        .px_4()
                        .py_3()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(colours.text_muted)
                        .child(crate::i18n::t("commands.palette.nothing_found")),
                )
            })
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(colours.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(
                                mono(crate::i18n::tr(
                                    "chat.message_actions.forward_count",
                                    &[("n", &picked.len()), ("limit", &FORWARD_LIMIT)],
                                ))
                                .debug_selector(|| "forward-count".into())
                                .text_color(
                                    if picked.is_empty() {
                                        colours.text_muted
                                    } else {
                                        colours.accent
                                    },
                                ),
                            )
                            .child(
                                mono(hints)
                                    .debug_selector(|| "forward-hints".into())
                                    .truncate()
                                    .text_color(colours.text_muted),
                            ),
                    )
                    .child(
                        text_button(
                            "forward-send",
                            crate::i18n::t("chat.message_actions.forward"),
                            Some(IconName::Forward),
                            true,
                            colours,
                        )
                        .flex_none()
                        .when(picked.is_empty(), |this| this.opacity(0.5))
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.send_forward(window, cx);
                        })),
                    ),
            )
    }
}
