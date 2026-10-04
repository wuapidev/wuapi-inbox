//! The conversation's wallpaper: a faint pattern of the brand's own
//! devices behind the messages.
//!
//! One tile, 168 design pixels a side, repeated: a lattice of the grid's
//! crosshairs, the double tick of a message that was read, the brackets of
//! a mono tag and dots (`brand/wallpaper-lines.svg`), and on its diagonal
//! the outline of the mark (`brand/wallpaper-marks.svg`), in a breath of
//! the accent. Both colours are tokens that sit just off the page, fainter
//! than a hairline, so nothing on the page is harder to read for it; what
//! stands on the page itself (the day, notices) is on a pill.
//!
//! It costs a handful of sprites: each layer is rasterised once, at the
//! window's own pixel density, into the same atlas the icons live in, and
//! painted from there as one quad per tile. Nothing is laid out, nothing
//! moves, and it does not scroll with the messages.

use crate::theme::{metrics, Palette};
use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, div, point, px, size, Bounds, ContentMask, Div, Hsla, Pixels, SharedString,
    TransformationMatrix,
};

/// The lattice: crosshairs, ticks, brackets and dots.
const LINES: &[u8] = include_bytes!("../../assets/brand/wallpaper-lines.svg");
/// The marks on the lattice's diagonal.
const MARKS: &[u8] = include_bytes!("../../assets/brand/wallpaper-marks.svg");

/// The side of one tile at the interface size in use: whole pixels, so
/// the tiles meet without a seam.
pub fn tile_side() -> Pixels {
    px(metrics::WALLPAPER_TILE().as_f32().round())
}

/// Where the tiles go to cover `area`, from its top left corner.
pub fn tiles(area: Bounds<Pixels>, side: Pixels) -> Vec<Bounds<Pixels>> {
    let mut placed = Vec::new();
    if side <= px(0.) {
        return placed;
    }
    let mut y = area.top();
    while y < area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            placed.push(Bounds {
                origin: point(x, y),
                size: size(side, side),
            });
            x += side;
        }
        y += side;
    }
    placed
}

/// The wallpaper, filling the box it is a child of (which must be
/// `relative`) without taking part in its layout.
pub fn wallpaper(palette: &Palette) -> Div {
    let layers: [(&'static str, &'static [u8], Hsla); 2] = [
        ("wallpaper-lines", LINES, palette.wallpaper),
        ("wallpaper-marks", MARKS, palette.wallpaper_accent),
    ];
    div()
        .debug_selector(|| "wallpaper".into())
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    let placed = tiles(bounds, tile_side());
                    window.with_content_mask(Some(ContentMask { bounds }), |window| {
                        for (name, bytes, colour) in layers {
                            for tile in &placed {
                                // A layer that cannot be drawn is left
                                // out: the page is then plain.
                                let _ = window.paint_svg(
                                    *tile,
                                    SharedString::new_static(name),
                                    Some(bytes),
                                    TransformationMatrix::unit(),
                                    colour,
                                    cx,
                                );
                            }
                        }
                    });
                },
            )
            .size_full(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    #[test]
    fn tiles_cover_the_pane_and_no_more() {
        let area = Bounds {
            origin: point(px(400.), px(48.)),
            size: size(px(840.), px(700.)),
        };
        let placed = tiles(area, px(168.));
        // 5 across, 5 down: a few dozen sprites, whatever the thread holds.
        assert_eq!(placed.len(), 25);
        assert_eq!(placed[0].origin, area.origin);
        let last = placed.last().unwrap();
        assert!(last.right() >= area.right() && last.bottom() >= area.bottom());
        assert!(last.left() < area.right() && last.top() < area.bottom());
        // They meet edge to edge.
        assert_eq!(placed[1].left(), placed[0].right());
        assert!(tiles(area, px(0.)).is_empty());
    }

    #[test]
    fn the_tile_is_whole_pixels_at_every_interface_size() {
        for step in crate::theme::SCALE_STEPS {
            crate::theme::set_scale(step);
            let side = tile_side().as_f32();
            assert_eq!(side, side.round(), "{step}%");
            assert!((120. ..=220.).contains(&side), "{step}%: {side}");
        }
        crate::theme::set_scale(100);
    }

    /// Both layers are drawings the renderer accepts, and mostly air: the
    /// pattern is a scatter of thin lines, not a texture.
    #[gpui_kit::test]
    fn both_layers_draw_and_are_mostly_empty(cx: &mut TestAppContext) {
        cx.update(|cx| {
            for (name, bytes) in [("lines", LINES), ("marks", MARKS)] {
                let image = cx
                    .svg_renderer()
                    .render_single_frame(bytes, 1.)
                    .unwrap_or_else(|error| panic!("{name}: {error}"));
                let frame = image.as_bytes(0).expect("one frame");
                // Every fourth byte is a pixel's alpha.
                let inked = frame
                    .iter()
                    .skip(3)
                    .step_by(4)
                    .filter(|alpha| **alpha > 24)
                    .count();
                let share = inked as f32 / (frame.len() / 4) as f32;
                assert!(
                    share > 0.002 && share < 0.06,
                    "{name}: {:.1}% of the tile is drawn on",
                    share * 100.
                );
            }
        });
    }
}
