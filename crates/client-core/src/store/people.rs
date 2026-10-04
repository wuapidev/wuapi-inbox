//! People in the store: one person, several ids (schema v7).
//!
//! WhatsApp names the same person by their phone number in one place and
//! by a hidden-number id in another: a contact of the address book, the
//! sender of a group message, a participant, somebody mentioned. The
//! table `identities` keeps the ids of one person together, as the
//! provider's address book says they belong; `seen_names` keeps the name
//! each sender's own messages came with. Everything that shows a name asks
//! [`People`], so a person reads the same everywhere:
//!
//! 1. the name saved in the address book (or a business's verified name),
//! 2. the name the group's participant list gives,
//! 3. the name on the person's own messages (their WhatsApp profile name),
//! 4. their username,
//! 5. their phone number, as people write it,
//!
//! and when none of these is known, nothing: the caller shows a neutral
//! placeholder, never the digits of an id.

use super::{Store, StoreChange, StoreResult};
use client_provider::{AccountId, ChatId, ContactId, Direction, Message, MessageContent};
use rusqlite::{params, Connection, Transaction};
use std::collections::HashMap;

/// Where a person's name comes from, best first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameSource {
    /// Saved in the address book, or a business's verified name.
    Saved,
    /// What the group's participant list calls them.
    Group,
    /// The profile name their own messages came with.
    Profile,
    /// Their WhatsApp username.
    Username,
    /// Their phone number.
    Phone,
}

/// A person as the store knows them, under whichever id they were asked
/// for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// The name to show and where it comes from; `None` when nothing is
    /// known but the id.
    pub name: Option<(String, NameSource)>,
    /// Their phone number in E.164, when known.
    pub phone: Option<String>,
    /// This is the account itself.
    pub me: bool,
    /// Every id the person goes by, the one asked for included.
    pub ids: Vec<ContactId>,
}

impl Person {
    /// The name to show, when there is one.
    pub fn shown(&self) -> Option<&str> {
        self.name.as_ref().map(|(name, _)| name.as_str())
    }
}

/// Keeps `ids` together as the ids of one person, and of nobody else: an
/// id that was kept with others before leaves them.
pub(crate) fn link_ids_tx(tx: &Transaction<'_>, account: &str, ids: &[&str]) -> StoreResult<()> {
    let mut ids: Vec<&str> = ids.iter().copied().filter(|id| !id.is_empty()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() < 2 {
        return Ok(());
    }
    // The number, when one of the ids is one: it reads best as the key.
    let person = ids
        .iter()
        .copied()
        .find(|id| crate::phone::is_e164(id))
        .unwrap_or(ids[0]);
    for id in &ids {
        tx.execute(
            "DELETE FROM identities WHERE account_id = ?1 AND id = ?2",
            params![account, id],
        )?;
    }
    // Whoever else was kept under that key was kept there through an id
    // that has just left: each stands alone again.
    tx.execute(
        "UPDATE identities SET person = id WHERE account_id = ?1 AND person = ?2",
        params![account, person],
    )?;
    for id in &ids {
        tx.execute(
            "INSERT INTO identities (account_id, id, person) VALUES (?1, ?2, ?3)",
            params![account, id, person],
        )?;
    }
    Ok(())
}

/// Remembers the name an incoming message came with, as the name of its
/// sender: the newest message decides.
pub(crate) fn note_sender_tx(tx: &Transaction<'_>, message: &Message) -> StoreResult<()> {
    if message.direction != Direction::Incoming {
        return Ok(());
    }
    let filled = |text: &Option<String>| {
        text.as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    let name = filled(&message.sender_name);
    let username = filled(&message.extras.sender_username);
    if name.is_none() && username.is_none() {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO seen_names (account_id, id, profile_name, username, seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (account_id, id) DO UPDATE SET
            profile_name = CASE WHEN excluded.seen_at >= seen_at
                                THEN COALESCE(excluded.profile_name, profile_name)
                                ELSE COALESCE(profile_name, excluded.profile_name) END,
            username = CASE WHEN excluded.seen_at >= seen_at
                            THEN COALESCE(excluded.username, username)
                            ELSE COALESCE(username, excluded.username) END,
            seen_at = MAX(seen_at, excluded.seen_at)",
        params![
            message.account_id.as_str(),
            message.sender.as_str(),
            name,
            username,
            message.timestamp.as_millis(),
        ],
    )?;
    Ok(())
}

/// The ids an account's own messages and memberships go by.
const MY_IDS: &str = "(SELECT self_contact FROM accounts
                       WHERE id = ?1 AND self_contact IS NOT NULL
                       UNION SELECT phone FROM accounts WHERE id = ?1 AND phone IS NOT NULL)";

/// SQL that is true when `column` names somebody: it is not empty, not
/// a phone number standing in for a name, and not the row's own id.
fn names(column: &str, id: &str) -> String {
    format!(
        "{column} IS NOT NULL AND TRIM({column}) <> '' AND {column} NOT GLOB '+[0-9]*' \
         AND {column} <> {id}"
    )
}

/// Names people for one read of the store, asking about each only once.
pub(crate) struct People<'c> {
    conn: &'c Connection,
    known: HashMap<(String, String, String), Person>,
}

impl<'c> People<'c> {
    pub(crate) fn new(conn: &'c Connection) -> Self {
        Self {
            conn,
            known: HashMap::new(),
        }
    }

    /// The connection this reads through.
    pub(crate) fn conn(&self) -> &'c Connection {
        self.conn
    }

    /// Who `id` is, as seen from `chat` (a group names its participants).
    pub(crate) fn person(&mut self, account: &str, chat: &str, id: &str) -> StoreResult<Person> {
        let key = (account.to_owned(), chat.to_owned(), id.to_owned());
        if let Some(known) = self.known.get(&key) {
            return Ok(known.clone());
        }
        let person = self.read(account, chat, id)?;
        self.known.insert(key, person.clone());
        Ok(person)
    }

    fn read(&self, account: &str, chat: &str, id: &str) -> StoreResult<Person> {
        let sql = format!(
            "WITH ids (id) AS (
                 SELECT ?3
                 UNION
                 SELECT i.id FROM identities i
                 WHERE i.account_id = ?1
                   AND i.person = (SELECT person FROM identities
                                   WHERE account_id = ?1 AND id = ?3))
             SELECT
                (SELECT COALESCE(NULLIF(TRIM(saved_name), ''), NULLIF(TRIM(business_name), ''))
                 FROM contacts
                 WHERE account_id = ?1 AND id IN (SELECT id FROM ids)
                   AND COALESCE(NULLIF(TRIM(saved_name), ''), NULLIF(TRIM(business_name), ''))
                       IS NOT NULL
                 LIMIT 1),
                (SELECT TRIM(name) FROM group_participants
                 WHERE account_id = ?1 AND group_id = ?2
                   AND contact_id IN (SELECT id FROM ids) AND {participant}
                 LIMIT 1),
                (SELECT TRIM(profile_name) FROM seen_names
                 WHERE account_id = ?1 AND id IN (SELECT id FROM ids) AND {seen}
                 ORDER BY seen_at DESC LIMIT 1),
                (SELECT TRIM(profile_name) FROM contacts
                 WHERE account_id = ?1 AND id IN (SELECT id FROM ids) AND {profile}
                 LIMIT 1),
                (SELECT TRIM(name) FROM contacts
                 WHERE account_id = ?1 AND id IN (SELECT id FROM ids) AND {given}
                   AND name <> COALESCE(phone, '')
                 LIMIT 1),
                (SELECT TRIM(title) FROM chats
                 WHERE account_id = ?1 AND kind = 'direct'
                   AND id IN (SELECT id FROM ids) AND {title}
                 LIMIT 1),
                (SELECT TRIM(username) FROM contacts
                 WHERE account_id = ?1 AND id IN (SELECT id FROM ids)
                   AND TRIM(COALESCE(username, '')) <> ''
                 LIMIT 1),
                (SELECT TRIM(username) FROM seen_names
                 WHERE account_id = ?1 AND id IN (SELECT id FROM ids)
                   AND TRIM(COALESCE(username, '')) <> ''
                 ORDER BY seen_at DESC LIMIT 1),
                (SELECT TRIM(phone) FROM contacts
                 WHERE account_id = ?1 AND id IN (SELECT id FROM ids)
                   AND TRIM(COALESCE(phone, '')) <> ''
                 LIMIT 1),
                EXISTS (SELECT 1 FROM ids WHERE id IN {MY_IDS}),
                (SELECT group_concat(id, char(10)) FROM ids)",
            participant = names("name", "contact_id"),
            seen = names("profile_name", "id"),
            profile = names("profile_name", "id"),
            given = names("name", "id"),
            title = names("title", "id"),
        );
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let text = |row: &rusqlite::Row<'_>, at: usize| row.get::<_, Option<String>>(at);
        stmt.query_row(params![account, chat, id], |row| {
            let ids: Vec<String> = text(row, 10)?
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            let phone =
                text(row, 8)?.or_else(|| ids.iter().find(|id| crate::phone::is_e164(id)).cloned());
            let name = None
                .or(text(row, 0)?.map(|name| (name, NameSource::Saved)))
                .or(text(row, 1)?.map(|name| (name, NameSource::Group)))
                .or(text(row, 2)?.map(|name| (name, NameSource::Profile)))
                .or(text(row, 3)?.map(|name| (name, NameSource::Profile)))
                .or(text(row, 4)?.map(|name| (name, NameSource::Profile)))
                .or(text(row, 5)?.map(|name| (name, NameSource::Profile)))
                .or(text(row, 6)?.map(|name| (name, NameSource::Username)))
                .or(text(row, 7)?.map(|name| (name, NameSource::Username)))
                .or(phone
                    .as_deref()
                    .map(|phone| (crate::phone::format(phone), NameSource::Phone)));
            Ok(Person {
                name,
                phone,
                me: row.get(9)?,
                ids: ids.into_iter().map(ContactId::new).collect(),
            })
        })
        .map_err(Into::into)
    }

    /// Names the people a message is about as everything else names
    /// them: its sender, the people its text mentions, who a notice of
    /// the chat is about.
    pub(crate) fn name_message(&mut self, message: &mut Message) -> StoreResult<()> {
        let (account, chat) = (message.account_id.to_string(), message.chat_id.to_string());
        if message.direction == Direction::Incoming {
            let sender = self.person(&account, &chat, message.sender.as_str())?;
            // The name the message came with stands in until more is
            // known; a number is the last thing a sender is called.
            let came_with = message
                .sender_name
                .take()
                .filter(|name| !name.trim().is_empty());
            message.sender_name = match sender.name {
                Some((name, source)) if source < NameSource::Username => Some(name),
                Some((name, _)) => came_with.or(Some(name)),
                None => came_with,
            };
        }
        self.name_mentions(message)?;
        self.name_parties(message)
    }

    /// Ties the mentions of a message to its text and names them.
    pub(crate) fn name_mentions(&mut self, message: &mut Message) -> StoreResult<()> {
        if message.extras.mentions.is_empty() {
            return Ok(());
        }
        let (account, chat) = (message.account_id.to_string(), message.chat_id.to_string());
        let mut candidates = Vec::with_capacity(message.extras.mentions.len());
        for mention in &mut message.extras.mentions {
            let person = self.person(&account, &chat, mention.id.as_str())?;
            candidates.push(
                person
                    .ids
                    .iter()
                    .map(client_provider::mention_handle)
                    .filter(|handle| !handle.is_empty())
                    .collect::<Vec<_>>(),
            );
            mention.me = person.me;
            // A name the provider sent with the mention stands until the
            // store knows the person by more than a username or a number.
            let came_with = mention.name.take().filter(|name| !name.trim().is_empty());
            mention.name = match person.name {
                Some((name, source)) if source <= NameSource::Profile => Some(name),
                Some((name, _)) => came_with.or(Some(name)),
                None => came_with,
            };
        }
        let text = match &message.content {
            MessageContent::Text { body } => body.as_str(),
            MessageContent::Media(media) => media.caption.as_deref().unwrap_or(""),
            _ => "",
        };
        crate::mentions::bind(text, &mut message.extras.mentions, &candidates);
        Ok(())
    }

    /// Names who a notice is about ("Ana added Luis").
    pub(crate) fn name_parties(&mut self, message: &mut Message) -> StoreResult<()> {
        let (account, chat) = (message.account_id.to_string(), message.chat_id.to_string());
        let MessageContent::System(event) = &mut message.content else {
            return Ok(());
        };
        for party in event.actor.iter_mut().chain(event.targets.iter_mut()) {
            let person = self.person(&account, &chat, party.id.as_str())?;
            let came_with = party.name.take().filter(|name| !name.trim().is_empty());
            party.name = match person.name {
                Some((name, source)) if source < NameSource::Profile => Some(name),
                Some((name, _)) => came_with.or(Some(name)),
                None => came_with,
            };
        }
        Ok(())
    }
}

impl Store {
    /// Who `id` is: their name by the order every view uses, their
    /// number, every id they go by. `chat` is the group they are looked
    /// at in, when there is one.
    pub fn person(
        &self,
        account: &AccountId,
        chat: Option<&ChatId>,
        id: &ContactId,
    ) -> StoreResult<Person> {
        self.read(|conn| {
            People::new(conn).person(
                account.as_str(),
                chat.map(ChatId::as_str).unwrap_or(""),
                id.as_str(),
            )
        })
    }

    /// Records that `ids` are one person's (see the module's notes).
    pub fn link_ids(&self, account: &AccountId, ids: &[ContactId]) -> StoreResult<()> {
        let ids: Vec<&str> = ids.iter().map(ContactId::as_str).collect();
        self.write(|tx| link_ids_tx(tx, account.as_str(), &ids))?;
        self.notify(StoreChange::Contacts {
            account_id: account.clone(),
        });
        Ok(())
    }
}
