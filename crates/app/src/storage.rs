//! Where a session's local database lives, and the key that opens it.
//!
//! The database on disk is encrypted (SQLCipher, see `client-core`). Its
//! key is 32 random bytes made on first run and kept in the OS keychain,
//! one per provider database, next to the API key but separate from it.
//! The key is never logged and never written anywhere else.
//!
//! [`prepare`] decides, before any window opens, what this start can use:
//!
//! * the encrypted file, creating it or converting a plain one left by an
//!   earlier version (the plain file is only replaced once the encrypted
//!   copy is complete; if converting fails it is left untouched);
//! * an in-memory database for this session, with the reason to show on
//!   screen, when there is no keychain to hold a key. A database is never
//!   written to disk in clear because the keychain is missing: that would
//!   turn "your chats are encrypted" into something true only on some
//!   machines, silently. Refusing to start was the alternative; keeping the
//!   application usable and saying plainly that nothing is saved seemed
//!   the better trade for a client whose local data is a cache;
//! * nothing yet: the file is encrypted and its key is gone ([`Recovery`]).
//!   The database is a cache of what the provider has, so the way out is
//!   to delete it and sync again, which the person is asked to confirm
//!   because messages still waiting to be sent go with it.

use client_core::{FileState, Store, StoreKey};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Somewhere to keep database keys. The OS keychain in the application; a
/// map in tests.
pub trait KeyVault: Send + Sync {
    /// The key stored under `name`, or `None` if there is none.
    fn load(&self, name: &str) -> Result<Option<StoreKey>, String>;
    /// Stores `key` under `name`, replacing any key stored before.
    fn save(&self, name: &str, key: &StoreKey) -> Result<(), String>;
    /// Forgets the key under `name`. Not having one is not an error.
    fn delete(&self, name: &str) -> Result<(), String>;
}

/// The name a database's key is filed under: the file's full path, so two
/// data directories never share (or overwrite) a key. Symbolic links are
/// resolved in the part of the path that exists, which makes the name the
/// same before the file is created and after.
pub fn key_name(path: &Path) -> String {
    let mut missing = Vec::new();
    let mut existing = path;
    let resolved = loop {
        if let Ok(resolved) = std::fs::canonicalize(existing) {
            break resolved;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name);
                existing = parent;
            }
            _ => break existing.to_owned(),
        }
    };
    missing
        .into_iter()
        .rev()
        .fold(resolved, |path, name| path.join(name))
        .to_string_lossy()
        .into_owned()
}

/// The keychain user of the key named `name`.
pub fn key_user(name: &str) -> String {
    format!("database:{name}")
}

/// An entry of the OS keychain, with the store's own reason when the
/// keychain itself is not there.
pub fn keyring_entry(service: &str, user: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, user).map_err(|error| {
        match (&error, keyring::Entry::store_status()) {
            (keyring::Error::NoDefaultStore, Err(cause)) => cause.to_string(),
            _ => error.to_string(),
        }
    })
}

/// The OS keychain: one entry per database, filed under the application.
pub struct OsKeychain {
    /// The keychain service: the application's name.
    pub service: String,
}

impl OsKeychain {
    fn entry(&self, name: &str) -> Result<keyring::Entry, String> {
        keyring_entry(&self.service, &key_user(name))
    }
}

impl KeyVault for OsKeychain {
    fn load(&self, name: &str) -> Result<Option<StoreKey>, String> {
        match self.entry(name)?.get_password() {
            Ok(hex) => StoreKey::from_hex(&hex)
                .map(Some)
                .ok_or_else(|| "the stored database key is not readable".to_owned()),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn save(&self, name: &str, key: &StoreKey) -> Result<(), String> {
        self.entry(name)?
            .set_password(&key.to_hex())
            .map_err(|error| error.to_string())
    }

    fn delete(&self, name: &str) -> Result<(), String> {
        match self.entry(name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// The file that says whose data a database holds.
fn owner_file(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".owner");
    PathBuf::from(name)
}

/// Whose data the database at `path` holds: the organization (and project)
/// that was signed in when it was filled. `None` when nobody wrote it down
/// (a database from before this was recorded).
pub fn owner(path: &Path) -> Option<String> {
    std::fs::read_to_string(owner_file(path))
        .ok()
        .map(|owner| owner.trim().to_owned())
        .filter(|owner| !owner.is_empty())
}

/// True when the database at `path` was filled by someone other than
/// `signing_in`. A database nobody signed is taken to be theirs: it comes
/// from a version that did not write this down, on a computer where the
/// same person has been signing in.
pub fn belongs_to_another(path: &Path, signing_in: &str) -> bool {
    owner(path).is_some_and(|previous| previous != signing_in)
}

/// Records whose data the database at `path` holds. Ids, not secrets.
pub fn set_owner(path: &Path, owner: &str) {
    if let Err(error) = std::fs::write(owner_file(path), owner) {
        tracing::warn!(%error, "could not record whose data the database holds");
    }
}

/// Deletes a database for good: the file, its journals, and its key. What
/// "Sign out" does to the chats on this computer. Returns what could not
/// be removed, if anything.
pub fn wipe(path: &Path, name: &str, vault: Option<&dyn KeyVault>) -> Result<(), String> {
    let mut failures = Vec::new();
    for file in files_of(path).into_iter().chain([owner_file(path)]) {
        if let Err(error) = std::fs::remove_file(&file) {
            if error.kind() != std::io::ErrorKind::NotFound {
                failures.push(format!("{}: {error}", file.display()));
            }
        }
    }
    if let Some(vault) = vault {
        if let Err(error) = vault.delete(name) {
            failures.push(format!("the database key: {error}"));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

/// What a session stores its data in.
#[derive(Clone)]
pub enum Storage {
    /// The encrypted file.
    Encrypted {
        /// The database file.
        path: PathBuf,
        /// Its key.
        key: StoreKey,
    },
    /// Nothing on disk: the data lasts for the session.
    Memory {
        /// Why, for the screen. `None` when it is simply what was asked
        /// for (the demo data without `--data-dir`).
        note: Option<String>,
    },
}

impl Storage {
    /// Opens the store.
    pub fn open(&self) -> Result<Store, String> {
        match self {
            Self::Encrypted { path, key } => Store::open(path, Some(key)),
            Self::Memory { .. } => Store::open_in_memory(),
        }
        .map_err(|error| match error {
            client_core::StoreError::NewerSchema { found, .. } => newer_database(found),
            error => format!("cannot open the local database: {error}"),
        })
    }

    /// What to say on screen about where the chats are kept, if anything
    /// needs saying.
    pub fn note(&self) -> Option<String> {
        match self {
            Self::Encrypted { .. } => None,
            Self::Memory { note } => note.clone(),
        }
    }
}

/// What [`prepare`] found.
pub enum Prepared {
    /// Ready to open.
    Ready(Storage),
    /// The file is encrypted and its key is gone.
    KeyLost(Recovery),
}

/// An encrypted database whose key is no longer in the keychain.
#[derive(Clone)]
pub struct Recovery {
    path: PathBuf,
    name: String,
    vault: Arc<dyn KeyVault>,
}

impl Recovery {
    /// Deletes the database and starts a new one under a new key. What was
    /// in it comes back from the provider, except messages that were still
    /// waiting to be sent.
    pub fn reset(&self) -> Storage {
        for file in files_of(&self.path) {
            if let Err(error) = std::fs::remove_file(&file) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return memory(format!(
                        "The old local database could not be removed ({error})."
                    ));
                }
            }
        }
        with_new_key(&self.path, &self.name, self.vault.as_ref())
    }
}

/// Why nothing is saved without a keychain: the message under every such
/// note.
const NOT_SAVED: &str = "Chats are kept in memory for this session and not saved to disk";

fn memory(reason: String) -> Storage {
    Storage::Memory {
        note: Some(format!("{NOT_SAVED}. {reason}")),
    }
}

/// The database file and the files SQLite keeps next to it.
fn files_of(path: &Path) -> [PathBuf; 3] {
    let beside = |suffix: &str| {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    [path.to_owned(), beside("-wal"), beside("-shm")]
}

/// 32 bytes from the operating system's generator.
fn new_key() -> Result<StoreKey, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| error.to_string())?;
    Ok(StoreKey::from_bytes(bytes))
}

/// Makes a key, puts it in the vault, and only then lets it be used.
fn with_new_key(path: &Path, name: &str, vault: &dyn KeyVault) -> Storage {
    let key = match new_key() {
        Ok(key) => key,
        Err(error) => return memory(format!("No database key could be made ({error}).")),
    };
    match vault.save(name, &key) {
        Ok(()) => with_key(path, key),
        Err(error) => memory(format!(
            "The system keychain could not hold the database key ({error})."
        )),
    }
}

/// The encrypted database under `key`, converting a plain file if that is
/// what is on disk.
fn with_key(path: &Path, key: StoreKey) -> Storage {
    if matches!(client_core::file_state(path), Ok(FileState::Plain)) {
        if let Err(error) = Store::encrypt_plain(path, &key) {
            tracing::warn!(%error, "the existing database could not be encrypted");
            return memory(format!(
                "The existing database could not be encrypted and was left untouched ({error})."
            ));
        }
    }
    // A database a newer version of the application wrote is never opened
    // (migrations only go forward) and never touched: the session runs in
    // memory and says why.
    if matches!(client_core::file_state(path), Ok(FileState::Encrypted)) {
        if let Ok(found) = Store::schema_of(path, Some(&key)) {
            if found > client_core::SCHEMA_VERSION {
                tracing::warn!(
                    found,
                    supported = client_core::SCHEMA_VERSION,
                    "the local database is from a newer version; it is left untouched"
                );
                return memory(newer_database(found));
            }
        }
    }
    Storage::Encrypted {
        path: path.to_owned(),
        key,
    }
}

/// What is said when the database on disk is from a newer version of the
/// application than the one running.
pub fn newer_database(found: u32) -> String {
    format!(
        "The chats saved on this computer were written by a newer version of {name} \
         (database schema {found}; this version reads up to schema {supported}). They \
         were left untouched: update {name} to open them.",
        name = crate::product::PRODUCT_NAME,
        supported = client_core::SCHEMA_VERSION,
    )
}

/// Works out what the database at `path` can be opened with.
///
/// `name` tells the databases apart in the vault. `vault` is `None` when
/// the keychain is not to be used (`--no-keychain`).
pub fn prepare(path: &Path, name: &str, vault: Option<Arc<dyn KeyVault>>) -> Prepared {
    let Some(vault) = vault else {
        return Prepared::Ready(memory(
            "The keychain is turned off (--no-keychain), so there is nowhere to keep the \
             database key."
                .to_owned(),
        ));
    };
    let state = match client_core::file_state(path) {
        Ok(state) => state,
        Err(error) => {
            return Prepared::Ready(memory(format!(
                "The local database could not be read ({error})."
            )))
        }
    };
    let lost = || {
        Prepared::KeyLost(Recovery {
            path: path.to_owned(),
            name: name.to_owned(),
            vault: vault.clone(),
        })
    };
    match (vault.load(name), state) {
        (Err(error), _) => Prepared::Ready(memory(format!(
            "The system keychain is not available ({error})."
        ))),
        // Encrypted, and nothing (or the wrong thing) to open it with.
        (Ok(None), FileState::Encrypted) => lost(),
        (Ok(Some(key)), FileState::Encrypted) if !Store::key_opens(path, &key) => lost(),
        (Ok(Some(key)), _) => Prepared::Ready(with_key(path, key)),
        (Ok(None), _) => Prepared::Ready(with_new_key(path, name, vault.as_ref())),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use client_provider::{
        Account, AccountId, Chat, ChatId, ChatKind, ConnectionState, ContactId, DeliveryStatus,
        Direction, Message, MessageContent, MessageId, Timestamp,
    };
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A keychain in a map, which can be told to be out of order.
    #[derive(Default)]
    pub(crate) struct FakeVault {
        pub(crate) keys: Mutex<HashMap<String, StoreKey>>,
        pub(crate) broken: bool,
    }

    impl KeyVault for FakeVault {
        fn load(&self, name: &str) -> Result<Option<StoreKey>, String> {
            if self.broken {
                return Err("no secret service".into());
            }
            Ok(self.keys.lock().unwrap().get(name).cloned())
        }

        fn save(&self, name: &str, key: &StoreKey) -> Result<(), String> {
            if self.broken {
                return Err("no secret service".into());
            }
            self.keys
                .lock()
                .unwrap()
                .insert(name.to_owned(), key.clone());
            Ok(())
        }

        fn delete(&self, name: &str) -> Result<(), String> {
            self.keys.lock().unwrap().remove(name);
            Ok(())
        }
    }

    const SECRET: &str = "the cat sleeps on the keyboard";

    /// Writes one account, one chat and one message.
    pub(crate) fn fill(store: &Store) {
        let account = AccountId::new("acc");
        store
            .upsert_accounts(
                "test",
                &[Account {
                    id: account.clone(),
                    display_name: "Test".into(),
                    phone: None,
                    self_contact: None,
                    connection: ConnectionState::Connected,
                    settings: Default::default(),
                }],
            )
            .unwrap();
        let message = Message {
            id: MessageId::new("m1"),
            client_id: None,
            account_id: account.clone(),
            chat_id: ChatId::new("chat"),
            sender: ContactId::new("them"),
            sender_name: None,
            direction: Direction::Incoming,
            timestamp: Timestamp::from_millis(1),
            content: MessageContent::text(SECRET),
            reply_to: None,
            status: DeliveryStatus::Delivered,
            edited: false,
            deleted: false,
            extras: Default::default(),
        };
        store
            .upsert_chat(
                &Chat {
                    id: ChatId::new("chat"),
                    account_id: account,
                    kind: ChatKind::Direct,
                    title: "Them".into(),
                    avatar: None,
                    unread_count: 0,
                    pinned: false,
                    muted: false,
                    archived: false,
                    last_message: Some(message),
                    unknown: Default::default(),
                    picture_id: None,
                    pinned_at: None,
                },
                false,
            )
            .unwrap();
    }

    fn found(store: &Store) -> usize {
        store
            .search_messages(&AccountId::new("acc"), "keyboard", 10)
            .unwrap()
            .len()
    }

    fn ready(prepared: Prepared) -> Storage {
        match prepared {
            Prepared::Ready(storage) => storage,
            Prepared::KeyLost(_) => panic!("the key was reported lost"),
        }
    }

    #[test]
    fn a_first_run_makes_a_key_and_later_runs_use_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wuapi.db");
        let vault = Arc::new(FakeVault::default());

        let storage = ready(prepare(&path, "wuapi:default", Some(vault.clone())));
        assert!(storage.note().is_none());
        fill(&storage.open().unwrap());
        let key = vault.keys.lock().unwrap()["wuapi:default"].clone();

        // The next start finds the same key and the same data.
        let again = ready(prepare(&path, "wuapi:default", Some(vault.clone())));
        assert!(matches!(&again, Storage::Encrypted { key: k, .. } if *k == key));
        assert_eq!(found(&again.open().unwrap()), 1);
        // And nothing readable is on disk.
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(8).any(|w| w == b"keyboard"));

        // Each database has its own key.
        let other = dir.path().join("mock.db");
        ready(prepare(&other, "mock", Some(vault.clone())));
        let keys = vault.keys.lock().unwrap();
        assert_ne!(keys["mock"], keys["wuapi:default"]);
    }

    #[test]
    fn a_plain_database_from_an_earlier_version_is_encrypted_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wuapi.db");
        fill(&Store::open(&path, None).unwrap());
        assert!(std::fs::read(&path)
            .unwrap()
            .windows(8)
            .any(|w| w == b"keyboard"));

        let vault = Arc::new(FakeVault::default());
        let storage = ready(prepare(&path, "wuapi:default", Some(vault)));
        assert!(storage.note().is_none());
        assert_eq!(
            found(&storage.open().unwrap()),
            1,
            "the messages came along"
        );
        for file in files_of(&path) {
            if let Ok(bytes) = std::fs::read(&file) {
                assert!(
                    !bytes.windows(8).any(|w| w == b"keyboard"),
                    "{} still has the message in clear",
                    file.display()
                );
            }
        }
    }

    #[test]
    fn a_database_from_a_newer_version_is_left_untouched_and_said_so() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wuapi.db");
        let vault = Arc::new(FakeVault::default());
        let storage = ready(prepare(&path, "wuapi:default", Some(vault.clone())));
        fill(&storage.open().unwrap());
        let Storage::Encrypted { key, .. } = &storage else {
            panic!("an encrypted database");
        };
        // A newer version ran over it (and was then rolled back).
        let newer = client_core::SCHEMA_VERSION + 1;
        Store::pretend_schema(&path, Some(key), newer).unwrap();

        let again = ready(prepare(&path, "wuapi:default", Some(vault.clone())));
        assert!(
            matches!(again, Storage::Memory { .. }),
            "the session does not open the file"
        );
        let note = again.note().unwrap();
        assert!(note.contains("newer version"), "{note}");
        assert!(note.contains(&format!("schema {newer}")), "{note}");
        assert!(note.contains("left untouched"), "{note}");
        // It is not offered to be reset either: the key still opens it.
        assert!(Store::key_opens(&path, key));
        assert_eq!(Store::schema_of(&path, Some(key)).unwrap(), newer);
        // And opening it directly says the same, in the same words.
        let said = storage.open().err().unwrap();
        assert_eq!(said, newer_database(newer));

        // Once the application is the newer one again, the chats are there.
        Store::pretend_schema(&path, Some(key), client_core::SCHEMA_VERSION).unwrap();
        let back = ready(prepare(&path, "wuapi:default", Some(vault)));
        assert_eq!(found(&back.open().unwrap()), 1);
    }

    #[test]
    fn without_a_keychain_nothing_is_written_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wuapi.db");
        let broken = Arc::new(FakeVault {
            broken: true,
            ..Default::default()
        });
        for vault in [None, Some(broken as Arc<dyn KeyVault>)] {
            let storage = ready(prepare(&path, "wuapi:default", vault));
            let note = storage.note().expect("said on screen");
            assert!(note.starts_with(NOT_SAVED), "{note}");
            fill(&storage.open().unwrap());
            assert!(!path.exists(), "no file, plain or otherwise");
        }

        // A plain database that is already there is left exactly as it is.
        fill(&Store::open(&path, None).unwrap());
        let before = std::fs::read(&path).unwrap();
        let storage = ready(prepare(&path, "wuapi:default", None));
        assert!(matches!(storage, Storage::Memory { .. }));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[test]
    fn signing_out_leaves_nothing_of_the_database_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wuapi.db");
        let vault = Arc::new(FakeVault::default());
        let storage = ready(prepare(&path, "db", Some(vault.clone())));
        fill(&storage.open().unwrap());
        assert!(!belongs_to_another(&path, "org_1/-"), "nobody's yet");
        set_owner(&path, "org_1/-");
        assert_eq!(owner(&path).as_deref(), Some("org_1/-"));
        // The same account again keeps its chats; another organization, or
        // another project's key, does not get to read them.
        assert!(!belongs_to_another(&path, "org_1/-"));
        assert!(belongs_to_another(&path, "org_2/-"));
        assert!(belongs_to_another(&path, "org_1/prj_9"));
        std::fs::write(dir.path().join("settings.json"), "{}").unwrap();

        wipe(&path, "db", Some(vault.as_ref())).unwrap();
        let left: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, ["settings.json"], "the settings stay, nothing else");
        assert!(vault.keys.lock().unwrap().is_empty(), "the key is gone too");
        assert_eq!(owner(&path), None);

        // Wiping what is not there is fine, and the next sign-in starts a
        // new database under a new key.
        wipe(&path, "db", Some(vault.as_ref())).unwrap();
        let fresh = ready(prepare(&path, "db", Some(vault)));
        assert_eq!(found(&fresh.open().unwrap()), 0);
    }

    #[test]
    fn signing_out_takes_the_sticker_and_gif_library_with_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wuapi.db");
        let vault = Arc::new(FakeVault::default());
        let storage = ready(prepare(&path, "db", Some(vault.clone())));
        let store = storage.open().unwrap();
        // A favorite sticker, in the same encrypted database as everything.
        let bytes = b"RIFF....WEBPa favorite sticker's unmistakable bytes".to_vec();
        let id = client_core::content_id(&bytes);
        store
            .library_add(
                client_core::NewLibraryItem {
                    kind: client_core::LibraryKind::Sticker,
                    bytes: bytes.clone(),
                    mime: "image/webp".into(),
                    animated: false,
                    size: None,
                    source: client_core::LibrarySource::Received,
                    name: None,
                    pack: None,
                    thumb: None,
                },
                &id,
                client_provider::Timestamp::now(),
                client_core::LIBRARY_BUDGET,
            )
            .unwrap();
        store.library_set_favorite(&id, true).unwrap();
        drop(store);
        // On disk it is encrypted: the sticker is in no file in clear.
        for file in files_of(&path) {
            if let Ok(on_disk) = std::fs::read(&file) {
                assert!(
                    !on_disk.windows(24).any(|w| w == &bytes[8..32]),
                    "{} holds the sticker in clear",
                    file.display()
                );
            }
        }
        wipe(&path, "db", Some(vault.as_ref())).unwrap();
        assert!(files_of(&path).iter().all(|file| !file.exists()));
        // The next sign-in starts with an empty library.
        let fresh = ready(prepare(&path, "db", Some(vault)));
        let store = fresh.open().unwrap();
        assert_eq!(store.library_stats().unwrap().items, 0);
        assert!(store.library_item(&id).unwrap().is_none());
    }

    #[test]
    fn a_lost_key_offers_a_reset_instead_of_failing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wuapi.db");
        let vault = Arc::new(FakeVault::default());
        fill(
            &ready(prepare(&path, "wuapi:default", Some(vault.clone())))
                .open()
                .unwrap(),
        );

        // The keychain was wiped. Or holds a key that is not this file's.
        for wrong in [None, Some(StoreKey::from_bytes([7; 32]))] {
            {
                let mut keys = vault.keys.lock().unwrap();
                keys.clear();
                keys.extend(wrong.map(|key| ("wuapi:default".to_owned(), key)));
            }
            let Prepared::KeyLost(recovery) = prepare(&path, "wuapi:default", Some(vault.clone()))
            else {
                panic!("an unreadable database was reported ready");
            };
            assert!(
                path.exists(),
                "nothing is deleted before the person says so"
            );

            let fresh = recovery.reset();
            assert!(fresh.note().is_none());
            let store = fresh.open().unwrap();
            assert_eq!(found(&store), 0, "a new, empty database");
            fill(&store);
        }
    }
}
