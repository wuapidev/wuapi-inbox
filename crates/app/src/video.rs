//! Story video, played inside the application.
//!
//! The decoder is libmpv, LGPL-2.1-or-later, loaded at runtime with
//! `libloading`. This crate stays Apache-2.0: it never links libmpv, and
//! `WUAPI_LIBMPV_PATH` replaces the copy a release embedded (the LGPL
//! replacement path). The embedded archive is chosen at build time with
//! `WUAPI_VIDEO_BUNDLE` and must be one self-contained shared library
//! (its LGPL and BSD pieces linked statically, no GPL build). Frames are
//! software BGRA, capped on the long side, because GPUI has no GPU
//! context to hand to libmpv. That renderer is CPU-bound; the cap is the
//! bound on it.
//!
//! Tests install [`install_opener`] and never need the native library.

use std::ffi::{CStr, CString};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};

const BUNDLE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/video-bundle.tar.gz"));
/// The longest side a frame is drawn at. The software renderer is
/// single-threaded; a story card is not a cinema screen.
const MAX_SIDE: i32 = 640;

/// What the viewer paints, and where playback is.
#[derive(Clone)]
pub(crate) struct Report {
    pub image: Option<Arc<gpui_kit::RenderImage>>,
    pub width: u32,
    pub height: u32,
    pub position: Duration,
    pub finished: bool,
    pub failure: Option<String>,
}

struct Shared {
    image: Option<Arc<gpui_kit::RenderImage>>,
    stale: Vec<Arc<gpui_kit::RenderImage>>,
    width: u32,
    height: u32,
    position: Duration,
    duration: Duration,
    finished: bool,
    failure: Option<String>,
    /// Advances on [`Clip::poll`] instead of a decoder thread.
    clock: bool,
}

/// One playback. Dropping it stops the decoder; it does not wait for it.
pub(crate) struct Clip {
    shared: Arc<Mutex<Shared>>,
    paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Clip {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.join.take() {
            std::thread::spawn(move || {
                let _ = handle.join();
            });
        }
    }
}

impl Clip {
    /// Plays `bytes`. A test opener, when one is installed, is used
    /// instead of libmpv.
    pub(crate) fn open(bytes: Arc<[u8]>) -> Result<Self, String> {
        if let Some(open) = opener() {
            return open(bytes);
        }
        let path = resolve_library(
            std::env::var_os("WUAPI_LIBMPV_PATH").map(PathBuf::from),
            BUNDLE,
            &cache_root(),
        )?;
        native::start(&path, bytes)
    }

    /// Holds or continues. A clock clip stops advancing; libmpv pauses.
    pub(crate) fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
    }

    /// Moves a clock clip forward by `by`. A libmpv clip keeps its own time.
    pub(crate) fn poll(&self, by: Duration) {
        let mut shared = lock(&self.shared);
        if !shared.clock || self.paused.load(Ordering::Relaxed) || shared.finished {
            return;
        }
        shared.position = shared.position.saturating_add(by);
        if shared.position >= shared.duration {
            shared.position = shared.duration;
            shared.finished = true;
        }
    }

    pub(crate) fn report(&self) -> Report {
        let shared = lock(&self.shared);
        Report {
            image: shared.image.clone(),
            width: shared.width,
            height: shared.height,
            position: shared.position,
            finished: shared.finished,
            failure: shared.failure.clone(),
        }
    }

    /// Frames the renderer has replaced. The window must drop them.
    pub(crate) fn take_stale(&self) -> Vec<Arc<gpui_kit::RenderImage>> {
        std::mem::take(&mut lock(&self.shared).stale)
    }
}

/// A clip whose clock is [`Clip::poll`] and whose picture is one solid frame.
pub(crate) fn clock(duration: Duration) -> Clip {
    let image = solid(2, 2, [0x10, 0x20, 0x30, 0xFF]);
    Clip {
        shared: Arc::new(Mutex::new(Shared {
            image: Some(image),
            stale: Vec::new(),
            width: 2,
            height: 2,
            position: Duration::ZERO,
            duration,
            finished: false,
            failure: None,
            clock: true,
        })),
        paused: Arc::new(AtomicBool::new(false)),
        stop: Arc::new(AtomicBool::new(false)),
        join: None,
    }
}

thread_local! {
    static OPENER: std::cell::RefCell<
        Option<Arc<dyn Fn(Arc<[u8]>) -> Result<Clip, String> + Send + Sync>>,
    > = std::cell::RefCell::new(None);
}

/// Installs the opener [`Clip::open`] uses on this thread, until the
/// guard is dropped.
pub(crate) fn install_opener(
    open: Arc<dyn Fn(Arc<[u8]>) -> Result<Clip, String> + Send + Sync>,
) -> OpenerGuard {
    OPENER.with(|slot| *slot.borrow_mut() = Some(open));
    OpenerGuard
}

pub(crate) struct OpenerGuard;

impl Drop for OpenerGuard {
    fn drop(&mut self) {
        OPENER.with(|slot| *slot.borrow_mut() = None);
    }
}

fn opener() -> Option<Arc<dyn Fn(Arc<[u8]>) -> Result<Clip, String> + Send + Sync>> {
    OPENER.with(|slot| slot.borrow().clone())
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn solid(width: u32, height: u32, bgra: [u8; 4]) -> Arc<gpui_kit::RenderImage> {
    let mut bytes = vec![0u8; (width * height * 4) as usize];
    for pixel in bytes.chunks_exact_mut(4) {
        pixel.copy_from_slice(&bgra);
    }
    let image = image::RgbaImage::from_raw(width, height, bytes).expect("the frame's size matches");
    Arc::new(gpui_kit::RenderImage::new(vec![image::Frame::new(image)]))
}

fn cache_root() -> PathBuf {
    dirs::cache_dir()
        .or_else(dirs::data_dir)
        .unwrap_or_else(std::env::temp_dir)
        .join("wuapi-inbox")
        .join("video")
}

/// The shared library to load: `WUAPI_LIBMPV_PATH` when it is set, otherwise
/// the embedded archive extracted under `cache`.
fn resolve_library(env: Option<PathBuf>, bundle: &[u8], cache: &Path) -> Result<PathBuf, String> {
    if let Some(path) = env {
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!(
            "WUAPI_LIBMPV_PATH is not a file ({})",
            path.display()
        ));
    }
    if bundle.is_empty() {
        return Err("no video runtime was embedded".into());
    }
    let dir = cache.join(digest(bundle));
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    let archive = flate2::read::GzDecoder::new(bundle);
    let mut tar = tar::Archive::new(archive);
    let entries = tar.entries().map_err(|error| error.to_string())?;
    let mut library = None;
    for entry in entries {
        let mut entry = entry.map_err(|error| error.to_string())?;
        let path = entry
            .path()
            .map_err(|error| error.to_string())?
            .into_owned();
        let Some(relative) = safe_relative(&path) else {
            return Err(format!(
                "the video runtime archive has an unsafe path ({path:?})"
            ));
        };
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let dest = dir.join(&relative);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut file = std::fs::File::create(&dest).map_err(|error| error.to_string())?;
        std::io::copy(&mut entry, &mut file).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644));
        }
        if is_library(relative.as_path()) {
            library = Some(dest);
        }
    }
    library.ok_or_else(|| "the video runtime archive has no libmpv".into())
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut out, byte| {
            out.push_str(&format!("{byte:02x}"));
            out
        })
}

fn safe_relative(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        return None;
    }
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

fn is_library(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    #[cfg(target_os = "windows")]
    {
        let name = name.to_ascii_lowercase();
        return name == "libmpv-2.dll" || name == "libmpv.dll" || name == "mpv-2.dll";
    }
    #[cfg(target_os = "macos")]
    {
        return name == "libmpv.dylib" || (name.starts_with("libmpv") && name.ends_with(".dylib"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        name == "libmpv.so" || name.starts_with("libmpv.so.")
    }
}

/// Even size that fits in [`MAX_SIDE`] on the long edge.
fn fitted_even(width: i32, height: i32) -> (i32, i32) {
    if width <= 0 || height <= 0 {
        return (2, 2);
    }
    let long = width.max(height);
    let scale = if long > MAX_SIDE {
        MAX_SIDE as f32 / long as f32
    } else {
        1.
    };
    let even = |value: f32| ((value * scale).round() as i32).max(2) & !1;
    (even(width as f32), even(height as f32))
}

/// libmpv's `bgr0` leaves the fourth byte uninitialised. The renderer
/// wants BGRA with a solid alpha.
fn bgr0_to_bgra(buf: &mut [u8], width: usize, stride: usize) {
    if width == 0 || stride < width * 4 || buf.len() < stride {
        return;
    }
    let height = buf.len() / stride;
    for y in 0..height {
        let row = y * stride;
        for x in 0..width {
            buf[row + x * 4 + 3] = 255;
        }
    }
}

fn pack_rows(buf: &[u8], width: usize, height: usize, stride: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        let start = y * stride;
        let end = start + width * 4;
        if end > buf.len() {
            break;
        }
        out.extend_from_slice(&buf[start..end]);
    }
    out
}

fn stride_for(width: i32) -> usize {
    let raw = width.max(1) as usize * 4;
    raw.div_ceil(64) * 64
}

mod native {
    use super::*;

    type Create = unsafe extern "C" fn() -> *mut std::ffi::c_void;
    type Init = unsafe extern "C" fn(*mut std::ffi::c_void) -> std::ffi::c_int;
    type Destroy = unsafe extern "C" fn(*mut std::ffi::c_void);
    type SetString = unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *const std::ffi::c_char,
        *const std::ffi::c_char,
    ) -> std::ffi::c_int;
    type Command = unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *const *const std::ffi::c_char,
    ) -> std::ffi::c_int;
    type WaitEvent = unsafe extern "C" fn(*mut std::ffi::c_void, f64) -> *mut std::ffi::c_void;
    type GetString = unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *const std::ffi::c_char,
    ) -> *mut std::ffi::c_char;
    type Free = unsafe extern "C" fn(*mut std::ffi::c_void);
    type Version = unsafe extern "C" fn() -> std::os::raw::c_ulong;
    type AddStream = unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *const std::ffi::c_char,
        *mut std::ffi::c_void,
        OpenFn,
    ) -> std::ffi::c_int;
    type RenderCreate = unsafe extern "C" fn(
        *mut *mut std::ffi::c_void,
        *mut std::ffi::c_void,
        *mut RenderParam,
    ) -> std::ffi::c_int;
    type Render = unsafe extern "C" fn(*mut std::ffi::c_void, *mut RenderParam) -> std::ffi::c_int;
    type RenderFree = unsafe extern "C" fn(*mut std::ffi::c_void);

    type OpenFn = unsafe extern "C" fn(
        *mut std::ffi::c_void,
        *mut std::ffi::c_char,
        *mut std::ffi::c_void,
    ) -> std::ffi::c_int;

    #[repr(C)]
    struct RenderParam {
        kind: std::ffi::c_int,
        data: *mut std::ffi::c_void,
    }

    struct Api {
        _library: libloading::Library,
        version: std::os::raw::c_ulong,
        create: Create,
        initialize: Init,
        destroy: Destroy,
        set_option: SetString,
        set_property: SetString,
        command: Command,
        wait_event: WaitEvent,
        get_property: GetString,
        free: Free,
        add_stream: AddStream,
        render_create: RenderCreate,
        render: Render,
        render_free: RenderFree,
    }

    // One process loads one library. A later path needs a new process.
    fn api(path: &Path) -> Result<Arc<Api>, String> {
        static LOADED: Mutex<Option<Arc<Api>>> = Mutex::new(None);
        let mut slot = LOADED
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(api) = slot.clone() {
            return Ok(api);
        }
        let loaded = Arc::new(Api::load(path)?);
        *slot = Some(Arc::clone(&loaded));
        Ok(loaded)
    }

    impl Api {
        fn load(path: &Path) -> Result<Self, String> {
            let library = unsafe { libloading::Library::new(path) }
                .map_err(|error| format!("could not load {}: {error}", path.display()))?;
            let version = sym::<Version>(&library, b"mpv_client_api_version\0")?;
            Ok(Self {
                version: unsafe { version() },
                create: sym(&library, b"mpv_create\0")?,
                initialize: sym(&library, b"mpv_initialize\0")?,
                destroy: sym(&library, b"mpv_terminate_destroy\0")?,
                set_option: sym(&library, b"mpv_set_option_string\0")?,
                set_property: sym(&library, b"mpv_set_property_string\0")?,
                command: sym(&library, b"mpv_command\0")?,
                wait_event: sym(&library, b"mpv_wait_event\0")?,
                get_property: sym(&library, b"mpv_get_property_string\0")?,
                free: sym(&library, b"mpv_free\0")?,
                add_stream: sym(&library, b"mpv_stream_cb_add_ro\0")?,
                render_create: sym(&library, b"mpv_render_context_create\0")?,
                render: sym(&library, b"mpv_render_context_render\0")?,
                render_free: sym(&library, b"mpv_render_context_free\0")?,
                _library: library,
            })
        }
    }

    fn sym<T: Copy>(library: &libloading::Library, name: &[u8]) -> Result<T, String> {
        let symbol: libloading::Symbol<T> = unsafe { library.get(name) }.map_err(|error| {
            format!(
                "libmpv is missing {}: {error}",
                String::from_utf8_lossy(name).trim_end_matches('\0')
            )
        })?;
        Ok(*symbol)
    }

    pub(super) fn start(path: &Path, bytes: Arc<[u8]>) -> Result<Clip, String> {
        if bytes.is_empty() {
            return Err("the video file is empty".into());
        }
        let api = api(path)?;
        let shared = Arc::new(Mutex::new(Shared {
            image: None,
            stale: Vec::new(),
            width: 0,
            height: 0,
            position: Duration::ZERO,
            duration: Duration::ZERO,
            finished: false,
            failure: None,
            clock: false,
        }));
        let paused = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared_worker, paused_worker, stop_worker) =
            (Arc::clone(&shared), Arc::clone(&paused), Arc::clone(&stop));
        let join = std::thread::Builder::new()
            .name("story-video".into())
            .spawn(move || play(api, bytes, shared_worker, paused_worker, stop_worker))
            .map_err(|error| error.to_string())?;
        Ok(Clip {
            shared,
            paused,
            stop,
            join: Some(join),
        })
    }

    fn play(
        api: Arc<Api>,
        bytes: Arc<[u8]>,
        shared: Arc<Mutex<Shared>>,
        paused: Arc<AtomicBool>,
        stop: Arc<AtomicBool>,
    ) {
        let result = session(&api, &bytes, &shared, &paused, &stop);
        if let Err(error) = result {
            tracing::warn!(%error, "story video stopped");
            lock(&shared).failure = Some(error);
        }
    }

    fn session(
        api: &Api,
        bytes: &Arc<[u8]>,
        shared: &Mutex<Shared>,
        paused: &AtomicBool,
        stop: &AtomicBool,
    ) -> Result<(), String> {
        let (ctx, render) = core(api, "auto").or_else(|error| {
            if error.contains("audio output") {
                core(api, "null")
            } else {
                Err(error)
            }
        })?;
        let source = Box::into_raw(Box::new(Source {
            bytes: Arc::clone(bytes),
            cancel: api.version >= ((1 << 16) | 106) as std::os::raw::c_ulong,
        }));
        let protocol = CString::new("wuapi").map_err(|error| error.to_string())?;
        let added = unsafe { (api.add_stream)(ctx, protocol.as_ptr(), source as *mut _, open_ro) };
        if added < 0 {
            unsafe { (api.render_free)(render) };
            unsafe { (api.destroy)(ctx) };
            unsafe { drop(Box::from_raw(source)) };
            return Err(format!("the video stream was refused ({added})"));
        }
        let load = CString::new("loadfile").unwrap();
        let uri = CString::new("wuapi://story").unwrap();
        let mode = CString::new("replace").unwrap();
        let args = [load.as_ptr(), uri.as_ptr(), mode.as_ptr(), std::ptr::null()];
        let loaded = unsafe { (api.command)(ctx, args.as_ptr()) };
        if loaded < 0 {
            unsafe { (api.render_free)(render) };
            unsafe { (api.destroy)(ctx) };
            unsafe { drop(Box::from_raw(source)) };
            return Err(format!("the video could not be opened ({loaded})"));
        }
        let mut applied_pause = false;
        let mut size: Option<(i32, i32)> = None;
        let mut last_frame = std::time::Instant::now()
            .checked_sub(Duration::from_millis(40))
            .unwrap_or_else(std::time::Instant::now);
        while !stop.load(Ordering::Relaxed) {
            let want_pause = paused.load(Ordering::Relaxed);
            if want_pause != applied_pause {
                let _ = set_string(
                    api,
                    ctx,
                    true,
                    "pause",
                    if want_pause { "yes" } else { "no" },
                );
                applied_pause = want_pause;
            }
            let event = unsafe { (api.wait_event)(ctx, 0.03) };
            if !event.is_null() {
                let event = unsafe { &*event.cast::<MpvEvent>() };
                // END_FILE is 7. The reason is the first int of the payload.
                if event.event_id == 7 {
                    let reason = if event.data.is_null() {
                        0
                    } else {
                        unsafe { *event.data.cast::<std::ffi::c_int>() }
                    };
                    if reason == 4 {
                        lock(shared).failure = Some("the video could not be read".into());
                        break;
                    }
                    if reason == 0 || reason == 2 {
                        publish_position(api, ctx, shared);
                        render_frame(api, render, ctx, shared, &mut size);
                        lock(shared).finished = true;
                        break;
                    }
                }
                if event.event_id == 1 {
                    break;
                }
            }
            publish_position(api, ctx, shared);
            if !want_pause && last_frame.elapsed() >= Duration::from_millis(33) {
                render_frame(api, render, ctx, shared, &mut size);
                last_frame = std::time::Instant::now();
            }
        }
        unsafe { (api.render_free)(render) };
        unsafe { (api.destroy)(ctx) };
        // Stream callbacks are finished once the core is destroyed.
        unsafe { drop(Box::from_raw(source)) };
        Ok(())
    }

    /// `mpv_event`: the reason of END_FILE is the first int behind `data`.
    #[repr(C)]
    struct MpvEvent {
        event_id: std::ffi::c_int,
        _error: std::ffi::c_int,
        _reply: u64,
        data: *mut std::ffi::c_void,
    }

    fn core(
        api: &Api,
        audio: &str,
    ) -> Result<(*mut std::ffi::c_void, *mut std::ffi::c_void), String> {
        let ctx = unsafe { (api.create)() };
        if ctx.is_null() {
            return Err("libmpv could not be created".into());
        }
        for (name, value) in [
            ("config", "no"),
            ("load-scripts", "no"),
            ("vo", "libmpv"),
            ("hwdec", "no"),
            ("idle", "yes"),
            ("ao", audio),
            ("keep-open", "no"),
            ("force-window", "no"),
            ("osc", "no"),
            ("terminal", "no"),
            ("input-default-bindings", "no"),
            ("input-vo-keyboard", "no"),
            ("ytdl", "no"),
            ("sw-fast", "yes"),
        ] {
            let required = name == "vo" || name == "idle" || name == "config";
            if let Err(error) = set_string(api, ctx, false, name, value) {
                if required || name == "ao" {
                    unsafe { (api.destroy)(ctx) };
                    return Err(if name == "ao" {
                        "audio output failed".into()
                    } else {
                        error
                    });
                }
            }
        }
        let code = unsafe { (api.initialize)(ctx) };
        if code < 0 {
            unsafe { (api.destroy)(ctx) };
            return Err(if code == -14 {
                "audio output failed".into()
            } else {
                format!("libmpv did not start ({code})")
            });
        }
        let mut render = std::ptr::null_mut();
        let api_type = CString::new("sw").unwrap();
        let mut params = [
            RenderParam {
                kind: 1,
                data: api_type.as_ptr() as *mut _,
            },
            RenderParam {
                kind: 0,
                data: std::ptr::null_mut(),
            },
        ];
        let created = unsafe { (api.render_create)(&mut render, ctx, params.as_mut_ptr()) };
        if created < 0 || render.is_null() {
            unsafe { (api.destroy)(ctx) };
            return Err(format!(
                "the video renderer is not in this libmpv ({created})"
            ));
        }
        Ok((ctx, render))
    }

    fn set_string(
        api: &Api,
        ctx: *mut std::ffi::c_void,
        property: bool,
        name: &str,
        value: &str,
    ) -> Result<(), String> {
        let name = CString::new(name).map_err(|error| error.to_string())?;
        let value = CString::new(value).map_err(|error| error.to_string())?;
        let code = unsafe {
            if property {
                (api.set_property)(ctx, name.as_ptr(), value.as_ptr())
            } else {
                (api.set_option)(ctx, name.as_ptr(), value.as_ptr())
            }
        };
        if code < 0 {
            return Err(format!("could not set {name:?} ({code})"));
        }
        Ok(())
    }

    fn property(api: &Api, ctx: *mut std::ffi::c_void, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let ptr = unsafe { (api.get_property)(ctx, name.as_ptr()) };
        if ptr.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(ptr) }
            .to_string_lossy()
            .into_owned();
        unsafe { (api.free)(ptr as *mut _) };
        Some(text)
    }

    fn publish_position(api: &Api, ctx: *mut std::ffi::c_void, shared: &Mutex<Shared>) {
        let Some(text) = property(api, ctx, "time-pos") else {
            return;
        };
        let Ok(seconds) = text.parse::<f64>() else {
            return;
        };
        if !seconds.is_finite() || seconds < 0. {
            return;
        }
        lock(shared).position = Duration::from_secs_f64(seconds);
    }

    fn render_frame(
        api: &Api,
        render: *mut std::ffi::c_void,
        ctx: *mut std::ffi::c_void,
        shared: &Mutex<Shared>,
        size: &mut Option<(i32, i32)>,
    ) {
        if size.is_none() {
            let width = property(api, ctx, "video-params/w").and_then(|text| text.parse().ok());
            let height = property(api, ctx, "video-params/h").and_then(|text| text.parse().ok());
            if let (Some(width), Some(height)) = (width, height) {
                *size = Some(fitted_even(width, height));
            }
        }
        let Some((width, height)) = *size else {
            return;
        };
        let stride = stride_for(width);
        let mut buffer = vec![0u8; stride * height as usize + 64];
        let offset = buffer.as_mut_ptr().align_offset(64);
        let offset = if offset == usize::MAX {
            0
        } else {
            offset.min(64)
        };
        let pointer = unsafe { buffer.as_mut_ptr().add(offset) };
        let mut sw = [width, height];
        let format = CString::new("bgr0").unwrap();
        let mut stride_value = stride;
        let mut block: std::ffi::c_int = 0;
        let mut params = [
            RenderParam {
                kind: 17,
                data: sw.as_mut_ptr().cast(),
            },
            RenderParam {
                kind: 18,
                data: format.as_ptr() as *mut _,
            },
            RenderParam {
                kind: 19,
                data: (&mut stride_value as *mut usize).cast(),
            },
            RenderParam {
                kind: 20,
                data: pointer.cast(),
            },
            RenderParam {
                kind: 12,
                data: (&mut block as *mut std::ffi::c_int).cast(),
            },
            RenderParam {
                kind: 0,
                data: std::ptr::null_mut(),
            },
        ];
        let code = unsafe { (api.render)(render, params.as_mut_ptr()) };
        if code < 0 {
            return;
        }
        let start = offset;
        let end = start + stride * height as usize;
        if end > buffer.len() {
            return;
        }
        bgr0_to_bgra(&mut buffer[start..end], width as usize, stride);
        let packed = pack_rows(&buffer[start..end], width as usize, height as usize, stride);
        let Some(image) = image::RgbaImage::from_raw(width as u32, height as u32, packed) else {
            return;
        };
        let image = Arc::new(gpui_kit::RenderImage::new(vec![image::Frame::new(image)]));
        let mut shared = lock(shared);
        if let Some(previous) = shared.image.replace(image) {
            shared.stale.push(previous);
            let extra = shared.stale.len().saturating_sub(4);
            if extra > 0 {
                shared.stale.drain(0..extra);
            }
        }
        shared.width = width as u32;
        shared.height = height as u32;
    }

    struct Source {
        bytes: Arc<[u8]>,
        cancel: bool,
    }

    struct Cursor {
        bytes: Arc<[u8]>,
        pos: Mutex<u64>,
    }

    unsafe extern "C" fn open_ro(
        user_data: *mut std::ffi::c_void,
        _uri: *mut std::ffi::c_char,
        info: *mut std::ffi::c_void,
    ) -> std::ffi::c_int {
        if user_data.is_null() || info.is_null() {
            return -13;
        }
        let source = unsafe { &*user_data.cast::<Source>() };
        let cursor = Box::into_raw(Box::new(Cursor {
            bytes: Arc::clone(&source.bytes),
            pos: Mutex::new(0),
        }));
        let slots = info.cast::<*mut std::ffi::c_void>();
        unsafe {
            *slots.add(0) = cursor.cast();
            *slots.add(1) = read_bytes as *mut _;
            *slots.add(2) = seek_bytes as *mut _;
            *slots.add(3) = size_bytes as *mut _;
            *slots.add(4) = close_bytes as *mut _;
            if source.cancel {
                *slots.add(5) = std::ptr::null_mut();
            }
        }
        0
    }

    unsafe extern "C" fn read_bytes(
        cookie: *mut std::ffi::c_void,
        buf: *mut std::ffi::c_char,
        nbytes: u64,
    ) -> i64 {
        if cookie.is_null() || buf.is_null() {
            return -20;
        }
        let cursor = unsafe { &*cookie.cast::<Cursor>() };
        let mut pos = cursor
            .pos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let at = *pos as usize;
        if at >= cursor.bytes.len() {
            return 0;
        }
        let count = nbytes.min((cursor.bytes.len() - at) as u64) as usize;
        unsafe {
            std::ptr::copy_nonoverlapping(cursor.bytes.as_ptr().add(at), buf.cast(), count);
        }
        *pos += count as u64;
        count as i64
    }

    unsafe extern "C" fn seek_bytes(cookie: *mut std::ffi::c_void, offset: i64) -> i64 {
        if cookie.is_null() || offset < 0 {
            return -18;
        }
        let cursor = unsafe { &*cookie.cast::<Cursor>() };
        if offset as usize > cursor.bytes.len() {
            return -20;
        }
        *cursor
            .pos
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = offset as u64;
        offset
    }

    unsafe extern "C" fn size_bytes(cookie: *mut std::ffi::c_void) -> i64 {
        if cookie.is_null() {
            return -18;
        }
        unsafe { &*cookie.cast::<Cursor>() }.bytes.len() as i64
    }

    unsafe extern "C" fn close_bytes(cookie: *mut std::ffi::c_void) {
        if !cookie.is_null() {
            unsafe { drop(Box::from_raw(cookie.cast::<Cursor>())) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn library_name() -> &'static str {
        if cfg!(target_os = "windows") {
            "libmpv-2.dll"
        } else if cfg!(target_os = "macos") {
            "libmpv.dylib"
        } else {
            "libmpv.so"
        }
    }

    fn archive(path: &str, bytes: &[u8]) -> Vec<u8> {
        let mut raw = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut raw);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            // The builder refuses `..` itself. The name is written here so
            // the test can hand the extractor a path it must reject.
            let name = path.as_bytes();
            assert!(name.len() < 100);
            header.as_gnu_mut().unwrap().name[..name.len()].copy_from_slice(name);
            header.set_cksum();
            builder.append(&header, bytes).unwrap();
            builder.finish().unwrap();
        }
        let mut out = Vec::new();
        let mut encoder = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap();
        out
    }

    #[test]
    fn the_embedded_runtime_is_extracted_and_a_replacement_wins() {
        let root = std::env::temp_dir().join(format!("wuapi-video-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let cache = root.join("cache");
        let packed = archive(
            &format!("runtime/{library}", library = library_name()),
            b"libmpv-bytes",
        );
        let extracted = resolve_library(None, &packed, &cache).unwrap();
        assert_eq!(std::fs::read(&extracted).unwrap(), b"libmpv-bytes");
        assert!(extracted.starts_with(&cache));
        let again = resolve_library(None, &packed, &cache).unwrap();
        assert_eq!(again, extracted);

        let replacement = root.join("other-libmpv");
        std::fs::write(&replacement, b"replacement").unwrap();
        assert_eq!(
            resolve_library(Some(replacement.clone()), &packed, &cache).unwrap(),
            replacement
        );
        let missing = resolve_library(None, &[], &cache).unwrap_err();
        assert!(!missing.to_lowercase().contains("system"));
        let unsafe_path = archive(&format!("../{}", library_name()), b"escape");
        let refused = resolve_library(None, &unsafe_path, &cache.join("slip")).unwrap_err();
        assert!(refused.contains("unsafe"));
        assert!(!root.join(library_name()).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_replacement_is_not_silently_swapped() {
        let missing =
            std::env::temp_dir().join(format!("wuapi-missing-libmpv-{}", std::process::id()));
        let error = resolve_library(Some(missing), b"bundle", &std::env::temp_dir()).unwrap_err();
        assert!(error.contains("WUAPI_LIBMPV_PATH"));
    }

    #[test]
    fn frames_are_capped_and_bgr0_becomes_solid_bgra() {
        assert_eq!(fitted_even(1920, 1080), (640, 360));
        assert_eq!(fitted_even(100, 80), (100, 80));
        assert_eq!(fitted_even(0, 10), (2, 2));
        let mut pixels = vec![1, 2, 3, 9, 4, 5, 6, 0];
        bgr0_to_bgra(&mut pixels, 2, 8);
        assert_eq!(pixels, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let mut padded = vec![1, 2, 3, 0, 4, 5, 6, 0, 7, 7, 7, 7];
        bgr0_to_bgra(&mut padded, 2, 12);
        assert_eq!(
            pack_rows(&padded, 2, 1, 12),
            vec![1, 2, 3, 255, 4, 5, 6, 255]
        );
        assert_eq!(stride_for(2) % 64, 0);
    }

    #[test]
    fn a_clock_clip_pauses_and_finishes_without_a_native_library() {
        let clip = clock(Duration::from_millis(1_000));
        assert!(clip.report().image.is_some());
        clip.poll(Duration::from_millis(400));
        assert_eq!(clip.report().position, Duration::from_millis(400));
        clip.set_paused(true);
        clip.poll(Duration::from_secs(5));
        assert_eq!(clip.report().position, Duration::from_millis(400));
        clip.set_paused(false);
        clip.poll(Duration::from_millis(700));
        let report = clip.report();
        assert!(report.finished);
        assert_eq!(report.position, Duration::from_millis(1_000));
    }

    #[test]
    fn an_installed_opener_is_what_open_uses() {
        let _guard = install_opener(Arc::new(|_| Ok(clock(Duration::from_secs(3)))));
        let clip = Clip::open(Arc::<[u8]>::from(&b"not a video"[..])).unwrap();
        assert_eq!(clip.report().position, Duration::ZERO);
        assert!(clip.report().failure.is_none());
        assert!(clip.report().image.is_some());
    }
}
