//! Who reacted to a message: the words under the pointer on a reaction,
//! and the panel a click on a reaction opens.
//!
//! The store keeps one reaction per person and message, with who made it
//! (`client_core::ReactionSummary::by`). People are called what they are
//! called elsewhere, and the account itself is "You".

use super::bubble::Row;
use super::shell::{Overlay, Shell};
use super::widgets::{label, AvatarKind};
use crate::format;
use crate::theme::px;
use crate::theme::Palette;
use client_core::{NameSource, ReactionSummary, Reactor, StoredMessage};
use client_provider::{AccountId, ChatId, ChatKind, ContactId, Message, MessageId};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, Pixels, Point, SharedString, Stateful, Window};

/// Width of the panel.
#[allow(non_snake_case)]
fn WIDTH() -> Pixels {
    px(280.)
}
/// Height of a person's line (`Shell::person_row`).
#[allow(non_snake_case)]
fn ROW() -> Pixels {
    px(44.)
}
/// What the panel's list shows at once; past it, it scrolls.
#[allow(non_snake_case)]
fn LIST_MAX() -> Pixels {
    ROW() * 6.
}

/// One line of the panel: a person and the emoji they reacted with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Shown {
    /// What they are called: "You" for the account itself.
    pub(super) name: String,
    pub(super) emoji: String,
    /// The account's own reaction.
    pub(super) mine: bool,
    /// Who it is, for their picture and their profile.
    pub(super) sender: ContactId,
}

/// The panel that is open: the message it is about, and what it lists.
pub(super) struct Asked {
    message: MessageId,
    /// The emoji that was clicked: its people come first.
    first: Option<String>,
    /// Where the pointer was; `None` when the keyboard asked.
    at: Option<Point<Pixels>>,
    /// The lines, as last read: when the panel opened, and whenever the
    /// messages are read again. Never while it is drawn.
    lines: Vec<Shown>,
}

/// The people of one reaction, as a hint says them: up to three by name,
/// more as two names and how many others.
pub(super) fn names_line(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [one, two] => format!("{one} and {two}"),
        [one, two, three] => format!("{one}, {two} and {three}"),
        [one, two, rest @ ..] => format!("{one}, {two} and {} more", rest.len()),
    }
}

/// The people of a reaction, the account itself first: it is never one
/// of the "more".
fn own_first(by: &[Reactor]) -> impl Iterator<Item = &Reactor> {
    by.iter()
        .filter(|reactor| reactor.from_me)
        .chain(by.iter().filter(|reactor| !reactor.from_me))
}

impl Shell {
    /// What somebody who reacted is called.
    fn reactor_name(&self, account: &AccountId, chat: &ChatId, reactor: &Reactor) -> String {
        if reactor.from_me {
            return "You".to_owned();
        }
        let person = self
            .store
            .person(account, Some(chat), &reactor.sender)
            .unwrap_or_default();
        if person.me {
            return "You".to_owned();
        }
        // In a chat of two, whoever is not the account is the other one,
        // under whichever id the reaction came: the chat is named after
        // them, unless the store knows them by a better name.
        let other = self
            .open
            .as_ref()
            .map(|open| &open.chat)
            .filter(|open| open.id == *chat && open.kind == ChatKind::Direct)
            .map(|open| open.title.clone())
            .filter(|title| !title.trim().is_empty());
        match (person.name, other) {
            (Some((name, source)), _) if source < NameSource::Username => name,
            (_, Some(title)) => title,
            (Some((name, _)), None) => name,
            (None, None) => match person.phone {
                Some(phone) => format::phone(&phone),
                None => reactor.sender.to_string(),
            },
        }
    }

    /// What the pointer on a reaction says: who made it.
    pub(super) fn reactors_hint(&self, message: &Message, reaction: &ReactionSummary) -> String {
        let names: Vec<String> = own_first(&reaction.by)
            .map(|reactor| self.reactor_name(&message.account_id, &message.chat_id, reactor))
            .collect();
        names_line(&names)
    }

    /// A message of the open chat, with its reactions.
    fn stored(&self, message: &MessageId) -> Option<&StoredMessage> {
        self.open.as_ref()?.rows.iter().find_map(|row| match row {
            Row::Message(row) if &row.stored.message.id == message => Some(&row.stored),
            _ => None,
        })
    }

    /// Whether anybody reacted to a message of the open chat.
    pub(super) fn has_reactions(&self, message: &MessageId) -> bool {
        self.stored(message)
            .is_some_and(|stored| !stored.reactions.is_empty())
    }

    /// Opens the panel of who reacted to the message of this row: at the
    /// pointer, or beside the message when the keyboard asked. The people
    /// of `first`, the reaction that was clicked, are listed first.
    pub(super) fn open_reactors(
        &mut self,
        key: String,
        first: Option<String>,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.focus_message(key, window, cx);
        let Some(message) = self.focused_message() else {
            return false;
        };
        if !self.has_reactions(&message.id) {
            return false;
        }
        self.acting.reactors = Some(Asked {
            message: message.id,
            first,
            at,
            lines: Vec::new(),
        });
        self.open_overlay(Overlay::Reactors, window, cx);
        self.refresh_reactors();
        true
    }

    /// Reads who the panel lists: the people of the reaction that was
    /// clicked, then of the others, most used first; within one, the
    /// account itself and then the others as they reacted.
    pub(super) fn refresh_reactors(&mut self) {
        // Only while it is open: a panel that was closed lists nothing.
        if self.overlay != Overlay::Reactors {
            return;
        }
        let Some(asked) = &self.acting.reactors else {
            return;
        };
        let lines = match self.stored(&asked.message) {
            Some(stored) => {
                let message = &stored.message;
                let clicked = |reaction: &&ReactionSummary| {
                    asked.first.as_deref() == Some(reaction.emoji.as_str())
                };
                stored
                    .reactions
                    .iter()
                    .filter(clicked)
                    .chain(
                        stored
                            .reactions
                            .iter()
                            .filter(|reaction| !clicked(reaction)),
                    )
                    .flat_map(|reaction| {
                        own_first(&reaction.by).map(|reactor| Shown {
                            name: self.reactor_name(&message.account_id, &message.chat_id, reactor),
                            emoji: reaction.emoji.clone(),
                            mine: reactor.from_me,
                            sender: reactor.sender.clone(),
                        })
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        if let Some(asked) = &mut self.acting.reactors {
            asked.lines = lines;
        }
    }

    /// The lines of the panel, for the tests.
    #[cfg(test)]
    pub(super) fn reactors_shown(&self) -> Vec<Shown> {
        self.acting
            .reactors
            .as_ref()
            .map(|asked| asked.lines.clone())
            .unwrap_or_default()
    }

    /// Where the panel goes, kept inside the window: at the pointer, or
    /// beside the message in focus.
    pub(super) fn reactors_origin(&self) -> Point<Pixels> {
        let asked = self.acting.reactors.as_ref();
        let at = asked
            .and_then(|asked| asked.at)
            .unwrap_or_else(|| self.focused_anchor());
        let lines = asked.map_or(0, |asked| asked.lines.len()) as f32;
        let height = px(38.) + (ROW() * lines).min(LIST_MAX());
        gpui_kit::point(
            at.x.min(self.viewport.width - WIDTH() - px(8.)).max(px(8.)),
            at.y.min(self.viewport.height - height - px(8.)).max(px(8.)),
        )
    }

    /// The panel: a line for each person who reacted, with their picture,
    /// their name and their emoji. A person's line is the way to their
    /// profile; the account's own takes its reaction back.
    pub(super) fn render_reactors(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let asked = self.acting.reactors.as_ref()?;
        let message = self.stored(&asked.message)?.message.clone();
        let account = &message.account_id;
        let can_react = self.engine.capabilities().reactions;
        let heading = match asked.lines.len() {
            0 => "No reactions".to_owned(),
            1 => "1 reaction".to_owned(),
            count => format!("{count} reactions"),
        };
        let mut list = div()
            .id("reactors-list")
            .max_h(LIST_MAX())
            .overflow_y_scroll()
            .flex()
            .flex_col();
        for (index, line) in asked.lines.iter().enumerate() {
            let mine = line.mine;
            let removable = mine && can_react;
            let (target, emoji, contact) =
                (message.clone(), line.emoji.clone(), line.sender.clone());
            list = list.child(
                self.person_row(
                    ("reactor", index),
                    format!("reactor-{index}"),
                    account,
                    &ChatId::new(line.sender.as_str()),
                    &line.name,
                    removable.then(|| "Click to remove".to_owned()),
                    AvatarKind::Person,
                    palette,
                )
                .child(
                    div()
                        .debug_selector(move || format!("reactor-emoji-{index}"))
                        .flex_none()
                        .text_size(px(18.))
                        .line_height(px(24.))
                        .child(SharedString::from(line.emoji.clone())),
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    if removable {
                        this.close_overlay(window, cx);
                        this.toggle_reaction(target.clone(), emoji.clone(), true, cx);
                    } else if !mine {
                        this.open_contact_profile(
                            target.account_id.clone(),
                            contact.clone(),
                            window,
                            cx,
                        );
                    }
                })),
            );
        }
        Some(
            self.card("reactors", palette)
                .absolute()
                .w(WIDTH())
                .p_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .debug_selector(|| "reactors-count".into())
                        .h(px(28.))
                        .px_2()
                        .flex()
                        .items_center()
                        .child(label(&heading, palette)),
                )
                .child(list),
        )
    }
}
