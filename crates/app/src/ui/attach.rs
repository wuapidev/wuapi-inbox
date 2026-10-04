//! Attaching files: the "+" button, a drop on the conversation, a paste
//! in the composer, and the sheet that shows what is about to be sent.
//!
//! The sheet is a carousel, as WhatsApp's media composer is: a large view
//! of the file on show, its caption, and a strip of every file to pick
//! another, take one out or add more. Each file has its own caption, with
//! its own mentions and its own caret and undo history: the caption on
//! screen is the field of the file on show, and everything the composer's
//! field can do (mentions, emoji completion, the emoji picker, several
//! lines) works in it through `Shell::writing`.
//!
//! Whatever way files arrive, they are read off the UI thread, checked
//! against the provider's size limit, and shown in the sheet first: a file
//! over the limit is shown flagged and holds back only itself. Nothing is
//! sent without Enter or a click on "Send". Sending is the engine's: each
//! file goes through the outbox like a text, as its own message in the
//! order of the strip, so a dropped connection keeps it waiting instead of
//! failing it.

use super::mentions::Mentioning;
use super::shell::{Overlay, PickKind, Shell};
use super::widgets::{icon_button, label, mono, switch, text_button};
use crate::attach::{self, Attachment, Pasted, Read};
use crate::format::file_size;
use crate::icons::{icon, IconName};
use crate::keys::Command;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::{NewMedia, SendMediaError};
use client_provider::{ClientMessageId, MediaKind};
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::*;
use gpui_kit::Focusable as _;
use gpui_kit::{
    div, img, ClipboardItem, Context, Div, Entity, Image, ImageFormat, ObjectFit, SharedString,
    Stateful, StyledImage, Subscription, Window,
};
use std::path::PathBuf;
use std::sync::Arc;

/// Why the attach button and the paste of a file say no.
pub(in crate::ui) const NOT_AVAILABLE: &str = "Sending files is not available yet";

/// The longest edge of the picture the sheet keeps of an image.
const PREVIEW_EDGE: u32 = 480;

/// One file in the sheet.
pub(in crate::ui) struct AttachItem {
    pub(in crate::ui) file: Attachment,
    /// Its size. A file over the limit is not read: it has this, and no
    /// bytes.
    size: u64,
    /// A picture of it, when it is one.
    preview: Option<Arc<Image>>,
    /// Why it cannot be sent, when it cannot. It holds back only itself.
    pub(in crate::ui) blocked: Option<SharedString>,
    /// Sent as a document, not as a picture or a video.
    pub(in crate::ui) as_document: bool,
    /// Its caption: a field of its own, made when the sheet is drawn
    /// (a field needs the window).
    pub(in crate::ui) caption: Option<Entity<TextareaState>>,
    /// The people its caption mentions.
    pub(in crate::ui) mentioning: Mentioning,
    _caption_events: Option<Subscription>,
}

impl AttachItem {
    /// What it will say, trimmed. Nothing for a file the field of which
    /// has not been made yet.
    fn caption_text(&self, cx: &gpui_kit::App) -> String {
        self.caption
            .as_ref()
            .map(|field| field.read(cx).value().trim().to_owned())
            .unwrap_or_default()
    }

    /// What it is, in a word.
    fn kind_label(&self) -> &'static str {
        match self.file.kind(self.as_document) {
            MediaKind::Image => "IMAGE",
            MediaKind::Video => "VIDEO",
            MediaKind::Audio | MediaKind::Voice => "AUDIO",
            _ => "DOCUMENT",
        }
    }
}

/// What is about to be sent.
pub(in crate::ui) struct AttachDraft {
    pub(in crate::ui) items: Vec<AttachItem>,
    /// The file on show.
    pub(in crate::ui) selected: usize,
    /// What could not be attached, in words.
    pub(in crate::ui) notes: Vec<SharedString>,
    /// Leaving was asked for and the sheet asked whether to throw the
    /// files and their captions away.
    pub(in crate::ui) confirm_discard: bool,
}

impl AttachDraft {
    /// The file on show.
    pub(in crate::ui) fn current(&self) -> Option<&AttachItem> {
        self.items.get(self.selected)
    }

    /// The first file that can go: the one a reply goes with.
    fn first_sendable(&self) -> Option<usize> {
        self.items.iter().position(|item| item.blocked.is_none())
    }

    /// Whether leaving loses something worth a question: more than one
    /// file, or a caption with text.
    fn worth_asking(&self, cx: &gpui_kit::App) -> bool {
        self.items.len() > 1
            || self
                .items
                .iter()
                .any(|item| !item.caption_text(cx).is_empty())
    }
}

/// A file read for the sheet, before it has a field of its own.
pub(in crate::ui) struct Staged {
    file: Attachment,
    size: u64,
    preview: Option<Arc<Image>>,
    blocked: Option<SharedString>,
}

/// A small picture of an image file, made here from its bytes.
fn preview_of(file: &Attachment) -> Option<Arc<Image>> {
    if !file.is_image() {
        return None;
    }
    let small = client_core::thumbnail(&file.bytes, PREVIEW_EDGE).ok()?;
    let format = if small.mime == "image/png" {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };
    Some(Arc::new(Image::from_bytes(format, small.bytes)))
}

/// A file as the sheet shows it: its picture, and whether it can go.
fn stage(read: Read, limit: Option<u64>) -> Staged {
    match read {
        Read::Ready(file) => {
            let size = file.bytes.len() as u64;
            let blocked = limit
                .filter(|limit| size > *limit)
                .map(|limit| SharedString::from(attach::too_large(size, limit)));
            let preview = blocked.is_none().then(|| preview_of(&file)).flatten();
            Staged {
                file,
                size,
                preview,
                blocked,
            }
        }
        Read::TooLarge { name, size } => Staged {
            file: Attachment {
                mime: attach::mime_of(&name, &[]),
                name,
                bytes: Arc::new(Vec::new()),
            },
            size,
            preview: None,
            blocked: limit.map(|limit| SharedString::from(attach::too_large(size, limit))),
        },
    }
}

impl Shell {
    /// Whether files can be sent in this session right now.
    pub(in crate::ui) fn can_attach(&self) -> bool {
        self.engine.uploads_available() == Some(true)
    }

    /// The "+" button: the system's file dialog, then the sheet.
    pub(in crate::ui) fn pick_attachments(&mut self, cx: &mut Context<Self>) {
        if self.open.is_none() || !self.can_attach() {
            return;
        }
        let picked = (self.pick_files)(PickKind::Files, cx);
        // On its own: a drop or a paste while the dialog is open has its
        // own work, and neither takes the other's place.
        cx.spawn(async move |this, cx| {
            let Some(paths) = picked.await else {
                return;
            };
            this.update(cx, |this, cx| this.attach_paths(paths, cx))
                .ok();
        })
        .detach();
    }

    /// Files by their paths (picked, dropped, or pasted from a file
    /// manager): read off this thread, then shown in the sheet, after the
    /// ones it already has.
    pub(in crate::ui) fn attach_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.open.is_none() || paths.is_empty() {
            return;
        }
        if !self.can_attach() {
            return self.show_problem(format!("{NOT_AVAILABLE}."), cx);
        }
        let limit = self.engine.upload_limit();
        let runtime = self.engine.runtime().clone();
        let work = runtime.spawn_blocking(move || {
            let (files, refused) = attach::read_files(&paths, limit);
            let staged: Vec<Staged> = files.into_iter().map(|read| stage(read, limit)).collect();
            (staged, refused)
        });
        // Awaited on the runtime, so the answer wakes this view properly.
        let work = runtime.spawn(work);
        // Each batch is read on its own: files that arrive while others
        // are still being read join them, and none is dropped on the way.
        cx.spawn(async move |this, cx| {
            let read = work.await;
            this.update(cx, |this, cx| match read {
                Ok(Ok((staged, refused))) => this.add_attachments(staged, refused, cx),
                _ => this.show_problem("The files could not be read.".to_owned(), cx),
            })
            .ok();
        })
        .detach();
    }

    /// A picture straight from the clipboard.
    fn attach_pasted(&mut self, file: Attachment, cx: &mut Context<Self>) {
        let staged = stage(Read::Ready(file), self.engine.upload_limit());
        self.add_attachments(vec![staged], Vec::new(), cx);
    }

    /// Puts files in the sheet, opening it if it is not open. The first
    /// of the new ones is the one on show.
    fn add_attachments(
        &mut self,
        staged: Vec<Staged>,
        refused: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if self.open.is_none() {
            return;
        }
        // A form with something typed in it is not swept away by a drop
        // on the conversation behind it.
        if matches!(
            self.overlay,
            Overlay::NewPoll | Overlay::NewGroup | Overlay::OwnProfile
        ) {
            return;
        }
        // A profile or a group's details give way, and are closed properly.
        if self.overlay == Overlay::ContactInfo {
            self.social.close();
        }
        if staged.is_empty() && self.attach.is_none() {
            // Nothing to show a sheet for: say why, where problems go.
            if let Some(first) = refused.into_iter().next() {
                self.show_problem(first, cx);
            }
            return;
        }
        let opening = self.attach.is_none();
        let draft = self.attach.get_or_insert_with(|| AttachDraft {
            items: Vec::new(),
            selected: 0,
            notes: Vec::new(),
            confirm_discard: false,
        });
        let first_new = draft.items.len();
        draft
            .items
            .extend(staged.into_iter().map(|staged| AttachItem {
                file: staged.file,
                size: staged.size,
                preview: staged.preview,
                blocked: staged.blocked,
                as_document: false,
                caption: None,
                mentioning: Mentioning::default(),
                _caption_events: None,
            }));
        if draft.items.len() > first_new {
            draft.selected = first_new;
        }
        draft.confirm_discard = false;
        draft
            .notes
            .extend(refused.into_iter().map(SharedString::from));
        if opening {
            self.attach_strip
                .set_offset(gpui_kit::point(px(0.), px(0.)));
        }
        self.attach_strip.scroll_to_item(first_new);
        self.attach_wants_focus = true;
        self.overlay = Overlay::AttachSheet;
        cx.notify();
    }

    /// Gives each file its caption field, when the window is at hand to
    /// make it: a composer's, of several lines, in which Enter sends and
    /// Shift+Enter is a line break.
    pub(in crate::ui) fn ensure_captions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.attach.as_mut() else {
            return;
        };
        for item in draft.items.iter_mut().filter(|item| item.caption.is_none()) {
            let field = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(1, 4)
                    .submit_on_enter(true)
                    .placeholder("Add a caption")
            });
            let events =
                cx.subscribe_in(&field, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        // Enter sends everything; with the list of people open,
                        // it picks. Shift+Enter is a line break and never gets
                        // here.
                        InputEvent::PressEnter { shift: false, .. } => {
                            if this.pick_mention_under_cursor(window, cx) {
                                return;
                            }
                            if let Some(draft) = this.attach.as_mut() {
                                // A question about leaving is answered by
                                // going on with the files.
                                if std::mem::take(&mut draft.confirm_discard) {
                                    return cx.notify();
                                }
                            }
                            this.send_attachments(window, cx);
                        }
                        InputEvent::Change => {
                            this.mention_text_changed();
                            this.emoji_completion_changed(cx);
                            if let Some(draft) = this.attach.as_mut() {
                                draft.confirm_discard = false;
                            }
                            cx.notify()
                        }
                        _ => {}
                    }
                });
            item.caption = Some(field);
            item._caption_events = Some(events);
        }
    }

    /// The caption field on screen: the file on show's, when it has one.
    pub(in crate::ui) fn captioned_field(&self) -> Option<Entity<TextareaState>> {
        self.attach
            .as_ref()?
            .current()
            .filter(|item| item.blocked.is_none())?
            .caption
            .clone()
    }

    /// Whether the caption on screen has the keyboard.
    pub(in crate::ui) fn caption_has_keyboard(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        self.captioned_field()
            .is_some_and(|field| field.focus_handle(cx).is_focused(window))
    }

    /// A paste in the composer. Returns whether it was taken: `false`
    /// lets the field paste the text itself.
    pub(in crate::ui) fn paste_into_chat(
        &mut self,
        item: &ClipboardItem,
        cx: &mut Context<Self>,
    ) -> bool {
        match attach::classify_paste(item, |path| path.is_file()) {
            Pasted::Text => false,
            _ if !self.can_attach() => {
                // A file or a picture, and nowhere to send it: said,
                // rather than pasting a path or nothing at all.
                self.show_problem(format!("{NOT_AVAILABLE}."), cx);
                true
            }
            Pasted::Files(paths) => {
                self.attach_paths(paths, cx);
                true
            }
            Pasted::Image(file) => {
                self.attach_pasted(file, cx);
                true
            }
        }
    }

    /// Shows another file. Its caption takes the keyboard when `refocus`
    /// (the caption had it, or a click chose the file); from the strip
    /// the keyboard stays where it is.
    pub(in crate::ui) fn select_attachment(
        &mut self,
        index: usize,
        refocus: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.attach.as_mut() else {
            return;
        };
        if index >= draft.items.len() {
            return;
        }
        draft.selected = index;
        self.attach_strip.scroll_to_item(index);
        self.attach_wants_focus |= refocus;
        cx.notify();
    }

    /// The file before or after the one on show.
    fn step_attachment(&mut self, by: isize, refocus: bool, cx: &mut Context<Self>) {
        let Some(draft) = self.attach.as_ref() else {
            return;
        };
        let next = draft
            .selected
            .saturating_add_signed(by)
            .min(draft.items.len().saturating_sub(1));
        self.select_attachment(next, refocus, cx);
    }

    /// Moves the file on show one place earlier or later in what is sent.
    fn move_attachment(&mut self, by: isize, cx: &mut Context<Self>) {
        let Some(draft) = self.attach.as_mut() else {
            return;
        };
        let from = draft.selected;
        let to = from.saturating_add_signed(by);
        if to >= draft.items.len() {
            return;
        }
        draft.items.swap(from, to);
        draft.selected = to;
        self.attach_strip.scroll_to_item(to);
        cx.notify();
    }

    /// Takes one file out of the sheet. The last one out closes it.
    fn remove_attachment(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.attach.as_mut() else {
            return;
        };
        if index < draft.items.len() {
            draft.items.remove(index);
            if index < draft.selected || draft.selected >= draft.items.len() {
                draft.selected = draft.selected.saturating_sub(1);
            }
            draft.confirm_discard = false;
        }
        if draft.items.is_empty() {
            return self.close_attach_sheet(window, cx);
        }
        let selected = draft.selected;
        self.attach_strip.scroll_to_item(selected);
        self.attach_wants_focus = true;
        cx.notify();
    }

    /// Whether "send as a document" of the file on show goes to all of
    /// them.
    fn apply_document_to_all(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.attach.as_mut() else {
            return;
        };
        let Some(choice) = draft.current().map(|item| item.as_document) else {
            return;
        };
        for item in &mut draft.items {
            item.as_document = choice;
        }
        cx.notify();
    }

    /// Cancel, Escape, or a click outside the sheet: with something to
    /// lose (several files, or a caption written) it asks first, and the
    /// next one means it.
    pub(in crate::ui) fn dismiss_attach_sheet(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = self.attach.as_mut() else {
            return self.close_attach_sheet(window, cx);
        };
        let asking = draft.confirm_discard;
        let ask = !asking && draft.worth_asking(cx);
        if ask {
            if let Some(draft) = self.attach.as_mut() {
                draft.confirm_discard = true;
            }
            // The caption gives the keyboard up: Enter is not "send" now.
            self.overlay_focus.focus(window, cx);
            return cx.notify();
        }
        self.close_attach_sheet(window, cx);
    }

    /// Closes the sheet and drops what it held. Nothing is sent.
    pub(in crate::ui) fn close_attach_sheet(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.attach = None;
        self.close_overlay(window, cx);
    }

    /// Send, or Enter: every file that can go is put into the outbox, in
    /// the order of the strip, each as a message of its own with its own
    /// caption and mentions; a reply goes with the first. The bubbles
    /// appear at once.
    pub(in crate::ui) fn send_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .attach
            .as_ref()
            .is_none_or(|draft| draft.first_sendable().is_none())
        {
            return;
        }
        let (Some(draft), Some(open)) = (self.attach.take(), self.open.as_ref()) else {
            return;
        };
        // What it answers, when a reply was under way: the first file is
        // the answer, and the reply is done with.
        let reply_to = match &self.acting.compose {
            super::message_actions::Compose::Reply(message) => Some(message.id.clone()),
            _ => None,
        };
        if reply_to.is_some() {
            self.acting.compose = super::message_actions::Compose::New;
        }
        let (account, chat) = (open.chat.account_id.clone(), open.chat.id.clone());
        let mut left_out = 0usize;
        let mut files: Vec<NewMedia> = Vec::new();
        for item in draft.items {
            if item.blocked.is_some() {
                left_out += 1;
                continue;
            }
            // The people picked from the list go by their ids in what is
            // sent, as in a text.
            let (caption, mentions) =
                crate::mentioning::outgoing(&item.caption_text(cx), &item.mentioning.picked);
            let first = files.is_empty();
            files.push(NewMedia {
                kind: item.file.kind(item.as_document),
                bytes: Arc::unwrap_or_clone(item.file.bytes),
                mime_type: item.file.mime,
                file_name: Some(item.file.name),
                caption: (!caption.is_empty()).then_some(caption),
                // As WhatsApp does: the reply goes with the first file.
                reply_to: reply_to.clone().filter(|_| first),
                mentions,
            });
        }
        open.list.scroll_to_end();
        let engine = self.engine.clone();
        let runtime = self.engine.runtime().clone();
        // One after the other, so they keep their order. A picture is
        // decoded for its thumbnail off every thread that matters; the
        // store is written from the runtime's own task.
        let blocking = runtime.clone();
        let work = runtime.spawn(async move {
            // Why each file that was not queued was not.
            let mut errors: Vec<String> = Vec::new();
            for file in files {
                let name = file.file_name.clone().unwrap_or_default();
                let preparing = engine.clone();
                let prepared = blocking
                    .spawn_blocking(move || preparing.prepare_media(file))
                    .await;
                let queued = match prepared {
                    Ok(Ok(prepared)) => engine.send_prepared(&account, &chat, prepared).err(),
                    Ok(Err(error)) => Some(error),
                    // The work itself broke: said, not passed over.
                    Err(_) => {
                        errors.push(format!("{name} was not sent: it could not be prepared."));
                        None
                    }
                };
                errors.extend(queued.map(|error| match error {
                    SendMediaError::TooLarge { size, limit } => format!(
                        "A file of {} was not sent: the most that can be sent is {}.",
                        file_size(size),
                        file_size(limit)
                    ),
                    other => other.to_string(),
                }));
            }
            errors
        });
        // On its own, apart from the reading of files attached next: what
        // was not sent is always said.
        cx.spawn(async move |this, cx| {
            let errors = work.await.unwrap_or_else(|_| {
                vec!["The files were not sent: they could not be prepared.".to_owned()]
            });
            this.update(cx, |this, cx| {
                if let Some(message) = errors.into_iter().next() {
                    this.show_problem(message, cx);
                }
                if let Some(open) = &this.open {
                    open.list.scroll_to_end();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        self.close_attach_sheet(window, cx);
        if left_out > 0 {
            let which = if left_out == 1 {
                "A file that is too large was".to_owned()
            } else {
                format!("{left_out} files that are too large were")
            };
            self.show_problem(format!("{which} not sent."), cx);
        }
    }

    /// "Cancel" on a bubble that has not gone yet.
    pub(in crate::ui) fn cancel_send(
        &mut self,
        client_id: ClientMessageId,
        cx: &mut Context<Self>,
    ) {
        match self.engine.cancel_send(&client_id) {
            Ok(true) => {}
            Ok(false) => self.show_problem(
                "Too late to cancel: the message is already on its way.".to_owned(),
                cx,
            ),
            Err(error) => tracing::error!(%error, "could not cancel a send"),
        }
        cx.notify();
    }

    /// The sheet: the file on show, its caption, the strip of all the
    /// files, and the way out.
    pub(in crate::ui) fn render_attach_sheet(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let draft = self.attach.as_ref()?;
        let sendable: Vec<&AttachItem> = draft
            .items
            .iter()
            .filter(|item| item.blocked.is_none())
            .collect();
        let total: u64 = sendable.iter().map(|item| item.size).sum();
        let blocked = draft.items.len() - sendable.len();
        let title = match draft.items.len() {
            1 => "Send 1 file".to_owned(),
            count => format!("Send {count} files"),
        };
        let media = draft
            .items
            .iter()
            .filter(|item| item.file.kind(false) != MediaKind::Document)
            .count();
        let current = draft.current();
        let first = draft.first_sendable();
        let room = self.viewport;

        // ----- the file on show
        let show = current.map(|item| {
            let face = match &item.preview {
                Some(preview) => img(preview.clone())
                    .size_full()
                    .object_fit(ObjectFit::Contain)
                    .into_any_element(),
                None => div()
                    .max_w_full()
                    .px_4()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .child(icon(
                        if item.blocked.is_some() {
                            IconName::TriangleAlert
                        } else {
                            IconName::FileText
                        },
                        px(36.),
                        palette.icon,
                    ))
                    .child(
                        div()
                            .max_w_full()
                            .truncate()
                            .text_size(metrics::TEXT_BODY())
                            .child(SharedString::from(item.file.name.clone())),
                    )
                    .child(
                        mono(format!("{} · {}", item.kind_label(), file_size(item.size)))
                            .text_color(palette.text_muted),
                    )
                    .into_any_element(),
            };
            div()
                .flex()
                .flex_col()
                .gap_2()
                .flex_1()
                .min_h_0()
                .child(
                    // Grows to the room there is, down to a sliver.
                    div()
                        .debug_selector(|| "attach-preview".into())
                        .flex_1()
                        .min_h(px(96.))
                        .max_h(px(280.))
                        .w_full()
                        .rounded(metrics::RADIUS())
                        .bg(palette.muted)
                        .overflow_hidden()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(face),
                )
                .child(
                    div()
                        .debug_selector(|| "attach-file-info".into())
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(metrics::TEXT_BODY())
                                .child(SharedString::from(item.file.name.clone())),
                        )
                        .child(
                            mono(format!("{} · {}", item.kind_label(), file_size(item.size)))
                                .flex_none()
                                .text_color(palette.text_muted),
                        ),
                )
                .children(item.blocked.as_ref().map(|reason| {
                    div()
                        .debug_selector(|| "attach-blocked".into())
                        .flex_none()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.danger)
                        .child(format!(
                            "{reason} This file will not be sent; the others will."
                        ))
                }))
        });

        // ----- its caption: the field of the file on show
        let caption = current
            .filter(|item| item.blocked.is_none())
            .and_then(|item| item.caption.clone())
            .map(|field| {
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .gap_1()
                    // While a mention or a shortcode is typed, the arrows,
                    // Tab and Escape are the list's before they are the
                    // field's or the sheet's.
                    .capture_key_down(cx.listener(
                        |this, event: &gpui_kit::KeyDownEvent, window, cx| {
                            this.mention_key(event, window, cx);
                            this.emoji_completion_escape(event, cx);
                        },
                    ))
                    .child(label("Caption", palette))
                    .children(self.render_mention_picker(palette, cx))
                    .children(self.render_emoji_completion(palette, cx))
                    .child(
                        // The composer's field, for the caption: it grows
                        // with its lines, Enter sends, Shift+Enter is a
                        // new line.
                        div()
                            .debug_selector(|| "attach-caption".into())
                            .min_h(px(36.))
                            .pl_2()
                            .pr(px(2.))
                            .py(px(2.))
                            .rounded(metrics::RADIUS())
                            .border_1()
                            .border_color(palette.border)
                            .bg(palette.background)
                            .flex()
                            .items_end()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h(metrics::CONTROL())
                                    .flex()
                                    .items_center()
                                    .child({
                                        // Another file pasted here joins
                                        // the ones in the sheet.
                                        let view = cx.entity().downgrade();
                                        Textarea::new(&field).appearance(false).small().on_paste(
                                            move |item, _, cx| {
                                                view.update(cx, |this, cx| {
                                                    this.paste_into_chat(item, cx)
                                                })
                                                .unwrap_or(false)
                                            },
                                        )
                                    }),
                            )
                            .child(self.render_caption_emoji_button(palette, cx)),
                    )
            });

        // ----- how it is sent, for this file
        let options = current
            .filter(|item| item.file.kind(false) != MediaKind::Document)
            .map(|item| {
                div()
                    .debug_selector(|| "attach-options".into())
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(18.))
                            .text_color(palette.text_muted)
                            .child(
                                "Send as a document: the file as it is, not as a \
                                 picture in the chat.",
                            ),
                    )
                    .when(media > 1, |this| {
                        let ink = palette.accent;
                        this.child(
                            div()
                                .id("attach-apply-all")
                                .debug_selector(|| "attach-apply-all".into())
                                .flex_none()
                                .cursor_pointer()
                                .text_size(metrics::TEXT_SMALL())
                                .text_color(ink)
                                .hover(|style| style.opacity(0.7))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.apply_document_to_all(cx);
                                }))
                                .child("Apply to all"),
                        )
                    })
                    .child(
                        switch("attach-as-document", item.as_document, palette).on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(item) = this
                                    .attach
                                    .as_mut()
                                    .and_then(|draft| draft.items.get_mut(draft.selected))
                                {
                                    item.as_document = !item.as_document;
                                }
                                cx.notify();
                            }),
                        ),
                    )
            });

        // ----- the strip of every file
        let ring = palette.focus_ring;
        let mut strip = div()
            .id("attach-strip")
            .debug_selector(|| "attach-strip".into())
            .flex_none()
            .w_full()
            .p(px(2.))
            .flex()
            .items_center()
            .gap_2()
            .overflow_x_scroll()
            .track_scroll(&self.attach_strip)
            .track_focus(&self.attach_strip_focus)
            .tab_index(0)
            .rounded(metrics::RADIUS())
            .border_1()
            .border_color(gpui_kit::transparent_black())
            .focus_visible(move |style| style.border_color(ring));
        for (index, item) in draft.items.iter().enumerate() {
            let on_show = index == draft.selected;
            let has_caption = !item.caption_text(cx).is_empty();
            let edge = if item.blocked.is_some() {
                palette.danger
            } else if on_show {
                palette.accent
            } else {
                palette.border
            };
            let face: Div = match &item.preview {
                Some(preview) => div().size_full().child(
                    img(preview.clone())
                        .size_full()
                        .object_fit(ObjectFit::Cover),
                ),
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(IconName::FileText, px(22.), palette.icon)),
            };
            strip = strip.child(
                div()
                    .id(("attach-item", index))
                    .debug_selector(move || format!("attach-item-{index}"))
                    .relative()
                    .flex_none()
                    .size(px(56.))
                    .rounded(metrics::RADIUS())
                    .border_2()
                    .border_color(edge)
                    .bg(palette.muted)
                    .overflow_hidden()
                    .cursor_pointer()
                    .when(!on_show, |this| this.opacity(0.8))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.select_attachment(index, true, cx);
                    }))
                    .child(face)
                    // A mark on the ones that have a caption, and on the
                    // ones that cannot be sent.
                    .when(has_caption, |this| {
                        this.child(
                            div()
                                .debug_selector(move || format!("attach-caption-mark-{index}"))
                                .absolute()
                                .left(px(3.))
                                .bottom(px(3.))
                                .size(px(10.))
                                .rounded_full()
                                .border_1()
                                .border_color(palette.elevated)
                                .bg(palette.accent_fill),
                        )
                    })
                    .when(item.blocked.is_some(), |this| {
                        this.child(
                            div()
                                .debug_selector(move || format!("attach-blocked-mark-{index}"))
                                .absolute()
                                .right(px(3.))
                                .bottom(px(3.))
                                .child(icon(IconName::TriangleAlert, px(14.), palette.danger)),
                        )
                    })
                    .child(
                        div()
                            .id(("attach-remove", index))
                            .debug_selector(move || format!("attach-remove-{index}"))
                            .absolute()
                            .top(px(2.))
                            .right(px(2.))
                            .size(px(18.))
                            .rounded_full()
                            .bg(palette.elevated)
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.7))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.remove_attachment(index, window, cx);
                            }))
                            .child(icon(IconName::X, px(11.), palette.icon)),
                    ),
            );
        }
        let add_hint = super::hints::with_keys("Add more files", Some(Command::Attach));
        strip = strip.child(
            icon_button("attach-add", IconName::Plus, palette)
                .size(px(56.))
                .tooltip(move |window, cx| Tooltip::new(add_hint.clone()).build(window, cx))
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.pick_attachments(cx);
                })),
        );

        // ----- the footer: the total, and the way out
        let summary = {
            let files = sendable.len();
            let mut text = format!(
                "{files} {} · {}",
                if files == 1 { "file" } else { "files" },
                file_size(total)
            );
            if blocked > 0 {
                text.push_str(&format!(" · {blocked} too large"));
            }
            text
        };
        let footer = div()
            .flex_none()
            .p_4()
            .border_t_1()
            .border_color(palette.border)
            .flex()
            .items_center()
            .justify_between()
            .gap_2();
        let footer = if draft.confirm_discard {
            footer
                .debug_selector(|| "attach-discard-confirm".into())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.text)
                        .child(if draft.items.len() > 1 {
                            format!(
                                "Discard these {} files and what was written? \
                                 Esc again discards.",
                                draft.items.len()
                            )
                        } else {
                            "Discard this file and its caption? Esc again discards.".to_owned()
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .gap_2()
                        .child(
                            text_button("attach-keep", "Keep editing", None, true, palette)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    if let Some(draft) = this.attach.as_mut() {
                                        draft.confirm_discard = false;
                                    }
                                    this.attach_wants_focus = true;
                                    cx.notify();
                                })),
                        )
                        .child(
                            text_button("attach-discard", "Discard", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_attach_sheet(window, cx);
                                })),
                        ),
                )
        } else {
            let can_send = first.is_some();
            footer
                .child(
                    mono(summary)
                        .debug_selector(|| "attach-total".into())
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(palette.text_muted),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .gap_2()
                        .child(
                            text_button("attach-cancel", "Cancel", None, false, palette).on_click(
                                cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.dismiss_attach_sheet(window, cx);
                                }),
                            ),
                        )
                        .child({
                            let send = text_button(
                                "attach-send",
                                "Send",
                                Some(IconName::SendHorizontal),
                                true,
                                palette,
                            );
                            if can_send {
                                send.on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.send_attachments(window, cx);
                                }))
                            } else {
                                send.opacity(0.45)
                            }
                        }),
                )
        };

        let reply_note = match (&self.acting.compose, first) {
            (super::message_actions::Compose::Reply(_), Some(first)) => {
                let name = draft.items[first].file.name.clone();
                Some(
                    div()
                        .debug_selector(|| "attach-reply-note".into())
                        .flex_none()
                        .mb(px(6.))
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .truncate()
                        .child(format!("The reply goes with the first file, {name}.")),
                )
            }
            _ => None,
        };

        Some(
            self.card("attach-sheet", palette)
                .w(px(520.))
                .max_w((room.width - px(32.)).max(px(240.)))
                .max_h((room.height - px(32.)).max(px(240.)).min(px(720.)))
                .flex()
                .flex_col()
                .child(super::menus::panel_header(
                    &title,
                    palette,
                    cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.dismiss_attach_sheet(window, cx);
                    }),
                ))
                .child(
                    div()
                        .id("attach-body")
                        .flex_1()
                        .min_h_0()
                        .overflow_y_scroll()
                        .p_4()
                        .flex()
                        .flex_col()
                        .gap_3()
                        // What the first file answers, when a reply was
                        // under way.
                        .children(
                            self.render_compose_bar(palette, cx)
                                .map(|bar| bar.mb(px(0.))),
                        )
                        .children(reply_note)
                        .children(show)
                        .children(caption)
                        .child(strip)
                        .children(options)
                        .children(draft.notes.iter().enumerate().map(|(index, note)| {
                            div()
                                .debug_selector(move || format!("attach-note-{index}"))
                                .flex_none()
                                .text_size(metrics::TEXT_SMALL())
                                .line_height(px(18.))
                                .text_color(palette.danger)
                                .child(note.clone())
                        })),
                )
                .child(footer),
        )
    }
}

impl Shell {
    /// The keys of the sheet, seen before the text field's own bindings:
    /// the emoji picker's key, and the registry's keys for the files
    /// (previous, next, earlier, later, remove, add), which are written
    /// so that none of them is something a caption does: Ctrl or Cmd
    /// with Page Up and Page Down, and the bare arrows and Delete only
    /// when no text field has the keyboard.
    pub(in crate::ui) fn attach_key(
        &mut self,
        stroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.overlay != Overlay::AttachSheet || self.attach.is_none() {
            return false;
        }
        let in_caption = self.caption_has_keyboard(window, cx);
        let emoji = crate::keys::binding(Command::EmojiPicker)
            .is_some_and(|binding| binding.chords.iter().any(|chord| chord.matches(stroke)));
        if in_caption && emoji {
            self.toggle_emoji_picker(window, cx);
            return true;
        }
        let Some(command) = crate::keys::resolve(stroke, self.key_context(window, cx)) else {
            return false;
        };
        match command {
            Command::AttachPrevious => self.step_attachment(-1, in_caption, cx),
            Command::AttachNext => self.step_attachment(1, in_caption, cx),
            Command::AttachMoveEarlier => self.move_attachment(-1, cx),
            Command::AttachMoveLater => self.move_attachment(1, cx),
            Command::AttachRemove => {
                let at = self.attach.as_ref().map_or(0, |draft| draft.selected);
                self.remove_attachment(at, window, cx);
            }
            Command::Attach => self.pick_attachments(cx),
            _ => return false,
        }
        true
    }
}
