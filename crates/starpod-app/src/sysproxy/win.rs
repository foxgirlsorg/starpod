//! Windows: the per-user proxy settings in the registry, and the WinInet
//! notification that makes a change take effect without a restart.
//!
//! The per-user `Run` value that starts Starpod at logon is also here, since
//! it is the same kind of registry access.

use std::ptr::{null, null_mut};

use starpod_core::domain::SystemProxyMode;
use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows_sys::Win32::Networking::WinInet::InternetSetOptionW;
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};

use super::{Backend, Plan, SavedSetting, SysproxyError};

const INTERNET_SETTINGS: &str = r"Software\Microsoft\Windows\CurrentVersion\Internet Settings";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Starpod";
const PROXY_ENABLE: &str = "ProxyEnable";
const PROXY_SERVER: &str = "ProxyServer";
const PROXY_OVERRIDE: &str = "ProxyOverride";
const AUTO_CONFIG_URL: &str = "AutoConfigURL";

/// Every value Starpod reads, changes and restores.
const CAPTURED: [&str; 4] = [PROXY_ENABLE, PROXY_SERVER, PROXY_OVERRIDE, AUTO_CONFIG_URL];

/// `INTERNET_OPTION_SETTINGS_CHANGED`: other programs re-read the settings.
const INTERNET_OPTION_SETTINGS_CHANGED: u32 = 39;
/// `INTERNET_OPTION_REFRESH`: the settings are reloaded now.
const INTERNET_OPTION_REFRESH: u32 = 37;

pub fn backend() -> Box<dyn Backend> {
    Box::new(Windows)
}

struct Windows;

impl Backend for Windows {
    fn name(&self) -> &'static str {
        "windows-registry"
    }

    fn capture(&self) -> Result<Vec<SavedSetting>, SysproxyError> {
        CAPTURED
            .iter()
            .map(|name| {
                Ok(SavedSetting {
                    location: INTERNET_SETTINGS.to_string(),
                    key: (*name).to_string(),
                    value: read(INTERNET_SETTINGS, name)?,
                })
            })
            .collect()
    }

    fn apply(&self, plan: &Plan) -> Result<Vec<String>, SysproxyError> {
        write_string(
            INTERNET_SETTINGS,
            PROXY_SERVER,
            &format!("127.0.0.1:{}", plan.port),
        )?;
        write_string(INTERNET_SETTINGS, PROXY_OVERRIDE, &plan.bypass)?;
        match (plan.mode, plan.pac_url.as_deref()) {
            (SystemProxyMode::Pac, Some(url)) => {
                write_dword(PROXY_ENABLE, 0)?;
                write_string(INTERNET_SETTINGS, AUTO_CONFIG_URL, url)?;
            }
            _ => {
                write_dword(PROXY_ENABLE, 1)?;
                delete(INTERNET_SETTINGS, AUTO_CONFIG_URL)?;
            }
        }
        Ok(notify())
    }

    fn restore(&self, saved: &[SavedSetting]) -> Result<Vec<String>, SysproxyError> {
        for setting in saved
            .iter()
            .filter(|setting| setting.location == INTERNET_SETTINGS)
        {
            match (&setting.value, setting.key.as_str()) {
                (None, name) => delete(INTERNET_SETTINGS, name)?,
                (Some(value), PROXY_ENABLE) => {
                    let number: u32 = value.parse().map_err(|_| {
                        SysproxyError(format!("the saved {PROXY_ENABLE} is not a number"))
                    })?;
                    write_dword(PROXY_ENABLE, number)?;
                }
                (Some(value), name) => write_string(INTERNET_SETTINGS, name, value)?,
            }
        }
        Ok(notify())
    }
}

/// Set or remove the per-user Run value. `Some` holds the full command line.
pub fn set_run_value(command: Option<&str>) -> Result<(), SysproxyError> {
    match command {
        Some(command) => write(RUN_KEY, RUN_VALUE, REG_SZ, &utf16_bytes(command)),
        None => delete(RUN_KEY, RUN_VALUE),
    }
}

/// Tell the system the settings changed, so WinINet reloads them.
fn notify() -> Vec<String> {
    [INTERNET_OPTION_SETTINGS_CHANGED, INTERNET_OPTION_REFRESH]
        .into_iter()
        .filter(|option| {
            // SAFETY: a null handle with a null buffer is the documented way to
            // notify the process-wide settings.
            let ok = unsafe { InternetSetOptionW(null(), *option, null(), 0) };
            ok == 0
        })
        .map(|option| {
            format!("Windows did not confirm the proxy change (option {option}); it may apply after a restart of the other program")
        })
        .collect()
}

/// An open registry key, closed when dropped.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the handle came from a successful RegOpenKeyExW and is closed
        // exactly once, here.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

/// Open a key under HKEY_CURRENT_USER. `None` when it does not exist.
fn open(subkey: &str, access: u32) -> Result<Option<Key>, SysproxyError> {
    let path = wide(subkey);
    let mut key: HKEY = null_mut();
    // SAFETY: `path` is a NUL-terminated UTF-16 string that outlives the call,
    // and `key` is a valid out-pointer.
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut key) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(status, "opened")?;
    Ok(Some(Key(key)))
}

/// Read a value as text. A DWORD is returned in decimal. `None` when the
/// value does not exist.
fn read(subkey: &str, name: &str) -> Result<Option<String>, SysproxyError> {
    let Some(key) = open(subkey, KEY_READ)? else {
        return Ok(None);
    };
    let name = wide(name);
    let mut kind: u32 = 0;
    let mut size: u32 = 0;
    // SAFETY: a null data pointer asks only for the size; `kind` and `size`
    // are valid out-pointers.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            null(),
            &mut kind,
            null_mut(),
            &mut size,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    check(status, "read")?;
    let mut data = vec![0u8; size as usize];
    // SAFETY: `data` holds `size` bytes, which is what the first call reported.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            null(),
            &mut kind,
            data.as_mut_ptr(),
            &mut size,
        )
    };
    check(status, "read")?;
    data.truncate(size as usize);
    match kind {
        REG_DWORD => {
            let bytes: [u8; 4] = data
                .get(..4)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| SysproxyError("a saved DWORD is too short".into()))?;
            Ok(Some(u32::from_le_bytes(bytes).to_string()))
        }
        REG_SZ => {
            let units: Vec<u16> = data
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            Ok(Some(
                String::from_utf16_lossy(&units)
                    .trim_end_matches('\0')
                    .to_string(),
            ))
        }
        other => Err(SysproxyError(format!(
            "the registry value {name_text} has type {other}, which Starpod does not change",
            name_text = String::from_utf16_lossy(&name[..name.len() - 1])
        ))),
    }
}

fn write(subkey: &str, name: &str, kind: u32, data: &[u8]) -> Result<(), SysproxyError> {
    let key = open(subkey, KEY_READ | KEY_WRITE)?
        .ok_or_else(|| SysproxyError(format!("the registry key {subkey} does not exist")))?;
    let name = wide(name);
    // SAFETY: `name` is NUL-terminated and `data` is valid for `data.len()` bytes.
    let status = unsafe {
        RegSetValueExW(
            key.0,
            name.as_ptr(),
            0,
            kind,
            data.as_ptr(),
            data.len() as u32,
        )
    };
    check(status, "written")
}

fn write_string(subkey: &str, name: &str, value: &str) -> Result<(), SysproxyError> {
    write(subkey, name, REG_SZ, &utf16_bytes(value))
}

fn write_dword(name: &str, value: u32) -> Result<(), SysproxyError> {
    write(INTERNET_SETTINGS, name, REG_DWORD, &value.to_le_bytes())
}

/// Remove a value. A missing key or value is not an error.
fn delete(subkey: &str, name: &str) -> Result<(), SysproxyError> {
    let Some(key) = open(subkey, KEY_WRITE)? else {
        return Ok(());
    };
    let name = wide(name);
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let status = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(());
    }
    check(status, "removed")
}

fn check(status: WIN32_ERROR, action: &str) -> Result<(), SysproxyError> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(SysproxyError(format!(
            "the registry value could not be {action} (error {status})"
        )))
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The bytes of a NUL-terminated UTF-16 string, as `REG_SZ` stores them.
fn utf16_bytes(text: &str) -> Vec<u8> {
    wide(text)
        .iter()
        .flat_map(|unit| unit.to_le_bytes())
        .collect()
}
