//! Rendering of one row of a conversation: a day separator or a message
//! bubble with its quote, media, reactions, time and ticks.

use super::media::{FileState, MediaShelf, MediaVisual};
use super::pills;
use super::senders::{self, Quoted, Sender};
use super::shell::{AudioView, Shell};
use super::tiles::{self, TileContext};
use super::transfer::{transfer_button, Transfer};
use super::widgets::{mono, status_tick};
use crate::animation::Motion;
use crate::format::{clock, duration, file_size};
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette, SENDER_TONES};
use client_core::StoredMessage;
use client_provider::{
    DeliveryStatus, Direction, Media, MediaKind, Message, MessageContent, ReplyRef,
};
use gpui_kit::prelude::*;
use gpui_kit::{div, img, Div, FontWeight, Hsla, ObjectFit, SharedString, StyledImage, WeakEntity};
use std::rc::Rc;

/// One row of the conversation list.
pub enum Row {
    /// A "Today" / "Yesterday" / date pill.
    Day(SharedString),
    /// A message.
    Message(Box<MessageRow>),
}

/// A message, plus what its position in the thread implies.
pub struct MessageRow {
    /// The message and its reactions.
    pub stored: StoredMessage,
    /// First of a run of messages by the same sender: gets extra space
    /// above and, in groups, the sender's name.
    pub first_of_run: bool,
    /// Last of a run: gets the tail, a squared bottom corner on the
    /// sender's side, the way the logo's bubble has one.
    pub last_of_run: bool,
    /// Who it is from, for an incoming message: their colour, and how
    /// their name is written.
    pub sender: Option<Sender>,
    /// Whose message it quotes, when it quotes one.
    pub quoted: Quoted,
    /// The first message that was unread when the chat was opened, and how
    /// many there were: "3 unread messages" stands above it, until the
    /// chat is left.
    pub unread_above: Option<u32>,
    /// Identity of the row across reloads.
    pub key: String,
    /// Changes whenever the row's size may have changed.
    pub signature: u64,
}

/// Colours that depend on which side a bubble is on.
pub(super) struct Side {
    pub(super) outgoing: bool,
    pub(super) bubble: Hsla,
    pub(super) outline: Hsla,
    /// Message text.
    pub(super) text: Hsla,
    /// Time, ticks and secondary text.
    pub(super) meta: Hsla,
    /// Fill of a quote or attachment block.
    pub(super) quote: Hsla,
    /// What stands out on this bubble: links, mentions, the chosen option
    /// of a poll. The accent for this bubble's fill.
    pub(super) signal: Hsla,
    /// The colours people are named in on this bubble.
    pub(super) senders: [Hsla; SENDER_TONES],
    /// The wash under selected text.
    pub(super) select: Hsla,
    /// The play button of a voice note, and the glyph on it.
    pub(super) play: Hsla,
    pub(super) on_play: Hsla,
}

impl Side {
    fn of(outgoing: bool, palette: &Palette) -> Self {
        if outgoing {
            Self {
                outgoing,
                bubble: palette.bubble_out,
                outline: palette.bubble_out_border,
                text: palette.on_bubble_out,
                meta: palette.meta_out,
                quote: palette.quote_out,
                signal: palette.signal_out,
                senders: palette.sender_out,
                select: palette.selection_out,
                play: palette.play_out,
                on_play: palette.on_play_out,
            }
        } else {
            Self {
                outgoing,
                bubble: palette.bubble_in,
                outline: palette.bubble_in_border,
                text: palette.text,
                meta: palette.meta_in,
                quote: palette.quote_in,
                signal: palette.accent,
                senders: palette.sender,
                select: palette.selection_in,
                play: palette.play_in,
                on_play: palette.on_play_in,
            }
        }
    }
}

/// What a row needs besides itself: where its media comes from, and the
/// view its clicks go to.
pub struct RowContext {
    /// The colours in use.
    pub palette: Palette,
    /// Nothing turns or slides: reduced motion.
    pub still: bool,
    /// What stickers and GIFs that move may do right now: run, hold
    /// their frame (the window is in the background, something lies over
    /// the conversation), or stand at one moment.
    pub motion: Motion,
    /// The engine, for what a bubble shows of a send under way.
    pub engine: client_core::SyncEngine,
    /// Pictures and media.
    pub shelf: Rc<MediaShelf>,
    /// Who people are, for the colour somebody mentioned is named in.
    pub senders: Rc<senders::Senders>,
    /// Which row of the list is being drawn: the order of its text among
    /// everything selectable in the window.
    pub row: std::cell::Cell<u64>,
    /// The message whose whole text shows as selected (Ctrl+A on the
    /// focused message).
    pub whole: Option<client_provider::MessageId>,
    /// The row key of the message the keyboard is on.
    pub focused: Option<Rc<str>>,
    /// Raised by a press that lands on a message's text: such a press
    /// starts a selection, it does not give the message the keyboard.
    pub text_pressed: Rc<std::cell::Cell<bool>>,
    /// The row that is lit for a moment: where a jump landed.
    pub flash: Option<Rc<str>>,
    /// What the search in the conversation looks for, in lower case: its
    /// occurrences are marked in the text.
    pub find: Option<Rc<str>>,
    /// The rows picked while messages are being selected; `None` when
    /// they are not.
    pub selecting: Option<Rc<std::collections::BTreeSet<String>>>,
    /// The shell.
    pub view: WeakEntity<Shell>,
    /// What the audio player is doing, as of this frame.
    pub audio: AudioView,
    /// What the tiles of places, cards, polls and events are drawn with.
    pub tiles: TileContext,
}

/// Renders a row.
pub fn render_row(row: &Row, palette: &Palette, in_group: bool, context: &RowContext) -> Div {
    match row {
        Row::Day(label) => pills::day(label.clone(), palette),
        Row::Message(message) => match message.unread_above {
            // The divider belongs to the message it stands above: one row
            // of the list, so the two are never apart.
            Some(count) => div()
                .w_full()
                .flex()
                .flex_col()
                .child(pills::unread(count, THREAD_GUTTER(), palette))
                .child(message_row(message, palette, in_group, context)),
            None => message_row(message, palette, in_group, context),
        },
    }
}

/// The room left of a row's bubbles: the pane's gutter, or in a group a
/// narrower one, since the senders' avatars stand in it.
fn leading_gutter(in_group: bool) -> gpui_kit::Pixels {
    if in_group {
        px(14.)
    } else {
        THREAD_GUTTER()
    }
}

/// The corners of a bubble: round, tighter where it meets the next bubble
/// of its run on the sender's side, and square at the foot of the run:
/// the tail, as the logo's bubble has one.
fn shaped(bubble: Div, outgoing: bool, first_of_run: bool, last_of_run: bool) -> Div {
    let (corner, joint) = (metrics::BUBBLE_CORNER(), metrics::BUBBLE_JOINT());
    let top = if first_of_run { corner } else { joint };
    let bottom = if last_of_run { px(0.) } else { joint };
    let bubble = bubble.rounded(corner);
    if outgoing {
        bubble.rounded_tr(top).rounded_br(bottom)
    } else {
        bubble.rounded_tl(top).rounded_bl(bottom)
    }
}

/// Space between the pane's edges and the bubbles.
#[allow(non_snake_case)]
fn THREAD_GUTTER() -> gpui_kit::Pixels {
    px(40.)
}

fn message_row(row: &MessageRow, palette: &Palette, in_group: bool, context: &RowContext) -> Div {
    let message = &row.stored.message;
    let outgoing = message.direction == Direction::Outgoing;
    let side = Side::of(outgoing, palette);
    let failed = outgoing && matches!(message.status, DeliveryStatus::Failed { .. });
    let extras = &message.extras;

    // A notice from the chat itself is a line on the page, not a bubble.
    if let (MessageContent::System(event), false) = (&message.content, message.deleted) {
        return tiles::notice_line(event, THREAD_GUTTER(), palette);
    }
    // In a group, the bubbles of the others stand beside their avatar.
    let beside_avatar = in_group && !outgoing;

    // A sticker stands on its own, as in WhatsApp: no bubble behind it,
    // only the time and the tick below.
    if let (MessageContent::Media(media), false) = (&message.content, message.deleted) {
        if media.kind == MediaKind::Sticker && !extras.view_once {
            return sticker_row(row, media, palette, in_group, context);
        }
    }

    let bubble = div()
        .flex()
        .flex_col()
        .min_w(px(72.))
        // Never wider than the column, which shrinks with a narrow pane:
        // without this a long line is laid out at its full width and runs
        // out of the pane instead of wrapping.
        .max_w_full()
        .debug_selector(|| "bubble".into())
        .px(px(12.))
        .pt(px(7.))
        .pb(px(7.))
        .bg(side.bubble)
        .border_1()
        .border_color(if failed { palette.danger } else { side.outline })
        .text_size(metrics::TEXT_BODY())
        .line_height(metrics::LINE_BODY())
        .text_color(side.text);
    let mut bubble = with_keyboard(
        shaped(bubble, outgoing, row.first_of_run, row.last_of_run),
        row,
        context,
    );

    // Who it is from, above what they said: on the first bubble of their
    // run, in their colour.
    if beside_avatar && row.first_of_run {
        if let (Some(name), Some(sender)) = (&message.sender_name, &row.sender) {
            bubble = bubble.child(senders::name_line(message, name, sender, &side, context));
        }
    }

    if extras.forwarded && !message.deleted {
        bubble = bubble.child(tiles::forwarded_label(&side, extras.forwarded_many));
    }

    if let Some(reply) = &message.reply_to {
        if !message.deleted {
            // An answer to a status says which one, and shows it while it
            // is there; anything else quotes a message.
            bubble = match &extras.story_reply {
                Some(story) => bubble.child(super::story_quote::story_quote(
                    story, message, &side, context,
                )),
                None => bubble.child(quote(reply, row.quoted, message, &side, context)),
            };
        }
    }

    let meta = meta_line(row, &side, palette);
    bubble = if message.deleted {
        bubble.child(with_trailing_meta(
            notice("This message was deleted", &side),
            meta,
        ))
    } else if extras.view_once {
        // Never downloaded, never previewed: it opens once, on the phone.
        let kind = match &message.content {
            MessageContent::Media(media) => Some(media.kind),
            _ => None,
        };
        under_tile(bubble, tiles::view_once(kind, &side), meta)
    } else {
        match &message.content {
            MessageContent::Text { body } => {
                let bubble = match &extras.link {
                    Some(link) => bubble.child(tiles::link_card(link, message, &side, context)),
                    None => bubble,
                };
                bubble.child(with_trailing_meta(
                    tiles::rich_text(body, message, &side, context),
                    meta,
                ))
            }
            // Audio brings its own bottom row: the length on the left, the
            // time and tick on the right, on one baseline.
            MessageContent::Media(media)
                if matches!(media.kind, MediaKind::Voice | MediaKind::Audio) =>
            {
                bubble.child(super::voice::audio_block(
                    media, message, &side, palette, context, meta,
                ))
            }
            MessageContent::Media(media) => {
                let caption = media.caption.clone().filter(|caption| !caption.is_empty());
                let mut attachment = media_block(media, message, &side, context);
                // A file of the user's own that has not gone yet: how far
                // it is, and the way to take it back.
                if let Some(line) = sending_line(message, &side, context) {
                    attachment = div().flex().flex_col().child(attachment).child(line);
                }
                match caption {
                    Some(caption) => {
                        bubble
                            .child(attachment)
                            .child(div().pt_1().child(with_trailing_meta(
                                tiles::rich_text(&caption, message, &side, context),
                                meta,
                            )))
                    }
                    None => under_tile(bubble, attachment, meta),
                }
            }
            MessageContent::Location(place) => under_tile(
                bubble,
                tiles::location(place, message, &side, context),
                meta,
            ),
            MessageContent::Contacts { cards } => under_tile(
                bubble,
                tiles::contacts(cards, message, &side, palette, context),
                meta,
            ),
            MessageContent::Poll(poll) => {
                under_tile(bubble, tiles::poll(poll, message, &side, context), meta)
            }
            MessageContent::Event(event) => under_tile(
                bubble,
                tiles::event(event, message, &side, palette, context),
                meta,
            ),
            MessageContent::Unsupported { description } => {
                under_tile(bubble, tiles::unsupported(description, &side), meta)
            }
            // Drawn as a line on the page, above.
            MessageContent::System(event) => bubble.child(with_trailing_meta(
                notice(&client_core::system_line(event), &side),
                meta,
            )),
            // Reactions never reach the list; the store folds them.
            MessageContent::Reaction { emoji, .. } => {
                bubble.child(SharedString::from(emoji.clone()))
            }
        }
    };

    // Last, so that it is over all of the above.
    let bubble = bubble.child(menu_arrow(row, context));

    let has_reactions = !row.stored.reactions.is_empty();
    let failure = match &message.status {
        DeliveryStatus::Failed { reason } if outgoing => Some(reason.clone()),
        _ => None,
    };

    let column = div()
        .flex()
        .flex_col()
        .min_w_0()
        .max_w(metrics::BUBBLE_MAX_WIDTH())
        .map(|this| {
            if outgoing {
                this.items_end()
            } else {
                this.items_start()
            }
        })
        .child(bubble)
        .when(has_reactions, |this| {
            this.child(reaction_chips(row, context))
        })
        .when_some(failure, |this, reason| {
            this.child(
                mono(if reason.is_empty() {
                    "Not sent".to_owned()
                } else {
                    format!("Not sent · {reason}")
                })
                .pt(px(4.))
                .text_color(palette.danger),
            )
        });

    row_marks(row, context)
        .debug_selector(|| format!("row-{}", message.id))
        .w_full()
        .flex()
        .pl(leading_gutter(in_group))
        .pr(THREAD_GUTTER())
        .pt(run_gap(row.first_of_run))
        .map(|this| {
            if outgoing {
                this.justify_end()
            } else {
                this.justify_start()
            }
        })
        .when_some(
            row.sender.as_ref().filter(|_| beside_avatar),
            |this, sender| {
                this.child(senders::run_avatar(
                    message,
                    sender,
                    row.last_of_run,
                    context,
                ))
            },
        )
        .child(column)
}

/// The row of a message, with what marks it as a whole: the wash of a
/// row a jump just landed on, and while messages are being selected, the
/// box that says whether this one is picked. Both are painted over the
/// row's own room and take none.
fn row_marks(row: &MessageRow, context: &RowContext) -> Div {
    let palette = &context.palette;
    let lit = context.flash.as_deref() == Some(row.key.as_str());
    let picked = context
        .selecting
        .as_ref()
        .map(|picked| picked.contains(&row.key));
    div()
        .relative()
        .when(lit || picked == Some(true), |this| {
            this.child(
                div()
                    .debug_selector(move || {
                        if lit {
                            "message-flash".into()
                        } else {
                            "message-picked".into()
                        }
                    })
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(palette.focus_ring.opacity(if lit { 0.16 } else { 0.1 })),
            )
        })
        .when_some(picked, |this, picked| {
            let (view, key) = (context.view.clone(), row.key.clone());
            this.child(
                div()
                    .id(SharedString::from(format!("pick-{}", row.key)))
                    .debug_selector(|| "message-check".into())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right(px(10.))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation()
                    })
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        view.update(cx, |shell, cx| shell.toggle_row(key.clone(), window, cx))
                            .ok();
                    })
                    .child(icon(
                        if picked {
                            IconName::SquareCheck
                        } else {
                            IconName::Square
                        },
                        px(16.),
                        if picked {
                            palette.focus_ring
                        } else {
                            palette.text_faint
                        },
                    )),
            )
        })
}

/// What makes a bubble (or a sticker) part of the keyboard's world: a
/// press on its empty room gives it the keyboard, a right-click opens its
/// menu (as does its arrow, [`menu_arrow`], which goes on last), and while
/// the keyboard is on it an outline says so. The outline is drawn outside
/// the box, on the page, so it reads on every bubble style and moves
/// nothing.
fn with_keyboard(shape: Div, row: &MessageRow, context: &RowContext) -> Div {
    let focused = context.focused.as_deref() == Some(row.key.as_str());
    let group = SharedString::from(format!("message-{}", row.key));
    let (view, key, pressed) = (
        context.view.clone(),
        row.key.clone(),
        context.text_pressed.clone(),
    );
    let (menu_view, menu_key) = (context.view.clone(), row.key.clone());
    let palette = &context.palette;
    // No further out than the room between two bubbles of a run: the
    // next bubble is painted after this one, and would cover it.
    let out = -metrics::FOCUS_OUT();
    shape
        .relative()
        .group(group)
        .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx| {
            // On the text, a press is the start of a selection.
            if pressed.replace(false) {
                return;
            }
            view.update(cx, |shell, cx| {
                // While messages are being selected, a press picks.
                if shell.acting.selecting.is_some() {
                    shell.toggle_row(key.clone(), window, cx)
                } else {
                    shell.focus_message(key.clone(), window, cx)
                }
            })
            .ok();
        })
        .on_mouse_down(
            gpui_kit::MouseButton::Right,
            move |event: &gpui_kit::MouseDownEvent, window, cx| {
                cx.stop_propagation();
                menu_view
                    .update(cx, |shell, cx| {
                        shell.open_message_menu(menu_key.clone(), Some(event.position), window, cx)
                    })
                    .ok();
            },
        )
        // For the tests: the bounds of this message's bubble, by its id.
        .when(cfg!(test), |this| {
            let id = row.stored.message.id.to_string();
            this.child(
                div()
                    .debug_selector(move || format!("bubble-{id}"))
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
        })
        .when(focused, |this| {
            this.child(
                div()
                    .debug_selector(|| "message-focus".into())
                    .absolute()
                    .top(out)
                    .left(out)
                    .right(out)
                    .bottom(out)
                    .rounded(metrics::BUBBLE_CORNER() + metrics::FOCUS_OUT())
                    .border_2()
                    .border_color(palette.focus_ring),
            )
        })
}

/// The arrow that opens a message's menu: on the top right corner of the
/// bubble (or sticker), there only while the pointer is on it. It is the
/// last thing added to the bubble, after everything the bubble holds: what
/// is added later is painted over what came before and takes its clicks,
/// so a quote, a picture, a link's card or the sender's name under that
/// corner neither covers the arrow nor takes the press meant for it. Its
/// own fill keeps it readable on any of them.
fn menu_arrow(row: &MessageRow, context: &RowContext) -> gpui_kit::Stateful<Div> {
    let group = SharedString::from(format!("message-{}", row.key));
    let (view, key) = (context.view.clone(), row.key.clone());
    let palette = &context.palette;
    div()
        .id(SharedString::from(format!("message-arrow-{}", row.key)))
        .debug_selector(|| "message-arrow".into())
        .absolute()
        .top(px(3.))
        .right(px(3.))
        .size(px(20.))
        .rounded_full()
        .bg(palette.surface)
        .border_1()
        .border_color(palette.border)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .invisible()
        .group_hover(group, |style| style.visible())
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation()
        })
        .on_click(move |event, window, cx| {
            cx.stop_propagation();
            let at = event.position();
            view.update(cx, |shell, cx| {
                shell.open_message_menu(key.clone(), Some(at), window, cx)
            })
            .ok();
        })
        .child(icon(IconName::ChevronDown, px(13.), palette.icon))
}

/// The room above a bubble: more where a run begins.
fn run_gap(first_of_run: bool) -> gpui_kit::Pixels {
    if first_of_run {
        metrics::RUN_GAP()
    } else {
        metrics::RUN_JOINT()
    }
}

/// A tile and, under it on the right, the time and ticks.
fn under_tile(bubble: Div, tile: Div, meta: Div) -> Div {
    bubble
        .child(tile)
        .child(div().pt_1().flex().justify_end().child(meta))
}

/// Body followed by the time and ticks: on the same line when they fit,
/// on a line of their own (right-aligned) when they do not.
fn with_trailing_meta(body: Div, meta: Div) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_end()
        .gap_x_2()
        .min_w_0()
        .max_w_full()
        .child(body.max_w_full())
        .child(meta.ml_auto())
}

/// Time, "Edited" and the delivery tick, in the mono face.
fn meta_line(row: &MessageRow, side: &Side, palette: &Palette) -> Div {
    let message = &row.stored.message;
    div()
        .flex_none()
        .h(px(16.))
        .flex()
        .items_center()
        .gap(px(4.))
        .line_height(px(16.))
        .text_color(side.meta)
        .when(message.extras.starred && !message.deleted, |this| {
            this.child(tiles::starred_mark(side))
        })
        .when(message.edited && !message.deleted, |this| {
            this.child(
                mono("edited")
                    .debug_selector(|| "edited-label".into())
                    .text_size(px(10.5)),
            )
        })
        .child(mono(clock(message.timestamp)).text_size(px(10.5)))
        .when(side.outgoing && !message.deleted, |this| {
            this.child(status_tick(
                &message.status,
                side.meta,
                palette.tick_read,
                palette,
            ))
        })
}

fn notice(text: &str, side: &Side) -> Div {
    div()
        .min_w_0()
        .flex()
        .items_center()
        .gap(px(6.))
        .italic()
        .text_color(side.meta)
        .child(icon(IconName::Ban, px(14.), side.meta))
        .child(SharedString::from(text.to_owned()))
}

/// The message being replied to: a block with a bar on its leading edge.
/// The bar and the name are in the colour of whoever wrote it: theirs, or
/// the bubble's accent for the account's own.
fn quote(
    reply: &ReplyRef,
    quoted: Quoted,
    message: &Message,
    side: &Side,
    context: &RowContext,
) -> gpui_kit::Stateful<Div> {
    let name: SharedString = match &reply.sender_name {
        Some(name) => name.clone().into(),
        None => "You".into(),
    };
    let preview: SharedString = reply
        .preview
        .clone()
        .unwrap_or_else(|| "Message".to_owned())
        .into();
    let (bar, ink) = match quoted {
        Quoted::Person(tone) => {
            let colour = side.senders[tone % SENDER_TONES];
            (colour, colour)
        }
        Quoted::Me => (side.signal, side.signal),
        Quoted::Unknown => (side.meta, side.text),
    };
    // A click goes to the message that is quoted, and lights it.
    let (view, target) = (context.view.clone(), reply.message_id.clone());
    div()
        .id(SharedString::from(format!("quote-{}", message.id)))
        .debug_selector(|| "quote".into())
        .cursor_pointer()
        .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
            cx.stop_propagation()
        })
        .on_click(move |_, _, cx| {
            cx.stop_propagation();
            view.update(cx, |shell, cx| shell.jump_and_flash(target.clone(), cx))
                .ok();
        })
        .mb(px(5.))
        .flex()
        .rounded(px(6.))
        .overflow_hidden()
        .bg(side.quote)
        .child(div().flex_none().w(px(3.)).bg(bar))
        .child(
            div()
                .min_w_0()
                .px(px(9.))
                .py(px(5.))
                .flex()
                .flex_col()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(18.))
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(ink)
                        .child(name),
                )
                .child(
                    div()
                        .max_w(px(420.))
                        .truncate()
                        .text_color(side.meta)
                        .child(preview),
                ),
        )
}

/// A placeholder for an attachment. The payload itself is not downloaded
/// or decoded yet.
/// A sticker and, under it, when it was sent.
fn sticker_row(
    row: &MessageRow,
    media: &Media,
    palette: &Palette,
    in_group: bool,
    context: &RowContext,
) -> Div {
    let message = &row.stored.message;
    let outgoing = message.direction == Direction::Outgoing;
    // On the page, not on a bubble: the page's own muted text.
    let side = Side {
        outgoing,
        bubble: palette.background,
        outline: palette.background,
        text: palette.text,
        meta: palette.text_muted,
        quote: palette.muted,
        signal: palette.accent,
        senders: palette.sender,
        select: palette.selection_in,
        play: palette.play_in,
        on_play: palette.on_play_in,
    };
    let column = div()
        .debug_selector(|| "sticker".into())
        .flex()
        .flex_col()
        .gap(px(2.))
        .map(|this| {
            if outgoing {
                this.items_end()
            } else {
                this.items_start()
            }
        })
        .child(image_block(media, message, &side, context, false))
        .child(meta_line(row, &side, palette))
        .when(!row.stored.reactions.is_empty(), |this| {
            this.child(reaction_chips(row, context))
        });
    let column = with_keyboard(column, row, context).child(menu_arrow(row, context));
    let beside_avatar = in_group && !outgoing;
    row_marks(row, context)
        .debug_selector(|| format!("row-{}", message.id))
        .w_full()
        .flex()
        .pl(leading_gutter(in_group))
        .pr(THREAD_GUTTER())
        .pt(run_gap(row.first_of_run))
        .map(|this| {
            if outgoing {
                this.justify_end()
            } else {
                this.justify_start()
            }
        })
        .when_some(
            row.sender.as_ref().filter(|_| beside_avatar),
            |this, sender| {
                this.child(senders::run_avatar(
                    message,
                    sender,
                    row.last_of_run,
                    context,
                ))
            },
        )
        .child(column)
}

/// An image or a sticker in its box. The box has its size before the image
/// is there and keeps it after, so the list never moves under the reader.
fn image_block(
    media: &Media,
    message: &Message,
    side: &Side,
    context: &RowContext,
    framed: bool,
) -> Div {
    let size = context.shelf.image_box(media);
    let visual = context.shelf.visual(&message.account_id, media);
    let frame = div()
        .id(SharedString::from(format!("media-{}", message.id)))
        .debug_selector(|| "media-box".into())
        .flex_none()
        .w(size.width)
        .h(size.height)
        .max_w_full()
        .rounded(px(4.))
        .overflow_hidden()
        .when(framed, |this| this.bg(side.quote))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .text_color(side.meta);
    let note = |glyph: IconName, text: &'static str, detail: Option<SharedString>| {
        let frame = div()
            .size_full()
            .when(!framed, |this| this.rounded(px(4.)).bg(side.quote))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2()
            .px_3()
            .child(icon(glyph, px(24.), side.meta))
            .child(mono(text));
        match detail {
            Some(detail) => frame.child(
                div()
                    .text_center()
                    .text_size(metrics::TEXT_META())
                    .line_height(px(15.))
                    .child(detail),
            ),
            None => frame,
        }
    };
    let element = match visual {
        MediaVisual::Image(image) => {
            let moving = moving_picture(media, message, context);
            let media_for_buttons = media;
            let (view, media) = (context.view.clone(), media.clone());
            let group = SharedString::from(format!("picture-{}", message.id));
            frame
                .relative()
                .group(group)
                .cursor_pointer()
                .on_click(move |_, window, cx| {
                    view.update(cx, |shell, cx| {
                        shell.begin_viewing(media.clone(), window, cx)
                    })
                    .ok();
                })
                .child(picture_buttons(media_for_buttons, message, context))
                .child(
                    div()
                        .debug_selector(|| "media-image".into())
                        .size_full()
                        .child(match moving {
                            Some(moving) => moving,
                            None => img(image)
                                .size_full()
                                .object_fit(ObjectFit::Contain)
                                .rounded(px(4.))
                                .into_any_element(),
                        }),
                )
        }
        // Never to come: said, with no button to press.
        MediaVisual::Unavailable(reason) => frame.child(
            div()
                .size_full()
                .debug_selector(|| "media-unavailable".into())
                .child(note(IconName::Ban, "NO LONGER AVAILABLE", Some(reason))),
        ),
        // A kind of sticker nothing here can draw.
        MediaVisual::Unsupported(reason) => frame.child(
            div()
                .size_full()
                .debug_selector(|| "media-unavailable".into())
                .child(note(IconName::Ban, "NOT AVAILABLE", Some(reason))),
        ),
        // Imported with the account's history: the provider has no file.
        MediaVisual::NoFile => frame.child(
            div()
                .size_full()
                .debug_selector(|| "media-unavailable".into())
                .child(note(
                    IconName::Ban,
                    "NOT AVAILABLE",
                    Some("From before this number was linked".into()),
                )),
        ),
        // Not here yet: the round button, in the middle of the box the
        // picture will fill. It downloads, shows how far the download
        // is and stops it, or tries again.
        visual @ (MediaVisual::Loading { .. }
        | MediaVisual::Download { .. }
        | MediaVisual::Failed(_)) => {
            let (state, caption): (Transfer, Option<SharedString>) = match visual {
                MediaVisual::Loading { fraction } => (Transfer::Loading { fraction }, None),
                MediaVisual::Download { size } => (
                    Transfer::Download { size },
                    size.map(|size| file_size(size).into()),
                ),
                MediaVisual::Failed(reason) => (Transfer::Retry(reason), Some("NOT LOADED".into())),
                _ => unreachable!("matched above"),
            };
            let (view, shelf) = (context.view.clone(), context.shelf.clone());
            let account = message.account_id.clone();
            let url = media.source.as_ref().map(|source| source.to_string());
            let stop = matches!(state, Transfer::Loading { .. });
            frame.child(
                div()
                    .size_full()
                    .when(!framed, |this| this.rounded(px(4.)).bg(side.quote))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(transfer_button(
                        SharedString::from(format!("transfer-{}", message.id)),
                        &state,
                        &context.palette,
                        context.still,
                        move |_, _, cx| {
                            if let Some(url) = &url {
                                if stop {
                                    shelf.cancel(url);
                                } else {
                                    shelf.ask(&account, url);
                                }
                            }
                            view.update(cx, |_, cx| cx.notify()).ok();
                        },
                    ))
                    .children(
                        caption.map(|caption| mono(caption).debug_selector(|| "media-size".into())),
                    ),
            )
        }
    };
    div().max_w_full().child(element)
}

/// The buttons in the corner of a picture, there while the pointer is on
/// it or the keyboard is on one of them: save, copy, open in the viewer.
fn picture_buttons(media: &Media, message: &Message, context: &RowContext) -> Div {
    let palette = &context.palette;
    let group = SharedString::from(format!("picture-{}", message.id));
    let button = |name: &'static str, glyph: IconName, hint: &'static str| {
        let ring = palette.focus_ring;
        div()
            .id(SharedString::from(format!("{name}-{}", message.id)))
            .debug_selector(move || name.into())
            .size(px(26.))
            .rounded_full()
            .bg(palette.media_button)
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .tab_index(0)
            .focus_visible(move |style| style.border_color(ring))
            .hover(|style| style.opacity(0.85))
            .tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(hint).build(window, cx)
            })
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation()
            })
            .child(icon(glyph, px(14.), palette.on_media_button))
    };
    let (save_view, save_media) = (context.view.clone(), media.clone());
    let (copy_view, copy_media) = (context.view.clone(), media.clone());
    let (open_view, open_media) = (context.view.clone(), media.clone());
    // A sticker can be starred, and a GIF kept, from where it is.
    let keeping = match media.kind {
        MediaKind::Sticker => Some((
            "picture-favorite",
            IconName::Star,
            "Add to favorites",
            client_core::LibraryKind::Sticker,
        )),
        _ if super::library_ui::is_gif(media) => Some((
            "picture-save-gif",
            IconName::Film,
            "Save GIF",
            client_core::LibraryKind::Gif,
        )),
        _ => None,
    };
    let (keep_view, keep_media) = (context.view.clone(), media.clone());
    let sent = message.direction == Direction::Outgoing;
    div()
        .debug_selector(|| "picture-buttons".into())
        .absolute()
        .top(px(6.))
        .right(px(6.))
        .flex()
        .gap(px(4.))
        .invisible()
        .group_hover(group, |style| style.visible())
        .child(
            button("picture-save", IconName::ArrowDownToLine, "Save as…").on_click(
                move |_, _, cx| {
                    cx.stop_propagation();
                    save_view
                        .update(cx, |shell, cx| shell.save_as(Some(save_media.clone()), cx))
                        .ok();
                },
            ),
        )
        .child(
            button("picture-copy", IconName::Copy, "Copy image").on_click(move |_, _, cx| {
                cx.stop_propagation();
                copy_view
                    .update(cx, |shell, cx| shell.copy_media(copy_media.clone(), cx))
                    .ok();
            }),
        )
        .children(keeping.map(|(name, glyph, hint, kind)| {
            button(name, glyph, hint).on_click(move |_, _, cx| {
                cx.stop_propagation();
                keep_view
                    .update(cx, |shell, cx| {
                        shell.keep_message_file(keep_media.clone(), kind, sent, cx)
                    })
                    .ok();
            })
        }))
        .child(
            button("picture-open", IconName::Image, "Open").on_click(move |_, window, cx| {
                cx.stop_propagation();
                open_view
                    .update(cx, |shell, cx| {
                        shell.begin_viewing(open_media.clone(), window, cx)
                    })
                    .ok();
            }),
        )
}

/// A sticker or GIF that moves, once its frames are decoded: painted
/// frame by frame, fitted inside its box like the still picture it
/// replaces. `None` under reduced motion, for a picture that does not
/// move, and until the animation is ready: the first frame stays.
///
/// Painting is what keeps it going: each paint shows the frame that is
/// due and asks for one repaint when the next is. A row that is not
/// painted (scrolled away, another chat) asks for nothing.
fn moving_picture(
    media: &Media,
    message: &Message,
    context: &RowContext,
) -> Option<gpui_kit::AnyElement> {
    if context.still {
        return None;
    }
    let animation = context.shelf.animation(&message.account_id, media)?;
    let url = media.source.as_ref()?.to_string();
    let (shelf, view, motion) = (context.shelf.clone(), context.view.clone(), context.motion);
    let (width, height) = (animation.size.0 as f32, animation.size.1 as f32);
    Some(
        div()
            .debug_selector(|| "media-animation".into())
            .size_full()
            .child(
                gpui_kit::canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        let now = cx.background_executor().now();
                        // Rows just outside the viewport are painted too (the
                        // list keeps a margin ready): they hold their frame.
                        let seen = window.content_mask().bounds.intersects(&bounds);
                        let motion = if seen || matches!(motion, Motion::Frozen(_)) {
                            motion
                        } else {
                            Motion::Paused
                        };
                        let Some(playing) = shelf.play(&url, now, motion) else {
                            return;
                        };
                        // Contained in the box, centred, never enlarged past it.
                        let scale = (bounds.size.width.as_f32() / width)
                            .min(bounds.size.height.as_f32() / height);
                        let size = gpui_kit::size(
                            gpui_kit::px(width * scale),
                            gpui_kit::px(height * scale),
                        );
                        let fitted = gpui_kit::Bounds {
                            origin: gpui_kit::point(
                                bounds.origin.x + (bounds.size.width - size.width) / 2.,
                                bounds.origin.y + (bounds.size.height - size.height) / 2.,
                            ),
                            size,
                        };
                        let _ = window.paint_image(
                            fitted,
                            fitted,
                            gpui_kit::Corners::all(px(4.)),
                            playing.image,
                            playing.frame,
                            false,
                        );
                        if let Some(next) = playing.next {
                            shelf.wake_in(next, view, cx);
                        }
                    },
                )
                .size_full(),
            )
            .into_any_element(),
    )
}

/// The line under a file's tile that fetches and opens it.
/// Under a file that is still on its way out: the upload's progress, or
/// that it is waiting, and "Cancel".
fn sending_line(message: &Message, side: &Side, context: &RowContext) -> Option<Div> {
    if message.direction != Direction::Outgoing || message.status != DeliveryStatus::Pending {
        return None;
    }
    let client_id = message.client_id.clone()?;
    let text = match context.engine.upload_progress(&client_id) {
        Some((sent, total)) if total > 0 => {
            format!("UPLOADING {}%", (sent.min(total) * 100 / total))
        }
        _ => "WAITING TO SEND".to_owned(),
    };
    let view = context.view.clone();
    let colour = side.text;
    Some(
        div()
            .debug_selector(|| "sending-line".into())
            .pt(px(6.))
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .text_color(side.meta)
            .child(mono(text))
            .child(
                div()
                    .id(SharedString::from(format!("cancel-{}", message.id)))
                    .debug_selector(|| "sending-cancel".into())
                    .cursor_pointer()
                    .text_color(colour)
                    .hover(|style| style.opacity(0.7))
                    .on_click(move |_, _, cx| {
                        view.update(cx, |shell, cx| shell.cancel_send(client_id.clone(), cx))
                            .ok();
                    })
                    .child(mono("CANCEL")),
            ),
    )
}

/// What stands at the head of a file's tile: the round button while the
/// file is not here (download, progress and stop, retry), and the glyph
/// of its kind once it is, or when it never will be.
fn tile_glyph(
    kind: IconName,
    media: &Media,
    message: &Message,
    side: &Side,
    context: &RowContext,
) -> gpui_kit::AnyElement {
    let Some(url) = media.source.as_ref().map(|source| source.to_string()) else {
        return icon(kind, px(24.), side.meta).into_any_element();
    };
    let state = match context.shelf.file_state(&url) {
        FileState::Ready => return icon(kind, px(24.), side.meta).into_any_element(),
        FileState::Unavailable(_) if context.shelf.file_expired(&url) => {
            return icon(IconName::Ban, px(24.), side.meta).into_any_element()
        }
        // Being fetched ahead: "Everything", or a GIF with the pictures.
        FileState::NotFetched
            if context.shelf.fetches_everything() || context.shelf.fetches_gif_unasked(media) =>
        {
            Transfer::Loading { fraction: None }
        }
        FileState::NotFetched => Transfer::Download {
            size: media.size_bytes,
        },
        FileState::Loading => Transfer::Loading {
            fraction: context.shelf.file_fraction(&url, media),
        },
        FileState::Unavailable(reason) => Transfer::Retry(reason),
    };
    let (view, shelf) = (context.view.clone(), context.shelf.clone());
    let account = message.account_id.clone();
    let stop = matches!(state, Transfer::Loading { .. });
    // A GIF started by the policy stays stopped. "Everything" keeps the
    // old meaning of the button: cancel this attempt, and the next draw
    // may ask again.
    let hold = media.gif && !context.shelf.fetches_everything();
    transfer_button(
        SharedString::from(format!("transfer-file-{}", message.id)),
        &state,
        &context.palette,
        context.still,
        move |_, _, cx| {
            if stop {
                if hold {
                    shelf.decline_file(&url);
                } else {
                    shelf.cancel_file(&url);
                }
            } else {
                // The file only: opening it is the line below.
                shelf.retry_file(&account, &url);
            }
            view.update(cx, |_, cx| cx.notify()).ok();
        },
    )
    .into_any_element()
}

fn file_action(media: &Media, message: &Message, side: &Side, context: &RowContext) -> Div {
    let line = div()
        .pt(px(6.))
        .flex()
        .items_center()
        .gap(px(6.))
        .text_color(side.meta);
    let Some(source) = &media.source else {
        return line
            .debug_selector(|| "media-unavailable".into())
            .child(icon(IconName::Ban, px(13.), side.meta))
            .child(mono("NOT AVAILABLE · FROM BEFORE THIS NUMBER WAS LINKED"));
    };
    let state = context.shelf.file_state(source.as_str());
    if state == FileState::NotFetched
        && (context.shelf.fetches_everything() || context.shelf.fetches_gif_unasked(media))
    {
        // "Everything", or a GIF under the same rule as a picture.
        context.shelf.prefetch(&message.account_id, source.as_str());
    }
    let text = match &state {
        FileState::NotFetched => "DOWNLOAD AND OPEN".to_owned(),
        FileState::Loading => "DOWNLOADING…".to_owned(),
        FileState::Ready => "OPEN".to_owned(),
        FileState::Unavailable(_) if context.shelf.file_expired(source.as_str()) => {
            "NO LONGER AVAILABLE".to_owned()
        }
        FileState::Unavailable(reason) => format!("NOT LOADED · {}", reason.to_uppercase()),
    };
    if matches!(state, FileState::Unavailable(_) | FileState::Loading) {
        return line.child(mono(text));
    }
    let saved = media.clone();
    let (view, media) = (context.view.clone(), media.clone());
    let text_colour = side.text;
    line.child(
        div()
            .id(SharedString::from(format!("open-{}", message.id)))
            .debug_selector(|| "media-open".into())
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .text_color(text_colour)
            .hover(|style| style.opacity(0.7))
            .on_click(move |_, _, cx| {
                view.update(cx, |shell, cx| shell.open_file(media.clone(), cx))
                    .ok();
            })
            .child(icon(IconName::ExternalLink, px(13.), text_colour))
            .child(mono(text)),
    )
    // And beside it, the way to keep the file: a name and a place are
    // asked for.
    .child({
        let (view, media) = (context.view.clone(), saved);
        div()
            .id(SharedString::from(format!("save-{}", message.id)))
            .debug_selector(|| "media-save".into())
            .ml(px(8.))
            .flex()
            .items_center()
            .gap(px(6.))
            .cursor_pointer()
            .text_color(text_colour)
            .hover(|style| style.opacity(0.7))
            .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                cx.stop_propagation()
            })
            .on_click(move |_, _, cx| {
                view.update(cx, |shell, cx| shell.save_as(Some(media.clone()), cx))
                    .ok();
            })
            .child(icon(IconName::ArrowDownToLine, px(13.), text_colour))
            .child(mono("SAVE AS…"))
    })
}

fn media_block(media: &Media, message: &Message, side: &Side, context: &RowContext) -> Div {
    match media.kind {
        MediaKind::Image | MediaKind::Sticker => image_block(media, message, side, context, true),
        MediaKind::Video => {
            // A GIF on WhatsApp is a short, silent, looping video. It is
            // fetched with the pictures. It is not played here: a click
            // opens it like a video.
            let (kind, title) = if media.gif {
                ("GIF", "GIF")
            } else {
                ("VIDEO", "Video")
            };
            let detail: SharedString = match (media.duration_secs, media.size_bytes) {
                (Some(secs), Some(bytes)) => {
                    format!("{kind} · {} · {}", duration(secs), file_size(bytes)).into()
                }
                (Some(secs), None) => format!("{kind} · {}", duration(secs)).into(),
                (None, Some(bytes)) => format!("{kind} · {}", file_size(bytes)).into(),
                (None, None) => kind.into(),
            };
            div()
                .w(px(300.))
                .p(px(10.))
                .rounded(px(4.))
                .bg(side.quote)
                .flex()
                .flex_col()
                .child(
                    div()
                        .debug_selector(|| "tile-row".into())
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .debug_selector(|| "tile-icon".into())
                                .flex_none()
                                .child(tile_glyph(IconName::Video, media, message, side, context)),
                        )
                        .child(
                            div()
                                .debug_selector(|| "tile-label".into())
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child(
                                            div().min_w_0().truncate().child(SharedString::from(
                                                media
                                                    .file_name
                                                    .clone()
                                                    .unwrap_or_else(|| title.to_owned()),
                                            )),
                                        )
                                        .when(media.gif, |this| {
                                            this.child(
                                                mono("GIF")
                                                    .debug_selector(|| "gif-badge".into())
                                                    .px(px(5.))
                                                    .rounded(px(3.))
                                                    .border_1()
                                                    .border_color(side.meta)
                                                    .line_height(px(15.))
                                                    .text_color(side.meta),
                                            )
                                        }),
                                )
                                .child(mono(detail).line_height(px(16.)).text_color(side.meta)),
                        ),
                )
                .child(file_action(media, message, side, context))
        }
        // Drawn by `audio_block`, which `message_row` calls directly.
        MediaKind::Voice | MediaKind::Audio => div(),
        MediaKind::Document => {
            let name: SharedString = media
                .file_name
                .clone()
                .unwrap_or_else(|| "Document".to_owned())
                .into();
            let kind = media
                .file_name
                .as_deref()
                .and_then(|name| name.rsplit_once('.'))
                .map(|(_, extension)| extension.to_uppercase())
                .unwrap_or_else(|| "FILE".to_owned());
            let detail: SharedString = match media.size_bytes {
                Some(bytes) => format!("{kind} · {}", file_size(bytes)).into(),
                None => kind.into(),
            };
            div()
                .w(px(300.))
                .p(px(10.))
                .rounded(px(4.))
                .bg(side.quote)
                .flex()
                .flex_col()
                .child(
                    div()
                        .debug_selector(|| "tile-row".into())
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(
                            div()
                                .debug_selector(|| "tile-icon".into())
                                .flex_none()
                                .child(tile_glyph(
                                    IconName::FileText,
                                    media,
                                    message,
                                    side,
                                    context,
                                )),
                        )
                        .child(
                            div()
                                .debug_selector(|| "tile-label".into())
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(div().truncate().child(name))
                                .child(mono(detail).line_height(px(16.)).text_color(side.meta)),
                        ),
                )
                .child(file_action(media, message, side, context))
        }
    }
}

/// The reactions under a bubble. The account's own stands out. The
/// pointer on one says who made it, and a click opens the panel that
/// lists everybody who reacted (`reactors.rs`).
fn reaction_chips(row: &MessageRow, context: &RowContext) -> Div {
    let (reactions, message) = (&row.stored.reactions, &row.stored.message);
    let palette = &context.palette;
    let mut chips = div()
        .mt(px(-6.))
        .mx(px(8.))
        .mb(px(2.))
        .flex()
        .items_center()
        .gap(px(3.));
    for (index, reaction) in reactions.iter().take(4).enumerate() {
        let mine = reaction.from_me;
        let (view, key, emoji) = (
            context.view.clone(),
            row.key.clone(),
            reaction.emoji.clone(),
        );
        let (hint_view, hint_of, hint_reaction) =
            (context.view.clone(), message.clone(), reaction.clone());
        chips = chips.child(
            div()
                .id(SharedString::from(format!(
                    "reaction-{}-{index}",
                    message.id
                )))
                .debug_selector(move || {
                    if mine {
                        "reaction-mine".into()
                    } else {
                        "reaction-chip".into()
                    }
                })
                .h(px(22.))
                .px(px(6.))
                .rounded_full()
                .border_1()
                // Your own reaction wears the accent.
                .border_color(if mine {
                    palette.focus_ring
                } else {
                    palette.border
                })
                .bg(if mine { palette.muted } else { palette.surface })
                .flex()
                .items_center()
                .gap(px(3.))
                .cursor_pointer()
                .text_size(px(12.5))
                .line_height(px(18.))
                .hover(|style| style.opacity(0.8))
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation()
                })
                // Under the pointer: who made it.
                .tooltip(move |window, cx| {
                    let hint = hint_view
                        .upgrade()
                        .map(|view| view.read(cx).reactors_hint(&hint_of, &hint_reaction))
                        .unwrap_or_default();
                    gpui_kit::component::tooltip::Tooltip::new(hint).build(window, cx)
                })
                // A click: everybody who reacted, this reaction's first.
                .on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    let at = event.position();
                    view.update(cx, |shell, cx| {
                        shell.open_reactors(key.clone(), Some(emoji.clone()), Some(at), window, cx)
                    })
                    .ok();
                })
                .child(SharedString::from(reaction.emoji.clone()))
                .when(reaction.count > 1, |this| {
                    this.child(
                        mono(reaction.count.to_string())
                            .text_size(px(10.5))
                            .text_color(palette.text_muted),
                    )
                }),
        );
    }
    chips
}
