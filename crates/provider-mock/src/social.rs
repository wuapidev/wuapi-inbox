//! Profiles and groups in the mock: groups with participants and roles,
//! a blocklist, contact and business profiles, the account's own profile.
//!
//! The groups are the seeded group chats: their members are the people the
//! thread was written by, the account is an admin of each, and the first
//! member is its owner. Tests change that with the helpers here, and can
//! make the next calls fail.

use crate::seed;
use crate::{MockProvider, State};
use client_provider::{
    refusal, AccountId, BusinessProfile, Chat, ChatId, ChatKind, Contact, ContactId, Group,
    GroupChange, GroupParticipant, GroupRole, JoinRequest, NewGroup, OwnProfile, ParticipantChange,
    ParticipantOutcome, ProfileChange, ProviderError, ProviderEvent, ProviderResult, Timestamp,
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
            announce: false,
            locked: false,
            join_approval: Some(false),
            members_can_add: Some(true),
            participants,
            subgroups: Vec::new(),
        };
        state.social.groups.insert(key.clone(), made);
    }
    Ok(state.social.groups.get_mut(&key).expect("just inserted"))
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
        let mut groups = Vec::new();
        for id in ids {
            let found = group_mut(&mut state, account, &id)?;
            if my_role(found).is_some() {
                groups.push(found.clone());
            }
        }
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
        Ok(found.clone())
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
        if new.subject.trim().is_empty() || new.participants.is_empty() {
            return Err(rejected(
                "invalid_request",
                "A group needs a name and at least one participant.",
            ));
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
            community: false,
            announce: false,
            locked: false,
            join_approval: Some(false),
            members_can_add: Some(true),
            participants,
            subgroups: Vec::new(),
        };
        state.world.chats.push(Chat {
            id: id.clone(),
            account_id: account.clone(),
            kind: ChatKind::Group,
            title: new.subject.clone(),
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
            .insert((account.clone(), id.clone()), Vec::new());
        state
            .social
            .created
            .insert(new.request_id.clone(), id.clone());
        state
            .social
            .groups
            .insert((account.clone(), id), made.clone());
        Ok(made)
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
