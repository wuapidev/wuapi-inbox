//! A picture retouched before it is sent: turned, cropped, drawn on,
//! written on, a part of it hidden.
//!
//! Nothing here draws on the screen. A retouch is a list of [`Edit`]s made
//! on the picture as it came; the picture that is sent is the original
//! with each of them applied in order ([`apply_all`]), so taking the last
//! one back is the same list without it, and nothing is lost by encoding
//! twice. Every measure of an edit is in the pixels of the picture as it
//! is when that edit is made (after the turns and crops before it).
//!
//! Slow work for a large picture (decoding, a few edits, encoding): it is
//! called off the interface's thread.

use ab_glyph::{Font as _, FontRef, PxScale, ScaleFont as _};
use image::{Rgba, RgbaImage};

/// The longest side a picture is retouched at. A larger one is scaled
/// down first: it would be on its way to WhatsApp too.
pub const MAX_SIDE: u32 = 4096;

/// A colour to draw with: red, green, blue, opaque.
pub type Ink = [u8; 3];

/// The colours offered, the first the one a tool starts with.
pub const INKS: [Ink; 7] = [
    [0xF0, 0x3E, 0x3E],
    [0xFF, 0xB0, 0x20],
    [0xC8, 0xF0, 0x3C],
    [0x2F, 0x9E, 0xF5],
    [0xB0, 0x5C, 0xF0],
    [0xFF, 0xFF, 0xFF],
    [0x11, 0x11, 0x11],
];

/// A rectangle of the picture, in its pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

impl Area {
    /// The rectangle between two corners, whichever way it was dragged.
    pub fn between(a: (f32, f32), b: (f32, f32)) -> Self {
        Self {
            left: a.0.min(b.0),
            top: a.1.min(b.1),
            right: a.0.max(b.0),
            bottom: a.1.max(b.1),
        }
    }

    /// The whole pixels of a picture of `size` it covers; `None` when
    /// that is less than a few pixels a side (a click, not a drag).
    pub fn pixels(self, size: (u32, u32)) -> Option<(u32, u32, u32, u32)> {
        let left = self.left.round().clamp(0., size.0 as f32) as u32;
        let top = self.top.round().clamp(0., size.1 as f32) as u32;
        let right = self.right.round().clamp(0., size.0 as f32) as u32;
        let bottom = self.bottom.round().clamp(0., size.1 as f32) as u32;
        (right >= left + 4 && bottom >= top + 4).then(|| (left, top, right - left, bottom - top))
    }
}

/// One change to the picture.
#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// A quarter turn, clockwise.
    Turn,
    /// Only this part is kept.
    Crop(Area),
    /// A line drawn by hand, through these points, this thick.
    Stroke {
        points: Vec<(f32, f32)>,
        width: f32,
        ink: Ink,
    },
    /// The outline of a rectangle.
    Frame { area: Area, width: f32, ink: Ink },
    /// This part is hidden: turned into large blocks of its own colours.
    Hide(Area),
    /// Words, their top left corner at `at`, their letters `size` tall.
    Words {
        at: (f32, f32),
        text: String,
        size: f32,
        ink: Ink,
    },
}

/// The thickness of a line and the height of letters that suit a picture
/// of this size: the same on screen whatever the picture's pixels.
pub fn stroke_for(size: (u32, u32)) -> f32 {
    (size.0.max(size.1) as f32 / 240.).clamp(2., 24.)
}

/// The height of the letters for a picture of this size.
pub fn letters_for(size: (u32, u32)) -> f32 {
    (size.0.max(size.1) as f32 / 22.).clamp(14., 220.)
}

/// Decodes a picture to retouch: upright, no larger than [`MAX_SIDE`].
pub fn open(bytes: &[u8]) -> Result<RgbaImage, String> {
    client_core::upright(bytes, MAX_SIDE)
        .map(|upright| upright.image.into_rgba8())
        .map_err(|error| format!("The picture could not be read: {error}"))
}

/// The picture with every edit made, in order.
pub fn apply_all(mut image: RgbaImage, edits: &[Edit]) -> RgbaImage {
    for edit in edits {
        image = apply(image, edit);
    }
    image
}

fn apply(mut image: RgbaImage, edit: &Edit) -> RgbaImage {
    let size = image.dimensions();
    match edit {
        Edit::Turn => image::imageops::rotate90(&image),
        Edit::Crop(area) => match area.pixels(size) {
            Some((left, top, width, height)) => {
                image::imageops::crop_imm(&image, left, top, width, height).to_image()
            }
            None => image,
        },
        Edit::Stroke { points, width, ink } => {
            let radius = width / 2.;
            match points.as_slice() {
                [] => {}
                [only] => segment(&mut image, *only, *only, radius, *ink),
                points => {
                    for pair in points.windows(2) {
                        segment(&mut image, pair[0], pair[1], radius, *ink);
                    }
                }
            }
            image
        }
        Edit::Frame { area, width, ink } => {
            let radius = width / 2.;
            let corners = [
                (area.left, area.top),
                (area.right, area.top),
                (area.right, area.bottom),
                (area.left, area.bottom),
            ];
            for side in 0..4 {
                segment(
                    &mut image,
                    corners[side],
                    corners[(side + 1) % 4],
                    radius,
                    *ink,
                );
            }
            image
        }
        Edit::Hide(area) => {
            if let Some((left, top, width, height)) = area.pixels(size) {
                hide(&mut image, left, top, width, height);
            }
            image
        }
        Edit::Words {
            at,
            text,
            size,
            ink,
        } => {
            words(&mut image, *at, text, *size, *ink);
            image
        }
    }
}

/// Lays `ink` over a pixel, as much as `cover` (0 to 1) says.
fn lay(image: &mut RgbaImage, x: i64, y: i64, ink: Ink, cover: f32) {
    if x < 0 || y < 0 || x >= i64::from(image.width()) || y >= i64::from(image.height()) {
        return;
    }
    let cover = cover.clamp(0., 1.);
    if cover <= 0. {
        return;
    }
    let Rgba(under) = *image.get_pixel(x as u32, y as u32);
    let mix = |under: u8, over: u8| {
        (f32::from(under) + (f32::from(over) - f32::from(under)) * cover).round() as u8
    };
    image.put_pixel(
        x as u32,
        y as u32,
        Rgba([
            mix(under[0], ink[0]),
            mix(under[1], ink[1]),
            mix(under[2], ink[2]),
            under[3].max((cover * 255.).round() as u8),
        ]),
    );
}

/// A line from `a` to `b` with round ends, `radius` to each side, its
/// edge softened over one pixel.
fn segment(image: &mut RgbaImage, a: (f32, f32), b: (f32, f32), radius: f32, ink: Ink) {
    let radius = radius.max(0.5);
    let reach = radius + 1.;
    let (from_x, to_x) = (
        (a.0.min(b.0) - reach).floor() as i64,
        (a.0.max(b.0) + reach).ceil() as i64,
    );
    let (from_y, to_y) = (
        (a.1.min(b.1) - reach).floor() as i64,
        (a.1.max(b.1) + reach).ceil() as i64,
    );
    let (along_x, along_y) = (b.0 - a.0, b.1 - a.1);
    let length = along_x * along_x + along_y * along_y;
    for y in from_y.max(0)..=to_y.min(i64::from(image.height()) - 1) {
        for x in from_x.max(0)..=to_x.min(i64::from(image.width()) - 1) {
            // The middle of the pixel, and the nearest point of the line.
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let along = if length > 0. {
                (((px - a.0) * along_x + (py - a.1) * along_y) / length).clamp(0., 1.)
            } else {
                0.
            };
            let (near_x, near_y) = (a.0 + along_x * along, a.1 + along_y * along);
            let away = ((px - near_x).powi(2) + (py - near_y).powi(2)).sqrt();
            lay(image, x, y, ink, radius + 0.5 - away);
        }
    }
}

/// Turns a part of the picture into blocks of its own average colours,
/// large enough that a face or a number in it cannot be read.
fn hide(image: &mut RgbaImage, left: u32, top: u32, width: u32, height: u32) {
    let block = (image.width().max(image.height()) / 64).clamp(8, 64);
    let mut y = top;
    while y < top + height {
        let tall = block.min(top + height - y);
        let mut x = left;
        while x < left + width {
            let wide = block.min(left + width - x);
            let mut sum = [0u64; 4];
            for inner_y in y..y + tall {
                for inner_x in x..x + wide {
                    let Rgba(pixel) = *image.get_pixel(inner_x, inner_y);
                    for (total, part) in sum.iter_mut().zip(pixel) {
                        *total += u64::from(part);
                    }
                }
            }
            let count = u64::from(wide * tall);
            let mean = Rgba(sum.map(|total| (total / count) as u8));
            for inner_y in y..y + tall {
                for inner_x in x..x + wide {
                    image.put_pixel(inner_x, inner_y, mean);
                }
            }
            x += wide;
        }
        y += tall;
    }
}

/// The face words are written in: the interface's own, semibold.
fn face() -> Option<FontRef<'static>> {
    FontRef::try_from_slice(include_bytes!("../assets/fonts/Inter-SemiBold.ttf")).ok()
}

/// Writes `text` with its top left corner at `at`, a line under the
/// other, with a thin dark edge so that it reads on any picture.
fn words(image: &mut RgbaImage, at: (f32, f32), text: &str, size: f32, ink: Ink) {
    let Some(face) = face() else { return };
    let scaled = face.as_scaled(PxScale::from(size.max(4.)));
    let line = scaled.height() + scaled.line_gap();
    // Light letters get a dark edge, dark ones a light edge.
    let light = u32::from(ink[0]) + u32::from(ink[1]) + u32::from(ink[2]) > 380;
    let edge: Ink = if light { [0, 0, 0] } else { [255, 255, 255] };
    let spread = (size / 28.).clamp(1., 4.);
    for (row, words) in text.lines().enumerate() {
        let baseline = at.1 + scaled.ascent() + line * row as f32;
        for (colour, offsets) in [
            (
                edge,
                vec![(-spread, 0.), (spread, 0.), (0., -spread), (0., spread)],
            ),
            (ink, vec![(0., 0.)]),
        ] {
            for (by_x, by_y) in offsets {
                let mut pen = at.0;
                let mut last = None;
                for letter in words.chars() {
                    let id = scaled.glyph_id(letter);
                    if let Some(last) = last {
                        pen += scaled.kern(last, id);
                    }
                    let glyph = id.with_scale_and_position(
                        scaled.scale(),
                        ab_glyph::point(pen + by_x, baseline + by_y),
                    );
                    pen += scaled.h_advance(id);
                    last = Some(id);
                    if let Some(outline) = face.outline_glyph(glyph) {
                        let bounds = outline.px_bounds();
                        let cover_of = if colour == edge { 0.55 } else { 1. };
                        outline.draw(|x, y, cover| {
                            lay(
                                image,
                                bounds.min.x as i64 + i64::from(x),
                                bounds.min.y as i64 + i64::from(y),
                                colour,
                                cover * cover_of,
                            );
                        });
                    }
                }
            }
        }
    }
}

/// The picture as a file to send: a PNG when it came as one (a
/// screenshot keeps its sharp edges) or has something see-through, a
/// JPEG otherwise. With its MIME type.
pub fn encode(image: &RgbaImage, came_as: &str) -> Result<(Vec<u8>, &'static str), String> {
    let see_through = image.pixels().any(|pixel| pixel.0[3] < 255);
    let mut out = Vec::new();
    let failed = |error: image::ImageError| format!("The picture could not be saved: {error}");
    if came_as == "image/png" || see_through {
        image
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(failed)?;
        return Ok((out, "image/png"));
    }
    let opaque = image::DynamicImage::ImageRgba8(image.clone()).into_rgb8();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92)
        .encode_image(&opaque)
        .map_err(failed)?;
    Ok((out, "image/jpeg"))
}

/// A file's name for the type it is sent as now: `photo.webp` retouched
/// goes as `photo.jpg`.
pub fn renamed(name: &str, mime: &str) -> String {
    let ending = if mime == "image/png" { "png" } else { "jpg" };
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name,
    };
    format!("{stem}.{ending}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn white(width: u32, height: u32) -> RgbaImage {
        RgbaImage::from_pixel(width, height, Rgba([255, 255, 255, 255]))
    }

    const RED: Ink = [255, 0, 0];

    #[test]
    fn a_turn_and_a_crop_change_the_size_and_nothing_else_does() {
        let edits = [
            Edit::Turn,
            Edit::Crop(Area::between((10., 20.), (60., 100.))),
            Edit::Hide(Area::between((0., 0.), (20., 20.))),
        ];
        let made = apply_all(white(300, 200), &edits);
        assert_eq!(made.dimensions(), (50, 80));
        // A crop that is a click keeps the picture whole.
        let click = [Edit::Crop(Area::between((10., 10.), (11., 12.)))];
        assert_eq!(apply_all(white(30, 20), &click).dimensions(), (30, 20));
    }

    #[test]
    fn a_turn_is_clockwise() {
        let mut image = white(4, 2);
        image.put_pixel(0, 0, Rgba([255, 0, 0, 255]));
        let turned = apply_all(image, &[Edit::Turn]);
        assert_eq!(turned.dimensions(), (2, 4));
        // The top left corner goes to the top right.
        assert_eq!(turned.get_pixel(1, 0).0, [255, 0, 0, 255]);
    }

    #[test]
    fn a_line_is_drawn_where_it_was_dragged_and_no_wider() {
        let made = apply_all(
            white(100, 60),
            &[Edit::Stroke {
                points: vec![(10., 30.), (90., 30.)],
                width: 6.,
                ink: RED,
            }],
        );
        assert_eq!(made.get_pixel(50, 30).0, [255, 0, 0, 255]);
        assert_eq!(made.get_pixel(50, 28).0, [255, 0, 0, 255]);
        assert_eq!(made.get_pixel(50, 20).0, [255, 255, 255, 255]);
        assert_eq!(made.get_pixel(50, 40).0, [255, 255, 255, 255]);
        // Round ends: a little past the first point, nothing far past it.
        assert_eq!(made.get_pixel(8, 30).0, [255, 0, 0, 255]);
        assert_eq!(made.get_pixel(2, 30).0, [255, 255, 255, 255]);
        // A click is a dot.
        let dot = apply_all(
            white(20, 20),
            &[Edit::Stroke {
                points: vec![(10., 10.)],
                width: 6.,
                ink: RED,
            }],
        );
        assert_eq!(dot.get_pixel(10, 10).0, [255, 0, 0, 255]);
        assert_eq!(dot.get_pixel(16, 10).0, [255, 255, 255, 255]);
    }

    #[test]
    fn a_frame_is_an_outline() {
        let made = apply_all(
            white(100, 100),
            &[Edit::Frame {
                area: Area::between((80., 80.), (20., 20.)),
                width: 4.,
                ink: RED,
            }],
        );
        assert_eq!(made.get_pixel(50, 20).0, [255, 0, 0, 255]);
        assert_eq!(made.get_pixel(20, 50).0, [255, 0, 0, 255]);
        assert_eq!(made.get_pixel(50, 50).0, [255, 255, 255, 255]);
    }

    #[test]
    fn a_hidden_part_cannot_be_read_and_the_rest_is_untouched() {
        // Stripes one pixel wide, black and white.
        let mut image = RgbaImage::from_fn(128, 64, |x, _| {
            if x % 2 == 0 {
                Rgba([0, 0, 0, 255])
            } else {
                Rgba([255, 255, 255, 255])
            }
        });
        image = apply_all(image, &[Edit::Hide(Area::between((0., 0.), (64., 64.)))]);
        let hidden = image.get_pixel(10, 10).0;
        assert!(hidden[0] > 100 && hidden[0] < 155, "{hidden:?}");
        assert_eq!(image.get_pixel(11, 10).0, hidden);
        assert_eq!(image.get_pixel(100, 10).0, [0, 0, 0, 255]);
        assert_eq!(image.get_pixel(101, 10).0, [255, 255, 255, 255]);
    }

    #[test]
    fn words_are_written_where_they_were_put() {
        let made = apply_all(
            white(300, 100),
            &[Edit::Words {
                at: (20., 20.),
                text: "HI".into(),
                size: 48.,
                ink: RED,
            }],
        );
        let red = |x0: u32, x1: u32| {
            (x0..x1)
                .flat_map(|x| (0..100).map(move |y| (x, y)))
                .filter(|(x, y)| made.get_pixel(*x, *y).0 == [255, 0, 0, 255])
                .count()
        };
        assert!(red(20, 120) > 200, "the letters are there");
        assert_eq!(red(0, 14), 0, "nothing before them");
        assert_eq!(red(200, 300), 0, "nothing far after them");
    }

    #[test]
    fn a_screenshot_stays_a_png_and_a_photo_a_jpeg() {
        let image = white(40, 30);
        let (bytes, mime) = encode(&image, "image/png").unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Png
        );
        let (bytes, mime) = encode(&image, "image/webp").unwrap();
        assert_eq!(mime, "image/jpeg");
        assert_eq!(
            image::guess_format(&bytes).unwrap(),
            image::ImageFormat::Jpeg
        );
        // Something see-through is never flattened.
        let mut clear = white(4, 4);
        clear.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        assert_eq!(encode(&clear, "image/jpeg").unwrap().1, "image/png");
        assert_eq!(renamed("photo.webp", "image/jpeg"), "photo.jpg");
        assert_eq!(renamed("pasted-image.png", "image/png"), "pasted-image.png");
        assert_eq!(renamed("scan", "image/png"), "scan.png");
    }

    #[test]
    fn a_line_is_as_thick_on_screen_whatever_the_picture() {
        assert_eq!(stroke_for((100, 80)), 2.);
        assert_eq!(stroke_for((2400, 1200)), 10.);
        assert_eq!(stroke_for((20_000, 100)), 24.);
    }
}
