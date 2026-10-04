//! Who an API key belongs to, reduced to what an application shows.
//!
//! These are the adapter's own small types, so its public API does not
//! change shape whenever the SDK's generated ones do.

use serde::Deserialize;
use wuapi::types as api;

/// An organization or a project, reduced to what identifies it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Named {
    /// Stable id.
    pub id: String,
    /// Display name.
    pub name: String,
}

/// An API key, without the key itself (which `/v1/me` never returns).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInfo {
    /// Id of the key.
    pub id: String,
    /// The label given to the key.
    pub name: String,
    /// Public prefix (`wu_live_9c9c`), safe to show.
    pub key_prefix: String,
}

/// Who the current API key is, from `GET /v1/me`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthContext {
    /// The organization the key belongs to.
    pub organization: Named,
    /// The key in use.
    pub api_key: KeyInfo,
    /// The project the key is scoped to; `None` for an organization key.
    pub project: Option<Named>,
}

impl From<api::AuthContext> for AuthContext {
    fn from(context: api::AuthContext) -> Self {
        Self {
            organization: Named {
                id: context.organization.id,
                name: context.organization.name,
            },
            api_key: KeyInfo {
                id: context.api_key.id,
                name: context.api_key.name,
                key_prefix: context.api_key.key_prefix,
            },
            project: context.project.map(|project| Named {
                id: project.id,
                name: project.name,
            }),
        }
    }
}
