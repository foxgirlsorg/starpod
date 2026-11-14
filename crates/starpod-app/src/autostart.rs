//! Starting Starpod when the user logs in.
//!
//! Windows writes a per-user `Run` value, Linux writes an XDG autostart
//! desktop entry, and macOS writes a per-user LaunchAgent. Each one starts the
//! executable with `--autostart`, which lets the app tell a login start from a
//! manual one. The executable path is written every time the setting is
//! applied, so it stays correct after the app is moved.

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::io;
use std::path::Path;
#[cfg(target_os = "macos")]
use std::path::PathBuf;

/// The command-line flag an autostart entry passes.
pub const AUTOSTART_FLAG: &str = "--autostart";

/// The name of the entry, in every platform's registry or directory.
pub const ENTRY_NAME: &str = "Starpod";

/// Whether this process was started by the login entry.
pub fn launched_by_autostart() -> bool {
    std::env::args().any(|argument| argument == AUTOSTART_FLAG)
}

/// The command line a Windows `Run` value holds: the quoted executable path
/// and the flag.
pub fn run_value(executable: &Path) -> String {
    format!("\"{}\" {AUTOSTART_FLAG}", executable.display())
}

/// The text of the XDG desktop entry.
pub fn desktop_entry(executable: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName={ENTRY_NAME}\nExec={} {AUTOSTART_FLAG}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        desktop_quote(&executable.display().to_string())
    )
}

/// Quote one `Exec=` argument for a desktop entry: wrap it in double quotes
/// and escape the characters the desktop entry specification reserves.
fn desktop_quote(argument: &str) -> String {
    let mut quoted = String::from("\"");
    for character in argument.chars() {
        if matches!(character, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(character);
    }
    quoted.push('"');
    quoted
}

/// The text of the macOS LaunchAgent property list.
pub fn launch_agent(executable: &Path) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\
         \t<string>org.starpod.app</string>\n\
         \t<key>ProgramArguments</key>\n\
         \t<array>\n\
         \t\t<string>{}</string>\n\
         \t\t<string>{AUTOSTART_FLAG}</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\
         \t<true/>\n\
         </dict>\n\
         </plist>\n",
        xml_escape(&executable.display().to_string())
    )
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Turn the login start on or off, and write the current executable path
/// when it is on.
pub fn set_enabled(enabled: bool) -> Result<(), String> {
    let executable = std::env::current_exe()
        .map_err(|error| format!("the program path is not known: {error}"))?;
    apply(enabled, &executable)
        .map_err(|error| format!("the login start could not be set: {error}"))
}

#[cfg(windows)]
fn apply(enabled: bool, executable: &Path) -> Result<(), String> {
    let value = enabled.then(|| run_value(executable));
    crate::sysproxy::set_run_value(value.as_deref()).map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn apply(enabled: bool, executable: &Path) -> Result<(), String> {
    let directory = dirs::config_dir()
        .ok_or_else(|| "the configuration directory is not known".to_string())?
        .join("autostart");
    let path = directory.join("starpod.desktop");
    if enabled {
        write_file(&path, &desktop_entry(executable)).map_err(|error| error.to_string())
    } else {
        remove_file(&path).map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "macos")]
fn apply(enabled: bool, executable: &Path) -> Result<(), String> {
    let path = launch_agent_path().ok_or_else(|| "the home directory is not known".to_string())?;
    if enabled {
        write_file(&path, &launch_agent(executable)).map_err(|error| error.to_string())
    } else {
        remove_file(&path).map_err(|error| error.to_string())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn apply(_enabled: bool, _executable: &Path) -> Result<(), String> {
    Err("the login start is not supported on this platform".into())
}

/// Where the macOS LaunchAgent is written.
#[cfg(target_os = "macos")]
fn launch_agent_path() -> Option<PathBuf> {
    dirs::home_dir().map(|home| {
        home.join("Library")
            .join("LaunchAgents")
            .join("org.starpod.app.plist")
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn write_file(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, text)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn remove_file(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_run_value_quotes_the_path_and_adds_the_flag() {
        let value = run_value(Path::new(r"C:\Program Files\Starpod\starpod.exe"));
        assert_eq!(
            value,
            "\"C:\\Program Files\\Starpod\\starpod.exe\" --autostart"
        );
    }

    #[test]
    fn the_desktop_entry_starts_the_quoted_executable_with_the_flag() {
        let entry = desktop_entry(Path::new("/opt/Star pod/starpod"));
        assert!(entry.contains("Exec=\"/opt/Star pod/starpod\" --autostart\n"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
    }

    #[test]
    fn desktop_quoting_escapes_reserved_characters() {
        assert_eq!(desktop_quote(r#"a"b$c`d\e"#), r#""a\"b\$c\`d\\e""#);
    }

    #[test]
    fn the_launch_agent_runs_at_load_with_the_flag() {
        let plist = launch_agent(Path::new("/Applications/Star&pod/starpod"));
        assert!(plist.contains("<string>org.starpod.app</string>"));
        assert!(plist.contains("<string>/Applications/Star&amp;pod/starpod</string>"));
        assert!(plist.contains("<string>--autostart</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
    }
}
