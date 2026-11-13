//! The Linux secret store: the Secret Service API over D-Bus.
//!
//! The Secret Service API is the desktop keyring protocol implemented by
//! GNOME Keyring, KWallet and others. `secret-service 5.2` talks to it over
//! the session bus.
//!
//! The crate is async; this module uses its **blocking** API, because the
//! secret store is a synchronous trait and because the blocking API is the
//! one that does not require a tokio runtime to be running — the caller may
//! be in a synchronous context.
//
//! Verified against `secret-service-5.2.0/src/blocking/{mod,collection,item}.rs`:
//!
//! - `SecretService::connect(EncryptionType::Plain)` connects to the session
//!   bus and negotiates a session.
//! - `get_default_collection()` returns the `default` collection.
//! - `collection.create_item(label, attributes, secret, replace, content_type)`
//!   creates an item; `replace = true` replaces an existing one.
//! - `service.search_items(attributes)` finds items by attribute; the result
//!   is split into `unlocked` and `locked`.
//! - `item.get_secret()` reads the secret; `item.delete()` removes it.
//!
//! The attribute used to find an item is `application`, set to the app's
//! name, plus `key`, set to the secret's key. That is what makes the item
//! findable again after a restart.

use std::collections::HashMap;

use super::{SecretError, SecretStore};

/// The attribute used to tag an item as belonging to this app.
const ATTR_APPLICATION: &str = "application";

/// The value of the `application` attribute.
const APPLICATION: &str = "Starpod";

/// The attribute holding the secret's key.
const ATTR_KEY: &str = "key";

/// The content type reported for the stored secret.
const CONTENT_TYPE: &str = "text/plain";

/// A secret store backed by the desktop keyring.
#[derive(Debug, Clone, Copy, Default)]
pub struct SecretServiceStore;

impl SecretServiceStore {
    /// A keyring-backed store.
    pub fn new() -> Self {
        Self
    }
}

/// Open the session bus and connect to the secret service.
///
/// The `Plain` encryption type is used: the transport is the session bus,
/// which is already confined to the user's own session, and `Plain` avoids
/// requiring the DH key exchange (and its crypto backend) for a store that
/// only ever holds a couple of short strings.
fn connect() -> Result<secret_service::blocking::SecretService<'static>, SecretError> {
    secret_service::blocking::SecretService::connect(secret_service::EncryptionType::Plain).map_err(
        |err| SecretError::Unavailable {
            reason: format!("could not reach the secret service: {err}"),
        },
    )
}

/// The attributes that identify one of this app's items.
fn attributes_for(key: &str) -> HashMap<&str, &str> {
    HashMap::from([(ATTR_APPLICATION, APPLICATION), (ATTR_KEY, key)])
}

impl SecretStore for SecretServiceStore {
    fn set(&self, key: &str, secret: &[u8]) -> Result<(), SecretError> {
        let service = connect()?;
        let collection = service
            .get_default_collection()
            .or_else(|_| service.get_any_collection())
            .map_err(|err| SecretError::Unavailable {
                reason: format!("the keyring has no collection to write to: {err}"),
            })?;

        // `replace = true` makes an existing item with the same attributes be
        // overwritten rather than duplicated.
        let item = collection
            .create_item(key, attributes_for(key), secret, true, CONTENT_TYPE)
            .map_err(|err| SecretError::Rejected {
                key: key.to_string(),
                reason: format!("the keyring refused to store the secret: {err}"),
            })?;

        // The item holds the secret only once it is unlocked.
        if item.is_locked().unwrap_or(false) {
            item.unlock().map_err(|err| SecretError::Rejected {
                key: key.to_string(),
                reason: format!("the keyring refused to unlock the item: {err}"),
            })?;
        }
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, SecretError> {
        let service = connect()?;
        let found =
            service
                .search_items(attributes_for(key))
                .map_err(|err| SecretError::Rejected {
                    key: key.to_string(),
                    reason: format!("the keyring refused the lookup: {err}"),
                })?;

        let item = found
            .unlocked
            .into_iter()
            .chain(found.locked)
            .next()
            .ok_or_else(|| SecretError::NotFound {
                key: key.to_string(),
            })?;

        if item.is_locked().unwrap_or(false) {
            item.unlock().map_err(|err| SecretError::Rejected {
                key: key.to_string(),
                reason: format!("the keyring refused to unlock the item: {err}"),
            })?;
        }

        item.get_secret().map_err(|err| SecretError::Rejected {
            key: key.to_string(),
            reason: format!("the keyring refused to return the secret: {err}"),
        })
    }

    fn remove(&self, key: &str) -> Result<(), SecretError> {
        let service = connect()?;
        let found =
            service
                .search_items(attributes_for(key))
                .map_err(|err| SecretError::Rejected {
                    key: key.to_string(),
                    reason: format!("the keyring refused the lookup: {err}"),
                })?;

        for item in found.unlocked.into_iter().chain(found.locked) {
            item.delete().map_err(|err| SecretError::Rejected {
                key: key.to_string(),
                reason: format!("the keyring refused to delete the secret: {err}"),
            })?;
        }
        Ok(())
    }

    fn is_encrypted(&self) -> bool {
        true
    }

    fn backend_name(&self) -> &'static str {
        "Secret Service keyring (encrypted)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_store_reports_that_it_is_encrypted() {
        assert!(SecretServiceStore::new().is_encrypted());
        assert!(SecretServiceStore::new().backend_name().contains("keyring"));
    }

    #[test]
    fn the_attributes_identify_the_item() {
        let attrs = attributes_for("my-key");
        assert_eq!(attrs.get(ATTR_APPLICATION), Some(&"Starpod"));
        assert_eq!(attrs.get(ATTR_KEY), Some(&"my-key"));
    }

    #[test]
    fn a_machine_without_a_keyring_reports_unavailable() {
        // On a desktop with a keyring this finds nothing (the test key was
        // never stored); on a headless machine it reports that the service
        // is unavailable. Both are acceptable — what matters is that the
        // call does not panic and does not return a wrong secret.
        let store = SecretServiceStore::new();
        let key = "starpod-store-test-key-that-does-not-exist";
        let _ = store.remove(key);
        match store.get(key) {
            Err(SecretError::NotFound { .. }) | Err(SecretError::Unavailable { .. }) => {}
            Err(other) => panic!("unexpected error: {other}"),
            Ok(bytes) => panic!("the keyring unexpectedly held {bytes:?}"),
        }
    }
}
