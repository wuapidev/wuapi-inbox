//! Pictures on the system's clipboard.
//!
//! The toolkit writes pictures to the clipboard on macOS and Windows. On
//! Linux it writes text only (its Wayland client offers the text types and
//! nothing else; its X11 client sets text), so there a picture goes through
//! a backend of our own:
//!
//! * **Wayland, data-control** (`wl-clipboard-rs`): the protocol wlroots
//!   compositors (Hyprland, Sway) and KDE give to clipboard tools. The
//!   picture is offered as `image/png` and, when it is one, as the JPEG or
//!   WebP it came as. A thread of ours keeps answering until another copy
//!   replaces it; it never holds the interface, and ends with the process.
//! * **X11** (`arboard`): as `image/png`. Also what is used on a Wayland
//!   session whose compositor does not give data-control to applications
//!   (GNOME): through XWayland, whose clipboard the compositor mirrors.
//!
//! Which one is tried, and in which order, is [`backends`]: a function of
//! the session, tested without a session.

use std::sync::Arc;

/// A picture as it is put on the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageOffer {
    /// The picture as PNG: what every paste target takes.
    pub png: Arc<Vec<u8>>,
    /// The picture as it came, with its MIME type, when it is not a PNG
    /// (a JPEG, a WebP): for targets that prefer the original.
    pub original: Option<(String, Arc<Vec<u8>>)>,
}

impl ImageOffer {
    /// The MIME types offered, the preferred one first.
    #[cfg(test)]
    pub fn types(&self) -> Vec<&str> {
        let mut types = vec!["image/png"];
        types.extend(self.original.as_ref().map(|(mime, _)| mime.as_str()));
        types
    }
}

/// Makes the offer for a picture's bytes: a PNG stays as it is; anything
/// else is decoded and encoded as PNG, and kept as it came beside it.
/// Decoding is behind the same limits as every picture shown.
pub fn offer(bytes: Vec<u8>, mime: Option<&str>) -> Result<ImageOffer, String> {
    let format = image::guess_format(&bytes).map_err(|_| "This is not a picture.".to_owned())?;
    let bytes = Arc::new(bytes);
    if format == image::ImageFormat::Png {
        return Ok(ImageOffer {
            png: bytes,
            original: None,
        });
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()));
    reader.set_format(format);
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("The picture could not be read: {error}"))?;
    let mut png = Vec::new();
    decoded
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .map_err(|error| format!("The picture could not be converted: {error}"))?;
    let named = match format {
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::WebP => Some("image/webp"),
        image::ImageFormat::Gif => Some("image/gif"),
        _ => None,
    };
    // What the message said it is, when it agrees with what it is.
    let mime = named.or(mime.filter(|mime| mime.starts_with("image/")));
    Ok(ImageOffer {
        png: Arc::new(png),
        original: mime.map(|mime| (mime.to_owned(), bytes)),
    })
}

/// A way of putting a picture on the clipboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// The toolkit's own clipboard (macOS, Windows).
    Toolkit,
    /// The Wayland data-control protocol.
    WaylandDataControl,
    /// X11, or XWayland on a Wayland session.
    X11,
}

/// What the session looks like, as far as the clipboard goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Session {
    /// Linux (or another Unix with these display servers).
    pub linux: bool,
    /// `WAYLAND_DISPLAY` is set.
    pub wayland: bool,
    /// `DISPLAY` is set.
    pub x11: bool,
}

impl Session {
    /// The session this process runs in.
    pub fn current() -> Self {
        let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
        Self {
            linux: cfg!(target_os = "linux"),
            wayland: set("WAYLAND_DISPLAY"),
            x11: set("DISPLAY"),
        }
    }
}

/// The backends to try for a picture, in order: the next one is tried
/// when one cannot do it.
pub fn backends(session: Session) -> Vec<Backend> {
    if !session.linux {
        return vec![Backend::Toolkit];
    }
    let mut order = Vec::new();
    if session.wayland {
        order.push(Backend::WaylandDataControl);
    }
    // On a Wayland session too: XWayland's clipboard is mirrored by the
    // compositor, which is the way in where data-control is not given.
    if session.x11 {
        order.push(Backend::X11);
    }
    order
}

/// Puts a picture on the clipboard through one backend. Never called for
/// [`Backend::Toolkit`], which needs the interface's thread.
#[cfg(target_os = "linux")]
fn copy_with(backend: Backend, offer: &ImageOffer) -> Result<(), String> {
    match backend {
        Backend::WaylandDataControl => {
            use wl_clipboard_rs::copy::{MimeSource, MimeType, Options, Source};
            let source = |mime: &str, bytes: &Arc<Vec<u8>>| MimeSource {
                source: Source::Bytes(bytes.as_slice().to_vec().into_boxed_slice()),
                mime_type: MimeType::Specific(mime.to_owned()),
            };
            let mut sources = vec![source("image/png", &offer.png)];
            sources.extend(
                offer
                    .original
                    .as_ref()
                    .map(|(mime, bytes)| source(mime, bytes)),
            );
            // Not in the foreground: a thread of the library's keeps
            // answering paste requests until another copy replaces this.
            Options::new()
                .copy_multi(sources)
                .map_err(|error| error.to_string())
        }
        Backend::X11 => {
            use std::sync::{Mutex, OnceLock};
            // Kept for the life of the process: on X11 the picture is
            // served by whoever owns the selection, which is this object.
            static CLIPBOARD: OnceLock<Mutex<Option<arboard::Clipboard>>> = OnceLock::new();
            let decoded = image::load_from_memory_with_format(&offer.png, image::ImageFormat::Png)
                .map_err(|error| error.to_string())?
                .into_rgba8();
            let (width, height) = decoded.dimensions();
            let mut kept = CLIPBOARD
                .get_or_init(|| Mutex::new(None))
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if kept.is_none() {
                *kept = Some(arboard::Clipboard::new().map_err(|error| error.to_string())?);
            }
            let clipboard = kept.as_mut().expect("just made");
            clipboard
                .set_image(arboard::ImageData {
                    width: width as usize,
                    height: height as usize,
                    bytes: decoded.into_raw().into(),
                })
                .map_err(|error| error.to_string())
        }
        Backend::Toolkit => Err("not on this platform".to_owned()),
    }
}

#[cfg(not(target_os = "linux"))]
fn copy_with(_: Backend, _: &ImageOffer) -> Result<(), String> {
    Err("not on this platform".to_owned())
}

/// Where pictures are copied to. The application has the system's; a test
/// brings one that remembers what it was given.
pub trait ImageClipboard: Send + Sync {
    /// Whether a copy has to be made on the interface's thread, through
    /// the toolkit ([`copy_on_toolkit`]).
    fn through_toolkit(&self) -> bool {
        false
    }

    /// Puts the picture on the clipboard. May take a moment (a round trip
    /// to the display server): called off the interface's thread.
    fn copy(&self, offer: &ImageOffer) -> Result<(), String>;
}

/// The system's clipboard.
pub struct SystemClipboard {
    order: Vec<Backend>,
}

impl Default for SystemClipboard {
    fn default() -> Self {
        Self {
            order: backends(Session::current()),
        }
    }
}

impl ImageClipboard for SystemClipboard {
    fn through_toolkit(&self) -> bool {
        self.order == [Backend::Toolkit]
    }

    fn copy(&self, offer: &ImageOffer) -> Result<(), String> {
        let mut failures = Vec::new();
        for backend in &self.order {
            match copy_with(*backend, offer) {
                Ok(()) => return Ok(()),
                Err(error) => {
                    tracing::debug!(?backend, %error, "this clipboard backend could not copy");
                    failures.push(format!("{backend:?}: {error}"));
                }
            }
        }
        Err(if failures.is_empty() {
            "no clipboard was found in this session".to_owned()
        } else {
            failures.join("; ")
        })
    }
}

/// Puts the picture on the toolkit's clipboard: on the interface's thread,
/// where the toolkit writes pictures (macOS, Windows).
pub fn copy_on_toolkit(offer: &ImageOffer, cx: &mut gpui_kit::App) {
    let (format, bytes) = match &offer.original {
        Some((mime, bytes)) if mime == "image/jpeg" => (gpui_kit::ImageFormat::Jpeg, bytes),
        Some((mime, bytes)) if mime == "image/webp" => (gpui_kit::ImageFormat::Webp, bytes),
        _ => (gpui_kit::ImageFormat::Png, &offer.png),
    };
    let image = gpui_kit::Image::from_bytes(format, bytes.as_slice().to_vec());
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_image(&image));
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A clipboard that remembers what was copied, or refuses.
    #[derive(Default)]
    pub struct FakeClipboard {
        pub copied: Mutex<Vec<ImageOffer>>,
        pub refuse: Mutex<Option<String>>,
    }

    impl ImageClipboard for FakeClipboard {
        fn copy(&self, offer: &ImageOffer) -> Result<(), String> {
            if let Some(reason) = self.refuse.lock().unwrap().clone() {
                return Err(reason);
            }
            self.copied.lock().unwrap().push(offer.clone());
            Ok(())
        }
    }

    fn picture(format: image::ImageFormat) -> Vec<u8> {
        let pixels = image::RgbImage::from_pixel(6, 4, image::Rgb([200, 30, 30]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(pixels)
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    #[test]
    fn the_backends_follow_the_session() {
        let on = |linux, wayland, x11| {
            backends(Session {
                linux,
                wayland,
                x11,
            })
        };
        // macOS and Windows: the toolkit writes pictures itself.
        assert_eq!(on(false, false, false), [Backend::Toolkit]);
        // Hyprland, Sway, KDE: data-control, and XWayland behind it.
        assert_eq!(
            on(true, true, true),
            [Backend::WaylandDataControl, Backend::X11]
        );
        // A Wayland session without XWayland.
        assert_eq!(on(true, true, false), [Backend::WaylandDataControl]);
        // X11.
        assert_eq!(on(true, false, true), [Backend::X11]);
        // Neither: nothing to try, and the copy says so.
        assert!(on(true, false, false).is_empty());
        let nowhere = SystemClipboard { order: Vec::new() };
        let png = ImageOffer {
            png: Arc::new(picture(image::ImageFormat::Png)),
            original: None,
        };
        assert!(nowhere.copy(&png).unwrap_err().contains("no clipboard"));
        assert!(!nowhere.through_toolkit());
    }

    #[test]
    fn a_picture_is_offered_as_png_and_as_it_came() {
        // A PNG is offered as it is.
        let png = picture(image::ImageFormat::Png);
        let offered = offer(png.clone(), Some("image/png")).unwrap();
        assert_eq!(offered.png.as_slice(), png.as_slice());
        assert_eq!(offered.types(), ["image/png"]);

        // A JPEG as a PNG made from it, and as the JPEG it is, untouched.
        let jpeg = picture(image::ImageFormat::Jpeg);
        let offered = offer(jpeg.clone(), None).unwrap();
        assert_eq!(offered.types(), ["image/png", "image/jpeg"]);
        assert_eq!(
            offered.original.as_ref().unwrap().1.as_slice(),
            jpeg.as_slice()
        );
        let back =
            image::load_from_memory_with_format(&offered.png, image::ImageFormat::Png).unwrap();
        assert_eq!((back.width(), back.height()), (6, 4));

        // What is not a picture is not copied as one.
        assert!(offer(b"not a picture".to_vec(), Some("image/png")).is_err());
    }
}
