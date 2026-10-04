//! Stickers and GIFs that move: which frame is on screen at a moment, and
//! when the next one is due.
//!
//! Nothing here draws or waits. An animation is a list of frames with the
//! time each one ends at; [`Animation::frame_at`] is a pure function of
//! the time it has been playing, and [`Playback`] keeps that time across
//! pauses (the window in the background, an overlay over the chat). The
//! view repaints when the frame on screen is due to change and not
//! before, and not at all while nothing animated is drawn.

use client_core::AnimationFrames;
use gpui_kit::RenderImage;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// A decoded animation, ready to be painted frame by frame.
pub struct Animation {
    /// Every frame, as the renderer wants its pixels.
    pub image: Arc<RenderImage>,
    /// When each frame ends, from the start of the loop.
    ends: Vec<Duration>,
    /// Its size in pixels.
    pub size: (u32, u32),
    /// The memory its frames take.
    pub bytes: usize,
}

impl Animation {
    /// Wraps decoded frames. Reorders every pixel (the renderer takes
    /// BGRA): call it where the frames were decoded, off the UI thread.
    pub fn new(frames: AnimationFrames) -> Self {
        let bytes = frames.bytes();
        let size = frames
            .frames
            .first()
            .map(|frame| frame.image.dimensions())
            .unwrap_or((1, 1));
        let mut ends = Vec::with_capacity(frames.frames.len());
        let mut end = Duration::ZERO;
        let frames: Vec<image::Frame> = frames
            .frames
            .into_iter()
            .map(|frame| {
                end += frame.delay;
                ends.push(end);
                let mut pixels = frame.image;
                let raw: &mut [u8] = &mut pixels;
                for red in (0..raw.len().saturating_sub(3)).step_by(4) {
                    raw.swap(red, red + 2);
                }
                image::Frame::new(pixels)
            })
            .collect();
        Self {
            image: Arc::new(RenderImage::new(frames)),
            ends,
            size,
            bytes,
        }
    }

    /// How many frames it has.
    #[cfg(test)]
    pub fn frames(&self) -> usize {
        self.ends.len()
    }

    /// One turn of the loop.
    #[cfg(test)]
    pub fn duration(&self) -> Duration {
        self.ends.last().copied().unwrap_or_default()
    }

    /// The frame on screen after playing for `elapsed`, looping for ever
    /// as WhatsApp does, and how long until the next one.
    pub fn frame_at(&self, elapsed: Duration) -> (usize, Duration) {
        frame_at(&self.ends, elapsed)
    }
}

/// See [`Animation::frame_at`]. `ends` is when each frame ends.
pub fn frame_at(ends: &[Duration], elapsed: Duration) -> (usize, Duration) {
    let total = ends.last().copied().unwrap_or_default();
    if total.is_zero() {
        return (0, Duration::MAX);
    }
    let within = Duration::from_nanos((elapsed.as_nanos() % total.as_nanos()) as u64);
    let index = ends.partition_point(|end| *end <= within);
    (index, ends[index] - within)
}

/// What the motion around an animation allows at this moment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Motion {
    /// It runs.
    Running,
    /// It holds the frame it is on: the window is in the background, or
    /// something lies over the conversation.
    Paused,
    /// Held at this moment of its timeline (`--motion-at`).
    Frozen(Duration),
}

/// How long one animation has been playing, pauses left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Playback {
    /// When it (re)started, shifted by what it had already played.
    started: Option<Instant>,
    /// What it had played when it was paused.
    held: Option<Duration>,
}

impl Playback {
    /// How long it has played at `now`. The first call starts it; a pause
    /// holds the time, and running again goes on from there.
    pub fn elapsed(&mut self, now: Instant, motion: Motion) -> Duration {
        match motion {
            Motion::Frozen(at) => at,
            Motion::Paused => {
                let played = self.played(now);
                self.held = Some(played);
                self.started = None;
                played
            }
            Motion::Running => {
                if let Some(held) = self.held.take() {
                    self.started = now.checked_sub(held).or(Some(now));
                }
                self.started.get_or_insert(now);
                self.played(now)
            }
        }
    }

    fn played(&self, now: Instant) -> Duration {
        match (self.held, self.started) {
            (Some(held), _) => held,
            (None, Some(started)) => now.saturating_duration_since(started),
            (None, None) => Duration::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_core::fixtures::animated_webp;
    use client_core::{animation_frames, AnimationLimits};

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn the_frame_on_screen_follows_the_time_and_loops() {
        // Three frames: 80, 120 and 40 ms.
        let ends = [ms(80), ms(200), ms(240)];
        assert_eq!(frame_at(&ends, ms(0)), (0, ms(80)));
        assert_eq!(frame_at(&ends, ms(79)), (0, ms(1)));
        assert_eq!(frame_at(&ends, ms(80)), (1, ms(120)));
        assert_eq!(frame_at(&ends, ms(199)), (1, ms(1)));
        assert_eq!(frame_at(&ends, ms(200)), (2, ms(40)));
        // Around again, any number of times.
        assert_eq!(frame_at(&ends, ms(240)), (0, ms(80)));
        assert_eq!(frame_at(&ends, ms(240 * 1000 + 90)), (1, ms(110)));
        // Nothing to play: the first frame, and never a wake-up.
        assert_eq!(frame_at(&[], ms(50)), (0, Duration::MAX));
    }

    #[test]
    fn a_pause_holds_the_frame_and_playing_goes_on_from_it() {
        let start = Instant::now();
        let at = |value: u64| start + ms(value);
        let mut playback = Playback::default();
        assert_eq!(playback.elapsed(start, Motion::Running), ms(0));
        assert_eq!(playback.elapsed(at(100), Motion::Running), ms(100));
        // The window goes to the background for a minute.
        assert_eq!(playback.elapsed(at(150), Motion::Paused), ms(150));
        assert_eq!(playback.elapsed(at(60_000), Motion::Paused), ms(150));
        // Back: it goes on from where it stood, not a minute later.
        assert_eq!(playback.elapsed(at(60_150), Motion::Running), ms(150));
        assert_eq!(playback.elapsed(at(60_200), Motion::Running), ms(200));
        // Held for a photograph: always the same moment.
        assert_eq!(playback.elapsed(at(70_000), Motion::Frozen(ms(90))), ms(90));
        // Paused before it ever ran: the first frame.
        let mut fresh = Playback::default();
        assert_eq!(fresh.elapsed(start, Motion::Paused), ms(0));
        assert_eq!(fresh.elapsed(at(500), Motion::Running), ms(0));
    }

    #[test]
    fn decoded_frames_become_one_image_with_their_times() {
        let frames = animation_frames(
            &animated_webp(32, 24, &[80, 120, 40]),
            &AnimationLimits::default(),
        )
        .unwrap()
        .unwrap();
        let red = frames.frames[1].image.get_pixel(5, 5).0;
        let animation = Animation::new(frames);
        assert_eq!(animation.frames(), 3);
        assert_eq!(animation.image.frame_count(), 3);
        assert_eq!(animation.size, (32, 24));
        assert_eq!(animation.bytes, 3 * 32 * 24 * 4);
        assert_eq!(animation.duration(), ms(240));
        assert_eq!(animation.frame_at(ms(100)), (1, ms(100)));
        // The renderer's order: blue first.
        let at = (5 * 32 + 5) * 4;
        let pixel = &animation.image.as_bytes(1).unwrap()[at..at + 4];
        assert_eq!(pixel, [red[2], red[1], red[0], red[3]]);
    }
}
