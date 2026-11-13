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
//! # Why the configuration is passed as a string
//!
//! The engine accepts `--config-string`, a base64-encoded YAML document.
//! Using it means the validation never touches the file the running engine
//! is using — a validation run cannot disturb a live proxy, and a
//! half-written file cannot be validated.

use std::path::Path;
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

/// Validate a configuration with the engine's own parser.
pub async fn validate_config(
    binary: &Path,
    config: &RuntimeConfig,
) -> EngineResult<ValidationReport> {
    let document = EngineDocument::build(config);
    let yaml = document
        .to_yaml()
        .map_err(|e| EngineError::Internal(e.to_string()))?;

    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(yaml.as_bytes());

    let mut command = tokio::process::Command::new(binary);
    command
        .arg("-t")
        .arg("--config-string")
        .arg(encoded)
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
            "the engine rejected the configuration without saying why".to_string()
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
        }
    }

    #[tokio::test]
    async fn a_missing_engine_binary_is_a_spawn_error() {
        let error = validate_config(Path::new("this-binary-does-not-exist"), &config())
            .await
            .expect_err("the binary does not exist");
        assert!(matches!(error, EngineError::Spawn(_)), "{error:?}");
    }
}
