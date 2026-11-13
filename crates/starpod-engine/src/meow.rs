//! The meow implementation of [`ProxyEngine`].
//!
//! This is the only module in the app that knows the engine is meow. It
//! ties together the pieces: the supervisor runs the process, the REST
//! client talks to it, the validator asks the engine's own parser whether
//! a configuration is acceptable.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use starpod_core::domain::{RuntimeConfig, TunSettings};

use crate::engine::{
    EngineHandle, EnginePhase, EngineStatus, ProxyEngine, StopRequest, ValidationReport,
};
use crate::error::EngineError;
use crate::log::LogStream;
use crate::rest::ControllerClient;
use crate::supervisor::Supervisor;

/// The proxy core, backed by the meow engine.
pub struct MeowEngine {
    supervisor: Arc<Supervisor>,
    binary: PathBuf,
    /// The controller client, rebuilt whenever the controller's address
    /// changes.
    client: Arc<std::sync::RwLock<Option<ControllerClient>>>,
    /// When the engine started, for the uptime.
    started_at: Arc<std::sync::RwLock<Option<std::time::Instant>>>,
    /// The engine's version, read once it answers.
    version: Arc<std::sync::RwLock<Option<String>>>,
    /// The last error the app saw.
    last_error: Arc<std::sync::RwLock<Option<String>>>,
}

impl MeowEngine {
    /// Build an engine around the given binary.
    pub fn new(binary: PathBuf) -> Self {
        Self {
            supervisor: Arc::new(Supervisor::new()),
            binary,
            client: Arc::new(std::sync::RwLock::new(None)),
            started_at: Arc::new(std::sync::RwLock::new(None)),
            version: Arc::new(std::sync::RwLock::new(None)),
            last_error: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Build an engine around the installed binary.
    pub fn installed() -> Self {
        Self::new(crate::download::installed_binary_path())
    }

    /// The binary this engine runs.
    pub fn binary(&self) -> &std::path::Path {
        &self.binary
    }

    /// The controller client, if the engine is running.
    fn client(&self) -> Option<ControllerClient> {
        self.client.read().ok().and_then(|client| client.clone())
    }

    /// Remember an error for the status view.
    fn remember_error(&self, error: &EngineError) {
        if let Ok(mut slot) = self.last_error.write() {
            *slot = Some(error.to_string());
        }
    }

    /// Forget the last error.
    fn forget_error(&self) {
        if let Ok(mut slot) = self.last_error.write() {
            *slot = None;
        }
    }
}

#[async_trait]
impl ProxyEngine for MeowEngine {
    async fn start(&self, cfg: &RuntimeConfig) -> Result<EngineHandle, EngineError> {
        if !self.binary.is_file() {
            let error = EngineError::NotInstalled(format!(
                "the engine binary is missing at {}",
                self.binary.display()
            ));
            self.remember_error(&error);
            return Err(error);
        }

        let handle = self.supervisor.start(&self.binary, cfg).await?;

        // Wait for the controller to answer. The engine binds its
        // controller before it starts its listeners, so this is the point
        // at which the engine is up.
        let secret = self.supervisor.secret().to_string();
        let controller = wait_for_controller(&self.supervisor, &secret).await;

        match controller {
            Some(address) => {
                let client = ControllerClient::new(format!("http://{address}"), &secret);
                if let Ok(mut slot) = self.client.write() {
                    *slot = Some(client);
                }
                if let Ok(mut slot) = self.started_at.write() {
                    *slot = Some(std::time::Instant::now());
                }
                self.forget_error();
                Ok(handle)
            }
            None => {
                let error = EngineError::EngineDown;
                self.remember_error(&error);
                let _ = self.supervisor.stop().await;
                Err(error)
            }
        }
    }

    async fn stop(&self) -> Result<(), EngineError> {
        self.supervisor.stop().await?;
        if let Ok(mut slot) = self.client.write() {
            *slot = None;
        }
        if let Ok(mut slot) = self.started_at.write() {
            *slot = None;
        }
        self.forget_error();
        Ok(())
    }

    async fn reload(&self, cfg: &RuntimeConfig) -> Result<(), EngineError> {
        // The engine's own parser is the authority on whether the new
        // configuration is acceptable. Reloading an invalid one would
        // leave the proxy in a broken state.
        let report = self.validate(cfg).await?;
        if !report.ok {
            let error = EngineError::EngineRejected(
                report
                    .engine_error
                    .unwrap_or_else(|| "the engine rejected the configuration".to_string()),
            );
            self.remember_error(&error);
            return Err(error);
        }

        let Some(client) = self.client() else {
            let error = EngineError::EngineDown;
            self.remember_error(&error);
            return Err(error);
        };

        let document = crate::config::EngineDocument::build(cfg);
        let yaml = document
            .to_yaml()
            .map_err(|e| EngineError::Internal(e.to_string()))?;
        match client.reload_from_payload(&yaml).await {
            Ok(()) => {
                self.forget_error();
                Ok(())
            }
            Err(error) => {
                self.remember_error(&error);
                Err(error)
            }
        }
    }

    async fn status(&self) -> Result<EngineStatus, EngineError> {
        let Some(client) = self.client() else {
            return Ok(EngineStatus {
                phase: EnginePhase::Stopped,
                last_error: self.last_error.read().ok().and_then(|slot| slot.clone()),
                ..EngineStatus::default()
            });
        };

        let phase = if client.is_ready().await {
            EnginePhase::Running
        } else {
            EnginePhase::Failed
        };

        let mut status = EngineStatus {
            phase,
            controller_ready: phase.is_running(),
            elevated: crate::tun::is_elevated(),
            last_error: self.last_error.read().ok().and_then(|slot| slot.clone()),
            ..EngineStatus::default()
        };

        if status.controller_ready {
            if let Ok(configs) = client.configs().await
                && let Some(mode) = configs.get("mode").and_then(|v| v.as_str())
            {
                status.mode = Some(mode.to_string());
            }
            if let Ok(version) = client.version().await {
                if let Ok(mut slot) = self.version.write() {
                    *slot = Some(version.clone());
                }
                status.version = Some(version);
            }
            if let Ok(Some(started)) = self.started_at.read().map(|slot| *slot) {
                status.uptime_secs = Some(started.elapsed().as_secs());
            }
        }

        Ok(status)
    }

    async fn validate(&self, cfg: &RuntimeConfig) -> Result<ValidationReport, EngineError> {
        crate::validate::validate_config(&self.binary, cfg).await
    }

    async fn set_tun(&self, tun: &TunSettings) -> Result<(), EngineError> {
        let Some(client) = self.client() else {
            let error = EngineError::EngineDown;
            self.remember_error(&error);
            return Err(error);
        };
        let patch = crate::tun::tun_patch(tun);
        match client.patch_runtime(None, None, Some(patch)).await {
            Ok(()) => {
                self.forget_error();
                Ok(())
            }
            Err(error) => {
                self.remember_error(&error);
                Err(error)
            }
        }
    }

    fn logs(&self) -> LogStream {
        LogStream::new(self.supervisor.logs.clone())
    }
}

impl MeowEngine {
    /// Ask the engine to stop, from outside an async context.
    ///
    /// The app's shutdown path needs a way to stop the engine even when the
    /// runtime is already winding down.
    pub fn stop_handle(&self) -> StopRequest {
        StopRequest::new({
            let supervisor = Arc::clone(&self.supervisor);
            move || {
                let supervisor = Arc::clone(&supervisor);
                tokio::spawn(async move {
                    let _ = supervisor.stop().await;
                });
            }
        })
    }
}

/// Wait for the engine's controller to answer, and return its address.
///
/// The engine publishes its address in a file named after its own pid. The
/// supervisor knows that pid, so there is nothing to guess.
async fn wait_for_controller(supervisor: &Supervisor, secret: &str) -> Option<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while std::time::Instant::now() < deadline {
        if let Some(address) = supervisor.controller_address() {
            let client = ControllerClient::new(format!("http://{address}"), secret);
            if client.is_ready().await {
                return Some(address);
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_engine_is_stopped() {
        let engine = MeowEngine::new(PathBuf::from("meow"));
        assert_eq!(engine.binary(), std::path::Path::new("meow"));
    }

    #[tokio::test]
    async fn an_engine_without_a_binary_cannot_start() {
        let engine = MeowEngine::new(PathBuf::from("no-such-engine-binary"));
        let config = RuntimeConfig {
            mixed_port: Some(7890),
            socks_port: None,
            port: None,
            allow_lan: false,
            bind_address: None,
            mode: starpod_core::domain::ProxyMode::Rule,
            log_level: starpod_core::domain::LogLevel::Info,
            tun: starpod_core::domain::TunSettings::default(),
            proxies: Vec::new(),
            proxy_groups: Vec::new(),
            rules: vec!["MATCH,PROXY".to_string()],
            rule_providers: Vec::new(),
            subscriptions: Vec::new(),
        };
        let error = match engine.start(&config).await {
            Ok(_) => panic!("the engine should not start without a binary"),
            Err(error) => error,
        };
        assert!(matches!(error, EngineError::NotInstalled(_)), "{error:?}");
    }

    #[tokio::test]
    async fn the_status_of_an_engine_that_never_started_is_stopped() {
        let engine = MeowEngine::new(PathBuf::from("no-such-engine-binary"));
        let status = engine.status().await.expect("status");
        assert_eq!(status.phase, EnginePhase::Stopped);
        assert!(!status.controller_ready);
    }
}
