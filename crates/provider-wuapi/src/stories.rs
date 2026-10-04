//! Stories on wuapi.
//!
//! * posting a story (`POST /v1/accounts/{accountId}/stories`: text with a
//!   colour and a font, a picture or a video with a caption), safe to
//!   repeat through the `Idempotency-Key`;
//! * the account's own stories with how many saw each
//!   (`GET …/stories/own`) and who (`GET …/stories/{id}/viewers`, with
//!   their reaction);
//! * contacts' stories, grouped by author (`GET …/stories`), each file
//!   fetched only when it is wanted (`GET …/stories/{id}/media`);
//! * the view receipt (`POST …/stories/{id}/view`): the only call that
//!   tells an author anything. Listing, reading and downloading never do;
//! * a reply (`replyToStoryId` on `POST /v1/messages`) and a reaction
//!   (`POST …/stories/{id}/react`);
//! * taking one down (`DELETE /v1/messages/{id}`: a story the account
//!   posted is a message, and this route is on every deployment);
//! * who sees them, read (`GET …/privacy/stories`).
//!
//! Contacts' stories reach wuapi only for a number that has them turned
//! on in the engine; until then the list is empty, which is what the
//! client shows. A deployment from before these routes answers 404 to
//! them: the part is then "not available yet"
//! ([`Missing`](crate::availability::Missing)) and the account's own
//! stories are read the way they were before, as the messages of the chat
//! `stories` (TEMPORARY(stories-rollout): remove [`own_story_messages`]
//! (WuapiClient::own_story_messages) and [`story_of_message`]'s use in
//! the listing once the routes are live in production).
//!
//! What the API still cannot do answers
//! [`ProviderError::Unsupported`] and its capability flag stays off:
//!
//! * `TODO(wuapi-api: stories mute write)`: `story_group.muted` says that
//!   an author is muted on the phone, and the poll passes it on, but no
//!   route mutes or unmutes one (whatsmeow has no builder for that app
//!   state mutation). Mutes made here stay on this computer.
//! * `TODO(wuapi-api: stories privacy edit)`: the lists cannot be changed
//!   (the same reason); only the setting, in `PATCH …/privacy`.
//! * `TODO(wuapi-api: stories events)`: `story.received`, `story.deleted`,
//!   `story.viewed` and `story.reacted` are webhooks only. The poll reads
//!   the lists instead (`events.rs`).

use crate::availability::no_route;
use crate::client::{WuapiClient, MAX_PAGE};
use crate::error::from_sdk;
use crate::mapping::{self, STORY_PREFIX};
use client_provider::{
    AccountId, ClientMessageId, ContactId, Feature, Media, MediaKind, MediaRef, MessageContent,
    MessageId, NewStory, NewStoryContent, ProviderError, ProviderResult, Story, StoryAudience,
    StoryBody, StoryFont, StoryPrivacy, StoryStyle, StoryViewer, Timestamp, STORY_LIFETIME,
};
use wuapi::types as api;

/// How many pages of the own stories are read at most: they are listed
/// newest first and stop mattering after a day.
const PAGES: usize = 4;

/// How many pages of authors are read at most: a hundred authors a page.
const GROUP_PAGES: usize = 5;

/// How many viewers of one story are read at most.
const MAX_VIEWERS: usize = 2_000;

/// The stories of an account as the API lists them.
pub(crate) struct Listed {
    /// The account's own and its contacts', not expired.
    pub(crate) stories: Vec<Story>,
    /// The authors whose stories the account muted on its phone, and
    /// those it did not, as far as this listing names them.
    pub(crate) muted: Vec<(ContactId, bool)>,
    /// What the API answered, the account's own first: every story it
    /// listed, the ones the client does not show included.
    pub(crate) wires: Vec<api::Story>,
    /// Why some of what the API listed could not be read at all.
    pub(crate) unread: Vec<String>,
}

/// Why the stories could not be listed: the API's own answer, kept as it
/// is for `--diagnose stories`, or a failure of the older way of reading
/// them.
#[derive(Debug)]
pub(crate) enum ListError {
    /// A story route failed.
    Api(Box<wuapi::Error>),
    /// TEMPORARY(stories-rollout): the messages of the chat `stories`
    /// could not be read.
    Messages(ProviderError),
}

impl From<ListError> for ProviderError {
    fn from(error: ListError) -> Self {
        match error {
            ListError::Api(error) => from_sdk(*error),
            ListError::Messages(error) => error,
        }
    }
}

impl WuapiClient {
    /// `POST …/stories` with a text story.
    pub(crate) async fn post_text_story(
        &self,
        account: &str,
        text: &str,
        style: &StoryStyle,
        client_id: &str,
    ) -> ProviderResult<api::Message> {
        let mut request = api::TextStoryCreateRequest::new(text);
        request.r#type = Some("text".to_owned());
        request.background_color = Some(style.background_hex());
        request.font = Some(match style.font.0 {
            1 => api::TextStoryCreateRequestFont::Value1,
            2 => api::TextStoryCreateRequestFont::Value2,
            6 => api::TextStoryCreateRequestFont::Value6,
            7 => api::TextStoryCreateRequestFont::Value7,
            8 => api::TextStoryCreateRequestFont::Value8,
            9 => api::TextStoryCreateRequestFont::Value9,
            10 => api::TextStoryCreateRequestFont::Value10,
            _ => api::TextStoryCreateRequestFont::Value0,
        });
        crate::compat::post_story(self.sdk().http(), account, request)
            .idempotency_key(client_id)
            .await
            .map(|message| message.0)
            .map_err(from_sdk)
    }

    /// `GET /v1/accounts/{accountId}/privacy/stories`.
    pub(crate) async fn story_privacy(&self, account: &str) -> ProviderResult<api::StoryPrivacy> {
        self.sdk()
            .privacy()
            .get_story_privacy(account)
            .await
            .map_err(from_sdk)
    }

    /// TEMPORARY(stories-rollout): the account's own stories as messages,
    /// newest first, of the last day, for a deployment without
    /// `GET …/stories/own`.
    pub(crate) async fn own_story_messages(
        &self,
        account: &str,
        now: Timestamp,
    ) -> ProviderResult<Vec<api::Message>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..PAGES {
            let page = self
                .messages(Some(account), Some("stories"), cursor.as_deref(), MAX_PAGE)
                .await?;
            let mut older = false;
            for wire in page.items {
                let posted = wire
                    .sent_at
                    .as_deref()
                    .and_then(mapping::timestamp)
                    .or_else(|| mapping::timestamp(&wire.created_at));
                if posted.is_some_and(|at| {
                    at.as_millis() + STORY_LIFETIME.as_millis() as i64 <= now.as_millis()
                }) {
                    older = true;
                    continue;
                }
                out.push(wire);
            }
            match page.next_cursor {
                Some(next) if !older => cursor = Some(next),
                _ => break,
            }
        }
        Ok(out)
    }

    /// `GET …/stories/own`: the stories the account posted in the last
    /// day, from here and from its phone, each with its view count.
    /// `None`: this deployment has no such route.
    async fn own_stories(
        &self,
        account: &str,
        unread: &mut Vec<String>,
    ) -> Result<Option<Vec<api::Story>>, Box<wuapi::Error>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..PAGES {
            let list = crate::compat::own_stories(
                self.sdk().http(),
                account,
                i64::from(MAX_PAGE),
                cursor.take(),
            );
            let page = match list.page().await {
                Ok(page) => page,
                Err(error) if no_route(&error) => return Ok(None),
                Err(error) => return Err(Box::new(error)),
            };
            out.extend(page.items);
            unread.extend(page.unread);
            // A page may be short while the cursor is set: the cursor
            // decides.
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(Some(out))
    }

    /// `GET …/stories`: contacts' stories, one group per author, the
    /// author with the newest story first. Reads stored rows: nothing is
    /// asked of WhatsApp and nobody is told anything. `None`: this
    /// deployment has no such route.
    async fn story_groups(
        &self,
        account: &str,
        unread: &mut Vec<String>,
    ) -> Result<Option<Vec<api::StoryGroup>>, Box<wuapi::Error>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..GROUP_PAGES {
            let list = crate::compat::story_groups(
                self.sdk().http(),
                account,
                i64::from(MAX_PAGE),
                cursor.take(),
            );
            let page = match list.page().await {
                Ok(page) => page,
                Err(error) if no_route(&error) => return Ok(None),
                Err(error) => return Err(Box::new(error)),
            };
            out.extend(page.items);
            unread.extend(page.unread);
            match page.next_cursor {
                Some(next) => cursor = Some(next),
                None => break,
            }
        }
        Ok(Some(out))
    }

    /// The account's stories and its contacts'. On a deployment without
    /// the story routes the own ones are read from the messages and the
    /// contacts' are none, with the part marked as not available yet.
    pub(crate) async fn stories(&self, account: &str, now: Timestamp) -> ProviderResult<Listed> {
        Ok(self.list_stories(account, now).await?)
    }

    /// [`stories`](Self::stories), with the API's own answer when it
    /// fails.
    pub(crate) async fn list_stories(
        &self,
        account: &str,
        now: Timestamp,
    ) -> Result<Listed, ListError> {
        let mut unread = Vec::new();
        let own = if self.missing.is(Feature::Stories, account) {
            None
        } else {
            let own = self
                .own_stories(account, &mut unread)
                .await
                .map_err(ListError::Api)?;
            if own.is_none() {
                self.missing.no_route(Feature::Stories);
            }
            own
        };
        let Some(own) = own else {
            // TEMPORARY(stories-rollout)
            let wires = self
                .own_story_messages(account, now)
                .await
                .map_err(ListError::Messages)?;
            return Ok(Listed {
                stories: wires.iter().filter_map(story_of_message).collect(),
                muted: Vec::new(),
                wires: Vec::new(),
                unread,
            });
        };
        let groups = match self
            .story_groups(account, &mut unread)
            .await
            .map_err(ListError::Api)?
        {
            Some(groups) => {
                self.missing.works(Feature::Stories, account);
                groups
            }
            None => {
                self.missing.no_route(Feature::Stories);
                Vec::new()
            }
        };
        if !unread.is_empty() {
            tracing::debug!(
                count = unread.len(),
                first = %unread[0],
                "some stories the API listed could not be read; they are left out"
            );
        }
        let mut stories: Vec<Story> = own.iter().filter_map(|wire| story(wire, now)).collect();
        let mut muted = Vec::new();
        let mut wires = own;
        for group in groups {
            muted.push((ContactId::new(group.contact_id.clone()), group.muted));
            stories.extend(group.stories.iter().filter_map(|wire| story(wire, now)));
            wires.extend(group.stories);
        }
        Ok(Listed {
            stories,
            muted,
            wires,
            unread,
        })
    }

    /// `POST …/stories/{storyId}/view`: the view receipt. The one call
    /// that tells the author the account saw their story. A story already
    /// seen is answered as it is and nothing is sent, so it is safe to
    /// repeat; the key makes a repeat the same request.
    pub(crate) async fn view_story(&self, account: &str, story: &str) -> ProviderResult<()> {
        if self.missing.is(Feature::Stories, account) {
            return Err(ProviderError::Unsupported("marking a story as seen"));
        }
        match self
            .sdk()
            .stories()
            .view(account, story)
            .idempotency_key(format!("view:{story}"))
            .await
        {
            Ok(_) => Ok(()),
            Err(error) if no_route(&error) => {
                self.missing.no_route(Feature::Stories);
                Err(ProviderError::Unsupported("marking a story as seen"))
            }
            Err(error) => Err(from_sdk(error)),
        }
    }

    /// `GET …/stories/{storyId}/viewers`: who saw a story of the
    /// account's, the latest first, with their reaction.
    pub(crate) async fn story_viewers(
        &self,
        account: &str,
        story: &str,
    ) -> ProviderResult<Vec<StoryViewer>> {
        if self.missing.is(Feature::Stories, account) {
            return Err(ProviderError::Unsupported("who saw a story"));
        }
        let params = api::StoriesListViewersParams {
            limit: Some(i64::from(MAX_PAGE)),
            ..Default::default()
        };
        match self
            .sdk()
            .stories()
            .list_viewers(account, story, params)
            .to_vec_max(MAX_VIEWERS)
            .await
        {
            Ok(viewers) => Ok(viewers.iter().filter_map(viewer).collect()),
            Err(error) if no_route(&error) => {
                self.missing.no_route(Feature::Stories);
                Err(ProviderError::Unsupported("who saw a story"))
            }
            Err(error) => Err(from_sdk(error)),
        }
    }

    /// `POST …/stories/{storyId}/react`: a reaction for the story's
    /// author alone. An empty emoji takes it back. `400 not_supported`
    /// (the account's story privacy leaves the author out, so the
    /// reaction could not go to them alone) is a refusal in the API's
    /// words.
    pub(crate) async fn react_to_story(
        &self,
        account: &str,
        story: &str,
        emoji: &str,
        client_id: &str,
    ) -> ProviderResult<()> {
        if self.missing.is(Feature::Stories, account) {
            return Err(ProviderError::Unsupported("reacting to a story"));
        }
        match self
            .sdk()
            .stories()
            .react(account, story, api::ReactRequest::new(emoji))
            .idempotency_key(client_id)
            .await
        {
            Ok(()) => Ok(()),
            Err(error) if no_route(&error) => {
                self.missing.no_route(Feature::Stories);
                Err(ProviderError::Unsupported("reacting to a story"))
            }
            Err(error) => Err(from_sdk(error)),
        }
    }
}

fn positive(value: Option<i64>) -> Option<u32> {
    value
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

/// Where a story's file is fetched from, decided by `downloaded` alone,
/// as for a message's (`mapping::media_source`): a stored file is its
/// URL; one still on WhatsApp names the story, and the request for it is
/// made through the SDK, on the API host.
fn story_source(id: &str, file: &api::StoryFile) -> Option<MediaRef> {
    let url = file.url.as_ref()?;
    if file.downloaded {
        return Some(MediaRef::new(url.clone()));
    }
    let size = file
        .size
        .filter(|size| *size >= 0)
        .map(|size| size.to_string())
        .unwrap_or_default();
    Some(MediaRef::new(format!("{STORY_PREFIX}{size}:{id}")))
}

/// `Story` -> [`Story`]. `None` for one that is deleted, failed, expired
/// or of a kind a story is not shown as.
pub(crate) fn story(wire: &api::Story, now: Timestamp) -> Option<Story> {
    use api::MessageType as T;
    if wire.deleted_at.is_some() || wire.status == api::MessageStatus::Failed {
        return None;
    }
    // One still queued has no time of its own yet.
    let posted_at = wire
        .posted_at
        .as_deref()
        .and_then(mapping::timestamp)
        .or_else(|| mapping::timestamp(&wire.created_at))?;
    let expires_at = wire.expires_at.as_deref().and_then(mapping::timestamp);
    let ends = expires_at.map_or(
        posted_at.as_millis() + STORY_LIFETIME.as_millis() as i64,
        |at| at.as_millis(),
    );
    if ends <= now.as_millis() {
        return None;
    }
    let words = wire.text.clone().filter(|text| !text.is_empty());
    let kind = match wire.r#type {
        T::Image => Some(MediaKind::Image),
        T::Video => Some(MediaKind::Video),
        T::Voice | T::Audio => Some(MediaKind::Voice),
        _ => None,
    };
    let body = match (&wire.r#type, kind) {
        (T::Text, _) => StoryBody::Text {
            text: words?,
            style: {
                let default = StoryStyle::default();
                StoryStyle {
                    background: wire
                        .background_color
                        .as_deref()
                        .and_then(StoryStyle::parse_background)
                        .unwrap_or(default.background),
                    font: wire
                        .font
                        .and_then(|font| u8::try_from(font).ok())
                        .map(StoryFont)
                        .filter(|font| font.is_known())
                        .unwrap_or(default.font),
                }
            },
        },
        (_, Some(kind)) => {
            let mut media = Media::new(kind);
            if let Some(file) = &wire.media {
                media.source = story_source(&wire.id, file);
                media.mime_type = file.mime_type.clone();
                media.file_name = file.filename.clone();
                media.size_bytes = file.size.and_then(|size| u64::try_from(size).ok());
                (media.width, media.height) = match (positive(file.width), positive(file.height)) {
                    (Some(width), Some(height)) => (Some(width), Some(height)),
                    _ => (None, None),
                };
                media.duration_secs = positive(file.duration_seconds);
                media.gif = kind == MediaKind::Video && file.gif_playback;
            }
            media.caption = words;
            StoryBody::Media(media)
        }
        // A kind WhatsApp added since: not a story this client can show.
        _ => return None,
    };
    Some(Story {
        id: MessageId::new(wire.id.clone()),
        client_id: None,
        account_id: AccountId::new(wire.account_id.clone()),
        author: ContactId::new(wire.contact_id.clone().unwrap_or_default()),
        author_name: wire
            .profile_name
            .clone()
            .filter(|name| !name.trim().is_empty()),
        mine: wire.own,
        posted_at,
        expires_at,
        body,
        mentions: Vec::new(),
        // Your own: nothing to see. A contact's: seen through the API or
        // on the phone.
        viewed: wire.own || wire.viewed_at.is_some(),
        view_count: wire
            .view_count
            .filter(|_| wire.own)
            .map(|count| u32::try_from(count.max(0)).unwrap_or(u32::MAX)),
    })
}

/// `StoryViewer` -> [`StoryViewer`].
pub(crate) fn viewer(wire: &api::StoryViewer) -> Option<StoryViewer> {
    Some(StoryViewer {
        contact: ContactId::new(wire.contact_id.clone()),
        // The API names nobody: the client has the address book.
        name: None,
        viewed_at: mapping::timestamp(&wire.viewed_at)?,
        reaction: wire.reaction.clone().filter(|emoji| !emoji.is_empty()),
    })
}

/// An own story, as the answer to a post gives it and as an older
/// deployment lists it: an outbound message of the chat `stories`. `None`
/// for anything else.
pub(crate) fn story_of_message(wire: &api::Message) -> Option<Story> {
    if wire.chat_type != api::ChatType::Story || wire.direction != api::MessageDirection::Outbound {
        return None;
    }
    let posted_at = wire
        .sent_at
        .as_deref()
        .and_then(mapping::timestamp)
        .or_else(|| mapping::timestamp(&wire.created_at))?;
    let body = match mapping::story_content(wire)? {
        MessageContent::Text { body } => StoryBody::Text {
            text: body,
            // Not on the message: the request that posted it has it.
            style: StoryStyle::default(),
        },
        MessageContent::Media(media)
            if matches!(media.kind, MediaKind::Image | MediaKind::Video) =>
        {
            StoryBody::Media(media)
        }
        _ => return None,
    };
    // A story that failed to go out is not one.
    if wire.status == api::MessageStatus::Failed {
        return None;
    }
    Some(Story {
        id: MessageId::new(wire.id.clone()),
        client_id: None,
        account_id: AccountId::new(wire.account_id.clone()),
        author: ContactId::new(wire.from.clone()),
        author_name: None,
        mine: true,
        posted_at,
        expires_at: None,
        body,
        mentions: Vec::new(),
        // Your own: nothing to see.
        viewed: true,
        // The message does not count them: `GET …/stories/own` does.
        view_count: None,
    })
}

/// The story a post made, from what the request said and what the API
/// answered: the API's message has no style, the request has.
pub(crate) fn posted(wire: &api::Message, new: &NewStory) -> ProviderResult<Story> {
    let mut story = story_of_message(wire).ok_or_else(|| {
        ProviderError::Protocol("the API's answer to a story is not a story".into())
    })?;
    story.client_id = Some(ClientMessageId::new(new.client_id.as_str()));
    if let (StoryBody::Text { style, .. }, NewStoryContent::Text { style: asked, .. }) =
        (&mut story.body, &new.content)
    {
        *style = *asked;
    }
    if let (StoryBody::Media(media), NewStoryContent::Media { caption, .. }) =
        (&mut story.body, &new.content)
    {
        if media.caption.is_none() {
            media.caption = caption.clone();
        }
    }
    Ok(story)
}

/// `StoryPrivacy` -> [`StoryPrivacy`]: the list in use says the setting,
/// and both lists are kept.
pub(crate) fn privacy(wire: &api::StoryPrivacy) -> StoryPrivacy {
    use api::StoryPrivacyListsItemType as T;
    let ids = |kind: T| -> Vec<ContactId> {
        wire.lists
            .iter()
            .filter(|list| list.r#type == kind)
            .flat_map(|list| list.contact_ids.iter().map(|id| ContactId::new(id.clone())))
            .collect()
    };
    let in_use = wire
        .lists
        .iter()
        .find(|list| list.default)
        .map(|list| &list.r#type);
    StoryPrivacy {
        audience: match in_use {
            Some(T::Blacklist) => StoryAudience::ContactsExcept,
            Some(T::Whitelist) => StoryAudience::OnlyShareWith,
            _ => StoryAudience::Contacts,
        },
        except: ids(T::Blacklist),
        only: ids(T::Whitelist),
    }
}
