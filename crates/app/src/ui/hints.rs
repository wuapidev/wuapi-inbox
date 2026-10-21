//! What the controls say when pointed at: their name and, for the ones
//! that are a command of the registry, its keys. Nobody learns a shortcut
//! that is not written where the pointer already is.

use crate::i18n;
use crate::keys::{self, Command};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{Div, SharedString, Stateful};

/// The controls that say something: by their name in the window, what
/// they are called, and the command they stand for.
pub(super) fn controls() -> [(&'static str, &'static str, Option<Command>); 14] {
    [
        (
            "nav-chats",
            i18n::t("common.hints.chats"),
            Some(Command::ShowChats),
        ),
        (
            "nav-status",
            i18n::t("common.hints.status"),
            Some(Command::ShowStatus),
        ),
        (
            "new-chat",
            i18n::t("common.hints.new_chat"),
            Some(Command::NewChat),
        ),
        (
            "list-menu",
            i18n::t("common.hints.menu"),
            Some(Command::Menu),
        ),
        (
            "chat-search",
            i18n::t("common.hints.search_conversation"),
            Some(Command::Find),
        ),
        (
            "chat-menu",
            i18n::t("common.hints.menu"),
            Some(Command::Menu),
        ),
        (
            "attach",
            i18n::t("common.hints.attach"),
            Some(Command::Attach),
        ),
        (
            "emoji",
            i18n::t("common.hints.emoji"),
            Some(Command::EmojiPicker),
        ),
        (
            "voice",
            i18n::t("common.hints.voice"),
            Some(Command::RecordVoice),
        ),
        ("send", i18n::t("common.hints.send"), None),
        (
            "add-number",
            i18n::t("common.hints.add_number"),
            Some(Command::AddNumber),
        ),
        (
            "toggle-theme",
            i18n::t("common.hints.theme"),
            Some(Command::ToggleTheme),
        ),
        (
            "shortcuts",
            i18n::t("common.hints.shortcuts"),
            Some(Command::Shortcuts),
        ),
        (
            "settings",
            i18n::t("common.hints.settings"),
            Some(Command::Settings),
        ),
    ]
}

/// A name with the keys of its command after it: "New chat  Ctrl+N".
pub(super) fn with_keys(name: &str, command: Option<Command>) -> SharedString {
    match command.and_then(keys::keys_label) {
        Some(keys) => format!("{name}  {keys}").into(),
        None => name.to_owned().into(),
    }
}

/// What the control called `id` says when pointed at.
pub(super) fn hint(id: &str) -> Option<SharedString> {
    controls()
        .into_iter()
        .find(|(known, _, _)| *known == id)
        .map(|(_, name, command)| with_keys(name, command))
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
    i18n::tr(
        "common.hints.status_line",
        &[
            (
                "status",
                &keys::keys_label(Command::ShowStatus).unwrap_or_default(),
            ),
            (
                "new",
                &keys::keys_label(Command::NewStatus).unwrap_or_default(),
            ),
        ],
    )
}

/// The line that says where everything is: on the start screen, and on
/// the way in.
pub(super) fn keys_line() -> String {
    let key = |command| keys::keys_label(command).unwrap_or_default();
    i18n::tr(
        "common.hints.keys_line",
        &[
            ("palette", &key(Command::Palette)),
            ("shortcuts", &key(Command::Shortcuts)),
        ],
    )
}
