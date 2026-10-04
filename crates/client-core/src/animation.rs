//! Animated stickers and GIFs: the frames of an animated WebP or GIF, with
//! how long each one stays.
//!
//! Like every image that arrives from the network, an animation is not
//! trusted to be what it says. It is decoded here, behind limits: how
//! large its canvas may be, how many frames it may have, and how much
//! memory the decoded frames may take together. Frames are scaled down as
//! they are decoded, and when a long animation would not fit the memory it
//! is allowed, all of it is made smaller rather than its end cut off; only
//! at the smallest size is the rest left out. Blocking and CPU-bound: call
//! it off the UI thread and off the async runtime's workers.

use image::codecs::gif::GifDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, ImageDecoder, Limits, RgbaImage};
use std::io::Cursor;
use std::time::Duration;

/// How long a frame stays when its file says "no time at all": what
/// browsers do with such files.
const DEFAULT_DELAY: Duration = Duration::from_millis(100);
/// Anything shorter is taken for "no time at all".
const SHORTEST_DELAY: Duration = Duration::from_millis(11);
/// No frame stays longer than this.
const LONGEST_DELAY: Duration = Duration::from_secs(10);

/// What an animation may cost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationLimits {
    /// The largest canvas, per side, that is decoded at all.
    pub max_canvas: u32,
    /// Frames are scaled down to fit this side.
    pub max_side: u32,
    /// When memory runs out, frames are halved down to this side before
    /// any is left out.
    pub min_side: u32,
    /// The most frames that are kept.
    pub max_frames: usize,
    /// The most memory the decoded frames may take together.
    pub max_bytes: usize,
}

impl Default for AnimationLimits {
    /// A sticker in a conversation: 512 px on WhatsApp, shown at about a
    /// third of that.
    fn default() -> Self {
        Self {
            max_canvas: 2048,
            max_side: 320,
            min_side: 96,
            max_frames: 240,
            max_bytes: 24 * 1024 * 1024,
        }
    }
}

/// One frame: the whole picture at that moment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimationFrame {
    /// The pixels, RGBA.
    pub image: RgbaImage,
    /// How long it stays.
    pub delay: Duration,
}

/// The decoded frames of an animated image. All frames have one size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnimationFrames {
    /// At least two frames.
    pub frames: Vec<AnimationFrame>,
    /// Frames the file had that were left out for the limits.
    pub truncated: bool,
}

impl AnimationFrames {
    /// The memory the pixels take.
    pub fn bytes(&self) -> usize {
        self.frames
            .iter()
            .map(|frame| frame.image.as_raw().len())
            .sum()
    }

    /// One turn of the loop.
    pub fn duration(&self) -> Duration {
        self.frames.iter().map(|frame| frame.delay).sum()
    }
}

/// The formats that can move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnimatedFormat {
    /// An animated WebP: what WhatsApp's animated stickers are.
    WebP,
    /// A GIF file.
    Gif,
}

impl AnimatedFormat {
    /// Its MIME type.
    pub fn mime(self) -> &'static str {
        match self {
            Self::WebP => "image/webp",
            Self::Gif => "image/gif",
        }
    }
}

/// Whether `bytes` are a WebP or a GIF that says it has more than one
/// frame, told from the content. Cheap: nothing is decoded for a WebP, and
/// a GIF is only walked block by block.
pub fn animated_format(bytes: &[u8]) -> Option<AnimatedFormat> {
    if bytes.len() >= 30 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        // The extended header's flags: bit 1 says "animation".
        let animated = &bytes[12..16] == b"VP8X" && bytes[20] & 0x02 != 0;
        return animated.then_some(AnimatedFormat::WebP);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return (gif_images(bytes, 2) >= 2).then_some(AnimatedFormat::Gif);
    }
    None
}

/// How many images a GIF holds, counted up to `enough`, without decoding
/// any. Anything malformed ends the count.
fn gif_images(bytes: &[u8], enough: usize) -> usize {
    let table = |packed: u8| -> usize {
        if packed & 0x80 != 0 {
            3 * (1usize << ((packed & 0x07) + 1))
        } else {
            0
        }
    };
    let Some(&packed) = bytes.get(10) else {
        return 0;
    };
    let mut at = 13 + table(packed);
    let mut images = 0;
    // Skips a run of sub-blocks; `None` when the file ends inside it.
    let skip_blocks = |mut at: usize| -> Option<usize> {
        loop {
            let size = *bytes.get(at)? as usize;
            at += 1;
            if size == 0 {
                return Some(at);
            }
            at += size;
        }
    };
    while images < enough {
        match bytes.get(at) {
            // An extension: a label, then sub-blocks.
            Some(0x21) => match skip_blocks(at + 2) {
                Some(next) => at = next,
                None => break,
            },
            // An image: its descriptor, maybe a colour table, the code
            // size, then sub-blocks.
            Some(0x2c) => {
                let Some(&packed) = bytes.get(at + 9) else {
                    break;
                };
                images += 1;
                match skip_blocks(at + 10 + table(packed) + 1) {
                    Some(next) => at = next,
                    None => break,
                }
            }
            _ => break,
        }
    }
    images
}

fn delay_of(frame: &image::Frame) -> Duration {
    let (numer, denom) = frame.delay().numer_denom_ms();
    let delay = Duration::from_micros(u64::from(numer) * 1_000 / u64::from(denom.max(1)));
    if delay < SHORTEST_DELAY {
        DEFAULT_DELAY
    } else {
        delay.min(LONGEST_DELAY)
    }
}

fn fitted(image: RgbaImage, side: u32) -> RgbaImage {
    if image.width() <= side && image.height() <= side {
        return image;
    }
    image::imageops::thumbnail(
        &image,
        (u64::from(image.width()) * u64::from(side) / u64::from(image.width().max(image.height())))
            .max(1) as u32,
        (u64::from(image.height()) * u64::from(side) / u64::from(image.width().max(image.height())))
            .max(1) as u32,
    )
}

/// Decodes the frames of an animated WebP or GIF within `limits`.
/// `Ok(None)` for an image that does not move (one frame, or a format
/// that cannot); an error for what cannot be read or is larger than
/// anyone should decode.
pub fn animation_frames(
    bytes: &[u8],
    limits: &AnimationLimits,
) -> Result<Option<AnimationFrames>, String> {
    let Some(format) = animated_format(bytes) else {
        return Ok(None);
    };
    let mut decode = Limits::default();
    decode.max_image_width = Some(limits.max_canvas);
    decode.max_image_height = Some(limits.max_canvas);
    // One canvas and the decoder's own buffers, not the whole animation.
    decode.max_alloc = Some(u64::from(limits.max_canvas) * u64::from(limits.max_canvas) * 4 * 3);
    let text = |error: image::ImageError| error.to_string();
    let frames: image::Frames<'_> = match format {
        AnimatedFormat::WebP => {
            let mut decoder = WebPDecoder::new(Cursor::new(bytes)).map_err(text)?;
            decoder.set_limits(decode).map_err(text)?;
            if !decoder.has_animation() {
                return Ok(None);
            }
            decoder.into_frames()
        }
        AnimatedFormat::Gif => {
            let mut decoder = GifDecoder::new(Cursor::new(bytes)).map_err(text)?;
            decoder.set_limits(decode).map_err(text)?;
            decoder.into_frames()
        }
    };

    let mut side = limits.max_side.max(1);
    let mut kept: Vec<AnimationFrame> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;
    for frame in frames {
        // A frame that cannot be read ends the animation where it stands:
        // what was decoded before it is still an animation.
        let Ok(frame) = frame else {
            truncated = true;
            break;
        };
        if kept.len() >= limits.max_frames {
            truncated = true;
            break;
        }
        let delay = delay_of(&frame);
        let mut image = fitted(frame.into_buffer(), side);
        // Out of memory for this many frames at this size: everything
        // gets smaller, down to the smallest size worth showing.
        while used + image.as_raw().len() > limits.max_bytes && side / 2 >= limits.min_side.max(1) {
            side /= 2;
            used = 0;
            for earlier in &mut kept {
                earlier.image = fitted(std::mem::take(&mut earlier.image), side);
                used += earlier.image.as_raw().len();
            }
            image = fitted(image, side);
        }
        if used + image.as_raw().len() > limits.max_bytes {
            truncated = true;
            break;
        }
        used += image.as_raw().len();
        kept.push(AnimationFrame { image, delay });
    }
    if kept.len() < 2 {
        return Ok(None);
    }
    Ok(Some(AnimationFrames {
        frames: kept,
        truncated,
    }))
}

/// Small animated files made on the spot, for tests: this crate's own and
/// those of whoever shows what it decodes.
#[cfg(any(test, feature = "fixtures"))]
pub mod fixtures {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::codecs::webp::WebPEncoder;
    use image::{Delay, ExtendedColorType, Frame, Rgba, RgbaImage};

    /// Frame `index` of a test animation: one flat colour that tells the
    /// frames apart, on a transparent corner.
    pub fn frame(width: u32, height: u32, index: u32) -> RgbaImage {
        RgbaImage::from_fn(width, height, |x, y| {
            if x < 2 && y < 2 {
                Rgba([0, 0, 0, 0])
            } else {
                Rgba([
                    (40 * index % 256) as u8,
                    200,
                    (255 - 30 * index % 256) as u8,
                    255,
                ])
            }
        })
    }

    /// An animated GIF of `frames` frames, each staying `delay_ms`.
    pub fn animated_gif(width: u32, height: u32, frames: u32, delay_ms: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut out);
            encoder.set_repeat(Repeat::Infinite).unwrap();
            for index in 0..frames {
                encoder
                    .encode_frame(Frame::from_parts(
                        frame(width, height, index),
                        0,
                        0,
                        Delay::from_numer_denom_ms(delay_ms, 1),
                    ))
                    .unwrap();
            }
        }
        out
    }

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = kind.to_vec();
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn u24(value: u32) -> [u8; 3] {
        let bytes = value.to_le_bytes();
        [bytes[0], bytes[1], bytes[2]]
    }

    /// An animated WebP, as WhatsApp's animated stickers are: the extended
    /// container with one lossless frame after the other, each staying as
    /// long as `delays_ms` says.
    pub fn animated_webp(width: u32, height: u32, delays_ms: &[u32]) -> Vec<u8> {
        let mut body = Vec::new();
        // VP8X: the animation and alpha flags, then the canvas, less one.
        let mut header = vec![0x02 | 0x10, 0, 0, 0];
        header.extend_from_slice(&u24(width - 1));
        header.extend_from_slice(&u24(height - 1));
        body.extend(chunk(b"VP8X", &header));
        // ANIM: background colour, loop count (0: for ever).
        body.extend(chunk(b"ANIM", &[0, 0, 0, 0, 0, 0]));
        for (index, delay) in delays_ms.iter().enumerate() {
            // One still, lossless WebP; its bitstream chunk is the frame.
            let mut still = Vec::new();
            WebPEncoder::new_lossless(&mut still)
                .encode(
                    frame(width, height, index as u32).as_raw(),
                    width,
                    height,
                    ExtendedColorType::Rgba8,
                )
                .unwrap();
            let bitstream = &still[12..];
            let mut data = Vec::new();
            data.extend_from_slice(&u24(0)); // x / 2
            data.extend_from_slice(&u24(0)); // y / 2
            data.extend_from_slice(&u24(width - 1));
            data.extend_from_slice(&u24(height - 1));
            data.extend_from_slice(&u24(*delay));
            data.push(0x02); // do not blend with what was there
            data.extend_from_slice(bitstream);
            body.extend(chunk(b"ANMF", &data));
        }
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32 + 4).to_le_bytes());
        out.extend_from_slice(b"WEBP");
        out.extend(body);
        out
    }

    /// A WebP that does not move: one lossless frame.
    pub fn still_webp(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        WebPEncoder::new_lossless(&mut out)
            .encode(
                frame(width, height, 0).as_raw(),
                width,
                height,
                ExtendedColorType::Rgba8,
            )
            .unwrap();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn decoded(bytes: &[u8], limits: &AnimationLimits) -> AnimationFrames {
        animation_frames(bytes, limits)
            .expect("readable")
            .expect("an animation")
    }

    #[test]
    fn an_animated_webp_gives_its_frames_and_how_long_each_stays() {
        let bytes = animated_webp(64, 48, &[80, 120, 40]);
        assert_eq!(animated_format(&bytes), Some(AnimatedFormat::WebP));
        let animation = decoded(&bytes, &AnimationLimits::default());
        assert_eq!(animation.frames.len(), 3);
        assert!(!animation.truncated);
        let delays: Vec<u64> = animation
            .frames
            .iter()
            .map(|frame| frame.delay.as_millis() as u64)
            .collect();
        assert_eq!(delays, [80, 120, 40]);
        assert_eq!(animation.duration(), Duration::from_millis(240));
        for (index, shown) in animation.frames.iter().enumerate() {
            assert_eq!(shown.image.dimensions(), (64, 48));
            // Each frame is the picture that was encoded, corner included.
            assert_eq!(shown.image, frame(64, 48, index as u32), "frame {index}");
        }
    }

    #[test]
    fn an_animated_gif_gives_its_frames_too() {
        let bytes = animated_gif(40, 40, 4, 70);
        assert_eq!(animated_format(&bytes), Some(AnimatedFormat::Gif));
        let animation = decoded(&bytes, &AnimationLimits::default());
        assert_eq!(animation.frames.len(), 4);
        assert!(animation
            .frames
            .iter()
            .all(|frame| frame.delay == Duration::from_millis(70)));
        assert_eq!(animation.frames[0].image.dimensions(), (40, 40));
    }

    #[test]
    fn a_picture_that_does_not_move_is_not_an_animation() {
        for still in [
            still_webp(32, 32),
            animated_gif(32, 32, 1, 100),
            crate::imaging::tests::png(32, 32),
            b"<html>not an image</html>".to_vec(),
            Vec::new(),
        ] {
            assert_eq!(animated_format(&still), None);
            assert_eq!(
                animation_frames(&still, &AnimationLimits::default()),
                Ok(None)
            );
        }
    }

    #[test]
    fn a_frame_with_no_time_gets_the_usual_tenth_of_a_second() {
        let animation = decoded(
            &animated_webp(16, 16, &[0, 5, 30, 60_000]),
            &AnimationLimits::default(),
        );
        let delays: Vec<Duration> = animation.frames.iter().map(|frame| frame.delay).collect();
        assert_eq!(
            delays,
            [
                DEFAULT_DELAY,
                DEFAULT_DELAY,
                Duration::from_millis(30),
                LONGEST_DELAY
            ]
        );
    }

    #[test]
    fn frames_are_scaled_down_to_the_side_they_are_shown_at() {
        let limits = AnimationLimits {
            max_side: 50,
            ..Default::default()
        };
        let animation = decoded(&animated_webp(200, 100, &[50, 50]), &limits);
        assert_eq!(animation.frames[0].image.dimensions(), (50, 25));
        assert_eq!(animation.bytes(), 2 * 50 * 25 * 4);
    }

    #[test]
    fn a_long_animation_gets_smaller_before_it_gets_shorter() {
        // Twelve frames of 128 px are 786 kB; 300 kB are allowed.
        let bytes = animated_webp(128, 128, &[40; 12]);
        let limits = AnimationLimits {
            max_side: 128,
            min_side: 32,
            max_bytes: 300_000,
            ..Default::default()
        };
        let animation = decoded(&bytes, &limits);
        assert_eq!(animation.frames.len(), 12, "every frame is kept");
        assert!(!animation.truncated);
        assert_eq!(animation.frames[0].image.dimensions(), (64, 64));
        assert!(animation.bytes() <= limits.max_bytes);
        let sizes: Vec<_> = animation
            .frames
            .iter()
            .map(|frame| frame.image.dimensions())
            .collect();
        assert!(sizes.iter().all(|size| *size == sizes[0]), "one size");

        // Nothing smaller is allowed: the end is left out, and said.
        let tight = AnimationLimits {
            max_side: 128,
            min_side: 128,
            max_bytes: 300_000,
            ..Default::default()
        };
        let animation = decoded(&bytes, &tight);
        assert_eq!(animation.frames.len(), 4);
        assert!(animation.truncated);
        assert!(animation.bytes() <= tight.max_bytes);
    }

    #[test]
    fn the_number_of_frames_is_bounded() {
        let limits = AnimationLimits {
            max_frames: 5,
            ..Default::default()
        };
        let animation = decoded(&animated_gif(16, 16, 9, 30), &limits);
        assert_eq!(animation.frames.len(), 5);
        assert!(animation.truncated);
    }

    #[test]
    fn a_canvas_larger_than_anyone_should_decode_is_refused() {
        let limits = AnimationLimits {
            max_canvas: 64,
            ..Default::default()
        };
        assert!(animation_frames(&animated_webp(200, 20, &[50, 50]), &limits).is_err());
        assert!(animation_frames(&animated_gif(20, 200, 2, 50), &limits).is_err());
        // A header that promises an enormous canvas, with nothing behind.
        let mut bomb = animated_webp(16, 16, &[50, 50]);
        bomb[24..27].copy_from_slice(&[0xff, 0xff, 0xff]);
        bomb[27..30].copy_from_slice(&[0xff, 0xff, 0xff]);
        assert!(animation_frames(&bomb, &AnimationLimits::default()).is_err());
    }

    #[test]
    fn a_file_cut_short_keeps_what_was_read_or_says_it_cannot_be_read() {
        let whole = animated_webp(32, 32, &[50; 6]);
        let cut = &whole[..whole.len() * 2 / 3];
        // Never a panic, never more than the limits.
        if let Ok(Some(animation)) = animation_frames(cut, &AnimationLimits::default()) {
            assert!(animation.frames.len() >= 2 && animation.frames.len() < 6);
        }
        let gif = animated_gif(32, 32, 6, 50);
        let _ = animation_frames(&gif[..gif.len() / 2], &AnimationLimits::default());
        assert_eq!(gif_images(&gif[..20], 2), 0);
    }
}
