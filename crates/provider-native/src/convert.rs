//! Between what the archive holds and the neutral model, and the messages
//! this provider writes itself. No I/O.

use crate::archive::{StoredChat, StoredContact, StoredMessage};
use crate::ids::{self, is_group, phone_of, STATUS_CHAT};
use crate::mapping::{self, Effect, MediaPointer, PollVotes};
use client_provider::{
    AccountId, Chat, ChatId, ChatKind, ChatUnknown, ClientMessageId, Contact, ContactId,
    DeliveryStatus, Direction, FilePreview, MediaKind, Message, MessageContent, MessageId, Story,
    StoryBody, StoryFont, StoryStyle, SystemEvent, Timestamp,
};
use sha2::{Digest, Sha256};
use whatsapp_rust::buffa::{Message as _, MessageField};
use whatsapp_rust::waproto::whatsapp as wa;
use whatsapp_rust::Jid;

/// What a row holds when it is a notice written here (a missed call,
/// somebody joined) and not a message that travelled: this mark, then the
/// notice as JSON.
const NOTICE: &[u8] = b"\0notice:";

/// A status as its rank (see [`DeliveryStatus::rank`]).
pub fn status_of(rank: u8) -> DeliveryStatus {
    match rank {
        0 | 1 => DeliveryStatus::Pending,
        2 => DeliveryStatus::Accepted,
        3 => DeliveryStatus::Sent,
        4 => DeliveryStatus::Delivered,
        _ => DeliveryStatus::Read,
    }
}

/// The rank of "WhatsApp has it".
pub const SENT: u8 = 3;
/// The rank of "on the other phone" (and of a received message).
pub const DELIVERED: u8 = 4;
/// The rank of "read".
pub const READ: u8 = 5;

/// A message as it is written to the archive.
pub fn encode(message: &wa::Message) -> Vec<u8> {
    message.encode_to_vec()
}

/// A message as it was written to the archive.
pub fn decode(proto: &[u8]) -> Option<wa::Message> {
    wa::Message::decode_from_slice(proto).ok()
}

/// A notice as it is written to the archive.
pub fn encode_notice(notice: &SystemEvent) -> Vec<u8> {
    let mut bytes = NOTICE.to_vec();
    bytes.extend(serde_json::to_vec(notice).unwrap_or_default());
    bytes
}

fn decode_notice(proto: &[u8]) -> Option<SystemEvent> {
    serde_json::from_slice(proto.strip_prefix(NOTICE)?).ok()
}

/// True for a row whose message is a poll: its tally is worth reading.
pub fn is_poll(stored: &StoredMessage) -> bool {
    decode(&stored.proto).is_some_and(|message| {
        let base = mapping::unwrap(&message);
        base.poll_creation_message.is_set()
            || base.poll_creation_message_v2.is_set()
            || base.poll_creation_message_v3.is_set()
    })
}

/// A stored message in the neutral model. `None` for a row that shows
/// nothing in a conversation.
pub fn to_message(
    account: &AccountId,
    stored: &StoredMessage,
    votes: Option<&PollVotes>,
) -> Option<Message> {
    let (content, reply_to, mut extras) = if stored.deleted {
        (MessageContent::text(""), None, Default::default())
    } else if let Some(notice) = decode_notice(&stored.proto) {
        (MessageContent::System(notice), None, Default::default())
    } else {
        match mapping::effect(&decode(&stored.proto)?, votes) {
            Effect::Content(mapped) => (mapped.content, mapped.reply_to, mapped.extras),
            _ => return None,
        }
    };
    extras.starred = stored.starred;
    // In a chat, somebody else's answer to a status answers the account's.
    if let Some(story) = &mut extras.story_reply {
        story.of_mine = !stored.from_me;
    }
    Some(Message {
        id: MessageId::new(stored.id.clone()),
        client_id: stored.client_id.clone().map(ClientMessageId::new),
        account_id: account.clone(),
        chat_id: ChatId::new(stored.chat.clone()),
        sender: ContactId::new(stored.sender.clone()),
        sender_name: stored.sender_name.clone(),
        direction: if stored.from_me {
            Direction::Outgoing
        } else {
            Direction::Incoming
        },
        timestamp: Timestamp::from_millis(stored.timestamp_ms),
        content,
        reply_to,
        status: if stored.from_me {
            status_of(stored.status)
        } else {
            status_of(stored.status.max(DELIVERED))
        },
        edited: stored.edited,
        deleted: stored.deleted,
        extras,
    })
}

/// A stored message of the status list as a story.
pub fn to_story(account: &AccountId, stored: &StoredMessage, views: Option<u32>) -> Option<Story> {
    if stored.deleted || stored.chat != STATUS_CHAT {
        return None;
    }
    let message = decode(&stored.proto)?;
    let base = mapping::unwrap(&message);
    let body = if let Some(media) = mapping::media_of(base) {
        StoryBody::Media(media)
    } else if let Some(text) = base.extended_text_message.as_option() {
        StoryBody::Text {
            text: text.text.clone().unwrap_or_default(),
            style: StoryStyle {
                background: text
                    .background_argb
                    .map(|argb| argb & 0xFF_FF_FF)
                    .unwrap_or(StoryStyle::default().background),
                font: StoryFont(text.font.map(|font| font as i32 as u8).unwrap_or(0)),
            },
        }
    } else {
        StoryBody::Text {
            text: base.conversation.clone()?,
            style: StoryStyle::default(),
        }
    };
    let mentions = match mapping::effect(&message, None) {
        Effect::Content(mapped) => mapped.extras.mentions,
        _ => Vec::new(),
    };
    Some(Story {
        id: MessageId::new(stored.id.clone()),
        client_id: stored.client_id.clone().map(ClientMessageId::new),
        account_id: account.clone(),
        author: ContactId::new(stored.sender.clone()),
        author_name: stored.sender_name.clone(),
        mine: stored.from_me,
        posted_at: Timestamp::from_millis(stored.timestamp_ms),
        expires_at: None,
        body,
        mentions,
        viewed: !stored.from_me && stored.status >= READ,
        view_count: if stored.from_me { views } else { None },
    })
}

/// A person in the neutral model, from what the archive knows and the
/// other id they go by.
pub fn to_contact(account: &AccountId, known: &StoredContact, alt: Option<String>) -> Contact {
    let phone = phone_of(&known.id).or_else(|| alt.as_deref().and_then(phone_of));
    Contact {
        name: known
            .saved_name
            .clone()
            .or_else(|| known.business_name.clone())
            .or_else(|| known.profile_name.clone()),
        phone,
        saved_name: known.saved_name.clone(),
        profile_name: known.profile_name.clone(),
        business_name: known.business_name.clone(),
        username: known.username.clone(),
        alt_ids: alt.into_iter().map(ContactId::new).collect(),
        ..Contact::new(account.clone(), ContactId::new(known.id.clone()))
    }
}

/// A chat in the neutral model. `person` is what is known of a direct
/// chat's contact, `last` its newest message that a conversation shows.
pub fn to_chat(
    account: &AccountId,
    stored: &StoredChat,
    person: Option<&StoredContact>,
    last: Option<Message>,
    now_ms: i64,
) -> Chat {
    let group = is_group(&stored.id);
    let named = |name: &Option<String>| name.clone().filter(|text| !text.trim().is_empty());
    let title = if group {
        named(&stored.name).unwrap_or_else(|| "Group".to_owned())
    } else {
        person
            .and_then(|person| {
                named(&person.saved_name)
                    .or_else(|| named(&person.business_name))
                    .or_else(|| named(&person.profile_name))
            })
            .or_else(|| named(&stored.name))
            .unwrap_or_else(|| stored.id.clone())
    };
    let muted = match stored.muted_until_ms {
        Some(-1) => true,
        Some(until) => until > now_ms,
        None => false,
    };
    Chat {
        id: ChatId::new(stored.id.clone()),
        account_id: account.clone(),
        kind: if group {
            ChatKind::Group
        } else {
            ChatKind::Direct
        },
        title,
        avatar: None,
        // A chat flagged as unread shows a badge without a number; the
        // model has one number, so the flag counts as one message.
        unread_count: stored.unread.max(u32::from(stored.marked_unread)),
        pinned: stored.pinned_at_ms.is_some(),
        muted,
        archived: stored.archived,
        last_message: last,
        unknown: ChatUnknown {
            picture: stored.picture_id.is_none(),
            ..ChatUnknown::default()
        },
        picture_id: stored.picture_id.clone(),
        pinned_at: stored
            .pinned_at_ms
            .filter(|at| *at > 0)
            .map(Timestamp::from_millis),
    }
}

/// An address the way the wire writes it, without its device.
pub fn wire(jid: &Jid) -> String {
    jid.to_non_ad_string()
}

/// What a message says about the one it answers, the people it mentions
/// and whether it was passed on. `None` when it says none of that.
pub fn context(
    quoted: Option<(&StoredMessage, Option<&Jid>)>,
    quoted_chat: Option<&Jid>,
    mentions: &[Jid],
    forwarded: bool,
) -> Option<wa::ContextInfo> {
    if quoted.is_none() && mentions.is_empty() && !forwarded {
        return None;
    }
    let mut context = wa::ContextInfo {
        mentioned_jid: mentions.iter().map(wire).collect(),
        ..Default::default()
    };
    if forwarded {
        context.is_forwarded = Some(true);
        context.forwarding_score = Some(1);
    }
    if let Some((stored, author)) = quoted {
        context.stanza_id = Some(stored.id.clone());
        context.participant = author.map(wire);
        // A story is quoted from the status list, which is how the other
        // side knows the message answers one.
        context.remote_jid = quoted_chat.map(wire);
        if let Some(message) = decode(&stored.proto) {
            context.quoted_message = MessageField::some(quotable(&message));
        }
    }
    Some(context)
}

/// A message as another one quotes it: without its own quote, so that
/// quotes do not nest.
fn quotable(message: &wa::Message) -> wa::Message {
    let mut quoted = mapping::unwrap(message).clone();
    if let Some(text) = quoted.extended_text_message.as_option_mut() {
        text.context_info = MessageField::none();
    }
    quoted.message_context_info = MessageField::none();
    quoted
}

/// A text, plain when nothing rides along.
pub fn text_message(body: &str, context: Option<wa::ContextInfo>) -> wa::Message {
    match context {
        None => wa::Message {
            conversation: Some(body.to_owned()),
            ..Default::default()
        },
        Some(context) => wa::Message {
            extended_text_message: MessageField::some(wa::message::ExtendedTextMessage {
                text: Some(body.to_owned()),
                context_info: MessageField::some(context),
                ..Default::default()
            }),
            ..Default::default()
        },
    }
}

/// A file that was uploaded, as the message that carries it.
pub fn media_message(
    pointer: &MediaPointer,
    kind: MediaKind,
    mime_type: Option<&str>,
    caption: Option<&str>,
    file_name: Option<&str>,
    gif: bool,
    context: Option<wa::ContextInfo>,
) -> wa::Message {
    let mime = mime_type
        .map(str::to_owned)
        .or_else(|| pointer.mime_type.clone());
    let caption = caption.filter(|text| !text.is_empty()).map(str::to_owned);
    let context = context.map(MessageField::some).unwrap_or_default();
    let (url, path) = (pointer.url.clone(), Some(pointer.direct_path.clone()));
    let (key, sha, enc) = (
        Some(pointer.media_key.clone()),
        Some(pointer.file_sha256.clone()),
        Some(pointer.file_enc_sha256.clone()),
    );
    let (length, stamp) = (Some(pointer.file_length), pointer.key_timestamp);
    match kind {
        MediaKind::Image => wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                url,
                mimetype: mime,
                caption,
                file_sha256: sha,
                file_length: length,
                media_key: key,
                file_enc_sha256: enc,
                direct_path: path,
                media_key_timestamp: stamp,
                context_info: context,
                ..Default::default()
            }),
            ..Default::default()
        },
        MediaKind::Video => wa::Message {
            video_message: MessageField::some(wa::message::VideoMessage {
                url,
                mimetype: mime,
                caption,
                file_sha256: sha,
                file_length: length,
                media_key: key,
                file_enc_sha256: enc,
                direct_path: path,
                media_key_timestamp: stamp,
                gif_playback: gif.then_some(true),
                context_info: context,
                ..Default::default()
            }),
            ..Default::default()
        },
        MediaKind::Audio | MediaKind::Voice => wa::Message {
            audio_message: MessageField::some(wa::message::AudioMessage {
                url,
                mimetype: mime,
                file_sha256: sha,
                file_length: length,
                media_key: key,
                file_enc_sha256: enc,
                direct_path: path,
                media_key_timestamp: stamp,
                ptt: Some(kind == MediaKind::Voice),
                context_info: context,
                ..Default::default()
            }),
            ..Default::default()
        },
        MediaKind::Sticker => wa::Message {
            sticker_message: MessageField::some(wa::message::StickerMessage {
                url,
                mimetype: mime,
                file_sha256: sha,
                file_length: length,
                media_key: key,
                file_enc_sha256: enc,
                direct_path: path,
                media_key_timestamp: stamp,
                context_info: context,
                ..Default::default()
            }),
            ..Default::default()
        },
        MediaKind::Document => wa::Message {
            document_message: MessageField::some(wa::message::DocumentMessage {
                url,
                mimetype: mime,
                title: file_name.map(str::to_owned),
                file_name: file_name.map(str::to_owned),
                caption,
                file_sha256: sha,
                file_length: length,
                media_key: key,
                file_enc_sha256: enc,
                direct_path: path,
                media_key_timestamp: stamp,
                context_info: context,
                ..Default::default()
            }),
            ..Default::default()
        },
    }
}

/// Puts on a document the picture the other side shows before they
/// download it, and how many pages it has. Nothing for other kinds.
pub fn with_preview(mut message: wa::Message, preview: Option<&FilePreview>) -> wa::Message {
    let (Some(preview), Some(document)) = (preview, message.document_message.as_option_mut())
    else {
        return message;
    };
    if !preview.jpeg.is_empty() {
        document.jpeg_thumbnail = Some(preview.jpeg.clone());
        document.thumbnail_width = preview.width;
        document.thumbnail_height = preview.height;
    }
    document.page_count = preview.pages;
    message
}

/// What WhatsApp stores a kind of file as.
pub fn class_of(kind: MediaKind) -> &'static str {
    match kind {
        MediaKind::Image => "image",
        MediaKind::Video => "video",
        MediaKind::Audio | MediaKind::Voice => "audio",
        MediaKind::Document => "document",
        MediaKind::Sticker => "sticker",
    }
}

/// A new poll. Its secret (what votes are encrypted under) is made from
/// the key the poll is sent under, so that the same send repeated is the
/// same poll, votes included.
pub fn poll_message(
    question: &str,
    options: &[String],
    max_choices: u32,
    client_id: &ClientMessageId,
) -> wa::Message {
    let mut hasher = Sha256::new();
    hasher.update(b"wuapi-inbox poll secret\0");
    hasher.update(client_id.as_str().as_bytes());
    let secret = hasher.finalize().to_vec();
    let poll = wa::message::PollCreationMessage {
        name: Some(question.to_owned()),
        options: options
            .iter()
            .map(|name| wa::message::poll_creation_message::Option {
                option_name: Some(name.clone()),
                ..Default::default()
            })
            .collect(),
        selectable_options_count: Some(max_choices),
        ..Default::default()
    };
    let mut message = wa::Message {
        message_context_info: MessageField::some(wa::MessageContextInfo {
            message_secret: Some(secret),
            ..Default::default()
        }),
        ..Default::default()
    };
    // WhatsApp names a poll by how many answers it takes: one, or any.
    if max_choices == 1 {
        message.poll_creation_message_v3 = MessageField::some(poll);
    } else {
        message.poll_creation_message = MessageField::some(poll);
    }
    message
}

/// The secret a poll's votes are encrypted under.
pub fn poll_secret(message: &wa::Message) -> Option<Vec<u8>> {
    message
        .message_context_info
        .as_option()
        .and_then(|info| info.message_secret.clone())
        .or_else(|| {
            mapping::unwrap(message)
                .message_context_info
                .as_option()
                .and_then(|info| info.message_secret.clone())
        })
        .filter(|secret| !secret.is_empty())
}

/// A text story: words on a colour.
pub fn story_text(text: &str, style: StoryStyle) -> wa::Message {
    use whatsapp_rust::buffa::Enumeration as _;
    wa::Message {
        extended_text_message: MessageField::some(wa::message::ExtendedTextMessage {
            text: Some(text.to_owned()),
            background_argb: Some(0xFF00_0000 | (style.background & 0xFF_FF_FF)),
            text_argb: Some(0xFFFF_FFFF),
            font: wa::message::extended_text_message::FontType::from_i32(i32::from(style.font.0)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// The id of a row that is a notice about a call.
pub fn call_notice_id(call: &str) -> String {
    format!("call-{call}")
}

/// The neutral id of a session's own person: its number when it has one.
pub fn own_id(pn: Option<&Jid>, lid: Option<&Jid>) -> Option<String> {
    pn.or(lid).map(ids::id_of)
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_provider::SystemKind;

    fn stored(proto: Vec<u8>) -> StoredMessage {
        StoredMessage {
            chat: "+584245550199".into(),
            id: "3EB0AA".into(),
            sender: "+584245550199".into(),
            sender_name: Some("Ana".into()),
            from_me: false,
            timestamp_ms: 1_700_000_000_000,
            status: DELIVERED,
            starred: true,
            edited: false,
            deleted: false,
            client_id: None,
            proto,
        }
    }

    fn account() -> AccountId {
        AccountId::new("native:1")
    }

    #[test]
    fn a_document_goes_with_its_preview_and_a_picture_without() {
        let pointer = MediaPointer {
            url: Some("https://mmg.whatsapp.net/d".into()),
            direct_path: "/v/t62/doc".into(),
            media_key: vec![1; 32],
            file_sha256: vec![2; 32],
            file_enc_sha256: vec![3; 32],
            file_length: 1234,
            class: "document".into(),
            mime_type: Some("application/pdf".into()),
            key_timestamp: None,
        };
        let preview = FilePreview {
            jpeg: vec![0xff, 0xd8, 9],
            width: Some(480),
            height: Some(250),
            pages: Some(3),
        };
        let document = media_message(
            &pointer,
            MediaKind::Document,
            None,
            None,
            Some("menu.pdf"),
            false,
            None,
        );
        let sent = with_preview(document, Some(&preview));
        let document = sent.document_message.as_option().unwrap();
        assert_eq!(
            document.jpeg_thumbnail.as_deref(),
            Some(&[0xff, 0xd8, 9][..])
        );
        assert_eq!(document.thumbnail_width, Some(480));
        assert_eq!(document.page_count, Some(3));
        // And it reads back as it went.
        let read = mapping::media_of(&sent).unwrap();
        assert_eq!(read.pages, Some(3));
        assert!(read.thumbnail.is_some());

        let image = media_message(&pointer, MediaKind::Image, None, None, None, false, None);
        assert_eq!(with_preview(image.clone(), Some(&preview)), image);
    }

    #[test]
    fn a_row_reads_back_as_the_message_it_was() {
        let row = stored(encode(&text_message("hello", None)));
        let message = to_message(&account(), &row, None).unwrap();
        assert_eq!(message.content, MessageContent::text("hello"));
        assert_eq!(message.direction, Direction::Incoming);
        assert_eq!(message.status, DeliveryStatus::Delivered);
        assert!(message.extras.starred);
        assert_eq!(message.sender_name.as_deref(), Some("Ana"));
    }

    #[test]
    fn a_message_taken_back_keeps_its_place_and_loses_its_words() {
        let mut row = stored(encode(&text_message("secret", None)));
        row.deleted = true;
        let message = to_message(&account(), &row, None).unwrap();
        assert!(message.deleted);
        assert_eq!(message.content, MessageContent::text(""));
    }

    #[test]
    fn a_notice_written_here_reads_back_as_a_notice() {
        let notice = SystemEvent {
            kind: SystemKind::MissedVoiceCall,
            actor: None,
            targets: Vec::new(),
            detail: None,
        };
        let row = stored(encode_notice(&notice));
        assert_eq!(
            to_message(&account(), &row, None).unwrap().content,
            MessageContent::System(notice)
        );
    }

    #[test]
    fn an_answer_to_a_status_says_whose_status_it_was() {
        let answer = text_message(
            "nice",
            Some(wa::ContextInfo {
                stanza_id: Some("3EB0DDDD".into()),
                remote_jid: Some(STATUS_CHAT.into()),
                ..Default::default()
            }),
        );
        let theirs = to_message(&account(), &stored(encode(&answer)), None).unwrap();
        let story = theirs.extras.story_reply.expect("it answers a status");
        assert!(story.of_mine, "somebody answered the account's status");
        assert!(theirs.reply_to.is_none());

        let mine = to_message(
            &account(),
            &StoredMessage {
                from_me: true,
                ..stored(encode(&answer))
            },
            None,
        )
        .unwrap();
        assert!(!mine.extras.story_reply.unwrap().of_mine);
    }

    #[test]
    fn a_reply_quotes_without_nesting_and_names_who_it_mentions() {
        let first = stored(encode(&text_message(
            "one",
            context(
                None,
                None,
                &["584125550100@s.whatsapp.net".parse().unwrap()],
                false,
            ),
        )));
        let author: Jid = "584245550199@s.whatsapp.net".parse().unwrap();
        let reply = text_message(
            "two",
            context(Some((&first, Some(&author))), None, &[], true),
        );
        let info = reply
            .extended_text_message
            .context_info
            .as_option()
            .unwrap();
        assert_eq!(info.stanza_id.as_deref(), Some("3EB0AA"));
        assert_eq!(
            info.participant.as_deref(),
            Some("584245550199@s.whatsapp.net")
        );
        assert_eq!(info.is_forwarded, Some(true));
        let quoted = info.quoted_message.as_option().unwrap();
        assert!(
            !quoted.extended_text_message.context_info.is_set(),
            "no quote in a quote"
        );
        assert_eq!(quoted.extended_text_message.text.as_deref(), Some("one"));
        assert_eq!(info.remote_jid, None, "a quote of the same chat names none");

        // An answer in private names the group its quote is from.
        let group: Jid = "120363000000000001@g.us".parse().unwrap();
        let private = text_message(
            "three",
            context(Some((&first, Some(&author))), Some(&group), &[], false),
        );
        let info = private
            .extended_text_message
            .context_info
            .as_option()
            .unwrap();
        assert_eq!(info.remote_jid.as_deref(), Some("120363000000000001@g.us"));
        assert_eq!(
            info.participant.as_deref(),
            Some("584245550199@s.whatsapp.net")
        );

        let row = stored(encode(&reply));
        let message = to_message(&account(), &row, None).unwrap();
        assert_eq!(message.reply_to.unwrap().preview.as_deref(), Some("one"));
        assert!(message.extras.forwarded);
    }

    #[test]
    fn the_same_key_makes_the_same_poll() {
        let key = ClientMessageId::new("k");
        let options = vec!["a".to_owned(), "b".to_owned()];
        let one = poll_message("q", &options, 1, &key);
        assert_eq!(one, poll_message("q", &options, 1, &key));
        assert_eq!(poll_secret(&one).unwrap().len(), 32);
        assert!(one.poll_creation_message_v3.is_set());
        assert!(poll_message("q", &options, 0, &key)
            .poll_creation_message
            .is_set());
        assert!(is_poll(&stored(encode(&one))));
    }

    #[test]
    fn a_text_story_keeps_its_colour_and_font() {
        let style = StoryStyle {
            background: 0x12_34_56,
            font: StoryFont(8),
        };
        let mut row = stored(encode(&story_text("today", style)));
        row.chat = STATUS_CHAT.into();
        let story = to_story(&account(), &row, None).unwrap();
        assert_eq!(
            story.body,
            StoryBody::Text {
                text: "today".into(),
                style
            }
        );
        assert!(!story.mine && !story.viewed);
        row.status = READ;
        assert!(to_story(&account(), &row, None).unwrap().viewed);
    }

    #[test]
    fn a_chat_is_named_after_its_person_and_muted_only_while_it_lasts() {
        let chat = StoredChat {
            id: "+584245550199".into(),
            muted_until_ms: Some(2_000),
            marked_unread: true,
            pinned_at_ms: Some(0),
            ..StoredChat::default()
        };
        let person = StoredContact {
            id: chat.id.clone(),
            profile_name: Some("Ana P.".into()),
            saved_name: Some("Ana".into()),
            ..StoredContact::default()
        };
        let shown = to_chat(&account(), &chat, Some(&person), None, 1_000);
        assert_eq!(shown.title, "Ana");
        assert!(shown.muted && shown.pinned);
        assert_eq!(shown.pinned_at, None, "a pin without a time");
        assert_eq!(shown.unread_count, 1);
        assert!(!to_chat(&account(), &chat, None, None, 3_000).muted);
        assert_eq!(
            to_chat(&account(), &chat, None, None, 0).title,
            "+584245550199"
        );
    }
}
