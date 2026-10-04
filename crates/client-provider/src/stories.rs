//! Stories (WhatsApp's status): the neutral model.
//!
//! A story is a text, a picture or a short video that its author shows to
//! the people their privacy list names, for [`STORY_LIFETIME`]. Contacts'
//! stories arrive as [`Story`] values (from [`Provider::list_stories`] and
//! [`ProviderEvent::StoryUpserted`]); the account's own are posted with
//! [`Provider::post_story`].
//!
//! [`Provider::list_stories`]: crate::Provider::list_stories
//! [`Provider::post_story`]: crate::Provider::post_story
//! [`ProviderEvent::StoryUpserted`]: crate::ProviderEvent::StoryUpserted

use crate::content::Mention;
use crate::ids::{AccountId, ClientMessageId, ContactId, MediaRef, MessageId};
use crate::model::{Media, MediaKind, Timestamp};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// How long a story lasts after it was posted.
pub const STORY_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// The longest text of a story, in characters.
pub const STORY_TEXT_MAX: usize = 4096;

/// A story's text font, as WhatsApp numbers them. Only these numbers
/// exist on the wire; what each looks like is the client's to draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StoryFont(pub u8);

impl StoryFont {
    /// Every font a story can be posted in, in the order they are offered.
    pub const ALL: [StoryFont; 8] = [
        StoryFont(0),
        StoryFont(1),
        StoryFont(2),
        StoryFont(6),
        StoryFont(7),
        StoryFont(8),
        StoryFont(9),
        StoryFont(10),
    ];

    /// The number is one WhatsApp posts stories in.
    pub fn is_known(self) -> bool {
        Self::ALL.contains(&self)
    }
}

/// How a text story looks: a flat colour behind white text, and a font.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StoryStyle {
    /// The background, `0xRRGGBB`.
    pub background: u32,
    /// The font.
    pub font: StoryFont,
}

impl Default for StoryStyle {
    fn default() -> Self {
        Self {
            background: 0x1F_6F_5C,
            font: StoryFont(0),
        }
    }
}

impl StoryStyle {
    /// The background as `#RRGGBB`, the way an API writes it.
    pub fn background_hex(&self) -> String {
        format!("#{:06X}", self.background & 0xFF_FF_FF)
    }

    /// Reads `#RRGGBB` (or `RRGGBB`).
    pub fn parse_background(hex: &str) -> Option<u32> {
        let digits = hex.strip_prefix('#').unwrap_or(hex);
        (digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(digits, 16).ok())
            .flatten()
    }
}

/// What a story shows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StoryBody {
    /// Words on a coloured background.
    Text {
        /// The words.
        text: String,
        /// The background and the font.
        style: StoryStyle,
    },
    /// A picture, a video or a voice note (by [`Media::kind`]), with its
    /// caption in [`Media::caption`].
    Media(Media),
}

impl StoryBody {
    /// The words a story carries: its text, or its caption.
    pub fn words(&self) -> Option<&str> {
        match self {
            Self::Text { text, .. } => Some(text.as_str()),
            Self::Media(media) => media.caption.as_deref(),
        }
        .filter(|words| !words.trim().is_empty())
    }

    /// The kind of media, `None` for a text.
    pub fn media_kind(&self) -> Option<MediaKind> {
        match self {
            Self::Text { .. } => None,
            Self::Media(media) => Some(media.kind),
        }
    }
}

/// A story.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Story {
    /// The story's id. A story is a message to the status list, so this is
    /// the id replies and reactions quote.
    pub id: MessageId,
    /// The id the account gave it when it posted it from here.
    #[serde(default)]
    pub client_id: Option<ClientMessageId>,
    /// The account that can see it.
    pub account_id: AccountId,
    /// Who posted it.
    pub author: ContactId,
    /// The name it came with, when it did.
    #[serde(default)]
    pub author_name: Option<String>,
    /// The account itself posted it.
    pub mine: bool,
    /// When it was posted.
    pub posted_at: Timestamp,
    /// When it goes. `None`: [`STORY_LIFETIME`] after it was posted.
    #[serde(default)]
    pub expires_at: Option<Timestamp>,
    /// What it shows.
    pub body: StoryBody,
    /// People the caption or text mentions.
    #[serde(default)]
    pub mentions: Vec<Mention>,
    /// The account has seen it, on this or another of its devices.
    #[serde(default)]
    pub viewed: bool,
    /// How many people saw it, for the account's own. `None`: the provider
    /// does not say.
    #[serde(default)]
    pub view_count: Option<u32>,
}

impl Story {
    /// When it goes.
    pub fn expiry(&self) -> Timestamp {
        self.expires_at.unwrap_or_else(|| {
            Timestamp::from_millis(self.posted_at.as_millis() + STORY_LIFETIME.as_millis() as i64)
        })
    }
}

/// Someone who saw one of the account's stories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryViewer {
    /// Who.
    pub contact: ContactId,
    /// Their name as the provider knows it, when it does.
    #[serde(default)]
    pub name: Option<String>,
    /// When they saw it.
    pub viewed_at: Timestamp,
    /// The emoji they reacted with, if they did.
    #[serde(default)]
    pub reaction: Option<String>,
}

/// What to post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NewStoryContent {
    /// Words on a coloured background.
    Text {
        /// The words, not blank, at most [`STORY_TEXT_MAX`] characters.
        text: String,
        /// The background and the font.
        style: StoryStyle,
    },
    /// A picture or a video that was uploaded with
    /// [`Provider::upload_media`](crate::Provider::upload_media).
    Media {
        /// Image or video.
        kind: MediaKind,
        /// What the upload answered.
        media: MediaRef,
        /// The file's type.
        mime_type: Option<String>,
        /// A caption.
        caption: Option<String>,
    },
}

/// A story to post. `client_id` is an idempotency key, as for a message:
/// posting the same one again must not put a second story on WhatsApp.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewStory {
    /// The key.
    pub client_id: ClientMessageId,
    /// The account that posts.
    pub account_id: AccountId,
    /// What to post.
    pub content: NewStoryContent,
}

/// Who a story is shown to, as WhatsApp's three settings have it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryAudience {
    /// Every contact.
    #[default]
    Contacts,
    /// Every contact except the ones listed in [`StoryPrivacy::except`].
    ContactsExcept,
    /// Only the ones listed in [`StoryPrivacy::only`].
    OnlyShareWith,
}

/// The account's story privacy. WhatsApp keeps both lists whichever is in
/// use, so both are carried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryPrivacy {
    /// The setting in use.
    pub audience: StoryAudience,
    /// "My contacts except...".
    pub except: Vec<ContactId>,
    /// "Only share with...".
    pub only: Vec<ContactId>,
}

impl StoryPrivacy {
    /// The people the setting in use names: nobody for "My contacts".
    pub fn listed(&self) -> &[ContactId] {
        match self.audience {
            StoryAudience::Contacts => &[],
            StoryAudience::ContactsExcept => &self.except,
            StoryAudience::OnlyShareWith => &self.only,
        }
    }
}

/// What a message that answers a story says about it. The story itself
/// is gone after a day; this is what stays with the reply.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoryReplyRef {
    /// The story's id.
    pub story: MessageId,
    /// What the story showed.
    pub kind: StoryReplyKind,
    /// Its words (text or caption), cut short.
    #[serde(default)]
    pub preview: Option<String>,
    /// The story was the account's own (somebody answered it), else the
    /// account answered somebody's.
    #[serde(default)]
    pub of_mine: bool,
}

/// What a story that was answered showed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoryReplyKind {
    /// Words.
    Text,
    /// A picture.
    Image,
    /// A video.
    Video,
    /// A voice note.
    Voice,
}

impl StoryReplyKind {
    /// The kind a story's body is.
    pub fn of(body: &StoryBody) -> Self {
        match body.media_kind() {
            None => Self::Text,
            Some(MediaKind::Video) => Self::Video,
            Some(MediaKind::Audio | MediaKind::Voice) => Self::Voice,
            Some(_) => Self::Image,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_are_written_and_read_the_way_apis_do() {
        let style = StoryStyle {
            background: 0x0a_bc_de,
            font: StoryFont(2),
        };
        assert_eq!(style.background_hex(), "#0ABCDE");
        assert_eq!(StoryStyle::parse_background("#0abcde"), Some(0x0abcde));
        assert_eq!(StoryStyle::parse_background("0ABCDE"), Some(0x0abcde));
        assert_eq!(StoryStyle::parse_background("#12345"), None);
        assert_eq!(StoryStyle::parse_background("#12345g"), None);
    }

    #[test]
    fn a_story_lasts_a_day_unless_told_otherwise() {
        let mut story = Story {
            id: MessageId::new("s"),
            client_id: None,
            account_id: AccountId::new("a"),
            author: ContactId::new("c"),
            author_name: None,
            mine: false,
            posted_at: Timestamp::from_millis(1_000),
            expires_at: None,
            body: StoryBody::Text {
                text: "hi".into(),
                style: StoryStyle::default(),
            },
            mentions: Vec::new(),
            viewed: false,
            view_count: None,
        };
        assert_eq!(story.expiry().as_millis(), 1_000 + 86_400_000);
        story.expires_at = Some(Timestamp::from_millis(5));
        assert_eq!(story.expiry().as_millis(), 5);
        assert!(StoryFont(6).is_known() && !StoryFont(3).is_known());
    }
}
