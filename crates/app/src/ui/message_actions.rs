//! What is done to messages from the conversation: replying, editing,
//! reacting, starring, deleting, forwarding, and selecting several of
//! them at once.
//!
//! Every action is local first: the engine shows it in the store at once
//! and tells the provider in the background (see `client-core`'s
//! `sync/actions.rs`), so nothing here waits on the network. What the
//! provider refuses comes back as a line in the window's problem bar.

use super::bubble::Row;
use super::menus::{fact, panel_header};
use super::shell::{Overlay, Shell};
use super::thread_keys::copy_text;
use super::widgets::{mono, text_button};
use crate::format::{clock, date};
use crate::icons::{icon, IconName};
use crate::keys::Command;
use crate::motion;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::message_preview;
use client_provider::{
    ChatId, ChatKind, DeliveryStatus, Direction, Message, MessageContent, MessageId,
};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, ClipboardItem, Context, Div, Entity, FontWeight, KeyDownEvent, SharedString, Stateful,
    Task, Window,
};
use std::collections::BTreeSet;

/// The reactions offered at once.
pub(super) const QUICK_REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🙏"];
/// How many chats a message is forwarded to at once: WhatsApp's own limit.
pub(super) const FORWARD_LIMIT: usize = 5;

/// What the composer is writing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum Compose {
    /// A new message.
    #[default]
    New,
    /// An answer to this message.
    Reply(Box<Message>),
    /// A new text for this message, which the account sent.
    Edit(Box<Message>),
}

/// The state of the actions on messages.
pub(super) struct Acting {
    /// What the composer is writing.
    pub(super) compose: Compose,
    /// The row that is lit for a moment: where a jump landed.
    pub(super) flash: Option<String>,
    _flash: Option<Task<()>>,
    /// The messages a sheet is about (delete, forward, react, info).
    pub(super) subjects: Vec<Message>,
    /// The rows picked while selecting messages; `None` when not.
    pub(super) selecting: Option<BTreeSet<String>>,
    /// The emoji the reaction sheet is on.
    pub(super) reaction_cursor: usize,
    /// Who reacted to a message, while the panel that lists them is open.
    pub(super) reactors: Option<super::reactors::Asked>,
    /// The search of the forward sheet.
    pub(super) forward_input: Entity<InputState>,
    /// The chats picked to forward to.
    pub(super) forward_to: Vec<ChatId>,
    /// What the forward sheet leaves out of a selection, and why: the
    /// kind of message and the reason, for each.
    pub(super) forward_skipped: Vec<(&'static str, &'static str)>,
    /// The chats the forward sheet lists, as last read.
    pub(super) forward_rows: Vec<(ChatId, String, ChatKind)>,
    /// The row of the forward sheet the keyboard is on.
    pub(super) forward_cursor: usize,
    /// The choice the delete sheet is on.
    pub(super) delete_cursor: usize,
}

impl Acting {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        Self {
            compose: Compose::New,
            flash: None,
            _flash: None,
            subjects: Vec::new(),
            selecting: None,
            reaction_cursor: 0,
            reactors: None,
            forward_input: cx.new(|cx| InputState::new(window, cx).placeholder("Search a chat")),
            forward_to: Vec::new(),
            forward_skipped: Vec::new(),
            forward_rows: Vec::new(),
            forward_cursor: 0,
            delete_cursor: 0,
        }
    }

    /// The chat was left: nothing is being written or selected.
    pub(super) fn leave_chat(&mut self) {
        self.compose = Compose::New;
        self.flash = None;
        self._flash = None;
        self.subjects.clear();
        self.selecting = None;
        self.reactors = None;
        self.forward_to.clear();
    }
}

/// The ways a message can be deleted, as the delete sheet offers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DeleteChoice {
    ForEveryone,
    ForMe,
    Cancel,
}

impl DeleteChoice {
    fn label(self) -> &'static str {
        match self {
            DeleteChoice::ForEveryone => "Delete for everyone",
            DeleteChoice::ForMe => "Delete for me",
            DeleteChoice::Cancel => "Cancel",
        }
    }

    fn selector(self) -> &'static str {
        match self {
            DeleteChoice::ForEveryone => "delete-for-everyone",
            DeleteChoice::ForMe => "delete-for-me",
            DeleteChoice::Cancel => "delete-cancel",
        }
    }
}

impl Shell {
    // ----- which messages -----------------------------------------------------------

    /// The messages an action is about: the ones picked while selecting,
    /// in the order of the conversation, else the one in focus.
    pub(super) fn subjects(&self) -> Vec<Message> {
        match (&self.acting.selecting, &self.open) {
            (Some(picked), Some(open)) => open
                .rows
                .iter()
                .filter_map(|row| match row {
                    Row::Message(row) if picked.contains(&row.key) => {
                        Some(row.stored.message.clone())
                    }
                    _ => None,
                })
                .collect(),
            _ => self.focused_message().into_iter().collect(),
        }
    }

    /// Lights a row for a moment: where a jump landed.
    pub(super) fn flash_row(&mut self, key: String, cx: &mut Context<Self>) {
        self.acting.flash = Some(key);
        self.acting._flash = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(motion::FLASH).await;
            this.update(cx, |this, cx| {
                this.acting.flash = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Goes to a message of the open chat and lights it.
    pub(super) fn jump_and_flash(&mut self, message: MessageId, cx: &mut Context<Self>) {
        self.jump_to_message(message.clone(), cx);
        if let Some(key) = self.key_of(&message) {
            self.flash_row(key, cx);
        }
    }

    // ----- reply and edit -----------------------------------------------------------

    /// Starts an answer to the message in focus: the composer shows what
    /// is being answered and takes the keyboard.
    pub(super) fn begin_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.focused_message() else {
            return false;
        };
        self.acting.compose = Compose::Reply(Box::new(message));
        self.leave_messages(window, cx);
        true
    }

    /// Starts editing the message in focus: its text goes into the
    /// composer, which says what it is doing.
    pub(super) fn begin_message_edit(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(message) = self.focused_message() else {
            return false;
        };
        let MessageContent::Text { body } = &message.content else {
            return false;
        };
        let body = body.clone();
        self.acting.compose = Compose::Edit(Box::new(message));
        self.mentioning.clear();
        self.composer
            .update(cx, |composer, cx| composer.set_value(body, window, cx));
        self.leave_messages(window, cx);
        true
    }

    /// Gives up the reply or the edit under way. An edit's text goes with
    /// it; a reply's stays, as a new message.
    pub(super) fn cancel_compose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.acting.compose, Compose::Edit(_)) {
            self.composer
                .update(cx, |composer, cx| composer.set_value("", window, cx));
        }
        self.acting.compose = Compose::New;
        cx.notify();
    }

    /// Enter in the composer while an edit is under way: the new text is
    /// shown at once and offered to the provider. An emptied text is not
    /// an edit.
    pub(super) fn save_message_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Compose::Edit(message) = &self.acting.compose else {
            return;
        };
        let text = self.composer.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        if let Err(error) =
            self.engine
                .edit_message(&message.account_id, &message.chat_id, &message.id, text)
        {
            tracing::error!(%error, "could not store the edit");
        }
        self.acting.compose = Compose::New;
        self.composer
            .update(cx, |composer, cx| composer.set_value("", window, cx));
        cx.notify();
    }

    /// The strip above the composer's field while a reply or an edit is
    /// under way: what it is about, and the way out.
    pub(super) fn render_compose_bar(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let (title, message, glyph, selector) = match &self.acting.compose {
            Compose::New => return None,
            Compose::Reply(message) => {
                let who = match (&message.direction, &message.sender_name) {
                    (Direction::Outgoing, _) => "Replying to yourself".to_owned(),
                    (_, Some(name)) => format!("Replying to {name}"),
                    _ => "Replying".to_owned(),
                };
                (who, message, IconName::MessageSquareText, "compose-reply")
            }
            Compose::Edit(message) => (
                "Editing message".to_owned(),
                message,
                IconName::Pencil,
                "compose-edit",
            ),
        };
        Some(
            div()
                .debug_selector(move || selector.into())
                .mb(px(6.))
                .pl(px(10.))
                .pr(px(4.))
                .py(px(5.))
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(palette.border)
                .bg(palette.surface)
                .flex()
                .items_center()
                .gap(px(10.))
                .child(icon(glyph, px(15.), palette.accent))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(metrics::TEXT_SMALL())
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(palette.accent)
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .debug_selector(|| "compose-about".into())
                                .truncate()
                                .text_size(metrics::TEXT_SMALL())
                                .text_color(palette.text_muted)
                                .child(SharedString::from(message_preview(message))),
                        ),
                )
                .child(
                    mono(crate::keys::key_label("escape"))
                        .text_color(palette.text_faint)
                        .text_size(px(10.)),
                )
                .child(
                    super::widgets::icon_button("compose-cancel", IconName::X, palette).on_click(
                        cx.listener(|this, _, window, cx| this.cancel_compose(window, cx)),
                    ),
                ),
        )
    }

    // ----- star, copy -------------------------------------------------------------

    /// Stars the messages an action is about, or unstars them when they
    /// all are.
    pub(super) fn star_subjects(&mut self, cx: &mut Context<Self>) -> bool {
        let subjects: Vec<Message> = self
            .subjects()
            .into_iter()
            .filter(|message| {
                self.command_state(Command::Star, message)
                    == super::message_menu::CommandState::Enabled
            })
            .collect();
        if subjects.is_empty() {
            return false;
        }
        let star = !subjects.iter().all(|message| message.extras.starred);
        for message in &subjects {
            if let Err(error) =
                self.engine
                    .star_message(&message.account_id, &message.chat_id, &message.id, star)
            {
                tracing::error!(%error, "could not store the star");
            }
        }
        cx.notify();
        true
    }

    /// Copies the text of every picked message, one per line, each with
    /// its time and who wrote it, as chat clients do.
    pub(super) fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let lines: Vec<String> = self
            .subjects()
            .iter()
            .filter_map(|message| {
                let text = copy_text(message)?;
                let who = match (&message.direction, &message.sender_name) {
                    (Direction::Outgoing, _) => "You".to_owned(),
                    (_, Some(name)) => name.clone(),
                    _ => "They".to_owned(),
                };
                Some(format!(
                    "[{} {}] {who}: {text}",
                    date(message.timestamp),
                    clock(message.timestamp)
                ))
            })
            .collect();
        if lines.is_empty() {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(lines.join("\n")));
        true
    }

    // ----- selecting messages -------------------------------------------------------

    /// Starts selecting, with the message in focus picked; or, already
    /// selecting, picks or lets go of the one in focus.
    pub(super) fn toggle_selected(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(key) = self.keys.focused.clone() else {
            return false;
        };
        let picked = self.acting.selecting.get_or_insert_with(BTreeSet::new);
        if !picked.remove(&key) {
            picked.insert(key);
        }
        cx.notify();
        true
    }

    /// A click on a row while selecting: picked, or let go.
    pub(super) fn toggle_row(&mut self, key: String, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_message(key, window, cx);
        self.toggle_selected(cx);
    }

    /// Shift with an arrow: the message in focus and the next one in that
    /// direction are both picked, and the focus moves there.
    pub(super) fn extend_selection(
        &mut self,
        up: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(from) = self.keys.focused.clone() else {
            return false;
        };
        self.acting
            .selecting
            .get_or_insert_with(BTreeSet::new)
            .insert(from);
        let command = if up {
            Command::MessageUp
        } else {
            Command::MessageDown
        };
        self.run_command(command, window, cx);
        if let (Some(picked), Some(now)) = (&mut self.acting.selecting, self.keys.focused.clone()) {
            picked.insert(now);
        }
        cx.notify();
        true
    }

    /// Leaves the selection: nothing is picked.
    pub(super) fn end_selecting(&mut self, cx: &mut Context<Self>) {
        self.acting.selecting = None;
        cx.notify();
    }

    /// The bar that takes the composer's place while messages are being
    /// selected: how many, and what can be done with them.
    pub(super) fn render_selection_bar(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let picked = self.acting.selecting.as_ref()?;
        let count = picked.len();
        let caps = self.engine.capabilities();
        let action = |id: &'static str, label: &'static str, glyph, command: Command| {
            text_button(id, label, Some(glyph), false, palette).on_click(cx.listener(
                move |this, _, window, cx| {
                    this.run_command(command, window, cx);
                },
            ))
        };
        Some(
            div()
                .debug_selector(|| "selection-bar".into())
                .flex_none()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(palette.border)
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .debug_selector(|| "selection-count".into())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(metrics::TEXT_BODY())
                        .font_weight(FontWeight::MEDIUM)
                        .child(SharedString::from(match count {
                            1 => "1 message selected".to_owned(),
                            n => format!("{n} messages selected"),
                        })),
                )
                .child(action(
                    "selection-copy",
                    "Copy",
                    IconName::Copy,
                    Command::Copy,
                ))
                .when(caps.forwards, |this| {
                    this.child(action(
                        "selection-forward",
                        "Forward",
                        IconName::Forward,
                        Command::Forward,
                    ))
                })
                .when(caps.stars, |this| {
                    this.child(action(
                        "selection-star",
                        "Star",
                        IconName::Star,
                        Command::Star,
                    ))
                })
                .child(action(
                    "selection-save",
                    "Save",
                    IconName::ArrowDownToLine,
                    Command::SaveAs,
                ))
                .child(action(
                    "selection-delete",
                    "Delete",
                    IconName::Trash,
                    Command::Delete,
                ))
                .child(
                    text_button("selection-cancel", "Cancel", None, false, palette)
                        .on_click(cx.listener(|this, _, _, cx| this.end_selecting(cx))),
                ),
        )
    }

    // ----- delete -------------------------------------------------------------------

    /// What the delete sheet offers for the messages it is about: for
    /// everyone only when every one of them is the account's own and the
    /// provider can.
    pub(super) fn delete_choices(&self) -> Vec<DeleteChoice> {
        let caps = self.engine.capabilities();
        let subjects = &self.acting.subjects;
        let all_own = subjects
            .iter()
            .all(|message| message.direction == Direction::Outgoing);
        let unsent = |message: &Message| {
            matches!(
                message.status,
                DeliveryStatus::Pending | DeliveryStatus::Failed { .. }
            )
        };
        let mut choices = Vec::new();
        // What never left is simply taken back: one way to delete it.
        let all_unsent = subjects.iter().all(unsent);
        if all_own && caps.deletes && !all_unsent && !subjects.iter().any(|m| m.deleted) {
            choices.push(DeleteChoice::ForEveryone);
        }
        let for_me = subjects.iter().all(|message| {
            if message.direction == Direction::Outgoing {
                caps.delete_for_me || unsent(message)
            } else {
                caps.delete_received
            }
        });
        if for_me {
            choices.push(DeleteChoice::ForMe);
        }
        choices.push(DeleteChoice::Cancel);
        choices
    }

    /// Asks how the messages an action is about are to be deleted.
    pub(super) fn begin_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let subjects: Vec<Message> = self
            .subjects()
            .into_iter()
            .filter(|message| {
                self.command_state(Command::Delete, message)
                    == super::message_menu::CommandState::Enabled
            })
            .collect();
        if subjects.is_empty() {
            return false;
        }
        self.acting.subjects = subjects;
        // The keyboard starts on the way out: deleting takes a choice.
        self.acting.delete_cursor = self.delete_choices().len() - 1;
        self.open_overlay(Overlay::DeleteMessage, window, cx);
        true
    }

    /// The choice was made.
    pub(super) fn delete_chosen(
        &mut self,
        choice: DeleteChoice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if choice != DeleteChoice::Cancel {
            for message in std::mem::take(&mut self.acting.subjects) {
                if let Err(error) = self.engine.delete_message(
                    &message.account_id,
                    &message.chat_id,
                    &message.id,
                    choice == DeleteChoice::ForEveryone,
                ) {
                    tracing::error!(%error, "could not delete the message");
                }
            }
            self.acting.selecting = None;
        }
        self.acting.subjects.clear();
        self.close_overlay(window, cx);
        // The message that had the keyboard may be gone: the next frame
        // finds none in focus and the composer has it again.
        cx.notify();
    }

    pub(super) fn render_delete_sheet(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let count = self.acting.subjects.len();
        let choices = self.delete_choices();
        let mut buttons = div().flex().flex_col().gap_2();
        for (index, choice) in choices.iter().copied().enumerate() {
            let on = index == self.acting.delete_cursor;
            buttons = buttons.child(
                text_button(
                    choice.selector(),
                    choice.label(),
                    (choice != DeleteChoice::Cancel).then_some(IconName::Trash),
                    false,
                    palette,
                )
                .when(on, |this| this.border_color(palette.focus_ring))
                .when(choice != DeleteChoice::Cancel, |this| {
                    this.text_color(palette.danger)
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.delete_chosen(choice, window, cx);
                })),
            );
        }
        self.card("delete-message", palette)
            .w(px(360.))
            .flex()
            .flex_col()
            .child(panel_header(
                if count == 1 {
                    "Delete message?"
                } else {
                    "Delete messages?"
                },
                palette,
                cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
            ))
            .child(
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(palette.text_muted)
                            .child(SharedString::from(match count {
                                1 => "For everyone takes it from the chat for all its \
                                      members; for me, from your devices only."
                                    .to_owned(),
                                n => format!("{n} messages."),
                            })),
                    )
                    .child(buttons),
            )
    }

    // ----- react --------------------------------------------------------------------

    /// The account's own reaction to a message, as the store has it.
    fn own_reaction(&self, message: &MessageId) -> Option<String> {
        self.open.as_ref()?.rows.iter().find_map(|row| match row {
            Row::Message(row) if &row.stored.message.id == message => row
                .stored
                .reactions
                .iter()
                .find(|reaction| reaction.from_me)
                .map(|reaction| reaction.emoji.clone()),
            _ => None,
        })
    }

    /// Opens the reaction sheet for the message in focus.
    pub(super) fn begin_react(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.focused_message() else {
            return false;
        };
        self.acting.subjects = vec![message];
        self.acting.reaction_cursor = 0;
        self.open_overlay(Overlay::React, window, cx);
        // The sheet's search field takes the keyboard: typing looks
        // through every emoji.
        self.emoji_begin_reaction(window, cx);
        true
    }

    /// Reacts to the sheet's message with `emoji`; the reaction the
    /// account already has there is taken back by picking it again.
    pub(super) fn react_with(&mut self, emoji: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self.acting.subjects.first().cloned() else {
            return;
        };
        let emoji = emoji.trim();
        let mine = self.own_reaction(&message.id);
        let send = if mine.as_deref() == Some(emoji) {
            ""
        } else {
            emoji
        };
        if mine.is_none() && send.is_empty() {
            return self.close_overlay(window, cx);
        }
        if let Err(error) =
            self.engine
                .react(&message.account_id, &message.chat_id, &message.id, send)
        {
            tracing::error!(%error, "could not queue the reaction");
        }
        self.acting.subjects.clear();
        self.close_overlay(window, cx);
    }

    /// The account's own reaction is taken back (from the panel of who
    /// reacted), or somebody else's is joined.
    pub(super) fn toggle_reaction(
        &mut self,
        message: Message,
        emoji: String,
        mine: bool,
        cx: &mut Context<Self>,
    ) {
        if !self.engine.capabilities().reactions {
            return;
        }
        let send = if mine { "" } else { emoji.as_str() };
        if let Err(error) =
            self.engine
                .react(&message.account_id, &message.chat_id, &message.id, send)
        {
            tracing::error!(%error, "could not queue the reaction");
        }
        cx.notify();
    }

    /// The keys of the reaction sheet while the keyboard is on the six:
    /// the arrows move, Enter picks, 1 to 6 pick at once. (With something
    /// typed in the search, the keys are the picker's.)
    pub(super) fn react_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count = QUICK_REACTIONS.len();
        match event.keystroke.key.as_str() {
            "left" => {
                self.acting.reaction_cursor = (self.acting.reaction_cursor + count - 1) % count
            }
            "right" => self.acting.reaction_cursor = (self.acting.reaction_cursor + 1) % count,
            "enter" => {
                let emoji = QUICK_REACTIONS[self.acting.reaction_cursor.min(count - 1)];
                self.react_with(emoji, window, cx);
            }
            digit @ ("1" | "2" | "3" | "4" | "5" | "6") => {
                let at: usize = digit.parse().unwrap_or(1);
                self.react_with(QUICK_REACTIONS[at - 1], window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn render_react_sheet(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let mine = self
            .acting
            .subjects
            .first()
            .and_then(|message| self.own_reaction(&message.id));
        let mut row = div().flex().items_center().gap(px(4.));
        for (index, emoji) in QUICK_REACTIONS.iter().copied().enumerate() {
            // The keyboard is on the six until the rest is asked for.
            let on = index == self.acting.reaction_cursor && !self.emoji.expanded;
            let is_mine = mine.as_deref() == Some(emoji);
            let hover = palette.hover;
            row = row.child(
                div()
                    .id(("reaction", index))
                    .debug_selector(move || format!("reaction-{index}"))
                    .size(px(40.))
                    .rounded_full()
                    .border_2()
                    .border_color(if on {
                        palette.focus_ring
                    } else {
                        gpui_kit::transparent_black()
                    })
                    // The account's own reaction stands on the quiet fill.
                    .when(is_mine, |this| this.bg(palette.muted))
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(px(22.))
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.react_with(emoji, window, cx);
                    }))
                    .child(emoji),
            );
        }
        self.card("react-sheet", palette)
            .absolute()
            .p(px(8.))
            .flex()
            .flex_col()
            .gap(px(8.))
            // The six, the way to all the others, and the search that
            // finds them (`emoji_picker.rs`).
            .child(row.child(self.render_reaction_more(palette, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(self.render_emoji_search("reaction-input", palette))
                    .when(mine.is_some(), |this| {
                        this.child(
                            text_button("reaction-remove", "Remove", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.react_with("", window, cx);
                                })),
                        )
                    }),
            )
            .children(self.render_reaction_body(palette, cx))
    }

    // ----- info ---------------------------------------------------------------------

    /// What is known about the message in focus: when it was sent, how
    /// far it got. WhatsApp's per-person "read by" is not something a
    /// provider reports, so it is not shown.
    pub(super) fn begin_info(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.focused_message() else {
            return false;
        };
        self.acting.subjects = vec![message];
        self.open_overlay(Overlay::MessageInfo, window, cx);
        true
    }

    pub(super) fn render_info_sheet(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let message = self.acting.subjects.first()?;
        let own = message.direction == Direction::Outgoing;
        let status: SharedString = match &message.status {
            DeliveryStatus::Pending => "Waiting to be sent".into(),
            DeliveryStatus::Accepted => "With the provider, on its way to WhatsApp".into(),
            DeliveryStatus::Sent => "Sent".into(),
            DeliveryStatus::Delivered => "Delivered".into(),
            DeliveryStatus::Read => "Read".into(),
            DeliveryStatus::Failed { reason } if reason.is_empty() => "Not sent".into(),
            DeliveryStatus::Failed { reason } => format!("Not sent: {reason}").into(),
        };
        let from: SharedString = match (&message.direction, &message.sender_name) {
            (Direction::Outgoing, _) => "You".into(),
            (_, Some(name)) => name.clone().into(),
            _ => "The other side".into(),
        };
        let mut marks = Vec::new();
        if message.edited {
            marks.push("edited");
        }
        if message.extras.starred {
            marks.push("starred");
        }
        if message.extras.forwarded_many {
            marks.push("forwarded many times");
        } else if message.extras.forwarded {
            marks.push("forwarded");
        }
        if message.deleted {
            marks.push("deleted");
        }
        Some(
            self.card("message-info", palette)
                .w(px(380.))
                .flex()
                .flex_col()
                .child(panel_header(
                    "Message info",
                    palette,
                    cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
                ))
                .child(
                    div()
                        .px_4()
                        .pb_3()
                        .flex()
                        .flex_col()
                        .child(fact("From", from, palette))
                        .child(fact(
                            if own { "Sent" } else { "Received" },
                            format!("{}, {}", date(message.timestamp), clock(message.timestamp))
                                .into(),
                            palette,
                        ))
                        .when(own, |this| this.child(fact("Status", status, palette)))
                        .when(!marks.is_empty(), |this| {
                            this.child(fact("Marks", marks.join(", ").into(), palette))
                        })
                        .child(fact("Kind", message_kind(message).into(), palette)),
                ),
        )
    }

    // ----- forward ------------------------------------------------------------------

    /// Reads the chats the forward sheet lists: the account's, narrowed
    /// by what is typed, in the chat list's order. Read when the sheet
    /// opens and when the text changes, not while it is drawn.
    pub(super) fn refresh_forward_rows(&mut self, cx: &mut Context<Self>) {
        let typed = self.acting.forward_input.read(cx).value().trim().to_owned();
        self.acting.forward_rows = match &self.account {
            Some(account) => self
                .engine
                .store()
                .chats(account, Some(typed.as_str()))
                .unwrap_or_default()
                .into_iter()
                .take(40)
                .map(|chat| (chat.id, chat.title, chat.kind))
                .collect(),
            None => Vec::new(),
        };
        self.acting.forward_cursor = 0;
        cx.notify();
    }

    /// The chats the forward sheet lists.
    pub(super) fn forward_rows(&self, _: &gpui_kit::App) -> Vec<(ChatId, String, ChatKind)> {
        self.acting.forward_rows.clone()
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
        self.acting.forward_cursor = 0;
        self.open_overlay(Overlay::Forward, window, cx);
        self.acting.forward_input.update(cx, |field, cx| {
            field.set_value("", window, cx);
            field.focus(window, cx);
        });
        self.refresh_forward_rows(cx);
        true
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
    /// once. What cannot be queued is said, with its reason, and does not
    /// stop the others.
    pub(super) fn send_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.acting.forward_to.is_empty() {
            return;
        }
        let Some(account) = self.account.clone() else {
            return;
        };
        let to = std::mem::take(&mut self.acting.forward_to);
        let mut subjects = std::mem::take(&mut self.acting.subjects);
        subjects.sort_by_key(|message| message.timestamp);
        let mut refused: Vec<(&'static str, &'static str)> =
            std::mem::take(&mut self.acting.forward_skipped);
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
                        client_core::ForwardError::Store(_) => "It could not be saved here",
                    };
                    if !refused.contains(&(message_kind(message), why)) {
                        refused.push((message_kind(message), why));
                    }
                }
            }
        }
        self.acting.selecting = None;
        self.close_overlay(window, cx);
        if !refused.is_empty() {
            self.show_problem(not_forwarded(&refused), cx);
        }
    }

    /// The keys of the forward sheet: the arrows walk the chats, Enter
    /// picks one, Cmd or Ctrl with Enter sends.
    pub(super) fn forward_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let rows = self.forward_rows(cx);
        let stroke = &event.keystroke;
        match stroke.key.as_str() {
            "down" if !rows.is_empty() => {
                self.acting.forward_cursor = (self.acting.forward_cursor + 1) % rows.len()
            }
            "up" if !rows.is_empty() => {
                self.acting.forward_cursor =
                    (self.acting.forward_cursor + rows.len() - 1) % rows.len()
            }
            "enter" if stroke.modifiers.secondary() => self.send_forward(window, cx),
            "enter" => {
                if let Some((chat, _, _)) = rows.get(self.acting.forward_cursor) {
                    self.toggle_forward_to(chat.clone(), cx);
                }
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    pub(super) fn render_forward_sheet(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let rows = self.forward_rows(cx);
        let picked = &self.acting.forward_to;
        let cursor = self.acting.forward_cursor.min(rows.len().saturating_sub(1));
        let full = picked.len() >= FORWARD_LIMIT;
        let mut list = div()
            .id("forward-list")
            .max_h(px(320.))
            .overflow_y_scroll()
            .flex()
            .flex_col();
        for (index, (chat, title, kind)) in rows.iter().enumerate() {
            let on = picked.contains(chat);
            let (hover, chosen) = (palette.hover, chat.clone());
            list = list.child(
                div()
                    .id(("forward-chat", index))
                    .debug_selector(move || format!("forward-chat-{index}"))
                    .h(px(38.))
                    .px_2()
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .when(index == cursor, |this| this.bg(palette.muted))
                    .when(full && !on, |this| this.opacity(0.5))
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_forward_to(chosen.clone(), cx);
                    }))
                    .child(icon(
                        if on {
                            IconName::SquareCheck
                        } else {
                            IconName::Square
                        },
                        px(16.),
                        if on { palette.accent } else { palette.icon },
                    ))
                    .child(icon(
                        match kind {
                            ChatKind::Group => IconName::Users,
                            ChatKind::Direct => IconName::MessageCircle,
                        },
                        px(14.),
                        palette.text_faint,
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_BODY())
                            .child(SharedString::from(title.clone())),
                    ),
            );
        }
        let count = self.acting.subjects.len();
        let title = if count == 1 {
            "Forward message".to_owned()
        } else {
            format!("Forward {count} messages")
        };
        let skipped = (!self.acting.forward_skipped.is_empty())
            .then(|| not_forwarded(&self.acting.forward_skipped));
        self.card("forward-sheet", palette)
            .w(px(420.))
            .flex()
            .flex_col()
            .child(panel_header(
                &title,
                palette,
                cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
            ))
            .when_some(skipped, |this, skipped| {
                this.child(
                    div()
                        .debug_selector(|| "forward-skipped".into())
                        .px_3()
                        .pt_2()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .child(SharedString::from(skipped)),
                )
            })
            .child(
                div().px_3().pt_3().pb_2().child(
                    div()
                        .h(px(34.))
                        .px_2()
                        .rounded(metrics::RADIUS())
                        .border_1()
                        .border_color(palette.border)
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(IconName::Search, px(14.), palette.text_muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&self.acting.forward_input).appearance(false)),
                        ),
                ),
            )
            .child(div().px_2().child(list))
            .child(
                div()
                    .p_3()
                    .border_t_1()
                    .border_color(palette.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        mono(format!("{} OF {FORWARD_LIMIT} CHATS", picked.len()))
                            .debug_selector(|| "forward-count".into())
                            .text_color(palette.text_muted),
                    )
                    .child(
                        text_button(
                            "forward-send",
                            "Forward",
                            Some(IconName::Forward),
                            true,
                            palette,
                        )
                        .when(picked.is_empty(), |this| this.opacity(0.5))
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.send_forward(window, cx);
                        })),
                    ),
            )
    }
}

/// What a forward leaves out, in a sentence: each kind of message with
/// its reason.
pub(super) fn not_forwarded(skipped: &[(&'static str, &'static str)]) -> String {
    let each: Vec<String> = skipped
        .iter()
        .map(|(kind, why)| format!("{kind} ({})", why.to_lowercase()))
        .collect();
    match skipped.len() {
        1 => format!("Not forwarded: {}", each[0]),
        n => format!("{n} messages not forwarded: {}", each.join(", ")),
    }
}

/// The text a forward carries: as it was written, mentions included as
/// they were typed.
fn copy_raw_text(message: &Message) -> Option<String> {
    match &message.content {
        MessageContent::Text { body } if !message.deleted && !body.trim().is_empty() => Some(
            client_core::with_mention_names(body, &message.extras.mentions),
        ),
        _ => None,
    }
}

/// What a message is, in a word.
fn message_kind(message: &Message) -> &'static str {
    use client_provider::MediaKind as M;
    match &message.content {
        _ if message.deleted => "Deleted message",
        MessageContent::Text { .. } => "Text",
        MessageContent::Media(media) => match media.kind {
            M::Image => "Photo",
            M::Video if media.gif => "GIF",
            M::Video => "Video",
            M::Voice => "Voice note",
            M::Audio => "Audio",
            M::Document => "Document",
            M::Sticker => "Sticker",
        },
        MessageContent::Location(_) => "Location",
        MessageContent::Contacts { .. } => "Contact card",
        MessageContent::Poll(_) => "Poll",
        MessageContent::Event(_) => "Event",
        MessageContent::System(_) => "Notice",
        MessageContent::Reaction { .. } => "Reaction",
        MessageContent::Unsupported { .. } => "Unsupported message",
    }
}
