//! Profiles and groups in the mock: groups with participants and roles,
//! a blocklist, contact and business profiles, the account's own profile.
//!
//! The groups are the seeded group chats: their members are the people the
//! thread was written by, the account is an admin of each, and the first
//! member is its owner. Tests change that with the helpers here, and can
//! make the next calls fail.

use crate::{community, seed};
use crate::{MockProvider, State};
use client_provider::{
    refusal, AccountId, BusinessProfile, Chat, ChatId, ChatKind, Contact, ContactId, Group,
    GroupChange, GroupParticipant, GroupRole, JoinRequest, NewGroup, OwnProfile, ParticipantChange,
    ParticipantOutcome, ProfileChange, ProviderError, ProviderEvent, ProviderResult, Subgroup,
    Timestamp,
};
use std::collections::{HashMap, VecDeque};

/// What the mock keeps for profiles and groups.
#[derive(Default)]
pub(crate) struct Social {
    groups: HashMap<(AccountId, ChatId), Group>,
    /// Errors the next profile and group calls answer with, one per call.
    failures: VecDeque<ProviderError>,
    /// The name of every profile and group call made.
    calls: Vec<&'static str>,
    /// Contacts whose privacy settings do not let them be added.
    no_adds: Vec<ContactId>,
    blocked: HashMap<AccountId, Vec<ContactId>>,
    abouts: HashMap<ContactId, String>,
    businesses: HashMap<ContactId, (String, BusinessProfile)>,
    profiles: HashMap<AccountId, OwnProfile>,
    /// The own profile cannot be read back, as with some backends.
    unreadable_profile: bool,
    requests: HashMap<(AccountId, ChatId), Vec<JoinRequest>>,
    /// How many times each group's invite link was reset.
    link_resets: HashMap<(AccountId, ChatId), u32>,
    /// Groups created, by request id: a repeat finds the same group.
    created: HashMap<String, ChatId>,
    /// Answers already given, by request id: a repeat gets the same one.
    answered: HashMap<String, Vec<ParticipantOutcome>>,
    next_group: u32,
    /// The community each linked group is in, and whether the group is
    /// that community's announcement group.
    links: HashMap<(AccountId, ChatId), (ChatId, bool)>,
    /// Groups the listing of every group leaves out.
    unlisted: Vec<(AccountId, ChatId)>,
}

impl Social {
    /// What the demo data starts with: its community and the groups the
    /// community links.
    pub(crate) fn seeded() -> Self {
        let account = AccountId::new(community::COMMUNITY_ACCOUNT);
        let parent = ChatId::new(community::COMMUNITY);
        let mut social = Self::default();
        let mut participants = vec![GroupParticipant {
            contact: community::me(),
            name: None,
            role: GroupRole::Admin,
        }];
        for (index, name) in community::MEMBERS.into_iter().enumerate() {
            participants.push(GroupParticipant {
                contact: seed::contact_for(name),
                name: Some(name.to_owned()),
                role: if index == 0 {
                    GroupRole::Owner
                } else {
                    GroupRole::Member
                },
            });
        }
        social.groups.insert(
            (account.clone(), parent.clone()),
            Group {
                id: parent.clone(),
                account_id: account.clone(),
                subject: community::COMMUNITY_NAME.to_owned(),
                description: Some("Everything about the school, in one place.".to_owned()),
                owner: Some(seed::contact_for(community::MEMBERS[0])),
                created_at: Some(Timestamp::from_millis(1_700_000_000_000)),
                community: true,
                community_id: None,
                announcements: false,
                announce: true,
                locked: true,
                join_approval: Some(false),
                members_can_add: Some(false),
                participants,
                subgroups: Vec::new(),
            },
        );
        let announcements = ChatId::new(community::COMMUNITY_ANNOUNCEMENTS);
        social
            .links
            .insert((account.clone(), announcements), (parent.clone(), true));
        for group in community::COMMUNITY_GROUPS {
            social.links.insert(
                (account.clone(), ChatId::new(group)),
                (parent.clone(), false),
            );
        }
        // A group the community links that this account is not in: no chat,
        // and no participants it could see.
        let unjoined = ChatId::new(community::COMMUNITY_UNJOINED);
        social.groups.insert(
            (account.clone(), unjoined.clone()),
            Group {
                id: unjoined.clone(),
                account_id: account.clone(),
                subject: community::COMMUNITY_UNJOINED_NAME.to_owned(),
                description: None,
                owner: Some(seed::contact_for(community::MEMBERS[0])),
                created_at: Some(Timestamp::from_millis(1_700_000_000_000)),
                community: false,
                community_id: Some(parent.clone()),
                announcements: false,
                announce: false,
                locked: false,
                join_approval: Some(false),
                members_can_add: Some(false),
                participants: Vec::new(),
                subgroups: Vec::new(),
            },
        );
        social.links.insert((account, unjoined), (parent, false));
        social
    }
}

fn rejected(code: &str, message: &str) -> ProviderError {
    ProviderError::Rejected {
        code: code.into(),
        message: message.into(),
    }
}

fn not_admin() -> ProviderError {
    rejected(refusal::NOT_ADMIN, "Only admins of the group can do that.")
}

/// Notes a call and takes the failure queued for it, if any.
fn enter(state: &mut State, call: &'static str) -> ProviderResult<()> {
    state.social.calls.push(call);
    match state.social.failures.pop_front() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// The group behind a group chat, made from the seeded members the first
/// time it is asked for.
fn group_mut<'a>(
    state: &'a mut State,
    account: &AccountId,
    group: &ChatId,
) -> ProviderResult<&'a mut Group> {
    let key = (account.clone(), group.clone());
    if !state.social.groups.contains_key(&key) {
        let chat = state
            .world
            .chats
            .iter()
            .find(|c| &c.id == group && &c.account_id == account && c.kind == ChatKind::Group)
            .ok_or_else(|| rejected(refusal::NOT_FOUND, "There is no such group."))?;
        let members = state.world.members.get(&key).cloned().unwrap_or_default();
        let mut participants = vec![GroupParticipant {
            contact: seed::self_contact(account),
            name: None,
            role: GroupRole::Admin,
        }];
        for (index, name) in members.iter().enumerate() {
            participants.push(GroupParticipant {
                contact: seed::contact_for(name),
                name: Some(name.clone()),
                role: if index == 0 {
                    GroupRole::Owner
                } else {
                    GroupRole::Member
                },
            });
        }
        let made = Group {
            id: group.clone(),
            account_id: account.clone(),
            subject: chat.title.clone(),
            description: None,
            owner: members.first().map(|name| seed::contact_for(name)),
            created_at: Some(Timestamp::from_millis(1_700_000_000_000)),
            community: false,
            community_id: None,
            announcements: false,
            announce: false,
            locked: false,
            join_approval: Some(false),
            members_can_add: Some(true),
            participants,
            subgroups: Vec::new(),
        };
        state.social.groups.insert(key.clone(), made);
    }
    let link = state.social.links.get(&key).cloned();
    let found = state.social.groups.get_mut(&key).expect("just inserted");
    found.announcements = link
        .as_ref()
        .is_some_and(|(_, announcements)| *announcements);
    found.community_id = link.map(|(community, _)| community);
    Ok(found)
}

/// The groups a community links, its announcement group first, then by
/// name.
fn subgroups_of(state: &State, account: &AccountId, community: &ChatId) -> Vec<Subgroup> {
    let mut linked: Vec<Subgroup> = state
        .social
        .links
        .iter()
        .filter(|((of, _), (parent, _))| of == account && parent == community)
        .map(|((_, group), (_, announcements))| Subgroup {
            id: group.clone(),
            subject: state
                .world
                .chats
                .iter()
                .find(|chat| &chat.account_id == account && &chat.id == group)
                .map(|chat| chat.title.clone())
                // A group the account is not in has no chat.
                .or_else(|| {
                    state
                        .social
                        .groups
                        .get(&(account.clone(), group.clone()))
                        .map(|found| found.subject.clone())
                })
                .unwrap_or_default(),
            announcements: *announcements,
        })
        .collect();
    linked.sort_by(|a, b| {
        b.announcements
            .cmp(&a.announcements)
            .then_with(|| a.subject.cmp(&b.subject))
    });
    linked
}

fn my_role(group: &Group) -> Option<GroupRole> {
    let me = seed::self_contact(&group.account_id);
    group
        .participants
        .iter()
        .find(|p| p.contact == me)
        .map(|p| p.role)
}

/// The group, if the account is an admin of it.
fn as_admin<'a>(
    state: &'a mut State,
    account: &AccountId,
    group: &ChatId,
) -> ProviderResult<&'a mut Group> {
    let found = group_mut(state, account, group)?;
    match my_role(found) {
        Some(role) if role.is_admin() => Ok(found),
        _ => Err(not_admin()),
    }
}

impl MockProvider {
    // ----- for tests ----------------------------------------------------

    /// Makes the next profile and group calls fail with the given errors,
    /// one per call, before they work again.
    pub fn fail_next_social(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().social.failures.extend(errors);
    }

    /// The names of the profile and group calls made so far, in order.
    pub fn social_calls(&self) -> Vec<&'static str> {
        self.state().social.calls.clone()
    }

    /// How many times one profile or group call was made.
    pub fn social_call_count(&self, call: &str) -> usize {
        let state = self.state();
        state.social.calls.iter().filter(|c| **c == call).count()
    }

    /// The mock's own copy of a group.
    pub fn group(&self, account: &AccountId, group: &ChatId) -> Option<Group> {
        group_mut(&mut self.state(), account, group).ok().cloned()
    }

    /// A group as [`Provider::fetch_group`](client_provider::Provider::fetch_group)
    /// answers it now, without counting as a call: a community with the
    /// groups it links.
    pub fn fetch_group_now(&self, account: &AccountId, group: &ChatId) -> Group {
        let mut state = self.state();
        let mut found = group_mut(&mut state, account, group)
            .expect("the group exists")
            .clone();
        if found.community {
            found.subgroups = subgroups_of(&state, account, group);
        }
        found
    }

    /// The id the account goes by in its groups.
    pub fn self_contact(&self, account: &AccountId) -> ContactId {
        seed::self_contact(account)
    }

    /// Changes a participant's role as if it had been done on a phone, or
    /// takes them out (`None`). Announced as a group change.
    pub fn set_group_role(
        &self,
        account: &AccountId,
        group: &ChatId,
        contact: &ContactId,
        role: Option<GroupRole>,
    ) {
        if let Ok(found) = group_mut(&mut self.state(), account, group) {
            found.participants.retain(|p| &p.contact != contact);
            if let Some(role) = role {
                found.participants.push(GroupParticipant {
                    contact: contact.clone(),
                    name: None,
                    role,
                });
            }
        }
        self.emit(ProviderEvent::GroupChanged {
            account_id: account.clone(),
            group_id: group.clone(),
        });
    }

    /// Links a group to a community as if it had been done on a phone.
    /// Announced as a change of the community.
    pub fn link_on_phone(&self, account: &AccountId, community: &ChatId, group: &ChatId) {
        self.state()
            .social
            .links
            .insert((account.clone(), group.clone()), (community.clone(), false));
        self.emit(ProviderEvent::CommunityChanged {
            account_id: account.clone(),
            community_id: community.clone(),
            groups: vec![group.clone()],
        });
    }

    /// Leaves a group out of the listing of every group from now on, as
    /// a backend does with a community this number is not listed in. It
    /// can still be read on its own.
    pub fn unlist_group(&self, account: &AccountId, group: &ChatId) {
        self.state()
            .social
            .unlisted
            .push((account.clone(), group.clone()));
    }

    /// Takes a group out of its community as if it had been done on a
    /// phone. Announced as a change of the community it was in.
    pub fn unlink_on_phone(&self, account: &AccountId, group: &ChatId) {
        let was = self
            .state()
            .social
            .links
            .remove(&(account.clone(), group.clone()));
        if let Some((community, _)) = was {
            self.emit(ProviderEvent::CommunityChanged {
                account_id: account.clone(),
                community_id: community,
                groups: vec![group.clone()],
            });
        }
    }

    /// Changes a group's subject as if it had been done on a phone: the
    /// chat list carries the new name.
    pub fn rename_group(&self, account: &AccountId, group: &ChatId, subject: &str) {
        let chat = {
            let mut state = self.state();
            if let Ok(found) = group_mut(&mut state, account, group) {
                found.subject = subject.to_owned();
            }
            let chat = state
                .world
                .chats
                .iter_mut()
                .find(|c| &c.id == group && &c.account_id == account);
            chat.map(|chat| {
                chat.title = subject.to_owned();
                chat.clone()
            })
        };
        if let Some(chat) = chat {
            self.emit(ProviderEvent::ChatUpdated(chat));
        }
    }

    /// A contact whose privacy settings do not let anyone add them to a
    /// group: adding answers an invite instead.
    pub fn restrict_adds(&self, contact: &ContactId) {
        self.state().social.no_adds.push(contact.clone());
    }

    /// The account's blocklist, as the mock holds it.
    pub fn blocked(&self, account: &AccountId) -> Vec<ContactId> {
        let state = self.state();
        state
            .social
            .blocked
            .get(account)
            .cloned()
            .unwrap_or_default()
    }

    /// What a lookup of `contact` says their "About" text is.
    pub fn set_about(&self, contact: &ContactId, about: &str) {
        let mut state = self.state();
        state
            .social
            .abouts
            .insert(contact.clone(), about.to_owned());
    }

    /// Makes `contact` a business with this verified name and profile.
    pub fn set_business(&self, contact: &ContactId, name: &str, profile: BusinessProfile) {
        let mut state = self.state();
        state
            .social
            .businesses
            .insert(contact.clone(), (name.to_owned(), profile));
    }

    /// The account's own profile, as the mock holds it.
    pub fn own_profile_of(&self, account: &AccountId) -> OwnProfile {
        let state = self.state();
        state
            .social
            .profiles
            .get(account)
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the own profile can be read back. When not, reading it
    /// answers no name and no About.
    pub fn unreadable_profile(&self, unreadable: bool) {
        self.state().social.unreadable_profile = unreadable;
    }

    /// Someone asks to join a group.
    pub fn add_join_request(&self, account: &AccountId, group: &ChatId, contact: &ContactId) {
        let mut state = self.state();
        let requests = state
            .social
            .requests
            .entry((account.clone(), group.clone()))
            .or_default();
        requests.push(JoinRequest {
            contact: contact.clone(),
            requested_at: Some(Timestamp::now()),
        });
    }

    // ----- the provider's side ------------------------------------------

    pub(crate) fn mock_lookup_contact(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Contact> {
        let mut state = self.state();
        enter(&mut state, "lookup_contact")?;
        let mut found = Contact::new(account.clone(), contact.clone());
        found.about = Some(
            state
                .social
                .abouts
                .get(contact)
                .cloned()
                .unwrap_or_else(|| "Hey there! I am using WhatsApp.".to_owned()),
        );
        found.business_name = state
            .social
            .businesses
            .get(contact)
            .map(|(name, _)| name.clone());
        Ok(found)
    }

    pub(crate) fn mock_business_profile(
        &self,
        contact: &ContactId,
    ) -> ProviderResult<Option<BusinessProfile>> {
        let mut state = self.state();
        enter(&mut state, "business_profile")?;
        Ok(state
            .social
            .businesses
            .get(contact)
            .map(|(_, profile)| profile.clone()))
    }

    pub(crate) fn mock_list_blocked(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        let mut state = self.state();
        enter(&mut state, "list_blocked")?;
        Ok(state
            .social
            .blocked
            .get(account)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) fn mock_set_blocked(
        &self,
        account: &AccountId,
        contact: &ContactId,
        blocked: bool,
    ) -> ProviderResult<()> {
        let mut state = self.state();
        enter(&mut state, "set_blocked")?;
        let list = state.social.blocked.entry(account.clone()).or_default();
        list.retain(|known| known != contact);
        if blocked {
            list.push(contact.clone());
        }
        Ok(())
    }

    pub(crate) fn mock_own_profile(&self, account: &AccountId) -> ProviderResult<OwnProfile> {
        let mut state = self.state();
        enter(&mut state, "own_profile")?;
        if state.social.unreadable_profile {
            return Ok(OwnProfile::default());
        }
        let display = state
            .world
            .accounts
            .iter()
            .find(|known| &known.id == account)
            .map(|known| known.display_name.clone());
        let stored = state
            .social
            .profiles
            .get(account)
            .cloned()
            .unwrap_or_default();
        Ok(OwnProfile {
            name: stored.name.or(display),
            about: Some(stored.about.unwrap_or_else(|| "Available".to_owned())),
        })
    }

    pub(crate) fn mock_update_profile(
        &self,
        account: &AccountId,
        change: &ProfileChange,
    ) -> ProviderResult<()> {
        let mut state = self.state();
        enter(&mut state, "update_profile")?;
        let profile = state.social.profiles.entry(account.clone()).or_default();
        match change {
            ProfileChange::Name(name) => profile.name = Some(name.clone()),
            ProfileChange::About(about) => profile.about = Some(about.clone()),
        }
        Ok(())
    }

    /// Sets or removes a picture: the account's own (`subject` is its own
    /// id) or a group's.
    pub(crate) fn mock_set_picture(
        &self,
        call: &'static str,
        account: &AccountId,
        subject: ChatId,
        group: bool,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        let mut state = self.state();
        enter(&mut state, call)?;
        if group {
            as_admin(&mut state, account, &subject)?;
        }
        let key = (account.clone(), subject);
        match jpeg {
            Some(bytes) => {
                let id = format!("pic-{}", state.avatars.len() + state.social.calls.len());
                state.avatars.insert(key, (id.clone(), bytes.to_vec()));
                Ok(Some(id))
            }
            None => {
                state.avatars.remove(&key);
                Ok(None)
            }
        }
    }

    pub(crate) fn mock_list_groups(&self, account: &AccountId) -> ProviderResult<Vec<Group>> {
        let mut state = self.state();
        enter(&mut state, "list_groups")?;
        let ids: Vec<ChatId> = state
            .world
            .chats
            .iter()
            .filter(|c| &c.account_id == account && c.kind == ChatKind::Group)
            .map(|c| c.id.clone())
            .collect();
        // A community has no chat. As with a backend whose listing does
        // not carry them, the groups it links are left out here: reading
        // the community on its own gives them.
        let mut groups: Vec<Group> = state
            .social
            .groups
            .values()
            .filter(|group| &group.account_id == account && group.community)
            .cloned()
            .collect();
        groups.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        for id in ids {
            let found = group_mut(&mut state, account, &id)?;
            if my_role(found).is_some() {
                groups.push(found.clone());
            }
        }
        let unlisted = &state.social.unlisted;
        groups.retain(|group| !unlisted.contains(&(group.account_id.clone(), group.id.clone())));
        Ok(groups)
    }

    pub(crate) fn mock_fetch_group(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> ProviderResult<Group> {
        let mut state = self.state();
        enter(&mut state, "fetch_group")?;
        let found = group_mut(&mut state, account, group)?;
        if my_role(found).is_none() {
            return Err(rejected(
                refusal::NOT_MEMBER,
                "This number is not in the group.",
            ));
        }
        let mut found = found.clone();
        if found.community {
            found.subgroups = subgroups_of(&state, account, group);
        }
        Ok(found)
    }

    pub(crate) fn mock_create_group(
        &self,
        account: &AccountId,
        new: &NewGroup,
    ) -> ProviderResult<Group> {
        let mut state = self.state();
        enter(&mut state, "create_group")?;
        if let Some(existing) = state.social.created.get(&new.request_id).cloned() {
            return group_mut(&mut state, account, &existing).cloned();
        }
        if new.subject.trim().is_empty() || (new.participants.is_empty() && !new.community) {
            return Err(rejected(
                "invalid_request",
                "A group needs a name and at least one participant.",
            ));
        }
        if new.community && new.in_community.is_some() {
            return Err(rejected(
                "invalid_request",
                "A community cannot be created inside another.",
            ));
        }
        if let Some(parent) = &new.in_community {
            let found = as_admin(&mut state, account, parent)?;
            if !found.community {
                return Err(rejected(
                    "invalid_request",
                    "That group is not a community.",
                ));
            }
        }
        state.social.next_group += 1;
        let id = ChatId::new(format!("group:new{}", state.social.next_group));
        let mut participants = vec![GroupParticipant {
            contact: seed::self_contact(account),
            name: None,
            role: GroupRole::Owner,
        }];
        for contact in &new.participants {
            // Whoever does not let themselves be added is left out.
            if !state.social.no_adds.contains(contact) {
                participants.push(GroupParticipant {
                    contact: contact.clone(),
                    name: None,
                    role: GroupRole::Member,
                });
            }
        }
        let made = Group {
            id: id.clone(),
            account_id: account.clone(),
            subject: new.subject.clone(),
            description: None,
            owner: Some(seed::self_contact(account)),
            created_at: Some(Timestamp::now()),
            community: new.community,
            community_id: new.in_community.clone(),
            announcements: false,
            announce: false,
            locked: false,
            join_approval: Some(false),
            members_can_add: Some(true),
            participants,
            subgroups: Vec::new(),
        };
        // A community has no chat; its announcement group is made with it.
        let mut chats = vec![(id.clone(), new.subject.clone())];
        if new.community {
            chats.clear();
            let announcements = ChatId::new(format!("{}announcements", id.as_str()));
            state
                .social
                .links
                .insert((account.clone(), announcements.clone()), (id.clone(), true));
            chats.push((announcements, "Announcements".to_owned()));
        }
        for (chat, title) in chats {
            state.world.chats.push(Chat {
                id: chat.clone(),
                account_id: account.clone(),
                kind: ChatKind::Group,
                title,
                avatar: None,
                unread_count: 0,
                pinned: false,
                muted: false,
                archived: false,
                last_message: None,
                unknown: Default::default(),
                picture_id: None,
                pinned_at: None,
            });
            state
                .world
                .messages
                .insert((account.clone(), chat), Vec::new());
        }
        if let Some(parent) = &new.in_community {
            state
                .social
                .links
                .insert((account.clone(), id.clone()), (parent.clone(), false));
        }
        state
            .social
            .created
            .insert(new.request_id.clone(), id.clone());
        state
            .social
            .groups
            .insert((account.clone(), id.clone()), made.clone());
        drop(state);
        if let Some(parent) = &new.in_community {
            self.emit(ProviderEvent::CommunityChanged {
                account_id: account.clone(),
                community_id: parent.clone(),
                groups: vec![id],
            });
        }
        Ok(made)
    }

    pub(crate) fn mock_link_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
    ) -> ProviderResult<()> {
        let mut state = self.state();
        enter(&mut state, "link_subgroup")?;
        if !as_admin(&mut state, account, community)?.community {
            return Err(rejected(
                "invalid_request",
                "That group is not a community.",
            ));
        }
        let key = (account.clone(), group.clone());
        // Admin of both, as the API asks.
        if as_admin(&mut state, account, group)?.community {
            return Err(rejected(
                "invalid_request",
                "A community cannot be linked to another.",
            ));
        }
        match state.social.links.get(&key) {
            Some((parent, _)) if parent == community => return Ok(()),
            Some(_) => {
                return Err(rejected(
                    "already_in_community",
                    "That group is already in a community.",
                ))
            }
            None => {}
        }
        state.social.links.insert(key, (community.clone(), false));
        drop(state);
        self.emit(ProviderEvent::CommunityChanged {
            account_id: account.clone(),
            community_id: community.clone(),
            groups: vec![group.clone()],
        });
        Ok(())
    }

    pub(crate) fn mock_unlink_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
    ) -> ProviderResult<()> {
        let mut state = self.state();
        enter(&mut state, "unlink_subgroup")?;
        as_admin(&mut state, account, community)?;
        let key = (account.clone(), group.clone());
        match state.social.links.get(&key) {
            Some((parent, true)) if parent == community => {
                return Err(rejected(
                    "invalid_request",
                    "The announcements group stays in its community.",
                ))
            }
            Some((parent, false)) if parent == community => {}
            // Not linked there: done already.
            _ => return Ok(()),
        }
        state.social.links.remove(&key);
        drop(state);
        self.emit(ProviderEvent::CommunityChanged {
            account_id: account.clone(),
            community_id: community.clone(),
            groups: vec![group.clone()],
        });
        Ok(())
    }

    pub(crate) fn mock_community_participants(
        &self,
        account: &AccountId,
        community: &ChatId,
    ) -> ProviderResult<Vec<ContactId>> {
        let mut state = self.state();
        enter(&mut state, "community_participants")?;
        let found = group_mut(&mut state, account, community)?;
        if !found.community {
            return Err(rejected(
                "invalid_request",
                "That group is not a community.",
            ));
        }
        Ok(found
            .participants
            .iter()
            .map(|participant| participant.contact.clone())
            .collect())
    }

    pub(crate) fn mock_update_group(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: &GroupChange,
    ) -> ProviderResult<()> {
        let mut state = self.state();
        enter(&mut state, "update_group")?;
        let found = as_admin(&mut state, account, group)?;
        match change {
            GroupChange::Subject(subject) => found.subject = subject.clone(),
            GroupChange::Description(text) => {
                found.description = Some(text.clone()).filter(|text| !text.is_empty())
            }
            GroupChange::Announce(on) => found.announce = *on,
            GroupChange::Locked(on) => found.locked = *on,
            GroupChange::JoinApproval(on) => found.join_approval = Some(*on),
            GroupChange::MembersCanAdd(on) => found.members_can_add = Some(*on),
        }
        if let GroupChange::Subject(subject) = change {
            if let Some(chat) = state
                .world
                .chats
                .iter_mut()
                .find(|c| &c.id == group && &c.account_id == account)
            {
                chat.title = subject.clone();
            }
        }
        Ok(())
    }

    pub(crate) fn mock_change_participants(
        &self,
        account: &AccountId,
        group: &ChatId,
        change: ParticipantChange,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        let mut state = self.state();
        enter(&mut state, "change_participants")?;
        if let Some(answer) = state.social.answered.get(request_id) {
            return Ok(answer.clone());
        }
        let no_adds = state.social.no_adds.clone();
        let found = as_admin(&mut state, account, group)?;
        let mut outcomes = Vec::new();
        for contact in contacts {
            let place = found
                .participants
                .iter()
                .position(|p| &p.contact == contact);
            let mut outcome = ParticipantOutcome {
                contact: contact.clone(),
                error: None,
                invite_code: None,
            };
            match (change, place) {
                (ParticipantChange::Add, Some(_)) => {
                    outcome.error = Some(refusal::ALREADY_MEMBER.to_owned())
                }
                (ParticipantChange::Add, None) if no_adds.contains(contact) => {
                    outcome.error = Some(refusal::PRIVACY.to_owned());
                    outcome.invite_code = Some("INVITE123".to_owned());
                }
                (ParticipantChange::Add, None) => found.participants.push(GroupParticipant {
                    contact: contact.clone(),
                    name: None,
                    role: GroupRole::Member,
                }),
                (_, None) => outcome.error = Some(refusal::NOT_MEMBER.to_owned()),
                (ParticipantChange::Remove, Some(at)) => {
                    found.participants.remove(at);
                }
                (ParticipantChange::Promote, Some(at)) => {
                    if found.participants[at].role == GroupRole::Member {
                        found.participants[at].role = GroupRole::Admin;
                    }
                }
                (ParticipantChange::Demote, Some(at)) => {
                    if found.participants[at].role == GroupRole::Owner {
                        outcome.error = Some(refusal::NOT_ADMIN.to_owned());
                    } else {
                        found.participants[at].role = GroupRole::Member;
                    }
                }
            }
            outcomes.push(outcome);
        }
        state
            .social
            .answered
            .insert(request_id.to_owned(), outcomes.clone());
        Ok(outcomes)
    }

    pub(crate) fn mock_invite_link(
        &self,
        account: &AccountId,
        group: &ChatId,
        reset: bool,
    ) -> ProviderResult<String> {
        let mut state = self.state();
        enter(&mut state, "group_invite_link")?;
        as_admin(&mut state, account, group)?;
        let resets = state
            .social
            .link_resets
            .entry((account.clone(), group.clone()))
            .or_default();
        if reset {
            *resets += 1;
        }
        let slug: String = group
            .as_str()
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect();
        Ok(format!("https://chat.whatsapp.com/{slug}{resets}"))
    }

    pub(crate) fn mock_join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> ProviderResult<Vec<JoinRequest>> {
        let mut state = self.state();
        enter(&mut state, "join_requests")?;
        as_admin(&mut state, account, group)?;
        let key = (account.clone(), group.clone());
        Ok(state.social.requests.get(&key).cloned().unwrap_or_default())
    }

    pub(crate) fn mock_answer_join_requests(
        &self,
        account: &AccountId,
        group: &ChatId,
        approve: bool,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        let mut state = self.state();
        enter(&mut state, "answer_join_requests")?;
        if let Some(answer) = state.social.answered.get(request_id) {
            return Ok(answer.clone());
        }
        as_admin(&mut state, account, group)?;
        let key = (account.clone(), group.clone());
        let mut outcomes = Vec::new();
        for contact in contacts {
            let pending = state.social.requests.entry(key.clone()).or_default();
            let asked = pending.iter().any(|request| &request.contact == contact);
            pending.retain(|request| &request.contact != contact);
            if asked && approve {
                group_mut(&mut state, account, group)?
                    .participants
                    .push(GroupParticipant {
                        contact: contact.clone(),
                        name: None,
                        role: GroupRole::Member,
                    });
            }
            outcomes.push(ParticipantOutcome {
                contact: contact.clone(),
                error: (!asked).then(|| refusal::NOT_FOUND.to_owned()),
                invite_code: None,
            });
        }
        state
            .social
            .answered
            .insert(request_id.to_owned(), outcomes.clone());
        Ok(outcomes)
    }

    pub(crate) fn mock_leave_group(
        &self,
        account: &AccountId,
        group: &ChatId,
    ) -> ProviderResult<()> {
        let mut state = self.state();
        enter(&mut state, "leave_group")?;
        let me = seed::self_contact(account);
        let found = group_mut(&mut state, account, group)?;
        found.participants.retain(|p| p.contact != me);
        Ok(())
    }
}
