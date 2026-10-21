//! A call's video: the machine's camera on one side, the protocol
//! library's channels on the other, and the pictures the window draws.
//!
//! The library carries H.264 that is already encoded and hands back what
//! the other side sent the same way: it never touches a picture. Encoding
//! and decoding are done by `ffmpeg`, run as a program beside this one, as
//! the library's own example does. Where there is no `ffmpeg`, there is no
//! video ([`available`]) and calls are voice only.
//!
//! The camera is one `ffmpeg` that writes access units; each side's
//! pictures come out of another one that reads access units and writes
//! plain pixels of a size fixed beforehand, so that they can be cut apart
//! by counting bytes. The account's own picture is decoded from what is
//! sent: it shows what the other side sees.

use client_provider::{CallCamera, CallPicture, CallScreen, CallVideo, ContactId};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use whatsapp_rust::async_channel::{self, Receiver, Sender, TrySendError};
use whatsapp_rust::voip::VideoFrame;
use whatsapp_rust::wacore::voip::h264::{
    au_has_idr, nal_unit_type, split_annexb, AnnexBAuSplitter,
};

/// What the camera is sent as. Fifteen pictures a second is the pace the
/// library stamps them at when it is not told another.
pub const VIDEO_WIDTH: u32 = 640;
/// See [`VIDEO_WIDTH`].
pub const VIDEO_HEIGHT: u32 = 360;
/// See [`VIDEO_WIDTH`].
pub const VIDEO_FPS: u32 = 15;
const VIDEO_KBPS: u32 = 700;
/// A picture that can be started from comes this often: what the other
/// side waits for at most after it lost some.
const KEYFRAME_SECONDS: u32 = 2;
/// The other side's pictures are fitted into a square of this side: a
/// phone is held upright or sideways, and turns during a call.
const REMOTE_SIDE: u32 = 480;
/// The size of the account's own small picture.
const PREVIEW: (u32, u32) = (160, 90);
/// How many of the camera's access units may wait before they are
/// dropped: video that is late is worse than video that is missing.
const WAITING_UNITS: usize = 4;
/// How many access units may wait to be decoded. One that is dropped
/// breaks every picture until the next that can be started from, so
/// there is room for a burst.
const WAITING_TO_DECODE: usize = 64;
/// How many parameter sets of a stream are remembered.
const KEPT_SETS: usize = 16;

/// How long somebody in a call with several may send no video before
/// their place in the picture is given up.
const GUEST_QUIET: Duration = Duration::from_secs(4);

/// How often the other side is asked for a first picture until one is
/// there.
const ASK_EVERY: Duration = Duration::from_millis(250);

/// Called when the pictures of the other side cannot be decoded: the
/// call asks them for one that can be started from.
type Stuck = Arc<dyn Fn() + Send + Sync>;
/// How long a camera has to give its first picture.
const CAMERA_READY: Duration = Duration::from_secs(6);

/// Whether `ffmpeg` was found, once it has been looked for.
static FOUND: OnceLock<bool> = OnceLock::new();

/// Whether video can be carried here at all: `ffmpeg` was found. Asked
/// once, and waited for: starting `ffmpeg` to ask takes tens of
/// milliseconds.
pub fn available() -> bool {
    // Only where a camera can be opened: see `Input::args`.
    cfg!(any(target_os = "linux", target_os = "macos"))
        && *FOUND.get_or_init(|| {
            Command::new("ffmpeg")
                .arg("-version")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|status| status.success())
        })
}

/// [`available`] without waiting: false until the answer is in. What is
/// drawn asks again at every frame, so the answer shows when it comes.
pub fn available_now() -> bool {
    cfg!(any(target_os = "linux", target_os = "macos")) && FOUND.get().copied().unwrap_or(false)
}

/// Looks for `ffmpeg` on a thread of its own, so that the answer is there
/// before anybody asks and nobody waits for it.
pub fn look_ahead() {
    if FOUND.get().is_none() {
        let _ = std::thread::Builder::new()
            .name("ffmpeg-check".into())
            .spawn(|| {
                available();
            });
    }
}

/// The cameras of this machine. On Linux, the capture nodes of
/// `/sys/class/video4linux`, one for each name: a camera shows up as
/// several nodes, of which the first takes pictures. Elsewhere the list is
/// empty and the system's first camera is used.
pub fn cameras() -> Vec<CallCamera> {
    #[cfg(target_os = "linux")]
    {
        let Ok(nodes) = std::fs::read_dir("/sys/class/video4linux") else {
            return Vec::new();
        };
        let found = nodes.flatten().filter_map(|node| {
            let file = node.file_name().into_string().ok()?;
            let number: u32 = file.strip_prefix("video")?.parse().ok()?;
            let read = |what: &str| std::fs::read_to_string(node.path().join(what)).ok();
            // A camera's other nodes (its metadata) are not the first.
            let first = read("index").is_none_or(|index| index.trim() == "0");
            first.then(|| (number, read("name").unwrap_or_default().trim().to_owned()))
        });
        // The raw nodes of a laptop's image processor are not cameras
        // that can be opened like one: they need a pipeline of their own.
        let found = found.filter(|(_, name)| !name.contains("ISYS Capture"));
        one_for_each_name(found.collect())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

/// The nodes in the order of their numbers, the first of each name.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn one_for_each_name(mut nodes: Vec<(u32, String)>) -> Vec<CallCamera> {
    nodes.sort();
    let mut cameras: Vec<CallCamera> = Vec::new();
    for (number, name) in nodes {
        let name = if name.is_empty() {
            format!("Camera {number}")
        } else {
            name
        };
        if !cameras.iter().any(|camera| camera.name == name) {
            cameras.push(CallCamera {
                id: format!("/dev/video{number}"),
                name,
            });
        }
    }
    cameras
}

/// What is filmed.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Input {
    /// A camera: the one chosen, or the first there is.
    Camera(Option<String>),
    /// A drawn pattern that moves, for the tests.
    #[cfg_attr(not(test), allow(dead_code))]
    Pattern,
}

impl Input {
    fn args(&self) -> Result<Vec<String>, String> {
        let words = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
        match self {
            Self::Pattern => Ok(words(&[
                "-re",
                "-f",
                "lavfi",
                "-i",
                &format!("testsrc2=size={VIDEO_WIDTH}x{VIDEO_HEIGHT}:rate={VIDEO_FPS}"),
            ])),
            Self::Camera(chosen) => {
                if cfg!(target_os = "linux") {
                    let device = chosen
                        .clone()
                        .filter(|device| std::path::Path::new(device).exists())
                        .or_else(|| cameras().into_iter().next().map(|camera| camera.id))
                        .ok_or("No camera was found.")?;
                    Ok(words(&[
                        "-f",
                        "v4l2",
                        "-framerate",
                        "30",
                        "-video_size",
                        "640x480",
                        "-i",
                        &device,
                    ]))
                } else if cfg!(target_os = "macos") {
                    let device = chosen.clone().unwrap_or_else(|| "0".into());
                    Ok(words(&[
                        "-f",
                        "avfoundation",
                        "-framerate",
                        "30",
                        "-video_size",
                        "1280x720",
                        "-i",
                        &format!("{device}:none"),
                    ]))
                } else {
                    Err("Video calls are not available on this system yet.".into())
                }
            }
        }
    }
}

/// How the camera is encoded: what WhatsApp's own clients send, smaller.
/// Every key picture carries its parameters again, and every access unit
/// starts with a delimiter, which is what they are cut apart by.
fn encoder_args() -> Vec<String> {
    let gop = (VIDEO_FPS * KEYFRAME_SECONDS).to_string();
    let rate = format!("{VIDEO_KBPS}k");
    let filter = format!(
        "scale={VIDEO_WIDTH}:{VIDEO_HEIGHT}:force_original_aspect_ratio=increase:out_range=tv,\
         crop={VIDEO_WIDTH}:{VIDEO_HEIGHT},setsar=1,fps={VIDEO_FPS},format=yuv420p"
    );
    [
        "-vf",
        &filter,
        "-fps_mode",
        "cfr",
        "-c:v",
        "libx264",
        "-profile:v",
        "baseline",
        "-level:v",
        "3.1",
        "-pix_fmt",
        "yuv420p",
        "-preset",
        "veryfast",
        "-tune",
        "zerolatency",
        "-g",
        &gop,
        "-keyint_min",
        &gop,
        "-sc_threshold",
        "0",
        "-b:v",
        &rate,
        "-maxrate",
        &rate,
        "-bufsize",
        &format!("{}k", VIDEO_KBPS / 4),
        "-x264-params",
        "repeat-headers=1:sliced-threads=0:threads=1",
        "-bsf:v",
        "h264_metadata=aud=insert",
        "-an",
        "-f",
        "h264",
        "pipe:1",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// How a picture that was taken with the phone turned is set upright.
fn upright(orientation: u8) -> Option<&'static str> {
    match orientation & 0x03 {
        1 => Some("transpose=cclock"),
        2 => Some("hflip,vflip"),
        3 => Some("transpose=clock"),
        _ => None,
    }
}

fn decoder_args(size: (u32, u32), orientation: u8) -> Vec<String> {
    let (width, height) = size;
    let turned = upright(orientation)
        .map(|filter| format!("{filter},"))
        .unwrap_or_default();
    let filter = format!(
        "{turned}scale={width}:{height}:force_original_aspect_ratio=decrease,\
         pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1"
    );
    [
        "-hide_banner",
        "-loglevel",
        "error",
        // Measured: with these a picture is out 0.15 s after it went in.
        // (`-fflags nobuffer`, which reads as if it would help, holds
        // two seconds of pictures back for as long as the call lasts.)
        "-flags",
        "low_delay",
        "-probesize",
        "32",
        "-analyzeduration",
        "0",
        "-f",
        "h264",
        "-i",
        "pipe:0",
        "-vf",
        &filter,
        "-f",
        "rawvideo",
        "-pix_fmt",
        "bgra",
        "pipe:1",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn stop(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// What `ffmpeg` complains about goes to the log, and the last line is
/// kept: it is the reason when the camera does not start.
fn hear_complaints(
    child: &mut Child,
    what: &'static str,
    stuck: Option<Stuck>,
) -> Arc<Mutex<Option<String>>> {
    let last = Arc::new(Mutex::new(None));
    if let Some(stderr) = child.stderr.take() {
        let kept = last.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                tracing::debug!("{what}: {line}");
                *lock(&kept) = Some(line);
                // A decoder only complains about what it could not
                // decode.
                if let Some(stuck) = &stuck {
                    stuck();
                }
            }
        });
    }
    last
}

/// The parameter sets a stream has sent. A phone sends them when it
/// starts and when it changes size, not with every key picture, and a
/// decoder that starts later needs them all the same.
#[derive(Default)]
struct ParameterSets(Vec<Vec<u8>>);

impl ParameterSets {
    /// Remembers the sets of an access unit.
    fn learn(&mut self, unit: &[u8]) {
        for nal in split_annexb(unit) {
            if !matches!(nal_unit_type(nal), 7 | 8) || self.0.iter().any(|set| set == nal) {
                continue;
            }
            if self.0.len() == KEPT_SETS {
                self.0.remove(0);
            }
            self.0.push(nal.to_vec());
        }
    }

    /// Every set, as a decoder reads them.
    fn written(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        for set in &self.0 {
            bytes.extend([0, 0, 0, 1]);
            bytes.extend(set);
        }
        bytes
    }
}

// ----- pictures out of access units ---------------------------------------

/// The `ffmpeg` that turns access units into pictures, as it runs now.
struct Decoding {
    child: Child,
    stdin: ChildStdin,
    orientation: u8,
}

/// One person's video as it is decoded: their decoder, the parameter
/// sets they sent, and the screen their pictures go to.
struct Stream {
    screen: CallScreen,
    running: Option<Decoding>,
    sets: ParameterSets,
    began: Instant,
    seen: u32,
    waited: u32,
    last: Instant,
}

impl Stream {
    fn new(screen: CallScreen) -> Self {
        Self {
            screen,
            running: None,
            sets: ParameterSets::default(),
            began: Instant::now(),
            seen: 0,
            waited: 0,
            last: Instant::now(),
        }
    }

    /// Takes one access unit. The decoder is started at a picture that
    /// can be started from, after the parameter sets the stream has sent,
    /// and again whenever it dies or the pictures are turned another way.
    fn take(&mut self, unit: &VideoFrame, size: (u32, u32), stuck: &Option<Stuck>) {
        self.last = Instant::now();
        self.sets.learn(&unit.data);
        let starts = au_has_idr(&unit.data);
        self.seen += 1;
        if self.seen == 1 {
            tracing::info!(
                after = ?self.began.elapsed(),
                width = size.0,
                starts,
                "the first access unit of a video arrived"
            );
        }
        if starts && self.running.is_none() {
            tracing::info!(
                after = ?self.began.elapsed(),
                width = size.0,
                waited = self.waited,
                "a video can be started from: decoding"
            );
        }
        let turned = self
            .running
            .as_ref()
            .is_some_and(|decoding| decoding.orientation != unit.orientation);
        if turned && starts {
            self.stop();
        }
        if self.running.is_none() {
            if !starts {
                self.waited += 1;
                tracing::info!(
                    after = ?self.began.elapsed(),
                    bytes = unit.data.len(),
                    sets = unit.keyframe,
                    "an access unit that cannot be started from"
                );
                if let Some(stuck) = stuck {
                    stuck();
                }
                return;
            }
            self.running = start_decoder(&self.screen, size, unit.orientation, stuck.clone());
            if let Some(decoding) = self.running.as_mut() {
                let _ = decoding.stdin.write_all(&self.sets.written());
            }
        }
        let Some(decoding) = self.running.as_mut() else {
            return;
        };
        if decoding.stdin.write_all(&unit.data).is_err() {
            // It died: the next key picture starts another.
            self.stop();
        }
    }

    fn stop(&mut self) {
        if let Some(mut old) = self.running.take() {
            stop(&mut old.child);
        }
    }
}

/// Where the pictures of what is decoded go.
enum Screens {
    /// One stream, one screen: the account's own camera.
    One(CallScreen),
    /// The other side. In a call between two there is one stream; in a
    /// call with several each access unit says whose it is, and each
    /// person has a decoder and a screen of their own.
    Call(CallVideo),
}

/// Decodes the access units of `units` until the channel closes. `stuck`
/// is told when there is nothing a decoder can start from.
fn decode(units: Receiver<VideoFrame>, screens: Screens, size: (u32, u32), stuck: Option<Stuck>) {
    let spawned = std::thread::Builder::new()
        .name("call-video-decode".into())
        .spawn(move || {
            // By whose video it is; `None` is the one of a call between
            // two.
            let mut streams: HashMap<Option<ContactId>, Stream> = HashMap::new();
            // Until the first access unit is here, the other side is
            // asked for a picture to start from, again and again. Their
            // first one is easily lost while the call is set up, nothing
            // that follows it can be put together without it, and they
            // send another only when asked (or many seconds later).
            let unit_or_wait = |any: bool| loop {
                match units.try_recv() {
                    Ok(unit) => return Some(Some(unit)),
                    Err(async_channel::TryRecvError::Closed) => return None,
                    Err(async_channel::TryRecvError::Empty) if !any => {
                        if let Some(stuck) = &stuck {
                            stuck();
                        }
                        std::thread::sleep(ASK_EVERY);
                    }
                    // Nothing for a moment: time to see who went quiet.
                    Err(async_channel::TryRecvError::Empty) => {
                        std::thread::sleep(Duration::from_millis(5));
                        return Some(None);
                    }
                }
            };
            let mut swept = Instant::now();
            while let Some(unit) = unit_or_wait(!streams.is_empty() || stuck.is_none()) {
                if let Some(unit) = unit {
                    let who = unit
                        .sender
                        .as_ref()
                        .map(|jid| ContactId::new(crate::ids::id_of(jid)));
                    let stream = streams.entry(who.clone()).or_insert_with(|| {
                        Stream::new(match (&screens, &who) {
                            (Screens::One(screen), _) => screen.clone(),
                            (Screens::Call(video), None) => video.remote.clone(),
                            (Screens::Call(video), Some(who)) => video.guests.screen_of(who),
                        })
                    });
                    stream.take(&unit, size, &stuck);
                }
                // Somebody in a call with several whose video stopped
                // coming (they left, or turned their camera off) gives
                // their place up.
                if swept.elapsed() > Duration::from_secs(1) {
                    swept = Instant::now();
                    streams.retain(|who, stream| {
                        let quiet = stream.last.elapsed() > GUEST_QUIET;
                        match (who, &screens) {
                            (Some(who), Screens::Call(video)) if quiet => {
                                stream.stop();
                                video.guests.remove(who);
                                false
                            }
                            _ => true,
                        }
                    });
                }
            }
            for (who, mut stream) in streams {
                stream.stop();
                stream.screen.show(None);
                if let (Some(who), Screens::Call(video)) = (who, &screens) {
                    video.guests.remove(&who);
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "a call's video could not be decoded");
    }
}

fn start_decoder(
    screen: &CallScreen,
    size: (u32, u32),
    orientation: u8,
    stuck: Option<Stuck>,
) -> Option<Decoding> {
    let mut child = Command::new("ffmpeg")
        .args(decoder_args(size, orientation))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| tracing::warn!(%error, "the video decoder could not start"))
        .ok()?;
    hear_complaints(&mut child, "video decoder", stuck);
    let stdin = child.stdin.take()?;
    let mut stdout = child.stdout.take()?;
    let screen = screen.clone();
    let (width, height) = size;
    std::thread::spawn(move || {
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let started = Instant::now();
        let mut first = true;
        while stdout.read_exact(&mut pixels).is_ok() {
            if std::mem::take(&mut first) {
                tracing::info!(after = ?started.elapsed(), width, "a video decoder gave its first picture");
            }
            screen.show(Some(Arc::new(CallPicture {
                width,
                height,
                bgra: pixels.clone(),
            })));
        }
    });
    Some(Decoding {
        child,
        stdin,
        orientation,
    })
}

// ----- access units out of the camera -------------------------------------

/// The camera while it films. Dropping it lets the camera go.
struct Filming {
    child: Arc<Mutex<Child>>,
}

impl Drop for Filming {
    fn drop(&mut self) {
        stop(&mut lock(&self.child));
    }
}

/// Starts filming `input`. Answers once the first picture is there, with
/// the channel the access units come on.
fn film(
    input: &Input,
    preview: Sender<VideoFrame>,
    wait: bool,
) -> Result<(Filming, Receiver<Vec<u8>>), String> {
    let asked = Instant::now();
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .args(input.args()?)
        .args(encoder_args())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("The camera could not be started ({error})."))?;
    let complaint = hear_complaints(&mut child, "camera", None);
    let mut stdout = child.stdout.take().ok_or("The camera could not be read.")?;
    let child = Arc::new(Mutex::new(child));
    let (units, camera) = async_channel::bounded::<Vec<u8>>(WAITING_UNITS);
    let (ready, started) = std::sync::mpsc::channel::<()>();
    let filming = child.clone();
    let heard = complaint.clone();
    std::thread::Builder::new()
        .name("call-video-camera".into())
        .spawn(move || {
            let mut splitter = AnnexBAuSplitter::default();
            let mut chunk = vec![0u8; 32 * 1024];
            let mut cut = Vec::new();
            // Nothing is sent before a picture that can be started from.
            let mut open = false;
            'filming: while let Ok(read) = stdout.read(&mut chunk) {
                if read == 0 {
                    break;
                }
                splitter.push(&chunk[..read], &mut cut);
                for unit in cut.drain(..) {
                    if !open && !au_has_idr(&unit) {
                        continue;
                    }
                    if !open {
                        open = true;
                        tracing::info!(after = ?asked.elapsed(), "the camera gave its first picture");
                        let _ = ready.send(());
                    }
                    let _ = preview.try_send(VideoFrame::new(unit.clone()));
                    // A call that is not taking pictures loses them; one
                    // that is over ends the filming.
                    if let Err(TrySendError::Closed(_)) = units.try_send(unit) {
                        break 'filming;
                    }
                }
            }
            if !open {
                let reason = lock(&heard).clone().unwrap_or_default();
                tracing::warn!(%reason, "the camera gave no picture");
            }
            stop(&mut lock(&filming));
        })
        .map_err(|error| format!("The camera could not be started ({error})."))?;
    let filming = Filming { child };
    if !wait {
        // The pictures come on the channel when the camera is ready.
        return Ok((filming, camera));
    }
    let until = Instant::now() + CAMERA_READY;
    loop {
        match started.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => return Ok((filming, camera)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) if Instant::now() < until => {}
            Err(_) => {
                let reason = lock(&complaint).clone();
                return Err(match reason {
                    Some(reason) => format!(
                        "The camera cannot be used: it may be in use, or not allowed ({reason})."
                    ),
                    None => "The camera gave no picture.".into(),
                });
            }
        }
    }
}

// ----- one call's video ----------------------------------------------------

/// The video of one call. Dropping it lets the camera go and ends the
/// programs that encode and decode.
pub struct VideoLink {
    /// Where the library puts what the other side shows.
    pub screen: Sender<VideoFrame>,
    /// The pictures of both sides, for the window.
    pub pictures: CallVideo,
    input: Input,
    preview: Sender<VideoFrame>,
    filming: Mutex<Option<Filming>>,
    stuck: Arc<Mutex<Option<Stuck>>>,
}

impl VideoLink {
    /// What to do when the other side's pictures cannot be decoded: ask
    /// them for one that can be started from. Called often while it
    /// lasts.
    pub fn when_stuck(&self, ask: impl Fn() + Send + Sync + 'static) {
        *lock(&self.stuck) = Some(Arc::new(ask));
    }

    /// Starts the camera. Answers the channel its encoded pictures come
    /// on, for the library to send: a new one each time. With `wait`, once
    /// the first picture is there, which is how it is known that the
    /// camera works; without, at once.
    pub fn start_camera(&self, wait: bool) -> Result<Receiver<Vec<u8>>, String> {
        let (filming, camera) = film(&self.input, self.preview.clone(), wait)?;
        *lock(&self.filming) = Some(filming);
        Ok(camera)
    }

    /// Lets the camera go. The other side's pictures keep coming.
    pub fn stop_camera(&self) {
        lock(&self.filming).take();
        self.pictures.local.show(None);
    }
}

impl Drop for VideoLink {
    fn drop(&mut self) {
        lock(&self.filming).take();
        // The decoders end with their channels.
        self.screen.close();
        self.preview.close();
    }
}

/// Opens the video of a call on the camera chosen (`None`: the first one
/// there is). Answers it with the channel the camera's pictures come on.
///
/// With `wait` it answers once the camera gave a picture: a call that is
/// answered says it has video, so it should have some. A call that is
/// placed does not wait: the camera starts while it rings.
pub fn open(camera: Option<&str>, wait: bool) -> Result<(VideoLink, Receiver<Vec<u8>>), String> {
    open_on(Input::Camera(camera.map(str::to_owned)), wait)
}

fn open_on(input: Input, wait: bool) -> Result<(VideoLink, Receiver<Vec<u8>>), String> {
    if !available() {
        return Err("Video calls need ffmpeg, which was not found on this computer.".into());
    }
    let pictures = CallVideo::default();
    let (screen, shown) = async_channel::bounded::<VideoFrame>(WAITING_TO_DECODE);
    let (preview, previewed) = async_channel::bounded::<VideoFrame>(WAITING_UNITS);
    let stuck: Arc<Mutex<Option<Stuck>>> = Arc::default();
    let ask = stuck.clone();
    let asks: Stuck = Arc::new(move || {
        let ask = lock(&ask).clone();
        if let Some(ask) = ask {
            ask();
        }
    });
    decode(
        shown,
        Screens::Call(pictures.clone()),
        (REMOTE_SIDE, REMOTE_SIDE),
        Some(asks),
    );
    decode(
        previewed,
        Screens::One(pictures.local.clone()),
        PREVIEW,
        None,
    );
    let link = VideoLink {
        screen,
        pictures,
        input,
        preview,
        filming: Mutex::new(None),
        stuck,
    };
    let camera = link.start_camera(wait)?;
    Ok((link, camera))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for<T>(mut look: impl FnMut() -> Option<T>) -> Option<T> {
        let until = Instant::now() + Duration::from_secs(15);
        while Instant::now() < until {
            if let Some(found) = look() {
                return Some(found);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    #[test]
    fn a_camera_is_listed_once_under_its_first_node() {
        let cameras = one_for_each_name(vec![
            (3, "USB cam".into()),
            (0, "Integrated".into()),
            (2, "USB cam".into()),
            (10, String::new()),
        ]);
        let listed: Vec<(&str, &str)> = cameras
            .iter()
            .map(|camera| (camera.id.as_str(), camera.name.as_str()))
            .collect();
        assert_eq!(
            listed,
            [
                ("/dev/video0", "Integrated"),
                ("/dev/video2", "USB cam"),
                ("/dev/video10", "Camera 10"),
            ]
        );
    }

    #[test]
    fn a_turned_picture_is_set_upright() {
        assert_eq!(upright(0), None);
        assert!(decoder_args((480, 480), 1)
            .iter()
            .any(|arg| arg.starts_with("transpose=cclock,scale=480:480")));
        assert!(decoder_args((160, 90), 0)
            .iter()
            .any(|arg| arg.starts_with("scale=160:90")));
    }

    /// A stream whose parameter sets came once, before the picture the
    /// decoder starts from: as a phone sends them.
    #[test]
    fn a_decoder_that_starts_late_is_given_the_sets_it_missed() {
        let sps = [0x67, 1, 2, 3];
        let pps = [0x68, 7, 7];
        let idr = [0x65, 9, 9, 9];
        let unit = |nals: &[&[u8]]| {
            let mut bytes = Vec::new();
            for nal in nals {
                bytes.extend([0, 0, 0, 1]);
                bytes.extend(*nal);
            }
            bytes
        };
        let mut sets = ParameterSets::default();
        sets.learn(&unit(&[&sps, &pps]));
        // Said again: once is enough. A picture is not a set.
        sets.learn(&unit(&[&sps, &pps, &idr]));
        assert_eq!(sets.written(), unit(&[&sps, &pps]));
    }

    /// What is filmed comes out as access units that start at a key
    /// picture, and what comes in is drawn: the whole way, on a pattern
    /// instead of a camera.
    #[test]
    fn what_is_filmed_is_sent_and_what_arrives_is_drawn() {
        if !available() {
            eprintln!("skipped: no ffmpeg here");
            return;
        }
        let (link, camera) = open_on(Input::Pattern, true).expect("the pattern films");
        let first = camera.recv_blocking().expect("an access unit");
        assert!(au_has_idr(&first), "the first one can be started from");

        // The own picture is decoded from what is sent.
        let own = wait_for(|| link.pictures.local.newest().1).expect("the own picture");
        assert_eq!((own.width, own.height), PREVIEW);

        // What the other side sends: the same stream, handed back.
        link.screen
            .send_blocking(VideoFrame::new(first))
            .expect("the screen takes it");
        let theirs = wait_for(|| {
            while let Ok(unit) = camera.try_recv() {
                let _ = link.screen.try_send(VideoFrame::new(unit));
            }
            link.pictures.remote.newest().1
        })
        .expect("their picture");
        assert_eq!((theirs.width, theirs.height), (REMOTE_SIDE, REMOTE_SIDE));
        assert_eq!(theirs.bgra.len(), (REMOTE_SIDE * REMOTE_SIDE * 4) as usize);
        let first_pixel = &theirs.bgra[..4];
        assert!(
            theirs.bgra.chunks(4).any(|pixel| pixel != first_pixel),
            "a picture, not a blank"
        );

        // In a call with several, an access unit says whose it is: each
        // person gets a screen of their own.
        assert!(link.pictures.guests.all().is_empty());
        let ana: whatsapp_rust::Jid = "5491100000001@s.whatsapp.net".parse().unwrap();
        let from_ana = |unit: Vec<u8>| {
            let mut frame = VideoFrame::new(unit);
            frame.sender = Some(ana.clone());
            frame
        };
        let hers = wait_for(|| {
            while let Ok(unit) = camera.try_recv() {
                let _ = link.screen.try_send(from_ana(unit));
            }
            let guests = link.pictures.guests.all();
            guests.first().and_then(|(_, screen)| screen.newest().1)
        })
        .expect("a guest's picture");
        assert_eq!((hers.width, hers.height), (REMOTE_SIDE, REMOTE_SIDE));
        let guests = link.pictures.guests.all();
        assert_eq!(guests.len(), 1);
        assert_eq!(guests[0].0.as_str(), "+5491100000001");

        // The camera off: the own picture goes, theirs stays.
        link.stop_camera();
        assert!(link.pictures.local.newest().1.is_none());
        assert!(link.pictures.remote.newest().1.is_some());
        // And on again, on a new channel that starts at a key picture.
        // Without waiting for it: the pictures come when it is ready.
        let again = link.start_camera(false).expect("the camera again");
        assert!(au_has_idr(&again.recv_blocking().unwrap()));
    }
}
