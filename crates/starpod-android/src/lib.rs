//! Starpod on Android.
//!
//! The APK (android/) is a NativeActivity whose library is this crate. The
//! activity calls [`android_main`], which does what the desktop `main` does:
//! open the service, load the saved state, and run the same Slint window.
//!
//! What differs from the desktop:
//!
//! - There is no `HOME` for the app's process, so it is set to the app's
//!   private files directory first. `dirs` then puts the data, config and
//!   cache directories under it, and nothing else in the workspace needs to
//!   know about Android.
//! - Logs go to logcat (tag `starpod`) as well as to the log file.
//! - There is no tray, no global shortcuts, no self-update and no
//!   single-instance channel: Android starts one activity and updates come
//!   as a new APK.
//! - The engine is the static musl Linux build of meow, installed from the
//!   app like on the desktop. The app targets API 28 so it may run a binary
//!   from its own data directory.
//! - Traffic reaches the engine through a VPN ([`vpn`]), never a system
//!   proxy: connecting turns the VPN on, disconnecting turns it off.

#![cfg(target_os = "android")]

mod vpn;

use std::ffi::{CString, c_char, c_int};
use std::io::{self, Write};
use std::time::Duration;

use slint::android::AndroidApp;
use starpod_app::{AppService, ui};
use tracing::{error, info};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

#[link(name = "log")]
unsafe extern "C" {
    fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
}

/// `ANDROID_LOG_INFO`.
const LOG_INFO: c_int = 4;

/// The entry point the NativeActivity glue calls on its own thread.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    // HOME first: everything below resolves its directories from it.
    if let Some(files) = app.internal_data_path() {
        // SAFETY: nothing else runs yet; this is the process's first thread
        // that reads the environment.
        unsafe {
            std::env::set_var("HOME", &files);
            std::env::set_var("TMPDIR", files.join("tmp"));
        }
        let _ = std::fs::create_dir_all(files.join("tmp"));
    }
    init_logging();
    std::panic::set_hook(Box::new(|info| {
        error!(
            "PANIC: {info}\n{}",
            std::backtrace::Backtrace::force_capture()
        );
    }));
    info!(
        version = env!("CARGO_PKG_VERSION"),
        "starpod starting on Android"
    );

    let activity = app.clone();
    if let Err(error) = slint::android::init(app) {
        error!(%error, "the Android window backend could not start");
        return;
    }
    if let Err(error) = run(&activity) {
        error!(%error, "starpod stopped with an error");
    }
}

fn run(app: &AndroidApp) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("starpod-runtime")
        .build()?;
    let mut service = AppService::open_default()?;
    let service = runtime.block_on(async move {
        service.load().await?;
        Ok::<_, starpod_app::AppError>(service.into_shared())
    })?;
    info!("service opened and loaded");
    if let Err(error) = vpn::install(app, runtime.handle(), service.clone()) {
        error!(%error, "the VPN could not be set up");
    }
    let outcome = ui::run(
        runtime.handle().clone(),
        service.clone(),
        ui::Launch {
            link: take_pending_link(),
            forwards: None,
            start_hidden: false,
        },
    );
    // The activity is going away: stop the engine rather than orphan it.
    runtime.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(5), async {
            let _ = service.lock().await.disconnect().await;
        })
        .await;
    });
    runtime.shutdown_timeout(Duration::from_secs(5));
    outcome
}

/// A deep link left in `<files>/link.txt`, used once and then removed.
///
/// A NativeActivity does not hand its intent to native code, so this is how
/// a link reaches the app for now: a debuggable build can be given one with
/// `adb shell run-as org.foxgirls.starpod sh -c 'echo <link> > files/link.txt'`.
/// Only the app (or `run-as` on a debuggable build) can write there.
fn take_pending_link() -> Option<String> {
    let path = std::path::PathBuf::from(std::env::var_os("HOME")?).join("link.txt");
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    let link = text.trim().to_string();
    (!link.is_empty()).then(|| {
        info!("opening the link left for this start");
        link
    })
}

/// Send tracing to logcat.
fn init_logging() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_ansi(false)
        .without_time()
        .with_writer(Logcat)
        .try_init();
}

/// A tracing writer that hands each line to logcat.
struct Logcat;

/// One event's text, written to logcat when it is dropped.
struct LogcatLine(Vec<u8>);

impl<'a> MakeWriter<'a> for Logcat {
    type Writer = LogcatLine;

    fn make_writer(&'a self) -> Self::Writer {
        LogcatLine(Vec::new())
    }
}

impl Write for LogcatLine {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for LogcatLine {
    fn drop(&mut self) {
        let text: Vec<u8> = self.0.iter().copied().filter(|&b| b != 0).collect();
        let (Ok(tag), Ok(text)) = (CString::new("starpod"), CString::new(text)) else {
            return;
        };
        // SAFETY: both pointers are valid NUL-terminated strings for the call.
        unsafe {
            __android_log_write(LOG_INFO, tag.as_ptr(), text.as_ptr());
        }
    }
}
