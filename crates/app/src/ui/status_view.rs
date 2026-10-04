//! Viewing a status.
//!
//! The viewer takes the main pane: the author in a bar on top, a segment
//! of progress for each of their stories, the story on a card, and under
//! it the way to answer (a reaction, a reply). What happens when is the
//! state machine in `crate::stories` ([`Player`]); this file feeds it time
//! (a timer that only runs while a story is on screen), tells it when the
//! story's content is on screen, and acts on what it says.
//!
//! Seeing is behaviour toward other people. A story counts as seen, and
//! its author is owed a view receipt when receipts are on, only when it was
//! drawn in this viewer: never because it was listed, prefetched or
//! downloaded. A video counts when a frame of it is on screen here. Until
//! its file is here it is only a tile, and the tile is not a view.

use super::shell::Shell;
use super::status::{tooltip_with_keys, Main, Part};
use super::status_post::{with_face, Sheet};
use super::transfer::{transfer_button, Transfer};
use super::widgets::{icon_button, label, mono, text_button};
use crate::format::{duration as clock_time, file_size};
use crate::icons::{icon, IconName};
use crate::keys::Command;
use crate::markup::{self, Block};
use crate::stories::{
    age_label, expires_label, face_of, text_size, Moment, Player, Reel, SlideKind,
};
use crate::theme::{metrics, px, story, Palette};
use client_core::{StoryAuthor, StoryItem};
use client_provider::{AccountId, ContactId, Media, MessageId, StoryBody, Timestamp};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::*;
use gpui_kit::Focusable as _;
use gpui_kit::{
    div, img, Context, Div, FontStyle, FontWeight, HighlightStyle, InteractiveText, ObjectFit,
    SharedString, Stateful, StrikethroughStyle, StyledImage, StyledText, UnderlineStyle, Window,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

/// What the viewer says when a story's video cannot be played here.
const VIDEO_FAILED: &str = "This video can't be played in the app.";

/// How often the viewer's clock ticks.
pub(super) const TICK: Duration = Duration::from_millis(40);
/// The same with motion reduced: the bar moves in steps.
pub(super) const TICK_STILL: Duration = Duration::from_millis(250);

/// The quick reactions under a story.
pub(super) const REACTIONS: [&str; 6] = ["❤️", "😂", "😮", "😢", "🙏", "👍"];

/// What the panel beside the story shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Panel {
    /// Who saw it (the account's own), or when it was posted and goes.
    Info,
}

/// The author of a reel, for the bar.
#[derive(Clone, Debug)]
pub(super) struct ViewAuthor {
    pub(super) key: String,
    pub(super) author: ContactId,
    pub(super) name: String,
    pub(super) mine: bool,
}

/// A status being viewed.
pub(super) struct Viewing {
    pub(super) player: Player,
    /// The author of each reel.
    pub(super) authors: Vec<ViewAuthor>,
    /// The stories as they were when the viewer opened: a story that
    /// expires meanwhile is still drawn from here until the player skips
    /// it.
    pub(super) items: HashMap<MessageId, StoryItem>,
    pub(super) panel: Option<Panel>,
    /// What was last said about a reply or a reaction, for a moment.
    pub(super) notice: Option<SharedString>,
    /// Why the reply was not sent.
    pub(super) reply_note: Option<SharedString>,
    /// The reaction the account just sent, per story.
    pub(super) reacted: HashMap<MessageId, &'static str>,
}

impl Viewing {
    pub(super) fn author(&self) -> Option<&ViewAuthor> {
        self.authors.get(self.player.reel_index())
    }

    pub(super) fn item(&self) -> Option<&StoryItem> {
        self.items.get(&self.player.current()?.id)
    }
}

fn view_author(author: &StoryAuthor) -> ViewAuthor {
    ViewAuthor {
        key: author.key.clone(),
        author: author.author.clone(),
        name: author.display_name(),
        mine: false,
    }
}

impl Shell {
    // ----- opening and closing ------------------------------------------------------

    /// Opens the stories of an author: from their first one not seen on,
    /// and on through the next authors of the list.
    pub(super) fn open_story_viewer(
        &mut self,
        key: &str,
        part: Part,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let feed = &self.status.feed;
        // Played in the order the list shows: what is new, then what was
        // seen. The muted ones are only watched on purpose, one author.
        let authors: Vec<&StoryAuthor> = if part == Part::Muted {
            feed.muted
                .iter()
                .filter(|author| author.key == key)
                .collect()
        } else {
            feed.recent.iter().chain(feed.viewed.iter()).collect()
        };
        let Some(start) = authors.iter().position(|author| author.key == key) else {
            return;
        };
        let reels: Vec<Reel> = authors.iter().map(|author| Reel::of(author)).collect();
        let slide = authors[start].start_index();
        let mut items = HashMap::new();
        let mut shown_authors = Vec::new();
        for author in &authors {
            for item in &author.stories {
                items.insert(item.story.id.clone(), item.clone());
            }
            shown_authors.push(view_author(author));
        }
        self.begin_story_viewing(
            Player::new(reels, start, slide),
            shown_authors,
            items,
            window,
            cx,
        );
    }

    /// Plays the account's own stories, from the first (or the one given).
    pub(super) fn open_own_story_viewer(
        &mut self,
        from: Option<MessageId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(account) = self.account.clone() else {
            return;
        };
        let mine: Vec<StoryItem> = self
            .status
            .feed
            .mine
            .iter()
            .filter(|item| item.post.is_none())
            .cloned()
            .collect();
        if mine.is_empty() {
            return;
        }
        let me = self
            .accounts
            .iter()
            .find(|known| known.id == account)
            .map(|known| known.display_name.clone())
            .unwrap_or_default();
        let slide = from
            .and_then(|id| mine.iter().position(|item| item.story.id == id))
            .unwrap_or(0);
        let reel = Reel {
            key: "me".into(),
            slides: mine.iter().map(crate::stories::Slide::of).collect(),
        };
        let author = ViewAuthor {
            key: "me".into(),
            author: mine[0].story.author.clone(),
            name: if me.is_empty() {
                "My status".into()
            } else {
                me
            },
            mine: true,
        };
        let items = mine
            .into_iter()
            .map(|item| (item.story.id.clone(), item))
            .collect();
        self.begin_story_viewing(
            Player::new(vec![reel], 0, slide),
            vec![author],
            items,
            window,
            cx,
        );
    }

    fn begin_story_viewing(
        &mut self,
        player: Player,
        authors: Vec<ViewAuthor>,
        items: HashMap<MessageId, StoryItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.status.viewing = Some(Viewing {
            player,
            authors,
            items,
            panel: None,
            notice: None,
            reply_note: None,
            reacted: HashMap::new(),
        });
        self.status.main = Main::Viewing;
        self.status.sheet = None;
        self.status
            .reply
            .update(cx, |field, cx| field.set_value("", window, cx));
        // The keyboard is the viewer's: the shell holds it.
        self.focus.focus(window, cx);
        self.pane_list = false;
        self.start_story_clock(cx);
        self.ask_viewers_of_current(cx);
        // A GIF on screen is fetched now, the way a picture is. The clock
        // waits for the file; nothing is handed to the system player.
        self.ensure_story_video(cx);
        cx.notify();
    }

    /// Closes the viewer: the list is as it was, with what was seen moved.
    pub(super) fn close_story_viewer(&mut self, cx: &mut Context<Self>) {
        if self.status.viewing.take().is_some() {
            self.status.clock = None;
            self.status.video_pending = None;
            self.status.video_error = None;
            self.retire_story_video(cx);
            if self.status.main == Main::Viewing {
                self.status.main = Main::Empty;
            }
            cx.notify();
        }
    }

    /// Watches one story, wherever the link to it was (a reply that
    /// quotes it): its author's stories open on it.
    pub(super) fn watch_story(
        &mut self,
        story: &MessageId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use super::status::ListMode;
        self.set_list_mode(ListMode::Status, window, cx);
        if self
            .status
            .feed
            .mine
            .iter()
            .any(|item| &item.story.id == story)
        {
            return self.open_own_story_viewer(Some(story.clone()), window, cx);
        }
        let found = self
            .status
            .feed
            .recent
            .iter()
            .map(|author| (Part::Recent, author))
            .chain(
                self.status
                    .feed
                    .viewed
                    .iter()
                    .map(|author| (Part::Viewed, author)),
            )
            .chain(
                self.status
                    .feed
                    .muted
                    .iter()
                    .map(|author| (Part::Muted, author)),
            )
            .find(|(_, author)| author.stories.iter().any(|item| &item.story.id == story))
            .map(|(part, author)| {
                (
                    part,
                    author.key.clone(),
                    author
                        .stories
                        .iter()
                        .position(|item| &item.story.id == story),
                )
            });
        let Some((part, key, at)) = found else {
            return;
        };
        self.open_story_viewer(&key, part, window, cx);
        // On the story that was asked for, not the first not seen.
        if let (Some(at), Some(viewing)) = (at, self.status.viewing.as_mut()) {
            let reel = viewing.player.reel_index();
            let reels = viewing.player.reels().to_vec();
            viewing.player = Player::new(reels, reel, at);
        }
    }

    // ----- the clock ------------------------------------------------------------------------

    fn start_story_clock(&mut self, cx: &mut Context<Self>) {
        let step = if crate::settings::reduce_motion(cx) {
            TICK_STILL
        } else {
            TICK
        };
        self.status.clock = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(step).await;
            let alive = this.update(cx, |this, cx| this.story_tick(step, cx));
            if !matches!(alive, Ok(true)) {
                return;
            }
        }));
    }

    /// Whether the content of the story on screen is drawn: a text at
    /// once, a picture when it has arrived, a tile always.
    fn story_content_ready(&self) -> bool {
        let Some(viewing) = &self.status.viewing else {
            return false;
        };
        let Some(item) = viewing.item() else {
            return false;
        };
        match &item.story.body {
            StoryBody::Text { .. } => true,
            StoryBody::Media(media) => match SlideKind::of(&item.story.body) {
                SlideKind::Image => self.account.as_ref().is_some_and(|account| {
                    matches!(
                        self.media.visual(account, media),
                        super::media::MediaVisual::Image(_)
                    )
                }),
                _ => true,
            },
        }
    }

    /// Lets `by` pass on the viewer's clock. Returns whether the viewer is
    /// still open (the timer stops when it is not).
    pub(super) fn story_tick(&mut self, by: Duration, cx: &mut Context<Self>) -> bool {
        if self.status.viewing.is_none() {
            return false;
        }
        self.ensure_story_video(cx);
        let user_paused = self.story_paused_by_user();
        if let Some((_, clip)) = &self.status.clip {
            clip.set_paused(user_paused);
            clip.poll(by);
        }
        let ready = self.story_content_ready();
        let video = self.current_slide_kind() == Some(SlideKind::Video);
        let report = self.current_video_report();
        let moments = {
            let viewing = self.status.viewing.as_mut().expect("checked above");
            let mut moments = Vec::new();
            if !video {
                if viewing.player.is_ready() != ready {
                    moments.extend(viewing.player.set_ready(ready));
                }
                moments.extend(viewing.player.tick(by));
            } else if let Some(report) = report {
                if !viewing.player.is_ready() {
                    moments.extend(viewing.player.set_ready(true));
                }
                if report.failure.is_some() {
                    // The note stays up. The story still moves on, so a
                    // file that will not play cannot hold the viewer.
                    moments.extend(viewing.player.tick(by));
                } else if report.finished {
                    if report.image.is_some() {
                        moments.extend(viewing.player.presented());
                    }
                    moments.extend(viewing.player.next_story());
                } else if report.image.is_some() {
                    moments.extend(viewing.player.presented());
                    moments.extend(viewing.player.sync(report.position));
                }
                // No frame yet: the decoder is working. The clock stays
                // where it is, and the decoder is not paused for that.
            } else {
                if !viewing.player.is_ready() {
                    moments.extend(viewing.player.set_ready(true));
                }
                moments.extend(viewing.player.tick(by));
            }
            moments
        };
        if self.apply_moments(moments, cx) {
            cx.notify();
        }
        // The story just moved to asks for its file now, not one tick later.
        if self.status.viewing.is_some() {
            self.ensure_story_video(cx);
        }
        self.status.viewing.is_some()
    }

    /// Acts on what the player said. Returns whether anything changed
    /// that wants a repaint.
    fn apply_moments(&mut self, moments: Vec<Moment>, cx: &mut Context<Self>) -> bool {
        let mut changed = !moments.is_empty() || self.story_progressing();
        for moment in moments {
            match moment {
                Moment::Shown(id) => {
                    if let Some(account) = self.account.clone() {
                        if let Err(error) = self.engine.story_shown(&account, &id) {
                            tracing::warn!(%error, "could not record that a story was seen");
                        }
                    }
                }
                Moment::Moved => {
                    if let Some(viewing) = self.status.viewing.as_mut() {
                        viewing.panel = None;
                        viewing.notice = None;
                        viewing.reply_note = None;
                    }
                    self.status.video_pending = None;
                    self.status.video_error = None;
                    self.retire_story_video(cx);
                    self.drop_story_draft(cx);
                    self.ask_viewers_of_current(cx);
                }
                Moment::Ended => {
                    self.close_story_viewer(cx);
                    changed = true;
                }
            }
        }
        changed
    }

    /// Another story is on screen: what was being written under the one
    /// before was for that one, and goes. A reply is sent to the story on
    /// screen when it is sent, so words left in the field would reach
    /// somebody they were not written to.
    fn drop_story_draft(&mut self, cx: &mut Context<Self>) {
        if self.status.reply.read(cx).value().is_empty() {
            return;
        }
        // The emoji offered for a shortcode in it go with it.
        self.emoji_completion_reset();
        let (field, window) = (self.status.reply.clone(), self.status.window);
        // A text field is written to with its window, and the clock that
        // moves the story on has none: right after this, then, and before
        // anything else can happen in the window.
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, cx| {
                field.update(cx, |field, cx| field.set_value("", window, cx));
            });
        });
    }

    fn story_progressing(&self) -> bool {
        self.status
            .viewing
            .as_ref()
            .is_some_and(|viewing| viewing.player.running())
    }

    /// The reply field has the keyboard (or let go of it): the clock waits.
    pub(super) fn story_typing(&mut self, typing: bool, cx: &mut Context<Self>) {
        // The emoji picker the reply opened took the keyboard: the reply
        // is still being written, and the story waits for it.
        let picking = self.overlay == super::shell::Overlay::EmojiPicker
            && self.emoji.target == super::emoji_picker::Target::Status;
        if let Some(viewing) = self.status.viewing.as_mut() {
            viewing.player.holds.typing = typing || picking;
            cx.notify();
        }
    }

    /// The feed changed under the viewer: a story that is gone (it
    /// expired, or was taken down) is skipped.
    pub(super) fn story_feed_changed(&mut self, cx: &mut Context<Self>) {
        let Some(viewing) = self.status.viewing.as_ref() else {
            return;
        };
        let alive: std::collections::HashSet<&MessageId> = self
            .status
            .feed
            .authors()
            .flat_map(|author| author.stories.iter())
            .chain(self.status.feed.mine.iter())
            .map(|item| &item.story.id)
            .collect();
        let gone: Vec<MessageId> = viewing
            .player
            .reels()
            .iter()
            .flat_map(|reel| reel.slides.iter())
            .map(|slide| slide.id.clone())
            .filter(|id| !alive.contains(id))
            .collect();
        if gone.is_empty() {
            return;
        }
        let mut moments = Vec::new();
        if let Some(viewing) = self.status.viewing.as_mut() {
            for id in &gone {
                moments.extend(viewing.player.forget(id));
            }
        }
        self.apply_moments(moments, cx);
        cx.notify();
    }

    // ----- commands --------------------------------------------------------------------------

    /// Runs a command of the viewer.
    pub(super) fn run_story_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        use Command as C;
        self.status.viewing.as_ref()?;
        let moments = match command {
            C::StoryNext => self.player_step(|player| player.next_story()),
            C::StoryPrevious => self.player_step(|player| player.previous_story()),
            C::StoryNextAuthor => self.player_step(|player| player.next_author()),
            C::StoryPreviousAuthor => self.player_step(|player| player.previous_author()),
            C::StoryPause => {
                let viewing = self.status.viewing.as_mut()?;
                viewing.player.holds.paused = !viewing.player.holds.paused;
                Vec::new()
            }
            C::StoryClose => {
                // In the reply field, Escape leaves it first.
                if self.status.reply.focus_handle(cx).is_focused(window) {
                    self.focus.focus(window, cx);
                    self.story_typing(false, cx);
                    return Some(true);
                }
                self.close_story_viewer(cx);
                return Some(true);
            }
            C::StoryMute => {
                let key = self.status.viewing.as_ref()?.author()?.key.clone();
                if self.status.viewing.as_ref()?.author()?.mine {
                    return Some(false);
                }
                self.toggle_mute_of(&key, cx);
                Vec::new()
            }
            C::StoryInfo => {
                let viewing = self.status.viewing.as_mut()?;
                viewing.panel = match viewing.panel {
                    Some(Panel::Info) => None,
                    None => Some(Panel::Info),
                };
                viewing.player.holds.panel = viewing.panel.is_some();
                Vec::new()
            }
            C::StoryReply => {
                if !self.caps_now().story_reply || self.status.viewing.as_ref()?.author()?.mine {
                    return Some(false);
                }
                let field = self.status.reply.clone();
                field.update(cx, |field, cx| field.focus(window, cx));
                // Typing: the clock waits (the field says so again when
                // it hears the focus arrive, and when it leaves).
                self.story_typing(true, cx);
                Vec::new()
            }
            C::StoryReact => {
                self.react_to_current("❤️", cx);
                Vec::new()
            }
            C::StoryOpenVideo => {
                self.open_current_media(cx);
                Vec::new()
            }
            C::StoryDelete => {
                let viewing = self.status.viewing.as_ref()?;
                if !viewing.author()?.mine {
                    return Some(false);
                }
                let id = viewing.player.current()?.id.clone();
                self.status.sheet = Some(Sheet::ConfirmDelete(id));
                self.open_overlay(super::shell::Overlay::Status, window, cx);
                Vec::new()
            }
            _ => return None,
        };
        self.apply_moments(moments, cx);
        cx.notify();
        Some(true)
    }

    fn player_step(&mut self, step: impl FnOnce(&mut Player) -> Vec<Moment>) -> Vec<Moment> {
        match self.status.viewing.as_mut() {
            Some(viewing) => step(&mut viewing.player),
            None => Vec::new(),
        }
    }

    /// The story on screen, with its media, if it has any.
    fn current_media(&self) -> Option<(MessageId, Media)> {
        let viewing = self.status.viewing.as_ref()?;
        let item = viewing.item()?;
        match &item.story.body {
            StoryBody::Media(media) => Some((item.story.id.clone(), media.clone())),
            StoryBody::Text { .. } => None,
        }
    }

    /// A video in this viewer, a voice note in the speakers. A video counts
    /// as seen when a frame is on screen, a voice note when it plays.
    pub(super) fn open_current_media(&mut self, cx: &mut Context<Self>) {
        let Some((id, media)) = self.current_media() else {
            return;
        };
        let Some(url) = media.source.as_ref().map(|source| source.to_string()) else {
            return;
        };
        match crate::stories::SlideKind::of(&StoryBody::Media(media.clone())) {
            SlideKind::Voice => {
                self.toggle_audio(media, cx);
                let moments = self
                    .status
                    .viewing
                    .as_mut()
                    .map(|viewing| viewing.player.opened_externally())
                    .unwrap_or_default();
                self.apply_moments(moments, cx);
            }
            SlideKind::Video => {
                let ready = self.media.file_state(&url) == super::media::FileState::Ready;
                if ready {
                    // A retry after a failure starts again. The file is
                    // already here: nothing is handed to the system.
                    self.status.video_error = None;
                    self.retire_story_video(cx);
                    self.play_story_video(id, &url, cx);
                    self.present_story_video(cx);
                } else if let Some(account) = self.account.clone() {
                    // Ask for the file without marking it as something the
                    // system player is about to open.
                    self.engine.want_file(&account, &url);
                    self.status.video_pending = Some((id, url));
                    if let Some(viewing) = self.status.viewing.as_mut() {
                        viewing.player.holds.loading = true;
                    }
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// A file arrived. A story video that was waited for starts here, and
    /// counts as seen once a frame is up. It is never opened outside.
    pub(super) fn status_media_arrived(&mut self, key: &str, cx: &mut Context<Self>) {
        let url = if let Some((_, url)) = self.status.video_pending.clone() {
            url
        } else {
            let Some((_, media)) = self.current_media() else {
                return;
            };
            if SlideKind::of(&StoryBody::Media(media.clone())) != SlideKind::Video {
                return;
            }
            let Some(url) = media.source.as_ref().map(|source| source.to_string()) else {
                return;
            };
            url
        };
        if key != client_core::file_key(&url) {
            return;
        }
        self.ensure_story_video(cx);
        self.present_story_video(cx);
        cx.notify();
    }

    /// The kind of the story on screen.
    fn current_slide_kind(&self) -> Option<SlideKind> {
        self.status
            .viewing
            .as_ref()
            .and_then(|viewing| viewing.player.current())
            .map(|slide| slide.kind)
    }

    /// Holds that are the person's, not the decoder's. Pausing for a
    /// download or for the first frame would stop the decoder before a
    /// frame existed.
    fn story_paused_by_user(&self) -> bool {
        self.status.viewing.as_ref().is_some_and(|viewing| {
            let holds = &viewing.player.holds;
            holds.pointer || holds.paused || holds.typing || holds.external || holds.panel
        })
    }

    /// The frame of the clip that belongs to the story on screen.
    fn current_video_report(&self) -> Option<crate::video::Report> {
        let id = self
            .status
            .viewing
            .as_ref()
            .and_then(|viewing| viewing.player.current())
            .map(|slide| slide.id.clone())?;
        let (clip_id, clip) = self.status.clip.as_ref()?;
        (clip_id == &id).then(|| clip.report())
    }

    /// Starts playback when the story on screen is a video whose file is
    /// already cached. A failure stays failed until the person tries again.
    fn ensure_story_video(&mut self, cx: &mut Context<Self>) {
        let Some((id, media)) = self.current_media() else {
            self.retire_story_video(cx);
            return;
        };
        if SlideKind::of(&StoryBody::Media(media.clone())) != SlideKind::Video {
            self.status.video_error = None;
            self.retire_story_video(cx);
            return;
        }
        if self
            .status
            .clip
            .as_ref()
            .is_some_and(|(clip_id, _)| clip_id == &id)
        {
            return;
        }
        if self
            .status
            .video_error
            .as_ref()
            .is_some_and(|(err_id, _)| err_id == &id)
        {
            return;
        }
        let Some(url) = media.source.as_ref().map(|source| source.to_string()) else {
            return;
        };
        if self.media.file_state(&url) != super::media::FileState::Ready {
            self.ask_for_story_gif(&id, &media, &url);
            return;
        }
        self.play_story_video(id, &url, cx);
    }

    /// A GIF the policy fetches by itself: ask for the file and hold the
    /// clock. A video that is not a GIF still waits for the Play button.
    /// Neither path marks the file as something the system player will open.
    fn ask_for_story_gif(&mut self, id: &MessageId, media: &Media, url: &str) {
        if !self.media.fetches_gif_unasked(media) {
            return;
        }
        if self
            .status
            .video_error
            .as_ref()
            .is_some_and(|(err_id, _)| err_id == id)
        {
            return;
        }
        if self
            .status
            .video_pending
            .as_ref()
            .is_some_and(|(pending, _)| pending == id)
        {
            if let Some(viewing) = self.status.viewing.as_mut() {
                viewing.player.holds.loading = true;
            }
            return;
        }
        let Some(account) = self.account.clone() else {
            return;
        };
        self.engine.want_file(&account, url);
        self.status.video_pending = Some((id.clone(), url.to_owned()));
        if let Some(viewing) = self.status.viewing.as_mut() {
            viewing.player.holds.loading = true;
        }
    }

    /// Opens `url` in the in-app player. The clock restarts: time spent on
    /// the tile is not time in the video.
    fn play_story_video(&mut self, id: MessageId, url: &str, cx: &mut Context<Self>) {
        self.retire_story_video(cx);
        let Some((bytes, _)) = self.media.file(url) else {
            return;
        };
        if bytes.is_empty() {
            tracing::warn!("the video file is empty");
            self.story_video_failed(id);
            return;
        }
        match crate::video::Clip::open(Arc::<[u8]>::from(bytes)) {
            Ok(clip) => {
                self.status.clip = Some((id, Arc::new(clip)));
                self.status.video_error = None;
                self.status.video_pending = None;
                if let Some(viewing) = self.status.viewing.as_mut() {
                    viewing.player.holds.loading = false;
                    viewing.player.rewind();
                }
            }
            Err(error) => {
                tracing::warn!(%error, "story video could not be opened");
                self.story_video_failed(id);
            }
        }
    }

    fn story_video_failed(&mut self, id: MessageId) {
        self.status.video_error = Some((id, VIDEO_FAILED.into()));
        self.status.video_pending = None;
        if let Some(viewing) = self.status.viewing.as_mut() {
            viewing.player.holds.loading = false;
        }
    }

    /// A frame is up: the story counts as seen. The clock follows playback
    /// on the next tick, not here, so arriving at the start does not skip
    /// the story.
    fn present_story_video(&mut self, cx: &mut Context<Self>) {
        let Some(report) = self.current_video_report() else {
            return;
        };
        if report.image.is_none() || report.failure.is_some() {
            return;
        }
        let moments = self
            .status
            .viewing
            .as_mut()
            .map(|viewing| viewing.player.presented())
            .unwrap_or_default();
        self.apply_moments(moments, cx);
    }

    /// Stops the decoder and gives its frames back to the window.
    fn retire_story_video(&mut self, cx: &mut Context<Self>) {
        let Some((_, clip)) = self.status.clip.take() else {
            return;
        };
        let mut images = clip.take_stale();
        if let Some(image) = clip.report().image {
            images.push(image);
        }
        let window = self.status.window;
        cx.defer(move |cx| {
            let _ = window.update(cx, |_, window, _| {
                for image in images {
                    let _ = window.drop_image(image);
                }
            });
        });
    }

    // ----- answering ---------------------------------------------------------------------------

    /// Sends the reply that is written, to the author of the story on
    /// screen, quoting the story.
    pub(super) fn send_story_reply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(account), Some(viewing)) = (self.account.clone(), self.status.viewing.as_ref())
        else {
            return;
        };
        let Some(slide) = viewing.player.current() else {
            return;
        };
        let id = slide.id.clone();
        let text = crate::stories::with_shortcodes(self.status.reply.read(cx).value().as_ref());
        match self.engine.reply_to_story(&account, &id, &text, Vec::new()) {
            Ok(_) => {
                self.status
                    .reply
                    .update(cx, |field, cx| field.set_value("", window, cx));
                if let Some(viewing) = self.status.viewing.as_mut() {
                    viewing.notice = Some("Reply sent".into());
                    viewing.reply_note = None;
                    viewing.player.holds.typing = false;
                }
                // The keyboard goes back to the viewer.
                self.focus.focus(window, cx);
            }
            Err(error) => {
                if let Some(viewing) = self.status.viewing.as_mut() {
                    viewing.reply_note = Some(error.to_string().into());
                }
            }
        }
        cx.notify();
    }

    /// Reacts to the story on screen with one emoji; the same again takes
    /// the reaction back.
    pub(super) fn react_to_current(&mut self, emoji: &'static str, cx: &mut Context<Self>) {
        let (Some(account), Some(viewing)) = (self.account.clone(), self.status.viewing.as_mut())
        else {
            return;
        };
        if viewing.author().is_some_and(|author| author.mine) {
            return;
        }
        let Some(id) = viewing.player.current().map(|slide| slide.id.clone()) else {
            return;
        };
        let taking_back = viewing.reacted.get(&id) == Some(&emoji);
        let sent = if taking_back { "" } else { emoji };
        match self.engine.react_to_story(&account, &id, sent) {
            Ok(_) => {
                if taking_back {
                    viewing.reacted.remove(&id);
                    viewing.notice = None;
                } else {
                    viewing.reacted.insert(id, emoji);
                    viewing.notice = Some(SharedString::from(format!("Reacted {emoji}")));
                }
            }
            Err(error) => viewing.reply_note = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn ask_viewers_of_current(&self, _cx: &mut Context<Self>) {
        let (Some(account), Some(viewing)) = (self.account.as_ref(), self.status.viewing.as_ref())
        else {
            return;
        };
        if viewing.author().is_some_and(|author| author.mine) {
            if let Some(slide) = viewing.player.current() {
                self.engine.want_story_viewers(account, &slide.id);
            }
        }
    }

    // ----- drawing ---------------------------------------------------------------------------------

    /// The viewer, in the main pane.
    pub(super) fn render_story_viewer(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let Some(viewing) = self.status.viewing.as_ref() else {
            return div().id("story-viewer");
        };
        let now = Timestamp::now();
        let item = viewing.item();
        let mine = viewing.author().is_some_and(|author| author.mine);
        let footer_h = if mine { px(64.) } else { px(112.) };
        // The card: as tall as the pane leaves room for, as wide as a
        // phone's screen is for that height, never wider than the pane.
        let pane_w = window.viewport_size().width - metrics::RAIL_WIDTH() - self.list_px;
        let stage_h = window.viewport_size().height
            - metrics::HEADER_HEIGHT()
            - footer_h
            - px(24.) // the progress bar
            - px(32.); // margins
        let card_h = stage_h.max(px(160.));
        let card_w = (card_h * (9. / 16.))
            .min(story::CARD_WIDTH())
            .min((pane_w - px(112.)).max(px(120.)));
        let card_h = (card_w * (16. / 9.)).min(card_h);
        div()
            .id("story-viewer")
            .debug_selector(|| "story-viewer".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .child(self.render_story_bar(viewing, now, palette, cx))
            .child(
                div()
                    .id("story-stage")
                    .debug_selector(|| "story-stage".into())
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(8.))
                    .py_4()
                    // The story, and the arrows to the ones around it.
                    .child(self.render_story_progress(viewing, card_w, palette))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(self.render_story_arrow(true, palette, cx))
                            .child(
                                self.render_story_card(viewing, item, card_w, card_h, palette, cx),
                            )
                            .child(self.render_story_arrow(false, palette, cx)),
                    )
                    .when(viewing.panel.is_some(), |this| {
                        this.child(self.render_story_panel(viewing, now, palette, cx))
                    }),
            )
            .child(self.render_story_footer(viewing, mine, footer_h, palette, cx))
    }

    fn render_story_bar(
        &self,
        viewing: &Viewing,
        now: Timestamp,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let author = viewing.author();
        let account = self.account.clone().unwrap_or_else(|| AccountId::new(""));
        let name = author.map(|author| author.name.clone()).unwrap_or_default();
        let mine = author.is_some_and(|author| author.mine);
        let item = viewing.item();
        let age = item.map_or(String::new(), |item| age_label(item.story.posted_at, now));
        let paused = viewing.player.holds.paused;
        let picture = author.filter(|author| !author.mine).and_then(|author| {
            self.media.avatar(
                &account,
                &client_provider::ChatId::new(author.author.as_str()),
            )
        });
        let tone = author
            .map(|author| self.senders.of(&account, None, &author.author).tone)
            .unwrap_or(0);
        let face =
            super::senders::person_avatar(picture, &name, metrics::AVATAR_MEDIUM(), tone, palette);
        let muted = author.is_some_and(|author| {
            self.status
                .feed
                .muted
                .iter()
                .any(|muted| muted.key == author.key)
        });
        let can_mute = !mine;
        div()
            .debug_selector(|| "story-bar".into())
            .flex_none()
            .h(metrics::HEADER_HEIGHT())
            .pl_4()
            .pr_2()
            .border_b_1()
            .border_color(palette.border)
            .flex()
            .items_center()
            .gap_3()
            .child(face)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .debug_selector(|| "story-author".into())
                            .truncate()
                            .text_size(metrics::TEXT_NAME())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .debug_selector(|| "story-age".into())
                            .truncate()
                            .text_size(metrics::TEXT_META())
                            .text_color(palette.text_muted)
                            .child(SharedString::from(age)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .child(
                        icon_button(
                            "story-pause",
                            if paused {
                                IconName::Play
                            } else {
                                IconName::Pause
                            },
                            palette,
                        )
                        .tooltip(tooltip_with_keys("Pause or go on", Command::StoryPause))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.run_story_command(Command::StoryPause, window, cx);
                        })),
                    )
                    .when(can_mute, |this| {
                        this.child(
                            icon_button(
                                "story-mute",
                                if muted {
                                    IconName::Bell
                                } else {
                                    IconName::BellOff
                                },
                                palette,
                            )
                            .tooltip(tooltip_with_keys(
                                if muted {
                                    "Unmute their status"
                                } else {
                                    "Mute their status"
                                },
                                Command::StoryMute,
                            ))
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.run_story_command(Command::StoryMute, window, cx);
                                },
                            )),
                        )
                    })
                    .child(
                        icon_button("story-info", IconName::Info, palette)
                            .tooltip(tooltip_with_keys(
                                "Who saw it, and when it goes",
                                Command::StoryInfo,
                            ))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.run_story_command(Command::StoryInfo, window, cx);
                            })),
                    )
                    .child(
                        icon_button("story-close", IconName::X, palette)
                            .tooltip(tooltip_with_keys("Close", Command::StoryClose))
                            .on_click(cx.listener(|this, _, _, cx| this.close_story_viewer(cx))),
                    ),
            )
    }

    /// One segment for each story of the author, filled as it plays.
    fn render_story_progress(
        &self,
        viewing: &Viewing,
        width: gpui_kit::Pixels,
        palette: &Palette,
    ) -> Div {
        let count = viewing.player.reel().map_or(0, |reel| reel.slides.len());
        let mut row = div()
            .debug_selector(|| "story-progress".into())
            .w(width)
            .flex_none()
            .flex()
            .gap(px(4.));
        for index in 0..count {
            let fill = viewing.player.progress(index);
            row = row.child(
                div()
                    .debug_selector(move || format!("story-progress-{index}"))
                    .flex_1()
                    .h(story::PROGRESS_HEIGHT())
                    .rounded_full()
                    .overflow_hidden()
                    .bg(palette.muted)
                    .child(
                        div()
                            .h_full()
                            .w(gpui_kit::relative(fill))
                            .bg(palette.accent),
                    ),
            );
        }
        row
    }

    fn render_story_arrow(
        &self,
        back: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let (id, command) = if back {
            ("story-prev", Command::StoryPrevious)
        } else {
            ("story-next", Command::StoryNext)
        };
        icon_button(
            id,
            if back {
                IconName::ChevronLeft
            } else {
                IconName::ChevronRight
            },
            palette,
        )
        .tooltip(tooltip_with_keys(
            if back { "Previous story" } else { "Next story" },
            command,
        ))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.run_story_command(command, window, cx);
        }))
    }

    /// The story: its words on its colour, its picture, or a tile.
    fn render_story_card(
        &self,
        viewing: &Viewing,
        item: Option<&StoryItem>,
        width: gpui_kit::Pixels,
        height: gpui_kit::Pixels,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let card = div()
            .id("story-card")
            .debug_selector(|| "story-card".into())
            .flex_none()
            .relative()
            .w(width)
            .h(height)
            .rounded(metrics::PANEL_RADIUS())
            .overflow_hidden()
            .border_1()
            .border_color(palette.border)
            // Held with the pointer: the story waits for as long as it is.
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(viewing) = this.status.viewing.as_mut() {
                        viewing.player.holds.pointer = true;
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(viewing) = this.status.viewing.as_mut() {
                        viewing.player.holds.pointer = false;
                    }
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if let Some(viewing) = this.status.viewing.as_mut() {
                        viewing.player.holds.pointer = false;
                    }
                    cx.notify();
                }),
            );
        let Some(item) = item else {
            return card.bg(palette.surface);
        };
        match &item.story.body {
            StoryBody::Text { text, style } => {
                let face = face_of(style.font);
                let shown = if face.caps {
                    text.to_uppercase()
                } else {
                    text.clone()
                };
                card.bg(story::background(style.background)).child(
                    with_face(
                        div()
                            .debug_selector(|| "story-text".into())
                            .size_full()
                            .p_6()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_center()
                            .text_color(story::on_background())
                            .text_size(px(text_size(text)))
                            .line_height(px(text_size(text) * 1.25)),
                        face,
                    )
                    .child(SharedString::from(shown)),
                )
            }
            StoryBody::Media(media) => {
                let card = card.bg(story::scrim());
                let kind = SlideKind::of(&item.story.body);
                let body: gpui_kit::AnyElement = match kind {
                    SlideKind::Image => self.render_story_picture(media, palette, cx),
                    SlideKind::Video => self.render_story_video(viewing, media, palette, cx),
                    SlideKind::Voice => self.render_story_tile(viewing, media, kind, palette, cx),
                    SlideKind::Text => div().into_any_element(),
                };
                card.child(body).children(
                    media
                        .caption
                        .clone()
                        .filter(|c| !c.trim().is_empty())
                        .map(|caption| self.render_story_caption(&caption, item, palette, cx)),
                )
            }
        }
    }

    /// The picture, through the same pipeline as a chat's: fetched as
    /// the policy says (or on a click), with its progress, its
    /// orientation read from the file.
    fn render_story_picture(
        &self,
        media: &Media,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(account) = self.account.clone() else {
            return div().into_any_element();
        };
        let still = crate::settings::reduce_motion(cx);
        let url = media.source.as_ref().map(|source| source.to_string());
        let centred = |child: gpui_kit::AnyElement| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .child(child)
                .into_any_element()
        };
        let say = |text: SharedString| {
            div()
                .debug_selector(|| "story-picture-note".into())
                .px_4()
                .text_center()
                .text_size(metrics::TEXT_SMALL())
                .text_color(story::on_background())
                .child(text)
                .into_any_element()
        };
        match self.media.visual(&account, media) {
            super::media::MediaVisual::Image(image) => div()
                .debug_selector(|| "story-picture".into())
                .size_full()
                .child(img(image).size_full().object_fit(ObjectFit::Contain))
                .into_any_element(),
            super::media::MediaVisual::Loading { fraction } => {
                let ask_url = url.clone().unwrap_or_default();
                centred(
                    transfer_button(
                        "story-loading",
                        &Transfer::Loading { fraction },
                        palette,
                        still,
                        cx.listener(move |this, _, _, cx| {
                            this.media.cancel(&ask_url);
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                )
            }
            super::media::MediaVisual::Download { size } => {
                let ask_url = url.clone().unwrap_or_default();
                centred(
                    transfer_button(
                        "story-download",
                        &Transfer::Download { size },
                        palette,
                        still,
                        cx.listener(move |this, _, _, cx| {
                            if let Some(account) = this.account.clone() {
                                this.media.ask(&account, &ask_url);
                            }
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                )
            }
            super::media::MediaVisual::Failed(reason) => {
                let ask_url = url.clone().unwrap_or_default();
                centred(
                    transfer_button(
                        "story-retry",
                        &Transfer::Retry(reason),
                        palette,
                        still,
                        cx.listener(move |this, _, _, cx| {
                            if let Some(account) = this.account.clone() {
                                this.media.ask(&account, &ask_url);
                            }
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
                )
            }
            super::media::MediaVisual::Unavailable(reason) => centred(say(reason)),
            super::media::MediaVisual::NoFile => {
                centred(say("This status has no picture to show.".into()))
            }
            super::media::MediaVisual::Unsupported(reason) => centred(say(reason)),
        }
    }

    /// A story video: the frame when there is one, a failure the person
    /// can retry, or the tile that asks for the file.
    fn render_story_video(
        &self,
        viewing: &Viewing,
        media: &Media,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let id = viewing.player.current().map(|slide| slide.id.clone());
        let failed = self
            .status
            .video_error
            .as_ref()
            .is_some_and(|(err_id, _)| Some(err_id) == id.as_ref())
            || self
                .current_video_report()
                .is_some_and(|report| report.failure.is_some());
        if failed {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .child(
                    div()
                        .debug_selector(|| "story-video-note".into())
                        .px_4()
                        .text_center()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(story::on_background())
                        .child(VIDEO_FAILED),
                )
                .child(
                    text_button(
                        "story-open-video",
                        "Try again",
                        Some(IconName::Play),
                        true,
                        palette,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.open_current_media(cx))),
                )
                .into_any_element();
        }
        if let Some((clip, image, width, height)) =
            self.status.clip.as_ref().and_then(|(clip_id, clip)| {
                if Some(clip_id) != id.as_ref() {
                    return None;
                }
                let report = clip.report();
                let image = report.image?;
                Some((Arc::clone(clip), image, report.width, report.height))
            })
        {
            return self.render_story_video_frame(clip, image, width, height);
        }
        self.render_story_tile(viewing, media, SlideKind::Video, palette, cx)
    }

    /// The current frame, contained in the card. Frames the decoder has
    /// replaced are given back to the window here.
    fn render_story_video_frame(
        &self,
        clip: Arc<crate::video::Clip>,
        image: Arc<gpui_kit::RenderImage>,
        width: u32,
        height: u32,
    ) -> gpui_kit::AnyElement {
        let width = width.max(1) as f32;
        let height = height.max(1) as f32;
        div()
            .debug_selector(|| "story-video".into())
            .size_full()
            .child(
                gpui_kit::canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        for stale in clip.take_stale() {
                            let _ = window.drop_image(stale);
                        }
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
                            gpui_kit::Corners::all(px(0.)),
                            Arc::clone(&image),
                            0,
                            false,
                        );
                    },
                )
                .size_full(),
            )
            .into_any_element()
    }

    /// A video that is not playing yet, or a voice note: a labelled tile.
    fn render_story_tile(
        &self,
        viewing: &Viewing,
        media: &Media,
        kind: SlideKind,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let length = media
            .duration_secs
            .map(clock_time)
            .or_else(|| media.size_bytes.map(file_size))
            .unwrap_or_default();
        let opened = viewing.player.holds.external;
        let (glyph, title, action, button_icon) = match kind {
            SlideKind::Voice => (
                IconName::Mic,
                "Voice message",
                "Play",
                IconName::ExternalLink,
            ),
            _ => (IconName::Video, "Video", "Play", IconName::Play),
        };
        div()
            .debug_selector(|| "story-tile".into())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .p_4()
            .text_color(story::on_background())
            .child(icon(glyph, px(36.), story::on_background()))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .child(
                        div()
                            .text_size(metrics::TEXT_BODY())
                            .font_weight(FontWeight::MEDIUM)
                            .child(title),
                    )
                    .child(mono(length).text_color(story::on_background().opacity(0.7))),
            )
            .child(
                text_button(
                    "story-open-video",
                    if opened { "Playing outside" } else { action },
                    Some(button_icon),
                    true,
                    palette,
                )
                .on_click(cx.listener(|this, _, _, cx| this.open_current_media(cx))),
            )
            .into_any_element()
    }

    /// The caption, over the foot of the picture: the existing markup,
    /// links that open in the browser.
    fn render_story_caption(
        &self,
        caption: &str,
        item: &StoryItem,
        _palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let mentions: Vec<markup::Handle> = item
            .story
            .mentions
            .iter()
            .enumerate()
            .map(|(who, mention)| markup::Handle {
                handle: mention.handle.clone(),
                name: mention.name.clone(),
                me: mention.me,
                who,
            })
            .collect();
        let mut column = div().flex().flex_col();
        for (index, block) in markup::parse(caption, &mentions).iter().enumerate() {
            let line = match block {
                Block::Paragraph(line)
                | Block::Quote(line)
                | Block::Bullet(line)
                | Block::Numbered(_, line) => line.clone(),
                Block::Code(code) => markup::Line {
                    text: code.clone(),
                    spans: Vec::new(),
                },
            };
            let mut highlights = Vec::new();
            let mut clicks = Vec::new();
            for span in &line.spans {
                let style = &span.style;
                highlights.push((
                    span.range.clone(),
                    HighlightStyle {
                        color: style
                            .link
                            .is_some()
                            .then(|| story::on_background().opacity(0.95)),
                        font_weight: (style.bold || style.mention).then_some(FontWeight::SEMIBOLD),
                        font_style: style.italic.then_some(FontStyle::Italic),
                        underline: style.link.as_ref().map(|_| UnderlineStyle {
                            thickness: gpui_kit::px(1.),
                            color: Some(story::on_background()),
                            wavy: false,
                        }),
                        strikethrough: style.strike.then_some(StrikethroughStyle {
                            thickness: gpui_kit::px(1.),
                            color: None,
                        }),
                        ..Default::default()
                    },
                ));
                if let Some(address) = &style.link {
                    clicks.push((span.range.clone(), address.clone()));
                }
            }
            let shown = SharedString::from(line.text.clone());
            let text = StyledText::new(shown).with_highlights(highlights);
            let element: gpui_kit::AnyElement = if clicks.is_empty() {
                text.into_any_element()
            } else {
                let (ranges, addresses): (Vec<_>, Vec<_>) = clicks.into_iter().unzip();
                let view = cx.weak_entity();
                InteractiveText::new(SharedString::from(format!("story-caption-{index}")), text)
                    .on_click(ranges, move |at, _, cx| {
                        if let Some(address) = addresses.get(at).cloned() {
                            view.update(cx, |this, cx| this.open_link(&address, cx))
                                .ok();
                        }
                    })
                    .into_any_element()
            };
            column = column.child(div().child(element));
        }
        div()
            .debug_selector(|| "story-caption".into())
            .absolute()
            .left_0()
            .right_0()
            .bottom_0()
            .px_4()
            .pt_6()
            .pb_3()
            .bg(story::scrim().opacity(0.55))
            .text_color(story::on_background())
            .text_size(metrics::TEXT_BODY())
            .line_height(px(21.))
            .child(column)
    }

    /// Who saw it, or when it was posted and goes.
    fn render_story_panel(
        &self,
        viewing: &Viewing,
        now: Timestamp,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let account = self.account.clone().unwrap_or_else(|| AccountId::new(""));
        let item = viewing.item();
        let mine = viewing.author().is_some_and(|author| author.mine);
        let mut body = div().flex().flex_col().gap_2();
        if let Some(item) = item {
            let posted = age_label(item.story.posted_at, now);
            body = body.child(super::menus::fact("Posted", posted.into(), palette));
            match expires_label(item.story.expiry(), now) {
                Some(left) => body = body.child(super::menus::fact("Goes", left.into(), palette)),
                None => body = body.child(super::menus::fact("Goes", "Gone".into(), palette)),
            }
            if mine {
                let viewers = self
                    .engine
                    .store()
                    .story_viewers(&account, &item.story.id)
                    .unwrap_or_default();
                body = body.child(label(&format!("Seen by {}", viewers.len()), palette));
                if !self.caps_now().story_viewers {
                    body = body.child(
                        div()
                            .debug_selector(|| "story-viewers-unavailable".into())
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text_muted)
                            .child("Who saw your status is not available yet from this provider."),
                    );
                }
                for (index, viewer) in viewers.iter().enumerate() {
                    body = body.child(
                        div()
                            .debug_selector(move || format!("story-viewer-row-{index}"))
                            .h(px(36.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(metrics::TEXT_BODY())
                                    .child(SharedString::from(viewer.name.clone().unwrap_or_else(
                                        || client_core::UNKNOWN_PERSON.to_owned(),
                                    ))),
                            )
                            .children(viewer.reaction.clone().map(SharedString::from))
                            .child(
                                mono(age_label(viewer.viewed_at, now))
                                    .text_color(palette.text_faint),
                            ),
                    );
                }
            }
        }
        div()
            .id("story-panel")
            .debug_selector(|| "story-panel".into())
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .w(px(300.))
            .max_w(px(360.))
            .bg(palette.elevated)
            .border_l_1()
            .border_color(palette.elevated_border)
            .flex()
            .flex_col()
            .child(super::menus::panel_header(
                "Details",
                palette,
                cx.listener(|this, _, window, cx| {
                    this.run_story_command(Command::StoryInfo, window, cx);
                }),
            ))
            .child(
                div()
                    .id("story-panel-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(body),
            )
    }

    /// Under the story: the way to answer, or for the account's own, how
    /// many saw it.
    fn render_story_footer(
        &self,
        viewing: &Viewing,
        mine: bool,
        height: gpui_kit::Pixels,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let caps = self.caps_now();
        let account = self.account.clone().unwrap_or_else(|| AccountId::new(""));
        let footer = div()
            .debug_selector(|| "story-footer".into())
            .flex_none()
            .min_h(height)
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_2();
        let note = viewing
            .reply_note
            .clone()
            .or_else(|| viewing.notice.clone());
        let note = note.map(|note| {
            div()
                .debug_selector(|| "story-note".into())
                .text_size(metrics::TEXT_SMALL())
                .text_color(if viewing.reply_note.is_some() {
                    palette.danger
                } else {
                    palette.accent
                })
                .child(note)
        });
        if mine {
            let count = viewing.item().and_then(|item| {
                self.engine
                    .store()
                    .story(&account, &item.story.id)
                    .ok()
                    .flatten()
                    .and_then(|stored| stored.story.view_count)
            });
            let views: SharedString = match (caps.story_viewers, count) {
                (true, Some(1)) => "1 view".into(),
                (true, Some(n)) => format!("{n} views").into(),
                (true, None) => "No views yet".into(),
                (false, _) => "Views are not available yet from this provider".into(),
            };
            let id = viewing.player.current().map(|slide| slide.id.clone());
            return footer
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            text_button(
                                "story-views",
                                views,
                                Some(IconName::Users),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.run_story_command(Command::StoryInfo, window, cx);
                                },
                            )),
                        )
                        .children(id.map(|id| {
                            text_button(
                                "story-delete",
                                "Delete",
                                Some(IconName::Trash),
                                false,
                                palette,
                            )
                            .text_color(palette.danger)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    let _ = &id;
                                    this.run_story_command(Command::StoryDelete, window, cx);
                                },
                            ))
                        })),
                )
                .children(note);
        }
        let reacted = viewing
            .player
            .current()
            .and_then(|slide| viewing.reacted.get(&slide.id).copied());
        let mut strip = div()
            .debug_selector(|| "story-reactions".into())
            .flex()
            .items_center()
            .gap_1();
        if caps.story_react {
            for (index, emoji) in REACTIONS.iter().copied().enumerate() {
                strip = strip.child(
                    div()
                        .id(("story-react", index))
                        .debug_selector(move || format!("story-react-{index}"))
                        .size(px(32.))
                        .rounded_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .text_size(px(18.))
                        .when(reacted == Some(emoji), |this| this.bg(palette.muted))
                        .hover({
                            let hover = palette.hover;
                            move |style| style.bg(hover)
                        })
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.react_to_current(emoji, cx)),
                        )
                        .child(emoji),
                );
            }
        }
        let reply = if caps.story_reply {
            div()
                .w_full()
                .max_w(px(480.))
                .flex()
                .flex_col()
                .gap_1()
                // The emoji a shortcode stands for, over the field.
                .children(self.render_emoji_completion(palette, cx))
                .child(
                    div()
                        .w_full()
                        .flex()
                        .items_end()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .debug_selector(|| "story-reply".into())
                                .child(Textarea::new(&self.status.reply).small()),
                        )
                        .child(self.render_status_emoji_button(palette, cx))
                        .child(
                            icon_button("story-send", IconName::SendHorizontal, palette)
                                .tooltip(|window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new("Send")
                                        .build(window, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_story_reply(window, cx)
                                })),
                        ),
                )
        } else {
            div()
                .debug_selector(|| "story-reply-unavailable".into())
                .text_size(metrics::TEXT_SMALL())
                .text_color(palette.text_faint)
                .child("Replying to a status is not available yet from this provider.")
        };
        let not_reacting = !caps.story_react;
        footer
            .when(!not_reacting, |this| this.child(strip))
            .child(reply)
            .children(note)
    }
}
