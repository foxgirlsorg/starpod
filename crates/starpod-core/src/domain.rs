//! The domain types the rest of the app is built on.
//!
//! These mirror the parts of the engine's configuration the app actually
//! owns, plus the parts the user configures that the engine has no concept
//! of (a rule's identity, its enabled flag, its order).
//!
//! Everything here is plain data: serialisable, comparable, and free of
//! behaviour that touches the outside world.

use std::collections::{BTreeMap, BTreeSet};

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

/// The TUN network stack the engine runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunStack {
    /// The gVisor user-space stack (the engine's usual choice).
    Gvisor,
    /// A mix of the system stack for TCP and gVisor for UDP.
    Mixed,
    /// The operating system's own stack.
    System,
}

impl TunStack {
    /// The engine's spelling of the stack.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gvisor => "gvisor",
            Self::Mixed => "mixed",
            Self::System => "system",
        }
    }
}

/// How the engine looks up the process that opened a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FindProcessMode {
    /// Never look up processes.
    Off,
    /// Look up processes only for connections a rule needs them for.
    Strict,
    /// Look up the process for every connection.
    Always,
}

impl FindProcessMode {
    /// The engine's spelling of the mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Strict => "strict",
            Self::Always => "always",
        }
    }
}

/// How the engine answers DNS queries for proxied traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DnsEnhancedMode {
    /// Answer with addresses from the fake-IP range, and map them back later.
    FakeIp,
    /// Answer with the real addresses, redirecting by host name.
    RedirHost,
}

impl DnsEnhancedMode {
    /// The engine's spelling of the mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::FakeIp => "fake-ip",
            Self::RedirHost => "redir-host",
        }
    }
}

/// The geodata format the engine reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GeodataMode {
    /// The `.dat` (v2fly) format.
    Dat,
    /// The MaxMind `.mmdb` format.
    Mmdb,
}

/// Which engine release line the app installs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineChannel {
    /// Tagged, non-prerelease engine versions.
    #[default]
    Stable,
    /// The rolling alpha builds.
    Alpha,
}

/// The TUN settings the engine accepts beyond the basic [`TunSettings`].
///
/// Every field is optional: an unset field is not written, so the engine's
/// own default applies.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct TunOptions {
    /// The network stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<TunStack>,
    /// Whether traffic that cannot be routed is dropped instead of leaking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict_route: Option<bool>,
    /// Whether the engine installs firewall redirects (Linux).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_redirect: Option<bool>,
    /// Whether the engine detects the outbound interface on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_detect_interface: Option<bool>,
    /// Whether UDP NAT mappings are kept per destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_independent_nat: Option<bool>,
    /// CIDRs routed through TUN. Empty means the engine's default route set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_address: Vec<String>,
    /// CIDRs kept off TUN, for example a LAN or a VPN the user relies on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub route_exclude_address: Vec<String>,
}

/// The DNS block.
///
/// The modelled keys are the ones the app edits. Keys the app does not model
/// (a subscription's `fake-ip-filter-mode`, for example) are kept in
/// `extra` and written back unchanged, so a subscription's DNS is not
/// narrowed by reading it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct DnsSettings {
    /// Whether the engine's DNS server runs at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable: Option<bool>,
    /// The enhanced mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enhanced_mode: Option<DnsEnhancedMode>,
    /// The fake-IP pool, as a CIDR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fake_ip_range: Option<String>,
    /// Domains that always receive real addresses, never fake ones.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fake_ip_filter: Vec<String>,
    /// The main upstream servers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub nameserver: Vec<String>,
    /// Servers consulted when the main ones fail or answer with a foreign address.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback: Vec<String>,
    /// Plain-IP servers used to resolve the other servers' host names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_nameserver: Vec<String>,
    /// Servers used to resolve proxy server host names.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proxy_server_nameserver: Vec<String>,
    /// Per-domain server choices: domain pattern to servers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub nameserver_policy: BTreeMap<String, Vec<String>>,
    /// Whether the system hosts table answers before the upstream servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_hosts: Option<bool>,
    /// Keys the app does not model, written back unchanged.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// The sniffer block: recovers a domain name from the first bytes of a connection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct SnifferSettings {
    /// Whether the sniffer runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable: Option<bool>,
    /// Whether DNS answers are used to recover the domain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force_dns_mapping: Option<bool>,
    /// Whether connections to a bare IP address are sniffed too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parse_pure_ip: Option<bool>,
    /// Whether a sniffed domain replaces the destination address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub override_destination: Option<bool>,
    /// The ports to sniff per protocol: `HTTP`, `TLS` or `QUIC` to a list of
    /// ports or port ranges such as `"443"` or `"1000-2000"`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sniff: BTreeMap<String, Vec<String>>,
    /// Domains sniffed even when their address is an IP.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub force_domain: Vec<String>,
    /// Domains never sniffed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_domain: Vec<String>,
}

/// The geodata URLs the engine downloads from. Unset fields keep the engine's
/// default sources.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GeoxUrls {
    /// The GeoIP `.dat` source. The engine ignores it; it is written for
    /// configurations that still carry it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geoip: Option<String>,
    /// The geosite source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geosite: Option<String>,
    /// The GeoIP `.mmdb` source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mmdb: Option<String>,
    /// The ASN `.mmdb` source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asn: Option<String>,
}

/// The geodata settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct GeoSettings {
    /// The geodata format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<GeodataMode>,
    /// The download sources.
    #[serde(default, skip_serializing_if = "GeoxUrls::is_default")]
    pub geox_url: GeoxUrls,
    /// Whether the engine refreshes its geodata in the background.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_update: Option<bool>,
    /// How often the background refresh runs, in hours.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_interval_hours: Option<u32>,
}

impl GeoxUrls {
    /// Whether no source is set.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The engine settings the user can edit beyond the core routing ones.
///
/// Every field defaults to "not set". An unset field is not written to the
/// generated configuration, so the engine's own default applies and the
/// generated file is unchanged from before these settings existed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct EngineOptions {
    /// The TUN extras.
    #[serde(default, skip_serializing_if = "TunOptions::is_default")]
    pub tun: TunOptions,
    /// Whether IPv6 is handled at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<bool>,
    /// Whether the latency of a connection is measured the same way for every
    /// protocol (unified delay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unified_delay: Option<bool>,
    /// Whether TCP connections race several addresses at once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tcp_concurrent: Option<bool>,
    /// How the process of a connection is looked up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub find_process_mode: Option<FindProcessMode>,
    /// The keep-alive interval for TCP connections, in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive_interval: Option<u32>,
    /// Whether the user's DNS block replaces the subscription's. Off by default:
    /// the subscription's DNS is used when it has one.
    #[serde(default, skip_serializing_if = "is_false")]
    pub override_subscription_dns: bool,
    /// The user's DNS block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<DnsSettings>,
    /// The static hosts table: a domain to an IP address.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hosts: BTreeMap<String, String>,
    /// The sniffer block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sniffer: Option<SnifferSettings>,
    /// The geodata settings.
    #[serde(default, skip_serializing_if = "GeoSettings::is_default")]
    pub geo: GeoSettings,
}

/// The serde skip predicate for a `false` flag.
fn is_false(value: &bool) -> bool {
    !*value
}

impl TunOptions {
    /// Whether no TUN extra is set.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl GeoSettings {
    /// Whether no geodata setting is set.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

impl EngineOptions {
    /// Whether no engine option is set. Such options are not saved at all,
    /// so a file written before they existed stays byte-identical.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The default latency-test URL. It is the URL the app has always used.
pub const DEFAULT_DELAY_TEST_URL: &str = "https://www.gstatic.com/generate_204";
/// The default latency-test timeout, in milliseconds.
pub const DEFAULT_DELAY_TEST_TIMEOUT_MS: u32 = 5000;
/// The default number of engine log lines the app keeps.
pub const DEFAULT_LOG_RETENTION_LINES: u32 = 1000;

/// The global shortcut for each app action, as an accelerator string such as
/// `Ctrl+Alt+KeyS`. An empty string means the action has no shortcut.
///
/// The app parses and registers these (starpod-app `ui::hotkeys`); this type
/// only stores them, so the domain stays free of the shortcut library.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct HotkeyBindings {
    /// Show the window, or hide it when it is shown.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub show_window: String,
    /// Connect when disconnected, disconnect when connected.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub toggle_connect: String,
    /// Turn the system proxy on or off.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub toggle_system_proxy: String,
    /// Switch the routing mode to rule.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode_rule: String,
    /// Switch the routing mode to global.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode_global: String,
    /// Switch the routing mode to direct.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode_direct: String,
}

impl HotkeyBindings {
    /// Whether no action has a shortcut.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
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
    /// How often subscriptions refresh automatically, in minutes.
    ///
    /// `None` means the user has not chosen, and the app's default interval
    /// applies. `Some(0)` means manual refresh only. Files written before this
    /// field existed load with `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_refresh_minutes: Option<u32>,
    /// The member the user picked in each `select` group: group name to
    /// member name. Applied to every group on each build, so a choice
    /// survives a subscription refresh. Missing groups and members are
    /// skipped. Files written before this field existed load empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub group_selections: BTreeMap<String, String>,
    /// Whether the app checks GitHub for a newer release at startup.
    ///
    /// `None` means the default, which is on. Files written before this field
    /// existed load as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check_updates: Option<bool>,
    /// The global shortcuts, one accelerator per action. All empty by default,
    /// which means no shortcut is registered.
    #[serde(default, skip_serializing_if = "HotkeyBindings::is_empty")]
    pub hotkeys: HotkeyBindings,
    /// The engine settings beyond the core routing ones (TUN extras, IPv6,
    /// DNS, sniffer, geodata, and so on). Unset fields are not written.
    #[serde(default, skip_serializing_if = "EngineOptions::is_default")]
    pub engine: EngineOptions,
    /// The URL the latency test measures against. `None` means
    /// [`DEFAULT_DELAY_TEST_URL`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_test_url: Option<String>,
    /// The latency-test timeout in milliseconds. `None` means
    /// [`DEFAULT_DELAY_TEST_TIMEOUT_MS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_test_timeout_ms: Option<u32>,
    /// How many engine log lines the app keeps. `None` means
    /// [`DEFAULT_LOG_RETENTION_LINES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_retention_lines: Option<u32>,
    /// Which engine release line to install. `None` means stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine_channel: Option<EngineChannel>,
    /// When the geodata was last refreshed, in Unix seconds. Written by the
    /// app after a successful refresh, never by the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geo_updated_at: Option<i64>,
    /// Whether the app sends device identification (`x-hwid`, `x-device-os`,
    /// `x-ver-os`, `x-device-model`) with each subscription fetch.
    ///
    /// Defaults to `true`, which is what providers that limit devices need.
    /// When `false`, none of the four headers is sent. Files written before
    /// this field existed load as `true`.
    #[serde(default = "default_true")]
    pub send_device_id: bool,
    /// Whether Starpod points the operating system's proxy at the running
    /// engine. Off by default: the proxy is only ever set by an explicit
    /// choice.
    #[serde(default)]
    pub system_proxy: bool,
    /// How the system proxy is set: a fixed server, or a PAC script.
    #[serde(default)]
    pub sysproxy_mode: SystemProxyMode,
    /// The hosts the system proxy is not used for, in the operating system's
    /// own syntax. See [`default_sysproxy_bypass`].
    #[serde(default = "default_sysproxy_bypass")]
    pub sysproxy_bypass: String,
    /// The PAC script served in PAC mode. `%mixed-port%` is replaced with the
    /// engine's mixed port. See [`default_pac_script`].
    #[serde(default = "default_pac_script")]
    pub pac_script: String,
    /// Whether Starpod starts when the user logs in.
    #[serde(default)]
    pub autostart: bool,
    /// Whether an autostarted Starpod stays hidden in the tray instead of
    /// showing its window. Only used when `autostart` launched the app.
    #[serde(default)]
    pub silent_start: bool,
    /// Whether Starpod connects once its state has loaded, when a subscription
    /// supplies servers.
    #[serde(default)]
    pub auto_connect_on_start: bool,
    /// How the Proxies page orders each group's members: `0` as configured,
    /// `1` by latency (untested and timed-out members last), `2` by name.
    /// Files written before this field existed load as `0`.
    #[serde(default)]
    pub proxies_sort: u8,
    /// Whether the Proxies page hides the members whose last test timed out.
    /// Files written before this field existed load as `false`.
    #[serde(default)]
    pub hide_unavailable: bool,
    /// The Proxies groups the user has opened, by name. Every other group is
    /// shown collapsed. Files written before this field existed load as empty.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub expanded_groups: BTreeSet<String>,
    /// Whether choosing a member of a group closes the connections that went
    /// through that group, so they re-open on the new member. On by default,
    /// as in Koala. Files written before this field existed load as `true`.
    #[serde(default = "default_true")]
    pub auto_close_connections_on_switch: bool,
    /// Whether closing the window hides Starpod to the system tray and the
    /// proxy keeps running. Off, closing stops the engine and quits. Without a
    /// tray, closing always quits. Files written before this field existed
    /// load as `true`.
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    /// The UI language: `""` follows the system locale, `"en"` or `"ru"`
    /// pins it. Files written before this field existed load as `""`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub language: String,
    /// The accent colour as `#rrggbb`. `""` means the Starpod red,
    /// [`DEFAULT_ACCENT_COLOR`]. Files written before this field existed load
    /// as `""`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub accent_color: String,
}

/// The accent colour used when the user has not picked one.
pub const DEFAULT_ACCENT_COLOR: &str = "#de6161";

impl UserPreferences {
    /// The accent colour to paint with: the saved one, or the default.
    pub fn effective_accent_color(&self) -> &str {
        if self.accent_color.is_empty() {
            DEFAULT_ACCENT_COLOR
        } else {
            &self.accent_color
        }
    }
}

/// How the system proxy is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SystemProxyMode {
    /// A fixed HTTP, HTTPS and SOCKS server at the engine's mixed port.
    #[default]
    Manual,
    /// An auto-config (PAC) script that Starpod serves on loopback.
    Pac,
}

impl SystemProxyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Pac => "pac",
        }
    }

    pub fn from_str_exact(value: &str) -> Option<Self> {
        Some(match value {
            "manual" => Self::Manual,
            "pac" => Self::Pac,
            _ => return None,
        })
    }
}

/// The default bypass list for the system proxy, in the syntax the operating
/// system expects: `;`-separated on Windows, `,`-separated elsewhere.
pub fn default_sysproxy_bypass() -> String {
    if cfg!(windows) {
        "localhost;127.*;192.168.*;10.*;172.16.*;172.17.*;172.18.*;172.19.*;172.20.*;172.21.*;172.22.*;172.23.*;172.24.*;172.25.*;172.26.*;172.27.*;172.28.*;172.29.*;172.30.*;172.31.*;<local>".to_string()
    } else {
        "localhost,127.0.0.1/8,::1,192.168.0.0/16,10.0.0.0/8,172.16.0.0/12".to_string()
    }
}

/// The default PAC script. Private and loopback hosts go direct; every other
/// host goes to the engine's mixed port, falling back to direct if the engine
/// does not answer.
pub fn default_pac_script() -> String {
    [
        "function FindProxyForURL(url, host) {",
        "  if (isPlainHostName(host) ||",
        "      isInNet(host, \"127.0.0.0\", \"255.0.0.0\") ||",
        "      isInNet(host, \"10.0.0.0\", \"255.0.0.0\") ||",
        "      isInNet(host, \"172.16.0.0\", \"255.240.0.0\") ||",
        "      isInNet(host, \"192.168.0.0\", \"255.255.0.0\"))",
        "    return \"DIRECT\";",
        "  return \"PROXY 127.0.0.1:%mixed-port%; DIRECT\";",
        "}",
        "",
    ]
    .join("\n")
}

impl Default for UserPreferences {
    /// A new user has chosen nothing. The ports are left unset on purpose:
    /// setting them here would make them the user's explicit choice, and a
    /// subscription's own ports could then never apply. The merge supplies
    /// [`DEFAULT_MIXED_PORT`](crate::config::DEFAULT_MIXED_PORT) and
    /// [`DEFAULT_SOCKS_PORT`](crate::config::DEFAULT_SOCKS_PORT) when neither
    /// the user nor a subscription sets them.
    fn default() -> Self {
        Self {
            mixed_port: None,
            socks_port: None,
            port: None,
            allow_lan: None,
            bind_address: None,
            mode: None,
            log_level: None,
            tun: TunSettings::default(),
            selected_group: None,
            selected_node_id: None,
            auto_refresh_minutes: None,
            group_selections: BTreeMap::new(),
            check_updates: None,
            hotkeys: HotkeyBindings::default(),
            send_device_id: true,
            system_proxy: false,
            sysproxy_mode: SystemProxyMode::default(),
            sysproxy_bypass: default_sysproxy_bypass(),
            pac_script: default_pac_script(),
            autostart: false,
            silent_start: false,
            auto_connect_on_start: false,
            proxies_sort: 0,
            hide_unavailable: false,
            expanded_groups: BTreeSet::new(),
            auto_close_connections_on_switch: true,
            close_to_tray: true,
            language: String::new(),
            accent_color: String::new(),
            engine: EngineOptions::default(),
            delay_test_url: None,
            delay_test_timeout_ms: None,
            log_retention_lines: None,
            engine_channel: None,
            geo_updated_at: None,
        }
    }
}

impl UserPreferences {
    /// The latency-test URL in effect.
    pub fn effective_delay_test_url(&self) -> &str {
        self.delay_test_url
            .as_deref()
            .unwrap_or(DEFAULT_DELAY_TEST_URL)
    }

    /// The latency-test timeout in effect, in milliseconds.
    pub fn effective_delay_test_timeout_ms(&self) -> u32 {
        self.delay_test_timeout_ms
            .unwrap_or(DEFAULT_DELAY_TEST_TIMEOUT_MS)
    }

    /// The number of engine log lines kept, in effect.
    pub fn effective_log_retention_lines(&self) -> u32 {
        self.log_retention_lines
            .unwrap_or(DEFAULT_LOG_RETENTION_LINES)
    }

    /// The engine release line in effect.
    pub fn effective_engine_channel(&self) -> EngineChannel {
        self.engine_channel.unwrap_or_default()
    }
}

/// A proxy node as the app understands it.
///
/// The engine's own node format is a large open map of protocol-specific
/// fields. The app keeps the fields it needs to *display* and *select* a
/// node, and keeps the untouched original map alongside, so that fields the
/// app does not model are still passed to the engine verbatim.
///
/// It is stored as one flat object: the original map with the app's fields
/// written over it (see [`flat_object`]).
#[derive(Debug, Clone, PartialEq)]
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
    pub tags: Vec<String>,
    /// The original engine-format node, passed through untouched.
    pub raw: serde_json::Map<String, serde_json::Value>,
}

/// A proxy group as the app understands it.
///
/// Stored like [`Node`]: one flat object.
#[derive(Debug, Clone, PartialEq)]
pub struct ProxyGroup {
    /// The group's unique id.
    pub id: String,
    /// The group's display name, unique within the configuration.
    pub name: String,
    /// The group type (`select`, `url-test`, `fallback`, `load-balance`, …).
    pub group_type: String,
    /// The group's members, by node or group name.
    pub members: Vec<String>,
    /// The selection strategy, for the strategies that have one.
    pub strategy: Option<String>,
    /// The health-check URL, for the groups that have one.
    pub url: Option<String>,
    /// The health-check interval in seconds.
    pub interval: Option<u64>,
    /// The original engine-format group, passed through untouched.
    pub raw: serde_json::Map<String, serde_json::Value>,
}

// ----- the stored form of nodes and groups --------------------------------------
//
// A node's `raw` map holds the engine's own keys, and some share a name with
// one of the app's fields (`name`, `server`, `port`; for a group also `type`,
// `strategy`, `url`, `interval`). A derived `#[serde(flatten)]` wrote such a
// key twice, and serde then refused to read the object back ("duplicate
// field"), so a cached subscription never loaded after a restart. The stored
// form is one object: `raw`, with the app's fields written over it. Reading
// goes through a JSON map, which keeps the last of duplicate keys, so files
// written the old way load too.

/// `raw` with `fields` written over it, as one object. A `None` field is
/// left out.
fn flat_object(
    raw: &serde_json::Map<String, serde_json::Value>,
    fields: Vec<(&str, Option<serde_json::Value>)>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut object = raw.clone();
    for (key, value) in fields {
        match value {
            Some(value) => {
                object.insert(key.to_string(), value);
            }
            None => {
                object.remove(key);
            }
        }
    }
    object
}

/// Take the app-only key `key` out of `object`.
fn take<T: serde::de::DeserializeOwned, E: serde::de::Error>(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<T>, E> {
    match object.remove(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(|error| E::custom(format!("`{key}`: {error}"))),
    }
}

/// Read the engine key `key` from `object`, leaving it in place for `raw`.
fn peek<T: serde::de::DeserializeOwned, E: serde::de::Error>(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<Option<T>, E> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value.clone())
            .map(Some)
            .map_err(|error| E::custom(format!("`{key}`: {error}"))),
    }
}

/// Remove `keys` whose value is `null`: an older build wrote an absent
/// field as `null`, which is not an engine value.
fn drop_nulls(object: &mut serde_json::Map<String, serde_json::Value>, keys: &[&str]) {
    for key in keys {
        if object.get(*key) == Some(&serde_json::Value::Null) {
            object.remove(*key);
        }
    }
}

impl Serialize for Node {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde_json::Value;
        let tags = (!self.tags.is_empty()).then(|| Value::from(self.tags.clone()));
        flat_object(
            &self.raw,
            vec![
                ("id", Some(Value::from(self.id.clone()))),
                ("name", Some(Value::from(self.name.clone()))),
                ("protocol", Some(Value::from(self.protocol.clone()))),
                ("server", self.server.clone().map(Value::from)),
                ("port", self.port.map(Value::from)),
                ("tags", tags),
            ],
        )
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut object = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        // The app's own keys come out; the engine's stay in `raw`.
        let id = take(&mut object, "id")?.ok_or_else(|| D::Error::missing_field("id"))?;
        let protocol =
            take(&mut object, "protocol")?.ok_or_else(|| D::Error::missing_field("protocol"))?;
        let tags = take(&mut object, "tags")?.unwrap_or_default();
        let name = peek(&object, "name")?.ok_or_else(|| D::Error::missing_field("name"))?;
        let server = peek(&object, "server")?;
        let port = peek(&object, "port")?;
        drop_nulls(&mut object, &["server", "port"]);
        Ok(Node {
            id,
            name,
            protocol,
            server,
            port,
            tags,
            raw: object,
        })
    }
}

impl Serialize for ProxyGroup {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde_json::Value;
        flat_object(
            &self.raw,
            vec![
                ("id", Some(Value::from(self.id.clone()))),
                ("name", Some(Value::from(self.name.clone()))),
                ("type", Some(Value::from(self.group_type.clone()))),
                ("members", Some(Value::from(self.members.clone()))),
                ("strategy", self.strategy.clone().map(Value::from)),
                ("url", self.url.clone().map(Value::from)),
                ("interval", self.interval.map(Value::from)),
            ],
        )
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ProxyGroup {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let mut object = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
        let id = take(&mut object, "id")?.ok_or_else(|| D::Error::missing_field("id"))?;
        let members = take(&mut object, "members")?.unwrap_or_default();
        let name = peek(&object, "name")?.ok_or_else(|| D::Error::missing_field("name"))?;
        let group_type = peek(&object, "type")?.ok_or_else(|| D::Error::missing_field("type"))?;
        let strategy = peek(&object, "strategy")?;
        let url = peek(&object, "url")?;
        let interval = peek(&object, "interval")?;
        drop_nulls(&mut object, &["strategy", "url", "interval"]);
        Ok(ProxyGroup {
            id,
            name,
            group_type,
            members,
            strategy,
            url,
            interval,
            raw: object,
        })
    }
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
    /// When the subscription was last fetched successfully, as RFC 3339.
    ///
    /// `None` means it has never been fetched successfully. A *failed*
    /// refresh does not change this field: the timestamp records the last
    /// fetch that worked, so the user can see how stale the working data
    /// is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<String>,
    /// The quota reported by the subscription's `subscription-userinfo` header.
    ///
    /// Only ever set for app-managed subscriptions — the engine's own fetcher
    /// discards that header, so for an engine-managed subscription the
    /// number does not exist and the UI says so rather than showing a zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quota: Option<SubscriptionQuota>,
    /// How many nodes the last valid payload carried.
    ///
    /// Zero for an engine-managed subscription: the app never sees its
    /// payload, so it cannot count what is in it.
    #[serde(default)]
    pub node_count: usize,
    /// How many proxy groups the last valid payload carried.
    #[serde(default)]
    pub group_count: usize,
    /// The last error, written for the user.
    ///
    /// The message is redacted at the point it is created — a provider
    /// error can carry a URL, and a URL carries the token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The profile title the provider sent in its `profile-title` header.
    ///
    /// Taken on a successful refresh; a header that is absent clears it.
    #[serde(default)]
    pub profile_title: Option<String>,
    /// The announcement the provider sent in its `announce` header.
    #[serde(default)]
    pub announce: Option<String>,
    /// The provider's support link, from the `support-url` header (http or
    /// https only).
    #[serde(default)]
    pub support_url: Option<String>,
    /// The provider's web page, from the `profile-web-page-url` header (http
    /// or https only).
    #[serde(default)]
    pub web_url: Option<String>,
    /// The label for the web page link, from `profile-web-page-name`.
    #[serde(default)]
    pub web_page_name: Option<String>,
    /// The link shown with the announcement, from `announce-url` (http or
    /// https only).
    #[serde(default)]
    pub announce_url: Option<String>,
    /// The file name of the cached logo, from `profile-logo`. The file lives in
    /// the subscription logo directory. `None` when there is no logo.
    #[serde(default)]
    pub logo_file: Option<String>,
    /// Whether the provider disallows the Global routing mode while this
    /// subscription is active (`global-mode: false`).
    #[serde(default)]
    pub global_mode_disabled: bool,
    /// The provider's `expand-proxy-groups` header. `Some(false)` means the
    /// proxy groups start collapsed; `None` means no preference was sent.
    #[serde(default)]
    pub expand_proxy_groups: Option<bool>,
    /// Whether the provider reported that this device has reached its device
    /// limit on the last refresh. Cleared by a successful refresh.
    #[serde(default)]
    pub device_limit: bool,
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
    /// The rule providers the payload declared under `rule-providers:`.
    ///
    /// Caches written before this field existed have none; they still load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rule_providers: Vec<RuleProvider>,
    /// The settings the payload carried at the top level — the values the
    /// merge treats as subscription-level preferences (ports, mode, …).
    #[serde(default)]
    pub settings: SubscriptionSettings,
    /// The subscription's own `dns:` block, when it has one. Used unless the
    /// user turns on their own DNS override. Caches written before this field
    /// existed load with `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dns: Option<DnsSettings>,
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
///
/// # What the engine (meow-rs v0.29.0) accepts
///
/// Sources: `meow-config/src/raw.rs` (`RawRuleProvider`, lines 710-737,
/// `#[serde(rename_all = "kebab-case")]`), `meow-config/src/rule_provider.rs`
/// (loader: `load_one`, `load_file`, `load_http`, `resolve_path`,
/// `parse_explicit_format`; the line numbers were not reliably visible when
/// fetched, so these are function names) and `meow-config/src/rule_parser.rs`
/// (`try_parse_rule_set`, around lines 136-166). Fetched from the v0.29.0 tag
/// on GitHub; nothing was run against a live engine.
///
/// - `type`: `http`, `file` or `inline`. The app accepts `http` and `inline`
///   only; a subscription never gets to point the engine at a local `file`.
/// - `behavior`: a string parsed into the engine's match kind before the type
///   is dispatched on. Values: `domain`, `ipcidr`, `classical`. An unknown value
///   fails that provider (dropped; fatal under `strict`).
/// - `format`: `yaml` (the default), `text` or `mrs`. `mrs` is also detected
///   from the magic bytes whatever is declared. An unknown value fails the load.
/// - `url`: the download address for `http`.
/// - `path`: read by `http` and `file` only. For `http` it is an on-disk cache
///   file under the engine's cache directory: optional, resolved with
///   `resolve_contained`, so it cannot escape that directory (`..` and
///   symlinks are rejected). With no cache directory (`--config-string`
///   validation) an `http` path is ignored. The subscription's own `path` is
///   never used; the app writes `rule-providers/<name>.<ext>`.
/// - `interval`: refresh period in seconds for `http` (0 or absent: loaded
///   once, revalidated on each start). Ignored with a warning for `file`;
///   rejected (provider fails) for `inline` when above zero.
/// - `proxy`: `http` only. `DIRECT` (case-insensitive) or the name of a proxy
///   or group to fetch through. An unknown name fails the fetch. Absent means
///   the engine's global default download route.
/// - `payload`: `inline` only, and required there: a list of rule strings.
/// - `header`: `http` only, custom request headers. Not modelled here.
///
/// `RULE-SET,<name>,<target>[,<flags>]`: `try_parse_rule_set` splits the line
/// into at most four fields (`splitn(4, ',')`). Field 3 is the target, field 4
/// is passed whole to the flag parser, so `no-resolve` (and `src`) are
/// accepted. An unknown provider name is an error (`unknown rule-provider`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleProvider {
    /// The provider's name, as referenced by `RULE-SET,<name>,<target>`.
    pub name: String,
    /// The provider's kind (`http`, `file`, `inline`).
    #[serde(rename = "type")]
    pub provider_type: String,
    /// The provider's URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The provider's payload path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The provider's behaviour — the match semantics of its rules.
    #[serde(default)]
    pub behavior: RuleProviderBehavior,
    /// The refresh interval in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval: Option<u64>,
    /// The payload format: `yaml`, `text` or `mrs`. The engine defaults to
    /// `yaml` when this is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// The rule strings of an `inline` provider. Required for `inline`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Vec<String>>,
    /// The proxy or group an `http` provider is fetched through, or `DIRECT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy: Option<String>,
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
    /// The group the Home page offers first, when the build succeeded and
    /// has any group. It is the `MATCH` tail's target when that is a group
    /// (see the P5 rule in `config`); otherwise the first `select` group a
    /// subscription supplies, else the first `select` group, else the first
    /// group.
    pub primary_group: Option<String>,
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
/// engine's YAML happens in `starpod-engine`, which is the only crate that
/// knows the engine's key names — and it must, because the engine ignores
/// keys it does not recognise. Validating it is the engine's `meow -t`,
/// which also happens outside this crate.
///
/// The separation is deliberate: this crate stays pure, so the whole
/// pipeline can be exercised with fixtures and no engine on the machine.
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
    /// The engine settings beyond the core routing ones. Each unset field is
    /// omitted from the generated configuration.
    #[serde(default, skip_serializing_if = "EngineOptions::is_default")]
    pub engine: EngineOptions,
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

#[cfg(test)]
mod preference_tests {
    use super::*;

    #[test]
    fn a_new_user_has_chosen_no_ports() {
        let prefs = UserPreferences::default();
        assert_eq!(prefs.mixed_port, None);
        assert_eq!(prefs.socks_port, None);
    }

    #[test]
    fn a_preferences_file_written_before_the_refresh_setting_still_loads() {
        let old = r#"{"mixed_port":1080}"#;
        let prefs: UserPreferences = serde_json::from_str(old).expect("the old file loads");
        assert_eq!(prefs.mixed_port, Some(1080));
        assert_eq!(prefs.auto_refresh_minutes, None);
    }

    #[test]
    fn an_unset_refresh_setting_is_not_written() {
        let text = serde_json::to_string(&UserPreferences::default()).expect("serialises");
        assert!(!text.contains("auto_refresh_minutes"), "{text}");
    }

    #[test]
    fn the_refresh_setting_round_trips_including_manual_only() {
        for minutes in [Some(0), Some(30), None] {
            let prefs = UserPreferences {
                auto_refresh_minutes: minutes,
                ..UserPreferences::default()
            };
            let text = serde_json::to_string(&prefs).expect("serialises");
            let back: UserPreferences = serde_json::from_str(&text).expect("deserialises");
            assert_eq!(back.auto_refresh_minutes, minutes);
        }
    }

    #[test]
    fn a_preferences_file_written_before_group_selections_still_loads() {
        let old = r#"{"mixed_port":1080,"selected_group":"Proxy"}"#;
        let prefs: UserPreferences = serde_json::from_str(old).expect("the old file loads");
        assert!(prefs.group_selections.is_empty());
        assert_eq!(prefs.selected_group.as_deref(), Some("Proxy"));
    }

    #[test]
    fn a_preferences_file_written_before_the_proxy_list_settings_still_loads() {
        let old = r#"{"mixed_port":1080}"#;
        let prefs: UserPreferences = serde_json::from_str(old).expect("the old file loads");
        assert_eq!(prefs.proxies_sort, 0);
        assert!(!prefs.hide_unavailable);
        assert!(
            prefs.auto_close_connections_on_switch,
            "auto-close is on unless the user turned it off"
        );
    }

    #[test]
    fn the_proxy_list_settings_round_trip() {
        let prefs = UserPreferences {
            proxies_sort: 2,
            hide_unavailable: true,
            auto_close_connections_on_switch: false,
            ..UserPreferences::default()
        };
        let text = serde_json::to_string(&prefs).expect("serialises");
        let back: UserPreferences = serde_json::from_str(&text).expect("deserialises");
        assert_eq!(back, prefs);
    }

    #[test]
    fn group_selections_round_trip() {
        let mut prefs = UserPreferences::default();
        prefs
            .group_selections
            .insert("Proxy".to_string(), "Node A".to_string());
        let text = serde_json::to_string(&prefs).expect("serialises");
        let back: UserPreferences = serde_json::from_str(&text).expect("deserialises");
        assert_eq!(
            back.group_selections.get("Proxy").map(String::as_str),
            Some("Node A")
        );
    }

    #[test]
    fn a_subscription_written_before_the_profile_fields_still_loads() {
        let old = r#"{"id":"sub-1","label":"Airport","mode":"app-managed","interval_secs":3600,"has_auth_header":false,"node_count":2,"group_count":1}"#;
        let subscription: Subscription = serde_json::from_str(old).expect("the old entry loads");
        assert_eq!(subscription.label, "Airport");
        assert_eq!(subscription.profile_title, None);
        assert_eq!(subscription.announce, None);
        assert_eq!(subscription.support_url, None);
        assert_eq!(subscription.web_url, None);
    }
}

#[cfg(test)]
mod subscription_payload_tests {
    use super::*;

    fn object(json: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        json.as_object().unwrap().clone()
    }

    #[test]
    fn a_cached_node_and_group_read_back_as_they_were_written() {
        let node = Node {
            id: "Sub / ::A@a.example.com:443".into(),
            name: "Sub / A".into(),
            protocol: "vless".into(),
            server: Some("a.example.com".into()),
            port: Some(443),
            tags: vec![],
            raw: object(serde_json::json!({
                "name": "A", "type": "vless", "server": "a.example.com", "port": 443, "uuid": "x"
            })),
        };
        let group = ProxyGroup {
            id: "g".into(),
            name: "Sub / G".into(),
            group_type: "url-test".into(),
            members: vec!["Sub / A".into()],
            strategy: None,
            url: Some("https://example.com/204".into()),
            interval: Some(300),
            raw: object(serde_json::json!({
                "name": "Sub / G", "type": "url-test", "proxies": ["Sub / A"],
                "url": "https://example.com/204", "interval": 300
            })),
        };
        let text = serde_json::to_string(&(&node, &group)).unwrap();
        let (node_back, group_back): (Node, ProxyGroup) =
            serde_json::from_str(&text).expect("no duplicate keys");
        assert_eq!(node_back.id, node.id);
        assert_eq!(node_back.name, node.name);
        assert_eq!((node_back.server, node_back.port), (node.server, node.port));
        assert_eq!(node_back.raw.get("uuid"), node.raw.get("uuid"));
        assert_eq!(node_back.raw.get("type"), node.raw.get("type"));
        assert_eq!(group_back.members, group.members);
        assert_eq!(
            (group_back.url, group_back.interval),
            (group.url, group.interval)
        );
        assert_eq!(group_back.group_type, "url-test");
    }

    #[test]
    fn a_node_written_with_duplicate_keys_by_an_older_build_still_loads() {
        let old = r#"{"id":"n","name":"Sub / A","protocol":"direct","server":null,"port":null,
            "tags":[],"name":"A","type":"direct","udp":true}"#;
        let node: Node = serde_json::from_str(old).expect("the old cache loads");
        assert_eq!(node.protocol, "direct");
        assert_eq!(node.server, None);
        assert!(
            !node.raw.contains_key("server"),
            "a null is not an engine value"
        );
        assert_eq!(node.raw.get("udp"), Some(&serde_json::Value::Bool(true)));
    }

    #[test]
    fn a_cache_written_before_rule_providers_still_loads() {
        // The shape a payload had before it carried rule providers.
        let old =
            r#"{"subscription_id":"sub-1","nodes":[],"groups":[],"rules":[],"dropped_entries":2}"#;
        let payload: SubscriptionPayload = serde_json::from_str(old).expect("the old cache loads");
        assert!(payload.rule_providers.is_empty());
        assert_eq!(payload.dropped_entries, 2);
    }

    #[test]
    fn a_rule_provider_round_trips_with_its_new_fields() {
        let provider = RuleProvider {
            name: "ukraine".to_string(),
            provider_type: "http".to_string(),
            url: Some("https://example.test/ukraine.txt".to_string()),
            path: Some("rule-providers/ukraine.txt".to_string()),
            behavior: RuleProviderBehavior::Domain,
            interval: Some(86_400),
            format: Some("text".to_string()),
            payload: None,
            proxy: Some("DIRECT".to_string()),
        };
        let text = serde_json::to_string(&provider).expect("serialises");
        let back: RuleProvider = serde_json::from_str(&text).expect("deserialises");
        assert_eq!(back, provider);
    }

    #[test]
    fn absent_optional_fields_are_not_written() {
        let provider = RuleProvider {
            name: "inline-one".to_string(),
            provider_type: "inline".to_string(),
            url: None,
            path: None,
            behavior: RuleProviderBehavior::Classical,
            interval: None,
            format: None,
            payload: Some(vec!["DOMAIN,example.test".to_string()]),
            proxy: None,
        };
        let text = serde_json::to_string(&provider).expect("serialises");
        assert!(!text.contains("format"), "{text}");
        assert!(!text.contains("proxy"), "{text}");
        assert!(!text.contains("interval"), "{text}");
    }
}
