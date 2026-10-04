//! The outbox of stories (schema v11): a story the user posted that the
//! provider has not answered yet.
//!
//! It follows the rules of the message outbox. The story is written to the
//! store first (with the bytes of its picture or video), so it shows under
//! "My status" at once and nothing depends on the original file. The post
//! carries the client id as its idempotency key, in every attempt, so a
//! request whose answer was lost is repeated without posting twice. A
//! failure that may pass keeps it queued; only a refusal, or running out of
//! time, fails it, and a failed post stays visible until the user retries
//! or discards it.

use super::stories::{StoryItem, StoryPostState};
use super::{Store, StoreChange, StoreResult};
use crate::outbox::OutboxFile;
use client_provider::{
    AccountId, ClientMessageId, ContactId, Media, MediaKind, MediaRef, MessageId, NewStory,
    NewStoryContent, Story, StoryBody, Timestamp,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::time::Duration;

/// The id a story has locally until the provider assigns one.
pub fn local_story_id(client_id: &ClientMessageId) -> MessageId {
    MessageId::new(format!("story-local:{client_id}"))
}

/// A story waiting to be posted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoryPostEntry {
    /// What to post, as it will be (re)submitted; a picture or a video
    /// carries the client's own reference until it is uploaded.
    pub post: NewStory,
    /// Failed attempts so far.
    pub attempts: u32,
    /// Earliest time of the next attempt.
    pub next_attempt_at: Timestamp,
    /// When the user posted it.
    pub created_at: Timestamp,
    /// When it stops being retried.
    pub expires_at: Timestamp,
    /// The error of the last failed attempt.
    pub last_error: Option<String>,
    /// For a picture or a video: the provider's reference to the upload.
    pub uploaded: Option<MediaRef>,
    /// How many times the file had to be uploaded again.
    pub upload_round: u32,
}

impl StoryPostEntry {
    /// Whether the story has a file that is not uploaded yet.
    pub fn needs_upload(&self) -> bool {
        matches!(self.post.content, NewStoryContent::Media { .. }) && self.uploaded.is_none()
    }

    /// The key every attempt at uploading the file carries.
    pub fn upload_key(&self) -> String {
        format!("story:{}:{}", self.post.client_id, self.upload_round)
    }
}

fn media_kind_name(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Video => "video",
        _ => "image",
    }
}

/// The posts that are not settled, as stories of the account's own.
pub(crate) fn pending_items(conn: &Connection, account: &AccountId) -> StoreResult<Vec<StoryItem>> {
    let mut stmt = conn.prepare_cached(
        "SELECT payload, state, created_at, expires_at, last_error FROM story_posts
         WHERE account_id = ?1 ORDER BY created_at, client_id",
    )?;
    let rows: Vec<(String, String, i64, i64, Option<String>)> = stmt
        .query_map(params![account.as_str()], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<Result<_, _>>()?;
    let self_contact: Option<String> = conn
        .query_row(
            "SELECT self_contact FROM accounts WHERE id = ?1",
            params![account.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let mut out = Vec::new();
    for (payload, state, created, _expires, error) in rows {
        let Ok(post) = serde_json::from_str::<NewStory>(&payload) else {
            continue;
        };
        let body = match &post.content {
            NewStoryContent::Text { text, style } => StoryBody::Text {
                text: text.clone(),
                style: *style,
            },
            NewStoryContent::Media {
                kind,
                media,
                mime_type,
                caption,
            } => {
                let mut media_body = Media::new(*kind);
                media_body.source = Some(media.clone());
                media_body.mime_type = mime_type.clone();
                media_body.caption = caption.clone();
                StoryBody::Media(media_body)
            }
        };
        let posted_at = Timestamp::from_millis(created);
        out.push(StoryItem {
            story: Story {
                id: local_story_id(&post.client_id),
                client_id: Some(post.client_id.clone()),
                account_id: account.clone(),
                author: ContactId::new(self_contact.clone().unwrap_or_else(|| "me".to_owned())),
                author_name: None,
                mine: true,
                posted_at,
                expires_at: Some(Timestamp::from_millis(
                    created + client_provider::STORY_LIFETIME.as_millis() as i64,
                )),
                body,
                mentions: Vec::new(),
                viewed: true,
                view_count: None,
            },
            viewed: true,
            viewed_at: Some(posted_at),
            post: Some(if state == "failed" {
                StoryPostState::Failed(error.unwrap_or_default())
            } else {
                StoryPostState::Queued
            }),
        });
    }
    Ok(out)
}

/// The provider has the story: its queued row goes, and the file it
/// carried moves to the media cache, where its tile still finds it under
/// the client's own key (and where it may be dropped when the cache is
/// full, like any other file).
pub(crate) fn settle_tx(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    client_id: &str,
) -> StoreResult<bool> {
    let queued = tx.execute(
        "DELETE FROM story_posts WHERE client_id = ?1 AND account_id = ?2",
        params![client_id, account],
    )?;
    tx.execute(
        "INSERT OR REPLACE INTO media_cache (key, bytes, mime, width, height, size, last_used)
         SELECT ?2, bytes, mime, NULL, NULL, size, ?3 FROM story_post_files
         WHERE client_id = ?1",
        params![
            client_id,
            format!("file:local:{client_id}"),
            Timestamp::now().as_millis()
        ],
    )?;
    tx.execute(
        "DELETE FROM story_post_files WHERE client_id = ?1",
        params![client_id],
    )?;
    Ok(queued > 0)
}

impl Store {
    /// Queues a story and shows it under "My status", atomically.
    /// Queueing the same client id twice is a no-op.
    ///
    /// A picture or a video carries `local_media_ref(client_id)` as its
    /// media and its bytes in `file`; `pixels` is the picture's size and
    /// `thumbnail` its small copy, for the tile that shows at once.
    pub fn enqueue_story_post(
        &self,
        post: &NewStory,
        file: Option<&OutboxFile>,
        now: Timestamp,
        max_age: Duration,
    ) -> StoreResult<bool> {
        let local_kind = match &post.content {
            NewStoryContent::Text { .. } => "text",
            NewStoryContent::Media { kind, .. } => media_kind_name(*kind),
        };
        let inserted = self.write(|tx| {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO story_posts
                    (client_id, account_id, payload, state, attempts, next_attempt_at,
                     created_at, expires_at, local_kind)
                 VALUES (?1, ?2, ?3, 'queued', 0, ?4, ?4, ?5, ?6)",
                params![
                    post.client_id.as_str(),
                    post.account_id.as_str(),
                    serde_json::to_string(post)?,
                    now.as_millis(),
                    now.as_millis() + max_age.as_millis() as i64,
                    local_kind,
                ],
            )? > 0;
            if inserted {
                if let Some(file) = file {
                    tx.execute(
                        "INSERT OR REPLACE INTO story_post_files
                            (client_id, bytes, mime, file_name, size)
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            post.client_id.as_str(),
                            file.bytes.as_slice(),
                            file.mime,
                            file.file_name,
                            file.bytes.len() as i64,
                        ],
                    )?;
                }
            }
            Ok(inserted)
        })?;
        if inserted {
            self.notify(StoreChange::Stories {
                account_id: post.account_id.clone(),
            });
        }
        Ok(inserted)
    }

    /// The stories waiting to be posted, oldest first.
    pub fn story_posts_pending(&self) -> StoreResult<Vec<StoryPostEntry>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT payload, attempts, next_attempt_at, created_at, expires_at, last_error,
                        uploaded, upload_round
                 FROM story_posts WHERE state = 'queued' ORDER BY created_at, client_id",
            )?;
            let rows = stmt
                .query_map([], |row| {
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
                })?
                .collect::<Result<Vec<_>, _>>()?;
            let mut entries = Vec::new();
            for (payload, attempts, next, created, expires, last_error, uploaded, round) in rows {
                entries.push(StoryPostEntry {
                    post: serde_json::from_str(&payload)?,
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

    /// The posts that failed for good, newest first.
    pub fn story_posts_failed(
        &self,
        account: &AccountId,
    ) -> StoreResult<Vec<(ClientMessageId, String)>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT client_id, COALESCE(last_error, '') FROM story_posts
                 WHERE account_id = ?1 AND state = 'failed' ORDER BY created_at DESC",
            )?;
            let rows = stmt
                .query_map(params![account.as_str()], |row| {
                    Ok((ClientMessageId::new(row.get::<_, String>(0)?), row.get(1)?))
                })?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    /// When the story outbox next has something to do.
    pub fn story_posts_next_wakeup(&self) -> StoreResult<Option<Timestamp>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT MIN(MIN(next_attempt_at, expires_at)) FROM story_posts
                     WHERE state = 'queued'",
                    [],
                    |row| row.get::<_, Option<i64>>(0),
                )?
                .map(Timestamp::from_millis))
        })
    }

    /// Puts back in the queue what was mid-flight when the application
    /// last stopped. Safe because posting a client id again never posts
    /// twice.
    pub fn story_posts_recover(&self) -> StoreResult<usize> {
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE story_posts SET state = 'queued' WHERE state IN ('uploading', 'posting')",
                [],
            )?)
        })
    }

    /// Makes an account's waiting posts due now (it reconnected).
    pub fn story_posts_flush_account(
        &self,
        account: &AccountId,
        now: Timestamp,
    ) -> StoreResult<usize> {
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE story_posts SET next_attempt_at = ?2
                 WHERE account_id = ?1 AND state = 'queued' AND next_attempt_at > ?2",
                params![account.as_str(), now.as_millis()],
            )?)
        })
    }

    /// The file kept for a queued story.
    pub fn story_post_file(&self, client_id: &ClientMessageId) -> StoreResult<Option<OutboxFile>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT bytes, mime, file_name FROM story_post_files WHERE client_id = ?1",
                    params![client_id.as_str()],
                    |row| {
                        Ok(OutboxFile {
                            bytes: std::sync::Arc::new(row.get::<_, Vec<u8>>(0)?),
                            mime: row.get(1)?,
                            file_name: row.get(2)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    pub(crate) fn story_post_set_state(
        &self,
        client_id: &ClientMessageId,
        state: &str,
    ) -> StoreResult<bool> {
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE story_posts SET state = ?2 WHERE client_id = ?1 AND state <> 'failed'",
                params![client_id.as_str(), state],
            )? > 0)
        })
    }

    pub(crate) fn story_post_set_uploaded(
        &self,
        client_id: &ClientMessageId,
        reference: &MediaRef,
    ) -> StoreResult<bool> {
        self.write(|tx| {
            Ok(tx.execute(
                "UPDATE story_posts SET uploaded = ?2, state = 'queued'
                 WHERE client_id = ?1 AND state <> 'failed'",
                params![client_id.as_str(), reference.as_str()],
            )? > 0)
        })
    }

    pub(crate) fn story_post_upload_again(
        &self,
        client_id: &ClientMessageId,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE story_posts SET uploaded = NULL, upload_round = upload_round + 1,
                        state = 'queued', next_attempt_at = ?2
                 WHERE client_id = ?1",
                params![client_id.as_str(), now.as_millis()],
            )?;
            Ok(())
        })
    }

    pub(crate) fn story_post_retry_later(
        &self,
        client_id: &ClientMessageId,
        error: &str,
        next: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE story_posts SET state = 'queued', attempts = attempts + 1,
                        next_attempt_at = ?2, last_error = ?3
                 WHERE client_id = ?1 AND state <> 'failed'",
                params![client_id.as_str(), next.as_millis(), error],
            )?;
            Ok(())
        })
    }

    pub(crate) fn story_post_fail(&self, post: &NewStory, reason: &str) -> StoreResult<()> {
        let failed = self.write(|tx| {
            let changed = tx.execute(
                "UPDATE story_posts SET state = 'failed', last_error = ?2
                 WHERE client_id = ?1",
                params![post.client_id.as_str(), reason],
            )?;
            if changed > 0 {
                // Nothing will be posted: the copy of the file is let go.
                tx.execute(
                    "DELETE FROM story_post_files WHERE client_id = ?1",
                    params![post.client_id.as_str()],
                )?;
            }
            Ok(changed > 0)
        })?;
        if failed {
            self.notify(StoreChange::Stories {
                account_id: post.account_id.clone(),
            });
        }
        Ok(())
    }

    /// Takes a failed post back in the queue, to be tried now. A picture
    /// or a video whose bytes were let go cannot be: it says so.
    pub fn story_post_retry_now(
        &self,
        client_id: &ClientMessageId,
        now: Timestamp,
        max_age: Duration,
    ) -> StoreResult<bool> {
        let account = self.write(|tx| {
            let row: Option<(String, String)> = tx
                .query_row(
                    "SELECT account_id, local_kind FROM story_posts
                     WHERE client_id = ?1 AND state = 'failed'",
                    params![client_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((account, kind)) = row else {
                return Ok(None);
            };
            let has_file: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM story_post_files WHERE client_id = ?1)",
                params![client_id.as_str()],
                |row| row.get(0),
            )?;
            if kind != "text" && !has_file {
                return Ok(None);
            }
            tx.execute(
                "UPDATE story_posts SET state = 'queued', attempts = 0, next_attempt_at = ?2,
                        created_at = ?2, expires_at = ?3, last_error = NULL
                 WHERE client_id = ?1",
                params![
                    client_id.as_str(),
                    now.as_millis(),
                    now.as_millis() + max_age.as_millis() as i64
                ],
            )?;
            Ok(Some(account))
        })?;
        if let Some(account) = &account {
            self.notify(StoreChange::Stories {
                account_id: AccountId::new(account.clone()),
            });
        }
        Ok(account.is_some())
    }

    /// The provider has the story that was posted from here: its queued
    /// row goes (a no-op when it already did).
    pub(crate) fn story_post_settle(&self, post: &NewStory) -> StoreResult<()> {
        self.write(|tx| {
            settle_tx(tx, post.account_id.as_str(), post.client_id.as_str())?;
            Ok(())
        })
    }

    /// Replaces the extras' story reference of the message queued under
    /// `client_id`: it answers that story.
    pub(crate) fn mark_story_reply(
        &self,
        account: &AccountId,
        client_id: &ClientMessageId,
        reply: &client_provider::StoryReplyRef,
    ) -> StoreResult<()> {
        let chat = self.write(|tx| {
            let row: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT chat_id, extras FROM messages WHERE account_id = ?1 AND client_id = ?2",
                    params![account.as_str(), client_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((chat, extras)) = row else {
                return Ok(None);
            };
            let mut extras: client_provider::MessageExtras = extras
                .and_then(|json| serde_json::from_str(&json).ok())
                .unwrap_or_default();
            extras.story_reply = Some(reply.clone());
            tx.execute(
                "UPDATE messages SET extras = ?3 WHERE account_id = ?1 AND client_id = ?2",
                params![
                    account.as_str(),
                    client_id.as_str(),
                    serde_json::to_string(&extras)?
                ],
            )?;
            Ok(Some(chat))
        })?;
        if let Some(chat) = chat {
            self.notify_messages(vec![(account.clone(), client_provider::ChatId::new(chat))]);
        }
        Ok(())
    }

    /// Drops a story that was never posted (the user gave up on it).
    pub fn story_post_discard(&self, client_id: &ClientMessageId) -> StoreResult<bool> {
        let account: Option<String> = self.write(|tx| {
            let account = tx
                .query_row(
                    "SELECT account_id FROM story_posts WHERE client_id = ?1",
                    params![client_id.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            tx.execute(
                "DELETE FROM story_posts WHERE client_id = ?1",
                params![client_id.as_str()],
            )?;
            tx.execute(
                "DELETE FROM story_post_files WHERE client_id = ?1",
                params![client_id.as_str()],
            )?;
            tx.execute(
                "DELETE FROM media_cache WHERE key IN (?1, ?2)",
                params![
                    format!("thumb:local:{client_id}"),
                    format!("file:local:{client_id}")
                ],
            )?;
            Ok(account)
        })?;
        if let Some(account) = &account {
            self.notify(StoreChange::Stories {
                account_id: AccountId::new(account.clone()),
            });
        }
        Ok(account.is_some())
    }
}
