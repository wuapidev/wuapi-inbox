//! API keys in the operating system's keychain.
//!
//! macOS Keychain, Windows Credential Manager, or the Secret Service on
//! Linux, through the `keyring` crate. The key is never written to a file.
//!
//! These functions block (the Secret Service is reached over D-Bus). From
//! async code, call them through `tokio::task::spawn_blocking`.

use crate::config::ApiKey;

/// The keychain could not be used.
#[derive(Debug, thiserror::Error)]
#[error("keychain error: {0}")]
pub struct KeychainError(String);

fn entry(service: &str, profile: &str) -> Result<keyring::Entry, KeychainError> {
    keyring::Entry::new(service, &format!("wuapi:{profile}")).map_err(|error| {
        // "No default store" only says that the platform's store could not
        // be set up; why is in the store's own status (no Secret Service on
        // the session bus, for one).
        match (&error, keyring::Entry::store_status()) {
            (keyring::Error::NoDefaultStore, Err(cause)) => KeychainError(cause.to_string()),
            _ => KeychainError(error.to_string()),
        }
    })
}

/// Saves `key` for `profile`, replacing any key stored before.
///
/// `service` is the name the entry is filed under, normally the
/// application's name; `profile` tells several wuapi logins apart.
pub fn store_api_key(service: &str, profile: &str, key: &ApiKey) -> Result<(), KeychainError> {
    entry(service, profile)?
        .set_password(key.expose())
        .map_err(|e| KeychainError(e.to_string()))
}

/// The key stored for `profile`, or `None` if there is none.
pub fn load_api_key(service: &str, profile: &str) -> Result<Option<ApiKey>, KeychainError> {
    match entry(service, profile)?.get_password() {
        Ok(key) => Ok(Some(ApiKey::new(key))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(KeychainError(e.to_string())),
    }
}

/// Forgets the key stored for `profile`. Not having one is not an error.
pub fn delete_api_key(service: &str, profile: &str) -> Result<(), KeychainError> {
    match entry(service, profile)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(KeychainError(e.to_string())),
    }
}
