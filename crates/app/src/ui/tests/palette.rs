//! The palette as an editor's: Ctrl+P to go somewhere, Ctrl+Shift+P for
//! the commands, prefixes for the rest, and questions asked in place.

use super::keyboard::{palette_rows, settle_search};
use super::*;
use crate::keys::{self, Command, When, BINDINGS};
use crate::ui::palette::Item;
use crate::ui::palette_steps::{
    live_words, menu_command, push_live_line, rail_command, Avail, OF_A_MESSAGE,
};
use crate::ui::rail_menu::RailTarget;

fn open_one(cx: &mut TestAppContext) -> Harness {
    open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    )
}

fn overlay(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

fn typed(harness: &Harness, cx: &mut TestAppContext) -> String {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .palette
            .input
            .read(cx)
            .value()
            .to_string()
    })
}

/// The names of the rows of one kind.
fn rows_of(harness: &Harness, cx: &mut TestAppContext, kind: &str) -> Vec<String> {
    palette_rows(harness, cx)
        .into_iter()
        .filter(|(found, _)| *found == kind)
        .map(|(_, title)| title)
        .collect()
}

/// Opens the palette on its commands and runs the one called `name`.
fn run(harness: &Harness, cx: &mut TestAppContext, name: &str) {
    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, name, cx);
    let rows = palette_rows(harness, cx);
    assert_eq!(
        rows.first().map(|(kind, title)| (*kind, title.as_str())),
        Some(("command", name)),
        "{rows:?}"
    );
    press(harness.window, "enter", cx);
}

#[gpui_kit::test]
fn every_menu_entry_and_every_shortcut_is_a_command_of_the_palette(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let harness = open_with_numbers(cx, 1);
    let first = cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_chat(client_provider::ChatId::new(first), None, cx)
        })
    });
    cx.run_until_parked();

    // The menus of the chat list, of a conversation and of a row.
    let mut walked = 0;
    for menu in [Overlay::ListMenu, Overlay::ChatMenu, Overlay::RowMenu] {
        let ids: Vec<&'static str> = cx.update(|cx| {
            harness.shell.update(cx, |shell, _| {
                shell.menu_target = shell.open.as_ref().map(|open| open.chat.clone());
                shell.overlay = menu;
                let ids = shell.menu_items_now().iter().map(|item| item.id).collect();
                shell.overlay = Overlay::None;
                ids
            })
        });
        assert!(!ids.is_empty(), "{menu:?}");
        for id in ids {
            let command = menu_command(id).unwrap_or_else(|| panic!("no command for `{id}`"));
            assert!(keys::binding(command).is_some(), "{id}");
            walked += 1;
        }
    }
    // The menu of a message.
    for (command, id, _) in crate::ui::message_menu::MENU {
        assert!(OF_A_MESSAGE.contains(&command), "{id}");
        assert!(keys::binding(command).is_some(), "{id}");
        walked += 1;
    }
    // The menus of the rail: of a number, and of a group of numbers.
    let account = stored_accounts(&harness)[0].id.clone();
    let group = group_first(&harness, 2, cx);
    for target in [RailTarget::Account(account), RailTarget::Group(group)] {
        let ids: Vec<String> = cx
            .update_window(harness.window.into(), |_, window, cx| {
                harness.shell.update(cx, |shell, cx| {
                    shell.open_rail_menu(target.clone(), Default::default(), window, cx);
                    let ids = shell
                        .rail_entries()
                        .iter()
                        .map(|entry| entry.id.clone())
                        .collect();
                    shell.close_overlay(window, cx);
                    ids
                })
            })
            .unwrap();
        assert!(!ids.is_empty(), "{target:?}");
        for id in ids {
            let command = rail_command(&id).unwrap_or_else(|| panic!("no command for `{id}`"));
            assert!(keys::binding(command).is_some(), "{id}");
            walked += 1;
        }
    }
    assert!(walked > 40, "{walked} entries walked");

    // Every command that applies anywhere or in a chat is listed, able to
    // run or dimmed with a reason; the few that are not are keys that
    // mean nothing as a row.
    let unlisted = [
        Command::Palette,
        Command::PaletteCommands,
        Command::FindNext,
        Command::FindPrevious,
        Command::LeaveConversation,
        Command::Menu,
        Command::PaneLeft,
        Command::PaneRight,
    ];
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        for binding in BINDINGS {
            if !matches!(binding.when, When::Anywhere | When::Chat) {
                continue;
            }
            let avail = shell.palette_avail(binding.command, cx);
            let hidden = matches!(binding.command, Command::Account(place) if place > 3)
                || binding.command == Command::SignOut;
            assert_eq!(
                avail == Avail::Hidden,
                unlisted.contains(&binding.command) || hidden,
                "{:?}: {avail:?}",
                binding.command
            );
        }
        // Every command has a name to list it by.
        for binding in BINDINGS {
            assert!(!binding.label.is_empty(), "{:?}", binding.command);
        }
    });
}

#[gpui_kit::test]
fn the_keys_open_it_in_a_mode_and_the_prefixes_change_it(cx: &mut TestAppContext) {
    let harness = open_one(cx);
    focus_composer(&harness, cx);

    // Ctrl+P: where to go. Chats first, nothing typed.
    press(harness.window, "ctrl-p", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
    assert_eq!(typed(&harness, cx), "");
    assert_eq!(palette_rows(&harness, cx)[0].0, "chat");
    // The same key closes it; Ctrl+K is the same thing.
    press(harness.window, "ctrl-p", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    press(harness.window, "ctrl-k", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
    press(harness.window, "escape", cx);

    // Ctrl+Shift+P: the commands, with ">" typed.
    press(harness.window, "ctrl-shift-p", cx);
    assert_eq!(typed(&harness, cx), ">");
    let rows = palette_rows(&harness, cx);
    assert!(rows.len() > 40, "{} commands", rows.len());
    assert!(rows.iter().all(|(kind, _)| *kind == "command"));
    // Deleting the prefix is going back to "go to".
    press(harness.window, "backspace", cx);
    assert_eq!(typed(&harness, cx), "");
    assert_eq!(palette_rows(&harness, cx)[0].0, "chat");

    // "?" lists the prefixes; choosing one types it.
    type_text(harness.window, "?", cx);
    let modes = rows_of(&harness, cx, "mode");
    assert_eq!(modes, ["", ">", "@", "#", "/"]);
    press(harness.window, "down", cx);
    press(harness.window, "enter", cx);
    assert_eq!(typed(&harness, cx), ">");
    assert_eq!(overlay(&harness, cx), Overlay::Palette);

    // "/" looks through the open conversation, and only there.
    press(harness.window, "backspace", cx);
    let said = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell
            .open
            .as_ref()
            .unwrap()
            .rows
            .iter()
            .find_map(|row| match row {
                Row::Message(row) => match &row.stored.message.content {
                    MessageContent::Text { body } => body
                        .split_whitespace()
                        .find(|word| word.len() > 4 && word.chars().all(char::is_alphabetic))
                        .map(str::to_owned),
                    _ => None,
                },
                _ => None,
            })
            .expect("a word of the conversation")
    });
    type_text(harness.window, &format!("/{said}"), cx);
    settle_search(&harness, cx);
    let rows = palette_rows(&harness, cx);
    assert!(
        !rows.is_empty() && rows.iter().all(|(kind, _)| *kind == "message"),
        "{rows:?}"
    );
    // The footer says what the prefixes are.
    assert!(shows(harness.window, "palette-hints", cx));
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

#[gpui_kit::test]
fn a_command_that_needs_to_be_told_something_asks_in_place(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| prepare(cx, Some(dir.path().join(settings::FILE_NAME))));
    let harness = open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let chat = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .open
            .as_ref()
            .unwrap()
            .chat
            .id
            .clone()
    });
    let steps =
        |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).palette.steps.len());

    // Mute the open chat for eight hours: the command, then how long.
    run(&harness, cx, "Mute the chat for…");
    assert_eq!((overlay(&harness, cx), steps(cx)), (Overlay::Palette, 1));
    assert!(shows(harness.window, "palette-step", cx));
    assert_eq!(typed(&harness, cx), "", "the field is the answer's");
    assert_eq!(
        rows_of(&harness, cx, "choice"),
        ["8 hours", "1 week", "1 year", "Until I unmute it"]
    );
    type_text(harness.window, "8", cx);
    assert_eq!(rows_of(&harness, cx, "choice"), ["8 hours"]);
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    harness.settle(cx);
    assert_eq!(harness.mock.timed_mutes(), [(chat.clone(), 8 * 3600)]);
    cx.update(|cx| assert!(harness.shell.read(cx).open.as_ref().unwrap().chat.muted));

    // Mute any chat: which one, then how long; Escape goes back a step
    // at a time, and only then closes.
    run(&harness, cx, "Mute a chat…");
    let offered = rows_of(&harness, cx, "chat");
    assert!(offered.len() > 3);
    let other = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_rows
            .iter()
            .find_map(|row| match row {
                ListRow::Chat(found) if found.id != chat && !found.muted => Some(found.clone()),
                _ => None,
            })
            .unwrap()
    });
    type_text(harness.window, &other.title, cx);
    settle_search(&harness, cx);
    assert_eq!(rows_of(&harness, cx, "chat")[0], other.title);
    // Tab takes the row's name into the field.
    press(harness.window, "backspace", cx);
    press(harness.window, "tab", cx);
    assert_eq!(typed(&harness, cx), other.title);
    settle_search(&harness, cx);
    press(harness.window, "enter", cx);
    assert_eq!(steps(cx), 2);
    assert_eq!(rows_of(&harness, cx, "choice").len(), 4);
    press(harness.window, "escape", cx);
    assert_eq!((overlay(&harness, cx), steps(cx)), (Overlay::Palette, 1));
    press(harness.window, "enter", cx);
    assert_eq!(steps(cx), 2);
    type_text(harness.window, "week", cx);
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    harness.settle(cx);
    assert_eq!(
        harness.mock.timed_mutes().last(),
        Some(&(other.id.clone(), 7 * 24 * 3600))
    );
    // Backspace in an empty field goes back too, then Escape closes.
    run(&harness, cx, "Theme…");
    assert_eq!(steps(cx), 1);
    press(harness.window, "backspace", cx);
    assert_eq!((overlay(&harness, cx), steps(cx)), (Overlay::Palette, 0));
    assert_eq!(typed(&harness, cx), ">");
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);

    // The interface size: its steps, the one in use marked; 90 %.
    run(&harness, cx, "Interface size…");
    assert_eq!(
        rows_of(&harness, cx, "choice"),
        ["80%", "90%", "100%", "110%", "125%"]
    );
    type_text(harness.window, "90", cx);
    press(harness.window, "enter", cx);
    assert_eq!(cx.update(|cx| settings::get(cx).interface_scale), 90);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    // Ctrl+Enter sets and keeps asking: another can be tried at once.
    run(&harness, cx, "Interface size…");
    type_text(harness.window, "110", cx);
    press(harness.window, "ctrl-enter", cx);
    assert_eq!(cx.update(|cx| settings::get(cx).interface_scale), 110);
    assert_eq!((overlay(&harness, cx), steps(cx)), (Overlay::Palette, 1));
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "100", cx);
    press(harness.window, "enter", cx);
    assert_eq!(cx.update(|cx| settings::get(cx).interface_scale), 100);

    // A new chat with a number: typed where the command asked.
    run(&harness, cx, "New chat");
    type_text(harness.window, "+58 424 555 0199", cx);
    settle_search(&harness, cx);
    assert_eq!(
        palette_rows(&harness, cx)[0],
        ("number", "+58 424 555 0199".to_owned())
    );
    press(harness.window, "enter", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert_eq!(
            shell.open.as_ref().map(|open| open.chat.id.to_string()),
            Some("+584245550199".to_owned())
        );
    });

    // What only reads: where the connection stands.
    run(&harness, cx, "Check the connection");
    assert!(shows(harness.window, "palette-line-0", cx));
    cx.update(|cx| {
        let lines: Vec<String> = harness
            .shell
            .read(cx)
            .palette
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Line(name, value) => Some(format!("{name}: {value}")),
                _ => None,
            })
            .collect();
        assert!(lines
            .iter()
            .any(|line| line.starts_with("Waiting to be sent")));
        assert!(
            lines.iter().any(|line| line == "Sending files: Available"),
            "{lines:?}"
        );
    });
    press(harness.window, "escape", cx);
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
}

#[gpui_kit::test]
fn commands_are_found_by_their_initials_and_what_is_used_comes_first(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    cx.update(|cx| prepare(cx, Some(dir.path().join(settings::FILE_NAME))));
    let harness = open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    press(harness.window, "ctrl-shift-p", cx);
    // Initials.
    type_text(harness.window, "np", cx);
    assert_eq!(rows_of(&harness, cx, "command")[0], "New poll");
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, ">mar", cx);
    assert_eq!(rows_of(&harness, cx, "command")[0], "Mark all as read");
    // A word of the name, wherever it is.
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, ">wallpaper", cx);
    assert_eq!(rows_of(&harness, cx, "command"), ["Chat wallpaper…"]);
    // A toggle says what it is set to, and Ctrl+Enter flips it in place.
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, ">read receipts", cx);
    let at = cx.update(|cx| harness.shell.read(cx).palette.cursor);
    assert!(shows(
        harness.window,
        Box::leak(format!("palette-detail-{at}").into_boxed_str()),
        cx
    ));
    assert!(cx.update(|cx| settings::get(cx).read_receipts));
    press(harness.window, "ctrl-enter", cx);
    assert!(!cx.update(|cx| settings::get(cx).read_receipts));
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
    press(harness.window, "ctrl-enter", cx);
    assert!(cx.update(|cx| settings::get(cx).read_receipts));
    press(harness.window, "escape", cx);

    // What was used is first the next time, among equals: with nothing
    // typed, and among the commands a word finds.
    press(harness.window, "ctrl-shift-p", cx);
    assert_eq!(rows_of(&harness, cx, "command")[0], "Read receipts");
    type_text(harness.window, "settings", cx);
    let before = rows_of(&harness, cx, "command");
    press(harness.window, "escape", cx);
    run(&harness, cx, "Settings: Audio");
    press(harness.window, "escape", cx);
    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, "settings:", cx);
    let after = rows_of(&harness, cx, "command");
    assert_eq!(after[0], "Settings: Audio", "{before:?} became {after:?}");
    press(harness.window, "escape", cx);
    // It is kept next to the settings.
    let kept = std::fs::read_to_string(dir.path().join("palette.json")).unwrap();
    assert!(kept.contains("SettingsAudio") && kept.contains("ToggleReadReceipts"));

    // A command that cannot be run is there, dimmed, with the reason,
    // only when it is what was typed.
    cx.update(|cx| harness.shell.update(cx, |shell, cx| shell.close_chat(cx)));
    press(harness.window, "ctrl-shift-p", cx);
    assert!(!rows_of(&harness, cx, "command").contains(&"New poll".to_owned()));
    assert!(rows_of(&harness, cx, "unavailable").is_empty());
    type_text(harness.window, "new poll", cx);
    assert_eq!(rows_of(&harness, cx, "unavailable"), ["New poll"]);
    cx.update(|cx| {
        let reason = harness
            .shell
            .read(cx)
            .palette
            .items
            .iter()
            .find_map(|item| match item {
                Item::Unavailable(Command::NewPoll, why) => Some(*why),
                _ => None,
            });
        assert_eq!(reason, Some("Open a chat first"));
    });
    // Enter on it does nothing.
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
}

#[gpui_kit::test]
fn the_commands_of_a_message_are_there_only_with_a_message_in_focus(cx: &mut TestAppContext) {
    let harness = open_one(cx);
    focus_composer(&harness, cx);
    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, "reply", cx);
    assert!(rows_of(&harness, cx, "command").is_empty());
    press(harness.window, "escape", cx);

    // With a message in focus they come first, with their keys.
    press(harness.window, "up", cx);
    let message = cx.update(|cx| harness.shell.read(cx).focused_message().unwrap());
    press(harness.window, "ctrl-shift-p", cx);
    let rows = rows_of(&harness, cx, "command");
    assert_eq!(rows[0], "Reply", "{rows:?}");
    for name in ["Forward", "Star or unstar", "Delete", "Message info"] {
        assert!(
            rows.contains(&name.to_owned()) || name == "Message info",
            "{name}: {rows:?}"
        );
    }
    let at = cx.update(|cx| harness.shell.read(cx).palette.cursor);
    assert!(shows(
        harness.window,
        Box::leak(format!("palette-keys-{at}").into_boxed_str()),
        cx
    ));
    // Run from here, it is about that message, and the keyboard goes
    // where the command puts it.
    press(harness.window, "enter", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    cx.update(|cx| {
        let compose = &harness.shell.read(cx).acting.compose;
        assert!(
            matches!(compose, crate::ui::message_actions::Compose::Reply(to) if to.id == message.id),
            "{compose:?}"
        );
    });
    press(harness.window, "escape", cx);

    // One the message cannot take is dimmed, with the menu's own reason.
    focus_composer(&harness, cx);
    press(harness.window, "up", cx);
    let own = cx.update(|cx| {
        harness.shell.read(cx).focused_message().unwrap().direction == Direction::Outgoing
    });
    press(harness.window, "ctrl-shift-p", cx);
    type_text(harness.window, "edit", cx);
    let (can, cannot) = (
        rows_of(&harness, cx, "command"),
        rows_of(&harness, cx, "unavailable"),
    );
    if own {
        assert!(can.contains(&"Edit".to_owned()) || cannot.contains(&"Edit".to_owned()));
    } else {
        assert!(
            !can.contains(&"Edit".to_owned()),
            "somebody else's message: {can:?}"
        );
    }
    press(harness.window, "escape", cx);
}

#[gpui_kit::test]
fn pinning_moved_to_its_new_key(cx: &mut TestAppContext) {
    let harness = open_one(cx);
    focus_composer(&harness, cx);
    let pinned = |cx: &mut TestAppContext| {
        cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.pinned)
    };
    let before = pinned(cx);
    // Ctrl+Shift+P is the palette now.
    press(harness.window, "ctrl-shift-p", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Palette);
    press(harness.window, "escape", cx);
    harness.settle(cx);
    assert_eq!(pinned(cx), before);
    // Ctrl+Shift+T pins.
    press(harness.window, "ctrl-shift-t", cx);
    harness.settle(cx);
    assert_eq!(pinned(cx), !before);
    assert_eq!(
        keys::keys_label(Command::PinChat).as_deref(),
        Some(if cfg!(target_os = "macos") {
            "⇧⌘T"
        } else {
            "Ctrl+Shift+T"
        })
    );
    assert_eq!(
        keys::keys_label(Command::PaletteCommands).as_deref(),
        Some(if cfg!(target_os = "macos") {
            "⇧⌘P"
        } else {
            "Ctrl+Shift+P"
        })
    );
}

// ----- the shortcuts, where people look ---------------------------------------------

#[gpui_kit::test]
fn the_shortcuts_are_a_click_away_and_every_control_says_its_keys(cx: &mut TestAppContext) {
    use crate::ui::hints::{hint, keys_line, CONTROLS};
    let harness = open_one(cx);
    harness.settle(cx);
    focus_composer(&harness, cx);

    // The rail's keyboard button, with the theme and the settings.
    let (tools, button) = (
        bounds(harness.window, "rail-tools", cx),
        bounds(harness.window, "shortcuts", cx),
    );
    assert!(within(button, tools));
    assert!(button.top() > bounds(harness.window, "toggle-theme", cx).top());
    assert!(button.bottom() <= bounds(harness.window, "settings", cx).top());
    click(harness.window, "shortcuts", cx);
    assert_eq!(overlay(&harness, cx), Overlay::Shortcuts);
    assert!(shows(harness.window, "shortcuts-search", cx));

    // The sheet is searched by typing: by the action, or by the key.
    assert!(shows(harness.window, "shortcut-0", cx) && shows(harness.window, "shortcut-20", cx));
    // (What is typed is drawn on the next frame.)
    let redraw = |cx: &mut TestAppContext| {
        cx.update_window(harness.window.into(), |_, window, _| window.refresh())
            .unwrap();
        cx.run_until_parked();
    };
    type_text(harness.window, "new chat", cx);
    redraw(cx);
    assert!(shows(harness.window, "shortcut-0", cx) && !shows(harness.window, "shortcut-1", cx));
    press(harness.window, "ctrl-a", cx);
    // The keys as this platform writes them ("Ctrl+Shift+M", "⇧⌘M"): the
    // sheet lists, and is searched by, the platform's own.
    let mute_keys = keys::keys_label(Command::MuteChat).unwrap();
    type_text(harness.window, &mute_keys.to_lowercase(), cx);
    redraw(cx);
    assert!(shows(harness.window, "shortcut-0", cx) && !shows(harness.window, "shortcut-1", cx));
    assert_eq!(
        crate::ui::palette::shortcut_rows_for(&mute_keys.to_lowercase())[0].1[0]
            .0
            .command,
        Command::MuteChat
    );
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "zzzz", cx);
    redraw(cx);
    assert!(shows(harness.window, "shortcuts-empty", cx));
    press(harness.window, "escape", cx);
    assert_eq!(overlay(&harness, cx), Overlay::None);
    // Opened again, it lists everything again.
    click(harness.window, "shortcuts", cx);
    assert!(shows(harness.window, "shortcut-20", cx));
    press(harness.window, "escape", cx);

    // The same sheet from the two "⋮" menus.
    for menu in ["list-menu", "chat-menu"] {
        click(harness.window, menu, cx);
        click(harness.window, "menu-shortcuts", cx);
        assert_eq!(overlay(&harness, cx), Overlay::Shortcuts, "{menu}");
        press(harness.window, "escape", cx);
    }

    // Every control that is a command says its keys when pointed at, as
    // this platform writes them, and is on screen to be pointed at.
    for (id, name, command) in CONTROLS {
        let said = hint(id).unwrap().to_string();
        assert!(said.starts_with(name), "{id}: {said}");
        match command.and_then(keys::keys_label) {
            Some(keys) => assert!(said.ends_with(&keys), "{id}: `{said}` lacks {keys}"),
            None => assert_eq!(said, name, "{id}"),
        }
        // "Send" takes the microphone's place once something is typed;
        // "Add a number" is there where numbers can be linked.
        if !matches!(id, "send" | "add-number") {
            assert!(shows(harness.window, id, cx), "`{id}` is not on screen");
        }
    }
    assert_eq!(
        hint("new-chat").unwrap().as_ref(),
        format!("New chat  {}", keys::keys_label(Command::NewChat).unwrap())
    );
    type_text(harness.window, "x", cx);
    assert!(shows(harness.window, "send", cx));

    // With no chat open, the start screen says where everything is.
    press(harness.window, "backspace", cx);
    press(harness.window, "ctrl-w", cx);
    assert!(shows(harness.window, "keys-line", cx));
    let line = keys_line();
    assert!(
        line.contains(&keys::keys_label(Command::Palette).unwrap())
            && line.contains(&keys::keys_label(Command::Shortcuts).unwrap()),
        "{line}"
    );
}

#[gpui_kit::test]
fn every_command_row_shows_its_keys_and_is_found_by_them(cx: &mut TestAppContext) {
    let harness = open_one(cx);
    focus_composer(&harness, cx);
    press(harness.window, "ctrl-shift-p", cx);
    // Every command with keys has them in a chip on its row, written as
    // the registry writes them; one without has no chip at all.
    let commands: Vec<(usize, Command)> = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .palette
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item {
                Item::Command(command) => Some((index, *command)),
                _ => None,
            })
            .collect()
    });
    let (mut with, mut without) = (0, 0);
    for (index, command) in commands {
        let chip: &'static str = Box::leak(format!("palette-keys-{index}").into_boxed_str());
        // Only the rows in view are drawn: what is drawn is checked.
        let row: &'static str = Box::leak(format!("palette-item-{index}").into_boxed_str());
        if !shows(harness.window, row, cx) {
            continue;
        }
        match keys::keys_label(command) {
            Some(_) => {
                assert!(shows(harness.window, chip, cx), "{command:?} has no chip");
                with += 1;
            }
            None => {
                assert!(
                    !shows(harness.window, chip, cx),
                    "{command:?} has an empty chip"
                );
                without += 1;
            }
        }
    }
    assert!(
        with > 10 && without > 10,
        "{with} with keys, {without} without"
    );

    // Typing the keys finds the command: as this platform writes them,
    // with a dash, spelled out, or as a Mac writes them.
    for (typed, command) in [
        ("ctrl+n", "New chat"),
        ("ctrl-shift-t", "Pin or unpin the chat"),
        ("Ctrl + ,", "Settings"),
        ("⌘N", "New chat"),
        ("cmd+shift+n", "New group"),
    ] {
        press(harness.window, "ctrl-a", cx);
        type_text(harness.window, &format!(">{typed}"), cx);
        let rows = palette_rows(&harness, cx);
        assert_eq!(
            rows.first().map(|(_, title)| title.as_str()),
            Some(command),
            "`{typed}` (the field holds `{}`): {rows:?}",
            self::typed(&harness, cx)
        );
    }
    press(harness.window, "escape", cx);

    // Somewhere to go says what Enter does, on the row the keyboard is on.
    press(harness.window, "ctrl-p", cx);
    let at = cx.update(|cx| harness.shell.read(cx).palette.cursor);
    let (hint, row) = (
        bounds(harness.window, "palette-enter", cx),
        bounds_of(harness.window, &format!("palette-item-{at}"), cx),
    );
    assert!(within(hint, row));
    // The footer has the keys of the mode it is in.
    assert!(shows(harness.window, "palette-hints", cx));
}

#[gpui_kit::test]
fn the_tip_that_points_at_the_palette_shows_until_it_is_dismissed(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| {
        prepare(cx, Some(file.clone()));
        // A first run: nobody has seen it.
        settings::update(cx, |settings| settings.palette_tip_done = false);
    });
    let harness = open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let tip = bounds(harness.window, "palette-tip", cx);
    let composer = bounds(harness.window, "composer", cx);
    assert!(within(tip, composer), "{tip:?} in {composer:?}");
    assert!(tip.bottom() <= bounds(harness.window, "composer-field", cx).top());
    assert!(within(
        bounds(harness.window, "palette-tip-dismiss", cx),
        tip
    ));
    // Dismissed: gone, and it stays gone.
    click(harness.window, "palette-tip-dismiss", cx);
    assert!(!shows(harness.window, "palette-tip", cx));
    assert!(cx.update(|cx| settings::get(cx).palette_tip_done));
    assert!(
        Settings::load(&file).palette_tip_done,
        "kept with the settings"
    );

    // Opening the palette is as good as dismissing it.
    cx.update(|cx| settings::update(cx, |settings| settings.palette_tip_done = false));
    cx.run_until_parked();
    assert!(shows(harness.window, "palette-tip", cx));
    press(harness.window, "ctrl-p", cx);
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "palette-tip", cx));
    assert!(Settings::load(&file).palette_tip_done);
}

#[test]
fn live_words_all_seven() {
    use client_provider::{LiveUpdates::*, PollingReason::*};
    assert_eq!(live_words(Stream), "Stream");
    assert_eq!(live_words(Reconnecting), "Stream, reconnecting");
    assert_eq!(live_words(Polling(Chosen)), "Polling");
    assert_eq!(
        live_words(Polling(Unavailable)),
        "Polling (the stream is not available yet)"
    );
    assert_eq!(
        live_words(Polling(ConnectionLimit)),
        "Polling (too many stream connections are open)"
    );
    assert_eq!(
        live_words(Polling(Refused)),
        "Polling (the stream refused this key)"
    );
    assert_eq!(
        live_words(Polling(Failing)),
        "Polling (the stream keeps failing)"
    );
}

#[test]
fn diagnostics_shows_live_updates_after_provider() {
    use client_provider::LiveUpdates;
    let mut lines: Vec<(gpui_kit::SharedString, gpui_kit::SharedString)> = vec![
        ("Sending files".into(), "Available".into()),
        ("Provider".into(), "wuapi".into()),
    ];
    push_live_line(&mut lines, Some(LiveUpdates::Reconnecting));
    let lines: Vec<(String, String)> = lines
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    assert_eq!(
        lines,
        [
            ("Sending files".to_owned(), "Available".to_owned()),
            ("Provider".to_owned(), "wuapi".to_owned()),
            ("Live updates".to_owned(), "Stream, reconnecting".to_owned()),
        ]
    );
}

#[gpui_kit::test]
fn diagnostics_omits_line_when_none(cx: &mut TestAppContext) {
    // The mock has no choice of transport: its panel has no such line, and
    // keeps the one that names it.
    let harness = open_one(cx);
    run(&harness, cx, "Check the connection");
    cx.update(|cx| {
        let lines: Vec<String> = harness
            .shell
            .read(cx)
            .palette
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Line(name, value) => Some(format!("{name}: {value}")),
                _ => None,
            })
            .collect();
        assert!(
            lines.iter().any(|line| line.starts_with("Provider: ")),
            "{lines:?}"
        );
        assert!(
            !lines.iter().any(|line| line.starts_with("Live updates")),
            "{lines:?}"
        );
    });
}
