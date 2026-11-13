//! The mock secret store, and the plain-file fallback that shares its shape.
//!
//! The mock is what the crate's own tests use: it never touches the real
//! keychain, so `cargo test` works on a build agent with no desktop session
//! and no keyring daemon.
//!
//! The plain-file fallback is a *production* implementation, not a test
//! double. It is used only when the platform store is unavailable, and it is
//! the one place where the "protect secrets" requirement is only partially
//! met — the file is protected by permissions, not by encryption.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::{SecretError, SecretStore};
use crate::atomic::write_atomic;

/// A secret store that keeps everything in memory.
///
/// Used by the tests, and by any caller that wants to run without touching
/// the machine's keychain.
#[derive(Debug, Default)]
pub struct MockSecretStore {
    secrets: Mutex<BTreeMap<String, Vec<u8>>>,
}

impl MockSecretStore {
    /// An empty mock store.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many secrets the store holds.
    pub fn len(&self) -> usize {
        self.secrets
            .lock()
            .expect("the mock store's lock is not poisoned")
            .len()
    }

    /// Whether the store holds no secrets.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MockSecretStore {
    fn set(&self, key: &str, secret: &[u8]) -> Result<(), SecretError> {
        let mut guard = self
            .secrets
            .lock()
            .expect("the mock store's lock is not poisoned");
        guard.insert(key.to_string(), secret.to_vec());
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, SecretError> {
        let guard = self
            .secrets
            .lock()
            .expect("the mock store's lock is not poisoned");
        guard
            .get(key)
            .cloned()
            .ok_or_else(|| SecretError::NotFound {
                key: key.to_string(),
            })
    }

    fn remove(&self, key: &str) -> Result<(), SecretError> {
        let mut guard = self
            .secrets
            .lock()
            .expect("the mock store's lock is not poisoned");
        guard.remove(key);
        Ok(())
    }

    fn is_encrypted(&self) -> bool {
        // The mock holds the secrets in the process's memory only, so
        // nothing is written to disk at all.
        true
    }

    fn backend_name(&self) -> &'static str {
        "in-memory (mock)"
    }
}

/// The on-disk shape of the plain-file fallback.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FallbackFile {
    /// The schema version of this file.
    schema: u32,
    /// The secrets, keyed by their key, base64-encoded.
    #[serde(default)]
    secrets: BTreeMap<String, String>,
}

/// A secret store backed by a plain file with restrictive permissions.
///
/// # This is a weaker guarantee
///
/// The file is **not encrypted**. It is readable by any process running as
/// the same user, and by anyone who can copy the file off the machine and
/// read it elsewhere. The permissions stop *other users on the machine* from
/// reading it; they do nothing against malware running as the user.
///
/// The file exists only because some machines have no keyring at all — a
/// headless Linux server, a minimal desktop without gnome-keyring or KWallet.
/// When the platform store is available, it is used instead, and
/// [`SecretStore::is_encrypted`] reports the difference so the Settings
/// screen can say so plainly.
#[derive(Debug, Clone)]
pub struct FileSecretStore {
    path: PathBuf,
}

impl FileSecretStore {
    /// Use the fallback file at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file the secrets are stored in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the whole fallback file.
    fn read(&self) -> Result<FallbackFile, SecretError> {
        let raw = match std::fs::read(&self.path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FallbackFile {
                    schema: crate::migration::FALLBACK_SECRET_SCHEMA,
                    secrets: BTreeMap::new(),
                });
            }
            Err(err) => {
                return Err(SecretError::Io {
                    path: self.path.clone(),
                    source: err,
                });
            }
        };
        serde_json::from_slice(&raw).map_err(|_| SecretError::Malformed {
            path: self.path.clone(),
        })
    }

    /// Write the whole fallback file, atomically and with `0600` permissions.
    fn write(&self, file: &FallbackFile) -> Result<(), SecretError> {
        let mut bytes = serde_json::to_vec_pretty(file).map_err(|err| SecretError::Io {
            path: self.path.clone(),
            source: std::io::Error::other(err.to_string()),
        })?;
        bytes.push(b'\n');
        write_atomic(&self.path, &bytes).map_err(|err| match err {
            crate::error::StoreError::Io { path, source } => SecretError::Io { path, source },
            other => SecretError::Io {
                path: self.path.clone(),
                source: std::io::Error::other(other.to_string()),
            },
        })
    }
}

impl SecretStore for FileSecretStore {
    fn set(&self, key: &str, secret: &[u8]) -> Result<(), SecretError> {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD;

        let mut file = self.read()?;
        file.schema = crate::migration::FALLBACK_SECRET_SCHEMA;
        file.secrets
            .insert(key.to_string(), STANDARD.encode(secret));
        self.write(&file)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, SecretError> {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD;

        let file = self.read()?;
        let encoded = file.secrets.get(key).ok_or_else(|| SecretError::NotFound {
            key: key.to_string(),
        })?;
        STANDARD
            .decode(encoded)
            .map_err(|_| SecretError::Malformed {
                path: self.path.clone(),
            })
    }

    fn remove(&self, key: &str) -> Result<(), SecretError> {
        let mut file = self.read()?;
        file.schema = crate::migration::FALLBACK_SECRET_SCHEMA;
        file.secrets.remove(key);
        self.write(&file)
    }

    fn is_encrypted(&self) -> bool {
        false
    }

    fn backend_name(&self) -> &'static str {
        "plain file (not encrypted)"
    }
}

/// A [`SecretStore`] that keeps its secrets in memory but reports itself as
/// unencrypted.
///
/// This exists so that the application can be told "no keyring was found"
/// without having to introduce a third storage backend for the duration of
/// the session. It is the honest description of a mock used in production
/// mode, and the Settings screen would show it as unencrypted.
#[derive(Debug, Default)]
pub struct EphemeralSecretStore {
    inner: MockSecretStore,
}

impl EphemeralSecretStore {
    /// An empty in-memory store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for EphemeralSecretStore {
    fn set(&self, key: &str, secret: &[u8]) -> Result<(), SecretError> {
        self.inner.set(key, secret)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, SecretError> {
        self.inner.get(key)
    }

    fn remove(&self, key: &str) -> Result<(), SecretError> {
        self.inner.remove(key)
    }

    fn is_encrypted(&self) -> bool {
        false
    }

    fn backend_name(&self) -> &'static str {
        "in-memory only (not persisted)"
    }
}

/// Read a file's contents, treating a missing file as empty.
///
/// A helper for the tests and for callers that want to inspect the fallback
/// file without going through the trait.
pub fn read_fallback_file(path: &Path) -> Result<Vec<u8>, std::io::Error> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mock_store_round_trips_a_secret() {
        let store = MockSecretStore::new();
        store.set("key", b"value").unwrap();
        assert_eq!(store.get("key").unwrap(), b"value");
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn the_mock_store_reports_a_missing_key() {
        let store = MockSecretStore::new();
        let err = store.get("nope").unwrap_err();
        assert!(matches!(err, SecretError::NotFound { .. }));
    }

    #[test]
    fn removing_a_missing_key_from_the_mock_is_not_an_error() {
        let store = MockSecretStore::new();
        store.remove("nope").unwrap();
    }

    #[test]
    fn the_mock_store_replaces_a_secret() {
        let store = MockSecretStore::new();
        store.set("key", b"one").unwrap();
        store.set("key", b"two").unwrap();
        assert_eq!(store.get("key").unwrap(), b"two");
    }

    #[test]
    fn the_file_fallback_round_trips_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"));
        store.set("key", b"value").unwrap();
        assert_eq!(store.get("key").unwrap(), b"value");
    }

    #[test]
    fn the_file_fallback_reports_a_missing_key() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"));
        let err = store.get("nope").unwrap_err();
        assert!(matches!(err, SecretError::NotFound { .. }));
    }

    #[test]
    fn the_file_fallback_removes_a_secret() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"));
        store.set("key", b"value").unwrap();
        store.remove("key").unwrap();
        assert!(matches!(
            store.get("key"),
            Err(SecretError::NotFound { .. })
        ));
    }

    #[test]
    fn the_file_fallback_reports_that_it_is_not_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets.json"));
        assert!(!store.is_encrypted());
        assert!(store.backend_name().contains("not encrypted"));
    }

    #[cfg(unix)]
    #[test]
    fn the_file_fallback_is_written_with_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let store = FileSecretStore::new(&path);
        store.set("key", b"value").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
    }

    #[test]
    fn the_file_fallback_stores_the_secret_base64_encoded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        let store = FileSecretStore::new(&path);
        store.set("key", b"a-secret-token").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("a-secret-token"),
            "the secret was written in the clear"
        );
    }

    #[test]
    fn a_malformed_fallback_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = FileSecretStore::new(&path);
        let err = store.get("key").unwrap_err();
        assert!(matches!(err, SecretError::Malformed { .. }));
    }

    #[test]
    fn the_ephemeral_store_is_not_encrypted_but_works() {
        let store = EphemeralSecretStore::new();
        store.set("key", b"value").unwrap();
        assert_eq!(store.get("key").unwrap(), b"value");
        assert!(!store.is_encrypted());
    }

    #[test]
    fn the_mock_store_can_be_used_as_a_trait_object() {
        let store: Box<dyn SecretStore> = Box::new(MockSecretStore::new());
        store.set("key", b"value").unwrap();
        assert_eq!(store.get("key").unwrap(), b"value");
    }

    #[test]
    fn the_file_fallback_can_be_used_as_a_trait_object() {
        let dir = tempfile::tempdir().unwrap();
        let store: Box<dyn SecretStore> = Box::new(FileSecretStore::new(dir.path().join("s.json")));
        store.set("key", b"value").unwrap();
        assert_eq!(store.get("key").unwrap(), b"value");
    }
}
