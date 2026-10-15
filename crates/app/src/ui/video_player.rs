//! A video of a conversation, played in the window.
//!
//! The decoder is the one stories use (`crate::video`): the file's bytes
//! stay in the encrypted store and are streamed to it, never written out
//! for another application. A video that is not here yet is fetched
//! first, with the same button as everywhere. When it cannot be played
//! here (no decoder in this build, a file it does not read), the player
//! says so and offers the system's player.

use super::shell::{Overlay, Shell};
use super::transfer::{transfer_button, Transfer};
use super::widgets::{icon_button, mono, text_button};
use crate::format::duration;
use crate::icons::IconName;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use crate::video::Clip;
use client_provider::Media;
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, RenderImage, SharedString, Task, Window};
use std::sync::Arc;
use std::time::Duration;

/// How often the player looks for a new frame.
const TICK: Duration = Duration::from_millis(33);
/// How far the arrows move.
const STEP: Duration = Duration::from_secs(5);

/// The video being played, if one is.
#[derive(Default)]
pub(super) struct VideoDeck {
    pub(super) media: Option<Media>,
    clip: Option<Arc<Clip>>,
    /// Why it cannot be played here.
    pub(super) failure: Option<SharedString>,
    paused: bool,
    /// Where the bar was drawn last, for a click on it.
    bar: std::rc::Rc<std::cell::Cell<Option<gpui_kit::Bounds<gpui_kit::Pixels>>>>,
    _tick: Option<Task<()>>,
}

impl VideoDeck {
    /// Whether it is held.
    #[cfg(test)]
    pub(super) fn paused(&self) -> bool {
        self.paused
    }

    /// Whether a decoder plays it.
    #[cfg(test)]
    pub(super) fn playing(&self) -> bool {
        self.clip.is_some()
    }
}

/// `m:ss` of a time.
fn clock_of(at: Duration) -> String {
    duration(at.as_secs() as u32)
}

impl Shell {
    /// Opens the player on a video, fetching it first if need be.
    pub(super) fn watch_video(
        &mut self,
        media: Media,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(account), Some(source)) = (self.account.clone(), media.source.clone()) else {
            return;
        };
        self.end_video(cx);
        if self.media.file_state(source.as_str()) != super::media::FileState::Ready {
            self.engine.want_file(&account, source.as_str());
        }
        self.video.media = Some(media);
        self.open_overlay(Overlay::Video, window, cx);
        self.start_video();
        self.video._tick = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            let alive = this.update(cx, |this, cx| this.video_tick(cx));
            if !matches!(alive, Ok(true)) {
                return;
            }
        }));
        cx.notify();
    }

    /// Starts the decoder once the file is here.
    pub(super) fn start_video(&mut self) {
        if self.video.clip.is_some() || self.video.failure.is_some() {
            return;
        }
        let Some(url) = self
            .video
            .media
            .as_ref()
            .and_then(|media| media.source.as_ref())
            .map(|source| source.to_string())
        else {
            return;
        };
        match self.media.file_state(&url) {
            super::media::FileState::Ready => {}
            super::media::FileState::Unavailable(reason) => {
                self.video.failure = Some(reason);
                return;
            }
            _ => return,
        }
        let Some((bytes, _)) = self.media.file(&url) else {
            return;
        };
        match Clip::open(Arc::<[u8]>::from(bytes)) {
            Ok(clip) => {
                self.video.clip = Some(Arc::new(clip));
                self.video.paused = false;
            }
            Err(error) => {
                tracing::warn!(%error, "a video could not be played in the app");
                self.video.failure = Some("This video can't be played in the app.".into());
            }
        }
    }

    /// One turn of the player's clock. Answers whether it is still open.
    fn video_tick(&mut self, cx: &mut Context<Self>) -> bool {
        if self.overlay != Overlay::Video || self.video.media.is_none() {
            return false;
        }
        self.start_video();
        if let Some(clip) = &self.video.clip {
            clip.poll(TICK);
            if let Some(failure) = clip.report().failure {
                tracing::warn!(%failure, "a video stopped");
                self.video.failure = Some("This video can't be played in the app.".into());
            }
        }
        cx.notify();
        true
    }

    /// The player was closed: the decoder stops and its frames leave the
    /// window.
    pub(super) fn end_video(&mut self, cx: &mut Context<Self>) {
        let deck = std::mem::take(&mut self.video);
        let Some(clip) = deck.clip else {
            return;
        };
        let mut images: Vec<Arc<RenderImage>> = clip.take_stale();
        images.extend(clip.report().image);
        let window = self.status.window;
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| {
                for image in images {
                    let _ = window.drop_image(image);
                }
            });
        });
    }

    /// Plays or holds; at the end, plays again from the start.
    pub(super) fn video_toggle(&mut self, cx: &mut Context<Self>) {
        let Some(clip) = &self.video.clip else {
            return;
        };
        if clip.report().finished {
            self.video_replay(cx);
            return;
        }
        self.video.paused = !self.video.paused;
        clip.set_paused(self.video.paused);
        cx.notify();
    }

    /// From the start again: the decoder that finished is let go.
    fn video_replay(&mut self, cx: &mut Context<Self>) {
        let media = self.video.media.clone();
        let tick = self.video._tick.take();
        self.end_video(cx);
        self.video.media = media;
        self.video._tick = tick;
        self.start_video();
        cx.notify();
    }

    /// Moves `by` forward, or back.
    pub(super) fn video_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(clip) = &self.video.clip else {
            return;
        };
        let report = clip.report();
        let at = if forward {
            report.position + STEP
        } else {
            report.position.saturating_sub(STEP)
        };
        clip.seek(report.duration.map_or(at, |length| at.min(length)));
        cx.notify();
    }

    /// The keys of the player. Answers whether one was used.
    pub(super) fn video_key(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        match key {
            "space" | "k" => self.video_toggle(cx),
            "right" | "l" => self.video_step(true, cx),
            "left" | "j" => self.video_step(false, cx),
            _ => return false,
        }
        true
    }

    pub(super) fn render_video(&self, palette: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        let media = self.video.media.as_ref()?;
        let account = self.account.as_ref()?;
        let url = media.source.as_ref()?.to_string();
        let report = self.video.clip.as_ref().map(|clip| clip.report());

        // The middle: the frame, the download, or why it does not play.
        let middle: gpui_kit::AnyElement = match (&self.video.failure, &self.video.clip, &report) {
            (Some(failure), _, _) => {
                let opened = media.clone();
                div()
                    .debug_selector(|| "video-failed".into())
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_3()
                    .child(div().text_size(metrics::TEXT_BODY()).child(failure.clone()))
                    .child(
                        text_button(
                            "video-system",
                            "Open with another application",
                            Some(IconName::ExternalLink),
                            true,
                            palette,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_overlay(window, cx);
                                this.open_file(opened.clone(), cx);
                            },
                        )),
                    )
                    .into_any_element()
            }
            (None, Some(clip), Some(report)) => {
                let clip = clip.clone();
                let (width, height) = (report.width.max(1) as f32, report.height.max(1) as f32);
                let image = report.image.clone();
                div()
                    .debug_selector(|| "video-frame".into())
                    .size_full()
                    .child(
                        gpui_kit::canvas(
                            |_, _, _| {},
                            move |bounds, _, window, _| {
                                for stale in clip.take_stale() {
                                    let _ = window.drop_image(stale);
                                }
                                let Some(image) = image.clone() else {
                                    return;
                                };
                                let scale = (bounds.size.width.as_f32() / width)
                                    .min(bounds.size.height.as_f32() / height);
                                let size = gpui_kit::size(
                                    gpui_kit::px(width * scale),
                                    gpui_kit::px(height * scale),
                                );
                                let fitted = gpui_kit::Bounds {
                                    origin: gpui_kit::point(
                                        bounds.origin.x + (bounds.size.width - size.width) / 2.,
                                        bounds.origin.y + (bounds.size.height - size.height) / 2.,
                                    ),
                                    size,
                                };
                                let _ = window.paint_image(
                                    fitted,
                                    fitted,
                                    gpui_kit::Corners::all(px(0.)),
                                    image,
                                    0,
                                    false,
                                );
                            },
                        )
                        .size_full(),
                    )
                    .into_any_element()
            }
            _ => {
                let state = match self.media.file_state(&url) {
                    super::media::FileState::Unavailable(reason) => Transfer::Retry(reason),
                    _ => Transfer::Loading {
                        fraction: self.media.file_fraction(&url, media),
                    },
                };
                let (shelf, account, url) = (self.media.clone(), account.clone(), url.clone());
                let stop = matches!(state, Transfer::Loading { .. });
                transfer_button(
                    "video-transfer",
                    &state,
                    palette,
                    crate::settings::reduce_motion(cx),
                    move |_, _, _| {
                        if stop {
                            shelf.cancel_file(&url);
                        } else {
                            shelf.retry_file(&account, &url);
                        }
                    },
                )
                .into_any_element()
            }
        };

        // Under it: play and hold, where it is, and how long it is.
        let (position, length, finished) = report
            .as_ref()
            .map(|report| (report.position, report.duration, report.finished))
            .unwrap_or_default();
        let playing = self.video.clip.is_some() && !self.video.paused && !finished;
        let fraction = length
            .filter(|length| !length.is_zero())
            .map(|length| (position.as_secs_f32() / length.as_secs_f32()).clamp(0., 1.));
        let seek_to = length;
        let controls = div()
            .id("video-controls")
            .debug_selector(|| "video-controls".into())
            .w_full()
            .max_w(px(720.))
            .px_3()
            .py_2()
            .rounded(metrics::PANEL_RADIUS())
            .border_1()
            .border_color(palette.elevated_border)
            .bg(palette.elevated)
            .flex()
            .items_center()
            .gap_3()
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                icon_button(
                    "video-play",
                    if playing {
                        IconName::Pause
                    } else {
                        IconName::Play
                    },
                    palette,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.video_toggle(cx);
                })),
            )
            .child(
                mono(clock_of(position))
                    .debug_selector(|| "video-position".into())
                    .text_color(palette.text_muted),
            )
            .child(
                // The bar: a click goes there.
                div()
                    .id("video-bar")
                    .debug_selector(|| "video-bar".into())
                    .flex_1()
                    .h(px(18.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(
                        gpui_kit::MouseButton::Left,
                        cx.listener(move |this, event: &gpui_kit::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            let (Some(length), Some(clip), Some(bounds)) =
                                (seek_to, &this.video.clip, this.video.bar.get())
                            else {
                                return;
                            };
                            let along = ((event.position.x - bounds.origin.x).as_f32()
                                / bounds.size.width.as_f32())
                            .clamp(0., 1.);
                            clip.seek(length.mul_f32(along));
                            cx.notify();
                        }),
                    )
                    .relative()
                    .child({
                        let bar = self.video.bar.clone();
                        gpui_kit::canvas(move |bounds, _, _| bar.set(Some(bounds)), |_, _, _, _| {})
                            .absolute()
                            .size_full()
                    })
                    .child(
                        div()
                            .w_full()
                            .h(px(4.))
                            .rounded_full()
                            .bg(palette.muted)
                            .child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(palette.accent)
                                    .w(gpui_kit::relative(fraction.unwrap_or(0.))),
                            ),
                    ),
            )
            .child(
                mono(length.map(clock_of).unwrap_or_else(|| "–:––".to_owned()))
                    .text_color(palette.text_muted),
            )
            .child(
                icon_button("video-save", IconName::ArrowDownToLine, palette).on_click({
                    let media = media.clone();
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.save_as(Some(media.clone()), cx);
                    })
                }),
            )
            .child(
                icon_button("video-open", IconName::ExternalLink, palette).on_click({
                    let media = media.clone();
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_overlay(window, cx);
                        this.open_file(media.clone(), cx);
                    })
                }),
            )
            .child(
                icon_button("video-close", IconName::X, palette).on_click(cx.listener(
                    |this, _, window, cx| {
                        cx.stop_propagation();
                        this.close_overlay(window, cx);
                    },
                )),
            );

        Some(
            div()
                .debug_selector(|| "video-player".into())
                .relative()
                .size_full()
                .p_4()
                .flex()
                .flex_col()
                .items_center()
                .gap_3()
                .text_color(palette.text)
                .child(
                    div()
                        .id("video-area")
                        .flex_1()
                        .min_h_0()
                        .w_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        // A click on the picture plays or holds it.
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.video_toggle(cx);
                        }))
                        .child(middle),
                )
                .child(controls)
                .children(self.render_overlay_word(palette)),
        )
    }
}
