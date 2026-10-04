//! The conversation pane: header, virtualised message list and composer.

use super::bubble::{render_row, RowContext};
use super::senders::person_avatar;
use super::shell::{OpenChat, Overlay, Shell};
use super::wallpaper::wallpaper;
use super::widgets::{
    avatar_or, brand_mark, corner_marks, icon_button, mono, reveal, tag, unavailable_button,
    AvatarKind, MarkPlay,
};
use crate::format::list_time;
use crate::icons::{icon, IconName};
use crate::motion;
use crate::product::{LICENCE, MAKER, PRODUCT_NAME, VERSION};
use crate::settings::{self, WallpaperChoice};
use crate::theme::px;
use crate::theme::{metrics, Palette};
use client_core::message_preview;
use client_provider::{ChatKind, Direction, PresenceState, Timestamp};
use gpui_kit::component::input::{Input, Textarea};
use gpui_kit::component::scroll::Scrollbar;
use gpui_kit::component::Sizable;
use gpui_kit::prelude::*;
use gpui_kit::{div, list, Context, Div, Focusable, FontWeight, SharedString, Window};

impl Shell {
    pub(super) fn render_conversation(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pane = div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(palette.background);
        match &self.open {
            Some(open) => pane
                .child(self.render_header(open, palette, cx))
                .children(self.render_thread_search(palette, cx))
                .child(self.render_messages(open, palette, window, cx))
                // A number that is not connected says so where the
                // message is written; sending is not held back.
                .children(self.render_connection_notice(&open.chat.account_id, palette, cx))
                // While messages are being selected, what can be done with
                // them takes the composer's place.
                .child(match self.render_selection_bar(palette, cx) {
                    Some(bar) => bar.into_any_element(),
                    None => self.render_composer(palette, window, cx).into_any_element(),
                }),
            None => pane.child(self.render_welcome(palette, window, cx)),
        }
    }

    fn render_header(
        &self,
        open: &OpenChat,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let chat = &open.chat;
        let presence = self.engine.presence(&chat.account_id, &chat.id);
        // Second line: what the other side is doing, when known.
        let subtitle: Option<(SharedString, _)> = match presence {
            Some(PresenceState::Typing) => Some(("typing…".into(), palette.accent)),
            Some(PresenceState::Recording) => Some(("recording audio…".into(), palette.accent)),
            Some(PresenceState::Online) => Some(("online".into(), palette.text_muted)),
            // Otherwise who is in the group, or the contact's number:
            // what the store has, read when the chat was opened.
            _ => open.subtitle.clone().map(|line| (line, palette.text_muted)),
        };
        let picture = self.media.avatar(&chat.account_id, &chat.id);
        let face = match chat.kind {
            ChatKind::Group => avatar_or(
                picture,
                &chat.title,
                AvatarKind::Group,
                metrics::AVATAR_MEDIUM(),
                palette,
            ),
            // A person has their colour here too.
            ChatKind::Direct => person_avatar(
                picture,
                &chat.title,
                metrics::AVATAR_MEDIUM(),
                self.senders
                    .of(
                        &chat.account_id,
                        None,
                        &client_provider::ContactId::new(chat.id.as_str().to_owned()),
                    )
                    .tone,
                palette,
            ),
        };

        div()
            .flex_none()
            .h(metrics::HEADER_HEIGHT())
            .pl_4()
            .pr_2()
            .border_b_1()
            .border_color(palette.border)
            .flex()
            .items_center()
            .gap_3()
            .child(
                div()
                    .id("header-avatar")
                    .debug_selector(|| "header-avatar".into())
                    .cursor_pointer()
                    // The picture opens the profile, or the group's details.
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(chat) = this.open.as_ref().map(|open| open.chat.clone()) {
                            this.open_chat_info(&chat, window, cx);
                        }
                    }))
                    // With a status, the ring is the way to it.
                    .child(match chat.kind {
                        ChatKind::Direct => self.with_story_ring(
                            face,
                            metrics::AVATAR_MEDIUM(),
                            &client_provider::ContactId::new(chat.id.as_str().to_owned()),
                            SharedString::from("header-ring"),
                            palette,
                            cx,
                        ),
                        ChatKind::Group => face.into_any_element(),
                    }),
            )
            .child(
                div()
                    .id("header-info")
                    .debug_selector(|| "header-info".into())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .cursor_pointer()
                    // And so does the name.
                    .on_click(cx.listener(|this, _, window, cx| {
                        if let Some(chat) = this.open.as_ref().map(|open| open.chat.clone()) {
                            this.open_chat_info(&chat, window, cx);
                        }
                    }))
                    .child(
                        div()
                            .debug_selector(|| "header-title".into())
                            .truncate()
                            .text_size(metrics::TEXT_NAME())
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(palette.text)
                            .child(SharedString::from(chat.title.clone())),
                    )
                    .when_some(subtitle, |this, (text, colour)| {
                        this.child(
                            div()
                                .debug_selector(|| "header-subtitle".into())
                                .truncate()
                                .text_size(metrics::TEXT_SMALL())
                                .text_color(colour)
                                .child(text),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    // No call buttons: calls are outside what this client
                    // does, and a button that cannot work is noise.
                    .child(
                        super::hints::hinted(
                            icon_button("chat-search", IconName::Search, palette),
                            "chat-search",
                        )
                        .when(self.thread_search.is_some(), |this| this.bg(palette.muted))
                        .on_click(
                            cx.listener(|this, _, window, cx| {
                                this.toggle_thread_search(window, cx)
                            }),
                        ),
                    )
                    .child(
                        super::hints::hinted(
                            icon_button("chat-menu", IconName::EllipsisVertical, palette),
                            "chat-menu",
                        )
                        .when(self.overlay == Overlay::ChatMenu, |this| {
                            this.bg(palette.muted)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_overlay(Overlay::ChatMenu, window, cx)
                        })),
                    ),
            )
    }

    /// The search of the open conversation: a field under the header and,
    /// while there is something typed, what it found. Picking a result
    /// scrolls the conversation to it.
    fn render_thread_search(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement> {
        let search = self.thread_search.as_ref()?;
        let now = Timestamp::now();
        let count: SharedString = match (search.query.is_empty(), search.hits.len()) {
            (true, _) => "".into(),
            (false, 0) => "NO MATCHES".into(),
            (false, n) => format!("{} OF {n}", search.current + 1).into(),
        };
        let mut results = div()
            .id("thread-hits")
            .max_h(px(232.))
            .overflow_y_scroll()
            .flex()
            .flex_col();
        for (index, hit) in search.hits.iter().enumerate() {
            let message = &hit.message;
            let target = message.id.clone();
            let hover = palette.hover;
            let who: SharedString = match (&message.direction, &message.sender_name) {
                (Direction::Outgoing, _) => "You".into(),
                (Direction::Incoming, Some(name)) => name.clone().into(),
                (Direction::Incoming, None) => "".into(),
            };
            results = results.child(
                div()
                    .id(("thread-hit", index))
                    .debug_selector(move || format!("thread-hit-{index}"))
                    .h(px(40.))
                    .px_4()
                    .border_t_1()
                    .border_color(palette.border)
                    .flex()
                    .items_center()
                    .gap_3()
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.jump_to_message(target.clone(), cx)),
                    )
                    .child(mono(list_time(message.timestamp, now)).text_color(palette.text_muted))
                    .child(
                        div()
                            .flex_none()
                            .text_size(metrics::TEXT_SMALL())
                            .font_weight(FontWeight::MEDIUM)
                            .child(who),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text_muted)
                            .child(SharedString::from(message_preview(message))),
                    ),
            );
        }
        Some(
            div()
                .flex_none()
                .debug_selector(|| "thread-search".into())
                .border_b_1()
                .border_color(palette.border)
                .bg(palette.background)
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(48.))
                        .pl_4()
                        .pr_2()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(icon(IconName::Search, px(15.), palette.text_muted))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Input::new(&search.input).appearance(false)),
                        )
                        .child(
                            mono(count)
                                .debug_selector(|| "thread-search-count".into())
                                .text_color(palette.text_muted),
                        )
                        .child(
                            icon_button("thread-search-previous", IconName::ChevronDown, palette)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step_match(false, cx);
                                })),
                        )
                        .child(
                            icon_button("thread-search-next", IconName::ChevronUp, palette)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.step_match(true, cx);
                                })),
                        )
                        .child(
                            icon_button("thread-search-close", IconName::X, palette).on_click(
                                cx.listener(|this, _, window, cx| {
                                    this.close_thread_search(window, cx)
                                }),
                            ),
                        ),
                )
                .child(results),
        )
    }

    fn render_messages(
        &self,
        open: &OpenChat,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let rows = open.rows.clone();
        let palette = *palette;
        let in_group = open.chat.kind == ChatKind::Group;
        // Rows are drawn by the list, only the ones near the viewport:
        // that is what makes media lazy.
        let context = RowContext {
            palette,
            still: settings::reduce_motion(cx),
            // Pictures that move do so only in front of the user: in the
            // active window, with nothing over the conversation.
            motion: match motion::frozen_at(cx) {
                Some(at) => crate::animation::Motion::Frozen(at),
                None if !window.is_window_active() || self.overlay != Overlay::None => {
                    crate::animation::Motion::Paused
                }
                None => crate::animation::Motion::Running,
            },
            engine: self.engine.clone(),
            shelf: self.media.clone(),
            senders: self.senders.clone(),
            row: Default::default(),
            whole: self
                .focused_row()
                .filter(|_| self.keys.whole)
                .map(|row| row.stored.message.id.clone()),
            focused: self.keys.focused.as_deref().map(Into::into),
            text_pressed: Default::default(),
            flash: self.acting.flash.as_deref().map(Into::into),
            find: self
                .thread_search
                .as_ref()
                .map(|search| search.query.to_lowercase())
                .filter(|query| !query.is_empty())
                .map(Into::into),
            selecting: self
                .acting
                .selecting
                .as_ref()
                .map(|picked| std::rc::Rc::new(picked.clone())),
            view: cx.entity().downgrade(),
            audio: self.audio.view(),
            tiles: self.tile_context(),
        };
        let zone = palette.accent;
        let patterned = settings::get(cx).wallpaper == WallpaperChoice::Pattern;
        div()
            .relative()
            .flex_1()
            .min_h_0()
            // Behind the messages, and outside their layout.
            .when(patterned, |this| this.child(wallpaper(&palette)))
            // Files dragged from the desktop: the pane lights up, and a
            // drop opens the sheet.
            .drag_over::<gpui_kit::ExternalPaths>(move |style, _, _, _| {
                style.bg(zone.opacity(0.12))
            })
            .on_drop(cx.listener(|this, paths: &gpui_kit::ExternalPaths, _, cx| {
                this.attach_paths(paths.paths().to_vec(), cx);
            }))
            // The room between the newest message and the composer belongs
            // to the pane, not to a row: it is there whatever the list has
            // measured, when a message arrives and when the composer grows.
            // The list paints nothing outside itself, and the outline of
            // the message the keyboard is on stands out of its bubble: the
            // list keeps that much of the inset inside itself, as padding.
            .pb(metrics::THREAD_INSET() - metrics::FOCUS_ROOM())
            .debug_selector(|| "thread".into())
            .child(
                list(open.list.clone(), move |index, _, _| {
                    match rows.get(index) {
                        Some(row) => {
                            context.row.set(index as u64);
                            render_row(row, &palette, in_group, &context).into_any_element()
                        }
                        None => div().into_any_element(),
                    }
                })
                .size_full()
                // The room of the outline, above the first row and under
                // the last: padding of the list, inside what it paints.
                // (Not a box around those rows: a row must stay what the
                // list measures, or its bubble is stretched.)
                .pt(metrics::FOCUS_ROOM())
                .pb(metrics::FOCUS_ROOM()),
            )
            .child(Scrollbar::vertical(&open.list))
    }

    /// What is under the messages: the composer, or the recording bar
    /// while a voice note is being recorded.
    fn render_composer(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        match self.render_record_bar(palette, cx) {
            Some(bar) => bar,
            None => self
                .render_composer_field(palette, window, cx)
                .into_any_element(),
        }
    }

    fn render_composer_field(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let has_text = !self.composer.read(cx).value().trim().is_empty();
        // The field wears the accent while it is where typing goes.
        let focused = self.composer.focus_handle(cx).is_focused(window);
        div()
            .debug_selector(|| "composer".into())
            .flex_none()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(palette.border)
            // While a mention is typed, the arrows, Tab and Escape are
            // the list's before they are the text field's.
            .capture_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                    this.mention_key(event, window, cx);
                    this.emoji_completion_escape(event, cx);
                }),
            )
            .children(self.render_mention_picker(palette, cx))
            .children(self.render_emoji_completion(palette, cx))
            .children(self.render_compose_bar(palette, cx))
            .children(self.render_palette_tip(palette, cx))
            .child(
                // One rounded field holds the tools, the text and the send
                // button; it grows upward with the text, the buttons
                // staying on its last line.
                div()
                    .debug_selector(|| {
                        if focused {
                            "composer-field-focused".into()
                        } else {
                            "composer-field".into()
                        }
                    })
                    .p(px(5.))
                    .rounded(metrics::COMPOSER_RADIUS())
                    .border_1()
                    .border_color(if focused {
                        palette.accent
                    } else {
                        palette.border
                    })
                    .bg(palette.surface)
                    .flex()
                    .items_end()
                    .gap(px(2.))
                    .child(if self.can_attach() {
                        super::hints::hinted(
                            icon_button("attach", IconName::Plus, palette),
                            "attach",
                        )
                        .rounded_full()
                        .on_click(cx.listener(|this, _, _, cx| this.pick_attachments(cx)))
                    } else {
                        unavailable_button(
                            "attach",
                            IconName::Plus,
                            super::attach::NOT_AVAILABLE,
                            palette,
                        )
                    })
                    .child(self.render_emoji_button(palette, cx))
                    .child(
                        div()
                            .debug_selector(|| "composer-text".into())
                            .flex_1()
                            .min_w_0()
                            // One line of text is exactly as tall as the
                            // buttons beside it, so they share a middle;
                            // more lines grow the field upward.
                            .min_h(metrics::CONTROL())
                            .px_1()
                            .py(px(3.))
                            .flex()
                            .items_center()
                            .child({
                                // A paste of files or of a picture is an
                                // attachment; of text, it is text.
                                let view = cx.entity().downgrade();
                                Textarea::new(&self.composer)
                                    .appearance(false)
                                    // The component's own padding would
                                    // make one line taller than a button.
                                    .small()
                                    .on_paste(move |item, _, cx| {
                                        view.update(cx, |this, cx| this.paste_into_chat(item, cx))
                                            .unwrap_or(false)
                                    })
                            }),
                    )
                    .child(if has_text {
                        // The one accent control on the screen.
                        div()
                            .id("send")
                            .debug_selector(|| "send".into())
                            .flex_none()
                            .size(metrics::CONTROL())
                            .rounded_full()
                            .bg(palette.accent_fill)
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.85))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.send_composed(window, cx)),
                            )
                            .child(icon(
                                IconName::SendHorizontal,
                                px(16.),
                                palette.on_accent_fill,
                            ))
                    } else {
                        // The microphone: there while nothing is typed.
                        if self.can_record() {
                            let hint: SharedString =
                                match crate::keys::keys_label(crate::keys::Command::RecordVoice) {
                                    Some(keys) => format!("Record a voice note ({keys})").into(),
                                    None => "Record a voice note".into(),
                                };
                            icon_button("voice", IconName::Mic, palette)
                                .rounded_full()
                                .tooltip(move |window, cx| {
                                    gpui_kit::component::tooltip::Tooltip::new(hint.clone())
                                        .build(window, cx)
                                })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.start_recording(window, cx)
                                }))
                        } else {
                            unavailable_button(
                                "voice",
                                IconName::Mic,
                                super::voice_record::NOT_AVAILABLE,
                                palette,
                            )
                        }
                    }),
            )
    }

    /// What the pane shows before a chat is opened. It doubles as the
    /// about screen: the logo in the rail leads back here.
    ///
    /// Composed from the brand's own pieces: the mark in a hairline frame
    /// with crosshairs on its corners, mono tags, the status light, and
    /// rows split by rules. It comes in once, piece after piece.
    fn render_welcome(
        &self,
        palette: &Palette,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let now = cx.background_executor().now();
        let step = |step: u32| self.entrance.reveal(step, now);
        // The mark moves only while this screen is the one being looked at.
        let play = MarkPlay {
            paused: !window.is_window_active() || self.overlay != Overlay::None,
            reduced: settings::reduce_motion(cx),
            intro: self.mark_intro,
            frozen: motion::frozen_at(cx),
        };

        // The pane can be narrow (a small window) or short: the rows then
        // stack, or step aside, instead of squeezing the text.
        let viewport = window.viewport_size();
        let pane = viewport.width - metrics::RAIL_WIDTH() - self.list_px;
        let narrow = pane < px(560.);
        let short = viewport.height < px(660.);

        let connected = self
            .accounts
            .iter()
            .filter(|account| account.connection.is_connected())
            .count();
        let (light, status): (_, SharedString) = match (self.accounts.len(), connected) {
            (0, _) => (palette.text_faint, "No numbers yet".into()),
            (1, 1) => (palette.accent, "1 number connected".into()),
            (all, on) if all == on => (palette.accent, format!("{on} numbers connected").into()),
            (all, on) => (
                palette.warning,
                format!("{on} of {all} numbers connected").into(),
            ),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            // The strip that carries the header rule across the window.
            .child(
                div()
                    .flex_none()
                    .h(metrics::HEADER_HEIGHT())
                    .px_4()
                    .border_b_1()
                    .border_color(palette.border)
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(reveal(
                        tag("Start", None, palette).when(narrow, |this| this.invisible()),
                        step(4),
                    ))
                    .child(reveal(
                        div().debug_selector(|| "start-status".into()).child(tag(
                            &status,
                            Some(light),
                            palette,
                        )),
                        step(4),
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .px_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .debug_selector(|| "start".into())
                            .w_full()
                            .max_w(metrics::FORM_WIDTH() + px(40.))
                            .flex()
                            .flex_col()
                            .items_center()
                            .child(mark_frame("start-mark", play, step(0), palette))
                            .child(reveal(
                                div()
                                    .pt_6()
                                    .text_size(metrics::TEXT_HERO())
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(palette.text)
                                    .child(PRODUCT_NAME),
                                step(1),
                            ))
                            .child(reveal(
                                div()
                                    .pt_2()
                                    .max_w(metrics::FORM_WIDTH())
                                    .text_center()
                                    .text_size(metrics::TEXT_BODY())
                                    .line_height(px(22.))
                                    .text_color(palette.text_muted)
                                    .child(
                                        "Pick a chat to start messaging, or start one \
                                         with the pencil above the list.",
                                    ),
                                step(2),
                            ))
                            .when(!short, |this| {
                                this.child(reveal(
                                    framed_rows(
                                        step(3),
                                        palette,
                                        [
                                            fact_row(
                                                "Local-first",
                                                "Chats open from this computer, without waiting \
                                             for the network.",
                                                false,
                                                palette,
                                            ),
                                            fact_row(
                                                "Outbox",
                                                "What you send waits out a dropped connection \
                                             and goes out once.",
                                                true,
                                                palette,
                                            ),
                                            fact_row(
                                                "Open source",
                                                "Apache-2.0. Read it, change it, plug in your \
                                             own backend.",
                                                true,
                                                palette,
                                            ),
                                        ]
                                        .map(|row| {
                                            if narrow {
                                                row.flex_col().items_start().gap_1()
                                            } else {
                                                row
                                            }
                                        }),
                                    )
                                    .mt_8(),
                                    step(3),
                                ))
                            }),
                    ),
            )
            // Where everything is, for whoever has not found it yet.
            .child(reveal(
                div()
                    .debug_selector(|| "keys-line".into())
                    .flex_none()
                    .pb_3()
                    .flex()
                    .justify_center()
                    .text_color(palette.text_muted)
                    .child(mono(super::hints::keys_line())),
                step(5),
            ))
            // And where Status is, where the provider has it.
            .when(self.has_status(), |this| {
                this.child(reveal(
                    div()
                        .debug_selector(|| "status-line".into())
                        .flex_none()
                        .pb_3()
                        .flex()
                        .justify_center()
                        .text_color(palette.text_faint)
                        .child(mono(super::hints::status_line())),
                    step(5),
                ))
            })
            .child(reveal(colophon(palette), step(5)))
    }
}

/// The mark in its frame: a hairline square with a crosshair on each
/// corner. `brand_mark` is the slot the animated mark goes into.
pub(super) fn mark_frame(
    id: &'static str,
    play: MarkPlay,
    progress: f32,
    palette: &Palette,
) -> Div {
    sized_mark_frame(
        id,
        metrics::MARK_FRAME(),
        metrics::MARK(),
        play,
        progress,
        palette,
    )
}

/// [`mark_frame`] at another size.
pub(super) fn sized_mark_frame(
    id: &'static str,
    frame: gpui_kit::Pixels,
    mark: gpui_kit::Pixels,
    play: MarkPlay,
    progress: f32,
    palette: &Palette,
) -> Div {
    reveal(
        div()
            .relative()
            .flex_none()
            .size(frame)
            .child(
                div()
                    .size_full()
                    .border_1()
                    .border_color(palette.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(brand_mark(id, mark, play, palette)),
            )
            .children(corner_marks(progress, palette)),
        progress,
    )
}

/// A box of rows split by rules, with a crosshair on each corner.
pub(super) fn framed_rows(
    progress: f32,
    palette: &Palette,
    rows: impl IntoIterator<Item = Div>,
) -> Div {
    div()
        .relative()
        .w_full()
        .child(
            div()
                .w_full()
                .border_1()
                .border_color(palette.border)
                .flex()
                .flex_col()
                .children(rows),
        )
        .children(corner_marks(progress, palette))
}

/// A row of a framed box: a mono tag on the left, one short line.
pub(super) fn fact_row(name: &str, text: &'static str, ruled: bool, palette: &Palette) -> Div {
    div()
        .px_4()
        .py_3()
        .when(ruled, |this| this.border_t_1().border_color(palette.border))
        .flex()
        .items_center()
        .gap_4()
        .child(
            div()
                .flex_none()
                .w(px(124.))
                .child(tag(name, None, palette)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_size(metrics::TEXT_SMALL())
                .line_height(px(19.))
                .text_color(palette.text_muted)
                .child(text),
        )
}

/// The line at the bottom of the start and sign-in screens.
pub(super) fn colophon(palette: &Palette) -> Div {
    div()
        .flex_none()
        .pb_5()
        .px_6()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .text_color(palette.text_muted)
        .child(mono(format!(
            "{PRODUCT_NAME} {VERSION} · {LICENCE} · built by {MAKER}"
        )))
        .child(mono("An independent client. Not affiliated with WhatsApp."))
}
