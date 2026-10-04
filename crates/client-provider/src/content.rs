//! What a message can carry besides text and files: a place, contact
//! cards, a poll, a calendar event, a notice from the chat itself; and what
//! rides along with any message (forwarded, starred, mentions, a link
//! preview).
//!
//! Like the rest of the model these are WhatsApp concepts, not any
//! provider's wire format.

use crate::ids::{ContactId, MediaRef};
use crate::model::Timestamp;
use serde::{Deserialize, Serialize};

/// A point on the map.
///
/// Kept as whole numbers (degrees × 10⁷, about a centimetre) so that the
/// model stays comparable with `Eq`, which floating point is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GeoPoint {
    lat_e7: i32,
    lon_e7: i32,
}

impl GeoPoint {
    /// A point from degrees. `None` when they are not a place on Earth
    /// (not finite, latitude beyond ±90, longitude beyond ±180).
    pub fn new(latitude: f64, longitude: f64) -> Option<Self> {
        let valid = latitude.is_finite()
            && longitude.is_finite()
            && (-90.0..=90.0).contains(&latitude)
            && (-180.0..=180.0).contains(&longitude);
        valid.then(|| Self {
            lat_e7: (latitude * 1e7).round() as i32,
            lon_e7: (longitude * 1e7).round() as i32,
        })
    }

    /// Degrees north of the equator.
    pub fn latitude(self) -> f64 {
        f64::from(self.lat_e7) / 1e7
    }

    /// Degrees east of Greenwich.
    pub fn longitude(self) -> f64 {
        f64::from(self.lon_e7) / 1e7
    }
}

/// A place somebody shared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Location {
    /// Where.
    pub point: GeoPoint,
    /// The name of the place, when one was attached ("Café Central").
    pub name: Option<String>,
    /// Its address, when one was attached.
    pub address: Option<String>,
    /// A live location: where the sender was when it was last updated,
    /// not a fixed place.
    #[serde(default)]
    pub live: bool,
}

/// A shared contact.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactCard {
    /// The name on the card.
    pub name: String,
    /// Its phone numbers, as the card has them (E.164 when the provider
    /// can tell).
    pub phones: Vec<String>,
}

/// One answer of a poll.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollOption {
    /// The answer. Unique within its poll: votes name options by it.
    pub name: String,
    /// How many people chose it.
    pub votes: u32,
}

/// A poll and its tally.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poll {
    /// The question.
    pub question: String,
    /// The answers, in the order they were written.
    pub options: Vec<PollOption>,
    /// How many answers a voter may pick; 0 means any number.
    pub max_choices: u32,
    /// How many people voted.
    pub voters: u32,
    /// The account's own vote, by option name. `None` when the provider
    /// cannot say: the client then keeps what it knows from the votes cast
    /// through it.
    #[serde(default)]
    pub chosen: Option<Vec<String>>,
}

impl Poll {
    /// True when a voter may pick more than one answer.
    pub fn multiple_choice(&self) -> bool {
        self.max_choices != 1
    }

    /// Whether the account chose this option, as far as is known.
    pub fn is_chosen(&self, option: &str) -> bool {
        self.chosen
            .as_ref()
            .is_some_and(|chosen| chosen.iter().any(|name| name == option))
    }

    /// The vote that a click on `option` asks for: on a single-choice poll
    /// the option alone (or nothing, when it was the one chosen); on a
    /// multiple-choice poll the option added to or taken from the vote.
    /// `None` when the poll allows no more answers.
    pub fn toggled(&self, option: &str) -> Option<Vec<String>> {
        let current = self.chosen.clone().unwrap_or_default();
        let had = current.iter().any(|name| name == option);
        if self.max_choices == 1 {
            return Some(if had {
                Vec::new()
            } else {
                vec![option.to_owned()]
            });
        }
        if had {
            return Some(current.into_iter().filter(|name| name != option).collect());
        }
        if self.max_choices != 0 && current.len() >= self.max_choices as usize {
            return None;
        }
        // In the poll's own order, whatever order the clicks came in.
        Some(
            self.options
                .iter()
                .map(|candidate| &candidate.name)
                .filter(|name| *name == option || current.contains(name))
                .cloned()
                .collect(),
        )
    }

    /// The poll as it stands once the account's vote is `choices`: the
    /// tally moves by the difference with the previous vote. Names that
    /// are not options of this poll are ignored.
    ///
    /// Applying the previous vote to the result gives the poll back, which
    /// is how a vote the provider refused is undone.
    pub fn with_vote(&self, choices: &[String]) -> Poll {
        let before = self.chosen.clone().unwrap_or_default();
        let mut after = Vec::new();
        let options = self
            .options
            .iter()
            .map(|option| {
                let was = before.contains(&option.name);
                let is = choices.contains(&option.name);
                if is {
                    after.push(option.name.clone());
                }
                PollOption {
                    name: option.name.clone(),
                    votes: match (was, is) {
                        (false, true) => option.votes.saturating_add(1),
                        (true, false) => option.votes.saturating_sub(1),
                        _ => option.votes,
                    },
                }
            })
            .collect();
        let voters = match (before.is_empty(), after.is_empty()) {
            (true, false) => self.voters.saturating_add(1),
            (false, true) => self.voters.saturating_sub(1),
            _ => self.voters,
        };
        Poll {
            question: self.question.clone(),
            options,
            max_choices: self.max_choices,
            voters,
            chosen: Some(after),
        }
    }
}

/// Whether a calendar event is a scheduled call, and of which kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCall {
    /// A voice call.
    Voice,
    /// A video call.
    Video,
}

/// Where a calendar event takes place.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventPlace {
    /// The name of the place.
    pub name: Option<String>,
    /// Its address.
    pub address: Option<String>,
    /// Where it is on the map.
    pub point: Option<GeoPoint>,
}

/// A calendar event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// What it is called.
    pub title: String,
    /// The longer text, if any.
    pub description: Option<String>,
    /// When it starts.
    pub starts_at: Option<Timestamp>,
    /// When it ends.
    pub ends_at: Option<Timestamp>,
    /// Where it takes place.
    pub place: Option<EventPlace>,
    /// Set when the event is a scheduled call.
    pub call: Option<EventCall>,
    /// The link to join that call.
    pub join_url: Option<String>,
    /// The event was called off.
    #[serde(default)]
    pub cancelled: bool,
}

/// Somebody a notice is about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Party {
    /// Who.
    pub id: ContactId,
    /// Their display name at the time, when known.
    pub name: Option<String>,
}

/// What a notice says happened.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemKind {
    /// The group was created (by the actor); `detail` is its name.
    GroupCreated,
    /// The targets joined by themselves (an invite link).
    Joined,
    /// The targets left.
    Left,
    /// The actor added the targets.
    Added,
    /// The actor removed the targets.
    Removed,
    /// The actor changed the group's name; `detail` is the new one.
    SubjectChanged,
    /// The actor changed the group's picture.
    PictureChanged,
    /// The targets were made admins.
    Promoted,
    /// The targets are no longer admins.
    Demoted,
    /// A voice call nobody answered.
    MissedVoiceCall,
    /// A video call nobody answered.
    MissedVideoCall,
    /// A voice call that took place.
    VoiceCall,
    /// A video call that took place.
    VideoCall,
}

/// A notice from the chat itself rather than from a person: somebody
/// joined, the group was renamed, a call was missed. Shown as a centred
/// line, never as a bubble.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemEvent {
    /// What happened.
    pub kind: SystemKind,
    /// Who did it, when known.
    pub actor: Option<Party>,
    /// Who it happened to.
    #[serde(default)]
    pub targets: Vec<Party>,
    /// The new name, for the kinds that carry one.
    #[serde(default)]
    pub detail: Option<String>,
}

/// Somebody mentioned in a message.
///
/// The text names them as `@` followed by `handle`; the client shows
/// `name` there instead. Providers leave `name` empty: the client fills
/// it from what it knows about the person. They set `handle` to what
/// their backend says stands in the text, or to [`mention_handle`] of the
/// id when it does not say; the client looks for the person's other ids
/// in the text when that one is not there.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mention {
    /// Who.
    pub id: ContactId,
    /// What follows the `@` in the message's text.
    pub handle: String,
    /// The name to show, when known.
    #[serde(default)]
    pub name: Option<String>,
    /// It is the account itself that is mentioned. Filled by the client.
    #[serde(default)]
    pub me: bool,
}

/// What follows the `@` where a text mentions the contact `id`: on
/// WhatsApp, the digits of the id (`+584245550199` and a hidden-number
/// id alike).
pub fn mention_handle(id: &ContactId) -> String {
    id.as_str().chars().filter(char::is_ascii_digit).collect()
}

/// The preview of a link, as the message itself carries it. The client
/// never fetches the page to make one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreview {
    /// The link.
    pub url: String,
    /// The page's title.
    pub title: Option<String>,
    /// A line or two about it.
    pub description: Option<String>,
    /// A small picture, fetched like any other image of a message.
    pub thumbnail: Option<MediaRef>,
}

/// What rides along with a message of any type.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MessageExtras {
    /// It was forwarded from another chat.
    pub forwarded: bool,
    /// It was forwarded many times (WhatsApp says so from the fifth
    /// forward on). Implies [`forwarded`](Self::forwarded).
    pub forwarded_many: bool,
    /// The account starred it.
    pub starred: bool,
    /// It can be opened once, on the phone. The client never downloads
    /// its file.
    pub view_once: bool,
    /// The people its text mentions.
    pub mentions: Vec<Mention>,
    /// The preview of the link in its text.
    pub link: Option<LinkPreview>,
    /// The sender's WhatsApp username, without the `@`, when the message
    /// came with one.
    pub sender_username: Option<String>,
    /// It answers a story.
    pub story_reply: Option<crate::StoryReplyRef>,
}

impl MessageExtras {
    /// Nothing rides along.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(max_choices: u32, chosen: Option<&[&str]>) -> Poll {
        Poll {
            question: "Where?".into(),
            options: [("Beach", 2), ("Mountains", 1), ("City", 0)]
                .into_iter()
                .map(|(name, votes)| PollOption {
                    name: name.into(),
                    votes,
                })
                .collect(),
            max_choices,
            voters: 3,
            chosen: chosen.map(|names| names.iter().map(|name| (*name).to_owned()).collect()),
        }
    }

    fn votes(poll: &Poll) -> Vec<u32> {
        poll.options.iter().map(|option| option.votes).collect()
    }

    #[test]
    fn points_keep_seven_decimals_and_refuse_nonsense() {
        let point = GeoPoint::new(10.491_016_5, -66.902_061_2).unwrap();
        assert!((point.latitude() - 10.491_016_5).abs() < 1e-7);
        assert!((point.longitude() + 66.902_061_2).abs() < 1e-7);
        assert!(GeoPoint::new(91.0, 0.0).is_none());
        assert!(GeoPoint::new(0.0, 181.0).is_none());
        assert!(GeoPoint::new(f64::NAN, 0.0).is_none());
    }

    #[test]
    fn a_vote_moves_the_tally_and_can_be_undone() {
        let before = poll(1, None);
        let voted = before.with_vote(&["City".to_owned()]);
        assert_eq!(votes(&voted), [2, 1, 1]);
        assert_eq!(voted.voters, 4);
        assert!(voted.is_chosen("City"));

        // Changing the vote moves one vote, not the number of voters.
        let changed = voted.with_vote(&["Beach".to_owned()]);
        assert_eq!(votes(&changed), [3, 1, 0]);
        assert_eq!(changed.voters, 4);

        // Putting the previous vote back restores the tally.
        let undone = voted.with_vote(&[]);
        assert_eq!(votes(&undone), votes(&before));
        assert_eq!(undone.voters, before.voters);
        assert_eq!(undone.chosen, Some(Vec::new()));

        // A name that is not an option changes nothing.
        let stray = before.with_vote(&["Moon".to_owned()]);
        assert_eq!(votes(&stray), votes(&before));
        assert_eq!(stray.chosen, Some(Vec::new()));
    }

    #[test]
    fn a_click_replaces_the_vote_on_single_choice_and_adds_on_multiple() {
        let single = poll(1, Some(&["Beach"]));
        assert_eq!(single.toggled("City"), Some(vec!["City".to_owned()]));
        assert_eq!(single.toggled("Beach"), Some(Vec::new()));

        let any = poll(0, Some(&["City"]));
        assert_eq!(
            any.toggled("Beach"),
            Some(vec!["Beach".to_owned(), "City".to_owned()]),
            "in the poll's order"
        );
        assert_eq!(any.toggled("City"), Some(Vec::new()));

        let two = poll(2, Some(&["Beach", "City"]));
        assert_eq!(two.toggled("Mountains"), None, "no more answers allowed");
        assert_eq!(two.toggled("Beach"), Some(vec!["City".to_owned()]));
        assert!(two.multiple_choice() && !single.multiple_choice());
    }

    #[test]
    fn extras_are_empty_by_default_and_read_from_older_rows() {
        assert!(MessageExtras::default().is_empty());
        let read: MessageExtras = serde_json::from_str(r#"{"starred":true}"#).unwrap();
        assert!(read.starred && !read.forwarded && read.mentions.is_empty());
    }
}
