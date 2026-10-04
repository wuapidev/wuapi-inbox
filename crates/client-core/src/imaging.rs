//! Turning what was downloaded into what is shown: a small image.
//!
//! Nothing that arrives from the network is trusted to be what it says it
//! is. Decoding happens here, behind limits on dimensions and memory, and
//! the result is re-encoded: what the UI gets is a JPEG or PNG this
//! process made, at most `max_side` on its longer side. That is also what
//! keeps memory small with hundreds of chats: an avatar is a few
//! kilobytes, whatever was uploaded.

use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader, Limits};
use std::io::Cursor;

/// The largest image, in pixels per side, that is decoded at all.
const MAX_DIMENSION: u32 = 12_000;
/// The most memory one decode may use.
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

/// A downscaled, re-encoded image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    /// PNG when the image has transparency (stickers), JPEG otherwise.
    pub bytes: Vec<u8>,
    /// `image/png` or `image/jpeg`.
    pub mime: &'static str,
    /// Its size in pixels: the original's proportions, at most `max_side`.
    pub width: u32,
    /// See `width`.
    pub height: u32,
}

/// A decoded image, the way up its camera meant it.
pub struct Upright {
    /// The pixels, turned as the file's EXIF orientation says, at most
    /// `max_side` on the longer side.
    pub image: DynamicImage,
    /// The size of the whole image as it is looked at (after turning,
    /// before any scaling down): what "100 %" refers to.
    pub natural: (u32, u32),
}

/// The size of an image once it is turned as `orientation` says: the
/// four orientations that lay it on its side swap width and height.
pub fn turned_size(orientation: Orientation, (width, height): (u32, u32)) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    }
}

/// Decodes `bytes` behind the same limits as [`thumbnail`], turns the
/// image upright (a phone's portrait photo is stored on its side, with a
/// note saying so) and scales it down to fit `max_side`. Blocking.
pub fn upright(bytes: &[u8], max_side: u32) -> Result<Upright, String> {
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|error| error.to_string())?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    let natural = turned_size(orientation, (image.width(), image.height()));
    let max_side = max_side.max(1);
    // Scaled down before it is turned: less to move about.
    if image.width() > max_side || image.height() > max_side {
        image = image.thumbnail(max_side, max_side);
    }
    image.apply_orientation(orientation);
    Ok(Upright { image, natural })
}

/// Decodes `bytes` (JPEG, PNG, WebP or GIF, told by content, not by any
/// header or name) and scales it down to fit `max_side`. Animated images
/// give their first frame. Blocking and CPU-bound: call it off the UI
/// thread and off the async runtime's workers.
pub fn thumbnail(bytes: &[u8], max_side: u32) -> Result<Thumbnail, String> {
    let image = upright(bytes, max_side)?.image;

    let mut out = Vec::new();
    let (format, mime, image) = if image.color().has_alpha() {
        (
            ImageFormat::Png,
            "image/png",
            DynamicImage::ImageRgba8(image.to_rgba8()),
        )
    } else {
        (
            ImageFormat::Jpeg,
            "image/jpeg",
            DynamicImage::ImageRgb8(image.to_rgb8()),
        )
    };
    image
        .write_to(&mut Cursor::new(&mut out), format)
        .map_err(|error| error.to_string())?;
    Ok(Thumbnail {
        bytes: out,
        mime,
        width: image.width(),
        height: image.height(),
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    /// A PNG of the given size, opaque.
    pub(crate) fn png(width: u32, height: u32) -> Vec<u8> {
        let image = RgbImage::from_fn(width, height, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 120])
        });
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(image)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    /// A JPEG of the given stored size whose EXIF says how to turn it.
    pub(crate) fn jpeg_turned(width: u32, height: u32, orientation: u8) -> Vec<u8> {
        // Left half red, right half blue, as stored.
        let image = RgbImage::from_fn(width, height, |x, _| {
            if x < width / 2 {
                Rgb([220, 20, 20])
            } else {
                Rgb([20, 20, 220])
            }
        });
        let mut jpeg = Vec::new();
        DynamicImage::ImageRgb8(image)
            .write_to(&mut Cursor::new(&mut jpeg), ImageFormat::Jpeg)
            .unwrap();
        // An APP1 segment right after the start marker: "Exif", a
        // little-endian TIFF header, one entry (0x0112, the orientation).
        let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0".to_vec();
        exif.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0, orientation, 0, 0, 0]);
        exif.extend_from_slice(&[0, 0, 0, 0]);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&exif);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    #[test]
    fn every_orientation_gives_the_size_it_is_looked_at() {
        use Orientation::*;
        let stored = (40, 30);
        for (code, orientation, size) in [
            (1, NoTransforms, (40, 30)),
            (2, FlipHorizontal, (40, 30)),
            (3, Rotate180, (40, 30)),
            (4, FlipVertical, (40, 30)),
            (5, Rotate90FlipH, (30, 40)),
            (6, Rotate90, (30, 40)),
            (7, Rotate270FlipH, (30, 40)),
            (8, Rotate270, (30, 40)),
        ] {
            assert_eq!(Orientation::from_exif(code), Some(orientation));
            assert_eq!(turned_size(orientation, stored), size, "{code}");
            // And the pixels really are turned to that size.
            let mut image = DynamicImage::new_rgb8(stored.0, stored.1);
            image.apply_orientation(orientation);
            assert_eq!((image.width(), image.height()), size, "{code}");
        }
    }

    #[test]
    fn a_photo_stored_on_its_side_comes_out_upright() {
        // Stored landscape, taken portrait (orientation 6: turn it a
        // quarter clockwise): what was the left is now the top.
        let photo = jpeg_turned(400, 200, 6);
        let seen = upright(&photo, 4096).unwrap();
        assert_eq!(seen.natural, (200, 400));
        let pixels = seen.image.to_rgb8();
        assert_eq!(pixels.dimensions(), (200, 400));
        assert!(pixels.get_pixel(100, 20)[0] > 150, "red on top");
        assert!(pixels.get_pixel(100, 380)[2] > 150, "blue below");
        // Scaled down, its natural size is still the whole photo's.
        let small = upright(&photo, 100).unwrap();
        assert_eq!(small.natural, (200, 400));
        assert_eq!((small.image.width(), small.image.height()), (50, 100));
        // The thumbnail is upright too, so its box in the chat is.
        let thumbnail = thumbnail(&photo, 100).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), (50, 100));
        // No note, no turning.
        let plain = upright(&png(40, 30), 4096).unwrap();
        assert_eq!(plain.natural, (40, 30));
    }

    #[test]
    fn images_are_scaled_down_and_keep_their_proportions() {
        let thumb = thumbnail(&png(1600, 800), 400).unwrap();
        assert_eq!((thumb.width, thumb.height), (400, 200));
        assert_eq!(thumb.mime, "image/jpeg");
        assert!(thumb.bytes.len() < png(1600, 800).len());
        // What comes out is a real image of that size.
        let again = image::load_from_memory(&thumb.bytes).unwrap();
        assert_eq!((again.width(), again.height()), (400, 200));

        // Small images are not blown up.
        let small = thumbnail(&png(40, 30), 400).unwrap();
        assert_eq!((small.width, small.height), (40, 30));
    }

    #[test]
    fn transparency_survives_for_stickers() {
        let sticker = RgbaImage::from_fn(64, 64, |x, _| Rgba([200, 30, 30, (x * 4) as u8]));
        let mut bytes = Vec::new();
        DynamicImage::ImageRgba8(sticker)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        let thumb = thumbnail(&bytes, 512).unwrap();
        assert_eq!(thumb.mime, "image/png");
        let again = image::load_from_memory(&thumb.bytes).unwrap();
        assert!(again.color().has_alpha());
    }

    #[test]
    fn what_is_not_an_image_is_refused_not_trusted() {
        assert!(thumbnail(b"<html>not an image</html>", 96).is_err());
        assert!(thumbnail(&[], 96).is_err());
        // A PNG header that promises more pixels than anyone should decode.
        let mut bomb = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        bomb.extend_from_slice(&[0, 0, 0, 13]);
        bomb.extend_from_slice(b"IHDR");
        bomb.extend_from_slice(&60_000u32.to_be_bytes());
        bomb.extend_from_slice(&60_000u32.to_be_bytes());
        bomb.extend_from_slice(&[8, 2, 0, 0, 0, 0, 0, 0, 0]);
        assert!(thumbnail(&bomb, 96).is_err());
        // A truncated file.
        let whole = png(200, 200);
        assert!(thumbnail(&whole[..whole.len() / 2], 96).is_err());
    }
}
