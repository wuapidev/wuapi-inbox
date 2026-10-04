//! What the Stickers and GIFs tabs draw: the bar over the grid, the grid,
//! and the strip under it. The behaviour is in `picker.rs`.

use super::emoji_picker::{Zone, BAR, CELL, GRID_WIDTH};
use super::picker::{
    texts, Action, Row, Source, Tab, TileKind, GIF_CELL_H, GIF_CELL_W, STICKER_CELL,
};
use super::shell::Shell;
use super::widgets::mono;
use crate::gifs::GifEntry;
use crate::icons::{icon, IconName};
use crate::keys::{self, Command};
use crate::theme::{metrics, px, Palette};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, list, AnyElement, Context, Div, FontWeight, MouseButton, MouseDownEvent, Pixels,
    SharedString, Stateful,
};

impl Shell {
    /// The row height of the grid on show.
    fn picker_row_height(&self) -> Pixels {
        match self.picker.tab {
            Tab::Gifs => GIF_CELL_H(),
            _ => STICKER_CELL(),
        }
    }

    /// The bar over the grid: the sections of the stickers, or where the
    /// GIFs are looked for.
    fn render_tile_bar(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let bar = div()
            .debug_selector(|| "picker-bar".into())
            .flex_none()
            .h(BAR())
            .flex()
            .items_center()
            .justify_between();
        if self.picker.tab == Tab::Gifs {
            let source =
                |id: &'static str, label: &'static str, which: Source, cx: &mut Context<Self>| {
                    let on = self.picker.source == which;
                    let hover = palette.elevated_selected;
                    div()
                        .id(id)
                        .debug_selector(move || id.into())
                        .h(px(28.))
                        .px(px(12.))
                        .rounded(metrics::RADIUS())
                        .when(on, |this| this.bg(palette.elevated_selected))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(if on { palette.text } else { palette.text_muted })
                        .font_weight(if on {
                            FontWeight::MEDIUM
                        } else {
                            FontWeight::NORMAL
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.picker_set_source(which, window, cx);
                        }))
                        .child(label)
                };
            return bar
                .px(px(2.))
                .gap(px(4.))
                .justify_start()
                .child(source("picker-source-saved", "Saved", Source::Saved, cx))
                .child(source("picker-source-online", "Giphy", Source::Online, cx));
        }
        let grid = self.picker.grid();
        let current = grid.section_of(grid.cursor);
        bar.children(grid.sections.iter().enumerate().map(|(at, section)| {
            let hover = palette.elevated_selected;
            let title = section_tip(&section.title, grid_count(grid, at));
            div()
                .id(("picker-section", at))
                .debug_selector(move || format!("picker-section-{at}"))
                .flex_none()
                .size(px(30.))
                .rounded(metrics::RADIUS())
                .when(at == current, |this| this.bg(palette.elevated_selected))
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.picker_go_to_section(at, cx);
                }))
                .child(icon(section.icon, px(15.), palette.icon))
        }))
    }

    /// One tile of a grid.
    fn render_tile(&self, index: usize, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let grid = self.picker.grid();
        let tile = &grid.tiles[index];
        let (width, height) = match self.picker.tab {
            Tab::Gifs => (GIF_CELL_W(), GIF_CELL_H()),
            _ => (STICKER_CELL(), STICKER_CELL()),
        };
        let on = index == grid.cursor;
        let in_grid = on && self.emoji.zone == Zone::Grid;
        let hover = palette.elevated_selected;
        let key = tile.key();
        let starred = matches!(&tile.kind, TileKind::Sticker(item) if item.favorite);
        // A place for a sticker of a chat is asked for when it is built,
        // which is when it is in view.
        if let Some(item) = tile.item().filter(|item| item.pending()) {
            self.picker_fetch(item, false, cx);
        }
        div()
            .id(("picker-tile", index))
            .debug_selector(move || format!("picker-tile-{index}"))
            .relative()
            .flex_none()
            .w(width)
            .h(height)
            .p(px(3.))
            .child(
                div()
                    .relative()
                    .size_full()
                    .rounded(metrics::RADIUS())
                    .border_2()
                    .border_color(if in_grid {
                        palette.focus_ring
                    } else {
                        gpui_kit::transparent_black()
                    })
                    .when(on, |this| this.bg(palette.elevated_selected))
                    .p(px(2.))
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(self.render_tile_picture(tile, palette, cx))
                    .when(starred, |this| {
                        this.child(div().absolute().top(px(3.)).right(px(3.)).child(icon(
                            IconName::Star,
                            px(11.),
                            palette.warning,
                        )))
                    }),
            )
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.picker_hovered(index, &key, *hovered, cx);
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.picker_open_menu(index, cx);
                }),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.picker_send(index, false, window, cx);
            }))
            .into_any_element()
    }

    /// One row: a heading, a line of words, or tiles. Rows have the
    /// height of what they hold: a heading is one line, words wrap inside
    /// the popover, tiles are a cell high.
    fn render_tile_row(&self, row: usize, palette: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let height = self.picker_row_height();
        self.picker.built.set(widen(self.picker.built.take(), row));
        match &self.picker.grid().rows[row] {
            Row::Header(section) => {
                let title = self.picker.grid().sections[*section].title.clone();
                let at = *section;
                div()
                    .debug_selector(move || format!("picker-heading-{at}"))
                    .w_full()
                    .px(px(4.))
                    .pt(px(if at == 0 { 2. } else { 8. }))
                    .pb(px(2.))
                    .child(
                        mono(format!("[ {} ]", title.to_uppercase()))
                            .text_color(palette.text_muted),
                    )
                    .into_any_element()
            }
            Row::Note(text) => div()
                .debug_selector(move || format!("picker-note-{row}"))
                .w_full()
                .px(px(6.))
                .pb(px(4.))
                .text_size(metrics::TEXT_SMALL())
                .text_color(palette.text_muted)
                .child(text.clone())
                .into_any_element(),
            Row::Tiles(range) => div()
                .w_full()
                .h(height)
                .flex()
                .children(
                    range
                        .clone()
                        .map(|index| self.render_tile(index, palette, cx)),
                )
                .into_any_element(),
        }
    }

    /// The grid, as many rows as are in view.
    fn render_tile_grid(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let colours = *palette;
        let rows = self.picker.grid().rows.len();
        // The rows asked for from here on are this frame's.
        self.picker.built.set(0..0);
        let scroll = self.picker.scroll().clone();
        div()
            .relative()
            .flex_none()
            .w(GRID_WIDTH())
            .h(CELL() * self.emoji_rows() as f32)
            .when(rows > 0, |this| {
                this.child(
                    list(
                        scroll.clone(),
                        cx.processor(move |this, row: usize, _, cx| {
                            this.render_tile_row(row, &colours, cx)
                        }),
                    )
                    .size_full(),
                )
                .child(Scrollbar::vertical(&scroll))
            })
    }

    /// A small button of the strip.
    fn strip_button(
        &self,
        id: &'static str,
        glyph: IconName,
        tip: impl Into<SharedString>,
        palette: &Palette,
    ) -> Stateful<Div> {
        let hover = palette.elevated_selected;
        let tip: SharedString = tip.into();
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex_none()
            .size(px(30.))
            .rounded(metrics::RADIUS())
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
            .child(icon(glyph, px(15.), palette.icon))
    }

    /// Under the grid: what is on, what can be done to it, or why nothing
    /// can be sent.
    fn render_tile_strip(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let strip = div()
            .debug_selector(|| "picker-strip".into())
            .flex_none()
            .min_h(px(44.))
            .pt(px(6.))
            .border_t_1()
            .border_color(palette.elevated_border)
            .flex()
            .items_center()
            .gap_2();
        let words = |text: SharedString, colour| {
            div()
                .flex_1()
                .min_w_0()
                .px(px(4.))
                .text_size(metrics::TEXT_SMALL())
                .text_color(colour)
                .child(text)
        };
        // The menu of the tile, in the strip's place.
        if let Some(menu) = &self.picker.menu {
            let tile = self.picker.grid().tiles.get(menu.tile).cloned();
            return strip
                .debug_selector(|| "picker-menu".into())
                .flex_wrap()
                .gap(px(2.))
                .children(menu.actions.iter().enumerate().map(|(at, action)| {
                    let hover = palette.elevated_selected;
                    let action = *action;
                    let label = tile.as_ref().map(|tile| action.label(tile)).unwrap_or("");
                    let tile_index = menu.tile;
                    div()
                        .id(("picker-action", at))
                        .debug_selector(move || format!("picker-action-{at}"))
                        .h(px(28.))
                        .px(px(10.))
                        .rounded(metrics::RADIUS())
                        .border_2()
                        .border_color(if at == menu.cursor {
                            palette.focus_ring
                        } else {
                            gpui_kit::transparent_black()
                        })
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .text_size(metrics::TEXT_SMALL())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.picker_do(action, tile_index, window, cx);
                        }))
                        .child(label)
                }));
        }
        let strip = strip.debug_selector(|| "picker-info".into());
        let import = if self.picker.tab == Tab::Stickers {
            self.strip_button(
                "picker-import",
                IconName::Plus,
                with_keys("Make a sticker from a picture…", Command::ImportSticker),
                palette,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.import_stickers(cx);
            }))
        } else {
            self.strip_button(
                "picker-import",
                IconName::Plus,
                with_keys("Add GIFs from files…", Command::ImportGif),
                palette,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.import_gifs(cx);
            }))
        };
        // Something to say first.
        let said: Option<(SharedString, gpui_kit::Hsla)> = if let Some(notice) = &self.picker.notice
        {
            Some((notice.clone(), palette.text))
        } else {
            self.picker_cannot_send()
                .map(|why| (SharedString::from(why), palette.warning))
        };
        if let Some((text, colour)) = said {
            return strip
                .child(words(text, colour).debug_selector(|| "picker-said".into()))
                .child(import);
        }
        let Some(tile) = self.picker.current().cloned() else {
            // What is said once in the grid is not said again here.
            let text = match self.picker.tab {
                Tab::Gifs if self.picker.source == Source::Saved => texts::SAVED_GIFS_HERE,
                _ => "",
            };
            return strip
                .child(words(text.into(), palette.text_muted))
                .child(import);
        };
        let star = tile.item().is_some_and(|item| item.favorite);
        let name = {
            let name = tile.name();
            if name.is_empty() {
                match &tile.kind {
                    TileKind::Sticker(_) => "Sticker".to_owned(),
                    TileKind::Gif(_) => "GIF".to_owned(),
                }
            } else {
                name
            }
        };
        let detail = match &tile.kind {
            TileKind::Sticker(item) | TileKind::Gif(GifEntry::Saved(item)) => {
                if item.pending() {
                    return strip
                        .child(words("Not downloaded yet.".into(), palette.text_muted))
                        .child(import);
                }
                let kind = if item.mime == "video/mp4" {
                    "MP4"
                } else if item.animated {
                    "moving"
                } else {
                    "still"
                };
                format!("{kind}, {} KB", item.bytes.div_ceil(1024))
            }
            TileKind::Gif(GifEntry::Online(hit)) => {
                format!("online, {} KB", hit.media_bytes.unwrap_or(0).div_ceil(1024))
            }
        };
        let hint = keys::keys_label(Command::FavoriteSticker);
        strip
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .debug_selector(|| "picker-name".into())
                            .truncate()
                            .text_size(metrics::TEXT_SMALL())
                            .font_weight(FontWeight::MEDIUM)
                            .child(SharedString::from(name)),
                    )
                    .child(
                        mono(detail)
                            .truncate()
                            .text_size(px(10.))
                            .text_color(palette.text_muted),
                    ),
            )
            .when(matches!(tile.kind, TileKind::Sticker(_)), |this| {
                let tip: SharedString = match (&hint, star) {
                    (Some(keys), true) => format!("Take off favorites ({keys})").into(),
                    (Some(keys), false) => format!("Add to favorites ({keys})").into(),
                    (None, true) => "Take off favorites".into(),
                    (None, false) => "Add to favorites".into(),
                };
                let at = self.picker.grid().cursor;
                this.child(
                    self.strip_button("picker-favorite", IconName::Star, tip, palette)
                        .when(star, |button| button.bg(palette.elevated_selected))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.picker_do(Action::Favorite, at, window, cx);
                        })),
                )
            })
            .child(import)
    }

    /// The Stickers and GIFs tabs under the search field: the bar, the
    /// grid and the strip.
    pub(super) fn render_tile_body(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(self.render_tile_bar(palette, cx))
            .child(
                div()
                    .debug_selector(|| "picker-grid".into())
                    .child(self.render_tile_grid(palette, cx)),
            )
            .child(
                div()
                    .debug_selector(|| "picker-strip".into())
                    .child(self.render_tile_strip(palette, cx)),
            )
    }
}

/// A tooltip: what a control does, and its shortcut when the registry has
/// one (else that it is in the command palette).
pub(super) fn with_keys(label: &str, command: Command) -> String {
    match keys::keys_label(command) {
        Some(keys) => format!("{label} ({keys})"),
        None => format!("{label} (also in the command palette)"),
    }
}

/// What a section's button in the bar says when pointed at.
pub(super) fn section_tip(title: &str, tiles: usize) -> SharedString {
    match tiles {
        0 => title.to_owned().into(),
        1 => format!("{title}: 1 sticker").into(),
        n => format!("{title}: {n} stickers").into(),
    }
}

/// How many tiles a section holds.
fn grid_count(grid: &super::picker::Grid, section: usize) -> usize {
    let start = grid.sections[section].tile;
    let end = grid
        .sections
        .get(section + 1)
        .map_or(grid.tiles.len(), |next| next.tile);
    end - start
}

/// The rows built this frame, with `row` among them.
fn widen(built: std::ops::Range<usize>, row: usize) -> std::ops::Range<usize> {
    if built.start >= built.end {
        row..row + 1
    } else {
        built.start.min(row)..built.end.max(row + 1)
    }
}
