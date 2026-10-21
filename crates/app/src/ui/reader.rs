//! The PDF reader: a PDF somebody sent, read without leaving the window.
//!
//! Its pages stand one under the other at the width of the window (never
//! wider than a page reads well at), and are scrolled like any document.
//! A page is drawn when it comes near what is on screen and let go when
//! it is far from it, so a file of five hundred pages costs what five of
//! them do. Drawing happens off the interface's thread; until a page is
//! there its paper is, at the size it will have, so nothing moves.
//!
//! What the file is and how its pixels are made is `crate::pdf`. What is
//! done with the file (save it, open it with another application) is what
//! is done with any file of a message (`media_out`).

use super::shell::{Overlay, Shell};
use super::transfer::{transfer_button, Transfer};
use super::widgets::{icon_button, mono, text_button};
use crate::icons::IconName;
use crate::pdf::{self, Document};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_provider::Media;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, img, point, Context, Div, FontWeight, KeyDownEvent, ObjectFit, Pixels, RenderImage,
    ScrollHandle, SharedString, StyledImage, Window,
};
use std::collections::HashMap;
use std::sync::Arc;

/// The widest a page is shown when it fits the window: past this a line
/// of text is too long to read.
const FIT_WIDTH_AT_100: f32 = 920.;
/// The room left at each side of the pages, above them (under the
/// toolbar) and below.
const SIDE_AT_100: f32 = 32.;
const TOP_AT_100: f32 = 64.;
const BOTTOM_AT_100: f32 = 32.;
/// Between two pages.
const GAP_AT_100: f32 = 12.;
/// The scales "+" and "-" stop at, against the fitted width.
const STEPS: [f32; 9] = [0.5, 0.67, 0.8, 1., 1.25, 1.5, 2., 3., 4.];
/// How many pages before and after the ones on screen are kept drawn.
const AHEAD: usize = 1;
/// What an arrow scrolls by.
const LINE_AT_100: f32 = 56.;

/// The measures above at the interface's size, in the window's points.
fn fit_width() -> f32 {
    px(FIT_WIDTH_AT_100).as_f32()
}
fn side() -> f32 {
    px(SIDE_AT_100).as_f32()
}
fn top() -> f32 {
    px(TOP_AT_100).as_f32()
}
fn bottom() -> f32 {
    px(BOTTOM_AT_100).as_f32()
}
fn gap() -> f32 {
    px(GAP_AT_100).as_f32()
}
fn line() -> f32 {
    px(LINE_AT_100).as_f32()
}

/// The file, as far as it got.
#[derive(Default)]
enum Opened {
    /// Not here yet, or being read.
    #[default]
    Waiting,
    /// Read: its pages can be drawn.
    Here(Document),
    /// Not a PDF that can be shown here, and why.
    Failed(SharedString),
}

/// A page that was drawn.
struct Page {
    image: Arc<RenderImage>,
    /// The width it was drawn for, in device pixels.
    asked: u32,
}

/// The reader's state.
pub struct Reader {
    /// The file being read.
    pub media: Option<Media>,
    opened: Opened,
    /// Counts the files read, so that what comes back for one that was
    /// closed is told apart.
    turn: u64,
    /// The file is being opened.
    opening: bool,
    /// The scale against the fitted width.
    zoom: f32,
    /// The pages drawn so far.
    pages: HashMap<usize, Page>,
    /// The pages being drawn, with the width asked for.
    drawing: HashMap<usize, u32>,
    /// Where the column is scrolled to.
    scroll: ScrollHandle,
    /// Pages let go, which the window still holds.
    stale: Vec<Arc<RenderImage>>,
}

impl Default for Reader {
    fn default() -> Self {
        Self {
            media: None,
            opened: Opened::Waiting,
            turn: 0,
            opening: false,
            zoom: 1.,
            pages: HashMap::new(),
            drawing: HashMap::new(),
            scroll: ScrollHandle::new(),
            stale: Vec::new(),
        }
    }
}

impl Reader {
    /// Nothing is read any more: the pages are let go.
    fn reset(&mut self) {
        self.media = None;
        self.opened = Opened::Waiting;
        self.turn += 1;
        self.opening = false;
        self.zoom = 1.;
        self.drawing.clear();
        self.stale
            .extend(self.pages.drain().map(|(_, page)| page.image));
        self.scroll
            .set_offset(point(gpui_kit::px(0.), gpui_kit::px(0.)));
    }

    /// Frees what the window holds of pages that were let go.
    pub fn release(&mut self, window: &mut Window) {
        for image in self.stale.drain(..) {
            let _ = window.drop_image(image);
        }
    }

    /// How many pages the file has, once it was read (for the tests).
    #[cfg(test)]
    pub fn page_count(&self) -> Option<usize> {
        match &self.opened {
            Opened::Here(document) => Some(document.pages.len()),
            _ => None,
        }
    }

    /// How many pages are drawn or being drawn (for the tests).
    #[cfg(test)]
    pub fn pages_drawn(&self) -> Vec<usize> {
        let mut drawn: Vec<usize> = self.pages.keys().copied().collect();
        drawn.sort_unstable();
        drawn
    }
}

/// Where the pages are, for one size of the window.
struct Sheet {
    /// The width every page is shown at.
    width: f32,
    /// Each page's top and height, from the top of the first.
    placed: Vec<(f32, f32)>,
    /// How far down the column is scrolled, from the top of the first
    /// page (negative above it, in the room under the toolbar).
    from: f32,
    /// The height of the window.
    room: f32,
}

impl Sheet {
    /// The page being read.
    fn current(&self) -> usize {
        pdf::current(&self.placed, self.from, self.room)
    }
}

/// The next stop from `zoom`, closer or further.
fn step(zoom: f32, closer: bool) -> f32 {
    let next = if closer {
        STEPS.iter().copied().find(|stop| *stop > zoom * 1.005)
    } else {
        STEPS
            .iter()
            .rev()
            .copied()
            .find(|stop| *stop < zoom * 0.995)
    };
    next.unwrap_or(zoom)
}

impl Shell {
    /// Whether a file is read here rather than handed to the system.
    pub(super) fn reads_here(media: &Media) -> bool {
        media.kind == client_provider::MediaKind::Document
            && pdf::is_pdf(media.mime_type.as_deref(), media.file_name.as_deref())
    }

    /// Opens a file of a message: a PDF in the reader, a video in the
    /// player, anything else with the system's application for it.
    pub(super) fn open_document(
        &mut self,
        media: Media,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if Self::reads_here(&media) {
            self.begin_reading(media, window, cx);
        } else if media.kind == client_provider::MediaKind::Video {
            self.watch_video(media, window, cx);
        } else {
            self.open_file(media, cx);
        }
    }

    /// Opens the reader on a PDF, fetching it first if need be.
    pub(super) fn begin_reading(
        &mut self,
        media: Media,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(account), Some(source)) = (self.account.clone(), media.source.clone()) else {
            return;
        };
        self.engine.want_file(&account, source.as_str());
        self.reader.reset();
        self.reader.media = Some(media);
        self.open_overlay(Overlay::Reader, window, cx);
        self.load_read(cx);
    }

    /// The reader was closed: its pages are let go.
    pub(super) fn end_reading(&mut self) {
        self.reader.reset();
    }

    /// Reads the file in the reader once it is here, off the interface's
    /// thread.
    pub(super) fn load_read(&mut self, cx: &mut Context<Self>) {
        if self.reader.opening || !matches!(self.reader.opened, Opened::Waiting) {
            return;
        }
        let Some(url) = self
            .reader
            .media
            .as_ref()
            .and_then(|media| media.source.as_ref())
            .map(|source| source.as_str().to_owned())
        else {
            return;
        };
        let Some((bytes, _)) = self.media.file(&url) else {
            return;
        };
        self.reader.opening = true;
        let turn = self.reader.turn;
        cx.spawn(async move |this, cx| {
            let opened = cx.background_spawn(async move { pdf::open(bytes) }).await;
            this.update(cx, |this, cx| {
                if this.reader.turn != turn {
                    // Another file is being read by now.
                    return;
                }
                this.reader.opening = false;
                this.reader.opened = match opened {
                    Ok(document) => Opened::Here(document),
                    Err(why) => Opened::Failed(why.into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Where the pages are in a window of this size.
    fn reader_sheet(&self, viewport: gpui_kit::Size<Pixels>) -> Option<Sheet> {
        let Opened::Here(document) = &self.reader.opened else {
            return None;
        };
        let fit = (viewport.width.as_f32() - side() * 2.).clamp(120., fit_width());
        let width = (fit * self.reader.zoom).round();
        Some(Sheet {
            width,
            placed: pdf::layout(&document.pages, width, gap()),
            from: -self.reader.scroll.offset().y.as_f32() - top(),
            room: viewport.height.as_f32(),
        })
    }

    /// Draws the pages that are on screen or about to be, and lets go of
    /// the ones far from it. Called for every frame the reader is in.
    pub(super) fn read_ahead(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay != Overlay::Reader {
            return;
        }
        let Some(sheet) = self.reader_sheet(window.viewport_size()) else {
            return;
        };
        let Opened::Here(document) = &self.reader.opened else {
            return;
        };
        let Some((first, last)) = pdf::visible(&sheet.placed, sheet.from, sheet.room) else {
            return;
        };
        let count = sheet.placed.len();
        let (first, last) = (first.saturating_sub(AHEAD), (last + AHEAD).min(count - 1));
        // Pages far from the screen are let go.
        let far: Vec<usize> = self
            .reader
            .pages
            .keys()
            .copied()
            .filter(|page| *page + AHEAD < first || *page > last + AHEAD)
            .collect();
        for page in far {
            if let Some(gone) = self.reader.pages.remove(&page) {
                self.reader.stale.push(gone.image);
            }
        }
        let wanted = (sheet.width * window.scale_factor()).round().max(1.) as u32;
        for index in first..=last {
            let (asked, _) = pdf::drawn_size(document.pages[index], wanted);
            // Drawn again only when it would be clearly sharper, or is
            // far larger than what is shown.
            let good = |had: u32| had * 8 >= asked * 7 && had <= asked * 2;
            if self
                .reader
                .pages
                .get(&index)
                .is_some_and(|page| good(page.asked))
                || self
                    .reader
                    .drawing
                    .get(&index)
                    .is_some_and(|had| good(*had))
            {
                continue;
            }
            self.reader.drawing.insert(index, asked);
            let (document, turn) = (document.clone(), self.reader.turn);
            cx.spawn(async move |this, cx| {
                let drawn = cx
                    .background_spawn(async move { document.draw(index, asked) })
                    .await;
                let image = drawn.and_then(|drawn| {
                    let pixels = image::RgbaImage::from_raw(drawn.width, drawn.height, drawn.bgra)?;
                    Some(Arc::new(RenderImage::new(vec![image::Frame::new(pixels)])))
                });
                this.update(cx, |this, cx| {
                    let Some(image) = image else {
                        if this.reader.turn == turn {
                            this.reader.drawing.remove(&index);
                        }
                        return;
                    };
                    // For a file that was closed, or a width nobody
                    // waits for any more: let go.
                    if this.reader.turn != turn || this.reader.drawing.get(&index) != Some(&asked) {
                        this.reader.stale.push(image);
                        return cx.notify();
                    }
                    this.reader.drawing.remove(&index);
                    if let Some(old) = this.reader.pages.insert(index, Page { image, asked }) {
                        this.reader.stale.push(old.image);
                    }
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Scrolls the column to `from` (measured from the top of the room
    /// above the first page), within what there is.
    fn scroll_reader_to(&mut self, from: f32, cx: &mut Context<Self>) {
        let most = self.reader.scroll.max_offset().y.as_f32().max(0.);
        let across = self.reader.scroll.offset().x;
        self.reader
            .scroll
            .set_offset(point(across, gpui_kit::px(-from.clamp(0., most))));
        cx.notify();
    }

    fn scroll_reader_by(&mut self, by: f32, cx: &mut Context<Self>) {
        let from = -self.reader.scroll.offset().y.as_f32();
        self.scroll_reader_to(from + by, cx);
    }

    /// Goes to the top of a page.
    pub(super) fn read_page(&mut self, page: usize, window: &Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.reader_sheet(window.viewport_size()) else {
            return;
        };
        if let Some((top, _)) = sheet.placed.get(page) {
            // The page's top just under the toolbar.
            self.scroll_reader_to(*top, cx);
        }
    }

    /// The page after (or before) the one being read.
    pub(super) fn step_page(
        &mut self,
        forward: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(sheet) = self.reader_sheet(window.viewport_size()) else {
            return false;
        };
        let at = sheet.current();
        let next = if forward {
            (at + 1).min(sheet.placed.len() - 1)
        } else {
            // From the middle of a page, "previous" is its own top.
            let top = sheet.placed[at].0;
            if sheet.from > top + 24. {
                at
            } else {
                at.saturating_sub(1)
            }
        };
        self.read_page(next, window, cx);
        true
    }

    /// A step closer or further; `None` fits the window again. The page
    /// being read stays the one being read.
    pub(super) fn zoom_reader(
        &mut self,
        closer: Option<bool>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(before) = self.reader_sheet(window.viewport_size()) else {
            return;
        };
        let to = match closer {
            Some(closer) => step(self.reader.zoom, closer),
            None => 1.,
        };
        if (to - self.reader.zoom).abs() < 0.001 {
            return;
        }
        // The same place of the same page under the top of the window.
        let page =
            pdf::visible(&before.placed, before.from.max(0.), 1.).map_or(0, |(first, _)| first);
        let (was_top, was_height) = before.placed[page];
        let inside = ((before.from - was_top) / was_height).clamp(0., 1.);
        self.reader.zoom = to;
        if let Some(after) = self.reader_sheet(window.viewport_size()) {
            let (page_top, height) = after.placed[page];
            let from = page_top + inside * height + top();
            // The column's new height is not laid out yet: the offset is
            // set as it is, and the next frame keeps it within bounds.
            let across = self.reader.scroll.offset().x;
            self.reader
                .scroll
                .set_offset(point(across, gpui_kit::px(-from.max(0.))));
        }
        cx.notify();
    }

    /// The reader's keys. The ones that act on a message's file act on
    /// the PDF; the arrows, Space and the page keys scroll.
    pub(super) fn reader_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let context = crate::keys::Context {
            chat: true,
            viewer: true,
            ..Default::default()
        };
        use crate::keys::Command as C;
        let page = (window.viewport_size().height.as_f32() - top() - bottom()).max(line());
        let stroke = &event.keystroke;
        match crate::keys::resolve(stroke, context) {
            Some(C::ViewerNext) => return self.step_page(true, window, cx),
            Some(C::ViewerPrevious) => return self.step_page(false, window, cx),
            Some(C::ZoomIn) => self.zoom_reader(Some(true), window, cx),
            Some(C::ZoomOut) => self.zoom_reader(Some(false), window, cx),
            Some(C::ZoomFit | C::ZoomActual) => self.zoom_reader(None, window, cx),
            Some(C::SaveAs) => return self.save_as(None, cx),
            Some(C::SaveToDownloads) => return self.save_to_downloads(None, cx),
            Some(C::Open) => self.open_read(window, cx),
            _ if stroke.modifiers.modified() && !stroke.modifiers.shift => return false,
            _ => match stroke.key.as_str() {
                "down" => self.scroll_reader_by(line(), cx),
                "up" => self.scroll_reader_by(-line(), cx),
                "space" if stroke.modifiers.shift => self.scroll_reader_by(-page, cx),
                "pagedown" | "space" => self.scroll_reader_by(page, cx),
                "pageup" => self.scroll_reader_by(-page, cx),
                "home" => self.scroll_reader_to(0., cx),
                "end" => self.scroll_reader_to(f32::MAX, cx),
                _ => return false,
            },
        }
        true
    }

    /// Hands the file to the system's application for it, and closes the
    /// reader: it is read there now.
    pub(super) fn open_read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(media) = self.reader.media.clone() {
            self.close_overlay(window, cx);
            self.open_file(media, cx);
        }
    }

    /// The name the file is shown by.
    fn reader_title(&self) -> SharedString {
        self.reader
            .media
            .as_ref()
            .and_then(|media| media.file_name.clone())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or_else(|| "PDF".to_owned())
            .into()
    }

    /// A PDF at the size of the window. A click beside its pages, or
    /// Escape, closes it.
    pub(super) fn render_reader(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let media = self.reader.media.as_ref()?;
        let account = self.account.as_ref()?;
        let url = media.source.as_ref()?.as_str();
        let viewport = window.viewport_size();
        let sheet = self.reader_sheet(viewport);

        let tool = |id: &'static str,
                    glyph: IconName,
                    command: Option<crate::keys::Command>,
                    hint: &'static str| {
            let hint: SharedString = match command.and_then(crate::keys::keys_label) {
                Some(keys) => format!("{hint} ({keys})").into(),
                None => hint.into(),
            };
            icon_button(id, glyph, palette).tooltip(move |window, cx| {
                gpui_kit::component::tooltip::Tooltip::new(hint.clone()).build(window, cx)
            })
        };
        let rule = || {
            div()
                .flex_none()
                .mx_1()
                .w(px(1.))
                .h(px(16.))
                .bg(palette.elevated_border)
        };
        use crate::keys::Command as C;
        let mut tools = div()
            .id("reader-tools")
            .on_click(|_, _, cx| cx.stop_propagation())
            .max_w(viewport.width - px(24.))
            .p(px(4.))
            .rounded(metrics::PANEL_RADIUS())
            .border_1()
            .border_color(palette.elevated_border)
            .bg(palette.elevated)
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                div()
                    .debug_selector(|| "reader-title".into())
                    .min_w_0()
                    .max_w(px(260.))
                    .px_2()
                    .truncate()
                    .text_size(metrics::TEXT_SMALL())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(palette.text)
                    .child(self.reader_title()),
            );
        if let Some(sheet) = &sheet {
            let (at, count) = (sheet.current(), sheet.placed.len());
            tools = tools
                .child(rule())
                .child(
                    tool(
                        "reader-previous",
                        IconName::ChevronLeft,
                        Some(C::ViewerPrevious),
                        crate::i18n::t("chat.reader.previous"),
                    )
                    .when(at == 0 && sheet.from <= 0., |this| this.opacity(0.35))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.step_page(false, window, cx);
                    })),
                )
                .child(
                    mono(format!("{} / {count}", at + 1))
                        .debug_selector(|| "reader-page".into())
                        .min_w(px(52.))
                        .text_center()
                        .text_color(palette.text_muted),
                )
                .child(
                    tool(
                        "reader-next",
                        IconName::ChevronRight,
                        Some(C::ViewerNext),
                        crate::i18n::t("chat.reader.next"),
                    )
                    .when(at + 1 >= count, |this| this.opacity(0.35))
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.step_page(true, window, cx);
                    })),
                )
                .child(rule())
                .child(
                    tool(
                        "reader-zoom-out",
                        IconName::ZoomOut,
                        Some(C::ZoomOut),
                        crate::i18n::t("chat.reader.zoom_out"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.zoom_reader(Some(false), window, cx);
                    })),
                )
                .child(
                    mono(format!("{:.0}%", self.reader.zoom * 100.))
                        .debug_selector(|| "reader-zoom".into())
                        .w(px(44.))
                        .text_center()
                        .text_color(palette.text_muted),
                )
                .child(
                    tool(
                        "reader-zoom-in",
                        IconName::ZoomIn,
                        Some(C::ZoomIn),
                        crate::i18n::t("chat.reader.zoom_in"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.zoom_reader(Some(true), window, cx);
                    })),
                )
                .child(
                    tool(
                        "reader-fit",
                        IconName::Maximize,
                        Some(C::ZoomFit),
                        crate::i18n::t("chat.reader.fit"),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.zoom_reader(None, window, cx);
                    })),
                );
        }
        tools = tools
            .child(rule())
            .child(
                tool(
                    "reader-save",
                    IconName::ArrowDownToLine,
                    Some(C::SaveAs),
                    crate::i18n::t("chat.reader.save"),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.save_as(None, cx);
                })),
            )
            .child(
                tool(
                    "reader-open",
                    IconName::ExternalLink,
                    Some(C::Open),
                    crate::i18n::t("chat.reader.open_other"),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_read(window, cx);
                })),
            )
            .child(
                tool(
                    "reader-close",
                    IconName::X,
                    None,
                    crate::i18n::t("chat.reader.close"),
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_overlay(window, cx);
                })),
            );
        let toolbar = div()
            .debug_selector(|| "reader-toolbar".into())
            .absolute()
            .top(px(12.))
            .left_0()
            .w_full()
            .flex()
            .justify_center()
            .child(tools);

        let body = match (&self.reader.opened, sheet) {
            (Opened::Here(_), Some(sheet)) => {
                let wide = sheet.width + side() * 2.;
                let mut column = div()
                    .flex_none()
                    .w(gpui_kit::px(wide.max(viewport.width.as_f32())))
                    .pt(gpui_kit::px(top()))
                    .pb(gpui_kit::px(bottom()))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(gpui_kit::px(gap()));
                for (index, (_, height)) in sheet.placed.iter().enumerate() {
                    column = column.child(
                        div()
                            .id(("reader-page", index))
                            .debug_selector(move || format!("reader-page-{index}"))
                            .flex_none()
                            .w(gpui_kit::px(sheet.width))
                            .h(gpui_kit::px(*height))
                            .rounded(px(2.))
                            .overflow_hidden()
                            // The paper, before what is on it.
                            .bg(gpui_kit::white())
                            // A click on a page is not a click beside it.
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .children(self.reader.pages.get(&index).map(|page| {
                                img(page.image.clone())
                                    .size_full()
                                    .object_fit(ObjectFit::Fill)
                            })),
                    );
                }
                div()
                    .id("reader-pages")
                    .debug_selector(|| "reader-pages".into())
                    .size_full()
                    .overflow_scroll()
                    .track_scroll(&self.reader.scroll)
                    .child(column)
            }
            (Opened::Failed(why), _) => div().id("reader-pages").size_full().child(
                self.reader_word(why.clone(), palette).child(
                    text_button(
                        "reader-open-instead",
                        crate::i18n::t("chat.reader.open_other"),
                        Some(IconName::ExternalLink),
                        false,
                        palette,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_read(window, cx);
                    })),
                ),
            ),
            _ => {
                // On its way, or it did not come: the same button as on
                // a picture says so.
                let state = match self.media.file_state(url) {
                    super::media::FileState::Unavailable(_) if self.media.file_expired(url) => None,
                    super::media::FileState::Unavailable(reason) => Some(Transfer::Retry(reason)),
                    super::media::FileState::Ready => None,
                    _ => Some(Transfer::Loading {
                        fraction: self.media.file_fraction(url, media),
                    }),
                };
                let words = match &state {
                    Some(Transfer::Retry(_)) => crate::i18n::t("chat.reader.download_failed"),
                    Some(_) => crate::i18n::t("chat.reader.downloading"),
                    None if self.media.file_expired(url) => crate::i18n::t("chat.reader.expired"),
                    None => crate::i18n::t("chat.reader.opening"),
                };
                let button = state.map(|state| {
                    let (shelf, account, url) =
                        (self.media.clone(), account.clone(), url.to_owned());
                    let stop = matches!(state, Transfer::Loading { .. });
                    transfer_button(
                        "reader-transfer",
                        &state,
                        palette,
                        crate::settings::reduce_motion(cx),
                        move |_, _, cx| {
                            cx.stop_propagation();
                            if stop {
                                shelf.cancel_file(&url);
                            } else {
                                shelf.retry_file(&account, &url);
                            }
                        },
                    )
                });
                div()
                    .id("reader-pages")
                    .size_full()
                    .child(self.reader_word(words.into(), palette).children(button))
            }
        };
        Some(
            div()
                .debug_selector(|| "reader".into())
                .relative()
                .size_full()
                .overflow_hidden()
                .text_color(palette.text)
                .child(body)
                .child(toolbar)
                .children(self.render_overlay_word(palette)),
        )
    }

    /// A few words in the middle of the reader, with room under them for
    /// what can be done.
    fn reader_word(&self, words: SharedString, palette: &Palette) -> Div {
        div()
            .debug_selector(|| "reader-word".into())
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .child(
                div()
                    .text_size(metrics::TEXT_BODY())
                    .text_color(palette.text_muted)
                    .child(words),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_zoom_stops_at_its_steps() {
        assert_eq!(step(1., true), 1.25);
        assert_eq!(step(1., false), 0.8);
        assert_eq!(step(4., true), 4.);
        assert_eq!(step(0.5, false), 0.5);
        let mut zoom = 0.5;
        for _ in 0..20 {
            zoom = step(zoom, true);
        }
        assert_eq!(zoom, 4.);
    }
}
