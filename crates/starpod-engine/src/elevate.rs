//! Starting the engine elevated, for TUN.
//!
//! Only the engine process is elevated. The app itself never is: an elevated
//! GUI would hand every window, the tray and the browser-facing parts of the
//! app the same power as the proxy. The elevation prompt belongs to the
//! engine alone, and the app never sees the user's password.
//!
//! # Three mechanisms
//!
//! | platform | how the engine is started | can the app stop it? |
//! |----------|---------------------------|-----------------------|
//! | Windows  | `ShellExecuteExW` with verb `runas` and `SW_HIDE`, returning the engine's own process handle (`SEE_MASK_NOCLOSEPROCESS`) | yes, through the handle (`TerminateProcess`); a graceful console event is attempted first and usually refused |
//! | Linux    | `pkexec /bin/sh -c <wrapper> ...`, a polkit prompt | only with a second `pkexec` prompt (`kill`) |
//! | macOS    | `osascript -e 'do shell script ... with administrator privileges'` | only with a second prompt (`kill`) |
//!
//! The unix wrapper script runs `exec` on the engine, so the shell's pid is
//! the engine's pid. The script writes that pid to a file in the app's own
//! directory before the exec. The app learns the pid from that file, which
//! is the only way to name the root-owned engine to a later `kill`.
//!
//! # What crosses the elevation boundary
//!
//! The engine's arguments: `-f <config>`, `--secret <secret>` and
//! `--ext-ctl 127.0.0.1:<port>`. Environment variables do not survive
//! `ShellExecuteExW`, `pkexec` or `osascript`, so `MEOW_RUNTIME_DIR` is not
//! set for an elevated engine. Its controller file therefore goes to the
//! engine's default directory. The app does not read that file for an
//! elevated engine, because `--ext-ctl` names the address directly.
//!
//! The config file the engine reads is the app's own 0600 file, which an
//! elevated process can read; nothing else is shared with the elevated side.
//!
//! # Quoting
//!
//! No shell ever parses the engine's arguments as text, except the two
//! fixed scripts below, which take every value as a positional parameter.
//! Windows arguments are quoted with the MSVCRT rules; POSIX arguments with
//! single quotes; the AppleScript wrapper escapes the POSIX text once more.

use std::path::{Path, PathBuf};
#[cfg(any(unix, test))]
use std::process::Stdio;

use crate::engine::ExitInfo;
use crate::error::{EngineError, EngineResult};

/// The arguments the engine is started with.
///
/// `controller` is `Some` for an elevated engine, whose address the app chose.
/// The secret is on the command line because it is the engine's only input
/// channel for it (see `supervisor.rs`).
pub fn engine_arguments(config_path: &Path, secret: &str, controller: Option<&str>) -> Vec<String> {
    // The home directory is the configuration's directory: the geodata the
    // app downloads sits there, and the engine reads it from there.
    let home = config_path.parent().unwrap_or_else(|| Path::new("."));
    let mut args = vec![
        "-d".to_string(),
        home.to_string_lossy().into_owned(),
        "-f".to_string(),
        config_path.to_string_lossy().into_owned(),
        "--secret".to_string(),
        secret.to_string(),
    ];
    if let Some(controller) = controller {
        args.push("--ext-ctl".to_string());
        args.push(controller.to_string());
    }
    args
}

/// Quote one argument for `/bin/sh`: single quotes, with embedded quotes
/// written as `'\''`. The result is always one word to the shell.
pub fn posix_quote(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('\'');
    for ch in arg.chars() {
        if ch == '\'' {
            quoted.push_str("'\\''");
        } else {
            quoted.push(ch);
        }
    }
    quoted.push('\'');
    quoted
}

/// Quote a whole argv for `/bin/sh`, space-separated.
pub fn posix_command_line(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| posix_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Quote one argument by the MSVCRT rules that `CommandLineToArgvW` reads.
///
/// Every argument is wrapped in quotes. A run of backslashes is doubled only
/// when it comes before a quote, whether that quote is an embedded one or the
/// closing one, so a path ending in a backslash cannot escape the final quote.
pub fn windows_quote(arg: &str) -> String {
    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            other => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push(other);
                backslashes = 0;
            }
        }
    }
    quoted.extend(std::iter::repeat_n('\\', backslashes * 2));
    quoted.push('"');
    quoted
}

/// The Windows command line for the engine's arguments (not the program).
pub fn windows_command_line(args: &[String]) -> String {
    args.iter()
        .map(|arg| windows_quote(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The fixed script the elevated process runs.
///
/// `$1` is the pid file, `$2` is the engine, and the rest are its arguments.
/// `$$` is this shell's pid, and `exec` keeps it, so the file names the engine.
const WRAPPER_SCRIPT: &str = r#"printf '%s\n' "$$" > "$1" && shift && exec "$@""#;

/// The argv that runs the engine through the wrapper.
///
/// The engine's path must be absolute: the wrapper runs as root, and a
/// relative path would resolve against the caller's working directory.
pub fn wrapper_argv(pidfile: &Path, binary: &Path, args: &[String]) -> Vec<String> {
    let mut argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        WRAPPER_SCRIPT.to_string(),
        "sh".to_string(),
        pidfile.to_string_lossy().into_owned(),
        binary.to_string_lossy().into_owned(),
    ];
    argv.extend(args.iter().cloned());
    argv
}

/// The signal an elevated engine is sent. Only these two are ever used.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopSignal {
    /// `SIGTERM`: the engine's graceful shutdown.
    Term,
    /// `SIGKILL`: after the grace period.
    Kill,
}

#[cfg(unix)]
impl StopSignal {
    fn name(self) -> &'static str {
        match self {
            Self::Term => "TERM",
            Self::Kill => "KILL",
        }
    }
}

/// The fixed script that signals a pid. `$1` is the signal name, `$2` the pid.
#[cfg(unix)]
const SIGNAL_SCRIPT: &str = r#"kill -s "$1" "$2""#;

/// The argv that sends a signal to `pid` through `/bin/sh`.
#[cfg(unix)]
pub fn signal_argv(pid: u32, signal: StopSignal) -> Vec<String> {
    vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        SIGNAL_SCRIPT.to_string(),
        "sh".to_string(),
        signal.name().to_string(),
        pid.to_string(),
    ]
}

/// The AppleScript that runs `argv` as an administrator.
///
/// The POSIX command line is built first, then escaped for an AppleScript
/// string literal, which only needs `\` and `"` escaped.
pub fn osascript_source(argv: &[String]) -> String {
    let shell = posix_command_line(argv);
    let escaped = shell.replace('\\', "\\\\").replace('"', "\\\"");
    format!("do shell script \"{escaped}\" with administrator privileges")
}

/// Read a pid file. Only a positive number is a pid; anything else is `None`.
pub fn read_pidfile(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    let pid: u32 = text.trim().parse().ok()?;
    (pid > 0).then_some(pid)
}

/// An engine started elevated, as the app can hold it.
pub enum Elevated {
    /// The unix wrapper (`pkexec` or `osascript`). It waits for the engine,
    /// so its exit is the engine's exit. Its pid is not the engine's.
    #[cfg(unix)]
    Wrapper(tokio::process::Child),
    /// The engine's own process, from `ShellExecuteExW`.
    #[cfg(windows)]
    Process(OwnedProcess),
}

impl Elevated {
    /// The engine's pid, when the app can know it without the pid file.
    pub fn pid(&self) -> Option<u32> {
        match self {
            #[cfg(unix)]
            Self::Wrapper(_) => None,
            #[cfg(windows)]
            Self::Process(process) => Some(process.pid()),
        }
    }

    /// Take the wrapper's stdout, to drain into the engine's logs. The engine's
    /// own process on Windows has no pipes, so there is nothing to take.
    pub fn take_stdout(&mut self) -> Option<tokio::process::ChildStdout> {
        match self {
            #[cfg(unix)]
            Self::Wrapper(child) => child.stdout.take(),
            #[cfg(windows)]
            Self::Process(_) => None,
        }
    }

    /// Take the wrapper's stderr, as [`Self::take_stdout`].
    pub fn take_stderr(&mut self) -> Option<tokio::process::ChildStderr> {
        match self {
            #[cfg(unix)]
            Self::Wrapper(child) => child.stderr.take(),
            #[cfg(windows)]
            Self::Process(_) => None,
        }
    }

    /// Wait for the engine to exit.
    pub async fn wait(&mut self) -> ExitInfo {
        match self {
            #[cfg(unix)]
            Self::Wrapper(child) => match child.wait().await {
                Ok(status) => ExitInfo::from_status(status),
                Err(_) => ExitInfo::unknown(),
            },
            #[cfg(windows)]
            Self::Process(process) => process.wait().await,
        }
    }

    /// Ask the engine to shut down gracefully.
    ///
    /// On unix this needs a second elevation: the engine is root, so a
    /// `kill` from the app's own user is refused. If the user declines that
    /// prompt, nothing is sent and the caller falls back to [`Self::force_stop`]
    /// after the grace period.
    #[cfg_attr(not(unix), allow(unused_variables))]
    pub async fn request_stop(&mut self, pidfile: &Path) {
        match self {
            #[cfg(unix)]
            Self::Wrapper(_) => {
                if let Some(pid) = read_pidfile(pidfile) {
                    let _ = signal_elevated(pid, StopSignal::Term).await;
                }
            }
            #[cfg(windows)]
            Self::Process(process) => {
                crate::signal::request_graceful(process.pid());
            }
        }
    }

    /// End the engine at once.
    #[cfg_attr(not(unix), allow(unused_variables))]
    pub async fn force_stop(&mut self, pidfile: &Path) {
        match self {
            #[cfg(unix)]
            Self::Wrapper(child) => {
                if let Some(pid) = read_pidfile(pidfile) {
                    let _ = signal_elevated(pid, StopSignal::Kill).await;
                }
                let _ = child.start_kill();
            }
            #[cfg(windows)]
            Self::Process(process) => process.terminate(),
        }
    }
}

/// Start `binary` with `args` elevated.
///
/// `pidfile` is where the wrapper records the engine's pid (unix). The
/// engine is not yet running when this returns; the caller waits for its
/// controller. `EngineError::ElevationDenied` means the user refused the
/// prompt.
pub fn spawn_elevated(binary: &Path, args: &[String], pidfile: &Path) -> EngineResult<Elevated> {
    if !binary.is_absolute() {
        return Err(EngineError::Spawn(
            "the engine's path must be absolute to run it elevated".to_string(),
        ));
    }
    spawn_platform(binary, args, pidfile)
}

/// Send `signal` to the root-owned engine `pid`, through one more elevation.
#[cfg(unix)]
pub async fn signal_elevated(pid: u32, signal: StopSignal) -> EngineResult<()> {
    if pid == 0 {
        return Err(EngineError::Internal(
            "the engine's pid is not valid".to_string(),
        ));
    }
    let argv = signal_argv(pid, signal);
    let mut command = elevated_command(&argv);
    let status = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|error| EngineError::Spawn(error.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(EngineError::Internal(
            "the engine did not stop: the administrator permission to stop it was declined or failed"
                .to_string(),
        ))
    }
}

/// The elevated command for an argv, by platform.
#[cfg(all(unix, not(target_os = "macos")))]
fn elevated_command(argv: &[String]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("pkexec");
    command.args(argv);
    command
}

/// The elevated command for an argv, by platform.
#[cfg(target_os = "macos")]
fn elevated_command(argv: &[String]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("osascript");
    command.arg("-e").arg(osascript_source(argv));
    command
}

/// Spawn the unix wrapper with its output captured for the engine's logs.
#[cfg(unix)]
fn spawn_wrapper(mut command: tokio::process::Command) -> EngineResult<Elevated> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match command.spawn() {
        Ok(child) => Ok(Elevated::Wrapper(child)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(EngineError::Spawn(missing_tool_hint().to_string()))
        }
        Err(error) => Err(EngineError::Spawn(error.to_string())),
    }
}

/// What the user is told when the elevation tool is missing.
#[cfg(all(unix, not(target_os = "macos")))]
fn missing_tool_hint() -> &'static str {
    "pkexec is not installed, so TUN cannot ask for administrator permission. \
     Install polkit, or run Starpod with sudo to use TUN"
}

/// What the user is told when the elevation tool is missing.
#[cfg(target_os = "macos")]
fn missing_tool_hint() -> &'static str {
    "osascript is not available, so TUN cannot ask for administrator permission"
}

#[cfg(all(unix, not(target_os = "macos")))]
fn spawn_platform(binary: &Path, args: &[String], pidfile: &Path) -> EngineResult<Elevated> {
    let argv = wrapper_argv(pidfile, binary, args);
    let mut command = tokio::process::Command::new("pkexec");
    command.args(&argv);
    spawn_wrapper(command)
}

#[cfg(target_os = "macos")]
fn spawn_platform(binary: &Path, args: &[String], pidfile: &Path) -> EngineResult<Elevated> {
    let argv = wrapper_argv(pidfile, binary, args);
    let mut command = tokio::process::Command::new("osascript");
    command.arg("-e").arg(osascript_source(&argv));
    spawn_wrapper(command)
}

#[cfg(windows)]
fn spawn_platform(binary: &Path, args: &[String], _pidfile: &Path) -> EngineResult<Elevated> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::{ERROR_CANCELLED, GetLastError};
    use windows_sys::Win32::UI::Shell::{
        SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, ShellExecuteExW,
    };

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    let verb = wide(OsStr::new("runas"));
    let file = wide(binary.as_os_str());
    let parameters_text = windows_command_line(args);
    let parameters = wide(OsStr::new(&parameters_text));

    // `nShow` 0 is `SW_HIDE`: the engine's console stays hidden. The app's
    // window is the product, not the engine's console.
    const SW_HIDE: i32 = 0;
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        nShow: SW_HIDE,
        ..Default::default()
    };

    // SAFETY: `info` is fully initialised. Its string pointers refer to the
    // three buffers above, which live until the call returns, and the API
    // only reads them. `SEE_MASK_NOCLOSEPROCESS` makes it return the process
    // handle, which is then owned by `OwnedProcess`.
    let ok = unsafe { ShellExecuteExW(&mut info) };
    if ok == 0 {
        // SAFETY: `GetLastError` reads this thread's last-error value and has
        // no preconditions.
        let code = unsafe { GetLastError() };
        return Err(if code == ERROR_CANCELLED {
            EngineError::ElevationDenied
        } else {
            EngineError::Spawn(format!(
                "Windows could not start the engine with administrator permission (error {code})"
            ))
        });
    }
    if info.hProcess.is_null() {
        return Err(EngineError::Spawn(
            "Windows started no engine process".to_string(),
        ));
    }
    Ok(Elevated::Process(OwnedProcess::new(info.hProcess)))
}

/// A process handle the app owns, for an engine started by `ShellExecuteExW`.
#[cfg(windows)]
pub struct OwnedProcess {
    handle: windows_sys::Win32::Foundation::HANDLE,
    pid: u32,
}

// SAFETY: a process handle is an opaque kernel reference, valid on any thread.
// It is only used for waits, `TerminateProcess` and the final `CloseHandle`.
// Sharing it (`Sync`) is needed because `wait` borrows it across an await.
// The waits take no lock and `TerminateProcess` is thread-safe on a handle.
#[cfg(windows)]
unsafe impl Send for OwnedProcess {}

#[cfg(windows)]
unsafe impl Sync for OwnedProcess {}

#[cfg(windows)]
impl OwnedProcess {
    fn new(handle: windows_sys::Win32::Foundation::HANDLE) -> Self {
        // SAFETY: `handle` is a live process handle from `ShellExecuteExW`.
        let pid = unsafe { windows_sys::Win32::System::Threading::GetProcessId(handle) };
        Self { handle, pid }
    }

    /// The engine's pid.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// `TerminateProcess` on the engine. The engine gets no chance to close
    /// its connections or remove its routes, so the caller turns TUN off
    /// through the controller first.
    fn terminate(&self) {
        // SAFETY: the handle is live until `drop`, and `TerminateProcess` takes
        // only the handle and an exit code.
        unsafe {
            windows_sys::Win32::System::Threading::TerminateProcess(self.handle, 1);
        }
    }

    /// Wait for the engine to exit, on a blocking thread.
    async fn wait(&self) -> ExitInfo {
        let raw = self.handle as usize;
        let code = tokio::task::spawn_blocking(move || exit_code_of(raw as _)).await;
        match code {
            Ok(Some(code)) => {
                use std::os::windows::process::ExitStatusExt as _;
                ExitInfo::from_status(std::process::ExitStatus::from_raw(code))
            }
            _ => ExitInfo::unknown(),
        }
    }
}

/// Block until the process behind `handle` ends, and return its exit code.
#[cfg(windows)]
fn exit_code_of(handle: windows_sys::Win32::Foundation::HANDLE) -> Option<u32> {
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, INFINITE, WaitForSingleObject,
    };
    let mut code = 0u32;
    // SAFETY: `handle` is a live process handle, kept alive by the owning
    // `OwnedProcess` until after this call returns. `code` is a valid out-pointer.
    unsafe {
        let _ = WaitForSingleObject(handle, INFINITE);
        if GetExitCodeProcess(handle, &mut code) != 0 {
            Some(code)
        } else {
            None
        }
    }
}

#[cfg(windows)]
impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // SAFETY: the handle was returned by `ShellExecuteExW` and is closed
        // exactly once, here.
        unsafe {
            let _ = windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// The pid file path the wrapper writes, in `dir`.
pub fn pidfile_in(dir: &Path) -> PathBuf {
    dir.join("elevated-engine.pid")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> Vec<String> {
        vec![
            "-f".to_string(),
            r"C:\Users\Ann Lee\AppData\Local\Starpod\data\runtime-config.yaml".to_string(),
            "--secret".to_string(),
            "a1b2c3".to_string(),
            "--ext-ctl".to_string(),
            "127.0.0.1:50123".to_string(),
        ]
    }

    #[test]
    fn the_engine_arguments_carry_the_config_secret_and_controller() {
        let config = Path::new("/home/ann/data/runtime-config.yaml");
        let args = engine_arguments(config, "s3cret", Some("127.0.0.1:9090"));
        assert_eq!(
            args,
            vec![
                "-d",
                "/home/ann/data",
                "-f",
                "/home/ann/data/runtime-config.yaml",
                "--secret",
                "s3cret",
                "--ext-ctl",
                "127.0.0.1:9090",
            ]
        );
    }

    #[test]
    fn an_engine_without_a_chosen_controller_gets_no_ext_ctl_flag() {
        let args = engine_arguments(Path::new("/c.yaml"), "s", None);
        assert!(!args.iter().any(|arg| arg == "--ext-ctl"));
    }

    /// Split a Windows command line the way `CommandLineToArgvW` does, so the
    /// quoting is checked against the reader's rules and not against itself.
    fn split_windows(line: &str) -> Vec<String> {
        let chars: Vec<char> = line.chars().collect();
        let mut args = Vec::new();
        let mut current = String::new();
        let mut in_quotes = false;
        let mut has_arg = false;
        let mut i = 0;
        while i < chars.len() {
            match chars[i] {
                '\\' => {
                    let start = i;
                    while i < chars.len() && chars[i] == '\\' {
                        i += 1;
                    }
                    let count = i - start;
                    has_arg = true;
                    if i < chars.len() && chars[i] == '"' {
                        current.extend(std::iter::repeat_n('\\', count / 2));
                        if count % 2 == 1 {
                            current.push('"');
                            i += 1;
                        }
                    } else {
                        current.extend(std::iter::repeat_n('\\', count));
                    }
                }
                '"' => {
                    in_quotes = !in_quotes;
                    has_arg = true;
                    i += 1;
                }
                ' ' | '\t' if !in_quotes => {
                    if has_arg {
                        args.push(std::mem::take(&mut current));
                        has_arg = false;
                    }
                    i += 1;
                }
                other => {
                    current.push(other);
                    has_arg = true;
                    i += 1;
                }
            }
        }
        if has_arg {
            args.push(current);
        }
        args
    }

    #[test]
    fn windows_arguments_survive_spaces_quotes_and_trailing_backslashes() {
        let tricky = vec![
            r"C:\Program Files\Starpod\meow.exe".to_string(),
            "a b".to_string(),
            r#"say "hi""#.to_string(),
            r"ends\with\backslash\".to_string(),
            r#"x\"y"#.to_string(),
            String::new(),
            "tab\tin".to_string(),
            "s3cret&|<>^%".to_string(),
        ];
        let line = windows_command_line(&tricky);
        assert_eq!(split_windows(&line), tricky, "command line was: {line}");
    }

    #[test]
    fn a_windows_argument_with_a_space_is_one_quoted_word() {
        assert_eq!(
            windows_quote(r"C:\Program Files\x.yaml"),
            r#""C:\Program Files\x.yaml""#
        );
    }

    #[test]
    fn a_posix_argument_with_quotes_stays_one_word_for_the_shell() {
        assert_eq!(posix_quote("it's a $HOME"), r#"'it'\''s a $HOME'"#);
        assert_eq!(posix_quote(""), "''");
    }

    #[test]
    fn the_osascript_source_escapes_quotes_and_backslashes_once() {
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            r#"kill -s "$1" "$2""#.to_string(),
            "sh".to_string(),
            "TERM".to_string(),
            "42".to_string(),
        ];
        assert_eq!(
            osascript_source(&argv),
            r#"do shell script "'/bin/sh' '-c' 'kill -s \"$1\" \"$2\"' 'sh' 'TERM' '42'" with administrator privileges"#
        );
    }

    #[test]
    fn a_path_with_a_quote_or_backslash_cannot_end_the_applescript_string() {
        let argv = vec!["/opt/a\"b\\c/meow".to_string()];
        let source = osascript_source(&argv);
        // The only unescaped quotes are the two that delimit the string.
        let unescaped = source
            .char_indices()
            .filter(|(i, c)| *c == '"' && !source[..*i].ends_with('\\'))
            .count();
        assert_eq!(unescaped, 2, "{source}");
    }

    #[test]
    fn the_wrapper_runs_the_program_with_its_arguments_and_names_its_pid() {
        let printf = Path::new("/usr/bin/printf");
        if !printf.exists() {
            return;
        }
        let dir = tempfile::TempDir::new().unwrap();
        let pidfile = dir.path().join("engine.pid");
        let args = vec![
            "%s|".to_string(),
            "a b".to_string(),
            "it's".to_string(),
            "q\"x".to_string(),
            "back\\slash".to_string(),
        ];
        let argv = wrapper_argv(&pidfile, printf, &args);
        let child = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let child_pid = child.id();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            "a b|it's|q\"x|back\\slash|"
        );
        // `exec` keeps the shell's pid, so the file names the program itself.
        assert_eq!(read_pidfile(&pidfile), Some(child_pid));
    }

    #[test]
    fn a_pid_file_only_yields_a_positive_pid() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("p");
        std::fs::write(&file, "0\n").unwrap();
        assert_eq!(read_pidfile(&file), None);
        std::fs::write(&file, " 4242\n").unwrap();
        assert_eq!(read_pidfile(&file), Some(4242));
        std::fs::write(&file, "not a pid").unwrap();
        assert_eq!(read_pidfile(&file), None);
        assert_eq!(read_pidfile(&dir.path().join("missing")), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_signal_argv_passes_the_pid_as_a_plain_argument() {
        let argv = signal_argv(77, StopSignal::Kill);
        assert_eq!(argv.last().map(String::as_str), Some("77"));
        assert!(argv.contains(&"KILL".to_string()));
        assert_eq!(argv[2], SIGNAL_SCRIPT);
    }

    #[test]
    fn a_relative_engine_path_is_refused_before_anything_is_started() {
        let error = spawn_elevated(Path::new("meow"), &args(), Path::new("/tmp/engine.pid"))
            .err()
            .expect("a relative path is refused");
        assert!(matches!(error, EngineError::Spawn(_)), "{error:?}");
    }
}
