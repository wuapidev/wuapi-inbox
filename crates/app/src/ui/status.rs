//! Status (WhatsApp's stories): the list, and how it is reached.
//!
//! Where it lives: Status is a destination of its own. The rail has two
//! buttons under the logo, Chats and Status, with a dot on Status while
//! there is something new; and the chat list wears a strip of the people
//! with something new above its rows (`status_strip.rs`), so a status is
//! one click away without going anywhere. The three panes stay three:
//! with Status chosen, the list pane lists updates and the main pane
//! shows the viewer, or the account's own status, or a page that says
//! what to do. Going back leaves the chats exactly as they were.
//!
//! This file holds the state, the rows, the commands and the pane of the
//! list. The viewer is `status_view.rs`, posting `status_post.rs`, the
//! account's own status and who sees it `status_mine.rs`.

use super::shell::{Overlay, Shell};
use super::widgets::{avatar_or, icon_button, label, mono, text_button, AvatarKind};
use crate::icons::{icon, IconName};
use crate::keys::Command;
use crate::stories::{age_label, describe};
use crate::theme::{metrics, px, story, Palette};
use client_core::{StoryAuthor, StoryFeed, StoryRing, STORIES_FRESH, STORIES_OPENED};
use client_provider::{AccountId, ContactId, Feature, MessageId, Timestamp};
use gpui_kit::component::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::Focusable as _;
use gpui_kit::{
    div, uniform_list, AnyWindowHandle, Context, Div, Entity, FontWeight, ScrollHandle,
    SharedString, Stateful, Subscription, Task, UniformListScrollHandle, Window,
};
use std::collections::HashMap;
use std::time::Duration;

/// What the list says when the provider does not deliver contacts'
/// updates.
pub(super) const NO_CONTACT_UPDATES: &str =
    "Your contacts' status updates are not available yet from this provider.";

/// How many authors' first story is fetched ahead.
const PREFETCH_AUTHORS: usize = 12;

/// What the list pane lists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ListMode {
    /// The chats.
    #[default]
    Chats,
    /// The updates.
    Status,
}

/// Which part of the list an author is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum Part {
    Recent,
    Viewed,
    Muted,
}

impl Part {
    fn title(self) -> &'static str {
        match self {
            Self::Recent => "Recent updates",
            Self::Viewed => "Viewed updates",
            Self::Muted => "Muted",
        }
    }
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Row {
    /// "My status".
    Mine,
    /// The title of a part. The muted one folds.
    Heading(Part),
    /// An author, by `StoryAuthor::key`, in a part.
    Author(Part, String),
}

impl Row {
    /// The keyboard can stop here.
    fn is_stop(&self) -> bool {
        !matches!(self, Self::Heading(part) if *part != Part::Muted)
    }
}

/// What there is to say about contacts' updates: nothing while some are
/// listed, and otherwise why there are none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Updates {
    /// Somebody has a status up.
    Listed,
    /// The provider is being asked.
    Loading,
    /// The number is not connected.
    Offline,
    /// The provider has them, its backend not yet for this number.
    Unavailable,
    /// The provider never gives contacts' updates.
    NotOffered,
    /// The last time it was asked it did not answer.
    Failed,
    /// Nobody has posted anything in the last day.
    Empty,
}

impl Updates {
    /// The name the state goes by in the window (`status-state-…`).
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Listed => "listed",
            Self::Loading => "loading",
            Self::Offline => "offline",
            Self::Unavailable => "unavailable",
            Self::NotOffered => "not-offered",
            Self::Failed => "failed",
            Self::Empty => "empty",
        }
    }

    /// What is said: a title, and a sentence under it.
    pub(super) fn words(self) -> (&'static str, &'static str) {
        match self {
            Self::Listed => ("", ""),
            Self::Loading => ("Checking for updates…", ""),
            Self::Offline => (
                "This number is offline",
                "Updates from your contacts appear when it is connected again.",
            ),
            Self::Unavailable => (
                "Not available for this number yet",
                "Status updates from your contacts are not turned on for this number yet. \
                 Your own status works.",
            ),
            Self::NotOffered => ("", NO_CONTACT_UPDATES),
            Self::Failed => (
                "Could not check for updates",
                "The connection dropped. It is tried again by itself.",
            ),
            Self::Empty => (
                "No updates yet",
                "Updates from your contacts in the last 24 hours appear here.",
            ),
        }
    }

    /// Asking again can change it.
    pub(super) fn can_check(self) -> bool {
        matches!(
            self,
            Self::Unavailable | Self::Failed | Self::Empty | Self::Offline
        )
    }
}

/// What the main pane shows while Status is chosen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Main {
    /// Nothing chosen: the page that says what Status is.
    Empty,
    /// A status being viewed.
    Viewing,
    /// The account's own status: each story and who saw it.
    Mine,
}

/// The state of Status in the shell.
pub(super) struct StatusUi {
    pub(super) mode: ListMode,
    pub(super) main: Main,
    /// What the store lists, as of the last change.
    pub(super) feed: StoryFeed,
    /// The ring of everybody with stories, by every id they go by.
    pub(super) rings: HashMap<ContactId, StoryRing>,
    /// The account the feed is of.
    pub(super) feed_of: Option<AccountId>,
    /// The accounts whose stories were asked for at least once.
    asked: Vec<AccountId>,
    pub(super) rows: Vec<Row>,
    /// For each row, how many authors are listed above it: the author
    /// rows are numbered down the list.
    ordinals: Vec<usize>,
    /// Where each author of the feed is, by `StoryAuthor::key`: the part
    /// and the place in it. A row finds its author without a search.
    places: HashMap<String, (Part, usize)>,
    /// The row the keyboard is on.
    pub(super) cursor: usize,
    /// The muted updates are listed.
    pub(super) muted_open: bool,
    pub(super) scroll: UniformListScrollHandle,
    /// Where the strip above the chats is scrolled to.
    pub(super) strip_scroll: ScrollHandle,
    /// The window all this is in: its text fields are written to from
    /// where no window is at hand (the viewer's clock).
    pub(super) window: AnyWindowHandle,
    /// A status being viewed (`status_view.rs`).
    pub(super) viewing: Option<super::status_view::Viewing>,
    /// The story the account's own detail has open, when it does.
    pub(super) mine_open: Option<MessageId>,
    /// A sheet over the window: post, audience, delete, who saw.
    pub(super) sheet: Option<super::status_post::Sheet>,
    /// What is being written to post.
    pub(super) draft: super::status_post::Draft,
    /// The reply field of the viewer.
    pub(super) reply: Entity<TextareaState>,
    /// The words of a text status being written.
    pub(super) words: Entity<TextareaState>,
    /// The caption of a picture or a video being posted.
    pub(super) caption: Entity<TextareaState>,
    /// The viewer's clock.
    pub(super) clock: Option<Task<()>>,
    /// The search field of the audience's list of people.
    pub(super) query: Entity<InputState>,
    /// A video that was asked to be opened outside, waiting for its file.
    pub(super) video_pending: Option<(MessageId, String)>,
    _subs: Vec<Subscription>,
}

impl StatusUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        let reply = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 3)
                .submit_on_enter(true)
                .placeholder("Reply")
        });
        let words = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .placeholder("Type a status")
        });
        let caption = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 4)
                .submit_on_enter(true)
                .placeholder("Add a caption")
        });
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search a name"));
        let subs =
            vec![
                cx.subscribe_in(&query, window, |this, field, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let typed = field.read(cx).value().to_string();
                        if let Some(edit) = this.status.draft.audience.as_mut() {
                            edit.query = typed;
                        }
                        cx.notify();
                    }
                }),
                // The three fields that are written in are "the field
                // being written in" while they are in hand (`writing`):
                // a shortcode typed in one is offered its emoji.
                cx.subscribe_in(&reply, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::PressEnter { .. } => this.send_story_reply(window, cx),
                        InputEvent::Focus => this.story_typing(true, cx),
                        InputEvent::Blur => this.story_typing(false, cx),
                        InputEvent::Change => this.emoji_completion_changed(cx),
                    }
                }),
                cx.subscribe_in(&words, window, |this, _, event: &InputEvent, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        this.status_words_changed(cx);
                        this.emoji_completion_changed(cx);
                    }
                }),
                cx.subscribe_in(
                    &caption,
                    window,
                    |this, _, event: &InputEvent, window, cx| match event {
                        InputEvent::PressEnter { .. } => this.post_status(window, cx),
                        InputEvent::Change => this.emoji_completion_changed(cx),
                        _ => {}
                    },
                ),
            ];
        Self {
            mode: ListMode::Chats,
            main: Main::Empty,
            feed: StoryFeed::default(),
            rings: HashMap::new(),
            feed_of: None,
            asked: Vec::new(),
            rows: Vec::new(),
            ordinals: Vec::new(),
            places: HashMap::new(),
            cursor: 0,
            muted_open: false,
            scroll: UniformListScrollHandle::new(),
            strip_scroll: ScrollHandle::new(),
            window: window.window_handle(),
            viewing: None,
            mine_open: None,
            sheet: None,
            draft: Default::default(),
            reply,
            words,
            caption,
            clock: None,
            query,
            video_pending: None,
            _subs: subs,
        }
    }
}

impl Shell {
    // ----- what the provider can do --------------------------------------------

    /// Whether the provider has Status at all: it lists stories or posts
    /// them. Without either there is no button in the rail and no strip.
    pub(super) fn has_status(&self) -> bool {
        let caps = self.caps_now();
        caps.story_list || caps.story_post || caps.story_contacts
    }

    /// Status is what the list pane shows.
    pub(super) fn status_active(&self) -> bool {
        self.status.mode == ListMode::Status
    }

    /// A conversation is on screen: one is open, and Status is not in
    /// its place. What is written, picked or attached for "the chat"
    /// goes to a conversation the user can see, never to one behind
    /// Status.
    pub(super) fn conversation_showing(&self) -> bool {
        self.open.is_some() && !self.status_active()
    }

    /// A status is on screen.
    pub(super) fn story_is_open(&self) -> bool {
        self.status_active() && self.status.viewing.is_some()
    }

    /// The status list has the keyboard: Status is chosen, nothing is
    /// being viewed, nothing is over the panes and the shell holds the
    /// keyboard.
    pub(super) fn status_list_has_keyboard(&self, window: &Window) -> bool {
        self.status_active()
            && self.status.viewing.is_none()
            && self.overlay == Overlay::None
            && self.status.sheet.is_none()
            && self.focus.is_focused(window)
    }

    // ----- reading the store -------------------------------------------------------

    /// Reads the account's stories again: the list, the rings around
    /// pictures, the dot on the rail, the strip above the chats. Asks the
    /// provider for them once per account, and again when the copy is
    /// old (see [`STORIES_FRESH`]).
    pub(super) fn reload_status(&mut self, cx: &mut Context<Self>) {
        let Some(account) = self.account.clone() else {
            self.status.feed = StoryFeed::default();
            self.status.rings.clear();
            self.status.rows.clear();
            self.status.ordinals.clear();
            self.status.places.clear();
            return;
        };
        if !self.status.asked.contains(&account) {
            self.status.asked.push(account.clone());
            self.engine.want_stories(&account, STORIES_FRESH);
        }
        let feed = self.engine.story_feed(&account).unwrap_or_default();
        let mut rings = HashMap::new();
        for author in feed.recent.iter().chain(feed.viewed.iter()) {
            let ring = StoryRing {
                total: author.stories.len(),
                unviewed: author.unviewed(),
            };
            for id in &author.ids {
                rings.insert(id.clone(), ring);
            }
        }
        self.status.rings = rings;
        self.status.feed = feed;
        self.prefetch_stories(&account, cx);
        self.status.feed_of = Some(account);
        self.rebuild_status_rows();
        // A story that went while it was being viewed leaves the viewer.
        self.story_feed_changed(cx);
    }

    /// While Status is on screen, fetches ahead the first story not seen of each author who has
    /// one, when it is a picture and the media policy fetches pictures by
    /// itself (the shelf asks the policy; "Nothing" fetches nothing). Never
    /// a video, never more than a dozen authors, and never anything that
    /// counts as seeing: a picture that is here before the viewer opens
    /// is only a picture that is here.
    fn prefetch_stories(&self, account: &AccountId, cx: &Context<Self>) {
        // Only for somebody who is looking at Status: the dot and the
        // rings need the list of stories, never their pictures.
        if !self.status_active() {
            return;
        }
        self.media.set_policy(crate::settings::get(cx).media);
        for author in self.status.feed.recent.iter().take(PREFETCH_AUTHORS) {
            let Some(item) = author.stories.iter().find(|item| !item.viewed) else {
                continue;
            };
            if let client_provider::StoryBody::Media(media) = &item.story.body {
                if media.kind == client_provider::MediaKind::Image {
                    let _ = self.media.visual(account, media);
                }
            }
        }
    }

    /// The rows of the list from the feed.
    fn rebuild_status_rows(&mut self) {
        let status = &mut self.status;
        let key = status
            .rows
            .get(status.cursor)
            .cloned()
            .filter(|row| matches!(row, Row::Author(..)));
        let mut rows = vec![Row::Mine];
        if !status.feed.recent.is_empty() {
            rows.push(Row::Heading(Part::Recent));
            rows.extend(
                status
                    .feed
                    .recent
                    .iter()
                    .map(|author| Row::Author(Part::Recent, author.key.clone())),
            );
        }
        if !status.feed.viewed.is_empty() {
            rows.push(Row::Heading(Part::Viewed));
            rows.extend(
                status
                    .feed
                    .viewed
                    .iter()
                    .map(|author| Row::Author(Part::Viewed, author.key.clone())),
            );
        }
        if !status.feed.muted.is_empty() {
            rows.push(Row::Heading(Part::Muted));
            if status.muted_open {
                rows.extend(
                    status
                        .feed
                        .muted
                        .iter()
                        .map(|author| Row::Author(Part::Muted, author.key.clone())),
                );
            }
        }
        // Numbered and indexed once, here, so that drawing a row is the
        // same work however long the list is.
        let mut authors = 0;
        status.ordinals = rows
            .iter()
            .map(|row| {
                let above = authors;
                if matches!(row, Row::Author(..)) {
                    authors += 1;
                }
                above
            })
            .collect();
        let mut places = HashMap::new();
        for (part, list) in [
            (Part::Recent, &status.feed.recent),
            (Part::Viewed, &status.feed.viewed),
            (Part::Muted, &status.feed.muted),
        ] {
            for (index, author) in list.iter().enumerate() {
                places.entry(author.key.clone()).or_insert((part, index));
            }
        }
        status.places = places;
        status.rows = rows;
        // The outline stays on the author it was on, wherever they went.
        status.cursor = match key {
            Some(key) => status.rows.iter().position(
                |row| matches!((row, &key), (Row::Author(_, a), Row::Author(_, b)) if a == b),
            ),
            None => None,
        }
        .unwrap_or_else(|| status.cursor.min(status.rows.len().saturating_sub(1)));
    }

    /// The author a row stands for.
    pub(super) fn status_author(&self, key: &str) -> Option<&StoryAuthor> {
        let (part, index) = *self.status.places.get(key)?;
        let feed = &self.status.feed;
        let list = match part {
            Part::Recent => &feed.recent,
            Part::Viewed => &feed.viewed,
            Part::Muted => &feed.muted,
        };
        list.get(index).filter(|author| author.key == key)
    }

    /// What there is to say about contacts' updates right now.
    pub(super) fn status_updates(&self) -> Updates {
        if !self.engine.capabilities().story_contacts {
            return Updates::NotOffered;
        }
        let Some(account) = &self.account else {
            return Updates::Empty;
        };
        if self.engine.feature_unavailable(account, Feature::Stories) {
            return Updates::Unavailable;
        }
        let feed = &self.status.feed;
        if !(feed.recent.is_empty() && feed.viewed.is_empty() && feed.muted.is_empty()) {
            return Updates::Listed;
        }
        let listing = self.engine.story_listing(account);
        if listing.loading {
            return Updates::Loading;
        }
        let connected = self
            .accounts
            .iter()
            .find(|known| &known.id == account)
            .is_none_or(|known| known.connection.is_connected());
        if !connected {
            Updates::Offline
        } else if listing.failed {
            Updates::Failed
        } else {
            Updates::Empty
        }
    }

    /// "Check again": the provider is asked now, whatever it remembered
    /// and however lately it was asked.
    pub(super) fn check_status_again(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(account) = self.account.clone() else {
            return false;
        };
        self.engine.look_at_stories(&account, Duration::ZERO);
        cx.notify();
        true
    }

    /// Opens or folds the strip of updates above the chats.
    pub(super) fn toggle_status_strip(&mut self, cx: &mut Context<Self>) {
        crate::settings::update(cx, |settings| {
            settings.story_strip = !settings.story_strip;
        });
        cx.notify();
    }

    /// Watches the status of the author `key` of the list of new updates,
    /// from the strip above the chats: Status opens with their stories
    /// playing.
    pub(super) fn watch_status_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_list_mode(ListMode::Status, window, cx);
        self.open_story_viewer(key, Part::Recent, window, cx);
    }

    /// The account's own status, from the strip above the chats: the page
    /// of what is up and who saw it, or the sheet to post the first one.
    pub(super) fn open_my_status(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.status.feed.mine.is_empty() && self.caps_now().story_post {
            self.open_status_post(window, cx);
            return;
        }
        self.set_list_mode(ListMode::Status, window, cx);
        self.open_mine(None, cx);
    }

    /// How many updates are waiting, for the dot on the rail's button.
    pub(super) fn status_unviewed(&self) -> usize {
        self.status.feed.unviewed()
    }

    /// The ring around the picture of somebody, if they have stories.
    pub(super) fn story_ring_of(&self, contact: &ContactId) -> Option<StoryRing> {
        self.status
            .rings
            .get(contact)
            .copied()
            .filter(|ring| ring.total > 0)
    }

    /// A person's picture, with their ring when they have a status: the
    /// ring is drawn outside the picture, so the row or the header keeps
    /// its layout, and a click on it (on the picture, then) watches their
    /// status. Without a ring the picture is returned as it is.
    pub(super) fn with_story_ring(
        &self,
        face: Div,
        side: gpui_kit::Pixels,
        contact: &ContactId,
        id: SharedString,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let Some(ring) = self.story_ring_of(contact).filter(|_| self.has_status()) else {
            return face.into_any_element();
        };
        let contact = contact.clone();
        let selector = id.to_string();
        div()
            .id(id)
            .debug_selector(move || selector.clone())
            .relative()
            .flex_none()
            .size(side)
            .cursor_pointer()
            .child(super::story_ring::outset(ring, side, palette))
            .child(face)
            .tooltip(|window, cx| Tooltip::new("Watch their status").build(window, cx))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.watch_status_of(&contact, window, cx);
            }))
            .into_any_element()
    }

    /// Watches the status of a person from wherever their ring was: Status
    /// opens, with their stories playing.
    pub(super) fn watch_status_of(
        &mut self,
        contact: &ContactId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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
            .find(|(_, author)| author.ids.contains(contact))
            .map(|(part, author)| (part, author.key.clone()));
        let Some((part, key)) = found else {
            return;
        };
        self.set_list_mode(ListMode::Status, window, cx);
        self.open_story_viewer(&key, part, window, cx);
    }

    // ----- going to Status and back ----------------------------------------------------

    /// Chooses what the list pane lists.
    pub(super) fn set_list_mode(
        &mut self,
        mode: ListMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.status.mode == mode {
            return;
        }
        self.status.mode = mode;
        // What was being offered for a shortcode was for a field that is
        // not on screen any more.
        self.emoji_completion_reset();
        match mode {
            ListMode::Status => {
                self.overlay = Overlay::None;
                self.reload_status(cx);
                if let Some(account) = self.account.clone() {
                    // Somebody who goes to Status wants what is up now:
                    // asked again unless it was a moment ago, and a part
                    // that was missing is asked about again.
                    self.engine.look_at_stories(&account, STORIES_OPENED);
                    self.engine.want_story_privacy(&account);
                }
                self.status.cursor = 0;
                self.pane_list = false;
                self.keys.clear();
            }
            ListMode::Chats => {
                self.close_story_viewer(cx);
                self.status.main = Main::Empty;
                self.status.sheet = None;
            }
        }
        // The keyboard is the list's: the shell holds it.
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// A chat is being opened: Status gives the panes back.
    pub(super) fn status_yields_to_chat(&mut self, cx: &mut Context<Self>) {
        if self.status.mode == ListMode::Status {
            self.close_story_viewer(cx);
            self.status.mode = ListMode::Chats;
            self.status.main = Main::Empty;
            self.status.sheet = None;
        }
    }

    /// Another number was chosen: its Status, not the last one's.
    pub(super) fn status_account_changed(&mut self, cx: &mut Context<Self>) {
        self.close_story_viewer(cx);
        self.status.main = Main::Empty;
        self.status.mine_open = None;
        self.status.sheet = None;
        self.status.cursor = 0;
        self.reload_status(cx);
        if self.status_active() {
            if let Some(account) = self.account.clone() {
                self.engine.look_at_stories(&account, STORIES_OPENED);
            }
        }
    }

    // ----- keys that are taken before a text field sees them -------------------------

    /// Escape in the reply field leaves it (the viewer goes on), and a
    /// paste of a picture or a file in the post sheet is the status.
    /// `true`: taken.
    pub(super) fn story_key(
        &mut self,
        stroke: &gpui_kit::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        // The emoji picker's key, from a field of Status: the picker is
        // that field's. The same key closes it.
        let emoji = crate::keys::binding(Command::EmojiPicker)
            .is_some_and(|binding| binding.chords.iter().any(|chord| chord.matches(stroke)));
        if emoji {
            let picking = self.overlay == Overlay::EmojiPicker
                && self.emoji.target == super::emoji_picker::Target::Status;
            let writing = self
                .status_writing()
                .is_some_and(|field| field.focus_handle(cx).is_focused(window));
            if picking || writing {
                self.toggle_emoji_picker(window, cx);
                return true;
            }
        }
        if self.story_is_open()
            && self.overlay == Overlay::None
            && stroke.key == "escape"
            && self.status.reply.focus_handle(cx).is_focused(window)
        {
            // The emoji offered for a shortcode close first.
            if self.emoji_completion_dismiss(cx) {
                return true;
            }
            self.focus.focus(window, cx);
            self.story_typing(false, cx);
            return true;
        }
        let pasting = crate::keys::binding(Command::Paste)
            .is_some_and(|binding| binding.chords.iter().any(|chord| chord.matches(stroke)));
        if pasting && self.status.sheet == Some(super::status_post::Sheet::Post) {
            if let Some(item) = cx.read_from_clipboard() {
                return self.paste_into_status(&item, cx);
            }
        }
        false
    }

    // ----- the keyboard ------------------------------------------------------------------

    /// Runs a command of Status. `None`: not one of its commands.
    pub(super) fn run_status_command(
        &mut self,
        command: Command,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        use Command as C;
        let caps = self.caps_now();
        let ran = match command {
            C::ShowStatus if self.has_status() => {
                self.set_list_mode(ListMode::Status, window, cx);
                true
            }
            C::ShowChats => {
                self.set_list_mode(ListMode::Chats, window, cx);
                true
            }
            C::NewStatus if caps.story_post => {
                self.open_status_post(window, cx);
                true
            }
            C::SettingsStatus => {
                self.open_audience(window, cx);
                true
            }
            C::ToggleStoryReceipts => {
                settings_toggle_story_receipts(self, cx);
                true
            }
            C::ToggleStatusStrip if self.has_status() => {
                self.toggle_status_strip(cx);
                true
            }
            C::StatusRefresh => self.check_status_again(cx),
            // The list.
            C::StatusUp => self.step_status_row(-1, cx),
            C::StatusDown => self.step_status_row(1, cx),
            C::StatusFirst => self.step_status_row(isize::MIN / 2, cx),
            C::StatusLast => self.step_status_row(isize::MAX / 2, cx),
            C::StatusOpen => self.open_status_row(window, cx),
            C::StatusMute => self.mute_status_row(cx),
            C::StatusToggleMuted => {
                self.toggle_muted_part(cx);
                true
            }
            C::StatusMine => {
                self.open_mine(None, cx);
                true
            }
            // The viewer.
            C::StoryNext
            | C::StoryPrevious
            | C::StoryNextAuthor
            | C::StoryPreviousAuthor
            | C::StoryPause
            | C::StoryClose
            | C::StoryMute
            | C::StoryInfo
            | C::StoryReply
            | C::StoryReact
            | C::StoryOpenVideo
            | C::StoryDelete => return self.run_story_command(command, window, cx),
            _ => return None,
        };
        Some(ran)
    }

    /// Moves the outline `by` stops, clamped to the ends.
    fn step_status_row(&mut self, by: isize, cx: &mut Context<Self>) -> bool {
        let stops: Vec<usize> = self
            .status
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_stop())
            .map(|(index, _)| index)
            .collect();
        if stops.is_empty() {
            return false;
        }
        let at = stops
            .iter()
            .position(|index| *index >= self.status.cursor)
            .unwrap_or(0);
        let next = (at as isize)
            .saturating_add(by)
            .clamp(0, stops.len() as isize - 1) as usize;
        self.status.cursor = stops[next];
        cx.notify();
        true
    }

    /// Opens what the outlined row stands for.
    fn open_status_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.status.rows.get(self.status.cursor).cloned() {
            Some(Row::Mine) => {
                self.open_mine(None, cx);
                true
            }
            Some(Row::Heading(Part::Muted)) => {
                self.toggle_muted_part(cx);
                true
            }
            Some(Row::Author(part, key)) => {
                self.open_story_viewer(&key, part, window, cx);
                true
            }
            _ => false,
        }
    }

    fn mute_status_row(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(Row::Author(_, key)) = self.status.rows.get(self.status.cursor).cloned() else {
            return false;
        };
        self.toggle_mute_of(&key, cx)
    }

    /// Mutes the author's stories, or lets them back: shown at once, and
    /// told to the provider when it keeps mutes.
    pub(super) fn toggle_mute_of(&mut self, key: &str, cx: &mut Context<Self>) -> bool {
        let (Some(account), Some(author)) = (
            self.account.clone(),
            self.status_author(key)
                .map(|author| (author.author.clone(), author.muted)),
        ) else {
            return false;
        };
        self.engine.set_story_muted(&account, &author.0, !author.1);
        cx.notify();
        true
    }

    fn toggle_muted_part(&mut self, cx: &mut Context<Self>) {
        self.status.muted_open = !self.status.muted_open;
        self.rebuild_status_rows();
        cx.notify();
    }

    // ----- the way in ------------------------------------------------------------------

    /// The rail's navigation: Chats and Status, under the logo. The one
    /// that is showing is marked, and Status wears a dot while somebody
    /// has something new.
    pub(super) fn render_rail_nav(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let status = self.status_active();
        let news = self.status_unviewed() > 0;
        let button = |id: &'static str, glyph: IconName, on: bool| {
            let (hover, ring) = (palette.muted, palette.accent);
            super::hints::hinted(
                div()
                    .id(id)
                    .debug_selector(move || id.into())
                    .relative()
                    .flex_none()
                    .size(metrics::CONTROL())
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .tab_index(0)
                    .hover(move |style| style.bg(hover))
                    .focus_visible(move |style| style.border_color(ring))
                    .when(on, |this| this.bg(palette.muted))
                    .child(icon(
                        glyph,
                        px(18.),
                        if on { palette.text } else { palette.icon },
                    )),
                id,
            )
        };
        // The one that is showing has the accent on the rail's edge.
        let cell = |on: bool, mark: &'static str, child: Stateful<Div>| {
            div()
                .relative()
                .w_full()
                .flex()
                .justify_center()
                .when(on, |this| {
                    this.child(
                        div()
                            .debug_selector(move || mark.into())
                            .absolute()
                            .left_0()
                            .top_0()
                            .h_full()
                            .w(px(2.))
                            .bg(palette.accent),
                    )
                })
                .child(child)
        };
        div()
            .debug_selector(|| "rail-nav".into())
            .flex_none()
            .w_full()
            .py(px(6.))
            .border_b_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .items_center()
            .gap(px(4.))
            .child(cell(
                !status,
                "nav-on-chats",
                button("nav-chats", IconName::MessageCircle, !status).on_click(cx.listener(
                    |this, _, window, cx| this.set_list_mode(ListMode::Chats, window, cx),
                )),
            ))
            .child(cell(
                status,
                "nav-on-status",
                button("nav-status", IconName::CircleDotDashed, status)
                    // Something new: the brand's signal, as an unread
                    // count is, and no number (the list has them).
                    .when(news, |this| {
                        this.child(
                            div()
                                .debug_selector(|| "status-dot".into())
                                .absolute()
                                .top(px(1.))
                                .right(px(1.))
                                .size(px(9.))
                                .rounded_full()
                                .border_1()
                                .border_color(palette.background)
                                .bg(palette.accent),
                        )
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_list_mode(ListMode::Status, window, cx)
                    })),
            ))
    }

    /// The title of the list pane: what it lists.
    pub(super) fn render_list_title(&self, palette: &Palette) -> impl IntoElement {
        div()
            .debug_selector(|| "list-title".into())
            .text_size(metrics::TEXT_TITLE())
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(palette.text)
            .child(if self.status_active() {
                "Status"
            } else {
                "Chats"
            })
    }

    /// The header's button in Status: a new status.
    pub(super) fn render_new_status_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let button = icon_button("new-status", IconName::Plus, palette);
        if self.caps_now().story_post {
            button
                .tooltip(tooltip_with_keys("New status", Command::NewStatus))
                .on_click(cx.listener(|this, _, window, cx| this.open_status_post(window, cx)))
        } else {
            super::widgets::unavailable_button(
                "new-status",
                IconName::Plus,
                "Posting a status is not available yet from this provider",
                palette,
            )
        }
    }

    // ----- the list pane -------------------------------------------------------------------

    /// The list pane: the chats, or Status.
    pub(super) fn render_list_pane(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        if !self.status_active() {
            return self.render_chat_list(palette, cx).into_any_element();
        }
        let palette = *palette;
        let rows = self.status.rows.len();
        let note = self.render_status_note(&palette, cx);
        // With something to say under the rows, the list is as high as
        // its rows (less when the pane is) and the words follow it;
        // without, it takes the pane.
        let list = div().relative().min_h_0();
        let list = if note.is_some() {
            list.h(story::ROW() * rows as f32).flex_shrink(1.)
        } else {
            list.flex_1()
        };
        div()
            .id("status-pane")
            .debug_selector(|| "status-pane".into())
            .flex_none()
            .relative()
            .w(self.list_px)
            .h_full()
            .bg(palette.background)
            .border_r_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .child(self.render_list_header(&palette, cx))
            .children(self.problem.clone().map(|problem| {
                div()
                    .flex_none()
                    .debug_selector(|| "problem".into())
                    .px_4()
                    .py(px(10.))
                    .border_b_1()
                    .border_color(palette.border)
                    .bg(palette.muted)
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(18.))
                    .text_color(palette.text)
                    .child(problem)
            }))
            .child(
                // Only the rows on screen are built, as in the chat list:
                // the list is redrawn with the window, and while a story
                // plays that is many times a second.
                list.child(
                    uniform_list(
                        "status-rows",
                        rows,
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            let now = Timestamp::now();
                            range
                                .map(|index| this.render_status_row(index, now, &palette, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.status.scroll)
                    .size_full(),
                )
                .child(Scrollbar::vertical(&self.status.scroll)),
            )
            .children(note)
            .into_any_element()
    }

    /// One row of the list. Every row is [`story::ROW`] high.
    fn render_status_row(
        &self,
        index: usize,
        now: Timestamp,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let keyboard = self.status.sheet.is_none() && self.status.viewing.is_none();
        let outlined = keyboard && index == self.status.cursor;
        match self.status.rows.get(index) {
            Some(Row::Mine) => self
                .render_mine_row(outlined, now, palette, cx)
                .into_any_element(),
            Some(Row::Heading(part)) => self
                .render_heading(*part, outlined, palette, cx)
                .into_any_element(),
            Some(Row::Author(part, key)) => match self.status_author(key) {
                Some(author) => {
                    let ordinal = self.status.ordinals.get(index).copied().unwrap_or(0);
                    self.render_author_row(
                        (index, ordinal),
                        author,
                        *part,
                        outlined,
                        now,
                        palette,
                        cx,
                    )
                    .into_any_element()
                }
                None => div().h(story::ROW()).into_any_element(),
            },
            None => div().h(story::ROW()).into_any_element(),
        }
    }

    /// Why nobody is listed, said under the rows with what to do next:
    /// the list is on its way, the number is offline, the provider or its
    /// backend does not have contacts' updates, or nobody posted anything.
    fn render_status_note(&self, palette: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        let updates = self.status_updates();
        if updates == Updates::Listed {
            return None;
        }
        let (title, detail) = updates.words();
        let name = updates.name();
        let can_post = self.caps_now().story_post;
        Some(
            div()
                .debug_selector(|| "status-none".into())
                .flex_none()
                .px_4()
                .py_6()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .debug_selector(move || format!("status-state-{name}"))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .when(!title.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_size(metrics::TEXT_NAME())
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(palette.text)
                                    .child(title),
                            )
                        })
                        .when(!detail.is_empty(), |this| {
                            this.child(
                                div()
                                    .text_size(metrics::TEXT_SMALL())
                                    .line_height(px(19.))
                                    .text_color(palette.text_muted)
                                    .child(detail),
                            )
                        }),
                )
                .child(
                    div()
                        .pt_2()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap_2()
                        .when(updates == Updates::Empty && can_post, |this| {
                            this.child(
                                text_button(
                                    "status-post-first",
                                    "Post a status",
                                    Some(IconName::Plus),
                                    true,
                                    palette,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.open_status_post(window, cx),
                                )),
                            )
                        })
                        .when(updates.can_check(), |this| {
                            this.child(
                                text_button("status-check", "Check again", None, false, palette)
                                    .tooltip(tooltip_with_keys(
                                        "Check again",
                                        Command::StatusRefresh,
                                    ))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.check_status_again(cx);
                                    })),
                            )
                        }),
                ),
        )
    }

    fn render_heading(
        &self,
        part: Part,
        outlined: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let foldable = part == Part::Muted;
        let count = self.status.feed.muted.len();
        let open = self.status.muted_open;
        let name = match part {
            Part::Muted => format!("Muted ({count})"),
            other => other.title().to_owned(),
        };
        let heading = div()
            .id(("status-heading", part as usize))
            .debug_selector(move || format!("status-heading-{part:?}").to_lowercase())
            // As high as any row (the list is built from rows of one
            // height), with its words at the foot, as the chat list's
            // section titles are.
            .w_full()
            .h(story::ROW())
            .px_4()
            .pb_2()
            .flex()
            .items_end()
            .justify_between()
            .when(outlined, |this| this.bg(palette.muted))
            .child(label(&name, palette));
        if foldable {
            heading
                .cursor_pointer()
                .hover(|style| style.opacity(0.8))
                .child(icon(
                    if open {
                        IconName::ChevronUp
                    } else {
                        IconName::ChevronDown
                    },
                    px(14.),
                    palette.text_faint,
                ))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_muted_part(cx)))
        } else {
            heading
        }
    }

    fn render_mine_row(
        &self,
        outlined: bool,
        now: Timestamp,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let account = self.account.clone();
        let mine = &self.status.feed.mine;
        let latest = mine.last();
        let picture = account.as_ref().and_then(|account| {
            self.accounts
                .iter()
                .find(|known| &known.id == account)
                .and_then(client_core::own_picture_subject)
                .and_then(|subject| self.media.stored_picture(account, &subject))
        });
        let name = self.current_account_name().unwrap_or_default();
        let face = avatar_or(picture, &name, AvatarKind::Person, story::AVATAR(), palette);
        let ring = (!mine.is_empty()).then_some(StoryRing {
            total: mine.len(),
            // Your own are never news to you.
            unviewed: 0,
        });
        let subtitle: SharedString = match latest {
            Some(item) => {
                let views = item
                    .story
                    .view_count
                    .filter(|_| self.caps_now().story_viewers)
                    .map(|count| match count {
                        1 => " · 1 view".to_owned(),
                        n => format!(" · {n} views"),
                    })
                    .unwrap_or_default();
                match &item.post {
                    Some(client_core::StoryPostState::Failed(_)) => "Not posted. Tap to see".into(),
                    Some(client_core::StoryPostState::Queued) => "Posting…".into(),
                    None => format!("{}{views}", age_label(item.story.posted_at, now)).into(),
                }
            }
            None => "Add status".into(),
        };
        div()
            .id("status-mine")
            .debug_selector(|| "status-mine".into())
            .w_full()
            .h(story::ROW())
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .when(outlined, |this| this.bg(palette.muted))
            .hover({
                let hover = palette.hover;
                move |style| style.bg(hover)
            })
            .on_click(cx.listener(|this, _, _, cx| this.open_mine(None, cx)))
            .child(super::story_ring::with_ring(face, ring, palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_NAME())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("My status"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "status-mine-line".into())
                            .truncate()
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text_muted)
                            .child(subtitle),
                    ),
            )
            .child(
                icon_button("status-add", IconName::Plus, palette)
                    .tooltip(tooltip_with_keys("New status", Command::NewStatus))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        if this.caps_now().story_post {
                            this.open_status_post(window, cx);
                        }
                    })),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn render_author_row(
        &self,
        (row, index): (usize, usize),
        author: &StoryAuthor,
        part: Part,
        outlined: bool,
        now: Timestamp,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let account = self.account.clone().unwrap_or_else(|| AccountId::new(""));
        let name = author.display_name();
        let key = author.key.clone();
        let latest = author.stories.last();
        let ring = StoryRing {
            total: author.stories.len(),
            unviewed: author.unviewed(),
        };
        let picture = self.media.avatar(
            &account,
            &client_provider::ChatId::new(author.author.as_str()),
        );
        let tone = self.senders.of(&account, None, &author.author).tone;
        let face = super::senders::person_avatar(picture, &name, story::AVATAR(), tone, palette);
        let line = latest.map_or(String::new(), |item| describe(&item.story.body));
        let when = latest.map_or(String::new(), |item| age_label(item.story.posted_at, now));
        let new = part == Part::Recent;
        let muted = author.muted;
        let click_key = key.clone();
        let mute_key = key.clone();
        div()
            .id(("status-author", row))
            .debug_selector(move || format!("status-author-{index}"))
            // As wide as the list, whatever is written in it: a long
            // name is cut, it does not widen the row.
            .w_full()
            .h(story::ROW())
            .px_3()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .when(outlined, |this| this.bg(palette.muted))
            .hover({
                let hover = palette.hover;
                move |style| style.bg(hover)
            })
            .group("status-row")
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_story_viewer(&click_key, part, window, cx)
            }))
            .child(super::story_ring::with_ring(face, Some(ring), palette))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_NAME())
                            .font_weight(if new {
                                FontWeight::SEMIBOLD
                            } else {
                                FontWeight::NORMAL
                            })
                            .child(SharedString::from(name)),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text_muted)
                            .child(SharedString::from(line)),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .flex_col()
                    .items_end()
                    .gap(px(2.))
                    .child(
                        mono(when)
                            .debug_selector(move || format!("status-time-{index}"))
                            .text_color(if new {
                                palette.accent
                            } else {
                                palette.text_faint
                            }),
                    )
                    .child(
                        div()
                            .id(("status-mute", row))
                            .debug_selector(move || format!("status-mute-{index}"))
                            .size(px(20.))
                            .rounded(px(4.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover({
                                let hover = palette.muted;
                                move |style| style.bg(hover)
                            })
                            .tooltip(tooltip_with_keys(
                                if muted { "Unmute" } else { "Mute" },
                                Command::StatusMute,
                            ))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.toggle_mute_of(&mute_key, cx);
                            }))
                            .child(icon(
                                if muted {
                                    IconName::Bell
                                } else {
                                    IconName::BellOff
                                },
                                px(13.),
                                palette.text_faint,
                            )),
                    ),
            )
    }

    // ----- the main pane ------------------------------------------------------------------

    /// What the conversation's place shows while Status is chosen.
    pub(super) fn render_status_main(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let pane = div()
            .id("status-main")
            .debug_selector(|| "status-main".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(palette.background);
        match self.status.main {
            Main::Viewing if self.status.viewing.is_some() => pane
                .child(self.render_story_viewer(palette, window, cx))
                .into_any_element(),
            Main::Mine => pane.child(self.render_mine(palette, cx)).into_any_element(),
            _ => pane
                .child(self.render_status_welcome(palette, cx))
                .into_any_element(),
        }
    }

    /// Nothing chosen yet: what Status is, and the way to a new one.
    fn render_status_welcome(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let caps = self.caps_now();
        let can_post = caps.story_post;
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_4()
            .p_8()
            .child(
                div()
                    .debug_selector(|| "status-welcome".into())
                    .max_w(px(420.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_center()
                    .child(
                        div()
                            .text_size(metrics::TEXT_TITLE())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Status"),
                    )
                    .child(
                        div()
                            .text_size(metrics::TEXT_BODY())
                            .line_height(px(22.))
                            .text_color(palette.text_muted)
                            .child(
                                "Pick an update from the list to watch it. Updates last a day, \
                                 and the person who posted one sees that you saw it unless \
                                 view receipts are off.",
                            ),
                    ),
            )
            .child(if can_post {
                text_button(
                    "status-new",
                    "New status",
                    Some(IconName::Plus),
                    true,
                    palette,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_status_post(window, cx)))
                .into_any_element()
            } else {
                div()
                    .debug_selector(|| "status-new-unavailable".into())
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_faint)
                    .child("Posting a status is not available yet from this provider.")
                    .into_any_element()
            })
            .child(
                div()
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_faint)
                    .child(SharedString::from(format!(
                        "{} to come back to the chats",
                        crate::keys::keys_label(Command::ShowChats).unwrap_or_default()
                    ))),
            )
    }
}

/// A tooltip: a name and the keys of its command.
pub(super) fn tooltip_with_keys(
    name: &'static str,
    command: Command,
) -> impl Fn(&mut Window, &mut gpui_kit::App) -> gpui_kit::AnyView + 'static {
    let text = super::hints::with_keys(name, Some(command));
    move |window, cx| Tooltip::new(text.clone()).build(window, cx)
}

/// Turns the story view receipts on or off in the settings.
fn settings_toggle_story_receipts(shell: &mut Shell, cx: &mut Context<Shell>) {
    crate::settings::update(cx, |settings| {
        settings.story_receipts = !settings.story_receipts;
    });
    cx.notify();
    let _ = shell;
}
