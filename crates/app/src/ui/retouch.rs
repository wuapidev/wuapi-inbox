//! Retouching a picture in the attach sheet before it is sent: a quarter
//! turn, a crop, a line drawn by hand, a rectangle, a part hidden, words.
//!
//! The picture on show is the file with the edits made so far. Each edit
//! is made with the pointer on the picture (or a click on "Turn"), joins
//! the file's list, and the file is made again from the picture as it
//! came, off the interface's thread: what the strip, the size in the
//! footer and "Send" have is always the retouched file. "Undo" takes the
//! last edit back; with none left the file is the one that came, byte
//! for byte.
//!
//! What an edit is and how it is made on the pixels is `crate::retouch`.
//! Here: which tool is in hand, the drag under way (drawn over the
//! picture until it is let go), and where the picture is on the screen.

use super::attach::AttachItem;
use super::shell::Shell;
use super::widgets::{icon_button, mono};
use crate::attach::Attachment;
use crate::icons::IconName;
use crate::retouch::{self, Area, Edit, Ink};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use gpui_kit::component::input::InputState;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{
    canvas, div, img, point, rgb, size, Bounds, Context, Div, Entity, MouseButton, ObjectFit,
    PathBuilder, Pixels, Point, RenderImage, StyledImage, Window,
};
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// The longest side of the picture the window holds for the sheet: the
/// file keeps its own pixels, the screen needs no more than this.
const SHOWN_SIDE: u32 = 2048;

/// What the pointer does on the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(in crate::ui) enum Tool {
    Crop,
    Pen,
    Words,
    Frame,
    Hide,
}

impl Tool {
    /// What to do with it, said under the tools.
    fn hint(self) -> &'static str {
        match self {
            Tool::Crop => "Drag over the part to keep.",
            Tool::Pen => "Draw on the picture.",
            Tool::Words => "Write, then click where the words go.",
            Tool::Frame => "Drag a rectangle around something.",
            Tool::Hide => "Drag over what should not be read.",
        }
    }

    /// Whether it draws in a colour.
    fn inked(self) -> bool {
        matches!(self, Tool::Pen | Tool::Words | Tool::Frame)
    }
}

/// What a file's retouch holds.
#[derive(Default)]
pub(in crate::ui) struct Retouched {
    /// What stands for this file among the pictures the window holds.
    id: u64,
    /// The edits made, in order.
    pub(in crate::ui) edits: Vec<Edit>,
    /// The file as it came, once an edit was made.
    original: Option<Attachment>,
    /// Its pixels, decoded once.
    decoded: Option<Arc<image::RgbaImage>>,
    /// The size of the picture with the edits made.
    natural: Option<(u32, u32)>,
    /// Counts what was asked for, so that an answer to an earlier ask is
    /// told apart.
    turn: u64,
    /// The file is being made again.
    pub(in crate::ui) busy: bool,
    /// It could not be read as a picture: it goes as it is.
    failed: bool,
}

/// The retouch's part of the window.
pub(in crate::ui) struct Retouching {
    /// The tool in hand.
    pub(in crate::ui) tool: Option<Tool>,
    /// The colour drawn with.
    ink: Ink,
    /// The points of the drag under way, in the window.
    drag: Option<Vec<Point<Pixels>>>,
    /// Where the picture's room is in the window, as last drawn.
    room: Rc<Cell<Bounds<Pixels>>>,
    /// The words to write.
    pub(in crate::ui) words: Option<Entity<InputState>>,
    /// The pictures the window holds, by file.
    shown: HashMap<u64, Arc<RenderImage>>,
    /// Pictures let go, which the window still holds.
    stale: Vec<Arc<RenderImage>>,
    /// The next file's id.
    next: u64,
}

impl Default for Retouching {
    fn default() -> Self {
        Self {
            tool: None,
            ink: retouch::INKS[0],
            drag: None,
            room: Rc::default(),
            words: None,
            shown: HashMap::new(),
            stale: Vec::new(),
            next: 1,
        }
    }
}

/// What making the file again gives.
struct Made {
    decoded: Arc<image::RgbaImage>,
    natural: (u32, u32),
    shown: Arc<RenderImage>,
    /// The file to send, when there are edits.
    file: Option<(Vec<u8>, &'static str)>,
}

/// Decodes (when it was not), makes the edits, and encodes. Blocking.
fn make(
    decoded: Option<Arc<image::RgbaImage>>,
    original: &Attachment,
    edits: &[Edit],
) -> Result<Made, String> {
    let decoded = match decoded {
        Some(decoded) => decoded,
        None => Arc::new(retouch::open(&original.bytes)?),
    };
    let edited = retouch::apply_all((*decoded).clone(), edits);
    let natural = edited.dimensions();
    let file = if edits.is_empty() {
        None
    } else {
        Some(retouch::encode(&edited, &original.mime)?)
    };
    // For the screen: no larger than it needs, in the renderer's order.
    let mut small = if natural.0.max(natural.1) > SHOWN_SIDE {
        image::imageops::thumbnail(
            &edited,
            (natural.0 as u64 * SHOWN_SIDE as u64 / natural.0.max(natural.1) as u64).max(1) as u32,
            (natural.1 as u64 * SHOWN_SIDE as u64 / natural.0.max(natural.1) as u64).max(1) as u32,
        )
    } else {
        edited
    };
    let raw: &mut [u8] = &mut small;
    for red in (0..raw.len().saturating_sub(3)).step_by(4) {
        raw.swap(red, red + 2);
    }
    Ok(Made {
        decoded,
        natural,
        shown: Arc::new(RenderImage::new(vec![image::Frame::new(small)])),
        file,
    })
}

/// Where a picture of `natural` size is drawn in `room`: whole, centred,
/// never larger than its own pixels.
pub(in crate::ui) fn placed(natural: (u32, u32), room: Bounds<Pixels>) -> Bounds<Pixels> {
    let (wide, tall) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    let (room_wide, room_tall) = (room.size.width.as_f32(), room.size.height.as_f32());
    let scale = (room_wide / wide).min(room_tall / tall).clamp(0., 1.);
    let shown = size(gpui_kit::px(wide * scale), gpui_kit::px(tall * scale));
    Bounds {
        origin: point(
            room.origin.x + (room.size.width - shown.width) / 2.,
            room.origin.y + (room.size.height - shown.height) / 2.,
        ),
        size: shown,
    }
}

/// A point of the window as a point of the picture drawn in `rect`.
fn in_picture(at: Point<Pixels>, rect: Bounds<Pixels>, natural: (u32, u32)) -> (f32, f32) {
    let scale = natural.0 as f32 / rect.size.width.as_f32().max(0.001);
    (
        (at.x - rect.origin.x).as_f32() * scale,
        (at.y - rect.origin.y).as_f32() * scale,
    )
}

impl Retouching {
    /// Frees what the window holds of pictures that were let go, and of
    /// files that are no longer in the sheet.
    pub(in crate::ui) fn release(&mut self, alive: &[u64], window: &mut Window) {
        let gone: Vec<u64> = self
            .shown
            .keys()
            .copied()
            .filter(|id| !alive.contains(id))
            .collect();
        for id in gone {
            self.stale.extend(self.shown.remove(&id));
        }
        for image in self.stale.drain(..) {
            let _ = window.drop_image(image);
        }
    }
}

impl AttachItem {
    /// Whether this file is a picture that can be retouched: a still
    /// one, that goes as a picture.
    pub(in crate::ui) fn retouchable(&self) -> bool {
        self.blocked.is_none()
            && !self.as_document
            && !self.retouch.failed
            && matches!(
                self.retouch
                    .original
                    .as_ref()
                    .map_or(self.file.mime.as_str(), |original| original.mime.as_str()),
                "image/jpeg" | "image/png" | "image/webp"
            )
    }
}

impl Shell {
    /// The files of the sheet that the window holds a picture for.
    pub(in crate::ui) fn retouch_alive(&self) -> Vec<u64> {
        self.attach.as_ref().map_or_else(Vec::new, |draft| {
            draft.items.iter().map(|item| item.retouch.id).collect()
        })
    }

    /// Whether a file is still being made again: nothing is sent
    /// meanwhile, or it would go without its last edit.
    pub(in crate::ui) fn retouch_busy(&self) -> bool {
        self.attach
            .as_ref()
            .is_some_and(|draft| draft.items.iter().any(|item| item.retouch.busy))
    }

    /// Reads the picture on show for the retouch, once, and makes the
    /// field the words are written in. Called for every frame of the
    /// sheet.
    pub(in crate::ui) fn ensure_retouch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.attach.is_none() {
            self.retouching.tool = None;
            self.retouching.drag = None;
            return;
        }
        if self.retouching.words.is_none() {
            self.retouching.words =
                Some(cx.new(|cx| InputState::new(window, cx).placeholder("Words to write")));
        }
        let Some(draft) = self.attach.as_mut() else {
            return;
        };
        let selected = draft.selected;
        let Some(item) = draft.items.get_mut(selected) else {
            return;
        };
        if item.retouch.id == 0 {
            item.retouch.id = self.retouching.next;
            self.retouching.next += 1;
        }
        let wanted = item.retouchable()
            && !item.retouch.busy
            && !self.retouching.shown.contains_key(&item.retouch.id);
        if wanted {
            self.remake(selected, cx);
        }
    }

    /// Makes the file at `index` again from the picture as it came and
    /// its edits, off the interface's thread.
    fn remake(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(item) = self
            .attach
            .as_mut()
            .and_then(|draft| draft.items.get_mut(index))
        else {
            return;
        };
        let retouch = &mut item.retouch;
        let original = retouch
            .original
            .get_or_insert_with(|| item.file.clone())
            .clone();
        retouch.turn += 1;
        retouch.busy = true;
        let (id, turn) = (retouch.id, retouch.turn);
        let (decoded, edits) = (retouch.decoded.clone(), retouch.edits.clone());
        let limit = self.engine.upload_limit();
        cx.spawn(async move |this, cx| {
            let made = cx
                .background_spawn(async move {
                    let made = make(decoded, &original, &edits)?;
                    // The small picture of the strip, made here too.
                    let file = match &made.file {
                        Some((bytes, mime)) => Attachment {
                            name: retouch::renamed(&original.name, mime),
                            mime: (*mime).to_owned(),
                            bytes: Arc::new(bytes.clone()),
                        },
                        None => original,
                    };
                    let preview = super::attach::preview_of(&file);
                    Ok::<_, String>((made, file, preview))
                })
                .await;
            this.update(cx, |this, cx| {
                let found = this.attach.as_mut().and_then(|draft| {
                    draft
                        .items
                        .iter_mut()
                        .find(|item| item.retouch.id == id && item.retouch.turn == turn)
                });
                let Some(item) = found else {
                    // The file left the sheet, or was edited again.
                    if let Ok((made, ..)) = made {
                        this.retouching.stale.push(made.shown);
                    }
                    return;
                };
                item.retouch.busy = false;
                match made {
                    Ok((made, file, preview)) => {
                        item.retouch.decoded = Some(made.decoded);
                        item.retouch.natural = Some(made.natural);
                        item.set_sent_file(file, preview, limit);
                        if let Some(old) = this.retouching.shown.insert(id, made.shown) {
                            this.retouching.stale.push(old);
                        }
                    }
                    Err(why) => {
                        // Not a picture after all: it goes as it came.
                        item.retouch.failed = true;
                        item.retouch.edits.clear();
                        tracing::debug!(%why, "a picture could not be retouched");
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Makes an edit on the file on show.
    fn retouch_with(&mut self, edit: Edit, cx: &mut Context<Self>) {
        let Some(draft) = self.attach.as_mut() else {
            return;
        };
        let selected = draft.selected;
        let Some(item) = draft
            .items
            .get_mut(selected)
            .filter(|item| item.retouchable())
        else {
            return;
        };
        item.retouch.edits.push(edit);
        draft.confirm_discard = false;
        self.remake(selected, cx);
    }

    /// Takes the last edit of the file on show back.
    pub(in crate::ui) fn retouch_undo(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.attach.as_mut() else {
            return;
        };
        let selected = draft.selected;
        if draft
            .items
            .get_mut(selected)
            .and_then(|item| item.retouch.edits.pop())
            .is_some()
        {
            self.remake(selected, cx);
        }
    }

    /// The picture on show: its size with the edits made, and where it is
    /// in the window.
    fn retouch_frame(&self) -> Option<((u32, u32), Bounds<Pixels>)> {
        let item = self.attach.as_ref()?.current()?;
        let natural = item.retouch.natural?;
        self.retouching.shown.get(&item.retouch.id)?;
        Some((natural, placed(natural, self.retouching.room.get())))
    }

    /// Picks a tool, or puts it down when it is the one in hand.
    fn pick_tool(&mut self, tool: Tool, window: &mut Window, cx: &mut Context<Self>) {
        self.retouching.drag = None;
        self.retouching.tool = (self.retouching.tool != Some(tool)).then_some(tool);
        if self.retouching.tool == Some(Tool::Words) {
            if let Some(words) = &self.retouching.words {
                words.update(cx, |words, cx| words.focus(window, cx));
            }
        }
        cx.notify();
    }

    /// The pointer went down on the picture with a tool in hand.
    fn retouch_press(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let (Some(tool), Some((natural, rect))) = (self.retouching.tool, self.retouch_frame())
        else {
            return;
        };
        if tool == Tool::Words {
            if !rect.contains(&at) {
                return;
            }
            let text = self
                .retouching
                .words
                .as_ref()
                .map(|words| words.read(cx).value().trim().to_owned())
                .unwrap_or_default();
            if !text.is_empty() {
                self.retouch_with(
                    Edit::Words {
                        at: in_picture(at, rect, natural),
                        text,
                        size: retouch::letters_for(natural),
                        ink: self.retouching.ink,
                    },
                    cx,
                );
            }
            return;
        }
        // A drag may begin beside the picture (a crop from its very
        // corner): what it covers is kept within the picture.
        self.retouching.drag = Some(vec![at]);
        cx.notify();
    }

    /// The pointer moved with its button down.
    fn retouch_drag(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(points) = self.retouching.drag.as_mut() else {
            return;
        };
        // A point for every couple of pixels is as smooth as a line gets.
        if points
            .last()
            .is_none_or(|last| (at.x - last.x).abs() + (at.y - last.y).abs() >= gpui_kit::px(2.))
        {
            points.push(at);
            cx.notify();
        }
    }

    /// The button was let go: the drag is the edit.
    fn retouch_drop(&mut self, cx: &mut Context<Self>) {
        let Some(points) = self.retouching.drag.take() else {
            return;
        };
        cx.notify();
        let (Some(tool), Some((natural, rect))) = (self.retouching.tool, self.retouch_frame())
        else {
            return;
        };
        let (Some(first), Some(last)) = (points.first(), points.last()) else {
            return;
        };
        let area = Area::between(
            in_picture(*first, rect, natural),
            in_picture(*last, rect, natural),
        );
        let (width, ink) = (retouch::stroke_for(natural), self.retouching.ink);
        let edit = match tool {
            Tool::Pen => Edit::Stroke {
                points: points
                    .iter()
                    .map(|at| in_picture(*at, rect, natural))
                    .collect(),
                width,
                ink,
            },
            Tool::Frame if area.pixels(natural).is_some() => Edit::Frame { area, width, ink },
            Tool::Crop if area.pixels(natural).is_some() => {
                // One crop is one decision: the tool is put down.
                self.retouching.tool = None;
                Edit::Crop(area)
            }
            Tool::Hide if area.pixels(natural).is_some() => Edit::Hide(area),
            _ => return,
        };
        self.retouch_with(edit, cx);
    }

    /// The tools, over the picture on show. `None` for a file that is
    /// not a picture to retouch.
    pub(in crate::ui) fn render_retouch_tools(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let item = self.attach.as_ref()?.current()?;
        if !item.retouchable() {
            return None;
        }
        let ready = self.retouching.shown.contains_key(&item.retouch.id);
        let in_hand = self.retouching.tool;
        let tool = |id: &'static str, glyph: IconName, hint: &'static str, on: bool| {
            icon_button(id, glyph, palette)
                .when(on, |this| {
                    this.bg(palette.muted).border_color(palette.accent)
                })
                .when(!ready, |this| this.opacity(0.4))
                .tooltip(move |window, cx| Tooltip::new(hint).build(window, cx))
        };
        let pick = |id: &'static str, glyph: IconName, hint: &'static str, which: Tool| {
            tool(id, glyph, hint, in_hand == Some(which)).on_click(cx.listener(
                move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.pick_tool(which, window, cx);
                },
            ))
        };
        let edits = item.retouch.edits.len();
        let bar = div()
            .debug_selector(|| "attach-tools".into())
            .flex_none()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                tool("retouch-turn", IconName::RotateCw, "Turn", false).on_click(cx.listener(
                    |this, _, _, cx| {
                        cx.stop_propagation();
                        this.retouch_with(Edit::Turn, cx);
                    },
                )),
            )
            .child(pick("retouch-crop", IconName::Crop, "Crop", Tool::Crop))
            .child(pick("retouch-pen", IconName::Pencil, "Draw", Tool::Pen))
            .child(pick("retouch-words", IconName::Type, "Write", Tool::Words))
            .child(pick(
                "retouch-frame",
                IconName::Square,
                "Rectangle",
                Tool::Frame,
            ))
            .child(pick(
                "retouch-hide",
                IconName::Grid3x3,
                "Hide a part",
                Tool::Hide,
            ))
            .child(
                div()
                    .flex_none()
                    .mx_1()
                    .w(px(1.))
                    .h(px(16.))
                    .bg(palette.border),
            )
            .child(
                tool("retouch-undo", IconName::Undo2, "Undo", false)
                    .when(edits == 0, |this| this.opacity(0.4))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.retouch_undo(cx);
                    })),
            )
            .child(div().flex_1())
            .child(
                mono(if item.retouch.busy && edits > 0 {
                    "APPLYING…".to_owned()
                } else {
                    match edits {
                        0 => String::new(),
                        1 => "1 EDIT".to_owned(),
                        edits => format!("{edits} EDITS"),
                    }
                })
                .debug_selector(|| "retouch-count".into())
                .text_color(palette.text_muted),
            );
        let Some(in_hand) = in_hand else {
            return Some(div().flex_none().flex().flex_col().gap_2().child(bar));
        };
        // What the tool in hand does, and what it draws with.
        let mut detail = div()
            .debug_selector(|| "retouch-detail".into())
            .flex_none()
            .min_h(px(28.))
            .flex()
            .items_center()
            .gap_2();
        if in_hand == Tool::Words {
            if let Some(words) = &self.retouching.words {
                detail = detail.child(
                    super::numbers::input_box("retouch-words-field", words, false, palette)
                        .flex_1()
                        .min_w_0()
                        .h(px(30.)),
                );
            }
        } else {
            detail = detail.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_muted)
                    .child(in_hand.hint()),
            );
        }
        if in_hand.inked() {
            for (index, ink) in retouch::INKS.into_iter().enumerate() {
                let chosen = ink == self.retouching.ink;
                detail = detail.child(
                    div()
                        .id(("retouch-ink", index))
                        .debug_selector(move || format!("retouch-ink-{index}"))
                        .flex_none()
                        .size(px(20.))
                        .rounded_full()
                        .border_2()
                        .border_color(if chosen {
                            palette.accent
                        } else {
                            palette.border
                        })
                        .bg(rgb(u32::from_be_bytes([0, ink[0], ink[1], ink[2]])))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.retouching.ink = ink;
                            cx.notify();
                        })),
                );
            }
        }
        Some(
            div()
                .flex_none()
                .flex()
                .flex_col()
                .gap_2()
                .child(bar)
                .child(detail),
        )
    }

    /// The picture on show with the edits made, taking the pointer for
    /// the tool in hand. `None` until its pixels are here: the sheet
    /// shows the small picture it has meanwhile.
    pub(in crate::ui) fn render_retouch_picture(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<gpui_kit::AnyElement> {
        let item = self.attach.as_ref()?.current()?;
        if !item.retouchable() {
            return None;
        }
        let image = self.retouching.shown.get(&item.retouch.id)?.clone();
        let natural = item.retouch.natural?;
        let room = self.retouching.room.clone();
        let rect = placed(natural, room.get());
        let view = cx.entity_id();
        // The drag under way, as it will be on the picture.
        let tool = self.retouching.tool;
        let drag = self.retouching.drag.clone();
        let ink = self.retouching.ink;
        let (lime, thick) = (
            palette.accent,
            // As thick on screen as it will be on the picture.
            retouch::stroke_for(natural) * rect.size.width.as_f32() / natural.0.max(1) as f32,
        );
        let over = canvas(
            move |bounds, _, cx| {
                // Where the room is, for the next frame and the pointer.
                if room.get() != bounds {
                    room.set(bounds);
                    cx.notify(view);
                }
            },
            move |_, _, window, _| {
                let (Some(tool), Some(points)) = (tool, drag) else {
                    return;
                };
                let (Some(first), Some(last)) = (points.first().copied(), points.last().copied())
                else {
                    return;
                };
                let colour = rgb(u32::from_be_bytes([0, ink[0], ink[1], ink[2]]));
                let (path, colour): (PathBuilder, gpui_kit::Hsla) = match tool {
                    Tool::Pen => {
                        if points.len() < 2 {
                            return;
                        }
                        let mut path = PathBuilder::stroke(gpui_kit::px(thick.max(1.)));
                        path.move_to(first);
                        for at in &points[1..] {
                            path.line_to(*at);
                        }
                        (path, colour.into())
                    }
                    Tool::Words => return,
                    boxed => {
                        let (wide, colour) = match boxed {
                            Tool::Frame => (thick.max(1.), colour.into()),
                            _ => (1.5, lime),
                        };
                        let mut path = PathBuilder::stroke(gpui_kit::px(wide));
                        path.move_to(first);
                        path.line_to(point(last.x, first.y));
                        path.line_to(last);
                        path.line_to(point(first.x, last.y));
                        path.close();
                        (path, colour)
                    }
                };
                if let Ok(path) = path.build() {
                    window.paint_path(path, colour);
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let origin = room_origin(&self.retouching.room);
        Some(
            div()
                .id("retouch-picture")
                .relative()
                .size_full()
                .when(tool.is_some(), |this| this.cursor_crosshair())
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                        if this.retouching.tool.is_some() {
                            cx.stop_propagation();
                            this.retouch_press(event.position, cx);
                        }
                    }),
                )
                .on_mouse_move(
                    cx.listener(|this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                        if event.dragging() {
                            this.retouch_drag(event.position, cx);
                        } else if this.retouching.drag.is_some() {
                            // Let go outside the window.
                            this.retouch_drop(cx);
                        }
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.retouch_drop(cx)),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.retouch_drop(cx)),
                )
                .child(
                    div()
                        .debug_selector(|| "retouch-shown".into())
                        .absolute()
                        .left(rect.origin.x - origin.x)
                        .top(rect.origin.y - origin.y)
                        .w(rect.size.width)
                        .h(rect.size.height)
                        .child(img(image).size_full().object_fit(ObjectFit::Fill)),
                )
                .child(over)
                .into_any_element(),
        )
    }
}

/// The corner of the picture's room, which what is drawn in it is
/// measured from.
fn room_origin(room: &Rc<Cell<Bounds<Pixels>>>) -> Point<Pixels> {
    room.get().origin
}
