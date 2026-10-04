//! Self-update for wuapi Inbox: signed, resumable, and undone when the
//! new version does not start.
//!
//! Pure logic and files; no window. The pieces, in the order an update
//! goes through them:
//!
//! - [`source`]: a base URL with a manifest, its signature and the files.
//! - [`sign`] and [`manifest`]: the manifest is believed only when its
//!   Ed25519 signature by an embedded key is good; then it says which
//!   file is this platform's, how large it is and what its SHA-256 is.
//! - [`updater`]: asks now and then, downloads in the background, checks
//!   the file and marks the update ready ([`stage`]).
//! - [`launch`]: at the next start, verifies everything again, renames
//!   the new version into place ([`install`]), starts it, and puts the
//!   version before back if it does not come up.
//!
//! `docs/RELEASING.md` has the format of the manifest, the release
//! pipeline and what each platform can and cannot do.

pub mod archive;
pub mod install;
pub mod launch;
pub mod manifest;
pub mod sign;
pub mod source;
pub mod stage;
pub mod updater;

pub use archive::PayloadKind;
pub use install::{Install, Target, WhyNot};
pub use launch::{Outcome, Startup};
pub use manifest::{platform_key, Artifact, Channel, Decision, Manifest, Rollback};
pub use semver::Version;
pub use sign::{embedded_keys, PublicKey, SecretKey, EMBEDDED_KEYS, PLACEHOLDER_KEY};
pub use source::{Source, Timeouts};
pub use stage::Layout;
pub use updater::{Config, Manual, Phase, Schedule, Snapshot, Updater};

use std::ffi::OsString;
use std::time::Duration;

/// The base URL updates come from unless another is given: the files of
/// this project's latest GitHub release. Nothing but this constant knows
/// that it is GitHub.
pub const DEFAULT_BASE_URL: &str =
    "https://github.com/wuapidev/wuapi-inbox/releases/latest/download";

/// Set in the environment of a process started by [`restart`]: it waits
/// for the process that started it to end before it does anything.
pub const WAIT_FOR_PARENT: &str = "WUAPI_INBOX_WAIT_FOR_PARENT";

/// Starts this executable again with `args`, to take over once this
/// process has ended. The caller then quits.
///
/// The new process is handed one end of a pipe and waits until the other
/// end closes, which is when this process is gone: two instances never
/// work the same data at once. If an update is ready, the new process is
/// what installs it (see [`launch`]).
pub fn restart(args: Vec<OsString>) -> std::io::Result<()> {
    let executable = std::env::current_exe()?;
    let mut child = std::process::Command::new(executable)
        .args(args)
        .env(WAIT_FOR_PARENT, "1")
        .stdin(std::process::Stdio::piped())
        .spawn()?;
    // Held open, and never written to, for as long as this process lives.
    std::mem::forget(child.stdin.take());
    Ok(())
}

/// In a process started by [`restart`]: waits until the process that
/// started it has ended, or `longest` has passed. Anywhere else it
/// returns at once.
pub fn wait_for_parent(longest: Duration) {
    if std::env::var_os(WAIT_FOR_PARENT).is_none() {
        return;
    }
    let (done, ended) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let mut sink = [0u8; 64];
        let mut input = std::io::stdin().lock();
        // Nothing is ever written: a read returns when the other end
        // closes.
        while matches!(input.read(&mut sink), Ok(read) if read > 0) {}
        let _ = done.send(());
    });
    let _ = ended.recv_timeout(longest);
}
