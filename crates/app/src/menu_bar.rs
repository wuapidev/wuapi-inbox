//! The menu bar of the application, which macOS draws at the top of the
//! screen: the application's own menu, Edit and Window.
//!
//! An entry is a GPUI action. Those that are commands of the registry
//! ([`command`]) are run by the window that has the keyboard, through the
//! same path as their keys; the keys the bar shows next to them are the
//! registry's ([`key_bindings`]), so the two cannot drift apart. Edit is
//! the text fields' own actions, and the rest is the platform's.
//!
//! Linux and Windows have no bar of this kind: [`install`] does nothing
//! there.

use crate::keys::{self, Command};
use crate::product::PRODUCT_NAME;
use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
use gpui_kit::{
    actions, Action, App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, Window,
};

actions!(
    wuapi_inbox,
    [
        /// Settings, on its About section.
        About,
        /// Settings.
        OpenSettings,
        /// Asks the update server now.
        CheckForUpdates,
        /// Hides the application's windows.
        Hide,
        /// Hides every other application.
        HideOthers,
        /// Shows the applications that were hidden.
        ShowAll,
        /// Ends the application.
        Quit,
        /// Puts the window in the Dock.
        Minimize,
        /// Makes the window as large as its content asks, and back.
        Zoom,
    ]
);

/// The command of the registry a menu entry runs; `None` for the entries
/// that are the platform's.
pub fn command(action: &dyn Action) -> Option<Command> {
    let is = |other: &dyn Action| action.partial_eq(other);
    if is(&About) {
        Some(Command::SettingsAbout)
    } else if is(&OpenSettings) {
        Some(Command::Settings)
    } else if is(&CheckForUpdates) {
        Some(Command::CheckForUpdates)
    } else if is(&Quit) {
        Some(Command::Quit)
    } else {
        None
    }
}

/// The menus, in the order of the bar.
pub fn menus() -> Vec<Menu> {
    vec![
        // macOS titles the first menu with the application's name.
        Menu::new(PRODUCT_NAME).items([
            MenuItem::action(format!("About {PRODUCT_NAME}"), About),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::action("Check for Updates…", CheckForUpdates),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {PRODUCT_NAME}"), Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {PRODUCT_NAME}"), Quit),
        ]),
        // The text fields' own actions: an entry acts on the field that
        // has the keyboard, and is greyed out when none has.
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        // By this name macOS adds its own entries, and the open windows.
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
    ]
}

/// The keys the platform gives its own entries on every Mac.
const PLATFORM_KEYS: [&str; 3] = ["cmd-h", "alt-cmd-h", "cmd-m"];

/// The keys of the bar's entries. A command's are the registry's; the
/// platform's entries have the keys every Mac application gives them.
pub fn key_bindings() -> Vec<KeyBinding> {
    let [hide, hide_others, minimize] = PLATFORM_KEYS;
    let mut bindings = vec![
        KeyBinding::new(hide, Hide, None),
        KeyBinding::new(hide_others, HideOthers, None),
        KeyBinding::new(minimize, Minimize, None),
    ];
    let commands: [Box<dyn Action>; 4] = [
        Box::new(About),
        Box::new(OpenSettings),
        Box::new(CheckForUpdates),
        Box::new(Quit),
    ];
    for action in commands {
        let chord = command(action.as_ref())
            .and_then(keys::binding)
            .and_then(|binding| binding.chords.first());
        if let Some(chord) = chord {
            bindings.push(
                KeyBinding::load(
                    &chord.keystroke(),
                    action,
                    None,
                    false,
                    None,
                    &gpui_kit::DummyKeyboardMapper,
                )
                .expect("a chord of the registry is a keystroke"),
            );
        }
    }
    bindings
}

/// Runs `act` on the window that has the keyboard.
fn in_active_window(cx: &mut App, act: impl FnOnce(&mut Window)) {
    if let Some(window) = cx.active_window() {
        let _ = window.update(cx, |_, window, _| act(window));
    }
}

/// Gives the application its menu bar, on macOS. What does not need a
/// window is answered here; the commands are answered by the window on
/// screen, when it can run them (`Shell::menu_bar_entry`).
pub fn install(cx: &mut App) {
    if !cfg!(target_os = "macos") {
        return;
    }
    // Quitting works whatever is on screen: the sign-in, a question half
    // answered, no window at all.
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| in_active_window(cx, |window| window.minimize_window()));
    cx.on_action(|_: &Zoom, cx| in_active_window(cx, |window| window.zoom_window()));
    // The keys first: the bar prints the ones that are bound.
    cx.bind_keys(key_bindings());
    cx.set_menus(menus());
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::Keystroke;

    /// The entries that run something, by name, of every menu.
    fn entries() -> Vec<(String, String, Box<dyn Action>)> {
        let mut found = Vec::new();
        for menu in menus() {
            for item in menu.items {
                if let MenuItem::Action { name, action, .. } = item {
                    found.push((menu.name.to_string(), name.to_string(), action));
                }
            }
        }
        found
    }

    fn named(menu: &str) -> Vec<String> {
        entries()
            .into_iter()
            .filter(|(of, ..)| of == menu)
            .map(|(_, name, _)| name)
            .collect()
    }

    #[test]
    fn the_bar_has_the_menus_a_mac_application_has() {
        let names: Vec<String> = menus().iter().map(|menu| menu.name.to_string()).collect();
        assert_eq!(names, [PRODUCT_NAME, "Edit", "Window"]);
        assert!(menus().iter().all(|menu| !menu.items.is_empty()));
        assert_eq!(
            named(PRODUCT_NAME),
            [
                format!("About {PRODUCT_NAME}"),
                "Settings…".to_owned(),
                "Check for Updates…".to_owned(),
                format!("Hide {PRODUCT_NAME}"),
                "Hide Others".to_owned(),
                "Show All".to_owned(),
                format!("Quit {PRODUCT_NAME}"),
            ]
        );
        assert_eq!(
            named("Edit"),
            ["Undo", "Redo", "Cut", "Copy", "Paste", "Select All"]
        );
        assert_eq!(named("Window"), ["Minimize", "Zoom"]);
    }

    #[test]
    fn quit_is_the_last_entry_of_the_first_menu_and_runs_the_command() {
        let first = menus().remove(0);
        let Some(MenuItem::Action { action, .. }) = first.items.last() else {
            panic!("the application's menu ends with an entry");
        };
        assert!(action.partial_eq(&Quit));
        assert_eq!(command(action.as_ref()), Some(Command::Quit));
    }

    #[test]
    fn the_entries_that_are_commands_are_in_the_registry() {
        let commands: Vec<Command> = entries()
            .iter()
            .filter_map(|(.., action)| command(action.as_ref()))
            .collect();
        assert_eq!(
            commands,
            [
                Command::SettingsAbout,
                Command::Settings,
                Command::CheckForUpdates,
                Command::Quit
            ]
        );
        for command in commands {
            assert!(keys::binding(command).is_some(), "{command:?} is not bound");
        }
        // The text fields' and the platform's are not commands.
        assert_eq!(command(&Undo), None);
        assert_eq!(command(&Minimize), None);
    }

    #[test]
    fn the_bar_shows_the_keys_of_the_registry() {
        let bound = |action: &dyn Action| -> Vec<String> {
            key_bindings()
                .iter()
                .filter(|binding| binding.action().partial_eq(action))
                .flat_map(|binding| binding.keystrokes().to_vec())
                .map(|stroke| stroke.inner().unparse())
                .collect()
        };
        let registry = |command: Command| -> Vec<String> {
            keys::binding(command)
                .and_then(|binding| binding.chords.first())
                .map(|chord| {
                    Keystroke::parse(&chord.keystroke())
                        .expect("a keystroke")
                        .unparse()
                })
                .into_iter()
                .collect()
        };
        assert_eq!(bound(&Quit), registry(Command::Quit));
        assert_eq!(bound(&OpenSettings), registry(Command::Settings));
        // No keys in the registry, none in the bar.
        assert!(bound(&About).is_empty());
        assert!(bound(&CheckForUpdates).is_empty());
        if cfg!(target_os = "macos") {
            assert_eq!(bound(&Quit), ["cmd-q"]);
            assert_eq!(bound(&OpenSettings), ["cmd-,"]);
        }
    }

    #[test]
    fn the_keys_of_the_platform_are_nobodys_in_the_registry() {
        // Cmd+H, Cmd+Alt+H and Cmd+M reach the bar before the window: a
        // command on one of them would never run.
        if !cfg!(target_os = "macos") {
            return;
        }
        let everywhere = keys::Context {
            chat: true,
            message: true,
            viewer: true,
            list: true,
            rail: true,
            recording: true,
            attaching: true,
            status: true,
            story: true,
            typing: false,
        };
        for key in PLATFORM_KEYS {
            let stroke = Keystroke::parse(key).expect("a keystroke");
            for context in [keys::Context::default(), everywhere] {
                assert_eq!(keys::resolve(&stroke, context), None, "{key} is taken");
            }
        }
    }
}
