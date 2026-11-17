//! Running the proxy engine as a supervised child process.
//!
//! The supervisor owns the engine's lifetime. It starts the child, drains
//! its output, notices when it dies, and stops it gracefully.
//!
//! # One task owns the child
//!
//! Each start spawns a waiter task that owns the child (or the elevated
//! wrapper) and is the only thing that waits on it. Stopping sends a message
//! to that task, which asks the engine to exit, waits out a grace period and
//! then forces the stop. Nothing else touches the child, so a stop can never
//! deadlock against a wait. (An earlier design held the child's lock across
//! `wait()`, which made every stop block forever.)
//!
//! # Graceful shutdown
//!
//! The engine shuts down on `SIGTERM`/`SIGINT` (unix) and on `CTRL_C_EVENT`
//! (Windows). It closes its listeners and connections on the way out, so a
//! graceful stop does not drop the user's traffic on the floor. An abrupt
//! kill does, and with TUN on it can leave routes behind.
//!
//! On Windows the engine is **not** created with `CREATE_NEW_PROCESS_GROUP`.
//! That flag disables `CTRL_C` for the new group, and the engine listens only
//! to `CTRL_C` (`tokio/src/signal/windows/sys.rs`, see `signal.rs`). The
//! engine is created with `CREATE_NO_WINDOW` instead: it gets a console of
//! its own, hidden, so `signal::request_graceful` can deliver `CTRL_C` to
//! that console and nothing else.
//!
//! If the event is not delivered, or the engine ignores it, the child is
//! killed after the grace period. A stuck engine must not keep the app from
//! quitting.
//!
//! # The command line
//!
//! The engine is started as
//!
//! ```text
//! meow -f <config-path> --secret <secret>
//! ```
//!
//! (plus `--ext-ctl <address>` for an elevated engine, see below). The
//! **secret** is on the command line and nowhere else: it is not in the
//! config file, because the engine accepts a top-level `secret:` key and the
//! app's writer refuses to produce one (`starpod_store::runtime_config`).
//!
//! # The controller address
//!
//! An unelevated engine's controller address is in the config file
//! (`127.0.0.1:0`), and the engine publishes the bound address in its
//! controller file (`<pid>.json` under `MEOW_RUNTIME_DIR`). The app reads
//! that file.
//!
//! An elevated engine cannot inherit `MEOW_RUNTIME_DIR` through
//! `ShellExecuteExW`, `pkexec` or `osascript`. Instead the app picks a free
//! loopback port itself and passes it as `--ext-ctl`, which overrides the
//! config file. The address is then known without the controller file. See
//! `elevate.rs`.
//!
//! The engine binds its controller once, at startup, and a reload never moves
//! it, so the address stays the same for the engine's whole life.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWriteExt as _, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, watch};

use starpod_core::domain::RuntimeConfig;

use crate::config::EngineDocument;
#[cfg(unix)]
use crate::elevate::StopSignal;
use crate::elevate::{self, Elevated};
use crate::engine::{EngineHandle, ExitInfo, StopRequest};
use crate::error::{EngineError, EngineResult};
use crate::log::{LogBuffer, LogStreamKind, Redactor};
use crate::rest::ControllerClient;
use crate::signal;

/// How long the engine may take to exit after a graceful stop request.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// How long a caller of [`Supervisor::stop`] waits in total. A forced stop
/// after the grace period may need a second elevation prompt, so this is
/// longer than one grace period.
const STOP_WAIT: Duration = Duration::from_secs(150);

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

/// The controller's address for an unelevated engine: loopback, port zero.
///
/// The controller API can rewrite the engine's configuration and read its
/// traffic. It must never be reachable from the network.
///
/// The port is `0`, so the OS assigns a free one. The app learns the
/// assigned port from the engine's controller file. Binding to an explicit
/// port would race with anything else on the machine that wants it.
pub const CONTROLLER_ADDRESS: &str = "127.0.0.1:0";

/// The `CREATE_NO_WINDOW` process-creation flag (`0x0800_0000`).
///
/// The engine gets a console, hidden, instead of a window. See the module
/// docs for why that is preferred to `CREATE_NEW_PROCESS_GROUP`.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// What the app knows about the engine it is running, for the snapshot and
/// for the ownership marker.
#[derive(Clone, PartialEq, Eq)]
pub struct RunInfo {
    /// The engine's pid, when known.
    pub pid: Option<u32>,
    /// Whether the engine was started elevated.
    pub elevated: bool,
    /// The controller's address, when known.
    pub controller: Option<String>,
    /// The controller secret. Never shown or logged.
    pub secret: String,
}

impl std::fmt::Debug for RunInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunInfo")
            .field("pid", &self.pid)
            .field("elevated", &self.elevated)
            .field("controller", &self.controller)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// A request to the waiter task that owns the engine.
#[derive(Debug)]
enum Control {
    /// Stop the engine gracefully, then forcibly if it does not exit.
    Stop,
}

/// What the app holds about the engine while it runs.
struct Running {
    /// Distinguishes this run from a later one, so a stale waiter never
    /// clears a newer run's record.
    run: u64,
    /// The engine's pid, once known.
    pid: Option<u32>,
    /// Whether the engine runs elevated.
    elevated: bool,
    /// The controller's address, for an elevated or adopted engine.
    controller: Option<String>,
    /// The secret this run's controller expects.
    secret: String,
    /// Where an elevated engine's wrapper records its pid.
    pidfile: Option<PathBuf>,
    /// The waiter's inbox. `None` for an engine adopted from an earlier run,
    /// which has no waiter and is stopped by pid.
    control: Option<mpsc::UnboundedSender<Control>>,
}

/// State shared between the supervisor and its waiter tasks.
struct Inner {
    /// The current run, if any. Held only briefly and never across an await.
    running: Mutex<Option<Running>>,
    /// Fires when the engine exits. Reset to `None` at each start.
    exited: watch::Sender<Option<ExitInfo>>,
    /// The last run number handed out.
    runs: AtomicU64,
}

/// The running child, in the form the waiter needs.
enum Launch {
    /// An unelevated engine the app spawned. Boxed so the enum stays as small
    /// as its elevated variant (clippy `large_enum_variant` on Windows).
    Direct { child: Box<Child>, pid: u32 },
    /// An elevated engine, behind its platform wrapper or handle. Boxed for
    /// the same reason as `Direct`.
    Elevated {
        process: Box<Elevated>,
        pidfile: PathBuf,
    },
}

impl Launch {
    /// Wait for the engine to exit.
    async fn wait(&mut self) -> ExitInfo {
        match self {
            Self::Direct { child, .. } => match child.wait().await {
                Ok(status) => ExitInfo::from_status(status),
                Err(_) => ExitInfo::unknown(),
            },
            Self::Elevated { process, .. } => process.wait().await,
        }
    }

    /// Ask the engine to shut down gracefully.
    async fn request_stop(&mut self) {
        match self {
            Self::Direct { pid, .. } => {
                signal::request_graceful(*pid);
            }
            Self::Elevated { process, pidfile } => process.request_stop(pidfile.as_path()).await,
        }
    }

    /// End the engine at once.
    async fn force_stop(&mut self) {
        match self {
            Self::Direct { child, .. } => {
                let _ = child.start_kill();
            }
            Self::Elevated { process, pidfile } => process.force_stop(pidfile.as_path()).await,
        }
    }
}

/// The waiter's reason for waking.
enum Event {
    /// The engine exited on its own.
    Exited(ExitInfo),
    /// A control message arrived, or the inbox closed (`None`).
    Control(Option<Control>),
}

/// Take the run record, tolerating a poisoned lock: the record is plain data
/// and is still valid after a panic elsewhere.
fn lock_running(inner: &Inner) -> MutexGuard<'_, Option<Running>> {
    inner.running.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The supervisor's public face: start, watch, stop.
pub struct Supervisor {
    /// The engine's logs.
    pub logs: LogBuffer,
    /// The secret this supervisor gives a new engine.
    secret: String,
    /// The redactor for the logs.
    redactor: Redactor,
    /// Where the generated configuration is written.
    config_path: PathBuf,
    /// The directory the engine publishes its controller file into.
    ///
    /// When it is set, the engine is pointed at it with `MEOW_RUNTIME_DIR`,
    /// so Starpod's controller file never collides with another meow's on
    /// the machine.
    controller_dir: Option<PathBuf>,
    inner: Arc<Inner>,
}

impl Supervisor {
    /// Build a supervisor with a fresh secret.
    ///
    /// There is deliberately no `Default`: a supervisor's secret is generated
    /// per instance, and a default implementation would make two supervisors
    /// share one by accident.
    ///
    /// The secret does not need to be persisted to start the engine. It is
    /// kept in the ownership marker (`adopt.rs`) so a restarted app can
    /// reattach to a live engine.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let secret = crate::secret::random_secret();
        let (exited, _) = watch::channel(None);
        Self {
            logs: LogBuffer::new(2000),
            secret: secret.clone(),
            redactor: Redactor::new([secret]),
            config_path: PathBuf::new(),
            controller_dir: None,
            inner: Arc::new(Inner {
                running: Mutex::new(None),
                exited,
                runs: AtomicU64::new(0),
            }),
        }
    }

    /// Tell the supervisor where the generated configuration lives.
    #[must_use]
    pub fn with_config_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = path.into();
        self
    }

    /// Tell the supervisor where the engine should publish its controller
    /// file. Applied to an unelevated engine's environment as
    /// `MEOW_RUNTIME_DIR`; an elevated engine cannot receive it (see the
    /// module docs).
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

    /// The secret a new engine is given.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// Whether an engine is running under this supervisor, or was adopted.
    pub fn is_running(&self) -> bool {
        lock_running(&self.inner).is_some()
    }

    /// Whether the engine's state is known to have been reached: the elevated
    /// wrapper wrote its pid file, or the elevation was granted on Windows.
    /// Used to tell "the prompt was refused" from "the engine crashed".
    pub fn elevation_completed(&self) -> bool {
        if cfg!(windows) {
            return true;
        }
        self.pidfile_path().exists()
    }

    /// How the last engine exited, if it has.
    pub fn last_exit(&self) -> Option<ExitInfo> {
        self.inner.exited.borrow().clone()
    }

    /// A watch receiver for the exit signal.
    pub fn exited(&self) -> watch::Receiver<Option<ExitInfo>> {
        self.inner.exited.subscribe()
    }

    /// The controller's address, if the engine is running and its address is
    /// known.
    ///
    /// For an unelevated engine this reads the engine's controller file, so
    /// it is `None` until the engine has bound its controller. Callers
    /// poll it while the engine starts.
    pub fn controller_address(&self) -> Option<String> {
        let (controller, pid) = {
            let slot = lock_running(&self.inner);
            let run = slot.as_ref()?;
            (run.controller.clone(), run.pid)
        };
        if let Some(controller) = controller {
            return Some(controller);
        }
        let pid = match pid {
            Some(pid) => pid,
            None => self.engine_pid()?,
        };
        let dir = self.controller_dir.clone().or_else(controller_file_dir)?;
        read_controller_address(&dir, pid)
    }

    /// The controller's address as a string, empty when unknown.
    pub fn controller(&self) -> String {
        self.controller_address().unwrap_or_default()
    }

    /// The engine's pid, if it is running and the pid is known.
    ///
    /// An elevated engine's pid is written by its wrapper to a pid file;
    /// it is read here and remembered.
    pub fn engine_pid(&self) -> Option<u32> {
        let (known, pidfile) = {
            let slot = lock_running(&self.inner);
            let run = slot.as_ref()?;
            (run.pid, run.pidfile.clone())
        };
        if known.is_some() {
            return known;
        }
        let pid = elevate::read_pidfile(&pidfile?)?;
        if let Some(run) = lock_running(&self.inner).as_mut() {
            run.pid = Some(pid);
        }
        Some(pid)
    }

    /// What the app knows about the running engine.
    pub fn run_info(&self) -> Option<RunInfo> {
        let (elevated, secret) = {
            let slot = lock_running(&self.inner);
            let run = slot.as_ref()?;
            (run.elevated, run.secret.clone())
        };
        Some(RunInfo {
            pid: self.engine_pid(),
            elevated,
            controller: self.controller_address(),
            secret,
        })
    }

    /// Start the engine, unelevated or elevated.
    ///
    /// The configuration is written to the config path first, and the engine
    /// is started on that file. An elevated start passes the controller
    /// address on the command line.
    pub async fn start(
        &self,
        binary: &Path,
        config: &RuntimeConfig,
        elevated: bool,
    ) -> EngineResult<EngineHandle> {
        if self.config_path.as_os_str().is_empty() {
            return Err(EngineError::Internal(
                "the supervisor has no configuration path".to_string(),
            ));
        }
        if self.is_running() {
            return Err(EngineError::Internal(
                "the engine is already running".to_string(),
            ));
        }

        let yaml = live_document(config)?;
        write_live_config(&self.config_path, yaml.as_bytes()).await?;

        let run = self.inner.runs.fetch_add(1, Ordering::Relaxed) + 1;
        let (launch, pid, controller, pidfile) = if elevated {
            let controller = format!("127.0.0.1:{}", pick_free_port()?);
            let pidfile = self.pidfile_path();
            if let Some(dir) = pidfile.parent() {
                std::fs::create_dir_all(dir).map_err(|error| EngineError::Io(error.to_string()))?;
            }
            let _ = std::fs::remove_file(&pidfile);
            let args =
                elevate::engine_arguments(&self.config_path, &self.secret, Some(&controller));
            let mut process = elevate::spawn_elevated(binary, &args, &pidfile)?;
            if let Some(stdout) = process.take_stdout() {
                spawn_drain(
                    stdout,
                    LogStreamKind::Stdout,
                    self.logs.clone(),
                    self.redactor.clone(),
                );
            }
            if let Some(stderr) = process.take_stderr() {
                spawn_drain(
                    stderr,
                    LogStreamKind::Stderr,
                    self.logs.clone(),
                    self.redactor.clone(),
                );
            }
            let pid = process.pid();
            (
                Launch::Elevated {
                    process: Box::new(process),
                    pidfile: pidfile.clone(),
                },
                pid,
                Some(controller),
                Some(pidfile),
            )
        } else {
            let (child, pid) = self.spawn_direct(binary)?;
            (
                Launch::Direct {
                    child: Box::new(child),
                    pid,
                },
                Some(pid),
                None,
                None,
            )
        };

        let (control_tx, control_rx) = mpsc::unbounded_channel();
        {
            let mut slot = lock_running(&self.inner);
            *slot = Some(Running {
                run,
                pid,
                elevated,
                controller,
                secret: self.secret.clone(),
                pidfile,
                control: Some(control_tx.clone()),
            });
        }
        // A new run has not exited. Without this, a restart would report the
        // previous run's exit as its own.
        self.inner.exited.send_replace(None);

        tokio::spawn(supervise(run, launch, control_rx, Arc::clone(&self.inner)));

        Ok(EngineHandle {
            exited: self.inner.exited.subscribe(),
            stop: StopRequest::new(move || {
                let _ = control_tx.send(Control::Stop);
            }),
        })
    }

    /// Spawn an unelevated engine and start draining its output.
    fn spawn_direct(&self, binary: &Path) -> EngineResult<(Child, u32)> {
        let mut command = Command::new(binary);
        // The home directory is the configuration's directory, as in
        // `elevate::engine_arguments`.
        let home = self.config_path.parent().unwrap_or_else(|| Path::new("."));
        command
            .arg("-d")
            .arg(home)
            .arg("-f")
            .arg(&self.config_path)
            .arg("--secret")
            .arg(&self.secret)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        // Point the engine's controller file at the app's own directory, so
        // Starpod's file never collides with another meow's on this machine
        // and never has to be guessed at.
        if let Some(dir) = &self.controller_dir {
            std::fs::create_dir_all(dir).map_err(|error| EngineError::Io(error.to_string()))?;
            command.env(CONTROLLER_DIR_ENV, dir);
        }

        #[cfg(windows)]
        {
            // A hidden console of its own; see `CREATE_NO_WINDOW`.
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command
            .spawn()
            .map_err(|error| EngineError::Spawn(error.to_string()))?;
        let pid = child.id().ok_or_else(|| {
            EngineError::Spawn("the engine exited before it had a pid".to_string())
        })?;

        // Drain the child's output. This runs for the child's whole lifetime
        // and must never be skipped: a pipe that fills up blocks the engine.
        if let Some(stdout) = child.stdout.take() {
            spawn_drain(
                stdout,
                LogStreamKind::Stdout,
                self.logs.clone(),
                self.redactor.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_drain(
                stderr,
                LogStreamKind::Stderr,
                self.logs.clone(),
                self.redactor.clone(),
            );
        }
        Ok((child, pid))
    }

    /// The pid file an elevated wrapper writes, in the controller directory
    /// (or beside the config when there is none).
    fn pidfile_path(&self) -> PathBuf {
        let dir = self
            .controller_dir
            .clone()
            .or_else(|| self.config_path.parent().map(Path::to_path_buf))
            .unwrap_or_else(|| PathBuf::from("."));
        elevate::pidfile_in(&dir)
    }

    /// Record an engine that an earlier run left running, so this supervisor
    /// controls it. Fails if an engine is already controlled.
    pub fn adopt(
        &self,
        pid: u32,
        controller: String,
        secret: String,
        elevated: bool,
    ) -> EngineResult<()> {
        let mut slot = lock_running(&self.inner);
        if slot.is_some() {
            return Err(EngineError::Internal(
                "an engine is already controlled".to_string(),
            ));
        }
        let run = self.inner.runs.fetch_add(1, Ordering::Relaxed) + 1;
        *slot = Some(Running {
            run,
            pid: Some(pid),
            elevated,
            controller: Some(controller),
            secret,
            pidfile: None,
            control: None,
        });
        drop(slot);
        self.inner.exited.send_replace(None);
        Ok(())
    }

    /// Stop the engine.
    ///
    /// Returns once the engine has exited. Does nothing when no engine is
    /// running.
    pub async fn stop(&self) -> EngineResult<()> {
        enum Plan {
            Nothing,
            Owned(mpsc::UnboundedSender<Control>),
            Adopted {
                pid: Option<u32>,
                elevated: bool,
                controller: Option<String>,
                secret: String,
            },
        }

        let plan = {
            let slot = lock_running(&self.inner);
            match slot.as_ref() {
                None => Plan::Nothing,
                Some(run) => match &run.control {
                    Some(control) => Plan::Owned(control.clone()),
                    None => Plan::Adopted {
                        pid: run.pid,
                        elevated: run.elevated,
                        controller: run.controller.clone(),
                        secret: run.secret.clone(),
                    },
                },
            }
        };

        match plan {
            Plan::Nothing => Ok(()),
            Plan::Owned(control) => {
                let _ = control.send(Control::Stop);
                let mut exited = self.inner.exited.subscribe();
                match tokio::time::timeout(STOP_WAIT, exited.wait_for(Option::is_some)).await {
                    Ok(Ok(_)) => Ok(()),
                    _ => Err(EngineError::Internal(
                        "the engine did not report that it exited".to_string(),
                    )),
                }
            }
            Plan::Adopted {
                pid,
                elevated,
                controller,
                secret,
            } => {
                stop_adopted(pid, elevated, controller.as_deref(), &secret).await?;
                *lock_running(&self.inner) = None;
                let mut info = ExitInfo::unknown();
                info.requested = true;
                self.inner.exited.send_replace(Some(info));
                Ok(())
            }
        }
    }
}

/// The waiter task for one run. It is the only thing that waits on `launch`.
async fn supervise(
    run: u64,
    mut launch: Launch,
    mut control: mpsc::UnboundedReceiver<Control>,
    inner: Arc<Inner>,
) {
    let event = tokio::select! {
        info = launch.wait() => Event::Exited(info),
        message = control.recv() => Event::Control(message),
    };
    let info = match event {
        Event::Exited(info) => info,
        Event::Control(None) => launch.wait().await,
        Event::Control(Some(_)) => stop_launch(&mut launch).await,
    };
    {
        let mut slot = lock_running(&inner);
        if slot.as_ref().is_some_and(|running| running.run == run) {
            *slot = None;
        }
    }
    inner.exited.send_replace(Some(info));
}

/// Ask a running engine to stop, then force it if it does not exit in time.
async fn stop_launch(launch: &mut Launch) -> ExitInfo {
    launch.request_stop().await;
    let mut info = match tokio::time::timeout(STOP_GRACE, launch.wait()).await {
        Ok(info) => info,
        Err(_) => {
            launch.force_stop().await;
            match tokio::time::timeout(STOP_GRACE, launch.wait()).await {
                Ok(info) => info,
                Err(_) => ExitInfo::unknown(),
            }
        }
    };
    info.requested = true;
    info
}

/// Stop an engine this app did not start in this run.
///
/// The pid is all the app has. An unelevated engine is signaled directly.
/// An elevated one needs a second elevation on unix. On Windows the app has
/// no handle to it and cannot open one with `PROCESS_TERMINATE` from an
/// unelevated process, so it cannot be stopped from here.
async fn stop_adopted(
    pid: Option<u32>,
    elevated: bool,
    controller: Option<&str>,
    secret: &str,
) -> EngineResult<()> {
    let Some(pid) = pid else {
        return Err(EngineError::Internal(
            "the adopted engine's pid is unknown, so it cannot be stopped".to_string(),
        ));
    };
    let client =
        controller.map(|address| ControllerClient::new(format!("http://{address}"), secret));

    if elevated {
        return stop_adopted_elevated(pid, client.as_ref()).await;
    }

    signal::request_graceful(pid);
    if wait_until_silent(client.as_ref()).await {
        return Ok(());
    }
    if signal::force_kill(pid) {
        Ok(())
    } else {
        Err(EngineError::Internal(
            "could not stop the engine process".to_string(),
        ))
    }
}

/// The unix half of [`stop_adopted`] for an elevated engine.
#[cfg(unix)]
async fn stop_adopted_elevated(pid: u32, client: Option<&ControllerClient>) -> EngineResult<()> {
    elevate::signal_elevated(pid, StopSignal::Term).await?;
    if wait_until_silent(client).await {
        return Ok(());
    }
    elevate::signal_elevated(pid, StopSignal::Kill).await
}

/// The Windows half of [`stop_adopted`] for an elevated engine.
#[cfg(windows)]
async fn stop_adopted_elevated(_pid: u32, _client: Option<&ControllerClient>) -> EngineResult<()> {
    Err(EngineError::Internal(
        "the engine was started with administrator permission by an earlier run, and \
         stopping it needs Starpod to be run as administrator. Its TUN mode has been turned off"
            .to_string(),
    ))
}

/// Poll the controller until it stops answering, for up to the grace period.
/// Returns `false` when there is no controller to ask, or it still answers.
async fn wait_until_silent(client: Option<&ControllerClient>) -> bool {
    let Some(client) = client else {
        return false;
    };
    let deadline = tokio::time::Instant::now() + STOP_GRACE;
    while tokio::time::Instant::now() < deadline {
        if !client.is_ready().await {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

/// Pick a free loopback port. The port is released before the engine binds
/// it, so another process could take it in between; the engine then fails to
/// start and the caller falls back.
fn pick_free_port() -> EngineResult<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| EngineError::Io(error.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|error| EngineError::Io(error.to_string()))?
        .port();
    drop(listener);
    Ok(port)
}

/// Drain a child's output into the log buffer.
///
/// The task runs until the pipe closes, which is when the child exits.
fn spawn_drain<R>(reader: R, kind: LogStreamKind, logs: LogBuffer, redactor: Redactor)
where
    R: AsyncRead + Unpin + Send + 'static,
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
        drop(file);
        starpod_store::permissions::restrict_to_current_user(path)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> RuntimeConfig {
        RuntimeConfig {
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
            engine: Default::default(),
        }
    }

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

    #[test]
    fn a_picked_port_is_a_real_loopback_port() {
        let port = pick_free_port().unwrap();
        assert!(port > 0);
    }

    #[tokio::test]
    async fn a_supervisor_has_a_secret_and_no_controller_until_it_starts() {
        let supervisor = Supervisor::new();
        assert!(!supervisor.secret().is_empty());
        assert_eq!(supervisor.controller(), "");
        assert!(!supervisor.is_running());
    }

    #[tokio::test]
    async fn a_supervisor_without_a_config_path_refuses_to_start() {
        let supervisor = Supervisor::new();
        let error = supervisor
            .start(Path::new("no-such-engine-binary"), &config(), false)
            .await
            .expect_err("the supervisor has nowhere to write the configuration");
        assert!(matches!(error, EngineError::Internal(_)), "{error:?}");
    }

    #[tokio::test]
    async fn stopping_a_supervisor_that_runs_nothing_is_a_no_op() {
        let supervisor = Supervisor::new();
        supervisor.stop().await.expect("nothing to stop");
        assert!(supervisor.last_exit().is_none());
    }

    /// A stand-in engine: a shell script that runs `body`.
    #[cfg(unix)]
    fn fake_engine(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("fake-meow");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    fn supervisor_in(dir: &Path) -> Supervisor {
        Supervisor::new().with_config_path(dir.join("runtime-config.yaml"))
    }

    #[cfg(unix)]
    async fn wait_for_exit(handle: &EngineHandle) -> ExitInfo {
        let mut exited = handle.exited.clone();
        let value = exited
            .wait_for(Option::is_some)
            .await
            .expect("the supervisor keeps its sender alive");
        value.clone().expect("an exit was reported")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_stop_request_ends_the_engine_and_is_reported_as_requested() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = fake_engine(dir.path(), "exec sleep 30");
        let supervisor = supervisor_in(dir.path());
        let handle = supervisor.start(&binary, &config(), false).await.unwrap();
        assert!(supervisor.is_running());

        supervisor.stop().await.expect("the engine stops");

        let info = supervisor.last_exit().expect("the exit is recorded");
        assert!(info.requested, "{info:?}");
        assert_eq!(info.signal, Some(15), "SIGTERM is the graceful stop");
        assert!(!supervisor.is_running());
        drop(handle);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_engine_that_exits_on_its_own_is_reported_as_not_requested() {
        let dir = tempfile::TempDir::new().unwrap();
        let binary = fake_engine(dir.path(), "exit 3");
        let supervisor = supervisor_in(dir.path());
        let handle = supervisor.start(&binary, &config(), false).await.unwrap();

        let info = wait_for_exit(&handle).await;

        assert_eq!(info.code, Some(3));
        assert!(!info.requested, "{info:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_restart_does_not_report_the_previous_run_as_its_exit() {
        // Regression: the exit watch was never reset, so a second start saw
        // the first run's exit as its own.
        let dir = tempfile::TempDir::new().unwrap();
        let binary = fake_engine(dir.path(), "exec sleep 30");
        let supervisor = supervisor_in(dir.path());
        supervisor.start(&binary, &config(), false).await.unwrap();
        supervisor.stop().await.unwrap();
        assert!(supervisor.last_exit().is_some());

        let _handle = supervisor.start(&binary, &config(), false).await.unwrap();

        assert!(supervisor.last_exit().is_none());
        assert!(supervisor.is_running());
        supervisor.stop().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_engine_that_ignores_the_graceful_stop_is_killed_after_the_grace_period() {
        let dir = tempfile::TempDir::new().unwrap();
        // The script creates `ready` only after it has ignored TERM, so the
        // stop cannot race ahead of the trap and kill the shell by default.
        let ready = dir.path().join("ready");
        let body = format!(
            "trap '' TERM\ntouch '{}'\nwhile :; do sleep 1; done",
            ready.display()
        );
        let binary = fake_engine(dir.path(), &body);
        let supervisor = supervisor_in(dir.path());
        supervisor.start(&binary, &config(), false).await.unwrap();
        for _ in 0..200 {
            if ready.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(ready.exists(), "the fake engine never installed its trap");

        let started = std::time::Instant::now();
        supervisor.stop().await.expect("the forced stop completes");
        assert!(
            started.elapsed() >= STOP_GRACE,
            "the engine was forced before its grace period ended"
        );

        let info = supervisor.last_exit().expect("the exit is recorded");
        assert!(info.requested);
        assert_eq!(
            info.signal,
            Some(9),
            "SIGKILL after the grace period: {info:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_adopted_engine_is_stopped_by_its_pid() {
        let dir = tempfile::TempDir::new().unwrap();
        // A process this supervisor did not start, standing in for an engine
        // left running by an earlier app. With no controller to observe, the
        // stop signals it and does not wait for it to answer.
        let mut orphan = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let supervisor = supervisor_in(dir.path());
        supervisor
            .adopt(orphan.id(), String::new(), "s".to_string(), false)
            .unwrap();
        assert!(supervisor.is_running());
        assert_eq!(supervisor.engine_pid(), Some(orphan.id()));

        supervisor.stop().await.expect("the adopted engine stops");

        assert!(!supervisor.is_running());
        // Reap it so the test does not leave a zombie.
        let status = orphan.wait().unwrap();
        assert!(!status.success());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn adopting_twice_is_refused() {
        let dir = tempfile::TempDir::new().unwrap();
        let supervisor = supervisor_in(dir.path());
        supervisor
            .adopt(1234, "127.0.0.1:1".to_string(), "s".to_string(), false)
            .unwrap();
        assert!(
            supervisor
                .adopt(5678, "127.0.0.1:2".to_string(), "t".to_string(), false)
                .is_err()
        );
    }

    #[test]
    fn run_info_redacts_the_secret_in_its_debug_form() {
        let info = RunInfo {
            pid: Some(1),
            elevated: false,
            controller: None,
            secret: "hunter2".to_string(),
        };
        assert!(!format!("{info:?}").contains("hunter2"));
    }
}
