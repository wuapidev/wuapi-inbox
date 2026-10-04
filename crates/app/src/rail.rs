//! The account rail's arrangement: the order of the numbers, the groups
//! they are put in, and how each one looks. All of it is the user's own,
//! local presentation; nothing here is sent to a provider.
//!
//! This module is the part that decides, with no drawing in it:
//!
//! * [`RailLayout`]: what is where, kept in `rail.json` next to
//!   `settings.json` and keyed by provider and account id, so it belongs
//!   to no provider in particular and survives a sign-out for the numbers
//!   that are still there afterwards;
//! * [`RailLayout::apply`]: what a drop does (between two items: move
//!   there; onto a number: group the two; onto a group: join it), which
//!   is also what the context menu and the keyboard go through;
//! * [`DragState`] and [`hit`]: when a press becomes a drag, and which
//!   drop the pointer is over.
//!
//! The drawing is `ui/rail.rs`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The file's name inside the data directory.
pub const FILE_NAME: &str = "rail.json";

/// What a group is called until it is renamed.
pub const GROUP_NAME: &str = "Group";

/// How many colours a number or a group can be given (`Palette::rail`).
pub const COLOURS: u8 = 6;

/// How far the pointer travels with the button down before a press is a
/// drag. Under it, letting go is a click.
pub const DRAG_THRESHOLD: f32 = 5.;

/// How close to the rail's top or bottom edge a drag scrolls it.
pub const SCROLL_EDGE: f32 = 28.;

/// A number, as the rail knows it: `provider:account`.
pub type Key = String;

/// The key of an account of a provider.
pub fn key(provider: &str, account: &str) -> Key {
    format!("{provider}:{account}")
}

/// How a number looks in the rail, where the user changed it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Look {
    /// A name of the user's own, shown instead of the provider's.
    pub label: Option<String>,
    /// An emoji or initials shown instead of the picture.
    pub glyph: Option<String>,
    /// The colour behind the glyph: an index into the rail's palette.
    pub colour: Option<u8>,
    /// A picture of the user's choice is kept for it (in the encrypted
    /// store, under the account).
    pub image: bool,
    /// Notifications for this number are silenced here.
    pub muted: bool,
}

impl Look {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// Several numbers kept together.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// Stable for as long as the group exists.
    pub id: u32,
    /// Its name.
    pub name: String,
    /// Its colour: an index into the rail's palette.
    #[serde(default)]
    pub colour: u8,
    /// Showing its members, or closed to four small icons.
    #[serde(default)]
    pub expanded: bool,
    /// The numbers in it, in order.
    pub members: Vec<Key>,
}

/// One thing in the rail, top to bottom.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RailItem {
    /// A number on its own.
    Account(Key),
    /// A group of numbers.
    Group(Group),
}

/// What is being moved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dragged {
    /// A number.
    Account(Key),
    /// A whole group.
    Group(u32),
}

/// Where it is dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Drop {
    /// Between two items of the rail: before the item at this position
    /// (the number of items: at the end).
    Top(usize),
    /// Between two members of an open group: before the member at this
    /// position.
    InGroup(u32, usize),
    /// Onto a number.
    OntoAccount(Key),
    /// Onto a group.
    OntoGroup(u32),
}

/// The rail's arrangement.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RailLayout {
    /// The numbers and groups, top to bottom.
    pub items: Vec<RailItem>,
    /// How numbers look, for those the user changed.
    pub looks: BTreeMap<Key, Look>,
    /// The id the next group gets.
    next_group: u32,
}

impl RailLayout {
    /// Reads the file; a missing or unreadable one is an empty layout.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes the file whole, through a temporary one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let draft = path.with_extension("json.tmp");
        std::fs::write(&draft, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&draft, path)
    }

    /// Every number in the rail, top to bottom, groups opened out.
    pub fn keys(&self) -> Vec<&Key> {
        self.items
            .iter()
            .flat_map(|item| match item {
                RailItem::Account(key) => std::slice::from_ref(key).iter(),
                RailItem::Group(group) => group.members.iter(),
            })
            .collect()
    }

    /// Brings the layout in line with the accounts that exist for a
    /// provider: new ones go to the end, and those that are gone leave,
    /// with how they looked. The numbers of other providers are not
    /// touched. Nothing is removed while `present` is empty: that is a
    /// session that has not loaded yet, not one without numbers.
    pub fn reconcile(&mut self, provider: &str, present: &[Key]) -> bool {
        let before = self.clone();
        let prefix = format!("{provider}:");
        if !present.is_empty() {
            let gone = |key: &Key| key.starts_with(&prefix) && !present.contains(key);
            for item in &mut self.items {
                if let RailItem::Group(group) = item {
                    group.members.retain(|key| !gone(key));
                }
            }
            self.items
                .retain(|item| !matches!(item, RailItem::Account(key) if gone(key)));
            self.looks.retain(|key, _| !gone(key));
            self.tidy();
        }
        for key in present {
            if !self.keys().contains(&key) {
                self.items.push(RailItem::Account(key.clone()));
            }
        }
        *self != before
    }

    /// The group with this id.
    pub fn group(&self, id: u32) -> Option<&Group> {
        self.items.iter().find_map(|item| match item {
            RailItem::Group(group) if group.id == id => Some(group),
            _ => None,
        })
    }

    fn group_mut(&mut self, id: u32) -> Option<&mut Group> {
        self.items.iter_mut().find_map(|item| match item {
            RailItem::Group(group) if group.id == id => Some(group),
            _ => None,
        })
    }

    /// The group a number is in.
    pub fn group_of(&self, key: &Key) -> Option<u32> {
        self.items.iter().find_map(|item| match item {
            RailItem::Group(group) if group.members.contains(key) => Some(group.id),
            _ => None,
        })
    }

    /// The groups, top to bottom.
    pub fn groups(&self) -> Vec<&Group> {
        self.items
            .iter()
            .filter_map(|item| match item {
                RailItem::Group(group) => Some(group),
                RailItem::Account(_) => None,
            })
            .collect()
    }

    /// How a number looks.
    pub fn look(&self, key: &Key) -> Look {
        self.looks.get(key).cloned().unwrap_or_default()
    }

    /// Changes how a number looks. A look that is back to the default is
    /// not kept.
    pub fn set_look(&mut self, key: &Key, change: impl FnOnce(&mut Look)) {
        let mut look = self.look(key);
        change(&mut look);
        if look.is_default() {
            self.looks.remove(key);
        } else {
            self.looks.insert(key.clone(), look);
        }
    }

    /// The position of a number or a group among the rail's items.
    fn top_index(&self, dragged: &Dragged) -> Option<usize> {
        self.items.iter().position(|item| match (item, dragged) {
            (RailItem::Account(key), Dragged::Account(wanted)) => key == wanted,
            (RailItem::Group(group), Dragged::Group(id)) => group.id == *id,
            _ => false,
        })
    }

    /// Takes a number out of wherever it is. Nothing is tidied yet.
    fn lift(&mut self, key: &Key) {
        for item in &mut self.items {
            if let RailItem::Group(group) = item {
                group.members.retain(|member| member != key);
            }
        }
        self.items
            .retain(|item| !matches!(item, RailItem::Account(found) if found == key));
    }

    /// A group left with one number dissolves, the number staying where
    /// the group was; one left empty goes.
    fn tidy(&mut self) {
        self.items = std::mem::take(&mut self.items)
            .into_iter()
            .filter_map(|item| match item {
                RailItem::Group(mut group) => match group.members.len() {
                    0 => None,
                    1 => Some(RailItem::Account(group.members.remove(0))),
                    _ => Some(RailItem::Group(group)),
                },
                account => Some(account),
            })
            .collect();
    }

    /// Moves `dragged` to `drop`. Returns whether anything changed.
    ///
    /// * between two items: it goes there (out of its group, if it was in
    ///   one);
    /// * between two members of a group: it goes into the group there;
    /// * onto a number: the two become a group where the target was (or
    ///   it joins the target's group);
    /// * onto a group: it joins the group, at the end.
    ///
    /// Groups do not nest: a group can only be moved between items.
    pub fn apply(&mut self, dragged: &Dragged, drop: &Drop) -> bool {
        let before = self.clone();
        match (dragged, drop) {
            (Dragged::Account(key), Drop::Top(index)) => {
                // What it lands before, remembered by what it is: lifting
                // the number moves the positions.
                let anchor = self.items.get(*index).cloned();
                if matches!(&anchor, Some(RailItem::Account(found)) if found == key) {
                    return false;
                }
                self.lift(key);
                let at = anchor
                    .and_then(|anchor| self.position_of(&anchor))
                    .unwrap_or(self.items.len());
                self.items.insert(at, RailItem::Account(key.clone()));
            }
            (Dragged::Account(key), Drop::InGroup(id, index)) => {
                let Some(group) = self.group(*id) else {
                    return false;
                };
                let anchor = group.members.get(*index).cloned();
                if anchor.as_ref() == Some(key) {
                    return false;
                }
                self.lift(key);
                let Some(group) = self.group_mut(*id) else {
                    return false;
                };
                let at = anchor
                    .and_then(|anchor| group.members.iter().position(|m| *m == anchor))
                    .unwrap_or(group.members.len());
                group.members.insert(at, key.clone());
            }
            (Dragged::Account(key), Drop::OntoAccount(target)) => {
                if key == target || !self.keys().contains(&target) {
                    return false;
                }
                match self.group_of(target) {
                    Some(id) => {
                        self.lift(key);
                        if let Some(group) = self.group_mut(id) {
                            let at = group
                                .members
                                .iter()
                                .position(|member| member == target)
                                .map_or(group.members.len(), |at| at + 1);
                            group.members.insert(at, key.clone());
                        }
                    }
                    None => {
                        self.lift(key);
                        let Some(at) = self.top_index(&Dragged::Account(target.clone())) else {
                            return false;
                        };
                        let id = self.next_group;
                        self.next_group += 1;
                        self.items[at] = RailItem::Group(Group {
                            id,
                            name: GROUP_NAME.to_owned(),
                            colour: 0,
                            // Open, so the user sees what just happened.
                            expanded: true,
                            members: vec![target.clone(), key.clone()],
                        });
                    }
                }
            }
            (Dragged::Account(key), Drop::OntoGroup(id)) => {
                if self.group_of(key) == Some(*id) || self.group(*id).is_none() {
                    return false;
                }
                self.lift(key);
                if let Some(group) = self.group_mut(*id) {
                    group.members.push(key.clone());
                }
            }
            (Dragged::Group(id), Drop::Top(index)) => {
                let Some(from) = self.top_index(dragged) else {
                    return false;
                };
                let anchor = self.items.get(*index).cloned();
                if matches!(&anchor, Some(RailItem::Group(group)) if group.id == *id) {
                    return false;
                }
                let moved = self.items.remove(from);
                let at = anchor
                    .and_then(|anchor| self.position_of(&anchor))
                    .unwrap_or(self.items.len());
                self.items.insert(at, moved);
            }
            // A group goes between items, never into or onto anything.
            (Dragged::Group(_), _) => return false,
        }
        self.tidy();
        *self != before
    }

    fn position_of(&self, wanted: &RailItem) -> Option<usize> {
        self.items.iter().position(|item| match (item, wanted) {
            (RailItem::Account(a), RailItem::Account(b)) => a == b,
            (RailItem::Group(a), RailItem::Group(b)) => a.id == b.id,
            _ => false,
        })
    }

    /// Puts a number in a new group of its own making with `other`... or,
    /// with nobody else, in a group by itself is not a thing: a group
    /// needs two. Used by the menu's "New group with…".
    pub fn group_with(&mut self, key: &Key, other: &Key) -> bool {
        self.apply(
            &Dragged::Account(key.clone()),
            &Drop::OntoAccount(other.clone()),
        )
    }

    /// Takes a number out of its group and puts it right after it.
    pub fn leave_group(&mut self, key: &Key) -> bool {
        let Some(id) = self.group_of(key) else {
            return false;
        };
        let Some(at) = self.top_index(&Dragged::Group(id)) else {
            return false;
        };
        self.apply(&Dragged::Account(key.clone()), &Drop::Top(at + 1))
    }

    /// Dissolves a group: its numbers stay where it was, in order.
    pub fn ungroup(&mut self, id: u32) -> bool {
        let Some(at) = self.top_index(&Dragged::Group(id)) else {
            return false;
        };
        let RailItem::Group(group) = self.items.remove(at) else {
            return false;
        };
        for (offset, key) in group.members.into_iter().enumerate() {
            self.items.insert(at + offset, RailItem::Account(key));
        }
        true
    }

    /// Opens or closes a group.
    pub fn toggle(&mut self, id: u32) {
        if let Some(group) = self.group_mut(id) {
            group.expanded = !group.expanded;
        }
    }

    /// Renames a group. An empty name puts the neutral one back.
    pub fn rename_group(&mut self, id: u32, name: &str) {
        if let Some(group) = self.group_mut(id) {
            let name = name.trim();
            group.name = if name.is_empty() {
                GROUP_NAME.to_owned()
            } else {
                name.chars().take(40).collect()
            };
        }
    }

    /// Gives a group a colour of the rail's palette.
    pub fn colour_group(&mut self, id: u32, colour: u8) {
        if let Some(group) = self.group_mut(id) {
            group.colour = colour % COLOURS;
        }
    }

    /// Moves a number or a group one place up or down, for the keyboard
    /// and the menu: among the rail's items, or within its group.
    pub fn nudge(&mut self, dragged: &Dragged, up: bool) -> bool {
        if let Dragged::Account(key) = dragged {
            if let Some(id) = self.group_of(key) {
                let Some(group) = self.group_mut(id) else {
                    return false;
                };
                let Some(at) = group.members.iter().position(|member| member == key) else {
                    return false;
                };
                let to = if up { at.checked_sub(1) } else { Some(at + 1) };
                return match to.filter(|to| *to < group.members.len()) {
                    Some(to) => {
                        group.members.swap(at, to);
                        true
                    }
                    None => false,
                };
            }
        }
        let Some(at) = self.top_index(dragged) else {
            return false;
        };
        let to = if up { at.checked_sub(1) } else { Some(at + 1) };
        match to.filter(|to| *to < self.items.len()) {
            Some(to) => {
                self.items.swap(at, to);
                true
            }
            None => false,
        }
    }
}

// ----- where the pointer is -------------------------------------------------

/// One row of the rail as drawn, for telling what the pointer is over.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    /// What the row shows.
    pub what: RowKind,
    /// Its top and bottom edges, in window coordinates.
    pub top: f32,
    /// See `top`.
    pub bottom: f32,
}

/// What a row of the rail shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// A number on its own: the item at this position.
    Account(usize, Key),
    /// A closed group, or the head of an open one: the item at this
    /// position.
    Group(usize, u32),
    /// A number inside an open group: the member at this position.
    Member(u32, usize, Key),
    /// The foot of an open group, with how many members it has: below it
    /// is outside the group.
    GroupEnd(usize, u32, usize),
}

/// The drop the pointer is over, at height `y`, while `dragged` is held.
///
/// The middle half of a row is "onto" it; its top and bottom quarters are
/// "between" it and its neighbours. A group being moved only ever goes
/// between items.
pub fn hit(rows: &[Row], y: f32, dragged: &Dragged, items: usize) -> Option<Drop> {
    let first = rows.first()?;
    if y < first.top {
        return Some(Drop::Top(0));
    }
    let moving_group = matches!(dragged, Dragged::Group(_));
    for (index, row) in rows.iter().enumerate() {
        // The gap under a row belongs to the row.
        let reach = rows.get(index + 1).map_or(row.bottom, |next| next.top);
        if y >= reach {
            continue;
        }
        let height = (row.bottom - row.top).max(1.);
        let part = ((y - row.top) / height).clamp(0., 1.);
        let (upper, lower) = (part < 0.25, part > 0.75);
        return Some(match &row.what {
            RowKind::Account(at, key) => {
                if upper || (moving_group && part < 0.5) {
                    Drop::Top(*at)
                } else if lower || moving_group {
                    Drop::Top(at + 1)
                } else {
                    Drop::OntoAccount(key.clone())
                }
            }
            RowKind::Group(at, id) => {
                if upper || (moving_group && part < 0.5) {
                    Drop::Top(*at)
                } else if moving_group {
                    Drop::Top(at + 1)
                } else {
                    Drop::OntoGroup(*id)
                }
            }
            RowKind::Member(id, at, key) => {
                if moving_group {
                    // Inside another group: nowhere a group can go.
                    return None;
                }
                if upper {
                    Drop::InGroup(*id, *at)
                } else if lower {
                    Drop::InGroup(*id, at + 1)
                } else {
                    Drop::OntoAccount(key.clone())
                }
            }
            RowKind::GroupEnd(at, id, members) => {
                if moving_group || part >= 0.5 {
                    Drop::Top(at + 1)
                } else {
                    Drop::InGroup(*id, *members)
                }
            }
        });
    }
    Some(Drop::Top(items))
}

/// How far to scroll the rail, in pixels, for a drag at height `y` in a
/// rail that shows from `top` to `bottom`: negative towards the top.
pub fn autoscroll(y: f32, top: f32, bottom: f32) -> f32 {
    if y < top + SCROLL_EDGE {
        -((top + SCROLL_EDGE - y).min(SCROLL_EDGE) / 2.)
    } else if y > bottom - SCROLL_EDGE {
        (y - (bottom - SCROLL_EDGE)).min(SCROLL_EDGE) / 2.
    } else {
        0.
    }
}

/// How a press on the rail ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Released {
    /// It never became a drag: a click.
    Click,
    /// A drag let go over a drop: the move to make.
    Dropped(Dragged, Drop),
    /// A drag let go over nothing: the item goes back where it was.
    Returned(Dragged),
}

/// A press on the rail on its way to being a click or a drag.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DragState {
    /// What was pressed, and where.
    pressed: Option<(Dragged, (f32, f32))>,
    /// The pointer has travelled far enough: it is a drag.
    dragging: bool,
    /// Where the pointer was last seen.
    pointer: (f32, f32),
    /// The drop the pointer is over, and since when.
    over: Option<(Drop, std::time::Instant)>,
    /// The press that just ended was a drag: the click that follows it
    /// is not a click.
    ended_as_drag: bool,
}

impl DragState {
    /// The button went down on a number or a group.
    pub fn press(&mut self, what: Dragged, at: (f32, f32)) {
        *self = Self {
            pressed: Some((what, at)),
            pointer: at,
            ..Self::default()
        };
    }

    /// The pointer moved to `at`. Returns whether a drag is under way.
    pub fn moved(&mut self, at: (f32, f32)) -> bool {
        if let Some((_, start)) = &self.pressed {
            let (dx, dy) = (at.0 - start.0, at.1 - start.1);
            if !self.dragging && (dx * dx + dy * dy).sqrt() >= DRAG_THRESHOLD {
                self.dragging = true;
            }
            self.pointer = at;
        }
        self.dragging
    }

    /// Where the pointer was last seen while something is held.
    pub fn pointer(&self) -> (f32, f32) {
        self.pointer
    }

    /// What is being dragged, once the press has become a drag.
    pub fn dragging(&self) -> Option<&Dragged> {
        self.pressed
            .as_ref()
            .filter(|_| self.dragging)
            .map(|(what, _)| what)
    }

    /// Says which drop the pointer is over at `now`. Resting on the same
    /// drop keeps the moment it got there.
    pub fn hover(&mut self, over: Option<Drop>, now: std::time::Instant) {
        if !self.dragging {
            return;
        }
        self.over = match (over, self.over.take()) {
            (Some(over), Some((known, since))) if over == known => Some((known, since)),
            (Some(over), _) => Some((over, now)),
            (None, _) => None,
        };
    }

    /// The drop the pointer is over.
    pub fn over(&self) -> Option<&Drop> {
        self.over
            .as_ref()
            .filter(|_| self.dragging)
            .map(|(over, _)| over)
    }

    /// Since when the pointer has been over that drop.
    #[cfg(test)]
    pub fn over_since(&self) -> Option<std::time::Instant> {
        self.over
            .as_ref()
            .filter(|_| self.dragging)
            .map(|(_, since)| *since)
    }

    /// The closed group the drag has rested on for `dwell`: it should
    /// open, so the item can be placed inside. Answers once per rest.
    pub fn dwelt(
        &mut self,
        now: std::time::Instant,
        dwell: std::time::Duration,
        closed: impl Fn(u32) -> bool,
    ) -> Option<u32> {
        if !self.dragging {
            return None;
        }
        match &mut self.over {
            Some((Drop::OntoGroup(id), since))
                if closed(*id) && now.saturating_duration_since(*since) >= dwell =>
            {
                // Counted from here again, should it close and be rested on.
                *since = now;
                Some(*id)
            }
            _ => None,
        }
    }

    /// The button came up.
    pub fn release(&mut self) -> Released {
        let was_drag = self.dragging;
        let outcome = match (self.pressed.take(), self.over.take()) {
            (Some((what, _)), Some((over, _))) if was_drag => Released::Dropped(what, over),
            (Some((what, _)), None) if was_drag => Released::Returned(what),
            _ => Released::Click,
        };
        *self = Self {
            ended_as_drag: was_drag,
            pointer: self.pointer,
            ..Self::default()
        };
        outcome
    }

    /// Escape: the drag is off, nothing moves, and what was held goes
    /// back where it was. `None` when nothing was being dragged.
    pub fn cancel(&mut self) -> Option<Dragged> {
        let held = self.dragging().cloned();
        if held.is_some() {
            *self = Self {
                ended_as_drag: true,
                pointer: self.pointer,
                ..Self::default()
            };
        }
        held
    }

    /// Whether the press that just ended was a drag, asked once by the
    /// click that follows it.
    pub fn swallow_click(&mut self) -> bool {
        std::mem::take(&mut self.ended_as_drag)
    }
}

/// Whether dropping `dragged` at `drop` would leave it where it is: no
/// room needs to open there.
pub fn stays_put(layout: &RailLayout, dragged: &Dragged, drop: &Drop) -> bool {
    match (dragged, drop) {
        (_, Drop::Top(index)) => layout
            .top_index(dragged)
            .is_some_and(|at| *index == at || *index == at + 1),
        (Dragged::Account(key), Drop::InGroup(id, index)) => layout
            .group(*id)
            .and_then(|group| group.members.iter().position(|member| member == key))
            .is_some_and(|at| *index == at || *index == at + 1),
        _ => false,
    }
}

/// The state to show for a group: the worst of its members'. Higher is
/// worse.
pub fn worst<T: Copy>(states: impl IntoIterator<Item = (T, u8)>) -> Option<T> {
    states
        .into_iter()
        .max_by_key(|(_, severity)| *severity)
        .map(|(state, _)| state)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(name: &str) -> Key {
        key("mock", name)
    }

    fn layout(names: &[&str]) -> RailLayout {
        let mut layout = RailLayout::default();
        let keys: Vec<Key> = names.iter().map(|name| k(name)).collect();
        layout.reconcile("mock", &keys);
        layout
    }

    /// The rail as text: `a [b c] d`, a group in brackets.
    fn shape(layout: &RailLayout) -> String {
        layout
            .items
            .iter()
            .map(|item| match item {
                RailItem::Account(key) => key.trim_start_matches("mock:").to_owned(),
                RailItem::Group(group) => format!(
                    "[{}]",
                    group
                        .members
                        .iter()
                        .map(|key| key.trim_start_matches("mock:"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn drag(layout: &mut RailLayout, name: &str, drop: Drop) -> bool {
        layout.apply(&Dragged::Account(k(name)), &drop)
    }

    #[test]
    fn a_drop_between_items_reorders() {
        let mut rail = layout(&["a", "b", "c", "d"]);
        assert!(drag(&mut rail, "d", Drop::Top(0)));
        assert_eq!(shape(&rail), "d a b c");
        assert!(drag(&mut rail, "d", Drop::Top(4)));
        assert_eq!(shape(&rail), "a b c d");
        assert!(drag(&mut rail, "a", Drop::Top(2)));
        assert_eq!(shape(&rail), "b a c d");
        // Before itself, or right after itself: where it already is.
        assert!(!drag(&mut rail, "a", Drop::Top(1)));
        assert!(!drag(&mut rail, "a", Drop::Top(2)));
        assert_eq!(shape(&rail), "b a c d");
    }

    #[test]
    fn a_drop_onto_a_number_makes_a_group_and_onto_a_group_joins_it() {
        let mut rail = layout(&["a", "b", "c", "d"]);
        assert!(drag(&mut rail, "d", Drop::OntoAccount(k("b"))));
        // Where the target was, target first, open, with the neutral name.
        assert_eq!(shape(&rail), "a [b d] c");
        let group = rail.groups()[0].clone();
        assert_eq!(group.name, GROUP_NAME);
        assert!(group.expanded);
        assert!(drag(&mut rail, "a", Drop::OntoGroup(group.id)));
        assert_eq!(shape(&rail), "[b d a] c");
        // Onto a member: into its group, after it.
        assert!(drag(&mut rail, "c", Drop::OntoAccount(k("b"))));
        assert_eq!(shape(&rail), "[b c d a]");
        // Onto itself, or onto the group it is in: nothing.
        assert!(!drag(&mut rail, "c", Drop::OntoAccount(k("c"))));
        assert!(!drag(&mut rail, "c", Drop::OntoGroup(group.id)));
        // A second group gets another id.
        let mut rail = layout(&["a", "b", "c", "d"]);
        drag(&mut rail, "b", Drop::OntoAccount(k("a")));
        drag(&mut rail, "d", Drop::OntoAccount(k("c")));
        assert_eq!(shape(&rail), "[a b] [c d]");
        assert_ne!(rail.groups()[0].id, rail.groups()[1].id);
    }

    #[test]
    fn a_number_is_reordered_inside_its_group_and_dragged_out_of_it() {
        let mut rail = layout(&["a", "b", "c", "d", "e"]);
        drag(&mut rail, "b", Drop::OntoAccount(k("a")));
        let id = rail.groups()[0].id;
        drag(&mut rail, "c", Drop::OntoGroup(id));
        assert_eq!(shape(&rail), "[a b c] d e");
        // Within the group.
        assert!(drag(&mut rail, "c", Drop::InGroup(id, 0)));
        assert_eq!(shape(&rail), "[c a b] d e");
        assert!(drag(&mut rail, "c", Drop::InGroup(id, 3)));
        assert_eq!(shape(&rail), "[a b c] d e");
        // From outside, between two members.
        assert!(drag(&mut rail, "e", Drop::InGroup(id, 1)));
        assert_eq!(shape(&rail), "[a e b c] d");
        // Out of it: between items outside the group.
        assert!(drag(&mut rail, "e", Drop::Top(1)));
        assert_eq!(shape(&rail), "[a b c] e d");
        assert!(drag(&mut rail, "b", Drop::Top(0)));
        assert_eq!(shape(&rail), "b [a c] e d");
    }

    #[test]
    fn a_group_left_with_one_number_dissolves_and_an_empty_one_goes() {
        let mut rail = layout(&["a", "b", "c"]);
        drag(&mut rail, "c", Drop::OntoAccount(k("b")));
        assert_eq!(shape(&rail), "a [b c]");
        // One left: the group dissolves, the number stays in its place.
        assert!(drag(&mut rail, "c", Drop::Top(0)));
        assert_eq!(shape(&rail), "c a b");
        assert!(rail.groups().is_empty());
        // The numbers of a group are deleted elsewhere: no shell is left.
        drag(&mut rail, "a", Drop::OntoAccount(k("c")));
        assert_eq!(shape(&rail), "[c a] b");
        rail.reconcile("mock", &[k("b")]);
        assert_eq!(shape(&rail), "b");
    }

    #[test]
    fn groups_move_like_numbers_and_do_not_nest() {
        let mut rail = layout(&["a", "b", "c", "d", "e"]);
        drag(&mut rail, "b", Drop::OntoAccount(k("a")));
        drag(&mut rail, "e", Drop::OntoAccount(k("d")));
        assert_eq!(shape(&rail), "[a b] c [d e]");
        let (first, second) = (rail.groups()[0].id, rail.groups()[1].id);
        assert!(rail.apply(&Dragged::Group(second), &Drop::Top(0)));
        assert_eq!(shape(&rail), "[d e] [a b] c");
        assert!(rail.apply(&Dragged::Group(second), &Drop::Top(3)));
        assert_eq!(shape(&rail), "[a b] c [d e]");
        // Into or onto anything: not a move a group can make.
        assert!(!rail.apply(&Dragged::Group(second), &Drop::OntoGroup(first)));
        assert!(!rail.apply(&Dragged::Group(second), &Drop::OntoAccount(k("c"))));
        assert!(!rail.apply(&Dragged::Group(second), &Drop::InGroup(first, 0)));
        assert_eq!(shape(&rail), "[a b] c [d e]");
    }

    #[test]
    fn the_menu_and_the_keyboard_make_the_same_moves() {
        let mut rail = layout(&["a", "b", "c", "d"]);
        assert!(rail.group_with(&k("b"), &k("a")));
        let id = rail.groups()[0].id;
        assert!(rail.apply(&Dragged::Account(k("c")), &Drop::OntoGroup(id)));
        assert_eq!(shape(&rail), "[a b c] d");
        // Up and down, inside the group and among the items.
        assert!(rail.nudge(&Dragged::Account(k("c")), true));
        assert_eq!(shape(&rail), "[a c b] d");
        assert!(
            !rail.nudge(&Dragged::Account(k("a")), true),
            "already first"
        );
        assert!(rail.nudge(&Dragged::Group(id), false));
        assert_eq!(shape(&rail), "d [a c b]");
        assert!(rail.nudge(&Dragged::Account(k("d")), false));
        assert_eq!(shape(&rail), "[a c b] d");
        // Out of the group: right after it.
        assert!(rail.leave_group(&k("c")));
        assert_eq!(shape(&rail), "[a b] c d");
        assert!(!rail.leave_group(&k("c")));
        // A name, a colour, open and closed.
        rail.rename_group(id, "  Work  ");
        rail.colour_group(id, 4);
        rail.toggle(id);
        let group = rail.group(id).unwrap();
        assert_eq!(
            (group.name.as_str(), group.colour, group.expanded),
            ("Work", 4, false)
        );
        rail.rename_group(id, " ");
        assert_eq!(rail.group(id).unwrap().name, GROUP_NAME);
        // Ungrouped: its numbers stay where it was.
        assert!(rail.ungroup(id));
        assert_eq!(shape(&rail), "a b c d");
    }

    #[test]
    fn the_layout_follows_the_accounts_that_exist_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(FILE_NAME);
        assert_eq!(RailLayout::load(&path), RailLayout::default());

        let mut rail = layout(&["a", "b", "c"]);
        drag(&mut rail, "c", Drop::OntoAccount(k("a")));
        rail.set_look(&k("b"), |look| {
            look.label = Some("Sales".into());
            look.colour = Some(2);
            look.muted = true;
        });
        // Another provider's numbers live in the same file.
        rail.reconcile("wuapi", &[key("wuapi", "x")]);
        rail.save(&path).unwrap();
        let mut loaded = RailLayout::load(&path);
        assert_eq!(loaded, rail);

        // A session that has not loaded yet removes nothing.
        assert!(!loaded.reconcile("mock", &[]));
        assert_eq!(shape(&loaded), "[a c] b wuapi:x");
        // Signed in again: `b` no longer exists, `d` is new.
        assert!(loaded.reconcile("mock", &[k("a"), k("c"), k("d")]));
        assert_eq!(shape(&loaded), "[a c] wuapi:x d");
        assert!(!loaded.looks.contains_key(&k("b")), "its look went with it");
        // The other provider's number was not touched.
        assert!(loaded.keys().contains(&&key("wuapi", "x")));
        // A look put back to the default is not kept.
        loaded.set_look(&k("a"), |look| look.muted = true);
        loaded.set_look(&k("a"), |look| look.muted = false);
        assert!(loaded.looks.is_empty());
    }

    fn rows() -> Vec<Row> {
        // a, an open group [b c] with its head and foot, d.
        vec![
            Row {
                what: RowKind::Account(0, k("a")),
                top: 0.,
                bottom: 40.,
            },
            Row {
                what: RowKind::Group(1, 7),
                top: 48.,
                bottom: 68.,
            },
            Row {
                what: RowKind::Member(7, 0, k("b")),
                top: 72.,
                bottom: 112.,
            },
            Row {
                what: RowKind::Member(7, 1, k("c")),
                top: 116.,
                bottom: 156.,
            },
            Row {
                what: RowKind::GroupEnd(1, 7, 2),
                top: 156.,
                bottom: 164.,
            },
            Row {
                what: RowKind::Account(2, k("d")),
                top: 172.,
                bottom: 212.,
            },
        ]
    }

    #[test]
    fn the_middle_of_a_row_is_onto_it_and_its_edges_are_between() {
        let rows = rows();
        let account = Dragged::Account(k("d"));
        let at = |y: f32| hit(&rows, y, &account, 3);
        // A number on its own.
        assert_eq!(at(4.), Some(Drop::Top(0)));
        assert_eq!(at(20.), Some(Drop::OntoAccount(k("a"))));
        assert_eq!(at(36.), Some(Drop::Top(1)));
        // The gap under it is still "after it".
        assert_eq!(at(44.), Some(Drop::Top(1)));
        // The head of the open group: before it, or into it.
        assert_eq!(at(50.), Some(Drop::Top(1)));
        assert_eq!(at(60.), Some(Drop::OntoGroup(7)));
        // Its members: between them, or onto one.
        assert_eq!(at(74.), Some(Drop::InGroup(7, 0)));
        assert_eq!(at(92.), Some(Drop::OntoAccount(k("b"))));
        assert_eq!(at(110.), Some(Drop::InGroup(7, 1)));
        assert_eq!(at(150.), Some(Drop::InGroup(7, 2)));
        // Its foot: the upper half is still inside, the lower half is out.
        assert_eq!(at(158.), Some(Drop::InGroup(7, 2)));
        assert_eq!(at(162.), Some(Drop::Top(2)));
        // Above everything, below everything.
        assert_eq!(at(-10.), Some(Drop::Top(0)));
        assert_eq!(at(400.), Some(Drop::Top(3)));
        assert_eq!(hit(&[], 10., &account, 0), None);

        // A group being moved only goes between items.
        let group = Dragged::Group(9);
        let at = |y: f32| hit(&rows, y, &group, 3);
        assert_eq!(at(10.), Some(Drop::Top(0)));
        assert_eq!(at(30.), Some(Drop::Top(1)));
        assert_eq!(at(60.), Some(Drop::Top(2)));
        assert_eq!(at(92.), None, "not inside another group");
        assert_eq!(at(190.), Some(Drop::Top(2)));
        assert_eq!(at(205.), Some(Drop::Top(3)));
    }

    #[test]
    fn a_press_is_a_click_until_it_has_moved_and_escape_calls_a_drag_off() {
        let now = std::time::Instant::now();
        let mut drag = DragState::default();
        // A click: down, a tremble, up.
        drag.press(Dragged::Account(k("a")), (20., 20.));
        assert!(!drag.moved((22., 21.)));
        drag.hover(Some(Drop::Top(2)), now);
        assert_eq!(drag.over(), None, "not a drag yet");
        assert_eq!(drag.release(), Released::Click);
        assert!(!drag.swallow_click(), "the click selects the number");

        // A drag: past the threshold, over a drop, let go.
        drag.press(Dragged::Account(k("a")), (20., 20.));
        assert!(drag.moved((20., 20. + DRAG_THRESHOLD)));
        assert_eq!(drag.dragging(), Some(&Dragged::Account(k("a"))));
        assert_eq!(drag.pointer(), (20., 20. + DRAG_THRESHOLD));
        drag.hover(Some(Drop::OntoAccount(k("b"))), now);
        assert_eq!(drag.over(), Some(&Drop::OntoAccount(k("b"))));
        assert_eq!(
            drag.release(),
            Released::Dropped(Dragged::Account(k("a")), Drop::OntoAccount(k("b")))
        );
        assert!(
            drag.swallow_click(),
            "the click after a drag is not a click"
        );
        assert!(!drag.swallow_click(), "asked once");
    }

    #[test]
    fn a_drag_let_go_over_nothing_or_cancelled_goes_back_home() {
        let now = std::time::Instant::now();
        let mut drag = DragState::default();
        // Let go outside every drop: nothing moves, the item returns.
        drag.press(Dragged::Account(k("a")), (20., 20.));
        drag.moved((300., 60.));
        drag.hover(None, now);
        assert_eq!(drag.release(), Released::Returned(Dragged::Account(k("a"))));
        assert_eq!(drag.pointer(), (300., 60.), "from where it was let go");
        assert!(drag.swallow_click());

        // Escape in the middle of a drag: the same, whatever it was over.
        drag.press(Dragged::Group(3), (20., 20.));
        drag.moved((20., 80.));
        drag.hover(Some(Drop::Top(0)), now);
        assert_eq!(drag.cancel(), Some(Dragged::Group(3)));
        assert_eq!(drag.dragging(), None);
        assert_eq!(drag.over(), None);
        assert_eq!(
            drag.release(),
            Released::Click,
            "the button coming up moves nothing"
        );
        // Escape with nothing held, or with a press that is not a drag
        // yet, is not for the rail.
        assert_eq!(DragState::default().cancel(), None);
        drag.press(Dragged::Group(3), (20., 20.));
        assert_eq!(drag.cancel(), None);
    }

    #[test]
    fn resting_on_a_closed_group_opens_it_after_a_dwell() {
        use std::time::Duration;
        let start = std::time::Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let dwell = Duration::from_millis(500);
        let closed = |_: u32| true;
        let mut drag = DragState::default();
        drag.press(Dragged::Account(k("a")), (20., 20.));
        drag.moved((20., 90.));

        drag.hover(Some(Drop::OntoGroup(7)), at(0));
        assert_eq!(drag.dwelt(at(499), dwell, closed), None, "not yet");
        // Still there on the next frames: the clock is not restarted.
        drag.hover(Some(Drop::OntoGroup(7)), at(300));
        assert_eq!(drag.over_since(), Some(at(0)));
        assert_eq!(drag.dwelt(at(500), dwell, closed), Some(7));
        assert_eq!(drag.dwelt(at(510), dwell, closed), None, "once per rest");

        // Passing over it does not open it.
        drag.hover(Some(Drop::Top(0)), at(600));
        drag.hover(Some(Drop::OntoGroup(7)), at(700));
        drag.hover(Some(Drop::Top(1)), at(900));
        assert_eq!(drag.dwelt(at(1300), dwell, closed), None);
        // A group that is already open, or a number, is not waited on.
        drag.hover(Some(Drop::OntoGroup(7)), at(2000));
        assert_eq!(drag.dwelt(at(3000), dwell, |_| false), None);
        drag.hover(Some(Drop::OntoAccount(k("b"))), at(3000));
        assert_eq!(drag.dwelt(at(4000), dwell, closed), None);
        // Nothing held: nothing opens.
        drag.release();
        assert_eq!(drag.dwelt(at(9000), dwell, closed), None);
    }

    #[test]
    fn no_room_opens_where_the_item_already_is() {
        let mut rail = layout(&["a", "b", "c", "d"]);
        drag(&mut rail, "c", Drop::OntoAccount(k("b")));
        assert_eq!(shape(&rail), "a [b c] d");
        let id = rail.groups()[0].id;
        let a = Dragged::Account(k("a"));
        assert!(stays_put(&rail, &a, &Drop::Top(0)));
        assert!(stays_put(&rail, &a, &Drop::Top(1)));
        assert!(!stays_put(&rail, &a, &Drop::Top(2)));
        assert!(!stays_put(&rail, &a, &Drop::OntoGroup(id)));
        let c = Dragged::Account(k("c"));
        assert!(stays_put(&rail, &c, &Drop::InGroup(id, 1)));
        assert!(stays_put(&rail, &c, &Drop::InGroup(id, 2)));
        assert!(!stays_put(&rail, &c, &Drop::InGroup(id, 0)));
        assert!(
            !stays_put(&rail, &c, &Drop::Top(1)),
            "out of its group is a move"
        );
        assert!(stays_put(&rail, &Dragged::Group(id), &Drop::Top(2)));
        assert!(!stays_put(&rail, &Dragged::Group(id), &Drop::Top(0)));
    }

    #[test]
    fn a_drag_near_an_edge_scrolls_the_rail() {
        assert_eq!(autoscroll(200., 50., 600.), 0.);
        assert!(autoscroll(55., 50., 600.) < 0.);
        assert!(autoscroll(595., 50., 600.) > 0.);
        // Faster the closer to the edge, and bounded.
        assert!(autoscroll(52., 50., 600.) < autoscroll(70., 50., 600.));
        assert_eq!(autoscroll(-500., 50., 600.), -SCROLL_EDGE / 2.);
    }

    #[test]
    fn a_group_shows_the_worst_state_of_its_members() {
        assert_eq!(worst([("ok", 0), ("down", 3), ("busy", 1)]), Some("down"));
        assert_eq!(worst::<&str>([]), None);
    }
}
