//! Turning the app's runtime configuration into the engine's YAML.
//!
//! This module is the **only** place that knows the engine's configuration
//! key names. Everything else in the app speaks the app's own vocabulary
//! (`RuntimeConfig`, `RuleSpec`, `UserPreferences`); this module is the
//! translation layer.
//!
//! # Why the nodes are rebuilt rather than serialized
//!
//! `starpod_core::domain::Node` carries the node's untouched engine-format
//! fields in `raw`, flattened into the struct. Serializing it directly would
//! emit `name` twice — once from the struct's own field, once from `raw` —
//! and the engine's YAML parser rejects duplicate keys
//! (`serde_saphyr`'s `DuplicateKeyPolicy::Error`, which is the default and
//! which meow relies on). So the emitter builds each node's map by hand:
//! the engine's fields, with `name` set once, from the app's name.
//!
//! The same applies to groups, whose member list the engine calls
//! `proxies`.
//!
//! # Field names
//!
//! Every key below is kebab-case, matching the engine's `RawConfig`, whose
//! serde attributes use `rename_all = "kebab-case"`. The engine's source of
//! truth is `meow-config-0.29.0/src/raw.rs`.

use serde_json::{Map, Value};
use starpod_core::domain::{Node, ProxyGroup, RuntimeConfig};

/// The engine's configuration, as a document ready to be serialized to
/// YAML and handed to the engine.
///
/// The controller address and secret are **not** part of this struct: they
/// belong to the app's engine harness, not to the user's configuration, and
/// keeping them apart means the user's configuration can be written,
/// compared and shown without leaking the secret.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineDocument {
    /// The document's body.
    pub body: Map<String, Value>,
}

impl EngineDocument {
    /// The address the engine's REST controller listens on.
    pub const CONTROLLER_KEY: &'static str = "external-controller";
    /// The engine's REST API secret — the key this document must **never**
    /// contain.
    ///
    /// The engine accepts it as a config key (`RawConfig.secret`,
    /// `meow-config-0.29.0/src/raw.rs:205`), so its absence from the
    /// document has to be a decision rather than an oversight. The secret
    /// reaches the engine through `--secret` on its command line, and
    /// nowhere else (ARCHITECTURE §5.1).
    pub const SECRET_KEY: &'static str = "secret";

    /// Build the document for a runtime configuration.
    pub fn build(config: &RuntimeConfig) -> Self {
        let mut body = Map::new();

        if let Some(port) = config.mixed_port {
            body.insert("mixed-port".to_string(), Value::from(port));
        }
        if let Some(port) = config.socks_port {
            body.insert("socks-port".to_string(), Value::from(port));
        }
        if let Some(port) = config.port {
            body.insert("port".to_string(), Value::from(port));
        }
        body.insert("allow-lan".to_string(), Value::Bool(config.allow_lan));
        if let Some(address) = &config.bind_address {
            body.insert("bind-address".to_string(), Value::String(address.clone()));
        }
        body.insert(
            "mode".to_string(),
            Value::String(config.mode.as_str().to_string()),
        );
        body.insert(
            "log-level".to_string(),
            Value::String(config.log_level.as_str().to_string()),
        );

        let tun = tun_section(&config.tun, &config.engine.tun);
        if let Some(tun) = tun {
            body.insert("tun".to_string(), tun);
        }
        engine_sections(&mut body, &config.engine);

        if !config.proxies.is_empty() {
            body.insert(
                "proxies".to_string(),
                Value::Array(config.proxies.iter().map(node_entry).collect()),
            );
        }
        if !config.proxy_groups.is_empty() {
            body.insert(
                "proxy-groups".to_string(),
                Value::Array(config.proxy_groups.iter().map(group_entry).collect()),
            );
        }
        body.insert(
            "rules".to_string(),
            Value::Array(
                config
                    .rules
                    .iter()
                    .map(|rule| Value::String(rule.clone()))
                    .collect(),
            ),
        );

        if !config.rule_providers.is_empty() {
            let mut providers = Map::new();
            for provider in &config.rule_providers {
                providers.insert(provider.name.clone(), rule_provider_entry(provider));
            }
            body.insert("rule-providers".to_string(), Value::Object(providers));
        }

        if !config.subscriptions.is_empty() {
            body.insert(
                "subscriptions".to_string(),
                Value::Array(
                    config
                        .subscriptions
                        .iter()
                        .map(subscription_entry)
                        .collect(),
                ),
            );
        }

        Self { body }
    }

    /// Attach the controller address.
    ///
    /// The address is a **loopback** address, and the engine binds it once,
    /// at startup — a reload never rebinds it (`meow-app-0.29.0/src/main.rs:1508-1571`
    /// binds the socket and spawns the API task; the `PUT /configs` path
    /// never touches the listener). So the address belongs in the *file*,
    /// where it survives every reload, and not only on the command line.
    ///
    /// The secret is deliberately **not** part of this. The controller
    /// secret is passed to the engine on its command line (`--secret`), and
    /// ARCHITECTURE §5.1 requires that it never reaches the file:
    ///
    /// > The `secret` key is never written to the YAML at all — the
    /// > controller secret is passed on the child's command line (`--secret`),
    /// > so the generated file does not carry it.
    ///
    /// The engine agrees: `--secret` overrides `config.api.secret` *after*
    /// the config is built (`meow-app-0.29.0/src/main.rs:612-615`), and the
    /// API's auth check reads that value (`meow-api-0.29.0/src/routes.rs:152`).
    /// Writing it into the file as well would put it in a user-readable
    /// artifact for no benefit.
    pub fn with_controller(mut self, controller: &str) -> Self {
        self.body.insert(
            Self::CONTROLLER_KEY.to_string(),
            Value::String(controller.to_string()),
        );
        self
    }

    /// The document as a JSON value, for tests and comparisons.
    pub fn to_value(&self) -> Value {
        Value::Object(self.body.clone())
    }

    /// The document as YAML.
    ///
    /// The engine parses YAML, so this is the format it must be given.
    ///
    /// The serializer options match the engine's own
    /// (`meow-config-0.29.0/src/lib.rs:887-890`): saphyr's default
    /// 80-column folded block style is disabled, so a long scalar is written
    /// on one line instead of being re-wrapped. A wrapped scalar is still
    /// the same string to the parser, but the engine's own saved configs are
    /// not wrapped, and matching them keeps a byte-level diff between the
    /// app's file and the engine's saved file readable.
    pub fn to_yaml(&self) -> Result<String, serde_saphyr::SerializeError> {
        serde_saphyr::to_string_with_options(&self.body, yaml_ser_options())
    }

    /// The document as a base64-encoded string, for the engine's
    /// `--config-string` flag.
    ///
    /// The engine decodes this as standard base64 into UTF-8 YAML.
    pub fn to_config_string(&self) -> Result<String, crate::error::EngineError> {
        let yaml = self
            .to_yaml()
            .map_err(|e| crate::error::EngineError::Internal(e.to_string()))?;
        use base64::Engine as _;
        Ok(base64::engine::general_purpose::STANDARD.encode(yaml.as_bytes()))
    }
}

/// The serializer options the engine itself uses.
///
/// Mirrored from `meow-config-0.29.0/src/lib.rs:887-890` so the app's file
/// and the engine's saved file are formatted the same way.
fn yaml_ser_options() -> serde_saphyr::SerializerOptions {
    serde_saphyr::ser_options! {
        folded_wrap_chars: usize::MAX,
    }
}

/// Build the engine's `tun:` section, or `None` when TUN is off entirely.
fn tun_section(
    tun: &starpod_core::domain::TunSettings,
    extras: &starpod_core::domain::TunOptions,
) -> Option<Value> {
    if tun.is_disabled() {
        return None;
    }
    let mut section = Map::new();
    section.insert("enable".to_string(), Value::Bool(tun.enabled));
    // The extras are written only with the section: a disabled TUN is left out
    // entirely, extras included.
    if let Some(stack) = extras.stack {
        section.insert(
            "stack".to_string(),
            Value::String(stack.as_str().to_string()),
        );
    }
    for (key, flag) in [
        ("strict-route", extras.strict_route),
        ("auto-redirect", extras.auto_redirect),
        ("auto-detect-interface", extras.auto_detect_interface),
        ("endpoint-independent-nat", extras.endpoint_independent_nat),
    ] {
        if let Some(flag) = flag {
            section.insert(key.to_string(), Value::Bool(flag));
        }
    }
    for (key, cidrs) in [
        ("route-address", &extras.route_address),
        ("route-exclude-address", &extras.route_exclude_address),
    ] {
        if !cidrs.is_empty() {
            section.insert(key.to_string(), string_array(cidrs));
        }
    }
    if let Some(device) = &tun.device {
        section.insert("device".to_string(), Value::String(device.clone()));
    }
    if let Some(mtu) = tun.mtu {
        section.insert("mtu".to_string(), Value::from(mtu));
    }
    if let Some(auto_route) = tun.auto_route {
        section.insert("auto-route".to_string(), Value::Bool(auto_route));
    }
    if let Some(dns_hijack) = &tun.dns_hijack {
        section.insert(
            "dns-hijack".to_string(),
            Value::Array(
                dns_hijack
                    .iter()
                    .map(|target| Value::String(target.clone()))
                    .collect(),
            ),
        );
    }
    Some(Value::Object(section))
}

/// Write the engine settings beyond TUN. Each unset setting writes nothing, so
/// a default configuration gets none of these keys.
fn engine_sections(body: &mut Map<String, Value>, options: &starpod_core::domain::EngineOptions) {
    if let Some(ipv6) = options.ipv6 {
        body.insert("ipv6".to_string(), Value::Bool(ipv6));
    }
    if let Some(unified) = options.unified_delay {
        body.insert("unified-delay".to_string(), Value::Bool(unified));
    }
    if let Some(concurrent) = options.tcp_concurrent {
        body.insert("tcp-concurrent".to_string(), Value::Bool(concurrent));
    }
    if let Some(mode) = options.find_process_mode {
        body.insert(
            "find-process-mode".to_string(),
            Value::String(mode.as_str().to_string()),
        );
    }
    if let Some(seconds) = options.keep_alive_interval {
        body.insert("keep-alive-interval".to_string(), Value::from(seconds));
    }
    if let Some(dns) = &options.dns {
        let section = dns_section(dns);
        if !section.is_empty() {
            body.insert("dns".to_string(), Value::Object(section));
        }
    }
    if !options.hosts.is_empty() {
        let hosts: Map<String, Value> = options
            .hosts
            .iter()
            .map(|(domain, address)| (domain.clone(), Value::String(address.clone())))
            .collect();
        body.insert("hosts".to_string(), Value::Object(hosts));
    }
    if let Some(sniffer) = &options.sniffer {
        let section = sniffer_section(sniffer);
        if !section.is_empty() {
            body.insert("sniffer".to_string(), Value::Object(section));
        }
    }
    let geo = &options.geo;
    if let Some(mode) = geo.mode {
        // The engine reads `geodata-mode` as a flag: true selects the `.dat`
        // format, false the `.mmdb` one.
        body.insert(
            "geodata-mode".to_string(),
            Value::Bool(mode == starpod_core::domain::GeodataMode::Dat),
        );
    }
    let mut urls = Map::new();
    for (key, url) in [
        ("geoip", &geo.geox_url.geoip),
        ("geosite", &geo.geox_url.geosite),
        ("mmdb", &geo.geox_url.mmdb),
        ("asn", &geo.geox_url.asn),
    ] {
        if let Some(url) = url {
            urls.insert(key.to_string(), Value::String(url.clone()));
        }
    }
    if !urls.is_empty() {
        body.insert("geox-url".to_string(), Value::Object(urls));
    }
    let mut auto = Map::new();
    if let Some(enabled) = geo.auto_update {
        auto.insert("auto-update".to_string(), Value::Bool(enabled));
    }
    if let Some(hours) = geo.update_interval_hours {
        auto.insert("auto-update-interval".to_string(), Value::from(hours));
    }
    if !auto.is_empty() {
        body.insert("geodata".to_string(), Value::Object(auto));
    }
}

/// Build the `dns:` map. A key the app does not model is written back as the
/// subscription gave it, but never over a modeled key.
fn dns_section(dns: &starpod_core::domain::DnsSettings) -> Map<String, Value> {
    let mut section = Map::new();
    if let Some(enable) = dns.enable {
        section.insert("enable".to_string(), Value::Bool(enable));
    }
    if let Some(mode) = dns.enhanced_mode {
        section.insert(
            "enhanced-mode".to_string(),
            Value::String(mode.as_str().to_string()),
        );
    }
    if let Some(range) = &dns.fake_ip_range {
        section.insert("fake-ip-range".to_string(), Value::String(range.clone()));
    }
    for (key, entries) in [
        ("fake-ip-filter", &dns.fake_ip_filter),
        ("nameserver", &dns.nameserver),
        ("fallback", &dns.fallback),
        ("default-nameserver", &dns.default_nameserver),
        ("proxy-server-nameserver", &dns.proxy_server_nameserver),
    ] {
        if !entries.is_empty() {
            section.insert(key.to_string(), string_array(entries));
        }
    }
    if !dns.nameserver_policy.is_empty() {
        let policy: Map<String, Value> = dns
            .nameserver_policy
            .iter()
            .map(|(domain, servers)| (domain.clone(), string_array(servers)))
            .collect();
        section.insert("nameserver-policy".to_string(), Value::Object(policy));
    }
    if let Some(use_hosts) = dns.use_hosts {
        section.insert("use-hosts".to_string(), Value::Bool(use_hosts));
    }
    for (key, value) in &dns.extra {
        section.entry(key.clone()).or_insert_with(|| value.clone());
    }
    section
}

/// Build the `sniffer:` map.
fn sniffer_section(sniffer: &starpod_core::domain::SnifferSettings) -> Map<String, Value> {
    let mut section = Map::new();
    for (key, flag) in [
        ("enable", sniffer.enable),
        ("force-dns-mapping", sniffer.force_dns_mapping),
        ("parse-pure-ip", sniffer.parse_pure_ip),
        ("override-destination", sniffer.override_destination),
    ] {
        if let Some(flag) = flag {
            section.insert(key.to_string(), Value::Bool(flag));
        }
    }
    if !sniffer.sniff.is_empty() {
        // The engine nests each protocol's ports under `ports:`.
        let sniff: Map<String, Value> = sniffer
            .sniff
            .iter()
            .map(|(protocol, ports)| {
                let mut entry = Map::new();
                entry.insert("ports".to_string(), string_array(ports));
                (protocol.clone(), Value::Object(entry))
            })
            .collect();
        section.insert("sniff".to_string(), Value::Object(sniff));
    }
    for (key, domains) in [
        ("force-domain", &sniffer.force_domain),
        ("skip-domain", &sniffer.skip_domain),
    ] {
        if !domains.is_empty() {
            section.insert(key.to_string(), string_array(domains));
        }
    }
    section
}

/// A JSON array of strings.
fn string_array(items: &[String]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|item| Value::String(item.clone()))
            .collect(),
    )
}

/// Build one `proxies:` entry.
///
/// The node's own fields come from `raw` — the untouched engine-format map
/// the subscription gave us — with `name` set once from the app's name.
/// The app's `protocol` field is *not* emitted: the engine reads the
/// protocol from the node's `type`, which `raw` already carries.
fn node_entry(node: &Node) -> Value {
    let mut entry = Map::new();
    // The name first, so it is the first key a reader sees, and so it
    // replaces whatever `raw` carried.
    entry.insert("name".to_string(), Value::String(node.name.clone()));
    for (key, value) in &node.raw {
        if key == "name" {
            continue;
        }
        entry.insert(key.clone(), value.clone());
    }
    Value::Object(entry)
}

/// Build one `proxy-groups:` entry.
///
/// The engine calls the member list `proxies`, and the group type `type`.
fn group_entry(group: &ProxyGroup) -> Value {
    let mut entry = Map::new();
    entry.insert("name".to_string(), Value::String(group.name.clone()));
    entry.insert("type".to_string(), Value::String(group.group_type.clone()));
    for (key, value) in &group.raw {
        if key == "name" || key == "type" || key == "proxies" || key == "hidden" {
            continue;
        }
        entry.insert(key.clone(), value.clone());
    }
    entry.insert(
        "proxies".to_string(),
        Value::Array(
            group
                .members
                .iter()
                .map(|member| Value::String(member.clone()))
                .collect(),
        ),
    );
    if let Some(url) = &group.url {
        entry.insert("url".to_string(), Value::String(url.clone()));
    }
    if let Some(interval) = group.interval {
        entry.insert("interval".to_string(), Value::from(interval));
    }
    if let Some(strategy) = &group.strategy {
        entry.insert("strategy".to_string(), Value::String(strategy.clone()));
    }
    if group.hidden {
        entry.insert("hidden".to_string(), Value::Bool(true));
    }
    Value::Object(entry)
}

/// Build one `rule-providers:` entry.
fn rule_provider_entry(provider: &starpod_core::domain::RuleProvider) -> Value {
    let mut entry = Map::new();
    entry.insert(
        "type".to_string(),
        Value::String(provider.provider_type.clone()),
    );
    entry.insert(
        "behavior".to_string(),
        Value::String(
            match provider.behavior {
                starpod_core::domain::RuleProviderBehavior::Domain => "domain",
                starpod_core::domain::RuleProviderBehavior::IpCidr => "ipcidr",
                starpod_core::domain::RuleProviderBehavior::Classical => "classical",
            }
            .to_string(),
        ),
    );
    if let Some(url) = &provider.url {
        entry.insert("url".to_string(), Value::String(url.clone()));
    }
    if let Some(path) = &provider.path {
        entry.insert("path".to_string(), Value::String(path.clone()));
    }
    if let Some(interval) = provider.interval {
        entry.insert("interval".to_string(), Value::from(interval));
    }
    if let Some(format) = &provider.format {
        entry.insert("format".to_string(), Value::String(format.clone()));
    }
    if let Some(payload) = &provider.payload {
        entry.insert(
            "payload".to_string(),
            Value::Array(
                payload
                    .iter()
                    .map(|rule| Value::String(rule.clone()))
                    .collect(),
            ),
        );
    }
    if let Some(proxy) = &provider.proxy {
        entry.insert("proxy".to_string(), Value::String(proxy.clone()));
    }
    Value::Object(entry)
}

/// Build one `subscriptions:` entry.
fn subscription_entry(subscription: &starpod_core::domain::EngineManagedSubscription) -> Value {
    let mut entry = Map::new();
    entry.insert("name".to_string(), Value::String(subscription.name.clone()));
    entry.insert("url".to_string(), Value::String(subscription.url.clone()));
    entry.insert(
        "interval".to_string(),
        Value::from(subscription.interval_secs),
    );
    if let Some(proxy) = &subscription.proxy {
        entry.insert("proxy".to_string(), Value::String(proxy.clone()));
    }
    Value::Object(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use starpod_core::domain::{LogLevel, ProxyMode, TunSettings};

    fn base_config() -> RuntimeConfig {
        RuntimeConfig {
            mixed_port: Some(7890),
            socks_port: Some(7891),
            port: None,
            allow_lan: false,
            bind_address: None,
            mode: ProxyMode::Rule,
            log_level: LogLevel::Info,
            tun: TunSettings::default(),
            proxies: Vec::new(),
            proxy_groups: Vec::new(),
            rules: vec!["MATCH,PROXY".to_string()],
            rule_providers: Vec::new(),
            subscriptions: Vec::new(),
            engine: Default::default(),
        }
    }

    #[test]
    fn a_node_is_emitted_with_one_name() {
        let mut raw = Map::new();
        raw.insert("name".to_string(), Value::String("old".to_string()));
        raw.insert("type".to_string(), Value::String("ss".to_string()));
        raw.insert("password".to_string(), Value::String("secret".to_string()));
        let config = RuntimeConfig {
            proxies: vec![Node {
                id: "id".to_string(),
                name: "new".to_string(),
                protocol: "ss".to_string(),
                server: Some("example.invalid".to_string()),
                port: Some(443),
                tags: Vec::new(),
                raw,
            }],
            ..base_config()
        };

        let document = EngineDocument::build(&config);
        let yaml = document.to_yaml().unwrap();
        assert_eq!(yaml.matches("name:").count(), 1, "{yaml}");
        assert!(yaml.contains("name: new"), "{yaml}");
        assert!(yaml.contains("type: ss"), "{yaml}");
        assert!(yaml.contains("password: secret"), "{yaml}");
    }

    #[test]
    fn a_group_is_emitted_with_its_members_under_proxies() {
        let config = RuntimeConfig {
            proxy_groups: vec![ProxyGroup {
                id: "id".to_string(),
                name: "Proxy".to_string(),
                group_type: "select".to_string(),
                members: vec!["HK 01".to_string()],
                strategy: None,
                url: None,
                interval: None,
                hidden: false,
                raw: Map::new(),
            }],
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        assert!(yaml.contains("type: select"), "{yaml}");
        assert!(yaml.contains("- HK 01"), "{yaml}");
    }

    #[test]
    fn the_groups_are_emitted_in_the_configured_order() {
        let group = |name: &str| ProxyGroup {
            id: format!("id-{name}"),
            name: name.to_string(),
            group_type: "select".to_string(),
            members: Vec::new(),
            strategy: None,
            url: None,
            interval: None,
            hidden: false,
            raw: Map::new(),
        };
        let config = RuntimeConfig {
            proxy_groups: vec![group("Zeta"), group("Alpha"), group("Mid")],
            ..base_config()
        };
        let document = EngineDocument::build(&config).to_value();
        let names: Vec<&str> = document["proxy-groups"]
            .as_array()
            .expect("the groups are an array")
            .iter()
            .map(|group| group["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["Zeta", "Alpha", "Mid"]);
    }

    #[test]
    fn the_ports_are_emitted_under_the_engines_names() {
        let yaml = EngineDocument::build(&base_config()).to_yaml().unwrap();
        assert!(yaml.contains("mixed-port: 7890"), "{yaml}");
        assert!(yaml.contains("socks-port: 7891"), "{yaml}");
        assert!(yaml.contains("allow-lan: false"), "{yaml}");
        assert!(yaml.contains("mode: rule"), "{yaml}");
        assert!(yaml.contains("log-level: info"), "{yaml}");
    }

    #[test]
    fn an_unset_port_is_left_out() {
        let config = RuntimeConfig {
            port: None,
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        // The plain `port:` key must be absent -- but `socks-port:` is a
        // different key and is present.
        assert!(
            !yaml.lines().any(|line| line.starts_with("port:")),
            "{yaml}"
        );
        assert!(yaml.contains("socks-port: 7891"), "{yaml}");
    }

    #[test]
    fn a_disabled_tun_is_left_out_entirely() {
        let yaml = EngineDocument::build(&base_config()).to_yaml().unwrap();
        assert!(!yaml.contains("tun:"), "{yaml}");
    }

    #[test]
    fn an_enabled_tun_is_emitted_with_its_settings() {
        let config = RuntimeConfig {
            tun: TunSettings {
                enabled: true,
                device: Some("starpod0".to_string()),
                mtu: Some(1500),
                auto_route: Some(true),
                dns_hijack: Some(vec!["any:53".to_string()]),
            },
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        assert!(yaml.contains("tun:"), "{yaml}");
        assert!(yaml.contains("enable: true"), "{yaml}");
        assert!(yaml.contains("device: starpod0"), "{yaml}");
        assert!(yaml.contains("mtu: 1500"), "{yaml}");
        assert!(yaml.contains("auto-route: true"), "{yaml}");
        assert!(yaml.contains("- any:53"), "{yaml}");
    }

    #[test]
    fn the_controller_and_the_secret_are_added_only_on_request() {
        let plain = EngineDocument::build(&base_config());
        assert!(!plain.body.contains_key("external-controller"));
        assert!(!plain.body.contains_key("secret"));

        let secured = EngineDocument::build(&base_config()).with_controller("127.0.0.1:9090");
        assert!(secured.body.contains_key("external-controller"));
        assert_eq!(
            secured.body.get("external-controller").unwrap(),
            &Value::String("127.0.0.1:9090".into())
        );
    }

    #[test]
    fn the_controller_is_added_without_a_secret() {
        // The secret reaches the engine through `--secret` on its command
        // line and nowhere else (ARCHITECTURE §5.1). There is no code path
        // in this crate that can put one in the file.
        let document = EngineDocument::build(&base_config()).with_controller("127.0.0.1:9090");
        assert!(
            !document.body.contains_key(EngineDocument::SECRET_KEY),
            "the controller secret must never reach the config file"
        );
        let yaml = document.to_yaml().unwrap();
        assert!(!yaml.contains("secret:"), "{yaml}");
    }

    // ------------------------------------------------------------------
    // The keys the engine actually reads
    //
    // The engine's parser ignores unknown keys, so a misspelled key is a
    // silent failure: the configuration validates, the proxy starts, and the
    // setting the key carried is simply gone. These tests are the guard
    // against that.
    // ------------------------------------------------------------------

    /// The top-level keys the engine reads.
    ///
    /// Source: `meow-config-0.29.0/src/raw.rs:184+` — `RawConfig`, whose
    /// serde attributes are `rename_all = "kebab-case"`, so `mixed_port`
    /// becomes `mixed-port`.
    ///
    /// The list is the struct's fields, transcribed. It is not a guess: an
    /// earlier version of this list omitted `tun`, and the test that uses
    /// it caught the omission immediately — which is exactly the failure
    /// mode this list exists to prevent.
    const ENGINE_KEYS: &[&str] = &[
        "port",
        "socks-port",
        "mixed-port",
        "allow-lan",
        "bind-address",
        "mode",
        "log-level",
        "external-controller",
        "external-ui",
        "external-ui-name",
        "external-ui-url",
        "secret",
        "dns",
        "proxies",
        "proxy-groups",
        "proxy-providers",
        "rules",
        "rule-providers",
        "sub-rules",
        "subscriptions",
        "strict",
        "tproxy-port",
        "tproxy-sni",
        "routing-mark",
        "tcp-connect-timeout",
        "connection-notes",
        "logbook",
        "traffic-ledger",
        "hosts",
        "sniffer",
        "listeners",
        "authentication",
        "skip-auth-prefixes",
        "geodata",
        "geox-url",
        "tun",
        "max-connections",
        "tcp-keepalive",
        "tcp-keepalive-idle",
        "tcp-keepalive-interval",
        "tcp-keepalive-probes",
        "tcp-idle-timeout",
        "firewall",
        "udp",
        "udp-timeout",
        "keep-alive-idle",
        "keep-alive-interval",
        "disable-keep-alive",
        // Written by the settings added for engine parity. These follow the
        // mihomo-compatible schema the engine advertises; the engine's own
        // `-t` check is what confirms each one (docs/research/meow-integration-contract.md).
        "ipv6",
        "unified-delay",
        "tcp-concurrent",
        "find-process-mode",
        "geodata-mode",
    ];

    /// A configuration with one of everything the emitter can produce.
    fn full_config() -> RuntimeConfig {
        let mut raw = Map::new();
        raw.insert("type".to_string(), Value::String("ss".to_string()));
        raw.insert(
            "server".to_string(),
            Value::String("example.invalid".to_string()),
        );
        raw.insert("port".to_string(), Value::from(443));
        raw.insert("password".to_string(), Value::String("pw".to_string()));

        RuntimeConfig {
            mixed_port: Some(7890),
            socks_port: Some(7891),
            port: Some(7892),
            allow_lan: true,
            bind_address: Some("0.0.0.0".to_string()),
            mode: ProxyMode::Rule,
            log_level: LogLevel::Info,
            tun: TunSettings {
                enabled: true,
                device: Some("starpod0".to_string()),
                mtu: Some(1500),
                auto_route: Some(true),
                dns_hijack: Some(vec!["any:53".to_string()]),
            },
            proxies: vec![Node {
                id: "n1".to_string(),
                name: "HK 01".to_string(),
                protocol: "ss".to_string(),
                server: Some("example.invalid".to_string()),
                port: Some(443),
                tags: Vec::new(),
                raw,
            }],
            proxy_groups: vec![ProxyGroup {
                id: "g1".to_string(),
                name: "PROXY".to_string(),
                group_type: "select".to_string(),
                members: vec!["HK 01".to_string()],
                strategy: None,
                url: None,
                interval: None,
                hidden: false,
                raw: Map::new(),
            }],
            rules: vec!["MATCH,PROXY".to_string()],
            rule_providers: vec![starpod_core::domain::RuleProvider {
                name: "reject-list".to_string(),
                provider_type: "http".to_string(),
                url: Some("https://example.invalid/rules.list".to_string()),
                path: None,
                behavior: starpod_core::domain::RuleProviderBehavior::Domain,
                interval: Some(86400),
                format: None,
                payload: None,
                proxy: None,
            }],
            subscriptions: vec![starpod_core::domain::EngineManagedSubscription {
                name: "airport".to_string(),
                url: "https://example.invalid/sub".to_string(),
                interval_secs: 3600,
                proxy: None,
            }],
            engine: Default::default(),
        }
    }

    #[test]
    fn every_key_the_emitter_writes_is_one_the_engine_reads() {
        let document = EngineDocument::build(&full_config()).with_controller("127.0.0.1:9090");
        for key in document.body.keys() {
            assert!(
                ENGINE_KEYS.contains(&key.as_str()),
                "{key:?} is not a key the engine reads — it would be silently ignored"
            );
        }
    }

    #[test]
    fn the_emitter_never_writes_the_apps_own_snake_case_names() {
        // The engine reads kebab-case. The app's domain types use
        // snake_case, and the engine ignores keys it does not know — so
        // writing the app's names would silently drop the setting.
        let document = EngineDocument::build(&full_config());
        let keys: Vec<&str> = document.body.keys().map(String::as_str).collect();

        assert!(keys.contains(&"mixed-port"), "{keys:?}");
        assert!(keys.contains(&"socks-port"), "{keys:?}");
        assert!(keys.contains(&"allow-lan"), "{keys:?}");
        assert!(keys.contains(&"log-level"), "{keys:?}");
        assert!(keys.contains(&"proxy-groups"), "{keys:?}");
        assert!(keys.contains(&"rule-providers"), "{keys:?}");

        for wrong in [
            "mixed_port",
            "socks_port",
            "allow_lan",
            "bind_address",
            "log_level",
            "proxy_groups",
            "rule_providers",
        ] {
            assert!(
                !keys.contains(&wrong),
                "the engine ignores {wrong:?}, so it must not be written: {keys:?}"
            );
        }
    }

    #[test]
    fn the_emitted_yaml_parses_back_to_the_same_document() {
        let document = EngineDocument::build(&full_config()).with_controller("127.0.0.1:9090");
        let yaml = document.to_yaml().unwrap();
        let parsed: serde_json::Value = serde_saphyr::from_str(&yaml).unwrap();
        assert_eq!(parsed, document.to_value());
    }

    #[test]
    fn a_long_scalar_is_not_wrapped() {
        // The engine's own emitter disables the folded block style
        // (`meow-config-0.29.0/src/lib.rs:887-890`); the app's file should
        // look the same, so a diff between the two is readable.
        let long = "x".repeat(300);
        let document = EngineDocument::build(&RuntimeConfig {
            subscriptions: vec![starpod_core::domain::EngineManagedSubscription {
                name: "airport".to_string(),
                url: format!("https://example.invalid/{long}"),
                interval_secs: 3600,
                proxy: None,
            }],
            ..full_config()
        });
        let yaml = document.to_yaml().unwrap();
        assert!(
            yaml.contains(&long),
            "the long scalar must be on one line, not folded"
        );
    }

    #[test]
    fn the_rules_are_emitted_in_order() {
        let config = RuntimeConfig {
            rules: vec!["DOMAIN,a.com,DIRECT".to_string(), "MATCH,PROXY".to_string()],
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        let first = yaml.find("DOMAIN,a.com,DIRECT").expect(&yaml);
        let second = yaml.find("MATCH,PROXY").expect(&yaml);
        assert!(first < second, "{yaml}");
    }

    #[test]
    fn the_config_string_is_base64_of_the_yaml() {
        use base64::Engine as _;
        let document = EngineDocument::build(&base_config());
        let encoded = document.to_config_string().unwrap();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let yaml = String::from_utf8(decoded).unwrap();
        assert_eq!(yaml, document.to_yaml().unwrap());
    }

    #[test]
    fn a_rule_provider_is_emitted_under_its_name() {
        let config = RuntimeConfig {
            rule_providers: vec![starpod_core::domain::RuleProvider {
                name: "reject-list".to_string(),
                provider_type: "http".to_string(),
                url: Some("https://example.invalid/rules.list".to_string()),
                path: None,
                behavior: starpod_core::domain::RuleProviderBehavior::Domain,
                interval: Some(86400),
                format: None,
                payload: None,
                proxy: None,
            }],
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        assert!(yaml.contains("rule-providers:"), "{yaml}");
        assert!(yaml.contains("reject-list:"), "{yaml}");
        assert!(yaml.contains("behavior: domain"), "{yaml}");
        assert!(yaml.contains("interval: 86400"), "{yaml}");
        assert!(!yaml.contains("format:"), "{yaml}");
        assert!(!yaml.contains("proxy:"), "{yaml}");
    }

    #[test]
    fn the_new_provider_fields_are_emitted_with_the_engines_keys() {
        let config = RuntimeConfig {
            rule_providers: vec![
                starpod_core::domain::RuleProvider {
                    name: "oisd_big".to_string(),
                    provider_type: "http".to_string(),
                    url: Some("https://example.invalid/oisd.txt".to_string()),
                    path: Some("rule-providers/oisd_big.txt".to_string()),
                    behavior: starpod_core::domain::RuleProviderBehavior::Domain,
                    interval: None,
                    format: Some("text".to_string()),
                    payload: None,
                    proxy: Some("Airport / A".to_string()),
                },
                starpod_core::domain::RuleProvider {
                    name: "inline-one".to_string(),
                    provider_type: "inline".to_string(),
                    url: None,
                    path: None,
                    behavior: starpod_core::domain::RuleProviderBehavior::Classical,
                    interval: None,
                    format: Some("yaml".to_string()),
                    payload: Some(vec!["DOMAIN-SUFFIX,example.test".to_string()]),
                    proxy: None,
                },
            ],
            rules: vec![
                "RULE-SET,oisd_big,REJECT".to_string(),
                "RULE-SET,inline-one,PROXY,no-resolve".to_string(),
                "MATCH,PROXY".to_string(),
            ],
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        assert!(yaml.contains("format: text"), "{yaml}");
        assert!(yaml.contains("proxy: Airport / A"), "{yaml}");
        assert!(yaml.contains("payload:"), "{yaml}");
        assert!(yaml.contains("DOMAIN-SUFFIX,example.test"), "{yaml}");
        assert!(
            yaml.contains("RULE-SET,inline-one,PROXY,no-resolve"),
            "{yaml}"
        );
    }

    #[test]
    fn an_engine_managed_subscription_is_emitted_with_its_url() {
        let config = RuntimeConfig {
            subscriptions: vec![starpod_core::domain::EngineManagedSubscription {
                name: "airport".to_string(),
                url: "https://example.invalid/sub?token=abc".to_string(),
                interval_secs: 3600,
                proxy: None,
            }],
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        assert!(yaml.contains("subscriptions:"), "{yaml}");
        assert!(yaml.contains("name: airport"), "{yaml}");
        assert!(yaml.contains("interval: 3600"), "{yaml}");
    }
    // ------------------------------------------------------------------
    // The engine settings beyond the core ones
    // ------------------------------------------------------------------

    /// Every setting the app can write, set to a non-default value.
    fn every_engine_option() -> starpod_core::domain::EngineOptions {
        use starpod_core::domain::{
            DnsEnhancedMode, DnsSettings, EngineOptions, FindProcessMode, GeoSettings, GeodataMode,
            GeoxUrls, SnifferSettings, TunOptions, TunStack,
        };
        let mut hosts = std::collections::BTreeMap::new();
        hosts.insert("router.lan".to_string(), "192.168.1.1".to_string());
        let mut policy = std::collections::BTreeMap::new();
        policy.insert("+.corp.test".to_string(), vec!["10.0.0.53".to_string()]);
        let mut sniff = std::collections::BTreeMap::new();
        sniff.insert("TLS".to_string(), vec!["443".to_string()]);
        EngineOptions {
            tun: TunOptions {
                stack: Some(TunStack::Gvisor),
                strict_route: Some(true),
                auto_redirect: Some(true),
                auto_detect_interface: Some(true),
                endpoint_independent_nat: Some(true),
                route_address: vec!["0.0.0.0/0".to_string()],
                route_exclude_address: vec!["10.0.0.0/8".to_string()],
            },
            ipv6: Some(true),
            unified_delay: Some(true),
            tcp_concurrent: Some(true),
            find_process_mode: Some(FindProcessMode::Always),
            keep_alive_interval: Some(60),
            override_subscription_dns: true,
            dns: Some(DnsSettings {
                enable: Some(true),
                enhanced_mode: Some(DnsEnhancedMode::FakeIp),
                fake_ip_range: Some("198.18.0.1/16".to_string()),
                fake_ip_filter: vec!["*.lan".to_string()],
                nameserver: vec!["1.1.1.1".to_string()],
                nameserver_policy: policy,
                use_hosts: Some(true),
                ..DnsSettings::default()
            }),
            hosts,
            sniffer: Some(SnifferSettings {
                enable: Some(true),
                sniff,
                ..SnifferSettings::default()
            }),
            geo: GeoSettings {
                mode: Some(GeodataMode::Dat),
                geox_url: GeoxUrls {
                    geosite: Some("https://geo.example.test/geosite.dat".to_string()),
                    ..GeoxUrls::default()
                },
                auto_update: Some(true),
                update_interval_hours: Some(24),
            },
        }
    }

    #[test]
    fn a_default_configuration_writes_none_of_the_new_keys() {
        let body = EngineDocument::build(&base_config()).body;
        for key in [
            "ipv6",
            "unified-delay",
            "tcp-concurrent",
            "find-process-mode",
            "keep-alive-interval",
            "dns",
            "hosts",
            "sniffer",
            "geodata-mode",
            "geox-url",
            "geodata",
        ] {
            assert!(
                !body.contains_key(key),
                "{key} must not be written by default"
            );
        }
    }

    #[test]
    fn the_scalar_engine_settings_are_written_under_the_engines_keys() {
        let config = RuntimeConfig {
            engine: starpod_core::domain::EngineOptions {
                ipv6: Some(false),
                unified_delay: Some(true),
                tcp_concurrent: Some(true),
                find_process_mode: Some(starpod_core::domain::FindProcessMode::Off),
                keep_alive_interval: Some(45),
                ..Default::default()
            },
            ..base_config()
        };
        let body = EngineDocument::build(&config).body;
        assert_eq!(body["ipv6"], Value::Bool(false));
        assert_eq!(body["unified-delay"], Value::Bool(true));
        assert_eq!(body["tcp-concurrent"], Value::Bool(true));
        assert_eq!(body["find-process-mode"], Value::String("off".to_string()));
        assert_eq!(body["keep-alive-interval"], Value::from(45));
    }

    #[test]
    fn tun_extras_are_written_inside_an_enabled_tun_section() {
        let config = RuntimeConfig {
            tun: TunSettings {
                enabled: true,
                ..TunSettings::default()
            },
            engine: every_engine_option(),
            ..base_config()
        };
        let body = EngineDocument::build(&config).body;
        let tun = &body["tun"];
        assert_eq!(tun["enable"], Value::Bool(true));
        assert_eq!(tun["stack"], "gvisor");
        assert_eq!(tun["strict-route"], true);
        assert_eq!(tun["auto-redirect"], true);
        assert_eq!(tun["auto-detect-interface"], true);
        assert_eq!(tun["endpoint-independent-nat"], true);
        assert_eq!(tun["route-address"], serde_json::json!(["0.0.0.0/0"]));
        assert_eq!(
            tun["route-exclude-address"],
            serde_json::json!(["10.0.0.0/8"])
        );
    }

    #[test]
    fn tun_extras_are_left_out_while_tun_is_disabled() {
        let config = RuntimeConfig {
            engine: starpod_core::domain::EngineOptions {
                tun: starpod_core::domain::TunOptions {
                    stack: Some(starpod_core::domain::TunStack::System),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..base_config()
        };
        assert!(!EngineDocument::build(&config).body.contains_key("tun"));
    }

    #[test]
    fn the_dns_block_is_written_with_its_keys_and_keeps_unmodeled_ones() {
        let mut config = RuntimeConfig {
            engine: every_engine_option(),
            ..base_config()
        };
        if let Some(dns) = config.engine.dns.as_mut() {
            dns.extra.insert(
                "fake-ip-filter-mode".to_string(),
                Value::String("blacklist".to_string()),
            );
        }
        let body = EngineDocument::build(&config).body;
        let dns = &body["dns"];
        assert_eq!(dns["enable"], Value::Bool(true));
        assert_eq!(dns["enhanced-mode"], "fake-ip");
        assert_eq!(dns["fake-ip-range"], "198.18.0.1/16");
        assert_eq!(dns["fake-ip-filter"], serde_json::json!(["*.lan"]));
        assert_eq!(dns["nameserver"], serde_json::json!(["1.1.1.1"]));
        assert_eq!(
            dns["nameserver-policy"]["+.corp.test"],
            serde_json::json!(["10.0.0.53"])
        );
        assert_eq!(dns["use-hosts"], Value::Bool(true));
        assert_eq!(dns["fake-ip-filter-mode"], "blacklist");
        assert!(dns.get("fallback").is_none());
    }

    #[test]
    fn hosts_sniffer_and_geodata_are_written_under_the_engines_keys() {
        let config = RuntimeConfig {
            engine: every_engine_option(),
            ..base_config()
        };
        let body = EngineDocument::build(&config).body;
        assert_eq!(body["hosts"]["router.lan"], "192.168.1.1");
        assert_eq!(body["sniffer"]["enable"], Value::Bool(true));
        assert_eq!(
            body["sniffer"]["sniff"]["TLS"]["ports"],
            serde_json::json!(["443"])
        );
        assert_eq!(body["geodata-mode"], Value::Bool(true));
        assert_eq!(
            body["geox-url"]["geosite"],
            "https://geo.example.test/geosite.dat"
        );
        assert_eq!(body["geodata"]["auto-update"], Value::Bool(true));
        assert_eq!(body["geodata"]["auto-update-interval"], Value::from(24));
    }

    #[test]
    fn every_engine_key_the_emitter_writes_is_in_the_engine_key_list() {
        let config = RuntimeConfig {
            engine: every_engine_option(),
            ..full_config()
        };
        let document = EngineDocument::build(&config);
        for key in document.body.keys() {
            assert!(
                ENGINE_KEYS.contains(&key.as_str()),
                "{key:?} is not in ENGINE_KEYS"
            );
        }
    }

    #[test]
    fn the_engine_options_survive_the_yaml_round_trip() {
        let config = RuntimeConfig {
            engine: every_engine_option(),
            ..base_config()
        };
        let document = EngineDocument::build(&config);
        let yaml = document.to_yaml().expect("serializes");
        let back: Value = serde_saphyr::from_str(&yaml).expect("parses");
        assert_eq!(back, document.to_value());
    }
}
