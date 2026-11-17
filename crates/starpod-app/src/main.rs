//! The Starpod desktop app.
//!
//! The tokio runtime runs on its own worker threads and owns the service.
//!
//! On Windows a release build is a GUI-subsystem executable, so no console
//! window opens with the app. The engine's console is created separately and
//! hidden (`CREATE_NO_WINDOW`), so this does not affect how the engine stops.
//! Debug builds keep the console, so `tracing` output stays visible.
//!
//! Every run also writes its log to `<data dir>/logs/starpod.log` (the
//! previous run's log is kept as `starpod.previous.log`), because a GUI build
//! has no console to show a crash on.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! The Slint event loop runs on the main thread (ARCHITECTURE §7). The two
//! meet only through `slint::invoke_from_event_loop` and the callbacks.

use std::error::Error;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use starpod_app::instance::{self, Startup};
use starpod_app::{AppError, AppService, SharedAppService, autostart, deeplink, scheme, ui};
use starpod_store::AppPaths;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

/// How long a panic waits for the engine to stop before the process ends.
const PANIC_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// The current run's log file, shared by every writer.
static LOG_FILE: OnceLock<Arc<Mutex<File>>> = OnceLock::new();

/// The runtime and service a panic must stop the engine for. Set once the
/// service is open; a panic before that has no engine to stop.
static ENGINE: OnceLock<(tokio::runtime::Handle, SharedAppService)> = OnceLock::new();

fn main() -> Result<(), Box<dyn Error>> {
    // The hook goes first so that a panic anywhere in startup is recorded.
    install_panic_hook();

    // One running Starpod at a time. This runs before anything that writes to
    // disk or changes the system: a second launch hands its link (or a request
    // to show the window) to the first and exits. It must not rotate the first
    // run's log file, nor re-register the scheme.
    let link = deeplink::find_in_args(std::env::args().skip(1));
    let mut startup_warnings = Vec::new();
    let (instance_guard, forwards) = match AppPaths::resolve() {
        Ok(paths) => match instance::start(paths.data_dir(), link.as_deref()) {
            Ok(Startup::Forwarded) => return Ok(()),
            Ok(Startup::Primary { forwards, guard }) => (Some(guard), Some(forwards)),
            Err(error) => {
                startup_warnings.push(format!(
                    "could not set up the single-instance channel; starting without it: {error}"
                ));
                (None, None)
            }
        },
        Err(error) => {
            startup_warnings.push(format!(
                "could not resolve the data directory; starting without the single-instance channel: {error}"
            ));
            (None, None)
        }
    };

    init_logging();
    info!(version = env!("CARGO_PKG_VERSION"), "starpod starting");
    // The binary an earlier update replaced is no longer running.
    starpod_app::update::remove_previous_binary();
    for warning in &startup_warnings {
        warn!("{warning}");
    }

    // Registered at every start by the run that owns the app; a failure is
    // only logged.
    scheme::register();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("starpod-runtime")
        .build()?;
    info!("tokio runtime built");
    // Opened outside `block_on`: on Linux the secret store probes the Secret
    // Service through zbus's *blocking* API, which panics with "Cannot start a
    // runtime from within a runtime" when called inside a tokio context.
    let service = AppService::open_default()?;
    info!("service opened");
    let service = runtime.block_on(load_service(service))?;
    // Set before auto-connect, so a panic while connecting still stops the
    // engine and restores the system proxy.
    let _ = ENGINE.set((runtime.handle().clone(), service.clone()));
    runtime.block_on(connect_at_start(&service));
    let start_hidden = autostart::launched_by_autostart()
        && runtime.block_on(async { service.lock().await.user_state().preferences.silent_start });

    select_renderer();
    let outcome = ui::run(
        runtime.handle().clone(),
        service,
        ui::Launch {
            link,
            forwards,
            start_hidden,
        },
    );
    match &outcome {
        Ok(()) => info!("event loop ended normally; shutting down the runtime"),
        Err(error) => error!(%error, "event loop ended with an error; shutting down the runtime"),
    }
    runtime.shutdown_timeout(Duration::from_secs(5));
    info!("starpod exiting");
    // Closed before an update's relaunch, so the new process becomes the
    // running instance.
    drop(instance_guard);
    starpod_app::update::relaunch_if_scheduled();
    outcome
}

/// Pick the software renderer unless `SLINT_BACKEND` says otherwise.
///
/// The OpenGL (femtovg) renderer was seen to exit the process silently right
/// after the window was shown, with no panic and no crash record, on a host
/// whose GPU driver logs watchdog resets. Slint only falls back to software
/// when the GL renderer cannot be *constructed*, not when the context fails
/// later, so the fallback has to be chosen up front. The UI is flat 2D and the
/// software renderer is more than fast enough for it.
fn select_renderer() {
    if std::env::var_os("SLINT_BACKEND").is_some() {
        info!("renderer chosen by SLINT_BACKEND");
        return;
    }
    match slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()
    {
        Ok(()) => info!("software renderer selected"),
        Err(error) => {
            warn!(%error, "could not select the software renderer; using Slint's default")
        }
    }
}

/// Load the saved state into the opened service.
///
/// The login entry is re-written here, since the executable may have moved.
async fn load_service(mut service: AppService) -> Result<SharedAppService, AppError> {
    service.load().await?;
    info!("state loaded");
    service.sync_autostart();
    Ok(service.into_shared())
}

/// When the user chose it and a subscription supplies servers, connect now.
async fn connect_at_start(service: &SharedAppService) {
    let mut svc = service.lock().await;
    if !svc.user_state().preferences.auto_connect_on_start {
        return;
    }
    match svc.connect().await {
        Ok(()) => info!("connected at start, as the settings ask"),
        Err(error) => warn!(%error, "did not connect at start"),
    }
}

/// Send tracing to stderr and to the run's log file.
///
/// The file is unbuffered so every line is on disk even when the process
/// aborts (`panic = "abort"`) a moment later.
fn init_logging() {
    match open_log_file() {
        Some(file) => {
            let _ = LOG_FILE.set(Arc::new(Mutex::new(file)));
        }
        None => eprintln!("starpod: could not open the log file; logging to stderr only"),
    }
    let sink = LogSink {
        file: LOG_FILE.get().cloned(),
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(sink)
        .init();
}

/// Create `<data dir>/logs/starpod.log` for this run, after moving the
/// previous run's log to `starpod.previous.log`.
fn open_log_file() -> Option<File> {
    let paths = AppPaths::resolve().ok()?;
    let dir = log_dir(&paths);
    std::fs::create_dir_all(&dir).ok()?;
    let current = dir.join("starpod.log");
    let previous = dir.join("starpod.previous.log");
    if current.exists() {
        let _ = std::fs::rename(&current, &previous);
    }
    File::create(current).ok()
}

/// The directory the log file lives in.
fn log_dir(paths: &AppPaths) -> PathBuf {
    paths.log_dir()
}

/// Writes every log line to stderr and to the log file.
struct LogSink {
    file: Option<Arc<Mutex<File>>>,
}

/// One write call's destination: both sinks. Errors from either are ignored,
/// because a GUI build has no stderr and a log must never stop the app.
struct Tee {
    file: Option<Arc<Mutex<File>>>,
}

impl<'a> MakeWriter<'a> for LogSink {
    type Writer = Tee;

    fn make_writer(&'a self) -> Self::Writer {
        Tee {
            file: self.file.clone(),
        }
    }
}

impl Write for Tee {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(Ok(mut file)) = self.file.as_ref().map(|file| file.lock()) {
            let _ = file.write_all(buf);
        }
        let _ = io::stderr().write_all(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(Ok(mut file)) = self.file.as_ref().map(|file| file.lock()) {
            let _ = file.flush();
        }
        let _ = io::stderr().flush();
        Ok(())
    }
}

/// Record a panic in the log, then stop the engine so a crash does not leave
/// the proxy orphaned. The previous hook still runs afterwards.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log_panic(info);
        if let Some((handle, service)) = ENGINE.get() {
            stop_engine_after_panic(handle, service);
        }
        previous(info);
    }));
}

/// Log the panic's message, location and a forced backtrace, and write the
/// same details to a crash report of their own.
fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    let message = starpod_app::crash::panic_message(info);
    let location = starpod_app::crash::panic_location(info);
    let backtrace = std::backtrace::Backtrace::force_capture();
    error!(%location, "PANIC: {message}\n{backtrace}");
    // The report is a file of its own, so it survives the log's rotation.
    if let Ok(paths) = AppPaths::resolve() {
        let _ = starpod_app::crash::write_panic_report(info, &paths.crash_dir());
    }
}

/// Run the graceful stop on a helper thread. The panicking thread may be the
/// runtime's own, where blocking is not allowed, and it may hold the service
/// lock, so both the lock and the stop are bounded by a timeout.
fn stop_engine_after_panic(handle: &tokio::runtime::Handle, service: &SharedAppService) {
    let (finished, done) = std::sync::mpsc::channel();
    let handle = handle.clone();
    let service = service.clone();
    std::thread::spawn(move || {
        let stop = handle.block_on(async {
            tokio::time::timeout(PANIC_STOP_TIMEOUT, async {
                let mut svc = service.lock().await;
                let result = svc.disconnect().await;
                // Restored here too: if the engine did not stop, the next start
                // still finds the record and restores the system proxy.
                svc.restore_system_proxy().await;
                result
            })
            .await
        });
        match stop {
            Ok(Ok(())) => info!("the engine stopped after a panic"),
            Ok(Err(error)) => warn!(%error, "the engine did not stop after a panic"),
            Err(_) => warn!("timed out stopping the engine after a panic"),
        }
        let _ = finished.send(());
    });
    if done
        .recv_timeout(PANIC_STOP_TIMEOUT + Duration::from_secs(1))
        .is_err()
    {
        warn!("the engine stop after a panic did not finish in time");
    }
}
