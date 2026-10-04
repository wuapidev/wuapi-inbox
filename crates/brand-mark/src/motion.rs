//! The mark's motion as a pure function of time: time in, [`Pose`] out.
//!
//! Nothing here touches a window or a clock, so all of it is unit-tested.
//! The numbers are the wuapi site's (`app/globals.css`, the `wu-logo` rules
//! and their `@keyframes`):
//!
//! | Movement | Site rule | Timing |
//! | --- | --- | --- |
//! | Bubble pops from its tail | `wu-pop` | 0.55 s, back-out, scale 0.4 to 1, fades in |
//! | Eyes open | `wu-open` | 0.22 s after 0.3 s, back-out |
//! | The w draws | `wu-draw` | 0.6 s after 0.5 s, `cubic-bezier(.65,0,.35,1)` |
//! | Blink | `wu-blink` | 7 s cycle after 2.5 s, linear; shut at 23 % and 84 %, 140 ms each |
//! | Look around | `wu-look` | 11 s cycle after 1.8 s, `cubic-bezier(.45,0,.55,1)` per step |
//! | Pointed at: glance | `wu-glance` | 0.9 s, ease-in-out, eyes 1.5 units right |
//! | Pointed at: blink twice | `wu-blink-twice` | 0.5 s, linear |
//! | Pointed at: redraw the w | `wu-redraw` | 0.7 s, `cubic-bezier(.65,0,.35,1)` |
//!
//! Blink and look run [`Repeat::Times(2)`](Repeat) on the site and then the
//! character rests. The two cycles have different lengths, so the
//! movements never line up into a visible loop. The site has no randomness:
//! marks that share a screen are told apart by a phase
//! ([`Timeline::mascot`]); [`Timeline::seeded`] derives that phase from a
//! seed.
//!
//! Distances are in the units of the mark's 64-unit grid.

use std::time::Duration;

/// Where every moving part of the mark is at one instant.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    /// Scale of the bubble about its tail, the square bottom-left corner.
    /// 1 at rest; it overshoots a little while it pops in.
    pub bubble_scale: f32,
    /// Opacity of the bubble, 0 to 1.
    pub bubble_opacity: f32,
    /// How far the whole face (eyes and w) has moved right, in grid units.
    pub face_dx: f32,
    /// How far the whole face has moved down, in grid units. Looking up is
    /// negative.
    pub face_dy: f32,
    /// How far the eyes alone have moved right, in grid units.
    pub eyes_dx: f32,
    /// Vertical scale of the eyes about their centre: 1 open, 0.1 at the
    /// bottom of a blink, 0 before they open.
    pub eye_open: f32,
    /// Where the visible part of the w's stroke starts, as a fraction of
    /// its length.
    pub mouth_start: f32,
    /// Where the visible part of the w's stroke ends, as a fraction of its
    /// length.
    pub mouth_end: f32,
}

impl Pose {
    /// The mark as the brand kit draws it: still, eyes open. This is the
    /// static fallback.
    pub const REST: Pose = Pose {
        bubble_scale: 1.,
        bubble_opacity: 1.,
        face_dx: 0.,
        face_dy: 0.,
        eyes_dx: 0.,
        eye_open: 1.,
        mouth_start: 0.,
        mouth_end: 1.,
    };
}

impl Default for Pose {
    fn default() -> Self {
        Self::REST
    }
}

/// How many times the blink and look cycles run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    /// This many cycles, then the character rests for good. The site runs 2
    /// (WCAG 2.2.2: nothing keeps moving by itself).
    Times(u32),
    /// The cycles never stop.
    Forever,
}

/// When the pose changes next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wake {
    /// Something is moving now: draw the next frame.
    NextFrame,
    /// Nothing moves until this time on the timeline.
    At(Duration),
    /// Nothing will move again.
    Never,
}

impl Wake {
    fn sooner(self, other: Wake) -> Wake {
        match (self, other) {
            (Wake::NextFrame, _) | (_, Wake::NextFrame) => Wake::NextFrame,
            (Wake::At(a), Wake::At(b)) => Wake::At(a.min(b)),
            (Wake::At(a), Wake::Never) | (Wake::Never, Wake::At(a)) => Wake::At(a),
            (Wake::Never, Wake::Never) => Wake::Never,
        }
    }
}

/// A pose and when to compute the next one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// What to draw.
    pub pose: Pose,
    /// When what to draw changes.
    pub wake: Wake,
}

impl Frame {
    /// The static mark, with nothing scheduled.
    pub const STILL: Frame = Frame {
        pose: Pose::REST,
        wake: Wake::Never,
    };
}

/// What the mark does over time. Time zero is the moment it appears.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Timeline {
    /// Play the entrance: the bubble pops from its tail, the eyes open, the
    /// w draws.
    pub intro: bool,
    /// Blink and look around.
    pub alive: bool,
    /// When the first blink cycle starts (`--wu-blink-delay`).
    pub blink_delay: Duration,
    /// When the first look cycle starts (`--wu-look-delay`).
    pub look_delay: Duration,
    /// How many blink and look cycles run.
    pub repeat: Repeat,
}

impl Timeline {
    /// The logo of the site's header: the entrance, then alive.
    pub fn site() -> Self {
        Self {
            intro: true,
            ..Self::mascot(0.)
        }
    }

    /// The site's `Mascot`: alive, no entrance. `phase` shifts its cycles so
    /// that two marks on one screen do not move together; the site uses 0,
    /// 1 and 2.
    pub fn mascot(phase: f32) -> Self {
        let phase = f64::from(phase.max(0.));
        Self {
            intro: false,
            alive: true,
            blink_delay: Duration::from_secs_f64(2.5 + phase * 1.7),
            look_delay: Duration::from_secs_f64(1.8 + phase * 2.3),
            repeat: Repeat::Times(2),
        }
    }

    /// A [`Timeline::mascot`] whose phase, between 0 and 3, comes from
    /// `seed`. The same seed always gives the same timeline.
    pub fn seeded(seed: u64) -> Self {
        Self::mascot(phase_from_seed(seed))
    }

    /// The same timeline with or without the entrance.
    pub fn intro(mut self, intro: bool) -> Self {
        self.intro = intro;
        self
    }

    /// The same timeline with another number of cycles.
    pub fn repeat(mut self, repeat: Repeat) -> Self {
        self.repeat = repeat;
        self
    }

    /// The pose at `at`, and when it changes next.
    ///
    /// `pointed_at` is the time on this timeline at which the pointer last
    /// arrived on the mark, which plays the gesture of the site's logo
    /// link: blink twice, glance, redraw the w.
    pub fn frame(&self, at: Duration, pointed_at: Option<Duration>) -> Frame {
        let t = at.as_secs_f64();
        let mut pose = Pose::REST;
        let mut wake = Wake::Never;

        if self.intro {
            let once = Run::once(0.);
            let pop = POP.value(t, once);
            pose.bubble_scale = pop[0];
            pose.bubble_opacity = pop[1].clamp(0., 1.);
            wake = wake.sooner(POP.wake(t, once));

            let open = Run::once(0.3);
            pose.eye_open = OPEN.value(t, open)[0];
            wake = wake.sooner(OPEN.wake(t, open));

            let draw = Run::once(0.5);
            (pose.mouth_start, pose.mouth_end) = dash_window(DRAW.value(t, draw)[0]);
            wake = wake.sooner(DRAW.wake(t, draw));
        }

        let gesture = pointed_at
            .map(|start| start.as_secs_f64())
            .filter(|start| t >= *start);

        // On the site the pointer's blink replaces the idle one while it runs.
        let blink_twice = gesture
            .map(Run::once)
            .filter(|run| BLINK_TWICE.running(t, *run));
        if let Some(run) = blink_twice {
            pose.eye_open *= BLINK_TWICE.value(t, run)[0];
            wake = wake.sooner(BLINK_TWICE.wake(t, run));
        }

        if self.alive {
            let cycles = match self.repeat {
                Repeat::Times(n) => Some(n),
                Repeat::Forever => None,
            };
            let blink = Run {
                delay: self.blink_delay.as_secs_f64(),
                cycles,
            };
            if blink_twice.is_none() {
                pose.eye_open *= BLINK.value(t, blink)[0];
            }
            wake = wake.sooner(BLINK.wake(t, blink));

            let look = Run {
                delay: self.look_delay.as_secs_f64(),
                cycles,
            };
            [pose.face_dx, pose.face_dy] = LOOK.value(t, look);
            wake = wake.sooner(LOOK.wake(t, look));
        }

        if let Some(run) = gesture.map(Run::once) {
            pose.eyes_dx = GLANCE.value(t, run)[0];
            wake = wake.sooner(GLANCE.wake(t, run));
            if REDRAW.running(t, run) {
                (pose.mouth_start, pose.mouth_end) = dash_window(REDRAW.value(t, run)[0]);
                wake = wake.sooner(REDRAW.wake(t, run));
            }
        }

        Frame { pose, wake }
    }

    /// The pose at `at`.
    pub fn pose(&self, at: Duration) -> Pose {
        self.frame(at, None).pose
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::site()
    }
}

/// A [`Timeline`] together with the switches that silence it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playback {
    /// What to play.
    pub timeline: Timeline,
    /// False pauses the mark: it shows [`Pose::REST`] and asks for nothing.
    pub animate: bool,
    /// The user asked for less motion: the mark shows [`Pose::REST`] and
    /// asks for nothing.
    pub reduced_motion: bool,
}

impl Playback {
    /// True when the mark moves at all.
    pub fn is_live(&self) -> bool {
        self.animate && !self.reduced_motion && (self.timeline.intro || self.timeline.alive)
    }

    /// The frame at `at`: [`Timeline::frame`] when live, [`Frame::STILL`]
    /// otherwise.
    pub fn frame(&self, at: Duration, pointed_at: Option<Duration>) -> Frame {
        if self.is_live() {
            self.timeline.frame(at, pointed_at)
        } else {
            Frame::STILL
        }
    }
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            timeline: Timeline::default(),
            animate: true,
            reduced_motion: false,
        }
    }
}

/// How long the pointer's gesture lasts (its longest part, the glance).
pub const GESTURE: Duration = Duration::from_millis(900);

/// A phase between 0 and 3 for [`Timeline::mascot`], derived from `seed`
/// (SplitMix64).
pub fn phase_from_seed(seed: u64) -> f32 {
    let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^= z >> 31;
    // 24 bits fit an f32 exactly, so the result stays below 3.
    ((z >> 40) as f32 / (1u64 << 24) as f32) * 3.
}

/// The visible part of a stroke whose dash is its whole length, shifted by
/// `offset` (CSS `stroke-dasharray: 1` with `stroke-dashoffset`).
fn dash_window(offset: f32) -> (f32, f32) {
    ((-offset).clamp(0., 1.), (1. - offset).clamp(0., 1.))
}

#[derive(Clone, Copy)]
enum Easing {
    Linear,
    /// CSS `cubic-bezier(x1, y1, x2, y2)`.
    Bezier(f64, f64, f64, f64),
}

impl Easing {
    fn apply(self, progress: f64) -> f64 {
        let Easing::Bezier(x1, y1, x2, y2) = self else {
            return progress;
        };
        // Exact at both ends, so a finished movement lands on its keyframe.
        if progress <= 0. {
            return 0.;
        }
        if progress >= 1. {
            return 1.;
        }
        let curve = |a: f64, b: f64, s: f64| {
            let r = 1. - s;
            3. * r * r * s * a + 3. * r * s * s * b + s * s * s
        };
        // x is monotonic in the curve's parameter, so bisect for it.
        let (mut low, mut high) = (0., 1.);
        for _ in 0..40 {
            let middle = (low + high) / 2.;
            if curve(x1, x2, middle) < progress {
                low = middle;
            } else {
                high = middle;
            }
        }
        curve(y1, y2, (low + high) / 2.)
    }
}

const BACK_OUT: Easing = Easing::Bezier(0.34, 1.56, 0.64, 1.);
const STROKE: Easing = Easing::Bezier(0.65, 0., 0.35, 1.);
const LOOK_STEP: Easing = Easing::Bezier(0.45, 0., 0.55, 1.);
const EASE_IN_OUT: Easing = Easing::Bezier(0.42, 0., 0.58, 1.);

/// When a track starts and how often it runs.
#[derive(Clone, Copy)]
struct Run {
    delay: f64,
    /// `None` runs forever.
    cycles: Option<u32>,
}

impl Run {
    fn once(delay: f64) -> Self {
        Self {
            delay,
            cycles: Some(1),
        }
    }
}

/// One CSS animation: keyframes over a duration, the easing applied between
/// each pair of keyframes, as CSS does.
struct Track {
    /// Seconds per cycle.
    duration: f64,
    easing: Easing,
    /// `(offset 0..=1, value)`, in order, from 0 to 1.
    keys: &'static [(f64, [f32; 2])],
}

impl Track {
    fn first(&self) -> [f32; 2] {
        self.keys[0].1
    }

    fn last(&self) -> [f32; 2] {
        self.keys[self.keys.len() - 1].1
    }

    /// The cycle and the position inside it at `t`; `None` before the start
    /// and after the last cycle.
    fn position(&self, t: f64, run: Run) -> Option<(u64, f64)> {
        let local = t - run.delay;
        if local < 0. {
            return None;
        }
        let cycle = (local / self.duration).floor();
        if run.cycles.is_some_and(|cycles| cycle >= f64::from(cycles)) {
            return None;
        }
        Some((cycle as u64, local / self.duration - cycle))
    }

    fn running(&self, t: f64, run: Run) -> bool {
        self.position(t, run).is_some()
    }

    /// The keyframe interval that contains `fraction`.
    fn segment(&self, fraction: f64) -> usize {
        self.keys
            .windows(2)
            .position(|pair| fraction < pair[1].0)
            .unwrap_or(self.keys.len() - 2)
    }

    fn moves(&self, segment: usize) -> bool {
        self.keys[segment].1 != self.keys[segment + 1].1
    }

    /// The value at `t`. It holds the first keyframe before the start and
    /// the last one after the end.
    fn value(&self, t: f64, run: Run) -> [f32; 2] {
        let Some((_, fraction)) = self.position(t, run) else {
            return if t < run.delay {
                self.first()
            } else {
                self.last()
            };
        };
        let segment = self.segment(fraction);
        let (from_offset, from) = self.keys[segment];
        let (to_offset, to) = self.keys[segment + 1];
        if from == to {
            return from;
        }
        let progress = ((fraction - from_offset) / (to_offset - from_offset)).clamp(0., 1.);
        let eased = self.easing.apply(progress) as f32;
        [
            from[0] + (to[0] - from[0]) * eased,
            from[1] + (to[1] - from[1]) * eased,
        ]
    }

    /// When the value changes next, seen from `t`.
    fn wake(&self, t: f64, run: Run) -> Wake {
        let start_of = |cycle: u64, segment: usize| {
            let seconds = run.delay + (cycle as f64 + self.keys[segment].0) * self.duration;
            let wake = Duration::from_secs_f64(seconds);
            if wake.as_secs_f64() > t {
                Wake::At(wake)
            } else {
                // Rounding put `t` a hair before a start it has reached.
                Wake::NextFrame
            }
        };
        let first_move_from =
            |segment: usize| (segment..self.keys.len() - 1).find(|s| self.moves(*s));

        if t < run.delay {
            return first_move_from(0).map_or(Wake::Never, |segment| start_of(0, segment));
        }
        let Some((cycle, fraction)) = self.position(t, run) else {
            return Wake::Never;
        };
        let segment = self.segment(fraction);
        if self.moves(segment) {
            return Wake::NextFrame;
        }
        if let Some(next) = first_move_from(segment + 1) {
            return start_of(cycle, next);
        }
        let more = run
            .cycles
            .is_none_or(|cycles| cycle + 1 < u64::from(cycles));
        match first_move_from(0) {
            Some(next) if more => start_of(cycle + 1, next),
            _ => Wake::Never,
        }
    }
}

/// `wu-pop`: `[scale, opacity]`.
const POP: Track = Track {
    duration: 0.55,
    easing: BACK_OUT,
    keys: &[(0., [0.4, 0.]), (1., [1., 1.])],
};

/// `wu-open`: the eyes' vertical scale.
const OPEN: Track = Track {
    duration: 0.22,
    easing: BACK_OUT,
    keys: &[(0., [0., 0.]), (1., [1., 0.])],
};

/// `wu-draw`: the w's dash offset.
const DRAW: Track = Track {
    duration: 0.6,
    easing: STROKE,
    keys: &[(0., [1., 0.]), (1., [0., 0.])],
};

/// `wu-blink`: the eyes' vertical scale. One blink is 2 % of the cycle.
const BLINK: Track = Track {
    duration: 7.,
    easing: Easing::Linear,
    keys: &[
        (0., [1., 0.]),
        (0.22, [1., 0.]),
        (0.23, [0.1, 0.]),
        (0.24, [1., 0.]),
        (0.83, [1., 0.]),
        (0.84, [0.1, 0.]),
        (0.85, [1., 0.]),
        (1., [1., 0.]),
    ],
};

/// `wu-look`: the face's `[dx, dy]`. The holds between glances read as
/// attention, not drift.
const LOOK: Track = Track {
    duration: 11.,
    easing: LOOK_STEP,
    keys: &[
        (0., [0., 0.]),
        (0.14, [0., 0.]),
        (0.18, [4., 0.]),
        (0.32, [4., 0.]),
        (0.36, [-4., 0.]),
        (0.50, [-4., 0.]),
        (0.54, [0., -3.]),
        (0.64, [0., -3.]),
        (0.68, [3., -2.]),
        (0.76, [3., -2.]),
        (0.80, [0., 0.]),
        (1., [0., 0.]),
    ],
};

/// `wu-glance`: the eyes' dx.
const GLANCE: Track = Track {
    duration: 0.9,
    easing: EASE_IN_OUT,
    keys: &[
        (0., [0., 0.]),
        (0.25, [1.5, 0.]),
        (0.70, [1.5, 0.]),
        (1., [0., 0.]),
    ],
};

/// `wu-blink-twice`: the eyes' vertical scale.
const BLINK_TWICE: Track = Track {
    duration: 0.5,
    easing: Easing::Linear,
    keys: &[
        (0., [1., 0.]),
        (0.14, [0.1, 0.]),
        (0.28, [1., 0.]),
        (0.50, [1., 0.]),
        (0.64, [0.1, 0.]),
        (0.78, [1., 0.]),
        (1., [1., 0.]),
    ],
};

/// `wu-redraw`: the w's dash offset. It runs off its far end, then draws
/// again from its start.
const REDRAW: Track = Track {
    duration: 0.7,
    easing: STROKE,
    keys: &[
        (0., [0., 0.]),
        (0.45, [-1., 0.]),
        (0.46, [1., 0.]),
        (1., [0., 0.]),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(seconds: f64) -> Duration {
        Duration::from_secs_f64(seconds)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// The mascot with the site's default delays: look from 1.8 s, blink
    /// from 2.5 s.
    fn mascot() -> Timeline {
        Timeline::mascot(0.)
    }

    #[test]
    fn it_rests_until_the_first_look() {
        let frame = mascot().frame(Duration::ZERO, None);
        assert_eq!(frame.pose, Pose::REST);
        // The first look starts 14 % into the 11 s cycle that starts at 1.8 s.
        let Wake::At(at) = frame.wake else {
            panic!("expected a scheduled wake, got {:?}", frame.wake);
        };
        assert!((at.as_secs_f64() - (1.8 + 0.14 * 11.)).abs() < 1e-9);
    }

    #[test]
    fn a_blink_takes_140_ms_and_closes_to_a_tenth() {
        let timeline = mascot();
        for shut in [2.5 + 0.23 * 7., 2.5 + 0.84 * 7., 2.5 + 7. + 0.23 * 7.] {
            let open = |at: f64| timeline.pose(secs(at)).eye_open;
            assert!(close(open(shut), 0.1), "shut at {shut}");
            assert!(close(open(shut - 0.035), 0.55), "half way down");
            assert!(close(open(shut + 0.035), 0.55), "half way up");
            assert!(close(open(shut - 0.071), 1.), "open before");
            assert!(close(open(shut + 0.071), 1.), "open after");
        }
    }

    #[test]
    fn it_looks_right_left_up_and_up_right_in_that_order() {
        let timeline = mascot();
        let face = |fraction: f64| {
            let pose = timeline.pose(secs(1.8 + fraction * 11.));
            (pose.face_dx, pose.face_dy)
        };
        let holds = [
            (0.07, (0., 0.)),
            (0.25, (4., 0.)),
            (0.43, (-4., 0.)),
            (0.59, (0., -3.)),
            (0.72, (3., -2.)),
            (0.90, (0., 0.)),
        ];
        for (fraction, (dx, dy)) in holds {
            let (x, y) = face(fraction);
            assert!(close(x, dx) && close(y, dy), "at {fraction}: {x}, {y}");
        }
        // The same again in the second cycle.
        let pose = timeline.pose(secs(1.8 + 11. + 0.25 * 11.));
        assert!(close(pose.face_dx, 4.));
    }

    #[test]
    fn a_glance_takes_440_ms_and_eases_in_and_out() {
        let timeline = mascot();
        let start = 1.8 + 0.14 * 11.;
        let dx = |after: f64| timeline.pose(secs(start + after)).face_dx;
        assert!(close(dx(0.), 0.));
        assert!(close(dx(0.22), 2.), "the curve is symmetric");
        assert!(dx(0.11) < 1., "slow out of the hold: {}", dx(0.11));
        assert!(dx(0.33) > 3., "slow into the hold: {}", dx(0.33));
        assert!(close(dx(0.44), 4.));
    }

    #[test]
    fn the_face_stays_inside_its_bubble() {
        let timeline = mascot().repeat(Repeat::Forever);
        for step in 0..6000 {
            let pose = timeline.pose(secs(f64::from(step) * 0.01));
            assert!(pose.face_dx.abs() <= 4.001);
            assert!((-3.001..=0.001).contains(&pose.face_dy));
            assert!((0.099..=1.001).contains(&pose.eye_open));
            assert_eq!(pose.bubble_scale, 1.);
            assert_eq!((pose.mouth_start, pose.mouth_end), (0., 1.));
        }
    }

    #[test]
    fn the_entrance_pops_the_bubble_opens_the_eyes_and_draws_the_w() {
        let timeline = Timeline::site();
        let start = timeline.pose(Duration::ZERO);
        assert!(close(start.bubble_scale, 0.4));
        assert!(close(start.bubble_opacity, 0.));
        assert!(close(start.eye_open, 0.));
        assert_eq!((start.mouth_start, start.mouth_end), (0., 0.));

        // The back-out curve overshoots before it settles.
        let overshoot = (0..55)
            .map(|ms| timeline.pose(secs(f64::from(ms) * 0.01)).bubble_scale)
            .fold(0f32, f32::max);
        assert!(overshoot > 1.02 && overshoot < 1.12, "{overshoot}");

        // Eyes: shut until 0.3 s, open by 0.52 s.
        assert!(close(timeline.pose(secs(0.29)).eye_open, 0.));
        assert!(close(timeline.pose(secs(0.53)).eye_open, 1.));

        // The w: nothing until 0.5 s, growing from its start, whole at 1.1 s.
        assert_eq!(timeline.pose(secs(0.49)).mouth_end, 0.);
        let half = timeline.pose(secs(0.8));
        assert_eq!(half.mouth_start, 0.);
        assert!(close(half.mouth_end, 0.5), "{}", half.mouth_end);
        assert_eq!(timeline.pose(secs(1.11)), Pose::REST);

        assert_eq!(timeline.frame(secs(0.1), None).wake, Wake::NextFrame);
        assert_eq!(timeline.frame(secs(0.9), None).wake, Wake::NextFrame);
    }

    #[test]
    fn it_rests_for_good_after_two_cycles() {
        let timeline = mascot();
        // Blink: 2.5 + 2 × 7. Look: 1.8 + 2 × 11.
        let last_move = 1.8 + 11. + 0.80 * 11.;
        assert_eq!(
            timeline.frame(secs(last_move - 0.1), None).wake,
            Wake::NextFrame
        );
        for at in [last_move + 0.001, 23.8, 60., 86_400.] {
            assert_eq!(timeline.frame(secs(at), None), Frame::STILL, "at {at}");
        }

        let forever = timeline.repeat(Repeat::Forever);
        for at in [24., 600., 86_400.] {
            assert_ne!(forever.frame(secs(at), None).wake, Wake::Never);
        }
        // Still on the beat a day later.
        let cycles = (86_400f64 / 7.).ceil() * 7.;
        assert!(close(
            forever.pose(secs(2.5 + cycles + 0.23 * 7.)).eye_open,
            0.1
        ));
    }

    /// Follows the wakes the way the element does and returns the times at
    /// which a frame was drawn.
    fn drive(playback: Playback, until: f64, pointed_at: Option<Duration>) -> Vec<f64> {
        const FRAME: f64 = 1. / 60.;
        let mut drawn = Vec::new();
        let mut t = 0.;
        while t < until {
            drawn.push(t);
            match playback.frame(secs(t), pointed_at).wake {
                Wake::NextFrame => t += FRAME,
                Wake::At(at) => {
                    assert!(at.as_secs_f64() > t, "a wake must be in the future");
                    t = at.as_secs_f64();
                }
                Wake::Never => break,
            }
        }
        drawn
    }

    #[test]
    fn nothing_changes_while_it_sleeps() {
        for timeline in [Timeline::site(), mascot(), Timeline::mascot(2.)] {
            let mut t = 0.;
            while t < 30. {
                let frame = timeline.frame(secs(t), None);
                if let Wake::At(at) = frame.wake {
                    let at = at.as_secs_f64();
                    assert!(at > t);
                    for step in 0..20 {
                        let between = t + (at - t) * f64::from(step) / 20.;
                        assert_eq!(timeline.pose(secs(between)), frame.pose, "at {between}");
                    }
                }
                t += 0.0137;
            }
        }
    }

    #[test]
    fn it_draws_only_while_something_moves() {
        let playback = Playback {
            timeline: mascot(),
            ..Playback::default()
        };
        let drawn = drive(playback, 60., None);
        // 4 blinks of 140 ms and 10 glances of 440 ms: about 5 s of motion
        // in the 23.8 s the two cycles last, and nothing after.
        let continuous = 23.8 * 60.;
        assert!(drawn.len() > 250, "{} frames", drawn.len());
        assert!(
            (drawn.len() as f64) < continuous * 0.3,
            "{} frames",
            drawn.len()
        );
        assert!(drawn.last().is_some_and(|last| *last < 23.8));
        // Every movement was drawn: the pose at its deepest point appears.
        let eye_at_first_blink = drawn
            .iter()
            .map(|t| playback.timeline.pose(secs(*t)).eye_open)
            .fold(1f32, f32::min);
        assert!(eye_at_first_blink < 0.25, "{eye_at_first_blink}");
    }

    #[test]
    fn paused_or_reduced_motion_asks_for_no_frames() {
        let paused = Playback {
            animate: false,
            ..Playback::default()
        };
        let reduced = Playback {
            reduced_motion: true,
            ..Playback::default()
        };
        let nothing_to_play = Playback {
            timeline: Timeline {
                intro: false,
                alive: false,
                ..Timeline::site()
            },
            ..Playback::default()
        };
        for playback in [paused, reduced, nothing_to_play] {
            assert!(!playback.is_live());
            for at in [0., 0.2, 1.54 + 2.5, 4.1, 9., 30.] {
                let pointed = Some(secs(at));
                assert_eq!(playback.frame(secs(at), None), Frame::STILL);
                assert_eq!(playback.frame(secs(at + 0.1), pointed), Frame::STILL);
            }
            assert_eq!(drive(playback, 60., None), vec![0.]);
        }
        assert!(Playback::default().is_live());
    }

    #[test]
    fn a_seed_always_gives_the_same_timeline() {
        for seed in [0, 1, 42, u64::MAX] {
            let (a, b) = (Timeline::seeded(seed), Timeline::seeded(seed));
            assert_eq!(a, b);
            let phase = phase_from_seed(seed);
            assert!((0. ..3.).contains(&phase), "{phase}");
            for step in 0..3000 {
                let at = secs(f64::from(step) * 0.01);
                assert_eq!(a.frame(at, None), b.frame(at, None));
            }
        }
        assert_ne!(Timeline::seeded(1), Timeline::seeded(2));
        // Phases spread over the range instead of bunching up.
        let phases: Vec<f32> = (0..64).map(phase_from_seed).collect();
        assert!(phases.iter().any(|phase| *phase < 1.));
        assert!(phases.iter().any(|phase| *phase > 2.));
    }

    #[test]
    fn mascot_phases_are_the_sites() {
        let second = Timeline::mascot(1.);
        assert!((second.look_delay.as_secs_f64() - 4.1).abs() < 1e-6);
        assert!((second.blink_delay.as_secs_f64() - 4.2).abs() < 1e-6);
        assert!(!second.intro && second.alive);
        assert_eq!(second.repeat, Repeat::Times(2));
        assert!(Timeline::site().intro);
        assert_eq!(Timeline::site().intro(false), Timeline::mascot(0.));
    }

    #[test]
    fn pointing_at_it_blinks_twice_glances_and_redraws_the_w() {
        let timeline = mascot();
        // After both cycles: the gesture is all that moves.
        let start = 40.;
        let pointed = Some(secs(start));
        let pose = |after: f64| timeline.frame(secs(start + after), pointed).pose;

        assert_eq!(pose(0.), Pose::REST);
        assert!(close(pose(0.07).eye_open, 0.1));
        assert!(close(pose(0.14).eye_open, 1.));
        assert!(close(pose(0.32).eye_open, 0.1));
        assert!(close(pose(0.45).eye_open, 1.));

        assert!(close(pose(0.4).eyes_dx, 1.5));
        assert!(pose(0.1).eyes_dx > 0. && pose(0.1).eyes_dx < 1.5);

        // The w runs off its far end by 0.315 s, then draws from its start.
        let leaving = pose(0.16);
        assert!(leaving.mouth_start > 0.2 && leaving.mouth_end == 1.);
        let gone = pose(0.3149);
        assert!(gone.mouth_end - gone.mouth_start < 0.01);
        let drawing = pose(0.5);
        assert!(drawing.mouth_start == 0. && drawing.mouth_end > 0.2 && drawing.mouth_end < 1.);

        assert_eq!(
            timeline.frame(secs(start + 0.5), pointed).wake,
            Wake::NextFrame
        );
        assert_eq!(
            timeline.frame(secs(start + GESTURE.as_secs_f64() + 0.001), pointed),
            Frame::STILL
        );
        // A gesture in the future changes nothing yet.
        assert_eq!(timeline.pose(secs(30.)), Pose::REST);
        assert_eq!(timeline.frame(secs(30.), pointed).pose, Pose::REST);
    }

    #[test]
    fn the_gesture_plays_over_the_idle_cycles() {
        let timeline = mascot();
        // Pointed at while it looks right (the hold from 18 % to 32 %).
        let start = 1.8 + 0.2 * 11.;
        let frame = timeline.frame(secs(start + 0.4), Some(secs(start)));
        assert!(close(frame.pose.face_dx, 4.));
        assert!(close(frame.pose.eyes_dx, 1.5));
        assert_eq!(frame.wake, Wake::NextFrame);
    }

    #[test]
    fn css_easing_matches_known_points() {
        assert!((EASE_IN_OUT.apply(0.5) - 0.5).abs() < 1e-6);
        assert!((LOOK_STEP.apply(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(Easing::Linear.apply(0.3), 0.3);
        assert!(BACK_OUT.apply(0.).abs() < 1e-6);
        assert!((BACK_OUT.apply(1.) - 1.).abs() < 1e-6);
        // cubic-bezier(.34, 1.56, .64, 1) peaks near 1.1.
        let peak = (0..=100)
            .map(|step| BACK_OUT.apply(f64::from(step) / 100.))
            .fold(0f64, f64::max);
        assert!(peak > 1.05 && peak < 1.15, "{peak}");
    }
}
