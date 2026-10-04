//! Stories in the mock: several authors with text, picture and video
//! stories of different ages (one about to expire), the account's own
//! stories with viewers and reactions, a muted author, an audience, and
//! live additions. Tests can add, remove and answer stories, and make the
//! next story calls fail.

use crate::seed::{self, World};
use crate::{MockProvider, State};
use client_provider::{
    AccountId, ChatKind, ClientMessageId, ContactId, Media, MediaKind, MessageId, NewStory,
    NewStoryContent, OutgoingMessage, ProviderError, ProviderEvent, ProviderResult, SendReceipt,
    Story, StoryAudience, StoryBody, StoryFont, StoryPrivacy, StoryReplyKind, StoryReplyRef,
    StoryStyle, StoryViewer, Timestamp,
};
use std::collections::{HashMap, VecDeque};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

/// What the mock keeps for stories.
#[derive(Default)]
pub(crate) struct MockStories {
    stories: HashMap<AccountId, Vec<Story>>,
    viewers: HashMap<(AccountId, MessageId), Vec<StoryViewer>>,
    /// Stories by the client id they were posted under: a repeat finds
    /// the same one.
    posted: HashMap<ClientMessageId, Story>,
    muted: HashMap<AccountId, Vec<ContactId>>,
    privacy: HashMap<AccountId, StoryPrivacy>,
    /// The view receipts that were sent, in order.
    views: Vec<(AccountId, MessageId, ContactId)>,
    /// Replies and reactions, in order: the story and the message.
    replies: Vec<(MessageId, OutgoingMessage)>,
    reactions: Vec<(MessageId, OutgoingMessage)>,
    /// Failures to return from upcoming story calls, in order.
    failures: VecDeque<ProviderError>,
    calls: Vec<&'static str>,
    next: u32,
    /// Story ids that were taken down.
    deleted: Vec<MessageId>,
    /// Answers already given to replies and reactions by client id.
    answered: HashMap<ClientMessageId, SendReceipt>,
    /// How long listing, posting and deleting take to answer.
    latency: std::time::Duration,
}

impl MockStories {
    /// A story that is up, as the message it is: what forwarding it by
    /// naming it passes on.
    pub(crate) fn as_message(
        &self,
        account: &AccountId,
        id: &MessageId,
    ) -> Option<client_provider::Message> {
        use client_provider::{DeliveryStatus, Direction, Message, MessageContent};
        let story = self.stories.get(account)?.iter().find(|s| &s.id == id)?;
        Some(Message {
            id: story.id.clone(),
            client_id: story.client_id.clone(),
            account_id: account.clone(),
            chat_id: client_provider::ChatId::new("status"),
            sender: story.author.clone(),
            sender_name: story.author_name.clone(),
            direction: if story.mine {
                Direction::Outgoing
            } else {
                Direction::Incoming
            },
            timestamp: story.posted_at,
            content: match &story.body {
                StoryBody::Text { text, .. } => MessageContent::text(text.clone()),
                StoryBody::Media(media) => MessageContent::Media(media.clone()),
            },
            reply_to: None,
            status: DeliveryStatus::Sent,
            edited: false,
            deleted: false,
            extras: Default::default(),
        })
    }
}

fn story_id(n: u32) -> MessageId {
    MessageId::new(format!("story{n}"))
}

#[allow(clippy::too_many_arguments)]
fn text(
    n: u32,
    account: &AccountId,
    author: &ContactId,
    name: Option<&str>,
    now: Timestamp,
    age: i64,
    words: &str,
    background: u32,
    font: u8,
) -> Story {
    Story {
        id: story_id(n),
        client_id: None,
        account_id: account.clone(),
        author: author.clone(),
        author_name: name.map(str::to_owned),
        mine: name.is_none(),
        posted_at: Timestamp::from_millis(now.as_millis() - age),
        expires_at: None,
        body: StoryBody::Text {
            text: words.to_owned(),
            style: StoryStyle {
                background,
                font: StoryFont(font),
            },
        },
        mentions: Vec::new(),
        viewed: false,
        view_count: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn media(
    n: u32,
    account: &AccountId,
    author: &ContactId,
    name: Option<&str>,
    now: Timestamp,
    age: i64,
    kind: MediaKind,
    caption: Option<&str>,
) -> Story {
    let mut file = Media::new(kind);
    file.source = Some(client_provider::MediaRef::new(match kind {
        MediaKind::Video => format!("mock://video/story{n}"),
        _ => format!("mock://image/story{n}/720x1280"),
    }));
    file.mime_type = Some(
        match kind {
            MediaKind::Video => "video/mp4",
            _ => "image/png",
        }
        .into(),
    );
    file.caption = caption.map(str::to_owned);
    file.width = Some(720);
    file.height = Some(1280);
    if kind == MediaKind::Video {
        file.duration_secs = Some(12);
    }
    Story {
        body: StoryBody::Media(file),
        ..text(n, account, author, name, now, age, "", 0, 0)
    }
}

/// The world's stories at `now`.
pub(crate) fn seed(world: &World, now: Timestamp) -> MockStories {
    let mut out = MockStories::default();
    for account in &world.accounts {
        let me = seed::self_contact(&account.id);
        let people: Vec<(ContactId, String)> = world
            .chats
            .iter()
            .filter(|chat| chat.account_id == account.id && chat.kind == ChatKind::Direct)
            .map(|chat| (ContactId::new(chat.id.as_str()), chat.title.clone()))
            .collect();
        let mut stories = Vec::new();
        let mut n = 0u32;
        let mut next = || {
            n += 1;
            n
        };
        if account.id.as_str() == "acc_personal" {
            let at = |index: usize| people.get(index).cloned();
            // Ana: three, the last two with news.
            if let Some((author, name)) = at(0) {
                stories.push(text(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    2 * HOUR,
                    "Sunday at the lake. Nobody is allowed to talk about Monday.",
                    0x0B_6E_4F,
                    0,
                ));
                stories.push(media(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    90 * MINUTE,
                    MediaKind::Image,
                    Some("The lake, 7 am"),
                ));
                stories.push(media(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    20 * MINUTE,
                    MediaKind::Video,
                    Some("Swimming is a strong word"),
                ));
            }
            // One in a serif, in a purple that is hard to read dark on.
            if let Some((author, name)) = at(1) {
                // A picture first (the one a client may fetch ahead of
                // time), then the words.
                stories.push(media(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    6 * HOUR,
                    MediaKind::Image,
                    Some("Load-in"),
                ));
                stories.push(text(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    5 * HOUR,
                    "Rehearsal went well. Mostly.",
                    0x5B_2A_86,
                    1,
                ));
            }
            // One that is about to go: posted 23 h 50 min ago, and another
            // 22 h ago.
            if let Some((author, name)) = at(2) {
                stories.push(media(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    22 * HOUR,
                    MediaKind::Image,
                    None,
                ));
                stories.push(text(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    23 * HOUR + 50 * MINUTE,
                    "Last call for the tickets.",
                    0xB0_3A_2E,
                    7,
                ));
            }
            // One the provider says was seen on another device.
            if let Some((author, name)) = at(3) {
                let mut seen = media(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    3 * HOUR,
                    MediaKind::Image,
                    Some("New desk"),
                );
                seen.viewed = true;
                stories.push(seen);
            }
            // A muted author.
            if let Some((author, name)) = at(4) {
                stories.push(text(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    4 * HOUR,
                    "Daily motivation, again.",
                    0x1D_4E_89,
                    9,
                ));
                stories.push(text(
                    next(),
                    &account.id,
                    &author,
                    Some(&name),
                    now,
                    HOUR,
                    "You can do it.",
                    0x1D_4E_89,
                    9,
                ));
                out.muted.insert(account.id.clone(), vec![author]);
            }
            // The account's own, with people who saw them.
            let mut mine_text = text(
                next(),
                &account.id,
                &me,
                None,
                now,
                3 * HOUR,
                "Back online. Ask me anything about stories.",
                0x1F_6F_5C,
                0,
            );
            let mut mine_image = media(
                next(),
                &account.id,
                &me,
                None,
                now,
                50 * MINUTE,
                MediaKind::Image,
                Some("The desk today"),
            );
            let mut watchers = Vec::new();
            for (index, (author, name)) in people.iter().take(5).enumerate() {
                watchers.push(StoryViewer {
                    contact: author.clone(),
                    name: Some(name.clone()),
                    viewed_at: Timestamp::from_millis(
                        now.as_millis() - (2 * HOUR + 20 * MINUTE) + index as i64 * 17 * MINUTE,
                    ),
                    reaction: (index % 2 == 0)
                        .then(|| ["🔥", "❤️", "😂"][index / 2 % 3].to_owned()),
                });
            }
            mine_text.view_count = Some(watchers.len() as u32);
            out.viewers
                .insert((account.id.clone(), mine_text.id.clone()), watchers.clone());
            let few: Vec<StoryViewer> = watchers
                .iter()
                .take(2)
                .cloned()
                .map(|mut viewer| {
                    viewer.viewed_at =
                        Timestamp::from_millis(now.as_millis() - 30 * MINUTE + MINUTE);
                    viewer
                })
                .collect();
            mine_image.view_count = Some(few.len() as u32);
            out.viewers
                .insert((account.id.clone(), mine_image.id.clone()), few);
            stories.push(mine_text);
            stories.push(mine_image);
        } else if let Some((author, name)) = people.first().cloned() {
            stories.push(text(
                next(),
                &account.id,
                &author,
                Some(&name),
                now,
                HOUR,
                "Standup moved to 10:30.",
                0x2B_5F_7A,
                0,
            ));
        }
        out.next = out.next.max(n);
        out.stories.insert(account.id.clone(), stories);
        out.privacy.insert(
            account.id.clone(),
            StoryPrivacy {
                audience: StoryAudience::ContactsExcept,
                except: people
                    .iter()
                    .skip(5)
                    .take(2)
                    .map(|(id, _)| id.clone())
                    .collect(),
                only: people.iter().take(3).map(|(id, _)| id.clone()).collect(),
            },
        );
    }
    out
}

/// The stories of the world that have not expired yet.
fn alive(stories: &[Story], now: Timestamp) -> Vec<Story> {
    stories
        .iter()
        .filter(|story| story.expiry() > now)
        .cloned()
        .collect()
}

fn enter(state: &mut State, call: &'static str) -> ProviderResult<()> {
    state.stories.calls.push(call);
    match state.stories.failures.pop_front() {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn gone() -> ProviderError {
    ProviderError::Rejected {
        code: "not_found".into(),
        message: "The status is no longer there.".into(),
    }
}

impl MockProvider {
    // ----- for tests ----------------------------------------------------

    /// Makes the next story calls fail with the given errors, one per
    /// call, in order.
    pub fn fail_next_story_calls(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().stories.failures.extend(errors);
    }

    /// How long listing, posting and taking down a story take to answer.
    /// What the call does is done before the wait: a list is as it was when
    /// it was asked for, and a caller that gives up meanwhile has still
    /// posted or deleted.
    pub fn set_story_latency(&self, latency: std::time::Duration) {
        self.state().stories.latency = latency;
    }

    /// Waits as long as a story call takes to answer.
    pub(crate) async fn story_wait(&self) {
        let latency = self.state().stories.latency;
        if !latency.is_zero() {
            tokio::time::sleep(latency).await;
        }
    }

    /// Records that stories were asked for again ("recheck" among the
    /// story calls).
    pub(crate) fn note_story_recheck(&self) {
        self.state().stories.calls.push("recheck");
    }

    /// The story calls that were made, in order.
    pub fn story_calls(&self) -> Vec<&'static str> {
        self.state().stories.calls.clone()
    }

    /// The view receipts that were sent: the story and its author.
    pub fn story_views(&self) -> Vec<(MessageId, ContactId)> {
        self.state()
            .stories
            .views
            .iter()
            .map(|(_, story, author)| (story.clone(), author.clone()))
            .collect()
    }

    /// The replies to stories that were sent: the story and the message.
    pub fn story_replies(&self) -> Vec<(MessageId, OutgoingMessage)> {
        self.state().stories.replies.clone()
    }

    /// The reactions to stories that were sent.
    pub fn story_reactions(&self) -> Vec<(MessageId, OutgoingMessage)> {
        self.state().stories.reactions.clone()
    }

    /// The stories that were taken down.
    pub fn deleted_stories(&self) -> Vec<MessageId> {
        self.state().stories.deleted.clone()
    }

    /// The stories of an account, expired ones included.
    pub fn stories_of(&self, account: &AccountId) -> Vec<Story> {
        self.state()
            .stories
            .stories
            .get(account)
            .cloned()
            .unwrap_or_default()
    }

    /// The stories posted through [`Provider::post_story`], by client id.
    pub fn posted_stories(&self) -> Vec<Story> {
        self.state().stories.posted.values().cloned().collect()
    }

    /// Who is muted, as the provider keeps it.
    pub fn muted_authors(&self, account: &AccountId) -> Vec<ContactId> {
        self.state()
            .stories
            .muted
            .get(account)
            .cloned()
            .unwrap_or_default()
    }

    /// The audience as the provider holds it.
    pub fn privacy_of(&self, account: &AccountId) -> StoryPrivacy {
        self.state()
            .stories
            .privacy
            .get(account)
            .cloned()
            .unwrap_or_default()
    }

    /// A contact posts a story now: the account hears of it.
    pub fn contact_posts_story(
        &self,
        account: &AccountId,
        author: &ContactId,
        name: &str,
        body: StoryBody,
    ) -> Story {
        let story = {
            let mut state = self.state();
            state.stories.next += 1;
            let story = Story {
                id: story_id(state.stories.next),
                client_id: None,
                account_id: account.clone(),
                author: author.clone(),
                author_name: Some(name.to_owned()),
                mine: false,
                posted_at: Timestamp::now(),
                expires_at: None,
                body,
                mentions: Vec::new(),
                viewed: false,
                view_count: None,
            };
            state
                .stories
                .stories
                .entry(account.clone())
                .or_default()
                .push(story.clone());
            story
        };
        self.emit(ProviderEvent::StoryUpserted(story.clone()));
        story
    }

    /// A contact's story is taken down.
    pub fn contact_removes_story(&self, account: &AccountId, story: &MessageId) {
        {
            let mut state = self.state();
            if let Some(list) = state.stories.stories.get_mut(account) {
                list.retain(|known| &known.id != story);
            }
        }
        self.emit(ProviderEvent::StoryRemoved {
            account_id: account.clone(),
            story_id: story.clone(),
        });
    }

    /// Somebody sees one of the account's stories now.
    pub fn contact_views_story(&self, account: &AccountId, story: &MessageId, viewer: StoryViewer) {
        {
            let mut state = self.state();
            let list = state
                .stories
                .viewers
                .entry((account.clone(), story.clone()))
                .or_default();
            list.retain(|known| known.contact != viewer.contact);
            list.insert(0, viewer.clone());
            let count = list.len() as u32;
            if let Some(known) = state
                .stories
                .stories
                .get_mut(account)
                .and_then(|list| list.iter_mut().find(|known| &known.id == story))
            {
                known.view_count = Some(count);
            }
        }
        self.emit(ProviderEvent::StoryViewed {
            account_id: account.clone(),
            story_id: story.clone(),
            viewer,
        });
    }

    /// A contact answers a story of the account's: a message in their
    /// chat that says which story it is about.
    pub fn contact_replies_to_story(
        &self,
        account: &AccountId,
        story: &MessageId,
        words: &str,
    ) -> Option<MessageId> {
        let item = self
            .state()
            .stories
            .stories
            .get(account)?
            .iter()
            .find(|known| &known.id == story)
            .cloned()?;
        let chat = client_provider::ChatId::new(item.author.as_str());
        let reply = StoryReplyRef {
            story: item.id.clone(),
            kind: StoryReplyKind::of(&item.body),
            preview: item.body.words().map(str::to_owned),
            of_mine: true,
        };
        self.receive_with_story(account, &chat, words, reply)
    }

    /// An incoming text in a chat that answers a story.
    fn receive_with_story(
        &self,
        account: &AccountId,
        chat: &client_provider::ChatId,
        words: &str,
        reply: StoryReplyRef,
    ) -> Option<MessageId> {
        let (message, snapshot) = {
            let mut state = self.state();
            let id = state.world.next_message;
            state.world.next_message += 1;
            let author = ContactId::new(chat.as_str());
            let name = state
                .world
                .chats
                .iter()
                .find(|known| &known.id == chat && &known.account_id == account)
                .map(|known| known.title.clone());
            let message = client_provider::Message {
                id: MessageId::new(format!("m{id}")),
                client_id: None,
                account_id: account.clone(),
                chat_id: chat.clone(),
                sender: author,
                sender_name: name,
                direction: client_provider::Direction::Incoming,
                timestamp: Timestamp::now(),
                content: client_provider::MessageContent::text(words),
                reply_to: None,
                status: client_provider::DeliveryStatus::Delivered,
                edited: false,
                deleted: false,
                extras: client_provider::MessageExtras {
                    story_reply: Some(reply),
                    ..Default::default()
                },
            };
            state
                .world
                .messages
                .entry((account.clone(), chat.clone()))
                .or_default()
                .push(message.clone());
            let snapshot = state
                .world
                .chats
                .iter_mut()
                .find(|known| &known.id == chat && &known.account_id == account)
                .map(|known| {
                    known.unread_count += 1;
                    known.last_message = Some(message.clone());
                    known.clone()
                });
            (message, snapshot)
        };
        let id = message.id.clone();
        self.emit(ProviderEvent::MessageUpserted(message));
        if let Some(chat) = snapshot {
            self.emit(ProviderEvent::ChatUpdated(chat));
        }
        Some(id)
    }

    // ----- the Provider calls -------------------------------------------

    pub(crate) fn story_list(&self, account: &AccountId) -> ProviderResult<Vec<Story>> {
        // A backend without the newer story routes still lists what the
        // account posted, and nothing of anybody else.
        let own_only = self.is_unavailable(account, client_provider::Feature::Stories);
        let mut state = self.state();
        enter(&mut state, "list")?;
        let now = Timestamp::now();
        let mut listed = alive(
            state
                .stories
                .stories
                .get(account)
                .map_or(&[], Vec::as_slice),
            now,
        );
        if own_only {
            listed.retain(|story| story.mine);
        }
        Ok(listed)
    }

    pub(crate) fn story_post(&self, new: NewStory) -> ProviderResult<Story> {
        let story = {
            let mut state = self.state();
            enter(&mut state, "post")?;
            if let Some(known) = state.stories.posted.get(&new.client_id) {
                return Ok(known.clone());
            }
            let body = match &new.content {
                NewStoryContent::Text { text, style } => {
                    if text.trim().is_empty() {
                        return Err(ProviderError::Rejected {
                            code: "invalid_request".into(),
                            message: "The status is empty.".into(),
                        });
                    }
                    StoryBody::Text {
                        text: text.clone(),
                        style: *style,
                    }
                }
                NewStoryContent::Media {
                    kind,
                    media,
                    mime_type,
                    caption,
                } => {
                    let Some((bytes, _)) = state.media.get(media.as_str()) else {
                        return Err(ProviderError::Rejected {
                            code: "upload_expired".into(),
                            message: "The uploaded file is no longer there.".into(),
                        });
                    };
                    let mut file = Media::new(*kind);
                    file.size_bytes = Some(bytes.len() as u64);
                    file.source = Some(media.clone());
                    file.mime_type = mime_type.clone();
                    file.caption = caption.clone();
                    StoryBody::Media(file)
                }
            };
            state.stories.next += 1;
            let me = seed::self_contact(&new.account_id);
            let story = Story {
                id: story_id(state.stories.next),
                client_id: Some(new.client_id.clone()),
                account_id: new.account_id.clone(),
                author: me,
                author_name: None,
                mine: true,
                posted_at: Timestamp::now(),
                expires_at: None,
                body,
                mentions: Vec::new(),
                viewed: true,
                view_count: Some(0),
            };
            state
                .stories
                .stories
                .entry(new.account_id.clone())
                .or_default()
                .push(story.clone());
            state
                .stories
                .posted
                .insert(new.client_id.clone(), story.clone());
            story
        };
        self.emit(ProviderEvent::StoryUpserted(story.clone()));
        Ok(story)
    }

    pub(crate) fn story_delete(&self, account: &AccountId, id: &MessageId) -> ProviderResult<()> {
        {
            let mut state = self.state();
            enter(&mut state, "delete")?;
            if let Some(list) = state.stories.stories.get_mut(account) {
                list.retain(|known| &known.id != id);
            }
            state.stories.deleted.push(id.clone());
        }
        self.emit(ProviderEvent::StoryRemoved {
            account_id: account.clone(),
            story_id: id.clone(),
        });
        Ok(())
    }

    pub(crate) fn story_view(
        &self,
        account: &AccountId,
        story: &MessageId,
        author: &ContactId,
    ) -> ProviderResult<()> {
        if self.is_unavailable(account, client_provider::Feature::Stories) {
            return Err(ProviderError::Unsupported("marking a story as seen"));
        }
        let mut state = self.state();
        enter(&mut state, "view")?;
        let known = state
            .stories
            .stories
            .get(account)
            .is_some_and(|list| list.iter().any(|known| &known.id == story));
        if !known {
            return Err(gone());
        }
        // A receipt is sent once per story, whatever the repeats.
        let entry = (account.clone(), story.clone(), author.clone());
        if !state.stories.views.contains(&entry) {
            state.stories.views.push(entry);
        }
        Ok(())
    }

    pub(crate) fn story_viewers_of(
        &self,
        account: &AccountId,
        story: &MessageId,
    ) -> ProviderResult<Vec<StoryViewer>> {
        if self.is_unavailable(account, client_provider::Feature::Stories) {
            return Err(ProviderError::Unsupported("who saw a story"));
        }
        let mut state = self.state();
        enter(&mut state, "viewers")?;
        Ok(state
            .stories
            .viewers
            .get(&(account.clone(), story.clone()))
            .cloned()
            .unwrap_or_default())
    }

    /// Records a reply or a reaction and lets `send` put the message in
    /// the author's chat (the same idempotency as any message).
    pub(crate) async fn story_answer(
        &self,
        story: &Story,
        message: OutgoingMessage,
        reaction: bool,
    ) -> ProviderResult<SendReceipt> {
        {
            let mut state = self.state();
            enter(&mut state, if reaction { "react" } else { "reply" })?;
            if let Some(receipt) = state.stories.answered.get(&message.client_id) {
                return Ok(receipt.clone());
            }
            let alive = state
                .stories
                .stories
                .get(&story.account_id)
                .is_some_and(|list| list.iter().any(|known| known.id == story.id));
            if !alive {
                return Err(gone());
            }
            let log = if reaction {
                &mut state.stories.reactions
            } else {
                &mut state.stories.replies
            };
            log.push((story.id.clone(), message.clone()));
        }
        let client_id = message.client_id.clone();
        let receipt = <Self as client_provider::Provider>::send(self, message).await?;
        self.state()
            .stories
            .answered
            .insert(client_id, receipt.clone());
        Ok(receipt)
    }

    pub(crate) fn story_muted_list(&self, account: &AccountId) -> ProviderResult<Vec<ContactId>> {
        let mut state = self.state();
        enter(&mut state, "muted")?;
        Ok(state
            .stories
            .muted
            .get(account)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) fn story_set_muted(
        &self,
        account: &AccountId,
        author: &ContactId,
        muted: bool,
    ) -> ProviderResult<()> {
        let mut state = self.state();
        enter(&mut state, "mute")?;
        let list = state.stories.muted.entry(account.clone()).or_default();
        list.retain(|known| known != author);
        if muted {
            list.push(author.clone());
        }
        Ok(())
    }

    pub(crate) fn story_privacy_of(&self, account: &AccountId) -> ProviderResult<StoryPrivacy> {
        let mut state = self.state();
        enter(&mut state, "privacy")?;
        Ok(state
            .stories
            .privacy
            .get(account)
            .cloned()
            .unwrap_or_default())
    }

    pub(crate) fn story_set_privacy(
        &self,
        account: &AccountId,
        privacy: &StoryPrivacy,
    ) -> ProviderResult<()> {
        let mut state = self.state();
        enter(&mut state, "set-privacy")?;
        state
            .stories
            .privacy
            .insert(account.clone(), privacy.clone());
        Ok(())
    }

    /// One story that appears by itself, for the ambient traffic: a text
    /// from one of the account's contacts. Returns nothing when no
    /// account has a direct chat.
    pub(crate) fn ambient_story(&self) {
        let pick = {
            let mut state = self.state();
            let people: Vec<(AccountId, ContactId, String)> = state
                .world
                .chats
                .iter()
                .filter(|chat| chat.kind == ChatKind::Direct)
                .map(|chat| {
                    (
                        chat.account_id.clone(),
                        ContactId::new(chat.id.as_str()),
                        chat.title.clone(),
                    )
                })
                .collect();
            if people.is_empty() {
                return;
            }
            let at = state.rng.below(people.len() as u64) as usize;
            let line = *state.rng.pick(&[
                "Coffee first, opinions later.",
                "Golden hour again.",
                "Who is up for Friday?",
                "New week, same chair.",
            ]);
            let colour = *state
                .rng
                .pick(&[0x0B_6E_4F, 0x5B_2A_86, 0x1D_4E_89, 0xB0_3A_2E]);
            let font = *state.rng.pick(&[0u8, 1, 2, 7]);
            (people[at].clone(), line, colour, font)
        };
        let ((account, author, name), line, colour, font) = pick;
        self.contact_posts_story(
            &account,
            &author,
            &name,
            StoryBody::Text {
                text: line.to_owned(),
                style: StoryStyle {
                    background: colour,
                    font: StoryFont(font),
                },
            },
        );
    }
}
