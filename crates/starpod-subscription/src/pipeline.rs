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
use starpod_core::domain::{LogLevel, SubscriptionSettings};
use starpod_core::rules::parse::parse_line;
use starpod_core::{Node, ProxyGroup, ProxyMode, RuleSpec, SubscriptionPayload};

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
/// | `direct` | 492 | none |
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
            settings: extracted.settings,
            dropped_entries: extracted.dropped.len(),
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
    settings: SubscriptionSettings,
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

    match mapping.get("proxies") {
        None => {}
        Some(serde_json::Value::Array(entries)) => {
            for entry in entries {
                match validate_node(entry, label) {
                    Ok(node) => nodes.push(node),
                    Err(reason) => dropped.push(DroppedEntry {
                        name: entry
                            .as_object()
                            .and_then(|object| object.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        reason,
                    }),
                }
            }
        }
        Some(_) => {
            return Err(SubscriptionError::InvalidBody(
                "'proxies:' is not a list".to_string(),
            ));
        }
    }

    let groups = match mapping.get("proxy-groups") {
        None => Vec::new(),
        Some(serde_json::Value::Array(entries)) => {
            let mut groups = Vec::new();
            for entry in entries {
                match validate_group(entry, label) {
                    Ok(group) => groups.push(group),
                    Err(reason) => dropped.push(DroppedEntry {
                        name: entry
                            .as_object()
                            .and_then(|object| object.get("name"))
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string),
                        reason,
                    }),
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

    let rules = match mapping.get("rules") {
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
                    Ok(rule) => rules.push(rule),
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

    let settings = read_settings(mapping);

    Ok(Extracted {
        nodes,
        groups,
        rules,
        settings,
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

    // The server. Every protocol the engine dispatches on requires a
    // non-empty `server:` string.
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
        server: Some(server.to_string()),
        port: Some(port),
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
    // nodes are — otherwise a group would point at nothing.
    let members: Vec<String> = object
        .get("proxies")
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|member| namespace(label, member))
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
            !KNOWN_PROTOCOLS.contains(&"trusttunnel"),
            "the engine's trusttunnel arm is feature-gated and the release build does not \
             enable it"
        );
    }
}
