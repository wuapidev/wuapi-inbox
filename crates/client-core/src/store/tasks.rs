//! Tasks (schema v15): things to do, kept on this computer.
//!
//! A task is written by the user, on its own or from a message. One made
//! from a message keeps where it came from (the number, the chat, the
//! message) and the message in one line as it was then, so it still says
//! what it was about when the message is gone. Nothing of it is sent to
//! WhatsApp, which has no tasks: it lives in the encrypted store with the
//! rest, and leaves with the number it came from.

use super::{Store, StoreChange, StoreResult};
use client_provider::{AccountId, ChatId, MessageId, Timestamp};
use rusqlite::{params, OptionalExtension, Row};
use std::collections::HashSet;

/// The message a task was made from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskSource {
    /// The number it came to.
    pub account_id: AccountId,
    /// Its chat.
    pub chat_id: ChatId,
    /// The message.
    pub message_id: MessageId,
    /// The message in one line, as it was when the task was made.
    pub quote: String,
}

/// A task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// Its id, made here.
    pub id: String,
    /// What is to be done.
    pub title: String,
    /// More about it.
    pub notes: Option<String>,
    /// The day it is due, as the start of that day.
    pub due_at: Option<Timestamp>,
    /// When it was done; `None` while it is not.
    pub done_at: Option<Timestamp>,
    /// When it was made.
    pub created_at: Timestamp,
    /// The message it came from.
    pub source: Option<TaskSource>,
}

/// What a new task, or a task as edited, says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskDraft {
    /// What is to be done. Not empty.
    pub title: String,
    /// More about it; empty is none.
    pub notes: Option<String>,
    /// The day it is due.
    pub due_at: Option<Timestamp>,
}

const TASK_COLUMNS: &str =
    "id, title, notes, due_at, done_at, created_at, account_id, chat_id, message_id, quote";

fn task_from(row: &Row<'_>) -> rusqlite::Result<Task> {
    let account: Option<String> = row.get(6)?;
    let chat: Option<String> = row.get(7)?;
    let message: Option<String> = row.get(8)?;
    let source = match (account, chat, message) {
        (Some(account), Some(chat), Some(message)) => Some(TaskSource {
            account_id: AccountId::new(account),
            chat_id: ChatId::new(chat),
            message_id: MessageId::new(message),
            quote: row.get::<_, Option<String>>(9)?.unwrap_or_default(),
        }),
        _ => None,
    };
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        notes: row.get(2)?,
        due_at: row.get::<_, Option<i64>>(3)?.map(Timestamp::from_millis),
        done_at: row.get::<_, Option<i64>>(4)?.map(Timestamp::from_millis),
        created_at: Timestamp::from_millis(row.get(5)?),
        source,
    })
}

/// The title as kept: trimmed, never empty.
fn clean(draft: &TaskDraft) -> Option<(String, Option<String>)> {
    let title = draft.title.trim();
    if title.is_empty() {
        return None;
    }
    let notes = draft
        .notes
        .as_deref()
        .map(str::trim)
        .filter(|notes| !notes.is_empty())
        .map(str::to_owned);
    Some((title.to_owned(), notes))
}

impl Store {
    /// Keeps a new task and answers it. `None` when its title is empty.
    pub fn add_task(
        &self,
        draft: &TaskDraft,
        source: Option<&TaskSource>,
        now: Timestamp,
    ) -> StoreResult<Option<Task>> {
        let Some((title, notes)) = clean(draft) else {
            return Ok(None);
        };
        let id = uuid::Uuid::new_v4().simple().to_string();
        self.write(|tx| {
            tx.execute(
                "INSERT INTO tasks (id, title, notes, due_at, done_at, created_at, updated_at,
                                    account_id, chat_id, message_id, quote)
                 VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?5, ?6, ?7, ?8, ?9)",
                params![
                    id,
                    title,
                    notes,
                    draft.due_at.map(|due| due.as_millis()),
                    now.as_millis(),
                    source.map(|source| source.account_id.as_str()),
                    source.map(|source| source.chat_id.as_str()),
                    source.map(|source| source.message_id.as_str()),
                    source.map(|source| source.quote.as_str()),
                ],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Tasks);
        self.task(&id)
    }

    /// One task, by id.
    pub fn task(&self, id: &str) -> StoreResult<Option<Task>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1"),
                    params![id],
                    task_from,
                )
                .optional()?)
        })
    }

    /// Every task: the ones to do first, the earliest due first and
    /// those with no day after them, then the ones done, the last done
    /// first.
    pub fn tasks(&self) -> StoreResult<Vec<Task>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {TASK_COLUMNS} FROM tasks
                 ORDER BY done_at IS NOT NULL,
                          CASE WHEN done_at IS NULL THEN due_at IS NULL END,
                          CASE WHEN done_at IS NULL THEN due_at END,
                          CASE WHEN done_at IS NULL THEN created_at END,
                          done_at DESC"
            ))?;
            let tasks = stmt.query_map([], task_from)?.collect::<Result<_, _>>()?;
            Ok(tasks)
        })
    }

    /// The messages of a chat that a task not done yet was made from.
    pub fn tasked_messages(
        &self,
        account: &AccountId,
        chat: &ChatId,
    ) -> StoreResult<HashSet<MessageId>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT message_id FROM tasks
                 WHERE account_id = ?1 AND chat_id = ?2 AND done_at IS NULL",
            )?;
            let ids = stmt
                .query_map(params![account.as_str(), chat.as_str()], |row| {
                    row.get::<_, String>(0)
                })?
                .map(|id| id.map(MessageId::new))
                .collect::<Result<_, _>>()?;
            Ok(ids)
        })
    }

    /// Changes what a task says. Answers whether it was there and the
    /// title was not empty.
    pub fn edit_task(&self, id: &str, draft: &TaskDraft, now: Timestamp) -> StoreResult<bool> {
        let Some((title, notes)) = clean(draft) else {
            return Ok(false);
        };
        let changed = self.write(|tx| {
            Ok(tx.execute(
                "UPDATE tasks SET title = ?2, notes = ?3, due_at = ?4, updated_at = ?5
                 WHERE id = ?1",
                params![
                    id,
                    title,
                    notes,
                    draft.due_at.map(|due| due.as_millis()),
                    now.as_millis()
                ],
            )?)
        })?;
        if changed > 0 {
            self.notify(StoreChange::Tasks);
        }
        Ok(changed > 0)
    }

    /// Marks a task done, or not done again.
    pub fn set_task_done(&self, id: &str, done: bool, now: Timestamp) -> StoreResult<()> {
        let changed = self.write(|tx| {
            Ok(tx.execute(
                "UPDATE tasks SET done_at = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, done.then(|| now.as_millis()), now.as_millis()],
            )?)
        })?;
        if changed > 0 {
            self.notify(StoreChange::Tasks);
        }
        Ok(())
    }

    /// Deletes a task.
    pub fn delete_task(&self, id: &str) -> StoreResult<()> {
        let changed = self.write(|tx| Ok(tx.execute("DELETE FROM tasks WHERE id = ?1", [id])?))?;
        if changed > 0 {
            self.notify(StoreChange::Tasks);
        }
        Ok(())
    }
}
