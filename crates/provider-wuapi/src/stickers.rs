//! The stickers starred on the account: WhatsApp's favorite stickers.
//!
//! * `GET /v1/accounts/{accountId}/stickers/favorites`: the list wuapi
//!   stored, a page at a time. It asks WhatsApp nothing.
//! * `GET …/{stickerId}/media` (`redirect=false`): where one's file is.
//!   A favorite is a reference to a file on WhatsApp, fetched by the API
//!   the first time it is asked for. The client asks only for what it is
//!   about to keep or show (`fetch_media` on the reference this module
//!   gives), never for the whole list by itself.
//! * `POST …/favorites` with `{messageId}` (a sticker the account sent or
//!   received: no file travels) or `{uploadId}` (a WebP uploaded first).
//! * `DELETE …/favorites/{stickerId}`.
//!
//! Favorite stickers are turned on in the engine one number at a time.
//! For a number without them the list is empty and a change answers
//! `400 not_supported`: that is "not available yet for this number", not a
//! failure. It is remembered ([`Missing`](crate::availability::Missing)),
//! changes answer `Unsupported` without a request meanwhile, and it is
//! asked again later. A deployment without the routes (`404`) is the same,
//! for every number.

use crate::availability::{no_route, not_supported};
use crate::client::{WuapiClient, MAX_PAGE};
use crate::error::from_sdk;
use crate::mapping::FAVORITE_PREFIX;
use client_provider::{
    FavoriteSticker, Feature, MediaKind, MediaRef, MediaUpload, ProviderError, ProviderResult,
    StickerFile,
};
use std::sync::Arc;
use wuapi::types as api;

/// The API keeps at most this many favorites.
const MAX_FAVORITES: usize = 2_000;

const UNSUPPORTED: &str = "favorite stickers";

fn unsupported() -> ProviderError {
    ProviderError::Unsupported(UNSUPPORTED)
}

fn positive(value: Option<i64>) -> Option<u32> {
    value
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

/// `FavoriteSticker` -> [`FavoriteSticker`]. `None` for what the client
/// could never show: a vector sticker (Lottie, `application/was`).
///
/// A favorite whose file WhatsApp no longer has (`media.url` is `null`)
/// is listed all the same: it is still starred on the phone, and leaving
/// it out would read as "unstarred there". Its file answers
/// `410 media_expired` when it is asked for, which is asked once (see
/// [`WuapiClient::media_file`]).
pub(crate) fn favorite(wire: &api::FavoriteSticker) -> Option<FavoriteSticker> {
    if wire.lottie {
        return None;
    }
    // Always through the favorite's own route: its stored file goes when
    // the favorite does, so a URL kept from a listing would not last.
    let size = wire
        .size
        .filter(|size| *size >= 0)
        .map(|size| size.to_string())
        .unwrap_or_default();
    let (width, height) = match (positive(wire.width), positive(wire.height)) {
        (Some(width), Some(height)) => (Some(width), Some(height)),
        _ => (None, None),
    };
    Some(FavoriteSticker {
        id: wire.id.clone(),
        source: MediaRef::new(format!("{FAVORITE_PREFIX}{size}:{}", wire.id)),
        mime_type: Some(wire.mime_type.clone()).filter(|mime| !mime.trim().is_empty()),
        width,
        height,
        size_bytes: wire.size.and_then(|size| u64::try_from(size).ok()),
    })
}

impl WuapiClient {
    /// The account's favorites, every page.
    pub(crate) async fn favorite_stickers(
        &self,
        account: &str,
    ) -> ProviderResult<Vec<FavoriteSticker>> {
        // A number that has them turned off still lists (nothing); only a
        // deployment without the route is not asked.
        if self.missing.lacks_route(Feature::StickerFavorites) {
            return Err(unsupported());
        }
        let params = api::FavoriteStickersListParams {
            limit: Some(i64::from(MAX_PAGE)),
            ..Default::default()
        };
        match self
            .sdk()
            .favorite_stickers()
            .list(account, params)
            .to_vec_max(MAX_FAVORITES)
            .await
        {
            Ok(listed) => Ok(listed.iter().filter_map(favorite).collect()),
            Err(error) if no_route(&error) => {
                self.missing.no_route(Feature::StickerFavorites);
                Err(unsupported())
            }
            Err(error) => Err(from_sdk(error)),
        }
    }

    /// `POST …/favorites`. Answers the favorite, or what to do about the
    /// refusal: `Ok(None)` when this way of naming the sticker cannot be
    /// used and the other should be tried.
    async fn add_favorite(
        &self,
        account: &str,
        request: api::FavoriteStickerAddRequest,
        key: String,
    ) -> ProviderResult<Option<api::FavoriteSticker>> {
        match self
            .sdk()
            .favorite_stickers()
            .add(account, request)
            .idempotency_key(key)
            .await
        {
            Ok(favorite) => {
                self.missing.works(Feature::StickerFavorites, account);
                Ok(Some(favorite))
            }
            Err(error) if no_route(&error) => {
                self.missing.no_route(Feature::StickerFavorites);
                Err(unsupported())
            }
            Err(error) if not_supported(&error) => {
                self.missing.off_for(Feature::StickerFavorites, account);
                Err(unsupported())
            }
            // The message (or the upload) is not there, or is not a
            // sticker the API can star.
            Err(wuapi::Error::Api {
                status: 400 | 404 | 410,
                ..
            }) => Ok(None),
            Err(error) => Err(from_sdk(error)),
        }
    }

    /// Stars a sticker on the account: by the message it was seen in when
    /// there is one, else (or when that message cannot be used) by
    /// uploading its file. Answers the id it is listed under. Starring
    /// what is starred answers the same favorite and sends nothing to
    /// WhatsApp, so a repeat is harmless; the keys make each step the
    /// same request every time.
    pub(crate) async fn star_sticker(
        &self,
        account: &str,
        sticker: &StickerFile,
    ) -> ProviderResult<String> {
        if self.missing.is(Feature::StickerFavorites, account) {
            return Err(unsupported());
        }
        if let Some(message) = &sticker.message {
            let request = api::FavoriteStickerFromMessage::new(message.as_str());
            let key = format!("favorite:message:{account}:{}", sticker.id);
            if let Some(favorite) = self.add_favorite(account, request.into(), key).await? {
                return Ok(favorite.id);
            }
        }
        let upload = MediaUpload {
            key: format!("favorite:{account}:{}", sticker.id),
            kind: MediaKind::Sticker,
            bytes: sticker.bytes.clone(),
            mime_type: sticker.mime_type.clone(),
            file_name: None,
        };
        let quiet: client_provider::UploadProgress = Arc::new(|_| {});
        let upload_id = self.upload(&upload, &quiet).await?;
        let request = api::FavoriteStickerFromUpload::new(upload_id);
        let key = format!("favorite:upload:{account}:{}", sticker.id);
        match self.add_favorite(account, request.into(), key).await? {
            Some(favorite) => Ok(favorite.id),
            None => Err(ProviderError::Rejected {
                code: "invalid_sticker".into(),
                message: "WhatsApp does not take this file as a favorite sticker.".into(),
            }),
        }
    }

    /// `DELETE …/favorites/{stickerId}`. One that is gone already is done.
    pub(crate) async fn unstar_sticker(&self, account: &str, id: &str) -> ProviderResult<()> {
        if self.missing.is(Feature::StickerFavorites, account) {
            return Err(unsupported());
        }
        match self.sdk().favorite_stickers().remove(account, id).await {
            Ok(()) => {
                self.missing.works(Feature::StickerFavorites, account);
                Ok(())
            }
            Err(error) if no_route(&error) => {
                self.missing.no_route(Feature::StickerFavorites);
                Err(unsupported())
            }
            Err(error) if not_supported(&error) => {
                self.missing.off_for(Feature::StickerFavorites, account);
                Err(unsupported())
            }
            Err(wuapi::Error::Api { status: 404, .. }) => Ok(()),
            Err(error) => Err(from_sdk(error)),
        }
    }
}
