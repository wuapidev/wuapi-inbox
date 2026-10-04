//! The command palette, and the sheet that lists every shortcut.
//!
//! The palette is one field over everything the application can go to or
//! do, the way an editor's is. Cmd or Ctrl with P (or K) opens it to go
//! somewhere: chats, people of the address book, messages (the local
//! full-text index). Cmd or Ctrl with Shift and P opens it on the
//! commands. A prefix says what is looked through, and can be typed or
//! deleted at any time: `>` commands, `@` people, `#` groups, `/` (or
//! `in:`) the messages of the open conversation, `?` the list of these.
//!
//! Every command is an entry of the registry (`crate::keys`), so nothing
//! that is on a key or in a menu is missing here; a command that needs to
//! be told something asks for it in place (`palette_steps.rs`).
//!
//! Commands are matched as they are typed. What is read from the store
//! (chats, people, messages) is read off the UI thread, a moment after
//! the typing stops; a newer keystroke drops the read that was under way.

use super::palette_steps::{Avail, Step, StepKind, OF_A_MESSAGE};
use super::shell::{ListRow, Overlay, Shell};
use super::widgets::mono;
use crate::format::list_time;
use crate::icons::{icon, IconName};
use crate::keys::{self, Binding, Command, Section, BINDINGS};
use crate::motion;
use crate::theme::px;
use crate::theme::{metrics, Palette as Colours};
use client_core::{message_preview, ChatSummary, SearchHit};
use client_provider::{ChatId, ChatKind, Contact, Message, Timestamp};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, Context, Div, Entity, FontWeight, KeyDownEvent, SharedString, Stateful, Task, Window,
};
use std::collections::HashMap;

/// How many of each kind the palette lists.
const CHATS: usize = 6;
const PEOPLE: usize = 5;
const COMMANDS: usize = 7;
const MESSAGES: usize = 12;
/// How many recent chats and commands are remembered.
const RECENT: usize = 5;
/// The file that remembers which commands are used, next to the settings.
const USAGE_FILE: &str = "palette.json";
/// A command that cannot be run is listed, dimmed, only when what was
/// typed is this close to its name: a word of it, at least.
const CLOSE: u32 = 600;

/// What the palette is looking through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    /// Where to go: chats, people, messages, and the commands after them.
    All,
    /// `>`: commands.
    Commands,
    /// `@`: people.
    People,
    /// `#`: groups.
    Groups,
    /// `/` or `in:`: the messages of the open conversation.
    InChat,
    /// Messages of every chat (Cmd or Ctrl with Shift and F).
    Messages,
    /// `?`: what the prefixes are.
    Help,
}

/// The prefixes, as `?` lists them.
pub(super) const MODES: [(&str, &str); 5] = [
    ("", "Go to a chat, a person or a message"),
    (">", "Run a command"),
    ("@", "Find a person"),
    ("#", "Find a group"),
    ("/", "Search this conversation"),
];

/// Splits what was typed into what to look through and what to look for.
pub(super) fn parse(typed: &str, messages_only: bool) -> (Scope, &str) {
    let typed = typed.trim_start();
    let lowered = typed.to_lowercase();
    if lowered.starts_with("in:") {
        return (Scope::InChat, typed[3..].trim());
    }
    match typed.chars().next() {
        Some('>') => (Scope::Commands, typed[1..].trim()),
        Some('@') => (Scope::People, typed[1..].trim()),
        Some('#') => (Scope::Groups, typed[1..].trim()),
        Some('/') => (Scope::InChat, typed[1..].trim()),
        Some('?') => (Scope::Help, typed[1..].trim()),
        _ if messages_only => (Scope::Messages, typed.trim()),
        _ => (Scope::All, typed.trim()),
    }
}

/// How well `text` answers `query`, higher being better; `None` when it
/// does not. The whole name beats a name that starts with it, which beats
/// its initials ("np" for "New poll"), which beat a word that starts with
/// it, which beats having it somewhere, which beats having its letters in
/// order. Among equals the shorter name wins.
pub(super) fn score(query: &str, text: &str) -> Option<u32> {
    let (query, text) = (query.trim().to_lowercase(), text.to_lowercase());
    if query.is_empty() {
        return Some(0);
    }
    let brevity = 99u32.saturating_sub(text.chars().count() as u32);
    if text == query {
        return Some(1000);
    }
    if text.starts_with(&query) {
        return Some(800 + brevity);
    }
    let words = || {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|word| !word.is_empty())
    };
    // The first letters of its words, from the first: an acronym.
    let initials: String = words().filter_map(|word| word.chars().next()).collect();
    let letters: String = query.chars().filter(|c| !c.is_whitespace()).collect();
    if letters.chars().count() >= 2 && initials.starts_with(&letters) {
        return Some(700 + brevity);
    }
    if words().any(|word| word.starts_with(&query)) {
        return Some(600 + brevity);
    }
    if text.contains(&query) {
        return Some(400 + brevity);
    }
    // Its letters in order, not too far apart: "ngr" finds "New group".
    let mut wanted_letters = letters.chars();
    let mut wanted = wanted_letters.next();
    for c in text.chars() {
        if Some(c) == wanted {
            wanted = wanted_letters.next();
        }
    }
    (wanted.is_none() && letters.chars().count() >= 2).then_some(200 + brevity)
}

/// `items` that answer `query`, best first; equals stay in their order.
pub(super) fn rank<T>(query: &str, items: Vec<T>, text: impl Fn(&T) -> String) -> Vec<T> {
    let mut scored: Vec<(u32, T)> = items
        .into_iter()
        .filter_map(|item| score(query, &text(&item)).map(|score| (score, item)))
        .collect();
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(_, item)| item).collect()
}

/// How often and how lately the commands were run from the palette.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct Usage {
    /// By the command's name: how many times, and the count of all runs
    /// when it was run last (a clock that only the palette moves).
    used: HashMap<String, (u32, u64)>,
    runs: u64,
}

impl Usage {
    fn load(cx: &gpui_kit::App) -> Self {
        crate::settings::sibling(USAGE_FILE, cx)
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, cx: &gpui_kit::App) {
        let Some(file) = crate::settings::sibling(USAGE_FILE, cx) else {
            return;
        };
        if let Ok(bytes) = serde_json::to_vec(self) {
            if let Err(error) = std::fs::write(file, bytes) {
                tracing::warn!(%error, "could not remember the commands used");
            }
        }
    }

    fn note(&mut self, command: Command) {
        self.runs += 1;
        let entry = self.used.entry(format!("{command:?}")).or_default();
        *entry = (entry.0.saturating_add(1), self.runs);
    }

    /// What having been used adds to a command's score: more for one
    /// used often, more for one used lately. Never enough to pass a
    /// better kind of match.
    pub(super) fn boost(&self, command: Command) -> u32 {
        let Some((count, last)) = self.used.get(&format!("{command:?}")) else {
            return 0;
        };
        let often = (*count).min(10) * 4;
        let lately = 40u32.saturating_sub((self.runs.saturating_sub(*last)).min(40) as u32);
        often + lately
    }
}

/// How far down the window the palette's top is: a fixed share of the
/// window's height, so it stays put while its list grows and shrinks.
pub(super) fn palette_top(window_height: gpui_kit::Pixels) -> gpui_kit::Pixels {
    gpui_kit::px((window_height.as_f32() * 0.18).round())
}

/// One row of the palette.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Item {
    /// A heading.
    Section(&'static str),
    /// A chat to open, or to give to the step that asked for one.
    Chat(ChatSummary),
    /// Somebody of the address book to write to.
    Person(Contact),
    /// A message to go to.
    Message(Box<SearchHit>),
    /// A command to run.
    Command(Command),
    /// A command that cannot be run right now, and why.
    Unavailable(Command, &'static str),
    /// A choice of the step being asked: its place among the choices.
    Choice(usize),
    /// A number typed in the "new chat" step: a chat with it.
    Number(String),
    /// A line to read.
    Line(SharedString, SharedString),
}

impl Item {
    fn is_row(&self) -> bool {
        !matches!(self, Item::Section(_) | Item::Line(..))
    }
}

/// What was read from the store for a query.
#[derive(Default)]
struct Found {
    chats: Vec<ChatSummary>,
    people: Vec<Contact>,
    messages: Vec<SearchHit>,
}

/// The palette's state.
pub(super) struct PaletteState {
    pub(super) input: Entity<InputState>,
    /// It was opened to search messages.
    pub(super) messages_only: bool,
    /// What it lists.
    pub(super) items: Vec<Item>,
    /// The row the keyboard is on, among `items`.
    pub(super) cursor: usize,
    /// The questions being asked, the last one on screen.
    pub(super) steps: Vec<Step>,
    /// The message that had the keyboard when the palette opened: what
    /// the commands of a message are about.
    pub(super) subject: Option<Message>,
    /// What the store answered for the query on screen.
    found: Found,
    /// The read under way; replacing it drops it.
    _search: Option<Task<()>>,
    /// The chats opened last, newest first.
    pub(super) recent_chats: Vec<ChatId>,
    /// The commands run from the palette last, newest first.
    pub(super) recent_commands: Vec<Command>,
    /// How often and how lately each command was run; `None` until read.
    pub(super) usage: Option<Usage>,
    /// The search of the shortcuts sheet.
    pub(super) shortcuts_input: Entity<InputState>,
}

impl PaletteState {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        Self {
            input: cx.new(|cx| {
                InputState::new(window, cx).placeholder("Go to a chat, or type > for commands")
            }),
            messages_only: false,
            items: Vec::new(),
            cursor: 0,
            steps: Vec::new(),
            subject: None,
            found: Found::default(),
            _search: None,
            recent_chats: Vec::new(),
            recent_commands: Vec::new(),
            usage: None,
            shortcuts_input: cx
                .new(|cx| InputState::new(window, cx).placeholder("Search an action or a key")),
        }
    }

    /// A chat was opened: it is the most recent one.
    pub(super) fn note_chat(&mut self, chat: &ChatId) {
        self.recent_chats.retain(|known| known != chat);
        self.recent_chats.insert(0, chat.clone());
        self.recent_chats.truncate(RECENT);
    }
}

/// What was typed, as a key is compared: lower case, no spaces, and a
/// "-" between keys read as "+".
fn as_keys(typed: &str) -> String {
    typed
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| if c == '-' { '+' } else { c })
        .collect()
}

/// The keys of a chord as written, one by one and in one order, so that
/// "Cmd+Shift+N", "shift+cmd+n" and "⇧⌘N" are the same thing.
fn key_parts(written: &str) -> Vec<String> {
    let spelled = as_keys(written)
        .replace('⌘', "cmd+")
        .replace('⇧', "shift+")
        .replace('⌥', "alt+")
        .replace('⌃', "ctrl+");
    let mut parts: Vec<String> = spelled
        .split('+')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    // "+" itself is a key: "ctrl++".
    if spelled.ends_with("++") || spelled == "+" {
        parts.push("+".to_owned());
    }
    parts.sort();
    parts
}

/// How well `query` names the keys of `command`, as this platform writes
/// them or as the other does ("ctrl+n", "⌘N"): the keys exactly, or
/// their start once a modifier is typed.
pub(super) fn key_score(query: &str, command: Command) -> Option<u32> {
    let typed = as_keys(query);
    if typed.chars().count() < 2 {
        return None;
    }
    let typed_parts = key_parts(query);
    let binding = keys::binding(command)?;
    let mut best = None;
    for chord in binding.chords {
        for mac in [false, true] {
            let label = chord.label_for(mac);
            if key_parts(&label) == typed_parts {
                best = best.max(Some(950));
            } else if as_keys(&label).starts_with(&typed) && typed.contains('+') {
                best = best.max(Some(500));
            }
        }
    }
    best
}

/// The rows of the shortcuts sheet that answer what was typed in it: by
/// the name of the action, or by its keys.
pub(super) fn shortcut_rows_for(query: &str) -> Vec<(Section, Vec<(&'static Binding, String)>)> {
    let wanted = query.trim().to_lowercase();
    let wanted_keys = as_keys(query);
    shortcut_rows()
        .into_iter()
        .map(|(section, rows)| {
            let rows = rows
                .into_iter()
                .filter(|(binding, keys)| {
                    wanted.is_empty()
                        || binding.label.to_lowercase().contains(&wanted)
                        || as_keys(keys).contains(&wanted_keys)
                })
                .collect::<Vec<_>>();
            (section, rows)
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

/// The rows of the shortcuts sheet: every binding that has keys, by
/// section, with its keys as this platform writes them.
pub(super) fn shortcut_rows() -> Vec<(Section, Vec<(&'static Binding, String)>)> {
    Section::ALL
        .into_iter()
        .map(|section| {
            let rows = BINDINGS
                .iter()
                .filter(|binding| binding.section == section && !binding.chords.is_empty())
                .map(|binding| {
                    let keys = binding
                        .chords
                        .iter()
                        .map(keys::Chord::label)
                        .collect::<Vec<_>>()
                        .join("  or  ");
                    (binding, keys)
                })
                .collect::<Vec<_>>();
            (section, rows)
        })
        .filter(|(_, rows)| !rows.is_empty())
        .collect()
}

impl Shell {
    // ----- the palette --------------------------------------------------------------

    /// Opens the palette: to go somewhere, or over messages alone.
    pub(super) fn open_palette(
        &mut self,
        messages_only: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.palette.messages_only = messages_only;
        self.palette.found = Found::default();
        self.palette._search = None;
        self.palette.steps.clear();
        // What the commands of a message will be about.
        if self.overlay != Overlay::Palette {
            self.palette.subject = self.focused_message();
        }
        if self.palette.usage.is_none() {
            self.palette.usage = Some(Usage::load(cx));
        }
        // Whoever opened it has found it: the tip has done its work.
        if !crate::settings::get(cx).palette_tip_done {
            crate::settings::update(cx, |settings| settings.palette_tip_done = true);
        }
        self.open_overlay(Overlay::Palette, window, cx);
        // People come from the address book in the store; a fresher copy
        // is fetched behind it, as for "New chat".
        if let Some(account) = &self.account {
            self.engine
                .want_contacts(account, std::time::Duration::from_secs(5 * 60));
        }
        self.palette.input.update(cx, |field, cx| {
            field.set_value("", window, cx);
            field.focus(window, cx);
        });
        self.fill_palette(cx);
    }

    /// Opens the palette with a prefix typed: `>` for the commands.
    pub(super) fn open_palette_with(
        &mut self,
        prefix: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_palette(false, window, cx);
        let prefix = prefix.to_owned();
        self.palette
            .input
            .update(cx, |field, cx| field.set_value(prefix, window, cx));
        self.fill_palette(cx);
    }

    /// Opens the palette on the question a command asks.
    pub(super) fn open_palette_on(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay != Overlay::Palette {
            self.open_palette(false, window, cx);
        }
        if let Some(step) = self.step_for(command, None, cx) {
            self.push_step(step, window, cx);
        }
    }

    /// Asks the next question: the field is emptied for its answer.
    fn push_step(&mut self, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        self.palette.steps.push(step);
        self.palette.found = Found::default();
        self.palette._search = None;
        self.palette
            .input
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.fill_palette(cx);
    }

    /// Escape, or Backspace in an empty field: back one question. `false`
    /// when none was being asked.
    pub(super) fn pop_step(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.palette.steps.pop().is_none() {
            return false;
        }
        // Back among the commands, where the question came from.
        let back = if self.palette.steps.is_empty() {
            ">"
        } else {
            ""
        };
        self.palette
            .input
            .update(cx, |field, cx| field.set_value(back, window, cx));
        self.palette.found = Found::default();
        self.fill_palette(cx);
        true
    }

    /// The commands the palette can list right now, with whether each
    /// can be run: everything of the registry that is not hidden here.
    fn palette_commands(&self, cx: &gpui_kit::App) -> Vec<(Command, Avail)> {
        BINDINGS
            .iter()
            .map(|binding| (binding.command, self.palette_avail(binding.command, cx)))
            .filter(|(_, avail)| *avail != Avail::Hidden)
            .collect()
    }

    /// What a command is called in the palette: a number by its name.
    fn command_title(&self, command: Command) -> SharedString {
        match command {
            Command::Account(place) => match self.accounts.get(usize::from(place) - 1) {
                Some(account) => format!("Go to {}", self.account_name(account)).into(),
                None => keys::label(command).into(),
            },
            _ => keys::label(command).into(),
        }
    }

    /// The commands that answer `query`, best first: by how well their
    /// name answers it, then by how often and how lately they were used.
    /// One that cannot be run is there only when it is what was typed.
    fn ranked_commands(&self, query: &str, limit: usize, cx: &gpui_kit::App) -> Vec<Item> {
        let usage = self.palette.usage.clone().unwrap_or_default();
        let mut scored: Vec<(u32, Item)> = self
            .palette_commands(cx)
            .into_iter()
            .filter_map(|(command, avail)| {
                // By its name, or by its keys ("ctrl+n").
                let matched =
                    key_score(query, command).max(score(query, &self.command_title(command)))?;
                match avail {
                    Avail::Enabled => {
                        Some((matched + usage.boost(command), Item::Command(command)))
                    }
                    Avail::Disabled(why) if !query.is_empty() && matched >= CLOSE => {
                        // Under everything that can be run.
                        Some((matched / 4, Item::Unavailable(command, why)))
                    }
                    _ => None,
                }
            })
            .collect();
        // What is about the message in focus comes first when nothing is
        // typed: that is what the palette was opened on.
        if query.is_empty() {
            for (score, item) in &mut scored {
                if matches!(item, Item::Command(command) if OF_A_MESSAGE.contains(command)) {
                    *score += 500;
                }
            }
        }
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored
            .into_iter()
            .take(limit)
            .map(|(_, item)| item)
            .collect()
    }

    /// Puts the rows together from what was typed and what the store
    /// answered last.
    fn fill_palette(&mut self, cx: &mut Context<Self>) {
        let typed = self.palette.input.read(cx).value().to_string();
        let mut items = Vec::new();
        let section = |items: &mut Vec<Item>, title: &'static str, rows: Vec<Item>| {
            if !rows.is_empty() {
                items.push(Item::Section(title));
                items.extend(rows);
            }
        };
        // The chats as the list has them: most recent first.
        let listed = |shell: &Self| -> Vec<ChatSummary> {
            shell
                .list_rows
                .iter()
                .filter_map(|row| match row {
                    ListRow::Chat(chat) => Some(chat.clone()),
                    _ => None,
                })
                .collect()
        };

        if let Some(step) = self.palette.steps.last() {
            // A question: its own list, narrowed by what is typed.
            let query = typed.trim();
            match &step.kind {
                StepKind::Choices(choices) => {
                    let places: Vec<usize> = (0..choices.len()).collect();
                    let ranked = rank(query, places, |place| choices[*place].label.to_string());
                    items.extend(ranked.into_iter().map(Item::Choice));
                }
                StepKind::Chat => {
                    let chats = if query.is_empty() {
                        listed(self)
                    } else {
                        self.palette.found.chats.clone()
                    };
                    let chats = rank(query, chats, |chat| chat.title.clone());
                    items.extend(chats.into_iter().take(CHATS * 2).map(Item::Chat));
                }
                StepKind::Contact => {
                    // A number as typed, then the people it could be.
                    let digits = query.chars().filter(char::is_ascii_digit).count();
                    if digits >= 7
                        && query
                            .chars()
                            .all(|c| c.is_ascii_digit() || "+ -()".contains(c))
                    {
                        items.push(Item::Number(query.to_owned()));
                    }
                    let people = rank(query, self.palette.found.people.clone(), |contact| {
                        contact.display_name()
                    });
                    items.extend(people.into_iter().take(PEOPLE * 2).map(Item::Person));
                }
                StepKind::Lines(lines) => {
                    items.extend(
                        lines
                            .iter()
                            .map(|(name, value)| Item::Line(name.clone(), value.clone())),
                    );
                }
            }
            self.palette.cursor = items.iter().position(Item::is_row).unwrap_or(0);
            self.palette.items = items;
            return cx.notify();
        }

        let (scope, query) = parse(&typed, self.palette.messages_only);
        if scope == Scope::Help {
            items.push(Item::Section("What to type"));
            items.extend((0..MODES.len()).map(Item::Choice));
        } else if scope == Scope::Commands {
            section(&mut items, "Commands", self.ranked_commands(query, 60, cx));
        } else if query.is_empty() && scope == Scope::All {
            // Nothing typed: what was used last, then the chats on top.
            let chats = listed(self);
            let recent: Vec<Item> = self
                .palette
                .recent_chats
                .iter()
                .filter_map(|id| chats.iter().find(|chat| &chat.id == id))
                .map(|chat| Item::Chat(chat.clone()))
                .collect();
            let rest: Vec<Item> = chats
                .iter()
                .filter(|chat| !self.palette.recent_chats.contains(&chat.id))
                .take(CHATS.saturating_sub(recent.len()))
                .map(|chat| Item::Chat(chat.clone()))
                .collect();
            section(&mut items, "Recent", recent);
            section(&mut items, "Chats", rest);
            section(
                &mut items,
                "Commands",
                self.ranked_commands("", COMMANDS, cx),
            );
        } else {
            let found = &self.palette.found;
            if matches!(scope, Scope::All | Scope::Groups) {
                let chats: Vec<ChatSummary> = found
                    .chats
                    .iter()
                    .filter(|chat| scope == Scope::All || chat.kind == ChatKind::Group)
                    .cloned()
                    .collect();
                let chats = rank(query, chats, |chat| chat.title.clone());
                section(
                    &mut items,
                    if scope == Scope::Groups {
                        "Groups"
                    } else {
                        "Chats"
                    },
                    chats.into_iter().take(CHATS).map(Item::Chat).collect(),
                );
            }
            if matches!(scope, Scope::All | Scope::People) {
                // Somebody with a chat is listed as that chat already.
                let people: Vec<Contact> = found
                    .people
                    .iter()
                    .filter(|contact| {
                        scope == Scope::People
                            || !found
                                .chats
                                .iter()
                                .any(|chat| chat.id.as_str() == contact.id.as_str())
                    })
                    .cloned()
                    .collect();
                let people = rank(query, people, |contact| contact.display_name());
                section(
                    &mut items,
                    "People",
                    people.into_iter().take(PEOPLE).map(Item::Person).collect(),
                );
            }
            if scope == Scope::All {
                section(
                    &mut items,
                    "Commands",
                    self.ranked_commands(query, COMMANDS, cx),
                );
            }
            if matches!(scope, Scope::All | Scope::Messages | Scope::InChat) {
                section(
                    &mut items,
                    if scope == Scope::InChat {
                        "In this conversation"
                    } else {
                        "Messages"
                    },
                    self.palette
                        .found
                        .messages
                        .iter()
                        .take(MESSAGES)
                        .map(|hit| Item::Message(Box::new(hit.clone())))
                        .collect(),
                );
            }
        }
        // The keyboard stays on a row, the first one after a change.
        self.palette.cursor = items.iter().position(Item::is_row).unwrap_or(0);
        self.palette.items = items;
        cx.notify();
    }

    /// The text changed: the commands answer at once; the store is asked a
    /// moment later, off this thread, and a newer change drops the asking.
    pub(super) fn palette_changed(&mut self, cx: &mut Context<Self>) {
        let typed = self.palette.input.read(cx).value().to_string();
        // What the question on screen reads from the store, if anything.
        let (scope, query) = match self.palette.steps.last().map(|step| &step.kind) {
            Some(StepKind::Chat) => (Scope::Groups, typed.trim().to_owned()),
            Some(StepKind::Contact) => (Scope::People, typed.trim().to_owned()),
            Some(_) => (Scope::Commands, String::new()),
            None => {
                let (scope, query) = parse(&typed, self.palette.messages_only);
                (scope, query.to_owned())
            }
        };
        // A chat step looks through every chat, not the groups alone.
        let any_chat = matches!(
            self.palette.steps.last().map(|step| &step.kind),
            Some(StepKind::Chat)
        );
        self.palette._search = None;
        if query.is_empty() || matches!(scope, Scope::Commands | Scope::Help) {
            self.palette.found = Found::default();
            return self.fill_palette(cx);
        }
        self.fill_palette(cx);
        let (Some(account), store) = (self.account.clone(), self.engine.store().clone()) else {
            return;
        };
        let in_chat = self.open.as_ref().map(|open| open.chat.id.clone());
        self.palette._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(motion::SEARCH_DEBOUNCE)
                .await;
            let read = cx.background_spawn(async move {
                let mut found = Found::default();
                if matches!(scope, Scope::All | Scope::Groups) || any_chat {
                    found.chats = store.chats(&account, Some(&query)).unwrap_or_default();
                }
                if matches!(scope, Scope::All | Scope::People) {
                    found.people = store
                        .contacts(&account, Some(&query), PEOPLE * 4)
                        .unwrap_or_default();
                }
                if matches!(scope, Scope::All | Scope::Messages | Scope::InChat) {
                    // The index is per account; a conversation's matches
                    // are picked out of a wider read.
                    let wanted = if scope == Scope::InChat {
                        400
                    } else {
                        MESSAGES * 2
                    };
                    found.messages = store
                        .search_messages(&account, &query, wanted)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|hit| match (&scope, &in_chat) {
                            (Scope::InChat, Some(chat)) => &hit.message.chat_id == chat,
                            (Scope::InChat, None) => false,
                            _ => true,
                        })
                        .collect();
                }
                found
            });
            let found = read.await;
            this.update(cx, |this, cx| {
                this.palette.found = found;
                this.fill_palette(cx);
            })
            .ok();
        }));
    }

    /// A command was run from the palette: it is remembered.
    fn note_command(&mut self, command: Command, cx: &mut Context<Self>) {
        self.palette
            .recent_commands
            .retain(|known| *known != command);
        self.palette.recent_commands.insert(0, command);
        self.palette.recent_commands.truncate(RECENT);
        let usage = self.palette.usage.get_or_insert_with(Usage::default);
        usage.note(command);
        usage.save(cx);
    }

    /// Runs the row the keyboard is on (or the one clicked). With `keep`,
    /// the palette stays open where that makes sense: after a toggle, or a
    /// choice of a step.
    pub(super) fn run_palette_item(
        &mut self,
        at: usize,
        keep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.palette.items.get(at).cloned() else {
            return;
        };
        let step = self.palette.steps.last().cloned();
        match item {
            Item::Section(_) | Item::Line(..) | Item::Unavailable(..) => {}
            Item::Chat(chat) => match step {
                // The chat a command was waiting for.
                Some(step) => {
                    self.note_command(step.command, cx);
                    match self.apply_to_chat(step.command, chat, cx) {
                        Some(next) => self.push_step(next, window, cx),
                        None => self.close_overlay(window, cx),
                    }
                }
                None => {
                    self.close_overlay(window, cx);
                    self.open_chat(chat.id, Some(window), cx);
                }
            },
            Item::Person(contact) => self.open_contact(contact, window, cx),
            Item::Number(number) => {
                self.close_overlay(window, cx);
                self.open_number(number, cx);
            }
            Item::Message(hit) => {
                self.close_overlay(window, cx);
                self.open_chat(hit.message.chat_id.clone(), Some(window), cx);
                self.jump_and_flash(hit.message.id.clone(), cx);
            }
            Item::Choice(place) => match step {
                None => {
                    // `?`: the prefix is typed for the user.
                    if let Some((prefix, _)) = MODES.get(place) {
                        let prefix = *prefix;
                        self.palette
                            .input
                            .update(cx, |field, cx| field.set_value(prefix, window, cx));
                        self.fill_palette(cx);
                    }
                }
                Some(step) => {
                    let StepKind::Choices(choices) = &step.kind else {
                        return;
                    };
                    let Some(choice) = choices.get(place) else {
                        return;
                    };
                    self.note_command(step.command, cx);
                    self.apply_choice(&step, choice.value.clone(), window, cx);
                    if keep {
                        // The same question, with what is set now marked.
                        if let Some(again) = self.step_for(step.command, step.chat.clone(), cx) {
                            if let Some(last) = self.palette.steps.last_mut() {
                                *last = again;
                            }
                        }
                        self.fill_palette(cx);
                    } else if self.overlay == Overlay::Palette {
                        self.close_overlay(window, cx);
                    }
                }
            },
            Item::Command(command) => {
                self.note_command(command, cx);
                // One that asks: its question, in place.
                if let Some(step) = self.step_for(command, None, cx) {
                    return self.push_step(step, window, cx);
                }
                if keep {
                    self.run_command(command, window, cx);
                    if self.overlay == Overlay::Palette {
                        self.fill_palette(cx);
                    }
                } else {
                    self.close_overlay(window, cx);
                    self.run_command(command, window, cx);
                }
            }
        }
        cx.notify();
    }

    /// The keys of the palette: the arrows walk the rows, skipping the
    /// headings; Tab takes the row's name into the field of a question;
    /// Backspace in an empty field goes back one question.
    pub(super) fn palette_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        if key == "backspace"
            && !self.palette.steps.is_empty()
            && self.palette.input.read(cx).value().is_empty()
        {
            return self.pop_step(window, cx);
        }
        let rows: Vec<usize> = self
            .palette
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.is_row())
            .map(|(index, _)| index)
            .collect();
        if rows.is_empty() {
            return false;
        }
        let at = rows
            .iter()
            .position(|index| *index == self.palette.cursor)
            .unwrap_or(0);
        self.palette.cursor = match key {
            "down" => rows[(at + 1) % rows.len()],
            "up" => rows[(at + rows.len() - 1) % rows.len()],
            "pagedown" => rows[(at + 6).min(rows.len() - 1)],
            "pageup" => rows[at.saturating_sub(6)],
            _ => return false,
        };
        cx.notify();
        true
    }

    /// Tab in the palette: the name of the row the keyboard is on goes
    /// into the field of the question being asked. Seen before the
    /// window's own Tab.
    pub(super) fn palette_tab(
        &mut self,
        stroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.overlay != Overlay::Palette || stroke.key != "tab" || stroke.modifiers.modified() {
            return false;
        }
        let Some(step) = self.palette.steps.last() else {
            return false;
        };
        let name: Option<String> = match self.palette.items.get(self.palette.cursor) {
            Some(Item::Choice(place)) => match &step.kind {
                StepKind::Choices(choices) => {
                    choices.get(*place).map(|choice| choice.label.to_string())
                }
                _ => None,
            },
            Some(Item::Chat(chat)) => Some(chat.title.clone()),
            Some(Item::Person(contact)) => Some(contact.display_name()),
            _ => None,
        };
        let Some(name) = name else {
            return true;
        };
        self.palette
            .input
            .update(cx, |field, cx| field.set_value(name, window, cx));
        self.palette_changed(cx);
        true
    }

    /// The first-run tip over the composer: where everything is. It goes
    /// when it is dismissed, or when the palette is opened, for good.
    pub(super) fn render_palette_tip(
        &self,
        colours: &Colours,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if crate::settings::get(cx).palette_tip_done {
            return None;
        }
        let key = |command| keys::keys_label(command).unwrap_or_default();
        Some(
            div()
                .debug_selector(|| "palette-tip".into())
                .mb_2()
                .px_3()
                .py(px(6.))
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(colours.border)
                .bg(colours.muted)
                .flex()
                .items_center()
                .gap_3()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(18.))
                .text_color(colours.text)
                .child(icon(IconName::Keyboard, px(15.), colours.icon))
                .child(div().flex_1().min_w_0().child(format!(
                    "Tip: {} finds any chat, {} runs any action, {} lists every shortcut.",
                    key(Command::Palette),
                    key(Command::PaletteCommands),
                    key(Command::Shortcuts)
                )))
                .child(
                    div()
                        .id("palette-tip-dismiss")
                        .debug_selector(|| "palette-tip-dismiss".into())
                        .flex_none()
                        .cursor_pointer()
                        .text_color(colours.accent)
                        .hover(|style| style.opacity(0.75))
                        .on_click(cx.listener(|_, _, _, cx| {
                            crate::settings::update(cx, |settings| {
                                settings.palette_tip_done = true
                            });
                            cx.notify();
                        }))
                        .child(mono("GOT IT")),
                ),
        )
    }

    pub(super) fn render_palette(
        &self,
        colours: &Colours,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let now = Timestamp::now();
        let step = self.palette.steps.last();
        let mut list = div()
            .id("palette-list")
            // As tall as the window leaves under its top, the field and
            // the footer; the results scroll inside.
            .max_h(
                (self.viewport.height - palette_top(self.viewport.height) - px(150.))
                    .min(px(440.))
                    .max(px(120.)),
            )
            .overflow_y_scroll()
            .p_1()
            .flex()
            .flex_col();
        for (index, item) in self.palette.items.iter().enumerate() {
            // The icon, the name, what is said beside it (dim), its keys,
            // and whether it can be run.
            let (glyph, title, detail, keys, dim): (
                IconName,
                SharedString,
                SharedString,
                Option<String>,
                bool,
            ) = match item {
                Item::Section(title) => {
                    list = list.child(
                        div()
                            .debug_selector(move || format!("palette-section-{index}"))
                            .px_2()
                            .pt(px(8.))
                            .pb(px(3.))
                            .child(
                                mono(format!("[ {} ]", title.to_uppercase()))
                                    .text_color(colours.text_muted),
                            ),
                    );
                    continue;
                }
                Item::Line(name, value) => {
                    list = list.child(
                        div()
                            .debug_selector(move || format!("palette-line-{index}"))
                            .h(px(32.))
                            .px(px(12.))
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .text_size(metrics::TEXT_BODY())
                            .child(div().min_w_0().truncate().child(name.clone()))
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(metrics::TEXT_SMALL())
                                    .text_color(colours.text_muted)
                                    .child(value.clone()),
                            ),
                    );
                    continue;
                }
                Item::Chat(chat) => (
                    match chat.kind {
                        ChatKind::Group => IconName::Users,
                        ChatKind::Direct => IconName::MessageCircle,
                    },
                    chat.title.clone().into(),
                    match (step.map(|step| step.command), chat.unread_count) {
                        // What the command would find: its state now.
                        (Some(Command::PinAChat), _) if chat.pinned => "Pinned".into(),
                        (Some(Command::ArchiveAChat), _) if chat.archived => "Archived".into(),
                        (Some(Command::MuteAChat), _) if chat.muted => "Muted".into(),
                        (_, 0) => "".into(),
                        (_, 1) => "1 unread".into(),
                        (_, n) => format!("{n} unread").into(),
                    },
                    None,
                    false,
                ),
                Item::Person(contact) => (
                    IconName::UserPlus,
                    contact.display_name().into(),
                    contact
                        .phone
                        .as_deref()
                        .map(crate::format::phone)
                        .unwrap_or_default()
                        .into(),
                    None,
                    false,
                ),
                Item::Number(number) => (
                    IconName::SquarePen,
                    format!("Chat with {number}").into(),
                    "".into(),
                    None,
                    false,
                ),
                Item::Message(hit) => (
                    IconName::MessageSquareText,
                    message_preview(&hit.message).into(),
                    format!(
                        "{} · {}",
                        hit.chat_title,
                        list_time(hit.message.timestamp, now)
                    )
                    .into(),
                    None,
                    false,
                ),
                Item::Command(command) => {
                    // The value it would change, else where it belongs.
                    let value = self.command_detail(*command, cx);
                    let section = keys::binding(*command)
                        .map(|binding| binding.section.title())
                        .unwrap_or_default();
                    (
                        IconName::ChevronRight,
                        self.command_title(*command),
                        if value.is_empty() {
                            section.into()
                        } else {
                            format!("{section} · {value}").into()
                        },
                        keys::keys_label(*command),
                        false,
                    )
                }
                Item::Unavailable(command, why) => (
                    IconName::Ban,
                    self.command_title(*command),
                    (*why).into(),
                    None,
                    true,
                ),
                Item::Choice(place) => match step.map(|step| &step.kind) {
                    Some(StepKind::Choices(choices)) => match choices.get(*place) {
                        Some(choice) => (
                            if choice.current {
                                IconName::Check
                            } else {
                                IconName::Circle
                            },
                            choice.label.clone(),
                            choice.detail.clone(),
                            None,
                            false,
                        ),
                        None => continue,
                    },
                    // `?`: a prefix and what it does.
                    _ => match MODES.get(*place) {
                        Some((prefix, what)) => (
                            IconName::ChevronRight,
                            (*what).into(),
                            if prefix.is_empty() {
                                "nothing".into()
                            } else {
                                (*prefix).into()
                            },
                            None,
                            false,
                        ),
                        None => continue,
                    },
                },
            };
            let on = index == self.palette.cursor;
            let hover = colours.elevated_selected;
            list = list.child(
                div()
                    .id(("palette-item", index))
                    .debug_selector(move || format!("palette-item-{index}"))
                    .relative()
                    .h(px(36.))
                    .pl(px(12.))
                    .pr_2()
                    .rounded(px(5.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .cursor_pointer()
                    .text_size(metrics::TEXT_BODY())
                    .when(dim, |this| this.text_color(colours.text_faint))
                    // The row the keyboard is on: filled, with a bar of
                    // the accent on its left.
                    .when(on, |this| {
                        this.bg(colours.elevated_selected).child(
                            div()
                                .debug_selector(|| "palette-cursor".into())
                                .absolute()
                                .left_0()
                                .top(px(6.))
                                .bottom(px(6.))
                                .w(px(3.))
                                .rounded_full()
                                .bg(colours.focus_ring),
                        )
                    })
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.run_palette_item(index, false, window, cx);
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(18.))
                            .flex()
                            .justify_center()
                            .child(icon(
                                glyph,
                                px(15.),
                                if dim {
                                    colours.text_faint
                                } else {
                                    colours.icon
                                },
                            )),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when(!detail.is_empty(), |this| {
                        this.child(
                            div()
                                .debug_selector(move || format!("palette-detail-{index}"))
                                .flex_none()
                                .max_w(px(240.))
                                .truncate()
                                .text_size(metrics::TEXT_SMALL())
                                .text_color(colours.text_muted)
                                .child(detail),
                        )
                    })
                    // What Enter does to somewhere to go, on the row the
                    // keyboard is on.
                    .when(
                        on && step.is_none()
                            && matches!(item, Item::Chat(_) | Item::Person(_) | Item::Message(_)),
                        |this| {
                            this.child(
                                mono(format!("{} to open", keys::key_label("enter")))
                                    .debug_selector(|| "palette-enter".into())
                                    .flex_none()
                                    .text_color(colours.text_muted),
                            )
                        },
                    )
                    .children(keys.map(|keys| {
                        mono(keys)
                            .debug_selector(move || format!("palette-keys-{index}"))
                            .px(px(6.))
                            .py(px(2.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(colours.elevated_border)
                            .text_color(colours.text_muted)
                    })),
            );
        }
        let empty = !self
            .palette
            .items
            .iter()
            .any(|item| item.is_row() || matches!(item, Item::Line(..)));
        // The few keys worth knowing where the palette is now.
        let typed = self.palette.input.read(cx).value().to_string();
        let scope = parse(&typed, self.palette.messages_only).0;
        let stay = format!(
            "ENTER RUN   {} RUN AND STAY   ESC CLOSE",
            keys::Chord::label(&keys::RUN_AND_STAY).to_uppercase()
        );
        let hints: SharedString = match (step.map(|step| &step.kind), scope) {
            (Some(StepKind::Lines(_)), _) => "ESC BACK".into(),
            (Some(_), _) => "ENTER CHOOSE   TAB COMPLETE   ESC BACK".into(),
            (None, Scope::Commands) => stay.into(),
            (None, Scope::Help) => "ENTER CHOOSE   ESC CLOSE".into(),
            (None, _) => {
                "ENTER OPEN   > COMMANDS   @ PEOPLE   / THIS CONVERSATION   ? ALL PREFIXES".into()
            }
        };
        self.card("palette", colours)
            .w(metrics::PALETTE_WIDTH())
            .max_w(self.viewport.width - px(32.))
            .overflow_hidden()
            .flex()
            .flex_col()
            // A rule of the accent along its top: this is the thing in
            // front.
            .child(
                div()
                    .debug_selector(|| "palette-rule".into())
                    .flex_none()
                    .h(px(2.5))
                    .bg(colours.accent),
            )
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
                    // The question being asked, before its answer.
                    .children(step.map(|step| {
                        div()
                            .debug_selector(|| "palette-step".into())
                            .flex_none()
                            .max_w(px(260.))
                            .truncate()
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(px(4.))
                            .bg(colours.elevated_selected)
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(colours.text)
                            .child(step.title.clone())
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.palette.input).appearance(false)),
                    )
                    .child(mono(keys::key_label("escape")).text_color(colours.text_muted)),
            )
            .child(list)
            .when(empty, |this| {
                this.child(
                    div()
                        .debug_selector(|| "palette-empty".into())
                        .px_4()
                        .pb_4()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(colours.text_muted)
                        .child("Nothing found."),
                )
            })
            .child(
                div()
                    .px_3()
                    .py_2()
                    .border_t_1()
                    .border_color(colours.border)
                    .child(
                        mono(hints)
                            .debug_selector(|| "palette-hints".into())
                            .text_color(colours.text_muted),
                    ),
            )
    }

    // ----- the shortcuts sheet ------------------------------------------------------

    pub(super) fn render_shortcuts(
        &self,
        colours: &Colours,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let mut body = div()
            .id("shortcuts-list")
            .max_h(px(520.))
            .overflow_y_scroll()
            .px_4()
            .pb_4()
            .flex()
            .flex_col();
        let mut row_index = 0usize;
        let wanted = self.palette.shortcuts_input.read(cx).value().to_string();
        let found = shortcut_rows_for(&wanted);
        if found.is_empty() {
            body = body.child(
                div()
                    .debug_selector(|| "shortcuts-empty".into())
                    .pt_4()
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(colours.text_muted)
                    .child("No shortcut by that name or key."),
            );
        }
        for (section, rows) in found {
            body = body.child(
                div().pt_4().pb_1().child(
                    mono(format!("[ {} ]", section.title().to_uppercase()))
                        .text_color(colours.text_muted),
                ),
            );
            for (binding, keys) in rows {
                let index = row_index;
                row_index += 1;
                body = body.child(
                    div()
                        .debug_selector(move || format!("shortcut-{index}"))
                        .min_h(px(30.))
                        .py(px(4.))
                        .border_b_1()
                        .border_color(colours.border)
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(metrics::TEXT_BODY())
                                .child(binding.label),
                        )
                        .child(
                            mono(keys)
                                .text_color(colours.text)
                                .font_weight(FontWeight::MEDIUM),
                        ),
                );
            }
        }
        self.card("shortcuts", colours)
            .w(px(560.))
            .max_w(self.viewport.width - px(32.))
            .flex()
            .flex_col()
            .child(super::menus::panel_header(
                "Keyboard shortcuts",
                colours,
                cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
            ))
            // Typing narrows it: by what an action is called, or by a key.
            .child(
                div()
                    .debug_selector(|| "shortcuts-search".into())
                    .flex_none()
                    .h(px(44.))
                    .px_3()
                    .border_b_1()
                    .border_color(colours.border)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(IconName::Search, px(15.), colours.text_muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.palette.shortcuts_input).appearance(false)),
                    ),
            )
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefix_says_what_to_look_through() {
        assert_eq!(parse("lisbon", false), (Scope::All, "lisbon"));
        assert_eq!(parse("  > new gr", false), (Scope::Commands, "new gr"));
        assert_eq!(parse("@ana", false), (Scope::People, "ana"));
        assert_eq!(parse("#trip", false), (Scope::Groups, "trip"));
        assert_eq!(parse("in: dentist", false), (Scope::InChat, "dentist"));
        assert_eq!(parse("IN:dentist", false), (Scope::InChat, "dentist"));
        assert_eq!(parse("/dentist", false), (Scope::InChat, "dentist"));
        assert_eq!(parse("?", false), (Scope::Help, ""));
        // Opened to search messages: that is what it does, unless told.
        assert_eq!(parse("dentist", true), (Scope::Messages, "dentist"));
        assert_eq!(parse(">theme", true), (Scope::Commands, "theme"));
        assert_eq!(parse("", false), (Scope::All, ""));
    }

    #[test]
    fn the_best_answer_comes_first() {
        let names = |query: &str, items: &[&str]| -> Vec<String> {
            rank(query, items.iter().map(|s| s.to_string()).collect(), |s| {
                s.clone()
            })
        };
        // Whole name, then a name that starts with it, then a word that
        // does, then anywhere, then its letters in order.
        assert_eq!(
            names(
                "new",
                &[
                    "Renew the lease",
                    "Brand new day",
                    "New group",
                    "New",
                    "Next week"
                ]
            ),
            [
                "New",
                "New group",
                "Brand new day",
                "Renew the lease",
                "Next week"
            ]
        );
        // Among names that start with it, the shorter one.
        assert_eq!(
            names("ma", &["Mateo Salazar", "Marta"]),
            ["Marta", "Mateo Salazar"]
        );
        // Letters in order find a command by its initials.
        assert_eq!(names("ngr", &["New chat", "New group"]), ["New group"]);
        // Initials are an acronym: they beat a word that merely starts
        // with what was typed, and anything further down.
        assert_eq!(
            names("np", &["Snap a picture", "New group", "New poll", "Npm"]),
            ["Npm", "New poll", "New group", "Snap a picture"]
        );
        assert_eq!(names("mar", &["Mark all as read", "Marta"])[0], "Marta");
        assert_eq!(
            names("tgl", &["Light or dark", "Settings"]),
            Vec::<String>::new()
        );
        // Case and stray spaces do not matter.
        assert_eq!(names("  LISBON ", &["Lisbon trip ✈️"]), ["Lisbon trip ✈️"]);
        // Equals keep their order: the list's own (most recent first).
        assert_eq!(names("a", &["Ana", "Abe"]), ["Ana", "Abe"]);
        // One letter does not match by "letters in order".
        assert_eq!(score("x", "Settings"), None);
        // Nothing typed matches everything, equally.
        assert_eq!(names("", &["b", "a"]), ["b", "a"]);
    }

    #[test]
    fn the_sheet_lists_every_shortcut_of_the_registry() {
        let rows = shortcut_rows();
        let listed: Vec<Command> = rows
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(binding, _)| binding.command))
            .collect();
        for binding in BINDINGS {
            assert_eq!(
                listed.contains(&binding.command),
                !binding.chords.is_empty(),
                "{:?}",
                binding.command
            );
        }
        // Every row says its keys, all of them.
        for (_, rows) in &rows {
            for (binding, keys) in rows {
                assert!(!keys.is_empty(), "{:?}", binding.command);
                assert_eq!(keys.matches("  or  ").count(), binding.chords.len() - 1);
            }
        }
        // In the sheet's own order of sections.
        let sections: Vec<Section> = rows.iter().map(|(section, _)| *section).collect();
        let mut sorted = sections.clone();
        sorted.sort_by_key(|section| Section::ALL.iter().position(|known| known == section));
        assert_eq!(sections, sorted);
    }
}
