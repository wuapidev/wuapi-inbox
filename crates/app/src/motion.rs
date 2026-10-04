//! The entrance of the start and sign-in screens: content fades up into
//! place, one piece after the other, once.
//!
//! The timing is the wuapi site's (`wu-reveal` and the logo's `wu-rise` in
//! `apps/wuapi/app/globals.css`): 14 px of travel, 0.45 s per piece on
//! `cubic-bezier(0.22, 1, 0.36, 1)`, 90 ms between pieces. Pure functions
//! of time, so they are tested without a window; the views repaint only
//! while [`Entrance::running`] says so.
//!
//! The logo's own animation is not here: it is the brand mark's (see
//! `widgets::brand_mark`).

use std::time::{Duration, Instant};

/// How far a piece travels as it comes in, in pixels.
pub const TRAVEL: f32 = 14.;
/// How long one piece takes.
const PIECE: Duration = Duration::from_millis(450);
/// The wait before the first piece.
const LEAD: Duration = Duration::from_millis(80);
/// The wait between one piece and the next.
const STAGGER: Duration = Duration::from_millis(90);
/// The pieces a screen staggers: steps `0..STEPS`.
pub const STEPS: u32 = 6;
/// One frame of the entrance.
pub const FRAME: Duration = Duration::from_millis(16);

/// `cubic-bezier(x1, y1, x2, y2)` at `progress`, as CSS defines it.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, progress: f32) -> f32 {
    if progress <= 0. {
        return 0.;
    }
    if progress >= 1. {
        return 1.;
    }
    let curve = |a: f32, b: f32, t: f32| {
        let u = 1. - t;
        3. * u * u * t * a + 3. * u * t * t * b + t * t * t
    };
    // x(t) is monotonic for control points inside [0, 1]: bisect for t.
    let (mut low, mut high) = (0f32, 1f32);
    for _ in 0..24 {
        let middle = (low + high) / 2.;
        if curve(x1, x2, middle) < progress {
            low = middle;
        } else {
            high = middle;
        }
    }
    curve(y1, y2, (low + high) / 2.)
}

/// The standard ease-out of the interface: fast out of the gate, a soft
/// landing (the curve of the entrances).
pub fn ease_out(progress: f32) -> f32 {
    cubic_bezier(0.22, 1., 0.36, 1., progress)
}

/// One thing moving from where it was to where it goes, once.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween {
    started: Instant,
    duration: Duration,
}

impl Tween {
    /// A move that begins now.
    pub fn begin(now: Instant, duration: Duration) -> Self {
        Self {
            started: now,
            duration,
        }
    }

    /// How far along it is at `now`, eased, from 0 to 1. With `still`
    /// (reduced motion) it is there at once.
    pub fn at(&self, now: Instant, still: bool) -> f32 {
        if still || self.duration.is_zero() {
            return 1.;
        }
        let elapsed = now.saturating_duration_since(self.started);
        ease_out(elapsed.as_secs_f32() / self.duration.as_secs_f32())
    }

    /// Whether it has arrived.
    pub fn done(&self, now: Instant, still: bool) -> bool {
        still || now.saturating_duration_since(self.started) >= self.duration
    }
}

/// How long the row a jump landed on stays lit (a quote that was clicked,
/// a search result). It is lit and then it is not: nothing is repainted
/// in between, with or without motion.
pub const FLASH: Duration = Duration::from_millis(1400);

/// How long the palette waits after a keystroke before it asks the store:
/// typing a word is one question, not one per letter.
pub const SEARCH_DEBOUNCE: Duration = Duration::from_millis(120);

/// The account rail's motion: every duration and factor of it, in one
/// place. All of it is skipped under reduced motion.
pub mod rail {
    use std::time::Duration;

    /// A dragged item rising out of its slot.
    pub const LIFT: Duration = Duration::from_millis(120);
    /// The other items moving apart where it would land.
    pub const GAP: Duration = Duration::from_millis(140);
    /// A group opening or closing.
    pub const FOLD: Duration = Duration::from_millis(180);
    /// A dropped item settling into its place, or a cancelled one going
    /// back to where it came from.
    pub const SETTLE: Duration = Duration::from_millis(160);
    /// How long a drag rests on a closed group before the group opens.
    pub const DWELL: Duration = Duration::from_millis(500);
    /// How much larger the lifted copy is.
    pub const LIFT_SCALE: f32 = 1.06;
    /// How solid the lifted copy is.
    pub const LIFT_OPACITY: f32 = 0.92;
    /// How faint the slot it left is.
    pub const PLACEHOLDER_OPACITY: f32 = 0.5;
}

/// Stickers and GIFs that move in a conversation. They hold their first
/// frame under reduced motion.
pub mod animated {
    use std::time::Duration;

    /// The least time between two repaints for moving pictures: a frame
    /// that asks for less shares a repaint with the next one.
    pub const MIN_TICK: Duration = Duration::from_millis(30);
    /// How long after it was last drawn a decoded animation may be let
    /// go to make room for another.
    pub const IDLE: Duration = Duration::from_secs(2);
}

/// The linking screen: the placeholder that waits where the QR code will
/// be, and the code taking its place. Under reduced motion the placeholder
/// is still and the code is there at once.
pub mod link {
    use std::time::Duration;

    /// One pass of the light across the placeholder.
    pub const SWEEP: Duration = Duration::from_millis(1600);
    /// How long the light keeps passing. A code that takes longer than
    /// this is not hurried by repainting: the placeholder rests.
    pub const SWEEP_FOR: Duration = Duration::from_secs(24);
    /// The code fading in over the placeholder.
    pub const CROSSFADE: Duration = Duration::from_millis(220);
    /// How much of the placeholder's diagonal the light covers, each side
    /// of its middle.
    const REACH: f32 = 0.22;
    /// How solid a module is away from the light, and under it.
    pub const DIM: f32 = 0.3;
    pub const LIT: f32 = 0.75;
    /// How long "Copied" stands in for "Copy code".
    pub const COPIED: Duration = Duration::from_millis(1600);

    /// Where the light is `elapsed` after the placeholder appeared, along
    /// the diagonal: it enters before the first corner and leaves past the
    /// last. `None` when nothing moves: reduced motion (`still`), or the
    /// wait has gone on for longer than [`SWEEP_FOR`].
    pub fn sweep(elapsed: Duration, still: bool) -> Option<f32> {
        if still || elapsed >= SWEEP_FOR {
            return None;
        }
        let turn = (elapsed.as_millis() % SWEEP.as_millis()) as f32 / SWEEP.as_millis() as f32;
        Some(-REACH + turn * (1. + 2. * REACH))
    }

    /// How solid the module at `along` (0 at the first corner, 1 at the
    /// last) is with the light at `light`.
    pub fn glow(along: f32, light: Option<f32>) -> f32 {
        let Some(light) = light else {
            return DIM;
        };
        let near = (1. - (along - light).abs() / REACH).max(0.);
        // Smooth at both ends of the light.
        DIM + (LIT - DIM) * near * near * (3. - 2. * near)
    }
}

/// `--motion-at`: every animation held at this moment of its timeline, so
/// a frame can be looked at (and photographed) for as long as needed. A
/// development aid; nothing sets it in normal use.
#[derive(Clone, Copy, Debug)]
pub struct FrozenAt(pub Duration);

impl gpui_kit::Global for FrozenAt {}

/// The moment the animations are held at, if they are.
pub fn frozen_at(cx: &gpui_kit::App) -> Option<Duration> {
    cx.try_global::<FrozenAt>().map(|frozen| frozen.0)
}

/// The entrance of one screen.
#[derive(Clone, Copy, Debug, Default)]
pub struct Entrance {
    /// When it began. `None`: everything is in place (it has not been
    /// asked for, it was skipped, or it is over).
    started: Option<Instant>,
    /// Held at this moment, never running (see [`FrozenAt`]).
    frozen: Option<Duration>,
}

impl Entrance {
    /// An entrance held `elapsed` after its start.
    pub fn frozen(elapsed: Duration) -> Self {
        Self {
            started: None,
            frozen: Some(elapsed),
        }
    }

    /// Starts the entrance.
    pub fn begin(&mut self, now: Instant) {
        self.started = Some(now);
    }

    /// Starts the entrance `delay` from now: the pieces wait for something
    /// else to finish first (the mark popping in).
    pub fn begin_after(&mut self, now: Instant, delay: Duration) {
        self.started = Some(now + delay);
    }

    /// Puts everything in place at once: reduced motion, or the screen
    /// going away.
    pub fn skip(&mut self) {
        self.started = None;
    }

    /// How far the piece at `step` has come in, from 0 (not there yet) to
    /// 1 (in place).
    pub fn reveal(&self, step: u32, now: Instant) -> f32 {
        let elapsed = match (self.frozen, self.started) {
            (Some(elapsed), _) => elapsed,
            (None, Some(started)) => now.saturating_duration_since(started),
            (None, None) => return 1.,
        };
        let begins = LEAD + STAGGER * step;
        let progress = elapsed.saturating_sub(begins).as_secs_f32() / PIECE.as_secs_f32();
        cubic_bezier(0.22, 1., 0.36, 1., progress)
    }

    /// True while a piece is still moving: the view repaints each frame.
    /// Once false, nothing more is drawn until something else changes.
    pub fn running(&self, now: Instant) -> bool {
        self.started.is_some_and(|started| {
            now.saturating_duration_since(started) < LEAD + STAGGER * (STEPS - 1) + PIECE
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_curves_match_css() {
        // Linear, and the two ends of any curve.
        assert!((cubic_bezier(0., 0., 1., 1., 0.3) - 0.3).abs() < 1e-3);
        assert_eq!(cubic_bezier(0.22, 1., 0.36, 1., 0.), 0.);
        assert!((cubic_bezier(0.22, 1., 0.36, 1., 1.) - 1.).abs() < 1e-4);
        // ease-out-expo-like: most of the way there a third of the way in.
        assert!(cubic_bezier(0.22, 1., 0.36, 1., 0.33) > 0.8);
        // The brand's "back" curve overshoots before it settles.
        let peak = (0..=100)
            .map(|i| cubic_bezier(0.34, 1.56, 0.64, 1., i as f32 / 100.))
            .fold(0f32, f32::max);
        assert!(peak > 1.05 && peak < 1.2, "overshoot {peak}");
    }

    #[test]
    fn a_tween_eases_to_its_end_and_reduced_motion_is_there_at_once() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let tween = Tween::begin(start, Duration::from_millis(160));
        assert_eq!(tween.at(start, false), 0.);
        let middle = tween.at(at(80), false);
        assert!(
            middle > 0.5 && middle < 1.,
            "ease-out: past half at half time"
        );
        assert!((tween.at(at(160), false) - 1.).abs() < 1e-3);
        assert!(!tween.done(at(159), false) && tween.done(at(160), false));
        // Still: no in-between.
        assert_eq!(tween.at(start, true), 1.);
        assert!(tween.done(start, true));
        assert_eq!(Tween::begin(start, Duration::ZERO).at(start, false), 1.);
    }

    #[test]
    fn the_light_crosses_the_placeholder_and_rests_when_it_may_not_move() {
        use link::{glow, sweep, DIM, LIT, SWEEP, SWEEP_FOR};
        let ms = Duration::from_millis;
        // It comes in from before the first corner and leaves past the last.
        let start = sweep(Duration::ZERO, false).unwrap();
        let end = sweep(SWEEP - ms(1), false).unwrap();
        assert!(start < 0. && end > 1., "{start} {end}");
        assert!(sweep(ms(400), false).unwrap() < sweep(ms(800), false).unwrap());
        // And again on the next pass.
        assert_eq!(sweep(SWEEP, false), sweep(Duration::ZERO, false));
        // Reduced motion, and a wait that has gone on: nothing moves.
        assert_eq!(sweep(ms(400), true), None);
        assert_eq!(sweep(SWEEP_FOR, false), None);

        // Under the light a module is lit; away from it, and with no
        // light at all, it is dim.
        assert!((glow(0.5, Some(0.5)) - LIT).abs() < 1e-4);
        assert_eq!(glow(0.5, Some(0.9)), DIM);
        assert_eq!(glow(0.5, None), DIM);
        let half = glow(0.5, Some(0.61));
        assert!(half > DIM && half < LIT);
        // Nothing is lit before the light arrives.
        assert_eq!(glow(0., Some(start)), DIM);
    }

    #[test]
    fn pieces_come_in_one_after_the_other_and_then_it_is_over() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut entrance = Entrance::default();
        assert_eq!(entrance.reveal(0, start), 1., "nothing asked: all in place");
        assert!(!entrance.running(start));

        entrance.begin(start);
        assert_eq!(entrance.reveal(0, start), 0.);
        assert!(entrance.running(start));
        // 200 ms in: the first piece is well on its way, the fourth has
        // not started.
        assert!(entrance.reveal(0, at(200)) > 0.5);
        assert_eq!(entrance.reveal(3, at(200)), 0.);
        assert!(entrance.reveal(0, at(200)) > entrance.reveal(1, at(200)));
        // Every piece is in place a second later, and the repainting stops.
        for step in 0..STEPS {
            assert!((entrance.reveal(step, at(1000)) - 1.).abs() < 1e-3);
        }
        assert!(entrance.running(at(900)));
        assert!(!entrance.running(at(1000)));
    }

    #[test]
    fn an_entrance_can_wait_for_the_mark() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let mut entrance = Entrance::default();
        entrance.begin_after(start, Duration::from_millis(500));
        assert_eq!(entrance.reveal(0, at(400)), 0., "still waiting");
        assert!(entrance.running(at(400)), "and still to be repainted");
        assert!(entrance.reveal(0, at(800)) > 0.5);
        assert!(!entrance.running(at(1600)));
    }

    #[test]
    fn reduced_motion_skips_the_entrance() {
        let start = Instant::now();
        let mut entrance = Entrance::default();
        entrance.begin(start);
        entrance.skip();
        assert_eq!(entrance.reveal(5, start), 1.);
        assert!(!entrance.running(start));
    }

    #[test]
    fn a_frozen_entrance_holds_its_frame_and_never_repaints() {
        let start = Instant::now();
        let held = Entrance::frozen(Duration::from_millis(200));
        let live = {
            let mut entrance = Entrance::default();
            entrance.begin(start);
            entrance
        };
        let later = start + Duration::from_secs(60);
        for step in 0..STEPS {
            assert_eq!(
                held.reveal(step, later),
                live.reveal(step, start + Duration::from_millis(200))
            );
        }
        assert!(!held.running(later));
    }
}
