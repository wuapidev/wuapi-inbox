//! Recording a voice note.
//!
//! Four parts, each usable without the others, like `audio.rs`:
//!
//! * [`AudioInput`] is where sound comes from. The real one,
//!   [`SystemInput`], is the system's microphone through `cpal`, read on
//!   the device's own thread; the tests bring their own, and nothing in
//!   them ever opens a microphone.
//! * [`Resampler`] brings whatever the device gives (44.1 or 48 kHz, one
//!   or two channels) to what a voice note is: one channel at 16 kHz.
//! * [`Recorder`] is the state of a recording: started, paused, resumed,
//!   stopped or thrown away. The device is open only between start and
//!   stop: nothing is heard before the user asks, and a recording that is
//!   cancelled is dropped where it stands.
//! * [`Take`] is a finished recording, and [`Take::encode`] makes the
//!   file WhatsApp takes as a voice note of it: Opus in an Ogg container
//!   (RFC 7845), mono, 16 kHz in, as the phones record them.

use crate::audio::Clip;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::Duration;

/// Frames a second of a recording: wideband, what WhatsApp's own voice
/// notes are recorded at.
pub const RATE: u32 = 16_000;
/// The longest recording: it stops by itself there. WhatsApp states no
/// limit of its own for voice notes; a quarter of an hour is about 2.7 MB
/// at the rate used here, well under what the upload takes.
pub const LONGEST: Duration = Duration::from_secs(15 * 60);
/// A recording shorter than this is not sent: a slip of the finger.
pub const SHORTEST: Duration = Duration::from_secs(1);
/// How much sound one bar of the live meter stands for.
pub const LEVEL_EVERY: Duration = Duration::from_millis(50);
/// Bits a second of the encoded voice.
const BITRATE: u32 = 24_000;
/// Frames in one Opus packet: 20 ms.
const PACKET: usize = RATE as usize / 50;
/// The MIME type of a voice note.
pub const MIME: &str = "audio/ogg; codecs=opus";

/// What an input device gives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    /// Frames a second.
    pub rate: u32,
    /// Samples per frame.
    pub channels: usize,
}

/// Where the device's thread puts what it hears: interleaved samples
/// from -1 to 1.
pub type Sink = Sender<Vec<f32>>;

/// Where sound comes from. The real one is [`SystemInput`].
pub trait AudioInput {
    /// The names of the input devices there are.
    fn devices(&self) -> Vec<String>;
    /// Opens `device` (the system's default when `None`, or when there is
    /// no device of that name any more) and starts feeding `sink`. `Err`
    /// says why there is no sound, in words for the user.
    fn open(&mut self, device: Option<&str>, sink: Sink) -> Result<Format, String>;
    /// Closes the device, now.
    fn close(&mut self);
}

/// The system's microphone, through `cpal`. A stream exists only while a
/// recording is under way.
// Never made in a test build: the tests have no microphone.
#[cfg_attr(test, allow(dead_code))]
#[derive(Default)]
pub struct SystemInput {
    stream: Option<cpal::Stream>,
}

impl AudioInput for SystemInput {
    fn devices(&self) -> Vec<String> {
        use cpal::traits::{DeviceTrait, HostTrait};
        let Ok(devices) = cpal::default_host().input_devices() else {
            return Vec::new();
        };
        devices
            .filter_map(|device| Some(device.description().ok()?.name().to_owned()))
            .collect()
    }

    fn open(&mut self, device: Option<&str>, sink: Sink) -> Result<Format, String> {
        use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
        self.close();
        let host = cpal::default_host();
        let named = device.and_then(|name| {
            host.input_devices().ok()?.find(|device| {
                device
                    .description()
                    .is_ok_and(|description| description.name() == name)
            })
        });
        let device = named
            .or_else(|| host.default_input_device())
            .ok_or("No microphone was found.")?;
        let supported = device.default_input_config().map_err(|error| {
            format!("The microphone cannot be used: it may be in use, or not allowed ({error}).")
        })?;
        let config = supported.config();
        let format = Format {
            rate: config.sample_rate,
            channels: usize::from(config.channels).max(1),
        };
        let failed = |error| tracing::warn!(%error, "the audio input failed");
        // The device's thread hands over what it heard and goes back to
        // listening; everything else happens elsewhere.
        let stream = match supported.sample_format() {
            cpal::SampleFormat::I16 => device.build_input_stream(
                config,
                move |data: &[i16], _: &_| {
                    let heard = data.iter().map(|s| f32::from(*s) / 32_768.).collect();
                    let _ = sink.send(heard);
                },
                failed,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                config,
                move |data: &[u16], _: &_| {
                    let heard = data
                        .iter()
                        .map(|s| (f32::from(*s) - 32_768.) / 32_768.)
                        .collect();
                    let _ = sink.send(heard);
                },
                failed,
                None,
            ),
            _ => device.build_input_stream(
                config,
                move |data: &[f32], _: &_| {
                    let _ = sink.send(data.to_vec());
                },
                failed,
                None,
            ),
        }
        .map_err(|error| {
            format!(
                "The microphone could not be opened: it may be in use, or not allowed ({error})."
            )
        })?;
        stream
            .play()
            .map_err(|error| format!("The microphone could not be started ({error})."))?;
        self.stream = Some(stream);
        Ok(format)
    }

    fn close(&mut self) {
        // Dropping the stream closes the device.
        self.stream = None;
    }
}

/// How far either side of a sample the resampler looks, in periods of the
/// slower of the two rates.
const REACH: f64 = 8.;

/// Brings interleaved sound at a device's rate and channels to one
/// channel at [`RATE`], as it arrives: a windowed-sinc filter that keeps
/// what a voice has below the new rate's half and nothing above it.
pub struct Resampler {
    channels: usize,
    /// Frames of the device per frame out.
    step: f64,
    /// The share of the device's band that is kept.
    cutoff: f64,
    /// How far the filter reaches, in frames of the device.
    reach: f64,
    /// What has been heard and may still be needed, one channel.
    heard: Vec<f32>,
    /// The frame of the device that `heard[0]` is.
    first: f64,
    /// Where the next frame out is, in frames of the device.
    at: f64,
}

impl Resampler {
    /// For a device of this format.
    pub fn new(format: Format) -> Self {
        let step = f64::from(format.rate.max(1)) / f64::from(RATE);
        let cutoff = (1. / step).min(1.);
        Self {
            channels: format.channels.max(1),
            step,
            cutoff,
            reach: REACH / cutoff,
            heard: Vec::new(),
            first: 0.,
            at: 0.,
        }
    }

    /// Takes in what the device heard and adds what can be told of it to
    /// `out`, at [`RATE`].
    pub fn feed(&mut self, interleaved: &[f32], out: &mut Vec<f32>) {
        self.heard.extend(
            interleaved
                .chunks(self.channels)
                .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32),
        );
        let have = self.first + self.heard.len() as f64;
        while self.at + self.reach < have {
            out.push(self.sample(self.at));
            self.at += self.step;
        }
        // What is behind the filter's reach is not needed again.
        let keep_from = (self.at - self.reach).floor();
        let spent = ((keep_from - self.first).max(0.) as usize).min(self.heard.len());
        if spent > 0 {
            self.heard.drain(..spent);
            self.first += spent as f64;
        }
    }

    /// The sound at frame `at` of the device.
    fn sample(&self, at: f64) -> f32 {
        let from = (at - self.reach).ceil().max(self.first) as i64;
        let to =
            ((at + self.reach).floor() as i64).min(self.first as i64 + self.heard.len() as i64 - 1);
        let (mut sum, mut weight) = (0f64, 0f64);
        for frame in from..=to {
            let distance = frame as f64 - at;
            let x = distance * self.cutoff * std::f64::consts::PI;
            let sinc = if x.abs() < 1e-9 { 1. } else { x.sin() / x };
            let window = 0.5 * (1. + (std::f64::consts::PI * distance / self.reach).cos());
            let w = sinc * window;
            sum += w * f64::from(self.heard[(frame - self.first as i64) as usize]);
            weight += w;
        }
        if weight.abs() < 1e-9 {
            0.
        } else {
            (sum / weight) as f32
        }
    }
}

/// A recording under way.
pub struct Recorder {
    input: Box<dyn AudioInput>,
    heard: Option<(Receiver<Vec<f32>>, Resampler)>,
    /// What has been recorded: one channel at [`RATE`].
    samples: Vec<f32>,
    /// The loudest of each [`LEVEL_EVERY`], from 0 to 255.
    levels: Vec<u8>,
    paused: bool,
}

impl Recorder {
    /// A recorder on an input. Nothing is opened.
    pub fn new(input: Box<dyn AudioInput>) -> Self {
        Self {
            input,
            heard: None,
            samples: Vec::new(),
            levels: Vec::new(),
            paused: false,
        }
    }

    /// The input devices there are.
    pub fn devices(&self) -> Vec<String> {
        self.input.devices()
    }

    /// Opens the microphone and starts a recording, dropping any that was
    /// under way.
    pub fn start(&mut self, device: Option<&str>) -> Result<(), String> {
        self.cancel();
        let (sink, heard) = channel();
        let format = self.input.open(device, sink)?;
        self.heard = Some((heard, Resampler::new(format)));
        Ok(())
    }

    /// Whether the microphone is open.
    pub fn is_open(&self) -> bool {
        self.heard.is_some()
    }

    /// Whether the recording is paused.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    /// Stops taking in what the microphone hears, keeping what there is.
    pub fn pause(&mut self) {
        self.pump();
        self.paused = true;
    }

    /// Goes on recording.
    pub fn resume(&mut self) {
        // What was said during the pause is not part of the note.
        self.drain();
        self.paused = false;
    }

    /// Throws away what the device heard and has not been taken in.
    fn drain(&mut self) {
        if let Some((heard, _)) = &self.heard {
            while heard.try_recv().is_ok() {}
        }
    }

    /// Takes in what the microphone heard since the last time. `true`
    /// when the recording has reached [`LONGEST`]: time to stop it.
    pub fn pump(&mut self) -> bool {
        if self.paused {
            self.drain();
            return false;
        }
        let longest = (LONGEST.as_secs_f64() * f64::from(RATE)) as usize;
        if let Some((heard, resampler)) = &mut self.heard {
            while let Ok(chunk) = heard.try_recv() {
                resampler.feed(&chunk, &mut self.samples);
            }
        }
        self.samples.truncate(longest);
        // The meter: one bar for every stretch that is complete.
        let every = (LEVEL_EVERY.as_secs_f64() * f64::from(RATE)) as usize;
        while (self.levels.len() + 1) * every <= self.samples.len() {
            let from = self.levels.len() * every;
            let peak = self.samples[from..from + every]
                .iter()
                .fold(0f32, |peak, sample| peak.max(sample.abs()));
            self.levels.push((peak.min(1.) * 255.) as u8);
        }
        self.samples.len() >= longest
    }

    /// How much has been recorded.
    pub fn elapsed(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(RATE))
    }

    /// The meter so far.
    pub fn levels(&self) -> &[u8] {
        &self.levels
    }

    /// Closes the microphone and throws the recording away.
    pub fn cancel(&mut self) {
        self.input.close();
        self.heard = None;
        self.paused = false;
        // Not only forgotten: gone from memory.
        self.samples.fill(0.);
        self.samples = Vec::new();
        self.levels = Vec::new();
    }

    /// Closes the microphone and hands the recording over.
    pub fn stop(&mut self) -> Take {
        self.pump();
        self.input.close();
        self.heard = None;
        self.paused = false;
        self.levels = Vec::new();
        let samples = std::mem::take(&mut self.samples)
            .into_iter()
            .map(|sample| (sample.clamp(-1., 1.) * 32_767.) as i16)
            .collect();
        Take { samples }
    }
}

/// A finished recording: one channel at [`RATE`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Take {
    samples: Vec<i16>,
}

impl Take {
    /// How long it lasts.
    pub fn duration(&self) -> Duration {
        Duration::from_secs_f64(self.samples.len() as f64 / f64::from(RATE))
    }

    /// Erases the sound: a recording thrown away, or sent, is not left
    /// lying in memory.
    pub fn erase(&mut self) {
        self.samples.fill(0);
        self.samples = Vec::new();
    }

    /// The recording as something to play, and to draw the waveform of.
    pub fn clip(&self) -> Clip {
        Clip {
            samples: self.samples.clone(),
            channels: 1,
            rate: RATE,
        }
    }

    /// The voice note: Opus packets of 20 ms in an Ogg container. CPU
    /// bound: call it off the interface's thread.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        use ropus::{Application, Bitrate, Channels, Encoder};
        let mut encoder = Encoder::builder(RATE, Channels::Mono, Application::Voip)
            .bitrate(Bitrate::Bits(BITRATE))
            .build()
            .map_err(|error| format!("The recording could not be encoded ({error})."))?;
        // What the encoder holds back at the start, which a player skips:
        // in 48 kHz samples, the unit of everything in the container.
        let pre_skip = encoder.lookahead() * (48_000 / RATE);
        let mut ogg = OggWriter::new(serial_of(&self.samples));
        let mut head = b"OpusHead".to_vec();
        head.push(1); // version
        head.push(1); // channels
        head.extend_from_slice(&(pre_skip as u16).to_le_bytes());
        head.extend_from_slice(&RATE.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes()); // output gain
        head.push(0); // one stream, mono or stereo
        ogg.page(&[&head], 0, false);
        let vendor = b"wuapi-inbox";
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes());
        ogg.page(&[&tags], 0, false);

        // The sound, and after it enough silence to push the encoder's
        // delay through; the last page says where the sound really ends.
        let total = self.samples.len() + encoder.lookahead() as usize;
        let packets = total.div_ceil(PACKET).max(1);
        let end = u64::from(pre_skip) + self.samples.len() as u64 * u64::from(48_000 / RATE);
        let mut frame = [0i16; PACKET];
        let mut packet = [0u8; 1_500];
        let mut page: Vec<Vec<u8>> = Vec::new();
        for index in 0..packets {
            let from = (index * PACKET).min(self.samples.len());
            let to = (from + PACKET).min(self.samples.len());
            frame.fill(0);
            frame[..to - from].copy_from_slice(&self.samples[from..to]);
            let length = encoder
                .encode(&frame, &mut packet)
                .map_err(|error| format!("The recording could not be encoded ({error})."))?;
            page.push(packet[..length].to_vec());
            let last = index + 1 == packets;
            // A page a second, as the reference encoder writes them.
            if page.len() == 50 || last {
                let encoded = (index as u64 + 1) * PACKET as u64 * u64::from(48_000 / RATE);
                let granule = if last { end } else { encoded };
                let parts: Vec<&[u8]> = page.iter().map(Vec::as_slice).collect();
                ogg.page(&parts, granule, last);
                page.clear();
            }
        }
        Ok(ogg.bytes)
    }
}

/// A stream's serial number: anything, as long as two streams in one file
/// differ. Taken from the sound itself, so the same take gives the same
/// file.
fn serial_of(samples: &[i16]) -> u32 {
    samples.iter().fold(0x9E37_79B9u32, |hash, sample| {
        hash.rotate_left(5) ^ (*sample as u16 as u32)
    })
}

/// Writes Ogg pages.
struct OggWriter {
    bytes: Vec<u8>,
    serial: u32,
    sequence: u32,
}

impl OggWriter {
    fn new(serial: u32) -> Self {
        Self {
            bytes: Vec::new(),
            serial,
            sequence: 0,
        }
    }

    /// One page holding whole packets, ending at `granule`.
    fn page(&mut self, packets: &[&[u8]], granule: u64, last: bool) {
        // A packet is cut into segments of 255 bytes; the one shorter
        // than that (maybe empty) ends it.
        let mut lacing = Vec::new();
        for packet in packets {
            lacing.extend(std::iter::repeat_n(255u8, packet.len() / 255));
            lacing.push((packet.len() % 255) as u8);
        }
        debug_assert!(lacing.len() <= 255, "too much for one page");
        let start = self.bytes.len();
        self.bytes.extend_from_slice(b"OggS");
        self.bytes.push(0);
        let first = self.sequence == 0;
        self.bytes
            .push(if first { 0x02 } else { 0 } | if last { 0x04 } else { 0 });
        self.bytes.extend_from_slice(&granule.to_le_bytes());
        self.bytes.extend_from_slice(&self.serial.to_le_bytes());
        self.bytes.extend_from_slice(&self.sequence.to_le_bytes());
        self.bytes.extend_from_slice(&[0; 4]);
        self.bytes.push(lacing.len() as u8);
        self.bytes.extend_from_slice(&lacing);
        for packet in packets {
            self.bytes.extend_from_slice(packet);
        }
        let checksum = ogg_crc(&self.bytes[start..]);
        self.bytes[start + 22..start + 26].copy_from_slice(&checksum.to_le_bytes());
        self.sequence += 1;
    }
}

/// Ogg's checksum: CRC-32 with the polynomial 0x04C11DB7, from zero, not
/// reflected.
fn ogg_crc(bytes: &[u8]) -> u32 {
    let mut crc = 0u32;
    for byte in bytes {
        crc ^= u32::from(*byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What a [`FakeInput`] was asked, and the way to make it "hear".
    #[derive(Default)]
    pub(crate) struct Mic {
        pub(crate) format: Option<Format>,
        pub(crate) opened: Vec<Option<String>>,
        pub(crate) closes: usize,
        pub(crate) sink: Option<Sink>,
        /// Set to make the next `open` fail.
        pub(crate) broken: Option<String>,
        pub(crate) names: Vec<String>,
        /// Where the tone it hears is, in frames.
        at: usize,
    }

    impl Mic {
        /// The microphone hears a tone of `hz` for `seconds`.
        pub(crate) fn hear(&mut self, hz: f32, seconds: f32) {
            let format = self.format.expect("the microphone is open");
            let frames = (seconds * format.rate as f32) as usize;
            let mut heard = Vec::with_capacity(frames * format.channels);
            for frame in self.at..self.at + frames {
                let value =
                    (frame as f32 * hz * std::f32::consts::TAU / format.rate as f32).sin() * 0.5;
                heard.extend(std::iter::repeat_n(value, format.channels));
            }
            self.at += frames;
            // In buffers, as a device hands them over.
            for buffer in heard.chunks(441 * format.channels) {
                let _ = self.sink.as_ref().expect("open").send(buffer.to_vec());
            }
        }
    }

    /// A microphone that hears what the test says and nothing else.
    pub(crate) struct FakeInput(pub(crate) Rc<RefCell<Mic>>, pub(crate) Format);

    impl AudioInput for FakeInput {
        fn devices(&self) -> Vec<String> {
            self.0.borrow().names.clone()
        }

        fn open(&mut self, device: Option<&str>, sink: Sink) -> Result<Format, String> {
            let mut mic = self.0.borrow_mut();
            if let Some(reason) = mic.broken.clone() {
                return Err(reason);
            }
            mic.opened.push(device.map(str::to_owned));
            mic.sink = Some(sink);
            mic.format = Some(self.1);
            mic.at = 0;
            Ok(self.1)
        }

        fn close(&mut self) {
            let mut mic = self.0.borrow_mut();
            if mic.sink.take().is_some() {
                mic.closes += 1;
            }
            mic.format = None;
        }
    }

    pub(crate) fn recorder(rate: u32, channels: usize) -> (Recorder, Rc<RefCell<Mic>>) {
        let mic = Rc::new(RefCell::new(Mic::default()));
        let input = FakeInput(mic.clone(), Format { rate, channels });
        (Recorder::new(Box::new(input)), mic)
    }

    /// The loudest frequency between 80 and 2000 Hz.
    fn pitch(samples: &[f32], rate: u32) -> f32 {
        let mut best = (0f32, 0f32);
        let mut hz = 80f32;
        while hz <= 2_000. {
            let step = std::f32::consts::TAU * hz / rate as f32;
            let (mut re, mut im) = (0f32, 0f32);
            for (n, sample) in samples.iter().enumerate() {
                re += sample * (n as f32 * step).cos();
                im += sample * (n as f32 * step).sin();
            }
            if re * re + im * im > best.1 {
                best = (hz, re * re + im * im);
            }
            hz += 2.;
        }
        best.0
    }

    #[test]
    fn every_device_is_brought_to_one_channel_at_sixteen_kilohertz() {
        for rate in [16_000, 44_100, 48_000] {
            for channels in [1, 2] {
                let (mut recorder, mic) = recorder(rate, channels);
                recorder.start(None).unwrap();
                mic.borrow_mut().hear(440., 2.);
                recorder.pump();
                let take = recorder.stop();
                let seconds = take.duration().as_secs_f32();
                assert!(
                    (seconds - 2.).abs() < 0.01,
                    "{rate} Hz x{channels}: {seconds} s"
                );
                let heard: Vec<f32> = take
                    .samples
                    .iter()
                    .map(|sample| f32::from(*sample) / 32_768.)
                    .collect();
                // The same tone, as loud as it was.
                let found = pitch(&heard[8_000..24_000], RATE);
                assert!(
                    (found - 440.).abs() <= 4.,
                    "{rate} Hz x{channels}: {found} Hz"
                );
                let loudest = heard[8_000..24_000]
                    .iter()
                    .fold(0f32, |a, b| a.max(b.abs()));
                assert!((loudest - 0.5).abs() < 0.02, "{loudest}");
            }
        }
        // What a voice note cannot carry is not folded back into it: a
        // tone above half the new rate is gone, not somewhere else.
        let (mut recorder, mic) = recorder(48_000, 1);
        recorder.start(None).unwrap();
        mic.borrow_mut().hear(11_000., 1.);
        let take = recorder.stop();
        let loudest = take.samples[4_000..12_000]
            .iter()
            .fold(0i16, |a, b| a.max(b.abs()));
        assert!(loudest < 400, "{loudest}");
    }

    #[test]
    fn a_recording_starts_pauses_resumes_and_stops() {
        let (mut recorder, mic) = recorder(48_000, 1);
        assert!(!recorder.is_open());
        assert_eq!(
            mic.borrow().opened.len(),
            0,
            "nothing is open before a start"
        );
        recorder.start(Some("USB microphone")).unwrap();
        assert!(recorder.is_open());
        assert_eq!(
            mic.borrow().opened,
            [Some("USB microphone".to_owned())],
            "the device that was chosen"
        );
        mic.borrow_mut().hear(300., 1.);
        assert!(!recorder.pump());
        assert!((recorder.elapsed().as_secs_f32() - 1.).abs() < 0.01);
        // A bar for every 50 ms, and they say there was sound.
        assert!((19..=20).contains(&recorder.levels().len()));
        assert!(recorder.levels().iter().skip(1).all(|level| *level > 100));

        // Paused: what is said is not recorded, and the place stays.
        recorder.pause();
        let paused_at = recorder.elapsed();
        mic.borrow_mut().hear(300., 3.);
        recorder.pump();
        assert_eq!(recorder.elapsed(), paused_at);
        recorder.resume();
        mic.borrow_mut().hear(300., 0.5);
        recorder.pump();
        assert!((recorder.elapsed().as_secs_f32() - 1.5).abs() < 0.02);

        // Stopped: the microphone is closed, the take is what was heard.
        let take = recorder.stop();
        assert!(!recorder.is_open());
        assert_eq!(mic.borrow().closes, 1);
        assert!(mic.borrow().sink.is_none());
        assert!((take.duration().as_secs_f32() - 1.5).abs() < 0.02);
        assert_eq!(recorder.elapsed(), Duration::ZERO);
    }

    #[test]
    fn a_cancelled_recording_is_gone_and_the_microphone_closed() {
        let (mut recorder, mic) = recorder(44_100, 2);
        recorder.start(None).unwrap();
        mic.borrow_mut().hear(300., 2.);
        recorder.pump();
        recorder.cancel();
        assert!(!recorder.is_open());
        assert_eq!(mic.borrow().closes, 1);
        assert_eq!(recorder.elapsed(), Duration::ZERO);
        assert!(recorder.levels().is_empty());
        assert_eq!(recorder.stop().duration(), Duration::ZERO);
    }

    #[test]
    fn a_recording_stops_at_its_longest() {
        let (mut recorder, mic) = recorder(16_000, 1);
        recorder.start(None).unwrap();
        // A quarter of an hour and a little more, in silence.
        let minute = vec![0f32; 16_000 * 60];
        for _ in 0..15 {
            mic.borrow()
                .sink
                .as_ref()
                .unwrap()
                .send(minute.clone())
                .unwrap();
            assert!(!recorder.pump() || recorder.elapsed() >= LONGEST);
        }
        mic.borrow().sink.as_ref().unwrap().send(minute).unwrap();
        assert!(recorder.pump(), "time to stop");
        assert_eq!(recorder.elapsed(), LONGEST);
    }

    #[test]
    fn no_microphone_is_an_answer_not_a_silence() {
        let (mut recorder, mic) = recorder(48_000, 1);
        mic.borrow_mut().broken = Some("No microphone was found.".into());
        assert_eq!(
            recorder.start(None).unwrap_err(),
            "No microphone was found."
        );
        assert!(!recorder.is_open());
        mic.borrow_mut().broken = None;
        recorder.start(None).unwrap();
        assert!(recorder.is_open());
    }

    #[test]
    fn what_is_encoded_plays_back_as_it_was_recorded() {
        let (mut recorder, mic) = recorder(48_000, 1);
        recorder.start(None).unwrap();
        mic.borrow_mut().hear(440., 2.3);
        let take = recorder.stop();
        let ogg = take.encode().unwrap();
        assert_eq!(&ogg[..4], b"OggS");
        // About 24 kbit/s: 2.3 s are some 7 kB, not the 74 kB they were.
        assert!((3_000..12_000).contains(&ogg.len()), "{} bytes", ogg.len());

        // The player's own decoder (another implementation of Opus) reads
        // it: the same length, the same tone.
        let clip = crate::audio::decode(ogg.clone(), Some(MIME)).unwrap();
        assert_eq!((clip.rate, clip.channels), (48_000, 1));
        let seconds = clip.duration().as_secs_f32();
        assert!((seconds - 2.3).abs() < 0.03, "{seconds} s");
        let heard: Vec<f32> = clip.samples[24_000..72_000]
            .iter()
            .map(|sample| f32::from(*sample) / 32_768.)
            .collect();
        let found = pitch(&heard, 48_000);
        assert!((found - 440.).abs() <= 4., "{found} Hz");
        let loudest = heard.iter().fold(0f32, |a, b| a.max(b.abs()));
        assert!((0.35..0.65).contains(&loudest), "{loudest}");
        // The same take is the same file.
        assert_eq!(take.encode().unwrap(), ogg);
        // And the take plays here before it is sent.
        assert_eq!(take.clip().duration(), take.duration());
    }

    #[test]
    fn the_container_is_what_a_voice_note_is() {
        let take = Take {
            samples: vec![0; RATE as usize * 3],
        };
        let ogg = take.encode().unwrap();
        // Pages: the head, the tags, then a page a second.
        let mut pages = Vec::new();
        let mut at = 0;
        while at < ogg.len() {
            assert_eq!(&ogg[at..at + 4], b"OggS", "a page at {at}");
            let segments = ogg[at + 26] as usize;
            let body: usize = ogg[at + 27..at + 27 + segments]
                .iter()
                .map(|length| *length as usize)
                .sum();
            let end = at + 27 + segments + body;
            // Its checksum is right.
            let mut page = ogg[at..end].to_vec();
            let stored = u32::from_le_bytes(page[22..26].try_into().unwrap());
            page[22..26].fill(0);
            assert_eq!(ogg_crc(&page), stored);
            let granule = u64::from_le_bytes(ogg[at + 6..at + 14].try_into().unwrap());
            pages.push((ogg[at + 5], granule, at + 27 + segments));
            at = end;
        }
        assert!(pages.len() >= 5, "{} pages", pages.len());
        // The first begins the stream and holds the head: one channel,
        // recorded at 16 kHz.
        let (flags, _, body) = pages[0];
        assert_eq!(flags, 0x02);
        assert_eq!(&ogg[body..body + 8], b"OpusHead");
        assert_eq!(ogg[body + 9], 1);
        let pre_skip = u16::from_le_bytes([ogg[body + 10], ogg[body + 11]]) as u64;
        assert!(pre_skip > 0);
        assert_eq!(
            u32::from_le_bytes(ogg[body + 12..body + 16].try_into().unwrap()),
            RATE
        );
        assert_eq!(&ogg[pages[1].2..pages[1].2 + 8], b"OpusTags");
        // The last ends it, at the end of the sound: three seconds of
        // 48 kHz after what is skipped.
        let (flags, granule, _) = *pages.last().unwrap();
        assert_eq!(flags, 0x04);
        assert_eq!(granule, pre_skip + 3 * 48_000);
        // Granules never go back.
        assert!(pages.windows(2).all(|pair| pair[0].1 <= pair[1].1));
    }
}
