//! Text that can be selected with the mouse and copied.
//!
//! A message's text is drawn by GPUI's `StyledText` (formatting) inside an
//! `InteractiveText` (links, mentions). This wraps either of them and makes
//! it a participant of the window's text selection (`gpui-base`'s
//! `TextSelection`, whose layer the window's root already mounts): a drag
//! selects, across several messages too; a double click takes a word, a
//! triple click the line; Ctrl+C (the root's `Copy` action) copies.
//!
//! What is copied is decided here, not by what is painted: a `copy`
//! function turns the selected stretch back into what was written
//! (`*bold*` again, a mention as `@Name`).
//!
//! The selection is painted under the text in a token of the bubble it is
//! on. Nothing here measures or lays out anything the text would not have
//! measured itself, so a selection never moves a row.

use gpui_kit::base::{
    TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun,
};
use gpui_kit::{
    transparent_black, AnyElement, App, BorderStyle, Bounds, Corners, CursorStyle, Edges, Element,
    ElementId, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, PaintQuad, Pixels, Point, SharedString, TextLayout, Window,
};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

/// Turns a selected stretch of the shown text into what is copied.
pub type CopyText = Rc<dyn Fn(Range<usize>) -> String>;

/// What the element keeps between frames.
#[doc(hidden)]
#[derive(Clone)]
pub struct Kept {
    handle: TextSelectionHandle,
    /// The stretch selected as of the last paint, for the copy.
    selected: Rc<RefCell<Option<Range<usize>>>>,
}

/// A piece of text that takes part in the window's selection.
pub struct Selectable {
    id: ElementId,
    /// The text as it is drawn: a `StyledText`, or an `InteractiveText`
    /// around one.
    inner: AnyElement,
    /// The layout of that text, shared with it.
    layout: TextLayout,
    /// The characters that are drawn.
    text: SharedString,
    /// Where it stands among everything selectable in the window.
    order: u64,
    /// The colour a selection is painted in.
    colour: Hsla,
    /// All of it shows as selected: the message is focused and Ctrl+A
    /// was pressed.
    whole: bool,
    copy: Option<CopyText>,
    /// Stretches to mark under the text (what a search found), and the
    /// colour to mark them in.
    marks: Vec<Range<usize>>,
    mark_colour: Hsla,
    /// Set when a press lands on the text: whoever is under it knows the
    /// press was about the text, not about the empty room around it.
    pressed: Option<Rc<std::cell::Cell<bool>>>,
}

impl Selectable {
    /// `inner` draws `text` and shares `layout` with it.
    pub fn new(
        id: impl Into<ElementId>,
        text: impl Into<SharedString>,
        layout: TextLayout,
        inner: impl IntoElement,
    ) -> Self {
        Self {
            id: id.into(),
            inner: inner.into_any_element(),
            layout,
            text: text.into(),
            order: 0,
            colour: transparent_black(),
            whole: false,
            copy: None,
            marks: Vec::new(),
            mark_colour: transparent_black(),
            pressed: None,
        }
    }

    /// Stretches to mark under the text, in `colour`.
    pub fn marks(mut self, marks: Vec<Range<usize>>, colour: Hsla) -> Self {
        self.marks = marks;
        self.mark_colour = colour;
        self
    }

    /// A flag to raise when a press lands on the text.
    pub fn pressed(mut self, flag: Rc<std::cell::Cell<bool>>) -> Self {
        self.pressed = Some(flag);
        self
    }

    /// Its place in reading order among the window's selectable texts.
    pub fn order(mut self, order: u64) -> Self {
        self.order = order;
        self
    }

    /// The colour of a selection.
    pub fn colour(mut self, colour: Hsla) -> Self {
        self.colour = colour;
        self
    }

    /// Shows all of it as selected.
    pub fn whole(mut self, whole: bool) -> Self {
        self.whole = whole;
        self
    }

    /// What a selected stretch is copied as. Without it, as it is shown.
    pub fn copy(mut self, copy: CopyText) -> Self {
        self.copy = Some(copy);
        self
    }
}

/// Where `needle` (already in lower case) occurs in `text`, whatever the
/// case. Nothing when lowering the text would move its bytes about (then
/// the places would not be the text's own).
pub fn occurrences(text: &str, needle: Option<&str>) -> Vec<Range<usize>> {
    let Some(needle) = needle.filter(|needle| !needle.is_empty()) else {
        return Vec::new();
    };
    let lowered = text.to_lowercase();
    if lowered.len() != text.len() {
        return Vec::new();
    }
    // Each word of the search is marked on its own, as the index finds
    // them on their own.
    let mut found = Vec::new();
    for word in needle.split_whitespace() {
        let mut from = 0;
        while let Some(at) = lowered[from..].find(word) {
            let start = from + at;
            let end = start + word.len();
            if text.is_char_boundary(start) && text.is_char_boundary(end) {
                found.push(start..end);
            }
            from = end;
        }
    }
    found.sort_by_key(|range| range.start);
    found
}

/// The rectangles a selection from `start` to `end` covers: one on a
/// single line, else the rest of the first line, the lines between, and
/// the last line up to its end.
fn selection_quads(
    start: Point<Pixels>,
    end: Point<Pixels>,
    bounds: Bounds<Pixels>,
    line_height: Pixels,
) -> Vec<Bounds<Pixels>> {
    if start.y == end.y {
        return vec![Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        )];
    }
    let mut quads = vec![Bounds::from_corners(
        start,
        Point::new(bounds.right(), start.y + line_height),
    )];
    if end.y > start.y + line_height {
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), start.y + line_height),
            Point::new(bounds.right(), end.y),
        ));
    }
    quads.push(Bounds::from_corners(
        Point::new(bounds.left(), end.y),
        Point::new(end.x, end.y + line_height),
    ));
    quads
}

impl IntoElement for Selectable {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Selectable {
    type RequestLayoutState = Kept;
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let text = self.text.clone();
        let kept = window.with_element_state(
            global_id.expect("a selectable text has an id"),
            |kept: Option<Kept>, _| {
                let kept = kept.unwrap_or_else(|| Kept {
                    handle: TextSelectionHandle::new(text.clone(), cx),
                    selected: Rc::default(),
                });
                (kept.clone(), kept)
            },
        );
        (self.inner.request_layout(window, cx), kept)
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        kept: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.inner.prepaint(window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let registration = TextSelectionRegistration::new(hitbox.clone(), bounds)
            .with_document_order(self.order)
            .with_text_bounds(vec![bounds])
            .with_rendered_element(&kept.handle, window, cx);
        kept.handle.register(registration, window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        kept: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let before = TextSelection::selected_text(window, cx);
        let projection = kept.handle.update_runs(
            &[
                TextSelectionRun::new(self.text.clone(), self.layout.clone(), bounds)
                    .with_document_order(self.order),
            ],
            cx,
        );
        let selected = projection.ranges().iter().flatten().next().cloned();
        *kept.selected.borrow_mut() = selected.clone();
        if let Some(copy) = self.copy.clone() {
            let selected = kept.selected.clone();
            kept.handle.copy_with(
                move |_| match selected.borrow().clone() {
                    Some(range) => copy(range),
                    None => String::new(),
                },
                cx,
            );
        }
        if before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }
        for mark in std::mem::take(&mut self.marks) {
            if let (Some(start), Some(end)) = (
                self.layout.position_for_index(mark.start),
                self.layout.position_for_index(mark.end),
            ) {
                let line = self.layout.line_height();
                for quad in selection_quads(start, end, self.layout.bounds(), line) {
                    window.paint_quad(PaintQuad {
                        bounds: quad,
                        background: self.mark_colour.into(),
                        corner_radii: Corners::all(gpui_kit::px(2.)),
                        border_widths: Edges::default(),
                        border_color: transparent_black(),
                        border_style: BorderStyle::default(),
                    });
                }
            }
        }
        let shown = if self.whole {
            Some(0..self.text.len())
        } else {
            selected
        };
        if let Some(range) = shown {
            if let (Some(start), Some(end)) = (
                self.layout.position_for_index(range.start),
                self.layout.position_for_index(range.end),
            ) {
                let line = self.layout.line_height();
                for quad in selection_quads(start, end, self.layout.bounds(), line) {
                    window.paint_quad(PaintQuad {
                        bounds: quad,
                        background: self.colour.into(),
                        corner_radii: Corners::default(),
                        border_widths: Edges::default(),
                        border_color: transparent_black(),
                        border_style: BorderStyle::default(),
                    });
                }
            }
        }
        if let Some(pressed) = self.pressed.clone() {
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |_: &gpui_kit::MouseDownEvent, phase, window, _| {
                if phase == gpui_kit::DispatchPhase::Capture && hitbox.is_hovered(window) {
                    pressed.set(true);
                }
            });
        }
        // Text is text under the pointer; a link under it says so itself,
        // after this.
        window.set_cursor_style(CursorStyle::IBeam, hitbox);
        self.inner.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::{point, px, size};

    #[test]
    fn a_selection_covers_whole_lines_between_its_ends() {
        let bounds = Bounds {
            origin: point(px(10.), px(100.)),
            size: size(px(200.), px(60.)),
        };
        let line = px(20.);
        // On one line: from start to end, one line tall.
        let one = selection_quads(
            point(px(30.), px(100.)),
            point(px(90.), px(100.)),
            bounds,
            line,
        );
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].size, size(px(60.), px(20.)));
        // Over three lines: the rest of the first, all of the second, the
        // last up to the end.
        let three = selection_quads(
            point(px(30.), px(100.)),
            point(px(90.), px(140.)),
            bounds,
            line,
        );
        assert_eq!(three.len(), 3);
        assert_eq!(three[0].right(), bounds.right());
        assert_eq!(three[1].size, size(px(200.), px(20.)));
        assert_eq!(three[2].left(), bounds.left());
        assert_eq!(three[2].right(), px(90.));
    }

    #[test]
    fn what_a_search_looks_for_is_found_whatever_the_case() {
        assert_eq!(
            occurrences("The dentist, the Dentist", Some("dentist")),
            [4..11, 17..24]
        );
        assert_eq!(
            occurrences("lunch at noon", Some("noon lunch")),
            [0..5, 9..13]
        );
        assert!(occurrences("anything", None).is_empty());
        assert!(occurrences("anything", Some("")).is_empty());
        assert!(occurrences("nothing here", Some("dentist")).is_empty());
        // Past an emoji the places are still the text's own.
        let text = "🦷 dentist";
        let found = occurrences(text, Some("dentist"));
        assert_eq!(&text[found[0].clone()], "dentist");
    }
}
