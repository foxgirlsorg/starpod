//! The engine's error types.
//!
//! Every variant carries a **user-facing** message. The app shows these
//! strings; it does not format its own messages from the error's parts. That
//! keeps the wording of an engine failure in one place, next to the code
//! that produced it.
//!
//! No variant ever carries the controller secret or a subscription URL.
//! Those are the two values GOAL.md §6 names as secrets, and an error
//! message is the last place they should end up — error messages are
//! logged, shown in a toast, and copied to the clipboard.

use thiserror::Error;

/// Something went wrong talking to the proxy engine.
#[derive(Debug, Error)]
pub enum EngineError {
    /// The engine's own message, verbatim.
    ///
    /// The engine's stderr is the authoritative explanation of why a
    /// configuration was rejected, so it is passed through as-is rather
    /// than reworded.
    #[error("the proxy engine rejected the configuration: {0}")]
    EngineRejected(String),

    /// The controller API answered 401 — the secret was wrong or missing.
    #[error("the proxy engine rejected the app’s access token")]
    Unauthorized,

    /// The controller API answered 404 for something that should exist.
    #[error("the proxy engine does not have that resource")]
    NotFound,

    /// The controller API answered 400 — the request was malformed.
    #[error("the proxy engine rejected the request: {0}")]
    BadRequest(String),

    /// The controller API is not answering — the engine is down or still
    /// starting.
    #[error("the proxy engine is not responding")]
    EngineDown,

    /// A request to the controller timed out.
    #[error("the proxy engine did not respond in time")]
    Timeout,

    /// The connection to the controller failed.
    #[error("could not reach the proxy engine: {0}")]
    Transport(String),

    /// The engine process could not be started.
    #[error("could not start the proxy engine: {0}")]
    Spawn(String),

    /// The engine process ended unexpectedly.
    #[error("the proxy engine stopped unexpectedly (code {code:?})")]
    Crashed { code: Option<i32> },

    /// The engine binary is missing or could not be installed.
    #[error("the proxy engine is not installed: {0}")]
    NotInstalled(String),

    /// The download of the engine binary failed or failed verification.
    #[error("could not install the proxy engine: {0}")]
    Install(String),

    /// The engine needs elevated privileges that were not granted.
    #[error("the proxy engine needs administrator permission for TUN mode")]
    ElevationDenied,

    /// A filesystem operation failed.
    #[error("could not access the configuration file: {0}")]
    Io(String),

    /// The engine took an unexpected action.
    #[error("unexpected engine condition: {0}")]
    Internal(String),
}

impl EngineError {
    /// Whether this error means the engine is not usable in its current
    /// state, as opposed to a request that was simply rejected.
    ///
    /// The app uses this to decide whether to offer a restart.
    pub fn is_fatal(&self) -> bool {
        matches!(
            self,
            Self::EngineDown | Self::Crashed { .. } | Self::NotInstalled(_) | Self::Spawn(_)
        )
    }

    /// Whether this error is the user refusing an elevation prompt.
    pub fn is_elevation_denied(&self) -> bool {
        matches!(self, Self::ElevationDenied)
    }
}

/// The result of a controller API call.
pub type EngineResult<T> = Result<T, EngineError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejection_carries_the_engines_own_message() {
        let err = EngineError::EngineRejected("unknown rule type: NOPE".to_string());
        assert!(err.to_string().contains("unknown rule type: NOPE"));
    }

    #[test]
    fn a_down_engine_is_fatal() {
        assert!(EngineError::EngineDown.is_fatal());
        assert!(!EngineError::BadRequest("x".to_string()).is_fatal());
    }

    #[test]
    fn the_elevation_denial_is_recognisable() {
        assert!(EngineError::ElevationDenied.is_elevation_denied());
        assert!(!EngineError::EngineDown.is_elevation_denied());
    }
}
