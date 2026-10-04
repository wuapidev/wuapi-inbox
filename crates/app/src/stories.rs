//! Stories (status): what is decided without drawing anything.
//!
//! * how long each kind of story stays on screen,
//! * the viewer's state machine: [`Player`], with its progress segments,
//!   pause, previous and next story and author, and the one moment a story
//!   counts as seen,
//! * the ring around a picture: [`ring_segments`],
//! * the fonts WhatsApp numbers, mapped to what this application can draw,
//! * the words a status is described in.
//!
//! The views read and drive these; none of it knows about GPUI, so it is
//! tested with a clock that is only ever told how much time passed.

use client_core::{StoryAuthor, StoryItem};
use client_provider::{MediaKind, MessageId, StoryBody, StoryFont, Timestamp};
use std::time::Duration;

/// How long a text or a picture stays up.
pub const STILL_STORY: Duration = Duration::from_secs(5);
/// A video or voice note whose length is not known.
pub const UNKNOWN_LENGTH: Duration = Duration::from_secs(10);
/// The longest a story is held for: WhatsApp's videos are at most this.
pub const LONGEST_STORY: Duration = Duration::from_secs(30);
/// A press on "previous" within this long of a story's start goes to the
/// story before it; later, it starts the story again.
pub const RESTART_WITHIN: Duration = Duration::from_millis(1500);
/// A story posted this long before it expires is "about to expire".
pub const EXPIRING_SOON: Duration = Duration::from_secs(60 * 60);

/// What a story shows, as the viewer cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlideKind {
    /// Words on a colour.
    Text,
    /// A picture.
    Image,
    /// A video: shown as a tile (there is no decoder here).
    Video,
    /// A voice note.
    Voice,
}

impl SlideKind {
    /// The kind a story's body is.
    pub fn of(body: &StoryBody) -> Self {
        match body {
            StoryBody::Text { .. } => Self::Text,
            StoryBody::Media(media) => match media.kind {
                MediaKind::Video => Self::Video,
                MediaKind::Audio | MediaKind::Voice => Self::Voice,
                _ => Self::Image,
            },
        }
    }

    /// Whether the story counts as seen when it is drawn. A video is only
    /// a tile here, and a voice note a play button: the person has not
    /// seen or heard them until they are opened in a player or played.
    pub fn seen_when_drawn(self) -> bool {
        !matches!(self, Self::Video | Self::Voice)
    }
}

/// How long a story stays on screen: text and pictures about five
/// seconds, a video or a voice note as long as it is (at most
/// [`LONGEST_STORY`]; [`UNKNOWN_LENGTH`] when nobody says).
pub fn duration_of(body: &StoryBody) -> Duration {
    match SlideKind::of(body) {
        SlideKind::Text | SlideKind::Image => STILL_STORY,
        SlideKind::Video | SlideKind::Voice => match body {
            StoryBody::Media(media) => media
                .duration_secs
                .filter(|seconds| *seconds > 0)
                .map(|seconds| Duration::from_secs(u64::from(seconds)).min(LONGEST_STORY))
                .unwrap_or(UNKNOWN_LENGTH),
            StoryBody::Text { .. } => STILL_STORY,
        },
    }
}

/// One story of the deck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Slide {
    /// The story.
    pub id: MessageId,
    /// What it shows.
    pub kind: SlideKind,
    /// How long it stays.
    pub duration: Duration,
}

impl Slide {
    /// The slide of a story.
    pub fn of(item: &StoryItem) -> Self {
        Self {
            id: item.story.id.clone(),
            kind: SlideKind::of(&item.story.body),
            duration: duration_of(&item.story.body),
        }
    }
}

/// One author's stories, in the order they play.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reel {
    /// What stands for the author (see `StoryAuthor::key`).
    pub key: String,
    /// Their slides, oldest first.
    pub slides: Vec<Slide>,
}

impl Reel {
    /// The reel of an author.
    pub fn of(author: &StoryAuthor) -> Self {
        Self {
            key: author.key.clone(),
            slides: author.stories.iter().map(Slide::of).collect(),
        }
    }
}

/// Something that happened that the window must act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Moment {
    /// The story was actually shown (or opened in a player): it counts as
    /// seen, and a receipt may be owed. Once per story per visit.
    Shown(MessageId),
    /// The viewer moved to another story.
    Moved,
    /// There is nothing after the last story: the viewer closes.
    Ended,
}

/// Why the clock is not running.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Holds {
    /// The pointer is held on the story.
    pub pointer: bool,
    /// Space (or the pause button).
    pub paused: bool,
    /// The reply field has the keyboard.
    pub typing: bool,
    /// A video was opened in the system's player.
    pub external: bool,
    /// A panel (who saw it, the menu) is open over the story.
    pub panel: bool,
}

impl Holds {
    /// Nothing holds the clock.
    pub fn none(&self) -> bool {
        !(self.pointer || self.paused || self.typing || self.external || self.panel)
    }
}

/// The viewer's state machine.
///
/// Time only passes in [`tick`](Self::tick), only while the story's
/// content is on screen (`ready`) and nothing [`holds`](Self::holds) it.
/// A story counts as seen the first time its content is on screen, never
/// when it is listed, prefetched or downloaded.
#[derive(Clone, Debug)]
pub struct Player {
    reels: Vec<Reel>,
    reel: usize,
    slide: usize,
    elapsed: Duration,
    ready: bool,
    /// What holds the clock.
    pub holds: Holds,
    /// The stories reported as shown during this visit.
    shown: Vec<MessageId>,
}

impl Player {
    /// A player over `reels`, at `slide` of `reel`.
    pub fn new(reels: Vec<Reel>, reel: usize, slide: usize) -> Self {
        let reel = reel.min(reels.len().saturating_sub(1));
        let slide = slide.min(
            reels
                .get(reel)
                .map_or(0, |reel| reel.slides.len().saturating_sub(1)),
        );
        Self {
            reels,
            reel,
            slide,
            elapsed: Duration::ZERO,
            ready: false,
            holds: Holds::default(),
            shown: Vec::new(),
        }
    }

    /// The reels.
    pub fn reels(&self) -> &[Reel] {
        &self.reels
    }

    /// The reel that is playing.
    pub fn reel_index(&self) -> usize {
        self.reel
    }

    /// The story that is playing, as its place in its reel.
    #[cfg(test)]
    pub fn slide_index(&self) -> usize {
        self.slide
    }

    /// The reel that is playing.
    pub fn reel(&self) -> Option<&Reel> {
        self.reels.get(self.reel)
    }

    /// The story that is on screen.
    pub fn current(&self) -> Option<&Slide> {
        self.reel()?.slides.get(self.slide)
    }

    /// How long the story has been on screen.
    #[cfg(test)]
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// Whether the story's content is on screen.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Whether the clock is running.
    pub fn running(&self) -> bool {
        self.ready && self.holds.none() && self.current().is_some()
    }

    /// How much of segment `index` of the playing reel is filled: whole
    /// for the stories before the one playing, none after, and the elapsed
    /// share of the one playing.
    pub fn progress(&self, index: usize) -> f32 {
        match index.cmp(&self.slide) {
            std::cmp::Ordering::Less => 1.,
            std::cmp::Ordering::Greater => 0.,
            std::cmp::Ordering::Equal => self.current().map_or(0., |slide| {
                (self.elapsed.as_secs_f32() / slide.duration.as_secs_f32().max(0.001)).min(1.)
            }),
        }
    }

    /// The content of the story is on screen (or is not: it went, or it
    /// is loading). The first time it is, the story counts as seen, if
    /// its kind does so when it is drawn.
    pub fn set_ready(&mut self, ready: bool) -> Vec<Moment> {
        self.ready = ready;
        let mut moments = Vec::new();
        if ready {
            if let Some(slide) = self.current().cloned() {
                if slide.kind.seen_when_drawn() {
                    self.note_shown(&slide.id, &mut moments);
                }
            }
        }
        moments
    }

    /// The story was opened in the system's player: the person is
    /// watching it, so it counts as seen, and the clock waits for them.
    pub fn opened_externally(&mut self) -> Vec<Moment> {
        self.holds.external = true;
        let mut moments = Vec::new();
        if let Some(slide) = self.current().cloned() {
            self.note_shown(&slide.id, &mut moments);
        }
        moments
    }

    fn note_shown(&mut self, id: &MessageId, moments: &mut Vec<Moment>) {
        if !self.shown.contains(id) {
            self.shown.push(id.clone());
            moments.push(Moment::Shown(id.clone()));
        }
    }

    /// Lets `by` pass. When the story's time is up, the next one starts
    /// (or the viewer ends).
    pub fn tick(&mut self, by: Duration) -> Vec<Moment> {
        if !self.running() {
            return Vec::new();
        }
        self.elapsed += by;
        let Some(slide) = self.current() else {
            return vec![Moment::Ended];
        };
        if self.elapsed >= slide.duration {
            return self.next_story();
        }
        Vec::new()
    }

    fn start(&mut self, reel: usize, slide: usize) -> Vec<Moment> {
        self.reel = reel;
        self.slide = slide;
        self.elapsed = Duration::ZERO;
        // The new story's content is not on screen yet.
        self.ready = false;
        self.holds.external = false;
        vec![Moment::Moved]
    }

    /// The next story of the author; after the last, the next author's
    /// first; after the last of all, the viewer ends.
    pub fn next_story(&mut self) -> Vec<Moment> {
        let Some(reel) = self.reel() else {
            return vec![Moment::Ended];
        };
        if self.slide + 1 < reel.slides.len() {
            return self.start(self.reel, self.slide + 1);
        }
        self.next_author_or_end()
    }

    fn next_author_or_end(&mut self) -> Vec<Moment> {
        match (self.reel + 1..self.reels.len()).find(|at| !self.reels[*at].slides.is_empty()) {
            Some(at) => self.start(at, 0),
            None => vec![Moment::Ended],
        }
    }

    /// The story before: a story that has played a moment starts again
    /// first; at the first story of an author, the previous author's last.
    pub fn previous_story(&mut self) -> Vec<Moment> {
        if self.elapsed > RESTART_WITHIN {
            return self.start(self.reel, self.slide);
        }
        if self.slide > 0 {
            return self.start(self.reel, self.slide - 1);
        }
        match (0..self.reel)
            .rev()
            .find(|at| !self.reels[*at].slides.is_empty())
        {
            Some(at) => {
                let last = self.reels[at].slides.len() - 1;
                self.start(at, last)
            }
            None => self.start(self.reel, 0),
        }
    }

    /// The next author's first story; after the last author, the viewer
    /// ends.
    pub fn next_author(&mut self) -> Vec<Moment> {
        self.next_author_or_end()
    }

    /// The previous author's first story; at the first, this one again.
    pub fn previous_author(&mut self) -> Vec<Moment> {
        match (0..self.reel)
            .rev()
            .find(|at| !self.reels[*at].slides.is_empty())
        {
            Some(at) => self.start(at, 0),
            None => self.start(self.reel, 0),
        }
    }

    /// A story went (it expired, or was taken down) while it was on
    /// screen or before it: the deck forgets it and the viewer lands on
    /// the story that took its place.
    pub fn forget(&mut self, id: &MessageId) -> Vec<Moment> {
        let on_screen = self.current().is_some_and(|slide| &slide.id == id);
        let mut moments = Vec::new();
        for (r, reel) in self.reels.iter_mut().enumerate() {
            if let Some(at) = reel.slides.iter().position(|slide| &slide.id == id) {
                reel.slides.remove(at);
                if r == self.reel && at < self.slide {
                    self.slide -= 1;
                }
            }
        }
        if on_screen {
            let reel = self.reel;
            if self
                .reels
                .get(reel)
                .is_some_and(|reel| reel.slides.is_empty())
            {
                return self.next_author_or_end_from(reel);
            }
            let slide = self
                .slide
                .min(self.reels[reel].slides.len().saturating_sub(1));
            moments.extend(self.start(reel, slide));
        }
        moments
    }

    fn next_author_or_end_from(&mut self, from: usize) -> Vec<Moment> {
        match (from + 1..self.reels.len()).find(|at| !self.reels[*at].slides.is_empty()) {
            Some(at) => self.start(at, 0),
            None => match (0..from)
                .rev()
                .find(|at| !self.reels[*at].slides.is_empty())
            {
                Some(at) => {
                    let last = self.reels[at].slides.len() - 1;
                    self.start(at, last)
                }
                None => vec![Moment::Ended],
            },
        }
    }
}

// ----- the ring ---------------------------------------------------------

/// One piece of the ring around a picture, in turns (0 to 1) from the top,
/// clockwise.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RingSegment {
    /// Where it starts.
    pub from: f32,
    /// Where it ends.
    pub to: f32,
    /// The story it stands for has been seen.
    pub viewed: bool,
}

/// The ring of somebody with stories: one piece per story, oldest first
/// from the top, with a gap between pieces (none for a single story). The
/// ones seen are marked, so the ring says how many there are and which
/// are new.
pub fn ring_segments(viewed: &[bool]) -> Vec<RingSegment> {
    let count = viewed.len();
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![RingSegment {
            from: 0.,
            to: 1.,
            viewed: viewed[0],
        }];
    }
    // The gap shrinks as the pieces get many, so each stays visible.
    let gap = (0.04_f32).min(0.5 / count as f32);
    let each = 1. / count as f32;
    viewed
        .iter()
        .enumerate()
        .map(|(index, viewed)| RingSegment {
            from: index as f32 * each + gap / 2.,
            to: (index + 1) as f32 * each - gap / 2.,
            viewed: *viewed,
        })
        .collect()
}

// ----- fonts --------------------------------------------------------------

/// How a text story is set, in what this application can draw. WhatsApp
/// numbers eight fonts of its own; none is bundled here, so each is
/// mapped to the nearest of Inter (regular, italic, bold, black),
/// JetBrains Mono and the system's serif, and the mapping is part of the
/// post composer, so a story reads the same on the way out as it does
/// coming in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Face {
    /// What the font is called in the composer.
    pub name: &'static str,
    /// The family to ask for.
    pub family: Family,
    /// Heavier than regular.
    pub bold: bool,
    /// Slanted.
    pub italic: bool,
    /// Letters in capitals.
    pub caps: bool,
}

/// A family of type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    /// Inter, the interface's.
    Sans,
    /// JetBrains Mono.
    Mono,
    /// The system's serif.
    Serif,
}

/// The face for one of WhatsApp's font numbers: 0 sans, 1 serif, 2
/// script (slanted sans), 6 bold, 7 condensed capitals (bold capitals), 8
/// typewriter (mono), 9 rounded (bold slanted), 10 display (serif bold).
pub fn face_of(font: StoryFont) -> Face {
    let (name, family, bold, italic, caps) = match font.0 {
        1 => ("Serif", Family::Serif, false, false, false),
        2 => ("Script", Family::Sans, false, true, false),
        6 => ("Bold", Family::Sans, true, false, false),
        7 => ("Capitals", Family::Sans, true, false, true),
        8 => ("Typewriter", Family::Mono, false, false, false),
        9 => ("Rounded", Family::Sans, true, true, false),
        10 => ("Display", Family::Serif, true, false, false),
        _ => ("Sans", Family::Sans, false, false, false),
    };
    Face {
        name,
        family,
        bold,
        italic,
        caps,
    }
}

/// The text size for a story's words: larger for few, so a line is a
/// headline and a paragraph still fits. Returns design pixels.
pub fn text_size(words: &str) -> f32 {
    let length = words.chars().count();
    match length {
        0..=24 => 40.,
        25..=60 => 32.,
        61..=140 => 26.,
        141..=300 => 21.,
        _ => 17.,
    }
}

// ----- what is typed ------------------------------------------------------------

/// Writes the emoji for each `:shortcode:` in `text` that names one
/// exactly (`:fire:`, `:thumbsup:`), as the composer's picker does when
/// one is chosen. A colon pair around anything else (a time, an address) is
/// left as it was typed.
pub fn with_shortcodes(text: &str) -> String {
    if !text.contains(':') {
        return text.to_owned();
    }
    let set = crate::emoji::data::EmojiSet::load();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find(':') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let named = after.find(':').filter(|close| {
            let name = &after[..*close];
            name.chars().count() >= 2
                && name.chars().next().is_some_and(char::is_alphabetic)
                && name
                    .chars()
                    .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '+' | '-'))
        });
        let emoji = named.and_then(|close| {
            let name = &after[..close];
            set.emojis
                .iter()
                .find(|emoji| emoji.shortcodes.iter().any(|code| &**code == name))
                .map(|emoji| (close, emoji.text.to_string()))
        });
        match emoji {
            Some((close, text)) => {
                out.push_str(&text);
                rest = &after[close + 1..];
            }
            None => {
                out.push(':');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ----- words ----------------------------------------------------------------

/// What a story is called in a line: "Photo", "Video", "Voice message",
/// or its words.
pub fn describe(body: &StoryBody) -> String {
    match SlideKind::of(body) {
        SlideKind::Text => body.words().unwrap_or("Status").to_owned(),
        SlideKind::Image => body
            .words()
            .map_or("Photo".to_owned(), |c| format!("Photo: {c}")),
        SlideKind::Video => body
            .words()
            .map_or("Video".to_owned(), |c| format!("Video: {c}")),
        SlideKind::Voice => "Voice message".to_owned(),
    }
}

/// How long ago, in the words the Status list uses: "Just now", "12 min
/// ago", "3 h ago", and the clock time past a day.
pub fn age_label(at: Timestamp, now: Timestamp) -> String {
    let minutes = ((now.as_millis() - at.as_millis()).max(0) / 60_000) as u64;
    match minutes {
        0 => "Just now".to_owned(),
        1..=59 => format!("{minutes} min ago"),
        60..=1439 => format!("{} h ago", minutes / 60),
        _ => crate::format::clock(at),
    }
}

/// How long a story has left, for the info line: "Expires in 3 h", "in
/// 10 min", or `None` when it is gone.
pub fn expires_label(expires_at: Timestamp, now: Timestamp) -> Option<String> {
    let minutes = (expires_at.as_millis() - now.as_millis()) / 60_000;
    if expires_at.as_millis() <= now.as_millis() {
        return None;
    }
    Some(match minutes {
        i64::MIN..=0 => "Expires in less than a minute".to_owned(),
        1..=59 => format!("Expires in {minutes} min"),
        _ => format!("Expires in {} h", minutes / 60),
    })
}

/// Whether a story is about to expire.
pub fn expiring_soon(expires_at: Timestamp, now: Timestamp) -> bool {
    let left = expires_at.as_millis() - now.as_millis();
    left > 0 && left <= EXPIRING_SOON.as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_provider::{Media, StoryStyle};

    fn text() -> StoryBody {
        StoryBody::Text {
            text: "hi".into(),
            style: StoryStyle::default(),
        }
    }

    fn media(kind: MediaKind, seconds: Option<u32>) -> StoryBody {
        let mut media = Media::new(kind);
        media.duration_secs = seconds;
        StoryBody::Media(media)
    }

    fn slide(id: &str, kind: SlideKind, secs: u64) -> Slide {
        Slide {
            id: MessageId::new(id),
            kind,
            duration: Duration::from_secs(secs),
        }
    }

    fn reel(key: &str, slides: Vec<Slide>) -> Reel {
        Reel {
            key: key.into(),
            slides,
        }
    }

    fn two_authors() -> Player {
        Player::new(
            vec![
                reel(
                    "a",
                    vec![
                        slide("a1", SlideKind::Text, 5),
                        slide("a2", SlideKind::Image, 5),
                    ],
                ),
                reel("b", vec![slide("b1", SlideKind::Image, 5)]),
            ],
            0,
            0,
        )
    }

    #[test]
    fn a_story_stays_as_long_as_its_kind_says() {
        assert_eq!(duration_of(&text()), Duration::from_secs(5));
        assert_eq!(
            duration_of(&media(MediaKind::Image, None)),
            Duration::from_secs(5)
        );
        assert_eq!(
            duration_of(&media(MediaKind::Video, Some(12))),
            Duration::from_secs(12)
        );
        assert_eq!(
            duration_of(&media(MediaKind::Video, Some(600))),
            LONGEST_STORY
        );
        assert_eq!(duration_of(&media(MediaKind::Video, None)), UNKNOWN_LENGTH);
        assert_eq!(
            duration_of(&media(MediaKind::Voice, Some(8))),
            Duration::from_secs(8)
        );
        assert_eq!(
            duration_of(&media(MediaKind::Voice, Some(0))),
            UNKNOWN_LENGTH
        );
    }

    #[test]
    fn time_passes_only_while_the_story_is_on_screen_and_nothing_holds_it() {
        let mut player = two_authors();
        // Not on screen yet: nothing runs.
        assert!(player.tick(Duration::from_secs(9)).is_empty());
        assert_eq!(player.progress(0), 0.);
        player.set_ready(true);
        player.tick(Duration::from_millis(2500));
        assert!((player.progress(0) - 0.5).abs() < 1e-6);
        // Held by the pointer, paused, typing, a panel: it waits.
        for hold in 0..4 {
            let mut held = player.clone();
            match hold {
                0 => held.holds.pointer = true,
                1 => held.holds.paused = true,
                2 => held.holds.typing = true,
                _ => held.holds.panel = true,
            }
            assert!(!held.running());
            held.tick(Duration::from_secs(60));
            assert!((held.progress(0) - 0.5).abs() < 1e-6, "hold {hold}");
        }
        // Let go: it goes on.
        player.holds = Holds::default();
        player.tick(Duration::from_millis(1000));
        assert!((player.progress(0) - 0.7).abs() < 1e-6);
    }

    #[test]
    fn a_story_that_has_run_its_time_hands_over_to_the_next() {
        let mut player = two_authors();
        player.set_ready(true);
        let moments = player.tick(Duration::from_secs(5));
        assert_eq!(moments, vec![Moment::Moved]);
        assert_eq!((player.reel_index(), player.slide_index()), (0, 1));
        // The new story is not on screen until it says so.
        assert!(!player.running());
        assert_eq!(player.elapsed(), Duration::ZERO);
        assert_eq!(player.progress(0), 1.);
        player.set_ready(true);
        // After the author's last: the next author's first.
        player.tick(Duration::from_secs(5));
        assert_eq!((player.reel_index(), player.slide_index()), (1, 0));
        player.set_ready(true);
        // After the last of all: the viewer ends.
        assert_eq!(player.tick(Duration::from_secs(5)), vec![Moment::Ended]);
    }

    #[test]
    fn a_story_counts_as_seen_when_it_is_shown_and_once() {
        let mut player = two_authors();
        assert!(player.set_ready(false).is_empty());
        assert_eq!(
            player.set_ready(true),
            vec![Moment::Shown(MessageId::new("a1"))]
        );
        // Drawn again (a repaint, the image arriving twice): not again.
        assert!(player.set_ready(true).is_empty());
        // Going away and coming back in the same visit is the same view.
        player.next_story();
        player.previous_story();
        assert!(player.set_ready(true).is_empty());
    }

    #[test]
    fn a_video_counts_as_seen_when_it_is_opened_in_a_player() {
        let mut player = Player::new(
            vec![reel("a", vec![slide("v", SlideKind::Video, 12)])],
            0,
            0,
        );
        // The tile on screen is not a view.
        assert!(player.set_ready(true).is_empty());
        let moments = player.opened_externally();
        assert_eq!(moments, vec![Moment::Shown(MessageId::new("v"))]);
        // And the clock waits for the person.
        assert!(!player.running());
        assert!(player.tick(Duration::from_secs(60)).is_empty());
        player.holds.external = false;
        assert!(player.running());
    }

    #[test]
    fn previous_restarts_a_story_that_has_played_and_goes_back_when_it_has_not() {
        let mut player = two_authors();
        player.set_ready(true);
        player.next_story();
        player.set_ready(true);
        player.tick(Duration::from_secs(3));
        // Played: starts again.
        player.previous_story();
        assert_eq!((player.reel_index(), player.slide_index()), (0, 1));
        assert_eq!(player.elapsed(), Duration::ZERO);
        // Just started: the one before.
        player.previous_story();
        assert_eq!((player.reel_index(), player.slide_index()), (0, 0));
        // The first of all: it starts again, and nothing is before it.
        player.previous_story();
        assert_eq!((player.reel_index(), player.slide_index()), (0, 0));
        // From the first story of the second author: the first author's last.
        let mut second = two_authors();
        second.next_author();
        second.previous_story();
        assert_eq!((second.reel_index(), second.slide_index()), (0, 1));
    }

    #[test]
    fn authors_are_stepped_over_and_the_last_ends_the_viewer() {
        let mut player = two_authors();
        player.set_ready(true);
        player.next_story();
        assert_eq!(player.slide_index(), 1);
        assert_eq!(player.next_author(), vec![Moment::Moved]);
        assert_eq!((player.reel_index(), player.slide_index()), (1, 0));
        assert_eq!(player.next_author(), vec![Moment::Ended]);
        assert_eq!(player.previous_author(), vec![Moment::Moved]);
        assert_eq!((player.reel_index(), player.slide_index()), (0, 0));
        // Nothing before the first: it starts again.
        player.previous_author();
        assert_eq!((player.reel_index(), player.slide_index()), (0, 0));
    }

    #[test]
    fn a_story_that_expires_while_on_screen_is_skipped() {
        let mut player = two_authors();
        player.set_ready(true);
        let moments = player.forget(&MessageId::new("a1"));
        assert_eq!(moments, vec![Moment::Moved]);
        assert_eq!(player.current().unwrap().id, MessageId::new("a2"));
        // One before the one on screen: the place moves, nothing else.
        let mut player = two_authors();
        player.next_story();
        player.forget(&MessageId::new("a1"));
        assert_eq!(player.current().unwrap().id, MessageId::new("a2"));
        // The only story of the last author: back to the one before it.
        let mut player = two_authors();
        player.next_author();
        assert_eq!(player.forget(&MessageId::new("b1")), vec![Moment::Moved]);
        assert_eq!(player.current().unwrap().id, MessageId::new("a2"));
        // Nothing left: it ends.
        let mut alone = Player::new(vec![reel("a", vec![slide("x", SlideKind::Text, 5)])], 0, 0);
        assert_eq!(alone.forget(&MessageId::new("x")), vec![Moment::Ended]);
    }

    #[test]
    fn the_ring_has_a_piece_per_story_and_says_which_are_new() {
        assert!(ring_segments(&[]).is_empty());
        let one = ring_segments(&[false]);
        assert_eq!(one.len(), 1);
        assert_eq!((one[0].from, one[0].to), (0., 1.));
        let three = ring_segments(&[true, true, false]);
        assert_eq!(three.len(), 3);
        assert!(three[0].viewed && three[1].viewed && !three[2].viewed);
        // Pieces do not overlap and leave a gap.
        for pair in three.windows(2) {
            assert!(pair[0].to < pair[1].from);
        }
        assert!(three.last().unwrap().to < 1.);
        // Many stories: every piece is still there, with a visible length.
        let many = ring_segments(&[false; 30]);
        assert_eq!(many.len(), 30);
        assert!(many.iter().all(|piece| piece.to - piece.from > 0.01));
    }

    #[test]
    fn every_whatsapp_font_has_a_face_and_none_is_lost() {
        for font in StoryFont::ALL {
            let face = face_of(font);
            assert!(!face.name.is_empty());
        }
        let names: std::collections::HashSet<_> = StoryFont::ALL
            .iter()
            .map(|font| face_of(*font).name)
            .collect();
        assert_eq!(names.len(), 8, "eight fonts, eight faces");
        assert_eq!(face_of(StoryFont(3)).name, "Sans");
        assert!(text_size("hi") > text_size(&"x".repeat(500)));
    }

    #[test]
    fn a_shortcode_that_names_an_emoji_is_written_as_one_and_nothing_else_is_touched() {
        assert_eq!(with_shortcodes("nice :fire: one"), "nice 🔥 one");
        assert_eq!(with_shortcodes(":thumbsup::fire:"), "👍🔥");
        // Times, addresses, and colons around things that are not emoji.
        assert_eq!(with_shortcodes("at 10:30:15"), "at 10:30:15");
        assert_eq!(
            with_shortcodes("see http://example.com:80/"),
            "see http://example.com:80/"
        );
        assert_eq!(with_shortcodes(":notanemojiatall:"), ":notanemojiatall:");
        assert_eq!(with_shortcodes("a:b"), "a:b");
        assert_eq!(with_shortcodes("no colons"), "no colons");
    }

    #[test]
    fn ages_are_written_the_way_the_list_does() {
        let now = Timestamp::from_millis(10_000_000_000);
        let ago = |minutes: i64| Timestamp::from_millis(now.as_millis() - minutes * 60_000);
        assert_eq!(age_label(ago(0), now), "Just now");
        assert_eq!(age_label(ago(12), now), "12 min ago");
        assert_eq!(age_label(ago(180), now), "3 h ago");
        let left = |minutes: i64| Timestamp::from_millis(now.as_millis() + minutes * 60_000);
        assert_eq!(
            expires_label(left(10), now).as_deref(),
            Some("Expires in 10 min")
        );
        assert_eq!(
            expires_label(left(200), now).as_deref(),
            Some("Expires in 3 h")
        );
        assert_eq!(expires_label(left(-1), now), None);
        assert!(expiring_soon(left(10), now) && !expiring_soon(left(200), now));
    }
}
