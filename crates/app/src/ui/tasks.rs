//! Tasks: what the user has to do, written here or made from a message.
//!
//! The list is a pane beside the conversation, so a chat and its tasks are
//! read at the same time; it stays open across chats and starts, until it
//! is closed (`Settings::tasks_open`). The rail has its button, with how
//! many are due today or late, and Alt+T opens and closes it. In a narrow
//! window the pane lies over the conversation instead of squeezing it.
//!
//! A task is made from a message with "Create task" in its menu (T): the
//! sheet opens with the message as the title and keeps where it came
//! from, which the task then shows and goes back to. The tasks are kept
//! in the encrypted store (`client-core`'s `store/tasks.rs`); WhatsApp has
//! no tasks, so nothing of them leaves this computer.

use super::menus::panel_header;
use super::shell::{Overlay, Shell};
use super::social::hint;
use super::widgets::{icon_button, label, mono, text_button};
use crate::i18n;
use crate::icons::{icon, IconName};
use crate::settings;
use crate::theme::px;
use crate::theme::{metrics, Palette};
use chrono::{DateTime, Datelike, Duration as Days, Local, NaiveDate, TimeZone};
use client_core::{message_preview, Task, TaskDraft, TaskSource};
use client_provider::{Message, MessageId, Timestamp};
use gpui_kit::component::input::{InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::Sizable as _;
use gpui_kit::prelude::*;
use gpui_kit::{
    div, Context, Div, Entity, SharedString, Stateful, Subscription, Task as Job, Window,
};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

/// The pane's width.
const PANE_WIDTH: f32 = 340.;
/// Below this width of the window the pane lies over the conversation.
const DOCKED_FROM: f32 = 1000.;
/// The longest a title made from a message starts as.
const TITLE_FROM_MESSAGE: usize = 140;
/// How long a task just made stays lit.
const FLASH: Duration = Duration::from_millis(1600);

// ----- days ------------------------------------------------------------------

fn local(ts: Timestamp) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(ts.as_millis())
        .single()
        .unwrap_or_else(Local::now)
}

/// The start of a day, here.
fn start_of(day: NaiveDate) -> Timestamp {
    let midnight = day.and_hms_opt(0, 0, 0).expect("midnight exists");
    let at = Local
        .from_local_datetime(&midnight)
        .earliest()
        .unwrap_or_else(|| Local.from_utc_datetime(&midnight));
    Timestamp::from_millis(at.timestamp_millis())
}

fn day_of(ts: Timestamp) -> NaiveDate {
    local(ts).date_naive()
}

/// The days the sheet offers in one click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QuickDay {
    None,
    Today,
    Tomorrow,
    NextWeek,
}

impl QuickDay {
    const ALL: [QuickDay; 4] = [
        QuickDay::None,
        QuickDay::Today,
        QuickDay::Tomorrow,
        QuickDay::NextWeek,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::None => i18n::t("groups.tasks.no_date"),
            Self::Today => i18n::t("groups.tasks.today"),
            Self::Tomorrow => i18n::t("groups.tasks.tomorrow"),
            Self::NextWeek => i18n::t("groups.tasks.next_week"),
        }
    }

    fn selector(self) -> &'static str {
        match self {
            Self::None => "task-due-none",
            Self::Today => "task-due-today",
            Self::Tomorrow => "task-due-tomorrow",
            Self::NextWeek => "task-due-next-week",
        }
    }

    /// The day it means, seen from `today`. "Next week" is the next
    /// Monday (a week on, from a Monday).
    pub(super) fn day(self, today: NaiveDate) -> Option<NaiveDate> {
        match self {
            Self::None => None,
            Self::Today => Some(today),
            Self::Tomorrow => Some(today + Days::days(1)),
            Self::NextWeek => {
                let ahead = 7 - today.weekday().num_days_from_monday() as i64;
                Some(today + Days::days(ahead))
            }
        }
    }
}

/// Where a task stands against today.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum When {
    Late,
    Today,
    Later,
    Undated,
}

pub(super) fn when(due: Option<Timestamp>, today: NaiveDate) -> When {
    match due.map(day_of) {
        None => When::Undated,
        Some(day) if day < today => When::Late,
        Some(day) if day == today => When::Today,
        Some(_) => When::Later,
    }
}

/// A due day in words, seen from `today`: "Today", "Tomorrow",
/// "Yesterday", "3 days late", a weekday within the week, a date after.
pub(super) fn due_words(due: Timestamp, today: NaiveDate) -> String {
    let day = day_of(due);
    let ahead = (day - today).num_days();
    match ahead {
        0 => i18n::t("groups.tasks.today").into(),
        1 => i18n::t("groups.tasks.tomorrow").into(),
        -1 => i18n::t("groups.tasks.yesterday").into(),
        ahead if ahead < 0 => i18n::plural("groups.tasks.days_late", (-ahead) as u64, &[]),
        2..=6 => crate::format::weekday(day.weekday()).to_owned(),
        _ if day.year() == today.year() => crate::format::day_short(&day, false),
        _ => crate::format::day_short(&day, true),
    }
}

/// The title a task made from a message starts with: its one line,
/// shortened at a word.
pub(super) fn title_from(message: &Message) -> String {
    let line = message_preview(message);
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.chars().count() <= TITLE_FROM_MESSAGE {
        return line;
    }
    let mut cut: String = line.chars().take(TITLE_FROM_MESSAGE).collect();
    if let Some(space) = cut.rfind(' ') {
        cut.truncate(space);
    }
    format!("{cut}…")
}

// ----- state -----------------------------------------------------------------

/// The tasks, as the views show them.
#[derive(Default)]
pub(super) struct TasksUi {
    /// Every task, as the store lists them.
    pub(super) list: Vec<Task>,
    /// Only the open chat's.
    pub(super) this_chat: bool,
    /// The ones done are shown.
    pub(super) show_done: bool,
    /// The field that adds a task.
    pub(super) quick: Option<(Entity<InputState>, Subscription)>,
    /// The sheet that makes or edits one.
    pub(super) sheet: Option<TaskSheet>,
    /// The task just made, lit for a moment.
    pub(super) flash: Option<String>,
    _flash: Option<Job<()>>,
    /// The messages of the open chat a task to do was made from.
    pub(super) tasked: Rc<HashSet<MessageId>>,
}

/// The sheet: a new task, or one being edited.
pub(super) struct TaskSheet {
    /// The task edited; `None` for a new one.
    pub(super) editing: Option<String>,
    pub(super) source: Option<TaskSource>,
    /// The chat the source is in, by name.
    pub(super) source_chat: Option<String>,
    pub(super) title: Entity<InputState>,
    pub(super) notes: Entity<TextareaState>,
    pub(super) due: Option<NaiveDate>,
    /// The month the calendar shows, when it is open.
    pub(super) month: Option<NaiveDate>,
    pub(super) error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    // ----- the list ------------------------------------------------------------

    /// Reads the tasks again.
    pub(super) fn reload_tasks(&mut self) {
        self.tasks.list = self.store.tasks().unwrap_or_else(|error| {
            tracing::error!(%error, "could not read the tasks");
            Vec::new()
        });
        self.tasks.tasked = Rc::new(
            self.open
                .as_ref()
                .and_then(|open| {
                    self.store
                        .tasked_messages(&open.chat.account_id, &open.chat.id)
                        .ok()
                })
                .unwrap_or_default(),
        );
    }

    /// Whether the pane is open.
    pub(super) fn tasks_open(&self, cx: &gpui_kit::App) -> bool {
        settings::get(cx).tasks_open
    }

    /// Opens or closes the pane.
    pub(super) fn toggle_tasks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = !self.tasks_open(cx);
        settings::update(cx, |settings| settings.tasks_open = open);
        if open {
            self.reload_tasks();
            self.focus_quick_task(window, cx);
        }
        cx.notify();
    }

    /// How many tasks to do are due today or late: the rail's count.
    pub(super) fn tasks_due(&self) -> usize {
        let today = Local::now().date_naive();
        self.tasks
            .list
            .iter()
            .filter(|task| task.done_at.is_none())
            .filter(|task| matches!(when(task.due_at, today), When::Late | When::Today))
            .count()
    }

    fn quick_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        if let Some((field, _)) = &self.tasks.quick {
            return field.clone();
        }
        let field = cx.new(|cx| {
            InputState::new(window, cx).placeholder(i18n::t("groups.tasks.add_placeholder"))
        });
        let subscription = cx.subscribe_in(&field, window, |this, field, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                let title = field.read(cx).value().to_string();
                if this.add_task(title, None, None, cx).is_some() {
                    field.update(cx, |field, cx| field.set_value("", window, cx));
                }
            }
        });
        self.tasks.quick = Some((field.clone(), subscription));
        field
    }

    fn focus_quick_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let field = self.quick_field(window, cx);
        field.update(cx, |field, cx| field.focus(window, cx));
    }

    /// Keeps a new task, lights it, and makes sure the pane shows it.
    fn add_task(
        &mut self,
        title: String,
        due: Option<NaiveDate>,
        source: Option<TaskSource>,
        cx: &mut Context<Self>,
    ) -> Option<Task> {
        let draft = TaskDraft {
            title,
            notes: None,
            due_at: due.map(start_of),
        };
        self.keep_task(draft, source, cx)
    }

    fn keep_task(
        &mut self,
        draft: TaskDraft,
        source: Option<TaskSource>,
        cx: &mut Context<Self>,
    ) -> Option<Task> {
        let task = match self
            .store
            .add_task(&draft, source.as_ref(), Timestamp::now())
        {
            Ok(task) => task?,
            Err(error) => {
                tracing::error!(%error, "could not keep the task");
                return None;
            }
        };
        settings::update(cx, |settings| settings.tasks_open = true);
        // Done ones are folded and the filter may hide it: it is shown.
        if self.tasks.this_chat && task.source.is_none() {
            self.tasks.this_chat = false;
        }
        self.reload_tasks();
        self.flash_task(task.id.clone(), cx);
        Some(task)
    }

    fn flash_task(&mut self, id: String, cx: &mut Context<Self>) {
        self.tasks.flash = Some(id);
        self.tasks._flash = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FLASH).await;
            this.update(cx, |this, cx| {
                this.tasks.flash = None;
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn toggle_task(&mut self, id: &str, cx: &mut Context<Self>) {
        let done = self
            .tasks
            .list
            .iter()
            .find(|task| task.id == id)
            .is_some_and(|task| task.done_at.is_none());
        if let Err(error) = self.store.set_task_done(id, done, Timestamp::now()) {
            tracing::error!(%error, "could not mark the task");
        }
        self.reload_tasks();
        cx.notify();
    }

    fn delete_task(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Err(error) = self.store.delete_task(id) {
            tracing::error!(%error, "could not delete the task");
        }
        self.reload_tasks();
        cx.notify();
    }

    /// Goes to the message a task was made from: its number, its chat,
    /// the message itself, lit.
    fn open_task_source(
        &mut self,
        source: &TaskSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.account.as_ref() != Some(&source.account_id) {
            self.select_account(source.account_id.clone(), window, cx);
        }
        let here = self
            .open
            .as_ref()
            .is_some_and(|open| open.chat.id == source.chat_id);
        if !here {
            self.show_chat(source.chat_id.clone(), Some(window), cx);
        }
        self.jump_and_flash(source.message_id.clone(), cx);
    }

    /// The name of the chat a source is in, as the list has it.
    fn chat_name(&self, source: &TaskSource) -> String {
        self.store
            .chat(&source.account_id, &source.chat_id)
            .ok()
            .flatten()
            .map(|chat| chat.title)
            .unwrap_or_else(|| source.chat_id.to_string())
    }

    // ----- the sheet -----------------------------------------------------------

    /// "Create task" on the message in focus.
    pub(super) fn begin_task_from_message(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(message) = self.focused_message() else {
            return false;
        };
        let source = TaskSource {
            account_id: message.account_id.clone(),
            chat_id: message.chat_id.clone(),
            message_id: message.id.clone(),
            quote: message_preview(&message),
        };
        self.open_task_sheet(
            None,
            title_from(&message),
            None,
            None,
            Some(source),
            window,
            cx,
        );
        true
    }

    /// "New task": the sheet, empty.
    pub(super) fn begin_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_task_sheet(None, String::new(), None, None, None, window, cx);
    }

    fn begin_edit_task(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self.tasks.list.iter().find(|task| task.id == id).cloned() else {
            return;
        };
        self.open_task_sheet(
            Some(task.id),
            task.title,
            task.notes,
            task.due_at.map(day_of),
            task.source,
            window,
            cx,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn open_task_sheet(
        &mut self,
        editing: Option<String>,
        title: String,
        notes: Option<String>,
        due: Option<NaiveDate>,
        source: Option<TaskSource>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title_field = cx.new(|cx| {
            let mut field =
                InputState::new(window, cx).placeholder(i18n::t("groups.tasks.title_placeholder"));
            field.set_value(title, window, cx);
            field
        });
        let notes_field = cx.new(|cx| {
            let mut field = TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder(i18n::t("groups.tasks.notes_placeholder"));
            if let Some(notes) = &notes {
                field.set_value(notes.clone(), window, cx);
            }
            field
        });
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &title_field,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => this.save_task_sheet(window, cx),
                InputEvent::Change => {
                    if let Some(sheet) = &mut this.tasks.sheet {
                        sheet.error = None;
                    }
                    cx.notify();
                }
                _ => {}
            },
        ));
        let source_chat = source.as_ref().map(|source| self.chat_name(source));
        self.tasks.sheet = Some(TaskSheet {
            editing,
            source,
            source_chat,
            title: title_field.clone(),
            notes: notes_field,
            due,
            month: None,
            error: None,
            _subscriptions: subscriptions,
        });
        self.open_overlay(Overlay::Task, window, cx);
        title_field.update(cx, |field, cx| field.focus(window, cx));
        cx.notify();
    }

    /// Save on the sheet: a new task, or the one edited.
    pub(super) fn save_task_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &self.tasks.sheet else {
            return;
        };
        let draft = TaskDraft {
            title: sheet.title.read(cx).value().to_string(),
            notes: Some(sheet.notes.read(cx).value().to_string()),
            due_at: sheet.due.map(start_of),
        };
        if draft.title.trim().is_empty() {
            if let Some(sheet) = &mut self.tasks.sheet {
                sheet.error = Some(i18n::t("groups.tasks.title_missing").into());
            }
            cx.notify();
            return;
        }
        match sheet.editing.clone() {
            Some(id) => {
                if let Err(error) = self.store.edit_task(&id, &draft, Timestamp::now()) {
                    tracing::error!(%error, "could not change the task");
                }
                self.reload_tasks();
            }
            None => {
                let source = sheet.source.clone();
                self.keep_task(draft, source, cx);
            }
        }
        self.tasks.sheet = None;
        self.close_overlay(window, cx);
        cx.notify();
    }

    fn set_task_due(&mut self, due: Option<NaiveDate>, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.tasks.sheet {
            sheet.due = due;
            sheet.month = None;
        }
        cx.notify();
    }

    fn toggle_task_calendar(&mut self, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.tasks.sheet {
            sheet.month = match sheet.month {
                Some(_) => None,
                None => {
                    let shown = sheet.due.unwrap_or_else(|| Local::now().date_naive());
                    shown.with_day(1)
                }
            };
        }
        cx.notify();
    }

    fn step_task_month(&mut self, forward: bool, cx: &mut Context<Self>) {
        if let Some(month) = self
            .tasks
            .sheet
            .as_mut()
            .and_then(|sheet| sheet.month.as_mut())
        {
            *month = if forward {
                (*month + Days::days(32)).with_day(1).unwrap_or(*month)
            } else {
                (*month - Days::days(1)).with_day(1).unwrap_or(*month)
            };
        }
        cx.notify();
    }

    pub(super) fn render_task_sheet(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let sheet = self.tasks.sheet.as_ref()?;
        let today = Local::now().date_naive();
        let editing = sheet.editing.clone();

        let mut body = div().p_4().flex().flex_col().gap_3();
        if let (Some(source), Some(chat)) = (&sheet.source, &sheet.source_chat) {
            body = body.child(
                div()
                    .debug_selector(|| "task-sheet-source".into())
                    .px_3()
                    .py_2()
                    .rounded(metrics::RADIUS())
                    .border_l_2()
                    .border_color(palette.accent)
                    .bg(palette.surface)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        mono(i18n::tr(
                            "groups.tasks.from_chat",
                            &[("chat", &chat.to_uppercase())],
                        ))
                        .text_color(palette.text_muted),
                    )
                    .child(
                        div()
                            .text_size(metrics::TEXT_SMALL())
                            .text_color(palette.text)
                            .line_clamp(3)
                            .child(SharedString::from(source.quote.clone())),
                    ),
            );
        }
        let boxed = |child: gpui_kit::AnyElement| {
            div()
                .px_2()
                .py(px(6.))
                .rounded(metrics::RADIUS())
                .border_1()
                .border_color(palette.border)
                .bg(palette.background)
                .child(child)
        };
        body = body
            .child(
                boxed(super::widgets::field(&sheet.title).into_any_element())
                    .debug_selector(|| "task-sheet-title".into()),
            )
            .child(
                boxed(
                    Textarea::new(&sheet.notes)
                        .appearance(false)
                        .small()
                        .into_any_element(),
                )
                .debug_selector(|| "task-sheet-notes".into()),
            );

        // The day: one click for the usual ones, the calendar for any.
        let chosen = sheet.due;
        let mut days = div()
            .flex()
            .flex_wrap()
            .gap_2()
            .child(label(i18n::t("groups.tasks.due"), palette));
        days = div().flex().flex_col().gap_2().child(days);
        let mut chips = div().flex().flex_wrap().gap_2();
        for quick in QuickDay::ALL {
            let day = quick.day(today);
            let on = day == chosen;
            chips = chips.child(
                text_button(quick.selector(), quick.label(), None, on, palette).on_click(
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_task_due(day, cx);
                    }),
                ),
            );
        }
        let custom = chosen.filter(|day| {
            !QuickDay::ALL
                .iter()
                .any(|quick| quick.day(today) == Some(*day))
        });
        chips = chips.child(
            text_button(
                "task-due-pick",
                match custom {
                    Some(day) => due_words(start_of(day), today),
                    None => i18n::t("groups.tasks.pick_day").to_owned(),
                },
                Some(IconName::Calendar),
                custom.is_some(),
                palette,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_task_calendar(cx);
            })),
        );
        days = days.child(chips);
        if let Some(month) = sheet.month {
            days = days.child(self.render_task_calendar(month, chosen, today, palette, cx));
        }
        body = body.child(days);

        if let Some(error) = &sheet.error {
            body = body.child(
                div()
                    .debug_selector(|| "task-sheet-error".into())
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(palette.danger)
                    .child(error.clone()),
            );
        }

        let mut buttons = div().flex().items_center().gap_2();
        if let Some(id) = editing.clone() {
            buttons = buttons.child(
                text_button(
                    "task-sheet-delete",
                    i18n::t("groups.tasks.delete"),
                    Some(IconName::Trash),
                    false,
                    palette,
                )
                .text_color(palette.danger)
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.delete_task(&id, cx);
                    this.tasks.sheet = None;
                    this.close_overlay(window, cx);
                })),
            );
        }
        buttons = buttons
            .child(div().flex_1())
            .child(
                text_button(
                    "task-sheet-cancel",
                    i18n::t("groups.tasks.cancel"),
                    None,
                    false,
                    palette,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.close_overlay(window, cx);
                })),
            )
            .child(
                text_button(
                    "task-sheet-save",
                    if editing.is_some() {
                        i18n::t("groups.tasks.save")
                    } else {
                        i18n::t("groups.tasks.create")
                    },
                    Some(IconName::Check),
                    true,
                    palette,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.save_task_sheet(window, cx);
                })),
            );

        Some(
            self.card("task-sheet", palette)
                .w(px(440.))
                .flex()
                .flex_col()
                .child(panel_header(
                    if editing.is_some() {
                        i18n::t("groups.tasks.edit_title")
                    } else {
                        i18n::t("groups.tasks.new_title")
                    },
                    palette,
                    cx.listener(|this, _, window, cx| this.close_overlay(window, cx)),
                ))
                .child(body.child(buttons)),
        )
    }

    /// A month to pick a day from, Monday first.
    fn render_task_calendar(
        &self,
        month: NaiveDate,
        chosen: Option<NaiveDate>,
        today: NaiveDate,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let first = month.with_day(1).unwrap_or(month);
        let lead = first.weekday().num_days_from_monday() as i64;
        let start = first - Days::days(lead);
        let mut grid = div().flex().flex_wrap().w(px(7. * 36.));
        for name in [
            i18n::t("groups.tasks.weekday_mon"),
            i18n::t("groups.tasks.weekday_tue"),
            i18n::t("groups.tasks.weekday_wed"),
            i18n::t("groups.tasks.weekday_thu"),
            i18n::t("groups.tasks.weekday_fri"),
            i18n::t("groups.tasks.weekday_sat"),
            i18n::t("groups.tasks.weekday_sun"),
        ] {
            grid = grid.child(
                div()
                    .w(px(36.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(mono(name).text_color(palette.text_muted)),
            );
        }
        for index in 0..42 {
            let day = start + Days::days(index);
            if index >= 35 && day.month() != first.month() {
                break;
            }
            let inside = day.month() == first.month();
            let on = Some(day) == chosen;
            let (hover, accent) = (palette.hover, palette.accent);
            grid = grid.child(
                div()
                    .id(SharedString::from(format!("task-day-{day}")))
                    .debug_selector(move || format!("task-day-{day}"))
                    .w(px(36.))
                    .h(px(30.))
                    .rounded(metrics::RADIUS())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .text_size(metrics::TEXT_SMALL())
                    .text_color(if inside {
                        palette.text
                    } else {
                        palette.text_muted
                    })
                    .when(day == today, |this| this.border_1().border_color(accent))
                    .when(on, |this| this.bg(accent).text_color(palette.background))
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_task_due(Some(day), cx);
                    }))
                    .child(SharedString::from(day.day().to_string())),
            );
        }
        div()
            .debug_selector(|| "task-calendar".into())
            .p_2()
            .rounded(metrics::RADIUS())
            .border_1()
            .border_color(palette.border)
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        icon_button("task-month-previous", IconName::ChevronLeft, palette)
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.step_task_month(false, cx);
                            })),
                    )
                    .child(
                        div()
                            .text_size(metrics::TEXT_SMALL())
                            .child(SharedString::from(crate::format::month_of(&first))),
                    )
                    .child(
                        icon_button("task-month-next", IconName::ChevronRight, palette).on_click(
                            cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.step_task_month(true, cx);
                            }),
                        ),
                    ),
            )
            .child(grid)
    }

    // ----- the pane ------------------------------------------------------------

    /// The pane beside the conversation, while it is open. `None` when it
    /// is closed.
    pub(super) fn render_tasks_pane(
        &mut self,
        palette: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        if !self.tasks_open(cx) {
            return None;
        }
        let quick = self.quick_field(window, cx);
        let today = Local::now().date_naive();
        let open_chat = self.open.as_ref().map(|open| {
            (
                open.chat.account_id.clone(),
                open.chat.id.clone(),
                open.chat.title.clone(),
            )
        });
        let this_chat = self.tasks.this_chat && open_chat.is_some();
        let shown: Vec<&Task> = self
            .tasks
            .list
            .iter()
            .filter(|task| {
                !this_chat
                    || task.source.as_ref().is_some_and(|source| {
                        open_chat.as_ref().is_some_and(|(account, chat, _)| {
                            &source.account_id == account && &source.chat_id == chat
                        })
                    })
            })
            .collect();
        let to_do: Vec<&Task> = shown
            .iter()
            .copied()
            .filter(|task| task.done_at.is_none())
            .collect();
        let done: Vec<&Task> = shown
            .iter()
            .copied()
            .filter(|task| task.done_at.is_some())
            .collect();

        let header = div()
            .flex_none()
            .h(metrics::HEADER_HEIGHT())
            .px_3()
            .border_b_1()
            .border_color(palette.border)
            .flex()
            .items_center()
            .gap_2()
            .child(icon(IconName::SquareCheck, px(16.), palette.icon))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(i18n::t("groups.tasks.heading")),
            )
            .child(
                mono(to_do.len().to_string())
                    .debug_selector(|| "tasks-count".into())
                    .text_color(palette.text_muted),
            )
            .child(
                icon_button("tasks-new", IconName::Plus, palette)
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                            "groups.tasks.new_tooltip",
                        ))
                        .build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.begin_task(window, cx);
                    })),
            )
            .child(
                icon_button("tasks-close", IconName::X, palette)
                    .tooltip(|window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(i18n::t(
                            "groups.tasks.close_tooltip",
                        ))
                        .build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.toggle_tasks(window, cx);
                    })),
            );

        // The field that adds one, and which tasks are shown.
        let add = div()
            .flex_none()
            .px_3()
            .pt_3()
            .pb_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .debug_selector(|| "tasks-add".into())
                    .h(px(34.))
                    .px_2()
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(palette.border)
                    .bg(palette.background)
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon(IconName::Plus, px(14.), palette.icon))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(super::widgets::field(&quick)),
                    ),
            )
            .when_some(open_chat.clone(), |this, (_, _, title)| {
                this.child(super::widgets::segmented(
                    "tasks-filter",
                    &[
                        (false, i18n::t("groups.tasks.filter_all")),
                        (true, i18n::t("groups.tasks.filter_this_chat")),
                    ],
                    this_chat,
                    palette,
                    {
                        let view = cx.weak_entity();
                        move |chat_only, _, cx| {
                            view.update(cx, |this, cx| {
                                this.tasks.this_chat = chat_only;
                                cx.notify();
                            })
                            .ok();
                        }
                    },
                ))
                .when(this_chat, |this| {
                    this.child(hint(
                        i18n::tr("groups.tasks.from_messages_of", &[("title", &title)]),
                        palette,
                    ))
                })
            });

        let mut list = div().flex().flex_col().pb_4();
        if to_do.is_empty() && done.is_empty() {
            list = list.child(
                div()
                    .debug_selector(|| "tasks-empty".into())
                    .px_4()
                    .py_6()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .text_center()
                    .child(icon(IconName::SquareCheck, px(28.), palette.icon))
                    .child(hint(
                        if this_chat {
                            i18n::t("groups.tasks.empty_chat")
                        } else {
                            i18n::t("groups.tasks.empty")
                        },
                        palette,
                    ))
                    .child(hint(i18n::t("groups.tasks.empty_help"), palette)),
            );
        }
        for (group, title) in [
            (When::Late, i18n::t("groups.tasks.group_late")),
            (When::Today, i18n::t("groups.tasks.today")),
            (When::Later, i18n::t("groups.tasks.group_later")),
            (When::Undated, i18n::t("groups.tasks.no_date")),
        ] {
            let in_group: Vec<&Task> = to_do
                .iter()
                .copied()
                .filter(|task| when(task.due_at, today) == group)
                .collect();
            if in_group.is_empty() {
                continue;
            }
            list = list.child(
                div().px_3().pt_3().pb_1().child(
                    label(title, palette)
                        .when(group == When::Late, |this| this.text_color(palette.danger)),
                ),
            );
            for task in in_group {
                list = list.child(self.render_task_row(task, today, palette, cx));
            }
        }
        if !done.is_empty() {
            let open = self.tasks.show_done;
            list = list.child(
                div()
                    .id("tasks-done")
                    .debug_selector(|| "tasks-done".into())
                    .mx_2()
                    .mt_3()
                    .px_1()
                    .py_1()
                    .rounded(metrics::RADIUS())
                    .cursor_pointer()
                    .hover(|style| style.opacity(0.8))
                    .flex()
                    .items_center()
                    .gap_2()
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.tasks.show_done = !this.tasks.show_done;
                        cx.notify();
                    }))
                    .child(icon(
                        if open {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        },
                        px(14.),
                        palette.icon,
                    ))
                    .child(label(
                        &i18n::tr("groups.tasks.done", &[("n", &done.len())]),
                        palette,
                    )),
            );
            if open {
                for task in done {
                    list = list.child(self.render_task_row(task, today, palette, cx));
                }
            }
        }

        let docked = self.viewport.width.as_f32() >= DOCKED_FROM;
        let pane = div()
            .debug_selector(|| "tasks-pane".into())
            .flex_none()
            .w(px(PANE_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(palette.border)
            .bg(palette.background)
            .flex()
            .flex_col()
            .child(header)
            .child(add)
            .child(
                div()
                    .id("tasks-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list),
            );
        Some(if docked {
            pane
        } else {
            // Over the conversation, at the window's right edge.
            div()
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .shadow_lg()
                .child(pane)
        })
    }

    fn render_task_row(
        &self,
        task: &Task,
        today: NaiveDate,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let done = task.done_at.is_some();
        let lit = self.tasks.flash.as_deref() == Some(task.id.as_str());
        let group = SharedString::from(format!("task-{}", task.id));
        let (check_id, edit_id, delete_id) = (task.id.clone(), task.id.clone(), task.id.clone());
        let open_id = task.id.clone();
        let hover = palette.hover;

        let mut meta = div().flex().flex_wrap().items_center().gap_2();
        if let Some(due) = task.due_at.filter(|_| !done) {
            let colour = match when(Some(due), today) {
                When::Late => palette.danger,
                When::Today => palette.accent,
                _ => palette.text_muted,
            };
            meta = meta.child(
                div()
                    .debug_selector(|| "task-due".into())
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_color(colour)
                    .child(icon(IconName::Calendar, px(11.), colour))
                    .child(mono(due_words(due, today)).text_color(colour)),
            );
        }
        if let Some(source) = &task.source {
            let source = source.clone();
            let chat = self.chat_name(&source);
            let quote = SharedString::from(source.quote.clone());
            meta = meta.child(
                div()
                    .id(SharedString::from(format!("task-source-{}", task.id)))
                    .debug_selector(|| "task-source".into())
                    .min_w_0()
                    .max_w(px(220.))
                    .px_1()
                    .rounded(px(3.))
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .text_color(palette.text_muted)
                    .hover(move |style| style.bg(hover))
                    .tooltip(move |window, cx| {
                        gpui_kit::component::tooltip::Tooltip::new(quote.clone()).build(window, cx)
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.open_task_source(&source, window, cx);
                    }))
                    .child(icon(IconName::MessageCircle, px(11.), palette.icon))
                    .child(
                        div()
                            .truncate()
                            .text_size(metrics::TEXT_META())
                            .child(SharedString::from(chat)),
                    ),
            );
        }
        if task.notes.is_some() {
            meta = meta.child(icon(IconName::FileText, px(11.), palette.icon));
        }

        let action = |id: &'static str, glyph: IconName, hint: &'static str| {
            div()
                .id(SharedString::from(format!("{id}-{}", task.id)))
                .debug_selector(move || id.into())
                .flex_none()
                .size(px(24.))
                .rounded(metrics::RADIUS())
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(hint).build(window, cx)
                })
                .child(icon(glyph, px(13.), palette.icon))
        };

        div()
            .id(SharedString::from(format!("task-{}", task.id)))
            .debug_selector(|| "task-row".into())
            .group(group.clone())
            .mx_2()
            .px_2()
            .py(px(6.))
            .rounded(metrics::RADIUS())
            .flex()
            .items_start()
            .gap_2()
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .when(lit, |this| this.bg(palette.selection_in))
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.begin_edit_task(&open_id, window, cx);
            }))
            .child(
                div()
                    .id(SharedString::from(format!("task-check-{}", task.id)))
                    .debug_selector(|| "task-check".into())
                    .flex_none()
                    .pt(px(1.))
                    .cursor_pointer()
                    .tab_index(0)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_task(&check_id, cx);
                    }))
                    .child(icon(
                        if done {
                            IconName::SquareCheck
                        } else {
                            IconName::Square
                        },
                        px(16.),
                        if done { palette.accent } else { palette.icon },
                    )),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .debug_selector(|| "task-title".into())
                            .text_size(metrics::TEXT_BODY())
                            .line_clamp(2)
                            .text_color(if done {
                                palette.text_muted
                            } else {
                                palette.text
                            })
                            .when(done, |this| this.line_through())
                            .child(SharedString::from(task.title.clone())),
                    )
                    .child(meta),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .invisible()
                    .group_hover(group, |style| style.visible())
                    .child(
                        action("task-edit", IconName::Pencil, i18n::t("groups.tasks.edit"))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.begin_edit_task(&edit_id, window, cx);
                            })),
                    )
                    .child(
                        action(
                            "task-delete",
                            IconName::Trash,
                            i18n::t("groups.tasks.delete"),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.delete_task(&delete_id, cx);
                        })),
                    ),
            )
    }

    /// The rail's way in: the button, lit while the pane is open, with
    /// how many tasks are due today or late.
    pub(super) fn render_tasks_rail_button(
        &self,
        palette: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = self.tasks_open(cx);
        let due = self.tasks_due();
        let (hover, ring) = (palette.muted, palette.accent);
        div()
            .debug_selector(|| "rail-tasks".into())
            .flex_none()
            .relative()
            .w_full()
            .py(px(6.))
            .border_b_1()
            .border_color(palette.border)
            .flex()
            .justify_center()
            .when(open, |this| {
                this.child(
                    div()
                        .debug_selector(|| "nav-on-tasks".into())
                        .absolute()
                        .left_0()
                        .top(px(6.))
                        .bottom(px(6.))
                        .w(px(2.))
                        .bg(palette.accent),
                )
            })
            .child(
                div()
                    .id("nav-tasks")
                    .debug_selector(|| "nav-tasks".into())
                    .relative()
                    .flex_none()
                    .size(metrics::CONTROL())
                    .rounded(metrics::RADIUS())
                    .border_1()
                    .border_color(gpui_kit::transparent_black())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .tab_index(0)
                    .hover(move |style| style.bg(hover))
                    .focus_visible(move |style| style.border_color(ring))
                    .when(open, |this| this.bg(palette.muted))
                    .tooltip(move |window, cx| {
                        let words = match due {
                            0 => i18n::t("groups.tasks.tooltip_none").to_owned(),
                            n => i18n::plural("groups.tasks.tooltip_due", n as u64, &[]),
                        };
                        gpui_kit::component::tooltip::Tooltip::new(words).build(window, cx)
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_tasks(window, cx)))
                    .child(icon(
                        IconName::SquareCheck,
                        px(18.),
                        if open { palette.text } else { palette.icon },
                    ))
                    .when(due > 0, |this| {
                        this.child(
                            div()
                                .debug_selector(|| "tasks-due-count".into())
                                .absolute()
                                .top(px(-3.))
                                .right(px(-4.))
                                .min_w(px(16.))
                                .h(px(16.))
                                .px(px(4.))
                                .rounded_full()
                                .bg(palette.accent)
                                .text_color(palette.background)
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_size(px(10.))
                                .child(SharedString::from(if due > 99 {
                                    "99+".to_owned()
                                } else {
                                    due.to_string()
                                })),
                        )
                    }),
            )
    }
}

#[cfg(test)]
impl Shell {
    pub(super) fn begin_edit_task_for_test(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_edit_task(id, window, cx);
    }

    pub(super) fn toggle_task_for_test(&mut self, id: &str, cx: &mut Context<Self>) {
        self.toggle_task(id, cx);
    }
}

/// The mark on a bubble whose message a task to do was made from.
pub(super) fn tasked_mark(colour: gpui_kit::Hsla) -> Div {
    div()
        .debug_selector(|| "tasked-mark".into())
        .flex_none()
        .child(icon(IconName::SquareCheck, px(11.), colour))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Weekday;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn the_usual_days_are_one_click_from_today() {
        // Thursday 15 October 2026.
        let today = day(2026, 10, 15);
        assert_eq!(today.weekday(), Weekday::Thu);
        assert_eq!(QuickDay::None.day(today), None);
        assert_eq!(QuickDay::Today.day(today), Some(today));
        assert_eq!(QuickDay::Tomorrow.day(today), Some(day(2026, 10, 16)));
        assert_eq!(QuickDay::NextWeek.day(today), Some(day(2026, 10, 19)));
        // From a Monday, next week is a week on.
        assert_eq!(
            QuickDay::NextWeek.day(day(2026, 10, 19)),
            Some(day(2026, 10, 26))
        );
    }

    #[test]
    fn a_due_day_reads_in_words_and_says_how_late() {
        let today = day(2026, 10, 15);
        let at = |d: NaiveDate| start_of(d);
        assert_eq!(due_words(at(today), today), "Today");
        assert_eq!(due_words(at(day(2026, 10, 16)), today), "Tomorrow");
        assert_eq!(due_words(at(day(2026, 10, 14)), today), "Yesterday");
        assert_eq!(due_words(at(day(2026, 10, 12)), today), "3 days late");
        assert_eq!(due_words(at(day(2026, 10, 18)), today), "Sunday");
        assert_eq!(due_words(at(day(2026, 11, 2)), today), "2 Nov");
        assert_eq!(due_words(at(day(2027, 1, 9)), today), "9 Jan 2027");
        assert_eq!(when(Some(at(day(2026, 10, 1))), today), When::Late);
        assert_eq!(when(None, today), When::Undated);
    }
}
