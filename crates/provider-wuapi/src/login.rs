//! Signing in without typing an API key: the device authorization flow
//! (RFC 8628) that `wuapi login` uses.
//!
//! 1. [`DeviceLogin::request_code`] asks the API for a code.
//! 2. The application opens [`DeviceCode::verification_uri_complete`] in
//!    the browser, where the person, signed in to wuapi, approves it.
//! 3. [`DeviceLogin::wait_for_token`] polls until the API hands over a new
//!    API key, which the application then stores in the keychain.
//!
//! These two endpoints are not part of wuapi's OpenAPI spec, so the SDK does
//! not cover them and the requests are made by hand, over the plain
//! transport of `http.rs`.
//!
//! Networks drop: every request is bounded, and a failed poll is simply
//! repeated until the code expires.

use crate::config::{ApiKey, WuapiConfig};
use crate::http::{ApiRequest, ReqwestTransport, Transport};
use crate::identity::Named;
use serde::Deserialize;
use serde_json::json;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

/// Why a login did not complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    /// Nobody approved the code before it expired.
    #[error("the login code expired before it was approved")]
    Expired,
    /// The person pressed "deny" in the browser.
    #[error("the login was denied in the browser")]
    Denied,
    /// The code is unknown or was already used.
    #[error("the login code is not valid any more")]
    InvalidGrant,
    /// The API could not be reached to start the login.
    #[error("could not reach the API: {0}")]
    Network(String),
    /// The API answered something unexpected.
    #[error("the login failed: {0}")]
    Unexpected(String),
}

/// A pending login: what to show the person, and what to poll with.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCode {
    /// The secret this client polls with. Never shown.
    pub device_code: String,
    /// The short code the person confirms in the browser.
    pub user_code: String,
    /// Where to approve, to type the code by hand.
    pub verification_uri: String,
    /// Where to approve, with the code filled in. Open this one.
    #[serde(default)]
    pub verification_uri_complete: String,
    /// Seconds until the code expires.
    #[serde(default)]
    pub expires_in: u64,
    /// Seconds to wait between polls.
    #[serde(default)]
    pub interval: u64,
}

impl fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCode")
            .field("user_code", &self.user_code)
            .field("verification_uri_complete", &self.verification_uri_complete)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// A completed login. The API key is shown exactly once: store it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenGrant {
    /// The new API key.
    pub api_key: ApiKey,
    /// Its public prefix, safe to display.
    pub key_prefix: Option<String>,
    /// The organization the key belongs to.
    pub organization: Named,
    /// The project the key is scoped to, if any.
    pub project: Option<Named>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireGrant {
    api_key: String,
    #[serde(default)]
    key_prefix: Option<String>,
    organization: Named,
    #[serde(default)]
    project: Option<Named>,
}

/// The result of asking once whether the login was approved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PollOutcome {
    /// Approved: here is the key.
    Granted(TokenGrant),
    /// Not approved yet. Ask again after the interval.
    Pending,
    /// Asking too often. Add five seconds to the interval.
    SlowDown,
    /// The API could not be reached or had a hiccup. Ask again after the
    /// interval; this is not a failure.
    Unreachable(String),
}

/// The device login flow against one API.
pub struct DeviceLogin {
    transport: Arc<dyn Transport>,
    base: String,
}

/// The error code of a login answer: `{ error: "..." }`, `{ error: { code } }`
/// or `{ code }`.
fn error_code(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let error = value.get("error");
    error
        .and_then(|e| e.as_str())
        .or_else(|| error.and_then(|e| e.get("code")).and_then(|c| c.as_str()))
        .or_else(|| value.get("code").and_then(|c| c.as_str()))
        .map(str::to_owned)
}

impl DeviceLogin {
    /// Prepares a login against the API in `config`.
    pub fn new(config: &WuapiConfig) -> Result<Self, LoginError> {
        let transport =
            ReqwestTransport::new(config).map_err(|e| LoginError::Unexpected(e.to_string()))?;
        Ok(Self::with_transport(config, Arc::new(transport)))
    }

    pub(crate) fn with_transport(config: &WuapiConfig, transport: Arc<dyn Transport>) -> Self {
        Self {
            transport,
            base: config.base().to_owned(),
        }
    }

    /// Starts a login. `client_name` is shown on the approval page so the
    /// person knows what is asking (the machine's host name is customary).
    ///
    /// Retried a few times on network errors and 5xx answers.
    pub async fn request_code(&self, client_name: &str) -> Result<DeviceCode, LoginError> {
        const ATTEMPTS: u32 = 4;
        let url = format!("{}/cli/device/code", self.base);
        let mut attempt = 1;
        loop {
            let request = ApiRequest::post(&url, Some(json!({ "clientName": client_name })));
            let failure = match self.transport.execute(request).await {
                Ok(response) if response.is_success() => {
                    let mut code: DeviceCode = serde_json::from_slice(&response.body)
                        .map_err(|e| LoginError::Unexpected(format!("unreadable answer: {e}")))?;
                    if code.verification_uri_complete.is_empty() {
                        code.verification_uri_complete = code.verification_uri.clone();
                    }
                    if code.expires_in == 0 {
                        code.expires_in = 600;
                    }
                    if code.interval == 0 {
                        code.interval = 5;
                    }
                    return Ok(code);
                }
                Ok(response) if response.status < 500 => {
                    let code = error_code(&response.body)
                        .unwrap_or_else(|| format!("HTTP {}", response.status));
                    return Err(LoginError::Unexpected(code));
                }
                Ok(response) => LoginError::Network(format!("HTTP {}", response.status)),
                Err(error) => LoginError::Network(error.0),
            };
            if attempt >= ATTEMPTS {
                return Err(failure);
            }
            tokio::time::sleep(Duration::from_secs(u64::from(attempt))).await;
            attempt += 1;
        }
    }

    /// Asks once whether the login was approved. Never sleeps; the caller
    /// decides when to ask again (see [`wait_for_token`](Self::wait_for_token)).
    pub async fn poll_once(&self, code: &DeviceCode) -> Result<PollOutcome, LoginError> {
        let url = format!("{}/cli/device/token", self.base);
        let request = ApiRequest::post(url, Some(json!({ "deviceCode": code.device_code })));
        let response = match self.transport.execute(request).await {
            Ok(response) => response,
            Err(error) => return Ok(PollOutcome::Unreachable(error.0)),
        };
        if response.status == 200 {
            let grant: WireGrant = serde_json::from_slice(&response.body)
                .map_err(|e| LoginError::Unexpected(format!("unreadable answer: {e}")))?;
            return Ok(PollOutcome::Granted(TokenGrant {
                api_key: ApiKey::new(grant.api_key),
                key_prefix: grant.key_prefix,
                organization: grant.organization,
                project: grant.project,
            }));
        }
        let code = error_code(&response.body);
        if matches!(response.status, 400 | 401 | 403) {
            match code.as_deref() {
                Some("authorization_pending") => return Ok(PollOutcome::Pending),
                Some("slow_down") => return Ok(PollOutcome::SlowDown),
                Some("expired_token") => return Err(LoginError::Expired),
                Some("access_denied") => return Err(LoginError::Denied),
                Some("invalid_grant") => return Err(LoginError::InvalidGrant),
                _ => {}
            }
        }
        match response.status {
            429 => Ok(PollOutcome::SlowDown),
            500..=599 => Ok(PollOutcome::Unreachable(format!(
                "HTTP {}",
                response.status
            ))),
            status => Err(LoginError::Unexpected(match code {
                Some(code) => format!("HTTP {status}, {code}"),
                None => format!("HTTP {status}"),
            })),
        }
    }

    /// Polls every `code.interval` seconds until the login is approved,
    /// denied, or the code expires.
    pub async fn wait_for_token(&self, code: &DeviceCode) -> Result<TokenGrant, LoginError> {
        let deadline = Instant::now() + Duration::from_secs(code.expires_in);
        let mut interval = Duration::from_secs(code.interval.max(1));
        loop {
            if Instant::now() + interval >= deadline {
                return Err(LoginError::Expired);
            }
            tokio::time::sleep(interval).await;
            match self.poll_once(code).await? {
                PollOutcome::Granted(grant) => return Ok(grant),
                PollOutcome::SlowDown => interval += Duration::from_secs(5),
                PollOutcome::Pending => {}
                PollOutcome::Unreachable(reason) => {
                    tracing::debug!(%reason, "login poll did not get through; still waiting");
                }
            }
        }
    }
}
