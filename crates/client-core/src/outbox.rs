//! The outbox: messages waiting to be handed to a provider.
//!
//! Sending is the one place where a flaky network could hurt the user, by
//! losing a message or by sending it twice. The outbox rules out both:
//!
//! * A message is written to the `outbox` table, together with the pending
//!   bubble the user sees, in one transaction *before* any network call. A
//!   crash or a restart cannot lose it.
//! * Every attempt submits the identical [`OutgoingMessage`], client id
//!   included. Providers must treat that id as an idempotency key, so an
//!   attempt whose answer was lost can simply be repeated.
//! * Attempts are bounded by a timeout. A timeout, a dropped connection, a
//!   5xx or a rate limit is *transient*: the message stays pending (clock
//!   icon) and is retried with backoff. The user is only told about
//!   *terminal* outcomes: the provider rejected the message, or it could
//!   not be sent within [`OutboxConfig::max_age`].
//! * Messages of one chat go out in the order they were written. A message
//!   that is waiting to be retried holds back the ones behind it.
//! * A message with a file takes two steps, both of them repeatable. The
//!   bytes are copied into the store when it is queued (`outbox_files`),
//!   so nothing depends on the original file still being there. First
//!   the file is uploaded ([`upload_entry`]), under a key that is the same
//!   for every attempt; then the message is sent with the reference the
//!   upload answered. A failure that may pass, in either step, keeps the
//!   message pending. If the provider has let the upload go by the time
//!   the message is sent (`upload_expired`), the file is uploaded again:
//!   the copy is still here.

use crate::store::{
    status_from_parts, status_to_parts, upsert_message_tx, Store, StoreError, StoreResult,
};
use client_provider::{
    AccountId, ChatId, ClientMessageId, ContactId, DeliveryStatus, Direction, MediaRef,
    MediaUpload, Message, MessageContent, MessageId, OutgoingContent, OutgoingMessage, Provider,
    ProviderError, SendReceipt, Timestamp, UploadProgress,
};
use rusqlite::{params, OptionalExtension};
use std::collections::HashSet;
use std::time::Duration;

/// Tuning of the outbox.
#[derive(Clone, Debug)]
pub struct OutboxConfig {
    /// Longest a single send attempt may take before it is abandoned and
    /// retried.
    pub send_timeout: Duration,
    /// Delay before the first retry; doubles on each further failure.
    pub retry_base: Duration,
    /// Upper bound of the retry delay.
    pub retry_max: Duration,
    /// How long a message may wait in the outbox. Past this it is marked
    /// failed rather than being delivered surprisingly late.
    ///
    /// Must stay below [`IDEMPOTENCY_WINDOW`](client_provider::IDEMPOTENCY_WINDOW):
    /// a provider only promises to recognise a client id for that long, so
    /// retrying an older message could send it twice.
    pub max_age: Duration,
    /// Longest a single upload attempt may take.
    pub upload_timeout: Duration,
}

impl Default for OutboxConfig {
    fn default() -> Self {
        Self {
            upload_timeout: Duration::from_secs(10 * 60),
            send_timeout: Duration::from_secs(30),
            retry_base: Duration::from_secs(2),
            retry_max: Duration::from_secs(60),
            max_age: client_provider::IDEMPOTENCY_WINDOW / 2,
        }
    }
}

impl OutboxConfig {
    /// The delay before attempt number `attempts + 1`, after `attempts`
    /// failures: exponential, capped at [`retry_max`](Self::retry_max).
    pub fn backoff(&self, attempts: u32) -> Duration {
        let factor = 1u32 << attempts.saturating_sub(1).min(16);
        self.retry_base.saturating_mul(factor).min(self.retry_max)
    }
}

/// A message waiting in the outbox.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxEntry {
    /// The message, exactly as it will be (re)submitted.
    pub message: OutgoingMessage,
    /// Failed attempts so far.
    pub attempts: u32,
    /// Earliest time of the next attempt.
    pub next_attempt_at: Timestamp,
    /// When the user sent it.
    pub created_at: Timestamp,
    /// When it stops being retried.
    pub expires_at: Timestamp,
    /// The error of the last failed attempt.
    pub last_error: Option<String>,
    /// For a message with a file: the provider's reference to it, once
    /// it has been uploaded.
    pub uploaded: Option<MediaRef>,
    /// How many times the file had to be uploaded again.
    pub upload_round: u32,
    /// The story this message answers (a reply to it, or a reaction):
    /// said when it was queued, so it holds however long the message
    /// waits and whatever becomes of the story.
    pub story: Option<MessageId>,
}

impl OutboxEntry {
    /// Whether this message has a file that is not uploaded yet.
    pub fn needs_upload(&self) -> bool {
        matches!(self.message.content, OutgoingContent::Media { .. }) && self.uploaded.is_none()
    }

    /// The key every attempt at uploading this message's file carries.
    pub fn upload_key(&self) -> String {
        format!("{}:{}", self.message.client_id, self.upload_round)
    }
}

/// A file kept for a queued message.
#[derive(Clone, PartialEq, Eq)]
pub struct OutboxFile {
    /// The bytes.
    pub bytes: std::sync::Arc<Vec<u8>>,
    /// Its MIME type.
    pub mime: String,
    /// Its name, when it has one.
    pub file_name: Option<String>,
}

/// The reference a queued message's file goes by until it is uploaded:
/// the client's own copy.
pub fn local_media_ref(client_id: &ClientMessageId) -> MediaRef {
    MediaRef::new(format!("{LOCAL_MEDIA}{client_id}"))
}

/// Prefix of [`local_media_ref`].
pub const LOCAL_MEDIA: &str = "local:";

/// What one attempt at uploading a file did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UploadOutcome {
    /// The provider has the file.
    Uploaded,
    /// It failed for now; the message stays pending and is tried again.
    Retry,
    /// It failed for good; the message is marked failed.
    Failed,
    /// The provider refused the credentials.
    Unauthorized(String),
    /// The message is no longer queued (it was cancelled meanwhile).
    Gone,
}

/// What one pass over the outbox did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutboxPass {
    /// Messages the provider accepted.
    pub sent: usize,
    /// Messages that hit a transient error and will be retried.
    pub retried: usize,
    /// Messages that failed for good.
    pub failed: usize,
    /// The provider refused the credentials, with its reason. The pass
    /// stopped there and the message stays queued: it is not the message
    /// that is wrong, and it goes out after the next sign-in.
    pub unauthorized: Option<String>,
}

/// The id a message has locally until the provider assigns one.
pub fn local_message_id(client_id: &ClientMessageId) -> MessageId {
    MessageId::new(format!("{LOCAL_ID}{client_id}"))
}

/// Prefix of [`local_message_id`].
pub(crate) const LOCAL_ID: &str = "local:";

/// Where a queued message's payload says which story it answers. The
/// payload is the [`OutgoingMessage`] as JSON; this key rides beside its
/// fields, and reading the message back ignores it.
const STORY_ANSWER: &str = "story_answer";

/// What the queue keeps of a message: the message, and the story it
/// answers when it answers one.
fn payload_of(message: &OutgoingMessage, story: Option<&MessageId>) -> StoreResult<String> {
    let mut payload = serde_json::to_value(message)?;
    if let (Some(story), Some(fields)) = (story, payload.as_object_mut()) {
        fields.insert(STORY_ANSWER.to_owned(), story.as_str().into());
    }
    Ok(serde_json::to_string(&payload)?)
}

/// The story a queued payload answers, if it answers one.
fn story_of(payload: &str) -> Option<MessageId> {
    serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get(STORY_ANSWER)?
        .as_str()
        .map(MessageId::new)
}

/// The message is with the provider: its queued row goes, and the file it
/// carried moves to the media cache, where its bubble still finds it
/// under the same reference (and where it may be dropped when the cache
/// is full, like any other file). Returns whether it was still queued.
fn finish_tx(tx: &rusqlite::Transaction<'_>, client_id: &str, now: Timestamp) -> StoreResult<bool> {
    let queued: bool = tx.query_row(
        "SELECT count(*) > 0 FROM outbox WHERE client_id = ?1 AND state <> 'failed'",
        params![client_id],
        |row| row.get(0),
    )?;
    tx.execute(
        "DELETE FROM outbox WHERE client_id = ?1",
        params![client_id],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO media_cache (key, bytes, mime, width, height, size, last_used)
         SELECT ?2, bytes, mime, NULL, NULL, size, ?3 FROM outbox_files
         WHERE client_id = ?1",
        params![
            client_id,
            format!("file:{LOCAL_MEDIA}{client_id}"),
            now.as_millis(),
        ],
    )?;
    tx.execute(
        "DELETE FROM outbox_files WHERE client_id = ?1",
        params![client_id],
    )?;
    Ok(queued)
}

/// The provider's own copy of a queued message has arrived (through an
/// event or a page of history) before any request for it was answered:
/// the request reached the provider and its answer was lost or is still
/// on its way. Nothing is left to send, so the message leaves the queue
/// now instead of waiting for a retry to be told the same.
pub(crate) fn settle_tx(
    tx: &rusqlite::Transaction<'_>,
    client_id: &str,
    now: Timestamp,
) -> StoreResult<()> {
    if finish_tx(tx, client_id, now)? {
        tracing::debug!(
            client_id,
            "the provider's copy arrived before its answer; nothing left to send"
        );
        crate::store::sends::accepted_tx(tx, &ClientMessageId::new(client_id), now, None)?;
    }
    Ok(())
}

impl Store {
    /// Queues a message and creates its pending bubble, atomically.
    ///
    /// Queueing the same client id twice is a no-op, so a caller that is
    /// unsure whether its first call went through can repeat it.
    pub fn enqueue(
        &self,
        message: &OutgoingMessage,
        now: Timestamp,
        max_age: Duration,
    ) -> StoreResult<()> {
        self.enqueue_marked(message, None, None, None, now, max_age)
    }

    /// Queues an answer to a story: a reply that quotes it, or a reaction
    /// to it. It is queued as what it is, in the same transaction, so it
    /// is never mistaken for an ordinary message later: not after a long
    /// wait, and not when the story is gone by the time it can be sent.
    pub fn enqueue_story_answer(
        &self,
        message: &OutgoingMessage,
        story: &MessageId,
        now: Timestamp,
        max_age: Duration,
    ) -> StoreResult<()> {
        self.enqueue_marked(message, None, None, Some(story), now, max_age)
    }

    /// Queues a message together with the file it carries: the bytes are
    /// copied into the store in the same transaction, so the send does
    /// not depend on the original file any more. `pixels` is the size of
    /// an image, for its bubble.
    pub fn enqueue_with_file(
        &self,
        message: &OutgoingMessage,
        file: Option<&OutboxFile>,
        pixels: Option<(u32, u32)>,
        now: Timestamp,
        max_age: Duration,
    ) -> StoreResult<()> {
        self.enqueue_marked(message, file, pixels, None, now, max_age)
    }

    fn enqueue_marked(
        &self,
        message: &OutgoingMessage,
        file: Option<&OutboxFile>,
        pixels: Option<(u32, u32)>,
        story: Option<&MessageId>,
        now: Timestamp,
        max_age: Duration,
    ) -> StoreResult<()> {
        let inserted = self.write(|tx| {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO outbox
                    (client_id, account_id, chat_id, payload, state, attempts,
                     next_attempt_at, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, 'queued', 0, ?7, ?5, ?6)",
                params![
                    message.client_id.as_str(),
                    message.account_id.as_str(),
                    message.chat_id.as_str(),
                    payload_of(message, story)?,
                    now.as_millis(),
                    now.as_millis() + max_age.as_millis() as i64,
                    due_at(now),
                ],
            )?;
            if inserted == 0 {
                return Ok(false);
            }
            crate::store::sends::queued_tx(tx, message, now)?;
            if let Some(file) = file {
                tx.execute(
                    "INSERT OR REPLACE INTO outbox_files (client_id, bytes, mime, file_name, size)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        message.client_id.as_str(),
                        file.bytes.as_slice(),
                        file.mime,
                        file.file_name,
                        file.bytes.len() as i64,
                    ],
                )?;
            }

            let self_contact: Option<String> = tx
                .query_row(
                    "SELECT self_contact FROM accounts WHERE id = ?1",
                    params![message.account_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let content = match &message.content {
                OutgoingContent::Text { body } => MessageContent::text(body.clone()),
                OutgoingContent::Reaction { target, emoji } => MessageContent::Reaction {
                    target: target.clone(),
                    emoji: emoji.clone(),
                },
                OutgoingContent::Media {
                    kind,
                    media: source,
                    mime_type,
                    caption,
                    file_name,
                    gif,
                } => {
                    // The bubble shows the copy kept here, at once.
                    let mut media = client_provider::Media::new(*kind);
                    media.source = Some(source.clone());
                    media.mime_type = mime_type.clone();
                    media.caption = caption.clone();
                    media.file_name = file_name.clone();
                    media.gif = *gif;
                    media.size_bytes = file.map(|file| file.bytes.len() as u64);
                    (media.width, media.height) = match pixels {
                        Some((width, height)) => (Some(width), Some(height)),
                        None => (None, None),
                    };
                    MessageContent::Media(media)
                }
                OutgoingContent::Poll {
                    question,
                    options,
                    max_choices,
                } => MessageContent::Poll(client_provider::Poll {
                    question: question.clone(),
                    options: options
                        .iter()
                        .map(|name| client_provider::PollOption {
                            name: name.clone(),
                            votes: 0,
                        })
                        .collect(),
                    max_choices: *max_choices,
                    voters: 0,
                    chosen: Some(Vec::new()),
                }),
                // The bubble of a forward is the message it passes on
                // ([`Store::enqueue_forward`]), not something this can say.
                OutgoingContent::Forward { .. } => MessageContent::Unsupported {
                    description: "forwarded message".to_owned(),
                },
            };
            let pending = Message {
                id: local_message_id(&message.client_id),
                client_id: Some(message.client_id.clone()),
                account_id: message.account_id.clone(),
                chat_id: message.chat_id.clone(),
                sender: ContactId::new(self_contact.unwrap_or_else(|| "me".to_owned())),
                sender_name: None,
                direction: Direction::Outgoing,
                timestamp: now,
                content,
                reply_to: message
                    .reply_to
                    .clone()
                    .map(|id| client_provider::ReplyRef {
                        message_id: id,
                        sender_name: None,
                        preview: None,
                    }),
                status: DeliveryStatus::Pending,
                edited: false,
                deleted: false,
                extras: client_provider::MessageExtras {
                    mentions: message.mentions.clone(),
                    forwarded: message.forwarded,
                    ..Default::default()
                },
            };
            upsert_message_tx(tx, &pending)?;
            Ok(true)
        })?;
        if inserted {
            self.notify_messages(vec![(message.account_id.clone(), message.chat_id.clone())]);
        }
        Ok(())
    }

    /// Queues the passing on of `source` to a chat
    /// ([`OutgoingContent::Forward`]) and creates its pending bubble:
    /// what `source` carries, marked as forwarded, from this account,
    /// atomically. A media bubble keeps the reference of the file it
    /// passes on, so it shows the picture the original does and no file
    /// is copied or sent from here. Queueing the same client id twice is
    /// a no-op.
    pub fn enqueue_forward(
        &self,
        message: &OutgoingMessage,
        source: &Message,
        now: Timestamp,
        max_age: Duration,
    ) -> StoreResult<()> {
        let inserted = self.write(|tx| {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO outbox
                    (client_id, account_id, chat_id, payload, state, attempts,
                     next_attempt_at, created_at, expires_at)
                 VALUES (?1, ?2, ?3, ?4, 'queued', 0, ?7, ?5, ?6)",
                params![
                    message.client_id.as_str(),
                    message.account_id.as_str(),
                    message.chat_id.as_str(),
                    serde_json::to_string(message)?,
                    now.as_millis(),
                    now.as_millis() + max_age.as_millis() as i64,
                    due_at(now),
                ],
            )?;
            if inserted == 0 {
                return Ok(false);
            }
            crate::store::sends::queued_tx(tx, message, now)?;
            let self_contact: Option<String> = tx
                .query_row(
                    "SELECT self_contact FROM accounts WHERE id = ?1",
                    params![message.account_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let mut content = source.content.clone();
            // A poll passed on starts again: nobody has voted in the copy.
            if let MessageContent::Poll(poll) = &mut content {
                for option in &mut poll.options {
                    option.votes = 0;
                }
                poll.voters = 0;
                poll.chosen = Some(Vec::new());
            }
            let pending = Message {
                id: local_message_id(&message.client_id),
                client_id: Some(message.client_id.clone()),
                account_id: message.account_id.clone(),
                chat_id: message.chat_id.clone(),
                sender: ContactId::new(self_contact.unwrap_or_else(|| "me".to_owned())),
                sender_name: None,
                direction: Direction::Outgoing,
                timestamp: now,
                content,
                reply_to: None,
                status: DeliveryStatus::Pending,
                edited: false,
                deleted: false,
                extras: client_provider::MessageExtras {
                    forwarded: true,
                    link: source.extras.link.clone(),
                    ..Default::default()
                },
            };
            upsert_message_tx(tx, &pending)?;
            Ok(true)
        })?;
        if inserted {
            self.notify_messages(vec![(message.account_id.clone(), message.chat_id.clone())]);
        }
        Ok(())
    }

    /// Every message still waiting, oldest first.
    pub fn outbox_pending(&self) -> StoreResult<Vec<OutboxEntry>> {
        self.write(|tx| {
            let mut stmt = tx.prepare_cached(
                "SELECT payload, attempts, next_attempt_at, created_at, expires_at, last_error,
                        uploaded, upload_round
                 FROM outbox WHERE state = 'queued' ORDER BY created_at, client_id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, u32>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, u32>(7)?,
                ))
            })?;
            let mut entries = Vec::new();
            for row in rows {
                let (payload, attempts, next, created, expires, last_error, uploaded, round) = row?;
                entries.push(OutboxEntry {
                    story: story_of(&payload),
                    message: serde_json::from_str(&payload)?,
                    attempts,
                    next_attempt_at: Timestamp::from_millis(next),
                    created_at: Timestamp::from_millis(created),
                    expires_at: Timestamp::from_millis(expires),
                    last_error,
                    uploaded: uploaded.map(MediaRef::new),
                    upload_round: round,
                });
            }
            Ok(entries)
        })
    }

    /// When the outbox next has something to do, if it has anything queued.
    pub fn outbox_next_wakeup(&self) -> StoreResult<Option<Timestamp>> {
        self.write(|tx| {
            let next: Option<i64> = tx.query_row(
                "SELECT MIN(MIN(next_attempt_at, expires_at)) FROM outbox WHERE state = 'queued'",
                [],
                |row| row.get(0),
            )?;
            Ok(next.map(Timestamp::from_millis))
        })
    }

    /// Puts back in the queue whatever was mid-flight when the application
    /// last stopped. Call once at startup, before the outbox runs. Safe
    /// because resubmitting a client id never sends twice.
    pub fn outbox_recover(&self) -> StoreResult<usize> {
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE outbox SET state = 'queued' WHERE state IN ('sending', 'uploading')",
                [],
            )?)
        })
    }

    /// Makes an account's waiting messages due immediately. Called when the
    /// account reconnects, so they go out without sitting through the rest
    /// of a backoff.
    pub fn outbox_flush_account(&self, account: &AccountId, now: Timestamp) -> StoreResult<usize> {
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE outbox SET next_attempt_at = ?2
                 WHERE account_id = ?1 AND state = 'queued' AND next_attempt_at > ?2",
                params![account.as_str(), now.as_millis()],
            )?)
        })
    }

    /// The chat a queued message is for.
    pub fn outbox_chat(
        &self,
        client_id: &ClientMessageId,
    ) -> StoreResult<Option<(AccountId, ChatId)>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT account_id, chat_id FROM outbox WHERE client_id = ?1",
                    params![client_id.as_str()],
                    |row| {
                        Ok((
                            AccountId::new(row.get::<_, String>(0)?),
                            ChatId::new(row.get::<_, String>(1)?),
                        ))
                    },
                )
                .optional()?)
        })
    }

    /// The file kept for a queued message.
    pub fn outbox_file(&self, client_id: &ClientMessageId) -> StoreResult<Option<OutboxFile>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT bytes, mime, file_name FROM outbox_files WHERE client_id = ?1",
                    params![client_id.as_str()],
                    |row| {
                        Ok(OutboxFile {
                            bytes: std::sync::Arc::new(row.get(0)?),
                            mime: row.get(1)?,
                            file_name: row.get(2)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    /// The upload step is done: the message can be sent with `reference`.
    /// Returns false when the message is no longer queued.
    fn outbox_set_uploaded(
        &self,
        client_id: &ClientMessageId,
        reference: &MediaRef,
    ) -> StoreResult<bool> {
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE outbox SET uploaded = ?2, state = 'queued', attempts = 0,
                        last_error = NULL
                 WHERE client_id = ?1",
                params![client_id.as_str(), reference.as_str()],
            )? > 0)
        })
    }

    /// The provider no longer has the uploaded file: back to the upload
    /// step, under a new key, at once.
    fn outbox_upload_again(&self, client_id: &ClientMessageId, now: Timestamp) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE outbox SET uploaded = NULL, upload_round = upload_round + 1,
                        state = 'queued', attempts = attempts + 1, next_attempt_at = ?2
                 WHERE client_id = ?1",
                params![client_id.as_str(), now.as_millis()],
            )?;
            Ok(())
        })
    }

    /// Takes a message out of the queue because the user said so: the
    /// queued row, its file and its bubble go. Only what has not been
    /// handed to the provider as a message can be taken back: returns
    /// false for a message that is being sent, or was.
    pub fn outbox_cancel(&self, client_id: &ClientMessageId) -> StoreResult<bool> {
        let cancelled = self.write(|tx| {
            let found: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT account_id, chat_id, state FROM outbox WHERE client_id = ?1",
                    params![client_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((account, chat, state)) = found else {
                return Ok(None);
            };
            if state == "sending" {
                return Ok(None);
            }
            tx.execute(
                "DELETE FROM outbox WHERE client_id = ?1",
                params![client_id.as_str()],
            )?;
            tx.execute(
                "DELETE FROM outbox_files WHERE client_id = ?1",
                params![client_id.as_str()],
            )?;
            tx.execute(
                "DELETE FROM messages WHERE account_id = ?1 AND client_id = ?2",
                params![account, client_id.as_str()],
            )?;
            Ok(Some((AccountId::new(account), ChatId::new(chat))))
        })?;
        match cancelled {
            Some(chat) => {
                self.notify_messages(vec![chat]);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Gives a queued text message another text: what will be sent, and
    /// what its bubble shows. Returns false when it is too late (the
    /// message is being sent or was), or when it is not a text.
    pub fn outbox_edit_text(&self, client_id: &ClientMessageId, text: &str) -> StoreResult<bool> {
        let edited = self.write(|tx| {
            let found: Option<(String, String)> = tx
                .query_row(
                    "SELECT payload, state FROM outbox WHERE client_id = ?1",
                    params![client_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((payload, state)) = found else {
                return Ok(None);
            };
            if state == "sending" {
                return Ok(None);
            }
            let mut message: OutgoingMessage = serde_json::from_str(&payload)?;
            if !matches!(message.content, OutgoingContent::Text { .. }) {
                return Ok(None);
            }
            // Another text, to the same story if it answers one.
            let story = story_of(&payload);
            message.content = OutgoingContent::Text {
                body: text.to_owned(),
            };
            tx.execute(
                "UPDATE outbox SET payload = ?2 WHERE client_id = ?1",
                params![client_id.as_str(), payload_of(&message, story.as_ref())?],
            )?;
            tx.execute(
                "UPDATE messages SET body = ?3, content = ?4
                 WHERE account_id = ?1 AND client_id = ?2",
                params![
                    message.account_id.as_str(),
                    client_id.as_str(),
                    text,
                    serde_json::to_string(&MessageContent::text(text.to_owned()))?,
                ],
            )?;
            Ok(Some((message.account_id, message.chat_id)))
        })?;
        match edited {
            Some(chat) => {
                self.notify_messages(vec![chat]);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Returns false when the message is no longer queued (cancelled, or
    /// the provider's copy of it arrived meanwhile).
    fn outbox_set_state(&self, client_id: &ClientMessageId, state: &str) -> StoreResult<bool> {
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE outbox SET state = ?2 WHERE client_id = ?1",
                params![client_id.as_str(), state],
            )? > 0)
        })
    }

    /// The provider accepted the message: drop it from the outbox and give
    /// the bubble its real id and status.
    fn outbox_complete(
        &self,
        entry: &OutgoingMessage,
        receipt: &SendReceipt,
        took: Duration,
    ) -> StoreResult<()> {
        let now = Timestamp::now();
        self.write(|tx| {
            // The file is no longer needed for sending; its bubble still
            // shows it, from the media cache.
            finish_tx(tx, entry.client_id.as_str(), now)?;
            crate::store::sends::accepted_tx(tx, &entry.client_id, now, Some(took))?;
            // The provider's copy may already be here through an event that
            // overtook the receipt; if so the pending copy is redundant.
            let provider_copy: Option<i64> = tx
                .query_row(
                    "SELECT pk FROM messages
                     WHERE account_id = ?1 AND id = ?2 AND client_id IS NOT ?3",
                    params![
                        entry.account_id.as_str(),
                        receipt.message_id.as_str(),
                        entry.client_id.as_str()
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(pk) = provider_copy {
                tx.execute(
                    "DELETE FROM messages WHERE account_id = ?1 AND client_id = ?2",
                    params![entry.account_id.as_str(), entry.client_id.as_str()],
                )?;
                tx.execute(
                    "UPDATE messages SET client_id = ?2 WHERE pk = ?1",
                    params![pk, entry.client_id.as_str()],
                )?;
                let (status, reason): (String, Option<String>) = tx.query_row(
                    "SELECT status, status_reason FROM messages WHERE pk = ?1",
                    params![pk],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                crate::store::sends::status_tx(
                    tx,
                    entry.client_id.as_str(),
                    &status_from_parts(&status, reason),
                    now,
                )?;
                return Ok(());
            }

            let (current, reason): (String, Option<String>) = match tx
                .query_row(
                    "SELECT status, status_reason FROM messages
                     WHERE account_id = ?1 AND client_id = ?2",
                    params![entry.account_id.as_str(), entry.client_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
            {
                Some(found) => found,
                None => return Ok(()),
            };
            let current = status_from_parts(&current, reason);
            let status = if current.can_advance_to(&receipt.status) {
                &receipt.status
            } else {
                &current
            };
            crate::store::sends::status_tx(tx, entry.client_id.as_str(), status, now)?;
            let (status, reason) = status_to_parts(status);
            tx.execute(
                "UPDATE messages SET id = ?3, status = ?4, status_reason = ?5,
                        ts = COALESCE(?6, ts)
                 WHERE account_id = ?1 AND client_id = ?2",
                params![
                    entry.account_id.as_str(),
                    entry.client_id.as_str(),
                    receipt.message_id.as_str(),
                    status,
                    reason,
                    receipt.timestamp.map(Timestamp::as_millis),
                ],
            )?;
            Ok(())
        })?;
        self.notify_messages(vec![(entry.account_id.clone(), entry.chat_id.clone())]);
        Ok(())
    }

    /// A transient failure: back in the queue, still pending for the user.
    fn outbox_retry_later(
        &self,
        client_id: &ClientMessageId,
        error: &str,
        next_attempt_at: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE outbox SET state = 'queued', attempts = attempts + 1,
                        next_attempt_at = ?2, last_error = ?3
                 WHERE client_id = ?1",
                params![client_id.as_str(), next_attempt_at.as_millis(), error],
            )?;
            Ok(())
        })
    }

    /// A terminal failure: the bubble turns into a failed message.
    fn outbox_fail(&self, entry: &OutgoingMessage, reason: &str) -> StoreResult<()> {
        let failed = self.write(|tx| {
            let queued = tx.execute(
                "UPDATE outbox SET state = 'failed', last_error = ?2 WHERE client_id = ?1",
                params![entry.client_id.as_str(), reason],
            )?;
            if queued == 0 {
                // No longer queued: the provider's copy arrived while this
                // request was on its way, so the provider has the message
                // whatever it answered to a repeat of it.
                return Ok(false);
            }
            // Nothing will be sent: the copy of the file is let go.
            tx.execute(
                "DELETE FROM outbox_files WHERE client_id = ?1",
                params![entry.client_id.as_str()],
            )?;
            tx.execute(
                "UPDATE messages SET status = 'failed', status_reason = ?3
                 WHERE account_id = ?1 AND client_id = ?2",
                params![entry.account_id.as_str(), entry.client_id.as_str(), reason],
            )?;
            crate::store::sends::status_tx(
                tx,
                entry.client_id.as_str(),
                &DeliveryStatus::Failed {
                    reason: reason.to_owned(),
                },
                Timestamp::now(),
            )?;
            Ok(true)
        })?;
        if failed {
            self.notify_messages(vec![(entry.account_id.clone(), entry.chat_id.clone())]);
        }
        Ok(())
    }
}

/// Told how far an upload is: the message, bytes out, bytes in all.
pub type UploadReport = std::sync::Arc<dyn Fn(&ClientMessageId, u64, u64) + Send + Sync>;

/// Starts the upload of an entry somewhere else.
pub type UploadStart = std::sync::Arc<dyn Fn(OutboxEntry) + Send + Sync>;

/// What the outbox does about files while it runs.
#[derive(Clone, Default)]
pub struct UploadHooks {
    /// Told how far each upload is: the message, bytes out, bytes in all.
    pub progress: Option<UploadReport>,
    /// Starts the upload of an entry somewhere else and returns at once,
    /// so that one large file does not hold up every other chat. Without
    /// it, files are uploaded within the pass.
    pub start: Option<UploadStart>,
}

/// Uploads the file of one queued message, once, and records what came of
/// it. Safe to repeat: every attempt carries the same key.
pub async fn upload_entry(
    store: &Store,
    provider: &dyn Provider,
    config: &OutboxConfig,
    entry: &OutboxEntry,
    now: Timestamp,
    hooks: &UploadHooks,
) -> Result<UploadOutcome, StoreError> {
    let message = &entry.message;
    let OutgoingContent::Media {
        kind, mime_type, ..
    } = &message.content
    else {
        return Ok(UploadOutcome::Uploaded);
    };
    let Some(file) = store.outbox_file(&message.client_id)? else {
        store.outbox_fail(message, "The file is no longer on this computer")?;
        return Ok(UploadOutcome::Failed);
    };
    if !store.outbox_set_state(&message.client_id, "uploading")? {
        return Ok(UploadOutcome::Gone);
    }
    let total = file.bytes.len() as u64;
    let progress: UploadProgress = match &hooks.progress {
        Some(report) => {
            let (report, id) = (report.clone(), message.client_id.clone());
            report(&id, 0, total);
            std::sync::Arc::new(move |sent| report(&id, sent.min(total), total))
        }
        None => std::sync::Arc::new(|_| {}),
    };
    let upload = MediaUpload {
        key: entry.upload_key(),
        kind: *kind,
        bytes: file.bytes.clone(),
        mime_type: mime_type.clone().unwrap_or_else(|| file.mime.clone()),
        file_name: file.file_name.clone(),
    };
    let attempt = tokio::time::timeout(
        config.upload_timeout,
        provider.upload_media(&message.account_id, upload, progress),
    );
    let result = match attempt.await {
        Ok(result) => result,
        Err(_) => Err(ProviderError::Transient("the upload timed out".to_owned())),
    };
    Ok(match result {
        Ok(reference) => {
            if store.outbox_set_uploaded(&message.client_id, &reference)? {
                UploadOutcome::Uploaded
            } else {
                UploadOutcome::Gone
            }
        }
        Err(error) if error.is_transient() => {
            let delay = error
                .retry_after()
                .unwrap_or_else(|| config.backoff(entry.attempts + 1));
            let next = Timestamp::from_millis(now.as_millis() + delay.as_millis() as i64);
            tracing::debug!(client_id = %message.client_id, %error, ?delay, "upload will be retried");
            store.outbox_retry_later(&message.client_id, &error.to_string(), next)?;
            UploadOutcome::Retry
        }
        Err(ProviderError::Unauthorized(reason)) => {
            store.outbox_retry_later(&message.client_id, "not signed in", now)?;
            UploadOutcome::Unauthorized(reason)
        }
        Err(error) => {
            tracing::warn!(client_id = %message.client_id, %error, "upload failed for good");
            store.outbox_fail(message, &error.to_string())?;
            UploadOutcome::Failed
        }
    })
}

/// Hands one message to the provider: a send, or, for the passing on of
/// another message, a forward of one item.
///
/// A provider whose forwarding is missing for now answers `Unsupported`
/// (a backend without the route). That is not a failure of the text that
/// was named: a text is what it always was, sent again with the mark,
/// under the same client id, so nothing is sent twice. Anything else
/// fails as "not available yet".
async fn submit(
    store: &Store,
    provider: &dyn Provider,
    message: OutgoingMessage,
) -> Result<SendReceipt, ProviderError> {
    let OutgoingContent::Forward { source } = &message.content else {
        return provider.send(message).await;
    };
    let item = client_provider::ForwardItem {
        client_id: message.client_id.clone(),
        message: source.clone(),
        to: message.chat_id.clone(),
    };
    let answer = match provider
        .forward_messages(&message.account_id, &[item])
        .await
    {
        Ok(answers) => answers.into_iter().next().unwrap_or_else(|| {
            Err(ProviderError::Protocol(
                "the provider answered nothing for a forward".into(),
            ))
        }),
        Err(error) => Err(error),
    };
    let Err(ProviderError::Unsupported(_)) = &answer else {
        return answer;
    };
    let original = store
        .message(&message.account_id, source)
        .map_err(|error| ProviderError::Transient(error.to_string()))?;
    match original.map(|original| original.content) {
        Some(MessageContent::Text { body }) => {
            provider
                .send(OutgoingMessage {
                    content: OutgoingContent::Text { body },
                    forwarded: true,
                    ..message
                })
                .await
        }
        _ => Err(ProviderError::Rejected {
            code: "forward_unavailable".to_owned(),
            message: crate::ForwardRefusal::NotAvailableYet.reason().to_owned(),
        }),
    }
}

/// What a queued answer to a story fails with when the story is gone.
const STORY_GONE: &str = "This status is no longer there.";

/// Hands a message to the provider. An answer to a story, or a reaction
/// to one, goes by the provider's own calls for them: the story is what
/// the message quotes, and it is in the store for as long as it lives.
///
/// A message that was queued as an answer to a story (`story`) is only
/// ever that. A story lasts a day and a message may wait through a long
/// outage: when the story is gone by the time the message can go, the
/// message fails, in words. It is never handed over as an ordinary
/// message, which would quote or react to something nobody has.
async fn dispatch(
    store: &Store,
    provider: &dyn Provider,
    message: OutgoingMessage,
    story: Option<&MessageId>,
) -> client_provider::ProviderResult<SendReceipt> {
    let answered = match story {
        Some(story) => match store.story(&message.account_id, story) {
            Ok(Some(item)) => Some(item),
            Ok(None) => {
                return Err(ProviderError::Rejected {
                    code: "story_gone".to_owned(),
                    message: STORY_GONE.to_owned(),
                })
            }
            // The store could not say: asked again, not guessed.
            Err(error) => return Err(ProviderError::Transient(error.to_string())),
        },
        // Not marked: an answer all the same if what it quotes is a story.
        None => {
            let target = match &message.content {
                OutgoingContent::Reaction { target, .. } => Some(target),
                _ => message.reply_to.as_ref(),
            };
            target.and_then(|target| store.story(&message.account_id, target).ok().flatten())
        }
    };
    match answered {
        Some(item) => match message.content {
            OutgoingContent::Reaction { .. } => provider.react_to_story(&item.story, message).await,
            _ => provider.reply_to_story(&item.story, message).await,
        },
        // Anything else: a send, or a forward by naming the original.
        None => submit(store, provider, message).await,
    }
}

/// Makes one pass over the outbox: attempts every message that is due at
/// `now`, in order, and records the outcome of each.
///
/// The background worker of the sync engine calls this whenever something
/// is due. It is a plain function of `now` so it can be tested without
/// timers.
pub async fn run_outbox_pass(
    store: &Store,
    provider: &dyn Provider,
    config: &OutboxConfig,
    now: Timestamp,
) -> Result<OutboxPass, StoreError> {
    run_outbox_pass_with(store, provider, config, now, &UploadHooks::default()).await
}

/// When a message queued at `queued` is first due: at once.
///
/// The time a message is queued at is its place in the order, and the
/// engine keeps those apart by a millisecond when several are queued in
/// the same one, so the stamp of the later ones is slightly ahead of the
/// clock. That must not make them "not due yet": a pass that runs in that
/// same millisecond would pass them over.
fn due_at(queued: Timestamp) -> i64 {
    queued.as_millis().min(Timestamp::now().as_millis())
}

/// [`run_outbox_pass`], saying what to do about files.
pub async fn run_outbox_pass_with(
    store: &Store,
    provider: &dyn Provider,
    config: &OutboxConfig,
    now: Timestamp,
    hooks: &UploadHooks,
) -> Result<OutboxPass, StoreError> {
    let mut pass = OutboxPass::default();
    // Chats in which an earlier message is still waiting: later messages of
    // the same chat must not overtake it. That holds for a file that is
    // still being uploaded too: a text typed after it goes out after it.
    let mut blocked: HashSet<(AccountId, ChatId)> = HashSet::new();

    for mut entry in store.outbox_pending()? {
        let chat = (
            entry.message.account_id.clone(),
            entry.message.chat_id.clone(),
        );

        if entry.expires_at <= now {
            let reason = match &entry.last_error {
                Some(error) => format!("Could not be sent in time ({error})"),
                None => "Could not be sent in time".to_owned(),
            };
            store.outbox_fail(&entry.message, &reason)?;
            pass.failed += 1;
            continue;
        }
        if blocked.contains(&chat) {
            continue;
        }
        if entry.next_attempt_at > now {
            blocked.insert(chat);
            continue;
        }

        // First step for a message with a file: the upload.
        if entry.needs_upload() {
            if let Some(start) = &hooks.start {
                // Somewhere else, so the other chats go on. This chat
                // waits for it.
                start(entry.clone());
                blocked.insert(chat);
                continue;
            }
            match upload_entry(store, provider, config, &entry, now, hooks).await? {
                UploadOutcome::Uploaded => {
                    let again = store
                        .outbox_pending()?
                        .into_iter()
                        .find(|known| known.message.client_id == entry.message.client_id);
                    match again {
                        Some(again) => entry = again,
                        None => continue,
                    }
                }
                UploadOutcome::Retry => {
                    blocked.insert(chat);
                    pass.retried += 1;
                    continue;
                }
                UploadOutcome::Failed => {
                    pass.failed += 1;
                    continue;
                }
                UploadOutcome::Unauthorized(reason) => {
                    pass.unauthorized = Some(reason);
                    break;
                }
                UploadOutcome::Gone => continue,
            }
        }

        // What is submitted: the message as queued, with the reference
        // the upload answered in the place of the local copy's.
        let mut submitted = entry.message.clone();
        if let (OutgoingContent::Media { media, .. }, Some(uploaded)) =
            (&mut submitted.content, &entry.uploaded)
        {
            *media = uploaded.clone();
        }
        let message = &entry.message;

        if !store.outbox_set_state(&message.client_id, "sending")? {
            // Taken out of the queue since this pass began: cancelled, or
            // the provider's copy of it arrived.
            continue;
        }
        let started = Timestamp::now();
        store.send_timing_attempt(&message.client_id, started)?;
        let clock = std::time::Instant::now();
        let attempt = tokio::time::timeout(
            config.send_timeout,
            dispatch(store, provider, submitted, entry.story.as_ref()),
        );
        let result = match attempt.await {
            Ok(result) => result,
            Err(_) => Err(ProviderError::Transient("the send timed out".to_owned())),
        };
        let took = clock.elapsed();
        // How long sending takes, for whoever measures it: no text, no key.
        tracing::debug!(
            client_id = %message.client_id,
            attempt = entry.attempts + 1,
            enter_to_post_ms = started.as_millis() - entry.created_at.as_millis(),
            post_ms = took.as_millis() as u64,
            accepted = result.is_ok(),
            "send timing: the request was answered"
        );

        match result {
            Ok(receipt) => {
                store.outbox_complete(message, &receipt, took)?;
                pass.sent += 1;
            }
            Err(error) if error.is_transient() => {
                let delay = error
                    .retry_after()
                    .unwrap_or_else(|| config.backoff(entry.attempts + 1));
                let next = Timestamp::from_millis(now.as_millis() + delay.as_millis() as i64);
                tracing::debug!(client_id = %message.client_id, %error, ?delay, "send will be retried");
                store.outbox_retry_later(&message.client_id, &error.to_string(), next)?;
                blocked.insert(chat);
                pass.retried += 1;
            }
            Err(ProviderError::Unauthorized(reason)) => {
                // Not a verdict on the message: keep it, and stop asking
                // with credentials that are no longer good.
                store.outbox_retry_later(&message.client_id, "not signed in", now)?;
                pass.unauthorized = Some(reason);
                break;
            }
            // The provider let the uploaded file go before the message
            // was sent. The copy is still here: upload it again.
            Err(ProviderError::Rejected { code, .. })
                if code == "upload_expired" && entry.uploaded.is_some() =>
            {
                tracing::debug!(client_id = %message.client_id, "the upload expired; uploading again");
                store.outbox_upload_again(&message.client_id, now)?;
                blocked.insert(chat);
                pass.retried += 1;
            }
            Err(error) => {
                tracing::warn!(client_id = %message.client_id, %error, "send failed for good");
                // What a provider refuses of a forward is said the way a
                // refusal before queueing is.
                let refusal = match (&message.content, &error) {
                    (OutgoingContent::Forward { .. }, ProviderError::Rejected { code, .. }) => {
                        crate::ForwardRefusal::of_code(code)
                    }
                    _ => None,
                };
                let reason = match refusal {
                    Some(refusal) => refusal.reason().to_owned(),
                    None => error.to_string(),
                };
                store.outbox_fail(message, &reason)?;
                pass.failed += 1;
            }
        }
    }
    Ok(pass)
}
