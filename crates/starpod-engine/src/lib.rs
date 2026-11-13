//! The proxy core: the engine abstraction and the meow implementation.
//!
//! Nothing outside this crate knows that the proxy core is meow, that it
//! runs as a subprocess, that it has a REST controller, or what any of its
//! configuration keys are called. The rest of the app sees [`ProxyEngine`]
//! and the app's own domain types.
//!
//! The crate's modules, roughly in dependency order:
//!
//! | module | what it does |
//! |--------|---------------|
//! | [`domain`] (in `starpod-core`) | the app's own vocabulary |
//! | [`config`] | the app's configuration → the engine's YAML |
//! | [`validate`] | the engine's own parser checks a candidate |
//! | [`download`] | installing the engine binary |
//! | [`secret`] | generating the controller's secret |
//! | [`log`] | the engine's output, redacted and bounded |
//! | [`rest`] | the controller's REST API |
//! | [`supervisor`] | the engine's process lifetime |
//! | [`tun`] | TUN mode and the elevation it needs |
//! | [`engine`] | the trait the rest of the app sees |

pub mod config;
pub mod download;
pub mod engine;
pub mod error;
pub mod log;
pub mod meow;
pub mod rest;
pub mod secret;
pub mod supervisor;
pub mod tun;
pub mod validate;

pub use engine::{
    EngineHandle, EnginePhase, EngineStatus, ExitInfo, ProxyEngine, StopRequest, ValidationReport,
};
pub use error::{EngineError, EngineResult};
pub use log::{LogBuffer, LogLine, LogStream, LogStreamKind, Redactor};
pub use meow::MeowEngine;
