//! Provider errors, split by what the client should do about them.

use std::time::Duration;

/// Why a provider call failed.
///
/// The variant decides the client's reaction, so classify carefully:
///
/// * [`Transient`](Self::Transient) and [`RateLimited`](Self::RateLimited)
///   are retried quietly. The user never sees them; a queued message keeps
///   its clock icon until it goes through.
/// * Everything else is terminal for that call and is shown to the user.
///
/// When in doubt between transient and terminal, choose transient only if
/// repeating the *same* request later can succeed.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ProviderError {
    /// The network or the backend hiccuped: timeouts, dropped connections,
    /// 5xx answers, an account that is reconnecting. Retrying later can work.
    #[error("temporary failure: {0}")]
    Transient(String),

    /// The backend asked to slow down. Retried after `retry_after` when
    /// given, with the client's own backoff otherwise.
    #[error("rate limited")]
    RateLimited {
        /// How long the backend asked to wait.
        retry_after: Option<Duration>,
    },

    /// The credentials are missing, expired or revoked. The user has to sign
    /// in again.
    #[error("not authorized: {0}")]
    Unauthorized(String),

    /// The provider does not implement this operation. Should never happen
    /// for a capability reported as `true`.
    #[error("not supported by this provider: {0}")]
    Unsupported(&'static str),

    /// The request is wrong and will stay wrong: unknown chat, empty text,
    /// recipient without WhatsApp, account logged out.
    #[error("{message}")]
    Rejected {
        /// Machine-readable reason, provider-specific (`not_on_whatsapp`).
        code: String,
        /// Human-readable reason, shown to the user.
        message: String,
    },

    /// The backend answered something the adapter could not understand.
    /// Treated as terminal: repeating the request would get the same answer.
    #[error("unexpected response: {0}")]
    Protocol(String),
}

impl ProviderError {
    /// True when the same request may succeed later, so the client should
    /// keep retrying instead of telling the user.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_) | Self::RateLimited { .. })
    }

    /// The delay the backend asked for, if it asked for one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => *retry_after,
            _ => None,
        }
    }
}

/// Shorthand for results of provider calls.
pub type ProviderResult<T> = Result<T, ProviderError>;
