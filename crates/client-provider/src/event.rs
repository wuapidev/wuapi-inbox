//! Live updates pushed from a provider to the client.

use crate::ids::{AccountId, ChatId, ContactId, MessageId};
use crate::model::{Chat, ConnectionState, Contact, DeliveryStatus, Message, PresenceState};
use crate::stories::{Story, StoryViewer};
use futures::stream::BoxStream;

/// Something changed on the provider's side.
///
/// Events are hints to update the local store, not a reliable log: the
/// client applies them idempotently and re-reads history after a gap. It is
/// therefore safe, and expected, to deliver an event more than once or to
/// repeat the current state after a reconnect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderEvent {
    /// A message is new or changed (edited, deleted, re-delivered). Carries
    /// the full message; the client inserts or replaces by id.
    MessageUpserted(Message),

    /// The delivery status of a message moved. Statuses only move forward;
    /// the client ignores updates that would move one back.
    MessageStatusChanged {
        /// The account the message belongs to.
        account_id: AccountId,
        /// The chat the message belongs to.
        chat_id: ChatId,
        /// The message.
        message_id: MessageId,
        /// Its new status.
        status: DeliveryStatus,
    },

    /// A chat is new or its metadata changed (title, pin, mute, unread
    /// count). Carries the full chat.
    ChatUpdated(Chat),

    /// A contact's name, number or picture became known or changed. The
    /// client uses contacts to name the senders of group messages.
    ContactUpdated(Contact),

    /// A contact started or stopped typing, recording, or being online.
    /// Ephemeral: never stored.
    Presence {
        /// The account that observed it.
        account_id: AccountId,
        /// The chat it happened in.
        chat_id: ChatId,
        /// Who.
        contact_id: ContactId,
        /// What they are doing.
        state: PresenceState,
    },

    /// Something about a group changed: its participants, its admins, its
    /// subject, description or settings. A hint without the details: the
    /// client reads the group again.
    GroupChanged {
        /// The account that is in the group.
        account_id: AccountId,
        /// The group.
        group_id: ChatId,
    },

    /// An account connected, dropped or was logged out.
    ConnectionChanged {
        /// The account.
        account_id: AccountId,
        /// Its new state.
        state: ConnectionState,
    },

    /// A story appeared or changed: a contact posted one, the account's
    /// own was posted from another device, or the number who saw it moved.
    StoryUpserted(Story),

    /// A story is gone: its author took it down, or it expired.
    StoryRemoved {
        /// The account.
        account_id: AccountId,
        /// The story.
        story_id: MessageId,
    },

    /// Somebody saw one of the account's own stories.
    StoryViewed {
        /// The account.
        account_id: AccountId,
        /// The story.
        story_id: MessageId,
        /// Who, and when.
        viewer: StoryViewer,
    },

    /// A contact's stories were muted or unmuted, on another device of
    /// the account.
    StoryMuteChanged {
        /// The account.
        account_id: AccountId,
        /// The author.
        contact: ContactId,
        /// Muted now.
        muted: bool,
    },
}

/// The stream returned by [`Provider::subscribe`](crate::Provider::subscribe).
///
/// It ends when the provider's transport gives up; the client then calls
/// `subscribe` again with backoff, so a provider does not have to hide
/// reconnection inside the stream (though it may).
pub type EventStream = BoxStream<'static, ProviderEvent>;
