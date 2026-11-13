//! The errors the service reports to the UI.
//!
//! Every message here is already redacted. The service scrubs anything it
//! received from the engine, the secret store, the network or the disk before
//! it becomes an `AppError`, so the text can be shown to the user or written
//! to the log as it is.

use thiserror::Error;

/// One rejected line of raw rule text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleLineError {
    /// The 1-based line number in the text the user pasted.
    pub line: usize,
    /// Why the line was rejected.
    pub reason: String,
}

/// Everything the service can fail with.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AppError {
    /// The input the user gave is not acceptable.
    #[error("{0}")]
    Invalid(String),
    /// Raw rule text had lines the parser rejected. Nothing was saved.
    #[error("the rules were not saved, nothing was changed: {}", join_rule_errors(.0))]
    RuleLines(Vec<RuleLineError>),
    /// The settings do not build into a configuration. Nothing was applied.
    #[error("the settings cannot be applied, nothing was changed: {}", .0.join("; "))]
    Build(Vec<String>),
    /// The engine's own parser rejected the candidate. The running proxy was
    /// not touched.
    #[error("the engine rejected the configuration, so nothing was changed: {0}")]
    EngineRejected(String),
    /// The proxy engine failed.
    #[error("the proxy engine failed: {0}")]
    Engine(String),
    /// A subscription could not be fetched or understood.
    #[error("{0}")]
    Subscription(String),
    /// The app's files could not be read or written.
    #[error("{0}")]
    Storage(String),
    /// The secret store could not be read or written.
    #[error("{0}")]
    Secret(String),
    /// The thing the caller named does not exist.
    #[error("{0} was not found")]
    NotFound(String),
    /// The saved state was written by a newer Starpod. It is kept as it is,
    /// so a change from this version would overwrite data it cannot read.
    #[error(
        "the saved state was written by a newer version of Starpod; it is read-only here, so nothing was changed"
    )]
    ReadOnly,
}

fn join_rule_errors(errors: &[RuleLineError]) -> String {
    errors
        .iter()
        .map(|error| format!("line {}: {}", error.line, error.reason))
        .collect::<Vec<_>>()
        .join("; ")
}
