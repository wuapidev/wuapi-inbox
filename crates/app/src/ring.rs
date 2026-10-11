//! The sound of a call that rings: here when somebody calls, and the
//! quiet tone that says it rings there when the account calls.
//!
//! The tones are made, not played from a file: a few notes that fade, on
//! the audio output chosen in the settings. The device is open only while
//! something rings.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// What rings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ringing {
    /// Somebody calls: meant to be heard across a room.
    Incoming,
    /// The account calls and it rings on the other side: meant for the
    /// person who is waiting at the screen.
    Outgoing,
}

/// One note of a tone: when it starts in the tone's round, its pitch, and
/// how long it takes to fade to a third.
struct Note {
    at: f32,
    hertz: f32,
    fade: f32,
}

impl Ringing {
    /// How long one round of the tone is, silence included.
    fn round(self) -> f32 {
        match self {
            Self::Incoming => 2.6,
            Self::Outgoing => 4.0,
        }
    }

    fn loudness(self) -> f32 {
        match self {
            Self::Incoming => 0.32,
            Self::Outgoing => 0.07,
        }
    }

    fn notes(self) -> &'static [Note] {
        const fn note(at: f32, hertz: f32, fade: f32) -> Note {
            Note { at, hertz, fade }
        }
        // A rising chord, said twice.
        const INCOMING: [Note; 6] = [
            note(0.00, 783.99, 0.22),
            note(0.16, 987.77, 0.22),
            note(0.32, 1174.66, 0.30),
            note(0.90, 783.99, 0.22),
            note(1.06, 987.77, 0.22),
            note(1.22, 1174.66, 0.30),
        ];
        // Two low notes together, long and soft.
        const OUTGOING: [Note; 2] = [note(0.0, 440.0, 0.9), note(0.0, 554.37, 0.9)];
        match self {
            Self::Incoming => &INCOMING,
            Self::Outgoing => &OUTGOING,
        }
    }

    /// The tone at `seconds` into its round, between -1 and 1.
    pub fn sample(self, seconds: f32) -> f32 {
        let at = seconds % self.round();
        let mut sum = 0.0;
        for note in self.notes() {
            let since = at - note.at;
            if since < 0.0 {
                continue;
            }
            // A soft start, so that a note does not click.
            let rise = (since / 0.008).min(1.0);
            let level = rise * (-since / note.fade).exp();
            let turn = since * note.hertz * std::f32::consts::TAU;
            // The note and a little of its octave: a bell more than a beep.
            sum += level * (turn.sin() + 0.25 * (turn * 2.0).sin());
        }
        (sum * self.loudness()).clamp(-1.0, 1.0)
    }
}

/// Rings while it is told to.
#[derive(Default)]
pub struct Ringer {
    ringing: Option<Ringing>,
    stream: Option<cpal::Stream>,
}

impl Ringer {
    /// What rings now.
    #[cfg(test)]
    pub fn ringing(&self) -> Option<Ringing> {
        self.ringing
    }

    /// Rings `what` on the output called `device` (`None`: the system's),
    /// or stops. Told the same again, it goes on as it is.
    pub fn set(&mut self, what: Option<Ringing>, device: Option<&str>) {
        if what == self.ringing {
            return;
        }
        self.ringing = what;
        // Dropping the stream closes the device.
        self.stream = None;
        // Under test nothing is opened: what would ring is all there is.
        if cfg!(test) {
            return;
        }
        if let Some(what) = what {
            match open(what, device) {
                Ok(stream) => self.stream = Some(stream),
                Err(error) => tracing::warn!(%error, "a call could not be rung"),
            }
        }
    }
}

fn open(what: Ringing, device: Option<&str>) -> Result<cpal::Stream, String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let host = cpal::default_host();
    let named = device.and_then(|name| {
        host.output_devices().ok()?.find(|device| {
            device
                .description()
                .is_ok_and(|description| description.name() == name)
        })
    });
    let device = named
        .or_else(|| host.default_output_device())
        .ok_or("no audio output device")?;
    let supported = device
        .default_output_config()
        .map_err(|error| error.to_string())?;
    let config = supported.config();
    let channels = usize::from(config.channels).max(1);
    let rate = config.sample_rate as f32;
    let played = Arc::new(AtomicU64::new(0));
    let next = move |frames: usize| -> Vec<f32> {
        let from = played.fetch_add(frames as u64, Ordering::Relaxed);
        // Within a round, so that the count stays exact however long it
        // rings.
        let round = (what.round() * rate) as u64;
        (0..frames as u64)
            .map(|frame| what.sample(((from + frame) % round) as f32 / rate))
            .collect()
    };
    let failed = |error| tracing::warn!(%error, "the ring's audio output failed");
    let stream = match supported.sample_format() {
        cpal::SampleFormat::I16 => device.build_output_stream(
            config,
            move |out: &mut [i16], _: &_| {
                let tone = next(out.len() / channels);
                for (frame, sample) in out.chunks_mut(channels).zip(tone) {
                    frame.fill((sample * 32_767.0) as i16);
                }
            },
            failed,
            None,
        ),
        cpal::SampleFormat::U16 => device.build_output_stream(
            config,
            move |out: &mut [u16], _: &_| {
                let tone = next(out.len() / channels);
                for (frame, sample) in out.chunks_mut(channels).zip(tone) {
                    frame.fill(((sample + 1.0) * 32_767.5) as u16);
                }
            },
            failed,
            None,
        ),
        _ => device.build_output_stream(
            config,
            move |out: &mut [f32], _: &_| {
                let tone = next(out.len() / channels);
                for (frame, sample) in out.chunks_mut(channels).zip(tone) {
                    frame.fill(sample);
                }
            },
            failed,
            None,
        ),
    }
    .map_err(|error| error.to_string())?;
    stream.play().map_err(|error| error.to_string())?;
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ring_is_notes_then_silence_and_never_too_loud() {
        for what in [Ringing::Incoming, Ringing::Outgoing] {
            let rate = 8_000.0;
            let round: Vec<f32> = (0..(what.round() * rate) as usize)
                .map(|n| what.sample(n as f32 / rate))
                .collect();
            let loudest = round.iter().fold(0.0f32, |most, s| most.max(s.abs()));
            assert!(loudest > 0.02 && loudest < 0.8, "{what:?}: {loudest}");
            // The end of a round is quiet: the next one starts from rest.
            let tail = &round[round.len() - 400..];
            assert!(tail.iter().all(|s| s.abs() < 0.02), "{what:?}");
            // It starts from nothing, without a click.
            assert!(round[0].abs() < 0.01);
        }
        // Somebody calling is heard further than the wait for an answer.
        assert!(Ringing::Incoming.loudness() > Ringing::Outgoing.loudness() * 3.0);
    }

    #[test]
    fn a_ringer_rings_what_it_is_told_and_stops() {
        let mut ringer = Ringer::default();
        assert_eq!(ringer.ringing(), None);
        ringer.set(Some(Ringing::Incoming), None);
        assert_eq!(ringer.ringing(), Some(Ringing::Incoming));
        ringer.set(None, None);
        assert_eq!(ringer.ringing(), None);
    }
}
