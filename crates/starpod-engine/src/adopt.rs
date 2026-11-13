//! The ownership marker: how a new app instance finds an engine it did not start.
//!
//! The engine outlives the app when the app is killed, crashes or loses
//! power. A marker in the app's own controller directory records the engine's
//! pid, its controller address and the secret the controller expects. The
//! next start reads it and asks the controller the one question that proves
//! ownership: does the engine answer with *this* secret?
//!
//! Only a controller that accepts the marker's secret is adopted. The pid is
//! recorded for the stop path, not trusted as proof of identity, because a
//! pid can be recycled and a controller on the same port can belong to
//! anything. A marker whose controller does not answer is stale and is
//! deleted.
//!
//! # Where the secret lives
//!
//! The marker holds the controller secret, which is the engine's only
//! credential. It is written owner-only (0600 on unix; a per-user DACL on
//! Windows through `starpod_store::permissions`), and it is never logged.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{EngineError, EngineResult};
use crate::rest::ControllerClient;

/// The marker's file name, inside the app's controller directory.
pub const MARKER_FILE: &str = "engine-owner.json";

/// What the app records about the engine it started.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnershipMarker {
    /// The engine's process id. On unix an elevated engine's pid is the one
    /// the wrapper recorded, which is the engine's own pid after `exec`.
    pub pid: u32,
    /// The controller's address, `127.0.0.1:<port>`.
    pub controller: String,
    /// The controller secret. Never shown or logged.
    pub secret: String,
    /// Whether the engine was started elevated (for TUN).
    #[serde(default)]
    pub elevated: bool,
}

impl std::fmt::Debug for OwnershipMarker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnershipMarker")
            .field("pid", &self.pid)
            .field("controller", &self.controller)
            .field("secret", &"<redacted>")
            .field("elevated", &self.elevated)
            .finish()
    }
}

/// The marker's full path in `dir`.
pub fn marker_path(dir: &Path) -> PathBuf {
    dir.join(MARKER_FILE)
}

/// Record `marker` in `dir`, replacing any earlier one.
///
/// The bytes go to a temporary file that is then renamed over the marker, so
/// a reader sees either the old marker or the new one.
pub fn write_marker(dir: &Path, marker: &OwnershipMarker) -> EngineResult<()> {
    std::fs::create_dir_all(dir).map_err(|error| EngineError::Io(error.to_string()))?;
    let bytes =
        serde_json::to_vec(marker).map_err(|error| EngineError::Internal(error.to_string()))?;
    let temporary = dir.join(format!("{MARKER_FILE}.tmp"));
    let _ = std::fs::remove_file(&temporary);
    if let Err(error) = write_owner_only(&temporary, &bytes) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    std::fs::rename(&temporary, marker_path(dir)).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        EngineError::Io(error.to_string())
    })
}

/// Read the marker in `dir`, if there is a readable one.
///
/// On unix a marker that another user owns is ignored: it could point the app
/// at a controller that user controls.
pub fn read_marker(dir: &Path) -> Option<OwnershipMarker> {
    let path = marker_path(dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let owner = std::fs::metadata(&path).ok()?.uid();
        if owner != nix::unistd::geteuid().as_raw() {
            return None;
        }
    }
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Delete the marker in `dir`, if any. Missing is not an error.
pub fn remove_marker(dir: &Path) {
    let _ = std::fs::remove_file(marker_path(dir));
}

/// Write `bytes` to a new file at `path` that only the current user can read.
fn write_owner_only(path: &Path, bytes: &[u8]) -> EngineResult<()> {
    use std::io::Write as _;
    let io = |error: std::io::Error| EngineError::Io(error.to_string());

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(io)?;
        file.write_all(bytes).map_err(io)?;
    }
    #[cfg(not(unix))]
    {
        let mut file = std::fs::File::create(path).map_err(io)?;
        file.write_all(bytes).map_err(io)?;
        drop(file);
        starpod_store::permissions::restrict_to_current_user(path)
            .map_err(|error| EngineError::Io(error.to_string()))?;
    }
    Ok(())
}

/// What a controller said when asked with a marker's secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Probe {
    /// It answered with the secret: it is the engine this marker describes.
    Answers,
    /// It answered 401: something is listening, but not with our secret.
    Refused,
    /// It did not answer at all: the engine is gone, or it is still starting.
    Silent,
}

/// Ask the controller named in `marker` whether it accepts the marker's secret.
pub async fn probe(marker: &OwnershipMarker) -> Probe {
    let client = ControllerClient::new(
        format!("http://{}", marker.controller),
        marker.secret.clone(),
    );
    match client.version().await {
        Ok(_) => Probe::Answers,
        Err(EngineError::Unauthorized) => Probe::Refused,
        Err(_) => Probe::Silent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn marker(controller: String, secret: &str) -> OwnershipMarker {
        OwnershipMarker {
            pid: 4321,
            controller,
            secret: secret.to_string(),
            elevated: false,
        }
    }

    #[test]
    fn a_marker_round_trips_through_its_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        let written = marker("127.0.0.1:9090".to_string(), "s3cret");
        write_marker(dir.path(), &written).unwrap();
        assert_eq!(read_marker(dir.path()), Some(written));
    }

    #[test]
    fn a_marker_is_replaced_not_appended_to() {
        let dir = tempfile::TempDir::new().unwrap();
        write_marker(dir.path(), &marker("127.0.0.1:1".to_string(), "a")).unwrap();
        let second = marker("127.0.0.1:2".to_string(), "b");
        write_marker(dir.path(), &second).unwrap();
        assert_eq!(read_marker(dir.path()), Some(second));
        assert!(!dir.path().join(format!("{MARKER_FILE}.tmp")).exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_marker_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::TempDir::new().unwrap();
        write_marker(dir.path(), &marker("127.0.0.1:1".to_string(), "a")).unwrap();
        let mode = std::fs::metadata(marker_path(dir.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "mode was {mode:o}");
    }

    #[test]
    fn the_debug_form_never_shows_the_secret() {
        let shown = format!("{:?}", marker("127.0.0.1:1".to_string(), "hunter2"));
        assert!(!shown.contains("hunter2"), "{shown}");
        assert!(shown.contains("<redacted>"));
    }

    #[test]
    fn a_missing_or_garbled_marker_reads_as_none() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(read_marker(dir.path()), None);
        std::fs::write(marker_path(dir.path()), "{ not json").unwrap();
        assert_eq!(read_marker(dir.path()), None);
    }

    #[test]
    fn removing_a_marker_twice_is_not_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        write_marker(dir.path(), &marker("127.0.0.1:1".to_string(), "a")).unwrap();
        remove_marker(dir.path());
        remove_marker(dir.path());
        assert_eq!(read_marker(dir.path()), None);
    }

    #[tokio::test]
    async fn a_controller_that_accepts_the_secret_is_recognised() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .and(header("authorization", "Bearer good"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "version": "0.29.0" })),
            )
            .mount(&server)
            .await;
        let answer = probe(&marker(server.address().to_string(), "good")).await;
        assert_eq!(answer, Probe::Answers);
    }

    #[tokio::test]
    async fn a_controller_that_refuses_the_secret_is_not_adopted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/version"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let answer = probe(&marker(server.address().to_string(), "wrong")).await;
        assert_eq!(answer, Probe::Refused);
    }

    #[tokio::test]
    async fn a_port_with_nothing_on_it_is_silent() {
        // Bind and drop, so the port is free and refuses connections.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let answer = probe(&marker(format!("127.0.0.1:{port}"), "any")).await;
        assert_eq!(answer, Probe::Silent);
    }
}
