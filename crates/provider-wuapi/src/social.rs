//! Profiles and groups through the wuapi API: the calls, the mapping to
//! the neutral model and the wording of refusals.
//!
//! What the API cannot do, each marked `TODO(wuapi-api)` where it bites:
//!
//! * a group's `joinApproval` and `memberAddMode` can be set but are not
//!   in the `Group` object, so they cannot be read back;
//! * the own "About" text has no route of its own: it is read with a
//!   contact lookup of the account's own number, which WhatsApp may or may
//!   not answer;
//! * no route lists the groups two accounts share: the client derives
//!   them from `GET …/groups`, which carries every group's participants;
//! * `group.updated`, `group.join_request` and `blocklist.updated` exist
//!   as webhooks only, so a change made on the phone is seen when the
//!   group is opened again (or, for a rename, when the chat list shows it).

use crate::client::{WuapiClient, MAX_PAGE};
use crate::compat;
use crate::error::from_sdk;
use crate::mapping;
use base64::Engine as _;
use client_provider::{
    refusal, AccountId, BusinessHours, BusinessProfile, ChatId, Contact, ContactId, Group,
    GroupChange, GroupParticipant, GroupRole, JoinRequest, NewGroup, OwnProfile, ParticipantChange,
    ParticipantOutcome, ProfileChange, ProviderError, ProviderResult, Subgroup,
};
use wuapi::types as api;

/// A guard against a listing that never ends.
const MAX_ITEMS: usize = 50 * MAX_PAGE as usize;

// ----- mapping --------------------------------------------------------------

fn role(wire: &api::GroupParticipantRole) -> GroupRole {
    match wire {
        api::GroupParticipantRole::Owner => GroupRole::Owner,
        api::GroupParticipantRole::Admin => GroupRole::Admin,
        // A role newer than the SDK: the least it can be.
        _ => GroupRole::Member,
    }
}

/// `Group` -> [`Group`].
pub(crate) fn group(wire: &api::Group, subgroups: Vec<Subgroup>) -> Group {
    let filled = |text: &Option<String>| text.clone().filter(|text| !text.trim().is_empty());
    Group {
        id: ChatId::new(wire.id.clone()),
        account_id: AccountId::new(wire.account_id.clone()),
        subject: wire.name.clone(),
        description: filled(&wire.description),
        owner: filled(&wire.owner_id).map(ContactId::new),
        created_at: wire.created_at.as_deref().and_then(mapping::timestamp),
        community: wire.community,
        community_id: filled(&wire.community_id).map(ChatId::new),
        announcements: wire.default,
        announce: wire.announce,
        locked: wire.locked,
        // TODO(wuapi-api): `joinApproval` and `memberAddMode` are accepted
        // by PATCH but are not part of the Group object.
        join_approval: None,
        members_can_add: None,
        participants: wire
            .participants
            .iter()
            .map(|participant| GroupParticipant {
                contact: ContactId::new(participant.contact_id.clone()),
                name: filled(&participant.name),
                role: role(&participant.role),
            })
            .collect(),
        subgroups,
    }
}

/// `BusinessProfile` -> [`BusinessProfile`]. `profileOptions` is a free
/// map; what is known to be in it is taken, the rest left alone.
pub(crate) fn business(wire: &api::BusinessProfile) -> BusinessProfile {
    let filled = |text: &Option<String>| text.clone().filter(|text| !text.trim().is_empty());
    let option = |key: &str| {
        wire.profile_options
            .get(key)
            .filter(|value| !value.trim().is_empty())
            .cloned()
    };
    BusinessProfile {
        description: option("description"),
        address: filled(&wire.address),
        email: filled(&wire.email),
        websites: ["website", "website1", "website2"]
            .into_iter()
            .filter_map(option)
            .collect(),
        categories: wire
            .categories
            .iter()
            .map(|category| category.name.clone())
            .filter(|name| !name.trim().is_empty())
            .collect(),
        hours: wire
            .business_hours
            .iter()
            .map(|day| BusinessHours {
                day: day.day_of_week.clone(),
                open: filled(&day.open_time),
                close: filled(&day.close_time),
                mode: day.mode.clone(),
            })
            .collect(),
        time_zone: filled(&wire.time_zone),
    }
}

/// `ParticipantResult` -> [`ParticipantOutcome`]. The API's reasons are
/// the neutral ones already (`privacy_restricted`, `already_member`,
/// `not_on_whatsapp`); the few spellings of "not in the group" and "not
/// allowed" are folded into theirs.
pub(crate) fn outcome(wire: &api::ParticipantResult) -> ParticipantOutcome {
    let error = wire
        .error
        .clone()
        .filter(|code| !code.trim().is_empty())
        .map(|code| match code.as_str() {
            "not_member" | "not_a_member" | "not_participant" | "not_found" => {
                refusal::NOT_MEMBER.to_owned()
            }
            "forbidden" | "not_authorized" | "whatsapp_forbidden" | "not_admin" => {
                refusal::NOT_ADMIN.to_owned()
            }
            _ => code,
        });
    ParticipantOutcome {
        contact: ContactId::new(wire.contact_id.clone()),
        error,
        invite_code: wire.invite_code.clone().filter(|code| !code.is_empty()),
    }
}

/// What a refusal of a group route means, by what was being done.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Doing {
    /// Reading the group: only members can.
    Reading,
    /// Something only admins can do.
    Managing,
    /// Leaving.
    Leaving,
}

/// Words the API's refusals of group routes. `403 whatsapp_forbidden` is
/// "not a member, not an admin, or not allowed": which one follows from
/// what was asked. Transient failures pass through untouched.
fn group_refusal(error: ProviderError, doing: Doing) -> ProviderError {
    let ProviderError::Rejected { code, message } = error else {
        return error;
    };
    let rejected = |code: &str, message: &str| ProviderError::Rejected {
        code: code.to_owned(),
        message: message.to_owned(),
    };
    match (code.as_str(), doing) {
        ("whatsapp_forbidden", Doing::Managing) => {
            rejected(refusal::NOT_ADMIN, "Only admins of the group can do that.")
        }
        ("whatsapp_forbidden", _) => {
            rejected(refusal::NOT_MEMBER, "This number is not in the group.")
        }
        ("group_not_found" | "whatsapp_not_found", Doing::Leaving) => {
            rejected(refusal::NOT_MEMBER, "This number is not in the group.")
        }
        ("group_not_found" | "whatsapp_not_found", _) => rejected(
            refusal::NOT_FOUND,
            "WhatsApp does not know this group any more.",
        ),
        ("temporary_ban", _) => rejected(
            "temporary_ban",
            "WhatsApp has restricted this number for a while. Try again later.",
        ),
        _ => ProviderError::Rejected { code, message },
    }
}

fn picture(jpeg: &[u8]) -> api::PictureUploadRequest {
    let encoded = base64::engine::general_purpose::STANDARD.encode(jpeg);
    api::PictureUploadRequestVariant2::new(encoded).into()
}

fn ids(contacts: &[ContactId]) -> api::ContactIdsRequest {
    api::ContactIdsRequest::new(contacts.iter().map(ContactId::to_string).collect())
}

// ----- the calls ------------------------------------------------------------

impl WuapiClient {
    /// `POST …/contacts/lookup` for one contact: what WhatsApp says about
    /// them (About, username, picture id, business name).
    pub(crate) async fn lookup_contact(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Contact> {
        let request = api::ContactLookupRequest::new(vec![contact.to_string()]);
        let list = self
            .sdk()
            .contacts()
            .lookup(account.as_str(), request)
            .await
            .map_err(from_sdk)?;
        let wire = list.items.first().ok_or_else(|| ProviderError::Rejected {
            code: refusal::NOT_FOUND.into(),
            message: "WhatsApp does not know this contact.".into(),
        })?;
        let mut found = mapping::contact(wire);
        // Under the id that was asked about, whatever form the answer's
        // has: it is the chat's id, and what the store keys by. A lookup
        // has no names, so `name` must not carry the number as one.
        found.id = contact.clone();
        found.account_id = account.clone();
        found.name = None;
        Ok(found)
    }

    /// `GET …/contacts/{contactId}/business-profile`. `None` for a contact
    /// that is not a business.
    pub(crate) async fn business_profile(
        &self,
        account: &AccountId,
        contact: &ContactId,
    ) -> ProviderResult<Option<BusinessProfile>> {
        let answer = self
            .sdk()
            .contacts()
            .get_business_profile(account.as_str(), contact.as_str())
            .await
            .map_err(from_sdk);
        match answer {
            Ok(wire) => Ok(Some(business(&wire))),
            Err(ProviderError::Rejected { code, .. })
                if [
                    "business_profile_not_found",
                    "whatsapp_not_found",
                    "not_found",
                ]
                .contains(&code.as_str()) =>
            {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// `GET …/blocklist`, every page. WhatsApp often lists a contact by
    /// its hidden-number id, so each entry is returned under every id the
    /// API gives for it: the listed one, the number and the `lid:`.
    pub(crate) async fn blocked(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        let params = api::ContactsListBlockedParams {
            limit: Some(i64::from(MAX_PAGE)),
            ..Default::default()
        };
        let entries = self
            .sdk()
            .contacts()
            .list_blocked(account.as_str(), params)
            .to_vec_max(MAX_ITEMS)
            .await
            .map_err(from_sdk)?;
        let mut blocked: Vec<ContactId> = Vec::new();
        for entry in entries {
            for id in [Some(entry.contact_id), entry.phone, entry.lid]
                .into_iter()
                .flatten()
                .filter(|id| !id.is_empty())
            {
                let id = ContactId::new(id);
                if !blocked.contains(&id) {
                    blocked.push(id);
                }
            }
        }
        Ok(blocked)
    }

    /// `POST …/contacts/{contactId}/block` or `/unblock`.
    pub(crate) async fn set_blocked(
        &self,
        account: &AccountId,
        contact: &ContactId,
        blocked: bool,
        request_id: &str,
    ) -> ProviderResult<()> {
        let contacts = self.sdk().contacts();
        let request = if blocked {
            contacts.block(account.as_str(), contact.as_str())
        } else {
            contacts.unblock(account.as_str(), contact.as_str())
        };
        request.idempotency_key(request_id).await.map_err(from_sdk)
    }

    /// The account's own profile: the display name from the account
    /// object, the About text from a lookup of its own number.
    ///
    /// TODO(wuapi-api): there is no `GET …/profile`. The lookup is the
    /// only way to the About text; when WhatsApp does not answer it for
    /// the own number, the About is simply not known.
    pub(crate) async fn own_profile(&self, account: &AccountId) -> ProviderResult<OwnProfile> {
        let wire = self.account(account.as_str()).await?;
        let filled = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
        let about = match &wire.phone {
            Some(phone) => match self
                .lookup_contact(account, &ContactId::new(phone.clone()))
                .await
            {
                Ok(found) => found.about,
                Err(error) if error.is_transient() => return Err(error),
                Err(error @ ProviderError::Unauthorized(_)) => return Err(error),
                Err(_) => None,
            },
            None => None,
        };
        Ok(OwnProfile {
            name: filled(wire.profile_name),
            about,
        })
    }

    /// `PATCH …/profile`.
    pub(crate) async fn update_profile(
        &self,
        account: &AccountId,
        change: &ProfileChange,
    ) -> ProviderResult<()> {
        let mut request = api::ProfileUpdateRequest::default();
        match change {
            ProfileChange::Name(name) => request.name = Some(name.clone()),
            ProfileChange::About(about) => request.about = Some(about.clone()),
        }
        self.sdk()
            .profile()
            .update(account.as_str(), request)
            .await
            .map_err(from_sdk)
    }

    /// `PUT` or `DELETE …/profile/picture`. The picture goes as base64.
    pub(crate) async fn set_profile_picture(
        &self,
        account: &AccountId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        let profile = self.sdk().profile();
        match jpeg {
            Some(jpeg) => profile
                .set_picture(account.as_str(), picture(jpeg))
                .await
                .map(|set| set.id.filter(|id| !id.is_empty()))
                .map_err(from_sdk),
            None => profile
                .delete_picture(account.as_str())
                .await
                .map(|()| None)
                .map_err(from_sdk),
        }
    }

    /// `GET …/groups/{groupId}`, and for a community the groups it links.
    pub(crate) async fn group(&self, account: &AccountId, id: &ChatId) -> ProviderResult<Group> {
        let wire = compat::group(self.sdk().http(), account.as_str(), id.as_str())
            .await
            .map_err(|error| group_refusal(from_sdk(error), Doing::Reading))?
            .0;
        let subgroups = if wire.community {
            let params = api::GroupsListSubgroupsParams {
                limit: Some(i64::from(MAX_PAGE)),
                ..Default::default()
            };
            // Read-only information: without it the community still shows.
            self.sdk()
                .groups()
                .list_subgroups(account.as_str(), id.as_str(), params)
                .to_vec_max(MAX_ITEMS)
                .await
                .map(|linked| {
                    linked
                        .into_iter()
                        .map(|linked| Subgroup {
                            id: ChatId::new(linked.id),
                            subject: linked.name,
                            announcements: linked.default,
                        })
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        Ok(group(&wire, subgroups))
    }

    /// `POST …/groups`. `request_id` is the idempotency key.
    pub(crate) async fn create_group(
        &self,
        account: &AccountId,
        new: &NewGroup,
    ) -> ProviderResult<Group> {
        let mut request = api::GroupCreateRequest::new(new.subject.clone());
        // A community may start with nobody else in it.
        if !(new.community && new.participants.is_empty()) {
            request.participants =
                Some(new.participants.iter().map(ContactId::to_string).collect());
        }
        request.community = new.community.then_some(true);
        request.community_id = new.in_community.as_ref().map(ChatId::to_string);
        let wire = compat::create_group(self.sdk().http(), account.as_str(), &request)
            .idempotency_key(&new.request_id)
            .await
            .map_err(|error| match from_sdk(error) {
                // The account's engine cannot put a group in a community
                // yet: not a failure to try again.
                ProviderError::Rejected { code, .. }
                    if code == "not_supported" && new.in_community.is_some() =>
                {
                    ProviderError::Rejected {
                        code,
                        message: "This number cannot create a group inside a community yet."
                            .to_owned(),
                    }
                }
                other => other,
            })?
            .0;
        Ok(group(&wire, Vec::new()))
    }

    /// `PATCH …/groups/{groupId}`: one field at a time.
    pub(crate) async fn update_group(
        &self,
        account: &AccountId,
        id: &ChatId,
        change: &GroupChange,
    ) -> ProviderResult<()> {
        let mut request = api::GroupUpdateRequest::default();
        match change {
            GroupChange::Subject(subject) => request.name = Some(subject.clone()),
            GroupChange::Description(text) => request.description = Some(text.clone()),
            GroupChange::Announce(on) => request.announce = Some(*on),
            GroupChange::Locked(on) => request.locked = Some(*on),
            GroupChange::JoinApproval(on) => request.join_approval = Some(*on),
            GroupChange::MembersCanAdd(on) => {
                request.member_add_mode = Some(if *on {
                    api::GroupUpdateRequestMemberAddMode::AllMembers
                } else {
                    api::GroupUpdateRequestMemberAddMode::Admins
                })
            }
        }
        compat::update_group(self.sdk().http(), account.as_str(), id.as_str(), &request)
            .await
            .map(drop)
            .map_err(|error| group_refusal(from_sdk(error), Doing::Managing))
    }

    /// `POST …/groups/{groupId}/participants/{add,remove,promote,demote}`.
    pub(crate) async fn change_participants(
        &self,
        account: &AccountId,
        id: &ChatId,
        change: ParticipantChange,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        let groups = self.sdk().groups();
        let (account, id, request) = (account.as_str(), id.as_str(), ids(contacts));
        let request = match change {
            ParticipantChange::Add => groups.add_participants(account, id, request),
            ParticipantChange::Remove => groups.remove_participants(account, id, request),
            ParticipantChange::Promote => groups.promote_participants(account, id, request),
            ParticipantChange::Demote => groups.demote_participants(account, id, request),
        };
        let answered = request
            .idempotency_key(request_id)
            .await
            .map_err(|error| group_refusal(from_sdk(error), Doing::Managing))?;
        Ok(answered.items.iter().map(outcome).collect())
    }

    /// `PUT` or `DELETE …/groups/{groupId}/picture`.
    pub(crate) async fn set_group_picture(
        &self,
        account: &AccountId,
        id: &ChatId,
        jpeg: Option<&[u8]>,
    ) -> ProviderResult<Option<String>> {
        let groups = self.sdk().groups();
        match jpeg {
            Some(jpeg) => groups
                .set_picture(account.as_str(), id.as_str(), picture(jpeg))
                .await
                .map(|set| set.id.filter(|id| !id.is_empty())),
            None => groups
                .delete_picture(account.as_str(), id.as_str())
                .await
                .map(|()| None),
        }
        .map_err(|error| group_refusal(from_sdk(error), Doing::Managing))
    }

    /// `GET …/invite-link`, or `POST …/invite-link/reset`.
    pub(crate) async fn invite_link(
        &self,
        account: &AccountId,
        id: &ChatId,
        reset: bool,
        request_id: &str,
    ) -> ProviderResult<String> {
        let groups = self.sdk().groups();
        let answer = if reset {
            groups
                .reset_invite_link(account.as_str(), id.as_str())
                .idempotency_key(request_id)
                .await
        } else {
            groups.get_invite_link(account.as_str(), id.as_str()).await
        };
        answer
            .map(|link| link.url)
            .map_err(|error| group_refusal(from_sdk(error), Doing::Managing))
    }

    /// `GET …/join-requests`, every page.
    pub(crate) async fn join_requests(
        &self,
        account: &AccountId,
        id: &ChatId,
    ) -> ProviderResult<Vec<JoinRequest>> {
        let params = api::GroupsListJoinRequestsParams {
            limit: Some(i64::from(MAX_PAGE)),
            ..Default::default()
        };
        let pending = self
            .sdk()
            .groups()
            .list_join_requests(account.as_str(), id.as_str(), params)
            .to_vec_max(MAX_ITEMS)
            .await
            .map_err(|error| group_refusal(from_sdk(error), Doing::Managing))?;
        Ok(pending
            .into_iter()
            .map(|request| JoinRequest {
                contact: ContactId::new(request.contact_id),
                requested_at: request.requested_at.as_deref().and_then(mapping::timestamp),
            })
            .collect())
    }

    /// `POST …/join-requests/approve` or `/reject`.
    pub(crate) async fn answer_join_requests(
        &self,
        account: &AccountId,
        id: &ChatId,
        approve: bool,
        contacts: &[ContactId],
        request_id: &str,
    ) -> ProviderResult<Vec<ParticipantOutcome>> {
        let groups = self.sdk().groups();
        let (account, id, request) = (account.as_str(), id.as_str(), ids(contacts));
        let request = if approve {
            groups.approve_join_requests(account, id, request)
        } else {
            groups.reject_join_requests(account, id, request)
        };
        let answered = request
            .idempotency_key(request_id)
            .await
            .map_err(|error| group_refusal(from_sdk(error), Doing::Managing))?;
        Ok(answered.items.iter().map(outcome).collect())
    }

    /// `POST …/groups/{groupId}/subgroups`. `request_id` is the
    /// idempotency key.
    pub(crate) async fn link_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
        request_id: &str,
    ) -> ProviderResult<()> {
        self.sdk()
            .groups()
            .link_subgroup(
                account.as_str(),
                community.as_str(),
                api::SubgroupLinkRequest::new(group.to_string()),
            )
            .idempotency_key(request_id)
            .await
            .map_err(|error| group_refusal(from_sdk(error), Doing::Managing))
    }

    /// `DELETE …/groups/{groupId}/subgroups/{subgroupId}`.
    pub(crate) async fn unlink_subgroup(
        &self,
        account: &AccountId,
        community: &ChatId,
        group: &ChatId,
    ) -> ProviderResult<()> {
        self.sdk()
            .groups()
            .unlink_subgroup(account.as_str(), community.as_str(), group.as_str())
            .await
            .map_err(|error| group_refusal(from_sdk(error), Doing::Managing))
    }

    /// `GET …/groups/{groupId}/community-participants`, every page.
    pub(crate) async fn community_participants(
        &self,
        account: &AccountId,
        community: &ChatId,
    ) -> ProviderResult<Vec<ContactId>> {
        let params = api::GroupsListCommunityParticipantsParams {
            limit: Some(i64::from(MAX_PAGE)),
            ..Default::default()
        };
        let listed = self
            .sdk()
            .groups()
            .list_community_participants(account.as_str(), community.as_str(), params)
            .to_vec_max(MAX_ITEMS)
            .await
            .map_err(|error| group_refusal(from_sdk(error), Doing::Reading))?;
        Ok(listed
            .into_iter()
            .map(|person| ContactId::new(person.contact_id))
            .collect())
    }

    /// `POST …/groups/{groupId}/leave`.
    pub(crate) async fn leave_group(
        &self,
        account: &AccountId,
        id: &ChatId,
        request_id: &str,
    ) -> ProviderResult<()> {
        self.sdk()
            .groups()
            .leave(account.as_str(), id.as_str())
            .idempotency_key(request_id)
            .await
            .map_err(|error| group_refusal(from_sdk(error), Doing::Leaving))
    }
}
