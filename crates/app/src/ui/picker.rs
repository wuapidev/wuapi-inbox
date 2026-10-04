//! The picker's two other tabs, Stickers and GIFs, around the emoji one.
//!
//! One popover over the composer, opened by its button on the tab used
//! last, or by Cmd or Ctrl with E (Emoji), Shift+E (Stickers) and Shift+J
//! (GIFs). The Emoji tab is the emoji picker as it was (`emoji_picker.rs`,
//! embedded: this file draws the bar of tabs above it and the other two
//! bodies, and asks it for the rest); the keyboard behaves the same in all
//! three. It never leaves the search field: typing searches, Tab walks
//! search, tabs and grid, the arrows walk the grid, Enter sends (with
//! Shift the picker stays open), Alt with Down offers what can be done to
//! the tile the keyboard is on, and Escape empties the search before it
//! closes.
//!
//! The grids are lists of rows of which only those in view are built, and
//! their pictures are small stills read from the encrypted store when a row
//! is first drawn. **Nothing moves except the tile the pointer or the
//! keyboard is on**, one at a time, and not at all under reduced motion
//! or with "Animate stickers and GIFs on hover" off.

use super::emoji_picker::{Target, Zone, BAR, GRID_WIDTH};
use super::shell::{Overlay, Shell};
use super::widgets::mono;
use crate::animation::{Animation, Motion, Playback};
use crate::gifs::{GifBackend, GifEntry, GifHit, Giphy, SavedGifs, SecretStore};
use crate::icons::{icon, IconName};
use crate::keys::{self, Command};
use crate::settings;
use crate::theme::{metrics, px, Palette};
use client_core::{AnimationLimits, LibraryItem, LibraryKind, Store};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, div, img, AnyElement, Context, Div, FontWeight, Image, Keystroke, ListAlignment,
    ListOffset, ListState, ObjectFit, Pixels, RenderImage, SharedString, StyledImage, Task, Window,
};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(super) use crate::settings::PickerTab as Tab;

/// Stickers per row, and GIFs.
const STICKER_COLUMNS: usize = 4;
const GIF_COLUMNS: usize = 3;
/// The most thumbnails kept in memory at once.
const SHELF_LIMIT: usize = 240;
/// How long the online search waits after a keystroke: a word is one
/// request.
pub(super) const ONLINE_DEBOUNCE: Duration = Duration::from_millis(350);
/// The shortest wait between two frames painted.
const MIN_TICK: Duration = Duration::from_millis(30);

/// What the tabs say when there is nothing to show. Each is one short
/// line, written once: the grid wraps it inside the popover.
pub(super) mod texts {
    pub(in crate::ui) const FAVORITES_LOCAL: &str =
        "Your phone's favorites are not available yet from this provider. \
         Stickers you star here are kept as favorites.";
    pub(in crate::ui) const FAVORITES_NOT_YET: &str =
        "Your phone's favorites are not available yet for this number. \
         Stickers you star here are kept as favorites.";
    pub(in crate::ui) const FAVORITES_SYNCED: &str =
        "Star a sticker to keep it here. Favorites sync with your phone.";
    pub(in crate::ui) const FROM_CHATS_EMPTY: &str =
        "Stickers you send or receive in your chats show up here.";
    pub(in crate::ui) const NO_STICKER_FOUND: &str =
        "No sticker has this name. Stickers are found by name and pack.";
    pub(in crate::ui) const NO_SAVED_GIFS: &str =
        "Save a GIF from a conversation (message menu, or G), or add files with +.";
    pub(in crate::ui) const NO_GIF_FOUND: &str = "No saved GIF has this name.";
    pub(in crate::ui) const ONLINE_OFF: &str =
        "Online search is off. Turn it on, with your own key, in Settings > Stickers and GIFs.";
    pub(in crate::ui) const ONLINE_NO_KEY: &str =
        "Online search needs your own key. Add it in Settings > Stickers and GIFs.";
    pub(in crate::ui) const SAVED_GIFS_HERE: &str = "Saved GIFs appear here.";
}

/// What the picker's moving tile may cost: a tile is small.
fn limits() -> AnimationLimits {
    AnimationLimits {
        max_canvas: 1024,
        max_side: 160,
        min_side: 64,
        max_frames: 120,
        max_bytes: 8 * 1024 * 1024,
    }
}

#[allow(non_snake_case)]
pub(super) fn STICKER_CELL() -> Pixels {
    GRID_WIDTH() / STICKER_COLUMNS as f32
}

#[allow(non_snake_case)]
pub(super) fn GIF_CELL_W() -> Pixels {
    GRID_WIDTH() / GIF_COLUMNS as f32
}

#[allow(non_snake_case)]
pub(super) fn GIF_CELL_H() -> Pixels {
    px(81.)
}

/// Where the GIFs are looked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// The library's.
    Saved,
    /// The online service's, when the user turned it on.
    Online,
}

/// One tile of a grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Tile {
    pub(super) kind: TileKind,
}

/// What a tile is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TileKind {
    Sticker(LibraryItem),
    Gif(GifEntry),
}

impl Tile {
    /// What the shelf and the animation know it by.
    pub(super) fn key(&self) -> String {
        match &self.kind {
            TileKind::Sticker(item) | TileKind::Gif(GifEntry::Saved(item)) => item.id.clone(),
            TileKind::Gif(GifEntry::Online(hit)) => format!("online:{}", hit.id),
        }
    }

    /// The library item behind it, when there is one.
    pub(super) fn item(&self) -> Option<&LibraryItem> {
        match &self.kind {
            TileKind::Sticker(item) | TileKind::Gif(GifEntry::Saved(item)) => Some(item),
            TileKind::Gif(GifEntry::Online(_)) => None,
        }
    }

    pub(super) fn name(&self) -> String {
        match &self.kind {
            TileKind::Sticker(item) | TileKind::Gif(GifEntry::Saved(item)) => {
                item.name.clone().unwrap_or_default()
            }
            TileKind::Gif(GifEntry::Online(hit)) => hit.title.clone(),
        }
    }
}

/// One row of a grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Row {
    /// A section's heading.
    Header(usize),
    /// A line of words in the grid's place.
    Note(SharedString),
    /// Up to a row's worth of tiles.
    Tiles(Range<usize>),
}

/// A section, as the grid lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Section {
    pub(super) title: SharedString,
    pub(super) icon: IconName,
    pub(super) row: usize,
    pub(super) tile: usize,
}

/// What a tab's grid shows.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Grid {
    pub(super) rows: Vec<Row>,
    pub(super) tiles: Vec<Tile>,
    tile_rows: Vec<usize>,
    pub(super) sections: Vec<Section>,
    columns: usize,
    /// The tile the keyboard, or the pointer, is on.
    pub(super) cursor: usize,
}

impl Grid {
    fn new(columns: usize) -> Self {
        Self {
            columns,
            ..Default::default()
        }
    }

    fn section(&mut self, title: &str, icon: IconName) {
        self.sections.push(Section {
            title: title.to_owned().into(),
            icon,
            row: self.rows.len(),
            tile: self.tiles.len(),
        });
        self.rows.push(Row::Header(self.sections.len() - 1));
    }

    fn note(&mut self, text: impl Into<SharedString>) {
        self.rows.push(Row::Note(text.into()));
    }

    fn fill(&mut self, tiles: Vec<Tile>) {
        for chunk in tiles.chunks(self.columns.max(1)) {
            let start = self.tiles.len();
            self.tile_rows
                .extend(std::iter::repeat_n(self.rows.len(), chunk.len()));
            self.tiles.extend_from_slice(chunk);
            self.rows.push(Row::Tiles(start..self.tiles.len()));
        }
    }

    /// The tile a row up or down from `tile`, in the same column where the
    /// row has one, across headings and notes.
    fn step_row(&self, tile: usize, down: bool) -> Option<usize> {
        let row = *self.tile_rows.get(tile)?;
        let Row::Tiles(range) = &self.rows[row] else {
            return None;
        };
        let column = tile - range.start;
        let mut at = row;
        loop {
            at = if down { at + 1 } else { at.checked_sub(1)? };
            if let Row::Tiles(range) = self.rows.get(at)? {
                return Some(range.start + column.min(range.len() - 1));
            }
        }
    }

    pub(super) fn section_of(&self, tile: usize) -> usize {
        self.sections
            .iter()
            .rposition(|section| section.tile <= tile)
            .unwrap_or(0)
    }

    /// The titles of the sections, for tests.
    #[cfg(test)]
    pub(super) fn titles(&self) -> Vec<String> {
        self.sections.iter().map(|s| s.title.to_string()).collect()
    }
}

/// What can be done to the tile the keyboard is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Action {
    /// Send it.
    Send,
    /// Star it, or take the star off.
    Favorite,
    /// Keep an online GIF in the library.
    Keep,
    /// One place earlier among the favorites.
    Earlier,
    /// One place later.
    Later,
    /// Take it out of the library.
    Remove,
}

impl Action {
    pub(super) fn label(self, on: &Tile) -> &'static str {
        match self {
            Self::Send if on.item().is_some_and(LibraryItem::pending) => "Download",
            Self::Send => "Send",
            Self::Favorite if on.item().is_some_and(|item| item.favorite) => "Take off favorites",
            Self::Favorite => "Add to favorites",
            Self::Keep => "Save to my GIFs",
            Self::Earlier => "Move earlier",
            Self::Later => "Move later",
            Self::Remove => "Remove from the library",
        }
    }
}

/// The menu of the tile under the keyboard, offered in the strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Menu {
    pub(super) tile: usize,
    pub(super) actions: Vec<Action>,
    pub(super) cursor: usize,
}

/// Where an online search stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Status {
    Idle,
    Loading,
    Failed(SharedString),
}

/// The online GIFs on show.
#[derive(Debug)]
pub(super) struct Online {
    pub(super) hits: Vec<GifHit>,
    pub(super) status: Status,
    /// The query the hits answer.
    pub(super) answers: Option<String>,
    serial: u64,
}

/// What the online search needs: where the key is kept, where the
/// service is, and the search made from them.
pub(super) struct Services {
    pub(super) secrets: Arc<dyn SecretStore>,
    /// The service's API address: overridden by tests.
    pub(super) base: String,
    /// The key as last read; `None` until read.
    key: RefCell<Option<Option<String>>>,
}

impl Services {
    pub(super) fn new(secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            secrets,
            base: crate::gifs::GIPHY_BASE.to_owned(),
            key: RefCell::new(None),
        }
    }

    /// The key, read from the keychain once until it is told it changed.
    pub(super) fn key(&self) -> Option<String> {
        let mut known = self.key.borrow_mut();
        known
            .get_or_insert_with(|| self.secrets.get().ok().flatten().filter(|k| !k.is_empty()))
            .clone()
    }

    pub(super) fn forget_key(&self) {
        *self.key.borrow_mut() = None;
    }
}

/// A moving tile's frames and where it is in them.
struct Moving {
    key: String,
    animation: Rc<Animation>,
    playback: Playback,
}

/// Small pictures of tiles, by key, and what is being fetched for them.
struct Shelf {
    store: Arc<Store>,
    thumbs: RefCell<HashMap<String, Option<Arc<Image>>>>,
    /// Online previews, by URL.
    online: RefCell<HashMap<String, Option<Arc<Image>>>>,
    asked: RefCell<HashSet<String>>,
}

impl Shelf {
    fn thumb(&self, item: &LibraryItem) -> Option<Arc<Image>> {
        if !item.has_thumb {
            return None;
        }
        let mut thumbs = self.thumbs.borrow_mut();
        if let Some(known) = thumbs.get(&item.id) {
            return known.clone();
        }
        if thumbs.len() > SHELF_LIMIT {
            thumbs.clear();
        }
        let image = self
            .store
            .library_thumb(&item.id)
            .ok()
            .flatten()
            .and_then(|(bytes, mime)| super::media::image_of(bytes, Some(&mime)));
        thumbs.insert(item.id.clone(), image.clone());
        image
    }

    fn clear(&self) {
        self.thumbs.borrow_mut().clear();
        self.online.borrow_mut().clear();
        self.asked.borrow_mut().clear();
    }
}

/// When the next repaint of the window is due, if one is asked for.
#[derive(Default)]
struct Wake {
    due: Cell<Option<Instant>>,
    serial: Cell<u64>,
}

/// The picker's state beside the emoji one.
pub(super) struct PickerUi {
    pub(super) tab: Tab,
    pub(super) source: Source,
    pub(super) stickers: Grid,
    pub(super) gifs: Grid,
    /// The tab the keyboard is on, while it is in the bar of tabs.
    pub(super) tab_cursor: usize,
    list_stickers: ListState,
    list_gifs: ListState,
    /// The rows built for the last frame.
    pub(super) built: Rc<Cell<Range<usize>>>,
    pub(super) menu: Option<Menu>,
    /// A shortcut just changed the tab: the same keystroke reaching the
    /// composer's own handler is not a second toggle.
    pub(super) switched: bool,
    shelf: Shelf,
    /// The tile the pointer is on.
    hover: Option<String>,
    /// What the tile on show moves as, and the frames it was let go of.
    moving: Rc<RefCell<Option<Moving>>>,
    /// The key of the tile whose animation is being decoded.
    decoding: Option<String>,
    released: Rc<RefCell<Vec<Arc<RenderImage>>>>,
    wake: Rc<Wake>,
    pub(super) online: Online,
    pub(super) services: Services,
    /// What the strip says instead of the tile's name, for a moment.
    pub(super) notice: Option<SharedString>,
    /// The tiles asked to be sent and not queued yet, by key: a second
    /// click or a repeated Enter on one of them is the same request, not
    /// another message. It outlives the picker: the work does too.
    sending: HashSet<String>,
    _tasks: Vec<Task<()>>,
    _online: Option<Task<()>>,
    _notice: Option<Task<()>>,
}

impl PickerUi {
    pub(super) fn new(store: Arc<Store>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            tab: Tab::Emoji,
            source: Source::Saved,
            stickers: Grid::new(STICKER_COLUMNS),
            gifs: Grid::new(GIF_COLUMNS),
            tab_cursor: 0,
            list_stickers: ListState::new(0, ListAlignment::Top, px(60.)),
            list_gifs: ListState::new(0, ListAlignment::Top, px(60.)),
            built: Rc::new(Cell::new(0..0)),
            menu: None,
            switched: false,
            shelf: Shelf {
                store,
                thumbs: RefCell::default(),
                online: RefCell::default(),
                asked: RefCell::default(),
            },
            hover: None,
            moving: Rc::default(),
            decoding: None,
            released: Rc::default(),
            wake: Rc::default(),
            online: Online {
                hits: Vec::new(),
                status: Status::Idle,
                answers: None,
                serial: 0,
            },
            services: Services::new(secrets),
            notice: None,
            sending: HashSet::new(),
            _tasks: Vec::new(),
            _online: None,
            _notice: None,
        }
    }

    /// The grid of the tab on show.
    pub(super) fn grid(&self) -> &Grid {
        match self.tab {
            Tab::Gifs => &self.gifs,
            _ => &self.stickers,
        }
    }

    fn grid_mut(&mut self) -> &mut Grid {
        match self.tab {
            Tab::Gifs => &mut self.gifs,
            _ => &mut self.stickers,
        }
    }

    /// The list of rows of the tab on show.
    pub(super) fn scroll(&self) -> &ListState {
        match self.tab {
            Tab::Gifs => &self.list_gifs,
            _ => &self.list_stickers,
        }
    }

    /// Tells the lists how many rows the grids have now, keeping where
    /// each is scrolled to: the rows have different heights (a heading, a
    /// line of words, tiles), so the list measures what it draws.
    pub(super) fn sync_lists(&self) {
        for (list, grid) in [
            (&self.list_stickers, &self.stickers),
            (&self.list_gifs, &self.gifs),
        ] {
            let at = list.logical_scroll_top();
            list.reset(grid.rows.len());
            if at.item_ix < grid.rows.len() {
                list.scroll_to(at);
            }
        }
    }

    /// Scrolls the grid on show to its first row.
    pub(super) fn scroll_to_top(&self) {
        self.scroll().scroll_to(ListOffset::default());
    }

    /// Lets go of the frames the window holds for what stopped moving.
    pub(super) fn release(&self, window: &mut Window) {
        for image in self.released.borrow_mut().drain(..) {
            let _ = window.drop_image(image);
        }
    }

    /// The picker closed: everything it held is let go.
    pub(super) fn close(&mut self) {
        self.menu = None;
        self.hover = None;
        self.decoding = None;
        self.notice = None;
        self._tasks.clear();
        self._online = None;
        self.stop_moving();
        self.shelf.clear();
        self.stickers = Grid::new(STICKER_COLUMNS);
        self.gifs = Grid::new(GIF_COLUMNS);
        self.sync_lists();
        self.online.hits.clear();
        self.online.status = Status::Idle;
        self.online.answers = None;
    }

    fn stop_moving(&self) {
        if let Some(old) = self.moving.borrow_mut().take() {
            self.released.borrow_mut().push(old.animation.image.clone());
        }
    }

    /// The rows built for the last frame, for tests.
    #[cfg(test)]
    pub(super) fn built_rows(&self) -> Range<usize> {
        let rows = self.built.take();
        self.built.set(rows.clone());
        rows
    }

    /// The key of the tile whose frames are on show, for tests.
    #[cfg(test)]
    pub(super) fn moving_key(&self) -> Option<String> {
        self.moving
            .borrow()
            .as_ref()
            .map(|moving| moving.key.clone())
    }

    /// The tile the keyboard or the pointer is on, in the grid on show.
    pub(super) fn current(&self) -> Option<&Tile> {
        let grid = self.grid();
        grid.tiles.get(grid.cursor)
    }
}

// ----- the actions -----------------------------------------------------------------------

impl Shell {
    /// Whether stickers and GIFs can be sent now, and if not, why: the
    /// tabs say it instead of failing after a click.
    pub(super) fn picker_cannot_send(&self) -> Option<&'static str> {
        if !self.engine.capabilities().media_upload {
            return Some("This provider cannot send files, so stickers and GIFs cannot be sent.");
        }
        match self.engine.uploads_available() {
            Some(true) => None,
            Some(false) => Some("Sending files is not available yet, so nothing can be sent."),
            None => Some("Checking whether files can be sent…"),
        }
    }

    /// Opens the picker on a tab, from the composer; on the tab it is on,
    /// the picker closes; on another it changes tab.
    pub(super) fn open_media_picker(
        &mut self,
        tab: Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Only over a conversation that is on screen: never over Status,
        // where a pick would go to the chat behind it.
        if !self.conversation_showing() {
            return;
        }
        if self.picker.switched {
            return;
        }
        // While files are being attached the picker is the caption's, and
        // it is emoji only: a sticker or a GIF is a message of its own, sent
        // at once, so its tabs wait until the sheet is sent or left.
        if self.attach.is_some() && tab != Tab::Emoji {
            return;
        }
        if self.overlay == Overlay::EmojiPicker && self.emoji.target == Target::Composer {
            if self.picker.tab == tab {
                return self.close_overlay(window, cx);
            }
            return self.picker_set_tab(tab, window, cx);
        }
        self.toggle_emoji_picker_on(Some(tab), window, cx);
    }

    /// The tab another tab's shortcut asks for, when the picker is on
    /// this one: a shortcut changes tab where the tab's own closes.
    pub(super) fn picker_switch_for(&self, command: Option<Command>) -> Option<Tab> {
        if self.emoji.target != Target::Composer {
            return None;
        }
        let tab = match command? {
            Command::EmojiPicker => Tab::Emoji,
            Command::StickerPicker => Tab::Stickers,
            Command::GifPicker => Tab::Gifs,
            _ => return None,
        };
        (tab != self.picker.tab).then_some(tab)
    }

    /// Changes tab for a shortcut, once for the keystroke.
    pub(super) fn picker_switch_by_key(
        &mut self,
        tab: Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker_set_tab(tab, window, cx);
        self.picker.switched = true;
        let view = cx.weak_entity();
        window.defer(cx, move |_, cx| {
            view.update(cx, |this, _| this.picker.switched = false).ok();
        });
    }

    /// Whether `command` is the shortcut that opened the tab on show: it
    /// closes the picker, where another tab's shortcut changes tab.
    pub(super) fn picker_is_opener(&self, command: Option<Command>) -> bool {
        let composer = self.emoji.target == Target::Composer;
        match (self.picker.tab, command) {
            (Tab::Emoji, Some(Command::EmojiPicker)) => true,
            (Tab::Stickers, Some(Command::StickerPicker)) => composer,
            (Tab::Gifs, Some(Command::GifPicker)) => composer,
            _ => false,
        }
    }

    /// The picker has just opened on its tab: the field says what it
    /// searches and the grid is read from the store.
    pub(super) fn picker_opened(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker_placeholder(window, cx);
        self.picker_load(cx);
    }

    fn picker_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = match self.picker.tab {
            Tab::Emoji => "Search emoji",
            Tab::Stickers => "Search stickers by name or pack",
            Tab::Gifs if self.picker.source == Source::Online => "Search GIFs online",
            Tab::Gifs => "Search saved GIFs",
        };
        self.emoji
            .search
            .update(cx, |field, cx| field.set_placeholder(text, window, cx));
    }

    /// Changes the tab of the open picker.
    pub(super) fn picker_set_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.tab == tab {
            return;
        }
        self.picker.tab = tab;
        self.picker.tab_cursor = tab_index(tab);
        self.picker.menu = None;
        self.picker.hover = None;
        self.picker.notice = None;
        self.picker.stop_moving();
        self.picker.decoding = None;
        settings::update(cx, |chosen| chosen.picker_tab = tab);
        self.emoji
            .search
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.emoji
            .search
            .update(cx, |field, cx| field.focus(window, cx));
        self.emoji_reset_search(cx);
        self.picker_placeholder(window, cx);
        self.picker_load(cx);
        cx.notify();
    }

    /// The picker opened (or the library changed under it): the grid of
    /// the tab on show is read from the store again.
    pub(super) fn picker_load(&mut self, cx: &mut Context<Self>) {
        let query = self.emoji_search_text(cx);
        match self.picker.tab {
            Tab::Emoji => {}
            Tab::Stickers => {
                self.engine_want_favorites();
                let cursor = self.picker.stickers.cursor;
                self.picker.stickers = self.build_stickers(&query);
                self.picker.stickers.cursor =
                    cursor.min(self.picker.stickers.tiles.len().saturating_sub(1));
            }
            Tab::Gifs => {
                let cursor = self.picker.gifs.cursor;
                self.picker.gifs = self.build_gifs(&query, cx);
                self.picker.gifs.cursor =
                    cursor.min(self.picker.gifs.tiles.len().saturating_sub(1));
                if self.picker.source == Source::Online {
                    self.online_search(&query, cx);
                }
            }
        }
        self.picker.sync_lists();
        self.picker_activate(cx);
    }

    fn engine_want_favorites(&self) {
        for account in &self.accounts {
            self.engine.want_favorite_stickers(&account.id);
        }
    }

    /// The text in the search field.
    pub(super) fn emoji_search_text(&self, cx: &gpui_kit::App) -> String {
        self.emoji.search.read(cx).value().trim().to_owned()
    }

    /// The library changed while the picker is open.
    pub(super) fn picker_library_changed(&mut self, cx: &mut Context<Self>) {
        if self.overlay == Overlay::EmojiPicker && self.picker.tab != Tab::Emoji {
            self.picker_load(cx);
            cx.notify();
        }
    }

    /// The search field changed, in a tab that is not Emoji.
    pub(super) fn picker_query_changed(&mut self, cx: &mut Context<Self>) {
        self.picker.menu = None;
        self.emoji.cursor = 0;
        self.picker.grid_mut().cursor = 0;
        self.picker_load(cx);
        self.picker.scroll_to_top();
        cx.notify();
    }

    fn build_stickers(&self, query: &str) -> Grid {
        let store = &self.store;
        let mut grid = Grid::new(STICKER_COLUMNS);
        let items = store
            .library_items(LibraryKind::Sticker)
            .unwrap_or_default();
        let packs = store.library_packs().unwrap_or_default();
        let tile = |item: LibraryItem| Tile {
            kind: TileKind::Sticker(item),
        };
        let query = query.to_lowercase();
        if !query.is_empty() {
            let pack_name = |id: &Option<String>| {
                packs
                    .iter()
                    .find(|pack| Some(&pack.id) == id.as_ref())
                    .map(|pack| pack.name.to_lowercase())
            };
            let found: Vec<Tile> = items
                .into_iter()
                .filter(|item| {
                    item.name
                        .as_deref()
                        .is_some_and(|name| name.to_lowercase().contains(&query))
                        || pack_name(&item.pack).is_some_and(|name| name.contains(&query))
                })
                .map(tile)
                .collect();
            grid.section("Found", IconName::Search);
            if found.is_empty() {
                grid.note(texts::NO_STICKER_FOUND);
            }
            grid.fill(found);
            return grid;
        }

        // What the sections hold, and what an empty one says. Sections with
        // something in them come first, in this order; an empty one is its
        // heading and one line, after them.
        let recent = store
            .library_recent(LibraryKind::Sticker, client_core::RECENT_LIMIT)
            .unwrap_or_default();
        let favorites = store
            .library_favorites(LibraryKind::Sticker)
            .unwrap_or_default();
        let shown: HashSet<&str> = recent
            .iter()
            .chain(&favorites)
            .map(|item| item.id.as_str())
            .collect();
        let heard: Vec<LibraryItem> = store
            .library_heard(client_core::HEARD_LIMIT + shown.len())
            .unwrap_or_default()
            .into_iter()
            .filter(|item| !shown.contains(item.id.as_str()))
            .take(client_core::HEARD_LIMIT)
            .collect();
        let mut sections: Vec<(String, IconName, Vec<LibraryItem>, Option<&str>)> = Vec::new();
        sections.push((
            "Recently used".into(),
            IconName::Clock3,
            recent.clone(),
            None,
        ));
        // Where the phone's favorites cannot be had, the first line says
        // so: a provider without them, or one that has not turned them
        // on for the number on screen yet. The ones starred here work
        // either way.
        let phone_missing = self.account.as_ref().is_some_and(|account| {
            self.engine
                .feature_unavailable(account, client_provider::Feature::StickerFavorites)
        });
        let favorites_note = if !self.engine.capabilities().sticker_favorites {
            Some(texts::FAVORITES_LOCAL)
        } else if phone_missing {
            Some(texts::FAVORITES_NOT_YET)
        } else if favorites.is_empty() {
            Some(texts::FAVORITES_SYNCED)
        } else {
            None
        };
        sections.push((
            "Favorites".into(),
            IconName::Star,
            favorites,
            favorites_note,
        ));
        for pack in &packs {
            let inside: Vec<LibraryItem> = items
                .iter()
                .filter(|item| item.pack.as_deref() == Some(pack.id.as_str()))
                .cloned()
                .collect();
            sections.push((pack.name.clone(), IconName::Sticker, inside, None));
        }
        let heard_note = heard.is_empty().then_some(texts::FROM_CHATS_EMPTY);
        sections.push((
            "From your chats".into(),
            IconName::MessageCircle,
            heard,
            heard_note,
        ));
        let loose: Vec<LibraryItem> = items
            .into_iter()
            .filter(|item| {
                !item.indexed
                    && item
                        .pack
                        .as_ref()
                        .is_none_or(|id| !packs.iter().any(|pack| &pack.id == id))
            })
            .collect();
        sections.push(("All stickers".into(), IconName::Sticker, loose, None));

        let (full, empty): (Vec<_>, Vec<_>) = sections
            .into_iter()
            .partition(|(_, _, tiles, _)| !tiles.is_empty());
        for (title, glyph, tiles, note) in full.into_iter().chain(empty) {
            if tiles.is_empty() && note.is_none() {
                continue;
            }
            grid.section(&title, glyph);
            if let Some(note) = note {
                grid.note(note);
            }
            grid.fill(tiles.into_iter().map(tile).collect());
        }
        grid
    }

    fn build_gifs(&self, query: &str, cx: &gpui_kit::App) -> Grid {
        let mut grid = Grid::new(GIF_COLUMNS);
        match self.picker.source {
            Source::Saved => {
                let saved = SavedGifs::new(self.store.clone());
                let found = saved.find(query);
                if found.is_empty() {
                    grid.note(if query.is_empty() {
                        texts::NO_SAVED_GIFS
                    } else {
                        texts::NO_GIF_FOUND
                    });
                }
                grid.fill(
                    found
                        .into_iter()
                        .map(|item| Tile {
                            kind: TileKind::Gif(GifEntry::Saved(item)),
                        })
                        .collect(),
                );
            }
            Source::Online => match self.online_readiness(cx) {
                Readiness::Ready => {
                    grid.fill(
                        self.picker
                            .online
                            .hits
                            .iter()
                            .cloned()
                            .map(|hit| Tile {
                                kind: TileKind::Gif(GifEntry::Online(hit)),
                            })
                            .collect(),
                    );
                    match &self.picker.online.status {
                        Status::Loading if self.picker.online.hits.is_empty() => {
                            grid.note("Searching…")
                        }
                        Status::Failed(why) => grid.note(why.clone()),
                        Status::Idle if self.picker.online.hits.is_empty() => {
                            grid.note("Nothing found.")
                        }
                        _ => {}
                    }
                }
                Readiness::Off => grid.note(texts::ONLINE_OFF),
                Readiness::NoKey => grid.note(texts::ONLINE_NO_KEY),
            },
        }
        grid
    }

    /// Whether the online search may run: on in the settings, with a key.
    pub(super) fn online_readiness(&self, cx: &gpui_kit::App) -> Readiness {
        if !settings::get(cx).gif_online {
            return Readiness::Off;
        }
        match self.picker.services.key() {
            Some(_) => Readiness::Ready,
            None => Readiness::NoKey,
        }
    }

    /// Looks for GIFs online, a moment after the typing stops. Only when
    /// the user turned it on and gave a key: otherwise this does nothing
    /// and nothing is sent anywhere.
    pub(super) fn online_search(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.online_readiness(cx) != Readiness::Ready {
            self.picker._online = None;
            return;
        }
        if self.picker.online.answers.as_deref() == Some(query)
            && self.picker.online.status != Status::Loading
        {
            return;
        }
        let Some(key) = self.picker.services.key() else {
            return;
        };
        let giphy = Giphy::new(&self.picker.services.base, &key);
        let runtime = self.engine.runtime().clone();
        let query = query.to_owned();
        self.picker.online.serial += 1;
        let serial = self.picker.online.serial;
        self.picker.online.status = Status::Loading;
        let wait = if query.is_empty() {
            Duration::ZERO
        } else {
            ONLINE_DEBOUNCE
        };
        self.picker._online = Some(cx.spawn(async move |this, cx| {
            if !wait.is_zero() {
                cx.background_executor().timer(wait).await;
            }
            let asked = query.clone();
            let found = runtime
                .spawn(async move { giphy.search(&asked).await })
                .await;
            this.update(cx, |this, cx| {
                if this.picker.online.serial != serial || this.picker.source != Source::Online {
                    return;
                }
                match found {
                    Ok(Ok(entries)) => {
                        this.picker.online.hits = entries
                            .into_iter()
                            .filter_map(|entry| match entry {
                                GifEntry::Online(hit) => Some(hit),
                                GifEntry::Saved(_) => None,
                            })
                            .collect();
                        this.picker.online.status = Status::Idle;
                        this.picker.online.answers = Some(query);
                    }
                    Ok(Err(error)) => {
                        this.picker.online.status = Status::Failed(error.to_string().into());
                    }
                    Err(_) => {
                        this.picker.online.status =
                            Status::Failed(crate::gifs::GifError::Offline.to_string().into());
                    }
                }
                let cursor = this.picker.gifs.cursor;
                let text = this.emoji_search_text(cx);
                this.picker.gifs = this.build_gifs(&text, cx);
                this.picker.gifs.cursor =
                    cursor.min(this.picker.gifs.tiles.len().saturating_sub(1));
                this.picker.sync_lists();
                cx.notify();
            })
            .ok();
        }));
    }

    /// Switches the GIF tab between the saved ones and the online search.
    pub(super) fn picker_set_source(
        &mut self,
        source: Source,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.picker.source == source {
            return;
        }
        // The field keeps the keyboard: what is typed next is the search.
        self.emoji
            .search
            .update(cx, |field, cx| field.focus(window, cx));
        self.picker.source = source;
        self.picker.gifs.cursor = 0;
        self.picker.menu = None;
        self.picker.stop_moving();
        self.picker.decoding = None;
        self.picker.services.forget_key();
        self.picker_placeholder(window, cx);
        self.picker_load(cx);
        self.picker.list_gifs.scroll_to(ListOffset::default());
        cx.notify();
    }

    /// Says something in the strip for a few seconds.
    pub(super) fn picker_say(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.picker.notice = Some(text.into());
        self.picker._notice = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(6)).await;
            this.update(cx, |this, cx| {
                this.picker.notice = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    // ----- sending ------------------------------------------------------------------------

    /// Sends the tile at `index`: a sticker or a saved GIF from the
    /// library, an online GIF after it is downloaded and kept. With
    /// `keep` the picker stays open.
    pub(super) fn picker_send(
        &mut self,
        index: usize,
        keep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.picker.grid().tiles.get(index).cloned() else {
            return;
        };
        // Never from under the attach sheet: the files there are sent
        // with the sheet, and nothing else goes out meanwhile.
        if self.attach.is_some() || self.emoji.target != Target::Composer {
            return;
        }
        self.picker.grid_mut().cursor = index;
        // A sticker of a chat that is not downloaded yet: the click asks
        // for its file (as a click on a picture that waits for one does);
        // it can be sent once it is here.
        if let Some(item) = tile.item().filter(|item| item.pending()) {
            self.picker_fetch(item, true, cx);
            return self.picker_say("Downloading this sticker…", cx);
        }
        if let Some(why) = self.picker_cannot_send() {
            return self.picker_say(why, cx);
        }
        let Some(open) = &self.open else { return };
        let (account, chat) = (open.chat.account_id.clone(), open.chat.id.clone());
        let reply_to = match &self.acting.compose {
            super::message_actions::Compose::Reply(message) => Some(message.id.clone()),
            _ => None,
        };
        let online = match &tile.kind {
            TileKind::Gif(GifEntry::Online(_)) => {
                let Some(key) = self.picker.services.key() else {
                    return;
                };
                if self.online_readiness(cx) != Readiness::Ready {
                    return;
                }
                Some(Giphy::new(&self.picker.services.base, &key))
            }
            _ => None,
        };
        // One request, one message: while this tile is on its way to the
        // outbox, asking again (a double click, Enter held down) is the
        // same request.
        let key = tile.key();
        if !self.picker.sending.insert(key.clone()) {
            return;
        }
        let engine = self.engine.clone();
        let request = Request {
            key,
            account,
            chat,
            reply_to: reply_to.clone(),
            keep,
        };
        match tile.kind {
            TileKind::Sticker(item) | TileKind::Gif(GifEntry::Saved(item)) => {
                cx.spawn_in(window, async move |this, cx| {
                    let prepared = cx
                        .background_spawn(
                            async move { engine.prepare_library_item(&item.id, reply_to) },
                        )
                        .await;
                    this.update_in(cx, |this, window, cx| {
                        this.picker_queue(request, prepared, window, cx)
                    })
                    .ok();
                })
                .detach();
            }
            TileKind::Gif(GifEntry::Online(hit)) => {
                let Some(giphy) = online else {
                    self.picker.sending.remove(&request.key);
                    return;
                };
                self.picker_say("Downloading the GIF…", cx);
                let runtime = engine.runtime().clone();
                cx.spawn_in(window, async move |this, cx| {
                    // Whatever stops it short of the outbox is said, and
                    // lets the tile be asked for again.
                    let failed = |this: &gpui_kit::WeakEntity<Self>,
                                  cx: &mut gpui_kit::AsyncWindowContext,
                                  key: String,
                                  why: String| {
                        this.update(cx, |this, cx| {
                            this.picker.sending.remove(&key);
                            this.picker_say(why, cx);
                        })
                        .ok();
                    };
                    let got = runtime
                        .spawn(async move { giphy.download(&hit).await.map(|bytes| (bytes, hit)) })
                        .await;
                    let (bytes, hit) = match got {
                        Ok(Ok(got)) => got,
                        Ok(Err(error)) => {
                            return failed(&this, cx, request.key, error.to_string());
                        }
                        Err(_) => {
                            let why = crate::gifs::GifError::Offline.to_string();
                            return failed(&this, cx, request.key, why);
                        }
                    };
                    let kept = {
                        let engine = engine.clone();
                        cx.background_spawn(async move {
                            engine.library_import_gif(
                                bytes,
                                Some(hit.title),
                                client_core::LibrarySource::Online,
                            )
                        })
                        .await
                    };
                    let item = match kept {
                        Ok(item) => item,
                        Err(error) => {
                            return failed(&this, cx, request.key, error.to_string());
                        }
                    };
                    let prepared = cx
                        .background_spawn(
                            async move { engine.prepare_library_item(&item.id, reply_to) },
                        )
                        .await;
                    this.update_in(cx, |this, window, cx| {
                        this.picker_queue(request, prepared, window, cx)
                    })
                    .ok();
                })
                .detach();
            }
        }
    }

    /// Asks for the file of a sticker listed from a chat and not
    /// downloaded yet, in the media queue the conversation uses and under
    /// the same policy: unasked only when pictures are fetched by
    /// themselves, always when `asked` (the user clicked it).
    pub(super) fn picker_fetch(&self, item: &LibraryItem, asked: bool, cx: &gpui_kit::App) {
        let Some((account, url)) = &item.origin else {
            return;
        };
        if asked || settings::get(cx).media != settings::MediaChoice::Never {
            self.engine.want_sticker_file(account, url, asked);
        }
    }

    /// A tile is ready for the outbox: it is queued for the chat it was
    /// asked for, unless files are being attached by now (the sheet sends
    /// first, and the reply under way is its to send). Only the chat it was
    /// sent in is touched: its reply is done with, and its picker closes.
    fn picker_queue(
        &mut self,
        request: Request,
        prepared: Result<client_core::LibrarySend, client_core::SendMediaError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.picker.sending.remove(&request.key);
        if self.attach.is_some() {
            return self.show_problem(
                "Not sent: files are being attached. Send it again after them.".to_owned(),
                cx,
            );
        }
        let queued = prepared.and_then(|prepared| {
            self.engine
                .send_prepared_library(&request.account, &request.chat, prepared)
        });
        if let Err(error) = queued {
            self.picker_say(error.to_string(), cx);
            return cx.notify();
        }
        let Some(open) = self
            .open
            .as_ref()
            .filter(|open| open.chat.account_id == request.account && open.chat.id == request.chat)
        else {
            // The user is somewhere else by now: what is being written
            // there, and the picker there, are not this message's.
            return cx.notify();
        };
        open.list.scroll_to_end();
        // The reply it answered is over, if it is still the one under way.
        let answered = match &self.acting.compose {
            super::message_actions::Compose::Reply(message) => {
                request.reply_to.as_ref() == Some(&message.id)
            }
            _ => false,
        };
        if answered {
            self.acting.compose = super::message_actions::Compose::New;
        }
        if !request.keep && self.overlay == Overlay::EmojiPicker {
            self.close_overlay(window, cx);
        }
        cx.notify();
    }

    // ----- what can be done to a tile --------------------------------------------------------

    /// The actions on offer for a tile.
    pub(super) fn picker_actions(&self, tile: &Tile) -> Vec<Action> {
        let mut actions = vec![Action::Send];
        match &tile.kind {
            TileKind::Sticker(item) if item.pending() => actions.push(Action::Remove),
            TileKind::Sticker(item) => {
                actions.push(Action::Favorite);
                if item.favorite {
                    actions.extend([Action::Earlier, Action::Later]);
                }
                actions.push(Action::Remove);
            }
            TileKind::Gif(GifEntry::Saved(_)) => actions.push(Action::Remove),
            TileKind::Gif(GifEntry::Online(_)) => actions.push(Action::Keep),
        }
        actions
    }

    /// Opens the menu of a tile in the strip.
    pub(super) fn picker_open_menu(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(tile) = self.picker.grid().tiles.get(index).cloned() else {
            return;
        };
        self.picker.grid_mut().cursor = index;
        let actions = self.picker_actions(&tile);
        self.picker.menu = Some(Menu {
            tile: index,
            actions,
            cursor: 0,
        });
        cx.notify();
    }

    /// Does an action to a tile.
    pub(super) fn picker_do(
        &mut self,
        action: Action,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tile) = self.picker.grid().tiles.get(index).cloned() else {
            return;
        };
        self.picker.menu = None;
        let engine = self.engine.clone();
        match (action, &tile.kind) {
            (Action::Send, _) => return self.picker_send(index, false, window, cx),
            (Action::Favorite, TileKind::Sticker(item)) => {
                let now = !item.favorite;
                if let Err(error) = engine.set_library_favorite(&item.id, now) {
                    tracing::warn!(%error, "could not star a sticker");
                }
                let text = if now {
                    "Added to favorites"
                } else {
                    "Taken off favorites"
                };
                self.picker_say(text, cx);
            }
            (Action::Earlier | Action::Later, TileKind::Sticker(item)) => {
                let favorites = self
                    .store
                    .library_favorites(LibraryKind::Sticker)
                    .unwrap_or_default();
                if let Some(at) = favorites.iter().position(|known| known.id == item.id) {
                    let to = if action == Action::Earlier {
                        at.saturating_sub(1)
                    } else {
                        at + 1
                    };
                    let _ = engine.move_library_favorite(&item.id, to);
                }
            }
            (Action::Remove, _) => {
                if let Some(item) = tile.item() {
                    // Through the engine: a favorite's star on the phone
                    // goes with it, or the next sync would bring it back.
                    match engine.remove_library_item(&item.id) {
                        Ok(_) => self.picker_say("Removed from the library", cx),
                        Err(error) => {
                            tracing::warn!(%error, "could not remove a library item");
                            self.picker_say("Could not remove it from the library", cx);
                        }
                    }
                }
            }
            (Action::Keep, TileKind::Gif(GifEntry::Online(hit))) => {
                self.keep_online_gif(hit.clone(), cx);
            }
            _ => {}
        }
        self.picker_load(cx);
        // The field keeps the keyboard.
        self.emoji
            .search
            .update(cx, |field, cx| field.focus(window, cx));
        cx.notify();
    }

    /// Stars or unstars the tile the keyboard is on (the registered
    /// command).
    pub(super) fn picker_toggle_favorite(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.picker.tab != Tab::Stickers {
            return false;
        }
        let index = self.picker.grid().cursor;
        if !matches!(
            self.picker.grid().tiles.get(index).map(|tile| &tile.kind),
            Some(TileKind::Sticker(item)) if !item.pending()
        ) {
            return false;
        }
        self.picker_do(Action::Favorite, index, window, cx);
        true
    }

    /// The pointer came onto a tile or left it: the one it is on moves.
    pub(super) fn picker_hovered(
        &mut self,
        index: usize,
        key: &str,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.picker.hover = Some(key.to_owned());
            if self.picker.menu.is_none() {
                self.picker.grid_mut().cursor = index;
            }
        } else if self.picker.hover.as_deref() == Some(key) {
            self.picker.hover = None;
        }
        self.picker_activate(cx);
        cx.notify();
    }

    // ----- moving ------------------------------------------------------------------------------

    /// The tile on show changed: it is the one that moves, and no other.
    /// Its frames are decoded off the UI thread, one tile at a time.
    pub(super) fn picker_activate(&mut self, cx: &mut Context<Self>) {
        let wanted = self.picker_active_tile();
        let key = wanted.as_ref().map(Tile::key);
        if self
            .picker
            .moving
            .borrow()
            .as_ref()
            .map(|moving| &moving.key)
            == key.as_ref()
            && key.is_some()
        {
            return;
        }
        if self
            .picker
            .moving
            .borrow()
            .as_ref()
            .map(|moving| moving.key.clone())
            != key
        {
            self.picker.stop_moving();
        }
        if self.picker.decoding == key {
            return;
        }
        self.picker.decoding = None;
        let (Some(tile), Some(key)) = (wanted, key) else {
            return;
        };
        if self.picker.tab == Tab::Emoji
            || !settings::get(cx).sticker_hover_animation
            || settings::reduce_motion(cx)
        {
            return;
        }
        let store = self.store.clone();
        let runtime = self.engine.runtime().clone();
        let decoding = key.clone();
        self.picker.decoding = Some(key.clone());
        let job: Option<tokio::task::JoinHandle<Option<Animation>>> = match &tile.kind {
            TileKind::Sticker(item) | TileKind::Gif(GifEntry::Saved(item))
                if item.animated && item.mime != "video/mp4" =>
            {
                let id = item.id.clone();
                let work = runtime.spawn_blocking(move || {
                    let (bytes, _) = store.library_file(&id).ok().flatten()?;
                    client_core::animation_frames(&bytes, &limits())
                        .ok()
                        .flatten()
                        .map(Animation::new)
                });
                // Awaited on the runtime, so the answer wakes this view
                // properly.
                Some(runtime.spawn(async move { work.await.ok().flatten() }))
            }
            TileKind::Gif(GifEntry::Online(hit)) if hit.moving_url.is_some() => {
                let Some(secret) = self.picker.services.key() else {
                    return;
                };
                let giphy = Giphy::new(&self.picker.services.base, &secret);
                let url = hit.moving_url.clone().unwrap_or_default();
                Some(runtime.clone().spawn(async move {
                    let bytes = giphy.preview(&url).await.ok()?;
                    tokio::task::spawn_blocking(move || {
                        client_core::animation_frames(&bytes, &limits())
                            .ok()
                            .flatten()
                            .map(Animation::new)
                    })
                    .await
                    .ok()
                    .flatten()
                }))
            }
            _ => None,
        };
        let Some(job) = job else { return };
        self.picker._tasks.push(cx.spawn(async move |this, cx| {
            let animation = job.await.ok().flatten();
            this.update(cx, |this, cx| {
                if this.picker.decoding.as_deref() != Some(decoding.as_str()) {
                    return;
                }
                this.picker.decoding = None;
                let Some(animation) = animation else { return };
                this.picker.stop_moving();
                *this.picker.moving.borrow_mut() = Some(Moving {
                    key: decoding,
                    animation: Rc::new(animation),
                    playback: Playback::default(),
                });
                cx.notify();
            })
            .ok();
        }));
    }

    /// The tile that moves: the one the pointer is on, else the one the
    /// keyboard is on in the grid.
    fn picker_active_tile(&self) -> Option<Tile> {
        if self.overlay != Overlay::EmojiPicker || self.picker.tab == Tab::Emoji {
            return None;
        }
        let grid = self.picker.grid();
        if let Some(hover) = &self.picker.hover {
            if let Some(tile) = grid.tiles.iter().find(|tile| &tile.key() == hover) {
                return Some(tile.clone());
            }
        }
        if self.emoji.zone == Zone::Grid {
            return grid.tiles.get(grid.cursor).cloned();
        }
        None
    }

    // ----- the keyboard ----------------------------------------------------------------------

    /// Puts the keyboard on a tile, for tests.
    #[cfg(test)]
    pub(super) fn picker_cursor_to_for_test(&mut self, tile: usize, cx: &mut Context<Self>) {
        self.picker_move_to(tile, cx);
    }

    /// After the favorites were rearranged: the keyboard goes with the
    /// sticker, to its place in the Favorites section.
    fn picker_follow(&mut self, id: &str, cx: &mut Context<Self>) {
        let grid = self.picker.grid();
        let from = grid
            .sections
            .iter()
            .find(|section| section.title == "Favorites")
            .map_or(0, |section| section.tile);
        let at = grid.tiles[from.min(grid.tiles.len())..]
            .iter()
            .position(|tile| tile.key() == id)
            .map(|at| from + at);
        if let Some(at) = at {
            self.picker_move_to(at, cx);
        }
    }

    /// Moves the keyboard to a tile of the grid and keeps it in view.
    fn picker_move_to(&mut self, tile: usize, cx: &mut Context<Self>) {
        let grid = self.picker.grid();
        let Some(row) = grid.tile_rows.get(tile).copied() else {
            return;
        };
        let came_from = grid.tile_rows.get(grid.cursor).copied();
        let under_heading = matches!(
            row.checked_sub(1).map(|above| &grid.rows[above]),
            Some(Row::Header(_) | Row::Note(_))
        );
        let shown = if under_heading && came_from.is_some_and(|from| row < from) {
            row - 1
        } else {
            row
        };
        self.picker.grid_mut().cursor = tile;
        self.emoji.zone = Zone::Grid;
        self.picker.menu = None;
        self.picker.scroll().scroll_to_reveal_item(shown);
        self.picker_activate(cx);
        cx.notify();
    }

    pub(super) fn picker_go_to_section(&mut self, section: usize, cx: &mut Context<Self>) {
        let Some(found) = self.picker.grid().sections.get(section).cloned() else {
            return;
        };
        let tile = found
            .tile
            .min(self.picker.grid().tiles.len().saturating_sub(1));
        self.picker.grid_mut().cursor = tile;
        self.picker.menu = None;
        self.picker.scroll().scroll_to(ListOffset {
            item_ix: found.row,
            offset_in_item: px(0.),
        });
        self.picker_activate(cx);
        cx.notify();
    }

    /// The keys of the Stickers and GIFs tabs, seen before the search
    /// field's own bindings. `true` when the key was the picker's.
    pub(super) fn picker_key(
        &mut self,
        stroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let held = &stroke.modifiers;
        let plain = !held.modified();
        let shift_only = held.shift && !held.control && !held.alt && !held.platform;
        let key = stroke.key.as_str();
        let searching = !self.emoji_search_text(cx).is_empty();

        // The command to star the tile in hand.
        if keys::resolve(stroke, self.key_context(window, cx)) == Some(Command::FavoriteSticker) {
            return self.picker_toggle_favorite(window, cx);
        }

        // Its menu, while it is on offer.
        if let Some(menu) = &mut self.picker.menu {
            let count = menu.actions.len();
            match key {
                "left" | "up" if plain => menu.cursor = (menu.cursor + count - 1) % count,
                "right" | "down" if plain => menu.cursor = (menu.cursor + 1) % count,
                "enter" if plain => {
                    let (action, tile) = (menu.actions[menu.cursor], menu.tile);
                    self.picker_do(action, tile, window, cx);
                }
                "escape" => self.picker.menu = None,
                _ => {
                    self.picker.menu = None;
                    cx.notify();
                    return self.picker_key(stroke, window, cx);
                }
            }
            cx.notify();
            return true;
        }

        let tiles = self.picker.grid().tiles.len();
        let cursor = self.picker.grid().cursor;
        match key {
            "escape" if plain && searching => {
                self.emoji
                    .search
                    .update(cx, |field, cx| field.set_value("", window, cx));
                self.emoji.zone = Zone::Search;
                self.picker_query_changed(cx);
            }
            "tab" if plain || shift_only => {
                let order = [Zone::Search, Zone::Tabs, Zone::Grid];
                let at = order
                    .iter()
                    .position(|zone| *zone == self.emoji.zone)
                    .unwrap_or(0);
                let next = if shift_only {
                    (at + order.len() - 1) % order.len()
                } else {
                    (at + 1) % order.len()
                };
                self.emoji.zone = order[next];
                if self.emoji.zone == Zone::Tabs {
                    self.picker.tab_cursor = tab_index(self.picker.tab);
                }
                if self.emoji.zone == Zone::Grid {
                    self.picker_move_to(cursor, cx);
                }
                self.picker_activate(cx);
            }
            "enter" if (plain || shift_only) && self.emoji.zone == Zone::Tabs => {
                let tab = TABS[self.picker.tab_cursor.min(2)];
                self.picker_set_tab(tab, window, cx);
            }
            "left" | "right" if plain && self.emoji.zone == Zone::Tabs => {
                let at = self.picker.tab_cursor;
                self.picker.tab_cursor = if key == "left" {
                    (at + 2) % 3
                } else {
                    (at + 1) % 3
                };
            }
            "enter" if plain || shift_only => {
                if tiles > 0 {
                    self.picker_send(cursor, shift_only, window, cx);
                }
            }
            "down" if held.alt && !held.control && !held.platform => {
                self.picker_open_menu(cursor, cx);
            }
            // A favorite one place earlier or later among the favorites.
            "left" | "right"
                if held.alt
                    && !held.control
                    && !held.platform
                    && self.picker.tab == Tab::Stickers =>
            {
                let on = match self.picker.grid().tiles.get(cursor).map(|tile| &tile.kind) {
                    Some(TileKind::Sticker(item)) if item.favorite => Some(item.id.clone()),
                    _ => None,
                };
                if let Some(id) = on {
                    let action = if key == "left" {
                        Action::Earlier
                    } else {
                        Action::Later
                    };
                    self.picker_do(action, cursor, window, cx);
                    self.picker_follow(&id, cx);
                }
            }
            "left" | "right" if plain && searching && self.emoji.zone == Zone::Search => {
                return false;
            }
            "left" if plain && tiles > 0 => self.picker_move_to(cursor.saturating_sub(1), cx),
            "right" if plain && tiles > 0 => self.picker_move_to((cursor + 1).min(tiles - 1), cx),
            "down" if plain && self.emoji.zone != Zone::Grid => {
                self.picker_move_to(cursor, cx);
            }
            "down" if plain => {
                if let Some(next) = self.picker.grid().step_row(cursor, true) {
                    self.picker_move_to(next, cx);
                }
            }
            "up" if plain && self.emoji.zone == Zone::Grid => {
                match self.picker.grid().step_row(cursor, false) {
                    Some(next) => self.picker_move_to(next, cx),
                    None => {
                        self.emoji.zone = Zone::Search;
                        self.picker.scroll_to_top();
                        self.picker_activate(cx);
                    }
                }
            }
            "up" if plain => self.emoji.zone = Zone::Search,
            "pagedown" | "pageup" if plain && self.picker.grid().sections.len() > 1 => {
                let grid = self.picker.grid();
                let at = grid.section_of(cursor);
                let last = grid.sections.len() - 1;
                let next = if key == "pagedown" {
                    (at + 1).min(last)
                } else if grid.sections.get(at).map(|section| section.tile) == Some(cursor) {
                    at.saturating_sub(1)
                } else {
                    at
                };
                self.picker_go_to_section(next, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    // ----- drawing ---------------------------------------------------------------------------------

    /// The bar of tabs, above the search.
    pub(super) fn render_picker_tabs(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let in_bar = self.emoji.zone == Zone::Tabs;
        div()
            .debug_selector(|| "picker-tabs".into())
            .flex_none()
            .h(BAR())
            .flex()
            .items_center()
            .gap(px(4.))
            .children(TABS.into_iter().enumerate().map(|(at, tab)| {
                let (glyph, label, hint) = tab_look(tab);
                let on = tab == self.picker.tab;
                let hover = palette.elevated_selected;
                let shortcut = keys::keys_label(hint);
                let tip: SharedString = match shortcut {
                    Some(keys) => format!("{label} ({keys})").into(),
                    None => label.into(),
                };
                div()
                    .id(("picker-tab", at))
                    .debug_selector(move || format!("picker-tab-{at}"))
                    .flex_1()
                    .min_w_0()
                    .h(px(30.))
                    .rounded(metrics::RADIUS())
                    .border_2()
                    .border_color(if in_bar && at == self.picker.tab_cursor {
                        palette.focus_ring
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .when(on, |this| this.bg(palette.elevated_selected))
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(px(6.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.picker_set_tab(tab, window, cx);
                    }))
                    .child(icon(
                        glyph,
                        px(14.),
                        if on { palette.text } else { palette.icon },
                    ))
                    .child(
                        div()
                            .text_size(metrics::TEXT_SMALL())
                            .font_weight(if on {
                                FontWeight::MEDIUM
                            } else {
                                FontWeight::NORMAL
                            })
                            .text_color(if on { palette.text } else { palette.text_muted })
                            .child(label),
                    )
            }))
    }

    /// The still, or the moving picture, of a tile.
    pub(super) fn render_tile_picture(
        &self,
        tile: &Tile,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let key = tile.key();
        // The one that moves is painted frame by frame.
        if self
            .picker
            .moving
            .borrow()
            .as_ref()
            .is_some_and(|moving| moving.key == key)
        {
            return self.moving_tile(cx.weak_entity()).into_any_element();
        }
        let still: Option<Arc<Image>> = match &tile.kind {
            TileKind::Sticker(item) | TileKind::Gif(GifEntry::Saved(item)) => {
                self.picker.shelf.thumb(item)
            }
            TileKind::Gif(GifEntry::Online(hit)) => hit
                .still_url
                .as_ref()
                .and_then(|url| self.online_still(url, cx)),
        };
        let fit = if matches!(tile.kind, TileKind::Sticker(_)) {
            ObjectFit::Contain
        } else {
            ObjectFit::Cover
        };
        match still {
            Some(image) => img(image)
                .size_full()
                .object_fit(fit)
                .rounded(px(4.))
                .into_any_element(),
            None => {
                // Nothing to draw: a labelled tile. There is no video
                // decoder here, so an MP4 has no first frame to show.
                let label = match &tile.kind {
                    TileKind::Gif(GifEntry::Saved(item)) if item.mime == "video/mp4" => "MP4",
                    TileKind::Gif(_) => "GIF",
                    TileKind::Sticker(_) => "…",
                };
                // A sticker of a chat that is not downloaded yet: a place
                // for it, until its file arrives.
                if let TileKind::Sticker(item) = &tile.kind {
                    let failed = item.origin.as_ref().is_some_and(|(_, url)| {
                        matches!(
                            self.engine.media_state(&client_core::thumbnail_key(url)),
                            client_core::MediaState::Unavailable(_)
                        )
                    });
                    return div()
                        .debug_selector(|| "picker-place".into())
                        .size_full()
                        .rounded(px(4.))
                        .bg(palette.muted)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(icon(
                            if failed {
                                IconName::CircleAlert
                            } else {
                                IconName::Sticker
                            },
                            px(18.),
                            palette.text_faint,
                        ))
                        .into_any_element();
                }
                let name = tile.name();
                div()
                    .size_full()
                    .rounded(px(4.))
                    .bg(palette.muted)
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(2.))
                    .child(icon(IconName::Film, px(16.), palette.text_faint))
                    .child(mono(label).text_color(palette.text_muted))
                    .when(!name.is_empty(), |this| {
                        this.child(
                            div()
                                .max_w_full()
                                .px(px(4.))
                                .truncate()
                                .text_size(px(10.))
                                .text_color(palette.text_faint)
                                .child(SharedString::from(name)),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    /// An online preview, fetched once, behind the user's key's service
    /// only.
    fn online_still(&self, url: &str, cx: &mut Context<Self>) -> Option<Arc<Image>> {
        if let Some(known) = self.picker.shelf.online.borrow().get(url) {
            return known.clone();
        }
        if !self.picker.shelf.asked.borrow_mut().insert(url.to_owned()) {
            return None;
        }
        let key = self.picker.services.key()?;
        if settings::get(cx).gif_online {
            let giphy = Giphy::new(&self.picker.services.base, &key);
            let runtime = self.engine.runtime().clone();
            let url = url.to_owned();
            cx.spawn(async move |this, cx| {
                let asked = url.clone();
                let bytes = runtime
                    .spawn(async move { giphy.preview(&asked).await })
                    .await;
                this.update(cx, |this, cx| {
                    let image = bytes.ok().and_then(Result::ok).and_then(image_from);
                    this.picker.shelf.online.borrow_mut().insert(url, image);
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        None
    }

    /// A moving tile: its frame, fitted inside its box, repainted when the
    /// next is due and not before.
    fn moving_tile(&self, view: gpui_kit::WeakEntity<Shell>) -> Div {
        let moving = self.picker.moving.clone();
        let wake = self.picker.wake.clone();
        div().size_full().child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, cx| {
                    let now = cx.background_executor().now();
                    let mut held = moving.borrow_mut();
                    let Some(held) = held.as_mut() else { return };
                    let motion = match crate::motion::frozen_at(cx) {
                        Some(at) => Motion::Frozen(at),
                        None => Motion::Running,
                    };
                    let (frame, next) = held.animation.frame_at(held.playback.elapsed(now, motion));
                    let (width, height) =
                        (held.animation.size.0 as f32, held.animation.size.1 as f32);
                    let scale = (bounds.size.width.as_f32() / width)
                        .min(bounds.size.height.as_f32() / height);
                    let size =
                        gpui_kit::size(gpui_kit::px(width * scale), gpui_kit::px(height * scale));
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
                        held.animation.image.clone(),
                        frame,
                        false,
                    );
                    if motion == Motion::Running && next != Duration::MAX {
                        let due = now + next.max(MIN_TICK);
                        if wake.due.get().is_some_and(|pending| pending <= due) {
                            return;
                        }
                        wake.due.set(Some(due));
                        let serial = wake.serial.get() + 1;
                        wake.serial.set(serial);
                        let (wake, view) = (wake.clone(), view.clone());
                        let clock = cx.background_executor().clone();
                        cx.spawn(async move |cx| {
                            clock
                                .timer(due.saturating_duration_since(clock.now()))
                                .await;
                            if wake.serial.get() != serial {
                                return;
                            }
                            wake.due.set(None);
                            view.update(cx, |_, cx| cx.notify()).ok();
                        })
                        .detach();
                    }
                },
            )
            .size_full(),
        )
    }
}

/// A tile on its way to the outbox: what it is, where it goes, what it
/// answers, and whether the picker stays open after it.
struct Request {
    key: String,
    account: client_provider::AccountId,
    chat: client_provider::ChatId,
    reply_to: Option<client_provider::MessageId>,
    keep: bool,
}

/// A picture of whatever the service sent a preview as, told by its
/// content.
fn image_from(bytes: Vec<u8>) -> Option<Arc<Image>> {
    let mime = match client_core::sniff(&bytes) {
        client_core::FileKind::Gif { .. } => "image/gif",
        client_core::FileKind::Png => "image/png",
        client_core::FileKind::Jpeg => "image/jpeg",
        client_core::FileKind::Webp { .. } => "image/webp",
        _ => return None,
    };
    super::media::image_of(bytes, Some(mime))
}

/// Whether the online search may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    Off,
    NoKey,
    Ready,
}

/// The tabs, in the order of the bar.
pub(super) const TABS: [Tab; 3] = [Tab::Emoji, Tab::Gifs, Tab::Stickers];

pub(super) fn tab_index(tab: Tab) -> usize {
    TABS.iter().position(|known| *known == tab).unwrap_or(0)
}

/// What a tab is called, drawn as, and opened by.
fn tab_look(tab: Tab) -> (IconName, &'static str, Command) {
    match tab {
        Tab::Emoji => (IconName::FaceSlightlySmiling, "Emoji", Command::EmojiPicker),
        Tab::Gifs => (IconName::Film, "GIF", Command::GifPicker),
        Tab::Stickers => (IconName::Sticker, "Stickers", Command::StickerPicker),
    }
}
