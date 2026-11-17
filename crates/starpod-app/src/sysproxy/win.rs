//! Windows: the per-user proxy settings in the registry, and the WinInet
//! notification that makes a change take effect without a restart.
//!
//! The per-user `Run` value that starts Starpod at logon is also here, since
//! it is the same kind of registry access.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr::{null, null_mut};

use starpod_core::domain::SystemProxyMode;
use windows_sys::Win32::Foundation::{
    ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, GetLastError, GlobalFree, WIN32_ERROR,
};
use windows_sys::Win32::Networking::WinInet::{
    INTERNET_OPTION_PER_CONNECTION_OPTION, INTERNET_PER_CONN, INTERNET_PER_CONN_AUTOCONFIG_URL,
    INTERNET_PER_CONN_FLAGS, INTERNET_PER_CONN_OPTION_LISTW, INTERNET_PER_CONN_OPTIONW,
    INTERNET_PER_CONN_OPTIONW_0, INTERNET_PER_CONN_PROXY_BYPASS, INTERNET_PER_CONN_PROXY_SERVER,
    InternetQueryOptionW, InternetSetOptionW, PROXY_TYPE_AUTO_PROXY_URL, PROXY_TYPE_DIRECT,
    PROXY_TYPE_PROXY,
};
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

/// Every registry value Starpod reads, changes and restores.
const CAPTURED: [&str; 4] = [PROXY_ENABLE, PROXY_SERVER, PROXY_OVERRIDE, AUTO_CONFIG_URL];

/// The saved-record location of the LAN connection's settings. They live in
/// WinINet, not in the registry values above, which WinINet readers ignore.
const LAN_LOCATION: &str = "WinINet LAN";
const LAN_FLAGS: &str = "Flags";
const LAN_SERVER: &str = "ProxyServer";
const LAN_BYPASS: &str = "ProxyBypass";
const LAN_AUTOCONFIG: &str = "AutoConfigURL";

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
        let mut settings: Vec<SavedSetting> = CAPTURED
            .iter()
            .map(|name| {
                Ok(SavedSetting {
                    location: INTERNET_SETTINGS.to_string(),
                    key: (*name).to_string(),
                    value: read(INTERNET_SETTINGS, name)?,
                })
            })
            .collect::<Result<_, SysproxyError>>()?;
        settings.extend(query_lan()?.to_settings());
        Ok(settings)
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
        set_lan(&lan_for(plan))?;
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
        if let Some(lan) = Lan::from_settings(saved)? {
            set_lan(&lan)?;
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

/// The LAN connection's proxy settings, as WinINet keeps them. These are what
/// Edge, Chrome and most other Windows programs read.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lan {
    /// `PROXY_TYPE_*` bits.
    flags: u32,
    server: String,
    bypass: String,
    autoconfig: String,
}

impl Lan {
    fn to_settings(&self) -> Vec<SavedSetting> {
        [
            (LAN_FLAGS, self.flags.to_string()),
            (LAN_SERVER, self.server.clone()),
            (LAN_BYPASS, self.bypass.clone()),
            (LAN_AUTOCONFIG, self.autoconfig.clone()),
        ]
        .into_iter()
        .map(|(key, value)| SavedSetting {
            location: LAN_LOCATION.to_string(),
            key: key.to_string(),
            value: Some(value),
        })
        .collect()
    }

    /// The LAN settings a saved record holds. `None` when it holds none.
    fn from_settings(saved: &[SavedSetting]) -> Result<Option<Self>, SysproxyError> {
        let find = |key: &str| {
            saved
                .iter()
                .find(|setting| setting.location == LAN_LOCATION && setting.key == key)
                .map(|setting| setting.value.clone().unwrap_or_default())
        };
        let Some(flags) = find(LAN_FLAGS) else {
            return Ok(None);
        };
        let flags = flags
            .parse::<u32>()
            .map_err(|_| SysproxyError("the saved LAN proxy flags are not a number".into()))?;
        Ok(Some(Self {
            flags,
            server: find(LAN_SERVER).unwrap_or_default(),
            bypass: find(LAN_BYPASS).unwrap_or_default(),
            autoconfig: find(LAN_AUTOCONFIG).unwrap_or_default(),
        }))
    }
}

/// The LAN settings a plan asks for: a fixed server, or a PAC script.
fn lan_for(plan: &Plan) -> Lan {
    let (flags, autoconfig) = match (plan.mode, plan.pac_url.as_deref()) {
        (SystemProxyMode::Pac, Some(url)) => (PROXY_TYPE_DIRECT | PROXY_TYPE_AUTO_PROXY_URL, url),
        _ => (PROXY_TYPE_DIRECT | PROXY_TYPE_PROXY, ""),
    };
    Lan {
        flags,
        server: format!("127.0.0.1:{}", plan.port),
        bypass: plan.bypass.clone(),
        autoconfig: autoconfig.to_string(),
    }
}

/// One LAN option to query. The value is filled in by WinINet.
fn lan_option(option: INTERNET_PER_CONN) -> INTERNET_PER_CONN_OPTIONW {
    INTERNET_PER_CONN_OPTIONW {
        dwOption: option,
        Value: INTERNET_PER_CONN_OPTIONW_0 { dwValue: 0 },
    }
}

/// Read the LAN connection's proxy settings. A null connection name means the
/// LAN, which is the connection the system proxy belongs to.
fn query_lan() -> Result<Lan, SysproxyError> {
    let mut options = [
        lan_option(INTERNET_PER_CONN_FLAGS),
        lan_option(INTERNET_PER_CONN_PROXY_SERVER),
        lan_option(INTERNET_PER_CONN_PROXY_BYPASS),
        lan_option(INTERNET_PER_CONN_AUTOCONFIG_URL),
    ];
    let mut list = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        pszConnection: null_mut(),
        dwOptionCount: options.len() as u32,
        dwOptionError: 0,
        pOptions: options.as_mut_ptr(),
    };
    let mut size = list.dwSize;
    // SAFETY: `list`, the options it points to and `size` all live through the
    // call. WinINet writes the options' values and allocates their strings.
    let ok = unsafe {
        InternetQueryOptionW(
            null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            (&mut list as *mut INTERNET_PER_CONN_OPTION_LISTW).cast::<c_void>(),
            &mut size,
        )
    };
    if ok == 0 {
        return Err(last_error("the LAN proxy settings could not be read"));
    }
    // SAFETY: after a successful query each string is null or WinINet-allocated,
    // and `take_string` frees each one exactly once.
    let lan = unsafe {
        Lan {
            flags: options[0].Value.dwValue,
            server: take_string(options[1].Value.pszValue),
            bypass: take_string(options[2].Value.pszValue),
            autoconfig: take_string(options[3].Value.pszValue),
        }
    };
    Ok(lan)
}

/// Write the LAN connection's proxy settings. WinINet readers pick them up
/// after the change notification that follows.
fn set_lan(lan: &Lan) -> Result<(), SysproxyError> {
    let mut server = wide(&lan.server);
    let mut bypass = wide(&lan.bypass);
    let mut autoconfig = wide(&lan.autoconfig);
    let mut options = [
        INTERNET_PER_CONN_OPTIONW {
            dwOption: INTERNET_PER_CONN_FLAGS,
            Value: INTERNET_PER_CONN_OPTIONW_0 { dwValue: lan.flags },
        },
        INTERNET_PER_CONN_OPTIONW {
            dwOption: INTERNET_PER_CONN_PROXY_SERVER,
            Value: INTERNET_PER_CONN_OPTIONW_0 {
                pszValue: server.as_mut_ptr(),
            },
        },
        INTERNET_PER_CONN_OPTIONW {
            dwOption: INTERNET_PER_CONN_PROXY_BYPASS,
            Value: INTERNET_PER_CONN_OPTIONW_0 {
                pszValue: bypass.as_mut_ptr(),
            },
        },
        INTERNET_PER_CONN_OPTIONW {
            dwOption: INTERNET_PER_CONN_AUTOCONFIG_URL,
            Value: INTERNET_PER_CONN_OPTIONW_0 {
                pszValue: autoconfig.as_mut_ptr(),
            },
        },
    ];
    let list = INTERNET_PER_CONN_OPTION_LISTW {
        dwSize: size_of::<INTERNET_PER_CONN_OPTION_LISTW>() as u32,
        pszConnection: null_mut(),
        dwOptionCount: options.len() as u32,
        dwOptionError: 0,
        pOptions: options.as_mut_ptr(),
    };
    // SAFETY: the list, its options and the three NUL-terminated strings all
    // live through the call, and `dwSize` is the list's size.
    let ok = unsafe {
        InternetSetOptionW(
            null(),
            INTERNET_OPTION_PER_CONNECTION_OPTION,
            (&list as *const INTERNET_PER_CONN_OPTION_LISTW).cast::<c_void>(),
            list.dwSize,
        )
    };
    if ok == 0 {
        return Err(last_error("the LAN proxy settings could not be written"));
    }
    Ok(())
}

/// Copy a string WinINet allocated, then free it. A null pointer is "".
///
/// # Safety
///
/// `text` is null, or a NUL-terminated string that WinINet allocated with
/// `GlobalAlloc` and that nothing else frees.
unsafe fn take_string(text: windows_sys::core::PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: `text` is NUL-terminated, so the loop stops within the string.
    let units = unsafe {
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        std::slice::from_raw_parts(text, length)
    };
    let value = String::from_utf16_lossy(units);
    // SAFETY: the string was allocated by WinINet with GlobalAlloc, and is
    // freed here exactly once.
    unsafe {
        GlobalFree(text.cast::<c_void>());
    }
    value
}

/// The error WinINet or the registry left behind, for a message.
fn last_error(action: &str) -> SysproxyError {
    // SAFETY: GetLastError has no preconditions.
    let code = unsafe { GetLastError() };
    SysproxyError(format!("{action} (error {code})"))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Puts the saved settings back when dropped, even if the test panics.
    struct Restore(Vec<SavedSetting>);

    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = Windows.restore(&self.0);
        }
    }

    #[test]
    fn lan_settings_survive_the_record() {
        let lan = Lan {
            flags: PROXY_TYPE_DIRECT | PROXY_TYPE_PROXY,
            server: "127.0.0.1:7890".into(),
            bypass: "<local>".into(),
            autoconfig: String::new(),
        };
        assert_eq!(Lan::from_settings(&lan.to_settings()).unwrap(), Some(lan));
    }

    /// Changes the real per-user proxy for a moment, then restores it. Run by
    /// hand: `cargo test -p starpod-app live_manual_proxy_round_trip -- --ignored --nocapture`.
    #[test]
    #[ignore = "changes the real system proxy; run by hand"]
    fn live_manual_proxy_round_trip() {
        let before = Windows.capture().unwrap();
        println!("before, WinINet: {:?}", query_lan().unwrap());
        println!(
            "before, registry ProxyEnable: {:?}",
            read(INTERNET_SETTINGS, PROXY_ENABLE).unwrap()
        );
        {
            let _restore = Restore(before.clone());
            let plan = Plan {
                mode: SystemProxyMode::Manual,
                port: 7890,
                bypass: "<local>".into(),
                pac_url: None,
            };
            let warnings = Windows.apply(&plan).unwrap();
            println!("applied, warnings: {warnings:?}");
            println!("applied, WinINet: {:?}", query_lan().unwrap());
            println!(
                "applied, registry ProxyEnable: {:?}",
                read(INTERNET_SETTINGS, PROXY_ENABLE).unwrap()
            );
        }
        let after = query_lan().unwrap();
        println!("restored, WinINet: {after:?}");
        println!(
            "restored, registry ProxyEnable: {:?}",
            read(INTERNET_SETTINGS, PROXY_ENABLE).unwrap()
        );
        assert_eq!(Lan::from_settings(&before).unwrap(), Some(after));
    }
}
