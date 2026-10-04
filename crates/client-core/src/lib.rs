//! The client's core: a local store, an outbox and a sync engine.
//!
//! ```text
//!   Provider  ──►  SyncEngine  ──►  Store (SQLite)  ──►  UI
//!      ▲               │                 │
//!      └── outbox ◄────┘                 └── StoreChange notifications
//! ```
//!
//! The UI reads only from the [`Store`] and never waits on the network.
//! The [`SyncEngine`] keeps the store up to date from any
//! [`Provider`](client_provider::Provider) in the background, and sends
//! what the user writes through a persistent, idempotent outbox.

#![warn(missing_docs)]

mod animation;
mod imaging;
mod library;
mod mentions;
mod migrations;
mod outbox;
pub mod phone;
mod picture;
mod store;
mod summary;
mod sync;

#[cfg(feature = "fixtures")]
pub use animation::fixtures;
pub use animation::{
    animated_format, animation_frames, AnimatedFormat, AnimationFrame, AnimationFrames,
    AnimationLimits,
};
pub use client_provider as provider;
pub use imaging::{thumbnail, turned_size, upright, Thumbnail, Upright};
pub use library::{
    content_id, gif_file, image_size, library_thumbnail, sniff, sticker_from_image, FileKind,
    GifFile, ImportError, Sticker, GIF_MAX, LIBRARY_THUMB_SIDE, STICKER_MOVING_MAX, STICKER_SIDE,
    STICKER_STILL_MAX,
};
pub use mentions::{mention_name, with_mention_names, UNKNOWN_PERSON, YOU};
pub use migrations::SCHEMA_VERSION;
pub use outbox::{
    local_media_ref, local_message_id, run_outbox_pass, run_outbox_pass_with, upload_entry,
    OutboxConfig, OutboxEntry, OutboxFile, OutboxPass, UploadHooks, UploadOutcome, LOCAL_MEDIA,
};
pub use picture::{profile_picture, PICTURE_MAX_FILE, PICTURE_SIDE};
pub use store::{
    file_state, local_story_id, message_preview, preview_text, CachedMedia, ChangeListener,
    ChatSummary, FileState, HistoryState, IndexState, LibraryItem, LibraryKind, LibraryPack,
    LibrarySource, LibraryStats, MessagePreview, NameSource, NewLibraryItem, Person,
    ReactionSummary, Reactor, SearchHit, SeenSticker, SendTiming, StickerMessage, Store,
    StoreChange, StoreError, StoreKey, StoreResult, StoredAvatar, StoredGroup, StoredMessage,
    StoredParticipant, StoryAuthor, StoryFeed, StoryItem, StoryPostEntry, StoryPostState,
    StoryReceipt, StoryRing, Upsert, STORIES_KEPT,
};
pub use summary::system_line;
pub use sync::{
    animation_key, failure_sentence, file_key, new_client_id, outcome_sentence,
    own_picture_subject, thumbnail_key, CreatedGroup, ForwardError, ForwardRefusal, HistoryMode,
    LibraryError, LibrarySend, MediaState, NewChat, NewMedia, SendMediaError, SyncConfig,
    SyncEngine, SyncError, ANIMATION_KEEP_LIMIT, AUTO_MEDIA_LIMIT, AVATAR_TTL, HEARD_LIMIT,
    LIBRARY_BUDGET, MANUAL_MEDIA_LIMIT, RECENT_LIMIT,
};
pub use sync::{
    PreparedMedia, ReceiptPass, StoryListing, StoryPass, StoryPostError, StoryReplyError,
    STORIES_FRESH, STORIES_OPENED,
};

#[cfg(test)]
mod tests;
