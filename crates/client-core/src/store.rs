//! The local SQLite store: the only thing the UI reads.
//!
//! Providers are slow and unreliable; this database is neither. Everything a
//! provider returns is copied here by the sync engine, and every screen is a
//! query against it. Writes announce themselves through [`StoreChange`]
//! notifications so views know when to re-query.

use crate::migrations;
use client_provider::{
    Account, AccountId, Chat, ChatChange, ChatId, ChatKind, ClientMessageId, ConnectionState,
    Contact, ContactId, Cursor, DeliveryStatus, Direction, MediaRef, Message, MessageContent,
    MessageId, Timestamp,
};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};
use tokio::sync::broadcast;

mod library;
mod people;
pub(crate) mod sends;
mod social;
mod stories;
mod story_posts;
pub use library::{
    IndexState, LibraryItem, LibraryKind, LibraryPack, LibrarySource, LibraryStats, NewLibraryItem,
    SeenSticker, StickerMessage,
};
pub(crate) use people::People;
pub use people::{NameSource, Person};
pub use sends::SendTiming;
pub use social::{StoredGroup, StoredParticipant};
pub use stories::{
    StoryAuthor, StoryFeed, StoryItem, StoryPostState, StoryReceipt, StoryRing, STORIES_KEPT,
};
pub use story_posts::{local_story_id, StoryPostEntry};

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite reported an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A stored JSON value could not be read back.
    #[error("corrupt stored value: {0}")]
    Corrupt(#[from] serde_json::Error),
    /// The file is not a database this key opens: the key is wrong, or the
    /// file is encrypted and no key was given, or it is plain and one was.
    #[error("the database cannot be opened with this key")]
    WrongKey,
    /// The database file could not be read or replaced.
    #[error("database file error: {0}")]
    Io(#[from] std::io::Error),
    /// The database was written by a newer version of the application.
    #[error("the database has schema version {found}, this build supports up to {supported}")]
    NewerSchema {
        /// Version found in the file.
        found: u32,
        /// Highest version this build knows.
        supported: u32,
    },
}

/// The key of an encrypted database: 32 raw bytes, handed to SQLCipher as
/// they are (no passphrase, so no key derivation on every open).
///
/// Where it comes from and where it is kept is the caller's business; the
/// store only uses it. `Debug` prints nothing of it.
#[derive(Clone, PartialEq, Eq)]
pub struct StoreKey([u8; 32]);

impl StoreKey {
    /// Wraps 32 bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The key as 64 hexadecimal digits, for a keychain that stores text.
    pub fn to_hex(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    /// Reads [`to_hex`](Self::to_hex) back. `None` for anything else.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.trim().as_bytes();
        if hex.len() != 64 {
            return None;
        }
        let mut bytes = [0u8; 32];
        for (byte, pair) in bytes.iter_mut().zip(hex.chunks(2)) {
            *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
        }
        Some(Self(bytes))
    }

    /// `PRAGMA key`'s raw-key literal. Hex digits only, so it is safe to
    /// put in SQL text, which is the only way SQLCipher takes it.
    fn literal(&self) -> String {
        format!("\"x'{}'\"", self.to_hex())
    }
}

impl std::fmt::Debug for StoreKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreKey(…)")
    }
}

/// What is at a database path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileState {
    /// No file (or an empty one).
    Missing,
    /// A plain SQLite database, as versions before encryption wrote.
    Plain,
    /// Anything else: an encrypted database.
    Encrypted,
}

/// Looks at the first bytes of the file at `path`: a plain SQLite database
/// starts with a fixed text, an encrypted one with noise.
pub fn file_state(path: impl AsRef<Path>) -> std::io::Result<FileState> {
    use std::io::Read;
    let mut header = [0u8; 16];
    let mut file = match std::fs::File::open(path.as_ref()) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FileState::Missing)
        }
        Err(error) => return Err(error),
    };
    let mut read = 0;
    while read < header.len() {
        match file.read(&mut header[read..])? {
            0 => break,
            n => read += n,
        }
    }
    Ok(match read {
        0 => FileState::Missing,
        16 if &header == b"SQLite format 3\0" => FileState::Plain,
        _ => FileState::Encrypted,
    })
}

/// Opens a connection and, with a key, unlocks it. The key is the first
/// thing the connection hears, as SQLCipher requires.
fn connect(path: &Path, key: Option<&StoreKey>) -> StoreResult<Connection> {
    let conn = Connection::open(path)?;
    if let Some(key) = key {
        conn.execute_batch(&format!("PRAGMA key = {};", key.literal()))?;
    }
    // The first real read is where a wrong key shows.
    match conn.query_row("SELECT count(*) FROM sqlite_master", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(_) => Ok(conn),
        Err(rusqlite::Error::SqliteFailure(error, _))
            if error.code == rusqlite::ErrorCode::NotADatabase =>
        {
            Err(StoreError::WrongKey)
        }
        Err(error) => Err(error.into()),
    }
}

/// Shorthand for results of store operations.
pub type StoreResult<T> = Result<T, StoreError>;

/// What changed in the store. Views re-query the part they show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreChange {
    /// Too much changed to enumerate (or the listener fell behind):
    /// re-query everything.
    Everything,
    /// The account list or an account's connection state.
    Accounts,
    /// The chat list of an account: a chat appeared, moved, or its preview
    /// or unread count changed.
    Chats {
        /// The account whose chats changed.
        account_id: AccountId,
    },
    /// The messages of one chat.
    Messages {
        /// The account.
        account_id: AccountId,
        /// The chat whose messages changed.
        chat_id: ChatId,
    },
    /// Somebody started or stopped typing in a chat.
    Presence {
        /// The account.
        account_id: AccountId,
        /// The chat.
        chat_id: ChatId,
    },
    /// A chat's picture arrived, changed, or turned out not to exist.
    Avatar {
        /// The account.
        account_id: AccountId,
        /// The chat (for a direct chat, the contact) the picture is of.
        subject: ChatId,
    },
    /// The address book of an account changed.
    Contacts {
        /// The account.
        account_id: AccountId,
    },
    /// A media payload was cached, or could not be.
    Media {
        /// The cache key (see [`Store::media`]).
        key: String,
    },
    /// A group's details or participants, the blocklist, or a profile
    /// changed. Read from the store by whoever shows them.
    Social {
        /// The account.
        account_id: AccountId,
    },
    /// The sticker and GIF library changed: an item came, went, was
    /// starred or used.
    Library,
    /// A story, who saw it, who is muted, the audience, or a story on its
    /// way out, changed. Read from the store by whoever shows them.
    Stories {
        /// The account.
        account_id: AccountId,
    },
    /// Something the user asked for did not happen, in words for them: a
    /// pin the provider refused, say. Shown once; nothing to re-query.
    Problem {
        /// What to tell the user.
        message: String,
    },
}

/// What the store knows about a chat's picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredAvatar {
    /// The provider's id of the picture. `None`: there is none.
    pub picture_id: Option<String>,
    /// The picture, ready to show. `None`: there is none.
    pub image: Option<Vec<u8>>,
    /// When the provider was last asked.
    pub checked_at: Timestamp,
}

/// A cached media payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedMedia {
    /// The bytes.
    pub bytes: Vec<u8>,
    /// MIME type.
    pub mime: Option<String>,
    /// Pixel size, for images.
    pub size: Option<(u32, u32)>,
}

/// A receiver of [`StoreChange`] notifications.
///
/// Notifications are hints: they say where to look, not what the new data
/// is. A listener that falls behind is told to refresh
/// [`Everything`](StoreChange::Everything) instead of receiving stale hints.
pub struct ChangeListener {
    receiver: broadcast::Receiver<StoreChange>,
}

impl ChangeListener {
    /// Waits for the next change. Returns `None` once the store is gone.
    ///
    /// Runtime-agnostic: it can be awaited from any executor, including a
    /// UI framework's own.
    pub async fn next(&mut self) -> Option<StoreChange> {
        match self.receiver.recv().await {
            Ok(change) => Some(change),
            Err(broadcast::error::RecvError::Lagged(_)) => Some(StoreChange::Everything),
            Err(broadcast::error::RecvError::Closed) => None,
        }
    }

    /// Returns a change that is already waiting, without blocking. Lets a
    /// view drain a burst and repaint once.
    pub fn try_next(&mut self) -> Option<StoreChange> {
        match self.receiver.try_recv() {
            Ok(change) => Some(change),
            Err(broadcast::error::TryRecvError::Lagged(_)) => Some(StoreChange::Everything),
            Err(_) => None,
        }
    }
}

/// The last message of a chat, reduced to what a chat list row shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessagePreview {
    /// One line describing the message ("Photo", the text...).
    pub text: String,
    /// Sent by the account.
    pub outgoing: bool,
    /// Delivery status, for the tick next to an outgoing preview.
    pub status: DeliveryStatus,
    /// Author's name, shown as a prefix in groups.
    pub sender_name: Option<String>,
    /// When it was sent.
    pub timestamp: Timestamp,
}

/// A chat as the chat list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatSummary {
    /// Chat id.
    pub id: ChatId,
    /// Owning account.
    pub account_id: AccountId,
    /// 1:1 or group.
    pub kind: ChatKind,
    /// Display name.
    pub title: String,
    /// Picture handle.
    pub avatar: Option<MediaRef>,
    /// Unread messages.
    pub unread_count: u32,
    /// Pinned to the top.
    pub pinned: bool,
    /// Notifications silenced.
    pub muted: bool,
    /// Archived.
    pub archived: bool,
    /// The newest message, if any is stored.
    pub last_message: Option<MessagePreview>,
}

/// One person who reacted to a message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reactor {
    /// Who it was, under the id their reaction came with.
    pub sender: ContactId,
    /// It is the account itself.
    pub from_me: bool,
}

/// Everyone who reacted to a message with the same emoji.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionSummary {
    /// The emoji.
    pub emoji: String,
    /// How many people used it.
    pub count: u32,
    /// The account's own reaction is among them.
    pub from_me: bool,
    /// Who used it, the first to react first.
    pub by: Vec<Reactor>,
}

/// A message together with what the store knows around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredMessage {
    /// The message. Its `reply_to` snapshot is filled in from the local
    /// copy of the quoted message when the provider did not send one.
    pub message: Message,
    /// Reactions, most used first.
    pub reactions: Vec<ReactionSummary>,
}

/// A full-text search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchHit {
    /// The matching message.
    pub message: Message,
    /// Title of the chat it is in.
    pub chat_title: String,
}

/// How much of a chat's history is in the store.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HistoryState {
    /// The newest page has been fetched at least once.
    pub loaded: bool,
    /// There is nothing older left to fetch.
    pub complete: bool,
    /// Where the next older page starts.
    pub cursor: Option<Cursor>,
}

/// What an upsert did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upsert {
    /// The row did not exist.
    Inserted,
    /// The row existed and changed.
    Updated,
    /// The row existed and already had these values.
    Unchanged,
}

/// The local database.
///
/// Thread-safe. Reads use their own connection (WAL mode), so a view
/// querying the store never waits behind the sync engine's writes.
pub struct Store {
    writer: Mutex<Connection>,
    /// `None` for in-memory databases, which cannot be opened twice.
    reader: Option<Mutex<Connection>>,
    changes: broadcast::Sender<StoreChange>,
}

/// The columns of a message, as read. The sender's name is the one the
/// account saved them under when the address book has it (a group shows
/// "Ana", not the name Ana gave herself), else the name the message came
/// with. Resolved here, from the store, so it works offline and follows
/// the address book as it changes.
const MESSAGE_COLUMNS: &str = "m.account_id, m.chat_id, m.id, m.client_id, m.sender, \
     m.sender_name, m.outgoing, m.ts, m.content, m.reply_to, m.status, m.status_reason, \
     m.edited, m.deleted, m.extras";

/// How many columns [`MESSAGE_COLUMNS`] selects.
const MESSAGE_COLUMN_COUNT: usize = 15;

impl Store {
    /// Opens (creating and migrating as needed) the database at `path`.
    ///
    /// With a `key` the file is encrypted (SQLCipher) and only that key
    /// opens it again; [`StoreError::WrongKey`] says it did not. Without
    /// one it is a plain SQLite file, which the application only uses to
    /// read what earlier versions left (see
    /// [`encrypt_plain`](Self::encrypt_plain)).
    pub fn open(path: impl AsRef<Path>, key: Option<&StoreKey>) -> StoreResult<Self> {
        let mut writer = connect(path.as_ref(), key)?;
        writer.pragma_update(None, "journal_mode", "WAL")?;
        writer.pragma_update(None, "synchronous", "NORMAL")?;
        writer.pragma_update(None, "foreign_keys", true)?;
        writer.busy_timeout(std::time::Duration::from_secs(5))?;
        migrations::migrate(&mut writer)?;

        let reader = connect(path.as_ref(), key)?;
        reader.pragma_update(None, "query_only", true)?;
        reader.busy_timeout(std::time::Duration::from_secs(5))?;

        Ok(Self::with_connections(writer, Some(reader)))
    }

    /// The schema version of the database at `path`, read without
    /// migrating or changing it. A build asks this before it opens a
    /// database, to say plainly that the file is from a newer build
    /// instead of failing on it (see [`StoreError::NewerSchema`]).
    pub fn schema_of(path: impl AsRef<Path>, key: Option<&StoreKey>) -> StoreResult<u32> {
        let conn = connect(path.as_ref(), key)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
    }

    /// Marks the database at `path` as written by a build at schema
    /// `version`: what a newer build leaves behind, for the tests of what
    /// an older one does with it.
    #[cfg(any(test, feature = "fixtures"))]
    pub fn pretend_schema(
        path: impl AsRef<Path>,
        key: Option<&StoreKey>,
        version: u32,
    ) -> StoreResult<()> {
        let conn = connect(path.as_ref(), key)?;
        conn.pragma_update(None, "user_version", version)?;
        Ok(())
    }

    /// True when `key` opens the database at `path`.
    pub fn key_opens(path: impl AsRef<Path>, key: &StoreKey) -> bool {
        connect(path.as_ref(), Some(key)).is_ok()
    }

    /// Turns the plain database at `path` into an encrypted one under
    /// `key`, in place.
    ///
    /// The encrypted copy is written next to the original
    /// (`sqlcipher_export`), checked, and only then moved over it; the
    /// plain file's `-wal` and `-shm` companions are removed. If anything
    /// fails before the move, the copy is deleted and the plain database
    /// is exactly as it was.
    pub fn encrypt_plain(path: impl AsRef<Path>, key: &StoreKey) -> StoreResult<()> {
        let path = path.as_ref();
        let beside = |suffix: &str| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            std::path::PathBuf::from(name)
        };
        let copy = beside(".encrypting");
        let export = || -> StoreResult<()> {
            if copy.exists() {
                std::fs::remove_file(&copy)?;
            }
            let plain = connect(path, None)?;
            plain.busy_timeout(std::time::Duration::from_secs(5))?;
            // Everything into the main file, and no journal files left
            // behind when this connection closes.
            plain.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
            plain.query_row("PRAGMA journal_mode = DELETE", [], |_| Ok(()))?;
            let version: u32 = plain.query_row("PRAGMA user_version", [], |row| row.get(0))?;
            let rows = |conn: &Connection, schema: &str| -> StoreResult<i64> {
                Ok(conn.query_row(
                    &format!("SELECT count(*) FROM {schema}.sqlite_master"),
                    [],
                    |row| row.get(0),
                )?)
            };
            plain.execute(
                &format!("ATTACH DATABASE ?1 AS encrypted KEY {}", key.literal()),
                [copy.to_string_lossy().as_ref()],
            )?;
            plain.query_row("SELECT sqlcipher_export('encrypted')", [], |_| Ok(()))?;
            plain.execute_batch(&format!("PRAGMA encrypted.user_version = {version};"))?;
            let complete = rows(&plain, "main")? == rows(&plain, "encrypted")?;
            plain.execute_batch("DETACH DATABASE encrypted;")?;
            drop(plain);
            // Not trusted until it has been opened with the key.
            let check = connect(&copy, Some(key))?;
            let same_version =
                check.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))? == version;
            if !(complete && same_version) {
                return Err(StoreError::WrongKey);
            }
            Ok(())
        };
        if let Err(error) = export() {
            let _ = std::fs::remove_file(&copy);
            return Err(error);
        }
        std::fs::rename(&copy, path)?;
        for stale in [beside("-wal"), beside("-shm")] {
            let _ = std::fs::remove_file(stale);
        }
        Ok(())
    }

    /// Opens a private in-memory database. For tests and throwaway
    /// sessions. Nothing reaches the disk, so there is nothing to encrypt.
    pub fn open_in_memory() -> StoreResult<Self> {
        let mut writer = Connection::open_in_memory()?;
        migrations::migrate(&mut writer)?;
        Ok(Self::with_connections(writer, None))
    }

    fn with_connections(writer: Connection, reader: Option<Connection>) -> Self {
        let (changes, _) = broadcast::channel(256);
        Self {
            writer: Mutex::new(writer),
            reader: reader.map(Mutex::new),
            changes,
        }
    }

    /// Subscribes to change notifications.
    pub fn subscribe(&self) -> ChangeListener {
        ChangeListener {
            receiver: self.changes.subscribe(),
        }
    }

    /// Announces a change. Having no listener is fine.
    pub fn notify(&self, change: StoreChange) {
        let _ = self.changes.send(change);
    }

    fn lock(conn: &Mutex<Connection>) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock cannot leave SQLite inconsistent
        // (the transaction is rolled back), so poisoning is not fatal.
        conn.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub(crate) fn read<T>(&self, f: impl FnOnce(&Connection) -> StoreResult<T>) -> StoreResult<T> {
        let conn = Self::lock(self.reader.as_ref().unwrap_or(&self.writer));
        f(&conn)
    }

    pub(crate) fn write<T>(
        &self,
        f: impl FnOnce(&Transaction<'_>) -> StoreResult<T>,
    ) -> StoreResult<T> {
        let mut conn = Self::lock(&self.writer);
        let tx = conn.transaction()?;
        let value = f(&tx)?;
        tx.commit()?;
        Ok(value)
    }

    // ----- pictures and media -----------------------------------------

    /// What is known about a chat's picture, or `None` if the provider
    /// was never asked.
    pub fn avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
    ) -> StoreResult<Option<StoredAvatar>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT picture_id, image, checked_at FROM avatars
                     WHERE account_id = ?1 AND subject = ?2",
                    params![account.as_str(), subject.as_str()],
                    |row| {
                        Ok(StoredAvatar {
                            picture_id: row.get(0)?,
                            image: row.get(1)?,
                            checked_at: Timestamp::from_millis(row.get(2)?),
                        })
                    },
                )
                .optional()?)
        })
    }

    /// Records a chat's picture (`None`: it has none) and when that was
    /// learned.
    pub fn put_avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
        picture: Option<(&str, &[u8])>,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO avatars (account_id, subject, picture_id, image, checked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (account_id, subject) DO UPDATE SET
                    picture_id = excluded.picture_id,
                    image = excluded.image,
                    checked_at = excluded.checked_at",
                params![
                    account.as_str(),
                    subject.as_str(),
                    picture.map(|(id, _)| id),
                    picture.map(|(_, image)| image),
                    now.as_millis(),
                ],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Avatar {
            account_id: account.clone(),
            subject: subject.clone(),
        });
        Ok(())
    }

    /// The id of a chat's picture as the provider's chat list last gave
    /// it. `None`: no picture known, or the list does not say.
    pub fn chat_picture_id(
        &self,
        account: &AccountId,
        chat: &ChatId,
    ) -> StoreResult<Option<String>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT picture_id FROM chats WHERE account_id = ?1 AND id = ?2",
                    params![account.as_str(), chat.as_str()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })
    }

    /// Notes that a chat's picture was checked and is still the same.
    pub fn touch_avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE avatars SET checked_at = ?3 WHERE account_id = ?1 AND subject = ?2",
                params![account.as_str(), subject.as_str(), now.as_millis()],
            )?;
            Ok(())
        })
    }

    /// A cached media payload, by key. Reading it counts as using it, for
    /// the eviction order.
    pub fn media(&self, key: &str, now: Timestamp) -> StoreResult<Option<CachedMedia>> {
        let found = self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT bytes, mime, width, height FROM media_cache WHERE key = ?1",
                    params![key],
                    |row| {
                        let (width, height): (Option<u32>, Option<u32>) =
                            (row.get(2)?, row.get(3)?);
                        Ok(CachedMedia {
                            bytes: row.get(0)?,
                            mime: row.get(1)?,
                            size: width.zip(height),
                        })
                    },
                )
                .optional()?)
        })?;
        if found.is_some() {
            self.write(|tx| {
                tx.execute(
                    "UPDATE media_cache SET last_used = ?2 WHERE key = ?1",
                    params![key, now.as_millis()],
                )?;
                Ok(())
            })?;
        }
        Ok(found)
    }

    /// The pixel size of a cached image, without reading it.
    pub fn media_size(&self, key: &str) -> StoreResult<Option<(u32, u32)>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT width, height FROM media_cache WHERE key = ?1",
                    params![key],
                    |row| Ok((row.get::<_, Option<u32>>(0)?, row.get::<_, Option<u32>>(1)?)),
                )
                .optional()?
                .and_then(|(width, height)| width.zip(height)))
        })
    }

    /// Caches a media payload and then makes room: while the cache holds
    /// more than `budget` bytes, the entries used longest ago are dropped
    /// (never the one just stored). Returns how many were dropped.
    pub fn put_media(
        &self,
        key: &str,
        media: &CachedMedia,
        now: Timestamp,
        budget: u64,
    ) -> StoreResult<usize> {
        let evicted = self.write(|tx| {
            tx.execute(
                "INSERT INTO media_cache (key, bytes, mime, width, height, size, last_used)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT (key) DO UPDATE SET
                    bytes = excluded.bytes, mime = excluded.mime, width = excluded.width,
                    height = excluded.height, size = excluded.size,
                    last_used = excluded.last_used",
                params![
                    key,
                    media.bytes,
                    media.mime,
                    media.size.map(|(width, _)| width),
                    media.size.map(|(_, height)| height),
                    media.bytes.len() as i64,
                    now.as_millis(),
                ],
            )?;
            let mut total: i64 = tx.query_row(
                "SELECT COALESCE(SUM(size), 0) FROM media_cache",
                [],
                |row| row.get(0),
            )?;
            // A budget larger than SQLite counts to is no budget.
            let budget = i64::try_from(budget).unwrap_or(i64::MAX);
            let mut evicted = 0;
            while total > budget {
                let oldest: Option<(String, i64)> = tx
                    .query_row(
                        "SELECT key, size FROM media_cache WHERE key <> ?1
                         ORDER BY last_used, key LIMIT 1",
                        params![key],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((oldest, size)) = oldest else { break };
                tx.execute("DELETE FROM media_cache WHERE key = ?1", params![oldest])?;
                total -= size;
                evicted += 1;
            }
            Ok(evicted)
        })?;
        self.notify(StoreChange::Media {
            key: key.to_owned(),
        });
        Ok(evicted)
    }

    /// How many bytes the media cache holds.
    pub fn media_total(&self) -> StoreResult<u64> {
        self.read(|conn| {
            let total: i64 = conn.query_row(
                "SELECT COALESCE(SUM(size), 0) FROM media_cache",
                [],
                |row| row.get(0),
            )?;
            Ok(total.max(0) as u64)
        })
    }

    // ----- accounts ---------------------------------------------------

    /// Stores the accounts a provider returned, in the order given.
    pub fn upsert_accounts(&self, provider: &str, accounts: &[Account]) -> StoreResult<()> {
        self.write(|tx| {
            for (position, account) in accounts.iter().enumerate() {
                tx.execute(
                    "INSERT INTO accounts
                        (id, provider, display_name, phone, self_contact, connection, position,
                         settings)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT (id) DO UPDATE SET
                        provider = excluded.provider,
                        display_name = excluded.display_name,
                        phone = excluded.phone,
                        self_contact = excluded.self_contact,
                        connection = excluded.connection,
                        position = excluded.position,
                        settings = excluded.settings",
                    params![
                        account.id.as_str(),
                        provider,
                        account.display_name,
                        account.phone,
                        account.self_contact.as_ref().map(ContactId::as_str),
                        serde_json::to_string(&account.connection)?,
                        position as i64,
                        serde_json::to_string(&account.settings)?,
                    ],
                )?;
            }
            Ok(())
        })?;
        self.notify(StoreChange::Accounts);
        Ok(())
    }

    /// Stores one account: a new one goes after the others, a known one
    /// keeps its place.
    pub fn upsert_account(&self, provider: &str, account: &Account) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO accounts
                    (id, provider, display_name, phone, self_contact, connection, position,
                     settings)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6,
                         (SELECT COALESCE(MAX(position), -1) + 1 FROM accounts), ?7)
                 ON CONFLICT (id) DO UPDATE SET
                    provider = excluded.provider,
                    display_name = excluded.display_name,
                    phone = excluded.phone,
                    self_contact = excluded.self_contact,
                    connection = excluded.connection,
                    settings = excluded.settings",
                params![
                    account.id.as_str(),
                    provider,
                    account.display_name,
                    account.phone,
                    account.self_contact.as_ref().map(ContactId::as_str),
                    serde_json::to_string(&account.connection)?,
                    serde_json::to_string(&account.settings)?,
                ],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Accounts);
        Ok(())
    }

    /// Forgets an account and everything stored for it: its chats, its
    /// messages, its pictures and what was waiting to be sent from it.
    pub fn remove_account(&self, account: &AccountId) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM outbox_files WHERE client_id IN
                    (SELECT client_id FROM outbox WHERE account_id = ?1)",
                params![account.as_str()],
            )?;
            for table in [
                "outbox",
                "reactions",
                "contacts",
                "messages",
                "chats",
                "avatars",
                "groups",
                "group_participants",
                "group_join_requests",
                "blocked_contacts",
                "contact_profiles",
            ] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE account_id = ?1"),
                    params![account.as_str()],
                )?;
            }
            tx.execute(
                "DELETE FROM own_profiles WHERE account_id = ?1",
                params![account.as_str()],
            )?;
            stories::remove_account_tx(tx, account.as_str())?;
            tx.execute(
                "DELETE FROM accounts WHERE id = ?1",
                params![account.as_str()],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Accounts);
        self.notify(StoreChange::Chats {
            account_id: account.clone(),
        });
        Ok(())
    }

    /// Records an account's new connection state.
    pub fn set_connection(&self, account: &AccountId, state: &ConnectionState) -> StoreResult<()> {
        let changed = self.write(|tx| {
            Ok(tx.execute(
                "UPDATE accounts SET connection = ?2 WHERE id = ?1 AND connection <> ?2",
                params![account.as_str(), serde_json::to_string(state)?],
            )?)
        })?;
        if changed > 0 {
            self.notify(StoreChange::Accounts);
        }
        Ok(())
    }

    /// Every account, in the provider's order.
    pub fn accounts(&self) -> StoreResult<Vec<Account>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, display_name, phone, self_contact, connection, settings
                 FROM accounts ORDER BY position, id",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })?;
            let mut accounts = Vec::new();
            for row in rows {
                let (id, display_name, phone, self_contact, connection, settings) = row?;
                accounts.push(Account {
                    id: AccountId::new(id),
                    display_name,
                    phone,
                    self_contact: self_contact.map(ContactId::new),
                    connection: serde_json::from_str(&connection)?,
                    // Written by an older build, or in a shape this one
                    // does not know: the next account listing fills it.
                    settings: settings
                        .and_then(|text| serde_json::from_str(&text).ok())
                        .unwrap_or_default(),
                });
            }
            Ok(accounts)
        })
    }

    // ----- chats ------------------------------------------------------

    /// Stores a chat and, if it carries one, its last message.
    ///
    /// With `keep_local_unread` the stored unread count survives the update;
    /// the engine uses it for providers that cannot report one.
    ///
    /// Returns whether the chat's last message was new to the store, which
    /// tells the engine that this chat has history worth fetching.
    pub fn upsert_chat(&self, chat: &Chat, keep_local_unread: bool) -> StoreResult<bool> {
        let (last_was_new, picture_gone) = self.write(|tx| {
            // A picture the provider said was there and now says is not:
            // the copy held here goes with it.
            let had_picture = !chat.unknown.picture
                && chat.picture_id.is_none()
                && tx
                    .query_row(
                        "SELECT picture_id IS NOT NULL FROM chats
                         WHERE account_id = ?1 AND id = ?2",
                        params![chat.account_id.as_str(), chat.id.as_str()],
                        |row| row.get::<_, bool>(0),
                    )
                    .optional()?
                    .unwrap_or(false);
            if had_picture {
                tx.execute(
                    "UPDATE avatars SET picture_id = NULL, image = NULL, checked_at = ?3
                     WHERE account_id = ?1 AND subject = ?2",
                    params![
                        chat.account_id.as_str(),
                        chat.id.as_str(),
                        Timestamp::now().as_millis()
                    ],
                )?;
            }
            tx.execute(
                "INSERT INTO chats
                    (account_id, id, kind, title, avatar, unread_count, pinned, muted, archived,
                     picture_id, pinned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?15, ?17)
                 ON CONFLICT (account_id, id) DO UPDATE SET
                    kind = excluded.kind,
                    title = excluded.title,
                    avatar = excluded.avatar,
                    unread_count = CASE WHEN ?10 OR ?16 THEN unread_count
                                        ELSE excluded.unread_count END,
                    -- A pin whose time the provider does not say keeps the
                    -- time it has here (the pin was made from here). The
                    -- bare column names are the row as it was.
                    pinned_at = CASE WHEN ?11 THEN pinned_at
                                     WHEN excluded.pinned AND excluded.pinned_at IS NULL
                                         THEN CASE WHEN pinned THEN pinned_at END
                                     ELSE excluded.pinned_at END,
                    pinned = CASE WHEN ?11 THEN pinned ELSE excluded.pinned END,
                    muted = CASE WHEN ?12 THEN muted ELSE excluded.muted END,
                    archived = CASE WHEN ?13 THEN archived ELSE excluded.archived END,
                    picture_id = CASE WHEN ?14 THEN picture_id ELSE excluded.picture_id END",
                params![
                    chat.account_id.as_str(),
                    chat.id.as_str(),
                    kind_to_str(chat.kind),
                    chat.title,
                    chat.avatar.as_ref().map(MediaRef::as_str),
                    chat.unread_count,
                    chat.pinned,
                    chat.muted,
                    chat.archived,
                    keep_local_unread,
                    // What the provider does not know keeps the value the
                    // store has: a pin the user just made is not undone by
                    // a list that has nothing to say about pins.
                    chat.unknown.pinned,
                    chat.unknown.muted,
                    chat.unknown.archived,
                    chat.unknown.picture,
                    chat.picture_id,
                    // A count the provider does not know is not a zero.
                    chat.unknown.unread,
                    chat.pinned_at
                        .filter(|_| chat.pinned)
                        .map(Timestamp::as_millis),
                ],
            )?;
            let last_was_new = match &chat.last_message {
                Some(message) => upsert_message_tx(tx, message)? == Upsert::Inserted,
                None => false,
            };
            Ok((last_was_new, had_picture))
        })?;
        if picture_gone {
            self.notify(StoreChange::Avatar {
                account_id: chat.account_id.clone(),
                subject: chat.id.clone(),
            });
        }
        self.notify(StoreChange::Chats {
            account_id: chat.account_id.clone(),
        });
        if last_was_new {
            self.notify(StoreChange::Messages {
                account_id: chat.account_id.clone(),
                chat_id: chat.id.clone(),
            });
        }
        Ok(last_was_new)
    }

    /// The chats of an account as the chat list shows them: pinned first
    /// (the last one pinned on top, those without a known time after),
    /// then most recently active. `filter` keeps chats whose title contains
    /// it (case-insensitive).
    pub fn chats(
        &self,
        account: &AccountId,
        filter: Option<&str>,
    ) -> StoreResult<Vec<ChatSummary>> {
        let pattern = filter.map(str::trim).filter(|f| !f.is_empty()).map(|f| {
            format!(
                "%{}%",
                f.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });
        self.read(|conn| {
            let sql = format!(
                "SELECT c.id, c.account_id, c.kind, c.title, c.avatar, c.unread_count,
                        c.pinned, c.muted, c.archived, {MESSAGE_COLUMNS}
                 FROM chats c
                 LEFT JOIN messages m ON m.pk = (
                     SELECT pk FROM messages
                     WHERE account_id = c.account_id AND chat_id = c.id
                     ORDER BY ts DESC, pk DESC LIMIT 1)
                 WHERE c.account_id = ?1 AND c.archived = 0
                   AND (?2 IS NULL OR c.title LIKE ?2 ESCAPE '\\')
                 ORDER BY c.pinned DESC,
                          CASE WHEN c.pinned THEN COALESCE(c.pinned_at, 0) ELSE 0 END DESC,
                          COALESCE(c.last_message_at, 0) DESC, c.title"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut rows = stmt.query(params![account.as_str(), pattern])?;
            let mut chats = Vec::new();
            while let Some(row) = rows.next()? {
                chats.push(chat_summary_from_row(conn, row)?);
            }
            Ok(chats)
        })
    }

    /// The archived chats of an account, most recently active first.
    pub fn archived_chats(&self, account: &AccountId) -> StoreResult<Vec<ChatSummary>> {
        self.read(|conn| {
            let sql = format!(
                "SELECT c.id, c.account_id, c.kind, c.title, c.avatar, c.unread_count,
                        c.pinned, c.muted, c.archived, {MESSAGE_COLUMNS}
                 FROM chats c
                 LEFT JOIN messages m ON m.pk = (
                     SELECT pk FROM messages
                     WHERE account_id = c.account_id AND chat_id = c.id
                     ORDER BY ts DESC, pk DESC LIMIT 1)
                 WHERE c.account_id = ?1 AND c.archived = 1
                 ORDER BY COALESCE(c.last_message_at, 0) DESC, c.title"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut rows = stmt.query(params![account.as_str()])?;
            let mut chats = Vec::new();
            while let Some(row) = rows.next()? {
                chats.push(chat_summary_from_row(conn, row)?);
            }
            Ok(chats)
        })
    }

    /// Applies a change of state to a chat: what the user just asked for,
    /// shown before the provider has answered.
    pub fn apply_chat_change(
        &self,
        account: &AccountId,
        chat: &ChatId,
        change: ChatChange,
    ) -> StoreResult<()> {
        let (sql, value) = match change {
            // Pinned now: the last one pinned is the first of the pinned.
            ChatChange::Pinned(true) => (
                "UPDATE chats SET pinned_at = CASE WHEN pinned THEN pinned_at ELSE ?4 END,
                                  pinned = ?3",
                true,
            ),
            ChatChange::Pinned(false) => ("UPDATE chats SET pinned = ?3, pinned_at = NULL", false),
            ChatChange::Muted(on) => ("UPDATE chats SET muted = ?3", on),
            ChatChange::MutedFor(_) => ("UPDATE chats SET muted = ?3", true),
            ChatChange::Archived(on) => ("UPDATE chats SET archived = ?3", on),
            // A mark has no count of its own: one is the least it can be.
            ChatChange::MarkedUnread => (
                "UPDATE chats SET unread_count = MAX(unread_count, ?3)",
                true,
            ),
        };
        let changed = self.write(|tx| {
            Ok(tx.execute(
                // `?4` (now) is named by every statement, whether it has a
                // use for it or not: a parameter must be.
                &format!("{sql} WHERE account_id = ?1 AND id = ?2 AND ?4 IS NOT NULL"),
                params![
                    account.as_str(),
                    chat.as_str(),
                    value,
                    Timestamp::now().as_millis()
                ],
            )?)
        })?;
        if changed > 0 {
            self.notify(StoreChange::Chats {
                account_id: account.clone(),
            });
        }
        Ok(())
    }

    /// One chat, or `None` if the store has never seen it.
    pub fn chat(&self, account: &AccountId, chat: &ChatId) -> StoreResult<Option<ChatSummary>> {
        self.read(|conn| {
            let sql = format!(
                "SELECT c.id, c.account_id, c.kind, c.title, c.avatar, c.unread_count,
                        c.pinned, c.muted, c.archived, {MESSAGE_COLUMNS}
                 FROM chats c
                 LEFT JOIN messages m ON m.pk = (
                     SELECT pk FROM messages
                     WHERE account_id = c.account_id AND chat_id = c.id
                     ORDER BY ts DESC, pk DESC LIMIT 1)
                 WHERE c.account_id = ?1 AND c.id = ?2"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut rows = stmt.query(params![account.as_str(), chat.as_str()])?;
            match rows.next()? {
                Some(row) => Ok(Some(chat_summary_from_row(conn, row)?)),
                None => Ok(None),
            }
        })
    }

    /// Sets a chat's unread count to zero.
    pub fn mark_chat_read(&self, account: &AccountId, chat: &ChatId) -> StoreResult<()> {
        let changed = self.write(|tx| {
            Ok(tx.execute(
                "UPDATE chats SET unread_count = 0
                 WHERE account_id = ?1 AND id = ?2 AND unread_count <> 0",
                params![account.as_str(), chat.as_str()],
            )?)
        })?;
        if changed > 0 {
            self.notify(StoreChange::Chats {
                account_id: account.clone(),
            });
        }
        Ok(())
    }

    /// Adds one to a chat's unread count. Used when the provider cannot
    /// report unread counts itself.
    pub fn bump_unread(&self, account: &AccountId, chat: &ChatId) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE chats SET unread_count = unread_count + 1
                 WHERE account_id = ?1 AND id = ?2",
                params![account.as_str(), chat.as_str()],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Chats {
            account_id: account.clone(),
        });
        Ok(())
    }

    /// The chats with messages of the account's own, newer than `since`,
    /// that the provider has and nobody is known to have read yet: their
    /// ticks may still move. Newest first, at most `limit`.
    pub fn chats_awaiting_receipts(
        &self,
        account: &AccountId,
        since: Timestamp,
        limit: usize,
    ) -> StoreResult<Vec<ChatId>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT chat_id, MAX(ts) AS newest FROM messages
                 WHERE account_id = ?1 AND outgoing = 1 AND ts > ?2
                   AND status IN ('accepted', 'sent', 'delivered')
                   AND id NOT LIKE 'local:%' AND deleted = 0
                 GROUP BY chat_id ORDER BY newest DESC LIMIT ?3",
            )?;
            let rows = stmt.query_map(
                params![account.as_str(), since.as_millis(), limit as i64],
                |row| Ok(ChatId::new(row.get::<_, String>(0)?)),
            )?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// How much of a chat's history has been fetched.
    pub fn history_state(&self, account: &AccountId, chat: &ChatId) -> StoreResult<HistoryState> {
        self.read(|conn| {
            let state = conn
                .query_row(
                    "SELECT history_loaded, history_complete, history_cursor
                     FROM chats WHERE account_id = ?1 AND id = ?2",
                    params![account.as_str(), chat.as_str()],
                    |row| {
                        Ok(HistoryState {
                            loaded: row.get(0)?,
                            complete: row.get(1)?,
                            cursor: row.get::<_, Option<String>>(2)?.map(Cursor::new),
                        })
                    },
                )
                .optional()?;
            Ok(state.unwrap_or_default())
        })
    }

    /// Records how much of a chat's history has been fetched.
    pub fn set_history_state(
        &self,
        account: &AccountId,
        chat: &ChatId,
        state: &HistoryState,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE chats SET history_loaded = ?3, history_complete = ?4, history_cursor = ?5
                 WHERE account_id = ?1 AND id = ?2",
                params![
                    account.as_str(),
                    chat.as_str(),
                    state.loaded,
                    state.complete,
                    state.cursor.as_ref().map(Cursor::as_str),
                ],
            )?;
            Ok(())
        })
    }

    /// Forgets how far the history of an account's chats was read, so
    /// that each is read again from its newest page: for when messages
    /// older than the stored ones may have appeared (history imported
    /// from the phone after a new link).
    pub fn forget_history_progress(&self, account: &AccountId) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE chats
                 SET history_loaded = 0, history_complete = 0, history_cursor = NULL
                 WHERE account_id = ?1",
                params![account.as_str()],
            )?;
            Ok(())
        })
    }

    // ----- contacts ---------------------------------------------------

    /// Stores what an event said about a contact. An event may know less
    /// than the store does, so what it leaves out is kept.
    pub fn upsert_contact(&self, contact: &Contact) -> StoreResult<()> {
        self.write(|tx| upsert_contact_tx(tx, contact, None))?;
        self.notify(StoreChange::Contacts {
            account_id: contact.account_id.clone(),
        });
        Ok(())
    }

    /// Stores a page of the provider's address book. The list is the
    /// truth for names and numbers: a name that was removed there is
    /// removed here. `listed_at` marks the pass the page belongs to, for
    /// [`prune_contacts`](Self::prune_contacts).
    pub fn upsert_listed_contacts(
        &self,
        account: &AccountId,
        contacts: &[Contact],
        listed_at: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            for contact in contacts {
                upsert_contact_tx(tx, contact, Some(listed_at))?;
            }
            Ok(())
        })?;
        if !contacts.is_empty() {
            self.notify(StoreChange::Contacts {
                account_id: account.clone(),
            });
        }
        Ok(())
    }

    /// Removes the contacts a complete pass over the provider's address
    /// book did not find: they were deleted on the phone. Contacts that
    /// only ever came from events are kept. Returns how many went.
    pub fn prune_contacts(&self, account: &AccountId, pass: Timestamp) -> StoreResult<usize> {
        let removed = self.write(|tx| {
            Ok(tx.execute(
                "DELETE FROM contacts
                 WHERE account_id = ?1 AND listed_at IS NOT NULL AND listed_at < ?2",
                params![account.as_str(), pass.as_millis()],
            )?)
        })?;
        if removed > 0 {
            self.notify(StoreChange::Contacts {
                account_id: account.clone(),
            });
        }
        Ok(removed)
    }

    /// The address book of an account, by name. `query` keeps the
    /// contacts whose name, username or number contains it (the number
    /// compared by its digits).
    pub fn contacts(
        &self,
        account: &AccountId,
        query: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<Contact>> {
        let typed = query.map(str::trim).filter(|q| !q.is_empty());
        let escape = |text: &str| {
            format!(
                "%{}%",
                text.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        };
        let pattern = typed.map(|q| escape(&q.to_lowercase()));
        let digits = typed
            .map(|q| q.chars().filter(char::is_ascii_digit).collect::<String>())
            .filter(|digits| digits.len() >= 3)
            .map(|digits| escape(&digits));
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT id, name, phone, avatar, saved_name, profile_name, business_name,
                        username, about, picture_id
                 FROM contacts
                 WHERE account_id = ?1
                   AND (?2 IS NULL
                        OR sort_name LIKE ?2 ESCAPE '\\'
                        OR lower(COALESCE(username, '')) LIKE ?2 ESCAPE '\\'
                        OR (?3 IS NOT NULL
                            AND COALESCE(phone, id) LIKE ?3 ESCAPE '\\'))
                 ORDER BY sort_name, id
                 LIMIT ?4",
            )?;
            let mut rows = stmt.query(params![account.as_str(), pattern, digits, limit as i64])?;
            let mut contacts = Vec::new();
            while let Some(row) = rows.next()? {
                contacts.push(Contact {
                    name: row.get(1)?,
                    phone: row.get(2)?,
                    avatar: row.get::<_, Option<String>>(3)?.map(MediaRef::new),
                    saved_name: row.get(4)?,
                    profile_name: row.get(5)?,
                    business_name: row.get(6)?,
                    username: row.get(7)?,
                    about: row.get(8)?,
                    picture_id: row.get(9)?,
                    ..Contact::new(account.clone(), ContactId::new(row.get::<_, String>(0)?))
                });
            }
            Ok(contacts)
        })
    }

    /// How many contacts an account's address book holds here.
    pub fn contact_count(&self, account: &AccountId) -> StoreResult<usize> {
        self.read(|conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM contacts WHERE account_id = ?1",
                params![account.as_str()],
                |row| row.get::<_, i64>(0),
            )? as usize)
        })
    }

    /// A stored contact.
    pub fn contact(&self, account: &AccountId, id: &ContactId) -> StoreResult<Option<Contact>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT name, phone, avatar, saved_name, profile_name, business_name,
                            username, about, picture_id
                     FROM contacts WHERE account_id = ?1 AND id = ?2",
                    params![account.as_str(), id.as_str()],
                    |row| {
                        Ok(Contact {
                            name: row.get(0)?,
                            phone: row.get(1)?,
                            avatar: row.get::<_, Option<String>>(2)?.map(MediaRef::new),
                            saved_name: row.get(3)?,
                            profile_name: row.get(4)?,
                            business_name: row.get(5)?,
                            username: row.get(6)?,
                            about: row.get(7)?,
                            picture_id: row.get(8)?,
                            ..Contact::new(account.clone(), id.clone())
                        })
                    },
                )
                .optional()?)
        })
    }

    // ----- messages ---------------------------------------------------

    /// Stores messages (inserting, merging or folding reactions as needed)
    /// and returns how many rows were inserted.
    ///
    /// Merging never moves a delivery status backwards and matches a
    /// message with the pending local copy that has the same client id, so
    /// a message this client sent is never shown twice.
    pub fn upsert_messages(&self, messages: &[Message]) -> StoreResult<usize> {
        if messages.is_empty() {
            return Ok(0);
        }
        let mut touched: Vec<(AccountId, ChatId)> = Vec::new();
        let inserted = self.write(|tx| {
            let mut inserted = 0;
            for message in messages {
                let outcome = upsert_message_tx(tx, message)?;
                if outcome == Upsert::Inserted {
                    inserted += 1;
                }
                if outcome != Upsert::Unchanged {
                    let key = (message.account_id.clone(), message.chat_id.clone());
                    if !touched.contains(&key) {
                        touched.push(key);
                    }
                }
            }
            Ok(inserted)
        })?;
        self.notify_messages(touched);
        Ok(inserted)
    }

    /// Stores one message. See [`upsert_messages`](Self::upsert_messages).
    pub fn upsert_message(&self, message: &Message) -> StoreResult<Upsert> {
        let outcome = self.write(|tx| upsert_message_tx(tx, message))?;
        if outcome != Upsert::Unchanged {
            self.notify_messages(vec![(message.account_id.clone(), message.chat_id.clone())]);
        }
        Ok(outcome)
    }

    pub(crate) fn notify_messages(&self, touched: Vec<(AccountId, ChatId)>) {
        let mut accounts: Vec<AccountId> = Vec::new();
        for (account_id, chat_id) in touched {
            if !accounts.contains(&account_id) {
                accounts.push(account_id.clone());
            }
            self.notify(StoreChange::Messages {
                account_id,
                chat_id,
            });
        }
        // A new or changed message can move its chat and change its preview.
        for account_id in accounts {
            self.notify(StoreChange::Chats { account_id });
        }
    }

    /// Moves a message's delivery status forward. Returns whether it
    /// changed; updates that would move it backwards are ignored.
    pub fn set_message_status(
        &self,
        account: &AccountId,
        message: &MessageId,
        status: &DeliveryStatus,
    ) -> StoreResult<bool> {
        let chat = self.write(|tx| {
            let current = tx
                .query_row(
                    "SELECT pk, chat_id, status, status_reason, client_id FROM messages
                     WHERE account_id = ?1 AND id = ?2",
                    params![account.as_str(), message.as_str()],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                        ))
                    },
                )
                .optional()?;
            let Some((pk, chat_id, current, reason, client_id)) = current else {
                return Ok(None);
            };
            if !status_from_parts(&current, reason).can_advance_to(status) {
                return Ok(None);
            }
            let (name, reason) = status_to_parts(status);
            tx.execute(
                "UPDATE messages SET status = ?2, status_reason = ?3 WHERE pk = ?1",
                params![pk, name, reason],
            )?;
            if let Some(client_id) = &client_id {
                sends::status_tx(tx, client_id, status, Timestamp::now())?;
            }
            Ok(Some(ChatId::new(chat_id)))
        })?;
        match chat {
            Some(chat_id) => {
                self.notify_messages(vec![(account.clone(), chat_id)]);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// The newest `limit` messages of a chat, oldest first, with their
    /// reactions. Reaction messages themselves are not returned.
    pub fn messages(
        &self,
        account: &AccountId,
        chat: &ChatId,
        limit: usize,
    ) -> StoreResult<Vec<StoredMessage>> {
        self.read(|conn| {
            let sql = format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages m
                 WHERE m.account_id = ?1 AND m.chat_id = ?2
                 ORDER BY m.ts DESC, m.pk DESC LIMIT ?3"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut rows = stmt.query(params![account.as_str(), chat.as_str(), limit as i64])?;
            let mut messages = Vec::new();
            while let Some(row) = rows.next()? {
                messages.push(message_from_row(row, 0)?);
            }
            messages.reverse();

            // Reactions for the whole chat, by target and emoji, with who
            // made each: the first to react first.
            let mut reactions: HashMap<String, Vec<ReactionSummary>> = HashMap::new();
            let mut stmt = conn.prepare_cached(
                "SELECT target_id, emoji, sender, from_me FROM reactions
                 WHERE account_id = ?1 AND chat_id = ?2
                 ORDER BY ts, sender",
            )?;
            let mut rows = stmt.query(params![account.as_str(), chat.as_str()])?;
            while let Some(row) = rows.next()? {
                let of_target = reactions.entry(row.get(0)?).or_default();
                let emoji: String = row.get(1)?;
                let reactor = Reactor {
                    sender: ContactId::new(row.get::<_, String>(2)?),
                    from_me: row.get(3)?,
                };
                let at = match of_target.iter().position(|known| known.emoji == emoji) {
                    Some(at) => at,
                    None => {
                        of_target.push(ReactionSummary {
                            emoji,
                            count: 0,
                            from_me: false,
                            by: Vec::new(),
                        });
                        of_target.len() - 1
                    }
                };
                let summary = &mut of_target[at];
                summary.count += 1;
                summary.from_me |= reactor.from_me;
                summary.by.push(reactor);
            }
            // Most used first; between two used as much, the one that
            // came first (the order they are in).
            for of_target in reactions.values_mut() {
                of_target.sort_by_key(|summary| std::cmp::Reverse(summary.count));
            }

            let mut out = Vec::with_capacity(messages.len());
            let mut people = People::new(conn);
            for mut message in messages {
                if message.reply_to.is_some() {
                    resolve_quote(&mut people, &mut message)?;
                }
                people.name_message(&mut message)?;
                let reactions = reactions.remove(message.id.as_str()).unwrap_or_default();
                out.push(StoredMessage { message, reactions });
            }
            Ok(out)
        })
    }

    /// How many messages of a chat are stored.
    pub fn message_count(&self, account: &AccountId, chat: &ChatId) -> StoreResult<usize> {
        self.read(|conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM messages WHERE account_id = ?1 AND chat_id = ?2",
                params![account.as_str(), chat.as_str()],
                |row| row.get(0),
            )?;
            Ok(count as usize)
        })
    }

    /// One stored message by its provider id.
    pub fn message(&self, account: &AccountId, id: &MessageId) -> StoreResult<Option<Message>> {
        self.read(|conn| {
            let sql = format!(
                "SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.account_id = ?1 AND m.id = ?2"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut rows = stmt.query(params![account.as_str(), id.as_str()])?;
            let message = match rows.next()? {
                Some(row) => Some(message_from_row(row, 0)?),
                None => None,
            };
            drop(rows);
            drop(stmt);
            match message {
                Some(mut message) => {
                    People::new(conn).name_message(&mut message)?;
                    Ok(Some(message))
                }
                None => Ok(None),
            }
        })
    }

    /// Replaces the poll a stored message carries: the tally and the
    /// account's own vote. Returns whether there was such a poll. Unlike
    /// an upsert, the vote given here is taken as it is.
    pub fn set_poll(
        &self,
        account: &AccountId,
        id: &MessageId,
        poll: &client_provider::Poll,
    ) -> StoreResult<bool> {
        let chat: Option<String> = self.write(|tx| {
            let found: Option<(i64, String, String)> = tx
                .query_row(
                    "SELECT pk, chat_id, content FROM messages
                     WHERE account_id = ?1 AND id = ?2",
                    params![account.as_str(), id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((pk, chat, content)) = found else {
                return Ok(None);
            };
            if !matches!(serde_json::from_str(&content)?, MessageContent::Poll(_)) {
                return Ok(None);
            }
            tx.execute(
                "UPDATE messages SET content = ?2 WHERE pk = ?1",
                params![
                    pk,
                    serde_json::to_string(&MessageContent::Poll(poll.clone()))?
                ],
            )?;
            Ok(Some(chat))
        })?;
        match chat {
            Some(chat) => {
                self.notify_messages(vec![(account.clone(), ChatId::new(chat))]);
                Ok(true)
            }
            None => Ok(false),
        }
    }

    /// Full-text search over an account's messages, newest first.
    ///
    /// Every word of `query` must appear (as a prefix) in the message;
    /// matching ignores case and diacritics.
    pub fn search_messages(
        &self,
        account: &AccountId,
        query: &str,
        limit: usize,
    ) -> StoreResult<Vec<SearchHit>> {
        let Some(fts_query) = fts_query(query) else {
            return Ok(Vec::new());
        };
        self.read(|conn| {
            // CROSS JOIN keeps the order as written: the index is asked
            // once and its matches looked up. Left to choose, SQLite walks
            // the messages by time and asks the index again for each one,
            // which takes seconds over a few thousand messages.
            let sql = format!(
                "SELECT {MESSAGE_COLUMNS}, c.title
                 FROM messages_fts f
                 CROSS JOIN messages m ON m.pk = f.rowid
                 CROSS JOIN chats c ON c.account_id = m.account_id AND c.id = m.chat_id
                 WHERE messages_fts MATCH ?1 AND m.account_id = ?2 AND m.deleted = 0
                 ORDER BY m.ts DESC LIMIT ?3"
            );
            let mut stmt = conn.prepare_cached(&sql)?;
            let mut rows = stmt.query(params![fts_query, account.as_str(), limit as i64])?;
            let mut hits = Vec::new();
            let mut people = People::new(conn);
            while let Some(row) = rows.next()? {
                let mut message = message_from_row(row, 0)?;
                people.name_message(&mut message)?;
                hits.push(SearchHit {
                    message,
                    chat_title: row.get(MESSAGE_COLUMN_COUNT)?,
                });
            }
            Ok(hits)
        })
    }
}

/// One line describing a message's content, for chat list previews and
/// reply quotes. [`message_preview`] also knows what rides along with the
/// message.
pub fn preview_text(content: &MessageContent, deleted: bool) -> String {
    crate::summary::content_line(content, deleted, false)
}

/// One line describing a message, for chat list previews and reply quotes.
pub fn message_preview(message: &Message) -> String {
    let line =
        crate::summary::content_line(&message.content, message.deleted, message.extras.view_once);
    crate::mentions::with_mention_names(&line, &message.extras.mentions)
}

/// Turns what the user typed into an FTS5 query: each word quoted (so
/// punctuation cannot be read as query syntax) and matched as a prefix.
fn fts_query(input: &str) -> Option<String> {
    let terms: Vec<String> = input
        .split_whitespace()
        .map(|word| word.replace('"', ""))
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .map(|word| format!("\"{word}\"*"))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

fn searchable_body(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::Text { body } => Some(body.clone()),
        MessageContent::Media(media) => media.caption.clone(),
        MessageContent::Location(location) => {
            let words: Vec<&str> = [&location.name, &location.address]
                .into_iter()
                .flatten()
                .map(String::as_str)
                .collect();
            (!words.is_empty()).then(|| words.join(" "))
        }
        MessageContent::Contacts { cards } => Some(
            cards
                .iter()
                .map(|card| card.name.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        ),
        MessageContent::Poll(poll) => Some(
            std::iter::once(poll.question.as_str())
                .chain(poll.options.iter().map(|option| option.name.as_str()))
                .collect::<Vec<_>>()
                .join(" "),
        ),
        MessageContent::Event(event) => Some(match &event.description {
            Some(description) => format!("{} {description}", event.title),
            None => event.title.clone(),
        }),
        MessageContent::Reaction { .. }
        | MessageContent::System(_)
        | MessageContent::Unsupported { .. } => None,
    }
}

/// Writes one contact. With `listed_at` the row comes from the
/// provider's address book and replaces what is stored (but for the
/// "About" text and the picture handle, which the list does not carry);
/// without it the row comes from an event and only adds to it.
fn upsert_contact_tx(
    tx: &rusqlite::Transaction<'_>,
    contact: &Contact,
    listed_at: Option<Timestamp>,
) -> StoreResult<()> {
    let listed = listed_at.is_some();
    tx.execute(
        "INSERT INTO contacts
            (account_id, id, name, phone, avatar, saved_name, profile_name, business_name,
             username, about, picture_id, listed_at, sort_name)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (account_id, id) DO UPDATE SET
            name = CASE WHEN ?14 THEN excluded.name ELSE COALESCE(excluded.name, name) END,
            phone = CASE WHEN ?14 THEN excluded.phone ELSE COALESCE(excluded.phone, phone) END,
            avatar = COALESCE(excluded.avatar, avatar),
            saved_name = CASE WHEN ?14 THEN excluded.saved_name
                              ELSE COALESCE(excluded.saved_name, saved_name) END,
            profile_name = CASE WHEN ?14 THEN excluded.profile_name
                                ELSE COALESCE(excluded.profile_name, profile_name) END,
            business_name = CASE WHEN ?14 THEN excluded.business_name
                                 ELSE COALESCE(excluded.business_name, business_name) END,
            username = CASE WHEN ?14 THEN excluded.username
                            ELSE COALESCE(excluded.username, username) END,
            about = COALESCE(excluded.about, about),
            picture_id = CASE WHEN ?14 THEN excluded.picture_id
                              ELSE COALESCE(excluded.picture_id, picture_id) END,
            listed_at = COALESCE(excluded.listed_at, listed_at)",
        params![
            contact.account_id.as_str(),
            contact.id.as_str(),
            contact.name,
            contact.phone,
            contact.avatar.as_ref().map(MediaRef::as_str),
            contact.saved_name,
            contact.profile_name,
            contact.business_name,
            contact.username,
            contact.about,
            contact.picture_id,
            listed_at.map(Timestamp::as_millis),
            contact.display_name().to_lowercase(),
            listed,
        ],
    )?;
    if !contact.alt_ids.is_empty() {
        // The same person under their other ids: one name for all of them.
        let ids: Vec<&str> = std::iter::once(&contact.id)
            .chain(&contact.alt_ids)
            .map(ContactId::as_str)
            .collect();
        people::link_ids_tx(tx, contact.account_id.as_str(), &ids)?;
    }
    if !listed {
        // The names may have been merged: sort by what is stored now.
        tx.execute(
            "UPDATE contacts SET sort_name = lower(COALESCE(
                 NULLIF(TRIM(saved_name), ''), NULLIF(TRIM(business_name), ''),
                 NULLIF(TRIM(profile_name), ''), NULLIF(TRIM(name), ''), phone, id))
             WHERE account_id = ?1 AND id = ?2",
            params![contact.account_id.as_str(), contact.id.as_str()],
        )?;
    }
    Ok(())
}

fn kind_to_str(kind: ChatKind) -> &'static str {
    match kind {
        ChatKind::Direct => "direct",
        ChatKind::Group => "group",
    }
}

fn kind_from_str(kind: &str) -> ChatKind {
    match kind {
        "group" => ChatKind::Group,
        _ => ChatKind::Direct,
    }
}

pub(crate) fn status_to_parts(status: &DeliveryStatus) -> (&'static str, Option<&str>) {
    match status {
        DeliveryStatus::Pending => ("pending", None),
        DeliveryStatus::Accepted => ("accepted", None),
        DeliveryStatus::Sent => ("sent", None),
        DeliveryStatus::Delivered => ("delivered", None),
        DeliveryStatus::Read => ("read", None),
        DeliveryStatus::Failed { reason } => ("failed", Some(reason)),
    }
}

pub(crate) fn status_from_parts(name: &str, reason: Option<String>) -> DeliveryStatus {
    match name {
        "accepted" => DeliveryStatus::Accepted,
        "sent" => DeliveryStatus::Sent,
        "delivered" => DeliveryStatus::Delivered,
        "read" => DeliveryStatus::Read,
        "failed" => DeliveryStatus::Failed {
            reason: reason.unwrap_or_default(),
        },
        _ => DeliveryStatus::Pending,
    }
}

/// Reads a message from a row whose message columns start at `base`.
fn message_from_row(row: &Row<'_>, base: usize) -> StoreResult<Message> {
    let content: String = row.get(base + 8)?;
    let reply_to: Option<String> = row.get(base + 9)?;
    let status: String = row.get(base + 10)?;
    Ok(Message {
        account_id: AccountId::new(row.get::<_, String>(base)?),
        chat_id: ChatId::new(row.get::<_, String>(base + 1)?),
        id: MessageId::new(row.get::<_, String>(base + 2)?),
        client_id: row
            .get::<_, Option<String>>(base + 3)?
            .map(ClientMessageId::new),
        sender: ContactId::new(row.get::<_, String>(base + 4)?),
        sender_name: row.get(base + 5)?,
        direction: if row.get::<_, bool>(base + 6)? {
            Direction::Outgoing
        } else {
            Direction::Incoming
        },
        timestamp: Timestamp::from_millis(row.get(base + 7)?),
        content: serde_json::from_str(&content)?,
        reply_to: reply_to.as_deref().map(serde_json::from_str).transpose()?,
        status: status_from_parts(&status, row.get(base + 11)?),
        edited: row.get(base + 12)?,
        deleted: row.get(base + 13)?,
        extras: match row.get::<_, Option<String>>(base + 14)? {
            Some(extras) => serde_json::from_str(&extras)?,
            None => Default::default(),
        },
    })
}

fn chat_summary_from_row(conn: &Connection, row: &Row<'_>) -> StoreResult<ChatSummary> {
    // The joined message columns are all NULL when the chat has no message.
    let has_message = row.get::<_, Option<String>>(9)?.is_some();
    let last_message = if has_message {
        let mut message = message_from_row(row, 9)?;
        // The line reads in the list as it does in the conversation: the
        // sender, the people mentioned and who a notice is about go by
        // the same names.
        People::new(conn).name_message(&mut message)?;
        Some(MessagePreview {
            text: message_preview(&message),
            outgoing: message.direction == Direction::Outgoing,
            status: message.status,
            sender_name: message.sender_name,
            timestamp: message.timestamp,
        })
    } else {
        None
    };
    Ok(ChatSummary {
        id: ChatId::new(row.get::<_, String>(0)?),
        account_id: AccountId::new(row.get::<_, String>(1)?),
        kind: kind_from_str(&row.get::<_, String>(2)?),
        title: row.get(3)?,
        avatar: row.get::<_, Option<String>>(4)?.map(MediaRef::new),
        unread_count: row.get(5)?,
        pinned: row.get(6)?,
        muted: row.get(7)?,
        archived: row.get(8)?,
        last_message,
    })
}

/// Fills a reply's snapshot from the local copy of the quoted message:
/// who wrote it, by the name everything else calls them, and its line.
fn resolve_quote(people: &mut People<'_>, message: &mut Message) -> StoreResult<()> {
    let Some(reply) = message.reply_to.as_mut() else {
        return Ok(());
    };
    let sql =
        format!("SELECT {MESSAGE_COLUMNS} FROM messages m WHERE m.account_id = ?1 AND m.id = ?2");
    let quoted = people
        .conn()
        .prepare_cached(&sql)?
        .query_row(
            params![message.account_id.as_str(), reply.message_id.as_str()],
            |row| Ok(message_from_row(row, 0)),
        )
        .optional()?;
    let Some(mut quoted) = quoted.transpose()? else {
        return Ok(());
    };
    people.name_message(&mut quoted)?;
    if reply.preview.is_none() {
        reply.preview = Some(message_preview(&quoted));
    }
    if quoted.direction == Direction::Incoming {
        // The quoted person by the name they go by now, not the one the
        // provider's snapshot had.
        reply.sender_name = quoted.sender_name.or(reply.sender_name.take());
    }
    Ok(())
}

/// Inserts or merges one message inside a transaction.
pub(crate) fn upsert_message_tx(tx: &Transaction<'_>, message: &Message) -> StoreResult<Upsert> {
    // The name a message comes with is its sender's, wherever else that
    // person shows up.
    people::note_sender_tx(tx, message)?;
    if let MessageContent::Reaction { target, emoji } = &message.content {
        return upsert_reaction_tx(tx, message, target, emoji);
    }

    // The same message can be known under two keys: the provider's id, and
    // the client id of the pending local copy.
    let mut stmt = tx.prepare_cached(
        "SELECT pk, id, status, status_reason, content, reply_to, ts, edited, deleted, client_id,
                extras
         FROM messages
         WHERE account_id = ?1 AND (id = ?2 OR (?3 IS NOT NULL AND client_id = ?3))
         ORDER BY (id = ?2) DESC",
    )?;
    struct Existing {
        pk: i64,
        id: String,
        status: DeliveryStatus,
        content: String,
        reply_to: Option<String>,
        ts: i64,
        edited: bool,
        deleted: bool,
        client_id: Option<String>,
        extras: Option<String>,
    }
    let existing: Vec<Existing> = stmt
        .query_map(
            params![
                message.account_id.as_str(),
                message.id.as_str(),
                message.client_id.as_ref().map(ClientMessageId::as_str),
            ],
            |row| {
                Ok(Existing {
                    pk: row.get(0)?,
                    id: row.get(1)?,
                    status: status_from_parts(&row.get::<_, String>(2)?, row.get(3)?),
                    content: row.get(4)?,
                    reply_to: row.get(5)?,
                    ts: row.get(6)?,
                    edited: row.get(7)?,
                    deleted: row.get(8)?,
                    client_id: row.get(9)?,
                    extras: row.get(10)?,
                })
            },
        )?
        .collect::<Result<_, _>>()?;
    drop(stmt);

    // A provider that cannot say how the account voted must not wipe the
    // vote this client knows of: the tally is taken, the vote is kept.
    let kept_vote = match (&message.content, existing.first()) {
        (MessageContent::Poll(poll), Some(first)) if poll.chosen.is_none() => {
            match serde_json::from_str(&first.content) {
                Ok(MessageContent::Poll(known)) if known.chosen.is_some() => {
                    Some(MessageContent::Poll(client_provider::Poll {
                        chosen: known.chosen,
                        ..poll.clone()
                    }))
                }
                _ => None,
            }
        }
        _ => None,
    };
    let stored_content = kept_vote.as_ref().unwrap_or(&message.content);
    let content = serde_json::to_string(stored_content)?;
    // What a reply to a story says about the story is the client's: the
    // provider's own copy of the message does not carry it back.
    // A provider that does say it names the story and nothing of what it
    // showed: the mark written here says more, and for a message that
    // came from somebody else the story held here does.
    let known_story_reply = existing
        .first()
        .and_then(|first| first.extras.as_deref())
        .and_then(|json| serde_json::from_str::<client_provider::MessageExtras>(json).ok())
        .and_then(|extras| extras.story_reply);
    let kept_story_reply = match (&message.extras.story_reply, known_story_reply) {
        (None, known) => known,
        (Some(told), Some(known)) if known.story == told.story && told.preview.is_none() => {
            Some(known)
        }
        (Some(told), _) if told.preview.is_none() => tx
            .query_row(
                "SELECT body FROM stories WHERE account_id = ?1 AND id = ?2",
                params![message.account_id.as_str(), told.story.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .and_then(|body| serde_json::from_str::<client_provider::StoryBody>(&body).ok())
            .map(|body| client_provider::StoryReplyRef {
                kind: client_provider::StoryReplyKind::of(&body),
                preview: body.words().map(|words| {
                    let mut chars = words.chars();
                    let head: String = chars.by_ref().take(80).collect();
                    match chars.next() {
                        Some(_) => format!("{}…", head.trim_end()),
                        None => head,
                    }
                }),
                ..told.clone()
            }),
        _ => None,
    };
    let mut merged_extras;
    let stored_extras = match kept_story_reply {
        Some(reply) => {
            merged_extras = message.extras.clone();
            merged_extras.story_reply = Some(reply);
            &merged_extras
        }
        None => &message.extras,
    };
    let extras = if stored_extras.is_empty() {
        None
    } else {
        Some(serde_json::to_string(stored_extras)?)
    };
    let reply_to = message
        .reply_to
        .as_ref()
        .map(serde_json::to_string)
        .transpose()?;
    let body = searchable_body(stored_content);
    let outgoing = message.direction == Direction::Outgoing;

    let outcome = match existing.as_slice() {
        [] => {
            let (status, reason) = status_to_parts(&message.status);
            tx.execute(
                "INSERT INTO messages
                    (account_id, chat_id, id, client_id, sender, sender_name, outgoing, ts,
                     body, content, reply_to, status, status_reason, edited, deleted, extras)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                         ?16)",
                params![
                    message.account_id.as_str(),
                    message.chat_id.as_str(),
                    message.id.as_str(),
                    message.client_id.as_ref().map(ClientMessageId::as_str),
                    message.sender.as_str(),
                    message.sender_name,
                    outgoing,
                    message.timestamp.as_millis(),
                    body,
                    content,
                    reply_to,
                    status,
                    reason,
                    message.edited,
                    message.deleted,
                    extras,
                ],
            )?;
            Upsert::Inserted
        }
        [first, rest @ ..] => {
            // Two rows for one message (the provider's copy arrived without
            // the client id before the local copy learned its real id):
            // keep the first, which is the one with the provider's id.
            let mut status = first.status.clone();
            let mut client_id = first.client_id.clone();
            for duplicate in rest {
                if duplicate.status.rank() > status.rank() {
                    status = duplicate.status.clone();
                }
                client_id = client_id.or_else(|| duplicate.client_id.clone());
                tx.execute("DELETE FROM messages WHERE pk = ?1", params![duplicate.pk])?;
            }
            if status.can_advance_to(&message.status) {
                status = message.status.clone();
            }
            let client_id = message
                .client_id
                .as_ref()
                .map(|c| c.as_str().to_owned())
                .or(client_id);

            let unchanged = rest.is_empty()
                && first.id == message.id.as_str()
                && first.status == status
                && first.content == content
                && first.reply_to == reply_to
                && first.ts == message.timestamp.as_millis()
                && first.edited == message.edited
                && first.deleted == message.deleted
                && first.extras == extras
                && first.client_id == client_id;
            if unchanged {
                Upsert::Unchanged
            } else {
                if let Some(own) = client_id.as_deref().filter(|_| outgoing) {
                    // The provider's copy of a message that was still
                    // waiting here: it has it, so nothing is left to send.
                    let waited = existing
                        .iter()
                        .any(|known| known.id.starts_with(crate::outbox::LOCAL_ID));
                    if waited && !message.id.as_str().starts_with(crate::outbox::LOCAL_ID) {
                        crate::outbox::settle_tx(tx, own, Timestamp::now())?;
                    }
                    if first.status != status {
                        sends::status_tx(tx, own, &status, Timestamp::now())?;
                    }
                }
                let (status, reason) = status_to_parts(&status);
                tx.execute(
                    "UPDATE messages SET
                        id = ?2, client_id = ?3, sender = ?4,
                        sender_name = COALESCE(?5, sender_name), outgoing = ?6, ts = ?7,
                        body = ?8, content = ?9, reply_to = ?10, status = ?11,
                        status_reason = ?12, edited = ?13, deleted = ?14, extras = ?15
                     WHERE pk = ?1",
                    params![
                        first.pk,
                        message.id.as_str(),
                        client_id,
                        message.sender.as_str(),
                        message.sender_name,
                        outgoing,
                        message.timestamp.as_millis(),
                        body,
                        content,
                        reply_to,
                        status,
                        reason,
                        message.edited,
                        message.deleted,
                        extras,
                    ],
                )?;
                // The provider names the file differently now (it stores
                // a file that was still on WhatsApp, the local copy has
                // its address): what was fetched is not fetched again.
                if let Some(new) = media_source(stored_content) {
                    for known in &existing {
                        if known.content == content {
                            continue;
                        }
                        let old = serde_json::from_str::<MessageContent>(&known.content).ok();
                        match old.as_ref().and_then(media_source) {
                            Some(old) if old != new => move_media_tx(tx, old, new)?,
                            _ => {}
                        }
                    }
                }
                Upsert::Updated
            }
        }
    };

    if outcome != Upsert::Unchanged {
        touch_chat_tx(tx, message)?;
    }
    Ok(outcome)
}

/// The reference of the file of a message, when it has one.
fn media_source(content: &MessageContent) -> Option<&str> {
    match content {
        MessageContent::Media(media) => media.source.as_ref().map(|source| source.as_str()),
        _ => None,
    }
}

/// Moves what the media cache holds for the reference `old` to the
/// reference `new`: every entry whose key is `<kind>:<old>`, whatever the
/// kind (the file, its thumbnail, what the application keeps of it). An
/// entry already there under the new key stays as it is. While another
/// message still has the old reference its entries stay too, and the new
/// one gets copies.
fn move_media_tx(tx: &Transaction<'_>, old: &str, new: &str) -> StoreResult<()> {
    let keys: Vec<String> = tx
        .prepare_cached(
            "SELECT key FROM media_cache
             WHERE instr(key, ':') > 0 AND substr(key, instr(key, ':') + 1) = ?1",
        )?
        .query_map(params![old], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if keys.is_empty() {
        return Ok(());
    }
    // As the reference is written in a message's content.
    let written = serde_json::to_string(old)?;
    let shared: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM messages WHERE instr(content, ?1) > 0)",
        params![written],
        |row| row.get(0),
    )?;
    for key in keys {
        let kind = &key[..key.len() - old.len()];
        let moved = format!("{kind}{new}");
        if shared {
            tx.execute(
                "INSERT OR IGNORE INTO media_cache
                    (key, bytes, mime, width, height, size, last_used)
                 SELECT ?2, bytes, mime, width, height, size, last_used
                 FROM media_cache WHERE key = ?1",
                params![key, moved],
            )?;
        } else {
            tx.execute(
                "UPDATE OR IGNORE media_cache SET key = ?2 WHERE key = ?1",
                params![key, moved],
            )?;
            // Still there when the new key was taken: nobody looks for it.
            tx.execute("DELETE FROM media_cache WHERE key = ?1", params![key])?;
        }
    }
    Ok(())
}

/// Makes sure the message's chat exists and is ordered by its newest
/// message.
fn touch_chat_tx(tx: &Transaction<'_>, message: &Message) -> StoreResult<()> {
    // A message can arrive before its chat is listed. Create a stub the
    // provider's chat listing will complete later.
    tx.execute(
        "INSERT OR IGNORE INTO chats (account_id, id, kind, title) VALUES (?1, ?2, 'direct', ?3)",
        params![
            message.account_id.as_str(),
            message.chat_id.as_str(),
            match (&message.direction, &message.sender_name) {
                (Direction::Incoming, Some(name)) => name.as_str(),
                _ => message.chat_id.as_str(),
            },
        ],
    )?;
    tx.execute(
        "UPDATE chats SET last_message_at = MAX(COALESCE(last_message_at, 0), ?3)
         WHERE account_id = ?1 AND id = ?2",
        params![
            message.account_id.as_str(),
            message.chat_id.as_str(),
            message.timestamp.as_millis(),
        ],
    )?;
    Ok(())
}

fn upsert_reaction_tx(
    tx: &Transaction<'_>,
    message: &Message,
    target: &MessageId,
    emoji: &str,
) -> StoreResult<Upsert> {
    let key = params![
        message.account_id.as_str(),
        target.as_str(),
        message.sender.as_str()
    ];
    let existing = tx
        .query_row(
            "SELECT emoji, ts FROM reactions
             WHERE account_id = ?1 AND target_id = ?2 AND sender = ?3",
            key,
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()?;
    let ts = message.timestamp.as_millis();
    match existing {
        // History is replayed in arbitrary order: only the newest reaction
        // of a person counts.
        Some((_, existing_ts)) if existing_ts > ts => Ok(Upsert::Unchanged),
        Some((existing_emoji, _)) if existing_emoji == emoji => Ok(Upsert::Unchanged),
        None if emoji.is_empty() => Ok(Upsert::Unchanged),
        existing => {
            if emoji.is_empty() {
                tx.execute(
                    "DELETE FROM reactions
                     WHERE account_id = ?1 AND target_id = ?2 AND sender = ?3",
                    key,
                )?;
            } else {
                tx.execute(
                    "INSERT OR REPLACE INTO reactions
                        (account_id, chat_id, target_id, sender, emoji, from_me, ts)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        message.account_id.as_str(),
                        message.chat_id.as_str(),
                        target.as_str(),
                        message.sender.as_str(),
                        emoji,
                        message.direction == Direction::Outgoing,
                        ts,
                    ],
                )?;
            }
            Ok(if existing.is_some() {
                Upsert::Updated
            } else {
                Upsert::Inserted
            })
        }
    }
}
