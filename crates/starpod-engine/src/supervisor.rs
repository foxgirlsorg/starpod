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
//!
//! # The command line
//!
//! The engine is started as
//!
//! ```text
//! meow -f <config-path> --secret <secret>
//! ```
//!
//! The **secret** is on the command line and nowhere else. It is not in the
//! config file: the engine accepts a top-level `secret:` key
//! (`meow-config-0.29.0/src/raw.rs:205`), and the app's own writer refuses
//! to produce one (`starpod_store::runtime_config::reject_secret_key`).
//! The engine overrides `config.api.secret` with `--secret` *after* the
//! config is built (`meow-app-0.29.0/src/main.rs:612-615`), so the value
//! the API actually checks is the one the app passed
//! (`meow-api-0.29.0/src/routes.rs:152`).
//!
//! A secret on a child's command line is visible to other processes of the
//! same user on some systems. That is a deliberate trade-off: it is the
//! only interface the engine offers, the engine's own controller file
//! stores the secret in the same user's home directory anyway, and the
//! alternative — putting it in the config file — would spread it to every
//! reader of that file, including the backup tools a user might run.
//!
//! # The controller address
//!
//! The controller's address is in the **config file**, not on the command
//! line, and that is not a stylistic choice. The engine binds its API
//! socket exactly once, at startup (`meow-app-0.29.0/src/main.rs:1508-1571`),
//! and the `PUT /configs` reload path never rebinds it. A reload therefore
//! cannot move the controller — so the address that the file names is the
//! address the controller keeps, for the engine's whole life.
//!
//! The app asks for port `0`, so the OS assigns a free port, and learns
//! which one from the engine's own controller file
//! (`<pid>.json` in the directory named by `MEOW_RUNTIME_DIR`).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use starpod_core::domain::RuntimeConfig;

use crate::config::EngineDocument;
use crate::error::{EngineError, EngineResult};
use crate::log::{LogBuffer, LogStreamKind, Redactor};

/// How long the engine may take to exit after a graceful stop request.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// The environment variable that redirects the engine's controller file.
///
/// `meow-common-0.29.0/src/controller_file.rs:24`.
pub const CONTROLLER_DIR_ENV: &str = "MEOW_RUNTIME_DIR";

/// The directory the engine writes its controller file into.
///
/// The engine publishes its controller's address there, in a file named
/// after its own pid. The file is the only reliable way to learn the port
/// the engine bound when the configuration asked for port zero.
pub fn controller_file_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os(CONTROLLER_DIR_ENV)
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
/// `dir` is the directory the child was pointed at with `MEOW_RUNTIME_DIR`.
/// It must be the same one: the file is written there, not in the global
/// default. The file is written by the engine once its controller is bound,
/// and removed when it exits.
pub fn read_controller_address(dir: &Path, pid: u32) -> Option<String> {
    let path = dir.join(format!("{pid}.json"));
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let controller = value.get("controller")?.as_str()?;
    Some(controller.to_string())
}

/// The controller's address: loopback only.
///
/// The controller API can rewrite the engine's configuration and read its
/// traffic. It must never be reachable from the network.
///
/// The port is `0`, so the OS assigns a free one. The app learns the
/// assigned port from the engine's controller file. Binding to an explicit
/// port would race with anything else on the machine that wants it.
pub const CONTROLLER_ADDRESS: &str = "127.0.0.1:0";

/// A running engine.
pub struct EngineProcess {
    child: Child,
    /// The secret the controller expects.
    secret: String,
    /// The engine's process id, for the controller file.
    pid: u32,
    /// The directory the engine was told to publish its controller file in.
    /// `None` means the engine chose its own, which is the global default.
    controller_dir: Option<PathBuf>,
}

impl EngineProcess {
    /// Start the engine with a configuration.
    ///
    /// The configuration is written to `config_path` first and the engine is
    /// started on it. Writing it to a file — rather than passing it inline
    /// — means the engine's own diagnostics name a file the user can look
    /// at, and it means the file's permissions can be restricted.
    ///
    /// The file also carries the `external-controller` address, because the
    /// controller is bound once and never moves; see the module docs.
    pub async fn start(
        binary: &Path,
        config: &RuntimeConfig,
        config_path: &Path,
        controller_dir: Option<&Path>,
        secret: &str,
        logs: &LogBuffer,
        redactor: &Redactor,
    ) -> EngineResult<Self> {
        let yaml = live_document(config)?;
        write_live_config(config_path, yaml.as_bytes()).await?;

        let mut command = Command::new(binary);
        command
            .arg("-f")
            .arg(config_path)
            .arg("--secret")
            .arg(secret)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Point the engine's controller file at the app's own directory, so
        // Starpod's file never collides with another meow's on this machine
        // and never has to be guessed at.
        if let Some(dir) = controller_dir {
            std::fs::create_dir_all(dir).map_err(|e| EngineError::Io(e.to_string()))?;
            command.env(CONTROLLER_DIR_ENV, dir);
        }

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
            controller_dir: controller_dir.map(Path::to_path_buf),
        })
    }

    /// The engine's process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The controller's address, once the engine has published it.
    pub fn controller_address(&self) -> Option<String> {
        let dir = match &self.controller_dir {
            Some(dir) => dir.clone(),
            None => controller_file_dir()?,
        };
        read_controller_address(&dir, self.pid)
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
            // `id()` is the child's pid, which the engine owns. The signal is
            // sent to the process, which is what SIGTERM is for; a pid that
            // has already exited is a no-op. `nix::sys::signal::kill` is a
            // safe wrapper, so no `unsafe` is needed here.
            nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(self.child.id().unwrap_or(0) as i32),
                nix::sys::signal::Signal::SIGTERM,
            )
            .ok();
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

/// The YAML the live configuration file holds.
///
/// It names the controller address, because the controller is bound once at
/// startup and a reload must not drop the address the file was started with.
/// The secret is never written here.
pub(crate) fn live_document(config: &RuntimeConfig) -> EngineResult<String> {
    EngineDocument::build(config)
        .with_controller(CONTROLLER_ADDRESS)
        .to_yaml()
        .map_err(|e| EngineError::Internal(e.to_string()))
}

/// Replace the live configuration file with `bytes`, atomically.
///
/// The bytes go to a sibling temporary file, which is then renamed over the
/// live one. A reader sees either the old file or the new one, never a
/// half-written file, and a failure part-way leaves the old file in place.
pub(crate) async fn write_live_config(path: &Path, bytes: &[u8]) -> EngineResult<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| EngineError::Io(e.to_string()))?;
    }
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);

    if let Err(error) = write_private_file(&temporary, bytes).await {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(EngineError::Io(error.to_string()));
    }
    tokio::fs::rename(&temporary, path).await.map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        EngineError::Io(error.to_string())
    })
}

/// Write a file that only its owner can read.
///
/// The configuration file holds subscription URLs, which carry tokens. It
/// must not be readable by other users on the machine.
async fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        // `mode` is an inherent method on `tokio::fs::OpenOptions` on Unix,
        // so no `OpenOptionsExt` import is needed.
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
    /// Where the generated configuration is written.
    config_path: PathBuf,
    /// The directory the engine publishes its controller file into.
    ///
    /// Set through [`Supervisor::with_controller_dir`]. When it is set, the
    /// engine is pointed at it with `MEOW_RUNTIME_DIR`, so Starpod's
    /// controller file never collides with another meow's on the machine.
    controller_dir: Option<PathBuf>,
}

impl Supervisor {
    /// Build a supervisor with a fresh secret.
    ///
    /// There is deliberately no `Default`: a supervisor's secret is
    /// generated per instance, and a default implementation would make
    /// two supervisors share one by accident.
    ///
    /// The secret does not need to be persisted. The app generates it,
    /// hands it to the engine on the command line, and keeps it for the
    /// engine's lifetime; a restart is a new engine with a new secret. The
    /// engine fixes the secret at startup and a reload never changes it
    /// (`meow-api-0.29.0/src/routes.rs` reads the value the server was
    /// constructed with), so persisting one would buy nothing.
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
            config_path: PathBuf::new(),
            controller_dir: None,
        }
    }

    /// Tell the supervisor where the generated configuration lives.
    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = path.into();
        self
    }

    /// Tell the supervisor where the engine should publish its controller
    /// file.
    ///
    /// The engine reads `MEOW_RUNTIME_DIR` from its environment
    /// (`meow-common-0.29.0/src/controller_file.rs:24`), so this is applied
    /// to the child's environment rather than being a path the app reads
    /// directly.
    #[must_use]
    pub fn with_controller_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.controller_dir = Some(dir.into());
        self
    }

    /// The directory the engine is told to publish its controller file in,
    /// if one was set.
    pub fn controller_dir(&self) -> Option<&Path> {
        self.controller_dir.as_deref()
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
        binary: &Path,
        config: &RuntimeConfig,
    ) -> EngineResult<crate::engine::EngineHandle> {
        if self.config_path.as_os_str().is_empty() {
            return Err(EngineError::Internal(
                "the supervisor has no configuration path".to_string(),
            ));
        }

        let mut guard = self.child.lock().await;
        if guard.is_some() {
            return Err(EngineError::Internal(
                "the engine is already running".to_string(),
            ));
        }

        let process = EngineProcess::start(
            binary,
            config,
            &self.config_path,
            self.controller_dir.as_deref(),
            &self.secret,
            &self.logs,
            &self.redactor,
        )
        .await?;
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
        if let Ok(mut slot) = self.controller.write() {
            slot.clear();
        }
        Ok(())
    }
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
    fn the_controller_address_asks_for_an_ephemeral_port() {
        // Port 0 lets the OS pick a free port. A fixed port would race with
        // whatever else on the machine wants it, and the engine binds the
        // controller exactly once — a bind failure is a hard startup error
        // (`meow-app-0.29.0/src/main.rs:1558-1560`).
        assert!(CONTROLLER_ADDRESS.ends_with(":0"), "{CONTROLLER_ADDRESS}");
    }

    #[tokio::test]
    async fn a_supervisor_has_a_secret_and_no_controller_until_it_starts() {
        let supervisor = Supervisor::new();
        assert!(!supervisor.secret().is_empty());
        assert_eq!(supervisor.controller(), "");
    }

    #[tokio::test]
    async fn a_supervisor_without_a_config_path_refuses_to_start() {
        let supervisor = Supervisor::new();
        let config = starpod_core::domain::RuntimeConfig {
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
            rules: vec!["MATCH,DIRECT".to_string()],
            rule_providers: Vec::new(),
            subscriptions: Vec::new(),
        };
        let error = supervisor
            .start(Path::new("no-such-engine-binary"), &config)
            .await
            .expect_err("the supervisor has nowhere to write the configuration");
        assert!(matches!(error, EngineError::Internal(_)), "{error:?}");
    }
}
