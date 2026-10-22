//! A call's sound: the machine's microphone and speakers on one side, the
//! protocol library's channels on the other.
//!
//! The library wants and gives one channel at 16 kHz, in frames of 960
//! samples (60 ms). A device has its own rate and channels, so both
//! directions are resampled here. The device's streams live on a thread of
//! their own (they cannot move between threads on every system) for as
//! long as the call does.

use client_provider::CallDevices;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use whatsapp_rust::async_channel::{self, Receiver, Sender};

/// The rate both ends of a call speak at.
pub const CALL_RATE: u32 = 16_000;
/// The samples of one frame of a call: 60 ms.
pub const CALL_FRAME: usize = 960;
/// How many frames may wait on either side before the oldest is dropped:
/// sound that is late is worse than sound that is missing.
const WAITING_FRAMES: usize = 8;

/// The devices the user chose for calls. They are the machine's, not a
/// number's, so every session reads the same ones.
static CHOSEN: RwLock<CallDevices> = RwLock::new(CallDevices {
    microphone: None,
    speaker: None,
    camera: None,
});

/// Calls use these devices from the next one on.
pub fn choose(devices: CallDevices) {
    *CHOSEN
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = devices;
}

/// The devices calls use.
pub fn chosen() -> CallDevices {
    CHOSEN
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// The device called `name` among `devices`. A name that is gone (the
/// headset was unplugged) is said in the log and answers `None`, for the
/// system's default to be used.
fn named<D: cpal::traits::DeviceTrait>(
    name: Option<&str>,
    devices: impl Iterator<Item = D>,
    what: &str,
) -> Option<D> {
    let name = name?;
    let found = devices.into_iter().find(|device| {
        device
            .description()
            .is_ok_and(|description| description.name() == name)
    });
    if found.is_none() {
        tracing::warn!(
            device = name,
            "the chosen {what} is not there; using the system's"
        );
    }
    found
}

/// Changes the rate of one channel of sound, a piece at a time, by
/// drawing a line between neighbouring samples.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Where the next output sample falls in `held`.
    position: f64,
    held: Vec<f32>,
}

impl Resampler {
    /// From one rate to another.
    pub fn new(from: u32, to: u32) -> Self {
        Self {
            step: f64::from(from.max(1)) / f64::from(to.max(1)),
            position: 0.0,
            held: Vec::new(),
        }
    }

    /// Takes a piece of sound and adds what it makes at the other rate to
    /// `out`.
    pub fn push(&mut self, mono: &[f32], out: &mut Vec<f32>) {
        self.held.extend_from_slice(mono);
        while self.position + 1.0 < self.held.len() as f64 {
            let index = self.position as usize;
            let between = (self.position - index as f64) as f32;
            out.push(self.held[index] + (self.held[index + 1] - self.held[index]) * between);
            self.position += self.step;
        }
        // What is behind the next sample is done with.
        let done = (self.position as usize).min(self.held.len().saturating_sub(1));
        self.held.drain(..done);
        self.position -= done as f64;
    }
}

/// What the microphone hears, cut into the frames a call sends.
pub struct Capture {
    channels: usize,
    resampler: Resampler,
    waiting: Vec<f32>,
}

impl Capture {
    /// For a device of this rate and number of channels.
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            channels: channels.max(1),
            resampler: Resampler::new(rate, CALL_RATE),
            waiting: Vec::new(),
        }
    }

    /// Takes what the device heard (its channels interleaved) and answers
    /// the frames that are complete.
    pub fn push(&mut self, interleaved: &[f32]) -> Vec<Vec<i16>> {
        let mono: Vec<f32> = interleaved
            .chunks(self.channels)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect();
        self.resampler.push(&mono, &mut self.waiting);
        let mut frames = Vec::new();
        while self.waiting.len() >= CALL_FRAME {
            frames.push(
                self.waiting
                    .drain(..CALL_FRAME)
                    .map(|sample| (sample.clamp(-1.0, 1.0) * 32_767.0) as i16)
                    .collect(),
            );
        }
        frames
    }
}

/// What a call plays, at the device's rate.
pub struct Playout {
    channels: usize,
    resampler: Resampler,
    waiting: VecDeque<f32>,
    /// At most this much waits to be played.
    limit: usize,
}

impl Playout {
    /// For a device of this rate and number of channels.
    pub fn new(rate: u32, channels: usize) -> Self {
        Self {
            channels: channels.max(1),
            resampler: Resampler::new(CALL_RATE, rate),
            waiting: VecDeque::new(),
            limit: rate as usize / 2,
        }
    }

    /// Takes a frame of the call.
    pub fn push(&mut self, frame: &[i16]) {
        let mono: Vec<f32> = frame
            .iter()
            .map(|sample| f32::from(*sample) / 32_768.0)
            .collect();
        let mut out = Vec::new();
        self.resampler.push(&mono, &mut out);
        self.waiting.extend(out);
        // The device fell behind: skip to what is being said now.
        let over = self.waiting.len().saturating_sub(self.limit);
        self.waiting.drain(..over);
    }

    /// True when the device will soon have nothing to play.
    pub fn hungry(&self, wanted: usize) -> bool {
        self.waiting.len() < wanted
    }

    /// Fills what the device asks for (its channels interleaved), with
    /// silence where the call gave nothing.
    pub fn fill(&mut self, out: &mut [f32]) {
        for frame in out.chunks_mut(self.channels) {
            let sample = self.waiting.pop_front().unwrap_or(0.0);
            frame.fill(sample);
        }
    }
}

/// The sound of one call. Dropping it lets the microphone and the
/// speakers go.
pub struct AudioLink {
    /// What the microphone hears, for the library to send.
    pub microphone: Receiver<Vec<i16>>,
    /// Where the library puts what the other side says.
    pub speaker: Sender<Vec<i16>>,
    muted: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    /// Set when the devices chosen changed, for the thread to look.
    changed: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioLink {
    /// Silences the microphone, or lets it be heard again.
    pub fn set_muted(&self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
    }

    /// Goes over to the microphone and speakers chosen now, if they are
    /// not the ones it uses. The call's channels stay as they are: the
    /// library notices nothing.
    pub fn follow_chosen(&self) {
        self.changed.store(true, Ordering::Relaxed);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
}

impl Drop for AudioLink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
        }
    }
}

/// Opens the microphone and the speakers for a call: the ones the user
/// chose, or the system's.
pub fn open() -> Result<AudioLink, String> {
    let devices = chosen();
    let (to_call, microphone) = async_channel::bounded::<Vec<i16>>(WAITING_FRAMES);
    let (speaker, from_call) = async_channel::bounded::<Vec<i16>>(WAITING_FRAMES);
    let muted = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let changed = Arc::new(AtomicBool::new(false));
    let (ready, opened) = std::sync::mpsc::channel::<Result<(), String>>();
    let (silenced, stopping, looked) = (muted.clone(), stop.clone(), changed.clone());
    let thread = std::thread::Builder::new()
        .name("call-audio".into())
        .spawn(move || {
            // The streams stay on this thread until the call is over.
            let open = |devices: &CallDevices| {
                streams(
                    devices,
                    to_call.clone(),
                    from_call.clone(),
                    silenced.clone(),
                )
            };
            let mut using = devices;
            let mut held = open(&using);
            let failed = held.is_err();
            let _ = ready.send(held.as_ref().map(|_| ()).map_err(Clone::clone));
            while !failed && !stopping.load(Ordering::Relaxed) {
                std::thread::park_timeout(std::time::Duration::from_millis(250));
                if !looked.swap(false, Ordering::Relaxed) {
                    continue;
                }
                let wanted = chosen();
                if (&wanted.microphone, &wanted.speaker) == (&using.microphone, &using.speaker) {
                    continue;
                }
                // The old devices go first: one may be the new one too.
                drop(std::mem::replace(&mut held, Err(String::new())));
                held = open(&wanted).or_else(|reason| {
                    tracing::warn!(%reason, "the devices chosen cannot be used: the system's are");
                    open(&CallDevices::default())
                });
                if let Err(reason) = &held {
                    tracing::warn!(%reason, "the call has no sound any more");
                }
                using = wanted;
            }
            drop(held);
        })
        .map_err(|error| format!("The call's audio could not start ({error})."))?;
    match opened.recv() {
        Ok(Ok(())) => Ok(AudioLink {
            microphone,
            speaker,
            muted,
            stop,
            changed,
            thread: Some(thread),
        }),
        Ok(Err(reason)) => Err(reason),
        Err(_) => Err("The call's audio could not start.".into()),
    }
}

fn streams(
    chosen: &CallDevices,
    to_call: Sender<Vec<i16>>,
    from_call: Receiver<Vec<i16>>,
    muted: Arc<AtomicBool>,
) -> Result<(cpal::Stream, cpal::Stream), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();

    let input = named(
        chosen.microphone.as_deref(),
        host.input_devices().into_iter().flatten(),
        "microphone",
    )
    .or_else(|| host.default_input_device())
    .ok_or("No microphone was found.")?;
    let supported = input.default_input_config().map_err(|error| {
        format!("The microphone cannot be used: it may be in use, or not allowed ({error}).")
    })?;
    let config = supported.config();
    let mut capture = Capture::new(config.sample_rate, usize::from(config.channels));
    let mut heard = move |samples: Vec<f32>| {
        for mut frame in capture.push(&samples) {
            if muted.load(Ordering::Relaxed) {
                frame.fill(0);
            }
            // A call that is not taking frames loses them: never wait on
            // the device's thread.
            let _ = to_call.try_send(frame);
        }
    };
    let failed = |error| tracing::warn!(%error, "the call's microphone failed");
    let microphone = match supported.sample_format() {
        cpal::SampleFormat::I16 => input.build_input_stream(
            config,
            move |data: &[i16], _: &_| {
                heard(data.iter().map(|s| f32::from(*s) / 32_768.).collect())
            },
            failed,
            None,
        ),
        cpal::SampleFormat::U16 => input.build_input_stream(
            config,
            move |data: &[u16], _: &_| {
                heard(
                    data.iter()
                        .map(|s| (f32::from(*s) - 32_768.) / 32_768.)
                        .collect(),
                );
            },
            failed,
            None,
        ),
        _ => input.build_input_stream(
            config,
            move |data: &[f32], _: &_| heard(data.to_vec()),
            failed,
            None,
        ),
    }
    .map_err(|error| format!("The microphone could not be opened ({error})."))?;

    let output = named(
        chosen.speaker.as_deref(),
        host.output_devices().into_iter().flatten(),
        "audio output",
    )
    .or_else(|| host.default_output_device())
    .ok_or("No audio output was found.")?;
    let supported = output
        .default_output_config()
        .map_err(|error| format!("The audio output cannot be used ({error})."))?;
    let config = supported.config();
    let mut playout = Playout::new(config.sample_rate, usize::from(config.channels));
    let mut scratch: Vec<f32> = Vec::new();
    let mut render = move |wanted: usize| -> Vec<f32> {
        while playout.hungry(wanted) {
            match from_call.try_recv() {
                Ok(frame) => playout.push(&frame),
                Err(_) => break,
            }
        }
        scratch.clear();
        scratch.resize(wanted, 0.0);
        playout.fill(&mut scratch);
        scratch.clone()
    };
    let failed = |error| tracing::warn!(%error, "the call's audio output failed");
    let speakers = match supported.sample_format() {
        cpal::SampleFormat::I16 => output.build_output_stream(
            config,
            move |out: &mut [i16], _: &_| {
                let rendered = render(out.len());
                for (slot, sample) in out.iter_mut().zip(rendered) {
                    *slot = (sample.clamp(-1.0, 1.0) * 32_767.0) as i16;
                }
            },
            failed,
            None,
        ),
        cpal::SampleFormat::U16 => output.build_output_stream(
            config,
            move |out: &mut [u16], _: &_| {
                let rendered = render(out.len());
                for (slot, sample) in out.iter_mut().zip(rendered) {
                    *slot = ((sample.clamp(-1.0, 1.0) + 1.0) * 32_767.5) as u16;
                }
            },
            failed,
            None,
        ),
        _ => output.build_output_stream(
            config,
            move |out: &mut [f32], _: &_| {
                let rendered = render(out.len());
                out.copy_from_slice(&rendered);
            },
            failed,
            None,
        ),
    }
    .map_err(|error| format!("The audio output could not be opened ({error})."))?;

    microphone
        .play()
        .map_err(|error| format!("The microphone could not be started ({error})."))?;
    speakers
        .play()
        .map_err(|error| format!("The audio output could not be started ({error})."))?;
    Ok((microphone, speakers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, hertz: f32, samples: usize) -> Vec<f32> {
        (0..samples)
            .map(|n| (n as f32 * hertz * std::f32::consts::TAU / rate as f32).sin() * 0.5)
            .collect()
    }

    /// How many times a sound crosses zero going up: its pitch, counted.
    fn crossings(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|pair| pair[0] <= 0.0 && pair[1] > 0.0)
            .count()
    }

    #[test]
    fn a_second_of_sound_is_a_second_at_any_rate() {
        for (from, to) in [
            (48_000, 16_000),
            (44_100, 16_000),
            (16_000, 48_000),
            (16_000, 16_000),
        ] {
            let mut resampler = Resampler::new(from, to);
            let mut out = Vec::new();
            // In pieces of uneven length, as a device hands them over.
            for piece in tone(from, 440.0, from as usize).chunks(441) {
                resampler.push(piece, &mut out);
            }
            let expected = to as usize;
            assert!(
                out.len().abs_diff(expected) <= 2,
                "{from} to {to}: {} samples",
                out.len()
            );
            let pitch = crossings(&out);
            assert!(pitch.abs_diff(440) <= 2, "{from} to {to}: {pitch} Hz");
        }
    }

    #[test]
    fn the_microphone_is_cut_into_frames_of_one_channel() {
        let mut capture = Capture::new(48_000, 2);
        let stereo: Vec<f32> = tone(48_000, 300.0, 48_000)
            .into_iter()
            .flat_map(|sample| [sample, sample])
            .collect();
        let frames: Vec<Vec<i16>> = stereo
            .chunks(960)
            .flat_map(|piece| capture.push(piece))
            .collect();
        assert_eq!(
            frames.len(),
            16,
            "a second is sixteen frames of 60 ms, the last still filling"
        );
        assert!(frames.iter().all(|frame| frame.len() == CALL_FRAME));
        let loudest = frames
            .iter()
            .flatten()
            .map(|sample| sample.unsigned_abs())
            .max()
            .unwrap();
        assert!(
            (15_000..=17_000).contains(&loudest),
            "half scale stays half scale: {loudest}"
        );
    }

    #[test]
    fn what_is_played_fills_every_channel_and_is_silent_when_the_call_is() {
        let mut playout = Playout::new(48_000, 2);
        assert!(playout.hungry(1));
        let mut out = vec![1.0; 64];
        playout.fill(&mut out);
        assert!(
            out.iter().all(|sample| *sample == 0.0),
            "nothing to play is silence"
        );

        playout.push(&vec![16_384; CALL_FRAME]);
        let mut out = vec![0.0; 200];
        playout.fill(&mut out);
        assert!(
            out.chunks(2).all(|frame| frame[0] == frame[1]),
            "both channels"
        );
        assert!((out[100] - 0.5).abs() < 0.01);
    }

    #[test]
    fn a_device_that_fell_behind_skips_to_now() {
        let mut playout = Playout::new(16_000, 1);
        for _ in 0..100 {
            playout.push(&vec![0; CALL_FRAME]);
        }
        // Half a second waits at most, not six.
        assert!(!playout.hungry(7_000));
        assert!(playout.hungry(8_001));
    }
}
