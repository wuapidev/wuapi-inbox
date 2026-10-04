//! What a bubble shows for the messages that are neither plain text nor a
//! file: a place, contact cards, a poll, a calendar event, a link's
//! preview, a view-once file, a type nobody models; the centred line of a
//! notice; and message text with its formatting, links and mentions.
//!
//! Nothing here reaches the network by being drawn. A map, a browser or a
//! calendar opens on a click, through the operating system.

use super::bubble::{RowContext, Side};
use super::media::MediaVisual;
use super::shell::Shell;
use super::widgets::{avatar, mono, AvatarKind};
use crate::calendar;
use crate::icons::{icon, IconName};
use crate::markup::{self, Block, Handle, Line};
use crate::theme::{fonts, hairline, metrics, px, Palette};
use client_core::{system_line, StoreChange};
use client_provider::{
    CalendarEvent, ContactCard, ContactId, DeliveryStatus, EventCall, GeoPoint, LinkPreview,
    Location, Media, MediaKind, Message, MessageId, Poll, SystemEvent, Timestamp,
};
use gpui_kit::prelude::*;
use gpui_kit::{
    div, img, relative, AnyElement, ClipboardItem, Context, Div, FontStyle, FontWeight,
    HighlightStyle, InteractiveText, ObjectFit, SharedString, StrikethroughStyle, StyledImage,
    StyledText, UnderlineStyle,
};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

/// How many cards of a message with several are listed before "View all".
const CARDS_SHOWN: usize = 3;

/// How long "Copied" stands in for "Copy number".
const COPIED_SHOWN: Duration = Duration::from_secs(2);

/// What the tiles remember between frames.
#[derive(Clone, Default)]
pub struct TileState {
    /// The messages whose contact cards are all listed.
    expanded: Rc<RefCell<HashSet<String>>>,
    /// The number that was just copied, as `message id/card index`.
    copied: Rc<RefCell<Option<String>>>,
}

/// What a row needs to know to draw its tile.
#[derive(Clone, Default)]
pub struct TileContext {
    state: TileState,
    /// The provider takes votes.
    can_vote: bool,
    /// A chat can be opened with a number or a contact.
    can_message: bool,
}

impl TileContext {
    /// The number that was just copied, as `message id/card index`.
    #[cfg(test)]
    pub(super) fn copied_key(&self) -> Option<String> {
        self.state.copied.borrow().clone()
    }
}

impl Shell {
    /// What the tiles of this frame are drawn with.
    pub(super) fn tile_context(&self) -> TileContext {
        let capabilities = self.engine.capabilities();
        TileContext {
            state: self.tiles.clone(),
            can_vote: capabilities.poll_votes,
            can_message: capabilities.start_chat || capabilities.contacts,
        }
    }

    /// Says that something did not work, where problems are said.
    fn say(&self, message: impl Into<String>) {
        self.engine.store().notify(StoreChange::Problem {
            message: message.into(),
        });
    }

    /// Hands a link to the system's browser. Only http(s) ever is.
    pub(super) fn open_link(&mut self, address: &str, cx: &mut Context<Self>) {
        if markup::opens_in_browser(address) {
            cx.open_url(address);
        }
    }

    /// Shows a place in the system's browser, on OpenStreetMap.
    pub(super) fn open_map(&mut self, point: GeoPoint, cx: &mut Context<Self>) {
        cx.open_url(&map_url(point));
    }

    /// Lists every card of a message, or only the first few again.
    pub(super) fn toggle_cards(&mut self, message: &MessageId, cx: &mut Context<Self>) {
        {
            let mut expanded = self.tiles.expanded.borrow_mut();
            if !expanded.remove(message.as_str()) {
                expanded.insert(message.to_string());
            }
        }
        // The row changed height.
        if let Some(open) = &self.open {
            open.list.remeasure();
        }
        cx.notify();
    }

    /// Puts a card's number on the clipboard, and says so for a moment.
    pub(super) fn copy_number(&mut self, key: String, phone: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(phone.to_owned()));
        *self.tiles.copied.borrow_mut() = Some(key.clone());
        let copied = self.tiles.copied.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_SHOWN).await;
            if copied.borrow().as_ref() == Some(&key) {
                *copied.borrow_mut() = None;
                this.update(cx, |_, cx| cx.notify()).ok();
            }
        })
        .detach();
        cx.notify();
    }

    /// Opens the conversation with the number of a contact card: the chat
    /// of the contact it belongs to when the address book has it, else
    /// the chat with the number, once the provider has said the number
    /// has WhatsApp. Nothing is sent.
    pub(super) fn message_card(&mut self, phone: String, cx: &mut Context<Self>) {
        let Some(account) = self.account.clone() else {
            return;
        };
        let digits = |text: &str| -> String { text.chars().filter(char::is_ascii_digit).collect() };
        let wanted = digits(&phone);
        if wanted.is_empty() {
            return;
        }
        let known = self
            .engine
            .store()
            .contacts(&account, Some(&phone), 8)
            .unwrap_or_default()
            .into_iter()
            .find(|contact| contact.phone.as_deref().map(digits).as_ref() == Some(&wanted));
        if let Some(contact) = known {
            match self.engine.chat_with_contact(&contact) {
                Ok(chat) => self.show_chat(chat, None, cx),
                Err(error) => self.say(format!("The chat could not be opened: {error}")),
            }
            return cx.notify();
        }
        if !self.engine.capabilities().start_chat {
            return self.say("This provider cannot start a chat with a new number.");
        }
        let engine = self.engine.clone();
        let asked = phone.clone();
        let work = self
            .engine
            .runtime()
            .spawn(async move { engine.start_chat_checked(&account, &asked).await });
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            this.update(cx, |this, cx| {
                match outcome {
                    Ok(Ok(client_core::NewChat::Open(chat))) => this.show_chat(chat, None, cx),
                    Ok(Ok(client_core::NewChat::NotOnWhatsApp(phone))) => {
                        this.say(format!("{phone} is not on WhatsApp."));
                    }
                    Ok(Err(error)) if error.is_transient() => {
                        this.say("Could not reach the provider. Try again.");
                    }
                    Ok(Err(error)) => this.say(format!("The chat could not be opened: {error}")),
                    Err(error) => this.say(format!("The chat could not be opened: {error}")),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// A click on a poll's option: the vote it asks for goes to the
    /// engine, which shows it at once and takes it back if it is refused.
    pub(super) fn vote(
        &mut self,
        message: &Message,
        poll: &Poll,
        option: &str,
        cx: &mut Context<Self>,
    ) {
        match poll.toggled(option) {
            Some(choices) => {
                self.engine
                    .vote_poll(&message.account_id, &message.chat_id, &message.id, choices)
            }
            None => self.say(format!(
                "This poll takes {} answers at most. Take one back first.",
                poll.max_choices
            )),
        }
        cx.notify();
    }

    /// Writes the event as an `.ics` file in the private directory of
    /// opened files and hands it to the system's calendar. Only ever on
    /// the user's click.
    pub(super) fn add_to_calendar(
        &mut self,
        event: &CalendarEvent,
        message: &MessageId,
        cx: &mut Context<Self>,
    ) {
        match export_event(&super::media::exports_dir(), event, message) {
            Ok(path) => cx.open_with_system(&path),
            Err(error) => self.say(format!("The event could not be added: {error}")),
        }
    }
}

/// Writes an event's `.ics` file under `exports` (the private directory
/// of files opened with other applications) and returns its path.
pub(super) fn export_event(
    exports: &std::path::Path,
    event: &CalendarEvent,
    message: &MessageId,
) -> Result<std::path::PathBuf, String> {
    let file = calendar::ics(event, message.as_str(), Timestamp::now())
        .ok_or("the event has no start time")?;
    let dir = exports.join("events");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let private = std::fs::Permissions::from_mode(0o700);
        let _ = std::fs::set_permissions(exports, private.clone());
        let _ = std::fs::set_permissions(&dir, private);
    }
    // Named after the message, in characters that are only a file name.
    let name: String = message
        .as_str()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(48)
        .collect();
    let path = dir.join(format!("event-{name}.ics"));
    std::fs::write(&path, file).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(path)
}

/// Where "Open in maps" leads: OpenStreetMap, with a marker on the place.
pub(super) fn map_url(point: GeoPoint) -> String {
    let (latitude, longitude) = (point.latitude(), point.longitude());
    format!(
        "https://www.openstreetmap.org/?mlat={latitude:.6}&mlon={longitude:.6}\
         #map=16/{latitude:.6}/{longitude:.6}"
    )
}

/// Coordinates as a person reads them: `38.70693, -9.14577`.
fn coordinates(point: GeoPoint) -> String {
    format!("{:.5}, {:.5}", point.latitude(), point.longitude())
}

/// The width of a tile's content, the same as a file's tile.
#[allow(non_snake_case)]
fn TILE_WIDTH() -> gpui_kit::Pixels {
    px(300.)
}

/// The block every tile is drawn on.
fn tile(selector: &'static str, side: &Side) -> Div {
    div()
        .debug_selector(move || selector.into())
        .w(TILE_WIDTH())
        .max_w_full()
        .p(px(10.))
        .rounded(px(4.))
        .bg(side.quote)
        .flex()
        .flex_col()
}

/// An icon and, beside it, a title over its details.
fn heading(glyph: IconName, title: Div, details: Vec<Div>, side: &Side) -> Div {
    div()
        .flex()
        .items_start()
        .gap_3()
        .child(
            div()
                .debug_selector(|| "tile-icon".into())
                .flex_none()
                .pt(px(1.))
                .child(icon(glyph, px(22.), side.meta)),
        )
        .child(
            div()
                .debug_selector(|| "tile-label".into())
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .child(title)
                .children(details),
        )
}

/// A secondary line of a tile: small, in the meta colour, wrapping.
fn detail(text: impl Into<SharedString>, side: &Side) -> Div {
    div()
        .min_w_0()
        .text_size(metrics::TEXT_SMALL())
        .line_height(px(18.))
        .text_color(side.meta)
        .child(text.into())
}

/// A clickable line under a tile: an icon and a label in the mono face.
fn action(
    id: String,
    selector: &'static str,
    glyph: IconName,
    text: impl Into<SharedString>,
    side: &Side,
) -> gpui_kit::Stateful<Div> {
    let colour = side.text;
    div()
        .id(SharedString::from(id))
        .debug_selector(move || selector.into())
        .flex_none()
        .flex()
        .items_center()
        .gap(px(6.))
        .cursor_pointer()
        .text_color(colour)
        .hover(|style| style.opacity(0.7))
        .child(icon(glyph, px(13.), colour))
        .child(mono(text))
}

/// The row of actions under a tile's content.
fn actions() -> Div {
    div()
        .pt(px(8.))
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(14.))
        .gap_y(px(6.))
}

// ----- location -------------------------------------------------------------

/// A place: its name and address when it has them, its coordinates, and
/// "Open in maps". No map is drawn: that would fetch tiles from a third
/// party just because the chat was opened.
pub(super) fn location(
    place: &Location,
    message: &Message,
    side: &Side,
    context: &RowContext,
) -> Div {
    let fallback = if place.live {
        "Live location"
    } else {
        "Location"
    };
    let title = div()
        .min_w_0()
        .font_weight(FontWeight::MEDIUM)
        .child(SharedString::from(
            place.name.clone().unwrap_or_else(|| fallback.to_owned()),
        ));
    let mut details = Vec::new();
    if let Some(address) = &place.address {
        details.push(detail(address.clone(), side));
    }
    if place.live && place.name.is_some() {
        details.push(detail("Live location", side));
    }
    details.push(
        div()
            .debug_selector(|| "location-coordinates".into())
            .pt(px(2.))
            .text_color(side.meta)
            .child(mono(coordinates(place.point)).line_height(px(16.))),
    );
    let (view, point) = (context.view.clone(), place.point);
    tile("location-tile", side)
        .child(heading(IconName::MapPin, title, details, side))
        .child(
            actions().child(
                action(
                    format!("map-{}", message.id),
                    "location-open",
                    IconName::ExternalLink,
                    "OPEN IN MAPS",
                    side,
                )
                .on_click(move |_, _, cx| {
                    view.update(cx, |shell, cx| shell.open_map(point, cx)).ok();
                }),
            ),
        )
}

// ----- contact cards --------------------------------------------------------

/// One card or several: the name, the numbers, the initials, and for each
/// number "Message" and "Copy number". Several are a compact list of the
/// first few with "View all".
pub(super) fn contacts(
    cards: &[ContactCard],
    message: &Message,
    side: &Side,
    palette: &Palette,
    context: &RowContext,
) -> Div {
    let expanded = context
        .tiles
        .state
        .expanded
        .borrow()
        .contains(message.id.as_str());
    let shown = if expanded || cards.len() <= CARDS_SHOWN {
        cards.len()
    } else {
        CARDS_SHOWN
    };
    let single = cards.len() == 1;
    let mut block = tile("contact-tile", side).gap(px(8.));
    for (index, card) in cards.iter().take(shown).enumerate() {
        block = block.child(contact_card(
            card, index, single, message, side, palette, context,
        ));
    }
    if cards.len() > CARDS_SHOWN {
        let (view, id) = (context.view.clone(), message.id.clone());
        let text = if expanded {
            "SHOW FEWER".to_owned()
        } else {
            format!("VIEW ALL · {}", cards.len())
        };
        block = block.child(
            div().flex().child(
                action(
                    format!("cards-{}", message.id),
                    "contacts-view-all",
                    if expanded {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    },
                    text,
                    side,
                )
                .on_click(move |_, _, cx| {
                    view.update(cx, |shell, cx| shell.toggle_cards(&id, cx))
                        .ok();
                }),
            ),
        );
    }
    block
}

fn contact_card(
    card: &ContactCard,
    index: usize,
    single: bool,
    message: &Message,
    side: &Side,
    palette: &Palette,
    context: &RowContext,
) -> Div {
    let name = if card.name.trim().is_empty() {
        card.phones
            .first()
            .cloned()
            .unwrap_or_else(|| "Contact".to_owned())
    } else {
        card.name.clone()
    };
    let size = if single {
        metrics::AVATAR_LARGE()
    } else {
        metrics::AVATAR_MEDIUM()
    };
    let numbers = card.phones.iter().map(|phone| {
        div()
            .text_color(side.meta)
            .child(mono(phone.clone()).line_height(px(16.)))
    });
    let who = div()
        .debug_selector(|| "contact-row".into())
        .flex()
        .items_center()
        .gap_3()
        .child(
            div()
                .debug_selector(|| "contact-avatar".into())
                .flex_none()
                .child(avatar(&name, AvatarKind::Person, size, palette)),
        )
        .child(
            div()
                .debug_selector(|| "contact-label".into())
                .min_w_0()
                .flex_1()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::MEDIUM)
                        .child(SharedString::from(name)),
                )
                .children(numbers),
        );
    let Some(phone) = card.phones.first().cloned() else {
        return who;
    };
    let key = format!("{}/{index}", message.id);
    let copied = context.tiles.state.copied.borrow().as_ref() == Some(&key);
    let mut row = actions().pt(px(6.));
    if context.tiles.can_message {
        let (view, phone) = (context.view.clone(), phone.clone());
        row = row.child(
            action(
                format!("message-{key}"),
                "contact-message",
                IconName::MessageCircle,
                "MESSAGE",
                side,
            )
            .on_click(move |_, _, cx| {
                view.update(cx, |shell, cx| shell.message_card(phone.clone(), cx))
                    .ok();
            }),
        );
    }
    let view = context.view.clone();
    row = row.child(
        action(
            format!("copy-{key}"),
            "contact-copy",
            if copied {
                IconName::Check
            } else {
                IconName::Copy
            },
            if copied { "COPIED" } else { "COPY NUMBER" },
            side,
        )
        .on_click(move |_, _, cx| {
            view.update(cx, |shell, cx| shell.copy_number(key.clone(), &phone, cx))
                .ok();
        }),
    );
    div().flex().flex_col().child(who).child(row)
}

// ----- poll -----------------------------------------------------------------

/// A poll: the question, how many answers it takes, each option with its
/// bar and count and whether the account chose it, and how many voted.
pub(super) fn poll(poll: &Poll, message: &Message, side: &Side, context: &RowContext) -> Div {
    // A poll still on its way out, or that never went, cannot be voted
    // on: not while it waits here, and not while the provider has it and
    // WhatsApp does not yet. The same rule as for answering or reacting
    // to a message.
    let votable = context.tiles.can_vote
        && !message.id.as_str().starts_with("local:")
        && !matches!(
            message.status,
            DeliveryStatus::Pending | DeliveryStatus::Accepted | DeliveryStatus::Failed { .. }
        );
    let rule: SharedString = match poll.max_choices {
        1 => "SELECT ONE".into(),
        0 => "SELECT ONE OR MORE".into(),
        many => format!("SELECT UP TO {many}").into(),
    };
    // A full bar is everybody who voted.
    let scale = poll.voters.max(1) as f32;
    let mut block = tile("poll-tile", side).gap(px(8.)).child(
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .debug_selector(|| "poll-question".into())
                    .min_w_0()
                    .font_weight(FontWeight::MEDIUM)
                    .child(SharedString::from(poll.question.clone())),
            )
            .child(
                div()
                    .debug_selector(|| "poll-rule".into())
                    .pt(px(2.))
                    .text_color(side.meta)
                    .child(mono(rule).line_height(px(16.))),
            ),
    );
    for (index, option) in poll.options.iter().enumerate() {
        let chosen = poll.is_chosen(&option.name);
        let share = (option.votes as f32 / scale).clamp(0., 1.);
        let mark = match (poll.multiple_choice(), chosen) {
            (true, true) => IconName::SquareCheck,
            (true, false) => IconName::Square,
            (false, true) => IconName::CircleCheck,
            (false, false) => IconName::Circle,
        };
        let row = div()
            .id(SharedString::from(format!("option-{}-{index}", message.id)))
            .debug_selector(|| "poll-option".into())
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .debug_selector(move || {
                                if chosen {
                                    "poll-chosen".into()
                                } else {
                                    "poll-mark".into()
                                }
                            })
                            .flex_none()
                            .child(icon(
                                mark,
                                px(15.),
                                if chosen { side.signal } else { side.meta },
                            )),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .child(SharedString::from(option.name.clone())),
                    )
                    .child(
                        div()
                            .debug_selector(|| "poll-count".into())
                            .text_color(side.meta)
                            .child(mono(option.votes.to_string())),
                    ),
            )
            .child(
                div()
                    .debug_selector(|| "poll-bar".into())
                    .w_full()
                    .h(px(4.))
                    .rounded_full()
                    .bg(side.meta.opacity(0.22))
                    .child(
                        div()
                            .h_full()
                            .w(relative(share))
                            .rounded_full()
                            .bg(if chosen { side.signal } else { side.meta }),
                    ),
            );
        block = block.child(if votable {
            let (view, message, poll, name) = (
                context.view.clone(),
                message.clone(),
                poll.clone(),
                option.name.clone(),
            );
            row.cursor_pointer()
                .hover(|style| style.opacity(0.8))
                .on_click(move |_, _, cx| {
                    view.update(cx, |shell, cx| shell.vote(&message, &poll, &name, cx))
                        .ok();
                })
        } else {
            row
        });
    }
    let voters: SharedString = match poll.voters {
        0 => "NO VOTES YET".into(),
        1 => "1 VOTER".into(),
        many => format!("{many} VOTERS").into(),
    };
    block.child(
        div()
            .debug_selector(|| "poll-voters".into())
            .text_color(side.meta)
            .child(mono(voters)),
    )
}

// ----- calendar event -------------------------------------------------------

/// A calendar event: its title, when it is (in this computer's timezone),
/// where, what it is about, and "Add to calendar".
pub(super) fn event(
    event: &CalendarEvent,
    message: &Message,
    side: &Side,
    palette: &Palette,
    context: &RowContext,
) -> Div {
    let title = div()
        .min_w_0()
        .font_weight(FontWeight::MEDIUM)
        .when(event.cancelled, |this| this.line_through())
        .child(SharedString::from(event.title.clone()));
    let mut details = Vec::new();
    if event.cancelled {
        details.push(
            div()
                .debug_selector(|| "event-cancelled".into())
                .text_color(palette.danger)
                .child(mono("CANCELLED").line_height(px(16.))),
        );
    }
    let when = calendar::time_range(event);
    if let Some(when) = &when {
        details.push(
            div()
                .debug_selector(|| "event-time".into())
                .min_w_0()
                .child(detail(when.clone(), side).text_color(side.text)),
        );
    }
    if let Some(place) = &event.place {
        let named: Vec<&str> = [&place.name, &place.address]
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        if !named.is_empty() {
            details.push(detail(named.join(" · "), side));
        } else if let Some(point) = place.point {
            details.push(detail(coordinates(point), side));
        }
    }
    if let Some(call) = event.call {
        details.push(detail(
            match call {
                EventCall::Voice => "Voice call",
                EventCall::Video => "Video call",
            },
            side,
        ));
    }
    if let Some(description) = &event.description {
        details.push(detail(description.clone(), side).pt(px(4.)));
    }
    let mut row = actions();
    let mut any = false;
    if when.is_some() && !event.cancelled {
        let (view, event, id) = (context.view.clone(), event.clone(), message.id.clone());
        row = row.child(
            action(
                format!("calendar-{}", message.id),
                "event-add",
                IconName::CalendarPlus,
                "ADD TO CALENDAR",
                side,
            )
            .on_click(move |_, _, cx| {
                view.update(cx, |shell, cx| shell.add_to_calendar(&event, &id, cx))
                    .ok();
            }),
        );
        any = true;
    }
    if let Some(point) = event.place.as_ref().and_then(|place| place.point) {
        let view = context.view.clone();
        row = row.child(
            action(
                format!("event-map-{}", message.id),
                "event-map",
                IconName::MapPin,
                "OPEN IN MAPS",
                side,
            )
            .on_click(move |_, _, cx| {
                view.update(cx, |shell, cx| shell.open_map(point, cx)).ok();
            }),
        );
        any = true;
    }
    if let Some(url) = event
        .join_url
        .clone()
        .filter(|url| markup::opens_in_browser(url) && !event.cancelled)
    {
        let view = context.view.clone();
        row = row.child(
            action(
                format!("event-join-{}", message.id),
                "event-join",
                IconName::ExternalLink,
                "JOIN",
                side,
            )
            .on_click(move |_, _, cx| {
                view.update(cx, |shell, cx| shell.open_link(&url, cx)).ok();
            }),
        );
        any = true;
    }
    tile("event-tile", side)
        .child(heading(IconName::Calendar, title, details, side))
        .when(any, |this| this.child(row))
}

// ----- link preview ---------------------------------------------------------

/// A link's preview, from what the message carries: title, description, a
/// small picture when it brought one, and the host, which opens the link.
/// The page itself is never fetched.
pub(super) fn link_card(
    link: &LinkPreview,
    message: &Message,
    side: &Side,
    context: &RowContext,
) -> Div {
    let host = markup::host(&link.url);
    // The picture is one of the message's own files: fetched and cached
    // like its images, by the same rules.
    let picture = link.thumbnail.as_ref().and_then(|source| {
        let mut media = Media::new(MediaKind::Image);
        media.source = Some(source.clone());
        match context.shelf.visual(&message.account_id, &media) {
            MediaVisual::Image(image) => Some(image),
            _ => None,
        }
    });
    let mut text = div().min_w_0().flex_1().flex().flex_col();
    if let Some(title) = link.title.as_ref().filter(|title| !title.trim().is_empty()) {
        text = text.child(
            div()
                .debug_selector(|| "link-title".into())
                .line_clamp(2)
                .font_weight(FontWeight::MEDIUM)
                .child(SharedString::from(title.clone())),
        );
    }
    if let Some(description) = link
        .description
        .as_ref()
        .filter(|description| !description.trim().is_empty())
    {
        text = text.child(detail(description.replace('\n', " "), side).line_clamp(2));
    }
    text = text.child(match host {
        Some(host) => {
            let (view, url) = (context.view.clone(), link.url.clone());
            div().pt(px(3.)).flex().child(
                action(
                    format!("link-{}", message.id),
                    "link-host",
                    IconName::ExternalLink,
                    host.to_uppercase(),
                    side,
                )
                .on_click(move |_, _, cx| {
                    view.update(cx, |shell, cx| shell.open_link(&url, cx)).ok();
                }),
            )
        }
        // Not an address a browser opens: shown, not clickable.
        None => div()
            .pt(px(3.))
            .text_color(side.meta)
            .child(mono(link.url.clone())),
    });
    div()
        .debug_selector(|| "link-card".into())
        .mb(px(5.))
        .w(TILE_WIDTH())
        .max_w_full()
        .p(px(8.))
        .rounded(px(4.))
        .bg(side.quote)
        .flex()
        .items_start()
        .gap_3()
        .when_some(picture, |this, picture| {
            this.child(
                div()
                    .debug_selector(|| "link-picture".into())
                    .flex_none()
                    .size(px(52.))
                    .rounded(px(4.))
                    .overflow_hidden()
                    .child(
                        img(picture)
                            .size_full()
                            .object_fit(ObjectFit::Cover)
                            .rounded(px(4.)),
                    ),
            )
        })
        .child(text)
}

// ----- view once, unsupported, notices --------------------------------------

/// A file that can be opened once, on the phone. It is never downloaded
/// here, and nothing about it is shown beyond its kind.
pub(super) fn view_once(kind: Option<MediaKind>, side: &Side) -> Div {
    let what = match kind {
        Some(MediaKind::Image) => "View once photo",
        Some(MediaKind::Video) => "View once video",
        Some(MediaKind::Voice | MediaKind::Audio) => "View once voice message",
        _ => "View once message",
    };
    tile("view-once-tile", side).child(heading(
        IconName::Timer,
        div().min_w_0().font_weight(FontWeight::MEDIUM).child(what),
        vec![div()
            .text_color(side.meta)
            .child(mono("OPEN IT ON YOUR PHONE").line_height(px(16.)))],
        side,
    ))
}

/// A message of a type this client does not draw: said plainly, with the
/// type's name as the provider gave it.
pub(super) fn unsupported(description: &str, side: &Side) -> Div {
    let name = description.trim();
    let mut details = Vec::new();
    if !name.is_empty() {
        details.push(
            div()
                .debug_selector(|| "unsupported-type".into())
                .min_w_0()
                .text_color(side.meta)
                .truncate()
                .font_family(fonts::MONO)
                .text_size(metrics::TEXT_META())
                .line_height(px(16.))
                .child(SharedString::from(name.to_uppercase())),
        );
    }
    tile("unsupported-tile", side).child(heading(
        IconName::Info,
        div()
            .min_w_0()
            .font_weight(FontWeight::MEDIUM)
            .child("Unsupported message"),
        details,
        side,
    ))
}

/// A notice from the chat itself, as a centred line on the page.
pub(super) fn notice_line(event: &SystemEvent, gutter: gpui_kit::Pixels, palette: &Palette) -> Div {
    div()
        .w_full()
        .flex()
        .justify_center()
        .px(gutter)
        .pt(px(8.))
        .pb(px(2.))
        .child(
            div()
                .debug_selector(|| "system-line".into())
                .max_w_full()
                // Of the family of the day's pill, one size down: a small
                // surface of its own on the wallpaper.
                .px(px(11.))
                .py(px(2.))
                .rounded(metrics::PILL() / 2.)
                .border_1()
                .border_color(palette.border)
                .bg(palette.surface)
                .text_center()
                .text_size(metrics::TEXT_META() + px(0.5))
                .line_height(px(17.))
                .text_color(palette.text_muted)
                .child(SharedString::from(system_line(event))),
        )
}

/// "Forwarded", above what a forwarded message carries, or WhatsApp's
/// "Forwarded many times" (two arrows) from the fifth forward on.
pub(super) fn forwarded_label(side: &Side, many: bool) -> Div {
    div()
        .debug_selector(|| "forwarded-label".into())
        .mb(px(2.))
        .flex()
        .items_center()
        .gap(px(5.))
        .italic()
        .text_size(metrics::TEXT_SMALL())
        .line_height(px(18.))
        .text_color(side.meta)
        .child(icon(IconName::Forward, px(12.), side.meta))
        .when(many, |this| {
            this.child(
                div()
                    .debug_selector(|| "forwarded-many".into())
                    .ml(px(-7.))
                    .child(icon(IconName::Forward, px(12.), side.meta)),
            )
        })
        .child(if many {
            "Forwarded many times"
        } else {
            "Forwarded"
        })
}

/// The star of a starred message, for the line with the time.
pub(super) fn starred_mark(side: &Side) -> Div {
    div()
        .debug_selector(|| "starred-mark".into())
        .flex_none()
        .child(icon(IconName::Star, px(11.), side.meta))
}

// ----- text -----------------------------------------------------------------

/// The size of a message made of a few emoji and nothing else: (text
/// size, line height), or `None` for ordinary text.
fn emoji_size(text: &str) -> Option<(gpui_kit::Pixels, gpui_kit::Pixels)> {
    match markup::emoji_only(text)? {
        1..=3 => Some((px(36.), px(46.))),
        4..=8 => Some((px(24.), px(32.))),
        _ => None,
    }
}

/// What a click on a stretch of text does.
enum TextClick {
    /// Opens the address in the browser.
    Link(String),
    /// Opens the profile of the contact that is mentioned.
    Profile(ContactId),
}

/// How strong the wash behind the account's own mention is.
const MENTION_ME_WASH: f32 = 0.16;

/// A piece of a message's text as something that can be selected and
/// copied: `shown` is what `inner` draws, `copy` what a stretch of it is
/// copied as.
#[allow(clippy::too_many_arguments)]
fn selectable(
    id: String,
    block: usize,
    shown: SharedString,
    layout: gpui_kit::TextLayout,
    inner: impl IntoElement,
    copy: super::select_text::CopyText,
    message: &Message,
    side: &Side,
    context: &RowContext,
) -> AnyElement {
    super::select_text::Selectable::new(SharedString::from(id), shown.clone(), layout, inner)
        .order(context.row.get() * 256 + block as u64)
        .colour(side.select)
        .whole(context.whole.as_ref() == Some(&message.id))
        .marks(
            super::select_text::occurrences(&shown, context.find.as_deref()),
            context.palette.find_mark,
        )
        .pressed(context.text_pressed.clone())
        .copy(copy)
        .into_any_element()
}

/// One stretch of text with its styles, links and mentions. `prefix` is
/// what the block starts with as it was typed (`> `, `- `), for a copy
/// that takes the block from its start.
fn styled(
    id: String,
    block: usize,
    prefix: &str,
    line: &Line,
    message: &Message,
    side: &Side,
    context: &RowContext,
) -> AnyElement {
    let view = &context.view;
    let mut highlights = Vec::new();
    let mut families = Vec::new();
    let mut clicks = Vec::new();
    for span in &line.spans {
        let style = &span.style;
        let signal = style.link.is_some() || style.mention;
        // Somebody mentioned is named in their own colour, the one their
        // messages carry; the account's own mention keeps the bubble's
        // accent and its wash, so "@You" stands apart.
        let person = style
            .mention_of
            .filter(|_| style.mention && !style.mention_me)
            .and_then(|who| message.extras.mentions.get(who))
            .map(|mention| {
                let tone = context
                    .senders
                    .of(&message.account_id, Some(&message.chat_id), &mention.id)
                    .tone;
                side.senders[tone % crate::theme::SENDER_TONES]
            });
        highlights.push((
            span.range.clone(),
            HighlightStyle {
                color: person.or(signal.then_some(side.signal)),
                font_weight: (style.bold || style.mention).then_some(FontWeight::SEMIBOLD),
                font_style: style.italic.then_some(FontStyle::Italic),
                // The account's own mention stands on a wash of the
                // bubble's accent, as WhatsApp marks "you".
                background_color: if style.mention_me {
                    Some(side.signal.opacity(MENTION_ME_WASH))
                } else {
                    style.mono.then_some(side.quote)
                },
                underline: style.link.as_ref().map(|_| UnderlineStyle {
                    thickness: hairline(),
                    color: Some(side.signal),
                    wavy: false,
                }),
                strikethrough: style.strike.then_some(StrikethroughStyle {
                    thickness: hairline(),
                    color: None,
                }),
                fade_out: None,
            },
        ));
        if style.mono {
            families.push((span.range.clone(), SharedString::from(fonts::MONO)));
        }
        if let Some(address) = &style.link {
            clicks.push((span.range.clone(), TextClick::Link(address.clone())));
        } else if let Some(mention) = style
            .mention_of
            .and_then(|who| message.extras.mentions.get(who))
        {
            clicks.push((span.range.clone(), TextClick::Profile(mention.id.clone())));
        }
    }
    let shown = SharedString::from(line.text.clone());
    let text = StyledText::new(shown.clone())
        .with_highlights(highlights)
        .with_font_family_overrides(families);
    let layout = text.layout().clone();
    let copy: super::select_text::CopyText = {
        let (line, prefix) = (line.clone(), prefix.to_owned());
        Rc::new(move |range: std::ops::Range<usize>| {
            let copied = line.copied(range.clone());
            if range.start == 0 {
                format!("{prefix}{copied}")
            } else {
                copied
            }
        })
    };
    if clicks.is_empty() {
        return selectable(id, block, shown, layout, text, copy, message, side, context);
    }
    let (ranges, clicks): (Vec<_>, Vec<_>) = clicks.into_iter().unzip();
    let view = view.clone();
    let account = message.account_id.clone();
    let inner = InteractiveText::new(SharedString::from(format!("{id}-links")), text).on_click(
        ranges,
        move |index, window, cx| {
            // The end of a drag that selected text is not a click.
            if gpui_kit::base::TextSelection::has_selection(window, cx) {
                return;
            }
            view.update(cx, |shell, cx| match clicks.get(index) {
                Some(TextClick::Link(address)) => shell.open_link(address, cx),
                Some(TextClick::Profile(contact)) => {
                    shell.open_contact_profile(account.clone(), contact.clone(), window, cx)
                }
                None => {}
            })
            .ok();
        },
    );
    selectable(
        id, block, shown, layout, inner, copy, message, side, context,
    )
}

/// Text drawn as it is (a few large emoji, a block of code), selectable
/// like the rest.
fn plain(
    id: String,
    block: usize,
    text: &str,
    message: &Message,
    side: &Side,
    context: &RowContext,
) -> AnyElement {
    let shown = SharedString::from(text.to_owned());
    let drawn = StyledText::new(shown.clone());
    let layout = drawn.layout().clone();
    let source = shown.clone();
    let copy: super::select_text::CopyText =
        Rc::new(move |range: std::ops::Range<usize>| source.get(range).unwrap_or("").to_owned());
    selectable(
        id, block, shown, layout, drawn, copy, message, side, context,
    )
}

/// Message text: WhatsApp's markup drawn, links clickable, mentions by
/// name, and a few emoji on their own enlarged.
pub(super) fn rich_text(body: &str, message: &Message, side: &Side, context: &RowContext) -> Div {
    if let Some((size, height)) = emoji_size(body) {
        return div()
            .debug_selector(|| "emoji-text".into())
            .min_w_0()
            .text_size(size)
            .line_height(height)
            .child(plain(
                format!("text-{}-emoji", message.id),
                0,
                body.trim(),
                message,
                side,
                context,
            ));
    }
    let mentions: Vec<Handle> = message
        .extras
        .mentions
        .iter()
        .enumerate()
        .map(|(who, mention)| Handle {
            handle: mention.handle.clone(),
            name: mention.name.clone(),
            me: mention.me,
            who,
        })
        .collect();
    let blocks = markup::parse(body, &mentions);
    let mut column = div()
        .debug_selector(|| "message-text".into())
        .min_w_0()
        .flex()
        .flex_col();
    let marker = |text: SharedString| {
        div()
            .flex_none()
            .min_w(px(14.))
            .text_color(side.meta)
            .child(text)
    };
    for (index, block) in blocks.iter().enumerate() {
        let id = format!("text-{}-{index}", message.id);
        column = column.child(match block {
            Block::Paragraph(line) => div()
                .min_w_0()
                .child(styled(id, index, "", line, message, side, context)),
            Block::Quote(line) => div()
                .debug_selector(|| "text-quote".into())
                .my(px(2.))
                .flex()
                .gap(px(8.))
                .child(div().flex_none().w(px(2.)).rounded_full().bg(side.meta))
                .child(
                    div()
                        .min_w_0()
                        .text_color(side.meta)
                        .child(styled(id, index, "> ", line, message, side, context)),
                ),
            Block::Bullet(line) => div()
                .debug_selector(|| "text-bullet".into())
                .flex()
                .gap(px(4.))
                .child(marker("•".into()))
                .child(
                    div()
                        .min_w_0()
                        .child(styled(id, index, "- ", line, message, side, context)),
                ),
            Block::Numbered(number, line) => div()
                .flex()
                .gap(px(4.))
                .child(marker(number.clone().into()))
                .child(div().min_w_0().child(styled(
                    id,
                    index,
                    &format!("{number} "),
                    line,
                    message,
                    side,
                    context,
                ))),
            Block::Code(code) => div()
                .debug_selector(|| "text-code".into())
                .my(px(3.))
                .min_w_0()
                .px(px(8.))
                .py(px(5.))
                .rounded(px(4.))
                .bg(side.quote)
                .font_family(fonts::MONO)
                .text_size(metrics::TEXT_SMALL())
                .child(plain(id, index, code, message, side, context)),
        });
    }
    column
}
