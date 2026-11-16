//! The body parsing pipeline: bytes in, a validated payload out.
//!
//! This is ARCHITECTURE §3.2.2, implemented as a pure function:
//!
//! ```text
//! bytes → sniff format → YAML parse → shape check → per-entry validation → SubscriptionPayload
//! ```
//!
//! # Where namespacing happens
//!
//! **Here, in the pipeline — not in the provider.** The provider returns
//! bytes and nothing else; [`parse_payload`] is handed the subscription's
//! label and is the single place where a node's name becomes
//! `<label> / <node name>`. The provider stays a transport, the pipeline
//! owns the app's naming rules, and there is exactly one place to look
//! when a name looks wrong.
//!
//! The namespace is `<label> / <name>` — the separator is chosen so it
//! cannot collide with a node name in practice and so the engine's own
//! name-keyed maps (`meow-config-0.29.0/src/lib.rs:2142-2151`, the
//! `proxies:` leaves are keyed by their YAML `name:`) keep the label
//! visible in the UI.
//!
//! # Why the app accepts base64 when the engine does not
//!
//! The engine's own subscription parser requires a `proxies:` key and
//! will not unwrap base64
//! (`meow-config-0.29.0/src/subscription.rs:336-340` —
//! `"subscription missing 'proxies' key"`). The *app* accepts
//! base64-wrapped YAML as well, because panels routinely serve the
//! subscription that way and the app is the layer that has to cope with
//! what publishers emit.
//!
//! # What "valid" means here
//!
//! Validation mirrors what the engine will accept, so that a node the app
//! accepts is a node the engine can build. The rules, and the engine
//! source that establishes each one:
//!
//! | check | engine source |
//! |-------|---------------|
//! | the protocol is one the engine dispatches on | `meow-config-0.29.0/src/proxy_parser.rs:326-539` |
//! | `server` is a non-empty string | `proxy_parser.rs:331-334` (ss), `:424-427` (trojan), `:1952-1955` (vless), `:3071-3074` (vmess) |
//! | `port` is a `u64` that fits `u16` and is non-zero | `proxy_parser.rs:20-33` (`required_port`) |
//! | the protocol's credential is present | `proxy_parser.rs:336-339` (ss `password`), `:429-432` (trojan `password`), `:1957-1959` (vless `uuid`), `:3076-3078` (vmess `uuid`), `:667-669` (snell `psk`), `:1282-1284` (anytls `password`), `:1416-1418` (hysteria2 `password`) |
//! | an `ss` node must not name an external SIP003 plugin | `meow-config-0.29.0/src/proxy_parser.rs:257-268` (`node_selects_external_plugin`), `meow-proxy-0.29.0/src/shadowsocks_adapter.rs:812-838` (the built-in names) |

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use std::collections::BTreeSet;

use starpod_core::domain::{
    DnsSettings, LogLevel, RuleProvider, RuleProviderBehavior, SubscriptionSettings,
};
use starpod_core::rules::parse::parse_line;
use starpod_core::rules::translate::translate;
use starpod_core::{Node, ProxyGroup, ProxyMode, RuleSpec, RuleTarget, SubscriptionPayload};

use crate::error::SubscriptionError;

/// The format the body was found to be in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadFormat {
    /// Clash/Mihomo YAML — the format the engine itself requires.
    ///
    /// `meow-config-0.29.0/src/subscription.rs:330-343`.
    Yaml,
    /// Base64 wrapping Clash/Mihomo YAML — what many panels serve, and
    /// what the engine does *not* accept.
    ///
    /// `meow-config-0.29.0/src/subscription.rs:278-288` — the engine
    /// fetches the body and hands it straight to the YAML parser.
    Base64Yaml,
}

impl PayloadFormat {
    /// A short, stable label for the UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Yaml => "yaml",
            Self::Base64Yaml => "base64-yaml",
        }
    }
}

/// The outcome of parsing a subscription body.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedBody {
    /// The payload.
    pub payload: SubscriptionPayload,
    /// The format the body was in.
    pub format: PayloadFormat,
    /// The nodes that were dropped during validation, with the reason.
    ///
    /// The payload itself only carries a count; this list is what the UI
    /// shows the user when they ask why a node is missing.
    pub dropped: Vec<DroppedEntry>,
}

/// A node that did not survive validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DroppedEntry {
    /// The node's name, when it had one.
    pub name: Option<String>,
    /// Why the node was dropped.
    pub reason: String,
}

/// The separator between the subscription label and the node name.
pub const NAMESPACE_SEPARATOR: &str = " / ";

/// The node protocols the engine dispatches on.
///
/// From `meow-config-0.29.0/src/proxy_parser.rs:326-539`:
///
/// | protocol | line | credential the engine requires |
/// |----------|------|-------------------------------|
/// | `ss` | 330 | `password`, `cipher` |
/// | `trojan` | 423 | `password` |
/// | `vless` | 474 | `uuid` |
/// | `http` | 480 | none |
/// | `socks5` | 486 | none |
/// | `direct` | 492 | none, and no server or port either (see [`validate_node`]) |
/// | `anytls` | 498 | `password` |
/// | `hysteria2` | 504 | `password` |
/// | `vmess` | 510 | `uuid` |
/// | `snell` | 517 | `psk` |
/// | `trusttunnel` | 328 | feature-gated in the engine |
///
/// `trusttunnel` is listed for completeness but is **not** accepted: the
/// engine's dispatch is behind the `trusttunnel` cargo feature
/// (`meow-config-0.29.0/src/proxy_parser.rs:524-525` errors with
/// `feature_gated_proxy_type`), and the shipped release builds do not
/// enable it (`meow-app-0.29.0/Cargo.toml`'s `full` feature list has no
/// `trusttunnel`). A node the engine cannot build is a node the app must
/// not offer.
pub const KNOWN_PROTOCOLS: &[&str] = &[
    "ss",
    "trojan",
    "vless",
    "vmess",
    "hysteria2",
    "anytls",
    "snell",
    "http",
    "socks5",
    "direct",
];

/// The SIP003 plugin names the engine implements in-process.
///
/// From `meow-proxy-0.29.0/src/shadowsocks_adapter.rs:812-838`:
/// `obfs`, `simple-obfs`, `v2ray-plugin`, `gost-plugin`, `shadow-tls`,
/// `restls`, `jls`, `kcptun`, `ech-tls-tunnel`. Any other plugin name is
/// spawned as an external executable, which is why the engine drops such a
/// node outright
/// (`meow-config-0.29.0/src/subscription.rs:370-380`).
pub const BUILTIN_SS_PLUGINS: &[&str] = &[
    "obfs",
    "simple-obfs",
    "v2ray-plugin",
    "gost-plugin",
    "shadow-tls",
    "restls",
    "jls",
    "kcptun",
    "ech-tls-tunnel",
];

/// The proxy group types the engine builds.
///
/// From `meow-config-0.29.0/src/proxy_parser.rs:3406-3467`: `select`,
/// `url-test`, `fallback`, `load-balance`, `relay`. Anything else is
/// `"unsupported group type"`.
pub const KNOWN_GROUP_TYPES: &[&str] = &["select", "url-test", "fallback", "load-balance", "relay"];

/// Parse a subscription body into a validated payload.
///
/// The `subscription_id` and `label` are the app's own: the id ties the
/// payload to the stored subscription, and the label namespaces the node
/// names so a subscription cannot hijack a user-defined node or group.
///
/// This is a pure function. It performs no I/O and reads no clock.
pub fn parse_payload(
    body: &[u8],
    subscription_id: &str,
    label: &str,
) -> Result<ParsedBody, SubscriptionError> {
    let (text, format) = sniff(body)?;
    let document = parse_yaml(&text)?;

    let extracted = extract(&document, label)?;

    if extracted.nodes.is_empty() {
        if extracted.dropped.is_empty() {
            return Err(SubscriptionError::EmptyAfterValidation);
        }
        return Err(SubscriptionError::EveryNodeDropped {
            dropped: extracted.dropped.len(),
            reason: extracted.dropped[0].reason.clone(),
        });
    }

    report_dropped(subscription_id, &extracted.dropped);

    Ok(ParsedBody {
        payload: SubscriptionPayload {
            subscription_id: subscription_id.to_string(),
            nodes: extracted.nodes,
            groups: extracted.groups,
            rules: extracted.rules,
            rule_providers: extracted.rule_providers,
            settings: extracted.settings,
            dropped_entries: extracted.dropped.len(),
            dns: extracted.dns,
        },
        format,
        dropped: extracted.dropped,
    })
}

/// Log the entries that were dropped.
///
/// A dropped entry is not an error — the subscription still loads — but it
/// is a *silent* loss of a node the provider offered, and a node that
/// keeps disappearing is a support question. The count goes into the
/// payload (`dropped_entries`); the individual reasons go into the log.
fn report_dropped(subscription_id: &str, dropped: &[DroppedEntry]) {
    for entry in dropped {
        tracing::warn!(
            subscription_id,
            node = entry.name.as_deref().unwrap_or("<unnamed>"),
            reason = entry.reason.as_str(),
            "dropped an entry from a subscription"
        );
    }
}

/// Step ① — sniff the format.
///
/// The body is either Clash YAML or base64-wrapped Clash YAML. The test
/// is the *shape*, not a guess: a document with a `proxies:` or
/// `proxy-groups:` key is YAML.
fn sniff(body: &[u8]) -> Result<(String, PayloadFormat), SubscriptionError> {
    let text = std::str::from_utf8(body).map_err(|error| {
        SubscriptionError::InvalidBody(format!("the body is not UTF-8: {error}"))
    })?;

    // The YAML path first: the engine's own parser requires a `proxies:`
    // key, so a document with one is YAML whether or not it also happens
    // to be valid base64.
    if looks_like_clash_yaml(text) {
        return Ok((text.to_string(), PayloadFormat::Yaml));
    }

    // Otherwise: base64, then YAML again. A base64 body that decodes to
    // something that is *not* a Clash configuration is not a subscription
    // either, and it gets the same "neither" verdict as a plain-text
    // body — saying "the base64 decoded but was not UTF-8" would blame the
    // encoding for a body that simply is not a subscription.
    if let Some(decoded) = decode_base64(text)
        && let Ok(decoded) = String::from_utf8(decoded)
        && looks_like_clash_yaml(&decoded)
    {
        return Ok((decoded, PayloadFormat::Base64Yaml));
    }

    Err(SubscriptionError::InvalidBody(
        "the body is neither a Clash configuration nor base64-wrapped Clash configuration: \
         it has neither a 'proxies:' nor a 'proxy-groups:' key"
            .to_string(),
    ))
}

/// Whether the text looks like a Clash configuration.
///
/// "Looks like" is deliberately loose: a YAML *syntax* error in a document
/// that clearly means to be one is a parse error, not a "this is not a
/// subscription" verdict, and the two deserve different messages.
///
/// The key must be a **top-level** one — the engine reads `proxies:` and
/// `proxy-groups:` off the root mapping
/// (`meow-config-0.29.0/src/subscription.rs:330-343`, where `root` is
/// `as_object()` and the lookups are on that mapping), so a `proxies:`
/// nested inside another key does not count.
fn looks_like_clash_yaml(text: &str) -> bool {
    // A cheap pre-filter: the whole document is small enough to scan, and
    // this avoids parsing a megabyte of base64 as YAML.
    if text.len() > 4 * 1024 * 1024 {
        return false;
    }
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" {
            continue;
        }
        // An indented line is not a top-level key.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        // A top-level key. `proxies:` and `proxy-groups:` are the two the
        // engine's own parser looks for
        // (`meow-config-0.29.0/src/subscription.rs:337-343` for `proxies:`
        // and `:330-332` for the root mapping).
        if let Some((key, _)) = trimmed.split_once(':') {
            let key = key.trim();
            if key == "proxies" || key == "proxy-groups" {
                return true;
            }
        }
    }
    false
}

/// Decode standard base64, tolerating the whitespace some panels wrap it in.
fn decode_base64(text: &str) -> Option<Vec<u8>> {
    let cleaned: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if cleaned.is_empty() {
        return None;
    }
    // Base64 of an empty document is empty; that is not a subscription.
    if !cleaned
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' || byte == b'=')
    {
        return None;
    }
    BASE64_STANDARD.decode(cleaned).ok()
}
/// Step ② — parse the YAML.
///
/// `serde-saphyr` is the engine's own YAML library
/// (`meow-config-0.29.0/Cargo.toml` pins `serde-saphyr = "=2.0.0"`), so
/// the app and the engine read the same document the same way. The
/// options below mirror the engine's `yaml_options()`
/// (`meow-config-0.29.0/src/lib.rs:858-866`): strict booleans, unsupported
/// tags rejected, non-finite floats passed through, and a nesting-depth
/// budget.
///
/// The depth budget is the engine's own value — 32
/// (`meow-common-0.29.0/src/yaml.rs:36`, `MAX_DEPTH`) — because that is
/// the depth contract the engine enforces on every document it ingests,
/// subscription payloads included
/// (`meow-config-0.29.0/src/lib.rs:864`, and the engine's own
/// subscription parser applies the same limit separately at
/// `meow-config-0.29.0/src/subscription.rs:321-325`).
fn parse_yaml(text: &str) -> Result<serde_json::Value, SubscriptionError> {
    let options = serde_saphyr::options! {
        strict_booleans: true,
        reject_unsupported_tags: true,
        ignore_binary_tag_for_string: true,
        non_finite_float_policy: serde_saphyr::NonFiniteFloatPolicy::PassThrough,
        budget: serde_saphyr::budget! { max_depth: 32 },
    };
    serde_saphyr::from_str_with_options::<serde_json::Value>(text, options).map_err(|error| {
        SubscriptionError::InvalidBody(format!("the body is not valid YAML: {error}"))
    })
}

/// What the pipeline extracts from a parsed subscription document.
struct Extracted {
    nodes: Vec<Node>,
    groups: Vec<ProxyGroup>,
    rules: Vec<RuleSpec>,
    rule_providers: Vec<RuleProvider>,
    settings: SubscriptionSettings,
    dns: Option<DnsSettings>,
    dropped: Vec<DroppedEntry>,
}

/// Step ③ and ④ — the shape check and the per-entry validation.
fn extract(document: &serde_json::Value, label: &str) -> Result<Extracted, SubscriptionError> {
    let Some(mapping) = document.as_object() else {
        return Err(SubscriptionError::InvalidBody(
            "the subscription root is not a mapping".to_string(),
        ));
    };

    // The shape check. The engine requires a `proxies:` key
    // (`meow-config-0.29.0/src/subscription.rs:336-340`); the app also
    // accepts a `proxy-groups:`-only document, because a group with no
    // nodes is not a useful subscription but it is still a *legible* one
    // and the "no nodes survived" check below gives the better message.
    if !mapping.contains_key("proxies") && !mapping.contains_key("proxy-groups") {
        let keys: Vec<&str> = mapping.keys().map(String::as_str).collect();
        return Err(SubscriptionError::InvalidBody(format!(
            "the subscription has neither a 'proxies:' nor a 'proxy-groups:' key; found: {keys:?}"
        )));
    }

    let mut nodes = Vec::new();
    let mut dropped = Vec::new();
    // The namespaced names of the proxies and groups that were dropped, so
    // references to them can be pruned once every entry has been read.
    let mut dropped_names = BTreeSet::new();

    match mapping.get("proxies") {
        None => {}
        Some(serde_json::Value::Array(entries)) => {
            for entry in entries {
                match validate_node(entry, label) {
                    Ok(node) => nodes.push(node),
                    Err(reason) => {
                        let name = entry_name(entry);
                        remember_dropped(&mut dropped_names, label, name.as_deref());
                        dropped.push(DroppedEntry { name, reason });
                    }
                }
            }
        }
        Some(_) => {
            return Err(SubscriptionError::InvalidBody(
                "'proxies:' is not a list".to_string(),
            ));
        }
    }

    let mut groups = match mapping.get("proxy-groups") {
        None => Vec::new(),
        Some(serde_json::Value::Array(entries)) => {
            let mut groups = Vec::new();
            for entry in entries {
                match validate_group(entry, label) {
                    Ok(group) => groups.push(group),
                    Err(reason) => {
                        let name = entry_name(entry);
                        remember_dropped(&mut dropped_names, label, name.as_deref());
                        dropped.push(DroppedEntry { name, reason });
                    }
                }
            }
            groups
        }
        Some(_) => {
            return Err(SubscriptionError::InvalidBody(
                "'proxy-groups:' is not a list".to_string(),
            ));
        }
    };

    expand_include_all(&mut groups, &nodes, label);

    // The names this subscription defines, already namespaced. A rule target,
    // a provider's download proxy or a RULE-SET that names one of them is
    // rewritten to the namespaced name, so it points at the same group or
    // node the subscription declared (see `retarget_rule`).
    let own_names: BTreeSet<String> = nodes
        .iter()
        .map(|node| node.name.clone())
        .chain(groups.iter().map(|group| group.name.clone()))
        .collect();

    let (rule_providers, dropped_providers) =
        extract_rule_providers(mapping, label, &own_names, &mut dropped)?;

    let mut rules = match mapping.get("rules") {
        None => Vec::new(),
        Some(serde_json::Value::Array(entries)) => {
            let mut rules = Vec::new();
            for entry in entries {
                let Some(line) = entry.as_str() else {
                    dropped.push(DroppedEntry {
                        name: None,
                        reason: "a rule is not a string".to_string(),
                    });
                    continue;
                };
                match parse_line(line) {
                    Ok(mut rule) => {
                        // A RULE-SET whose provider the app dropped could
                        // never validate, so it goes with the provider.
                        if let RuleSpec::RuleSet { provider, .. } = &rule
                            && dropped_providers.contains(provider)
                        {
                            dropped.push(DroppedEntry {
                                name: Some(line.to_string()),
                                reason: format!(
                                    "it uses the rule provider “{provider}”, which was dropped"
                                ),
                            });
                            continue;
                        }
                        retarget_rule(&mut rule, label, &own_names);
                        rules.push(rule);
                    }
                    // A rule the app cannot understand is dropped, not
                    // fatal: the engine would reject it too, but one bad
                    // rule must not cost the user every node in the
                    // subscription.
                    Err(reason) => dropped.push(DroppedEntry {
                        name: Some(line.to_string()),
                        reason: format!("the rule could not be parsed: {reason}"),
                    }),
                }
            }
            rules
        }
        Some(_) => {
            return Err(SubscriptionError::InvalidBody(
                "'rules:' is not a list".to_string(),
            ));
        }
    };

    prune_dangling(
        label,
        &own_names,
        &dropped_names,
        &mut groups,
        &mut rules,
        &mut dropped,
    );

    let settings = read_settings(mapping);

    // The subscription's DNS block is kept whole, so the user's choice to use it
    // (or not) is the only thing that decides whether it reaches the engine.
    let dns = match mapping.get("dns") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => match serde_json::from_value::<DnsSettings>(value.clone()) {
            Ok(mut dns) => {
                retarget_dns(&mut dns, label, &own_names, &dropped_names);
                Some(dns)
            }
            Err(error) => {
                dropped.push(DroppedEntry {
                    name: None,
                    reason: format!("the 'dns:' block could not be read: {error}"),
                });
                None
            }
        },
    };

    Ok(Extracted {
        nodes,
        groups,
        rules,
        rule_providers,
        settings,
        dns,
        dropped,
    })
}

/// Read the subscription-level settings.
///
/// These are the top-level keys the engine's `RawConfig` accepts
/// (`meow-config-0.29.0/src/raw.rs:184-191`): `port`, `socks-port`,
/// `mixed-port`, `allow-lan`, `bind-address`, `mode`, `log-level`. The
/// subscription's values are a *fallback* for the user's own — the merge
/// in `starpod-core` resolves them, and the user's always win.
fn read_settings(mapping: &serde_json::Map<String, serde_json::Value>) -> SubscriptionSettings {
    let port = |key: &str| {
        mapping
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
    };
    SubscriptionSettings {
        mixed_port: port("mixed-port"),
        socks_port: port("socks-port"),
        port: port("port"),
        allow_lan: mapping
            .get("allow-lan")
            .and_then(serde_json::Value::as_bool),
        bind_address: mapping
            .get("bind-address")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        mode: mapping
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .and_then(ProxyMode::from_str_exact),
        log_level: mapping
            .get("log-level")
            .and_then(serde_json::Value::as_str)
            .and_then(LogLevel::from_str_exact),
    }
}

/// Read the top-level `rule-providers:` mapping.
///
/// Returns the providers the app keeps, and the names of the ones it dropped.
/// The engine's own `path:` is never used: each `http` provider gets
/// `rule-providers/<name>.<ext>` (see [`provider_file_stem`]), so a
/// subscription cannot choose where the engine reads or writes.
fn extract_rule_providers(
    mapping: &serde_json::Map<String, serde_json::Value>,
    label: &str,
    own_names: &BTreeSet<String>,
    dropped: &mut Vec<DroppedEntry>,
) -> Result<(Vec<RuleProvider>, BTreeSet<String>), SubscriptionError> {
    let entries = match mapping.get("rule-providers") {
        None | Some(serde_json::Value::Null) => return Ok((Vec::new(), BTreeSet::new())),
        Some(serde_json::Value::Object(entries)) => entries,
        Some(_) => {
            return Err(SubscriptionError::InvalidBody(
                "'rule-providers:' is not a mapping".to_string(),
            ));
        }
    };

    let mut kept = Vec::new();
    let mut dropped_names = BTreeSet::new();
    for (name, value) in entries {
        match validate_rule_provider(name, value, label, own_names) {
            Ok(provider) => kept.push(provider),
            Err(reason) => {
                tracing::warn!(
                    provider = name.as_str(),
                    reason = reason.as_str(),
                    "dropped a rule provider from a subscription"
                );
                dropped_names.insert(name.clone());
                dropped.push(DroppedEntry {
                    name: Some(name.clone()),
                    reason,
                });
            }
        }
    }
    Ok((kept, dropped_names))
}

/// Validate one rule provider against what the engine accepts.
///
/// Accepts `type: http` (with an `http`/`https` URL) and `type: inline`.
/// A `file` provider is refused: a subscription must never point the engine
/// at a local file. Engine schema: see the comment on
/// `starpod_core::domain::RuleProvider`.
fn validate_rule_provider(
    name: &str,
    value: &serde_json::Value,
    label: &str,
    own_names: &BTreeSet<String>,
) -> Result<RuleProvider, String> {
    if name.trim().is_empty() {
        return Err("the rule provider has no name".to_string());
    }
    let Some(object) = value.as_object() else {
        return Err("the rule provider is not a mapping".to_string());
    };

    let provider_type = optional_string(object, "type")?
        .ok_or_else(|| "the rule provider has no type".to_string())?;
    match provider_type.as_str() {
        "http" | "inline" => {}
        "file" => {
            return Err(
                "a rule provider of type 'file' is not accepted: a subscription cannot point \
                 the engine at a local file"
                    .to_string(),
            );
        }
        other => {
            return Err(format!(
                "'{other}' is not a rule provider type the engine supports"
            ));
        }
    }

    let behavior = match optional_string(object, "behavior")?.as_deref() {
        Some("domain") => RuleProviderBehavior::Domain,
        Some("ipcidr") => RuleProviderBehavior::IpCidr,
        Some("classical") => RuleProviderBehavior::Classical,
        Some(other) => {
            return Err(format!(
                "'{other}' is not a rule provider behavior the engine supports"
            ));
        }
        None => return Err("the rule provider has no behavior".to_string()),
    };

    let format = match optional_string(object, "format")?.as_deref() {
        None | Some("yaml") => "yaml",
        Some("text") => "text",
        Some("mrs") => "mrs",
        Some(other) => {
            return Err(format!(
                "'{other}' is not a rule provider format the engine supports"
            ));
        }
    };

    let mut provider = RuleProvider {
        name: name.to_string(),
        provider_type: provider_type.clone(),
        url: None,
        path: None,
        behavior,
        interval: None,
        format: Some(format.to_string()),
        payload: None,
        proxy: None,
    };

    if provider_type == "http" {
        let url = optional_string(object, "url")?
            .map(|url| url.trim().to_string())
            .filter(|url| !url.is_empty())
            .ok_or_else(|| "the rule provider has no url".to_string())?;
        let lower = url.to_ascii_lowercase();
        if !(lower.starts_with("http://") || lower.starts_with("https://"))
            || url.chars().any(char::is_whitespace)
        {
            return Err("the rule provider's url is not an http or https address".to_string());
        }
        provider.url = Some(url);
        provider.path = Some(format!(
            "rule-providers/{}.{}",
            provider_file_stem(name),
            provider_extension(format)
        ));
        provider.interval = optional_u64(object, "interval")?;
        provider.proxy = optional_string(object, "proxy")?
            .map(|proxy| proxy.trim().to_string())
            .filter(|proxy| !proxy.is_empty())
            .map(|proxy| {
                if proxy.eq_ignore_ascii_case("DIRECT") {
                    "DIRECT".to_string()
                } else {
                    retarget_name(&proxy, label, own_names)
                }
            });
    } else {
        // An inline provider's rules are the payload itself. The engine
        // refuses an interval above zero for these, so none is kept.
        let entries = object
            .get("payload")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| "the inline rule provider has no payload list".to_string())?;
        let mut rules = Vec::with_capacity(entries.len());
        for entry in entries {
            let Some(rule) = entry.as_str() else {
                return Err(
                    "an inline rule provider's payload holds something that is not a string"
                        .to_string(),
                );
            };
            rules.push(rule.to_string());
        }
        provider.payload = Some(rules);
    }

    Ok(provider)
}

/// A string field of a subscription mapping. `Ok(None)` when it is absent or
/// null; an error when it is present but not a string.
fn optional_string(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<String>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("'{key}' is not a string")),
    }
}

/// A whole-number field of a subscription mapping, with the same rules as
/// [`optional_string`].
fn optional_u64(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<u64>, String> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("'{key}' is not a whole number of seconds")),
    }
}

/// The file stem for a provider's on-disk cache, from its name.
///
/// Characters outside `[A-Za-z0-9._-]` become `_`. When that changes the name
/// (or the name starts with a dot, or is longer than 64 characters), a hash
/// of the whole name is appended, so two different names cannot end up on the
/// same file: `a b` and `a_b` stay apart.
fn provider_file_stem(name: &str) -> String {
    let mut cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    if cleaned.starts_with('.') {
        cleaned.replace_range(0..1, "_");
    }
    if cleaned == name {
        cleaned
    } else {
        format!("{cleaned}-{:08x}", fnv1a_32(name))
    }
}

/// The extension a provider's cache file gets, from its format.
fn provider_extension(format: &str) -> &'static str {
    match format {
        "text" => "txt",
        "mrs" => "mrs",
        _ => "yaml",
    }
}

/// 32-bit FNV-1a. Stable across builds and Rust versions, unlike
/// `DefaultHasher`, so a cache file name does not move between releases.
fn fnv1a_32(text: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in text.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Rewrite a rule's target, when it names one of this subscription's own
/// groups or nodes, to the namespaced name those were given.
fn retarget_rule(rule: &mut RuleSpec, label: &str, own_names: &BTreeSet<String>) {
    match rule {
        RuleSpec::Simple { target, .. }
        | RuleSpec::Match { target }
        | RuleSpec::Logic { target, .. }
        | RuleSpec::RuleSet { target, .. } => {
            *target = retarget_target(target, label, own_names);
        }
        RuleSpec::SubRule { .. } => {}
    }
}

/// The target a rule sends traffic to, after namespacing. A built-in outbound
/// (`DIRECT`, `REJECT`, …) and a name the subscription does not define are
/// left as written: the latter may name a user group, which is valid.
fn retarget_target(target: &RuleTarget, label: &str, own_names: &BTreeSet<String>) -> RuleTarget {
    if target.builtin_kind().is_some() {
        return target.clone();
    }
    RuleTarget::new(retarget_name(target.as_str(), label, own_names))
}

/// A name, namespaced when the subscription defines it, else unchanged.
fn retarget_name(name: &str, label: &str, own_names: &BTreeSet<String>) -> String {
    let namespaced = namespace(label, name);
    if own_names.contains(&namespaced) {
        namespaced
    } else {
        name.to_string()
    }
}

/// Bring a payload cached by an earlier build up to what [`parse_payload`]
/// makes today, without the original body.
///
/// The cache keeps the parsed payload, not the response, so a fix to the
/// parser would otherwise reach a subscription only at its next refresh, and
/// until then the engine could refuse the configuration. Both steps are safe
/// to repeat on a payload that has them already: an `include-all` group has
/// its keys removed once expanded, and a nameserver already in the engine's
/// syntax carries no `#<name>` tag. `label` is the subscription's label, the
/// namespace its nodes were given.
pub fn upgrade_cached_payload(payload: &mut SubscriptionPayload, label: &str) {
    expand_include_all(&mut payload.groups, &payload.nodes, label);
    let own_names: BTreeSet<String> = payload
        .nodes
        .iter()
        .map(|node| node.name.clone())
        .chain(payload.groups.iter().map(|group| group.name.clone()))
        .collect();
    if let Some(dns) = payload.dns.as_mut() {
        retarget_dns(dns, label, &own_names, &BTreeSet::new());
    }
}

/// Fill a group that asks for `include-all` with this subscription's nodes.
///
/// In mihomo, `include-all: true` adds every proxy and every provider's
/// proxies to the group, narrowed by `filter`, `exclude-filter` and
/// `exclude-type`. The engine reads `include-all` as providers only, and
/// takes top-level proxies through `include-all-proxies`
/// (meow `website/guide/proxy-groups.md`). The app does not pass providers
/// on, and letting the engine include every top-level proxy would also pull
/// in other subscriptions' nodes and the user's own. So the group is
/// expanded here, from this subscription's nodes only, and the keys are
/// removed so the engine does not apply them a second time.
///
/// `filter` and `exclude-filter` are regular expressions matched against the
/// node name as the provider wrote it; several can be joined with a
/// backtick, as in mihomo. A pattern that does not compile leaves the group
/// as it was.
fn expand_include_all(groups: &mut [ProxyGroup], nodes: &[Node], label: &str) {
    const KEYS: [&str; 5] = [
        "include-all",
        "include-all-proxies",
        "filter",
        "exclude-filter",
        "exclude-type",
    ];
    for group in groups.iter_mut() {
        let flag =
            |key: &str| group.raw.get(key).and_then(serde_json::Value::as_bool) == Some(true);
        if !flag("include-all") && !flag("include-all-proxies") {
            continue;
        }
        let patterns = |key: &str| -> Result<Vec<regex::Regex>, regex::Error> {
            group
                .raw
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(|text| {
                    text.split('`')
                        .filter(|part| !part.is_empty())
                        .map(regex::Regex::new)
                        .collect()
                })
                .unwrap_or_else(|| Ok(Vec::new()))
        };
        let (Ok(include), Ok(exclude)) = (patterns("filter"), patterns("exclude-filter")) else {
            continue;
        };
        let excluded_types: Vec<String> = match group.raw.get("exclude-type") {
            Some(serde_json::Value::String(text)) => text
                .split('|')
                .map(|kind| kind.trim().to_ascii_lowercase())
                .collect(),
            Some(serde_json::Value::Array(kinds)) => kinds
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|kind| kind.trim().to_ascii_lowercase())
                .collect(),
            _ => Vec::new(),
        };
        let prefix = format!("{label}{NAMESPACE_SEPARATOR}");
        for node in nodes {
            let original = node.name.strip_prefix(&prefix).unwrap_or(&node.name);
            let wanted = (include.is_empty() || include.iter().any(|re| re.is_match(original)))
                && !exclude.iter().any(|re| re.is_match(original))
                && !excluded_types.contains(&node.protocol.to_ascii_lowercase());
            if wanted && !group.members.contains(&node.name) {
                group.members.push(node.name.clone());
            }
        }
        for key in KEYS {
            group.raw.remove(key);
        }
        group.raw.insert(
            "proxies".to_string(),
            serde_json::Value::Array(
                group
                    .members
                    .iter()
                    .map(|member| serde_json::Value::String(member.clone()))
                    .collect(),
            ),
        );
    }
}

/// Rewrite the outbound each nameserver in a subscription's DNS block names.
///
/// Subscriptions are written for mihomo, where `<server>#<name>` sends the
/// query through the proxy or group `<name>`. The engine reads `#` on a
/// `tls://` or `https://` entry as the TLS server name instead, and takes the
/// outbound in a `?proxy=<name>` parameter (`meow-dns-0.29.0/src/upstream.rs`,
/// `NameServerEntry`). A mihomo entry such as `https://8.8.8.8/dns-query#DIRECT`
/// would therefore make every lookup fail its TLS handshake. On top of that,
/// the subscription's groups are namespaced, so `#<group>` no longer names
/// anything.
///
/// For each entry whose `#` names `DIRECT` or one of this subscription's own
/// nodes or groups:
/// - `DIRECT` is dropped, since a query goes direct without a tag;
/// - an own name becomes `?proxy=<namespaced name>` on `tls://` and
///   `https://` entries, and `#<namespaced name>` on the plain forms;
/// - `default-nameserver` may not name a proxy at all (the engine refuses
///   it, because it resolves the proxies' own hosts), so the tag is dropped.
///
/// A `#` that names something else is left as written: it may be a real
/// server name or an interface.
fn retarget_dns(
    dns: &mut DnsSettings,
    label: &str,
    own_names: &BTreeSet<String>,
    dropped_names: &BTreeSet<String>,
) {
    let fix = |entries: &mut Vec<String>, allow_proxy: bool| {
        for entry in entries.iter_mut() {
            *entry = retarget_nameserver(entry, label, own_names, dropped_names, allow_proxy);
        }
    };
    fix(&mut dns.nameserver, true);
    fix(&mut dns.fallback, true);
    fix(&mut dns.proxy_server_nameserver, true);
    fix(&mut dns.default_nameserver, false);
    for servers in dns.nameserver_policy.values_mut() {
        fix(servers, true);
    }
    if let Some(serde_json::Value::Array(entries)) = dns.extra.get_mut("direct-nameserver") {
        for entry in entries.iter_mut() {
            if let Some(text) = entry.as_str() {
                *entry = serde_json::Value::String(retarget_nameserver(
                    text,
                    label,
                    own_names,
                    dropped_names,
                    true,
                ));
            }
        }
    }
}

/// One nameserver entry, rewritten as [`retarget_dns`] describes.
fn retarget_nameserver(
    entry: &str,
    label: &str,
    own_names: &BTreeSet<String>,
    dropped_names: &BTreeSet<String>,
    allow_proxy: bool,
) -> String {
    let retagged = retag_nameserver(entry, label, own_names, dropped_names, allow_proxy);
    strip_plain_path(&retagged)
}

/// A plain nameserver (no `scheme://`) with a path, such as
/// `94.140.14.14/dns-query`, is a provider's slip that mihomo forgives: it
/// reads the entry as plain DNS to the host and ignores the path. The engine
/// refuses the whole configuration over it ("invalid host"), so the path is
/// dropped here. A `#tag` after it is kept; it may itself hold a `/`, as a
/// namespaced group name does.
fn strip_plain_path(entry: &str) -> String {
    if entry.contains("://") {
        return entry.to_string();
    }
    let (address, tag) = match entry.split_once('#') {
        Some((address, tag)) => (address, Some(tag)),
        None => (entry, None),
    };
    let Some((host, _path)) = address.split_once('/') else {
        return entry.to_string();
    };
    match tag {
        Some(tag) => format!("{host}#{tag}"),
        None => host.to_string(),
    }
}

/// [`retarget_nameserver`]'s handling of the `#` tag.
fn retag_nameserver(
    entry: &str,
    label: &str,
    own_names: &BTreeSet<String>,
    dropped_names: &BTreeSet<String>,
    allow_proxy: bool,
) -> String {
    let trimmed = entry.trim();
    let Some((head, tag)) = trimmed.rsplit_once('#') else {
        return entry.to_string();
    };
    let tag = tag.trim();
    let encrypted = head.starts_with("tls://") || head.starts_with("https://");
    if tag.eq_ignore_ascii_case("DIRECT") {
        return head.to_string();
    }
    let namespaced = namespace(label, tag);
    if dropped_names.contains(&namespaced) {
        return head.to_string();
    }
    if !own_names.contains(&namespaced) {
        return entry.to_string();
    }
    // The engine refuses a proxy name holding `#`, `?` or `&`, and a
    // parameter section on an entry that already has one.
    let unusable = namespaced.contains(['#', '?', '&']) || (encrypted && head.contains('?'));
    if !allow_proxy || unusable {
        return head.to_string();
    }
    if encrypted {
        format!("{head}?proxy={namespaced}")
    } else {
        format!("{head}#{namespaced}")
    }
}

/// The `name:` of a proxy or group entry as written, when it has one.
fn entry_name(entry: &serde_json::Value) -> Option<String> {
    entry
        .as_object()
        .and_then(|object| object.get("name"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Record the namespaced name of a dropped proxy or group, so that a
/// reference to it can be pruned. A blank name names nothing and is skipped.
fn remember_dropped(dropped_names: &mut BTreeSet<String>, label: &str, name: Option<&str>) {
    if let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) {
        dropped_names.insert(namespace(label, name));
    }
}

/// A group member as the group names it. A builtin outbound (`DIRECT`,
/// `REJECT`, …) is left as written; anything else is namespaced like a node.
fn namespace_member(label: &str, member: &str) -> String {
    if RuleTarget::new(member).builtin_kind().is_some() {
        member.to_string()
    } else {
        namespace(label, member)
    }
}

/// The target of a rule, when it has one. A `SUB-RULE` has none.
fn rule_target(rule: &RuleSpec) -> Option<&RuleTarget> {
    match rule {
        RuleSpec::Simple { target, .. }
        | RuleSpec::Match { target }
        | RuleSpec::Logic { target, .. }
        | RuleSpec::RuleSet { target, .. } => Some(target),
        RuleSpec::SubRule { .. } => None,
    }
}

/// Remove references to proxies and groups this subscription dropped.
///
/// Without this, one dropped entry (say a `type: direct` or `type: dns`
/// node the app does not accept) leaves a group or rule pointing at a name
/// that no longer exists, and the core validator rejects the whole
/// subscription for it.
///
/// Only names the subscription itself defined and then dropped are pruned.
/// A name it never defined may be a user's own group, so it is left for the
/// core validator, and builtin outbounds are never touched. Every removal is
/// recorded in `dropped`. A group left with no members points at `DIRECT`.
fn prune_dangling(
    label: &str,
    own_names: &BTreeSet<String>,
    dropped_names: &BTreeSet<String>,
    groups: &mut [ProxyGroup],
    rules: &mut Vec<RuleSpec>,
    dropped: &mut Vec<DroppedEntry>,
) {
    // A name the subscription dropped and did not also keep under the same name.
    let is_dropped = |name: &str| dropped_names.contains(name) && !own_names.contains(name);

    for group in groups.iter_mut() {
        let mut kept = Vec::with_capacity(group.members.len());
        for member in &group.members {
            if RuleTarget::new(member).builtin_kind().is_none() && is_dropped(member) {
                dropped.push(DroppedEntry {
                    name: Some(member.clone()),
                    reason: format!(
                        "the group “{}” listed it, but it was dropped from the subscription, \
                         so the group no longer refers to it",
                        group.name
                    ),
                });
            } else {
                kept.push(member.clone());
            }
        }
        if kept.len() == group.members.len() {
            continue;
        }
        if kept.is_empty() {
            kept.push("DIRECT".to_string());
            dropped.push(DroppedEntry {
                name: Some(group.name.clone()),
                reason: "every member of the group was dropped from the subscription, \
                         so the group now sends traffic DIRECT"
                    .to_string(),
            });
        }
        // Keep the raw map in step with the members it is emitted from.
        if group.raw.contains_key("proxies") {
            group.raw.insert(
                "proxies".to_string(),
                serde_json::Value::Array(
                    kept.iter()
                        .map(|member| serde_json::Value::String(member.clone()))
                        .collect(),
                ),
            );
        }
        group.members = kept;
    }

    rules.retain(|rule| {
        let Some(target) = rule_target(rule) else {
            return true;
        };
        if target.builtin_kind().is_some() {
            return true;
        }
        if !is_dropped(&namespace(label, target.as_str())) {
            return true;
        }
        dropped.push(DroppedEntry {
            name: Some(translate(rule)),
            reason: format!(
                "it sends traffic to “{}”, which was dropped from the subscription",
                target.as_str()
            ),
        });
        false
    });
}

/// Validate one node against what the engine will accept.
fn validate_node(entry: &serde_json::Value, label: &str) -> Result<Node, String> {
    let Some(object) = entry.as_object() else {
        return Err("the node is not a mapping".to_string());
    };

    // The name. The engine requires a string `name:`
    // (`meow-config-0.29.0/src/subscription.rs:383-396`) — a node without
    // one cannot be keyed, tracked or removed on refresh.
    let name = object
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "the node has no name".to_string())?;

    // The protocol. The engine dispatches on the exact `type:` string
    // (`meow-config-0.29.0/src/proxy_parser.rs:320-330` — the `type` is
    // read and the `match` on it begins).
    let protocol = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "the node has no type".to_string())?;
    if !KNOWN_PROTOCOLS.contains(&protocol) {
        return Err(format!(
            "'{protocol}' is not a protocol the engine supports"
        ));
    }

    // The address. `direct` is the one protocol with no server and no port:
    // the engine builds it from its name and optional fields alone
    // (`meow-config-0.29.0/src/proxy_parser.rs:492`). Every other protocol
    // requires both.
    let (server, port) = if protocol == "direct" {
        (None, None)
    } else {
        // The server. Every other protocol the engine dispatches on requires
        // a non-empty `server:` string.
        let server = object
            .get("server")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "the node has no server".to_string())?;

        // The port. The engine's `required_port`
        // (`meow-config-0.29.0/src/proxy_parser.rs:20-33`) wants a `u64` that
        // fits a `u16` and is not zero.
        let raw_port = object
            .get("port")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "the node has no port".to_string())?;
        let port =
            u16::try_from(raw_port).map_err(|_| format!("the port {raw_port} is out of range"))?;
        if port == 0 {
            return Err("the port is zero".to_string());
        }
        (Some(server.to_string()), Some(port))
    };

    // The protocol's credential.
    match protocol {
        "ss" => {
            require_string(object, "password", protocol)?;
            require_string(object, "cipher", protocol)?;
            // An `ss` node must not name an external SIP003 plugin: that
            // name reaches `Command::new`, so remote content would select
            // a local executable. The engine drops such a node
            // (`meow-config-0.29.0/src/subscription.rs:370-380`).
            if let Some(plugin) = object.get("plugin").and_then(serde_json::Value::as_str)
                && !plugin.is_empty()
                && !BUILTIN_SS_PLUGINS.contains(&plugin)
            {
                return Err(format!(
                    "the ss plugin '{plugin}' is an external plugin, which the engine \
                     refuses to run for a remote node"
                ));
            }
        }
        "trojan" | "anytls" | "hysteria2" => {
            require_string(object, "password", protocol)?;
        }
        "vless" | "vmess" => {
            require_string(object, "uuid", protocol)?;
        }
        "snell" => {
            require_string(object, "psk", protocol)?;
        }
        // `http` and `socks5` take an optional username/password *pair* —
        // the engine rejects one without the other
        // (`meow-config-0.29.0/src/proxy_parser.rs:834-843` for `http`,
        // `:904-913` for `socks5`) — but requires neither.
        "http" | "socks5" => {
            let username = object.get("username").and_then(serde_json::Value::as_str);
            let password = object.get("password").and_then(serde_json::Value::as_str);
            if username.is_some() != password.is_some() {
                return Err(format!(
                    "{protocol}: 'username' and 'password' must be set together, or neither"
                ));
            }
        }
        _ => {}
    }

    // The raw map the engine will read, minus the fields the app models
    // itself. `starpod-core`'s `Node` flattens the rest, so the engine
    // sees the node verbatim.
    let mut raw: serde_json::Map<String, serde_json::Value> = object.clone();
    raw.insert(
        "name".to_string(),
        serde_json::Value::String(namespace(label, name)),
    );

    let node_id = node_id(label, name, &raw);
    Ok(Node {
        id: node_id,
        name: namespace(label, name),
        protocol: protocol.to_string(),
        server,
        port,
        tags: Vec::new(),
        raw,
    })
}

/// Validate one proxy group against what the engine will accept.
fn validate_group(entry: &serde_json::Value, label: &str) -> Result<ProxyGroup, String> {
    let Some(object) = entry.as_object() else {
        return Err("the proxy group is not a mapping".to_string());
    };

    let name = object
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "the proxy group has no name".to_string())?;

    let group_type = object
        .get("type")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "the proxy group has no type".to_string())?;
    if !KNOWN_GROUP_TYPES.contains(&group_type) {
        return Err(format!(
            "'{group_type}' is not a proxy group type the engine supports"
        ));
    }

    // The members. The engine's `RawProxyGroup` calls the field `proxies`
    // (`meow-config-0.29.0/src/raw.rs:613`).
    //
    // The members name nodes, so they are namespaced the same way the
    // nodes are — otherwise a group would point at nothing. A builtin
    // outbound such as `DIRECT` is not a node and is kept as written.
    let members: Vec<String> = object
        .get("proxies")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|member| namespace_member(label, member))
                .collect()
        })
        .unwrap_or_default();

    let mut raw: serde_json::Map<String, serde_json::Value> = object.clone();
    raw.insert(
        "name".to_string(),
        serde_json::Value::String(namespace(label, name)),
    );
    if !members.is_empty() {
        raw.insert(
            "proxies".to_string(),
            serde_json::Value::Array(
                members
                    .iter()
                    .map(|member| serde_json::Value::String(member.clone()))
                    .collect(),
            ),
        );
    }

    Ok(ProxyGroup {
        id: group_id(label, name),
        name: namespace(label, name),
        group_type: group_type.to_string(),
        members,
        strategy: object
            .get("strategy")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        url: object
            .get("url")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        interval: object.get("interval").and_then(serde_json::Value::as_u64),
        raw,
    })
}

/// Require a non-empty string field.
fn require_string(
    object: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    protocol: &str,
) -> Result<(), String> {
    object
        .get(field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|_| ())
        .ok_or_else(|| format!("the {protocol} node has no {field}"))
}

/// Namespace a name with the subscription's label.
fn namespace(label: &str, name: &str) -> String {
    if label.is_empty() {
        return name.to_string();
    }
    format!("{label}{NAMESPACE_SEPARATOR}{name}")
}

/// A node's stable id.
///
/// The id must survive a refresh — that is what lets the user's selection
/// in a group survive the subscription changing underneath it
/// (`starpod-core`'s merge resolves the selection by id). So it is derived
/// from the *subscription* and the node's *original* name, never from its
/// position in the list and never from the namespaced name (which would
/// change if the label changed).
fn node_id(label: &str, name: &str, raw: &serde_json::Map<String, serde_json::Value>) -> String {
    // The node's own identity within the subscription: the server and port
    // it dials. Two nodes with the same name in the same subscription are
    // a publisher's mistake; two nodes with different servers are two
    // different nodes even if the publisher renamed one.
    let server = raw
        .get("server")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let port = raw
        .get("port")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    format!("{}::{name}@{server}:{port}", namespace(label, ""))
}

/// A group's stable id.
fn group_id(label: &str, name: &str) -> String {
    format!("{}::{name}", namespace(label, ""))
}

/// The subscription-level settings, keyed as the engine spells them.
///
/// The engine's `RawConfig` uses kebab-case
/// (`meow-config-0.29.0/src/raw.rs:183-190`:
/// `#[serde(rename_all = "kebab-case")]`, with `mixed_port` →
/// `mixed-port`, `socks_port` → `socks-port`, `allow_lan` → `allow-lan`,
/// `bind_address` → `bind-address`, `log_level` → `log-level`).
pub const SETTINGS_KEYS: &[(&str, &str)] = &[
    ("mixed-port", "mixed_port"),
    ("socks-port", "socks_port"),
    ("port", "port"),
    ("allow-lan", "allow_lan"),
    ("bind-address", "bind_address"),
    ("mode", "mode"),
    ("log-level", "log_level"),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A body with two good nodes, one group and one rule.
    fn valid_body() -> &'static str {
        "\
mixed-port: 7899
proxies:
  - name: \"Node A\"
    type: trojan
    server: a.example.com
    port: 443
    password: secret
  - name: \"Node B\"
    type: vmess
    server: b.example.com
    port: 8443
    uuid: 00000000-0000-0000-0000-000000000001
    alterId: 0
    cipher: auto
proxy-groups:
  - name: \"Auto\"
    type: url-test
    proxies:
      - \"Node A\"
      - \"Node B\"
    url: https://example.com/generate_204
    interval: 300
rules:
  - DOMAIN,example.com,DIRECT
  - MATCH,DIRECT
"
    }

    #[test]
    fn a_valid_body_parses_into_a_payload() {
        let parsed = parse_payload(valid_body().as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.format, PayloadFormat::Yaml);
        assert_eq!(parsed.payload.subscription_id, "sub-1");
        assert_eq!(parsed.payload.nodes.len(), 2);
        assert_eq!(parsed.payload.groups.len(), 1);
        assert_eq!(parsed.payload.rules.len(), 2);
        assert_eq!(parsed.payload.dropped_entries, 0);
    }

    #[test]
    fn node_names_are_namespaced_with_the_subscription_label() {
        let parsed = parse_payload(valid_body().as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.nodes[0].name, "Airport / Node A");
        assert_eq!(parsed.payload.nodes[1].name, "Airport / Node B");
    }

    #[test]
    fn group_names_and_members_are_namespaced_too() {
        let parsed = parse_payload(valid_body().as_bytes(), "sub-1", "Airport").expect("parses");
        let group = &parsed.payload.groups[0];
        assert_eq!(group.name, "Airport / Auto");
        assert_eq!(
            group.members,
            vec![
                "Airport / Node A".to_string(),
                "Airport / Node B".to_string()
            ],
            "a group must point at the namespaced nodes, not at the raw names"
        );
    }

    #[test]
    fn the_raw_map_carries_the_namespaced_name_for_the_engine() {
        let parsed = parse_payload(valid_body().as_bytes(), "sub-1", "Airport").expect("parses");
        let node = &parsed.payload.nodes[0];
        assert_eq!(
            node.raw.get("name").and_then(|v| v.as_str()),
            Some("Airport / Node A"),
            "the engine keys the proxies: leaves by their YAML name"
        );
        assert_eq!(
            node.raw.get("password").and_then(|v| v.as_str()),
            Some("secret"),
            "the credential is passed through verbatim"
        );
    }

    #[test]
    fn a_node_id_is_stable_across_refreshes() {
        let first = parse_payload(valid_body().as_bytes(), "sub-1", "Airport").expect("parses");
        let second = parse_payload(valid_body().as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(first.payload.nodes[0].id, second.payload.nodes[0].id);
        assert_ne!(
            first.payload.nodes[0].id, first.payload.nodes[1].id,
            "two different nodes must have different ids"
        );
    }

    #[test]
    fn a_node_id_does_not_depend_on_the_position_in_the_list() {
        let reordered = "\
proxies:
  - name: \"Node B\"
    type: vmess
    server: b.example.com
    port: 8443
    uuid: 00000000-0000-0000-0000-000000000001
  - name: \"Node A\"
    type: trojan
    server: a.example.com
    port: 443
    password: secret
";
        let original = parse_payload(valid_body().as_bytes(), "sub-1", "Airport").expect("parses");
        let swapped = parse_payload(reordered.as_bytes(), "sub-1", "Airport").expect("parses");
        let by_name = |payload: &SubscriptionPayload, name: &str| {
            payload
                .nodes
                .iter()
                .find(|node| node.name == format!("Airport / {name}"))
                .map(|node| node.id.clone())
                .expect("the node is present")
        };
        assert_eq!(
            by_name(&original.payload, "Node A"),
            by_name(&swapped.payload, "Node A"),
            "the id must not depend on the order the publisher shipped the nodes in"
        );
    }

    #[test]
    fn the_subscription_settings_are_read_from_the_top_level_keys() {
        let parsed = parse_payload(valid_body().as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.settings.mixed_port, Some(7899));
        assert_eq!(parsed.payload.settings.mode, None);
    }

    #[test]
    fn the_settings_read_the_kebab_case_keys_the_engine_uses() {
        let body = "\
mode: global
log-level: warning
allow-lan: true
bind-address: 127.0.0.1
socks-port: 7891
port: 7892
proxies:
  - { name: A, type: http, server: a.example.com, port: 8080 }
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        let settings = &parsed.payload.settings;
        assert_eq!(settings.mode, Some(ProxyMode::Global));
        assert_eq!(
            settings.log_level,
            Some(starpod_core::domain::LogLevel::Warning)
        );
        assert_eq!(settings.allow_lan, Some(true));
        assert_eq!(settings.bind_address.as_deref(), Some("127.0.0.1"));
        assert_eq!(settings.socks_port, Some(7891));
        assert_eq!(settings.port, Some(7892));
    }

    #[test]
    fn a_base64_wrapped_body_is_unwrapped() {
        let encoded = BASE64_STANDARD.encode(valid_body());
        let parsed = parse_payload(encoded.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.format, PayloadFormat::Base64Yaml);
        assert_eq!(parsed.payload.nodes.len(), 2);
    }

    #[test]
    fn a_base64_body_with_whitespace_is_unwrapped() {
        let encoded = BASE64_STANDARD.encode(valid_body());
        let wrapped: String = encoded
            .as_bytes()
            .chunks(64)
            .map(|chunk| format!("{}\n", String::from_utf8_lossy(chunk)))
            .collect();
        let parsed = parse_payload(wrapped.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.format, PayloadFormat::Base64Yaml);
    }

    #[test]
    fn a_body_that_is_neither_yaml_nor_base64_is_rejected() {
        let error = parse_payload(b"just some text", "sub-1", "Airport").unwrap_err();
        assert!(
            matches!(error, SubscriptionError::InvalidBody(_)),
            "unexpected error: {error:?}"
        );
        assert!(error.to_string().contains("neither"), "{error}");
    }

    #[test]
    fn a_body_that_is_not_utf8_is_rejected() {
        let error = parse_payload(&[0xff, 0xfe, 0x00], "sub-1", "Airport").unwrap_err();
        assert!(matches!(error, SubscriptionError::InvalidBody(_)));
        assert!(error.to_string().contains("UTF-8"), "{error}");
    }

    #[test]
    fn a_yaml_body_with_a_syntax_error_is_reported_as_such() {
        let body = "proxies:\n  - name: A\n   type: http\n";
        let error = parse_payload(body.as_bytes(), "sub-1", "Airport").unwrap_err();
        assert!(matches!(error, SubscriptionError::InvalidBody(_)));
        assert!(error.to_string().contains("YAML"), "{error}");
    }

    #[test]
    fn a_document_without_proxies_or_groups_is_rejected() {
        let body = "mixed-port: 7890\n";
        let error = parse_payload(body.as_bytes(), "sub-1", "Airport").unwrap_err();
        assert!(error.to_string().contains("neither"), "{error}");
    }

    #[test]
    fn an_invalid_node_is_skipped_and_counted() {
        let body = "\
proxies:
  - name: \"Good\"
    type: trojan
    server: a.example.com
    port: 443
    password: secret
  - name: \"Bad Protocol\"
    type: wireguard
    server: b.example.com
    port: 443
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.nodes.len(), 1);
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert_eq!(parsed.dropped.len(), 1);
        assert_eq!(parsed.dropped[0].name.as_deref(), Some("Bad Protocol"));
        assert!(parsed.dropped[0].reason.contains("wireguard"));
    }

    /// A node that always survives validation.
    ///
    /// The "one node was dropped" tests need a *surviving* node: a
    /// subscription whose every node is rejected is rejected as a whole
    /// (see [`SubscriptionError::EveryNodeDropped`]), so it never reaches
    /// the "the bad entry was counted" assertion.
    const GOOD: &str = "  - { name: Good, type: http, server: good.example.com, port: 8080 }\n";

    #[test]
    fn a_node_without_a_server_is_dropped() {
        let body = format!("proxies:\n{GOOD}  - {{ name: A, type: http, port: 8080 }}\n");
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.nodes.len(), 1, "the good node survives");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("server"));
    }

    #[test]
    fn a_node_with_an_empty_server_is_dropped() {
        let body =
            format!("proxies:\n{GOOD}  - {{ name: A, type: http, server: \"\", port: 8080 }}\n");
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.dropped_entries, 1);
    }

    #[test]
    fn a_node_with_a_zero_port_is_dropped() {
        let body = format!(
            "proxies:\n{GOOD}  - {{ name: A, type: http, server: a.example.com, port: 0 }}\n"
        );
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("zero"));
    }

    #[test]
    fn a_node_with_a_port_above_the_range_is_dropped() {
        let body = format!(
            "proxies:\n{GOOD}  - {{ name: A, type: http, server: a.example.com, port: 70000 }}\n"
        );
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("range"));
    }

    #[test]
    fn a_node_without_a_name_is_dropped() {
        let body =
            format!("proxies:\n{GOOD}  - {{ type: http, server: a.example.com, port: 8080 }}\n");
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("name"));
    }

    #[test]
    fn a_node_without_a_credential_is_dropped() {
        let body = format!(
            "proxies:\n{GOOD}  - {{ name: A, type: trojan, server: a.example.com, port: 443 }}\n"
        );
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("password"));
    }

    #[test]
    fn an_ss_node_without_a_cipher_is_dropped() {
        let body = format!(
            "proxies:\n{GOOD}  - {{ name: A, type: ss, server: a.example.com, port: 443, \
             password: p }}\n"
        );
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("cipher"));
    }

    #[test]
    fn an_ss_node_with_a_builtin_plugin_is_kept() {
        let body = "proxies:\n  - { name: A, type: ss, server: a.example.com, port: 443, \
                    password: p, cipher: aes-256-gcm, plugin: obfs }\n";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.nodes.len(), 1);
    }

    #[test]
    fn an_ss_node_with_an_external_plugin_is_dropped() {
        let body = format!(
            "proxies:\n{GOOD}  - {{ name: A, type: ss, server: a.example.com, port: 443, \
             password: p, cipher: aes-256-gcm, plugin: /usr/bin/evil }}\n"
        );
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.nodes.len(), 1, "the good node survives");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("external plugin"));
    }

    #[test]
    fn a_vless_node_without_a_uuid_is_dropped() {
        let body = format!(
            "proxies:\n{GOOD}  - {{ name: A, type: vless, server: a.example.com, port: 443 }}\n"
        );
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("uuid"));
    }

    #[test]
    fn a_socks5_node_with_only_a_username_is_dropped() {
        let body = format!(
            "proxies:\n{GOOD}  - {{ name: A, type: socks5, server: a.example.com, port: 1080, \
             username: u }}\n"
        );
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("username"));
    }

    #[test]
    fn a_socks5_node_with_both_credentials_is_kept() {
        let body = "proxies:\n  - { name: A, type: socks5, server: a.example.com, port: 1080, \
                    username: u, password: p }\n";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.nodes.len(), 1);
    }

    #[test]
    fn a_subscription_where_every_node_was_dropped_is_rejected() {
        let body = "proxies:\n  - { name: A, type: wireguard, server: a.example.com, port: 443 }\n";
        let error = parse_payload(body.as_bytes(), "sub-1", "Airport").unwrap_err();
        match error {
            SubscriptionError::EveryNodeDropped { dropped, .. } => assert_eq!(dropped, 1),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn a_subscription_with_no_proxies_at_all_is_empty() {
        let body = "proxy-groups:\n  - { name: G, type: select, proxies: [A] }\n";
        let error = parse_payload(body.as_bytes(), "sub-1", "Airport").unwrap_err();
        assert!(matches!(error, SubscriptionError::EmptyAfterValidation));
    }

    #[test]
    fn a_group_with_an_unknown_type_is_dropped() {
        let body = "\
proxies:
  - { name: A, type: http, server: a.example.com, port: 8080 }
proxy-groups:
  - { name: G, type: magic, proxies: [A] }
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.groups.len(), 0);
        assert_eq!(parsed.payload.nodes.len(), 1, "the node survives");
        assert_eq!(parsed.payload.dropped_entries, 1);
        assert!(parsed.dropped[0].reason.contains("magic"));
    }

    #[test]
    fn an_unparsable_rule_is_dropped_but_the_nodes_survive() {
        let body = "\
proxies:
  - { name: A, type: http, server: a.example.com, port: 8080 }
rules:
  - DOMAIN,example.com,DIRECT
  - NOPE,x,DIRECT
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.rules.len(), 1);
        assert_eq!(parsed.payload.nodes.len(), 1);
        assert_eq!(parsed.payload.dropped_entries, 1);
    }

    #[test]
    fn an_empty_label_leaves_the_name_untouched() {
        let body = "proxies:\n  - { name: A, type: http, server: a.example.com, port: 8080 }\n";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "").expect("parses");
        assert_eq!(parsed.payload.nodes[0].name, "A");
    }

    #[test]
    fn the_format_has_a_stable_label() {
        assert_eq!(PayloadFormat::Yaml.as_str(), "yaml");
        assert_eq!(PayloadFormat::Base64Yaml.as_str(), "base64-yaml");
    }

    #[test]
    fn the_settings_keys_are_the_engines_spelling() {
        let keys = SETTINGS_KEYS;
        let find = |key: &str| {
            keys.iter()
                .find(|(engine_key, _)| *engine_key == key)
                .map(|(_, app_key)| *app_key)
        };
        assert_eq!(find("mixed-port"), Some("mixed_port"));
        assert_eq!(find("socks-port"), Some("socks_port"));
        assert_eq!(find("allow-lan"), Some("allow_lan"));
        assert_eq!(find("bind-address"), Some("bind_address"));
        assert_eq!(find("log-level"), Some("log_level"));
    }

    #[test]
    fn the_known_protocols_are_the_ones_the_engine_dispatches_on() {
        for protocol in KNOWN_PROTOCOLS {
            assert!(!protocol.is_empty());
        }
        assert!(
            KNOWN_PROTOCOLS.contains(&"direct"),
            "the engine dispatches on `direct` (proxy_parser.rs:492)"
        );
        assert!(
            !KNOWN_PROTOCOLS.contains(&"dns"),
            "the engine has no `dns` proxy type"
        );
        assert!(
            !KNOWN_PROTOCOLS.contains(&"trusttunnel"),
            "the engine's trusttunnel arm is feature-gated and the release build does not \
             enable it"
        );
    }

    #[test]
    fn a_direct_node_with_only_a_name_is_kept() {
        let body = "proxies:\n  - { name: Без VPN, type: direct }\n";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Cumzone").expect("parses");
        assert_eq!(parsed.payload.nodes.len(), 1);
        assert_eq!(parsed.payload.dropped_entries, 0);
        let node = &parsed.payload.nodes[0];
        assert_eq!(node.name, "Cumzone / Без VPN");
        assert_eq!(node.protocol, "direct");
        assert_eq!(node.server, None, "a direct node has no server");
        assert_eq!(node.port, None, "a direct node has no port");
        assert_eq!(
            node.raw.get("type").and_then(|v| v.as_str()),
            Some("direct"),
            "the raw map carries the engine's type verbatim"
        );
        assert!(
            !node.raw.contains_key("server") && !node.raw.contains_key("port"),
            "no server or port is invented for the engine"
        );
    }

    #[test]
    fn a_payload_cached_before_the_fixes_is_upgraded_and_stays_put_after() {
        let body = "\
proxies:
  - { name: \"🇷🇺 Moscow\", type: http, server: m.example.com, port: 8080 }
proxy-groups:
  - { name: Web, type: select, proxies: [DIRECT] }
";
        let mut payload = parse_payload(body.as_bytes(), "sub-1", "Sub")
            .expect("parses")
            .payload;
        // What an earlier build cached: the tags and keys as the provider wrote them.
        payload.groups[0]
            .raw
            .insert("include-all".into(), serde_json::Value::Bool(true));
        payload.dns = Some(DnsSettings {
            default_nameserver: vec!["https://94.140.14.14/dns-query#DIRECT".into()],
            nameserver: vec!["https://8.8.8.8/dns-query#Web".into()],
            ..DnsSettings::default()
        });

        upgrade_cached_payload(&mut payload, "Sub");
        let dns = payload.dns.clone().unwrap();
        assert_eq!(
            dns.default_nameserver,
            vec!["https://94.140.14.14/dns-query"]
        );
        assert_eq!(
            dns.nameserver,
            vec!["https://8.8.8.8/dns-query?proxy=Sub / Web"]
        );
        assert_eq!(payload.groups[0].members, vec!["DIRECT", "Sub / 🇷🇺 Moscow"]);

        let once = payload.clone();
        upgrade_cached_payload(&mut payload, "Sub");
        assert_eq!(payload, once, "a second upgrade changes nothing");
    }

    #[test]
    fn include_all_is_expanded_from_this_subscriptions_nodes() {
        let body = "\
proxies:
  - { name: \"🇷🇺 Moscow\", type: http, server: m.example.com, port: 8080 }
  - { name: \"🇷🇺➡️ Chained\", type: http, server: c.example.com, port: 8080 }
  - { name: \"🇩🇪 Berlin\", type: http, server: b.example.com, port: 8080 }
proxy-groups:
  - { name: RU, type: select, include-all: true, filter: \"🇷🇺\", exclude-filter: \"🇷🇺➡️\", proxies: [] }
  - { name: All, type: select, include-all-proxies: true, proxies: [DIRECT] }
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Sub").expect("parses");
        let ru = &parsed.payload.groups[0];
        assert_eq!(ru.members, vec!["Sub / 🇷🇺 Moscow".to_string()]);
        assert_eq!(
            ru.raw.get("proxies"),
            Some(&serde_json::json!(["Sub / 🇷🇺 Moscow"]))
        );
        for key in ["include-all", "filter", "exclude-filter"] {
            assert!(!ru.raw.contains_key(key), "{key} is applied, not passed on");
        }
        let all = &parsed.payload.groups[1];
        assert_eq!(
            all.members,
            vec![
                "DIRECT",
                "Sub / 🇷🇺 Moscow",
                "Sub / 🇷🇺➡️ Chained",
                "Sub / 🇩🇪 Berlin"
            ]
        );
    }

    #[test]
    fn mihomo_nameserver_proxy_tags_become_engine_syntax() {
        let body = "\
proxies:
  - { name: Good, type: http, server: good.example.com, port: 8080 }
proxy-groups:
  - { name: \"🌍 Web\", type: select, proxies: [Good, DIRECT] }
dns:
  default-nameserver:
    - https://94.140.14.14/dns-query#DIRECT
    - 1.1.1.1#🌍 Web
  nameserver:
    - https://8.8.8.8/dns-query#🌍 Web
    - tls://1.1.1.1#cloudflare-dns.com
    - 9.9.9.9#Good
  direct-nameserver:
    - https://77.88.8.8/dns-query#DIRECT
    - 94.140.14.14/dns-query#DIRECT
    - 1.1.1.1/dns-query#🌍 Web
  nameserver-policy:
    rule-set:youtube:
      - https://8.8.8.8/dns-query#🌍 Web
      - rcode://refused
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Sub").expect("parses");
        let dns = parsed.payload.dns.expect("the dns block is kept");
        assert_eq!(
            dns.default_nameserver,
            vec!["https://94.140.14.14/dns-query", "1.1.1.1"],
            "default-nameserver may not name a proxy"
        );
        assert_eq!(
            dns.nameserver,
            vec![
                "https://8.8.8.8/dns-query?proxy=Sub / 🌍 Web",
                "tls://1.1.1.1#cloudflare-dns.com",
                "9.9.9.9#Sub / Good",
            ],
            "own names are namespaced, a real SNI stays"
        );
        assert_eq!(
            dns.extra.get("direct-nameserver"),
            Some(&serde_json::json!([
                "https://77.88.8.8/dns-query",
                "94.140.14.14",
                "1.1.1.1#Sub / 🌍 Web"
            ])),
            "a plain entry loses the path mihomo ignores"
        );
        assert_eq!(
            dns.nameserver_policy["rule-set:youtube"],
            vec![
                "https://8.8.8.8/dns-query?proxy=Sub / 🌍 Web",
                "rcode://refused"
            ]
        );
    }

    #[test]
    fn a_dropped_dns_node_is_pruned_from_groups_and_rules() {
        let body = "\
proxies:
  - { name: Good, type: http, server: good.example.com, port: 8080 }
  - { name: DNS-OUT, type: dns }
proxy-groups:
  - { name: G, type: select, proxies: [DNS-OUT, Good, DIRECT] }
rules:
  - DST-PORT,53,DNS-OUT
  - MATCH,G
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Cumzone").expect("parses");
        assert_eq!(parsed.payload.nodes.len(), 1, "the dns node is dropped");
        assert_eq!(parsed.payload.nodes[0].name, "Cumzone / Good");

        let group = &parsed.payload.groups[0];
        assert_eq!(
            group.members,
            vec!["Cumzone / Good".to_string(), "DIRECT".to_string()],
            "the dropped member goes, the surviving node and the builtin stay"
        );
        assert_eq!(
            group.raw.get("proxies"),
            Some(&serde_json::json!(["Cumzone / Good", "DIRECT"])),
            "the raw map follows the members"
        );

        let lines: Vec<String> = parsed.payload.rules.iter().map(translate).collect();
        assert_eq!(lines, vec!["MATCH,Cumzone / G".to_string()]);

        let reasons: Vec<&str> = parsed.dropped.iter().map(|d| d.reason.as_str()).collect();
        assert!(
            parsed
                .dropped
                .iter()
                .any(|d| d.name.as_deref() == Some("DNS-OUT")),
            "the dropped node is reported: {reasons:?}"
        );
        assert!(
            parsed
                .dropped
                .iter()
                .any(|d| d.name.as_deref() == Some("Cumzone / DNS-OUT")),
            "the pruned group member is reported: {reasons:?}"
        );
        assert!(
            parsed
                .dropped
                .iter()
                .any(|d| d.name.as_deref() == Some("DST-PORT,53,DNS-OUT")),
            "the pruned rule is reported: {reasons:?}"
        );
        assert_eq!(parsed.payload.dropped_entries, parsed.dropped.len());
    }

    #[test]
    fn a_group_whose_only_member_was_dropped_points_at_direct() {
        let body = "\
proxies:
  - { name: Good, type: http, server: good.example.com, port: 8080 }
  - { name: DNS-OUT, type: dns }
proxy-groups:
  - { name: Solo, type: select, proxies: [DNS-OUT] }
  - { name: G, type: select, proxies: [Good] }
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Cumzone").expect("parses");
        let solo = parsed
            .payload
            .groups
            .iter()
            .find(|group| group.name == "Cumzone / Solo")
            .expect("the group survives");
        assert_eq!(solo.members, vec!["DIRECT".to_string()]);
        assert!(
            parsed
                .dropped
                .iter()
                .any(|d| d.reason.contains("DIRECT") && d.name.as_deref() == Some("Cumzone / Solo")),
            "the emptied group is reported: {:?}",
            parsed.dropped
        );
    }

    #[test]
    fn a_reference_to_a_name_never_defined_is_left_for_the_core_validator() {
        let body = "\
proxies:
  - { name: Good, type: http, server: good.example.com, port: 8080 }
proxy-groups:
  - { name: G, type: select, proxies: [Good, Ghost] }
rules:
  - MATCH,Ghost
";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Cumzone").expect("parses");
        assert_eq!(
            parsed.payload.groups[0].members,
            vec!["Cumzone / Good".to_string(), "Cumzone / Ghost".to_string()],
            "an undefined member is not pruned"
        );
        assert_eq!(
            parsed.payload.rules.len(),
            1,
            "an undefined target is not pruned"
        );
        assert_eq!(parsed.payload.dropped_entries, 0);
    }
}

#[cfg(test)]
mod rule_provider_tests {
    use super::*;
    use starpod_core::rules::translate::translate;

    /// A Clash body in the shape the bug report describes: providers of each
    /// kind, a non-ASCII group that rules target, and `no-resolve`.
    fn fixture() -> &'static str {
        r#"
proxies:
  - { name: A, type: http, server: a.example.com, port: 8080 }
proxy-groups:
  - { name: "🚫 Недоступные сайты", type: select, proxies: [A] }
  - { name: Proxy, type: select, proxies: [A] }
rule-providers:
  geoip-private:
    type: http
    behavior: ipcidr
    url: https://rules.example.com/geoip-private.yaml
    path: ../../etc/evil.yaml
    interval: 86400
    proxy: DIRECT
  oisd_big:
    type: http
    behavior: domain
    format: text
    url: https://rules.example.com/oisd.txt
    proxy: A
  ukraine:
    type: http
    behavior: domain
    url: https://rules.example.com/ukraine.yaml
    proxy: "🚫 Недоступные сайты"
  local-file:
    type: file
    behavior: domain
    path: /etc/passwd
  inline-one:
    type: inline
    behavior: classical
    payload:
      - DOMAIN-SUFFIX,example.test
      - DOMAIN-KEYWORD,ads
  broken:
    type: http
    behavior: sideways
    url: https://x.example.com/x
rules:
  - RULE-SET,geoip-private,DIRECT,no-resolve
  - RULE-SET,oisd_big,REJECT
  - RULE-SET,ukraine,🚫 Недоступные сайты
  - RULE-SET,local-file,DIRECT
  - RULE-SET,broken,DIRECT
  - RULE-SET,inline-one,Proxy
  - DOMAIN,example.test,Proxy
  - MATCH,Proxy
"#
    }

    fn parsed() -> SubscriptionPayload {
        parse_payload(fixture().as_bytes(), "sub-1", "Airport")
            .expect("the fixture parses")
            .payload
    }

    fn provider<'a>(payload: &'a SubscriptionPayload, name: &str) -> &'a RuleProvider {
        payload
            .rule_providers
            .iter()
            .find(|provider| provider.name == name)
            .unwrap_or_else(|| panic!("no provider named {name}"))
    }

    #[test]
    fn the_usable_providers_are_kept_and_the_others_dropped() {
        let payload = parsed();
        let names: Vec<&str> = payload
            .rule_providers
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        let mut names = names;
        names.sort_unstable();
        assert_eq!(
            names,
            ["geoip-private", "inline-one", "oisd_big", "ukraine"]
        );
        // Two providers dropped (file, broken) and the two rules naming them.
        assert_eq!(payload.dropped_entries, 4);
        assert_eq!(payload.rules.len(), 6);
    }

    #[test]
    fn an_http_provider_gets_our_own_cache_path_never_the_subscriptions() {
        let payload = parsed();
        assert_eq!(
            provider(&payload, "geoip-private").path.as_deref(),
            Some("rule-providers/geoip-private.yaml")
        );
        assert_eq!(
            provider(&payload, "oisd_big").path.as_deref(),
            Some("rule-providers/oisd_big.txt"),
            "a text format is cached as .txt"
        );
        for provider in &payload.rule_providers {
            if let Some(path) = &provider.path {
                assert!(!path.contains(".."), "{path}");
                assert!(!path.starts_with('/'), "{path}");
            }
        }
    }

    #[test]
    fn the_http_fields_are_read_as_the_engine_spells_them() {
        let payload = parsed();
        let geoip = provider(&payload, "geoip-private");
        assert_eq!(geoip.provider_type, "http");
        assert_eq!(geoip.behavior, RuleProviderBehavior::IpCidr);
        assert_eq!(
            geoip.format.as_deref(),
            Some("yaml"),
            "the default is written out"
        );
        assert_eq!(geoip.interval, Some(86_400));
        assert_eq!(geoip.proxy.as_deref(), Some("DIRECT"));
        assert_eq!(
            geoip.url.as_deref(),
            Some("https://rules.example.com/geoip-private.yaml")
        );
    }

    #[test]
    fn a_download_proxy_that_names_a_subscription_group_is_namespaced() {
        let payload = parsed();
        assert_eq!(
            provider(&payload, "ukraine").proxy.as_deref(),
            Some("Airport / 🚫 Недоступные сайты")
        );
        assert_eq!(
            provider(&payload, "oisd_big").proxy.as_deref(),
            Some("Airport / A")
        );
    }

    #[test]
    fn an_inline_provider_keeps_its_payload_and_no_interval() {
        let payload = parsed();
        let inline = provider(&payload, "inline-one");
        assert_eq!(inline.provider_type, "inline");
        assert_eq!(inline.behavior, RuleProviderBehavior::Classical);
        assert_eq!(
            inline.payload.as_deref(),
            Some(
                &[
                    "DOMAIN-SUFFIX,example.test".to_string(),
                    "DOMAIN-KEYWORD,ads".to_string(),
                ][..]
            )
        );
        assert_eq!(inline.path, None);
        assert_eq!(inline.url, None);
    }

    #[test]
    fn a_rule_set_targeting_a_non_ascii_group_is_namespaced_and_keeps_its_flags() {
        let payload = parsed();
        let lines: Vec<String> = payload.rules.iter().map(translate).collect();
        assert!(
            lines.contains(&"RULE-SET,ukraine,Airport / 🚫 Недоступные сайты".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"RULE-SET,geoip-private,DIRECT,no-resolve".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"RULE-SET,oisd_big,REJECT".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"RULE-SET,inline-one,Airport / Proxy".to_string()),
            "{lines:?}"
        );
    }

    #[test]
    fn a_rule_set_naming_a_dropped_provider_is_dropped_with_it() {
        let payload = parsed();
        assert!(!payload.rules.iter().any(|rule| {
            matches!(rule, RuleSpec::RuleSet { provider, .. } if provider == "local-file" || provider == "broken")
        }));
    }

    #[test]
    fn a_file_provider_is_refused_with_its_reason() {
        let own = BTreeSet::new();
        let value =
            serde_json::json!({"type": "file", "behavior": "domain", "path": "/etc/passwd"});
        let reason = validate_rule_provider("local", &value, "Airport", &own).unwrap_err();
        assert!(reason.contains("local file"), "{reason}");
    }

    #[test]
    fn an_unknown_behavior_or_format_drops_the_provider() {
        let own = BTreeSet::new();
        let sideways = serde_json::json!({"type": "http", "behavior": "sideways", "url": "https://x.example.com/x"});
        assert!(
            validate_rule_provider("p", &sideways, "", &own)
                .unwrap_err()
                .contains("sideways")
        );
        let odd_format = serde_json::json!({"type": "http", "behavior": "domain", "format": "json", "url": "https://x.example.com/x"});
        assert!(
            validate_rule_provider("p", &odd_format, "", &own)
                .unwrap_err()
                .contains("json")
        );
    }

    #[test]
    fn an_http_provider_needs_an_http_or_https_url() {
        let own = BTreeSet::new();
        for url in [
            "file:///etc/passwd",
            "ftp://x.example.com/a",
            "relative/path",
            "https://x.example.com/a b",
        ] {
            let value = serde_json::json!({"type": "http", "behavior": "domain", "url": url});
            assert!(
                validate_rule_provider("p", &value, "", &own).is_err(),
                "{url}"
            );
        }
    }

    #[test]
    fn an_inline_provider_without_a_payload_is_dropped() {
        let value = serde_json::json!({"type": "inline", "behavior": "domain"});
        assert!(validate_rule_provider("p", &value, "", &BTreeSet::new()).is_err());
    }

    #[test]
    fn a_rule_set_naming_no_provider_here_is_kept_for_the_app_to_resolve() {
        // A user rule or another subscription may declare it.
        let body = "proxies:\n  - { name: A, type: http, server: a.example.com, port: 8080 }\nrules:\n  - RULE-SET,elsewhere,DIRECT\n";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(parsed.payload.rules.len(), 1);
    }

    #[test]
    fn a_rule_to_a_user_group_keeps_its_name() {
        let body = "proxies:\n  - { name: A, type: http, server: a.example.com, port: 8080 }\nrules:\n  - DOMAIN,example.test,Ghost\n";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert_eq!(
            translate(&parsed.payload.rules[0]),
            "DOMAIN,example.test,Ghost"
        );
    }

    #[test]
    fn a_plain_rule_to_a_subscription_group_is_namespaced_like_a_set() {
        let payload = parsed();
        assert_eq!(
            translate(&payload.rules[4]),
            "DOMAIN,example.test,Airport / Proxy"
        );
    }

    #[test]
    fn provider_file_stems_never_collide_and_never_escape_the_directory() {
        assert_eq!(provider_file_stem("ukraine"), "ukraine");
        assert_ne!(provider_file_stem("a b"), provider_file_stem("a_b"));
        assert_ne!(provider_file_stem("🚫 x"), provider_file_stem("__ x"));
        for name in ["../x", "a/b", "..", ".hidden", "🚫 Недоступные сайты"] {
            let stem = provider_file_stem(name);
            assert!(!stem.contains('/') && !stem.contains('\\'), "{stem}");
            assert!(!stem.contains(".."), "{stem}");
            assert!(!stem.starts_with('.'), "{stem}");
        }
    }

    #[test]
    fn a_subscription_without_rule_providers_has_none() {
        let body = "proxies:\n  - { name: A, type: http, server: a.example.com, port: 8080 }\n";
        let parsed = parse_payload(body.as_bytes(), "sub-1", "Airport").expect("parses");
        assert!(parsed.payload.rule_providers.is_empty());
    }
}
