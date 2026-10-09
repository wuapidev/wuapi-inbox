//! UI tests: the real views in a headless window, driven by real clicks and
//! key presses, on top of the mock provider.

use super::app::{AppView, Screen};
use super::bubble::Row;
use super::shell::{ChatFilter, IdentityState, ListRow, Overlay, SessionKind, Shell, ShellOptions};
use crate::login::tests::{code, grant};
use crate::login::{BoxFuture, Identity, KeySaved, LoginFlow, Phase, Session};
use crate::providers::Launch;
use crate::settings::{self, MotionChoice, Settings, ThemeChoice};
use crate::theme::{self, Appearance};
use client_core::{HistoryMode, Store, SyncConfig, SyncEngine};
use client_provider::{DeliveryStatus, Direction, MessageContent, OutgoingContent};
use gpui_kit::test::TestWindowExt;
use gpui_kit::VisualTestContext;
use gpui_kit::{
    px, size, AppContext as _, Bounds, ElementId, Entity, Modifiers, Point, TestAppContext,
    WindowBounds, WindowHandle, WindowOptions,
};
use provider_mock::MockProvider;
use provider_wuapi::{DeviceCode, LoginError, PollOutcome, TokenGrant};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;

/// What `main` does before the first window: the component library and
/// the settings (kept in memory unless a test gives them a file). Nothing
/// moves, so every element is where it ends up.
fn prepare(cx: &mut gpui_kit::App, settings_file: Option<std::path::PathBuf>) {
    gpui_kit::init(cx);
    settings::init(settings_file, Some(Appearance::Light), cx);
    settings::update(cx, |settings| {
        settings.motion = MotionChoice::Off;
        // The first-run tip is its own test's business.
        settings.palette_tip_done = true;
    });
}

/// [`prepare`] with the settings in memory.
fn prepare_app(cx: &mut gpui_kit::App) {
    prepare(cx, None);
}

/// Clicks the element drawn with this `debug_selector`, in its middle.
fn click(
    window: WindowHandle<gpui_kit::base::Root>,
    selector: &'static str,
    cx: &mut TestAppContext,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("`{selector}` is not on screen"));
    visual.simulate_click(bounds.center(), Modifiers::none());
    visual.run_until_parked();
}

/// Whether an element with this `debug_selector` was drawn.
fn shows(
    window: WindowHandle<gpui_kit::base::Root>,
    selector: &'static str,
    cx: &mut TestAppContext,
) -> bool {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .is_some()
}

/// A key as this platform has it. The tests write the secondary key as
/// `ctrl`, which is what it is on Linux and Windows; on macOS it is Cmd
/// (`keys::Chord`). The few chords that are Control on every platform
/// stay as written.
fn platform_key(key: &str) -> String {
    const CONTROL_EVERYWHERE: [&str; 4] = ["ctrl-tab", "ctrl-shift-tab", "ctrl-left", "ctrl-right"];
    if cfg!(target_os = "macos") && !CONTROL_EVERYWHERE.contains(&key) {
        key.replacen("ctrl-", "cmd-", 1)
    } else {
        key.to_owned()
    }
}

fn press(window: WindowHandle<gpui_kit::base::Root>, key: &str, cx: &mut TestAppContext) {
    let key = platform_key(key);
    let key = key.as_str();
    cx.update_window(window.into(), |_, window, cx| window.press(key, cx))
        .unwrap();
    cx.run_until_parked();
}

fn type_text(window: WindowHandle<gpui_kit::base::Root>, text: &str, cx: &mut TestAppContext) {
    cx.update_window(window.into(), |_, window, cx| window.input(text, cx))
        .unwrap();
    cx.run_until_parked();
}

struct Harness {
    // Dropped last: the engine's background calls run here.
    runtime: Runtime,
    engine: SyncEngine,
    mock: MockProvider,
    window: WindowHandle<gpui_kit::base::Root>,
    shell: Entity<Shell>,
}

/// Opens the main window over a store that already holds the mock's data.
/// The engine's background loops are not started: the test decides when
/// the "network" makes progress.
fn open(cx: &mut TestAppContext, options: ShellOptions) -> Harness {
    cx.update(|cx| prepare(cx, None));
    open_prepared(cx, options)
}

/// As [`open`], for a test that set the application up itself.
fn open_prepared(cx: &mut TestAppContext, options: ShellOptions) -> Harness {
    open_over(cx, options, MockProvider::quiet())
}

/// As [`open_prepared`], over a provider the test made.
fn open_over(cx: &mut TestAppContext, options: ShellOptions, mock: MockProvider) -> Harness {
    open_over_provider(cx, options, mock.clone(), Arc::new(mock))
}

/// As [`open_over`], with the engine talking to `provider`, which wraps
/// `mock` (the world the test reaches into through the harness): a wrapper
/// that says something the mock does not, such as how its updates arrive.
fn open_over_provider(
    cx: &mut TestAppContext,
    options: ShellOptions,
    mock: MockProvider,
    provider: Arc<dyn client_provider::Provider>,
) -> Harness {
    // A current-thread runtime only makes progress inside `block_on`, on
    // this thread. The test therefore decides when background work runs,
    // which GPUI's deterministic test scheduler requires.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        provider,
        // A session that has been used: the newest page of every chat is
        // in the store, as after opening each one.
        SyncConfig {
            history: HistoryMode::Recent(usize::MAX),
            preload_pace: Duration::ZERO,
            ..SyncConfig::default()
        },
        runtime.handle().clone(),
    );
    runtime.block_on(engine.refresh()).unwrap();
    runtime.block_on(engine.preload()).unwrap();

    let (window, shell) = cx.update(|cx| {
        let view_engine = engine.clone();
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1240.), px(800.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Shell::new(view_engine, options, window, cx)),
        )
        .unwrap()
    });
    cx.run_until_parked();
    Harness {
        runtime,
        engine,
        mock,
        window: window.downcast().expect("the window has a base root"),
        shell,
    }
}

impl Harness {
    /// Lets the engine's background tasks run to completion, then lets the
    /// UI react to what they stored.
    fn settle(&self, cx: &mut TestAppContext) {
        self.runtime.block_on(async {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
        });
        cx.run_until_parked();
    }
}

/// The newest message row of the open chat: (text, status, outgoing).
fn newest_message(shell: &Shell) -> Option<(String, DeliveryStatus, bool)> {
    let open = shell.open.as_ref()?;
    open.rows.iter().rev().find_map(|row| match row {
        Row::Message(row) => {
            let message = &row.stored.message;
            let text = match &message.content {
                MessageContent::Text { body } => body.clone(),
                other => format!("{other:?}"),
            };
            Some((
                text,
                message.status.clone(),
                message.direction == Direction::Outgoing,
            ))
        }
        Row::Day(_) => None,
    })
}

#[gpui_kit::test]
fn the_window_shows_the_store_without_any_network(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.accounts.len(), 2);
        let chats = shell
            .list_rows
            .iter()
            .filter(|row| matches!(row, ListRow::Chat(_)))
            .count();
        assert!(chats >= 24, "the chat list is populated, got {chats}");
        assert!(shell.open.is_none(), "no chat is open until one is picked");
        // Each account's unread total feeds its badge on the rail, the
        // one on screen included.
        assert_eq!(shell.unread.len(), 2);
    });
}

#[gpui_kit::test]
fn typing_and_pressing_enter_sends_through_the_outbox(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let composer = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.open.is_some(), "the requested chat opened");
        shell.composer.clone()
    });
    let field: ElementId = ("input", composer.entity_id()).into();

    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx);
        window.input("see you at 8", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();

    // The message is in the outbox and on screen, pending, and the field
    // is empty again. The provider has not been called at all.
    let pending = harness.engine.store().outbox_pending().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].message.content,
        OutgoingContent::Text {
            body: "see you at 8".into()
        }
    );
    assert_eq!(harness.mock.send_calls(), 0);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.composer.read(cx).value().as_ref(), "");
        assert_eq!(
            newest_message(shell),
            Some(("see you at 8".to_owned(), DeliveryStatus::Pending, true))
        );
    });

    // Shift+Enter is a line break, not a send.
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.input("line one", cx);
        window.press("shift-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.composer.read(cx).value().as_ref(), "line one\n");
    });
    assert_eq!(harness.engine.store().outbox_pending().unwrap().len(), 1);

    // The outbox reaches the provider: the same bubble gets its tick.
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 1);
    cx.run_until_parked();
    cx.update(|cx| {
        assert_eq!(
            newest_message(harness.shell.read(cx)),
            Some(("see you at 8".to_owned(), DeliveryStatus::Sent, true))
        );
    });
    assert_eq!(harness.mock.delivered_count(), 1);
}

#[gpui_kit::test]
fn searching_filters_chats_and_finds_messages(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let search = cx.update(|cx| harness.shell.read(cx).search.clone());
    let field: ElementId = ("input", search.entity_id()).into();

    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx);
        window.input("lisbon", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let titles: Vec<_> = shell
            .list_rows
            .iter()
            .filter_map(|row| match row {
                ListRow::Chat(chat) => Some(chat.title.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(titles, ["Lisbon trip ✈️"]);
    });

    // A word that only appears inside messages lists the messages.
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.press(&platform_key("ctrl-a"), cx);
        window.input("dentist", cx);
    })
    .unwrap();
    cx.run_until_parked();
    // The messages are looked for off this thread, a moment after the
    // last key: typing never waits for them.
    assert_eq!(message_hits(&harness, cx), 0);
    wait_for_message_search(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(matches!(
            shell.list_rows.first(),
            Some(ListRow::Section("Messages"))
        ));
        assert!(shell
            .list_rows
            .iter()
            .any(|row| matches!(row, ListRow::Hit(_))));
    });
}

/// How many messages the chat list shows as found.
fn message_hits(harness: &Harness, cx: &mut TestAppContext) -> usize {
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_rows
            .iter()
            .filter(|row| matches!(row, ListRow::Hit(_)))
            .count()
    })
}

/// Lets the chat list's message search start and answer.
fn wait_for_message_search(cx: &mut TestAppContext) {
    cx.executor()
        .advance_clock(crate::motion::SEARCH_DEBOUNCE + Duration::from_millis(10));
    cx.run_until_parked();
}

/// Replaces what the search field holds with `text`.
fn search_for(harness: &Harness, text: &str, cx: &mut TestAppContext) {
    let search = cx.update(|cx| harness.shell.read(cx).search.clone());
    let field: ElementId = ("input", search.entity_id()).into();
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx);
        window.press(&platform_key("ctrl-a"), cx);
        if text.is_empty() {
            window.press("backspace", cx);
        } else {
            window.input(text, cx);
        }
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_newer_search_wins_and_an_emptied_one_clears_the_messages(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());

    // A search replaced before it started never shows what it found.
    search_for(&harness, "dentist", cx);
    search_for(&harness, "zzzzzz", cx);
    wait_for_message_search(cx);
    assert_eq!(message_hits(&harness, cx), 0);

    search_for(&harness, "dentist", cx);
    wait_for_message_search(cx);
    assert!(message_hits(&harness, cx) > 0);

    // Emptying the field takes the messages away at once, and a search
    // still waiting to start does not bring them back.
    search_for(&harness, "dentis", cx);
    search_for(&harness, "", cx);
    assert_eq!(message_hits(&harness, cx), 0);
    wait_for_message_search(cx);
    assert_eq!(message_hits(&harness, cx), 0);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.query.is_empty());
        assert!(shell
            .list_rows
            .iter()
            .any(|row| matches!(row, ListRow::Chat(_))));
    });
}

#[gpui_kit::test]
fn one_letter_filters_the_chats_without_searching_the_messages(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let all_chats = cx.update(|cx| harness.shell.read(cx).list_rows.len());

    // A single letter starts a great share of all the words there are:
    // the titles are filtered by it, the messages are left alone.
    search_for(&harness, "d", cx);
    wait_for_message_search(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(!shell.list_rows.is_empty());
        assert!(shell.list_rows.len() < all_chats);
        assert!(shell.list_rows.iter().all(|row| match row {
            ListRow::Chat(chat) => chat.title.to_lowercase().contains('d'),
            _ => false,
        }));
    });

    // The second letter asks.
    search_for(&harness, "de", cx);
    wait_for_message_search(cx);
    assert!(message_hits(&harness, cx) > 0);
}

#[gpui_kit::test]
fn scrolling_back_pages_in_older_history(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    // The mock's most recently active chat is its very long thread.
    let (account, long_thread) = harness.runtime.block_on(async {
        use client_provider::Provider as _;
        let account = harness.mock.list_accounts().await.unwrap().remove(0).id;
        let chat = harness
            .mock
            .list_chats(&account, None)
            .await
            .unwrap()
            .items
            .remove(0);
        (account, chat.id)
    });
    let message_rows = |shell: &Shell| {
        let open = shell.open.as_ref().expect("a chat is open");
        assert_eq!(
            open.list.item_count(),
            open.rows.len(),
            "the list tracks the rows"
        );
        open.rows
            .iter()
            .filter(|row| matches!(row, Row::Message(_)))
            .count()
    };

    harness.shell.update(cx, |shell, cx| {
        shell.open_chat(long_thread.clone(), None, cx);
    });
    harness.settle(cx);
    let initial = cx.update(|cx| message_rows(harness.shell.read(cx)));
    assert!(
        initial > 0 && initial <= 50,
        "one page is stored at first, got {initial}"
    );

    // Reaching the top asks the engine for the next page.
    harness.shell.update(cx, |shell, cx| shell.load_older(cx));
    harness.settle(cx);
    let after = cx.update(|cx| message_rows(harness.shell.read(cx)));
    assert!(
        after > initial,
        "older messages were added ({initial} -> {after})"
    );
    let stored = harness
        .engine
        .store()
        .message_count(&account, &long_thread)
        .unwrap();
    assert_eq!(after, stored, "everything stored is shown");

    // The newest message is still the last row: history was prepended.
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let open = shell.open.as_ref().unwrap();
        assert!(matches!(open.rows.first(), Some(Row::Day(_))));
        assert!(matches!(open.rows.last(), Some(Row::Message(_))));
    });
}

#[gpui_kit::test]
fn a_long_message_wraps_inside_a_narrow_conversation_pane(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let composer = cx.update(|cx| harness.shell.read(cx).composer.clone());
    let field: ElementId = ("input", composer.entity_id()).into();
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    // Narrow enough that the pane is smaller than a bubble's maximum width.
    visual.simulate_resize(size(px(820.), px(800.)));
    visual.run_until_parked();

    let long = "I think the second option is better. It's cheaper and we don't depend on \
                the venue's schedule, which was the part that worried me the most.";
    visual.update(|window, cx| {
        window.click(field, cx);
        window.input(long, cx);
        window.press("enter", cx);
    });
    visual.run_until_parked();

    let thread = visual
        .debug_bounds("thread")
        .expect("the thread is on screen");
    // The newest bubble is painted last, so it owns the selector.
    let bubble = visual
        .debug_bounds("bubble")
        .expect("a bubble is on screen");
    assert!(
        bubble.left() >= thread.left() && bubble.right() <= thread.right(),
        "the bubble stays inside the pane: bubble {bubble:?}, pane {thread:?}"
    );
    assert!(
        bubble.size.height > px(60.),
        "the text wrapped onto several lines: {bubble:?}"
    );
}

// ----- signing in ---------------------------------------------------------

/// A scripted sign-in: queued answers instead of an API, a session on the
/// mock provider instead of wuapi, and no keychain.
struct FakeFlow {
    codes: Mutex<VecDeque<Result<DeviceCode, LoginError>>>,
    /// Answers to "was it approved?". "Not yet" once they run out.
    answers: Mutex<VecDeque<Result<PollOutcome, LoginError>>>,
    polls: AtomicUsize,
    sign_outs: AtomicUsize,
    /// Times only the key was forgotten (the provider refused it).
    forgotten: AtomicUsize,
    key_saved: KeySaved,
    engine: SyncEngine,
    /// The provider behind `engine`, to make it fail.
    mock: MockProvider,
}

impl FakeFlow {
    fn answer(&self, answer: Result<PollOutcome, LoginError>) {
        self.answers.lock().unwrap().push_back(answer);
    }

    fn polls(&self) -> usize {
        self.polls.load(Ordering::SeqCst)
    }
}

impl LoginFlow for FakeFlow {
    fn request_code(&self) -> BoxFuture<Result<DeviceCode, LoginError>> {
        let next = self.codes.lock().unwrap().pop_front();
        Box::pin(std::future::ready(next.unwrap_or_else(|| Ok(code(600, 5)))))
    }

    fn poll(&self, _: &DeviceCode) -> BoxFuture<Result<PollOutcome, LoginError>> {
        self.polls.fetch_add(1, Ordering::SeqCst);
        let next = self.answers.lock().unwrap().pop_front();
        Box::pin(std::future::ready(next.unwrap_or(Ok(PollOutcome::Pending))))
    }

    fn open_session(&self, grant: TokenGrant) -> BoxFuture<Result<Session, String>> {
        let identity = Identity {
            organization: grant.organization.name,
            project: grant.project.map(|project| project.name),
            key_prefix: grant.key_prefix,
        };
        Box::pin(std::future::ready(Ok(Session {
            engine: self.engine.clone(),
            identity: Some(Arc::new(move || {
                Box::pin(std::future::ready(Ok(identity.clone())))
            })),
            key_saved: self.key_saved.clone(),
            storage_note: None,
        })))
    }

    fn forget_key(&self) -> BoxFuture<Result<(), String>> {
        self.forgotten.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(())))
    }

    fn sign_out(&self) -> BoxFuture<Result<(), String>> {
        self.sign_outs.fetch_add(1, Ordering::SeqCst);
        Box::pin(std::future::ready(Ok(())))
    }
}

struct LoginHarness {
    // Dropped last, as in `Harness`.
    runtime: Runtime,
    flow: Arc<FakeFlow>,
    window: WindowHandle<gpui_kit::base::Root>,
    app: Entity<AppView>,
}

/// Opens the window on the sign-in screen.
fn open_login(
    cx: &mut TestAppContext,
    codes: Vec<Result<DeviceCode, LoginError>>,
    keychain_error: Option<&str>,
    key_saved: KeySaved,
) -> LoginHarness {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mock = MockProvider::quiet();
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(mock.clone()),
        SyncConfig::default(),
        runtime.handle().clone(),
    );
    // What the real flow does before it hands the session over.
    runtime.block_on(engine.refresh()).unwrap();
    let flow = Arc::new(FakeFlow {
        codes: Mutex::new(codes.into()),
        answers: Mutex::default(),
        polls: AtomicUsize::new(0),
        sign_outs: AtomicUsize::new(0),
        forgotten: AtomicUsize::new(0),
        key_saved,
        engine,
        mock,
    });
    let launch = Launch::Login {
        flow: flow.clone(),
        keychain_error: keychain_error.map(str::to_owned),
        storage_note: None,
    };
    let (window, app) = open_app(cx, launch);
    LoginHarness {
        runtime,
        flow,
        window,
        app,
    }
}

fn open_app(
    cx: &mut TestAppContext,
    launch: Launch,
) -> (WindowHandle<gpui_kit::base::Root>, Entity<AppView>) {
    // The welcome has been seen, unless a test is about it.
    cx.update(|cx| {
        if !cx.has_global::<gpui_kit::component::Theme>() {
            prepare(cx, None);
        }
        settings::update(cx, |settings| settings.welcome_done = true);
    });
    open_app_with(cx, launch, false)
}

fn open_app_with(
    cx: &mut TestAppContext,
    launch: Launch,
    force_welcome: bool,
) -> (WindowHandle<gpui_kit::base::Root>, Entity<AppView>) {
    let (window, app) = cx.update(|cx| {
        if !cx.has_global::<gpui_kit::component::Theme>() {
            prepare(cx, None);
        }
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1240.), px(800.)),
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| AppView::new(launch, None, force_welcome, window, cx)),
        )
        .unwrap()
    });
    cx.run_until_parked();
    (window.downcast().expect("the window has a base root"), app)
}

impl LoginHarness {
    /// The sign-in's phase, or `None` once the chats are showing.
    fn phase(&self, cx: &mut TestAppContext) -> Option<Phase> {
        cx.update(|cx| match &self.app.read(cx).screen {
            Screen::Login(screen) => Some(screen.read(cx).machine.phase().clone()),
            _ => None,
        })
    }

    fn click(&self, id: &'static str, cx: &mut TestAppContext) {
        click(self.window, id, cx);
    }

    fn has(&self, id: &'static str, cx: &mut TestAppContext) -> bool {
        shows(self.window, id, cx)
    }

    /// Lets time pass on the executor's clock, which is the screen's.
    fn wait(&self, seconds: u64, cx: &mut TestAppContext) {
        cx.executor().advance_clock(Duration::from_secs(seconds));
        cx.run_until_parked();
    }
}

fn is_waiting(phase: Option<Phase>, lost: bool) -> bool {
    matches!(phase, Some(Phase::Waiting { unreachable, .. }) if unreachable == lost)
}

#[gpui_kit::test]
fn signing_in_goes_from_connect_to_the_chat_list_and_back_out(cx: &mut TestAppContext) {
    let harness = open_login(cx, vec![], None, KeySaved::Yes);
    assert_eq!(harness.phase(cx), Some(Phase::Start));
    assert!(
        !harness.has("keychain-notice", cx) && harness.has("connect", cx),
        "the first screen is the connect action, with nothing to warn about"
    );

    // Connect: the code is on screen, large, inside the form, with the
    // time it has left.
    harness.click("connect", cx);
    assert!(is_waiting(harness.phase(cx), false));
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    let form = visual.debug_bounds("login-form").expect("the form");
    let shown = visual.debug_bounds("user-code").expect("the code");
    assert!(shown.left() >= form.left() && shown.right() <= form.right());
    assert!(shown.size.height >= px(34.), "the code is large: {shown:?}");
    assert!(visual.debug_bounds("countdown").is_some());

    // Nothing opens by itself; the two buttons do what they say.
    assert_eq!(cx.opened_url(), None);
    harness.click("open-browser", cx);
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://wuapi.dev/cli?code=WXYZ-1234")
    );
    harness.click("copy-code", cx);
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()),
        Some("WXYZ-1234".to_owned())
    );

    // The server asked for five seconds between questions.
    assert_eq!(harness.flow.polls(), 0);
    harness.wait(4, cx);
    assert_eq!(harness.flow.polls(), 0);
    harness.wait(1, cx);
    assert_eq!(harness.flow.polls(), 1);
    assert!(is_waiting(harness.phase(cx), false));

    // Approved in the browser: the chats, with the accounts already there.
    harness.flow.answer(Ok(PollOutcome::Granted(grant())));
    harness.wait(5, cx);
    assert_eq!(harness.phase(cx), None, "the chat list took over");
    let shell = cx.update(|cx| match &harness.app.read(cx).screen {
        Screen::Chats(shell) => shell.clone(),
        _ => panic!("still signing in"),
    });
    cx.update(|cx| {
        let shell = shell.read(cx);
        assert_eq!(shell.accounts.len(), 2);
        assert_eq!(shell.session, SessionKind::Keychain);
    });
    // The sign-in is over: nobody is asking the API any more.
    harness.wait(60, cx);
    assert_eq!(harness.flow.polls(), 2);

    // Settings, "Sign out": the key is forgotten and the sign-in is back.
    assert!(!harness.has("sign-out", cx));
    harness.click("settings", cx);
    // Who signed in is there, with the key's prefix and never the key.
    cx.update(|cx| {
        assert_eq!(
            shell.read(cx).identity,
            IdentityState::Known(Identity {
                organization: "Acme Labs".into(),
                project: None,
                key_prefix: Some("wu_live_0123".into()),
            })
        );
    });
    // It deletes things, so it asks first; nothing is waiting to be sent.
    harness.click("sign-out", cx);
    assert!(harness.has("confirm-sign-out", cx) && !harness.has("unsent-warning", cx));
    assert_eq!(harness.flow.sign_outs.load(Ordering::SeqCst), 0);
    harness.click("sign-out-confirm", cx);
    assert_eq!(harness.flow.sign_outs.load(Ordering::SeqCst), 1);
    assert_eq!(harness.flow.forgotten.load(Ordering::SeqCst), 0);
    assert_eq!(harness.phase(cx), Some(Phase::Start));
    assert!(harness.has("connect", cx));
}

#[gpui_kit::test]
fn a_dead_network_never_ends_the_sign_in_but_the_clock_does(cx: &mut TestAppContext) {
    // The code lasts a minute; the server wants a question every 5 s.
    let harness = open_login(cx, vec![Ok(code(60, 5))], None, KeySaved::Yes);
    harness.click("connect", cx);

    // The question does not get through, twice. The code stays up.
    harness
        .flow
        .answer(Ok(PollOutcome::Unreachable("could not connect".into())));
    harness
        .flow
        .answer(Ok(PollOutcome::Unreachable("HTTP 502".into())));
    harness.wait(5, cx);
    assert!(is_waiting(harness.phase(cx), true), "said, not fatal");
    assert!(harness.has("open-browser", cx) && !harness.has("retry", cx));
    harness.wait(5, cx);
    assert!(is_waiting(harness.phase(cx), true));
    assert_eq!(harness.flow.polls(), 2);

    // Back online, and told to slow down: five more seconds per question.
    harness.flow.answer(Ok(PollOutcome::SlowDown));
    harness.wait(5, cx);
    assert!(is_waiting(harness.phase(cx), false));
    assert_eq!(harness.flow.polls(), 3);
    harness.wait(5, cx);
    assert_eq!(harness.flow.polls(), 3, "not yet: the pace is 10 s now");
    harness.wait(5, cx);
    assert_eq!(harness.flow.polls(), 4);

    // Nobody approves. At the minute the code is over, and so is asking.
    harness.wait(40, cx);
    assert_eq!(harness.phase(cx), Some(Phase::Expired));
    let asked = harness.flow.polls();
    harness.wait(120, cx);
    assert_eq!(harness.flow.polls(), asked);

    // A new code is one click away.
    harness.click("retry", cx);
    assert!(is_waiting(harness.phase(cx), false));
}

#[gpui_kit::test]
fn offline_and_denied_offer_to_try_again(cx: &mut TestAppContext) {
    let offline = Err(LoginError::Network("could not connect".into()));
    let harness = open_login(cx, vec![offline], None, KeySaved::Yes);

    // No code to be had: say so, offer to retry, ask nothing meanwhile.
    harness.click("connect", cx);
    assert_eq!(harness.phase(cx), Some(Phase::Offline));
    harness.wait(60, cx);
    assert_eq!(harness.flow.polls(), 0);

    // Enter is the screen's main button.
    press(harness.window, "enter", cx);
    assert!(is_waiting(harness.phase(cx), false));

    harness.flow.answer(Err(LoginError::Denied));
    harness.wait(5, cx);
    assert_eq!(harness.phase(cx), Some(Phase::Denied));
    harness.wait(60, cx);
    assert_eq!(harness.flow.polls(), 1, "a denial is final for that code");

    // Cancel leaves a code behind and returns to the start.
    harness.click("retry", cx);
    harness.click("cancel", cx);
    assert_eq!(harness.phase(cx), Some(Phase::Start));
    harness.wait(60, cx);
    assert_eq!(harness.flow.polls(), 1);
}

#[gpui_kit::test]
fn a_missing_keychain_is_said_on_screen(cx: &mut TestAppContext) {
    // The keychain could not be read at start: the notice is there before
    // anything is pressed, and signing in still works.
    let reason = "no secret service on the session bus";
    let harness = open_login(cx, vec![], Some(reason), KeySaved::No(reason.into()));
    assert!(harness.has("connect", cx));
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    assert!(visual.debug_bounds("keychain-notice").is_some());
    harness.click("connect", cx);
    harness.flow.answer(Ok(PollOutcome::Granted(grant())));
    harness.wait(5, cx);
    assert_eq!(
        harness.phase(cx),
        None,
        "already told: straight to the chats"
    );
}

#[gpui_kit::test]
fn a_key_that_could_not_be_saved_is_said_before_the_chats(cx: &mut TestAppContext) {
    // The keychain looked fine at start and then refused the key.
    let harness = open_login(
        cx,
        vec![],
        None,
        KeySaved::No("the keyring is locked".into()),
    );
    harness.click("connect", cx);
    harness.flow.answer(Ok(PollOutcome::Granted(grant())));
    harness.wait(5, cx);
    assert_eq!(
        harness.phase(cx),
        Some(Phase::KeyNotSaved {
            reason: "the keyring is locked".into()
        })
    );
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    assert!(visual.debug_bounds("keychain-notice").is_some());
    harness.click("continue", cx);
    assert_eq!(harness.phase(cx), None);
    // The settings say what kind of session this is, and it can be left.
    cx.update(|cx| match &harness.app.read(cx).screen {
        Screen::Chats(shell) => assert_eq!(shell.read(cx).session, SessionKind::Unsaved),
        _ => unreachable!(),
    });
    harness.click("settings", cx);
    assert!(harness.has("sign-out", cx));
}

#[gpui_kit::test]
fn closing_the_window_stops_the_polling(cx: &mut TestAppContext) {
    let LoginHarness {
        runtime: _runtime,
        flow,
        window,
        app,
    } = open_login(cx, vec![], None, KeySaved::Yes);
    click(window, "connect", cx);
    cx.executor().advance_clock(Duration::from_secs(5));
    cx.run_until_parked();
    assert_eq!(flow.polls(), 1);

    // The window owns the view, and the view owns the waiting.
    drop(app);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(120));
    cx.run_until_parked();
    assert_eq!(flow.polls(), 1);
}

#[gpui_kit::test]
fn the_demo_provider_needs_no_sign_in_and_offers_no_sign_out(cx: &mut TestAppContext) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mock = MockProvider::quiet();
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(mock.clone()),
        SyncConfig::default(),
        runtime.handle().clone(),
    );
    runtime.block_on(engine.refresh()).unwrap();
    let (window, app) = open_app(
        cx,
        Launch::Chats {
            engine,
            session: SessionKind::Demo,
            login: None,
            identity: None,
            storage_note: None,
        },
    );
    cx.update(|cx| match &app.read(cx).screen {
        Screen::Chats(shell) => assert_eq!(shell.read(cx).accounts.len(), 2),
        _ => panic!("the demo data asked to sign in"),
    });

    // The settings button answers, with nothing to sign out of.
    click(window, "settings", cx);
    assert!(shows(window, "settings-panel", cx));
    assert!(!shows(window, "sign-out", cx));
    // Escape closes it.
    press(window, "escape", cx);
    assert!(!shows(window, "settings-panel", cx));
}

// ----- menus, settings, new chat ------------------------------------------

/// The chats of the list, as (id, pinned, unread).
fn chat_rows(shell: &Shell) -> Vec<(String, bool, u32)> {
    shell
        .list_rows
        .iter()
        .filter_map(|row| match row {
            ListRow::Chat(chat) => Some((chat.id.to_string(), chat.pinned, chat.unread_count)),
            _ => None,
        })
        .collect()
}

#[gpui_kit::test]
fn the_chat_list_menu_opens_closes_and_marks_everything_read(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let unread = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            chat_rows(harness.shell.read(cx))
                .iter()
                .filter(|(_, _, unread)| *unread > 0)
                .count()
        })
    };
    assert!(unread(cx) > 0, "the demo data has unread chats");

    // The button opens it; Escape and a click elsewhere close it.
    click(harness.window, "list-menu", cx);
    assert!(shows(harness.window, "menu", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "menu", cx));
    click(harness.window, "list-menu", cx);
    click(harness.window, "start", cx);
    assert!(
        !shows(harness.window, "menu", cx),
        "a click outside closed it"
    );

    // "New group" opens its form, where the provider can create groups.
    click(harness.window, "list-menu", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let group = shell
            .menu_items_now()
            .into_iter()
            .find(|item| item.id == "menu-new-group")
            .unwrap();
        assert!(group.action.is_some() && group.hint.is_none());
    });
    click(harness.window, "menu-new-group", cx);
    assert!(shows(harness.window, "new-group", cx));
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "new-group", cx));
    click(harness.window, "list-menu", cx);

    // With the keyboard: down past "New group" and "New community" to
    // "Mark all as read", Enter.
    press(harness.window, "down", cx);
    press(harness.window, "down", cx);
    press(harness.window, "down", cx);
    press(harness.window, "enter", cx);
    assert!(!shows(harness.window, "menu", cx));
    assert_eq!(unread(cx), 0);
    // The provider hears about it in the background.
    harness.settle(cx);
    assert_eq!(unread(cx), 0);

    // "Archived chats" switches the list to the archive, empty so far.
    click(harness.window, "list-menu", cx);
    click(harness.window, "menu-archived", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.filter, ChatFilter::Archived);
        assert!(chat_rows(shell).is_empty());
    });
}

#[gpui_kit::test]
fn the_conversation_menu_pins_and_archives_the_chat(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    // A chat from the middle of the list, not pinned.
    let (position, chat) = cx.update(|cx| {
        let rows = chat_rows(harness.shell.read(cx));
        let position = rows.iter().rposition(|(_, pinned, _)| !pinned).unwrap();
        (
            position,
            client_provider::ChatId::new(rows[position].0.clone()),
        )
    });
    assert!(position > 3, "far enough down for the pin to be visible");
    harness
        .shell
        .update(cx, |shell, cx| shell.open_chat(chat.clone(), None, cx));
    cx.run_until_parked();

    click(harness.window, "chat-menu", cx);
    assert!(shows(harness.window, "menu", cx));
    click(harness.window, "menu-pin", cx);
    assert!(!shows(harness.window, "menu", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let rows = chat_rows(shell);
        let now = rows
            .iter()
            .position(|(id, ..)| id == chat.as_str())
            .unwrap();
        assert!(rows[now].1, "the row carries the pin");
        assert!(now < position, "and moved up: {position} -> {now}");
        assert!(shell.open.as_ref().unwrap().chat.pinned);
    });
    // It went through the engine to the provider, not around it.
    harness.settle(cx);
    let account = cx.update(|cx| harness.shell.read(cx).account.clone().unwrap());
    assert!(harness.mock.chat(&account, &chat).unwrap().pinned);

    // The same entry now undoes it.
    click(harness.window, "chat-menu", cx);
    cx.update(|cx| {
        let pin = harness
            .shell
            .read(cx)
            .menu_items_now()
            .into_iter()
            .find(|item| item.id == "menu-pin")
            .unwrap();
        assert_eq!(pin.label, "Unpin");
    });

    // Archive: out of the list, the conversation closes, the archive has it.
    click(harness.window, "menu-archive", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.open.is_none());
        assert!(chat_rows(shell).iter().all(|(id, ..)| id != chat.as_str()));
    });
    click(harness.window, "filter-archived", cx);
    cx.update(|cx| {
        let rows = chat_rows(harness.shell.read(cx));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, chat.as_str());
    });
}

#[gpui_kit::test]
fn contact_info_and_search_open_from_the_conversation(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    // A word from the chat's own history.
    let word = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let open = shell.open.as_ref().unwrap();
        open.rows
            .iter()
            .find_map(|row| match row {
                Row::Message(row) => match &row.stored.message.content {
                    MessageContent::Text { body } => body
                        .split_whitespace()
                        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
                        .find(|w| w.len() > 5)
                        .map(str::to_owned),
                    _ => None,
                },
                Row::Day(_) => None,
            })
            .expect("the chat has a text message with a long word")
    });

    click(harness.window, "chat-menu", cx);
    click(harness.window, "menu-info", cx);
    assert!(shows(harness.window, "contact-info", cx));
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::ContactInfo));
    click(harness.window, "close-panel", cx);
    assert!(!shows(harness.window, "contact-info", cx));

    // The header's search: a field over the thread, and what it finds.
    click(harness.window, "chat-search", cx);
    assert!(shows(harness.window, "thread-search", cx));
    type_text(harness.window, &word, cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let search = shell.thread_search.as_ref().unwrap();
        let open = shell.open.as_ref().unwrap();
        assert!(!search.hits.is_empty(), "`{word}` is in this chat");
        assert!(search
            .hits
            .iter()
            .all(|hit| hit.message.chat_id == open.chat.id));
    });
    // Picking a result keeps the conversation, scrolled to it.
    click(harness.window, "thread-hit-0", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).open.is_some()));
    click(harness.window, "thread-search-close", cx);
    assert!(!shows(harness.window, "thread-search", cx));

    // The same search from the menu.
    click(harness.window, "chat-menu", cx);
    click(harness.window, "menu-search", cx);
    assert!(shows(harness.window, "thread-search", cx));
}

#[gpui_kit::test]
fn new_chat_opens_a_conversation_with_a_number(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "new-chat", cx);
    assert!(shows(harness.window, "new-chat-panel", cx));

    // Not a number: said in the panel, nothing opens.
    type_text(harness.window, "maria", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "new-chat-error", cx));
    cx.update(|cx| assert!(harness.shell.read(cx).open.is_none()));

    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "+58 424 555 0199", cx);
    click(harness.window, "new-chat-open", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        let open = shell.open.as_ref().expect("the new conversation is open");
        assert_eq!(open.chat.id.as_str(), "+584245550199");
    });

    // And it can be written to: through the outbox, like any other.
    let composer = cx.update(|cx| harness.shell.read(cx).composer.clone());
    let field: ElementId = ("input", composer.entity_id()).into();
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx);
        window.input("hello", cx);
        window.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 1);
}

#[gpui_kit::test]
fn settings_change_the_theme_and_remember_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| {
        gpui_kit::init(cx);
        // No `--theme`: the saved choice decides.
        settings::init(Some(file.clone()), None, cx);
        settings::update(cx, |settings| {
            settings.theme = ThemeChoice::Light;
            settings.motion = MotionChoice::Off;
        });
    });
    let harness = open_prepared(cx, ShellOptions::default());
    assert!(!cx.update(|cx| theme::palette(cx).is_dark()));

    click(harness.window, "settings", cx);
    assert!(shows(harness.window, "settings-panel", cx));
    assert!(!shows(harness.window, "sign-out", cx), "demo data");
    // Each number says what the server downloads by itself.
    assert!(shows(harness.window, "number-server-media", cx));

    click(harness.window, "settings-appearance", cx);
    click(harness.window, "theme-dark", cx);
    assert!(cx.update(|cx| theme::palette(cx).is_dark()));
    click(harness.window, "motion-on", cx);
    // The notifications section says what it is, and keeps the choice.
    click(harness.window, "settings-notifications", cx);
    assert!(shows(harness.window, "notifications-note", cx));
    click(harness.window, "notify-previews", cx);
    click(harness.window, "settings-about", cx);
    click(harness.window, "open-repository", cx);
    assert_eq!(cx.opened_url().as_deref(), Some(crate::product::REPOSITORY));

    // On disk, as the next start will read it.
    let saved = Settings::load(&file);
    assert_eq!(saved.theme, ThemeChoice::Dark);
    assert_eq!(saved.motion, MotionChoice::On);
    assert!(saved.desktop_notifications && !saved.message_previews);

    // The rail's toggle is the same setting.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "settings-panel", cx));
    click(harness.window, "toggle-theme", cx);
    assert!(!cx.update(|cx| theme::palette(cx).is_dark()));
    assert_eq!(Settings::load(&file).theme, ThemeChoice::Light);

    // A new start with that file comes up as it was left.
    cx.update(|cx| {
        settings::update(cx, |settings| settings.theme = ThemeChoice::Dark);
        settings::init(Some(file.clone()), None, cx);
        assert!(theme::palette(cx).is_dark());
        assert!(!settings::reduce_motion(cx), "motion was turned on");
    });
}

#[gpui_kit::test]
fn the_start_screen_comes_in_once_and_then_stops_repainting(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        settings::init(None, Some(Appearance::Light), cx);
        settings::update(cx, |settings| settings.motion = MotionChoice::On);
    });
    let harness = open_prepared(cx, ShellOptions::default());
    let now = |cx: &mut TestAppContext| cx.executor().now();
    let (first, last) = cx.update(|cx| {
        let entrance = harness.shell.read(cx).entrance;
        let now = cx.background_executor().now();
        (entrance.reveal(0, now), entrance.running(now))
    });
    assert_eq!(first, 0., "not in yet");
    assert!(last);

    cx.executor().advance_clock(Duration::from_millis(1500));
    cx.run_until_parked();
    let at = now(cx);
    cx.update(|cx| {
        let entrance = harness.shell.read(cx).entrance;
        assert!((entrance.reveal(5, at) - 1.).abs() < 1e-3);
        assert!(!entrance.running(at), "over: nothing repaints any more");
    });
    assert!(shows(harness.window, "start-status", cx));
}

#[gpui_kit::test]
fn the_keyboard_reaches_every_menu(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let overlay = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).overlay);

    // Typing in the composer, as one usually is.
    let composer = cx.update(|cx| harness.shell.read(cx).composer.clone());
    let field: ElementId = ("input", composer.entity_id()).into();
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx)
    })
    .unwrap();
    cx.run_until_parked();

    press(harness.window, "ctrl-,", cx);
    assert_eq!(overlay(cx), Overlay::Settings);
    // The arrows walk the sections; Enter is not a way out of the panel.
    press(harness.window, "down", cx);
    assert!(shows(harness.window, "history-detail", cx));
    press(harness.window, "down", cx);
    assert!(shows(harness.window, "theme-dark", cx));
    press(harness.window, "enter", cx);
    assert_eq!(overlay(cx), Overlay::Settings);
    press(harness.window, "escape", cx);
    assert_eq!(overlay(cx), Overlay::None);

    press(harness.window, "ctrl-n", cx);
    assert_eq!(overlay(cx), Overlay::NewChat);
    press(harness.window, "escape", cx);
    assert_eq!(overlay(cx), Overlay::None);

    // With a conversation open the menu is the conversation's: arrows to
    // the pin entry, Enter.
    let pinned = |cx: &mut TestAppContext| {
        cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.pinned)
    };
    let before = pinned(cx);
    press(harness.window, "ctrl-.", cx);
    assert_eq!(overlay(cx), Overlay::ChatMenu);
    for _ in 0..3 {
        press(harness.window, "down", cx);
    }
    press(harness.window, "enter", cx);
    assert_eq!(overlay(cx), Overlay::None);
    assert_eq!(pinned(cx), !before, "the pin was toggled from the keyboard");

    press(harness.window, "ctrl-f", cx);
    assert!(shows(harness.window, "thread-search", cx));
    // Escape closes the search and leaves the conversation open; closing
    // it has a key of its own.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "thread-search", cx));
    cx.update(|cx| assert!(harness.shell.read(cx).open.is_some()));
    press(harness.window, "escape", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).open.is_some()));
    press(harness.window, "ctrl-w", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).open.is_none()));

    // Without one, it is the chat list's.
    press(harness.window, "ctrl-.", cx);
    assert_eq!(overlay(cx), Overlay::ListMenu);
}

// ----- the welcome --------------------------------------------------------

/// A scripted sign-in over demo data, as `open_login` builds it.
fn fake_flow(runtime: &Runtime) -> Arc<FakeFlow> {
    let mock = MockProvider::quiet();
    let engine = SyncEngine::new(
        Arc::new(Store::open_in_memory().unwrap()),
        Arc::new(mock.clone()),
        SyncConfig::default(),
        runtime.handle().clone(),
    );
    runtime.block_on(engine.refresh()).unwrap();
    Arc::new(FakeFlow {
        codes: Mutex::default(),
        answers: Mutex::default(),
        polls: AtomicUsize::new(0),
        sign_outs: AtomicUsize::new(0),
        forgotten: AtomicUsize::new(0),
        key_saved: KeySaved::Yes,
        engine,
        mock,
    })
}

/// Which screen the window shows.
fn screen_name(app: &Entity<AppView>, cx: &mut TestAppContext) -> &'static str {
    cx.update(|cx| match &app.read(cx).screen {
        Screen::Recovery(_) => "recovery",
        Screen::Welcome(_) => "welcome",
        Screen::Login(_) => "login",
        Screen::Chats(_) => "chats",
    })
}

#[gpui_kit::test]
fn a_first_run_is_welcomed_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let login = |flow: &Arc<FakeFlow>| Launch::Login {
        flow: flow.clone(),
        keychain_error: None,
        storage_note: None,
    };

    // No account, never welcomed: the welcome, not the sign-in.
    let flow = fake_flow(&runtime);
    let (window, app) = open_app_with(cx, login(&flow), false);
    assert_eq!(screen_name(&app, cx), "welcome");
    assert!(shows(window, "get-started", cx));
    assert!(!Settings::load(&file).welcome_done);

    // Escape has nothing to leave here, and leaves nothing.
    press(window, "escape", cx);
    assert_eq!(screen_name(&app, cx), "welcome");
    assert!(shows(window, "get-started", cx));

    // Enter is "Get started": the provider step, wuapi already chosen and
    // the others said to be missing rather than hidden.
    press(window, "enter", cx);
    assert!(shows(window, "provider-wuapi", cx) && shows(window, "provider-other", cx));
    assert!(!shows(window, "get-started", cx));
    // Escape steps back, and nothing is decided yet.
    press(window, "escape", cx);
    assert!(shows(window, "get-started", cx));
    assert!(!Settings::load(&file).welcome_done);

    click(window, "get-started", cx);
    click(window, "choose-wuapi", cx);
    assert_eq!(screen_name(&app, cx), "login");
    assert!(shows(window, "connect", cx));
    assert!(Settings::load(&file).welcome_done, "remembered on disk");

    // The next start, with the same settings file: straight to the sign-in.
    cx.update(|cx| settings::init(Some(file.clone()), Some(Appearance::Light), cx));
    let (_, again) = open_app_with(cx, login(&flow), false);
    assert_eq!(screen_name(&again, cx), "login");

    // `--welcome` shows it all the same.
    let (_, forced) = open_app_with(cx, login(&flow), true);
    assert_eq!(screen_name(&forced, cx), "welcome");
}

#[gpui_kit::test]
fn the_welcome_can_be_looked_at_on_demo_data(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let demo = |engine: SyncEngine| Launch::Chats {
        engine,
        session: SessionKind::Demo,
        login: None,
        identity: None,
        storage_note: None,
    };
    let engine = fake_flow(&runtime).engine.clone();

    // Demo data is never welcomed by itself: there is no account to make.
    let (_, plain) = open_app_with(cx, demo(engine.clone()), false);
    assert_eq!(screen_name(&plain, cx), "chats");

    // With `--welcome` it is, and "Get started" simply enters: there is no
    // provider to choose.
    let (window, app) = open_app_with(cx, demo(engine), true);
    assert_eq!(screen_name(&app, cx), "welcome");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let form = visual.debug_bounds("welcome-form").expect("the welcome");
    let button = visual.debug_bounds("get-started").expect("its action");
    assert!(button.left() >= form.left() && button.right() <= form.right());
    press(window, "enter", cx);
    assert_eq!(screen_name(&app, cx), "chats");
    cx.update(|cx| assert!(settings::get(cx).welcome_done));
}

// ----- the local database --------------------------------------------------

#[gpui_kit::test]
fn a_database_whose_key_is_lost_is_reset_only_when_asked(cx: &mut TestAppContext) {
    use crate::storage::tests::{fill, FakeVault};
    use crate::storage::{prepare, KeyVault, Prepared, Storage};

    cx.update(|cx| {
        prepare_app(cx);
        settings::update(cx, |settings| settings.welcome_done = true);
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wuapi.db");
    let vault = Arc::new(FakeVault::default());
    let ready = |prepared| match prepared {
        Prepared::Ready(storage) => storage,
        Prepared::KeyLost(_) => panic!("the key was reported lost"),
    };
    // An encrypted database from an earlier run, and then the keychain is
    // wiped.
    let vault_dyn: Arc<dyn KeyVault> = vault.clone();
    fill(
        &ready(prepare(&path, "db", Some(vault_dyn.clone())))
            .open()
            .unwrap(),
    );
    vault.keys.lock().unwrap().clear();
    let before = std::fs::read(&path).unwrap();
    let Prepared::KeyLost(recovery) = prepare(&path, "db", Some(vault_dyn)) else {
        panic!("an unreadable database was reported ready");
    };

    let handle = runtime.handle().clone();
    let launch = Launch::Recover {
        recovery,
        resume: Box::new(move |storage: Storage| {
            let note = storage.note();
            let engine = SyncEngine::new(
                Arc::new(storage.open()?),
                Arc::new(MockProvider::quiet()),
                SyncConfig::default(),
                handle,
            );
            Ok(Launch::Chats {
                engine,
                session: SessionKind::Demo,
                login: None,
                identity: None,
                storage_note: note,
            })
        }),
    };
    let (window, app) = open_app_with(cx, launch, false);

    // The prompt, saying what is lost. Nothing is deleted by showing it,
    // and Enter does not delete either.
    assert_eq!(screen_name(&app, cx), "recovery");
    assert!(shows(window, "reset-database", cx) && shows(window, "recovery-quit", cx));
    press(window, "enter", cx);
    assert_eq!(screen_name(&app, cx), "recovery");
    assert_eq!(std::fs::read(&path).unwrap(), before);

    // Asked: a new database under a new key, and on with the launch.
    click(window, "reset-database", cx);
    assert_eq!(screen_name(&app, cx), "chats");
    assert_eq!(
        vault.keys.lock().unwrap().len(),
        1,
        "a new key is in the vault"
    );
    assert_ne!(std::fs::read(&path).unwrap(), before);
    assert!(
        !shows(window, "storage-note", cx),
        "saved, encrypted: nothing to warn of"
    );
}

#[gpui_kit::test]
fn chats_that_are_not_being_saved_say_so(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            storage_note: Some(
                "Chats are kept in memory for this session and not saved to disk. The \
                 system keychain is not available (no secret service)."
                    .into(),
            ),
            ..Default::default()
        },
    );
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    let note = visual.debug_bounds("storage-note").expect("the note");
    // In the chat list's column, above the list, and not a sliver.
    let rail = theme::metrics::RAIL_WIDTH();
    let list = cx.update(|cx| harness.shell.read(cx).list_px);
    assert!(note.left() >= rail && note.right() <= rail + list);
    assert!(note.size.height >= px(30.));

    // Without a note there is no strip.
    let plain = open(cx, ShellOptions::default());
    assert!(!shows(plain.window, "storage-note", cx));
}

#[gpui_kit::test]
fn the_start_screen_fits_a_small_window(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    // The smallest window the application allows.
    visual.simulate_resize(size(px(820.), px(520.)));
    visual.run_until_parked();
    let start = visual.debug_bounds("start").expect("the start screen");
    let list = visual.update(|_, cx| harness.shell.read(cx).list_px);
    let pane_left = theme::metrics::RAIL_WIDTH() + list;
    assert!(
        start.left() >= pane_left && start.right() <= px(820.),
        "inside its pane: {start:?}"
    );
    assert!(
        start.top() >= theme::metrics::HEADER_HEIGHT() && start.bottom() <= px(520.),
        "not cut off above or below: {start:?}"
    );
    assert!(start.size.width > px(250.), "and not squeezed: {start:?}");
}

// ----- signing out, and being signed out -----------------------------------

/// Signs in through the fake flow and returns the shell.
fn sign_in(harness: &LoginHarness, cx: &mut TestAppContext) -> Entity<Shell> {
    harness.click("connect", cx);
    harness.flow.answer(Ok(PollOutcome::Granted(grant())));
    harness.wait(5, cx);
    cx.update(|cx| match &harness.app.read(cx).screen {
        Screen::Chats(shell) => shell.clone(),
        _ => panic!("not signed in"),
    })
}

impl LoginHarness {
    /// Lets the engine's background work run, then the UI react.
    fn settle(&self, cx: &mut TestAppContext) {
        self.runtime.block_on(async {
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
        });
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn signing_out_with_messages_waiting_takes_a_deliberate_click(cx: &mut TestAppContext) {
    let harness = open_login(cx, vec![], None, KeySaved::Yes);
    let shell = sign_in(&harness, cx);
    // A message that has not gone out yet.
    cx.update(|cx| {
        let shell = shell.read(cx);
        let chat = shell.list_rows.iter().find_map(|row| match row {
            ListRow::Chat(chat) => Some(chat.clone()),
            _ => None,
        });
        let chat = chat.unwrap();
        shell
            .engine
            .send_text(&chat.account_id, &chat.id, "not sent yet", None)
            .unwrap();
    });

    harness.click("settings", cx);
    harness.click("sign-out", cx);
    assert!(
        harness.has("unsent-warning", cx),
        "what would be lost is said"
    );
    // Enter is not enough when something would be lost; Escape backs out.
    press(harness.window, "enter", cx);
    assert_eq!(harness.flow.sign_outs.load(Ordering::SeqCst), 0);
    assert!(harness.has("confirm-sign-out", cx));
    press(harness.window, "escape", cx);
    assert!(!harness.has("confirm-sign-out", cx));
    assert_eq!(harness.phase(cx), None, "still signed in");

    harness.click("settings", cx);
    harness.click("sign-out", cx);
    harness.click("sign-out-confirm", cx);
    assert_eq!(harness.flow.sign_outs.load(Ordering::SeqCst), 1);
    assert_eq!(harness.phase(cx), Some(Phase::Start));
    // The settings are not part of what goes.
    cx.update(|cx| assert!(settings::get(cx).welcome_done));
}

#[gpui_kit::test]
fn with_nothing_waiting_enter_confirms_the_sign_out(cx: &mut TestAppContext) {
    let harness = open_login(cx, vec![], None, KeySaved::Yes);
    sign_in(&harness, cx);
    harness.click("settings", cx);
    harness.click("sign-out", cx);
    press(harness.window, "enter", cx);
    assert_eq!(harness.flow.sign_outs.load(Ordering::SeqCst), 1);
    assert_eq!(harness.phase(cx), Some(Phase::Start));
}

#[gpui_kit::test]
fn a_revoked_key_leads_to_the_sign_in_and_a_dropped_connection_does_not(cx: &mut TestAppContext) {
    use client_provider::ProviderError;
    let harness = open_login(cx, vec![], None, KeySaved::Yes);
    let shell = sign_in(&harness, cx);
    let chats: Vec<_> = cx.update(|cx| {
        shell
            .read(cx)
            .list_rows
            .iter()
            .filter_map(|row| match row {
                ListRow::Chat(chat) => Some(chat.id.clone()),
                _ => None,
            })
            .collect()
    });

    // The connection drops while a chat's history loads: nothing happens
    // that the user can see. Nobody is signed out.
    harness.flow.mock.fail_next_history([
        ProviderError::Transient("could not connect".into()),
        ProviderError::Transient("HTTP 502".into()),
    ]);
    shell.update(cx, |shell, cx| shell.open_chat(chats[0].clone(), None, cx));
    harness.settle(cx);
    shell.update(cx, |shell, cx| shell.open_chat(chats[1].clone(), None, cx));
    harness.settle(cx);
    assert_eq!(harness.phase(cx), None, "still in the chats");
    assert!(!harness.flow.engine.is_auth_lost());

    // The API answers 401: the key was revoked. That is the end of the
    // session, said plainly, and not something to retry.
    harness
        .flow
        .mock
        .fail_next_history([ProviderError::Unauthorized("revoked".into())]);
    shell.update(cx, |shell, cx| shell.open_chat(chats[2].clone(), None, cx));
    harness.settle(cx);
    assert_eq!(harness.phase(cx), Some(Phase::Start), "the sign-in screen");
    assert!(harness.has("login-notice", cx));
    cx.update(|cx| match &harness.app.read(cx).screen {
        Screen::Login(screen) => assert_eq!(
            screen.read(cx).notice.as_deref(),
            Some(super::app::SIGNED_OUT)
        ),
        _ => unreachable!(),
    });
    // The useless key is forgotten; the chats on this computer are not
    // touched until someone signs in.
    assert_eq!(harness.flow.forgotten.load(Ordering::SeqCst), 1);
    assert_eq!(harness.flow.sign_outs.load(Ordering::SeqCst), 0);
    let calls = harness.flow.mock.history_calls();
    harness.settle(cx);
    assert_eq!(
        harness.flow.mock.history_calls(),
        calls,
        "nothing is retried"
    );
}

// ----- history, and how long the first frames take --------------------------

#[gpui_kit::test]
fn the_history_mode_is_chosen_in_settings_and_takes_effect_at_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    let harness = open_prepared(cx, ShellOptions::default());

    click(harness.window, "settings", cx);
    click(harness.window, "settings-sync", cx);
    assert!(shows(harness.window, "history-detail", cx));
    click(harness.window, "history-recent chats", cx);
    assert_eq!(harness.engine.history(), HistoryMode::Recent(20));
    click(harness.window, "recent-50", cx);
    assert_eq!(harness.engine.history(), HistoryMode::Recent(50));
    let saved = Settings::load(&file);
    assert_eq!(saved.history_mode(), HistoryMode::Recent(50));

    click(harness.window, "history-everything", cx);
    assert_eq!(harness.engine.history(), HistoryMode::Everything);
    assert!(
        !shows(harness.window, "recent-50", cx),
        "no count to choose"
    );
    click(harness.window, "history-when i open a chat", cx);
    assert_eq!(harness.engine.history(), HistoryMode::OnOpen);
    assert_eq!(Settings::load(&file).history_mode(), HistoryMode::OnOpen);
}

#[gpui_kit::test]
fn the_first_frames_come_from_the_store_without_waiting(cx: &mut TestAppContext) {
    // Wall-clock time of building and laying out a frame in the headless
    // window (no GPU, no network): what the application itself spends
    // before the compositor gets a frame.
    let started = std::time::Instant::now();
    let harness = open(cx, ShellOptions::default());
    let setup = started.elapsed();

    let list = std::time::Instant::now();
    harness.shell.update(cx, |shell, cx| shell.reload_chats(cx));
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.render_frame(cx)
    })
    .unwrap();
    let list = list.elapsed();
    assert!(shows(harness.window, "start", cx));

    let chat = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_rows
            .iter()
            .find_map(|row| match row {
                ListRow::Chat(chat) => Some(chat.id.clone()),
                _ => None,
            })
            .unwrap()
    });
    let calls = harness.mock.history_calls();
    let open_chat = std::time::Instant::now();
    harness
        .shell
        .update(cx, |shell, cx| shell.open_chat(chat, None, cx));
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.render_frame(cx)
    })
    .unwrap();
    let open_chat = open_chat.elapsed();
    // The messages on screen came from the store: the provider was not
    // asked before the frame.
    assert_eq!(harness.mock.history_calls(), calls);
    assert!(shows(harness.window, "thread", cx));
    cx.update(|cx| assert!(newest_message(harness.shell.read(cx)).is_some()));

    eprintln!(
        "first frames (headless): chat list {list:?}, opened chat {open_chat:?} \
         (window and store setup {setup:?})"
    );
    let limit = Duration::from_millis(500);
    assert!(
        list < limit && open_chat < limit,
        "{list:?} / {open_chat:?}"
    );
}

// ----- pictures and media ---------------------------------------------------

/// A PNG of the given size.
fn png(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 90])
    });
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

/// Puts a media message at the end of the open chat, as if it had just
/// arrived (or, with `outgoing`, been sent from the phone).
fn push_media(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    kind: client_provider::MediaKind,
    source: Option<&str>,
    outgoing: bool,
) {
    use client_provider::{
        ContactId, Media, MediaRef, Message, MessageId, ProviderEvent, Timestamp,
    };
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let mut media = Media::new(kind);
    media.source = source.map(MediaRef::new);
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(Message {
            id: MessageId::new(id),
            client_id: None,
            account_id: chat.account_id.clone(),
            chat_id: chat.id.clone(),
            sender: ContactId::new(if outgoing { "me" } else { "them" }),
            sender_name: None,
            direction: if outgoing {
                Direction::Outgoing
            } else {
                Direction::Incoming
            },
            timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000),
            content: MessageContent::Media(media),
            reply_to: None,
            status: DeliveryStatus::Delivered,
            edited: false,
            deleted: false,
            extras: Default::default(),
        }))
        .unwrap();
    cx.run_until_parked();
}

/// Caches a thumbnail for `url`, as the engine does once it has fetched it.
fn cache_thumbnail(harness: &Harness, cx: &mut TestAppContext, url: &str, size: (u32, u32)) {
    use client_core::{thumbnail_key, CachedMedia};
    let media = CachedMedia {
        bytes: png(size.0, size.1),
        mime: Some("image/png".into()),
        size: Some(size),
    };
    harness
        .engine
        .store()
        .put_media(
            &thumbnail_key(url),
            &media,
            client_provider::Timestamp::now(),
            u64::MAX,
        )
        .unwrap();
    cx.run_until_parked();
}

fn bounds(
    window: WindowHandle<gpui_kit::base::Root>,
    selector: &'static str,
    cx: &mut TestAppContext,
) -> Bounds<gpui_kit::Pixels> {
    VisualTestContext::from_window(window.into(), cx)
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("`{selector}` is not on screen"))
}

#[gpui_kit::test]
fn a_profile_picture_takes_the_place_of_the_initials_and_nothing_moves(cx: &mut TestAppContext) {
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let before = bounds(harness.window, "header-avatar", cx);
    let thread = bounds(harness.window, "thread", cx);
    assert!(
        !shows(harness.window, "avatar-image", cx),
        "initials so far"
    );

    // The picture arrives in the store, as the engine puts it there.
    harness
        .engine
        .store()
        .put_avatar(
            &chat.account_id,
            &chat.id,
            Some(("pic-1", &png(96, 96))),
            client_provider::Timestamp::now(),
        )
        .unwrap();
    cx.run_until_parked();

    assert!(
        shows(harness.window, "avatar-image", cx),
        "the picture is drawn"
    );
    assert_eq!(bounds(harness.window, "header-avatar", cx), before);
    assert_eq!(bounds(harness.window, "thread", cx), thread);
    let picture = bounds(harness.window, "avatar-image", cx);
    assert_eq!(picture.size.width, picture.size.height, "a circle's box");

    // Taken away again (the contact removed it): back to the initials.
    harness
        .engine
        .store()
        .put_avatar(
            &chat.account_id,
            &chat.id,
            None,
            client_provider::Timestamp::now(),
        )
        .unwrap();
    cx.run_until_parked();
    assert!(!shows(harness.window, "avatar-image", cx));
    assert_eq!(bounds(harness.window, "header-avatar", cx), before);
}

#[gpui_kit::test]
fn an_image_fills_its_box_when_it_arrives_and_opens_in_the_viewer(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let url = "https://media.example/holiday.jpg";
    push_media(&harness, cx, "m-photo", MediaKind::Image, Some(url), false);

    // The row has its size before there is anything to show in it.
    let placeholder = bounds(harness.window, "media-box", cx);
    let bubble = bounds(harness.window, "bubble", cx);
    assert_eq!(placeholder.size, super::media::PLACEHOLDER());
    assert!(!shows(harness.window, "media-image", cx));
    let rows = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().rows.len());

    // The thumbnail arrives: the image is in the same box, and neither the
    // bubble nor the list moved.
    cache_thumbnail(&harness, cx, url, (600, 300));
    let image = bounds(harness.window, "media-image", cx);
    assert_eq!(image, placeholder, "exactly where the placeholder was");
    assert_eq!(bounds(harness.window, "media-box", cx), placeholder);
    assert_eq!(bounds(harness.window, "bubble", cx), bubble);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let open = shell.open.as_ref().unwrap();
        assert_eq!(open.rows.len(), rows);
        assert_eq!(open.list.item_count(), rows);
    });

    // A click opens it large; Escape closes it.
    click(harness.window, "media-image", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::Viewer));
    assert!(shows(harness.window, "viewer", cx));
    let viewer = bounds(harness.window, "viewer", cx);
    assert!(viewer.size.width > px(1000.), "the size of the window");
    press(harness.window, "escape", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
    assert!(!shows(harness.window, "viewer", cx));
}

#[gpui_kit::test]
fn a_sticker_sent_from_the_phone_shows_without_a_bubble(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    // Outgoing, but not through this client: sent from the phone. Its file
    // is one wuapi stored, like an inbound one.
    let url = "https://api.wuapi.dev/v1/media/stk123";
    push_media(
        &harness,
        cx,
        "m-sticker",
        MediaKind::Sticker,
        Some(url),
        true,
    );
    let text_bubble = bounds(harness.window, "bubble", cx);
    let sticker = bounds(harness.window, "sticker", cx);
    let frame = bounds(harness.window, "media-box", cx);
    assert_eq!(
        frame.size,
        size(super::media::STICKER(), super::media::STICKER()),
        "sticker size, not a photo's"
    );
    // No bubble was drawn for it: the last bubble on screen is an earlier
    // message's, above the sticker.
    assert!(
        text_bubble.bottom() <= sticker.top(),
        "{text_bubble:?} / {sticker:?}"
    );
    // On the sender's side, with the time and tick under it.
    let thread = bounds(harness.window, "thread", cx);
    assert!(sticker.right() > thread.right() - px(60.));
    assert!(sticker.size.height > frame.size.height + px(10.));

    cache_thumbnail(&harness, cx, url, (512, 512));
    assert_eq!(bounds(harness.window, "media-image", cx), frame);
    assert_eq!(
        bounds(harness.window, "sticker", cx),
        sticker,
        "nothing moved"
    );
}

#[gpui_kit::test]
fn media_the_provider_has_no_file_for_says_so(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let calls = harness.mock.media_calls();
    // Imported with the account's history: there is no URL.
    push_media(&harness, cx, "m-old", MediaKind::Image, None, false);
    assert!(shows(harness.window, "media-unavailable", cx));
    assert_eq!(
        bounds(harness.window, "media-box", cx).size,
        super::media::PLACEHOLDER()
    );
    push_media(&harness, cx, "m-old-doc", MediaKind::Document, None, false);
    assert!(shows(harness.window, "media-unavailable", cx));
    assert!(!shows(harness.window, "media-open", cx), "nothing to open");
    harness.settle(cx);
    assert_eq!(
        harness.mock.media_calls(),
        calls,
        "and nothing was asked for"
    );
}

#[gpui_kit::test]
fn with_automatic_downloads_off_an_image_waits_for_a_click(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    cx.update(|cx| {
        prepare(cx, None);
        settings::update(cx, |settings| {
            settings.media = crate::settings::MediaChoice::Never
        });
    });
    let harness = open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    harness.settle(cx);
    assert_eq!(harness.mock.media_calls(), 0, "the demo's photos included");

    let url = "https://media.example/later.png";
    harness.mock.set_media(url, png(400, 200), "image/png");
    push_media(&harness, cx, "m-later", MediaKind::Image, Some(url), false);
    harness.settle(cx);
    // Not fetched by itself: the round button, in the middle of the box.
    assert!(shows(harness.window, "media-download", cx));
    assert_eq!(harness.mock.media_calls(), 0);
    let placeholder = bounds(harness.window, "media-box", cx);
    let button = bounds(harness.window, "media-download", cx);
    assert!(within(button, placeholder));
    assert!((button.center().x - placeholder.center().x).abs() <= px(1.));
    assert!(button.size.width == button.size.height, "round");
    let bubble = bounds(harness.window, "bubble", cx);

    // Asked for: fetched, through the provider, and shown in the same box.
    click(harness.window, "media-download", cx);
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(10));
        harness.settle(cx);
        if shows(harness.window, "media-image", cx) {
            break;
        }
    }
    assert_eq!(harness.mock.media_calls(), 1);
    assert_eq!(bounds(harness.window, "media-image", cx), placeholder);
    assert_eq!(
        bounds(harness.window, "bubble", cx),
        bubble,
        "the row did not move"
    );
    assert!(!shows(harness.window, "media-download", cx));
}

// ----- audio ----------------------------------------------------------------

use crate::audio::tests::{FakeOutput, Tape, VOICE_NOTE};
use crate::audio::{Playback, Speed};

/// The open chat with three voice notes at its end (the mock serves a real
/// Ogg/Opus file for each) and a player that makes no sound.
fn with_voice_notes(
    cx: &mut TestAppContext,
) -> (
    Harness,
    std::rc::Rc<std::cell::RefCell<Tape>>,
    Vec<client_provider::Media>,
) {
    use client_provider::{Media, MediaKind, MediaRef};
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let tape = std::rc::Rc::new(std::cell::RefCell::new(Tape::default()));
    let output = Box::new(FakeOutput(tape.clone()));
    harness
        .shell
        .update(cx, |shell, _| shell.set_audio_output(output));
    let mut notes = Vec::new();
    for n in 1..=3 {
        let url = format!("https://media.example/voice-{n}.ogg");
        harness
            .mock
            .set_media(&url, VOICE_NOTE.to_vec(), "audio/ogg; codecs=opus");
        let id: &'static str = Box::leak(format!("m-voice-{n}").into_boxed_str());
        push_media(&harness, cx, id, MediaKind::Voice, Some(&url), false);
        let mut media = Media::new(MediaKind::Voice);
        media.source = Some(MediaRef::new(url));
        notes.push(media);
    }
    (harness, tape, notes)
}

/// Lets downloads and decoding (which run off this thread) finish.
fn until(harness: &Harness, cx: &mut TestAppContext, done: impl Fn(&Shell) -> bool) {
    for _ in 0..300 {
        harness.settle(cx);
        if cx.update(|cx| done(harness.shell.read(cx))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("the window never got there");
}

fn url_of(media: &client_provider::Media) -> String {
    media.source.as_ref().unwrap().to_string()
}

#[gpui_kit::test]
fn a_voice_note_downloads_decodes_and_plays_on_the_first_click(cx: &mut TestAppContext) {
    let (harness, tape, notes) = with_voice_notes(cx);
    let first = url_of(&notes[0]);
    let downloads = harness.mock.media_calls();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(
            shell.media.shape(&first),
            None,
            "nothing known before a click"
        );
        assert!(!shell.audio.player.is_playing());
    });
    harness.settle(cx);
    assert_eq!(
        harness.mock.media_calls(),
        downloads,
        "audio is not fetched by itself"
    );

    // Play: the file is fetched, decoded off this thread, and plays.
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[0].clone(), cx));
    cx.update(|cx| {
        assert_eq!(
            harness.shell.read(cx).audio.waiting.as_deref(),
            Some(first.as_str())
        );
    });
    until(&harness, cx, |shell| shell.audio.player.is_playing());
    assert_eq!(harness.mock.media_calls(), downloads + 1);
    {
        let tape = tape.borrow();
        assert_eq!(tape.started.len(), 1);
        let (frames, from, speed) = tape.started[0];
        assert!(
            (119_000..123_000).contains(&frames),
            "2.5 s at 48 kHz: {frames}"
        );
        assert_eq!((from, speed), (Duration::ZERO, 1.));
    }
    // Its real length and waveform are now known, and kept.
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let shape = shell
            .media
            .shape(&first)
            .expect("decoded once, known from now on");
        assert!((shape.duration.as_secs_f32() - 2.5).abs() < 0.05);
        assert!(shape.bars.iter().any(|bar| *bar > 100));
        assert_eq!(shell.audio.waiting, None);
    });

    // The position is observed, not driven, by the UI.
    tape.borrow_mut().advance(Duration::from_secs(1));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    let playback = |cx: &mut TestAppContext, url: &str| {
        cx.update(|cx| harness.shell.read(cx).audio.player.playback(url))
    };
    assert_eq!(
        playback(cx, &first),
        Playback::Playing(Duration::from_secs(1))
    );

    // Pause and resume from the same place; no second download.
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[0].clone(), cx));
    assert_eq!(
        playback(cx, &first),
        Playback::Paused(Duration::from_secs(1))
    );
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[0].clone(), cx));
    assert_eq!(
        tape.borrow().started.last().unwrap().1,
        Duration::from_secs(1)
    );
    assert_eq!(harness.mock.media_calls(), downloads + 1);

    // Seek and speed.
    harness
        .shell
        .update(cx, |shell, cx| shell.seek_audio(&first, 0.5, cx));
    let from = tape.borrow().started.last().unwrap().1;
    assert!((from.as_secs_f32() - 1.25).abs() < 0.03, "{from:?}");
    harness
        .shell
        .update(cx, |shell, cx| shell.cycle_audio_speed(cx));
    cx.update(|cx| assert_eq!(harness.shell.read(cx).audio.player.speed(), Speed::Faster));
    assert_eq!(tape.borrow().speeds.last(), Some(&1.5));
}

#[gpui_kit::test]
fn one_voice_note_at_a_time_then_the_next_and_silence_when_the_chat_closes(
    cx: &mut TestAppContext,
) {
    let (harness, tape, notes) = with_voice_notes(cx);
    let current = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .audio
                .player
                .current()
                .map(str::to_owned)
        })
    };
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[0].clone(), cx));
    until(&harness, cx, |shell| shell.audio.player.is_playing());

    // Starting the third stops the first.
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[2].clone(), cx));
    until(&harness, cx, |shell| {
        shell.audio.player.current() == Some(url_of(&notes[2]).as_str())
    });
    assert_eq!(
        cx.update(|cx| harness
            .shell
            .read(cx)
            .audio
            .player
            .playback(&url_of(&notes[0]))),
        Playback::Idle
    );

    // The first again, played to its end: the second follows by itself,
    // and after the third (the last of the run) there is silence.
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[0].clone(), cx));
    until(&harness, cx, |shell| {
        shell.audio.player.current() == Some(url_of(&notes[0]).as_str())
    });
    for next in [1, 2] {
        tape.borrow_mut().advance(Duration::from_secs(10));
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        until(&harness, cx, |shell| {
            shell.audio.player.current() == Some(url_of(&notes[next]).as_str())
                && shell.audio.player.is_playing()
        });
    }
    tape.borrow_mut().advance(Duration::from_secs(10));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    harness.settle(cx);
    assert_eq!(current(cx), None, "nothing follows the last one");

    // Closing the chat mid-note stops it.
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[1].clone(), cx));
    until(&harness, cx, |shell| shell.audio.player.is_playing());
    let halts = tape.borrow().halts;
    harness.shell.update(cx, |shell, cx| shell.close_chat(cx));
    assert!(tape.borrow().halts > halts);
    assert_eq!(current(cx), None);
}

#[gpui_kit::test]
fn audio_that_cannot_play_says_why_and_can_be_retried(cx: &mut TestAppContext) {
    use client_provider::{Media, MediaKind, MediaRef};
    let (harness, tape, notes) = with_voice_notes(cx);

    // No sound card: said in the bubble, not swallowed.
    tape.borrow_mut().broken = Some("No audio output device".into());
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[0].clone(), cx));
    until(&harness, cx, |shell| shell.audio.no_output.is_some());
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let (clip, reason) = shell.audio.no_output.clone().unwrap();
        assert_eq!(clip, url_of(&notes[0]));
        assert_eq!(reason.as_ref(), "No audio output device");
        assert!(!shell.audio.player.is_playing());
    });
    // Plugged in: the same button plays.
    tape.borrow_mut().broken = None;
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(notes[0].clone(), cx));
    until(&harness, cx, |shell| shell.audio.player.is_playing());
    cx.update(|cx| assert!(harness.shell.read(cx).audio.no_output.is_none()));

    // A file that is not audio: an error on the bubble, and a retry.
    let broken = "https://media.example/broken.ogg";
    harness
        .mock
        .set_media(broken, b"not audio at all".to_vec(), "audio/ogg");
    push_media(
        &harness,
        cx,
        "m-broken",
        MediaKind::Voice,
        Some(broken),
        false,
    );
    let mut media = Media::new(MediaKind::Voice);
    media.source = Some(MediaRef::new(broken));
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(media.clone(), cx));
    until(&harness, cx, |shell| {
        shell.audio.failed.contains_key(broken)
    });
    // The file is fixed (re-sent, say): pressing play again plays it.
    cx.update(|cx| assert_eq!(harness.shell.read(cx).audio.waiting, None));
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(media.clone(), cx));
    until(&harness, cx, |shell| {
        shell.audio.failed.contains_key(broken)
    });
    cx.update(|cx| {
        assert!(harness.shell.read(cx).audio.player.current() != Some(broken));
    });
}

/// The open chat with one voice note at its end, whose file the provider
/// names `source`, and a player that makes no sound.
fn with_one_voice_note(
    cx: &mut TestAppContext,
    source: &str,
) -> (
    Harness,
    std::rc::Rc<std::cell::RefCell<Tape>>,
    client_provider::Media,
) {
    use client_provider::{Media, MediaKind, MediaRef};
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let tape = std::rc::Rc::new(std::cell::RefCell::new(Tape::default()));
    let output = Box::new(FakeOutput(tape.clone()));
    harness
        .shell
        .update(cx, |shell, _| shell.set_audio_output(output));
    harness
        .mock
        .set_media(source, VOICE_NOTE.to_vec(), "audio/ogg; codecs=opus");
    push_media(
        &harness,
        cx,
        "m-voice",
        MediaKind::Voice,
        Some(source),
        false,
    );
    let mut media = Media::new(MediaKind::Voice);
    media.source = Some(MediaRef::new(source));
    harness.settle(cx);
    assert!(shows(harness.window, "audio-download", cx));
    assert!(!shows(harness.window, "audio-play", cx));
    (harness, tape, media)
}

/// Plays what is in the player to its end.
fn play_out(
    harness: &Harness,
    tape: &std::rc::Rc<std::cell::RefCell<Tape>>,
    cx: &mut TestAppContext,
) {
    tape.borrow_mut().advance(Duration::from_secs(10));
    cx.executor().advance_clock(Duration::from_millis(100));
    cx.run_until_parked();
    harness.settle(cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).audio.player.current(), None));
}

#[gpui_kit::test]
fn a_voice_note_that_was_heard_stays_here(cx: &mut TestAppContext) {
    let (harness, tape, note) = with_one_voice_note(cx, "https://media.example/voice.ogg");
    let downloads = harness.mock.media_calls();
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(note.clone(), cx));
    until(&harness, cx, |shell| shell.audio.player.is_playing());
    play_out(&harness, &tape, cx);

    // Heard to its end: the file is still here, and so is its waveform.
    assert!(shows(harness.window, "audio-play", cx));
    assert!(!shows(harness.window, "audio-download", cx));
    assert!(shows(harness.window, "audio-wave-real", cx));
    assert_eq!(harness.mock.media_calls(), downloads + 1);
}

#[gpui_kit::test]
fn a_voice_note_stays_here_when_the_provider_names_its_file_differently(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    // Still on WhatsApp: the provider names the message, not an address.
    let (harness, tape, note) = with_one_voice_note(cx, "provider-media:9000:m-voice");
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(note.clone(), cx));
    until(&harness, cx, |shell| shell.audio.player.is_playing());
    play_out(&harness, &tape, cx);
    let downloads = harness.mock.media_calls();

    // The provider stores the file now: the same message comes back with
    // the address of the file.
    let stored = "https://media.example/m-voice.ogg";
    push_media(
        &harness,
        cx,
        "m-voice",
        MediaKind::Voice,
        Some(stored),
        false,
    );
    harness.settle(cx);

    assert!(shows(harness.window, "audio-play", cx));
    assert!(!shows(harness.window, "audio-download", cx));
    assert!(shows(harness.window, "audio-wave-real", cx));
    assert_eq!(harness.mock.media_calls(), downloads, "not fetched again");
}

#[gpui_kit::test]
fn a_voice_note_goes_on_playing_when_the_provider_names_its_file_differently(
    cx: &mut TestAppContext,
) {
    use client_provider::{MediaKind, MediaRef};
    let (harness, tape, note) = with_one_voice_note(cx, "provider-media:9000:m-voice");
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(note.clone(), cx));
    until(&harness, cx, |shell| shell.audio.player.is_playing());
    tape.borrow_mut().advance(Duration::from_secs(1));
    let downloads = harness.mock.media_calls();

    let stored = "https://media.example/m-voice.ogg";
    push_media(
        &harness,
        cx,
        "m-voice",
        MediaKind::Voice,
        Some(stored),
        false,
    );
    harness.settle(cx);

    // The bubble still is the one that plays: its button pauses.
    assert!(shows(harness.window, "audio-pause", cx));
    assert!(!shows(harness.window, "audio-play", cx));
    let mut renamed = note.clone();
    renamed.source = Some(MediaRef::new(stored));
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(renamed.clone(), cx));
    cx.update(|cx| {
        assert_eq!(
            harness.shell.read(cx).audio.player.playback(stored),
            Playback::Paused(Duration::from_secs(1))
        );
    });
    // And resumes from there, with the clip it already has.
    harness
        .shell
        .update(cx, |shell, cx| shell.toggle_audio(renamed.clone(), cx));
    assert_eq!(
        tape.borrow().started.last().unwrap().1,
        Duration::from_secs(1)
    );
    assert_eq!(tape.borrow().started.len(), 2);
    assert_eq!(harness.mock.media_calls(), downloads);
}

// ----- layout of the bubbles ------------------------------------------------

fn centre_y(bounds: Bounds<gpui_kit::Pixels>) -> gpui_kit::Pixels {
    bounds.top() + bounds.size.height / 2.
}

fn within(inner: Bounds<gpui_kit::Pixels>, outer: Bounds<gpui_kit::Pixels>) -> bool {
    inner.left() >= outer.left()
        && inner.right() <= outer.right()
        && inner.top() >= outer.top()
        && inner.bottom() <= outer.bottom()
}

#[gpui_kit::test]
fn the_audio_bubble_is_one_centred_row_of_a_fixed_width(cx: &mut TestAppContext) {
    let (harness, _tape, _notes) = with_voice_notes(cx);
    let bubble = bounds(harness.window, "bubble", cx);
    let button = bounds(harness.window, "audio-button", cx);
    let wave = bounds(harness.window, "audio-wave", cx);
    let time = bounds(harness.window, "audio-time", cx);
    // Button and waveform on one line, centred on it to the pixel.
    assert!(
        (centre_y(button) - centre_y(wave)).abs() <= px(1.),
        "{button:?} / {wave:?}"
    );
    for part in [button, wave, time] {
        assert!(within(part, bubble), "{part:?} outside {bubble:?}");
    }
    assert!(wave.left() > button.right() && time.top() >= wave.bottom());
    // Every voice note has the same width: a fixed width of content, the
    // bubble's padding and its border.
    let row = bounds(harness.window, "audio-row", cx);
    assert_eq!(row.size.width, theme::metrics::AUDIO_WIDTH());
    assert!(bubble.size.width > row.size.width && bubble.size.width < row.size.width + px(40.));
    // The same padding left and right of the row.
    assert!(((row.left() - bubble.left()) - (bubble.right() - row.right())).abs() <= px(1.));
}

#[gpui_kit::test]
fn file_tiles_keep_their_icon_and_label_centred_inside_the_bubble(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    for (id, kind) in [
        ("m-doc", MediaKind::Document),
        ("m-video", MediaKind::Video),
    ] {
        let url = format!("https://media.example/{id}");
        push_media(&harness, cx, id, kind, Some(&url), false);
        let bubble = bounds(harness.window, "bubble", cx);
        let icon = bounds(harness.window, "tile-icon", cx);
        let label = bounds(harness.window, "tile-label", cx);
        let action = bounds(harness.window, "media-open", cx);
        assert!(
            (centre_y(icon) - centre_y(label)).abs() <= px(1.),
            "{kind:?}: {icon:?} / {label:?}"
        );
        for part in [icon, label, action] {
            assert!(
                within(part, bubble),
                "{kind:?}: {part:?} outside {bubble:?}"
            );
        }
        assert!(label.left() > icon.right() && action.top() >= label.bottom());
    }
}

#[gpui_kit::test]
fn the_newest_message_is_never_under_the_composer(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    let clear = |cx: &mut TestAppContext, when: &str| {
        let newest = bounds(harness.window, "bubble", cx);
        let composer = bounds(harness.window, "composer", cx);
        assert!(
            newest.bottom() + px(8.) <= composer.top(),
            "{when}: the bubble ends at {:?}, the composer starts at {:?}",
            newest.bottom(),
            composer.top()
        );
    };
    clear(cx, "on opening");

    // Messages arrive, of every height.
    push_media(
        &harness,
        cx,
        "m-v",
        MediaKind::Voice,
        Some("https://media.example/v"),
        false,
    );
    clear(cx, "after a voice note");
    push_media(
        &harness,
        cx,
        "m-d",
        MediaKind::Document,
        Some("https://media.example/d"),
        false,
    );
    clear(cx, "after a document");

    // The user sends some.
    let composer = cx.update(|cx| harness.shell.read(cx).composer.clone());
    let field: ElementId = ("input", composer.entity_id()).into();
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx)
    })
    .unwrap();
    for text in ["one", "two", "three"] {
        type_text(harness.window, text, cx);
        press(harness.window, "enter", cx);
        clear(cx, "after sending");
    }

    // The composer grows to several lines: the thread gives way, the
    // newest message stays whole above it.
    let before = bounds(harness.window, "composer", cx);
    for line in ["a long", "message", "in", "several", "lines"] {
        type_text(harness.window, line, cx);
        press(harness.window, "shift-enter", cx);
    }
    let grown = bounds(harness.window, "composer", cx);
    assert!(
        grown.size.height > before.size.height + px(40.),
        "{before:?} -> {grown:?}"
    );
    clear(cx, "with a tall composer");
}

// ----- pinning from the chat list -------------------------------------------

#[gpui_kit::test]
fn a_chat_is_pinned_from_its_row_and_stays_pinned(cx: &mut TestAppContext) {
    use client_provider::ProviderError;
    let harness = open(cx, ShellOptions::default());
    // The provider only reports pins it has observed, like wuapi.
    harness.mock.observe_no_chat_state(true);
    let rows = |cx: &mut TestAppContext| cx.update(|cx| chat_rows(harness.shell.read(cx)));
    let before = rows(cx);
    let position = before.iter().rposition(|(_, pinned, _)| !pinned).unwrap();
    let chat = before[position].0.clone();
    let summary = cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .list_rows
            .iter()
            .find_map(|row| match row {
                ListRow::Chat(c) if c.id.as_str() == chat => Some(c.clone()),
                _ => None,
            })
            .unwrap()
    });

    // A right-click on a row opens its menu, where the pointer is.
    let first_id = cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone());
    let first_row = bounds_of(harness.window, &format!("chat-{first_id}"), cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_event(gpui_kit::MouseDownEvent {
        position: gpui_kit::point(px(200.), first_row.center().y),
        modifiers: Modifiers::none(),
        button: gpui_kit::MouseButton::Right,
        click_count: 1,
        first_mouse: false,
    });
    visual.run_until_parked();
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::RowMenu));
    assert!(shows(harness.window, "menu-pin", cx) && shows(harness.window, "menu-archive", cx));
    press(harness.window, "escape", cx);

    // The same menu, for the chat far down the list: pin it.
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_row_menu(summary, gpui_kit::point(px(120.), px(300.)), window, cx)
        })
    })
    .unwrap();
    cx.run_until_parked();
    click(harness.window, "menu-pin", cx);
    let now = rows(cx);
    let moved = now.iter().position(|(id, ..)| *id == chat).unwrap();
    assert!(
        now[moved].1 && moved < position,
        "pinned and moved up: {position} -> {moved}"
    );
    harness.settle(cx);
    let account = cx.update(|cx| harness.shell.read(cx).account.clone().unwrap());
    let chat_id = client_provider::ChatId::new(chat.clone());
    assert!(
        harness.mock.chat(&account, &chat_id).unwrap().pinned,
        "the endpoint was called"
    );

    // The chat list is refreshed (the provider still says nothing about
    // pins): the pin stays where the user put it.
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    let after = rows(cx);
    let still = after.iter().position(|(id, ..)| *id == chat).unwrap();
    assert!(
        after[still].1 && still == moved,
        "still pinned, still on top"
    );

    // A pin the provider refuses is undone with a word of explanation.
    let other = after
        .iter()
        .rfind(|(_, pinned, _)| !pinned)
        .unwrap()
        .0
        .clone();
    harness
        .mock
        .fail_next_chat_updates([ProviderError::Rejected {
            code: "whatsapp_error".into(),
            message: "WhatsApp allows three pinned chats".into(),
        }]);
    let other_id = client_provider::ChatId::new(other.clone());
    harness.engine.update_chat(
        &account,
        &other_id,
        client_provider::ChatChange::Pinned(true),
    );
    harness.settle(cx);
    assert!(shows(harness.window, "problem", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell
            .problem
            .as_deref()
            .unwrap()
            .contains("could not be pinned"));
        let row = chat_rows(shell)
            .into_iter()
            .find(|(id, ..)| *id == other)
            .unwrap();
        assert!(!row.1);
    });
    // The notice goes away by itself.
    cx.executor().advance_clock(Duration::from_secs(9));
    cx.run_until_parked();
    assert!(!shows(harness.window, "problem", cx));
}

// ----- numbers ------------------------------------------------------------

/// One round of the linking screen's wait: the timer, the provider's
/// answer, and the view's reaction.
fn link_round(harness: &Harness, cx: &mut TestAppContext) {
    cx.executor().advance_clock(crate::linking::POLL);
    cx.run_until_parked();
    harness.settle(cx);
}

fn rejected(code: &str, message: &str) -> client_provider::ProviderError {
    client_provider::ProviderError::Rejected {
        code: code.into(),
        message: message.into(),
    }
}

fn stored_accounts(harness: &Harness) -> Vec<client_provider::Account> {
    harness.engine.store().accounts().unwrap()
}

#[gpui_kit::test]
fn a_number_is_linked_from_the_rail_by_scanning(cx: &mut TestAppContext) {
    use super::numbers::LinkStage;
    use client_provider::{HistoryImport, LinkStep};
    let harness = open(cx, ShellOptions::default());
    assert_eq!(stored_accounts(&harness).len(), 2);

    click(harness.window, "add-number", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "add-number", cx));
    // The place defaults to the country of a number already linked.
    cx.update(|cx| {
        let flow = harness.shell.read(cx).linking.as_ref().unwrap();
        assert_eq!(flow.place.as_ref().unwrap().country, "VE");
        assert!(flow.history, "history is imported unless switched off");
    });
    type_text(harness.window, "Support", cx);
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.create_calls(), 1);
    // The number exists at once, in the rail, with its light.
    let accounts = stored_accounts(&harness);
    assert_eq!(accounts.len(), 3);
    let new = accounts[2].clone();
    assert_eq!(new.display_name, "Support");
    assert_eq!(new.settings.history_import, Some(HistoryImport::Recent));
    assert!(!new.connection.is_connected());
    cx.update(|cx| assert_eq!(harness.shell.read(cx).accounts.len(), 3));

    // The code arrives on a later round, and is drawn.
    assert!(!shows(harness.window, "link-qr", cx));
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-qr", cx));
    // Nothing happens for a while: it keeps waiting, quietly.
    link_round(&harness, cx);
    link_round(&harness, cx);
    cx.update(|cx| {
        let flow = harness.shell.read(cx).linking.as_ref().unwrap();
        assert!(matches!(
            &flow.stage,
            LinkStage::Waiting(status) if matches!(status.step, LinkStep::Scan { .. })
        ));
    });

    // The phone scans it.
    harness.mock.complete_link(&new.id);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-done", cx));
    let checks = harness.mock.link_checks();
    link_round(&harness, cx);
    assert_eq!(
        harness.mock.link_checks(),
        checks,
        "nothing is asked after the end"
    );

    click(harness.window, "link-open", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert!(shell.linking.is_none());
        assert_eq!(shell.account.as_ref(), Some(&new.id));
    });
    assert!(stored_accounts(&harness)[2].connection.is_connected());
}

#[gpui_kit::test]
fn a_refusal_to_link_is_said_in_words_and_a_dropped_request_can_be_sent_again(
    cx: &mut TestAppContext,
) {
    use super::numbers::LinkStage;
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "add-number", cx);
    harness.settle(cx);

    // The plan does not allow another number: the provider's words.
    harness.mock.fail_next_links([rejected(
        "upgrade_required",
        "The Free plan includes 1 number. Upgrade to link more.",
    )]);
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "link-error", cx));
    cx.update(|cx| {
        let flow = harness.shell.read(cx).linking.as_ref().unwrap();
        assert_eq!(flow.stage, LinkStage::Form);
        assert_eq!(
            flow.error.as_ref().unwrap().as_ref(),
            "The Free plan includes 1 number. Upgrade to link more."
        );
    });
    assert_eq!(stored_accounts(&harness).len(), 2);

    // The connection dropped: nothing is lost, and it can be sent again.
    harness
        .mock
        .fail_next_links([client_provider::ProviderError::Transient("eof".into())]);
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let flow = harness.shell.read(cx).linking.as_ref().unwrap();
        assert_eq!(flow.stage, LinkStage::Form);
        assert!(flow.error.as_ref().unwrap().contains("Try again"));
    });
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    assert_eq!(stored_accounts(&harness).len(), 3);

    // While waiting, a round that fails is not a failure: it keeps going.
    harness
        .mock
        .fail_next_link_checks([client_provider::ProviderError::Transient("eof".into())]);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-note", cx));
    assert!(!shows(harness.window, "link-stopped", cx));
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-qr", cx));
    assert!(!shows(harness.window, "link-note", cx));
}

#[gpui_kit::test]
fn cancelling_asks_before_deleting_a_number_that_never_linked(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "add-number", cx);
    harness.settle(cx);
    // Before anything is created, leaving is just leaving.
    press(harness.window, "escape", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
    assert_eq!(harness.mock.create_calls(), 0);

    click(harness.window, "add-number", cx);
    harness.settle(cx);
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    link_round(&harness, cx);
    let new = stored_accounts(&harness)[2].id.clone();

    // Escape does not drop it silently, and does not delete it either.
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "link-confirm-cancel", cx));
    assert!(harness.mock.deleted_accounts().is_empty());
    // Escape again: back to waiting.
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "link-confirm-cancel", cx));
    assert!(shows(harness.window, "link-qr", cx));

    click(harness.window, "link-cancel", cx);
    click(harness.window, "link-delete", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.deleted_accounts(), vec![new]);
    assert_eq!(stored_accounts(&harness).len(), 2);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert_eq!(shell.accounts.len(), 2);
    });
    // And nobody is still asking about it.
    let checks = harness.mock.link_checks();
    link_round(&harness, cx);
    assert_eq!(harness.mock.link_checks(), checks);
}

#[gpui_kit::test]
fn a_number_links_with_a_code_and_a_stopped_link_can_be_tried_again(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "add-number", cx);
    harness.settle(cx);
    click(harness.window, "link-method-code", cx);
    // No number yet: said, nothing created.
    click(harness.window, "link-submit", cx);
    assert!(shows(harness.window, "link-error", cx));
    assert_eq!(harness.mock.create_calls(), 0);

    // The place follows the number being typed.
    click(harness.window, "link-phone", cx);
    type_text(harness.window, "+56 9 5550 1234", cx);
    cx.update(|cx| {
        let flow = harness.shell.read(cx).linking.as_ref().unwrap();
        assert_eq!(flow.place.as_ref().unwrap().country, "CL");
    });
    // Until the user chooses one.
    click(harness.window, "link-place-search", cx);
    type_text(harness.window, "miami", cx);
    click(harness.window, "link-place-0", cx);
    cx.update(|cx| {
        let flow = harness.shell.read(cx).linking.as_ref().unwrap();
        assert_eq!(flow.place.as_ref().unwrap().city, "miami");
    });
    click(harness.window, "link-history", cx);

    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-code", cx));
    let new = stored_accounts(&harness)[2].clone();
    assert_eq!(
        new.settings.history_import,
        Some(client_provider::HistoryImport::Off),
        "the switch was turned off"
    );

    // It stops: the reason is said, and trying again brings a new code.
    harness
        .mock
        .stop_link(&new.id, "The number was not linked in time.");
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-stopped", cx));
    click(harness.window, "link-retry", cx);
    harness.settle(cx);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-code", cx));
    harness.mock.complete_link(&new.id);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-done", cx));
    assert_eq!(
        stored_accounts(&harness)[2].phone.as_deref(),
        Some("+56955501234")
    );
}

#[gpui_kit::test]
fn numbers_are_renamed_logged_out_and_linked_again_from_settings(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let first = stored_accounts(&harness)[0].id.clone();
    click(harness.window, "settings", cx);
    // What the server does for the number is said, with the way out.
    assert!(shows(harness.window, "number-history-0", cx));
    assert!(shows(harness.window, "number-server-media", cx));

    // Rename.
    click(harness.window, "number-rename-0", cx);
    assert!(shows(harness.window, "number-action", cx));
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "Ventas", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::Settings);
        assert_eq!(shell.accounts[0].display_name, "Ventas");
    });
    assert_eq!(harness.mock.account(&first).unwrap().display_name, "Ventas");

    // History: off for this number, with the one thing that can be done.
    assert_eq!(
        stored_accounts(&harness)[0].settings.history_import,
        Some(client_provider::HistoryImport::Off)
    );
    click(harness.window, "number-import-0", cx);
    harness.settle(cx);
    assert_eq!(
        harness
            .mock
            .account(&first)
            .unwrap()
            .settings
            .history_import,
        Some(client_provider::HistoryImport::Recent)
    );
    assert!(!shows(harness.window, "number-import-0", cx));
    // Nothing was unlinked for it.
    assert!(harness
        .mock
        .account(&first)
        .unwrap()
        .connection
        .is_connected());

    // Logging out asks first.
    click(harness.window, "number-logout-0", cx);
    assert!(shows(harness.window, "number-action", cx));
    click(harness.window, "number-cancel", cx);
    assert!(harness
        .mock
        .account(&first)
        .unwrap()
        .connection
        .is_connected());
    click(harness.window, "number-logout-0", cx);
    click(harness.window, "number-confirm", cx);
    harness.settle(cx);
    assert_eq!(
        harness.mock.account(&first).unwrap().connection,
        client_provider::ConnectionState::LoggedOut
    );
    assert!(!shows(harness.window, "number-logout-0", cx));

    // Linking it again shows a code; leaving keeps the number.
    click(harness.window, "number-reconnect-0", cx);
    harness.settle(cx);
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-qr", cx));
    click(harness.window, "link-cancel", cx);
    assert!(
        !shows(harness.window, "link-confirm-cancel", cx),
        "it was linked before"
    );
    assert!(harness.mock.deleted_accounts().is_empty());
    assert_eq!(stored_accounts(&harness).len(), 2);
}

#[gpui_kit::test]
fn waiting_for_the_phone_gives_up_after_a_quarter_of_an_hour(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    click(harness.window, "add-number", cx);
    harness.settle(cx);
    click(harness.window, "link-submit", cx);
    harness.settle(cx);
    let rounds = (crate::linking::GIVE_UP.as_secs() / crate::linking::POLL.as_secs()) as usize;
    for _ in 0..rounds - 1 {
        link_round(&harness, cx);
    }
    assert!(shows(harness.window, "link-qr", cx), "still waiting");
    link_round(&harness, cx);
    assert!(shows(harness.window, "link-stopped", cx));
    // It stopped asking, and the number is still there to try again or
    // to delete: nothing was decided for the user.
    let checks = harness.mock.link_checks();
    link_round(&harness, cx);
    assert_eq!(harness.mock.link_checks(), checks);
    assert_eq!(stored_accounts(&harness).len(), 3);
    assert!(shows(harness.window, "link-retry", cx));
}

// ----- contacts -----------------------------------------------------------

#[gpui_kit::test]
fn searching_lists_matching_contacts_and_asks_for_chats_again(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = stored_accounts(&harness)[0].id.clone();
    let mut ana = client_provider::Contact::new(
        account.clone(),
        client_provider::ContactId::new("+584140000009"),
    );
    ana.phone = Some("+584140000009".to_owned());
    ana.saved_name = Some("Ana Quintero".to_owned());
    harness.mock.set_contacts(&account, vec![ana]);
    harness
        .runtime
        .block_on(harness.engine.sync_contacts(&account))
        .unwrap();
    cx.run_until_parked();

    let search = cx.update(|cx| harness.shell.read(cx).search.clone());
    let field: ElementId = ("input", search.entity_id()).into();
    let lists = harness.mock.account_calls();
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx);
        window.input("quintero", cx);
    })
    .unwrap();
    harness.settle(cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell.list_rows.iter().any(|row| matches!(
            row,
            ListRow::Contact(contact) if contact.saved_name.as_deref() == Some("Ana Quintero")
        )));
    });
    // The first letters asked the phone for the chats, once: the next
    // ones within the same half minute do not ask again.
    assert_eq!(harness.mock.account_calls(), lists + 1);
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.input("s", cx);
    })
    .unwrap();
    harness.settle(cx);
    assert_eq!(harness.mock.account_calls(), lists + 1);
}

#[gpui_kit::test]
fn new_chat_lists_the_address_book_and_checks_a_new_number(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = stored_accounts(&harness)[0].id.clone();
    let person = |phone: &str, name: &str| {
        let mut contact =
            client_provider::Contact::new(account.clone(), client_provider::ContactId::new(phone));
        contact.phone = Some(phone.to_owned());
        contact.saved_name = Some(name.to_owned());
        contact
    };

    // Before the address book has been copied: said, and a number can
    // still be typed.
    click(harness.window, "new-chat", cx);
    assert!(shows(harness.window, "new-chat-empty", cx));
    assert!(!shows(harness.window, "new-chat-contact-0", cx));
    press(harness.window, "escape", cx);

    harness.mock.set_contacts(
        &account,
        vec![
            person("+584140000001", "Ana Pérez"),
            person("+584140000002", "Bruno"),
            person("+584140000003", "Carla"),
        ],
    );
    harness
        .runtime
        .block_on(harness.engine.sync_contacts(&account))
        .unwrap();
    cx.run_until_parked();

    click(harness.window, "new-chat", cx);
    assert!(shows(harness.window, "new-chat-contact-2", cx));
    assert!(!shows(harness.window, "new-chat-empty", cx));
    // Typing narrows the list; nothing is asked of the provider.
    let calls = harness.mock.contact_calls();
    type_text(harness.window, "bru", cx);
    assert!(shows(harness.window, "new-chat-contact-0", cx));
    assert!(!shows(harness.window, "new-chat-contact-1", cx));
    assert_eq!(harness.mock.contact_calls(), calls);
    // Enter opens the highlighted contact, without asking anyone.
    press(harness.window, "enter", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        let open = shell.open.as_ref().expect("the conversation is open");
        assert_eq!(open.chat.id.as_str(), "+584140000002");
        assert_eq!(open.chat.title, "Bruno");
    });
    assert_eq!(harness.mock.check_calls(), 0);

    // The arrows walk the rows; a click opens one too.
    click(harness.window, "new-chat", cx);
    press(harness.window, "down", cx);
    press(harness.window, "down", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).new_chat_cursor, 2));
    click(harness.window, "new-chat-contact-0", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.open.as_ref().unwrap().chat.title, "Ana Pérez");
    });

    // A number in nobody's address book: checked first.
    harness.mock.without_whatsapp("+584140009999");
    click(harness.window, "new-chat", cx);
    type_text(harness.window, "+58 414 000 9999", cx);
    assert!(shows(harness.window, "new-chat-open", cx));
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "new-chat-error", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::NewChat, "nothing opened");
        assert_eq!(
            shell.new_chat_error.as_ref().unwrap().as_ref(),
            "+584140009999 is not on WhatsApp."
        );
        assert_eq!(shell.open.as_ref().unwrap().chat.title, "Ana Pérez");
    });
    // A number that is found by its digits is offered as the contact it is.
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "0000003", cx);
    assert!(shows(harness.window, "new-chat-contact-0", cx));
    click(harness.window, "new-chat-contact-0", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.open.as_ref().unwrap().chat.title, "Carla");
    });
}

// ----- interface size -----------------------------------------------------

/// The first unread chat row: its index and id.
fn first_unread(harness: &Harness, cx: &mut TestAppContext) -> (usize, String) {
    cx.update(|cx| {
        chat_rows(harness.shell.read(cx))
            .iter()
            .enumerate()
            .find(|(_, (_, _, unread))| *unread > 0)
            .map(|(index, (id, _, _))| (index, id.clone()))
            .expect("the demo data has an unread chat")
    })
}

/// The bounds of an element whose selector is built at run time.
fn bounds_of(
    window: WindowHandle<gpui_kit::base::Root>,
    selector: &str,
    cx: &mut TestAppContext,
) -> Bounds<gpui_kit::Pixels> {
    let leaked: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    bounds(window, leaked, cx)
}

#[gpui_kit::test]
fn every_interface_size_keeps_the_rows_and_bubbles_in_proportion(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    let harness = open_prepared(cx, ShellOptions::default());
    let (index, id) = first_unread(&harness, cx);

    // A long message, to see where it wraps.
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            let chat = client_provider::ChatId::new(id.clone());
            shell.open_chat(chat, None, cx);
        })
    });
    cx.run_until_parked();
    let account = stored_accounts(&harness)[0].id.clone();
    harness
        .engine
        .send_text(
            &account,
            &client_provider::ChatId::new(id.clone()),
            "A long line that has to wrap inside the pane, whatever the size. ".repeat(12),
            None,
        )
        .unwrap();
    cx.run_until_parked();
    // Back to the list with that chat unread again, so its badge shows.
    harness
        .mock
        .set_unread(&account, &client_provider::ChatId::new(id.clone()), 7);
    press(harness.window, "ctrl-w", cx);
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    let (index, _) = {
        let _ = index;
        first_unread(&harness, cx)
    };

    let mut measured = Vec::new();
    for step in theme::SCALE_STEPS {
        cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
        cx.run_until_parked();
        assert_eq!(cx.update(|_| theme::scale()), f32::from(step) / 100.);

        let row_id = cx.update(|cx| chat_rows(harness.shell.read(cx))[index].0.clone());
        let row = bounds_of(harness.window, &format!("chat-{row_id}"), cx);
        let avatar = bounds_of(harness.window, &format!("row-avatar-{index}"), cx);
        let name = bounds_of(harness.window, &format!("row-name-{index}"), cx);
        let time = bounds_of(harness.window, &format!("row-time-{index}"), cx);
        let badge = bounds_of(harness.window, &format!("row-badge-{index}"), cx);

        // The tokens are what is drawn.
        assert_eq!(
            row.size.height,
            theme::metrics::CHAT_ROW_HEIGHT(),
            "{step}%"
        );
        assert_eq!(avatar.size.width, theme::metrics::AVATAR_LARGE(), "{step}%");
        assert_eq!(
            avatar.size.height,
            theme::metrics::AVATAR_LARGE(),
            "{step}%"
        );
        // Everything inside the row, nothing on top of anything else.
        for (what, part) in [
            ("avatar", avatar),
            ("name", name),
            ("time", time),
            ("badge", badge),
        ] {
            assert!(
                within(part, row),
                "{step}%: {what} {part:?} outside {row:?}"
            );
        }
        assert!(
            avatar.right() <= name.left(),
            "{step}%: the name starts after the avatar"
        );
        assert!(
            name.right() <= time.left(),
            "{step}%: the name stops before the time"
        );
        assert!(
            time.bottom() <= badge.top() + px(1.),
            "{step}%: the badge is under the time"
        );
        assert!(badge.right() <= row.right() && time.right() <= row.right());
        let list = cx.update(|cx| harness.shell.read(cx).list_px);
        assert!(
            row.right() <= theme::metrics::RAIL_WIDTH() + list,
            "{step}%"
        );

        // A bubble wraps inside its pane.
        cx.update(|cx| {
            harness.shell.update(cx, |shell, cx| {
                shell.open_chat(client_provider::ChatId::new(row_id.clone()), None, cx)
            })
        });
        cx.run_until_parked();
        let thread = bounds(harness.window, "thread", cx);
        let bubble = bounds(harness.window, "bubble", cx);
        assert!(
            bubble.left() >= thread.left() && bubble.right() <= thread.right(),
            "{step}%: {bubble:?} outside {thread:?}"
        );
        assert!(bubble.size.width <= theme::metrics::BUBBLE_MAX_WIDTH() + px(1.));
        assert!(
            bubble.size.height > theme::metrics::LINE_BODY() * 3.,
            "{step}%: the long text wrapped"
        );
        press(harness.window, "ctrl-w", cx);
        harness
            .mock
            .set_unread(&account, &client_provider::ChatId::new(row_id.clone()), 7);
        harness.runtime.block_on(harness.engine.refresh()).unwrap();
        cx.run_until_parked();

        measured.push((
            row.size.height.as_f32(),
            avatar.size.height.as_f32() / row.size.height.as_f32(),
        ));
    }
    // Larger at every step, and the same proportions throughout.
    for pair in measured.windows(2) {
        assert!(pair[1].0 > pair[0].0, "{measured:?}");
        assert!((pair[1].1 - pair[0].1).abs() < 0.02, "{measured:?}");
    }
    // The last step chosen is on disk, and is what the next start uses.
    assert_eq!(Settings::load(&file).interface_scale, 125);
    cx.update(|cx| {
        theme::set_scale(100);
        settings::init(Some(file.clone()), Some(Appearance::Light), cx);
        assert_eq!(theme::scale(), 1.25);
    });
}

#[gpui_kit::test]
fn the_interface_size_is_chosen_in_settings_or_from_the_keyboard(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    let harness = open_prepared(cx, ShellOptions::default());
    let row_height = |cx: &mut TestAppContext| {
        let id = cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone());
        bounds_of(harness.window, &format!("chat-{id}"), cx)
            .size
            .height
    };
    let design = row_height(cx);
    assert_eq!(design, px(59.), "the design's row at 100%");

    // From the keyboard, anywhere.
    press(harness.window, "ctrl-=", cx);
    assert_eq!(Settings::load(&file).interface_scale, 110);
    assert!(row_height(cx) > design);
    press(harness.window, "ctrl-=", cx);
    press(harness.window, "ctrl-=", cx);
    assert_eq!(
        Settings::load(&file).interface_scale,
        125,
        "the largest step"
    );
    press(harness.window, "ctrl-0", cx);
    assert_eq!(row_height(cx), design);
    press(harness.window, "ctrl--", cx);
    press(harness.window, "ctrl--", cx);
    press(harness.window, "ctrl--", cx);
    assert_eq!(
        Settings::load(&file).interface_scale,
        80,
        "the smallest step"
    );
    assert!(row_height(cx) < design);

    // In Settings > Appearance, with the panel itself as the preview.
    click(harness.window, "settings", cx);
    click(harness.window, "settings-appearance", cx);
    let small = bounds(harness.window, "settings-panel", cx);
    click(harness.window, "scale-110%", cx);
    let large = bounds(harness.window, "settings-panel", cx);
    assert!(
        large.size.width > small.size.width,
        "the panel grew at once"
    );
    assert_eq!(Settings::load(&file).interface_scale, 110);
    // A hairline stays a hairline.
    assert_eq!(theme::hairline(), px(1.));
}

#[gpui_kit::test]
fn the_chat_list_is_resized_by_its_edge_and_remembers(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    let harness = open_prepared(cx, ShellOptions::default());
    let list = |cx: &mut TestAppContext| cx.update(|cx| harness.shell.read(cx).list_px);
    let rail = theme::metrics::RAIL_WIDTH();
    // By itself: about 30 % of the window, between its limits.
    let default = list(cx);
    assert_eq!(
        default,
        theme::metrics::LIST_WIDTH(),
        "a 1240 px window: the upper limit"
    );

    let drag_to = |x: f32, cx: &mut TestAppContext| {
        let handle = bounds(harness.window, "list-resize", cx);
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_mouse_down(
            handle.center(),
            gpui_kit::MouseButton::Left,
            Modifiers::none(),
        );
        visual.simulate_mouse_move(
            gpui_kit::point(px(x), handle.center().y),
            Some(gpui_kit::MouseButton::Left),
            Modifiers::none(),
        );
        visual.simulate_mouse_up(
            gpui_kit::point(px(x), handle.center().y),
            gpui_kit::MouseButton::Left,
            Modifiers::none(),
        );
        visual.run_until_parked();
    };
    drag_to(53. + 420., cx);
    assert_eq!(list(cx), px(420.));
    assert_eq!(Settings::load(&file).list_width, Some(420));
    // The conversation starts where the list ends.
    let thread = bounds(harness.window, "start", cx);
    assert!(thread.left() >= rail + px(420.));

    // Not narrower or wider than its limits.
    drag_to(60., cx);
    assert_eq!(list(cx), theme::metrics::LIST_MIN());
    drag_to(1200., cx);
    assert_eq!(list(cx), theme::metrics::LIST_MAX());

    // The width is the design's: it scales with the interface.
    drag_to(53. + 300., cx);
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 125));
    cx.run_until_parked();
    assert_eq!(list(cx), px(375.));
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
    cx.run_until_parked();
    // A small window keeps room for the conversation.
    let visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_resize(size(px(820.), px(520.)));
    visual.run_until_parked();
    drag_to(700., cx);
    assert!(list(cx) <= px(820.) - rail - theme::metrics::THREAD_MIN());

    // A double click lets it follow the window again.
    let handle = bounds(harness.window, "list-resize", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_event(gpui_kit::MouseDownEvent {
        button: gpui_kit::MouseButton::Left,
        position: handle.center(),
        modifiers: Modifiers::none(),
        click_count: 2,
        first_mouse: false,
    });
    visual.run_until_parked();
    assert_eq!(Settings::load(&file).list_width, None);
    assert_eq!(
        list(cx),
        theme::metrics::LIST_DEFAULT_MIN(),
        "30% of 820, at its lower limit"
    );
}

// ----- unread counts ------------------------------------------------------

#[gpui_kit::test]
fn an_unread_chat_shows_its_count_and_opening_it_clears_it(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let account = stored_accounts(&harness)[0].id.clone();
    // Read everything, then let the provider say one chat has three.
    click(harness.window, "list-menu", cx);
    click(harness.window, "menu-read-all", cx);
    harness.settle(cx);
    let quiet = cx.update(|cx| chat_rows(harness.shell.read(cx))[2].0.clone());
    let chat = client_provider::ChatId::new(quiet.clone());
    assert!(!shows(harness.window, "unread-badge", cx));
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).unread.get(&account).copied()),
        None
    );

    harness.mock.set_unread(&account, &chat, 3);
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    let index = cx.update(|cx| {
        chat_rows(harness.shell.read(cx))
            .iter()
            .position(|(id, _, unread)| *id == quiet && *unread == 3)
            .expect("the chat is unread, with the provider's count")
    });
    assert!(shows(harness.window, "unread-badge", cx));
    let badge = bounds_of(harness.window, &format!("row-badge-{index}"), cx);
    assert!(
        badge.size.width >= badge.size.height,
        "a pill, not a sliver"
    );
    // The rail counts it for the number.
    assert_eq!(
        cx.update(|cx| harness.shell.read(cx).unread.get(&account).copied()),
        Some(3)
    );
    // And the Unread filter shows it, alone.
    click(harness.window, "filter-unread", cx);
    cx.update(|cx| {
        let rows = chat_rows(harness.shell.read(cx));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].0, quiet);
    });
    click(harness.window, "filter-all", cx);

    // Opening it clears the count here at once, and tells the provider,
    // receipts included.
    let leaked: &'static str = Box::leak(format!("chat-{quiet}").into_boxed_str());
    click(harness.window, leaked, cx);
    cx.update(|cx| {
        let rows = chat_rows(harness.shell.read(cx));
        assert!(
            rows.iter().all(|(_, _, unread)| *unread == 0),
            "cleared at once"
        );
    });
    assert!(!shows(harness.window, "unread-badge", cx));
    harness.settle(cx);
    assert_eq!(harness.mock.marked_read().last(), Some(&chat));
    assert_eq!(harness.mock.receipts_sent().last(), Some(&chat));
    // A refresh does not bring it back.
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    assert!(!shows(harness.window, "unread-badge", cx));

    // With read receipts off the chat is still marked as read, quietly.
    press(harness.window, "escape", cx);
    click(harness.window, "settings", cx);
    click(harness.window, "settings-sync", cx);
    click(harness.window, "read-receipts", cx);
    press(harness.window, "escape", cx);
    assert!(!cx.update(|cx| settings::get(cx).read_receipts));
    let other = cx.update(|cx| chat_rows(harness.shell.read(cx))[4].0.clone());
    let other_chat = client_provider::ChatId::new(other.clone());
    harness.mock.set_unread(&account, &other_chat, 1);
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    let receipts = harness.mock.receipts_sent().len();
    let leaked: &'static str = Box::leak(format!("chat-{other}").into_boxed_str());
    click(harness.window, leaked, cx);
    harness.settle(cx);
    assert_eq!(harness.mock.marked_read().last(), Some(&other_chat));
    assert_eq!(
        harness.mock.receipts_sent().len(),
        receipts,
        "no receipt went out"
    );
}

#[gpui_kit::test]
fn a_number_that_never_linked_is_offered_to_link_or_remove(cx: &mut TestAppContext) {
    use client_provider::{HistoryImport, NewAccount, Provider as _};
    let harness = open(cx, ShellOptions::default());
    // A number created elsewhere that nobody linked in time.
    let new = NewAccount {
        name: Some("Never linked".into()),
        place: None,
        pairing_phone: None,
        history: HistoryImport::Off,
        request_id: "elsewhere".into(),
    };
    let id = harness
        .runtime
        .block_on(harness.mock.create_account(&new))
        .unwrap()
        .account
        .id;
    harness
        .mock
        .stop_link(&id, "The number was not linked in time.");
    harness
        .runtime
        .block_on(harness.mock.link_status(&id))
        .unwrap();
    // One of the two linked numbers was deleted elsewhere meanwhile.
    let second = stored_accounts(&harness)[1].id.clone();
    harness
        .runtime
        .block_on(harness.mock.delete_account(&second))
        .unwrap();
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.accounts.len(), 2, "the deleted one is gone");
        assert!(shell.accounts[1].never_linked());
        assert!(!shell.accounts[0].never_linked());
    });
    // Nothing to show for it yet: it is not in the rail.
    assert!(shows(harness.window, "rail-account-0", cx));
    assert!(!shows(harness.window, "rail-account-1", cx));

    click(harness.window, "settings", cx);
    // Not a broken session: it says what it is, with the two things to do.
    assert!(shows(harness.window, "number-unlinked-1", cx));
    assert!(shows(harness.window, "number-link-1", cx));
    assert!(!shows(harness.window, "number-logout-1", cx));
    assert!(!shows(harness.window, "number-unlinked-0", cx));

    // Further down the panel than fits: scrolled to, like anyone would.
    let scroll_down = |cx: &mut TestAppContext| {
        let panel = bounds(harness.window, "settings-panel", cx);
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        visual.simulate_event(gpui_kit::ScrollWheelEvent {
            position: panel.center(),
            delta: gpui_kit::ScrollDelta::Pixels(gpui_kit::point(px(0.), px(-600.))),
            ..Default::default()
        });
        visual.run_until_parked();
    };
    scroll_down(cx);
    // Remove asks, then deletes.
    click(harness.window, "number-remove-1", cx);
    assert!(shows(harness.window, "number-action", cx));
    click(harness.window, "number-cancel", cx);
    assert_eq!(harness.mock.deleted_accounts().len(), 1);
    scroll_down(cx);
    click(harness.window, "number-remove-1", cx);
    click(harness.window, "number-confirm", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.deleted_accounts(), vec![second, id]);
    assert_eq!(stored_accounts(&harness).len(), 1);
    assert!(!shows(harness.window, "number-unlinked-1", cx));
}

#[gpui_kit::test]
fn a_text_field_is_as_tall_as_its_line(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    // The component library would give it two rems less sixteen pixels,
    // which is less than a line at this text size: the text was cut.
    let field = bounds(harness.window, "chat-search-field", cx);
    let line = crate::theme::metrics::TEXT_BODY() * 1.25;
    assert!(
        field.size.height >= line,
        "{:?} < {line:?}",
        field.size.height
    );
    assert!(field.size.height < line + px(1.), "{:?}", field.size.height);
}

// ----- the rail -------------------------------------------------------------

fn right_click(
    window: WindowHandle<gpui_kit::base::Root>,
    selector: &str,
    cx: &mut TestAppContext,
) {
    let at = bounds_of(window, selector, cx).center();
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_event(gpui_kit::MouseDownEvent {
        position: at,
        modifiers: Modifiers::none(),
        button: gpui_kit::MouseButton::Right,
        click_count: 1,
        first_mouse: false,
    });
    visual.run_until_parked();
}

/// The rail as text: `a [b c]`, by account id.
fn rail_shape(harness: &Harness, cx: &mut TestAppContext) -> String {
    use crate::rail::RailItem;
    cx.update(|cx| {
        harness
            .shell
            .read(cx)
            .rail
            .items
            .iter()
            .map(|item| match item {
                RailItem::Account(key) => key.trim_start_matches("mock:acc_").to_owned(),
                RailItem::Group(group) => format!(
                    "[{}]",
                    group
                        .members
                        .iter()
                        .map(|key| key.trim_start_matches("mock:acc_"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            })
            .collect::<Vec<_>>()
            .join(" ")
    })
}

#[gpui_kit::test]
fn the_rails_menu_renames_looks_mutes_and_reads(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    // A file dialog that answers with a picture on disk.
    let picture = dir.path().join("icon.png");
    image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 30, 30, 255]))
        .save(&picture)
        .unwrap();
    let picked = picture.clone();
    let harness = open_prepared(
        cx,
        ShellOptions {
            pick_files: Some(std::rc::Rc::new(move |_, _| {
                gpui_kit::Task::ready(Some(vec![picked.clone()]))
            })),
            ..Default::default()
        },
    );
    let accounts = stored_accounts(&harness);
    let (first, second) = (accounts[0].id.clone(), accounts[1].id.clone());
    let key = |account: &client_provider::AccountId| crate::rail::key("mock", account.as_str());
    assert_eq!(rail_shape(&harness, cx), "personal work");
    // The header of the chat list says whose chats these are.
    assert!(shows(harness.window, "list-account", cx));

    // A right-click opens the number's menu; Escape closes it.
    right_click(harness.window, "rail-account-0", cx);
    assert!(shows(harness.window, "rail-menu", cx));
    assert!(shows(harness.window, "rail-rename", cx) && shows(harness.window, "rail-logout", cx));
    assert!(!shows(harness.window, "rail-link", cx), "it is linked");
    press(harness.window, "escape", cx);
    assert!(!shows(harness.window, "rail-menu", cx));

    // Rename: the provider's name, through the card that Settings uses.
    right_click(harness.window, "rail-account-0", cx);
    click(harness.window, "rail-rename", cx);
    assert!(shows(harness.window, "number-action", cx));
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "Ventas", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.account(&first).unwrap().display_name, "Ventas");
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(
            shell.overlay,
            Overlay::None,
            "back to the rail, not to Settings"
        );
        assert_eq!(shell.current_account_name().as_deref(), Some("Ventas"));
    });
    // The provider refuses a name: it is kept here instead.
    harness
        .mock
        .fail_next_account_updates([rejected("forbidden", "This key cannot rename numbers.")]);
    right_click(harness.window, "rail-account-0", cx);
    click(harness.window, "rail-rename", cx);
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "Mine", cx);
    press(harness.window, "enter", cx);
    harness.settle(cx);
    assert_eq!(harness.mock.account(&first).unwrap().display_name, "Ventas");
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.current_account_name().as_deref(), Some("Mine"));
        assert_eq!(shell.rail.look(&key(&first)).label.as_deref(), Some("Mine"));
    });

    // Icon and colour: an emoji on a colour of the palette.
    right_click(harness.window, "rail-account-0", cx);
    click(harness.window, "rail-look", cx);
    assert!(shows(harness.window, "rail-edit", cx));
    type_text(harness.window, "VE", cx);
    click(harness.window, "rail-colour-3", cx);
    click(harness.window, "rail-edit-done", cx);
    cx.update(|cx| {
        let look = harness.shell.read(cx).rail.look(&key(&first));
        assert_eq!(look.glyph.as_deref(), Some("VE"));
        assert_eq!(look.colour, Some(3));
        assert!(!look.image);
    });
    // Or a picture, through the file dialog: kept in the store.
    right_click(harness.window, "rail-account-0", cx);
    click(harness.window, "rail-look", cx);
    click(harness.window, "rail-edit-image", cx);
    for _ in 0..2000 {
        harness.settle(cx);
        if cx.update(|cx| harness.shell.read(cx).rail.look(&key(&first)).image) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    cx.update(|cx| assert!(harness.shell.read(cx).rail.look(&key(&first)).image));
    let icon = harness
        .engine
        .store()
        .avatar(
            &first,
            &client_provider::ChatId::new(super::rail::ICON_SUBJECT),
        )
        .unwrap()
        .expect("the picture is in the store");
    assert!(icon.image.is_some());
    click(harness.window, "rail-edit-reset", cx);
    cx.update(|cx| {
        let look = harness.shell.read(cx).rail.look(&key(&first));
        assert!(!look.image && look.glyph.is_none() && look.colour.is_none());
        assert_eq!(
            look.label.as_deref(),
            Some("Mine"),
            "the name is another matter"
        );
    });

    // Mute, for this number, here.
    right_click(harness.window, "rail-account-0", cx);
    click(harness.window, "rail-mute", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).rail.look(&key(&first)).muted));

    // Mark all as read: every unread chat of the number.
    let unread = |cx: &mut TestAppContext, account: &client_provider::AccountId| {
        cx.update(|cx| {
            harness
                .shell
                .read(cx)
                .unread
                .get(account)
                .copied()
                .unwrap_or(0)
        })
    };
    assert!(unread(cx, &second) > 0, "the demo data has unread chats");
    right_click(harness.window, "rail-account-1", cx);
    click(harness.window, "rail-read", cx);
    harness.settle(cx);
    assert_eq!(unread(cx, &second), 0);
    right_click(harness.window, "rail-account-1", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let read = shell
            .rail_entries()
            .into_iter()
            .find(|entry| entry.id == "rail-read")
            .unwrap();
        assert!(
            read.action.is_none(),
            "nothing left to read: said, not hidden"
        );
    });
    // The WhatsApp profile: its own screen, reached from here.
    click(harness.window, "rail-profile", cx);
    assert!(shows(harness.window, "own-profile", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::OwnProfile);
        assert_eq!(
            shell.social.own.as_ref().map(|own| &own.account),
            Some(&second),
            "the profile of the number the menu was opened on"
        );
    });
    // Escape closes it: it did not come from Settings, so it does not go
    // back there.
    press(harness.window, "escape", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));

    // All of it is on disk, by provider and account.
    let saved = crate::rail::RailLayout::load(&dir.path().join(crate::rail::FILE_NAME));
    assert!(saved.look(&key(&first)).muted);
    assert_eq!(saved.look(&key(&first)).label.as_deref(), Some("Mine"));
}

#[gpui_kit::test]
fn numbers_are_grouped_reordered_and_rolled_up_from_the_menu_and_the_keyboard(
    cx: &mut TestAppContext,
) {
    use client_provider::{HistoryImport, NewAccount, Provider as _};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(settings::FILE_NAME);
    cx.update(|cx| prepare(cx, Some(file.clone())));
    let harness = open_prepared(cx, ShellOptions::default());
    // A third number, linked meanwhile.
    let new = NewAccount {
        name: Some("Third".into()),
        place: None,
        pairing_phone: None,
        history: HistoryImport::Off,
        request_id: "third".into(),
    };
    let third = harness
        .runtime
        .block_on(harness.mock.create_account(&new))
        .unwrap()
        .account
        .id;
    // Until a phone is linked to it, it is not in the rail.
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    assert_eq!(rail_shape(&harness, cx), "personal work");
    harness.mock.complete_link(&third);
    harness
        .runtime
        .block_on(harness.mock.link_status(&third))
        .unwrap();
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    assert_eq!(rail_shape(&harness, cx), "personal work new_1");
    right_click(harness.window, "rail-account-2", cx);

    // A group, from the menu: this number with another.
    click(harness.window, "rail-group-with-0", cx);
    assert_eq!(rail_shape(&harness, cx), "[personal new_1] work");
    let group = cx.update(|cx| harness.shell.read(cx).rail.groups()[0].clone());
    assert_eq!(group.name, "Group");
    assert!(group.expanded);
    let head: &'static str = Box::leak(format!("rail-group-{}", group.id).into_boxed_str());
    let open: &'static str = Box::leak(format!("rail-group-open-{}", group.id).into_boxed_str());
    assert!(shows(harness.window, open, cx));
    // Its members are inside its container.
    let container = bounds(harness.window, open, cx);
    assert!(within(
        bounds(harness.window, "rail-account-0", cx),
        container
    ));
    assert!(within(
        bounds(harness.window, "rail-account-2", cx),
        container
    ));
    assert!(!within(
        bounds(harness.window, "rail-account-1", cx),
        container
    ));

    // A click closes it to one tile, with the unread of its members on it.
    let accounts = stored_accounts(&harness);
    let total = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        shell.unread.get(&accounts[0].id).copied().unwrap_or(0)
            + shell.unread.get(&third).copied().unwrap_or(0)
    });
    assert!(total > 0);
    click(harness.window, head, cx);
    assert!(!shows(harness.window, open, cx));
    assert!(
        !shows(harness.window, "rail-account-0", cx),
        "inside the closed group"
    );
    let badge: &'static str = Box::leak(format!("rail-badge-{total}").into_boxed_str());
    assert!(
        shows(harness.window, badge, cx),
        "the group shows its members' unread"
    );
    click(harness.window, head, cx);
    assert!(shows(harness.window, open, cx));

    // The group's own menu: a name, a colour.
    right_click(harness.window, head, cx);
    click(harness.window, "rail-group-edit", cx);
    press(harness.window, "ctrl-a", cx);
    type_text(harness.window, "Work numbers", cx);
    click(harness.window, "rail-colour-4", cx);
    press(harness.window, "enter", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let group = shell.rail.groups()[0];
        assert_eq!((group.name.as_str(), group.colour), ("Work numbers", 4));
        assert_eq!(shell.overlay, Overlay::None);
    });

    // The other number joins, moves within, and leaves, from its menu.
    right_click(harness.window, "rail-account-1", cx);
    let join: &'static str = Box::leak(format!("rail-move-{}", group.id).into_boxed_str());
    click(harness.window, join, cx);
    assert_eq!(rail_shape(&harness, cx), "[personal new_1 work]");
    right_click(harness.window, "rail-account-1", cx);
    click(harness.window, "rail-up", cx);
    assert_eq!(rail_shape(&harness, cx), "[personal work new_1]");
    right_click(harness.window, "rail-account-1", cx);
    click(harness.window, "rail-leave-group", cx);
    assert_eq!(rail_shape(&harness, cx), "[personal new_1] work");

    // The keyboard: Alt with an arrow moves the focused item, and the menu
    // key opens its menu.
    let stroke = |keys: &str| gpui_kit::KeyDownEvent {
        keystroke: gpui_kit::Keystroke::parse(keys).unwrap(),
        is_held: false,
        prefer_character_input: false,
    };
    let work = accounts[1].id.clone();
    cx.update_window(harness.window.into(), |_, window, cx| {
        harness.shell.update(cx, |shell, cx| {
            let target = super::rail_menu::RailTarget::Account(work.clone());
            shell.rail_key_on(target.clone(), &stroke("alt-up"), window, cx);
            assert_eq!(shell.rail.items.len(), 2);
            shell.rail_key_on(target, &stroke("shift-f10"), window, cx);
            assert_eq!(shell.overlay, Overlay::RailMenu);
        })
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(rail_shape(&harness, cx), "work [personal new_1]");
    // Arrows and Enter in the menu.
    press(harness.window, "end", cx);
    press(harness.window, "escape", cx);

    // The order and the group are what the next start finds.
    let saved = crate::rail::RailLayout::load(&dir.path().join(crate::rail::FILE_NAME));
    cx.update(|cx| assert_eq!(saved, harness.shell.read(cx).rail));
    let again = open_prepared(cx, ShellOptions::default());
    // (The third number does not exist in that session: it is gone from
    // the rail, and a group of one is no group.)
    assert_eq!(rail_shape(&again, cx), "work personal");

    // Ungroup puts the numbers back where the group was.
    right_click(harness.window, head, cx);
    click(harness.window, "rail-ungroup", cx);
    assert_eq!(rail_shape(&harness, cx), "work personal new_1");
}

#[gpui_kit::test]
fn a_number_is_dragged_between_onto_and_out_and_a_click_is_still_a_click(cx: &mut TestAppContext) {
    let harness = open(cx, ShellOptions::default());
    let accounts = stored_accounts(&harness);
    assert_eq!(rail_shape(&harness, cx), "personal work");
    let centre = |selector: &'static str, cx: &mut TestAppContext| {
        bounds(harness.window, selector, cx).center()
    };
    let left = gpui_kit::MouseButton::Left;

    // A click, with the tremble of a hand: the number is selected, and
    // nothing moves.
    let on_second = centre("rail-account-1", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(on_second, left, Modifiers::none());
    visual.simulate_mouse_move(
        on_second + gpui_kit::point(px(1.), px(2.)),
        Some(left),
        Modifiers::none(),
    );
    visual.simulate_mouse_up(
        on_second + gpui_kit::point(px(1.), px(2.)),
        left,
        Modifiers::none(),
    );
    visual.run_until_parked();
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.account.as_ref(), Some(&accounts[1].id));
        assert!(shell.rail_drag.dragging().is_none());
    });
    assert_eq!(rail_shape(&harness, cx), "personal work");

    // Dragged above the first number: the line shows where, and it lands
    // there. The number that was on screen stays the one on screen.
    let first = bounds(harness.window, "rail-account-0", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(on_second, left, Modifiers::none());
    let above = gpui_kit::point(first.center().x, first.top() + px(2.));
    visual.simulate_mouse_move(above, Some(left), Modifiers::none());
    visual.run_until_parked();
    assert!(visual.debug_bounds("rail-insertion").is_some());
    visual.simulate_mouse_up(above, left, Modifiers::none());
    visual.run_until_parked();
    assert_eq!(rail_shape(&harness, cx), "work personal");
    cx.update(|cx| {
        assert_eq!(
            harness.shell.read(cx).account.as_ref(),
            Some(&accounts[1].id)
        );
    });

    // Escape in the middle of a drag: nothing moves.
    let from = centre("rail-account-0", cx);
    let onto = centre("rail-account-1", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(from, left, Modifiers::none());
    visual.simulate_mouse_move(onto, Some(left), Modifiers::none());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("rail-insertion").is_none(),
        "onto, not between"
    );
    // The target shows the group the two would make: a tile, both faces.
    let preview = visual.debug_bounds("rail-preview").expect("the tile to be");
    for face in ["rail-mini-0", "rail-mini-1"] {
        let face = visual.debug_bounds(face).expect("both faces");
        assert!(within(face, preview));
    }
    press(harness.window, "escape", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_up(onto, left, Modifiers::none());
    visual.run_until_parked();
    assert_eq!(rail_shape(&harness, cx), "work personal");

    // Dropped onto the middle of another number: the two are a group.
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(from, left, Modifiers::none());
    visual.simulate_mouse_move(onto, Some(left), Modifiers::none());
    visual.simulate_mouse_up(onto, left, Modifiers::none());
    visual.run_until_parked();
    // Where the target was, the target first.
    assert_eq!(rail_shape(&harness, cx), "[work personal]");

    // Dragged out, below the group: one number is left, so the group
    // dissolves.
    let member = centre("rail-account-0", cx);
    let numbers = bounds(harness.window, "rail-numbers", cx);
    let below = gpui_kit::point(member.x, numbers.bottom() - px(40.));
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(member, left, Modifiers::none());
    visual.simulate_mouse_move(below, Some(left), Modifiers::none());
    visual.simulate_mouse_up(below, left, Modifiers::none());
    visual.run_until_parked();
    assert_eq!(rail_shape(&harness, cx), "work personal");
    cx.update(|cx| assert!(harness.shell.read(cx).rail.groups().is_empty()));
}

// ----- attachments ----------------------------------------------------------

fn sheet_open(shell: &Shell) -> bool {
    shell.attach.is_some()
}

/// How many of the user's own media bubbles the open chat shows.
fn own_media(shell: &Shell) -> usize {
    shell.open.as_ref().map_or(0, |open| {
        open.rows
            .iter()
            .filter(|row| match row {
                Row::Message(row) => {
                    matches!(row.stored.message.content, MessageContent::Media(_))
                        && row.stored.message.direction == Direction::Outgoing
                        && row.stored.message.client_id.is_some()
                }
                _ => false,
            })
            .count()
    })
}

/// A harness with a chat open, files on disk to attach, and a file dialog
/// that answers with them.
fn open_for_attaching(
    cx: &mut TestAppContext,
    dir: &std::path::Path,
) -> (Harness, std::path::PathBuf, std::path::PathBuf) {
    let picture = dir.join("photo.png");
    image::RgbaImage::from_pixel(320, 160, image::Rgba([30, 120, 200, 255]))
        .save(&picture)
        .unwrap();
    let document = dir.join("report.pdf");
    std::fs::write(&document, [b"%PDF-1.7 ".as_slice(), &[7; 3000]].concat()).unwrap();
    let picked = vec![picture.clone(), document.clone()];
    let harness = open(
        cx,
        ShellOptions {
            pick_files: Some(std::rc::Rc::new(move |_, _| {
                gpui_kit::Task::ready(Some(picked.clone()))
            })),
            ..Default::default()
        },
    );
    // The provider says whether files can be sent.
    harness.settle(cx);
    let first = cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_chat(client_provider::ChatId::new(first), None, cx)
        })
    });
    cx.run_until_parked();
    (harness, picture, document)
}

fn focus_composer(harness: &Harness, cx: &mut TestAppContext) {
    let composer = cx.update(|cx| harness.shell.read(cx).composer.clone());
    let field: ElementId = ("input", composer.entity_id()).into();
    cx.update_window(harness.window.into(), |_, window, cx| {
        window.click(field, cx)
    })
    .unwrap();
    cx.run_until_parked();
}

/// The newest media bubble of the open chat: (kind, status, caption).
fn newest_media(
    harness: &Harness,
    cx: &mut TestAppContext,
) -> Vec<(client_provider::MediaKind, DeliveryStatus, Option<String>)> {
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let open = shell.open.as_ref().unwrap();
        open.rows
            .iter()
            .filter_map(|row| match row {
                Row::Message(row) => match &row.stored.message.content {
                    MessageContent::Media(media)
                        if row.stored.message.direction == Direction::Outgoing
                            && row.stored.message.client_id.is_some() =>
                    {
                        Some((
                            media.kind,
                            row.stored.message.status.clone(),
                            media.caption.clone(),
                        ))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect()
    })
}

#[gpui_kit::test]
fn files_are_picked_previewed_and_sent_through_the_outbox(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let dir = tempfile::tempdir().unwrap();
    let (harness, _, _) = open_for_attaching(cx, dir.path());

    // The "+" opens the file dialog; what it answers is shown first.
    click(harness.window, "attach", cx);
    until(&harness, cx, sheet_open);
    assert!(shows(harness.window, "attach-sheet", cx));
    assert!(
        shows(harness.window, "attach-item-0", cx) && shows(harness.window, "attach-item-1", cx)
    );
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert_eq!(draft.items[0].file.mime, "image/png");
        assert_eq!(draft.items[1].file.mime, "application/pdf");
        assert_eq!(harness.shell.read(cx).overlay, Overlay::AttachSheet);
    });
    assert_eq!(harness.mock.upload_calls(), 0, "nothing goes before Send");
    // Everything inside the sheet.
    let sheet = bounds(harness.window, "attach-sheet", cx);
    for part in [
        "attach-item-0",
        "attach-item-1",
        "attach-caption",
        "attach-send",
        "attach-total",
    ] {
        assert!(within(bounds(harness.window, part, cx), sheet), "{part}");
    }

    // A caption, typed where the keyboard already is; Enter sends.
    type_text(harness.window, "the two files", cx);
    press(harness.window, "enter", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.overlay, Overlay::None);
        assert!(shell.attach.is_none());
    });
    until(&harness, cx, |shell| own_media(shell) == 2);
    // Two bubbles at once, in order, pending, drawn from the copy here:
    // the picture as an image with the caption, the other as a document.
    let sent = newest_media(&harness, cx);
    assert_eq!(sent.len(), 2);
    assert_eq!(
        sent[0],
        (
            MediaKind::Image,
            DeliveryStatus::Pending,
            Some("the two files".into())
        )
    );
    assert_eq!(
        sent[1],
        (MediaKind::Document, DeliveryStatus::Pending, None)
    );
    assert!(
        shows(harness.window, "media-image", cx),
        "the picture shows before any upload"
    );
    assert!(shows(harness.window, "sending-line", cx));
    assert_eq!(harness.mock.upload_calls(), 0);
    // Inside its bubble.
    let bubble = bounds(harness.window, "bubble", cx);
    assert!(within(bounds(harness.window, "sending-line", cx), bubble));

    // The outbox runs: uploaded, sent, and the bubbles keep working.
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(pass.sent, 2);
    assert_eq!(harness.mock.uploaded_count(), 2);
    let sent = newest_media(&harness, cx);
    assert!(sent
        .iter()
        .all(|(_, status, _)| *status == DeliveryStatus::Sent));
    assert!(!shows(harness.window, "sending-line", cx));
    assert!(shows(harness.window, "media-image", cx));
}

#[gpui_kit::test]
fn the_sheet_removes_files_sends_as_documents_and_cancels(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let dir = tempfile::tempdir().unwrap();
    let (harness, _, _) = open_for_attaching(cx, dir.path());

    // Escape: two files are worth a question; the next Escape means it.
    // Nothing is sent.
    click(harness.window, "attach", cx);
    until(&harness, cx, sheet_open);
    press(harness.window, "escape", cx);
    assert!(shows(harness.window, "attach-discard-confirm", cx));
    cx.update(|cx| assert!(harness.shell.read(cx).attach.is_some()));
    press(harness.window, "escape", cx);
    cx.update(|cx| assert!(harness.shell.read(cx).attach.is_none()));
    assert!(newest_media(&harness, cx).is_empty());

    // One file taken out; the picture sent as a document.
    click(harness.window, "attach", cx);
    until(&harness, cx, sheet_open);
    click(harness.window, "attach-remove-1", cx);
    assert!(!shows(harness.window, "attach-item-1", cx));
    click(harness.window, "attach-as-document", cx);
    click(harness.window, "attach-send", cx);
    until(&harness, cx, |shell| own_media(shell) == 1);
    let sent = newest_media(&harness, cx);
    assert_eq!(sent[0].0, MediaKind::Document);

    // The upload does not get through; the bubble offers to take it back.
    harness
        .mock
        .fail_next_uploads([client_provider::ProviderError::Transient("eof".into())]);
    harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        newest_media(&harness, cx)[0].1,
        DeliveryStatus::Pending,
        "not a failure"
    );
    click(harness.window, "sending-cancel", cx);
    assert!(newest_media(&harness, cx).is_empty(), "the bubble is gone");
    let pass = harness
        .runtime
        .block_on(harness.engine.flush_outbox())
        .unwrap();
    assert_eq!(pass.sent, 0);
    assert_eq!(harness.mock.uploaded_count(), 0);

    // The last file out closes the sheet.
    click(harness.window, "attach", cx);
    until(&harness, cx, sheet_open);
    click(harness.window, "attach-remove-0", cx);
    click(harness.window, "attach-remove-0", cx);
    cx.update(|cx| assert_eq!(harness.shell.read(cx).overlay, Overlay::None));
}

#[gpui_kit::test]
fn a_paste_is_a_picture_files_or_text(cx: &mut TestAppContext) {
    use gpui_kit::{ClipboardEntry, ClipboardItem, ExternalPaths, Image, ImageFormat};
    let dir = tempfile::tempdir().unwrap();
    let (harness, picture, document) = open_for_attaching(cx, dir.path());
    let screenshot = std::fs::read(&picture).unwrap();

    // Text pastes as text, into the composer.
    focus_composer(&harness, cx);
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("just words".into())));
    press(harness.window, "ctrl-v", cx);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(shell.composer.read(cx).value().as_ref(), "just words");
        assert!(shell.attach.is_none());
    });

    // A screenshot: a picture, shown in the sheet, not pasted as anything.
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem {
            entries: vec![ClipboardEntry::Image(Image::from_bytes(
                ImageFormat::Png,
                screenshot.clone(),
            ))],
        })
    });
    press(harness.window, "ctrl-v", cx);
    assert!(shows(harness.window, "attach-sheet", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let draft = shell.attach.as_ref().unwrap();
        assert_eq!(draft.items.len(), 1);
        assert_eq!(draft.items[0].file.name, "pasted-image.png");
        assert_eq!(draft.items[0].file.bytes.as_slice(), screenshot.as_slice());
        assert_eq!(shell.composer.read(cx).value().as_ref(), "just words");
    });
    press(harness.window, "escape", cx);

    // Files copied in a file manager: read from the disk, into the sheet.
    focus_composer(&harness, cx);
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem {
            entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(
                vec![document.clone()].into(),
            ))],
        })
    });
    press(harness.window, "ctrl-v", cx);
    until(&harness, cx, sheet_open);
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert_eq!(draft.items.len(), 1);
        assert_eq!(draft.items[0].file.name, "report.pdf");
    });
    press(harness.window, "escape", cx);

    // The same on Linux, where the copy arrives as an address in text.
    focus_composer(&harness, cx);
    let address = format!("file://{}", picture.display());
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(address)));
    press(harness.window, "ctrl-v", cx);
    until(&harness, cx, sheet_open);
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert_eq!(
            shell.attach.as_ref().unwrap().items[0].file.name,
            "photo.png"
        );
        assert_eq!(
            shell.composer.read(cx).value().as_ref(),
            "just words",
            "the address was not pasted as text"
        );
    });
}

#[gpui_kit::test]
fn files_dropped_on_the_conversation_open_the_sheet(cx: &mut TestAppContext) {
    use gpui_kit::{ExternalPaths, FileDropEvent};
    let dir = tempfile::tempdir().unwrap();
    let (harness, picture, document) = open_for_attaching(cx, dir.path());
    let over = bounds(harness.window, "thread", cx).center();
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_event(FileDropEvent::Entered {
        position: over,
        paths: ExternalPaths(vec![picture, document].into()),
    });
    visual.simulate_event(FileDropEvent::Pending { position: over });
    visual.simulate_event(FileDropEvent::Submit { position: over });
    visual.run_until_parked();
    until(&harness, cx, sheet_open);
    assert!(shows(harness.window, "attach-sheet", cx));
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert_eq!(draft.items.len(), 2);
    });
}

#[gpui_kit::test]
fn a_file_too_large_is_flagged_and_holds_back_only_itself(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (harness, _, _) = open_for_attaching(cx, dir.path());
    // The provider takes at most 2000 bytes: the document is 3009.
    harness.mock.set_upload_limit(2000);
    click(harness.window, "attach", cx);
    until(&harness, cx, sheet_open);
    assert!(shows(harness.window, "attach-sheet", cx));
    cx.update(|cx| {
        let draft = harness.shell.read(cx).attach.as_ref().unwrap();
        assert_eq!(draft.items.len(), 2, "both are shown");
        assert!(draft.items[0].blocked.is_none());
        let why = draft.items[1].blocked.as_ref().expect("flagged");
        assert!(why.contains("Too large"), "{why}");
    });
    assert!(shows(harness.window, "attach-blocked-mark-1", cx));
    click(harness.window, "attach-item-1", cx);
    assert!(shows(harness.window, "attach-blocked", cx));
    assert!(
        !shows(harness.window, "attach-caption", cx),
        "no caption for a file that cannot go"
    );
    click(harness.window, "attach-send", cx);
    until(&harness, cx, |shell| own_media(shell) == 1);
    assert_eq!(harness.mock.upload_calls(), 0);
}

#[gpui_kit::test]
fn where_files_cannot_be_sent_the_button_says_so_and_a_paste_is_not_lost_in_silence(
    cx: &mut TestAppContext,
) {
    use gpui_kit::{ClipboardEntry, ClipboardItem, Image, ImageFormat};
    let dir = tempfile::tempdir().unwrap();
    let picture = dir.path().join("photo.png");
    image::RgbaImage::from_pixel(8, 8, image::Rgba([1, 2, 3, 255]))
        .save(&picture)
        .unwrap();
    cx.update(|cx| prepare(cx, None));
    let harness = open_prepared(cx, ShellOptions::default());
    // A backend that does not have uploads yet.
    harness.mock.set_uploads_available(false);
    harness.engine.check_uploads();
    harness.settle(cx);
    let first = cx.update(|cx| chat_rows(harness.shell.read(cx))[0].0.clone());
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            shell.open_chat(client_provider::ChatId::new(first), None, cx)
        })
    });
    cx.run_until_parked();
    cx.update(|cx| assert!(!harness.shell.read(cx).can_attach()));

    // The button is there, and does nothing but say why.
    click(harness.window, "attach", cx);
    harness.settle(cx);
    assert!(!shows(harness.window, "attach-sheet", cx));
    // A pasted picture is not dropped silently, and nothing is queued.
    focus_composer(&harness, cx);
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem {
            entries: vec![ClipboardEntry::Image(Image::from_bytes(
                ImageFormat::Png,
                std::fs::read(&picture).unwrap(),
            ))],
        })
    });
    press(harness.window, "ctrl-v", cx);
    assert!(!shows(harness.window, "attach-sheet", cx));
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        assert!(shell
            .problem
            .as_ref()
            .unwrap()
            .contains("not available yet"));
    });
    // Text still pastes.
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string("words".into())));
    press(harness.window, "ctrl-v", cx);
    cx.update(|cx| {
        assert_eq!(
            harness.shell.read(cx).composer.read(cx).value().as_ref(),
            "words"
        );
    });
}

mod animated;
mod attach_files;
mod communities;
mod connection;
mod design;
mod emoji;
mod forward;
mod keyboard;
mod link_dialog;
mod media_out;
mod message_types;
mod palette;
mod panes;
mod people;
mod rail_marks;
mod reactions;
mod social;
mod status;
mod status_ways;
mod stickers;
mod together;
mod transport;
mod updates;
mod viewer;
mod voice_note;

// ----- the rail's layout and motion -----------------------------------------

/// A harness with `extra` more numbers than the demo's two.
fn open_with_numbers(cx: &mut TestAppContext, extra: usize) -> Harness {
    use client_provider::{HistoryImport, NewAccount, Provider as _};
    let harness = open_prepared(cx, ShellOptions::default());
    for index in 0..extra {
        let new = NewAccount {
            name: Some(format!("Number {index}")),
            place: None,
            pairing_phone: None,
            history: HistoryImport::Off,
            request_id: format!("extra-{index}"),
        };
        let id = harness
            .runtime
            .block_on(harness.mock.create_account(&new))
            .unwrap()
            .account
            .id;
        harness.mock.complete_link(&id);
        harness
            .runtime
            .block_on(harness.mock.link_status(&id))
            .unwrap();
    }
    harness.runtime.block_on(harness.engine.refresh()).unwrap();
    cx.run_until_parked();
    harness
}

/// Puts the first `members` numbers in one closed group, the rest alone.
fn group_first(harness: &Harness, members: usize, cx: &mut TestAppContext) -> u32 {
    use crate::rail::{Dragged, Drop};
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            let ids: Vec<u32> = shell.rail.groups().iter().map(|group| group.id).collect();
            let keys: Vec<String> = shell
                .accounts
                .iter()
                .map(|account| shell.rail_key(&account.id))
                .collect();
            shell.change_rail(
                |rail| {
                    for id in ids {
                        rail.ungroup(id);
                    }
                    // Back in the accounts' own order.
                    for (place, key) in keys.iter().enumerate() {
                        rail.apply(&Dragged::Account(key.clone()), &Drop::Top(place));
                    }
                    rail.apply(
                        &Dragged::Account(keys[1].clone()),
                        &Drop::OntoAccount(keys[0].clone()),
                    );
                    let id = rail.groups()[0].id;
                    for key in &keys[2..members] {
                        rail.apply(&Dragged::Account(key.clone()), &Drop::OntoGroup(id));
                    }
                    rail.toggle(id);
                },
                cx,
            );
            shell.rail.groups()[0].id
        })
    })
}

fn close_to(a: gpui_kit::Pixels, b: gpui_kit::Pixels) -> bool {
    (a - b).abs() <= px(0.75)
}

fn intersects(a: Bounds<gpui_kit::Pixels>, b: Bounds<gpui_kit::Pixels>) -> bool {
    a.left() < b.right() && b.left() < a.right() && a.top() < b.bottom() && b.top() < a.bottom()
}

#[gpui_kit::test]
fn a_closed_group_is_one_cell_like_any_number_at_every_size(cx: &mut TestAppContext) {
    use super::rail::RailGeometry;
    cx.update(|cx| prepare(cx, None));
    let harness = open_with_numbers(cx, 6);
    assert_eq!(stored_accounts(&harness).len(), 8);
    let window_height = px(800.);

    for members in [2usize, 3, 4, 6] {
        let id = group_first(&harness, members, cx);
        for step in theme::SCALE_STEPS {
            cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
            cx.run_until_parked();
            let geometry = cx.update(|_| RailGeometry::now());
            let rail_width = theme::metrics::RAIL_WIDTH();
            let what = format!("{members} members at {step}%");

            // Every item, top to bottom: the group, then the numbers alone.
            let count = cx.update(|cx| harness.shell.read(cx).rail.items.len());
            assert_eq!(count, 8 - members + 1);
            let items: Vec<_> = (0..count)
                .map(|at| bounds_of(harness.window, &format!("rail-item-{at}"), cx))
                .collect();
            for item in &items {
                // One footprint for all: the closed group is a number's cell.
                assert!(close_to(item.size.width, geometry.cell), "{what}: {item:?}");
                assert!(
                    close_to(item.size.height, geometry.cell),
                    "{what}: {item:?}"
                );
                // On the rail's centre line, inside the rail.
                assert!(
                    close_to(item.center().x, rail_width / 2.),
                    "{what}: {item:?}"
                );
            }
            for pair in items.windows(2) {
                assert!(!intersects(pair[0], pair[1]), "{what}: items touch");
                assert!(
                    close_to(pair[1].top() - pair[0].bottom(), geometry.gap),
                    "{what}: the gaps are equal: {} then {}",
                    pair[0].bottom(),
                    pair[1].top()
                );
            }

            // Inside the tile: small faces two by two, the same room all
            // round.
            let tile = bounds_of(harness.window, &format!("rail-tile-{id}"), cx);
            assert!(
                within(tile, items[0]),
                "{what}: the tile is inside its cell"
            );
            let shown = if members > 4 { 3 } else { members };
            let minis: Vec<_> = (0..shown)
                .map(|cell| bounds_of(harness.window, &format!("rail-mini-{cell}"), cx))
                .collect();
            assert!(!shows(
                harness.window,
                Box::leak(format!("rail-mini-{shown}").into_boxed_str()),
                cx
            ));
            let mut cells = minis.clone();
            if members > 4 {
                cells.push(bounds(harness.window, "rail-mini-more", cx));
            } else {
                assert!(!shows(harness.window, "rail-mini-more", cx));
            }
            let room = cells[0].left() - tile.left();
            for (place, cell) in cells.iter().enumerate() {
                assert!(within(*cell, tile), "{what}: face {place} outside the tile");
                assert!(close_to(cell.size.width, geometry.mini()), "{what}");
                assert!(close_to(cell.size.height, geometry.mini()), "{what}");
                let (row, column) = (place / 2, place % 2);
                // Its column and its row.
                assert!(
                    close_to(cell.left(), cells[column].left()),
                    "{what}: column"
                );
                assert!(close_to(cell.top(), cells[row * 2].top()), "{what}: row");
            }
            assert!(
                close_to(cells[0].top() - tile.top(), room),
                "{what}: top room"
            );
            assert!(
                close_to(tile.right() - cells[1].right(), room),
                "{what}: right room"
            );
            assert!(cells[1].left() > cells[0].right(), "{what}: side by side");
            if cells.len() > 2 {
                assert!(cells[2].top() > cells[0].bottom(), "{what}: two rows");
                assert!(
                    close_to(tile.bottom() - cells[2].bottom(), room),
                    "{what}: bottom room"
                );
            }
            for pair in cells.windows(2) {
                assert!(!intersects(pair[0], pair[1]), "{what}: faces overlap");
            }

            // The light sits on the group where it sits on a number.
            let group_light = bounds_of(harness.window, &format!("rail-led-g{id}"), cx);
            let lone_index = members; // the first number left alone
            let lone_light = bounds_of(harness.window, &format!("rail-led-a{lone_index}"), cx);
            let lone = items[1];
            assert!(close_to(
                items[0].right() - group_light.right(),
                lone.right() - lone_light.right()
            ));
            assert!(close_to(
                items[0].bottom() - group_light.bottom(),
                lone.bottom() - lone_light.bottom()
            ));
            assert!(
                close_to(group_light.left(), lone_light.left()),
                "{what}: one column of lights"
            );
            // Lights and counts stay inside the rail and off the neighbours.
            assert!(group_light.right() <= rail_width && lone_light.right() <= rail_width);
            assert!(
                !intersects(group_light, items[1]),
                "{what}: the light is on a neighbour"
            );

            // The logo above and the tools below stay where they are.
            let logo = bounds(harness.window, "rail-logo", cx);
            let tools = bounds(harness.window, "rail-tools", cx);
            assert_eq!(logo.top(), px(0.));
            assert_eq!(tools.bottom(), window_height);
            assert!(
                items[0].top() >= logo.bottom() + geometry.gap,
                "{what}: under the logo"
            );
            assert!(
                items[count - 1].bottom() <= tools.top(),
                "{what}: above the tools"
            );
            for tool in ["add-number", "toggle-theme", "settings"] {
                assert!(
                    within(bounds(harness.window, tool, cx), tools),
                    "{what}: {tool}"
                );
            }
        }
    }
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
}

#[gpui_kit::test]
fn a_groups_count_sits_where_a_numbers_does(cx: &mut TestAppContext) {
    cx.update(|cx| prepare(cx, None));
    let harness = open_with_numbers(cx, 1);
    let accounts = stored_accounts(&harness);
    // The first and the third number in a closed group; the second alone.
    cx.update(|cx| {
        harness.shell.update(cx, |shell, cx| {
            let keys: Vec<String> = shell
                .accounts
                .iter()
                .map(|account| shell.rail_key(&account.id))
                .collect();
            shell.change_rail(
                |rail| {
                    rail.apply(
                        &crate::rail::Dragged::Account(keys[2].clone()),
                        &crate::rail::Drop::OntoAccount(keys[0].clone()),
                    );
                    let id = rail.groups()[0].id;
                    rail.toggle(id);
                },
                cx,
            );
        })
    });
    cx.run_until_parked();
    let (in_group, alone) = cx.update(|cx| {
        let shell = harness.shell.read(cx);
        (
            shell.unread.get(&accounts[0].id).copied().unwrap_or(0),
            shell.unread.get(&accounts[1].id).copied().unwrap_or(0),
        )
    });
    assert!(
        in_group > 0 && alone > 0 && in_group != alone,
        "{in_group} and {alone}"
    );
    let group = bounds(harness.window, "rail-item-0", cx);
    let number = bounds(harness.window, "rail-item-1", cx);
    let group_badge = bounds_of(harness.window, &format!("rail-badge-{in_group}"), cx);
    let number_badge = bounds_of(harness.window, &format!("rail-badge-{alone}"), cx);
    // The same corner: the counts line up down the rail.
    assert!(close_to(
        group_badge.top() - group.top(),
        number_badge.top() - number.top()
    ));
    assert!(close_to(
        group.right() - group_badge.left(),
        number.right() - number_badge.left()
    ));
    // Not on a neighbour, not off the rail.
    assert!(
        !intersects(number_badge, group),
        "a count on the item above"
    );
    assert!(number_badge.top() >= group.bottom());
    let rail = theme::metrics::RAIL_WIDTH();
    assert!(group_badge.right() <= rail && number_badge.right() <= rail);
    // The number on screen is in the group: the group carries its mark.
    cx.update(|cx| {
        assert_eq!(
            harness.shell.read(cx).account.as_ref(),
            Some(&accounts[0].id)
        );
    });
}

#[gpui_kit::test]
fn an_open_group_wraps_its_numbers_with_the_same_room_on_every_side(cx: &mut TestAppContext) {
    use super::rail::RailGeometry;
    cx.update(|cx| prepare(cx, None));
    let harness = open_with_numbers(cx, 3);
    for members in [2usize, 3, 4] {
        let id = group_first(&harness, members, cx);
        let head: &'static str = Box::leak(format!("rail-group-{id}").into_boxed_str());
        click(harness.window, head, cx);
        for step in theme::SCALE_STEPS {
            cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = step));
            cx.run_until_parked();
            let geometry = cx.update(|_| RailGeometry::now());
            let what = format!("{members} members at {step}%");
            let container = bounds_of(harness.window, &format!("rail-group-open-{id}"), cx);
            let header = bounds(harness.window, head, cx);
            let faces: Vec<_> = (0..members)
                .map(|index| bounds_of(harness.window, &format!("rail-account-{index}"), cx))
                .collect();
            let room = header.left() - container.left();
            assert!(room > px(1.), "{what}: there is room");
            assert!(
                close_to(header.top() - container.top(), room),
                "{what}: top"
            );
            assert!(
                close_to(container.right() - header.right(), room),
                "{what}: right"
            );
            assert!(
                close_to(container.bottom() - faces[members - 1].bottom(), room),
                "{what}: bottom"
            );
            // Its numbers at full size, with the rail's own spacing.
            let mut above = header;
            for face in &faces {
                assert!(within(*face, container), "{what}");
                assert!(
                    close_to(face.size.width, geometry.cell),
                    "{what}: full size"
                );
                assert!(
                    close_to(face.size.height, geometry.cell),
                    "{what}: full size"
                );
                assert!(
                    close_to(face.left() - container.left(), room),
                    "{what}: left"
                );
                assert!(
                    close_to(face.top() - above.bottom(), geometry.gap),
                    "{what}: spacing"
                );
                above = *face;
            }
            // The container is an item like the others: the next number
            // comes the same gap below it, and it stays inside the rail.
            let next = bounds(harness.window, "rail-item-1", cx);
            assert!(
                close_to(next.top() - container.bottom(), geometry.gap),
                "{what}"
            );
            assert!(
                container.left() >= px(0.) && container.right() <= theme::metrics::RAIL_WIDTH()
            );
            assert!(
                close_to(container.center().x, next.center().x),
                "{what}: one centre line"
            );
        }
        click(harness.window, head, cx);
    }
    cx.update(|cx| settings::update(cx, |settings| settings.interface_scale = 100));
}

/// Lets `ms` of animation play, a frame at a time.
fn play(cx: &mut TestAppContext, ms: u64) {
    for _ in 0..ms.div_ceil(16) {
        cx.executor().advance_clock(Duration::from_millis(16));
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn a_dragged_number_lifts_leaves_its_slot_and_opens_room_where_it_would_land(
    cx: &mut TestAppContext,
) {
    use super::rail::RailGeometry;
    cx.update(|cx| {
        prepare(cx, None);
        // This is about what moves: motion is on.
        settings::update(cx, |settings| settings.motion = MotionChoice::On);
    });
    let harness = open_with_numbers(cx, 2);
    let geometry = cx.update(|_| RailGeometry::now());
    let left = gpui_kit::MouseButton::Left;
    let items: Vec<_> = (0..4)
        .map(|at| bounds_of(harness.window, &format!("rail-item-{at}"), cx))
        .collect();
    assert!(!shows(harness.window, "rail-floating", cx));

    // Pressed on the first number, a little off its centre, and moved.
    let grab = gpui_kit::point(px(4.), px(-3.));
    let press = items[0].center() + grab;
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(press, left, Modifiers::none());
    // Under the threshold: still a press.
    visual.simulate_mouse_move(
        press + gpui_kit::point(px(2.), px(2.)),
        Some(left),
        Modifiers::none(),
    );
    visual.run_until_parked();
    assert!(visual.debug_bounds("rail-floating").is_none());
    // Past it, down to the lower edge of the third number: it would land
    // after it.
    let pointer = gpui_kit::point(items[2].center().x + px(30.), items[2].bottom() - px(2.));
    visual.simulate_mouse_move(pointer, Some(left), Modifiers::none());
    visual.run_until_parked();
    play(cx, 200);

    // The copy follows the pointer, held where it was grabbed, a little
    // larger than the item.
    let floating = bounds(harness.window, "rail-floating", cx);
    let lifted = geometry.cell * crate::motion::rail::LIFT_SCALE;
    assert!(close_to(floating.size.width, lifted) && close_to(floating.size.height, lifted));
    assert!(
        close_to(floating.center().x, pointer.x - grab.x),
        "{floating:?}"
    );
    assert!(
        close_to(floating.center().y, pointer.y - grab.y),
        "{floating:?}"
    );
    // Its slot is still there, empty, exactly where the item was.
    let placeholder = bounds(harness.window, "rail-placeholder", cx);
    assert!(close_to(placeholder.top(), items[0].top()));
    assert!(close_to(placeholder.size.height, geometry.cell));
    assert!(close_to(placeholder.center().x, items[0].center().x));
    assert!(
        !shows(harness.window, "rail-account-0", cx),
        "the item itself is in the air"
    );
    // Room has opened after the third number: what was below it has moved
    // down by one item, and what is above has not moved.
    let gap = bounds(harness.window, "rail-gap", cx);
    assert!(close_to(gap.size.height, geometry.cell), "{gap:?}");
    assert!(close_to(gap.top() - items[2].bottom(), geometry.gap));
    let third = bounds(harness.window, "rail-item-2", cx);
    let fourth = bounds(harness.window, "rail-item-3", cx);
    assert!(close_to(third.top(), items[2].top()));
    assert!(close_to(
        fourth.top(),
        items[3].top() + geometry.cell + geometry.gap
    ));
    assert!(shows(harness.window, "rail-insertion", cx));

    // Escape: it goes back, over a moment, and nothing has changed.
    let before = rail_shape(&harness, cx);
    press_key_during_drag(&harness, cx);
    play(cx, 32);
    let returning = bounds(harness.window, "rail-floating", cx);
    assert!(
        returning.top() < floating.top(),
        "on its way home: {returning:?}"
    );
    play(cx, 300);
    assert!(!shows(harness.window, "rail-floating", cx));
    assert!(!shows(harness.window, "rail-placeholder", cx));
    assert!(!shows(harness.window, "rail-gap", cx));
    assert_eq!(rail_shape(&harness, cx), before);
    let home = bounds(harness.window, "rail-item-0", cx);
    assert!(close_to(home.top(), items[0].top()));
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_up(pointer, left, Modifiers::none());
    visual.run_until_parked();
    assert_eq!(rail_shape(&harness, cx), before);
}

fn press_key_during_drag(harness: &Harness, cx: &mut TestAppContext) {
    press(harness.window, "escape", cx);
}

#[gpui_kit::test]
fn a_drop_settles_and_resting_on_a_closed_group_opens_it(cx: &mut TestAppContext) {
    use super::rail::RailGeometry;
    cx.update(|cx| {
        prepare(cx, None);
        settings::update(cx, |settings| settings.motion = MotionChoice::On);
    });
    let harness = open_with_numbers(cx, 2);
    let geometry = cx.update(|_| RailGeometry::now());
    let left = gpui_kit::MouseButton::Left;
    // The first two numbers in a closed group; two numbers alone below.
    let id = group_first(&harness, 2, cx);
    play(cx, 400);
    let tile = bounds(harness.window, "rail-item-0", cx);
    let last = bounds(harness.window, "rail-item-2", cx);

    // The last number, held over the middle of the closed group.
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(last.center(), left, Modifiers::none());
    visual.simulate_mouse_move(tile.center(), Some(left), Modifiers::none());
    visual.run_until_parked();
    play(cx, 300);
    cx.update(|cx| assert!(!harness.shell.read(cx).rail.group(id).unwrap().expanded));
    assert!(!shows(harness.window, "rail-gap", cx), "onto, not between");
    // It rests there: after the dwell the group opens for it.
    play(cx, 400);
    cx.update(|cx| assert!(harness.shell.read(cx).rail.group(id).unwrap().expanded));
    play(cx, 300);
    let open: &'static str = Box::leak(format!("rail-group-open-{id}").into_boxed_str());
    let container = bounds(harness.window, open, cx);
    assert!(
        container.size.height > geometry.cell * 2.,
        "fully open: {container:?}"
    );

    // Dropped between its two members: it settles there.
    let second = bounds(harness.window, "rail-account-1", cx);
    let between = gpui_kit::point(second.center().x, second.top() + px(1.));
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_move(between, Some(left), Modifiers::none());
    visual.run_until_parked();
    play(cx, 200);
    let gap = bounds(harness.window, "rail-gap", cx);
    assert!(
        within(gap, bounds(harness.window, open, cx)),
        "the room opens inside the group"
    );
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_up(between, left, Modifiers::none());
    visual.run_until_parked();
    assert_eq!(rail_shape(&harness, cx), "[personal new_2 work] new_1");
    // For a moment the copy is still on its way to its place.
    play(cx, 32);
    assert!(shows(harness.window, "rail-floating", cx));
    play(cx, 300);
    assert!(!shows(harness.window, "rail-floating", cx));
    assert!(!shows(harness.window, "rail-placeholder", cx));
    let landed = bounds(harness.window, "rail-account-3", cx);
    assert!(within(landed, bounds(harness.window, open, cx)));
    assert!(close_to(landed.size.height, geometry.cell));

    // Dragged out again, below the group: its slot closes behind it and
    // the container shrinks.
    let tall = bounds(harness.window, open, cx);
    let numbers = bounds(harness.window, "rail-numbers", cx);
    let below = gpui_kit::point(landed.center().x, numbers.bottom() - px(30.));
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(landed.center(), left, Modifiers::none());
    visual.simulate_mouse_move(below, Some(left), Modifiers::none());
    visual.run_until_parked();
    play(cx, 300);
    let short = bounds(harness.window, open, cx);
    assert!(
        close_to(
            tall.size.height - short.size.height,
            geometry.cell + geometry.gap
        ),
        "one member shorter: {} then {}",
        tall.size.height,
        short.size.height
    );
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_up(below, left, Modifiers::none());
    visual.run_until_parked();
    play(cx, 300);
    assert_eq!(rail_shape(&harness, cx), "[personal work] new_1 new_2");
}

#[gpui_kit::test]
fn a_group_folds_open_over_a_moment_and_at_once_when_motion_is_off(cx: &mut TestAppContext) {
    use super::rail::RailGeometry;
    cx.update(|cx| {
        prepare(cx, None);
        settings::update(cx, |settings| settings.motion = MotionChoice::On);
    });
    let harness = open_with_numbers(cx, 1);
    let geometry = cx.update(|_| RailGeometry::now());
    let id = group_first(&harness, 3, cx);
    play(cx, 400);
    let head: &'static str = Box::leak(format!("rail-group-{id}").into_boxed_str());
    let open: &'static str = Box::leak(format!("rail-group-open-{id}").into_boxed_str());
    let closed = bounds(harness.window, "rail-item-0", cx);
    assert!(close_to(closed.size.height, geometry.cell));

    click(harness.window, head, cx);
    play(cx, 64);
    // Part of the way: taller than a cell, not yet its full height.
    let midway = bounds(harness.window, open, cx);
    play(cx, 400);
    let full = bounds(harness.window, open, cx);
    assert!(midway.size.height > geometry.cell && midway.size.height < full.size.height);
    assert!(midway.size.width <= full.size.width);
    // And back.
    click(harness.window, head, cx);
    play(cx, 64);
    let closing = bounds(harness.window, open, cx);
    assert!(closing.size.height < full.size.height && closing.size.height > geometry.cell);
    play(cx, 400);
    assert!(!shows(harness.window, open, cx));
    assert!(close_to(
        bounds(harness.window, "rail-item-0", cx).size.height,
        geometry.cell
    ));

    // Reduced motion: there at once, no in-between frame.
    cx.update(|cx| settings::update(cx, |settings| settings.motion = MotionChoice::Off));
    click(harness.window, head, cx);
    let at_once = bounds(harness.window, open, cx);
    assert!(close_to(at_once.size.height, full.size.height));
    // And a drag neither lifts over time nor settles: it is, or is not.
    let first = bounds(harness.window, "rail-account-0", cx);
    let left = gpui_kit::MouseButton::Left;
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    visual.simulate_mouse_down(first.center(), left, Modifiers::none());
    visual.simulate_mouse_move(
        first.center() + gpui_kit::point(px(40.), px(0.)),
        Some(left),
        Modifiers::none(),
    );
    visual.run_until_parked();
    let floating = visual
        .debug_bounds("rail-floating")
        .expect("lifted at once");
    assert!(close_to(
        floating.size.width,
        geometry.cell * crate::motion::rail::LIFT_SCALE
    ));
    visual.simulate_mouse_up(gpui_kit::point(px(600.), px(400.)), left, Modifiers::none());
    visual.run_until_parked();
    assert!(
        visual.debug_bounds("rail-floating").is_none(),
        "back at once"
    );
}

// ----- media that is not here yet -------------------------------------------

/// An incoming media message of the open chat, with what the provider
/// said about the file.
fn push_described_media(
    harness: &Harness,
    cx: &mut TestAppContext,
    id: &str,
    kind: client_provider::MediaKind,
    url: &str,
    describe: impl FnOnce(&mut client_provider::Media),
) {
    use client_provider::{
        ContactId, Media, MediaRef, Message, MessageId, ProviderEvent, Timestamp,
    };
    let chat = cx.update(|cx| harness.shell.read(cx).open.as_ref().unwrap().chat.clone());
    let mut media = Media::new(kind);
    media.source = Some(MediaRef::new(url));
    describe(&mut media);
    harness
        .engine
        .apply_event(ProviderEvent::MessageUpserted(Message {
            id: MessageId::new(id),
            client_id: None,
            account_id: chat.account_id.clone(),
            chat_id: chat.id.clone(),
            sender: ContactId::new("them"),
            sender_name: None,
            direction: Direction::Incoming,
            timestamp: Timestamp::from_millis(Timestamp::now().as_millis() + 60_000),
            content: MessageContent::Media(media),
            reply_to: None,
            status: DeliveryStatus::Delivered,
            edited: false,
            deleted: false,
            extras: Default::default(),
        }))
        .unwrap();
    cx.run_until_parked();
}

fn open_chat_with_policy(cx: &mut TestAppContext, policy: crate::settings::MediaChoice) -> Harness {
    cx.update(|cx| {
        prepare(cx, None);
        settings::update(cx, |settings| settings.media = policy);
    });
    let harness = open_prepared(
        cx,
        ShellOptions {
            open_chat: Some(1),
            ..Default::default()
        },
    );
    harness.settle(cx);
    harness
}

/// Lets a download that runs on the runtime get somewhere: until the
/// window shows `selector`.
fn until_shown(harness: &Harness, cx: &mut TestAppContext, selector: &'static str) {
    for _ in 0..300 {
        harness.settle(cx);
        if shows(harness.window, selector, cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("`{selector}` never showed");
}

#[gpui_kit::test]
fn a_download_says_its_size_shows_its_progress_and_can_be_stopped(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Never);
    let url = "https://media.example/big.png";
    let bytes = png(400, 200);
    let size = bytes.len() as u64;
    harness.mock.set_media(url, bytes, "image/png");
    push_described_media(&harness, cx, "m-big", MediaKind::Image, url, |media| {
        media.size_bytes = Some(size);
        (media.width, media.height) = (Some(400), Some(200));
    });
    harness.settle(cx);
    // The box already has the picture's shape, and says how large it is.
    let placeholder = bounds(harness.window, "media-box", cx);
    assert!(
        (placeholder.size.width / placeholder.size.height - 2.).abs() < 0.05,
        "{placeholder:?}"
    );
    assert!(shows(harness.window, "media-download", cx) && shows(harness.window, "media-size", cx));
    assert!(within(
        bounds(harness.window, "media-size", cx),
        placeholder
    ));

    // A slow download: the button becomes a ring, with what has arrived.
    harness.mock.set_media_latency(Duration::from_millis(400));
    click(harness.window, "media-download", cx);
    until_shown(&harness, cx, "media-progress");
    assert!(!shows(harness.window, "media-download", cx));
    assert_eq!(bounds(harness.window, "media-box", cx), placeholder);
    let key = client_core::thumbnail_key(url);
    for _ in 0..100 {
        harness.settle(cx);
        if harness.engine.media_progress(&key).is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(harness.engine.media_progress(&key), Some(size / 2));

    // Stopped: back to the button, and nothing arrives after all.
    click(harness.window, "media-progress", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "media-download", cx));
    std::thread::sleep(Duration::from_millis(500));
    harness.settle(cx);
    assert!(
        !shows(harness.window, "media-image", cx),
        "a stopped download stays stopped"
    );
    assert!(harness.engine.store().media_size(&key).unwrap().is_none());

    // Asked for again: it comes, in the same box.
    harness.mock.set_media_latency(Duration::ZERO);
    click(harness.window, "media-download", cx);
    until_shown(&harness, cx, "media-image");
    assert_eq!(bounds(harness.window, "media-image", cx), placeholder);
}

#[gpui_kit::test]
fn a_failed_download_offers_to_try_again_and_an_expired_file_does_not(cx: &mut TestAppContext) {
    use client_provider::{MediaKind, ProviderError};
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Never);
    let url = "https://media.example/flaky.png";
    harness.mock.set_media(url, png(300, 300), "image/png");
    push_described_media(&harness, cx, "m-flaky", MediaKind::Image, url, |_| {});
    harness.settle(cx);

    // A dropped connection is not a failure: it is still on its way.
    harness
        .mock
        .fail_next_media([ProviderError::Transient("eof".into())]);
    click(harness.window, "media-download", cx);
    harness.settle(cx);
    assert!(shows(harness.window, "media-progress", cx));
    assert!(!shows(harness.window, "media-retry", cx));

    // A refusal is: the reason, and a way to try again.
    let other = "https://media.example/refused.png";
    harness.mock.set_media(other, png(300, 300), "image/png");
    push_described_media(&harness, cx, "m-refused", MediaKind::Image, other, |_| {});
    harness
        .mock
        .fail_next_media([rejected("forbidden", "The file could not be read.")]);
    click(harness.window, "media-download", cx);
    until_shown(&harness, cx, "media-retry");
    let calls = harness.mock.media_calls();
    click(harness.window, "media-retry", cx);
    until_shown(&harness, cx, "media-image");
    assert!(harness.mock.media_calls() > calls);

    // WhatsApp no longer has the file: said, with nothing to press.
    let gone = "https://media.example/gone.png";
    push_described_media(&harness, cx, "m-gone", MediaKind::Image, gone, |_| {});
    harness.mock.fail_next_media([rejected(
        "media_expired",
        "WhatsApp no longer has this file.",
    )]);
    click(harness.window, "media-download", cx);
    until_shown(&harness, cx, "media-unavailable");
    cx.update(|cx| {
        let shell = harness.shell.read(cx);
        let state = shell.engine.media_state(&client_core::thumbnail_key(gone));
        assert!(
            matches!(state, client_core::MediaState::Expired(_)),
            "{state:?}"
        );
    });
    // The newest row is the expired one: no button on it.
    let unavailable = bounds(harness.window, "media-unavailable", cx);
    for button in ["media-download", "media-retry", "media-progress"] {
        let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
        if let Some(found) = visual.debug_bounds(button) {
            assert!(
                !intersects(found, unavailable),
                "{button} on an expired file"
            );
        }
    }
}

#[gpui_kit::test]
fn what_loads_by_itself_shows_a_ring_and_a_file_tile_shows_the_button(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    // A picture, fetched without asking: never a bare box, and never the
    // download button.
    let url = "https://media.example/auto.png";
    harness.mock.set_media(url, png(200, 200), "image/png");
    harness.mock.set_media_latency(Duration::from_millis(300));
    push_described_media(&harness, cx, "m-auto", MediaKind::Image, url, |_| {});
    assert!(shows(harness.window, "media-progress", cx));
    assert!(!shows(harness.window, "media-download", cx));
    until_shown(&harness, cx, "media-image");
    harness.mock.set_media_latency(Duration::ZERO);

    // A picture too large to be fetched unasked waits for a click, with
    // its size.
    let large = "https://media.example/large.png";
    push_described_media(&harness, cx, "m-large", MediaKind::Image, large, |media| {
        media.size_bytes = Some(client_core::AUTO_MEDIA_LIMIT + 1);
    });
    assert!(shows(harness.window, "media-download", cx) && shows(harness.window, "media-size", cx));

    // A document is not fetched under "Images": its tile carries the same
    // button, and "Download and open" stays.
    let file = "https://media.example/contract.pdf";
    harness
        .mock
        .set_media(file, b"%PDF-1.7 contract".to_vec(), "application/pdf");
    push_described_media(&harness, cx, "m-doc", MediaKind::Document, file, |media| {
        media.file_name = Some("contract.pdf".into());
        media.size_bytes = Some(17);
    });
    let tile = bounds(harness.window, "tile-icon", cx);
    let button = bounds(harness.window, "media-download", cx);
    assert!(within(button, tile), "the button is the tile's glyph");
    assert!(shows(harness.window, "media-open", cx));
    let calls = harness.mock.media_calls();
    click(harness.window, "media-download", cx);
    for _ in 0..200 {
        harness.settle(cx);
        if harness.mock.media_calls() > calls {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.settle(cx);
    // Downloaded, not opened: the glyph of its kind is back, and it can
    // be opened.
    let tile = bounds(harness.window, "tile-icon", cx);
    let mut visual = VisualTestContext::from_window(harness.window.into(), cx);
    if let Some(left) = visual.debug_bounds("media-download") {
        assert!(!intersects(left, tile), "the document's button is gone");
    }
    assert!(cx.opened_url().is_none(), "nothing was opened");
    assert!(shows(harness.window, "media-open", cx));
}

#[gpui_kit::test]
fn a_gif_downloads_by_itself_like_a_sticker_and_a_video_waits(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);

    let sticker = "https://media.example/auto-sticker.png";
    harness.mock.set_media(sticker, png(64, 64), "image/png");
    push_described_media(
        &harness,
        cx,
        "m-stk",
        MediaKind::Sticker,
        sticker,
        |media| {
            media.mime_type = Some("image/png".into());
            media.size_bytes = Some(64);
        },
    );

    let gif = "https://media.example/auto-loop.mp4";
    harness
        .mock
        .set_media(gif, b"gif-mp4".to_vec(), "video/mp4");
    push_described_media(&harness, cx, "m-gif", MediaKind::Video, gif, |media| {
        media.mime_type = Some("video/mp4".into());
        media.gif = true;
        media.size_bytes = Some(7);
    });

    let video = "https://media.example/auto-clip.mp4";
    harness.mock.set_media(video, b"clip".to_vec(), "video/mp4");
    push_described_media(&harness, cx, "m-vid", MediaKind::Video, video, |media| {
        media.mime_type = Some("video/mp4".into());
        media.size_bytes = Some(4);
    });

    let huge = "https://media.example/auto-huge.mp4";
    push_described_media(&harness, cx, "m-huge", MediaKind::Video, huge, |media| {
        media.mime_type = Some("video/mp4".into());
        media.gif = true;
        media.size_bytes = Some(client_core::AUTO_MEDIA_LIMIT + 1);
    });

    let cached = |url: &str| {
        harness
            .engine
            .store()
            .media(
                &client_core::file_key(url),
                client_provider::Timestamp::now(),
            )
            .unwrap()
            .is_some()
    };
    let mut sticker_here = false;
    let mut gif_here = false;
    for _ in 0..200 {
        harness.settle(cx);
        sticker_here = harness
            .engine
            .store()
            .media_size(&client_core::thumbnail_key(sticker))
            .unwrap()
            .is_some();
        gif_here = cached(gif);
        if sticker_here && gif_here {
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(sticker_here, "a sticker downloads without a click");
    assert!(gif_here, "a gif downloads without a click");
    assert!(
        cx.opened_url().is_none(),
        "downloading does not open the system player"
    );
    assert!(!cached(video), "a video still waits for a click");
    assert_eq!(
        harness.engine.media_state(&client_core::file_key(video)),
        client_core::MediaState::Idle
    );
    assert!(!cached(huge), "a gif over the automatic limit waits");
    assert_eq!(
        harness.engine.media_state(&client_core::file_key(huge)),
        client_core::MediaState::Idle
    );
}

#[gpui_kit::test]
fn stopping_an_automatic_gif_does_not_start_it_again(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Images);
    for _ in 0..100 {
        harness.settle(cx);
        if !shows(harness.window, "media-progress", cx) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    harness.mock.set_media_latency(Duration::from_secs(2));
    let gif = "https://media.example/stop-loop.mp4";
    harness
        .mock
        .set_media(gif, b"gif-mp4".to_vec(), "video/mp4");
    push_described_media(&harness, cx, "m-stop", MediaKind::Video, gif, |media| {
        media.mime_type = Some("video/mp4".into());
        media.gif = true;
        media.size_bytes = Some(7);
    });
    let mut loading = false;
    for _ in 0..50 {
        harness.settle(cx);
        loading = matches!(
            harness.engine.media_state(&client_core::file_key(gif)),
            client_core::MediaState::Loading
        );
        if loading {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(loading, "the gif started on its own");
    let calls = harness.mock.media_calls();
    click(harness.window, "media-progress", cx);
    for _ in 0..30 {
        harness.settle(cx);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        harness.engine.media_state(&client_core::file_key(gif)),
        client_core::MediaState::Idle
    );
    assert_eq!(
        harness.mock.media_calls(),
        calls,
        "stopping it does not ask again"
    );
    assert!(cx.opened_url().is_none());
}

#[gpui_kit::test]
fn with_automatic_downloads_off_a_gif_and_a_sticker_wait(cx: &mut TestAppContext) {
    use client_provider::MediaKind;
    let harness = open_chat_with_policy(cx, crate::settings::MediaChoice::Never);
    let sticker = "https://media.example/later-sticker.png";
    let gif = "https://media.example/later-loop.mp4";
    harness.mock.set_media(sticker, png(64, 64), "image/png");
    harness
        .mock
        .set_media(gif, b"gif-mp4".to_vec(), "video/mp4");
    push_described_media(
        &harness,
        cx,
        "m-stk",
        MediaKind::Sticker,
        sticker,
        |media| {
            media.mime_type = Some("image/png".into());
        },
    );
    push_described_media(&harness, cx, "m-gif", MediaKind::Video, gif, |media| {
        media.gif = true;
        media.mime_type = Some("video/mp4".into());
    });
    for _ in 0..20 {
        harness.settle(cx);
        std::thread::sleep(Duration::from_millis(3));
    }
    assert_eq!(
        harness
            .engine
            .media_state(&client_core::thumbnail_key(sticker)),
        client_core::MediaState::Idle
    );
    assert_eq!(
        harness.engine.media_state(&client_core::file_key(gif)),
        client_core::MediaState::Idle
    );
    assert!(shows(harness.window, "media-download", cx));
    assert!(cx.opened_url().is_none());
}
