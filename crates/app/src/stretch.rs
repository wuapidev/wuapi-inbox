//! Playing a clip faster without raising its pitch, and bringing it to
//! the rate and the channels of the audio device.
//!
//! Faster is not "the samples, sooner": that is a tape sped up, the
//! voice a fifth higher at 1.5× and an octave at 2×. Here the clip is cut
//! into short overlapping pieces (30 ms, half overlapped), and the pieces
//! are laid down at their recorded pace while the place they are taken
//! from moves through the clip faster: WSOLA, waveform-similarity
//! overlap-add. Each piece is taken from where it continues the one
//! before it best (the cross-correlation over ±12 ms around where it
//! should come from), which is what keeps a voice's periods whole. Two
//! overlapped pieces always weigh one together, so nothing is louder
//! than it was recorded.
//!
//! [`Voice`] is what the audio device's thread owns: everything it needs
//! is allocated when it is made, and from then on it allocates nothing
//! and waits for nobody. The interface reads where it is, and tells it
//! the speed, through atomics ([`Shared`]).

use crate::audio::Clip;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Half a piece: what is laid down at a time.
const HOP_SECONDS: f64 = 0.015;
/// How far from its place a piece may be taken.
const SEARCH_SECONDS: f64 = 0.012;

/// What the interface and the audio thread share.
pub struct Shared {
    /// Where in the clip the sound is, in frames of the clip (the bits of
    /// an `f64`).
    position: AtomicU64,
    /// The clip has played to its end.
    done: AtomicBool,
    /// The speed (the bits of an `f32`).
    speed: AtomicU32,
    rate: u32,
}

impl Shared {
    /// Where in the clip the sound is, or `None` once it has ended.
    pub fn position(&self) -> Option<Duration> {
        if self.done.load(Ordering::Relaxed) {
            return None;
        }
        let frames = f64::from_bits(self.position.load(Ordering::Relaxed));
        Some(Duration::from_secs_f64(
            frames.max(0.) / f64::from(self.rate.max(1)),
        ))
    }

    /// Plays on at another speed, from the next piece.
    pub fn set_speed(&self, speed: f32) {
        self.speed.store(speed.to_bits(), Ordering::Relaxed);
    }

    fn speed(&self) -> f64 {
        f64::from(f32::from_bits(self.speed.load(Ordering::Relaxed))).clamp(0.25, 4.)
    }
}

/// A clip being played: what the audio device's thread reads from.
pub struct Voice {
    clip: Arc<Clip>,
    shared: Arc<Shared>,
    /// Frames laid down at a time.
    hop: usize,
    /// Frames either side of its place a piece is looked for.
    search: usize,
    /// The rising half of the window, `hop` long; the falling half is
    /// what is left to one.
    rise: Vec<f32>,
    /// Where the next piece should come from, in frames of the clip.
    nominal: f64,
    /// Where the piece that would simply continue the last one starts.
    natural: Option<i64>,
    /// The second half of the last piece, already faded: `hop` frames of
    /// the clip's channels.
    tail: Vec<f32>,
    /// The frames laid down last, at the clip's rate and channels.
    block: Vec<f32>,
    /// The last frame of the block before, for the device's first frame
    /// of this one.
    last: [f32; 2],
    /// Where in `block` the device's next frame is; a whole block away
    /// when there is none.
    cursor: f64,
    /// Where the clip was when `block` was laid down, and how fast.
    block_from: f64,
    block_speed: f64,
    /// Frames of the clip per frame of the device.
    ratio: f64,
    ended: bool,
}

impl Voice {
    /// A clip to be played from `from` at `speed` on a device of
    /// `device_rate` frames a second. Allocates; from here on nothing does.
    pub fn new(
        clip: Arc<Clip>,
        from: Duration,
        speed: f32,
        device_rate: u32,
    ) -> (Self, Arc<Shared>) {
        let rate = clip.rate.max(1);
        let hop = ((f64::from(rate) * HOP_SECONDS).round() as usize).max(8);
        let search = ((f64::from(rate) * SEARCH_SECONDS).round() as usize).max(4);
        let channels = clip.channels.clamp(1, 2);
        let nominal = from.as_secs_f64() * f64::from(rate);
        let shared = Arc::new(Shared {
            position: AtomicU64::new(nominal.to_bits()),
            done: AtomicBool::new(false),
            speed: AtomicU32::new(speed.to_bits()),
            rate,
        });
        let rise = (0..hop)
            .map(|i| {
                let phase = std::f32::consts::PI * (i as f32 + 0.5) / hop as f32;
                0.5 * (1. - phase.cos())
            })
            .collect();
        let voice = Self {
            hop,
            search,
            rise,
            nominal,
            natural: None,
            tail: vec![0.; hop * channels],
            block: vec![0.; hop * channels],
            last: [0.; 2],
            cursor: hop as f64,
            block_from: nominal,
            block_speed: f64::from(speed),
            ratio: f64::from(rate) / f64::from(device_rate.max(1)),
            ended: false,
            shared: shared.clone(),
            clip,
        };
        (voice, shared)
    }

    fn channels(&self) -> usize {
        self.clip.channels.clamp(1, 2)
    }

    /// One sample of the clip, from -1 to 1; silence outside it.
    #[inline]
    fn sample(&self, frame: i64, channel: usize) -> f32 {
        if frame < 0 {
            return 0.;
        }
        let index = frame as usize * self.clip.channels + channel;
        match self.clip.samples.get(index) {
            Some(sample) => f32::from(*sample) / 32_768.,
            None => 0.,
        }
    }

    /// How well the piece at `candidate` continues the last one: its
    /// likeness to what would have come next, whatever its loudness.
    fn likeness(&self, candidate: i64, natural: i64, stride: usize) -> f32 {
        let (mut dot, mut energy) = (0f32, 1e-9f32);
        let mut i = 0;
        while i < self.hop {
            let a = self.sample(natural + i as i64, 0);
            let b = self.sample(candidate + i as i64, 0);
            dot += a * b;
            energy += b * b;
            i += stride;
        }
        dot / energy.sqrt()
    }

    /// Where the next piece is taken from: its place, or near it where it
    /// continues the last piece best.
    fn place(&self, target: i64) -> i64 {
        let Some(natural) = self.natural else {
            return target;
        };
        let rate = self.clip.rate as usize;
        // A voice's pitch is far below 4 kHz: every few samples is enough
        // to tell two pieces alike, and a coarse pass finds the region.
        let (stride, coarse) = ((rate / 8_000).max(1), (rate / 16_000).max(1) as i64);
        let search = self.search as i64;
        let (mut best, mut best_score) = (target, f32::MIN);
        let mut offset = -search;
        while offset <= search {
            let score = self.likeness(target + offset, natural, stride);
            if score > best_score {
                (best, best_score) = (target + offset, score);
            }
            offset += coarse;
        }
        let centre = best;
        for candidate in centre - coarse + 1..centre + coarse {
            let score = self.likeness(candidate, natural, stride);
            if score > best_score {
                (best, best_score) = (candidate, score);
            }
        }
        best
    }

    /// Lays down the next `hop` frames into `block`. `false` at the end.
    fn lay(&mut self) -> bool {
        let frames = self.clip.frames() as f64;
        if self.ended || self.nominal >= frames {
            self.ended = true;
            return false;
        }
        let speed = self.shared.speed();
        let target = self.nominal.round() as i64;
        let start = match self.natural {
            // At the recorded pace what comes next is what came next in
            // the clip, wherever the last piece was taken from: nothing to
            // look for, and the clip comes out as it went in.
            Some(natural)
                if (speed - 1.).abs() < 1e-3
                    && (natural - target).unsigned_abs() as usize <= self.search =>
            {
                natural
            }
            _ => self.place(target),
        };
        let (hop, channels) = (self.hop, self.channels());
        for i in 0..hop {
            let rise = self.rise[i];
            for channel in 0..channels {
                let at = i * channels + channel;
                // The end of the last piece fading out, this one fading in.
                self.block[at] = self.tail[at] + rise * self.sample(start + i as i64, channel);
                self.tail[at] = (1. - rise) * self.sample(start + (hop + i) as i64, channel);
            }
        }
        self.natural = Some(start + hop as i64);
        (self.block_from, self.block_speed) = (self.nominal, speed);
        self.nominal += hop as f64 * speed;
        true
    }

    /// Fills a buffer of the device, `out_channels` samples per frame.
    /// Silence once the clip has ended.
    pub fn render<T: cpal::SizedSample + cpal::FromSample<f32>>(
        &mut self,
        out: &mut [T],
        out_channels: usize,
    ) {
        let channels = self.channels();
        let hop = self.hop as f64;
        for frame in out.chunks_mut(out_channels.max(1)) {
            while !self.ended && self.cursor >= hop {
                // The frame before the block's first, to blend from.
                for channel in 0..channels {
                    self.last[channel] = self.block[(self.hop - 1) * channels + channel];
                }
                if self.lay() {
                    self.cursor -= hop;
                }
            }
            if self.ended {
                frame.fill(T::from_sample(0.));
                continue;
            }
            let index = self.cursor as usize;
            let blend = (self.cursor - index as f64) as f32;
            let mut pair = [0f32; 2];
            for (channel, value) in pair.iter_mut().enumerate().take(channels) {
                let before = match index {
                    0 => self.last[channel],
                    _ => self.block[(index - 1) * channels + channel],
                };
                let here = self.block[index * channels + channel];
                *value = before + (here - before) * blend;
            }
            if channels == 1 {
                pair[1] = pair[0];
            }
            match frame.len() {
                // One speaker: both sides of a stereo clip.
                1 => frame[0] = T::from_sample((pair[0] + pair[1]) / 2.),
                _ => {
                    for (channel, sample) in frame.iter_mut().enumerate() {
                        *sample = T::from_sample(pair[channel.min(1)]);
                    }
                }
            }
            self.cursor += self.ratio;
        }
        // Where the sound is, in the clip's own time: the place of the
        // piece being heard, and how far into it.
        let into = self.cursor.min(hop) * self.block_speed;
        self.shared
            .position
            .store((self.block_from + into).to_bits(), Ordering::Relaxed);
        if self.ended {
            self.shared.done.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sine of `hz`, `seconds` long, nine tenths of full scale.
    fn tone(hz: f32, seconds: f32, rate: u32, channels: usize) -> Arc<Clip> {
        let frames = (seconds * rate as f32) as usize;
        let mut samples = Vec::with_capacity(frames * channels);
        for frame in 0..frames {
            let value = (frame as f32 * hz * std::f32::consts::TAU / rate as f32).sin() * 0.9;
            for _ in 0..channels {
                samples.push((value * 32_767.) as i16);
            }
        }
        Arc::new(Clip {
            samples,
            channels,
            rate,
        })
    }

    /// Plays a voice to its end into a device that makes no sound, a
    /// buffer at a time as a device asks. What the device was given, one
    /// channel of it, up to the end of the clip.
    fn play(
        voice: &mut Voice,
        shared: &Shared,
        device_channels: usize,
        mut between: impl FnMut(usize, &Shared),
    ) -> Vec<f32> {
        let mut heard = Vec::new();
        let mut buffer = vec![0f32; 441 * device_channels];
        for _ in 0..100_000 {
            if shared.position().is_none() {
                return heard;
            }
            voice.render(&mut buffer, device_channels);
            let ended = voice.ended;
            for frame in buffer.chunks(device_channels) {
                heard.push(frame[0]);
            }
            if ended {
                // What follows the end in the last buffer is silence.
                let silent = heard
                    .iter()
                    .rev()
                    .take_while(|sample| **sample == 0.)
                    .count();
                heard.truncate(heard.len() - silent.min(441));
            }
            between(heard.len(), shared);
        }
        panic!("the clip never ended");
    }

    /// The loudest frequency between 80 and 1000 Hz in `samples`.
    fn pitch(samples: &[f32], rate: u32) -> f32 {
        let mut best = (0f32, 0f32);
        let mut hz = 80f32;
        while hz <= 1_000. {
            let step = std::f32::consts::TAU * hz / rate as f32;
            let (mut re, mut im) = (0f32, 0f32);
            for (n, sample) in samples.iter().enumerate() {
                re += sample * (n as f32 * step).cos();
                im += sample * (n as f32 * step).sin();
            }
            let power = re * re + im * im;
            if power > best.1 {
                best = (hz, power);
            }
            hz += 1.;
        }
        best.0
    }

    #[test]
    fn a_clip_lasts_its_length_divided_by_the_speed_on_every_device() {
        let seconds = 2.;
        for rate in [8_000, 16_000, 24_000, 44_100, 48_000] {
            for channels in [1, 2] {
                let clip = tone(220., seconds, rate, channels);
                for device_rate in [44_100, 48_000] {
                    for device_channels in [1, 2] {
                        for speed in [1., 1.5, 2.] {
                            let (mut voice, shared) =
                                Voice::new(clip.clone(), Duration::ZERO, speed, device_rate);
                            let heard = play(&mut voice, &shared, device_channels, |_, _| {});
                            let lasted = heard.len() as f32 / device_rate as f32;
                            let expected = seconds / speed;
                            assert!(
                                (lasted - expected).abs() <= 0.04,
                                "{rate} Hz x{channels} on {device_rate} Hz x{device_channels} \
                                 at {speed}x lasted {lasted} s, not {expected} s"
                            );
                            // Never louder than it was recorded.
                            let loudest = heard.iter().fold(0f32, |a, b| a.max(b.abs()));
                            assert!(loudest <= 0.92, "{loudest}");
                            assert!(loudest > 0.5, "it is sound: {loudest}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn faster_keeps_the_pitch() {
        for rate in [16_000, 48_000] {
            for hz in [110., 220., 330.] {
                let clip = tone(hz, 2., rate, 1);
                for speed in [1., 1.5, 2.] {
                    let (mut voice, shared) =
                        Voice::new(clip.clone(), Duration::ZERO, speed, 48_000);
                    let heard = play(&mut voice, &shared, 1, |_, _| {});
                    // The middle of it, away from the fades at the ends.
                    let middle = &heard[heard.len() / 4..heard.len() * 3 / 4];
                    let found = pitch(middle, 48_000);
                    assert!(
                        (found - hz).abs() <= hz * 0.03,
                        "{hz} Hz at {rate} Hz, {speed}x: heard {found} Hz"
                    );
                }
            }
        }
    }

    #[test]
    fn the_pieces_join_without_a_click_even_when_the_speed_changes() {
        let clip = tone(220., 3., 48_000, 1);
        let (mut voice, shared) = Voice::new(clip, Duration::ZERO, 1., 48_000);
        // 1× → 1.5× → 2× → 1× while it plays.
        let heard = play(&mut voice, &shared, 1, |frames, shared| {
            shared.set_speed(match frames {
                0..=24_000 => 1.,
                24_001..=48_000 => 1.5,
                48_001..=72_000 => 2.,
                _ => 1.,
            });
        });
        // A sine of 220 Hz at 0.9 moves 0.026 a sample at most. A piece
        // that did not continue the one before would jump many times that.
        let steepest = heard
            .windows(2)
            .map(|pair| (pair[1] - pair[0]).abs())
            .fold(0f32, f32::max);
        assert!(steepest < 0.06, "a jump of {steepest}");
        // 0.5 s at 1×, 0.5 s at 1.5×, 0.5 s at 2× are 2.25 s of the clip;
        // the rest, 0.75 s, at 1×.
        let lasted = heard.len() as f32 / 48_000.;
        assert!((lasted - 2.25).abs() < 0.06, "{lasted} s");
    }

    #[test]
    fn the_place_is_told_in_the_clips_own_time() {
        let clip = tone(220., 4., 16_000, 1);
        for (speed, device_rate) in [(1., 48_000), (1.5, 44_100), (2., 48_000)] {
            // From one second in.
            let (mut voice, shared) =
                Voice::new(clip.clone(), Duration::from_secs(1), speed, device_rate);
            assert_eq!(shared.position(), Some(Duration::from_secs(1)));
            // One second of listening.
            let mut buffer = vec![0f32; device_rate as usize];
            voice.render(&mut buffer, 1);
            let at = shared.position().unwrap().as_secs_f32();
            assert!(
                (at - (1. + speed)).abs() < 0.04,
                "after a second at {speed}x the clip is at {at} s"
            );
        }
        // To the end: no place any more.
        let (mut voice, shared) = Voice::new(clip, Duration::from_millis(3_900), 1., 48_000);
        let mut buffer = vec![0f32; 48_000];
        voice.render(&mut buffer, 2);
        assert_eq!(shared.position(), None);
        assert!(buffer[40_000..].iter().all(|sample| *sample == 0.));
    }

    #[test]
    fn at_the_recorded_pace_the_clip_comes_out_as_it_went_in() {
        // Same rate in and out, 1×: after the first piece fades in, every
        // sample is the clip's own.
        let clip = tone(220., 1., 48_000, 2);
        let (mut voice, _) = Voice::new(clip.clone(), Duration::ZERO, 1., 48_000);
        let mut out = vec![0f32; 48_000 * 2];
        voice.render(&mut out, 2);
        // The device hears each frame one frame late (it blends from the
        // frame before).
        for frame in 2_000..40_000 {
            let was = f32::from(clip.samples[(frame - 1) * 2]) / 32_768.;
            assert!((out[frame * 2] - was).abs() < 1e-4, "frame {frame}");
            assert_eq!(out[frame * 2], out[frame * 2 + 1]);
        }
    }
}
