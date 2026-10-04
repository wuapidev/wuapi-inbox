//! The emoji picker: every emoji, by category, with a search in the
//! user's language.
//!
//! One picker, in two places. Over the composer (its emoji button, or
//! Cmd or Ctrl with E) a pick goes into the text at the caret and the
//! picker stays open for more. In the reaction sheet the six quick
//! reactions stay; "+" or anything typed opens the rest, and a pick is
//! the reaction.
//!
//! What it shows comes from `crate::emoji`: the tables are unpacked the
//! first time a picker opens, off the UI thread, and kept; the search runs
//! off the UI thread too, a moment after the typing stops, and a newer
//! keystroke drops the one under way. The grid is a list of rows of which
//! only those in view are built.
//!
//! The keyboard never leaves the search field, so typing always searches.
//! The keys that move are taken before the field sees them
//! (`Shell::emoji_key`, a keystroke interceptor like the composer's): the
//! arrows walk the grid across rows and categories, Tab goes from the
//! search to the categories to the grid, Page Up and Page Down jump a
//! category, Enter picks (with Shift the picker stays open), Alt with Down
//! offers the skin tones of the emoji the keyboard is on, and Escape
//! empties the search before it closes.

use super::message_actions::QUICK_REACTIONS;
use super::shell::{Overlay, Shell};
use super::widgets::{icon_button, mono};
use crate::emoji::data::{EmojiSet, Lang, LangPack, Tone};
use crate::emoji::font::{self, Support};
use crate::emoji::locale::{self, LanguageChoice, Locale, SearchLanguage};
use crate::emoji::prefs;
use crate::emoji::search::Index;
use crate::icons::{icon, IconName};
use crate::keys::{self, Command};
use crate::theme::{metrics, px, Palette};
use gpui_kit::component::input::{Input, InputEvent, InputState, TextareaState};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, div, uniform_list, AnyElement, Bounds, Context, Div, Entity, Focusable, FontWeight,
    Keystroke, MouseButton, MouseDownEvent, Pixels, ScrollStrategy, SharedString, Stateful,
    Subscription, Task, UniformListScrollHandle, Window,
};
use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Emoji per row of the grid.
pub(super) const COLUMNS: usize = 9;
/// Rows of the grid in view, when the window leaves room for them.
const ROWS: usize = 8;
/// How many used emoji are offered first: two rows.
const FREQUENT: usize = COLUMNS * 2;
/// The most emoji a search lists.
const RESULTS: usize = COLUMNS * 20;
/// How many emoji the composer offers for a shortcode being typed.
const COMPLETIONS: usize = 6;
/// How long the search waits after a keystroke: a word is one search.
pub(super) const SEARCH_DEBOUNCE: Duration = Duration::from_millis(40);
/// How long a press is held before it asks for the skin tones.
pub(super) const LONG_PRESS: Duration = Duration::from_millis(450);

/// The side of a cell of the grid.
#[allow(non_snake_case)]
pub(super) fn CELL() -> Pixels {
    px(36.)
}

/// The room around the grid inside the card.
#[allow(non_snake_case)]
pub(super) fn PAD() -> Pixels {
    px(8.)
}

/// The width of the grid.
#[allow(non_snake_case)]
pub(super) fn GRID_WIDTH() -> Pixels {
    CELL() * COLUMNS as f32
}

/// The height of the search field, of the category bar and of the strip
/// under the grid.
#[allow(non_snake_case)]
pub(super) fn BAR() -> Pixels {
    px(34.)
}

/// The glyph a category is known by in the bar: the used ones, then the
/// nine groups of the keyboard in their order.
const FREQUENT_GLYPH: &str = "🕘";
const GROUP_GLYPHS: [&str; 9] = ["😀", "👋", "🐻", "🍔", "🚗", "⚽", "💡", "🔣", "🏁"];

/// Where a pick goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Target {
    /// Into the composer's text, at the caret.
    Composer,
    /// Into the caption of what is being attached. The sheet comes back
    /// when the picker closes.
    Caption,
    /// Into a field of Status: the reply under a story, or the words or
    /// the caption of a post. What was under the picker comes back when
    /// it closes.
    Status,
    /// Onto the message the reaction sheet is about.
    Reaction,
}

/// The part of the picker the moving keys act on. The keyboard itself
/// stays in the search field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Zone {
    Search,
    /// The bar of tabs: Emoji, GIF, Stickers (see `picker.rs`).
    Tabs,
    Categories,
    Grid,
}

/// The tables, once they are unpacked.
struct Loaded {
    set: Arc<EmojiSet>,
    pack: Option<Arc<LangPack>>,
    language: SearchLanguage,
    index: Arc<Index>,
    support: Arc<Support>,
}

/// One emoji of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GridCell {
    /// As it is shown and picked: in the preferred skin tone.
    pub(super) text: SharedString,
    /// Its place in the table.
    pub(super) base: usize,
}

/// One row of the grid.
#[derive(Clone, Debug, PartialEq, Eq)]
enum GridRow {
    /// A category's heading: the section at this place.
    Header(usize),
    /// Up to [`COLUMNS`] cells.
    Cells(Range<usize>),
}

/// A category, as the grid lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Section {
    title: SharedString,
    glyph: &'static str,
    /// The row of its heading.
    row: usize,
    /// Its first cell.
    cell: usize,
}

/// What the grid shows: built when the tables arrive, the search answers
/// or a preference changes, never while drawing.
#[derive(Debug, Default, PartialEq, Eq)]
struct Layout {
    rows: Vec<GridRow>,
    cells: Vec<GridCell>,
    /// The row each cell is in.
    cell_rows: Vec<usize>,
    sections: Vec<Section>,
}

impl Layout {
    /// Starts a category.
    fn section(&mut self, title: &str, glyph: &'static str) {
        self.sections.push(Section {
            title: title.to_owned().into(),
            glyph,
            row: self.rows.len(),
            cell: self.cells.len(),
        });
        self.rows.push(GridRow::Header(self.sections.len() - 1));
    }

    /// Lays cells out in rows.
    fn fill(&mut self, cells: Vec<GridCell>) {
        for chunk in cells.chunks(COLUMNS) {
            let start = self.cells.len();
            self.cell_rows
                .extend(std::iter::repeat_n(self.rows.len(), chunk.len()));
            self.cells.extend_from_slice(chunk);
            self.rows.push(GridRow::Cells(start..self.cells.len()));
        }
    }

    /// Takes back a category that turned out empty.
    fn drop_empty_section(&mut self) {
        if self
            .sections
            .last()
            .is_some_and(|section| section.cell == self.cells.len())
        {
            self.sections.pop();
            self.rows.pop();
        }
    }

    /// The cell a row up or down from `cell`, in the same column where the
    /// row has one, across headings and categories.
    fn step_row(&self, cell: usize, down: bool) -> Option<usize> {
        let row = *self.cell_rows.get(cell)?;
        let GridRow::Cells(range) = &self.rows[row] else {
            return None;
        };
        let column = cell - range.start;
        let mut at = row;
        loop {
            at = if down { at + 1 } else { at.checked_sub(1)? };
            match self.rows.get(at)? {
                GridRow::Cells(range) => {
                    return Some(range.start + column.min(range.len() - 1));
                }
                GridRow::Header(_) => {}
            }
        }
    }

    /// The category a cell is in.
    fn section_of(&self, cell: usize) -> usize {
        self.sections
            .iter()
            .rposition(|section| section.cell <= cell)
            .unwrap_or(0)
    }
}

/// The skin tones of one emoji, offered under the grid.
struct ToneRow {
    /// The emoji in each tone; the first is the emoji without one.
    options: Vec<(SharedString, Option<Tone>)>,
    /// The option the keyboard is on.
    cursor: usize,
}

/// What the composer offers for a shortcode being typed (`:fir`).
#[derive(Default)]
struct Completion {
    /// The emoji and the shortcode each is offered under.
    items: Vec<(SharedString, SharedString)>,
    cursor: usize,
    /// The text the list was closed at with Escape.
    dismissed: Option<String>,
    _search: Option<Task<()>>,
}

/// The picker's state.
pub(super) struct EmojiUi {
    /// The search field: it has the keyboard while a picker is open.
    pub(super) search: Entity<InputState>,
    pub(super) target: Target,
    /// The whole picker is showing. Always for the composer's; in the
    /// reaction sheet after "+" or a keystroke.
    pub(super) expanded: bool,
    /// The operating system's language, read once.
    system: Option<Locale>,
    loaded: Option<Loaded>,
    /// The language being unpacked.
    loading: Option<SearchLanguage>,
    _loading: Option<Task<()>>,
    /// What the search found for the text in the field; `None` while the
    /// field is empty.
    results: Option<Vec<usize>>,
    _searching: Option<Task<()>>,
    layout: Layout,
    /// The emoji used most, as of when the picker opened: the grid does
    /// not move under the pointer while picks are made.
    used: Vec<String>,
    /// The cell the keyboard, or the pointer, is on.
    pub(super) cursor: usize,
    pub(super) zone: Zone,
    /// The category the keyboard is on, while it is in the bar.
    category: usize,
    tones: Option<ToneRow>,
    scroll: UniformListScrollHandle,
    /// Where the composer's emoji button was drawn: the picker opens
    /// above it.
    pub(super) anchor: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Where the picker stays while it is the caption's: the sheet, and
    /// its button, are not drawn under it.
    pub(super) pinned: Option<Bounds<Pixels>>,
    /// A press on a cell on its way to being a long one.
    _press: Option<Task<()>>,
    /// The press under way opened the skin tones: its release is not a
    /// pick.
    held: bool,
    /// The rows of the grid built for the last frame.
    pub(super) built: Rc<Cell<Range<usize>>>,
    completion: Completion,
    _subscriptions: Vec<Subscription>,
}

/// Which emoji the fonts draw: asked once per process.
static SUPPORT: OnceLock<Arc<Support>> = OnceLock::new();

impl EmojiUi {
    pub(super) fn new(
        composer: &Entity<TextareaState>,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) -> Self {
        prefs::init(cx);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search emoji"));
        let own_window = window.window_handle();
        let view = cx.weak_entity();
        let subscriptions = vec![
            // The keys that move in the picker, before the search field's
            // own bindings see them.
            cx.intercept_keystrokes(move |event, window, cx| {
                if window.window_handle() != own_window {
                    return;
                }
                let taken = view
                    .update(cx, |this, cx| this.emoji_key(&event.keystroke, window, cx))
                    .unwrap_or(false);
                if taken {
                    cx.stop_propagation();
                }
            }),
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.emoji_query_changed(cx);
                }
            }),
            cx.subscribe_in(composer, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.emoji_completion_changed(cx);
                }
            }),
        ];
        Self {
            search,
            target: Target::Composer,
            expanded: false,
            // Tests run in English whatever the machine they run on speaks.
            system: if cfg!(test) { None } else { locale::system() },
            loaded: None,
            loading: None,
            _loading: None,
            results: None,
            _searching: None,
            layout: Layout::default(),
            used: Vec::new(),
            cursor: 0,
            zone: Zone::Search,
            category: 0,
            tones: None,
            scroll: UniformListScrollHandle::new(),
            anchor: Default::default(),
            pinned: None,
            _press: None,
            held: false,
            built: Rc::new(Cell::new(0..0)),
            completion: Completion::default(),
            _subscriptions: subscriptions,
        }
    }

    /// The emoji of the grid, in its order.
    #[cfg(test)]
    pub(super) fn cells(&self) -> &[GridCell] {
        &self.layout.cells
    }

    /// How many rows the grid has, headings included.
    #[cfg(test)]
    pub(super) fn row_count(&self) -> usize {
        self.layout.rows.len()
    }

    /// The titles of the categories listed.
    #[cfg(test)]
    pub(super) fn sections(&self) -> Vec<String> {
        self.layout
            .sections
            .iter()
            .map(|section| section.title.to_string())
            .collect()
    }

    /// The first cell of the category at `section`.
    #[cfg(test)]
    pub(super) fn section_start(&self, section: usize) -> Option<usize> {
        self.layout
            .sections
            .get(section)
            .map(|section| section.cell)
    }

    /// The skin tones on offer, when their row is open.
    #[cfg(test)]
    pub(super) fn tone_options(&self) -> Vec<String> {
        self.tones
            .iter()
            .flat_map(|tones| tones.options.iter().map(|(text, _)| text.to_string()))
            .collect()
    }

    /// The language the names are searched and shown in.
    #[cfg(test)]
    pub(super) fn language(&self) -> Option<SearchLanguage> {
        self.loaded.as_ref().map(|loaded| loaded.language)
    }

    /// What the composer offers for the shortcode being typed.
    #[cfg(test)]
    pub(super) fn completions(&self) -> Vec<(String, String)> {
        self.completion
            .items
            .iter()
            .map(|(emoji, code)| (emoji.to_string(), code.to_string()))
            .collect()
    }

    /// The name under the grid: of the emoji the keyboard is on, in the
    /// search language, and in English where that has none.
    #[cfg(test)]
    pub(super) fn preview_name(&self) -> Option<String> {
        let loaded = self.loaded.as_ref()?;
        let cell = self.layout.cells.get(self.cursor)?;
        Some(local_name(loaded, cell.base))
    }
}

/// An emoji's name in the search language, else in English.
fn local_name(loaded: &Loaded, base: usize) -> String {
    loaded
        .pack
        .as_ref()
        .and_then(|pack| pack.name(base, loaded.language.regional))
        .unwrap_or(&loaded.set.emojis[base].name)
        .to_owned()
}

/// The search language for the settings as they are, on this system.
fn search_language(system: Option<&Locale>, cx: &gpui_kit::App) -> SearchLanguage {
    locale::resolve(prefs::language(cx), system)
}

impl Shell {
    // ----- the tables ----------------------------------------------------------------

    /// Unpacks the tables and the names of the search language, off the UI
    /// thread, unless they are at hand. The first time, the fonts are
    /// asked which emoji they draw.
    fn emoji_load(&mut self, cx: &mut Context<Self>) {
        let language = search_language(self.emoji.system.as_ref(), cx);
        let at_hand = self
            .emoji
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.language == language);
        if at_hand || self.emoji.loading == Some(language) {
            return;
        }
        self.emoji.loading = Some(language);
        // The test platform shapes one glyph per character, which says
        // nothing about fonts: there everything is taken to draw.
        let shaper = (!cfg!(test)).then(|| font::system_shaper(cx.text_system().clone()));
        self.emoji._loading = Some(cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    let set = EmojiSet::load();
                    let pack = LangPack::load(language.lang);
                    let index = Arc::new(Index::new(set.clone(), pack.as_deref()));
                    let support = SUPPORT
                        .get_or_init(|| {
                            Arc::new(match shaper {
                                Some(shape) => {
                                    let support = Support::probe(&set, shape);
                                    let (emoji, variants) = support.hidden();
                                    tracing::debug!(
                                        emoji,
                                        variants,
                                        "emoji the fonts cannot draw are left out"
                                    );
                                    support
                                }
                                None => Support::all(&set),
                            })
                        })
                        .clone();
                    Loaded {
                        set,
                        pack,
                        language,
                        index,
                        support,
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                this.emoji.loading = None;
                this.emoji.loaded = Some(loaded);
                this.emoji_layout(cx);
                // What was typed while they were on their way.
                this.emoji_query_changed(cx);
                this.emoji_completion_changed(cx);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Builds what the grid shows: the search's answer, or the used emoji
    /// and the nine groups. Only what the fonts draw, each in the
    /// preferred skin tone.
    fn emoji_layout(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.emoji.loaded else {
            self.emoji.layout = Layout::default();
            return;
        };
        let chosen = prefs::get(cx);
        let cell = |base: usize| -> Option<GridCell> {
            if !loaded.support.base(base) {
                return None;
            }
            let (text, variant) = loaded.set.in_tone(base, chosen.tone);
            let text = match variant {
                Some(variant) if !loaded.support.variant(base, variant) => {
                    &loaded.set.emojis[base].text
                }
                _ => text,
            };
            Some(GridCell {
                text: text.to_owned().into(),
                base,
            })
        };
        let mut layout = Layout::default();
        match &self.emoji.results {
            Some(found) => layout.fill(found.iter().filter_map(|base| cell(*base)).collect()),
            None => {
                // The used ones as they were picked, tone and all.
                let used: Vec<GridCell> = self
                    .emoji
                    .used
                    .iter()
                    .filter_map(|text| {
                        let (base, variant) = loaded.set.locate(text)?;
                        let drawn = loaded.support.base(base)
                            && variant.is_none_or(|at| loaded.support.variant(base, at));
                        drawn.then(|| GridCell {
                            text: text.to_owned().into(),
                            base,
                        })
                    })
                    .collect();
                if !used.is_empty() {
                    layout.section("Frequently used", FREQUENT_GLYPH);
                    layout.fill(used);
                }
                for (at, group) in loaded.set.groups.iter().enumerate() {
                    layout.section(&group.name, GROUP_GLYPHS.get(at).copied().unwrap_or("•"));
                    layout.fill(group.range.clone().filter_map(&cell).collect());
                    layout.drop_empty_section();
                }
            }
        }
        self.emoji.layout = layout;
        self.emoji.cursor = self
            .emoji
            .cursor
            .min(self.emoji.layout.cells.len().saturating_sub(1));
    }

    /// The search is over (the tab changed): the categories again, at
    /// their top.
    pub(super) fn emoji_reset_search(&mut self, cx: &mut Context<Self>) {
        self.emoji.results = None;
        self.emoji._searching = None;
        self.emoji.tones = None;
        self.emoji.cursor = 0;
        self.emoji_layout(cx);
        self.emoji.scroll.scroll_to_item(0, ScrollStrategy::Top);
    }

    // ----- opening --------------------------------------------------------------------

    /// A picker opens: empty search, the grid at its top, the keyboard in
    /// the search field.
    fn emoji_begin(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji.target = target;
        self.emoji.expanded = target != Target::Reaction;
        self.emoji.zone = Zone::Search;
        self.emoji.cursor = 0;
        self.emoji.category = 0;
        self.emoji.tones = None;
        self.emoji.results = None;
        self.emoji._searching = None;
        self.emoji._press = None;
        self.emoji.held = false;
        self.emoji
            .search
            .update(cx, |field, cx| field.set_value("", window, cx));
        self.emoji_load(cx);
        // The used emoji are as of now.
        self.emoji.used = prefs::get(cx)
            .frequent(FREQUENT)
            .into_iter()
            .map(str::to_owned)
            .collect();
        self.emoji_layout(cx);
        self.emoji.scroll.scroll_to_item(0, ScrollStrategy::Top);
        self.emoji
            .search
            .update(cx, |field, cx| field.focus(window, cx));
        cx.notify();
    }

    /// The composer's button: opens the picker on the tab used last, or
    /// closes it when it is open.
    pub(super) fn toggle_emoji_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_emoji_picker_on(None, window, cx);
    }

    /// Opens the picker on `tab` (the one used last when `None`), or
    /// closes it when it is open.
    pub(super) fn toggle_emoji_picker_on(
        &mut self,
        tab: Option<super::picker::Tab>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.overlay == Overlay::EmojiPicker {
            return self.close_overlay(window, cx);
        }
        // From a field of Status it is that field's, whatever chat is
        // open behind: it opens where the field's button was.
        let status = self.status_writing().is_some();
        if !status && !self.conversation_showing() {
            return;
        }
        let target = if status {
            self.emoji.pinned = self.emoji.anchor.get();
            Target::Status
        // From the attach sheet it is the caption's: it opens where the
        // sheet's button was, and the sheet comes back after.
        } else if self.overlay == Overlay::AttachSheet && self.attach.is_some() {
            self.emoji.pinned = self.emoji.anchor.get();
            Target::Caption
        } else {
            self.emoji.pinned = None;
            Target::Composer
        };
        // The caption's picker, and that of a field of Status, is emoji
        // only (a sticker or a GIF is a message of a conversation); the
        // composer's opens on the tab asked for, else on the one used last.
        self.picker.tab = if target != Target::Composer {
            super::picker::Tab::Emoji
        } else {
            tab.unwrap_or(crate::settings::get(cx).picker_tab)
        };
        self.picker.tab_cursor = super::picker::tab_index(self.picker.tab);
        self.picker.menu = None;
        self.open_overlay(Overlay::EmojiPicker, window, cx);
        self.emoji_begin(target, window, cx);
        self.picker_opened(window, cx);
    }

    /// The reaction sheet opened: its search takes the keyboard.
    pub(super) fn emoji_begin_reaction(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.emoji_begin(Target::Reaction, window, cx);
    }

    /// "+" of the reaction sheet: all the others.
    fn emoji_expand(&mut self, cx: &mut Context<Self>) {
        self.emoji.expanded = true;
        self.emoji.zone = Zone::Grid;
        self.emoji.cursor = 0;
        cx.notify();
    }

    /// Whether one of the two pickers is open.
    fn emoji_open(&self) -> bool {
        matches!(self.overlay, Overlay::EmojiPicker | Overlay::React)
    }

    // ----- searching ------------------------------------------------------------------

    /// The text of the search field.
    fn emoji_query(&self, cx: &gpui_kit::App) -> String {
        self.emoji.search.read(cx).value().trim().to_owned()
    }

    /// The search field changed: an empty one shows the categories again;
    /// anything else is looked for off this thread, a moment later, and a
    /// newer change drops the search under way.
    fn emoji_query_changed(&mut self, cx: &mut Context<Self>) {
        if !self.emoji_open() {
            return;
        }
        // In the other tabs the same field searches stickers and GIFs.
        if self.overlay == Overlay::EmojiPicker && self.picker.tab != super::picker::Tab::Emoji {
            return self.picker_query_changed(cx);
        }
        let query = self.emoji_query(cx);
        self.emoji._searching = None;
        self.emoji.tones = None;
        if query.is_empty() {
            if self.emoji.results.take().is_some() {
                self.emoji.cursor = 0;
                self.emoji_layout(cx);
                self.emoji.scroll.scroll_to_item(0, ScrollStrategy::Top);
            }
            return cx.notify();
        }
        // Typing in the reaction sheet is asking for the others.
        self.emoji.expanded = true;
        self.emoji.zone = Zone::Search;
        cx.notify();
        let Some(index) = self
            .emoji
            .loaded
            .as_ref()
            .map(|loaded| loaded.index.clone())
        else {
            // Asked again when the tables arrive.
            return;
        };
        self.emoji._searching = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let found = cx
                .background_spawn(async move { index.search(&query, RESULTS) })
                .await;
            this.update(cx, |this, cx| {
                this.emoji.results = Some(found);
                this.emoji.cursor = 0;
                this.emoji_layout(cx);
                this.emoji.scroll.scroll_to_item(0, ScrollStrategy::Top);
                cx.notify();
            })
            .ok();
        }));
    }

    // ----- picking --------------------------------------------------------------------

    /// An emoji was chosen: it is remembered as used, and goes where the
    /// picker was opened for. `keep` leaves the composer's picker open.
    fn emoji_pick(&mut self, text: &str, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        prefs::update(cx, |chosen| chosen.note(text));
        self.emoji.tones = None;
        match self.emoji.target {
            // Through the same path as the six: the engine's outbox.
            Target::Reaction => self.react_with(text, window, cx),
            // The story it was for went while the picker was open.
            Target::Status if self.status_writing().is_none() => {
                self.close_overlay(window, cx);
            }
            Target::Composer | Target::Caption | Target::Status => {
                let text = text.to_owned();
                self.writing()
                    .clone()
                    .update(cx, |composer, cx| composer.insert(text, window, cx));
                if !keep {
                    self.close_overlay(window, cx);
                }
            }
        }
        cx.notify();
    }

    /// Picks the cell at `index` of the grid.
    pub(super) fn emoji_pick_cell(
        &mut self,
        index: usize,
        keep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(cell) = self.emoji.layout.cells.get(index) else {
            return;
        };
        let text = cell.text.clone();
        self.emoji.cursor = index;
        self.emoji_pick(&text, keep, window, cx);
    }

    /// Enter in the grid: the cell the keyboard is on; with nothing found,
    /// an emoji typed or pasted into the search itself.
    fn emoji_pick_under_cursor(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.emoji.layout.cells.is_empty() {
            return self.emoji_pick_cell(self.emoji.cursor, keep, window, cx);
        }
        let typed = self.emoji_query(cx);
        if crate::markup::emoji_only(&typed) == Some(1) {
            self.emoji_pick(&typed, keep, window, cx);
        }
    }

    // ----- skin tones -----------------------------------------------------------------

    /// Offers the skin tones of the cell at `index`, when it has any the
    /// fonts draw.
    pub(super) fn emoji_open_tones(&mut self, index: usize, cx: &mut Context<Self>) -> bool {
        let (Some(loaded), Some(cell)) = (&self.emoji.loaded, self.emoji.layout.cells.get(index))
        else {
            return false;
        };
        let emoji = &loaded.set.emojis[cell.base];
        let mut options: Vec<(SharedString, Option<Tone>)> =
            vec![(emoji.text.to_string().into(), None)];
        options.extend(
            emoji
                .variants
                .iter()
                .enumerate()
                .filter(|(at, _)| loaded.support.variant(cell.base, *at))
                // A tone for one person, or the same for both, is a
                // preference; a mixed pair is a pick and nothing more.
                .map(|(_, variant)| (variant.text.to_string().into(), variant.uniform())),
        );
        if options.len() < 2 {
            return false;
        }
        let cursor = options
            .iter()
            .position(|(text, _)| *text == cell.text)
            .unwrap_or(0);
        self.emoji.cursor = index;
        self.emoji.tones = Some(ToneRow { options, cursor });
        cx.notify();
        true
    }

    /// Picks the option at `at` of the skin tones. A tone everybody in the
    /// emoji shares, or none, becomes the tone emoji are offered in.
    pub(super) fn emoji_pick_tone(
        &mut self,
        at: usize,
        keep: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((text, tone)) = self
            .emoji
            .tones
            .as_ref()
            .and_then(|tones| tones.options.get(at).cloned())
        else {
            return;
        };
        // The first option is the emoji without a tone: the default goes
        // back to none. A mixed pair changes nothing.
        if at == 0 || tone.is_some() {
            prefs::update(cx, |chosen| chosen.tone = tone);
            self.emoji_layout(cx);
        }
        self.emoji_pick(&text, keep, window, cx);
    }

    /// A press on a cell: held long enough, it asks for the skin tones.
    fn emoji_press(&mut self, index: usize, cx: &mut Context<Self>) {
        self.emoji.held = false;
        self.emoji._press = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LONG_PRESS).await;
            this.update(cx, |this, cx| {
                this.emoji.held = this.emoji_open_tones(index, cx);
            })
            .ok();
        }));
    }

    // ----- the keyboard ---------------------------------------------------------------

    /// Moves the keyboard to a cell and keeps it in view.
    fn emoji_move_to(&mut self, cell: usize, cx: &mut Context<Self>) {
        let Some(row) = self.emoji.layout.cell_rows.get(cell).copied() else {
            return;
        };
        // Going up into the first row of a category, its heading comes
        // along.
        let came_from = self.emoji.layout.cell_rows.get(self.emoji.cursor).copied();
        let under_heading = matches!(
            row.checked_sub(1)
                .map(|above| &self.emoji.layout.rows[above]),
            Some(GridRow::Header(_))
        );
        let shown = if under_heading && came_from.is_some_and(|from| row < from) {
            row - 1
        } else {
            row
        };
        self.emoji.cursor = cell;
        self.emoji.zone = Zone::Grid;
        self.emoji.tones = None;
        self.emoji
            .scroll
            .scroll_to_item(shown, ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Jumps to the category at `section`: its heading at the top, the
    /// keyboard on its first emoji.
    pub(super) fn emoji_go_to_section(&mut self, section: usize, cx: &mut Context<Self>) {
        let Some(found) = self.emoji.layout.sections.get(section) else {
            return;
        };
        let (row, cell) = (found.row, found.cell);
        self.emoji.cursor = cell;
        self.emoji.category = section;
        self.emoji.zone = Zone::Grid;
        self.emoji.tones = None;
        self.emoji.scroll.scroll_to_item(row, ScrollStrategy::Top);
        cx.notify();
    }

    /// The keys of the pickers, seen before the search field's own
    /// bindings. `true` when the key was the picker's.
    pub(super) fn emoji_key(
        &mut self,
        stroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.emoji_open() {
            return self.emoji_completion_key(stroke, window, cx);
        }
        let held = &stroke.modifiers;
        let plain = !held.modified();
        let shift_only = held.shift && !held.control && !held.alt && !held.platform;
        let key = stroke.key.as_str();

        // Another tab's shortcut changes tab.
        if self.overlay == Overlay::EmojiPicker {
            if let Some(tab) =
                self.picker_switch_for(keys::resolve(stroke, self.key_context(window, cx)))
            {
                self.picker_switch_by_key(tab, window, cx);
                return true;
            }
        }
        // The shortcut that opened the composer's picker closes it. Once
        // this keystroke is done with: the composer's own interceptor sees
        // the same keystroke after this one, and would open it again.
        if self.overlay == Overlay::EmojiPicker
            && self.picker_is_opener(keys::resolve(stroke, self.key_context(window, cx)))
        {
            let view = cx.weak_entity();
            window.defer(cx, move |window, cx| {
                view.update(cx, |this, cx| {
                    if this.overlay == Overlay::EmojiPicker {
                        this.close_overlay(window, cx);
                    }
                })
                .ok();
            });
            return true;
        }
        // Typing always goes to the search field.
        if !self.emoji.search.focus_handle(cx).is_focused(window) {
            self.emoji
                .search
                .update(cx, |field, cx| field.focus(window, cx));
        }

        // Stickers and GIFs have their own keys.
        if self.overlay == Overlay::EmojiPicker && self.picker.tab != super::picker::Tab::Emoji {
            return self.picker_key(stroke, window, cx);
        }

        // The skin tones, while they are on offer.
        if let Some(tones) = &mut self.emoji.tones {
            let count = tones.options.len();
            match key {
                "left" if plain => tones.cursor = (tones.cursor + count - 1) % count,
                "right" if plain => tones.cursor = (tones.cursor + 1) % count,
                "enter" if plain || shift_only => {
                    let at = tones.cursor;
                    self.emoji_pick_tone(at, shift_only, window, cx);
                }
                "escape" | "up" if plain => self.emoji.tones = None,
                // Anything else is for the grid or the search again.
                _ => {
                    self.emoji.tones = None;
                    cx.notify();
                    return self.emoji_key(stroke, window, cx);
                }
            }
            cx.notify();
            return true;
        }

        // The reaction sheet with only its six: the arrows walk them,
        // Enter or a digit picks, Tab or Down opens the rest.
        if self.overlay == Overlay::React && !self.emoji.expanded {
            let count = QUICK_REACTIONS.len();
            match key {
                "left" if plain => {
                    self.acting.reaction_cursor = (self.acting.reaction_cursor + count - 1) % count
                }
                "right" if plain => {
                    self.acting.reaction_cursor = (self.acting.reaction_cursor + 1) % count
                }
                "enter" if plain => {
                    let emoji = QUICK_REACTIONS[self.acting.reaction_cursor.min(count - 1)];
                    self.react_with(emoji, window, cx);
                }
                "1" | "2" | "3" | "4" | "5" | "6" if plain => {
                    let at: usize = key.parse().unwrap_or(1);
                    self.react_with(QUICK_REACTIONS[at - 1], window, cx);
                }
                "tab" | "down" if plain => self.emoji_expand(cx),
                _ => return false,
            }
            cx.notify();
            return true;
        }

        let cells = self.emoji.layout.cells.len();
        let searching = !self.emoji_query(cx).is_empty();
        match key {
            // Escape empties the search first; then it closes, as it does
            // every overlay.
            "escape" if plain && searching => {
                self.emoji
                    .search
                    .update(cx, |field, cx| field.set_value("", window, cx));
                self.emoji.zone = Zone::Search;
                // Setting a value says nothing: the grid is told here.
                self.emoji_query_changed(cx);
            }
            // The bar of tabs, over the search.
            "enter" if (plain || shift_only) && self.emoji.zone == Zone::Tabs => {
                let tab = super::picker::TABS[self.picker.tab_cursor.min(2)];
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
            "enter" if (plain || shift_only) && self.emoji.zone == Zone::Categories => {
                self.emoji_go_to_section(self.emoji.category, cx)
            }
            "enter" if plain || shift_only => {
                // The composer's picker closes on Enter and stays with
                // Shift; a reaction is one pick either way.
                self.emoji_pick_under_cursor(shift_only, window, cx)
            }
            "tab" if plain || shift_only => {
                // Search, categories, grid, and around. A search has no
                // categories.
                let mut order = vec![Zone::Search];
                if self.emoji.target == Target::Composer {
                    order.push(Zone::Tabs);
                }
                if self.emoji.results.is_none() {
                    order.push(Zone::Categories);
                }
                order.push(Zone::Grid);
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
                    self.picker.tab_cursor = super::picker::tab_index(self.picker.tab);
                }
                if self.emoji.zone == Zone::Categories {
                    self.emoji.category = self.emoji.layout.section_of(self.emoji.cursor);
                }
                if self.emoji.zone == Zone::Grid {
                    self.emoji_move_to(self.emoji.cursor, cx);
                }
            }
            "down" if held.alt && !held.control && !held.platform => {
                self.emoji_open_tones(self.emoji.cursor, cx);
            }
            "left" | "right" if plain && self.emoji.zone == Zone::Categories => {
                let count = self.emoji.layout.sections.len().max(1);
                self.emoji.category = if key == "left" {
                    (self.emoji.category + count - 1) % count
                } else {
                    (self.emoji.category + 1) % count
                };
            }
            // With something typed and the keyboard not in the grid, Left
            // and Right are the text's.
            "left" | "right" if plain && searching && self.emoji.zone == Zone::Search => {
                return false;
            }
            "left" if plain && cells > 0 => {
                self.emoji_move_to(self.emoji.cursor.saturating_sub(1), cx)
            }
            "right" if plain && cells > 0 => {
                self.emoji_move_to((self.emoji.cursor + 1).min(cells - 1), cx)
            }
            "down" if plain && self.emoji.zone != Zone::Grid => {
                // Into the grid, on the cell that is lit.
                self.emoji_move_to(self.emoji.cursor, cx);
                self.emoji.zone = Zone::Grid;
            }
            "down" if plain => {
                if let Some(cell) = self.emoji.layout.step_row(self.emoji.cursor, true) {
                    self.emoji_move_to(cell, cx);
                }
            }
            "up" if plain && self.emoji.zone == Zone::Grid => {
                match self.emoji.layout.step_row(self.emoji.cursor, false) {
                    Some(cell) => self.emoji_move_to(cell, cx),
                    // Above the first row is the search.
                    None => {
                        self.emoji.zone = Zone::Search;
                        self.emoji.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    }
                }
            }
            "up" if plain => self.emoji.zone = Zone::Search,
            "pagedown" | "pageup" if plain && self.emoji.results.is_none() => {
                let at = self.emoji.layout.section_of(self.emoji.cursor);
                let count = self.emoji.layout.sections.len();
                let next = if key == "pagedown" {
                    (at + 1).min(count.saturating_sub(1))
                } else if self
                    .emoji
                    .layout
                    .sections
                    .get(at)
                    .map(|section| section.cell)
                    == Some(self.emoji.cursor)
                {
                    at.saturating_sub(1)
                } else {
                    // Back to the start of this category first.
                    at
                };
                self.emoji_go_to_section(next, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    // ----- drawing --------------------------------------------------------------------

    /// How many rows of the grid fit over the composer, or in the window.
    pub(super) fn emoji_rows(&self) -> usize {
        let chrome = BAR() * 3. + PAD() * 4. + px(120.);
        let room = ((self.viewport.height - chrome) / CELL()).floor() as usize;
        room.clamp(3, ROWS)
    }

    /// The size the reaction sheet has right now, for keeping it inside
    /// the window.
    pub(super) fn react_sheet_size(&self) -> gpui_kit::Size<Pixels> {
        if self.emoji.expanded {
            gpui_kit::size(
                GRID_WIDTH() + PAD() * 2. + px(8.),
                CELL() * self.emoji_rows() as f32 + BAR() * 3. + px(40.) + px(60.),
            )
        } else {
            gpui_kit::size(px(320.), px(140.))
        }
    }

    /// The composer's emoji button.
    pub(super) fn render_emoji_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let anchor = self.emoji.anchor.clone();
        let hint: SharedString = match keys::keys_label(Command::EmojiPicker) {
            Some(keys) => format!("Emoji ({keys})").into(),
            None => "Emoji".into(),
        };
        icon_button("emoji", IconName::FaceSlightlySmiling, palette)
            .relative()
            .rounded_full()
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_emoji_picker(window, cx)))
            // Where it is, for the picker to open above it.
            .child(
                canvas(
                    move |bounds, _, _| anchor.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }

    /// The caption's emoji button, in the attach sheet: the same picker,
    /// for the caption.
    pub(super) fn render_caption_emoji_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let anchor = self.emoji.anchor.clone();
        let hint = super::hints::with_keys("Emoji", Some(Command::EmojiPicker));
        icon_button("caption-emoji", IconName::FaceSlightlySmiling, palette)
            .relative()
            .rounded_full()
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.toggle_emoji_picker(window, cx)
            }))
            .child(
                canvas(
                    move |bounds, _, _| anchor.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }

    /// The search field of a picker. `selector` names its box for tests.
    pub(super) fn render_emoji_search(&self, selector: &'static str, palette: &Palette) -> Div {
        div()
            .debug_selector(move || selector.into())
            .flex_1()
            .min_w_0()
            .h(BAR())
            .px_2()
            .rounded(metrics::RADIUS())
            .border_1()
            .border_color(if self.emoji.zone == Zone::Search {
                palette.focus_ring
            } else {
                palette.elevated_border
            })
            .flex()
            .items_center()
            .gap_2()
            .child(icon(IconName::Search, px(14.), palette.text_faint))
            .child(Input::new(&self.emoji.search).appearance(false))
    }

    /// One cell of the grid.
    fn render_emoji_cell(
        &self,
        index: usize,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cell = &self.emoji.layout.cells[index];
        let on = index == self.emoji.cursor;
        let in_grid = on && self.emoji.zone == Zone::Grid;
        let toned = self
            .emoji
            .loaded
            .as_ref()
            .is_some_and(|loaded| !loaded.set.emojis[cell.base].variants.is_empty());
        let keep = self.emoji.target == Target::Composer;
        let hover = palette.elevated_selected;
        div()
            .id(("emoji-cell", index))
            .debug_selector(move || format!("emoji-cell-{index}"))
            .relative()
            .flex_none()
            .size(CELL())
            .rounded(metrics::RADIUS())
            .border_2()
            .border_color(if in_grid {
                palette.focus_ring
            } else {
                gpui_kit::transparent_black()
            })
            .when(on, |this| this.bg(palette.elevated_selected))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .text_size(px(22.))
            .hover(move |style| style.bg(hover))
            // The name under the grid follows the pointer too.
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered && this.emoji.cursor != index && this.emoji.tones.is_none() {
                    this.emoji.cursor = index;
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| this.emoji_press(index, cx)),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.emoji_open_tones(index, cx);
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.emoji._press = None;
                // The release of a long press chose nothing yet.
                if std::mem::take(&mut this.emoji.held) {
                    return;
                }
                this.emoji_pick_cell(index, keep, window, cx);
            }))
            .child(cell.text.clone())
            // A corner mark on what comes in skin tones.
            .when(toned, |this| {
                this.child(
                    div()
                        .absolute()
                        .right(px(3.))
                        .bottom(px(3.))
                        .size(px(4.))
                        .rounded_full()
                        .bg(palette.text_faint),
                )
            })
            .into_any_element()
    }

    /// One row of the grid: a heading, or its cells.
    fn render_emoji_row(
        &self,
        row: usize,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &self.emoji.layout.rows[row] {
            GridRow::Header(section) => {
                let title = self.emoji.layout.sections[*section].title.clone();
                let section = *section;
                div()
                    .debug_selector(move || format!("emoji-section-{section}"))
                    .h(CELL())
                    .px(px(4.))
                    .pb(px(4.))
                    .flex()
                    .items_end()
                    .child(
                        mono(format!("[ {} ]", title.to_uppercase()))
                            .text_color(palette.text_muted),
                    )
                    .into_any_element()
            }
            GridRow::Cells(range) => div()
                .h(CELL())
                .flex()
                .children(
                    range
                        .clone()
                        .map(|index| self.render_emoji_cell(index, palette, cx)),
                )
                .into_any_element(),
        }
    }

    /// The bar of categories; while searching, what was found.
    fn render_emoji_categories(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let bar = div()
            .debug_selector(|| "emoji-categories".into())
            .flex_none()
            .h(BAR())
            .flex()
            .items_center()
            .justify_between();
        if let Some(found) = &self.emoji.results {
            let cells = self.emoji.layout.cells.len();
            let said = match cells {
                0 if found.is_empty() => "Nothing found".to_owned(),
                0 => "Nothing this computer's fonts can draw".to_owned(),
                1 => "1 emoji".to_owned(),
                n => format!("{n} emoji"),
            };
            return bar.px(px(4.)).child(
                mono(said.to_uppercase())
                    .debug_selector(|| "emoji-found".into())
                    .text_color(palette.text_muted),
            );
        }
        let current = self.emoji.layout.section_of(self.emoji.cursor);
        let in_bar = self.emoji.zone == Zone::Categories;
        bar.children(
            self.emoji
                .layout
                .sections
                .iter()
                .enumerate()
                .map(|(at, section)| {
                    let hover = palette.elevated_selected;
                    let title = section.title.clone();
                    div()
                        .id(("emoji-category", at))
                        .debug_selector(move || format!("emoji-category-{at}"))
                        .flex_none()
                        .size(px(30.))
                        .rounded(metrics::RADIUS())
                        .border_2()
                        .border_color(if in_bar && at == self.emoji.category {
                            palette.focus_ring
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .when(at == current, |this| this.bg(palette.elevated_selected))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .text_size(px(16.))
                        .hover(move |style| style.bg(hover))
                        .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.emoji_go_to_section(at, cx);
                        }))
                        .child(section.glyph)
                }),
        )
    }

    /// Under the grid: the emoji the keyboard is on, with its name in the
    /// search language; or its skin tones.
    fn render_emoji_strip(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let strip = div()
            .flex_none()
            .min_h(px(44.))
            .pt(px(6.))
            .border_t_1()
            .border_color(palette.elevated_border)
            .flex()
            .items_center()
            .gap_2();
        if let Some(tones) = &self.emoji.tones {
            let keep = self.emoji.target == Target::Composer;
            return strip
                .debug_selector(|| "emoji-tones".into())
                .flex_wrap()
                .gap(px(2.))
                .children(tones.options.iter().enumerate().map(|(at, (text, _))| {
                    let hover = palette.elevated_selected;
                    div()
                        .id(("emoji-tone", at))
                        .debug_selector(move || format!("emoji-tone-{at}"))
                        .flex_none()
                        .size(CELL())
                        .rounded(metrics::RADIUS())
                        .border_2()
                        .border_color(if at == tones.cursor {
                            palette.focus_ring
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .text_size(px(22.))
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.emoji_pick_tone(at, keep, window, cx);
                        }))
                        .child(text.clone())
                }));
        }
        let strip = strip.debug_selector(|| "emoji-preview".into());
        let (Some(loaded), Some(cell)) = (
            &self.emoji.loaded,
            self.emoji.layout.cells.get(self.emoji.cursor),
        ) else {
            let said = if self.emoji.loaded.is_none() {
                "Loading…"
            } else {
                "Try another word, or a shortcode like :fire:"
            };
            return strip.child(
                div()
                    .px(px(4.))
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_muted)
                    .child(said),
            );
        };
        let emoji = &loaded.set.emojis[cell.base];
        let code = emoji
            .shortcodes
            .first()
            .map(|code| format!(":{code}:"))
            .unwrap_or_default();
        strip
            .child(
                div()
                    .flex_none()
                    .w(CELL())
                    .flex()
                    .justify_center()
                    .text_size(px(26.))
                    .child(cell.text.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .debug_selector(|| "emoji-preview-name".into())
                            .truncate()
                            .text_size(metrics::TEXT_SMALL())
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(local_name(loaded, cell.base))),
                    )
                    .child(
                        mono(code)
                            .truncate()
                            .text_size(px(10.))
                            .text_color(palette.text_muted),
                    ),
            )
            .when(!emoji.variants.is_empty(), |this| {
                this.child(
                    mono(format!(
                        "{} tones",
                        keys::Chord {
                            key: "down",
                            secondary: false,
                            shift: false,
                            alt: true,
                            control: false,
                        }
                        .label()
                    ))
                    .flex_none()
                    .text_size(px(10.))
                    .text_color(palette.text_faint),
                )
            })
    }

    /// The picker under its search field: the categories, the grid, and
    /// the strip that names the emoji the keyboard is on.
    pub(super) fn render_emoji_body(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let colours = *palette;
        let rows = self.emoji.layout.rows.len();
        let built = self.emoji.built.clone();
        let grid = div()
            .relative()
            .flex_none()
            .w(GRID_WIDTH())
            .h(CELL() * self.emoji_rows() as f32)
            .when(rows > 0, |this| {
                this.child(
                    uniform_list(
                        "emoji-grid",
                        rows,
                        cx.processor(move |this, range: Range<usize>, _, cx| {
                            // Only the rows in view are built.
                            built.set(range.clone());
                            range
                                .map(|row| this.render_emoji_row(row, &colours, cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .track_scroll(&self.emoji.scroll)
                    .size_full(),
                )
                .child(Scrollbar::vertical(&self.emoji.scroll))
            });
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(self.render_emoji_categories(palette, cx))
            .child(div().debug_selector(|| "emoji-grid".into()).child(grid))
            .child(self.render_emoji_strip(palette, cx))
    }

    /// The composer's picker, above its button.
    pub(super) fn render_emoji_picker(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let width = GRID_WIDTH() + PAD() * 2. + px(2.);
        let (left, bottom) = match self.emoji.pinned.or(self.emoji.anchor.get()) {
            Some(button) => (button.left(), self.viewport.height - button.top() + px(10.)),
            None => (metrics::RAIL_WIDTH() + self.list_px + px(16.), px(64.)),
        };
        // A field of Status can be near the top of the window (the words
        // of a post): where the picker has no room over its button it
        // opens under it, and never past an edge of the window.
        let under = self
            .emoji
            .pinned
            .filter(|_| self.emoji.target == Target::Status)
            .and_then(|button| {
                let tall = PAD() * 2. + BAR() * 3. + CELL() * self.emoji_rows() as f32 + px(24.);
                (button.top() - px(10.) - tall < px(8.)).then(|| {
                    (button.bottom() + px(10.))
                        .min(self.viewport.height - tall - px(8.))
                        .max(px(8.))
                })
            });
        self.card("emoji-picker", palette)
            .absolute()
            .left(left.min(self.viewport.width - width - px(8.)).max(px(8.)))
            .map(|this| match under {
                Some(top) => this.top(top),
                None => this.bottom(bottom),
            })
            .w(width)
            .p(PAD())
            .flex()
            .flex_col()
            .gap(px(6.))
            .when(self.emoji.target == Target::Composer, |this| {
                this.child(self.render_picker_tabs(palette, cx))
            })
            .child(
                div()
                    .flex()
                    .child(self.render_emoji_search("emoji-search", palette)),
            )
            .child(match self.picker.tab {
                super::picker::Tab::Emoji => self.render_emoji_body(palette, cx),
                _ => self.render_tile_body(palette, cx),
            })
    }

    /// "+" at the end of the six quick reactions: all the others.
    pub(super) fn render_reaction_more(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let hover = palette.elevated_selected;
        div()
            .id("reaction-more")
            .debug_selector(|| "reaction-more".into())
            .size(px(40.))
            .rounded_full()
            .border_1()
            .border_color(palette.elevated_border)
            .when(self.emoji.expanded, |this| {
                this.bg(palette.elevated_selected)
            })
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(|window, cx| Tooltip::new("All emoji").build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.emoji_expand(cx);
            }))
            .child(icon(IconName::Plus, px(16.), palette.icon))
    }

    /// The rest of the picker in the reaction sheet, once it was asked for.
    pub(super) fn render_reaction_body(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        self.emoji
            .expanded
            .then(|| self.render_emoji_body(palette, cx))
    }

    // ----- a shortcode typed in the composer --------------------------------------------

    /// The composer's text up to the caret, and what follows it.
    fn emoji_around_caret(&self, cx: &gpui_kit::App) -> (String, String) {
        let composer = self.writing().read(cx);
        let text = composer.value().to_string();
        let mut caret = composer.cursor().min(text.len());
        while !text.is_char_boundary(caret) {
            caret -= 1;
        }
        (text[..caret].to_owned(), text[caret..].to_owned())
    }

    /// The picker that the caption opened is closing: the attach sheet
    /// is what was under it, and the caption takes the keyboard back.
    /// `false` when it was not that picker.
    pub(super) fn emoji_back_to_caption(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.overlay != Overlay::EmojiPicker
            || self.emoji.target != Target::Caption
            || self.attach.is_none()
        {
            return false;
        }
        self.emoji.target = Target::Composer;
        self.emoji.pinned = None;
        self.overlay = Overlay::AttachSheet;
        // The file that was on show, and its caret.
        self.attach_wants_focus = true;
        cx.notify();
        true
    }

    /// The picker that a field of Status opened is closing: what was
    /// under it is back (the post sheet, or the story), and the field
    /// takes the keyboard again. `false` when it was not that picker, or
    /// when what it was opened from is gone.
    pub(super) fn emoji_back_to_status(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.overlay != Overlay::EmojiPicker || self.emoji.target != Target::Status {
            return false;
        }
        let field = self.status_writing().cloned();
        self.emoji.target = Target::Composer;
        self.emoji.pinned = None;
        let Some(field) = field else {
            return false;
        };
        self.overlay = if self.status.sheet == Some(super::status_post::Sheet::Post) {
            Overlay::Status
        } else {
            Overlay::None
        };
        field.update(cx, |field, cx| field.focus(window, cx));
        cx.notify();
        true
    }

    /// The emoji button of a field of Status: the same picker, for that
    /// field.
    pub(super) fn render_status_emoji_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let anchor = self.emoji.anchor.clone();
        let hint = super::hints::with_keys("Emoji", Some(Command::EmojiPicker));
        icon_button("status-emoji", IconName::FaceSlightlySmiling, palette)
            .relative()
            .rounded_full()
            .tooltip(move |window, cx| Tooltip::new(hint.clone()).build(window, cx))
            .on_click(cx.listener(|this, _, window, cx| {
                cx.stop_propagation();
                this.toggle_emoji_picker(window, cx)
            }))
            .child(
                canvas(
                    move |bounds, _, _| anchor.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }

    /// Nothing is offered for a shortcode any more: the field it was
    /// typed in is not the one in hand now.
    pub(super) fn emoji_completion_reset(&mut self) {
        self.emoji.completion._search = None;
        self.emoji.completion.items.clear();
        self.emoji.completion.dismissed = None;
    }

    /// Escape while emoji are offered for a shortcode closes the list,
    /// and nothing else. `false` when none is offered.
    pub(super) fn emoji_completion_dismiss(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.emoji_completing() {
            return false;
        }
        self.emoji.completion.dismissed = Some(self.emoji_around_caret(cx).0);
        self.emoji.completion.items.clear();
        cx.notify();
        true
    }

    /// Whether the field being written in has the keyboard, with nothing
    /// over it: the composer, the caption in its sheet, or a field of
    /// Status.
    fn writing_has_keyboard(&self, window: &Window, cx: &gpui_kit::App) -> bool {
        let over = match self.overlay {
            Overlay::None => false,
            Overlay::AttachSheet => !self.captioning(),
            Overlay::Status => self.status_writing().is_none(),
            _ => true,
        };
        !over && self.writing().focus_handle(cx).is_focused(window)
    }

    /// Whether the composer is offering emoji for a shortcode.
    pub(super) fn emoji_completing(&self) -> bool {
        !self.emoji.completion.items.is_empty()
    }

    /// The composer's text changed: a shortcode being typed before the
    /// caret (`:fir`) is looked up with the picker's own search, off this
    /// thread.
    pub(super) fn emoji_completion_changed(&mut self, cx: &mut Context<Self>) {
        self.emoji.completion._search = None;
        let (before, _) = self.emoji_around_caret(cx);
        let typed = Index::typing(&before).map(str::to_owned);
        if self.emoji.completion.dismissed.as_deref() != Some(before.as_str()) {
            self.emoji.completion.dismissed = None;
        }
        // Where a shortcode can be completed: in a field of Status, or in
        // a conversation that is on screen (its composer, or a caption).
        let here = self.status_writing().is_some()
            || ((self.overlay == Overlay::None || self.captioning())
                && self.conversation_showing());
        let Some(typed) = typed.filter(|_| here && self.emoji.completion.dismissed.is_none())
        else {
            if !self.emoji.completion.items.is_empty() {
                self.emoji.completion.items.clear();
                cx.notify();
            }
            return;
        };
        self.emoji_load(cx);
        let Some(loaded) = &self.emoji.loaded else {
            // Asked again when the tables arrive.
            return;
        };
        let (index, set, support) = (
            loaded.index.clone(),
            loaded.set.clone(),
            loaded.support.clone(),
        );
        let tone = prefs::get(cx).tone;
        self.emoji.completion._search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let items = cx
                .background_spawn(async move {
                    let folded = typed.to_lowercase();
                    index
                        .search(&format!(":{typed}"), COMPLETIONS * 4)
                        .into_iter()
                        .filter(|base| support.base(*base))
                        .take(COMPLETIONS)
                        .map(|base| {
                            let emoji = &set.emojis[base];
                            let (text, variant) = set.in_tone(base, tone);
                            let text = match variant {
                                Some(variant) if !support.variant(base, variant) => &emoji.text,
                                _ => text,
                            };
                            // Under the shortcode that was being typed,
                            // when it has one that starts so.
                            let code = emoji
                                .shortcodes
                                .iter()
                                .find(|code| code.starts_with(&folded))
                                .or(emoji.shortcodes.first())
                                .map(|code| format!(":{code}:"))
                                .unwrap_or_default();
                            (
                                SharedString::from(text.to_owned()),
                                SharedString::from(code),
                            )
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                this.emoji.completion.items = items;
                this.emoji.completion.cursor = 0;
                cx.notify();
            })
            .ok();
        }));
    }

    /// Puts the offered emoji at `at` in place of the shortcode typed.
    pub(super) fn emoji_complete(
        &mut self,
        at: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((emoji, _)) = self.emoji.completion.items.get(at).cloned() else {
            return;
        };
        let (before, after) = self.emoji_around_caret(cx);
        let Some(typed) = Index::typing(&before) else {
            return;
        };
        // The colon goes with what was typed after it.
        let kept = &before[..before.len() - typed.len() - 1];
        let before = format!("{kept}{emoji}");
        prefs::update(cx, |chosen| chosen.note(&emoji));
        self.emoji.completion.items.clear();
        // What follows the caret is put back first and the rest typed in
        // front of it, which leaves the caret right after the emoji.
        self.writing().clone().update(cx, |composer, cx| {
            composer.set_value(after, window, cx);
            composer.insert(before, window, cx);
            composer.focus(window, cx);
        });
        cx.notify();
    }

    /// The keys of the list of emoji over the composer: the arrows move,
    /// Enter or Tab picks. (Escape is `emoji_completion_escape`.) The
    /// list of people to mention comes first when both could be meant.
    fn emoji_completion_key(
        &mut self,
        stroke: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count = self.emoji.completion.items.len();
        if count == 0
            || stroke.modifiers.modified()
            || !self.writing_has_keyboard(window, cx)
            || !self.mention_matches(cx).is_empty()
        {
            return false;
        }
        let cursor = self.emoji.completion.cursor.min(count - 1);
        match stroke.key.as_str() {
            "down" => self.emoji.completion.cursor = (cursor + 1) % count,
            "up" => self.emoji.completion.cursor = (cursor + count - 1) % count,
            "enter" | "tab" => self.emoji_complete(cursor, window, cx),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Escape in the composer while the list is open closes the list, and
    /// nothing else; it stays closed until the text changes.
    pub(super) fn emoji_completion_escape(
        &mut self,
        event: &gpui_kit::KeyDownEvent,
        cx: &mut Context<Self>,
    ) -> bool {
        if event.keystroke.key != "escape" || !self.emoji_completion_dismiss(cx) {
            return false;
        }
        cx.stop_propagation();
        true
    }

    /// The list of emoji over the composer, while a shortcode is typed.
    pub(super) fn render_emoji_completion(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.emoji_completing() || !self.mention_matches(cx).is_empty() {
            return None;
        }
        let cursor = self
            .emoji
            .completion
            .cursor
            .min(self.emoji.completion.items.len() - 1);
        let mut list = div()
            .debug_selector(|| "emoji-completion".into())
            .mb(px(6.))
            .p(px(4.))
            .rounded(metrics::RADIUS())
            .border_1()
            .border_color(palette.elevated_border)
            .bg(palette.elevated)
            .flex()
            .flex_col();
        for (index, (emoji, code)) in self.emoji.completion.items.iter().enumerate() {
            let hover = palette.elevated_selected;
            list = list.child(
                div()
                    .id(("emoji-completion", index))
                    .debug_selector(move || format!("emoji-completion-{index}"))
                    .px_2()
                    .h(px(32.))
                    .rounded(px(4.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .cursor_pointer()
                    .when(index == cursor, |this| this.bg(palette.elevated_selected))
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.emoji_complete(index, window, cx)
                    }))
                    .child(
                        div()
                            .flex_none()
                            .w(px(24.))
                            .flex()
                            .justify_center()
                            .text_size(px(18.))
                            .child(emoji.clone()),
                    )
                    .child(
                        mono(code.clone())
                            .min_w_0()
                            .truncate()
                            .text_color(palette.text_muted),
                    ),
            );
        }
        Some(list)
    }
}

// ----- the setting ---------------------------------------------------------------------

/// The choices of the setting, in the order it steps through them.
fn language_choices() -> Vec<LanguageChoice> {
    std::iter::once(LanguageChoice::System)
        .chain(Lang::ALL.into_iter().map(LanguageChoice::Lang))
        .collect()
}

/// Settings > Appearance > Emoji search language: the system's, or one of
/// the languages there are names for. Twelve choices do not fit side by
/// side, so the two arrows step through them.
pub(super) fn language_row(palette: &Palette, cx: &mut Context<Shell>) -> Div {
    let chosen = prefs::language(cx);
    let system = if cfg!(test) { None } else { locale::system() };
    let detected = locale::resolve(LanguageChoice::System, system.as_ref()).lang;
    let label: SharedString = match chosen {
        LanguageChoice::System => format!("System ({})", detected.label()).into(),
        LanguageChoice::Lang(lang) => lang.label().into(),
    };
    let step = |id: &'static str, glyph: IconName, by: usize| {
        icon_button(id, glyph, palette).on_click(move |_, _, cx| {
            cx.stop_propagation();
            let choices = language_choices();
            let at = choices
                .iter()
                .position(|choice| *choice == prefs::language(cx))
                .unwrap_or(0);
            let next = choices[(at + by) % choices.len()];
            prefs::update(cx, |chosen| chosen.language = next);
            cx.refresh_windows();
        })
    };
    div()
        .debug_selector(|| "emoji-language".into())
        .py_2()
        .flex()
        .items_center()
        .justify_between()
        .gap_4()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(
                    div()
                        .text_size(metrics::TEXT_BODY())
                        .font_weight(FontWeight::MEDIUM)
                        .child("Emoji search language"),
                )
                .child(
                    div()
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.text_muted)
                        .child(
                            "What emoji are called when you search them. English always works too.",
                        ),
                ),
        )
        .child(
            div()
                .flex_none()
                .p(px(2.))
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(palette.border)
                .bg(palette.surface)
                .flex()
                .items_center()
                .gap(px(2.))
                .child(step(
                    "emoji-language-previous",
                    IconName::ChevronLeft,
                    language_choices().len() - 1,
                ))
                .child(
                    div()
                        .debug_selector(|| "emoji-language-value".into())
                        .w(px(132.))
                        .flex()
                        .justify_center()
                        .text_size(metrics::TEXT_SMALL())
                        .font_weight(FontWeight::MEDIUM)
                        .child(label),
                )
                .child(step("emoji-language-next", IconName::ChevronRight, 1)),
        )
}
