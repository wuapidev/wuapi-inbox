//! WhatsApp's own message (the protobuf every device exchanges) to the
//! neutral model. No I/O here: what a message carries is read, never
//! fetched.

use crate::ids::{id_of_wire, STATUS_CHAT};
use base64::Engine as _;
use client_provider::{
    CalendarEvent, ContactCard, ContactId, EventCall, EventPlace, GeoPoint, LinkPreview, Location,
    Media, MediaKind, MediaRef, Mention, MessageContent, MessageExtras, MessageId, Poll,
    PollOption, ReplyRef, StoryReplyKind, StoryReplyRef, SystemEvent, SystemKind, Timestamp,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use whatsapp_rust::waproto::whatsapp as wa;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// Where a file is on WhatsApp and how to open it: what a [`MediaRef`] of
/// this provider holds. WhatsApp keeps files encrypted under a key that
/// travels in the message, so the reference is the message's own pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaPointer {
    /// The path on WhatsApp's media hosts.
    #[serde(rename = "p")]
    pub direct_path: String,
    /// The key the file is encrypted under.
    #[serde(rename = "k", with = "b64")]
    pub media_key: Vec<u8>,
    /// The hash of the file.
    #[serde(rename = "s", with = "b64")]
    pub file_sha256: Vec<u8>,
    /// The hash of the encrypted file.
    #[serde(rename = "e", with = "b64")]
    pub file_enc_sha256: Vec<u8>,
    /// Its length in bytes.
    #[serde(rename = "n")]
    pub file_length: u64,
    /// What WhatsApp stores it as: `image`, `video`, `audio`, `document`,
    /// `sticker`.
    #[serde(rename = "t")]
    pub class: String,
    /// Its MIME type.
    #[serde(rename = "m", default)]
    pub mime_type: Option<String>,
    /// The full address, for a file uploaded from here.
    #[serde(rename = "u", default)]
    pub url: Option<String>,
    /// When its key was made, for a file uploaded from here.
    #[serde(rename = "ts", default)]
    pub key_timestamp: Option<i64>,
}

mod b64 {
    use super::B64;
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&B64.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        B64.decode(text).map_err(serde::de::Error::custom)
    }
}

/// A reference that is the picture itself: a link preview's thumbnail
/// travels inside its message.
const INLINE: &str = "inline:";

impl MediaPointer {
    /// The reference the client hands back to download the file.
    pub fn to_ref(&self) -> MediaRef {
        MediaRef::new(serde_json::to_string(self).unwrap_or_default())
    }

    /// Reads a reference of this provider.
    pub fn parse(media: &MediaRef) -> Option<Self> {
        serde_json::from_str(media.as_str()).ok()
    }
}

/// A reference to bytes a message carries itself.
pub fn inline_ref(bytes: &[u8]) -> MediaRef {
    MediaRef::new(format!("{INLINE}{}", B64.encode(bytes)))
}

/// The bytes of [`inline_ref`].
pub fn inline_bytes(media: &MediaRef) -> Option<Vec<u8>> {
    B64.decode(media.as_str().strip_prefix(INLINE)?).ok()
}

/// What a message is, once its wrappers are off.
#[derive(Clone, Debug, PartialEq)]
pub enum Effect {
    /// Something a conversation shows.
    Content(Box<Mapped>),
    /// It takes another message back, for everyone.
    Revoke {
        /// The message taken back.
        target: String,
    },
    /// It replaces the text of another message.
    Edit {
        /// The message edited.
        target: String,
        /// What it says now.
        replacement: Box<wa::Message>,
    },
    /// A vote in a poll, still encrypted.
    PollVote {
        /// The poll.
        poll: String,
    },
    /// Nothing a conversation shows: keys, receipts between devices.
    Nothing,
}

/// A message's content in the neutral model.
#[derive(Clone, Debug, PartialEq)]
pub struct Mapped {
    /// What it carries.
    pub content: MessageContent,
    /// What it quotes.
    pub reply_to: Option<ReplyRef>,
    /// What rides along.
    pub extras: MessageExtras,
}

/// The votes of a poll, as far as this device has seen them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PollVotes {
    /// One entry per voter: whether it is the account itself, and the
    /// hashes of the options it stands for.
    pub voters: Vec<(bool, Vec<Vec<u8>>)>,
}

/// The message inside the wrappers WhatsApp puts around one: disappearing,
/// view once, sent from another device, a document with its caption, an
/// edit.
pub fn unwrap(message: &wa::Message) -> &wa::Message {
    let mut current = message;
    for _ in 0..8 {
        let inner = current
            .device_sent_message
            .as_option()
            .and_then(|sent| sent.message.as_option())
            .or_else(|| future_proof(&current.ephemeral_message))
            .or_else(|| future_proof(&current.view_once_message))
            .or_else(|| future_proof(&current.view_once_message_v2))
            .or_else(|| future_proof(&current.view_once_message_v2_extension))
            .or_else(|| future_proof(&current.document_with_caption_message))
            .or_else(|| future_proof(&current.edited_message))
            .or_else(|| future_proof(&current.group_mentioned_message))
            .or_else(|| future_proof(&current.lottie_sticker_message));
        match inner {
            Some(inner) => current = inner,
            None => break,
        }
    }
    current
}

fn future_proof(
    field: &whatsapp_rust::buffa::MessageField<wa::message::FutureProofMessage>,
) -> Option<&wa::Message> {
    field
        .as_option()
        .and_then(|wrapper| wrapper.message.as_option())
}

fn is_view_once(message: &wa::Message) -> bool {
    message.view_once_message.is_set()
        || message.view_once_message_v2.is_set()
        || message.view_once_message_v2_extension.is_set()
        || message
            .ephemeral_message
            .as_option()
            .and_then(|wrapper| wrapper.message.as_option())
            .is_some_and(is_view_once)
        || message
            .device_sent_message
            .as_option()
            .and_then(|sent| sent.message.as_option())
            .is_some_and(is_view_once)
}

/// What a message does or shows.
pub fn effect(message: &wa::Message, votes: Option<&PollVotes>) -> Effect {
    let base = unwrap(message);
    if let Some(protocol) = base.protocol_message.as_option() {
        use wa::message::protocol_message::Type;
        let target = protocol
            .key
            .as_option()
            .and_then(|key| key.id.clone())
            .unwrap_or_default();
        return match protocol.r#type {
            Some(Type::REVOKE) if !target.is_empty() => Effect::Revoke { target },
            Some(Type::MESSAGE_EDIT) if !target.is_empty() => {
                match protocol.edited_message.as_option() {
                    Some(replacement) => Effect::Edit {
                        target,
                        replacement: Box::new(replacement.clone()),
                    },
                    None => Effect::Nothing,
                }
            }
            _ => Effect::Nothing,
        };
    }
    if let Some(update) = base.poll_update_message.as_option() {
        let poll = update
            .poll_creation_message_key
            .as_option()
            .and_then(|key| key.id.clone())
            .unwrap_or_default();
        return if poll.is_empty() {
            Effect::Nothing
        } else {
            Effect::PollVote { poll }
        };
    }
    match content(base, votes) {
        Some(content) => {
            let context = context_of(base);
            let mut extras = extras_of(base, context);
            extras.view_once = extras.view_once || is_view_once(message);
            Effect::Content(Box::new(Mapped {
                reply_to: reply_of(context),
                content,
                extras,
            }))
        }
        None => Effect::Nothing,
    }
}

/// What a message shows, `None` for one that shows nothing.
fn content(base: &wa::Message, votes: Option<&PollVotes>) -> Option<MessageContent> {
    if let Some(text) = base.conversation.as_deref().filter(|text| !text.is_empty()) {
        return Some(MessageContent::text(text));
    }
    if let Some(text) = base.extended_text_message.as_option() {
        return Some(MessageContent::text(text.text.clone().unwrap_or_default()));
    }
    if let Some(reaction) = base.reaction_message.as_option() {
        let target = reaction.key.as_option().and_then(|key| key.id.clone())?;
        return Some(MessageContent::Reaction {
            target: MessageId::new(target),
            emoji: reaction.text.clone().unwrap_or_default(),
        });
    }
    if let Some(media) = media_of(base) {
        return Some(MessageContent::Media(media));
    }
    if let Some(place) = base.location_message.as_option() {
        return Some(location(
            place.degrees_latitude,
            place.degrees_longitude,
            place.name.clone(),
            place.address.clone(),
            place.is_live.unwrap_or(false),
        ));
    }
    if let Some(place) = base.live_location_message.as_option() {
        return Some(location(
            place.degrees_latitude,
            place.degrees_longitude,
            place.caption.clone(),
            None,
            true,
        ));
    }
    if let Some(card) = base.contact_message.as_option() {
        return Some(MessageContent::Contacts {
            cards: vec![contact_card(card)],
        });
    }
    if let Some(cards) = base.contacts_array_message.as_option() {
        return Some(MessageContent::Contacts {
            cards: cards.contacts.iter().map(contact_card).collect(),
        });
    }
    let poll = base
        .poll_creation_message
        .as_option()
        .or_else(|| base.poll_creation_message_v2.as_option())
        .or_else(|| base.poll_creation_message_v3.as_option());
    if let Some(poll) = poll {
        return Some(MessageContent::Poll(poll_of(poll, votes)));
    }
    if let Some(event) = base.event_message.as_option() {
        return Some(MessageContent::Event(calendar_event(event)));
    }
    if let Some(call) = base.call_log_messsage.as_option() {
        return Some(MessageContent::System(call_notice(call)));
    }
    unsupported(base).map(|description| MessageContent::Unsupported {
        description: description.to_owned(),
    })
}

/// The name of a kind of message this client does not draw. `None` for a
/// message that is not for a conversation at all.
fn unsupported(base: &wa::Message) -> Option<&'static str> {
    let kinds: [(bool, &'static str); 16] = [
        (base.template_message.is_set(), "template"),
        (base.buttons_message.is_set(), "buttons"),
        (base.buttons_response_message.is_set(), "button reply"),
        (base.list_message.is_set(), "list"),
        (base.list_response_message.is_set(), "list reply"),
        (base.interactive_message.is_set(), "interactive"),
        (
            base.interactive_response_message.is_set(),
            "interactive reply",
        ),
        (base.order_message.is_set(), "order"),
        (base.product_message.is_set(), "product"),
        (base.send_payment_message.is_set(), "payment"),
        (base.request_payment_message.is_set(), "payment request"),
        (base.group_invite_message.is_set(), "group invite"),
        (base.sticker_pack_message.is_set(), "sticker pack"),
        (base.album_message.is_set(), "album"),
        (
            base.newsletter_admin_invite_message.is_set(),
            "channel admin invite",
        ),
        (
            base.scheduled_call_creation_message.is_set(),
            "scheduled call",
        ),
    ];
    kinds
        .into_iter()
        .find(|(set, _)| *set)
        .map(|(_, name)| name)
}

fn location(
    latitude: Option<f64>,
    longitude: Option<f64>,
    name: Option<String>,
    address: Option<String>,
    live: bool,
) -> MessageContent {
    match GeoPoint::new(latitude.unwrap_or(f64::NAN), longitude.unwrap_or(f64::NAN)) {
        Some(point) => MessageContent::Location(Location {
            point,
            name: name.filter(|text| !text.is_empty()),
            address: address.filter(|text| !text.is_empty()),
            live,
        }),
        None => MessageContent::Unsupported {
            description: "location".into(),
        },
    }
}

fn contact_card(card: &wa::message::ContactMessage) -> ContactCard {
    ContactCard {
        name: card.display_name.clone().unwrap_or_default(),
        phones: card.vcard.as_deref().map(vcard_phones).unwrap_or_default(),
    }
}

/// The numbers of a vCard: the WhatsApp id of a line when it has one (it
/// is the number in full), else what the line says.
pub fn vcard_phones(vcard: &str) -> Vec<String> {
    let mut phones = Vec::new();
    for line in vcard.lines() {
        let Some((head, value)) = line.split_once(':') else {
            continue;
        };
        let head = head.to_ascii_uppercase();
        if !head
            .split(';')
            .any(|part| part.rsplit('.').next() == Some("TEL"))
        {
            continue;
        }
        let waid = head
            .split(';')
            .find_map(|part| part.strip_prefix("WAID="))
            .map(|digits| format!("+{digits}"));
        let phone = waid.unwrap_or_else(|| value.trim().to_owned());
        if !phone.is_empty() && !phones.contains(&phone) {
            phones.push(phone);
        }
    }
    phones
}

/// The hash a vote names an option by.
pub fn option_hash(name: &str) -> Vec<u8> {
    Sha256::digest(name.as_bytes()).to_vec()
}

fn poll_of(poll: &wa::message::PollCreationMessage, votes: Option<&PollVotes>) -> Poll {
    let names: Vec<String> = poll
        .options
        .iter()
        .map(|option| option.option_name.clone().unwrap_or_default())
        .collect();
    let empty = PollVotes::default();
    let votes = votes.unwrap_or(&empty);
    let options = names
        .iter()
        .map(|name| {
            let hash = option_hash(name);
            PollOption {
                name: name.clone(),
                votes: votes
                    .voters
                    .iter()
                    .filter(|(_, chosen)| chosen.contains(&hash))
                    .count() as u32,
            }
        })
        .collect();
    let chosen = votes
        .voters
        .iter()
        .find(|(mine, _)| *mine)
        .map(|(_, chosen)| {
            names
                .iter()
                .filter(|name| chosen.contains(&option_hash(name)))
                .cloned()
                .collect()
        });
    Poll {
        question: poll.name.clone().unwrap_or_default(),
        options,
        max_choices: poll.selectable_options_count.unwrap_or(0),
        voters: votes
            .voters
            .iter()
            .filter(|(_, chosen)| !chosen.is_empty())
            .count() as u32,
        chosen,
    }
}

fn calendar_event(event: &wa::message::EventMessage) -> CalendarEvent {
    let place = event.location.as_option().map(|place| EventPlace {
        name: place.name.clone().filter(|text| !text.is_empty()),
        address: place.address.clone().filter(|text| !text.is_empty()),
        point: GeoPoint::new(
            place.degrees_latitude.unwrap_or(f64::NAN),
            place.degrees_longitude.unwrap_or(f64::NAN),
        ),
    });
    let seconds = |value: Option<i64>| {
        value
            .filter(|seconds| *seconds > 0)
            .map(|seconds| Timestamp::from_millis(seconds.saturating_mul(1000)))
    };
    CalendarEvent {
        title: event.name.clone().unwrap_or_default(),
        description: event.description.clone().filter(|text| !text.is_empty()),
        starts_at: seconds(event.start_time),
        ends_at: seconds(event.end_time),
        place,
        // The message says that it is a call, not of which kind.
        call: event
            .is_schedule_call
            .unwrap_or(false)
            .then_some(EventCall::Voice),
        join_url: event.join_link.clone().filter(|text| !text.is_empty()),
        cancelled: event.is_canceled.unwrap_or(false),
    }
}

fn call_notice(call: &wa::message::CallLogMessage) -> SystemEvent {
    use wa::message::call_log_message::CallOutcome;
    let video = call.is_video.unwrap_or(false);
    let took_place = matches!(
        call.call_outcome,
        Some(CallOutcome::CONNECTED | CallOutcome::ACCEPTED_ELSEWHERE | CallOutcome::ONGOING)
    );
    SystemEvent {
        kind: match (took_place, video) {
            (true, false) => SystemKind::VoiceCall,
            (true, true) => SystemKind::VideoCall,
            (false, false) => SystemKind::MissedVoiceCall,
            (false, true) => SystemKind::MissedVideoCall,
        },
        actor: None,
        targets: Vec::new(),
        detail: None,
    }
}

/// A pointer from the five fields every file of a message has. `None`
/// when WhatsApp no longer says where the file is.
#[allow(clippy::too_many_arguments)]
fn pointer(
    class: &str,
    direct_path: &Option<String>,
    media_key: &Option<Vec<u8>>,
    file_sha256: &Option<Vec<u8>>,
    file_enc_sha256: &Option<Vec<u8>>,
    file_length: Option<u64>,
    mime_type: &Option<String>,
) -> Option<MediaRef> {
    Some(
        MediaPointer {
            direct_path: direct_path.clone().filter(|path| !path.is_empty())?,
            media_key: media_key.clone().filter(|key| !key.is_empty())?,
            file_sha256: file_sha256.clone().unwrap_or_default(),
            file_enc_sha256: file_enc_sha256.clone().unwrap_or_default(),
            file_length: file_length.unwrap_or(0),
            class: class.to_owned(),
            mime_type: mime_type.clone(),
            url: None,
            key_timestamp: None,
        }
        .to_ref(),
    )
}

/// The file a message carries.
pub fn media_of(base: &wa::Message) -> Option<Media> {
    if let Some(image) = base.image_message.as_option() {
        return Some(Media {
            source: pointer(
                "image",
                &image.direct_path,
                &image.media_key,
                &image.file_sha256,
                &image.file_enc_sha256,
                image.file_length,
                &image.mimetype,
            ),
            mime_type: image.mimetype.clone(),
            caption: image.caption.clone().filter(|text| !text.is_empty()),
            size_bytes: image.file_length,
            width: image.width,
            height: image.height,
            ..Media::new(MediaKind::Image)
        });
    }
    let video = base
        .video_message
        .as_option()
        .or_else(|| base.ptv_message.as_option());
    if let Some(video) = video {
        return Some(Media {
            source: pointer(
                "video",
                &video.direct_path,
                &video.media_key,
                &video.file_sha256,
                &video.file_enc_sha256,
                video.file_length,
                &video.mimetype,
            ),
            mime_type: video.mimetype.clone(),
            caption: video.caption.clone().filter(|text| !text.is_empty()),
            size_bytes: video.file_length,
            width: video.width,
            height: video.height,
            duration_secs: video.seconds,
            gif: video.gif_playback.unwrap_or(false),
            ..Media::new(MediaKind::Video)
        });
    }
    if let Some(audio) = base.audio_message.as_option() {
        let voice = audio.ptt.unwrap_or(false);
        return Some(Media {
            source: pointer(
                "audio",
                &audio.direct_path,
                &audio.media_key,
                &audio.file_sha256,
                &audio.file_enc_sha256,
                audio.file_length,
                &audio.mimetype,
            ),
            mime_type: audio.mimetype.clone(),
            size_bytes: audio.file_length,
            duration_secs: audio.seconds,
            ..Media::new(if voice {
                MediaKind::Voice
            } else {
                MediaKind::Audio
            })
        });
    }
    if let Some(document) = base.document_message.as_option() {
        return Some(Media {
            source: pointer(
                "document",
                &document.direct_path,
                &document.media_key,
                &document.file_sha256,
                &document.file_enc_sha256,
                document.file_length,
                &document.mimetype,
            ),
            mime_type: document.mimetype.clone(),
            file_name: document
                .file_name
                .clone()
                .or_else(|| document.title.clone())
                .filter(|text| !text.is_empty()),
            caption: document.caption.clone().filter(|text| !text.is_empty()),
            size_bytes: document.file_length,
            ..Media::new(MediaKind::Document)
        });
    }
    if let Some(sticker) = base.sticker_message.as_option() {
        return Some(Media {
            source: pointer(
                "sticker",
                &sticker.direct_path,
                &sticker.media_key,
                &sticker.file_sha256,
                &sticker.file_enc_sha256,
                sticker.file_length,
                &sticker.mimetype,
            ),
            mime_type: sticker.mimetype.clone(),
            size_bytes: sticker.file_length,
            width: sticker.width,
            height: sticker.height,
            ..Media::new(MediaKind::Sticker)
        });
    }
    None
}

/// What a message says about the message it answers, forwards or mentions.
pub fn context_of(base: &wa::Message) -> Option<&wa::ContextInfo> {
    macro_rules! first {
        ($($field:ident),+ $(,)?) => {
            None$(.or_else(|| base.$field.as_option().and_then(|part| part.context_info.as_option())))+
        };
    }
    first!(
        extended_text_message,
        image_message,
        video_message,
        ptv_message,
        audio_message,
        document_message,
        sticker_message,
        location_message,
        live_location_message,
        contact_message,
        contacts_array_message,
        poll_creation_message,
        poll_creation_message_v2,
        poll_creation_message_v3,
        event_message,
    )
}

fn reply_of(context: Option<&wa::ContextInfo>) -> Option<ReplyRef> {
    let context = context?;
    // What answers a story quotes it too; that is `story_reply`.
    if context.remote_jid.as_deref() == Some(STATUS_CHAT) {
        return None;
    }
    let id = context.stanza_id.clone().filter(|id| !id.is_empty())?;
    Some(ReplyRef {
        message_id: MessageId::new(id),
        sender_name: None,
        preview: context.quoted_message.as_option().and_then(preview_of),
    })
}

/// A few words of a message, for a quote.
pub fn preview_of(message: &wa::Message) -> Option<String> {
    let base = unwrap(message);
    let words = base
        .conversation
        .clone()
        .or_else(|| {
            base.extended_text_message
                .as_option()
                .and_then(|text| text.text.clone())
        })
        .or_else(|| media_of(base).and_then(|media| media.caption))
        .filter(|text| !text.trim().is_empty())?;
    Some(words.chars().take(160).collect())
}

fn story_kind(message: &wa::Message) -> StoryReplyKind {
    match media_of(unwrap(message)).map(|media| media.kind) {
        Some(MediaKind::Image | MediaKind::Sticker) => StoryReplyKind::Image,
        Some(MediaKind::Video) => StoryReplyKind::Video,
        Some(MediaKind::Voice | MediaKind::Audio) => StoryReplyKind::Voice,
        _ => StoryReplyKind::Text,
    }
}

fn extras_of(base: &wa::Message, context: Option<&wa::ContextInfo>) -> MessageExtras {
    let mut extras = MessageExtras::default();
    if let Some(context) = context {
        extras.forwarded = context.is_forwarded.unwrap_or(false);
        // WhatsApp says "Forwarded many times" from the fifth forward on.
        extras.forwarded_many = context.forwarding_score.unwrap_or(0) >= 5;
        extras.forwarded = extras.forwarded || extras.forwarded_many;
        extras.mentions = context
            .mentioned_jid
            .iter()
            .map(|address| {
                let id = ContactId::new(id_of_wire(address));
                Mention {
                    handle: client_provider::mention_handle(&id),
                    id,
                    name: None,
                    me: false,
                }
            })
            .collect();
        if context.remote_jid.as_deref() == Some(STATUS_CHAT) {
            if let Some(story) = context.stanza_id.clone().filter(|id| !id.is_empty()) {
                let quoted = context.quoted_message.as_option();
                extras.story_reply = Some(StoryReplyRef {
                    story: MessageId::new(story),
                    kind: quoted.map(story_kind).unwrap_or(StoryReplyKind::Text),
                    preview: quoted.and_then(preview_of),
                    of_mine: false,
                });
            }
        }
    }
    if let Some(text) = base.extended_text_message.as_option() {
        let url = text.matched_text.clone().filter(|url| !url.is_empty());
        let described = text.title.is_some() || text.description.is_some();
        if let (Some(url), true) = (url, described) {
            extras.link = Some(LinkPreview {
                url,
                title: text.title.clone().filter(|text| !text.is_empty()),
                description: text.description.clone().filter(|text| !text.is_empty()),
                thumbnail: text
                    .jpeg_thumbnail
                    .as_deref()
                    .filter(|bytes| !bytes.is_empty())
                    .map(inline_ref),
            });
        }
    }
    let once = |flag: Option<bool>| flag.unwrap_or(false);
    extras.view_once = base
        .image_message
        .as_option()
        .is_some_and(|m| once(m.view_once))
        || base
            .video_message
            .as_option()
            .is_some_and(|m| once(m.view_once))
        || base
            .audio_message
            .as_option()
            .is_some_and(|m| once(m.view_once));
    extras
}

#[cfg(test)]
mod tests {
    use super::*;
    use whatsapp_rust::buffa::MessageField;

    fn content_of(message: &wa::Message) -> Mapped {
        match effect(message, None) {
            Effect::Content(mapped) => *mapped,
            other => panic!("expected content, got {other:?}"),
        }
    }

    fn image() -> wa::message::ImageMessage {
        wa::message::ImageMessage {
            mimetype: Some("image/jpeg".into()),
            caption: Some("the beach".into()),
            direct_path: Some("/v/t62/abc".into()),
            media_key: Some(vec![7; 32]),
            file_sha256: Some(vec![1; 32]),
            file_enc_sha256: Some(vec![2; 32]),
            file_length: Some(4096),
            width: Some(800),
            height: Some(600),
            ..Default::default()
        }
    }

    #[test]
    fn a_text_is_a_text_plain_or_extended() {
        let plain = wa::Message {
            conversation: Some("hello".into()),
            ..Default::default()
        };
        assert_eq!(content_of(&plain).content, MessageContent::text("hello"));

        let extended = wa::Message {
            extended_text_message: MessageField::some(wa::message::ExtendedTextMessage {
                text: Some("see https://example.com".into()),
                matched_text: Some("https://example.com".into()),
                title: Some("Example".into()),
                jpeg_thumbnail: Some(vec![9, 9, 9]),
                context_info: MessageField::some(wa::ContextInfo {
                    stanza_id: Some("3EB0AAAA".into()),
                    participant: Some("584245550199@s.whatsapp.net".into()),
                    quoted_message: MessageField::some(plain.clone()),
                    mentioned_jid: vec!["584125550100@s.whatsapp.net".into()],
                    is_forwarded: Some(true),
                    forwarding_score: Some(7),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mapped = content_of(&extended);
        assert_eq!(
            mapped.content,
            MessageContent::text("see https://example.com")
        );
        let reply = mapped.reply_to.expect("it quotes");
        assert_eq!(reply.message_id.as_str(), "3EB0AAAA");
        assert_eq!(reply.preview.as_deref(), Some("hello"));
        assert!(mapped.extras.forwarded && mapped.extras.forwarded_many);
        assert_eq!(mapped.extras.mentions[0].id.as_str(), "+584125550100");
        assert_eq!(mapped.extras.mentions[0].handle, "584125550100");
        let link = mapped.extras.link.expect("a preview");
        assert_eq!(link.title.as_deref(), Some("Example"));
        assert_eq!(
            inline_bytes(&link.thumbnail.unwrap()).unwrap(),
            vec![9, 9, 9]
        );
    }

    #[test]
    fn a_picture_keeps_the_pointer_that_downloads_it() {
        let message = wa::Message {
            image_message: MessageField::some(image()),
            ..Default::default()
        };
        let MessageContent::Media(media) = content_of(&message).content else {
            panic!("a picture");
        };
        assert_eq!(media.kind, MediaKind::Image);
        assert_eq!(media.caption.as_deref(), Some("the beach"));
        assert_eq!((media.width, media.height), (Some(800), Some(600)));
        let pointer = MediaPointer::parse(&media.source.unwrap()).unwrap();
        assert_eq!(pointer.direct_path, "/v/t62/abc");
        assert_eq!(pointer.media_key, vec![7; 32]);
        assert_eq!(pointer.class, "image");
    }

    #[test]
    fn a_file_whatsapp_no_longer_points_to_has_no_source() {
        let message = wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                direct_path: None,
                ..image()
            }),
            ..Default::default()
        };
        let MessageContent::Media(media) = content_of(&message).content else {
            panic!("a picture");
        };
        assert!(media.source.is_none());
    }

    #[test]
    fn wrappers_come_off_and_view_once_is_kept() {
        let inner = wa::Message {
            image_message: MessageField::some(image()),
            ..Default::default()
        };
        let wrapped = wa::Message {
            ephemeral_message: MessageField::some(wa::message::FutureProofMessage {
                message: MessageField::some(wa::Message {
                    view_once_message_v2: MessageField::some(wa::message::FutureProofMessage {
                        message: MessageField::some(inner),
                    }),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let mapped = content_of(&wrapped);
        assert!(matches!(mapped.content, MessageContent::Media(_)));
        assert!(mapped.extras.view_once);
    }

    #[test]
    fn a_voice_note_is_not_an_audio_file() {
        let message = wa::Message {
            audio_message: MessageField::some(wa::message::AudioMessage {
                ptt: Some(true),
                seconds: Some(12),
                ..Default::default()
            }),
            ..Default::default()
        };
        let MessageContent::Media(media) = content_of(&message).content else {
            panic!("a voice note");
        };
        assert_eq!(media.kind, MediaKind::Voice);
        assert_eq!(media.duration_secs, Some(12));
    }

    #[test]
    fn a_reaction_names_its_target_and_an_empty_one_takes_it_back() {
        let message = wa::Message {
            reaction_message: MessageField::some(wa::message::ReactionMessage {
                key: MessageField::some(wa::MessageKey {
                    id: Some("3EB0BBBB".into()),
                    ..Default::default()
                }),
                text: Some(String::new()),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            content_of(&message).content,
            MessageContent::Reaction {
                target: MessageId::new("3EB0BBBB"),
                emoji: String::new(),
            }
        );
    }

    #[test]
    fn a_revoke_and_an_edit_name_what_they_change() {
        use wa::message::protocol_message::Type;
        let key = MessageField::some(wa::MessageKey {
            id: Some("3EB0CCCC".into()),
            ..Default::default()
        });
        let revoke = wa::Message {
            protocol_message: MessageField::some(wa::message::ProtocolMessage {
                key: key.clone(),
                r#type: Some(Type::REVOKE),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            effect(&revoke, None),
            Effect::Revoke {
                target: "3EB0CCCC".into()
            }
        );

        let edit = wa::Message {
            protocol_message: MessageField::some(wa::message::ProtocolMessage {
                key,
                r#type: Some(Type::MESSAGE_EDIT),
                edited_message: MessageField::some(wa::Message {
                    conversation: Some("fixed".into()),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let Effect::Edit {
            target,
            replacement,
        } = effect(&edit, None)
        else {
            panic!("an edit");
        };
        assert_eq!(target, "3EB0CCCC");
        assert_eq!(replacement.conversation.as_deref(), Some("fixed"));
    }

    #[test]
    fn keys_between_devices_show_nothing() {
        let keys = wa::Message {
            sender_key_distribution_message: MessageField::some(Default::default()),
            ..Default::default()
        };
        assert_eq!(effect(&keys, None), Effect::Nothing);
        assert_eq!(effect(&wa::Message::default(), None), Effect::Nothing);
    }

    #[test]
    fn what_is_not_drawn_is_named_and_not_dropped() {
        let message = wa::Message {
            order_message: MessageField::some(Default::default()),
            ..Default::default()
        };
        assert_eq!(
            content_of(&message).content,
            MessageContent::Unsupported {
                description: "order".into()
            }
        );
    }

    #[test]
    fn a_poll_counts_the_votes_it_was_given() {
        let poll = wa::Message {
            poll_creation_message_v3: MessageField::some(wa::message::PollCreationMessage {
                name: Some("Where?".into()),
                options: ["Beach", "City"]
                    .into_iter()
                    .map(|name| wa::message::poll_creation_message::Option {
                        option_name: Some(name.into()),
                        ..Default::default()
                    })
                    .collect(),
                selectable_options_count: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let votes = PollVotes {
            voters: vec![
                (false, vec![option_hash("Beach")]),
                (true, vec![option_hash("City")]),
                (false, Vec::new()),
            ],
        };
        let Effect::Content(mapped) = effect(&poll, Some(&votes)) else {
            panic!("content");
        };
        let MessageContent::Poll(tally) = mapped.content else {
            panic!("a poll");
        };
        assert_eq!(tally.question, "Where?");
        assert_eq!(
            tally.options.iter().map(|o| o.votes).collect::<Vec<_>>(),
            [1, 1]
        );
        assert_eq!(tally.voters, 2);
        assert_eq!(tally.chosen, Some(vec!["City".to_owned()]));
        assert_eq!(tally.max_choices, 1);

        // Without the account's own vote, the tally does not claim one.
        let MessageContent::Poll(unknown) = content_of(&poll).content else {
            panic!("a poll");
        };
        assert_eq!(unknown.chosen, None);
    }

    #[test]
    fn a_reply_to_a_story_is_marked_and_is_not_a_quote() {
        let message = wa::Message {
            extended_text_message: MessageField::some(wa::message::ExtendedTextMessage {
                text: Some("nice".into()),
                context_info: MessageField::some(wa::ContextInfo {
                    stanza_id: Some("3EB0DDDD".into()),
                    remote_jid: Some(STATUS_CHAT.into()),
                    quoted_message: MessageField::some(wa::Message {
                        image_message: MessageField::some(image()),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mapped = content_of(&message);
        assert!(mapped.reply_to.is_none());
        let story = mapped.extras.story_reply.expect("it answers a story");
        assert_eq!(story.story.as_str(), "3EB0DDDD");
        assert_eq!(story.kind, StoryReplyKind::Image);
        assert_eq!(story.preview.as_deref(), Some("the beach"));
    }

    #[test]
    fn a_card_gives_the_whatsapp_number_when_it_has_one() {
        let vcard = "BEGIN:VCARD\nVERSION:3.0\nFN:Ana\nitem1.TEL;waid=584245550199:+58 424-5550199\nTEL;type=HOME:0212 555 01 00\nEND:VCARD";
        assert_eq!(vcard_phones(vcard), ["+584245550199", "0212 555 01 00"]);
    }

    #[test]
    fn a_pointer_survives_its_reference() {
        let pointer = MediaPointer {
            direct_path: "/v/x".into(),
            media_key: vec![1, 2, 3],
            file_sha256: vec![4],
            file_enc_sha256: vec![5],
            file_length: 10,
            class: "video".into(),
            mime_type: Some("video/mp4".into()),
            url: Some("https://mmg.whatsapp.net/v/x".into()),
            key_timestamp: Some(1_700_000_000),
        };
        assert_eq!(MediaPointer::parse(&pointer.to_ref()), Some(pointer));
        assert_eq!(
            MediaPointer::parse(&MediaRef::new("https://elsewhere")),
            None
        );
    }
}
