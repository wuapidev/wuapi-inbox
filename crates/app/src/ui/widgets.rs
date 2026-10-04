//! Small shared pieces: avatars, icon buttons, ticks, the logo and the
//! marks of the hairline grid.

use crate::format::initials;
use crate::icons::{icon, IconName};
use crate::motion;
use crate::product::MAKER;
use crate::settings::{self, ThemeChoice};
use crate::theme::px;
use crate::theme::{fonts, metrics, Palette};
use brand_mark::{AnimatedMark, Timeline};
use client_provider::{ConnectionState, DeliveryStatus};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, img, transparent_black, AnyElement, App, Div, FontWeight, Hsla, Image, ObjectFit, Pixels,
    SharedString, Stateful, StyledImage, Window,
};
use std::sync::Arc;
use std::time::Duration;

/// What an avatar stands for when there is no picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvatarKind {
    /// A person or a linked number: initials.
    Person,
    /// A group: the people glyph.
    Group,
}

/// A round avatar: initials in the mono face on the quiet fill, or the
/// group glyph.
///
/// Profile pictures are not downloaded yet; when they are, this is the
/// fallback for contacts without one.
pub fn avatar(name: &str, kind: AvatarKind, size: Pixels, palette: &Palette) -> Div {
    let face = div()
        .flex_none()
        .size(size)
        .rounded_full()
        .bg(palette.muted)
        .border_1()
        .border_color(palette.border)
        .flex()
        .items_center()
        .justify_center();
    match kind {
        AvatarKind::Group => face.child(icon(IconName::Users, size * 0.42, palette.on_avatar)),
        AvatarKind::Person => face
            .font_family(fonts::MONO)
            .text_color(palette.on_avatar)
            .text_size(size * 0.31)
            .font_weight(FontWeight::MEDIUM)
            .child(SharedString::from(initials(name))),
    }
}

/// An avatar with the chat's picture when there is one: the same circle,
/// the same size, so nothing moves when the picture arrives. Without one
/// it is [`avatar`]: initials, or the group glyph.
pub fn avatar_or(
    picture: Option<Arc<Image>>,
    name: &str,
    kind: AvatarKind,
    size: Pixels,
    palette: &Palette,
) -> Div {
    match picture {
        Some(picture) => div()
            .flex_none()
            .size(size)
            .rounded_full()
            .overflow_hidden()
            .border_1()
            .border_color(palette.border)
            .bg(palette.muted)
            .debug_selector(|| "avatar-image".into())
            .child(
                img(picture)
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            ),
        None => avatar(name, kind, size, palette),
    }
}

/// A square, borderless toolbar button showing one icon. Reachable with
/// Tab, pressed with Enter or Space.
pub fn icon_button(id: &'static str, name: IconName, palette: &Palette) -> Stateful<Div> {
    let (hover, ring) = (palette.muted, palette.accent);
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .size(metrics::CONTROL())
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(transparent_black())
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .hover(move |style| style.bg(hover))
        .focus_visible(move |style| style.border_color(ring))
        .child(icon(name, px(18.), palette.icon))
}

/// A toolbar button for something that does not exist yet: drawn faint,
/// not clickable, and saying so when pointed at.
pub fn unavailable_button(
    id: &'static str,
    name: IconName,
    hint: &'static str,
    palette: &Palette,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .size(metrics::CONTROL())
        .rounded(metrics::RADIUS())
        .flex()
        .items_center()
        .justify_center()
        .opacity(0.45)
        .tooltip(move |window, cx| Tooltip::new(hint).build(window, cx))
        .child(icon(name, px(18.), palette.icon))
}

/// A button with a label. `primary` is the screen's one accent control,
/// the lime fill; the others are a hairline on the raised surface.
pub fn text_button(
    id: &'static str,
    text: impl Into<SharedString>,
    glyph: Option<IconName>,
    primary: bool,
    palette: &Palette,
) -> Stateful<Div> {
    let (fill, ink, glyph_ink, edge) = if primary {
        (
            palette.accent_fill,
            palette.on_accent_fill,
            palette.on_accent_fill,
            palette.accent_fill,
        )
    } else {
        (palette.surface, palette.text, palette.icon, palette.border)
    };
    let ring = palette.text;
    div()
        .id(id)
        .debug_selector(move || id.into())
        .h(metrics::BUTTON())
        .px_4()
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(edge)
        .bg(fill)
        .text_color(ink)
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .cursor_pointer()
        .text_size(metrics::TEXT_BODY())
        .font_weight(FontWeight::MEDIUM)
        .tab_index(0)
        .hover(|style| style.opacity(0.85))
        .focus_visible(move |style| style.border_color(ring))
        .when_some(glyph, |this, name| {
            this.child(icon(name, px(15.), glyph_ink))
        })
        .child(text.into())
}

/// One choice out of a few, side by side: the selected one on the quiet
/// fill. `selector` prefixes each segment's test name (`theme-dark`).
pub fn segmented<T: Copy + PartialEq + 'static>(
    selector: &'static str,
    options: &[(T, &'static str)],
    selected: T,
    palette: &Palette,
    on_pick: impl Fn(T, &mut Window, &mut App) + Clone + 'static,
) -> Div {
    let mut row = div()
        .flex_none()
        .p(px(2.))
        .rounded(metrics::RADIUS())
        .border_1()
        .border_color(palette.border)
        .bg(palette.surface)
        .flex()
        .gap(px(2.));
    for (index, (value, name)) in options.iter().copied().enumerate() {
        let active = value == selected;
        let (hover, ring) = (palette.hover, palette.accent);
        let pick = on_pick.clone();
        let test_name = format!("{selector}-{}", name.to_lowercase());
        row = row.child(
            div()
                .id((selector, index))
                .debug_selector(move || test_name.clone())
                .h(px(26.))
                .px_3()
                .rounded(px(4.))
                .border_1()
                .border_color(transparent_black())
                .flex()
                .items_center()
                .cursor_pointer()
                .text_size(metrics::TEXT_SMALL())
                .tab_index(0)
                .focus_visible(move |style| style.border_color(ring))
                .map(|this| {
                    if active {
                        this.bg(palette.muted)
                            .text_color(palette.text)
                            .font_weight(FontWeight::MEDIUM)
                    } else {
                        this.text_color(palette.text_muted)
                            .hover(move |style| style.bg(hover))
                    }
                })
                .on_click(move |_, window, cx| pick(value, window, cx))
                .child(name),
        );
    }
    row
}

/// An on/off switch: the lime fill when on.
pub fn switch(id: &'static str, on: bool, palette: &Palette) -> Stateful<Div> {
    let ring = palette.text;
    let (track, knob) = if on {
        (palette.accent_fill, palette.on_accent_fill)
    } else {
        (palette.muted, palette.text_faint)
    };
    div()
        .id(id)
        .debug_selector(move || id.into())
        .flex_none()
        .w(px(36.))
        .h(px(20.))
        .p(px(2.))
        .rounded_full()
        .border_1()
        .border_color(palette.border)
        .bg(track)
        .flex()
        .items_center()
        .cursor_pointer()
        .tab_index(0)
        .focus_visible(move |style| style.border_color(ring))
        .when(on, |this| this.justify_end())
        .child(div().size(px(14.)).rounded_full().bg(knob))
}

/// The brand's status light.
pub fn led(colour: Hsla) -> Div {
    div().flex_none().size(px(8.)).rounded_full().bg(colour)
}

/// A mono tag in brackets, `[ LIVE ]`, optionally with the status light.
pub fn tag(text: &str, light: Option<Hsla>, palette: &Palette) -> Div {
    div()
        .flex_none()
        .flex()
        .items_center()
        .gap_2()
        .children(light.map(led))
        .child(mono(format!("[ {} ]", text.to_uppercase())).text_color(palette.text_muted))
}

/// An element at `progress` of its entrance (see `motion.rs`): faded and a
/// little below its place until it is in.
pub fn reveal(element: Div, progress: f32) -> Div {
    if progress >= 1. {
        return element;
    }
    element
        .relative()
        .top(px((1. - progress) * motion::TRAVEL))
        .opacity(progress.clamp(0., 1.))
}

/// The rail of the screens that come before the chats (welcome, sign-in):
/// the logo cell on top and the theme toggle at the bottom, on the same
/// hairline grid as the main window's.
pub fn screen_rail(palette: &Palette) -> Div {
    let dark = palette.is_dark();
    div()
        .flex_none()
        .w(metrics::RAIL_WIDTH())
        .h_full()
        .border_r_1()
        .border_color(palette.border)
        .flex()
        .flex_col()
        .items_center()
        .child(
            div()
                .flex_none()
                .w_full()
                .h(metrics::HEADER_HEIGHT())
                .border_b_1()
                .border_color(palette.border)
                .flex()
                .items_center()
                .justify_center()
                .child(logo(px(24.), palette)),
        )
        .child(div().flex_1())
        .child(
            div().flex_none().pb_3().child(
                icon_button(
                    "toggle-theme",
                    if dark { IconName::Sun } else { IconName::Moon },
                    palette,
                )
                .on_click(move |_, _, cx| {
                    let next = if dark {
                        ThemeChoice::Light
                    } else {
                        ThemeChoice::Dark
                    };
                    settings::update(cx, |settings| settings.theme = next);
                }),
            ),
        )
}

/// The strip that carries the header rule across those screens: a title
/// on the left, the maker's tag on the right.
pub fn screen_header(title: &'static str, progress: f32, palette: &Palette) -> Div {
    div()
        .flex_none()
        .h(metrics::HEADER_HEIGHT())
        .px_4()
        .border_b_1()
        .border_color(palette.border)
        .flex()
        .items_center()
        .justify_between()
        .child(
            div()
                .text_size(metrics::TEXT_TITLE())
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(reveal(tag(MAKER, None, palette), progress))
}

/// How the mark on a screen behaves right now.
#[derive(Clone, Copy, Debug)]
pub struct MarkPlay {
    /// Something else has the user's eye (the window is in the background,
    /// a panel is open): the static mark, no timers, no repaints.
    pub paused: bool,
    /// Reduced motion: the static mark.
    pub reduced: bool,
    /// Pop in from the tail before coming alive: the first time the screen
    /// shows, not every time it comes back.
    pub intro: bool,
    /// `--motion-at`: held at this moment of its timeline.
    pub frozen: Option<Duration>,
}

/// The slot for the wuapi mark on the start and sign-in screens: the
/// animated mark of `crates/brand-mark`, which blinks and looks around as
/// it does on the wuapi site and falls back to the static mark whenever it
/// may not move.
///
/// THE ONE PLACE THE MARK IS CHOSEN: every screen goes through here.
/// `mark` is the height of the mark itself; on light surfaces it sits on
/// its ink tile, of which it takes 60 %.
pub fn brand_mark(id: &'static str, mark: Pixels, play: MarkPlay, palette: &Palette) -> AnyElement {
    let on_tile = !palette.is_dark();
    let size = if on_tile { mark / 0.6 } else { mark };
    let timeline = Timeline::site().intro(play.intro);
    let element = AnimatedMark::new(size)
        .id(id)
        .tile(on_tile)
        // Always lime, with the ink that goes on lime, from the tokens.
        .lime(palette.accent_fill)
        .ink(palette.on_accent_fill)
        .tile_color(palette.on_accent_fill)
        .timeline(timeline)
        .animate(!play.paused)
        .reduced_motion(play.reduced);
    match play.frozen {
        Some(at) => element
            .pose(timeline.intro(true).pose(at))
            .into_any_element(),
        None => element.into_any_element(),
    }
}

/// How much of the tick's colour a message shows that the provider has
/// and WhatsApp does not yet.
const ACCEPTED_TICK_ALPHA: f32 = 0.45;

/// The delivery tick of an outgoing message: clock (still on this
/// computer), a faint tick (the provider has it), one tick (WhatsApp has
/// it), two ticks, two ticks in `read`, or an alert.
pub fn status_tick(
    status: &DeliveryStatus,
    pending: Hsla,
    read: Hsla,
    palette: &Palette,
) -> AnyElement {
    match status {
        DeliveryStatus::Pending => icon(IconName::Clock3, px(12.), pending).into_any_element(),
        DeliveryStatus::Accepted => icon(
            IconName::Check,
            px(15.),
            Hsla {
                a: pending.a * ACCEPTED_TICK_ALPHA,
                ..pending
            },
        )
        .into_any_element(),
        DeliveryStatus::Sent => icon(IconName::Check, px(15.), pending).into_any_element(),
        DeliveryStatus::Delivered => {
            icon(IconName::CheckCheck, px(15.), pending).into_any_element()
        }
        DeliveryStatus::Read => icon(IconName::CheckCheck, px(15.), read).into_any_element(),
        DeliveryStatus::Failed { .. } => {
            icon(IconName::CircleAlert, px(14.), palette.danger).into_any_element()
        }
    }
}

/// Technical text: times, counts, sizes. Mono face at the meta size.
pub fn mono(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .font_family(fonts::MONO)
        .text_size(metrics::TEXT_META())
        .child(text.into())
}

/// A section label: mono, uppercase, muted.
pub fn label(text: &str, palette: &Palette) -> Div {
    mono(text.to_uppercase()).text_color(palette.text_muted)
}

/// The static logo, with the mark `mark` pixels tall: the lime mark, on
/// its ink tile where the page is light. The tile is the brand's
/// application icon, in which the mark takes 60% of the side.
pub fn logo(mark: Pixels, palette: &Palette) -> impl IntoElement {
    let size = if palette.is_dark() { mark } else { mark / 0.6 };
    img(palette.logo).flex_none().size(size)
}

/// The colour of a number's status light: the accent while it is
/// connected, amber while it is on its way, grey when it is off, red when
/// it has to be linked again.
pub fn status_colour(connection: &ConnectionState, palette: &Palette) -> Hsla {
    match connection {
        ConnectionState::Connected => palette.accent,
        ConnectionState::Connecting | ConnectionState::Reconnecting => palette.warning,
        ConnectionState::Disconnected { .. } => palette.text_faint,
        ConnectionState::LoggedOut => palette.danger,
    }
}

/// A crosshair centred on the point where the rule `x` pixels from the
/// left meets the rule `y` pixels from the top. `x` and `y` are the far
/// edges of the two one-pixel rules.
pub fn grid_mark(x: Pixels, y: Pixels, palette: &Palette) -> Div {
    grid_mark_in(x, y, 1., palette)
}

/// A crosshair at `progress` of its entrance: it grows into place from a
/// third of its size (the site's `wu-mark-in`, without the quarter turn,
/// which a symmetric cross does not show at either end).
pub fn grid_mark_in(x: Pixels, y: Pixels, progress: f32, palette: &Palette) -> Div {
    let progress = progress.clamp(0., 1.);
    let length = metrics::GRID_MARK() * (0.3 + 0.7 * progress);
    // Whole pixels, and odd, so the cross stays centred on the rule.
    let length = px((length.as_f32() / 2.).floor() * 2. + 1.);
    let arm = (length - crate::theme::hairline()) / 2.;
    let colour = palette.grid_mark.opacity(progress);
    mark_at(x, y, length, arm, colour)
}

/// A crosshair on each corner of the box it is a child of (which must be
/// `relative`), at `progress` of its entrance. The box itself has no
/// border: the bordered box is its other child, and the crosshairs are
/// drawn after it, on top of its rules.
pub fn corner_marks(progress: f32, palette: &Palette) -> Vec<Div> {
    let progress = progress.clamp(0., 1.);
    let length = metrics::GRID_MARK() * (0.3 + 0.7 * progress);
    let length = px((length.as_f32() / 2.).floor() * 2. + 1.);
    let arm = (length - crate::theme::hairline()) / 2.;
    let colour = palette.grid_mark.opacity(progress);
    // Centred on the corner pixel of the box.
    let out = -arm;
    [(true, true), (true, false), (false, true), (false, false)]
        .into_iter()
        .map(|(top, left)| {
            div()
                .absolute()
                .size(length)
                .map(|this| if top { this.top(out) } else { this.bottom(out) })
                .map(|this| {
                    if left {
                        this.left(out)
                    } else {
                        this.right(out)
                    }
                })
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .top(arm)
                        .w(length)
                        .h(crate::theme::hairline())
                        .bg(colour),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(arm)
                        .w(crate::theme::hairline())
                        .h(length)
                        .bg(colour),
                )
        })
        .collect()
}

fn mark_at(x: Pixels, y: Pixels, length: Pixels, arm: Pixels, colour: Hsla) -> Div {
    let (left, top) = (x - crate::theme::hairline(), y - crate::theme::hairline());
    div()
        .absolute()
        .top_0()
        .left_0()
        .child(
            div()
                .absolute()
                .left(left - arm)
                .top(top)
                .w(length)
                .h(crate::theme::hairline())
                .bg(colour),
        )
        .child(
            div()
                .absolute()
                .left(left)
                .top(top - arm)
                .w(crate::theme::hairline())
                .h(length)
                .bg(colour),
        )
}
