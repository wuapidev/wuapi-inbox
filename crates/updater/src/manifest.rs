//! The release manifest: what the newest version is and where its files are.
//!
//! One JSON file per channel (`latest.json` for `stable`, `beta.json` for
//! `beta`), signed as a whole (see [`crate::sign`]). The only way to get a
//! [`Manifest`] from bytes that came over the network is
//! [`Manifest::verified`], which checks the signature first.

use crate::sign::{self, PublicKey, SignError};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The version of the manifest's own format that this build reads.
pub const FORMAT: u32 = 1;

/// The largest manifest that is read.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

/// A release channel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// Releases.
    #[default]
    Stable,
    /// Pre-releases.
    Beta,
}

impl Channel {
    /// The channel's name, as the manifest and the About section say it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Beta => "beta",
        }
    }

    /// The name of the channel's manifest under the base URL.
    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::Stable => "latest.json",
            Self::Beta => "beta.json",
        }
    }

    /// The name of the manifest's signature under the base URL.
    pub fn signature_name(self) -> String {
        format!("{}.sig", self.manifest_name())
    }

    /// Reads a channel's name.
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "stable" => Some(Self::Stable),
            "beta" => Some(Self::Beta),
            _ => None,
        }
    }
}

/// The file of one platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    /// Where the file is: a file name next to the manifest (the usual
    /// case, which makes a release independent of where it is hosted), or
    /// an absolute `https://` URL.
    pub url: String,
    /// Its size in bytes.
    pub size: u64,
    /// Its SHA-256, in lower-case hexadecimal.
    pub sha256: String,
}

/// Marks a manifest whose version is lower than the versions it replaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rollback {
    /// The versions that are to go back to the manifest's version. A
    /// build that is not one of them ignores the manifest, so a rollback
    /// manifest replayed later takes nobody back.
    pub from: Vec<Version>,
}

/// A release manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// The format of this file: [`FORMAT`].
    pub format: u32,
    /// The channel this manifest is for.
    pub channel: Channel,
    /// The version it announces.
    pub version: Version,
    /// When it was released (RFC 3339, UTC).
    pub released: String,
    /// What is new, as short Markdown.
    #[serde(default)]
    pub notes: String,
    /// The oldest version that can update itself to this one. An older
    /// build is told to install the new version by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_supported: Option<Version>,
    /// Present when this manifest takes named versions back to an older one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<Rollback>,
    /// The file for each platform, by [`platform_key`].
    pub platforms: BTreeMap<String, Artifact>,
}

/// Why a manifest is not used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    /// The signature is missing, damaged, or not by a trusted key.
    #[error("the manifest's signature is not good: {0}")]
    Signature(#[from] SignError),
    /// The file is larger than any manifest is.
    #[error("the manifest is too large")]
    TooLarge,
    /// The file is not a manifest.
    #[error("the manifest cannot be read: {0}")]
    Unreadable(String),
    /// The manifest is in a format this build does not know.
    #[error("the manifest is in format {0}, which this version does not read")]
    Format(u32),
    /// The manifest is for another channel.
    #[error("the manifest is for the {found} channel, not {wanted}")]
    Channel {
        /// The channel it names.
        found: &'static str,
        /// The channel that was asked for.
        wanted: &'static str,
    },
    /// An artifact's description is not usable.
    #[error("the manifest's file for {0} is not usable: {1}")]
    Artifact(String, &'static str),
}

/// What to do about a manifest, for a build at some version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Nothing: this build is the manifest's version or a newer one.
    UpToDate,
    /// Download and install this file.
    Install(Artifact),
    /// The new version cannot be installed over this build (it is older
    /// than `min_supported`): it has to be installed by hand.
    Reinstall,
    /// There is a new version, but no file for this platform.
    NoArtifact,
}

impl Manifest {
    /// Reads a manifest whose signature by one of `keys` is good. Nothing
    /// of the file is looked at before the signature is.
    pub fn verified(
        bytes: &[u8],
        signature: &str,
        keys: &[PublicKey],
        channel: Channel,
    ) -> Result<Self, ManifestError> {
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        sign::verify(keys, bytes, signature)?;
        let manifest = Self::parse(bytes)?;
        if manifest.channel != channel {
            return Err(ManifestError::Channel {
                found: manifest.channel.name(),
                wanted: channel.name(),
            });
        }
        Ok(manifest)
    }

    /// Reads a manifest without asking who wrote it: for the tool that
    /// writes them, and for the tests. The application uses
    /// [`Manifest::verified`].
    pub fn parse(bytes: &[u8]) -> Result<Self, ManifestError> {
        // The format is looked at first, so that a later format is said to
        // be that and not "unreadable".
        #[derive(Deserialize)]
        struct Head {
            format: u32,
        }
        let head: Head = serde_json::from_slice(bytes)
            .map_err(|error| ManifestError::Unreadable(error.to_string()))?;
        if head.format != FORMAT {
            return Err(ManifestError::Format(head.format));
        }
        let manifest: Self = serde_json::from_slice(bytes)
            .map_err(|error| ManifestError::Unreadable(error.to_string()))?;
        for (platform, artifact) in &manifest.platforms {
            artifact
                .check()
                .map_err(|why| ManifestError::Artifact(platform.clone(), why))?;
        }
        Ok(manifest)
    }

    /// The manifest as the file that is signed and published.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = serde_json::to_vec_pretty(self).expect("a manifest is plain data");
        bytes.push(b'\n');
        bytes
    }

    /// What a build at `current` on `platform` does with this manifest.
    ///
    /// A lower version is never installed, unless the manifest says it is
    /// a rollback from exactly this build's version.
    pub fn decide(&self, current: &Version, platform: &str) -> Decision {
        let wanted = match self.version.cmp(current) {
            std::cmp::Ordering::Equal => false,
            std::cmp::Ordering::Greater => true,
            std::cmp::Ordering::Less => self
                .rollback
                .as_ref()
                .is_some_and(|rollback| rollback.from.contains(current)),
        };
        if !wanted {
            return Decision::UpToDate;
        }
        if self
            .min_supported
            .as_ref()
            .is_some_and(|oldest| current < oldest)
        {
            return Decision::Reinstall;
        }
        match self.platforms.get(platform) {
            Some(artifact) => Decision::Install(artifact.clone()),
            None => Decision::NoArtifact,
        }
    }
}

impl Artifact {
    fn check(&self) -> Result<(), &'static str> {
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("the SHA-256 is not 64 lower-case hexadecimal digits");
        }
        if self.size == 0 {
            return Err("the size is zero");
        }
        if self.url.is_empty() {
            return Err("the address is empty");
        }
        Ok(())
    }
}

/// The key of a platform in a manifest: `linux-x86_64`, `macos-aarch64`…
pub fn platform_key_of(os: &str, arch: &str) -> String {
    format!("{os}-{arch}")
}

/// The key of the platform this build runs on.
pub fn platform_key() -> String {
    platform_key_of(std::env::consts::OS, std::env::consts::ARCH)
}

/// The platforms releases are built for.
pub const PLATFORMS: [&str; 5] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-aarch64",
    "macos-x86_64",
    "windows-x86_64",
];

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sign::SecretKey;

    pub(crate) fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    pub(crate) fn manifest(version_text: &str) -> Manifest {
        Manifest {
            format: FORMAT,
            channel: Channel::Stable,
            version: version(version_text),
            released: "2026-10-02T12:00:00Z".into(),
            notes: "- Faster.\n- **Safer.**".into(),
            min_supported: None,
            rollback: None,
            platforms: BTreeMap::from([(
                "linux-x86_64".to_owned(),
                Artifact {
                    url: format!("wuapi-inbox-{version_text}-linux-x86_64.tar.gz"),
                    size: 1234,
                    sha256: "ab".repeat(32),
                },
            )]),
        }
    }

    #[test]
    fn a_manifest_reads_back_as_it_was_written() {
        let mut written = manifest("1.4.0");
        written.min_supported = Some(version("1.0.0"));
        let bytes = written.to_bytes();
        assert_eq!(Manifest::parse(&bytes).unwrap(), written);
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.contains(r#""version": "1.4.0""#), "{text}");
        assert!(text.contains(r#""channel": "stable""#), "{text}");
        assert!(!text.contains("rollback"), "left out when there is none");
    }

    #[test]
    fn the_published_shape_is_read() {
        let text = r#"{
            "format": 1,
            "channel": "stable",
            "version": "0.2.0",
            "released": "2026-10-02T12:00:00Z",
            "notes": "Fixes.",
            "min_supported": "0.1.0",
            "platforms": {
                "windows-x86_64": {
                    "url": "https://example.com/wuapi-inbox-0.2.0-windows-x86_64.tar.gz",
                    "size": 10,
                    "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
                }
            },
            "something_added_later": true
        }"#;
        let manifest = Manifest::parse(text.as_bytes()).unwrap();
        assert_eq!(manifest.version, version("0.2.0"));
        assert_eq!(manifest.platforms["windows-x86_64"].size, 10);
    }

    #[test]
    fn what_is_not_a_manifest_is_refused() {
        for (text, what) in [
            ("", "empty"),
            ("{}", "no format"),
            (r#"{"format": 2}"#, "a later format"),
            (
                r#"{"format":1,"channel":"nightly","version":"1.0.0","released":"","platforms":{}}"#,
                "an unknown channel",
            ),
            (
                r#"{"format":1,"channel":"stable","version":"one","released":"","platforms":{}}"#,
                "a version that is not one",
            ),
            (
                r#"{"format":1,"channel":"stable","version":"1.0.0","released":"","platforms":{"linux-x86_64":{"url":"a","size":1,"sha256":"XYZ"}}}"#,
                "a hash that is not one",
            ),
            (
                r#"{"format":1,"channel":"stable","version":"1.0.0","released":"","platforms":{"linux-x86_64":{"url":"a","size":0,"sha256":"0000000000000000000000000000000000000000000000000000000000000000"}}}"#,
                "an empty file",
            ),
        ] {
            assert!(Manifest::parse(text.as_bytes()).is_err(), "{what}");
        }
        assert_eq!(
            Manifest::parse(br#"{"format": 2}"#),
            Err(ManifestError::Format(2))
        );
    }

    #[test]
    fn only_a_signed_manifest_is_read() {
        let secret = SecretKey::generate().unwrap();
        let keys = [secret.public()];
        let bytes = manifest("2.0.0").to_bytes();
        let signature = sign::sign(&secret, &bytes, "version:2.0.0");
        let read = Manifest::verified(&bytes, &signature, &keys, Channel::Stable).unwrap();
        assert_eq!(read.version, version("2.0.0"));

        // One byte changed: the version reads 2.0.1, the signature does not.
        let tampered = String::from_utf8(bytes.clone())
            .unwrap()
            .replace("\"2.0.0\"", "\"2.0.1\"")
            .into_bytes();
        assert_eq!(
            Manifest::verified(&tampered, &signature, &keys, Channel::Stable),
            Err(ManifestError::Signature(SignError::Mismatch))
        );
        // Signed by somebody else.
        let other = SecretKey::generate().unwrap();
        assert!(matches!(
            Manifest::verified(&bytes, &signature, &[other.public()], Channel::Stable),
            Err(ManifestError::Signature(SignError::UnknownKey(_)))
        ));
        // No key embedded yet: nothing verifies.
        assert!(Manifest::verified(&bytes, &signature, &[], Channel::Stable).is_err());
        // A good signature on a manifest of another channel.
        assert_eq!(
            Manifest::verified(&bytes, &signature, &keys, Channel::Beta),
            Err(ManifestError::Channel {
                found: "stable",
                wanted: "beta"
            })
        );
        // Not signed at all.
        assert!(Manifest::verified(&bytes, "", &keys, Channel::Stable).is_err());
    }

    #[test]
    fn too_large_a_manifest_is_refused_before_anything_else() {
        let secret = SecretKey::generate().unwrap();
        let bytes = vec![b' '; MAX_MANIFEST_BYTES + 1];
        let signature = sign::sign(&secret, &bytes, "c");
        assert_eq!(
            Manifest::verified(&bytes, &signature, &[secret.public()], Channel::Stable),
            Err(ManifestError::TooLarge)
        );
    }

    #[test]
    fn a_newer_version_is_installed_and_an_older_one_never() {
        let manifest = manifest("1.4.0");
        let install = Decision::Install(manifest.platforms["linux-x86_64"].clone());
        assert_eq!(
            manifest.decide(&version("1.3.9"), "linux-x86_64"),
            install.clone()
        );
        assert_eq!(
            manifest.decide(&version("1.4.0-beta.2"), "linux-x86_64"),
            install,
            "a release is newer than its pre-releases"
        );
        assert_eq!(
            manifest.decide(&version("1.4.0"), "linux-x86_64"),
            Decision::UpToDate
        );
        assert_eq!(
            manifest.decide(&version("1.4.1"), "linux-x86_64"),
            Decision::UpToDate,
            "no downgrade"
        );
        assert_eq!(
            manifest.decide(&version("2.0.0"), "linux-x86_64"),
            Decision::UpToDate
        );
        assert_eq!(
            manifest.decide(&version("1.10.0"), "linux-x86_64"),
            Decision::UpToDate,
            "1.10 is after 1.4: numbers, not text"
        );
    }

    #[test]
    fn a_rollback_takes_back_only_the_versions_it_names() {
        let mut manifest = manifest("1.4.0");
        manifest.rollback = Some(Rollback {
            from: vec![version("1.5.0")],
        });
        assert!(matches!(
            manifest.decide(&version("1.5.0"), "linux-x86_64"),
            Decision::Install(_)
        ));
        assert_eq!(
            manifest.decide(&version("1.5.1"), "linux-x86_64"),
            Decision::UpToDate,
            "a version it does not name stays"
        );
        assert_eq!(
            manifest.decide(&version("1.4.0"), "linux-x86_64"),
            Decision::UpToDate
        );
        // It is still an ordinary update for what is older.
        assert!(matches!(
            manifest.decide(&version("1.3.0"), "linux-x86_64"),
            Decision::Install(_)
        ));
    }

    #[test]
    fn too_old_a_build_is_told_to_reinstall() {
        let mut manifest = manifest("3.0.0");
        manifest.min_supported = Some(version("2.0.0"));
        assert_eq!(
            manifest.decide(&version("1.9.0"), "linux-x86_64"),
            Decision::Reinstall
        );
        assert!(matches!(
            manifest.decide(&version("2.0.0"), "linux-x86_64"),
            Decision::Install(_)
        ));
    }

    #[test]
    fn a_platform_without_a_file_is_said_so() {
        assert_eq!(
            manifest("1.4.0").decide(&version("1.0.0"), "macos-aarch64"),
            Decision::NoArtifact
        );
        assert_eq!(
            manifest("1.4.0").decide(&version("1.4.0"), "macos-aarch64"),
            Decision::UpToDate
        );
    }

    #[test]
    fn this_platform_has_a_key() {
        let key = platform_key();
        assert!(key.contains('-'));
        if cfg!(any(
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "x86_64"),
        )) {
            assert!(PLATFORMS.contains(&key.as_str()), "{key}");
        }
    }

    #[test]
    fn channels_have_their_own_files() {
        assert_eq!(Channel::Stable.manifest_name(), "latest.json");
        assert_eq!(Channel::Stable.signature_name(), "latest.json.sig");
        assert_eq!(Channel::Beta.manifest_name(), "beta.json");
        assert_eq!(Channel::parse("beta"), Some(Channel::Beta));
        assert_eq!(Channel::parse("nightly"), None);
    }
}
