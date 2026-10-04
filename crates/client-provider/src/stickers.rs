//! Favorite stickers: the ones starred in WhatsApp, which a provider may
//! be able to list, add to and remove from (see
//! [`Capabilities::sticker_favorites`](crate::Capabilities)).

use crate::ids::MediaRef;

/// A sticker starred on the account. The file is fetched like any other
/// media, with [`Provider::fetch_media`](crate::Provider::fetch_media) on
/// [`source`](Self::source).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FavoriteSticker {
    /// A stable id for the sticker on this account: what a removal names.
    /// Providers use the file's hash when WhatsApp gives one.
    pub id: String,
    /// Handle to download the sticker's file.
    pub source: MediaRef,
    /// MIME type, when known (`image/webp` for a sticker).
    pub mime_type: Option<String>,
    /// Pixel width, when known.
    pub width: Option<u32>,
    /// Pixel height, when known.
    pub height: Option<u32>,
    /// File size in bytes, when known.
    pub size_bytes: Option<u64>,
}

/// A sticker's file, handed to a provider to be starred on the account.
#[derive(Clone, PartialEq, Eq)]
pub struct StickerFile {
    /// The client's id for the sticker (the hash of its bytes). The same
    /// file is always the same id, so starring it twice is one star.
    pub id: String,
    /// The bytes.
    pub bytes: std::sync::Arc<Vec<u8>>,
    /// Its MIME type.
    pub mime_type: String,
    /// A message of this account the sticker was seen in, when it was: a
    /// provider that can star a sticker by naming its message does, and
    /// no file travels. Without one, or when that message cannot be
    /// used, the bytes are what is starred.
    pub message: Option<crate::MessageId>,
}

impl std::fmt::Debug for StickerFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the bytes.
        f.debug_struct("StickerFile")
            .field("id", &self.id)
            .field("bytes", &self.bytes.len())
            .field("mime_type", &self.mime_type)
            .field("message", &self.message)
            .finish()
    }
}
