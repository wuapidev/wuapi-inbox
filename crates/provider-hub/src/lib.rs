//! Several providers behind one.
//!
//! The sync engine speaks to one [`Provider`]. A [`HubProvider`] is that
//! one, made of others: each number belongs to the provider it was linked
//! through, every call about a number goes to its provider, and the lists
//! and the live updates of all of them come out together. Nothing above the
//! trait knows there is more than one.
//!
//! Its members come and go while it runs ([`HubProvider::attach`],
//! [`HubProvider::detach`]): signing in to a service adds its numbers to
//! the ones already there, and signing out takes only those away.
//!
//! Two things a provider says for itself are said here for each member:
//!
//! * **A refusal of the credentials** ([`ProviderError::Unauthorized`])
//!   would stop the whole engine. A member that answers it is set aside
//!   instead: its numbers stay listed, its calls are turned down with a
//!   sentence, and [`HubProvider::refusals`] tells the application, which
//!   asks the user to sign in to that one again. The other members go on.
//! * **Weather** in one member is not weather in the others. A member
//!   whose list of numbers cannot be read keeps the one it gave last, and
//!   a member whose live updates cannot be opened is tried again later
//!   while the others' are followed.

use async_trait::async_trait;
use client_provider::{
    Account, AccountChange, AccountId, Avatar, AvatarAnswer, BusinessProfile, CallCamera,
    CallDevices, CallVideo, Capabilities, Chat, ChatChange, ChatId, Contact, ContactId, Cursor,
    EventStream, FavoriteSticker, Feature, ForwardItem, Group, GroupChange, IncomingWebhook,
    IncomingWebhookChange, JoinOutcome, JoinRequest, LinkPlace, LinkStatus, LiveUpdates, MediaData,
    MediaLimit, MediaRef, MediaUpload, Message, MessageId, NewAccount, NewGroup, NewStory,
    NumberCheck, OutgoingMessage, OwnProfile, Page, ParticipantChange, ParticipantOutcome,
    ProfileChange, Provider, ProviderError, ProviderResult, SendReceipt, StickerFile, Story,
    StoryPrivacy, StoryViewer, UploadProgress,
};
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;
use tokio::sync::watch;

#[cfg(test)]
mod tests;

/// The id of a hub, whatever it is made of.
pub const HUB_ID: &str = "hub";

/// The code of the error a call to a member that was set aside answers.
pub const SIGNED_OUT: &str = "signed_out";

/// How long a member whose live updates could not be opened waits before
/// it is tried again, the first time. Doubles up to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_secs(5);
const RETRY_MAX: Duration = Duration::from_secs(60);

/// A member refused its credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The member's [`Provider::id`].
    pub provider: &'static str,
    /// What it said.
    pub reason: String,
}

/// One of the providers of a hub.
struct Member {
    provider: Arc<dyn Provider>,
    /// The numbers it listed last, or was known to have when it joined.
    /// `None` until either: nothing is known of its numbers yet.
    accounts: Mutex<Option<Vec<Account>>>,
    /// Set once it refused its credentials.
    refused: AtomicBool,
    /// How many times in a row its live updates could not be opened.
    stream_failures: AtomicU32,
}

impl Member {
    fn id(&self) -> &'static str {
        self.provider.id()
    }

    fn is_refused(&self) -> bool {
        self.refused.load(Ordering::SeqCst)
    }

    /// What a call to a member that was set aside answers.
    fn check(&self) -> ProviderResult<()> {
        if self.is_refused() {
            return Err(ProviderError::Rejected {
                code: SIGNED_OUT.into(),
                message: "This number's service signed this computer out. Sign in again.".into(),
            });
        }
        Ok(())
    }
}

struct Inner {
    /// In the order they joined, which is the order their numbers are
    /// listed in.
    members: RwLock<Vec<Arc<Member>>>,
    /// Which member each number belongs to.
    routes: RwLock<HashMap<AccountId, &'static str>>,
    /// Which member each place to link from was listed by.
    places: Mutex<HashMap<LinkPlace, &'static str>>,
    /// Which member each attempt to link a number went to, by its request
    /// id: a retry goes to the same one.
    links: Mutex<HashMap<String, &'static str>>,
    /// Counts the changes of membership. Whoever follows the live updates
    /// is cut off at each, and comes back for those of the new members.
    changed: watch::Sender<u64>,
    refusals: watch::Sender<Option<Refusal>>,
}

/// Several providers behind one. Cheap to clone; clones are the same hub.
#[derive(Clone)]
pub struct HubProvider {
    inner: Arc<Inner>,
}

impl HubProvider {
    /// A hub of `members`. Their order is the order of their numbers, and
    /// a number linked without saying where goes to the first that links
    /// without being told a place.
    pub fn new(members: Vec<Arc<dyn Provider>>) -> Self {
        let hub = Self {
            inner: Arc::new(Inner {
                members: RwLock::new(Vec::new()),
                routes: RwLock::new(HashMap::new()),
                places: Mutex::new(HashMap::new()),
                links: Mutex::new(HashMap::new()),
                changed: watch::channel(0).0,
                refusals: watch::channel(None).0,
            }),
        };
        for member in members {
            hub.attach(member, None);
        }
        hub
    }

    /// Adds a member, or replaces the one of the same id (signed in again,
    /// with new credentials). `known` are the numbers it is known to have,
    /// from the last time the application ran: they stand in until it lists
    /// its own, so that a member that cannot be reached at startup does not
    /// read as one that has none.
    pub fn attach(&self, provider: Arc<dyn Provider>, known: Option<Vec<Account>>) {
        let id = provider.id();
        let member = Arc::new(Member {
            provider,
            accounts: Mutex::new(known),
            refused: AtomicBool::new(false),
            stream_failures: AtomicU32::new(0),
        });
        self.inner.learn(&member);
        {
            let mut members = self.inner.members.write().expect("members lock");
            match members.iter_mut().find(|known| known.id() == id) {
                Some(place) => *place = member,
                None => members.push(member),
            }
        }
        self.inner.refusals.send_if_modified(|refusal| {
            let was_its = refusal.as_ref().is_some_and(|r| r.provider == id);
            if was_its {
                *refusal = None;
            }
            was_its
        });
        self.inner.changed.send_modify(|count| *count += 1);
    }

    /// Takes a member out. Its numbers are no longer listed, which is how
    /// the client comes to forget them.
    pub fn detach(&self, id: &str) {
        self.inner
            .members
            .write()
            .expect("members lock")
            .retain(|member| member.id() != id);
        self.inner
            .routes
            .write()
            .expect("routes lock")
            .retain(|_, member| *member != id);
        self.inner
            .places
            .lock()
            .expect("places lock")
            .retain(|_, member| *member != id);
        self.inner
            .links
            .lock()
            .expect("links lock")
            .retain(|_, member| *member != id);
        self.inner.changed.send_modify(|count| *count += 1);
    }

    /// Whether a provider of this id is a member.
    pub fn has(&self, id: &str) -> bool {
        self.inner.member(id).is_some()
    }

    /// The [`Provider::id`] of the member a number belongs to.
    pub fn owner_of(&self, account: &AccountId) -> Option<&'static str> {
        self.inner
            .routes
            .read()
            .expect("routes lock")
            .get(account)
            .copied()
    }

    /// The member that listed a place to link a number from.
    pub fn owner_of_place(&self, place: &LinkPlace) -> Option<&'static str> {
        self.inner
            .places
            .lock()
            .expect("places lock")
            .get(place)
            .copied()
    }

    /// Follows the members' word on their credentials: `Some` once one has
    /// refused them, until that member is attached again or another
    /// refuses.
    pub fn refusals(&self) -> watch::Receiver<Option<Refusal>> {
        self.inner.refusals.subscribe()
    }
}

impl Inner {
    fn members(&self) -> Vec<Arc<Member>> {
        self.members.read().expect("members lock").clone()
    }

    fn member(&self, id: &str) -> Option<Arc<Member>> {
        self.members
            .read()
            .expect("members lock")
            .iter()
            .find(|member| member.id() == id)
            .cloned()
    }

    /// The member a number belongs to.
    fn of(&self, account: &AccountId) -> ProviderResult<Arc<Member>> {
        let id = self
            .routes
            .read()
            .expect("routes lock")
            .get(account)
            .copied();
        id.and_then(|id| self.member(id))
            .ok_or_else(|| ProviderError::Rejected {
                code: "unknown_account".into(),
                message: "This number is not linked here any more.".into(),
            })
    }

    /// The member a number belongs to, ready to be called.
    fn ready(&self, account: &AccountId) -> ProviderResult<Arc<Member>> {
        let member = self.of(account)?;
        member.check()?;
        Ok(member)
    }

    /// Remembers whose the numbers a member is known to have are.
    fn learn(&self, member: &Member) {
        let accounts = member.accounts.lock().expect("accounts lock");
        let Some(accounts) = accounts.as_ref() else {
            return;
        };
        let mut routes = self.routes.write().expect("routes lock");
        routes.retain(|_, owner| *owner != member.id());
        for account in accounts {
            routes.insert(account.id.clone(), member.id());
        }
    }

    /// Remembers a number a member just made or changed.
    fn keep(&self, member: &Member, account: &Account) {
        self.routes
            .write()
            .expect("routes lock")
            .insert(account.id.clone(), member.id());
        let mut accounts = member.accounts.lock().expect("accounts lock");
        let accounts = accounts.get_or_insert_with(Vec::new);
        match accounts.iter_mut().find(|known| known.id == account.id) {
            Some(known) => *known = account.clone(),
            None => accounts.push(account.clone()),
        }
    }

    /// Forgets a number its member deleted.
    fn forget(&self, member: &Member, account: &AccountId) {
        self.routes.write().expect("routes lock").remove(account);
        if let Some(accounts) = member.accounts.lock().expect("accounts lock").as_mut() {
            accounts.retain(|known| &known.id != account);
        }
    }

    /// Takes a member's answer. A refusal of its credentials sets the
    /// member aside and comes out as an ordinary rejection: the engine
    /// stops for good on `Unauthorized`, and the other members have not
    /// refused anything.
    fn heard<T>(&self, member: &Member, result: ProviderResult<T>) -> ProviderResult<T> {
        match result {
            Err(ProviderError::Unauthorized(reason)) => {
                if !member.refused.swap(true, Ordering::SeqCst) {
                    tracing::warn!(provider = member.id(), %reason, "a provider refused its credentials");
                    self.refusals.send_replace(Some(Refusal {
                        provider: member.id(),
                        reason: reason.clone(),
                    }));
                    // Its live updates are of no more use.
                    self.changed.send_modify(|count| *count += 1);
                }
                Err(ProviderError::Rejected {
                    code: SIGNED_OUT.into(),
                    message: reason,
                })
            }
            other => other,
        }
    }

    /// The member a new number is linked through.
    fn linker(&self, new: &NewAccount) -> ProviderResult<Arc<Member>> {
        let asked = self
            .links
            .lock()
            .expect("links lock")
            .get(&new.request_id)
            .copied();
        if let Some(member) = asked.and_then(|id| self.member(id)) {
            return Ok(member);
        }
        let member = match &new.place {
            // A place is one a member listed.
            Some(place) => {
                let id = self.places.lock().expect("places lock").get(place).copied();
                id.and_then(|id| self.member(id))
            }
            // No place: the first member that links without one.
            None => {
                let with_places: Vec<&'static str> = self
                    .places
                    .lock()
                    .expect("places lock")
                    .values()
                    .copied()
                    .collect();
                self.members().into_iter().find(|member| {
                    member.provider.capabilities().link_accounts
                        && !member.is_refused()
                        && !with_places.contains(&member.id())
                })
            }
        };
        let member = member.ok_or_else(|| ProviderError::Rejected {
            code: "no_place".into(),
            message: "Choose where the number connects from.".into(),
        })?;
        self.links
            .lock()
            .expect("links lock")
            .insert(new.request_id.clone(), member.id());
        Ok(member)
    }
}

/// Asks the member a number belongs to, and takes its answer.
macro_rules! route {
    ($self:ident, $account:expr, |$provider:ident| $call:expr) => {{
        let member = $self.inner.ready($account)?;
        let $provider = &member.provider;
        let answer = $call.await;
        $self.inner.heard(&member, answer)
    }};
}

#[async_trait]
impl Provider for HubProvider {
    fn id(&self) -> &'static str {
        HUB_ID
    }

    /// Everything any member supports: what one number cannot do is in
    /// [`capabilities_for`](Provider::capabilities_for).
    fn capabilities(&self) -> Capabilities {
        self.inner
            .members()
            .iter()
            .fold(Capabilities::none(), |all, member| {
                all.union(member.provider.capabilities())
            })
    }

    fn capabilities_for(&self, account: &AccountId) -> Capabilities {
        match self.inner.of(account) {
            Ok(member) => member.provider.capabilities_for(account),
            Err(_) => Capabilities::none(),
        }
    }

    fn owner_of(&self, account: &AccountId) -> &'static str {
        HubProvider::owner_of(self, account).unwrap_or(HUB_ID)
    }

    fn unavailable(&self, account: &AccountId, feature: Feature) -> bool {
        self.inner
            .of(account)
            .is_ok_and(|member| member.provider.unavailable(account, feature))
    }

    fn live_updates(&self) -> Option<LiveUpdates> {
        self.inner
            .members()
            .iter()
            .find_map(|member| member.provider.live_updates())
    }

    fn recheck(&self, account: &AccountId, feature: Feature) {
        if let Ok(member) = self.inner.of(account) {
            member.provider.recheck(account, feature);
        }
    }

    async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
        let mut all = Vec::new();
        for member in self.inner.members() {
            if !member.is_refused() {
                let listed = member.provider.list_accounts().await;
                match self.inner.heard(&member, listed) {
                    Ok(accounts) => {
                        *member.accounts.lock().expect("accounts lock") = Some(accounts);
                        self.inner.learn(&member);
                    }
                    Err(error) => {
                        // What it said last stands. With nothing said yet,
                        // its numbers are not known to be none: the whole
                        // list waits, or the client would forget them.
                        if member.accounts.lock().expect("accounts lock").is_none() {
                            return Err(error);
                        }
                        tracing::debug!(provider = member.id(), %error, "the numbers could not be listed");
                    }
                }
            }
            let known = member.accounts.lock().expect("accounts lock").clone();
            all.extend(known.unwrap_or_default());
        }
        Ok(all)
    }

    async fn list_chats(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Chat>> {
        route!(self, account, |p| p.list_chats(account, cursor))
    }

    async fn fetch_messages(
        &self,
        account: &AccountId,
        chat: &ChatId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> ProviderResult<Page<Message>> {
        route!(self, account, |p| p
            .fetch_messages(account, chat, cursor, limit))
    }

    async fn send(&self, message: OutgoingMessage) -> ProviderResult<SendReceipt> {
        let account = message.account_id.clone();
        route!(self, &account, |p| p.send(message))
    }

    async fn mark_read(
        &self,
        account: &AccountId,
        chat: &ChatId,
        up_to: Option<&MessageId>,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.mark_read(account, chat, up_to))
    }

    async fn mark_read_quietly(&self, account: &AccountId, chat: &ChatId) -> ProviderResult<()> {
        route!(self, account, |p| p.mark_read_quietly(account, chat))
    }

    async fn update_chat(
        &self,
        account: &AccountId,
        chat: &ChatId,
        change: ChatChange,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.update_chat(account, chat, change))
    }

    async fn delete_chat(&self, account: &AccountId, chat: &ChatId) -> ProviderResult<()> {
        route!(self, account, |p| p.delete_chat(account, chat))
    }

    async fn start_chat(&self, account: &AccountId, phone: &str) -> ProviderResult<Chat> {
        route!(self, account, |p| p.start_chat(account, phone))
    }

    async fn download_media(
        &self,
        account: &AccountId,
        media: &MediaRef,
    ) -> ProviderResult<MediaData> {
        route!(self, account, |p| p.download_media(account, media))
    }

    async fn fetch_media_reporting(
        &self,
        account: &AccountId,
        media: &MediaRef,
        limit: MediaLimit,
        progress: UploadProgress,
    ) -> ProviderResult<MediaData> {
        route!(self, account, |p| p
            .fetch_media_reporting(account, media, limit, progress))
    }

    async fn upload_media(
        &self,
        account: &AccountId,
        upload: MediaUpload,
        progress: UploadProgress,
    ) -> ProviderResult<MediaRef> {
        route!(self, account, |p| p.upload_media(account, upload, progress))
    }

    /// The smallest of the members' limits: a file that fits it fits
    /// whichever number it is sent from.
    fn media_upload_limit(&self) -> Option<u64> {
        self.inner
            .members()
            .iter()
            .filter_map(|member| member.provider.media_upload_limit())
            .min()
    }

    async fn media_upload_ready(&self) -> bool {
        for member in self.inner.members() {
            if !member.is_refused() && member.provider.media_upload_ready().await {
                return true;
            }
        }
        false
    }

    async fn fetch_media(
        &self,
        account: &AccountId,
        media: &MediaRef,
        limit: MediaLimit,
    ) -> ProviderResult<MediaData> {
        route!(self, account, |p| p.fetch_media(account, media, limit))
    }

    async fn fetch_avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
        known: Option<&str>,
    ) -> ProviderResult<AvatarAnswer> {
        route!(self, account, |p| p.fetch_avatar(account, subject, known))
    }

    async fn fetch_full_avatar(
        &self,
        account: &AccountId,
        subject: &ChatId,
    ) -> ProviderResult<Option<Avatar>> {
        route!(self, account, |p| p.fetch_full_avatar(account, subject))
    }

    async fn vote_poll(
        &self,
        account: &AccountId,
        chat: &ChatId,
        poll: &MessageId,
        choices: &[String],
    ) -> ProviderResult<Option<Message>> {
        route!(self, account, |p| p.vote_poll(account, chat, poll, choices))
    }

    async fn edit_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        text: &str,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p
            .edit_message(account, chat, message, text))
    }

    async fn delete_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        for_everyone: bool,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.delete_message(
            account,
            chat,
            message,
            for_everyone
        ))
    }

    async fn star_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        starred: bool,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p
            .star_message(account, chat, message, starred))
    }

    async fn forward_messages(
        &self,
        account: &AccountId,
        items: &[ForwardItem],
    ) -> ProviderResult<Vec<ProviderResult<SendReceipt>>> {
        route!(self, account, |p| p.forward_messages(account, items))
    }

    async fn list_favorite_stickers(
        &self,
        account: &AccountId,
    ) -> ProviderResult<Vec<FavoriteSticker>> {
        route!(self, account, |p| p.list_favorite_stickers(account))
    }

    async fn add_favorite_sticker(
        &self,
        account: &AccountId,
        sticker: StickerFile,
    ) -> ProviderResult<String> {
        route!(self, account, |p| p.add_favorite_sticker(account, sticker))
    }

    async fn remove_favorite_sticker(&self, account: &AccountId, id: &str) -> ProviderResult<()> {
        route!(self, account, |p| p.remove_favorite_sticker(account, id))
    }

    async fn list_contacts(
        &self,
        account: &AccountId,
        cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Contact>> {
        route!(self, account, |p| p.list_contacts(account, cursor))
    }

    async fn check_numbers(
        &self,
        account: &AccountId,
        phones: &[String],
    ) -> ProviderResult<Vec<NumberCheck>> {
        route!(self, account, |p| p.check_numbers(account, phones))
    }

    async fn lookup_contact(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Contact> {
        route!(self, account, |p| p.lookup_contact(account, contact))
    }

    async fn business_profile(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Option<BusinessProfile>> {
        route!(self, account, |p| p.business_profile(account, contact))
    }

    async fn list_blocked(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        route!(self, account, |p| p.list_blocked(account))
    }

    async fn set_blocked(
        &self,
        account: &AccountId,
        contact: &ContactId,
        blocked: bool,
        request_id: &str,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p
            .set_blocked(account, contact, blocked, request_id))
    }

    async fn own_profile(&self, account: &AccountId) -> ProviderResult<OwnProfile> {
        route!(self, account, |p| p.own_profile(account))
    }

    async fn update_profile(
        &self,
        account: &AccountId,
        change: &ProfileChange,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.update_profile(account, change))
    }

    async fn set_profile_picture(
        &self,
        account: &AccountId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        route!(self, account, |p| p.set_profile_picture(account, jpeg))
    }

    async fn list_groups(&self, account: &AccountId) -> ProviderResult<Vec<Group>> {
        route!(self, account, |p| p.list_groups(account))
    }

    async fn fetch_group(&self, account: &AccountId, group: &ChatId) -> ProviderResult<Group> {
        route!(self, account, |p| p.fetch_group(account, group))
    }

    async fn create_group(&self, account: &AccountId, new: &NewGroup) -> ProviderResult<Group> {
        route!(self, account, |p| p.create_group(account, new))
    }

    async fn update_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: &GroupChange,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.update_group(account, group, change))
    }

    async fn change_participants(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: ParticipantChange,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        route!(self, account, |p| p.change_participants(
            account, group, change, contacts, request_id
        ))
    }

    async fn set_group_picture(
        &self,
        account: &AccountId,
        group: &ChatId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        route!(self, account, |p| p.set_group_picture(account, group, jpeg))
    }

    async fn group_invite_link(
        &self,
        account: &AccountId,
        group: &ChatId,
        reset: bool,
        request_id: &str,
    ) -> ProviderResult<String> {
        route!(self, account, |p| p
            .group_invite_link(account, group, reset, request_id))
    }

    async fn join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> ProviderResult<Vec<JoinRequest>> {
        route!(self, account, |p| p.join_requests(account, group))
    }

    async fn answer_join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
        approve: bool,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        route!(self, account, |p| p.answer_join_requests(
            account, group, approve, contacts, request_id
        ))
    }

    async fn leave_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        request_id: &str,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.leave_group(account, group, request_id))
    }

    async fn invite_info(&self, account: &AccountId, code: &str) -> ProviderResult<Group> {
        route!(self, account, |p| p.invite_info(account, code))
    }

    async fn join_group(
        &self,
        account: &AccountId,
        code: &str,
        request_id: &str,
    ) -> ProviderResult<JoinOutcome> {
        route!(self, account, |p| p.join_group(account, code, request_id))
    }

    async fn link_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
        request_id: &str,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p
            .link_subgroup(account, community, group, request_id))
    }

    async fn unlink_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p
            .unlink_subgroup(account, community, group))
    }

    async fn community_participants(
        &self,
        account: &AccountId,
        community: &ChatId,
    ) -> ProviderResult<Vec<ContactId>> {
        route!(self, account, |p| p
            .community_participants(account, community))
    }

    async fn list_stories(&self, account: &AccountId) -> ProviderResult<Vec<Story>> {
        route!(self, account, |p| p.list_stories(account))
    }

    async fn post_story(&self, story: NewStory) -> ProviderResult<Story> {
        let account = story.account_id.clone();
        route!(self, &account, |p| p.post_story(story))
    }

    async fn delete_story(&self, account: &AccountId, story: &MessageId) -> ProviderResult<()> {
        route!(self, account, |p| p.delete_story(account, story))
    }

    async fn view_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        author: &ContactId,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.view_story(account, story, author))
    }

    async fn story_viewers(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> ProviderResult<Vec<StoryViewer>> {
        route!(self, account, |p| p.story_viewers(account, story))
    }

    async fn reply_to_story(
        &self,
        story: &Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        let account = message.account_id.clone();
        route!(self, &account, |p| p.reply_to_story(story, message))
    }

    async fn react_to_story(
        &self,
        story: &Story,
        message: OutgoingMessage,
    ) -> ProviderResult<SendReceipt> {
        let account = message.account_id.clone();
        route!(self, &account, |p| p.react_to_story(story, message))
    }

    async fn muted_story_authors(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        route!(self, account, |p| p.muted_story_authors(account))
    }

    async fn set_story_muted(
        &self,
        account: &AccountId,
        author: &ContactId,
        muted: bool,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.set_story_muted(account, author, muted))
    }

    async fn story_privacy(&self, account: &AccountId) -> ProviderResult<StoryPrivacy> {
        route!(self, account, |p| p.story_privacy(account))
    }

    async fn set_story_privacy(
        &self,
        account: &AccountId,
        privacy: &StoryPrivacy,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.set_story_privacy(account, privacy))
    }

    async fn list_incoming_webhooks(
        &self,
        account: &AccountId,
        chat: &ChatId,
    ) -> ProviderResult<Vec<IncomingWebhook>> {
        route!(self, account, |p| p.list_incoming_webhooks(account, chat))
    }

    async fn create_incoming_webhook(
        &self,
        account: &AccountId,
        chat: &ChatId,
        name: &str,
    ) -> ProviderResult<IncomingWebhook> {
        route!(self, account, |p| p
            .create_incoming_webhook(account, chat, name))
    }

    async fn update_incoming_webhook(
        &self,
        account: &AccountId,
        id: &str,
        change: IncomingWebhookChange,
    ) -> ProviderResult<IncomingWebhook> {
        route!(self, account, |p| p
            .update_incoming_webhook(account, id, change))
    }

    async fn delete_incoming_webhook(&self, account: &AccountId, id: &str) -> ProviderResult<()> {
        route!(self, account, |p| p.delete_incoming_webhook(account, id))
    }

    /// The places of every member that asks for one. A member that cannot
    /// say for now is left out: the others still link.
    async fn link_places(&self) -> ProviderResult<Vec<LinkPlace>> {
        let mut all = Vec::new();
        for member in self.inner.members() {
            if member.is_refused() {
                continue;
            }
            let listed = member.provider.link_places().await;
            match self.inner.heard(&member, listed) {
                Ok(places) => {
                    let mut owners = self.inner.places.lock().expect("places lock");
                    owners.retain(|_, owner| *owner != member.id());
                    for place in &places {
                        owners.insert(place.clone(), member.id());
                    }
                    all.extend(places);
                }
                Err(error) => {
                    tracing::debug!(provider = member.id(), %error, "the places could not be listed");
                }
            }
        }
        Ok(all)
    }

    /// Links through the member that listed the place asked for, or, with
    /// no place, through the first that needs none.
    async fn create_account(&self, new: &NewAccount) -> ProviderResult<LinkStatus> {
        let member = self.inner.linker(new)?;
        member.check()?;
        let answer = member.provider.create_account(new).await;
        let status = self.inner.heard(&member, answer)?;
        self.inner.keep(&member, &status.account);
        Ok(status)
    }

    async fn link_status(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        route!(self, account, |p| p.link_status(account))
    }

    async fn pairing_code(&self, account: &AccountId, phone: &str) -> ProviderResult<LinkStatus> {
        route!(self, account, |p| p.pairing_code(account, phone))
    }

    async fn scan_instead(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        route!(self, account, |p| p.scan_instead(account))
    }

    async fn update_account(
        &self,
        account: &AccountId,
        change: AccountChange,
    ) -> ProviderResult<Account> {
        let member = self.inner.ready(account)?;
        let answer = member.provider.update_account(account, change).await;
        let changed = self.inner.heard(&member, answer)?;
        self.inner.keep(&member, &changed);
        Ok(changed)
    }

    async fn reconnect_account(&self, account: &AccountId) -> ProviderResult<LinkStatus> {
        route!(self, account, |p| p.reconnect_account(account))
    }

    async fn unlink_account(&self, account: &AccountId) -> ProviderResult<Account> {
        let member = self.inner.ready(account)?;
        let answer = member.provider.unlink_account(account).await;
        let changed = self.inner.heard(&member, answer)?;
        self.inner.keep(&member, &changed);
        Ok(changed)
    }

    async fn delete_account(&self, account: &AccountId) -> ProviderResult<()> {
        let member = self.inner.ready(account)?;
        let answer = member.provider.delete_account(account).await;
        self.inner.heard(&member, answer)?;
        self.inner.forget(&member, account);
        Ok(())
    }

    async fn start_call(
        &self,
        account: &AccountId,
        chat: &ChatId,
        video: bool,
    ) -> ProviderResult<String> {
        route!(self, account, |p| p.start_call(account, chat, video))
    }

    async fn accept_call(&self, account: &AccountId, call: &str) -> ProviderResult<()> {
        route!(self, account, |p| p.accept_call(account, call))
    }

    async fn end_call(&self, account: &AccountId, call: &str) -> ProviderResult<()> {
        route!(self, account, |p| p.end_call(account, call))
    }

    async fn mute_call(&self, account: &AccountId, call: &str, muted: bool) -> ProviderResult<()> {
        route!(self, account, |p| p.mute_call(account, call, muted))
    }

    async fn invite_to_call(
        &self,
        account: &AccountId,
        call: &str,
        contact: &ContactId,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.invite_to_call(account, call, contact))
    }

    async fn set_call_camera(
        &self,
        account: &AccountId,
        call: &str,
        on: bool,
    ) -> ProviderResult<()> {
        route!(self, account, |p| p.set_call_camera(account, call, on))
    }

    fn call_video(&self, account: &AccountId, call: &str) -> Option<CallVideo> {
        self.inner
            .of(account)
            .ok()?
            .provider
            .call_video(account, call)
    }

    /// Every member is told: the devices are the machine's, not a
    /// number's.
    fn set_call_devices(&self, devices: CallDevices) {
        for member in self.inner.members() {
            member.provider.set_call_devices(devices.clone());
        }
    }

    fn call_cameras(&self) -> Vec<CallCamera> {
        let mut cameras: Vec<CallCamera> = Vec::new();
        for member in self.inner.members() {
            for camera in member.provider.call_cameras() {
                if !cameras.contains(&camera) {
                    cameras.push(camera);
                }
            }
        }
        cameras
    }

    /// The live updates of every member, as they come.
    ///
    /// The stream ends when any member's does, and at every change of
    /// membership: the engine then subscribes again and refreshes, which
    /// is how it catches up with what the member that went quiet missed
    /// and how it starts hearing a member that just joined. A member whose
    /// updates cannot be opened does not hold the others back: it is left
    /// out, and the stream ends by itself a little later to try it again.
    async fn subscribe(&self) -> ProviderResult<EventStream> {
        // Before the members are read: a change between the two cuts the
        // stream at once instead of being missed.
        let mut changed = self.inner.changed.subscribe();
        changed.borrow_and_update();
        let mut streams = Vec::new();
        for member in self.inner.members() {
            if member.is_refused() {
                continue;
            }
            let opened = member.provider.subscribe().await;
            match self.inner.heard(&member, opened) {
                Ok(events) => {
                    member.stream_failures.store(0, Ordering::SeqCst);
                    // `None` marks the end of this member's updates.
                    streams.push(
                        events
                            .map(Some)
                            .chain(futures::stream::once(async { None }))
                            .boxed(),
                    );
                }
                Err(error) => {
                    let failures = member.stream_failures.fetch_add(1, Ordering::SeqCst);
                    let wait = RETRY_FIRST
                        .saturating_mul(1 << failures.min(8))
                        .min(RETRY_MAX);
                    tracing::debug!(provider = member.id(), %error, ?wait, "the live updates could not be opened");
                    if !member.is_refused() {
                        streams.push(
                            futures::stream::once(async move {
                                tokio::time::sleep(wait).await;
                                None
                            })
                            .boxed(),
                        );
                    }
                }
            }
        }
        if streams.is_empty() {
            // Nobody to hear: quiet until somebody joins.
            streams.push(futures::stream::pending().boxed());
        }
        let cut = async move {
            // An error is the hub gone: nothing more comes either way.
            let _ = changed.changed().await;
        };
        let merged = futures::stream::select_all(streams)
            .take_while(|event| futures::future::ready(event.is_some()))
            .filter_map(futures::future::ready)
            .take_until(cut);
        Ok(merged.boxed())
    }
}
