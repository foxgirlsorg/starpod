//! Writing the generated engine configuration.
//!
//! The generated YAML is a **secret-adjacent artifact**. ARCHITECTURE §5.1
//! says so explicitly:
//!
//! > The generated YAML that the app writes for meow contains, unavoidably:
//! > - **node credentials** (passwords, UUIDs, private keys) for every
//! >   app-managed subscription node;
//! > - **subscription URLs** for every engine-managed subscription (the
//! >   engine needs them).
//!
//! So it is written with restrictive permissions (`0600` on unix, a
//! user-only DACL on Windows) and into the app's own data directory.
//!
//! The **controller secret is never written to it.** The secret is passed to
//! the engine on the child's command line (`--secret`), which is the only
//! interface the engine offers (ARCHITECTURE §1.2, §5). [`write_runtime_config`]
//! refuses a configuration that carries a `secret` key, so a call site that
//! gets this wrong fails loudly instead of leaking the secret into a
//! user-readable file.
//!
//! ## This module writes bytes; it does not build them
//!
//! The YAML *content* is produced by `starpod-engine`'s `EngineDocument`,
//! which is the only place in the app that knows the engine's key names.
//! That separation is not a matter of taste — it is a correctness
//! requirement, and it was verified against the engine's own binary.
//!
//! The engine's configuration type is `meow_config::raw::RawConfig`, which
//! is `#[serde(rename_all = "kebab-case")]` (`meow-config-0.29.0/src/raw.rs:183-184`)
//! and has **no `deny_unknown_fields`**. The app's own domain types use
//! snake_case field names, so serialising them directly would emit
//! `mixed_port`, `allow_lan`, `log_level`, `proxy_groups` — **none of which
//! the engine reads**.
//!
//! That failure is silent, which is what makes it dangerous. Verified
//! against meow 0.29.0 (`x86_64-pc-windows-msvc`):
//!
//! ```text
//! $ meow -t --config-string <base64 of "mixed-port: 99999 …">
//! Error: --config-string: line 1 column 13: mixed-port: invalid u16
//!
//! $ meow -t --config-string <base64 of "mixed_port: 99999 …">
//! Configuration test passed (--config-string)
//! ```
//!
//! The same invalid port is rejected under the engine's spelling and
//! accepted under the app's. A configuration written that way validates
//! cleanly, starts cleanly, and runs with the wrong ports, the wrong log
//! level and no proxy groups at all — and reports nothing wrong.
//!
//! So the emitter lives in `starpod-engine`, and this module only owns the
//! *file*: where it goes, how it is written, and the guard that keeps the
//! controller secret out of it.
//!
//! ## Why JSON for the state, YAML for the config
//!
//! The state files are JSON because they are small, machine-written and
//! machine-read, and `serde_json` is already in the tree (ARCHITECTURE §5,
//! decision Q7). The generated config is YAML because **the engine parses
//! YAML** — writing JSON there would mean the file on disk is not the file
//! the engine reads. The subscription cache is likewise YAML, for the same
//! reason: it is a payload in the engine's own format, and it must be
//! re-readable by the engine without a conversion step.

use std::path::Path;

use crate::atomic::write_atomic;
use crate::error::StoreError;

/// The key the engine's controller secret would appear under.
///
/// The engine accepts it as a config key (`RawConfig.secret`,
/// `meow-config-0.29.0/src/raw.rs:205`), which is exactly why it has to be
/// checked: the app uses `--secret` on the command line instead.
pub const FORBIDDEN_SECRET_KEY: &str = "secret";

/// The result of writing the generated configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenConfig {
    /// The path the configuration was written to.
    pub path: std::path::PathBuf,
    /// How many bytes were written.
    pub bytes: usize,
}

/// Write the engine's YAML configuration to `path`, with restrictive
/// permissions.
///
/// The YAML is written verbatim: the caller has already produced it, and
/// re-serialising it here would risk changing the bytes the engine is about
/// to validate. ARCHITECTURE §1.3 is explicit that the app writes the YAML to
/// disk and then hands the *path* to the engine, so that `meow -t` validates
/// the exact same bytes that will be loaded.
///
/// The write is atomic (write temp → fsync → rename) and the resulting file
/// is readable only by its owner on every platform, because the file carries
/// node credentials and subscription URLs.
///
/// # Errors
///
/// Returns [`StoreError::ForbiddenSecretKey`] if the configuration carries a
/// `secret` key. The controller secret must be passed on the engine's
/// command line, never written into a user-readable file.
pub fn write_runtime_config(path: &Path, yaml: &[u8]) -> Result<WrittenConfig, StoreError> {
    reject_secret_key(yaml)?;
    write_atomic(path, yaml)?;
    Ok(WrittenConfig {
        path: path.to_path_buf(),
        bytes: yaml.len(),
    })
}

/// Refuse a configuration that carries the controller secret.
///
/// The check is on the *text*, because the file is written as text and the
/// point of the check is to stop the secret reaching the disk in any form.
/// It is deliberately simple: a line whose key is exactly `secret` at the
/// top level. A YAML document is a mapping at the top level, so a top-level
/// key is one that starts in column zero.
///
/// A *nested* `secret:` key is allowed. Only the top-level key is the
/// engine's controller secret; a nested one is some other thing entirely
/// (a node's own field, for instance).
pub fn reject_secret_key(yaml: &[u8]) -> Result<(), StoreError> {
    let text = String::from_utf8_lossy(yaml);
    for line in text.lines() {
        let trimmed = line.trim_end();
        // A top-level key: no leading whitespace, a `key:` prefix.
        if trimmed.starts_with(char::is_whitespace) {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix(FORBIDDEN_SECRET_KEY) else {
            continue;
        };
        if rest.starts_with(':') || rest.starts_with(' ') {
            return Err(StoreError::ForbiddenSecretKey {
                key: FORBIDDEN_SECRET_KEY.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_config_without_a_secret_key_is_accepted() {
        let yaml = "mixed-port: 7890\nmode: rule\nrules:\n  - MATCH,DIRECT\n";
        assert!(reject_secret_key(yaml.as_bytes()).is_ok());
    }

    #[test]
    fn a_config_with_a_top_level_secret_key_is_refused() {
        let yaml = "mixed-port: 7890\nsecret: hunter2\n";
        let err = reject_secret_key(yaml.as_bytes()).unwrap_err();
        assert!(matches!(err, StoreError::ForbiddenSecretKey { .. }));
    }

    #[test]
    fn a_nested_secret_key_is_allowed() {
        // Only the *top-level* `secret:` key is the engine's controller
        // secret. A nested key with the same name is some other thing.
        let yaml = "proxies:\n  - name: n\n    secret: not-the-controller-secret\n";
        assert!(reject_secret_key(yaml.as_bytes()).is_ok());
    }

    #[test]
    fn a_key_that_merely_starts_with_secret_is_allowed() {
        let yaml = "secretary: 1\n";
        assert!(reject_secret_key(yaml.as_bytes()).is_ok());
    }

    #[test]
    fn writing_the_config_refuses_a_secret_and_leaves_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-config.yaml");
        let err = write_runtime_config(&path, b"secret: hunter2\n").unwrap_err();
        assert!(matches!(err, StoreError::ForbiddenSecretKey { .. }));
        assert!(!path.exists(), "the file must not have been created");
    }

    #[test]
    fn writing_the_config_produces_the_expected_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-config.yaml");
        let yaml = "mixed-port: 7890\nmode: rule\n";
        let written = write_runtime_config(&path, yaml.as_bytes()).unwrap();
        assert_eq!(written.bytes, yaml.len());
        assert_eq!(std::fs::read(&path).unwrap(), yaml.as_bytes());
    }

    #[test]
    fn writing_the_config_twice_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-config.yaml");
        write_runtime_config(&path, b"mode: rule\n").unwrap();
        write_runtime_config(&path, b"mode: global\n").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"mode: global\n");
    }

    #[test]
    fn writing_the_config_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-config.yaml");
        write_runtime_config(&path, b"mode: rule\n").unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "runtime-config.yaml")
            .collect();
        assert!(leftovers.is_empty(), "left behind {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_written_config_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runtime-config.yaml");
        write_runtime_config(&path, b"mode: rule\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
    }
}
