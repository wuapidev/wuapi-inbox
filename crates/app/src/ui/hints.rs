//! What the controls say when pointed at: their name and, for the ones
//! that are a command of the registry, its keys. Nobody learns a shortcut
//! that is not written where the pointer already is.

use crate::keys::{self, Command};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{Div, SharedString, Stateful};

/// The controls that say something: by their name in the window, what
/// they are called, and the command they stand for.
pub(super) const CONTROLS: [(&str, &str, Option<Command>); 14] = [
    ("nav-chats", "Chats", Some(Command::ShowChats)),
    ("nav-status", "Status", Some(Command::ShowStatus)),
    ("new-chat", "New chat", Some(Command::NewChat)),
    ("list-menu", "Menu", Some(Command::Menu)),
    (
        "chat-search",
        "Search in the conversation",
        Some(Command::Find),
    ),
    ("chat-menu", "Menu", Some(Command::Menu)),
    ("attach", "Attach files", Some(Command::Attach)),
    ("emoji", "Emoji", Some(Command::EmojiPicker)),
    ("voice", "Record a voice note", Some(Command::RecordVoice)),
    ("send", "Send", None),
    ("add-number", "Add a number", Some(Command::AddNumber)),
    ("toggle-theme", "Light or dark", Some(Command::ToggleTheme)),
    ("shortcuts", "Keyboard shortcuts", Some(Command::Shortcuts)),
    ("settings", "Settings", Some(Command::Settings)),
];

/// A name with the keys of its command after it: "New chat  Ctrl+N".
pub(super) fn with_keys(name: &str, command: Option<Command>) -> SharedString {
    match command.and_then(keys::keys_label) {
        Some(keys) => format!("{name}  {keys}").into(),
        None => name.to_owned().into(),
    }
}

/// What the control called `id` says when pointed at.
pub(super) fn hint(id: &str) -> Option<SharedString> {
    CONTROLS
        .iter()
        .find(|(known, _, _)| *known == id)
        .map(|(_, name, command)| with_keys(name, *command))
}

/// Gives a control its hint.
pub(super) fn hinted(control: Stateful<Div>, id: &'static str) -> Stateful<Div> {
    match hint(id) {
        Some(hint) => {
            control.tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
        }
        None => control,
    }
}

/// The line of the start screen that says where Status is: its key, for
/// whoever has not seen the button in the rail.
pub(super) fn status_line() -> String {
    format!(
        "{} for Status, {} for a new one",
        keys::keys_label(Command::ShowStatus).unwrap_or_default(),
        keys::keys_label(Command::NewStatus).unwrap_or_default()
    )
}

/// The line that says where everything is: on the start screen, and on
/// the way in.
pub(super) fn keys_line() -> String {
    let key = |command| keys::keys_label(command).unwrap_or_default();
    format!(
        "Press {} to do anything, {} for all shortcuts",
        key(Command::Palette),
        key(Command::Shortcuts)
    )
}
