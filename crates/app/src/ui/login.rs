//! The sign-in screen: what `--provider wuapi` shows until there is an API
//! key.
//!
//! The person presses "Connect", gets a short code, approves it in the
//! browser, and the application receives a key (RFC 8628). The decisions
//! are [`LoginMachine`]'s; this view draws its phase and runs the waiting:
//! one task asks for the code and then polls at the server's pace, another
//! repaints the countdown. Both belong to the view, so closing the window
//! stops them, and the network work itself runs on the Tokio runtime (see
//! `providers.rs`), never here.

use super::conversation::{colophon, framed_rows, mark_frame};
use super::widgets::{
    grid_mark_in, label, led, mono, reveal, screen_header, screen_rail, tag, text_button, MarkPlay,
};
use crate::icons::{icon, IconName};
use crate::login::{countdown, IdentityLoader, KeySaved, LoginFlow, LoginMachine, Phase, Session};
use crate::motion::{self, Entrance};
use crate::product::PRODUCT_NAME;
use crate::settings;
use crate::theme::px;
use crate::theme::{fonts, metrics, palette, Palette};
use client_core::SyncEngine;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, AnyElement, ClipboardItem, Context, Div, EventEmitter, FocusHandle, FontFeatures,
    FontWeight, Hsla, KeyDownEvent, SharedString, Task, Window,
};
use provider_wuapi::DeviceCode;
use std::sync::Arc;
use std::time::Duration;

/// How long "Copied" stays on the copy button.
const COPIED_FOR: Duration = Duration::from_secs(2);

/// What the screen tells its owner.
pub enum LoginEvent {
    /// Signed in: here is the running session.
    SignedIn {
        /// The engine, already started.
        engine: SyncEngine,
        /// Who signed in.
        identity: Option<IdentityLoader>,
        /// The key is in the keychain for the next start.
        saved: bool,
        /// What to say about where the chats are kept, if they are not
        /// being saved.
        storage_note: Option<String>,
    },
}

/// The sign-in screen.
pub struct LoginScreen {
    flow: Arc<dyn LoginFlow>,
    pub(super) machine: LoginMachine,
    /// Why the OS keychain cannot be used, when it cannot.
    pub(super) keychain_error: Option<String>,
    /// Something to say before anything is pressed: why the person is
    /// here again (the key stopped working), or what signing out could not
    /// remove.
    pub(super) notice: Option<SharedString>,
    /// A session that is waiting for the person to read that the key was
    /// not saved.
    session: Option<(SyncEngine, Option<IdentityLoader>, Option<String>)>,
    /// The screen's entrance, played once.
    entrance: Entrance,
    /// The mark still has its entrance to play: true until the window has
    /// been in the background once.
    mark_intro: bool,
    _activation: gpui_kit::Subscription,
    _refocus: gpui_kit::Subscription,
    _entrance: Option<Task<()>>,
    copied: bool,
    focus: FocusHandle,
    /// Asks for the code, then polls. Dropped with the view.
    _attempt: Option<Task<()>>,
    /// Repaints the countdown once a second while a code is on screen.
    _ticker: Option<Task<()>>,
    _copied: Option<Task<()>>,
}

impl EventEmitter<LoginEvent> for LoginScreen {}

impl LoginScreen {
    /// Builds the screen at its start: nothing is asked of the API until
    /// the person presses "Connect".
    pub fn new(
        flow: Arc<dyn LoginFlow>,
        keychain_error: Option<String>,
        notice: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        // Enter presses the screen's main button.
        focus.focus(window, cx);
        let mut entrance = Entrance::default();
        let mut playing = None;
        // A button that was focused and is gone (the phase changed) leaves
        // the keyboard nowhere: bring it back to the screen.
        let refocus = cx.on_focus_lost(window, |this, window, cx| this.focus.focus(window, cx));
        // Losing or regaining the window decides whether the mark may move.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.mark_intro = false;
            }
            cx.notify();
        });
        if let Some(at) = motion::frozen_at(cx) {
            entrance = Entrance::frozen(at);
        } else if !settings::reduce_motion(cx) {
            entrance.begin(cx.background_executor().now());
            // Repaint each frame while it runs, and not once more after.
            playing = Some(cx.spawn(async move |this, cx| {
                let clock = cx.background_executor().clone();
                loop {
                    clock.timer(motion::FRAME).await;
                    let running = this.update(cx, |this: &mut Self, cx| {
                        cx.notify();
                        this.entrance.running(clock.now())
                    });
                    if !matches!(running, Ok(true)) {
                        return;
                    }
                }
            }));
        }
        Self {
            flow,
            machine: LoginMachine::default(),
            keychain_error,
            notice: notice.map(SharedString::from),
            session: None,
            entrance,
            mark_intro: true,
            _activation: activation,
            _refocus: refocus,
            _entrance: playing,
            copied: false,
            focus,
            _attempt: None,
            _ticker: None,
            _copied: None,
        }
    }

    // ----- the sign-in --------------------------------------------------

    /// Asks for a code and waits for its approval. Starting again drops
    /// the previous attempt, so its answers cannot arrive late.
    pub(super) fn connect(&mut self, cx: &mut Context<Self>) {
        self.machine.begin();
        self.session = None;
        self.copied = false;
        let flow = self.flow.clone();
        self._attempt = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            let issued = flow.request_code().await;
            let Ok(Some(code)) = this.update(cx, |this, cx| {
                cx.notify();
                match issued {
                    Ok(code) => {
                        this.machine.code_issued(code.clone(), clock.now());
                        this.start_ticker(cx);
                        Some(code)
                    }
                    Err(error) => {
                        this.machine.code_refused(error);
                        None
                    }
                }
            }) else {
                return;
            };
            loop {
                let Ok(Some(wait)) = this.update(cx, |this, cx| {
                    cx.notify();
                    this.machine.next_poll(clock.now())
                }) else {
                    return;
                };
                clock.timer(wait).await;
                let answer = flow.poll(&code).await;
                let Ok(grant) = this.update(cx, |this, cx| {
                    cx.notify();
                    this.machine.polled(answer, clock.now())
                }) else {
                    return;
                };
                if let Some(grant) = grant {
                    let session = flow.open_session(grant).await;
                    this.update(cx, |this, cx| this.session_opened(session, cx))
                        .ok();
                    return;
                }
            }
        }));
        cx.notify();
    }

    fn start_ticker(&mut self, cx: &mut Context<Self>) {
        self._ticker = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            loop {
                clock.timer(Duration::from_secs(1)).await;
                let waiting = this.update(cx, |this, cx| {
                    this.machine.tick(clock.now());
                    cx.notify();
                    matches!(this.machine.phase(), Phase::Waiting { .. })
                });
                if !matches!(waiting, Ok(true)) {
                    return;
                }
            }
        }));
    }

    fn session_opened(&mut self, session: Result<Session, String>, cx: &mut Context<Self>) {
        match session {
            Ok(Session {
                engine,
                identity,
                key_saved,
                storage_note,
            }) => match key_saved {
                // Told once that the keychain is not there, the person is
                // not stopped again to hear it.
                KeySaved::No(reason) if self.keychain_error.is_none() => {
                    self.machine.key_not_saved(reason);
                    self.session = Some((engine, identity, storage_note));
                }
                saved => cx.emit(LoginEvent::SignedIn {
                    engine,
                    identity,
                    saved: saved == KeySaved::Yes,
                    storage_note,
                }),
            },
            Err(reason) => self.machine.failed(reason),
        }
        cx.notify();
    }

    /// Back to the start, dropping the code on screen.
    fn cancel(&mut self, cx: &mut Context<Self>) {
        self._attempt = None;
        self._ticker = None;
        self.machine.reset();
        cx.notify();
    }

    /// Into the chat list with a session whose key was not saved.
    fn proceed(&mut self, cx: &mut Context<Self>) {
        if let Some((engine, identity, storage_note)) = self.session.take() {
            cx.emit(LoginEvent::SignedIn {
                engine,
                identity,
                saved: false,
                storage_note,
            });
        }
    }

    fn open_browser(&mut self, cx: &mut Context<Self>) {
        if let Phase::Waiting { code, .. } = self.machine.phase() {
            cx.open_url(&code.verification_uri_complete);
        }
    }

    fn copy_code(&mut self, cx: &mut Context<Self>) {
        let Phase::Waiting { code, .. } = self.machine.phase() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(code.user_code.clone()));
        self.copied = true;
        self._copied = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_FOR).await;
            this.update(cx, |this, cx| {
                this.copied = false;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// What Enter does: the main button of the current phase.
    fn primary(&mut self, cx: &mut Context<Self>) {
        match self.machine.phase() {
            Phase::Start
            | Phase::Expired
            | Phase::Denied
            | Phase::Offline
            | Phase::Failed { .. } => self.connect(cx),
            Phase::Waiting { .. } => self.open_browser(cx),
            Phase::KeyNotSaved { .. } => self.proceed(cx),
            Phase::Requesting | Phase::Approved { .. } => {}
        }
    }

    // ----- drawing ------------------------------------------------------

    fn render_phase(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let column = div().w_full().flex().flex_col().items_center();
        match self.machine.phase() {
            Phase::Start => column
                .child(heading(&format!("Welcome to {PRODUCT_NAME}")))
                .child(body(
                    "It reaches WhatsApp through your wuapi account. You approve this \
                     computer in the browser; no password or API key is typed here.",
                    palette,
                ))
                .child(
                    text_button("connect", "Connect your wuapi account", None, true, palette)
                        .mt_6()
                        .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                ),
            Phase::Requesting => column
                .child(heading("Connecting to wuapi"))
                .child(body("Asking for a sign-in code.", palette))
                .child(
                    div()
                        .pt_6()
                        .h(metrics::BUTTON() + px(24.))
                        .flex()
                        .items_center()
                        .child(tag("Requesting a code", Some(palette.warning), palette)),
                ),
            Phase::Waiting { code, unreachable } => {
                self.render_waiting(column, code, *unreachable, palette, cx)
            }
            Phase::Approved { organization } => column
                .child(heading(&format!("Signed in to {organization}")))
                .child(body("Loading your accounts.", palette))
                .child(
                    div()
                        .pt_6()
                        .h(metrics::BUTTON() + px(24.))
                        .flex()
                        .items_center()
                        .child(tag("Approved", Some(palette.accent), palette)),
                ),
            Phase::KeyNotSaved { .. } => column
                .child(heading("Signed in"))
                .child(body(
                    "This session works. The API key could not be saved, so you will be \
                     asked to sign in again the next time you start.",
                    palette,
                ))
                .child(
                    text_button("continue", "Continue to your chats", None, true, palette)
                        .mt_6()
                        .on_click(cx.listener(|this, _, _, cx| this.proceed(cx))),
                ),
            Phase::Expired => self.render_stopped(
                column,
                (IconName::Clock3, palette.text_muted),
                "The code expired",
                "Nobody approved it in time. A new code takes a second.".into(),
                "Get a new code",
                palette,
                cx,
            ),
            Phase::Denied => self.render_stopped(
                column,
                (IconName::CircleX, palette.danger),
                "Sign-in denied",
                "The request was denied in the browser. If that was not you, there is \
                 nothing else to do: no key was issued."
                    .into(),
                "Try again",
                palette,
                cx,
            ),
            Phase::Offline => self.render_stopped(
                column,
                (IconName::WifiOff, palette.warning),
                "Can't reach wuapi",
                "Check your connection. Nothing was lost; try again when you are back \
                 online."
                    .into(),
                "Try again",
                palette,
                cx,
            ),
            Phase::Failed { reason } => self.render_stopped(
                column,
                (IconName::CircleAlert, palette.danger),
                "Sign-in failed",
                reason.clone().into(),
                "Try again",
                palette,
                cx,
            ),
        }
    }

    fn render_waiting(
        &self,
        column: Div,
        code: &DeviceCode,
        unreachable: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let remaining = self
            .machine
            .remaining(cx.background_executor().now())
            .unwrap_or_default();
        let (copy_icon, copy_label) = if self.copied {
            (IconName::Check, "Copied")
        } else {
            (IconName::Copy, "Copy code")
        };
        column
            .child(heading("Approve this computer"))
            .child(body(
                "Open wuapi in the browser and confirm that it shows this code.",
                palette,
            ))
            .child(
                div()
                    .mt_5()
                    .w_full()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.surface)
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(palette.border)
                            .flex()
                            .items_center()
                            .justify_between()
                            .child(label("Your code", palette))
                            .child(div().debug_selector(|| "countdown".into()).child(label(
                                &format!("expires in {}", countdown(remaining)),
                                palette,
                            ))),
                    )
                    .child(
                        div()
                            .debug_selector(|| "user-code".into())
                            .py_4()
                            .font_family(fonts::MONO)
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(metrics::TEXT_CODE())
                            .text_color(palette.text)
                            .child(SharedString::from(code.user_code.clone())),
                    ),
            )
            .child(
                div()
                    .mt_3()
                    .w_full()
                    .flex()
                    .gap_2()
                    .child(
                        text_button(
                            "open-browser",
                            "Open wuapi in the browser",
                            Some(IconName::ExternalLink),
                            true,
                            palette,
                        )
                        .flex_1()
                        .on_click(cx.listener(|this, _, _, cx| this.open_browser(cx))),
                    )
                    .child(
                        text_button("copy-code", copy_label, Some(copy_icon), false, palette)
                            .on_click(cx.listener(|this, _, _, cx| this.copy_code(cx))),
                    ),
            )
            .child(
                div()
                    .mt_4()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(if unreachable {
                        // Weather, not a verdict: the code is still good and
                        // the question is asked again.
                        tag(
                            "Connection lost. Still trying",
                            Some(palette.danger),
                            palette,
                        )
                    } else {
                        tag("Waiting for approval", Some(palette.warning), palette)
                    })
                    .child(
                        div()
                            .id("cancel")
                            .debug_selector(|| "cancel".into())
                            .cursor_pointer()
                            .tab_index(0)
                            .text_color(palette.text_muted)
                            .hover(|style| style.opacity(0.7))
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx)))
                            .child(mono("CANCEL")),
                    ),
            )
            .child(
                div()
                    .pt_3()
                    .w_full()
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(19.))
                    .text_color(palette.text_muted)
                    .child(SharedString::from(format!(
                        "No browser on this computer? Open {} on any device and type the code.",
                        code.verification_uri
                    ))),
            )
    }

    /// A phase that ended without a session: what happened, and the button
    /// that starts over.
    #[allow(clippy::too_many_arguments)]
    fn render_stopped(
        &self,
        column: Div,
        (glyph, colour): (IconName, Hsla),
        title: &str,
        text: SharedString,
        action: &'static str,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        column
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(glyph, px(20.), colour))
                    .child(heading(title)),
            )
            .child(body(text, palette))
            .child(
                text_button("retry", action, None, true, palette)
                    .mt_6()
                    .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
            )
    }

    /// Where the sign-in stands, as three cells in a frame: the light is
    /// on the step that is happening now.
    fn render_steps(&self, progress: f32, palette: &Palette) -> Div {
        // (current step, its light). Steps before it are done.
        let (current, light) = match self.machine.phase() {
            Phase::Start | Phase::Offline | Phase::Failed { .. } => (0, palette.text_faint),
            Phase::Requesting => (0, palette.warning),
            Phase::Waiting { .. } => (1, palette.warning),
            Phase::Expired | Phase::Denied => (1, palette.danger),
            Phase::Approved { .. } => (2, palette.warning),
            Phase::KeyNotSaved { .. } => (2, palette.accent),
        };
        let mut row = div().flex();
        for (index, name) in ["Connect", "Approve", "Chats"].into_iter().enumerate() {
            let colour = match index.cmp(&current) {
                std::cmp::Ordering::Less => palette.accent,
                std::cmp::Ordering::Equal => light,
                std::cmp::Ordering::Greater => palette.border,
            };
            row = row.child(
                div()
                    .flex_1()
                    .h(px(38.))
                    .when(index > 0, |this| {
                        this.border_l_1().border_color(palette.border)
                    })
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(led(colour))
                    .child(
                        mono(format!("{} {}", index + 1, name.to_uppercase())).text_color(
                            if index == current {
                                palette.text
                            } else {
                                palette.text_muted
                            },
                        ),
                    ),
            );
        }
        framed_rows(progress, palette, [row])
    }

    /// The keychain's absence, said plainly and with what to do about it.
    fn render_keychain_notice(&self, palette: &Palette) -> Option<AnyElement> {
        let reason = match (self.machine.phase(), &self.keychain_error) {
            (Phase::KeyNotSaved { reason }, _) => reason,
            (Phase::Start | Phase::Requesting | Phase::Waiting { .. }, Some(reason)) => reason,
            _ => return None,
        };
        let advice = if cfg!(target_os = "linux") {
            "You can still sign in, but the API key cannot be saved and you will be asked \
             again at every start. To fix it, install and unlock a Secret Service keyring \
             (GNOME Keyring or KWallet), then restart. On a machine without one, start with \
             WUAPI_API_KEY set instead."
        } else {
            "You can still sign in, but the API key cannot be saved and you will be asked \
             again at every start. Unlock the system keychain and restart, or start with \
             WUAPI_API_KEY set instead."
        };
        Some(
            div()
                .debug_selector(|| "keychain-notice".into())
                .mt_5()
                .w_full()
                .p_3()
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(palette.border)
                .flex()
                .gap_3()
                .child(icon(IconName::TriangleAlert, px(16.), palette.warning))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(metrics::TEXT_SMALL())
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(palette.text)
                                .child("The system keychain is not available"),
                        )
                        .child(
                            div()
                                .text_size(metrics::TEXT_SMALL())
                                .line_height(px(19.))
                                .text_color(palette.text_muted)
                                .child(advice),
                        )
                        .child(
                            div()
                                .font_family(fonts::MONO)
                                .text_size(metrics::TEXT_META())
                                .line_height(px(16.))
                                .text_color(palette.text_faint)
                                .child(SharedString::from(reason.clone())),
                        ),
                )
                .into_any_element(),
        )
    }
}

impl Render for LoginScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let features = FontFeatures(Arc::new(
            fonts::SANS_FEATURES
                .iter()
                .map(|(tag, value)| ((*tag).to_owned(), *value))
                .collect(),
        ));
        let now = cx.background_executor().now();
        let step = |step: u32| self.entrance.reveal(step, now);
        // The mark moves only while this window is the one being looked at.
        let play = MarkPlay {
            paused: !window.is_window_active(),
            reduced: settings::reduce_motion(cx),
            intro: self.mark_intro,
            frozen: motion::frozen_at(cx),
        };
        // The frame leaves room for the notice when there is one.
        let notice = self.render_keychain_notice(&palette);
        let compact = notice.is_some() && matches!(self.machine.phase(), Phase::Waiting { .. });

        div()
            .id("login")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                // Enter on the screen itself is its main button. With a
                // button focused, Enter is that button's.
                if event.keystroke.key == "enter" && this.focus.is_focused(window) {
                    this.primary(cx);
                }
            }))
            .relative()
            .size_full()
            .flex()
            .bg(palette.background)
            .text_color(palette.text)
            .font_family(fonts::SANS)
            .font_features(features)
            .child(screen_rail(&palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(screen_header("Sign in", step(4), &palette))
                    .child(
                        div()
                            .id("login-scroll")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .px_6()
                            .py_4()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .child(
                                div()
                                    .debug_selector(|| "login-form".into())
                                    .w_full()
                                    .max_w(metrics::FORM_WIDTH())
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    .when(!compact, |this| {
                                        this.child(div().pb_6().child(mark_frame(
                                            "login-mark",
                                            play,
                                            step(0),
                                            &palette,
                                        )))
                                    })
                                    .children(self.notice.clone().map(|text| {
                                        div()
                                            .debug_selector(|| "login-notice".into())
                                            .mb_5()
                                            .w_full()
                                            .p_3()
                                            .rounded(metrics::RADIUS())
                                            .border_1()
                                            .border_color(palette.border)
                                            .flex()
                                            .gap_3()
                                            .child(icon(
                                                IconName::CircleAlert,
                                                px(16.),
                                                palette.warning,
                                            ))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_size(metrics::TEXT_SMALL())
                                                    .line_height(px(19.))
                                                    .text_color(palette.text)
                                                    .child(text),
                                            )
                                    }))
                                    .child(reveal(self.render_phase(&palette, cx), step(1)))
                                    .children(notice)
                                    .child(reveal(
                                        self.render_steps(step(3), &palette).mt_6(),
                                        step(3),
                                    )),
                            ),
                    )
                    .child(reveal(colophon(&palette), step(5))),
            )
            .child(grid_mark_in(
                metrics::RAIL_WIDTH(),
                metrics::HEADER_HEIGHT(),
                step(0),
                &palette,
            ))
    }
}

fn heading(text: &str) -> Div {
    div()
        .text_center()
        .text_size(metrics::TEXT_DISPLAY())
        .font_weight(FontWeight::SEMIBOLD)
        .child(SharedString::from(text.to_owned()))
}

fn body(text: impl Into<SharedString>, palette: &Palette) -> Div {
    div()
        .pt_2()
        .text_center()
        .text_size(metrics::TEXT_BODY())
        .line_height(px(22.))
        .text_color(palette.text_muted)
        .child(text.into())
}
