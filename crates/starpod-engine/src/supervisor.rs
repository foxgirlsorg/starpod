//! Running the proxy engine as a supervised child process.
//!
//! The supervisor owns the engine's lifetime. It starts the child, drains
//! its output, notices when it dies, and stops it gracefully.
//!
//! # Graceful shutdown
//!
//! The engine shuts down on `SIGTERM`/`SIGINT` (unix) and on a console
//! control event (Windows). It closes its listeners and its connections on
//! the way out, so a graceful stop does not drop the user's traffic on the
//! floor — an abrupt kill does.
//!
//! On Windows the child is created in its own process group and receives
//! `CTRL_BREAK_EVENT`. `CTRL_C_EVENT` cannot be used: the Windows console
//! gives it to every process in the console, including the app, and the app
//! has its own idea about when to quit. `CTRL_BREAK_EVENT` can be targeted
//! at one process group.
//!
//! If the child does not exit within the grace period, it is killed. A
//! stuck engine must not keep the app from quitting.

use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use starpod_core::domain::RuntimeConfig;

use crate::config::EngineDocument;
use crate::error::{EngineError, EngineResult};
use crate::log::{LogBuffer, LogStreamKind, Redactor};
use crate::validate::validate_config;

/// How long the engine may take to exit after a graceful stop request.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// The directory the engine writes its controller file into.
///
/// The engine publishes its controller's address there, in a file named
/// after its own pid. The file is the only reliable way to learn the port
/// the engine bound when the configuration asked for port zero.
pub fn controller_file_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MEOW_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return Some(PathBuf::from(dir));
    }
    #[cfg(windows)]
    {
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(PathBuf::from(local).join("meow").join("controllers"))
    }
    #[cfg(all(unix, target_os = "macos"))]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join("Library/Caches/meow/controllers"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = std::env::var_os("HOME")?;
        Some(PathBuf::from(home).join(".cache/meow/controllers"))
    }
}

/// The controller's address, read from the engine's controller file.
///
/// The file is written by the engine once its controller is bound, and
/// removed when it exits.
pub fn read_controller_address(pid: u32) -> Option<String> {
    let dir = controller_file_dir()?;
    let path = dir.join(format!("{pid}.json"));
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let controller = value.get("controller")?.as_str()?;
    Some(controller.to_string())
}

/// Where the engine's configuration file lives.
pub fn config_path() -> PathBuf {
    crate::download::install_dir()
        .parent()
        .map(|parent| parent.join("runtime.yaml"))
        .unwrap_or_else(|| PathBuf::from("runtime.yaml"))
}

/// The controller's address: loopback only.
///
/// The controller API can rewrite the engine's configuration and read its
/// traffic. It must never be reachable from the network.
pub const CONTROLLER_ADDRESS: &str = "127.0.0.1:0";

/// A running engine.
pub struct EngineProcess {
    child: Child,
    /// The secret the controller expects.
    secret: String,
    /// The engine's process id, for the controller file.
    pid: u32,
}

impl EngineProcess {
    /// Start the engine with a configuration.
    ///
    /// The configuration is written to a file first and the engine is
    /// started on it. Writing it to a file — rather than passing it inline
    /// — means the engine's own diagnostics name a file the user can look
    /// at, and it means the file's permissions can be restricted.
    pub async fn start(
        binary: &std::path::Path,
        config: &RuntimeConfig,
        secret: &str,
        logs: &LogBuffer,
        redactor: &Redactor,
    ) -> EngineResult<Self> {
        let document = EngineDocument::build(config).with_controller(CONTROLLER_ADDRESS, secret);
        let yaml = document
            .to_yaml()
            .map_err(|e| EngineError::Internal(e.to_string()))?;

        let path = config_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| EngineError::Io(e.to_string()))?;
        }
        write_private_file(&path, yaml.as_bytes())
            .await
            .map_err(|e| EngineError::Io(e.to_string()))?;

        let mut command = Command::new(binary);
        command
            .arg("-f")
            .arg(&path)
            .arg("--secret")
            .arg(secret)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        #[cfg(windows)]
        {
            // A new process group, so a `CTRL_BREAK_EVENT` reaches the
            // engine and not the app.
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        let mut child = command
            .spawn()
            .map_err(|e| EngineError::Spawn(e.to_string()))?;

        // Drain the child's output. This runs for the child's whole
        // lifetime and must never be skipped: a pipe that fills up blocks
        // the engine.
        if let Some(stdout) = child.stdout.take() {
            spawn_drain(
                stdout,
                LogStreamKind::Stdout,
                logs.clone(),
                redactor.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_drain(
                stderr,
                LogStreamKind::Stderr,
                logs.clone(),
                redactor.clone(),
            );
        }

        Ok(Self {
            pid: child.id().unwrap_or(0),
            child,
            secret: secret.to_string(),
        })
    }

    /// The engine's process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The controller's address, once the engine has published it.
    pub fn controller_address(&self) -> Option<String> {
        read_controller_address(self.pid)
    }

    /// The controller's secret.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Wait for the child to exit.
    pub async fn wait(&mut self) -> EngineResult<crate::engine::ExitInfo> {
        let status = self
            .child
            .wait()
            .await
            .map_err(|e| EngineError::Internal(e.to_string()))?;
        Ok(crate::engine::ExitInfo {
            code: status.code(),
            #[cfg(unix)]
            signal: signal_of(&status),
            requested: false,
        })
    }

    /// Ask the child to stop, gracefully.
    ///
    /// The engine closes its listeners and its connections when it receives
    /// the signal. If it has not exited within the grace period, it is
    /// killed.
    pub async fn stop(&mut self) -> EngineResult<()> {
        if self.child.try_wait().ok().flatten().is_some() {
            return Ok(());
        }

        #[cfg(unix)]
        {
            // SAFETY: `id()` is the child's pid, which the engine owns.
            // The signal is sent to the process, which is what SIGTERM is
            // for; a pid that has already exited is a no-op.
            unsafe {
                nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(self.child.id().unwrap_or(0) as i32),
                    nix::sys::signal::Signal::SIGTERM,
                )
                .ok();
            }
        }

        #[cfg(windows)]
        {
            // `CTRL_BREAK_EVENT` to the child's process group.
            let pid = self.child.id().unwrap_or(0);
            // SAFETY: `pid` is the child's pid, and the child's process
            // group is the one it was created in.
            unsafe {
                windows_sys::Win32::System::Console::GenerateConsoleCtrlEvent(
                    windows_sys::Win32::System::Console::CTRL_BREAK_EVENT,
                    pid,
                );
            }
        }

        let grace = tokio::time::timeout(STOP_GRACE, self.child.wait()).await;
        match grace {
            Ok(_) => Ok(()),
            Err(_) => {
                // The child ignored the request. Kill it: the app must be
                // able to quit.
                self.child
                    .kill()
                    .await
                    .map_err(|e| EngineError::Internal(e.to_string()))?;
                let _ = self.child.wait().await;
                Ok(())
            }
        }
    }
}

/// The `CREATE_NEW_PROCESS_GROUP` flag.
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// The signal that killed a process, on unix.
#[cfg(unix)]
fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal()
}

/// Drain a child's output into the log buffer.
///
/// The task runs until the pipe closes, which is when the child exits.
fn spawn_drain<R>(reader: R, kind: LogStreamKind, logs: LogBuffer, redactor: Redactor)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            logs.push(kind, redactor.redact(&line));
        }
    });
}

/// Write a file that only its owner can read.
///
/// The configuration file holds subscription URLs, which carry tokens. It
/// must not be readable by other users on the machine.
async fn write_private_file(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .await?;
        file.write_all(bytes).await?;
        file.flush().await?;
    }
    #[cfg(not(unix))]
    {
        let mut file = tokio::fs::File::create(path).await?;
        file.write_all(bytes).await?;
        file.flush().await?;
    }
    Ok(())
}

use tokio::io::AsyncWriteExt as _;

/// The supervisor's public face: start, watch, stop.
pub struct Supervisor {
    /// The engine's logs.
    pub logs: LogBuffer,
    /// The secret in use.
    secret: String,
    /// The child, when it is running.
    child: Arc<tokio::sync::Mutex<Option<EngineProcess>>>,
    /// Fires when the engine exits.
    exited: watch::Sender<Option<crate::engine::ExitInfo>>,
    /// The controller's address, once known.
    controller: Arc<std::sync::RwLock<String>>,
    /// The redactor for the logs.
    redactor: Redactor,
}

impl Supervisor {
    /// Build a supervisor with a fresh secret.
    ///
    /// There is deliberately no `Default`: a supervisor's secret is
    /// generated per instance, and a default implementation would make
    /// two supervisors share one by accident.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let secret = crate::secret::random_secret();
        let (exited, _) = watch::channel(None);
        Self {
            logs: LogBuffer::new(2000),
            secret: secret.clone(),
            child: Arc::new(tokio::sync::Mutex::new(None)),
            exited,
            controller: Arc::new(std::sync::RwLock::new(String::new())),
            redactor: Redactor::new([secret.clone()]),
        }
    }

    /// The controller's address.
    pub fn controller(&self) -> String {
        self.controller
            .read()
            .map(|value| value.clone())
            .unwrap_or_default()
    }

    /// The controller's address, read from the engine's controller file.
    ///
    /// This does not wait for the lock: it is called while the engine is
    /// starting, from a task that would deadlock on it.
    pub fn controller_address(&self) -> Option<String> {
        let guard = self.child.try_lock().ok()?;
        guard
            .as_ref()
            .and_then(|process| process.controller_address())
    }

    /// The secret.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// A watch receiver for the exit signal.
    pub fn exited(&self) -> watch::Receiver<Option<crate::engine::ExitInfo>> {
        self.exited.subscribe()
    }

    /// Start the engine.
    pub async fn start(
        &self,
        binary: &std::path::Path,
        config: &RuntimeConfig,
    ) -> EngineResult<crate::engine::EngineHandle> {
        let mut guard = self.child.lock().await;
        if guard.is_some() {
            return Err(EngineError::Internal(
                "the engine is already running".to_string(),
            ));
        }

        let process =
            EngineProcess::start(binary, config, &self.secret, &self.logs, &self.redactor).await?;
        *guard = Some(process);

        let sender = self.exited.clone();
        let child = Arc::clone(&self.child);
        tokio::spawn(async move {
            let mut guard = child.lock().await;
            let Some(process) = guard.as_mut() else {
                return;
            };
            let info = process.wait().await.unwrap_or(crate::engine::ExitInfo {
                code: None,
                #[cfg(unix)]
                signal: None,
                requested: false,
            });
            let _ = guard.take();
            let _ = sender.send(Some(info));
        });

        Ok(crate::engine::EngineHandle {
            exited: self.exited.subscribe(),
            stop: crate::engine::StopRequest::new({
                let child = Arc::clone(&self.child);
                move || {
                    let child = Arc::clone(&child);
                    tokio::spawn(async move {
                        let mut guard = child.lock().await;
                        if let Some(process) = guard.as_mut() {
                            let _ = process.stop().await;
                        }
                    });
                }
            }),
        })
    }

    /// Stop the engine.
    pub async fn stop(&self) -> EngineResult<()> {
        let mut guard = self.child.lock().await;
        if let Some(process) = guard.as_mut() {
            process.stop().await?;
        }
        *guard = None;
        Ok(())
    }
}

/// Validate a configuration with the engine's own parser.
///
/// This runs the engine binary with `-t`, which parses the configuration
/// and exits without starting the proxy. It is the authoritative check that
/// a configuration the app built is one the engine will accept.
pub async fn validate_with_engine(
    binary: &std::path::Path,
    config: &RuntimeConfig,
) -> EngineResult<crate::engine::ValidationReport> {
    validate_config(binary, config).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_controller_address_is_loopback_only() {
        assert!(
            CONTROLLER_ADDRESS.starts_with("127.0.0.1:"),
            "{CONTROLLER_ADDRESS}"
        );
    }

    #[test]
    fn the_config_path_sits_beside_the_engine() {
        let path = config_path();
        assert!(path.ends_with("runtime.yaml"), "{path:?}");
    }

    #[tokio::test]
    async fn a_supervisor_has_a_secret_and_no_controller_until_it_starts() {
        let supervisor = Supervisor::new();
        assert!(!supervisor.secret().is_empty());
        assert_eq!(supervisor.controller(), "");
    }
}
