//! Incoming webhooks in the mock: kept in memory, with URLs that go
//! nowhere, so the client can make, change and delete them.

use crate::{MockProvider, State};
use client_provider::{
    AccountId, ChatId, IncomingWebhook, IncomingWebhookChange, ProviderError, ProviderResult,
};

/// What the mock keeps for incoming webhooks.
#[derive(Default)]
pub(crate) struct Hooks {
    /// Every hook, in the order it was made.
    hooks: Vec<IncomingWebhook>,
    /// How many URLs were handed out, for the next one.
    issued: u64,
}

fn url(state: &mut State) -> String {
    state.hooks.issued += 1;
    format!("https://hooks.mock/{}", state.hooks.issued)
}

impl MockProvider {
    /// Every hook the mock holds, whatever chat it sends to.
    pub fn incoming_webhooks_held(&self) -> Vec<IncomingWebhook> {
        self.state().hooks.hooks.clone()
    }

    pub(crate) fn hooks_of(&self, account: &AccountId, chat: &ChatId) -> Vec<IncomingWebhook> {
        self.state()
            .hooks
            .hooks
            .iter()
            .filter(|hook| &hook.account_id == account && &hook.to == chat)
            .cloned()
            .collect()
    }

    pub(crate) fn hook_made(
        &self,
        account: &AccountId,
        chat: &ChatId,
        name: &str,
    ) -> ProviderResult<IncomingWebhook> {
        let mut state = self.state();
        let url = url(&mut state);
        let hook = IncomingWebhook {
            id: format!("ih_{}", state.hooks.issued),
            account_id: account.clone(),
            to: chat.clone(),
            name: name.to_owned(),
            enabled: true,
            url,
        };
        state.hooks.hooks.push(hook.clone());
        Ok(hook)
    }

    pub(crate) fn hook_changed(
        &self,
        id: &str,
        change: IncomingWebhookChange,
    ) -> ProviderResult<IncomingWebhook> {
        let mut state = self.state();
        let fresh = matches!(change, IncomingWebhookChange::RotateUrl).then(|| url(&mut state));
        let Some(hook) = state.hooks.hooks.iter_mut().find(|hook| hook.id == id) else {
            return Err(ProviderError::Rejected {
                code: "not_found".into(),
                message: "Incoming webhook not found.".into(),
            });
        };
        match change {
            IncomingWebhookChange::Rename(name) => hook.name = name,
            IncomingWebhookChange::Enabled(enabled) => hook.enabled = enabled,
            IncomingWebhookChange::RotateUrl => hook.url = fresh.unwrap_or_default(),
        }
        Ok(hook.clone())
    }

    pub(crate) fn hook_deleted(&self, id: &str) {
        self.state().hooks.hooks.retain(|hook| hook.id != id);
    }
}
