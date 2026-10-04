//! Forwarding by naming the message: `POST /v1/messages/{messageId}/forward`.
//!
//! ```text
//! POST /v1/messages/{messageId}/forward
//! Idempotency-Key: <ForwardItem::client_id>
//! { "to": ["<chat id>"] }
//! ```
//!
//! The API passes the stored message on the way WhatsApp does (the file
//! WhatsApp already holds, the "Forwarded" label, "Forwarded many times"
//! from the fifth forward) and answers one `queued` message per chat. It
//! takes up to five chats in a request, all or nothing. The client's
//! contract is one idempotency key per copy and a refusal that fails one
//! copy alone, so each item is its own request, under its own key: a
//! repeat answers the message already queued, never a second one.
//!
//! What the API refuses is said in the neutral codes of
//! [`client_provider::refusal`], which the client words the way it words
//! its own refusals. A deployment without the route (`404`, "Route not
//! found.") is "not available yet": remembered in
//! [`Missing`](crate::availability::Missing), answered as `Unsupported`,
//! and asked again later.
//!
//! The copy carries no `clientMessageId`: the route takes no `metadata`,
//! so the client knows its copy by the id this answers.

use crate::availability::no_route;
use crate::client::WuapiClient;
use crate::error::from_sdk;
use crate::events::EventSource;
use crate::mapping;
use client_provider::{
    refusal, AccountId, Feature, ForwardItem, ProviderError, ProviderResult, SendReceipt,
};
use wuapi::types as api;

const UNSUPPORTED: &str = "forwarding a message by naming it";

/// `details.reason` of `400 not_forwardable`, as the neutral code and the
/// words that go with it when the client has none of its own.
fn not_forwardable(reason: Option<&str>, message: String) -> ProviderError {
    let code = match reason {
        Some("deleted") => refusal::FORWARD_DELETED,
        Some("view_once") => refusal::FORWARD_VIEW_ONCE,
        // Still queued, or it never went.
        Some("not_sent") => refusal::FORWARD_NOT_SENT,
        // wuapi never stored the file and has nothing to fetch it with.
        Some("media_not_stored") => refusal::FORWARD_NO_FILE,
        // `poll`, `calendar_event`, `reaction`, `unknown`, `empty`, and
        // `story` (a contact's story: WhatsApp forwards only the account's
        // own), and any reason newer than this adapter.
        _ => refusal::FORWARD_KIND,
    };
    ProviderError::Rejected {
        code: code.to_owned(),
        message,
    }
}

/// The SDK's error for one forward, as what that copy fails with.
fn refused(error: wuapi::Error) -> ProviderError {
    match error {
        wuapi::Error::Api {
            code,
            message,
            details,
            ..
        } if code == "not_forwardable" => {
            let reason = details
                .as_ref()
                .and_then(|details| details.get("reason"))
                .and_then(|reason| reason.as_str());
            not_forwardable(reason, message)
        }
        wuapi::Error::Api { code, .. } if code == "media_expired" => ProviderError::Rejected {
            code: refusal::FORWARD_FILE_GONE.to_owned(),
            message: "WhatsApp no longer has this file.".to_owned(),
        },
        wuapi::Error::Api { code, .. } if code == "media_too_large" => ProviderError::Rejected {
            code: refusal::FORWARD_TOO_LARGE.to_owned(),
            message: "This file is too large to forward.".to_owned(),
        },
        other => from_sdk(other),
    }
}

/// Forwards `items`, one request each. One answer per item, in order.
pub(crate) async fn forward(
    client: &WuapiClient,
    events: &dyn EventSource,
    account: &AccountId,
    items: &[ForwardItem],
) -> ProviderResult<Vec<ProviderResult<SendReceipt>>> {
    if client.missing.is(Feature::ForwardAny, account.as_str()) {
        return Err(ProviderError::Unsupported(UNSUPPORTED));
    }
    let mut answers = Vec::with_capacity(items.len());
    for item in items {
        let request = api::ForwardMessageRequest::new(vec![item.to.to_string()]);
        let answer = client
            .sdk()
            .messages()
            .forward(item.message.as_str(), request)
            .idempotency_key(item.client_id.as_str())
            .await;
        let list = match answer {
            Ok(list) => list,
            Err(error) if no_route(&error) => {
                client.missing.no_route(Feature::ForwardAny);
                return Err(ProviderError::Unsupported(UNSUPPORTED));
            }
            // A revoked key is not about this message.
            Err(error @ wuapi::Error::Api { status: 401, .. }) => return Err(from_sdk(error)),
            Err(error) => {
                answers.push(Err(refused(error)));
                continue;
            }
        };
        client.missing.works(Feature::ForwardAny, account.as_str());
        // One chat was named, so one message is answered.
        let Some(sent) = list.items.first() else {
            answers.push(Err(ProviderError::Protocol(
                "the API answered no message for a forward".into(),
            )));
            continue;
        };
        answers.push(match mapping::message(sent) {
            Some(mapped) => {
                // From here on its ticks are followed.
                events.accepted(sent);
                Ok(SendReceipt {
                    message_id: mapped.id,
                    status: mapped.status,
                    timestamp: Some(mapped.timestamp),
                })
            }
            None => Err(ProviderError::Protocol(
                "the forwarded message came back unreadable".into(),
            )),
        });
    }
    Ok(answers)
}
