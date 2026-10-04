//! What is replaced by an update, and how.
//!
//! The unit is one file (the executable, on Linux and Windows) or one
//! directory (the `.app` bundle, on macOS). Replacing it is a rename on
//! the same file system, never a write over what is there, so there is no
//! moment at which half of the old version and half of the new one are in
//! place; the version replaced is kept next to it until the new one has
//! started, and is put back if it does not.
//!
//! An install that is somebody else's to change (a system package, a
//! sandbox, a read-only location, Program Files) is never touched: the
//! user is told there is a new version and where to get it.

use crate::archive::PayloadKind;
use std::path::{Path, PathBuf};

/// What an update replaces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// A file or a bundle.
    pub kind: PayloadKind,
    /// Where it is.
    pub path: PathBuf,
    /// The executable to start: the file itself, or the one in the bundle.
    pub executable: PathBuf,
}

/// Why an install does not update itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhyNot {
    /// Installed by the system's package manager.
    Packaged,
    /// Running inside a sandbox that has its own way of updating
    /// (Flatpak, Snap).
    Sandboxed,
    /// Running as an AppImage, which this build does not replace.
    AppImage,
    /// The place it is installed in cannot be written to.
    ReadOnly,
    /// macOS is running it from a randomised read-only copy, because it
    /// was opened from where it was downloaded to.
    Translocated,
    /// macOS: the executable is not inside an `.app` bundle.
    NotABundle,
    /// Windows: the place it is installed in needs administrator rights.
    NeedsElevation,
    /// A build made here with `cargo`, not an installed release.
    Development,
}

impl WhyNot {
    /// The reason, in a sentence for the About section.
    pub fn explain(self) -> &'static str {
        match self {
            Self::Packaged => "This copy was installed by the system's package manager, which updates it.",
            Self::Sandboxed => "This copy runs in a sandbox (Flatpak or Snap), which updates it.",
            Self::AppImage => "This copy is an AppImage, which is replaced by downloading the new one.",
            Self::ReadOnly => "This copy is in a place that cannot be written to, so it cannot replace itself.",
            Self::Translocated => "macOS is running this copy from a temporary read-only place. Move the application to the Applications folder and open it from there.",
            Self::NotABundle => "This copy is not inside an application bundle, so it cannot replace itself.",
            Self::NeedsElevation => "This copy is installed where changes need administrator rights, so it cannot replace itself.",
            Self::Development => "This is a development build.",
        }
    }
}

/// Whether, and what, this install replaces by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// Updates are downloaded and applied.
    SelfUpdating(Target),
    /// Updates are announced, with a link.
    NotifyOnly(WhyNot),
}

/// The operating systems an install is told apart by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    /// Linux.
    Linux,
    /// macOS.
    MacOs,
    /// Windows.
    Windows,
}

impl Os {
    /// The one this build runs on. `None` on anything else, where nothing
    /// is replaced.
    pub fn current() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::MacOs),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }
}

/// The parts of a path as text, so that a path of any system can be
/// looked at on any system (the tests describe Windows installs on Linux).
fn parts(path: &str) -> Vec<&str> {
    path.split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect()
}

fn starts_with(path: &str, prefix: &str, ignore_case: bool) -> bool {
    let (path, prefix) = (parts(path), parts(prefix));
    !prefix.is_empty()
        && path.len() >= prefix.len()
        && path.iter().zip(&prefix).all(|(a, b)| {
            if ignore_case {
                a.eq_ignore_ascii_case(b)
            } else {
                a == b
            }
        })
}

/// What kind of install the executable at `exe` is.
///
/// `env` reads an environment variable and `writable` says whether a
/// directory can be written to; both are parameters so that every case
/// can be tested without the case being true of this machine.
pub fn detect_in(
    exe: &Path,
    os: Os,
    env: &dyn Fn(&str) -> Option<String>,
    writable: &dyn Fn(&Path) -> bool,
) -> Install {
    let text = exe.to_string_lossy();
    let names = parts(&text);
    // `target/debug`, `target/release`, `target/<triple>/release/deps`…
    let from_cargo = names.contains(&"target")
        && names
            .iter()
            .rev()
            .skip(1)
            .take(2)
            .any(|name| matches!(*name, "debug" | "release"));
    if from_cargo {
        return Install::NotifyOnly(WhyNot::Development);
    }
    let Some(dir) = exe.parent().filter(|dir| !dir.as_os_str().is_empty()) else {
        return Install::NotifyOnly(WhyNot::ReadOnly);
    };
    let file = |why_not: WhyNot| {
        if writable(dir) {
            Install::SelfUpdating(Target {
                kind: PayloadKind::File,
                path: exe.to_owned(),
                executable: exe.to_owned(),
            })
        } else {
            Install::NotifyOnly(why_not)
        }
    };
    match os {
        Os::Linux => {
            if env("APPIMAGE").is_some() {
                return Install::NotifyOnly(WhyNot::AppImage);
            }
            if env("FLATPAK_ID").is_some() || env("SNAP").is_some() {
                return Install::NotifyOnly(WhyNot::Sandboxed);
            }
            let packaged = [
                "/usr/bin",
                "/usr/lib",
                "/usr/lib64",
                "/usr/libexec",
                "/usr/share",
                "/usr/games",
                "/bin",
                "/sbin",
                "/nix",
                "/gnu",
                "/snap",
                "/app",
                "/var/lib/flatpak",
            ];
            if packaged
                .iter()
                .any(|prefix| starts_with(&text, prefix, false))
            {
                return Install::NotifyOnly(WhyNot::Packaged);
            }
            file(WhyNot::ReadOnly)
        }
        Os::Windows => {
            let protected = [
                "ProgramFiles",
                "ProgramFiles(x86)",
                "ProgramW6432",
                "SystemRoot",
            ];
            let under_protected = protected
                .iter()
                .filter_map(|name| env(name))
                .any(|prefix| starts_with(&text, &prefix, true))
                || names
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case("WindowsApps"));
            if under_protected {
                return Install::NotifyOnly(WhyNot::NeedsElevation);
            }
            file(WhyNot::NeedsElevation)
        }
        Os::MacOs => {
            if names.contains(&"AppTranslocation") {
                return Install::NotifyOnly(WhyNot::Translocated);
            }
            if names.contains(&"Caskroom") {
                return Install::NotifyOnly(WhyNot::Packaged);
            }
            // <bundle>.app/Contents/MacOS/<executable>
            let bundle = exe
                .parent()
                .filter(|dir| dir.file_name().is_some_and(|name| name == "MacOS"))
                .and_then(Path::parent)
                .filter(|dir| dir.file_name().is_some_and(|name| name == "Contents"))
                .and_then(Path::parent)
                .filter(|dir| dir.extension().is_some_and(|extension| extension == "app"));
            let Some(bundle) = bundle else {
                return Install::NotifyOnly(WhyNot::NotABundle);
            };
            // Mounted disk images are read-only; so is a bundle another
            // user installed.
            let around = bundle.parent().filter(|dir| !dir.as_os_str().is_empty());
            match around {
                Some(around) if writable(around) && writable(bundle) => {
                    Install::SelfUpdating(Target {
                        kind: PayloadKind::Bundle,
                        path: bundle.to_owned(),
                        executable: exe.to_owned(),
                    })
                }
                _ => Install::NotifyOnly(WhyNot::ReadOnly),
            }
        }
    }
}

/// What kind of install the running executable is.
pub fn detect(exe: &Path) -> Install {
    let Some(os) = Os::current() else {
        return Install::NotifyOnly(WhyNot::ReadOnly);
    };
    detect_in(exe, os, &|name| std::env::var(name).ok(), &can_write_in)
}

/// True when a file can be made in `dir`: asked by making one.
pub fn can_write_in(dir: &Path) -> bool {
    let probe = dir.join(format!(".wuapi-inbox-probe-{}", std::process::id()));
    let made = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();
    if made {
        let _ = std::fs::remove_file(&probe);
    }
    made
}

impl Target {
    fn beside(&self, suffix: &str) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.path.with_file_name(format!(".{name}.{suffix}"))
    }

    /// Where the new version is put together before it takes the
    /// target's place: next to it, so that taking its place is a rename.
    pub fn staging(&self) -> PathBuf {
        self.beside("update")
    }

    /// Where the version that was replaced is kept until the new one has
    /// started.
    pub fn backup(&self) -> PathBuf {
        self.beside("previous")
    }

    /// Where a version that did not start is moved out of the way.
    pub fn failed(&self) -> PathBuf {
        self.beside("failed")
    }

    /// The executable of a target at `path`, given this target's.
    fn executable_in(&self, path: &Path) -> PathBuf {
        match self.executable.strip_prefix(&self.path) {
            Ok(inside) if !inside.as_os_str().is_empty() => path.join(inside),
            _ => path.to_owned(),
        }
    }

    /// Puts `new` (a file or directory on the same file system, usually
    /// under [`Target::staging`]) in the target's place and keeps what was
    /// there at [`Target::backup`].
    ///
    /// If it fails, the target is what it was.
    pub fn replace_with(&self, new: &Path) -> std::io::Result<()> {
        let backup = self.backup();
        remove(&backup)?;
        if self.kind == PayloadKind::File {
            make_executable(new)?;
        }
        match self.kind {
            // One step: the name always leads to a whole executable.
            #[cfg(unix)]
            PayloadKind::File => {
                if std::fs::hard_link(&self.path, &backup).is_err() {
                    std::fs::copy(&self.path, &backup)?;
                }
                std::fs::rename(new, &self.path)?;
            }
            #[cfg(not(unix))]
            PayloadKind::File => two_steps(&self.path, new, &backup)?,
            PayloadKind::Bundle => {
                if exchange(new, &self.path).is_ok() {
                    // `new` is now the old bundle.
                    std::fs::rename(new, &backup)?;
                } else {
                    two_steps(&self.path, new, &backup)?;
                }
            }
        }
        sync_dir(&self.path);
        Ok(())
    }

    /// Puts the version kept at [`Target::backup`] back in the target's
    /// place. The version that was there goes to [`Target::failed`] and is
    /// removed when it can be.
    pub fn restore(&self) -> std::io::Result<()> {
        let (backup, failed) = (self.backup(), self.failed());
        if !exists(&backup) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the previous version is not there to go back to",
            ));
        }
        remove(&failed)?;
        #[cfg(unix)]
        {
            if self.kind == PayloadKind::File {
                std::fs::rename(&backup, &self.path)?;
                sync_dir(&self.path);
                return Ok(());
            }
        }
        if exists(&self.path) {
            two_steps(&self.path, &backup, &failed)?;
        } else {
            std::fs::rename(&backup, &self.path)?;
        }
        let _ = remove(&failed);
        sync_dir(&self.path);
        Ok(())
    }

    /// Removes what updating leaves next to the target: the staging
    /// directory, a version that failed, and (with `backup`) the version
    /// kept for going back. What is still in use stays for the next time.
    pub fn clean(&self, backup: bool) {
        let mut leftovers = vec![self.staging(), self.failed()];
        if backup {
            leftovers.push(self.backup());
        }
        for path in leftovers {
            if let Err(error) = remove(&path) {
                tracing::debug!(%error, "an update leftover is still in use");
            }
        }
    }

    /// The executable to start once the target is in place.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// This target, were it at `path`: what is checked before it is moved
    /// into place.
    pub fn staged_executable(&self, path: &Path) -> PathBuf {
        self.executable_in(path)
    }
}

/// True for a file, a directory, or a link that leads nowhere.
fn exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// Removes a file or a directory. Nothing there is not an error.
pub(crate) fn remove(path: &Path) -> std::io::Result<()> {
    let outcome = match std::fs::symlink_metadata(path) {
        Err(error) => Err(error),
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
    };
    match outcome {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// The old one aside, the new one in; the old one back if the new one
/// will not go in. This is also how a running executable is replaced on
/// Windows, which lets a running file be renamed but not written or
/// deleted.
fn two_steps(target: &Path, new: &Path, aside: &Path) -> std::io::Result<()> {
    std::fs::rename(target, aside)?;
    if let Err(error) = std::fs::rename(new, target) {
        if let Err(undo) = std::fs::rename(aside, target) {
            tracing::error!(%undo, "the previous version could not be put back");
        }
        return Err(error);
    }
    Ok(())
}

/// Swaps two paths in one step, where the system can (macOS).
#[cfg(target_os = "macos")]
fn exchange(a: &Path, b: &Path) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = |path: &Path| {
        std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::other("a path with a NUL in it"))
    };
    let (a, b) = (c(a)?, c(b)?);
    // SAFETY: both are valid NUL-terminated strings that outlive the call.
    let answer = unsafe { libc::renamex_np(a.as_ptr(), b.as_ptr(), libc::RENAME_SWAP) };
    if answer == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(target_os = "macos"))]
fn exchange(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no one-step exchange here",
    ))
}

fn make_executable(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = path;
    Ok(())
}

/// Asks the system to write the directory's new entries down, so that a
/// power cut after an update does not undo half of it.
fn sync_dir(target: &Path) {
    #[cfg(unix)]
    if let Some(dir) = target.parent() {
        if let Ok(dir) = std::fs::File::open(dir) {
            let _ = dir.sync_all();
        }
    }
    let _ = target;
}

/// Whether a staged bundle's code signature lets it replace the running
/// one.
///
/// A build signed by a team is only ever replaced by a build signed by
/// the same team whose seal is whole. An unsigned build (one made
/// without the certificate) has nothing to hold the next one to, and the
/// signed manifest is what vouches for it.
pub fn signature_allows(
    running_team: Option<&str>,
    staged: &Result<Option<String>, String>,
) -> Result<(), String> {
    match (running_team, staged) {
        (Some(team), Ok(Some(staged))) if team == staged => Ok(()),
        (Some(team), Ok(Some(staged))) => Err(format!(
            "the update is signed by team {staged}, this application by {team}"
        )),
        (Some(_), Ok(None)) => {
            Err("the update is not signed by a team, and this application is".into())
        }
        (Some(_), Err(error)) => Err(format!("the update's code signature is not good: {error}")),
        (None, _) => Ok(()),
    }
}

/// Asks macOS's `codesign` about a bundle: `Ok(Some(team))` when its seal
/// is whole and a team signed it, `Ok(None)` when it is whole but signed
/// by nobody in particular (ad hoc), `Err` when the seal is broken or
/// there is none.
#[cfg(target_os = "macos")]
pub fn code_signature(bundle: &Path) -> Result<Option<String>, String> {
    use std::process::Command;
    let codesign = Path::new("/usr/bin/codesign");
    if !codesign.exists() {
        return Err("codesign is not available".into());
    }
    let verify = Command::new(codesign)
        .args(["--verify", "--deep", "--strict"])
        .arg(bundle)
        .output()
        .map_err(|error| error.to_string())?;
    if !verify.status.success() {
        return Err(String::from_utf8_lossy(&verify.stderr).trim().to_owned());
    }
    let describe = Command::new(codesign)
        .args(["--display", "--verbose=2"])
        .arg(bundle)
        .output()
        .map_err(|error| error.to_string())?;
    // `codesign` describes on its error stream.
    let text = String::from_utf8_lossy(&describe.stderr).into_owned();
    Ok(team_of(&text))
}

/// The team in what `codesign --display --verbose=2` prints.
pub fn team_of(description: &str) -> Option<String> {
    description
        .lines()
        .find_map(|line| line.trim().strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn detect_plain(path: &str, os: Os, writable: bool) -> Install {
        detect_in(Path::new(path), os, &no_env, &|_| writable)
    }

    #[test]
    fn a_binary_in_the_home_directory_updates_itself() {
        let install = detect_plain("/home/ana/.local/bin/wuapi-inbox", Os::Linux, true);
        let Install::SelfUpdating(target) = install else {
            panic!("{install:?}");
        };
        assert_eq!(target.kind, PayloadKind::File);
        assert_eq!(target.path, Path::new("/home/ana/.local/bin/wuapi-inbox"));
        assert_eq!(
            target.backup(),
            Path::new("/home/ana/.local/bin/.wuapi-inbox.previous")
        );
        assert_eq!(
            target.staging(),
            Path::new("/home/ana/.local/bin/.wuapi-inbox.update")
        );
    }

    #[test]
    fn a_packaged_or_read_only_linux_install_is_left_alone() {
        for path in [
            "/usr/bin/wuapi-inbox",
            "/usr/lib/wuapi-inbox/wuapi-inbox",
            "/nix/store/abc-wuapi-inbox/bin/wuapi-inbox",
            "/snap/wuapi-inbox/12/bin/wuapi-inbox",
            "/app/bin/wuapi-inbox",
        ] {
            assert_eq!(
                detect_plain(path, Os::Linux, true),
                Install::NotifyOnly(WhyNot::Packaged),
                "{path}"
            );
        }
        assert_eq!(
            detect_plain("/opt/wuapi-inbox/wuapi-inbox", Os::Linux, false),
            Install::NotifyOnly(WhyNot::ReadOnly)
        );
        assert!(matches!(
            detect_plain("/opt/wuapi-inbox/wuapi-inbox", Os::Linux, true),
            Install::SelfUpdating(_)
        ));
        // `/usr/local` is the administrator's own: it goes by whether it
        // can be written to.
        assert_eq!(
            detect_plain("/usr/local/bin/wuapi-inbox", Os::Linux, false),
            Install::NotifyOnly(WhyNot::ReadOnly)
        );
        let appimage = |name: &str| (name == "APPIMAGE").then(|| "/tmp/x.AppImage".to_owned());
        assert_eq!(
            detect_in(
                Path::new("/tmp/.mount_x/usr/bin/wuapi-inbox"),
                Os::Linux,
                &appimage,
                &|_| true
            ),
            Install::NotifyOnly(WhyNot::AppImage)
        );
        let flatpak = |name: &str| (name == "FLATPAK_ID").then(|| "dev.wuapi.inbox".to_owned());
        assert_eq!(
            detect_in(
                Path::new("/home/ana/bin/wuapi-inbox"),
                Os::Linux,
                &flatpak,
                &|_| true
            ),
            Install::NotifyOnly(WhyNot::Sandboxed)
        );
    }

    #[test]
    fn a_build_from_cargo_never_replaces_itself() {
        for path in [
            "/home/ana/code/inbox/target/debug/wuapi-inbox",
            "/home/ana/code/inbox/target/release/wuapi-inbox",
            "/home/ana/code/inbox/target/debug/deps/updater-0123456789abcdef",
            "/home/ana/code/inbox/target/x86_64-pc-windows-gnu/release/wuapi-inbox.exe",
            r"C:\src\inbox\target\release\wuapi-inbox.exe",
        ] {
            for os in [Os::Linux, Os::MacOs, Os::Windows] {
                assert_eq!(
                    detect_plain(path, os, true),
                    Install::NotifyOnly(WhyNot::Development),
                    "{path}"
                );
            }
        }
        // The tests themselves run from there.
        let here = std::env::current_exe().unwrap();
        assert_eq!(detect(&here), Install::NotifyOnly(WhyNot::Development));
    }

    #[test]
    fn a_mac_bundle_is_the_unit_and_only_where_it_can_be_replaced() {
        let exe = "/Applications/wuapi Inbox.app/Contents/MacOS/wuapi-inbox";
        let Install::SelfUpdating(target) = detect_plain(exe, Os::MacOs, true) else {
            panic!("a bundle in Applications updates itself");
        };
        assert_eq!(target.kind, PayloadKind::Bundle);
        assert_eq!(target.path, Path::new("/Applications/wuapi Inbox.app"));
        assert_eq!(target.executable(), Path::new(exe));
        assert_eq!(
            target.backup(),
            Path::new("/Applications/.wuapi Inbox.app.previous")
        );
        assert_eq!(
            target.staged_executable(Path::new("/Applications/.wuapi Inbox.app.update/x.app")),
            Path::new("/Applications/.wuapi Inbox.app.update/x.app/Contents/MacOS/wuapi-inbox")
        );

        // From the disk image, or a folder of another user.
        assert_eq!(
            detect_plain(
                "/Volumes/wuapi Inbox/wuapi Inbox.app/Contents/MacOS/wuapi-inbox",
                Os::MacOs,
                false
            ),
            Install::NotifyOnly(WhyNot::ReadOnly)
        );
        // Opened from Downloads: macOS runs a translocated copy.
        assert_eq!(
            detect_plain(
                "/private/var/folders/ab/T/AppTranslocation/1234/d/wuapi Inbox.app/Contents/MacOS/wuapi-inbox",
                Os::MacOs,
                true
            ),
            Install::NotifyOnly(WhyNot::Translocated)
        );
        assert_eq!(
            detect_plain("/Users/ana/bin/wuapi-inbox", Os::MacOs, true),
            Install::NotifyOnly(WhyNot::NotABundle)
        );
        assert_eq!(
            detect_plain(
                "/opt/homebrew/Caskroom/wuapi-inbox/1.0/wuapi Inbox.app/Contents/MacOS/wuapi-inbox",
                Os::MacOs,
                true
            ),
            Install::NotifyOnly(WhyNot::Packaged)
        );
    }

    #[test]
    fn windows_updates_a_per_user_install_and_not_program_files() {
        let env = |name: &str| match name {
            "ProgramFiles" => Some(r"C:\Program Files".to_owned()),
            "ProgramFiles(x86)" => Some(r"C:\Program Files (x86)".to_owned()),
            _ => None,
        };
        let detect = |path: &str, writable: bool| {
            detect_in(Path::new(path), Os::Windows, &env, &|_| writable)
        };
        assert!(matches!(
            detect(
                "C:/Users/ana/AppData/Local/Programs/wuapi Inbox/wuapi-inbox.exe",
                true
            ),
            Install::SelfUpdating(_)
        ));
        for path in [
            "C:/Program Files/wuapi Inbox/wuapi-inbox.exe",
            "c:/program files (x86)/wuapi Inbox/wuapi-inbox.exe",
            "C:/Users/ana/AppData/Local/Microsoft/WindowsApps/wuapi/wuapi-inbox.exe",
        ] {
            // Even when this process happens to be elevated.
            assert_eq!(
                detect(path, true),
                Install::NotifyOnly(WhyNot::NeedsElevation),
                "{path}"
            );
        }
        assert_eq!(
            detect("D:/Tools/wuapi-inbox.exe", false),
            Install::NotifyOnly(WhyNot::NeedsElevation)
        );
    }

    fn file_target(dir: &Path) -> Target {
        let path = dir.join("wuapi-inbox");
        Target {
            kind: PayloadKind::File,
            executable: path.clone(),
            path,
        }
    }

    #[test]
    fn a_file_is_replaced_kept_and_put_back() {
        let dir = tempfile::tempdir().unwrap();
        let target = file_target(dir.path());
        std::fs::write(&target.path, "old").unwrap();
        std::fs::create_dir(target.staging()).unwrap();
        let new = target.staging().join("wuapi-inbox");
        std::fs::write(&new, "new").unwrap();

        target.replace_with(&new).unwrap();
        assert_eq!(std::fs::read_to_string(&target.path).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(target.backup()).unwrap(), "old");
        assert!(!new.exists(), "moved, not copied");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&target.path)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }

        target.restore().unwrap();
        assert_eq!(std::fs::read_to_string(&target.path).unwrap(), "old");
        assert!(!target.backup().exists());
        assert!(!target.failed().exists());

        target.clean(true);
        assert!(!target.staging().exists());
        // Nothing to go back to a second time, and it says so.
        assert!(target.restore().is_err());
        assert_eq!(std::fs::read_to_string(&target.path).unwrap(), "old");
    }

    #[test]
    fn a_replacement_that_is_not_there_leaves_the_target_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let target = file_target(dir.path());
        std::fs::write(&target.path, "old").unwrap();
        assert!(target.replace_with(&dir.path().join("missing")).is_err());
        assert_eq!(std::fs::read_to_string(&target.path).unwrap(), "old");
    }

    #[test]
    fn the_two_step_swap_undoes_itself_when_the_new_one_will_not_go_in() {
        let dir = tempfile::tempdir().unwrap();
        let (target, aside) = (dir.path().join("app.exe"), dir.path().join("app.old"));
        std::fs::write(&target, "old").unwrap();
        assert!(two_steps(&target, &dir.path().join("missing"), &aside).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "old");
        assert!(!aside.exists());

        let new = dir.path().join("app.new");
        std::fs::write(&new, "new").unwrap();
        two_steps(&target, &new, &aside).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(&aside).unwrap(), "old");
    }

    fn bundle(at: &Path, body: &str) {
        std::fs::create_dir_all(at.join("Contents/MacOS")).unwrap();
        std::fs::write(at.join("Contents/MacOS/wuapi-inbox"), body).unwrap();
        std::fs::write(at.join("Contents/Info.plist"), body).unwrap();
    }

    #[test]
    fn a_bundle_is_replaced_whole_kept_and_put_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wuapi Inbox.app");
        let target = Target {
            kind: PayloadKind::Bundle,
            executable: path.join("Contents/MacOS/wuapi-inbox"),
            path,
        };
        bundle(&target.path, "old");
        // A file only the old version has: a bundle is replaced, never
        // merged into.
        std::fs::write(target.path.join("Contents/only-old"), "x").unwrap();
        std::fs::create_dir(target.staging()).unwrap();
        let new = target.staging().join("wuapi Inbox.app");
        bundle(&new, "new");

        target.replace_with(&new).unwrap();
        let read = |path: &Path| std::fs::read_to_string(path).unwrap();
        assert_eq!(read(target.executable()), "new");
        assert!(!target.path.join("Contents/only-old").exists());
        assert_eq!(
            read(&target.backup().join("Contents/MacOS/wuapi-inbox")),
            "old"
        );
        assert!(!new.exists());

        target.restore().unwrap();
        assert_eq!(read(target.executable()), "old");
        assert!(target.path.join("Contents/only-old").exists());
        assert!(!target.backup().exists());
        assert!(!target.failed().exists(), "the failed version is removed");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn two_bundles_change_places_in_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a.app"), dir.path().join("b.app"));
        bundle(&a, "a");
        bundle(&b, "b");
        exchange(&a, &b).unwrap();
        let read =
            |path: &Path| std::fs::read_to_string(path.join("Contents/MacOS/wuapi-inbox")).unwrap();
        assert_eq!(read(&a), "b");
        assert_eq!(read(&b), "a");
    }

    #[test]
    fn a_signed_application_is_only_replaced_by_one_of_the_same_team() {
        let team = Some("ABCDE12345");
        assert!(signature_allows(team, &Ok(Some("ABCDE12345".into()))).is_ok());
        assert!(signature_allows(team, &Ok(Some("ZZZZZ99999".into()))).is_err());
        assert!(signature_allows(team, &Ok(None)).is_err());
        assert!(signature_allows(team, &Err("a sealed resource is missing".into())).is_err());
        // An unsigned build goes by the signed manifest alone.
        assert!(signature_allows(None, &Ok(None)).is_ok());
        assert!(signature_allows(None, &Err("not signed at all".into())).is_ok());
        assert!(signature_allows(None, &Ok(Some("ABCDE12345".into()))).is_ok());
    }

    #[test]
    fn the_team_is_read_from_what_codesign_prints() {
        let text = "Executable=/Applications/x.app/Contents/MacOS/x\n\
                    Identifier=dev.wuapi.inbox\n\
                    Authority=Developer ID Application: Someone (ABCDE12345)\n\
                    TeamIdentifier=ABCDE12345\n";
        assert_eq!(team_of(text).as_deref(), Some("ABCDE12345"));
        assert_eq!(team_of("Signature=adhoc\nTeamIdentifier=not set\n"), None);
        assert_eq!(team_of(""), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn codesign_says_an_unsigned_bundle_is_not_signed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.app");
        bundle(&path, "not a real binary");
        assert!(code_signature(&path).is_err());
    }

    #[test]
    fn the_probe_tells_a_directory_that_can_be_written_to() {
        let dir = tempfile::tempdir().unwrap();
        assert!(can_write_in(dir.path()));
        assert!(!can_write_in(&dir.path().join("not-there")));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
