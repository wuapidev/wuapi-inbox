//! The sticker and GIF library: the engine's side.
//!
//! Three kinds of work:
//!
//! * **Keeping**: a file enters the library (imported, saved from a
//!   message, found online, starred on the phone) under the hash of its
//!   bytes, with a small still for the picker. The library has a budget;
//!   past it what was used longest ago goes, favorites never.
//! * **Sending**: a library item is sent like any file, through the
//!   outbox: its bytes are copied into the store with the pending bubble,
//!   uploaded under a key that is the same for every attempt, and sent.
//!   A sticker goes as a sticker; an MP4 as a video flagged to play as a
//!   GIF; a `.gif` file as the image it is (see the module notes of
//!   `docs/ARCHITECTURE.md`: the API converts nothing).
//! * **Favorites on the phone**, when the provider can
//!   (`Capabilities::sticker_favorites`): one reconciliation per account
//!   that pulls what was starred there, pushes what was starred here and
//!   carries removals both ways. It runs from a state it remembers (what
//!   the account was known to have starred when it last ran), so a
//!   removal is told from something that was never there. It is safe to
//!   repeat: every step sets a value, and a step that fails in passing is
//!   done at the next run.

use super::{SendMediaError, SyncEngine};
use crate::library::{self, ImportError};
use crate::store::{
    LibraryItem, LibraryKind, LibrarySource, NewLibraryItem, StoreChange, StoreError,
};
use crate::NewMedia;
use client_provider::{
    AccountId, ChatId, ClientMessageId, Feature, MediaKind, MediaLimit, MessageId, ProviderError,
    StickerFile, Timestamp,
};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::Duration;
use tokio::time::Instant;

/// How many bytes the library may hold: 64 MB. Favorites are never let go
/// for it.
pub const LIBRARY_BUDGET: u64 = 64 * 1024 * 1024;
/// How many items the recent section lists.
pub const RECENT_LIMIT: usize = 36;
/// How long a reconciliation of an account's favorites is taken to hold.
const FAVORITES_FRESH: Duration = Duration::from_secs(5 * 60);
/// How soon one that failed in passing runs again.
const FAVORITES_RETRY: Duration = Duration::from_secs(30);
/// The largest sticker taken from the phone: stickers are small.
const FAVORITE_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Why something could not be put in the library.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// The file is not what it has to be.
    #[error(transparent)]
    Import(#[from] ImportError),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A library item checked and ready to be queued
/// ([`SyncEngine::prepare_library_item`]).
#[derive(Clone, Debug)]
pub struct LibrarySend {
    id: String,
    prepared: super::PreparedMedia,
    gif: bool,
}

/// What the engine keeps for the library.
#[derive(Default)]
pub(super) struct LibrarySync {
    /// The accounts whose favorites are being reconciled, and whether
    /// another run was asked for meanwhile.
    running: Mutex<HashMap<AccountId, bool>>,
    /// When each account's favorites are next due.
    due: Mutex<HashMap<AccountId, Instant>>,
}

impl SyncEngine {
    // ----- keeping ------------------------------------------------------

    /// Puts a file in the library. Blocking only for the still it makes:
    /// call it off the UI thread for a large picture.
    pub fn library_add(
        &self,
        kind: LibraryKind,
        bytes: Vec<u8>,
        mime: &str,
        source: LibrarySource,
        name: Option<String>,
        pack: Option<String>,
    ) -> Result<(LibraryItem, bool), LibraryError> {
        let animated = match library::sniff(&bytes) {
            library::FileKind::Webp { animated } | library::FileKind::Gif { animated } => animated,
            library::FileKind::Mp4 => true,
            _ => false,
        };
        let id = library::content_id(&bytes);
        let size = library::image_size(&bytes);
        let thumb = library::library_thumbnail(&bytes);
        let new = NewLibraryItem {
            kind,
            bytes,
            mime: mime.to_owned(),
            animated,
            size,
            source,
            name: name.filter(|name| !name.trim().is_empty()),
            pack,
            thumb,
        };
        Ok(self
            .inner
            .store
            .library_add(new, &id, Timestamp::now(), LIBRARY_BUDGET)?)
    }

    /// Makes a sticker of a picture (see [`crate::sticker_from_image`])
    /// and keeps it. Answers the item and what was done to the file, in
    /// words for the user. Blocking and CPU-bound.
    pub fn library_import_sticker(
        &self,
        bytes: &[u8],
        name: Option<String>,
        pack: Option<String>,
    ) -> Result<(LibraryItem, Vec<&'static str>), LibraryError> {
        let sticker = library::sticker_from_image(bytes)?;
        let notes = sticker.notes.clone();
        let (item, _) = self.library_add(
            LibraryKind::Sticker,
            sticker.bytes,
            sticker.mime,
            LibrarySource::Imported,
            name,
            pack,
        )?;
        Ok((item, notes))
    }

    /// Keeps a GIF: a `.gif` file or a short MP4.
    pub fn library_import_gif(
        &self,
        bytes: Vec<u8>,
        name: Option<String>,
        source: LibrarySource,
    ) -> Result<LibraryItem, LibraryError> {
        let file = library::gif_file(&bytes)?;
        let (item, _) = self.library_add(LibraryKind::Gif, bytes, file.mime, source, name, None)?;
        Ok(item)
    }

    /// Keeps a sticker as it is (the original file of a message, say),
    /// optionally starring it at once. Answers whether it is new.
    pub fn library_save_sticker(
        &self,
        bytes: Vec<u8>,
        mime: &str,
        source: LibrarySource,
        favorite: bool,
    ) -> Result<(LibraryItem, bool), LibraryError> {
        match library::sniff(&bytes) {
            library::FileKind::Webp { .. }
            | library::FileKind::Png
            | library::FileKind::Gif { .. } => {}
            _ => return Err(ImportError::NotAnImage.into()),
        }
        let (mut item, created) =
            self.library_add(LibraryKind::Sticker, bytes, mime, source, None, None)?;
        if favorite {
            self.set_library_favorite(&item.id, true)?;
            item.favorite = true;
        }
        Ok((item, created))
    }

    /// Keeps a GIF of a message, as it is.
    pub fn library_save_gif(
        &self,
        bytes: Vec<u8>,
        source: LibrarySource,
    ) -> Result<(LibraryItem, bool), LibraryError> {
        let file = library::gif_file(&bytes)?;
        self.library_add(LibraryKind::Gif, bytes, file.mime, source, None, None)
    }

    /// Notes that an item is the file of a message of `account` (the
    /// media reference the message carries it under), so that it can be
    /// starred on the phone by naming that message instead of sending its
    /// file. What the item already says of where it came from stays.
    pub fn library_seen_in(
        &self,
        id: &str,
        account: &AccountId,
        media: &client_provider::MediaRef,
    ) -> Result<(), StoreError> {
        let store = &self.inner.store;
        let message = store.message_with_media(account, media.as_str())?;
        store.library_set_origin(id, account, media.as_str(), message.as_ref())
    }

    /// Stars or unstars an item, here at once and, where the provider can,
    /// on the account's phone in the background.
    pub fn set_library_favorite(&self, id: &str, favorite: bool) -> Result<(), StoreError> {
        let changed = self.inner.store.library_set_favorite(id, favorite)?;
        if changed {
            self.sync_all_favorites();
        }
        Ok(())
    }

    /// Takes an item out of the library, favorite or not. Where the
    /// provider can, the account's phone is told that its star is gone, in
    /// the background: what the user took out does not come back from
    /// there. Answers whether there was such an item.
    pub fn remove_library_item(&self, id: &str) -> Result<bool, StoreError> {
        let removed = self.inner.store.library_remove(id)?;
        if removed {
            self.sync_all_favorites();
        }
        Ok(removed)
    }

    /// Moves a favorite to a place among the favorites.
    pub fn move_library_favorite(&self, id: &str, to: usize) -> Result<bool, StoreError> {
        self.inner.store.library_move_favorite(id, to)
    }

    // ----- sending ------------------------------------------------------

    /// Checks a library item against what the provider takes and makes
    /// what the bubble shows. Blocking: call it off the UI thread.
    pub fn prepare_library_item(
        &self,
        id: &str,
        reply_to: Option<MessageId>,
    ) -> Result<LibrarySend, SendMediaError> {
        let store = &self.inner.store;
        let found = store
            .library_item(id)
            .ok()
            .flatten()
            .zip(store.library_file(id).ok().flatten());
        let Some((item, (bytes, mime))) = found else {
            return Err(SendMediaError::Empty);
        };
        // A sticker seen in a chat whose file has not arrived is only a
        // place for it.
        if item.pending() {
            return Err(SendMediaError::Empty);
        }
        // A sticker is a sticker. An MP4 is a video that plays as a GIF;
        // a `.gif` file is an image: the API converts nothing.
        let (kind, gif) = match (item.kind, mime.as_str()) {
            (LibraryKind::Sticker, _) => (MediaKind::Sticker, false),
            (LibraryKind::Gif, "image/gif") => (MediaKind::Image, false),
            (LibraryKind::Gif, _) => (MediaKind::Video, true),
        };
        let prepared = self.prepare_media(NewMedia {
            kind,
            bytes,
            mime_type: mime,
            file_name: None,
            caption: None,
            reply_to,
            mentions: Vec::new(),
        })?;
        Ok(LibrarySend {
            id: item.id,
            prepared,
            gif,
        })
    }

    /// Queues a prepared item for a chat, answering the message that
    /// shows at once. The item counts as used: it is the first of the
    /// recent ones from now on.
    pub fn send_prepared_library(
        &self,
        account: &AccountId,
        chat: &ChatId,
        send: LibrarySend,
    ) -> Result<ClientMessageId, SendMediaError> {
        let LibrarySend { id, prepared, gif } = send;
        let client_id = self.queue_prepared(account, chat, prepared, gif)?;
        if let Err(error) = self.inner.store.library_touch(&id, Timestamp::now()) {
            tracing::warn!(%error, "could not note a sticker as used");
        }
        Ok(client_id)
    }

    /// [`prepare_library_item`](Self::prepare_library_item) and
    /// [`send_prepared_library`](Self::send_prepared_library) in one.
    pub fn send_library_item(
        &self,
        account: &AccountId,
        chat: &ChatId,
        id: &str,
        reply_to: Option<MessageId>,
    ) -> Result<ClientMessageId, SendMediaError> {
        let prepared = self.prepare_library_item(id, reply_to)?;
        self.send_prepared_library(account, chat, prepared)
    }

    // ----- favorites on the phone -----------------------------------------

    /// Says the favorites are about to be shown (or the account is back):
    /// they are reconciled with the account's phone in the background,
    /// unless that was done a few minutes ago.
    pub fn want_favorite_stickers(&self, account: &AccountId) {
        if !self.inner.capabilities.sticker_favorites || self.is_stopped() {
            return;
        }
        {
            let mut due = self.inner.library.due.lock().expect("library lock");
            let now = Instant::now();
            if due.get(account).is_some_and(|at| now < *at) {
                return;
            }
            due.insert(account.clone(), now + FAVORITES_FRESH);
        }
        self.run_favorites(account);
    }

    /// Reconciles every account's favorites now: something changed here.
    fn sync_all_favorites(&self) {
        if !self.inner.capabilities.sticker_favorites || self.is_stopped() {
            return;
        }
        let Ok(accounts) = self.inner.store.accounts() else {
            return;
        };
        for account in accounts {
            self.run_favorites(&account.id);
        }
    }

    /// Starts a reconciliation of one account, or asks the one under way
    /// to go round again.
    fn run_favorites(&self, account: &AccountId) {
        {
            let mut running = self.inner.library.running.lock().expect("library lock");
            if let Some(again) = running.get_mut(account) {
                *again = true;
                return;
            }
            running.insert(account.clone(), false);
        }
        let this = self.clone();
        let account = account.clone();
        self.inner.runtime.spawn(async move {
            loop {
                let missing = this.feature_unavailable(&account, Feature::StickerFavorites);
                let result = this.reconcile_favorites(&account).await;
                // The picker says whether the phone's favorites are to be
                // had: it is told when that changed.
                if this.feature_unavailable(&account, Feature::StickerFavorites) != missing {
                    this.inner.store.notify(StoreChange::Library);
                }
                let retry = match &result {
                    Err(error) if error.is_transient() => true,
                    Err(ProviderError::Unsupported(_)) => false,
                    Err(error) => {
                        tracing::warn!(%error, "favorite stickers failed");
                        false
                    }
                    Ok(_) => false,
                };
                if retry {
                    this.inner
                        .library
                        .due
                        .lock()
                        .expect("library lock")
                        .insert(account.clone(), Instant::now() + FAVORITES_RETRY);
                }
                let again = {
                    let mut running = this.inner.library.running.lock().expect("library lock");
                    let again = running.get(&account).copied().unwrap_or(false);
                    if again {
                        running.insert(account.clone(), false);
                    } else {
                        running.remove(&account);
                    }
                    again
                };
                if !again || this.is_stopped() {
                    break;
                }
            }
        });
    }

    /// One reconciliation: pull, carry removals, push. Answers how many
    /// stickers it changed here.
    pub async fn reconcile_favorites(&self, account: &AccountId) -> Result<usize, ProviderError> {
        let inner = &self.inner;
        let store = &inner.store;
        let storing = |error: StoreError| ProviderError::Protocol(error.to_string());
        let remote = self
            .bounded(inner.provider.list_favorite_stickers(account))
            .await?;
        let known: HashMap<String, String> = store
            .library_remote(account)
            .map_err(storing)?
            .into_iter()
            .collect();
        let known_remote: HashSet<&str> = known.values().map(String::as_str).collect();
        let listed: HashSet<&str> = remote.iter().map(|sticker| sticker.id.as_str()).collect();
        let mut changed = 0;
        let mut failure = None;

        // Starred on the phone and not seen before: kept here, starred.
        for sticker in remote
            .iter()
            .filter(|sticker| !known_remote.contains(sticker.id.as_str()))
        {
            let limit = MediaLimit {
                max_bytes: FAVORITE_MAX_BYTES,
                images_only: true,
            };
            let data = match self
                .bounded(inner.provider.fetch_media(account, &sticker.source, limit))
                .await
            {
                Ok(data) => data,
                // Asked to slow down: the rest waits for the next run,
                // instead of being refused one by one.
                Err(error @ ProviderError::RateLimited { .. }) => {
                    failure = Some(error);
                    break;
                }
                Err(error) if error.is_transient() => {
                    failure = Some(error);
                    continue;
                }
                Err(error) => {
                    tracing::debug!(%error, "a favorite sticker could not be fetched");
                    continue;
                }
            };
            let mime = data
                .mime_type
                .clone()
                .unwrap_or_else(|| "image/webp".to_owned());
            let (item, _) = match self.library_add(
                LibraryKind::Sticker,
                data.bytes,
                &mime,
                LibrarySource::Phone,
                None,
                None,
            ) {
                Ok(added) => added,
                Err(error) => {
                    tracing::debug!(%error, "a favorite sticker is not a picture");
                    continue;
                }
            };
            store
                .library_set_favorite(&item.id, true)
                .map_err(storing)?;
            store
                .library_set_remote(account, &item.id, &sticker.id)
                .map_err(storing)?;
            changed += 1;
        }

        // What the account had starred and no longer lists, or what was
        // unstarred here: carried over.
        for (item_id, remote_id) in &known {
            let here = store.library_item(item_id).map_err(storing)?;
            if !listed.contains(remote_id.as_str()) {
                // Unstarred on the phone.
                if here.is_some_and(|item| item.favorite) {
                    store
                        .library_set_favorite(item_id, false)
                        .map_err(storing)?;
                    changed += 1;
                }
                store
                    .library_forget_remote(account, item_id)
                    .map_err(storing)?;
            } else if here.as_ref().is_none_or(|item| !item.favorite) {
                // Unstarred here, or taken out of the library (by the user,
                // or let go for room after it was unstarred): the phone is
                // told, and what it had starred is forgotten only once it
                // answered.
                match self
                    .bounded(inner.provider.remove_favorite_sticker(account, remote_id))
                    .await
                {
                    Ok(()) => store
                        .library_forget_remote(account, item_id)
                        .map_err(storing)?,
                    Err(error) => failure = Some(error),
                }
            }
        }

        // Starred here and unknown to the account: the phone is told.
        let mapped: HashSet<String> = store
            .library_remote(account)
            .map_err(storing)?
            .into_iter()
            .map(|(item, _)| item)
            .collect();
        for item in store
            .library_favorites(LibraryKind::Sticker)
            .map_err(storing)?
            .into_iter()
            .filter(|item| !mapped.contains(&item.id))
        {
            let Some((bytes, mime)) = store.library_file(&item.id).map_err(storing)? else {
                continue;
            };
            let file = StickerFile {
                id: item.id.clone(),
                bytes: std::sync::Arc::new(bytes),
                mime_type: mime,
                // The message it was seen in, when that was on this
                // account: the provider may star it by naming it.
                message: item
                    .origin
                    .as_ref()
                    .filter(|(owner, _)| owner == account)
                    .and(item.origin_message.clone()),
            };
            match self
                .bounded(inner.provider.add_favorite_sticker(account, file))
                .await
            {
                // Under the id the provider lists it by from now on.
                Ok(remote) => store
                    .library_set_remote(account, &item.id, &remote)
                    .map_err(storing)?,
                // Not to be had for this number for now: the others would
                // be answered the same.
                Err(error @ ProviderError::Unsupported(_)) => {
                    failure = Some(error);
                    break;
                }
                Err(error) => failure = Some(error),
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(changed),
        }
    }
}
