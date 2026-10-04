//! Choosing and constructing the provider for this session.

use crate::cli::{Options, ProviderKind};
use crate::login::{BoxFuture, Identity, IdentityLoader, KeySaved, LoginFlow, Session};
use crate::product;
use crate::settings::{self, Settings};
use crate::storage::{self, KeyVault, OsKeychain, Prepared, Recovery, Storage};
use crate::ui::SessionKind;
use client_core::{HistoryMode, SyncConfig, SyncEngine};
use client_provider::{Provider, ProviderError};
use provider_mock::MockProvider;
use provider_wuapi::{
    ApiKey, DeviceCode, DeviceLogin, LoginError, PollOutcome, TokenGrant, WuapiConfig,
    WuapiProvider,
};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::runtime::Handle;

/// How the application starts.
pub enum Launch {
    /// Straight into the chat list.
    Chats {
        /// The engine, already started.
        engine: SyncEngine,
        /// What kind of session this is, for the settings menu.
        session: SessionKind,
        /// The sign-in to return to after "Sign out", when there is one.
        login: Option<Arc<dyn LoginFlow>>,
        /// Who is signed in, when the provider can say.
        identity: Option<IdentityLoader>,
        /// What to say on screen about where the chats are kept, when
        /// they are not being saved.
        storage_note: Option<String>,
    },
    /// The sign-in screen first: there is no API key yet.
    Login {
        /// The sign-in.
        flow: Arc<dyn LoginFlow>,
        /// Set when the OS keychain could not be read, with its reason.
        keychain_error: Option<String>,
        /// As in [`Launch::Chats`].
        storage_note: Option<String>,
    },
    /// The local database is encrypted and its key is gone: ask before
    /// deleting it.
    Recover {
        /// The database in question.
        recovery: Recovery,
        /// Carries on with the launch once there is a database again.
        resume: Box<dyn FnOnce(Storage) -> Result<Launch, String>>,
    },
}

/// Picks the provider the user asked for and starts whatever can start
/// without the network or the user.
pub fn launch(options: &Options, runtime: &Handle) -> Result<Launch, String> {
    let database = match (options.provider, &options.data_dir) {
        // The mock invents a new world on every start, so persisting its
        // messages would only pile up stale ones. An explicit `--data-dir`
        // still gets a real file, to exercise the store.
        (ProviderKind::Mock, None) => None,
        // The example's world is in memory too, and new at every start.
        #[cfg(feature = "provider-example")]
        (ProviderKind::Example, _) => None,
        (ProviderKind::Mock, Some(dir)) => Some(database_in(dir, "mock")?),
        (ProviderKind::Wuapi, dir) => Some(database_in(
            &dir.clone().unwrap_or_else(product::data_dir),
            "wuapi",
        )?),
    };
    let mut home = None;
    let storage = match database {
        None => Storage::Memory { note: None },
        Some(path) => {
            let vault = (!options.no_keychain).then(|| {
                Arc::new(OsKeychain {
                    service: product::slug(),
                }) as Arc<dyn KeyVault>
            });
            // Each database file has its own key, named after the file.
            let name = storage::key_name(&path);
            home = Some(Database {
                path: path.clone(),
                name: name.clone(),
                vault: vault.clone(),
            });
            match storage::prepare(&path, &name, vault) {
                Prepared::Ready(storage) => storage,
                Prepared::KeyLost(recovery) => {
                    let (options, runtime) = (options.clone(), runtime.clone());
                    return Ok(Launch::Recover {
                        recovery,
                        resume: Box::new(move |storage| {
                            launch_on(&options, &runtime, storage, home)
                        }),
                    });
                }
            }
        }
    };
    launch_on(options, runtime, storage, home)
}

/// How many sends `--diagnose sends` lists.
const DIAGNOSED_SENDS: usize = 20;

/// The `sends` report: how long the last messages sent from here took,
/// read from the local database beside whatever has it open. Nothing is
/// migrated or written, the API is not asked, and the lines carry
/// durations only: no text, no number, no key.
fn diagnose_sends(options: &Options) -> Result<Vec<String>, String> {
    let dir = options.data_dir.clone().unwrap_or_else(product::data_dir);
    let path = dir.join("wuapi.db");
    let key = match client_core::file_state(&path).map_err(|error| error.to_string())? {
        client_core::FileState::Missing => {
            return Err(format!("there is no database at {}", path.display()))
        }
        client_core::FileState::Plain => None,
        client_core::FileState::Encrypted if options.no_keychain => {
            return Err("the database is encrypted: drop --no-keychain".into())
        }
        client_core::FileState::Encrypted => {
            let vault = OsKeychain {
                service: product::slug(),
            };
            Some(
                vault
                    .load(&storage::key_name(&path))
                    .map_err(|error| format!("the OS keychain is not available: {error}"))?
                    .ok_or("the key of the database is not in the keychain")?,
            )
        }
    };
    let timings = client_core::Store::read_send_timings(&path, key.as_ref(), DIAGNOSED_SENDS)
        .map_err(|error| error.to_string())?;
    Ok(sends_report(timings))
}

/// The lines of the `sends` report. Every duration counts from Enter,
/// except `post`, which is how long the answered request itself took.
fn sends_report(timings: Option<Vec<client_core::SendTiming>>) -> Vec<String> {
    let Some(timings) = timings else {
        return vec![
            "sends: this database is from a build that did not time sends yet; \
             start the application once and send a message"
                .to_owned(),
        ];
    };
    let mut lines = vec![format!(
        "sends {} (newest first; times since Enter, post = the request alone)",
        timings.len()
    )];
    lines.extend(timings.iter().map(client_core::SendTiming::line));
    lines
}

/// The wuapi configuration the command line asks for, and the keychain
/// profile of the API it names. Built here once, for the session and for the
/// reports alike.
fn wuapi_config(options: &Options) -> (WuapiConfig, String) {
    let mut config = WuapiConfig::new(product::user_agent());
    let mut profile = WUAPI_PROFILE.to_owned();
    if let Some(url) = &options.api_url {
        config.base_url = url.trim_end_matches('/').to_owned();
        // A key for another API never replaces the production one.
        profile = config.base_url.clone();
    }
    config.stream_url = options.stream_url.clone();
    if let Some(live) = options.live {
        config.live = live;
    }
    (config, profile)
}

/// The report `--diagnose` asks for, as lines to print. `chats` and
/// `stories` use the key the application would use (the environment's,
/// else the keychain's), never print it and touch no database; `sends`
/// only reads the local database, and asks the API nothing; `stream` opens
/// one connection to the event stream and listens for a while.
pub async fn diagnose(
    options: &Options,
    report: crate::cli::Diagnose,
) -> Result<Vec<String>, String> {
    if report == crate::cli::Diagnose::Sends {
        return diagnose_sends(options);
    }
    let (config, profile) = wuapi_config(options);
    let key = match std::env::var("WUAPI_API_KEY")
        .ok()
        .filter(|k| !k.is_empty())
    {
        Some(key) => ApiKey::new(key),
        None if options.no_keychain => {
            return Err("there is no API key: set WUAPI_API_KEY or drop --no-keychain".into())
        }
        None => provider_wuapi::load_api_key(&product::slug(), &profile)
            .map_err(|error| format!("the OS keychain is not available: {error}"))?
            .ok_or("not signed in: there is no API key in the keychain")?,
    };
    let provider = WuapiProvider::new(config, key).map_err(|error| error.to_string())?;
    match report {
        crate::cli::Diagnose::Chats => provider
            .diagnose_chats()
            .await
            .map_err(|error| error.to_string()),
        crate::cli::Diagnose::Stories => provider
            .diagnose_stories()
            .await
            .map_err(|error| error.to_string()),
        crate::cli::Diagnose::Sends => diagnose_sends(options),
        crate::cli::Diagnose::Stream => provider
            .diagnose_stream()
            .await
            .map_err(|error| error.to_string()),
    }
}

/// A database on disk: where it is and where its key is kept.
#[derive(Clone)]
struct Database {
    path: PathBuf,
    /// The name its key is filed under.
    name: String,
    /// `None`: the keychain is not used.
    vault: Option<Arc<dyn KeyVault>>,
}

/// The settings as they are on disk now.
fn saved_settings(options: &Options) -> Settings {
    let dir = options.data_dir.clone().unwrap_or_else(product::data_dir);
    Settings::load(&dir.join(settings::FILE_NAME))
}

/// The launch, once it is known where the data goes.
fn launch_on(
    options: &Options,
    runtime: &Handle,
    storage: Storage,
    home: Option<Database>,
) -> Result<Launch, String> {
    let storage_note = storage.note();
    let history = saved_settings(options).history_mode();
    match options.provider {
        ProviderKind::Mock => {
            // Frames for the documentation are taken without a keychain
            // (see `capture`), and show the window as it is with one.
            #[cfg(feature = "capture")]
            let storage_note = storage_note.filter(|_| !crate::capture::running());
            let engine = start_engine(
                Arc::new(MockProvider::default()),
                &storage,
                history,
                runtime,
            )?;
            Ok(Launch::Chats {
                engine,
                session: SessionKind::Demo,
                login: None,
                identity: None,
                storage_note,
            })
        }
        // A provider that needs no sign-in is this much: build it, start
        // an engine on it, say what kind of session it is.
        #[cfg(feature = "provider-example")]
        ProviderKind::Example => Ok(Launch::Chats {
            engine: start_engine(
                Arc::new(provider_example::EchoProvider::default()),
                &storage,
                history,
                runtime,
            )?,
            session: SessionKind::Demo,
            login: None,
            identity: None,
            storage_note,
        }),
        ProviderKind::Wuapi => {
            let login = Arc::new(WuapiLogin::new(options, storage, home, runtime)?);
            // A key in the environment wins and is never stored.
            if let Some(key) = std::env::var("WUAPI_API_KEY")
                .ok()
                .filter(|k| !k.is_empty())
            {
                let (engine, identity) = login.engine(ApiKey::new(key))?;
                return Ok(Launch::Chats {
                    engine,
                    session: SessionKind::Environment,
                    // Kept for the day the key stops working: the sign-in
                    // is where the user is taken then.
                    login: Some(login),
                    identity: Some(identity),
                    storage_note,
                });
            }
            let stored = if options.no_keychain {
                Ok(None)
            } else if options.relogin {
                provider_wuapi::delete_api_key(&login.service, &login.profile).map(|()| None)
            } else {
                provider_wuapi::load_api_key(&login.service, &login.profile)
            };
            match stored {
                Ok(Some(key)) => {
                    let (engine, identity) = login.engine(key)?;
                    Ok(Launch::Chats {
                        engine,
                        session: SessionKind::Keychain,
                        login: Some(login),
                        identity: Some(identity),
                        storage_note,
                    })
                }
                Ok(None) => Ok(Launch::Login {
                    flow: login,
                    keychain_error: None,
                    storage_note,
                }),
                Err(error) => {
                    tracing::warn!(%error, "the OS keychain is not available");
                    Ok(Launch::Login {
                        flow: login,
                        keychain_error: Some(error.to_string()),
                        storage_note,
                    })
                }
            }
        }
    }
}

/// Opens the store and starts an engine on `provider`.
fn start_engine(
    provider: Arc<dyn Provider>,
    storage: &Storage,
    history: HistoryMode,
    runtime: &Handle,
) -> Result<SyncEngine, String> {
    let store = storage.open()?;
    let engine = SyncEngine::new(
        Arc::new(store),
        provider,
        SyncConfig {
            history,
            ..SyncConfig::default()
        },
        runtime.clone(),
    );
    engine.start();
    Ok(engine)
}

/// Who a stored key belongs to: `GET /v1/me`, asked on the Tokio runtime
/// each time the settings screen wants to know.
fn identity_of(provider: Arc<WuapiProvider>, runtime: Handle) -> IdentityLoader {
    Arc::new(move || {
        let provider = provider.clone();
        let task = runtime.spawn(async move {
            let me = provider.me().await.map_err(|error| error.to_string())?;
            Ok(Identity {
                organization: me.organization.name,
                project: me.project.map(|project| project.name),
                key_prefix: Some(me.api_key.key_prefix),
            })
        });
        Box::pin(async move { task.await.unwrap_or_else(|error| Err(error.to_string())) })
    })
}

/// The keychain profile of the production API. One for now; several wuapi
/// organizations side by side would each get their own.
const WUAPI_PROFILE: &str = "default";

/// The wuapi sign-in: the device flow of `provider-wuapi`, the OS keychain
/// and the session that follows. All its network and keychain work runs on
/// the Tokio runtime, never on the UI thread.
pub struct WuapiLogin {
    config: WuapiConfig,
    device: Arc<DeviceLogin>,
    /// Where the session's data goes: the encrypted file, or memory.
    /// Replaced when the data is wiped and a new database begins.
    storage: Arc<Mutex<Storage>>,
    /// The database on disk, when there is one.
    home: Option<Database>,
    /// For the settings as they are when a session starts.
    options: Options,
    runtime: Handle,
    /// The keychain service: the application.
    service: String,
    /// The keychain profile: which API the key is for.
    profile: String,
    /// `--no-keychain`: the key is neither read from nor written to the OS
    /// keychain.
    use_keychain: bool,
}

impl WuapiLogin {
    fn new(
        options: &Options,
        storage: Storage,
        home: Option<Database>,
        runtime: &Handle,
    ) -> Result<Self, String> {
        let (config, profile) = wuapi_config(options);
        Ok(Self {
            device: Arc::new(DeviceLogin::new(&config).map_err(|e| e.to_string())?),
            config,
            storage: Arc::new(Mutex::new(storage)),
            home,
            options: options.clone(),
            runtime: runtime.clone(),
            service: product::slug(),
            profile,
            use_keychain: !options.no_keychain,
        })
    }

    /// An engine on wuapi with this key. Does not touch the network.
    fn engine(&self, key: ApiKey) -> Result<(SyncEngine, IdentityLoader), String> {
        let provider =
            Arc::new(WuapiProvider::new(self.config.clone(), key).map_err(|e| e.to_string())?);
        let storage = self.storage.lock().expect("storage lock").clone();
        let history = saved_settings(&self.options).history_mode();
        let engine = start_engine(provider.clone(), &storage, history, &self.runtime)?;
        Ok((engine, identity_of(provider, self.runtime.clone())))
    }

    /// Runs `work` on the Tokio runtime and hands its result to whoever
    /// awaits, on any executor.
    fn off_thread<T: Send + 'static>(
        &self,
        failed: fn(String) -> T,
        work: impl Future<Output = T> + Send + 'static,
    ) -> BoxFuture<T> {
        let task = self.runtime.spawn(work);
        Box::pin(async move { task.await.unwrap_or_else(|error| failed(error.to_string())) })
    }
}

impl LoginFlow for WuapiLogin {
    fn request_code(&self) -> BoxFuture<Result<DeviceCode, LoginError>> {
        let device = self.device.clone();
        let name = product::client_name();
        self.off_thread(|e| Err(LoginError::Unexpected(e)), async move {
            device.request_code(&name).await
        })
    }

    fn poll(&self, code: &DeviceCode) -> BoxFuture<Result<PollOutcome, LoginError>> {
        let (device, code) = (self.device.clone(), code.clone());
        self.off_thread(|e| Err(LoginError::Unexpected(e)), async move {
            device.poll_once(&code).await
        })
    }

    fn open_session(&self, grant: TokenGrant) -> BoxFuture<Result<Session, String>> {
        let config = self.config.clone();
        let storage = self.storage.clone();
        let home = self.home.clone();
        let history = saved_settings(&self.options).history_mode();
        let runtime = self.runtime.clone();
        let (service, profile) = (self.service.clone(), self.profile.clone());
        let use_keychain = self.use_keychain;
        self.off_thread(Err, async move {
            let key = grant.api_key;
            let provider =
                Arc::new(WuapiProvider::new(config, key.clone()).map_err(|e| e.to_string())?);
            // The first look at the accounts doubles as the check that the
            // new key works. Weather is not a reason to stay on the sign-in
            // screen: the engine keeps trying.
            let accounts = match provider.list_accounts().await {
                Ok(accounts) => accounts,
                Err(ProviderError::Unauthorized(reason)) => {
                    return Err(format!("wuapi did not accept the new API key: {reason}"))
                }
                Err(error) => {
                    tracing::debug!(%error, "the accounts are not available yet");
                    Vec::new()
                }
            };
            let key_saved = if use_keychain {
                // The keychain blocks (D-Bus on Linux).
                let stored = tokio::task::spawn_blocking(move || {
                    provider_wuapi::store_api_key(&service, &profile, &key)
                        .map_err(|e| e.to_string())
                })
                .await
                .unwrap_or_else(|error| Err(error.to_string()));
                match stored {
                    Ok(()) => KeySaved::Yes,
                    Err(error) => {
                        tracing::warn!(%error, "the API key could not be saved to the OS keychain");
                        KeySaved::No(error)
                    }
                }
            } else {
                KeySaved::NotAsked
            };

            // Whose chats are these? Data left by another organization (or
            // another project's key) is deleted before this one syncs: it
            // is not theirs to read, and its numbers are not theirs.
            let owner = format!(
                "{}/{}",
                grant.organization.id,
                grant
                    .project
                    .as_ref()
                    .map_or("-", |project| project.id.as_str())
            );
            if let Some(home) = &home {
                if storage::belongs_to_another(&home.path, &owner) {
                    tracing::info!("the local data belongs to another account; deleting it");
                    *storage.lock().expect("storage lock") = start_over(home);
                }
            }
            let storage = storage.lock().expect("storage lock").clone();
            let store = storage.open()?;
            if let (Some(home), Storage::Encrypted { .. }) = (&home, &storage) {
                storage::set_owner(&home.path, &owner);
            }
            if !accounts.is_empty() {
                store
                    .upsert_accounts(provider.id(), &accounts)
                    .map_err(|error| format!("cannot write the local database: {error}"))?;
            }
            // The grant already says who signed in: no need to ask again.
            let identity = Identity {
                organization: grant.organization.name,
                project: grant.project.map(|project| project.name),
                key_prefix: grant.key_prefix,
            };
            let config = SyncConfig {
                history,
                ..SyncConfig::default()
            };
            let engine = SyncEngine::new(Arc::new(store), provider, config, runtime);
            engine.start();
            Ok(Session {
                engine,
                storage_note: storage.note(),
                identity: Some(Arc::new(move || {
                    Box::pin(std::future::ready(Ok(identity.clone())))
                })),
                key_saved,
            })
        })
    }

    fn sign_out(&self) -> BoxFuture<Result<(), String>> {
        let forgotten = self.forget_key();
        let (storage, home) = (self.storage.clone(), self.home.clone());
        let task = self.runtime.spawn(async move {
            let key = forgotten.await;
            let data = tokio::task::spawn_blocking(move || match &home {
                Some(home) => {
                    let wiped = storage::wipe(&home.path, &home.name, home.vault.as_deref());
                    // The next sign-in starts from an empty database.
                    *storage.lock().expect("storage lock") = start_over(home);
                    wiped
                }
                // Nothing on disk: the session's memory goes with it.
                None => Ok(()),
            })
            .await
            .unwrap_or_else(|error| Err(error.to_string()));
            match (key, data) {
                (Ok(()), Ok(())) => Ok(()),
                (Err(key), Ok(())) => Err(format!("The API key could not be removed: {key}")),
                (Ok(()), Err(data)) => Err(format!("Some local data could not be removed: {data}")),
                (Err(key), Err(data)) => Err(format!(
                    "The API key could not be removed ({key}), nor some local data ({data})"
                )),
            }
        });
        Box::pin(async move { task.await.unwrap_or_else(|error| Err(error.to_string())) })
    }

    fn forget_key(&self) -> BoxFuture<Result<(), String>> {
        if !self.use_keychain {
            return Box::pin(std::future::ready(Ok(())));
        }
        let (service, profile) = (self.service.clone(), self.profile.clone());
        let task = self.runtime.spawn_blocking(move || {
            provider_wuapi::delete_api_key(&service, &profile).map_err(|e| e.to_string())
        });
        Box::pin(async move { task.await.unwrap_or_else(|error| Err(error.to_string())) })
    }
}

/// A new, empty database where `home`'s was: what is there is deleted,
/// with its key, and a new key is made.
fn start_over(home: &Database) -> Storage {
    if let Err(error) = storage::wipe(&home.path, &home.name, home.vault.as_deref()) {
        tracing::warn!(%error, "some local data could not be removed");
    }
    match storage::prepare(&home.path, &home.name, home.vault.clone()) {
        Prepared::Ready(storage) => storage,
        // Cannot be: the file was just deleted. If it could not be, there
        // is nothing safe to open on disk.
        Prepared::KeyLost(_) => Storage::Memory {
            note: Some(
                "Chats are kept in memory for this session and not saved to disk. The old \
                 local database could not be removed."
                    .to_owned(),
            ),
        },
    }
}

/// The database file for a provider inside `dir`, creating the directory.
fn database_in(dir: &Path, provider: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    Ok(dir.join(format!("{provider}.db")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use client_provider::{
        AccountId, ChatId, ClientMessageId, OutgoingContent, OutgoingMessage, Timestamp,
    };

    fn parsed(args: &[&str]) -> Options {
        match crate::cli::parse(args.iter().map(|a| a.to_string())) {
            Ok(crate::cli::Command::Run(options)) => options,
            _ => panic!("the arguments parse"),
        }
    }

    #[test]
    fn wuapi_config_built_once_sets_base_stream_live() {
        let (config, profile) = wuapi_config(&parsed(&[
            "--api-url=http://127.0.0.1:8787/",
            "--stream-url=http://127.0.0.1:9100",
            "--live=stream",
        ]));
        // The address without its trailing slash, and the key kept apart for
        // that API.
        assert_eq!(config.base_url, "http://127.0.0.1:8787");
        assert_eq!(profile, "http://127.0.0.1:8787");
        assert_eq!(config.stream_url.as_deref(), Some("http://127.0.0.1:9100"));
        assert_eq!(config.live, provider_wuapi::LiveTransport::Stream);

        // Nothing asked: the adapter's own defaults, and the production
        // profile.
        let (config, profile) = wuapi_config(&parsed(&[]));
        let default = WuapiConfig::new(product::user_agent());
        assert_eq!(config.base_url, default.base_url);
        assert_eq!(config.live, default.live);
        assert_eq!(config.stream_url, None);
        assert_eq!(profile, WUAPI_PROFILE);

        // Auto with the production API: the API's own address stays.
        let (config, _) = wuapi_config(&parsed(&["--live=auto"]));
        assert_eq!(config.live, provider_wuapi::LiveTransport::Auto);
        assert_eq!(config.base_url, default.base_url);
    }

    fn options(dir: &Path) -> Options {
        let arguments = [
            "--provider=wuapi".to_owned(),
            format!("--data-dir={}", dir.display()),
            "--diagnose=sends".to_owned(),
        ];
        match crate::cli::parse(arguments.into_iter()) {
            Ok(crate::cli::Command::Run(options)) => options,
            _ => panic!("the arguments parse"),
        }
    }

    #[test]
    fn the_sends_report_reads_the_database_and_says_nothing_of_the_messages() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            diagnose_sends(&options(dir.path())).is_err(),
            "no database yet"
        );
        {
            let store = client_core::Store::open(dir.path().join("wuapi.db"), None).unwrap();
            store
                .enqueue(
                    &OutgoingMessage {
                        client_id: ClientMessageId::new("0190-secret-tail"),
                        account_id: AccountId::new("acc"),
                        chat_id: ChatId::new("+584245550199"),
                        content: OutgoingContent::Text {
                            body: "Esta bien.".into(),
                        },
                        reply_to: None,
                        mentions: Vec::new(),
                        forwarded: false,
                    },
                    Timestamp::now(),
                    std::time::Duration::from_secs(60),
                )
                .unwrap();
        }
        let lines = diagnose_sends(&options(dir.path())).unwrap();
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert!(lines[0].starts_with("sends 1 "));
        assert!(lines[1].starts_with("send ..tail kind=text "), "{lines:?}");
        assert!(lines[1].contains("accepted=-") && lines[1].contains("delivered=-"));
        let report = lines.join("\n");
        for private in ["Esta bien", "584245550199", "secret"] {
            assert!(!report.contains(private), "{report}");
        }
        // A database from before sends were timed says so.
        assert!(sends_report(None)[0].contains("did not time sends"));
    }
}
