//! Linux: the proxy settings of GNOME (through `gsettings`) and of KDE
//! (through `kwriteconfig5` or `kwriteconfig6` on `kioslaverc`). Each desktop
//! is changed only when its tool is present. No shell is run, and arguments
//! are passed as lists.

use std::process::Command;

use starpod_core::domain::SystemProxyMode;

use super::{Backend, Plan, SavedSetting, SysproxyError};

const GNOME_PROXY: &str = "org.gnome.system.proxy";
const GNOME_KEYS: [(&str, &str); 9] = [
    (GNOME_PROXY, "mode"),
    (GNOME_PROXY, "autoconfig-url"),
    (GNOME_PROXY, "ignore-hosts"),
    ("org.gnome.system.proxy.http", "host"),
    ("org.gnome.system.proxy.http", "port"),
    ("org.gnome.system.proxy.https", "host"),
    ("org.gnome.system.proxy.https", "port"),
    ("org.gnome.system.proxy.socks", "host"),
    ("org.gnome.system.proxy.socks", "port"),
];

const KDE_FILE: &str = "kioslaverc";
const KDE_GROUP: &str = "Proxy Settings";
const KDE_KEYS: [&str; 6] = [
    "ProxyType",
    "httpProxy",
    "httpsProxy",
    "socksProxy",
    "NoProxyFor",
    "Proxy Config Script",
];

/// Location prefixes, so a record says which desktop each value came from.
const GNOME_LOCATION: &str = "gsettings:";
const KDE_LOCATION: &str = "kioslaverc:";

pub fn backend() -> Box<dyn Backend> {
    Box::new(Linux)
}

struct Linux;

impl Backend for Linux {
    fn name(&self) -> &'static str {
        "linux-gsettings-kde"
    }

    fn capture(&self) -> Result<Vec<SavedSetting>, SysproxyError> {
        let mut saved = Vec::new();
        if gnome_available() {
            for (schema, key) in GNOME_KEYS {
                saved.push(SavedSetting {
                    location: format!("{GNOME_LOCATION}{schema}"),
                    key: key.to_string(),
                    value: gsettings_get(schema, key),
                });
            }
        }
        if let Some(tool) = kde_tool() {
            for key in KDE_KEYS {
                saved.push(SavedSetting {
                    location: format!("{KDE_LOCATION}{KDE_GROUP}"),
                    key: key.to_string(),
                    value: kreadconfig(tool, key),
                });
            }
        }
        Ok(saved)
    }

    fn apply(&self, plan: &Plan) -> Result<Vec<String>, SysproxyError> {
        let gnome = gnome_available();
        let kde = kde_tool();
        if !gnome && kde.is_none() {
            return Err(SysproxyError(
                "this desktop has no proxy setting Starpod can change (GNOME or KDE)".into(),
            ));
        }
        let mut warnings = Vec::new();
        if gnome {
            apply_gnome(plan)?;
        }
        if let Some(tool) = kde {
            apply_kde(tool, plan)?;
            if let Err(warning) = notify_kde() {
                warnings.push(warning);
            }
        }
        Ok(warnings)
    }

    fn restore(&self, saved: &[SavedSetting]) -> Result<Vec<String>, SysproxyError> {
        let mut warnings = Vec::new();
        let mut kde_restored = false;
        for setting in saved {
            if let Some(schema) = setting.location.strip_prefix(GNOME_LOCATION) {
                match &setting.value {
                    Some(value) => gsettings_set(schema, &setting.key, value)?,
                    None => gsettings_reset(schema, &setting.key)?,
                }
            } else if setting.location.starts_with(KDE_LOCATION)
                && let Some(tool) = kde_tool()
            {
                match &setting.value {
                    Some(value) => kwriteconfig(tool, &setting.key, value)?,
                    None => kdelete(tool, &setting.key)?,
                }
                kde_restored = true;
            }
        }
        if kde_restored && let Err(warning) = notify_kde() {
            warnings.push(warning);
        }
        Ok(warnings)
    }
}

fn apply_gnome(plan: &Plan) -> Result<(), SysproxyError> {
    let port = plan.port.to_string();
    let ignore = gvariant_string_list(&plan.bypass);
    gsettings_set(GNOME_PROXY, "ignore-hosts", &ignore)?;
    match (plan.mode, plan.pac_url.as_deref()) {
        (SystemProxyMode::Pac, Some(url)) => {
            gsettings_set(GNOME_PROXY, "mode", &gvariant_string("auto"))?;
            gsettings_set(GNOME_PROXY, "autoconfig-url", &gvariant_string(url))?;
        }
        _ => {
            gsettings_set(GNOME_PROXY, "mode", &gvariant_string("manual"))?;
            gsettings_set(GNOME_PROXY, "autoconfig-url", &gvariant_string(""))?;
            for schema in [
                "org.gnome.system.proxy.http",
                "org.gnome.system.proxy.https",
                "org.gnome.system.proxy.socks",
            ] {
                gsettings_set(schema, "host", &gvariant_string("127.0.0.1"))?;
                gsettings_set(schema, "port", &port)?;
            }
        }
    }
    Ok(())
}

fn apply_kde(tool: &str, plan: &Plan) -> Result<(), SysproxyError> {
    let port = plan.port.to_string();
    kwriteconfig(tool, "NoProxyFor", &plan.bypass.replace(';', ","))?;
    match (plan.mode, plan.pac_url.as_deref()) {
        (SystemProxyMode::Pac, Some(url)) => {
            kwriteconfig(tool, "ProxyType", "2")?;
            kwriteconfig(tool, "Proxy Config Script", url)?;
        }
        _ => {
            kwriteconfig(tool, "ProxyType", "1")?;
            kwriteconfig(tool, "httpProxy", &format!("http://127.0.0.1 {port}"))?;
            kwriteconfig(tool, "httpsProxy", &format!("http://127.0.0.1 {port}"))?;
            kwriteconfig(tool, "socksProxy", &format!("socks://127.0.0.1 {port}"))?;
        }
    }
    Ok(())
}

fn gnome_available() -> bool {
    gsettings_get(GNOME_PROXY, "mode").is_some()
}

/// `gsettings get`, as the raw GVariant text. `None` when the key cannot be read.
fn gsettings_get(schema: &str, key: &str) -> Option<String> {
    let output = Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn gsettings_set(schema: &str, key: &str, value: &str) -> Result<(), SysproxyError> {
    run("gsettings", &["set", schema, key, value]).map(|_| ())
}

fn gsettings_reset(schema: &str, key: &str) -> Result<(), SysproxyError> {
    run("gsettings", &["reset", schema, key]).map(|_| ())
}

/// The KDE tool to use: the newer `kwriteconfig6`, else `kwriteconfig5`.
fn kde_tool() -> Option<&'static str> {
    ["kwriteconfig6", "kwriteconfig5"]
        .into_iter()
        .find(|tool| Command::new(tool).arg("--help").output().is_ok())
}

/// The matching `kreadconfig` for a `kwriteconfig` tool.
fn reader_for(writer: &str) -> String {
    writer.replace("kwriteconfig", "kreadconfig")
}

fn kreadconfig(writer: &str, key: &str) -> Option<String> {
    let output = Command::new(reader_for(writer))
        .args(["--file", KDE_FILE, "--group", KDE_GROUP, "--key", key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn kwriteconfig(tool: &str, key: &str, value: &str) -> Result<(), SysproxyError> {
    run(
        tool,
        &[
            "--file", KDE_FILE, "--group", KDE_GROUP, "--key", key, value,
        ],
    )
    .map(|_| ())
}

fn kdelete(tool: &str, key: &str) -> Result<(), SysproxyError> {
    run(
        tool,
        &[
            "--file", KDE_FILE, "--group", KDE_GROUP, "--key", key, "--delete",
        ],
    )
    .map(|_| ())
}

/// Ask running KDE applications to reload their proxy settings. A failure is
/// a warning: the settings are written either way.
fn notify_kde() -> Result<(), String> {
    run(
        "dbus-send",
        &[
            "--session",
            "--type=signal",
            "/KIO/Scheduler",
            "org.kde.KIO.Scheduler.reparseSlaveConfiguration",
            "string:",
        ],
    )
    .map(|_| ())
    .map_err(|error| format!("KDE applications may need a restart to see the proxy: {error}"))
}

/// Run a tool with a list of arguments and return its standard output.
fn run(program: &str, args: &[&str]) -> Result<String, SysproxyError> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| SysproxyError(format!("{program} could not be run: {error}")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(SysproxyError(format!(
            "{program} failed: {}",
            detail.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A GVariant string literal, quoted and escaped.
fn gvariant_string(text: &str) -> String {
    format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
}

/// A GVariant array of strings, from a comma- or semicolon-separated list.
fn gvariant_string_list(bypass: &str) -> String {
    let items: Vec<String> = bypass
        .split([',', ';'])
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(gvariant_string)
        .collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvariant_strings_are_quoted_and_escaped() {
        assert_eq!(gvariant_string("manual"), "'manual'");
        assert_eq!(gvariant_string("it's"), "'it\\'s'");
    }

    #[test]
    fn the_bypass_list_becomes_a_gvariant_array() {
        assert_eq!(
            gvariant_string_list("localhost,127.0.0.1/8, ::1"),
            "['localhost', '127.0.0.1/8', '::1']"
        );
        assert_eq!(gvariant_string_list(""), "[]");
    }

    #[test]
    fn the_kde_reader_matches_its_writer() {
        assert_eq!(reader_for("kwriteconfig6"), "kreadconfig6");
        assert_eq!(reader_for("kwriteconfig5"), "kreadconfig5");
    }
}
