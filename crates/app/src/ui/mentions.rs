//! Mentioning somebody from the composer: `@` in a group lists its
//! participants, and a pick puts the person's name in the text. What is
//! sent is decided in `crate::mentioning`.
//!
//! "The composer" is whichever field is being written in: the
//! conversation's, the caption of what is being attached, or a field of
//! Status (the reply under a story, the words or the caption of a post)
//! (`Shell::writing`). Everything here, and the emoji that complete a
//! shortcode, goes by that, so those fields can do what the composer can.
//! People are mentioned in a group's conversation only: a field of Status
//! is not written in a chat, so it lists nobody.

use super::senders::person_avatar;
use super::shell::Shell;
use crate::mentioning::{self, Picked};
use crate::theme::{metrics, px, Palette};
use client_core::StoredParticipant;
use client_provider::ChatKind;
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, KeyDownEvent, SharedString, Window};

/// How many people the list shows at once.
const SHOWN: usize = 6;

/// What the composer remembers about the mentions in its text.
#[derive(Default)]
pub(super) struct Mentioning {
    /// The people picked so far, by the names the text shows them with.
    pub(super) picked: Vec<Picked>,
    /// The row of the list the keyboard is on.
    pub(super) cursor: usize,
    /// The text the list was closed at with Escape: it stays closed until
    /// the text changes.
    pub(super) dismissed: Option<String>,
}

impl Mentioning {
    /// The composer was emptied, or another chat was opened.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
}

impl Shell {
    /// Whether what is being written is the caption of the file shown in
    /// the attach sheet (also while the emoji picker it opened is over it).
    pub(super) fn captioning(&self) -> bool {
        self.attach
            .as_ref()
            .and_then(|draft| draft.current())
            .is_some_and(|item| item.caption.is_some() && item.blocked.is_none())
            && (self.overlay == super::shell::Overlay::AttachSheet
                || (self.overlay == super::shell::Overlay::EmojiPicker
                    && self.emoji.target == super::emoji_picker::Target::Caption))
    }

    /// The field of Status being written in, when it is one of them:
    /// the words or the caption of the post sheet (by its tab), or the
    /// reply under the story on screen. Also while the emoji picker one
    /// of them opened is over it.
    pub(super) fn status_writing(
        &self,
    ) -> Option<&gpui_kit::Entity<gpui_kit::component::input::TextareaState>> {
        use super::shell::Overlay;
        let picking = self.overlay == Overlay::EmojiPicker
            && self.emoji.target == super::emoji_picker::Target::Status;
        if self.status.sheet == Some(super::status_post::Sheet::Post) {
            let field = match self.status.draft.tab {
                super::status_post::PostTab::Text => &self.status.words,
                super::status_post::PostTab::Media => &self.status.caption,
            };
            return (self.overlay == Overlay::Status || picking).then_some(field);
        }
        (self.story_is_open() && (self.overlay == Overlay::None || picking))
            .then_some(&self.status.reply)
    }

    /// The field being written in: a field of Status while it is the one
    /// in hand, the caption of the file on show while files are being
    /// attached, the conversation's composer otherwise.
    pub(super) fn writing(&self) -> &gpui_kit::Entity<gpui_kit::component::input::TextareaState> {
        if let Some(field) = self.status_writing() {
            return field;
        }
        match self
            .attach
            .as_ref()
            .and_then(|draft| draft.current())
            .and_then(|item| item.caption.as_ref())
            .filter(|_| self.captioning())
        {
            Some(caption) => caption,
            None => &self.composer,
        }
    }

    /// The mentions of the field being written in.
    pub(super) fn mentions(&self) -> &Mentioning {
        if self.captioning() {
            if let Some(item) = self.attach.as_ref().and_then(|draft| draft.current()) {
                return &item.mentioning;
            }
        }
        &self.mentioning
    }

    fn mentions_mut(&mut self) -> &mut Mentioning {
        if self.captioning() {
            if let Some(item) = self
                .attach
                .as_mut()
                .and_then(|draft| draft.items.get_mut(draft.selected))
            {
                return &mut item.mentioning;
            }
        }
        &mut self.mentioning
    }

    /// Whether a text sent from here can mention people.
    fn can_mention(&self) -> bool {
        self.engine.capabilities().mentions
            // A field of Status is not a group's conversation.
            && self.status_writing().is_none()
            && self
                .open
                .as_ref()
                .is_some_and(|open| open.chat.kind == ChatKind::Group)
    }

    /// The composer's text up to the caret, and what follows it.
    fn around_caret(&self, cx: &gpui_kit::App) -> (String, String) {
        let composer = self.writing().read(cx);
        let text = composer.value().to_string();
        let mut caret = composer.cursor().min(text.len());
        while !text.is_char_boundary(caret) {
            caret -= 1;
        }
        (text[..caret].to_owned(), text[caret..].to_owned())
    }

    /// The participants the `@` being typed matches, the account itself
    /// left out. Empty when no mention is being typed.
    pub(super) fn mention_matches(&self, cx: &gpui_kit::App) -> Vec<StoredParticipant> {
        let Some(open) = self.open.as_ref().filter(|_| self.can_mention()) else {
            return Vec::new();
        };
        let (text, _) = self.around_caret(cx);
        if self.mentions().dismissed.as_deref() == Some(text.as_str()) {
            return Vec::new();
        }
        let Some(query) = mentioning::typing(&text) else {
            return Vec::new();
        };
        let query = Some(query).filter(|query| !query.is_empty());
        self.engine
            .store()
            .group_participants(&open.chat.account_id, &open.chat.id, query, SHOWN * 4)
            .unwrap_or_default()
            .into_iter()
            .filter(|participant| !participant.me)
            .take(SHOWN)
            .collect()
    }

    /// Puts the person at `index` of the list in the text.
    pub(super) fn pick_mention(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(person) = self.mention_matches(cx).into_iter().nth(index) else {
            return;
        };
        let name = crate::format::phone(&person.name);
        let (before, after) = self.around_caret(cx);
        let before = mentioning::insert(&before, &name);
        let handle = self.engine.mention_handle(&person.contact);
        let mentions = self.mentions_mut();
        mentions
            .picked
            .retain(|picked| picked.name != name || picked.id == person.contact);
        if !mentions.picked.iter().any(|picked| picked.name == name) {
            mentions.picked.push(Picked {
                name,
                handle,
                id: person.contact,
            });
        }
        mentions.cursor = 0;
        // What follows the caret is put back first and the rest typed in
        // front of it, which leaves the caret right after the name.
        self.writing().clone().update(cx, |composer, cx| {
            composer.set_value(after, window, cx);
            composer.insert(before, window, cx);
            composer.focus(window, cx);
        });
        cx.notify();
    }

    /// Enter in the composer while the list is open picks; returns
    /// whether it did.
    pub(super) fn pick_mention_under_cursor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count = self.mention_matches(cx).len();
        if count == 0 {
            return false;
        }
        self.pick_mention(self.mentions().cursor.min(count - 1), window, cx);
        true
    }

    /// The keys the list takes before the text field sees them: the
    /// arrows move, Tab picks, Escape closes.
    pub(super) fn mention_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let count = self.mention_matches(cx).len();
        if count == 0 {
            return false;
        }
        let cursor = self.mentions().cursor;
        match event.keystroke.key.as_str() {
            "down" => self.mentions_mut().cursor = (cursor + 1) % count,
            "up" => self.mentions_mut().cursor = (cursor + count - 1) % count,
            "tab" => self.pick_mention(cursor.min(count - 1), window, cx),
            "escape" => {
                let closed_at = self.around_caret(cx).0;
                self.mentions_mut().dismissed = Some(closed_at);
            }
            _ => return false,
        }
        cx.stop_propagation();
        cx.notify();
        true
    }

    /// The text changed: the list starts over.
    pub(super) fn mention_text_changed(&mut self) {
        let mentions = self.mentions_mut();
        mentions.cursor = 0;
        mentions.dismissed = None;
    }

    /// The list of people over the composer, while a mention is typed.
    pub(super) fn render_mention_picker(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let people = self.mention_matches(cx);
        if people.is_empty() {
            return None;
        }
        let cursor = self.mentions().cursor.min(people.len() - 1);
        let mut list = div()
            .debug_selector(|| "mention-picker".into())
            .mb(px(6.))
            .p(px(4.))
            .rounded(metrics::RADIUS())
            .border_1()
            .border_color(palette.elevated_border)
            .bg(palette.elevated)
            .flex()
            .flex_col();
        for (index, person) in people.into_iter().enumerate() {
            let name = crate::format::phone(&person.name);
            let detail = person
                .phone
                .as_deref()
                .map(crate::format::phone)
                .filter(|phone| *phone != name);
            let hover = palette.hover;
            // In the colour their messages carry in this group.
            let tone = self.open.as_ref().map_or(0, |open| {
                self.senders
                    .of(&open.chat.account_id, Some(&open.chat.id), &person.contact)
                    .tone
            });
            list =
                list.child(
                    div()
                        .id(("mention", index))
                        .debug_selector(move || format!("mention-{index}"))
                        .px_2()
                        .h(px(36.))
                        .rounded(px(4.))
                        .flex()
                        .items_center()
                        .gap_2()
                        .cursor_pointer()
                        .when(index == cursor, |this| this.bg(palette.hover))
                        .hover(move |style| style.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.pick_mention(index, window, cx)
                        }))
                        .child(person_avatar(None, &name, px(24.), tone, palette))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(metrics::TEXT_SMALL())
                                .child(SharedString::from(name)),
                        )
                        .children(detail.map(|detail| {
                            div()
                                .flex_none()
                                .text_size(metrics::TEXT_META())
                                .text_color(palette.text_muted)
                                .child(SharedString::from(detail))
                        })),
                );
        }
        Some(list)
    }
}
