//! The engine settings beyond the core routing ones: their validation and the
//! DNS precedence.
//!
//! Precedence, in the same model as the rest of the pipeline:
//!
//! | Setting | Precedence |
//! |---|---|
//! | every `EngineOptions` field except `dns` | user > default (not written) |
//! | `dns` | when the user turns on the DNS override, the user's block; otherwise the first subscription's `dns:` block; otherwise the user's block |
//!
//! Every field is `Option` or empty, so an unset field is never written and
//! the generated configuration is unchanged from before these settings
//! existed.

use std::net::IpAddr;

use crate::domain::{
    DnsSettings, EngineOptions, Finding, Severity, SubscriptionPayload, UserPreferences,
};

/// The sniffer protocols the engine accepts as `sniff:` keys.
const SNIFF_PROTOCOLS: &[&str] = &["HTTP", "TLS", "QUIC"];

/// The fewest milliseconds a latency probe may be given.
const MIN_DELAY_TIMEOUT_MS: u32 = 100;
/// The most milliseconds a latency probe may be given.
const MAX_DELAY_TIMEOUT_MS: u32 = 60_000;
/// The fewest log lines the app may keep.
const MIN_LOG_RETENTION: u32 = 100;
/// The most log lines the app may keep. The engine's log buffer holds this
/// many, so a larger setting could not be honored.
const MAX_LOG_RETENTION: u32 = 2_000;
/// The longest keep-alive interval, in seconds (one day).
const MAX_KEEP_ALIVE_SECS: u32 = 86_400;
/// The longest geodata refresh interval, in hours (one year).
const MAX_GEO_INTERVAL_HOURS: u32 = 8_760;

fn error(message: impl Into<String>, subject: Option<&str>) -> Finding {
    Finding {
        severity: Severity::Error,
        message: message.into(),
        subject: subject.map(str::to_string),
    }
}

/// Check the app's own engine settings. Problems are errors, so the candidate
/// is refused and nothing is applied.
pub(crate) fn validate_preferences(prefs: &UserPreferences, findings: &mut Vec<Finding>) {
    // The DNS hijack list is `host:port` entries, such as `any:53`.
    for target in prefs.tun.dns_hijack.iter().flatten() {
        let port_ok = target
            .rsplit_once(':')
            .is_some_and(|(host, port)| !host.is_empty() && port.parse::<u16>().is_ok());
        if !port_ok {
            findings.push(error(
                format!("“{target}” is not a DNS hijack target (use host:port, such as any:53)"),
                Some(target),
            ));
        }
    }
    if let Some(url) = &prefs.delay_test_url {
        let scheme_ok = url.starts_with("http://") || url.starts_with("https://");
        if !scheme_ok || url.chars().any(char::is_whitespace) {
            findings.push(error(
                "the latency-test address must be an http or https address with no spaces",
                Some(url),
            ));
        }
    }
    if let Some(timeout) = prefs.delay_test_timeout_ms
        && !(MIN_DELAY_TIMEOUT_MS..=MAX_DELAY_TIMEOUT_MS).contains(&timeout)
    {
        findings.push(error(
            format!(
                "the latency-test timeout must be between {MIN_DELAY_TIMEOUT_MS} and {MAX_DELAY_TIMEOUT_MS} milliseconds"
            ),
            None,
        ));
    }
    if let Some(lines) = prefs.log_retention_lines
        && !(MIN_LOG_RETENTION..=MAX_LOG_RETENTION).contains(&lines)
    {
        findings.push(error(
            format!(
                "the number of log lines kept must be between {MIN_LOG_RETENTION} and {MAX_LOG_RETENTION}"
            ),
            None,
        ));
    }
    validate_engine_options(&prefs.engine, findings);
}

/// Check the engine options. Shared by the preferences check and the tests.
fn validate_engine_options(options: &EngineOptions, findings: &mut Vec<Finding>) {
    for cidr in &options.tun.route_address {
        if !is_cidr(cidr) {
            findings.push(error(
                format!("“{cidr}” is not a valid route address (use an IP address with a prefix length, such as 10.0.0.0/8)"),
                Some(cidr),
            ));
        }
    }
    for cidr in &options.tun.route_exclude_address {
        if !is_cidr(cidr) {
            findings.push(error(
                format!("“{cidr}” is not a valid excluded address (use an IP address with a prefix length, such as 192.168.0.0/16)"),
                Some(cidr),
            ));
        }
    }
    if let Some(seconds) = options.keep_alive_interval
        && !(1..=MAX_KEEP_ALIVE_SECS).contains(&seconds)
    {
        findings.push(error(
            format!("the keep-alive interval must be between 1 and {MAX_KEEP_ALIVE_SECS} seconds"),
            None,
        ));
    }
    if let Some(dns) = &options.dns {
        validate_dns(dns, findings);
    }
    for (domain, address) in &options.hosts {
        if domain.is_empty() || domain.chars().any(char::is_whitespace) {
            findings.push(error(
                format!("“{domain}” is not a valid host name for the hosts table"),
                Some(domain),
            ));
        }
        if address.parse::<IpAddr>().is_err() {
            findings.push(error(
                format!("“{address}” is not an IP address for the host “{domain}”"),
                Some(address),
            ));
        }
    }
    if let Some(sniffer) = &options.sniffer {
        validate_sniffer(sniffer, findings);
    }
    let geo = &options.geo;
    for (label, value) in [
        ("geoip", &geo.geox_url.geoip),
        ("geosite", &geo.geox_url.geosite),
        ("mmdb", &geo.geox_url.mmdb),
        ("asn", &geo.geox_url.asn),
    ] {
        if let Some(url) = value
            && !url.starts_with("https://")
        {
            findings.push(error(
                format!("the {label} download address must start with https://"),
                Some(url),
            ));
        }
    }
    if let Some(hours) = geo.update_interval_hours
        && !(1..=MAX_GEO_INTERVAL_HOURS).contains(&hours)
    {
        findings.push(error(
            format!(
                "the geodata refresh interval must be between 1 and {MAX_GEO_INTERVAL_HOURS} hours"
            ),
            None,
        ));
    }
}

/// Check the DNS block.
fn validate_dns(dns: &DnsSettings, findings: &mut Vec<Finding>) {
    if let Some(range) = &dns.fake_ip_range
        && !is_cidr(range)
    {
        findings.push(error(
            format!("“{range}” is not a valid fake-IP range (use a CIDR such as 198.18.0.1/16)"),
            Some(range),
        ));
    }
    let lists = [
        ("fake-ip-filter", &dns.fake_ip_filter),
        ("nameserver", &dns.nameserver),
        ("fallback", &dns.fallback),
        ("default-nameserver", &dns.default_nameserver),
        ("proxy-server-nameserver", &dns.proxy_server_nameserver),
    ];
    for (label, entries) in lists {
        for entry in entries {
            if entry.is_empty() || entry.chars().any(char::is_whitespace) {
                findings.push(error(
                    format!("an entry of {label} is empty or contains a space"),
                    Some(entry),
                ));
            }
        }
    }
    for (domain, servers) in &dns.nameserver_policy {
        if domain.is_empty() || servers.is_empty() {
            findings.push(error(
                format!(
                    "the nameserver policy for “{domain}” needs a domain and at least one server"
                ),
                Some(domain),
            ));
        }
        for server in servers {
            if server.is_empty() || server.chars().any(char::is_whitespace) {
                findings.push(error(
                    format!(
                        "a nameserver in the policy for “{domain}” is empty or contains a space"
                    ),
                    Some(server),
                ));
            }
        }
    }
}

/// Check the sniffer block.
fn validate_sniffer(sniffer: &crate::domain::SnifferSettings, findings: &mut Vec<Finding>) {
    for (protocol, ports) in &sniffer.sniff {
        if !SNIFF_PROTOCOLS.contains(&protocol.as_str()) {
            findings.push(error(
                format!("“{protocol}” is not a sniffed protocol (use HTTP, TLS or QUIC)"),
                Some(protocol),
            ));
        }
        for port in ports {
            if !is_port_or_range(port) {
                findings.push(error(
                    format!("“{port}” is not a port or port range for {protocol}"),
                    Some(port),
                ));
            }
        }
    }
    for domain in sniffer.force_domain.iter().chain(&sniffer.skip_domain) {
        if domain.is_empty() || domain.chars().any(char::is_whitespace) {
            findings.push(error(
                format!("“{domain}” is not a valid domain for the sniffer"),
                Some(domain),
            ));
        }
    }
}

/// Whether `text` is an IP address with a prefix length (`10.0.0.0/8`), or a
/// bare address.
pub fn is_cidr(text: &str) -> bool {
    let (address, prefix) = match text.split_once('/') {
        Some((address, prefix)) => (address, Some(prefix)),
        None => (text, None),
    };
    let Ok(address) = address.parse::<IpAddr>() else {
        return false;
    };
    let max = if address.is_ipv4() { 32 } else { 128 };
    match prefix {
        None => true,
        Some(prefix) => prefix.parse::<u8>().is_ok_and(|bits| bits <= max),
    }
}

/// Whether `text` is one port (`443`) or an inclusive range (`1000-2000`).
fn is_port_or_range(text: &str) -> bool {
    let valid = |port: &str| port.parse::<u16>().is_ok_and(|value| value >= 1);
    match text.split_once('-') {
        None => valid(text),
        Some((low, high)) => valid(low) && valid(high),
    }
}

/// The engine options the merged configuration carries.
///
/// The user's options are used as they are, except for the DNS block, which
/// follows the subscription's unless the user has turned the override on.
pub(crate) fn resolve(
    prefs: &UserPreferences,
    subscriptions: &[SubscriptionPayload],
) -> EngineOptions {
    let mut options = prefs.engine.clone();
    let subscription_dns = subscriptions.iter().find_map(|payload| payload.dns.clone());
    options.dns = if prefs.engine.override_subscription_dns {
        prefs.engine.dns.clone()
    } else {
        subscription_dns.or_else(|| prefs.engine.dns.clone())
    };
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DnsEnhancedMode, SnifferSettings, TunOptions, TunStack};

    fn errors(prefs: &UserPreferences) -> Vec<String> {
        let mut findings = Vec::new();
        validate_preferences(prefs, &mut findings);
        findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .map(|f| f.message.clone())
            .collect()
    }

    fn with_engine(engine: EngineOptions) -> UserPreferences {
        UserPreferences {
            engine,
            ..UserPreferences::default()
        }
    }

    #[test]
    fn default_preferences_have_no_errors() {
        assert!(errors(&UserPreferences::default()).is_empty());
    }

    #[test]
    fn a_valid_cidr_is_accepted_and_a_bad_one_is_refused() {
        assert!(is_cidr("10.0.0.0/8"));
        assert!(is_cidr("fd00::/8"));
        assert!(is_cidr("192.168.1.1"));
        assert!(!is_cidr("10.0.0.0/33"));
        assert!(!is_cidr("not-an-address/8"));
        assert!(!is_cidr("10.0.0.0/"));
    }

    #[test]
    fn a_bad_route_address_is_an_error_naming_the_entry() {
        let prefs = with_engine(EngineOptions {
            tun: TunOptions {
                route_exclude_address: vec!["192.168.0.0/99".to_string()],
                ..TunOptions::default()
            },
            ..EngineOptions::default()
        });
        let messages = errors(&prefs);
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].contains("192.168.0.0/99"));
    }

    #[test]
    fn a_good_tun_extra_is_not_an_error() {
        let prefs = with_engine(EngineOptions {
            tun: TunOptions {
                stack: Some(TunStack::Mixed),
                strict_route: Some(true),
                route_address: vec!["0.0.0.0/0".to_string()],
                route_exclude_address: vec!["192.168.0.0/16".to_string()],
                ..TunOptions::default()
            },
            ..EngineOptions::default()
        });
        assert!(errors(&prefs).is_empty());
    }

    #[test]
    fn a_fake_ip_range_must_be_a_cidr() {
        let prefs = with_engine(EngineOptions {
            dns: Some(DnsSettings {
                fake_ip_range: Some("198.18.0.1/40".to_string()),
                enhanced_mode: Some(DnsEnhancedMode::FakeIp),
                ..DnsSettings::default()
            }),
            ..EngineOptions::default()
        });
        assert_eq!(errors(&prefs).len(), 1);
    }

    #[test]
    fn a_host_entry_needs_an_ip_address() {
        let mut hosts = std::collections::BTreeMap::new();
        hosts.insert("example.test".to_string(), "not-an-ip".to_string());
        let prefs = with_engine(EngineOptions {
            hosts,
            ..EngineOptions::default()
        });
        assert_eq!(errors(&prefs).len(), 1);
    }

    #[test]
    fn sniffer_protocols_and_ports_are_checked() {
        let mut sniff = std::collections::BTreeMap::new();
        sniff.insert(
            "HTTP".to_string(),
            vec!["80".to_string(), "8000-9000".to_string()],
        );
        sniff.insert("FTP".to_string(), vec!["21".to_string()]);
        sniff.insert("TLS".to_string(), vec!["70000".to_string()]);
        let prefs = with_engine(EngineOptions {
            sniffer: Some(SnifferSettings {
                enable: Some(true),
                sniff,
                ..SnifferSettings::default()
            }),
            ..EngineOptions::default()
        });
        // FTP is not a protocol and 70000 is not a port.
        assert_eq!(errors(&prefs).len(), 2);
    }

    #[test]
    fn the_latency_and_log_settings_have_bounds() {
        let prefs = UserPreferences {
            delay_test_timeout_ms: Some(5),
            log_retention_lines: Some(50_000),
            ..UserPreferences::default()
        };
        assert_eq!(errors(&prefs).len(), 2);
        let prefs = UserPreferences {
            delay_test_url: Some("ftp://example.test".to_string()),
            ..UserPreferences::default()
        };
        assert_eq!(errors(&prefs).len(), 1);
    }

    #[test]
    fn the_user_dns_wins_only_when_the_override_is_on() {
        let subscription = SubscriptionPayload {
            dns: Some(DnsSettings {
                enable: Some(true),
                nameserver: vec!["https://sub.example.test/dns".to_string()],
                ..DnsSettings::default()
            }),
            ..empty_payload()
        };
        let user_dns = DnsSettings {
            enable: Some(true),
            nameserver: vec!["1.1.1.1".to_string()],
            ..DnsSettings::default()
        };

        // Override off: the subscription's block is used.
        let prefs = with_engine(EngineOptions {
            dns: Some(user_dns.clone()),
            ..EngineOptions::default()
        });
        let resolved = resolve(&prefs, std::slice::from_ref(&subscription));
        assert_eq!(resolved.dns, subscription.dns);

        // Override on: the user's block is used.
        let prefs = with_engine(EngineOptions {
            dns: Some(user_dns.clone()),
            override_subscription_dns: true,
            ..EngineOptions::default()
        });
        let resolved = resolve(&prefs, std::slice::from_ref(&subscription));
        assert_eq!(resolved.dns, Some(user_dns));
    }

    #[test]
    fn without_a_subscription_dns_the_user_block_is_used() {
        let user_dns = DnsSettings {
            enable: Some(true),
            ..DnsSettings::default()
        };
        let prefs = with_engine(EngineOptions {
            dns: Some(user_dns.clone()),
            ..EngineOptions::default()
        });
        assert_eq!(resolve(&prefs, &[]).dns, Some(user_dns));
    }

    #[test]
    fn a_build_carries_the_chosen_dns_and_refuses_a_bad_engine_option() {
        let subscription = SubscriptionPayload {
            dns: Some(DnsSettings {
                enable: Some(true),
                nameserver: vec!["https://sub.example.test/dns".to_string()],
                ..DnsSettings::default()
            }),
            ..empty_payload()
        };
        let input = crate::domain::BuildInput {
            preferences: with_engine(EngineOptions {
                ipv6: Some(true),
                ..EngineOptions::default()
            }),
            subscriptions: vec![subscription.clone()],
            ..Default::default()
        };
        let config = crate::build(&input).config.expect("a valid build");
        assert_eq!(config.engine.ipv6, Some(true));
        assert_eq!(config.engine.dns, subscription.dns);

        let bad = crate::domain::BuildInput {
            preferences: with_engine(EngineOptions {
                keep_alive_interval: Some(0),
                ..EngineOptions::default()
            }),
            ..Default::default()
        };
        let outcome = crate::build(&bad);
        assert!(outcome.config.is_none());
        assert!(!outcome.is_ok());
    }

    #[test]
    fn a_dns_block_keeps_keys_the_app_does_not_model() {
        let text = r#"{"enable": true, "fake-ip-filter-mode": "blacklist"}"#;
        let dns: DnsSettings = serde_json::from_str(text).expect("loads");
        assert_eq!(dns.enable, Some(true));
        assert_eq!(
            dns.extra.get("fake-ip-filter-mode"),
            Some(&serde_json::Value::String("blacklist".to_string()))
        );
        let back = serde_json::to_value(&dns).expect("serializes");
        assert_eq!(back["fake-ip-filter-mode"], "blacklist");
    }

    fn empty_payload() -> SubscriptionPayload {
        SubscriptionPayload {
            subscription_id: "s".to_string(),
            nodes: Vec::new(),
            groups: Vec::new(),
            rules: Vec::new(),
            rule_providers: Vec::new(),
            settings: Default::default(),
            dns: None,
            dropped_entries: 0,
        }
    }
}
