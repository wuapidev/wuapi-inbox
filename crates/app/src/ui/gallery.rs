//! The gallery of a chat: its photos and videos, its files and its links,
//! opened from the contact's or the group's panel.
//!
//! Everything in it is what the store holds of the chat: nothing is asked
//! of the provider to list it, so it is there at once and offline. The
//! newest comes first, under the month it was sent in. A picture opens in
//! the viewer, a file where files open, a link in the browser, and each
//! of them can be shown where it stands in the conversation.

use super::media::MediaVisual;
use super::shell::Shell;
use super::social::hint;
use super::widgets::{label, mono, segmented, text_button};
use crate::format;
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::GalleryKind;
use client_provider::{
    AccountId, ChatId, Direction, Media, MediaKind, Message, MessageContent, MessageId,
};
use gpui_kit::prelude::*;
use gpui_kit::{div, img, Context, Div, ObjectFit, SharedString, StyledImage, Window};

/// How many more entries each "Show more" brings.
const PAGE: usize = 120;
/// The side of a picture in the grid.
const CELL: f32 = 116.;

/// The gallery that is open in the info panel.
pub(super) struct Gallery {
    pub(super) account: AccountId,
    pub(super) chat: ChatId,
    pub(super) tab: GalleryKind,
    /// How many entries of the tab are asked for.
    limit: usize,
    /// The entries of the tab, newest first.
    entries: Vec<Message>,
    /// How many each tab has in all: media, files, links.
    totals: [usize; 3],
}

fn slot(kind: GalleryKind) -> usize {
    match kind {
        GalleryKind::Media => 0,
        GalleryKind::Files => 1,
        GalleryKind::Links => 2,
    }
}

/// The first web address in a text.
pub(super) fn first_link(text: &str) -> Option<&str> {
    text.split_whitespace()
        .find(|word| word.starts_with("http://") || word.starts_with("https://"))
}

impl Shell {
    /// How many photos, videos, files and links a chat has in the store.
    pub(super) fn gallery_total(&self, account: &AccountId, chat: &ChatId) -> usize {
        if let Some((known, total)) = self.social.gallery_count.borrow().as_ref() {
            if known == chat {
                return *total;
            }
        }
        let total = self.count_gallery(account, chat);
        *self.social.gallery_count.borrow_mut() = Some((chat.clone(), total));
        total
    }

    fn count_gallery(&self, account: &AccountId, chat: &ChatId) -> usize {
        [GalleryKind::Media, GalleryKind::Files, GalleryKind::Links]
            .into_iter()
            .map(|kind| {
                self.store
                    .gallery(account, chat, kind, 0)
                    .map_or(0, |(_, total)| total)
            })
            .sum()
    }

    /// Opens the gallery of a chat in the info panel.
    pub(super) fn open_gallery(
        &mut self,
        account: AccountId,
        chat: ChatId,
        cx: &mut Context<Self>,
    ) {
        let mut gallery = Gallery {
            account,
            chat,
            tab: GalleryKind::Media,
            limit: PAGE,
            entries: Vec::new(),
            totals: [0; 3],
        };
        for kind in [GalleryKind::Media, GalleryKind::Files, GalleryKind::Links] {
            gallery.totals[slot(kind)] = self
                .store
                .gallery(&gallery.account, &gallery.chat, kind, 0)
                .map_or(0, |(_, total)| total);
        }
        // Open on the first tab that has something.
        if let Some(kind) = [GalleryKind::Media, GalleryKind::Files, GalleryKind::Links]
            .into_iter()
            .find(|kind| gallery.totals[slot(*kind)] > 0)
        {
            gallery.tab = kind;
        }
        self.social.gallery = Some(gallery);
        self.load_gallery();
        cx.notify();
    }

    /// Reads the open tab from the store.
    fn load_gallery(&mut self) {
        let Some(gallery) = self.social.gallery.as_mut() else {
            return;
        };
        match self
            .store
            .gallery(&gallery.account, &gallery.chat, gallery.tab, gallery.limit)
        {
            Ok((entries, total)) => {
                gallery.entries = entries;
                gallery.totals[slot(gallery.tab)] = total;
            }
            Err(error) => {
                tracing::error!(%error, "could not read a chat's gallery");
                gallery.entries.clear();
            }
        }
    }

    fn gallery_tab(&mut self, tab: GalleryKind, cx: &mut Context<Self>) {
        if let Some(gallery) = self.social.gallery.as_mut() {
            gallery.tab = tab;
            gallery.limit = PAGE;
        }
        self.load_gallery();
        cx.notify();
    }

    fn gallery_more(&mut self, cx: &mut Context<Self>) {
        if let Some(gallery) = self.social.gallery.as_mut() {
            gallery.limit += PAGE;
        }
        self.load_gallery();
        cx.notify();
    }

    /// Whether the gallery is of the chat that is open, so that an entry
    /// can be shown where it stands.
    fn gallery_is_open_chat(&self) -> bool {
        match (&self.social.gallery, &self.open) {
            (Some(gallery), Some(open)) => {
                open.chat.id == gallery.chat && open.chat.account_id == gallery.account
            }
            _ => false,
        }
    }

    /// Closes the panel and goes to the message in the conversation.
    fn gallery_show(&mut self, message: MessageId, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlay(window, cx);
        self.jump_and_flash(message, cx);
    }

    /// The gallery, as the body of the info panel.
    pub(super) fn render_gallery(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let Some(gallery) = &self.social.gallery else {
            return div();
        };
        let total = gallery.totals[slot(gallery.tab)];
        let view = cx.weak_entity();
        let tabs = segmented(
            "gallery-tab",
            &[
                (GalleryKind::Media, "Media"),
                (GalleryKind::Files, "Files"),
                (GalleryKind::Links, "Links"),
            ],
            gallery.tab,
            palette,
            move |tab, _, cx| {
                view.update(cx, |shell, cx| shell.gallery_tab(tab, cx)).ok();
            },
        );
        let counted = |count: usize, one: &str, many: &str| match count {
            1 => format!("1 {one}"),
            count => format!("{count} {many}"),
        };
        let summary = format!(
            "{} · {} · {}",
            counted(gallery.totals[0], "photo or video", "photos and videos"),
            counted(gallery.totals[1], "file", "files"),
            counted(gallery.totals[2], "link", "links"),
        );
        let head = div()
            .px_4()
            .pt_3()
            .pb_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(div().flex().child(tabs))
            .child(hint(summary, palette).debug_selector(|| "gallery-summary".into()));

        let mut body = div()
            .debug_selector(|| "gallery".into())
            .flex()
            .flex_col()
            .child(head);
        if gallery.entries.is_empty() {
            let nothing = match gallery.tab {
                GalleryKind::Media => "No photos or videos in this chat yet.",
                GalleryKind::Files => "No files in this chat yet.",
                GalleryKind::Links => "No links in this chat yet.",
            };
            return body.child(
                div()
                    .px_4()
                    .py_6()
                    .debug_selector(|| "gallery-empty".into())
                    .child(hint(nothing, palette)),
            );
        }
        let can_show = self.gallery_is_open_chat();
        // Under the month each was sent in, newest month first.
        let mut months: Vec<(String, Vec<&Message>)> = Vec::new();
        for message in &gallery.entries {
            let month = format::month(message.timestamp);
            match months.last_mut() {
                Some((known, of_month)) if *known == month => of_month.push(message),
                _ => months.push((month, vec![message])),
            }
        }
        for (month, of_month) in months {
            let section = div()
                .px_4()
                .pt_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(label(&month, palette));
            let content = match gallery.tab {
                GalleryKind::Media => div().flex().flex_wrap().gap(px(4.)).children(
                    of_month
                        .into_iter()
                        .map(|message| self.gallery_cell(message, can_show, palette, cx)),
                ),
                GalleryKind::Files | GalleryKind::Links => div().flex().flex_col().children(
                    of_month
                        .into_iter()
                        .map(|message| self.gallery_line(message, can_show, palette, cx)),
                ),
            };
            body = body.child(section.child(content));
        }
        if gallery.entries.len() < total {
            let left = total - gallery.entries.len();
            body = body.child(
                div().px_4().pt_3().flex().justify_center().child(
                    text_button(
                        "gallery-more",
                        format!("Show more ({left} left)"),
                        None,
                        false,
                        palette,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.gallery_more(cx);
                    })),
                ),
            );
        }
        body.pb_4()
    }

    /// One photo or video of the grid.
    fn gallery_cell(
        &self,
        message: &Message,
        can_show: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let MessageContent::Media(media) = &message.content else {
            return div().into_any_element();
        };
        let group = SharedString::from(format!("gallery-{}", message.id));
        let opened = media.clone();
        let video = media.kind == MediaKind::Video;
        let cell = div()
            .id(SharedString::from(format!("gallery-cell-{}", message.id)))
            .debug_selector(|| "gallery-cell".into())
            .group(group.clone())
            .relative()
            .flex_none()
            .size(px(CELL))
            .rounded(px(4.))
            .overflow_hidden()
            .bg(palette.muted)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .text_color(palette.text_muted)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                if video {
                    this.open_document(opened.clone(), window, cx);
                } else {
                    this.begin_viewing(opened.clone(), window, cx);
                }
            }));
        let cell = if video {
            cell.child(icon(IconName::Video, px(24.), palette.icon))
                .child(mono(video_line(media)))
        } else {
            match self.media.visual(&message.account_id, media) {
                MediaVisual::Image(image) => {
                    cell.child(img(image).size_full().object_fit(ObjectFit::Cover))
                }
                MediaVisual::Loading { .. } => cell.child(mono("LOADING…")),
                MediaVisual::Unavailable(_) | MediaVisual::Unsupported(_) | MediaVisual::NoFile => {
                    cell.child(icon(IconName::Ban, px(24.), palette.icon))
                }
                MediaVisual::Download { .. } | MediaVisual::Failed(_) => {
                    cell.child(icon(IconName::Image, px(24.), palette.icon))
                }
            }
        };
        // Who and when, and the way to the message, while the pointer is
        // on the picture.
        let who = sender_of(message);
        let id = message.id.clone();
        cell.child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .px(px(6.))
                .py(px(4.))
                .bg(gpui_kit::black().opacity(0.6))
                .text_color(gpui_kit::white())
                .invisible()
                .group_hover(group, |style| style.visible())
                .flex()
                .items_center()
                .justify_between()
                .gap_1()
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(metrics::TEXT_META())
                        .child(SharedString::from(format!(
                            "{who} · {}",
                            format::short_date(message.timestamp)
                        ))),
                )
                .when(can_show, |this| {
                    this.child(
                        div()
                            .id(SharedString::from(format!("gallery-show-{}", message.id)))
                            .debug_selector(|| "gallery-show".into())
                            .flex_none()
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.7))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.gallery_show(id.clone(), window, cx);
                            }))
                            .child(icon(IconName::MessageCircle, px(14.), gpui_kit::white())),
                    )
                }),
        )
        .into_any_element()
    }

    /// One file or one link of the list.
    fn gallery_line(
        &self,
        message: &Message,
        can_show: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let stamp = format!(
            "{} · {}",
            sender_of(message),
            format::short_date(message.timestamp)
        );
        let (glyph, title, detail, open): (IconName, String, String, Opens) = match &message.content
        {
            MessageContent::Media(media) => {
                let name = media.file_name.clone().unwrap_or_else(|| {
                    if media.kind == MediaKind::Audio {
                        "Audio".to_owned()
                    } else {
                        "Document".to_owned()
                    }
                });
                let kind = name
                    .rsplit_once('.')
                    .map(|(_, extension)| extension.to_uppercase())
                    .unwrap_or_else(|| "FILE".to_owned());
                let detail = match media.size_bytes {
                    Some(bytes) => format!("{kind} · {} · {stamp}", format::file_size(bytes)),
                    None => format!("{kind} · {stamp}"),
                };
                let glyph = if media.kind == MediaKind::Audio {
                    IconName::Mic
                } else {
                    IconName::FileText
                };
                (glyph, name, detail, Opens::File(media.clone()))
            }
            MessageContent::Text { body } => {
                let link = first_link(body).unwrap_or(body).to_owned();
                (IconName::Link, link.clone(), stamp, Opens::Link(link))
            }
            _ => return div().into_any_element(),
        };
        let hover = palette.hover;
        let id = message.id.clone();
        div()
            .id(SharedString::from(format!("gallery-line-{}", message.id)))
            .debug_selector(|| "gallery-line".into())
            .px_2()
            .py_2()
            .rounded(metrics::RADIUS())
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                match &open {
                    Opens::File(media) => this.open_document(media.clone(), window, cx),
                    Opens::Link(link) => cx.open_url(link),
                }
            }))
            .child(icon(glyph, px(20.), palette.icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_BODY())
                            .text_color(palette.text)
                            .child(SharedString::from(title)),
                    )
                    .child(
                        div()
                            .truncate()
                            .child(mono(detail).text_color(palette.text_muted)),
                    ),
            )
            .when(can_show, |this| {
                this.child(
                    div()
                        .id(SharedString::from(format!("gallery-show-{}", message.id)))
                        .debug_selector(|| "gallery-show".into())
                        .flex_none()
                        .p_1()
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.7))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.gallery_show(id.clone(), window, cx);
                        }))
                        .child(icon(IconName::MessageCircle, px(16.), palette.icon)),
                )
            })
            .into_any_element()
    }
}

/// What a click on a line of the gallery opens.
enum Opens {
    File(Media),
    Link(String),
}

/// Who sent a message, as the gallery names them.
fn sender_of(message: &Message) -> String {
    match message.direction {
        Direction::Outgoing => "You".to_owned(),
        Direction::Incoming => message
            .sender_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| message.sender.to_string()),
    }
}

/// What stands under the glyph of a video in the grid.
fn video_line(media: &Media) -> String {
    let kind = if media.gif { "GIF" } else { "VIDEO" };
    match media.duration_secs {
        Some(secs) => format!("{kind} · {}", format::duration(secs)),
        None => kind.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::first_link;

    #[test]
    fn the_first_address_of_a_text_is_its_link() {
        assert_eq!(
            first_link("look at https://example.org/a?b=1 and http://other.example"),
            Some("https://example.org/a?b=1")
        );
        assert_eq!(first_link("nothing here"), None);
    }
}
