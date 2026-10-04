//! Plain HTTP for the two things the wuapi SDK does not cover: the device
//! login (`/cli/device/*` is not part of the OpenAPI spec) and downloading
//! media bytes from a URL. Everything under `/v1` goes through the SDK; see
//! `client.rs`.
//!
//! One small trait and one reqwest implementation. The login tests swap the
//! transport for a fake that records requests and replays fixtures.

use crate::config::{ApiKey, WuapiConfig};
use async_trait::async_trait;
use client_provider::{MediaData, MediaLimit, ProviderError, ProviderResult};

/// The HTTP methods the adapter uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Post,
}

/// A request, fully described and independent of any HTTP library.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ApiRequest {
    pub(crate) method: Method,
    /// Absolute URL, query string included.
    pub(crate) url: String,
    pub(crate) body: Option<serde_json::Value>,
    /// Sent as `Authorization: Bearer ...`. An [`ApiKey`] prints only its
    /// prefix, so the key never shows up when a request is printed.
    pub(crate) bearer: Option<ApiKey>,
}

impl ApiRequest {
    pub(crate) fn get(url: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            url: url.into(),
            body: None,
            bearer: None,
        }
    }

    pub(crate) fn post(url: impl Into<String>, body: Option<serde_json::Value>) -> Self {
        Self {
            method: Method::Post,
            url: url.into(),
            body,
            bearer: None,
        }
    }

    pub(crate) fn bearer(mut self, key: &ApiKey) -> Self {
        self.bearer = Some(key.clone());
        self
    }
}

/// What came back.
#[derive(Clone, Debug, Default)]
pub(crate) struct ApiResponse {
    pub(crate) status: u16,
    /// `Retry-After`, in seconds, when the server sent one.
    pub(crate) retry_after: Option<u64>,
    pub(crate) content_type: Option<String>,
    pub(crate) body: Vec<u8>,
}

/// The request never produced an HTTP response: DNS, connect, TLS, a
/// dropped socket, or the timeout. Always worth retrying.
#[derive(Debug)]
pub(crate) struct NetworkError(pub(crate) String);

impl From<NetworkError> for ProviderError {
    fn from(error: NetworkError) -> Self {
        ProviderError::Transient(error.0)
    }
}

/// Sends requests. Implementations must bound every call with a timeout.
#[async_trait]
pub(crate) trait Transport: Send + Sync {
    async fn execute(&self, request: ApiRequest) -> Result<ApiResponse, NetworkError>;
}

/// The real transport.
pub(crate) struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub(crate) fn new(config: &WuapiConfig) -> ProviderResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(config.user_agent.clone())
            .timeout(config.request_timeout)
            .connect_timeout(config.connect_timeout)
            .build()
            .map_err(|e| ProviderError::Protocol(format!("could not set up HTTP: {e}")))?;
        Ok(Self { client })
    }

    /// The underlying client, so the SDK shares its connection pool and
    /// connect timeout.
    pub(crate) fn client(&self) -> reqwest::Client {
        self.client.clone()
    }
}

/// How long one media download may take, body included.
const MEDIA_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(110);

impl ReqwestTransport {
    /// Downloads `url` within `limit`: refused from the headers when they
    /// already say it is too large or not an image, and cut off at the
    /// limit when they do not say. Redirects are followed by reqwest;
    /// only http(s) URLs get here.
    pub(crate) async fn fetch_limited(
        &self,
        url: &str,
        bearer: Option<&ApiKey>,
        limit: MediaLimit,
        progress: Option<&client_provider::UploadProgress>,
    ) -> ProviderResult<MediaData> {
        let too_large = || ProviderError::Rejected {
            code: "too_large".into(),
            message: "The file is larger than the download limit.".into(),
        };
        // A file takes longer than an API call: its own, longer deadline
        // for the whole download (the connection deadline is unchanged).
        let mut builder = self.client.get(url).timeout(MEDIA_TIMEOUT);
        if let Some(key) = bearer {
            builder = builder.bearer_auth(key.expose());
        }
        let mut response = builder
            .send()
            .await
            .map_err(|e| ProviderError::Transient(describe(e.without_url())))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse().ok());
            return Err(crate::error::from_response(ApiResponse {
                status,
                retry_after,
                content_type: None,
                body: Vec::new(),
            }));
        }
        let mime_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_lowercase());
        if limit.images_only
            && !mime_type
                .as_deref()
                .is_some_and(|m| m.starts_with("image/"))
        {
            return Err(ProviderError::Rejected {
                code: "not_an_image".into(),
                message: "The file is not an image.".into(),
            });
        }
        if response
            .content_length()
            .is_some_and(|length| length > limit.max_bytes)
        {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| ProviderError::Transient(describe(e.without_url())))?
        {
            if (bytes.len() + chunk.len()) as u64 > limit.max_bytes {
                // The headers did not say, or lied. Stop here.
                return Err(too_large());
            }
            bytes.extend_from_slice(&chunk);
            if let Some(progress) = progress {
                progress(bytes.len() as u64);
            }
        }
        Ok(MediaData { bytes, mime_type })
    }
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn execute(&self, request: ApiRequest) -> Result<ApiResponse, NetworkError> {
        let method = match request.method {
            Method::Get => reqwest::Method::GET,
            Method::Post => reqwest::Method::POST,
        };
        let mut builder = self
            .client
            .request(method, &request.url)
            .header("Accept", "application/json");
        if let Some(key) = &request.bearer {
            builder = builder.bearer_auth(key.expose());
        }
        if let Some(body) = &request.body {
            builder = builder.json(body);
        }
        // `without_url` keeps query strings out of error messages and logs.
        let response = builder
            .send()
            .await
            .map_err(|e| NetworkError(describe(e.without_url())))?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let retry_after = header("retry-after").and_then(|v| v.trim().parse().ok());
        let content_type = header("content-type");
        let body = response
            .bytes()
            .await
            .map_err(|e| NetworkError(describe(e.without_url())))?
            .to_vec();
        Ok(ApiResponse {
            status,
            retry_after,
            content_type,
            body,
        })
    }
}

/// A network failure in words, without the URL.
pub(crate) fn describe(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "the request timed out".to_owned()
    } else if error.is_connect() {
        "could not connect".to_owned()
    } else {
        error.to_string()
    }
}

impl ApiResponse {
    pub(crate) fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// `scheme://host[:port]` of a URL, lowercased; `None` if it has no
/// authority or carries user info (never trusted with credentials).
pub(crate) fn origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    Some(format!(
        "{}://{}",
        scheme.to_lowercase(),
        authority.to_lowercase()
    ))
}
