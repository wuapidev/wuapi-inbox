//! Updates in the window: the About section, the notice in the rail and
//! the two commands of the palette. The updater itself is played by a
//! recorder: nothing is downloaded and nothing restarts.

use super::keyboard::palette_rows;
use super::*;
use crate::update::tests::{snapshot_of, Recorded};
use crate::update::{self, Center, UrlFrom};
use std::rc::Rc;
use updater::{Manual, Phase, Version, WhyNot};

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

fn ready() -> Phase {
    Phase::Ready {
        version: version("9.1.0"),
        notes: "- Sends are faster.\n- A crash on start is fixed.".into(),
    }
}

/// Gives the application an updater that says `phase` and records what
/// it is asked.
fn with_updater(cx: &mut TestAppContext, phase: Phase) -> Rc<Recorded> {
    let recorded = Rc::new(Recorded::default());
    let actions = recorded.clone();
    cx.update(|cx| {
        update::install(
            cx,
            Center {
                snapshot: snapshot_of(phase),
                base_url: updater::DEFAULT_BASE_URL.to_owned(),
                url_from: UrlFrom::Default,
                saved_file: None,
                actions,
                relocation: None,
            },
        )
    });
    recorded
}

/// Makes this copy one that could move to the Applications folder. The
/// move is played by `outcome`; how often it was asked for is counted.
fn movable(
    cx: &mut TestAppContext,
    outcome: Result<(), String>,
) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = asked.clone();
    cx.update(|cx| {
        cx.global_mut::<Center>().relocation = Some(update::Move {
            folder: "/Applications".into(),
            run: std::sync::Arc::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                outcome.clone()
            }),
        });
    });
    asked
}

fn from_the_image() -> Phase {
    Phase::Available {
        version: version("9.1.0"),
        notes: String::new(),
        why: Manual::Install(WhyNot::ReadOnly),
    }
}

/// What the updater says changes, as when a check ends.
fn says(cx: &mut TestAppContext, change: impl FnOnce(&mut updater::Snapshot)) {
    cx.update(|cx| {
        change(&mut cx.global_mut::<Center>().snapshot);
        cx.refresh_windows();
    });
    cx.run_until_parked();
}

fn calls(recorded: &Recorded) -> Vec<String> {
    recorded.calls.borrow().clone()
}

fn overlay(harness: &Harness, cx: &mut TestAppContext) -> Overlay {
    cx.update(|cx| harness.shell.read(cx).overlay)
}

fn on_about(harness: &Harness, cx: &mut TestAppContext) -> bool {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell.overlay == Overlay::Settings
            && shell.settings_section == crate::ui::shell::SettingsSection::About
    })
}

fn open_one(cx: &mut TestAppContext) -> Harness {
    open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    )
}

#[gpui_kit::test]
fn about_says_the_version_the_channel_and_when_it_last_checked(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let recorded = with_updater(cx, Phase::UpToDate);
    let harness = open_prepared(cx, ShellOptions::default());

    // Nothing is waiting: the rail says nothing.
    assert!(!shows(harness.window, "update-ready", cx));
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    for selector in [
        "about-channel",
        "about-last-check",
        "update-status",
        "update-check",
        "update-automatic",
    ] {
        assert!(shows(harness.window, selector, cx), "{selector}");
    }
    assert!(!shows(harness.window, "update-restart", cx));
    assert!(!shows(harness.window, "update-notes", cx));
    assert!(!shows(harness.window, "update-download", cx));
    cx.update(|cx| {
        let snapshot = update::snapshot(cx);
        assert_eq!(update::status_line(&snapshot), "Wuapi is up to date.");
        assert_eq!(update::last_checked(&snapshot, 100), "Never");
        assert_eq!(update::CHANNEL.name(), "stable");
    });

    // "Check now" asks, and asks nothing while a check is under way.
    click(harness.window, "update-check", cx);
    assert_eq!(calls(&recorded), ["check"]);
    says(cx, |snapshot| snapshot.phase = Phase::Checking);
    click(harness.window, "update-check", cx);
    assert_eq!(calls(&recorded), ["check"], "one check at a time");
    says(cx, |snapshot| {
        snapshot.phase = Phase::UpToDate;
        snapshot.last_check = Some(updater::stage::now());
    });
    cx.update(|cx| {
        let snapshot = update::snapshot(cx);
        assert_eq!(
            update::last_checked(&snapshot, updater::stage::now()),
            "Just now"
        );
    });

    // The switch is the setting, and the updater hears it.
    assert!(cx.update(|cx| settings::get(cx).auto_update));
    click(harness.window, "update-automatic", cx);
    assert!(!cx.update(|cx| settings::get(cx).auto_update));
    assert_eq!(calls(&recorded), ["check", "automatic:false"]);
    assert!(!cx.update(|cx| update::snapshot(cx).automatic));
    click(harness.window, "update-automatic", cx);
    assert!(cx.update(|cx| settings::get(cx).auto_update));
}

#[gpui_kit::test]
fn a_downloaded_update_is_a_quiet_notice_and_a_restart_away(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let recorded = with_updater(cx, Phase::UpToDate);
    let harness = open_prepared(cx, ShellOptions::default());
    assert!(!shows(harness.window, "update-ready", cx));
    assert_eq!(overlay(&harness, cx), Overlay::None);

    // While it downloads, nothing shows outside About.
    says(cx, |snapshot| {
        snapshot.phase = Phase::Downloading {
            version: version("9.1.0"),
            received: 1,
            total: 4,
        }
    });
    assert!(!shows(harness.window, "update-ready", cx));

    // Ready: a notice with the rail's tools, and nothing opens by itself.
    says(cx, |snapshot| snapshot.phase = ready());
    assert_eq!(
        overlay(&harness, cx),
        Overlay::None,
        "nothing opens on its own"
    );
    let (tools, notice) = (
        bounds(harness.window, "rail-tools", cx),
        bounds(harness.window, "update-ready", cx),
    );
    assert!(within(notice, tools));
    assert!(notice.bottom() <= bounds(harness.window, "toggle-theme", cx).top());
    // The same size as the buttons under it: nothing in the rail moved
    // sideways for it.
    assert_eq!(notice.size, bounds(harness.window, "settings", cx).size);

    // A click shows the version and what is new, with the way to restart.
    click(harness.window, "update-ready", cx);
    assert!(on_about(&harness, cx));
    assert!(shows(harness.window, "update-notes", cx));
    assert!(shows(harness.window, "update-restart", cx));
    assert!(!shows(harness.window, "update-check", cx));
    cx.update(|cx| {
        let line = update::status_line(&update::snapshot(cx));
        assert!(
            line.contains("9.1.0") && line.contains("restarts"),
            "{line}"
        );
    });
    assert!(calls(&recorded).is_empty(), "looking restarts nothing");

    // With nothing under way, "Restart now" restarts.
    click(harness.window, "update-restart", cx);
    assert_eq!(calls(&recorded), ["restart"]);
    assert!(!shows(harness.window, "update-restart-warning", cx));
}

#[gpui_kit::test]
fn restarting_with_messages_on_their_way_out_asks_first(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let recorded = with_updater(cx, ready());
    let harness = open_one(cx);

    // A message that has not left yet (the engine's loops are not running).
    let composer = cx.update(|cx| harness.shell.read(cx).composer.clone());
    let field: ElementId = ("input", composer.entity_id()).into();
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx);
        window.input("see you at 8", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(harness.engine.store().outbox_pending().unwrap().len(), 1);
    let interrupts = cx
        .update(|cx| harness.shell.read(cx).restart_interrupts())
        .expect("a send is under way");
    assert!(interrupts.contains("1 message"), "{interrupts}");
    assert!(interrupts.contains("kept"), "{interrupts}");

    click(harness.window, "update-ready", cx);
    click(harness.window, "update-restart", cx);
    assert!(calls(&recorded).is_empty(), "not without asking");
    assert!(shows(harness.window, "update-restart-warning", cx));
    // The same button, now saying what it will do.
    click(harness.window, "update-restart", cx);
    assert_eq!(calls(&recorded), ["restart"]);
    // The message is still queued: a restart loses nothing of the outbox.
    assert_eq!(harness.engine.store().outbox_pending().unwrap().len(), 1);
}

#[gpui_kit::test]
fn the_palette_checks_for_updates_and_restarts_to_update(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let recorded = with_updater(cx, Phase::UpToDate);
    let harness = open_one(cx);
    let run = |cx: &mut TestAppContext, name: &str| {
        press(harness.window, "ctrl-shift-p", cx);
        type_text(harness.window, name, cx);
        let rows = palette_rows(&harness, cx);
        press(harness.window, "enter", cx);
        rows
    };

    // "Check for updates" asks and shows where the answer will be.
    let rows = run(cx, "Check for updates");
    assert_eq!(
        rows.first().map(|(kind, title)| (*kind, title.as_str())),
        Some(("command", "Check for updates")),
        "{rows:?}"
    );
    assert_eq!(calls(&recorded), ["check"]);
    assert!(on_about(&harness, cx));
    press(harness.window, "escape", cx);

    // With no update waiting, "Restart to update" is there dimmed, with
    // the reason, and does nothing.
    let rows = run(cx, "Restart to update");
    assert_eq!(
        rows.first().map(|(kind, title)| (*kind, title.as_str())),
        Some(("unavailable", "Restart to update")),
        "{rows:?}"
    );
    assert_eq!(calls(&recorded), ["check"]);
    press(harness.window, "escape", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(
            shell.palette_avail(crate::keys::Command::RestartToUpdate, cx),
            crate::ui::palette_steps::Avail::Disabled("No update is waiting")
        );
    });

    // With one waiting, it restarts.
    says(cx, |snapshot| snapshot.phase = ready());
    let rows = run(cx, "Restart to update");
    assert_eq!(
        rows.first().map(|(kind, title)| (*kind, title.as_str())),
        Some(("command", "Restart to update")),
        "{rows:?}"
    );
    assert_eq!(calls(&recorded), ["check", "restart"]);
}

#[gpui_kit::test]
fn without_an_update_key_the_window_says_updates_are_off(cx: &mut TestAppContext) {
    // No updater at all: what a build with the placeholder key shows.
    let harness = open(cx, ShellOptions::default());
    assert!(!shows(harness.window, "update-ready", cx));
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    assert!(shows(harness.window, "update-status", cx));
    assert!(!shows(harness.window, "update-check", cx));
    assert!(!shows(harness.window, "update-automatic", cx));
    assert!(!shows(harness.window, "update-advanced", cx));
    cx.update(|cx| {
        let line = update::status_line(&update::snapshot(cx));
        assert!(line.starts_with("Updates are off"), "{line}");
        let shell = harness.shell.read(cx);
        for command in [
            crate::keys::Command::CheckForUpdates,
            crate::keys::Command::RestartToUpdate,
        ] {
            assert_eq!(
                shell.palette_avail(command, cx),
                crate::ui::palette_steps::Avail::Disabled("Updates are off in this build")
            );
        }
    });
    // The rest of About is as it was.
    click(harness.window, "open-repository", cx);
    assert_eq!(cx.opened_url().as_deref(), Some(crate::product::REPOSITORY));
}

#[gpui_kit::test]
fn an_install_that_does_not_replace_itself_gets_a_link(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let recorded = with_updater(
        cx,
        Phase::Available {
            version: version("9.1.0"),
            notes: "- Sends are faster.".into(),
            why: Manual::Install(WhyNot::Packaged),
        },
    );
    let harness = open_prepared(cx, ShellOptions::default());
    // Nothing to restart for: the rail says nothing.
    assert!(!shows(harness.window, "update-ready", cx));
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    assert!(shows(harness.window, "update-notes", cx));
    assert!(!shows(harness.window, "update-restart", cx));
    cx.update(|cx| {
        let line = update::status_line(&update::snapshot(cx));
        assert!(
            line.contains("9.1.0") && line.contains("package manager"),
            "{line}"
        );
    });
    click(harness.window, "update-download", cx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some(update::download_page().as_str())
    );
    assert!(calls(&recorded).is_empty());

    // A release without a build for this system has nothing to download.
    says(cx, |snapshot| {
        snapshot.phase = Phase::Available {
            version: version("9.1.0"),
            notes: String::new(),
            why: Manual::NoBuild,
        }
    });
    assert!(!shows(harness.window, "update-download", cx));
}

#[gpui_kit::test]
fn an_update_that_was_undone_is_said_once(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let recorded = with_updater(cx, Phase::UpToDate);
    says(cx, |snapshot| {
        snapshot.notice = Some("Version 9.1.0 did not start, so version 9.0.0 was put back.".into())
    });
    let harness = open_prepared(cx, ShellOptions::default());
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    assert!(shows(harness.window, "update-notice", cx));
    click(harness.window, "update-notice-ok", cx);
    assert!(!shows(harness.window, "update-notice", cx));
    assert_eq!(calls(&recorded), ["dismiss"]);
}

#[gpui_kit::test]
fn another_update_source_is_checked_before_it_is_saved(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(update::FILE_NAME);
    cx.update(|cx| prepare(cx, None));
    let recorded = with_updater(cx, Phase::UpToDate);
    cx.update(|cx| cx.global_mut::<Center>().saved_file = Some(file.clone()));
    let harness = open_prepared(cx, ShellOptions::default());
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);

    // Folded away until asked for.
    assert!(!shows(harness.window, "update-url", cx));
    click(harness.window, "update-advanced", cx);
    assert!(shows(harness.window, "update-url", cx));
    assert!(!shows(harness.window, "update-url-reset", cx));

    let field = cx.update(|cx| harness.shell.read(cx).updates.url_input.clone());
    let said = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .updates
                .said
                .clone()
                .unwrap_or_default()
                .to_string()
        })
    };
    let write = |cx: &mut TestAppContext, text: &'static str| {
        cx.update_window(harness.window.into(), |_, window, cx| {
            field.update(cx, |field, cx| field.set_value(text, window, cx));
        })
        .unwrap();
        cx.run_until_parked();
    };

    // Plain http to somewhere else is refused, and nothing changes.
    write(cx, "http://mirror.example.com/inbox");
    click(harness.window, "update-url-save", cx);
    assert!(
        said(cx).starts_with("Not saved: only https"),
        "{}",
        said(cx)
    );
    assert!(calls(&recorded).is_empty());
    assert!(!file.exists());
    assert_eq!(
        cx.update(|cx| update::base_url(cx)),
        (updater::DEFAULT_BASE_URL.to_owned(), UrlFrom::Default)
    );

    // An https mirror is taken, saved, and asked at once.
    write(cx, "https://mirror.example.com/inbox");
    click(harness.window, "update-url-save", cx);
    assert!(said(cx).starts_with("Saved"), "{}", said(cx));
    assert_eq!(
        calls(&recorded),
        ["url:https://mirror.example.com/inbox", "check"]
    );
    assert_eq!(
        cx.update(|cx| update::base_url(cx)),
        (
            "https://mirror.example.com/inbox".to_owned(),
            UrlFrom::Setting
        )
    );
    let saved = std::fs::read_to_string(&file).unwrap();
    assert!(
        saved.contains("https://mirror.example.com/inbox"),
        "{saved}"
    );

    // "Reset" goes back to the project's releases, and the file goes.
    click(harness.window, "update-url-reset", cx);
    assert_eq!(
        cx.update(|cx| update::base_url(cx)),
        (updater::DEFAULT_BASE_URL.to_owned(), UrlFrom::Default)
    );
    assert!(!file.exists());
    assert_eq!(
        calls(&recorded).last().map(String::as_str),
        Some("check"),
        "the default is asked at once too"
    );
    assert!(!shows(harness.window, "update-url-reset", cx));
}

#[gpui_kit::test]
fn a_source_given_on_the_command_line_is_shown_and_not_changed(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    with_updater(cx, Phase::UpToDate);
    cx.update(|cx| {
        let center = cx.global_mut::<Center>();
        center.base_url = "http://127.0.0.1:8000".into();
        center.url_from = UrlFrom::CommandLine;
    });
    let harness = open_prepared(cx, ShellOptions::default());
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    // Unfolded from the start: it is not the default.
    assert!(shows(harness.window, "update-url", cx));
    assert!(shows(harness.window, "update-url-state", cx));
    assert!(!shows(harness.window, "update-url-save", cx));
    assert!(!shows(harness.window, "update-url-reset", cx));
    let field = cx.update(|cx| harness.shell.read(cx).updates.url_input.clone());
    assert_eq!(
        cx.update(|cx| field.read(cx).value().to_string()),
        "http://127.0.0.1:8000"
    );
}

#[gpui_kit::test]
fn a_copy_that_could_update_itself_elsewhere_is_asked_once_to_move(cx: &mut TestAppContext) {
    use std::sync::atomic::Ordering;
    cx.update(|cx| prepare(cx, None));
    with_updater(cx, Phase::UpToDate);
    let asked = movable(cx, Ok(()));
    let harness = open_prepared(cx, ShellOptions::default());

    // Asked at the start, with nothing new out: the point is the update
    // that will come.
    assert!(shows(harness.window, "move-prompt", cx));
    assert!(shows(harness.window, "move-accept", cx));
    click(harness.window, "move-not-now", cx);
    assert!(!shows(harness.window, "move-prompt", cx));
    assert_eq!(asked.load(Ordering::SeqCst), 0);
    cx.update(|cx| assert!(settings::get(cx).move_prompt_done));

    // "Not now" is not "never": About keeps the way there.
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    assert!(shows(harness.window, "update-move", cx));
    click(harness.window, "update-move", cx);
    cx.run_until_parked();
    assert_eq!(asked.load(Ordering::SeqCst), 1);
}

#[gpui_kit::test]
fn the_prompt_is_not_shown_again_and_not_to_a_copy_that_has_no_use_for_it(cx: &mut TestAppContext) {
    // Said "not now" in an earlier session.
    cx.update(|cx| prepare(cx, None));
    with_updater(cx, Phase::UpToDate);
    movable(cx, Ok(()));
    cx.update(|cx| settings::update(cx, |settings| settings.move_prompt_done = true));
    let harness = open_prepared(cx, ShellOptions::default());
    assert!(!shows(harness.window, "move-prompt", cx));
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    assert!(shows(harness.window, "update-move", cx));
}

#[gpui_kit::test]
fn a_copy_that_updates_itself_is_never_asked_to_move_or_sent_to_download(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    with_updater(cx, ready());
    let harness = open_prepared(cx, ShellOptions::default());
    assert!(!shows(harness.window, "move-prompt", cx));
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    // A restart is the one step there is.
    assert!(shows(harness.window, "update-restart", cx));
    assert!(!shows(harness.window, "update-move", cx));
    assert!(!shows(harness.window, "update-download", cx));
    for phase in [
        Phase::UpToDate,
        Phase::Checking,
        Phase::Downloading {
            version: version("9.1.0"),
            received: 1,
            total: 2,
        },
    ] {
        says(cx, |snapshot| snapshot.phase = phase);
        assert!(!shows(harness.window, "update-download", cx));
        assert!(!shows(harness.window, "update-move", cx));
    }
}

#[gpui_kit::test]
fn a_new_version_for_a_copy_that_can_move_offers_the_move_not_the_download_page(
    cx: &mut TestAppContext,
) {
    use std::sync::atomic::Ordering;
    cx.update(|cx| prepare(cx, None));
    with_updater(cx, from_the_image());
    let asked = movable(cx, Err("the disk is full".into()));
    let harness = open_prepared(cx, ShellOptions::default());
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    assert!(shows(harness.window, "update-move", cx));
    assert!(!shows(harness.window, "update-download", cx));

    // A move that fails says why, where it was asked for and in the
    // prompt, and can be asked for again.
    assert!(!shows(harness.window, "update-move-failed", cx));
    click(harness.window, "update-move", cx);
    cx.run_until_parked();
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert!(shows(harness.window, "update-move-failed", cx));
    cx.update(|cx| {
        let said = harness.shell.read(cx).move_failure();
        let said = said.expect("the failure is said");
        assert!(said.contains("the disk is full"), "{said}");
        assert!(said.contains("Nothing was changed"), "{said}");
    });
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "move-failed", cx));
    click(harness.window, "move-accept", cx);
    cx.run_until_parked();
    assert_eq!(asked.load(Ordering::SeqCst), 2);

    // A reason a move does not help keeps its link.
    cx.update(|cx| cx.global_mut::<Center>().relocation = None);
    says(cx, |snapshot| {
        snapshot.phase = Phase::Available {
            version: version("9.1.0"),
            notes: String::new(),
            why: Manual::Install(WhyNot::Packaged),
        }
    });
    click(harness.window, "settings", cx);
    click(harness.window, "settings-about", cx);
    assert!(shows(harness.window, "update-download", cx));
    assert!(!shows(harness.window, "update-move", cx));
}
