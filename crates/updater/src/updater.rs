//! The updater while the application runs: it asks the source now and
//! then, downloads and checks a new version in the background, and says
//! where it stands. It never installs anything while the application
//! runs; that is the start's business (see [`crate::launch`]).

use crate::archive::{self, ArchiveError};
use crate::install::{Install, WhyNot};
use crate::manifest::{Channel, Decision, Manifest, ManifestError};
use crate::sign::PublicKey;
use crate::source::{Source, SourceError, Timeouts};
use crate::stage::{self, Layout, Ready, State};
use semver::Version;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// The largest update that is downloaded.
pub const MAX_ARTIFACT_BYTES: u64 = 500 * 1024 * 1024;

/// When the source is asked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Schedule {
    /// How long after the start the first check waits: the start of the
    /// application is never slowed by it.
    pub first: Duration,
    /// How long between checks that went well.
    pub every: Duration,
    /// How long after a first failed check the next one waits. It doubles
    /// with every failure in a row.
    pub retry: Duration,
    /// The longest wait after failures.
    pub retry_max: Duration,
    /// How much of a wait is random, as a fraction (0.2: within a fifth
    /// either way), so that installs do not all ask at the same moment.
    pub jitter: f64,
}

impl Default for Schedule {
    fn default() -> Self {
        Self {
            first: Duration::from_secs(45),
            every: Duration::from_secs(6 * 60 * 60),
            retry: Duration::from_secs(15 * 60),
            retry_max: Duration::from_secs(24 * 60 * 60),
            jitter: 0.2,
        }
    }
}

impl Schedule {
    /// The wait before the next check, after `failures` failed ones in a
    /// row. `random` is a number in `0..1`.
    pub fn wait(&self, failures: u32, random: f64) -> Duration {
        let base = if failures == 0 {
            self.every
        } else {
            let doubled = self
                .retry
                .saturating_mul(1u32 << (failures - 1).min(16))
                .min(self.retry_max);
            // Failing is never a reason to ask more often than usual.
            doubled.max(self.retry)
        };
        self.jittered(base, random)
    }

    /// The wait before the first check.
    pub fn first_wait(&self, random: f64) -> Duration {
        self.jittered(self.first, random)
    }

    fn jittered(&self, base: Duration, random: f64) -> Duration {
        let factor = 1.0 + self.jitter * (random.clamp(0.0, 1.0) * 2.0 - 1.0);
        base.mul_f64(factor.max(0.0))
    }
}

/// A number in `0..1` from the operating system's generator.
fn random() -> f64 {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        return 0.5;
    }
    f64::from(u32::from_le_bytes(bytes)) / (f64::from(u32::MAX) + 1.0)
}

/// What the updater needs to know.
#[derive(Clone, Debug)]
pub struct Config {
    /// The version that is running.
    pub current: Version,
    /// The channel it follows.
    pub channel: Channel,
    /// The platform's key in a manifest.
    pub platform: String,
    /// Where updates come from.
    pub base_url: String,
    /// The keys a manifest must be signed by.
    pub keys: Vec<PublicKey>,
    /// The updater's files.
    pub layout: Layout,
    /// What kind of install this is.
    pub install: Install,
    /// The name of the executable inside an update's archive.
    pub binary_name: String,
    /// The `User-Agent` of its requests: the product, its version, the
    /// platform and the channel.
    pub user_agent: String,
    /// When it asks.
    pub schedule: Schedule,
    /// How long the network may take.
    pub timeouts: Timeouts,
}

/// The `User-Agent` of the updater's requests: all that a request says.
pub fn user_agent(product: &str, version: &Version, platform: &str, channel: Channel) -> String {
    format!("{product}/{version} ({platform}; {})", channel.name())
}

/// Why a new version is there to be had but is not installed by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Manual {
    /// This install does not replace itself.
    Install(WhyNot),
    /// This version is too old to update itself to the new one.
    TooOld,
    /// The release has no build for this platform.
    NoBuild,
}

/// Where the updater stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Updates are turned off in this build, and why.
    Disabled(String),
    /// Nothing has been asked yet.
    Idle,
    /// The source is being asked.
    Checking,
    /// This is the newest version (or the source has nothing to say).
    UpToDate,
    /// A new version is on its way.
    Downloading {
        /// The version.
        version: Version,
        /// Bytes on disk.
        received: u64,
        /// Bytes in all.
        total: u64,
    },
    /// A new version is downloaded and checked: a restart installs it.
    Ready {
        /// The version.
        version: Version,
        /// What is new in it.
        notes: String,
    },
    /// A new version exists, and installing it is up to the user.
    Available {
        /// The version.
        version: Version,
        /// What is new in it.
        notes: String,
        /// Why it is not installed by itself.
        why: Manual,
    },
}

/// What the interface shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// Where the updater stands.
    pub phase: Phase,
    /// When the source was last asked, in seconds since 1970.
    pub last_check: Option<u64>,
    /// Something to say once: an update that was undone.
    pub notice: Option<String>,
    /// Whether it asks by itself.
    pub automatic: bool,
    /// The last check could not reach the source (or what it found did
    /// not verify). Said in the About section only; never a failure.
    pub unreachable: bool,
}

/// Why a check did not end in an answer.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// The source.
    #[error(transparent)]
    Source(#[from] SourceError),
    /// The manifest.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// The file is larger than any update is.
    #[error("the update is {0} bytes, more than is ever downloaded")]
    TooLarge(u64),
    /// The file is not the one the manifest names.
    #[error("the downloaded file does not match the manifest's hash")]
    Hash,
    /// The archive.
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    /// The staging area.
    #[error("the update could not be stored: {0}")]
    Io(#[from] std::io::Error),
}

/// One check, start to end: ask, verify, decide, download, verify, mark
/// ready. `report` hears the phases it goes through.
pub async fn check(
    config: &Config,
    report: &(dyn Fn(Phase) + Send + Sync),
) -> Result<Phase, CheckError> {
    report(Phase::Checking);
    let source = Source::new(&config.base_url, &config.user_agent, config.timeouts)?;
    let Some(fetched) = source.manifest(config.channel).await? else {
        // Nothing published where this build looks (or nothing it is
        // allowed to see): no update, and nothing to complain about.
        return Ok(Phase::UpToDate);
    };
    let manifest = Manifest::verified(
        &fetched.manifest,
        &fetched.signature,
        &config.keys,
        config.channel,
    )?;
    let layout = &config.layout;
    let mut state = State::load(layout);
    let version = manifest.version.clone();
    let notes = manifest.notes.clone();

    let artifact = match manifest.decide(&config.current, &config.platform) {
        Decision::UpToDate => {
            if state.ready.take().is_some() {
                state.save(layout);
            }
            layout.drop_staged_except(None);
            return Ok(Phase::UpToDate);
        }
        // A version that was installed and did not start is not offered
        // again, by any road.
        _ if state.refused.contains(&version) => return Ok(Phase::UpToDate),
        Decision::Reinstall => {
            return Ok(Phase::Available {
                version,
                notes,
                why: Manual::TooOld,
            })
        }
        Decision::NoArtifact => {
            return Ok(Phase::Available {
                version,
                notes,
                why: Manual::NoBuild,
            })
        }
        Decision::Install(artifact) => artifact,
    };
    let target = match &config.install {
        Install::NotifyOnly(why) => {
            // Nothing is kept for an install that will not use it.
            if state.ready.take().is_some() {
                state.save(layout);
            }
            layout.drop_staged_except(None);
            return Ok(Phase::Available {
                version,
                notes,
                why: Manual::Install(*why),
            });
        }
        Install::SelfUpdating(target) => target.clone(),
    };
    if artifact.size > MAX_ARTIFACT_BYTES {
        return Err(CheckError::TooLarge(artifact.size));
    }

    let ready = Phase::Ready {
        version: version.clone(),
        notes: notes.clone(),
    };
    let (file, part) = (layout.artifact_file(&version), layout.part_file(&version));
    layout.drop_staged_except(Some(&version));
    tokio::fs::create_dir_all(layout.staged(&version)).await?;
    // The manifest is kept as it was served, with its signature: the
    // start verifies both again before anything is installed.
    tokio::fs::write(layout.manifest_file(&version), &fetched.manifest).await?;
    tokio::fs::write(layout.signature_file(&version), &fetched.signature).await?;

    let already = state.ready.as_ref().is_some_and(|r| r.version == version)
        && matches_manifest(&file, &artifact.sha256, artifact.size).await;
    if already {
        return Ok(ready);
    }
    if state.ready.take().is_some() {
        state.save(layout);
    }

    let total = artifact.size;
    report(Phase::Downloading {
        version: version.clone(),
        received: 0,
        total,
    });
    let progress = {
        let version = version.clone();
        // A report per megabyte is plenty for a line in a settings page.
        let last = std::sync::atomic::AtomicU64::new(0);
        move |received: u64| {
            let before = last.load(std::sync::atomic::Ordering::Relaxed);
            if received == total || received >= before + 1024 * 1024 {
                last.store(received, std::sync::atomic::Ordering::Relaxed);
                report(Phase::Downloading {
                    version: version.clone(),
                    received,
                    total,
                });
            }
        }
    };
    source.download(&artifact, &part, &progress).await?;

    if !matches_manifest(&part, &artifact.sha256, artifact.size).await {
        // Not a file to go on from: the next attempt starts over.
        let _ = tokio::fs::remove_file(&part).await;
        return Err(CheckError::Hash);
    }
    // It is the file that was signed for. Does it hold what is installed?
    let (kind, name) = (target.kind, config.binary_name.clone());
    let (probe, archive) = (layout.staged(&version).join("probe"), part.clone());
    tokio::task::spawn_blocking(move || -> Result<(), CheckError> {
        crate::install::remove(&probe)?;
        std::fs::create_dir(&probe)?;
        let outcome = archive::unpack(&archive, &probe, kind)
            .and_then(|()| archive::find_payload(&probe, kind, &name).map(|_| ()));
        let _ = crate::install::remove(&probe);
        Ok(outcome?)
    })
    .await
    .map_err(std::io::Error::other)??;

    tokio::fs::rename(&part, &file).await?;
    let mut state = State::load(layout);
    state.ready = Some(Ready {
        version: version.clone(),
        notes,
    });
    state.save(layout);
    tracing::info!(%version, "an update is downloaded and checked; a restart installs it");
    Ok(ready)
}

/// True when the file at `path` has the size and the hash a manifest names.
async fn matches_manifest(path: &std::path::Path, sha256: &str, size: u64) -> bool {
    let (path, sha256) = (path.to_owned(), sha256.to_owned());
    tokio::task::spawn_blocking(move || {
        archive::sha256_file(&path).is_ok_and(|(hash, length)| length == size && hash == sha256)
    })
    .await
    .unwrap_or(false)
}

/// What the start of a run finds on disk: an update that was downloaded
/// before and is still waiting.
pub fn initial_phase(config: &Config) -> Phase {
    let state = State::load(&config.layout);
    match state.ready {
        Some(ready)
            if ready.version != config.current
                && config.layout.artifact_file(&ready.version).exists()
                && matches!(config.install, Install::SelfUpdating(_)) =>
        {
            Phase::Ready {
                version: ready.version,
                notes: ready.notes,
            }
        }
        _ => Phase::Idle,
    }
}

enum Command {
    CheckNow,
    Automatic(bool),
    BaseUrl(String),
    DismissNotice,
}

/// The running updater: a task that checks, and what it last said.
#[derive(Clone)]
pub struct Updater {
    commands: Option<mpsc::UnboundedSender<Command>>,
    state: watch::Receiver<Snapshot>,
    // Keeps the channel open for an updater that has no task.
    _hold: Arc<watch::Sender<Snapshot>>,
}

impl Updater {
    /// An updater that does nothing, and says why.
    pub fn disabled(reason: impl Into<String>) -> Self {
        let (sender, state) = watch::channel(Snapshot {
            phase: Phase::Disabled(reason.into()),
            last_check: None,
            notice: None,
            automatic: false,
            unreachable: false,
        });
        Self {
            commands: None,
            state,
            _hold: Arc::new(sender),
        }
    }

    /// Starts the updater on `runtime`. With `automatic` off it only
    /// checks when asked.
    pub fn start(config: Config, automatic: bool, runtime: &tokio::runtime::Handle) -> Self {
        let stored = State::load(&config.layout);
        let (sender, state) = watch::channel(Snapshot {
            phase: initial_phase(&config),
            last_check: stored.last_check,
            notice: stored.notice,
            automatic,
            unreachable: false,
        });
        let sender = Arc::new(sender);
        let (commands, inbox) = mpsc::unbounded_channel();
        runtime.spawn(run(config, automatic, inbox, sender.clone()));
        Self {
            commands: Some(commands),
            state,
            _hold: sender,
        }
    }

    /// Where the updater stands now.
    pub fn snapshot(&self) -> Snapshot {
        self.state.borrow().clone()
    }

    /// A receiver that wakes whenever the snapshot changes.
    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.state.clone()
    }

    fn send(&self, command: Command) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(command);
        }
    }

    /// Asks the source now, whatever the schedule says.
    pub fn check_now(&self) {
        self.send(Command::CheckNow);
    }

    /// Turns checking by itself on or off.
    pub fn set_automatic(&self, automatic: bool) {
        self.send(Command::Automatic(automatic));
    }

    /// Takes updates from another address from now on.
    pub fn set_base_url(&self, base_url: impl Into<String>) {
        self.send(Command::BaseUrl(base_url.into()));
    }

    /// Forgets the notice: it was read.
    pub fn dismiss_notice(&self) {
        self.send(Command::DismissNotice);
    }
}

async fn run(
    mut config: Config,
    mut automatic: bool,
    mut inbox: mpsc::UnboundedReceiver<Command>,
    state: Arc<watch::Sender<Snapshot>>,
) {
    let mut failures = State::load(&config.layout).failures;
    let mut wait = config.schedule.first_wait(random());
    loop {
        tokio::select! {
            command = inbox.recv() => match command {
                None => return,
                Some(Command::CheckNow) => {}
                Some(Command::Automatic(on)) => {
                    automatic = on;
                    state.send_modify(|snapshot| snapshot.automatic = on);
                    continue;
                }
                Some(Command::BaseUrl(base_url)) => {
                    if base_url != config.base_url {
                        config.base_url = base_url;
                        // Another source owes nothing to this one's failures.
                        failures = 0;
                    }
                    continue;
                }
                Some(Command::DismissNotice) => {
                    let mut stored = State::load(&config.layout);
                    stored.notice = None;
                    stored.save(&config.layout);
                    state.send_modify(|snapshot| snapshot.notice = None);
                    continue;
                }
            },
            () = tokio::time::sleep(wait), if automatic => {}
        }

        let before = state.borrow().phase.clone();
        let report = {
            let state = state.clone();
            move |phase: Phase| {
                state.send_if_modified(|snapshot| {
                    // An update that is ready stays ready on screen while
                    // the source is asked again.
                    let keep = matches!(snapshot.phase, Phase::Ready { .. })
                        && matches!(phase, Phase::Checking);
                    if keep || snapshot.phase == phase {
                        return false;
                    }
                    snapshot.phase = phase;
                    true
                });
            }
        };
        let outcome = check(&config, &report).await;
        let now = stage::now();
        let mut stored = State::load(&config.layout);
        stored.last_check = Some(now);
        let unreachable = outcome.is_err();
        let phase = match outcome {
            Ok(phase) => {
                failures = 0;
                phase
            }
            Err(error) => {
                failures = failures.saturating_add(1);
                let transient =
                    matches!(&error, CheckError::Source(source) if source.is_transient());
                if transient {
                    tracing::debug!(%error, "no update could be looked for; trying again later");
                } else {
                    tracing::warn!(%error, "the update was not taken");
                }
                // Not reachable, not signed, not whole: for the user it
                // all comes to "no update", said quietly. What was ready
                // before is still ready.
                match before {
                    ready @ Phase::Ready { .. } => ready,
                    _ => Phase::UpToDate,
                }
            }
        };
        stored.failures = failures;
        stored.save(&config.layout);
        state.send_modify(|snapshot| {
            snapshot.phase = phase;
            snapshot.last_check = Some(now);
            snapshot.unreachable = unreachable;
        });
        wait = config.schedule.wait(failures, random());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_next_check_waits_longer_after_every_failure() {
        let schedule = Schedule {
            jitter: 0.0,
            ..Schedule::default()
        };
        assert_eq!(schedule.wait(0, 0.5), Duration::from_secs(6 * 60 * 60));
        assert_eq!(schedule.wait(1, 0.5), Duration::from_secs(15 * 60));
        assert_eq!(schedule.wait(2, 0.5), Duration::from_secs(30 * 60));
        assert_eq!(schedule.wait(3, 0.5), Duration::from_secs(60 * 60));
        // It stops growing at a day, however long the server is away.
        assert_eq!(schedule.wait(12, 0.5), Duration::from_secs(24 * 60 * 60));
        assert_eq!(
            schedule.wait(u32::MAX, 0.5),
            Duration::from_secs(24 * 60 * 60)
        );
        let mut before = Duration::ZERO;
        for failures in 1..40 {
            let wait = schedule.wait(failures, 0.5);
            assert!(wait >= before, "{failures}");
            assert!(wait >= schedule.retry, "never a retry storm");
            before = wait;
        }
    }

    #[test]
    fn waits_are_spread_so_that_installs_do_not_ask_together() {
        let schedule = Schedule::default();
        let every = schedule.every.as_secs_f64();
        let (low, high) = (schedule.wait(0, 0.0), schedule.wait(0, 1.0));
        assert!((low.as_secs_f64() - every * 0.8).abs() < 1.0);
        assert!((high.as_secs_f64() - every * 1.2).abs() < 1.0);
        assert_eq!(schedule.wait(0, 0.5), schedule.every);
        // The first check never comes at once.
        assert!(schedule.first_wait(0.0) >= Duration::from_secs(30));
        for _ in 0..64 {
            let number = random();
            assert!((0.0..1.0).contains(&number));
        }
    }

    #[test]
    fn a_request_says_the_version_the_platform_and_the_channel_only() {
        let agent = user_agent(
            "wuapi-inbox",
            &Version::parse("1.2.3").unwrap(),
            "linux-x86_64",
            Channel::Stable,
        );
        assert_eq!(agent, "wuapi-inbox/1.2.3 (linux-x86_64; stable)");
    }

    #[test]
    fn an_updater_without_a_key_does_nothing_and_says_why() {
        let updater = Updater::disabled("no key");
        assert_eq!(updater.snapshot().phase, Phase::Disabled("no key".into()));
        // Asking it is harmless.
        updater.check_now();
        updater.set_automatic(true);
        assert_eq!(updater.snapshot().phase, Phase::Disabled("no key".into()));
    }
}
