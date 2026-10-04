//! The account's own status: each story with who saw it, their
//! reactions, and what can be done with it (watch it, pass it on, take it
//! down). A story that is still on its way out says how far it is, and a
//! failed one can be tried again or given up.

use super::shell::Shell;
use super::status::{tooltip_with_keys, Main};
use super::status_post::{with_face, Sheet};
use super::widgets::{icon_button, label, mono, text_button};
use crate::icons::{icon, IconName};
use crate::keys::Command;
use crate::stories::{age_label, describe, expires_label, expiring_soon, face_of, SlideKind};
use crate::theme::{metrics, px, story, Palette};
use client_core::{StoryItem, StoryPostState};
use client_provider::{AccountId, ChatId, MessageId, StoryBody, Timestamp};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, FontWeight, SharedString, Stateful};

/// How many people are listed under a story before "and N more".
const VIEWERS_LISTED: usize = 12;

impl Shell {
    /// Shows the account's own status in the main pane.
    pub(super) fn open_mine(&mut self, story: Option<MessageId>, cx: &mut Context<Self>) {
        self.close_story_viewer(cx);
        self.status.main = Main::Mine;
        self.status.mine_open = story;
        if let Some(account) = self.account.clone() {
            // Who saw each of the last stories, asked behind the page.
            for item in self.status.feed.mine.iter().rev().take(10) {
                if item.post.is_none() {
                    self.engine.want_story_viewers(&account, &item.story.id);
                }
            }
        }
        cx.notify();
    }

    /// Takes one of the account's stories down: gone at once, told to the
    /// provider through drops, put back if it refuses.
    pub(super) fn delete_my_story(
        &mut self,
        account: AccountId,
        story: MessageId,
        cx: &mut Context<Self>,
    ) {
        self.engine.delete_story(&account, &story);
        cx.notify();
    }

    /// Passes a status on to a chat, through the outbox, the way a
    /// message is: its words as a text, anything else by naming it when
    /// the provider forwards that way.
    pub(super) fn forward_story(
        &mut self,
        story: &MessageId,
        chat: ChatId,
        cx: &mut Context<Self>,
    ) {
        let Some(account) = self.account.clone() else {
            return;
        };
        if let Err(error) = self.engine.forward_story(&account, story, &chat) {
            self.show_problem(error.to_string(), cx);
        }
    }

    /// The pane of the account's own status.
    pub(super) fn render_mine(&self, palette: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let caps = self.caps_now();
        let now = Timestamp::now();
        let account = self.account.clone().unwrap_or_else(|| AccountId::new(""));
        let mut list = div().flex().flex_col().gap_3();
        let mine: Vec<&StoryItem> = self.status.feed.mine.iter().rev().collect();
        for (index, item) in mine.iter().enumerate() {
            list = list.child(self.render_mine_card(index, item, &account, now, palette, cx));
        }
        if mine.is_empty() {
            list = list.child(
                div()
                    .debug_selector(|| "mine-empty".into())
                    .py_6()
                    .text_size(metrics::TEXT_BODY())
                    .text_color(palette.text_muted)
                    .child("You have no status right now. What you post stays a day."),
            );
        }
        div()
            .id("status-mine-pane")
            .debug_selector(|| "status-mine-pane".into())
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(metrics::HEADER_HEIGHT())
                    .pl_4()
                    .pr_2()
                    .border_b_1()
                    .border_color(palette.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(metrics::TEXT_TITLE())
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("My status"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .child(if caps.story_privacy {
                                icon_button("status-audience", IconName::Lock, palette)
                                    .tooltip(tooltip_with_keys(
                                        "Who sees my status",
                                        Command::SettingsStatus,
                                    ))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_audience(window, cx)
                                    }))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            .child(if caps.story_post {
                                icon_button("status-new-mine", IconName::Plus, palette)
                                    .tooltip(tooltip_with_keys("New status", Command::NewStatus))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.open_status_post(window, cx)
                                    }))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    ),
            )
            .child(
                div()
                    .id("mine-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p_4()
                    .child(
                        div()
                            .max_w(px(560.))
                            .mx_auto()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(list)
                            .when(!caps.story_viewers, |this| {
                                this.child(
                                    div()
                                        .debug_selector(|| "mine-viewers-unavailable".into())
                                        .text_size(metrics::TEXT_SMALL())
                                        .text_color(palette.text_faint)
                                        .child(
                                            "Who saw your status is not available yet from this \
                                             provider.",
                                        ),
                                )
                            }),
                    ),
            )
    }

    fn render_mine_card(
        &self,
        index: usize,
        item: &StoryItem,
        account: &AccountId,
        now: Timestamp,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let caps = self.caps_now();
        let id = item.story.id.clone();
        let words = describe(&item.story.body);
        let preview = match &item.story.body {
            StoryBody::Text { style, .. } => with_face(
                div()
                    .size(px(56.))
                    .flex_none()
                    .rounded(px(6.))
                    .bg(story::background(style.background))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(story::on_background())
                    .text_size(px(14.)),
                face_of(style.font),
            )
            .child("Aa"),
            StoryBody::Media(_) => div()
                .size(px(56.))
                .flex_none()
                .rounded(px(6.))
                .bg(palette.surface)
                .border_1()
                .border_color(palette.border)
                .flex()
                .items_center()
                .justify_center()
                .child(icon(
                    match SlideKind::of(&item.story.body) {
                        SlideKind::Video => IconName::Video,
                        SlideKind::Voice => IconName::Mic,
                        _ => IconName::Image,
                    },
                    px(20.),
                    palette.icon,
                )),
        };
        let status_line: SharedString = match &item.post {
            Some(StoryPostState::Queued) => {
                let progress = item
                    .story
                    .client_id
                    .as_ref()
                    .and_then(|client| self.engine.story_upload_progress(client));
                match progress {
                    Some((sent, total)) if total > 0 => {
                        format!("Uploading {}%", sent * 100 / total).into()
                    }
                    _ => "Posting…".into(),
                }
            }
            Some(StoryPostState::Failed(reason)) => format!("Not posted: {reason}").into(),
            None => {
                let left = expires_label(item.story.expiry(), now).unwrap_or_default();
                format!("{} · {left}", age_label(item.story.posted_at, now)).into()
            }
        };
        let soon = item.post.is_none() && expiring_soon(item.story.expiry(), now);
        let viewers = if item.post.is_none() && caps.story_viewers {
            self.engine
                .store()
                .story_viewers(account, &id)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let count = item.story.view_count.unwrap_or(viewers.len() as u32);
        let client = item.story.client_id.clone();
        let mut card = div()
            .id(("mine-card", index))
            .debug_selector(move || format!("mine-card-{index}"))
            .p_3()
            .rounded(metrics::PANEL_RADIUS())
            .border_1()
            .border_color(palette.border)
            .bg(palette.surface)
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div().flex().items_center().gap_3().child(preview).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .truncate()
                                .text_size(metrics::TEXT_BODY())
                                .child(SharedString::from(words)),
                        )
                        .child(
                            div()
                                .debug_selector(move || format!("mine-line-{index}"))
                                .truncate()
                                .text_size(metrics::TEXT_SMALL())
                                .text_color(if soon {
                                    palette.warning
                                } else if matches!(item.post, Some(StoryPostState::Failed(_))) {
                                    palette.danger
                                } else {
                                    palette.text_muted
                                })
                                .child(status_line),
                        ),
                ),
            );
        // How many saw it, and who: names and times, with the reactions.
        if item.post.is_none() && caps.story_viewers {
            card = card.child(label(
                &match count {
                    1 => "Seen by 1".to_owned(),
                    n => format!("Seen by {n}"),
                },
                palette,
            ));
            for (row, viewer) in viewers.iter().take(VIEWERS_LISTED).enumerate() {
                card =
                    card.child(
                        div()
                            .debug_selector(move || format!("mine-viewer-{index}-{row}"))
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(metrics::TEXT_BODY())
                                    .child(SharedString::from(viewer.name.clone().unwrap_or_else(
                                        || client_core::UNKNOWN_PERSON.to_owned(),
                                    ))),
                            )
                            .children(viewer.reaction.clone().map(|emoji| {
                                div()
                                    .debug_selector(move || format!("mine-reaction-{index}-{row}"))
                                    .child(SharedString::from(emoji))
                            }))
                            .child(
                                mono(age_label(viewer.viewed_at, now))
                                    .text_color(palette.text_faint),
                            ),
                    );
            }
            if viewers.len() > VIEWERS_LISTED {
                card = card.child(
                    div()
                        .text_size(metrics::TEXT_SMALL())
                        .text_color(palette.text_muted)
                        .child(SharedString::from(format!(
                            "and {} more",
                            viewers.len() - VIEWERS_LISTED
                        ))),
                );
            }
        }
        // What can be done with it.
        let mut actions = div().flex().flex_wrap().gap_2();
        match &item.post {
            None => {
                let watch = id.clone();
                actions = actions.child(
                    text_button("mine-watch", "Watch", Some(IconName::Play), false, palette)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_own_story_viewer(Some(watch.clone()), window, cx)
                        })),
                );
                if caps.forwards {
                    let pass = id.clone();
                    actions = actions.child(
                        text_button(
                            "mine-forward",
                            "Forward",
                            Some(IconName::Forward),
                            false,
                            palette,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.status.sheet = Some(Sheet::Forward(pass.clone()));
                                this.open_overlay(super::shell::Overlay::Status, window, cx);
                            },
                        )),
                    );
                }
                if caps.story_delete {
                    let gone = id.clone();
                    actions = actions.child(
                        text_button(
                            "mine-delete",
                            "Delete",
                            Some(IconName::Trash),
                            false,
                            palette,
                        )
                        .text_color(palette.danger)
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.status.sheet = Some(Sheet::ConfirmDelete(gone.clone()));
                                this.open_overlay(super::shell::Overlay::Status, window, cx);
                            },
                        )),
                    );
                }
            }
            Some(StoryPostState::Failed(_)) => {
                if let Some(client) = client.clone() {
                    let again = client.clone();
                    actions = actions
                        .child(
                            text_button(
                                "mine-retry",
                                "Try again",
                                Some(IconName::RotateCw),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let _ = this.engine.retry_story_post(&again);
                                    cx.notify();
                                },
                            )),
                        )
                        .child(
                            text_button(
                                "mine-discard",
                                "Discard",
                                Some(IconName::Trash),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    let _ = this.engine.discard_story_post(&client);
                                    cx.notify();
                                },
                            )),
                        );
                }
            }
            Some(StoryPostState::Queued) => {}
        }
        card.child(actions)
    }
}
