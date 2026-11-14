//! The engine settings form, as plain values.
//!
//! The Bridge model in `starpod-ui/ui/bridge.slint` (`EngineSettingsModel`)
//! is generated Slint code. This module is its Rust twin: the same fields, in
//! plain Rust, with the conversion both ways. The UI glue copies one into the
//! other field by field, so the conversion itself is tested here, without a
//! window.
//!
//! The conventions are the Bridge's: a tri-state is `-1` (not set), `0` (off)
//! or `1` (on); a number is `-1` when not set; a string is `""` when not set;
//! a list is one entry per line.
//!
//! Saving goes through [`EngineForm::apply`], then the existing settings save
//! (`AppService::save_preferences`), so validation is the core's.

use std::collections::BTreeMap;

use starpod_core::domain::{
    DnsEnhancedMode, DnsSettings, EngineChannel, EngineOptions, FindProcessMode, GeoSettings,
    GeodataMode, GeoxUrls, SnifferSettings, TunOptions, TunStack, UserPreferences,
};

/// The engine settings form. Mirrors `EngineSettingsModel` in the Bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineForm {
    /// `""`, `gvisor`, `mixed` or `system`.
    pub tun_stack: String,
    /// Tri-state.
    pub tun_strict_route: i32,
    /// Tri-state.
    pub tun_auto_redirect: i32,
    /// Tri-state.
    pub tun_auto_detect_interface: i32,
    /// Tri-state.
    pub tun_endpoint_independent_nat: i32,
    /// One CIDR per line.
    pub tun_route_address: String,
    /// One CIDR per line.
    pub tun_route_exclude_address: String,
    /// Tri-state.
    pub ipv6: i32,
    /// Tri-state.
    pub unified_delay: i32,
    /// Tri-state.
    pub tcp_concurrent: i32,
    /// `""`, `off`, `strict` or `always`.
    pub find_process_mode: String,
    /// Seconds, or `-1`.
    pub keep_alive_interval: i32,
    /// Whether the user's DNS block replaces the subscription's.
    pub override_dns: bool,
    /// Tri-state.
    pub dns_enable: i32,
    /// `""`, `fake-ip` or `redir-host`.
    pub dns_enhanced_mode: String,
    /// A CIDR, or `""`.
    pub dns_fake_ip_range: String,
    /// One entry per line.
    pub dns_fake_ip_filter: String,
    /// One entry per line.
    pub dns_nameserver: String,
    /// One entry per line.
    pub dns_fallback: String,
    /// One entry per line.
    pub dns_default_nameserver: String,
    /// One entry per line.
    pub dns_proxy_server_nameserver: String,
    /// One `domain: server, server` per line.
    pub dns_nameserver_policy: String,
    /// Tri-state.
    pub dns_use_hosts: i32,
    /// One `domain ip` per line.
    pub hosts: String,
    /// Tri-state.
    pub sniffer_enable: i32,
    /// Tri-state.
    pub sniffer_force_dns_mapping: i32,
    /// Tri-state.
    pub sniffer_parse_pure_ip: i32,
    /// Tri-state.
    pub sniffer_override_destination: i32,
    /// One `HTTP: 80, 8080-8880` per line.
    pub sniffer_sniff: String,
    /// One domain per line.
    pub sniffer_force_domain: String,
    /// One domain per line.
    pub sniffer_skip_domain: String,
    /// `""`, `dat` or `mmdb`.
    pub geodata_mode: String,
    /// A URL, or `""` for the default source.
    pub geox_geoip: String,
    /// A URL, or `""` for the default source.
    pub geox_geosite: String,
    /// A URL, or `""` for the default source.
    pub geox_mmdb: String,
    /// A URL, or `""` for the default source.
    pub geox_asn: String,
    /// Tri-state.
    pub geodata_auto_update: i32,
    /// Hours, or `-1`.
    pub geodata_update_interval_hours: i32,
    /// A URL, or `""` for the app default.
    pub delay_test_url: String,
    /// Milliseconds, or `-1` for the app default.
    pub delay_test_timeout_ms: i32,
    /// Lines, or `-1` for the app default.
    pub log_retention_lines: i32,
    /// `stable` or `alpha`. `""` leaves the channel unchanged.
    pub engine_channel: String,
}

impl Default for EngineForm {
    /// Nothing set: every field is at its "not set" value.
    fn default() -> Self {
        Self {
            tun_stack: String::new(),
            tun_strict_route: -1,
            tun_auto_redirect: -1,
            tun_auto_detect_interface: -1,
            tun_endpoint_independent_nat: -1,
            tun_route_address: String::new(),
            tun_route_exclude_address: String::new(),
            ipv6: -1,
            unified_delay: -1,
            tcp_concurrent: -1,
            find_process_mode: String::new(),
            keep_alive_interval: -1,
            override_dns: false,
            dns_enable: -1,
            dns_enhanced_mode: String::new(),
            dns_fake_ip_range: String::new(),
            dns_fake_ip_filter: String::new(),
            dns_nameserver: String::new(),
            dns_fallback: String::new(),
            dns_default_nameserver: String::new(),
            dns_proxy_server_nameserver: String::new(),
            dns_nameserver_policy: String::new(),
            dns_use_hosts: -1,
            hosts: String::new(),
            sniffer_enable: -1,
            sniffer_force_dns_mapping: -1,
            sniffer_parse_pure_ip: -1,
            sniffer_override_destination: -1,
            sniffer_sniff: String::new(),
            sniffer_force_domain: String::new(),
            sniffer_skip_domain: String::new(),
            geodata_mode: String::new(),
            geox_geoip: String::new(),
            geox_geosite: String::new(),
            geox_mmdb: String::new(),
            geox_asn: String::new(),
            geodata_auto_update: -1,
            geodata_update_interval_hours: -1,
            delay_test_url: String::new(),
            delay_test_timeout_ms: -1,
            log_retention_lines: -1,
            engine_channel: String::new(),
        }
    }
}

/// Read a tri-state: `-1` is not set, `0` off, `1` on.
fn tri(value: i32, label: &str) -> Result<Option<bool>, String> {
    match value {
        -1 => Ok(None),
        0 => Ok(Some(false)),
        1 => Ok(Some(true)),
        other => Err(format!("{label} must be not set, on or off (got {other})")),
    }
}

/// Write a tri-state back.
fn tri_to_int(value: Option<bool>) -> i32 {
    match value {
        None => -1,
        Some(false) => 0,
        Some(true) => 1,
    }
}

/// Read a number where `-1` is not set.
fn number(value: i32, label: &str) -> Result<Option<u32>, String> {
    if value == -1 {
        return Ok(None);
    }
    u32::try_from(value)
        .map(Some)
        .map_err(|_| format!("{label} must be a whole number"))
}

/// Write a number back, `-1` when not set.
fn number_to_int(value: Option<u32>) -> i32 {
    value.map_or(-1, |value| i32::try_from(value).unwrap_or(i32::MAX))
}

/// A text field as a value: blank means not set.
fn text(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// A list field, one entry per line. Blank lines are dropped.
fn lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// A list of entries, one per line, back to text.
fn lines_to_text(entries: &[String]) -> String {
    entries.join("\n")
}

/// `domain: server, server` per line.
fn policy(value: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut map = BTreeMap::new();
    for line in lines(value) {
        let (domain, servers) = line
            .split_once(':')
            .ok_or_else(|| format!("“{line}” should read “domain: server, server”"))?;
        let servers: Vec<String> = servers
            .split(',')
            .map(str::trim)
            .filter(|server| !server.is_empty())
            .map(str::to_string)
            .collect();
        map.insert(domain.trim().to_string(), servers);
    }
    Ok(map)
}

/// `domain: server, server` per line, back to text.
fn policy_to_text(map: &BTreeMap<String, Vec<String>>) -> String {
    map.iter()
        .map(|(domain, servers)| format!("{domain}: {}", servers.join(", ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `domain ip` per line.
fn hosts(value: &str) -> Result<BTreeMap<String, String>, String> {
    let mut map = BTreeMap::new();
    for line in lines(value) {
        let mut parts = line.split_whitespace();
        match (parts.next(), parts.next(), parts.next()) {
            (Some(domain), Some(address), None) => {
                map.insert(domain.to_string(), address.to_string());
            }
            _ => return Err(format!("“{line}” should read “domain ip”")),
        }
    }
    Ok(map)
}

/// `domain ip` per line, back to text.
fn hosts_to_text(map: &BTreeMap<String, String>) -> String {
    map.iter()
        .map(|(domain, address)| format!("{domain} {address}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `HTTP: 80, 8080-8880` per line.
fn sniff(value: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    // The same shape as the policy: a protocol, a colon, then its ports.
    policy(value)
}

impl EngineForm {
    /// The form as the saved preferences show it.
    pub fn from_prefs(prefs: &UserPreferences) -> Self {
        let engine = &prefs.engine;
        let tun = &engine.tun;
        let dns = engine.dns.clone().unwrap_or_default();
        let sniffer = engine.sniffer.clone().unwrap_or_default();
        let geo = &engine.geo;
        Self {
            tun_stack: tun
                .stack
                .map_or(String::new(), |stack| stack.as_str().to_string()),
            tun_strict_route: tri_to_int(tun.strict_route),
            tun_auto_redirect: tri_to_int(tun.auto_redirect),
            tun_auto_detect_interface: tri_to_int(tun.auto_detect_interface),
            tun_endpoint_independent_nat: tri_to_int(tun.endpoint_independent_nat),
            tun_route_address: lines_to_text(&tun.route_address),
            tun_route_exclude_address: lines_to_text(&tun.route_exclude_address),
            ipv6: tri_to_int(engine.ipv6),
            unified_delay: tri_to_int(engine.unified_delay),
            tcp_concurrent: tri_to_int(engine.tcp_concurrent),
            find_process_mode: engine
                .find_process_mode
                .map_or(String::new(), |mode| mode.as_str().to_string()),
            keep_alive_interval: number_to_int(engine.keep_alive_interval),
            override_dns: engine.override_subscription_dns,
            dns_enable: tri_to_int(dns.enable),
            dns_enhanced_mode: dns
                .enhanced_mode
                .map_or(String::new(), |mode| mode.as_str().to_string()),
            dns_fake_ip_range: dns.fake_ip_range.clone().unwrap_or_default(),
            dns_fake_ip_filter: lines_to_text(&dns.fake_ip_filter),
            dns_nameserver: lines_to_text(&dns.nameserver),
            dns_fallback: lines_to_text(&dns.fallback),
            dns_default_nameserver: lines_to_text(&dns.default_nameserver),
            dns_proxy_server_nameserver: lines_to_text(&dns.proxy_server_nameserver),
            dns_nameserver_policy: policy_to_text(&dns.nameserver_policy),
            dns_use_hosts: tri_to_int(dns.use_hosts),
            hosts: hosts_to_text(&engine.hosts),
            sniffer_enable: tri_to_int(sniffer.enable),
            sniffer_force_dns_mapping: tri_to_int(sniffer.force_dns_mapping),
            sniffer_parse_pure_ip: tri_to_int(sniffer.parse_pure_ip),
            sniffer_override_destination: tri_to_int(sniffer.override_destination),
            sniffer_sniff: policy_to_text(&sniffer.sniff),
            sniffer_force_domain: lines_to_text(&sniffer.force_domain),
            sniffer_skip_domain: lines_to_text(&sniffer.skip_domain),
            geodata_mode: geo.mode.map_or(String::new(), |mode| match mode {
                GeodataMode::Dat => "dat".to_string(),
                GeodataMode::Mmdb => "mmdb".to_string(),
            }),
            geox_geoip: geo.geox_url.geoip.clone().unwrap_or_default(),
            geox_geosite: geo.geox_url.geosite.clone().unwrap_or_default(),
            geox_mmdb: geo.geox_url.mmdb.clone().unwrap_or_default(),
            geox_asn: geo.geox_url.asn.clone().unwrap_or_default(),
            geodata_auto_update: tri_to_int(geo.auto_update),
            geodata_update_interval_hours: number_to_int(geo.update_interval_hours),
            delay_test_url: prefs.delay_test_url.clone().unwrap_or_default(),
            delay_test_timeout_ms: number_to_int(prefs.delay_test_timeout_ms),
            log_retention_lines: number_to_int(prefs.log_retention_lines),
            engine_channel: match prefs.engine_channel {
                None => String::new(),
                Some(EngineChannel::Stable) => "stable".to_string(),
                Some(EngineChannel::Alpha) => "alpha".to_string(),
            },
        }
    }

    /// The preferences with this form applied.
    ///
    /// Everything the form covers is replaced. What it does not cover is kept:
    /// the DNS keys the app does not model, the geodata's history and the
    /// subscription-side settings. Values are only parsed here; their bounds
    /// are checked by the core when the candidate is built, so a bad value is
    /// reported the same way as any other save error.
    pub fn apply(&self, base: &UserPreferences) -> Result<UserPreferences, String> {
        let tun = TunOptions {
            stack: match self.tun_stack.trim() {
                "" => None,
                "gvisor" => Some(TunStack::Gvisor),
                "mixed" => Some(TunStack::Mixed),
                "system" => Some(TunStack::System),
                other => return Err(format!("“{other}” is not a TUN stack")),
            },
            strict_route: tri(self.tun_strict_route, "strict route")?,
            auto_redirect: tri(self.tun_auto_redirect, "auto redirect")?,
            auto_detect_interface: tri(self.tun_auto_detect_interface, "auto-detect interface")?,
            endpoint_independent_nat: tri(
                self.tun_endpoint_independent_nat,
                "endpoint-independent NAT",
            )?,
            route_address: lines(&self.tun_route_address),
            route_exclude_address: lines(&self.tun_route_exclude_address),
        };

        let find_process_mode = match self.find_process_mode.trim() {
            "" => None,
            "off" => Some(FindProcessMode::Off),
            "strict" => Some(FindProcessMode::Strict),
            "always" => Some(FindProcessMode::Always),
            other => return Err(format!("“{other}” is not a process mode")),
        };

        // The DNS block is written only when something in it is set. The keys
        // the app does not model come from the saved block, so they survive.
        let extra = base
            .engine
            .dns
            .as_ref()
            .map(|dns| dns.extra.clone())
            .unwrap_or_default();
        let dns = DnsSettings {
            enable: tri(self.dns_enable, "DNS enable")?,
            enhanced_mode: match self.dns_enhanced_mode.trim() {
                "" => None,
                "fake-ip" => Some(DnsEnhancedMode::FakeIp),
                "redir-host" => Some(DnsEnhancedMode::RedirHost),
                other => return Err(format!("“{other}” is not a DNS enhanced mode")),
            },
            fake_ip_range: text(&self.dns_fake_ip_range),
            fake_ip_filter: lines(&self.dns_fake_ip_filter),
            nameserver: lines(&self.dns_nameserver),
            fallback: lines(&self.dns_fallback),
            default_nameserver: lines(&self.dns_default_nameserver),
            proxy_server_nameserver: lines(&self.dns_proxy_server_nameserver),
            nameserver_policy: policy(&self.dns_nameserver_policy)?,
            use_hosts: tri(self.dns_use_hosts, "use hosts")?,
            extra,
        };
        let dns_is_empty = dns
            == DnsSettings {
                extra: dns.extra.clone(),
                ..DnsSettings::default()
            };
        let dns = (!dns_is_empty).then_some(dns);

        let sniffer = SnifferSettings {
            enable: tri(self.sniffer_enable, "sniffer enable")?,
            force_dns_mapping: tri(self.sniffer_force_dns_mapping, "force DNS mapping")?,
            parse_pure_ip: tri(self.sniffer_parse_pure_ip, "parse pure IP")?,
            override_destination: tri(self.sniffer_override_destination, "override destination")?,
            sniff: sniff(&self.sniffer_sniff)?,
            force_domain: lines(&self.sniffer_force_domain),
            skip_domain: lines(&self.sniffer_skip_domain),
        };
        let sniffer = (sniffer != SnifferSettings::default()).then_some(sniffer);

        let geodata_mode = match self.geodata_mode.trim() {
            "" => None,
            "dat" => Some(GeodataMode::Dat),
            "mmdb" => Some(GeodataMode::Mmdb),
            other => return Err(format!("“{other}” is not a geodata mode")),
        };
        let geo = GeoSettings {
            mode: geodata_mode,
            geox_url: GeoxUrls {
                geoip: text(&self.geox_geoip),
                geosite: text(&self.geox_geosite),
                mmdb: text(&self.geox_mmdb),
                asn: text(&self.geox_asn),
            },
            auto_update: tri(self.geodata_auto_update, "geodata auto-update")?,
            update_interval_hours: number(
                self.geodata_update_interval_hours,
                "the geodata refresh interval",
            )?,
        };

        let channel = match self.engine_channel.trim() {
            "" => base.engine_channel,
            "stable" => Some(EngineChannel::Stable),
            "alpha" => Some(EngineChannel::Alpha),
            other => return Err(format!("“{other}” is not an engine channel")),
        };

        let engine = EngineOptions {
            tun,
            ipv6: tri(self.ipv6, "IPv6")?,
            unified_delay: tri(self.unified_delay, "unified delay")?,
            tcp_concurrent: tri(self.tcp_concurrent, "TCP concurrent")?,
            find_process_mode,
            keep_alive_interval: number(self.keep_alive_interval, "the keep-alive interval")?,
            override_subscription_dns: self.override_dns,
            dns,
            hosts: hosts(&self.hosts)?,
            sniffer,
            geo,
        };

        Ok(UserPreferences {
            engine,
            delay_test_url: text(&self.delay_test_url),
            delay_test_timeout_ms: number(self.delay_test_timeout_ms, "the latency timeout")?,
            log_retention_lines: number(self.log_retention_lines, "the log lines kept")?,
            engine_channel: channel,
            ..base.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(prefs: &UserPreferences) -> UserPreferences {
        // The form must read what it wrote, so the round trip is the test.
        let form = EngineForm::from_prefs(prefs);
        form.apply(prefs).expect("the saved values apply")
    }

    #[test]
    fn a_blank_form_saves_the_defaults_unchanged() {
        let base = UserPreferences::default();
        let applied = EngineForm::default().apply(&base).expect("applies");
        assert_eq!(applied, base);
        assert!(applied.engine.is_default());
    }

    #[test]
    fn a_full_form_round_trips_through_the_preferences() {
        let mut hosts_map = BTreeMap::new();
        hosts_map.insert("router.lan".to_string(), "192.168.1.1".to_string());
        let mut policy_map = BTreeMap::new();
        policy_map.insert(
            "+.example.test".to_string(),
            vec![
                "https://dns.example.test/dns-query".to_string(),
                "1.1.1.1".to_string(),
            ],
        );
        let prefs = UserPreferences {
            engine: EngineOptions {
                tun: TunOptions {
                    stack: Some(TunStack::Mixed),
                    strict_route: Some(true),
                    auto_redirect: None,
                    auto_detect_interface: Some(false),
                    endpoint_independent_nat: None,
                    route_address: vec!["0.0.0.0/0".to_string()],
                    route_exclude_address: vec!["192.168.0.0/16".to_string()],
                },
                ipv6: Some(false),
                unified_delay: Some(true),
                tcp_concurrent: None,
                find_process_mode: Some(FindProcessMode::Strict),
                keep_alive_interval: Some(30),
                override_subscription_dns: true,
                dns: Some(DnsSettings {
                    enable: Some(true),
                    enhanced_mode: Some(DnsEnhancedMode::FakeIp),
                    fake_ip_range: Some("198.18.0.1/16".to_string()),
                    fake_ip_filter: vec!["*.lan".to_string()],
                    nameserver: vec!["1.1.1.1".to_string()],
                    nameserver_policy: policy_map,
                    use_hosts: Some(true),
                    ..DnsSettings::default()
                }),
                hosts: hosts_map,
                sniffer: Some(SnifferSettings {
                    enable: Some(true),
                    sniff: BTreeMap::from([(
                        "HTTP".to_string(),
                        vec!["80".to_string(), "8080-8880".to_string()],
                    )]),
                    force_domain: vec!["example.test".to_string()],
                    ..SnifferSettings::default()
                }),
                geo: GeoSettings {
                    mode: Some(GeodataMode::Mmdb),
                    geox_url: GeoxUrls {
                        mmdb: Some("https://geo.example.test/country.mmdb".to_string()),
                        ..GeoxUrls::default()
                    },
                    auto_update: Some(true),
                    update_interval_hours: Some(12),
                },
            },
            delay_test_url: Some("https://probe.example.test/generate_204".to_string()),
            delay_test_timeout_ms: Some(3000),
            log_retention_lines: Some(1500),
            engine_channel: Some(EngineChannel::Alpha),
            ..UserPreferences::default()
        };
        assert_eq!(saved(&prefs), prefs);
    }

    #[test]
    fn an_unknown_dns_key_survives_a_save() {
        let mut extra = serde_json::Map::new();
        extra.insert(
            "fake-ip-filter-mode".to_string(),
            serde_json::Value::String("blacklist".to_string()),
        );
        let base = UserPreferences {
            engine: EngineOptions {
                dns: Some(DnsSettings {
                    enable: Some(true),
                    extra,
                    ..DnsSettings::default()
                }),
                ..EngineOptions::default()
            },
            ..UserPreferences::default()
        };
        let mut form = EngineForm::from_prefs(&base);
        form.dns_nameserver = "9.9.9.9".to_string();
        let applied = form.apply(&base).expect("applies");
        let dns = applied.engine.dns.expect("a DNS block");
        assert_eq!(dns.nameserver, vec!["9.9.9.9".to_string()]);
        assert!(dns.extra.contains_key("fake-ip-filter-mode"));
    }

    #[test]
    fn a_bad_tri_state_or_mode_is_refused_with_its_name() {
        let base = UserPreferences::default();
        let form = EngineForm {
            ipv6: 7,
            ..EngineForm::default()
        };
        let error = form.apply(&base).expect_err("refused");
        assert!(error.contains("IPv6"), "{error}");

        let form = EngineForm {
            tun_stack: "turbo".to_string(),
            ..EngineForm::default()
        };
        assert!(form.apply(&base).expect_err("refused").contains("turbo"));
    }

    #[test]
    fn a_malformed_host_or_policy_line_is_refused() {
        let base = UserPreferences::default();
        let form = EngineForm {
            hosts: "router.lan 192.168.1.1 extra".to_string(),
            ..EngineForm::default()
        };
        assert!(form.apply(&base).is_err());
        let form = EngineForm {
            dns_nameserver_policy: "no colon here".to_string(),
            ..EngineForm::default()
        };
        assert!(form.apply(&base).is_err());
    }

    #[test]
    fn the_core_rejects_a_bad_value_the_form_passes_through() {
        // The form reads the number; the core owns the bound.
        let base = UserPreferences::default();
        let form = EngineForm {
            delay_test_timeout_ms: 5,
            ..EngineForm::default()
        };
        let applied = form.apply(&base).expect("the form parses it");
        let outcome = starpod_core::build(&starpod_core::domain::BuildInput {
            preferences: applied,
            ..Default::default()
        });
        assert!(!outcome.is_ok());
    }
}
