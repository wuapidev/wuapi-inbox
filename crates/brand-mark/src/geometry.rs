//! The mark's shapes for a [`Pose`], on its 64-unit grid.
//!
//! The numbers are `components/site/logo-geometry.ts` of the wuapi site, the
//! source the brand kit's SVGs (`mark-color.svg`,
//! `app-icon-rounded-color.svg`) are generated from. The origin is the top
//! left of the bubble and y grows downwards.

use crate::motion::Pose;

/// Side of the mark's grid.
pub const GRID: f32 = 64.;
/// Corner radius of the bubble. The bottom-left corner is square: it is the
/// tail.
pub const BUBBLE_RADIUS: f32 = 10.;
/// Side of the tile's grid (`app-icon-rounded-color.svg`).
pub const TILE_GRID: f32 = 100.;
/// Corner radius of the tile, on the tile's grid.
pub const TILE_RADIUS: f32 = 15.625;
/// Where the mark's origin sits on the tile's grid, in both axes.
pub const TILE_INSET: f32 = 20.;
/// Size of a mark unit on the tile's grid: the mark is 60 of its 100 units.
pub const TILE_MARK_SCALE: f32 = 0.9375;

/// The face is drawn at 96 %, a touch below the middle.
const FACE_SCALE: f32 = 0.96;
const FACE_OFFSET: (f32, f32) = (1.25, 3.1);
const EYE_SIZE: f32 = 6.4;
const EYE_Y: f32 = 14.8;
const EYE_X: [f32; 2] = [19.8, 38.8];
/// The one-stroke w.
const MOUTH: [(f32, f32); 5] = [(12., 30.), (20., 45.), (32.5, 32.), (45., 45.), (53., 30.)];
const MOUTH_STROKE: f32 = 7.;

/// The bubble: a square with three rounded corners, anchored at its tail.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bubble {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width and height.
    pub side: f32,
    /// Radius of the top-left, top-right and bottom-right corners.
    pub radius: f32,
    /// Opacity, 0 to 1.
    pub opacity: f32,
}

/// One eye: an ellipse, a circle when fully open.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Eye {
    /// Centre.
    pub cx: f32,
    /// Centre.
    pub cy: f32,
    /// Horizontal radius.
    pub rx: f32,
    /// Vertical radius; a blink squashes it.
    pub ry: f32,
}

/// Everything to draw for one pose.
#[derive(Clone, Debug, PartialEq)]
pub struct Shapes {
    /// The bubble.
    pub bubble: Bubble,
    /// Left and right eye. `None` while they are shut flat.
    pub eyes: Option<[Eye; 2]>,
    /// The visible part of the w as a polyline, stroked [`Shapes::stroke`]
    /// wide with round caps and joins. Empty while none of it shows.
    pub mouth: Vec<(f32, f32)>,
    /// Stroke width of the w.
    pub stroke: f32,
}

impl Shapes {
    /// The shapes of `pose`.
    pub fn of(pose: &Pose) -> Self {
        // The bubble scales about its tail, the bottom-left corner.
        let scale = pose.bubble_scale.max(0.);
        let bubble = Bubble {
            x: 0.,
            y: GRID - GRID * scale,
            side: GRID * scale,
            radius: BUBBLE_RADIUS * scale,
            opacity: pose.bubble_opacity.clamp(0., 1.),
        };

        // The face moves as one, in grid units, outside its own 96 % scale.
        let face = |(x, y): (f32, f32)| {
            (
                FACE_OFFSET.0 + FACE_SCALE * x + pose.face_dx,
                FACE_OFFSET.1 + FACE_SCALE * y + pose.face_dy,
            )
        };

        let open = pose.eye_open.max(0.);
        let eyes = (open > 0.004).then(|| {
            EYE_X.map(|x| {
                let (cx, cy) = face((x + EYE_SIZE / 2. + pose.eyes_dx, EYE_Y + EYE_SIZE / 2.));
                let radius = FACE_SCALE * EYE_SIZE / 2.;
                Eye {
                    cx,
                    cy,
                    rx: radius,
                    ry: radius * open,
                }
            })
        });

        let mouth = trim(&MOUTH, pose.mouth_start, pose.mouth_end)
            .into_iter()
            .map(face)
            .collect();

        Self {
            bubble,
            eyes,
            mouth,
            stroke: MOUTH_STROKE * FACE_SCALE,
        }
    }
}

/// The part of `line` between the fractions `start` and `end` of its length.
fn trim(line: &[(f32, f32)], start: f32, end: f32) -> Vec<(f32, f32)> {
    let (start, end) = (start.clamp(0., 1.), end.clamp(0., 1.));
    if end - start < 0.002 {
        return Vec::new();
    }
    if start == 0. && end == 1. {
        return line.to_vec();
    }
    let lengths: Vec<f32> = line
        .windows(2)
        .map(|pair| (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1))
        .collect();
    let total: f32 = lengths.iter().sum();
    let (from, to) = (start * total, end * total);

    let mut points = Vec::with_capacity(line.len());
    let mut walked = 0.;
    for (pair, length) in line.windows(2).zip(lengths) {
        let (begin, finish) = (walked, walked + length);
        walked = finish;
        if finish <= from || begin >= to {
            continue;
        }
        let along = |distance: f32| {
            let t = (distance - begin) / length;
            (
                pair[0].0 + (pair[1].0 - pair[0].0) * t,
                pair[0].1 + (pair[1].1 - pair[0].1) * t,
            )
        };
        if points.is_empty() {
            points.push(along(from.max(begin)));
        }
        points.push(along(to.min(finish)));
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn length(line: &[(f32, f32)]) -> f32 {
        line.windows(2)
            .map(|pair| (pair[1].0 - pair[0].0).hypot(pair[1].1 - pair[0].1))
            .sum()
    }

    #[test]
    fn at_rest_it_is_the_brand_kits_mark() {
        let shapes = Shapes::of(&Pose::REST);
        assert_eq!(
            shapes.bubble,
            Bubble {
                x: 0.,
                y: 0.,
                side: 64.,
                radius: 10.,
                opacity: 1.
            }
        );
        // translate(1.25 3.1) scale(0.96) of the 6.4-unit dots at x 19.8 and
        // 38.8, y 14.8.
        let [left, right] = shapes.eyes.expect("the eyes are open");
        assert!(close(left.cx, 1.25 + 0.96 * 23.) && close(left.cy, 3.1 + 0.96 * 18.));
        assert!(close(right.cx, 1.25 + 0.96 * 42.) && close(right.cy, left.cy));
        assert!(close(left.rx, 3.072) && close(left.ry, 3.072));
        assert_eq!(shapes.mouth.len(), 5);
        assert!(close(shapes.mouth[0].0, 1.25 + 0.96 * 12.));
        assert!(close(shapes.mouth[2].1, 3.1 + 0.96 * 32.));
        assert!(close(shapes.stroke, 6.72));
        // The face sits half a unit right of the bubble's centre.
        assert!(close((left.cx + right.cx) / 2., 32.45));
    }

    #[test]
    fn a_blink_squashes_the_eyes_about_their_centre() {
        let open = Shapes::of(&Pose::REST).eyes.unwrap();
        let blink = Shapes::of(&Pose {
            eye_open: 0.1,
            ..Pose::REST
        })
        .eyes
        .unwrap();
        for (open, blink) in open.iter().zip(blink) {
            assert_eq!((open.cx, open.cy, open.rx), (blink.cx, blink.cy, blink.rx));
            assert!(close(blink.ry, 0.3072));
        }
        let shut = Shapes::of(&Pose {
            eye_open: 0.,
            ..Pose::REST
        });
        assert_eq!(shut.eyes, None);
    }

    #[test]
    fn looking_moves_the_whole_face_and_a_glance_only_the_eyes() {
        let rest = Shapes::of(&Pose::REST);
        let looking = Shapes::of(&Pose {
            face_dx: 4.,
            face_dy: -3.,
            ..Pose::REST
        });
        let (eye, was) = (looking.eyes.unwrap()[0], rest.eyes.unwrap()[0]);
        assert!(close(eye.cx - was.cx, 4.) && close(eye.cy - was.cy, -3.));
        for (now, before) in looking.mouth.iter().zip(&rest.mouth) {
            assert!(close(now.0 - before.0, 4.) && close(now.1 - before.1, -3.));
        }
        assert_eq!(looking.bubble, rest.bubble);
        // Furthest right, the w's cap stays inside the bubble.
        let reach = looking.mouth[4].0 + looking.stroke / 2.;
        assert!(reach < GRID, "{reach}");

        let glancing = Shapes::of(&Pose {
            eyes_dx: 1.5,
            ..Pose::REST
        });
        assert!(close(
            glancing.eyes.unwrap()[1].cx - rest.eyes.unwrap()[1].cx,
            1.44
        ));
        assert_eq!(glancing.mouth, rest.mouth);
    }

    #[test]
    fn the_bubble_pops_from_its_tail() {
        let small = Shapes::of(&Pose {
            bubble_scale: 0.4,
            bubble_opacity: 0.,
            ..Pose::REST
        })
        .bubble;
        assert!(close(small.x, 0.) && close(small.y + small.side, 64.));
        assert!(close(small.side, 25.6) && close(small.radius, 4.));
        assert_eq!(small.opacity, 0.);
    }

    #[test]
    fn the_w_draws_from_its_start() {
        let whole = length(&MOUTH);
        let half = trim(&MOUTH, 0., 0.5);
        assert_eq!(half[0], MOUTH[0]);
        assert!(close(length(&half), whole / 2.));
        // Symmetric letter: half its length ends on the middle apex.
        assert!(close(half.last().unwrap().0, 32.5));

        let tail = trim(&MOUTH, 0.75, 1.);
        assert_eq!(*tail.last().unwrap(), MOUTH[4]);
        assert!(close(length(&tail), whole / 4.));

        assert!(trim(&MOUTH, 0.4, 0.4).is_empty());
        assert!(trim(&MOUTH, 1., 1.).is_empty());
        assert_eq!(trim(&MOUTH, 0., 1.), MOUTH.to_vec());
        assert!(Shapes::of(&Pose {
            mouth_end: 0.,
            ..Pose::REST
        })
        .mouth
        .is_empty());
    }

    #[test]
    fn the_tile_frames_the_mark_like_the_app_icon() {
        // translate(20 20) scale(0.9375) centres the 64-unit mark on the
        // 100-unit tile.
        assert!(close(TILE_INSET * 2. + GRID * TILE_MARK_SCALE, TILE_GRID));
    }
}
