//! The chat list pane: title, search, filters and the virtualised list.

use super::shell::{ChatFilter, ListRow, Overlay, Shell};
use super::social::Target;
use super::widgets::{
    avatar_or, icon_button, label, mono, status_tick, unavailable_button, AvatarKind,
};
use crate::format::list_time;
use crate::icons::{icon, IconName};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::{message_preview, ChatSummary, SearchHit};
use client_provider::{ChatKind, Direction, PresenceState, Timestamp};
use gpui_kit::component::input::Input;
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, uniform_list, AnyElement, ClickEvent, Context, FontWeight, MouseButton, MouseDownEvent,
    SharedString,
};

/// The second line of a community's row: how many groups it links, when
/// that is known.
pub(super) fn community_subtitle(groups: Option<usize>) -> String {
    match groups {
        Some(1) => "Community · 1 group".to_owned(),
        Some(count) => format!("Community · {count} groups"),
        None => "Community".to_owned(),
    }
}

impl Shell {
    pub(super) fn render_chat_list(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let row_count = self.list_rows.len();
        let palette = *palette;

        let (grip, held) = (palette.accent, self.resizing_list());
        div()
            .flex_none()
            .relative()
            .w(self.list_px)
            .h_full()
            .bg(palette.background)
            .border_r_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            // The pane's edge: drag it to resize the list, double-click to
            // let it follow the window again.
            .child(
                div()
                    .id("list-resize")
                    .debug_selector(|| "list-resize".into())
                    .absolute()
                    .top_0()
                    .right_0()
                    .w(gpui_kit::px(5.))
                    .h_full()
                    .cursor_col_resize()
                    .when(held, |this| this.bg(grip.opacity(0.4)))
                    .hover(move |style| style.bg(grip.opacity(0.4)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui_kit::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.grab_list_edge(event.click_count, cx);
                        }),
                    ),
            )
            .child(self.render_list_header(&palette, cx))
            .children(self.render_storage_banner(&palette))
            .children(self.problem.clone().map(|problem| {
                div()
                    .flex_none()
                    .debug_selector(|| "problem".into())
                    .px_4()
                    .py(px(10.))
                    .border_b_1()
                    .border_color(palette.border)
                    .bg(palette.muted)
                    .flex()
                    .gap_2()
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(18.))
                    .text_color(palette.text)
                    .child(
                        div()
                            .flex_none()
                            .mt(px(5.))
                            .size(px(8.))
                            .rounded_full()
                            .bg(palette.danger),
                    )
                    .child(div().flex_1().min_w_0().child(problem))
            }))
            .children(self.leaving.notice.clone().map(|notice| {
                // Something went well: said like a problem, with the
                // accent's light and, for a saved file, the way to it.
                div()
                    .flex_none()
                    .debug_selector(|| "notice".into())
                    .px_4()
                    .py(px(10.))
                    .border_b_1()
                    .border_color(palette.border)
                    .bg(palette.muted)
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(metrics::TEXT_SMALL())
                    .line_height(px(18.))
                    .text_color(palette.text)
                    .child(
                        div()
                            .flex_none()
                            .size(px(8.))
                            .rounded_full()
                            .bg(palette.accent),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(notice.text))
                    .when(notice.reveal.is_some(), |this| {
                        this.child(
                            div()
                                .id("notice-reveal")
                                .debug_selector(|| "notice-reveal".into())
                                .flex_none()
                                .cursor_pointer()
                                .text_color(palette.accent)
                                .hover(|style| style.opacity(0.75))
                                .on_click(cx.listener(|this, _, _, cx| this.reveal_saved(cx)))
                                .child(mono("SHOW IN FOLDER")),
                        )
                    })
            }))
            .children(self.render_connection_banner(&palette, cx))
            // Who has something new, a click away from the chats.
            .children(self.render_story_strip(&palette, cx))
            .child(self.render_search(&palette))
            .child(self.render_filters(&palette, cx))
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .when(row_count == 0, |this| {
                        this.child(self.render_empty_list(&palette))
                    })
                    .when(row_count > 0, |this| {
                        this.child(
                            uniform_list(
                                "chats",
                                row_count,
                                cx.processor(
                                    move |this, range: std::ops::Range<usize>, window, cx| {
                                        let now = Timestamp::now();
                                        // The chat the keyboard is on, while
                                        // the list has the keyboard.
                                        let cursor = this
                                            .list_has_keyboard(window)
                                            .then(|| this.list_cursor.clone())
                                            .flatten();
                                        range
                                            .map(|index| {
                                                this.render_list_row(
                                                    index,
                                                    now,
                                                    cursor.as_ref(),
                                                    &palette,
                                                    cx,
                                                )
                                            })
                                            .collect::<Vec<_>>()
                                    },
                                ),
                            )
                            .track_scroll(&self.chat_scroll)
                            .size_full(),
                        )
                        .child(Scrollbar::vertical(&self.chat_scroll))
                    }),
            )
    }

    pub(super) fn render_list_header(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(self.render_list_title(palette))
                    // Which number these are the chats of.
                    .children(self.current_account_name().map(|name| {
                        div()
                            .debug_selector(|| "list-account".into())
                            .truncate()
                            .text_size(metrics::TEXT_META())
                            .text_color(palette.text_muted)
                            .child(SharedString::from(name))
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(if self.status_active() {
                        self.render_new_status_button(palette, cx)
                    } else if self.engine.capabilities().start_chat {
                        super::hints::hinted(
                            icon_button("new-chat", IconName::SquarePen, palette),
                            "new-chat",
                        )
                        .when(self.overlay == Overlay::NewChat, |this| {
                            this.bg(palette.muted)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_overlay(Overlay::NewChat, window, cx)
                        }))
                    } else {
                        unavailable_button(
                            "new-chat",
                            IconName::SquarePen,
                            "Starting a chat is not available with this provider",
                            palette,
                        )
                    })
                    .child(
                        super::hints::hinted(
                            icon_button("list-menu", IconName::EllipsisVertical, palette),
                            "list-menu",
                        )
                        .when(self.overlay == Overlay::ListMenu, |this| {
                            this.bg(palette.muted)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_overlay(Overlay::ListMenu, window, cx)
                        })),
                    ),
            )
    }

    /// A strip that says the chats are not being saved, and why: there is
    /// no keychain to hold the database key, and nothing is written to
    /// disk in clear instead.
    fn render_storage_banner(&self, palette: &Palette) -> Option<impl IntoElement> {
        let note = self.storage_note.clone()?;
        Some(
            div()
                .flex_none()
                .debug_selector(|| "storage-note".into())
                .px_4()
                .py(px(10.))
                .border_b_1()
                .border_color(palette.border)
                .bg(palette.muted)
                .flex()
                .gap_2()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(18.))
                .text_color(palette.text_muted)
                .child(
                    div()
                        .flex_none()
                        .mt(px(5.))
                        .size(px(8.))
                        .rounded_full()
                        .bg(palette.warning),
                )
                .child(div().flex_1().min_w_0().child(note)),
        )
    }

    fn render_search(&self, palette: &Palette) -> impl IntoElement {
        div().flex_none().px_3().pt_3().pb_2().child(
            div()
                .h(px(36.))
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(palette.border)
                .bg(palette.surface)
                .pl(px(10.))
                .pr_1()
                .flex()
                .items_center()
                .gap_1()
                .child(icon(IconName::Search, px(15.), palette.text_muted))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Input::new(&self.search).appearance(false).cleanable(true)),
                ),
        )
    }

    fn render_filters(&self, palette: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let mut chips = div()
            .flex_none()
            .px_3()
            .pb_2()
            .flex()
            .items_center()
            .gap_1();
        for (index, (filter, name)) in [
            (ChatFilter::All, "All"),
            (ChatFilter::Unread, "Unread"),
            (ChatFilter::Groups, "Groups"),
            (ChatFilter::Communities, "Communities"),
            (ChatFilter::Archived, "Archived"),
        ]
        .into_iter()
        .enumerate()
        .filter(|(_, (filter, _))| *filter != ChatFilter::Communities || self.has_communities)
        {
            let active = self.filter == filter;
            let (hover, hover_text) = (palette.hover, palette.text);
            chips = chips.child(
                div()
                    .id(("filter", index))
                    .debug_selector(move || format!("filter-{}", name.to_lowercase()))
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(metrics::RADIUS())
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_size(metrics::TEXT_SMALL())
                    .map(|this| {
                        if active {
                            this.bg(palette.muted)
                                .text_color(palette.text)
                                .font_weight(FontWeight::MEDIUM)
                        } else {
                            this.text_color(palette.text_muted)
                                .hover(move |style| style.bg(hover).text_color(hover_text))
                        }
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_filter(filter, cx)))
                    .child(name),
            );
        }
        chips
    }

    fn render_empty_list(&self, palette: &Palette) -> impl IntoElement {
        let (title, detail): (&str, &str) = if !self.query.is_empty() {
            ("No results", "No chats or messages match your search.")
        } else if self.filter == ChatFilter::Archived {
            ("No archived chats", "Chats you archive are kept here.")
        } else if self.filter != ChatFilter::All {
            ("Nothing here", "No chats match this filter.")
        } else if self.accounts.is_empty() {
            ("Getting things ready", "Your chats will appear here.")
        } else {
            ("No chats yet", "New conversations will appear here.")
        };
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .pt_16()
            .px_6()
            .gap_1()
            .child(
                div()
                    .text_size(metrics::TEXT_NAME())
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(palette.text)
                    .child(title),
            )
            .child(
                div()
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_muted)
                    .child(detail),
            )
    }

    fn render_list_row(
        &self,
        index: usize,
        now: Timestamp,
        cursor: Option<&client_provider::ChatId>,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match self.list_rows.get(index) {
            Some(ListRow::Chat(chat)) => {
                self.render_chat_row(index, chat, now, cursor == Some(&chat.id), palette, cx)
            }
            Some(ListRow::Hit(hit)) => self.render_hit_row(index, hit, now, palette, cx),
            Some(ListRow::Section(name)) => div()
                .h(metrics::CHAT_ROW_HEIGHT())
                .px_4()
                .pb_2()
                .flex()
                .items_end()
                .child(label(name, palette))
                .into_any_element(),
            Some(ListRow::Community(community)) => self.render_community_row(
                index,
                community,
                cursor == Some(&community.id),
                palette,
                cx,
            ),
            None => div().h(metrics::CHAT_ROW_HEIGHT()).into_any_element(),
        }
    }

    /// A community's row in the Communities view: a row like a chat's,
    /// with the community's picture, its name, how many groups it links
    /// and a chevron. It opens the community itself, which has no chat.
    fn render_community_row(
        &self,
        index: usize,
        community: &super::shell::CommunityHeading,
        focused: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let account = self.account.clone();
        let active = self.overlay == Overlay::ContactInfo
            && matches!(
                self.social.target(),
                Some(Target::Group { group, .. }) if *group == community.id
            );
        let groups = account
            .as_ref()
            .and_then(|account| {
                self.engine
                    .store()
                    .group(account, &community.id)
                    .ok()
                    .flatten()
            })
            .map(|stored| stored.group.subgroups.len())
            .filter(|count| *count > 0);
        let subtitle: SharedString = community_subtitle(groups).into();
        let face = avatar_or(
            account
                .as_ref()
                .and_then(|account| self.media.avatar(account, &community.id)),
            &community.name,
            AvatarKind::Group,
            metrics::AVATAR_LARGE(),
            palette,
        );
        let hover = palette.hover;
        let group = community.id.clone();
        div()
            .id(("community", index))
            .debug_selector(move || format!("community-heading-{index}"))
            .relative()
            .w_full()
            .h(metrics::CHAT_ROW_HEIGHT())
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(palette.muted)
                } else {
                    this.hover(move |style| style.bg(hover))
                }
            })
            .when(focused, |this| {
                this.child(
                    div()
                        .debug_selector(|| "list-focus".into())
                        .absolute()
                        .top(px(2.))
                        .bottom(px(2.))
                        .left(px(4.))
                        .right(px(4.))
                        .rounded(metrics::RADIUS())
                        .border_2()
                        .border_color(palette.focus_ring),
                )
            })
            .when(active, |this| {
                this.child(
                    div()
                        .debug_selector(move || format!("community-selected-{index}"))
                        .absolute()
                        .left_0()
                        .top_2()
                        .bottom_2()
                        .w(px(2.))
                        .rounded_full()
                        .bg(palette.text),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Some(account) = this.account.clone() {
                    let group = group.clone();
                    this.open_info(Target::Group { account, group }, window, cx);
                }
            }))
            .child(div().flex_none().child(face))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .debug_selector(move || format!("community-name-{index}"))
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_NAME())
                            .text_color(palette.text)
                            .font_weight(FontWeight::MEDIUM)
                            .child(community.name.clone()),
                    )
                    .child(
                        div()
                            .debug_selector(move || format!("community-subtitle-{index}"))
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text_muted)
                            .child(subtitle),
                    ),
            )
            .child(icon(IconName::ChevronRight, px(16.), palette.text_faint))
            .into_any_element()
    }

    fn render_chat_row(
        &self,
        index: usize,
        chat: &ChatSummary,
        now: Timestamp,
        focused: bool,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self
            .open
            .as_ref()
            .is_some_and(|open| open.chat.id == chat.id);
        let unread = chat.unread_count > 0;
        let presence = self.engine.presence(&chat.account_id, &chat.id);
        let chat_id = chat.id.clone();

        let time = chat
            .last_message
            .as_ref()
            .map(|message| list_time(message.timestamp, now))
            .unwrap_or_default();

        // Second line: who is typing, or the last message.
        let preview = match presence {
            Some(PresenceState::Typing) => div()
                .text_color(palette.accent)
                .child("typing…")
                .into_any_element(),
            Some(PresenceState::Recording) => div()
                .text_color(palette.accent)
                .child("recording audio…")
                .into_any_element(),
            _ => match &chat.last_message {
                Some(message) => {
                    let text: SharedString = match (&chat.kind, &message.sender_name) {
                        (ChatKind::Group, Some(name)) if !message.outgoing => {
                            let first = name.split_whitespace().next().unwrap_or(name);
                            format!("{first}: {}", message.text).into()
                        }
                        _ => message.text.clone().into(),
                    };
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .when(message.outgoing, |this| {
                            this.child(div().flex_none().child(status_tick(
                                &message.status,
                                palette.text_faint,
                                palette.accent,
                                palette,
                            )))
                        })
                        .child(div().min_w_0().truncate().child(text))
                        .into_any_element()
                }
                // A chat nobody has written in yet (a new group, say): said,
                // so the row does not look broken.
                None => div()
                    .debug_selector(|| "row-empty".into())
                    .italic()
                    .text_color(palette.text_faint)
                    .child("No messages yet")
                    .into_any_element(),
            },
        };

        let hover = palette.hover;
        let kind = match chat.kind {
            ChatKind::Group => AvatarKind::Group,
            ChatKind::Direct => AvatarKind::Person,
        };
        let name = chat.id.to_string();
        let (menu_chat, arrow_chat) = (chat.clone(), chat.clone());
        // The arrow that opens the same menu, for those who do not
        // right-click. It shows when the pointer is on the row.
        let arrow = div()
            .id(("row-menu", index))
            .debug_selector(|| "row-menu-button".into())
            .flex_none()
            .size(px(18.))
            .rounded(px(4.))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .invisible()
            .group_hover("chat-row", |style| style.visible())
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.open_row_menu(arrow_chat.clone(), event.position(), window, cx);
            }))
            .child(icon(IconName::ChevronDown, px(14.), palette.text_muted));
        div()
            .id(("chat", index))
            .group("chat-row")
            .debug_selector(move || format!("chat-{name}"))
            .relative()
            .w_full()
            .h(metrics::CHAT_ROW_HEIGHT())
            .px_4()
            .flex()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .map(|this| {
                if active {
                    this.bg(palette.muted)
                } else {
                    this.hover(move |style| style.bg(hover))
                }
            })
            // The keyboard is on this chat: the outline a message in
            // focus wears, inside the row. The open chat keeps its fill
            // and its bar; the two can be on different rows.
            .when(focused, |this| {
                this.child(
                    div()
                        .debug_selector(|| "list-focus".into())
                        .absolute()
                        .top(px(2.))
                        .bottom(px(2.))
                        .left(px(4.))
                        .right(px(4.))
                        .rounded(metrics::RADIUS())
                        .border_2()
                        .border_color(palette.focus_ring),
                )
            })
            .when(active, |this| {
                // The open chat's marker: a short ink bar on the edge.
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_2()
                        .bottom_2()
                        .w(px(2.))
                        .rounded_full()
                        .bg(palette.text),
                )
            })
            .child(
                div()
                    .flex_none()
                    .debug_selector(move || format!("row-avatar-{index}"))
                    .child({
                        let face = avatar_or(
                            self.media.avatar(&chat.account_id, &chat.id),
                            &chat.title,
                            kind,
                            metrics::AVATAR_LARGE(),
                            palette,
                        );
                        // Somebody with a status wears a ring, and a click
                        // on it watches the status (not the chat).
                        match chat.kind {
                            ChatKind::Direct => self.with_story_ring(
                                face,
                                metrics::AVATAR_LARGE(),
                                &client_provider::ContactId::new(chat.id.as_str()),
                                SharedString::from(format!("row-ring-{index}")),
                                palette,
                                cx,
                            ),
                            ChatKind::Group => face.into_any_element(),
                        }
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .debug_selector(move || format!("row-name-{index}"))
                                    .min_w_0()
                                    .truncate()
                                    .text_size(metrics::TEXT_NAME())
                                    .text_color(palette.text)
                                    .font_weight(if unread {
                                        FontWeight::SEMIBOLD
                                    } else {
                                        FontWeight::MEDIUM
                                    })
                                    .child(SharedString::from(chat.title.clone())),
                            )
                            // The community's announcement group says so.
                            .when(
                                self.filter == ChatFilter::Communities
                                    && chat.community.as_ref().is_some_and(|c| c.announcements),
                                |this| {
                                    this.child(
                                        div()
                                            .debug_selector(move || {
                                                format!("row-announcements-{index}")
                                            })
                                            .flex_none()
                                            .child(label("Announcements", palette)),
                                    )
                                },
                            )
                            // The time of an unread chat carries the
                            // accent, like its badge.
                            .child(
                                mono(time)
                                    .debug_selector(move || format!("row-time-{index}"))
                                    .text_color(if unread {
                                        palette.accent
                                    } else {
                                        palette.text_muted
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(metrics::TEXT_SMALL())
                                    .text_color(if unread {
                                        palette.text
                                    } else {
                                        palette.text_muted
                                    })
                                    .child(preview),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .when(chat.muted, |this| {
                                        this.child(icon(
                                            IconName::BellOff,
                                            px(14.),
                                            palette.text_faint,
                                        ))
                                    })
                                    .when(chat.pinned, |this| {
                                        this.child(icon(IconName::Pin, px(14.), palette.text_faint))
                                    })
                                    .when(unread, |this| {
                                        this.child(
                                            div()
                                                .debug_selector(move || {
                                                    format!("row-badge-{index}")
                                                })
                                                .child(unread_badge(
                                                    chat.unread_count,
                                                    chat.muted,
                                                    palette,
                                                )),
                                        )
                                    })
                                    .child(arrow),
                            ),
                    ),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_chat(chat_id.clone(), Some(window), cx);
            }))
            // The row's own menu: a right-click anywhere on it.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_row_menu(menu_chat.clone(), event.position, window, cx);
                }),
            )
            .into_any_element()
    }

    /// A message found by the search field.
    fn render_hit_row(
        &self,
        index: usize,
        hit: &SearchHit,
        now: Timestamp,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let message = &hit.message;
        let chat_id = message.chat_id.clone();
        let body = message_preview(message);
        let text: SharedString = match (&message.direction, &message.sender_name) {
            (Direction::Outgoing, _) => format!("You: {body}").into(),
            (Direction::Incoming, Some(name)) if name != &hit.chat_title => {
                format!("{}: {body}", name.split_whitespace().next().unwrap_or(name)).into()
            }
            _ => body.into(),
        };
        let hover = palette.hover;
        div()
            .id(("hit", index))
            .w_full()
            .h(metrics::CHAT_ROW_HEIGHT())
            .px_4()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(3.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_NAME())
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(palette.text)
                            .child(SharedString::from(hit.chat_title.clone())),
                    )
                    .child(mono(list_time(message.timestamp, now)).text_color(palette.text_muted)),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.text_muted)
                    .child(text),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_chat(chat_id.clone(), Some(window), cx);
            }))
            .into_any_element()
    }
}

/// The unread count: the lime fill with ink, or a quiet chip for a muted
/// chat.
fn unread_badge(count: u32, muted: bool, palette: &Palette) -> impl IntoElement {
    let (fill, ink) = if muted {
        (palette.badge_muted, palette.on_badge_muted)
    } else {
        (palette.accent_fill, palette.on_accent_fill)
    };
    div()
        .debug_selector(|| "unread-badge".into())
        .min_w(px(19.))
        .h(px(19.))
        .px(px(5.))
        .rounded_full()
        .bg(fill)
        .flex()
        .items_center()
        .justify_center()
        .text_color(ink)
        .child(
            mono(if count > 999 {
                "999+".to_owned()
            } else {
                count.to_string()
            })
            .text_size(px(10.5))
            .font_weight(FontWeight::MEDIUM),
        )
}
