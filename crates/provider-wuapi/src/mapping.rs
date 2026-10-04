//! The SDK's types translated into the neutral model.
//!
//! Pure functions, no I/O: this is the part of an adapter that deserves the
//! most tests, and the easiest to test.

use client_provider::{
    Account, AccountId, AccountSettings, CalendarEvent, Chat, ChatId, ChatKind, ChatUnknown,
    ClientMessageId, ConnectionState, Contact, ContactCard, ContactId, DeliveryStatus, Direction,
    EventCall, EventPlace, GeoPoint, HistoryImport, LinkPlace, LinkStatus, LinkStep, Location,
    Media, MediaKind, MediaRef, Mention, Message, MessageContent, MessageExtras, MessageId, Poll,
    PollOption, ReplyRef, ServerMedia, StoryReplyKind, StoryReplyRef, Timestamp,
};
use wuapi::types as api;

/// The metadata key under which the adapter stores the client's message id
/// when sending, so that wuapi echoes it back on every copy of the message.
pub(crate) const CLIENT_ID_KEY: &str = "clientMessageId";

/// Prefix of the media refs of files still on WhatsApp: the rest is
/// `<size or empty>:<message id>`, see [`media_source`].
pub(crate) const ON_DEMAND_PREFIX: &str = "wuapi-media:";

/// Where a message's file is fetched from, decided by `downloaded` alone.
///
/// * `downloaded: true`: `url` is the file itself.
/// * `downloaded: false`: the file is still on WhatsApp, and `url` is the
///   API route that fetches it, which needs the API key. The ref names the
///   message instead of carrying that URL, so the request is made through
///   the SDK to the configured API host and the key cannot follow a URL
///   that came in a message. The size, when known, rides along so that a
///   file over the limit is refused before the API is asked to fetch it.
/// * no `url`: wuapi has nothing to fetch the file with.
fn media_source(message_id: &str, media: &api::MessageMedia) -> Option<MediaRef> {
    let url = media.url.as_ref()?;
    if media.downloaded {
        return Some(MediaRef::new(url.clone()));
    }
    let size = media
        .size
        .filter(|size| *size >= 0)
        .map(|size| size.to_string())
        .unwrap_or_default();
    Some(MediaRef::new(format!(
        "{ON_DEMAND_PREFIX}{size}:{message_id}"
    )))
}

/// Prefix of the media refs of a story's file still on WhatsApp:
/// `<size or empty>:<story id>`.
pub(crate) const STORY_PREFIX: &str = "wuapi-story:";

/// Prefix of the media refs of a favorite sticker's file:
/// `<size or empty>:<sticker id>`.
pub(crate) const FAVORITE_PREFIX: &str = "wuapi-favorite:";

/// What a media ref names when it is not a URL: something the API holds
/// a file for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OnDemand<'a> {
    /// The file of this message.
    Message(&'a str),
    /// The file of this story.
    Story(&'a str),
    /// The file of this favorite sticker.
    Favorite(&'a str),
}

/// Splits an on-demand media ref into its size, when known, and what it
/// names.
pub(crate) fn on_demand(reference: &str) -> Option<(Option<u64>, OnDemand<'_>)> {
    type Make = for<'a> fn(&'a str) -> OnDemand<'a>;
    let kinds: [(&str, Make); 3] = [
        (ON_DEMAND_PREFIX, |id| OnDemand::Message(id)),
        (STORY_PREFIX, |id| OnDemand::Story(id)),
        (FAVORITE_PREFIX, |id| OnDemand::Favorite(id)),
    ];
    kinds.iter().find_map(|(prefix, make)| {
        let (size, id) = reference.strip_prefix(prefix)?.split_once(':')?;
        Some((size.parse().ok(), make(id)))
    })
}

/// Prefix of the media refs that stand for a contact's profile picture.
/// They are resolved to a download URL only when the picture is wanted.
pub(crate) const PICTURE_PREFIX: &str = "picture:";

/// Parses an RFC 3339 timestamp into milliseconds since the epoch.
pub(crate) fn timestamp(value: &str) -> Option<Timestamp> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|t| Timestamp::from_millis(t.timestamp_millis()))
}

/// `Account` -> [`Account`].
pub(crate) fn account(wire: &api::Account) -> Account {
    let display_name = [&wire.name, &wire.profile_name, &wire.phone]
        .into_iter()
        .flatten()
        .find(|s| !s.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| wire.id.clone());
    Account {
        id: AccountId::new(wire.id.clone()),
        display_name,
        self_contact: wire.phone.clone().map(ContactId::new),
        phone: wire.phone.clone(),
        connection: connection(wire),
        settings: AccountSettings {
            history_import: match wire.history_sync {
                api::HistorySyncSetting::None => Some(HistoryImport::Off),
                api::HistorySyncSetting::Recent => Some(HistoryImport::Recent),
                _ => None,
            },
            server_media: server_media(&wire.media_auto_download),
            ever_linked: Some(wire.linked_at.is_some()),
        },
    }
}

/// Where linking an account stands, from its `status`, its QR code and
/// its pairing code.
///
/// `qrCodeUrl` is a PNG data URL (`data:image/png;base64,...`), decoded
/// here so that nothing downstream handles a URL. One that cannot be read
/// leaves the step at `Starting`: the next answer brings another.
pub(crate) fn link_status(wire: &api::Account) -> LinkStatus {
    use api::AccountStatus as S;
    let was_linked = wire.linked_at.is_some();
    let step = match wire.status {
        S::Ready => LinkStep::Linked,
        S::Authenticating => LinkStep::Finishing,
        S::QrReady => match (&wire.pairing_code, &wire.qr_code_url) {
            (Some(code), _) => LinkStep::TypeCode {
                code: code.clone(),
                expires_at: wire.pairing_code_expires_at.as_deref().and_then(timestamp),
            },
            (None, Some(url)) => match qr_png(url) {
                Some(png) => LinkStep::Scan { png },
                None => LinkStep::Starting,
            },
            (None, None) => LinkStep::Starting,
        },
        S::Failed => LinkStep::Stopped {
            reason: stop_reason(wire),
        },
        // A number that was `ready` and dropped is reconnecting on its
        // own; one that is down for a reason of its own has stopped.
        S::Disconnected if !wire.reconnecting && wire.disconnect_reason.is_some() => {
            LinkStep::Stopped {
                reason: stop_reason(wire),
            }
        }
        _ => LinkStep::Starting,
    };
    LinkStatus {
        account: account(wire),
        step,
        was_linked,
    }
}

/// Why linking stopped, in words.
fn stop_reason(wire: &api::Account) -> String {
    match wire.disconnect_reason.as_deref() {
        Some("link_timeout") => "The number was not linked in time.".to_owned(),
        Some("logged_out") => "The phone unlinked this device.".to_owned(),
        Some("free_limit_reached") => "This month's limit of the Free plan was reached.".to_owned(),
        Some("proxy_paused") => "Connections are paused for this organization.".to_owned(),
        Some(other) => wire
            .last_error
            .clone()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| format!("The session stopped ({other}).")),
        None => wire
            .last_error
            .clone()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or_else(|| "The session stopped.".to_owned()),
    }
}

/// The PNG inside a `data:image/png;base64,` URL, bounded: a QR code is a
/// few kilobytes.
fn qr_png(url: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    const MAX_ENCODED: usize = 2 * 1024 * 1024;
    let (head, data) = url.strip_prefix("data:")?.split_once(',')?;
    if !head.starts_with("image/png") || !head.ends_with(";base64") || data.len() > MAX_ENCODED {
        return None;
    }
    let png = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    png.starts_with(b"\x89PNG").then_some(png)
}

/// `Contact` -> [`Contact`]. The id is the contact's chat id.
pub(crate) fn contact(wire: &api::Contact) -> Contact {
    let filled = |text: &Option<String>| text.clone().filter(|text| !text.trim().is_empty());
    let mut contact = Contact::new(
        AccountId::new(wire.account_id.clone()),
        ContactId::new(wire.id.clone()),
    );
    contact.phone = filled(&wire.phone);
    contact.saved_name = filled(&wire.saved_name);
    contact.profile_name = filled(&wire.profile_name);
    contact.business_name = filled(&wire.business_name);
    contact.username = filled(&wire.username);
    contact.about = filled(&wire.about);
    contact.picture_id = filled(&wire.picture_id);
    contact.avatar = Some(MediaRef::new(format!("{PICTURE_PREFIX}{}", wire.id)));
    contact.name = Some(contact.display_name());
    // The same person by their number and by their hidden-number id:
    // messages, mentions and group participants use either.
    contact.alt_ids = [filled(&wire.phone), filled(&wire.lid)]
        .into_iter()
        .flatten()
        .filter(|id| *id != wire.id)
        .map(ContactId::new)
        .collect();
    contact
}

/// A proxy location -> [`LinkPlace`].
pub(crate) fn link_place(wire: &api::ProxyLocationItem) -> LinkPlace {
    LinkPlace {
        country: wire.country.clone(),
        country_name: wire.country_name.clone(),
        city: wire.city.clone(),
        city_name: wire.city_name.clone(),
    }
}

/// An account's `mediaAutoDownload` -> [`ServerMedia`]. A shape this
/// adapter does not know is "not said", never a guess.
fn server_media(wire: &api::MediaAutoDownloadSetting) -> Option<ServerMedia> {
    use api::MediaAutoDownloadSetting as S;
    use api::MediaAutoDownloadSettingVariant1 as Word;
    match wire {
        S::MediaAutoDownloadSettingVariant1(Word::None) => Some(ServerMedia::OnDemand),
        S::MediaAutoDownloadSettingVariant1(Word::All) => Some(ServerMedia::Everything),
        S::MediaAutoDownloadSettingVariant2(some) => Some(ServerMedia::Some {
            max_bytes: u64::try_from(some.max_bytes).unwrap_or(0),
            kinds: some
                .types
                .iter()
                .map(|kind| kind.as_str().to_owned())
                .collect(),
        }),
        _ => None,
    }
}

/// An account's `status`, `reconnecting` and `disconnectReason` ->
/// [`ConnectionState`].
///
/// `reconnecting` wins over the status: wuapi sets it while a number that
/// was `ready` is recovering on its own, and keeps accepting sends
/// meanwhile, which is exactly what `Reconnecting` means to the client.
pub(crate) fn connection(wire: &api::Account) -> ConnectionState {
    if wire.status == api::AccountStatus::Ready {
        return ConnectionState::Connected;
    }
    if wire.reconnecting {
        return ConnectionState::Reconnecting;
    }
    match wire.status {
        api::AccountStatus::Initializing
        | api::AccountStatus::QrReady
        | api::AccountStatus::Authenticating => ConnectionState::Connecting,
        _ if wire.disconnect_reason.as_deref() == Some("logged_out") => ConnectionState::LoggedOut,
        _ => ConnectionState::Disconnected {
            reason: wire
                .disconnect_reason
                .clone()
                .or_else(|| wire.last_error.clone()),
        },
    }
}

fn chat_kind(chat_type: &api::ChatType) -> Option<ChatKind> {
    match chat_type {
        api::ChatType::Direct => Some(ChatKind::Direct),
        api::ChatType::Group => Some(ChatKind::Group),
        // Channels and stories are not conversations the client shows, and
        // neither is a chat type this version of the SDK does not know.
        _ => None,
    }
}

fn status(wire: &api::Message) -> DeliveryStatus {
    use api::MessageStatus as S;
    match wire.status {
        // wuapi has it and will send it: no longer only on this device.
        S::Queued => DeliveryStatus::Accepted,
        S::Sent => DeliveryStatus::Sent,
        S::Delivered | S::Received => DeliveryStatus::Delivered,
        S::Read => DeliveryStatus::Read,
        S::Failed => DeliveryStatus::Failed {
            reason: wire
                .error
                .as_ref()
                .map(|e| {
                    if e.message.is_empty() {
                        e.code.to_string()
                    } else {
                        e.message.clone()
                    }
                })
                .unwrap_or_else(|| "The message could not be sent".to_owned()),
        },
        // A status this adapter does not know: the least committal answer.
        _ => match wire.direction {
            api::MessageDirection::Inbound => DeliveryStatus::Delivered,
            // Whatever it is, wuapi answered it: it has the message.
            _ => DeliveryStatus::Accepted,
        },
    }
}

/// What a story message says it is: [`content`] for a message of the
/// chat `stories`.
pub(crate) fn story_content(wire: &api::Message) -> Option<MessageContent> {
    content(wire)
}

fn content(wire: &api::Message) -> Option<MessageContent> {
    use api::MessageType as T;
    let media_kind = match wire.r#type {
        T::Image => Some(MediaKind::Image),
        T::Video => Some(MediaKind::Video),
        T::Audio => Some(MediaKind::Audio),
        T::Voice => Some(MediaKind::Voice),
        T::Document => Some(MediaKind::Document),
        T::Sticker => Some(MediaKind::Sticker),
        _ => None,
    };
    if let Some(kind) = media_kind {
        let mut media = Media::new(kind);
        if let Some(m) = &wire.media {
            media.source = media_source(&wire.id, m);
            media.mime_type = m.mime_type.clone();
            media.file_name = m.filename.clone();
            media.size_bytes = m.size.and_then(|size| u64::try_from(size).ok());
            // An image or a sticker sent through the API, and a received
            // file when the engine reports them; the client keeps its
            // fallbacks for the rest.
            let positive = |value: Option<i64>| {
                value
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value > 0)
            };
            (media.width, media.height) = match (positive(m.width), positive(m.height)) {
                (Some(width), Some(height)) => (Some(width), Some(height)),
                _ => (None, None),
            };
            media.duration_secs = positive(m.duration_seconds);
            // A GIF on WhatsApp is a `video` of `video/mp4` flagged to
            // play as one. The flag means nothing on anything else.
            media.gif = kind == MediaKind::Video && m.gif_playback;
        }
        media.caption = wire.text.clone().filter(|t| !t.is_empty());
        return Some(MessageContent::Media(media));
    }
    // What the type promises may be missing (a message imported with the
    // history, a field newer than this adapter): the type's name is then
    // all there is to show.
    let unreadable = || MessageContent::Unsupported {
        description: wire.r#type.as_str().to_owned(),
    };
    Some(match &wire.r#type {
        T::Text => MessageContent::text(wire.text.clone().unwrap_or_default()),
        // `text` is the emoji (empty when removed) and `replyToMessageId`
        // the message reacted to. Without a target there is nothing to do.
        T::Reaction => MessageContent::Reaction {
            target: MessageId::new(wire.reply_to_message_id.clone()?),
            emoji: wire.text.clone().unwrap_or_default(),
        },
        T::Location => wire
            .location
            .as_ref()
            .and_then(location)
            .map_or_else(unreadable, MessageContent::Location),
        // `contacts` holds every card of either type; `contact` is the
        // first one, and all there is when `contacts` is missing.
        T::Contact | T::Contacts => {
            let cards: Vec<ContactCard> = match (&wire.contacts, &wire.contact) {
                (Some(cards), _) if !cards.is_empty() => cards.iter().map(card).collect(),
                (_, Some(only)) => vec![card(only)],
                _ => Vec::new(),
            };
            if cards.is_empty() {
                unreadable()
            } else {
                MessageContent::Contacts { cards }
            }
        }
        T::Poll => wire
            .poll
            .as_ref()
            .map_or_else(unreadable, |wire| MessageContent::Poll(poll(wire))),
        T::CalendarEvent => wire
            .calendar_event
            .as_ref()
            .map_or_else(unreadable, |wire| {
                MessageContent::Event(calendar_event(wire))
            }),
        // `unknown`, and any type newer than the SDK: named, not guessed.
        _ => unreadable(),
    })
}

fn filled(text: &Option<String>) -> Option<String> {
    text.clone().filter(|text| !text.trim().is_empty())
}

/// `MessageLocation` -> [`Location`]. `None` for coordinates that are not
/// a place.
///
/// TODO(wuapi-api): the API does not say whether a location is a live
/// one, nor carries its later positions; every location is a fixed place.
fn location(wire: &api::MessageLocation) -> Option<Location> {
    Some(Location {
        point: GeoPoint::new(wire.latitude, wire.longitude)?,
        name: filled(&wire.name),
        address: filled(&wire.address),
        live: false,
    })
}

/// `MessageContact` -> [`ContactCard`]. The API reduces a card to a name
/// and one number.
fn card(wire: &api::MessageContact) -> ContactCard {
    ContactCard {
        name: wire.name.clone(),
        phones: Some(wire.phone.clone())
            .filter(|phone| !phone.trim().is_empty())
            .into_iter()
            .collect(),
    }
}

fn count(value: i64) -> u32 {
    u32::try_from(value.max(0)).unwrap_or(u32::MAX)
}

/// `MessagePoll` -> [`Poll`].
///
/// TODO(wuapi-api): the tally does not say which options the account
/// itself chose, so `chosen` is "not known" and the client keeps the vote
/// it cast.
fn poll(wire: &api::MessagePoll) -> Poll {
    Poll {
        question: wire.name.clone(),
        options: wire
            .options
            .iter()
            .map(|option| PollOption {
                name: option.name.clone(),
                votes: count(option.vote_count),
            })
            .collect(),
        max_choices: count(wire.selectable_count),
        voters: count(wire.voter_count),
        chosen: None,
    }
}

/// `MessageCalendarEvent` -> [`CalendarEvent`].
fn calendar_event(wire: &api::MessageCalendarEvent) -> CalendarEvent {
    use api::MessageCalendarEventCallType as Call;
    CalendarEvent {
        title: wire.name.clone(),
        description: filled(&wire.description),
        starts_at: wire.starts_at.as_deref().and_then(timestamp),
        ends_at: wire.ends_at.as_deref().and_then(timestamp),
        place: wire.location.as_ref().and_then(|place| {
            let place = EventPlace {
                name: filled(&place.name),
                address: filled(&place.address),
                point: match (place.latitude, place.longitude) {
                    (Some(latitude), Some(longitude)) => GeoPoint::new(latitude, longitude),
                    _ => None,
                },
            };
            (place.name.is_some() || place.address.is_some() || place.point.is_some())
                .then_some(place)
        }),
        call: match &wire.call_type {
            Some(Call::Audio) => Some(EventCall::Voice),
            Some(Call::Video) => Some(EventCall::Video),
            _ => None,
        },
        // Only a link a browser can open is kept: it is shown as one.
        join_url: filled(&wire.join_url).filter(|url| url.starts_with("https://")),
        cancelled: wire.cancelled,
    }
}

/// What rides along with a message.
///
/// TODO(wuapi-api): a received message's link preview (title,
/// description, thumbnail) is not in the API's `Message`, so `link` is
/// always empty: links are clickable, without a card.
fn extras(wire: &api::Message) -> MessageExtras {
    MessageExtras {
        // "Forwarded many times" is a forward too, whatever the wire says.
        forwarded: wire.forwarded || wire.forwarded_many_times,
        forwarded_many: wire.forwarded_many_times,
        starred: wire.starred,
        view_once: wire.view_once,
        // TODO(wuapi-api): `mentions` lists the mentioned contacts' ids,
        // with a hidden-number id already turned into the number when
        // the engine knows it, but not what stands for each of them in
        // the text (the engine has it: the `token` of `mentionedUsers`),
        // and the text of a mention made by hidden-number id carries
        // those digits, not the number's. The handle here is the likely
        // one; the client looks for the person's other ids too.
        mentions: wire
            .mentions
            .iter()
            .filter(|id| !id.trim().is_empty())
            .map(|id| {
                let id = ContactId::new(id.clone());
                Mention {
                    handle: client_provider::mention_handle(&id),
                    id,
                    name: None,
                    me: false,
                }
            })
            .collect(),
        sender_username: filled(&wire.username),
        link: None,
        // `replyToStoryId`: the message answers a story. What the story
        // showed is not on the message: the client fills that in from the
        // story it holds, and keeps the mark it wrote for its own answers.
        story_reply: wire
            .reply_to_story_id
            .as_ref()
            .filter(|id| !id.is_empty())
            .map(|id| StoryReplyRef {
                story: MessageId::new(id.clone()),
                kind: StoryReplyKind::Text,
                preview: None,
                // Somebody answered a story of the account's, or the
                // account answered somebody's.
                of_mine: wire.direction == api::MessageDirection::Inbound,
            }),
    }
}

/// `Message` -> [`Message`]. `None` for messages the client has no place
/// for: channel posts, stories, and reactions without a target.
pub(crate) fn message(wire: &api::Message) -> Option<Message> {
    chat_kind(&wire.chat_type)?;
    let content = content(wire)?;
    let direction = match wire.direction {
        api::MessageDirection::Outbound => Direction::Outgoing,
        _ => Direction::Incoming,
    };
    let reply_to = match content {
        MessageContent::Reaction { .. } => None,
        _ => wire.reply_to_message_id.as_ref().map(|id| ReplyRef {
            message_id: MessageId::new(id.clone()),
            // wuapi sends only the id; the client resolves the quote from
            // its own copy of the message.
            sender_name: None,
            preview: None,
        }),
    };
    let sent_at = wire.sent_at.as_deref().and_then(timestamp);
    Some(Message {
        id: MessageId::new(wire.id.clone()),
        client_id: wire
            .metadata
            .get(CLIENT_ID_KEY)
            .cloned()
            .map(ClientMessageId::new),
        account_id: AccountId::new(wire.account_id.clone()),
        chat_id: ChatId::new(wire.chat_id.clone()),
        sender: ContactId::new(wire.from.clone()),
        sender_name: wire.profile_name.clone().filter(|n| !n.is_empty()),
        direction,
        timestamp: sent_at
            .or_else(|| timestamp(&wire.created_at))
            .unwrap_or_default(),
        status: status(wire),
        edited: wire.edited_at.is_some(),
        deleted: wire.deleted_at.is_some(),
        extras: extras(wire),
        content,
        reply_to,
    })
}

/// `Chat` -> [`Chat`]. `None` for channels (and chat types newer than the
/// SDK), which the client does not show.
///
/// What wuapi has not observed is `null` on the wire; the neutral model
/// has no "unknown", so it becomes the quiet answer:
///
/// * `pinned`, `archived`, `muted`: `null` until WhatsApp reports a change
///   for the chat after linking. That is said in [`Chat::unknown`]: the
///   client then keeps its own value (a pin made here stays), and a chat
///   it has never heard of is not pinned, not archived, not muted.
/// * `unreadCount` is wuapi's own count (`null` or too low for chats that
///   were unread before linking), and `unread: true` with a count of 0 is
///   a chat *marked* as unread. The model only has a count, so a chat that
///   is unread without a known count gets 1: the badge and the "Unread"
///   filter work, and the number is the least that can be true.
pub(crate) fn chat(wire: &api::Chat) -> Option<Chat> {
    let kind = chat_kind(&wire.r#type)?;
    let counted = wire
        .unread_count
        .map_or(0, |n| u32::try_from(n.max(0)).unwrap_or(u32::MAX));
    let marked = u32::from(wire.unread == Some(true));
    Some(Chat {
        id: ChatId::new(wire.id.clone()),
        account_id: AccountId::new(wire.account_id.clone()),
        kind,
        // `name` is already the best one: the saved name, else the group's
        // subject or the contact's profile name.
        title: [&wire.name, &wire.saved_name, &wire.profile_name]
            .into_iter()
            .flatten()
            .find(|name| !name.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| wire.id.clone()),
        avatar: avatar(kind, &wire.id),
        unread_count: counted.max(marked),
        pinned: wire.pinned.unwrap_or(false),
        muted: wire.muted.unwrap_or(false),
        archived: wire.archived.unwrap_or(false),
        last_message: wire.last_message.as_ref().and_then(message),
        // `null` is "never observed", not "no": the client keeps what it
        // has for these.
        unknown: ChatUnknown {
            pinned: wire.pinned.is_none(),
            muted: wire.muted.is_none(),
            archived: wire.archived.is_none(),
            picture: false,
            // Neither `unread` nor `unreadCount`: wuapi does not know (a
            // chat from before it kept count, or one that only holds
            // imported history). The client counts for itself until it
            // does.
            unread: wire.unread.is_none() && wire.unread_count.is_none(),
        },
        // `null`: no picture wuapi knows of (none set, hidden, or not seen
        // yet). The client then falls back to asking now and then.
        picture_id: wire.picture_id.clone().filter(|id| !id.is_empty()),
        // `null` when the chat is not pinned, or the time is not known.
        pinned_at: wire
            .pinned_at
            .as_deref()
            .filter(|_| wire.pinned == Some(true))
            .and_then(timestamp),
    })
}

/// What a person typed as a phone number, as E.164 (`+584245550199`):
/// spaces, dashes, dots and brackets dropped, a leading `00` read as `+`.
/// `None` when it cannot be a full international number (7 to 15 digits,
/// not starting with 0).
pub(crate) fn e164(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed
        .chars()
        .any(|c| !(c.is_ascii_digit() || " -.()+".contains(c)))
    {
        return None;
    }
    let digits: String = typed.chars().filter(char::is_ascii_digit).collect();
    let digits = match typed.strip_prefix('+') {
        Some(_) => digits.as_str(),
        None => digits.strip_prefix("00").unwrap_or(&digits),
    };
    ((7..=15).contains(&digits.len()) && !digits.starts_with('0')).then(|| format!("+{digits}"))
}

/// The chat with a number the account has not talked to yet.
pub(crate) fn new_direct_chat(account: &AccountId, number: &str) -> Chat {
    Chat {
        id: ChatId::new(number),
        account_id: account.clone(),
        kind: ChatKind::Direct,
        title: number.to_owned(),
        avatar: avatar(ChatKind::Direct, number),
        unread_count: 0,
        pinned: false,
        muted: false,
        archived: false,
        last_message: None,
        unknown: ChatUnknown {
            picture: true,
            ..Default::default()
        },
        picture_id: None,
        pinned_at: None,
    }
}

fn avatar(kind: ChatKind, chat_id: &str) -> Option<MediaRef> {
    match kind {
        ChatKind::Direct => Some(MediaRef::new(format!("{PICTURE_PREFIX}{chat_id}"))),
        // Read through the same picture route, with the group's id, by
        // `fetch_avatar`; there is no ref to carry for it.
        ChatKind::Group => None,
    }
}
