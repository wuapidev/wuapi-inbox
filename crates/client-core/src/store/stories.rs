//! Stories in the store (schema v11): contacts' stories and the account's
//! own, who saw the latter, who is muted, the audience, and the view
//! receipts that are owed to authors.
//!
//! A story is kept until it expires (a day after it was posted); the
//! media cached for it goes with it. Every function that depends on the
//! time takes it as `now`, so tests drive the clock.
//!
//! "Viewed" is this client's own record: it is set when the viewer showed
//! the story (never when it was listed or prefetched), and a provider that
//! says the account saw it elsewhere only adds to it.

use super::{people::People, Store, StoreChange, StoreResult};
use client_provider::{
    AccountId, ClientMessageId, ContactId, MessageId, Story, StoryBody, StoryPrivacy, StoryViewer,
    Timestamp,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::collections::{HashMap, HashSet};

/// The most stories kept per account. A day of stories from a large
/// address book stays well under it.
pub const STORIES_KEPT: usize = 600;

/// What the account's own story that is still on its way out is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoryPostState {
    /// Waiting to be uploaded or posted, or being retried.
    Queued,
    /// Refused for good, with the reason.
    Failed(String),
}

/// One story as the views read it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoryItem {
    /// The story.
    pub story: Story,
    /// It counts as seen: shown here, or seen elsewhere.
    pub viewed: bool,
    /// When it was shown here.
    pub viewed_at: Option<Timestamp>,
    /// The account's own story that has not been posted yet (its id is a
    /// local one until then).
    pub post: Option<StoryPostState>,
}

/// The stories of one person, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoryAuthor {
    /// What stands for the person under all of their ids (their number
    /// when known).
    pub key: String,
    /// The id the newest story names.
    pub author: ContactId,
    /// Every id the person goes by.
    pub ids: Vec<ContactId>,
    /// The name every view calls them by, when one is known.
    pub name: Option<String>,
    /// Their number, when known.
    pub phone: Option<String>,
    /// Their unexpired stories, oldest first.
    pub stories: Vec<StoryItem>,
    /// Their stories are muted.
    pub muted: bool,
}

impl StoryAuthor {
    /// When the newest of their stories was posted.
    pub fn latest_at(&self) -> Timestamp {
        self.stories
            .iter()
            .map(|item| item.story.posted_at)
            .max()
            .unwrap_or_default()
    }

    /// How many of their stories have not been seen.
    pub fn unviewed(&self) -> usize {
        self.stories.iter().filter(|item| !item.viewed).count()
    }

    /// Where the viewer starts: their first story not seen yet, else the
    /// first.
    pub fn start_index(&self) -> usize {
        self.stories
            .iter()
            .position(|item| !item.viewed)
            .unwrap_or(0)
    }

    /// The name to show: the name, else the number, else a neutral word.
    pub fn display_name(&self) -> String {
        self.name
            .clone()
            .or_else(|| self.phone.as_deref().map(crate::phone::format))
            .unwrap_or_else(|| "Someone".to_owned())
    }
}

/// What the Status view lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoryFeed {
    /// The account's own stories, oldest first, those on their way out
    /// included.
    pub mine: Vec<StoryItem>,
    /// Authors with a story not seen yet, the newest first.
    pub recent: Vec<StoryAuthor>,
    /// Authors whose stories were all seen, the newest first.
    pub viewed: Vec<StoryAuthor>,
    /// Authors that are muted, the newest first.
    pub muted: Vec<StoryAuthor>,
}

impl StoryFeed {
    /// How many stories of other people have not been seen (muted
    /// authors do not count).
    pub fn unviewed(&self) -> usize {
        self.recent.iter().map(StoryAuthor::unviewed).sum()
    }

    /// Every author, in the order the list shows them.
    pub fn authors(&self) -> impl Iterator<Item = &StoryAuthor> {
        self.recent
            .iter()
            .chain(self.viewed.iter())
            .chain(self.muted.iter())
    }
}

/// How many stories somebody has, for the ring around their picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoryRing {
    /// Unexpired stories.
    pub total: usize,
    /// Of them, not seen.
    pub unviewed: usize,
}

/// A receipt owed to an author.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoryReceipt {
    /// The account.
    pub account_id: AccountId,
    /// The story that was seen.
    pub story: MessageId,
    /// Its author.
    pub author: ContactId,
    /// How many times it was tried.
    pub attempts: u32,
    /// When it is next due.
    pub next_attempt_at: Timestamp,
}

const COLUMNS: &str = "s.account_id, s.id, s.client_id, s.author, s.author_name, s.mine, \
     s.posted_at, s.expires_at, s.body, s.mentions, s.viewed_at, s.provider_viewed, s.view_count";

fn kind_of(body: &StoryBody) -> &'static str {
    use client_provider::MediaKind::*;
    match body {
        StoryBody::Text { .. } => "text",
        StoryBody::Media(media) => match media.kind {
            Video => "video",
            Audio | Voice => "voice",
            _ => "image",
        },
    }
}

pub(crate) fn item_from_row(row: &Row<'_>) -> StoryResultRow {
    let body: String = row.get(8)?;
    let mentions: Option<String> = row.get(9)?;
    let viewed_at: Option<i64> = row.get(10)?;
    let provider_viewed: bool = row.get(11)?;
    let expires: i64 = row.get(7)?;
    let story = Story {
        id: MessageId::new(row.get::<_, String>(1)?),
        client_id: row.get::<_, Option<String>>(2)?.map(ClientMessageId::new),
        account_id: AccountId::new(row.get::<_, String>(0)?),
        author: ContactId::new(row.get::<_, String>(3)?),
        author_name: row.get(4)?,
        mine: row.get(5)?,
        posted_at: Timestamp::from_millis(row.get(6)?),
        expires_at: Some(Timestamp::from_millis(expires)),
        body: serde_json::from_str(&body).unwrap_or(StoryBody::Text {
            text: String::new(),
            style: Default::default(),
        }),
        mentions: mentions
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default(),
        viewed: provider_viewed,
        view_count: row.get(12)?,
    };
    Ok(StoryItem {
        viewed: viewed_at.is_some() || provider_viewed,
        viewed_at: viewed_at.map(Timestamp::from_millis),
        story,
        post: None,
    })
}

type StoryResultRow = rusqlite::Result<StoryItem>;

/// The key that stands for a person under all of their ids: their number
/// when one of the ids is one, else the first id in order.
fn person_key(ids: &[ContactId], asked: &ContactId) -> String {
    let mut all: Vec<&str> = ids.iter().map(ContactId::as_str).collect();
    all.push(asked.as_str());
    all.sort_unstable();
    all.iter()
        .copied()
        .find(|id| crate::phone::is_e164(id))
        .unwrap_or(all[0])
        .to_owned()
}

fn muted_ids(conn: &Connection, account: &str) -> StoreResult<HashSet<String>> {
    let mut stmt = conn.prepare_cached("SELECT author FROM story_mutes WHERE account_id = ?1")?;
    let rows = stmt.query_map(params![account], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The cache keys of the media a story's rows used (the thumbnail, the
/// file, the animation, the local copy of one posted from here).
fn cache_keys(body: &StoryBody, client_id: Option<&str>) -> Vec<String> {
    let mut keys = Vec::new();
    if let StoryBody::Media(media) = body {
        if let Some(url) = media.source.as_ref().map(|source| source.as_str()) {
            keys.push(crate::sync::thumbnail_key(url));
            keys.push(crate::sync::file_key(url));
            keys.push(crate::sync::animation_key(url));
        }
    }
    if let Some(client_id) = client_id {
        keys.push(format!("thumb:local:{client_id}"));
        keys.push(format!("file:local:{client_id}"));
    }
    keys
}

impl Store {
    /// Adds or updates a story. A story that has already expired at `now`
    /// is not kept (and one that was is forgotten). Returns whether the
    /// story is new here.
    ///
    /// What was recorded about it here (when it was shown) is kept; the
    /// provider saying the account saw it elsewhere is added to it.
    pub fn upsert_story(&self, story: &Story, now: Timestamp) -> StoreResult<bool> {
        let inserted = self.write(|tx| upsert_tx(tx, story, now))?;
        self.notify(StoreChange::Stories {
            account_id: story.account_id.clone(),
        });
        Ok(inserted)
    }

    /// Replaces what the provider listed: the stories in `listed` are
    /// added or updated, and the ones of the account that the provider no
    /// longer lists are dropped (its stories on their way out are not
    /// touched). Returns how many are new here.
    pub fn sync_stories(
        &self,
        account: &AccountId,
        listed: &[Story],
        now: Timestamp,
    ) -> StoreResult<usize> {
        let new = self.write(|tx| {
            let mut new = 0;
            let mut keep: HashSet<&str> = HashSet::new();
            for story in listed {
                if upsert_tx(tx, story, now)? {
                    new += 1;
                }
                keep.insert(story.id.as_str());
            }
            let mut stmt = tx.prepare("SELECT id FROM stories WHERE account_id = ?1")?;
            let held: Vec<String> = stmt
                .query_map(params![account.as_str()], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            drop(stmt);
            for id in held {
                if !keep.contains(id.as_str()) {
                    delete_tx(tx, account.as_str(), &id)?;
                }
            }
            Ok(new)
        })?;
        self.notify(StoreChange::Stories {
            account_id: account.clone(),
        });
        Ok(new)
    }

    /// Forgets a story, its viewers and its cached media. Returns whether
    /// it was there.
    pub fn remove_story(&self, account: &AccountId, story: &MessageId) -> StoreResult<bool> {
        let gone = self.write(|tx| delete_tx(tx, account.as_str(), story.as_str()))?;
        if gone {
            self.notify(StoreChange::Stories {
                account_id: account.clone(),
            });
        }
        Ok(gone)
    }

    /// One story, with whether it was seen.
    pub fn story(&self, account: &AccountId, id: &MessageId) -> StoreResult<Option<StoryItem>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM stories s WHERE s.account_id = ?1 AND s.id = ?2"
                    ),
                    params![account.as_str(), id.as_str()],
                    item_from_row,
                )
                .optional()?)
        })
    }

    /// Records that the story was shown to the user. Returns `true` the
    /// first time only: that is when a receipt may be owed.
    pub fn mark_story_viewed(
        &self,
        account: &AccountId,
        id: &MessageId,
        now: Timestamp,
    ) -> StoreResult<bool> {
        let first = self.write(|tx| {
            Ok(tx.execute(
                "UPDATE stories SET viewed_at = ?3
                 WHERE account_id = ?1 AND id = ?2 AND viewed_at IS NULL",
                params![account.as_str(), id.as_str(), now.as_millis()],
            )? == 1)
        })?;
        if first {
            self.notify(StoreChange::Stories {
                account_id: account.clone(),
            });
        }
        Ok(first)
    }

    /// What the Status view lists at `now`.
    pub fn story_feed(&self, account: &AccountId, now: Timestamp) -> StoreResult<StoryFeed> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT {COLUMNS} FROM stories s
                 WHERE s.account_id = ?1 AND s.expires_at > ?2
                 ORDER BY s.posted_at, s.id"
            ))?;
            let items: Vec<StoryItem> = stmt
                .query_map(params![account.as_str(), now.as_millis()], item_from_row)?
                .collect::<Result<_, _>>()?;
            drop(stmt);

            let mut feed = StoryFeed::default();
            let muted = muted_ids(conn, account.as_str())?;
            let mut people = People::new(conn);
            let mut authors: Vec<StoryAuthor> = Vec::new();
            let mut at: HashMap<String, usize> = HashMap::new();
            for item in items {
                if item.story.mine {
                    feed.mine.push(item);
                    continue;
                }
                let author = item.story.author.clone();
                let person = people.person(account.as_str(), "", author.as_str())?;
                let key = person_key(&person.ids, &author);
                let slot = *at.entry(key.clone()).or_insert_with(|| {
                    let named = person.name.as_ref().map(|(name, _)| name.clone());
                    let named = named.or_else(|| item.story.author_name.clone());
                    authors.push(StoryAuthor {
                        key,
                        author: author.clone(),
                        ids: if person.ids.is_empty() {
                            vec![author.clone()]
                        } else {
                            person.ids.clone()
                        },
                        name: named,
                        phone: person.phone.clone(),
                        stories: Vec::new(),
                        muted: false,
                    });
                    authors.len() - 1
                });
                authors[slot].author = author;
                authors[slot].stories.push(item);
            }
            for mut author in authors {
                author.muted = author.ids.iter().any(|id| muted.contains(id.as_str()));
                let list = if author.muted {
                    &mut feed.muted
                } else if author.unviewed() > 0 {
                    &mut feed.recent
                } else {
                    &mut feed.viewed
                };
                list.push(author);
            }
            for list in [&mut feed.recent, &mut feed.viewed, &mut feed.muted] {
                list.sort_by(|a, b| {
                    b.latest_at()
                        .cmp(&a.latest_at())
                        .then_with(|| a.key.cmp(&b.key))
                });
            }
            // Posts that are still on their way out are the account's too.
            feed.mine
                .extend(super::story_posts::pending_items(conn, account)?);
            feed.mine.sort_by_key(|item| item.story.posted_at);
            Ok(feed)
        })
    }

    /// The rings around people's pictures: for each id a person with
    /// unexpired stories goes by, how many they have and how many are not
    /// seen. Muted authors have none.
    pub fn story_rings(
        &self,
        account: &AccountId,
        now: Timestamp,
    ) -> StoreResult<HashMap<ContactId, StoryRing>> {
        let feed = self.story_feed(account, now)?;
        let mut rings = HashMap::new();
        for author in feed.recent.iter().chain(feed.viewed.iter()) {
            let ring = StoryRing {
                total: author.stories.len(),
                unviewed: author.unviewed(),
            };
            for id in &author.ids {
                rings.insert(id.clone(), ring);
            }
        }
        Ok(rings)
    }

    /// How many stories of other people have not been seen, for the
    /// dot on the Status button.
    pub fn unviewed_story_count(&self, account: &AccountId, now: Timestamp) -> StoreResult<usize> {
        Ok(self.story_feed(account, now)?.unviewed())
    }

    // ----- viewers ------------------------------------------------------

    /// Replaces who saw one of the account's own stories.
    pub fn put_story_viewers(
        &self,
        account: &AccountId,
        story: &MessageId,
        viewers: &[StoryViewer],
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM story_viewers WHERE account_id = ?1 AND story_id = ?2",
                params![account.as_str(), story.as_str()],
            )?;
            for viewer in viewers {
                put_viewer_tx(tx, account.as_str(), story.as_str(), viewer)?;
            }
            tx.execute(
                "UPDATE stories SET view_count = ?3 WHERE account_id = ?1 AND id = ?2",
                params![account.as_str(), story.as_str(), viewers.len() as i64],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Stories {
            account_id: account.clone(),
        });
        Ok(())
    }

    /// Adds one viewer (or updates theirs) and counts them.
    pub fn add_story_viewer(
        &self,
        account: &AccountId,
        story: &MessageId,
        viewer: &StoryViewer,
    ) -> StoreResult<()> {
        self.write(|tx| {
            put_viewer_tx(tx, account.as_str(), story.as_str(), viewer)?;
            tx.execute(
                "UPDATE stories SET view_count =
                    (SELECT COUNT(*) FROM story_viewers WHERE account_id = ?1 AND story_id = ?2)
                 WHERE account_id = ?1 AND id = ?2",
                params![account.as_str(), story.as_str()],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Stories {
            account_id: account.clone(),
        });
        Ok(())
    }

    /// Who saw one of the account's own stories, the newest first, with
    /// the names every view calls them by.
    pub fn story_viewers(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> StoreResult<Vec<StoryViewer>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT contact, name, viewed_at, reaction FROM story_viewers
                 WHERE account_id = ?1 AND story_id = ?2 ORDER BY viewed_at DESC, contact",
            )?;
            let rows: Vec<StoryViewer> = stmt
                .query_map(params![account.as_str(), story.as_str()], |row| {
                    Ok(StoryViewer {
                        contact: ContactId::new(row.get::<_, String>(0)?),
                        name: row.get(1)?,
                        viewed_at: Timestamp::from_millis(row.get(2)?),
                        reaction: row.get(3)?,
                    })
                })?
                .collect::<Result<_, _>>()?;
            drop(stmt);
            let mut people = People::new(conn);
            let mut out = Vec::with_capacity(rows.len());
            for mut viewer in rows {
                let person = people.person(account.as_str(), "", viewer.contact.as_str())?;
                if let Some((name, _)) = person.name {
                    viewer.name = Some(name);
                }
                out.push(viewer);
            }
            Ok(out)
        })
    }

    // ----- mutes --------------------------------------------------------

    /// Mutes or unmutes an author's stories (every id they go by).
    pub fn set_story_muted(
        &self,
        account: &AccountId,
        author: &ContactId,
        muted: bool,
    ) -> StoreResult<()> {
        self.write(|tx| {
            let mut ids = vec![author.as_str().to_owned()];
            {
                let mut people = People::new(tx);
                let person = people.person(account.as_str(), "", author.as_str())?;
                ids.extend(person.ids.iter().map(|id| id.as_str().to_owned()));
            }
            ids.sort();
            ids.dedup();
            for id in ids {
                if muted {
                    tx.execute(
                        "INSERT OR IGNORE INTO story_mutes (account_id, author) VALUES (?1, ?2)",
                        params![account.as_str(), id],
                    )?;
                } else {
                    tx.execute(
                        "DELETE FROM story_mutes WHERE account_id = ?1 AND author = ?2",
                        params![account.as_str(), id],
                    )?;
                }
            }
            Ok(())
        })?;
        self.notify(StoreChange::Stories {
            account_id: account.clone(),
        });
        Ok(())
    }

    /// Takes what the provider says is muted as the whole truth.
    pub fn put_muted_story_authors(
        &self,
        account: &AccountId,
        authors: &[ContactId],
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM story_mutes WHERE account_id = ?1",
                params![account.as_str()],
            )?;
            for author in authors {
                tx.execute(
                    "INSERT OR IGNORE INTO story_mutes (account_id, author) VALUES (?1, ?2)",
                    params![account.as_str(), author.as_str()],
                )?;
            }
            Ok(())
        })?;
        self.notify(StoreChange::Stories {
            account_id: account.clone(),
        });
        Ok(())
    }

    /// Whether an author's stories are muted.
    pub fn story_muted(&self, account: &AccountId, author: &ContactId) -> StoreResult<bool> {
        self.read(|conn| {
            let muted = muted_ids(conn, account.as_str())?;
            let mut people = People::new(conn);
            let person = people.person(account.as_str(), "", author.as_str())?;
            Ok(muted.contains(author.as_str())
                || person.ids.iter().any(|id| muted.contains(id.as_str())))
        })
    }

    // ----- audience -----------------------------------------------------

    /// The audience the provider last answered, with when it did.
    pub fn cached_story_privacy(
        &self,
        account: &AccountId,
    ) -> StoreResult<Option<(StoryPrivacy, Timestamp)>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT privacy, fetched_at FROM story_privacy WHERE account_id = ?1",
                    params![account.as_str()],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
                .and_then(|(json, at)| {
                    serde_json::from_str(&json)
                        .ok()
                        .map(|privacy| (privacy, Timestamp::from_millis(at)))
                }))
        })
    }

    /// Keeps the audience the provider answered, or the one the user just
    /// set.
    pub fn put_story_privacy(
        &self,
        account: &AccountId,
        privacy: &StoryPrivacy,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO story_privacy (account_id, privacy, fetched_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account_id) DO UPDATE SET
                    privacy = excluded.privacy, fetched_at = excluded.fetched_at",
                params![
                    account.as_str(),
                    serde_json::to_string(privacy)?,
                    now.as_millis()
                ],
            )?;
            Ok(())
        })?;
        self.notify(StoreChange::Stories {
            account_id: account.clone(),
        });
        Ok(())
    }

    // ----- expiry -------------------------------------------------------

    /// Forgets the stories that have expired at `now`, with their viewers
    /// and the media cached for them. Returns how many went.
    ///
    /// A reply to a story keeps what it said about it; only the story is
    /// gone ("story unavailable").
    pub fn expire_stories(&self, now: Timestamp) -> StoreResult<usize> {
        let (gone, accounts) = self.write(|tx| {
            let mut stmt =
                tx.prepare("SELECT account_id, id FROM stories WHERE expires_at <= ?1")?;
            let expired: Vec<(String, String)> = stmt
                .query_map(params![now.as_millis()], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?
                .collect::<Result<_, _>>()?;
            drop(stmt);
            let mut accounts = HashSet::new();
            for (account, id) in &expired {
                delete_tx(tx, account, id)?;
                accounts.insert(account.clone());
            }
            // Receipts for stories that are gone cannot be sent.
            tx.execute(
                "DELETE FROM story_receipts WHERE NOT EXISTS
                    (SELECT 1 FROM stories s
                     WHERE s.account_id = story_receipts.account_id
                       AND s.id = story_receipts.story_id)",
                [],
            )?;
            Ok((expired.len(), accounts))
        })?;
        for account in accounts {
            self.notify(StoreChange::Stories {
                account_id: AccountId::new(account),
            });
        }
        Ok(gone)
    }

    /// The soonest moment a story expires, for the timer that cleans up.
    pub fn next_story_expiry(&self) -> StoreResult<Option<Timestamp>> {
        self.read(|conn| {
            Ok(conn
                .query_row("SELECT MIN(expires_at) FROM stories", [], |row| {
                    row.get::<_, Option<i64>>(0)
                })?
                .map(Timestamp::from_millis))
        })
    }

    // ----- receipts owed ------------------------------------------------

    /// Notes that the author is owed a view receipt for a story.
    pub fn owe_story_receipt(
        &self,
        account: &AccountId,
        story: &MessageId,
        author: &ContactId,
        now: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO story_receipts
                    (account_id, story_id, author, attempts, next_attempt_at, created_at)
                 VALUES (?1, ?2, ?3, 0, ?4, ?4)",
                params![
                    account.as_str(),
                    story.as_str(),
                    author.as_str(),
                    now.as_millis()
                ],
            )?;
            Ok(())
        })
    }

    /// The receipts that are due at `now`, oldest first.
    pub fn story_receipts_due(&self, now: Timestamp) -> StoreResult<Vec<StoryReceipt>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT account_id, story_id, author, attempts, next_attempt_at
                 FROM story_receipts WHERE next_attempt_at <= ?1
                 ORDER BY created_at, story_id",
            )?;
            let rows = stmt
                .query_map(params![now.as_millis()], receipt_from_row)?
                .collect::<Result<_, _>>()?;
            Ok(rows)
        })
    }

    /// Every receipt still owed.
    pub fn story_receipts_owed(&self) -> StoreResult<Vec<StoryReceipt>> {
        self.story_receipts_due(Timestamp::from_millis(i64::MAX))
    }

    /// When the next receipt is due.
    pub fn next_story_receipt(&self) -> StoreResult<Option<Timestamp>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT MIN(next_attempt_at) FROM story_receipts",
                    [],
                    |row| row.get::<_, Option<i64>>(0),
                )?
                .map(Timestamp::from_millis))
        })
    }

    /// The receipt was sent (or can never be): it is owed no more.
    pub fn story_receipt_done(&self, account: &AccountId, story: &MessageId) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM story_receipts WHERE account_id = ?1 AND story_id = ?2",
                params![account.as_str(), story.as_str()],
            )?;
            Ok(())
        })
    }

    /// The receipt did not get through: it is tried again at `next`.
    pub fn story_receipt_retry(
        &self,
        account: &AccountId,
        story: &MessageId,
        next: Timestamp,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE story_receipts SET attempts = attempts + 1, next_attempt_at = ?3
                 WHERE account_id = ?1 AND story_id = ?2",
                params![account.as_str(), story.as_str(), next.as_millis()],
            )?;
            Ok(())
        })
    }
}

fn receipt_from_row(row: &Row<'_>) -> rusqlite::Result<StoryReceipt> {
    Ok(StoryReceipt {
        account_id: AccountId::new(row.get::<_, String>(0)?),
        story: MessageId::new(row.get::<_, String>(1)?),
        author: ContactId::new(row.get::<_, String>(2)?),
        attempts: row.get(3)?,
        next_attempt_at: Timestamp::from_millis(row.get(4)?),
    })
}

fn put_viewer_tx(
    tx: &rusqlite::Transaction<'_>,
    account: &str,
    story: &str,
    viewer: &StoryViewer,
) -> StoreResult<()> {
    tx.execute(
        "INSERT INTO story_viewers (account_id, story_id, contact, name, viewed_at, reaction)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (account_id, story_id, contact) DO UPDATE SET
            name = COALESCE(excluded.name, name),
            viewed_at = MIN(viewed_at, excluded.viewed_at),
            reaction = COALESCE(excluded.reaction, reaction)",
        params![
            account,
            story,
            viewer.contact.as_str(),
            viewer.name,
            viewer.viewed_at.as_millis(),
            viewer.reaction,
        ],
    )?;
    Ok(())
}

/// Adds or updates a story inside a transaction. `true`: it is new.
fn upsert_tx(tx: &rusqlite::Transaction<'_>, story: &Story, now: Timestamp) -> StoreResult<bool> {
    let account = story.account_id.as_str();
    if story.expiry() <= now {
        delete_tx(tx, account, story.id.as_str())?;
        return Ok(false);
    }
    // The story that was posted from here comes back with its provider's
    // id: it takes the place of the local one.
    if let Some(client) = &story.client_id {
        super::story_posts::settle_tx(tx, account, client.as_str())?;
        // A copy under another id (the event came before the answer to
        // the post) is the same story.
        tx.execute(
            "DELETE FROM stories WHERE account_id = ?1 AND client_id = ?2 AND id <> ?3",
            params![account, client.as_str(), story.id.as_str()],
        )?;
    }
    let held: Option<String> = tx
        .query_row(
            "SELECT body FROM stories WHERE account_id = ?1 AND id = ?2",
            params![account, story.id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let existed = held.is_some();
    // A provider that cannot read a text story's style back lists it with
    // the defaults: the style this client posted it with is kept, as long
    // as the words are the same.
    let body = match (
        &story.body,
        held.and_then(|json| serde_json::from_str(&json).ok()),
    ) {
        (
            StoryBody::Text { text, style },
            Some(StoryBody::Text {
                text: known,
                style: kept,
            }),
        ) if style == &client_provider::StoryStyle::default() && text == &known => {
            StoryBody::Text {
                text: known,
                style: kept,
            }
        }
        _ => story.body.clone(),
    };
    tx.execute(
        "INSERT INTO stories
            (account_id, id, client_id, author, author_name, mine, posted_at, expires_at, kind,
             body, mentions, viewed_at, provider_viewed, view_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, NULL, ?12, ?13)
         ON CONFLICT (account_id, id) DO UPDATE SET
            client_id = COALESCE(excluded.client_id, client_id),
            author_name = COALESCE(excluded.author_name, author_name),
            expires_at = excluded.expires_at,
            body = excluded.body,
            mentions = excluded.mentions,
            provider_viewed = provider_viewed OR excluded.provider_viewed,
            view_count = COALESCE(excluded.view_count, view_count)",
        params![
            account,
            story.id.as_str(),
            story.client_id.as_ref().map(ClientMessageId::as_str),
            story.author.as_str(),
            story.author_name,
            story.mine,
            story.posted_at.as_millis(),
            story.expiry().as_millis(),
            kind_of(&story.body),
            serde_json::to_string(&body)?,
            (!story.mentions.is_empty())
                .then(|| serde_json::to_string(&story.mentions))
                .transpose()?,
            story.viewed,
            story.view_count,
        ],
    )?;
    // Bounded: the oldest beyond the cap go.
    tx.execute(
        "DELETE FROM stories WHERE account_id = ?1 AND id IN
            (SELECT id FROM stories WHERE account_id = ?1
             ORDER BY posted_at DESC, id LIMIT -1 OFFSET ?2)",
        params![account, STORIES_KEPT as i64],
    )?;
    Ok(!existed)
}

/// Removes a story, its viewers, and the media cached for it.
fn delete_tx(tx: &rusqlite::Transaction<'_>, account: &str, id: &str) -> StoreResult<bool> {
    let row: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT body, client_id FROM stories WHERE account_id = ?1 AND id = ?2",
            params![account, id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((body, client)) = row else {
        return Ok(false);
    };
    if let Ok(body) = serde_json::from_str::<StoryBody>(&body) {
        for key in cache_keys(&body, client.as_deref()) {
            tx.execute("DELETE FROM media_cache WHERE key = ?1", params![key])?;
        }
    }
    tx.execute(
        "DELETE FROM story_viewers WHERE account_id = ?1 AND story_id = ?2",
        params![account, id],
    )?;
    tx.execute(
        "DELETE FROM story_receipts WHERE account_id = ?1 AND story_id = ?2",
        params![account, id],
    )?;
    tx.execute(
        "DELETE FROM stories WHERE account_id = ?1 AND id = ?2",
        params![account, id],
    )?;
    Ok(true)
}

/// What an account being removed leaves behind.
pub(crate) fn remove_account_tx(tx: &rusqlite::Transaction<'_>, account: &str) -> StoreResult<()> {
    tx.execute(
        "DELETE FROM story_post_files WHERE client_id IN
            (SELECT client_id FROM story_posts WHERE account_id = ?1)",
        params![account],
    )?;
    for table in [
        "stories",
        "story_viewers",
        "story_mutes",
        "story_privacy",
        "story_posts",
        "story_receipts",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE account_id = ?1"),
            params![account],
        )?;
    }
    Ok(())
}
