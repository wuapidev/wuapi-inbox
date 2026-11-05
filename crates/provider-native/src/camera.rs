//! The camera outside a call: what it sees, picture after picture, so that
//! one can be taken and sent as a photo. It is the same `ffmpeg` and the
//! same camera a call with video uses ([`crate::call_video`]), writing
//! plain pixels of a size fixed beforehand instead of video to send.

use crate::call_video;
use client_provider::{CallPicture, CallScreen};
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

/// The size of what the camera shows: what the call's camera is asked
/// for, so that a camera that films a call also takes pictures.
#[cfg(target_os = "macos")]
pub const SIZE: (u32, u32) = (1280, 720);
/// See above.
#[cfg(not(target_os = "macos"))]
pub const SIZE: (u32, u32) = (640, 480);
/// How many pictures a second are shown.
const SHOWN_FPS: u32 = 15;

/// Whether there is a camera to take pictures with here: `ffmpeg` was
/// found, without waiting for the answer.
pub fn available_now() -> bool {
    call_video::available_now()
}

/// The camera while it shows what it sees. Dropping it lets the camera
/// go.
pub struct Viewfinder {
    screen: CallScreen,
    child: Arc<Mutex<Child>>,
    problem: Arc<Mutex<Option<String>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Viewfinder {
    /// Starts `camera` (a [`client_provider::CallCamera::id`]), or the
    /// first there is. Answers at once: the pictures show on
    /// [`Viewfinder::screen`] once the camera gives them, and what went
    /// wrong, if it gives none, in [`Viewfinder::problem`].
    pub fn open(camera: Option<String>) -> Result<Self, String> {
        let (width, height) = SIZE;
        let filter = format!(
            "scale={width}:{height}:force_original_aspect_ratio=decrease,\
             pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,fps={SHOWN_FPS}"
        );
        let mut child = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error"])
            .args(call_video::camera_args(camera)?)
            .args(["-an", "-vf", &filter, "-f", "rawvideo", "-pix_fmt", "bgra"])
            .arg("pipe:1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("The camera could not be started ({error})."))?;
        let complaint = call_video::complaints(&mut child, "camera");
        let mut stdout = child.stdout.take().ok_or("The camera could not be read.")?;
        let child = Arc::new(Mutex::new(child));
        let screen = CallScreen::default();
        let problem = Arc::new(Mutex::new(None));
        let shown = screen.clone();
        let said = problem.clone();
        let filming = child.clone();
        std::thread::Builder::new()
            .name("camera-viewfinder".into())
            .spawn(move || {
                let mut pictures = 0u64;
                loop {
                    let mut bgra = vec![0u8; (width * height * 4) as usize];
                    if stdout.read_exact(&mut bgra).is_err() {
                        break;
                    }
                    pictures += 1;
                    shown.show(Some(Arc::new(CallPicture {
                        width,
                        height,
                        bgra,
                    })));
                }
                if pictures == 0 {
                    let reason = lock(&complaint).clone();
                    tracing::warn!(?reason, "the camera gave no picture");
                    *lock(&said) = Some(match reason {
                        Some(reason) => format!(
                            "The camera cannot be used: it may be in use, or not allowed ({reason})."
                        ),
                        None => "The camera gave no picture.".into(),
                    });
                }
                call_video::end(&mut lock(&filming));
            })
            .map_err(|error| format!("The camera could not be started ({error})."))?;
        Ok(Self {
            screen,
            child,
            problem,
        })
    }

    /// What the camera sees, newest first.
    pub fn screen(&self) -> &CallScreen {
        &self.screen
    }

    /// Why the camera showed nothing, once it is known.
    pub fn problem(&self) -> Option<String> {
        lock(&self.problem).clone()
    }
}

impl Drop for Viewfinder {
    fn drop(&mut self) {
        call_video::end(&mut lock(&self.child));
    }
}
