//! Pictures and message media, as the views see them.
//!
//! Everything shown comes from the store's encrypted cache, where the
//! engine puts small, re-encoded images (see `client-core`'s `imaging`):
//! a 96 px avatar, a thumbnail of at most 720 px. The views ask the
//! [`MediaShelf`] while they draw; it answers from memory, reads the store
//! on a first look, and tells the engine what is on screen so that it is
//! fetched: lazily, because only rows that are drawn ever ask.
//!
//! Decoding to pixels is GPUI's and happens off the UI thread. A row's
//! size never depends on whether its picture has arrived: an avatar has
//! the size of its placeholder, and an image's box is decided the first
//! time the row is drawn and kept.
//!
//! Animated stickers and GIFs are the one thing kept as it came: the
//! engine stores the animated file next to its first frame, and the shelf
//! decodes it (behind limits, on the blocking pool) into frames it paints
//! itself. Only a row that is painted plays, and it is the painting that
//! asks for the next repaint, when its next frame is due: with nothing
//! animated on screen, nothing repaints.

use crate::animation::{Animation, Motion, Playback};
use crate::audio::Shape;
use crate::motion::animated::{IDLE, MIN_TICK};
use crate::settings::MediaChoice;
use crate::theme::media_px;
use client_core::{
    animation_key, file_key, thumbnail_key, AnimationLimits, MediaState, Store, StoreChange,
    SyncEngine,
};
use client_provider::{AccountId, ChatId, Media, MediaKind, Timestamp};
use gpui_kit::{Image, ImageFormat, Pixels, RenderImage, SharedString, Size};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The most memory the decoded animations of the open chat may take
/// together. One animation is at most `AnimationLimits::max_bytes`.
const ANIMATION_BUDGET: usize = 96 * 1024 * 1024;

/// What the blocking pool hands back: an image's url and its animation.
type Decoded = Arc<Mutex<Vec<(String, Option<Animation>)>>>;

/// Where the moving version of an image stands.
enum Moving {
    /// It does not move, or cannot be shown moving: the first frame is
    /// all there is.
    Still,
    /// The engine was asked whether it moves.
    Wanted,
    /// Its frames are being decoded.
    Decoding,
    /// It plays.
    Ready {
        animation: Rc<Animation>,
        playback: Playback,
        /// When it was last painted, and with which frame.
        painted: Option<(Instant, usize)>,
    },
}

/// What to paint of an animation right now.
pub struct Playing {
    /// Every frame.
    pub image: Arc<RenderImage>,
    /// The one to paint.
    pub frame: usize,
    /// When the next one is due; `None` while it is held.
    pub next: Option<Duration>,
}

/// The box of an image whose size is not known yet. The API rarely says
/// how large an image is, so every image starts in this box and keeps it
/// when it arrives (fitted inside): the list never jumps.
#[allow(non_snake_case)]
pub fn PLACEHOLDER() -> Size<Pixels> {
    Size {
        width: media_px(300.),
        height: media_px(200.),
    }
}

/// The largest an image is shown in a bubble.
#[allow(non_snake_case)]
fn MAX_IMAGE() -> Size<Pixels> {
    Size {
        width: media_px(300.),
        height: media_px(340.),
    }
}

/// The side of a sticker.
#[allow(non_snake_case)]
pub fn STICKER() -> Pixels {
    media_px(160.)
}

/// What to draw for a media attachment.
#[derive(Clone)]
pub enum MediaVisual {
    /// The image.
    Image(Arc<Image>),
    /// On its way. `fraction` is how much has arrived, when the message
    /// says how large the file is.
    Loading {
        /// From 0 to 1.
        fraction: Option<f32>,
    },
    /// It did not arrive, with the reason. Asking again may work.
    Failed(SharedString),
    /// It will never come, with the reason: WhatsApp no longer has it.
    Unavailable(SharedString),
    /// The provider has no file for it (a message imported with the
    /// account's history).
    NoFile,
    /// The file is of a kind that cannot be drawn here, with what it is.
    /// It is not fetched.
    Unsupported(SharedString),
    /// Not loaded by itself (the user's choice, or it is too large to be
    /// fetched unasked): a click loads it.
    Download {
        /// Its size, when the message says.
        size: Option<u64>,
    },
}

/// An account and one of its chats.
type ChatKey = (AccountId, ChatId);

/// The images the views show, and the engine that fetches them.
pub struct MediaShelf {
    engine: SyncEngine,
    store: Arc<Store>,
    /// `None`: looked up, and there is none (yet).
    avatars: RefCell<HashMap<ChatKey, Option<Arc<Image>>>>,
    thumbnails: RefCell<HashMap<String, Option<Arc<Image>>>>,
    /// The box each image of the open chat was first drawn in.
    boxes: RefCell<HashMap<String, Size<Pixels>>>,
    /// Images the user asked for with a click, whatever the policy.
    asked: RefCell<HashSet<String>>,
    /// Images the user stopped while they were loading by themselves.
    declined: RefCell<HashSet<String>>,
    /// The length and waveform of audio that has been decoded once.
    shapes: RefCell<HashMap<String, Option<Shape>>>,
    policy: Cell<MediaChoice>,
    /// The stickers and GIFs of the open chat that move, by url.
    moving: RefCell<HashMap<String, Moving>>,
    /// Animations the blocking pool has finished decoding (`None`: it
    /// turned out not to be one).
    decoded: Decoded,
    /// Animations let go, whose frames the window still holds.
    released: RefCell<Vec<Arc<RenderImage>>>,
    /// The memory the decoded animations may take together.
    animation_budget: Cell<usize>,
    /// When the repaint that is already asked for happens.
    wake: Cell<Option<Instant>>,
    /// Counts the repaints asked for, so one that was overtaken by an
    /// earlier one does nothing.
    wake_serial: Cell<u64>,
}

pub(super) fn image_of(bytes: Vec<u8>, mime: Option<&str>) -> Option<Arc<Image>> {
    let format = match mime? {
        "image/jpeg" | "image/jpg" => ImageFormat::Jpeg,
        "image/png" => ImageFormat::Png,
        "image/webp" => ImageFormat::Webp,
        "image/gif" => ImageFormat::Gif,
        _ => return None,
    };
    Some(Arc::new(Image::from_bytes(format, bytes)))
}

/// The format of image bytes this application made itself.
fn own_image(bytes: Vec<u8>) -> Arc<Image> {
    let format = if bytes.starts_with(b"\x89PNG") {
        ImageFormat::Png
    } else {
        ImageFormat::Jpeg
    };
    Arc::new(Image::from_bytes(format, bytes))
}

impl MediaShelf {
    /// A shelf over an engine's store.
    pub fn new(engine: SyncEngine) -> Self {
        Self {
            store: engine.store().clone(),
            engine,
            avatars: RefCell::default(),
            thumbnails: RefCell::default(),
            boxes: RefCell::default(),
            asked: RefCell::default(),
            declined: RefCell::default(),
            shapes: RefCell::default(),
            policy: Cell::new(MediaChoice::default()),
            moving: RefCell::default(),
            decoded: Arc::default(),
            released: RefCell::default(),
            animation_budget: Cell::new(ANIMATION_BUDGET),
            wake: Cell::new(None),
            wake_serial: Cell::new(0),
        }
    }

    /// What is loaded without being asked.
    pub fn set_policy(&self, policy: MediaChoice) {
        self.policy.set(policy);
    }

    /// A chat's picture, if it has one that has arrived. Asking is what
    /// gets it fetched.
    pub fn avatar(&self, account: &AccountId, chat: &ChatId) -> Option<Arc<Image>> {
        self.engine.want_avatar(account, chat);
        let key = (account.clone(), chat.clone());
        if let Some(known) = self.avatars.borrow().get(&key) {
            return known.clone();
        }
        let image = self
            .store
            .avatar(account, chat)
            .ok()
            .flatten()
            .and_then(|stored| stored.image)
            .map(own_image);
        self.avatars.borrow_mut().insert(key, image.clone());
        image
    }

    /// A picture kept in the store under `subject` that no provider is
    /// asked about: the icon the user chose for a number.
    pub fn stored_picture(&self, account: &AccountId, subject: &ChatId) -> Option<Arc<Image>> {
        let key = (account.clone(), subject.clone());
        if let Some(known) = self.avatars.borrow().get(&key) {
            return known.clone();
        }
        let image = self
            .store
            .avatar(account, subject)
            .ok()
            .flatten()
            .and_then(|stored| stored.image)
            .map(own_image);
        self.avatars.borrow_mut().insert(key, image.clone());
        image
    }

    /// A picture changed in the store: look again next time.
    pub fn forget_avatar(&self, account: &AccountId, chat: &ChatId) {
        self.avatars
            .borrow_mut()
            .remove(&(account.clone(), chat.clone()));
    }

    /// Something was cached (or could not be) under `key`.
    pub fn forget_media(&self, key: &str) {
        if let Some(url) = key.strip_prefix("thumb:") {
            self.thumbnails.borrow_mut().remove(url);
        }
        if let Some(url) = key.strip_prefix("anim:") {
            // The engine answered whether it moves: look again.
            let mut moving = self.moving.borrow_mut();
            if matches!(moving.get(url), Some(Moving::Wanted)) {
                moving.remove(url);
            }
            drop(moving);
            self.take_decoded();
        }
    }

    /// The chat on screen changed: its images' boxes and thumbnails are
    /// let go, and downloads that have not started are dropped.
    pub fn leave_chat(&self) {
        self.thumbnails.borrow_mut().clear();
        self.boxes.borrow_mut().clear();
        self.asked.borrow_mut().clear();
        self.stop_animations();
        self.engine.cancel_media();
    }

    // ----- stickers and GIFs that move ----------------------------------

    /// Lets every decoded animation go: its frames leave memory now and
    /// the window's atlas at the next [`release`](Self::release).
    pub fn stop_animations(&self) {
        let mut released = self.released.borrow_mut();
        for (_, moving) in self.moving.borrow_mut().drain() {
            if let Moving::Ready { animation, .. } = moving {
                released.push(animation.image.clone());
            }
        }
        self.decoded.lock().expect("decoded lock").clear();
        self.wake.set(None);
        self.wake_serial.set(self.wake_serial.get() + 1);
    }

    /// Frees the frames the window holds of animations that were let go.
    pub fn release(&self, window: &mut gpui_kit::Window) {
        for image in self.released.borrow_mut().drain(..) {
            let _ = window.drop_image(image);
        }
    }

    /// Whether an attachment can be an image that moves: a sticker, or a
    /// picture that says it is a GIF or a WebP.
    pub fn may_move(media: &Media) -> bool {
        media.kind == MediaKind::Sticker
            || (media.kind == MediaKind::Image
                && matches!(media.mime_type.as_deref(), Some("image/gif" | "image/webp")))
    }

    /// The animation of a sticker or GIF whose first frame is already
    /// shown, once it is known to move and its frames are decoded. Asking
    /// is what gets that done; until then (and for what does not move)
    /// the answer is `None` and the first frame stays.
    pub fn animation(&self, account: &AccountId, media: &Media) -> Option<Rc<Animation>> {
        if !Self::may_move(media) {
            return None;
        }
        let url = media.source.as_ref()?.as_str();
        match self.moving.borrow().get(url) {
            Some(Moving::Ready { animation, .. }) => return Some(animation.clone()),
            Some(_) => return None,
            None => {}
        }
        let cached = self
            .store
            .media(&animation_key(url), Timestamp::now())
            .ok()
            .flatten();
        let state = match cached {
            // From before animations were kept: asked for by itself,
            // where pictures are fetched by themselves at all.
            None if self.fetched_unasked(media) || self.asked.borrow().contains(url) => {
                self.engine.want_animation(account, url);
                Moving::Wanted
            }
            None => Moving::Still,
            Some(cached) if cached.bytes.is_empty() => Moving::Still,
            Some(cached) => {
                self.decode(url, cached.bytes);
                Moving::Decoding
            }
        };
        self.moving.borrow_mut().insert(url.to_owned(), state);
        None
    }

    /// Decodes an animated file on the blocking pool; the store's change
    /// notification is what brings the result back to the views.
    fn decode(&self, url: &str, bytes: Vec<u8>) {
        let (inbox, store, url) = (self.decoded.clone(), self.store.clone(), url.to_owned());
        let runtime = self.engine.runtime().clone();
        let work = runtime.spawn_blocking(move || {
            client_core::animation_frames(&bytes, &AnimationLimits::default())
                .ok()
                .flatten()
                .map(Animation::new)
        });
        // Awaited on the runtime, so the change is announced from the
        // runtime's own task and wakes the views properly.
        runtime.spawn(async move {
            let animation = work.await.ok().flatten();
            inbox
                .lock()
                .expect("decoded lock")
                .push((url.clone(), animation));
            store.notify(StoreChange::Media {
                key: animation_key(&url),
            });
        });
    }

    /// Takes in what has been decoded, within the memory the animations
    /// of one chat may use: one that is not being looked at makes room,
    /// and when all of them are, the newcomer stays a still picture.
    fn take_decoded(&self) {
        let arrived: Vec<_> = self
            .decoded
            .lock()
            .expect("decoded lock")
            .drain(..)
            .collect();
        if arrived.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut moving = self.moving.borrow_mut();
        for (url, animation) in arrived {
            // The chat was left while it was decoding.
            if !matches!(moving.get(&url), Some(Moving::Decoding)) {
                continue;
            }
            let Some(animation) = animation else {
                moving.insert(url, Moving::Still);
                continue;
            };
            let held = |moving: &HashMap<String, Moving>| -> usize {
                moving
                    .values()
                    .map(|state| match state {
                        Moving::Ready { animation, .. } => animation.bytes,
                        _ => 0,
                    })
                    .sum()
            };
            while held(&moving) + animation.bytes > self.animation_budget.get() {
                // The one painted longest ago, if it is off screen.
                let idle = moving
                    .iter()
                    .filter_map(|(url, state)| match state {
                        Moving::Ready { painted, .. } => {
                            let last = painted.map(|(at, _)| at);
                            last.is_none_or(|at| now.saturating_duration_since(at) >= IDLE)
                                .then(|| (last, url.clone()))
                        }
                        _ => None,
                    })
                    .min_by_key(|(last, _)| *last);
                let Some((_, idle)) = idle else { break };
                // Forgotten, not marked still: drawn again, it is decoded
                // again.
                if let Some(Moving::Ready { animation, .. }) = moving.remove(&idle) {
                    self.released.borrow_mut().push(animation.image.clone());
                }
            }
            let state = if held(&moving) + animation.bytes > self.animation_budget.get() {
                Moving::Still
            } else {
                Moving::Ready {
                    animation: Rc::new(animation),
                    playback: Playback::default(),
                    painted: None,
                }
            };
            moving.insert(url, state);
        }
    }

    /// What to paint of the animation at `url` at `now`, as the motion
    /// around it allows. Called while its row is painted, which is what
    /// keeps it playing.
    pub fn play(&self, url: &str, now: Instant, motion: Motion) -> Option<Playing> {
        let mut moving = self.moving.borrow_mut();
        let Some(Moving::Ready {
            animation,
            playback,
            painted,
        }) = moving.get_mut(url)
        else {
            return None;
        };
        let (frame, next) = animation.frame_at(playback.elapsed(now, motion));
        *painted = Some((Instant::now(), frame));
        Some(Playing {
            image: animation.image.clone(),
            frame,
            next: (motion == Motion::Running && next != Duration::MAX).then_some(next),
        })
    }

    /// Asks for one repaint of `view` when the frame on screen is due to
    /// change. Every animated row that is painted asks; the earliest
    /// request is the one that happens, and the repaint it causes is what
    /// asks for the next.
    pub fn wake_in<V: 'static>(
        self: &Rc<Self>,
        delay: Duration,
        view: gpui_kit::WeakEntity<V>,
        cx: &mut gpui_kit::App,
    ) {
        let clock = cx.background_executor().clone();
        let due = clock.now() + delay.max(MIN_TICK);
        if self.wake.get().is_some_and(|pending| pending <= due) {
            return;
        }
        self.wake.set(Some(due));
        let serial = self.wake_serial.get() + 1;
        self.wake_serial.set(serial);
        let shelf = self.clone();
        cx.spawn(async move |cx| {
            clock
                .timer(due.saturating_duration_since(clock.now()))
                .await;
            // Overtaken by an earlier request, or the chat was left.
            if shelf.wake_serial.get() != serial {
                return;
            }
            shelf.wake.set(None);
            view.update(cx, |_, cx| cx.notify()).ok();
        })
        .detach();
    }

    /// Sets the memory the decoded animations may take together.
    #[cfg(test)]
    pub fn set_animation_budget(&self, bytes: usize) {
        self.animation_budget.set(bytes);
    }

    /// When the next repaint for an animation is due, if one is asked for.
    #[cfg(test)]
    pub fn pending_wake(&self) -> Option<Instant> {
        self.wake.get()
    }

    /// The frame of the animation at `url` that was painted last.
    #[cfg(test)]
    pub fn painted_frame(&self, url: &str) -> Option<usize> {
        match self.moving.borrow().get(url) {
            Some(Moving::Ready { painted, .. }) => painted.map(|(_, frame)| frame),
            _ => None,
        }
    }

    /// The memory the decoded animations take, and how many there are.
    #[cfg(test)]
    pub fn animations(&self) -> (usize, usize) {
        let moving = self.moving.borrow();
        let ready = moving.values().filter_map(|state| match state {
            Moving::Ready { animation, .. } => Some(animation.bytes),
            _ => None,
        });
        (ready.clone().sum(), ready.count())
    }

    fn thumbnail(&self, url: &str) -> Option<Arc<Image>> {
        if let Some(known) = self.thumbnails.borrow().get(url) {
            return known.clone();
        }
        let image = self
            .store
            .media(&thumbnail_key(url), Timestamp::now())
            .ok()
            .flatten()
            .map(|cached| own_image(cached.bytes));
        self.thumbnails
            .borrow_mut()
            .insert(url.to_owned(), image.clone());
        image
    }

    /// The interface size changed: the boxes are fitted again.
    pub fn forget_boxes(&self) {
        self.boxes.borrow_mut().clear();
    }

    /// What to draw for an image or a sticker, fetching it if the policy
    /// (or an earlier click) says so.
    pub fn visual(&self, account: &AccountId, media: &Media) -> MediaVisual {
        let Some(source) = &media.source else {
            return MediaVisual::NoFile;
        };
        // A sticker that is not a picture: WhatsApp's vector stickers
        // (Lottie, `application/was`). Nothing here draws them, and
        // downloading one would only be refused as "not an image".
        if media.kind == MediaKind::Sticker
            && media
                .mime_type
                .as_deref()
                .is_some_and(|mime| !mime.trim().to_ascii_lowercase().starts_with("image/"))
        {
            return MediaVisual::Unsupported(
                "An animated sticker of a kind this app cannot show".into(),
            );
        }
        let url = source.as_str();
        if let Some(image) = self.thumbnail(url) {
            return MediaVisual::Image(image);
        }
        let key = thumbnail_key(url);
        let asked = self.asked.borrow().contains(url);
        match self.engine.media_state(&key) {
            MediaState::Expired(reason) => MediaVisual::Unavailable(reason.into()),
            MediaState::Unavailable(reason) => MediaVisual::Failed(reason.into()),
            MediaState::Loading => MediaVisual::Loading {
                fraction: self.fraction(&key, media),
            },
            MediaState::Idle if asked => {
                self.engine.want_thumbnail_asked(account, url);
                MediaVisual::Loading { fraction: None }
            }
            MediaState::Idle if self.fetched_unasked(media) => {
                self.engine.want_thumbnail(account, url);
                MediaVisual::Loading { fraction: None }
            }
            MediaState::Idle => MediaVisual::Download {
                size: media.size_bytes,
            },
        }
    }

    /// Whether a picture is fetched without being asked: the policy says
    /// so, it is not known to be over the limit for that, and the user
    /// has not stopped it.
    fn fetched_unasked(&self, media: &Media) -> bool {
        let declined = media
            .source
            .as_ref()
            .is_some_and(|source| self.declined.borrow().contains(source.as_str()));
        self.policy.get() != MediaChoice::Never
            && !declined
            && media
                .size_bytes
                .is_none_or(|size| size <= client_core::AUTO_MEDIA_LIMIT)
    }

    /// How much of the download under `key` has arrived, when the size
    /// of the file is known.
    fn fraction(&self, key: &str, media: &Media) -> Option<f32> {
        let total = media.size_bytes.filter(|size| *size > 0)?;
        let received = self.engine.media_progress(key).unwrap_or(0);
        Some((received as f32 / total as f32).clamp(0., 1.))
    }

    /// How much of the whole file at `url` has arrived.
    pub fn file_fraction(&self, url: &str, media: &Media) -> Option<f32> {
        self.fraction(&file_key(url), media)
    }

    /// The user stopped a picture that was loading: it is not fetched
    /// again until they ask.
    pub fn cancel(&self, url: &str) {
        self.asked.borrow_mut().remove(url);
        self.declined.borrow_mut().insert(url.to_owned());
        self.engine.cancel_media_key(&thumbnail_key(url));
    }

    /// The user stopped a file that was downloading.
    pub fn cancel_file(&self, url: &str) {
        self.engine.cancel_media_key(&file_key(url));
    }

    /// Whether the whole file at `url` is gone for good.
    pub fn file_expired(&self, url: &str) -> bool {
        matches!(
            self.engine.media_state(&file_key(url)),
            MediaState::Expired(_)
        )
    }

    /// The user clicked an image that is not loaded by itself.
    pub fn ask(&self, account: &AccountId, url: &str) {
        self.asked.borrow_mut().insert(url.to_owned());
        self.declined.borrow_mut().remove(url);
        // A failure before this click is not held against it.
        self.engine.retry_media(&thumbnail_key(url));
        self.engine.want_thumbnail_asked(account, url);
    }

    /// The box an image is drawn in: its own proportions when its
    /// thumbnail was already there the first time the row was drawn, the
    /// placeholder's otherwise, and the same from then on.
    pub fn image_box(&self, media: &Media) -> Size<Pixels> {
        if media.kind == MediaKind::Sticker {
            return Size {
                width: STICKER(),
                height: STICKER(),
            };
        }
        let Some(url) = media.source.as_ref().map(|source| source.as_str()) else {
            return PLACEHOLDER();
        };
        if let Some(known) = self.boxes.borrow().get(url) {
            return *known;
        }
        let pixels = match (media.width, media.height) {
            (Some(width), Some(height)) => Some((width, height)),
            _ => self.store.media_size(&thumbnail_key(url)).ok().flatten(),
        };
        let fitted = match pixels {
            Some((width, height)) if width > 0 && height > 0 => {
                let (width, height) = (width as f32, height as f32);
                let scale = (MAX_IMAGE().width.as_f32() / width)
                    .min(MAX_IMAGE().height.as_f32() / height)
                    .min(1.);
                Size {
                    width: gpui_kit::px((width * scale).max(media_px(72.).as_f32())),
                    height: gpui_kit::px((height * scale).max(media_px(48.).as_f32())),
                }
            }
            _ => PLACEHOLDER(),
        };
        self.boxes.borrow_mut().insert(url.to_owned(), fitted);
        fitted
    }

    /// The length and waveform of the audio at `url`, known once it has
    /// been decoded (now or in an earlier session).
    pub fn shape(&self, url: &str) -> Option<Shape> {
        if let Some(known) = self.shapes.borrow().get(url) {
            return known.clone();
        }
        let shape = self
            .store
            .media(&shape_key(url), Timestamp::now())
            .ok()
            .flatten()
            .and_then(|cached| Shape::from_bytes(&cached.bytes));
        self.shapes
            .borrow_mut()
            .insert(url.to_owned(), shape.clone());
        shape
    }

    /// Keeps the shape of a clip that was just decoded, with the media it
    /// belongs to.
    pub fn keep_shape(&self, url: &str, shape: Shape) {
        let cached = client_core::CachedMedia {
            bytes: shape.to_bytes(),
            mime: None,
            size: None,
        };
        if let Err(error) =
            self.store
                .put_media(&shape_key(url), &cached, Timestamp::now(), u64::MAX)
        {
            tracing::warn!(%error, "could not keep an audio's waveform");
        }
        self.shapes.borrow_mut().insert(url.to_owned(), Some(shape));
    }

    /// The bytes of a downloaded file, to decode.
    pub fn file(&self, url: &str) -> Option<(Vec<u8>, Option<String>)> {
        self.store
            .media(&file_key(url), Timestamp::now())
            .ok()
            .flatten()
            .map(|cached| (cached.bytes, cached.mime))
    }

    /// Lets a download that failed be tried again.
    pub fn retry_file(&self, account: &AccountId, url: &str) {
        self.engine.retry_media(&file_key(url));
        self.engine.want_file(account, url);
    }

    /// Fetches a whole file ahead of a click ("Everything").
    pub fn prefetch(&self, account: &AccountId, url: &str) {
        self.engine.want_file(account, url);
    }

    /// Whether files other than images are fetched without being asked.
    pub fn fetches_everything(&self) -> bool {
        self.policy.get() == MediaChoice::Everything
    }

    /// Where the whole file at `url` stands.
    pub fn file_state(&self, url: &str) -> FileState {
        let key = file_key(url);
        if matches!(self.store.media_size(&key), Ok(Some(_)))
            || self
                .store
                .media(&key, Timestamp::now())
                .is_ok_and(|cached| cached.is_some())
        {
            return FileState::Ready;
        }
        match self.engine.media_state(&key) {
            MediaState::Loading => FileState::Loading,
            MediaState::Unavailable(reason) | MediaState::Expired(reason) => {
                FileState::Unavailable(reason.into())
            }
            MediaState::Idle => FileState::NotFetched,
        }
    }

    /// Writes a downloaded file where the operating system's applications
    /// can open it, and returns the path. This is the one place data
    /// leaves the encrypted store, and it happens only because the user
    /// asked to open that file: a private directory under the system's
    /// temporary one, emptied by [`clear_exports`].
    pub fn export(&self, url: &str, media: &Media) -> Result<PathBuf, String> {
        let cached = self
            .store
            .media(&file_key(url), Timestamp::now())
            .map_err(|error| error.to_string())?
            .ok_or("The file has not been downloaded.")?;
        let name = export_name(media, cached.mime.as_deref());
        let dir = exports_dir().join(format!("{:016x}", fingerprint(url)));
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let private = std::fs::Permissions::from_mode(0o700);
            let _ = std::fs::set_permissions(exports_dir(), private.clone());
            let _ = std::fs::set_permissions(&dir, private);
        }
        let path = dir.join(name);
        std::fs::write(&path, cached.bytes).map_err(|error| error.to_string())?;
        Ok(path)
    }
}

/// Where the whole file of an attachment stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileState {
    NotFetched,
    Loading,
    Ready,
    Unavailable(SharedString),
}

/// The cache key of the length and waveform of the audio at `url`.
fn shape_key(url: &str) -> String {
    format!("wave:{url}")
}

fn fingerprint(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

/// Files opened with other applications are written here.
pub fn exports_dir() -> PathBuf {
    std::env::temp_dir().join(format!("{}-opened", crate::product::SLUG))
}

/// Removes the files that were exported to be opened. Called at start and
/// on sign-out.
pub fn clear_exports() {
    let _ = std::fs::remove_dir_all(exports_dir());
}

/// A file name that is only a file name: what the sender called the file,
/// without anything that could place it elsewhere, or a name made from its
/// kind and type.
pub fn export_name(media: &Media, mime: Option<&str>) -> String {
    let given = media
        .file_name
        .as_deref()
        .map(|name| name.rsplit(['/', '\\']).next().unwrap_or(name))
        .map(|name| {
            name.chars()
                .filter(|c| !c.is_control())
                .collect::<String>()
                .trim_matches(['.', ' '])
                .to_owned()
        })
        .filter(|name| !name.is_empty());
    if let Some(name) = given {
        return name;
    }
    let extension = match mime.or(media.mime_type.as_deref()).unwrap_or("") {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        "image/gif" => "gif",
        "video/mp4" => "mp4",
        "audio/ogg" | "audio/ogg; codecs=opus" => "ogg",
        "audio/mpeg" => "mp3",
        "audio/mp4" => "m4a",
        "application/pdf" => "pdf",
        _ => "bin",
    };
    let stem = match media.kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Audio => "audio",
        MediaKind::Voice => "voice-note",
        MediaKind::Document => "document",
        MediaKind::Sticker => "sticker",
    };
    format!("{stem}.{extension}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exported_names_cannot_leave_their_directory() {
        let named = |name: &str| {
            let mut media = Media::new(MediaKind::Document);
            media.file_name = Some(name.to_owned());
            export_name(&media, Some("application/pdf"))
        };
        assert_eq!(named("contract.pdf"), "contract.pdf");
        assert_eq!(named("../../.ssh/authorized_keys"), "authorized_keys");
        assert_eq!(named("C:\\Users\\x\\run.bat"), "run.bat");
        assert_eq!(named(".."), "document.pdf");
        assert_eq!(named("a\nb.txt"), "ab.txt");
        // No name: one made from what it is.
        let voice = Media::new(MediaKind::Voice);
        assert_eq!(export_name(&voice, Some("audio/ogg")), "voice-note.ogg");
        assert_eq!(
            export_name(&Media::new(MediaKind::Video), None),
            "video.bin"
        );
    }
}
