//! A voice note or other audio in a bubble.
//!
//! One compact row of a fixed width: the round button (the bubble's accent
//! control), the waveform to seek on, and the speed once the clip is in
//! the player. Under it, the length on the left and the message's time
//! and tick on the right.
//!
//! The button is also where a file that is not here yet says so: an arrow
//! (with the size under the waveform when the message carries it), a ring
//! while it downloads, a retry when it could not be played. The waveform
//! is the real one once the clip has been decoded; before that it is a row
//! of low, even bars, which claims nothing about the sound, in the same
//! box, so the bubble does not change size when the real one arrives.
//!
//! Presentation only: what a click does is the player's (`Shell`).

use super::bubble::{RowContext, Side};
use super::media::FileState;
use super::transfer::ring;
use super::widgets::mono;
use crate::audio::{Playback, BARS};
use crate::format::{duration, file_size};
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_provider::{Media, MediaKind, Message};
use gpui_kit::prelude::*;
use gpui_kit::{div, relative, Div, SharedString};

/// How tall the box of the waveform is, in design pixels.
const WAVE_HEIGHT: f32 = 30.;
/// The tallest and the shortest bar of a decoded waveform.
const BAR_MAX: f32 = 26.;
const BAR_MIN: f32 = 3.;
/// Every bar of a waveform that has not been decoded: low and even.
const BAR_IDLE: f32 = 5.;
/// The dot that marks the position on the waveform.
const KNOB: f32 = 9.;

/// What the round button shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Button {
    /// The provider has no file for this message.
    Missing,
    /// Could not be played: a click tries again.
    Retry,
    /// Being fetched or decoded: a ring, as much of it filled as has
    /// arrived when the size is known.
    Loading(Option<f32>),
    /// Playing: a click pauses.
    Pause,
    /// Not here yet: a click fetches it and plays.
    Download,
    /// Ready: a click plays.
    Play,
}

impl Button {
    fn glyph(self) -> IconName {
        match self {
            Self::Missing => IconName::Ban,
            Self::Retry => IconName::RotateCw,
            Self::Loading(_) | Self::Download => IconName::ArrowDownToLine,
            Self::Pause => IconName::Pause,
            Self::Play => IconName::Play,
        }
    }

    /// The name the button's state goes by in tests.
    fn selector(self) -> &'static str {
        match self {
            Self::Missing => "audio-missing",
            Self::Retry => "audio-retry",
            Self::Loading(_) => "audio-loading",
            Self::Pause => "audio-pause",
            Self::Download => "audio-download",
            Self::Play => "audio-play",
        }
    }
}

/// A voice note or other audio: play/pause, the waveform to seek on, the
/// speed, and under them the length and the message's time.
pub(super) fn audio_block(
    media: &Media,
    message: &Message,
    side: &Side,
    palette: &Palette,
    context: &RowContext,
    meta: Div,
) -> Div {
    let url = media.source.as_ref().map(|source| source.to_string());
    let audio = &context.audio;
    let playback = match (&audio.current, &url) {
        (Some((current, playback)), Some(url)) if current == url => *playback,
        _ => Playback::Idle,
    };
    let shape = url.as_deref().and_then(|url| context.shelf.shape(url));
    let waiting = url.is_some() && audio.waiting == url;
    let failure = url.as_ref().and_then(|url| audio.failed.get(url)).cloned();
    let no_output = match (&audio.no_output, &url) {
        (Some((clip, reason)), Some(url)) if clip == url => Some(reason.clone()),
        _ => None,
    };
    let file = url.as_deref().map(|url| context.shelf.file_state(url));
    if let (Some(url), true) = (&url, context.shelf.fetches_everything()) {
        // "Everything": the file is fetched ahead of the click.
        if file == Some(FileState::NotFetched) {
            context.shelf.prefetch(&message.account_id, url);
        }
    }

    // The length: what the provider said, else what decoding found.
    let total = media
        .duration_secs
        .map(|secs| std::time::Duration::from_secs(u64::from(secs)))
        .or(shape.as_ref().map(|shape| shape.duration));
    let (position, active) = match playback {
        Playback::Playing(at) | Playback::Paused(at) => (Some(at), true),
        Playback::Idle => (None, false),
    };
    let progress = match (position, total) {
        (Some(at), Some(total)) if !total.is_zero() => {
            (at.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.)
        }
        _ => 0.,
    };

    // The button: the accent control of the bubble.
    let not_here = file == Some(FileState::NotFetched) && !context.shelf.fetches_everything();
    let state = match &url {
        None => Button::Missing,
        Some(_) if failure.is_some() || no_output.is_some() => Button::Retry,
        Some(url) if waiting => Button::Loading(context.shelf.file_fraction(url, media)),
        Some(_) if matches!(playback, Playback::Playing(_)) => Button::Pause,
        Some(_) if not_here && !active => Button::Download,
        Some(_) => Button::Play,
    };
    let (fill, ink) = match state {
        // Nothing to press: quiet.
        Button::Missing => (side.quote, side.meta),
        _ => (side.play, side.on_play),
    };
    let selector = state.selector();
    let button = div()
        .id(SharedString::from(format!("play-{}", message.id)))
        .debug_selector(|| "audio-button".into())
        .relative()
        .flex_none()
        .size(metrics::AUDIO_BUTTON())
        .rounded_full()
        .bg(fill)
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .debug_selector(move || selector.into())
                .flex()
                .items_center()
                .justify_center()
                .child(icon(
                    state.glyph(),
                    px(if matches!(state, Button::Loading(_)) {
                        13.
                    } else {
                        16.
                    }),
                    ink,
                )),
        )
        .when_some(
            match state {
                Button::Loading(fraction) => Some(fraction),
                _ => None,
            },
            |this, fraction| this.child(ring(fraction, ink.opacity(0.25), ink, context.still)),
        )
        .map(|this| match (&url, waiting) {
            (Some(_), false) => {
                let (view, media) = (context.view.clone(), media.clone());
                this.cursor_pointer()
                    .hover(|style| style.opacity(0.85))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |shell, cx| shell.toggle_audio(media.clone(), cx))
                            .ok();
                    })
            }
            _ => this,
        });

    // The waveform: bars with round ends, the played part in the bubble's
    // accent and the rest muted.
    let decoded = shape.is_some();
    let mut wave = div()
        .debug_selector(move || {
            if decoded {
                "audio-wave-real".into()
            } else {
                "audio-wave-idle".into()
            }
        })
        .size_full()
        .flex()
        .items_center()
        .justify_between();
    for bar in 0..BARS {
        let height = match &shape {
            Some(shape) => BAR_MIN + f32::from(shape.bars[bar]) / 255. * (BAR_MAX - BAR_MIN),
            None => BAR_IDLE,
        };
        let fraction = (bar as f32 + 0.5) / BARS as f32;
        let done = active && fraction <= progress;
        let colour = match (done, decoded) {
            (true, _) => side.signal,
            (false, true) => side.meta.opacity(0.55),
            // Not loaded: fainter than any real bar.
            (false, false) => side.meta.opacity(0.3),
        };
        let column = div()
            .id(SharedString::from(format!("bar-{}-{bar}", message.id)))
            .h_full()
            .flex_1()
            .flex()
            .items_center()
            .justify_center()
            .child(div().w(px(2.5)).h(px(height)).rounded_full().bg(colour));
        wave = wave.child(match (&url, active) {
            // A click moves there; so does dragging across the bars.
            (Some(url), true) => {
                let (view, url) = (context.view.clone(), url.clone());
                let (drag_view, drag_url) = (view.clone(), url.clone());
                column
                    .cursor_pointer()
                    .on_click(move |_, _, cx| {
                        view.update(cx, |shell, cx| shell.seek_audio(&url, fraction, cx))
                            .ok();
                    })
                    .on_mouse_move(move |event, _, cx| {
                        if event.dragging() {
                            drag_view
                                .update(cx, |shell, cx| shell.seek_audio(&drag_url, fraction, cx))
                                .ok();
                        }
                    })
            }
            _ => column,
        });
    }
    // Where the clip is: a dot on the waveform, moved by a click or a
    // drag across the bars under it.
    let wave = div()
        .debug_selector(|| "audio-wave".into())
        .relative()
        .flex_1()
        .min_w_0()
        .h(px(WAVE_HEIGHT))
        .child(wave)
        .when(active, |this| {
            this.child(
                div()
                    .debug_selector(|| "audio-knob".into())
                    .absolute()
                    .top(px((WAVE_HEIGHT - KNOB) / 2.))
                    .left(relative(progress))
                    .ml(-px(KNOB / 2.))
                    .size(px(KNOB))
                    .rounded_full()
                    .bg(side.signal),
            )
        });

    // The speed: only on the clip in the player, in a slot that is always
    // there so the waveform has the same width in every bubble.
    let speed = div()
        .flex_none()
        .w(px(36.))
        .flex()
        .justify_end()
        .when(active, |this| {
            let view = context.view.clone();
            this.child(
                div()
                    .id(SharedString::from(format!("speed-{}", message.id)))
                    .debug_selector(|| "audio-speed".into())
                    .h(px(20.))
                    .px(px(7.))
                    .rounded_full()
                    .bg(side.quote)
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_color(side.text)
                    .hover(|style| style.opacity(0.8))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |shell, cx| shell.cycle_audio_speed(cx))
                            .ok();
                    })
                    .child(mono(audio.speed.label()).text_size(px(10.5))),
            )
        });

    let clock = |at: std::time::Duration| duration(at.as_secs() as u32);
    let (note, alarming): (Option<SharedString>, bool) = if url.is_none() {
        (Some("NOT AVAILABLE".into()), false)
    } else if let Some(reason) = no_output {
        (Some(reason.to_uppercase().into()), true)
    } else if let Some(reason) = failure {
        (
            Some(format!("COULD NOT PLAY · {reason}").to_uppercase().into()),
            true,
        )
    } else if waiting {
        (Some("DOWNLOADING".into()), false)
    } else {
        // The length when it is known; never a made-up 0:00. A file that
        // is not here yet says how large it is, when the message does.
        let size = media.size_bytes.filter(|_| state == Button::Download);
        (
            match (position, total, size) {
                (Some(at), Some(total), _) => {
                    Some(format!("{} / {}", clock(at), clock(total)).into())
                }
                (Some(at), None, _) => Some(clock(at).into()),
                (None, Some(total), Some(size)) => {
                    Some(format!("{} · {}", clock(total), file_size(size)).into())
                }
                (None, Some(total), None) => Some(clock(total).into()),
                (None, None, Some(size)) => Some(file_size(size).into()),
                (None, None, None) => None,
            },
            false,
        )
    };
    let note_ink = if alarming { palette.danger } else { side.meta };

    div()
        .w(metrics::AUDIO_WIDTH())
        .max_w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .debug_selector(|| "audio-row".into())
                .h(px(40.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(button)
                .child(wave)
                .child(speed),
        )
        .child(
            div()
                .h(px(16.))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .debug_selector(|| "audio-time".into())
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(4.))
                        .overflow_hidden()
                        .text_color(note_ink)
                        // A voice note, not a music file: the mic says so.
                        .when(media.kind == MediaKind::Voice, |this| {
                            this.child(
                                div()
                                    .debug_selector(|| "audio-mic".into())
                                    .flex_none()
                                    .child(icon(IconName::Mic, px(11.), note_ink)),
                            )
                        })
                        .children(note.map(|note| {
                            div()
                                .min_w_0()
                                .truncate()
                                .child(mono(note).text_size(px(10.5)))
                        })),
                )
                .child(meta),
        )
}
