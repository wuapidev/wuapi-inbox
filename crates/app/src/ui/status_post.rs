//! Posting a status, and who sees it.
//!
//! "New status" is a sheet: a text on a colour and a font, or a picture or
//! a video from a file (the system's dialog, a paste, or a drop on the
//! sheet) with a caption. The audience is shown before anything is
//! posted, with the way to change it. Posting is the engine's: the story
//! is written to the store and shows under "My status" at once, and the
//! story outbox takes it from there through any number of dropped
//! connections (`SyncEngine::post_story_text`, `post_story_prepared`).
//!
//! The sheets of Status (post, audience, delete, forward) are one overlay,
//! [`Sheet`] says which.

use super::shell::{Overlay, PickKind, Shell};
use super::widgets::{label, mono, text_button};
use crate::attach::{self, Attachment, Pasted};
use crate::format::file_size;
use crate::icons::{icon, IconName};
use crate::stories::{face_of, text_size, Face, Family};
use crate::theme::{fonts, metrics, px, story, Palette};
use client_core::NewMedia;
use client_provider::{
    ContactId, MediaKind, MessageId, StoryAudience, StoryFont, StoryPrivacy, STORY_TEXT_MAX,
};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, img, Context, Div, ExternalPaths, FontWeight, Image, ImageFormat, ObjectFit, SharedString,
    Stateful, StyledImage, Task, Window,
};
use std::path::PathBuf;
use std::sync::Arc;

/// What the sheet over Status is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Sheet {
    /// A new status.
    Post,
    /// Who sees the account's status.
    Audience,
    /// Are you sure: take a status down.
    ConfirmDelete(MessageId),
    /// Pass a status on to a chat.
    Forward(MessageId),
}

/// What kind of status is being written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PostTab {
    #[default]
    Text,
    Media,
}

/// A picture or a video chosen for a status.
pub(super) struct StoryFile {
    pub(super) attachment: Attachment,
    pub(super) kind: MediaKind,
    /// A small picture of it, when it is one.
    pub(super) preview: Option<Arc<Image>>,
}

/// What is being written to post.
pub(super) struct Draft {
    pub(super) tab: PostTab,
    /// The background of a text status, `0xRRGGBB`.
    pub(super) background: u32,
    pub(super) font: StoryFont,
    pub(super) file: Option<StoryFile>,
    /// Why something did not work, in words.
    pub(super) note: Option<SharedString>,
    /// A file is being read, or the post is being prepared.
    pub(super) busy: bool,
    /// The audience editor's working copy, and the list being picked.
    pub(super) audience: Option<AudienceEdit>,
    pub(super) work: Option<Task<()>>,
}

impl Default for Draft {
    fn default() -> Self {
        Self {
            tab: PostTab::Text,
            background: story::BACKGROUNDS[0],
            font: StoryFont(0),
            file: None,
            note: None,
            busy: false,
            audience: None,
            work: None,
        }
    }
}

/// The audience as it is being edited.
pub(super) struct AudienceEdit {
    pub(super) privacy: StoryPrivacy,
    /// The list of people being picked, when one is open.
    pub(super) picking: Option<StoryAudience>,
    pub(super) query: String,
}

/// The face of a font as a style on an element.
pub(super) fn with_face<E: Styled>(element: E, face: Face) -> E {
    let family = match face.family {
        Family::Sans => fonts::SANS,
        Family::Mono => fonts::MONO,
        Family::Serif => "serif",
    };
    let element = element.font_family(family).font_weight(if face.bold {
        FontWeight::BOLD
    } else {
        FontWeight::NORMAL
    });
    if face.italic {
        element.italic()
    } else {
        element
    }
}

/// A small picture of an image file.
fn preview_of(file: &Attachment) -> Option<Arc<Image>> {
    if !file.is_image() {
        return None;
    }
    let small = client_core::thumbnail(&file.bytes, 360).ok()?;
    let format = if small.mime == "image/png" {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };
    Some(Arc::new(Image::from_bytes(format, small.bytes)))
}

impl Shell {
    // ----- opening and closing ---------------------------------------------------

    /// "New status".
    pub(super) fn open_status_post(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.engine.capabilities().story_post {
            return;
        }
        if !self.status_active() {
            self.set_list_mode(super::status::ListMode::Status, window, cx);
        }
        self.status.draft = Draft::default();
        self.status
            .words
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.status
            .caption
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.status.sheet = Some(Sheet::Post);
        self.emoji_completion_reset();
        if let Some(account) = self.account.clone() {
            self.engine.want_story_privacy(&account);
        }
        self.open_overlay(Overlay::Status, window, cx);
        let words = self.status.words.clone();
        words.update(cx, |field, cx| field.focus(window, cx));
    }

    /// "Who sees my status".
    pub(super) fn open_audience(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.account.clone() else {
            return;
        };
        self.engine.want_story_privacy(&account);
        let privacy = self
            .engine
            .story_privacy_cached(&account)
            .ok()
            .flatten()
            .unwrap_or_default();
        self.status.draft.audience = Some(AudienceEdit {
            privacy,
            picking: None,
            query: String::new(),
        });
        self.status.sheet = Some(Sheet::Audience);
        self.open_overlay(Overlay::Status, window, cx);
    }

    /// Closes the sheet over Status.
    pub(super) fn close_status_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status.sheet = None;
        self.status.draft.work = None;
        self.emoji_completion_reset();
        if self.overlay == Overlay::Status {
            self.close_overlay(window, cx);
        }
        cx.notify();
    }

    /// Escape: a list being picked goes back to the audience first.
    pub(super) fn dismiss_status_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(edit) = self.status.draft.audience.as_mut() {
            if edit.picking.take().is_some() {
                edit.query.clear();
                return cx.notify();
            }
        }
        self.close_status_sheet(window, cx);
    }

    // ----- what is being written -----------------------------------------------------

    pub(super) fn status_words_changed(&mut self, cx: &mut Context<Self>) {
        self.status.draft.note = None;
        cx.notify();
    }

    fn set_post_tab(&mut self, tab: PostTab, window: &mut Window, cx: &mut Context<Self>) {
        self.status.draft.tab = tab;
        self.status.draft.note = None;
        self.emoji_completion_reset();
        let field = match tab {
            PostTab::Text => self.status.words.clone(),
            PostTab::Media => self.status.caption.clone(),
        };
        field.update(cx, |field, cx| field.focus(window, cx));
        cx.notify();
    }

    /// The system's file dialog, for a picture or a video.
    fn pick_story_file(&mut self, cx: &mut Context<Self>) {
        let picked = (self.pick_files)(PickKind::Files, cx);
        self.status.draft.work = Some(cx.spawn(async move |this, cx| {
            let Some(paths) = picked.await else { return };
            this.update(cx, |this, cx| this.story_paths(paths, cx)).ok();
        }));
    }

    /// Files by their paths (picked or dropped): the first picture or
    /// video is read off this thread and shown.
    pub(super) fn story_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() || self.status.sheet != Some(Sheet::Post) {
            return;
        }
        let limit = self.engine.upload_limit();
        let runtime = self.engine.runtime().clone();
        self.status.draft.busy = true;
        self.status.draft.note = None;
        let work = runtime.spawn_blocking(move || {
            let (files, mut refused) = attach::read_files(&paths[..1], limit);
            // The attach sheet lists a file over the limit with why it
            // cannot go; a status has one file, so it says it in its note.
            let file = match files.into_iter().next() {
                Some(attach::Read::Ready(attachment)) => {
                    let preview = preview_of(&attachment);
                    Some((attachment, preview))
                }
                Some(attach::Read::TooLarge { size, .. }) => {
                    if let Some(limit) = limit {
                        refused.insert(0, attach::too_large(size, limit));
                    }
                    None
                }
                None => None,
            };
            (file, refused)
        });
        let work = runtime.spawn(work);
        self.status.draft.work = Some(cx.spawn(async move |this, cx| {
            let result = work.await.and_then(|inner| inner);
            this.update(cx, |this, cx| {
                this.status.draft.busy = false;
                match result {
                    Ok((Some((attachment, preview)), _)) => {
                        this.story_file_read(attachment, preview, cx)
                    }
                    Ok((None, refused)) => {
                        this.status.draft.note = Some(
                            refused
                                .into_iter()
                                .next()
                                .unwrap_or_else(|| "That file could not be read.".to_owned())
                                .into(),
                        )
                    }
                    Err(_) => {}
                }
                cx.notify();
            })
            .ok();
        }));
    }

    fn story_file_read(
        &mut self,
        attachment: Attachment,
        preview: Option<Arc<Image>>,
        cx: &mut Context<Self>,
    ) {
        let kind = attachment.kind(false);
        if !matches!(kind, MediaKind::Image | MediaKind::Video) {
            self.status.draft.note = Some("A status is a picture, a video or a text.".into());
            return;
        }
        self.status.draft.file = Some(StoryFile {
            attachment,
            kind,
            preview,
        });
        self.status.draft.tab = PostTab::Media;
        cx.notify();
    }

    /// A paste while the sheet is open: a picture or a file becomes the
    /// status. `false` lets the text field paste text itself.
    pub(super) fn paste_into_status(
        &mut self,
        item: &gpui_kit::ClipboardItem,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.status.sheet != Some(Sheet::Post) {
            return false;
        }
        match attach::classify_paste(item, |path| path.is_file()) {
            Pasted::Text => false,
            Pasted::Files(paths) => {
                self.story_paths(paths, cx);
                true
            }
            Pasted::Image(file) => {
                let preview = preview_of(&file);
                self.story_file_read(file, preview, cx);
                true
            }
        }
    }

    /// Posts what is written: queued at once, shown under "My status",
    /// and the sheet closes. The words or the file are checked first.
    pub(super) fn post_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.account.clone() else {
            return;
        };
        if self.status.draft.busy {
            return;
        }
        match self.status.draft.tab {
            PostTab::Text => {
                let text =
                    crate::stories::with_shortcodes(self.status.words.read(cx).value().as_ref());
                let style = client_provider::StoryStyle {
                    background: self.status.draft.background,
                    font: self.status.draft.font,
                };
                match self.engine.post_story_text(&account, &text, style) {
                    Ok(_) => self.status_posted(window, cx),
                    Err(error) => {
                        self.status.draft.note = Some(error.to_string().into());
                        cx.notify();
                    }
                }
            }
            PostTab::Media => {
                let Some(file) = self.status.draft.file.take() else {
                    self.status.draft.note = Some("Choose a picture or a video first.".into());
                    return cx.notify();
                };
                let caption =
                    crate::stories::with_shortcodes(self.status.caption.read(cx).value().as_ref());
                let media = NewMedia {
                    kind: file.kind,
                    bytes: file.attachment.bytes.as_ref().clone(),
                    mime_type: file.attachment.mime.clone(),
                    file_name: Some(file.attachment.name.clone()),
                    caption: Some(caption).filter(|caption| !caption.trim().is_empty()),
                    reply_to: None,
                    mentions: Vec::new(),
                };
                self.status.draft.busy = true;
                self.status.draft.note = None;
                let engine = self.engine.clone();
                let runtime = engine.runtime().clone();
                let kept = file;
                let work = runtime.spawn_blocking({
                    let engine = engine.clone();
                    move || engine.prepare_story_media(media)
                });
                let work = runtime.spawn(work);
                let account = account.clone();
                self.status.draft.work = Some(cx.spawn_in(window, async move |this, cx| {
                    let prepared = work.await;
                    this.update_in(cx, |this, window, cx| {
                        this.status.draft.busy = false;
                        let outcome = match prepared.ok().and_then(|inner| inner.ok()) {
                            Some(Ok(prepared)) => this
                                .engine
                                .post_story_prepared(&account, prepared)
                                .map_err(|error| error.to_string()),
                            Some(Err(error)) => Err(error.to_string()),
                            None => Err("The file could not be prepared.".to_owned()),
                        };
                        match outcome {
                            Ok(_) => this.status_posted(window, cx),
                            Err(reason) => {
                                // The file stays: the user can try again.
                                this.status.draft.file = Some(kept);
                                this.status.draft.note = Some(reason.into());
                                cx.notify();
                            }
                        }
                    })
                    .ok();
                }));
                cx.notify();
            }
        }
    }

    fn status_posted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.status
            .words
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.status
            .caption
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.status.draft = Draft::default();
        self.close_status_sheet(window, cx);
        // Under "My status", where it already is.
        self.open_mine(None, cx);
    }

    // ----- the audience ----------------------------------------------------------------

    /// What the post sheet says about who will see it: a title, and the
    /// people it names.
    pub(super) fn audience_summary(&self) -> (SharedString, Option<usize>) {
        let privacy = self
            .account
            .as_ref()
            .and_then(|account| self.engine.story_privacy_cached(account).ok().flatten());
        match privacy {
            None => ("The people your privacy settings choose".into(), None),
            Some(privacy) => match privacy.audience {
                StoryAudience::Contacts => ("My contacts".into(), None),
                StoryAudience::ContactsExcept => {
                    ("My contacts except".into(), Some(privacy.except.len()))
                }
                StoryAudience::OnlyShareWith => {
                    ("Only share with".into(), Some(privacy.only.len()))
                }
            },
        }
    }

    fn set_audience_mode(&mut self, mode: StoryAudience, cx: &mut Context<Self>) {
        if let Some(edit) = self.status.draft.audience.as_mut() {
            edit.privacy.audience = mode;
            // A list that says "except" or "only" with nobody in it is
            // asked for first.
            if mode != StoryAudience::Contacts && edit.privacy.listed().is_empty() {
                edit.picking = Some(mode);
            }
        }
        cx.notify();
    }

    fn toggle_listed(&mut self, mode: StoryAudience, contact: ContactId, cx: &mut Context<Self>) {
        if let Some(edit) = self.status.draft.audience.as_mut() {
            let list = match mode {
                StoryAudience::ContactsExcept => &mut edit.privacy.except,
                StoryAudience::OnlyShareWith => &mut edit.privacy.only,
                StoryAudience::Contacts => return,
            };
            match list.iter().position(|known| *known == contact) {
                Some(at) => {
                    list.remove(at);
                }
                None => list.push(contact),
            }
        }
        cx.notify();
    }

    /// Keeps the audience: shown at once, told to the provider through
    /// drops.
    fn save_audience(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(account), Some(edit)) = (self.account.clone(), self.status.draft.audience.take())
        else {
            return;
        };
        self.engine.set_story_privacy(&account, edit.privacy);
        self.close_status_sheet(window, cx);
    }

    // ----- drawing the sheets ------------------------------------------------------------

    /// The sheet over Status.
    pub(super) fn render_status_sheet(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        Some(match self.status.sheet.clone()? {
            Sheet::Post => self.render_post_sheet(palette, cx),
            Sheet::Audience => self.render_audience_sheet(palette, cx),
            Sheet::ConfirmDelete(story) => self.render_delete_story(story, palette, cx),
            Sheet::Forward(story) => self.render_forward_story(story, palette, cx),
        })
    }

    fn render_post_sheet(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let draft = &self.status.draft;
        let (audience, count) = self.audience_summary();
        let can_edit = self.engine.capabilities().story_privacy;
        let tab = |id: &'static str, name: &'static str, on: bool| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .h(px(32.))
                .px_3()
                .rounded(metrics::RADIUS())
                .flex()
                .items_center()
                .cursor_pointer()
                .text_size(metrics::TEXT_SMALL())
                .font_weight(FontWeight::MEDIUM)
                .when(on, |this| this.bg(palette.muted).text_color(palette.text))
                .when(!on, |this| this.text_color(palette.text_muted))
                .child(name)
        };
        let body = match draft.tab {
            PostTab::Text => self.render_text_post(palette, cx).into_any_element(),
            PostTab::Media => self.render_media_post(palette, cx).into_any_element(),
        };
        let ready = !draft.busy;
        self.card("status-post", palette)
            .w(px(480.))
            .max_h(self.viewport.height - px(48.))
            .flex()
            .flex_col()
            // A file dropped on the sheet is the status.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.story_paths(paths.paths().to_vec(), cx);
            }))
            .child(super::menus::panel_header(
                "New status",
                palette,
                cx.listener(|this, _, window, cx| this.close_status_sheet(window, cx)),
            ))
            .child(
                div()
                    .id("status-sheet-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(
                                tab("post-tab-text", "Text", draft.tab == PostTab::Text).on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.set_post_tab(PostTab::Text, window, cx)
                                    }),
                                ),
                            )
                            .child(
                                tab(
                                    "post-tab-media",
                                    "Photo or video",
                                    draft.tab == PostTab::Media,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.set_post_tab(PostTab::Media, window, cx)
                                    },
                                )),
                            ),
                    )
                    .child(body)
                    .child(
                        div()
                            .debug_selector(|| "post-audience".into())
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .py_2()
                            .border_t_1()
                            .border_color(palette.border)
                            .child(
                                div()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(label("Visible to", palette))
                                    .child(
                                        div()
                                            .debug_selector(|| "post-audience-name".into())
                                            .text_size(metrics::TEXT_BODY())
                                            .child(match count {
                                                Some(n) => {
                                                    SharedString::from(format!("{audience} ({n})"))
                                                }
                                                None => audience,
                                            }),
                                    ),
                            )
                            .child(if can_edit {
                                text_button("post-audience-edit", "Change", None, false, palette)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.status.sheet = None;
                                        this.open_audience(window, cx);
                                    }))
                                    .into_any_element()
                            } else {
                                div()
                                    .debug_selector(|| "post-audience-readonly".into())
                                    .text_size(metrics::TEXT_SMALL())
                                    .text_color(palette.text_faint)
                                    .child("Change it on your phone")
                                    .into_any_element()
                            }),
                    )
                    .children(draft.note.clone().map(|note| {
                        div()
                            .debug_selector(|| "post-note".into())
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.danger)
                            .child(note)
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(palette.border)
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button("post-cancel", "Cancel", None, false, palette).on_click(
                            cx.listener(|this, _, window, cx| this.close_status_sheet(window, cx)),
                        ),
                    )
                    .child(
                        text_button(
                            "post-send",
                            if ready { "Post" } else { "Reading…" },
                            Some(IconName::SendHorizontal),
                            true,
                            palette,
                        )
                        .when(!ready, |this| this.opacity(0.5))
                        .on_click(cx.listener(|this, _, window, cx| this.post_status(window, cx))),
                    ),
            )
    }

    fn render_text_post(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let draft = &self.status.draft;
        let words = self.status.words.read(cx).value().to_string();
        let face = face_of(draft.font);
        let shown = if face.caps {
            words.to_uppercase()
        } else {
            words.clone()
        };
        let left = STORY_TEXT_MAX as i64 - words.chars().count() as i64;
        let background = draft.background;
        let font = draft.font;
        let mut swatches = div().flex().flex_wrap().gap_2();
        for (index, colour) in story::BACKGROUNDS.iter().copied().enumerate() {
            let on = colour == background;
            swatches = swatches.child(
                div()
                    .id(("swatch", index))
                    .debug_selector(move || format!("swatch-{index}"))
                    .size(story::SWATCH())
                    .rounded_full()
                    .cursor_pointer()
                    .border_2()
                    .border_color(if on { palette.text } else { palette.border })
                    .bg(story::background(colour))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.status.draft.background = colour;
                        cx.notify();
                    })),
            );
        }
        let mut fonts_row = div().flex().flex_wrap().gap_2();
        for (index, option) in StoryFont::ALL.iter().copied().enumerate() {
            let face = face_of(option);
            let on = option == font;
            fonts_row = fonts_row.child(
                with_face(
                    div()
                        .id(("font", index))
                        .debug_selector(move || format!("font-{index}"))
                        .h(px(30.))
                        .px_3()
                        .rounded(metrics::RADIUS())
                        .border_1()
                        .border_color(if on { palette.text } else { palette.border })
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(if on { palette.text } else { palette.text_muted })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.status.draft.font = option;
                            cx.notify();
                        })),
                    face,
                )
                .child(face.name),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(self.render_post_field("post-words", &self.status.words, palette, cx))
            .child(
                // What it will look like: the words on the colour, in the
                // font, as a viewer draws them.
                with_face(
                    div()
                        .debug_selector(|| "post-preview".into())
                        .h(px(200.))
                        .w_full()
                        .rounded(metrics::PANEL_RADIUS())
                        .overflow_hidden()
                        .bg(story::background(background))
                        .p_4()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_center()
                        .text_color(story::on_background())
                        .text_size(px(text_size(&words).min(28.))),
                    face,
                )
                .child(if shown.trim().is_empty() {
                    SharedString::from("Your status")
                } else {
                    SharedString::from(shown)
                }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(label("Background", palette))
                    .child(mono(format!("{left}")).text_color(if left < 0 {
                        palette.danger
                    } else {
                        palette.text_faint
                    })),
            )
            .child(swatches)
            .child(label("Font", palette))
            .child(fonts_row)
    }

    fn render_media_post(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let draft = &self.status.draft;
        let limit = self.engine.upload_limit();
        let body = match &draft.file {
            Some(file) => {
                let name = file.attachment.name.clone();
                let size = file_size(file.attachment.bytes.len() as u64);
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(match &file.preview {
                        Some(picture) => div()
                            .debug_selector(|| "post-picture".into())
                            .h(px(240.))
                            .w_full()
                            .rounded(metrics::PANEL_RADIUS())
                            .overflow_hidden()
                            .bg(palette.surface)
                            .child(
                                img(picture.clone())
                                    .size_full()
                                    .object_fit(ObjectFit::Contain),
                            ),
                        // A video: there is no decoder here, so the file
                        // is a labelled tile, and it is posted as it is.
                        None => div()
                            .debug_selector(|| "post-video".into())
                            .h(px(120.))
                            .w_full()
                            .rounded(metrics::PANEL_RADIUS())
                            .bg(palette.surface)
                            .border_1()
                            .border_color(palette.border)
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .gap_1()
                            .child(icon(IconName::Video, px(22.), palette.icon))
                            .child(
                                div()
                                    .text_size(metrics::TEXT_SMALL())
                                    .text_color(palette.text_muted)
                                    .child("Video"),
                            ),
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(metrics::TEXT_SMALL())
                                    .child(SharedString::from(name)),
                            )
                            .child(mono(size).text_color(palette.text_faint)),
                    )
                    .into_any_element()
            }
            None => div()
                .debug_selector(|| "post-drop".into())
                .h(px(160.))
                .w_full()
                .rounded(metrics::PANEL_RADIUS())
                .border_1()
                .border_color(palette.border)
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(icon(IconName::Image, px(24.), palette.icon))
                .child(
                    div()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .child("Drop a picture or a video here, or paste a picture"),
                )
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .child(body)
            .child(
                text_button(
                    "post-choose",
                    if draft.file.is_some() {
                        "Choose another file"
                    } else {
                        "Choose a file"
                    },
                    Some(IconName::Camera),
                    false,
                    palette,
                )
                .on_click(cx.listener(|this, _, _, cx| this.pick_story_file(cx))),
            )
            .child(self.render_post_field("post-caption", &self.status.caption, palette, cx))
            .children(limit.map(|limit| {
                div()
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_faint)
                    .child(SharedString::from(format!("Up to {}.", file_size(limit))))
            }))
    }

    /// A field of the post sheet with what the composer has: the emoji a
    /// shortcode stands for over it, and the emoji picker's button.
    fn render_post_field(
        &self,
        selector: &'static str,
        field: &gpui_kit::Entity<gpui_kit::component::input::TextareaState>,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_1()
            // While a shortcode is typed, Escape is the list's before it
            // is the sheet's.
            .capture_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                this.emoji_completion_escape(event, cx);
            }))
            .children(self.render_emoji_completion(palette, cx))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .debug_selector(move || selector.into())
                            .child(Textarea::new(field).small()),
                    )
                    .child(self.render_status_emoji_button(palette, cx)),
            )
    }

    // ----- who sees ----------------------------------------------------------------------------

    fn render_audience_sheet(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let caps = self.engine.capabilities();
        let editable = caps.story_privacy_edit;
        let Some(edit) = self.status.draft.audience.as_ref() else {
            return self.card("status-audience", palette);
        };
        let account = self.account.clone().unwrap_or_default_account();
        let name_of = |id: &ContactId| -> String {
            self.engine
                .store()
                .person(&account, None, id)
                .ok()
                .and_then(|person| person.shown().map(str::to_owned))
                .unwrap_or_else(|| client_core::UNKNOWN_PERSON.to_owned())
        };
        let mode_row =
            |index: usize, mode: StoryAudience, title: &'static str, about: &'static str| {
                let on = edit.privacy.audience == mode;
                let listed = match mode {
                    StoryAudience::Contacts => None,
                    StoryAudience::ContactsExcept => Some(edit.privacy.except.len()),
                    StoryAudience::OnlyShareWith => Some(edit.privacy.only.len()),
                };
                div()
                    .id(("audience-mode", index))
                    .debug_selector(move || format!("audience-mode-{index}"))
                    .px_3()
                    .py_2()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(if on { palette.text } else { palette.border })
                    .flex()
                    .items_center()
                    .gap_3()
                    .when(editable, |this| this.cursor_pointer())
                    .child(icon(
                        if on {
                            IconName::CircleCheck
                        } else {
                            IconName::Circle
                        },
                        px(18.),
                        if on {
                            palette.accent
                        } else {
                            palette.text_faint
                        },
                    ))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(metrics::TEXT_BODY())
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(match listed {
                                        Some(n) => SharedString::from(format!("{title} ({n})")),
                                        None => SharedString::from(title),
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(metrics::TEXT_SMALL())
                                    .text_color(palette.text_muted)
                                    .child(about),
                            ),
                    )
                    .when(listed.is_some() && editable, |this| {
                        let pick: &'static str = match index {
                            1 => "audience-pick-1",
                            _ => "audience-pick-2",
                        };
                        this.child(text_button(pick, "Edit", None, false, palette).on_click(
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                if let Some(edit) = this.status.draft.audience.as_mut() {
                                    edit.privacy.audience = mode;
                                    edit.picking = Some(mode);
                                    edit.query.clear();
                                }
                                cx.notify();
                            }),
                        ))
                    })
                    .when(editable, |this| {
                        this.on_click(
                            cx.listener(move |this, _, _, cx| this.set_audience_mode(mode, cx)),
                        )
                    })
            };
        let picking = edit.picking;
        let list: Div = match picking {
            Some(mode) => self.render_audience_picker(mode, edit, palette, cx),
            None => {
                let rows = div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(mode_row(
                        0,
                        StoryAudience::Contacts,
                        "My contacts",
                        "Everyone in your address book.",
                    ))
                    .child(mode_row(
                        1,
                        StoryAudience::ContactsExcept,
                        "My contacts except…",
                        "Everyone but the people you pick.",
                    ))
                    .child(mode_row(
                        2,
                        StoryAudience::OnlyShareWith,
                        "Only share with…",
                        "Only the people you pick.",
                    ));
                let named: Vec<SharedString> = edit
                    .privacy
                    .listed()
                    .iter()
                    .take(6)
                    .map(|id| SharedString::from(name_of(id)))
                    .collect();
                rows.when(!named.is_empty(), |this| {
                    this.child(
                        div()
                            .debug_selector(|| "audience-named".into())
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text_muted)
                            .child(SharedString::from(
                                named
                                    .iter()
                                    .map(|name| name.to_string())
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            )),
                    )
                })
            }
        };
        self.card("status-audience", palette)
            .w(px(480.))
            .max_h(self.viewport.height - px(48.))
            .flex()
            .flex_col()
            .child(super::menus::panel_header(
                "Who sees my status",
                palette,
                cx.listener(|this, _, window, cx| this.close_status_sheet(window, cx)),
            ))
            .child(
                div()
                    .id("status-sheet-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(list)
                    // Read-only where the provider cannot change it.
                    .when(!editable, |this| {
                        this.child(
                            div()
                                .debug_selector(|| "audience-readonly".into())
                                .text_size(metrics::TEXT_SMALL())
                                .line_height(px(19.))
                                .text_color(palette.text_muted)
                                .child(
                                    "Changing who sees your status is not available yet from \
                                     this provider. This is how it is set on your phone; change \
                                     it there.",
                                ),
                        )
                    }),
            )
            .child(
                div()
                    .flex_none()
                    .px_4()
                    .py_3()
                    .border_t_1()
                    .border_color(palette.border)
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        text_button(
                            "audience-cancel",
                            if editable { "Cancel" } else { "Close" },
                            None,
                            false,
                            palette,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.dismiss_status_sheet(window, cx)
                            }),
                        ),
                    )
                    .when(editable && picking.is_none(), |this| {
                        this.child(
                            text_button("audience-save", "Save", None, true, palette).on_click(
                                cx.listener(|this, _, window, cx| this.save_audience(window, cx)),
                            ),
                        )
                    })
                    .when(editable && picking.is_some(), |this| {
                        this.child(
                            text_button("audience-done", "Done", None, true, palette).on_click(
                                cx.listener(|this, _, _, cx| {
                                    if let Some(edit) = this.status.draft.audience.as_mut() {
                                        edit.picking = None;
                                    }
                                    cx.notify();
                                }),
                            ),
                        )
                    }),
            )
    }

    /// The people of the address book, with a check beside each one the
    /// list names.
    fn render_audience_picker(
        &self,
        mode: StoryAudience,
        edit: &AudienceEdit,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let account = self.account.clone().unwrap_or_default_account();
        let contacts = self
            .engine
            .store()
            .contacts(&account, Some(edit.query.as_str()), 60)
            .unwrap_or_default();
        let named: &[ContactId] = match mode {
            StoryAudience::ContactsExcept => &edit.privacy.except,
            _ => &edit.privacy.only,
        };
        let mut rows = div().flex().flex_col();
        for (index, contact) in contacts.iter().enumerate() {
            let on = named.contains(&contact.id);
            let id = contact.id.clone();
            rows = rows.child(
                div()
                    .id(("audience-contact", index))
                    .debug_selector(move || format!("audience-contact-{index}"))
                    .h(px(40.))
                    .px_2()
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .hover({
                        let hover = palette.hover;
                        move |style| style.bg(hover)
                    })
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_listed(mode, id.clone(), cx)),
                    )
                    .child(icon(
                        if on {
                            IconName::SquareCheck
                        } else {
                            IconName::Square
                        },
                        px(18.),
                        if on {
                            palette.accent
                        } else {
                            palette.text_faint
                        },
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_BODY())
                            .child(SharedString::from(contact.display_name())),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap_2()
            .child(label(
                if mode == StoryAudience::ContactsExcept {
                    "Hide my status from"
                } else {
                    "Share my status only with"
                },
                palette,
            ))
            .child(
                div().debug_selector(|| "audience-query".into()).child(
                    gpui_kit::component::input::Input::new(&self.status.query)
                        .small()
                        .py_0(),
                ),
            )
            .child(rows)
            .when(contacts.is_empty(), |this| {
                this.child(
                    div()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .child("No contacts to pick from yet."),
                )
            })
    }

    fn render_delete_story(
        &self,
        story: MessageId,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let account = self.account.clone();
        self.card("status-delete", palette)
            .w(px(360.))
            .flex()
            .flex_col()
            .child(super::menus::panel_header(
                "Delete status?",
                palette,
                cx.listener(|this, _, window, cx| this.close_status_sheet(window, cx)),
            ))
            .child(
                div()
                    .p_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_size(metrics::TEXT_SMALL())
                            .line_height(px(19.))
                            .text_color(palette.text_muted)
                            .child(
                                "It is taken down for everyone who could see it, including the \
                                 people who already did.",
                            ),
                    )
                    .child(
                        text_button(
                            "status-delete-yes",
                            "Delete",
                            Some(IconName::Trash),
                            false,
                            palette,
                        )
                        .text_color(palette.danger)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                if let Some(account) = &account {
                                    this.delete_my_story(account.clone(), story.clone(), cx);
                                }
                                this.close_status_sheet(window, cx);
                            },
                        )),
                    )
                    .child(
                        text_button("status-delete-no", "Cancel", None, false, palette).on_click(
                            cx.listener(|this, _, window, cx| this.close_status_sheet(window, cx)),
                        ),
                    ),
            )
    }

    fn render_forward_story(
        &self,
        story: MessageId,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        // Whether it can be passed on is the engine's to say, as for a
        // message: a text always, anything else where the provider
        // forwards by naming the original.
        let refusal = match &self.account {
            Some(account) => self
                .engine
                .story_forward_refusal(account, &story)
                .unwrap_or(Some(client_core::ForwardRefusal::Deleted)),
            None => Some(client_core::ForwardRefusal::Deleted),
        };
        let mut chats = div().flex().flex_col();
        match refusal {
            None => {
                for (index, row) in self
                    .list_rows
                    .iter()
                    .filter_map(|row| match row {
                        super::shell::ListRow::Chat(chat) => Some(chat.clone()),
                        _ => None,
                    })
                    .take(12)
                    .enumerate()
                {
                    let chat = row.id.clone();
                    let story = story.clone();
                    chats = chats.child(
                        div()
                            .id(("forward-chat", index))
                            .debug_selector(move || format!("forward-chat-{index}"))
                            .h(px(40.))
                            .px_2()
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .cursor_pointer()
                            .hover({
                                let hover = palette.hover;
                                move |style| style.bg(hover)
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.forward_story(&story, chat.clone(), cx);
                                this.close_status_sheet(window, cx);
                            }))
                            .child(SharedString::from(row.title.clone())),
                    );
                }
            }
            Some(refusal) => {
                chats = chats.child(
                    div()
                        .debug_selector(|| "status-forward-refused".into())
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .child(format!("{}.", refusal.reason())),
                );
            }
        }
        self.card("status-forward", palette)
            .w(px(380.))
            .max_h(self.viewport.height - px(48.))
            .flex()
            .flex_col()
            .child(super::menus::panel_header(
                "Forward to",
                palette,
                cx.listener(|this, _, window, cx| this.close_status_sheet(window, cx)),
            ))
            .child(
                div()
                    .id("forward-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_2()
                    .child(chats),
            )
    }
}

/// An account id that is empty when there is none (a story of nobody).
trait OrDefaultAccount {
    fn unwrap_or_default_account(self) -> client_provider::AccountId;
}

impl OrDefaultAccount for Option<client_provider::AccountId> {
    fn unwrap_or_default_account(self) -> client_provider::AccountId {
        self.unwrap_or_else(|| client_provider::AccountId::new(""))
    }
}
