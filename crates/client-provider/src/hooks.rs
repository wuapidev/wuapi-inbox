//! Incoming webhooks: a secret URL that sends a WhatsApp message to one
//! chat when something posts to it, which a provider may be able to make
//! and manage (see [`Capabilities::incoming_webhooks`](crate::Capabilities)).

use crate::ids::{AccountId, ChatId};

/// A URL that sends a message from one number to one fixed chat.
///
/// The URL is the credential: anything that can reach it writes to
/// [`to`](Self::to). The client shows it and never keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IncomingWebhook {
    /// The provider's id for the hook: what a change or a removal names.
    pub id: String,
    /// The number the messages are sent from.
    pub account_id: AccountId,
    /// The chat the messages are sent to. It never changes.
    pub to: ChatId,
    /// A label for the user.
    pub name: String,
    /// A paused hook sends nothing and refuses what is posted to it.
    pub enabled: bool,
    /// Where to post a message.
    pub url: String,
}

/// One change to an incoming webhook.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IncomingWebhookChange {
    /// A new label.
    Rename(String),
    /// Paused (`false`) or sending again (`true`).
    Enabled(bool),
    /// A new URL. The old one stops working at once.
    RotateUrl,
}
