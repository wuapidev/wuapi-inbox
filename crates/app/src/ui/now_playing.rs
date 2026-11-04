//! The bar that follows a voice note to other chats.
//!
//! A clip started in a chat goes on when another chat is opened. While
//! the chat it came from is not the one on screen, this bar over the
//! chat list says what plays and holds its controls: play or pause, how
//! far it is, the speed, and the × that stops it. A click on the name
//! goes back to the message.

use super::shell::Shell;
use super::widgets::{icon_button, mono};
use crate::audio::Playback;
use crate::format::duration;
use crate::i18n;
use crate::icons::{icon, IconName};
use crate::theme::{metrics, px, Palette};
use gpui_kit::prelude::*;
use gpui_kit::{div, relative, Div, SharedString};

impl Shell {
    pub(super) fn render_now_playing(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let now = self.audio.now_playing()?;
        // In its own chat the bubble is the control.
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.chat.id == now.chat)
        {
            return None;
        }
        let playback = self.audio.player.playback(&now.url);
        let total = self.audio.clips.get(&now.url).map(|clip| clip.duration());
        let (at, playing) = match playback {
            Playback::Playing(at) => (at, true),
            Playback::Paused(at) => (at, false),
            Playback::Idle => return None,
        };
        let progress = match total {
            Some(total) if !total.is_zero() => {
                (at.as_secs_f32() / total.as_secs_f32()).clamp(0., 1.)
            }
            _ => 0.,
        };
        let clock = |at: std::time::Duration| duration(at.as_secs() as u32);
        let time: SharedString = match total {
            Some(total) => format!("{} / {}", clock(at), clock(total)).into(),
            None => clock(at).into(),
        };
        let label = now.label.clone();
        Some(
            div()
                .flex_none()
                .debug_selector(|| "now-playing".into())
                .relative()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(palette.border)
                .bg(palette.muted)
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .id("now-playing-toggle")
                        .debug_selector(|| "now-playing-toggle".into())
                        .flex_none()
                        .size(px(30.))
                        .rounded_full()
                        .bg(palette.play_in)
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .hover(|style| style.opacity(0.85))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_now_playing(cx);
                        }))
                        .child(icon(
                            if playing {
                                IconName::Pause
                            } else {
                                IconName::Play
                            },
                            px(14.),
                            palette.on_play_in,
                        )),
                )
                .child(
                    div()
                        .id("now-playing-open")
                        .debug_selector(|| "now-playing-open".into())
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .cursor_pointer()
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                                "common.voice.go_to_message",
                            ))
                            .build(window, cx)
                        })
                        .on_click(cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_now_playing(window, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(4.))
                                .text_size(metrics::TEXT_SMALL())
                                .text_color(palette.text)
                                .child(icon(IconName::Mic, px(11.), palette.text_muted))
                                .child(div().min_w_0().truncate().child(label)),
                        )
                        .child(
                            mono(time)
                                .text_size(px(10.5))
                                .text_color(palette.text_muted),
                        ),
                )
                .child(
                    div()
                        .id("now-playing-speed")
                        .debug_selector(|| "now-playing-speed".into())
                        .flex_none()
                        .h(px(20.))
                        .px(px(7.))
                        .rounded_full()
                        .bg(palette.surface)
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .text_color(palette.text)
                        .hover(|style| style.opacity(0.8))
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.cycle_audio_speed(cx);
                        }))
                        .child(mono(self.audio.player.speed().label()).text_size(px(10.5))),
                )
                .child(
                    icon_button("now-playing-close", IconName::X, palette)
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                                "common.voice.stop_playing",
                            ))
                            .build(window, cx)
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.stop_now_playing(cx);
                        })),
                )
                // How far it is, along the bar's bottom edge.
                .child(
                    div()
                        .absolute()
                        .left_0()
                        .bottom_0()
                        .h(px(2.))
                        .w(relative(progress))
                        .bg(palette.accent),
                ),
        )
    }
}
