//! Forwarding in the mock: any message can be passed on to other chats,
//! each copy marked as forwarded, and a test can make some of them fail.

use crate::{seed, MockProvider, State};
use client_provider::{
    AccountId, ClientMessageId, DeliveryStatus, Direction, ForwardItem, Message, MessageContent,
    MessageId, ProviderError, ProviderResult, SendReceipt, Timestamp,
};
use std::collections::{HashMap, VecDeque};

/// What the mock keeps for forwards.
#[derive(Default)]
pub(crate) struct Forwards {
    /// The provider says it cannot forward by naming a message.
    off: bool,
    /// Every item it was asked to forward, in order, repeats included.
    calls: Vec<ForwardItem>,
    /// How many times `forward_messages` was called.
    call_count: usize,
    /// Errors the next calls answer with, one per call (the whole call).
    failures: VecDeque<ProviderError>,
    /// Messages that cannot be forwarded, and why.
    refused: HashMap<MessageId, ProviderError>,
    /// The copies made, by client id: the idempotency table.
    done: HashMap<ClientMessageId, SendReceipt>,
}

impl MockProvider {
    /// Makes the provider say it can only forward text, as a backend
    /// without a forward route does. Call it before an engine is made
    /// over the provider: capabilities are read once.
    pub fn set_forward_available(&self, available: bool) {
        self.state().forwards.off = !available;
    }

    pub(crate) fn forward_available(&self) -> bool {
        !self.state().forwards.off
    }

    /// Makes the next `forward_messages` calls fail as a whole with the
    /// given errors, one per call.
    pub fn fail_next_forwards(&self, errors: impl IntoIterator<Item = ProviderError>) {
        self.state().forwards.failures.extend(errors);
    }

    /// Makes one message refuse to be forwarded, as WhatsApp does for a
    /// file it no longer has or a view-once message.
    pub fn refuse_forward_of(&self, message: &MessageId, code: &str, text: &str) {
        self.state().forwards.refused.insert(
            message.clone(),
            ProviderError::Rejected {
                code: code.to_owned(),
                message: text.to_owned(),
            },
        );
    }

    /// The items it was asked to forward, in order, repeats included.
    pub fn forward_calls(&self) -> Vec<ForwardItem> {
        self.state().forwards.calls.clone()
    }

    /// The messages of a chat in the world, oldest first.
    pub fn thread(&self, account: &AccountId, chat: &client_provider::ChatId) -> Vec<Message> {
        self.state()
            .world
            .messages
            .get(&(account.clone(), chat.clone()))
            .cloned()
            .unwrap_or_default()
    }

    /// A forward was asked for while forwarding was not available.
    pub(crate) fn note_forward_refused(&self) {
        self.state().forwards.call_count += 1;
    }

    /// How many times `forward_messages` was called, whatever it
    /// answered.
    pub fn forward_call_count(&self) -> usize {
        self.state().forwards.call_count
    }

    /// How many distinct copies were made.
    pub fn forwarded_copies(&self) -> usize {
        self.state().forwards.done.len()
    }
}

fn forward_one(
    state: &mut State,
    account: &AccountId,
    item: &ForwardItem,
) -> ProviderResult<SendReceipt> {
    if let Some(receipt) = state.forwards.done.get(&item.client_id) {
        return Ok(receipt.clone());
    }
    if let Some(error) = state.forwards.refused.get(&item.message) {
        return Err(error.clone());
    }
    // A story is a message too: one that is up can be named like any.
    let Some(source) = state
        .world
        .messages
        .values()
        .flatten()
        .find(|message| message.id == item.message && &message.account_id == account)
        .cloned()
        .or_else(|| state.stories.as_message(account, &item.message))
    else {
        return Err(ProviderError::Rejected {
            code: "not_found".into(),
            message: "No such message.".into(),
        });
    };
    if source.deleted {
        return Err(ProviderError::Rejected {
            code: "not_forwardable".into(),
            message: "The message was deleted.".into(),
        });
    }
    if source.extras.view_once {
        return Err(ProviderError::Rejected {
            code: "not_forwardable".into(),
            message: "A view-once message cannot be forwarded.".into(),
        });
    }
    let mut content = source.content.clone();
    match &mut content {
        MessageContent::Reaction { .. }
        | MessageContent::System(_)
        | MessageContent::Unsupported { .. } => {
            return Err(ProviderError::Rejected {
                code: "not_forwardable".into(),
                message: "This kind of message cannot be forwarded.".into(),
            });
        }
        MessageContent::Poll(poll) => {
            for option in &mut poll.options {
                option.votes = 0;
            }
            poll.voters = 0;
            poll.chosen = Some(Vec::new());
        }
        _ => {}
    }
    let key = (account.clone(), item.to.clone());
    if !state
        .world
        .chats
        .iter()
        .any(|chat| chat.id == item.to && &chat.account_id == account)
    {
        return Err(ProviderError::Rejected {
            code: "not_found".into(),
            message: "No such chat.".into(),
        });
    }
    let n = state.world.next_message;
    state.world.next_message += 1;
    let now = Timestamp::now();
    let copy = Message {
        id: MessageId::new(format!("m{n}")),
        client_id: Some(item.client_id.clone()),
        account_id: account.clone(),
        chat_id: item.to.clone(),
        sender: seed::self_contact(account),
        sender_name: None,
        direction: Direction::Outgoing,
        timestamp: now,
        content,
        reply_to: None,
        status: DeliveryStatus::Sent,
        edited: false,
        deleted: false,
        extras: client_provider::MessageExtras {
            forwarded: true,
            link: source.extras.link.clone(),
            ..Default::default()
        },
    };
    let receipt = SendReceipt {
        message_id: copy.id.clone(),
        status: DeliveryStatus::Sent,
        timestamp: Some(now),
    };
    state
        .forwards
        .done
        .insert(item.client_id.clone(), receipt.clone());
    state
        .world
        .messages
        .entry(key)
        .or_default()
        .push(copy.clone());
    if let Some(chat) = state
        .world
        .chats
        .iter_mut()
        .find(|chat| chat.id == item.to && &chat.account_id == account)
    {
        chat.last_message = Some(copy);
    }
    Ok(receipt)
}

impl MockProvider {
    pub(crate) fn forward_items(
        &self,
        account: &AccountId,
        items: &[ForwardItem],
    ) -> ProviderResult<Vec<ProviderResult<SendReceipt>>> {
        let mut state = self.state();
        state.forwards.call_count += 1;
        state.forwards.calls.extend(items.iter().cloned());
        if let Some(error) = state.forwards.failures.pop_front() {
            return Err(error);
        }
        Ok(items
            .iter()
            .map(|item| forward_one(&mut state, account, item))
            .collect())
    }
}
