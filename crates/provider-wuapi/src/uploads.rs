//! Sending files: the upload routes and the send that refers to an upload.
//!
//! The steps, each safe to repeat:
//!
//! 1. `POST /v1/uploads` `{mimeType, size, filename?}` with an
//!    `Idempotency-Key`: the same key answers the same upload and URL. A
//!    file of up to [`INLINE_LIMIT`] goes in this one request, as base64,
//!    and is ready at once.
//! 2. `POST {uploadUrl}` with the bytes. That URL is a storage host, not
//!    the API: **the API key is never sent there**. It is the one request
//!    of this file that does not go through the SDK, which only talks to
//!    the API.
//! 3. `POST /v1/uploads/{id}/complete` `{storageId}`.
//!
//! Then `POST /v1/messages` with `media: {uploadId}`.
//!
//! TEMPORARY(uploads-rollout): a deployment that does not have the routes
//! yet answers 404 to step 1. That is remembered for [`UPLOADS_RECHECK`]
//! (files are "not available yet" meanwhile) and asked about again later.
//! The SDK has no call that says which routes a deployment has, so
//! [`WuapiClient::uploads_ready`] still asks with a request that creates
//! nothing. Remove both once the routes are live in production.

use crate::client::WuapiClient;
use crate::compat;
use crate::error::{classify, from_sdk};
use crate::http::describe;
use crate::mapping::CLIENT_ID_KEY;
use base64::Engine as _;
use client_provider::{MediaKind, MediaUpload, ProviderError, ProviderResult, UploadProgress};
use futures::StreamExt;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use wuapi::types as api;

/// The largest file the API takes: 100 MiB.
pub(crate) const UPLOAD_LIMIT: u64 = 100 * 1024 * 1024;

/// Up to this size a file travels inside the create request (the API
/// takes 5 MiB that way; base64 and the JSON around it stay well under).
const INLINE_LIMIT: usize = 3 * 1024 * 1024;

/// How long "the upload routes are not there" is believed.
pub(crate) const UPLOADS_RECHECK: Duration = Duration::from_secs(10 * 60);

/// How much of a file goes out between two progress reports.
const CHUNK: usize = 64 * 1024;

/// Prefix of the media refs that stand for an upload.
pub(crate) const UPLOAD_PREFIX: &str = "wuapi-upload:";

/// What the storage host answers to the bytes.
#[derive(Deserialize)]
struct Stored {
    #[serde(rename = "storageId")]
    storage_id: String,
}

fn too_large() -> ProviderError {
    ProviderError::Rejected {
        code: "too_large".into(),
        message: "The file is larger than can be sent (100 MB).".into(),
    }
}

/// The SDK's error for an upload route, as the error to act on.
fn refused(error: wuapi::Error) -> ProviderError {
    match &error {
        wuapi::Error::Api { status: 413, .. } => too_large(),
        wuapi::Error::Api { code, .. } if code == "media_too_large" => too_large(),
        _ => from_sdk(error),
    }
}

/// A non-2xx answer of the storage host as the error to act on.
async fn storage_refusal(response: reqwest::Response) -> ProviderError {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_secs);
    let body = response.bytes().await.unwrap_or_default();
    let parsed: Option<api::ApiError> = serde_json::from_slice(&body).ok();
    let (code, message) = match parsed {
        Some(error) => (error.code, error.message),
        None => (format!("http_{status}"), format!("HTTP {status}")),
    };
    match (status, code.as_str()) {
        (413, _) | (_, "media_too_large") => too_large(),
        _ => classify(status, code, message, retry_after),
    }
}

fn transient(error: reqwest::Error) -> ProviderError {
    ProviderError::Transient(describe(error.without_url()))
}

fn unreadable(what: &str) -> ProviderError {
    ProviderError::Protocol(format!("unreadable answer to {what}"))
}

/// A 404 to a send that names an upload: the upload is gone (they are
/// kept for a day once ready), or the account is. Only the first can be
/// put right from here, by uploading again.
fn gone_upload(error: wuapi::Error) -> ProviderError {
    match &error {
        wuapi::Error::Api {
            status: 404,
            message,
            ..
        } if message.to_lowercase().contains("upload") => expired(),
        _ => refused(error),
    }
}

/// The metadata every message sent from here carries: the client's id,
/// which the API returns on every copy of the message.
fn client_metadata(client_id: &str) -> Option<BTreeMap<String, String>> {
    Some(BTreeMap::from([(
        CLIENT_ID_KEY.to_owned(),
        client_id.to_owned(),
    )]))
}

/// What a media send carries besides its file.
pub(crate) struct MediaSend<'a> {
    pub(crate) account: &'a str,
    pub(crate) to: &'a str,
    pub(crate) kind: MediaKind,
    pub(crate) upload_id: &'a str,
    pub(crate) mime_type: Option<&'a str>,
    pub(crate) file_name: Option<&'a str>,
    pub(crate) caption: Option<&'a str>,
    pub(crate) reply_to: Option<&'a str>,
    pub(crate) mentions: &'a [String],
    pub(crate) gif: bool,
    pub(crate) client_id: &'a str,
}

impl MediaSend<'_> {
    /// The request of the message's type. Every type has its own schema
    /// in the SDK, with the same fields around a different `media`.
    fn request(&self) -> api::SendMessageRequest {
        let mime = self.mime_type.map(str::to_owned);
        let name = self.file_name.map(str::to_owned);
        let plain = || {
            let mut media = api::SendMediaUpload::new(self.upload_id);
            media.mime_type = mime.clone();
            media.filename = name.clone();
            api::SendMedia::from(media)
        };
        let text = self
            .caption
            .filter(|caption| !caption.trim().is_empty())
            .map(str::to_owned);
        let reply = self.reply_to.map(str::to_owned);
        // `mentions`: the contact ids the caption mentions, at most 256.
        let mentions = (!self.mentions.is_empty())
            .then(|| self.mentions.iter().take(256).cloned().collect::<Vec<_>>());
        let metadata = client_metadata(self.client_id);
        macro_rules! fill {
            ($request:expr) => {{
                let mut request = $request;
                request.text = text;
                request.reply_to_message_id = reply;
                request.mentions = mentions;
                request.metadata = metadata;
                api::SendMessageRequest::from(request)
            }};
        }
        match self.kind {
            MediaKind::Image => {
                let mut media = api::SendImageMediaUpload::new(self.upload_id);
                // The API re-encodes an image the way the WhatsApp apps do
                // (`quality`, the account's `imageQuality` by default).
                // A `.gif` sent as the image it is must stay that file.
                if mime.as_deref().is_some_and(is_gif) {
                    media.quality = Some(api::ImageQualitySetting::Original);
                }
                media.mime_type = mime.clone();
                media.filename = name.clone();
                fill!(api::SendImageMessageRequest::new(
                    self.account,
                    self.to,
                    media.into()
                ))
            }
            MediaKind::Video => {
                let mut media = api::SendVideoMediaUpload::new(self.upload_id);
                media.mime_type = mime.clone();
                media.filename = name.clone();
                // A GIF on WhatsApp is a video flagged to play as one.
                // The API takes the flag on a video send and nothing else.
                media.gif_playback = self.gif.then_some(true);
                fill!(api::SendVideoMessageRequest::new(
                    self.account,
                    self.to,
                    media.into()
                ))
            }
            MediaKind::Audio => fill!(api::SendAudioMessageRequest::new(
                self.account,
                self.to,
                plain()
            )),
            MediaKind::Voice => fill!(api::SendVoiceMessageRequest::new(
                self.account,
                self.to,
                plain()
            )),
            MediaKind::Document => fill!(api::SendDocumentMessageRequest::new(
                self.account,
                self.to,
                plain()
            )),
            MediaKind::Sticker => fill!(api::SendStickerMessageRequest::new(
                self.account,
                self.to,
                plain()
            )),
        }
    }
}

fn is_gif(mime: &str) -> bool {
    mime.split(';')
        .next()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("image/gif"))
}

impl WuapiClient {
    /// TEMPORARY(uploads-rollout): whether the upload routes exist on this
    /// deployment, asked with a request that creates nothing (an empty
    /// body is `400 invalid_request` where the route exists, and 404
    /// where it does not). A "no" is remembered for [`UPLOADS_RECHECK`];
    /// a "yes" for good.
    pub(crate) async fn uploads_ready(&self) -> bool {
        match *self.uploads_state.lock().expect("uploads lock") {
            Some((true, _)) => return true,
            Some((false, at)) if at.elapsed() < UPLOADS_RECHECK => return false,
            _ => {}
        }
        let probe = self
            .sdk()
            .uploads()
            .create(api::UploadCreateRequest::Unknown(serde_json::json!({})))
            .await;
        let ready = match probe.as_ref().map_err(wuapi::Error::status) {
            Err(Some(404)) => {
                tracing::warn!(
                    "this wuapi deployment has no upload routes yet; \
                     files cannot be sent until it does"
                );
                false
            }
            // Refused for what it is (an empty body), or for anything
            // else that is not "no such route": the route is there.
            Ok(_) | Err(Some(400..=499)) => true,
            // Weather: no answer either way. Not remembered.
            Err(_) => return false,
        };
        *self.uploads_state.lock().expect("uploads lock") = Some((ready, Instant::now()));
        ready
    }

    /// Step 1. `key` is the idempotency key: the same upload however
    /// often this is sent.
    async fn create_upload(&self, upload: &MediaUpload, key: &str) -> ProviderResult<api::Upload> {
        let request: api::UploadCreateRequest = if upload.bytes.len() <= INLINE_LIMIT {
            let mut inline = api::InlineUploadCreateRequest::new(
                upload.mime_type.as_str(),
                base64::engine::general_purpose::STANDARD.encode(upload.bytes.as_slice()),
            );
            inline.filename = upload.file_name.clone();
            inline.into()
        } else {
            let mut file = api::FileUploadCreateRequest::new(
                upload.mime_type.as_str(),
                i64::try_from(upload.bytes.len()).unwrap_or(i64::MAX),
            );
            file.filename = upload.file_name.clone();
            file.into()
        };
        let answer = self
            .sdk()
            .uploads()
            .create(request)
            .idempotency_key(key)
            .timeout(self.media_timeout())
            .await;
        match answer {
            Ok(created) => {
                *self.uploads_state.lock().expect("uploads lock") = Some((true, Instant::now()));
                Ok(created)
            }
            Err(wuapi::Error::Api { status: 404, .. }) => {
                // TEMPORARY(uploads-rollout): no such route here, yet. Not
                // a verdict on the file: it waits.
                *self.uploads_state.lock().expect("uploads lock") = Some((false, Instant::now()));
                Err(ProviderError::Transient(
                    "sending files is not available yet".into(),
                ))
            }
            Err(error) => Err(refused(error)),
        }
    }

    /// Step 2: the bytes, to the storage host. No API key: not in a
    /// header, not anywhere.
    async fn store_bytes(
        &self,
        url: &str,
        upload: &MediaUpload,
        progress: &UploadProgress,
    ) -> ProviderResult<String> {
        if !(url.starts_with("https://") || url.starts_with("http://")) {
            return Err(unreadable("creating an upload (its URL)"));
        }
        let (bytes, report) = (upload.bytes.clone(), progress.clone());
        let total = bytes.len();
        // In chunks, so that there is progress to report.
        let chunks = futures::stream::iter((0..total).step_by(CHUNK)).map(move |start| {
            let end = (start + CHUNK).min(total);
            report(end as u64);
            Ok::<_, std::io::Error>(bytes[start..end].to_vec())
        });
        let response = self
            .http()
            .post(url)
            .header("Content-Type", &upload.mime_type)
            .header("Content-Length", total)
            .timeout(self.upload_timeout())
            .body(reqwest::Body::wrap_stream(chunks))
            .send()
            .await
            .map_err(transient)?;
        match response.status().as_u16() {
            200..=299 => {}
            // The URL is good for an hour: after that the upload is gone.
            401 | 403 | 404 | 410 => return Err(expired()),
            _ => return Err(storage_refusal(response).await),
        }
        let stored: Stored = response
            .json()
            .await
            .map_err(|_| unreadable("storing a file"))?;
        Ok(stored.storage_id)
    }

    /// Step 3.
    async fn complete_upload(&self, id: &str, storage_id: &str) -> ProviderResult<api::Upload> {
        self.sdk()
            .uploads()
            .complete(id, api::UploadCompleteRequest::new(storage_id))
            .timeout(self.media_timeout())
            .await
            .map_err(|error| match error {
                wuapi::Error::Api { status: 404, .. } => expired(),
                other => refused(other),
            })
    }

    /// The three steps, under one key. Answers the upload's id.
    async fn upload_once(
        &self,
        upload: &MediaUpload,
        key: &str,
        progress: &UploadProgress,
    ) -> ProviderResult<String> {
        let created = self.create_upload(upload, key).await?;
        if created.status == api::UploadStatus::Ready {
            progress(upload.bytes.len() as u64);
            return Ok(created.id);
        }
        let url = created.upload_url.ok_or_else(expired)?;
        let storage_id = self.store_bytes(&url, upload, progress).await?;
        let ready = self.complete_upload(&created.id, &storage_id).await?;
        Ok(ready.id)
    }

    /// Uploads a file. Repeating it with the same [`MediaUpload::key`] is
    /// the same upload. An upload that expired midway (its URL is good
    /// for an hour) is started again under another key: the bytes are
    /// right here.
    pub(crate) async fn upload(
        &self,
        upload: &MediaUpload,
        progress: &UploadProgress,
    ) -> ProviderResult<String> {
        if upload.bytes.len() as u64 > UPLOAD_LIMIT {
            return Err(too_large());
        }
        match self.upload_once(upload, &upload.key, progress).await {
            Err(ProviderError::Rejected { code, .. }) if code == "upload_expired" => {
                let again = format!("{}~again", upload.key);
                self.upload_once(upload, &again, progress).await
            }
            other => other,
        }
    }

    /// `POST /v1/messages` with `media: {uploadId}`. The client id travels
    /// as the `Idempotency-Key` and in `metadata`, as for a text.
    pub(crate) async fn send_media(&self, send: MediaSend<'_>) -> ProviderResult<api::Message> {
        compat::send(self.sdk().http(), send.request())
            .idempotency_key(send.client_id)
            .await
            .map(|message| message.0)
            .map_err(gone_upload)
    }

    /// `POST /v1/accounts/{accountId}/stories` with a picture or a video
    /// that was uploaded first: `media: {uploadId}`. The client id is the
    /// `Idempotency-Key`, so a repeat answers the same story.
    pub(crate) async fn post_media_story(
        &self,
        account: &str,
        kind: MediaKind,
        upload_id: &str,
        mime_type: Option<&str>,
        caption: Option<&str>,
        client_id: &str,
    ) -> ProviderResult<api::Message> {
        let kind = match kind {
            MediaKind::Video => api::MediaStoryCreateRequestType::Video,
            _ => api::MediaStoryCreateRequestType::Image,
        };
        let mut media = api::StoryMediaUpload::new(upload_id);
        media.mime_type = mime_type.map(str::to_owned);
        let mut request = api::MediaStoryCreateRequest::new(kind, media.into());
        request.text = caption
            .filter(|caption| !caption.trim().is_empty())
            .map(str::to_owned);
        compat::post_story(self.sdk().http(), account, request)
            .idempotency_key(client_id)
            .await
            .map(|message| message.0)
            .map_err(gone_upload)
    }
}

/// The upload is no longer there: the client uploads again.
fn expired() -> ProviderError {
    ProviderError::Rejected {
        code: "upload_expired".into(),
        message: "The uploaded file is no longer there.".into(),
    }
}
