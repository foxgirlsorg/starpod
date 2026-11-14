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
//! The app does **not** elevate itself. Only the engine process is elevated,
//! by [`crate::elevate`]: when TUN is wanted and the app is not elevated, the
//! engine is started through the platform's own prompt (`runas`, `pkexec`,
//! `osascript`). The user is told what that means, and the app never handles
//! their password.
//!
//! When elevation is refused or fails, the engine starts without TUN and the
//! proxy keeps working. A proxy that does not work is better than an app that
//! quits, and the user is told that TUN is off and why.

use starpod_core::domain::{RuntimeConfig, TunSettings};

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

/// The configuration with TUN turned off, for the fallback when elevation
/// fails. Everything else is unchanged.
pub fn without_tun(config: &RuntimeConfig) -> RuntimeConfig {
    let mut plain = config.clone();
    plain.tun.enabled = false;
    plain
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
    fn the_fallback_configuration_differs_only_in_tun() {
        let mut config = RuntimeConfig {
            mixed_port: Some(7890),
            socks_port: None,
            port: None,
            allow_lan: false,
            bind_address: None,
            mode: starpod_core::domain::ProxyMode::Rule,
            log_level: starpod_core::domain::LogLevel::Info,
            tun: TunSettings::default(),
            proxies: Vec::new(),
            proxy_groups: Vec::new(),
            rules: vec!["MATCH,DIRECT".to_string()],
            rule_providers: Vec::new(),
            subscriptions: Vec::new(),
            engine: Default::default(),
        };
        config.tun.enabled = true;
        let plain = without_tun(&config);
        assert!(!plain.tun.enabled);
        assert_eq!(plain.mixed_port, config.mixed_port);
        assert_eq!(plain.rules, config.rules);
        // The original is untouched: the fallback is a copy.
        assert!(config.tun.enabled);
    }
}
