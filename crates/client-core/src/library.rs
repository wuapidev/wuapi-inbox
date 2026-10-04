//! What goes into the sticker and GIF library, and what a sticker has to
//! be: told from the bytes, never from a name.
//!
//! A WhatsApp sticker is a WebP of at most 512 by 512 pixels, about 100 KB
//! when it is still and 500 KB when it moves. The API and the engines do
//! not check any of that or convert anything (a PNG is sent as the PNG it
//! is), so the client makes stickers the way phones do before it sends
//! one: [`sticker_from_image`] turns a picture into a 512 by 512 WebP with
//! transparent padding.
//!
//! **The encoder** is the `image` crate's, which writes lossless WebP and
//! nothing else (its `image-webp` has no lossy encoder and no animation
//! writer; a lossy one would be a C library, `libwebp`). To fit the size a
//! sticker is allowed, the colours are rounded to fewer bits (which a
//! lossless coder compresses much better) and, when that is not enough,
//! the picture is made smaller inside its 512 by 512 canvas. **Animated
//! sources are not converted**: an animated GIF gives its first frame, and
//! an animated WebP is taken as it is when it already fits (at most 512
//! per side, 500 KB) and refused otherwise. Making one from a GIF would
//! take an animated WebP writer.

use image::imageops::FilterType;
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageReader, Limits};
use image::{Rgba, RgbaImage};
use sha2::{Digest, Sha256};
use std::io::Cursor;

/// The side of a sticker's canvas, in pixels.
pub const STICKER_SIDE: u32 = 512;
/// The largest a still sticker is made, in bytes.
pub const STICKER_STILL_MAX: usize = 100 * 1024;
/// The largest an animated sticker may be, in bytes.
pub const STICKER_MOVING_MAX: usize = 500 * 1024;
/// The largest GIF the library takes, in bytes.
pub const GIF_MAX: usize = 8 * 1024 * 1024;
/// The side of the still kept for the picker.
pub const LIBRARY_THUMB_SIDE: u32 = 160;

/// The most pixels per side of a picture that is decoded to make a
/// sticker, and the memory it may take.
const MAX_SOURCE: u32 = 8_000;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

/// What a file is, told from its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileKind {
    /// A WebP, still or moving.
    Webp {
        /// It has more than one frame.
        animated: bool,
    },
    /// A GIF, still or moving.
    Gif {
        /// It has more than one frame.
        animated: bool,
    },
    /// A PNG.
    Png,
    /// A JPEG.
    Jpeg,
    /// An MP4 or another ISO media file (QuickTime included).
    Mp4,
    /// Anything else.
    Other,
}

/// What `bytes` are.
pub fn sniff(bytes: &[u8]) -> FileKind {
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return FileKind::Webp {
            animated: crate::animated_format(bytes).is_some(),
        };
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return FileKind::Gif {
            animated: crate::animated_format(bytes).is_some(),
        };
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return FileKind::Png;
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return FileKind::Jpeg;
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return FileKind::Mp4;
    }
    FileKind::Other
}

/// The name of a file by its content: the hex SHA-256 of its bytes. The
/// same sticker is the same id wherever it came from.
pub fn content_id(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut id = String::with_capacity(64);
    for byte in digest {
        id.push_str(&format!("{byte:02x}"));
    }
    id
}

/// Why a file did not become a sticker or a GIF.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// Nothing in it.
    #[error("The file is empty.")]
    Empty,
    /// Not a picture this application reads.
    #[error("This is not a picture that can be made into a sticker (PNG, JPEG, WebP or GIF).")]
    NotAnImage,
    /// An animated WebP larger than a sticker may be.
    #[error(
        "This animated WebP is larger than a sticker may be (512 by 512 pixels, 500 KB). \
         Moving pictures are not resized here."
    )]
    MovingTooLarge,
    /// Could not be made small enough.
    #[error("This picture is too detailed to fit in a sticker.")]
    TooDetailed,
    /// Not an MP4 or a GIF.
    #[error("A GIF here is a .gif file or a short MP4.")]
    NotAGif,
    /// Larger than the library takes.
    #[error("The file is larger than a GIF may be (8 MB).")]
    GifTooLarge,
    /// The file could not be read as the picture it says it is.
    #[error("The picture could not be read: {0}")]
    Unreadable(String),
}

/// A sticker, ready to be sent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Sticker {
    /// The WebP.
    pub bytes: Vec<u8>,
    /// `image/webp`.
    pub mime: &'static str,
    /// Its size in pixels.
    pub width: u32,
    /// Its size in pixels.
    pub height: u32,
    /// It moves.
    pub animated: bool,
    /// What was done to the file, to tell the user: the picture was
    /// scaled, an animation was reduced to its first frame, and so on.
    pub notes: Vec<&'static str>,
}

/// The canvas size of a WebP, from its header, without decoding it.
fn webp_canvas(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() >= 30 && &bytes[12..16] == b"VP8X" {
        let at = |i: usize| {
            u32::from(bytes[i]) | u32::from(bytes[i + 1]) << 8 | u32::from(bytes[i + 2]) << 16
        };
        return Some((at(24) + 1, at(27) + 1));
    }
    None
}

fn reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, ImportError> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| ImportError::Unreadable(error.to_string()))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE);
    limits.max_image_height = Some(MAX_SOURCE);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    Ok(reader)
}

/// Encodes pixels as a lossless WebP.
fn webp(image: &RgbaImage) -> Result<Vec<u8>, ImportError> {
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .write_image(
            image.as_raw(),
            image.width(),
            image.height(),
            ExtendedColorType::Rgba8,
        )
        .map_err(|error| ImportError::Unreadable(error.to_string()))?;
    Ok(out)
}

/// Rounds every channel to `bits` bits and clears the colour of what is
/// transparent: both make a lossless coder's job much smaller.
fn rounded(image: &RgbaImage, bits: u8) -> RgbaImage {
    let mut out = image.clone();
    let drop = 8 - bits.min(8);
    for pixel in out.pixels_mut() {
        if pixel[3] == 0 {
            *pixel = Rgba([0, 0, 0, 0]);
            continue;
        }
        if drop > 0 {
            let round = |value: u8| -> u8 {
                let step = 1u16 << drop;
                let up = (u16::from(value) + step / 2).min(255);
                (up / step * step).min(255) as u8
            };
            for channel in 0..4 {
                pixel[channel] = round(pixel[channel]);
            }
        }
    }
    out
}

/// The picture scaled to fit `side` inside, centred on a transparent
/// 512 by 512 canvas.
fn on_canvas(image: &DynamicImage, side: u32) -> RgbaImage {
    let side = side.clamp(16, STICKER_SIDE);
    let scaled = if image.width() == side && image.height() <= side
        || image.height() == side && image.width() <= side
    {
        image.to_rgba8()
    } else {
        image.resize(side, side, FilterType::Lanczos3).to_rgba8()
    };
    let mut canvas = RgbaImage::from_pixel(STICKER_SIDE, STICKER_SIDE, Rgba([0, 0, 0, 0]));
    let left = (STICKER_SIDE - scaled.width()) / 2;
    let top = (STICKER_SIDE - scaled.height()) / 2;
    image::imageops::replace(&mut canvas, &scaled, i64::from(left), i64::from(top));
    canvas
}

/// Makes a sticker of a picture: a WebP of 512 by 512 pixels, the picture
/// in the middle in its own proportions on transparent padding, at most
/// [`STICKER_STILL_MAX`] bytes. See the module's notes on the encoder and
/// on what is not converted. Blocking and CPU-bound: call it off the UI
/// thread.
pub fn sticker_from_image(bytes: &[u8]) -> Result<Sticker, ImportError> {
    if bytes.is_empty() {
        return Err(ImportError::Empty);
    }
    let mut notes = Vec::new();
    match sniff(bytes) {
        FileKind::Webp { animated: true } => {
            // A moving sticker is taken as it is when it fits.
            let fits = webp_canvas(bytes)
                .is_some_and(|(width, height)| width <= STICKER_SIDE && height <= STICKER_SIDE);
            if !fits || bytes.len() > STICKER_MOVING_MAX {
                return Err(ImportError::MovingTooLarge);
            }
            let (width, height) = webp_canvas(bytes).unwrap_or((STICKER_SIDE, STICKER_SIDE));
            return Ok(Sticker {
                bytes: bytes.to_vec(),
                mime: "image/webp",
                width,
                height,
                animated: true,
                notes,
            });
        }
        FileKind::Gif { animated: true } => {
            notes.push("Only the first frame of the animation was kept.");
        }
        FileKind::Webp { .. } | FileKind::Gif { .. } | FileKind::Png | FileKind::Jpeg => {}
        FileKind::Mp4 | FileKind::Other => return Err(ImportError::NotAnImage),
    }
    let source = reader(bytes)?;
    let mut decoder = source
        .into_decoder()
        .map_err(|error| ImportError::Unreadable(error.to_string()))?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut decoded = DynamicImage::from_decoder(decoder)
        .map_err(|error| ImportError::Unreadable(error.to_string()))?;
    decoded.apply_orientation(orientation);
    // Already a sticker (a still WebP of the right size): kept as it is,
    // so that it is the file everybody else has.
    if matches!(sniff(bytes), FileKind::Webp { animated: false })
        && decoded.width() == STICKER_SIDE
        && decoded.height() == STICKER_SIDE
        && bytes.len() <= STICKER_STILL_MAX
    {
        return Ok(Sticker {
            bytes: bytes.to_vec(),
            mime: "image/webp",
            width: STICKER_SIDE,
            height: STICKER_SIDE,
            animated: false,
            notes,
        });
    }
    if decoded.width().max(decoded.height()) > STICKER_SIDE {
        notes.push("The picture was made smaller to fit 512 by 512 pixels.");
    }
    // From the most faithful to the least: the picture as it is, then its
    // colours rounded, then smaller inside the canvas.
    const STEPS: [(u32, u8); 8] = [
        (512, 8),
        (512, 6),
        (512, 5),
        (448, 5),
        (384, 5),
        (384, 4),
        (320, 4),
        (256, 4),
    ];
    let mut reduced = false;
    for (index, (side, bits)) in STEPS.into_iter().enumerate() {
        let canvas = on_canvas(&decoded, side);
        let canvas = if bits < 8 || index == 0 {
            rounded(&canvas, bits)
        } else {
            canvas
        };
        let out = webp(&canvas)?;
        if out.len() <= STICKER_STILL_MAX {
            if reduced {
                notes.push("Its colours were reduced to keep the sticker under 100 KB.");
            }
            return Ok(Sticker {
                bytes: out,
                mime: "image/webp",
                width: STICKER_SIDE,
                height: STICKER_SIDE,
                animated: false,
                notes,
            });
        }
        reduced = true;
    }
    Err(ImportError::TooDetailed)
}

/// What a GIF of the library is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GifFile {
    /// `video/mp4` or `image/gif`.
    pub mime: &'static str,
    /// It is a video: sent flagged to play as a GIF.
    pub video: bool,
    /// It moves (a `.gif` of one frame does not; a video always does).
    pub animated: bool,
    /// Its size in pixels, when it can be read without decoding video.
    pub size: Option<(u32, u32)>,
}

/// Checks a file that is to be kept as a GIF: a `.gif` or an MP4, at most
/// [`GIF_MAX`] bytes. Nothing is decoded for a video: this application has
/// no video decoder, so it has no still for it either.
pub fn gif_file(bytes: &[u8]) -> Result<GifFile, ImportError> {
    if bytes.is_empty() {
        return Err(ImportError::Empty);
    }
    if bytes.len() > GIF_MAX {
        return Err(ImportError::GifTooLarge);
    }
    match sniff(bytes) {
        FileKind::Mp4 => Ok(GifFile {
            mime: "video/mp4",
            video: true,
            animated: true,
            size: None,
        }),
        FileKind::Gif { animated } => {
            let size = reader(bytes)
                .ok()
                .and_then(|reader| reader.into_dimensions().ok());
            Ok(GifFile {
                mime: "image/gif",
                video: false,
                animated,
                size,
            })
        }
        _ => Err(ImportError::NotAGif),
    }
}

/// The size of a picture in pixels, read from its header.
pub fn image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    reader(bytes).ok()?.into_dimensions().ok()
}

/// A small still of the first frame of a picture, for the picker.
pub fn library_thumbnail(bytes: &[u8]) -> Option<(Vec<u8>, String)> {
    let thumb = crate::thumbnail(bytes, LIBRARY_THUMB_SIDE).ok()?;
    Some((thumb.bytes, thumb.mime.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    fn png_of(image: DynamicImage) -> Vec<u8> {
        let mut out = Vec::new();
        image
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    /// A photograph-like picture: smooth gradients with noise.
    fn busy(width: u32, height: u32) -> DynamicImage {
        let mut state = 12345u32;
        DynamicImage::ImageRgb8(image::RgbImage::from_fn(width, height, |x, y| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = (state >> 24) as u8 / 4;
            Rgb([
                ((x * 255 / width) as u8).saturating_add(noise),
                ((y * 255 / height) as u8).saturating_add(noise),
                (((x + y) * 255 / (width + height)) as u8).saturating_add(noise),
            ])
        }))
    }

    #[test]
    fn a_picture_becomes_a_512_webp_with_transparent_padding() {
        // 600 by 300: scaled to 512 by 256, centred.
        let flat =
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(600, 300, Rgba([200, 30, 30, 255])));
        let sticker = sticker_from_image(&png_of(flat)).unwrap();
        assert_eq!(sniff(&sticker.bytes), FileKind::Webp { animated: false });
        assert_eq!((sticker.width, sticker.height), (512, 512));
        assert!(!sticker.animated);
        assert!(sticker.bytes.len() <= STICKER_STILL_MAX);
        let back = image::load_from_memory(&sticker.bytes).unwrap().to_rgba8();
        assert_eq!(back.dimensions(), (512, 512));
        // Padding above and below is transparent, the middle is the picture.
        assert_eq!(back.get_pixel(256, 10)[3], 0);
        assert_eq!(back.get_pixel(256, 500)[3], 0);
        assert_eq!(back.get_pixel(256, 256).0, [200, 30, 30, 255]);
        assert!(sticker
            .notes
            .iter()
            .any(|note| note.contains("made smaller")));
    }

    #[test]
    fn transparency_in_the_source_is_kept() {
        let mut ring = RgbaImage::from_pixel(256, 256, Rgba([0, 0, 0, 0]));
        for (x, y, pixel) in ring.enumerate_pixels_mut() {
            let (dx, dy) = (x as f32 - 128., y as f32 - 128.);
            if (dx * dx + dy * dy).sqrt() < 100. {
                *pixel = Rgba([10, 120, 240, 255]);
            }
        }
        let sticker = sticker_from_image(&png_of(DynamicImage::ImageRgba8(ring))).unwrap();
        let back = image::load_from_memory(&sticker.bytes).unwrap().to_rgba8();
        // A 256 picture is scaled up to the canvas; its corner stays clear.
        assert_eq!(back.get_pixel(2, 2)[3], 0);
        assert_eq!(back.get_pixel(256, 256)[3], 255);
    }

    #[test]
    fn a_detailed_picture_is_brought_under_the_limit() {
        let sticker = sticker_from_image(&png_of(busy(1000, 1000))).unwrap();
        assert!(
            sticker.bytes.len() <= STICKER_STILL_MAX,
            "{} bytes",
            sticker.bytes.len()
        );
        assert_eq!((sticker.width, sticker.height), (512, 512));
        assert!(sticker
            .notes
            .iter()
            .any(|note| note.contains("colours were reduced")));
    }

    #[test]
    fn a_sticker_that_is_one_already_is_kept_as_it_is() {
        let first = sticker_from_image(&png_of(busy(300, 300))).unwrap();
        let again = sticker_from_image(&first.bytes).unwrap();
        assert_eq!(again.bytes, first.bytes);
    }

    #[test]
    fn a_moving_webp_that_fits_is_kept_and_one_that_does_not_is_refused() {
        let small = crate::animation::fixtures::animated_webp(64, 48, &[80, 80]);
        let kept = sticker_from_image(&small).unwrap();
        assert!(kept.animated);
        assert_eq!(kept.bytes, small);
        assert_eq!((kept.width, kept.height), (64, 48));
        let big = crate::animation::fixtures::animated_webp(700, 100, &[80, 80]);
        assert_eq!(sticker_from_image(&big), Err(ImportError::MovingTooLarge));
    }

    #[test]
    fn an_animated_gif_gives_its_first_frame() {
        let gif = crate::animation::fixtures::animated_gif(40, 40, 3, 100);
        let sticker = sticker_from_image(&gif).unwrap();
        assert!(!sticker.animated);
        assert!(sticker
            .notes
            .iter()
            .any(|note| note.contains("first frame")));
    }

    #[test]
    fn what_is_not_a_picture_is_refused() {
        assert_eq!(sticker_from_image(&[]), Err(ImportError::Empty));
        assert_eq!(
            sticker_from_image(b"not a picture at all, just text"),
            Err(ImportError::NotAnImage)
        );
        let mut mp4 = vec![0, 0, 0, 24];
        mp4.extend_from_slice(b"ftypisom");
        mp4.extend_from_slice(&[0; 16]);
        assert_eq!(sticker_from_image(&mp4), Err(ImportError::NotAnImage));
    }

    #[test]
    fn the_id_is_the_hash_of_the_content() {
        assert_eq!(
            content_id(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(content_id(b"abc"), content_id(b"abc"));
        assert_ne!(content_id(b"abc"), content_id(b"abd"));
    }

    #[test]
    fn a_gif_file_is_a_gif_or_an_mp4_within_the_limit() {
        let gif = crate::animation::fixtures::animated_gif(30, 20, 2, 80);
        let file = gif_file(&gif).unwrap();
        assert_eq!(file.mime, "image/gif");
        assert!(file.animated && !file.video);
        assert_eq!(file.size, Some((30, 20)));

        let mut mp4 = vec![0, 0, 0, 24];
        mp4.extend_from_slice(b"ftypisom");
        mp4.extend_from_slice(&[0; 16]);
        let file = gif_file(&mp4).unwrap();
        assert_eq!(file.mime, "video/mp4");
        assert!(file.video && file.animated);

        assert_eq!(
            gif_file(b"hello world, hello world"),
            Err(ImportError::NotAGif)
        );
        assert_eq!(gif_file(&[]), Err(ImportError::Empty));
        let mut big = mp4.clone();
        big.resize(GIF_MAX + 1, 0);
        assert_eq!(gif_file(&big), Err(ImportError::GifTooLarge));
    }
}
