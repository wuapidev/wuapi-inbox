//! The file a platform's update travels in: a gzipped tar with one
//! directory in it, which holds the binary (Linux, Windows) or the `.app`
//! bundle (macOS).
//!
//! The same code packs it for a release (`cargo xtask package`) and
//! unpacks it in the application, so the two cannot disagree. Unpacking
//! trusts nothing: an entry that would land outside the directory it is
//! unpacked into, a link that points out of it, a device or a hard link,
//! or more data than any release has, stops it.

use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sha2::{Digest as _, Sha256};
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};

/// The most an archive may unpack to.
pub const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// The most entries an archive may have.
pub const MAX_ENTRIES: usize = 50_000;

/// What is installed: one file, or a directory that is an application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayloadKind {
    /// The executable.
    File,
    /// A macOS `.app` bundle.
    Bundle,
}

/// Why an archive was not unpacked.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// Reading or writing failed, or the archive is damaged.
    #[error("the archive could not be unpacked: {0}")]
    Io(#[from] std::io::Error),
    /// An entry is of a kind, or at a place, no release has.
    #[error("the archive has an entry that is not allowed: {0}")]
    Refused(String),
    /// The archive unpacks to more than any release does.
    #[error("the archive is too large")]
    TooLarge,
    /// What was to be installed is not in the archive.
    #[error("the archive does not hold `{0}`")]
    NoPayload(String),
}

/// Packs `source` (a file or a directory) into the gzipped tar `out`, as
/// `<root>/<name of source>`. The same input gives the same bytes: no
/// times, owners or host details go in.
pub fn pack(source: &Path, root: &str, out: &Path) -> std::io::Result<()> {
    pack_all(&[source], root, out)
}

/// As [`pack`], with several files or directories under the one root.
pub fn pack_all(sources: &[&Path], root: &str, out: &Path) -> std::io::Result<()> {
    let file = std::fs::File::create(out)?;
    let mut builder = tar::Builder::new(GzEncoder::new(file, flate2::Compression::best()));
    builder.mode(tar::HeaderMode::Deterministic);
    builder.follow_symlinks(false);
    for source in sources {
        let name = source
            .file_name()
            .ok_or_else(|| std::io::Error::other("the source has no name"))?;
        let inside = Path::new(root).join(name);
        if source.is_dir() {
            builder.append_dir_all(&inside, source)?;
        } else {
            append_file(&mut builder, source, &inside)?;
        }
    }
    let mut file = builder.into_inner()?.finish()?;
    file.flush()?;
    file.sync_all()
}

/// One file, executable when its name says it is a program or the file
/// system says so. (On Windows the file system does not say; what has no
/// extension, or `.exe`, is a program.)
fn append_file<W: std::io::Write>(
    builder: &mut tar::Builder<W>,
    source: &Path,
    inside: &Path,
) -> std::io::Result<()> {
    let bytes = std::fs::read(source)?;
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(if is_program(source) { 0o755 } else { 0o644 });
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    builder.append_data(&mut header, inside, bytes.as_slice())
}

fn is_program(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        match path.extension().and_then(|e| e.to_str()) {
            None => true,
            Some(extension) => extension.eq_ignore_ascii_case("exe"),
        }
    }
}

/// The path of an entry, if every part of it is an ordinary name.
fn plain(path: &Path) -> Option<PathBuf> {
    let mut clean = PathBuf::new();
    for part in path.components() {
        match part {
            Component::Normal(name) => clean.push(name),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}

/// True when a link at `at` (a clean path inside the archive) to `target`
/// stays inside the archive's directory.
fn link_stays_inside(at: &Path, target: &Path) -> bool {
    if target.is_absolute() {
        return false;
    }
    let mut depth = at.components().count() as isize - 1;
    for part in target.components() {
        match part {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => depth -= 1,
            _ => return false,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

/// Unpacks `archive` into the directory `into`, which must exist and
/// should be empty. Symbolic links are taken only for a bundle (a macOS
/// framework is made of them).
pub fn unpack(archive: &Path, into: &Path, kind: PayloadKind) -> Result<(), ArchiveError> {
    let file = std::fs::File::open(archive)?;
    let mut tar = tar::Archive::new(GzDecoder::new(std::io::BufReader::new(file)));
    tar.set_preserve_mtime(false);
    tar.set_overwrite(false);
    let (mut total, mut entries) = (0u64, 0usize);
    for entry in tar.entries()? {
        let mut entry = entry?;
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(ArchiveError::TooLarge);
        }
        let path = entry.path()?.into_owned();
        let refused = || ArchiveError::Refused(path.display().to_string());
        let clean = plain(&path).ok_or_else(refused)?;
        let kind_of = entry.header().entry_type();
        match kind_of {
            tar::EntryType::Regular | tar::EntryType::Directory => {}
            tar::EntryType::Symlink if kind == PayloadKind::Bundle => {
                let target = entry.link_name()?.ok_or_else(refused)?;
                if !link_stays_inside(&clean, &target) {
                    return Err(refused());
                }
            }
            _ => return Err(refused()),
        }
        total = total.saturating_add(entry.size());
        if total > MAX_UNPACKED_BYTES {
            return Err(ArchiveError::TooLarge);
        }
        if !entry.unpack_in(into)? {
            return Err(refused());
        }
    }
    Ok(())
}

/// Finds what is to be installed in an unpacked archive: the file called
/// `name`, or the first `.app` directory, at the top or one directory down.
pub fn find_payload(dir: &Path, kind: PayloadKind, name: &str) -> Result<PathBuf, ArchiveError> {
    let is_payload = |path: &Path| -> bool {
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            return false;
        };
        let Ok(meta) = std::fs::symlink_metadata(path) else {
            return false;
        };
        match kind {
            PayloadKind::File => meta.is_file() && file_name == name,
            PayloadKind::Bundle => meta.is_dir() && file_name.ends_with(".app"),
        }
    };
    let list = |dir: &Path| -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        paths.sort();
        paths
    };
    let top = list(dir);
    if let Some(found) = top.iter().find(|path| is_payload(path)) {
        return Ok(found.clone());
    }
    top.iter()
        .filter(|path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
        .flat_map(|sub| list(sub))
        .find(|path| is_payload(path))
        .ok_or_else(|| {
            ArchiveError::NoPayload(match kind {
                PayloadKind::File => name.to_owned(),
                PayloadKind::Bundle => "an .app bundle".to_owned(),
            })
        })
}

/// The SHA-256 of a file, in lower-case hexadecimal, and its size.
pub fn sha256_file(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((hex(&hasher.finalize()), size))
}

/// Bytes in lower-case hexadecimal.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An archive made by hand, entry by entry.
    fn archive_of(
        dir: &Path,
        entries: impl FnOnce(&mut tar::Builder<GzEncoder<std::fs::File>>),
    ) -> PathBuf {
        let path = dir.join("made.tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let mut builder = tar::Builder::new(GzEncoder::new(file, flate2::Compression::fast()));
        entries(&mut builder);
        builder.into_inner().unwrap().finish().unwrap();
        path
    }

    fn file_entry<W: std::io::Write>(builder: &mut tar::Builder<W>, path: &str, bytes: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        // `append_data` refuses `..`; a hostile archive is written raw.
        let name = path.as_bytes();
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
        header.set_cksum();
        builder.append(&header, bytes).unwrap();
    }

    #[test]
    fn what_is_packed_unpacks_to_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("wuapi-inbox");
        std::fs::write(&binary, b"#!/bin/sh\necho new\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let archive = dir.path().join("a.tar.gz");
        pack(&binary, "wuapi-inbox-1.0.0-linux-x86_64", &archive).unwrap();

        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        unpack(&archive, &out, PayloadKind::File).unwrap();
        let found = find_payload(&out, PayloadKind::File, "wuapi-inbox").unwrap();
        assert_eq!(
            found,
            out.join("wuapi-inbox-1.0.0-linux-x86_64")
                .join("wuapi-inbox")
        );
        assert_eq!(std::fs::read(&found).unwrap(), b"#!/bin/sh\necho new\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&found).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "still a program");
        }
    }

    #[test]
    fn packing_twice_gives_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("wuapi-inbox.exe");
        std::fs::write(&binary, vec![7u8; 4096]).unwrap();
        let (a, b) = (dir.path().join("a.tar.gz"), dir.path().join("b.tar.gz"));
        pack(&binary, "root", &a).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        pack(&binary, "root", &b).unwrap();
        assert_eq!(sha256_file(&a).unwrap(), sha256_file(&b).unwrap());
    }

    #[test]
    fn a_bundle_is_a_directory_and_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("wuapi Inbox.app");
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), "<plist/>").unwrap();
        std::fs::write(bundle.join("Contents/MacOS/wuapi-inbox"), "binary").unwrap();
        let archive = dir.path().join("a.tar.gz");
        pack(&bundle, "wuapi-inbox-1.0.0-macos-aarch64", &archive).unwrap();

        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        unpack(&archive, &out, PayloadKind::Bundle).unwrap();
        let found = find_payload(&out, PayloadKind::Bundle, "").unwrap();
        assert!(found.ends_with("wuapi Inbox.app"));
        assert_eq!(
            std::fs::read_to_string(found.join("Contents/MacOS/wuapi-inbox")).unwrap(),
            "binary"
        );
        // A binary is not found where a bundle is asked for, and the
        // other way round.
        assert!(matches!(
            find_payload(&out, PayloadKind::File, "wuapi-inbox"),
            Err(ArchiveError::NoPayload(_))
        ));
    }

    #[test]
    fn an_entry_that_climbs_out_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let archive = archive_of(dir.path(), |builder| {
            file_entry(builder, "root/../../escaped", b"x");
        });
        let out = dir.path().join("deep").join("out");
        std::fs::create_dir_all(&out).unwrap();
        assert!(matches!(
            unpack(&archive, &out, PayloadKind::File),
            Err(ArchiveError::Refused(_))
        ));
        assert!(!dir.path().join("escaped").exists());
        assert!(!dir.path().join("deep").join("escaped").exists());
    }

    #[test]
    fn links_are_refused_for_a_binary_and_held_inside_for_a_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let link = |target: &'static str| {
            move |builder: &mut tar::Builder<GzEncoder<std::fs::File>>| {
                let mut header = tar::Header::new_gnu();
                header.set_size(0);
                header.set_entry_type(tar::EntryType::Symlink);
                header.set_mode(0o777);
                builder
                    .append_link(&mut header, "App.app/Contents/link", target)
                    .unwrap();
            }
        };
        let out = |name: &str| {
            let out = dir.path().join(name);
            std::fs::create_dir_all(&out).unwrap();
            out
        };
        let inside = archive_of(dir.path(), link("MacOS/binary"));
        assert!(matches!(
            unpack(&inside, &out("a"), PayloadKind::File),
            Err(ArchiveError::Refused(_))
        ));
        if cfg!(unix) {
            unpack(&inside, &out("b"), PayloadKind::Bundle).unwrap();
        }
        for target in ["/etc/passwd", "../../../outside", "../../.."] {
            let hostile = archive_of(dir.path(), link(target));
            assert!(
                matches!(
                    unpack(&hostile, &out("c"), PayloadKind::Bundle),
                    Err(ArchiveError::Refused(_))
                ),
                "{target}"
            );
        }
        assert!(link_stays_inside(
            Path::new("A.app/Contents/Frameworks/F.framework/Versions/Current"),
            Path::new("A")
        ));
        assert!(link_stays_inside(Path::new("a/b/link"), Path::new("../c")));
        assert!(!link_stays_inside(
            Path::new("a/link"),
            Path::new("../../c")
        ));
    }

    #[test]
    fn other_kinds_of_entry_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let archive = archive_of(dir.path(), |builder| {
            let mut header = tar::Header::new_gnu();
            header.set_size(0);
            header.set_entry_type(tar::EntryType::Link);
            builder
                .append_link(&mut header, "root/hard", "root/other")
                .unwrap();
        });
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        assert!(matches!(
            unpack(&archive, &out, PayloadKind::Bundle),
            Err(ArchiveError::Refused(_))
        ));
    }

    #[test]
    fn rubbish_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("rubbish.tar.gz");
        std::fs::write(&archive, b"this is not an archive").unwrap();
        let out = dir.path().join("out");
        std::fs::create_dir(&out).unwrap();
        assert!(unpack(&archive, &out, PayloadKind::File).is_err());
    }

    #[test]
    fn the_hash_of_a_file_is_its_sha256() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc");
        std::fs::write(&path, b"abc").unwrap();
        let (hash, size) = sha256_file(&path).unwrap();
        assert_eq!(
            hash,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(size, 3);
    }
}
