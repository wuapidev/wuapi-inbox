//! The keyboard in a conversation: which message has it, moving from one
//! to the next, and running the commands of the registry (`crate::keys`).
//!
//! A message "in focus" is the one the single keys act on. While one is,
//! the keyboard belongs to the shell itself and not to a text field, so a
//! key can never be both a letter typed and a command. Escape gives the
//! keyboard back to the composer.

use super::bubble::{MessageRow, Row};
use super::shell::{ChatFilter, ListRow, Overlay, SettingsSection, Shell};
use crate::keys::{self, Command};
use client_core::with_mention_names;
use client_provider::{ChatChange, ChatId, MediaKind, Message, MessageContent, MessageId};
use gpui_kit::{ClipboardItem, Context, Focusable, Pixels, Point, Window};

/// What the keyboard is doing in the open conversation.
#[derive(Default)]
pub(super) struct ThreadKeys {
    /// The row key of the message in focus.
    pub(super) focused: Option<String>,
    /// All of its text shows as selected (Ctrl+A).
    pub(super) whole: bool,
}

impl ThreadKeys {
    /// The chat was left: nothing is in focus.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What "Copy" copies of a message: its text, or the caption of what it
/// carries, with mentions by name. `None` when it has no text of its own.
pub(super) fn copy_text(message: &Message) -> Option<String> {
    if message.deleted {
        return None;
    }
    let text = match &message.content {
        MessageContent::Text { body } => body.clone(),
        MessageContent::Media(media) => media.caption.clone().unwrap_or_default(),
        MessageContent::Location(place) => [place.name.clone(), place.address.clone()]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
        MessageContent::Contacts { cards } => cards
            .iter()
            .map(|card| {
                std::iter::once(card.name.clone())
                    .chain(card.phones.iter().cloned())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        MessageContent::Poll(poll) => poll.question.clone(),
        MessageContent::Event(event) => event.title.clone(),
        _ => String::new(),
    };
    let text = with_mention_names(&text, &message.extras.mentions);
    (!text.trim().is_empty()).then_some(text)
}

/// The link or the number a message carries: its preview's address, the
/// first address in its text, a contact card's number, a place on the map.
pub(super) fn copy_link(message: &Message) -> Option<String> {
    if message.deleted {
        return None;
    }
    if let Some(link) = &message.extras.link {
        return Some(link.url.clone());
    }
    let text = match &message.content {
        MessageContent::Text { body } => Some(body.as_str()),
        MessageContent::Media(media) => media.caption.as_deref(),
        MessageContent::Contacts { cards } => {
            return cards.iter().find_map(|card| card.phones.first().cloned())
        }
        MessageContent::Location(place) => return Some(super::tiles::map_url(place.point)),
        _ => None,
    }?;
    crate::markup::parse(text, &[])
        .iter()
        .find_map(|block| match block {
            crate::markup::Block::Paragraph(line)
            | crate::markup::Block::Quote(line)
            | crate::markup::Block::Bullet(line)
            | crate::markup::Block::Numbered(_, line) => {
                line.spans.iter().find_map(|span| span.style.link.clone())
            }
            crate::markup::Block::Code(_) => None,
        })
}

/// Whether a row is a message the keyboard stops on: not a day, not a
/// notice of the chat.
fn stops(row: &Row) -> Option<&MessageRow> {
    match row {
        Row::Message(row)
            if row.stored.message.deleted
                || !matches!(row.stored.message.content, MessageContent::System(_)) =>
        {
            Some(row)
        }
        _ => None,
    }
}

impl Shell {
    // ----- which message has the keyboard ------------------------------------

    /// The message in focus.
    pub(super) fn focused_row(&self) -> Option<&MessageRow> {
        let key = self.keys.focused.as_deref()?;
        self.open
            .as_ref()?
            .rows
            .iter()
            .filter_map(stops)
            .find(|row| row.key == key)
    }

    /// The message in focus, as a copy.
    pub(super) fn focused_message(&self) -> Option<Message> {
        self.focused_row().map(|row| row.stored.message.clone())
    }

    /// Where a row is in the list.
    fn row_index(&self, key: &str) -> Option<usize> {
        self.open
            .as_ref()?
            .rows
            .iter()
            .position(|row| matches!(row, Row::Message(row) if row.key == key))
    }

    /// The row key of a message, when it is among the rows at hand.
    pub(super) fn key_of(&self, message: &MessageId) -> Option<String> {
        self.open.as_ref()?.rows.iter().find_map(|row| match row {
            Row::Message(row) if &row.stored.message.id == message => Some(row.key.clone()),
            _ => None,
        })
    }

    /// Gives a message the keyboard, and brings it on screen.
    pub(super) fn focus_message(
        &mut self,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.row_index(&key) else {
            return;
        };
        if self.keys.focused.as_deref() != Some(key.as_str()) {
            self.keys.whole = false;
        }
        self.keys.focused = Some(key);
        self.pane_list = false;
        if let Some(open) = &self.open {
            // The list stops following its end while the keyboard is
            // somewhere else in it; it follows again at the bottom.
            open.list.pause_following_tail();
            open.list.scroll_to_reveal_item(index);
        }
        // The keyboard is the shell's now: nothing typed goes anywhere.
        self.focus.focus(window, cx);
        gpui_kit::base::TextSelection::clear(window, cx);
        cx.notify();
    }

    /// The newest message takes the keyboard.
    pub(super) fn focus_newest(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let newest = self
            .open
            .as_ref()
            .and_then(|open| open.rows.iter().rev().find_map(stops))
            .map(|row| row.key.clone());
        if let Some(key) = newest {
            self.focus_message(key, window, cx);
        }
    }

    /// Moves the focus `by` messages: up for a negative number. At the
    /// top, older history is asked for.
    fn step_focus(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        let keys: Vec<&str> = open
            .rows
            .iter()
            .filter_map(stops)
            .map(|row| row.key.as_str())
            .collect();
        if keys.is_empty() {
            return;
        }
        let at = self
            .keys
            .focused
            .as_deref()
            .and_then(|focused| keys.iter().position(|key| *key == focused));
        let last = keys.len() as isize - 1;
        let next = match at {
            Some(at) => (at as isize + by).clamp(0, last),
            None => last,
        };
        let key = keys[next as usize].to_owned();
        let at_top = next == 0;
        self.focus_message(key, window, cx);
        if at_top && by < 0 {
            // What is older comes in above; the focus stays where it is.
            self.load_older(cx);
        }
    }

    /// The keyboard goes back to the composer.
    pub(super) fn leave_messages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.keys.clear();
        self.composer
            .update(cx, |composer, cx| composer.focus(window, cx));
        cx.notify();
    }

    /// The keys that are the conversation's while the composer has the
    /// keyboard, seen before the text field's own bindings: Escape gives
    /// up a reply or an edit under way, Up in an empty composer goes to
    /// the newest message, and what the registry binds with Cmd, Ctrl or
    /// Alt is the registry's (a text field would move its caret on
    /// Alt+Up).
    pub(super) fn composer_key(
        &mut self,
        stroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.open.is_none()
            || self.overlay != Overlay::None
            || !self.composer.focus_handle(cx).is_focused(window)
        {
            return false;
        }
        let plain = !stroke.modifiers.modified();
        if stroke.key == "escape"
            && plain
            && self.acting.compose != super::message_actions::Compose::New
        {
            // The list of people to mention, or of emoji, closes first.
            if !self.mention_matches(cx).is_empty() || self.emoji_completing() {
                return false;
            }
            self.cancel_compose(window, cx);
            return true;
        }
        // Escape with nothing typed and nothing under way: the keyboard
        // goes to the chat list. The conversation stays open.
        if stroke.key == "escape"
            && plain
            && self.thread_search.is_none()
            && self.mention_matches(cx).is_empty()
            && self.composer.read(cx).value().is_empty()
        {
            self.focus_list(window, cx);
            return true;
        }
        if stroke.key == "up" && plain && self.composer.read(cx).value().is_empty() {
            self.focus_newest(window, cx);
            return self.keys.focused.is_some();
        }
        let modified =
            stroke.modifiers.alt || stroke.modifiers.control || stroke.modifiers.platform;
        if !modified {
            return false;
        }
        match keys::resolve(stroke, self.key_context(window, cx)) {
            // On macOS Option with an arrow moves the caret by a word:
            // with something typed, it is the text field's.
            Some(Command::PaneLeft | Command::PaneRight)
                if cfg!(target_os = "macos") && !self.composer.read(cx).value().is_empty() =>
            {
                false
            }
            Some(command) => {
                let ran = self.run_command(command, window, cx);
                if ran {
                    cx.notify();
                }
                ran
            }
            None => false,
        }
    }

    // ----- the registry, run -----------------------------------------------------

    /// What is true of the window for the registry: a chat is open, a
    /// message has the keyboard.
    pub(super) fn key_context(&self, window: &Window, cx: &gpui_kit::App) -> keys::Context {
        let rail = self.overlay == Overlay::None && self.rail_focused(window).is_some();
        let attaching = self.overlay == Overlay::AttachSheet && self.attach.is_some();
        keys::Context {
            // The keys of a chat are those of a conversation on screen:
            // with Status in its place they do nothing to the chat behind.
            chat: self.conversation_showing(),
            message: self.keys.focused.is_some()
                && self.overlay == Overlay::None
                && self.focus.is_focused(window),
            // Anything but the shell itself having the keyboard may be a
            // text field: a key on its own is then left alone.
            typing: if attaching {
                // Over the sheet, only the caption is a text field.
                self.caption_has_keyboard(window, cx)
            } else {
                !self.focus.is_focused(window) && !rail
            },
            viewer: self.overlay == Overlay::Viewer,
            list: self.list_has_keyboard(window) && !self.recording_has_keyboard(window),
            rail,
            recording: self.recording_has_keyboard(window),
            attaching,
            status: self.status_list_has_keyboard(window),
            story: self.story_is_open(),
        }
    }

    /// The chat the chat commands are about: the open one.
    fn open_summary(&self) -> Option<client_core::ChatSummary> {
        self.open.as_ref().map(|open| open.chat.clone())
    }

    /// Opens the chat `by` rows from the open one in the list (the first
    /// or the last when none is open), skipping what is not a chat.
    fn step_chat(
        &mut self,
        by: isize,
        unread_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let chats: Vec<(ChatId, bool)> = self
            .list_rows
            .iter()
            .filter_map(|row| match row {
                ListRow::Chat(chat) => Some((chat.id.clone(), chat.unread_count > 0)),
                _ => None,
            })
            .collect();
        if chats.is_empty() {
            return;
        }
        let at = self
            .open
            .as_ref()
            .and_then(|open| chats.iter().position(|(id, _)| *id == open.chat.id));
        let count = chats.len() as isize;
        let mut next = match at {
            Some(at) => at as isize,
            None if by > 0 => -1,
            None => count,
        };
        for _ in 0..chats.len() {
            next = (next + by).rem_euclid(count);
            if !unread_only || chats[next as usize].1 {
                let chat = chats[next as usize].0.clone();
                self.open_chat(chat, Some(window), cx);
                return;
            }
        }
    }

    /// Selects the number `by` places from the one on screen in the rail's
    /// order, or the one at `place` (from 0).
    fn step_account(
        &mut self,
        place: Option<usize>,
        by: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let accounts: Vec<_> = self
            .linked_accounts()
            .map(|account| account.id.clone())
            .collect();
        if accounts.is_empty() {
            return;
        }
        let at = self
            .account
            .as_ref()
            .and_then(|current| accounts.iter().position(|id| id == current))
            .unwrap_or(0);
        let next = match place {
            Some(place) if place < accounts.len() => place,
            Some(_) => return,
            None => (at as isize + by).rem_euclid(accounts.len() as isize) as usize,
        };
        self.select_account(accounts[next].clone(), window, cx);
    }

    /// Runs a command. `false` when it does not apply right now: the key
    /// then goes on to whoever else wants it.
    pub(super) fn run_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        use Command as C;
        if let Some(ran) = self.run_record_command(command, window, cx) {
            return ran;
        }
        if let Some(ran) = self.run_picker_command(command, window, cx) {
            if ran {
                cx.notify();
            }
            return ran;
        }
        if let Some(ran) = self.run_palette_command(command, window, cx) {
            if ran {
                cx.notify();
            }
            return ran;
        }
        if let Some(ran) = self.run_pane_command(command, window, cx) {
            if ran {
                cx.notify();
            }
            return ran;
        }
        if let Some(ran) = self.run_status_command(command, window, cx) {
            if ran {
                cx.notify();
            }
            return ran;
        }
        let caps = self.engine.capabilities();
        match command {
            // ----- anywhere
            C::Palette => {
                if self.overlay == Overlay::Palette {
                    self.close_overlay(window, cx);
                } else {
                    self.open_palette(false, window, cx);
                }
            }
            C::SearchMessages => self.open_palette(true, window, cx),
            C::Shortcuts => self.toggle_overlay(Overlay::Shortcuts, window, cx),
            C::FindNext | C::FindPrevious => {
                if self.thread_search.is_none() {
                    self.open_thread_search(window, cx);
                } else {
                    return self.step_match(command == C::FindNext, cx);
                }
            }
            C::Settings => self.toggle_overlay(Overlay::Settings, window, cx),
            C::SettingsAppearance
            | C::SettingsSync
            | C::SettingsNotifications
            | C::SettingsAudio
            | C::SettingsKeyboard => {
                self.open_overlay(Overlay::Settings, window, cx);
                self.settings_section = match command {
                    C::SettingsAppearance => SettingsSection::Appearance,
                    C::SettingsSync => SettingsSection::Sync,
                    C::SettingsKeyboard => SettingsSection::Keyboard,
                    C::SettingsAudio => SettingsSection::Audio,
                    _ => SettingsSection::Notifications,
                };
            }
            C::NewChat if caps.start_chat => self.toggle_overlay(Overlay::NewChat, window, cx),
            C::NewGroup if caps.group_create && self.account.is_some() => {
                self.open_overlay(Overlay::NewGroup, window, cx)
            }
            C::NewCommunity
                if caps.group_create && caps.community_manage && self.account.is_some() =>
            {
                self.open_new_group(super::social::GroupKind::Community, window, cx)
            }
            C::Menu => {
                let menu = if self.open.is_some() {
                    Overlay::ChatMenu
                } else {
                    Overlay::ListMenu
                };
                self.toggle_overlay(menu, window, cx);
            }
            C::Find => {
                self.overlay = Overlay::None;
                if self.open.is_some() {
                    self.open_thread_search(window, cx);
                } else {
                    self.search.update(cx, |field, cx| field.focus(window, cx));
                }
            }
            C::NextChat => self.step_chat(1, false, window, cx),
            C::PreviousChat => self.step_chat(-1, false, window, cx),
            C::NextUnread => self.step_chat(1, true, window, cx),
            C::Account(place) => self.step_account(Some(usize::from(place) - 1), 0, window, cx),
            C::NextAccount => self.step_account(None, 1, window, cx),
            C::PreviousAccount => self.step_account(None, -1, window, cx),
            C::ToggleTheme => self.toggle_theme(cx),
            C::Larger => self.step_scale(Some(true), cx),
            C::Smaller => self.step_scale(Some(false), cx),
            C::ActualSize => self.step_scale(None, cx),
            C::MarkAllRead => {
                for row in &self.list_rows {
                    if let ListRow::Chat(chat) = row {
                        if chat.unread_count > 0 {
                            self.engine.mark_read(&chat.account_id, &chat.id);
                        }
                    }
                }
            }
            C::ShowArchived => self.set_filter(ChatFilter::Archived, cx),
            C::AddNumber if caps.link_accounts => self.open_overlay(Overlay::AddNumber, window, cx),
            C::SignOut if self.can_sign_out() => {
                self.open_overlay(Overlay::ConfirmSignOut, window, cx)
            }

            // ----- a chat
            C::Attach if self.conversation_showing() && self.can_attach() => {
                self.pick_attachments(cx)
            }
            C::NewPoll if self.open.is_some() && caps.polls => {
                self.open_overlay(Overlay::NewPoll, window, cx)
            }
            C::ChatInfo => match self.open_summary() {
                Some(chat) => self.open_chat_info(&chat, window, cx),
                None => return false,
            },
            C::MuteChat | C::ArchiveChat | C::PinChat | C::MarkUnread if caps.chat_state => {
                let Some(chat) = self.open_summary() else {
                    return false;
                };
                let change = match command {
                    C::MuteChat => ChatChange::Muted(!chat.muted),
                    C::ArchiveChat => ChatChange::Archived(!chat.archived),
                    C::PinChat => ChatChange::Pinned(!chat.pinned),
                    _ => ChatChange::MarkedUnread,
                };
                self.engine.update_chat(&chat.account_id, &chat.id, change);
                // A chat just marked unread or put away is not one to keep
                // looking at: looking would read it again.
                if matches!(
                    change,
                    ChatChange::MarkedUnread | ChatChange::Archived(true)
                ) {
                    self.close_chat(cx);
                }
            }
            C::CloseChat => {
                if self.open.is_none() {
                    return false;
                }
                self.close_chat(cx);
                // With no conversation the keyboard is the chat list's.
                self.focus.focus(window, cx);
            }
            C::FocusMessages if self.open.is_some() => self.focus_newest(window, cx),
            C::EmojiPicker if self.open.is_some() => self.toggle_emoji_picker(window, cx),

            // ----- moving through the messages
            C::MessageUp => self.step_focus(-1, window, cx),
            C::MessageDown => self.step_focus(1, window, cx),
            C::MessageFirst => self.step_focus(isize::MIN / 2, window, cx),
            C::MessageLast => self.step_focus(isize::MAX / 2, window, cx),
            C::PageUp | C::PageDown => {
                let Some(open) = &self.open else { return false };
                let screen = open.list.viewport_bounds().size.height * 0.85;
                open.list.scroll_by(if command == C::PageUp {
                    -screen
                } else {
                    screen
                });
            }
            C::BackToComposer => {
                // Out of a selection first, then out of the messages.
                if self.acting.selecting.is_some() {
                    self.end_selecting(cx);
                } else {
                    self.leave_messages(window, cx);
                }
            }
            C::ExtendUp => return self.extend_selection(true, window, cx),
            C::ExtendDown => return self.extend_selection(false, window, cx),
            C::SelectMessages => return self.toggle_selected(cx),

            // ----- the message in focus, or the ones selected
            C::Copy if self.acting.selecting.is_some() => return self.copy_selection(cx),
            C::Copy => return self.copy_focused(window, cx),
            C::Star => return self.star_subjects(cx),
            C::Delete => return self.begin_delete(window, cx),
            C::Forward => return self.begin_forward(window, cx),
            C::Reply | C::Edit | C::React | C::Info => {
                let Some(message) = self.focused_message() else {
                    return false;
                };
                if self.command_state(command, &message)
                    != super::message_menu::CommandState::Enabled
                {
                    return false;
                }
                return match command {
                    C::Reply => self.begin_reply(window, cx),
                    C::Edit => self.begin_message_edit(window, cx),
                    C::React => self.begin_react(window, cx),
                    _ => self.begin_info(window, cx),
                };
            }
            C::Reactions => {
                let Some(key) = self.keys.focused.clone() else {
                    return false;
                };
                return self.open_reactors(key, None, None, window, cx);
            }
            C::CopyLink => {
                let Some(link) = self.focused_message().as_ref().and_then(copy_link) else {
                    return false;
                };
                cx.write_to_clipboard(ClipboardItem::new_string(link));
            }
            C::CopyImage => return self.copy_image(cx),
            C::SaveAs if self.acting.selecting.is_some() => return self.save_selection(cx),
            C::SaveAs => return self.save_as(None, cx),
            C::SaveToDownloads => return self.save_to_downloads(None, cx),
            C::SelectText => {
                if self.focused_row().is_none() {
                    return false;
                }
                self.keys.whole = true;
            }
            C::Open => return self.open_focused(window, cx),
            C::Download => return self.download_focused(cx),
            C::MessageMenu => {
                let Some(key) = self.keys.focused.clone() else {
                    return false;
                };
                self.open_message_menu(key, None, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Whether "Sign out" is offered: not for the demo data.
    fn can_sign_out(&self) -> bool {
        matches!(
            self.session,
            super::shell::SessionKind::Keychain | super::shell::SessionKind::Unsaved
        )
    }

    /// Copies what is selected, else the text of the message in focus.
    fn copy_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let selected = gpui_kit::base::TextSelection::selected_text(window, cx);
        if !selected.trim().is_empty() && !self.keys.whole {
            cx.write_to_clipboard(ClipboardItem::new_string(selected.trim().to_owned()));
            return true;
        }
        let Some(text) = self.focused_message().as_ref().and_then(copy_text) else {
            // A picture with nothing written under it: the picture.
            return self.copy_image(cx);
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        true
    }

    /// Opens what the message in focus carries: its picture in the
    /// viewer, its file with the system's application, its link in the
    /// browser.
    fn open_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.focused_message() else {
            return false;
        };
        if message.deleted || message.extras.view_once {
            return false;
        }
        match &message.content {
            MessageContent::Media(media) => match media.kind {
                MediaKind::Image => self.begin_viewing(media.clone(), window, cx),
                MediaKind::Sticker => return false,
                MediaKind::Voice | MediaKind::Audio => self.toggle_audio(media.clone(), cx),
                MediaKind::Video | MediaKind::Document => self.open_file(media.clone(), cx),
            },
            _ => match copy_link(&message) {
                Some(link) if link.starts_with("http") => self.open_link(&link, cx),
                _ => return false,
            },
        }
        true
    }

    /// Fetches the file of the message in focus.
    fn download_focused(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(message) = self.focused_message() else {
            return false;
        };
        let MessageContent::Media(media) = &message.content else {
            return false;
        };
        if message.deleted || message.extras.view_once {
            return false;
        }
        let Some(url) = media.source.as_ref().map(|source| source.to_string()) else {
            return false;
        };
        match media.kind {
            MediaKind::Image | MediaKind::Sticker => self.media.ask(&message.account_id, &url),
            _ => self.media.retry_file(&message.account_id, &url),
        }
        cx.notify();
        true
    }

    /// Where something that belongs to the message in focus opens when
    /// no pointer says where: at the row's top, inside the conversation.
    pub(super) fn focused_anchor(&self) -> Point<Pixels> {
        let row = self
            .keys
            .focused
            .as_deref()
            .and_then(|key| self.row_index(key))
            .and_then(|index| self.open.as_ref()?.list.bounds_for_item(index));
        match row {
            Some(row) => gpui_kit::point(
                row.left() + row.size.width / 2. - crate::theme::px(124.),
                row.top() + crate::theme::px(12.),
            ),
            None => gpui_kit::point(self.viewport.width / 2., self.viewport.height / 3.),
        }
    }

    /// Whether a message can be passed on, and if not, why.
    pub(super) fn forwardable(&self, message: &Message) -> Result<(), &'static str> {
        match self.engine.forward_refusal(message) {
            None => Ok(()),
            Some(refusal) => Err(refusal.reason()),
        }
    }

    /// The message menu: at the pointer, or beside the message in focus.
    pub(super) fn open_message_menu(
        &mut self,
        key: String,
        at: Option<Point<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_message(key, window, cx);
        if self.keys.focused.is_none() {
            return;
        }
        self.message_menu_at = at;
        self.open_overlay(Overlay::MessageMenu, window, cx);
    }
}
