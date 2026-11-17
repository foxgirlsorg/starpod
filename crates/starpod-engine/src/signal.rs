//! Asking an engine process to stop, by its process id.
//!
//! Two requests, each reporting whether it could be delivered:
//!
//! - [`request_graceful`] asks the engine to shut down cleanly. On unix that
//!   is `SIGTERM`. On Windows it is a `CTRL_C_EVENT` delivered to the console
//!   the engine owns (see the Windows section below for why it is not
//!   `CTRL_BREAK_EVENT`).
//! - [`force_kill`] ends the process at once. On unix `SIGKILL`; on Windows
//!   `TerminateProcess`.
//!
//! Both take a pid rather than a `Child`, so a stop can be sent while another
//! task owns the child and is waiting on it.
//!
//! # Windows: why CTRL_C through a shared console
//!
//! The engine shuts down when `tokio::signal::ctrl_c()` fires
//! (`meow-app-0.29.0/src/main.rs:142-175`). On Windows that listener is wired
//! to `CTRL_C_EVENT` only: tokio's `sys.rs` maps `CTRL_BREAK_EVENT` to a
//! separate `ctrl_break()` listener that the engine does not use
//! (`tokio-1.54.1/src/signal/windows/sys.rs:108-115`). So a `CTRL_BREAK_EVENT`,
//! which the older design sent, is silently ignored by the engine. The
//! integration contract's claim that `ctrl_c()` also fires on break is wrong.
//!
//! The engine's child is also spawned without `CREATE_NEW_PROCESS_GROUP`,
//! because that flag disables `CTRL_C` for the new group, so the event would
//! never be delivered at all.
//!
//! `GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)` reaches every process on the
//! console the caller is attached to. The app therefore attaches to the
//! engine's *own* console (it was created hidden for the engine with
//! `CREATE_NO_WINDOW`, see `supervisor.rs`), ignores the event in itself, and
//! sends it. The attempt is refused when the app itself has a console, because
//! then the engine shares the terminal and the event would reach the user's
//! shell; the caller then falls back to a forced stop after the grace period.

/// Ask the process `pid` to shut down cleanly. Returns whether the request
/// was delivered. A `false` means the caller should wait out the grace
/// period and then force the stop.
pub fn request_graceful(pid: u32) -> bool {
    #[cfg(unix)]
    {
        send_unix(pid, nix::sys::signal::Signal::SIGTERM)
    }
    #[cfg(windows)]
    {
        ctrl_c_to_console_of(pid)
    }
}

/// End the process `pid` at once. Returns whether the request was delivered.
pub fn force_kill(pid: u32) -> bool {
    #[cfg(unix)]
    {
        send_unix(pid, nix::sys::signal::Signal::SIGKILL)
    }
    #[cfg(windows)]
    {
        terminate_pid(pid)
    }
}

/// Send `signal` to `pid`, refusing pids that would address a process group.
///
/// `kill(0, _)` and `kill(-n, _)` signal whole groups; a `u32` above
/// `i32::MAX` would become negative. Neither may ever reach `kill`.
#[cfg(unix)]
fn send_unix(pid: u32, signal: nix::sys::signal::Signal) -> bool {
    let Ok(raw) = i32::try_from(pid) else {
        return false;
    };
    if raw <= 0 {
        return false;
    }
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(raw), signal).is_ok()
}

/// Send `CTRL_C_EVENT` to the console that `pid` owns. See the module docs.
#[cfg(windows)]
fn ctrl_c_to_console_of(pid: u32) -> bool {
    use windows_sys::Win32::System::Console::{
        AttachConsole, CTRL_C_EVENT, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleWindow,
        SetConsoleCtrlHandler,
    };

    // SAFETY: every call takes plain integers or a handler routine that lives
    // for the whole process, and each result is checked.
    unsafe {
        // A console window that belongs to this app means the engine would
        // share it with the user's terminal; refuse rather than reach it.
        if !GetConsoleWindow().is_null() {
            return false;
        }
        // The event reaches this process too, on a thread Windows starts for
        // it, at some point after `GenerateConsoleCtrlEvent` returns. A
        // handler that is switched off again right after sending can be off
        // by then, and Windows' default handling ends the app at once, with
        // no panic and no crash report. So the handler is installed once and
        // never removed. It is a routine rather than `SetConsoleCtrlHandler(
        // None, TRUE)`, because that "ignore Ctrl+C" state is inherited by
        // child processes, and the engine started next would ignore its stop.
        static SWALLOW: std::sync::Once = std::sync::Once::new();
        SWALLOW.call_once(|| {
            SetConsoleCtrlHandler(Some(swallow_ctrl_c), 1);
        });
        if AttachConsole(pid) == 0 {
            return false;
        }
        let sent = GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) != 0;
        FreeConsole();
        sent
    }
}

/// The app's own Ctrl+C handler: it only receives the event it sends to the
/// engine's console, so it marks Ctrl+C as handled. Other events (close,
/// log off, shut down) go on to the default handling.
#[cfg(windows)]
unsafe extern "system" fn swallow_ctrl_c(ctrl_type: u32) -> windows_sys::core::BOOL {
    i32::from(ctrl_type == windows_sys::Win32::System::Console::CTRL_C_EVENT)
}

/// `TerminateProcess` on `pid`, through a handle opened for that purpose.
#[cfg(windows)]
fn terminate_pid(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};

    // SAFETY: `OpenProcess` returns either null or a handle we own; the
    // handle is used only before it is closed exactly once.
    unsafe {
        let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if handle.is_null() {
            return false;
        }
        let terminated = TerminateProcess(handle, 1) != 0;
        let _ = CloseHandle(handle);
        terminated
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_pid_that_would_address_a_group_is_never_signaled() {
        // Pid 0 means "the caller's process group"; a pid above i32::MAX
        // would wrap to a negative group. Both must be refused outright.
        assert!(!request_graceful(0));
        assert!(!force_kill(0));
        assert!(!request_graceful(u32::MAX));
        assert!(!force_kill(u32::MAX));
    }

    #[cfg(unix)]
    #[test]
    fn a_pid_that_does_not_exist_is_reported_as_not_delivered() {
        // A very large pid that the kernel will not have assigned.
        assert!(!force_kill(4_000_000_000 / 2));
    }
}
