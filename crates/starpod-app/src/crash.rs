//! Crash reports for panics.
//!
//! The desktop app and the Android app both install a panic hook that calls
//! [`write_panic_report`], so a panic leaves a file of its own in the crashes
//! folder (see `starpod_store::crash`). The writer itself lives in the store,
//! where the engine's exit reports are written too.

use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};

/// The panic's message, or a note when the payload is not text.
pub fn panic_message(info: &PanicHookInfo<'_>) -> String {
    info.payload()
        .downcast_ref::<&str>()
        .map(|text| (*text).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_string())
}

/// The `file:line:column` the panic was raised at.
pub fn panic_location(info: &PanicHookInfo<'_>) -> String {
    info.location()
        .map_or_else(|| "unknown location".to_string(), ToString::to_string)
}

/// Write a crash report for `info` into `crash_dir`.
///
/// The report holds the app version, the operating system and architecture,
/// the time, the thread's name, the message, the location and a forced
/// backtrace. Errors are ignored: this runs inside a panic hook, where a
/// second panic would abort the process before the first one is handled.
pub fn write_panic_report(info: &PanicHookInfo<'_>, crash_dir: &Path) -> Option<PathBuf> {
    let thread = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .to_string();
    let time = starpod_core::timestamp::now().unwrap_or_else(|| "unknown".to_string());
    let backtrace = std::backtrace::Backtrace::force_capture();
    let body = format!(
        "Starpod crash report\n\
         version: {version}\n\
         os: {os} {arch}\n\
         time: {time}\n\
         thread: {thread}\n\
         panic: {message}\n\
         location: {location}\n\n\
         backtrace:\n{backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        message = panic_message(info),
        location = panic_location(info),
    );
    starpod_store::crash::write_report(crash_dir, "crash", &body)
}
