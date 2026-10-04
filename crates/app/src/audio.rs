//! Playing voice notes and other audio.
//!
//! Three parts, each usable without the others:
//!
//! * [`decode`] turns a downloaded file into samples: Ogg/Opus (what a
//!   voice note is) with a pure-Rust Opus decoder, and MP3, AAC/M4A, Ogg
//!   Vorbis and WAV through Symphonia. No system library, no C.
//! * [`Player`] is the state of playback: which clip, playing or paused,
//!   where, how fast. One clip at a time. It knows nothing about sound
//!   cards: it drives an [`AudioOutput`], which the tests replace.
//! * [`SystemOutput`] is the real output, through `cpal`: the samples are
//!   fed from the audio device's own thread, so the UI never produces
//!   sound itself and only reads the position back.
//!
//! Speeds other than 1× keep the pitch of the voice: the clip is
//! time-stretched on its way out (`stretch.rs`).

use crate::stretch::{Shared, Voice};
use std::sync::Arc;
use std::time::Duration;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL, CODEC_TYPE_OPUS};
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// How many bars a waveform has.
pub const BARS: usize = 48;
/// The longest audio that is decoded into memory to be played here.
/// Longer files are opened with another application instead.
const MAX_SECONDS: usize = 30 * 60;

/// Decoded audio: interleaved samples, ready to play.
#[derive(Clone, Debug, PartialEq)]
pub struct Clip {
    /// Interleaved, `channels` per frame.
    pub samples: Vec<i16>,
    /// One or two.
    pub channels: usize,
    /// Frames per second.
    pub rate: u32,
}

impl Clip {
    /// How many frames the clip has.
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    /// How long it lasts.
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.frames() as f64 / f64::from(self.rate.max(1)))
    }

    /// The clip as [`BARS`] heights from 0 to 255: the loudest sample of
    /// each stretch, relative to the loudest of the clip.
    pub fn waveform(&self) -> Vec<u8> {
        let frames = self.frames();
        if frames == 0 {
            return vec![0; BARS];
        }
        let peaks: Vec<u32> = (0..BARS)
            .map(|bar| {
                let from = bar * frames / BARS;
                let to = ((bar + 1) * frames / BARS).max(from + 1).min(frames);
                self.samples[from * self.channels..to * self.channels]
                    .iter()
                    .map(|sample| u32::from(sample.unsigned_abs()))
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let loudest = peaks.iter().copied().max().unwrap_or(0).max(1);
        peaks
            .into_iter()
            .map(|peak| (peak * 255 / loudest) as u8)
            .collect()
    }
}

/// What is kept of a clip once it has been decoded, so that its bubble can
/// show its length and shape without decoding it again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shape {
    /// How long the clip lasts.
    pub duration: Duration,
    /// [`BARS`] heights.
    pub bars: Vec<u8>,
}

impl Shape {
    /// Of a clip.
    pub fn of(clip: &Clip) -> Self {
        Self {
            duration: clip.duration(),
            bars: clip.waveform(),
        }
    }

    /// For the cache: the duration in milliseconds, then the bars.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = (self.duration.as_millis() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&self.bars);
        bytes
    }

    /// From the cache.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let (millis, bars) = bytes.split_first_chunk::<4>()?;
        (bars.len() == BARS).then(|| Self {
            duration: Duration::from_millis(u64::from(u32::from_le_bytes(*millis))),
            bars: bars.to_vec(),
        })
    }
}

fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1., 1.) * f32::from(i16::MAX)) as i16
}

/// Decodes an audio file. The format is told from the content; `mime` is a
/// hint. Blocking and CPU-bound: call it off the UI thread.
pub fn decode(bytes: Vec<u8>, mime: Option<&str>) -> Result<Clip, String> {
    let source = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if let Some(mime) = mime {
        hint.mime_type(mime.split(';').next().unwrap_or(mime).trim());
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("not an audio file this app can play ({error})"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("the file has no audio in it")?;
    let (track_id, params) = (track.id, track.codec_params.clone());
    let channels = params
        .channels
        .map(|channels| channels.count())
        .unwrap_or(1)
        .clamp(1, 2);

    let mut samples: Vec<i16> = Vec::new();
    let rate;
    if params.codec == CODEC_TYPE_OPUS {
        // Opus always decodes at 48 kHz, whatever was recorded.
        rate = 48_000;
        let mut decoder =
            opus_decoder::OpusDecoder::new(rate, channels).map_err(|error| format!("{error:?}"))?;
        let mut frame = vec![0f32; decoder.max_frame_size_per_channel() * channels];
        // The encoder's start-up samples, to be dropped.
        let mut skip = params.delay.unwrap_or(0) as usize * channels;
        while let Ok(packet) = format.next_packet() {
            if packet.track_id() != track_id {
                continue;
            }
            // A damaged packet is a click, not the end of the note.
            let Ok(decoded) = decoder.decode_float(&packet.data, &mut frame, false) else {
                continue;
            };
            let produced = &frame[..decoded * channels];
            let dropped = skip.min(produced.len());
            skip -= dropped;
            samples.extend(produced[dropped..].iter().copied().map(to_i16));
            if samples.len() > MAX_SECONDS * rate as usize * channels {
                return Err("too long to play here".to_owned());
            }
        }
    } else {
        let mut decoder = symphonia::default::get_codecs()
            .make(&params, &DecoderOptions::default())
            .map_err(|error| format!("this kind of audio cannot be played here ({error})"))?;
        let mut found_rate = params.sample_rate;
        let mut buffer: Option<SampleBuffer<i16>> = None;
        let mut source_channels = channels;
        while let Ok(packet) = format.next_packet() {
            if packet.track_id() != track_id {
                continue;
            }
            let Ok(decoded) = decoder.decode(&packet) else {
                continue;
            };
            let spec = *decoded.spec();
            found_rate = Some(spec.rate);
            source_channels = spec.channels.count().max(1);
            let buffer = buffer
                .get_or_insert_with(|| SampleBuffer::<i16>::new(decoded.capacity() as u64, spec));
            buffer.copy_interleaved_ref(decoded);
            // More than two channels: keep the first two.
            if source_channels <= 2 {
                samples.extend_from_slice(buffer.samples());
            } else {
                for frame in buffer.samples().chunks(source_channels) {
                    samples.extend_from_slice(&frame[..2]);
                }
            }
            if samples.len() > MAX_SECONDS * 48_000 * 2 {
                return Err("too long to play here".to_owned());
            }
        }
        rate = found_rate.ok_or("the file does not say how fast to play it")?;
        if samples.is_empty() {
            return Err("the file has no audio in it".to_owned());
        }
        return Ok(Clip {
            samples,
            channels: source_channels.min(2),
            rate,
        });
    }
    if samples.is_empty() {
        return Err("the file has no audio in it".to_owned());
    }
    Ok(Clip {
        samples,
        channels,
        rate,
    })
}

/// How fast a clip is played.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Speed {
    /// As recorded.
    #[default]
    Normal,
    /// 1.5×.
    Faster,
    /// 2×.
    Double,
}

impl Speed {
    /// The factor.
    pub fn factor(self) -> f32 {
        match self {
            Self::Normal => 1.,
            Self::Faster => 1.5,
            Self::Double => 2.,
        }
    }

    /// The next speed of the toggle: 1× → 1.5× → 2× → 1×.
    pub fn next(self) -> Self {
        match self {
            Self::Normal => Self::Faster,
            Self::Faster => Self::Double,
            Self::Double => Self::Normal,
        }
    }

    /// As the toggle shows it.
    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "1×",
            Self::Faster => "1.5×",
            Self::Double => "2×",
        }
    }
}

/// Where sound goes. The real one is [`SystemOutput`]; tests use a fake.
pub trait AudioOutput {
    /// Plays `clip` from `from`, at `speed` times its recorded pace,
    /// replacing whatever was playing. `Err` says why there is no sound
    /// (no output device, say).
    fn play(&mut self, clip: Arc<Clip>, from: Duration, speed: f32) -> Result<(), String>;
    /// What is playing goes on at `speed`, from where it is, without a
    /// gap.
    fn set_speed(&mut self, speed: f32);
    /// The names of the output devices there are.
    fn devices(&self) -> Vec<String>;
    /// Plays on `device` from the next clip on; `None` is the system's
    /// default, which is also used when there is no device of that name.
    fn set_device(&mut self, device: Option<String>);
    /// Silence, now.
    fn halt(&mut self);
    /// Where in the clip the sound is, or `None` once the clip has played
    /// to its end (or nothing is playing).
    fn position(&self) -> Option<Duration>;
}

/// What a clip is doing, for its bubble.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Playback {
    /// Not the clip in the player.
    Idle,
    /// Playing, and where.
    Playing(Duration),
    /// Paused, and where.
    Paused(Duration),
}

struct Track {
    id: String,
    clip: Arc<Clip>,
    /// `Some` while paused: where.
    paused_at: Option<Duration>,
}

/// The player: one clip at a time.
pub struct Player {
    output: Box<dyn AudioOutput>,
    track: Option<Track>,
    speed: Speed,
}

impl Player {
    /// A player on an output.
    pub fn new(output: Box<dyn AudioOutput>) -> Self {
        Self {
            output,
            track: None,
            speed: Speed::default(),
        }
    }

    /// Plays the clip `id` from its start, or from where it was paused if
    /// it is the paused clip. Whatever else was playing stops: there is
    /// only ever one.
    pub fn play(&mut self, id: &str, clip: Arc<Clip>) -> Result<(), String> {
        let from = match &self.track {
            Some(track) if track.id == id => track.paused_at.unwrap_or_default(),
            _ => Duration::ZERO,
        };
        self.start(id, clip, from)
    }

    fn start(&mut self, id: &str, clip: Arc<Clip>, from: Duration) -> Result<(), String> {
        // From the end is from the start: the clip was played through.
        let from = if from >= clip.duration() {
            Duration::ZERO
        } else {
            from
        };
        self.output.halt();
        match self.output.play(clip.clone(), from, self.speed.factor()) {
            Ok(()) => {
                self.track = Some(Track {
                    id: id.to_owned(),
                    clip,
                    paused_at: None,
                });
                Ok(())
            }
            Err(error) => {
                self.track = None;
                Err(error)
            }
        }
    }

    /// Pauses what is playing, keeping its place.
    pub fn pause(&mut self) {
        if let Some(track) = &mut self.track {
            if track.paused_at.is_none() {
                track.paused_at = Some(self.output.position().unwrap_or_default());
                self.output.halt();
            }
        }
    }

    /// Moves within the clip `id`, to `fraction` of its length. Only the
    /// clip in the player can be moved in; playing continues from there,
    /// a paused clip stays paused there.
    pub fn seek(&mut self, id: &str, fraction: f32) -> Result<(), String> {
        let Some(track) = &mut self.track else {
            return Ok(());
        };
        if track.id != id {
            return Ok(());
        }
        let to = track.clip.duration().mul_f32(fraction.clamp(0., 1.));
        if track.paused_at.is_some() {
            track.paused_at = Some(to);
            return Ok(());
        }
        let clip = track.clip.clone();
        self.start(id, clip, to)
    }

    /// The next speed. What is playing goes on from where it is, faster
    /// or slower, without stopping.
    pub fn cycle_speed(&mut self) -> Speed {
        self.set_speed(self.speed.next());
        self.speed
    }

    /// Plays at `speed` from now on: what is playing, and what comes next.
    pub fn set_speed(&mut self, speed: Speed) {
        self.speed = speed;
        if self.is_playing() {
            self.output.set_speed(speed.factor());
        }
    }

    /// The output devices there are.
    pub fn output_devices(&self) -> Vec<String> {
        self.output.devices()
    }

    /// Plays on `device` from the next clip on.
    pub fn set_output_device(&mut self, device: Option<String>) {
        self.output.set_device(device);
    }

    /// The speed in use.
    pub fn speed(&self) -> Speed {
        self.speed
    }

    /// Stops and forgets the clip: the chat was closed, or the session.
    pub fn stop(&mut self) {
        self.output.halt();
        self.track = None;
    }

    /// Looks at the output. Returns the id of the clip if it has just
    /// played to its end, which is the moment to play the next one.
    pub fn poll(&mut self) -> Option<String> {
        let track = self.track.as_ref()?;
        if track.paused_at.is_some() || self.output.position().is_some() {
            return None;
        }
        self.output.halt();
        self.track.take().map(|track| track.id)
    }

    /// The clip `id` goes by `to` from now on: the same clip, playing or
    /// paused where it was.
    pub fn rename(&mut self, id: &str, to: &str) {
        if let Some(track) = self.track.as_mut().filter(|track| track.id == id) {
            track.id = to.to_owned();
        }
    }

    /// What the clip `id` is doing.
    pub fn playback(&self, id: &str) -> Playback {
        match &self.track {
            Some(track) if track.id == id => match track.paused_at {
                Some(at) => Playback::Paused(at),
                None => Playback::Playing(self.output.position().unwrap_or_default()),
            },
            _ => Playback::Idle,
        }
    }

    /// True while a clip is producing sound.
    pub fn is_playing(&self) -> bool {
        self.track
            .as_ref()
            .is_some_and(|track| track.paused_at.is_none())
    }

    /// The id of the clip in the player, playing or paused.
    pub fn current(&self) -> Option<&str> {
        self.track.as_ref().map(|track| track.id.as_str())
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        self.output.halt();
    }
}

/// The system's default audio output, through `cpal`.
///
/// A stream exists only while something plays: the device is opened by
/// [`play`](AudioOutput::play) and released by [`halt`](AudioOutput::halt),
/// so an idle application holds no audio device. The device's thread owns
/// what it plays ([`Voice`]); this side only reads where it is.
#[derive(Default)]
pub struct SystemOutput {
    stream: Option<cpal::Stream>,
    shared: Option<Arc<Shared>>,
    /// The device chosen in the settings; `None` is the system's.
    device: Option<String>,
}

impl AudioOutput for SystemOutput {
    fn play(&mut self, clip: Arc<Clip>, from: Duration, speed: f32) -> Result<(), String> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        self.halt();
        let host = cpal::default_host();
        let named = self.device.as_deref().and_then(|name| {
            host.output_devices().ok()?.find(|device| {
                device
                    .description()
                    .is_ok_and(|description| description.name() == name)
            })
        });
        let device = named
            .or_else(|| host.default_output_device())
            .ok_or("No audio output device")?;
        let supported = device
            .default_output_config()
            .map_err(|error| format!("No usable audio output ({error})"))?;
        let config = supported.config();
        let out_channels = usize::from(config.channels);
        // Everything the device's thread needs is made here; it allocates
        // nothing and takes no lock.
        let (mut voice, shared) = Voice::new(clip, from, speed, config.sample_rate);
        let failed = |error| tracing::warn!(%error, "the audio output failed");
        let stream = match supported.sample_format() {
            cpal::SampleFormat::I16 => device.build_output_stream(
                config,
                move |out: &mut [i16], _: &_| voice.render(out, out_channels),
                failed,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_output_stream(
                config,
                move |out: &mut [u16], _: &_| voice.render(out, out_channels),
                failed,
                None,
            ),
            _ => device.build_output_stream(
                config,
                move |out: &mut [f32], _: &_| voice.render(out, out_channels),
                failed,
                None,
            ),
        }
        .map_err(|error| format!("The audio output could not be opened ({error})"))?;
        stream
            .play()
            .map_err(|error| format!("The audio output could not be started ({error})"))?;
        self.stream = Some(stream);
        self.shared = Some(shared);
        Ok(())
    }

    fn set_speed(&mut self, speed: f32) {
        if let Some(shared) = &self.shared {
            shared.set_speed(speed);
        }
    }

    fn devices(&self) -> Vec<String> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let Ok(devices) = cpal::default_host().output_devices() else {
            return Vec::new();
        };
        devices
            .filter_map(|device| Some(device.description().ok()?.name().to_owned()))
            .collect()
    }

    fn set_device(&mut self, device: Option<String>) {
        self.device = device;
    }

    fn halt(&mut self) {
        // Dropping the stream closes the device.
        self.stream = None;
        self.shared = None;
    }

    fn position(&self) -> Option<Duration> {
        self.shared.as_ref()?.position()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    pub(crate) const VOICE_NOTE: &[u8] = include_bytes!("../tests/assets/voice-note.ogg");
    const MP3: &[u8] = include_bytes!("../tests/assets/tone.mp3");
    const M4A: &[u8] = include_bytes!("../tests/assets/tone.m4a");

    /// What a [`FakeOutput`] was asked to do, and the clock it plays by.
    #[derive(Default)]
    pub(crate) struct Tape {
        /// (frames of the clip, where it started, at what speed).
        pub(crate) started: Vec<(usize, Duration, f32)>,
        pub(crate) halts: usize,
        /// The speeds a playing clip was moved to.
        pub(crate) speeds: Vec<f32>,
        /// The output devices it says there are, and the one chosen.
        pub(crate) names: Vec<String>,
        pub(crate) device: Option<String>,
        /// The clip being played: its length, where it started, the speed,
        /// and how much time has passed since.
        playing: Option<(Duration, Duration, f32, Duration)>,
        /// Set to make the next `play` fail.
        pub(crate) broken: Option<String>,
    }

    impl Tape {
        /// Lets time pass, as the audio device would.
        pub(crate) fn advance(&mut self, by: Duration) {
            if let Some(playing) = &mut self.playing {
                playing.3 += by;
            }
        }
    }

    /// An output that makes no sound and keeps time by hand.
    pub(crate) struct FakeOutput(pub(crate) Rc<RefCell<Tape>>);

    impl AudioOutput for FakeOutput {
        fn play(&mut self, clip: Arc<Clip>, from: Duration, speed: f32) -> Result<(), String> {
            let mut tape = self.0.borrow_mut();
            if let Some(reason) = tape.broken.clone() {
                return Err(reason);
            }
            tape.started.push((clip.frames(), from, speed));
            tape.playing = Some((clip.duration(), from, speed, Duration::ZERO));
            Ok(())
        }

        fn set_speed(&mut self, speed: f32) {
            let at = self.position();
            let mut tape = self.0.borrow_mut();
            tape.speeds.push(speed);
            if let (Some(playing), Some(at)) = (&mut tape.playing, at) {
                // On from where it is, at the new pace.
                (playing.1, playing.2, playing.3) = (at, speed, Duration::ZERO);
            }
        }

        fn devices(&self) -> Vec<String> {
            self.0.borrow().names.clone()
        }

        fn set_device(&mut self, device: Option<String>) {
            self.0.borrow_mut().device = device;
        }

        fn halt(&mut self) {
            let mut tape = self.0.borrow_mut();
            tape.halts += 1;
            tape.playing = None;
        }

        fn position(&self) -> Option<Duration> {
            let (length, from, speed, elapsed) = self.0.borrow().playing?;
            let at = from + elapsed.mul_f32(speed);
            (at < length).then_some(at)
        }
    }

    pub(crate) fn player() -> (Player, Rc<RefCell<Tape>>) {
        let tape = Rc::new(RefCell::new(Tape::default()));
        (Player::new(Box::new(FakeOutput(tape.clone()))), tape)
    }

    /// A clip of silence, `seconds` long.
    pub(crate) fn silence(seconds: u32) -> Arc<Clip> {
        Arc::new(Clip {
            samples: vec![0; (seconds * 8_000) as usize],
            channels: 1,
            rate: 8_000,
        })
    }

    const SECOND: Duration = Duration::from_secs(1);

    #[test]
    fn a_voice_note_decodes_to_its_real_length_and_shape() {
        // Ogg/Opus, as WhatsApp records them: a 2.5 s tone.
        let clip = decode(VOICE_NOTE.to_vec(), Some("audio/ogg; codecs=opus")).unwrap();
        assert_eq!((clip.rate, clip.channels), (48_000, 1));
        let seconds = clip.duration().as_secs_f32();
        assert!((seconds - 2.5).abs() < 0.05, "{seconds} s");
        // It is sound, not silence, all the way through.
        let shape = Shape::of(&clip);
        assert_eq!(shape.bars.len(), BARS);
        assert!(shape.bars.iter().all(|bar| *bar > 100), "{:?}", shape.bars);
        // It survives the cache, to the millisecond.
        let cached = Shape::from_bytes(&shape.to_bytes()).unwrap();
        assert_eq!(cached.bars, shape.bars);
        assert_eq!(cached.duration.as_millis(), shape.duration.as_millis());

        // The type is told from the content: a wrong or missing hint is fine.
        assert!(decode(VOICE_NOTE.to_vec(), None).is_ok());
        assert!(decode(VOICE_NOTE.to_vec(), Some("audio/mpeg")).is_ok());
    }

    #[test]
    fn other_audio_formats_decode_too() {
        let mp3 = decode(MP3.to_vec(), Some("audio/mpeg")).unwrap();
        assert_eq!(mp3.channels, 2);
        assert!((mp3.duration().as_secs_f32() - 1.5).abs() < 0.15);
        let m4a = decode(M4A.to_vec(), Some("audio/mp4")).unwrap();
        assert!((m4a.duration().as_secs_f32() - 1.2).abs() < 0.15);
        assert!(m4a.waveform().iter().any(|bar| *bar > 100));
    }

    #[test]
    fn what_is_not_audio_is_an_error_not_a_crash() {
        assert!(decode(b"<html>".to_vec(), Some("audio/ogg")).is_err());
        assert!(decode(Vec::new(), None).is_err());
        // A voice note cut short still plays what there is of it.
        let half = VOICE_NOTE[..VOICE_NOTE.len() / 2].to_vec();
        let clip = decode(half, None).unwrap();
        assert!(clip.duration() < Duration::from_secs(2));
        assert_eq!(Shape::from_bytes(&[1, 2, 3]), None);
    }

    #[test]
    fn a_waveform_follows_the_sound() {
        // Quiet for the first half, loud for the second.
        let mut samples = vec![100i16; 4_000];
        samples.extend(vec![20_000i16; 4_000]);
        let clip = Clip {
            samples,
            channels: 1,
            rate: 8_000,
        };
        let bars = clip.waveform();
        assert!(bars[..BARS / 2].iter().all(|bar| *bar < 5));
        assert!(bars[BARS / 2..].iter().all(|bar| *bar == 255));
        // Silence is flat, and nothing divides by zero.
        assert_eq!(silence(1).waveform(), vec![0; BARS]);
    }

    #[test]
    fn play_pause_and_resume_keep_the_place() {
        let (mut player, tape) = player();
        assert_eq!(player.playback("a"), Playback::Idle);
        player.play("a", silence(10)).unwrap();
        assert!(player.is_playing());
        tape.borrow_mut().advance(3 * SECOND);
        assert_eq!(player.playback("a"), Playback::Playing(3 * SECOND));

        player.pause();
        assert!(!player.is_playing());
        assert_eq!(player.playback("a"), Playback::Paused(3 * SECOND));
        // Paused is paused, however long.
        tape.borrow_mut().advance(60 * SECOND);
        assert_eq!(player.poll(), None);

        // Playing it again goes on from there, not from the start.
        player.play("a", silence(10)).unwrap();
        assert_eq!(tape.borrow().started.last().unwrap().1, 3 * SECOND);
        assert_eq!(player.playback("a"), Playback::Playing(3 * SECOND));
    }

    #[test]
    fn only_one_clip_plays_at_a_time() {
        let (mut player, tape) = player();
        player.play("a", silence(10)).unwrap();
        tape.borrow_mut().advance(2 * SECOND);
        // Starting another silences the first, which loses its place.
        player.play("b", silence(5)).unwrap();
        assert_eq!(player.current(), Some("b"));
        assert_eq!(player.playback("a"), Playback::Idle);
        assert_eq!(player.playback("b"), Playback::Playing(Duration::ZERO));
        assert_eq!(tape.borrow().started.len(), 2);
        assert!(tape.borrow().halts >= 1, "the first was stopped");
    }

    #[test]
    fn seeking_moves_within_the_clip_in_the_player() {
        let (mut player, tape) = player();
        player.play("a", silence(10)).unwrap();
        player.seek("a", 0.5).unwrap();
        assert_eq!(player.playback("a"), Playback::Playing(5 * SECOND));
        // Out of range is the nearest end.
        player.seek("a", 7.).unwrap();
        assert_eq!(tape.borrow().started.last().unwrap().1, Duration::ZERO);
        // Paused: the place moves, the sound does not start.
        player.seek("a", 0.2).unwrap();
        player.pause();
        let starts = tape.borrow().started.len();
        player.seek("a", 0.75).unwrap();
        let paused = Playback::Paused(Duration::from_millis(7_500));
        assert_eq!(player.playback("a"), paused);
        assert_eq!(tape.borrow().started.len(), starts);
        // Another clip's waveform does nothing to this one.
        player.seek("b", 0.1).unwrap();
        assert_eq!(player.playback("a"), paused);
    }

    #[test]
    fn the_speed_cycles_and_applies_from_where_the_clip_is() {
        let (mut player, tape) = player();
        assert_eq!(player.speed().label(), "1×");
        player.play("a", silence(10)).unwrap();
        tape.borrow_mut().advance(2 * SECOND);
        assert_eq!(player.cycle_speed(), Speed::Faster);
        // The clip goes on, faster: it is not started again.
        assert_eq!(tape.borrow().started.len(), 1);
        assert_eq!(tape.borrow().speeds, [1.5]);
        assert_eq!(player.playback("a"), Playback::Playing(2 * SECOND));
        // Two seconds of listening at 1.5× are three seconds of the clip.
        tape.borrow_mut().advance(2 * SECOND);
        assert_eq!(player.playback("a"), Playback::Playing(5 * SECOND));
        assert_eq!(player.cycle_speed(), Speed::Double);
        assert_eq!(player.cycle_speed(), Speed::Normal);
        // The choice outlives the clip: the next one starts at it.
        player.cycle_speed();
        player.play("b", silence(4)).unwrap();
        assert_eq!(tape.borrow().started.last().unwrap().2, 1.5);
    }

    #[test]
    fn a_clip_that_ends_is_reported_once_and_stopping_forgets_it() {
        let (mut player, tape) = player();
        player.play("a", silence(4)).unwrap();
        tape.borrow_mut().advance(3 * SECOND);
        assert_eq!(player.poll(), None, "still playing");
        tape.borrow_mut().advance(2 * SECOND);
        assert_eq!(
            player.poll(),
            Some("a".to_owned()),
            "the cue for the next one"
        );
        assert_eq!(player.poll(), None);
        assert_eq!(player.playback("a"), Playback::Idle);
        assert!(!player.is_playing());

        // The chat closes mid-clip: silence, and nothing to resume.
        player.play("b", silence(4)).unwrap();
        let halts = tape.borrow().halts;
        player.stop();
        assert!(tape.borrow().halts > halts);
        assert_eq!(player.current(), None);
        assert_eq!(player.poll(), None);
    }

    #[test]
    fn no_output_device_is_an_answer_not_a_silence() {
        let (mut player, tape) = player();
        tape.borrow_mut().broken = Some("No audio output device".into());
        let error = player.play("a", silence(3)).unwrap_err();
        assert_eq!(error, "No audio output device");
        assert!(!player.is_playing());
        assert_eq!(player.playback("a"), Playback::Idle);
        // When there is one again, it plays.
        tape.borrow_mut().broken = None;
        player.play("a", silence(3)).unwrap();
        assert!(player.is_playing());
    }
}
