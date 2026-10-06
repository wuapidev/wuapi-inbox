//! Updates, as the application sees them.
//!
//! The work is in the `updater` crate: asking for a new version,
//! downloading and checking it, installing it at the next start and
//! undoing it when it does not come up. This module is what ties it to
//! the application: where its files live (`updates/` in the data
//! directory), what the start does ([`Boot`]), what the window shows
//! ([`snapshot`]) and what the window can ask for ([`check_now`],
//! [`restart`], [`set_automatic`], [`set_base_url`]).
//!
//! Nothing here opens a window or blocks one: the window reads the last
//! snapshot, and is redrawn when there is another.

use crate::cli::Options;
use crate::product;
use crate::settings;
use gpui_kit::{App, Global};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;
use updater::{
    Channel, Config, Install, Layout, Manual, Outcome, Phase, Schedule, Snapshot, Startup,
    Timeouts, Updater, Version,
};

/// The channel this build follows.
pub const CHANNEL: Channel = Channel::Stable;

/// The file next to the settings that holds another update source, when
/// one was set in Settings > About.
pub const FILE_NAME: &str = "update.json";

/// How long the start watches a new version come up before it leaves it
/// to itself.
const WATCH: Duration = Duration::from_secs(20);

/// What is said where updates are off because no key is embedded.
const NO_KEY: &str = "this build has no update key embedded";

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    /// The base URL to take updates from, instead of the default.
    base_url: Option<String>,
}

fn load_saved(path: &Path) -> Saved {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_saved(path: &Path, saved: &Saved) -> std::io::Result<()> {
    if saved.base_url.is_none() {
        return match std::fs::remove_file(path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        };
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let draft = path.with_extension("json.tmp");
    std::fs::write(&draft, serde_json::to_vec_pretty(saved)?)?;
    std::fs::rename(&draft, path)
}

/// Whether updates can be taken from `url`: `Err` says why not.
pub fn check_base_url(url: &str) -> Result<(), String> {
    updater::Source::new(url, "check", Timeouts::default())
        .map(|_| ())
        .map_err(|error| match error {
            updater::source::SourceError::BadBase(_, why) => why.to_owned(),
            other => other.to_string(),
        })
}

/// Where an update source came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlFrom {
    /// The project's releases.
    Default,
    /// Settings > About.
    Setting,
    /// `--update-url`: it has the say for this run.
    CommandLine,
}

/// The version of this build.
pub fn current_version() -> Version {
    Version::parse(product::VERSION).expect("the package version is a version")
}

/// The name of the executable, as an update's archive holds it.
fn binary_name() -> String {
    format!("{}{}", product::SLUG, std::env::consts::EXE_SUFFIX)
}

/// Where a new version is downloaded by hand.
pub fn download_page() -> String {
    format!("{}/releases/latest", product::REPOSITORY)
}

/// The arguments a restarted (or updated) instance is started with: the
/// ones this instance got, less those that ask for something once
/// (`--login` would sign the user out again at every restart).
fn carried_over(args: impl Iterator<Item = OsString>) -> Vec<OsString> {
    args.filter(|arg| !matches!(arg.to_str(), Some("--login" | "--welcome")))
        .collect()
}

/// The updater's part in the start of the application: made once the
/// command line is read, used before the store is opened and after the
/// window is.
pub struct Boot {
    startup: Startup,
    args: Vec<OsString>,
    base_url: String,
    url_from: UrlFrom,
    saved_file: PathBuf,
    automatic: bool,
    relocation: Option<updater::Relocation>,
}

impl Boot {
    /// Reads what the start needs. Nothing is changed yet.
    pub fn new(options: &Options) -> Self {
        let data_dir = options.data_dir.clone().unwrap_or_else(product::data_dir);
        let saved_file = data_dir.join(FILE_NAME);
        let saved = load_saved(&saved_file).base_url.filter(|url| {
            let good = check_base_url(url).is_ok();
            if !good {
                tracing::warn!("the saved update source is not usable; using the default");
            }
            good
        });
        let (base_url, url_from) = match (&options.update_url, saved) {
            (Some(url), _) => (url.clone(), UrlFrom::CommandLine),
            (None, Some(url)) => (url, UrlFrom::Setting),
            (None, None) => (updater::DEFAULT_BASE_URL.to_owned(), UrlFrom::Default),
        };
        let install = match std::env::current_exe() {
            Ok(executable) => updater::install::detect(&executable),
            Err(_) => Install::NotifyOnly(updater::WhyNot::ReadOnly),
        };
        // Opened from the disk image or from Downloads, the copy cannot
        // replace itself where it is; in the Applications folder it can.
        let relocation = std::env::current_exe()
            .ok()
            .and_then(|executable| updater::install::relocation(&executable, &install));
        let automatic = settings::Settings::load(&data_dir.join(settings::FILE_NAME)).auto_update;
        Self {
            startup: Startup {
                current: current_version(),
                layout: Layout::in_data_dir(&data_dir),
                keys: updater::embedded_keys(),
                channel: CHANNEL,
                platform: updater::platform_key(),
                install,
                binary_name: binary_name(),
                schema: client_core::SCHEMA_VERSION,
                watch: WATCH,
            },
            args: carried_over(std::env::args_os().skip(1)),
            base_url,
            url_from,
            saved_file,
            automatic,
            relocation,
        }
    }

    /// True when this build can verify an update at all.
    fn enabled(&self) -> bool {
        !self.startup.keys.is_empty()
    }

    /// The first thing the start does: an update that is ready is put in
    /// place and started, and this process is told to leave
    /// ([`Outcome::Exit`]) or to go on as the version it is.
    pub fn at_start(&self) -> Outcome {
        if !self.enabled() {
            return Outcome::Continue;
        }
        let spawn = updater::launch::spawn_with(self.args.clone());
        match self.startup.run(&spawn) {
            // Nothing to install or to watch: an install from the first
            // releases takes the name the application has today.
            Outcome::Continue => self.startup.take_todays_name(&spawn),
            exit => exit,
        }
    }

    /// Before the store is opened: from here on the database may be
    /// migrated, and an update is not undone behind it.
    pub fn opening_store(&self) {
        if self.enabled() {
            self.startup.opening_store();
        }
    }

    /// Once the window is open: this version is up.
    pub fn confirm_started(&self) -> bool {
        self.enabled() && self.startup.confirm_started()
    }

    /// Starts the updater in the background and gives the window its
    /// view of it.
    pub fn start(self, runtime: &tokio::runtime::Handle, cx: &mut App) {
        let Self {
            startup,
            args,
            base_url,
            url_from,
            saved_file,
            automatic,
            relocation,
        } = self;
        let relocation = relocation.map(|plan| {
            let args = args.clone();
            Move {
                folder: plan.folder.clone(),
                run: Arc::new(move || make_move(&plan, args.clone())),
            }
        });
        let updater = if startup.keys.is_empty() {
            tracing::warn!(
                "updates are disabled: {NO_KEY} (replace the placeholder in \
                 updater::sign::EMBEDDED_KEYS)"
            );
            Updater::disabled(NO_KEY)
        } else {
            let config = Config {
                user_agent: updater::updater::user_agent(
                    product::SLUG,
                    &startup.current,
                    &startup.platform,
                    startup.channel,
                ),
                current: startup.current,
                channel: startup.channel,
                platform: startup.platform,
                base_url: base_url.clone(),
                keys: startup.keys,
                layout: startup.layout,
                install: startup.install,
                binary_name: startup.binary_name,
                schedule: Schedule::default(),
                timeouts: Timeouts::default(),
            };
            Updater::start(config, automatic, runtime)
        };
        install(
            cx,
            Center {
                snapshot: updater.snapshot(),
                base_url,
                url_from,
                saved_file: Some(saved_file),
                actions: Rc::new(Running {
                    updater: updater.clone(),
                    args,
                }),
                relocation,
            },
        );
        // The window follows what the updater says.
        let mut changes = updater.subscribe();
        cx.spawn(async move |cx| {
            while changes.changed().await.is_ok() {
                let snapshot = changes.borrow_and_update().clone();
                cx.update(|cx| {
                    if cx.has_global::<Center>() {
                        cx.global_mut::<Center>().snapshot = snapshot;
                        cx.refresh_windows();
                    }
                });
            }
        })
        .detach();
    }
}

/// What the window can ask of the updater. A trait so that the tests of
/// the window bring their own, which restarts nothing.
pub trait Actions {
    /// Ask the source now.
    fn check_now(&self);
    /// Check by itself, or only when asked.
    fn set_automatic(&self, automatic: bool);
    /// Take updates from another address.
    fn set_base_url(&self, base_url: &str);
    /// The notice was read.
    fn dismiss_notice(&self);
    /// Start the application again; the caller then quits.
    fn restart(&self) -> Result<(), String>;
}

struct Running {
    updater: Updater,
    args: Vec<OsString>,
}

impl Actions for Running {
    fn check_now(&self) {
        self.updater.check_now();
    }

    fn set_automatic(&self, automatic: bool) {
        self.updater.set_automatic(automatic);
    }

    fn set_base_url(&self, base_url: &str) {
        self.updater.set_base_url(base_url);
    }

    fn dismiss_notice(&self) {
        self.updater.dismiss_notice();
    }

    fn restart(&self) -> Result<(), String> {
        updater::restart(self.args.clone()).map_err(|error| error.to_string())
    }
}

/// The window's view of the updater.
pub struct Center {
    /// What the updater last said.
    pub snapshot: Snapshot,
    /// Where updates are taken from.
    pub base_url: String,
    /// Who said so.
    pub url_from: UrlFrom,
    /// Where another source is saved. `None` keeps it for the session.
    pub saved_file: Option<PathBuf>,
    /// What can be asked of the updater.
    pub actions: Rc<dyn Actions>,
    /// The move to the Applications folder, for a copy that would update
    /// itself from there. `None` for one that already does, and for one
    /// no move would help.
    pub relocation: Option<Move>,
}

/// A move of this copy to the Applications folder
/// ([`updater::Relocation`]), as the window asks for it.
#[derive(Clone)]
pub struct Move {
    /// The folder it goes to.
    pub folder: PathBuf,
    /// Copies the application there and starts the copy; the caller then
    /// quits. It takes as long as copying the application does: never
    /// called on the thread that draws. `Err` says why nothing was moved,
    /// and then nothing was changed.
    pub run: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

/// Makes the move, and starts the copy with the arguments this instance
/// has: it waits for this one to end, as after "Restart now".
#[cfg(target_os = "macos")]
fn make_move(plan: &updater::Relocation, args: Vec<OsString>) -> Result<(), String> {
    plan.run(&|executable| updater::start_to_take_over(executable, args.clone()))
}

/// Only macOS has a move to make ([`updater::install::relocation_in`]).
#[cfg(not(target_os = "macos"))]
fn make_move(_: &updater::Relocation, _: Vec<OsString>) -> Result<(), String> {
    Err("There is no Applications folder on this system.".to_owned())
}

/// The move to the Applications folder, where it would let this copy
/// update itself.
pub fn relocation(cx: &App) -> Option<Move> {
    cx.try_global::<Center>()
        .and_then(|center| center.relocation.clone())
}

impl Global for Center {}

/// Gives the window its view of the updater.
pub fn install(cx: &mut App, center: Center) {
    cx.set_global(center);
}

/// What the updater last said. Without an updater (a test that brings
/// none), updates are off.
pub fn snapshot(cx: &App) -> Snapshot {
    match cx.try_global::<Center>() {
        Some(center) => center.snapshot.clone(),
        None => Snapshot {
            phase: Phase::Disabled(NO_KEY.to_owned()),
            last_check: None,
            notice: None,
            automatic: false,
            failure: None,
        },
    }
}

/// Where updates are taken from, and who said so.
pub fn base_url(cx: &App) -> (String, UrlFrom) {
    match cx.try_global::<Center>() {
        Some(center) => (center.base_url.clone(), center.url_from),
        None => (updater::DEFAULT_BASE_URL.to_owned(), UrlFrom::Default),
    }
}

fn actions(cx: &App) -> Option<Rc<dyn Actions>> {
    cx.try_global::<Center>()
        .map(|center| center.actions.clone())
}

/// Asks the source now.
pub fn check_now(cx: &mut App) {
    if let Some(actions) = actions(cx) {
        actions.check_now();
    }
}

/// Turns checking by itself on or off, and saves the choice.
pub fn set_automatic(cx: &mut App, automatic: bool) {
    settings::update(cx, |settings| settings.auto_update = automatic);
    if let Some(actions) = actions(cx) {
        actions.set_automatic(automatic);
    }
    if cx.has_global::<Center>() {
        cx.global_mut::<Center>().snapshot.automatic = automatic;
    }
}

/// Takes updates from `url` from now on, and asks it at once. An empty
/// `url` goes back to the project's releases. `Err` says why the address
/// is not taken, and changes nothing.
pub fn set_base_url(cx: &mut App, url: &str) -> Result<(), String> {
    let url = url.trim();
    let (base_url, url_from) = if url.is_empty() || url == updater::DEFAULT_BASE_URL {
        (updater::DEFAULT_BASE_URL.to_owned(), UrlFrom::Default)
    } else {
        check_base_url(url)?;
        (url.to_owned(), UrlFrom::Setting)
    };
    if !cx.has_global::<Center>() {
        return Err("Updates are off in this build.".into());
    }
    let center = cx.global_mut::<Center>();
    if let Some(file) = &center.saved_file {
        let saved = Saved {
            base_url: (url_from == UrlFrom::Setting).then(|| base_url.clone()),
        };
        save_saved(file, &saved).map_err(|error| format!("It could not be saved: {error}"))?;
    }
    center.base_url = base_url.clone();
    center.url_from = url_from;
    let actions = center.actions.clone();
    actions.set_base_url(&base_url);
    actions.check_now();
    Ok(())
}

/// The notice (an update that was undone) was read.
pub fn dismiss_notice(cx: &mut App) {
    if let Some(actions) = actions(cx) {
        actions.dismiss_notice();
    }
    if cx.has_global::<Center>() {
        cx.global_mut::<Center>().snapshot.notice = None;
    }
}

/// Starts the application again and quits this one. With an update
/// ready, the new start installs it.
pub fn restart(cx: &mut App) -> Result<(), String> {
    let actions = actions(cx).ok_or("Updates are off in this build.")?;
    actions.restart()?;
    cx.quit();
    Ok(())
}

/// The version an update is ready for, when a restart would install one.
pub fn ready_version(cx: &App) -> Option<Version> {
    match snapshot(cx).phase {
        Phase::Ready { version, .. } => Some(version),
        _ => None,
    }
}

/// How long ago, in words: "just now", "5 minutes ago", "2 days ago".
pub fn ago(seconds: u64) -> String {
    let count = |n: u64, unit: &str| match n {
        1 => format!("1 {unit} ago"),
        n => format!("{n} {unit}s ago"),
    };
    match seconds {
        0..=59 => "just now".to_owned(),
        60..=3_599 => count(seconds / 60, "minute"),
        3_600..=86_399 => count(seconds / 3_600, "hour"),
        _ => count(seconds / 86_400, "day"),
    }
}

/// When the source was last asked, in words.
pub fn last_checked(snapshot: &Snapshot, now: u64) -> String {
    match snapshot.last_check {
        None => "Never".to_owned(),
        Some(at) => {
            let mut text = ago(now.saturating_sub(at));
            if let Some(first) = text.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            text
        }
    }
}

/// Where the updater stands, in a sentence for the About section.
pub fn status_line(snapshot: &Snapshot) -> String {
    let name = product::PRODUCT_NAME;
    match &snapshot.phase {
        Phase::Disabled(reason) => format!("Updates are off: {reason}."),
        Phase::Idle => "Not checked yet.".to_owned(),
        Phase::Checking => "Checking…".to_owned(),
        // A check that failed says which way, and why: a server that is
        // away and an update that was not taken are looked into
        // differently. (The reason never holds the address.)
        Phase::UpToDate => match &snapshot.failure {
            Some(updater::Failure::Unreachable(reason)) => format!(
                "The update server could not be reached: {reason}. It is asked again later."
            ),
            Some(updater::Failure::Refused(reason)) => {
                format!("The update could not be used: {reason}. It is asked again later.")
            }
            None => format!("{name} is up to date."),
        },
        Phase::Downloading {
            version,
            received,
            total,
        } => {
            let percent = if *total == 0 {
                0
            } else {
                received.saturating_mul(100) / total
            };
            format!("Downloading version {version}: {percent} %")
        }
        Phase::Ready { version, .. } => {
            format!("Version {version} is ready. It is installed when {name} restarts.")
        }
        Phase::Available { version, why, .. } => match why {
            Manual::Install(why) => {
                format!("Version {version} is available. {}", why.explain())
            }
            Manual::TooOld => format!(
                "Version {version} is available. This version is too old to update itself \
                 to it: install it from the download page."
            ),
            Manual::NoBuild => {
                format!("Version {version} is out, without a build for this system yet.")
            }
        },
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;

    /// What a test's window asked of the updater.
    #[derive(Default)]
    pub(crate) struct Recorded {
        pub(crate) calls: RefCell<Vec<String>>,
    }

    impl Actions for Recorded {
        fn check_now(&self) {
            self.calls.borrow_mut().push("check".into());
        }

        fn set_automatic(&self, automatic: bool) {
            self.calls
                .borrow_mut()
                .push(format!("automatic:{automatic}"));
        }

        fn set_base_url(&self, base_url: &str) {
            self.calls.borrow_mut().push(format!("url:{base_url}"));
        }

        fn dismiss_notice(&self) {
            self.calls.borrow_mut().push("dismiss".into());
        }

        fn restart(&self) -> Result<(), String> {
            self.calls.borrow_mut().push("restart".into());
            Ok(())
        }
    }

    pub(crate) fn snapshot_of(phase: Phase) -> Snapshot {
        Snapshot {
            phase,
            last_check: None,
            notice: None,
            automatic: true,
            failure: None,
        }
    }

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    #[test]
    fn the_version_of_the_build_is_a_version() {
        assert_eq!(current_version().to_string(), product::VERSION);
        assert!(binary_name().starts_with("wuapi-inbox"));
        assert!(download_page().starts_with("https://"));
        assert!(download_page().ends_with("/releases/latest"));
    }

    #[test]
    fn the_default_source_is_the_projects_releases_and_carries_no_token() {
        let base = updater::DEFAULT_BASE_URL;
        assert!(base.starts_with(product::REPOSITORY), "{base}");
        assert!(base.ends_with("/releases/latest/download"));
        assert!(!base.contains('?') && !base.contains('@'));
        assert!(check_base_url(base).is_ok());
    }

    #[test]
    fn an_update_source_is_https_or_this_machine() {
        assert!(check_base_url("https://mirror.example.com/inbox").is_ok());
        assert!(check_base_url("http://localhost:8000").is_ok());
        assert_eq!(
            check_base_url("http://mirror.example.com/inbox").unwrap_err(),
            "only https is used"
        );
        assert!(check_base_url("mirror.example.com").is_err());
    }

    #[test]
    fn another_source_is_kept_next_to_the_settings() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("nested").join(FILE_NAME);
        assert_eq!(load_saved(&file).base_url, None);
        let saved = Saved {
            base_url: Some("https://mirror.example.com/inbox".into()),
        };
        save_saved(&file, &saved).unwrap();
        assert_eq!(load_saved(&file).base_url, saved.base_url);
        // Back to the default: the file goes.
        save_saved(&file, &Saved::default()).unwrap();
        assert!(!file.exists());
        save_saved(&file, &Saved::default()).unwrap();
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(load_saved(&file).base_url, None);
    }

    #[test]
    fn the_start_reads_the_source_from_the_command_line_then_the_setting() {
        let dir = tempfile::tempdir().unwrap();
        let mut options = match crate::cli::parse(std::iter::empty()) {
            Ok(crate::cli::Command::Run(options)) => options,
            _ => panic!("options"),
        };
        options.data_dir = Some(dir.path().to_owned());
        let boot = Boot::new(&options);
        assert_eq!(boot.base_url, updater::DEFAULT_BASE_URL);
        assert_eq!(boot.url_from, UrlFrom::Default);
        assert!(boot.automatic);
        assert_eq!(boot.startup.layout.root(), dir.path().join("updates"));
        assert_eq!(boot.startup.schema, client_core::SCHEMA_VERSION);
        // A build made by cargo (this test) never replaces itself.
        assert_eq!(
            boot.startup.install,
            Install::NotifyOnly(updater::WhyNot::Development)
        );

        let saved = Saved {
            base_url: Some("https://mirror.example.com/inbox".into()),
        };
        save_saved(&dir.path().join(FILE_NAME), &saved).unwrap();
        let boot = Boot::new(&options);
        assert_eq!(boot.base_url, "https://mirror.example.com/inbox");
        assert_eq!(boot.url_from, UrlFrom::Setting);

        options.update_url = Some("http://127.0.0.1:9".into());
        let boot = Boot::new(&options);
        assert_eq!(boot.base_url, "http://127.0.0.1:9");
        assert_eq!(boot.url_from, UrlFrom::CommandLine);

        // A saved source that is not usable is not used.
        options.update_url = None;
        std::fs::write(
            dir.path().join(FILE_NAME),
            r#"{"base_url":"http://example.com"}"#,
        )
        .unwrap();
        assert_eq!(Boot::new(&options).url_from, UrlFrom::Default);

        // "Check for updates automatically", turned off.
        let off = settings::Settings {
            auto_update: false,
            ..Default::default()
        };
        off.save(&dir.path().join(settings::FILE_NAME)).unwrap();
        assert!(!Boot::new(&options).automatic);
    }

    #[test]
    fn until_a_key_is_embedded_the_start_does_nothing() {
        // This holds for as long as the placeholder is in EMBEDDED_KEYS;
        // with a real key the start is covered by the updater's own tests.
        let dir = tempfile::tempdir().unwrap();
        let mut options = match crate::cli::parse(std::iter::empty()) {
            Ok(crate::cli::Command::Run(options)) => options,
            _ => panic!("options"),
        };
        options.data_dir = Some(dir.path().to_owned());
        let boot = Boot::new(&options);
        if updater::embedded_keys().is_empty() {
            assert!(!boot.enabled());
            assert_eq!(boot.at_start(), Outcome::Continue);
            boot.opening_store();
            assert!(!boot.confirm_started());
            assert!(!dir.path().join("updates").exists(), "nothing is written");
        } else {
            assert!(boot.enabled());
            assert_eq!(boot.at_start(), Outcome::Continue, "nothing is staged");
        }
    }

    #[test]
    fn a_restart_does_not_repeat_what_was_asked_for_once() {
        let args = [
            "--provider",
            "wuapi",
            "--login",
            "--theme=dark",
            "--welcome",
        ]
        .into_iter()
        .map(OsString::from);
        assert_eq!(
            carried_over(args),
            ["--provider", "wuapi", "--theme=dark"].map(OsString::from)
        );
    }

    #[test]
    fn times_are_said_in_words() {
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(60), "1 minute ago");
        assert_eq!(ago(5 * 60 + 3), "5 minutes ago");
        assert_eq!(ago(3_600), "1 hour ago");
        assert_eq!(ago(7_300), "2 hours ago");
        assert_eq!(ago(86_400), "1 day ago");
        assert_eq!(ago(3 * 86_400 + 5), "3 days ago");
        let mut snapshot = snapshot_of(Phase::UpToDate);
        assert_eq!(last_checked(&snapshot, 1_000), "Never");
        snapshot.last_check = Some(1_000);
        assert_eq!(last_checked(&snapshot, 1_000), "Just now");
        assert_eq!(last_checked(&snapshot, 1_000 + 600), "10 minutes ago");
        assert_eq!(last_checked(&snapshot, 10), "Just now", "a clock set back");
    }

    #[test]
    fn every_state_reads_as_a_sentence() {
        let line = |phase| status_line(&snapshot_of(phase));
        assert!(line(Phase::Disabled(NO_KEY.into())).contains("no update key"));
        assert_eq!(line(Phase::UpToDate), "Wuapi is up to date.");
        assert_eq!(line(Phase::Checking), "Checking…");
        assert_eq!(
            line(Phase::Downloading {
                version: version("1.2.0"),
                received: 5,
                total: 20
            }),
            "Downloading version 1.2.0: 25 %"
        );
        let ready = line(Phase::Ready {
            version: version("1.2.0"),
            notes: String::new(),
        });
        assert!(
            ready.contains("1.2.0") && ready.contains("restarts"),
            "{ready}"
        );
        let packaged = line(Phase::Available {
            version: version("1.2.0"),
            notes: String::new(),
            why: Manual::Install(updater::WhyNot::Packaged),
        });
        assert!(packaged.contains("package manager"), "{packaged}");
    }

    #[test]
    fn a_failed_check_says_which_way_it_failed_and_why() {
        let failed = |failure| {
            status_line(&Snapshot {
                failure: Some(failure),
                ..snapshot_of(Phase::UpToDate)
            })
        };
        // Not reached: the reason, and that it passes by itself.
        assert_eq!(
            failed(updater::Failure::Unreachable(
                "tcp connect error: Connection refused".into()
            )),
            "The update server could not be reached: tcp connect error: Connection refused. \
             It is asked again later."
        );
        // Reached, and what it had was not taken: said as that, never as
        // a server that is away.
        let refused = failed(updater::Failure::Refused(
            "the manifest's signature is not good: no key of this build made it".into(),
        ));
        assert_eq!(
            refused,
            "The update could not be used: the manifest's signature is not good: no key of \
             this build made it. It is asked again later."
        );
        assert!(!refused.contains("reached"));
        // An update that is ready stays what is said, whatever the check
        // after it ran into.
        let ready = status_line(&Snapshot {
            failure: Some(updater::Failure::Unreachable("timed out".into())),
            ..snapshot_of(Phase::Ready {
                version: version("1.2.0"),
                notes: String::new(),
            })
        });
        assert!(ready.contains("is ready"), "{ready}");
    }
}
