//! The domain types the rest of the app is built on.
//!
//! These mirror the parts of the engine's configuration the app actually
//! owns, plus the parts the user configures that the engine has no concept
//! of (a rule's identity, its enabled flag, its order).
//!
//! Everything here is plain data: serialisable, comparable, and free of
//! behaviour that touches the outside world.

use serde::{Deserialize, Serialize};

use crate::rules::{RuleKind, RuleSpec, RuleTarget};

/// The routing mode the engine runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// Route through the selected proxy.
    #[default]
    Rule,
    /// Send everything through the selected proxy.
    Global,
    /// Send everything directly.
    Direct,
}

impl ProxyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Rule => "rule",
            Self::Global => "global",
            Self::Direct => "direct",
        }
    }

    pub fn from_str_exact(value: &str) -> Option<Self> {
        Some(match value {
            "rule" => Self::Rule,
            "global" => Self::Global,
            "direct" => Self::Direct,
            _ => return None,
        })
    }
}

/// The engine's log verbosity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Silent,
    Error,
    Warning,
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Silent => "silent",
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    pub fn from_str_exact(value: &str) -> Option<Self> {
        Some(match value {
            "silent" => Self::Silent,
            "error" => Self::Error,
            "warning" => Self::Warning,
            "info" => Self::Info,
            "debug" => Self::Debug,
            _ => return None,
        })
    }
}

/// The TUN inbound settings.
///
/// TUN is the only part of the configuration the app toggles at runtime
/// rather than by rewriting the config file, because the engine exposes it
/// as a live `PATCH`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunSettings {
    /// Whether the TUN inbound is on.
    pub enabled: bool,
    /// The device name the engine creates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// The device MTU. The engine hard-errors below 1280.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtu: Option<u16>,
    /// Whether the engine installs routes on startup.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_route: Option<bool>,
    /// The DNS hijack targets.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dns_hijack: Option<Vec<String>>,
}

impl Default for TunSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            device: None,
            mtu: None,
            auto_route: Some(true),
            dns_hijack: None,
        }
    }
}

/// The user's own settings — the things they configure by hand.
///
/// This type is the *only* place user intent lives. It is stored separately
/// from subscription-derived data (GOAL.md §3), which is what makes
/// "a subscription refresh never overwrites a user setting" a structural
/// property rather than a promise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserPreferences {
    /// The HTTP proxy port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mixed_port: Option<u16>,
    /// The SOCKS proxy port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socks_port: Option<u16>,
    /// The HTTP proxy port (the plain `port:` key, separate from mixed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Whether the proxy listens on the LAN as well as loopback.
    ///
    /// This is `Option<bool>` and not a plain `bool` for a reason: `None`
    /// means "the user has not expressed a preference", which is different
    /// from `Some(false)`, which means "the user explicitly turned this
    /// off". Without the distinction, a subscription that ships
    /// `allow-lan: true` would be able to expose the proxy to the network
    /// merely because the user had never touched the setting — which is
    /// exactly the "insecure default for convenience" GOAL.md §6 rules out.
    ///
    /// The effective default, when nothing else says otherwise, is **off**.
    #[serde(default)]
    pub allow_lan: Option<bool>,
    /// The address the proxy binds to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
    /// The routing mode.
    ///
    /// `None` means the user has not chosen a mode; the subscription's mode
    /// is then used, and failing that the engine's default.
    #[serde(default)]
    pub mode: Option<ProxyMode>,
    /// The engine's log verbosity.
    ///
    /// `None` means the user has not chosen a level.
    #[serde(default)]
    pub log_level: Option<LogLevel>,
    /// The TUN settings.
    #[serde(default)]
    pub tun: TunSettings,
    /// The name of the proxy group the engine selects from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_group: Option<String>,
    /// The node the user picked inside the selected group, by node id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_node_id: Option<String>,
}

impl Default for UserPreferences {
    fn default() -> Self {
        Self {
            mixed_port: Some(7890),
            socks_port: Some(7891),
            port: None,
            allow_lan: None,
            bind_address: None,
            mode: None,
            log_level: None,
            tun: TunSettings::default(),
            selected_group: None,
            selected_node_id: None,
        }
    }
}

/// A proxy node as the app understands it.
///
/// The engine's own node format is a large open map of protocol-specific
/// fields. The app keeps the fields it needs to *display* and *select* a
/// node, and keeps the untouched original map alongside, so that fields the
/// app does not model are still passed to the engine verbatim.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    /// The node's unique id, stable across subscription refreshes.
    pub id: String,
    /// The node's display name, unique within the whole configuration.
    pub name: String,
    /// The protocol (`vmess`, `trojan`, `ss`, `hysteria2`, …).
    pub protocol: String,
    /// The server address.
    pub server: Option<String>,
    /// The server port.
    pub port: Option<u16>,
    /// The node's traffic labels, if the subscription supplied any.
    #[serde(default)]
    pub tags: Vec<String>,
    /// The original engine-format node, passed through untouched.
    #[serde(flatten)]
    pub raw: serde_json::Map<String, serde_json::Value>,
}

/// A proxy group as the app understands it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProxyGroup {
    /// The group's unique id.
    pub id: String,
    /// The group's display name, unique within the configuration.
    pub name: String,
    /// The group type (`select`, `url-test`, `fallback`, `load-balance`, …).
    #[serde(rename = "type")]
    pub group_type: String,
    /// The group's members, by node or group name.
    #[serde(default)]
    pub members: Vec<String>,
    /// The selection strategy, for the strategies that have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// The health-check URL, for the groups that have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The health-check interval in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    /// The original engine-format group, passed through untouched.
    #[serde(flatten)]
    pub raw: serde_json::Map<String, serde_json::Value>,
}

/// A rule as the app stores it.
///
/// The rule's *shape* lives in [`RuleSpec`]; this adds the app-level
/// concepts the engine knows nothing about: a stable id, whether the rule is
/// enabled, and its position.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredRule {
    /// The rule's stable id, used to break order ties deterministically.
    pub id: String,
    /// The rule's shape.
    #[serde(flatten)]
    pub spec: RuleSpec,
    /// Whether the rule takes part in routing.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// The rule's position among the user's own rules.
    pub order: u32,
}

fn default_true() -> bool {
    true
}

impl StoredRule {
    /// The rule's target, if it has one.
    pub fn target(&self) -> Option<&RuleTarget> {
        self.spec.target()
    }

    /// Whether this rule needs a geo database the engine must download.
    pub fn needs_geodata(&self) -> bool {
        self.spec.needs_geodata()
    }
}

/// One subscription as the app stores it.
///
/// The URL is *not* stored here — it lives in the secret store, because it
/// usually carries the token that authenticates the subscription. What is
/// stored is the metadata and the id under which the secret can be found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Subscription {
    /// The subscription's stable id.
    pub id: String,
    /// The label the user gave it.
    pub label: String,
    /// Who fetches the subscription.
    pub mode: SubscriptionMode,
    /// How often the subscription is refreshed, in seconds.
    pub interval_secs: u64,
    /// The optional `Authorization` header value, also a secret.
    pub has_auth_header: bool,
    /// When the subscription was last fetched successfully.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    /// The quota reported by the subscription's `subscription-userinfo` header.
    ///
    /// Only ever set for app-managed subscriptions — the engine's own fetcher
    /// discards that header, so for an engine-managed subscription the
    /// number does not exist and the UI says so rather than showing a zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<SubscriptionQuota>,
}

/// Who fetches a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SubscriptionMode {
    /// The app fetches, parses, validates, caches and merges the payload.
    #[default]
    AppManaged,
    /// The engine owns the fetch and the refresh; the app supervises.
    EngineManaged,
}

impl SubscriptionMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AppManaged => "app-managed",
            Self::EngineManaged => "engine-managed",
        }
    }
}

/// The traffic quota a subscription's provider reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionQuota {
    /// The bytes used so far.
    pub used: u64,
    /// The total bytes allowed, if the provider reports one.
    pub total: Option<u64>,
    /// When the quota resets, as reported by the provider.
    pub expires: Option<String>,
}

/// The parsed contents of an app-managed subscription.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct SubscriptionPayload {
    /// The subscription this payload came from.
    pub subscription_id: String,
    /// The nodes the payload carried.
    #[serde(default)]
    pub nodes: Vec<Node>,
    /// The proxy groups the payload carried.
    #[serde(default)]
    pub groups: Vec<ProxyGroup>,
    /// The rules the payload carried.
    #[serde(default)]
    pub rules: Vec<RuleSpec>,
    /// The settings the payload carried at the top level — the values the
    /// merge treats as subscription-level preferences (ports, mode, …).
    #[serde(default)]
    pub settings: SubscriptionSettings,
    /// How many entries were dropped during validation.
    #[serde(default)]
    pub dropped_entries: usize,
}

/// The top-level settings an app-managed subscription can carry.
///
/// These are the subscription-level preferences that the merge resolves
/// against the user's own (P1 and P2).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SubscriptionSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mixed_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socks_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_lan: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ProxyMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_level: Option<LogLevel>,
}

/// The complete set of inputs the configuration pipeline consumes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BuildInput {
    /// The user's own preferences.
    pub preferences: UserPreferences,
    /// The user's own rules.
    pub user_rules: Vec<StoredRule>,
    /// The user's own nodes.
    pub user_nodes: Vec<Node>,
    /// The user's own groups.
    pub user_groups: Vec<ProxyGroup>,
    /// The payloads of the app-managed subscriptions.
    pub subscriptions: Vec<SubscriptionPayload>,
    /// The declarations of the engine-managed subscriptions, which the
    /// generated config declares but the engine populates itself.
    pub engine_managed: Vec<EngineManagedSubscription>,
    /// The rule providers the generated config must declare.
    pub rule_providers: Vec<RuleProvider>,
}

/// An engine-managed subscription, as declared in the generated config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineManagedSubscription {
    /// The name the engine knows the subscription by.
    pub name: String,
    /// The URL, including its token.
    ///
    /// This is a secret. It only ever reaches the generated config file,
    /// which is written with restrictive permissions.
    pub url: String,
    /// The refresh interval in seconds.
    pub interval_secs: u64,
    /// The optional dialer used for the download.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
}

/// A rule provider the generated config declares.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProvider {
    /// The provider's name, as referenced by `RULE-SET,<name>,<target>`.
    pub name: String,
    /// The provider's kind (`http`, `file`, `inline`).
    #[serde(rename = "type")]
    pub provider_type: String,
    /// The provider's URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The provider's payload path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The provider's behaviour — the match semantics of its rules.
    #[serde(default)]
    pub behavior: RuleProviderBehavior,
    /// The refresh interval in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
}

/// A rule provider's behaviour — what its rules match on.
///
/// Source: `meow-config-0.29.0/src/raw.rs` — `RawRuleProvider.behavior`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleProviderBehavior {
    /// The rules match domains.
    #[default]
    Domain,
    /// The rules match IP CIDRs.
    IpCidr,
    /// The rules match classical rules.
    Classical,
}

/// The rule kinds a rule provider can supply.
pub const KNOWN_PROVIDER_BEHAVIORS: &[RuleProviderBehavior] = &[
    RuleProviderBehavior::Domain,
    RuleProviderBehavior::IpCidr,
    RuleProviderBehavior::Classical,
];

/// A problem found while building the configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// How serious the finding is.
    pub severity: Severity,
    /// The message, written for the user.
    pub message: String,
    /// What the finding is about, when it can be attributed to one thing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// How serious a finding is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The configuration cannot be applied.
    Error,
    /// The configuration can be applied, but something is probably wrong.
    Warning,
    /// Something worth knowing.
    Info,
}

/// The result of building a configuration.
///
/// The pipeline never mutates the running configuration: it produces a
/// candidate, and the caller decides whether to apply it. A build that
/// produced errors has no usable configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct BuildOutcome {
    /// The candidate configuration, if the build succeeded.
    pub config: Option<RuntimeConfig>,
    /// Everything the build found.
    pub findings: Vec<Finding>,
}

impl BuildOutcome {
    /// Whether the build produced an applicable configuration.
    pub fn is_ok(&self) -> bool {
        self.config.is_some()
    }

    /// The errors among the findings.
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
    }
}

/// The built configuration, ready to be validated and applied.
///
/// This is the app's own model of a configuration. Turning it into the
/// engine's YAML is [`RuntimeConfig::to_yaml`], and validating it is the
/// engine's `meow -t` — both happen outside this crate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// The HTTP proxy port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mixed_port: Option<u16>,
    /// The SOCKS proxy port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socks_port: Option<u16>,
    /// The HTTP proxy port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Whether the proxy listens on the LAN.
    pub allow_lan: bool,
    /// The address the proxy binds to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bind_address: Option<String>,
    /// The routing mode.
    pub mode: ProxyMode,
    /// The engine's log verbosity.
    pub log_level: LogLevel,
    /// The TUN settings.
    #[serde(skip_serializing_if = "TunSettings::is_disabled")]
    pub tun: TunSettings,
    /// The nodes.
    pub proxies: Vec<Node>,
    /// The proxy groups.
    pub proxy_groups: Vec<ProxyGroup>,
    /// The rules, in the order the engine will evaluate them.
    pub rules: Vec<String>,
    /// The rule providers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rule_providers: Vec<RuleProvider>,
    /// The engine-managed subscription declarations.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub subscriptions: Vec<EngineManagedSubscription>,
}

impl TunSettings {
    /// Whether these settings mean "no TUN at all".
    ///
    /// A disabled TUN with no other settings is omitted from the generated
    /// config entirely rather than written as `tun: {enable: false}`.
    pub fn is_disabled(&self) -> bool {
        !self.enabled && self.device.is_none() && self.mtu.is_none() && self.dns_hijack.is_none()
    }
}

/// The rule kinds the app knows how to describe in its own UI, for the
/// structured rule editor's payload field.
pub fn payload_hint(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Domain | RuleKind::DomainSuffix | RuleKind::DomainKeyword => "example.com",
        RuleKind::DomainRegex => "^ads\\..*$",
        RuleKind::DomainWildcard => "*.example.com",
        RuleKind::GeoSite => "category-ads-all",
        RuleKind::GeoIp | RuleKind::SrcGeoIp => "CN",
        RuleKind::IpCidr | RuleKind::SrcIpCidr => "192.0.2.0/24",
        RuleKind::IpSuffix | RuleKind::SrcIpSuffix => "192.0.2.0/24",
        RuleKind::IpAsn | RuleKind::SrcIpAsn => "4134",
        RuleKind::SrcPort | RuleKind::DstPort | RuleKind::InPort => "443",
        RuleKind::Network => "tcp",
        RuleKind::Dscp => "af11",
        RuleKind::ProcessName => "curl.exe",
        RuleKind::ProcessPath => "/usr/bin/curl",
        RuleKind::Uid => "1000",
        RuleKind::InName => "en0",
        RuleKind::InType => "Ethernet",
        RuleKind::InUser => "alice",
        RuleKind::RuleSet => "my-rule-set",
        RuleKind::SubRule => "my-block",
        RuleKind::And | RuleKind::Or | RuleKind::Not => "((DOMAIN,a.com),(DST-PORT,443))",
        RuleKind::Match => "",
    }
}
