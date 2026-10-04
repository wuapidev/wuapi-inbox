//! Moving an install made under the product's working name to its name.
//!
//! Before it was "wuapi Inbox" the application kept its data in a
//! directory and a keychain service named after its working title. On the
//! first start under the new name, if the new directory does not exist and
//! the old one does, everything moves: the directory (databases, their
//! journals, `settings.json`) and the keychain entries (the API key, and
//! each database's key, which is filed under the database's path and so
//! has to be filed again under the new one).
//!
//! The order is what keeps an encrypted database openable whatever goes
//! wrong: the new keychain entries are written first, then the directory
//! is renamed (one atomic step), and only then are the old entries
//! deleted. A failure before the rename undoes the new entries and leaves
//! the old install exactly as it was; a failure after it leaves stale old
//! entries, which nothing reads.

use crate::storage;
use client_core::FileState;
use std::path::Path;

/// A keychain, reduced to what the move needs.
pub trait Secrets {
    /// The secret filed under `service` and `user`, if there is one.
    fn get(&self, service: &str, user: &str) -> Result<Option<String>, String>;
    /// Files a secret, replacing any filed before.
    fn set(&self, service: &str, user: &str, secret: &str) -> Result<(), String>;
    /// Removes a secret. Not having one is not an error.
    fn delete(&self, service: &str, user: &str) -> Result<(), String>;
}

/// The OS keychain.
pub struct OsSecrets;

impl Secrets for OsSecrets {
    fn get(&self, service: &str, user: &str) -> Result<Option<String>, String> {
        match storage::keyring_entry(service, user)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    fn set(&self, service: &str, user: &str, secret: &str) -> Result<(), String> {
        storage::keyring_entry(service, user)?
            .set_password(secret)
            .map_err(|error| error.to_string())
    }

    fn delete(&self, service: &str, user: &str) -> Result<(), String> {
        match storage::keyring_entry(service, user)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }
}

/// What became of the move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// There was nothing to move, or it was moved before.
    NothingToDo,
    /// The old install is now the new one.
    Moved,
    /// Not this time, with the reason; the old install is untouched.
    NotMoved(String),
}

/// The keychain user of the wuapi API key of the production API.
const API_KEY_USER: &str = "wuapi:default";

/// Moves the install in `old_dir` (keychain service `old_service`) to
/// `new_dir` (`new_service`), if there is one to move and no new one yet.
pub fn migrate(
    old_dir: &Path,
    new_dir: &Path,
    old_service: &str,
    new_service: &str,
    secrets: &dyn Secrets,
) -> Outcome {
    if new_dir.exists() || !old_dir.is_dir() {
        return Outcome::NothingToDo;
    }
    let databases: Vec<_> = match std::fs::read_dir(old_dir) {
        Ok(entries) => entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "db"))
            .collect(),
        Err(error) => return Outcome::NotMoved(error.to_string()),
    };

    // What has to be filed again: (user under the old service, user under
    // the new one, the secret).
    let mut refile = Vec::new();
    for database in &databases {
        let Some(file) = database.file_name() else {
            continue;
        };
        let old_user = storage::key_user(&storage::key_name(database));
        let new_user = storage::key_user(&storage::key_name(&new_dir.join(file)));
        match secrets.get(old_service, &old_user) {
            Ok(Some(key)) => refile.push((old_user, new_user, key)),
            Ok(None) => {}
            // Without its key an encrypted database must not move: under
            // its new path nobody would find the key again.
            Err(error) => {
                if !matches!(client_core::file_state(database), Ok(FileState::Plain)) {
                    return Outcome::NotMoved(format!("the keychain is not available: {error}"));
                }
            }
        }
    }
    // No key, or no keychain: there is no session to carry over.
    if let Ok(Some(key)) = secrets.get(old_service, API_KEY_USER) {
        refile.push((API_KEY_USER.to_owned(), API_KEY_USER.to_owned(), key));
    }

    let undo = |filed: &[&(String, String, String)]| {
        for (_, new_user, _) in filed {
            let _ = secrets.delete(new_service, new_user);
        }
    };
    let mut filed = Vec::new();
    for entry in &refile {
        if let Err(error) = secrets.set(new_service, &entry.1, &entry.2) {
            undo(&filed);
            return Outcome::NotMoved(format!("the keychain refused an entry: {error}"));
        }
        filed.push(entry);
    }
    let moved = new_dir
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::rename(old_dir, new_dir));
    if let Err(error) = moved {
        undo(&filed);
        return Outcome::NotMoved(error.to_string());
    }
    for (old_user, _, _) in &refile {
        let _ = secrets.delete(old_service, old_user);
    }
    Outcome::Moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::tests::fill;
    use crate::storage::{prepare, KeyVault, Prepared, Storage};
    use client_core::StoreKey;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// A keychain in a map. `broken` makes every call fail; `full` makes
    /// writes fail.
    #[derive(Default)]
    struct FakeSecrets {
        entries: Mutex<HashMap<(String, String), String>>,
        broken: bool,
        full: bool,
    }

    impl Secrets for FakeSecrets {
        fn get(&self, service: &str, user: &str) -> Result<Option<String>, String> {
            if self.broken {
                return Err("no secret service".into());
            }
            let key = (service.to_owned(), user.to_owned());
            Ok(self.entries.lock().unwrap().get(&key).cloned())
        }
        fn set(&self, service: &str, user: &str, secret: &str) -> Result<(), String> {
            if self.broken || self.full {
                return Err("no secret service".into());
            }
            let key = (service.to_owned(), user.to_owned());
            self.entries.lock().unwrap().insert(key, secret.to_owned());
            Ok(())
        }
        fn delete(&self, service: &str, user: &str) -> Result<(), String> {
            let key = (service.to_owned(), user.to_owned());
            self.entries.lock().unwrap().remove(&key);
            Ok(())
        }
    }

    /// The vault of one service of a [`FakeSecrets`], as the application
    /// sees its keychain.
    struct ServiceVault(Arc<FakeSecrets>, &'static str);

    impl KeyVault for ServiceVault {
        fn load(&self, name: &str) -> Result<Option<StoreKey>, String> {
            Ok(self
                .0
                .get(self.1, &storage::key_user(name))?
                .and_then(|hex| StoreKey::from_hex(&hex)))
        }
        fn save(&self, name: &str, key: &StoreKey) -> Result<(), String> {
            self.0.set(self.1, &storage::key_user(name), &key.to_hex())
        }
        fn delete(&self, name: &str) -> Result<(), String> {
            self.0.delete(self.1, &storage::key_user(name))
        }
    }

    /// An install under the old name: an encrypted database with a
    /// message, settings, and an API key.
    fn old_install(root: &Path, secrets: &Arc<FakeSecrets>) -> std::path::PathBuf {
        let old = root.join("fastwhatsapp");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.json"), r#"{"theme":"dark"}"#).unwrap();
        let database = old.join("wuapi.db");
        let vault: Arc<dyn KeyVault> = Arc::new(ServiceVault(secrets.clone(), "fastwhatsapp"));
        let Prepared::Ready(storage) =
            prepare(&database, &storage::key_name(&database), Some(vault))
        else {
            panic!("a new database was reported lost");
        };
        fill(&storage.open().unwrap());
        secrets
            .set("fastwhatsapp", API_KEY_USER, "wu_live_secret")
            .unwrap();
        old
    }

    #[test]
    fn an_install_under_the_old_name_keeps_its_session_and_its_database() {
        let root = tempfile::tempdir().unwrap();
        let secrets = Arc::new(FakeSecrets::default());
        let old = old_install(root.path(), &secrets);
        let new = root.path().join("wuapi-inbox");

        let outcome = migrate(&old, &new, "fastwhatsapp", "wuapi-inbox", secrets.as_ref());
        assert_eq!(outcome, Outcome::Moved);
        assert!(!old.exists() && new.join("wuapi.db").exists());
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            r#"{"theme":"dark"}"#
        );

        // The session came along, and nothing is left under the old name.
        assert_eq!(
            secrets.get("wuapi-inbox", API_KEY_USER).unwrap().as_deref(),
            Some("wu_live_secret")
        );
        assert!(secrets
            .entries
            .lock()
            .unwrap()
            .keys()
            .all(|(service, _)| service == "wuapi-inbox"));

        // And the database opens where it now is, with what was in it:
        // its key was filed again under its new path.
        let database = new.join("wuapi.db");
        let vault: Arc<dyn KeyVault> = Arc::new(ServiceVault(secrets.clone(), "wuapi-inbox"));
        let Prepared::Ready(storage) =
            prepare(&database, &storage::key_name(&database), Some(vault))
        else {
            panic!("the moved database lost its key");
        };
        assert!(matches!(storage, Storage::Encrypted { .. }));
        let store = storage.open().unwrap();
        let hits = store
            .search_messages(&client_provider::AccountId::new("acc"), "keyboard", 5)
            .unwrap();
        assert_eq!(hits.len(), 1);

        // A second start has nothing to do.
        let again = migrate(&old, &new, "fastwhatsapp", "wuapi-inbox", secrets.as_ref());
        assert_eq!(again, Outcome::NothingToDo);
    }

    #[test]
    fn a_new_install_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let secrets = Arc::new(FakeSecrets::default());
        let old = old_install(root.path(), &secrets);
        let new = root.path().join("wuapi-inbox");
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("settings.json"), "{}").unwrap();

        let outcome = migrate(&old, &new, "fastwhatsapp", "wuapi-inbox", secrets.as_ref());
        assert_eq!(outcome, Outcome::NothingToDo);
        assert!(old.join("wuapi.db").exists());
        assert_eq!(
            std::fs::read_to_string(new.join("settings.json")).unwrap(),
            "{}"
        );
    }

    #[test]
    fn a_move_that_cannot_keep_the_key_does_not_happen() {
        // The keychain takes no new entries: the encrypted database stays
        // where its key still opens it.
        let root = tempfile::tempdir().unwrap();
        let working = Arc::new(FakeSecrets::default());
        let old = old_install(root.path(), &working);
        let new = root.path().join("wuapi-inbox");
        let before = working.entries.lock().unwrap().clone();
        let full = FakeSecrets {
            entries: Mutex::new(before.clone()),
            full: true,
            ..Default::default()
        };
        let outcome = migrate(&old, &new, "fastwhatsapp", "wuapi-inbox", &full);
        assert!(matches!(outcome, Outcome::NotMoved(_)), "{outcome:?}");
        assert!(old.join("wuapi.db").exists() && !new.exists());
        assert_eq!(*full.entries.lock().unwrap(), before, "nothing half-filed");

        // The keychain cannot be read at all: same.
        let broken = FakeSecrets {
            broken: true,
            ..Default::default()
        };
        let outcome = migrate(&old, &new, "fastwhatsapp", "wuapi-inbox", &broken);
        assert!(matches!(outcome, Outcome::NotMoved(_)));
        assert!(old.join("wuapi.db").exists() && !new.exists());

        // An install with nothing encrypted (settings only) moves even so.
        let plain = root.path().join("plain-old");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("settings.json"), "{}").unwrap();
        let target = root.path().join("plain-new");
        assert_eq!(
            migrate(&plain, &target, "fastwhatsapp", "wuapi-inbox", &broken),
            Outcome::Moved
        );
        assert!(target.join("settings.json").exists());
    }
}
