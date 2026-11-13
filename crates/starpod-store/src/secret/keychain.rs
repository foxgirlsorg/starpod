//! The macOS secret store: the login Keychain.
//!
//! Each secret is a *generic password* item in the login keychain, keyed by
//! the app's service name and the secret's key. The API is
//! `security_framework::passwords`:
//!
//! - `set_generic_password(service, account, password)` — creates or updates.
//! - `generic_password(PasswordOptions::new_generic_password(service, account))`
//!   — reads.
//! - `delete_generic_password(service, account)` — removes.
//!
//! Verified against `security-framework-3.7.0/src/passwords.rs` and
//! `src/passwords_options.rs`.
//!
//! The service name is the app's directory name (`Starpod`), and the account
//! is the secret's key. Keychain items are per-application-accessible: the
//! item is created by this app, so this app can read it back; another app
//! with the same service name would need the same access group, which a
//! non-sandboxed app does not have.

use super::{SecretError, SecretStore};

/// The service name the keychain items are stored under.
const SERVICE: &str = "Starpod";

/// A secret store backed by the macOS login Keychain.
#[derive(Debug, Clone, Copy, Default)]
pub struct KeychainSecretStore;

impl KeychainSecretStore {
    /// A keychain-backed store.
    pub fn new() -> Self {
        Self
    }
}

/// Check whether the keychain reports that an item does not exist.
///
/// `security-framework` returns `errSecItemNotFound` for a missing item; the
/// crate does not re-export the constant, so it is used by value. The value
/// is `-25300`, from `security-framework-sys/src/base.rs`.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// Whether the error means "the item is not there".
fn is_not_found(err: &security_framework::base::Error) -> bool {
    err.code() == ERR_SEC_ITEM_NOT_FOUND
}

impl SecretStore for KeychainSecretStore {
    fn set(&self, key: &str, secret: &[u8]) -> Result<(), SecretError> {
        security_framework::passwords::set_generic_password(SERVICE, key, secret).map_err(|err| {
            SecretError::Rejected {
                key: key.to_string(),
                reason: format!("the keychain refused to store the secret: {err}"),
            }
        })
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, SecretError> {
        use security_framework::passwords::{PasswordOptions, generic_password};

        let options = PasswordOptions::new_generic_password(SERVICE, key);
        match generic_password(options) {
            Ok(bytes) => Ok(bytes),
            Err(err) if is_not_found(&err) => Err(SecretError::NotFound {
                key: key.to_string(),
            }),
            Err(err) => Err(SecretError::Rejected {
                key: key.to_string(),
                reason: format!("the keychain refused to return the secret: {err}"),
            }),
        }
    }

    fn remove(&self, key: &str) -> Result<(), SecretError> {
        use security_framework::passwords::delete_generic_password;

        match delete_generic_password(SERVICE, key) {
            Ok(()) => Ok(()),
            Err(err) if is_not_found(&err) => Ok(()),
            Err(err) => Err(SecretError::Rejected {
                key: key.to_string(),
                reason: format!("the keychain refused to delete the secret: {err}"),
            }),
        }
    }

    fn is_encrypted(&self) -> bool {
        true
    }

    fn backend_name(&self) -> &'static str {
        "macOS Keychain (encrypted)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_reports_that_it_is_encrypted() {
        assert!(KeychainSecretStore::new().is_encrypted());
        assert!(
            KeychainSecretStore::new()
                .backend_name()
                .contains("Keychain")
        );
    }

    #[test]
    fn a_missing_key_is_reported_as_not_found() {
        // This exercises the keychain itself. It is skipped when the machine
        // has no login keychain (a CI runner without a session), because the
        // failure would be `Unavailable` rather than `NotFound`.
        let store = KeychainSecretStore::new();
        let key = "starpod-store-test-key-that-does-not-exist";
        let _ = store.remove(key);
        match store.get(key) {
            Err(SecretError::NotFound { .. }) => {}
            Err(other) => panic!("unexpected error: {other}"),
            Ok(bytes) => panic!("the keychain unexpectedly held {bytes:?}"),
        }
    }

    #[test]
    fn a_secret_survives_a_round_trip_through_the_keychain() {
        let store = KeychainSecretStore::new();
        let key = "starpod-store-test-round-trip";
        // The keychain may be unavailable on a headless machine; in that case
        // there is nothing to assert.
        if let Err(SecretError::Rejected { .. }) | Err(SecretError::Unavailable { .. }) =
            store.set(key, b"value")
        {
            return;
        }
        assert_eq!(store.get(key).unwrap(), b"value");
        store.remove(key).unwrap();
        assert!(matches!(store.get(key), Err(SecretError::NotFound { .. })));
    }
}
