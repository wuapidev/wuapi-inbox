//! The worked example of `docs/PROVIDERS.md`: a whole provider in one
//! file, over a backend that lives in memory and says back what it is
//! sent. Copy it to start your own.
//!
//! It is not in the application unless it is asked for:
//!
//! ```sh
//! cargo run -p wuapi-inbox --features provider-example -- --provider example
//! ```
//!
//! # The smallest provider
//!
//! Nine methods are required; everything else has a default that answers
//! [`ProviderError::Unsupported`]. This is all of them, for a backend with
//! nothing in it. The README shows the same lines, and a test keeps the
//! two alike.
//!
//! ```rust
//! use async_trait::async_trait;
//! use client_provider::*;
//! use futures::StreamExt as _;
//!
//! pub struct Minimal;
//!
//! #[async_trait]
//! impl Provider for Minimal {
//!     fn id(&self) -> &'static str {
//!         "minimal"
//!     }
//!     // What the backend can do. The UI hides the rest.
//!     fn capabilities(&self) -> Capabilities {
//!         Capabilities { chat_list: true, replies: true, ..Capabilities::none() }
//!     }
//!     async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
//!         Ok(Vec::new())
//!     }
//!     async fn list_chats(&self, _: &AccountId, _: Option<Cursor>) -> ProviderResult<Page<Chat>> {
//!         Ok(Page::last(Vec::new()))
//!     }
//!     async fn fetch_messages(
//!         &self, _: &AccountId, _: &ChatId, _: Option<Cursor>, _limit: u32,
//!     ) -> ProviderResult<Page<Message>> {
//!         Ok(Page::last(Vec::new())) // newest first
//!     }
//!     // The same client_id must always answer the same message id.
//!     async fn send(&self, message: OutgoingMessage) -> ProviderResult<SendReceipt> {
//!         let message_id = MessageId::new(message.client_id.as_str());
//!         Ok(SendReceipt { message_id, status: DeliveryStatus::Sent, timestamp: None })
//!     }
//!     async fn mark_read(&self, _: &AccountId, _: &ChatId, _: Option<&MessageId>) -> ProviderResult<()> {
//!         Ok(())
//!     }
//!     async fn download_media(&self, _: &AccountId, _: &MediaRef) -> ProviderResult<MediaData> {
//!         Err(ProviderError::Unsupported("media"))
//!     }
//!     // Live updates: a stream of events, pushed or polled for.
//!     async fn subscribe(&self) -> ProviderResult<EventStream> {
//!         Ok(futures::stream::pending().boxed())
//!     }
//! }
//! ```

use async_trait::async_trait;
use client_provider::{
    Account, AccountId, Capabilities, Chat, ChatId, ChatKind, ClientMessageId, ConnectionState,
    ContactId, Cursor, DeliveryStatus, Direction, EventStream, MediaData, MediaRef, Message,
    MessageContent, MessageId, OutgoingContent, OutgoingMessage, Page, Provider, ProviderError,
    ProviderEvent, ProviderResult, SendReceipt, Timestamp,
};
use futures::StreamExt as _;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::sync::broadcast;

/// The one account and the one chat of the backend.
const ACCOUNT: &str = "echo";
const CHAT: &str = "echo";

/// A backend that says back what it is sent: one account, one chat.
pub struct EchoProvider {
    backend: Mutex<Backend>,
    /// What `subscribe` hands out: every listener gets every event.
    events: broadcast::Sender<ProviderEvent>,
}

/// What a real provider keeps on a server.
#[derive(Default)]
struct Backend {
    /// The chat's messages, oldest first.
    messages: Vec<Message>,
    /// The answer to each send, by the client's id for it: a send that is
    /// repeated gets the same answer and adds nothing.
    receipts: HashMap<ClientMessageId, SendReceipt>,
}

impl Default for EchoProvider {
    fn default() -> Self {
        let provider = Self {
            backend: Mutex::default(),
            events: broadcast::channel(64).0,
        };
        let hello = message(
            "hello",
            None,
            Direction::Incoming,
            "Write something and I will say it back.",
        );
        provider.backend().messages.push(hello);
        provider
    }
}

impl EchoProvider {
    fn backend(&self) -> std::sync::MutexGuard<'_, Backend> {
        self.backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// A text message of the chat, in the neutral model.
fn message(
    id: &str,
    client_id: Option<ClientMessageId>,
    direction: Direction,
    body: &str,
) -> Message {
    Message {
        id: MessageId::new(id),
        client_id,
        account_id: AccountId::new(ACCOUNT),
        chat_id: ChatId::new(CHAT),
        sender: ContactId::new(match direction {
            Direction::Incoming => "echo",
            Direction::Outgoing => "me",
        }),
        sender_name: None,
        direction,
        timestamp: Timestamp::now(),
        content: MessageContent::text(body),
        reply_to: None,
        status: match direction {
            Direction::Incoming => DeliveryStatus::Delivered,
            Direction::Outgoing => DeliveryStatus::Sent,
        },
        edited: false,
        deleted: false,
        extras: Default::default(),
    }
}

/// A refusal that will stay a refusal: the client shows it and does not
/// try again.
fn rejected(code: &str, message: &str) -> ProviderError {
    ProviderError::Rejected {
        code: code.to_owned(),
        message: message.to_owned(),
    }
}

#[async_trait]
impl Provider for EchoProvider {
    fn id(&self) -> &'static str {
        "example"
    }

    /// Only what is true: the chat list is the real one and events are
    /// pushed. Everything else stays off, and the window offers none of it.
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            chat_list: true,
            realtime_push: true,
            ..Capabilities::none()
        }
    }

    async fn list_accounts(&self) -> ProviderResult<Vec<Account>> {
        Ok(vec![Account {
            id: AccountId::new(ACCOUNT),
            display_name: "Echo".into(),
            phone: None,
            self_contact: Some(ContactId::new("me")),
            connection: ConnectionState::Connected,
            settings: Default::default(),
        }])
    }

    async fn list_chats(
        &self,
        account: &AccountId,
        _cursor: Option<Cursor>,
    ) -> ProviderResult<Page<Chat>> {
        if account.as_str() != ACCOUNT {
            return Err(rejected("not_found", "There is no such account."));
        }
        Ok(Page::last(vec![Chat {
            id: ChatId::new(CHAT),
            account_id: account.clone(),
            kind: ChatKind::Direct,
            title: "Echo".into(),
            avatar: None,
            unread_count: 0,
            pinned: false,
            muted: false,
            archived: false,
            // With the newest message in the listing, the chat list has
            // its preview before any history is asked for.
            last_message: self.backend().messages.last().cloned(),
            unknown: Default::default(),
            picture_id: None,
            pinned_at: None,
        }]))
    }

    /// Newest first, a page at a time. The cursor is ours to choose: here,
    /// how many messages the pages before this one held.
    async fn fetch_messages(
        &self,
        _account: &AccountId,
        chat: &ChatId,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> ProviderResult<Page<Message>> {
        if chat.as_str() != CHAT {
            return Err(rejected("not_found", "There is no such chat."));
        }
        let skip = match &cursor {
            None => 0,
            Some(cursor) => cursor
                .as_str()
                .parse::<usize>()
                .map_err(|_| ProviderError::Protocol("a cursor that is not ours".into()))?,
        };
        let backend = self.backend();
        let items: Vec<Message> = backend
            .messages
            .iter()
            .rev()
            .skip(skip)
            .take(limit as usize)
            .cloned()
            .collect();
        let read = skip + items.len();
        let next_cursor = (read < backend.messages.len()).then(|| Cursor::new(read.to_string()));
        Ok(Page { items, next_cursor })
    }

    /// The one rule that cannot bend: sent again with the same
    /// `client_id`, a message is still one message, with the same id.
    async fn send(&self, outgoing: OutgoingMessage) -> ProviderResult<SendReceipt> {
        let OutgoingContent::Text { body } = &outgoing.content else {
            return Err(rejected("text_only", "Echo only takes text."));
        };
        if outgoing.chat_id.as_str() != CHAT {
            return Err(rejected("not_found", "There is no such chat."));
        }
        let mut backend = self.backend();
        if let Some(receipt) = backend.receipts.get(&outgoing.client_id) {
            return Ok(receipt.clone());
        }
        let key = outgoing.client_id.as_str();
        // The client's id comes back on our copy of the message, so the
        // client knows it for the bubble it is already showing.
        let sent = message(
            &format!("sent-{key}"),
            Some(outgoing.client_id.clone()),
            Direction::Outgoing,
            body,
        );
        let reply = message(&format!("reply-{key}"), None, Direction::Incoming, body);
        let receipt = SendReceipt {
            message_id: sent.id.clone(),
            status: DeliveryStatus::Sent,
            timestamp: Some(sent.timestamp),
        };
        backend
            .receipts
            .insert(outgoing.client_id.clone(), receipt.clone());
        backend.messages.push(sent);
        backend.messages.push(reply.clone());
        // Nobody listening is not an error: the reply is in the history.
        let _ = self.events.send(ProviderEvent::MessageUpserted(reply));
        Ok(receipt)
    }

    async fn mark_read(
        &self,
        _account: &AccountId,
        _chat: &ChatId,
        _up_to: Option<&MessageId>,
    ) -> ProviderResult<()> {
        Ok(())
    }

    async fn download_media(
        &self,
        _account: &AccountId,
        _media: &MediaRef,
    ) -> ProviderResult<MediaData> {
        Err(ProviderError::Unsupported("media"))
    }

    /// Live updates. This backend pushes; one that cannot would ask its
    /// server at an interval in a task of its own and send what it finds
    /// into the same channel.
    async fn subscribe(&self) -> ProviderResult<EventStream> {
        let receiver = self.events.subscribe();
        Ok(
            futures::stream::unfold(receiver, |mut receiver| async move {
                loop {
                    match receiver.recv().await {
                        Ok(event) => return Some((event, receiver)),
                        // Events are hints: one that was missed is made up
                        // for when the client reads the history again.
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => return None,
                    }
                }
            })
            .boxed(),
        )
    }
}
