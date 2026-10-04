//! A message in one line: what the chat list shows under a chat's name and
//! what a reply shows of the message it quotes.

use client_provider::{MediaKind, MessageContent, Party, SystemEvent, SystemKind};

/// One line for a message's content. `view_once` hides what a view-once
/// file is beyond its kind.
pub(crate) fn content_line(content: &MessageContent, deleted: bool, view_once: bool) -> String {
    if deleted {
        return "This message was deleted".to_owned();
    }
    let flat = |text: &str| text.replace('\n', " ");
    let labelled = |label: &str, detail: Option<&str>| match detail.map(str::trim) {
        Some(detail) if !detail.is_empty() => format!("{label} · {}", flat(detail)),
        _ => label.to_owned(),
    };
    match content {
        MessageContent::Text { body } => flat(body),
        MessageContent::Media(media) if view_once => match media.kind {
            MediaKind::Image => "View once photo",
            MediaKind::Video => "View once video",
            MediaKind::Voice | MediaKind::Audio => "View once voice message",
            MediaKind::Document | MediaKind::Sticker => "View once message",
        }
        .to_owned(),
        MessageContent::Media(media) => {
            let label = match media.kind {
                MediaKind::Image => "Photo",
                MediaKind::Video if media.gif => "GIF",
                MediaKind::Video => "Video",
                MediaKind::Audio => "Audio",
                MediaKind::Voice => "Voice message",
                MediaKind::Sticker => "Sticker",
                MediaKind::Document => media.file_name.as_deref().unwrap_or("Document"),
            };
            labelled(label, media.caption.as_deref())
        }
        MessageContent::Reaction { emoji, .. } => emoji.clone(),
        MessageContent::Location(location) => labelled(
            if location.live {
                "Live location"
            } else {
                "Location"
            },
            location.name.as_deref().or(location.address.as_deref()),
        ),
        MessageContent::Contacts { cards } => match cards.as_slice() {
            [] => "Contact".to_owned(),
            [card] => labelled("Contact", Some(&card.name)),
            [first, rest @ ..] => format!(
                "Contacts · {} and {} more",
                flat(first.name.trim()),
                rest.len()
            ),
        },
        MessageContent::Poll(poll) => labelled("Poll", Some(&poll.question)),
        MessageContent::Event(event) => labelled(
            if event.cancelled {
                "Cancelled event"
            } else {
                "Event"
            },
            Some(&event.title),
        ),
        MessageContent::System(event) => system_line(event),
        MessageContent::Unsupported { .. } => "Unsupported message".to_owned(),
    }
}

fn party(party: &Party) -> String {
    match party.name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => party.id.to_string(),
    }
}

/// "Ana", "Ana and Luis", "Ana, Luis and 3 more".
fn parties(list: &[Party]) -> Option<String> {
    match list {
        [] => None,
        [one] => Some(party(one)),
        [one, two] => Some(format!("{} and {}", party(one), party(two))),
        [one, two, rest @ ..] => Some(format!(
            "{}, {} and {} more",
            party(one),
            party(two),
            rest.len()
        )),
    }
}

/// A notice from the chat, in words: "Ana added Luis", "Missed voice call".
pub fn system_line(event: &SystemEvent) -> String {
    let actor = event.actor.as_ref().map(party);
    let targets = parties(&event.targets);
    let plural = event.targets.len() > 1;
    let named = event
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty());
    let who = |fallback: &str| actor.clone().unwrap_or_else(|| fallback.to_owned());
    match event.kind {
        SystemKind::GroupCreated => match (actor, named) {
            (Some(actor), Some(name)) => format!("{actor} created the group “{name}”"),
            (Some(actor), None) => format!("{actor} created the group"),
            (None, Some(name)) => format!("The group “{name}” was created"),
            (None, None) => "The group was created".to_owned(),
        },
        SystemKind::Joined => match targets {
            Some(targets) => format!("{targets} joined"),
            None => format!("{} joined", who("Someone")),
        },
        SystemKind::Left => match targets {
            Some(targets) => format!("{targets} left"),
            None => format!("{} left", who("Someone")),
        },
        SystemKind::Added => match (actor, targets) {
            (Some(actor), Some(targets)) => format!("{actor} added {targets}"),
            (None, Some(targets)) => {
                format!("{targets} {} added", if plural { "were" } else { "was" })
            }
            (actor, None) => format!(
                "{} added someone",
                actor.unwrap_or_else(|| "Someone".to_owned())
            ),
        },
        SystemKind::Removed => match (actor, targets) {
            (Some(actor), Some(targets)) => format!("{actor} removed {targets}"),
            (None, Some(targets)) => {
                format!("{targets} {} removed", if plural { "were" } else { "was" })
            }
            (actor, None) => format!(
                "{} removed someone",
                actor.unwrap_or_else(|| "Someone".to_owned())
            ),
        },
        SystemKind::SubjectChanged => match (actor, named) {
            (Some(actor), Some(name)) => format!("{actor} changed the group name to “{name}”"),
            (None, Some(name)) => format!("The group name changed to “{name}”"),
            (Some(actor), None) => format!("{actor} changed the group name"),
            (None, None) => "The group name changed".to_owned(),
        },
        SystemKind::PictureChanged => match actor {
            Some(actor) => format!("{actor} changed the group picture"),
            None => "The group picture changed".to_owned(),
        },
        SystemKind::Promoted => match targets {
            Some(targets) if plural => format!("{targets} are now admins"),
            Some(targets) => format!("{targets} is now an admin"),
            None => "A new admin was named".to_owned(),
        },
        SystemKind::Demoted => match targets {
            Some(targets) if plural => format!("{targets} are no longer admins"),
            Some(targets) => format!("{targets} is no longer an admin"),
            None => "An admin was removed".to_owned(),
        },
        SystemKind::MissedVoiceCall => "Missed voice call".to_owned(),
        SystemKind::MissedVideoCall => "Missed video call".to_owned(),
        SystemKind::VoiceCall => "Voice call".to_owned(),
        SystemKind::VideoCall => "Video call".to_owned(),
    }
}
