//! Recording a voice note in the open conversation: the bar that takes
//! the composer's place, and what it does.
//!
//! A click on the microphone (or its shortcut) starts recording, hands
//! free: nothing is held down. While it records, the bar says so in a way
//! that cannot be missed, with the time and a live meter; it can be
//! paused, sent, or thrown away. Stopped, it can be listened to before it
//! is sent. The microphone is open only while it records: it is closed
//! the moment the recording is stopped, sent or thrown away, and what is
//! thrown away is gone.
//!
//! Sending goes the way every file goes: the encoded note is copied into
//! the store with its pending bubble and the outbox uploads and sends it,
//! through drops, like any attachment.

use super::shell::Shell;
use super::widgets::{icon_button, mono};
use crate::audio::{Playback, Shape};
use crate::icons::{icon, IconName};
use crate::keys::{self, Command};
use crate::record::{self, Recorder, Take};
use crate::theme::{metrics, px, Palette};
use client_core::NewMedia;
use client_provider::MediaKind;
use gpui_kit::prelude::*;
use gpui_kit::{div, AnyElement, Context, Div, SharedString, Task, Window};
use std::sync::Arc;
use std::time::Duration;

/// The id the take goes by in the player while it is listened to.
pub(super) const PREVIEW: &str = "recording:preview";
/// A recording longer than this is not thrown away on one Escape.
pub(super) const ASK_ABOVE: Duration = Duration::from_secs(3);
/// How often the bar is repainted while it records.
const TICK: Duration = Duration::from_millis(50);
/// Bars of the live meter.
const METER: usize = 40;
/// Why the microphone button does nothing where files cannot be sent.
pub(super) const NOT_AVAILABLE: &str =
    "Voice notes cannot be sent: sending files is not available yet";

/// The system's microphone. In the tests there is none: a test that has
/// not brought its own can never open a real one.
pub(super) fn system_input() -> Box<dyn record::AudioInput> {
    #[cfg(not(test))]
    {
        Box::<record::SystemInput>::default()
    }
    #[cfg(test)]
    {
        struct NoInput;
        impl record::AudioInput for NoInput {
            fn devices(&self) -> Vec<String> {
                Vec::new()
            }
            fn open(&mut self, _: Option<&str>, _: record::Sink) -> Result<record::Format, String> {
                Err("No microphone in the tests.".to_owned())
            }
            fn close(&mut self) {}
        }
        Box::new(NoInput)
    }
}

/// Where a recording stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    /// None under way.
    Idle,
    /// The microphone is open.
    Recording,
    /// Stopped: it can be listened to, sent or thrown away.
    Preview,
    /// Being encoded and queued.
    Sending,
}

/// The recording of the open conversation.
pub(super) struct VoiceDeck {
    pub(super) recorder: Recorder,
    pub(super) phase: Phase,
    /// The stopped recording.
    pub(super) take: Option<Take>,
    /// What went wrong, in words: shown in the bar.
    pub(super) error: Option<SharedString>,
    /// Escape was pressed on a recording worth asking about.
    pub(super) asking: bool,
    /// The input device chosen in the settings; `None` is the system's.
    pub(super) device: Option<String>,
    /// The microphone is open for the settings' level test.
    pub(super) testing: bool,
    _tick: Option<Task<()>>,
    _work: Option<Task<()>>,
}

impl VoiceDeck {
    pub(super) fn new(recorder: Recorder, device: Option<String>) -> Self {
        Self {
            recorder,
            phase: Phase::Idle,
            take: None,
            error: None,
            asking: false,
            device,
            testing: false,
            _tick: None,
            _work: None,
        }
    }

    /// Whether the bar is in the composer's place.
    pub(super) fn shown(&self) -> bool {
        self.phase != Phase::Idle || self.error.is_some()
    }

    /// How long the recording is so far.
    pub(super) fn length(&self) -> Duration {
        match &self.take {
            Some(take) => take.duration(),
            None => self.recorder.elapsed(),
        }
    }
}

/// Minutes and seconds.
fn minutes(length: Duration) -> String {
    let seconds = length.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

impl Shell {
    /// Whether a voice note recorded now could be sent.
    pub(super) fn can_record(&self) -> bool {
        self.open.is_some() && self.can_attach()
    }

    /// Whether the keys of the recording bar apply.
    pub(super) fn recording_has_keyboard(&self, window: &Window) -> bool {
        matches!(self.voice.phase, Phase::Recording | Phase::Preview)
            && self.overlay == super::shell::Overlay::None
            && self.focus.is_focused(window)
    }

    /// The microphone button, and its shortcut: starts a recording, or
    /// stops the one under way so that it can be listened to.
    pub(super) fn toggle_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.voice.phase {
            Phase::Idle => self.start_recording(window, cx),
            Phase::Recording => self.stop_recording(cx),
            Phase::Preview | Phase::Sending => return false,
        }
        true
    }

    /// Opens the microphone and starts recording.
    pub(super) fn start_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.open.is_none() {
            return;
        }
        // Nothing is recorded that could not be sent.
        if !self.can_record() {
            return self.show_problem(format!("{NOT_AVAILABLE}."), cx);
        }
        self.end_level_test();
        // What is playing would be recorded with the voice.
        self.audio.player.pause();
        self.voice.error = None;
        self.voice.asking = false;
        self.voice.take = None;
        match self.voice.recorder.start(self.voice.device.as_deref()) {
            Ok(()) => {
                self.voice.phase = Phase::Recording;
                self.keys.clear();
                self.pane_list = false;
                // The bar's keys are the shell's: Enter, Escape, Space.
                self.focus.focus(window, cx);
                self.tick_recording(cx);
            }
            // No microphone, or not allowed: said in the bar.
            Err(reason) => {
                self.voice.phase = Phase::Idle;
                self.voice.error = Some(reason.into());
            }
        }
        cx.notify();
    }

    /// While the microphone is open: takes in what it heard, repaints the
    /// time and the meter, and stops at the longest a note can be.
    fn tick_recording(&mut self, cx: &mut Context<Self>) {
        self.voice._tick = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            loop {
                clock.timer(TICK).await;
                let open = this.update(cx, |this, cx| {
                    if !this.voice.recorder.is_open() {
                        return false;
                    }
                    let full = this.voice.recorder.pump();
                    if full && this.voice.phase == Phase::Recording {
                        this.stop_recording(cx);
                        this.voice.error = Some(
                            format!(
                                "A voice note can be {} minutes long: this one stopped there.",
                                record::LONGEST.as_secs() / 60
                            )
                            .into(),
                        );
                    }
                    cx.notify();
                    this.voice.recorder.is_open()
                });
                if !matches!(open, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// Pauses the recording, or goes on with it.
    pub(super) fn pause_recording(&mut self, cx: &mut Context<Self>) {
        if self.voice.phase != Phase::Recording {
            return;
        }
        if self.voice.recorder.is_paused() {
            self.voice.recorder.resume();
        } else {
            self.voice.recorder.pause();
        }
        self.voice.asking = false;
        cx.notify();
    }

    /// Closes the microphone; the recording stays, to be listened to.
    pub(super) fn stop_recording(&mut self, cx: &mut Context<Self>) {
        if self.voice.phase != Phase::Recording {
            return;
        }
        let take = self.voice.recorder.stop();
        self.voice._tick = None;
        self.audio
            .clips
            .insert(PREVIEW.to_owned(), Arc::new(take.clip()));
        self.voice.take = Some(take);
        self.voice.phase = Phase::Preview;
        self.voice.asking = false;
        cx.notify();
    }

    /// Plays the stopped recording, or pauses it.
    pub(super) fn play_take(&mut self, cx: &mut Context<Self>) {
        if self.voice.phase != Phase::Preview {
            return;
        }
        match self.audio.player.playback(PREVIEW) {
            Playback::Playing(_) => self.audio.player.pause(),
            _ => self.play_clip(PREVIEW, cx),
        }
        cx.notify();
    }

    /// Escape, or the bin: throws the recording away. One that is worth
    /// something is asked about first, unless `sure`.
    pub(super) fn discard_recording(
        &mut self,
        sure: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.voice.phase == Phase::Sending {
            return;
        }
        if !sure && !self.voice.asking && self.voice.length() > ASK_ABOVE {
            self.voice.asking = true;
            return cx.notify();
        }
        self.erase_recording();
        if self.open.is_some() {
            self.composer
                .update(cx, |composer, cx| composer.focus(window, cx));
        }
        cx.notify();
    }

    /// The recording is gone: the microphone closed, the sound erased.
    fn erase_recording(&mut self) {
        self.voice.recorder.cancel();
        self.voice._tick = None;
        if let Some(mut take) = self.voice.take.take() {
            take.erase();
        }
        if self.audio.player.current() == Some(PREVIEW) {
            self.audio.player.stop();
        }
        self.audio.clips.remove(PREVIEW);
        self.voice.phase = Phase::Idle;
        self.voice.asking = false;
        self.voice.error = None;
    }

    /// The conversation is left: a recording does not follow to another.
    pub(super) fn leave_recording(&mut self) {
        if self.voice.phase != Phase::Sending {
            self.erase_recording();
        }
        self.end_level_test();
    }

    /// Enter, or the send button: the note goes out.
    pub(super) fn send_recording(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = &self.open else { return };
        let (account, chat) = (open.chat.account_id.clone(), open.chat.id.clone());
        let take = match self.voice.phase {
            Phase::Recording => {
                self.voice._tick = None;
                self.voice.recorder.stop()
            }
            Phase::Preview => match self.voice.take.take() {
                Some(take) => take,
                None => return,
            },
            Phase::Idle | Phase::Sending => return,
        };
        if self.audio.player.current() == Some(PREVIEW) {
            self.audio.player.stop();
        }
        self.audio.clips.remove(PREVIEW);
        self.voice.asking = false;
        if take.duration() < record::SHORTEST {
            // A slip of the finger: nothing to send, and it is said.
            self.voice.phase = Phase::Idle;
            self.voice.error = Some("Too short to send: nothing was sent.".into());
            self.composer
                .update(cx, |composer, cx| composer.focus(window, cx));
            return cx.notify();
        }
        self.voice.phase = Phase::Sending;
        self.voice.error = None;
        let engine = self.engine.clone();
        let window = window.window_handle();
        self.voice._work = Some(cx.spawn(async move |this, cx| {
            // Encoding is CPU work: off the interface's thread.
            let made = cx
                .background_spawn(async move {
                    let encoded = take.encode();
                    (take, encoded)
                })
                .await;
            this.update(cx, |this, cx| {
                let (mut take, encoded) = made;
                let queued = encoded.and_then(|bytes| {
                    engine
                        .send_media(
                            &account,
                            &chat,
                            NewMedia {
                                kind: MediaKind::Voice,
                                bytes,
                                mime_type: record::MIME.to_owned(),
                                file_name: None,
                                caption: None,
                                reply_to: None,
                                mentions: Vec::new(),
                            },
                        )
                        .map_err(|error| error.to_string())
                });
                match queued {
                    Ok(client_id) => {
                        // Its length and its shape are known here: the
                        // bubble shows them at once, and it plays from
                        // the copy in hand.
                        let local = client_core::local_media_ref(&client_id);
                        let clip = take.clip();
                        this.media.keep_shape(local.as_str(), Shape::of(&clip));
                        this.audio.clips.insert(local.to_string(), Arc::new(clip));
                        take.erase();
                        this.voice.phase = Phase::Idle;
                        if let Some(open) = &this.open {
                            open.list.scroll_to_end();
                        }
                    }
                    // Not lost: it can be sent again, or thrown away.
                    Err(reason) => {
                        this.audio
                            .clips
                            .insert(PREVIEW.to_owned(), Arc::new(take.clip()));
                        this.voice.take = Some(take);
                        this.voice.phase = Phase::Preview;
                        this.voice.error = Some(reason.into());
                    }
                }
                cx.notify();
            })
            .ok();
            // The keyboard goes back to the composer once it is on its way.
            window
                .update(cx, |_, window, cx| {
                    this.update(cx, |this, cx| {
                        if this.voice.phase == Phase::Idle && this.open.is_some() {
                            this.composer
                                .update(cx, |composer, cx| composer.focus(window, cx));
                        }
                    })
                    .ok();
                })
                .ok();
        }));
        cx.notify();
    }

    /// Runs a command of the recording. `None`: not one of them.
    pub(super) fn run_record_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        Some(match command {
            Command::RecordVoice => self.toggle_recording(window, cx),
            Command::RecordSend => {
                self.send_recording(window, cx);
                true
            }
            Command::RecordDiscard => {
                self.discard_recording(false, window, cx);
                true
            }
            Command::RecordPause => {
                match self.voice.phase {
                    Phase::Recording => self.pause_recording(cx),
                    Phase::Preview => self.play_take(cx),
                    _ => return Some(false),
                }
                true
            }
            _ => return None,
        })
    }

    // ----- the settings' level test -----------------------------------------

    /// Opens the microphone to show its level, or closes it. Nothing of
    /// what it hears is kept.
    pub(super) fn toggle_level_test(&mut self, cx: &mut Context<Self>) {
        if self.voice.testing {
            self.end_level_test();
            return cx.notify();
        }
        if self.voice.phase != Phase::Idle {
            return;
        }
        match self.voice.recorder.start(self.voice.device.as_deref()) {
            Ok(()) => {
                self.voice.testing = true;
                self.voice.error = None;
                self.tick_level_test(cx);
            }
            Err(reason) => self.voice.error = Some(reason.into()),
        }
        cx.notify();
    }

    /// The level test ends: the microphone is closed, nothing is kept.
    pub(super) fn end_level_test(&mut self) {
        if std::mem::take(&mut self.voice.testing) {
            self.voice.recorder.cancel();
            self.voice._tick = None;
        }
    }

    fn tick_level_test(&mut self, cx: &mut Context<Self>) {
        self.voice._tick = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            // Ten seconds, then it closes by itself.
            for _ in 0..200 {
                clock.timer(TICK).await;
                let going = this.update(cx, |this, cx| {
                    if !this.voice.testing || this.overlay != super::shell::Overlay::Settings {
                        this.end_level_test();
                        cx.notify();
                        return false;
                    }
                    this.voice.recorder.pump();
                    cx.notify();
                    true
                });
                if !matches!(going, Ok(true)) {
                    return;
                }
            }
            this.update(cx, |this, cx| {
                this.end_level_test();
                cx.notify();
            })
            .ok();
        }));
    }

    /// The level the microphone hears right now, from 0 to 1.
    pub(super) fn input_level(&self) -> f32 {
        self.voice
            .recorder
            .levels()
            .last()
            .map_or(0., |level| f32::from(*level) / 255.)
    }

    // ----- the bar ------------------------------------------------------------

    /// The bar in the composer's place while a recording is under way,
    /// stopped, or could not start.
    pub(super) fn render_record_bar(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.voice.shown() {
            return None;
        }
        let phase = self.voice.phase.clone();
        let recording = phase == Phase::Recording;
        let paused = recording && self.voice.recorder.is_paused();
        let length = self.voice.length();
        let hint = |command: Command, text: &'static str| -> SharedString {
            match keys::keys_label(command) {
                Some(keys) => format!("{text} ({keys})").into(),
                None => text.into(),
            }
        };
        let tip = |button: gpui_kit::Stateful<Div>, hint: SharedString| {
            button.tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
            })
        };

        // What is said above the bar: the question, or what went wrong.
        let line = if self.voice.asking {
            Some((
                "record-ask",
                SharedString::from(
                    "Throw this recording away? Escape again does; anything else keeps it.",
                ),
                palette.text,
            ))
        } else {
            self.voice
                .error
                .clone()
                .map(|error| ("record-error", error, palette.danger))
        };
        let line = line.map(|(selector, text, colour)| {
            div()
                .debug_selector(move || selector.into())
                .px_2()
                .pb_2()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(18.))
                .text_color(colour)
                .child(text)
        });

        // Nothing under way: only what went wrong, and a way to dismiss it.
        if phase == Phase::Idle {
            return Some(
                div()
                    .debug_selector(|| "composer".into())
                    .flex_none()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(palette.border)
                    .child(
                        div()
                            .debug_selector(|| "record-bar".into())
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(icon(IconName::CircleAlert, px(16.), palette.danger))
                            .child(
                                div()
                                    .debug_selector(|| "record-error".into())
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(metrics::TEXT_SMALL())
                                    .line_height(px(18.))
                                    .text_color(palette.text)
                                    .child(self.voice.error.clone().unwrap_or_default()),
                            )
                            .child(
                                icon_button("record-dismiss", IconName::X, palette).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.discard_recording(true, window, cx)
                                    }),
                                ),
                            ),
                    )
                    .into_any_element(),
            );
        }

        // The meter while it records; the take's own shape once stopped,
        // with what has been heard of it in the accent.
        let playback = self.audio.player.playback(PREVIEW);
        let heard = match (playback, self.voice.take.as_ref()) {
            (Playback::Playing(at) | Playback::Paused(at), Some(take)) => {
                at.as_secs_f32() / take.duration().as_secs_f32().max(0.001)
            }
            _ => 0.,
        };
        let bars: Vec<u8> = match &self.voice.take {
            Some(take) => {
                let shape = Shape::of(&take.clip());
                (0..METER)
                    .map(|bar| shape.bars[bar * shape.bars.len() / METER])
                    .collect()
            }
            None => {
                let levels = self.voice.recorder.levels();
                let from = levels.len().saturating_sub(METER);
                let mut bars = vec![0; METER - (levels.len() - from)];
                bars.extend_from_slice(&levels[from..]);
                bars
            }
        };
        let stopped = self.voice.take.is_some();
        let meter = div()
            .debug_selector(|| "record-levels".into())
            .flex_1()
            .min_w_0()
            .h(metrics::CONTROL())
            .overflow_hidden()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(2.))
            .children(bars.into_iter().enumerate().map(|(index, level)| {
                let tall = px(3.) + px(21.) * (f32::from(level) / 255.);
                let lit = stopped && (index as f32 + 0.5) / METER as f32 <= heard;
                div()
                    .flex_none()
                    .w(px(2.))
                    .h(tall)
                    .rounded_full()
                    .bg(if lit {
                        palette.accent
                    } else {
                        palette.text_muted
                    })
            }));

        let (label, light) = match (&phase, paused) {
            (Phase::Recording, false) => ("REC", palette.danger),
            (Phase::Recording, true) => ("PAUSED", palette.text_faint),
            (Phase::Sending, _) => ("SENDING", palette.text_faint),
            _ => ("READY", palette.text_faint),
        };
        let middle = match phase {
            Phase::Recording => tip(
                icon_button(
                    "record-pause",
                    if paused {
                        IconName::Mic
                    } else {
                        IconName::Pause
                    },
                    palette,
                ),
                hint(
                    Command::RecordPause,
                    if paused { "Go on recording" } else { "Pause" },
                ),
            )
            .on_click(cx.listener(|this, _, _, cx| this.pause_recording(cx))),
            _ => tip(
                icon_button(
                    "record-play",
                    if matches!(playback, Playback::Playing(_)) {
                        IconName::Pause
                    } else {
                        IconName::Play
                    },
                    palette,
                ),
                hint(Command::RecordPause, "Listen to it"),
            )
            .on_click(cx.listener(|this, _, _, cx| this.play_take(cx))),
        };
        let sending = phase == Phase::Sending;
        Some(
            div()
                .debug_selector(|| "composer".into())
                .flex_none()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(palette.border)
                .children(line)
                .child(
                    div()
                        .debug_selector(|| "record-bar".into())
                        .p(px(5.))
                        .rounded(metrics::COMPOSER_RADIUS())
                        .border_1()
                        // The bar wears the colour of the light while the
                        // microphone is open.
                        .border_color(if recording && !paused {
                            palette.danger
                        } else {
                            palette.border
                        })
                        .bg(palette.surface)
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            tip(
                                icon_button("record-discard", IconName::Trash, palette),
                                hint(Command::RecordDiscard, "Throw it away"),
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| this.discard_recording(false, window, cx),
                            )),
                        )
                        .child(
                            div()
                                .debug_selector(|| "record-light".into())
                                .flex_none()
                                .size(px(10.))
                                .rounded_full()
                                .bg(light),
                        )
                        .child(
                            mono(label)
                                .debug_selector(|| "record-state".into())
                                .flex_none()
                                .text_color(if recording && !paused {
                                    palette.danger
                                } else {
                                    palette.text_muted
                                }),
                        )
                        .child(
                            mono(minutes(length))
                                .debug_selector(|| "record-time".into())
                                .flex_none()
                                .text_color(palette.text),
                        )
                        .child(meter)
                        .child(middle)
                        .child(if recording {
                            tip(
                                icon_button("record-stop", IconName::Square, palette),
                                hint(Command::RecordVoice, "Stop, and listen before sending"),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.stop_recording(cx)))
                            .into_any_element()
                        } else {
                            div().into_any_element()
                        })
                        .child(
                            // The one accent control on the screen.
                            tip(
                                div()
                                    .id("record-send")
                                    .debug_selector(|| "record-send".into())
                                    .flex_none()
                                    .size(metrics::CONTROL())
                                    .rounded_full()
                                    .bg(palette.accent_fill)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .cursor_pointer()
                                    .tab_index(0)
                                    .when(sending, |this| this.opacity(0.45))
                                    .hover(|style| style.opacity(0.85))
                                    .child(icon(
                                        IconName::SendHorizontal,
                                        px(16.),
                                        palette.on_accent_fill,
                                    )),
                                hint(Command::RecordSend, "Send"),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send_recording(window, cx)),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }
}

// ----- Settings > Audio ---------------------------------------------------------

/// The file that remembers the devices chosen, next to the settings.
const DEVICES_FILE: &str = "audio.json";

/// The devices chosen in the settings. `None` is the system's default.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub(super) struct AudioDevices {
    pub(super) input: Option<String>,
    pub(super) output: Option<String>,
}

impl AudioDevices {
    /// What was chosen, as it was left.
    pub(super) fn load(cx: &gpui_kit::App) -> Self {
        crate::settings::sibling(DEVICES_FILE, cx)
            .and_then(|file| std::fs::read(file).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, cx: &gpui_kit::App) {
        let Some(file) = crate::settings::sibling(DEVICES_FILE, cx) else {
            return;
        };
        let saved = serde_json::to_vec_pretty(self)
            .map_err(|error| error.to_string())
            .and_then(|bytes| std::fs::write(file, bytes).map_err(|error| error.to_string()));
        if let Err(error) = saved {
            tracing::warn!(%error, "could not remember the audio devices");
        }
    }
}

/// The device `by` places from `chosen` among the system's default and
/// `names`, going round.
fn step_device(chosen: Option<&str>, names: &[String], by: usize) -> Option<String> {
    let at = chosen
        .and_then(|chosen| names.iter().position(|name| name == chosen))
        .map_or(0, |at| at + 1);
    match (at + by) % (names.len() + 1) {
        0 => None,
        place => Some(names[place - 1].clone()),
    }
}

impl Shell {
    /// The devices chosen, as the deck and the player hold them.
    fn audio_devices(&self) -> AudioDevices {
        AudioDevices {
            input: self.voice.device.clone(),
            output: self.audio.output_device.clone(),
        }
    }

    /// Takes the devices from the settings' file: at the start.
    pub(super) fn load_audio_devices(&mut self, cx: &gpui_kit::App) {
        let devices = AudioDevices::load(cx);
        self.voice.device = devices.input;
        self.audio.player.set_output_device(devices.output.clone());
        self.audio.output_device = devices.output;
    }

    /// The next or the previous microphone, or speakers.
    pub(super) fn step_audio_device(&mut self, input: bool, forward: bool, cx: &mut Context<Self>) {
        let names = if input {
            self.voice.recorder.devices()
        } else {
            self.audio.player.output_devices()
        };
        let by = if forward { 1 } else { names.len() };
        if input {
            // A level test on the old one ends with it.
            self.end_level_test();
            self.voice.device = step_device(self.voice.device.as_deref(), &names, by);
        } else {
            let next = step_device(self.audio.output_device.as_deref(), &names, by);
            self.audio.player.set_output_device(next.clone());
            self.audio.output_device = next;
        }
        self.audio_devices().save(cx);
        cx.notify();
    }

    /// Settings > Audio: the microphone, the speakers, and a level test.
    pub(super) fn render_audio(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let stepper = |name: &'static str,
                       chosen: Option<&str>,
                       input: bool,
                       cx: &mut Context<Self>| {
            let label: SharedString = chosen.unwrap_or("System default").to_owned().into();
            let step =
                |id: &'static str, glyph: IconName, forward: bool, cx: &mut Context<Self>| {
                    icon_button(id, glyph, palette).on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.step_audio_device(input, forward, cx);
                    }))
                };
            let (previous, next, value): (&'static str, &'static str, &'static str) = if input {
                (
                    "audio-input-previous",
                    "audio-input-next",
                    "audio-input-value",
                )
            } else {
                (
                    "audio-output-previous",
                    "audio-output-next",
                    "audio-output-value",
                )
            };
            div()
                .debug_selector(move || name.into())
                .flex_none()
                .p(px(2.))
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(palette.border)
                .bg(palette.surface)
                .flex()
                .items_center()
                .gap(px(2.))
                .child(step(previous, IconName::ChevronLeft, false, cx))
                .child(
                    div()
                        .debug_selector(move || value.into())
                        .w(px(200.))
                        .px_1()
                        .truncate()
                        .text_center()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text)
                        .child(label),
                )
                .child(step(next, IconName::ChevronRight, true, cx))
        };
        let row = |title: &'static str, detail: &'static str, control: Div| {
            div()
                .py_2()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .text_size(metrics::TEXT_BODY())
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(metrics::TEXT_SMALL())
                                .line_height(px(18.))
                                .text_color(palette.text_muted)
                                .child(detail),
                        ),
                )
                .child(control)
        };
        let testing = self.voice.testing;
        let level = if testing { self.input_level() } else { 0. };
        let test = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_2()
            .child(
                // The level: a track, and how much of it the voice fills.
                div()
                    .debug_selector(|| "audio-level".into())
                    .w(px(120.))
                    .h(px(6.))
                    .rounded_full()
                    .bg(palette.muted)
                    .child(
                        div()
                            .debug_selector(|| "audio-level-fill".into())
                            .h_full()
                            .w(gpui_kit::relative(level.clamp(0., 1.)))
                            .rounded_full()
                            .bg(palette.accent),
                    ),
            )
            .child(
                div()
                    .id("audio-test")
                    .debug_selector(|| "audio-test".into())
                    .h(metrics::CONTROL())
                    .px_3()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(if testing {
                        palette.danger
                    } else {
                        palette.border
                    })
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .tab_index(0)
                    .hover(|style| style.opacity(0.85))
                    .when(testing, |this| {
                        // Unmistakable while the microphone is open.
                        this.child(
                            div()
                                .debug_selector(|| "audio-test-light".into())
                                .size(px(8.))
                                .rounded_full()
                                .bg(palette.danger),
                        )
                    })
                    .child(mono(if testing { "STOP" } else { "TEST" }))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_level_test(cx);
                    })),
            );
        let input = stepper("audio-input", self.voice.device.as_deref(), true, cx);
        let output = stepper(
            "audio-output",
            self.audio.output_device.as_deref(),
            false,
            cx,
        );
        div()
            .px_4()
            .pt_4()
            .pb_4()
            .flex()
            .flex_col()
            .child(
                div()
                    .pb_2()
                    .child(mono("[ AUDIO ]").text_color(palette.text_muted)),
            )
            .child(row(
                "Microphone",
                "What voice notes are recorded with.",
                input,
            ))
            .child(row(
                "Microphone test",
                "Opens the microphone to show its level. Nothing is kept, and it closes by \
                 itself after ten seconds.",
                test,
            ))
            .children(
                self.voice
                    .error
                    .clone()
                    .filter(|_| self.voice.phase == Phase::Idle)
                    .map(|error| {
                        div()
                            .debug_selector(|| "audio-error".into())
                            .pb_2()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(18.))
                            .text_color(palette.danger)
                            .child(error)
                    }),
            )
            .child(row(
                "Speakers",
                "What voice notes and audio are played on.",
                output,
            ))
    }
}
