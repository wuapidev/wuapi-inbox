//! Incoming webhooks: secret URLs that send a WhatsApp message from one
//! number to one fixed chat, the ones the dashboard makes under
//! Integrations.
//!
//! * `GET /v1/incoming-webhooks`: every hook the key can see, a page at a
//!   time. The API lists them for the whole key, so the ones of a chat are
//!   picked here, by `accountId` and `to`.
//! * `POST /v1/incoming-webhooks` with `{accountId, name, to}`. The SDK
//!   sends an idempotency key, so a retry after a timeout makes one hook.
//! * `PATCH /v1/incoming-webhooks/{id}` with `name`, `enabled` or
//!   `rotateUrl`. Not retried: a rotation sent twice would be two.
//! * `DELETE /v1/incoming-webhooks/{id}`.
//!
//! `to` is the chat's id as the API gives it (`+584121234567`,
//! `lid:<digits>`, `…@g.us`), which is the [`ChatId`] this adapter
//! already uses. A deployment without the routes (`404` "Route not
//! found") answers `Unsupported`.
//!
//! The URL is the credential: it is handed to the client and never
//! logged.

use crate::availability::no_route;
use crate::client::{WuapiClient, MAX_PAGE};
use crate::error::from_sdk;
use client_provider::{
    AccountId, ChatId, IncomingWebhook, IncomingWebhookChange, ProviderError, ProviderResult,
};
use wuapi::types as api;

/// The API keeps at most 50 per organization; the rest is room to spare.
const MAX_HOOKS: usize = 500;

fn failed(error: wuapi::Error) -> ProviderError {
    if no_route(&error) {
        ProviderError::Unsupported("incoming webhooks")
    } else {
        from_sdk(error)
    }
}

/// `IncomingWebhook` -> [`IncomingWebhook`].
fn hook(wire: api::IncomingWebhook) -> IncomingWebhook {
    IncomingWebhook {
        id: wire.id,
        account_id: AccountId::new(wire.account_id),
        to: ChatId::new(wire.to),
        name: wire.name,
        enabled: wire.enabled,
        url: wire.url,
    }
}

impl WuapiClient {
    /// The hooks that send from `account` to `chat`.
    pub(crate) async fn incoming_webhooks(
        &self,
        account: &str,
        chat: &str,
    ) -> ProviderResult<Vec<IncomingWebhook>> {
        let params = api::IncomingWebhooksListParams {
            limit: Some(i64::from(MAX_PAGE)),
            ..Default::default()
        };
        let listed = self
            .sdk()
            .incoming_webhooks()
            .list(params)
            .to_vec_max(MAX_HOOKS)
            .await
            .map_err(failed)?;
        Ok(listed
            .into_iter()
            .filter(|wire| wire.account_id == account && wire.to == chat)
            .map(hook)
            .collect())
    }

    /// `POST /v1/incoming-webhooks`
    pub(crate) async fn create_incoming_webhook(
        &self,
        account: &str,
        chat: &str,
        name: &str,
    ) -> ProviderResult<IncomingWebhook> {
        self.sdk()
            .incoming_webhooks()
            .create(api::IncomingWebhookCreateRequest::new(account, name, chat))
            .await
            .map(hook)
            .map_err(failed)
    }

    /// `PATCH /v1/incoming-webhooks/{id}`
    pub(crate) async fn update_incoming_webhook(
        &self,
        id: &str,
        change: IncomingWebhookChange,
    ) -> ProviderResult<IncomingWebhook> {
        let params = match change {
            IncomingWebhookChange::Rename(name) => api::IncomingWebhookUpdateRequest {
                name: Some(name),
                ..Default::default()
            },
            IncomingWebhookChange::Enabled(enabled) => api::IncomingWebhookUpdateRequest {
                enabled: Some(enabled),
                ..Default::default()
            },
            IncomingWebhookChange::RotateUrl => api::IncomingWebhookUpdateRequest {
                rotate_url: Some(true),
                ..Default::default()
            },
        };
        self.sdk()
            .incoming_webhooks()
            .update(id, params)
            .await
            .map(hook)
            .map_err(failed)
    }

    /// `DELETE /v1/incoming-webhooks/{id}`. Already gone is done.
    pub(crate) async fn delete_incoming_webhook(&self, id: &str) -> ProviderResult<()> {
        match self
            .sdk()
            .incoming_webhooks()
            .delete(id)
            .await
            .map_err(failed)
        {
            Err(ProviderError::Rejected { code, .. }) if code == "not_found" => Ok(()),
            other => other,
        }
    }
}
