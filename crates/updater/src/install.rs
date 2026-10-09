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

/// What the application's bundle is called on macOS.
pub const BUNDLE_NAME: &str = "Wuapi.app";

/// What it was called in its first releases ("wuapi Inbox"). Installs of
/// those are still bundles of that name: an update takes their place
/// whatever its own bundle is called, and does not rename them.
pub const FORMER_BUNDLE_NAMES: [&str; 1] = ["wuapi Inbox.app"];

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
            let Some(bundle) = bundle_of(exe) else {
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

/// macOS: the bundle the executable at `exe` is in
/// (`<bundle>.app/Contents/MacOS/<executable>`).
fn bundle_of(exe: &Path) -> Option<&Path> {
    exe.parent()
        .filter(|dir| dir.file_name().is_some_and(|name| name == "MacOS"))
        .and_then(Path::parent)
        .filter(|dir| dir.file_name().is_some_and(|name| name == "Contents"))
        .and_then(Path::parent)
        .filter(|dir| dir.extension().is_some_and(|extension| extension == "app"))
}

/// A move of the running bundle to an Applications folder, for a copy
/// that cannot update itself only because of where it was opened from:
/// the disk image, or the folder it was downloaded to. From there on it
/// replaces itself like any other.
///
/// It is a copy: the bundle that runs stays where it is. (A translocated
/// one does not even run from where the user has it.)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Relocation {
    /// The bundle that runs now.
    pub from: PathBuf,
    /// The Applications folder it goes to: the system's, or the user's
    /// own where the system's cannot be written to.
    pub folder: PathBuf,
    /// The bundle it becomes.
    pub target: Target,
    /// The folder is not there yet (`~/Applications` is made by whoever
    /// first needs it).
    pub makes_folder: bool,
    /// An older copy is in its place, and is swapped for this one.
    pub replaces: bool,
    /// An older copy is in the folder under a former name
    /// ([`FORMER_BUNDLE_NAMES`]): it is set aside for this one, so that
    /// two applications are not left side by side.
    pub supersedes: Option<PathBuf>,
}

/// One file operation of a [`Relocation`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    /// Makes the folder the bundle goes to.
    MakeFolder(PathBuf),
    /// Copies the bundle whole (its links, its permissions, its code
    /// signature) to a place next to where it will be.
    Copy {
        /// The bundle that runs.
        from: PathBuf,
        /// Under [`Target::staging`].
        to: PathBuf,
    },
    /// Takes the mark of a download off the copy: with it, macOS would
    /// run the copy from a temporary place all over again.
    ClearQuarantine(PathBuf),
    /// Moves an older copy under a former name out of the way, to where a
    /// replaced version is kept ([`Target::backup`]): a rename, undone if
    /// the move fails, and never a removal.
    SetAside {
        /// The older copy.
        old: PathBuf,
        /// Where it is kept.
        to: PathBuf,
    },
    /// Puts the copy in its place, in one rename. With `replaces` it
    /// takes the place of an older copy, which is kept aside
    /// ([`Target::replace_with`]) and never deleted first.
    PutInPlace {
        /// The copy.
        new: PathBuf,
        /// An older copy is there.
        replaces: bool,
    },
    /// Starts the executable of the copy; this process then quits.
    Start(PathBuf),
}

impl Relocation {
    /// Where the copy is put together: next to its place, so that taking
    /// it is a rename.
    pub fn copy(&self) -> PathBuf {
        let name = self.target.path.file_name().unwrap_or_default();
        self.target.staging().join(name)
    }

    /// What the move does, in order. A step that fails ends it, and what
    /// was there before is what is there.
    pub fn steps(&self) -> Vec<Step> {
        let copy = self.copy();
        let mut steps = Vec::new();
        if self.makes_folder {
            steps.push(Step::MakeFolder(self.folder.clone()));
        }
        steps.extend([
            Step::Copy {
                from: self.from.clone(),
                to: copy.clone(),
            },
            Step::ClearQuarantine(copy.clone()),
        ]);
        if let Some(old) = &self.supersedes {
            steps.push(Step::SetAside {
                old: old.clone(),
                to: self.target.backup(),
            });
        }
        steps.extend([
            Step::PutInPlace {
                new: copy,
                replaces: self.replaces,
            },
            Step::Start(self.target.executable.clone()),
        ]);
        steps
    }
}

/// The move to offer the install at `exe`, if one would let it update
/// itself.
///
/// Only macOS has one, and only for [`WhyNot::Translocated`] and
/// [`WhyNot::ReadOnly`]: every other reason is true of the copy wherever
/// it is. `env`, `writable` and `exists` are parameters for the same
/// reason as in [`detect_in`].
pub fn relocation_in(
    exe: &Path,
    os: Os,
    install: &Install,
    env: &dyn Fn(&str) -> Option<String>,
    writable: &dyn Fn(&Path) -> bool,
    exists: &dyn Fn(&Path) -> bool,
) -> Option<Relocation> {
    if os != Os::MacOs
        || !matches!(
            install,
            Install::NotifyOnly(WhyNot::Translocated | WhyNot::ReadOnly)
        )
    {
        return None;
    }
    let bundle = bundle_of(exe)?;
    let own_name = bundle.file_name()?;
    // The copy is called as the application is today. A name the user
    // gave the bundle is theirs, and is kept.
    let ours = own_name == BUNDLE_NAME || FORMER_BUNDLE_NAMES.iter().any(|old| own_name == *old);
    let name = if ours {
        std::ffi::OsStr::new(BUNDLE_NAME)
    } else {
        own_name
    };
    let inside = exe.strip_prefix(bundle).ok()?;
    let own = env("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| Path::new(&home).join("Applications"));
    // The system's folder first: it is where an application is looked for.
    [Some(PathBuf::from("/Applications")), own]
        .into_iter()
        .flatten()
        .find_map(|folder| {
            let path = folder.join(name);
            let former: Vec<PathBuf> = FORMER_BUNDLE_NAMES
                .iter()
                .filter(|_| ours)
                .map(|old| folder.join(old))
                .collect();
            // The copy that runs is not moved onto itself, nor set aside.
            if path == bundle || former.iter().any(|old| old == bundle) {
                return None;
            }
            let makes_folder = !exists(&folder);
            let replaces = !makes_folder && exists(&path);
            let supersedes = former
                .into_iter()
                .find(|old| !makes_folder && !replaces && exists(old));
            // What `detect_in` will ask of the copy once it is there. A
            // folder that is not there is made in the one around it; the
            // system's is never made.
            let possible = if makes_folder {
                folder != Path::new("/Applications") && folder.parent().is_some_and(writable)
            } else {
                writable(&folder)
                    && (!replaces || writable(&path))
                    && supersedes.as_ref().is_none_or(|old| writable(old))
            };
            possible.then(|| Relocation {
                from: bundle.to_owned(),
                target: Target {
                    kind: PayloadKind::Bundle,
                    executable: path.join(inside),
                    path,
                },
                folder,
                makes_folder,
                replaces,
                supersedes,
            })
        })
}

/// The move to offer the running executable, if any.
pub fn relocation(exe: &Path, install: &Install) -> Option<Relocation> {
    relocation_in(
        exe,
        Os::current()?,
        install,
        &|name| std::env::var(name).ok(),
        &can_write_in,
        &exists,
    )
}

/// The mark macOS puts on what was downloaded.
#[cfg(target_os = "macos")]
const QUARANTINE: &str = "com.apple.quarantine";

#[cfg(target_os = "macos")]
fn c_path(path: &Path) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt as _;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::other("a path with a NUL in it"))
}

/// Whether `path` itself (not what a link leads to) has the mark of a
/// download.
#[cfg(target_os = "macos")]
fn quarantined(path: &Path) -> bool {
    let (Ok(path), Ok(name)) = (c_path(path), std::ffi::CString::new(QUARANTINE)) else {
        return false;
    };
    // SAFETY: both are valid NUL-terminated strings that outlive the
    // call, and no buffer is given: only the size is asked for.
    let size = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    size >= 0
}

/// Takes the mark of a download off `path` and everything in it. The
/// mark is not part of what a code signature seals.
#[cfg(target_os = "macos")]
fn clear_quarantine(path: &Path) -> std::io::Result<()> {
    let name = std::ffi::CString::new(QUARANTINE).expect("no NUL in the name");
    let c = c_path(path)?;
    // SAFETY: both are valid NUL-terminated strings that outlive the call.
    let answer = unsafe { libc::removexattr(c.as_ptr(), name.as_ptr(), libc::XATTR_NOFOLLOW) };
    if answer != 0 {
        let error = std::io::Error::last_os_error();
        // Not marked is what is wanted.
        if error.raw_os_error() != Some(libc::ENOATTR) {
            return Err(error);
        }
    }
    if std::fs::symlink_metadata(path)?.is_dir() {
        for entry in std::fs::read_dir(path)? {
            clear_quarantine(&entry?.path())?;
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
impl Relocation {
    /// Makes the move, step by step ([`Relocation::steps`]). `start`
    /// starts the copy's executable once it is in place; the caller then
    /// quits.
    ///
    /// `Err` says why it was not made, and then everything is as it was:
    /// the copy is gone, an older one that was replaced is back.
    pub fn run(&self, start: &dyn Fn(&Path) -> std::io::Result<()>) -> Result<(), String> {
        let mut done = Vec::new();
        for step in self.steps() {
            if let Err(why) = self.take(&step, start) {
                self.undo(&done);
                return Err(why);
            }
            done.push(step);
        }
        // The copy was moved out of it.
        let _ = remove(&self.target.staging());
        Ok(())
    }

    fn take(
        &self,
        step: &Step,
        start: &dyn Fn(&Path) -> std::io::Result<()>,
    ) -> Result<(), String> {
        let said = |error: std::io::Error| error.to_string();
        match step {
            Step::MakeFolder(folder) => std::fs::create_dir(folder).map_err(said),
            Step::Copy { from, to } => {
                // What an earlier try left there is this application's.
                remove(&self.target.staging()).map_err(said)?;
                std::fs::create_dir(self.target.staging()).map_err(said)?;
                // `ditto` copies a bundle as the Finder does: links stay
                // links, and permissions, extended attributes and the
                // signature's files come along.
                let copied = std::process::Command::new("/usr/bin/ditto")
                    .arg(from)
                    .arg(to)
                    .output()
                    .map_err(said)?;
                if !copied.status.success() {
                    let why = String::from_utf8_lossy(&copied.stderr).trim().to_owned();
                    return Err(if why.is_empty() {
                        "the application could not be copied".to_owned()
                    } else {
                        why
                    });
                }
                // A signed application stays one: the copy's seal is
                // whole, and the same team's.
                match code_signature(from) {
                    Ok(team) => signature_allows(team.as_deref(), &code_signature(to))
                        .map_err(|why| why.replace("the update", "the copy")),
                    Err(_) => Ok(()),
                }
            }
            Step::ClearQuarantine(copy) => {
                clear_quarantine(copy).map_err(said)?;
                if quarantined(copy) {
                    return Err("the copy is still marked as a download".to_owned());
                }
                Ok(())
            }
            Step::SetAside { old, to } => {
                remove(to).map_err(said)?;
                std::fs::rename(old, to).map_err(said)
            }
            Step::PutInPlace {
                new,
                replaces: true,
            } => self.target.replace_with(new).map_err(said),
            Step::PutInPlace {
                new,
                replaces: false,
            } => std::fs::rename(new, &self.target.path).map_err(said),
            Step::Start(executable) => start(executable).map_err(said),
        }
    }

    /// Takes back the steps that were made, newest first.
    fn undo(&self, done: &[Step]) {
        let _ = remove(&self.target.staging());
        for step in done.iter().rev() {
            let undone = match step {
                Step::PutInPlace { replaces: true, .. } => self.target.restore(),
                Step::PutInPlace {
                    replaces: false, ..
                } => remove(&self.target.path),
                Step::SetAside { old, to } => std::fs::rename(to, old),
                // Only if nothing else is in it by now.
                Step::MakeFolder(folder) => std::fs::remove_dir(folder),
                Step::Copy { .. } | Step::ClearQuarantine(_) | Step::Start(_) => Ok(()),
            };
            if let Err(error) = undone {
                tracing::error!(%error, "a move to the Applications folder was not undone");
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

    /// This bundle under today's name ([`BUNDLE_NAME`]), when it still
    /// has a former one and nothing beside it has today's. `None` for
    /// everything else: a file, a bundle already named so, one the user
    /// named.
    pub fn under_todays_name(&self, exists: &dyn Fn(&Path) -> bool) -> Option<Target> {
        let name = self.path.file_name()?;
        let former = FORMER_BUNDLE_NAMES.iter().any(|old| name == *old);
        if self.kind != PayloadKind::Bundle || !former {
            return None;
        }
        let path = self.path.with_file_name(BUNDLE_NAME);
        (!exists(&path)).then(|| Target {
            kind: self.kind,
            executable: self.executable_in(&path),
            path,
        })
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
        let exe = "/Applications/Wuapi.app/Contents/MacOS/wuapi-inbox";
        let Install::SelfUpdating(target) = detect_plain(exe, Os::MacOs, true) else {
            panic!("a bundle in Applications updates itself");
        };
        assert_eq!(target.kind, PayloadKind::Bundle);
        assert_eq!(target.path, Path::new("/Applications/Wuapi.app"));
        assert_eq!(target.executable(), Path::new(exe));
        assert_eq!(
            target.backup(),
            Path::new("/Applications/.Wuapi.app.previous")
        );
        assert_eq!(
            target.staged_executable(Path::new("/Applications/.Wuapi.app.update/x.app")),
            Path::new("/Applications/.Wuapi.app.update/x.app/Contents/MacOS/wuapi-inbox")
        );

        // From the disk image, or a folder of another user.
        assert_eq!(
            detect_plain(
                "/Volumes/Wuapi/Wuapi.app/Contents/MacOS/wuapi-inbox",
                Os::MacOs,
                false
            ),
            Install::NotifyOnly(WhyNot::ReadOnly)
        );
        // Opened from Downloads: macOS runs a translocated copy.
        assert_eq!(
            detect_plain(
                "/private/var/folders/ab/T/AppTranslocation/1234/d/Wuapi.app/Contents/MacOS/wuapi-inbox",
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
                "/opt/homebrew/Caskroom/wuapi-inbox/1.0/Wuapi.app/Contents/MacOS/wuapi-inbox",
                Os::MacOs,
                true
            ),
            Install::NotifyOnly(WhyNot::Packaged)
        );
    }

    const FROM_DOWNLOADS: &str =
        "/private/var/folders/ab/T/AppTranslocation/1234/d/Wuapi.app/Contents/MacOS/wuapi-inbox";
    const FROM_THE_IMAGE: &str = "/Volumes/Wuapi/Wuapi.app/Contents/MacOS/wuapi-inbox";

    fn home_of_ana(name: &str) -> Option<String> {
        (name == "HOME").then(|| "/Users/ana".to_owned())
    }

    /// The move offered to the copy at `exe`, on a Mac where the paths in
    /// `writable` can be written to and those in `there` exist.
    fn relocation(exe: &str, writable: &[&str], there: &[&str]) -> Option<Relocation> {
        let exe = Path::new(exe);
        let can_write = |path: &Path| writable.iter().any(|known| Path::new(known) == path);
        let exists = |path: &Path| there.iter().any(|known| Path::new(known) == path);
        let install = detect_in(exe, Os::MacOs, &home_of_ana, &can_write);
        relocation_in(exe, Os::MacOs, &install, &home_of_ana, &can_write, &exists)
    }

    #[test]
    fn a_copy_opened_from_downloads_or_the_image_is_offered_the_applications_folder() {
        for exe in [FROM_DOWNLOADS, FROM_THE_IMAGE] {
            let plan = relocation(exe, &["/Applications"], &["/Applications"])
                .unwrap_or_else(|| panic!("{exe} is not offered a move"));
            // The bundle that runs is what is copied, from where it runs:
            // for a translocated copy that is not where it was opened
            // from, and nothing looks for that place.
            assert_eq!(
                plan.from,
                Path::new(exe).ancestors().nth(3).unwrap(),
                "{exe}"
            );
            assert_eq!(plan.folder, Path::new("/Applications"));
            assert_eq!(plan.target.kind, PayloadKind::Bundle);
            assert_eq!(plan.target.path, Path::new("/Applications/Wuapi.app"));
            assert_eq!(
                plan.target.executable(),
                Path::new("/Applications/Wuapi.app/Contents/MacOS/wuapi-inbox")
            );
            assert!(!plan.replaces && !plan.makes_folder);
            // Where it lands, the copy updates itself.
            assert!(matches!(
                detect_plain(&plan.target.executable().to_string_lossy(), Os::MacOs, true),
                Install::SelfUpdating(target) if target == plan.target
            ));
        }
    }

    #[test]
    fn the_move_is_a_copy_made_aside_then_put_in_place_then_started() {
        let plan = relocation(FROM_DOWNLOADS, &["/Applications"], &["/Applications"]).unwrap();
        let aside = Path::new("/Applications/.Wuapi.app.update/Wuapi.app");
        assert_eq!(
            plan.steps(),
            [
                Step::Copy {
                    from: plan.from.clone(),
                    to: aside.to_owned(),
                },
                Step::ClearQuarantine(aside.to_owned()),
                Step::PutInPlace {
                    new: aside.to_owned(),
                    replaces: false,
                },
                Step::Start(
                    Path::new("/Applications/Wuapi.app/Contents/MacOS/wuapi-inbox").to_owned()
                ),
            ]
        );
        // Nothing in it removes or moves the copy that runs.
        assert!(plan.steps().iter().all(|step| match step {
            Step::Copy { to, .. } => !to.starts_with(&plan.from),
            Step::MakeFolder(path) | Step::ClearQuarantine(path) | Step::Start(path) =>
                !path.starts_with(&plan.from),
            Step::PutInPlace { new, .. } => !new.starts_with(&plan.from),
            Step::SetAside { old, to } =>
                !old.starts_with(&plan.from) && !to.starts_with(&plan.from),
        }));
    }

    #[test]
    fn an_older_copy_in_the_applications_folder_is_swapped_never_deleted_first() {
        let plan = relocation(
            FROM_THE_IMAGE,
            &["/Applications", "/Applications/Wuapi.app"],
            &["/Applications", "/Applications/Wuapi.app"],
        )
        .unwrap();
        assert!(plan.replaces);
        assert!(plan.steps().contains(&Step::PutInPlace {
            new: Path::new("/Applications/.Wuapi.app.update/Wuapi.app").to_owned(),
            replaces: true,
        }));
        // An older copy that is somebody else's is left alone: the move
        // goes to the user's own folder.
        let plan = relocation(
            FROM_THE_IMAGE,
            &["/Applications", "/Users/ana"],
            &["/Applications", "/Applications/Wuapi.app"],
        )
        .unwrap();
        assert_eq!(plan.folder, Path::new("/Users/ana/Applications"));
    }

    const OLD_IN_APPLICATIONS: &str = "/Applications/wuapi Inbox.app";

    #[test]
    fn a_copy_under_the_former_name_becomes_the_bundle_of_todays_name() {
        // Whatever the copy that runs is called, the one it becomes is
        // called as the application is today.
        for exe in [
            FROM_THE_IMAGE,
            "/Volumes/wuapi Inbox/wuapi Inbox.app/Contents/MacOS/wuapi-inbox",
        ] {
            let plan = relocation(exe, &["/Applications"], &["/Applications"]).unwrap();
            assert_eq!(
                plan.target.path,
                Path::new("/Applications/Wuapi.app"),
                "{exe}"
            );
            assert_eq!(
                plan.target.executable(),
                Path::new("/Applications/Wuapi.app/Contents/MacOS/wuapi-inbox")
            );
            assert_eq!(plan.supersedes, None);
        }
        // A name somebody gave it is theirs: it is kept.
        let plan = relocation(
            "/Volumes/x/Work chat.app/Contents/MacOS/wuapi-inbox",
            &["/Applications"],
            &["/Applications", OLD_IN_APPLICATIONS],
        )
        .unwrap();
        assert_eq!(plan.target.path, Path::new("/Applications/Work chat.app"));
        assert_eq!(plan.supersedes, None);
    }

    #[test]
    fn an_older_copy_under_the_former_name_is_set_aside_not_left_beside() {
        let plan = relocation(
            FROM_THE_IMAGE,
            &["/Applications", OLD_IN_APPLICATIONS],
            &["/Applications", OLD_IN_APPLICATIONS],
        )
        .unwrap();
        assert_eq!(plan.target.path, Path::new("/Applications/Wuapi.app"));
        assert_eq!(
            plan.supersedes.as_deref(),
            Some(Path::new(OLD_IN_APPLICATIONS))
        );
        assert!(!plan.replaces);
        // Aside first, by a rename, then the new one in: never deleted.
        let aside = Path::new("/Applications/.Wuapi.app.update/Wuapi.app");
        assert_eq!(
            plan.steps()[2..4],
            [
                Step::SetAside {
                    old: Path::new(OLD_IN_APPLICATIONS).to_owned(),
                    to: Path::new("/Applications/.Wuapi.app.previous").to_owned(),
                },
                Step::PutInPlace {
                    new: aside.to_owned(),
                    replaces: false,
                },
            ]
        );
        // One that is somebody else's is left alone, as any older copy.
        let plan = relocation(
            FROM_THE_IMAGE,
            &["/Applications", "/Users/ana"],
            &["/Applications", OLD_IN_APPLICATIONS],
        )
        .unwrap();
        assert_eq!(plan.folder, Path::new("/Users/ana/Applications"));
        assert_eq!(plan.supersedes, None);
        // The copy that runs is never the one set aside.
        let plan = relocation(
            "/Applications/wuapi Inbox.app/Contents/MacOS/wuapi-inbox",
            &["/Users/ana"],
            &["/Applications", OLD_IN_APPLICATIONS],
        )
        .unwrap();
        assert_eq!(
            plan.target.path,
            Path::new("/Users/ana/Applications/Wuapi.app")
        );
        assert_eq!(plan.supersedes, None);
    }

    #[test]
    fn an_install_under_the_former_name_takes_todays_only_where_it_is_free() {
        let old = Target {
            kind: PayloadKind::Bundle,
            path: OLD_IN_APPLICATIONS.into(),
            executable: Path::new(OLD_IN_APPLICATIONS).join("Contents/MacOS/wuapi-inbox"),
        };
        let renamed = old.under_todays_name(&|_| false).expect("a new name");
        assert_eq!(renamed.path, Path::new("/Applications/Wuapi.app"));
        assert_eq!(
            renamed.executable(),
            Path::new("/Applications/Wuapi.app/Contents/MacOS/wuapi-inbox")
        );
        // Something has the name already: nothing is renamed over it.
        assert_eq!(
            old.under_todays_name(&|path| path == Path::new("/Applications/Wuapi.app")),
            None
        );
        // Already named so, named by the user, or not a bundle: left.
        assert_eq!(renamed.under_todays_name(&|_| false), None);
        let theirs = Target {
            path: "/Applications/Work chat.app".into(),
            ..old.clone()
        };
        assert_eq!(theirs.under_todays_name(&|_| false), None);
        let file = Target {
            kind: PayloadKind::File,
            ..old
        };
        assert_eq!(file.under_todays_name(&|_| false), None);
    }

    #[test]
    fn without_the_right_to_write_to_applications_the_users_own_folder_is_taken() {
        // There already.
        let plan = relocation(
            FROM_DOWNLOADS,
            &["/Users/ana/Applications"],
            &["/Applications", "/Users/ana/Applications"],
        )
        .unwrap();
        assert_eq!(
            plan.target.path,
            Path::new("/Users/ana/Applications/Wuapi.app")
        );
        assert!(!plan.makes_folder);
        // Not there yet: it is made first.
        let plan = relocation(FROM_DOWNLOADS, &["/Users/ana"], &["/Applications"]).unwrap();
        assert!(plan.makes_folder);
        assert_eq!(
            plan.steps().first(),
            Some(&Step::MakeFolder(
                Path::new("/Users/ana/Applications").to_owned()
            ))
        );
        // Nowhere to put it: nothing is offered.
        assert_eq!(relocation(FROM_DOWNLOADS, &[], &["/Applications"]), None);
        // A copy of another user in Applications goes to this user's.
        let plan = relocation(
            "/Applications/Wuapi.app/Contents/MacOS/wuapi-inbox",
            &["/Users/ana"],
            &["/Applications", "/Applications/Wuapi.app"],
        )
        .unwrap();
        assert_eq!(plan.folder, Path::new("/Users/ana/Applications"));
        // And one that is already in the only folder there is stays.
        assert_eq!(
            relocation(
                "/Users/ana/Applications/Wuapi.app/Contents/MacOS/wuapi-inbox",
                &[],
                &["/Applications", "/Users/ana/Applications"],
            ),
            None
        );
    }

    #[test]
    fn what_cannot_be_helped_by_a_move_is_not_offered_one() {
        let anywhere = |_: &Path| true;
        let mac = |exe: &str| {
            let exe = Path::new(exe);
            let install = detect_in(exe, Os::MacOs, &home_of_ana, &anywhere);
            relocation_in(exe, Os::MacOs, &install, &home_of_ana, &anywhere, &anywhere)
        };
        // Updates itself already; Homebrew's; a build made here; no bundle.
        for exe in [
            "/Applications/Wuapi.app/Contents/MacOS/wuapi-inbox",
            "/opt/homebrew/Caskroom/wuapi-inbox/1.0/Wuapi.app/Contents/MacOS/wuapi-inbox",
            "/Users/ana/code/inbox/target/release/wuapi-inbox",
            "/Users/ana/bin/wuapi-inbox",
        ] {
            assert_eq!(mac(exe), None, "{exe}");
        }
        // The other systems have no Applications folder to move to.
        for (os, exe) in [
            (Os::Linux, "/opt/wuapi-inbox/wuapi-inbox"),
            (Os::Windows, "C:/Program Files/Wuapi/wuapi-inbox.exe"),
        ] {
            for why in [WhyNot::ReadOnly, WhyNot::Translocated] {
                assert_eq!(
                    relocation_in(
                        Path::new(exe),
                        os,
                        &Install::NotifyOnly(why),
                        &home_of_ana,
                        &anywhere,
                        &anywhere
                    ),
                    None
                );
            }
        }
        // Every other reason stays what it is, wherever the bundle is.
        for why in [
            WhyNot::Packaged,
            WhyNot::Sandboxed,
            WhyNot::AppImage,
            WhyNot::NotABundle,
            WhyNot::NeedsElevation,
            WhyNot::Development,
        ] {
            assert_eq!(
                relocation_in(
                    Path::new(FROM_THE_IMAGE),
                    Os::MacOs,
                    &Install::NotifyOnly(why),
                    &home_of_ana,
                    &anywhere,
                    &anywhere
                ),
                None,
                "{why:?}"
            );
        }
    }

    /// A bundle as a download leaves it: an executable, a link inside,
    /// and the mark of a download on all of it.
    #[cfg(target_os = "macos")]
    fn downloaded_bundle(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let bundle = dir.join("Wuapi.app");
        let programs = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&programs).unwrap();
        std::fs::create_dir_all(bundle.join("Contents/Frameworks")).unwrap();
        let program = programs.join("wuapi-inbox");
        std::fs::write(&program, body).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(
            "../MacOS/wuapi-inbox",
            bundle.join("Contents/Frameworks/Current"),
        )
        .unwrap();
        for path in [&bundle, &program] {
            let marked = std::process::Command::new("/usr/bin/xattr")
                .args(["-w", QUARANTINE, "0081;00000000;Safari;"])
                .arg(path)
                .status()
                .unwrap();
            assert!(marked.success());
            assert!(quarantined(path));
        }
        bundle
    }

    #[cfg(target_os = "macos")]
    fn relocation_to(from: &Path, folder: &Path) -> Relocation {
        let path = folder.join("Wuapi.app");
        Relocation {
            from: from.to_owned(),
            target: Target {
                kind: PayloadKind::Bundle,
                executable: path.join("Contents/MacOS/wuapi-inbox"),
                path: path.clone(),
            },
            makes_folder: !folder.exists(),
            replaces: path.exists(),
            supersedes: None,
            folder: folder.to_owned(),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_move_copies_the_bundle_whole_and_starts_the_copy() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let from = downloaded_bundle(&dir.path().join("Downloads"), "new");
        let folder = dir.path().join("home/Applications");
        std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
        let plan = relocation_to(&from, &folder);
        assert!(plan.makes_folder && !plan.replaces);

        let started = std::cell::RefCell::new(Vec::new());
        plan.run(&|executable| {
            started.borrow_mut().push(executable.to_owned());
            Ok(())
        })
        .unwrap();

        let program = plan.target.executable();
        assert_eq!(*started.borrow(), [program.to_owned()]);
        assert_eq!(std::fs::read_to_string(program).unwrap(), "new");
        let mode = std::fs::metadata(program).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
        // A link is still a link, to the same place.
        let link = plan.target.path.join("Contents/Frameworks/Current");
        assert_eq!(
            std::fs::read_link(link).unwrap(),
            Path::new("../MacOS/wuapi-inbox")
        );
        // The mark of a download is gone from the copy, and nothing is
        // left next to it.
        assert!(!quarantined(&plan.target.path) && !quarantined(program));
        assert!(!plan.target.staging().exists());
        // The copy that ran is as it was.
        assert!(quarantined(&from));
        assert_eq!(
            std::fs::read_to_string(from.join("Contents/MacOS/wuapi-inbox")).unwrap(),
            "new"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_move_keeps_an_older_copy_aside_and_undoes_itself_when_it_fails() {
        let dir = tempfile::tempdir().unwrap();
        let from = downloaded_bundle(&dir.path().join("Downloads"), "new");
        let folder = dir.path().join("Applications");
        let older = downloaded_bundle(&folder, "old");
        let plan = relocation_to(&from, &folder);
        assert!(plan.replaces && !plan.makes_folder);
        let program = plan.target.executable().to_owned();

        // The copy will not start: the older one is back, and whole.
        let said = plan
            .run(&|_| Err(std::io::Error::other("it will not start")))
            .unwrap_err();
        assert!(said.contains("it will not start"), "{said}");
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "old");
        assert!(!plan.target.staging().exists() && !plan.target.backup().exists());

        // A bundle that is not there: nothing was touched.
        let missing = relocation_to(&dir.path().join("gone.app"), &folder);
        assert!(missing.run(&|_| Ok(())).is_err());
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "old");
        assert!(!missing.target.staging().exists());

        // It starts: the new one is in place, the older one kept aside
        // for the next start to remove, as after an update.
        plan.run(&|_| Ok(())).unwrap();
        assert_eq!(older, plan.target.path);
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(plan.target.backup().join("Contents/MacOS/wuapi-inbox"))
                .unwrap(),
            "old"
        );
        plan.target.clean(true);
        assert!(!plan.target.backup().exists());

        // An older copy under the former name: set aside whole, and back
        // when the move fails.
        plan.target.clean(true);
        std::fs::rename(&plan.target.path, folder.join("wuapi Inbox.app")).unwrap();
        let former = folder.join("wuapi Inbox.app");
        let renaming = Relocation {
            replaces: false,
            supersedes: Some(former.clone()),
            ..plan.clone()
        };
        assert!(renaming.run(&|_| Err(std::io::Error::other("no"))).is_err());
        assert!(former.join("Contents/MacOS/wuapi-inbox").exists());
        assert!(!renaming.target.path.exists() && !renaming.target.backup().exists());
        renaming.run(&|_| Ok(())).unwrap();
        assert!(!former.exists(), "not left beside the new one");
        assert_eq!(std::fs::read_to_string(&program).unwrap(), "new");
        assert!(renaming
            .target
            .backup()
            .join("Contents/MacOS/wuapi-inbox")
            .exists());

        // A folder made for a move that failed is not left behind.
        let folder = dir.path().join("home/Applications");
        std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
        let plan = relocation_to(&from, &folder);
        assert!(plan.run(&|_| Err(std::io::Error::other("no"))).is_err());
        assert!(!folder.exists());
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
                "C:/Users/ana/AppData/Local/Programs/Wuapi/wuapi-inbox.exe",
                true
            ),
            Install::SelfUpdating(_)
        ));
        for path in [
            "C:/Program Files/Wuapi/wuapi-inbox.exe",
            "c:/program files (x86)/Wuapi/wuapi-inbox.exe",
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
        let path = dir.path().join("Wuapi.app");
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
        let new = target.staging().join("Wuapi.app");
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
