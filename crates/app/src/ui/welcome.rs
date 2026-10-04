//! The welcome screen: what the application is, before anything is asked.
//!
//! Shown the first time the application starts without an account (and
//! with `--welcome`, for a look). Two steps: the welcome itself, and the
//! choice of provider, which today is wuapi. It belongs to the same family
//! as the sign-in and start screens: the rail, the header rule, the mark
//! in its frame with crosshairs, mono tags, rows split by hairlines.
//!
//! Seeing it through is remembered in the settings (see `ui/app.rs`), so it
//! is not shown again.

use super::conversation::{colophon, fact_row, framed_rows, sized_mark_frame};
use super::widgets::{
    grid_mark_in, led, mono, reveal, screen_header, screen_rail, tag, text_button, MarkPlay,
};
use crate::motion::{self, Entrance};
use crate::product::{MAKER, PRODUCT_NAME};
use crate::settings;
use crate::theme::px;
use crate::theme::{fonts, metrics, palette, Palette};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, Context, Div, EventEmitter, FocusHandle, FontFeatures, FontWeight, KeyDownEvent,
    Subscription, Task, Window,
};
use std::sync::Arc;
use std::time::Duration;

/// The text waits for the mark: its bubble has popped and its `w` is being
/// drawn by then.
const AFTER_THE_MARK: Duration = Duration::from_millis(650);

/// What the screen tells its owner.
pub enum WelcomeEvent {
    /// Seen through: on to the sign-in, or into the application.
    Done,
}

/// Where the welcome stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Step {
    /// What the application is, and "Get started".
    Hello,
    /// Which provider to connect.
    Provider,
}

/// The welcome screen.
pub struct WelcomeScreen {
    pub(super) step: Step,
    /// There is a provider to choose and sign in to. False for the demo
    /// data, where "Get started" simply enters the application.
    choose_provider: bool,
    entrance: Entrance,
    mark_intro: bool,
    focus: FocusHandle,
    _entrance: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<WelcomeEvent> for WelcomeScreen {}

impl WelcomeScreen {
    /// Builds the screen. `choose_provider` adds the provider step.
    pub fn new(choose_provider: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        // Enter is "Get started".
        focus.focus(window, cx);
        let subscriptions = vec![
            cx.on_focus_lost(window, |this, window, cx| this.focus.focus(window, cx)),
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.mark_intro = false;
                }
                cx.notify();
            }),
        ];
        let mut this = Self {
            step: Step::Hello,
            choose_provider,
            entrance: Entrance::default(),
            mark_intro: true,
            focus,
            _entrance: None,
            _subscriptions: subscriptions,
        };
        if let Some(at) = motion::frozen_at(cx) {
            // The pieces are timed from the moment they start to come in.
            this.entrance = Entrance::frozen(at.saturating_sub(AFTER_THE_MARK));
        } else if !settings::reduce_motion(cx) {
            this.play_entrance(AFTER_THE_MARK, cx);
        }
        this
    }

    /// Brings the text in, repainting each frame while it moves and not
    /// once more after.
    fn play_entrance(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.entrance
            .begin_after(cx.background_executor().now(), delay);
        self._entrance = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            // Nothing to draw until the mark has had its moment.
            clock.timer(delay).await;
            loop {
                clock.timer(motion::FRAME).await;
                let running = this.update(cx, |this, cx| {
                    cx.notify();
                    this.entrance.running(clock.now())
                });
                if !matches!(running, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// "Get started": on to the provider, or straight in when there is
    /// none to choose.
    pub(super) fn get_started(&mut self, cx: &mut Context<Self>) {
        if self.choose_provider {
            self.step = Step::Provider;
            self.mark_intro = false;
            if motion::frozen_at(cx).is_none() && !settings::reduce_motion(cx) {
                self.play_entrance(Duration::ZERO, cx);
            }
            cx.notify();
        } else {
            cx.emit(WelcomeEvent::Done);
        }
    }

    /// wuapi it is: the sign-in comes next.
    pub(super) fn choose_wuapi(&mut self, cx: &mut Context<Self>) {
        cx.emit(WelcomeEvent::Done);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.step = Step::Hello;
        self.entrance.skip();
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match (event.keystroke.key.as_str(), self.step) {
            // With a button focused, Enter is that button's.
            ("enter", Step::Hello) if self.focus.is_focused(window) => self.get_started(cx),
            ("enter", Step::Provider) if self.focus.is_focused(window) => self.choose_wuapi(cx),
            // Escape steps back; on the first step there is nothing to
            // leave, and it does nothing.
            ("escape", Step::Provider) => self.back(cx),
            _ => {}
        }
    }

    fn render_hello(
        &self,
        step: &dyn Fn(u32) -> f32,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .child(reveal(
                div()
                    .pt_6()
                    .text_size(metrics::TEXT_HERO())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(PRODUCT_NAME),
                step(0),
            ))
            .child(reveal(
                div()
                    .pt_2()
                    .max_w(metrics::FORM_WIDTH())
                    .text_center()
                    .text_size(metrics::TEXT_BODY())
                    .line_height(px(22.))
                    .text_color(palette.text_muted)
                    .child("A fast, native, open-source WhatsApp client for your wuapi numbers."),
                step(1),
            ))
            // The two keys worth knowing before anything else.
            .child(reveal(
                div()
                    .debug_selector(|| "welcome-keys".into())
                    .pt_2()
                    .text_color(palette.text_muted)
                    .child(mono(format!(
                        "Once you are in: {}",
                        super::hints::keys_line()
                    ))),
                step(1),
            ))
            .child(reveal(
                framed_rows(
                    step(2),
                    palette,
                    [
                        fact_row(
                            "Native",
                            "No browser inside. Chats open from this computer, at once.",
                            false,
                            palette,
                        ),
                        fact_row(
                            "Drop-proof",
                            "Messages queue and go out when the connection is back.",
                            true,
                            palette,
                        ),
                        fact_row(
                            "Open source",
                            "Apache-2.0, with pluggable providers. wuapi is the first.",
                            true,
                            palette,
                        ),
                    ],
                )
                .mt_6(),
                step(2),
            ))
            .child(reveal(
                div().pt_6().child(
                    text_button("get-started", "Get started", None, true, palette)
                        .on_click(cx.listener(|this, _, _, cx| this.get_started(cx))),
                ),
                step(3),
            ))
    }

    fn render_provider(
        &self,
        step: &dyn Fn(u32) -> f32,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let row = |ruled: bool| {
            div()
                .px_4()
                .py_3()
                .when(ruled, |this| this.border_t_1().border_color(palette.border))
                .flex()
                .items_center()
                .gap_3()
        };
        let lines = |title: &'static str, detail: &'static str, muted: bool| {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(metrics::TEXT_BODY())
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(if muted {
                            palette.text_faint
                        } else {
                            palette.text
                        })
                        .child(title),
                )
                .child(
                    div()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(19.))
                        .text_color(if muted {
                            palette.text_faint
                        } else {
                            palette.text_muted
                        })
                        .child(detail),
                )
        };
        div()
            .w_full()
            .flex()
            .flex_col()
            .items_center()
            .child(reveal(
                div()
                    .pt_6()
                    .text_size(metrics::TEXT_DISPLAY())
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Choose how to connect"),
                step(0),
            ))
            .child(reveal(
                div()
                    .pt_2()
                    .max_w(metrics::FORM_WIDTH())
                    .text_center()
                    .text_size(metrics::TEXT_BODY())
                    .line_height(px(22.))
                    .text_color(palette.text_muted)
                    .child("A provider is what links your WhatsApp numbers to this client."),
                step(1),
            ))
            .child(reveal(
                framed_rows(
                    step(2),
                    palette,
                    [
                        // The one provider there is, already chosen.
                        row(false)
                            .debug_selector(|| "provider-wuapi".into())
                            .bg(palette.muted)
                            .child(led(palette.accent))
                            .child(lines(
                                MAKER,
                                "A hosted WhatsApp API. You sign in from the browser.",
                                false,
                            ))
                            .child(tag("Default", None, palette)),
                        // Said, not hidden: there will be others, there are
                        // none yet, and this is how one is written.
                        row(true)
                            .debug_selector(|| "provider-other".into())
                            .child(led(palette.border))
                            .child(lines(
                                "Other providers",
                                "wuapi is the only adapter today. Writing one is one trait: \
                                 docs/PROVIDERS.md.",
                                true,
                            ))
                            .child(
                                mono("NOT AVAILABLE YET")
                                    .text_size(px(9.5))
                                    .text_color(palette.text_faint),
                            ),
                    ],
                )
                .mt_6(),
                step(2),
            ))
            .child(reveal(
                div()
                    .pt_6()
                    .flex()
                    .gap_2()
                    .child(
                        text_button("welcome-back", "Back", None, false, palette)
                            .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
                    )
                    .child(
                        text_button("choose-wuapi", "Continue with wuapi", None, true, palette)
                            .on_click(cx.listener(|this, _, _, cx| this.choose_wuapi(cx))),
                    ),
                step(3),
            ))
    }
}

impl Render for WelcomeScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = palette(cx);
        let features = FontFeatures(Arc::new(
            fonts::SANS_FEATURES
                .iter()
                .map(|(tag, value)| ((*tag).to_owned(), *value))
                .collect(),
        ));
        let now = cx.background_executor().now();
        let entrance = self.entrance;
        let step = move |step: u32| entrance.reveal(step, now);
        let play = MarkPlay {
            paused: !window.is_window_active(),
            reduced: settings::reduce_motion(cx),
            intro: self.mark_intro,
            frozen: motion::frozen_at(cx),
        };
        let content = match self.step {
            Step::Hello => self.render_hello(&step, &palette, cx),
            Step::Provider => self.render_provider(&step, &palette, cx),
        };

        div()
            .id("welcome")
            .track_focus(&self.focus)
            .on_key_down(
                cx.listener(|this, event: &KeyDownEvent, window, cx| this.key(event, window, cx)),
            )
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
                    .child(screen_header("Welcome", step(4), &palette))
                    .child(
                        div()
                            .id("welcome-scroll")
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
                                    .debug_selector(|| "welcome-form".into())
                                    .w_full()
                                    .max_w(metrics::FORM_WIDTH() + px(40.))
                                    .flex()
                                    .flex_col()
                                    .items_center()
                                    // The mark is the hero: it is there
                                    // from the first frame, popping in on
                                    // its own, and the text follows.
                                    .child(sized_mark_frame(
                                        "welcome-mark",
                                        metrics::HERO_FRAME(),
                                        metrics::HERO_MARK(),
                                        play,
                                        1.,
                                        &palette,
                                    ))
                                    .child(content),
                            ),
                    )
                    .child(reveal(colophon(&palette), step(5))),
            )
            .child(grid_mark_in(
                metrics::RAIL_WIDTH(),
                metrics::HEADER_HEIGHT(),
                1.,
                &palette,
            ))
    }
}
