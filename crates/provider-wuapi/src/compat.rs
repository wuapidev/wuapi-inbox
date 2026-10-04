//! Answers of a backend older than the SDK.
//!
//! TEMPORARY(response-compat): the generated types read a response field
//! that the API added later as required: a `Message` without
//! `forwardedManyTimes` (0.12.0), a message's `media` without
//! `gifPlayback` (0.12.0) or an `Account` without `imageQuality` (0.9.0)
//! does not decode at all. The SDK is published when the spec is merged,
//! and the backend is deployed after that: in between, every message,
//! every chat (it carries its last message) and every account would be
//! unreadable, and with them the whole application, for the sake of
//! fields that have an obvious "not said".
//!
//! So the calls whose answers carry a `Message` or an `Account` are made
//! here: the same requests the generated methods build (the SDK's
//! [`RequestParts`] are its documented way to do that), through the SDK's
//! own HTTP client (its key, timeouts and error handling), decoded into
//! the SDK's own types after the missing fields were given the value an
//! older backend means by leaving them out. Nothing else differs.
//!
//! Remove this module, and call the generated methods again (each
//! function names its own), once the backend that sends those fields is
//! live in production, or once the SDK reads a field added later as
//! optional.

use serde::de::{DeserializeOwned, Error as _, IgnoredAny};
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use wuapi::types as api;
use wuapi::{encode_path, CursorPage, HttpClient, Method, Paginator, Request, RequestParts};

/// A response read with the fields an older backend leaves out filled in.
pub(crate) struct Lenient<T>(pub(crate) T);

impl<'de, T: DeserializeOwned> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        fill(&mut value);
        serde_json::from_value(value)
            .map(Lenient)
            .map_err(D::Error::custom)
    }
}

impl<P: CursorPage> CursorPage for Lenient<P> {
    type Item = P::Item;

    fn next_cursor(&self) -> Option<&str> {
        self.0.next_cursor()
    }

    fn into_items(self) -> Vec<Self::Item> {
        self.0.into_items()
    }
}

/// Gives every message and account in `value` the fields a backend from
/// before them does not send, with what their absence means: not
/// forwarded many times, not a GIF, the default image quality.
pub(crate) fn fill(value: &mut Value) {
    match value {
        Value::Array(items) => items.iter_mut().for_each(fill),
        Value::Object(map) => {
            map.values_mut().for_each(fill);
            match map.get("object").and_then(Value::as_str) {
                Some("message") => {
                    map.entry("forwardedManyTimes")
                        .or_insert(Value::Bool(false));
                    if let Some(Value::Object(media)) = map.get_mut("media") {
                        media.entry("gifPlayback").or_insert(Value::Bool(false));
                    }
                }
                Some("account") => {
                    map.entry("imageQuality")
                        .or_insert_with(|| Value::String("standard".to_owned()));
                }
                _ => {}
            }
        }
        _ => {}
    }
}

fn account_path(account: &str, tail: &str) -> String {
    format!("/v1/accounts/{}{tail}", encode_path(account))
}

fn message_path(message: &str, tail: &str) -> String {
    format!("/v1/messages/{}{tail}", encode_path(message))
}

/// `accounts().list`
pub(crate) fn accounts(http: &HttpClient, limit: i64) -> Paginator<Lenient<api::AccountList>> {
    let parts = RequestParts::new(Method::Get, "/v1/accounts")
        .query("limit", limit)
        .retryable();
    Paginator::new(http.clone(), parts, "cursor", None)
}

/// `accounts().create`
pub(crate) fn create_account(
    http: &HttpClient,
    params: &api::AccountsCreateParams,
) -> Request<Lenient<api::Account>> {
    http.request(
        RequestParts::new(Method::Post, "/v1/accounts")
            .body(params)
            .keyed(),
    )
}

/// `accounts().get`
pub(crate) fn account(http: &HttpClient, account: &str) -> Request<Lenient<api::Account>> {
    http.request(RequestParts::new(Method::Get, account_path(account, "")).retryable())
}

/// `accounts().update`
pub(crate) fn update_account(
    http: &HttpClient,
    account: &str,
    params: &api::AccountsUpdateParams,
) -> Request<Lenient<api::Account>> {
    http.request(RequestParts::new(Method::Patch, account_path(account, "")).body(params))
}

/// `accounts().reconnect`
pub(crate) fn reconnect(http: &HttpClient, account: &str) -> Request<Lenient<api::Account>> {
    http.request(RequestParts::new(Method::Post, account_path(account, "/reconnect")).keyed())
}

/// `accounts().logout`
pub(crate) fn logout(http: &HttpClient, account: &str) -> Request<Lenient<api::Account>> {
    http.request(RequestParts::new(Method::Post, account_path(account, "/logout")).keyed())
}

/// `chats().list`, one page: a chat carries its last message.
pub(crate) fn chats(
    http: &HttpClient,
    account: &str,
    limit: i64,
    cursor: Option<String>,
) -> Paginator<Lenient<api::ChatList>> {
    let parts = RequestParts::new(Method::Get, account_path(account, "/chats"))
        .query("limit", limit)
        .retryable();
    Paginator::new(http.clone(), parts, "cursor", cursor)
}

/// `chats().get`: a chat carries its last message.
pub(crate) fn chat(http: &HttpClient, account: &str, chat: &str) -> Request<Lenient<api::Chat>> {
    let tail = format!("/chats/{}", encode_path(chat));
    http.request(RequestParts::new(Method::Get, account_path(account, &tail)).retryable())
}

/// `messages().list`, one page.
pub(crate) fn messages(
    http: &HttpClient,
    account: Option<&str>,
    chat: Option<&str>,
    limit: i64,
    cursor: Option<String>,
) -> Paginator<Lenient<api::MessageList>> {
    let parts = RequestParts::new(Method::Get, "/v1/messages")
        .query_opt("accountId", account)
        .query_opt("chatId", chat)
        .query("limit", limit)
        .retryable();
    Paginator::new(http.clone(), parts, "cursor", cursor)
}

/// `messages().get`
pub(crate) fn message(http: &HttpClient, message: &str) -> Request<Lenient<api::Message>> {
    http.request(RequestParts::new(Method::Get, message_path(message, "")).retryable())
}

/// `messages().send`
pub(crate) fn send(
    http: &HttpClient,
    params: impl Into<api::MessagesSendParams>,
) -> Request<Lenient<api::Message>> {
    let params = params.into();
    http.request(
        RequestParts::new(Method::Post, "/v1/messages")
            .body(&params)
            .keyed(),
    )
}

/// `messages().vote`
pub(crate) fn vote(
    http: &HttpClient,
    message: &str,
    params: &api::MessagesVoteParams,
) -> Request<Lenient<api::Message>> {
    http.request(
        RequestParts::new(Method::Post, message_path(message, "/vote"))
            .body(params)
            .keyed(),
    )
}

/// `messages().edit`, `messages().star` and `messages().unstar` answer
/// the message, which nobody reads: it is not decoded at all.
pub(crate) fn edit(
    http: &HttpClient,
    message: &str,
    params: &api::MessagesEditParams,
) -> Request<IgnoredAny> {
    http.request(RequestParts::new(Method::Patch, message_path(message, "")).body(params))
}

/// `messages().star` or `messages().unstar`
pub(crate) fn star(http: &HttpClient, message: &str, starred: bool) -> Request<IgnoredAny> {
    let tail = if starred { "/star" } else { "/unstar" };
    http.request(RequestParts::new(Method::Post, message_path(message, tail)).keyed())
}

/// `stories().create`: the answer is the story's message.
pub(crate) fn post_story(
    http: &HttpClient,
    account: &str,
    params: impl Into<api::StoriesCreateParams>,
) -> Request<Lenient<api::Message>> {
    let params = params.into();
    http.request(
        RequestParts::new(Method::Post, account_path(account, "/stories"))
            .body(&params)
            .keyed(),
    )
}

// ----- stories ------------------------------------------------------------
//
// Not temporary, unlike the rest of this module: a list of stories is
// read one story at a time. The generated page type reads all of them or
// none, so one story the SDK cannot read (a field of another type, one a
// backend leaves out) would leave Status empty for everybody, with
// nothing said. Read apart, that story is one story less, and why is
// kept for `--diagnose stories`.

/// A page of a list whose items are read one at a time.
pub(crate) struct Tolerant<T> {
    /// The items that could be read.
    pub(crate) items: Vec<T>,
    /// Why each of the others could not, without anything they said.
    pub(crate) unread: Vec<String>,
    pub(crate) next_cursor: Option<String>,
}

/// An item of a [`Tolerant`] page: how it is read out of its JSON.
pub(crate) trait Item: Sized + Send + 'static {
    fn read(value: Value, unread: &mut Vec<String>) -> Option<Self>;
}

impl<'de, T: Item> Deserialize<'de> for Tolerant<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        let Some(Value::Array(wires)) = value.get_mut("items").map(Value::take) else {
            return Err(D::Error::custom("a list without `items`"));
        };
        let mut unread = Vec::new();
        let items = wires
            .into_iter()
            .filter_map(|wire| T::read(wire, &mut unread))
            .collect();
        Ok(Self {
            items,
            unread,
            next_cursor: value
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned),
        })
    }
}

impl<T: Item> CursorPage for Tolerant<T> {
    type Item = T;

    fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_deref()
    }

    fn into_items(self) -> Vec<Self::Item> {
        self.items
    }
}

/// A decoding error without what the answer said: the text between
/// quotes is a value (a name, a caption), the text between backticks is
/// a field's name.
pub(crate) fn why(error: &serde_json::Error) -> String {
    let text = error.to_string();
    let mut out = String::with_capacity(text.len());
    let mut quoted = false;
    for c in text.chars() {
        match c {
            '"' => {
                if !quoted {
                    out.push_str("\"…\"");
                }
                quoted = !quoted;
            }
            _ if quoted => {}
            c => out.push(c),
        }
    }
    out
}

/// Gives `map` the fields it lacks, as `null`.
fn nulls(map: &mut serde_json::Map<String, Value>, fields: &[&str]) {
    for field in fields {
        map.entry(*field).or_insert(Value::Null);
    }
}

/// A number that should be whole and is not becomes the nearest whole
/// one: a length of 12.5 seconds is not a reason to lose a story.
fn whole(map: &mut serde_json::Map<String, Value>, fields: &[&str]) {
    for field in fields {
        if let Some(value) = map.get_mut(*field) {
            if let (None, Some(float)) = (value.as_i64(), value.as_f64()) {
                *value = Value::from(float.round() as i64);
            }
        }
    }
}

/// Gives a story the fields a backend may leave out, with what their
/// absence means. `author`: who the list says posted it.
fn fill_story(value: &mut Value, author: Option<&str>) {
    let Value::Object(map) = value else {
        return;
    };
    nulls(
        map,
        &[
            "projectId",
            "contactId",
            "profileName",
            "username",
            "text",
            "backgroundColor",
            "font",
            "media",
            "viewedAt",
            "authorNotified",
            "reaction",
            "viewCount",
            "postedAt",
            "expiresAt",
            "deletedAt",
        ],
    );
    map.entry("object").or_insert_with(|| "story".into());
    map.entry("own").or_insert(Value::Bool(false));
    map.entry("status").or_insert_with(|| "received".into());
    if !map.contains_key("createdAt") {
        if let Some(posted) = map.get("postedAt").filter(|at| at.is_string()).cloned() {
            map.insert("createdAt".into(), posted);
        }
    }
    whole(map, &["font", "viewCount"]);
    if let Some(author) = author {
        map.insert("contactId".into(), author.into());
    }
    if let Some(Value::Object(media)) = map.get_mut("media") {
        nulls(
            media,
            &[
                "url",
                "mimeType",
                "filename",
                "size",
                "width",
                "height",
                "durationSeconds",
            ],
        );
        media.entry("gifPlayback").or_insert(Value::Bool(false));
        media.entry("downloaded").or_insert(Value::Bool(false));
        whole(media, &["size", "width", "height", "durationSeconds"]);
    }
}

pub(crate) fn read_story(
    mut value: Value,
    author: Option<&str>,
    unread: &mut Vec<String>,
) -> Option<api::Story> {
    fill_story(&mut value, author);
    match serde_json::from_value(value) {
        Ok(story) => Some(story),
        Err(error) => {
            unread.push(format!("story: {}", why(&error)));
            None
        }
    }
}

impl Item for api::Story {
    fn read(value: Value, unread: &mut Vec<String>) -> Option<Self> {
        read_story(value, None, unread)
    }
}

impl Item for api::StoryGroup {
    /// An author's stories: each read apart, and each the author's
    /// whatever it says itself (the group is what was asked for).
    fn read(mut value: Value, unread: &mut Vec<String>) -> Option<Self> {
        let Value::Object(map) = &mut value else {
            unread.push("story_group: not an object".to_owned());
            return None;
        };
        let author = map
            .get("contactId")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let wires = match map.insert("stories".into(), Value::Array(Vec::new())) {
            Some(Value::Array(wires)) => wires,
            _ => Vec::new(),
        };
        let stories: Vec<api::Story> = wires
            .into_iter()
            .filter_map(|wire| read_story(wire, author.as_deref(), unread))
            .collect();
        nulls(map, &["profileName", "username"]);
        map.entry("object").or_insert_with(|| "story_group".into());
        map.entry("muted").or_insert(Value::Bool(false));
        map.insert("storyCount".into(), stories.len().into());
        map.insert(
            "unviewedCount".into(),
            stories
                .iter()
                .filter(|story| story.viewed_at.is_none())
                .count()
                .into(),
        );
        if !map.get("lastPostedAt").is_some_and(Value::is_string) {
            let last = stories
                .last()
                .and_then(|story| story.posted_at.clone())
                .unwrap_or_default();
            map.insert("lastPostedAt".into(), last.into());
        }
        match serde_json::from_value::<api::StoryGroup>(value) {
            Ok(mut group) => {
                group.stories = stories;
                Some(group)
            }
            Err(error) => {
                unread.push(format!("story_group: {}", why(&error)));
                None
            }
        }
    }
}

/// `stories().list_own`, one page, each story read apart.
pub(crate) fn own_stories(
    http: &HttpClient,
    account: &str,
    limit: i64,
    cursor: Option<String>,
) -> Paginator<Tolerant<api::Story>> {
    let parts = RequestParts::new(Method::Get, account_path(account, "/stories/own"))
        .query("limit", limit)
        .retryable();
    Paginator::new(http.clone(), parts, "cursor", cursor)
}

/// `stories().list`, one page of authors, each story read apart.
pub(crate) fn story_groups(
    http: &HttpClient,
    account: &str,
    limit: i64,
    cursor: Option<String>,
) -> Paginator<Tolerant<api::StoryGroup>> {
    let parts = RequestParts::new(Method::Get, account_path(account, "/stories"))
        .query("limit", limit)
        .retryable();
    Paginator::new(http.clone(), parts, "cursor", cursor)
}
