//! Profiles and groups: who a contact is, who the account is, and the
//! groups it belongs to or runs.
//!
//! Everything here is optional for a provider. Each operation has a
//! capability flag and a default implementation that answers
//! [`ProviderError::Unsupported`](crate::ProviderError::Unsupported).

use crate::ids::{AccountId, ChatId, ContactId};
use crate::model::Timestamp;
use serde::{Deserialize, Serialize};

/// Machine-readable reasons a profile or group operation is refused, used
/// as [`ProviderError::Rejected::code`](crate::ProviderError::Rejected) and
/// as [`ParticipantOutcome::error`]. A provider maps its backend's codes to
/// these where they mean the same; the client words them for the user.
pub mod refusal {
    /// The account is not an admin of the group (or not in it any more).
    pub const NOT_ADMIN: &str = "not_admin";
    /// The contact does not let this account add them to groups.
    pub const PRIVACY: &str = "privacy_restricted";
    /// The contact is already in the group.
    pub const ALREADY_MEMBER: &str = "already_member";
    /// The contact is not in the group.
    pub const NOT_MEMBER: &str = "not_member";
    /// The number has no WhatsApp.
    pub const NOT_ON_WHATSAPP: &str = "not_on_whatsapp";
    /// The group, or the contact, is not known to WhatsApp (any more).
    pub const NOT_FOUND: &str = "not_found";

    /// A forward was refused: the message was deleted.
    pub const FORWARD_DELETED: &str = "forward_deleted";
    /// A forward was refused: the message is view-once.
    pub const FORWARD_VIEW_ONCE: &str = "forward_view_once";
    /// A forward was refused: this kind of message is not forwarded.
    pub const FORWARD_KIND: &str = "forward_kind";
    /// A forward was refused: the message has not reached WhatsApp.
    pub const FORWARD_NOT_SENT: &str = "forward_not_sent";
    /// A forward was refused: the backend has no file for the message.
    pub const FORWARD_NO_FILE: &str = "forward_no_file";
    /// A forward was refused: WhatsApp no longer has the file.
    pub const FORWARD_FILE_GONE: &str = "forward_file_gone";
    /// A forward was refused: the file is larger than the backend moves.
    pub const FORWARD_TOO_LARGE: &str = "forward_too_large";
}

/// What a participant may do in a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupRole {
    /// A regular participant.
    Member,
    /// An admin.
    Admin,
    /// The creator: an admin nobody can demote.
    Owner,
}

impl GroupRole {
    /// True for admins and the owner.
    pub fn is_admin(self) -> bool {
        !matches!(self, Self::Member)
    }
}

/// One member of a group.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupParticipant {
    /// Who.
    pub contact: ContactId,
    /// Their name as the provider knows it, when it does. The client
    /// prefers the name saved in the address book.
    pub name: Option<String>,
    /// Member, admin or owner.
    pub role: GroupRole,
}

/// A group linked to a community.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subgroup {
    /// The linked group.
    pub id: ChatId,
    /// Its subject.
    pub subject: String,
    /// It is the community's announcement group.
    pub announcements: bool,
}

/// A group, with its participants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    /// The group's chat id.
    pub id: ChatId,
    /// The account that is in the group.
    pub account_id: AccountId,
    /// The group's name.
    pub subject: String,
    /// Its description, when it has one.
    pub description: Option<String>,
    /// Who created it, when known.
    pub owner: Option<ContactId>,
    /// When it was created, when known.
    pub created_at: Option<Timestamp>,
    /// It is a community: it links other groups (see `subgroups`).
    pub community: bool,
    /// Only admins can send messages.
    pub announce: bool,
    /// Only admins can edit the subject, description and picture.
    pub locked: bool,
    /// New members need an admin's approval. `None`: the provider cannot
    /// read this setting (it may still be able to set it).
    pub join_approval: Option<bool>,
    /// Any member can add people, not only admins. `None`: not readable.
    pub members_can_add: Option<bool>,
    /// Everyone in the group, the account included.
    pub participants: Vec<GroupParticipant>,
    /// The groups a community links. Empty for a plain group, and when
    /// the provider does not say.
    pub subgroups: Vec<Subgroup>,
}

/// A group to create.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewGroup {
    /// Its name.
    pub subject: String,
    /// Who to put in it, besides the account. Not empty.
    pub participants: Vec<ContactId>,
    /// Made once per attempt by the client and sent again on every retry,
    /// so that a request repeated after a dropped connection creates one
    /// group, not two.
    pub request_id: String,
}

/// A change to a group's details or settings. Every variant sets a value,
/// so applying one twice is the same as applying it once.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupChange {
    /// The group's name.
    Subject(String),
    /// The description. Empty removes it.
    Description(String),
    /// Whether only admins can send messages.
    Announce(bool),
    /// Whether only admins can edit the group's details.
    Locked(bool),
    /// Whether new members need an admin's approval.
    JoinApproval(bool),
    /// Whether any member can add people.
    MembersCanAdd(bool),
}

/// What to do with some participants of a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParticipantChange {
    /// Put them in the group.
    Add,
    /// Take them out.
    Remove,
    /// Make them admins.
    Promote,
    /// Make them regular participants again.
    Demote,
}

/// How a change went for one participant. Changes are answered per
/// contact: some may go through while others are refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticipantOutcome {
    /// Who.
    pub contact: ContactId,
    /// Why it was refused for this contact (see [`refusal`]), or `None`
    /// when it worked.
    pub error: Option<String>,
    /// An invite the contact has to accept instead of being added, when
    /// their privacy settings do not allow adding them.
    pub invite_code: Option<String>,
}

impl ParticipantOutcome {
    /// True when the change was made for this contact.
    pub fn worked(&self) -> bool {
        self.error.is_none() && self.invite_code.is_none()
    }
}

/// Someone asking to join a group that needs approval.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JoinRequest {
    /// Who asks.
    pub contact: ContactId,
    /// Since when, if the provider says.
    pub requested_at: Option<Timestamp>,
}

/// The opening hours of a business on one day.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessHours {
    /// The day, as the provider names it (`monday`).
    pub day: String,
    /// When it opens (`09:00`), when it has hours that day.
    pub open: Option<String>,
    /// When it closes.
    pub close: Option<String>,
    /// The provider's word for the day's mode (`open_24h`, `closed`,
    /// `specific_hours`, `appointment_only`).
    pub mode: String,
}

/// What a business says about itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessProfile {
    /// Its description.
    pub description: Option<String>,
    /// Its address.
    pub address: Option<String>,
    /// Its e-mail address.
    pub email: Option<String>,
    /// Its web sites.
    pub websites: Vec<String>,
    /// What kind of business it is.
    pub categories: Vec<String>,
    /// When it is open.
    pub hours: Vec<BusinessHours>,
    /// The time zone of `hours`.
    pub time_zone: Option<String>,
}

impl BusinessProfile {
    /// True when the profile says nothing at all.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The account's own WhatsApp profile, as far as the provider can read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnProfile {
    /// The name others see. `None`: the provider cannot read it.
    pub name: Option<String>,
    /// The "About" text. `None`: the provider cannot read it.
    pub about: Option<String>,
}

/// A change to the account's own profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProfileChange {
    /// The name others see.
    Name(String),
    /// The "About" text.
    About(String),
}
