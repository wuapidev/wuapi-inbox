//! Favorite stickers in the mock: a list per account that the client can
//! read, add to and remove from, and that a test can change "on the phone".

use crate::{MockProvider, State};
use client_provider::{
    AccountId, FavoriteSticker, MediaRef, ProviderError, ProviderResult, StickerFile,
};
use std::collections::{HashMap, VecDeque};

/// What the mock keeps for favorite stickers.
#[derive(Default)]
pub(crate) struct Stickers {
    /// The favorites of each account, in the order they were starred.
    favorites: HashMap<AccountId, Vec<FavoriteSticker>>,
    /// Errors the next favorite-sticker calls answer with, one per call.
    failures: VecDeque<ProviderError>,
    /// How many calls succeed before those failures.
    failures_after: usize,
    /// The provider says it cannot do favorites at all.
    off: bool,
    /// Every call made: `list`, `add <id>`, `remove <id>`.
    calls: Vec<String>,
    /// The message each added sticker named, in order (`None`: by file).
    named: Vec<Option<client_provider::MessageId>>,
    /// Favorites are listed under ids of the backend's own (`fav-<id>`)
    /// instead of the client's.
    own_ids: bool,
}

fn enter(state: &mut State, call: String) -> ProviderResult<()> {
    state.stickers.calls.push(call);
    if state.stickers.failures_after > 0 {
        state.stickers.failures_after -= 1;
        return Ok(());
    }
    match state.stickers.failures.pop_front() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

impl MockProvider {
    /// Stars a sticker on an account as if it had been done on the phone:
    /// its bytes are served under the sticker's source.
    pub fn star_sticker_on_phone(&self, account: &AccountId, id: &str, bytes: Vec<u8>, mime: &str) {
        let mut state = self.state();
        let source = format!("mock://sticker/{id}");
        state
            .media
            .insert(source.clone(), (bytes.clone(), mime.to_owned()));
        let sticker = FavoriteSticker {
            id: id.to_owned(),
            source: MediaRef::new(source),
            mime_type: Some(mime.to_owned()),
            width: None,
            height: None,
            size_bytes: Some(bytes.len() as u64),
        };
        let list = state.stickers.favorites.entry(account.clone()).or_default();
        list.retain(|known| known.id != id);
        list.push(sticker);
    }

    /// Takes a star off as if it had been done on the phone.
    pub fn unstar_sticker_on_phone(&self, account: &AccountId, id: &str) {
        if let Some(list) = self.state().stickers.favorites.get_mut(account) {
            list.retain(|known| known.id != id);
        }
    }

    /// Makes the provider say it has no favorite stickers, as a backend
    /// without the routes does. Call it before an engine is made over the
    /// provider: capabilities are read once.
    pub fn set_sticker_favorites_available(&self, available: bool) {
        self.state().stickers.off = !available;
    }

    /// Makes the provider list what the client stars under ids of its
    /// own (`fav-<client id>`), as a backend that names its favorites
    /// does.
    pub fn use_own_sticker_ids(&self) {
        self.state().stickers.own_ids = true;
    }

    /// The message each `add` named, in order: `None` for one that was
    /// starred by its file.
    pub fn sticker_messages_named(&self) -> Vec<Option<client_provider::MessageId>> {
        self.state().stickers.named.clone()
    }

    /// The ids starred on an account, in order.
    pub fn favorite_sticker_ids(&self, account: &AccountId) -> Vec<String> {
        self.state()
            .stickers
            .favorites
            .get(account)
            .map(|list| list.iter().map(|sticker| sticker.id.clone()).collect())
            .unwrap_or_default()
    }

    /// Makes the next favorite-sticker calls fail with the given errors,
    /// one per call.
    pub fn fail_next_sticker_calls(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().stickers.failures.extend(errors);
    }

    /// Like [`fail_next_sticker_calls`](Self::fail_next_sticker_calls),
    /// after `succeed` calls that go through: a run that lists fine and
    /// then drops on its first change.
    pub fn fail_sticker_calls_after(
        &self,
        succeed: usize,
        errors: impl IntoIterator<Item = ProviderError>,
    ) {
        let mut state = self.state();
        state.stickers.failures_after = succeed;
        state.stickers.failures.extend(errors);
    }

    /// The favorite-sticker calls made, in order: `list`, `add <id>`,
    /// `remove <id>`.
    pub fn sticker_calls(&self) -> Vec<String> {
        self.state().stickers.calls.clone()
    }

    pub(crate) fn favorites_available(&self) -> bool {
        !self.state().stickers.off
    }

    pub(crate) fn favorites_of(&self, account: &AccountId) -> ProviderResult<Vec<FavoriteSticker>> {
        let mut state = self.state();
        enter(&mut state, "list".into())?;
        Ok(state
            .stickers
            .favorites
            .get(account)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) fn favorite_added(
        &self,
        account: &AccountId,
        sticker: StickerFile,
    ) -> ProviderResult<String> {
        if self.is_unavailable(account, client_provider::Feature::StickerFavorites) {
            return Err(ProviderError::Unsupported("favorite stickers"));
        }
        let mut state = self.state();
        enter(&mut state, format!("add {}", sticker.id))?;
        state.stickers.named.push(sticker.message.clone());
        let id = if state.stickers.own_ids {
            format!("fav-{}", sticker.id)
        } else {
            sticker.id.clone()
        };
        let source = format!("mock://sticker/{id}");
        state.media.insert(
            source.clone(),
            (sticker.bytes.as_ref().clone(), sticker.mime_type.clone()),
        );
        let list = state.stickers.favorites.entry(account.clone()).or_default();
        // A star is a value: starring twice is one star.
        if !list.iter().any(|known| known.id == id) {
            list.push(FavoriteSticker {
                id: id.clone(),
                source: MediaRef::new(source),
                mime_type: Some(sticker.mime_type),
                width: None,
                height: None,
                size_bytes: Some(sticker.bytes.len() as u64),
            });
        }
        Ok(id)
    }

    pub(crate) fn favorite_removed(&self, account: &AccountId, id: &str) -> ProviderResult<()> {
        if self.is_unavailable(account, client_provider::Feature::StickerFavorites) {
            return Err(ProviderError::Unsupported("favorite stickers"));
        }
        let mut state = self.state();
        enter(&mut state, format!("remove {id}"))?;
        if let Some(list) = state.stickers.favorites.get_mut(account) {
            list.retain(|known| known.id != id);
        }
        Ok(())
    }
}
