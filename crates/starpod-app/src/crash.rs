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

/// Where [`native_crash_filter`] writes its report. Set once, before the
/// filter is installed.
#[cfg(windows)]
static NATIVE_CRASH_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Write a crash report into `crash_dir` when the process dies of a native
/// fault, such as an access violation or a stack overflow, that never
/// reaches the panic hook. Windows only; elsewhere this does nothing.
pub fn install_native_crash_handler(crash_dir: PathBuf) {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter;

        if NATIVE_CRASH_DIR.set(crash_dir).is_ok() {
            // SAFETY: the filter is a plain function that lives for the
            // whole process, and it only reads the record Windows passes.
            unsafe {
                SetUnhandledExceptionFilter(Some(native_crash_filter));
            }
        }
    }
    #[cfg(not(windows))]
    let _ = crash_dir;
}

/// The unhandled-exception filter: writes the exception code and address,
/// then lets Windows go on ending the process.
#[cfg(windows)]
unsafe extern "system" fn native_crash_filter(
    info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    /// Let the next handler, and then Windows, deal with the exception.
    const EXCEPTION_CONTINUE_SEARCH: i32 = 0;

    // SAFETY: Windows passes a valid pointer, or null, which is checked.
    let (code, address) = unsafe {
        info.as_ref()
            .and_then(|pointers| pointers.ExceptionRecord.as_ref())
            .map_or((0, 0), |record| {
                (
                    record.ExceptionCode as u32,
                    record.ExceptionAddress as usize,
                )
            })
    };
    if let Some(dir) = NATIVE_CRASH_DIR.get() {
        let thread = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        let kind = match code {
            0xC000_0005 => "access violation",
            0xC000_00FD => "stack overflow",
            0xC000_0409 => "stack buffer overrun",
            0xC000_001D => "illegal instruction",
            _ => "native exception",
        };
        let body = format!(
            "Starpod crash report\n\
             version: {version}\n\
             os: {os} {arch}\n\
             thread: {thread}\n\
             crash: {kind} (code {code:#010x}) at {address:#x}\n",
            version = env!("CARGO_PKG_VERSION"),
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
        );
        let _ = starpod_store::crash::write_report(dir, "crash", &body);
    }
    EXCEPTION_CONTINUE_SEARCH
}
