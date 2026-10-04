//! What the updater keeps on disk, in `updates/` under the application's
//! data directory: what it knows (`state.json`), the update that is
//! downloaded and checked (`staged/<version>/`), and the mark that says a
//! new version was just put in place and has not been seen to start yet
//! (`pending.json`).
//!
//! None of it is trusted when it is read back: a staged update is
//! verified again, from the signature down, before it is installed.

use semver::Version;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where the updater's files are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    /// The name of the updater's directory inside the data directory.
    pub const DIR_NAME: &'static str = "updates";

    /// A layout rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The layout inside the application's data directory.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self::new(data_dir.join(Self::DIR_NAME))
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn state_file(&self) -> PathBuf {
        self.root.join("state.json")
    }

    fn pending_file(&self) -> PathBuf {
        self.root.join("pending.json")
    }

    /// Where staged updates are, one directory per version.
    pub fn staged_root(&self) -> PathBuf {
        self.root.join("staged")
    }

    /// The directory of one staged version.
    pub fn staged(&self, version: &Version) -> PathBuf {
        self.staged_root().join(version.to_string())
    }

    /// The manifest that announced the staged version, as it was served.
    pub fn manifest_file(&self, version: &Version) -> PathBuf {
        self.staged(version).join("manifest.json")
    }

    /// That manifest's signature.
    pub fn signature_file(&self, version: &Version) -> PathBuf {
        self.staged(version).join("manifest.json.sig")
    }

    /// The downloaded file, whole and checked.
    pub fn artifact_file(&self, version: &Version) -> PathBuf {
        self.staged(version).join("artifact.tar.gz")
    }

    /// The downloaded file while it is arriving.
    pub fn part_file(&self, version: &Version) -> PathBuf {
        self.staged(version).join("artifact.part")
    }

    /// Removes every staged version but `keep`.
    pub fn drop_staged_except(&self, keep: Option<&Version>) {
        let keep = keep.map(|version| version.to_string());
        let Ok(entries) = std::fs::read_dir(self.staged_root()) else {
            return;
        };
        for entry in entries.flatten() {
            if keep.as_deref() != entry.file_name().to_str() {
                let _ = crate::install::remove(&entry.path());
            }
        }
    }
}

/// Writes a small file whole: through a temporary one, so that a crash
/// midway leaves the old file or the new one.
fn write_whole(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let draft = path.with_extension("tmp");
    std::fs::write(&draft, bytes)?;
    std::fs::rename(&draft, path)
}

/// An update that is downloaded, checked and waiting for a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ready {
    /// Its version.
    pub version: Version,
    /// What is new in it.
    #[serde(default)]
    pub notes: String,
}

/// What the updater remembers between runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    /// When the source was last asked, in seconds since 1970.
    pub last_check: Option<u64>,
    /// How many checks in a row failed: what the wait before the next
    /// one grows with.
    pub failures: u32,
    /// The update waiting for a restart.
    pub ready: Option<Ready>,
    /// Versions that were installed and did not start. They are not
    /// installed again; a later version is.
    pub refused: Vec<Version>,
    /// Something to tell the user in the About section: an update that
    /// was undone, and why.
    pub notice: Option<String>,
}

impl State {
    /// Reads the state. A missing or damaged file is an empty state.
    pub fn load(layout: &Layout) -> Self {
        match std::fs::read(layout.state_file()) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                tracing::warn!(%error, "the updater's state is unreadable; starting over");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Writes the state.
    pub fn save(&self, layout: &Layout) {
        let bytes = serde_json::to_vec_pretty(self).expect("the state is plain data");
        if let Err(error) = write_whole(&layout.state_file(), &bytes) {
            tracing::warn!(%error, "the updater's state could not be saved");
        }
    }
}

/// The mark left when a new version is put in place: it stays until that
/// version has been seen to start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The version that was put in place.
    pub version: Version,
    /// The version it replaced, which is kept next to it.
    pub previous: Version,
    /// The newest database schema the replaced version can open.
    pub previous_schema: u32,
    /// The schema the new version said it was about to bring the
    /// database to, once it got that far.
    #[serde(default)]
    pub schema: Option<u32>,
    /// How many times the new version has started without getting as far
    /// as its window.
    #[serde(default)]
    pub starts: u32,
}

/// Why the version before cannot be put back by itself.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "version {version} already upgraded the local database to schema {schema}, and \
     version {previous} only opens it up to schema {previous_schema}"
)]
pub struct SchemaAhead {
    /// The version that upgraded it.
    pub version: Version,
    /// The version that would be gone back to.
    pub previous: Version,
    /// The schema the database may be at.
    pub schema: u32,
    /// The newest schema the version before opens.
    pub previous_schema: u32,
}

impl Pending {
    /// Reads the mark, if there is one.
    pub fn load(layout: &Layout) -> Option<Self> {
        let bytes = std::fs::read(layout.pending_file()).ok()?;
        match serde_json::from_slice(&bytes) {
            Ok(pending) => Some(pending),
            Err(error) => {
                tracing::warn!(%error, "the update mark is unreadable; dropping it");
                Self::clear(layout);
                None
            }
        }
    }

    /// Writes the mark.
    pub fn save(&self, layout: &Layout) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(self).expect("the mark is plain data");
        write_whole(&layout.pending_file(), &bytes)
    }

    /// Removes the mark.
    pub fn clear(layout: &Layout) {
        let _ = std::fs::remove_file(layout.pending_file());
    }

    /// Whether going back to the version before is safe for the data.
    ///
    /// Migrations only go forward, and an older build refuses a database
    /// with a newer schema. So once the new version may have migrated the
    /// database past what the old one opens, the old one is not put back
    /// by itself: it could not open the chats.
    pub fn may_go_back(&self) -> Result<(), SchemaAhead> {
        match self.schema {
            Some(schema) if schema > self.previous_schema => Err(SchemaAhead {
                version: self.version.clone(),
                previous: self.previous.clone(),
                schema,
                previous_schema: self.previous_schema,
            }),
            _ => Ok(()),
        }
    }
}

/// Seconds since 1970, now.
pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn the_state_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::in_data_dir(dir.path());
        assert_eq!(State::load(&layout), State::default(), "no file yet");
        let state = State {
            last_check: Some(1_790_000_000),
            failures: 2,
            ready: Some(Ready {
                version: version("1.2.0"),
                notes: "Faster.".into(),
            }),
            refused: vec![version("1.1.0")],
            notice: Some("1.1.0 could not start.".into()),
        };
        state.save(&layout);
        assert_eq!(State::load(&layout), state);
        assert!(layout.root().ends_with("updates"));
    }

    #[test]
    fn a_damaged_state_is_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        std::fs::write(dir.path().join("state.json"), "{ not json").unwrap();
        assert_eq!(State::load(&layout), State::default());
        // A state from a version that knew less keeps what it has.
        std::fs::write(dir.path().join("state.json"), r#"{"failures": 3}"#).unwrap();
        assert_eq!(State::load(&layout).failures, 3);
    }

    #[test]
    fn the_mark_is_written_read_and_cleared() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        assert_eq!(Pending::load(&layout), None);
        let pending = Pending {
            version: version("1.2.0"),
            previous: version("1.1.0"),
            previous_schema: 12,
            schema: None,
            starts: 0,
        };
        pending.save(&layout).unwrap();
        assert_eq!(Pending::load(&layout), Some(pending));
        Pending::clear(&layout);
        assert_eq!(Pending::load(&layout), None);
        // Rubbish is dropped rather than obeyed.
        std::fs::write(dir.path().join("pending.json"), "rubbish").unwrap();
        assert_eq!(Pending::load(&layout), None);
        assert!(!dir.path().join("pending.json").exists());
    }

    #[test]
    fn going_back_is_refused_once_the_database_is_ahead() {
        let mut pending = Pending {
            version: version("2.0.0"),
            previous: version("1.9.0"),
            previous_schema: 12,
            schema: None,
            starts: 1,
        };
        // It never got as far as the database.
        assert!(pending.may_go_back().is_ok());
        // It opened it, at the schema the old version knows too.
        pending.schema = Some(12);
        assert!(pending.may_go_back().is_ok());
        // It was about to migrate it.
        pending.schema = Some(13);
        let ahead = pending.may_go_back().unwrap_err();
        assert_eq!((ahead.schema, ahead.previous_schema), (13, 12));
        let said = ahead.to_string();
        assert!(said.contains("2.0.0") && said.contains("1.9.0"), "{said}");
        assert!(
            said.contains("schema 13") && said.contains("schema 12"),
            "{said}"
        );
    }

    #[test]
    fn staged_versions_have_their_own_directories() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::new(dir.path());
        let (old, new) = (version("1.0.0"), version("1.1.0"));
        for staged in [&old, &new] {
            std::fs::create_dir_all(layout.staged(staged)).unwrap();
            std::fs::write(layout.artifact_file(staged), "x").unwrap();
        }
        layout.drop_staged_except(Some(&new));
        assert!(!layout.staged(&old).exists());
        assert!(layout.artifact_file(&new).exists());
        layout.drop_staged_except(None);
        assert!(!layout.staged(&new).exists());
    }
}
