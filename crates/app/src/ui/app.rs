//! The window's root: the recovery screen if the local database cannot be
//! opened, the welcome on a first run, the sign-in screen until
//! there is a session, the shell after, and back to the sign-in on "Sign
//! out".

use super::login::{LoginEvent, LoginScreen};
use super::recovery::{RecoveryEvent, RecoveryScreen};
use super::shell::{CloudOffer, SessionKind, Shell, ShellEvent, ShellOptions};
use super::welcome::{WelcomeEvent, WelcomeScreen};
use crate::login::{IdentityLoader, LoginFlow};
use crate::providers::{Cloud, Launch};
use crate::settings;
use crate::storage::{Recovery, Storage};
use client_core::SyncEngine;
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Entity, Subscription, Task, Window};
use std::sync::Arc;

/// What the window shows.
pub(super) enum Screen {
    /// The local database cannot be opened: reset it, or quit.
    Recovery(Entity<RecoveryScreen>),
    /// The welcome, on a first run.
    Welcome(Entity<WelcomeScreen>),
    /// The sign-in.
    Login(Entity<LoginScreen>),
    /// The chats.
    Chats(Entity<Shell>),
}

/// Carries on with a launch once there is a database again.
type Resume = Box<dyn FnOnce(Storage) -> Result<Launch, String>>;

/// The root view.
pub struct AppView {
    pub(super) screen: Screen,
    /// The database waiting for the person's word, while the recovery
    /// screen is showing.
    recovery: Option<(Recovery, Resume)>,
    /// What comes after the welcome, while the welcome is showing.
    after_welcome: Option<Launch>,
    /// The sign-in to return to on "Sign out"; `None` when the session has
    /// nothing to sign out of (demo data, a key from the environment).
    login: Option<Arc<dyn LoginFlow>>,
    /// The running session's engine, stopped on "Sign out".
    engine: Option<SyncEngine>,
    /// What to say about where the chats are kept, when they are not
    /// being saved.
    storage_note: Option<String>,
    /// Open this chat once the list has loaded (development aid).
    open_chat: Option<usize>,
    /// `--welcome`.
    force_welcome: bool,
    _subscription: Subscription,
    /// Waits for the provider to refuse the session's credentials.
    _auth: Option<Task<()>>,
    /// Set when wuapi is an addition to a session that runs without it
    /// too (the numbers of this machine): signing in to it and out of it
    /// then leaves the engine and the other numbers alone.
    beside: bool,
    /// Waits for wuapi to refuse its key, in such a session.
    _cloud: Option<Task<()>>,
    /// Follows the desktop between light and dark, for the "System" theme.
    _appearance: Subscription,
}

/// What the sign-in says when the key stopped working in a session that
/// goes on without it.
pub(super) const CLOUD_SIGNED_OUT: &str =
    "Wuapi Cloud signed this computer out: the API key is no longer valid. Sign in again \
     to get its numbers back; the numbers linked to this computer go on working.";

/// What the sign-in says when the key stopped working.
pub(super) const SIGNED_OUT: &str =
    "This device was signed out: the API key is no longer valid. Sign in again to continue; \
     your chats on this computer are still here.";

/// The first screen of a launch, and what the root keeps of it.
struct Opening {
    screen: Screen,
    subscription: Subscription,
    recovery: Option<(Recovery, Resume)>,
    after_welcome: Option<Launch>,
    login: Option<Arc<dyn LoginFlow>>,
    engine: Option<SyncEngine>,
    storage_note: Option<String>,
    cloud: Option<Cloud>,
}

impl AppView {
    /// Builds the root for how the application was launched.
    ///
    /// The welcome comes first when there is no account and it has not been
    /// seen through yet, and whenever `force_welcome` (`--welcome`) says so.
    /// A database that cannot be opened comes before everything.
    pub fn new(
        launch: Launch,
        open_chat: Option<usize>,
        force_welcome: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let appearance = cx.observe_window_appearance(window, |_, _, cx| {
            settings::apply_theme(cx);
            cx.notify();
        });
        let opening = open(launch, open_chat, force_welcome, window, cx);
        let auth = watch_auth(opening.engine.as_ref(), window, cx);
        let beside = opening.cloud.is_some();
        let cloud = watch_cloud(opening.cloud, window, cx);
        Self {
            _auth: auth,
            beside,
            _cloud: cloud,
            screen: opening.screen,
            recovery: opening.recovery,
            after_welcome: opening.after_welcome,
            login: opening.login,
            engine: opening.engine,
            storage_note: opening.storage_note,
            open_chat,
            force_welcome,
            _subscription: opening.subscription,
            _appearance: appearance,
        }
    }

    fn show(&mut self, opening: Opening, window: &mut Window, cx: &mut Context<Self>) {
        self._auth = watch_auth(opening.engine.as_ref(), window, cx);
        self.beside = opening.cloud.is_some();
        self._cloud = watch_cloud(opening.cloud, window, cx);
        self.screen = opening.screen;
        self._subscription = opening.subscription;
        self.recovery = opening.recovery;
        self.after_welcome = opening.after_welcome;
        self.login = opening.login;
        self.engine = opening.engine;
        self.storage_note = opening.storage_note;
        cx.notify();
    }

    /// The person agreed to lose the local copy: delete it, start a new
    /// database, and carry on with the launch.
    fn reset_database(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((recovery, resume)) = self.recovery.take() else {
            return;
        };
        match resume(recovery.reset()) {
            Ok(launch) => {
                let opening = open(launch, self.open_chat, self.force_welcome, window, cx);
                self.show(opening, window, cx);
            }
            Err(error) => {
                // Nothing more can be tried from here; say why.
                if let Screen::Recovery(screen) = &self.screen {
                    screen.update(cx, |screen, cx| {
                        screen.error = Some(error.into());
                        cx.notify();
                    });
                }
            }
        }
    }

    /// The welcome was seen through: remember it, and go where the launch
    /// was going.
    fn welcomed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(launch) = self.after_welcome.take() else {
            return;
        };
        settings::update(cx, |settings| settings.welcome_done = true);
        let opening = enter(launch, self.open_chat, window, cx);
        self.show(opening, window, cx);
    }

    /// Signed in: on to the chats.
    fn signed_in(
        &mut self,
        engine: SyncEngine,
        identity: Option<IdentityLoader>,
        saved: bool,
        storage_note: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.beside {
            // The session was running all along: wuapi joined it.
            return self.show_chats(CloudOffer::Connected, identity, window, cx);
        }
        self.storage_note = storage_note;
        let options = ShellOptions {
            open_chat: self.open_chat,
            session: if saved {
                SessionKind::Keychain
            } else {
                SessionKind::Unsaved
            },
            identity,
            storage_note: self.storage_note.clone(),
            pick_files: None,
            out: None,
            cloud: Default::default(),
        };
        let (screen, subscription) = chats(&engine, options, window, cx);
        self.screen = screen;
        self._subscription = subscription;
        self._auth = watch_auth(Some(&engine), window, cx);
        self.engine = Some(engine);
        cx.notify();
    }

    /// The chats of a session that runs with or without wuapi, drawn
    /// anew: wuapi was just signed in to, or out of.
    fn show_chats(
        &mut self,
        cloud: CloudOffer,
        identity: Option<IdentityLoader>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(engine) = self.engine.clone() else {
            return;
        };
        let options = ShellOptions {
            open_chat: None,
            session: local_session(),
            identity,
            storage_note: self.storage_note.clone(),
            pick_files: None,
            out: None,
            cloud,
        };
        let (screen, subscription) = chats(&engine, options, window, cx);
        self.screen = screen;
        self._subscription = subscription;
        cx.notify();
    }

    /// "Connect Wuapi Cloud": the sign-in, over a session that goes on
    /// underneath.
    fn connect_cloud(
        &mut self,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (true, Some(flow)) = (self.beside, self.login.clone()) else {
            return;
        };
        let (screen, subscription) = login(&flow, None, notice, window, cx);
        self.screen = screen;
        self._subscription = subscription;
        cx.notify();
    }

    /// "Not now", on that sign-in: back to the chats as they were.
    fn back_to_chats(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.beside && matches!(self.screen, Screen::Login(_)) {
            self.show_chats(CloudOffer::Offered, None, window, cx);
        }
    }

    /// Signs out of wuapi in a session that goes on without it: its key
    /// and its numbers go, the numbers of this machine stay. `notice` is
    /// said on the sign-in, which shows when it was wuapi that ended it.
    fn leave_cloud(&mut self, notice: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(flow) = self.login.clone() else {
            return;
        };
        match notice {
            Some(notice) => self.connect_cloud(Some(notice), window, cx),
            None => self.show_chats(CloudOffer::Offered, None, window, cx),
        }
        let done = flow.sign_out();
        cx.spawn(async move |_, _| {
            if let Err(error) = done.await {
                tracing::warn!(%error, "signing out of wuapi left something behind");
            }
        })
        .detach();
    }

    /// The provider no longer accepts the key (revoked, or deleted): the
    /// session is over. Back to the sign-in, saying why. The chats on this
    /// computer stay: whoever signs in next decides what becomes of them
    /// (the same account keeps them, another one starts clean).
    fn signed_out_by_provider(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
        self._auth = None;
        let Some(flow) = self.login.clone() else {
            return;
        };
        let (screen, subscription) = login(&flow, None, Some(SIGNED_OUT.to_owned()), window, cx);
        self.screen = screen;
        self._subscription = subscription;
        // The key is of no more use to anyone. Only the key goes.
        let forgotten = flow.forget_key();
        cx.spawn(async move |_, _| {
            if let Err(error) = forgotten.await {
                tracing::warn!(%error, "the rejected API key could not be removed");
            }
        })
        .detach();
        cx.notify();
    }

    /// Signs out: stops the session, shows the sign-in, and deletes the
    /// API key and the chats kept on this computer. Settings stay.
    fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.beside {
            return self.leave_cloud(None, window, cx);
        }
        let Some(flow) = self.login.clone() else {
            return;
        };
        if let Some(engine) = self.engine.take() {
            engine.shutdown();
        }
        self._auth = None;
        // The shell goes first, and with it its hold on the database.
        let (screen, subscription) = login(&flow, None, None, window, cx);
        self.screen = screen;
        self._subscription = subscription;
        self.storage_note = None;
        cx.notify();

        // Anything that was written out to be opened goes too.
        super::media::clear_exports();
        let done = flow.sign_out();
        cx.spawn(async move |this, cx| {
            // Off the UI thread. What could not be removed is said on the
            // sign-in screen.
            if let Err(error) = done.await {
                this.update(cx, |this, cx| {
                    if let Screen::Login(screen) = &this.screen {
                        screen.update(cx, |screen, cx| {
                            screen.notice = Some(error.into());
                            cx.notify();
                        });
                    }
                })
                .ok();
            }
        })
        .detach();
    }
}

/// Follows the engine's word on whether the provider still accepts the
/// credentials.
fn watch_auth(
    engine: Option<&SyncEngine>,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> Option<Task<()>> {
    let mut auth = engine?.auth_lost();
    Some(cx.spawn_in(window, async move |this, cx| {
        while auth.borrow_and_update().is_none() {
            if auth.changed().await.is_err() {
                return;
            }
        }
        this.update_in(cx, |this, window, cx| {
            this.signed_out_by_provider(window, cx)
        })
        .ok();
    }))
}

/// Follows wuapi's word on its key, in a session that goes on without it.
fn watch_cloud(
    cloud: Option<Cloud>,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> Option<Task<()>> {
    let mut refusals = cloud?.refusals;
    Some(cx.spawn_in(window, async move |this, cx| loop {
        if refusals.borrow_and_update().is_some() {
            let told = this.update_in(cx, |this, window, cx| {
                // A call that is on keeps its card: the sign-in does not
                // take the window from it, and Settings offers it after.
                let on_a_call = this
                    .engine
                    .as_ref()
                    .is_some_and(|engine| !engine.calls().is_empty());
                let notice = (!on_a_call).then(|| CLOUD_SIGNED_OUT.to_owned());
                this.leave_cloud(notice, window, cx)
            });
            if told.is_err() {
                return;
            }
        }
        if refusals.changed().await.is_err() {
            return;
        }
    }))
}

/// The kind of a session that runs on the numbers of this machine.
fn local_session() -> SessionKind {
    #[cfg(feature = "provider-native")]
    return SessionKind::Local;
    #[cfg(not(feature = "provider-native"))]
    return SessionKind::Demo;
}

/// What a launch shows first: the recovery screen for a database that
/// cannot be opened, the welcome on a first run, or where it was going.
fn open(
    launch: Launch,
    open_chat: Option<usize>,
    force_welcome: bool,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> Opening {
    let launch = match launch {
        Launch::Recover { recovery, resume } => {
            let screen = cx.new(|cx| RecoveryScreen::new(window, cx));
            let subscription = cx.subscribe_in(
                &screen,
                window,
                |this, _, event: &RecoveryEvent, window, cx| match event {
                    RecoveryEvent::Reset => this.reset_database(window, cx),
                },
            );
            return Opening {
                screen: Screen::Recovery(screen),
                subscription,
                recovery: Some((recovery, resume)),
                after_welcome: None,
                login: None,
                engine: None,
                storage_note: None,
                cloud: None,
            };
        }
        launch => launch,
    };
    let first_run = matches!(launch, Launch::Login { .. }) && !settings::get(cx).welcome_done;
    if force_welcome || first_run {
        // Only a provider that is signed in to has something to choose.
        let choose_provider = matches!(launch, Launch::Login { .. });
        let welcome = cx.new(|cx| WelcomeScreen::new(choose_provider, window, cx));
        let subscription = cx.subscribe_in(
            &welcome,
            window,
            |this, _, event: &WelcomeEvent, window, cx| match event {
                WelcomeEvent::Done => this.welcomed(window, cx),
            },
        );
        return Opening {
            screen: Screen::Welcome(welcome),
            subscription,
            recovery: None,
            after_welcome: Some(launch),
            login: None,
            engine: None,
            storage_note: None,
            cloud: None,
        };
    }
    enter(launch, open_chat, window, cx)
}

/// Where a launch was going: the chats, or the sign-in.
fn enter(
    launch: Launch,
    open_chat: Option<usize>,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> Opening {
    match launch {
        Launch::Chats {
            engine,
            session,
            login,
            identity,
            storage_note,
            cloud,
        } => {
            let options = ShellOptions {
                open_chat,
                session,
                identity,
                storage_note: storage_note.clone(),
                pick_files: None,
                out: None,
                cloud: cloud
                    .as_ref()
                    .map_or(CloudOffer::Unavailable, |cloud| cloud.offer),
            };
            let (screen, subscription) = chats(&engine, options, window, cx);
            Opening {
                screen,
                subscription,
                recovery: None,
                after_welcome: None,
                login,
                engine: Some(engine),
                storage_note,
                cloud,
            }
        }
        Launch::Login {
            flow,
            keychain_error,
            storage_note,
        } => {
            let (screen, subscription) = self::login(&flow, keychain_error, None, window, cx);
            Opening {
                screen,
                subscription,
                recovery: None,
                after_welcome: None,
                login: Some(flow),
                engine: None,
                storage_note,
                cloud: None,
            }
        }
        // `open` has dealt with it; a launch never asks twice.
        Launch::Recover { .. } => unreachable!("recovery is handled before entering"),
    }
}

fn chats(
    engine: &SyncEngine,
    options: ShellOptions,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> (Screen, Subscription) {
    let engine = engine.clone();
    let shell = cx.new(|cx| Shell::new(engine, options, window, cx));
    let subscription = cx.subscribe_in(
        &shell,
        window,
        |this, _, event: &ShellEvent, window, cx| match event {
            ShellEvent::SignOut => this.sign_out(window, cx),
            ShellEvent::ConnectCloud => this.connect_cloud(None, window, cx),
        },
    );
    (Screen::Chats(shell), subscription)
}

fn login(
    flow: &Arc<dyn LoginFlow>,
    keychain_error: Option<String>,
    notice: Option<String>,
    window: &mut Window,
    cx: &mut Context<AppView>,
) -> (Screen, Subscription) {
    let flow = flow.clone();
    let screen = cx.new(|cx| LoginScreen::new(flow, keychain_error, notice, window, cx));
    let subscription = cx.subscribe_in(
        &screen,
        window,
        |this, _, event: &LoginEvent, window, cx| match event {
            LoginEvent::SignedIn {
                engine,
                identity,
                saved,
                storage_note,
            } => this.signed_in(
                engine.clone(),
                identity.clone(),
                *saved,
                storage_note.clone(),
                window,
                cx,
            ),
        },
    );
    (Screen::Login(screen), subscription)
}

impl Render for AppView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div().size_full();
        match &self.screen {
            Screen::Recovery(screen) => root.child(screen.clone()),
            Screen::Welcome(screen) => root.child(screen.clone()),
            // Over a session that goes on underneath, there is a way back.
            Screen::Login(screen) => {
                root.child(screen.clone())
                    .when(self.beside && self.engine.is_some(), |root| {
                        let palette = crate::theme::palette(cx);
                        root.relative().child(
                            div().absolute().top_4().left_4().child(
                                super::widgets::text_button(
                                    "login-back",
                                    "Not now",
                                    None,
                                    false,
                                    &palette,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.back_to_chats(window, cx);
                                    },
                                )),
                            ),
                        )
                    })
            }
            Screen::Chats(shell) => root.child(shell.clone()),
        }
    }
}

#[cfg(feature = "capture")]
impl AppView {
    /// What a capture script waits for and reports: the rows of the chat
    /// list, the rows of the open conversation and which chat that is.
    pub(crate) fn capture_state(&self, cx: &gpui_kit::App) -> (usize, usize, Option<String>) {
        let Screen::Chats(shell) = &self.screen else {
            return (0, 0, None);
        };
        let open = shell.read(cx).open.as_ref();
        (
            shell.read(cx).list_rows.len(),
            open.map_or(0, |open| open.rows.len()),
            open.map(|open| open.chat.id.as_str().to_owned()),
        )
    }
}
