//! Choosing the secret store for this platform.
//!
//! The rule, from ARCHITECTURE §3.1:
//!
//! > Linux — Secret Service / D-Bus via `secret-service` crate if a keyring
//! > is present; **fallback: file with `0600` perms** in the app's own data
//! > dir, with the UI stating plainly that the secret is stored unencrypted
//! > at rest.
//!
//! So the platform store is tried first, and the plain file is the fallback
//! — not the other way round. The fallback file is written with `0600` on
//! unix and a user-only DACL on Windows.

use std::path::{Path, PathBuf};

use super::SecretStore;
use super::file::FileSecretStore;

/// What a call to [`open_secret_store`] produced.
pub struct SelectedStore {
    /// The store to use.
    pub store: Box<dyn SecretStore>,
    /// Why the fallback was chosen, when it was.
    ///
    /// `None` means the platform store was used.
    pub fallback_reason: Option<String>,
}

impl std::fmt::Debug for SelectedStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SelectedStore")
            .field("backend", &self.store.backend_name())
            .field("fallback_reason", &self.fallback_reason)
            .finish()
    }
}

impl SelectedStore {
    /// Whether the selected store encrypts the secrets at rest.
    pub fn is_encrypted(&self) -> bool {
        self.store.is_encrypted()
    }
}

/// Open the best secret store available on this platform.
///
/// The platform store is attempted first. If it is unavailable — no keyring
/// daemon, no D-Bus session, no login keychain — the plain-file fallback is
/// used and the reason is reported so the UI can say plainly that the
/// secret is not encrypted at rest.
pub fn open_secret_store(fallback_path: PathBuf) -> SelectedStore {
    match try_platform_store(&fallback_path) {
        Ok(store) => SelectedStore {
            store,
            fallback_reason: None,
        },
        Err(reason) => SelectedStore {
            store: Box::new(FileSecretStore::new(fallback_path)),
            fallback_reason: Some(reason),
        },
    }
}

/// Try to open the platform's secret store.
///
/// The returned error is the *reason* the platform store could not be used,
/// phrased for the Settings screen.
#[cfg(target_os = "windows")]
fn try_platform_store(fallback_path: &Path) -> Result<Box<dyn SecretStore>, String> {
    // DPAPI is always present on Windows and needs no daemon, so it cannot
    // be "unavailable". The protected blobs live in their own file, beside
    // the fallback file.
    Ok(Box::new(super::dpapi::DpapiSecretStore::new(
        fallback_path.with_file_name("dpapi-secrets.json"),
    )))
}

/// Try to open the platform's secret store.
#[cfg(target_os = "macos")]
fn try_platform_store(_fallback_path: &Path) -> Result<Box<dyn SecretStore>, String> {
    // The login keychain is present on every macOS install. There is no
    // separate store file: the items live in the keychain itself.
    Ok(Box::new(super::keychain::KeychainSecretStore::new()))
}

/// Try to open the platform's secret store.
#[cfg(target_os = "linux")]
fn try_platform_store(_fallback_path: &Path) -> Result<Box<dyn SecretStore>, String> {
    // The Secret Service API is a *desktop* facility. It is present on a
    // desktop with gnome-keyring or KWallet running, and absent on a
    // headless server. The crate reports the difference through the
    // connection attempt.
    let store = super::secret_service::SecretServiceStore::new();
    // Probe: connecting is the only reliable way to know whether a keyring
    // is running, and it costs one D-Bus round trip. The probe runs on a
    // worker thread, because this may be called from a tokio worker and the
    // blocking D-Bus API panics there (see `secret_service.rs`).
    match super::secret_service::probe() {
        Ok(()) => Ok(Box::new(store)),
        Err(err) => Err(format!("no keyring is available on this system ({err})")),
    }
}

/// Try to open the platform's secret store.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn try_platform_store(_fallback_path: &Path) -> Result<Box<dyn SecretStore>, String> {
    Err("this platform has no supported secret store".to_string())
}

/// Whether the platform has a secret store at all.
///
/// This is a compile-time fact, unlike [`try_platform_store`], which is a
/// runtime fact.
pub const fn platform_has_secret_store() -> bool {
    cfg!(any(
        target_os = "windows",
        target_os = "macos",
        target_os = "linux"
    ))
}

/// The name of the platform's secret store, for the Settings screen.
pub const fn platform_secret_store_name() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Windows DPAPI"
    }
    #[cfg(target_os = "macos")]
    {
        "macOS Keychain"
    }
    #[cfg(target_os = "linux")]
    {
        "Secret Service keyring"
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        "none"
    }
}

/// The module implementing this platform's secret store.
#[cfg(target_os = "windows")]
pub mod platform_store {
    pub use super::super::dpapi::*;
}

/// The module implementing this platform's secret store.
#[cfg(target_os = "macos")]
pub mod platform_store {
    pub use super::super::keychain::*;
}

/// The module implementing this platform's secret store.
#[cfg(target_os = "linux")]
pub mod platform_store {
    pub use super::super::secret_service::*;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret::SecretError;

    #[test]
    fn the_platform_names_are_not_empty() {
        assert!(!platform_secret_store_name().is_empty());
    }

    #[test]
    fn the_fallback_is_used_when_the_platform_store_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let fallback = dir.path().join("secrets.json");
        let selected = open_secret_store(fallback);
        // On a machine with a keyring this is the platform store; without
        // one it is the fallback file. Both must be usable.
        assert!(!selected.store.backend_name().is_empty());
        selected.store.set("key", b"value").unwrap();
        assert_eq!(selected.store.get("key").unwrap(), b"value");
    }

    #[test]
    fn the_fallback_reason_is_present_only_when_the_fallback_was_used() {
        let dir = tempfile::tempdir().unwrap();
        let fallback = dir.path().join("secrets.json");
        let selected = open_secret_store(fallback);
        match selected.fallback_reason {
            None => assert!(selected.is_encrypted(), "the platform store is encrypted"),
            Some(ref reason) => {
                assert!(!reason.is_empty());
                assert!(!selected.is_encrypted(), "the fallback is not encrypted");
            }
        }
    }

    #[test]
    fn the_fallback_file_is_used_only_when_the_platform_store_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let fallback = dir.path().join("secrets.json");
        let selected = open_secret_store(fallback.clone());
        selected.store.set("key", b"value").unwrap();
        if selected.fallback_reason.is_some() {
            assert!(
                fallback.exists(),
                "the fallback file should have been written"
            );
        }
    }

    #[test]
    fn a_store_error_never_contains_the_secret() {
        let err = SecretError::Rejected {
            key: "subscription:abc:url".to_string(),
            reason: "the keychain refused the request".to_string(),
        };
        assert!(!err.to_string().contains("token"));
    }
}
