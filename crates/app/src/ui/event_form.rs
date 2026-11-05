//! "New event": what it is called, when it starts (and, if it is said,
//! when it ends), what it is about and where. It is sent like any
//! message: into the outbox, then to the provider with the same
//! idempotency key until it is accepted.

use super::shell::Shell;
use super::widgets::{label, text_button};
use crate::icons::IconName;
use crate::theme::{metrics, px, Palette};
use chrono::{DateTime, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Timelike};
use client_core::new_client_id;
use client_provider::{CalendarEvent, EventPlace, OutgoingContent, OutgoingMessage, Timestamp};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::prelude::*;
use gpui_kit::{div, Context, Div, Entity, SharedString, Stateful, Subscription, Window};

/// The longest a name may be, in characters.
const MAX_NAME_LENGTH: usize = 255;
/// The longest a description may be, in characters.
const MAX_DESCRIPTION_LENGTH: usize = 2048;
/// How dates and times are typed, and shown in the fields.
const DATE_FORMAT: &str = "%Y-%m-%d";
const TIME_FORMAT: &str = "%H:%M";

/// The event being written.
pub(super) struct EventForm {
    pub(super) name: Entity<InputState>,
    pub(super) start_date: Entity<InputState>,
    pub(super) start_time: Entity<InputState>,
    pub(super) end_date: Entity<InputState>,
    pub(super) end_time: Entity<InputState>,
    pub(super) description: Entity<InputState>,
    pub(super) place: Entity<InputState>,
    /// What is wrong with it, said under the fields.
    pub(super) error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

/// What was typed, as text.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct Typed {
    pub(super) name: String,
    pub(super) start_date: String,
    pub(super) start_time: String,
    pub(super) end_date: String,
    pub(super) end_time: String,
    pub(super) description: String,
    pub(super) place: String,
}

/// Which field a problem is about, so the keyboard goes back to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Field {
    Name,
    StartDate,
    StartTime,
    EndDate,
    EndTime,
    Description,
}

/// The next whole hour after `now`: where a new event starts unless it is
/// said otherwise.
pub(super) fn next_hour(now: NaiveDateTime) -> NaiveDateTime {
    let hour = now
        .date()
        .and_time(NaiveTime::from_hms_opt(now.hour(), 0, 0).expect("a whole hour"));
    hour + Duration::hours(1)
}

fn date(text: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(text.trim(), DATE_FORMAT).ok()
}

/// A time as "18:30", "18.30" or "18".
fn time(text: &str) -> Option<NaiveTime> {
    let text = text.trim().replace('.', ":");
    NaiveTime::parse_from_str(&text, TIME_FORMAT)
        .ok()
        .or_else(|| {
            let hour: u32 = text.parse().ok()?;
            NaiveTime::from_hms_opt(hour, 0, 0)
        })
}

/// A local wall-clock moment as a point in time. A moment a change of
/// the clocks skips is not one; one it repeats is the first.
fn instant<Tz: TimeZone>(zone: &Tz, at: NaiveDateTime) -> Option<DateTime<Tz>> {
    zone.from_local_datetime(&at).earliest()
}

/// What was typed, checked against `now` in `zone`: the event to send, or
/// what is wrong, in words, and the field it is about.
pub(super) fn checked<Tz: TimeZone>(
    typed: &Typed,
    now: DateTime<Tz>,
    zone: &Tz,
) -> Result<CalendarEvent, (Field, String)> {
    let problem = |field, key: &str| Err((field, crate::i18n::t(key).to_owned()));
    let name = typed.name.trim();
    if name.is_empty() {
        return problem(Field::Name, "chat.event_form.write_name");
    }
    if name.chars().count() > MAX_NAME_LENGTH {
        return Err((
            Field::Name,
            crate::i18n::tr("chat.event_form.name_max", &[("max", &MAX_NAME_LENGTH)]),
        ));
    }
    let Some(start_date) = date(&typed.start_date) else {
        return problem(Field::StartDate, "chat.event_form.bad_date");
    };
    let Some(start_time) = time(&typed.start_time) else {
        return problem(Field::StartTime, "chat.event_form.bad_time");
    };
    let Some(starts) = instant(zone, start_date.and_time(start_time)) else {
        return problem(Field::StartTime, "chat.event_form.bad_time");
    };
    if starts < now {
        return problem(Field::StartTime, "chat.event_form.in_the_past");
    }
    // An end is said with a time; its date is the start's unless given.
    let ends = match (typed.end_date.trim(), typed.end_time.trim()) {
        ("", "") => None,
        (_, "") => return problem(Field::EndTime, "chat.event_form.bad_time"),
        (end_date, end_time) => {
            let day = if end_date.is_empty() {
                Some(start_date)
            } else {
                date(end_date)
            };
            let Some(day) = day else {
                return problem(Field::EndDate, "chat.event_form.bad_date");
            };
            let Some(at) = time(end_time) else {
                return problem(Field::EndTime, "chat.event_form.bad_time");
            };
            let Some(ends) = instant(zone, day.and_time(at)) else {
                return problem(Field::EndTime, "chat.event_form.bad_time");
            };
            if ends <= starts {
                return problem(Field::EndTime, "chat.event_form.ends_before");
            }
            Some(ends)
        }
    };
    let description = typed.description.trim();
    if description.chars().count() > MAX_DESCRIPTION_LENGTH {
        return Err((
            Field::Description,
            crate::i18n::tr(
                "chat.event_form.description_max",
                &[("max", &MAX_DESCRIPTION_LENGTH)],
            ),
        ));
    }
    let place = typed.place.trim();
    let millis = |at: DateTime<Tz>| Timestamp::from_millis(at.timestamp_millis());
    Ok(CalendarEvent {
        title: name.to_owned(),
        description: (!description.is_empty()).then(|| description.to_owned()),
        starts_at: Some(millis(starts)),
        ends_at: ends.map(millis),
        place: (!place.is_empty()).then(|| EventPlace {
            name: Some(place.to_owned()),
            address: None,
            point: None,
        }),
        call: None,
        join_url: None,
        cancelled: false,
    })
}

impl Shell {
    /// Opens an empty form starting at the next whole hour, with the
    /// keyboard in the name.
    pub(super) fn begin_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let starts = next_hour(Local::now().naive_local());
        let mut field = |placeholder: &str, value: Option<String>, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_owned();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx).placeholder(placeholder);
                if let Some(value) = value {
                    state.set_value(value, window, cx);
                }
                state
            })
        };
        let name = field(crate::i18n::t("chat.event_form.name"), None, cx);
        let start_date = field(
            crate::i18n::t("chat.event_form.date_hint"),
            Some(starts.format(DATE_FORMAT).to_string()),
            cx,
        );
        let start_time = field(
            crate::i18n::t("chat.event_form.time_hint"),
            Some(starts.format(TIME_FORMAT).to_string()),
            cx,
        );
        let end_date = field(crate::i18n::t("chat.event_form.date_hint"), None, cx);
        let end_time = field(crate::i18n::t("chat.event_form.time_hint"), None, cx);
        let description = field(crate::i18n::t("chat.event_form.description"), None, cx);
        let place = field(crate::i18n::t("chat.event_form.place"), None, cx);
        let order = [
            name.clone(),
            start_date.clone(),
            start_time.clone(),
            end_date.clone(),
            end_time.clone(),
            description.clone(),
            place.clone(),
        ];
        let mut subscriptions = Vec::new();
        for (index, input) in order.iter().enumerate() {
            let next = order.get(index + 1).cloned();
            subscriptions.push(cx.subscribe_in(
                input,
                window,
                move |this, _, event: &InputEvent, window, cx| match event {
                    // Enter goes on to the next field; in the last, it sends.
                    InputEvent::PressEnter { .. } => match &next {
                        Some(next) => next.update(cx, |field, cx| field.focus(window, cx)),
                        None => this.send_event(window, cx),
                    },
                    InputEvent::Change => {
                        if let Some(form) = &mut this.event_form {
                            form.error = None;
                        }
                        cx.notify();
                    }
                    _ => {}
                },
            ));
        }
        self.event_form = Some(EventForm {
            name: name.clone(),
            start_date,
            start_time,
            end_date,
            end_time,
            description,
            place,
            error: None,
            _subscriptions: subscriptions,
        });
        name.update(cx, |field, cx| field.focus(window, cx));
    }

    /// Queues the event for the open chat and closes the form; or says
    /// what is wrong and keeps it open.
    pub(super) fn send_event(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(open), Some(form)) = (&self.open, &self.event_form) else {
            return;
        };
        let read = |field: &Entity<InputState>| field.read(cx).value().to_string();
        let typed = Typed {
            name: read(&form.name),
            start_date: read(&form.start_date),
            start_time: read(&form.start_time),
            end_date: read(&form.end_date),
            end_time: read(&form.end_time),
            description: read(&form.description),
            place: read(&form.place),
        };
        let event = match checked(&typed, Local::now(), &Local) {
            Ok(event) => event,
            Err((field, problem)) => {
                let input = match field {
                    Field::Name => form.name.clone(),
                    Field::StartDate => form.start_date.clone(),
                    Field::StartTime => form.start_time.clone(),
                    Field::EndDate => form.end_date.clone(),
                    Field::EndTime => form.end_time.clone(),
                    Field::Description => form.description.clone(),
                };
                if let Some(form) = &mut self.event_form {
                    form.error = Some(problem.into());
                }
                input.update(cx, |field, cx| field.focus(window, cx));
                return cx.notify();
            }
        };
        // Into the outbox, off this thread: the bubble appears as soon as
        // it is queued, the network comes later.
        self.queue_send(super::shell::Outgoing::Message(OutgoingMessage {
            client_id: new_client_id(),
            account_id: open.chat.account_id.clone(),
            chat_id: open.chat.id.clone(),
            content: OutgoingContent::Event(event),
            reply_to: None,
            mentions: Vec::new(),
            forwarded: false,
        }));
        open.list.scroll_to_end();
        self.event_form = None;
        self.close_overlay(window, cx);
        cx.notify();
    }

    /// The form, as a card over the conversation.
    pub(super) fn render_event_form(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let form = self.event_form.as_ref()?;
        let field = |input: &Entity<InputState>, name: &'static str| {
            div()
                .debug_selector(move || format!("event-form-{name}"))
                .h(px(36.))
                .min_w_0()
                .px_2()
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(palette.border)
                .bg(palette.background)
                .flex()
                .items_center()
                .child(super::widgets::field(input))
        };
        // A date and a time, side by side.
        let when = |date: &Entity<InputState>,
                    time: &Entity<InputState>,
                    date_name: &'static str,
                    time_name: &'static str| {
            div()
                .flex()
                .gap_2()
                .child(field(date, date_name).flex_1())
                .child(field(time, time_name).w(px(96.)))
        };
        Some(
            self.card("event-form", palette)
                .w(metrics::FORM_WIDTH())
                .max_w_full()
                .p_4()
                .flex()
                .flex_col()
                .gap_3()
                .on_click(|_, _, cx| cx.stop_propagation())
                .child(label(crate::i18n::t("chat.event_form.title"), palette))
                .child(field(&form.name, "name"))
                .child(label(crate::i18n::t("chat.event_form.starts"), palette))
                .child(when(
                    &form.start_date,
                    &form.start_time,
                    "start-date",
                    "start-time",
                ))
                .child(label(crate::i18n::t("chat.event_form.ends"), palette))
                .child(when(&form.end_date, &form.end_time, "end-date", "end-time"))
                .child(field(&form.description, "description"))
                .child(field(&form.place, "place"))
                .children(form.error.clone().map(|error| {
                    div()
                        .debug_selector(|| "event-form-error".into())
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
                            text_button(
                                "event-form-cancel",
                                crate::i18n::t("chat.event_form.cancel"),
                                None,
                                false,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_overlay(window, cx);
                                },
                            )),
                        )
                        .child(
                            text_button(
                                "event-form-send",
                                crate::i18n::t("chat.event_form.send"),
                                Some(IconName::SendHorizontal),
                                true,
                                palette,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.send_event(window, cx);
                                },
                            )),
                        ),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{checked, next_hour, Field, Typed};
    use chrono::{NaiveDate, TimeZone, Utc};

    fn typed(name: &str, start: (&str, &str), end: (&str, &str)) -> Typed {
        Typed {
            name: name.into(),
            start_date: start.0.into(),
            start_time: start.1.into(),
            end_date: end.0.into(),
            end_time: end.1.into(),
            description: " Bring snacks ".into(),
            place: "".into(),
        }
    }

    #[test]
    fn a_new_event_starts_at_the_next_whole_hour() {
        let now = NaiveDate::from_ymd_opt(2026, 11, 4)
            .unwrap()
            .and_hms_opt(23, 20, 5)
            .unwrap();
        assert_eq!(
            next_hour(now),
            NaiveDate::from_ymd_opt(2026, 11, 5)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
        );
    }

    #[test]
    fn an_event_needs_a_name_and_a_start_to_come_and_ends_after_it() {
        let now = Utc.with_ymd_and_hms(2026, 11, 4, 12, 0, 0).unwrap();
        let event = checked(
            &typed(" Dinner ", ("2026-11-05", "20:30"), ("", "22")),
            now,
            &Utc,
        )
        .unwrap();
        assert_eq!(event.title, "Dinner");
        assert_eq!(event.description.as_deref(), Some("Bring snacks"));
        assert!(event.place.is_none());
        let at = |h, m| {
            Utc.with_ymd_and_hms(2026, 11, 5, h, m, 0)
                .unwrap()
                .timestamp_millis()
        };
        assert_eq!(event.starts_at.unwrap().as_millis(), at(20, 30));
        // The end's day is the start's when it is not said.
        assert_eq!(event.ends_at.unwrap().as_millis(), at(22, 0));
        for (typed, field) in [
            (typed("", ("2026-11-05", "20:30"), ("", "")), Field::Name),
            (typed("x", ("5/11", "20:30"), ("", "")), Field::StartDate),
            (
                typed("x", ("2026-11-05", "25:00"), ("", "")),
                Field::StartTime,
            ),
            (
                typed("x", ("2026-11-03", "20:30"), ("", "")),
                Field::StartTime,
            ),
            (
                typed("x", ("2026-11-05", "20:30"), ("", "19:00")),
                Field::EndTime,
            ),
            (
                typed("x", ("2026-11-05", "20:30"), ("2026-11-06", "")),
                Field::EndTime,
            ),
        ] {
            assert_eq!(
                checked(&typed, now, &Utc).unwrap_err().0,
                field,
                "{typed:?}"
            );
        }
        // An end on a later day may be earlier in the day.
        let late = checked(
            &typed("x", ("2026-11-05", "23:00"), ("2026-11-06", "01:00")),
            now,
            &Utc,
        )
        .unwrap();
        assert!(late.ends_at.unwrap() > late.starts_at.unwrap());
    }
}
