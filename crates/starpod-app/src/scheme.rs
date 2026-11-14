//! Registers the `starpod://` scheme with the operating system, so a link
//! opened in a browser or a shell starts Starpod.
//!
//! Only `starpod` is registered. `clash://`, `mihomo://` and `koala-clash://`
//! belong to other clients, and taking them over would break those clients.
//! They are still parsed when they are passed on the command line.
//!
//! Registration runs at every start. It is idempotent, and a failure is only
//! logged, because the app works without it.

use tracing::warn;

/// The scheme this app registers.
pub const SCHEME: &str = "starpod";

/// Register the scheme for the current user, pointing at this executable.
pub fn register() {
    match std::env::current_exe() {
        Ok(exe) => platform::register(&exe),
        Err(error) => warn!(
            %error,
            "could not find the executable, so the starpod:// scheme is not registered"
        ),
    }
}

#[cfg(windows)]
mod platform {
    use std::io;
    use std::path::Path;

    use tracing::{info, warn};
    use winreg::RegKey;
    use winreg::enums::HKEY_CURRENT_USER;

    pub fn register(exe: &Path) {
        match register_windows(exe) {
            Ok(()) => info!("the starpod:// scheme is registered for this user"),
            Err(error) => warn!(%error, "could not register the starpod:// scheme"),
        }
    }

    fn register_windows(exe: &Path) -> io::Result<()> {
        let exe = exe.display();
        let (root, _) =
            RegKey::predef(HKEY_CURRENT_USER).create_subkey(r"Software\Classes\starpod")?;
        root.set_value("", &"URL:Starpod")?;
        root.set_value("URL Protocol", &"")?;

        let (icon, _) = root.create_subkey("DefaultIcon")?;
        icon.set_value("", &format!("\"{exe}\",0"))?;

        let (command, _) = root.create_subkey(r"shell\open\command")?;
        command.set_value("", &format!("\"{exe}\" \"%1\""))?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use tracing::{debug, info, warn};

    const DESKTOP_FILE: &str = "starpod.desktop";
    const MIME_TYPE: &str = "x-scheme-handler/starpod";

    pub fn register(exe: &Path) {
        if let Err(error) = write_desktop_file(exe) {
            warn!(%error, "could not write the starpod desktop entry");
            return;
        }
        // No shell: the arguments are passed as they are.
        match Command::new("xdg-mime")
            .args(["default", DESKTOP_FILE, MIME_TYPE])
            .status()
        {
            Ok(status) if status.success() => {
                info!("the starpod:// scheme is registered for this user");
            }
            Ok(status) => debug!(?status, "xdg-mime did not set the starpod:// handler"),
            Err(error) => debug!(%error, "xdg-mime is not available"),
        }
    }

    /// Write the desktop entry, unless its content is already the same.
    fn write_desktop_file(exe: &Path) -> io::Result<()> {
        let path = applications_dir()?.join(DESKTOP_FILE);
        let content = desktop_entry(exe);
        if fs::read_to_string(&path).ok().as_deref() == Some(content.as_str()) {
            return Ok(());
        }
        fs::write(path, content)
    }

    fn applications_dir() -> io::Result<PathBuf> {
        let dir = dirs::data_dir()
            .ok_or_else(|| io::Error::other("there is no data directory"))?
            .join("applications");
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    fn desktop_entry(exe: &Path) -> String {
        format!(
            "[Desktop Entry]\nType=Application\nName=Starpod\nExec=\"{}\" %u\nTerminal=false\nNoDisplay=false\nMimeType={MIME_TYPE};\n",
            escape_exec(&exe.to_string_lossy())
        )
    }

    /// Escape the characters that keep their meaning inside a quoted Exec
    /// argument.
    fn escape_exec(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        for character in text.chars() {
            if matches!(character, '"' | '`' | '$' | '\\') {
                out.push('\\');
            }
            out.push(character);
        }
        out
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod platform {
    use std::path::Path;

    use tracing::debug;

    pub fn register(_exe: &Path) {
        // macOS registers a scheme through an app bundle's Info.plist, which
        // this build does not have. See docs/DEVELOPMENT.md.
        debug!("the starpod:// scheme is not registered on this platform");
    }
}

#[cfg(test)]
mod tests {
    use super::SCHEME;

    #[test]
    fn only_the_starpod_scheme_is_registered() {
        assert_eq!(SCHEME, "starpod");
    }
}
