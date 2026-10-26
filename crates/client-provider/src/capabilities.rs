//! What a provider can and cannot do.

use serde::{Deserialize, Serialize};

/// The features a provider supports.
///
/// The client reads this once per provider and hides whatever is missing:
/// no reaction picker without `reactions`, no attach button without
/// `media_upload`, and so on. Be honest here. A capability reported as
/// `true` and then answered with [`ProviderError::Unsupported`]
/// (crate::ProviderError) gives the user a button that does nothing.
///
/// New fields are added as the client grows, so build values with struct
/// update syntax from [`Capabilities::none`] to stay source-compatible:
///
/// ```
/// use client_provider::Capabilities;
///
/// let caps = Capabilities {
///     replies: true,
///     read_receipts: true,
///     ..Capabilities::none()
/// };
/// assert!(!caps.reactions);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// [`Provider::list_chats`](crate::Provider::list_chats) returns the
    /// account's real, complete chat list (names, unread counts, pins).
    /// When `false` the listing is a best-effort reconstruction, for example
    /// derived from recent messages, and the client treats it as partial.
    pub chat_list: bool,
    /// [`Provider::update_chat`](crate::Provider::update_chat) can pin,
    /// mute, archive and mark a chat as unread. When `false` the client
    /// shows those actions disabled.
    pub chat_state: bool,
    /// [`Provider::delete_chat`](crate::Provider::delete_chat) deletes a
    /// chat and its messages, on every device of the account.
    pub delete_chat: bool,
    /// [`Provider::start_chat`](crate::Provider::start_chat) can open a
    /// conversation with a phone number the account has never talked to.
    pub start_chat: bool,
    /// Numbers can be linked from the client:
    /// [`Provider::create_account`](crate::Provider::create_account) and
    /// [`Provider::link_status`](crate::Provider::link_status).
    pub link_accounts: bool,
    /// A number can be linked by typing a code on the phone, not only by
    /// scanning ([`NewAccount::pairing_phone`](crate::NewAccount)).
    pub link_by_code: bool,
    /// A number that is linking with a code can go back to the QR code
    /// without being created again
    /// ([`Provider::scan_instead`](crate::Provider::scan_instead)). When
    /// `false` the client offers the way back only until the code is asked
    /// for.
    pub link_back_to_scan: bool,
    /// Numbers can be renamed, reconnected, logged out and deleted.
    pub manage_accounts: bool,
    /// The provider can import the phone's recent chats when a number is
    /// linked, and says so per number
    /// ([`AccountSettings::history_import`](crate::AccountSettings)).
    pub history_import: bool,
    /// [`Provider::list_contacts`](crate::Provider::list_contacts) lists
    /// the account's address book.
    pub contacts: bool,
    /// [`Provider::check_numbers`](crate::Provider::check_numbers) says
    /// whether a number has WhatsApp before anything is sent to it.
    pub number_check: bool,
    /// Chats and messages carry the names the user saved in their address
    /// book. When `false`, only WhatsApp profile names or phone numbers are
    /// available.
    pub contact_names: bool,
    /// Group chats are listed and can be sent to.
    pub groups: bool,
    /// The event stream is pushed by the backend in real time. When `false`
    /// the provider polls behind the scenes: events still arrive, late, and
    /// presence is not available.
    pub realtime_push: bool,
    /// History can be fetched incrementally ("everything since X"). When
    /// `false` catching up after being offline means re-reading recent pages.
    pub incremental_sync: bool,
    /// Messages can quote another message.
    pub replies: bool,
    /// A message of a group can be answered in private: quoted in the
    /// chat with whoever wrote it
    /// ([`OutgoingMessage::reply_to`](crate::OutgoingMessage)).
    pub private_replies: bool,
    /// Reactions can be sent.
    pub reactions: bool,
    /// Sent messages can be edited.
    pub edits: bool,
    /// Sent messages can be deleted for everyone
    /// ([`Provider::delete_message`](crate::Provider::delete_message)).
    pub deletes: bool,
    /// Sent messages can be deleted for the account alone: gone here and
    /// on its other devices, still there for everybody else.
    pub delete_for_me: bool,
    /// Messages of other people can be deleted for the account alone.
    pub delete_received: bool,
    /// Messages can be starred and unstarred
    /// ([`Provider::star_message`](crate::Provider::star_message)).
    pub stars: bool,
    /// Messages can be pinned at the top of their chat, for everyone in
    /// it ([`Provider::pin_message`](crate::Provider::pin_message)).
    pub pins: bool,
    /// A sent message can be marked as forwarded
    /// ([`OutgoingMessage::forwarded`](crate::OutgoingMessage)), which is
    /// how a message is forwarded: its content sent again, with the mark.
    pub forwards: bool,
    /// Polls can be created
    /// ([`OutgoingContent::Poll`](crate::OutgoingContent)).
    pub polls: bool,
    /// [`Provider::vote_poll`](crate::Provider::vote_poll) casts the
    /// account's vote in a poll. When `false` a poll shows its tally and
    /// cannot be voted on.
    pub poll_votes: bool,
    /// Incoming media can be downloaded.
    pub media_download: bool,
    /// [`Provider::fetch_avatar`](crate::Provider::fetch_avatar) returns
    /// profile and group pictures.
    pub avatars: bool,
    /// Local files can be sent. When `false`,
    /// [`OutgoingContent::Media`](crate::OutgoingContent) is rejected.
    pub media_upload: bool,
    /// [`Provider::mark_read`](crate::Provider::mark_read) sends read
    /// receipts (blue ticks) to the other side.
    pub read_receipts: bool,
    /// [`Provider::mark_read_quietly`](crate::Provider::mark_read_quietly)
    /// clears a chat's unread count without sending read receipts.
    pub quiet_read: bool,
    /// Typing and recording indicators arrive as
    /// [`ProviderEvent::Presence`](crate::ProviderEvent).
    pub presence: bool,
    /// [`Provider::lookup_contact`](crate::Provider::lookup_contact) asks
    /// WhatsApp for a contact's "About" text, username and picture id.
    pub contact_lookup: bool,
    /// [`Provider::business_profile`](crate::Provider::business_profile)
    /// reads what a business says about itself.
    pub business_profiles: bool,
    /// Contacts can be blocked and unblocked, and the blocklist read.
    pub blocking: bool,
    /// The account's own name, "About" text and picture can be changed.
    pub profile_edit: bool,
    /// [`Provider::fetch_group`](crate::Provider::fetch_group) returns a
    /// group's details and participants.
    pub group_info: bool,
    /// Groups can be created.
    pub group_create: bool,
    /// An admin can add, remove, promote and demote participants and edit a
    /// group's subject, description, picture and settings.
    pub group_manage: bool,
    /// An admin can read and reset a group's invite link.
    pub group_invites: bool,
    /// An admin can list, approve and reject requests to join a group.
    pub group_join_requests: bool,
    /// The account can leave a group.
    pub group_leave: bool,
    /// The account can see a group from its invite link and join it
    /// ([`Provider::invite_info`](crate::Provider::invite_info),
    /// [`Provider::join_group`](crate::Provider::join_group)).
    pub group_join: bool,
    /// An admin can link a group to a community and take one out of it,
    /// create a group inside a community, and create a community
    /// ([`Provider::link_subgroup`](crate::Provider::link_subgroup),
    /// [`Provider::unlink_subgroup`](crate::Provider::unlink_subgroup),
    /// [`NewGroup::community`], [`NewGroup::in_community`]). Creating
    /// inside a community may still be refused for a number whose engine
    /// cannot do it.
    pub community_manage: bool,
    /// [`Provider::community_participants`](crate::Provider::community_participants)
    /// lists the people in all of a community's groups.
    pub community_members: bool,
    /// A sent text can mention people
    /// ([`OutgoingMessage::mentions`](crate::OutgoingMessage)).
    pub mentions: bool,
    /// The account's favorite stickers (the ones starred on the phone) can
    /// be listed, added to and removed from
    /// ([`Provider::list_favorite_stickers`](crate::Provider::list_favorite_stickers)).
    /// When `false` favorites live on this device alone.
    pub sticker_favorites: bool,
    /// Any message, whatever it carries, can be forwarded by naming it:
    /// [`Provider::forward_messages`](crate::Provider::forward_messages)
    /// passes the original on to other chats the way WhatsApp does, the
    /// provider reusing what it holds (a media reference included) and
    /// marking the copies as forwarded. When `false` only a text can be
    /// forwarded, by sending it again with
    /// [`OutgoingMessage::forwarded`](crate::OutgoingMessage) set
    /// ([`forwards`](Self::forwards)).
    pub forward_any: bool,
    /// A poll can be forwarded by naming it. Only looked at with
    /// [`forward_any`](Self::forward_any).
    pub forward_polls: bool,
    /// A calendar event can be forwarded by naming it. Only looked at
    /// with [`forward_any`](Self::forward_any).
    pub forward_events: bool,
    /// Own and contacts' stories can be listed ([`Provider::list_stories`]). Which of them it returns is `story_contacts`.
    pub story_list: bool,
    /// Contacts' stories are delivered, listed and pushed as events.
    pub story_contacts: bool,
    /// The account can post stories ([`Provider::post_story`]).
    pub story_post: bool,
    /// A story can be marked as seen, telling its author ([`Provider::view_story`]).
    pub story_view: bool,
    /// Who saw the account's own stories can be listed.
    pub story_viewers: bool,
    /// A story can be replied to ([`Provider::reply_to_story`]).
    pub story_reply: bool,
    /// A story can be reacted to ([`Provider::react_to_story`]).
    pub story_react: bool,
    /// A contact's stories can be muted, and the provider keeps that.
    pub story_mute: bool,
    /// Who sees the account's stories can be read.
    pub story_privacy: bool,
    /// Who sees the account's stories can be changed, lists included.
    pub story_privacy_edit: bool,
    /// The account's own stories can be taken down.
    pub story_delete: bool,
    /// Voice calls can be placed and answered
    /// ([`Provider::start_call`](crate::Provider::start_call)), and a call
    /// that comes in is announced
    /// ([`ProviderEvent::Call`](crate::ProviderEvent::Call)). The provider
    /// carries the sound itself.
    pub calls: bool,
    /// A call can carry video. Only looked at with [`calls`](Self::calls).
    pub video_calls: bool,
    /// A chat can be given incoming webhooks: URLs that send a message to
    /// it when something posts to them
    /// ([`Provider::list_incoming_webhooks`](crate::Provider::list_incoming_webhooks)).
    #[serde(default)]
    pub incoming_webhooks: bool,
}

impl Capabilities {
    /// Nothing supported. The starting point for struct update syntax.
    pub const fn none() -> Self {
        Self {
            chat_list: false,
            chat_state: false,
            start_chat: false,
            link_accounts: false,
            link_by_code: false,
            link_back_to_scan: false,
            manage_accounts: false,
            history_import: false,
            contacts: false,
            number_check: false,
            contact_names: false,
            groups: false,
            realtime_push: false,
            incremental_sync: false,
            replies: false,
            private_replies: false,
            reactions: false,
            edits: false,
            deletes: false,
            delete_for_me: false,
            delete_received: false,
            stars: false,
            pins: false,
            forwards: false,
            polls: false,
            poll_votes: false,
            media_download: false,
            avatars: false,
            media_upload: false,
            read_receipts: false,
            quiet_read: false,
            presence: false,
            contact_lookup: false,
            business_profiles: false,
            blocking: false,
            profile_edit: false,
            group_info: false,
            group_create: false,
            group_manage: false,
            group_invites: false,
            group_join_requests: false,
            group_leave: false,
            delete_chat: false,
            group_join: false,
            community_manage: false,
            community_members: false,
            mentions: false,
            sticker_favorites: false,
            forward_any: false,
            forward_polls: false,
            forward_events: false,
            story_list: false,
            story_contacts: false,
            story_post: false,
            story_view: false,
            story_viewers: false,
            story_reply: false,
            story_react: false,
            story_mute: false,
            story_privacy: false,
            story_privacy_edit: false,
            story_delete: false,
            calls: false,
            video_calls: false,
            incoming_webhooks: false,
        }
    }

    /// Everything supported. Useful for mocks and tests.
    pub const fn all() -> Self {
        Self {
            chat_list: true,
            chat_state: true,
            start_chat: true,
            link_accounts: true,
            link_by_code: true,
            link_back_to_scan: true,
            manage_accounts: true,
            history_import: true,
            contacts: true,
            number_check: true,
            contact_names: true,
            groups: true,
            realtime_push: true,
            incremental_sync: true,
            replies: true,
            private_replies: true,
            reactions: true,
            edits: true,
            deletes: true,
            delete_for_me: true,
            delete_received: true,
            stars: true,
            pins: true,
            forwards: true,
            polls: true,
            poll_votes: true,
            media_download: true,
            avatars: true,
            media_upload: true,
            read_receipts: true,
            quiet_read: true,
            presence: true,
            contact_lookup: true,
            business_profiles: true,
            blocking: true,
            profile_edit: true,
            group_info: true,
            group_create: true,
            group_manage: true,
            group_invites: true,
            group_join_requests: true,
            group_leave: true,
            delete_chat: true,
            group_join: true,
            community_manage: true,
            community_members: true,
            mentions: true,
            sticker_favorites: true,
            forward_any: true,
            forward_polls: true,
            forward_events: true,
            story_list: true,
            story_contacts: true,
            story_post: true,
            story_view: true,
            story_viewers: true,
            story_reply: true,
            story_react: true,
            story_mute: true,
            story_privacy: true,
            story_privacy_edit: true,
            story_delete: true,
            calls: true,
            video_calls: true,
            incoming_webhooks: true,
        }
    }

    /// What either of two providers supports: everything `self` does and
    /// everything `other` does. What a provider made of several answers
    /// for all of them at once.
    pub const fn union(self, other: Self) -> Self {
        Self {
            chat_list: self.chat_list || other.chat_list,
            chat_state: self.chat_state || other.chat_state,
            start_chat: self.start_chat || other.start_chat,
            link_accounts: self.link_accounts || other.link_accounts,
            link_by_code: self.link_by_code || other.link_by_code,
            link_back_to_scan: self.link_back_to_scan || other.link_back_to_scan,
            manage_accounts: self.manage_accounts || other.manage_accounts,
            history_import: self.history_import || other.history_import,
            contacts: self.contacts || other.contacts,
            number_check: self.number_check || other.number_check,
            contact_names: self.contact_names || other.contact_names,
            groups: self.groups || other.groups,
            realtime_push: self.realtime_push || other.realtime_push,
            incremental_sync: self.incremental_sync || other.incremental_sync,
            replies: self.replies || other.replies,
            private_replies: self.private_replies || other.private_replies,
            reactions: self.reactions || other.reactions,
            edits: self.edits || other.edits,
            deletes: self.deletes || other.deletes,
            delete_for_me: self.delete_for_me || other.delete_for_me,
            delete_received: self.delete_received || other.delete_received,
            stars: self.stars || other.stars,
            pins: self.pins || other.pins,
            forwards: self.forwards || other.forwards,
            polls: self.polls || other.polls,
            poll_votes: self.poll_votes || other.poll_votes,
            media_download: self.media_download || other.media_download,
            avatars: self.avatars || other.avatars,
            media_upload: self.media_upload || other.media_upload,
            read_receipts: self.read_receipts || other.read_receipts,
            quiet_read: self.quiet_read || other.quiet_read,
            presence: self.presence || other.presence,
            contact_lookup: self.contact_lookup || other.contact_lookup,
            business_profiles: self.business_profiles || other.business_profiles,
            blocking: self.blocking || other.blocking,
            profile_edit: self.profile_edit || other.profile_edit,
            group_info: self.group_info || other.group_info,
            group_create: self.group_create || other.group_create,
            group_manage: self.group_manage || other.group_manage,
            group_invites: self.group_invites || other.group_invites,
            group_join_requests: self.group_join_requests || other.group_join_requests,
            group_leave: self.group_leave || other.group_leave,
            delete_chat: self.delete_chat || other.delete_chat,
            group_join: self.group_join || other.group_join,
            community_manage: self.community_manage || other.community_manage,
            community_members: self.community_members || other.community_members,
            mentions: self.mentions || other.mentions,
            sticker_favorites: self.sticker_favorites || other.sticker_favorites,
            forward_any: self.forward_any || other.forward_any,
            forward_polls: self.forward_polls || other.forward_polls,
            forward_events: self.forward_events || other.forward_events,
            story_list: self.story_list || other.story_list,
            story_contacts: self.story_contacts || other.story_contacts,
            story_post: self.story_post || other.story_post,
            story_view: self.story_view || other.story_view,
            story_viewers: self.story_viewers || other.story_viewers,
            story_reply: self.story_reply || other.story_reply,
            story_react: self.story_react || other.story_react,
            story_mute: self.story_mute || other.story_mute,
            story_privacy: self.story_privacy || other.story_privacy,
            story_privacy_edit: self.story_privacy_edit || other.story_privacy_edit,
            story_delete: self.story_delete || other.story_delete,
            calls: self.calls || other.calls,
            video_calls: self.video_calls || other.video_calls,
            incoming_webhooks: self.incoming_webhooks || other.incoming_webhooks,
        }
    }
}

/// A part of a provider that its [`Capabilities`] promise and that may all
/// the same be missing for a while: a backend that does not have the
/// routes yet, or that turns the part on one account at a time.
///
/// Capabilities are read once and say what the provider is built to do.
/// [`Provider::unavailable`](crate::Provider::unavailable) says what, of
/// that, is known not to work right now. The client then shows "not
/// available yet" in that place, keeps everything else working, and asks
/// again later.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Forwarding by naming the message
    /// ([`Capabilities::forward_any`]).
    ForwardAny,
    /// The stickers starred on the account
    /// ([`Capabilities::sticker_favorites`]).
    StickerFavorites,
    /// Contacts' stories, their view receipts, and who saw the account's
    /// own ([`Capabilities::story_contacts`], `story_view`,
    /// `story_viewers`, `story_reply`, `story_react`).
    Stories,
}
