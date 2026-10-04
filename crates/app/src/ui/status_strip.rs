//! The strip of status updates above the chats.
//!
//! "My status" first, with a "+" to post one, then everybody who has
//! something new: their picture in its ring, their name under it. A click
//! on somebody opens Status with their stories playing. It is what makes
//! Status visible to somebody who never goes looking for it: it is there
//! whenever the provider has stories at all, and says in a few words why
//! nobody is in it when nobody is.
//!
//! Drawing the strip is only drawing: nothing here marks a story as seen,
//! tells an author anything or fetches a story's file. The pictures are
//! the profile pictures the chat list asks for too.
//!
//! It scrolls sideways, and only the cells in view (and a few beside
//! them) are built: the rest is room of the same width. It folds to one
//! line, and stays as it was left (`Settings::story_strip`).

use super::shell::Shell;
use super::status::{tooltip_with_keys, Updates};
use super::widgets::{avatar_or, label, mono, AvatarKind};
use crate::icons::{icon, IconName};
use crate::keys::Command;
use crate::theme::{metrics, px, story, Palette};
use client_core::{StoryAuthor, StoryRing};
use client_provider::AccountId;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::prelude::*;
use gpui_kit::{div, AnyElement, Context, Div, FontWeight, Pixels, SharedString, Stateful};

/// How many cells beside the ones in view are built as well, on each
/// side: a scroll shows them before the next frame.
const BESIDE: usize = 2;

/// The authors of `count` to build: the ones the strip shows at this
/// scroll position, and a few beside them. `scrolled` is how far the
/// strip was scrolled, `lead` the room before the first author (the
/// account's own cell).
pub(super) fn window_of(
    count: usize,
    scrolled: Pixels,
    lead: Pixels,
    cell: Pixels,
    width: Pixels,
) -> std::ops::Range<usize> {
    if count == 0 || cell <= gpui_kit::px(0.) {
        return 0..0;
    }
    let first = (((scrolled - lead) / cell).floor().max(0.) as usize).saturating_sub(BESIDE);
    let shown = (width / cell).ceil().max(1.) as usize + 2 * BESIDE + 1;
    let first = first.min(count);
    first..(first + shown).min(count)
}

/// What the strip says in place of people, when nobody has anything new.
fn note_of(updates: Updates) -> &'static str {
    match updates {
        Updates::Listed | Updates::Empty => "No new updates",
        Updates::Loading => "Checking for updates…",
        Updates::Offline => "Offline: updates come back with the connection",
        Updates::Unavailable => "Updates are not available for this number yet",
        Updates::NotOffered => "Contacts' updates are not available from this provider",
        Updates::Failed => "Could not check for updates",
    }
}

impl Shell {
    /// The strip, where the provider has stories at all.
    pub(super) fn render_story_strip(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !self.has_status() {
            return None;
        }
        Some(if crate::settings::get(cx).story_strip {
            self.render_strip_open(palette, cx).into_any_element()
        } else {
            self.render_strip_folded(palette, cx).into_any_element()
        })
    }

    /// One line: that there is a strip, how many people have something
    /// new, and the way to open it.
    fn render_strip_folded(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let people = self.status.feed.recent.len();
        let hover = palette.hover;
        div()
            .id("story-strip")
            .debug_selector(|| "story-strip".into())
            .flex_none()
            .h(story::STRIP_FOLDED())
            .pl_4()
            .pr_2()
            .border_b_1()
            .border_color(palette.border)
            .flex()
            .items_center()
            .justify_between()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(tooltip_with_keys(
                "Show status updates",
                Command::ToggleStatusStrip,
            ))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_status_strip(cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Status", palette))
                    .when(people > 0, |this| {
                        this.child(
                            div()
                                .debug_selector(|| "story-strip-count".into())
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .child(
                                    div()
                                        .flex_none()
                                        .size(px(7.))
                                        .rounded_full()
                                        .bg(palette.accent),
                                )
                                .child(mono(format!("{people} new")).text_color(palette.text)),
                        )
                    }),
            )
            .child(
                div()
                    .debug_selector(|| "story-strip-unfold".into())
                    .flex_none()
                    .size(metrics::CONTROL())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icon(IconName::ChevronDown, px(14.), palette.text_faint)),
            )
    }

    fn render_strip_open(&self, palette: &Palette, cx: &mut Context<Self>) -> Div {
        let cell = story::STRIP_CELL();
        let recent = &self.status.feed.recent;
        let scrolled = -self.status.strip_scroll.offset().x;
        let shown = window_of(recent.len(), scrolled, cell, cell, self.list_px);
        let account = self.account.clone().unwrap_or_else(|| AccountId::new(""));

        // The strip itself is the row that scrolls: what it holds is as
        // long as its people, and only the ones in view are in it.
        let mut cells = div()
            .id("story-strip-scroll")
            .debug_selector(|| "story-strip-scroll".into())
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .overflow_x_scroll()
            .track_scroll(&self.status.strip_scroll)
            .child(self.render_strip_mine(palette, cx));
        if recent.is_empty() {
            cells = cells.child(self.render_strip_note(palette, cx));
        } else {
            // Room for the people before the ones in view, as wide as
            // they are, so the strip is as long as its people.
            cells = cells.child(div().flex_none().w(cell * shown.start as f32));
            for index in shown.clone() {
                cells = cells.child(self.render_strip_author(
                    index,
                    &recent[index],
                    &account,
                    palette,
                    cx,
                ));
            }
            cells = cells.child(
                div()
                    .flex_none()
                    .w(cell * (recent.len() - shown.end) as f32),
            );
        }

        let hover = palette.muted;
        div()
            .debug_selector(|| "story-strip".into())
            .flex_none()
            .relative()
            .h(story::STRIP())
            .border_b_1()
            .border_color(palette.border)
            .flex()
            .child(cells)
            .child(
                // Folding it away, in the corner: small, and out of the
                // people's way.
                div()
                    .id("story-strip-fold")
                    .debug_selector(|| "story-strip-fold".into())
                    .absolute()
                    .top(px(2.))
                    .right(px(2.))
                    .size(px(18.))
                    .rounded(metrics::RADIUS())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .bg(palette.background)
                    .hover(move |style| style.bg(hover))
                    .tooltip(tooltip_with_keys(
                        "Fold status updates",
                        Command::ToggleStatusStrip,
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_status_strip(cx);
                    }))
                    .child(icon(IconName::ChevronUp, px(12.), palette.text_faint)),
            )
    }

    /// A cell of the strip: a picture in its ring, and a name under it.
    fn strip_cell(face: Div, name: SharedString, strong: bool, palette: &Palette) -> Div {
        div()
            .flex_none()
            .w(story::STRIP_CELL())
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(3.))
            .child(face)
            .child(
                div()
                    .w_full()
                    .px(px(4.))
                    .text_center()
                    .truncate()
                    .text_size(metrics::TEXT_META())
                    .text_color(if strong {
                        palette.text
                    } else {
                        palette.text_muted
                    })
                    .when(strong, |this| this.font_weight(FontWeight::MEDIUM))
                    .child(name),
            )
    }

    /// "My status": the account's own picture, with a ring when it has
    /// something up, and a "+" where a status can be posted.
    fn render_strip_mine(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let mine = self.status.feed.mine.len();
        let picture = self.account.as_ref().and_then(|account| {
            self.accounts
                .iter()
                .find(|known| &known.id == account)
                .and_then(client_core::own_picture_subject)
                .and_then(|subject| self.media.stored_picture(account, &subject))
        });
        let name = self.current_account_name().unwrap_or_default();
        let face = avatar_or(
            picture,
            &name,
            AvatarKind::Person,
            story::STRIP_AVATAR(),
            palette,
        );
        let ring = (mine > 0).then_some(StoryRing {
            total: mine,
            // Your own are never news to you.
            unviewed: 0,
        });
        let can_post = self.caps_now().story_post;
        let face = div()
            .relative()
            .child(super::story_ring::with_ring(face, ring, palette))
            .when(can_post, |this| {
                this.child(
                    div()
                        .id("story-strip-add")
                        .debug_selector(|| "story-strip-add".into())
                        .absolute()
                        .right_0()
                        .bottom_0()
                        .size(story::BADGE())
                        .rounded_full()
                        .border_1()
                        .border_color(palette.background)
                        .bg(palette.accent_fill)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .tooltip(tooltip_with_keys("New status", Command::NewStatus))
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_status_post(window, cx);
                        }))
                        .child(icon(IconName::Plus, px(10.), palette.on_accent_fill)),
                )
            });
        let hover = palette.hover;
        Self::strip_cell(face, "My status".into(), false, palette)
            .id("story-strip-mine")
            .debug_selector(|| "story-strip-mine".into())
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .tooltip(move |window, cx| {
                Tooltip::new(if mine > 0 {
                    "My status"
                } else {
                    "Add to my status"
                })
                .build(window, cx)
            })
            .on_click(cx.listener(|this, _, window, cx| this.open_my_status(window, cx)))
    }

    /// Somebody with something new.
    fn render_strip_author(
        &self,
        index: usize,
        author: &StoryAuthor,
        account: &AccountId,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let name = author.display_name();
        let ring = StoryRing {
            total: author.stories.len(),
            unviewed: author.unviewed(),
        };
        let picture = self.media.avatar(
            account,
            &client_provider::ChatId::new(author.author.as_str()),
        );
        let tone = self.senders.of(account, None, &author.author).tone;
        let face =
            super::senders::person_avatar(picture, &name, story::STRIP_AVATAR(), tone, palette);
        let key = author.key.clone();
        let hover = palette.hover;
        Self::strip_cell(
            super::story_ring::with_ring(face, Some(ring), palette),
            name.into(),
            true,
            palette,
        )
        .id(("story-strip-author", index))
        .debug_selector(move || format!("story-strip-author-{index}"))
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .tooltip(|window, cx| Tooltip::new("Watch their status").build(window, cx))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.watch_status_key(&key, window, cx);
        }))
    }

    /// Nobody has anything new: why, in a few words, and the way to
    /// Status, where it is said in full with what to do.
    fn render_strip_note(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let updates = self.status_updates();
        let name = updates.name();
        let ink = palette.text;
        div()
            .id("story-strip-note")
            .debug_selector(|| "story-strip-note".into())
            .flex_none()
            .h_full()
            .pl_2()
            .pr_6()
            .flex()
            .items_center()
            .cursor_pointer()
            .text_size(metrics::TEXT_SMALL())
            .text_color(palette.text_muted)
            .hover(move |style| style.text_color(ink))
            .tooltip(tooltip_with_keys("Status", Command::ShowStatus))
            .on_click(cx.listener(|this, _, window, cx| {
                this.set_list_mode(super::status::ListMode::Status, window, cx)
            }))
            .child(
                div()
                    .debug_selector(move || format!("story-strip-{name}"))
                    .child(note_of(updates)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::window_of;
    use gpui_kit::px;

    #[test]
    fn only_the_people_in_view_and_a_few_beside_them_are_built() {
        let (cell, lead, width) = (px(60.), px(60.), px(300.));
        // At the start: the first ones.
        assert_eq!(window_of(1000, px(0.), lead, cell, width), 0..10);
        // Scrolled far: the ones there, two before them.
        let far = window_of(1000, px(60. * 501.), lead, cell, width);
        assert_eq!(far, 498..508);
        // The end of the list is the end.
        assert_eq!(window_of(4, px(0.), lead, cell, width), 0..4);
        assert_eq!(
            window_of(1000, px(60. * 2000.), lead, cell, width),
            1000..1000
        );
        assert_eq!(window_of(0, px(0.), lead, cell, width), 0..0);
        // Never more than a window's worth, however many there are.
        for scrolled in [0., 300., 12_345., 59_000.] {
            let built = window_of(1000, px(scrolled), lead, cell, width);
            assert!(built.len() <= 10, "{built:?}");
        }
    }
}
