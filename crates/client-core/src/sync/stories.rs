//! Stories in the sync engine: listing them, following the provider's
//! events, posting, view receipts, replies and reactions, muting, who saw
//! what, the audience, and the timer that lets expired stories go.
//!
//! The rules are the engine's own. The user's action shows in the store at
//! once and the provider is told in the background; a failure that may
//! pass is retried with backoff and never shown; only a refusal puts the
//! old state back, in words. Work resumes on events (a number reconnecting,
//! something queued, the clock reaching a deadline), never on a scan.
//!
//! Viewing is behaviour toward other people: [`SyncEngine::story_shown`]
//! is the only way a view receipt is owed, it is called for a story that
//! was actually shown, and with receipts off nothing is owed at all.

use super::{
    new_client_id, ForwardError, ForwardRefusal, NewMedia, PreparedMedia, SyncEngine, SyncError,
};
use crate::store::{StoreError, StoryFeed};
use client_provider::{
    AccountId, ClientMessageId, Contact, ContactId, Feature, MediaKind, MediaUpload, MessageId,
    NewStory, NewStoryContent, OutgoingContent, OutgoingMessage, ProviderError, Story,
    StoryPrivacy, StoryReplyKind, StoryReplyRef, StoryStyle, Timestamp, UploadProgress,
    STORY_LIFETIME, STORY_TEXT_MAX,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// How old a listing of an account's stories may be before looking at
/// the Status view asks the provider again.
pub const STORIES_FRESH: Duration = Duration::from_secs(5 * 60);
/// How old a listing may be when the user goes to Status before the
/// provider is asked again: somebody who opens Status wants what is up
/// now, not what was up five minutes ago.
pub const STORIES_OPENED: Duration = Duration::from_secs(15);
/// How long the audience read from the provider is believed.
const PRIVACY_FRESH: Duration = Duration::from_secs(10 * 60);
/// How long a list of who saw a story is believed.
const VIEWERS_FRESH: Duration = Duration::from_secs(20);
/// The longest the cleanup timer sleeps when nothing is due.
const IDLE: Duration = Duration::from_secs(6 * 60 * 60);
/// How many times a change is offered to the provider before the engine
/// gives up on it.
const ATTEMPTS: u32 = 8;
/// The side of the small copy a story picture shows from at once.
const THUMBNAIL_SIDE: u32 = 640;

/// What is known of the listing of an account's stories: what the Status
/// view says while it has nothing to list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoryListing {
    /// The provider is being asked right now.
    pub loading: bool,
    /// It answered at least once since the engine started.
    pub listed: bool,
    /// The last time it was asked it did not answer (a dropped
    /// connection, a deadline): asked again by itself.
    pub failed: bool,
}

/// What the stories part of the engine keeps in memory.
pub(super) struct Stories {
    /// Wakes the stories worker: a post or a receipt was queued, an
    /// account is back.
    pub(super) wake: Notify,
    /// When each account's stories were last listed.
    listed: Mutex<HashMap<AccountId, Instant>>,
    /// How each account's listing is doing, and how many are under way.
    listing: Mutex<HashMap<AccountId, (StoryListing, u32)>>,
    /// Whether view receipts are sent (see [`SyncEngine::set_story_receipts`]).
    receipts: AtomicBool,
    /// The newest mute, deletion or audience change asked for per key. A
    /// retry that is no longer the newest stops.
    ops: Mutex<HashMap<(AccountId, String, u8), u64>>,
    /// How far the upload of each story's file is.
    uploads: Mutex<HashMap<ClientMessageId, (u64, u64)>>,
    /// When the viewers of each story were last asked for.
    viewers: Mutex<HashMap<(AccountId, MessageId), Instant>>,
    /// When the audience was last asked for.
    privacy: Mutex<HashMap<AccountId, Instant>>,
    /// The account's own stories the user took down here, and when. What
    /// the provider says of them afterwards (a list that was on its way,
    /// one taken before the provider heard, a late event) does not put
    /// them back; only the provider refusing to delete does. Kept for as
    /// long as a story can live.
    taken_down: Mutex<HashMap<(AccountId, MessageId), Instant>>,
    /// The posts the user gave up, and when. One of them may have reached
    /// the provider all the same (its answer was on its way, or was
    /// lost): when its story turns up it is taken down, not shown.
    given_up: Mutex<HashMap<ClientMessageId, Instant>>,
}

impl Default for Stories {
    fn default() -> Self {
        Self {
            wake: Notify::new(),
            listed: Mutex::new(HashMap::new()),
            listing: Mutex::new(HashMap::new()),
            receipts: AtomicBool::new(true),
            ops: Mutex::new(HashMap::new()),
            uploads: Mutex::new(HashMap::new()),
            viewers: Mutex::new(HashMap::new()),
            privacy: Mutex::new(HashMap::new()),
            taken_down: Mutex::new(HashMap::new()),
            given_up: Mutex::new(HashMap::new()),
        }
    }
}

/// Why a story was not queued.
#[derive(Debug, thiserror::Error)]
pub enum StoryPostError {
    /// The provider cannot post stories.
    #[error("Posting a status is not available from this provider yet.")]
    NotAvailable,
    /// A text story with nothing in it.
    #[error("Write something first.")]
    Blank,
    /// A text longer than a story takes.
    #[error("A status can have at most {STORY_TEXT_MAX} characters.")]
    TooLong,
    /// A file that is not a picture or a video.
    #[error("A status is a picture, a video or a text.")]
    NotAStoryFile,
    /// Larger than the provider takes.
    #[error("The file is larger than can be posted.")]
    TooLarge {
        /// The file's size, in bytes.
        size: u64,
        /// The most the provider takes, in bytes.
        limit: u64,
    },
    /// Nothing in it.
    #[error("The file is empty.")]
    Empty,
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Why a reply or a reaction to a story was not queued.
#[derive(Debug, thiserror::Error)]
pub enum StoryReplyError {
    /// The provider cannot do it.
    #[error("Replying to a status is not available from this provider yet.")]
    NotAvailable,
    /// The story is gone (it expired or was taken down).
    #[error("This status is no longer there.")]
    Gone,
    /// Nothing to send.
    #[error("Write something first.")]
    Blank,
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// What one pass over the story outbox did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StoryPass {
    /// Stories the provider accepted.
    pub posted: usize,
    /// Stories that hit a transient error and will be retried.
    pub retried: usize,
    /// Stories that failed for good.
    pub failed: usize,
    /// The provider refused the credentials.
    pub unauthorized: Option<String>,
}

/// What one pass over the receipts did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReceiptPass {
    /// Receipts the provider took.
    pub sent: usize,
    /// Receipts that will be tried again.
    pub retried: usize,
    /// Receipts dropped: receipts are off, the story is gone, or the
    /// provider refused.
    pub dropped: usize,
}

const OP_MUTE: u8 = 1;
const OP_DELETE: u8 = 2;
const OP_PRIVACY: u8 = 3;

impl SyncEngine {
    // ----- looking ------------------------------------------------------

    /// What the Status view lists at this moment.
    pub fn story_feed(&self, account: &AccountId) -> Result<StoryFeed, StoreError> {
        self.inner.store.story_feed(account, Timestamp::now())
    }

    /// Whether the provider delivers contacts' stories.
    pub fn stories_from_contacts(&self) -> bool {
        self.inner.capabilities.story_contacts
    }

    /// Asks the provider for the account's stories when the copy held is
    /// older than `within`. Called when the Status view is looked at.
    pub fn want_stories(&self, account: &AccountId, within: Duration) {
        if !self.inner.capabilities.story_list || self.is_stopped() {
            return;
        }
        {
            let mut listed = self.inner.stories.listed.lock().expect("stories lock");
            if listed.get(account).is_some_and(|at| at.elapsed() < within) {
                return;
            }
            // Counts as asked from now: a second look does not ask again.
            listed.insert(account.clone(), Instant::now());
        }
        let this = self.clone();
        let account = account.clone();
        self.inner.runtime.spawn(async move {
            let result = this.sync_stories(&account).await;
            if result.is_err() {
                // Not listed: the next look asks again.
                this.inner
                    .stories
                    .listed
                    .lock()
                    .expect("stories lock")
                    .remove(&account);
            }
            this.log_failure("listing stories", result.map(|_| ()));
        });
    }

    /// The user went to Status, or asked for it again ("Check again"):
    /// what the provider remembered about stories not being available is
    /// forgotten, and it is asked now unless it was a moment ago
    /// (`within`; zero asks whatever was asked before).
    pub fn look_at_stories(&self, account: &AccountId, within: Duration) {
        if !self.inner.capabilities.story_list || self.is_stopped() {
            return;
        }
        if self.feature_unavailable(account, Feature::Stories) {
            self.inner.provider.recheck(account, Feature::Stories);
            // What was listed while it was missing says nothing of now.
            self.inner
                .stories
                .listed
                .lock()
                .expect("stories lock")
                .remove(account);
            self.inner.store.notify(crate::StoreChange::Stories {
                account_id: account.clone(),
            });
        }
        self.want_stories(account, within);
    }

    /// How the listing of the account's stories is doing: being asked
    /// for, answered, or not answered the last time.
    pub fn story_listing(&self, account: &AccountId) -> StoryListing {
        self.inner
            .stories
            .listing
            .lock()
            .expect("stories lock")
            .get(account)
            .map(|(listing, _)| *listing)
            .unwrap_or_default()
    }

    /// Records that a listing started or ended, and says so when what
    /// the view would say changed.
    fn note_listing(&self, account: &AccountId, ended: Option<bool>) {
        let changed = {
            let mut all = self.inner.stories.listing.lock().expect("stories lock");
            let (listing, under_way) = all.entry(account.clone()).or_default();
            let before = *listing;
            match ended {
                None => *under_way += 1,
                Some(answered) => {
                    *under_way = under_way.saturating_sub(1);
                    listing.listed |= answered;
                    listing.failed = !answered;
                }
            }
            listing.loading = *under_way > 0;
            *listing != before
        };
        if changed {
            self.inner.store.notify(crate::StoreChange::Stories {
                account_id: account.clone(),
            });
        }
    }

    /// Copies the provider's stories (and which authors are muted) into
    /// the store. Returns how many stories are new here.
    pub async fn sync_stories(&self, account: &AccountId) -> Result<usize, SyncError> {
        if !self.inner.capabilities.story_list {
            return Ok(0);
        }
        self.note_listing(account, None);
        let result = self.sync_stories_once(account).await;
        self.note_listing(account, Some(result.is_ok()));
        result
    }

    async fn sync_stories_once(&self, account: &AccountId) -> Result<usize, SyncError> {
        let inner = &self.inner;
        let missing = self.feature_unavailable(account, Feature::Stories);
        let listed = self.bounded(inner.provider.list_stories(account)).await;
        // Whether contacts' stories are to be had is said in the view: it
        // is told when that changed.
        if self.feature_unavailable(account, Feature::Stories) != missing {
            inner.store.notify(crate::StoreChange::Stories {
                account_id: account.clone(),
            });
        }
        let listed = listed?;
        // Without what the user took down or gave up meanwhile: the list
        // may be older than that.
        let listed: Vec<Story> = listed
            .into_iter()
            .filter(|story| self.story_is_wanted(story))
            .collect();
        let now = Timestamp::now();
        let new = inner.store.sync_stories(account, &listed, now)?;
        if inner.capabilities.story_mute {
            // What the user just muted here and the provider has not
            // heard of yet must not be undone by an older answer.
            let muted = self
                .bounded(inner.provider.muted_story_authors(account))
                .await?;
            if !self.has_pending_ops(account, OP_MUTE) {
                inner.store.put_muted_story_authors(account, &muted)?;
            }
        }
        inner.stories.wake.notify_one();
        Ok(new)
    }

    fn has_pending_ops(&self, account: &AccountId, kind: u8) -> bool {
        self.inner
            .stories
            .ops
            .lock()
            .expect("stories lock")
            .keys()
            .any(|(owner, _, op)| owner == account && *op == kind)
    }

    /// Whether a story the provider tells of belongs in the store. Not
    /// one the user took down here, and not the story of a post the user
    /// gave up: that one is taken down at the provider as well, now that
    /// it is known to be there.
    fn story_is_wanted(&self, story: &Story) -> bool {
        let slot = (story.account_id.clone(), story.id.clone());
        if self
            .inner
            .stories
            .taken_down
            .lock()
            .expect("stories lock")
            .contains_key(&slot)
        {
            return false;
        }
        let given_up = story.client_id.as_ref().is_some_and(|client| {
            self.inner
                .stories
                .given_up
                .lock()
                .expect("stories lock")
                .contains_key(client)
        });
        // Where the provider cannot delete, the story is up and stays up:
        // hiding it would only hide that.
        if given_up && self.inner.capabilities.story_delete {
            self.take_story_down(&story.account_id, &story.id, None);
            return false;
        }
        true
    }

    /// From now on what the provider says of this story does not put it
    /// back in the store.
    fn note_taken_down(&self, account: &AccountId, story: &MessageId) {
        let mut taken = self.inner.stories.taken_down.lock().expect("stories lock");
        taken.retain(|_, at| at.elapsed() < STORY_LIFETIME);
        taken.insert((account.clone(), story.clone()), Instant::now());
    }

    /// Tells the provider to delete one of the account's own stories, in
    /// the background, through drops, and keeps it out of the store
    /// meanwhile and afterwards. If the provider refuses, the story comes
    /// back (`before`, or with the next list) and the problem says why.
    fn take_story_down(&self, account: &AccountId, story: &MessageId, before: Option<Story>) {
        self.note_taken_down(account, story);
        let generation = self.next_op(account, story.as_str(), OP_DELETE);
        let this = self.clone();
        let (account, story) = (account.clone(), story.clone());
        self.inner.runtime.spawn(async move {
            let mut attempt = 0;
            let failure = loop {
                attempt += 1;
                match this
                    .bounded(this.inner.provider.delete_story(&account, &story))
                    .await
                {
                    Ok(()) => break None,
                    Err(error) if error.is_transient() && attempt < ATTEMPTS => {
                        this.note_unauthorized(&error);
                        tokio::time::sleep(
                            error.retry_after().unwrap_or_else(|| this.backoff(attempt)),
                        )
                        .await;
                    }
                    Err(error) => break Some(error),
                }
            };
            this.finish_op(&account, story.as_str(), OP_DELETE, generation);
            if let Some(error) = failure {
                this.note_unauthorized(&error);
                // The provider still has it: what it lists is the truth
                // again.
                this.inner
                    .stories
                    .taken_down
                    .lock()
                    .expect("stories lock")
                    .remove(&(account.clone(), story.clone()));
                if matches!(error, ProviderError::Rejected { .. }) {
                    // Put back, and say why.
                    if let Some(before) = &before {
                        let _ = this.inner.store.upsert_story(before, Timestamp::now());
                    }
                    this.inner.store.notify(crate::StoreChange::Problem {
                        message: format!("The status could not be deleted: {error}"),
                    });
                }
            }
        });
    }

    // ----- viewing ------------------------------------------------------

    /// Whether the story receipts are on. A view receipt is sent only
    /// while "Send read receipts" is on **and** this is.
    pub fn story_receipts(&self) -> bool {
        self.inner.stories.receipts.load(Ordering::Relaxed)
    }

    /// Turns the story view receipts on or off, from now on.
    pub fn set_story_receipts(&self, send: bool) {
        self.inner.stories.receipts.store(send, Ordering::Relaxed);
    }

    /// Whether a view receipt would be sent right now.
    pub fn story_receipts_effective(&self) -> bool {
        self.inner.capabilities.story_view && self.read_receipts() && self.story_receipts()
    }

    /// The story was shown to the user. Records it as seen, here, and
    /// returns `true` the first time. If that was the first time, the
    /// story is somebody else's, the provider can send a view receipt and
    /// receipts are on, the author is owed one: it goes out in the
    /// background, retried through drops. With receipts off viewing is
    /// local only.
    ///
    /// Listing, prefetching and downloading never call this.
    pub fn story_shown(&self, account: &AccountId, story: &MessageId) -> Result<bool, StoreError> {
        let now = Timestamp::now();
        let first = self.inner.store.mark_story_viewed(account, story, now)?;
        if !first {
            return Ok(false);
        }
        if self.story_receipts_effective() {
            if let Some(item) = self.inner.store.story(account, story)? {
                if !item.story.mine {
                    self.inner
                        .store
                        .owe_story_receipt(account, story, &item.story.author, now)?;
                    self.inner.stories.wake.notify_one();
                }
            }
        }
        Ok(true)
    }

    /// Sends the view receipts that are due at `now`.
    pub async fn flush_story_receipts_at(&self, now: Timestamp) -> Result<ReceiptPass, StoreError> {
        let inner = &self.inner;
        let mut pass = ReceiptPass::default();
        for receipt in inner.store.story_receipts_due(now)? {
            if !self.story_receipts_effective() {
                // Turned off since: viewing is local only.
                inner
                    .store
                    .story_receipt_done(&receipt.account_id, &receipt.story)?;
                pass.dropped += 1;
                continue;
            }
            if inner
                .store
                .story(&receipt.account_id, &receipt.story)?
                .is_none()
            {
                inner
                    .store
                    .story_receipt_done(&receipt.account_id, &receipt.story)?;
                pass.dropped += 1;
                continue;
            }
            let result = self
                .bounded(inner.provider.view_story(
                    &receipt.account_id,
                    &receipt.story,
                    &receipt.author,
                ))
                .await;
            match result {
                Ok(()) => {
                    inner
                        .store
                        .story_receipt_done(&receipt.account_id, &receipt.story)?;
                    pass.sent += 1;
                }
                Err(error) if error.is_transient() => {
                    let delay = error
                        .retry_after()
                        .unwrap_or_else(|| inner.config.outbox.backoff(receipt.attempts + 1));
                    inner.store.story_receipt_retry(
                        &receipt.account_id,
                        &receipt.story,
                        Timestamp::from_millis(now.as_millis() + delay.as_millis() as i64),
                    )?;
                    pass.retried += 1;
                }
                Err(error) => {
                    self.note_unauthorized(&error);
                    // Not a verdict on the story, when it is the
                    // credentials: asked again after the next sign-in.
                    if matches!(error, ProviderError::Unauthorized(_)) {
                        inner.store.story_receipt_retry(
                            &receipt.account_id,
                            &receipt.story,
                            now,
                        )?;
                        break;
                    }
                    tracing::debug!(%error, "a story view receipt was refused; dropped");
                    inner
                        .store
                        .story_receipt_done(&receipt.account_id, &receipt.story)?;
                    pass.dropped += 1;
                }
            }
        }
        Ok(pass)
    }

    // ----- posting ------------------------------------------------------

    /// Posts a text story: words on a coloured background. It shows under
    /// "My status" at once and goes through the story outbox.
    pub fn post_story_text(
        &self,
        account: &AccountId,
        text: &str,
        style: StoryStyle,
    ) -> Result<ClientMessageId, StoryPostError> {
        if !self.inner.capabilities.story_post {
            return Err(StoryPostError::NotAvailable);
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(StoryPostError::Blank);
        }
        if text.chars().count() > STORY_TEXT_MAX {
            return Err(StoryPostError::TooLong);
        }
        let post = NewStory {
            client_id: new_client_id(),
            account_id: account.clone(),
            content: NewStoryContent::Text {
                text: text.to_owned(),
                style,
            },
        };
        self.queue_story(post, None)
    }

    /// Checks a picture or a video against what the provider takes and
    /// makes the small copy that shows at once. Touches no store and no
    /// network; call it off the UI thread.
    pub fn prepare_story_media(&self, file: NewMedia) -> Result<PreparedMedia, StoryPostError> {
        let inner = &self.inner;
        if !inner.capabilities.story_post {
            return Err(StoryPostError::NotAvailable);
        }
        if !matches!(file.kind, MediaKind::Image | MediaKind::Video) {
            return Err(StoryPostError::NotAStoryFile);
        }
        let size = file.bytes.len() as u64;
        if size == 0 {
            return Err(StoryPostError::Empty);
        }
        if let Some(limit) = inner.provider.media_upload_limit() {
            if size > limit {
                return Err(StoryPostError::TooLarge { size, limit });
            }
        }
        let thumbnail = (file.kind == MediaKind::Image)
            .then(|| crate::thumbnail(&file.bytes, THUMBNAIL_SIDE).ok())
            .flatten();
        Ok(PreparedMedia { file, thumbnail })
    }

    /// Queues a prepared picture or video as a story.
    pub fn post_story_prepared(
        &self,
        account: &AccountId,
        prepared: PreparedMedia,
    ) -> Result<ClientMessageId, StoryPostError> {
        let inner = &self.inner;
        let PreparedMedia { file, thumbnail } = prepared;
        let client_id = new_client_id();
        let local = crate::outbox::local_media_ref(&client_id);
        if let Some(small) = thumbnail {
            let cached = crate::CachedMedia {
                size: Some((small.width, small.height)),
                mime: Some(small.mime.to_owned()),
                bytes: small.bytes,
            };
            inner.store.put_media(
                &super::thumbnail_key(local.as_str()),
                &cached,
                Timestamp::now(),
                inner.config.media_budget,
            )?;
        }
        let post = NewStory {
            client_id: client_id.clone(),
            account_id: account.clone(),
            content: NewStoryContent::Media {
                kind: file.kind,
                media: local,
                mime_type: Some(file.mime_type.clone()),
                caption: file.caption.filter(|caption| !caption.trim().is_empty()),
            },
        };
        let stored = crate::outbox::OutboxFile {
            bytes: std::sync::Arc::new(file.bytes),
            mime: file.mime_type,
            file_name: file.file_name,
        };
        self.queue_story(post, Some(stored))
    }

    fn queue_story(
        &self,
        post: NewStory,
        file: Option<crate::outbox::OutboxFile>,
    ) -> Result<ClientMessageId, StoryPostError> {
        let inner = &self.inner;
        inner.store.enqueue_story_post(
            &post,
            file.as_ref(),
            Timestamp::now(),
            inner.config.outbox.max_age,
        )?;
        inner.stories.wake.notify_one();
        Ok(post.client_id)
    }

    /// How far the upload of a queued story's file is: bytes out, bytes in
    /// all. `None` when it is not being uploaded right now.
    pub fn story_upload_progress(&self, client_id: &ClientMessageId) -> Option<(u64, u64)> {
        self.inner
            .stories
            .uploads
            .lock()
            .expect("stories lock")
            .get(client_id)
            .copied()
    }

    /// Takes a failed post back in the queue, to be tried now.
    pub fn retry_story_post(&self, client_id: &ClientMessageId) -> Result<bool, StoreError> {
        let queued = self.inner.store.story_post_retry_now(
            client_id,
            Timestamp::now(),
            self.inner.config.outbox.max_age,
        )?;
        if queued {
            self.inner.stories.wake.notify_one();
        }
        Ok(queued)
    }

    /// Gives up on a story that was not posted, as far as is known here.
    /// If it reached the provider all the same (the request was on its
    /// way, or its answer was lost), it is taken down there as soon as
    /// that shows, and never listed here.
    pub fn discard_story_post(&self, client_id: &ClientMessageId) -> Result<bool, StoreError> {
        self.inner
            .stories
            .uploads
            .lock()
            .expect("stories lock")
            .remove(client_id);
        let discarded = self.inner.store.story_post_discard(client_id)?;
        if discarded {
            let mut given_up = self.inner.stories.given_up.lock().expect("stories lock");
            given_up.retain(|_, at| at.elapsed() < STORY_LIFETIME);
            given_up.insert(client_id.clone(), Instant::now());
        }
        Ok(discarded)
    }

    /// Uploads the file of a queued story, once, and records what came of
    /// it. Every attempt carries the same key.
    async fn upload_story(
        &self,
        entry: &crate::store::StoryPostEntry,
        now: Timestamp,
    ) -> Result<UploadStep, StoreError> {
        let inner = &self.inner;
        let post = &entry.post;
        let NewStoryContent::Media {
            kind, mime_type, ..
        } = &post.content
        else {
            return Ok(UploadStep::Done);
        };
        let Some(file) = inner.store.story_post_file(&post.client_id)? else {
            inner
                .store
                .story_post_fail(post, "The file is no longer on this computer")?;
            return Ok(UploadStep::Failed);
        };
        if !inner
            .store
            .story_post_set_state(&post.client_id, "uploading")?
        {
            return Ok(UploadStep::Gone);
        }
        let total = file.bytes.len() as u64;
        let client_id = post.client_id.clone();
        let progress: UploadProgress = {
            let this = self.clone();
            let client_id = client_id.clone();
            let account = post.account_id.clone();
            this.inner
                .stories
                .uploads
                .lock()
                .expect("stories lock")
                .insert(client_id.clone(), (0, total));
            std::sync::Arc::new(move |sent| {
                this.inner
                    .stories
                    .uploads
                    .lock()
                    .expect("stories lock")
                    .insert(client_id.clone(), (sent.min(total), total));
                this.inner.store.notify(crate::StoreChange::Stories {
                    account_id: account.clone(),
                });
            })
        };
        let upload = MediaUpload {
            key: entry.upload_key(),
            kind: *kind,
            bytes: file.bytes.clone(),
            mime_type: mime_type.clone().unwrap_or_else(|| file.mime.clone()),
            file_name: file.file_name.clone(),
        };
        let attempt = tokio::time::timeout(
            inner.config.outbox.upload_timeout,
            inner
                .provider
                .upload_media(&post.account_id, upload, progress),
        );
        let result = match attempt.await {
            Ok(result) => result,
            Err(_) => Err(ProviderError::Transient("the upload timed out".to_owned())),
        };
        inner
            .stories
            .uploads
            .lock()
            .expect("stories lock")
            .remove(&post.client_id);
        Ok(match result {
            Ok(reference) => {
                if inner
                    .store
                    .story_post_set_uploaded(&post.client_id, &reference)?
                {
                    UploadStep::Done
                } else {
                    UploadStep::Gone
                }
            }
            Err(error) if error.is_transient() => {
                let delay = error
                    .retry_after()
                    .unwrap_or_else(|| inner.config.outbox.backoff(entry.attempts + 1));
                let next = Timestamp::from_millis(now.as_millis() + delay.as_millis() as i64);
                inner
                    .store
                    .story_post_retry_later(&post.client_id, &error.to_string(), next)?;
                UploadStep::Retry
            }
            Err(ProviderError::Unauthorized(reason)) => {
                inner
                    .store
                    .story_post_retry_later(&post.client_id, "not signed in", now)?;
                UploadStep::Unauthorized(reason)
            }
            Err(error) => {
                inner.store.story_post_fail(post, &error.to_string())?;
                UploadStep::Failed
            }
        })
    }

    /// Makes one pass over the story outbox: attempts every story that is
    /// due at `now`, oldest first, and records the outcome of each. A
    /// story that is waiting to be retried holds back the later ones of
    /// its account, so they appear in the order they were posted.
    pub async fn flush_story_posts_at(&self, now: Timestamp) -> Result<StoryPass, StoreError> {
        let inner = &self.inner;
        let mut pass = StoryPass::default();
        let mut blocked: std::collections::HashSet<AccountId> = Default::default();
        for mut entry in inner.store.story_posts_pending()? {
            let account = entry.post.account_id.clone();
            if entry.expires_at <= now {
                let reason = match &entry.last_error {
                    Some(error) => format!("Could not be posted in time ({error})"),
                    None => "Could not be posted in time".to_owned(),
                };
                inner.store.story_post_fail(&entry.post, &reason)?;
                pass.failed += 1;
                continue;
            }
            if blocked.contains(&account) {
                continue;
            }
            if entry.next_attempt_at > now {
                blocked.insert(account);
                continue;
            }
            if entry.needs_upload() {
                match self.upload_story(&entry, now).await? {
                    UploadStep::Done => {
                        let again = inner
                            .store
                            .story_posts_pending()?
                            .into_iter()
                            .find(|known| known.post.client_id == entry.post.client_id);
                        match again {
                            Some(again) => entry = again,
                            None => continue,
                        }
                    }
                    UploadStep::Retry => {
                        blocked.insert(account);
                        pass.retried += 1;
                        continue;
                    }
                    UploadStep::Failed => {
                        pass.failed += 1;
                        continue;
                    }
                    UploadStep::Unauthorized(reason) => {
                        pass.unauthorized = Some(reason);
                        break;
                    }
                    UploadStep::Gone => continue,
                }
            }
            let mut submitted = entry.post.clone();
            if let (NewStoryContent::Media { media, .. }, Some(uploaded)) =
                (&mut submitted.content, &entry.uploaded)
            {
                *media = uploaded.clone();
            }
            if !inner
                .store
                .story_post_set_state(&entry.post.client_id, "posting")?
            {
                continue;
            }
            let attempt = tokio::time::timeout(
                inner.config.outbox.send_timeout,
                inner.provider.post_story(submitted),
            );
            let result = match attempt.await {
                Ok(result) => result,
                Err(_) => Err(ProviderError::Transient("the post timed out".to_owned())),
            };
            match result {
                Ok(mut story) => {
                    story.mine = true;
                    story.client_id = Some(entry.post.client_id.clone());
                    // Given up while the request was on its way: taken
                    // down, not shown.
                    if !self.story_is_wanted(&story) {
                        continue;
                    }
                    inner.store.upsert_story(&story, now)?;
                    inner.store.story_post_settle(&entry.post)?;
                    pass.posted += 1;
                }
                Err(error) if error.is_transient() => {
                    let delay = error
                        .retry_after()
                        .unwrap_or_else(|| inner.config.outbox.backoff(entry.attempts + 1));
                    let next = Timestamp::from_millis(now.as_millis() + delay.as_millis() as i64);
                    tracing::debug!(client_id = %entry.post.client_id, %error, ?delay, "a story will be retried");
                    inner.store.story_post_retry_later(
                        &entry.post.client_id,
                        &error.to_string(),
                        next,
                    )?;
                    blocked.insert(account);
                    pass.retried += 1;
                }
                Err(ProviderError::Unauthorized(reason)) => {
                    inner.store.story_post_retry_later(
                        &entry.post.client_id,
                        "not signed in",
                        now,
                    )?;
                    pass.unauthorized = Some(reason);
                    break;
                }
                Err(ProviderError::Rejected { code, .. })
                    if code == "upload_expired" && entry.uploaded.is_some() =>
                {
                    inner
                        .store
                        .story_post_upload_again(&entry.post.client_id, now)?;
                    blocked.insert(account);
                    pass.retried += 1;
                }
                Err(error) => {
                    tracing::warn!(client_id = %entry.post.client_id, %error, "a story failed for good");
                    inner
                        .store
                        .story_post_fail(&entry.post, &error.to_string())?;
                    pass.failed += 1;
                }
            }
        }
        Ok(pass)
    }

    // ----- taking down --------------------------------------------------

    /// Takes one of the account's own stories down. It goes from the store
    /// at once; the provider is told in the background, through drops. If
    /// the provider refuses, the story comes back and the problem says why.
    pub fn delete_story(&self, account: &AccountId, story: &MessageId) {
        let inner = &self.inner;
        let before = match inner.store.story(account, story) {
            Ok(Some(item)) => item,
            _ => {
                // A post that was never posted: nothing to tell anybody.
                if let Some(client) = story.as_str().strip_prefix("story-local:") {
                    let _ = self.discard_story_post(&ClientMessageId::new(client));
                }
                return;
            }
        };
        // Before it goes from the store: a list that lands in between
        // must not put it back.
        if inner.capabilities.story_delete {
            self.note_taken_down(account, story);
        }
        if let Err(error) = inner.store.remove_story(account, story) {
            tracing::error!(%error, "could not take the story down");
            inner
                .stories
                .taken_down
                .lock()
                .expect("stories lock")
                .remove(&(account.clone(), story.clone()));
            return;
        }
        if !inner.capabilities.story_delete {
            return;
        }
        self.take_story_down(account, story, Some(before.story));
    }

    fn next_op(&self, account: &AccountId, key: &str, kind: u8) -> u64 {
        let mut ops = self.inner.stories.ops.lock().expect("stories lock");
        let entry = ops
            .entry((account.clone(), key.to_owned(), kind))
            .or_insert(0);
        *entry += 1;
        *entry
    }

    fn is_newest_op(&self, account: &AccountId, key: &str, kind: u8, generation: u64) -> bool {
        self.inner.stories.ops.lock().expect("stories lock").get(&(
            account.clone(),
            key.to_owned(),
            kind,
        )) == Some(&generation)
    }

    fn finish_op(&self, account: &AccountId, key: &str, kind: u8, generation: u64) {
        let mut ops = self.inner.stories.ops.lock().expect("stories lock");
        let slot = (account.clone(), key.to_owned(), kind);
        if ops.get(&slot) == Some(&generation) {
            ops.remove(&slot);
        }
    }

    // ----- who saw ------------------------------------------------------

    /// Asks the provider who saw one of the account's own stories, unless
    /// it was asked a moment ago. The answer lands in the store.
    pub fn want_story_viewers(&self, account: &AccountId, story: &MessageId) {
        let inner = &self.inner;
        if !inner.capabilities.story_viewers || self.is_stopped() {
            return;
        }
        {
            let mut asked = inner.stories.viewers.lock().expect("stories lock");
            let key = (account.clone(), story.clone());
            if asked
                .get(&key)
                .is_some_and(|at| at.elapsed() < VIEWERS_FRESH)
            {
                return;
            }
            asked.insert(key, Instant::now());
        }
        let this = self.clone();
        let (account, story) = (account.clone(), story.clone());
        inner.runtime.spawn(async move {
            let result = this
                .bounded(this.inner.provider.story_viewers(&account, &story))
                .await;
            match result {
                Ok(viewers) => {
                    let _ = this
                        .inner
                        .store
                        .put_story_viewers(&account, &story, &viewers);
                }
                Err(error) => {
                    this.note_unauthorized(&error);
                    // Asked again at the next look.
                    this.inner
                        .stories
                        .viewers
                        .lock()
                        .expect("stories lock")
                        .remove(&(account, story));
                }
            }
        });
    }

    // ----- muting -------------------------------------------------------

    /// Mutes or unmutes an author's stories. It shows at once; a provider
    /// that keeps mutes is told in the background (the newest change
    /// wins), one that does not leaves it local.
    pub fn set_story_muted(&self, account: &AccountId, author: &ContactId, muted: bool) {
        let inner = &self.inner;
        if let Err(error) = inner.store.set_story_muted(account, author, muted) {
            tracing::error!(%error, "could not store the mute");
            return;
        }
        if !inner.capabilities.story_mute {
            return;
        }
        let generation = self.next_op(account, author.as_str(), OP_MUTE);
        let this = self.clone();
        let (account, author) = (account.clone(), author.clone());
        inner.runtime.spawn(async move {
            let mut attempt = 0;
            loop {
                if !this.is_newest_op(&account, author.as_str(), OP_MUTE, generation) {
                    return;
                }
                attempt += 1;
                match this
                    .bounded(
                        this.inner
                            .provider
                            .set_story_muted(&account, &author, muted),
                    )
                    .await
                {
                    Ok(()) => break,
                    Err(error) if error.is_transient() && attempt < ATTEMPTS => {
                        this.note_unauthorized(&error);
                        tokio::time::sleep(
                            error.retry_after().unwrap_or_else(|| this.backoff(attempt)),
                        )
                        .await;
                    }
                    Err(error) => {
                        this.note_unauthorized(&error);
                        // Kept here all the same: the mute is the user's.
                        tracing::debug!(%error, "the provider did not take the mute");
                        break;
                    }
                }
            }
            this.finish_op(&account, author.as_str(), OP_MUTE, generation);
        });
    }

    // ----- the audience -------------------------------------------------

    /// The audience as last known, and whether the provider can be asked
    /// to change it.
    pub fn story_privacy_cached(
        &self,
        account: &AccountId,
    ) -> Result<Option<StoryPrivacy>, StoreError> {
        Ok(self
            .inner
            .store
            .cached_story_privacy(account)?
            .map(|(privacy, _)| privacy))
    }

    /// Asks the provider who sees the account's stories, unless that was
    /// asked lately.
    pub fn want_story_privacy(&self, account: &AccountId) {
        let inner = &self.inner;
        if !inner.capabilities.story_privacy || self.is_stopped() {
            return;
        }
        {
            let mut asked = inner.stories.privacy.lock().expect("stories lock");
            if asked
                .get(account)
                .is_some_and(|at| at.elapsed() < PRIVACY_FRESH)
            {
                return;
            }
            asked.insert(account.clone(), Instant::now());
        }
        let this = self.clone();
        let account = account.clone();
        inner.runtime.spawn(async move {
            match this
                .bounded(this.inner.provider.story_privacy(&account))
                .await
            {
                Ok(privacy) => {
                    // What the user just set and the provider has not
                    // heard of yet is not undone by an older answer.
                    if !this.has_pending_ops(&account, OP_PRIVACY) {
                        let _ = this.inner.store.put_story_privacy(
                            &account,
                            &privacy,
                            Timestamp::now(),
                        );
                    }
                }
                Err(error) => {
                    this.note_unauthorized(&error);
                    this.inner
                        .stories
                        .privacy
                        .lock()
                        .expect("stories lock")
                        .remove(&account);
                }
            }
        });
    }

    /// Changes who sees the account's stories. It shows at once and the
    /// provider is told in the background, through drops; only a refusal
    /// puts the old audience back, in words.
    pub fn set_story_privacy(&self, account: &AccountId, privacy: StoryPrivacy) {
        let inner = &self.inner;
        if !inner.capabilities.story_privacy_edit {
            return;
        }
        let before = inner
            .store
            .cached_story_privacy(account)
            .ok()
            .flatten()
            .map(|(privacy, _)| privacy);
        if let Err(error) = inner
            .store
            .put_story_privacy(account, &privacy, Timestamp::now())
        {
            tracing::error!(%error, "could not store the audience");
            return;
        }
        let generation = self.next_op(account, "audience", OP_PRIVACY);
        let this = self.clone();
        let account = account.clone();
        inner.runtime.spawn(async move {
            let mut attempt = 0;
            let failure = loop {
                if !this.is_newest_op(&account, "audience", OP_PRIVACY, generation) {
                    return;
                }
                attempt += 1;
                match this
                    .bounded(this.inner.provider.set_story_privacy(&account, &privacy))
                    .await
                {
                    Ok(()) => break None,
                    Err(error) if error.is_transient() && attempt < ATTEMPTS => {
                        this.note_unauthorized(&error);
                        tokio::time::sleep(
                            error.retry_after().unwrap_or_else(|| this.backoff(attempt)),
                        )
                        .await;
                    }
                    Err(error) => break Some(error),
                }
            };
            this.finish_op(&account, "audience", OP_PRIVACY, generation);
            if let Some(error) = failure {
                this.note_unauthorized(&error);
                if let (ProviderError::Rejected { .. }, Some(before)) = (&error, before) {
                    let _ = this
                        .inner
                        .store
                        .put_story_privacy(&account, &before, Timestamp::now());
                    this.inner.store.notify(crate::StoreChange::Problem {
                        message: format!("Who sees your status could not be changed: {error}"),
                    });
                }
            }
        });
    }

    // ----- replying and reacting ----------------------------------------

    /// The direct chat with a story's author, created in the store if the
    /// account has none with them yet.
    fn chat_of_story_author(&self, story: &Story) -> Result<client_provider::ChatId, StoreError> {
        let contact = self
            .inner
            .store
            .contact(&story.account_id, &story.author)?
            .unwrap_or_else(|| {
                let mut contact = Contact::new(story.account_id.clone(), story.author.clone());
                contact.profile_name = story.author_name.clone();
                contact
            });
        self.chat_with_contact(&contact)
    }

    /// Replies to somebody's story with a text: a message to the author
    /// that quotes the story, shown in their chat as a reply to it. It is
    /// queued like any message, so it goes through drops and is never
    /// sent twice.
    pub fn reply_to_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        text: &str,
        mentions: Vec<ContactId>,
    ) -> Result<ClientMessageId, StoryReplyError> {
        let inner = &self.inner;
        if !inner.capabilities.story_reply {
            return Err(StoryReplyError::NotAvailable);
        }
        let text = text.trim();
        if text.is_empty() {
            return Err(StoryReplyError::Blank);
        }
        let item = inner
            .store
            .story(account, story)?
            .ok_or(StoryReplyError::Gone)?;
        let chat = self.chat_of_story_author(&item.story)?;
        let client_id = new_client_id();
        inner.store.enqueue_story_answer(
            &OutgoingMessage {
                client_id: client_id.clone(),
                account_id: account.clone(),
                chat_id: chat,
                content: OutgoingContent::Text {
                    body: text.to_owned(),
                },
                reply_to: Some(item.story.id.clone()),
                mentions: self.mentions_of(mentions),
                forwarded: false,
            },
            &item.story.id,
            Timestamp::now(),
            inner.config.outbox.max_age,
        )?;
        inner.store.mark_story_reply(
            account,
            &client_id,
            &StoryReplyRef {
                story: item.story.id.clone(),
                kind: StoryReplyKind::of(&item.story.body),
                preview: item.story.body.words().map(|words| cut(words, 80)),
                of_mine: item.story.mine,
            },
        )?;
        inner.outbox_wake.notify_one();
        Ok(client_id)
    }

    /// Reacts to somebody's story with one emoji (an empty one takes the
    /// reaction back). Queued like a message.
    pub fn react_to_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        emoji: &str,
    ) -> Result<ClientMessageId, StoryReplyError> {
        let inner = &self.inner;
        if !inner.capabilities.story_react {
            return Err(StoryReplyError::NotAvailable);
        }
        let item = inner
            .store
            .story(account, story)?
            .ok_or(StoryReplyError::Gone)?;
        let chat = self.chat_of_story_author(&item.story)?;
        let client_id = new_client_id();
        inner.store.enqueue_story_answer(
            &OutgoingMessage {
                client_id: client_id.clone(),
                account_id: account.clone(),
                chat_id: chat,
                content: OutgoingContent::Reaction {
                    target: item.story.id.clone(),
                    emoji: emoji.to_owned(),
                },
                reply_to: None,
                mentions: Vec::new(),
                forwarded: false,
            },
            &item.story.id,
            Timestamp::now(),
            inner.config.outbox.max_age,
        )?;
        inner.outbox_wake.notify_one();
        Ok(client_id)
    }

    // ----- passing a story on -------------------------------------------

    /// Why a story cannot be passed on to a chat, or `None` when it can.
    /// The rules are a message's ([`SyncEngine::forward_refusal`]): the
    /// words of a text story are sent again with the mark, a picture, a
    /// video or a voice note needs a provider that forwards by naming
    /// the original ([`Capabilities::forward_any`](client_provider::Capabilities)).
    /// A story that is gone is refused as a deleted message is.
    pub fn story_forward_refusal(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> Result<Option<ForwardRefusal>, StoreError> {
        Ok(match self.inner.store.story(account, story)? {
            Some(item) => self.forward_refusal(&message_of(&item.story)),
            None => Some(ForwardRefusal::Deleted),
        })
    }

    /// Passes a story on to a chat, marked as forwarded, through the
    /// outbox: [`SyncEngine::forward_message`] with the story as the
    /// message it is, so a text goes as a text and anything else by
    /// naming the story ([`Provider::forward_messages`](client_provider::Provider::forward_messages)),
    /// no file passing through here.
    pub fn forward_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        to: &client_provider::ChatId,
    ) -> Result<ClientMessageId, ForwardError> {
        let item = self
            .inner
            .store
            .story(account, story)?
            .ok_or(ForwardError::Refused(ForwardRefusal::Deleted))?;
        self.forward_message(account, &message_of(&item.story), to)
    }

    // ----- events -------------------------------------------------------

    /// Applies a story event from the provider.
    pub(super) fn apply_story_event(
        &self,
        event: client_provider::ProviderEvent,
    ) -> Result<(), StoreError> {
        use client_provider::ProviderEvent as E;
        let store = &self.inner.store;
        match event {
            // Not one the user took down or gave up: the event may be
            // older than that.
            E::StoryUpserted(story) => {
                if self.story_is_wanted(&story) {
                    store.upsert_story(&story, Timestamp::now())?;
                    self.inner.stories.wake.notify_one();
                }
            }
            E::StoryRemoved {
                account_id,
                story_id,
            } => {
                store.remove_story(&account_id, &story_id)?;
            }
            E::StoryViewed {
                account_id,
                story_id,
                viewer,
            } => store.add_story_viewer(&account_id, &story_id, &viewer)?,
            E::StoryMuteChanged {
                account_id,
                contact,
                muted,
            } => store.set_story_muted(&account_id, &contact, muted)?,
            _ => {}
        }
        Ok(())
    }

    /// An account is back: its stories are listed again and what waited
    /// to be posted goes now.
    pub(super) fn stories_reconnected(&self, account: &AccountId) {
        let inner = &self.inner;
        let _ = inner
            .store
            .story_posts_flush_account(account, Timestamp::now());
        inner.stories.wake.notify_one();
        self.want_stories(account, Duration::from_secs(60));
    }

    // ----- the worker ---------------------------------------------------

    /// Lets expired stories go (and the media cached for them), posts what
    /// is due and sends the receipts owed, then sleeps until the next
    /// deadline or until woken. Runs once at start, so stories that
    /// expired while the application was closed are gone before anything
    /// is shown.
    pub(super) async fn run_stories(self) {
        let inner = &self.inner;
        if let Err(error) = inner.store.story_posts_recover() {
            tracing::error!(%error, "could not recover the story outbox");
        }
        loop {
            if self.is_auth_lost() {
                return;
            }
            let now = Timestamp::now();
            if let Err(error) = inner.store.expire_stories(now) {
                tracing::error!(%error, "could not clean up expired stories");
            }
            if let Err(error) = self.flush_story_posts_at(now).await {
                tracing::error!(%error, "the story outbox failed");
            }
            if let Err(error) = self.flush_story_receipts_at(now).await {
                tracing::error!(%error, "story receipts failed");
            }
            let soonest = [
                inner.store.next_story_expiry().ok().flatten(),
                inner.store.story_posts_next_wakeup().ok().flatten(),
                inner.store.next_story_receipt().ok().flatten(),
            ]
            .into_iter()
            .flatten()
            .min();
            let wait = soonest
                .map(|at| {
                    Duration::from_millis(
                        (at.as_millis() - Timestamp::now().as_millis()).max(0) as u64
                    )
                    // Never a tight loop on a deadline that did not move.
                    .max(Duration::from_millis(250))
                })
                .unwrap_or(IDLE)
                .min(IDLE);
            tokio::select! {
                _ = inner.stories.wake.notified() => {}
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }
}

/// What one try at uploading a story's file did.
enum UploadStep {
    Done,
    Retry,
    Failed,
    Unauthorized(String),
    Gone,
}

/// A story as the message it is (a story is a message to the status
/// list): what forwarding needs of it, its id and what it carries.
fn message_of(story: &Story) -> client_provider::Message {
    use client_provider::{DeliveryStatus, Direction, Message, MessageContent, StoryBody};
    Message {
        id: story.id.clone(),
        client_id: story.client_id.clone(),
        account_id: story.account_id.clone(),
        chat_id: client_provider::ChatId::new(story.author.as_str()),
        sender: story.author.clone(),
        sender_name: story.author_name.clone(),
        direction: if story.mine {
            Direction::Outgoing
        } else {
            Direction::Incoming
        },
        timestamp: story.posted_at,
        content: match &story.body {
            StoryBody::Text { text, .. } => MessageContent::text(text.clone()),
            StoryBody::Media(media) => MessageContent::Media(media.clone()),
        },
        reply_to: None,
        status: DeliveryStatus::Sent,
        edited: false,
        deleted: false,
        extras: Default::default(),
    }
}

/// Cuts a text to `max` characters, on a character.
fn cut(text: &str, max: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{}…", head.trim_end())
    } else {
        head
    }
}
