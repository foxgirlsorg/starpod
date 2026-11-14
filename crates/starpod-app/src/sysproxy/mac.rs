//! macOS: the proxy settings of every enabled network service, through
//! `networksetup`. Every call passes its arguments as a list, never through a
//! shell, so a service name or bypass entry cannot inject a command.

use std::collections::BTreeMap;
use std::process::Command;

use starpod_core::domain::SystemProxyMode;

use super::{Backend, Plan, SavedSetting, SysproxyError};

const TOOL: &str = "networksetup";
const LOOPBACK: &str = "127.0.0.1";

pub fn backend() -> Box<dyn Backend> {
    Box::new(MacOs)
}

struct MacOs;

impl Backend for MacOs {
    fn name(&self) -> &'static str {
        "macos-networksetup"
    }

    fn capture(&self) -> Result<Vec<SavedSetting>, SysproxyError> {
        let mut saved = Vec::new();
        for service in enabled_services()? {
            for (protocol, getter) in [
                ("web", "-getwebproxy"),
                ("secure", "-getsecurewebproxy"),
                ("socks", "-getsocksfirewallproxy"),
            ] {
                let text = run(&[getter, service.as_str()])?;
                let enabled = field(&text, "Enabled").map(|value| on_off(&value));
                let server = field(&text, "Server").unwrap_or_default();
                let port = field(&text, "Port").unwrap_or_default();
                let port = if port == "0" { String::new() } else { port };
                push(
                    &mut saved,
                    &service,
                    &format!("{protocol}.enabled"),
                    enabled,
                );
                push(
                    &mut saved,
                    &service,
                    &format!("{protocol}.server"),
                    Some(server),
                );
                push(
                    &mut saved,
                    &service,
                    &format!("{protocol}.port"),
                    Some(port),
                );
            }
            let bypass = run(&["-getproxybypassdomains", service.as_str()])?;
            let domains = if bypass.trim_start().starts_with("There aren't") {
                String::new()
            } else {
                bypass
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            push(&mut saved, &service, "bypass", Some(domains));

            let auto = run(&["-getautoproxyurl", service.as_str()])?;
            push(
                &mut saved,
                &service,
                "autoproxy.enabled",
                field(&auto, "Enabled").map(|value| on_off(&value)),
            );
            push(
                &mut saved,
                &service,
                "autoproxy.url",
                Some(field(&auto, "URL").unwrap_or_default()),
            );
        }
        Ok(saved)
    }

    fn apply(&self, plan: &Plan) -> Result<Vec<String>, SysproxyError> {
        let services = enabled_services()?;
        if services.is_empty() {
            return Err(SysproxyError(
                "no enabled network service was found to change".into(),
            ));
        }
        let port = plan.port.to_string();
        let domains: Vec<&str> = plan
            .bypass
            .split([',', ';'])
            .map(str::trim)
            .filter(|domain| !domain.is_empty())
            .collect();
        let mut warnings = Vec::new();
        for service in &services {
            let service = service.as_str();
            let mut steps: Vec<Vec<&str>> = Vec::new();
            match plan.mode {
                SystemProxyMode::Manual => {
                    steps.push(vec!["-setwebproxy", service, LOOPBACK, port.as_str()]);
                    steps.push(vec!["-setwebproxystate", service, "on"]);
                    steps.push(vec!["-setsecurewebproxy", service, LOOPBACK, port.as_str()]);
                    steps.push(vec!["-setsecurewebproxystate", service, "on"]);
                    steps.push(vec![
                        "-setsocksfirewallproxy",
                        service,
                        LOOPBACK,
                        port.as_str(),
                    ]);
                    steps.push(vec!["-setsocksfirewallproxystate", service, "on"]);
                    steps.push(vec!["-setautoproxystate", service, "off"]);
                }
                SystemProxyMode::Pac => {
                    steps.push(vec!["-setwebproxystate", service, "off"]);
                    steps.push(vec!["-setsecurewebproxystate", service, "off"]);
                    steps.push(vec!["-setsocksfirewallproxystate", service, "off"]);
                    if let Some(url) = plan.pac_url.as_deref() {
                        steps.push(vec!["-setautoproxyurl", service, url]);
                    }
                    steps.push(vec!["-setautoproxystate", service, "on"]);
                }
            }
            let mut bypass_step = vec!["-setproxybypassdomains", service];
            if domains.is_empty() {
                bypass_step.push("Empty");
            } else {
                bypass_step.extend(domains.iter().copied());
            }
            steps.push(bypass_step);

            for step in steps {
                if let Err(error) = run(&step) {
                    warnings.push(format!("{service}: {error}"));
                }
            }
        }
        Ok(warnings)
    }

    fn restore(&self, saved: &[SavedSetting]) -> Result<Vec<String>, SysproxyError> {
        let mut by_service: BTreeMap<&str, BTreeMap<&str, Option<&str>>> = BTreeMap::new();
        for setting in saved {
            by_service
                .entry(setting.location.as_str())
                .or_default()
                .insert(setting.key.as_str(), setting.value.as_deref());
        }
        let mut warnings = Vec::new();
        for (service, values) in by_service {
            let mut steps: Vec<Vec<String>> = Vec::new();
            for (protocol, setter, state) in [
                ("web", "-setwebproxy", "-setwebproxystate"),
                ("secure", "-setsecurewebproxy", "-setsecurewebproxystate"),
                (
                    "socks",
                    "-setsocksfirewallproxy",
                    "-setsocksfirewallproxystate",
                ),
            ] {
                let server = values
                    .get(format!("{protocol}.server").as_str())
                    .copied()
                    .flatten()
                    .unwrap_or("");
                let port = values
                    .get(format!("{protocol}.port").as_str())
                    .copied()
                    .flatten()
                    .unwrap_or("");
                if !server.is_empty() && !port.is_empty() {
                    steps.push(strings(&[setter, service, server, port]));
                }
                let enabled = values
                    .get(format!("{protocol}.enabled").as_str())
                    .copied()
                    .flatten();
                steps.push(strings(&[
                    state,
                    service,
                    if enabled == Some("on") { "on" } else { "off" },
                ]));
            }
            let domains = values.get("bypass").copied().flatten().unwrap_or("");
            let mut bypass = vec!["-setproxybypassdomains".to_string(), service.to_string()];
            let list: Vec<&str> = domains
                .split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .collect();
            if list.is_empty() {
                bypass.push("Empty".into());
            } else {
                bypass.extend(list.iter().map(|d| (*d).to_string()));
            }
            steps.push(bypass);
            let url = values.get("autoproxy.url").copied().flatten().unwrap_or("");
            if !url.is_empty() {
                steps.push(strings(&["-setautoproxyurl", service, url]));
            }
            let auto = values.get("autoproxy.enabled").copied().flatten();
            steps.push(strings(&[
                "-setautoproxystate",
                service,
                if auto == Some("on") { "on" } else { "off" },
            ]));

            for step in steps {
                let args: Vec<&str> = step.iter().map(String::as_str).collect();
                if let Err(error) = run(&args) {
                    warnings.push(format!("{service}: {error}"));
                }
            }
        }
        Ok(warnings)
    }
}

/// Run `networksetup` with `args` and return its standard output.
fn run(args: &[&str]) -> Result<String, SysproxyError> {
    let output = Command::new(TOOL)
        .args(args)
        .output()
        .map_err(|error| SysproxyError(format!("networksetup could not be run: {error}")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(SysproxyError(format!(
            "networksetup {} failed: {}",
            args.first().copied().unwrap_or(""),
            detail.trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The names of the enabled network services. The first line of the listing
/// is a note, and a service marked with `*` is disabled.
fn enabled_services() -> Result<Vec<String>, SysproxyError> {
    let listing = run(&["-listallnetworkservices"])?;
    Ok(listing
        .lines()
        .skip(1)
        .filter(|line| !line.starts_with('*') && !line.trim().is_empty())
        .map(|line| line.trim().to_string())
        .collect())
}

/// The value after `label:` on the line that starts with it.
fn field(text: &str, label: &str) -> Option<String> {
    let prefix = format!("{label}:");
    text.lines()
        .find_map(|line| line.trim().strip_prefix(&prefix))
        .map(|value| value.trim().to_string())
}

fn on_off(value: &str) -> String {
    if value.eq_ignore_ascii_case("yes") || value.eq_ignore_ascii_case("on") {
        "on".into()
    } else {
        "off".into()
    }
}

fn push(saved: &mut Vec<SavedSetting>, service: &str, key: &str, value: Option<String>) {
    saved.push(SavedSetting {
        location: service.to_string(),
        key: key.to_string(),
        value,
    });
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}
