//! Failures translated into [`ProviderError`]: the SDK's error enum, and
//! the raw HTTP answers of the requests the SDK does not make.
//!
//! The split that matters is transient against terminal. The client keeps
//! retrying a transient failure without telling anyone; a terminal one fails
//! the message in front of the user. Connections to WhatsApp drop all the
//! time, so anything that waiting can fix must land on the transient side.

use crate::http::{describe, ApiResponse};
use client_provider::ProviderError;
use std::time::Duration;

/// `409` codes that mean "not now", not "no":
///
/// * `account_not_ready`: the number is reconnecting; wuapi takes the
///   request again once it is back.
/// * `idempotency_conflict`: the first attempt with this key is still
///   running.
/// * `state_resyncing`: chat state (read marks, pins) is resyncing with the
///   phone, for example right after linking.
/// * `message_sending`: the message is being handed to WhatsApp this second.
const WAIT_CODES: [&str; 4] = [
    "account_not_ready",
    "idempotency_conflict",
    "state_resyncing",
    "message_sending",
];

/// Turns a non-2xx answer of the API into the error the client should act
/// on.
///
/// * 5xx and 408: the backend or something on the way hiccuped, retry.
/// * 429: slow down for `Retry-After`.
/// * 401: the key is gone, the user has to sign in again.
/// * 409 with one of the waiting codes: fixed by waiting.
/// * any other 4xx: the request itself is wrong and stays wrong.
pub(crate) fn classify(
    status: u16,
    code: String,
    message: String,
    retry_after: Option<Duration>,
) -> ProviderError {
    match status {
        408 | 500..=599 => ProviderError::Transient(format!("{code}: {message}")),
        429 => ProviderError::RateLimited { retry_after },
        401 => ProviderError::Unauthorized(message),
        409 if WAIT_CODES.contains(&code.as_str()) => {
            ProviderError::Transient(format!("{code}: {message}"))
        }
        _ => ProviderError::Rejected { code, message },
    }
}

/// The SDK's error as a [`ProviderError`].
pub(crate) fn from_sdk(error: wuapi::Error) -> ProviderError {
    match error {
        wuapi::Error::Api {
            status,
            code,
            message,
            retry_after,
            ..
        } => classify(status, code, message, retry_after),
        wuapi::Error::Timeout { .. } => ProviderError::Transient("the request timed out".into()),
        // `without_url` keeps query strings (phone numbers) out of messages
        // and logs.
        wuapi::Error::Network(error) => ProviderError::Transient(describe(error.without_url())),
        wuapi::Error::Decode { source, .. } => {
            ProviderError::Protocol(format!("unreadable response body: {source}"))
        }
        // Nothing was sent, and sending the same thing again changes nothing.
        wuapi::Error::Encode(reason) => {
            ProviderError::Protocol(format!("the request could not be built: {reason}"))
        }
        wuapi::Error::Config(reason) => ProviderError::Protocol(reason),
        // The enum is non-exhaustive. A failure this adapter does not know
        // must not fail a message that is safe to send again.
        other => ProviderError::Transient(other.to_string()),
    }
}

/// The error for a non-2xx answer to a request made without the SDK.
pub(crate) fn from_response(response: ApiResponse) -> ProviderError {
    let parsed: Option<wuapi::types::ApiError> = serde_json::from_slice(&response.body).ok();
    let (code, message) = match parsed {
        Some(e) => (e.code, e.message),
        None => (
            format!("http_{}", response.status),
            format!("HTTP {}", response.status),
        ),
    };
    classify(
        response.status,
        code,
        message,
        response.retry_after.map(Duration::from_secs),
    )
}
