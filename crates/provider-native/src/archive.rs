//! What this device has seen, kept so that it can be listed.
//!
//! WhatsApp has no "list my chats" or "give me this chat's messages": a
//! linked device is handed its history once and then hears each message as
//! it happens. The client's engine asks a provider for listings, so this
//! provider keeps what it was handed and answers from it. It is the
//! backend's memory, not the client's store: nothing here is searched or
//! shown, and the client still keeps its own copy.
//!
//! The file is encrypted (SQLCipher) under the key the application gives.

use crate::mapping::PollVotes;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::sync::Mutex;

/// A failure of the archive itself.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// SQLite refused.
    #[error("the session archive failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Shorthand for the archive's results.
pub type ArchiveResult<T> = Result<T, ArchiveError>;

/// One linked number, as it was last known.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredAccount {
    /// The account's id (`native:<device>`).
    pub id: String,
    /// The session's number in the protocol store.
    pub device: i32,
    /// The user's label.
    pub label: Option<String>,
    /// Its number, as an id (`+…`).
    pub phone: Option<String>,
    /// Its hidden-number id (`lid:…`).
    pub lid: Option<String>,
    /// The name the account gave itself.
    pub push_name: Option<String>,
    /// A phone was linked to it at least once.
    pub ever_linked: bool,
    /// The phone's history is imported when it links.
    pub history: bool,
}

/// One message, as it travelled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    /// The chat it is in, as a neutral id.
    pub chat: String,
    /// WhatsApp's id of it.
    pub id: String,
    /// Who wrote it, as a neutral id.
    pub sender: String,
    /// The name it came with.
    pub sender_name: Option<String>,
    /// The account wrote it.
    pub from_me: bool,
    /// When, in milliseconds.
    pub timestamp_ms: i64,
    /// How far it got, as [`client_provider::DeliveryStatus::rank`].
    pub status: u8,
    /// The account starred it.
    pub starred: bool,
    /// Its text was replaced.
    pub edited: bool,
    /// It was taken back.
    pub deleted: bool,
    /// The key it was sent under from here.
    pub client_id: Option<String>,
    /// The message itself, encoded.
    pub proto: Vec<u8>,
}

/// A chat, as far as this device knows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredChat {
    /// Its neutral id.
    pub id: String,
    /// Its name, when the phone or the group gave one.
    pub name: Option<String>,
    /// How many messages are unread.
    pub unread: u32,
    /// When it was pinned, in milliseconds; 0 for a pin without a time.
    pub pinned_at_ms: Option<i64>,
    /// Until when it is muted, in milliseconds; -1 for "until unmuted".
    pub muted_until_ms: Option<i64>,
    /// It is in the archive.
    pub archived: bool,
    /// It is flagged as unread.
    pub marked_unread: bool,
    /// When its last message was sent.
    pub last_ms: i64,
    /// The id of its picture.
    pub picture_id: Option<String>,
}

/// A person, as far as this device knows them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoredContact {
    /// Their neutral id.
    pub id: String,
    /// The name in the account's address book.
    pub saved_name: Option<String>,
    /// The name they gave themselves.
    pub profile_name: Option<String>,
    /// A business's verified name.
    pub business_name: Option<String>,
    /// Their username.
    pub username: Option<String>,
}

/// A vote in a poll.
#[derive(Clone, Copy, Debug)]
pub struct Vote<'a> {
    /// The chat the poll is in.
    pub chat: &'a str,
    /// The poll.
    pub poll: &'a str,
    /// Who voted.
    pub voter: &'a str,
    /// The account itself.
    pub mine: bool,
    /// The hashes of the options they now stand for.
    pub hashes: &'a [Vec<u8>],
    /// When, in milliseconds.
    pub timestamp_ms: i64,
}

/// Someone who saw a story of the account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredView {
    /// Who.
    pub viewer: String,
    /// When, in milliseconds.
    pub at_ms: i64,
    /// What they reacted with.
    pub reaction: Option<String>,
}

/// The archive.
pub struct Archive {
    db: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS accounts (
    id TEXT PRIMARY KEY, device INTEGER NOT NULL, label TEXT, phone TEXT, lid TEXT,
    push_name TEXT, ever_linked INTEGER NOT NULL DEFAULT 0, history INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS chats (
    account TEXT NOT NULL, id TEXT NOT NULL, name TEXT, unread INTEGER NOT NULL DEFAULT 0,
    pinned_at INTEGER, muted_until INTEGER, archived INTEGER NOT NULL DEFAULT 0,
    marked_unread INTEGER NOT NULL DEFAULT 0, last_ms INTEGER NOT NULL DEFAULT 0, picture_id TEXT,
    PRIMARY KEY (account, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS messages (
    account TEXT NOT NULL, chat TEXT NOT NULL, id TEXT NOT NULL, sender TEXT NOT NULL,
    sender_name TEXT, from_me INTEGER NOT NULL, ts INTEGER NOT NULL, status INTEGER NOT NULL,
    starred INTEGER NOT NULL DEFAULT 0, edited INTEGER NOT NULL DEFAULT 0,
    deleted INTEGER NOT NULL DEFAULT 0, client_id TEXT, proto BLOB NOT NULL,
    PRIMARY KEY (account, chat, id)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS messages_by_time ON messages (account, chat, ts DESC, id DESC);
CREATE INDEX IF NOT EXISTS messages_by_id ON messages (account, id);
CREATE TABLE IF NOT EXISTS sends (
    account TEXT NOT NULL, client_id TEXT NOT NULL, message_id TEXT NOT NULL, ts INTEGER NOT NULL,
    PRIMARY KEY (account, client_id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS contacts (
    account TEXT NOT NULL, id TEXT NOT NULL, saved_name TEXT, profile_name TEXT,
    business_name TEXT, username TEXT,
    PRIMARY KEY (account, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS aliases (
    account TEXT NOT NULL, lid TEXT NOT NULL, pn TEXT NOT NULL,
    PRIMARY KEY (account, lid)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS aliases_by_pn ON aliases (account, pn);
CREATE TABLE IF NOT EXISTS poll_votes (
    account TEXT NOT NULL, chat TEXT NOT NULL, poll TEXT NOT NULL, voter TEXT NOT NULL,
    mine INTEGER NOT NULL, hashes TEXT NOT NULL, ts INTEGER NOT NULL,
    PRIMARY KEY (account, chat, poll, voter)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS story_views (
    account TEXT NOT NULL, story TEXT NOT NULL, viewer TEXT NOT NULL, at_ms INTEGER NOT NULL,
    reaction TEXT,
    PRIMARY KEY (account, story, viewer)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS pins (
    account TEXT NOT NULL, chat TEXT NOT NULL, id TEXT NOT NULL, until_ms INTEGER NOT NULL,
    PRIMARY KEY (account, chat, id)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS kv (
    account TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
    PRIMARY KEY (account, key)
) WITHOUT ROWID;
";

/// How long an idempotency record is kept: longer than the window the
/// client may repeat a send in.
const SENDS_KEPT_MS: i64 = 3 * 24 * 60 * 60 * 1000;

fn message_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMessage> {
    Ok(StoredMessage {
        chat: row.get(0)?,
        id: row.get(1)?,
        sender: row.get(2)?,
        sender_name: row.get(3)?,
        from_me: row.get(4)?,
        timestamp_ms: row.get(5)?,
        status: row.get(6)?,
        starred: row.get(7)?,
        edited: row.get(8)?,
        deleted: row.get(9)?,
        client_id: row.get(10)?,
        proto: row.get(11)?,
    })
}

const MESSAGE_COLUMNS: &str =
    "chat, id, sender, sender_name, from_me, ts, status, starred, edited, deleted, client_id, proto";

fn chat_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredChat> {
    Ok(StoredChat {
        id: row.get(0)?,
        name: row.get(1)?,
        unread: row.get(2)?,
        pinned_at_ms: row.get(3)?,
        muted_until_ms: row.get(4)?,
        archived: row.get(5)?,
        marked_unread: row.get(6)?,
        last_ms: row.get(7)?,
        picture_id: row.get(8)?,
    })
}

const CHAT_COLUMNS: &str =
    "id, name, unread, pinned_at, muted_until, archived, marked_unread, last_ms, picture_id";

impl Archive {
    /// Opens the archive at `path`, encrypted under `key_hex` (64
    /// hexadecimal digits).
    pub fn open(path: &Path, key_hex: Option<&str>) -> ArchiveResult<Self> {
        let db = Connection::open(path)?;
        if let Some(key) = key_hex.filter(|key| key.chars().all(|c| c.is_ascii_hexdigit())) {
            // Hex digits only, so it is safe in SQL text, which is the only
            // way SQLCipher takes a raw key.
            db.execute_batch(&format!("PRAGMA key = \"x'{key}'\";"))?;
        }
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.pragma_update(None, "synchronous", "NORMAL")?;
        Self::with(db)
    }

    /// An archive that lasts as long as the process.
    pub fn in_memory() -> ArchiveResult<Self> {
        Self::with(Connection::open_in_memory()?)
    }

    fn with(db: Connection) -> ArchiveResult<Self> {
        db.execute_batch(SCHEMA)?;
        Ok(Self { db: Mutex::new(db) })
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ----- accounts -------------------------------------------------------

    /// Writes an account, whole.
    pub fn put_account(&self, account: &StoredAccount) -> ArchiveResult<()> {
        self.db().execute(
            "INSERT INTO accounts (id, device, label, phone, lid, push_name, ever_linked, history)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (id) DO UPDATE SET device = ?2, label = ?3, phone = ?4, lid = ?5,
                 push_name = ?6, ever_linked = ?7, history = ?8",
            params![
                account.id,
                account.device,
                account.label,
                account.phone,
                account.lid,
                account.push_name,
                account.ever_linked,
                account.history
            ],
        )?;
        Ok(())
    }

    /// Every account, in the order they were linked.
    pub fn accounts(&self) -> ArchiveResult<Vec<StoredAccount>> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT id, device, label, phone, lid, push_name, ever_linked, history
             FROM accounts ORDER BY device",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(StoredAccount {
                id: row.get(0)?,
                device: row.get(1)?,
                label: row.get(2)?,
                phone: row.get(3)?,
                lid: row.get(4)?,
                push_name: row.get(5)?,
                ever_linked: row.get(6)?,
                history: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// One account.
    pub fn account(&self, id: &str) -> ArchiveResult<Option<StoredAccount>> {
        Ok(self
            .accounts()?
            .into_iter()
            .find(|account| account.id == id))
    }

    /// Forgets what an account held: its chats, messages and people. The
    /// account's own row stays unless `row` is set.
    pub fn clear_account(&self, id: &str, row: bool) -> ArchiveResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        for table in [
            "chats",
            "messages",
            "sends",
            "contacts",
            "aliases",
            "poll_votes",
            "story_views",
            "pins",
            "kv",
        ] {
            tx.execute(&format!("DELETE FROM {table} WHERE account = ?1"), [id])?;
        }
        if row {
            tx.execute("DELETE FROM accounts WHERE id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }

    // ----- messages -------------------------------------------------------

    /// Writes a message. One that is already here keeps what only this
    /// device knows about it (the key it was sent under, a star) and never
    /// loses ground: its status only moves forward, and a copy that was
    /// taken back stays taken back.
    pub fn put_message(&self, account: &str, message: &StoredMessage) -> ArchiveResult<()> {
        self.db().execute(
            "INSERT INTO messages (account, chat, id, sender, sender_name, from_me, ts, status,
                 starred, edited, deleted, client_id, proto)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT (account, chat, id) DO UPDATE SET
                 sender_name = COALESCE(excluded.sender_name, sender_name),
                 status = MAX(status, excluded.status),
                 starred = MAX(starred, excluded.starred),
                 edited = MAX(edited, excluded.edited),
                 deleted = MAX(deleted, excluded.deleted),
                 client_id = COALESCE(client_id, excluded.client_id),
                 proto = CASE WHEN deleted = 1 OR edited > excluded.edited
                     THEN proto ELSE excluded.proto END",
            params![
                account,
                message.chat,
                message.id,
                message.sender,
                message.sender_name,
                message.from_me,
                message.timestamp_ms,
                message.status,
                message.starred,
                message.edited,
                message.deleted,
                message.client_id,
                message.proto
            ],
        )?;
        Ok(())
    }

    /// One message of a chat.
    pub fn message(
        &self,
        account: &str,
        chat: &str,
        id: &str,
    ) -> ArchiveResult<Option<StoredMessage>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages
                     WHERE account = ?1 AND chat = ?2 AND id = ?3"
                ),
                params![account, chat, id],
                message_from,
            )
            .optional()?)
    }

    /// A message by its id alone, in whichever chat it is. Ids are made at
    /// random by the device that sends, so two chats do not share one.
    pub fn find_message(&self, account: &str, id: &str) -> ArchiveResult<Option<StoredMessage>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages
                     WHERE account = ?1 AND id = ?2 ORDER BY ts DESC LIMIT 1"
                ),
                params![account, id],
                message_from,
            )
            .optional()?)
    }

    /// A page of a chat, newest first, starting after `before` (a
    /// message's time and id).
    pub fn page(
        &self,
        account: &str,
        chat: &str,
        before: Option<(i64, &str)>,
        limit: u32,
    ) -> ArchiveResult<Vec<StoredMessage>> {
        let db = self.db();
        let (ts, id) = before.unwrap_or((i64::MAX, ""));
        let mut statement = db.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE account = ?1 AND chat = ?2
               AND (?3 = 0 OR ts < ?4 OR (ts = ?4 AND id < ?5))
             ORDER BY ts DESC, id DESC LIMIT ?6"
        ))?;
        let rows = statement.query_map(
            params![account, chat, before.is_some(), ts, id, limit],
            message_from,
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The oldest message of a chat this device holds.
    pub fn oldest(&self, account: &str, chat: &str) -> ArchiveResult<Option<StoredMessage>> {
        Ok(self
            .db()
            .query_row(
                &format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages
                     WHERE account = ?1 AND chat = ?2 ORDER BY ts ASC, id ASC LIMIT 1"
                ),
                params![account, chat],
                message_from,
            )
            .optional()?)
    }

    /// The newest message of a chat this device holds.
    pub fn newest(&self, account: &str, chat: &str) -> ArchiveResult<Option<StoredMessage>> {
        Ok(self.page(account, chat, None, 1)?.into_iter().next())
    }

    /// The received messages of a chat nobody was told were read, newest
    /// first.
    pub fn unread_incoming(
        &self,
        account: &str,
        chat: &str,
        limit: u32,
    ) -> ArchiveResult<Vec<StoredMessage>> {
        let db = self.db();
        let mut statement = db.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE account = ?1 AND chat = ?2 AND from_me = 0 AND status < 5
             ORDER BY ts DESC, id DESC LIMIT ?3"
        ))?;
        let rows = statement.query_map(params![account, chat, limit], message_from)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Moves a message's status forward. Answers whether it moved.
    pub fn advance(&self, account: &str, chat: &str, id: &str, status: u8) -> ArchiveResult<bool> {
        let changed = self.db().execute(
            "UPDATE messages SET status = ?4
             WHERE account = ?1 AND chat = ?2 AND id = ?3 AND status < ?4",
            params![account, chat, id, status],
        )?;
        Ok(changed > 0)
    }

    /// Marks a message as taken back.
    pub fn mark_deleted(&self, account: &str, chat: &str, id: &str) -> ArchiveResult<bool> {
        let changed = self.db().execute(
            "UPDATE messages SET deleted = 1 WHERE account = ?1 AND chat = ?2 AND id = ?3",
            params![account, chat, id],
        )?;
        Ok(changed > 0)
    }

    /// Removes a message from this device's memory (deleted for the
    /// account alone).
    pub fn remove_message(&self, account: &str, chat: &str, id: &str) -> ArchiveResult<()> {
        self.db().execute(
            "DELETE FROM messages WHERE account = ?1 AND chat = ?2 AND id = ?3",
            params![account, chat, id],
        )?;
        Ok(())
    }

    /// Replaces what a message says, marking it edited.
    pub fn replace(
        &self,
        account: &str,
        chat: &str,
        id: &str,
        proto: &[u8],
    ) -> ArchiveResult<bool> {
        let changed = self.db().execute(
            "UPDATE messages SET proto = ?4, edited = 1
             WHERE account = ?1 AND chat = ?2 AND id = ?3 AND deleted = 0",
            params![account, chat, id, proto],
        )?;
        Ok(changed > 0)
    }

    /// Stars or unstars a message.
    pub fn set_starred(
        &self,
        account: &str,
        chat: &str,
        id: &str,
        starred: bool,
    ) -> ArchiveResult<bool> {
        let changed = self.db().execute(
            "UPDATE messages SET starred = ?4 WHERE account = ?1 AND chat = ?2 AND id = ?3",
            params![account, chat, id, starred],
        )?;
        Ok(changed > 0)
    }

    /// Pins a message until `until_ms`, or unpins it (`None`).
    pub fn set_pin(
        &self,
        account: &str,
        chat: &str,
        id: &str,
        until_ms: Option<i64>,
    ) -> ArchiveResult<()> {
        match until_ms {
            Some(until) => self.db().execute(
                "INSERT INTO pins (account, chat, id, until_ms) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account, chat, id) DO UPDATE SET until_ms = excluded.until_ms",
                params![account, chat, id, until],
            )?,
            None => self.db().execute(
                "DELETE FROM pins WHERE account = ?1 AND chat = ?2 AND id = ?3",
                params![account, chat, id],
            )?,
        };
        Ok(())
    }

    /// Until when a message is pinned, if it is or was.
    pub fn pin_until(&self, account: &str, chat: &str, id: &str) -> ArchiveResult<Option<i64>> {
        Ok(self
            .db()
            .query_row(
                "SELECT until_ms FROM pins WHERE account = ?1 AND chat = ?2 AND id = ?3",
                params![account, chat, id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Moves everything a chat holds to another id: the same person was
    /// known by a hidden-number id and is now known by their number.
    pub fn merge_chat(&self, account: &str, from: &str, into: &str) -> ArchiveResult<()> {
        let mut db = self.db();
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE OR IGNORE messages SET chat = ?3 WHERE account = ?1 AND chat = ?2",
            params![account, from, into],
        )?;
        tx.execute(
            "DELETE FROM messages WHERE account = ?1 AND chat = ?2",
            params![account, from],
        )?;
        tx.execute(
            "UPDATE OR IGNORE pins SET chat = ?3 WHERE account = ?1 AND chat = ?2",
            params![account, from, into],
        )?;
        tx.execute(
            "DELETE FROM pins WHERE account = ?1 AND chat = ?2",
            params![account, from],
        )?;
        tx.execute(
            "UPDATE OR IGNORE chats SET id = ?3 WHERE account = ?1 AND id = ?2",
            params![account, from, into],
        )?;
        tx.execute(
            "DELETE FROM chats WHERE account = ?1 AND id = ?2",
            params![account, from],
        )?;
        tx.commit()?;
        Ok(())
    }

    // ----- sends ----------------------------------------------------------

    /// The message a key was already sent as, with its time.
    pub fn sent_as(&self, account: &str, client_id: &str) -> ArchiveResult<Option<(String, i64)>> {
        Ok(self
            .db()
            .query_row(
                "SELECT message_id, ts FROM sends WHERE account = ?1 AND client_id = ?2",
                params![account, client_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// Records that a key went out as a message, and forgets the records
    /// nobody can ask about any more.
    pub fn record_send(
        &self,
        account: &str,
        client_id: &str,
        message_id: &str,
        now_ms: i64,
    ) -> ArchiveResult<()> {
        let db = self.db();
        db.execute(
            "INSERT OR REPLACE INTO sends (account, client_id, message_id, ts)
             VALUES (?1, ?2, ?3, ?4)",
            params![account, client_id, message_id, now_ms],
        )?;
        db.execute("DELETE FROM sends WHERE ts < ?1", [now_ms - SENDS_KEPT_MS])?;
        Ok(())
    }

    // ----- chats ----------------------------------------------------------

    /// One chat.
    pub fn chat(&self, account: &str, id: &str) -> ArchiveResult<Option<StoredChat>> {
        Ok(self
            .db()
            .query_row(
                &format!("SELECT {CHAT_COLUMNS} FROM chats WHERE account = ?1 AND id = ?2"),
                params![account, id],
                chat_from,
            )
            .optional()?)
    }

    /// Writes a chat, whole.
    pub fn put_chat(&self, account: &str, chat: &StoredChat) -> ArchiveResult<()> {
        self.db().execute(
            "INSERT INTO chats (account, id, name, unread, pinned_at, muted_until, archived,
                 marked_unread, last_ms, picture_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT (account, id) DO UPDATE SET name = ?3, unread = ?4, pinned_at = ?5,
                 muted_until = ?6, archived = ?7, marked_unread = ?8, last_ms = ?9,
                 picture_id = ?10",
            params![
                account,
                chat.id,
                chat.name,
                chat.unread,
                chat.pinned_at_ms,
                chat.muted_until_ms,
                chat.archived,
                chat.marked_unread,
                chat.last_ms,
                chat.picture_id
            ],
        )?;
        Ok(())
    }

    /// The chat with this id, made if it was not there, changed by
    /// `change` and written back.
    pub fn update_chat(
        &self,
        account: &str,
        id: &str,
        change: impl FnOnce(&mut StoredChat),
    ) -> ArchiveResult<StoredChat> {
        let mut chat = self.chat(account, id)?.unwrap_or_else(|| StoredChat {
            id: id.to_owned(),
            ..StoredChat::default()
        });
        change(&mut chat);
        self.put_chat(account, &chat)?;
        Ok(chat)
    }

    /// A page of the chat list, most recently active first.
    pub fn chats(&self, account: &str, offset: u32, limit: u32) -> ArchiveResult<Vec<StoredChat>> {
        let db = self.db();
        let mut statement = db.prepare(&format!(
            "SELECT {CHAT_COLUMNS} FROM chats WHERE account = ?1
             ORDER BY last_ms DESC, id LIMIT ?2 OFFSET ?3"
        ))?;
        let rows = statement.query_map(params![account, limit, offset], chat_from)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Forgets a chat and its messages.
    pub fn remove_chat(&self, account: &str, id: &str) -> ArchiveResult<()> {
        let db = self.db();
        db.execute(
            "DELETE FROM messages WHERE account = ?1 AND chat = ?2",
            params![account, id],
        )?;
        db.execute(
            "DELETE FROM chats WHERE account = ?1 AND id = ?2",
            params![account, id],
        )?;
        Ok(())
    }

    // ----- people ---------------------------------------------------------

    /// One person.
    pub fn contact(&self, account: &str, id: &str) -> ArchiveResult<Option<StoredContact>> {
        Ok(self
            .db()
            .query_row(
                "SELECT id, saved_name, profile_name, business_name, username FROM contacts
                 WHERE account = ?1 AND id = ?2",
                params![account, id],
                |row| {
                    Ok(StoredContact {
                        id: row.get(0)?,
                        saved_name: row.get(1)?,
                        profile_name: row.get(2)?,
                        business_name: row.get(3)?,
                        username: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    /// Adds what is known about a person to what was known. A name that
    /// is `None` leaves the one held. Answers the person as now known,
    /// and whether anything changed.
    pub fn learn_contact(
        &self,
        account: &str,
        learned: &StoredContact,
    ) -> ArchiveResult<(StoredContact, bool)> {
        let before = self.contact(account, &learned.id)?;
        let mut after = before.clone().unwrap_or_else(|| StoredContact {
            id: learned.id.clone(),
            ..StoredContact::default()
        });
        let take = |held: &mut Option<String>, new: &Option<String>| {
            if let Some(new) = new.as_ref().filter(|text| !text.trim().is_empty()) {
                *held = Some(new.clone());
            }
        };
        take(&mut after.saved_name, &learned.saved_name);
        take(&mut after.profile_name, &learned.profile_name);
        take(&mut after.business_name, &learned.business_name);
        take(&mut after.username, &learned.username);
        let changed = before.as_ref() != Some(&after);
        if changed {
            self.db().execute(
                "INSERT OR REPLACE INTO contacts
                     (account, id, saved_name, profile_name, business_name, username)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    account,
                    after.id,
                    after.saved_name,
                    after.profile_name,
                    after.business_name,
                    after.username
                ],
            )?;
        }
        Ok((after, changed))
    }

    /// Forgets the name the account saved somebody under.
    pub fn forget_saved_name(&self, account: &str, id: &str) -> ArchiveResult<()> {
        self.db().execute(
            "UPDATE contacts SET saved_name = NULL WHERE account = ?1 AND id = ?2",
            params![account, id],
        )?;
        Ok(())
    }

    /// Everybody this device knows something about.
    pub fn contacts(&self, account: &str) -> ArchiveResult<Vec<StoredContact>> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT id, saved_name, profile_name, business_name, username FROM contacts
             WHERE account = ?1 ORDER BY id",
        )?;
        let rows = statement.query_map([account], |row| {
            Ok(StoredContact {
                id: row.get(0)?,
                saved_name: row.get(1)?,
                profile_name: row.get(2)?,
                business_name: row.get(3)?,
                username: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Records that a hidden-number id and a number are one person.
    /// Answers whether that was news.
    pub fn link_ids(&self, account: &str, lid: &str, pn: &str) -> ArchiveResult<bool> {
        if self.number_of(account, lid)?.as_deref() == Some(pn) {
            return Ok(false);
        }
        self.db().execute(
            "INSERT OR REPLACE INTO aliases (account, lid, pn) VALUES (?1, ?2, ?3)",
            params![account, lid, pn],
        )?;
        Ok(true)
    }

    /// The number of a hidden-number id, when it is known.
    pub fn number_of(&self, account: &str, lid: &str) -> ArchiveResult<Option<String>> {
        Ok(self
            .db()
            .query_row(
                "SELECT pn FROM aliases WHERE account = ?1 AND lid = ?2",
                params![account, lid],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The hidden-number id of a number, when it is known.
    pub fn hidden_id_of(&self, account: &str, pn: &str) -> ArchiveResult<Option<String>> {
        Ok(self
            .db()
            .query_row(
                "SELECT lid FROM aliases WHERE account = ?1 AND pn = ?2 LIMIT 1",
                params![account, pn],
                |row| row.get(0),
            )
            .optional()?)
    }

    // ----- polls ----------------------------------------------------------

    /// Records what a voter stands for in a poll, replacing their earlier
    /// vote unless this one is older.
    pub fn put_vote(&self, account: &str, vote: &Vote<'_>) -> ArchiveResult<()> {
        let Vote {
            chat,
            poll,
            voter,
            mine,
            hashes,
            timestamp_ms,
        } = *vote;
        let encoded: Vec<String> = hashes
            .iter()
            .map(|hash| hash.iter().map(|byte| format!("{byte:02x}")).collect())
            .collect();
        self.db().execute(
            "INSERT INTO poll_votes (account, chat, poll, voter, mine, hashes, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT (account, chat, poll, voter) DO UPDATE SET
                 mine = excluded.mine, hashes = excluded.hashes, ts = excluded.ts
             WHERE excluded.ts >= ts",
            params![
                account,
                chat,
                poll,
                voter,
                mine,
                encoded.join(","),
                timestamp_ms
            ],
        )?;
        Ok(())
    }

    /// The votes of a poll.
    pub fn votes(&self, account: &str, chat: &str, poll: &str) -> ArchiveResult<PollVotes> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT mine, hashes FROM poll_votes WHERE account = ?1 AND chat = ?2 AND poll = ?3",
        )?;
        let rows = statement.query_map(params![account, chat, poll], |row| {
            let mine: bool = row.get(0)?;
            let hashes: String = row.get(1)?;
            Ok((mine, hashes))
        })?;
        let mut votes = PollVotes::default();
        for row in rows {
            let (mine, hashes) = row?;
            let decoded = hashes
                .split(',')
                .filter(|hash| !hash.is_empty())
                .filter_map(|hash| {
                    (0..hash.len() / 2)
                        .map(|i| u8::from_str_radix(hash.get(i * 2..i * 2 + 2)?, 16).ok())
                        .collect::<Option<Vec<u8>>>()
                })
                .collect();
            votes.voters.push((mine, decoded));
        }
        Ok(votes)
    }

    // ----- stories --------------------------------------------------------

    /// Records that somebody saw a story of the account, keeping the
    /// first time and the latest reaction.
    pub fn put_view(&self, account: &str, story: &str, view: &StoredView) -> ArchiveResult<()> {
        self.db().execute(
            "INSERT INTO story_views (account, story, viewer, at_ms, reaction)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (account, story, viewer) DO UPDATE SET
                 at_ms = MIN(at_ms, excluded.at_ms),
                 reaction = COALESCE(excluded.reaction, reaction)",
            params![account, story, view.viewer, view.at_ms, view.reaction],
        )?;
        Ok(())
    }

    /// Who saw a story, first viewer first.
    pub fn views(&self, account: &str, story: &str) -> ArchiveResult<Vec<StoredView>> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT viewer, at_ms, reaction FROM story_views
             WHERE account = ?1 AND story = ?2 ORDER BY at_ms",
        )?;
        let rows = statement.query_map(params![account, story], |row| {
            Ok(StoredView {
                viewer: row.get(0)?,
                at_ms: row.get(1)?,
                reaction: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Messages of a chat sent after a moment, oldest first.
    pub fn since(
        &self,
        account: &str,
        chat: &str,
        after_ms: i64,
    ) -> ArchiveResult<Vec<StoredMessage>> {
        let db = self.db();
        let mut statement = db.prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE account = ?1 AND chat = ?2 AND ts > ?3 ORDER BY ts ASC, id ASC"
        ))?;
        let rows = statement.query_map(params![account, chat, after_ms], message_from)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Forgets the messages of a chat sent before a moment.
    pub fn forget_before(&self, account: &str, chat: &str, before_ms: i64) -> ArchiveResult<()> {
        self.db().execute(
            "DELETE FROM messages WHERE account = ?1 AND chat = ?2 AND ts < ?3",
            params![account, chat, before_ms],
        )?;
        Ok(())
    }

    // ----- anything else, by name ------------------------------------------

    /// A value kept under a name.
    pub fn value(&self, account: &str, key: &str) -> ArchiveResult<Option<String>> {
        Ok(self
            .db()
            .query_row(
                "SELECT value FROM kv WHERE account = ?1 AND key = ?2",
                params![account, key],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Keeps a value under a name; `None` forgets it.
    pub fn set_value(&self, account: &str, key: &str, value: Option<&str>) -> ArchiveResult<()> {
        match value {
            Some(value) => self.db().execute(
                "INSERT OR REPLACE INTO kv (account, key, value) VALUES (?1, ?2, ?3)",
                params![account, key, value],
            )?,
            None => self.db().execute(
                "DELETE FROM kv WHERE account = ?1 AND key = ?2",
                params![account, key],
            )?,
        };
        Ok(())
    }

    /// The names kept under a prefix, with their values.
    pub fn values(&self, account: &str, prefix: &str) -> ArchiveResult<Vec<(String, String)>> {
        let db = self.db();
        let mut statement = db.prepare(
            "SELECT key, value FROM kv WHERE account = ?1 AND substr(key, 1, ?3) = ?2 ORDER BY key",
        )?;
        let rows = statement.query_map(params![account, prefix, prefix.len() as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "native:1";

    fn message(chat: &str, id: &str, ts: i64) -> StoredMessage {
        StoredMessage {
            chat: chat.into(),
            id: id.into(),
            sender: "+584245550199".into(),
            sender_name: None,
            from_me: false,
            timestamp_ms: ts,
            status: 4,
            starred: false,
            edited: false,
            deleted: false,
            client_id: None,
            proto: vec![1, 2, 3],
        }
    }

    #[test]
    fn a_chat_pages_newest_first_to_its_start() {
        let archive = Archive::in_memory().unwrap();
        for n in 0..5 {
            archive
                .put_message(ME, &message("+1", &format!("M{n}"), 1000 + n))
                .unwrap();
        }
        archive
            .put_message(ME, &message("+2", "OTHER", 5000))
            .unwrap();

        let first = archive.page(ME, "+1", None, 2).unwrap();
        assert_eq!(ids(&first), ["M4", "M3"]);
        let last = first.last().unwrap();
        let second = archive
            .page(ME, "+1", Some((last.timestamp_ms, &last.id)), 10)
            .unwrap();
        assert_eq!(ids(&second), ["M2", "M1", "M0"]);
        assert_eq!(archive.oldest(ME, "+1").unwrap().unwrap().id, "M0");
        assert_eq!(archive.newest(ME, "+1").unwrap().unwrap().id, "M4");
    }

    fn ids(messages: &[StoredMessage]) -> Vec<&str> {
        messages.iter().map(|message| message.id.as_str()).collect()
    }

    #[test]
    fn a_second_copy_never_loses_what_the_first_knew() {
        let archive = Archive::in_memory().unwrap();
        let mut sent = message("+1", "M", 1000);
        sent.from_me = true;
        sent.status = 5;
        sent.client_id = Some("key".into());
        archive.put_message(ME, &sent).unwrap();

        // The server's copy: older status, no key.
        let mut copy = message("+1", "M", 1000);
        copy.from_me = true;
        copy.status = 3;
        archive.put_message(ME, &copy).unwrap();

        let held = archive.message(ME, "+1", "M").unwrap().unwrap();
        assert_eq!(held.status, 5, "a status only moves forward");
        assert_eq!(held.client_id.as_deref(), Some("key"));

        assert!(!archive.advance(ME, "+1", "M", 4).unwrap());
        assert!(archive.mark_deleted(ME, "+1", "M").unwrap());
        let mut late = message("+1", "M", 1000);
        late.proto = vec![9];
        archive.put_message(ME, &late).unwrap();
        let held = archive.message(ME, "+1", "M").unwrap().unwrap();
        assert!(held.deleted, "taken back stays taken back");
        assert_eq!(held.proto, vec![1, 2, 3]);
    }

    #[test]
    fn a_key_is_remembered_as_the_message_it_went_out_as() {
        let archive = Archive::in_memory().unwrap();
        assert_eq!(archive.sent_as(ME, "key").unwrap(), None);
        archive.record_send(ME, "key", "3EB0AA", 10_000).unwrap();
        assert_eq!(
            archive.sent_as(ME, "key").unwrap(),
            Some(("3EB0AA".into(), 10_000))
        );
        // Long after the window, the record goes.
        archive
            .record_send(ME, "later", "3EB0BB", 10_000 + SENDS_KEPT_MS + 1)
            .unwrap();
        assert_eq!(archive.sent_as(ME, "key").unwrap(), None);
    }

    #[test]
    fn names_add_up_and_a_missing_one_takes_nothing_away() {
        let archive = Archive::in_memory().unwrap();
        let (_, changed) = archive
            .learn_contact(
                ME,
                &StoredContact {
                    id: "+1".into(),
                    profile_name: Some("Ana P.".into()),
                    ..StoredContact::default()
                },
            )
            .unwrap();
        assert!(changed);
        let (known, changed) = archive
            .learn_contact(
                ME,
                &StoredContact {
                    id: "+1".into(),
                    saved_name: Some("Ana".into()),
                    ..StoredContact::default()
                },
            )
            .unwrap();
        assert!(changed);
        assert_eq!(known.saved_name.as_deref(), Some("Ana"));
        assert_eq!(known.profile_name.as_deref(), Some("Ana P."));
        let (_, changed) = archive
            .learn_contact(
                ME,
                &StoredContact {
                    id: "+1".into(),
                    ..StoredContact::default()
                },
            )
            .unwrap();
        assert!(!changed);
    }

    #[test]
    fn one_person_under_two_ids_becomes_one_chat() {
        let archive = Archive::in_memory().unwrap();
        archive.put_message(ME, &message("lid:9", "A", 1)).unwrap();
        archive.put_message(ME, &message("+1", "B", 2)).unwrap();
        archive
            .update_chat(ME, "lid:9", |chat| chat.unread = 2)
            .unwrap();
        assert!(archive.link_ids(ME, "lid:9", "+1").unwrap());
        assert!(!archive.link_ids(ME, "lid:9", "+1").unwrap());
        archive.merge_chat(ME, "lid:9", "+1").unwrap();

        assert_eq!(ids(&archive.page(ME, "+1", None, 10).unwrap()), ["B", "A"]);
        assert!(archive.chat(ME, "lid:9").unwrap().is_none());
        assert_eq!(
            archive.number_of(ME, "lid:9").unwrap().as_deref(),
            Some("+1")
        );
        assert_eq!(
            archive.hidden_id_of(ME, "+1").unwrap().as_deref(),
            Some("lid:9")
        );
    }

    #[test]
    fn a_later_vote_replaces_an_earlier_one() {
        let archive = Archive::in_memory().unwrap();
        let cast = |voter: &str, mine: bool, hashes: Vec<Vec<u8>>, timestamp_ms: i64| {
            let vote = Vote {
                chat: "g",
                poll: "P",
                voter,
                mine,
                hashes: &hashes,
                timestamp_ms,
            };
            archive.put_vote(ME, &vote).unwrap();
        };
        cast("+1", false, vec![vec![1, 2]], 10);
        cast("+1", false, vec![vec![3, 4]], 20);
        cast("+1", false, vec![vec![9]], 5);
        cast("+2", true, Vec::new(), 30);
        let votes = archive.votes(ME, "g", "P").unwrap();
        assert_eq!(
            votes.voters,
            [(false, vec![vec![3, 4]]), (true, Vec::new())]
        );
    }

    #[test]
    fn an_account_can_be_emptied_and_removed() {
        let archive = Archive::in_memory().unwrap();
        let account = StoredAccount {
            id: ME.into(),
            device: 1,
            history: true,
            ..StoredAccount::default()
        };
        archive.put_account(&account).unwrap();
        archive.put_message(ME, &message("+1", "A", 1)).unwrap();
        archive.set_value(ME, "k", Some("v")).unwrap();
        archive.clear_account(ME, false).unwrap();
        assert!(archive.page(ME, "+1", None, 10).unwrap().is_empty());
        assert_eq!(archive.value(ME, "k").unwrap(), None);
        assert_eq!(archive.accounts().unwrap(), [account]);
        archive.clear_account(ME, true).unwrap();
        assert!(archive.accounts().unwrap().is_empty());
    }

    #[test]
    fn an_encrypted_file_does_not_open_without_its_key() {
        let dir = std::env::temp_dir().join(format!("native-archive-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("archive.db");
        let _ = std::fs::remove_file(&path);
        let key = "ab".repeat(32);
        {
            let archive = Archive::open(&path, Some(&key)).unwrap();
            archive.put_message(ME, &message("+1", "A", 1)).unwrap();
        }
        assert!(Archive::open(&path, Some(&key)).is_ok());
        assert!(Archive::open(&path, Some(&"cd".repeat(32))).is_err());
        assert!(Archive::open(&path, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
