//! This device's identity, as subscription providers see it.
//!
//! Providers that limit devices read four request headers: `x-hwid` (a stable
//! anonymous id), `x-device-os`, `x-ver-os` and `x-device-model`. This module
//! computes them. The subscription crate only sends them, so the platform
//! probing lives here, in the app.
//!
//! # The HWID
//!
//! `x-hwid` is the lowercase hex of `SHA-256("starpod-hwid:" + machine id)`,
//! cut to its first 32 hex characters. The machine id is:
//!
//! - Windows: `HKLM\SOFTWARE\Microsoft\Cryptography\MachineGuid`;
//! - Linux: `/etc/machine-id`, else `/var/lib/dbus/machine-id`;
//! - macOS: `IOPlatformUUID` from `ioreg -rd1 -c IOPlatformExpertDevice`.
//!
//! When none of those can be read, a random id is generated once and kept in
//! the app's data directory, so the HWID stays the same across runs.
//!
//! The hash means the raw machine id never leaves the device. The prefix keeps
//! the HWID from being the same hash a different tool would compute of the same
//! machine id.
//!
//! The OS version and model are best effort. Anything that cannot be read is
//! replaced by a fallback, so a header is never empty.

use std::path::Path;

use sha2::{Digest, Sha256};
use starpod_subscription::DeviceHeaders;

/// The prefix hashed together with the machine id.
const HWID_PREFIX: &str = "starpod-hwid:";

/// The longest header value the app sends, in characters.
const HEADER_MAX_CHARS: usize = 128;

/// This device, as the provider headers describe it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceProfile {
    /// The `x-hwid` value: 32 lowercase hex characters.
    pub hwid: String,
    /// The `x-device-os` value: `Windows`, `Linux` or `macOS`.
    pub os: String,
    /// The `x-ver-os` value.
    pub os_version: String,
    /// The `x-device-model` value.
    pub model: String,
}

impl DeviceProfile {
    /// Probe this device. `device_id_file` is where the fallback id is kept
    /// when the platform has no machine id.
    pub fn detect(device_id_file: &Path) -> Self {
        let machine_id = machine_id().unwrap_or_else(|| stored_fallback_id(device_id_file));
        Self {
            hwid: hwid_from_machine_id(&machine_id),
            os: os_label().to_string(),
            os_version: header_safe(&os_version(), std::env::consts::OS),
            model: header_safe(&model(), std::env::consts::ARCH),
        }
    }

    /// The headers to send, every value made safe for an HTTP header.
    pub fn headers(&self) -> DeviceHeaders {
        DeviceHeaders {
            hwid: header_safe(&self.hwid, "unknown"),
            os: header_safe(&self.os, "Unknown"),
            os_version: header_safe(&self.os_version, std::env::consts::OS),
            model: header_safe(&self.model, std::env::consts::ARCH),
        }
    }
}

/// The HWID for a machine id: 32 lowercase hex characters.
pub fn hwid_from_machine_id(machine_id: &str) -> String {
    let digest = Sha256::digest(format!("{HWID_PREFIX}{machine_id}").as_bytes());
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()[..32]
        .to_string()
}

/// The OS name a provider's panel expects.
fn os_label() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Windows",
        "linux" => "Linux",
        "macos" => "macOS",
        _ => "Unknown",
    }
}

/// A value safe to put in an HTTP header: printable ASCII only, trimmed,
/// capped in length. A value with nothing left is replaced by `fallback`.
pub fn header_safe(text: &str, fallback: &str) -> String {
    let cleaned: String = text
        .chars()
        .filter(|character| (' '..='~').contains(character))
        .take(HEADER_MAX_CHARS)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        fallback.to_string()
    } else {
        cleaned.to_string()
    }
}

/// The fallback id, generated on first use and kept in `path`.
///
/// A file that cannot be read or written does not fail the probe: the id is
/// then generated fresh, so the HWID changes between runs. That is the same
/// result as having no machine id at all, and it is never an empty value.
fn stored_fallback_id(path: &Path) -> String {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let existing = existing.trim();
        if !existing.is_empty() {
            return existing.to_string();
        }
    }
    let fresh = uuid::Uuid::new_v4().simple().to_string();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, &fresh);
    fresh
}

/// The platform's machine id, if it has one the app can read.
fn machine_id() -> Option<String> {
    #[cfg(windows)]
    {
        registry_string(r"SOFTWARE\Microsoft\Cryptography", "MachineGuid")
    }
    #[cfg(target_os = "linux")]
    {
        ["/etc/machine-id", "/var/lib/dbus/machine-id"]
            .iter()
            .find_map(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty())
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.lines()
            .find(|line| line.contains("IOPlatformUUID"))
            .and_then(|line| line.split('"').nth(3))
            .map(str::to_string)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The OS version, best effort.
fn os_version() -> String {
    #[cfg(windows)]
    {
        // Windows 10 and 11 share the NT version 10.0; the build number says
        // which release it is.
        registry_string(
            r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
            "CurrentBuild",
        )
        .map(|build| format!("10.0.{build}"))
        .unwrap_or_default()
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("PRETTY_NAME="))
                    .map(|name| name.trim_matches('"').to_string())
            })
            .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        command_output("sw_vers", &["-productVersion"]).unwrap_or_default()
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        String::new()
    }
}

/// The device model, best effort. The CPU architecture when the model is not
/// readable, which is what the caller falls back to.
fn model() -> String {
    #[cfg(windows)]
    {
        registry_string(r"HARDWARE\DESCRIPTION\System\BIOS", "SystemProductName")
            .unwrap_or_default()
    }
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/sys/devices/virtual/dmi/id/product_name")
            .map(|text| text.trim().to_string())
            .unwrap_or_default()
    }
    #[cfg(target_os = "macos")]
    {
        command_output("sysctl", &["-n", "hw.model"]).unwrap_or_default()
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        String::new()
    }
}

/// The trimmed standard output of a program run without a shell.
#[cfg(target_os = "macos")]
fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// A string value under `HKEY_LOCAL_MACHINE`. `None` when the key or value is
/// missing or is not a string.
#[cfg(windows)]
fn registry_string(subkey: &str, value: &str) -> Option<String> {
    use winreg::RegKey;
    use winreg::enums::HKEY_LOCAL_MACHINE;

    let text: String = RegKey::predef(HKEY_LOCAL_MACHINE)
        .open_subkey(subkey)
        .ok()?
        .get_value(value)
        .ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hwid_is_32_lowercase_hex_characters() {
        let hwid = hwid_from_machine_id("a1b2c3d4-machine");
        assert_eq!(hwid.len(), 32);
        assert!(
            hwid.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
            "{hwid}"
        );
    }

    #[test]
    fn the_hwid_is_stable_for_the_same_machine_id() {
        assert_eq!(
            hwid_from_machine_id("machine-1"),
            hwid_from_machine_id("machine-1")
        );
    }

    #[test]
    fn different_machines_get_different_hwids() {
        assert_ne!(
            hwid_from_machine_id("machine-1"),
            hwid_from_machine_id("machine-2")
        );
    }

    #[test]
    fn the_hwid_is_not_the_raw_machine_id() {
        let machine = "9f8e7d6c-5b4a-3210-fedc-ba9876543210";
        assert!(!hwid_from_machine_id(machine).contains(&machine[..8]));
    }

    #[test]
    fn a_header_value_keeps_printable_ascii_only() {
        assert_eq!(
            header_safe("  Intel(R) Core\u{0}\r\n™  ", "x"),
            "Intel(R) Core"
        );
        assert_eq!(header_safe("\u{7f}\u{1}", "fallback"), "fallback");
        assert_eq!(header_safe("", "x86_64"), "x86_64");
    }

    #[test]
    fn a_header_value_is_capped_in_length() {
        let long = "a".repeat(500);
        assert_eq!(header_safe(&long, "x").len(), HEADER_MAX_CHARS);
    }

    #[test]
    fn the_fallback_id_is_generated_once_and_then_reused() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("device-id");
        let first = stored_fallback_id(&path);
        let second = stored_fallback_id(&path);
        assert_eq!(first, second, "the id survives a second probe");
        assert!(!first.is_empty());
    }

    #[test]
    fn detect_yields_a_complete_profile_with_no_empty_header() {
        let dir = tempfile::TempDir::new().unwrap();
        let profile = DeviceProfile::detect(&dir.path().join("device-id"));
        assert_eq!(profile.hwid.len(), 32);
        let headers = profile.headers();
        for value in [
            &headers.hwid,
            &headers.os,
            &headers.os_version,
            &headers.model,
        ] {
            assert!(!value.is_empty());
            assert!(value.bytes().all(|byte| (0x20..0x7f).contains(&byte)));
        }
    }
}
