//! What is done to a message after it was sent: editing it, deleting it,
//! starring it, reacting to it, passing it on.
//!
//! Each of these shows at once and is told to the provider in the
//! background, like a chat's pin or a vote in a poll. A dropped connection
//! is retried with backoff, quietly: every one of them sets a value, so
//! repeating it is harmless. Only a refusal by the provider (WhatsApp's
//! edit window has closed, the message is somebody else's) or running out
//! of attempts puts the message back as it was, and says why
//! ([`StoreChange::Problem`]).
//!
//! A message that has not left yet is another matter: it is still in the
//! outbox, so editing it rewrites what will be sent and deleting it takes
//! it back, with no provider involved.
//!
//! Reactions and forwards are messages of their own and go through the
//! outbox like any other.

use super::new_client_id;
use super::SyncEngine;
use crate::store::{StoreChange, StoreError};
use client_provider::{
    AccountId, ChatId, ClientMessageId, DeliveryStatus, Direction, Feature, Message,
    MessageContent, MessageId, OutgoingContent, OutgoingMessage, ProviderError,
};

/// What is being done.
#[derive(Clone, Debug)]
enum Action {
    Edit(String),
    Delete { for_everyone: bool },
    Star(bool),
}

impl Action {
    /// Which of a message's settings it is about: a newer action of the
    /// same kind supersedes an older one still being retried.
    fn kind(&self) -> u8 {
        match self {
            Action::Edit(_) => 0,
            Action::Delete { .. } => 1,
            Action::Star(_) => 2,
        }
    }

    /// "The message was not …".
    fn undone(&self) -> &'static str {
        match self {
            Action::Edit(_) => "edited",
            Action::Delete { .. } => "deleted",
            Action::Star(true) => "starred",
            Action::Star(false) => "unstarred",
        }
    }
}

/// Why a message cannot be forwarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardRefusal {
    /// The provider cannot forward at all.
    NoForwards,
    /// The message was deleted.
    Deleted,
    /// A view-once message.
    ViewOnce,
    /// A reaction, a notice or a kind this client does not model.
    Kind,
    /// Only text can be forwarded with this provider so far.
    NotAvailableYet,
    /// It is still on its way, or never went.
    NotSent,
    /// The provider has no file for it.
    NoFile,
    /// WhatsApp no longer has the file.
    FileGone,
    /// The file is larger than the provider passes on.
    TooLarge,
}

impl ForwardRefusal {
    /// The reason, in words for the user.
    pub fn reason(self) -> &'static str {
        match self {
            Self::NoForwards => "This provider cannot forward messages",
            Self::Deleted => "This message was deleted",
            Self::ViewOnce => "A view-once message cannot be forwarded",
            Self::Kind => "This kind of message cannot be forwarded",
            Self::NotAvailableYet => "Not available yet from this provider",
            Self::NotSent => "It has not been sent yet",
            Self::NoFile => "This file is not available",
            Self::FileGone => "WhatsApp no longer has this file",
            Self::TooLarge => "This file is too large to forward",
        }
    }

    /// The refusal a provider's neutral code stands for
    /// ([`client_provider::refusal`]), so that what a provider refuses
    /// when the copy is sent reads like what is refused before it is
    /// queued.
    pub fn of_code(code: &str) -> Option<Self> {
        use client_provider::refusal as code_of;
        Some(match code {
            code_of::FORWARD_DELETED => Self::Deleted,
            code_of::FORWARD_VIEW_ONCE => Self::ViewOnce,
            code_of::FORWARD_KIND => Self::Kind,
            code_of::FORWARD_NOT_SENT => Self::NotSent,
            code_of::FORWARD_NO_FILE => Self::NoFile,
            code_of::FORWARD_FILE_GONE => Self::FileGone,
            code_of::FORWARD_TOO_LARGE => Self::TooLarge,
            _ => return None,
        })
    }
}

/// Why a message was not queued to be forwarded.
#[derive(Debug, thiserror::Error)]
pub enum ForwardError {
    /// It cannot be forwarded, with the reason.
    #[error("{}", .0.reason())]
    Refused(ForwardRefusal),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl SyncEngine {
    /// Replaces the text of a message the account sent. The new text shows
    /// at once, marked as edited; see the module's notes for the rest.
    ///
    /// A message still waiting in the outbox is simply sent with the new
    /// text: nobody ever saw the old one, so it is not an edit.
    pub fn edit_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        text: String,
    ) -> Result<(), StoreError> {
        let store = &self.inner.store;
        let Some(before) = store.message(account, message)? else {
            return Ok(());
        };
        if before.direction != Direction::Outgoing
            || !matches!(before.content, MessageContent::Text { .. })
            || before.deleted
        {
            return Ok(());
        }
        if before.content == MessageContent::text(text.clone()) {
            return Ok(());
        }
        if before.status == DeliveryStatus::Pending {
            if let Some(client_id) = &before.client_id {
                if store.outbox_edit_text(client_id, &text)? {
                    return Ok(());
                }
            }
            if is_local(message) {
                store.notify(StoreChange::Problem {
                    message: "The message is being sent right now: edit it in a moment.".into(),
                });
                return Ok(());
            }
        }
        let mut edited = before.clone();
        edited.content = MessageContent::text(text.clone());
        edited.edited = true;
        store.upsert_message(&edited)?;
        self.tell_provider(account, chat, before, Action::Edit(text));
        Ok(())
    }

    /// Deletes a message: for everyone (the placeholder takes its place),
    /// or for the account alone (it goes from the conversation).
    ///
    /// A message still waiting in the outbox is taken back instead.
    pub fn delete_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        for_everyone: bool,
    ) -> Result<(), StoreError> {
        let store = &self.inner.store;
        let Some(before) = store.message(account, message)? else {
            return Ok(());
        };
        let unsent = matches!(
            before.status,
            DeliveryStatus::Pending | DeliveryStatus::Failed { .. }
        );
        if before.direction == Direction::Outgoing && unsent {
            if let Some(client_id) = &before.client_id {
                if self.cancel_send(client_id)? {
                    return Ok(());
                }
            }
            if is_local(message) {
                // It failed, or its entry is gone: only the bubble is left.
                if matches!(before.status, DeliveryStatus::Failed { .. }) {
                    store.remove_message(account, message)?;
                } else {
                    store.notify(StoreChange::Problem {
                        message: "The message is being sent right now: delete it in a moment."
                            .into(),
                    });
                }
                return Ok(());
            }
        }
        if for_everyone {
            let mut deleted = before.clone();
            deleted.deleted = true;
            store.upsert_message(&deleted)?;
        } else {
            store.remove_message(account, message)?;
        }
        self.tell_provider(account, chat, before, Action::Delete { for_everyone });
        Ok(())
    }

    /// Stars or unstars a message.
    pub fn star_message(
        &self,
        account: &AccountId,
        chat: &ChatId,
        message: &MessageId,
        starred: bool,
    ) -> Result<(), StoreError> {
        let store = &self.inner.store;
        let Some(before) = store.message(account, message)? else {
            return Ok(());
        };
        if before.extras.starred == starred || is_local(message) {
            return Ok(());
        }
        let mut now = before.clone();
        now.extras.starred = starred;
        store.upsert_message(&now)?;
        self.tell_provider(account, chat, before, Action::Star(starred));
        Ok(())
    }

    /// Reacts to a message with one emoji; an empty one takes the
    /// account's reaction back. It is queued like a message: the chip
    /// shows at once and the outbox delivers it.
    pub fn react(
        &self,
        account: &AccountId,
        chat: &ChatId,
        target: &MessageId,
        emoji: &str,
    ) -> Result<ClientMessageId, StoreError> {
        self.send(OutgoingMessage {
            client_id: new_client_id(),
            account_id: account.clone(),
            chat_id: chat.clone(),
            content: OutgoingContent::Reaction {
                target: target.clone(),
                emoji: emoji.to_owned(),
            },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: false,
        })
    }

    /// Passes a text on to a chat, with WhatsApp's "Forwarded" mark. Each
    /// chat gets a message of its own through the outbox.
    pub fn forward_text(
        &self,
        account: &AccountId,
        to: &ChatId,
        text: String,
    ) -> Result<ClientMessageId, StoreError> {
        self.send(OutgoingMessage {
            client_id: new_client_id(),
            account_id: account.clone(),
            chat_id: to.clone(),
            content: OutgoingContent::Text { body: text },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: true,
        })
    }

    /// Why `message` cannot be passed on, or `None` when it can: a text
    /// always can (it is sent again with the mark where it cannot be
    /// named); any other kind needs a provider that forwards by naming
    /// the message ([`Capabilities::forward_any`]) and has that working
    /// for the account right now ([`SyncEngine::feature_unavailable`]).
    pub fn forward_refusal(&self, message: &Message) -> Option<ForwardRefusal> {
        use ForwardRefusal as R;
        if !self.inner.capabilities.forwards {
            return Some(R::NoForwards);
        }
        if message.deleted {
            return Some(R::Deleted);
        }
        match &message.content {
            MessageContent::Text { .. } => return None,
            MessageContent::Reaction { .. }
            | MessageContent::System(_)
            | MessageContent::Unsupported { .. } => return Some(R::Kind),
            _ => {}
        }
        let capabilities = &self.inner.capabilities;
        if !capabilities.forward_any
            || self.feature_unavailable(&message.account_id, Feature::ForwardAny)
        {
            return Some(R::NotAvailableYet);
        }
        // Kinds some backends do not pass on.
        match &message.content {
            MessageContent::Poll(_) if !capabilities.forward_polls => return Some(R::Kind),
            MessageContent::Event(_) if !capabilities.forward_events => return Some(R::Kind),
            _ => {}
        }
        if message.extras.view_once {
            return Some(R::ViewOnce);
        }
        if is_local(&message.id) || matches!(message.status, DeliveryStatus::Failed { .. }) {
            return Some(R::NotSent);
        }
        if let MessageContent::Media(media) = &message.content {
            let Some(source) = &media.source else {
                return Some(R::NoFile);
            };
            if matches!(
                self.media_state(&crate::file_key(source.as_str())),
                crate::MediaState::Expired(_)
            ) || matches!(
                self.media_state(&crate::thumbnail_key(source.as_str())),
                crate::MediaState::Expired(_)
            ) {
                return Some(R::FileGone);
            }
        }
        None
    }

    /// Passes `source` on to a chat, marked as forwarded. It is queued
    /// like any message: written to the store with its pending bubble in
    /// one transaction, then handed to the provider under a client id
    /// that is the same for every attempt, so a dropped connection never
    /// makes a second copy. A text is sent again with the mark; any other
    /// kind is forwarded by naming the original
    /// ([`Provider::forward_messages`]), so no file passes through here.
    /// A message that cannot be forwarded is refused with the reason.
    pub fn forward_message(
        &self,
        account: &AccountId,
        source: &Message,
        to: &ChatId,
    ) -> Result<ClientMessageId, ForwardError> {
        if let Some(reason) = self.forward_refusal(source) {
            return Err(ForwardError::Refused(reason));
        }
        // A text the provider holds is named like anything else: it is
        // then the original that is passed on (with what WhatsApp counts
        // of its forwards). One it does not hold, or cannot name right
        // now, is sent again with the mark.
        if let MessageContent::Text { body } = &source.content {
            if !self.names_text(account, source)? {
                return Ok(self.forward_text(account, to, body.clone())?);
            }
        }
        let message = OutgoingMessage {
            client_id: new_client_id(),
            account_id: account.clone(),
            chat_id: to.clone(),
            content: OutgoingContent::Forward {
                source: source.id.clone(),
            },
            reply_to: None,
            mentions: Vec::new(),
            forwarded: true,
        };
        self.inner.store.enqueue_forward(
            &message,
            source,
            self.queue_time(),
            self.inner.config.outbox.max_age,
        )?;
        self.inner.outbox_wake.notify_one();
        Ok(message.client_id)
    }

    /// Whether a text is forwarded by naming it: the provider can, and the
    /// message is one it holds (a message of a chat, with the provider's
    /// own id, that went out).
    fn names_text(&self, account: &AccountId, source: &Message) -> Result<bool, StoreError> {
        if !self.inner.capabilities.forward_any
            || self.feature_unavailable(account, Feature::ForwardAny)
            || is_local(&source.id)
            || matches!(
                source.status,
                DeliveryStatus::Pending | DeliveryStatus::Failed { .. }
            )
        {
            return Ok(false);
        }
        Ok(self.inner.store.message(account, &source.id)?.is_some())
    }

    /// Tells the provider what was done to a message, until it answers;
    /// `before` is the message as it was, to put back if it refuses.
    fn tell_provider(&self, account: &AccountId, chat: &ChatId, before: Message, action: Action) {
        let key = (account.clone(), before.id.clone(), action.kind());
        let generation = {
            let mut ops = self.inner.message_ops.lock().expect("message ops lock");
            let generation = ops.get(&key).copied().unwrap_or(0) + 1;
            ops.insert(key.clone(), generation);
            generation
        };
        let this = self.clone();
        let chat = chat.clone();
        self.inner.runtime.spawn(async move {
            let (account, message, _) = &key;
            let is_newest = |this: &SyncEngine| {
                let ops = this.inner.message_ops.lock().expect("message ops lock");
                ops.get(&key) == Some(&generation)
            };
            let mut attempt = 0;
            let failure = loop {
                attempt += 1;
                let provider = &this.inner.provider;
                let outcome = match &action {
                    Action::Edit(text) => {
                        this.bounded(provider.edit_message(account, &chat, message, text))
                            .await
                    }
                    Action::Delete { for_everyone } => {
                        this.bounded(provider.delete_message(
                            account,
                            &chat,
                            message,
                            *for_everyone,
                        ))
                        .await
                    }
                    Action::Star(starred) => {
                        this.bounded(provider.star_message(account, &chat, message, *starred))
                            .await
                    }
                };
                match outcome {
                    Ok(()) => return,
                    Err(error)
                        if error.is_transient()
                            && attempt < this.inner.config.chat_change_attempts =>
                    {
                        tracing::debug!(%error, "message action: will be retried");
                        let wait = error.retry_after().unwrap_or(this.backoff(attempt));
                        tokio::time::sleep(wait).await;
                        if !is_newest(&this) {
                            return;
                        }
                    }
                    Err(error) => break error,
                }
            };
            if this.note_unauthorized(&failure) {
                return;
            }
            tracing::warn!(error = %failure, "a message action did not reach the provider");
            if !is_newest(&this) {
                return;
            }
            // Put back what this action changed, on the message as it
            // stands now: another kind of change made meanwhile stays.
            let store = &this.inner.store;
            let restored = match store.message(account, message) {
                Ok(Some(mut now)) => {
                    match &action {
                        Action::Edit(_) => {
                            now.content = before.content.clone();
                            now.edited = before.edited;
                        }
                        Action::Delete { .. } => now.deleted = before.deleted,
                        Action::Star(_) => now.extras.starred = before.extras.starred,
                    }
                    now
                }
                // Deleted for the account alone: the row itself comes back.
                _ => before.clone(),
            };
            if let Err(error) = store.upsert_message(&restored) {
                tracing::error!(%error, "could not undo a message action");
            }
            let why = reason(&failure);
            store.notify(StoreChange::Problem {
                message: format!("The message was not {}: {why}", action.undone()),
            });
        });
    }
}

/// A message that only this client knows: it has no id at the provider.
fn is_local(message: &MessageId) -> bool {
    message.as_str().starts_with("local:")
}

/// Why it did not go through, in words a person can use.
fn reason(failure: &ProviderError) -> String {
    match failure {
        failure if failure.is_transient() => "the connection kept failing. Try again.".to_owned(),
        ProviderError::Rejected { message, .. } if !message.trim().is_empty() => message.clone(),
        other => other.to_string(),
    }
}

impl crate::Store {
    /// Takes a message out of the store: it is gone from its
    /// conversation. Its chat's preview follows from what is left.
    pub fn remove_message(
        &self,
        account: &AccountId,
        message: &MessageId,
    ) -> Result<(), StoreError> {
        let chat = self.write(|tx| {
            use rusqlite::OptionalExtension as _;
            let chat: Option<String> = tx
                .query_row(
                    "SELECT chat_id FROM messages WHERE account_id = ?1 AND id = ?2",
                    rusqlite::params![account.as_str(), message.as_str()],
                    |row| row.get(0),
                )
                .optional()?;
            tx.execute(
                "DELETE FROM messages WHERE account_id = ?1 AND id = ?2",
                rusqlite::params![account.as_str(), message.as_str()],
            )?;
            Ok(chat)
        })?;
        if let Some(chat) = chat {
            self.notify_messages(vec![(account.clone(), ChatId::new(chat))]);
        }
        Ok(())
    }
}
