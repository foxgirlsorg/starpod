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

use starpod_app::{AppError, AppService, SharedAppService, ui};
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
    init_logging();
    info!(version = env!("CARGO_PKG_VERSION"), "starpod starting");

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("starpod-runtime")
        .build()?;
    info!("tokio runtime built");
    let service = runtime.block_on(open_service())?;
    let _ = ENGINE.set((runtime.handle().clone(), service.clone()));

    select_renderer();
    let outcome = ui::run(runtime.handle().clone(), service);
    match &outcome {
        Ok(()) => info!("event loop ended normally; shutting down the runtime"),
        Err(error) => error!(%error, "event loop ended with an error; shutting down the runtime"),
    }
    runtime.shutdown_timeout(Duration::from_secs(5));
    info!("starpod exiting");
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

/// Open the app's real dependencies and load the saved state.
async fn open_service() -> Result<SharedAppService, AppError> {
    let mut service = AppService::open_default()?;
    info!("service opened");
    service.load().await?;
    info!("state loaded");
    Ok(service.into_shared())
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
    paths.data_dir().join("logs")
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

/// Log the panic's message, location and a forced backtrace.
fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string());
    let location = info
        .location()
        .map_or_else(|| "unknown location".to_string(), ToString::to_string);
    let backtrace = std::backtrace::Backtrace::force_capture();
    error!(%location, "PANIC: {message}\n{backtrace}");
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
                service.lock().await.disconnect().await
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
