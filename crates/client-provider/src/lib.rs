//! The provider extension point.
//!
//! This crate is the only thing an adapter depends on. It contains:
//!
//! * [`Provider`], the trait a backend implements;
//! * the neutral model the trait speaks ([`Account`], [`Chat`], [`Message`],
//!   [`Contact`], [`DeliveryStatus`] and the id newtypes);
//! * [`Capabilities`], through which a provider says what it supports;
//! * [`ProviderEvent`], the live updates a provider pushes;
//! * [`ProviderError`], which tells the client whether to retry.
//!
//! It deliberately has no I/O, no storage and no UI dependencies. See
//! `docs/PROVIDERS.md` in the repository for a walkthrough of writing an
//! adapter, and the `provider-mock` and `provider-wuapi` crates for two
//! complete ones.

#![warn(missing_docs)]

mod capabilities;
mod content;
mod error;
mod event;
mod ids;
mod model;
mod provider;
mod social;
mod stickers;
mod stories;

pub use capabilities::{Capabilities, Feature};
pub use content::{
    mention_handle, CalendarEvent, ContactCard, EventCall, EventPlace, GeoPoint, LinkPreview,
    Location, Mention, MessageExtras, Party, Poll, PollOption, SystemEvent, SystemKind,
};
pub use error::{ProviderError, ProviderResult};
pub use event::{EventStream, ProviderEvent};
pub use ids::{AccountId, ChatId, ClientMessageId, ContactId, Cursor, MediaRef, MessageId};
pub use model::{
    Account, AccountChange, AccountSettings, Avatar, AvatarAnswer, Chat, ChatChange, ChatKind,
    ChatUnknown, ConnectionState, Contact, DeliveryStatus, Direction, ForwardItem, HistoryImport,
    LinkPlace, LinkStatus, LinkStep, Media, MediaData, MediaKind, MediaLimit, MediaUpload, Message,
    MessageContent, NewAccount, NumberCheck, OutgoingContent, OutgoingMessage, Page, PresenceState,
    ReplyRef, SendReceipt, ServerMedia, Timestamp, UploadProgress,
};
pub use provider::{LiveUpdates, PollingReason, Provider, IDEMPOTENCY_WINDOW};
pub use social::{
    refusal, BusinessHours, BusinessProfile, Group, GroupChange, GroupParticipant, GroupRole,
    JoinRequest, NewGroup, OwnProfile, ParticipantChange, ParticipantOutcome, ProfileChange,
    Subgroup,
};

pub use stickers::{FavoriteSticker, StickerFile};
pub use stories::{
    NewStory, NewStoryContent, Story, StoryAudience, StoryBody, StoryFont, StoryPrivacy,
    StoryReplyKind, StoryReplyRef, StoryStyle, StoryViewer, STORY_LIFETIME, STORY_TEXT_MAX,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_only_moves_forward() {
        let failed = DeliveryStatus::Failed { reason: "x".into() };
        assert!(DeliveryStatus::Pending.can_advance_to(&DeliveryStatus::Sent));
        assert!(DeliveryStatus::Sent.can_advance_to(&DeliveryStatus::Read));
        assert!(!DeliveryStatus::Read.can_advance_to(&DeliveryStatus::Delivered));
        assert!(!DeliveryStatus::Sent.can_advance_to(&DeliveryStatus::Sent));
        assert!(DeliveryStatus::Pending.can_advance_to(&failed));
        // Accepted by the provider sits between "on this device" and
        // "on WhatsApp", and can still fail there.
        assert!(DeliveryStatus::Pending.can_advance_to(&DeliveryStatus::Accepted));
        assert!(DeliveryStatus::Accepted.can_advance_to(&DeliveryStatus::Sent));
        assert!(!DeliveryStatus::Sent.can_advance_to(&DeliveryStatus::Accepted));
        assert!(DeliveryStatus::Accepted.can_advance_to(&failed));
        assert!(!DeliveryStatus::Delivered.can_advance_to(&failed));
        assert!(failed.can_advance_to(&DeliveryStatus::Sent));
    }

    #[test]
    fn only_retryable_errors_are_transient() {
        assert!(ProviderError::Transient("eof".into()).is_transient());
        assert!(ProviderError::RateLimited { retry_after: None }.is_transient());
        assert!(!ProviderError::Unauthorized("revoked".into()).is_transient());
        assert!(!ProviderError::Rejected {
            code: "not_on_whatsapp".into(),
            message: "no WhatsApp".into()
        }
        .is_transient());
    }

    /// A provider that implements only what the trait requires.
    struct Bare;

    #[async_trait::async_trait]
    impl Provider for Bare {
        fn id(&self) -> &'static str {
            "bare"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::default()
        }
        async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
            unreachable!()
        }
        async fn list_chats(&self, _: &AccountId, _: Option<Cursor>) -> ProviderResult<Page<Chat>> {
            unreachable!()
        }
        async fn fetch_messages(
            &self,
            _: &AccountId,
            _: &ChatId,
            _: Option<Cursor>,
            _: u32,
        ) -> ProviderResult<Page<Message>> {
            unreachable!()
        }
        async fn send(&self, _: OutgoingMessage) -> ProviderResult<SendReceipt> {
            unreachable!()
        }
        async fn mark_read(
            &self,
            _: &AccountId,
            _: &ChatId,
            _: Option<&MessageId>,
        ) -> ProviderResult<()> {
            unreachable!()
        }
        async fn update_chat(
            &self,
            _: &AccountId,
            _: &ChatId,
            _: ChatChange,
        ) -> ProviderResult<()> {
            unreachable!()
        }
        async fn download_media(&self, _: &AccountId, _: &MediaRef) -> ProviderResult<MediaData> {
            unreachable!()
        }
        async fn subscribe(&self) -> ProviderResult<EventStream> {
            unreachable!()
        }
    }

    #[test]
    fn live_updates_default_none() {
        // A provider with no choice of transport says nothing, so the
        // diagnostics leave the line out.
        assert_eq!(Bare.live_updates(), None);
    }
}
