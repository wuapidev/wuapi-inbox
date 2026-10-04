//! Profiles and groups in the store: a group's details and participants,
//! the requests to join it, the blocklist, business profiles and the
//! account's own profile (schema v4).
//!
//! All of it is a cache of what the provider said last, plus what the user
//! just changed and the provider has not answered yet.

use super::{Store, StoreChange, StoreResult};
use client_provider::{
    AccountId, BusinessProfile, ChatId, ContactId, Group, GroupChange, GroupParticipant, GroupRole,
    JoinRequest, OwnProfile, Subgroup, Timestamp,
};
use rusqlite::{params, OptionalExtension, Transaction};

/// A group as the store holds it. `group.participants` is empty: they are
/// read a screenful at a time with
/// [`Store::group_participants`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredGroup {
    /// The details.
    pub group: Group,
    /// How many participants it has.
    pub participant_count: usize,
    /// The account's own role, or `None` when it is not in the list: it
    /// left, or the provider names it by an id the account does not know
    /// as its own (then the role is simply not known).
    pub my_role: Option<GroupRole>,
    /// The invite link, once an admin asked for it.
    pub invite_link: Option<String>,
    /// The account left the group or was removed from it.
    pub departed: bool,
    /// When the provider was last asked.
    pub fetched_at: Timestamp,
}

/// A participant of a group, named for display.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredParticipant {
    /// Who.
    pub contact: ContactId,
    /// The name saved in the address book, else the one the provider gave,
    /// else the number, else the id.
    pub name: String,
    /// The name comes from the address book.
    pub saved: bool,
    /// The number, when the address book or the id says.
    pub phone: Option<String>,
    /// Member, admin or owner.
    pub role: GroupRole,
    /// This is the account itself.
    pub me: bool,
}

fn role_to_str(role: GroupRole) -> &'static str {
    match role {
        GroupRole::Member => "member",
        GroupRole::Admin => "admin",
        GroupRole::Owner => "owner",
    }
}

fn role_from_str(role: &str) -> GroupRole {
    match role {
        "owner" => GroupRole::Owner,
        "admin" => GroupRole::Admin,
        _ => GroupRole::Member,
    }
}

/// The ids an account's own messages and group memberships go by.
const MY_IDS: &str = "(SELECT self_contact FROM accounts
                       WHERE id = ?1 AND self_contact IS NOT NULL
                       UNION SELECT phone FROM accounts WHERE id = ?1 AND phone IS NOT NULL)";

fn put_group_tx(tx: &Transaction<'_>, group: &Group, now: Timestamp) -> StoreResult<bool> {
    let account = group.account_id.as_str();
    let subgroups = serde_json::to_string(&group.subgroups)?;
    tx.execute(
        "INSERT INTO groups
            (account_id, id, subject, description, owner, created_at, community, announce,
             locked, join_approval, members_can_add, subgroups, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (account_id, id) DO UPDATE SET
            subject = excluded.subject,
            description = excluded.description,
            owner = excluded.owner,
            created_at = COALESCE(excluded.created_at, created_at),
            community = excluded.community,
            announce = excluded.announce,
            locked = excluded.locked,
            -- A setting the provider cannot read keeps what was set here.
            join_approval = COALESCE(excluded.join_approval, join_approval),
            members_can_add = COALESCE(excluded.members_can_add, members_can_add),
            subgroups = excluded.subgroups,
            fetched_at = excluded.fetched_at",
        params![
            account,
            group.id.as_str(),
            group.subject,
            group.description.as_deref().filter(|text| !text.is_empty()),
            group.owner.as_ref().map(ContactId::as_str),
            group.created_at.map(Timestamp::as_millis),
            group.community,
            group.announce,
            group.locked,
            group.join_approval,
            group.members_can_add,
            subgroups,
            now.as_millis(),
        ],
    )?;
    tx.execute(
        "DELETE FROM group_participants WHERE account_id = ?1 AND group_id = ?2",
        params![account, group.id.as_str()],
    )?;
    for participant in &group.participants {
        put_participant_tx(tx, &group.account_id, &group.id, participant)?;
    }
    // In the list: in the group. Not finding the account in it proves
    // nothing (a provider may name it by an id the account does not know
    // as its own), so that never marks the group as left.
    tx.execute(
        &format!(
            "UPDATE groups SET departed = 0
             WHERE account_id = ?1 AND id = ?2 AND departed = 1
               AND EXISTS (SELECT 1 FROM group_participants
                           WHERE account_id = ?1 AND group_id = ?2 AND contact_id IN {MY_IDS})"
        ),
        params![account, group.id.as_str()],
    )?;
    // The chat goes by the group's subject.
    let renamed = tx.execute(
        "UPDATE chats SET title = ?3
         WHERE account_id = ?1 AND id = ?2 AND kind = 'group' AND title <> ?3 AND ?3 <> ''",
        params![account, group.id.as_str(), group.subject],
    )?;
    Ok(renamed > 0)
}

fn put_participant_tx(
    tx: &Transaction<'_>,
    account: &AccountId,
    group: &ChatId,
    participant: &GroupParticipant,
) -> StoreResult<()> {
    tx.execute(
        "INSERT INTO group_participants (account_id, group_id, contact_id, name, role)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (account_id, group_id, contact_id) DO UPDATE SET
            name = COALESCE(excluded.name, name), role = excluded.role",
        params![
            account.as_str(),
            group.as_str(),
            participant.contact.as_str(),
            participant
                .name
                .as_deref()
                .filter(|name| !name.trim().is_empty()),
            role_to_str(participant.role),
        ],
    )?;
    Ok(())
}

impl Store {
    fn social_changed(&self, account: &AccountId) {
        self.notify(StoreChange::Social {
            account_id: account.clone(),
        });
    }

    // ----- groups -----------------------------------------------------

    /// Stores groups as the provider just gave them, participants
    /// included, replacing what was held for each.
    pub fn put_groups(&self, groups: &[Group], now: Timestamp) -> StoreResult<()> {
        let mut renamed = Vec::new();
        self.write(|tx| {
            for group in groups {
                if put_group_tx(tx, group, now)? {
                    renamed.push(group.account_id.clone());
                }
            }
            Ok(())
        })?;
        renamed.dedup();
        for account in renamed {
            self.notify(StoreChange::Chats {
                account_id: account,
            });
        }
        let mut accounts: Vec<&AccountId> = groups.iter().map(|group| &group.account_id).collect();
        accounts.dedup();
        for account in accounts {
            self.social_changed(account);
        }
        Ok(())
    }

    /// Stores one group as the provider just gave it.
    pub fn put_group(&self, group: &Group, now: Timestamp) -> StoreResult<()> {
        self.put_groups(std::slice::from_ref(group), now)
    }

    /// What the store holds about a group, or `None` if the provider was
    /// never asked.
    pub fn group(&self, account: &AccountId, group: &ChatId) -> StoreResult<Option<StoredGroup>> {
        self.read(|conn| {
            let found = conn
                .query_row(
                    &format!(
                        "SELECT g.subject, g.description, g.owner, g.created_at, g.community,
                                g.announce, g.locked, g.join_approval, g.members_can_add,
                                g.subgroups, g.invite_link, g.departed, g.fetched_at,
                                (SELECT count(*) FROM group_participants p
                                 WHERE p.account_id = g.account_id AND p.group_id = g.id),
                                (SELECT p.role FROM group_participants p
                                 WHERE p.account_id = g.account_id AND p.group_id = g.id
                                   AND p.contact_id IN {MY_IDS} LIMIT 1)
                         FROM groups g WHERE g.account_id = ?1 AND g.id = ?2"
                    ),
                    params![account.as_str(), group.as_str()],
                    |row| {
                        Ok((
                            Group {
                                id: group.clone(),
                                account_id: account.clone(),
                                subject: row.get(0)?,
                                description: row.get(1)?,
                                owner: row.get::<_, Option<String>>(2)?.map(ContactId::new),
                                created_at: row
                                    .get::<_, Option<i64>>(3)?
                                    .map(Timestamp::from_millis),
                                community: row.get(4)?,
                                announce: row.get(5)?,
                                locked: row.get(6)?,
                                join_approval: row.get(7)?,
                                members_can_add: row.get(8)?,
                                participants: Vec::new(),
                                subgroups: Vec::new(),
                            },
                            row.get::<_, Option<String>>(9)?,
                            row.get::<_, Option<String>>(10)?,
                            row.get::<_, bool>(11)?,
                            row.get::<_, i64>(12)?,
                            row.get::<_, i64>(13)?,
                            row.get::<_, Option<String>>(14)?,
                        ))
                    },
                )
                .optional()?;
            let Some((mut details, subgroups, invite_link, departed, fetched_at, count, role)) =
                found
            else {
                return Ok(None);
            };
            if let Some(json) = subgroups {
                details.subgroups = serde_json::from_str::<Vec<Subgroup>>(&json)?;
            }
            Ok(Some(StoredGroup {
                group: details,
                participant_count: count as usize,
                my_role: role.as_deref().map(role_from_str).filter(|_| !departed),
                invite_link,
                departed,
                fetched_at: Timestamp::from_millis(fetched_at),
            }))
        })
    }

    /// The participants of a group: the account first, then the owner and
    /// the admins, then everyone by name. `query` keeps those whose name
    /// or number contains it.
    pub fn group_participants(
        &self,
        account: &AccountId,
        group: &ChatId,
        query: Option<&str>,
        limit: usize,
    ) -> StoreResult<Vec<StoredParticipant>> {
        let typed = query
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty());
        let pattern = typed.map(|q| {
            format!(
                "%{}%",
                q.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            )
        });
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(&format!(
                "SELECT p.contact_id, p.role,
                        COALESCE(NULLIF(TRIM(k.saved_name), ''), NULLIF(TRIM(k.business_name), '')),
                        COALESCE(NULLIF(TRIM(p.name), ''),
                                 (SELECT NULLIF(TRIM(s.profile_name), '') FROM seen_names s
                                  WHERE s.account_id = p.account_id AND s.id = p.contact_id),
                                 NULLIF(TRIM(k.profile_name), ''),
                                 NULLIF(TRIM(k.name), '')),
                        k.phone,
                        p.contact_id IN {MY_IDS} AS me
                 FROM group_participants p
                 -- The address book's row for the participant: under the
                 -- id the group lists, or under another id of the same
                 -- person (their number, for a hidden-number id).
                 LEFT JOIN contacts k ON k.account_id = p.account_id AND k.id = COALESCE(
                     (SELECT c.id FROM contacts c
                      WHERE c.account_id = p.account_id AND c.id = p.contact_id),
                     (SELECT c.id FROM contacts c
                      JOIN identities i ON i.account_id = c.account_id AND i.id = c.id
                      WHERE c.account_id = p.account_id
                        AND i.person = (SELECT person FROM identities
                                        WHERE account_id = p.account_id AND id = p.contact_id)
                      LIMIT 1))
                 WHERE p.account_id = ?1 AND p.group_id = ?2
                   AND (?3 IS NULL
                        OR lower(COALESCE(k.saved_name, '') || ' ' || COALESCE(k.business_name, '')
                                 || ' ' || COALESCE(p.name, '') || ' '
                                 || COALESCE(k.profile_name, '') || ' ' || COALESCE(k.phone, '')
                                 || ' ' || p.contact_id) LIKE ?3 ESCAPE '\\')
                 ORDER BY me DESC,
                          CASE p.role WHEN 'owner' THEN 0 WHEN 'admin' THEN 1 ELSE 2 END,
                          lower(COALESCE(NULLIF(TRIM(k.saved_name), ''), NULLIF(TRIM(p.name), ''),
                                         k.phone, p.contact_id)),
                          p.contact_id
                 LIMIT ?4"
            ))?;
            let mut rows = stmt.query(params![
                account.as_str(),
                group.as_str(),
                pattern,
                limit as i64
            ])?;
            let mut participants = Vec::new();
            while let Some(row) = rows.next()? {
                let id: String = row.get(0)?;
                let saved: Option<String> = row.get(2)?;
                let given: Option<String> = row.get(3)?;
                let phone = row
                    .get::<_, Option<String>>(4)?
                    .or_else(|| id.starts_with('+').then(|| id.clone()));
                participants.push(StoredParticipant {
                    name: saved
                        .clone()
                        .or(given)
                        .or_else(|| phone.clone())
                        .unwrap_or_else(|| id.clone()),
                    saved: saved.is_some(),
                    phone,
                    role: role_from_str(&row.get::<_, String>(1)?),
                    me: row.get(5)?,
                    contact: ContactId::new(id),
                });
            }
            Ok(participants)
        })
    }

    /// One participant's role, or `None` when they are not in the group.
    pub fn participant_role(
        &self,
        account: &AccountId,
        group: &ChatId,
        contact: &ContactId,
    ) -> StoreResult<Option<GroupRole>> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT role FROM group_participants
                     WHERE account_id = ?1 AND group_id = ?2 AND contact_id = ?3",
                    params![account.as_str(), group.as_str(), contact.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .map(|role| role_from_str(&role)))
        })
    }

    /// Puts a participant in a group, or changes their role. `None` takes
    /// them out.
    pub fn set_participant(
        &self,
        account: &AccountId,
        group: &ChatId,
        contact: &ContactId,
        role: Option<GroupRole>,
    ) -> StoreResult<()> {
        self.write(|tx| {
            match role {
                Some(role) => put_participant_tx(
                    tx,
                    account,
                    group,
                    &GroupParticipant {
                        contact: contact.clone(),
                        name: None,
                        role,
                    },
                )?,
                None => {
                    tx.execute(
                        "DELETE FROM group_participants
                         WHERE account_id = ?1 AND group_id = ?2 AND contact_id = ?3",
                        params![account.as_str(), group.as_str(), contact.as_str()],
                    )?;
                }
            }
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    /// Applies a change to a group's details, and answers the change that
    /// puts the old value back (`None` when the group is not stored, or
    /// the old value was not known).
    pub fn apply_group_change(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: &GroupChange,
    ) -> StoreResult<Option<GroupChange>> {
        let Some(stored) = self.group(account, group)? else {
            return Ok(None);
        };
        let was = &stored.group;
        let (column, undo): (&str, Option<GroupChange>) = match change {
            GroupChange::Subject(_) => ("subject", Some(GroupChange::Subject(was.subject.clone()))),
            GroupChange::Description(_) => (
                "description",
                Some(GroupChange::Description(
                    was.description.clone().unwrap_or_default(),
                )),
            ),
            GroupChange::Announce(_) => ("announce", Some(GroupChange::Announce(was.announce))),
            GroupChange::Locked(_) => ("locked", Some(GroupChange::Locked(was.locked))),
            GroupChange::JoinApproval(_) => (
                "join_approval",
                was.join_approval.map(GroupChange::JoinApproval),
            ),
            GroupChange::MembersCanAdd(_) => (
                "members_can_add",
                was.members_can_add.map(GroupChange::MembersCanAdd),
            ),
        };
        let renamed = self.write(|tx| {
            let sql = format!("UPDATE groups SET {column} = ?3 WHERE account_id = ?1 AND id = ?2");
            match change {
                GroupChange::Subject(text) => {
                    tx.execute(&sql, params![account.as_str(), group.as_str(), text])?;
                    Ok(tx.execute(
                        "UPDATE chats SET title = ?3 WHERE account_id = ?1 AND id = ?2",
                        params![account.as_str(), group.as_str(), text],
                    )? > 0)
                }
                GroupChange::Description(text) => {
                    let text = Some(text).filter(|text| !text.is_empty());
                    tx.execute(&sql, params![account.as_str(), group.as_str(), text])?;
                    Ok(false)
                }
                GroupChange::Announce(on)
                | GroupChange::Locked(on)
                | GroupChange::JoinApproval(on)
                | GroupChange::MembersCanAdd(on) => {
                    tx.execute(&sql, params![account.as_str(), group.as_str(), on])?;
                    Ok(false)
                }
            }
        })?;
        if renamed {
            self.notify(StoreChange::Chats {
                account_id: account.clone(),
            });
        }
        self.social_changed(account);
        Ok(undo)
    }

    /// Forgets the value of a setting the provider cannot read, after a
    /// change to it was refused: what it is now is not known.
    pub fn forget_group_setting(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: &GroupChange,
    ) -> StoreResult<()> {
        let column = match change {
            GroupChange::JoinApproval(_) => "join_approval",
            GroupChange::MembersCanAdd(_) => "members_can_add",
            _ => return Ok(()),
        };
        self.write(|tx| {
            tx.execute(
                &format!("UPDATE groups SET {column} = NULL WHERE account_id = ?1 AND id = ?2"),
                params![account.as_str(), group.as_str()],
            )?;
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    /// Keeps a group's invite link (`None`: it is not known any more).
    pub fn set_invite_link(
        &self,
        account: &AccountId,
        group: &ChatId,
        link: Option<&str>,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE groups SET invite_link = ?3 WHERE account_id = ?1 AND id = ?2",
                params![account.as_str(), group.as_str(), link],
            )?;
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    /// Notes that the account left a group (or is back in it).
    pub fn set_departed(
        &self,
        account: &AccountId,
        group: &ChatId,
        departed: bool,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "UPDATE groups SET departed = ?3 WHERE account_id = ?1 AND id = ?2",
                params![account.as_str(), group.as_str(), departed],
            )?;
            if departed {
                tx.execute(
                    &format!(
                        "DELETE FROM group_participants
                         WHERE account_id = ?1 AND group_id = ?2 AND contact_id IN {MY_IDS}"
                    ),
                    params![account.as_str(), group.as_str()],
                )?;
            }
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    /// The groups the account and a contact are both in, by subject, as
    /// far as the store holds their participants.
    pub fn groups_in_common(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> StoreResult<Vec<(ChatId, String)>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT g.id, g.subject FROM group_participants p
                 JOIN groups g ON g.account_id = p.account_id AND g.id = p.group_id
                 WHERE p.account_id = ?1 AND p.contact_id = ?2
                   AND g.departed = 0 AND g.community = 0
                 ORDER BY lower(g.subject), g.id",
            )?;
            let mut rows = stmt.query(params![account.as_str(), contact.as_str()])?;
            let mut groups = Vec::new();
            while let Some(row) = rows.next()? {
                groups.push((ChatId::new(row.get::<_, String>(0)?), row.get(1)?));
            }
            Ok(groups)
        })
    }

    /// How many groups of an account the store holds the participants of.
    pub fn group_count(&self, account: &AccountId) -> StoreResult<usize> {
        self.read(|conn| {
            Ok(conn.query_row(
                "SELECT count(*) FROM groups WHERE account_id = ?1",
                params![account.as_str()],
                |row| row.get::<_, i64>(0),
            )? as usize)
        })
    }

    // ----- requests to join ---------------------------------------------

    /// Replaces the pending requests to join a group.
    pub fn put_join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
        requests: &[JoinRequest],
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM group_join_requests WHERE account_id = ?1 AND group_id = ?2",
                params![account.as_str(), group.as_str()],
            )?;
            for request in requests {
                tx.execute(
                    "INSERT OR REPLACE INTO group_join_requests
                        (account_id, group_id, contact_id, requested_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        account.as_str(),
                        group.as_str(),
                        request.contact.as_str(),
                        request.requested_at.map(Timestamp::as_millis),
                    ],
                )?;
            }
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    /// The pending requests to join a group, oldest first.
    pub fn join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> StoreResult<Vec<JoinRequest>> {
        self.read(|conn| {
            let mut stmt = conn.prepare_cached(
                "SELECT contact_id, requested_at FROM group_join_requests
                 WHERE account_id = ?1 AND group_id = ?2
                 ORDER BY COALESCE(requested_at, 0), contact_id",
            )?;
            let mut rows = stmt.query(params![account.as_str(), group.as_str()])?;
            let mut requests = Vec::new();
            while let Some(row) = rows.next()? {
                requests.push(JoinRequest {
                    contact: ContactId::new(row.get::<_, String>(0)?),
                    requested_at: row.get::<_, Option<i64>>(1)?.map(Timestamp::from_millis),
                });
            }
            Ok(requests)
        })
    }

    /// Adds or removes one pending request.
    pub fn set_join_request(
        &self,
        account: &AccountId,
        group: &ChatId,
        request: &JoinRequest,
        pending: bool,
    ) -> StoreResult<()> {
        self.write(|tx| {
            if pending {
                tx.execute(
                    "INSERT OR REPLACE INTO group_join_requests
                        (account_id, group_id, contact_id, requested_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        account.as_str(),
                        group.as_str(),
                        request.contact.as_str(),
                        request.requested_at.map(Timestamp::as_millis),
                    ],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM group_join_requests
                     WHERE account_id = ?1 AND group_id = ?2 AND contact_id = ?3",
                    params![account.as_str(), group.as_str(), request.contact.as_str()],
                )?;
            }
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    // ----- the blocklist ------------------------------------------------

    /// Replaces an account's blocklist with what the provider listed.
    pub fn put_blocked(&self, account: &AccountId, blocked: &[ContactId]) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM blocked_contacts WHERE account_id = ?1",
                params![account.as_str()],
            )?;
            for contact in blocked {
                tx.execute(
                    "INSERT OR IGNORE INTO blocked_contacts (account_id, contact_id)
                     VALUES (?1, ?2)",
                    params![account.as_str(), contact.as_str()],
                )?;
            }
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    /// Notes that a contact is blocked, or no longer is.
    pub fn set_blocked(
        &self,
        account: &AccountId,
        contact: &ContactId,
        blocked: bool,
    ) -> StoreResult<()> {
        self.write(|tx| {
            let sql = if blocked {
                "INSERT OR IGNORE INTO blocked_contacts (account_id, contact_id) VALUES (?1, ?2)"
            } else {
                "DELETE FROM blocked_contacts WHERE account_id = ?1 AND contact_id = ?2"
            };
            tx.execute(sql, params![account.as_str(), contact.as_str()])?;
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    /// Whether the account has blocked a contact, as far as the store
    /// knows.
    pub fn is_blocked(&self, account: &AccountId, contact: &ContactId) -> StoreResult<bool> {
        self.read(|conn| {
            Ok(conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM blocked_contacts
                                WHERE account_id = ?1 AND contact_id = ?2)",
                params![account.as_str(), contact.as_str()],
                |row| row.get(0),
            )?)
        })
    }

    // ----- profiles -----------------------------------------------------

    /// Keeps what a business says about itself (`None`: the contact is not
    /// a business), and when that was asked.
    pub fn put_business_profile(
        &self,
        account: &AccountId,
        contact: &ContactId,
        profile: Option<&BusinessProfile>,
        now: Timestamp,
    ) -> StoreResult<()> {
        let json = profile.map(serde_json::to_string).transpose()?;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO contact_profiles (account_id, contact_id, business, looked_up_at)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT (account_id, contact_id) DO UPDATE SET
                    business = excluded.business, looked_up_at = excluded.looked_up_at",
                params![account.as_str(), contact.as_str(), json, now.as_millis()],
            )?;
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }

    /// A contact's business profile, when it is one and was asked about.
    pub fn business_profile(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> StoreResult<Option<BusinessProfile>> {
        let json = self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT business FROM contact_profiles
                     WHERE account_id = ?1 AND contact_id = ?2",
                    params![account.as_str(), contact.as_str()],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .flatten())
        })?;
        Ok(json.as_deref().map(serde_json::from_str).transpose()?)
    }

    /// The account's own profile, as far as it is known here. A field the
    /// provider could not read and nobody set from this client is `None`.
    pub fn own_profile(&self, account: &AccountId) -> StoreResult<OwnProfile> {
        self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT name, about FROM own_profiles WHERE account_id = ?1",
                    params![account.as_str()],
                    |row| {
                        Ok(OwnProfile {
                            name: row.get(0)?,
                            about: row.get(1)?,
                        })
                    },
                )
                .optional()?
                .unwrap_or_default())
        })
    }

    /// Keeps the fields of the account's own profile that `profile`
    /// carries. With `replace`, a `None` forgets the stored value too.
    pub fn put_own_profile(
        &self,
        account: &AccountId,
        profile: &OwnProfile,
        replace: bool,
    ) -> StoreResult<()> {
        self.write(|tx| {
            tx.execute(
                "INSERT INTO own_profiles (account_id, name, about) VALUES (?1, ?2, ?3)
                 ON CONFLICT (account_id) DO UPDATE SET
                    name = CASE WHEN ?4 THEN excluded.name ELSE COALESCE(excluded.name, name) END,
                    about = CASE WHEN ?4 THEN excluded.about
                                 ELSE COALESCE(excluded.about, about) END",
                params![account.as_str(), profile.name, profile.about, replace],
            )?;
            Ok(())
        })?;
        self.social_changed(account);
        Ok(())
    }
}
