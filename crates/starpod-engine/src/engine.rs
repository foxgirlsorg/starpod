//! The proxy core abstraction.
//!
//! GOAL.md §2 asks for an abstraction around the core "so GUI/business logic
//! don't depend on its internals", while also warning against "unnecessary
//! abstractions". The resolution is **one trait with seven methods**,
//! implemented by a `MeowEngine` that drives the engine as a subprocess.
//!
//! This trait is the *only* thing the GUI and the business logic see.
//! Nothing outside `starpod-engine` names a REST route, spawns a process, or
//! knows the word "meow". That property is enforced by the crate boundary:
//! `starpod-engine` is the only crate that imports `reqwest`,
//! `tokio::process`, or knows the engine's CLI.
//!
//! # Why a subprocess and not a library
//!
//! The engine is a separate workspace of crates with its own release cadence.
//! Linking it in would tie the app to its exact version and would pull its
//! whole dependency tree into the app's binary. Driving it as a subprocess
//! means the app can upgrade the engine independently, and — more
//! importantly — that a crashing engine cannot take the app down with it.

use std::sync::Arc;

use async_trait::async_trait;
use starpod_core::domain::{RuntimeConfig, TunSettings};

use crate::connections::ConnectionsReply;
use crate::error::EngineError;
use crate::log::LogStream;
use crate::reassert::{EngineSubscription, LiveProxy};

/// The lifecycle phase of the proxy engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum EnginePhase {
    /// The engine is not running.
    #[default]
    Stopped,
    /// The engine is starting; the controller is not answering yet.
    Starting,
    /// The engine is running and its controller is answering.
    Running,
    /// The engine is being asked to stop.
    Stopping,
    /// The engine stopped unexpectedly, or failed to start.
    Failed,
}

impl EnginePhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopping => "stopping",
            Self::Failed => "failed",
        }
    }

    /// Whether the engine is up and its controller is answering.
    pub const fn is_running(self) -> bool {
        matches!(self, Self::Running)
    }
}

/// A handle to a running engine.
///
/// The handle is how the rest of the app watches the engine without holding
/// the engine itself: it can report whether the child is still alive, ask
/// it to stop, and report how it ended.
#[derive(Debug)]
pub struct EngineHandle {
    /// A signal that fires when the engine stops for any reason.
    pub exited: tokio::sync::watch::Receiver<Option<ExitInfo>>,
    /// A handle that asks the engine to stop gracefully.
    pub stop: StopRequest,
}

/// How the engine ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    /// The process exit code, if it exited normally.
    pub code: Option<i32>,
    /// The signal that killed it, on unix.
    #[cfg(unix)]
    pub signal: Option<i32>,
    /// Whether the engine was asked to stop, as opposed to crashing.
    pub requested: bool,
}

impl ExitInfo {
    /// An exit whose cause is not known.
    pub fn unknown() -> Self {
        Self {
            code: None,
            #[cfg(unix)]
            signal: None,
            requested: false,
        }
    }

    /// The exit described by a process status. `requested` starts false; the
    /// supervisor sets it when the stop was asked for.
    pub fn from_status(status: std::process::ExitStatus) -> Self {
        Self {
            code: status.code(),
            #[cfg(unix)]
            signal: {
                use std::os::unix::process::ExitStatusExt as _;
                status.signal()
            },
            requested: false,
        }
    }
}

/// A request to stop the engine.
#[derive(Clone)]
pub struct StopRequest {
    inner: Arc<dyn Fn() + Send + Sync>,
}

impl StopRequest {
    /// Build a stop request from a closure.
    pub fn new(f: impl Fn() + Send + Sync + 'static) -> Self {
        Self { inner: Arc::new(f) }
    }

    /// Ask the engine to stop. Idempotent.
    pub fn stop(&self) {
        (self.inner)();
    }
}

impl std::fmt::Debug for StopRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StopRequest")
    }
}

/// The engine's operational state.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EngineStatus {
    /// The lifecycle phase.
    pub phase: EnginePhase,
    /// The routing mode the engine is running in.
    pub mode: Option<String>,
    /// How long the engine has been running, in seconds.
    pub uptime_secs: Option<u64>,
    /// The engine's own version string.
    pub version: Option<String>,
    /// The last error the engine reported.
    pub last_error: Option<String>,
    /// Whether the controller API is answering.
    pub controller_ready: bool,
    /// Whether the engine is running elevated (needed for TUN).
    pub elevated: bool,
}

/// The result of validating a candidate configuration.
///
/// A report and not a bare `Result`: the engine-side check (`meow -t`) is
/// authoritative, but it is not the whole story — the app-side layer also
/// produces findings, including *warnings* that must not block the apply.
/// A bare `Result<(), E>` would force the app to either lose those warnings
/// or treat them as failures.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ValidationReport {
    /// True only if the engine accepted the configuration.
    pub ok: bool,
    /// The engine's own message, verbatim.
    pub engine_error: Option<String>,
    /// Non-blocking findings from the app-side layer.
    pub warnings: Vec<String>,
}

/// The proxy core, as the rest of the app sees it.
#[async_trait]
pub trait ProxyEngine: Send + Sync {
    /// Start the engine with the given configuration.
    ///
    /// The configuration is written to the engine's configured path first,
    /// and the engine is started on that file.
    async fn start(&self, cfg: &RuntimeConfig) -> Result<EngineHandle, EngineError>;

    /// Stop the engine gracefully.
    async fn stop(&self) -> Result<(), EngineError>;

    /// Replace the running configuration with a new one.
    ///
    /// This is a hot reload: the engine closes its existing connections, so
    /// the app warns the user before calling it.
    ///
    /// The implementation validates the candidate before applying it, and
    /// the running configuration is left untouched if the check fails.
    async fn reload(&self, cfg: &RuntimeConfig) -> Result<(), EngineError>;

    /// The engine's operational state.
    async fn status(&self) -> Result<EngineStatus, EngineError>;

    /// Validate a candidate configuration *without* applying it.
    ///
    /// This is the authoritative pre-apply check: it runs the engine's own
    /// parser over the candidate, so a rule the app thought was valid but
    /// the engine rejects is caught before the running configuration is
    /// touched.
    async fn validate(&self, cfg: &RuntimeConfig) -> Result<ValidationReport, EngineError>;

    /// Turn the TUN inbound on or off, live.
    async fn set_tun(&self, tun: &TunSettings) -> Result<(), EngineError>;

    /// Choose `member` in the `select` group `group`, on the running engine.
    ///
    /// Unlike a reload, this closes no connections. It fails with
    /// [`EngineError::EngineDown`] when the engine is not running.
    async fn select(&self, group: &str, member: &str) -> Result<(), EngineError>;

    /// Measure the latency of `proxy` against `url`, in milliseconds.
    ///
    /// `timeout_ms` bounds the probe. It fails with
    /// [`EngineError::EngineDown`] when the engine is not running.
    async fn delay(&self, proxy: &str, url: &str, timeout_ms: u32) -> Result<u32, EngineError>;

    /// Measure every member of a proxy group at once, in milliseconds. Only
    /// the members that answered are returned.
    ///
    /// The default refuses, so an engine without the call says so rather than
    /// returning an empty result that looks like "nothing answered".
    async fn group_delay(
        &self,
        _group: &str,
        _url: &str,
        _timeout_ms: u32,
    ) -> Result<std::collections::BTreeMap<String, u32>, EngineError> {
        Err(EngineError::Unsupported(
            "group latency tests are not available for this engine".to_string(),
        ))
    }

    /// The engine's log stream.
    fn logs(&self) -> LogStream;

    // ----- engine-managed subscriptions (ARCHITECTURE §4.6) ----------------
    //
    // The methods below are optional. A proxy core that cannot offer one
    // answers `EngineError::Unsupported`, which the caller treats as "nothing
    // to re-assert".

    /// The engine's rule list, in evaluation order.
    async fn rules(&self) -> Result<Vec<String>, EngineError> {
        Err(EngineError::Unsupported(
            "reading the rule list".to_string(),
        ))
    }

    /// Replace the engine's rule list with `rules`, in that order.
    async fn replace_rules(&self, _rules: &[String]) -> Result<(), EngineError> {
        Err(EngineError::Unsupported(
            "replacing the rule list".to_string(),
        ))
    }

    /// The engine-managed subscriptions and when each last refreshed.
    async fn engine_subscriptions(&self) -> Result<Vec<EngineSubscription>, EngineError> {
        Err(EngineError::Unsupported(
            "listing subscriptions".to_string(),
        ))
    }

    /// The proxies in the engine's live configuration.
    async fn proxies(&self) -> Result<Vec<LiveProxy>, EngineError> {
        Err(EngineError::Unsupported("listing proxies".to_string()))
    }

    /// The engine's open connections, with the traffic totals.
    async fn connections(&self) -> Result<ConnectionsReply, EngineError> {
        Err(EngineError::Unsupported("listing connections".to_string()))
    }

    /// Close one open connection by its id.
    async fn close_connection(&self, _id: &str) -> Result<(), EngineError> {
        Err(EngineError::Unsupported("closing a connection".to_string()))
    }

    /// Close every open connection.
    async fn close_all_connections(&self) -> Result<(), EngineError> {
        Err(EngineError::Unsupported("closing connections".to_string()))
    }

    /// Re-attach to an engine that an earlier run of the app left running.
    ///
    /// Returns `Ok(true)` when a live engine answering with this app's secret
    /// was found and is now the one this engine controls, and `Ok(false)`
    /// when there is none. The default finds nothing.
    async fn adopt(&self) -> Result<bool, EngineError> {
        Ok(false)
    }
}
