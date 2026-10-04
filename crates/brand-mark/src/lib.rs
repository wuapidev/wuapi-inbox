//! The wuapi mark for GPUI, moving the way it does on the wuapi site.
//!
//! The mark is the lime speech bubble with a face: two eyes and the `w` as
//! its mouth. It pops in from its tail, blinks now and then, looks around
//! inside its bubble, and blinks twice when the pointer lands on it. It
//! never winks or changes expression. It is always lime: bare on dark
//! surfaces, on its ink tile on light ones.
//!
//! It is drawn from vector shapes, split into parts so each can move; there
//! is no image, video or web view behind it.
//!
//! # Use
//!
//! ```no_run
//! use brand_mark::{AnimatedMark, Timeline};
//! use gpui_kit::{div, px, rgb, IntoElement, ParentElement};
//!
//! fn empty_state(light_theme: bool, reduced_motion: bool) -> impl IntoElement {
//!     div().child(
//!         AnimatedMark::new(px(96.))
//!             // Light surfaces show the lime mark on its ink tile.
//!             .tile(light_theme)
//!             // Colours default to the brand's; pass theme tokens to override.
//!             .lime(rgb(0xd4ff3f))
//!             .timeline(Timeline::site())
//!             .reduced_motion(reduced_motion),
//!     )
//! }
//! ```
//!
//! [`AnimatedMark::still`] is the static mark. [`paint_mark`] draws one
//! [`Pose`] from a paint callback.
//!
//! # Cost
//!
//! The element asks for frames only while a part is moving. Between
//! movements it sets one timer for the start of the next one and the window
//! is not redrawn for it. With the site's timeline that is about five
//! seconds of frames in the first 24, and none after. Pausing it, reduced
//! motion, or leaving the screen drops its state and its timer.
//!
//! # Without a window
//!
//! [`motion`] is the animation as a pure function, time in and [`Pose`]
//! out, and [`geometry`] turns a pose into shapes on the mark's grid. Both
//! are tested without GPUI:
//!
//! ```
//! use brand_mark::{Pose, Timeline, Wake};
//! use std::time::Duration;
//!
//! let timeline = Timeline::mascot(0.);
//! // Still, and asleep until the first look 3.34 s in.
//! let frame = timeline.frame(Duration::ZERO, None);
//! assert_eq!(frame.pose, Pose::REST);
//! assert!(matches!(frame.wake, Wake::At(_)));
//! // Half way through the first blink the eyes are a line.
//! let blink = timeline.pose(Duration::from_millis(2500 + 1610));
//! assert!(blink.eye_open < 0.11);
//! ```

#![warn(missing_docs)]

mod element;
pub mod geometry;
pub mod motion;

pub use element::{paint_mark, AnimatedMark, MarkColors, INK, LIME};
pub use motion::{Frame, Playback, Pose, Repeat, Timeline, Wake};
