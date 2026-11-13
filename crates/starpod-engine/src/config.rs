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
    /// The engine's REST API secret.
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

        let tun = tun_section(&config.tun);
        if let Some(tun) = tun {
            body.insert("tun".to_string(), tun);
        }

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

    /// Attach the controller address and the API secret.
    ///
    /// The secret is a secret: it is written into a file the engine reads
    /// and into nothing else. It is never logged, never shown in the UI, and
    /// never included in a configuration the user exports.
    pub fn with_controller(mut self, controller: &str, secret: &str) -> Self {
        self.body.insert(
            Self::CONTROLLER_KEY.to_string(),
            Value::String(controller.to_string()),
        );
        if !secret.is_empty() {
            self.body.insert(
                Self::SECRET_KEY.to_string(),
                Value::String(secret.to_string()),
            );
        }
        self
    }

    /// The document as a JSON value, for tests and comparisons.
    pub fn to_value(&self) -> Value {
        Value::Object(self.body.clone())
    }

    /// The document as YAML.
    ///
    /// The engine parses YAML, so this is the format it must be given.
    pub fn to_yaml(&self) -> Result<String, serde_saphyr::SerializeError> {
        serde_saphyr::to_string(&self.body)
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

/// Build the engine's `tun:` section, or `None` when TUN is off entirely.
fn tun_section(tun: &starpod_core::domain::TunSettings) -> Option<Value> {
    if tun.is_disabled() {
        return None;
    }
    let mut section = Map::new();
    section.insert("enable".to_string(), Value::Bool(tun.enabled));
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
        if key == "name" || key == "type" || key == "proxies" {
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
                raw: Map::new(),
            }],
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        assert!(yaml.contains("type: select"), "{yaml}");
        assert!(yaml.contains("- HK 01"), "{yaml}");
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

        let secured =
            EngineDocument::build(&base_config()).with_controller("127.0.0.1:9090", "s3cret");
        assert!(secured.body.contains_key("external-controller"));
        assert_eq!(
            secured.body.get("secret").unwrap(),
            &Value::String("s3cret".into())
        );
    }

    #[test]
    fn an_empty_secret_is_not_written() {
        let document = EngineDocument::build(&base_config()).with_controller("127.0.0.1:9090", "");
        assert!(!document.body.contains_key("secret"));
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
            }],
            ..base_config()
        };
        let yaml = EngineDocument::build(&config).to_yaml().unwrap();
        assert!(yaml.contains("rule-providers:"), "{yaml}");
        assert!(yaml.contains("reject-list:"), "{yaml}");
        assert!(yaml.contains("behavior: domain"), "{yaml}");
        assert!(yaml.contains("interval: 86400"), "{yaml}");
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
}
