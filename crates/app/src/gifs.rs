//! Finding GIFs: where the picker's GIF tab gets its pictures from.
//!
//! A [`GifBackend`] answers a search with [`GifEntry`]s. Two exist:
//!
//! * **Saved** ([`SavedGifs`]): the GIFs in the library, whether saved from
//!   a chat, imported from disk, sent before or found online earlier. It is
//!   always there and reads the encrypted store; it never leaves the
//!   computer.
//! * **Giphy** ([`Giphy`]): the one online service, **off by default**.
//!   WhatsApp's own GIF search is a third party's (Tenor or Giphy), and
//!   this application calls no third party unless the user turned it on in
//!   Settings and gave their own API key for it. Nothing here has a key,
//!   and nothing is requested while the switch is off: the only way to get
//!   a [`Giphy`] is [`Giphy::new`] with a key, which the settings screen
//!   calls when both are there.
//!
//! Giphy's public API is one `GET` per search with the key in the query
//! (<https://developers.giphy.com/docs/api/endpoint>). What a search sends
//! is the key and the words typed; what comes back is a list of GIFs, each
//! with an MP4 (what is sent: WhatsApp's GIFs are MP4s) and small previews.
//! Previews and the MP4 are fetched only from the service's own hosts,
//! only over what the base URL says, behind a size limit.

use async_trait::async_trait;
use client_core::{LibraryItem, LibraryKind, Store};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

/// Where Giphy's API is.
pub const GIPHY_BASE: &str = "https://api.giphy.com";
/// How many results a search asks for.
pub const RESULTS: usize = 24;
/// The most a preview may be, in bytes.
const PREVIEW_MAX: u64 = 600 * 1024;
/// How long a request may take.
const TIMEOUT: Duration = Duration::from_secs(20);

/// Why a search or a download did not work, in words for the user.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GifError {
    /// The service refused the key.
    #[error("The service did not accept the API key. Check it in Settings.")]
    Key,
    /// Too many requests.
    #[error("The service says too many searches were made. Try again in a moment.")]
    Limit,
    /// No connection, or the service did not answer.
    #[error("The search could not reach the service.")]
    Offline,
    /// An answer this application cannot read.
    #[error("The service answered something unexpected.")]
    Unexpected,
    /// A file that is not what it should be, or too large.
    #[error("{0}")]
    File(String),
}

/// One GIF found online.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GifHit {
    /// The service's id.
    pub id: String,
    /// What the service calls it.
    pub title: String,
    /// The MP4 that is sent.
    pub media_url: String,
    /// Its size in bytes, when the service says.
    pub media_bytes: Option<u64>,
    /// Its size in pixels.
    pub width: u32,
    /// Its size in pixels.
    pub height: u32,
    /// A small still of the first frame.
    pub still_url: Option<String>,
    /// A small moving preview (a GIF), fetched when it is looked at.
    pub moving_url: Option<String>,
}

/// One result of the GIF tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GifEntry {
    /// A GIF of the library.
    Saved(LibraryItem),
    /// A GIF of the online service, not kept yet.
    Online(GifHit),
}

/// Somewhere to look for GIFs.
#[async_trait]
pub trait GifBackend: Send + Sync {
    /// The GIFs for `query`; with nothing typed, the backend's own idea of
    /// a first page (the saved ones, the trending ones).
    async fn search(&self, query: &str) -> Result<Vec<GifEntry>, GifError>;
}

/// The library's GIFs: those used last first when nothing is typed, and
/// by name and pack when it is.
pub struct SavedGifs {
    store: Arc<Store>,
}

impl SavedGifs {
    /// The saved GIFs of a store.
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// The search itself, without waiting for anything.
    pub fn find(&self, query: &str) -> Vec<LibraryItem> {
        let mut items = self
            .store
            .library_items(LibraryKind::Gif)
            .unwrap_or_default();
        let recent: Vec<String> = self
            .store
            .library_recent(LibraryKind::Gif, client_core::RECENT_LIMIT)
            .unwrap_or_default()
            .into_iter()
            .map(|item| item.id)
            .collect();
        let query = query.trim().to_lowercase();
        if !query.is_empty() {
            items.retain(|item| {
                item.name
                    .as_deref()
                    .is_some_and(|name| name.to_lowercase().contains(&query))
            });
            return items;
        }
        // The recent ones first, in the order they were used, then the rest
        // newest first.
        items.sort_by_key(|item| {
            recent
                .iter()
                .position(|id| *id == item.id)
                .unwrap_or(usize::MAX)
        });
        items
    }
}

#[async_trait]
impl GifBackend for SavedGifs {
    async fn search(&self, query: &str) -> Result<Vec<GifEntry>, GifError> {
        Ok(self.find(query).into_iter().map(GifEntry::Saved).collect())
    }
}

// ----- Giphy ----------------------------------------------------------------------

/// Giphy's search, with the user's own key.
#[derive(Clone)]
pub struct Giphy {
    http: reqwest::Client,
    base: String,
    key: String,
}

impl std::fmt::Debug for Giphy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never the key.
        f.debug_struct("Giphy").field("base", &self.base).finish()
    }
}

#[derive(Deserialize)]
struct Answer {
    #[serde(default)]
    data: Vec<Item>,
}

#[derive(Deserialize)]
struct Item {
    id: String,
    #[serde(default)]
    title: String,
    images: Images,
}

#[derive(Deserialize, Default)]
struct Images {
    original_mp4: Option<Rendition>,
    fixed_height: Option<Rendition>,
    fixed_width_small_still: Option<Rendition>,
    fixed_width_small: Option<Rendition>,
}

#[derive(Deserialize)]
struct Rendition {
    url: Option<String>,
    mp4: Option<String>,
    width: Option<String>,
    height: Option<String>,
    mp4_size: Option<String>,
}

/// The MP4 sent is the original when it is small enough to go over a
/// phone's connection, and the service's compressed one when it is not.
const MP4_COMFORTABLE: u64 = 3 * 1024 * 1024;

impl Giphy {
    /// A search over `base` (Giphy's API, [`GIPHY_BASE`]) with `key`.
    pub fn new(base: &str, key: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .user_agent(crate::product::user_agent())
                .build()
                .unwrap_or_default(),
            base: base.trim_end_matches('/').to_owned(),
            key: key.trim().to_owned(),
        }
    }

    async fn list(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<GifEntry>, GifError> {
        let mut params: Vec<(&str, String)> = vec![
            ("api_key", self.key.clone()),
            ("limit", RESULTS.to_string()),
            ("rating", "pg-13".to_owned()),
        ];
        params.extend(query.iter().cloned());
        let url = reqwest::Url::parse_with_params(&format!("{}{path}", self.base), &params)
            .map_err(|_| GifError::Unexpected)?;
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|_| GifError::Offline)?;
        match response.status().as_u16() {
            200..=299 => {}
            401 | 403 => return Err(GifError::Key),
            429 => return Err(GifError::Limit),
            _ => return Err(GifError::Offline),
        }
        let answer: Answer = response.json().await.map_err(|_| GifError::Unexpected)?;
        Ok(answer
            .data
            .into_iter()
            .filter_map(hit)
            .map(GifEntry::Online)
            .collect())
    }

    /// Whether `url` is somewhere this search may fetch from: the
    /// service's own API host, or a subdomain of `giphy.com` over HTTPS.
    fn trusted(&self, url: &str) -> bool {
        let Ok(parsed) = reqwest::Url::parse(url) else {
            return false;
        };
        let Ok(base) = reqwest::Url::parse(&self.base) else {
            return false;
        };
        let host = parsed.host_str().unwrap_or_default();
        let same = parsed.scheme() == base.scheme()
            && host == base.host_str().unwrap_or_default()
            && parsed.port() == base.port();
        let giphy =
            parsed.scheme() == "https" && (host == "giphy.com" || host.ends_with(".giphy.com"));
        same || giphy
    }

    /// A file of a result, at most `max` bytes.
    pub async fn fetch(&self, url: &str, max: u64) -> Result<(Vec<u8>, Option<String>), GifError> {
        if !self.trusted(url) {
            return Err(GifError::File("That file is not on the service.".into()));
        }
        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|_| GifError::Offline)?;
        if !response.status().is_success() {
            return Err(GifError::Offline);
        }
        if response.content_length().is_some_and(|length| length > max) {
            return Err(GifError::File("The GIF is too large.".into()));
        }
        let mime = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .map(|value| {
                value
                    .split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_owned()
            });
        // Read up to the limit and no further.
        let mut bytes = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|_| GifError::Offline)? {
            if bytes.len() as u64 + chunk.len() as u64 > max {
                return Err(GifError::File("The GIF is too large.".into()));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok((bytes, mime))
    }

    /// The MP4 of a result: what is sent.
    pub async fn download(&self, hit: &GifHit) -> Result<Vec<u8>, GifError> {
        let (bytes, _) = self
            .fetch(&hit.media_url, client_core::GIF_MAX as u64)
            .await?;
        Ok(bytes)
    }

    /// A small preview of a result.
    pub async fn preview(&self, url: &str) -> Result<Vec<u8>, GifError> {
        Ok(self.fetch(url, PREVIEW_MAX).await?.0)
    }
}

fn number(text: &Option<String>) -> Option<u64> {
    text.as_deref()?.trim().parse().ok()
}

/// A result of the service as a [`GifHit`]; `None` when it has no MP4.
fn hit(item: Item) -> Option<GifHit> {
    let original = item.images.original_mp4.as_ref();
    let comfortable = original
        .filter(|rendition| number(&rendition.mp4_size).is_none_or(|size| size <= MP4_COMFORTABLE));
    let chosen = comfortable
        .or(item.images.fixed_height.as_ref())
        .or(original)?;
    let media_url = chosen.mp4.clone()?;
    Some(GifHit {
        id: item.id,
        title: item.title.trim().to_owned(),
        media_bytes: number(&chosen.mp4_size),
        width: number(&chosen.width).unwrap_or(0) as u32,
        height: number(&chosen.height).unwrap_or(0) as u32,
        still_url: item
            .images
            .fixed_width_small_still
            .and_then(|rendition| rendition.url),
        moving_url: item
            .images
            .fixed_width_small
            .and_then(|rendition| rendition.url),
        media_url,
    })
}

#[async_trait]
impl GifBackend for Giphy {
    async fn search(&self, query: &str) -> Result<Vec<GifEntry>, GifError> {
        let query = query.trim();
        if query.is_empty() {
            self.list("/v1/gifs/trending", &[]).await
        } else {
            self.list("/v1/gifs/search", &[("q", query.to_owned())])
                .await
        }
    }
}

// ----- the key -----------------------------------------------------------------------

/// Where the user's API key for the online service is kept: the OS
/// keychain, never the settings file.
pub trait SecretStore: Send + Sync {
    /// The key, when there is one.
    fn get(&self) -> Result<Option<String>, String>;
    /// Keeps a key, replacing the one before.
    fn set(&self, key: &str) -> Result<(), String>;
    /// Forgets the key.
    fn delete(&self) -> Result<(), String>;
}

/// The key in the OS keychain, filed under the application.
pub struct KeychainSecret {
    service: String,
}

impl KeychainSecret {
    /// The application's own entry.
    pub fn new() -> Self {
        Self {
            service: crate::product::slug(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry, String> {
        crate::storage::keyring_entry(&self.service, "gif-search-key:giphy")
    }
}

impl Default for KeychainSecret {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore for KeychainSecret {
    fn get(&self) -> Result<Option<String>, String> {
        match self.entry()?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, key: &str) -> Result<(), String> {
        self.entry()?
            .set_password(key)
            .map_err(|error| error.to_string())
    }

    fn delete(&self) -> Result<(), String> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// A key held in memory: for tests, and for a session without a keychain.
#[derive(Default)]
pub struct MemorySecret(std::sync::Mutex<Option<String>>);

impl SecretStore for MemorySecret {
    fn get(&self) -> Result<Option<String>, String> {
        Ok(self.0.lock().expect("secret lock").clone())
    }

    fn set(&self, key: &str) -> Result<(), String> {
        *self.0.lock().expect("secret lock") = Some(key.to_owned());
        Ok(())
    }

    fn delete(&self) -> Result<(), String> {
        *self.0.lock().expect("secret lock") = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_core::{LibrarySource, NewLibraryItem};
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const SEARCH: &str = r#"{
        "data": [
            {"id": "a1", "title": "  Cat waves ",
             "images": {
                "original_mp4": {"mp4": "MEDIA/a1-original.mp4", "width": "480", "height": "270", "mp4_size": "900000"},
                "fixed_height": {"mp4": "MEDIA/a1-small.mp4", "width": "356", "height": "200", "mp4_size": "100000"},
                "fixed_width_small_still": {"url": "MEDIA/a1-still.gif", "width": "100", "height": "56"},
                "fixed_width_small": {"url": "MEDIA/a1-small.gif", "width": "100", "height": "56"}}},
            {"id": "b2", "title": "Big dog",
             "images": {
                "original_mp4": {"mp4": "MEDIA/b2-original.mp4", "width": "800", "height": "450", "mp4_size": "9000000"},
                "fixed_height": {"mp4": "MEDIA/b2-small.mp4", "width": "356", "height": "200", "mp4_size": "300000"}}},
            {"id": "c3", "title": "No video here", "images": {}}
        ],
        "meta": {"status": 200, "msg": "OK"}
    }"#;

    async fn serving() -> MockServer {
        let server = MockServer::start().await;
        let body = SEARCH.replace("MEDIA", &server.uri());
        Mock::given(method("GET"))
            .and(path("/v1/gifs/search"))
            .and(query_param("api_key", "my-key"))
            .and(query_param("q", "cat"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body.clone(), "application/json"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/gifs/trending"))
            .and(query_param("api_key", "my-key"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/json"))
            .mount(&server)
            .await;
        server
    }

    fn hits(entries: Vec<GifEntry>) -> Vec<GifHit> {
        entries
            .into_iter()
            .map(|entry| match entry {
                GifEntry::Online(hit) => hit,
                GifEntry::Saved(_) => panic!("online results"),
            })
            .collect()
    }

    #[tokio::test]
    async fn a_search_sends_the_key_and_the_words_and_reads_the_gifs() {
        let server = serving().await;
        let giphy = Giphy::new(&server.uri(), " my-key ");
        let found = hits(giphy.search("cat").await.unwrap());
        // The one without a video is left out.
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].id, "a1");
        assert_eq!(found[0].title, "Cat waves");
        assert_eq!((found[0].width, found[0].height), (480, 270));
        assert_eq!(
            found[0].media_url,
            format!("{}/a1-original.mp4", server.uri())
        );
        assert_eq!(
            found[0].still_url.as_deref(),
            Some(format!("{}/a1-still.gif", server.uri()).as_str())
        );
        // A big original is not what is sent: the compressed one is.
        assert_eq!(found[1].media_url, format!("{}/b2-small.mp4", server.uri()));
        assert_eq!(found[1].media_bytes, Some(300_000));
        let asked = server.received_requests().await.unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].url.path(), "/v1/gifs/search");
        let query: Vec<(String, String)> = asked[0]
            .url
            .query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
        assert!(query.contains(&("q".into(), "cat".into())));
        assert!(query.contains(&("api_key".into(), "my-key".into())));
    }

    #[tokio::test]
    async fn with_nothing_typed_it_asks_for_the_trending_ones() {
        let server = serving().await;
        let giphy = Giphy::new(&server.uri(), "my-key");
        assert_eq!(hits(giphy.search("  ").await.unwrap()).len(), 2);
        assert_eq!(
            server.received_requests().await.unwrap()[0].url.path(),
            "/v1/gifs/trending"
        );
    }

    #[tokio::test]
    async fn a_refused_key_and_a_busy_service_are_said_in_words() {
        let server = MockServer::start().await;
        Mock::given(path("/v1/gifs/search"))
            .and(query_param("q", "no-key"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        Mock::given(path("/v1/gifs/search"))
            .and(query_param("q", "busy"))
            .respond_with(ResponseTemplate::new(429))
            .mount(&server)
            .await;
        Mock::given(path("/v1/gifs/search"))
            .and(query_param("q", "odd"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<html>"))
            .mount(&server)
            .await;
        let giphy = Giphy::new(&server.uri(), "k");
        assert_eq!(giphy.search("no-key").await.unwrap_err(), GifError::Key);
        assert_eq!(giphy.search("busy").await.unwrap_err(), GifError::Limit);
        assert_eq!(giphy.search("odd").await.unwrap_err(), GifError::Unexpected);
        // Nothing listening: offline.
        let gone = Giphy::new("http://127.0.0.1:1", "k");
        assert_eq!(gone.search("x").await.unwrap_err(), GifError::Offline);
    }

    #[tokio::test]
    async fn files_come_only_from_the_service_and_within_the_limit() {
        let server = serving().await;
        Mock::given(method("GET"))
            .and(path("/a1-original.mp4"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![7u8; 1000], "video/mp4"))
            .mount(&server)
            .await;
        let giphy = Giphy::new(&server.uri(), "my-key");
        let found = hits(giphy.search("cat").await.unwrap());
        assert_eq!(giphy.download(&found[0]).await.unwrap().len(), 1000);
        // Somewhere else is refused before anything is requested.
        let elsewhere = GifHit {
            media_url: "https://example.com/x.mp4".into(),
            ..found[0].clone()
        };
        assert!(matches!(
            giphy.download(&elsewhere).await,
            Err(GifError::File(_))
        ));
        // Too large for a preview.
        Mock::given(method("GET"))
            .and(path("/huge.gif"))
            .respond_with(
                ResponseTemplate::new(200).set_body_raw(vec![1u8; 700 * 1024], "image/gif"),
            )
            .mount(&server)
            .await;
        assert!(matches!(
            giphy.preview(&format!("{}/huge.gif", server.uri())).await,
            Err(GifError::File(_))
        ));
        assert!(giphy.trusted("https://media3.giphy.com/media/x/giphy.mp4"));
        assert!(!giphy.trusted("http://media3.giphy.com/media/x/giphy.mp4"));
        assert!(!giphy.trusted("https://giphy.com.evil.example/x"));
    }

    #[tokio::test]
    async fn the_saved_backend_lists_the_library_and_never_goes_anywhere() {
        let store = Arc::new(Store::open_in_memory().unwrap());
        let put = |tag: u8, name: &str, now: i64| {
            let bytes = vec![tag; 64];
            let id = client_core::content_id(&bytes);
            store
                .library_add(
                    NewLibraryItem {
                        kind: LibraryKind::Gif,
                        bytes,
                        mime: "video/mp4".into(),
                        animated: true,
                        size: None,
                        source: LibrarySource::Imported,
                        name: Some(name.into()),
                        pack: None,
                        thumb: None,
                    },
                    &id,
                    client_provider::Timestamp::from_millis(now),
                    client_core::LIBRARY_BUDGET,
                )
                .unwrap()
                .0
        };
        let wave = put(1, "Cat wave", 1);
        let dance = put(2, "Dog dance", 2);
        let saved = SavedGifs::new(store.clone());
        // Newest first; the one used last goes before them.
        let names = |entries: Vec<GifEntry>| -> Vec<String> {
            entries
                .into_iter()
                .map(|entry| match entry {
                    GifEntry::Saved(item) => item.name.unwrap(),
                    _ => unreachable!(),
                })
                .collect()
        };
        assert_eq!(
            names(saved.search("").await.unwrap()),
            ["Dog dance", "Cat wave"]
        );
        store
            .library_touch(&wave.id, client_provider::Timestamp::from_millis(10))
            .unwrap();
        assert_eq!(
            names(saved.search("").await.unwrap()),
            ["Cat wave", "Dog dance"]
        );
        assert_eq!(names(saved.search("DOG").await.unwrap()), ["Dog dance"]);
        assert!(saved.search("zebra").await.unwrap().is_empty());
        let _ = dance;
    }

    #[test]
    fn a_key_in_memory_is_set_read_and_forgotten() {
        let secret = MemorySecret::default();
        assert_eq!(secret.get().unwrap(), None);
        secret.set("abc").unwrap();
        assert_eq!(secret.get().unwrap().as_deref(), Some("abc"));
        secret.delete().unwrap();
        assert_eq!(secret.get().unwrap(), None);
        // And the debug output never prints a key.
        assert!(!format!("{:?}", Giphy::new(GIPHY_BASE, "super-secret")).contains("super-secret"));
    }
}
