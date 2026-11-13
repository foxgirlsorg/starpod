//! The secret store: where the subscription URLs and the controller secret
//! live.
//!
//! The trait is deliberately small — four operations — because the rest of
//! the app must not care *how* a secret is kept. What it must care about is
//! the guarantee, and the guarantee differs per platform:
//!
//! | Platform | Mechanism | Guarantee |
//! |---|---|---|
//! | Windows | DPAPI (`CryptProtectData`) | encrypted, bound to the user account |
//! | macOS | Keychain (`security-framework`) | encrypted, in the login keychain |
//! | Linux | Secret Service (`secret-service`) | encrypted, in the desktop keyring |
//! | any, as a fallback | a plain file with `0600` / a user-only ACL | **not encrypted** |
//!
//! The fallback is a *weaker* guarantee and the difference is not cosmetic:
//! anything that can read the file can read the secret. That is why
//! [`SecretStore::is_encrypted`] exists — the Settings screen has to be able
//! to say plainly that the secret is stored unencrypted at rest, as
//! ARCHITECTURE §5 requires ("the Settings screen states plainly that it is
//! not encrypted").
//!
//! The trait is object-safe on purpose: the application holds a
//! `Box<dyn SecretStore>` so the fallback can be swapped in at runtime when
//! the platform store turns out to be unavailable.

use std::path::PathBuf;

/// The in-memory store and the plain-file fallback.
pub mod file;

/// Choosing the secret store for this platform.
pub mod factory;

#[cfg(target_os = "windows")]
pub mod dpapi;

#[cfg(target_os = "macos")]
pub mod keychain;

#[cfg(target_os = "linux")]
pub mod secret_service;

pub use factory::{SelectedStore, open_secret_store};
pub use file::{EphemeralSecretStore, FileSecretStore, MockSecretStore};

/// A failure while reading or writing a secret.
///
/// The message never contains the secret itself.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    /// The platform's secret store is not available on this machine.
    ///
    /// This is what the application sees when there is no keyring at all —
    /// a headless Linux box, for instance. The caller falls back to the
    /// plain file.
    #[error("the platform secret store is unavailable: {reason}")]
    Unavailable {
        /// Why it is unavailable.
        reason: String,
    },

    /// The requested secret is not in the store.
    #[error("no secret is stored under the key {key:?}")]
    NotFound {
        /// The key that was looked up.
        key: String,
    },

    /// The secret store refused the operation.
    #[error("the secret store refused the operation for the key {key:?}: {reason}")]
    Rejected {
        /// The key involved.
        key: String,
        /// The platform's own message.
        reason: String,
    },

    /// The plain-file fallback could not be read or written.
    #[error("could not access the fallback secret file {path}: {source}")]
    Io {
        /// The fallback file.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The fallback file exists but does not contain a valid secret.
    #[error("the fallback secret file {path} is malformed")]
    Malformed {
        /// The fallback file.
        path: PathBuf,
    },
}

/// A place secrets can be kept.
///
/// Object-safe on purpose, so the application can hold a trait object and
/// swap the implementation at runtime.
pub trait SecretStore: Send + Sync {
    /// Store `secret` under `key`, replacing anything already there.
    fn set(&self, key: &str, secret: &[u8]) -> Result<(), SecretError>;

    /// Read the secret stored under `key`.
    ///
    /// Returns [`SecretError::NotFound`] when there is no such secret.
    fn get(&self, key: &str) -> Result<Vec<u8>, SecretError>;

    /// Remove the secret stored under `key`.
    ///
    /// Removing a secret that is not there is **not** an error: the desired
    /// end state holds either way.
    fn remove(&self, key: &str) -> Result<(), SecretError>;

    /// Whether this store encrypts the secrets at rest.
    ///
    /// The plain-file fallback returns `false`. The UI uses this to state
    /// the weaker guarantee honestly rather than implying protection the
    /// store does not provide.
    fn is_encrypted(&self) -> bool;

    /// A short name for the backing mechanism, for the Settings screen.
    fn backend_name(&self) -> &'static str;
}

/// The key under which the controller secret is stored.
pub const CONTROLLER_SECRET_KEY: &str = "controller:secret";

/// The key under which a subscription's URL is stored.
///
/// The subscription id is appended, so two subscriptions never collide.
pub fn subscription_url_key(subscription_id: &str) -> String {
    format!("subscription:{subscription_id}:url")
}

/// The key under which a subscription's `Authorization` header is stored.
pub fn subscription_header_key(subscription_id: &str) -> String {
    format!("subscription:{subscription_id}:header")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_names_are_distinct_per_purpose_and_per_subscription() {
        assert_ne!(CONTROLLER_SECRET_KEY, subscription_url_key("a"));
        assert_ne!(subscription_url_key("a"), subscription_url_key("b"));
        assert_ne!(subscription_url_key("a"), subscription_header_key("a"));
    }

    #[test]
    fn the_key_names_are_stable() {
        assert_eq!(subscription_url_key("abc"), "subscription:abc:url");
        assert_eq!(subscription_header_key("abc"), "subscription:abc:header");
    }
}
