//! "New poll": a question, its options and whether a voter may pick more
//! than one. It is sent like any message: into the outbox, then to the
//! provider with the same idempotency key until it is accepted.

use super::shell::Shell;
use super::widgets::{label, switch, text_button};
use crate::icons::IconName;
use crate::theme::{metrics, px, Palette};
use client_core::new_client_id;
use client_provider::{OutgoingContent, OutgoingMessage};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, Entity, SharedString, Stateful, Subscription, Window};

/// The fewest options a poll has.
const MIN_OPTIONS: usize = 2;
/// The most options WhatsApp takes.
const MAX_OPTIONS: usize = 12;
/// The longest an option may be, in characters.
const MAX_OPTION_LENGTH: usize = 100;
/// The longest a question may be, in characters.
const MAX_QUESTION_LENGTH: usize = 255;

/// The poll being written.
pub(super) struct PollForm {
    pub(super) question: Entity<InputState>,
    pub(super) options: Vec<Entity<InputState>>,
    /// A voter may pick more than one option.
    pub(super) multiple: bool,
    /// What is wrong with it, said under the fields.
    pub(super) error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

/// What was typed, checked: the question and the options to send, or what
/// is wrong, in words.
pub(super) fn checked(question: &str, options: &[String]) -> Result<(String, Vec<String>), String> {
    let question = question.trim();
    if question.is_empty() {
        return Err("Write the question.".into());
    }
    if question.chars().count() > MAX_QUESTION_LENGTH {
        return Err(format!(
            "The question takes {MAX_QUESTION_LENGTH} characters at most."
        ));
    }
    let mut kept: Vec<String> = Vec::new();
    for option in options.iter().map(|option| option.trim()) {
        if option.is_empty() {
            continue;
        }
        if option.chars().count() > MAX_OPTION_LENGTH {
            return Err(format!(
                "An option takes {MAX_OPTION_LENGTH} characters at most."
            ));
        }
        if kept.iter().any(|known| known == option) {
            return Err(format!("“{option}” is there twice."));
        }
        kept.push(option.to_owned());
    }
    if kept.len() < MIN_OPTIONS {
        return Err("A poll needs at least two options.".into());
    }
    Ok((question.to_owned(), kept))
}

impl Shell {
    /// Opens an empty form, with the keyboard in the question.
    pub(super) fn begin_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let question = cx.new(|cx| InputState::new(window, cx).placeholder("Ask a question"));
        let mut form = PollForm {
            question: question.clone(),
            options: Vec::new(),
            multiple: false,
            error: None,
            _subscriptions: Vec::new(),
        };
        // Enter in the question goes on to the first option.
        form._subscriptions.push(cx.subscribe_in(
            &question,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.focus_poll_option(0, window, cx),
                InputEvent::Change => this.poll_changed(cx),
                _ => {}
            },
        ));
        self.poll_form = Some(form);
        for _ in 0..3 {
            self.add_poll_option(window, cx);
        }
        question.update(cx, |field, cx| field.focus(window, cx));
    }

    fn poll_changed(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.poll_form {
            form.error = None;
        }
        cx.notify();
    }

    /// One more option field, up to what WhatsApp takes.
    pub(super) fn add_poll_option(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(count) = self.poll_form.as_ref().map(|form| form.options.len()) else {
            return;
        };
        if count >= MAX_OPTIONS {
            return;
        }
        let field =
            cx.new(|cx| InputState::new(window, cx).placeholder(format!("Option {}", count + 1)));
        // Enter goes on to the next option, adding one after the last.
        let subscription = cx.subscribe_in(
            &field,
            window,
            move |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.focus_poll_option(count + 1, window, cx),
                InputEvent::Change => this.poll_changed(cx),
                _ => {}
            },
        );
        if let Some(form) = &mut self.poll_form {
            form.options.push(field);
            form._subscriptions.push(subscription);
        }
        cx.notify();
    }

    fn focus_poll_option(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.poll_form.as_ref().map_or(0, |form| form.options.len());
        if index >= count {
            self.add_poll_option(window, cx);
        }
        let field = self
            .poll_form
            .as_ref()
            .and_then(|form| form.options.get(index).or(form.options.last()).cloned());
        if let Some(field) = field {
            field.update(cx, |field, cx| field.focus(window, cx));
        }
    }

    pub(super) fn toggle_poll_multiple(&mut self, cx: &mut Context<Self>) {
        if let Some(form) = &mut self.poll_form {
            form.multiple = !form.multiple;
        }
        cx.notify();
    }

    /// Queues the poll for the open chat and closes the form; or says what
    /// is missing and keeps it open.
    pub(super) fn send_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(open), Some(form)) = (&self.open, &self.poll_form) else {
            return;
        };
        let typed: Vec<String> = form
            .options
            .iter()
            .map(|field| field.read(cx).value().to_string())
            .collect();
        let question = form.question.read(cx).value().to_string();
        let multiple = form.multiple;
        let (question, options) = match checked(&question, &typed) {
            Ok(poll) => poll,
            Err(problem) => {
                // Said under the fields, with the keyboard back in the
                // first one that still needs something.
                let missing = if question.trim().is_empty() {
                    Some(form.question.clone())
                } else {
                    form.options
                        .iter()
                        .find(|field| field.read(cx).value().trim().is_empty())
                        .cloned()
                };
                if let Some(form) = &mut self.poll_form {
                    form.error = Some(problem.into());
                }
                if let Some(field) = missing {
                    field.update(cx, |field, cx| field.focus(window, cx));
                }
                return cx.notify();
            }
        };
        // Into the outbox: the bubble appears now, the network comes later.
        let queued = self.engine.send(OutgoingMessage {
            client_id: new_client_id(),
            account_id: open.chat.account_id.clone(),
            chat_id: open.chat.id.clone(),
            content: OutgoingContent::Poll {
                question,
                options,
                max_choices: if multiple { 0 } else { 1 },
            },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: false,
        });
        match queued {
            Ok(_) => {
                open.list.scroll_to_end();
                self.poll_form = None;
                self.close_overlay(window, cx);
            }
            Err(error) => {
                tracing::error!(%error, "could not queue the poll");
                if let Some(form) = &mut self.poll_form {
                    form.error = Some("The poll could not be saved. Try again.".into());
                }
            }
        }
        cx.notify();
    }

    /// The form, as a card over the conversation.
    pub(super) fn render_poll_form(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let form = self.poll_form.as_ref()?;
        let field = |input: &Entity<InputState>| {
            div()
                .h(px(36.))
                .px_2()
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(palette.border)
                .bg(palette.background)
                .flex()
                .items_center()
                .child(super::widgets::field(input))
        };
        let mut options = div().flex().flex_col().gap_2();
        for (index, option) in form.options.iter().enumerate() {
            options = options
                .child(field(option).debug_selector(move || format!("poll-form-option-{index}")));
        }
        let can_add = form.options.len() < MAX_OPTIONS;
        Some(
            self.card("poll-form", palette)
                .w(metrics::FORM_WIDTH())
                .max_w_full()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                // A click inside the card is not a click outside it.
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(label("New poll", palette))
                .child(field(&form.question).debug_selector(|| "poll-form-question".into()))
                .child(label("Options", palette))
                .child(options)
                .when(can_add, |this| {
                    this.child(
                        div().flex().child(
                            text_button(
                                "poll-form-add",
                                "Add option",
                                Some(IconName::Plus),
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.add_poll_option(window, cx);
                                },
                            )),
                        ),
                    )
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .min_w_0()
                                .text_size(metrics::TEXT_BODY())
                                .child("Allow several answers"),
                        )
                        .child(
                            switch("poll-form-multiple", form.multiple, palette).on_click(
                                cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_poll_multiple(cx);
                                }),
                            ),
                        ),
                )
                .children(form.error.clone().map(|error| {
                    div()
                        .debug_selector(|| "poll-form-error".into())
                        .text_size(metrics::TEXT_SMALL())
                        .line_height(px(18.))
                        .text_color(palette.danger)
                        .child(error)
                }))
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            text_button("poll-form-cancel", "Cancel", None, false, palette)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_overlay(window, cx);
                                })),
                        )
                        .child(
                            text_button(
                                "poll-form-send",
                                "Send poll",
                                Some(IconName::SendHorizontal),
                                true,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.send_poll(window, cx);
                                },
                            )),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::checked;

    fn options(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn a_poll_needs_a_question_and_two_different_options() {
        assert_eq!(
            checked(" Lunch? ", &options(&["Pizza", "", " Sushi "])),
            Ok(("Lunch?".to_owned(), options(&["Pizza", "Sushi"])))
        );
        for (question, typed, said) in [
            ("", &["a", "b"][..], "question"),
            ("Lunch?", &["a", " "][..], "two options"),
            ("Lunch?", &["a", "b", "a"][..], "twice"),
        ] {
            let problem = checked(question, &options(typed)).unwrap_err();
            assert!(problem.contains(said), "{problem}");
        }
        let long = "x".repeat(101);
        assert!(checked("Lunch?", &options(&["a", &long])).is_err());
        assert!(checked(&"q".repeat(256), &options(&["a", "b"])).is_err());
    }
}
