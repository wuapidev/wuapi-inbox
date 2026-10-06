//! Profiles and groups: the engine's side.
//!
//! Two kinds of work, both off the UI thread:
//!
//! * **Reading** (`want_*`): a group's details, a contact's profile, the
//!   blocklist, the account's own profile. Asked for by whoever is about
//!   to show them; fetched in the background when the copy in the store is
//!   older than its freshness, and written to the store, which is what the
//!   views read.
//! * **Changing**. Where the old state can be put back, the store shows
//!   the new state at once and the provider is told in the background
//!   (blocking, roles, removals, a group's details and settings, pictures,
//!   the own profile, answers to requests to join). A drop is retried with
//!   backoff, with the same request id, so nothing happens twice; only a
//!   refusal, or running out of attempts, puts the old state back, and
//!   says so in words ([`StoreChange::Problem`]). What cannot be shown
//!   before it happened (creating a group, adding people, the invite link,
//!   leaving) is awaited by the dialog that asked, with fewer attempts.

use super::{SyncEngine, SyncError};
use crate::store::StoreChange;
use client_provider::{
    refusal, AccountId, Chat, ChatId, ChatKind, ChatUnknown, ContactId, GroupChange, GroupRole,
    JoinRequest, NewGroup, OwnProfile, ParticipantChange, ParticipantOutcome, ProfileChange,
    ProviderError, Timestamp,
};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;
// The engine's own clock: the one its waits run on.
use tokio::time::Instant;

/// How long a group's details are taken to be current.
const GROUP_FRESH: Duration = Duration::from_secs(5 * 60);
/// How long the list of every group (for "groups in common") is.
const GROUPS_FRESH: Duration = Duration::from_secs(30 * 60);
/// How long what was asked about a contact is.
const PROFILE_FRESH: Duration = Duration::from_secs(10 * 60);
/// How long the blocklist is.
const BLOCKLIST_FRESH: Duration = Duration::from_secs(5 * 60);
/// How long the requests to join a group are.
const REQUESTS_FRESH: Duration = Duration::from_secs(30);
/// How soon a read that failed in passing may be tried again.
const READ_RETRY: Duration = Duration::from_secs(15);
/// How many communities a listing of the groups left out are read on
/// their own after it, for their names. The rest wait for the next one.
const COMMUNITIES_AT_ONCE: usize = 8;
/// How many times a dialog's request is offered before it says "try
/// again", and the longest it waits between two attempts.
const DIALOG_ATTEMPTS: u32 = 3;
const DIALOG_WAIT: Duration = Duration::from_secs(4);
/// Profile and group pictures are kept at this many pixels per side.
const PICTURE_SIDE: u32 = 96;

/// What a background read is about.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Topic {
    Group(AccountId, ChatId),
    Groups(AccountId),
    Requests(AccountId, ChatId),
    Profile(AccountId, ContactId),
    Blocklist(AccountId),
    Own(AccountId),
}

/// The engine's memory of profile and group work.
#[derive(Default)]
pub(super) struct Social {
    /// When each topic is next worth reading.
    due: Mutex<HashMap<Topic, Instant>>,
    /// The newest change asked for per subject. A retry that is no longer
    /// the newest does not undo anything: the user changed their mind.
    generations: Mutex<HashMap<String, u64>>,
    /// Subjects with a change on its way: a read that lands meanwhile
    /// does not overwrite them with what the provider had before.
    busy: Mutex<HashMap<String, u32>>,
    /// Blocks and unblocks on their way.
    blocking: Mutex<HashMap<(AccountId, ContactId), bool>>,
    /// Accounts whose every group was listed at least once.
    listed: Mutex<HashSet<AccountId>>,
    /// Groups somebody looked at since this engine started: a change the
    /// provider hints at is read at once for these, and for the others
    /// when they are looked at.
    looked_at: Mutex<HashSet<(AccountId, ChatId)>>,
}

/// Where a new group goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GroupPlace {
    /// On its own.
    Plain,
    /// A community is created instead of a group.
    Community,
    /// Inside this community, as one of its groups.
    InCommunity(ChatId),
}

/// A group that was just created.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedGroup {
    /// Its chat, in the store. For a community, which has no chat, the
    /// community's own group id.
    pub chat: ChatId,
    /// It is a community.
    pub community: bool,
    /// The people who were asked for and are not in it: WhatsApp did not
    /// let this account add them.
    pub missing: Vec<ContactId>,
}

/// A failure as one sentence for the user. A passing one never names the
/// machinery: it can be tried again.
pub fn failure_sentence(error: &SyncError) -> String {
    match error {
        SyncError::Provider(error) => reason(error),
        other => capitalised(&other.to_string()),
    }
}

/// Why one participant could not be added, removed, promoted or demoted,
/// as a sentence. `None` when it worked.
pub fn outcome_sentence(
    name: &str,
    change: ParticipantChange,
    outcome: &ParticipantOutcome,
) -> Option<String> {
    if outcome.invite_code.is_some() && outcome.error.is_none() {
        return Some(format!(
            "{name} cannot be added directly because of their privacy settings. Send them \
             the invite link instead."
        ));
    }
    let code = outcome.error.as_deref()?;
    let action = match change {
        ParticipantChange::Add => "added",
        ParticipantChange::Remove => "removed",
        ParticipantChange::Promote => "made an admin",
        ParticipantChange::Demote => "made a regular participant",
    };
    Some(match code {
        refusal::PRIVACY => format!(
            "{name} cannot be {action} because of their privacy settings. Send them the \
             invite link instead."
        ),
        refusal::ALREADY_MEMBER => format!("{name} is already in the group."),
        refusal::NOT_MEMBER => format!("{name} is not in the group."),
        refusal::NOT_ON_WHATSAPP => format!("{name} is not on WhatsApp."),
        refusal::NOT_ADMIN => {
            format!("{name} could not be {action}: only admins of the group can do that.")
        }
        other => format!(
            "{name} could not be {action} ({}).",
            other.replace('_', " ")
        ),
    })
}

fn capitalised(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Why a request failed, as a sentence.
fn reason(error: &ProviderError) -> String {
    match error {
        ProviderError::Transient(_) => "Could not reach the provider. Try again.".to_owned(),
        ProviderError::RateLimited { .. } => {
            "WhatsApp is asking this number to slow down. Try again in a moment.".to_owned()
        }
        ProviderError::Rejected { code, message } => match code.as_str() {
            refusal::NOT_ADMIN => "Only admins of the group can do that.".to_owned(),
            refusal::NOT_MEMBER => "This number is not in the group any more.".to_owned(),
            refusal::PRIVACY => "Their privacy settings do not allow it.".to_owned(),
            _ if message.trim().is_empty() => "The provider refused.".to_owned(),
            _ => capitalised(message),
        },
        ProviderError::Unsupported(_) => "Not available with this provider.".to_owned(),
        other => capitalised(&other.to_string()),
    }
}

impl SyncEngine {
    fn social(&self) -> &Social {
        &self.inner.social
    }

    /// Takes the turn to read `topic` if it is due; nobody else reads it
    /// for `fresh` from now.
    fn claim(&self, topic: &Topic, fresh: Duration) -> bool {
        if self.is_stopped() {
            return false;
        }
        let mut due = self.social().due.lock().expect("social lock");
        let now = Instant::now();
        if due.get(topic).is_some_and(|at| now < *at) {
            return false;
        }
        due.insert(topic.clone(), now + fresh);
        true
    }

    /// Notes how a background read went: one that failed in passing is
    /// due again soon.
    fn read_done<T>(&self, topic: Topic, what: &str, result: Result<T, SyncError>) {
        if matches!(&result, Err(error) if error.is_transient()) {
            let mut due = self.social().due.lock().expect("social lock");
            due.insert(topic, Instant::now() + READ_RETRY);
        }
        if let Err(SyncError::Provider(ProviderError::Unsupported(_))) = &result {
            return;
        }
        self.log_failure(what, result);
    }

    /// Makes `subject`'s newest change this one, and marks it as on its
    /// way.
    fn begin(&self, subject: &str) -> u64 {
        *self
            .social()
            .busy
            .lock()
            .expect("social lock")
            .entry(subject.to_owned())
            .or_default() += 1;
        let mut generations = self.social().generations.lock().expect("social lock");
        let generation = generations.entry(subject.to_owned()).or_default();
        *generation += 1;
        *generation
    }

    /// The change is no longer on its way. Returns whether it is still the
    /// newest asked for `subject`.
    fn end(&self, subject: &str, generation: u64) -> bool {
        let mut busy = self.social().busy.lock().expect("social lock");
        if let Some(count) = busy.get_mut(subject) {
            *count -= 1;
            if *count == 0 {
                busy.remove(subject);
            }
        }
        drop(busy);
        let generations = self.social().generations.lock().expect("social lock");
        generations.get(subject) == Some(&generation)
    }

    fn is_busy(&self, subject: &str) -> bool {
        let busy = self.social().busy.lock().expect("social lock");
        busy.contains_key(subject)
    }

    /// Offers a request to the provider until it answers something other
    /// than "not now", at most `attempts` times, waiting at most `longest`
    /// in between. The caller passes the same request id every time.
    async fn persist<T, F, Fut>(
        &self,
        attempts: u32,
        longest: Duration,
        call: F,
    ) -> Result<T, ProviderError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.bounded(call()).await {
                Err(error) if error.is_transient() && attempt < attempts && !self.is_stopped() => {
                    tracing::debug!(%error, "will be retried");
                    let wait = error.retry_after().unwrap_or(self.backoff(attempt));
                    tokio::time::sleep(wait.min(longest)).await;
                }
                other => return self.noted(other),
            }
        }
    }

    /// A request made in the background for something already shown.
    async fn offer<T, F, Fut>(&self, call: F) -> Result<T, ProviderError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        let attempts = self.inner.config.chat_change_attempts;
        self.persist(attempts, self.inner.config.retry_max, call)
            .await
    }

    /// A request a dialog is waiting on.
    async fn ask<T, F, Fut>(&self, call: F) -> Result<T, ProviderError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, ProviderError>>,
    {
        self.persist(DIALOG_ATTEMPTS, DIALOG_WAIT, call).await
    }

    /// Says that something already shown was put back, and why. A refused
    /// key is not said here: the sign-in screen says it.
    fn put_back(&self, what: &str, error: &ProviderError) {
        if matches!(error, ProviderError::Unauthorized(_)) {
            return;
        }
        let why = match error {
            ProviderError::Transient(_) => "The connection kept failing. Try again.".to_owned(),
            other => reason(other),
        };
        self.inner.store.notify(StoreChange::Problem {
            message: format!("{what}. {why}"),
        });
    }

    fn say(&self, message: String) {
        self.inner.store.notify(StoreChange::Problem { message });
    }

    /// The name a contact goes by, for a sentence.
    fn name_of(&self, account: &AccountId, contact: &ContactId) -> String {
        match self.inner.store.contact(account, contact) {
            Ok(Some(known)) => known.display_name(),
            _ => contact.to_string(),
        }
    }

    // ----- groups: reading ----------------------------------------------

    /// Says a group's details are about to be shown: they are read again
    /// in the background if the copy in the store is not fresh.
    pub fn want_group(&self, account: &AccountId, group: &ChatId) {
        if !self.inner.capabilities.group_info {
            return;
        }
        self.social()
            .looked_at
            .lock()
            .expect("social lock")
            .insert((account.clone(), group.clone()));
        let topic = Topic::Group(account.clone(), group.clone());
        if !self.claim(&topic, GROUP_FRESH) {
            return;
        }
        let this = self.clone();
        let (account, group) = (account.clone(), group.clone());
        self.inner.runtime.spawn(async move {
            let result = this.refresh_group(&account, &group).await;
            this.read_done(topic, "group details", result);
        });
    }

    /// Reads a group from the provider into the store.
    pub async fn refresh_group(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> Result<(), SyncError> {
        let inner = &self.inner;
        let fetched = self
            .bounded(inner.provider.fetch_group(account, group))
            .await;
        let details = match self.noted(fetched) {
            Ok(details) => details,
            // The account is not in it any more: that is an answer.
            Err(ProviderError::Rejected { code, .. }) if code == refusal::NOT_MEMBER => {
                return Ok(inner.store.set_departed(account, group, true)?);
            }
            Err(error) => return Err(error.into()),
        };
        inner.store.put_group(&details, Timestamp::now())?;
        // Its community, when that was never read: the chat list shows
        // the community's name. One read, and a community is in none.
        if let Some(community) = &details.community_id {
            if inner.store.group(account, community)?.is_none() {
                let fetched = self
                    .bounded(inner.provider.fetch_group(account, community))
                    .await;
                match self.noted(fetched) {
                    Ok(parent) => inner.store.put_group(&parent, Timestamp::now())?,
                    // Not in it, or not now: the group itself was read.
                    Err(error) => tracing::debug!(%error, "a group's community was not read"),
                }
            }
        }
        Ok(())
    }

    /// The provider said a group changed: what the store holds of it is
    /// read again, now, when somebody looked at the group since this
    /// engine started. A group nobody looked at is read when it is.
    pub(super) fn group_changed(&self, account: &AccountId, group: &ChatId) {
        let topic = Topic::Group(account.clone(), group.clone());
        self.social()
            .due
            .lock()
            .expect("social lock")
            .remove(&topic);
        let looked_at = self
            .social()
            .looked_at
            .lock()
            .expect("social lock")
            .contains(&(account.clone(), group.clone()));
        if looked_at && matches!(self.inner.store.group(account, group), Ok(Some(_))) {
            self.want_group(account, group);
        }
    }

    /// The provider said groups were linked to a community or unlinked
    /// from it: the community and each of them are read again, now,
    /// whether or not they were held, because the chat list says which
    /// community a chat is in.
    pub(super) fn community_changed(
        &self,
        account: &AccountId,
        community: &ChatId,
        groups: &[ChatId],
    ) {
        if !self.inner.capabilities.group_info || self.is_stopped() {
            return;
        }
        let mut wanted = vec![community.clone()];
        for group in groups {
            if !wanted.contains(group) {
                wanted.push(group.clone());
            }
        }
        {
            // Each of them is being read: nobody else needs to meanwhile.
            let mut due = self.social().due.lock().expect("social lock");
            for group in &wanted {
                due.insert(
                    Topic::Group(account.clone(), group.clone()),
                    Instant::now() + GROUP_FRESH,
                );
            }
        }
        let this = self.clone();
        let account = account.clone();
        self.inner.runtime.spawn(async move {
            for group in wanted {
                let result = this.refresh_group(&account, &group).await;
                let topic = Topic::Group(account.clone(), group);
                this.read_done(topic, "a community's groups", result);
            }
        });
    }

    /// A chat arrived from the provider: when it is a group whose subject
    /// is not the one held here, the group changed.
    pub(super) fn note_group_chat(&self, chat: &Chat) {
        if chat.kind != ChatKind::Group || !self.inner.capabilities.group_info {
            return;
        }
        if let Ok(Some(stored)) = self.inner.store.group(&chat.account_id, &chat.id) {
            let subject = format!("group:{}:{}:subject", chat.account_id, chat.id);
            if stored.group.subject != chat.title && !self.is_busy(&subject) {
                self.group_changed(&chat.account_id, &chat.id);
            }
        }
    }

    /// Says the groups a contact shares with the account are about to be
    /// shown: every group's participants are read in the background, if
    /// that was not done lately.
    pub fn want_groups(&self, account: &AccountId) {
        if !self.inner.capabilities.group_info {
            return;
        }
        let topic = Topic::Groups(account.clone());
        if !self.claim(&topic, GROUPS_FRESH) {
            return;
        }
        let this = self.clone();
        let account = account.clone();
        self.inner.runtime.spawn(async move {
            let result = this.sync_groups(&account).await;
            this.read_done(topic, "the list of groups", result);
        });
    }

    /// Lists the groups of every connected number, after a refresh: one
    /// request per number says which community each group is linked to,
    /// so the chat list knows without any group being opened. A listing
    /// that fails is not a failure of the refresh: it is asked for again
    /// at the next one, or when somebody looks.
    pub(super) async fn refresh_groups(&self) {
        if !self.inner.capabilities.group_info {
            return;
        }
        let accounts = match self.inner.store.accounts() {
            Ok(accounts) => accounts,
            Err(error) => return tracing::error!(%error, "could not read the accounts"),
        };
        for account in accounts {
            // Read live from WhatsApp by some providers: not while the
            // number is offline.
            if !account.connection.is_connected() || self.is_stopped() {
                continue;
            }
            let topic = Topic::Groups(account.id.clone());
            self.social()
                .due
                .lock()
                .expect("social lock")
                .insert(topic.clone(), Instant::now() + GROUPS_FRESH);
            let result = self.sync_groups(&account.id).await;
            self.read_done(topic, "the list of groups", result);
        }
    }

    /// Reads every group of an account, with its participants and the
    /// community it is linked to, into the store. Returns how many there
    /// are.
    pub async fn sync_groups(&self, account: &AccountId) -> Result<usize, SyncError> {
        let inner = &self.inner;
        let listed = self.bounded(inner.provider.list_groups(account)).await;
        let groups = self.noted(listed)?;
        inner.store.put_groups(&groups, Timestamp::now())?;
        self.social()
            .listed
            .lock()
            .expect("social lock")
            .insert(account.clone());
        {
            // Each of them was just read. Not a community: a listing does
            // not say which groups it links, so it is still read when it
            // is looked at.
            let mut due = self.social().due.lock().expect("social lock");
            for group in groups.iter().filter(|group| !group.community) {
                due.insert(
                    Topic::Group(account.clone(), group.id.clone()),
                    Instant::now() + GROUP_FRESH,
                );
            }
        }
        // A community the listing left out is read on its own, for its
        // name: one read per community, never one per group.
        let mut missing: Vec<&ChatId> = Vec::new();
        for community in groups
            .iter()
            .filter_map(|group| group.community_id.as_ref())
        {
            if !missing.contains(&community)
                && !groups.iter().any(|group| &group.id == community)
                && inner.store.group(account, community)?.is_none()
            {
                missing.push(community);
            }
        }
        for community in missing.into_iter().take(COMMUNITIES_AT_ONCE) {
            let result = self.refresh_group(account, community).await;
            if let Err(error) = result {
                tracing::debug!(%error, "a community was not read");
            }
        }
        Ok(groups.len())
    }

    /// Whether every group of `account` was listed since this engine
    /// started: only then is "groups in common" the whole answer.
    pub fn groups_listed(&self, account: &AccountId) -> bool {
        let listed = self.social().listed.lock().expect("social lock");
        listed.contains(account)
    }

    /// Says the requests to join a group are about to be shown.
    pub fn want_join_requests(&self, account: &AccountId, group: &ChatId) {
        if !self.inner.capabilities.group_join_requests {
            return;
        }
        let topic = Topic::Requests(account.clone(), group.clone());
        if !self.claim(&topic, REQUESTS_FRESH) {
            return;
        }
        let this = self.clone();
        let (account, group) = (account.clone(), group.clone());
        self.inner.runtime.spawn(async move {
            let inner = &this.inner;
            let listed = this
                .bounded(inner.provider.join_requests(&account, &group))
                .await;
            let result = match this.noted(listed) {
                Ok(requests) => inner
                    .store
                    .put_join_requests(&account, &group, &requests)
                    .map_err(SyncError::from),
                // Not an admin (any more): nothing to show.
                Err(ProviderError::Rejected { .. }) => inner
                    .store
                    .put_join_requests(&account, &group, &[])
                    .map_err(SyncError::from),
                Err(error) => Err(error.into()),
            };
            this.read_done(topic, "requests to join", result);
        });
    }

    // ----- groups: changing ---------------------------------------------

    /// Creates a group and puts it in the store, chat included. `picture`
    /// (a JPEG) is set afterwards, in the background. Await it off the UI
    /// thread; `request_id` is the same for every attempt at this group.
    pub async fn create_group(
        &self,
        account: &AccountId,
        subject: &str,
        participants: Vec<ContactId>,
        picture: Option<Vec<u8>>,
        request_id: &str,
    ) -> Result<CreatedGroup, SyncError> {
        self.create_group_in(
            account,
            subject,
            participants,
            picture,
            request_id,
            GroupPlace::Plain,
        )
        .await
    }

    /// [`create_group`](Self::create_group) where `place` says: on its
    /// own, inside a community (which is read again, so that it lists the
    /// new group), or as a community. A community has no chat: none is
    /// made, and [`CreatedGroup::chat`] is the community's group id.
    pub async fn create_group_in(
        &self,
        account: &AccountId,
        subject: &str,
        participants: Vec<ContactId>,
        picture: Option<Vec<u8>>,
        request_id: &str,
        place: GroupPlace,
    ) -> Result<CreatedGroup, SyncError> {
        let inner = &self.inner;
        let new = NewGroup {
            subject: subject.trim().to_owned(),
            participants,
            request_id: request_id.to_owned(),
            community: place == GroupPlace::Community,
            in_community: match &place {
                GroupPlace::InCommunity(community) => Some(community.clone()),
                _ => None,
            },
        };
        let created = self
            .ask(|| inner.provider.create_group(account, &new))
            .await?;
        inner.store.put_group(&created, Timestamp::now())?;
        if !created.community && inner.store.chat(account, &created.id)?.is_none() {
            inner.store.upsert_chat(
                &Chat {
                    id: created.id.clone(),
                    account_id: account.clone(),
                    kind: ChatKind::Group,
                    title: created.subject.clone(),
                    avatar: None,
                    unread_count: 0,
                    pinned: false,
                    muted: false,
                    archived: false,
                    last_message: None,
                    unknown: ChatUnknown::default(),
                    picture_id: None,
                    pinned_at: None,
                },
                true,
            )?;
        }
        if let Some(community) = &new.in_community {
            self.reread(account, std::slice::from_ref(community)).await;
        }
        let missing = new
            .participants
            .iter()
            .filter(|asked| !created.participants.iter().any(|p| &p.contact == *asked))
            .cloned()
            .collect();
        if let Some(jpeg) = picture {
            self.set_group_picture(account, &created.id, Some(jpeg));
        }
        Ok(CreatedGroup {
            community: created.community,
            chat: created.id,
            missing,
        })
    }

    /// Links an existing group to a community. When this returns the
    /// store has the community and the group as the provider now has
    /// them, so the chat list says which community the chat is in. Await
    /// it off the UI thread; `request_id` is the same for every attempt.
    pub async fn link_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
        request_id: &str,
    ) -> Result<(), SyncError> {
        let inner = &self.inner;
        self.ask(|| {
            inner
                .provider
                .link_subgroup(account, community, group, request_id)
        })
        .await?;
        self.reread(account, &[community.clone(), group.clone()])
            .await;
        Ok(())
    }

    /// Takes a group out of a community, and reads both again.
    pub async fn unlink_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
    ) -> Result<(), SyncError> {
        let inner = &self.inner;
        self.ask(|| inner.provider.unlink_subgroup(account, community, group))
            .await?;
        self.reread(account, &[community.clone(), group.clone()])
            .await;
        Ok(())
    }

    /// Who is in all of a community's groups. Not kept: asked for each
    /// time the list is shown. Await it off the UI thread.
    pub async fn community_participants(
        &self,
        account: &AccountId,
        community: &ChatId,
    ) -> Result<Vec<ContactId>, SyncError> {
        let inner = &self.inner;
        Ok(self
            .ask(|| inner.provider.community_participants(account, community))
            .await?)
    }

    /// Reads groups again, now, after a change of their links was made
    /// here. The change is made: a read that fails is logged and left to
    /// the provider's own announcement of it and the next listing.
    async fn reread(&self, account: &AccountId, groups: &[ChatId]) {
        {
            let mut due = self.social().due.lock().expect("social lock");
            for group in groups {
                due.insert(
                    Topic::Group(account.clone(), group.clone()),
                    Instant::now() + GROUP_FRESH,
                );
            }
        }
        for group in groups {
            if let Err(error) = self.refresh_group(account, group).await {
                tracing::debug!(%error, "a group was not read after its links changed");
            }
        }
    }

    /// Adds people to a group. One outcome per contact: those it worked
    /// for are in the store when this returns.
    pub async fn add_participants(
        &self,
        account: &AccountId,
        group: &ChatId,
        contacts: &[ContactId],
        request_id: &str,
    ) -> Result<Vec<ParticipantOutcome>, SyncError> {
        let inner = &self.inner;
        let outcomes = self
            .ask(|| {
                inner.provider.change_participants(
                    account,
                    group,
                    ParticipantChange::Add,
                    contacts,
                    request_id,
                )
            })
            .await?;
        for outcome in &outcomes {
            let already = outcome.error.as_deref() == Some(refusal::ALREADY_MEMBER);
            let known = inner
                .store
                .participant_role(account, group, &outcome.contact)?;
            if (outcome.worked() || already) && known.is_none() {
                inner.store.set_participant(
                    account,
                    group,
                    &outcome.contact,
                    Some(GroupRole::Member),
                )?;
            }
        }
        Ok(outcomes)
    }

    /// Removes, promotes or demotes one participant. Shown at once; put
    /// back, with the reason, if the provider refuses.
    pub fn change_participant(
        &self,
        account: &AccountId,
        group: &ChatId,
        contact: &ContactId,
        change: ParticipantChange,
    ) {
        let inner = &self.inner;
        let after = match change {
            ParticipantChange::Remove => None,
            ParticipantChange::Promote => Some(GroupRole::Admin),
            ParticipantChange::Demote => Some(GroupRole::Member),
            // Adding is not shown before it happened.
            ParticipantChange::Add => return,
        };
        let Ok(Some(before)) = inner.store.participant_role(account, group, contact) else {
            return;
        };
        if let Err(error) = inner.store.set_participant(account, group, contact, after) {
            tracing::error!(%error, "could not store the participant change");
            return;
        }
        let subject = format!("participant:{account}:{group}:{contact}");
        let generation = self.begin(&subject);
        let request_id = super::new_client_id().to_string();
        let this = self.clone();
        let (account, group, contact) = (account.clone(), group.clone(), contact.clone());
        inner.runtime.spawn(async move {
            let contacts = [contact.clone()];
            let answer = this
                .offer(|| {
                    this.inner.provider.change_participants(
                        &account,
                        &group,
                        change,
                        &contacts,
                        &request_id,
                    )
                })
                .await;
            let newest = this.end(&subject, generation);
            let name = this.name_of(&account, &contact);
            let failure = match answer {
                Ok(outcomes) => outcomes
                    .iter()
                    .find(|outcome| outcome.contact == contact)
                    // Already gone is removed.
                    .filter(|outcome| {
                        !(change == ParticipantChange::Remove
                            && outcome.error.as_deref() == Some(refusal::NOT_MEMBER))
                    })
                    .and_then(|outcome| outcome_sentence(&name, change, outcome)),
                Err(ProviderError::Unauthorized(_)) => return,
                Err(error) => Some(match (&error, change) {
                    (ProviderError::Transient(_), _) => {
                        format!("{name} is unchanged: the connection kept failing. Try again.")
                    }
                    (other, _) => format!("{name} is unchanged. {}", reason(other)),
                }),
            };
            let Some(sentence) = failure else { return };
            if newest {
                let undone =
                    this.inner
                        .store
                        .set_participant(&account, &group, &contact, Some(before));
                if let Err(error) = undone {
                    tracing::error!(%error, "could not undo the participant change");
                }
            }
            this.say(sentence);
        });
    }

    /// Changes a group's subject, description or one of its settings.
    /// Shown at once; put back, with the reason, if the provider refuses.
    pub fn update_group(&self, account: &AccountId, group: &ChatId, change: GroupChange) {
        let inner = &self.inner;
        let undo = match inner.store.apply_group_change(account, group, &change) {
            Ok(undo) => undo,
            Err(error) => {
                tracing::error!(%error, "could not store the group change");
                return;
            }
        };
        let field = match &change {
            GroupChange::Subject(_) => "subject",
            GroupChange::Description(_) => "description",
            GroupChange::Announce(_) => "announce",
            GroupChange::Locked(_) => "locked",
            GroupChange::JoinApproval(_) => "join_approval",
            GroupChange::MembersCanAdd(_) => "members_can_add",
        };
        let subject = format!("group:{account}:{group}:{field}");
        let generation = self.begin(&subject);
        let this = self.clone();
        let (account, group) = (account.clone(), group.clone());
        inner.runtime.spawn(async move {
            let answer = this
                .offer(|| this.inner.provider.update_group(&account, &group, &change))
                .await;
            let newest = this.end(&subject, generation);
            let Err(error) = answer else { return };
            if newest {
                let store = &this.inner.store;
                let undone = match &undo {
                    Some(undo) => store.apply_group_change(&account, &group, undo).map(drop),
                    None => store.forget_group_setting(&account, &group, &change),
                };
                if let Err(error) = undone {
                    tracing::error!(%error, "could not undo the group change");
                }
            }
            let what = match change {
                GroupChange::Subject(_) => "The group's name was not changed",
                GroupChange::Description(_) => "The group's description was not changed",
                _ => "The group's setting was not changed",
            };
            this.put_back(what, &error);
        });
    }

    /// Sets (a JPEG) or removes a group's picture. Shown at once; the old
    /// one comes back, with the reason, if the provider refuses.
    pub fn set_group_picture(&self, account: &AccountId, group: &ChatId, jpeg: Option<Vec<u8>>) {
        self.change_picture(account.clone(), group.clone(), Some(group.clone()), jpeg);
    }

    /// A group's invite link, read from the provider and kept in the
    /// store. With `reset`, the old link stops working.
    pub async fn invite_link(
        &self,
        account: &AccountId,
        group: &ChatId,
        reset: bool,
        request_id: &str,
    ) -> Result<String, SyncError> {
        let inner = &self.inner;
        let link = self
            .ask(|| {
                inner
                    .provider
                    .group_invite_link(account, group, reset, request_id)
            })
            .await?;
        inner.store.set_invite_link(account, group, Some(&link))?;
        Ok(link)
    }

    /// Approves or rejects one request to join. It leaves the list at
    /// once, and comes back, with the reason, if the provider refuses.
    pub fn answer_join_request(
        &self,
        account: &AccountId,
        group: &ChatId,
        request: &JoinRequest,
        approve: bool,
    ) {
        let inner = &self.inner;
        if let Err(error) = inner.store.set_join_request(account, group, request, false) {
            tracing::error!(%error, "could not store the answer to a request");
            return;
        }
        let request_id = super::new_client_id().to_string();
        let this = self.clone();
        let (account, group, request) = (account.clone(), group.clone(), request.clone());
        inner.runtime.spawn(async move {
            let contacts = [request.contact.clone()];
            let answer = this
                .offer(|| {
                    this.inner.provider.answer_join_requests(
                        &account,
                        &group,
                        approve,
                        &contacts,
                        &request_id,
                    )
                })
                .await;
            let name = this.name_of(&account, &request.contact);
            let store = &this.inner.store;
            let refused = match answer {
                Ok(outcomes) => outcomes
                    .iter()
                    .find(|outcome| outcome.contact == request.contact)
                    .and_then(|outcome| outcome.error.clone())
                    .map(|code| {
                        format!(
                            "{name}'s request was not answered ({}).",
                            code.replace('_', " ")
                        )
                    }),
                Err(ProviderError::Unauthorized(_)) => return,
                Err(ProviderError::Transient(_)) => Some(format!(
                    "{name}'s request was not answered: the connection kept failing. Try again."
                )),
                Err(error) => Some(format!(
                    "{name}'s request was not answered. {}",
                    reason(&error)
                )),
            };
            let stored = match &refused {
                Some(_) => store.set_join_request(&account, &group, &request, true),
                None if approve => store.set_participant(
                    &account,
                    &group,
                    &request.contact,
                    Some(GroupRole::Member),
                ),
                None => Ok(()),
            };
            if let Err(error) = stored {
                tracing::error!(%error, "could not store the answer to a request");
            }
            if let Some(sentence) = refused {
                this.say(sentence);
            }
        });
    }

    /// The account leaves a group. Await it off the UI thread.
    pub async fn leave_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        request_id: &str,
    ) -> Result<(), SyncError> {
        let inner = &self.inner;
        let left = self
            .ask(|| inner.provider.leave_group(account, group, request_id))
            .await;
        match left {
            Ok(()) => {}
            // Already out.
            Err(ProviderError::Rejected { code, .. }) if code == refusal::NOT_MEMBER => {}
            Err(error) => return Err(error.into()),
        }
        Ok(inner.store.set_departed(account, group, true)?)
    }

    // ----- contacts -----------------------------------------------------

    /// Says a contact's profile is about to be shown: WhatsApp is asked
    /// about them (About, username, picture, business profile), and for
    /// the blocklist, when that was not done lately.
    pub fn want_profile(&self, account: &AccountId, contact: &ContactId) {
        self.want_blocklist(account);
        let caps = self.inner.capabilities;
        if !(caps.contact_lookup || caps.business_profiles) {
            return;
        }
        let topic = Topic::Profile(account.clone(), contact.clone());
        if !self.claim(&topic, PROFILE_FRESH) {
            return;
        }
        let this = self.clone();
        let (account, contact) = (account.clone(), contact.clone());
        self.inner.runtime.spawn(async move {
            let result = this.refresh_profile(&account, &contact).await;
            this.read_done(topic, "a contact's profile", result);
        });
    }

    /// Asks the provider about one contact and stores the answer.
    pub async fn refresh_profile(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> Result<(), SyncError> {
        let inner = &self.inner;
        let caps = inner.capabilities;
        if caps.contact_lookup {
            let looked = self
                .bounded(inner.provider.lookup_contact(account, contact))
                .await;
            match self.noted(looked) {
                Ok(found) => inner.store.upsert_contact(&found)?,
                // WhatsApp has nothing to say about them: not a failure.
                Err(ProviderError::Rejected { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        if !caps.business_profiles {
            return Ok(());
        }
        // Only a business has one; a lookup is what says who is one.
        let is_business = inner
            .store
            .contact(account, contact)?
            .is_some_and(|known| known.business_name.is_some());
        if caps.contact_lookup && !is_business {
            return Ok(());
        }
        let asked = self
            .bounded(inner.provider.business_profile(account, contact))
            .await;
        let profile = match self.noted(asked) {
            Ok(profile) => profile,
            Err(ProviderError::Rejected { .. }) => None,
            Err(error) => return Err(error.into()),
        };
        inner.store.put_business_profile(
            account,
            contact,
            profile.as_ref().filter(|profile| !profile.is_empty()),
            Timestamp::now(),
        )?;
        Ok(())
    }

    /// Reads the blocklist in the background, when it was not read lately.
    pub fn want_blocklist(&self, account: &AccountId) {
        if !self.inner.capabilities.blocking {
            return;
        }
        let topic = Topic::Blocklist(account.clone());
        if !self.claim(&topic, BLOCKLIST_FRESH) {
            return;
        }
        let this = self.clone();
        let account = account.clone();
        self.inner.runtime.spawn(async move {
            let result = this.refresh_blocklist(&account).await;
            this.read_done(topic, "the blocklist", result);
        });
    }

    /// Reads the blocklist from the provider into the store.
    pub async fn refresh_blocklist(&self, account: &AccountId) -> Result<(), SyncError> {
        let inner = &self.inner;
        let listed = self.bounded(inner.provider.list_blocked(account)).await;
        let blocked = self.noted(listed)?;
        inner.store.put_blocked(account, &blocked)?;
        // What the user changed meanwhile stands until it is answered.
        let pending: Vec<(ContactId, bool)> = {
            let blocking = self.social().blocking.lock().expect("social lock");
            blocking
                .iter()
                .filter(|((owner, _), _)| owner == account)
                .map(|((_, contact), blocked)| (contact.clone(), *blocked))
                .collect()
        };
        for (contact, blocked) in pending {
            inner.store.set_blocked(account, &contact, blocked)?;
        }
        Ok(())
    }

    /// Blocks or unblocks a contact. Shown at once; put back, with the
    /// reason, if the provider refuses.
    pub fn set_blocked(&self, account: &AccountId, contact: &ContactId, blocked: bool) {
        let inner = &self.inner;
        if let Err(error) = inner.store.set_blocked(account, contact, blocked) {
            tracing::error!(%error, "could not store the block");
            return;
        }
        let key = (account.clone(), contact.clone());
        self.social()
            .blocking
            .lock()
            .expect("social lock")
            .insert(key.clone(), blocked);
        let subject = format!("block:{account}:{contact}");
        let generation = self.begin(&subject);
        let request_id = super::new_client_id().to_string();
        let this = self.clone();
        inner.runtime.spawn(async move {
            let (account, contact) = &key;
            let answer = this
                .offer(|| {
                    this.inner
                        .provider
                        .set_blocked(account, contact, blocked, &request_id)
                })
                .await;
            if !this.end(&subject, generation) {
                return;
            }
            this.social()
                .blocking
                .lock()
                .expect("social lock")
                .remove(&key);
            let Err(error) = answer else { return };
            if let Err(error) = this.inner.store.set_blocked(account, contact, !blocked) {
                tracing::error!(%error, "could not undo the block");
            }
            let name = this.name_of(account, contact);
            let what = if blocked {
                format!("{name} was not blocked")
            } else {
                format!("{name} was not unblocked")
            };
            this.put_back(&what, &error);
        });
    }

    // ----- the account's own profile ------------------------------------

    /// The chat id the account's own picture is kept under.
    pub fn own_subject(&self, account: &AccountId) -> Option<ChatId> {
        let accounts = self.inner.store.accounts().ok()?;
        let own = accounts.into_iter().find(|known| &known.id == account)?;
        own_picture_subject(&own)
    }

    /// Says the account's own profile is about to be shown: what the
    /// provider can read of it is read in the background.
    pub fn want_own_profile(&self, account: &AccountId) {
        if !self.inner.capabilities.profile_edit {
            return;
        }
        let topic = Topic::Own(account.clone());
        if !self.claim(&topic, PROFILE_FRESH) {
            return;
        }
        let this = self.clone();
        let account = account.clone();
        self.inner.runtime.spawn(async move {
            let result = this.refresh_own_profile(&account).await;
            this.read_done(topic, "the own profile", result);
        });
    }

    /// Reads the account's own profile into the store. What the provider
    /// cannot read keeps the value last set from here.
    pub async fn refresh_own_profile(&self, account: &AccountId) -> Result<(), SyncError> {
        let inner = &self.inner;
        let read = self.bounded(inner.provider.own_profile(account)).await;
        let mut profile = self.noted(read)?;
        if self.is_busy(&format!("own:{account}:name")) {
            profile.name = None;
        }
        if self.is_busy(&format!("own:{account}:about")) {
            profile.about = None;
        }
        Ok(inner.store.put_own_profile(account, &profile, false)?)
    }

    /// Changes the account's own name or "About" text. Shown at once; the
    /// old one comes back, with the reason, if the provider refuses.
    pub fn update_profile(&self, account: &AccountId, change: ProfileChange) {
        let inner = &self.inner;
        let before = inner.store.own_profile(account).unwrap_or_default();
        let (field, after) = match &change {
            ProfileChange::Name(name) => (
                "name",
                OwnProfile {
                    name: Some(name.clone()),
                    about: None,
                },
            ),
            ProfileChange::About(about) => (
                "about",
                OwnProfile {
                    name: None,
                    about: Some(about.clone()),
                },
            ),
        };
        if let Err(error) = inner.store.put_own_profile(account, &after, false) {
            tracing::error!(%error, "could not store the profile change");
            return;
        }
        let subject = format!("own:{account}:{field}");
        let generation = self.begin(&subject);
        let this = self.clone();
        let account = account.clone();
        inner.runtime.spawn(async move {
            let answer = this
                .offer(|| this.inner.provider.update_profile(&account, &change))
                .await;
            let newest = this.end(&subject, generation);
            let Err(error) = answer else { return };
            if newest {
                let store = &this.inner.store;
                let mut now = store.own_profile(&account).unwrap_or_default();
                match change {
                    ProfileChange::Name(_) => now.name = before.name,
                    ProfileChange::About(_) => now.about = before.about,
                }
                if let Err(error) = store.put_own_profile(&account, &now, true) {
                    tracing::error!(%error, "could not undo the profile change");
                }
            }
            let what = match field {
                "name" => "Your name was not changed",
                _ => "Your About text was not changed",
            };
            this.put_back(what, &error);
        });
    }

    /// Sets (a JPEG) or removes the account's own picture. Shown at once;
    /// the old one comes back, with the reason, if the provider refuses.
    pub fn set_profile_picture(&self, account: &AccountId, jpeg: Option<Vec<u8>>) {
        let Some(subject) = self.own_subject(account) else {
            return;
        };
        self.change_picture(account.clone(), subject, None, jpeg);
    }

    /// The common path of the two pictures a user can change: the
    /// account's own (`group` is `None`) and a group's.
    fn change_picture(
        &self,
        account: AccountId,
        subject: ChatId,
        group: Option<ChatId>,
        jpeg: Option<Vec<u8>>,
    ) {
        let key = format!("picture:{account}:{subject}");
        let generation = self.begin(&key);
        let this = self.clone();
        self.inner.runtime.spawn(async move {
            let store = &this.inner.store;
            let before = store.avatar(&account, &subject).ok().flatten();
            // What is shown meanwhile: the avatar-sized copy of the new
            // picture, under an id no provider gives.
            let small = match jpeg.clone() {
                Some(bytes) => {
                    let made =
                        tokio::task::spawn_blocking(move || crate::thumbnail(&bytes, PICTURE_SIDE))
                            .await;
                    match made {
                        Ok(Ok(small)) => Some(small.bytes),
                        _ => {
                            this.end(&key, generation);
                            return this.say(
                                "The picture was not changed. The file is not an image this \
                                 app can read."
                                    .to_owned(),
                            );
                        }
                    }
                }
                None => None,
            };
            let now = Timestamp::now();
            let shown = store.put_avatar(
                &account,
                &subject,
                small.as_deref().map(|bytes| (PENDING_PICTURE, bytes)),
                now,
            );
            if let Err(error) = shown {
                tracing::error!(%error, "could not store the picture");
            }
            let answer = this
                .offer(|| async {
                    let provider = &this.inner.provider;
                    match &group {
                        Some(group) => {
                            provider
                                .set_group_picture(&account, group, jpeg.as_deref())
                                .await
                        }
                        None => {
                            provider
                                .set_profile_picture(&account, jpeg.as_deref())
                                .await
                        }
                    }
                })
                .await;
            let newest = this.end(&key, generation);
            match answer {
                Ok(id) => {
                    // Under the provider's id from here on, so that the
                    // chat list's picture id finds it current.
                    if let (true, Some(id), Some(bytes)) = (newest, id, small.as_deref()) {
                        let kept = store.put_avatar(&account, &subject, Some((&id, bytes)), now);
                        if let Err(error) = kept {
                            tracing::error!(%error, "could not store the picture");
                        }
                    }
                }
                Err(error) => {
                    if newest {
                        let old = before.as_ref().and_then(|old| {
                            Some((old.picture_id.as_deref()?, old.image.as_deref()?))
                        });
                        if let Err(error) = store.put_avatar(&account, &subject, old, now) {
                            tracing::error!(%error, "could not put the picture back");
                        }
                    }
                    this.put_back("The picture was not changed", &error);
                }
            }
        });
    }
}

/// The id a picture is kept under between the moment the user chose it and
/// the provider's answer.
const PENDING_PICTURE: &str = "pending";

/// The chat id an account's own profile picture is kept under: its number,
/// else the id its own messages go by. One rule for everything that shows
/// that picture (the profile editor, the account rail), so a change of
/// picture reaches all of it.
pub fn own_picture_subject(account: &client_provider::Account) -> Option<ChatId> {
    account
        .phone
        .clone()
        .or_else(|| account.self_contact.clone().map(ContactId::into_string))
        .map(ChatId::new)
}
