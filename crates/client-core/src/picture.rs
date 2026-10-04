//! Pictures the user chooses: their own profile picture, a group's.
//!
//! WhatsApp wants a square JPEG. Whatever file was picked is decoded
//! behind limits, cropped to its centre square, scaled down and encoded
//! again, so what is uploaded is always an image this process made.

use image::imageops::FilterType;
use image::{DynamicImage, ImageFormat, ImageReader, Limits, Rgb, RgbImage};
use std::io::Cursor;

/// The side of an uploaded picture, in pixels.
pub const PICTURE_SIDE: u32 = 640;
/// The largest file accepted as a picture.
pub const PICTURE_MAX_FILE: u64 = 25 * 1024 * 1024;
const MAX_DIMENSION: u32 = 12_000;
const MAX_DECODE_BYTES: u64 = 256 * 1024 * 1024;

/// Turns an image file (JPEG, PNG, WebP or GIF, told by content) into the
/// square JPEG a profile or group picture is uploaded as. Transparent
/// parts become white. Blocking and CPU-bound: call it off the UI thread.
pub fn profile_picture(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() as u64 > PICTURE_MAX_FILE {
        return Err("The file is larger than 25 MB.".to_owned());
    }
    let not_an_image = |_| "The file is not an image this app can read.".to_owned();
    let mut reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_DIMENSION);
    limits.max_image_height = Some(MAX_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_BYTES);
    reader.limits(limits);
    let image = reader.decode().map_err(not_an_image)?;
    let side = image.width().min(image.height());
    if side == 0 {
        return Err("The image is empty.".to_owned());
    }
    let square = image.crop_imm(
        (image.width() - side) / 2,
        (image.height() - side) / 2,
        side,
        side,
    );
    let square = if side > PICTURE_SIDE {
        square.resize_exact(PICTURE_SIDE, PICTURE_SIDE, FilterType::Lanczos3)
    } else {
        square
    };
    // JPEG has no transparency: on white, as WhatsApp shows it.
    let rgba = square.to_rgba8();
    let mut flat = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = u32::from(pixel[3]);
        let over = |channel: u8| ((u32::from(channel) * alpha + 255 * (255 - alpha)) / 255) as u8;
        flat.put_pixel(x, y, Rgb([over(pixel[0]), over(pixel[1]), over(pixel[2])]));
    }
    let mut out = Vec::new();
    DynamicImage::ImageRgb8(flat)
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Jpeg)
        .map_err(|error| error.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32, alpha: u8) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([10, 200, 30, alpha]));
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(image)
            .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn any_image_becomes_a_square_jpeg_of_at_most_640() {
        let made = profile_picture(&png(1600, 900, 255)).unwrap();
        assert_eq!(&made[..3], [0xFF, 0xD8, 0xFF], "a JPEG");
        let back = image::load_from_memory(&made).unwrap();
        assert_eq!((back.width(), back.height()), (640, 640));

        // A small one is cropped, not blown up.
        let small =
            image::load_from_memory(&profile_picture(&png(120, 300, 255)).unwrap()).unwrap();
        assert_eq!((small.width(), small.height()), (120, 120));
    }

    #[test]
    fn transparency_becomes_white_and_what_is_no_image_is_refused() {
        let made = profile_picture(&png(64, 64, 0)).unwrap();
        let back = image::load_from_memory(&made).unwrap().to_rgb8();
        let pixel = back.get_pixel(32, 32);
        assert!(pixel.0.iter().all(|channel| *channel > 240), "{pixel:?}");

        assert!(profile_picture(b"not an image at all").is_err());
        assert!(profile_picture(&[]).is_err());
    }
}
