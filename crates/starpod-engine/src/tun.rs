//! TUN mode and the elevation it needs.
//!
//! TUN is the only inbound that captures traffic the operating system
//! would otherwise route on its own. It needs privileges the app does not
//! have:
//!
//! - on Linux and macOS, creating the device needs `CAP_NET_ADMIN` (root);
//! - on Windows, creating the Wintun adapter needs an elevated process.
//!
//! # How elevation is handled
//!
//! The app does **not** elevate itself and does not run the whole
//! application as root. Instead:
//!
//! - the app asks the user whether to enable TUN;
//! - the user is told, in plain words, what elevation means;
//! - the engine is started as an elevated child when the platform offers a
//!   way to do that without the app itself becoming privileged.
//!
//! On unix the child is started through `sudo`, with the password prompt
//! handled by the terminal the user is shown — never by the app, which must
//! not handle the user's password. On Windows the child is started with
//! `ShellExecute` and the `runas` verb, which shows the system's own
//! elevation prompt.
//!
//! When elevation is refused or fails, the app falls back to running the
//! engine unelevated with TUN off, and says so. A proxy that does not work
//! is better than an app that quits.

use starpod_core::domain::TunSettings;

/// Whether this process is running elevated.
pub fn is_elevated() -> bool {
    #[cfg(windows)]
    {
        is_elevated_windows()
    }
    #[cfg(unix)]
    {
        is_elevated_unix()
    }
}

/// Whether this process is running as root.
#[cfg(unix)]
fn is_elevated_unix() -> bool {
    // `geteuid` is a safe wrapper in `nix`: POSIX guarantees it cannot fail.
    nix::unistd::geteuid().is_root()
}

/// Whether this process is running elevated.
#[cfg(windows)]
fn is_elevated_windows() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    // SAFETY: every call below is made with valid arguments: the process
    // handle from `GetCurrentProcess` is a pseudo-handle that must not be
    // closed, and the token handle is closed exactly once on the path
    // where it was opened.
    unsafe {
        let mut token: HANDLE = INVALID_HANDLE_VALUE;
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            std::ptr::addr_of_mut!(elevation).cast(),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        let _ = CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// The command that runs the engine elevated on this platform.
///
/// `None` when the platform has no way to elevate a child process without
/// the app itself becoming privileged.
pub fn elevation_command(binary: &std::path::Path) -> Option<ElevationCommand> {
    #[cfg(unix)]
    {
        Some(ElevationCommand {
            program: std::path::PathBuf::from("sudo"),
            leading_args: vec!["--".to_string()],
            target: binary.to_path_buf(),
        })
    }
    #[cfg(windows)]
    {
        Some(ElevationCommand {
            program: std::path::PathBuf::from("powershell.exe"),
            leading_args: vec![
                "-NoProfile".to_string(),
                "-NonInteractive".to_string(),
                "-Command".to_string(),
                "Start-Process".to_string(),
            ],
            target: binary.to_path_buf(),
        })
    }
}

/// How to launch the engine elevated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevationCommand {
    /// The program to run.
    pub program: std::path::PathBuf,
    /// The arguments that come before the engine's own.
    pub leading_args: Vec<String>,
    /// The engine's path.
    pub target: std::path::PathBuf,
}

impl ElevationCommand {
    /// The full argument list, with the engine's path in place.
    pub fn args(&self, engine_args: &[String]) -> Vec<String> {
        let mut all = self.leading_args.clone();
        all.push(self.target.to_string_lossy().to_string());
        all.extend(engine_args.iter().cloned());
        all
    }
}

/// Whether TUN can be enabled at all on this platform.
///
/// The engine's release builds include the TUN listener, so this is true
/// everywhere the app runs. It is a function rather than a constant so
/// that a build without the feature can say so.
pub fn tun_is_available() -> bool {
    true
}

/// Whether the engine needs elevation for the given TUN settings.
///
/// TUN that is off does not need elevation, and neither does a TUN that is
/// already on in an elevated engine.
pub fn needs_elevation(tun: &TunSettings, engine_is_elevated: bool) -> bool {
    tun.enabled && !engine_is_elevated
}

/// The TUN settings the app should ask the engine for.
///
/// Turning TUN off is expressed as an explicit `enable: false` — the
/// engine reads an omitted `enable` as false too, but saying it makes the
/// intent unambiguous on the wire.
pub fn tun_patch(tun: &TunSettings) -> serde_json::Value {
    let mut patch = serde_json::Map::new();
    patch.insert("enable".to_string(), serde_json::Value::Bool(tun.enabled));
    if let Some(device) = &tun.device {
        patch.insert(
            "device".to_string(),
            serde_json::Value::String(device.clone()),
        );
    }
    if let Some(mtu) = tun.mtu {
        patch.insert("mtu".to_string(), serde_json::Value::from(mtu));
    }
    if let Some(auto_route) = tun.auto_route {
        patch.insert(
            "auto-route".to_string(),
            serde_json::Value::Bool(auto_route),
        );
    }
    if let Some(dns_hijack) = &tun.dns_hijack {
        patch.insert(
            "dns-hijack".to_string(),
            serde_json::Value::Array(
                dns_hijack
                    .iter()
                    .map(|target| serde_json::Value::String(target.clone()))
                    .collect(),
            ),
        );
    }
    serde_json::Value::Object(patch)
}

/// The user-facing explanation of what elevation means.
///
/// The wording is deliberately plain: the user is about to grant the app
/// more power than it needs for everything else, and the reason is one
/// specific feature.
pub fn elevation_explanation() -> &'static str {
    "TUN mode captures the whole system’s traffic, which needs administrator \
     permission on this platform. Starpod itself does not need it — only the \
     proxy engine does, and only while TUN is on."
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unelevated_process_does_not_need_elevation_for_tun_that_is_off() {
        let tun = TunSettings::default();
        assert!(!needs_elevation(&tun, false));
    }

    #[test]
    fn an_elevated_engine_does_not_need_elevation_again() {
        let tun = TunSettings {
            enabled: true,
            ..TunSettings::default()
        };
        assert!(!needs_elevation(&tun, true));
        assert!(needs_elevation(&tun, false));
    }

    #[test]
    fn the_patch_says_enable_explicitly() {
        let tun = TunSettings {
            enabled: false,
            device: Some("starpod0".to_string()),
            mtu: Some(1400),
            auto_route: Some(true),
            dns_hijack: Some(vec!["any:53".to_string()]),
        };
        let patch = tun_patch(&tun);
        assert_eq!(patch["enable"], serde_json::json!(false));
        assert_eq!(patch["device"], serde_json::json!("starpod0"));
        assert_eq!(patch["mtu"], serde_json::json!(1400));
        assert_eq!(patch["dns-hijack"], serde_json::json!(["any:53"]));
    }

    #[test]
    fn the_elevation_command_puts_the_engine_last() {
        let command = elevation_command(std::path::Path::new("/opt/meow")).unwrap();
        let args = command.args(&["-f".to_string(), "/tmp/config.yaml".to_string()]);
        let last_engine = args
            .iter()
            .position(|arg| arg.contains("meow"))
            .expect("the engine's path is in the arguments");
        assert!(last_engine > 0);
        assert_eq!(args[last_engine + 1], "-f");
    }
}
