//! Calls: the card that says a call rings or is on, with what can be done
//! about it, and the button that places one.
//!
//! Nothing of a call is kept: the cards are drawn from what the engine
//! says is ringing or on right now (`SyncEngine::calls`). The provider
//! carries the sound and the video; this is only the window's side of it:
//! with video, the pictures the provider hands over are drawn as they
//! come.

use super::shell::Shell;
use super::widgets::{avatar_or, mono, AvatarKind};
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::LiveCall;
use client_provider::{CallPicture, CallScreen, CallState, ChatKind, Timestamp};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, img, AnyElement, Context, Div, Entity, FontWeight, Hsla, ObjectFit, Pixels, Point,
    RenderImage, SharedString, Stateful, Task,
};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// How often the pictures of a call with video are looked at again.
const VIDEO_TICK: Duration = Duration::from_millis(66);
/// How often the time of a call without video is drawn again.
const CLOCK_TICK: Duration = Duration::from_millis(500);

/// A picture as it was last drawn: the count it had on the provider's
/// screen, and the picture, if that side shows one.
type Drawn = (u64, Option<Arc<RenderImage>>);

/// What the window keeps of the calls on screen: the pictures as it draws
/// them, and the clock that repaints them.
#[derive(Default)]
pub(super) struct CallDeck {
    /// The picture drawn for each side of each call, with the count it
    /// had on the provider's screen.
    drawn: RefCell<HashMap<(String, String), Drawn>>,
    /// Pictures that were replaced, which the window still holds.
    stale: RefCell<Vec<Arc<RenderImage>>>,
    /// The sound of a call that rings.
    pub(super) ringer: crate::ring::Ringer,
    /// How far the cards were dragged from their corner: to the right
    /// and down. Kept from one call to the next.
    pub(super) moved: Point<Pixels>,
    /// While a card is held: where the pointer was when it was pressed,
    /// less how far the cards had been moved already.
    held: Option<Point<Pixels>>,
    /// How wide the cards were made by their corner; `None`: as they
    /// come. Kept from one call to the next.
    pub(super) width: Option<Pixels>,
    /// While a card's corner is held: where the pointer was when it was
    /// pressed, and how wide and how far right the cards were then.
    sizing: Option<(Point<Pixels>, Pixels, Pixels)>,
    /// The call somebody is being added to, and what is typed to find
    /// them.
    adding: Option<Adding>,
    _tick: Option<Task<()>>,
}

/// Somebody is being looked for, to be added to a call.
struct Adding {
    call: String,
    search: Entity<InputState>,
    _typed: gpui_kit::Subscription,
}

/// The slots of a call's pictures that are not a guest's.
const REMOTE: &str = "remote";
const LOCAL: &str = "local";
/// How many people are offered at once to be added to a call.
const ADD_SHOWN: usize = 5;

/// What should ring for these calls: somebody calling before anything
/// else, and nothing over a call that is on, which is not to be talked
/// over.
pub(super) fn what_rings(calls: &[LiveCall]) -> Option<crate::ring::Ringing> {
    let on = calls.iter().any(|call| {
        matches!(
            call.event.state,
            CallState::Connecting | CallState::Connected
        )
    });
    if on {
        None
    } else if calls.iter().any(LiveCall::waits_for_answer) {
        Some(crate::ring::Ringing::Incoming)
    } else if calls.iter().any(|call| {
        matches!(
            call.event.state,
            CallState::Ringing {
                incoming: false,
                ..
            }
        )
    }) {
        Some(crate::ring::Ringing::Outgoing)
    } else {
        None
    }
}

impl CallDeck {
    /// The newest picture of one side of a call, ready to draw.
    fn picture(&self, call: &str, slot: &str, screen: &CallScreen) -> Option<Arc<RenderImage>> {
        let (count, newest) = screen.newest();
        let mut drawn = self.drawn.borrow_mut();
        let key = (call.to_owned(), slot.to_owned());
        if let Some((seen, image)) = drawn.get(&key) {
            if *seen == count {
                return image.clone();
            }
        }
        let image = newest.as_deref().and_then(render_image);
        if let Some((_, Some(previous))) = drawn.insert(key, (count, image.clone())) {
            self.stale.borrow_mut().push(previous);
        }
        image
    }

    /// Lets go of the pictures of calls that are over.
    fn keep(&self, live: &[LiveCall]) {
        let mut drawn = self.drawn.borrow_mut();
        let mut stale = self.stale.borrow_mut();
        drawn.retain(|(call, _), (_, image)| {
            let on = live.iter().any(|held| held.event.call_id == *call);
            if !on {
                stale.extend(image.take());
            }
            on
        });
    }

    /// Frees the pictures the window holds that were replaced.
    pub(super) fn release(&self, window: &mut gpui_kit::Window) {
        for image in self.stale.borrow_mut().drain(..) {
            let _ = window.drop_image(image);
        }
    }
}

/// A picture of a call as the window draws one.
fn render_image(picture: &CallPicture) -> Option<Arc<RenderImage>> {
    let image = image::RgbaImage::from_raw(picture.width, picture.height, picture.bgra.clone())?;
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(image)])))
}

/// How wide a call's card is.
#[allow(non_snake_case)]
fn CARD_WIDTH() -> Pixels {
    px(340.)
}

/// How far from the window's edge the cards start.
#[allow(non_snake_case)]
fn CARD_MARGIN() -> Pixels {
    px(16.)
}

/// The narrowest and the widest a card is made by its corner.
#[allow(non_snake_case)]
fn CARD_NARROWEST() -> Pixels {
    px(260.)
}

#[allow(non_snake_case)]
fn CARD_WIDEST() -> Pixels {
    px(900.)
}

/// How the people of a call with several share its pictures: how many
/// across, how many down, and how tall a place is for its width. Two
/// stand side by side, upright as a phone films; more make a grid of
/// squares.
pub(super) fn grid_of(people: usize) -> (usize, usize, f32) {
    match people {
        0 | 1 => (1, 1, 1.0),
        2 => (2, 1, 1.5),
        3 | 4 => (2, 2, 1.0),
        more => (3, more.div_ceil(3), 1.0),
    }
}

/// One of the round buttons under a call.
fn round_button(
    selector: &'static str,
    says: &'static str,
    glyph: IconName,
    fill: Hsla,
    ink: Hsla,
    palette: &Palette,
) -> Stateful<Div> {
    let ring = palette.accent;
    div()
        .id(selector)
        .debug_selector(move || selector.into())
        .flex_none()
        .size(px(48.))
        .rounded_full()
        .bg(fill)
        .border_2()
        .border_color(gpui_kit::transparent_black())
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .tab_index(0)
        .hover(|style| style.opacity(0.88))
        .focus_visible(move |style| style.border_color(ring))
        .tooltip(move |window, cx| Tooltip::new(says).build(window, cx))
        .child(icon(glyph, px(20.), ink))
}

/// How long a call has been on, as a clock.
pub(super) fn call_time(call: &LiveCall, now: Timestamp) -> Option<String> {
    let since = call.connected_at?;
    if !matches!(call.event.state, CallState::Connected) {
        return None;
    }
    let seconds = (now.as_millis() - since.as_millis()).max(0) / 1000;
    Some(crate::format::duration(seconds as u32))
}

/// What a card says under the name.
pub(super) fn call_words(call: &LiveCall) -> &'static str {
    match (&call.event.state, call.event.video) {
        (
            CallState::Ringing {
                incoming: true,
                group: true,
            },
            _,
        ) => "Incoming group call",
        (CallState::Ringing { incoming: true, .. }, true) => "Incoming video call",
        (CallState::Ringing { incoming: true, .. }, false) => "Incoming voice call",
        (
            CallState::Ringing {
                incoming: false, ..
            },
            _,
        ) => "Calling…",
        (CallState::Connecting, _) => "Connecting…",
        (CallState::Connected, _) if call.muted => "On a call, microphone off",
        (CallState::Connected, _) => "On a call",
        (CallState::Ended { .. }, _) => "Call ended",
    }
}

impl Shell {
    /// Calls the contact of the chat that is open, with video or without.
    pub(super) fn call_open_chat(&mut self, video: bool, cx: &mut Context<Self>) {
        let Some(open) = &self.open else {
            return;
        };
        if open.chat.kind != ChatKind::Direct {
            return;
        }
        self.engine
            .start_call(&open.chat.account_id, &open.chat.id, video);
        cx.notify();
    }

    /// Development aid: the contact of the open chat calls, with video.
    pub(super) fn demo_incoming_call(&self) {
        let Some(open) = &self.open else {
            return;
        };
        let _ = self
            .engine
            .apply_event(client_provider::ProviderEvent::Call(
                client_provider::CallEvent {
                    account_id: open.chat.account_id.clone(),
                    call_id: "demo-incoming".into(),
                    chat_id: open.chat.id.clone(),
                    peer: client_provider::ContactId::new(open.chat.id.as_str()),
                    video: true,
                    state: CallState::Ringing {
                        incoming: true,
                        group: false,
                    },
                    at: Timestamp::now(),
                },
            ));
    }

    /// A card is pressed: it follows the pointer until it is let go.
    fn hold_call_card(&mut self, at: Point<Pixels>) {
        self.call_deck.held = Some(at - self.call_deck.moved);
    }

    /// The pointer moved: a card that is held goes with it, as far as the
    /// window's edges.
    pub(super) fn drag_call_card(
        &mut self,
        at: Point<Pixels>,
        pressed: bool,
        window: &gpui_kit::Window,
        cx: &mut Context<Self>,
    ) {
        // The button's release is not always heard here (a click on one
        // of the card's switches keeps it to itself): a pointer that
        // moves with no button down holds nothing.
        if !pressed {
            self.call_deck.held = None;
            self.call_deck.sizing = None;
        }
        let room = window.viewport_size();
        let (left, top) = (CARD_MARGIN(), metrics::HEADER_HEIGHT() + px(12.));
        // Held by its corner, the card grows and shrinks with the
        // pointer, its other corner staying where it is.
        if let Some((from, width, right)) = self.call_deck.sizing {
            let most = (room.width - left * 2.)
                .min(CARD_WIDEST())
                .max(CARD_NARROWEST());
            let wanted = (width + (at.x - from.x)).clamp(CARD_NARROWEST(), most);
            self.call_deck.width = Some(wanted);
            self.call_deck.moved.x = right + (wanted - width);
            cx.notify();
            return;
        }
        let Some(from) = self.call_deck.held else {
            return;
        };
        // The corner it starts in is the top right. It may go left until
        // its left edge meets the window's, and down while its top with
        // its name is still in the window.
        let most_left = (room.width - self.card_width() - left).max(px(0.));
        let most_down = (room.height - top - px(96.)).max(px(0.));
        let moved = at - from;
        self.call_deck.moved = gpui_kit::point(
            moved.x.clamp(-most_left, left),
            moved.y.clamp(-top, most_down),
        );
        cx.notify();
    }

    /// The pointer was let go.
    pub(super) fn drop_call_card(&mut self) {
        self.call_deck.held = None;
        self.call_deck.sizing = None;
    }

    /// How wide a call's card is: as the user made it, or as it comes.
    pub(super) fn card_width(&self) -> Pixels {
        self.call_deck.width.unwrap_or(CARD_WIDTH())
    }

    /// Opens or closes the search for somebody to add to a call.
    fn toggle_adding(&mut self, call: &str, window: &mut gpui_kit::Window, cx: &mut Context<Self>) {
        let open = self
            .call_deck
            .adding
            .as_ref()
            .is_some_and(|adding| adding.call == call);
        self.call_deck.adding = None;
        if !open {
            let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search a contact"));
            let typed = cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            });
            search.update(cx, |search, cx| search.focus(window, cx));
            self.call_deck.adding = Some(Adding {
                call: call.to_owned(),
                search,
                _typed: typed,
            });
        }
        cx.notify();
    }

    /// The people who can be added to `call`: who the number has a chat
    /// with, then its other contacts, that match what is typed, less who
    /// the call is with already.
    fn people_to_add(
        &self,
        call: &LiveCall,
        cx: &gpui_kit::App,
    ) -> Vec<(client_provider::ContactId, SharedString)> {
        let Some(adding) = &self.call_deck.adding else {
            return Vec::new();
        };
        let typed = adding.search.read(cx).value().trim().to_lowercase();
        let there: Vec<client_provider::ContactId> = self
            .engine
            .call_video(&call.event.call_id)
            .map(|video| video.guests.all().into_iter().map(|(who, _)| who).collect())
            .unwrap_or_default();
        let store = self.engine.store();
        let account = &call.event.account_id;
        let mut people: Vec<(client_provider::ContactId, SharedString)> = Vec::new();
        let chats = store.chats(account, Some(&typed)).unwrap_or_default();
        let from_chats = chats
            .into_iter()
            .filter(|chat| chat.kind == ChatKind::Direct)
            .map(|chat| {
                (
                    client_provider::ContactId::new(chat.id.as_str()),
                    chat.title,
                )
            });
        let contacts = store
            .contacts(account, Some(&typed), ADD_SHOWN * 4)
            .unwrap_or_default();
        let from_contacts = contacts.into_iter().map(|contact| {
            let name = contact
                .name
                .or(contact.phone)
                .unwrap_or_else(|| contact.id.to_string());
            (contact.id, name)
        });
        for (who, name) in from_chats.chain(from_contacts) {
            let known = who == call.event.peer
                || there.contains(&who)
                || people.iter().any(|(listed, _)| *listed == who);
            if !known {
                people.push((who, name.into()));
            }
            if people.len() == ADD_SHOWN {
                break;
            }
        }
        people
    }

    /// What somebody in a call is called: their chat's name, their
    /// contact's, or their number.
    fn guest_name(&self, call: &LiveCall, who: &client_provider::ContactId) -> SharedString {
        let store = self.engine.store();
        let account = &call.event.account_id;
        store
            .chat(account, &client_provider::ChatId::new(who.as_str()))
            .ok()
            .flatten()
            .map(|chat| chat.title)
            .unwrap_or_else(|| who.to_string())
            .into()
    }

    /// Whether the chat that is open can be called: a person, on a number
    /// whose provider places calls, with no call on already.
    pub(super) fn can_call_open_chat(&self) -> bool {
        self.caps_now().calls
            && self.engine.calls().is_empty()
            && self
                .open
                .as_ref()
                .is_some_and(|open| open.chat.kind == ChatKind::Direct)
    }

    /// Whether the chat that is open can be called with video.
    pub(super) fn can_video_call_open_chat(&self) -> bool {
        self.can_call_open_chat() && self.caps_now().video_calls
    }

    /// While a call rings or is on, its card is drawn again and again: the
    /// pictures of a call with video as they come, the time of any call.
    /// Stops by itself when no call is left.
    pub(super) fn watch_calls(&mut self, cx: &mut Context<Self>) {
        // A call that rings is heard, on the speakers chosen in the
        // settings; one that stopped ringing is not.
        let calls = self.engine.calls();
        self.call_deck
            .ringer
            .set(what_rings(&calls), self.audio.output_device.as_deref());
        if self.call_deck._tick.is_some() || calls.is_empty() {
            return;
        }
        self.call_deck._tick = Some(cx.spawn(async move |this, cx| {
            let clock = cx.background_executor().clone();
            loop {
                let pace = this.update(cx, |this, cx| {
                    let calls = this.engine.calls();
                    this.call_deck.keep(&calls);
                    let device = this.audio.output_device.clone();
                    this.call_deck
                        .ringer
                        .set(what_rings(&calls), device.as_deref());
                    cx.notify();
                    if calls.is_empty() {
                        this.call_deck._tick = None;
                        return None;
                    }
                    let video = calls.iter().any(|call| {
                        call.event.video && !matches!(call.event.state, CallState::Ringing { .. })
                    });
                    Some(if video { VIDEO_TICK } else { CLOCK_TICK })
                });
                match pace {
                    Ok(Some(pace)) => clock.timer(pace).await,
                    _ => break,
                }
            }
        }));
    }

    /// The stage of a call with video: the other side's picture edge to
    /// edge, who it is and for how long written over it, and the own
    /// camera small in its corner.
    fn call_stage(
        &self,
        call: &LiveCall,
        name: &SharedString,
        words: &SharedString,
    ) -> Option<AnyElement> {
        let on = matches!(
            call.event.state,
            CallState::Connecting | CallState::Connected
        );
        if !call.event.video || !on {
            return None;
        }
        let id = &call.event.call_id;
        let video = self.engine.call_video(id)?;
        let guests = video.guests.all();
        let remote = guests
            .is_empty()
            .then(|| self.call_deck.picture(id, REMOTE, &video.remote))
            .flatten();
        let local = (!call.camera_off)
            .then(|| self.call_deck.picture(id, LOCAL, &video.local))
            .flatten();
        let ink = gpui_kit::white();
        // The pictures are as wide as the card, less its border, and
        // between two as tall as wide: a phone is held upright or
        // sideways, and turns during a call.
        let side = self.card_width() - gpui_kit::px(2.);
        let (across, down, tall) = grid_of(guests.len());
        let place = side / across as f32;
        let height = place * tall * down as f32;
        let tiles: Vec<AnyElement> = guests
            .iter()
            .enumerate()
            .map(|(index, (who, screen))| {
                let image = self.call_deck.picture(id, who.as_str(), screen);
                let label = self.guest_name(call, who);
                div()
                    .debug_selector(move || format!("call-video-guest-{index}"))
                    .absolute()
                    .left(place * (index % across) as f32)
                    .top(place * tall * (index / across) as f32)
                    .w(place)
                    .h(place * tall)
                    .overflow_hidden()
                    .border_1()
                    .border_color(gpui_kit::black())
                    .children(image.map(|image| {
                        img(image)
                            .w(place)
                            .h(place * tall)
                            .object_fit(ObjectFit::Cover)
                    }))
                    .child(
                        div()
                            .absolute()
                            .left(px(6.))
                            .bottom(px(6.))
                            .max_w(place - px(12.))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(4.))
                            .bg(gpui_kit::black().opacity(0.55))
                            .truncate()
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(ink)
                            .child(label),
                    )
                    .into_any_element()
            })
            .collect();
        let waiting = remote.is_none() && guests.is_empty();
        Some(
            div()
                .debug_selector(|| "call-video".into())
                .relative()
                .w_full()
                .h(height)
                .bg(gpui_kit::black())
                .overflow_hidden()
                .flex()
                .items_center()
                .justify_center()
                // The picture is given its room: left to itself it takes
                // the height its own proportions ask for.
                .children(remote.map(|image| {
                    div()
                        .debug_selector(|| "call-video-remote".into())
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .child(img(image).w_full().h(height).object_fit(ObjectFit::Contain))
                }))
                .children(tiles)
                .when(waiting, |this| {
                    this.child(
                        div()
                            .debug_selector(|| "call-video-waiting".into())
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .size(px(72.))
                                    .rounded_full()
                                    .bg(ink.opacity(0.1))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon(IconName::Video, px(28.), ink.opacity(0.75))),
                            )
                            .child(
                                div()
                                    .text_size(metrics::TEXT_SMALL())
                                    .text_color(ink.opacity(0.65))
                                    .child("Waiting for their video…"),
                            ),
                    )
                })
                // Who and how long, over the picture: on a shade that
                // fades out, so it reads on any picture.
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left_0()
                        .right_0()
                        .px_4()
                        .pt_3()
                        .pb_6()
                        .bg(gpui_kit::linear_gradient(
                            180.,
                            gpui_kit::linear_color_stop(gpui_kit::black().opacity(0.6), 0.),
                            gpui_kit::linear_color_stop(gpui_kit::black().opacity(0.), 1.),
                        ))
                        .flex()
                        .flex_col()
                        .gap(px(1.))
                        .child(
                            div()
                                .debug_selector(|| "call-name".into())
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .text_size(metrics::TEXT_BODY())
                                .text_color(ink)
                                .child(name.clone()),
                        )
                        .child(
                            mono(words.clone())
                                .debug_selector(|| "call-state".into())
                                .text_color(ink.opacity(0.75)),
                        ),
                )
                .children(local.map(|image| {
                    div()
                        .debug_selector(|| "call-video-local".into())
                        .absolute()
                        .right(px(12.))
                        .bottom(px(12.))
                        .w(side * 0.3)
                        .h(side * 0.3 * 9. / 16.)
                        .rounded(px(10.))
                        .overflow_hidden()
                        .border_1()
                        .border_color(ink.opacity(0.35))
                        .bg(gpui_kit::black())
                        .shadow_lg()
                        .child(
                            img(image)
                                .size_full()
                                .rounded(px(10.))
                                .object_fit(ObjectFit::Cover),
                        )
                }))
                .into_any_element(),
        )
    }

    /// Who a call without pictures is with: the face, large, the name
    /// and where the call stands.
    fn call_face(
        &self,
        call: &LiveCall,
        name: &SharedString,
        words: &SharedString,
        palette: &Palette,
    ) -> AnyElement {
        let picture = self
            .media
            .avatar(&call.event.account_id, &call.event.chat_id);
        // A call that rings has a ring around its face.
        let rings = matches!(call.event.state, CallState::Ringing { .. });
        div()
            .w_full()
            .pt_6()
            .pb_2()
            .px_4()
            .flex()
            .flex_col()
            .items_center()
            .gap_3()
            .child(
                div()
                    .p(px(4.))
                    .rounded_full()
                    .border_2()
                    .border_color(if rings {
                        palette.accent
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .child(avatar_or(
                        picture,
                        name,
                        AvatarKind::Person,
                        px(72.),
                        palette,
                    )),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(2.))
                    .child(
                        div()
                            .debug_selector(|| "call-name".into())
                            .max_w_full()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .text_size(metrics::TEXT_TITLE())
                            .child(name.clone()),
                    )
                    .child(
                        mono(words.clone())
                            .debug_selector(|| "call-state".into())
                            .text_color(palette.text_muted),
                    ),
            )
            .into_any_element()
    }

    fn call_card(&self, call: &LiveCall, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let store = self.engine.store();
        let name: SharedString = store
            .chat(&call.event.account_id, &call.event.chat_id)
            .ok()
            .flatten()
            .map(|chat| chat.title)
            .unwrap_or_else(|| call.event.peer.to_string())
            .into();
        let id = call.event.call_id.clone();
        let can_video = self
            .engine
            .capabilities_of(&call.event.account_id)
            .video_calls;
        // A video call cannot be answered where there is no camera
        // behind it: it is said, and it can still be declined.
        let answerable = call.waits_for_answer() && (!call.event.video || can_video);
        let on = matches!(
            call.event.state,
            CallState::Connecting | CallState::Connected
        );
        let (muted, camera_off) = (call.muted, call.camera_off);
        let words: SharedString = match call_time(call, Timestamp::now()) {
            Some(time) => format!("{} · {time}", call_words(call)).into(),
            None => call_words(call).into(),
        };
        let (accept_id, end_id, mute_id, camera_id, add_id) =
            (id.clone(), id.clone(), id.clone(), id.clone(), id.clone());
        let adding = on
            && self
                .call_deck
                .adding
                .as_ref()
                .is_some_and(|adding| adding.call == id);
        let add_panel = adding.then(|| self.call_add_panel(call, palette, cx));
        let stage = self.call_stage(call, &name, &words);
        let face = stage
            .is_none()
            .then(|| self.call_face(call, &name, &words, palette));
        // A switch that is off is drawn filled, so that it is seen.
        let switch = |selector: &'static str, says: &'static str, glyph: IconName, off: bool| {
            let (fill, ink) = if off {
                (palette.text, palette.background)
            } else {
                (palette.muted, palette.text)
            };
            round_button(selector, says, glyph, fill, ink, palette)
        };
        div()
            .id(SharedString::from(format!("call-{id}")))
            .debug_selector(|| "call-card".into())
            .relative()
            .w(self.card_width())
            .rounded(px(16.))
            .overflow_hidden()
            .border_1()
            .border_color(palette.elevated_border)
            .bg(palette.elevated)
            .shadow_lg()
            .flex()
            .flex_col()
            // A click on the card is not a click on what is under it.
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.drop_call_card();
            }))
            // Pressed anywhere, it is moved to where it is wanted.
            .cursor_grab()
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.hold_call_card(event.position);
                }),
            )
            // Let go over the card or anywhere else, it stays there.
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, _| this.drop_call_card()),
            )
            .on_mouse_up_out(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, _| this.drop_call_card()),
            )
            .children(stage)
            .children(face)
            .when(call.waits_for_answer() && !answerable, |this| {
                this.child(
                    div()
                        .px_4()
                        .pt_2()
                        .text_center()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.text_muted)
                        .child("Video calls cannot be answered here. Answer it on the phone."),
                )
            })
            .child(
                div()
                    .w_full()
                    .px_4()
                    .py_4()
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .when(on, |this| {
                        this.child(
                            switch(
                                "call-mute",
                                if muted {
                                    "Turn the microphone on"
                                } else {
                                    "Turn the microphone off"
                                },
                                if muted {
                                    IconName::MicOff
                                } else {
                                    IconName::Mic
                                },
                                muted,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.engine.mute_call(&mute_id, !muted);
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .when(on, |this| {
                        this.child(
                            switch(
                                "call-add",
                                "Add somebody to the call",
                                IconName::UserPlus,
                                adding,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.toggle_adding(&add_id, window, cx);
                                },
                            )),
                        )
                    })
                    .when(on && call.event.video, |this| {
                        this.child(
                            switch(
                                "call-camera",
                                if camera_off {
                                    "Turn the camera on"
                                } else {
                                    "Turn the camera off"
                                },
                                if camera_off {
                                    IconName::VideoOff
                                } else {
                                    IconName::Video
                                },
                                camera_off,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.engine.set_call_camera(&camera_id, camera_off);
                                    cx.notify();
                                },
                            )),
                        )
                    })
                    .child(
                        round_button(
                            "call-end",
                            if call.waits_for_answer() {
                                "Decline"
                            } else if on {
                                "Hang up"
                            } else {
                                "Cancel the call"
                            },
                            IconName::PhoneOff,
                            palette.danger,
                            gpui_kit::white(),
                            palette,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.engine.end_call(&end_id);
                            cx.notify();
                        })),
                    )
                    .when(answerable, |this| {
                        this.child(
                            round_button(
                                "call-accept",
                                "Answer",
                                if call.event.video {
                                    IconName::Video
                                } else {
                                    IconName::Phone
                                },
                                palette.accent_fill,
                                palette.on_accent_fill,
                                palette,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.engine.accept_call(&accept_id);
                                    cx.notify();
                                },
                            )),
                        )
                    }),
            )
            .children(add_panel)
            // The corner the card is made larger and smaller by.
            .child(
                div()
                    .id(SharedString::from(format!("call-size-{id}")))
                    .debug_selector(|| "call-size".into())
                    .absolute()
                    .right_0()
                    .bottom_0()
                    .size(px(20.))
                    .cursor_nwse_resize()
                    .flex()
                    .items_end()
                    .justify_end()
                    .p(px(5.))
                    .child(
                        div()
                            .size(px(8.))
                            .border_r_2()
                            .border_b_2()
                            .border_color(palette.text_faint)
                            .rounded_br(px(3.)),
                    )
                    .on_mouse_down(
                        gpui_kit::MouseButton::Left,
                        cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            let width = this.card_width();
                            this.call_deck.sizing =
                                Some((event.position, width, this.call_deck.moved.x));
                        }),
                    ),
            )
            .into_any_element()
    }

    /// The search for somebody to add to a call, under its switches.
    fn call_add_panel(
        &self,
        call: &LiveCall,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let people = self.people_to_add(call, cx);
        let search = self
            .call_deck
            .adding
            .as_ref()
            .map(|adding| adding.search.clone());
        let id = call.event.call_id.clone();
        let hover = palette.hover;
        let mut list = div().flex().flex_col();
        for (index, (who, name)) in people.iter().enumerate() {
            let name = name.clone();
            let (call_id, who) = (id.clone(), who.clone());
            list = list.child(
                div()
                    .id(("call-add-person", index))
                    .debug_selector(move || format!("call-add-{index}"))
                    .h(px(36.))
                    .px_2()
                    .rounded(px(6.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(avatar_or(None, &name, AvatarKind::Person, px(24.), palette))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_SMALL())
                            .child(name.clone()),
                    )
                    .child(icon(IconName::Plus, px(14.), palette.icon))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.engine.invite_to_call(&call_id, &who);
                        this.call_deck.adding = None;
                        cx.notify();
                    })),
            );
        }
        div()
            .debug_selector(|| "call-add-panel".into())
            .w_full()
            .px_3()
            .pb_4()
            .flex()
            .flex_col()
            .gap_1()
            // Typing here is not taking hold of the card.
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation()
            })
            .children(search.map(|search| {
                super::numbers::input_box("call-add-search", &search, false, palette)
            }))
            .child(list)
            .when(people.is_empty(), |this| {
                this.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .child("Nobody to add by that name."),
                )
            })
            .into_any_element()
    }

    /// The calls that ring or are on, and why the last one did not
    /// happen: in the window's corner, above everything.
    pub(super) fn render_calls(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let calls = self.engine.calls();
        let problem = self.engine.call_problem();
        if calls.is_empty() && problem.is_none() {
            return None;
        }
        let cards: Vec<AnyElement> = calls
            .iter()
            .map(|call| self.call_card(call, palette, cx))
            .collect();
        Some(
            div()
                .debug_selector(|| "call-cards".into())
                .absolute()
                .top(metrics::HEADER_HEIGHT() + px(12.) + self.call_deck.moved.y)
                .right(CARD_MARGIN() - self.call_deck.moved.x)
                .flex()
                .flex_col()
                .gap_2()
                .children(cards)
                .children(problem.map(|problem| {
                    div()
                        .debug_selector(|| "call-problem".into())
                        .w(self.card_width())
                        .px_4()
                        .py_3()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(palette.elevated_border)
                        .bg(palette.elevated)
                        .shadow_lg()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .child(format!("The call did not go through. {problem}"))
                }))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_provider::{AccountId, CallEvent, ChatId, ContactId, Timestamp};

    fn call(state: CallState, video: bool, muted: bool) -> LiveCall {
        LiveCall {
            event: CallEvent {
                account_id: AccountId::new("a"),
                call_id: "c".into(),
                chat_id: ChatId::new("+1"),
                peer: ContactId::new("+1"),
                video,
                state,
                at: Timestamp::from_millis(0),
            },
            connected_at: None,
            muted,
            camera_off: false,
        }
    }

    #[test]
    fn a_call_says_where_it_stands() {
        let ringing = CallState::Ringing {
            incoming: true,
            group: false,
        };
        assert_eq!(
            call_words(&call(ringing.clone(), false, false)),
            "Incoming voice call"
        );
        assert_eq!(
            call_words(&call(ringing, true, false)),
            "Incoming video call"
        );
        let calling = CallState::Ringing {
            incoming: false,
            group: false,
        };
        assert_eq!(call_words(&call(calling, false, false)), "Calling…");
        assert_eq!(
            call_words(&call(CallState::Connected, false, true)),
            "On a call, microphone off"
        );
        assert_eq!(
            call_words(&call(CallState::Connecting, false, false)),
            "Connecting…"
        );
    }
}
