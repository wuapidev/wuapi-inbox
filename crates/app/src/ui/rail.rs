//! The account rail: the logo, the linked numbers (alone or in groups),
//! and the buttons that are always in reach.
//!
//! What is where comes from `crate::rail` (the arrangement and what a drag
//! does); this file draws it and feeds it the pointer.

use super::connection::connection_words;
use super::rail_menu::RailTarget;
use super::shell::{Overlay, Shell};
use super::widgets::{icon_button, logo, mono, status_colour};
use crate::format::initials;
use crate::icons::{icon, IconName};
use crate::motion::{self, Tween};
use crate::rail::{self, Dragged, Drop, Group, Key, RailItem, Released, Row, RowKind};
use crate::settings;
use crate::theme::px;
use crate::theme::{fonts, metrics, Palette};
use client_provider::{Account, AccountId, ChatId, ConnectionState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, div, img, AnyElement, BoxShadow, Context, Div, FontWeight, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, ObjectFit, Pixels, Point, SharedString, StyledImage, Task, Window,
};
use std::collections::HashMap;
use std::time::Instant;

/// The sizes the rail is built from. Every item, a number or a closed
/// group, is one `cell`; that is what keeps them on one centre line with
/// one rhythm, and their badges and lights in one column.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct RailGeometry {
    /// The side of a number's face.
    pub(super) face: Pixels,
    /// The side of an item: the face, the room around it and its ring.
    pub(super) cell: Pixels,
    /// The room between two items, anywhere in the rail.
    pub(super) gap: Pixels,
    /// The room inside an open group's container, on every side.
    pub(super) pad: Pixels,
    /// The height of an open group's head.
    pub(super) head: Pixels,
    /// The room inside a closed group's tile, on every side.
    pub(super) tile_pad: Pixels,
    /// The room between the small faces of a closed group.
    pub(super) tile_gap: Pixels,
}

/// The width of an item's ring, in device-independent pixels at every
/// interface size.
const RING: f32 = 2.;

/// The rim of the rail's background a count and a status light are cut
/// out of the ring and the face with.
#[allow(non_snake_case)]
pub(super) fn KNOCKOUT() -> Pixels {
    px(2.3)
}

impl RailGeometry {
    /// The rail's sizes at the interface size in use.
    pub(super) fn now() -> Self {
        let face = metrics::AVATAR_MEDIUM();
        Self {
            face,
            cell: face + px(2.) * 2. + gpui_kit::px(RING * 2.),
            gap: px(8.),
            pad: px(4.),
            head: px(18.),
            tile_pad: px(3.),
            tile_gap: px(2.),
        }
    }

    /// The side of the tile inside a closed group's ring, within its
    /// hairline: where the small faces are laid out.
    pub(super) fn tile_inner(&self) -> Pixels {
        self.cell - gpui_kit::px(RING * 2.) - gpui_kit::px(2.)
    }

    /// The side of a small face: two of them, the room between and the
    /// room around fill the tile exactly.
    pub(super) fn mini(&self) -> Pixels {
        (self.tile_inner() - self.tile_pad * 2. - self.tile_gap) / 2.
    }

    /// The corner radius of a tile and of an open group's container:
    /// larger than a field's, so it reads as something that holds things.
    fn tile_radius(&self) -> Pixels {
        metrics::RADIUS() * 2.
    }

    /// The width of an open group's container.
    fn open_width(&self) -> Pixels {
        self.cell + self.pad * 2. + gpui_kit::px(2.)
    }

    /// The height of an open group's container with `members` in it.
    fn open_height(&self, members: usize) -> Pixels {
        self.pad * 2. + gpui_kit::px(2.) + self.head + (self.gap + self.cell) * members as f32
    }

    /// Where an item's left edge is: centred in the rail.
    fn cell_left(&self) -> Pixels {
        (metrics::RAIL_WIDTH() - self.cell) / 2.
    }
}

/// A dragged item on its way to rest: into its new place, or back home.
#[derive(Clone, Debug)]
struct Landing {
    what: Dragged,
    /// Where the lifted copy was when it was let go.
    from: Point<Pixels>,
    tween: Tween,
}

/// What is in motion on the rail.
#[derive(Default)]
pub(super) struct RailFx {
    /// Since when the held item has been lifted.
    lifted: Option<Tween>,
    /// Where in the item it was grabbed: the pointer, less the item's
    /// corner.
    grab: Point<Pixels>,
    /// The room opening where the item would land.
    gap: Option<(Drop, Tween)>,
    /// A member has been dragged out of its group: its slot closes.
    leaving: Option<Tween>,
    /// Groups opening (`true`) or closing.
    folds: HashMap<u32, (Tween, bool)>,
    landing: Option<Landing>,
    ticker: Option<Task<()>>,
    /// The rows of the frame being drawn, as they are measured.
    drawing: std::rc::Rc<std::cell::RefCell<Vec<Row>>>,
}

/// The subject under which a number's chosen icon is kept in the store's
/// pictures. No provider is ever asked about it.
pub(super) const ICON_SUBJECT: &str = "\u{1}rail-icon";

/// What the corner of a rail item says about the connection.
#[derive(Clone, Copy)]
enum RailLight {
    /// Connected, or on its way: a light of the state's colour.
    Dot(Hsla),
    /// Never linked: nothing to light yet.
    Hollow,
    /// Not connected or logged out: it will not come back by itself.
    Alert,
}

impl RailLight {
    fn of(account: &Account, palette: &Palette) -> Self {
        match &account.connection {
            _ if account.never_linked() => Self::Hollow,
            ConnectionState::Disconnected { .. } | ConnectionState::LoggedOut => Self::Alert,
            connection => Self::Dot(status_colour(connection, palette)),
        }
    }
}

/// How bad a connection state is, for a group's light: the worst shows.
fn severity(account: &Account) -> u8 {
    match account.connection {
        ConnectionState::Connected => 0,
        _ if account.never_linked() => 1,
        ConnectionState::Connecting | ConnectionState::Reconnecting => 2,
        ConnectionState::Disconnected { .. } => 3,
        ConnectionState::LoggedOut => 4,
    }
}

impl Shell {
    /// The rail's key of an account of this session's provider.
    pub(super) fn rail_key(&self, account: &AccountId) -> Key {
        rail::key(self.engine.provider_id(), account.as_str())
    }

    /// The numbers there is something to show for: the ones a phone was
    /// linked to. One that was created and never linked is not in the rail
    /// and is not switched to; it waits under Settings, to link or remove.
    pub(super) fn linked_accounts(&self) -> impl Iterator<Item = &Account> {
        self.accounts
            .iter()
            .filter(|account| !account.never_linked())
    }

    /// The account a key of the rail stands for, when it is one of this
    /// session's.
    pub(super) fn rail_account(&self, key: &Key) -> Option<&Account> {
        self.linked_accounts()
            .find(|account| self.rail_key(&account.id) == *key)
    }

    /// The name a number goes by here: the user's own label for it, else
    /// the provider's.
    pub(super) fn account_name(&self, account: &Account) -> String {
        self.rail
            .look(&self.rail_key(&account.id))
            .label
            .filter(|label| !label.trim().is_empty())
            .unwrap_or_else(|| account.display_name.clone())
    }

    /// The name of the number whose chats are showing.
    pub(super) fn current_account_name(&self) -> Option<String> {
        let current = self.account.as_ref()?;
        self.accounts
            .iter()
            .find(|account| &account.id == current)
            .map(|account| self.account_name(account))
    }

    /// Writes the rail's arrangement to its file.
    pub(super) fn save_rail(&self) {
        if let Some(path) = &self.rail_path {
            if let Err(error) = self.rail.save(path) {
                tracing::warn!(%error, "the rail's arrangement could not be saved");
            }
        }
    }

    /// Changes the rail's arrangement and keeps the change.
    pub(super) fn change_rail(
        &mut self,
        change: impl FnOnce(&mut rail::RailLayout),
        cx: &mut Context<Self>,
    ) {
        let before = self.rail.clone();
        change(&mut self.rail);
        if self.rail != before {
            self.save_rail();
        }
        cx.notify();
    }

    /// Unread messages of a number that count: none while it is muted.
    fn rail_unread(&self, account: &Account) -> (u32, bool) {
        let unread = self.unread.get(&account.id).copied().unwrap_or(0);
        (unread, self.rail.look(&self.rail_key(&account.id)).muted)
    }

    // ----- motion --------------------------------------------------------

    /// Whether the rail moves at all: not under reduced motion.
    fn rail_still(&self, cx: &gpui_kit::App) -> bool {
        settings::reduce_motion(cx)
    }

    /// Opens or closes a group, with its animation.
    pub(super) fn toggle_rail_group(&mut self, id: u32, cx: &mut Context<Self>) {
        let Some(group) = self.rail.group(id) else {
            return;
        };
        let opening = !group.expanded;
        let now = cx.background_executor().now();
        self.rail_fx
            .folds
            .insert(id, (Tween::begin(now, motion::rail::FOLD), opening));
        self.change_rail(|rail| rail.toggle(id), cx);
        self.run_rail_ticker(cx);
    }

    /// How open a group is drawn: 0 closed, 1 open, in between while it
    /// folds.
    fn fold_progress(&self, group: &Group, now: Instant, still: bool) -> f32 {
        let target = if group.expanded { 1. } else { 0. };
        match self.rail_fx.folds.get(&group.id) {
            Some((tween, opening)) if !tween.done(now, still) => {
                let progress = tween.at(now, still);
                if *opening {
                    progress
                } else {
                    1. - progress
                }
            }
            _ => target,
        }
    }

    /// Whether anything on the rail is still moving, or held.
    fn rail_in_motion(&self, now: Instant, still: bool) -> bool {
        let fx = &self.rail_fx;
        self.rail_drag.dragging().is_some()
            || fx
                .landing
                .as_ref()
                .is_some_and(|l| !l.tween.done(now, still))
            || fx.folds.values().any(|(tween, _)| !tween.done(now, still))
            || fx
                .gap
                .as_ref()
                .is_some_and(|(_, tween)| !tween.done(now, still))
    }

    /// Repaints the rail each frame while something on it moves or is
    /// held, and does what only time does: scrolls the rail while a drag
    /// rests near an edge, and opens a closed group a drag rests on.
    fn run_rail_ticker(&mut self, cx: &mut Context<Self>) {
        if self.rail_fx.ticker.is_some() {
            return;
        }
        self.rail_fx.ticker = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            loop {
                clock.timer(motion::FRAME).await;
                let running = this.update(cx, |this, cx| {
                    let now = clock.now();
                    let still = this.rail_still(cx);
                    this.rail_tick(now, cx);
                    cx.notify();
                    let running = this.rail_in_motion(now, still);
                    if !running {
                        // What has arrived is forgotten.
                        this.rail_fx.landing = None;
                        this.rail_fx.folds.clear();
                        this.rail_fx.ticker = None;
                    }
                    running
                });
                if !matches!(running, Ok(true)) {
                    return;
                }
            }
        }));
    }

    /// One frame of a drag: the pointer may not have moved, but time has.
    fn rail_tick(&mut self, now: Instant, cx: &mut Context<Self>) {
        let Some(dragged) = self.rail_drag.dragging().cloned() else {
            return;
        };
        let (_, y) = self.rail_drag.pointer();
        // Near an edge, with more numbers than fit: the rail scrolls for
        // as long as the pointer stays there.
        let bounds = self.rail_scroll.bounds();
        let step = rail::autoscroll(y, bounds.top().as_f32(), bounds.bottom().as_f32());
        if step != 0. {
            let mut offset = self.rail_scroll.offset();
            let lowest = -self.rail_scroll.max_offset().y;
            offset.y = (offset.y - gpui_kit::px(step)).clamp(lowest, gpui_kit::px(0.));
            self.rail_scroll.set_offset(offset);
        }
        self.rail_hover(&dragged, y, now, cx);
        // Resting on a closed group opens it, so the item can go inside.
        let closed: Vec<u32> = self
            .rail
            .groups()
            .iter()
            .filter(|group| !group.expanded)
            .map(|group| group.id)
            .collect();
        if let Some(id) = self
            .rail_drag
            .dwelt(now, motion::rail::DWELL, |id| closed.contains(&id))
        {
            self.rail_fx
                .folds
                .insert(id, (Tween::begin(now, motion::rail::FOLD), true));
            self.change_rail(|rail| rail.toggle(id), cx);
        }
    }

    /// Works out what the drag is over, and what that sets in motion.
    fn rail_hover(&mut self, dragged: &Dragged, y: f32, now: Instant, cx: &mut Context<Self>) {
        let rows = self.rail_rows.borrow().clone();
        let over = rail::hit(&rows, y, dragged, self.rail.items.len());
        self.rail_drag.hover(over.clone(), now);
        // Room opens where it would land, unless that is where it is.
        let slot = over
            .clone()
            .filter(|drop| matches!(drop, Drop::Top(_) | Drop::InGroup(..)))
            .filter(|drop| !rail::stays_put(&self.rail, dragged, drop));
        if self.rail_fx.gap.as_ref().map(|(drop, _)| drop) != slot.as_ref() {
            self.rail_fx.gap = slot.map(|drop| (drop, Tween::begin(now, motion::rail::GAP)));
        }
        // A member over anything outside its group has left it: the slot
        // it held closes, and the container shrinks with it.
        let home = match dragged {
            Dragged::Account(key) => self.rail.group_of(key),
            Dragged::Group(_) => None,
        };
        let outside = match (&over, home) {
            (Some(Drop::Top(_)), Some(_)) => true,
            (Some(Drop::InGroup(id, _)), Some(home)) => *id != home,
            (Some(Drop::OntoGroup(id)), Some(home)) => *id != home,
            (Some(Drop::OntoAccount(key)), Some(home)) => self.rail.group_of(key) != Some(home),
            _ => false,
        };
        match (outside, self.rail_fx.leaving.is_some()) {
            (true, false) => self.rail_fx.leaving = Some(Tween::begin(now, motion::rail::GAP)),
            (false, true) => self.rail_fx.leaving = None,
            _ => {}
        }
        cx.notify();
    }

    // ----- the pointer ---------------------------------------------------

    /// The button went down on a number or a group: it may become a drag.
    fn rail_press(&mut self, what: Dragged, at: Point<Pixels>) {
        // Where in the item it was grabbed, so the lifted copy rises
        // from under the pointer instead of jumping to it.
        let geometry = RailGeometry::now();
        let top = self
            .rail_row_of(&what)
            .map_or(at.y - geometry.cell / 2., |row| gpui_kit::px(row.top));
        self.rail_fx.grab = Point {
            x: at.x - geometry.cell_left(),
            y: at.y - top,
        };
        self.rail_fx.landing = None;
        self.rail_drag.press(what, (at.x.as_f32(), at.y.as_f32()));
    }

    /// The pointer moved: a held number or group may have become a drag,
    /// and a drag follows it.
    pub(super) fn rail_pointer(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let was_dragging = self.rail_drag.dragging().is_some();
        if !self.rail_drag.moved((at.x.as_f32(), at.y.as_f32())) {
            return;
        }
        let Some(dragged) = self.rail_drag.dragging().cloned() else {
            return;
        };
        let now = cx.background_executor().now();
        if !was_dragging {
            // It lifts.
            self.rail_fx.lifted = Some(Tween::begin(now, motion::rail::LIFT));
            self.rail_fx.gap = None;
            self.rail_fx.leaving = None;
            self.run_rail_ticker(cx);
        }
        self.rail_hover(&dragged, at.y.as_f32(), now, cx);
    }

    /// Where the lifted copy's corner is, for a pointer at `(x, y)`.
    fn floating_origin(&self) -> Point<Pixels> {
        let (x, y) = self.rail_drag.pointer();
        Point {
            x: gpui_kit::px(x) - self.rail_fx.grab.x,
            y: gpui_kit::px(y) - self.rail_fx.grab.y,
        }
    }

    /// The drag is over: nothing is lifted, nothing is held open.
    fn rail_let_go(&mut self, what: Dragged, from: Point<Pixels>, cx: &mut Context<Self>) {
        let now = cx.background_executor().now();
        self.rail_fx.lifted = None;
        self.rail_fx.gap = None;
        self.rail_fx.leaving = None;
        // It settles into its place (or back where it was) from where
        // it was let go.
        self.rail_fx.landing = (!self.rail_still(cx)).then(|| Landing {
            what,
            from,
            tween: Tween::begin(now, motion::rail::SETTLE),
        });
        self.run_rail_ticker(cx);
        cx.notify();
    }

    /// The button came up: a drag over a drop makes its move, and one
    /// over nothing takes the item back where it was.
    pub(super) fn rail_release(&mut self, cx: &mut Context<Self>) {
        let from = self.floating_origin();
        match self.rail_drag.release() {
            Released::Click => {}
            Released::Dropped(dragged, drop) => {
                self.change_rail(
                    |rail| {
                        rail.apply(&dragged, &drop);
                    },
                    cx,
                );
                self.rail_let_go(dragged, from, cx);
            }
            Released::Returned(dragged) => self.rail_let_go(dragged, from, cx),
        }
    }

    /// Escape during a drag: the item goes back where it was. Returns
    /// whether there was a drag to call off.
    pub(super) fn rail_cancel(&mut self, cx: &mut Context<Self>) -> bool {
        let from = self.floating_origin();
        match self.rail_drag.cancel() {
            Some(dragged) => {
                self.rail_let_go(dragged, from, cx);
                true
            }
            None => false,
        }
    }

    /// The row a number or a group was last drawn in.
    fn rail_row_of(&self, what: &Dragged) -> Option<Row> {
        self.rail_rows
            .borrow()
            .iter()
            .find(|row| match (&row.what, what) {
                (RowKind::Account(_, key), Dragged::Account(wanted))
                | (RowKind::Member(_, _, key), Dragged::Account(wanted)) => key == wanted,
                (RowKind::Group(_, id), Dragged::Group(wanted)) => id == wanted,
                _ => false,
            })
            .cloned()
    }

    // ----- drawing -------------------------------------------------------

    /// A number's face: the picture the user chose, else their emoji or
    /// initials on their colour, else the number's own WhatsApp picture,
    /// else its initials. A circle of exactly `size`, clipped.
    fn rail_face(&self, account: &Account, size: Pixels, palette: &Palette) -> Div {
        let look = self.rail.look(&self.rail_key(&account.id));
        let name = self.account_name(account);
        let picture = if look.image {
            self.media
                .stored_picture(&account.id, &ChatId::new(ICON_SUBJECT))
        } else if look.glyph.is_none() && look.colour.is_none() {
            // The number's own profile picture, through the same cache
            // as any contact's, under the subject the profile editor
            // writes it to: a new picture set there shows here.
            client_core::own_picture_subject(account)
                .filter(|_| account.connection.is_connected())
                .and_then(|subject| self.media.avatar(&account.id, &subject))
        } else {
            None
        };
        let face = div()
            .flex_none()
            .size(size)
            .rounded_full()
            .overflow_hidden()
            .border_1()
            .border_color(palette.border)
            .flex()
            .items_center()
            .justify_center();
        match picture {
            Some(picture) => face.bg(palette.muted).child(
                img(picture)
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            ),
            None => {
                // One letter in a small face: two would not fit.
                let small = size < RailGeometry::now().face * 0.75;
                let glyph = look
                    .glyph
                    .clone()
                    .filter(|glyph| !glyph.trim().is_empty())
                    .unwrap_or_else(|| initials(&name));
                let glyph: String = if small {
                    glyph.chars().take(1).collect()
                } else {
                    glyph
                };
                let (fill, ink): (Hsla, Hsla) = match look.colour {
                    Some(colour) => (
                        palette.rail[usize::from(colour) % palette.rail.len()],
                        palette.on_rail,
                    ),
                    None => (palette.muted, palette.on_avatar),
                };
                face.bg(fill)
                    .font_family(fonts::MONO)
                    .text_color(ink)
                    .text_size(size * if small { 0.5 } else { 0.34 })
                    .line_height(size)
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(glyph))
            }
        }
    }

    /// The ring of an item: what says it is the one selected, or the one a
    /// drag would land on. It is drawn first, under the face, the count
    /// and the light. (A box's own border is painted after its children,
    /// which is how the ring used to run across the count.)
    fn rail_ring(colour: Hsla, radius: Option<Pixels>, selector: String) -> Div {
        let out = gpui_kit::px(-RING);
        let ring = div()
            .debug_selector(move || selector.clone())
            .absolute()
            .top(out)
            .left(out)
            .right(out)
            .bottom(out)
            .border_2()
            .border_color(colour);
        match radius {
            Some(radius) => ring.rounded(radius),
            None => ring.rounded_full(),
        }
    }

    /// The fill of a count, the ink on it, and what it is cut out of the
    /// ring and the face with: the rail's own background.
    pub(super) fn rail_badge_colours(muted: bool, palette: &Palette) -> (Hsla, Hsla, Hsla) {
        if muted {
            (
                palette.badge_muted,
                palette.on_badge_muted,
                palette.background,
            )
        } else {
            (
                palette.accent_fill,
                palette.on_accent_fill,
                palette.background,
            )
        }
    }

    /// A count on the top-right corner of an item. The same place on a
    /// number and on a group, so the counts line up down the rail.
    ///
    /// It stands on a rim of the rail's background, so the ring and the
    /// face stop before it; its fill is opaque. A longer count grows to
    /// the left, away from the rail's edge.
    fn rail_badge(count: u32, muted: bool, palette: &Palette) -> Div {
        let (fill, ink, rim) = Self::rail_badge_colours(muted, palette);
        div()
            .debug_selector(move || format!("rail-badge-{count}"))
            .absolute()
            .top(px(-4.))
            .right(px(-6.))
            .p(KNOCKOUT())
            .rounded_full()
            .bg(rim)
            .child(
                div()
                    .debug_selector(move || format!("rail-badge-fill-{count}"))
                    .min_w(px(14.))
                    .h(px(14.))
                    .px(px(3.))
                    .rounded_full()
                    .bg(fill)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(ink)
                    .child(
                        mono(if count > 99 {
                            "99+".to_owned()
                        } else {
                            count.to_string()
                        })
                        .text_size(px(9.5))
                        .font_weight(FontWeight::MEDIUM),
                    ),
            )
    }

    /// The status light on the bottom-right corner of an item, on the
    /// same rim. A number that needs somebody carries a mark instead of
    /// the light: larger, and read without knowing the colours.
    fn rail_light(light: RailLight, selector: String, palette: &Palette) -> Div {
        let dot = div().size(px(8.)).rounded_full();
        div()
            .debug_selector(move || selector.clone())
            .absolute()
            .bottom(px(-1.))
            .right(px(-1.))
            .p(KNOCKOUT())
            .rounded_full()
            .bg(palette.background)
            .child(match light {
                RailLight::Dot(colour) => dot.bg(colour),
                // Never linked: a hollow ring, not an alarm.
                RailLight::Hollow => dot
                    .border_1()
                    .border_color(palette.text_faint)
                    .bg(palette.background),
                RailLight::Alert => div()
                    .size(px(13.))
                    .rounded_full()
                    .bg(palette.danger)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(10.))
                    .line_height(px(13.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(palette.background)
                    .child("!"),
            })
    }

    /// What a number is called when it is pointed at: its name, its
    /// phone and whether it is connected.
    pub(super) fn rail_hint(&self, account: &Account) -> SharedString {
        let name = self.account_name(account);
        let state = connection_words(account).label;
        match &account.phone {
            Some(phone) if *phone != name => format!("{name} · {phone} · {state}").into(),
            _ => format!("{name} · {state}").into(),
        }
    }

    /// What an item of the rail is drawn from, bottom to top: the ring
    /// lowest, then the face, and the count and the light over both.
    pub(super) fn rail_layers(
        ring: Div,
        face: Div,
        badge: Option<Div>,
        light: Option<Div>,
    ) -> Vec<(&'static str, Div)> {
        let mut layers = vec![("ring", ring), ("face", face)];
        layers.extend(badge.map(|badge| ("badge", badge)));
        layers.extend(light.map(|light| ("light", light)));
        layers
    }

    /// Records where a row was drawn, for the drag to know what it is
    /// over. `border` is the width of the border of the box it is put
    /// in, so that what is recorded is the box itself, border included.
    fn rail_probe(&self, what: RowKind, border: f32) -> impl IntoElement {
        let rows = self.rail_fx.drawing.clone();
        let out = gpui_kit::px(-border);
        canvas(
            move |bounds, _, _| {
                rows.borrow_mut().push(Row {
                    what: what.clone(),
                    top: bounds.top().as_f32(),
                    bottom: bounds.bottom().as_f32(),
                });
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top(out)
        .left(out)
        .right(out)
        .bottom(out)
    }

    /// The room that opens where a dragged item would land, with the
    /// accent line across it. It grows to an item's height and the room
    /// above an item, pushing what is below out of the way. (Items carry
    /// the room above them as a margin, not as the column's gap, so that
    /// this can grow from nothing.)
    fn rail_gap(
        &self,
        drop: &Drop,
        geometry: &RailGeometry,
        now: Instant,
        still: bool,
        palette: &Palette,
    ) -> Option<Div> {
        if self.rail_drag.over() != Some(drop) {
            return None;
        }
        let open = match &self.rail_fx.gap {
            Some((slot, tween)) if slot == drop => tween.at(now, still),
            // Where it already is: the line alone says so.
            _ => 0.,
        };
        Some(
            div()
                .debug_selector(|| "rail-gap".into())
                .flex_none()
                .w_full()
                .h(geometry.cell * open)
                .mt(geometry.gap * open)
                .relative()
                .child(
                    div()
                        .debug_selector(|| "rail-insertion".into())
                        .absolute()
                        .top(geometry.cell * open / 2. - gpui_kit::px(1.))
                        .left(px(8.))
                        .right(px(8.))
                        .h(gpui_kit::px(2.))
                        .rounded_full()
                        .bg(palette.accent),
                ),
        )
    }

    /// The slot a lifted item left: its footprint, as a dashed outline.
    /// `open` shrinks it while a member is dragged out of its group.
    fn rail_placeholder(geometry: &RailGeometry, round: bool, open: f32, palette: &Palette) -> Div {
        div()
            .debug_selector(|| "rail-placeholder".into())
            .flex_none()
            .w(geometry.cell)
            .h(geometry.cell * open)
            .map(|this| {
                if round {
                    this.rounded_full()
                } else {
                    this.rounded(geometry.tile_radius())
                }
            })
            .when(open > 0.5, |this| {
                this.border_1()
                    .border_dashed()
                    .border_color(palette.text_faint)
            })
            .opacity(motion::rail::PLACEHOLDER_OPACITY)
    }

    /// How much of its slot a group's member keeps: all of it, unless it
    /// is being dragged out of the group, when the slot closes behind it.
    fn slot_open(&self, key: &Key, now: Instant, still: bool) -> f32 {
        let held = self.rail_drag.dragging() == Some(&Dragged::Account(key.clone()));
        match (&self.rail_fx.leaving, held) {
            (Some(tween), true) => 1. - tween.at(now, still),
            _ => 1.,
        }
    }

    /// One number in the rail: a cell with its face, its count and its
    /// light.
    #[allow(clippy::too_many_arguments)]
    fn rail_number(
        &self,
        account: &Account,
        index: usize,
        row: RowKind,
        geometry: &RailGeometry,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = self.rail_key(&account.id);
        let now = cx.background_executor().now();
        let still = self.rail_still(cx);
        let held = self.rail_drag.dragging() == Some(&Dragged::Account(key.clone()));
        let landing =
            self.rail_fx.landing.as_ref().is_some_and(|l| {
                l.what == Dragged::Account(key.clone()) && !l.tween.done(now, still)
            });
        if held || landing {
            // Lifted: its slot stays, empty, where it was. Dragged out of
            // its group, the slot closes.
            let open = self.slot_open(&key, now, still);
            let dashed = if open > 0.5 { 1. } else { 0. };
            return Self::rail_placeholder(geometry, true, open, palette)
                .relative()
                .child(self.rail_probe(row, dashed))
                .into_any_element();
        }
        let active = self.account.as_ref() == Some(&account.id);
        let (unread, muted) = self.rail_unread(account);
        let onto = self.rail_drag.over() == Some(&Drop::OntoAccount(key.clone()));
        let hint = self.rail_hint(account);
        let (select, press, menu, keys) = (
            account.id.clone(),
            key.clone(),
            account.id.clone(),
            account.id.clone(),
        );
        let (focus_ring, hover) = (palette.accent, palette.hover);
        let slot = match &row {
            RowKind::Member(..) => format!("rail-led-m{index}"),
            _ => format!("rail-led-a{index}"),
        };
        // Onto a number on its own: the two would become a group. The
        // target shows the tile they would make.
        let preview = onto && matches!(row, RowKind::Account(..));
        let dragged_account = match self.rail_drag.dragging() {
            Some(Dragged::Account(dragged)) => self.rail_account(dragged),
            _ => None,
        };
        let handle = self.rail_handle(&key, cx);
        let cell = div()
            .id(("account", index))
            .track_focus(&handle)
            .debug_selector(move || format!("rail-account-{index}"))
            .relative()
            .flex_none()
            .size(geometry.cell)
            // The room of the ring, which is drawn as the first child (see
            // `rail_ring`); the border itself only shows keyboard focus.
            .border_2()
            .border_color(gpui_kit::transparent_black())
            .map(|this| {
                if preview {
                    this.rounded(geometry.tile_radius())
                        .bg(palette.accent.opacity(0.18))
                } else {
                    this.rounded_full()
                }
            })
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(move |style| style.border_color(focus_ring))
            .when(!onto, |this| this.hover(move |style| style.bg(hover)))
            .when(account.never_linked(), |this| this.opacity(0.55))
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, _| {
                    this.rail_press(Dragged::Account(press.clone()), event.position);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    let target = RailTarget::Account(menu.clone());
                    this.open_rail_menu(target, event.position, window, cx);
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                // The click that ends a drag is not a click.
                if !this.rail_drag.swallow_click() {
                    this.select_account(select.clone(), window, cx);
                }
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.rail_key_on(RailTarget::Account(keys.clone()), event, window, cx);
            }));
        let face = match (preview, dragged_account) {
            (true, Some(other)) => self
                .rail_minis(&[account, other], geometry, palette)
                .debug_selector(|| "rail-preview".into()),
            _ => self.rail_face(account, geometry.face, palette),
        };
        let ring = Self::rail_ring(
            if onto {
                palette.accent
            } else if active {
                palette.text
            } else {
                gpui_kit::transparent_black()
            },
            preview.then(|| geometry.tile_radius()),
            format!("rail-ring-{index}"),
        );
        let layers = Self::rail_layers(
            ring,
            face,
            (unread > 0).then(|| Self::rail_badge(unread, muted, palette)),
            Some(Self::rail_light(
                RailLight::of(account, palette),
                slot,
                palette,
            )),
        );
        cell.children(layers.into_iter().map(|(_, layer)| layer))
            .child(self.rail_probe(row, RING))
            .into_any_element()
    }

    /// The small faces of a group, two by two: where each one goes is
    /// worked out, not left to wrapping, so they are a grid at every
    /// size. Up to four members show as they are; with more, the first
    /// three and a count of the rest.
    fn rail_minis(&self, members: &[&Account], geometry: &RailGeometry, palette: &Palette) -> Div {
        let mini = geometry.mini();
        let step = mini + geometry.tile_gap;
        let place = |cell: usize| {
            let (row, column) = (cell / 2, cell % 2);
            div()
                .absolute()
                .left(geometry.tile_pad + step * column as f32)
                .top(geometry.tile_pad + step * row as f32)
                .size(mini)
        };
        let shown = if members.len() > 4 { 3 } else { members.len() };
        let mut grid = div().relative().flex_none().size(geometry.tile_inner());
        for (cell, account) in members.iter().take(shown).enumerate() {
            grid = grid.child(
                place(cell)
                    .debug_selector(move || format!("rail-mini-{cell}"))
                    .child(self.rail_face(account, mini, palette)),
            );
        }
        if members.len() > 4 {
            let more = members.len() - 3;
            grid = grid.child(
                place(3)
                    .debug_selector(|| "rail-mini-more".into())
                    .rounded_full()
                    .bg(palette.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .font_family(fonts::MONO)
                    .text_size(mini * 0.5)
                    .line_height(mini)
                    .text_color(palette.text_muted)
                    .child(SharedString::from(if more > 9 {
                        "9+".to_owned()
                    } else {
                        format!("+{more}")
                    })),
            );
        }
        grid
    }

    /// A group: a tile of small faces when closed, a container with its
    /// numbers when open, and something in between while it folds.
    fn rail_group(
        &self,
        group: &Group,
        at: usize,
        geometry: &RailGeometry,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = cx.background_executor().now();
        let still = self.rail_still(cx);
        let members: Vec<&Account> = group
            .members
            .iter()
            .filter_map(|key| self.rail_account(key))
            .collect();
        let id = group.id;
        let colour = palette.rail[usize::from(group.colour) % palette.rail.len()];
        let onto = self.rail_drag.over() == Some(&Drop::OntoGroup(id));
        let held = self.rail_drag.dragging() == Some(&Dragged::Group(id));
        let landing = self
            .rail_fx
            .landing
            .as_ref()
            .is_some_and(|l| l.what == Dragged::Group(id) && !l.tween.done(now, still));
        let open = self.fold_progress(group, now, still);
        if held || landing {
            // Lifted: the slot keeps the footprint it had.
            let slot = Self::rail_placeholder(geometry, false, 1., palette)
                .relative()
                .child(self.rail_probe(RowKind::Group(at, id), 1.));
            return if open > 0. {
                slot.w(geometry.open_width())
                    .h(geometry.open_height(group.members.len()))
                    .into_any_element()
            } else {
                slot.into_any_element()
            };
        }
        // What rolls up to the group: every unread message of a number
        // that is not muted, and the worst of the lights.
        let counted: u32 = members
            .iter()
            .map(|account| self.rail_unread(account))
            .filter(|(_, muted)| !muted)
            .map(|(unread, _)| unread)
            .sum();
        let muted_only: u32 = members
            .iter()
            .map(|account| self.rail_unread(account))
            .filter(|(_, muted)| *muted)
            .map(|(unread, _)| unread)
            .sum();
        let light = rail::worst(
            members
                .iter()
                .map(|account| (RailLight::of(account, palette), severity(account))),
        )
        .unwrap_or(RailLight::Hollow);
        let names: Vec<String> = members
            .iter()
            .map(|account| self.account_name(account))
            .collect();
        let hint: SharedString = format!("{} · {}", group.name, names.join(", ")).into();
        // The number on screen is one of these: the group carries its mark.
        let active = members
            .iter()
            .any(|account| self.account.as_ref() == Some(&account.id));
        let (focus_ring, hover) = (palette.accent, palette.hover);

        let handle = self.rail_handle(&super::panes::rail_group_name(id), cx);
        let head = div()
            .id(("rail-group", id as usize))
            .track_focus(&handle)
            .debug_selector(move || format!("rail-group-{id}"))
            .relative()
            .flex_none()
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(move |style| style.border_color(focus_ring))
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, _| {
                    this.rail_press(Dragged::Group(id), event.position);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_rail_menu(RailTarget::Group(id), event.position, window, cx);
                }),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.rail_drag.swallow_click() {
                    this.toggle_rail_group(id, cx);
                }
            }))
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, window, cx| {
                this.rail_key_on(RailTarget::Group(id), event, window, cx);
            }));

        // The tile's fill and hairline, inside the ring.
        let tile = |fade: f32| {
            div()
                .flex_none()
                .size(geometry.cell - gpui_kit::px(RING * 2.))
                .rounded(geometry.tile_radius() - gpui_kit::px(RING))
                .overflow_hidden()
                .border_1()
                .border_color(palette.border)
                .bg(colour.opacity(0.2))
                .opacity(fade)
                .child(self.rail_minis(&members, geometry, palette))
        };

        if open <= 0. {
            // Closed: one cell, exactly a number's footprint.
            return head
                .size(geometry.cell)
                .rounded(geometry.tile_radius())
                .border_2()
                .border_color(gpui_kit::transparent_black())
                .flex()
                .items_center()
                .justify_center()
                .when(onto, |this| this.bg(palette.accent.opacity(0.18)))
                .when(!onto, |this| this.hover(move |style| style.bg(hover)))
                .children(
                    Self::rail_layers(
                        Self::rail_ring(
                            if onto {
                                palette.accent
                            } else if active {
                                palette.text
                            } else {
                                gpui_kit::transparent_black()
                            },
                            Some(geometry.tile_radius()),
                            format!("rail-ring-g{id}"),
                        ),
                        tile(1.).debug_selector(move || format!("rail-tile-{id}")),
                        (counted > 0 || muted_only > 0).then(|| {
                            if counted > 0 {
                                Self::rail_badge(counted, false, palette)
                            } else {
                                Self::rail_badge(muted_only, true, palette)
                            }
                        }),
                        (!members.is_empty())
                            .then(|| Self::rail_light(light, format!("rail-led-g{id}"), palette)),
                    )
                    .into_iter()
                    .map(|(_, layer)| layer),
                )
                .child(self.rail_probe(RowKind::Group(at, id), RING))
                .into_any_element();
        }

        // Open: a container around a head (which closes it) and the
        // numbers, each a full cell, with the same room on every side.
        let folding = open < 1.;
        let mut column = div().flex().flex_col().items_center().opacity(open).child(
            head.w(geometry.cell)
                .h(geometry.head)
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(gpui_kit::transparent_black())
                .flex()
                .items_center()
                .justify_center()
                .hover(move |style| style.bg(hover))
                .child(icon(IconName::ChevronDown, px(13.), colour))
                .child(self.rail_probe(RowKind::Group(at, id), 1.)),
        );
        for (place, key) in group.members.iter().enumerate() {
            let Some(account) = self.rail_account(key) else {
                continue;
            };
            let index = self
                .accounts
                .iter()
                .position(|known| known.id == account.id)
                .unwrap_or(0);
            column = column
                .children(self.rail_gap(&Drop::InGroup(id, place), geometry, now, still, palette))
                .child(
                    div()
                        .flex_none()
                        // The room above it goes with its slot.
                        .mt(geometry.gap * self.slot_open(key, now, still))
                        .child(self.rail_number(
                            account,
                            index,
                            RowKind::Member(id, place, key.clone()),
                            geometry,
                            palette,
                            cx,
                        )),
                );
        }
        column = column.children(self.rail_gap(
            &Drop::InGroup(id, group.members.len()),
            geometry,
            now,
            still,
            palette,
        ));
        let full = geometry.open_height(group.members.len());
        div()
            .debug_selector(move || format!("rail-group-open-{id}"))
            .relative()
            .flex_none()
            .p(geometry.pad)
            .rounded(geometry.tile_radius() + geometry.pad)
            .border_1()
            .border_color(if onto { palette.accent } else { palette.border })
            .bg(colour.opacity(0.14))
            .when(onto, |this| this.bg(palette.accent.opacity(0.18)))
            .map(|this| {
                if folding {
                    // Between a cell and its full size, with what is
                    // inside fading in and the small faces fading out.
                    this.w(geometry.cell + (geometry.open_width() - geometry.cell) * open)
                        .h(geometry.cell + (full - geometry.cell) * open)
                        .overflow_hidden()
                        .child(
                            div()
                                .absolute()
                                .top(gpui_kit::px(RING - 1.))
                                .left(gpui_kit::px(RING - 1.))
                                .child(tile(1. - open)),
                        )
                } else {
                    this.w(geometry.open_width())
                }
            })
            .child(column)
            // The foot: below it is outside the group.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(geometry.pad)
                    .child(self.rail_probe(RowKind::GroupEnd(at, id, group.members.len()), 0.)),
            )
            .into_any_element()
    }

    /// The copy of a lifted item that follows the pointer, or settles
    /// into place after it was let go. Drawn over the whole window.
    pub(super) fn render_rail_floating(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let now = cx.background_executor().now();
        let still = self.rail_still(cx);
        let geometry = RailGeometry::now();
        // Held: under the pointer, a little larger. Let go: on its way
        // from there to its row, back at its own size.
        let (what, origin, lift) = match (self.rail_drag.dragging(), &self.rail_fx.landing) {
            (Some(what), _) => {
                let lift = self
                    .rail_fx
                    .lifted
                    .as_ref()
                    .map_or(1., |tween| tween.at(now, still));
                (what.clone(), self.floating_origin(), lift)
            }
            (None, Some(landing)) if !landing.tween.done(now, still) => {
                let progress = landing.tween.at(now, still);
                let home = self.rail_row_of(&landing.what).map(|row| Point {
                    x: geometry.cell_left(),
                    y: gpui_kit::px(row.top),
                })?;
                let origin = Point {
                    x: landing.from.x + (home.x - landing.from.x) * progress,
                    y: landing.from.y + (home.y - landing.from.y) * progress,
                };
                (landing.what.clone(), origin, 1. - progress)
            }
            _ => return None,
        };
        let scale = 1. + (motion::rail::LIFT_SCALE - 1.) * lift;
        let side = geometry.cell * scale;
        // Grown about its centre, so it does not slide as it lifts.
        let shift = (side - geometry.cell) / 2.;
        let face: AnyElement = match &what {
            Dragged::Account(key) => {
                let account = self.rail_account(key)?;
                div()
                    .size(side)
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(self.rail_face(account, geometry.face * scale, palette))
                    .into_any_element()
            }
            Dragged::Group(id) => {
                let group = self.rail.group(*id)?;
                let members: Vec<&Account> = group
                    .members
                    .iter()
                    .filter_map(|key| self.rail_account(key))
                    .collect();
                let colour = palette.rail[usize::from(group.colour) % palette.rail.len()];
                div()
                    .size(side)
                    .rounded(geometry.tile_radius())
                    .bg(palette.background)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .rounded(geometry.tile_radius())
                            .overflow_hidden()
                            .bg(colour.opacity(0.2))
                            .child(self.rail_minis(&members, &geometry, palette)),
                    )
                    .into_any_element()
            }
        };
        let round = matches!(what, Dragged::Account(_));
        Some(
            div()
                .id("rail-drag-layer")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                // The hand is closed on what it carries.
                .when(self.rail_drag.dragging().is_some(), |this| {
                    this.cursor_grabbing()
                })
                .child(
                    div()
                        .debug_selector(|| "rail-floating".into())
                        .absolute()
                        .left(origin.x - shift)
                        .top(origin.y - shift)
                        .size(side)
                        .map(|this| {
                            if round {
                                this.rounded_full()
                            } else {
                                this.rounded(geometry.tile_radius())
                            }
                        })
                        // Flat, like everything else: a hairline, and a
                        // faint glow where a shadow would be.
                        .border_1()
                        .border_color(palette.lift_outline)
                        .shadow(vec![BoxShadow {
                            color: palette.lift_glow,
                            offset: gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)),
                            blur_radius: px(10.) * lift,
                            spread_radius: gpui_kit::px(0.),
                            inset: false,
                        }])
                        .bg(palette.background)
                        .opacity(1. - (1. - motion::rail::LIFT_OPACITY) * lift)
                        .child(face),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_rail(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // What the last frame drew is what the pointer is tested against
        // and what a settling item heads for; this frame is measured
        // into a list of its own.
        {
            let mut drawn = self.rail_fx.drawing.borrow_mut();
            if !drawn.is_empty() {
                *self.rail_rows.borrow_mut() = std::mem::take(&mut *drawn);
            }
        }
        let geometry = RailGeometry::now();
        let now = cx.background_executor().now();
        let still = self.rail_still(cx);
        // Each item carries the room above it, so the column itself has
        // no gap and room can open between two items from nothing.
        let mut items = div()
            .flex()
            .flex_col()
            .items_center()
            .pt(px(4.))
            .pb(geometry.gap + px(4.));
        for (at, item) in self.rail.items.iter().enumerate() {
            let element = match item {
                RailItem::Account(key) => {
                    let Some(account) = self.rail_account(key) else {
                        // A number of another provider: not this session's.
                        continue;
                    };
                    let index = self
                        .accounts
                        .iter()
                        .position(|known| known.id == account.id)
                        .unwrap_or(at);
                    self.rail_number(
                        account,
                        index,
                        RowKind::Account(at, key.clone()),
                        &geometry,
                        palette,
                        cx,
                    )
                }
                RailItem::Group(group) => self.rail_group(group, at, &geometry, palette, cx),
            };
            items = items
                .children(self.rail_gap(&Drop::Top(at), &geometry, now, still, palette))
                .child(
                    div()
                        .debug_selector(move || format!("rail-item-{at}"))
                        .flex_none()
                        .mt(geometry.gap)
                        .child(element),
                );
        }
        items = items.children(self.rail_gap(
            &Drop::Top(self.rail.items.len()),
            &geometry,
            now,
            still,
            palette,
        ));

        let theme_icon = if palette.is_dark() {
            IconName::Sun
        } else {
            IconName::Moon
        };

        div()
            .flex_none()
            .w(metrics::RAIL_WIDTH())
            .h_full()
            .bg(palette.background)
            .border_r_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .items_center()
            .child(
                // The logo cell. Its rule continues the headers' rule, and
                // the logo leads back to the start screen.
                div()
                    .id("home")
                    .debug_selector(|| "rail-logo".into())
                    .flex_none()
                    .w_full()
                    .h(metrics::HEADER_HEIGHT())
                    .border_b_1()
                    .border_color(palette.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.close_chat(cx)))
                    .child(logo(px(24.), palette)),
            )
            // Where to: the chats, or Status.
            .when(self.has_status(), |this| {
                this.child(self.render_rail_nav(palette, cx))
            })
            // The numbers scroll when there are more than fit; what is
            // above and below them stays put.
            .child(
                div()
                    .id("rail-numbers")
                    .debug_selector(|| "rail-numbers".into())
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.rail_scroll)
                    .child(items),
            )
            .child(
                div()
                    .debug_selector(|| "rail-tools".into())
                    .flex_none()
                    .w_full()
                    .py(geometry.gap)
                    .border_t_1()
                    .border_color(palette.border)
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(geometry.gap / 2.)
                    // Only while a new version waits for a restart.
                    .children(self.render_update_notice(palette, cx))
                    .when(self.engine.capabilities().link_accounts, |this| {
                        this.child(
                            super::hints::hinted(
                                icon_button("add-number", IconName::Plus, palette),
                                "add-number",
                            )
                            .when(self.overlay == Overlay::AddNumber, |this| {
                                this.bg(palette.muted)
                            })
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.open_overlay(Overlay::AddNumber, window, cx)
                                },
                            )),
                        )
                    })
                    .child(
                        super::hints::hinted(
                            icon_button("toggle-theme", theme_icon, palette),
                            "toggle-theme",
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx))),
                    )
                    // Every shortcut, a click away: nobody learns the
                    // keys of a sheet they do not know is there.
                    .child(
                        super::hints::hinted(
                            icon_button("shortcuts", IconName::Keyboard, palette),
                            "shortcuts",
                        )
                        .when(self.overlay == Overlay::Shortcuts, |this| {
                            this.bg(palette.muted)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_overlay(Overlay::Shortcuts, window, cx)
                        })),
                    )
                    .child(
                        super::hints::hinted(
                            icon_button("settings", IconName::Settings, palette),
                            "settings",
                        )
                        .when(self.overlay == Overlay::Settings, |this| {
                            this.bg(palette.muted)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_overlay(Overlay::Settings, window, cx)
                        })),
                    ),
            )
    }

    /// Keys on a focused number or group: Enter or Space is the click,
    /// the menu key (or Shift+F10) opens its menu, and Alt with an arrow
    /// moves it.
    pub(super) fn rail_key_on(
        &mut self,
        target: RailTarget,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stroke = &event.keystroke;
        let dragged = match &target {
            RailTarget::Account(account) => Dragged::Account(self.rail_key(account)),
            RailTarget::Group(id) => Dragged::Group(*id),
        };
        match stroke.key.as_str() {
            "enter" | "space" => match &target {
                RailTarget::Account(account) => self.select_account(account.clone(), window, cx),
                RailTarget::Group(id) => self.toggle_rail_group(*id, cx),
            },
            "menu" | "f10" if stroke.key == "menu" || stroke.modifiers.shift => {
                // Beside the item, where a right-click would have been.
                let at = Point {
                    x: metrics::RAIL_WIDTH() - px(8.),
                    y: self
                        .rail_row_top(&target)
                        .unwrap_or(metrics::HEADER_HEIGHT()),
                };
                self.open_rail_menu(target, at, window, cx);
            }
            "up" | "down" if stroke.modifiers.alt => {
                let up = stroke.key == "up";
                self.change_rail(
                    |rail| {
                        rail.nudge(&dragged, up);
                    },
                    cx,
                );
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Where a number or group was last drawn.
    fn rail_row_top(&self, target: &RailTarget) -> Option<Pixels> {
        let wanted = match target {
            RailTarget::Account(account) => Some(self.rail_key(account)),
            RailTarget::Group(_) => None,
        };
        self.rail_rows
            .borrow()
            .iter()
            .find(|row| match (&row.what, target) {
                (RowKind::Account(_, key), _) | (RowKind::Member(_, _, key), _) => {
                    Some(key) == wanted.as_ref()
                }
                (RowKind::Group(_, id), RailTarget::Group(wanted)) => id == wanted,
                _ => false,
            })
            .map(|row| gpui_kit::px(row.top))
    }
}
