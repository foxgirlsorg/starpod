//! The Windows secret store: DPAPI.
//!
//! DPAPI (`CryptProtectData` / `CryptUnprotectData`) encrypts a blob with a
//! key derived from the user's credentials. A blob encrypted by one user
//! account cannot be decrypted by another, and cannot be decrypted on a
//! different machine — which is exactly the property a subscription URL
//! needs.
//!
//! `CRYPTPROTECT_UI_FORBIDDEN` is passed on both calls: the app must never
//! pop a dialog asking for a password, because it runs in a tray with no
//! window to parent the dialog to.
//!
//! Each secret is stored under its own DPAPI blob. The blobs are kept in one
//! file so that the store is a single artefact, and each blob is prefixed
//! with a magic marker so that a file written by a different implementation
//! is detected rather than misread.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{SecretError, SecretStore};
use crate::atomic::write_atomic;
use crate::error::StoreError;

/// The marker that identifies a DPAPI-protected blob.
///
/// The value is arbitrary but stable; it exists so the store can tell a
/// protected blob from raw bytes and refuse to misread one.
const BLOB_MAGIC: &[u8; 8] = b"STPRDPA1";

/// The on-disk shape of the DPAPI store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct DpapiFile {
    /// The schema version of this file.
    schema: u32,
    /// The protected blobs, keyed by their key.
    #[serde(default)]
    blobs: BTreeMap<String, String>,
}

/// A secret store backed by Windows DPAPI.
///
/// The blobs are held in a file so that the store survives a restart. The
/// file itself is not secret — the blobs are — but it is written with
/// restrictive permissions anyway, because a blob is only as safe as the
/// key that decrypts it, and the file is one step away from it.
#[derive(Debug, Clone)]
pub struct DpapiSecretStore {
    path: PathBuf,
}

impl DpapiSecretStore {
    /// Use the DPAPI store at `path`.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file the protected blobs are stored in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Read the whole store file.
    fn read(&self) -> Result<DpapiFile, SecretError> {
        let raw = match std::fs::read(&self.path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(DpapiFile {
                    schema: crate::migration::FALLBACK_SECRET_SCHEMA,
                    blobs: BTreeMap::new(),
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

    /// Write the whole store file, atomically.
    fn write(&self, file: &DpapiFile) -> Result<(), SecretError> {
        let mut bytes = serde_json::to_vec_pretty(file).map_err(|err| SecretError::Io {
            path: self.path.clone(),
            source: std::io::Error::other(err.to_string()),
        })?;
        bytes.push(b'\n');
        write_atomic(&self.path, &bytes).map_err(store_error_to_secret)
    }
}

/// Convert a store error into a secret error.
fn store_error_to_secret(err: StoreError) -> SecretError {
    match err {
        StoreError::Io { path, source } => SecretError::Io { path, source },
        other => SecretError::Io {
            path: PathBuf::new(),
            source: std::io::Error::other(other.to_string()),
        },
    }
}

/// Encrypt `plaintext` with DPAPI.
fn protect(plaintext: &[u8]) -> Result<Vec<u8>, SecretError> {
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext
            .len()
            .try_into()
            .map_err(|_| SecretError::Rejected {
                key: "<memory>".to_string(),
                reason: "the secret is too large to protect".to_string(),
            })?,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    // SAFETY: `input` points at the caller's buffer for the duration of the
    // call, `output` is filled in by the API, and the description pointer is
    // null, which the API accepts.
    unsafe {
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|err| SecretError::Rejected {
        key: "<memory>".to_string(),
        reason: format!("DPAPI refused to protect the secret: {err}"),
    })?;

    if output.pbData.is_null() {
        return Err(SecretError::Rejected {
            key: "<memory>".to_string(),
            reason: "DPAPI returned no data".to_string(),
        });
    }

    // SAFETY: the API allocated the buffer with `LocalAlloc`, so it is freed
    // with `LocalFree` after the bytes are copied out.
    let protected = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            output.pbData as *mut std::ffi::c_void,
        )));
        bytes
    };

    let mut blob = Vec::with_capacity(BLOB_MAGIC.len() + protected.len());
    blob.extend_from_slice(BLOB_MAGIC);
    blob.extend_from_slice(&protected);
    Ok(blob)
}

/// Decrypt a blob produced by [`protect`].
fn unprotect(blob: &[u8]) -> Result<Vec<u8>, SecretError> {
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };

    if blob.len() < BLOB_MAGIC.len() || &blob[..BLOB_MAGIC.len()] != BLOB_MAGIC {
        return Err(SecretError::Rejected {
            key: "<memory>".to_string(),
            reason: "the stored blob is not a DPAPI blob".to_string(),
        });
    }
    let payload = &blob[BLOB_MAGIC.len()..];

    let input = CRYPT_INTEGER_BLOB {
        cbData: payload
            .len()
            .try_into()
            .map_err(|_| SecretError::Rejected {
                key: "<memory>".to_string(),
                reason: "the stored blob is too large".to_string(),
            })?,
        pbData: payload.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };

    // SAFETY: as in `protect`: the input buffer is live for the call and the
    // output buffer is owned by the API and freed below.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .map_err(|err| SecretError::Rejected {
        key: "<memory>".to_string(),
        reason: format!("DPAPI refused to unprotect the secret: {err}"),
    })?;

    if output.pbData.is_null() {
        return Err(SecretError::Rejected {
            key: "<memory>".to_string(),
            reason: "DPAPI returned no data".to_string(),
        });
    }

    // SAFETY: see `protect`.
    let plain = unsafe {
        let bytes = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            output.pbData as *mut std::ffi::c_void,
        )));
        bytes
    };
    Ok(plain)
}

impl SecretStore for DpapiSecretStore {
    fn set(&self, key: &str, secret: &[u8]) -> Result<(), SecretError> {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD;

        let blob = protect(secret)?;
        let mut file = self.read()?;
        file.schema = crate::migration::FALLBACK_SECRET_SCHEMA;
        file.blobs.insert(key.to_string(), STANDARD.encode(&blob));
        self.write(&file)
    }

    fn get(&self, key: &str) -> Result<Vec<u8>, SecretError> {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD;

        let file = self.read()?;
        let encoded = file.blobs.get(key).ok_or_else(|| SecretError::NotFound {
            key: key.to_string(),
        })?;
        let blob = STANDARD
            .decode(encoded)
            .map_err(|_| SecretError::Malformed {
                path: self.path.clone(),
            })?;
        unprotect(&blob)
    }

    fn remove(&self, key: &str) -> Result<(), SecretError> {
        let mut file = self.read()?;
        file.schema = crate::migration::FALLBACK_SECRET_SCHEMA;
        file.blobs.remove(key);
        self.write(&file)
    }

    fn is_encrypted(&self) -> bool {
        true
    }

    fn backend_name(&self) -> &'static str {
        "Windows DPAPI (encrypted)"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_survives_a_round_trip_through_dpapi() {
        let dir = tempfile::tempdir().unwrap();
        let store = DpapiSecretStore::new(dir.path().join("dpapi.json"));
        store.set("key", b"a-subscription-token").unwrap();
        assert_eq!(store.get("key").unwrap(), b"a-subscription-token");
    }

    #[test]
    fn the_stored_file_does_not_contain_the_secret_in_the_clear() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dpapi.json");
        let store = DpapiSecretStore::new(&path);
        store.set("key", b"a-subscription-token").unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("a-subscription-token"),
            "the secret was stored in the clear"
        );
    }

    #[test]
    fn a_missing_key_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let store = DpapiSecretStore::new(dir.path().join("dpapi.json"));
        let err = store.get("nope").unwrap_err();
        assert!(matches!(err, SecretError::NotFound { .. }));
    }

    #[test]
    fn removing_a_secret_removes_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = DpapiSecretStore::new(dir.path().join("dpapi.json"));
        store.set("key", b"value").unwrap();
        store.remove("key").unwrap();
        assert!(matches!(
            store.get("key"),
            Err(SecretError::NotFound { .. })
        ));
    }

    #[test]
    fn removing_a_missing_secret_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = DpapiSecretStore::new(dir.path().join("dpapi.json"));
        store.remove("never-set").unwrap();
    }

    #[test]
    fn the_store_reports_that_it_is_encrypted() {
        let dir = tempfile::tempdir().unwrap();
        let store = DpapiSecretStore::new(dir.path().join("dpapi.json"));
        assert!(store.is_encrypted());
    }

    #[test]
    fn a_blob_without_the_magic_marker_is_refused() {
        let err = unprotect(b"not a dpapi blob").unwrap_err();
        assert!(matches!(err, SecretError::Rejected { .. }));
    }

    #[test]
    fn a_malformed_store_file_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dpapi.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let store = DpapiSecretStore::new(&path);
        assert!(matches!(
            store.get("key"),
            Err(SecretError::Malformed { .. })
        ));
    }
}
