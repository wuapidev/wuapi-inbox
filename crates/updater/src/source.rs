//! Where updates come from: a base URL with a manifest, its signature and
//! the files next to them.
//!
//! The default is this project's GitHub Releases, but nothing here knows
//! about GitHub: any static host that serves the same files under one
//! address works, and so does a directory (`file://`), which is what the
//! tests and a local trial use.
//!
//! A source that cannot be reached, or that answers "not found" (a private
//! repository answers that to everybody without a token), is not an error
//! the user hears about: there is simply no update.
//!
//! What a request says about the user: the application's version, the
//! platform and the channel, in the `User-Agent`. Nothing else, and no
//! identifier of any kind.

use crate::manifest::{Artifact, Channel, MAX_MANIFEST_BYTES};
use futures::StreamExt as _;
use reqwest::{StatusCode, Url};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::AsyncWriteExt as _;

/// The largest signature file that is read.
const MAX_SIGNATURE_BYTES: usize = 4 * 1024;

/// How long the network may take.
#[derive(Clone, Copy, Debug)]
pub struct Timeouts {
    /// To connect.
    pub connect: Duration,
    /// Between two pieces of an answer: a connection that goes silent for
    /// this long is given up on (and the download resumed later).
    pub read: Duration,
    /// For the manifest and its signature, each, as a whole.
    pub manifest: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(15),
            read: Duration::from_secs(30),
            manifest: Duration::from_secs(30),
        }
    }
}

/// Why something could not be fetched.
#[derive(Debug, thiserror::Error)]
pub enum SourceError {
    /// The base address is not one updates are taken from.
    #[error("updates cannot be taken from `{0}`: {1}")]
    BadBase(String, &'static str),
    /// The network, or the server, is having a bad moment.
    #[error("the update server could not be reached: {0}")]
    Unreachable(String),
    /// There is a manifest but no signature next to it.
    #[error("the manifest has no signature next to it")]
    Unsigned,
    /// The manifest names a file at an address that is not allowed.
    #[error("the update's file is at an address that is not allowed: {0}")]
    Refused(String),
    /// More bytes arrived than the file was said to have.
    #[error("the file is larger than the manifest says")]
    TooLarge,
    /// The transfer ended before the file was whole. What arrived is
    /// kept, and the next attempt goes on from there.
    #[error("the download stopped at {received} of {expected} bytes")]
    Incomplete {
        /// Bytes on disk.
        received: u64,
        /// Bytes the manifest says the file has.
        expected: u64,
    },
    /// The staging area could not be written.
    #[error("the download could not be written: {0}")]
    Io(#[from] std::io::Error),
}

impl SourceError {
    /// True for what passes by itself: the next check may well succeed,
    /// and nobody needs to be told meanwhile.
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Unreachable(_) | Self::Incomplete { .. })
    }
}

/// A manifest and its signature, as they were served. Nothing in them is
/// to be believed until [`crate::Manifest::verified`] has looked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fetched {
    /// The manifest's bytes.
    pub manifest: Vec<u8>,
    /// The signature file's text.
    pub signature: String,
}

#[derive(Clone, Debug)]
enum Base {
    /// `https://…`, or `http://` on this machine.
    Web { url: Url, local: bool },
    /// A directory.
    Files(PathBuf),
}

/// A place updates are fetched from.
#[derive(Clone, Debug)]
pub struct Source {
    base: Base,
    client: reqwest::Client,
    timeouts: Timeouts,
}

fn is_loopback(url: &Url) -> bool {
    match url.host_str() {
        Some("localhost") => true,
        Some(host) => host
            .trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback()),
        None => false,
    }
}

impl Source {
    /// A source at `base`.
    ///
    /// `https://` only. Plain `http://` is taken for this machine alone
    /// (`localhost`, `127.0.0.1`, `[::1]`: a local server in a test), and
    /// `file://` for a directory.
    pub fn new(base: &str, user_agent: &str, timeouts: Timeouts) -> Result<Self, SourceError> {
        let bad = |why| SourceError::BadBase(base.to_owned(), why);
        let mut url = Url::parse(base.trim()).map_err(|_| bad("it is not a URL"))?;
        let base = match url.scheme() {
            "file" => Base::Files(
                url.to_file_path()
                    .map_err(|()| bad("it is not a local path"))?,
            ),
            "https" | "http" => {
                let local = is_loopback(&url);
                if url.scheme() == "http" && !local {
                    return Err(bad("only https is used"));
                }
                if url.query().is_some() || url.fragment().is_some() {
                    return Err(bad("it has a query"));
                }
                if !url.username().is_empty() || url.password().is_some() {
                    return Err(bad("it carries credentials"));
                }
                // A base is a directory: names are joined under it.
                if !url.path().ends_with('/') {
                    let path = format!("{}/", url.path());
                    url.set_path(&path);
                }
                Base::Web { url, local }
            }
            _ => return Err(bad("only https is used")),
        };
        let https_only = matches!(&base, Base::Web { local: false, .. });
        let client = reqwest::Client::builder()
            .user_agent(user_agent)
            .https_only(https_only)
            .connect_timeout(timeouts.connect)
            .read_timeout(timeouts.read)
            .redirect(reqwest::redirect::Policy::limited(8))
            .build()
            .map_err(|error| SourceError::Unreachable(error.to_string()))?;
        Ok(Self {
            base,
            client,
            timeouts,
        })
    }

    /// Where the file called `name` next to the manifest is.
    fn beside(&self, name: &str) -> Result<Place, SourceError> {
        let plain = !name.is_empty()
            && name != "."
            && name != ".."
            && !name.contains(['/', '\\', '?', '#', ':', '%']);
        if !plain {
            return Err(SourceError::Refused(name.to_owned()));
        }
        Ok(match &self.base {
            Base::Files(dir) => Place::File(dir.join(name)),
            Base::Web { url, .. } => Place::Web(
                url.join(name)
                    .map_err(|_| SourceError::Refused(name.to_owned()))?,
            ),
        })
    }

    /// Where an artifact is: a name next to the manifest, or an absolute
    /// address of the kind the base itself may be.
    fn place_of(&self, artifact: &Artifact) -> Result<Place, SourceError> {
        let address = artifact.url.as_str();
        if !address.contains("://") {
            return self.beside(address);
        }
        let refused = || SourceError::Refused(address.to_owned());
        let url = Url::parse(address).map_err(|_| refused())?;
        match (url.scheme(), &self.base) {
            ("https", Base::Web { .. }) => Ok(Place::Web(url)),
            ("http", Base::Web { local: true, .. }) if is_loopback(&url) => Ok(Place::Web(url)),
            ("file", Base::Files(_)) => url.to_file_path().map(Place::File).map_err(|()| refused()),
            _ => Err(refused()),
        }
    }

    /// The channel's manifest and its signature. `None` when the source
    /// has no manifest (or will not show it).
    pub async fn manifest(&self, channel: Channel) -> Result<Option<Fetched>, SourceError> {
        let Some(manifest) = self
            .small(
                self.beside(channel.manifest_name())?,
                MAX_MANIFEST_BYTES + 1,
            )
            .await?
        else {
            return Ok(None);
        };
        let signature = self
            .small(self.beside(&channel.signature_name())?, MAX_SIGNATURE_BYTES)
            .await?
            .ok_or(SourceError::Unsigned)?;
        Ok(Some(Fetched {
            manifest,
            signature: String::from_utf8(signature).map_err(|_| SourceError::Unsigned)?,
        }))
    }

    /// Reads a small file whole, giving up past `limit` bytes.
    async fn small(&self, place: Place, limit: usize) -> Result<Option<Vec<u8>>, SourceError> {
        match place {
            Place::File(path) => match tokio::fs::read(&path).await {
                Ok(bytes) if bytes.len() > limit => Err(SourceError::TooLarge),
                Ok(bytes) => Ok(Some(bytes)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(error) => Err(SourceError::Unreachable(error.to_string())),
            },
            Place::Web(url) => {
                let read = async {
                    let response = self.client.get(url).send().await.map_err(offline)?;
                    let status = response.status();
                    if absent(status) {
                        return Ok(None);
                    }
                    if !status.is_success() {
                        return Err(SourceError::Unreachable(format!("HTTP {status}")));
                    }
                    let mut bytes = Vec::new();
                    let mut stream = response.bytes_stream();
                    while let Some(chunk) = stream.next().await {
                        bytes.extend_from_slice(&chunk.map_err(offline)?);
                        if bytes.len() > limit {
                            return Err(SourceError::TooLarge);
                        }
                    }
                    Ok(Some(bytes))
                };
                tokio::time::timeout(self.timeouts.manifest, read)
                    .await
                    .map_err(|_| SourceError::Unreachable("timed out".into()))?
            }
        }
    }

    /// Downloads `artifact` into `part`, going on from what `part` already
    /// holds. On success `part` has exactly the size the manifest says;
    /// its content is still to be checked against the manifest's hash.
    ///
    /// `progress` hears how many bytes are on disk.
    pub async fn download(
        &self,
        artifact: &Artifact,
        part: &Path,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<(), SourceError> {
        let expected = artifact.size;
        let mut have = match tokio::fs::metadata(part).await {
            Ok(meta) => meta.len(),
            Err(_) => 0,
        };
        if have > expected {
            tokio::fs::remove_file(part).await?;
            have = 0;
        }
        if have == expected {
            progress(have);
            return Ok(());
        }
        match self.place_of(artifact)? {
            Place::File(from) => {
                let size = tokio::fs::metadata(&from)
                    .await
                    .map_err(|error| SourceError::Unreachable(error.to_string()))?
                    .len();
                if size > expected {
                    return Err(SourceError::TooLarge);
                }
                tokio::fs::copy(&from, part).await?;
                progress(size);
                if size != expected {
                    return Err(SourceError::Incomplete {
                        received: size,
                        expected,
                    });
                }
                Ok(())
            }
            Place::Web(url) => {
                // Twice at most: the second time from the start, when the
                // server would not continue from where the file stands.
                for _ in 0..2 {
                    match self.transfer(&url, part, have, expected, progress).await? {
                        Transfer::Done => return Ok(()),
                        Transfer::StartOver => {
                            let _ = tokio::fs::remove_file(part).await;
                            have = 0;
                        }
                    }
                }
                Err(SourceError::Unreachable(
                    "the server would not serve the file".into(),
                ))
            }
        }
    }

    async fn transfer(
        &self,
        url: &Url,
        part: &Path,
        mut have: u64,
        expected: u64,
        progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<Transfer, SourceError> {
        let mut request = self.client.get(url.clone());
        if have > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={have}-"));
        }
        let response = request.send().await.map_err(offline)?;
        let status = response.status();
        let mut file = match status {
            StatusCode::PARTIAL_CONTENT if have > 0 => {
                // The server must be continuing from where the file
                // stands; anything else would splice two files together.
                let starts_here = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    .is_some_and(|value| value.starts_with(&format!("bytes {have}-")));
                if !starts_here {
                    return Ok(Transfer::StartOver);
                }
                tokio::fs::OpenOptions::new()
                    .append(true)
                    .open(part)
                    .await?
            }
            StatusCode::OK => {
                // The whole file, whatever was asked for.
                have = 0;
                tokio::fs::File::create(part).await?
            }
            StatusCode::RANGE_NOT_SATISFIABLE if have > 0 => return Ok(Transfer::StartOver),
            status if absent(status) => {
                return Err(SourceError::Unreachable(format!(
                    "the file is not there (HTTP {status})"
                )))
            }
            status => return Err(SourceError::Unreachable(format!("HTTP {status}"))),
        };
        if response
            .content_length()
            .is_some_and(|length| have + length > expected)
        {
            drop(file);
            let _ = tokio::fs::remove_file(part).await;
            return Err(SourceError::TooLarge);
        }
        let mut stream = response.bytes_stream();
        let outcome = loop {
            match stream.next().await {
                None => break Ok(()),
                Some(Err(error)) => break Err(offline(error)),
                Some(Ok(chunk)) => {
                    have += chunk.len() as u64;
                    if have > expected {
                        drop(file);
                        let _ = tokio::fs::remove_file(part).await;
                        return Err(SourceError::TooLarge);
                    }
                    file.write_all(&chunk).await?;
                    progress(have);
                }
            }
        };
        // What arrived is kept whatever happened: it is where the next
        // attempt goes on from.
        file.flush().await?;
        file.sync_all().await?;
        outcome?;
        if have != expected {
            return Err(SourceError::Incomplete {
                received: have,
                expected,
            });
        }
        Ok(Transfer::Done)
    }
}

enum Transfer {
    Done,
    StartOver,
}

enum Place {
    Web(Url),
    File(PathBuf),
}

/// "There is nothing here for you": also what a private repository
/// answers to a request without a token.
fn absent(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::GONE | StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    )
}

fn offline(error: reqwest::Error) -> SourceError {
    // Without the URL: it is in the settings, and a log line is no place
    // for whatever somebody put in it.
    SourceError::Unreachable(error.without_url().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(base: &str) -> Result<Source, SourceError> {
        Source::new(base, "test/1.0", Timeouts::default())
    }

    #[test]
    fn only_https_this_machine_or_a_directory() {
        assert!(source("https://example.com/releases/latest/download").is_ok());
        assert!(source("http://127.0.0.1:8080/updates").is_ok());
        assert!(source("http://localhost:8080").is_ok());
        assert!(source("http://[::1]:8080/").is_ok());
        assert!(source("file:///tmp/updates").is_ok() || cfg!(windows));
        for bad in [
            "http://example.com/updates",
            "ftp://example.com/updates",
            "example.com/updates",
            "https://user:secret@example.com/updates",
            "https://example.com/updates?token=abc",
            "",
        ] {
            assert!(
                matches!(source(bad), Err(SourceError::BadBase(..))),
                "{bad}"
            );
        }
    }

    #[test]
    fn names_are_joined_under_the_base() {
        let source = source("https://example.com/releases/latest/download").unwrap();
        let Place::Web(url) = source.beside("latest.json").unwrap() else {
            panic!("a web place");
        };
        assert_eq!(
            url.as_str(),
            "https://example.com/releases/latest/download/latest.json"
        );
        for bad in [
            "",
            "..",
            "../latest.json",
            "a/b",
            "a\\b",
            "c:evil",
            "x?y",
            "%2e%2e",
        ] {
            assert!(source.beside(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_artifact_is_fetched_only_from_where_the_base_could_be() {
        let artifact = |url: &str| Artifact {
            url: url.to_owned(),
            size: 1,
            sha256: String::new(),
        };
        let web = source("https://example.com/updates").unwrap();
        assert!(web.place_of(&artifact("app-1.0.tar.gz")).is_ok());
        assert!(web
            .place_of(&artifact("https://cdn.example.net/app-1.0.tar.gz"))
            .is_ok());
        for refused in [
            "http://cdn.example.net/app.tar.gz",
            "http://127.0.0.1/app.tar.gz",
            "file:///etc/passwd",
            "ftp://example.com/app.tar.gz",
            "../app.tar.gz",
        ] {
            assert!(
                matches!(
                    web.place_of(&artifact(refused)),
                    Err(SourceError::Refused(_))
                ),
                "{refused}"
            );
        }
        let local = source("http://127.0.0.1:9/updates").unwrap();
        assert!(local
            .place_of(&artifact("http://localhost:9/app.tar.gz"))
            .is_ok());
        assert!(local
            .place_of(&artifact("http://example.com/app.tar.gz"))
            .is_err());
        assert!(local.place_of(&artifact("file:///etc/passwd")).is_err());
    }
}
