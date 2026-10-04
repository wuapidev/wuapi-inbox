//! A look at what the API answers, for diagnosing a session without a
//! window: one line per chat with the fields the chat list is drawn from,
//! or one line per story with what Status is drawn from, and nothing
//! that identifies anyone.
//!
//! The lines never carry a name, a message's text or the API key. A phone
//! number is cut down to its last four digits and a group id to its last
//! four characters before the `@`.

use crate::config::LiveTransport;
use crate::mapping;
use crate::provider::WuapiProvider;
use crate::sse::{SseItem, SseParser};
use crate::stories::{ListError, Listed};
use crate::stream::{Answer, Refused, ReqwestStreamTransport, StreamBody, StreamTransport};
use client_provider::{Feature, ProviderError, ProviderResult, Timestamp};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use wuapi::types as api;

/// The `live` line: the mode asked for.
fn live_line(live: LiveTransport) -> String {
    format!(
        "live {}",
        match live {
            LiveTransport::Auto => "auto",
            LiveTransport::Stream => "stream",
            LiveTransport::Polling => "polling",
        }
    )
}

/// How a refused connect is told: the status, the API's code when the body
/// is the API's, and `Retry-After`. Never the API's words.
fn refusal_line(refused: &Refused) -> String {
    let code = serde_json::from_slice::<wuapi::types::ApiError>(&refused.body)
        .map(|error| error.code)
        .ok()
        .filter(|code| code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    let mut line = format!("connect status={}", refused.status);
    if let Some(code) = code {
        line.push_str(&format!(" code={code}"));
    }
    if let Some(wait) = refused.retry_after {
        line.push_str(&format!(" retry_after={}", wait.as_secs()));
    }
    line
}

/// What was heard in the window. Event types and counts, times: nothing
/// that was in the frames.
#[derive(Default)]
struct Heard {
    retry: Option<Duration>,
    /// When the first ping (any comment) came, from the connect.
    ping: Option<Duration>,
    cursor: bool,
    /// The connection ended before the window did, after this long.
    closed: Option<Duration>,
    types: BTreeMap<String, usize>,
}

impl Heard {
    fn lines(&self, window: Duration) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(retry) = self.retry {
            lines.push(format!("retry {}", retry.as_millis()));
        }
        lines.push(match self.ping {
            Some(at) => format!("ping within_ms={}", at.as_millis()),
            None => "ping none".to_owned(),
        });
        lines.push(format!("cursor seen={}", self.cursor));
        if let Some(after) = self.closed {
            lines.push(format!("closed after_ms={}", after.as_millis()));
        }
        lines.push(format!(
            "events {} window_ms={}",
            self.types.values().sum::<usize>(),
            window.as_millis()
        ));
        lines.extend(
            self.types
                .iter()
                .map(|(kind, count)| format!("event {kind} x{count}")),
        );
        lines
    }
}

/// An event type as a word safe to print: what the gateway names its
/// events, and nothing else.
fn type_word(name: &str) -> String {
    if !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
    {
        name.to_owned()
    } else {
        "other".to_owned()
    }
}

/// Reads the stream for `window`.
async fn listen(mut body: Box<dyn StreamBody>, window: Duration, max_frame: usize) -> Heard {
    let mut heard = Heard::default();
    let mut parser = SseParser::new(max_frame);
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + window;
    loop {
        let chunk = tokio::select! {
            chunk = body.chunk() => chunk,
            () = tokio::time::sleep_until(deadline) => return heard,
        };
        let bytes = match chunk {
            Ok(Some(bytes)) => bytes,
            _ => {
                heard.closed = Some(started.elapsed());
                return heard;
            }
        };
        for item in parser.feed(&bytes) {
            match item {
                SseItem::Retry(wait) => heard.retry.get_or_insert(wait),
                SseItem::Comment => heard.ping.get_or_insert(started.elapsed()),
                SseItem::Cursor(_) => {
                    heard.cursor = true;
                    continue;
                }
                SseItem::Event { event, id, .. } => {
                    heard.cursor |= id.is_some();
                    *heard.types.entry(type_word(&event)).or_default() += 1;
                    continue;
                }
                SseItem::Oversized => continue,
            };
        }
    }
}

/// How many chats of each account the report shows: the first page.
const CHATS: u32 = 50;

/// An id with everything but its tail removed.
pub(crate) fn short_id(id: &str) -> String {
    let (body, kind) = match id.split_once('@') {
        Some((body, _)) => (body, "group"),
        None if id.starts_with("lid:") => (id, "lid"),
        None if id.starts_with('+') => (id, "phone"),
        None => (id, "id"),
    };
    let tail: String = {
        let chars: Vec<char> = body.chars().collect();
        chars[chars.len().saturating_sub(4)..].iter().collect()
    };
    format!("{kind}:..{tail}")
}

fn flag(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "true",
        Some(false) => "false",
        None => "null",
    }
}

/// One chat as the chat-list endpoint answered it.
pub(crate) fn chat_line(wire: &api::Chat) -> String {
    let unread_count = wire
        .unread_count
        .map_or_else(|| "null".to_owned(), |count| count.to_string());
    format!(
        "chat {id} type={kind} unread={unread} unreadCount={unread_count} pinned={pinned} \
         archived={archived} muted={muted} pictureId={picture} lastMessageAt={at} \
         source=endpoint",
        id = short_id(&wire.id),
        kind = wire.r#type.as_str(),
        unread = flag(wire.unread),
        pinned = flag(wire.pinned),
        archived = flag(wire.archived),
        muted = flag(wire.muted),
        picture = if wire.picture_id.is_some() {
            "yes"
        } else {
            "no"
        },
        at = wire.last_message_at,
    )
}

/// Where a story's file is, as the API says: `none` for a story without
/// one (a text), `downloaded` when wuapi stores it, `on_whatsapp` when it
/// is fetched on first use, `missing` when there is nothing to fetch it
/// with. Never the URL.
fn media_state(wire: &api::Story) -> &'static str {
    match &wire.media {
        None => "none",
        Some(file) if file.url.is_none() => "missing",
        Some(file) if file.downloaded => "downloaded",
        Some(_) => "on_whatsapp",
    }
}

/// One story as a story route answered it: who (cut down), what kind,
/// when, whether it was seen, where its file is, and whether the client
/// shows it. No name, no text, no caption, no URL.
pub(crate) fn story_line(wire: &api::Story, now: Timestamp) -> String {
    let time = |at: &Option<String>| at.clone().unwrap_or_else(|| "null".to_owned());
    format!(
        "story {id} author={author} own={own} type={kind} status={status} posted={posted} \
         expires={expires} viewed={viewed} media={media} shown={shown}",
        id = short_id(&wire.id),
        author = wire
            .contact_id
            .as_deref()
            .map_or_else(|| "null".to_owned(), short_id),
        own = wire.own,
        kind = wire.r#type.as_str(),
        status = wire.status.as_str(),
        posted = time(&wire.posted_at),
        expires = time(&wire.expires_at),
        viewed = wire.viewed_at.is_some(),
        media = media_state(wire),
        shown = crate::stories::story(wire, now).is_some(),
    )
}

/// Why a listing failed: the HTTP status and the API's code, never its
/// words.
pub(crate) fn error_line(error: &ListError) -> String {
    match error {
        ListError::Api(error) => match error.status() {
            Some(status) => format!("error status={status} code={}", error.code()),
            None => format!("error status=none code={}", error.code()),
        },
        ListError::Messages(error) => format!(
            "error status=none code={}",
            match error {
                ProviderError::Transient(_) => "transient",
                ProviderError::RateLimited { .. } => "rate_limited",
                ProviderError::Unauthorized(_) => "unauthorized",
                ProviderError::Rejected { code, .. } => code.as_str(),
                ProviderError::Unsupported(_) => "unsupported",
                ProviderError::Protocol(_) => "invalid_response",
            }
        ),
    }
}

/// The lines of one account's stories: the summary, then a line per
/// story, then why any of what the API listed could not be read.
pub(crate) fn story_lines(
    head: &str,
    available: bool,
    answer: &Result<Listed, ListError>,
    now: Timestamp,
) -> Vec<String> {
    let stories = if available {
        "available"
    } else {
        "not_available"
    };
    let listed = match answer {
        Ok(listed) => listed,
        Err(error) => {
            return vec![format!(
                "{head} stories={stories} own=? contacts=? {}",
                error_line(error)
            )]
        }
    };
    let own = listed.stories.iter().filter(|story| story.mine).count();
    let contacts = listed.stories.len() - own;
    let unviewed = listed
        .stories
        .iter()
        .filter(|story| !story.mine && !story.viewed)
        .count();
    let mut lines = vec![format!(
        "{head} stories={stories} own={own} contacts={contacts} unviewed={unviewed} \
         authors={authors} muted_authors={muted} answered={answered} unread={unread}",
        authors = listed.muted.len(),
        muted = listed.muted.iter().filter(|(_, muted)| *muted).count(),
        answered = listed.wires.len(),
        unread = listed.unread.len(),
    )];
    lines.extend(listed.wires.iter().map(|wire| story_line(wire, now)));
    lines.extend(listed.unread.iter().map(|why| format!("unread {why}")));
    lines
}

impl WuapiProvider {
    /// How the event stream answers one connection, as lines safe to paste
    /// anywhere: the status and how long the connect took, the `retry:`
    /// hint, whether a ping and a cursor came, and the event types heard
    /// with their counts, over a window of twenty seconds. No key, no event
    /// id, no cursor, no payload and none of the API's words. It uses one
    /// of the organization's stream connections while it listens.
    pub async fn diagnose_stream(&self) -> ProviderResult<Vec<String>> {
        let mut lines = vec![
            format!("sdk wuapi {}", wuapi::VERSION),
            format!("api {}", self.base_url()),
        ];
        let endpoint = match self.config.stream_endpoint() {
            Ok(endpoint) => endpoint,
            Err(_) => {
                lines.push("stream unusable".to_owned());
                lines.push(live_line(self.config.live));
                return Ok(lines);
            }
        };
        lines.push(format!("stream {endpoint}"));
        lines.push(live_line(self.config.live));
        let transport = ReqwestStreamTransport::new(&self.config, self.key.clone())?;
        let started = Instant::now();
        match transport.connect(None).await {
            Err(_) => lines.push("connect status=none code=transient".to_owned()),
            Ok(Answer::Refused(refused)) => lines.push(refusal_line(&refused)),
            Ok(Answer::Stream(body)) => {
                lines.push(format!(
                    "connect status=200 content_type=text/event-stream took_ms={}",
                    started.elapsed().as_millis()
                ));
                let window = self.config.tuning.diagnose_window;
                let heard = listen(body, window, self.config.tuning.max_frame).await;
                lines.extend(heard.lines(window));
            }
        }
        Ok(lines)
    }

    /// Every account's stories, through the call the application makes
    /// to list them (`Provider::list_stories` and the poll both end
    /// there), as lines safe to paste anywhere: whether stories are
    /// available for the number, how many of its own and of its contacts'
    /// there are, and a line per story. Reads only: nothing is marked as
    /// seen and no author is told anything.
    pub async fn diagnose_stories(&self) -> ProviderResult<Vec<String>> {
        let mut lines = vec![
            format!("sdk wuapi {}", wuapi::VERSION),
            format!("api {}", self.base_url()),
        ];
        let accounts = self.client().accounts().await?;
        lines.push(format!("accounts {}", accounts.len()));
        for account in &accounts {
            let now = Timestamp::now();
            let answer = self.client().list_stories(&account.id, now).await;
            let head = format!(
                "account {id} status={status} connection={state:?}",
                id = short_id(&account.id),
                status = account.status.as_str(),
                state = mapping::connection(account),
            );
            let available = !self.client().missing.is(Feature::Stories, &account.id);
            lines.extend(story_lines(&head, available, &answer, now));
        }
        Ok(lines)
    }

    /// The first page of every account's chats, through the same calls
    /// the application makes, as lines safe to paste anywhere.
    pub async fn diagnose_chats(&self) -> ProviderResult<Vec<String>> {
        let mut lines = vec![
            format!("sdk wuapi {}", wuapi::VERSION),
            format!("api {}", self.base_url()),
        ];
        let accounts = self.client().accounts().await?;
        lines.push(format!("accounts {}", accounts.len()));
        for account in &accounts {
            let state = mapping::connection(account);
            match self.client().chats(&account.id, None, CHATS).await {
                Ok(page) => {
                    lines.push(format!(
                        "account {id} status={status} connection={state:?} chats={count} \
                         more={more} source=endpoint",
                        id = short_id(&account.id),
                        status = account.status.as_str(),
                        count = page.items.len(),
                        more = page.next_cursor.is_some(),
                    ));
                    lines.extend(page.items.iter().map(chat_line));
                }
                Err(error) => lines.push(format!(
                    "account {id} status={status} connection={state:?} chats=? error={error}",
                    id = short_id(&account.id),
                    status = account.status.as_str(),
                )),
            }
        }
        Ok(lines)
    }
}
