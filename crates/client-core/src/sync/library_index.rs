//! Stickers the account already has in its chats, listed in the library.
//!
//! The sticker picker is useful on an account that never saved a sticker:
//! what the account sent (from here or from its phone) and what it
//! received are read from the messages already in the store and listed as
//! library rows made by the indexer (`LibraryItem::indexed`), so that
//! starring, recent use and eviction work for them as for any item.
//!
//! * **From the cache, never from the network.** A sticker whose thumbnail
//!   is cached becomes an item at once: an animated one with the original
//!   file the cache keeps for it, a still one as a 512 by 512 WebP made of
//!   the cached thumbnail (the original of a still sticker is not kept;
//!   the thumbnail is the same pixels). One whose file is not cached is a
//!   place for it, with no bytes, and the picker asks for the file when
//!   the place scrolls into view ([`SyncEngine::want_sticker_file`], under
//!   the same media policy and queue as a bubble). When the file arrives
//!   the engine fills the place in.
//! * **Incremental and bounded.** The indexer remembers where it got to
//!   in the messages (`library_index`): what is above is new and is looked
//!   at as it arrives or is sent, what is below is looked at newest first
//!   in windows, a few stickers per run, and stops when the caps are met.
//!   A run is blocking work of bounded size; the loop runs it off the
//!   async workers and again when something it waits for changed.
//! * **Caps.** [`RECENT_LIMIT`] stickers sent and [`HEARD_LIMIT`] received
//!   are kept; older ones that were not starred or used are let go.

use super::SyncEngine;
use crate::library::{self, FileKind};
use crate::outbox::LOCAL_MEDIA;
use crate::store::{
    IndexState, LibraryKind, LibrarySource, NewLibraryItem, SeenSticker, StickerMessage,
    StoreChange, StoreError,
};
use crate::sync::{animation_key, thumbnail_key, MediaState, RECENT_LIMIT};
use client_provider::{AccountId, Timestamp};
use std::time::Duration;

/// How many stickers received from the chats are kept.
pub const HEARD_LIMIT: usize = 120;
/// How many stickers a run makes files of (the rest waits for the next).
const RUN_ITEMS: usize = 24;
/// How many messages (by row number) one window of old ones covers.
const WINDOW: i64 = 20_000;
/// How many windows of old messages one run reads.
const WINDOWS_PER_RUN: usize = 4;
/// How many stickers are read from a window at once.
const BATCH: usize = 200;
/// How long the loop waits after a change before it looks, so that a page
/// of history is one run.
const SETTLE: Duration = Duration::from_millis(400);
/// How long between two runs when one was not enough.
const BETWEEN_RUNS: Duration = Duration::from_millis(250);

impl SyncEngine {
    /// One bounded run of the indexer: new messages first, then old ones,
    /// then the places whose files have arrived. Blocking: call it off the
    /// UI thread and the async workers. Answers whether there is more to
    /// do (call it again, after a pause).
    pub fn index_stickers(&self) -> Result<bool, StoreError> {
        let store = &self.inner.store;
        let mut budget = RUN_ITEMS;
        let mut changed = false;
        let mut more = false;

        let state = match store.library_index_state()? {
            Some(state) => state,
            None => {
                let newest = store.newest_message_pk()?;
                let state = IndexState {
                    high: newest,
                    low: newest + 1,
                    done: false,
                };
                store.library_set_index_state(state)?;
                state
            }
        };
        let mut state = state;

        // What is new: from the last one looked at up to what is there now.
        let newest = store.newest_message_pk()?;
        while state.high < newest && budget > 0 {
            let found = store.sticker_messages(state.high, newest, false, BATCH)?;
            let (last, stopped) = self.see_stickers(&found, &mut budget, &mut changed)?;
            if found.len() < BATCH && !stopped {
                state.high = newest;
            } else if let Some(pk) = last {
                state.high = pk;
            } else {
                break;
            }
            store.library_set_index_state(state)?;
        }
        more |= state.high < newest;

        // What is old: newest first, in windows, until the caps are met.
        let mut windows = 0;
        while !state.done && budget > 0 && windows < WINDOWS_PER_RUN {
            windows += 1;
            let floor = (state.low - 1 - WINDOW).max(0);
            let found = store.sticker_messages(floor, state.low - 1, true, BATCH)?;
            let (last, stopped) = self.see_stickers(&found, &mut budget, &mut changed)?;
            if found.len() < BATCH && !stopped {
                state.low = floor + 1;
            } else if let Some(pk) = last {
                state.low = pk;
            } else {
                break;
            }
            if state.low <= 1 {
                state.done = true;
            }
            let (sent, heard) = store.library_indexed_counts()?;
            if sent >= RECENT_LIMIT && heard >= HEARD_LIMIT {
                state.done = true;
            }
            store.library_set_index_state(state)?;
        }
        more |= !state.done;

        changed |= self.complete_stickers(&mut budget)?;
        if store.library_prune_indexed(RECENT_LIMIT, HEARD_LIMIT)? > 0 {
            changed = true;
        }
        if changed {
            store.notify(StoreChange::Library);
        }
        Ok(more || budget == 0)
    }

    /// Lists the stickers of messages in order while the budget lasts.
    /// Answers the last message it listed and whether it stopped before
    /// the end for want of budget.
    fn see_stickers(
        &self,
        found: &[StickerMessage],
        budget: &mut usize,
        changed: &mut bool,
    ) -> Result<(Option<i64>, bool), StoreError> {
        let mut last = None;
        for message in found {
            if *budget == 0 {
                return Ok((last, true));
            }
            *changed |= self.see_sticker(message, budget)?;
            last = Some(message.pk);
        }
        Ok((last, false))
    }

    /// Lists the sticker of one message. Answers whether the library
    /// changed.
    fn see_sticker(
        &self,
        message: &StickerMessage,
        budget: &mut usize,
    ) -> Result<bool, StoreError> {
        let store = &self.inner.store;
        // What was sent from here is the library item that was sent, and
        // already there; a vector sticker cannot be shown or sent.
        if message.url.starts_with(LOCAL_MEDIA)
            || message
                .mime
                .as_deref()
                .is_some_and(|mime| !mime.trim().to_ascii_lowercase().starts_with("image/"))
        {
            return Ok(false);
        }
        let seen = SeenSticker {
            account: message.account.clone(),
            message: message.id.clone(),
            url: message.url.clone(),
            outgoing: message.outgoing,
            at: message.at,
        };
        if let Some(id) = store.library_by_origin(&seen.account, &seen.url)? {
            store.library_see(&id, seen.outgoing, seen.at)?;
            // A row from before the message was kept learns it now.
            store.library_set_origin(&id, &seen.account, &seen.url, Some(&seen.message))?;
            return Ok(true);
        }
        *budget = budget.saturating_sub(1);
        let source = if seen.outgoing {
            LibrarySource::Sent
        } else {
            LibrarySource::Received
        };
        let file = self
            .sticker_file(&seen.account, &seen.url, source)?
            .map(|new| {
                let id = library::content_id(&new.bytes);
                (new, id)
            });
        store.library_add_seen(&seen, file, Timestamp::now())?;
        Ok(true)
    }

    /// The file of the sticker at `url` as the cache has it, or `None`
    /// while the cache lacks it.
    fn sticker_file(
        &self,
        account: &AccountId,
        url: &str,
        source: LibrarySource,
    ) -> Result<Option<NewLibraryItem>, StoreError> {
        let _ = account;
        let store = &self.inner.store;
        let now = Timestamp::now();
        let Some(thumb) = store.media(&thumbnail_key(url), now)? else {
            return Ok(None);
        };
        // Whether it moves is answered next to the thumbnail; an answer
        // that is not there yet is asked for when the place is looked at.
        let Some(moving) = store.media(&animation_key(url), now)? else {
            return Ok(None);
        };
        let (bytes, mime, animated) = if moving.bytes.is_empty() {
            match library::sticker_from_image(&thumb.bytes) {
                Ok(sticker) => (sticker.bytes, sticker.mime.to_owned(), false),
                Err(error) => {
                    tracing::debug!(%error, "a sticker of a chat could not be made a file");
                    return Ok(None);
                }
            }
        } else {
            (
                moving.bytes,
                moving.mime.unwrap_or_else(|| "image/webp".to_owned()),
                true,
            )
        };
        if !matches!(
            library::sniff(&bytes),
            FileKind::Webp { .. } | FileKind::Gif { .. } | FileKind::Png
        ) {
            return Ok(None);
        }
        Ok(Some(NewLibraryItem {
            kind: LibraryKind::Sticker,
            size: library::image_size(&bytes),
            thumb: library::library_thumbnail(&bytes),
            bytes,
            mime,
            animated,
            source,
            name: None,
            pack: None,
        }))
    }

    /// Fills in the places whose files have arrived, and drops those
    /// WhatsApp no longer has. Answers whether the library changed.
    fn complete_stickers(&self, budget: &mut usize) -> Result<bool, StoreError> {
        let store = &self.inner.store;
        let mut changed = false;
        for (id, account, url) in store.library_pending(RUN_ITEMS * 4)? {
            if matches!(
                self.media_state(&thumbnail_key(&url)),
                MediaState::Expired(_)
            ) {
                store.library_remove(&id)?;
                changed = true;
                continue;
            }
            if *budget == 0 {
                return Ok(changed);
            }
            let source = store
                .library_item(&id)?
                .map_or(LibrarySource::Received, |item| item.source);
            let Some(new) = self.sticker_file(&account, &url, source)? else {
                continue;
            };
            *budget -= 1;
            let new_id = library::content_id(&new.bytes);
            store.library_complete(&id, new, &new_id)?;
            changed = true;
        }
        Ok(changed)
    }

    /// Asks for the file of a sticker the library lists and does not have
    /// yet: its thumbnail, or, when that is here, the answer to whether it
    /// moves. The same download a bubble would make, in the same queue.
    /// `asked`: the user clicked it, so the policy that keeps files from
    /// coming unasked does not apply.
    pub fn want_sticker_file(&self, account: &AccountId, url: &str, asked: bool) {
        let have_thumbnail = matches!(
            self.inner.store.media_size(&thumbnail_key(url)),
            Ok(Some(_))
        );
        if !have_thumbnail {
            if asked {
                self.retry_media(&thumbnail_key(url));
                self.want_thumbnail_asked(account, url);
            } else {
                self.want_thumbnail(account, url);
            }
        } else {
            self.retry_media(&animation_key(url));
            self.want_animation(account, url);
        }
    }

    /// The loop that keeps the library's stickers up to date: it runs the
    /// indexer, and again when messages arrive or a thumbnail or an answer
    /// about movement is cached; it never polls.
    pub(crate) async fn run_library_index(self) {
        let mut changes = self.inner.store.subscribe();
        loop {
            if self.is_stopped() {
                return;
            }
            let this = self.clone();
            let more = tokio::task::spawn_blocking(move || this.index_stickers()).await;
            let more = match more {
                Ok(Ok(more)) => more,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "could not index the stickers of the chats");
                    false
                }
                Err(_) => return,
            };
            if more {
                tokio::time::sleep(BETWEEN_RUNS).await;
                while changes.try_next().is_some() {}
                continue;
            }
            loop {
                match changes.next().await {
                    None => return,
                    Some(StoreChange::Messages { .. } | StoreChange::Everything) => break,
                    Some(StoreChange::Media { key })
                        if key.starts_with("thumb:") || key.starts_with("anim:") =>
                    {
                        break
                    }
                    Some(_) => {}
                }
            }
            tokio::time::sleep(SETTLE).await;
            while changes.try_next().is_some() {}
        }
    }
}
