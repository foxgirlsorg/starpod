//! Validating a configuration with the engine's own parser.
//!
//! The app builds configurations itself, and the app's own validation layer
//! only knows the rules it was written to know. The engine's parser knows
//! all of them. So the authoritative pre-apply check runs the engine binary
//! with `-t`, which parses the configuration and exits without starting the
//! proxy.
//!
//! This is GOAL.md §3's "validate before applying" done the only way that
//! actually holds: the thing that will run the configuration is the thing
//! that checks it.
//!
//! # Why the candidate is validated as a *file*
//!
//! The engine accepts `--config-string`, a base64-encoded YAML document,
//! and that is the tempting choice: it never touches the file the running
//! engine is using, so a validation run cannot disturb a live proxy.
//!
//! It is the wrong choice here, for a concrete reason.
//!
//! The engine derives the directory it caches remote provider payloads in
//! from the *config file's* location
//! (`meow-config-0.29.0/src/lib.rs:4210-4222` —
//! `resource_cache_dir_for_config_path`), and passes it into the build as
//! the `cache_dir` that contains every provider's on-disk payload. With
//! `--config-string` there is no file, so `cache_dir` is `None`
//! (`meow-app-0.29.0/src/main.rs:596-605`).
//!
//! With no `cache_dir`, a `file:` rule-provider is a **hard error** and an
//! `http:` one silently loses its on-disk cache:
//!
//! > without a `cache_dir` (runtime rebuilds such as `PUT /configs` or
//! > `--config-string`) there is no containment root, so a `path:` is never
//! > honored: http providers fall back to fetching into memory, file
//! > providers hard-error.
//!
//! — `meow-config-0.29.0/src/rule_provider.rs:1699-1741`
//!
//! So validating a `--config-string` would report a configuration as broken
//! that the engine would run perfectly well from a file. The candidate is
//! therefore written to a file and validated with `-f`.
//!
//! # The candidate is validated where it will live
//!
//! The candidate is written to a **sibling** of the live configuration, and
//! validated there. Two things fall out of that:
//!
//! - The bytes the engine validates are the bytes the app will promote, so
//!   there is no gap between "what was checked" and "what will run".
//! - The live configuration is never modified until the replacement has
//!   been validated. That is GOAL.md §3's "do not modify the running
//!   configuration until the replacement validates", and it is a property
//!   of the sequence rather than a promise.
//!
//! The file is given the same name as the live one with a `.candidate`
//! suffix, so it sits in the same directory — which matters, because a
//! provider's relative `path:` resolves against the config file's
//! directory.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use starpod_core::domain::RuntimeConfig;

use crate::config::EngineDocument;
use crate::engine::ValidationReport;
use crate::error::{EngineError, EngineResult};

/// How long the engine may take to parse a configuration before the check
/// is abandoned.
///
/// A parse is fast; a slow one means the engine is stuck on something else,
/// usually a network fetch the offline mode was supposed to prevent.
const VALIDATE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The suffix the candidate configuration is written under.
///
/// It sits beside the live file, so a relative provider `path:` resolves
/// against the same directory in the validation run as in the real run.
const CANDIDATE_SUFFIX: &str = ".candidate";

/// The path the candidate configuration is written to.
pub fn candidate_path(live_path: &Path) -> PathBuf {
    let mut name = live_path.file_name().unwrap_or_default().to_os_string();
    name.push(CANDIDATE_SUFFIX);
    match live_path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Validate a configuration with the engine's own parser.
///
/// The configuration is written to `live_path`'s sibling and validated in
/// place, so that the check exercises the same directory layout the engine
/// will see. The candidate file is removed afterwards, whether the check
/// passed or failed.
pub async fn validate_config(
    binary: &Path,
    config: &RuntimeConfig,
    live_path: &Path,
) -> EngineResult<ValidationReport> {
    let document = EngineDocument::build(config);
    let yaml = document
        .to_yaml()
        .map_err(|e| EngineError::Internal(e.to_string()))?;

    let candidate = candidate_path(live_path);
    if let Some(parent) = candidate.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| EngineError::Io(e.to_string()))?;
    }
    if let Err(error) = tokio::fs::write(&candidate, yaml.as_bytes()).await {
        return Err(EngineError::Io(error.to_string()));
    }

    // The candidate must not outlive the check, whatever the outcome. A
    // rejected one is kept beside the live file, so the reason can be found.
    let outcome = run_validation(binary, &candidate).await;
    if matches!(&outcome, Ok(report) if !report.ok) {
        let _ = tokio::fs::copy(&candidate, candidate.with_extension("rejected.yaml")).await;
    }

    let _ = tokio::fs::remove_file(&candidate).await;

    outcome
}

/// Run the engine's own parser over a configuration file.
async fn run_validation(binary: &Path, path: &Path) -> EngineResult<ValidationReport> {
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("-t")
        .arg("-f")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let output = tokio::time::timeout(VALIDATE_TIMEOUT, command.output())
        .await
        .map_err(|_| EngineError::Timeout)?
        .map_err(|e| EngineError::Spawn(e.to_string()))?;

    if output.status.success() {
        return Ok(ValidationReport {
            ok: true,
            engine_error: None,
            warnings: Vec::new(),
        });
    }

    // The engine's stderr is the authoritative explanation of why the
    // configuration was rejected. It is passed through verbatim: rewording
    // it would lose the detail that identifies the offending rule.
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        stderr.trim().to_string()
    };

    Ok(ValidationReport {
        ok: false,
        engine_error: Some(if message.is_empty() {
            format!(
                "the engine rejected the configuration without saying why ({})",
                output.status
            )
        } else {
            message
        }),
        warnings: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use starpod_core::domain::{LogLevel, ProxyMode, TunSettings};

    fn config() -> RuntimeConfig {
        RuntimeConfig {
            mixed_port: Some(7890),
            socks_port: Some(7891),
            port: None,
            allow_lan: false,
            bind_address: None,
            mode: ProxyMode::Rule,
            log_level: LogLevel::Info,
            tun: TunSettings::default(),
            proxies: Vec::new(),
            proxy_groups: Vec::new(),
            rules: vec!["MATCH,PROXY".to_string()],
            rule_providers: Vec::new(),
            subscriptions: Vec::new(),
            engine: Default::default(),
        }
    }

    #[test]
    fn the_candidate_sits_beside_the_live_configuration() {
        let live = Path::new("/data/Starpod/runtime-config.yaml");
        let candidate = candidate_path(live);
        assert_eq!(candidate.parent(), live.parent());
        assert!(
            candidate.to_string_lossy().ends_with(".candidate"),
            "{candidate:?}"
        );
    }

    #[tokio::test]
    async fn a_missing_engine_binary_is_a_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("runtime-config.yaml");
        let error = validate_config(Path::new("this-binary-does-not-exist"), &config(), &live)
            .await
            .expect_err("the binary does not exist");
        assert!(matches!(error, EngineError::Spawn(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_failed_validation_leaves_no_candidate_behind() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("runtime-config.yaml");
        let _ = validate_config(Path::new("this-binary-does-not-exist"), &config(), &live).await;
        assert!(
            !candidate_path(&live).exists(),
            "the candidate was left behind"
        );
    }

    #[tokio::test]
    async fn the_live_configuration_is_not_touched_by_a_validation() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("runtime-config.yaml");
        tokio::fs::write(&live, b"mode: global\n").await.unwrap();
        let _ = validate_config(Path::new("this-binary-does-not-exist"), &config(), &live).await;
        assert_eq!(
            tokio::fs::read(&live).await.unwrap(),
            b"mode: global\n",
            "the running configuration must survive a failed validation"
        );
    }
}
