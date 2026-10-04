//! The mark as a GPUI element.

use std::cell::RefCell;
use std::panic::Location;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::{
    fill, point, px, rgb, size, App, Bounds, ContentMask, Corners, DispatchPhase, Element,
    ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseMoveEvent, PathBuilder, Pixels, Point, Style, Task, Window,
};

use crate::geometry::{Eye, Shapes, GRID, TILE_GRID, TILE_INSET, TILE_MARK_SCALE, TILE_RADIUS};
use crate::motion::{Playback, Pose, Repeat, Timeline, Wake};

/// The brand lime, `#D4FF3F`.
pub const LIME: u32 = 0xd4ff3f;
/// The brand ink of the logo, `#0A0A0A`.
pub const INK: u32 = 0x0a0a0a;

/// The mark's colours. The bubble is always the lime; the defaults are the
/// brand kit's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkColors {
    /// The bubble.
    pub lime: Hsla,
    /// The eyes and the w, drawn on the bubble.
    pub ink: Hsla,
    /// The rounded tile behind the mark, when it has one.
    pub tile: Hsla,
}

impl Default for MarkColors {
    fn default() -> Self {
        Self {
            lime: rgb(LIME).into(),
            ink: rgb(INK).into(),
            tile: rgb(INK).into(),
        }
    }
}

/// The wuapi mark, alive: it pops in, blinks and looks around as it does on
/// the wuapi site, and answers the pointer with a double blink.
///
/// It is a square of the given size. It repaints only while something
/// moves; between movements it sleeps on a timer, and it stops for good
/// when [`animate(false)`](Self::animate), under reduced motion, or when it
/// is no longer part of what the window draws.
///
/// ```no_run
/// use brand_mark::AnimatedMark;
/// use gpui_kit::px;
///
/// // On a dark surface: the bare lime mark.
/// let dark = AnimatedMark::new(px(96.));
/// // On a light surface: the lime mark on its ink tile.
/// let light = AnimatedMark::new(px(96.)).tile(true);
/// ```
pub struct AnimatedMark {
    id: ElementId,
    size: Pixels,
    tile: bool,
    colors: MarkColors,
    playback: Playback,
    hover: bool,
    pose: Option<Pose>,
}

impl AnimatedMark {
    /// A mark `size` wide and high, with the timeline of the site's header
    /// logo ([`Timeline::site`]): the entrance, then two rounds of blinking
    /// and looking around.
    ///
    /// Its state is keyed by the place in the code that calls this. Marks
    /// built in a loop need an [`id`](Self::id) each.
    #[track_caller]
    pub fn new(size: Pixels) -> Self {
        Self {
            id: ElementId::CodeLocation(*Location::caller()),
            size,
            tile: false,
            colors: MarkColors::default(),
            playback: Playback::default(),
            hover: true,
            pose: None,
        }
    }

    /// The static mark: [`Pose::REST`], no animation, no state.
    #[track_caller]
    pub fn still(size: Pixels) -> Self {
        Self::new(size).animate(false)
    }

    /// Identifies this mark among its siblings. The animation restarts when
    /// the id changes.
    pub fn id(mut self, id: impl Into<ElementId>) -> Self {
        self.id = id.into();
        self
    }

    /// Draws the mark on its rounded ink tile, as the brand does on light
    /// backgrounds (`app-icon-rounded-color.svg`). The tile fills the
    /// element and the mark takes 60 % of it. Off by default.
    pub fn tile(mut self, tile: bool) -> Self {
        self.tile = tile;
        self
    }

    /// All three colours at once.
    pub fn colors(mut self, colors: MarkColors) -> Self {
        self.colors = colors;
        self
    }

    /// The bubble's colour. The brand lime by default.
    pub fn lime(mut self, lime: impl Into<Hsla>) -> Self {
        self.colors.lime = lime.into();
        self
    }

    /// The colour of the eyes and the w. The brand ink by default.
    pub fn ink(mut self, ink: impl Into<Hsla>) -> Self {
        self.colors.ink = ink.into();
        self
    }

    /// The tile's colour. The brand ink by default.
    pub fn tile_color(mut self, tile: impl Into<Hsla>) -> Self {
        self.colors.tile = tile.into();
        self
    }

    /// False shows the static mark and stops every timer and repaint. Turning
    /// it back on starts the timeline from zero.
    pub fn animate(mut self, animate: bool) -> Self {
        self.playback.animate = animate;
        self
    }

    /// True shows the static mark, eyes open, as the site does under
    /// `prefers-reduced-motion`. GPUI's own `App::reduce_motion` has the
    /// same effect.
    pub fn reduced_motion(mut self, reduced_motion: bool) -> Self {
        self.playback.reduced_motion = reduced_motion;
        self
    }

    /// What it plays. [`Timeline::site`] by default; [`Timeline::mascot`]
    /// for a mark that is already on screen, with a phase per mark.
    pub fn timeline(mut self, timeline: Timeline) -> Self {
        self.playback.timeline = timeline;
        self
    }

    /// Whether it pops in when it appears. On by default.
    pub fn intro(mut self, intro: bool) -> Self {
        self.playback.timeline.intro = intro;
        self
    }

    /// How many rounds of blinking and looking around it plays. Two by
    /// default, as on the site; [`Repeat::Forever`] never rests.
    pub fn repeat(mut self, repeat: Repeat) -> Self {
        self.playback.timeline.repeat = repeat;
        self
    }

    /// Whether the pointer arriving on the mark plays the gesture of the
    /// site's logo link: blink twice, glance, redraw the w. On by default.
    /// The mark never swallows the pointer's events.
    pub fn play_on_hover(mut self, hover: bool) -> Self {
        self.hover = hover;
        self
    }

    /// Freezes the mark in one pose, for previews and tests.
    pub fn pose(mut self, pose: Pose) -> Self {
        self.pose = Some(pose);
        self
    }

    fn is_live(&self, cx: &App) -> bool {
        self.pose.is_none() && self.playback.is_live() && !cx.reduce_motion()
    }
}

/// What a live mark remembers between frames. GPUI drops it, and with it
/// the timer, as soon as a frame is drawn without the mark.
struct Runtime {
    started: Instant,
    /// When the pointer last arrived, on the timeline.
    pointed_at: Option<Duration>,
    hovered: bool,
    /// Wakes the view for the next movement. Replaced, and so cancelled, on
    /// every paint.
    timer: Option<Task<()>>,
}

type SharedRuntime = Rc<RefCell<Runtime>>;

impl IntoElement for AnimatedMark {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for AnimatedMark {
    type RequestLayoutState = ();
    type PrepaintState = Option<Hitbox>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let style = Style {
            size: size(self.size.into(), self.size.into()),
            flex_shrink: 0.,
            ..Style::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        (self.hover && self.is_live(cx))
            .then(|| window.insert_hitbox(bounds, HitboxBehavior::Normal))
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let live = id.filter(|_| self.is_live(cx));
        let Some(id) = live else {
            // Not touching the element state lets GPUI drop it: no timer
            // survives a pause.
            let pose = self.pose.unwrap_or(Pose::REST);
            paint_mark(bounds, &pose, self.tile, &self.colors, window);
            return;
        };

        let now = Instant::now();
        let runtime = window.with_element_state::<SharedRuntime, _>(id, |runtime, _| {
            let runtime = runtime.unwrap_or_else(|| {
                Rc::new(RefCell::new(Runtime {
                    started: now,
                    pointed_at: None,
                    hovered: false,
                    timer: None,
                }))
            });
            (runtime.clone(), runtime)
        });

        let view = window.current_view();
        let frame = {
            let mut state = runtime.borrow_mut();
            let at = now.saturating_duration_since(state.started);
            let frame = self.playback.frame(at, state.pointed_at);
            state.timer = match frame.wake {
                Wake::NextFrame => {
                    window.request_animation_frame();
                    None
                }
                Wake::At(wake) => {
                    // Sleep until the next movement starts, then redraw
                    // the view that holds the mark.
                    let sleep = wake.saturating_sub(at).max(Duration::from_millis(1));
                    Some(window.spawn(cx, async move |cx| {
                        cx.background_executor().timer(sleep).await;
                        cx.update(|_, cx| cx.notify(view)).ok();
                    }))
                }
                Wake::Never => None,
            };
            frame
        };

        if let Some(hitbox) = hitbox.take() {
            let runtime = runtime.clone();
            window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let hovered = hitbox.is_hovered(window);
                let mut state = runtime.borrow_mut();
                if hovered && !state.hovered {
                    state.pointed_at = Some(state.started.elapsed());
                    cx.notify(view);
                }
                state.hovered = hovered;
            });
        }

        paint_mark(bounds, &frame.pose, self.tile, &self.colors, window);
    }
}

/// Paints the mark in `pose` into `bounds` (a square), with or without its
/// tile. This is all [`AnimatedMark`] draws; call it from a `canvas` to
/// place the mark in a custom element. Paint phase only.
pub fn paint_mark(
    bounds: Bounds<Pixels>,
    pose: &Pose,
    tile: bool,
    colors: &MarkColors,
    window: &mut Window,
) {
    let side = bounds.size.width.min(bounds.size.height);
    // From the mark's grid to the window.
    let (unit, inset) = if tile {
        let unit = side / TILE_GRID;
        (unit * TILE_MARK_SCALE, unit * TILE_INSET)
    } else {
        (side / GRID, px(0.))
    };
    let origin = bounds.origin + point(inset, inset);
    let at = |(x, y): (f32, f32)| origin + point(unit * x, unit * y);
    let shapes = Shapes::of(pose);

    // The site's SVG clips to its box, which trims the bubble's overshoot
    // as it pops in. On a tile the overshoot has room.
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        if tile {
            let radius = side / TILE_GRID * TILE_RADIUS;
            window.paint_quad(fill(bounds, colors.tile).corner_radii(radius));
        }

        let bubble = shapes.bubble;
        if bubble.opacity > 0. && bubble.side > 0. {
            let radius = unit * bubble.radius;
            window.paint_quad(
                fill(
                    Bounds::new(
                        at((bubble.x, bubble.y)),
                        size(unit * bubble.side, unit * bubble.side),
                    ),
                    colors.lime.opacity(bubble.opacity),
                )
                .corner_radii(Corners {
                    top_left: radius,
                    top_right: radius,
                    bottom_right: radius,
                    // The tail.
                    bottom_left: px(0.),
                }),
            );
        }

        for eye in shapes.eyes.iter().flatten() {
            paint_ellipse(eye, &at, colors.ink, window);
        }
        paint_stroke(&shapes.mouth, shapes.stroke, unit, &at, colors.ink, window);
    });
}

/// How far a cubic's control points sit from its ends to draw a quarter of
/// a circle of radius 1.
const QUARTER_ARC: f32 = 0.552_284_8;

fn paint_ellipse(
    eye: &Eye,
    at: &impl Fn((f32, f32)) -> Point<Pixels>,
    color: Hsla,
    window: &mut Window,
) {
    let Eye { cx, cy, rx, ry } = *eye;
    let (kx, ky) = (rx * QUARTER_ARC, ry * QUARTER_ARC);
    let mut path = PathBuilder::fill();
    path.move_to(at((cx + rx, cy)));
    path.cubic_bezier_to(
        at((cx, cy + ry)),
        at((cx + rx, cy + ky)),
        at((cx + kx, cy + ry)),
    );
    path.cubic_bezier_to(
        at((cx - rx, cy)),
        at((cx - kx, cy + ry)),
        at((cx - rx, cy + ky)),
    );
    path.cubic_bezier_to(
        at((cx, cy - ry)),
        at((cx - rx, cy - ky)),
        at((cx - kx, cy - ry)),
    );
    path.cubic_bezier_to(
        at((cx + rx, cy)),
        at((cx + kx, cy - ry)),
        at((cx + rx, cy - ky)),
    );
    path.close();
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// A polyline stroked with round caps and joins: a disc on every point and
/// a bar along every segment, all in one opaque colour.
fn paint_stroke(
    line: &[(f32, f32)],
    width: f32,
    unit: Pixels,
    at: &impl Fn((f32, f32)) -> Point<Pixels>,
    color: Hsla,
    window: &mut Window,
) {
    let half = width / 2.;
    for &(cx, cy) in line {
        let disc = Eye {
            cx,
            cy,
            rx: half,
            ry: half,
        };
        paint_ellipse(&disc, at, color, window);
    }
    for pair in line.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        let length = (x1 - x0).hypot(y1 - y0);
        // Shorter than a device pixel: the discs already cover it.
        if f32::from(unit * length) < 0.25 {
            continue;
        }
        let (nx, ny) = (-(y1 - y0) / length * half, (x1 - x0) / length * half);
        let mut path = PathBuilder::fill();
        path.move_to(at((x0 + nx, y0 + ny)));
        path.line_to(at((x1 + nx, y1 + ny)));
        path.line_to(at((x1 - nx, y1 - ny)));
        path.line_to(at((x0 - nx, y0 - ny)));
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
    }
}
