//! People in a conversation: the colour each one is told apart by, the
//! name line above their bubbles and the avatar beside them.
//!
//! A person's colour comes from who they are, not from the id a message
//! happened to name them by: the store keeps the ids of one person together
//! (`client_core`'s `identities`), and the colour is derived from the key
//! that stands for all of them (their number when it is known, else the
//! first of their ids). So somebody who writes under their number in one
//! place and under a hidden-number id in another has one colour, and has
//! it everywhere: the name on their bubbles, their avatar, the bar and the
//! name of a quote of theirs, their row in a group's panel.
//!
//! Asking the store is one read per person, kept until the address book
//! or a group changes ([`Senders::forget`]); a row that is drawn carries
//! what it needs (`MessageRow::sender`), so nothing is looked up per frame.

use super::bubble::{RowContext, Side};
use crate::format;
use crate::theme::px;
use crate::theme::{fonts, metrics, Palette, SENDER_TONES};
use client_core::{NameSource, Store};
use client_provider::{AccountId, ChatId, ContactId, Message};
use gpui_kit::prelude::*;
use gpui_kit::{div, img, Div, FontWeight, Image, ObjectFit, Pixels, SharedString, StyledImage};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Who a message is from, as far as drawing goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Sender {
    /// What stands for the person under all of their ids.
    pub key: Rc<str>,
    /// Which of the people colours is theirs: an index into
    /// `Palette::sender` and its companions.
    pub tone: usize,
    /// The name they are shown by is the one they gave themselves on
    /// WhatsApp (they are not in the address book, and the group does not
    /// name them): it is written `~Name`, as WhatsApp does.
    pub own_name: bool,
    /// Their number as people write it, when it is known and is not
    /// already what they are called.
    pub phone: Option<SharedString>,
}

/// Whose message a quote shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Quoted {
    /// Not among the messages at hand: drawn without a colour.
    #[default]
    Unknown,
    /// The account's own.
    Me,
    /// Somebody's, in their colour.
    Person(usize),
}

/// The colour of a person, from the key that stands for them: FNV-1a, so
/// it is the same on every run and every machine.
pub fn tone_of(key: &str) -> usize {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in key.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash as usize % SENDER_TONES
}

/// The key that stands for a person under all of their ids: their number
/// when one of the ids is one, else the first id in order. It is how the
/// store keys them too.
pub fn identity_key(ids: &[ContactId], asked: &ContactId) -> String {
    let mut all: Vec<&str> = ids.iter().map(ContactId::as_str).collect();
    all.push(asked.as_str());
    all.sort_unstable();
    all.iter()
        .copied()
        .find(|id| client_core::phone::is_e164(id))
        .unwrap_or(all[0])
        .to_owned()
}

type Asked = (AccountId, ChatId, ContactId);

/// Who people are, asked of the store once each.
pub struct Senders {
    store: Arc<Store>,
    known: RefCell<HashMap<Asked, Sender>>,
}

impl Senders {
    /// Over this store.
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            known: RefCell::default(),
        }
    }

    /// Who `id` is, as seen from `chat` (a group names its participants).
    pub fn of(&self, account: &AccountId, chat: Option<&ChatId>, id: &ContactId) -> Sender {
        let asked = (
            account.clone(),
            chat.cloned().unwrap_or_else(|| ChatId::new("")),
            id.clone(),
        );
        if let Some(known) = self.known.borrow().get(&asked) {
            return known.clone();
        }
        let person = self.store.person(account, chat, id).unwrap_or_default();
        let key = identity_key(&person.ids, id);
        let named = person.name.as_ref().map(|(name, _)| name.as_str());
        let phone = person
            .phone
            .as_deref()
            .map(format::phone)
            .filter(|phone| Some(phone.as_str()) != named)
            .map(SharedString::from);
        let sender = Sender {
            tone: tone_of(&key),
            key: key.into(),
            own_name: matches!(person.name, Some((_, NameSource::Profile))),
            phone,
        };
        self.known.borrow_mut().insert(asked, sender.clone());
        sender
    }

    /// The address book or a group changed: people are asked about again.
    pub fn forget(&self) {
        self.known.borrow_mut().clear();
    }
}

/// A person's round avatar: their picture with a ring of their colour, or
/// their initials on it. The same circle either way, so nothing moves when
/// the picture arrives.
pub fn person_avatar(
    picture: Option<Arc<Image>>,
    name: &str,
    size: Pixels,
    tone: usize,
    palette: &Palette,
) -> Div {
    let tone = tone % SENDER_TONES;
    let face = div().flex_none().size(size).rounded_full().border_1();
    match picture {
        Some(picture) => face
            .overflow_hidden()
            .border_color(palette.sender[tone])
            .bg(palette.muted)
            .debug_selector(|| "avatar-image".into())
            .child(
                img(picture)
                    .size_full()
                    .rounded_full()
                    .object_fit(ObjectFit::Cover),
            ),
        None => face
            .border_color(palette.sender_fill[tone])
            .bg(palette.sender_fill[tone])
            .flex()
            .items_center()
            .justify_center()
            .font_family(fonts::MONO)
            .text_color(palette.on_sender_fill)
            .text_size(size * 0.34)
            .font_weight(FontWeight::MEDIUM)
            .child(SharedString::from(format::initials(name))),
    }
}

/// The width the avatar column takes beside a group's incoming bubbles:
/// the avatar and the room after it.
pub fn avatar_column() -> Pixels {
    metrics::AVATAR_SMALL() + metrics::AVATAR_GAP()
}

/// What stands beside an incoming bubble of a group: the sender's avatar
/// on the last bubble of their run, level with its foot, and the same
/// room, empty, beside the others. A click opens the profile.
pub fn run_avatar(
    message: &Message,
    sender: &Sender,
    last_of_run: bool,
    context: &RowContext,
) -> Div {
    let column = div()
        .flex_none()
        .w(avatar_column())
        .flex()
        .flex_col()
        .justify_end();
    if !last_of_run {
        return column;
    }
    let name = message.sender_name.as_deref().unwrap_or("");
    let picture = context.shelf.avatar(
        &message.account_id,
        &ChatId::new(message.sender.as_str().to_owned()),
    );
    let view = context.view.clone();
    let (account, contact) = (message.account_id.clone(), message.sender.clone());
    column.child(
        div()
            .id(SharedString::from(format!("sender-avatar-{}", message.id)))
            .debug_selector(|| "sender-avatar".into())
            .size(metrics::AVATAR_SMALL())
            .cursor_pointer()
            .hover(|style| style.opacity(0.8))
            .on_click(move |_, window, cx| {
                view.update(cx, |shell, cx| {
                    shell.open_contact_profile(account.clone(), contact.clone(), window, cx)
                })
                .ok();
            })
            .child(person_avatar(
                picture,
                name,
                metrics::AVATAR_SMALL(),
                sender.tone,
                &context.palette,
            )),
    )
}

/// The line above the first bubble of somebody's run in a group: their
/// name in their colour, semibold and a size under the message, and after
/// it, muted, their number when the name is one they gave themselves. The
/// name is the way to their profile.
pub fn name_line(
    message: &Message,
    name: &str,
    sender: &Sender,
    side: &Side,
    context: &RowContext,
) -> Div {
    let colour = side.senders[sender.tone % SENDER_TONES];
    let shown: SharedString = if sender.own_name {
        format!("~{name}").into()
    } else {
        name.to_owned().into()
    };
    let view = context.view.clone();
    let (account, contact) = (message.account_id.clone(), message.sender.clone());
    div()
        .debug_selector(|| "sender-line".into())
        .pb(px(2.))
        .max_w_full()
        .flex()
        .items_baseline()
        .gap(px(8.))
        .text_size(metrics::TEXT_SMALL())
        .line_height(px(17.))
        .child(
            div()
                .id(SharedString::from(format!("sender-{}", message.id)))
                .debug_selector(|| "sender-name".into())
                .min_w_0()
                .truncate()
                .cursor_pointer()
                .hover(|style| style.opacity(0.75))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(colour)
                .on_click(move |_, window, cx| {
                    view.update(cx, |shell, cx| {
                        shell.open_contact_profile(account.clone(), contact.clone(), window, cx)
                    })
                    .ok();
                })
                .child(shown),
        )
        .when_some(
            sender.phone.clone().filter(|_| sender.own_name),
            |this, phone| {
                this.child(
                    div()
                        .debug_selector(|| "sender-number".into())
                        .flex_none()
                        .font_family(fonts::MONO)
                        .text_size(metrics::TEXT_META())
                        .text_color(side.meta)
                        .child(phone),
                )
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_has_one_colour_under_any_of_their_ids() {
        let number = ContactId::new("+351912345678");
        let hidden = ContactId::new("lid:200055501000001");
        let both = [hidden.clone(), number.clone()];
        // Asked by the number or by the hidden id: the same key, so the
        // same colour.
        let by_number = identity_key(&both, &number);
        let by_hidden = identity_key(&both, &hidden);
        assert_eq!(by_number, "+351912345678");
        assert_eq!(by_number, by_hidden);
        assert_eq!(tone_of(&by_number), tone_of(&by_hidden));
        // Nothing known but the id: the id is the key.
        assert_eq!(identity_key(&[], &hidden), "lid:200055501000001");
        // Without a number, the first id in order, whichever was asked.
        let two = [ContactId::new("lid:9"), ContactId::new("lid:1")];
        assert_eq!(identity_key(&two, &two[0]), "lid:1");
        assert_eq!(identity_key(&two, &two[1]), "lid:1");
    }

    #[test]
    fn colours_are_stable_and_spread_over_the_palette() {
        // The same on every run: these are fixed points of the hash.
        assert_eq!(tone_of("+351912345678"), tone_of("+351912345678"));
        let mut used = [0usize; SENDER_TONES];
        for n in 0..900 {
            used[tone_of(&format!("+3519{n:08}"))] += 1;
        }
        // Every colour is used, none by more than twice its share.
        for (tone, count) in used.iter().enumerate() {
            assert!((40..=200).contains(count), "tone {tone}: {count} of 900");
        }
    }
}
