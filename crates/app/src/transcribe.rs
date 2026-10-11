//! Turning a voice note into text: where the "Transcribe" action gets its words from.
//!
//! A [`Transcriber`] takes an audio file and answers with its text. Two
//! exist:
//!
//! * **Online** ([`OpenAiCompatible`]): any server that speaks OpenAI's
//!   `POST {base}/audio/transcriptions` (OpenAI, Groq, a local whisper.cpp or
//!   faster-whisper server, ...). **Off by default.** The audio is sent to
//!   the server the user named, with the key they gave, and to no one else.
//!   Nothing is requested while transcription is off: the only way to get one
//!   is [`OpenAiCompatible::new`] with a base URL and a key, which the shell
//!   calls only when the mode is [`Mode::Online`].
//! * **System**: the operating system's own recogniser, where it has one
//!   that works from a file (see [`system`]).
//!
//! The server's address, model and language are not secret and live in
//! `transcription.json` next to the settings; the key lives in the OS
//! keychain, like the GIF search key, never in a file.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// The file's name next to the settings file.
pub const FILE_NAME: &str = "transcription.json";
/// OpenAI's own API, the starting point of the form.
pub const OPENAI_BASE: &str = "https://api.openai.com/v1";
/// The model a new configuration starts with.
pub const DEFAULT_MODEL: &str = "whisper-1";
/// A voice note longer than this is not sent (the servers' own limit is
/// 25 MB; a voice note of an hour is about 10).
const MAX_BYTES: usize = 24 * 1024 * 1024;
/// How long a request may take: long notes take a while to transcribe.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Where transcripts come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// No transcribing: the action is not offered, nothing is sent.
    #[default]
    Off,
    /// The operating system's recogniser, on this computer.
    System,
    /// An OpenAI-compatible server the user configured.
    Online,
}

/// The server's coordinates. Everything but the key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Up to and including the version, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// The model's name at that server, e.g. `whisper-1`.
    pub model: String,
    /// ISO 639-1 code of the spoken language (a hint that makes the result
    /// better). Empty: the server detects it.
    pub language: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: OPENAI_BASE.into(),
            model: DEFAULT_MODEL.into(),
            language: String::new(),
        }
    }
}

impl Config {
    /// Reads the file; a missing or unreadable one is the defaults.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Writes the file whole, through a temporary one.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let draft = path.with_extension("json.tmp");
        std::fs::write(&draft, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&draft, path)
    }

    /// The language to send: `None` when none was given.
    pub fn language_hint(&self) -> Option<&str> {
        Some(self.language.trim()).filter(|code| !code.is_empty())
    }
}

/// Why a transcription did not work, in words for the user.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TranscribeError {
    /// The server refused the key.
    #[error("The service did not accept the API key. Check it in Settings.")]
    Key,
    /// Too many requests.
    #[error("The service says too many requests were made. Try again in a moment.")]
    Limit,
    /// No connection, or the server did not answer.
    #[error("The service could not be reached.")]
    Offline,
    /// An answer this application cannot read.
    #[error("The service answered something unexpected.")]
    Unexpected,
    /// The address or the model is not usable.
    #[error("{0}")]
    Setup(String),
    /// The audio is too large, or in a format the service does not take.
    #[error("{0}")]
    File(String),
    /// Nothing can transcribe on this computer.
    #[error("{0}")]
    Unsupported(String),
}

/// An audio file to transcribe, as it was received.
#[derive(Clone, Debug)]
pub struct Audio {
    /// The file.
    pub bytes: Vec<u8>,
    /// Its media type, when known (`audio/ogg; codecs=opus`).
    pub mime: Option<String>,
}

impl Audio {
    /// The name the file is sent under: servers tell the format from the
    /// extension.
    fn file_name(&self) -> &'static str {
        let mime = self.mime.as_deref().unwrap_or_default().to_lowercase();
        match () {
            _ if mime.contains("ogg") || mime.contains("opus") => "voice.ogg",
            _ if mime.contains("mpeg") || mime.contains("mp3") => "voice.mp3",
            _ if mime.contains("mp4") || mime.contains("m4a") || mime.contains("aac") => {
                "voice.m4a"
            }
            _ if mime.contains("wav") => "voice.wav",
            _ if mime.contains("webm") => "voice.webm",
            // WhatsApp's voice notes are Ogg/Opus.
            _ => "voice.ogg",
        }
    }

    fn content_type(&self) -> String {
        self.mime
            .as_deref()
            .map(|mime| mime.split(';').next().unwrap_or(mime).trim().to_owned())
            .filter(|mime| mime.starts_with("audio/"))
            .unwrap_or_else(|| "audio/ogg".into())
    }
}

/// Somewhere that turns audio into text.
#[async_trait]
pub trait Transcriber: Send + Sync {
    /// The words of `audio`. `language` is an ISO 639-1 hint.
    async fn transcribe(
        &self,
        audio: Audio,
        language: Option<&str>,
    ) -> Result<String, TranscribeError>;
}

/// Whether `base` may be used: HTTPS, or plain HTTP to this computer (a
/// server the user runs themselves). A key never travels in the clear over
/// a network.
pub fn check_base(base: &str) -> Result<String, TranscribeError> {
    let base = base.trim().trim_end_matches('/');
    let url = reqwest::Url::parse(base)
        .map_err(|_| TranscribeError::Setup("The server address is not a valid URL.".into()))?;
    let host = url.host_str().unwrap_or_default().trim_matches(['[', ']']);
    let local = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    match url.scheme() {
        "https" => Ok(base.to_owned()),
        "http" if local => Ok(base.to_owned()),
        "http" => Err(TranscribeError::Setup(
            "Use https:// for the server address (plain http:// is only for this computer).".into(),
        )),
        _ => Err(TranscribeError::Setup(
            "The server address must start with https://.".into(),
        )),
    }
}

/// An OpenAI-compatible transcription endpoint, with the user's own key.
#[derive(Clone)]
pub struct OpenAiCompatible {
    http: reqwest::Client,
    base: String,
    key: String,
    model: String,
}

impl std::fmt::Debug for OpenAiCompatible {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key.
        f.debug_struct("OpenAiCompatible")
            .field("base", &self.base)
            .field("model", &self.model)
            .finish()
    }
}

#[derive(Deserialize)]
struct Answer {
    text: String,
}

impl OpenAiCompatible {
    /// The endpoint at `base`, signed with `key`, asking for `model`.
    pub fn new(base: &str, key: &str, model: &str) -> Result<Self, TranscribeError> {
        let base = check_base(base)?;
        let model = model.trim();
        if model.is_empty() {
            return Err(TranscribeError::Setup("Enter the model's name.".into()));
        }
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|_| TranscribeError::Offline)?;
        Ok(Self {
            http,
            base,
            key: key.trim().to_owned(),
            model: model.to_owned(),
        })
    }
}

#[async_trait]
impl Transcriber for OpenAiCompatible {
    async fn transcribe(
        &self,
        audio: Audio,
        language: Option<&str>,
    ) -> Result<String, TranscribeError> {
        if audio.bytes.is_empty() {
            return Err(TranscribeError::File("The voice note is empty.".into()));
        }
        if audio.bytes.len() > MAX_BYTES {
            return Err(TranscribeError::File(
                "The voice note is too long to transcribe (the limit is 24 MB).".into(),
            ));
        }
        let part = reqwest::multipart::Part::bytes(audio.bytes.clone())
            .file_name(audio.file_name())
            .mime_str(&audio.content_type())
            .map_err(|_| TranscribeError::Unexpected)?;
        let mut form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", self.model.clone())
            .text("response_format", "json");
        if let Some(language) = language {
            form = form.text("language", language.to_owned());
        }
        let mut request = self
            .http
            .post(format!("{}/audio/transcriptions", self.base))
            .multipart(form);
        if !self.key.is_empty() {
            request = request.bearer_auth(&self.key);
        }
        let response = request.send().await.map_err(|_| TranscribeError::Offline)?;
        match response.status().as_u16() {
            200..=299 => {}
            401 | 403 => return Err(TranscribeError::Key),
            429 => return Err(TranscribeError::Limit),
            400 | 404 | 415 | 422 => {
                // The server's own words say what it does not take: a model
                // it does not have, a format it cannot read.
                let said = response.text().await.unwrap_or_default();
                return Err(TranscribeError::Setup(refusal(&said)));
            }
            413 => {
                return Err(TranscribeError::File(
                    "The service says the voice note is too large.".into(),
                ))
            }
            _ => return Err(TranscribeError::Unexpected),
        }
        let answer: Answer = response
            .json()
            .await
            .map_err(|_| TranscribeError::Unexpected)?;
        Ok(answer.text.trim().to_owned())
    }
}

/// The part of an error body worth showing: OpenAI-style
/// `{"error":{"message":..}}`, or the first line of plain text.
fn refusal(body: &str) -> String {
    #[derive(Deserialize)]
    struct Wrapped {
        error: Inner,
    }
    #[derive(Deserialize)]
    struct Inner {
        message: String,
    }
    let said = serde_json::from_str::<Wrapped>(body)
        .map(|wrapped| wrapped.error.message)
        .unwrap_or_else(|_| body.lines().next().unwrap_or_default().to_owned());
    let said: String = said.chars().take(200).collect();
    if said.trim().is_empty() {
        "The service did not accept the request. Check the address and the model.".into()
    } else {
        format!("The service said: {}", said.trim())
    }
}

pub mod system;

/// A transcriber that answers a fixed text, for tests.
#[cfg(test)]
pub struct Canned(pub Result<String, TranscribeError>);

#[cfg(test)]
#[async_trait]
impl Transcriber for Canned {
    async fn transcribe(&self, _: Audio, _: Option<&str>) -> Result<String, TranscribeError> {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn note() -> Audio {
        Audio {
            bytes: vec![1, 2, 3, 4],
            mime: Some("audio/ogg; codecs=opus".into()),
        }
    }

    #[tokio::test]
    async fn the_text_comes_back_trimmed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .and(header("authorization", "Bearer secret"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"text": " hola \n"})),
            )
            .expect(1)
            .mount(&server)
            .await;
        let base = format!("{}/v1", server.uri());
        let client = OpenAiCompatible::new(&base, "secret", "whisper-1").unwrap();
        let text = client.transcribe(note(), Some("es")).await.unwrap();
        assert_eq!(text, "hola");

        // The form carried the file under an extension, the model and the
        // language.
        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&requests[0].body).into_owned();
        assert!(body.contains("filename=\"voice.ogg\""));
        assert!(body.contains("name=\"model\"") && body.contains("whisper-1"));
        assert!(body.contains("name=\"language\"") && body.contains("\r\n\r\nes\r\n"));
    }

    #[tokio::test]
    async fn refusals_are_told_apart() {
        for (status, expected) in [
            (401, TranscribeError::Key),
            (429, TranscribeError::Limit),
            (500, TranscribeError::Unexpected),
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            let client = OpenAiCompatible::new(&server.uri(), "k", "m").unwrap();
            assert_eq!(client.transcribe(note(), None).await, Err(expected));
        }
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(404).set_body_json(
                serde_json::json!({"error": {"message": "The model `nope` does not exist"}}),
            ))
            .mount(&server)
            .await;
        let client = OpenAiCompatible::new(&server.uri(), "k", "nope").unwrap();
        assert_eq!(
            client.transcribe(note(), None).await,
            Err(TranscribeError::Setup(
                "The service said: The model `nope` does not exist".into()
            ))
        );
    }

    #[test]
    fn a_key_never_goes_over_plain_http_to_another_computer() {
        assert!(check_base("https://api.openai.com/v1").is_ok());
        assert!(check_base("http://localhost:8080/v1").is_ok());
        assert!(check_base("http://127.0.0.1:9000").is_ok());
        assert!(check_base("http://example.com/v1").is_err());
        assert!(check_base("ftp://example.com").is_err());
        assert!(check_base("not a url").is_err());
        // A trailing slash is not part of the base.
        assert_eq!(
            check_base("https://api.groq.com/openai/v1/").unwrap(),
            "https://api.groq.com/openai/v1"
        );
    }

    #[test]
    fn the_key_is_never_printed() {
        let client = OpenAiCompatible::new(OPENAI_BASE, "sk-very-secret", "whisper-1").unwrap();
        assert!(!format!("{client:?}").contains("sk-very-secret"));
    }

    #[test]
    fn the_configuration_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        assert_eq!(Config::load(&path), Config::default());
        let chosen = Config {
            base_url: "https://api.groq.com/openai/v1".into(),
            model: "whisper-large-v3".into(),
            language: "es".into(),
        };
        chosen.save(&path).unwrap();
        assert_eq!(Config::load(&path), chosen);
        assert_eq!(chosen.language_hint(), Some("es"));
        assert_eq!(Config::default().language_hint(), None);
    }
}
