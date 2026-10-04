//! How long sending takes (schema v8): one row per message sent from
//! here, with the moments that matter between Enter and the ticks.
//!
//! Nothing of the message is kept: no text, no file, nobody's name. The
//! rows are for measuring, by `--diagnose sends` and at debug level in the
//! log, and the newest [`KEPT`] of them are all there is.

use super::{connect, Store, StoreKey, StoreResult};
use client_provider::{
    AccountId, ChatId, ClientMessageId, DeliveryStatus, OutgoingContent, OutgoingMessage, Timestamp,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use std::path::Path;
use std::time::Duration;

/// How many sends are remembered.
const KEPT: usize = 200;

/// The moments of one send, in milliseconds since the epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendTiming {
    /// The message.
    pub client_id: ClientMessageId,
    /// The account it was sent from.
    pub account_id: AccountId,
    /// The chat it was sent to.
    pub chat_id: ChatId,
    /// `text`, `media`, `poll` or `reaction`.
    pub kind: String,
    /// When the user sent it.
    pub queued_at: Timestamp,
    /// When the first request for it left.
    pub post_started_at: Option<Timestamp>,
    /// How many requests were made.
    pub attempts: u32,
    /// When the provider said it has the message.
    pub accepted_at: Option<Timestamp>,
    /// How long the request that was answered took.
    pub post: Option<Duration>,
    /// When the bubble first left the clock.
    pub first_change_at: Option<Timestamp>,
    /// When WhatsApp had it.
    pub sent_at: Option<Timestamp>,
    /// When it reached the other phone.
    pub delivered_at: Option<Timestamp>,
    /// When it was read.
    pub read_at: Option<Timestamp>,
    /// When it failed for good.
    pub failed_at: Option<Timestamp>,
}

impl SendTiming {
    /// How long after the user sent it `at` was.
    pub fn since_queued(&self, at: Option<Timestamp>) -> Option<Duration> {
        let millis = at?.as_millis() - self.queued_at.as_millis();
        Some(Duration::from_millis(millis.max(0) as u64))
    }

    /// The row as one line: durations only, and ids cut to their tail.
    pub fn line(&self) -> String {
        let after = |at: Option<Timestamp>| match self.since_queued(at) {
            Some(took) => format!("{}ms", took.as_millis()),
            None => "-".to_owned(),
        };
        let tail = |id: &str| {
            let chars: Vec<char> = id.chars().collect();
            chars[chars.len().saturating_sub(4)..]
                .iter()
                .collect::<String>()
        };
        format!(
            "send ..{id} kind={kind} queuedAt={queued} toPost={post_start} post={post} \
             attempts={attempts} accepted={accepted} firstChange={first} sent={sent} \
             delivered={delivered} read={read} failed={failed}",
            id = tail(self.client_id.as_str()),
            kind = self.kind,
            queued = self.queued_at.as_millis(),
            post_start = after(self.post_started_at),
            post = self
                .post
                .map_or_else(|| "-".to_owned(), |took| format!("{}ms", took.as_millis())),
            attempts = self.attempts,
            accepted = after(self.accepted_at),
            first = after(self.first_change_at),
            sent = after(self.sent_at),
            delivered = after(self.delivered_at),
            read = after(self.read_at),
            failed = after(self.failed_at),
        )
    }
}

fn kind_of(content: &OutgoingContent) -> &'static str {
    match content {
        OutgoingContent::Text { .. } => "text",
        OutgoingContent::Media { .. } => "media",
        OutgoingContent::Poll { .. } => "poll",
        OutgoingContent::Reaction { .. } => "reaction",
        OutgoingContent::Forward { .. } => "forward",
    }
}

/// A message was queued: its row begins.
pub(crate) fn queued_tx(
    tx: &Transaction<'_>,
    message: &OutgoingMessage,
    now: Timestamp,
) -> StoreResult<()> {
    tx.execute(
        "INSERT OR IGNORE INTO send_timings (client_id, account_id, chat_id, kind, queued_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            message.client_id.as_str(),
            message.account_id.as_str(),
            message.chat_id.as_str(),
            kind_of(&message.content),
            now.as_millis(),
        ],
    )?;
    tx.execute(
        "DELETE FROM send_timings WHERE client_id NOT IN
            (SELECT client_id FROM send_timings ORDER BY queued_at DESC, client_id LIMIT ?1)",
        params![KEPT as i64],
    )?;
    Ok(())
}

/// The provider has the message, as its answer to a request (`post`: how
/// long that request took) or as a copy that arrived some other way.
pub(crate) fn accepted_tx(
    tx: &Transaction<'_>,
    client_id: &ClientMessageId,
    now: Timestamp,
    post: Option<Duration>,
) -> StoreResult<()> {
    tx.execute(
        "UPDATE send_timings SET accepted_at = COALESCE(accepted_at, ?2),
                post_ms = COALESCE(post_ms, ?3)
         WHERE client_id = ?1",
        params![
            client_id.as_str(),
            now.as_millis(),
            post.map(|took| took.as_millis() as i64),
        ],
    )?;
    Ok(())
}

/// The message's status moved to `status`: the first time each tick is
/// reached is written down, and said at debug level.
pub(crate) fn status_tx(
    tx: &Transaction<'_>,
    client_id: &str,
    status: &DeliveryStatus,
    now: Timestamp,
) -> StoreResult<()> {
    let rank = status.rank();
    let reached = |least: &DeliveryStatus| {
        (!matches!(status, DeliveryStatus::Failed { .. }) && rank >= least.rank())
            .then_some(now.as_millis())
    };
    let failed = matches!(status, DeliveryStatus::Failed { .. }).then_some(now.as_millis());
    let left_the_clock = (*status != DeliveryStatus::Pending).then_some(now.as_millis());
    let changed = tx.execute(
        "UPDATE send_timings SET
            first_change_at = COALESCE(first_change_at, ?2),
            sent_at = COALESCE(sent_at, ?3),
            delivered_at = COALESCE(delivered_at, ?4),
            read_at = COALESCE(read_at, ?5),
            failed_at = COALESCE(failed_at, ?6)
         WHERE client_id = ?1",
        params![
            client_id,
            left_the_clock,
            reached(&DeliveryStatus::Sent),
            reached(&DeliveryStatus::Delivered),
            reached(&DeliveryStatus::Read),
            failed,
        ],
    )?;
    if changed > 0 && tracing::enabled!(tracing::Level::DEBUG) {
        let queued_at: Option<i64> = tx
            .query_row(
                "SELECT queued_at FROM send_timings WHERE client_id = ?1",
                params![client_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(queued_at) = queued_at {
            tracing::debug!(
                client_id,
                status = super::status_to_parts(status).0,
                since_enter_ms = now.as_millis() - queued_at,
                "send timing: the status moved"
            );
        }
    }
    Ok(())
}

fn read_timings(conn: &Connection, limit: usize) -> StoreResult<Vec<SendTiming>> {
    let mut stmt = conn.prepare(
        "SELECT client_id, account_id, chat_id, kind, queued_at, post_started_at, attempts,
                accepted_at, post_ms, first_change_at, sent_at, delivered_at, read_at, failed_at
         FROM send_timings ORDER BY queued_at DESC, client_id LIMIT ?1",
    )?;
    let at = |millis: Option<i64>| millis.map(Timestamp::from_millis);
    let rows = stmt.query_map(params![limit as i64], |row| {
        Ok(SendTiming {
            client_id: ClientMessageId::new(row.get::<_, String>(0)?),
            account_id: AccountId::new(row.get::<_, String>(1)?),
            chat_id: ChatId::new(row.get::<_, String>(2)?),
            kind: row.get(3)?,
            queued_at: Timestamp::from_millis(row.get(4)?),
            post_started_at: at(row.get(5)?),
            attempts: row.get(6)?,
            accepted_at: at(row.get(7)?),
            post: row
                .get::<_, Option<i64>>(8)?
                .map(|millis| Duration::from_millis(millis.max(0) as u64)),
            first_change_at: at(row.get(9)?),
            sent_at: at(row.get(10)?),
            delivered_at: at(row.get(11)?),
            read_at: at(row.get(12)?),
            failed_at: at(row.get(13)?),
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

impl Store {
    /// A request for the message is about to leave.
    pub(crate) fn send_timing_attempt(
        &self,
        client_id: &ClientMessageId,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE send_timings SET post_started_at = COALESCE(post_started_at, ?2),
                        attempts = attempts + 1
                 WHERE client_id = ?1",
                params![client_id.as_str(), now.as_millis()],
            )?;
            Ok(())
        })
    }

    /// The newest `limit` sends, newest first.
    pub fn send_timings(&self, limit: usize) -> StoreResult<Vec<SendTiming>> {
        self.read(|conn| read_timings(conn, limit))
    }

    /// The newest `limit` sends of the database at `path`, read without
    /// opening it as a store: nothing is migrated or written, so this is
    /// safe beside a running application. `None` when the file is from a
    /// build that did not measure sends yet.
    pub fn read_send_timings(
        path: impl AsRef<Path>,
        key: Option<&StoreKey>,
        limit: usize,
    ) -> StoreResult<Option<Vec<SendTiming>>> {
        let conn = connect(path.as_ref(), key)?;
        conn.pragma_update(None, "query_only", true)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        let measured: bool = conn.query_row(
            "SELECT count(*) > 0 FROM sqlite_master WHERE type = 'table' AND name = 'send_timings'",
            [],
            |row| row.get(0),
        )?;
        if !measured {
            return Ok(None);
        }
        read_timings(&conn, limit).map(Some)
    }
}
