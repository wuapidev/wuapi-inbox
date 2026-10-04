//! What happens at the start of the application: a downloaded update is
//! put in place, the new version is started and watched until it is seen
//! to be up, and the version before is put back if it is not.
//!
//! The process that does this is the *old* version. That is what makes
//! going back dependable: a new build that cannot even start (a missing
//! library, a damaged file that still passed every check) is noticed by a
//! process that works.
//!
//! ```text
//! old version starts
//!   └─ an update is ready ─► verify again (signature, hash) ─► unpack next
//!      to the target ─► rename into place, the old one kept ─► start the
//!      new version ─► watch it
//!         ├─ it reaches its window (it removes the mark)   ─► exit
//!         ├─ it exits with an error before that            ─► put the old
//!         │     one back, never install that version again, go on starting
//!         └─ …but it had already upgraded the database     ─► leave it,
//!               say why, exit
//! ```

use crate::archive::{self, PayloadKind};
use crate::install::{Install, Target};
use crate::manifest::{Channel, Decision, Manifest};
use crate::sign::PublicKey;
use crate::stage::{Layout, Pending, State};
use semver::Version;
use std::ffi::OsString;
use std::path::Path;
use std::time::{Duration, Instant};

/// How many times a new version may start without reaching its window
/// before it takes itself out (when nobody is watching it: the watching
/// process usually notices the first time).
pub const MAX_UNCONFIRMED_STARTS: u32 = 3;

/// What the start of the application needs to know.
pub struct Startup {
    /// The version that is starting.
    pub current: Version,
    /// The updater's files.
    pub layout: Layout,
    /// The keys a manifest must be signed by.
    pub keys: Vec<PublicKey>,
    /// The channel this build follows.
    pub channel: Channel,
    /// The platform's key in a manifest.
    pub platform: String,
    /// What kind of install this is.
    pub install: Install,
    /// The name of the executable inside an update's archive.
    pub binary_name: String,
    /// The newest database schema this version opens.
    pub schema: u32,
    /// How long to watch a new version start.
    pub watch: Duration,
}

/// What the application does next.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Go on starting, as the version this is.
    Continue,
    /// This process is done: the new version is running (0), or could not
    /// be started and could not be undone (1).
    Exit(i32),
}

/// A process that was started and can be asked whether it has ended.
pub trait Child {
    /// `None` while it runs; then whether it ended well.
    fn ended(&mut self) -> std::io::Result<Option<bool>>;
}

impl Child for std::process::Child {
    fn ended(&mut self) -> std::io::Result<Option<bool>> {
        Ok(self.try_wait()?.map(|status| status.success()))
    }
}

/// Starts the executable at a path with the arguments this process was
/// started with.
pub type Spawn<'a> = &'a dyn Fn(&Path) -> std::io::Result<Box<dyn Child>>;

/// Why a staged update was not installed.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// The staged files do not verify any more.
    #[error("the downloaded update no longer verifies: {0}")]
    Untrusted(String),
    /// The archive could not be unpacked.
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    /// A file could not be moved.
    #[error("the update could not be put in place: {0}")]
    Io(#[from] std::io::Error),
}

impl Startup {
    /// Runs the start-of-application step. See the module's description.
    pub fn run(&self, spawn: Spawn<'_>) -> Outcome {
        let mut state = State::load(&self.layout);

        if let Some(mut pending) = Pending::load(&self.layout) {
            if pending.version == self.current {
                // This is the new version, and it has not been seen to
                // get as far as its window yet.
                pending.starts += 1;
                let _ = pending.save(&self.layout);
                if pending.starts > MAX_UNCONFIRMED_STARTS {
                    if let Some(outcome) = self.take_itself_out(&pending, &mut state, spawn) {
                        return outcome;
                    }
                }
                return Outcome::Continue;
            }
            // A mark for a version that is not the one starting: the
            // version before was started again. Its mark means nothing now.
            Pending::clear(&self.layout);
        }

        let Install::SelfUpdating(target) = &self.install else {
            return Outcome::Continue;
        };
        let Some(ready) = state.ready.clone() else {
            // Nothing to install: what earlier updates left behind goes.
            target.clean(true);
            return Outcome::Continue;
        };

        if let Err(error) = self.put_in_place(target, &ready.version) {
            tracing::warn!(%error, version = %ready.version, "the update was not installed");
            Pending::clear(&self.layout);
            state.ready = None;
            state.save(&self.layout);
            self.layout.drop_staged_except(None);
            target.clean(false);
            return Outcome::Continue;
        }
        state.ready = None;
        state.save(&self.layout);
        self.layout.drop_staged_except(None);
        tracing::info!(version = %ready.version, "the update is in place; starting it");

        self.watch(target, &ready.version, &mut state, spawn)
    }

    /// Verifies the staged update from the signature down, unpacks it next
    /// to the target and renames it into place.
    fn put_in_place(&self, target: &Target, version: &Version) -> Result<(), ApplyError> {
        let untrusted = |why: String| ApplyError::Untrusted(why);
        let manifest = std::fs::read(self.layout.manifest_file(version))?;
        let signature = std::fs::read_to_string(self.layout.signature_file(version))?;
        let manifest = Manifest::verified(&manifest, &signature, &self.keys, self.channel)
            .map_err(|error| untrusted(error.to_string()))?;
        if &manifest.version != version {
            return Err(untrusted("it is the manifest of another version".into()));
        }
        // The rules are asked again: what was right to download for one
        // version is not installed by another.
        let Decision::Install(artifact) = manifest.decide(&self.current, &self.platform) else {
            return Err(untrusted("it is not an update of this version".into()));
        };
        let file = self.layout.artifact_file(version);
        let (hash, size) = archive::sha256_file(&file)?;
        if size != artifact.size || hash != artifact.sha256 {
            return Err(untrusted(
                "the file is not the one the manifest names".into(),
            ));
        }

        let staging = target.staging();
        crate::install::remove(&staging)?;
        std::fs::create_dir(&staging)?;
        let outcome = (|| {
            archive::unpack(&file, &staging, target.kind)?;
            let payload = archive::find_payload(&staging, target.kind, &self.binary_name)?;
            if target.kind == PayloadKind::Bundle {
                check_code_signature(target, &payload).map_err(untrusted)?;
            }
            Pending {
                version: version.clone(),
                previous: self.current.clone(),
                previous_schema: self.schema,
                schema: None,
                starts: 0,
            }
            .save(&self.layout)?;
            target.replace_with(&payload)?;
            Ok(())
        })();
        let _ = crate::install::remove(&staging);
        outcome
    }

    /// Starts the new version and watches it until it is seen to be up.
    fn watch(
        &self,
        target: &Target,
        version: &Version,
        state: &mut State,
        spawn: Spawn<'_>,
    ) -> Outcome {
        let mut child = match spawn(target.executable()) {
            Ok(child) => child,
            Err(error) => {
                tracing::warn!(%error, "the new version could not be started");
                return self.go_back(target, version, state);
            }
        };
        let deadline = Instant::now() + self.watch;
        loop {
            if Pending::load(&self.layout).is_none() {
                // It reached its window and said so.
                return Outcome::Exit(0);
            }
            match child.ended() {
                Ok(Some(true)) => {
                    // It ran and ended well before it got to say so (it
                    // was closed at once): nothing is wrong with it.
                    Pending::clear(&self.layout);
                    return Outcome::Exit(0);
                }
                Ok(Some(false)) => return self.go_back(target, version, state),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "the new version cannot be watched");
                    return Outcome::Exit(0);
                }
            }
            if Instant::now() >= deadline {
                // Still starting. It is left to itself: the mark stays,
                // and it counts its own starts from here.
                return Outcome::Exit(0);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The new version did not start: the version before comes back,
    /// unless the data has moved on.
    fn go_back(&self, target: &Target, version: &Version, state: &mut State) -> Outcome {
        let pending = Pending::load(&self.layout);
        Pending::clear(&self.layout);
        let previous = &self.current;
        if let Some(Err(ahead)) = pending.as_ref().map(Pending::may_go_back) {
            let notice = format!(
                "Version {version} did not start, and version {previous} was not put back by \
                 itself: {ahead}. Nothing was lost. Install {version} again, or a newer \
                 version."
            );
            tracing::error!("{notice}");
            eprintln!("{notice}");
            state.notice = Some(notice);
            state.save(&self.layout);
            return Outcome::Exit(1);
        }
        match target.restore() {
            Ok(()) => {
                let notice = format!(
                    "Version {version} did not start, so version {previous} was put back. \
                     That update will not be installed again; the next one will."
                );
                tracing::warn!("{notice}");
                if !state.refused.contains(version) {
                    state.refused.push(version.clone());
                }
                state.notice = Some(notice);
                state.save(&self.layout);
                Outcome::Continue
            }
            Err(error) => {
                tracing::error!(%error, "the version before could not be put back");
                eprintln!(
                    "Version {version} did not start and version {previous} could not be put \
                     back ({error}). Install the application again."
                );
                Outcome::Exit(1)
            }
        }
    }

    /// A new version that has started several times without getting to
    /// its window, with nobody watching: it puts the version before back
    /// itself and starts it. `None` when it cannot, and goes on trying to
    /// start instead.
    fn take_itself_out(
        &self,
        pending: &Pending,
        state: &mut State,
        spawn: Spawn<'_>,
    ) -> Option<Outcome> {
        let Install::SelfUpdating(target) = &self.install else {
            return None;
        };
        if let Err(ahead) = pending.may_go_back() {
            tracing::warn!(%ahead, "this version keeps failing to start and cannot be undone");
            return None;
        }
        if let Err(error) = target.restore() {
            tracing::warn!(%error, "this version keeps failing to start and cannot be undone");
            return None;
        }
        Pending::clear(&self.layout);
        let notice = format!(
            "Version {} did not start, so version {} was put back. That update will not be \
             installed again; the next one will.",
            pending.version, pending.previous
        );
        tracing::warn!("{notice}");
        if !state.refused.contains(&pending.version) {
            state.refused.push(pending.version.clone());
        }
        state.ready = None;
        state.notice = Some(notice);
        state.save(&self.layout);
        match spawn(target.executable()) {
            Ok(_) => Some(Outcome::Exit(0)),
            Err(error) => {
                tracing::error!(%error, "the version put back could not be started");
                Some(Outcome::Exit(1))
            }
        }
    }

    /// Tells the updater that the database is about to be opened by a
    /// build that brings it to `self.schema`. From here on, going back to
    /// a version that only opens an older schema is not done by itself.
    pub fn opening_store(&self) {
        if let Some(mut pending) = Pending::load(&self.layout) {
            if pending.version == self.current {
                pending.schema = Some(pending.schema.unwrap_or(0).max(self.schema));
                let _ = pending.save(&self.layout);
            }
        }
    }

    /// Tells the updater that this version is up (its window is open).
    /// Returns `true` when this was the first start after an update. The
    /// version kept for going back is removed.
    pub fn confirm_started(&self) -> bool {
        let first = Pending::load(&self.layout).is_some_and(|p| p.version == self.current);
        if first {
            Pending::clear(&self.layout);
            if let Install::SelfUpdating(target) = &self.install {
                target.clean(true);
            }
            tracing::info!(version = %self.current, "the update started; it is confirmed");
        }
        first
    }
}

/// macOS: a staged bundle must carry a whole seal by the same team as the
/// running one, when the running one is signed by a team.
#[cfg(target_os = "macos")]
fn check_code_signature(target: &Target, staged: &Path) -> Result<(), String> {
    let running = crate::install::code_signature(&target.path).ok().flatten();
    let staged = crate::install::code_signature(staged);
    if running.is_none() {
        tracing::info!("this build is not signed by a team; the update goes by its manifest");
    }
    crate::install::signature_allows(running.as_deref(), &staged)
}

#[cfg(not(target_os = "macos"))]
fn check_code_signature(_: &Target, _: &Path) -> Result<(), String> {
    Ok(())
}

/// Starts `executable` with `args`, detached from this process's
/// terminal input. The usual [`Spawn`].
pub fn spawn_with(
    args: Vec<OsString>,
) -> impl Fn(&Path) -> std::io::Result<Box<dyn Child>> + 'static {
    move |executable: &Path| {
        let child = std::process::Command::new(executable)
            .args(&args)
            .env_remove(crate::WAIT_FOR_PARENT)
            .stdin(std::process::Stdio::null())
            .spawn()?;
        Ok(Box::new(child) as Box<dyn Child>)
    }
}
