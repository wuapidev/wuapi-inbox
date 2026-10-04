//! The whole road of an update, against a local server and a temporary
//! directory that stands for an install: check, download (and resume),
//! verify, ready, install at the next start, the first start of the new
//! version, and going back when it does not come up.
//!
//! Nothing here touches a real install: every "installed" file is made in
//! a temporary directory by the test.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use updater::archive::{self, PayloadKind};
use updater::launch::{Child, Spawn};
use updater::manifest::FORMAT;
use updater::sign;
use updater::stage::{Pending, State};
use updater::updater::{check, CheckError};
use updater::{
    Artifact, Channel, Config, Install, Layout, Manifest, Manual, Outcome, Phase, Rollback,
    Schedule, SecretKey, Startup, Target, Timeouts, Updater, Version, WhyNot,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const PLATFORM: &str = "test-x86_64";
const BINARY: &str = "wuapi-inbox";
const SCHEMA: u32 = 12;

fn version(text: &str) -> Version {
    Version::parse(text).unwrap()
}

/// A script that ends with `code`: what stands for a version of the
/// application where a real process is started.
fn program(label: &str, code: i32) -> String {
    format!("#!/bin/sh\n# {label}\nexit {code}\n")
}

/// An install in a temporary directory, the key releases are signed with,
/// and the updater's own files.
struct World {
    dir: tempfile::TempDir,
    secret: SecretKey,
    layout: Layout,
    target: Target,
}

impl World {
    fn new(kind: PayloadKind) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let install = dir.path().join("install");
        std::fs::create_dir(&install).unwrap();
        let target = match kind {
            PayloadKind::File => {
                let path = install.join(BINARY);
                Target {
                    kind,
                    executable: path.clone(),
                    path,
                }
            }
            PayloadKind::Bundle => {
                let path = install.join("wuapi Inbox.app");
                Target {
                    kind,
                    executable: path.join("Contents").join("MacOS").join(BINARY),
                    path,
                }
            }
        };
        let world = Self {
            layout: Layout::in_data_dir(&dir.path().join("data")),
            secret: SecretKey::generate().unwrap(),
            target,
            dir,
        };
        world.write_version(&world.target.path, &program("1.0.0", 0));
        world
    }

    /// Writes a version of the application at `at`: the file, or the
    /// bundle around it.
    fn write_version(&self, at: &Path, body: &str) {
        let executable = self.target.staged_executable(at);
        std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
        std::fs::write(&executable, body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        if self.target.kind == PayloadKind::Bundle {
            std::fs::write(at.join("Contents").join("Info.plist"), body).unwrap();
        }
    }

    /// What is installed now: the content of the executable.
    fn installed(&self) -> String {
        std::fs::read_to_string(self.target.executable()).unwrap()
    }

    /// Builds the release of `version`: its archive, its manifest and the
    /// manifest's signature, in a directory of their own.
    fn release(&self, version_text: &str, body: &str) -> Release {
        let dir = self.dir.path().join(format!("release-{version_text}"));
        let build = dir.join("build");
        std::fs::create_dir_all(&build).unwrap();
        let name = self.target.path.file_name().unwrap();
        self.write_version(&build.join(name), body);
        let file_name = format!("{BINARY}-{version_text}-{PLATFORM}.tar.gz");
        let file = dir.join(&file_name);
        archive::pack(
            &build.join(name),
            &format!("{BINARY}-{version_text}-{PLATFORM}"),
            &file,
        )
        .unwrap();
        let (sha256, size) = archive::sha256_file(&file).unwrap();
        let manifest = Manifest {
            format: FORMAT,
            channel: Channel::Stable,
            version: version(version_text),
            released: "2026-10-02T12:00:00Z".into(),
            notes: format!("What is new in {version_text}."),
            min_supported: None,
            rollback: None,
            platforms: [(
                PLATFORM.to_owned(),
                Artifact {
                    url: file_name.clone(),
                    size,
                    sha256,
                },
            )]
            .into(),
        };
        let mut release = Release {
            dir,
            file_name,
            manifest: Vec::new(),
            signature: String::new(),
            file: std::fs::read(&file).unwrap(),
        };
        release.sign(&manifest, &self.secret);
        release
    }

    fn config(&self, base_url: &str) -> Config {
        Config {
            current: version("1.0.0"),
            channel: Channel::Stable,
            platform: PLATFORM.into(),
            base_url: base_url.into(),
            keys: vec![self.secret.public()],
            layout: self.layout.clone(),
            install: Install::SelfUpdating(self.target.clone()),
            binary_name: BINARY.into(),
            user_agent: updater::updater::user_agent(
                "wuapi-inbox",
                &version("1.0.0"),
                PLATFORM,
                Channel::Stable,
            ),
            schedule: Schedule::default(),
            timeouts: Timeouts {
                connect: Duration::from_secs(5),
                read: Duration::from_secs(5),
                manifest: Duration::from_secs(5),
            },
        }
    }

    /// The start of the application, as the version `current`.
    fn startup(&self, current: &str, schema: u32) -> Startup {
        Startup {
            current: version(current),
            layout: self.layout.clone(),
            keys: vec![self.secret.public()],
            channel: Channel::Stable,
            platform: PLATFORM.into(),
            install: Install::SelfUpdating(self.target.clone()),
            binary_name: BINARY.into(),
            schema,
            watch: Duration::from_secs(20),
        }
    }
}

struct Release {
    dir: PathBuf,
    file_name: String,
    manifest: Vec<u8>,
    signature: String,
    file: Vec<u8>,
}

impl Release {
    fn sign(&mut self, manifest: &Manifest, secret: &SecretKey) {
        self.manifest = manifest.to_bytes();
        self.signature = sign::sign(secret, &self.manifest, "test");
        std::fs::write(self.dir.join("latest.json"), &self.manifest).unwrap();
        std::fs::write(self.dir.join("latest.json.sig"), &self.signature).unwrap();
    }

    fn read(&self) -> Manifest {
        Manifest::parse(&self.manifest).unwrap()
    }

    /// Serves the release from a local server, whole files only.
    async fn serve(&self) -> MockServer {
        let server = MockServer::start().await;
        self.serve_manifest(&server).await;
        Mock::given(method("GET"))
            .and(path(format!("/{}", self.file_name)))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(self.file.clone()))
            .mount(&server)
            .await;
        server
    }

    async fn serve_manifest(&self, server: &MockServer) {
        Mock::given(method("GET"))
            .and(path("/latest.json"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(self.manifest.clone()))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/latest.json.sig"))
            .respond_with(ResponseTemplate::new(200).set_body_string(self.signature.clone()))
            .mount(server)
            .await;
    }
}

fn quiet(_: Phase) {}

/// How many requests the server got for the release's file.
async fn downloads(server: &MockServer, release: &Release) -> usize {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with(&release.file_name))
        .count()
}

/// A process that is not one: it "runs" until the closure says how it ended.
struct Fake<F: FnMut() -> Option<bool>>(F);

impl<F: FnMut() -> Option<bool>> Child for Fake<F> {
    fn ended(&mut self) -> std::io::Result<Option<bool>> {
        Ok((self.0)())
    }
}

fn fake(ends: impl FnMut() -> Option<bool> + 'static) -> std::io::Result<Box<dyn Child>> {
    Ok(Box::new(Fake(ends)))
}

/// A spawn that must not be reached.
fn no_spawn(_: &Path) -> std::io::Result<Box<dyn Child>> {
    panic!("nothing should be started here");
}

/// The whole road, for one kind of install.
async fn updates_end_to_end(kind: PayloadKind) {
    let world = World::new(kind);
    let old = world.installed();
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let server = release.serve().await;
    let config = world.config(&server.uri());

    // ----- check, download, verify, ready
    let phases = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = phases.clone();
    let phase = check(&config, &move |phase| seen.lock().unwrap().push(phase))
        .await
        .unwrap();
    assert_eq!(
        phase,
        Phase::Ready {
            version: version("1.1.0"),
            notes: "What is new in 1.1.0.".into()
        }
    );
    let phases = phases.lock().unwrap().clone();
    assert_eq!(phases[0], Phase::Checking);
    assert!(
        phases.iter().any(
            |phase| matches!(phase, Phase::Downloading { received, total, .. } if received == total)
        ),
        "{phases:?}"
    );
    assert_eq!(world.installed(), old, "nothing is installed while running");
    let staged = world.layout.artifact_file(&version("1.1.0"));
    assert_eq!(std::fs::read(&staged).unwrap(), release.file);
    assert!(!world.layout.part_file(&version("1.1.0")).exists());
    assert_eq!(
        State::load(&world.layout).ready.unwrap().version,
        version("1.1.0")
    );

    // What a request says: the version, the platform, the channel.
    for request in server.received_requests().await.unwrap() {
        assert_eq!(
            request.headers.get("user-agent").unwrap(),
            "wuapi-inbox/1.0.0 (test-x86_64; stable)"
        );
        assert_eq!(request.url.query(), None);
        assert!(request.headers.get("cookie").is_none());
        assert!(request.headers.get("authorization").is_none());
    }

    // Asking again downloads nothing again.
    assert!(matches!(
        check(&config, &quiet).await.unwrap(),
        Phase::Ready { .. }
    ));
    assert_eq!(downloads(&server, &release).await, 1);
    assert!(matches!(
        updater::updater::initial_phase(&config),
        Phase::Ready { .. }
    ));

    // ----- the next start installs it, starts it and watches it
    let started = Arc::new(AtomicUsize::new(0));
    let new_version = world.startup("1.1.0", SCHEMA);
    let outcome = {
        let started = started.clone();
        let executable = world.target.executable().to_owned();
        let layout = world.layout.clone();
        let spawn = move |path: &Path| {
            assert_eq!(path, executable);
            started.fetch_add(1, Ordering::SeqCst);
            // The mark is there for the new version to find.
            let pending = Pending::load(&layout).expect("a mark is left");
            assert_eq!(pending.version, version("1.1.0"));
            assert_eq!(pending.previous, version("1.0.0"));
            assert_eq!(pending.previous_schema, SCHEMA);
            // The new version starts, opens its store and its window.
            assert_eq!(new_version.run(&no_spawn), Outcome::Continue);
            new_version.opening_store();
            assert!(
                new_version.confirm_started(),
                "the first start after an update"
            );
            assert!(!new_version.confirm_started(), "and only the first");
            fake(|| None)
        };
        world.startup("1.0.0", SCHEMA).run(&spawn)
    };
    assert_eq!(outcome, Outcome::Exit(0), "the old process hands over");
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(world.installed(), program("1.1.0", 0));
    assert!(!world.target.backup().exists(), "removed once it started");
    assert!(!world.target.staging().exists());
    assert!(!world.target.failed().exists());
    assert_eq!(Pending::load(&world.layout), None);
    let state = State::load(&world.layout);
    assert_eq!(state.ready, None);
    assert!(state.refused.is_empty());
    assert!(!world.layout.staged(&version("1.1.0")).exists());

    // ----- later starts have nothing to do
    assert_eq!(
        world.startup("1.1.0", SCHEMA).run(&no_spawn),
        Outcome::Continue
    );
    assert_eq!(world.installed(), program("1.1.0", 0));
}

#[tokio::test]
async fn a_binary_is_updated_end_to_end() {
    updates_end_to_end(PayloadKind::File).await;
}

#[tokio::test]
async fn a_bundle_is_updated_end_to_end() {
    updates_end_to_end(PayloadKind::Bundle).await;
}

/// The version before comes back when the new one ends with an error
/// before it reaches its window, and that version is never installed again.
async fn goes_back_when_the_new_version_fails(kind: PayloadKind, real_process: bool) {
    let world = World::new(kind);
    let old = world.installed();
    let release = world.release("1.1.0", &program("1.1.0 (broken)", 3));
    let server = release.serve().await;
    let config = world.config(&server.uri());
    assert!(matches!(
        check(&config, &quiet).await.unwrap(),
        Phase::Ready { .. }
    ));

    let startup = world.startup("1.0.0", SCHEMA);
    let outcome = if real_process {
        // The script is really started, and really exits with 3.
        let spawn = updater::launch::spawn_with(Vec::new());
        startup.run(&spawn)
    } else {
        let installed = world.target.executable().to_owned();
        let spawn = move |_: &Path| {
            assert_eq!(
                std::fs::read_to_string(&installed).unwrap(),
                program("1.1.0 (broken)", 3),
                "the new version is in place when it is started"
            );
            fake(|| Some(false))
        };
        startup.run(&spawn)
    };
    assert_eq!(
        outcome,
        Outcome::Continue,
        "the old version goes on starting"
    );
    assert_eq!(world.installed(), old, "the version before is back");
    assert!(!world.target.backup().exists());
    assert!(!world.target.failed().exists());
    assert_eq!(Pending::load(&world.layout), None);
    let state = State::load(&world.layout);
    assert_eq!(state.refused, vec![version("1.1.0")]);
    assert_eq!(state.ready, None);
    let notice = state.notice.unwrap();
    assert!(
        notice.contains("1.1.0") && notice.contains("1.0.0"),
        "{notice}"
    );

    // The same release is not taken again…
    let before = downloads(&server, &release).await;
    assert_eq!(check(&config, &quiet).await.unwrap(), Phase::UpToDate);
    assert_eq!(downloads(&server, &release).await, before);
    assert_eq!(startup.run(&no_spawn), Outcome::Continue);
    assert_eq!(world.installed(), old);

    // …and the one after it is.
    let fixed = world.release("1.1.1", &program("1.1.1", 0));
    let server = fixed.serve().await;
    let config = world.config(&server.uri());
    assert_eq!(
        check(&config, &quiet).await.unwrap(),
        Phase::Ready {
            version: version("1.1.1"),
            notes: "What is new in 1.1.1.".into()
        }
    );
}

#[tokio::test]
async fn a_binary_that_does_not_start_is_undone() {
    goes_back_when_the_new_version_fails(PayloadKind::File, false).await;
}

#[tokio::test]
async fn a_bundle_that_does_not_start_is_undone() {
    goes_back_when_the_new_version_fails(PayloadKind::Bundle, false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_real_process_that_exits_with_an_error_is_undone() {
    goes_back_when_the_new_version_fails(PayloadKind::File, true).await;
    goes_back_when_the_new_version_fails(PayloadKind::Bundle, true).await;
}

#[cfg(unix)]
#[tokio::test]
async fn a_real_process_that_ends_well_is_kept() {
    let world = World::new(PayloadKind::File);
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let server = release.serve().await;
    check(&world.config(&server.uri()), &quiet).await.unwrap();
    let spawn = updater::launch::spawn_with(Vec::new());
    assert_eq!(world.startup("1.0.0", SCHEMA).run(&spawn), Outcome::Exit(0));
    assert_eq!(world.installed(), program("1.1.0", 0));
    assert_eq!(Pending::load(&world.layout), None);
    assert!(State::load(&world.layout).refused.is_empty());
}

#[tokio::test]
async fn a_version_that_cannot_be_started_at_all_is_undone() {
    let world = World::new(PayloadKind::File);
    let old = world.installed();
    let release = world.release("1.1.0", "not a program");
    let server = release.serve().await;
    check(&world.config(&server.uri()), &quiet).await.unwrap();
    let spawn = |_: &Path| -> std::io::Result<Box<dyn Child>> {
        Err(std::io::Error::other("exec format error"))
    };
    assert_eq!(
        world.startup("1.0.0", SCHEMA).run(&spawn),
        Outcome::Continue
    );
    assert_eq!(world.installed(), old);
    assert_eq!(State::load(&world.layout).refused, vec![version("1.1.0")]);
}

#[tokio::test]
async fn going_back_is_not_done_once_the_database_was_upgraded() {
    let world = World::new(PayloadKind::File);
    let release = world.release("2.0.0", &program("2.0.0", 0));
    let server = release.serve().await;
    check(&world.config(&server.uri()), &quiet).await.unwrap();

    // The new version opens the store (schema 13, which 1.0.0 cannot
    // open), then dies.
    let new_version = world.startup("2.0.0", SCHEMA + 1);
    let spawn = move |_: &Path| {
        assert_eq!(new_version.run(&no_spawn), Outcome::Continue);
        new_version.opening_store();
        fake(|| Some(false))
    };
    assert_eq!(
        world.startup("1.0.0", SCHEMA).run(&spawn),
        Outcome::Exit(1),
        "the old version does not start over a database it cannot open"
    );
    assert_eq!(
        world.installed(),
        program("2.0.0", 0),
        "the new version stays in place"
    );
    let state = State::load(&world.layout);
    assert!(state.refused.is_empty());
    let notice = state.notice.unwrap();
    assert!(
        notice.contains("schema 13") && notice.contains("schema 12") && notice.contains("2.0.0"),
        "{notice}"
    );

    // Had it died before it got to the store, it would have been undone.
    let world = World::new(PayloadKind::File);
    let old = world.installed();
    let release = world.release("2.0.0", &program("2.0.0", 0));
    let server = release.serve().await;
    check(&world.config(&server.uri()), &quiet).await.unwrap();
    let spawn = |_: &Path| fake(|| Some(false));
    assert_eq!(
        world.startup("1.0.0", SCHEMA).run(&spawn),
        Outcome::Continue
    );
    assert_eq!(world.installed(), old);
}

#[tokio::test]
async fn a_version_nobody_watches_takes_itself_out_after_failing_to_start() {
    let world = World::new(PayloadKind::File);
    let old = world.installed();
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let server = release.serve().await;
    check(&world.config(&server.uri()), &quiet).await.unwrap();

    // Installed, and the watching process gave up waiting (the new
    // version hangs before its window).
    let mut startup = world.startup("1.0.0", SCHEMA);
    startup.watch = Duration::from_millis(100);
    assert_eq!(startup.run(&|_: &Path| fake(|| None)), Outcome::Exit(0));
    assert_eq!(world.installed(), program("1.1.0", 0));
    assert!(
        Pending::load(&world.layout).is_some(),
        "still not confirmed"
    );

    // It is started again and again and never gets to its window.
    let new_version = world.startup("1.1.0", SCHEMA);
    for start in 1..=updater::launch::MAX_UNCONFIRMED_STARTS {
        assert_eq!(
            new_version.run(&no_spawn),
            Outcome::Continue,
            "start {start}"
        );
        assert_eq!(world.installed(), program("1.1.0", 0));
    }
    // The next start puts the version before back and starts it.
    let started = Arc::new(AtomicUsize::new(0));
    let count = started.clone();
    let spawn = move |_: &Path| {
        count.fetch_add(1, Ordering::SeqCst);
        fake(|| None)
    };
    assert_eq!(new_version.run(&spawn), Outcome::Exit(0));
    assert_eq!(started.load(Ordering::SeqCst), 1);
    assert_eq!(world.installed(), old);
    assert_eq!(State::load(&world.layout).refused, vec![version("1.1.0")]);
    assert_eq!(Pending::load(&world.layout), None);
}

/// Answers a `Range` request with the rest of the file, as a static host
/// does.
struct Ranged(Vec<u8>);

impl Respond for Ranged {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let from = request
            .headers
            .get("range")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("bytes="))
            .and_then(|value| value.strip_suffix('-'))
            .and_then(|value| value.parse::<usize>().ok());
        match from {
            Some(from) if from < self.0.len() => ResponseTemplate::new(206)
                .insert_header(
                    "content-range",
                    format!("bytes {from}-{}/{}", self.0.len() - 1, self.0.len()),
                )
                .set_body_bytes(self.0[from..].to_vec()),
            Some(_) => ResponseTemplate::new(416),
            None => ResponseTemplate::new(200).set_body_bytes(self.0.clone()),
        }
    }
}

#[tokio::test]
async fn a_download_goes_on_from_where_it_stopped() {
    let world = World::new(PayloadKind::File);
    // Large enough to be worth resuming, and not compressible to nothing.
    let mut body = program("1.1.0", 0);
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    for _ in 0..20_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        body.push_str(&format!("# {seed:016x}\n"));
    }
    let release = world.release("1.1.0", &body);
    assert!(release.file.len() > 100_000, "{}", release.file.len());
    let server = MockServer::start().await;
    release.serve_manifest(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("/{}", release.file_name)))
        .respond_with(Ranged(release.file.clone()))
        .mount(&server)
        .await;

    // A transfer that was cut off earlier left the first part on disk.
    let v = version("1.1.0");
    let have = release.file.len() / 3;
    std::fs::create_dir_all(world.layout.staged(&v)).unwrap();
    std::fs::write(world.layout.part_file(&v), &release.file[..have]).unwrap();

    let config = world.config(&server.uri());
    assert!(matches!(
        check(&config, &quiet).await.unwrap(),
        Phase::Ready { .. }
    ));
    let requests = server.received_requests().await.unwrap();
    let download = requests
        .iter()
        .find(|request| request.url.path().ends_with(&release.file_name))
        .unwrap();
    assert_eq!(
        download.headers.get("range").unwrap().to_str().unwrap(),
        format!("bytes={have}-"),
        "only the rest is asked for"
    );
    assert_eq!(
        std::fs::read(world.layout.artifact_file(&v)).unwrap(),
        release.file,
        "and the two parts make the file"
    );
}

#[tokio::test]
async fn a_server_that_cannot_resume_is_asked_for_the_whole_file() {
    let world = World::new(PayloadKind::File);
    let release = world.release("1.1.0", &program("1.1.0", 0));
    // This server ignores `Range` and always sends everything.
    let server = release.serve().await;
    let v = version("1.1.0");
    std::fs::create_dir_all(world.layout.staged(&v)).unwrap();
    std::fs::write(world.layout.part_file(&v), &release.file[..10]).unwrap();
    assert!(matches!(
        check(&world.config(&server.uri()), &quiet).await.unwrap(),
        Phase::Ready { .. }
    ));
    assert_eq!(
        std::fs::read(world.layout.artifact_file(&v)).unwrap(),
        release.file
    );
}

#[tokio::test]
async fn a_part_that_was_not_this_file_is_thrown_away() {
    let world = World::new(PayloadKind::File);
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let server = MockServer::start().await;
    release.serve_manifest(&server).await;
    Mock::given(method("GET"))
        .and(path(format!("/{}", release.file_name)))
        .respond_with(Ranged(release.file.clone()))
        .mount(&server)
        .await;
    // Bytes of something else, left under the same name.
    let v = version("1.1.0");
    std::fs::create_dir_all(world.layout.staged(&v)).unwrap();
    std::fs::write(world.layout.part_file(&v), vec![0u8; 16]).unwrap();
    let config = world.config(&server.uri());
    assert!(matches!(
        check(&config, &quiet).await,
        Err(CheckError::Hash)
    ));
    assert!(!world.layout.part_file(&v).exists());
    assert_eq!(State::load(&world.layout).ready, None);
    // The next attempt starts from nothing and gets it right.
    assert!(matches!(
        check(&config, &quiet).await.unwrap(),
        Phase::Ready { .. }
    ));
}

#[tokio::test]
async fn a_manifest_that_was_changed_or_signed_by_another_is_not_acted_on() {
    let world = World::new(PayloadKind::File);
    let old = world.installed();
    let mut release = world.release("1.1.0", &program("1.1.0", 0));

    // Changed after it was signed.
    let honest = release.manifest.clone();
    release.manifest = String::from_utf8(honest.clone())
        .unwrap()
        .replace("What is new", "What is NEW")
        .into_bytes();
    let server = release.serve().await;
    let config = world.config(&server.uri());
    assert!(matches!(
        check(&config, &quiet).await,
        Err(CheckError::Manifest(_))
    ));
    assert_eq!(downloads(&server, &release).await, 0, "nothing is fetched");

    // Signed, but by a key this build does not know.
    release.manifest = honest;
    release.signature = sign::sign(&SecretKey::generate().unwrap(), &release.manifest, "x");
    let server = release.serve().await;
    let config = world.config(&server.uri());
    assert!(matches!(
        check(&config, &quiet).await,
        Err(CheckError::Manifest(_))
    ));
    assert_eq!(downloads(&server, &release).await, 0);

    // Not signed at all.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/latest.json"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(release.manifest.clone()))
        .mount(&server)
        .await;
    assert!(matches!(
        check(&world.config(&server.uri()), &quiet).await,
        Err(CheckError::Source(_))
    ));

    // A build with no key embedded believes nobody.
    release.sign(&release.read(), &world.secret);
    let server = release.serve().await;
    let mut config = world.config(&server.uri());
    config.keys.clear();
    assert!(check(&config, &quiet).await.is_err());

    assert_eq!(State::load(&world.layout).ready, None);
    assert_eq!(
        world.startup("1.0.0", SCHEMA).run(&no_spawn),
        Outcome::Continue
    );
    assert_eq!(world.installed(), old);
}

#[tokio::test]
async fn a_file_that_is_not_the_one_signed_for_is_not_installed() {
    let world = World::new(PayloadKind::File);
    let mut release = world.release("1.1.0", &program("1.1.0", 0));
    // Same size, other bytes.
    let last = release.file.len() - 1;
    release.file[last] ^= 0xff;
    let server = release.serve().await;
    let config = world.config(&server.uri());
    assert!(matches!(
        check(&config, &quiet).await,
        Err(CheckError::Hash)
    ));
    assert!(!world.layout.artifact_file(&version("1.1.0")).exists());
    assert!(!world.layout.part_file(&version("1.1.0")).exists());

    // Larger than the manifest says: cut off, not stored.
    release.file.extend_from_slice(&[0u8; 4096]);
    let server = release.serve().await;
    assert!(matches!(
        check(&world.config(&server.uri()), &quiet).await,
        Err(CheckError::Source(updater::source::SourceError::TooLarge))
    ));
    assert!(!world.layout.part_file(&version("1.1.0")).exists());

    // Smaller: kept as a part, never called ready.
    release.file.truncate(100);
    let server = release.serve().await;
    let error = check(&world.config(&server.uri()), &quiet)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, CheckError::Source(source) if source.is_transient()),
        "{error}"
    );
    assert_eq!(State::load(&world.layout).ready, None);
}

#[tokio::test]
async fn an_archive_without_the_application_in_it_is_not_made_ready() {
    let world = World::new(PayloadKind::File);
    let mut release = world.release("1.1.0", &program("1.1.0", 0));
    // A well-signed manifest for an archive that holds something else.
    let other = world.dir.path().join("README");
    std::fs::write(&other, "hello").unwrap();
    let file = release.dir.join(&release.file_name);
    archive::pack(&other, "root", &file).unwrap();
    let (sha256, size) = archive::sha256_file(&file).unwrap();
    release.file = std::fs::read(&file).unwrap();
    let mut manifest = release.read();
    let artifact = manifest.platforms.get_mut(PLATFORM).unwrap();
    (artifact.sha256, artifact.size) = (sha256, size);
    release.sign(&manifest, &world.secret);
    let server = release.serve().await;
    assert!(matches!(
        check(&world.config(&server.uri()), &quiet).await,
        Err(CheckError::Archive(_))
    ));
    assert_eq!(State::load(&world.layout).ready, None);
}

#[tokio::test]
async fn an_older_version_is_never_installed_unless_it_is_a_signed_rollback() {
    let world = World::new(PayloadKind::File);
    let mut release = world.release("0.9.0", &program("0.9.0", 0));
    let server = release.serve().await;
    let config = world.config(&server.uri());
    assert_eq!(check(&config, &quiet).await.unwrap(), Phase::UpToDate);
    assert_eq!(downloads(&server, &release).await, 0);

    // The same version: nothing to do either.
    let same = world.release("1.0.0", &program("1.0.0", 0));
    let server = same.serve().await;
    assert_eq!(
        check(&world.config(&server.uri()), &quiet).await.unwrap(),
        Phase::UpToDate
    );

    // A rollback that names another version leaves this one alone…
    let mut manifest = release.read();
    manifest.rollback = Some(Rollback {
        from: vec![version("1.0.1")],
    });
    release.sign(&manifest, &world.secret);
    let server = release.serve().await;
    assert_eq!(
        check(&world.config(&server.uri()), &quiet).await.unwrap(),
        Phase::UpToDate
    );

    // …and one that names this version takes it back.
    manifest.rollback = Some(Rollback {
        from: vec![version("1.0.0")],
    });
    release.sign(&manifest, &world.secret);
    let server = release.serve().await;
    assert!(matches!(
        check(&world.config(&server.uri()), &quiet).await.unwrap(),
        Phase::Ready { version: v, .. } if v == version("0.9.0")
    ));
    let confirm = world.startup("0.9.0", SCHEMA);
    let spawn = move |_: &Path| {
        confirm.confirm_started();
        fake(|| None)
    };
    assert_eq!(world.startup("1.0.0", SCHEMA).run(&spawn), Outcome::Exit(0));
    assert_eq!(world.installed(), program("0.9.0", 0));
}

#[tokio::test]
async fn a_staged_update_is_verified_again_before_it_is_installed() {
    let world = World::new(PayloadKind::File);
    let old = world.installed();
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let server = release.serve().await;
    let config = world.config(&server.uri());
    let v = version("1.1.0");

    // The file was swapped on disk between the download and the restart.
    check(&config, &quiet).await.unwrap();
    let evil = world.dir.path().join(BINARY);
    std::fs::write(&evil, program("evil", 0)).unwrap();
    archive::pack(&evil, "root", &world.layout.artifact_file(&v)).unwrap();
    assert_eq!(
        world.startup("1.0.0", SCHEMA).run(&no_spawn),
        Outcome::Continue
    );
    assert_eq!(world.installed(), old);
    assert_eq!(State::load(&world.layout).ready, None, "and it is dropped");
    assert_eq!(Pending::load(&world.layout), None);
    assert!(!world.target.staging().exists());

    // The manifest on disk was replaced by one signed by somebody else.
    check(&config, &quiet).await.unwrap();
    let forged = sign::sign(&SecretKey::generate().unwrap(), &release.manifest, "x");
    std::fs::write(world.layout.signature_file(&v), forged).unwrap();
    assert_eq!(
        world.startup("1.0.0", SCHEMA).run(&no_spawn),
        Outcome::Continue
    );
    assert_eq!(world.installed(), old);

    // A version that is already past the staged one does not go back to it.
    check(&config, &quiet).await.unwrap();
    assert_eq!(
        world.startup("1.2.0", SCHEMA).run(&no_spawn),
        Outcome::Continue
    );
    assert_eq!(world.installed(), old);
}

#[tokio::test]
async fn nothing_published_or_nothing_visible_is_no_update_and_no_error() {
    let world = World::new(PayloadKind::File);
    // What a private repository answers to a request without a token.
    for status in [404, 401, 403, 410] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status))
            .mount(&server)
            .await;
        assert_eq!(
            check(&world.config(&server.uri()), &quiet).await.unwrap(),
            Phase::UpToDate,
            "{status}"
        );
    }
    // A server having a bad moment is tried again later, quietly.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    let error = check(&world.config(&server.uri()), &quiet)
        .await
        .unwrap_err();
    assert!(matches!(&error, CheckError::Source(source) if source.is_transient()));
    // Nobody listening at all.
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let error = check(&world.config(&closed), &quiet).await.unwrap_err();
    assert!(matches!(&error, CheckError::Source(source) if source.is_transient()));
    // Plain http to anywhere else is not even tried.
    assert!(check(&world.config("http://example.com/updates"), &quiet)
        .await
        .is_err());
}

#[tokio::test]
async fn an_install_that_does_not_replace_itself_is_told_about_the_version() {
    let world = World::new(PayloadKind::File);
    let old = world.installed();
    let mut release = world.release("1.1.0", &program("1.1.0", 0));
    let server = release.serve().await;
    let mut config = world.config(&server.uri());
    config.install = Install::NotifyOnly(WhyNot::Packaged);
    assert_eq!(
        check(&config, &quiet).await.unwrap(),
        Phase::Available {
            version: version("1.1.0"),
            notes: "What is new in 1.1.0.".into(),
            why: Manual::Install(WhyNot::Packaged),
        }
    );
    assert_eq!(
        downloads(&server, &release).await,
        0,
        "and nothing is fetched"
    );
    let mut startup = world.startup("1.0.0", SCHEMA);
    startup.install = Install::NotifyOnly(WhyNot::Packaged);
    assert_eq!(startup.run(&no_spawn), Outcome::Continue);
    assert_eq!(world.installed(), old);

    // No build for this platform.
    let mut config = world.config(&server.uri());
    config.platform = "plan9-mips".into();
    assert!(matches!(
        check(&config, &quiet).await.unwrap(),
        Phase::Available {
            why: Manual::NoBuild,
            ..
        }
    ));

    // Too old to update itself.
    let mut manifest = release.read();
    manifest.min_supported = Some(version("1.0.5"));
    release.sign(&manifest, &world.secret);
    let server = release.serve().await;
    assert!(matches!(
        check(&world.config(&server.uri()), &quiet).await.unwrap(),
        Phase::Available {
            why: Manual::TooOld,
            ..
        }
    ));
    assert_eq!(downloads(&server, &release).await, 0);
}

#[tokio::test]
async fn a_directory_is_a_source_too() {
    let world = World::new(PayloadKind::File);
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let base = reqwest::Url::from_directory_path(&release.dir)
        .unwrap()
        .to_string();
    assert!(base.starts_with("file://"), "{base}");
    assert!(matches!(
        check(&world.config(&base), &quiet).await.unwrap(),
        Phase::Ready { .. }
    ));
    // An empty directory has no update.
    let empty = world.dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let base = reqwest::Url::from_directory_path(&empty)
        .unwrap()
        .to_string();
    assert_eq!(
        check(&world.config(&base), &quiet).await.unwrap(),
        Phase::UpToDate
    );
}

#[tokio::test]
async fn an_update_larger_than_any_is_not_downloaded() {
    let world = World::new(PayloadKind::File);
    let mut release = world.release("1.1.0", &program("1.1.0", 0));
    let mut manifest = release.read();
    manifest.platforms.get_mut(PLATFORM).unwrap().size = 10 * 1024 * 1024 * 1024;
    release.sign(&manifest, &world.secret);
    let server = release.serve().await;
    assert!(matches!(
        check(&world.config(&server.uri()), &quiet).await,
        Err(CheckError::TooLarge(_))
    ));
    assert_eq!(downloads(&server, &release).await, 0);
}

/// Waits until the updater says something that satisfies `done`.
async fn until(updater: &Updater, done: impl Fn(&updater::Snapshot) -> bool) -> updater::Snapshot {
    let mut changes = updater.subscribe();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let snapshot = changes.borrow_and_update().clone();
            if done(&snapshot) {
                return snapshot;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .expect("the updater gets there")
}

#[tokio::test]
async fn the_running_updater_checks_by_itself_and_when_asked() {
    let world = World::new(PayloadKind::File);
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let server = release.serve().await;
    let mut config = world.config(&server.uri());
    config.schedule.first = Duration::from_millis(10);

    // By itself, shortly after the start.
    let updater = Updater::start(config.clone(), true, &tokio::runtime::Handle::current());
    assert_eq!(updater.snapshot().phase, Phase::Idle);
    let snapshot = until(&updater, |s| matches!(s.phase, Phase::Ready { .. })).await;
    assert!(snapshot.last_check.is_some());
    assert!(!snapshot.unreachable);
    assert!(State::load(&world.layout).last_check.is_some());
    drop(updater);

    // The next run knows at once that an update is waiting.
    let updater = Updater::start(config.clone(), false, &tokio::runtime::Handle::current());
    assert!(matches!(updater.snapshot().phase, Phase::Ready { .. }));
    assert!(!updater.snapshot().automatic);
}

#[tokio::test]
async fn with_automatic_checks_off_nothing_is_asked_until_somebody_does() {
    let world = World::new(PayloadKind::File);
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let server = release.serve().await;
    let mut config = world.config(&server.uri());
    config.schedule.first = Duration::from_millis(1);
    let updater = Updater::start(config, false, &tokio::runtime::Handle::current());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(server.received_requests().await.unwrap().is_empty());
    assert_eq!(updater.snapshot().phase, Phase::Idle);

    updater.check_now();
    until(&updater, |s| matches!(s.phase, Phase::Ready { .. })).await;
}

#[tokio::test]
async fn a_source_that_cannot_be_reached_is_no_update_said_quietly() {
    let world = World::new(PayloadKind::File);
    let closed = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}", listener.local_addr().unwrap())
    };
    let updater = Updater::start(
        world.config(&closed),
        false,
        &tokio::runtime::Handle::current(),
    );
    updater.check_now();
    let snapshot = until(&updater, |s| s.last_check.is_some()).await;
    assert_eq!(snapshot.phase, Phase::UpToDate);
    assert!(snapshot.unreachable);
    assert_eq!(State::load(&world.layout).failures, 1);

    // Another address is taken from then on, and its failures start at none.
    let release = world.release("1.1.0", &program("1.1.0", 0));
    let server = release.serve().await;
    updater.set_base_url(server.uri());
    updater.check_now();
    let snapshot = until(&updater, |s| matches!(s.phase, Phase::Ready { .. })).await;
    assert!(!snapshot.unreachable);
    assert_eq!(State::load(&world.layout).failures, 0);
}

/// Windows lets a running executable be renamed but not written or
/// deleted, which is what the two-step replacement relies on; Unix lets
/// it be replaced outright. This starts a real process from the "installed"
/// executable and replaces that executable while it runs.
#[test]
fn the_executable_of_a_running_process_is_replaced_and_put_back() {
    let dir = tempfile::tempdir().unwrap();
    let name = format!("wuapi-inbox{}", std::env::consts::EXE_SUFFIX);
    let path = dir.path().join(&name);
    // A real program to run: this test binary, asked to run the test
    // below, which only sleeps.
    let this = std::env::current_exe().unwrap();
    std::fs::copy(&this, &path).unwrap();
    let original = std::fs::read(&path).unwrap();
    let target = Target {
        kind: PayloadKind::File,
        executable: path.clone(),
        path: path.clone(),
    };
    let mut running = std::process::Command::new(&path)
        .args(["a_process_that_only_sleeps", "--exact", "--ignored"])
        .env("UPDATER_TEST_SLEEP", "20")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert!(running.try_wait().unwrap().is_none(), "it is running");

    std::fs::create_dir(target.staging()).unwrap();
    let new = target.staging().join(&name);
    std::fs::write(&new, b"the new version").unwrap();
    let replaced = target.replace_with(&new);
    let after = std::fs::read(&path);
    let restored = target.restore();
    let back = std::fs::read(&path);
    target.clean(true);
    running.kill().unwrap();
    running.wait().unwrap();

    replaced.unwrap();
    assert_eq!(after.unwrap(), b"the new version");
    restored.unwrap();
    assert_eq!(back.unwrap(), original);
    // Once the process is gone nothing is left next to the executable.
    target.clean(true);
    let left: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(left, vec![std::ffi::OsString::from(&name)]);
}

#[test]
#[ignore = "run by the test above, in a process of its own"]
fn a_process_that_only_sleeps() {
    if let Ok(seconds) = std::env::var("UPDATER_TEST_SLEEP") {
        std::thread::sleep(Duration::from_secs(seconds.parse().unwrap()));
    }
}

#[test]
fn a_restarted_process_waits_for_the_one_that_started_it() {
    // Not started by `restart`: no wait at all.
    let before = std::time::Instant::now();
    updater::wait_for_parent(Duration::from_secs(30));
    assert!(before.elapsed() < Duration::from_secs(5));
}

/// The spawn type is what the application passes; keep it nameable.
#[allow(dead_code)]
fn spawn_is_object_safe(spawn: Spawn<'_>) -> Spawn<'_> {
    spawn
}
