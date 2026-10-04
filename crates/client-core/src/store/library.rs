//! The sticker and GIF library (schema v9): files the user keeps, to be
//! sent again.
//!
//! Every item is one file, told by the hash of its bytes (`id`), so the
//! same sticker saved from a chat, imported from disk and starred on the
//! phone is one row. Next to the bytes a row keeps what is needed to show
//! it (a small still of its first frame), to find it (name, pack, when it
//! was last used) and to arrange it (favorite, and its place among the
//! favorites).
//!
//! The library has a budget. Past it the items used longest ago are let
//! go; a favorite never is.

use super::{Store, StoreChange, StoreResult};
use client_provider::{AccountId, MediaKind, MessageContent, MessageId, Timestamp};
use rusqlite::{params, OptionalExtension, Row};

/// What a library item is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibraryKind {
    /// A sticker: a WebP of at most 512 by 512.
    Sticker,
    /// A GIF: a short MP4 that plays as one, or a `.gif` file.
    Gif,
}

impl LibraryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sticker => "sticker",
            Self::Gif => "gif",
        }
    }

    fn of(text: &str) -> Self {
        if text == "gif" {
            Self::Gif
        } else {
            Self::Sticker
        }
    }
}

/// Where an item came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LibrarySource {
    /// Saved from a message somebody sent.
    Received,
    /// Saved from a message sent from here.
    Sent,
    /// An image or video on this computer.
    Imported,
    /// Starred on the account's phone.
    Phone,
    /// Found with the online search.
    Online,
}

impl LibrarySource {
    fn as_str(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Sent => "sent",
            Self::Imported => "imported",
            Self::Phone => "phone",
            Self::Online => "online",
        }
    }

    fn of(text: &str) -> Self {
        match text {
            "received" => Self::Received,
            "sent" => Self::Sent,
            "phone" => Self::Phone,
            "online" => Self::Online,
            _ => Self::Imported,
        }
    }
}

/// A file to put in the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewLibraryItem {
    /// Sticker or GIF.
    pub kind: LibraryKind,
    /// The file.
    pub bytes: Vec<u8>,
    /// Its MIME type.
    pub mime: String,
    /// Whether it moves.
    pub animated: bool,
    /// Its size in pixels, when known.
    pub size: Option<(u32, u32)>,
    /// Where it came from.
    pub source: LibrarySource,
    /// A name to find it by.
    pub name: Option<String>,
    /// The pack it belongs to.
    pub pack: Option<String>,
    /// A still of its first frame (PNG or JPEG) and its type.
    pub thumb: Option<(Vec<u8>, String)>,
}

/// One item of the library, without its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryItem {
    /// The hash of its bytes.
    pub id: String,
    /// Sticker or GIF.
    pub kind: LibraryKind,
    /// Its MIME type.
    pub mime: String,
    /// Whether it moves.
    pub animated: bool,
    /// Its size in pixels, when known.
    pub size: Option<(u32, u32)>,
    /// Its size in bytes.
    pub bytes: u64,
    /// Where it came from.
    pub source: LibrarySource,
    /// A name to find it by.
    pub name: Option<String>,
    /// The pack it belongs to.
    pub pack: Option<String>,
    /// When it came in.
    pub added_at: Timestamp,
    /// When it was last sent or used.
    pub last_used: Option<Timestamp>,
    /// Starred.
    pub favorite: bool,
    /// Its place among the favorites.
    pub position: i64,
    /// Whether a still of it is kept.
    pub has_thumb: bool,
    /// Made by the indexer from a sticker in a chat, not kept by the
    /// user (until it is starred, which keeps it).
    pub indexed: bool,
    /// When the account last sent it, from here or from its phone.
    pub sent_at: Option<Timestamp>,
    /// When the account last received it.
    pub heard_at: Option<Timestamp>,
    /// The message file it was seen in, when it was seen in one: the
    /// account and the media reference.
    pub origin: Option<(AccountId, String)>,
    /// The message of that account it was seen in, when that is known.
    pub origin_message: Option<MessageId>,
}

impl LibraryItem {
    /// A sticker seen in a chat whose file is not here yet: a place for
    /// it, with nothing to show or send until the file arrives.
    pub fn pending(&self) -> bool {
        self.bytes == 0
    }
}

/// A pack: a set the user's imports made, or one that was given a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryPack {
    /// Its id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// When it was made.
    pub created_at: Timestamp,
}

/// What the library holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LibraryStats {
    /// How many items.
    pub items: u64,
    /// How many bytes they take (the stills not counted).
    pub bytes: u64,
    /// How many of them are favorites.
    pub favorites: u64,
}

const COLUMNS: &str = "id, kind, mime, animated, width, height, size, source, name, pack,
                       added_at, last_used, favorite, position, thumb IS NOT NULL,
                       indexed, sent_at, heard_at, origin_account, origin_url,
                       origin_message";

fn item(row: &Row<'_>) -> rusqlite::Result<LibraryItem> {
    let (width, height): (Option<u32>, Option<u32>) = (row.get(4)?, row.get(5)?);
    let last_used: Option<i64> = row.get(11)?;
    Ok(LibraryItem {
        id: row.get(0)?,
        kind: LibraryKind::of(&row.get::<_, String>(1)?),
        mime: row.get(2)?,
        animated: row.get::<_, i64>(3)? != 0,
        size: width.zip(height),
        bytes: row.get::<_, i64>(6)? as u64,
        source: LibrarySource::of(&row.get::<_, String>(7)?),
        name: row.get(8)?,
        pack: row.get(9)?,
        added_at: Timestamp::from_millis(row.get(10)?),
        last_used: last_used.map(Timestamp::from_millis),
        favorite: row.get::<_, i64>(12)? != 0,
        position: row.get(13)?,
        has_thumb: row.get::<_, i64>(14)? != 0,
        indexed: row.get::<_, i64>(15)? != 0,
        sent_at: row.get::<_, Option<i64>>(16)?.map(Timestamp::from_millis),
        heard_at: row.get::<_, Option<i64>>(17)?.map(Timestamp::from_millis),
        origin: row
            .get::<_, Option<String>>(18)?
            .zip(row.get::<_, Option<String>>(19)?)
            .map(|(account, url)| (AccountId::new(account), url)),
        origin_message: row.get::<_, Option<String>>(20)?.map(MessageId::new),
    })
}

impl Store {
    /// Puts a file in the library. The same bytes are one item: adding
    /// them again keeps what the item has (its favorite, its place) and
    /// only fills in what it lacked (a name, a pack). `budget` is the most
    /// bytes the library may hold; past it the items used longest ago go,
    /// favorites excepted. Answers the item and whether it is new.
    pub fn library_add(
        &self,
        new: NewLibraryItem,
        id: &str,
        now: Timestamp,
        budget: u64,
    ) -> StoreResult<(LibraryItem, bool)> {
        let created = self.write(|tx| {
            let known: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM library_items WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                tx.execute(
                    "UPDATE library_items SET
                        name = COALESCE(name, ?2), pack = COALESCE(pack, ?3),
                        thumb = COALESCE(thumb, ?4), thumb_mime = COALESCE(thumb_mime, ?5)
                     WHERE id = ?1",
                    params![
                        id,
                        new.name,
                        new.pack,
                        new.thumb.as_ref().map(|(bytes, _)| bytes),
                        new.thumb.as_ref().map(|(_, mime)| mime),
                    ],
                )?;
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO library_items
                    (id, kind, mime, animated, width, height, size, source, name, pack,
                     bytes, thumb, thumb_mime, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    id,
                    new.kind.as_str(),
                    new.mime,
                    new.animated as i64,
                    new.size.map(|(width, _)| width),
                    new.size.map(|(_, height)| height),
                    new.bytes.len() as i64,
                    new.source.as_str(),
                    new.name,
                    new.pack,
                    new.bytes,
                    new.thumb.as_ref().map(|(bytes, _)| bytes),
                    new.thumb.as_ref().map(|(_, mime)| mime),
                    now.as_millis(),
                ],
            )?;
            Ok(true)
        })?;
        self.library_evict(budget, Some(id))?;
        let stored = self
            .library_item(id)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        self.notify(StoreChange::Library);
        Ok((stored, created))
    }

    /// Lets go of the items used longest ago until the library fits its
    /// budget. A favorite is never let go, and neither is `keep` (what was
    /// just added). Answers how many went.
    pub fn library_evict(&self, budget: u64, keep: Option<&str>) -> StoreResult<usize> {
        let evicted = self.write(|tx| {
            let mut total: i64 = tx.query_row(
                "SELECT COALESCE(SUM(size), 0) FROM library_items",
                [],
                |r| r.get(0),
            )?;
            let mut evicted = 0;
            if total as u64 <= budget {
                return Ok(0);
            }
            let candidates: Vec<(String, i64)> = {
                let mut statement = tx.prepare(
                    "SELECT id, size FROM library_items WHERE favorite = 0
                     ORDER BY indexed DESC, COALESCE(last_used, added_at), added_at, id",
                )?;
                let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
                rows.collect::<Result<_, _>>()?
            };
            for (id, size) in candidates {
                if total as u64 <= budget {
                    break;
                }
                if keep == Some(id.as_str()) {
                    continue;
                }
                // What an account still has starred under it stays noted
                // in `library_remote`: the favorites sync takes that star
                // off there before it forgets it.
                tx.execute("DELETE FROM library_items WHERE id = ?1", params![id])?;
                total -= size;
                evicted += 1;
            }
            Ok(evicted)
        })?;
        if evicted > 0 {
            self.notify(StoreChange::Library);
        }
        Ok(evicted)
    }

    /// One item, without its bytes.
    pub fn library_item(&self, id: &str) -> StoreResult<Option<LibraryItem>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM library_items WHERE id = ?1"),
                    params![id],
                    item,
                )
                .optional()?)
        })
    }

    /// Every item of a kind, newest first, without their bytes.
    pub fn library_items(&self, kind: LibraryKind) -> StoreResult<Vec<LibraryItem>> {
        self.read(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM library_items WHERE kind = ?1
                 ORDER BY added_at DESC, id"
            ))?;
            let rows = statement.query_map(params![kind.as_str()], item)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The favorites of a kind, in the user's order.
    pub fn library_favorites(&self, kind: LibraryKind) -> StoreResult<Vec<LibraryItem>> {
        self.read(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM library_items WHERE kind = ?1 AND favorite = 1
                 ORDER BY position, added_at, id"
            ))?;
            let rows = statement.query_map(params![kind.as_str()], item)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The items of a kind used or sent by the account last, most recent
    /// first, at most `limit`. A sticker the account sent from its phone
    /// counts as used when it was sent.
    pub fn library_recent(&self, kind: LibraryKind, limit: usize) -> StoreResult<Vec<LibraryItem>> {
        self.read(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM library_items
                 WHERE kind = ?1 AND (last_used IS NOT NULL OR sent_at IS NOT NULL)
                 ORDER BY MAX(COALESCE(last_used, 0), COALESCE(sent_at, 0)) DESC, id
                 LIMIT ?2"
            ))?;
            let rows = statement.query_map(params![kind.as_str(), limit as i64], item)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The file of an item and its type.
    pub fn library_file(&self, id: &str) -> StoreResult<Option<(Vec<u8>, String)>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT bytes, mime FROM library_items WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
        })
    }

    /// The still of an item and its type.
    pub fn library_thumb(&self, id: &str) -> StoreResult<Option<(Vec<u8>, String)>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT thumb, thumb_mime FROM library_items
                     WHERE id = ?1 AND thumb IS NOT NULL",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
        })
    }

    /// Notes that an item was sent or used just now: it goes first among
    /// the recent ones, and is the last to be let go.
    pub fn library_touch(&self, id: &str, now: Timestamp) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE library_items SET last_used = ?2 WHERE id = ?1",
                params![id, now.as_millis()],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Library);
        Ok(())
    }

    /// Stars or unstars an item. A new favorite goes last. Answers
    /// whether anything changed.
    pub fn library_set_favorite(&self, id: &str, favorite: bool) -> StoreResult<bool> {
        let changed = self.write(|tx| {
            let now: Option<i64> = tx
                .query_row(
                    "SELECT favorite FROM library_items WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?;
            match now {
                None => Ok(false),
                Some(now) if (now != 0) == favorite => Ok(false),
                Some(_) => {
                    let place: i64 = tx.query_row(
                        "SELECT COALESCE(MAX(position) + 1, 0) FROM library_items
                         WHERE favorite = 1",
                        [],
                        |row| row.get(0),
                    )?;
                    tx.execute(
                        "UPDATE library_items SET favorite = ?2, position = ?3 WHERE id = ?1",
                        params![id, favorite as i64, if favorite { place } else { 0 }],
                    )?;
                    Ok(true)
                }
            }
        })?;
        if changed {
            self.notify(StoreChange::Library);
        }
        Ok(changed)
    }

    /// Moves a favorite to `to` among the favorites of its kind (0 is
    /// first). Answers whether it moved.
    pub fn library_move_favorite(&self, id: &str, to: usize) -> StoreResult<bool> {
        let moved = self.write(|tx| {
            let kind: Option<String> = tx
                .query_row(
                    "SELECT kind FROM library_items WHERE id = ?1 AND favorite = 1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(kind) = kind else { return Ok(false) };
            let mut order: Vec<String> = {
                let mut statement = tx.prepare(
                    "SELECT id FROM library_items WHERE kind = ?1 AND favorite = 1
                     ORDER BY position, added_at, id",
                )?;
                let rows = statement.query_map(params![kind], |row| row.get(0))?;
                rows.collect::<Result<_, _>>()?
            };
            let Some(from) = order.iter().position(|known| known == id) else {
                return Ok(false);
            };
            let to = to.min(order.len() - 1);
            if from == to {
                return Ok(false);
            }
            let moving = order.remove(from);
            order.insert(to, moving);
            for (place, id) in order.iter().enumerate() {
                tx.execute(
                    "UPDATE library_items SET position = ?2 WHERE id = ?1",
                    params![id, place as i64],
                )?;
            }
            Ok(true)
        })?;
        if moved {
            self.notify(StoreChange::Library);
        }
        Ok(moved)
    }

    /// Takes an item out of the library, favorite or not. What an account
    /// has starred under it stays noted in `library_remote` until the
    /// favorites sync has taken that star off there: forgotten at once,
    /// the next pull would bring the item back, starred.
    pub fn library_remove(&self, id: &str) -> StoreResult<bool> {
        let removed = self.write(|tx| {
            Ok(tx.execute("DELETE FROM library_items WHERE id = ?1", params![id])? > 0)
        })?;
        if removed {
            self.notify(StoreChange::Library);
        }
        Ok(removed)
    }

    /// Empties the library. With `keep_favorites` the starred items stay.
    /// Answers how many items went.
    pub fn library_clear(&self, keep_favorites: bool) -> StoreResult<usize> {
        let removed = self.write(|tx| {
            let filter = if keep_favorites {
                " WHERE favorite = 0"
            } else {
                ""
            };
            tx.execute(
                &format!(
                    "DELETE FROM library_remote WHERE item_id IN
                        (SELECT id FROM library_items{filter})"
                ),
                [],
            )?;
            let removed = tx.execute(&format!("DELETE FROM library_items{filter}"), [])?;
            if !keep_favorites {
                tx.execute("DELETE FROM library_packs", [])?;
            }
            Ok(removed)
        })?;
        if removed > 0 {
            self.notify(StoreChange::Library);
        }
        Ok(removed)
    }

    /// How much the library holds.
    pub fn library_stats(&self) -> StoreResult<LibraryStats> {
        self.read(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*), COALESCE(SUM(size), 0), COALESCE(SUM(favorite), 0)
                 FROM library_items WHERE indexed = 0 OR favorite = 1",
                [],
                |row| {
                    Ok(LibraryStats {
                        items: row.get::<_, i64>(0)? as u64,
                        bytes: row.get::<_, i64>(1)? as u64,
                        favorites: row.get::<_, i64>(2)? as u64,
                    })
                },
            )?)
        })
    }

    // ----- packs -----------------------------------------------------------

    /// Makes a pack, or finds the one with this id.
    pub fn library_pack(&self, id: &str, name: &str, now: Timestamp) -> StoreResult<LibraryPack> {
        self.write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO library_packs (id, name, created_at) VALUES (?1, ?2, ?3)",
                params![id, name, now.as_millis()],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Library);
        Ok(self
            .library_packs()?
            .into_iter()
            .find(|pack| pack.id == id)
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?)
    }

    /// The packs, newest first.
    pub fn library_packs(&self) -> StoreResult<Vec<LibraryPack>> {
        self.read(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, name, created_at FROM library_packs ORDER BY created_at DESC, id",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(LibraryPack {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    created_at: Timestamp::from_millis(row.get(2)?),
                })
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Puts items in a pack.
    pub fn library_set_pack(&self, ids: &[String], pack: Option<&str>) -> StoreResult<()> {
        self.write(|tx| {
            for id in ids {
                tx.execute(
                    "UPDATE library_items SET pack = ?2 WHERE id = ?1",
                    params![id, pack],
                )?;
            }
            Ok(())
        })?;
        self.notify(StoreChange::Library);
        Ok(())
    }

    // ----- what the provider knows as starred -------------------------------

    /// What each account is known to have starred as of the last sync:
    /// the item and the id the provider calls it.
    pub fn library_remote(&self, account: &AccountId) -> StoreResult<Vec<(String, String)>> {
        self.read(|conn| {
            let mut statement = conn.prepare(
                "SELECT item_id, remote_id FROM library_remote WHERE account_id = ?1
                 ORDER BY item_id",
            )?;
            let rows = statement.query_map(params![account.as_str()], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Notes that an account has an item starred, under `remote_id`.
    pub fn library_set_remote(
        &self,
        account: &AccountId,
        item: &str,
        remote_id: &str,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO library_remote (account_id, item_id, remote_id)
                 VALUES (?1, ?2, ?3)",
                params![account.as_str(), item, remote_id],
            )?;
            Ok(())
        })
    }

    /// Notes that an account no longer has an item starred.
    pub fn library_forget_remote(&self, account: &AccountId, item: &str) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM library_remote WHERE account_id = ?1 AND item_id = ?2",
                params![account.as_str(), item],
            )?;
            Ok(())
        })
    }
}

// ----- stickers seen in the chats (schema v10) --------------------------------------

/// Where the indexer has got to in the messages (by their row number):
/// those above `high` have not been looked at (they are new), those below
/// `low` have not either (they are old), and `done` says the old ones
/// need no more looking at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexState {
    /// The newest message looked at.
    pub high: i64,
    /// Everything from here down is still to look at.
    pub low: i64,
    /// Nothing below `low` is wanted.
    pub done: bool,
}

/// A sticker in a message, as the indexer reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StickerMessage {
    /// The message's row number.
    pub pk: i64,
    /// The message.
    pub id: MessageId,
    /// The account.
    pub account: AccountId,
    /// The media reference of its file.
    pub url: String,
    /// The account sent it (from here or from its phone).
    pub outgoing: bool,
    /// When it was sent.
    pub at: Timestamp,
    /// The file's type, when the message says.
    pub mime: Option<String>,
}

/// A sticker seen in a chat, to be listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeenSticker {
    /// The account.
    pub account: AccountId,
    /// The message it was seen in.
    pub message: MessageId,
    /// The media reference it came from.
    pub url: String,
    /// The account sent it.
    pub outgoing: bool,
    /// When it was sent or received.
    pub at: Timestamp,
}

impl Store {
    /// Where the indexer is, or `None` before its first run.
    pub fn library_index_state(&self) -> StoreResult<Option<IndexState>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT high, low, done FROM library_index WHERE id = 1",
                    [],
                    |row| {
                        Ok(IndexState {
                            high: row.get(0)?,
                            low: row.get(1)?,
                            done: row.get::<_, i64>(2)? != 0,
                        })
                    },
                )
                .optional()?)
        })
    }

    /// Notes where the indexer is.
    pub fn library_set_index_state(&self, state: IndexState) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO library_index (id, high, low, done)
                 VALUES (1, ?1, ?2, ?3)",
                params![state.high, state.low, state.done as i64],
            )?;
            Ok(())
        })
    }

    /// Starts the indexer over: every message is to be looked at again.
    pub fn library_reset_index(&self) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute("DELETE FROM library_index", [])?;
            Ok(())
        })
    }

    /// The newest message's row number, `0` with none.
    pub fn newest_message_pk(&self) -> StoreResult<i64> {
        self.read(|conn| {
            Ok(
                conn.query_row("SELECT COALESCE(MAX(pk), 0) FROM messages", [], |row| {
                    row.get(0)
                })?,
            )
        })
    }

    /// The stickers in the messages with a row number above `above` and
    /// not above `up_to`, oldest first (`newest_first` false) or newest
    /// first, at most `limit`. Deleted messages and stickers without a
    /// file to fetch are not listed.
    pub fn sticker_messages(
        &self,
        above: i64,
        up_to: i64,
        newest_first: bool,
        limit: usize,
    ) -> StoreResult<Vec<StickerMessage>> {
        self.read(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT pk, account_id, outgoing, ts, content, id FROM messages
                 WHERE pk > ?1 AND pk <= ?2 AND deleted = 0
                   AND content LIKE '%\"kind\":\"sticker\"%'
                 ORDER BY pk {} LIMIT ?3",
                if newest_first { "DESC" } else { "ASC" }
            ))?;
            let rows = statement.query_map(params![above, up_to, limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? != 0,
                    row.get::<_, i64>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                ))
            })?;
            let mut found = Vec::new();
            for row in rows {
                let (pk, account, outgoing, ts, content, id) = row?;
                let Ok(MessageContent::Media(media)) = serde_json::from_str(&content) else {
                    continue;
                };
                if media.kind != MediaKind::Sticker {
                    continue;
                }
                let Some(source) = media.source else { continue };
                found.push(StickerMessage {
                    pk,
                    id: MessageId::new(id),
                    account: AccountId::new(account),
                    url: source.as_str().to_owned(),
                    outgoing,
                    at: Timestamp::from_millis(ts),
                    mime: media.mime_type,
                });
            }
            Ok(found)
        })
    }

    /// The id of the item that came from this file of a message, if one
    /// did.
    pub fn library_by_origin(&self, account: &AccountId, url: &str) -> StoreResult<Option<String>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT id FROM library_items WHERE origin_url = ?1 AND origin_account = ?2",
                    params![url, account.as_str()],
                    |row| row.get(0),
                )
                .optional()?)
        })
    }

    /// Notes that an item was seen again in a message: sent or received
    /// at `at`. Only ever moves the time forward.
    pub fn library_see(&self, id: &str, outgoing: bool, at: Timestamp) -> StoreResult<()> {
        self.write(|tx| {
            see(tx, id, outgoing, at)?;
            Ok(())
        })
    }

    /// Notes where an item was seen: the account, the media reference and
    /// the message. What the item already says of its origin stays.
    pub fn library_set_origin(
        &self,
        id: &str,
        account: &AccountId,
        url: &str,
        message: Option<&MessageId>,
    ) -> StoreResult<()> {
        self.write(|tx| {
            // A message is only of use with the account it belongs to.
            tx.execute(
                "UPDATE library_items SET
                    origin_message = CASE
                        WHEN origin_account IS NULL OR origin_account = ?2
                            THEN COALESCE(origin_message, ?4)
                        ELSE origin_message END,
                    origin_url = COALESCE(origin_url, ?3),
                    origin_account = COALESCE(origin_account, ?2)
                 WHERE id = ?1",
                params![id, account.as_str(), url, message.map(MessageId::as_str)],
            )?;
            Ok(())
        })
    }

    /// The newest message of an account that carries the file at `url`
    /// and that the provider knows by an id of its own.
    pub fn message_with_media(
        &self,
        account: &AccountId,
        url: &str,
    ) -> StoreResult<Option<MessageId>> {
        // The reference as the content's JSON writes it.
        let quoted = serde_json::to_string(url)?;
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT id FROM messages
                     WHERE account_id = ?1 AND deleted = 0 AND id NOT LIKE 'local:%'
                       AND instr(content, ?2) > 0
                     ORDER BY pk DESC LIMIT 1",
                    params![account.as_str(), quoted],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(MessageId::new))
        })
    }

    /// Lists a sticker seen in a chat. With a `file` the item is that
    /// file (the same file is the same item, kept or not: it only gains
    /// where it was seen); without one it is a place for it, with an id
    /// of its own, until [`library_complete`](Self::library_complete).
    /// Answers whether a row was made.
    pub fn library_add_seen(
        &self,
        seen: &SeenSticker,
        file: Option<(NewLibraryItem, String)>,
        now: Timestamp,
    ) -> StoreResult<bool> {
        let made = self.write(|tx| {
            let (id, new) = match file {
                Some((new, id)) => (id, Some(new)),
                None => (pending_id(&seen.account, &seen.url), None),
            };
            let known: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM library_items WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                tx.execute(
                    "UPDATE library_items SET
                        origin_message = CASE
                            WHEN origin_account IS NULL OR origin_account = ?2
                                THEN COALESCE(origin_message, ?4)
                            ELSE origin_message END,
                        origin_account = COALESCE(origin_account, ?2),
                        origin_url = COALESCE(origin_url, ?3)
                     WHERE id = ?1",
                    params![id, seen.account.as_str(), seen.url, seen.message.as_str()],
                )?;
                see(tx, &id, seen.outgoing, seen.at)?;
                return Ok(false);
            }
            let (kind, mime, animated, size, source, bytes, thumb) = match new {
                Some(new) => (
                    new.kind,
                    new.mime,
                    new.animated,
                    new.size,
                    new.source,
                    new.bytes,
                    new.thumb,
                ),
                None => (
                    LibraryKind::Sticker,
                    "image/webp".to_owned(),
                    false,
                    None,
                    if seen.outgoing {
                        LibrarySource::Sent
                    } else {
                        LibrarySource::Received
                    },
                    Vec::new(),
                    None,
                ),
            };
            tx.execute(
                "INSERT INTO library_items
                    (id, kind, mime, animated, width, height, size, source, bytes, thumb,
                     thumb_mime, added_at, indexed, origin_account, origin_url, origin_message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?14, ?15)",
                params![
                    id,
                    kind.as_str(),
                    mime,
                    animated as i64,
                    size.map(|(width, _)| width),
                    size.map(|(_, height)| height),
                    bytes.len() as i64,
                    source.as_str(),
                    bytes,
                    thumb.as_ref().map(|(bytes, _)| bytes),
                    thumb.as_ref().map(|(_, mime)| mime),
                    now.as_millis(),
                    seen.account.as_str(),
                    seen.url,
                    seen.message.as_str(),
                ],
            )?;
            see(tx, &id, seen.outgoing, seen.at)?;
            Ok(true)
        })?;
        Ok(made)
    }

    /// The places for stickers whose files are not here yet, oldest
    /// first: the id, the account and the media reference.
    pub fn library_pending(&self, limit: usize) -> StoreResult<Vec<(String, AccountId, String)>> {
        self.read(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, origin_account, origin_url FROM library_items
                 WHERE size = 0 AND origin_url IS NOT NULL
                 ORDER BY added_at, id LIMIT ?1",
            )?;
            let rows = statement.query_map(params![limit as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    AccountId::new(row.get::<_, String>(1)?),
                    row.get::<_, String>(2)?,
                ))
            })?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// The file of a place for a sticker has arrived: the place becomes
    /// the item, or, when the library already has that file, is folded
    /// into it (which gains where the place was seen).
    pub fn library_complete(
        &self,
        pending: &str,
        new: NewLibraryItem,
        id: &str,
    ) -> StoreResult<()> {
        self.write(|tx| {
            let known: Option<i64> = tx
                .query_row(
                    "SELECT 1 FROM library_items WHERE id = ?1 AND id <> ?2",
                    params![id, pending],
                    |row| row.get(0),
                )
                .optional()?;
            if known.is_some() {
                // When it was sent and heard, and where it was seen: the
                // account, the media reference, the message.
                type Seen = (
                    Option<i64>,
                    Option<i64>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                );
                let (sent, heard, account, url, message): Seen = tx.query_row(
                    "SELECT sent_at, heard_at, origin_account, origin_url, origin_message
                     FROM library_items WHERE id = ?1",
                    params![pending],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )?;
                tx.execute("DELETE FROM library_items WHERE id = ?1", params![pending])?;
                tx.execute(
                    "UPDATE library_items SET
                        sent_at = MAX(COALESCE(sent_at, 0), COALESCE(?2, 0)),
                        heard_at = MAX(COALESCE(heard_at, 0), COALESCE(?3, 0)),
                        origin_message = CASE
                            WHEN origin_account IS NULL OR origin_account = ?4
                                THEN COALESCE(origin_message, ?6)
                            ELSE origin_message END,
                        origin_account = COALESCE(origin_account, ?4),
                        origin_url = COALESCE(origin_url, ?5)
                     WHERE id = ?1",
                    params![id, sent, heard, account, url, message],
                )?;
                // A zero is no time at all.
                tx.execute(
                    "UPDATE library_items SET sent_at = NULLIF(sent_at, 0),
                        heard_at = NULLIF(heard_at, 0) WHERE id = ?1",
                    params![id],
                )?;
                return Ok(());
            }
            tx.execute(
                "UPDATE library_items SET id = ?2, mime = ?3, animated = ?4, width = ?5,
                    height = ?6, size = ?7, bytes = ?8, thumb = ?9, thumb_mime = ?10
                 WHERE id = ?1",
                params![
                    pending,
                    id,
                    new.mime,
                    new.animated as i64,
                    new.size.map(|(width, _)| width),
                    new.size.map(|(_, height)| height),
                    new.bytes.len() as i64,
                    new.bytes,
                    new.thumb.as_ref().map(|(bytes, _)| bytes),
                    new.thumb.as_ref().map(|(_, mime)| mime),
                ],
            )?;
            Ok(())
        })
    }

    /// The stickers heard from the chats, most recent first, at most
    /// `limit`: those the indexer made or that were seen in a message.
    pub fn library_heard(&self, limit: usize) -> StoreResult<Vec<LibraryItem>> {
        self.read(|conn| {
            let mut statement = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM library_items
                 WHERE kind = 'sticker' AND heard_at IS NOT NULL
                 ORDER BY heard_at DESC, id LIMIT ?1"
            ))?;
            let rows = statement.query_map(params![limit as i64], item)?;
            Ok(rows.collect::<Result<_, _>>()?)
        })
    }

    /// Lets go of the items the indexer made that are past the newest
    /// `sent_keep` the account sent and the newest `heard_keep` it
    /// received, unless they were starred or used. Answers how many went.
    pub fn library_prune_indexed(&self, sent_keep: usize, heard_keep: usize) -> StoreResult<usize> {
        let gone = self.write(|tx| {
            Ok(tx.execute(
                "DELETE FROM library_items
                 WHERE indexed = 1 AND favorite = 0 AND last_used IS NULL
                   AND id NOT IN (SELECT id FROM library_items
                                  WHERE indexed = 1 AND sent_at IS NOT NULL
                                  ORDER BY sent_at DESC LIMIT ?1)
                   AND id NOT IN (SELECT id FROM library_items
                                  WHERE indexed = 1 AND heard_at IS NOT NULL
                                  ORDER BY heard_at DESC LIMIT ?2)",
                params![sent_keep as i64, heard_keep as i64],
            )?)
        })?;
        Ok(gone)
    }

    /// How many items the indexer made that were sent by the account and
    /// how many were received: to tell when it has all it wants.
    pub fn library_indexed_counts(&self) -> StoreResult<(usize, usize)> {
        self.read(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(CASE WHEN sent_at IS NOT NULL THEN 1 END),
                        COUNT(CASE WHEN heard_at IS NOT NULL AND sent_at IS NULL THEN 1 END)
                 FROM library_items WHERE indexed = 1",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)? as usize,
                        row.get::<_, i64>(1)? as usize,
                    ))
                },
            )?)
        })
    }
}

/// The id of the place for a sticker whose file is not here: it is made
/// of where the file is, not of what it holds.
fn pending_id(account: &AccountId, url: &str) -> String {
    format!(
        "pending:{}",
        crate::library::content_id(format!("{account}\n{url}").as_bytes())
    )
}

fn see(tx: &rusqlite::Transaction<'_>, id: &str, outgoing: bool, at: Timestamp) -> StoreResult<()> {
    let column = if outgoing { "sent_at" } else { "heard_at" };
    tx.execute(
        &format!(
            "UPDATE library_items SET {column} = MAX(COALESCE({column}, 0), ?2) WHERE id = ?1"
        ),
        params![id, at.as_millis()],
    )?;
    Ok(())
}
