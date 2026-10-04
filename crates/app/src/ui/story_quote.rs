//! What a reply to a status looks like in a conversation: a block above
//! the text with a small picture of the status (its picture, or its words
//! on their colour) while the status is still there, and "Status no longer
//! available" once it is not. It has one height either way, so a status
//! that expires does not move the messages around it.

use super::bubble::{RowContext, Side};
use super::media::MediaVisual;
use crate::icons::{icon, IconName};
use crate::stories::{face_of, SlideKind};
use crate::theme::{metrics, px, story};
use client_provider::{Message, StoryBody, StoryReplyKind, StoryReplyRef};
use gpui_kit::prelude::*;
use gpui_kit::{div, img, Div, FontWeight, ObjectFit, SharedString, Stateful, StyledImage};

/// The side of the small picture.
fn thumb() -> gpui_kit::Pixels {
    px(44.)
}

/// What the quoted status was, in a word.
fn kind_word(kind: StoryReplyKind) -> &'static str {
    match kind {
        StoryReplyKind::Text => "Status",
        StoryReplyKind::Image => "Photo",
        StoryReplyKind::Video => "Video",
        StoryReplyKind::Voice => "Voice message",
    }
}

/// The block that says which status a message answers.
pub(super) fn story_quote(
    reply: &StoryReplyRef,
    message: &Message,
    side: &Side,
    context: &RowContext,
) -> Stateful<Div> {
    let item = context
        .engine
        .store()
        .story(&message.account_id, &reply.story)
        .ok()
        .flatten();
    let heading: SharedString = if reply.of_mine {
        "Replied to your status".into()
    } else if message.direction == client_provider::Direction::Outgoing {
        "You replied to a status".into()
    } else {
        "Replied to a status".into()
    };
    // The picture: the status itself while it lives.
    let face: gpui_kit::AnyElement = match &item {
        Some(item) => match &item.story.body {
            StoryBody::Text { style, .. } => div()
                .size(thumb())
                .flex_none()
                .rounded(px(4.))
                .bg(story::background(style.background))
                .flex()
                .items_center()
                .justify_center()
                .text_color(story::on_background())
                .text_size(px(13.))
                .child("Aa")
                .into_any_element(),
            StoryBody::Media(media) => match SlideKind::of(&item.story.body) {
                SlideKind::Image => match context.shelf.visual(&message.account_id, media) {
                    MediaVisual::Image(picture) => div()
                        .size(thumb())
                        .flex_none()
                        .rounded(px(4.))
                        .overflow_hidden()
                        .child(img(picture).size_full().object_fit(ObjectFit::Cover))
                        .into_any_element(),
                    _ => tile(IconName::Image, side),
                },
                SlideKind::Video => tile(IconName::Video, side),
                _ => tile(IconName::Mic, side),
            },
        },
        None => tile(IconName::Clock3, side),
    };
    let (line, gone): (SharedString, bool) = match &item {
        Some(_) => (
            reply
                .preview
                .clone()
                .unwrap_or_else(|| kind_word(reply.kind).to_owned())
                .into(),
            false,
        ),
        None => ("Status no longer available".into(), true),
    };
    let (view, story_id) = (context.view.clone(), reply.story.clone());
    let available = item.is_some();
    let _ = face_of;
    div()
        .id(SharedString::from(format!("story-quote-{}", message.id)))
        .debug_selector(move || {
            if available {
                "story-quote".to_owned()
            } else {
                "story-quote-gone".to_owned()
            }
        })
        .mb(px(5.))
        .h(thumb() + px(10.))
        .flex()
        .items_center()
        .gap(px(9.))
        .rounded(px(6.))
        .overflow_hidden()
        .bg(side.quote)
        .pr(px(9.))
        .when(available, |this| {
            this.cursor_pointer()
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation()
                })
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    view.update(cx, |shell, cx| shell.watch_story(&story_id, window, cx))
                        .ok();
                })
        })
        .child(div().flex_none().w(px(3.)).h_full().bg(side.signal))
        .child(div().flex_none().child(face))
        .child(
            div()
                .min_w_0()
                .flex()
                .flex_col()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(18.))
                .child(
                    div()
                        .truncate()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(side.signal)
                        .child(heading),
                )
                .child(
                    div()
                        .max_w(px(320.))
                        .truncate()
                        .text_color(side.meta)
                        .when(gone, |this| this.italic())
                        .child(line),
                ),
        )
}

/// A tile of the small picture's size with a glyph in it.
fn tile(glyph: IconName, side: &Side) -> gpui_kit::AnyElement {
    div()
        .size(thumb())
        .flex_none()
        .rounded(px(4.))
        .bg(side.bubble)
        .flex()
        .items_center()
        .justify_center()
        .child(icon(glyph, px(18.), side.meta))
        .into_any_element()
}
