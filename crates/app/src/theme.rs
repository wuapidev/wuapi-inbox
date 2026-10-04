//! Design tokens.
//!
//! Every colour, size, radius and font the UI uses is defined here, once per
//! theme. Views read them through [`palette`], [`metrics`] and [`fonts`];
//! nothing else in the crate contains a colour literal.
//!
//! The values are the wuapi design system (`brandbook.md` §6 and the tokens
//! of the wuapi site): stone neutrals on paper or near-black, hairline rules
//! instead of shadows, a 6 px radius, Inter with JetBrains Mono for
//! everything technical, and one accent that is a signal and never
//! decoration: lime on dark surfaces, olive on light ones. The layout stays
//! the one every chat client shares.
//!
//! Contrast (WCAG 2.x relative luminance) for the pairs that carry text is
//! checked by the tests at the bottom of this file.

use crate::brand;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{hsla, rgb, App, Global, Hsla, Pixels, Rgba};
use serde::{Deserialize, Serialize};
use std::cell::Cell;

/// The interface sizes on offer, in percent. 100 is the design.
pub const SCALE_STEPS: [u16; 5] = [80, 90, 100, 110, 125];

thread_local! {
    /// The interface size in use, in percent. Per thread: the interface
    /// lives on one thread, and each test window on its own.
    static SCALE: Cell<u16> = const { Cell::new(100) };
    /// How the account's own bubbles are drawn. Per thread, like the size.
    static BUBBLES: Cell<BubbleStyle> = const { Cell::new(BubbleStyle::Brand) };
}

/// How many colours people are told apart by.
pub const SENDER_TONES: usize = 9;

/// How the account's own bubbles are drawn (Settings > Appearance >
/// Message bubbles). Each is a set of tokens, per theme; the incoming
/// bubble is the same in all three.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BubbleStyle {
    /// The brand's lime as a quiet tint: deep olive under light text on
    /// dark, pale lime under ink on light.
    #[default]
    Brand,
    /// The lime fill itself, under ink, in both themes.
    Lime,
    /// No colour: ink on paper, paper on ink.
    Neutral,
}

/// Chooses how the account's own bubbles are drawn. [`apply`] is what puts
/// it on screen.
pub fn set_bubbles(style: BubbleStyle) {
    BUBBLES.with(|bubbles| bubbles.set(style));
}

/// How the account's own bubbles are drawn.
pub fn bubbles() -> BubbleStyle {
    BUBBLES.with(Cell::get)
}

/// How the sizes written in the views relate to the tokens: the tokens
/// below were retuned by hand to a more compact design, and the sizes the
/// views write themselves (paddings, icon sizes, the odd width) follow by
/// this one factor instead of being retyped one by one.
const DENSITY: f32 = 0.855;

/// Sets the interface size, in percent, to the nearest step on offer.
pub fn set_scale(percent: u16) {
    SCALE.with(|scale| scale.set(nearest_step(percent)));
}

/// The step on offer nearest to `percent`.
pub fn nearest_step(percent: u16) -> u16 {
    SCALE_STEPS
        .into_iter()
        .min_by_key(|step| step.abs_diff(percent))
        .unwrap_or(100)
}

/// The step after (`up`) or before the one in use; the same at the ends.
pub fn next_step(percent: u16, up: bool) -> u16 {
    let at = SCALE_STEPS
        .iter()
        .position(|step| *step == nearest_step(percent))
        .unwrap_or(2);
    let next = if up {
        (at + 1).min(SCALE_STEPS.len() - 1)
    } else {
        at.saturating_sub(1)
    };
    SCALE_STEPS[next]
}

/// The interface size in use, as a factor (1.0 is the design).
pub fn scale() -> f32 {
    f32::from(SCALE.with(Cell::get)) / 100.
}

/// Half pixels are as fine as a size gets: text and boxes stay crisp.
fn snap(value: f32) -> f32 {
    (value * 2.).round() / 2.
}

/// A design token at the interface size in use.
fn token(value: f32) -> Pixels {
    gpui_kit::px(snap(value * scale()))
}

/// A size written in a view, at the interface size in use. The views use
/// this `px`, not the component library's.
pub fn px(value: f32) -> Pixels {
    gpui_kit::px(snap(value * DENSITY * scale()))
}

/// A size of a picture in a conversation (an image's box, a sticker): it
/// follows the interface size, but only so far, because a photo at 80 %
/// is hard to read and at 125 % crowds the pane.
pub fn media_px(value: f32) -> Pixels {
    gpui_kit::px(snap(value * DENSITY * scale().clamp(0.9, 1.1)))
}

/// One device-independent pixel, at every interface size: the rules
/// between panes and the arms of the grid marks stay hairlines.
pub fn hairline() -> Pixels {
    gpui_kit::px(1.)
}

/// Which theme is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Appearance {
    /// Light theme.
    Light,
    /// Dark theme.
    Dark,
}

/// The colours of one theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Which theme this is.
    pub appearance: Appearance,

    // Surfaces
    /// The page: rail, chat list, conversation, header and composer bars.
    pub background: Hsla,
    /// Raised pieces on the page: fields, incoming bubbles, chips.
    pub surface: Hsla,
    /// Quiet fill: the open chat's row, the selected filter, avatars.
    pub muted: Hsla,
    /// Hairlines: pane rules, field and bubble outlines.
    pub border: Hsla,
    /// The crosshair where two rules meet, one step stronger than `border`.
    pub grid_mark: Hsla,
    /// Row or control under the pointer.
    pub hover: Hsla,
    /// The veil over the window behind a panel.
    pub scrim: Hsla,
    /// What is lifted over the window: the palette, sheets, menus. Told
    /// apart from the page behind it by the veil, its outline and a rule
    /// of the accent, not by a heavy shadow.
    pub elevated: Hsla,
    /// The outline of something lifted: stronger than a hairline.
    pub elevated_border: Hsla,
    /// The row the keyboard is on, in something lifted.
    pub elevated_selected: Hsla,
    /// The soft shade under something lifted.
    pub elevated_shadow: Hsla,
    /// What a QR code is drawn on: white in both themes, so a phone's
    /// camera reads it.
    pub qr_paper: Hsla,
    /// The colours a number or a group can be given in the rail: quiet
    /// enough to sit next to the accent, dark enough for `on_rail` text.
    pub rail: [Hsla; 6],
    /// Initials or an emoji on one of the `rail` colours.
    pub on_rail: Hsla,
    /// The round button on a file that is not here yet: dark enough to
    /// read on a picture's placeholder and on a bubble, in both themes.
    pub media_button: Hsla,
    /// The arrow, the ring and the cross on it.
    pub on_media_button: Hsla,
    /// The hairline around something lifted off the page to be dragged.
    pub lift_outline: Hsla,
    /// The faint glow under it: what stands in for a shadow.
    pub lift_glow: Hsla,

    // Text
    /// Names, message text.
    pub text: Hsla,
    /// Previews, subtitles, timestamps, placeholders.
    pub text_muted: Hsla,
    /// Glyphs that only decorate: pin, mute, search, scrollbars.
    pub text_faint: Hsla,
    /// Toolbar icons.
    pub icon: Hsla,

    // Signal
    /// The accent on the page: connected, typing, unread time, read ticks in
    /// the chat list. Lime on dark, olive on light.
    pub accent: Hsla,
    /// The lime fill, the same in both themes: the send button and unread
    /// badges.
    pub accent_fill: Hsla,
    /// Ink on top of `accent_fill`.
    pub on_accent_fill: Hsla,
    /// Errors: failed messages, a logged-out number.
    pub danger: Hsla,
    /// A number that is connecting or reconnecting.
    pub warning: Hsla,
    /// Unread badge of a muted chat.
    pub badge_muted: Hsla,
    /// Count on a muted chat's badge.
    pub on_badge_muted: Hsla,

    // Bubbles
    /// Incoming bubble.
    pub bubble_in: Hsla,
    /// Outline of an incoming bubble.
    pub bubble_in_border: Hsla,
    /// Outgoing bubble.
    pub bubble_out: Hsla,
    /// Message text in an outgoing bubble.
    pub on_bubble_out: Hsla,
    /// Time, ticks and secondary text in an incoming bubble.
    pub meta_in: Hsla,
    /// Time, ticks and secondary text in an outgoing bubble.
    pub meta_out: Hsla,
    /// Quote or attachment block inside an incoming bubble.
    pub quote_in: Hsla,
    /// Quote or attachment block inside an outgoing bubble.
    pub quote_out: Hsla,
    /// Read ticks inside an outgoing bubble: the accent for that bubble's
    /// fill, which is the opposite of the page's.
    pub tick_read: Hsla,
    /// Outline of an outgoing bubble.
    pub bubble_out_border: Hsla,
    /// What stands out as text in an outgoing bubble: links, mentions, the
    /// chosen option of a poll, the played part of a voice note.
    pub signal_out: Hsla,
    /// What a search found, marked under the text of a bubble.
    pub find_mark: Hsla,
    /// Selected text in an incoming bubble: a wash under it.
    pub selection_in: Hsla,
    /// Selected text in an outgoing bubble.
    pub selection_out: Hsla,
    /// The outline of the message the keyboard is on. It is drawn outside
    /// the bubble, on the page, so it reads the same on every bubble.
    pub focus_ring: Hsla,
    /// The play button of a voice note in an incoming bubble.
    pub play_in: Hsla,
    /// The glyph on it.
    pub on_play_in: Hsla,
    /// The play button of a voice note in an outgoing bubble.
    pub play_out: Hsla,
    /// The glyph on it.
    pub on_play_out: Hsla,

    // People
    /// The colours people are told apart by, as text on an incoming bubble
    /// and on the page: a sender's name, the bar and the name of a quote.
    /// A person keeps one of them everywhere (see `ui/senders.rs`).
    pub sender: [Hsla; SENDER_TONES],
    /// The same colours, in the same order, for text in an outgoing bubble.
    pub sender_out: [Hsla; SENDER_TONES],
    /// The same colours as the fill of an avatar without a picture.
    pub sender_fill: [Hsla; SENDER_TONES],
    /// Initials on one of the `sender_fill` colours.
    pub on_sender_fill: Hsla,

    // The conversation's wallpaper
    /// The lines of the pattern behind the messages: barely off the page.
    pub wallpaper: Hsla,
    /// The marks in it, a breath of the accent.
    pub wallpaper_accent: Hsla,

    // Identity
    /// Initials on an avatar.
    pub on_avatar: Hsla,
    /// The logo for this theme's page, as an asset path. Always the lime
    /// mark: bare on dark, on its ink tile on light.
    pub logo: &'static str,
}

/// The brand's raw colours. Only the two palettes below use them.
mod raw {
    pub const PAPER: u32 = 0xfafaf9;
    pub const WHITE: u32 = 0xffffff;
    pub const INK: u32 = 0x0c0a09;
    pub const BLACK: u32 = 0x0a0a0a;
    pub const STONE_100: u32 = 0xf5f5f4;
    pub const STONE_200: u32 = 0xe7e5e4;
    pub const STONE_400: u32 = 0xa8a29e;
    pub const STONE_450: u32 = 0x8c8580;
    pub const STONE_500: u32 = 0x78716c;
    pub const STONE_600: u32 = 0x57534e;
    pub const NIGHT_SURFACE: u32 = 0x141414;
    pub const NIGHT_MUTED: u32 = 0x1c1c1c;
    pub const NIGHT_BORDER: u32 = 0x262626;
    /// One step above the night's surface: what is lifted over the window.
    pub const NIGHT_RAISED: u32 = 0x242424;
    /// The row the keyboard is on, there.
    pub const NIGHT_PICKED: u32 = 0x363636;
    /// Its outline.
    pub const NIGHT_EDGE: u32 = 0x5a5a5a;
    pub const LIME: u32 = 0xd4ff3f;
    /// The rail's own colours: olive (the brand's accent on paper), then
    /// teal, blue, violet, rose and amber at the same depth.
    pub const RAIL_COLOURS: [u32; 6] = [0x4d7c0f, 0x0f766e, 0x1d4ed8, 0x6d28d9, 0xbe123c, 0xb45309];
    pub const OLIVE: u32 = 0x4d7c0f;
    /// Olive one step deeper: text on a lime tint.
    pub const OLIVE_DEEP: u32 = 0x3f6212;
    /// Olive at its deepest: text on the lime fill.
    pub const OLIVE_INK: u32 = 0x365314;
    /// People, for dark grounds: lime (the brand's), amber, orange, rose,
    /// pink, purple, indigo, blue and cyan. No green: nothing here may
    /// read as WhatsApp's.
    pub const SENDER_TINTS: [u32; 9] = [
        0xd4ff3f, 0xfbbf24, 0xfb923c, 0xfb7185, 0xf472b6, 0xc084fc, 0x818cf8, 0x60a5fa, 0x22d3ee,
    ];
    /// The same people for light grounds, olive standing in for lime.
    pub const SENDER_DEEP: [u32; 9] = [
        0x4d7c0f, 0xb45309, 0xc2410c, 0xbe123c, 0xbe185d, 0x7e22ce, 0x4338ca, 0x1d4ed8, 0x0e7490,
    ];
    /// One step deeper, for grounds that are light but not white: the
    /// lime fill, and a quote on a paper bubble.
    pub const SENDER_DEEPER: [u32; 9] = [
        0x3f6212, 0x92400e, 0x9a3412, 0x9f1239, 0x9d174d, 0x6b21a8, 0x3730a3, 0x1e40af, 0x155e75,
    ];
    pub const RED_700: u32 = 0xb91c1c;
    pub const RED_400: u32 = 0xf87171;
    pub const AMBER_700: u32 = 0xa16207;
    pub const AMBER_400: u32 = 0xfacc15;
}

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

/// `amount` of `top` over `base`, as an opaque colour: what CSS calls
/// `color-mix(in srgb, top amount, base)`.
fn mix(top: u32, base: u32, amount: f32) -> Hsla {
    let (top, base) = (rgb(top), rgb(base));
    let channel = |t: f32, b: f32| t * amount + b * (1. - amount);
    Rgba {
        r: channel(top.r, base.r),
        g: channel(top.g, base.g),
        b: channel(top.b, base.b),
        a: 1.,
    }
    .into()
}

impl Palette {
    /// The light theme.
    pub fn light() -> Self {
        use raw::*;
        Self {
            appearance: Appearance::Light,
            background: hex(PAPER),
            surface: hex(WHITE),
            muted: hex(STONE_100),
            border: hex(STONE_200),
            grid_mark: mix(INK, STONE_200, 0.28),
            hover: mix(STONE_100, PAPER, 0.6),
            scrim: hex(INK).opacity(0.46),
            elevated: hex(WHITE),
            elevated_border: hex(STONE_400),
            elevated_selected: hex(STONE_200),
            elevated_shadow: hex(INK).opacity(0.16),
            qr_paper: hex(WHITE),
            rail: RAIL_COLOURS.map(hex),
            on_rail: hex(PAPER),
            media_button: hex(INK).opacity(0.72),
            on_media_button: hex(PAPER),
            lift_outline: hex(STONE_600),
            lift_glow: hex(INK).opacity(0.18),
            text: hex(INK),
            text_muted: hex(STONE_600),
            text_faint: hex(STONE_500),
            icon: hex(STONE_600),
            accent: hex(OLIVE),
            accent_fill: hex(LIME),
            on_accent_fill: hex(BLACK),
            danger: hex(RED_700),
            warning: hex(AMBER_700),
            badge_muted: hex(STONE_200),
            on_badge_muted: hex(INK),
            bubble_in: hex(WHITE),
            bubble_in_border: hex(STONE_200),
            bubble_out: hex(INK),
            on_bubble_out: hex(PAPER),
            meta_in: hex(STONE_600),
            meta_out: mix(PAPER, INK, 0.7),
            quote_in: hex(STONE_100),
            quote_out: mix(PAPER, INK, 0.12),
            tick_read: hex(LIME),
            bubble_out_border: hex(INK),
            signal_out: hex(LIME),
            focus_ring: hex(OLIVE),
            find_mark: hex(AMBER_400).opacity(0.5),
            selection_in: hex(OLIVE).opacity(0.28),
            selection_out: hex(PAPER).opacity(0.24),
            play_in: hex(LIME),
            on_play_in: hex(BLACK),
            play_out: hex(LIME),
            on_play_out: hex(BLACK),
            sender: SENDER_DEEP.map(hex),
            sender_out: SENDER_TINTS.map(hex),
            sender_fill: SENDER_DEEP.map(hex),
            on_sender_fill: hex(PAPER),
            wallpaper: mix(INK, PAPER, 0.08),
            wallpaper_accent: mix(OLIVE, PAPER, 0.11),
            on_avatar: hex(STONE_600),
            logo: brand::APP_ICON,
        }
        .with_bubbles(BubbleStyle::Brand)
    }

    /// The dark theme.
    pub fn dark() -> Self {
        use raw::*;
        Self {
            appearance: Appearance::Dark,
            background: hex(BLACK),
            surface: hex(NIGHT_SURFACE),
            muted: hex(NIGHT_MUTED),
            border: hex(NIGHT_BORDER),
            grid_mark: mix(PAPER, NIGHT_BORDER, 0.22),
            hover: mix(NIGHT_MUTED, BLACK, 0.6),
            scrim: hex(BLACK).opacity(0.64),
            elevated: hex(NIGHT_RAISED),
            elevated_border: hex(NIGHT_EDGE),
            elevated_selected: hex(NIGHT_PICKED),
            elevated_shadow: hex(BLACK).opacity(0.5),
            qr_paper: hex(WHITE),
            rail: RAIL_COLOURS.map(hex),
            on_rail: hex(PAPER),
            media_button: hex(BLACK).opacity(0.72),
            on_media_button: hex(PAPER),
            lift_outline: hex(STONE_400),
            lift_glow: hex(LIME).opacity(0.22),
            text: hex(PAPER),
            text_muted: hex(STONE_400),
            text_faint: hex(STONE_450),
            icon: hex(STONE_400),
            accent: hex(LIME),
            accent_fill: hex(LIME),
            on_accent_fill: hex(BLACK),
            danger: hex(RED_400),
            warning: hex(AMBER_400),
            badge_muted: hex(NIGHT_BORDER),
            on_badge_muted: hex(PAPER),
            bubble_in: hex(NIGHT_SURFACE),
            bubble_in_border: hex(NIGHT_BORDER),
            bubble_out: hex(PAPER),
            on_bubble_out: hex(BLACK),
            meta_in: hex(STONE_400),
            meta_out: mix(BLACK, PAPER, 0.7),
            quote_in: hex(NIGHT_MUTED),
            quote_out: mix(BLACK, PAPER, 0.08),
            tick_read: hex(OLIVE),
            bubble_out_border: hex(PAPER),
            signal_out: hex(OLIVE_DEEP),
            focus_ring: hex(LIME),
            find_mark: hex(AMBER_400).opacity(0.38),
            selection_in: hex(LIME).opacity(0.28),
            selection_out: hex(BLACK).opacity(0.2),
            play_in: hex(LIME),
            on_play_in: hex(BLACK),
            play_out: hex(BLACK),
            on_play_out: hex(LIME),
            sender: SENDER_TINTS.map(hex),
            sender_out: SENDER_DEEPER.map(hex),
            sender_fill: SENDER_DEEP.map(hex),
            on_sender_fill: hex(PAPER),
            wallpaper: mix(PAPER, BLACK, 0.1),
            wallpaper_accent: mix(LIME, BLACK, 0.12),
            on_avatar: hex(STONE_400),
            logo: brand::MARK,
        }
        .with_bubbles(BubbleStyle::Brand)
    }

    /// This palette with the account's own bubbles drawn as `style`.
    ///
    /// Everything an outgoing bubble is drawn with changes together: its
    /// fill and outline, the text, the time, the quote block, the ticks,
    /// links, the play button, and the colours people are named in on it.
    pub fn with_bubbles(mut self, style: BubbleStyle) -> Self {
        use raw::*;
        // The fill, as a colour to mix the others from.
        let over = |top: u32, amount: f32, base: Hsla| -> Hsla {
            let (top, base) = (rgb(top), Rgba::from(base));
            let channel = |t: f32, b: f32| t * amount + b * (1. - amount);
            Rgba {
                r: channel(top.r, base.r),
                g: channel(top.g, base.g),
                b: channel(top.b, base.b),
                a: 1.,
            }
            .into()
        };
        match (style, self.appearance) {
            // Deep olive: lime at 14 % over the page's black. Quiet enough
            // for a long thread, and plainly ours.
            (BubbleStyle::Brand, Appearance::Dark) => {
                let fill = mix(LIME, BLACK, 0.14);
                self.bubble_out = fill;
                self.bubble_out_border = mix(LIME, BLACK, 0.24);
                self.on_bubble_out = hex(PAPER);
                self.meta_out = over(PAPER, 0.66, fill);
                self.quote_out = over(BLACK, 0.38, fill);
                self.tick_read = hex(LIME);
                self.signal_out = hex(LIME);
                self.play_out = hex(LIME);
                self.on_play_out = hex(BLACK);
                self.sender_out = SENDER_TINTS.map(hex);
            }
            // Pale lime: lime at 40 % over white, under ink.
            (BubbleStyle::Brand, Appearance::Light) => {
                let fill = mix(LIME, WHITE, 0.4);
                self.bubble_out = fill;
                self.bubble_out_border = mix(OLIVE, WHITE, 0.3);
                self.on_bubble_out = hex(INK);
                self.meta_out = over(INK, 0.68, fill);
                self.quote_out = over(WHITE, 0.55, fill);
                self.tick_read = hex(OLIVE);
                self.signal_out = hex(OLIVE_DEEP);
                self.play_out = hex(OLIVE_DEEP);
                self.on_play_out = hex(PAPER);
                self.sender_out = SENDER_DEEP.map(hex);
            }
            // The lime fill, in both themes, as the send button has it.
            (BubbleStyle::Lime, _) => {
                let fill = hex(LIME);
                self.bubble_out = fill;
                self.bubble_out_border = fill;
                self.on_bubble_out = hex(BLACK);
                self.meta_out = over(BLACK, 0.7, fill);
                self.quote_out = over(WHITE, 0.5, fill);
                self.tick_read = hex(BLACK);
                self.signal_out = hex(OLIVE_INK);
                self.play_out = hex(BLACK);
                self.on_play_out = hex(LIME);
                self.sender_out = SENDER_DEEPER.map(hex);
            }
            // Ink on paper.
            (BubbleStyle::Neutral, Appearance::Light) => {
                self.bubble_out = hex(INK);
                self.bubble_out_border = hex(INK);
                self.on_bubble_out = hex(PAPER);
                self.meta_out = mix(PAPER, INK, 0.7);
                self.quote_out = mix(PAPER, INK, 0.12);
                self.tick_read = hex(LIME);
                self.signal_out = hex(LIME);
                self.play_out = hex(LIME);
                self.on_play_out = hex(BLACK);
                self.sender_out = SENDER_TINTS.map(hex);
            }
            // Paper on ink.
            (BubbleStyle::Neutral, Appearance::Dark) => {
                self.bubble_out = hex(PAPER);
                self.bubble_out_border = hex(PAPER);
                self.on_bubble_out = hex(BLACK);
                self.meta_out = mix(BLACK, PAPER, 0.7);
                self.quote_out = mix(BLACK, PAPER, 0.08);
                self.tick_read = hex(OLIVE);
                self.signal_out = hex(OLIVE_DEEP);
                self.play_out = hex(BLACK);
                self.on_play_out = hex(LIME);
                self.sender_out = SENDER_DEEPER.map(hex);
            }
        }
        // A selection in the account's own bubble is a wash of its text
        // colour, whatever the style: it shows on the lime fill too.
        self.selection_out = self.on_bubble_out.opacity(0.22);
        self
    }

    /// The palette of an appearance.
    pub fn of(appearance: Appearance) -> Self {
        match appearance {
            Appearance::Light => Self::light(),
            Appearance::Dark => Self::dark(),
        }
    }

    /// True for the dark theme.
    pub fn is_dark(&self) -> bool {
        self.appearance == Appearance::Dark
    }
}

/// Font families. Both are bundled with the application (see
/// `assets/ASSETS.md`) and loaded by [`install_fonts`].
pub mod fonts {
    /// Interface and message text.
    pub const SANS: &str = "Inter";
    /// Everything technical: times, counts, sizes, labels.
    pub const MONO: &str = "JetBrains Mono";
    /// The OpenType features the wuapi site turns on for Inter: the
    /// single-storey `a` and open digits.
    pub const SANS_FEATURES: [(&str, u32); 2] = [("cv11", 1), ("ss01", 1)];
}

/// Sizes and radii, shared by both themes.
/// Sizes, at the interface size in use.
///
/// The numbers are the design at 100 %; every one is multiplied by the
/// interface scale (Settings > Appearance > Interface size) when it is
/// read, so they are functions, named like the constants they stand for.
/// Views take every size from here or from [`px`], never from the
/// component library's own `px`, which is how the whole interface scales
/// as one.
#[allow(non_snake_case)]
pub mod metrics {
    use super::{token, Pixels};

    /// Width of the account rail.
    pub fn RAIL_WIDTH() -> Pixels {
        token(53.0)
    }
    /// Width of the chat list pane, at its widest by default: it takes about
    /// 30 % of the window between [`LIST_DEFAULT_MIN`] and this, until the
    /// user drags its edge.
    pub fn LIST_WIDTH() -> Pixels {
        token(323.0)
    }
    /// The narrowest the chat list gets by itself.
    pub fn LIST_DEFAULT_MIN() -> Pixels {
        token(266.0)
    }
    /// The narrowest the user can drag the chat list.
    pub fn LIST_MIN() -> Pixels {
        token(228.0)
    }
    /// The widest the user can drag the chat list.
    pub fn LIST_MAX() -> Pixels {
        token(494.0)
    }
    /// The least width the conversation keeps, whatever the list is dragged to.
    pub fn THREAD_MIN() -> Pixels {
        token(342.0)
    }
    /// Height of the top strip of every pane. Their bottom rules line up
    /// into one line across the window.
    pub fn HEADER_HEIGHT() -> Pixels {
        token(47.5)
    }
    /// Height of one chat row.
    pub fn CHAT_ROW_HEIGHT() -> Pixels {
        token(59.0)
    }
    /// Avatar in a chat row.
    pub fn AVATAR_LARGE() -> Pixels {
        token(36.0)
    }
    /// Avatar in the conversation header and the account rail.
    pub fn AVATAR_MEDIUM() -> Pixels {
        token(30.5)
    }
    /// A sender's avatar next to their bubbles, in a group.
    pub fn AVATAR_SMALL() -> Pixels {
        token(26.0)
    }
    /// The room between that avatar and the bubbles.
    pub fn AVATAR_GAP() -> Pixels {
        token(7.0)
    }
    /// Corner radius of a bubble in the conversation.
    pub fn BUBBLE_CORNER() -> Pixels {
        token(12.0)
    }
    /// The corners two bubbles of one run turn to each other, on the
    /// sender's side.
    pub fn BUBBLE_JOINT() -> Pixels {
        token(4.0)
    }
    /// Room above the first bubble of a run.
    pub fn RUN_GAP() -> Pixels {
        token(10.0)
    }
    /// Room between two bubbles of one run.
    pub fn RUN_JOINT() -> Pixels {
        token(2.0)
    }
    /// Height of a pill on the conversation's page: the day, the unread
    /// count.
    pub fn PILL() -> Pixels {
        token(21.0)
    }
    /// Corner radius of the composer's field.
    pub fn COMPOSER_RADIUS() -> Pixels {
        token(16.0)
    }
    /// The width of what a voice note's bubble holds: the same for every
    /// one of them.
    pub fn AUDIO_WIDTH() -> Pixels {
        token(236.0)
    }
    /// The play button of a voice note.
    pub fn AUDIO_BUTTON() -> Pixels {
        token(32.0)
    }
    /// One tile of the conversation's wallpaper.
    pub fn WALLPAPER_TILE() -> Pixels {
        token(168.0)
    }
    /// A toolbar button.
    pub fn CONTROL() -> Pixels {
        token(28.5)
    }
    /// Corner radius of a bubble.
    pub fn BUBBLE_RADIUS() -> Pixels {
        token(7.5)
    }
    /// Corner radius of what is lifted over the window: the palette, the
    /// sheets, the menus.
    pub fn PANEL_RADIUS() -> Pixels {
        token(10.0)
    }
    /// Width of the command palette.
    pub fn PALETTE_WIDTH() -> Pixels {
        token(640.0)
    }
    /// Corner radius of fields, buttons, chips and blocks inside a bubble.
    pub fn RADIUS() -> Pixels {
        token(5.5)
    }
    /// Widest a bubble may get.
    pub fn BUBBLE_MAX_WIDTH() -> Pixels {
        token(494.0)
    }
    /// The room kept between the newest message and the composer.
    pub fn THREAD_INSET() -> Pixels {
        token(11.5)
    }
    /// How far the outline of the message the keyboard is on stands out
    /// of its bubble: the room between two bubbles of one run, so it is
    /// never under the next one.
    pub fn FOCUS_OUT() -> Pixels {
        RUN_JOINT()
    }
    /// The room the first and the last row of a conversation keep for
    /// that outline, inside the list (what is outside it is not painted).
    pub fn FOCUS_ROOM() -> Pixels {
        token(4.0)
    }
    /// Length of a crosshair's arms, end to end.
    pub fn GRID_MARK() -> Pixels {
        token(10.5)
    }
    /// The sign-in code: large, in the mono face.
    pub fn TEXT_CODE() -> Pixels {
        token(28.5)
    }
    /// Height of a button with a label.
    pub fn BUTTON() -> Pixels {
        token(32.5)
    }
    /// Width of the sign-in column and of the settings menu's widest line.
    pub fn FORM_WIDTH() -> Pixels {
        token(361.0)
    }
    /// The frame around the mark on the start and sign-in screens.
    pub fn MARK_FRAME() -> Pixels {
        token(95.0)
    }
    /// The mark inside that frame.
    pub fn MARK() -> Pixels {
        token(42.0)
    }
    /// The frame around the mark on the welcome screen, where it is the
    /// hero.
    pub fn HERO_FRAME() -> Pixels {
        token(142.5)
    }
    /// The mark inside that frame.
    pub fn HERO_MARK() -> Pixels {
        token(64.5)
    }
    /// The product name on the start screen.
    pub fn TEXT_HERO() -> Pixels {
        token(22.0)
    }
    /// Headings of the sign-in screen and of empty states.
    pub fn TEXT_DISPLAY() -> Pixels {
        token(19.0)
    }
    /// Pane titles ("Chats").
    pub fn TEXT_TITLE() -> Pixels {
        token(14.5)
    }
    /// Chat names, header title.
    pub fn TEXT_NAME() -> Pixels {
        token(13.0)
    }
    /// Message text, composer text.
    pub fn TEXT_BODY() -> Pixels {
        token(12.5)
    }
    /// Previews, subtitles, quotes.
    pub fn TEXT_SMALL() -> Pixels {
        token(11.5)
    }
    /// Timestamps, badges, labels: set in the mono face.
    pub fn TEXT_META() -> Pixels {
        token(10.0)
    }
    /// Line height of message text.
    pub fn LINE_BODY() -> Pixels {
        token(17.5)
    }
}

struct ActivePalette(Palette);

impl Global for ActivePalette {}

/// The active palette.
pub fn palette(cx: &App) -> Palette {
    cx.global::<ActivePalette>().0
}

/// The active palette, if a theme has been applied yet.
pub fn try_palette(cx: &App) -> Option<Palette> {
    cx.try_global::<ActivePalette>().map(|active| active.0)
}

/// Registers the bundled fonts. Call once, before the first window opens.
pub fn install_fonts(cx: &App) {
    if let Err(error) = cx.text_system().add_fonts(brand::fonts()) {
        // The system's default faces take over; nothing else changes.
        tracing::warn!(%error, "could not load the bundled fonts");
    }
}

/// What stories (status) are drawn with: the colours a text story can be
/// posted on, and the sizes of the ring, the viewer's bars and its card.
///
/// A story's own colour is content, chosen by its author; the text on it
/// is white (WhatsApp's), whichever theme this is. Sizes are functions, at
/// the interface size in use, like [`metrics`].
#[allow(non_snake_case)]
pub mod story {
    use super::{px, rgb, Hsla};

    /// The backgrounds of the post composer: WhatsApp's own palette of
    /// flat colours, `0xRRGGBB`, each dark enough to carry white text.
    pub const BACKGROUNDS: [u32; 12] = [
        0x0B_6E_4F, 0x1D_4E_89, 0x5B_2A_86, 0xB0_3A_2E, 0xB5_5A_0E, 0x6B_4F_1D, 0x2B_5F_7A,
        0x8E_2A_5B, 0x3B_3F_46, 0x1F_6F_5C, 0x7A_3E_9D, 0x0E_1F_2D,
    ];

    /// The colour of a story's background.
    pub fn background(rgb_value: u32) -> Hsla {
        rgb(rgb_value & 0xFF_FF_FF).into()
    }

    /// The words on a text story, and on a picture's scrim.
    pub fn on_background() -> Hsla {
        rgb(0xFF_FF_FF).into()
    }

    /// The scrim behind a caption or the viewer's header over a picture:
    /// dark, whatever the picture is.
    pub fn scrim() -> Hsla {
        rgb(0x00_00_00).into()
    }

    /// The side of the story avatar in the Status list, ring excluded.
    pub fn AVATAR() -> gpui_kit::Pixels {
        px(40.)
    }
    /// The ring's stroke.
    pub fn RING_STROKE() -> gpui_kit::Pixels {
        px(2.5)
    }
    /// The space between a picture and its ring.
    pub fn RING_GAP() -> gpui_kit::Pixels {
        px(3.)
    }
    /// The height of one progress segment of the viewer.
    pub fn PROGRESS_HEIGHT() -> gpui_kit::Pixels {
        px(3.)
    }
    /// The width of the card a story is drawn on, at most; the pane's
    /// width less its margins otherwise.
    pub fn CARD_WIDTH() -> gpui_kit::Pixels {
        px(420.)
    }
    /// The height of a row of the Status list.
    pub fn ROW() -> gpui_kit::Pixels {
        px(64.)
    }
    /// The side of a colour swatch of the composer.
    pub fn SWATCH() -> gpui_kit::Pixels {
        px(28.)
    }
    /// The side of a person's picture in the strip of updates above the
    /// chats, ring excluded.
    pub fn STRIP_AVATAR() -> gpui_kit::Pixels {
        px(40.)
    }
    /// The width of one person's cell in the strip.
    pub fn STRIP_CELL() -> gpui_kit::Pixels {
        px(66.)
    }
    /// The height of the strip while it is open.
    pub fn STRIP() -> gpui_kit::Pixels {
        px(86.)
    }
    /// The height of the strip while it is folded: one line.
    pub fn STRIP_FOLDED() -> gpui_kit::Pixels {
        px(30.)
    }
    /// The side of the dot that says there is something new, and of the
    /// "+" on the account's own picture.
    pub fn BADGE() -> gpui_kit::Pixels {
        px(16.)
    }
}

/// Switches theme: installs the palette and aligns the component library
/// (text fields, scrollbars) with it.
pub fn apply(appearance: Appearance, cx: &mut App) {
    let palette = Palette::of(appearance).with_bubbles(bubbles());
    cx.set_global(ActivePalette(palette));

    let mode = match appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.colors.background = palette.background;
        theme.colors.foreground = palette.text;
        theme.colors.muted_foreground = palette.text_muted;
        theme.colors.caret = palette.accent;
        theme.colors.primary = palette.accent;
        theme.colors.ring = palette.accent;
        theme.colors.selection = palette.accent.opacity(0.3);
        theme.colors.border = palette.border;
        theme.colors.scrollbar = hsla(0., 0., 0., 0.);
        theme.colors.scrollbar_thumb = palette.text_faint.opacity(0.45);
        theme.colors.scrollbar_thumb_hover = palette.text_faint.opacity(0.7);
        theme.font_family = fonts::SANS.into();
        theme.mono_font_family = fonts::MONO.into();
        theme.font_size = metrics::TEXT_BODY();
        theme.radius = metrics::RADIUS();
        theme.radius_lg = metrics::BUBBLE_RADIUS();
    });
    cx.refresh_windows();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.x contrast ratio between two opaque colours.
    fn contrast(a: Hsla, b: Hsla) -> f32 {
        fn luminance(colour: Hsla) -> f32 {
            let rgba = Rgba::from(colour);
            assert!(rgba.a > 0.999, "contrast needs opaque colours");
            let linear = |c: f32| {
                if c <= 0.03928 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * linear(rgba.r) + 0.7152 * linear(rgba.g) + 0.0722 * linear(rgba.b)
        }
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Text pairs must reach AA for small text; glyphs and ticks, which are
    /// graphics, 3:1.
    #[test]
    fn text_and_ticks_stay_legible_in_both_themes() {
        const TEXT: f32 = 4.5;
        const GRAPHIC: f32 = 3.0;
        for p in [Palette::light(), Palette::dark()] {
            let theme = p.appearance;
            let pairs = [
                ("text on the page", p.text, p.background, TEXT),
                ("text on the open row", p.text, p.muted, TEXT),
                ("muted text on the page", p.text_muted, p.background, TEXT),
                ("muted text on the open row", p.text_muted, p.muted, TEXT),
                ("muted text in a field", p.text_muted, p.surface, TEXT),
                ("accent text on the page", p.accent, p.background, TEXT),
                ("accent text on the open row", p.accent, p.muted, TEXT),
                ("danger text on the page", p.danger, p.background, TEXT),
                ("warning text on the page", p.warning, p.background, TEXT),
                ("incoming text", p.text, p.bubble_in, TEXT),
                ("incoming time", p.meta_in, p.bubble_in, TEXT),
                ("incoming quote", p.meta_in, p.quote_in, TEXT),
                ("incoming link", p.accent, p.bubble_in, TEXT),
                ("incoming link in a block", p.accent, p.quote_in, TEXT),
                ("pill text", p.text_muted, p.surface, TEXT),
                ("unread pill", p.accent, p.surface, TEXT),
                (
                    "initials on a person's colour",
                    p.on_sender_fill,
                    p.sender_fill[0],
                    TEXT,
                ),
                ("play glyph, incoming", p.on_play_in, p.play_in, GRAPHIC),
                ("unread badge", p.on_accent_fill, p.accent_fill, TEXT),
                ("muted unread badge", p.on_badge_muted, p.badge_muted, TEXT),
                ("initials", p.on_avatar, p.muted, TEXT),
                ("read tick in the list", p.accent, p.background, GRAPHIC),
                ("glyph on the page", p.text_faint, p.background, GRAPHIC),
                ("glyph on the open row", p.text_faint, p.muted, GRAPHIC),
                ("toolbar icon", p.icon, p.background, GRAPHIC),
            ];
            for (name, foreground, background, minimum) in pairs {
                let ratio = contrast(foreground, background);
                assert!(
                    ratio >= minimum,
                    "{theme:?}: {name} is {ratio:.2}:1, below {minimum}:1"
                );
            }
        }
    }

    /// What is lifted over the window (the palette, sheets, menus) stands
    /// apart from what is behind it and reads on its own surface.
    #[test]
    fn what_is_lifted_stands_apart_and_reads_in_both_themes() {
        for p in [Palette::light(), Palette::dark()] {
            let theme = p.appearance;
            let veiled_page = over(p.scrim, p.background);
            let veiled_bubble = over(p.scrim, p.bubble_in);
            let pairs = [
                // On a dark page a surface can only be so much lighter: there
                // the outline is what parts it from the veiled window.
                (
                    "the surface on the veiled page",
                    p.elevated,
                    veiled_page,
                    1.2,
                ),
                (
                    "the surface on a veiled bubble",
                    p.elevated,
                    veiled_bubble,
                    1.2,
                ),
                (
                    "the outline on the surface",
                    p.elevated_border,
                    p.elevated,
                    1.8,
                ),
                // A menu has no veil: its outline is what parts it from a
                // bubble or the page under it.
                (
                    "the outline on the page",
                    p.elevated_border,
                    p.background,
                    1.8,
                ),
                (
                    "the outline on a bubble",
                    p.elevated_border,
                    p.bubble_in,
                    1.8,
                ),
                (
                    "the picked row on the surface",
                    p.elevated_selected,
                    p.elevated,
                    1.2,
                ),
                (
                    "the accent bar on the picked row",
                    p.focus_ring,
                    p.elevated_selected,
                    3.0,
                ),
                ("text", p.text, p.elevated, 4.5),
                ("text on the picked row", p.text, p.elevated_selected, 4.5),
                ("muted text", p.text_muted, p.elevated, 4.5),
                (
                    "muted text on the picked row",
                    p.text_muted,
                    p.elevated_selected,
                    4.5,
                ),
                ("shortcut labels", p.text_muted, p.elevated, 4.5),
                ("icons", p.icon, p.elevated, 3.0),
                ("the accent rule", p.accent, p.elevated, 3.0),
            ];
            for (name, foreground, background, minimum) in pairs {
                let ratio = contrast(foreground, background);
                assert!(
                    ratio >= minimum,
                    "{theme:?}: {name} is {ratio:.2}:1, below {minimum}:1"
                );
            }
            // Against the veiled window, the surface or its outline stands
            // out plainly: the surface on light, the outline on dark.
            let edge =
                contrast(p.elevated, veiled_page).max(contrast(p.elevated_border, veiled_page));
            assert!(edge >= 2.0, "{theme:?}: the edge is {edge:.2}:1");
            // The veil is dark in both themes, and what is behind it reads
            // as background: text there loses at least a third of its
            // contrast.
            let behind = contrast(over(p.scrim, p.text), veiled_page);
            assert!(
                behind <= contrast(p.text, p.background) * 0.67,
                "{theme:?}: text behind the veil is still {behind:.1}:1"
            );
            let veil = Rgba::from(p.scrim);
            assert!(veil.r + veil.g + veil.b < 0.2 && veil.a >= 0.4, "{theme:?}");
        }
        // In the dark theme the surface itself differs from the page and
        // from a bubble; in the light one all three are papers, and it is
        // the veil, the outline and the rule that part them.
        let dark = Palette::dark();
        assert_ne!(dark.elevated, dark.background);
        assert_ne!(dark.elevated, dark.bubble_in);
    }

    const STYLES: [BubbleStyle; 3] = [BubbleStyle::Brand, BubbleStyle::Lime, BubbleStyle::Neutral];

    /// Every style of the account's own bubble, in both themes: text, time,
    /// ticks, links, the quote block and the play button.
    #[test]
    fn every_bubble_style_stays_legible_in_both_themes() {
        const TEXT: f32 = 4.5;
        const GRAPHIC: f32 = 3.0;
        for base in [Palette::light(), Palette::dark()] {
            for style in STYLES {
                let p = base.with_bubbles(style);
                let which = format!("{:?} {style:?}", p.appearance);
                let pairs = [
                    ("outgoing text", p.on_bubble_out, p.bubble_out, TEXT),
                    (
                        "outgoing text in a block",
                        p.on_bubble_out,
                        p.quote_out,
                        TEXT,
                    ),
                    ("outgoing time", p.meta_out, p.bubble_out, TEXT),
                    ("outgoing quote", p.meta_out, p.quote_out, TEXT),
                    ("outgoing link", p.signal_out, p.bubble_out, TEXT),
                    ("outgoing link in a block", p.signal_out, p.quote_out, TEXT),
                    ("read tick", p.tick_read, p.bubble_out, GRAPHIC),
                    ("unread tick", p.meta_out, p.bubble_out, GRAPHIC),
                    ("play glyph, outgoing", p.on_play_out, p.play_out, GRAPHIC),
                    (
                        "play button on its bubble",
                        p.play_out,
                        p.bubble_out,
                        GRAPHIC,
                    ),
                    ("failed outline", p.danger, p.background, GRAPHIC),
                    (
                        "focus ring on the page",
                        p.focus_ring,
                        p.background,
                        GRAPHIC,
                    ),
                    // Text that is selected is still text.
                    (
                        "selected text, outgoing",
                        p.on_bubble_out,
                        over(p.selection_out, p.bubble_out),
                        TEXT,
                    ),
                    (
                        "selected link, outgoing",
                        p.signal_out,
                        over(p.selection_out, p.bubble_out),
                        GRAPHIC,
                    ),
                    (
                        "selected text, incoming",
                        p.text,
                        over(p.selection_in, p.bubble_in),
                        TEXT,
                    ),
                    (
                        "selected link, incoming",
                        p.accent,
                        over(p.selection_in, p.bubble_in),
                        GRAPHIC,
                    ),
                    // And the selection itself can be seen.
                    (
                        "a selection, outgoing",
                        over(p.selection_out, p.bubble_out),
                        p.bubble_out,
                        1.15,
                    ),
                    (
                        "a selection, incoming",
                        over(p.selection_in, p.bubble_in),
                        p.bubble_in,
                        1.15,
                    ),
                    // The button on a picture that is not here yet stands
                    // on the block's fill.
                    (
                        "media button glyph",
                        p.on_media_button,
                        over(p.media_button, p.quote_out),
                        GRAPHIC,
                    ),
                    (
                        "media button glyph, incoming",
                        p.on_media_button,
                        over(p.media_button, p.quote_in),
                        GRAPHIC,
                    ),
                ];
                for (name, foreground, background, minimum) in pairs {
                    let ratio = contrast(foreground, background);
                    assert!(
                        ratio >= minimum,
                        "{which}: {name} is {ratio:.2}:1, below {minimum}:1"
                    );
                }
                // The incoming bubble is the same in every style.
                assert_eq!(p.bubble_in, base.bubble_in, "{which}");
            }
        }
        // Brand is what a palette comes with.
        let dark = Palette::dark();
        assert_eq!(
            dark.bubble_out,
            dark.with_bubbles(BubbleStyle::Brand).bubble_out
        );
        assert_ne!(
            dark.bubble_out,
            dark.with_bubbles(BubbleStyle::Neutral).bubble_out
        );
        assert_ne!(
            dark.bubble_out,
            dark.with_bubbles(BubbleStyle::Lime).bubble_out
        );
    }

    /// A translucent colour over an opaque one, as it is seen.
    fn over(top: Hsla, base: Hsla) -> Hsla {
        let (top, base) = (Rgba::from(top), Rgba::from(base));
        let channel = |t: f32, b: f32| t * top.a + b * (1. - top.a);
        Rgba {
            r: channel(top.r, base.r),
            g: channel(top.g, base.g),
            b: channel(top.b, base.b),
            a: 1.,
        }
        .into()
    }

    /// Every colour a person can have reads as text on every ground a name
    /// is written on: the incoming bubble and its quote block, and the
    /// outgoing bubble and its quote block in each style. Their initials
    /// read on their colour.
    #[test]
    fn every_sender_colour_reads_on_every_bubble() {
        for base in [Palette::light(), Palette::dark()] {
            for style in STYLES {
                let p = base.with_bubbles(style);
                let which = format!("{:?} {style:?}", p.appearance);
                for tone in 0..SENDER_TONES {
                    let grounds = [
                        ("the incoming bubble", p.sender[tone], p.bubble_in),
                        ("an incoming quote", p.sender[tone], p.quote_in),
                        ("the outgoing bubble", p.sender_out[tone], p.bubble_out),
                        ("an outgoing quote", p.sender_out[tone], p.quote_out),
                        ("their avatar", p.on_sender_fill, p.sender_fill[tone]),
                    ];
                    for (ground, foreground, background) in grounds {
                        let ratio = contrast(foreground, background);
                        assert!(
                            ratio >= 4.5,
                            "{which}: person {tone} is {ratio:.2}:1 on {ground}"
                        );
                    }
                }
            }
        }
    }

    /// People are told apart: no two of the colours are the same, and the
    /// brand's own is one of them (lime on dark, olive on light).
    #[test]
    fn the_sender_colours_are_distinct_and_include_the_brands() {
        for p in [Palette::light(), Palette::dark()] {
            for (a, first) in p.sender.iter().enumerate() {
                for second in &p.sender[a + 1..] {
                    assert_ne!(first, second, "{:?}", p.appearance);
                }
            }
            assert_eq!(p.sender[0], p.accent, "{:?}", p.appearance);
        }
        // Nothing near WhatsApp's green (hue 142): every hue is outside
        // the greens between lime and teal.
        for value in raw::SENDER_TINTS
            .into_iter()
            .chain(raw::SENDER_DEEP)
            .chain(raw::SENDER_DEEPER)
        {
            let hue = hex(value).h * 360.;
            assert!(
                !(96. ..=178.).contains(&hue),
                "{value:06x} is a green (hue {hue:.0})"
            );
        }
    }

    /// The wallpaper is there and never in the way: its lines are barely
    /// off the page, far below anything that carries text.
    #[test]
    fn the_wallpaper_is_fainter_than_a_hairline() {
        for p in [Palette::light(), Palette::dark()] {
            let rule = contrast(p.border, p.background);
            for (name, colour) in [("lines", p.wallpaper), ("marks", p.wallpaper_accent)] {
                let ratio = contrast(colour, p.background);
                assert!(
                    (1.05..=1.3).contains(&ratio),
                    "{:?}: the wallpaper's {name} are {ratio:.2}:1",
                    p.appearance
                );
                assert!(
                    ratio <= rule + 0.02,
                    "{:?}: {name} louder than a rule",
                    p.appearance
                );
            }
        }
    }

    #[test]
    fn the_logo_is_the_lime_mark_in_both_themes() {
        for p in [Palette::light(), Palette::dark()] {
            let svg = brand::load(p.logo).expect("the logo is bundled");
            let svg = std::str::from_utf8(svg).expect("an SVG is text");
            assert!(svg.contains("#D4FF3F"), "{:?}: no lime", p.appearance);
        }
    }
}
