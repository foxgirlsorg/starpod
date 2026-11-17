//! The view state the UI renders.
//!
//! An [`AppSnapshot`] is plain data: it can be cloned, compared and sent to
//! the UI thread. It deliberately holds no URL, no `Authorization` header, no
//! node credential and no raw engine-format node, so nothing in it can leak a
//! secret even when it is printed with `{:?}`.

use std::path::PathBuf;

use starpod_core::rules::{BuiltinTarget, RuleKind, translate};
use starpod_core::{
    ProxyMode, RuleSpec, RuntimeConfig, Subscription, SubscriptionMode, SubscriptionQuota,
    UserPreferences, domain::payload_hint,
};
use starpod_engine::EnginePhase;
use starpod_subscription::pipeline::NAMESPACE_SEPARATOR;

use crate::connections::ConnectionsView;

/// Why Connect is blocked when the engine is not installed.
pub const ENGINE_MISSING_REASON: &str = "Install the engine first.";
/// Why Connect is blocked when there is no subscription at all.
pub const NO_SUBSCRIPTION_REASON: &str = "Add a subscription first";
/// Why Connect is blocked when subscriptions exist but none supplies servers.
pub const NO_SERVERS_REASON: &str = "Refresh your subscription — it has no servers yet";

/// The one rule for connecting, and the reason it blocks, if it does.
///
/// Connecting needs at least one subscription that supplies servers. An
/// engine-managed subscription counts: the engine holds its nodes. An
/// app-managed one counts only with a cached payload that has at least one
/// node, so a subscription that never fetched successfully does not. Without
/// this rule the generated configuration has no nodes and the engine runs a
/// proxy that sends everything direct.
///
/// Each subscription is paired with the node count of its cached payload,
/// `None` when it has no cached payload. The service enforces this rule for
/// connect and restart; the snapshot reports it so the UI can show it.
pub(crate) fn connect_blocked_reason(
    engine_installed: bool,
    subscriptions: &[(&Subscription, Option<usize>)],
) -> Option<String> {
    if !engine_installed {
        return Some(crate::i18n::t(ENGINE_MISSING_REASON).to_string());
    }
    if subscriptions.is_empty() {
        return Some(crate::i18n::t(NO_SUBSCRIPTION_REASON).to_string());
    }
    let usable = subscriptions
        .iter()
        .any(|(subscription, cached_nodes)| match subscription.mode {
            SubscriptionMode::EngineManaged => true,
            SubscriptionMode::AppManaged => cached_nodes.is_some_and(|count| count > 0),
        });
    (!usable).then(|| crate::i18n::t(NO_SERVERS_REASON).to_string())
}

/// How serious a banner is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeKind {
    /// Something worth knowing.
    Info,
    /// Something went wrong, but the app is still in a known state.
    Warning,
    /// The last action failed.
    Error,
}

/// The engine release installed, the newest one on the chosen channel, and
/// whether they differ.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EngineUpdateView {
    /// The installed release's label, or `None` when no engine is installed.
    pub installed_version: Option<String>,
    /// The newest release on the chosen channel, as of the last check.
    pub latest_version: Option<String>,
    /// Whether the newest release differs from the installed one.
    pub update_available: bool,
    /// The release line the app installs from.
    pub channel: starpod_core::domain::EngineChannel,
}

/// Everything the UI renders, as one value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppSnapshot {
    /// The engine release installed and the newest one available.
    pub engine_update: EngineUpdateView,
    /// When the geodata was last refreshed, in Unix seconds.
    pub geo_updated_at: Option<i64>,
    /// The proxy engine.
    pub engine: EngineView,
    /// The effective routing mode.
    pub mode: ProxyMode,
    /// The IP address of the server in use, without a port. A host name is
    /// shown until it resolves. `None` when not connected, or when the node
    /// in use has no server.
    pub server_address: Option<String>,
    /// The node the user selected, if it is in the current configuration.
    pub current_node: Option<NodeView>,
    /// Every subscription the user added.
    pub subscriptions: Vec<SubscriptionView>,
    /// The nodes in the current configuration, user and subscription alike.
    pub nodes: Vec<NodeView>,
    /// The user's own rules, in the order they are applied.
    pub rules: Vec<RuleView>,
    /// The rules subscriptions provide, as they arrived. They come after the
    /// user's rules in the configuration. A `MATCH` a subscription supplies
    /// is listed here; the configuration's tail takes its target, unless the
    /// user selected a group.
    pub subscription_rules: Vec<RuleView>,
    /// The rule kinds the rule editor offers.
    pub rule_kinds: Vec<RuleKindView>,
    /// The targets a rule may send traffic to.
    pub rule_targets: Vec<String>,
    /// The proxy groups of the last successfully composed configuration, in
    /// configuration order.
    pub groups: Vec<GroupView>,
    /// The group the Home page offers first. See
    /// [`starpod_core::BuildOutcome::primary_group`]. `None` when there are no
    /// groups.
    pub primary_group: Option<String>,
    /// One announcement per subscription whose provider sent a non-empty
    /// `announce` header, and one device-limit notice per subscription at its
    /// device limit.
    pub announcements: Vec<AnnouncementView>,
    /// The profile card of the active subscription, or `None` when there is no
    /// subscription. The active one is the selected subscription, else the
    /// first.
    pub profile_card: Option<ProfileCard>,
    /// Whether the Global routing mode may be chosen. `false` while an active
    /// subscription sets `global-mode: false`.
    pub global_mode_allowed: bool,
    /// The user's own settings.
    pub preferences: UserPreferences,
    /// The engine's open connections and the history of closed ones. Empty
    /// while the engine is not running.
    pub connections: ConnectionsView,
    /// Warnings from the last successful build.
    pub warnings: Vec<String>,
    /// The app's own log, oldest first.
    pub log_lines: Vec<String>,
    /// The engine's output, oldest first, redacted.
    pub engine_log_lines: Vec<String>,
    /// The name of the secret store in use.
    pub secret_backend: String,
    /// Whether the secret store encrypts secrets at rest.
    pub secret_encrypted: bool,
    /// Why the platform store was not used, when the fallback was chosen.
    pub secret_fallback_reason: Option<String>,
    /// The directory the app keeps its state in.
    pub data_dir: PathBuf,
    /// A banner for the user, if there is one.
    pub notice: Option<(NoticeKind, String)>,
    /// Why connecting is not allowed, or `None` when it is. The service
    /// refuses connect and restart while this is set.
    pub connect_blocked: Option<String>,
    /// Whether the user has turned the system proxy on.
    pub system_proxy_on: bool,
    /// Whether Starpod has actually pointed the operating system's proxy at
    /// the engine. It is `false` while the engine is stopped, even when
    /// `system_proxy_on` is set.
    pub system_proxy_active: bool,
    /// Why the last system proxy change failed, if it did and no later change
    /// has succeeded.
    pub system_proxy_error: Option<String>,
}

/// The state of the proxy engine.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EngineView {
    /// The lifecycle phase.
    pub phase: EnginePhase,
    /// Whether the engine binary is installed.
    pub installed: bool,
    /// The engine's version, once it has answered.
    pub version: Option<String>,
    /// How long the engine has been running, in seconds.
    pub uptime_secs: Option<u64>,
    /// Whether the engine is running with administrator rights.
    pub elevated: bool,
    /// The last error, redacted.
    pub error: Option<String>,
}

/// Where a subscription's data stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionStatus {
    /// It has never been fetched successfully.
    NeverFetched,
    /// The last refresh worked.
    Current,
    /// The last refresh failed. The previous working data is still in use.
    Failing,
    /// The engine fetches and refreshes it. The app cannot see its data.
    EngineManaged,
}

/// One subscription, as the UI shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct SubscriptionView {
    /// Whether this is the subscription in use. Exactly one is, when there
    /// are any; the others take no part in the configuration.
    pub active: bool,
    /// The subscription's id.
    pub id: String,
    /// The label the user gave it.
    pub label: String,
    /// Who fetches it.
    pub mode: SubscriptionMode,
    /// Where its data stands.
    pub status: SubscriptionStatus,
    /// When it was last fetched successfully, as RFC 3339.
    pub last_updated: Option<String>,
    /// The last error, already redacted.
    pub error: Option<String>,
    /// How many nodes the last valid payload carried.
    pub node_count: usize,
    /// How many groups the last valid payload carried.
    pub group_count: usize,
    /// The reported quota. `None` means the provider reported none, or that
    /// the engine manages the subscription and the app cannot see it. It is
    /// never a zero standing in for "unknown".
    pub quota: Option<SubscriptionQuota>,
    /// Whether an `Authorization` header is stored for it.
    pub has_auth_header: bool,
    /// Whether it was added from pasted text rather than a link. A local
    /// profile is never fetched; its text is edited instead.
    pub local: bool,
    /// The profile title the provider sent, or the label when it sent none.
    pub title: String,
    /// The `announce` header, if the provider sent one.
    pub announce: Option<String>,
    /// The `support-url` header, if it was sent and is an http(s) URL.
    pub support_url: Option<String>,
    /// The `profile-web-page-url` header, if it was sent and is an http(s) URL.
    pub web_url: Option<String>,
    /// The `profile-web-page-name` header: the label for the web page link.
    pub web_page_name: Option<String>,
    /// The `announce-url` header: a link shown with the announcement.
    pub announce_url: Option<String>,
    /// The cached profile logo, when the provider sent one and it was kept.
    pub logo_path: Option<PathBuf>,
    /// How often the subscription refreshes, in seconds: the provider's
    /// `profile-update-interval`, or the default.
    pub interval_secs: u64,
    /// Whether the provider disallows Global mode while this subscription is
    /// active.
    pub global_mode_disabled: bool,
    /// The provider's `expand-proxy-groups` preference. `Some(false)` means the
    /// groups start collapsed.
    pub expand_proxy_groups: Option<bool>,
    /// Whether the provider reported this device at its device limit.
    pub device_limit: bool,
}

/// An announcement from a subscription, as the Home page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnouncementView {
    /// The id of the subscription that sent it.
    pub subscription_id: String,
    /// The subscription's title (profile title, or its label).
    pub title: String,
    /// The announcement text, capped at 2000 characters. For a device-limit
    /// notice, the fixed device-limit message.
    pub text: String,
    /// The link shown with the announcement, if the provider sent one.
    pub announce_url: Option<String>,
    /// The support link, if the provider sent one.
    pub support_url: Option<String>,
    /// The provider's web page, if it sent one.
    pub web_url: Option<String>,
}

/// The message shown when the provider has no device slot left for this device.
pub const DEVICE_LIMIT_MESSAGE: &str =
    "This subscription has reached its device limit. Contact support to free a slot.";

/// The profile card on the Home page: the active subscription's name, logo,
/// announcement, links and traffic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileCard {
    /// The subscription the card describes.
    pub subscription_id: String,
    /// The profile title, or the label.
    pub title: String,
    /// The cached logo, when there is one.
    pub logo_path: Option<PathBuf>,
    /// The announcement text, if the provider sent one.
    pub announce: Option<String>,
    /// The link shown with the announcement.
    pub announce_url: Option<String>,
    /// The support link.
    pub support_url: Option<String>,
    /// The provider's web page.
    pub web_url: Option<String>,
    /// The label for the web page link. `None` means show the URL instead.
    pub web_page_name: Option<String>,
    /// Whether the provider reported a quota at all.
    pub has_quota: bool,
    /// Bytes used on the plan, or `0` when the provider reported no quota.
    pub traffic_used_bytes: u64,
    /// The plan's total bytes, or `0` when the provider reported no total.
    pub traffic_total_bytes: u64,
    /// When the plan ends, as the provider reported it (RFC 3339), if it did.
    pub expires: Option<String>,
    /// Whether the plan has already ended.
    pub expired: bool,
    /// Whole days until the plan ends, rounded toward zero. `-1` means there is
    /// no expiry. Zero or more is the days left; `-2` or less means the plan
    /// has ended, and the more negative, the longer ago.
    pub days_remaining: i64,
    /// Whether the plan ends within three days, or has already ended.
    pub expiring_soon: bool,
    /// Whether the provider reported this device at its device limit.
    pub device_limit: bool,
}

/// Build the profile card for one subscription.
///
/// `now` is a unix timestamp in seconds. The function does no I/O, so the
/// whole card is testable with a fixed clock.
pub(crate) fn profile_card(
    subscription: &Subscription,
    logo_path: Option<PathBuf>,
    now: i64,
) -> ProfileCard {
    let quota = subscription.quota.as_ref();
    let used = quota.map_or(0, |quota| quota.used);
    let total = quota.and_then(|quota| quota.total).unwrap_or(0);
    // Providers send `expire=0` for a plan with no end date (the Clash
    // convention, like `total=0` for no traffic limit), so a timestamp at or
    // before the epoch is no expiry, not a plan that ended in 1970.
    let expires_unix = quota
        .and_then(|quota| quota.expires.as_deref())
        .and_then(starpod_core::timestamp::parse)
        .filter(|&expires| expires > 0);
    let expires = expires_unix.and_then(|_| quota.and_then(|quota| quota.expires.clone()));
    let days_remaining = expires_unix.map_or(-1, |expires| days_until(expires, now));
    ProfileCard {
        subscription_id: subscription.id.clone(),
        title: display_title(subscription),
        logo_path,
        announce: subscription.announce.clone(),
        announce_url: subscription.announce_url.clone(),
        support_url: subscription.support_url.clone(),
        web_url: subscription.web_url.clone(),
        web_page_name: subscription.web_page_name.clone(),
        has_quota: quota.is_some(),
        traffic_used_bytes: used,
        traffic_total_bytes: total,
        expired: expires_unix.is_some_and(|expires| expires < now),
        expires,
        days_remaining,
        expiring_soon: expires_unix.is_some_and(|expires| expires < now + 3 * SECONDS_PER_DAY),
        device_limit: subscription.device_limit,
    }
}

const SECONDS_PER_DAY: i64 = 86_400;

/// Whole days from `now` until `expires`, rounded toward zero, with the
/// negative range reserved so that `-1` can mean "no expiry".
///
/// A plan that ended less than a day ago reads as `-2`; one that ended a full
/// day or more ago reads as `-2` minus the full days since. So every expired
/// plan is `-2` or less.
fn days_until(expires: i64, now: i64) -> i64 {
    let remaining = expires - now;
    if remaining >= 0 {
        remaining / SECONDS_PER_DAY
    } else {
        -2 - (-remaining) / SECONDS_PER_DAY
    }
}

/// A proxy group, as the Home and Proxies pages show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupView {
    /// The group's engine name. It is the name a selection is sent by.
    pub name: String,
    /// The name to show: `name` without its subscription's `<label> / `
    /// prefix. Equal to `name` for built-ins and the app's own groups.
    pub label: String,
    /// The group type as the configuration spells it: `select`, `url-test`,
    /// `fallback`, `load-balance` or `relay`.
    pub kind: String,
    /// The member the group uses now. For a `select` group, the member the
    /// user chose or the engine reports; `None` only for an empty group.
    pub selected: Option<String>,
    /// The members, in configuration order.
    pub members: Vec<MemberView>,
    /// Whether the configuration marks the group `hidden`. A hidden group still routes
    /// traffic, so the Home page keeps it; the Proxies page and the tray leave it out.
    pub hidden: bool,
}

/// One member of a proxy group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberView {
    /// The member's engine name: a node, a group or a built-in outbound.
    pub name: String,
    /// The name to show, as for [`GroupView::label`].
    pub label: String,
    /// `group` for a group, `direct` or `reject` for the built-ins, or the
    /// node's protocol.
    pub protocol: String,
    /// The member's latency from the last test.
    pub latency: Latency,
}

/// The result of a latency test for one member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Latency {
    /// Not measured since the proxy last started.
    Untested,
    /// The last probe timed out or failed.
    TimedOut,
    /// The last probe answered in this many milliseconds.
    Measured(u32),
}

/// One node, as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeView {
    /// The node's id, stable across refreshes.
    pub id: String,
    /// The node's display name.
    pub name: String,
    /// The protocol.
    pub protocol: String,
    /// The first group that holds the node, if any.
    pub group: Option<String>,
    /// Whether the user selected this node.
    pub selected: bool,
    /// The latency from the last test, in milliseconds.
    ///
    /// `None` means the node has not been tested, or its last test failed or
    /// timed out. The two are not told apart here.
    pub latency_ms: Option<u32>,
}

/// One rule, as the UI shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleView {
    /// The rule's position in its own list.
    pub index: usize,
    /// The rule's id. Subscription rules have none.
    pub id: Option<String>,
    /// Whether the rule takes part in routing.
    pub enabled: bool,
    /// The rule kind, as the engine spells it (`DOMAIN-SUFFIX`, `RULE-SET`, …).
    pub kind: String,
    /// The payload: the domain, the CIDR, the provider name, and so on.
    pub payload: String,
    /// The target. Empty for `SUB-RULE`.
    pub target: String,
    /// The engine line the rule translates to.
    pub line: String,
    /// Whether a subscription supplied the rule.
    pub from_subscription: bool,
}

/// A rule kind the editor offers, with a hint for its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleKindView {
    /// The kind's token.
    pub token: String,
    /// An example payload for it.
    pub payload_hint: String,
}

/// Build the view of one rule.
pub(crate) fn rule_view(
    index: usize,
    id: Option<&str>,
    enabled: bool,
    spec: &RuleSpec,
    from_subscription: bool,
) -> RuleView {
    let line = translate(spec);
    let (kind, payload) = match spec {
        RuleSpec::Simple { kind, payload, .. } => (kind.as_str().to_string(), payload.clone()),
        RuleSpec::Match { .. } => ("MATCH".to_string(), String::new()),
        RuleSpec::Logic { kind, .. } => {
            let prefix = format!("{},", kind.as_str());
            let payload = line
                .strip_prefix(prefix.as_str())
                .and_then(|rest| rest.rsplit_once(','))
                .map(|(payload, _)| payload.to_string())
                .unwrap_or_default();
            (kind.as_str().to_string(), payload)
        }
        RuleSpec::RuleSet { provider, .. } => ("RULE-SET".to_string(), provider.clone()),
        RuleSpec::SubRule { block } => ("SUB-RULE".to_string(), block.clone()),
    };
    RuleView {
        index,
        id: id.map(str::to_string),
        enabled,
        kind,
        payload,
        target: spec
            .target()
            .map(|target| target.as_str().to_string())
            .unwrap_or_default(),
        line,
        from_subscription,
    }
}

/// Every rule kind the editor offers, in the order the grammar lists them.
///
/// `SUB-RULE` is left out: the app defines no rule blocks, so such a rule can
/// never validate. See `compose::unsupported_rule`. `RULE-SET` is offered; it
/// validates only when a profile declares a provider of that name.
pub(crate) fn rule_kind_views() -> Vec<RuleKindView> {
    RuleKind::ALL
        .iter()
        .filter(|kind| !matches!(kind, RuleKind::SubRule))
        .map(|kind| RuleKindView {
            token: kind.as_str().to_string(),
            payload_hint: payload_hint(*kind).to_string(),
        })
        .collect()
}

/// The name a subscription is shown under: the provider's profile title when
/// it sent one, otherwise the label the user gave it.
pub(crate) fn display_title(subscription: &Subscription) -> String {
    subscription
        .profile_title
        .clone()
        .unwrap_or_else(|| subscription.label.clone())
}

/// The name a group or node is shown under.
///
/// A subscription's groups and nodes are named `<label> / <name>`. When
/// `origin` is the label of the subscription the name came from, that prefix
/// is removed. Built-ins and the app's own groups have no origin and keep
/// their name, and so does any name that does not start with the prefix.
pub(crate) fn display_label(name: &str, origin: Option<&str>) -> String {
    origin
        .filter(|label| !label.is_empty())
        .map(|label| format!("{label}{NAMESPACE_SEPARATOR}"))
        .and_then(|prefix| name.strip_prefix(prefix.as_str()))
        .filter(|rest| !rest.is_empty())
        .map_or_else(|| name.to_string(), str::to_string)
}

/// What a group member is: a node's protocol, `group` for a group, `direct`
/// or `reject` for the built-in outbounds, `builtin` for the rest. Empty
/// when the name is none of these.
pub(crate) fn member_protocol(config: &RuntimeConfig, member: &str) -> String {
    if let Some(node) = config.proxies.iter().find(|node| node.name == member) {
        return node.protocol.clone();
    }
    if config.proxy_groups.iter().any(|group| group.name == member) {
        return "group".to_string();
    }
    let protocol = match BuiltinTarget::from_str_exact(member) {
        Some(BuiltinTarget::Direct) => "direct",
        Some(BuiltinTarget::Reject | BuiltinTarget::RejectDrop) => "reject",
        Some(_) => "builtin",
        None => "",
    };
    protocol.to_string()
}

/// The built-in targets, as strings.
pub(crate) fn builtin_target_names() -> Vec<String> {
    BuiltinTarget::ALL
        .iter()
        .map(|target| target.as_str().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscription(mode: SubscriptionMode) -> Subscription {
        Subscription {
            id: "sub-1".to_string(),
            label: "Airport".to_string(),
            mode,
            interval_secs: 3600,
            has_auth_header: false,
            last_updated: None,
            quota: None,
            node_count: 0,
            group_count: 0,
            last_error: None,
            profile_title: None,
            announce: None,
            support_url: None,
            web_url: None,
            web_page_name: None,
            announce_url: None,
            logo_file: None,
            global_mode_disabled: false,
            expand_proxy_groups: None,
            device_limit: false,
            local: false,
            auto_named: false,
        }
    }

    fn group(name: &str, group_type: &str, members: &[&str]) -> starpod_core::ProxyGroup {
        starpod_core::ProxyGroup {
            id: name.to_string(),
            name: name.to_string(),
            group_type: group_type.to_string(),
            members: members.iter().map(|m| m.to_string()).collect(),
            strategy: None,
            url: None,
            interval: None,
            hidden: false,
            raw: Default::default(),
        }
    }

    fn config_with(groups: Vec<starpod_core::ProxyGroup>, rules: &[&str]) -> RuntimeConfig {
        RuntimeConfig {
            mixed_port: None,
            socks_port: None,
            port: None,
            allow_lan: false,
            bind_address: None,
            mode: ProxyMode::Rule,
            log_level: starpod_core::LogLevel::Info,
            tun: Default::default(),
            proxies: Vec::new(),
            proxy_groups: groups,
            rules: rules.iter().map(|r| r.to_string()).collect(),
            rule_providers: Vec::new(),
            subscriptions: Vec::new(),
            engine: Default::default(),
        }
    }

    #[test]
    fn a_subscription_prefix_is_removed_only_for_its_own_label() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(
            display_label("Airport / Auto", Some("Airport")),
            "Auto",
            "the subscription the name came from"
        );
        assert_eq!(
            display_label("Airport / Auto", Some("Other")),
            "Airport / Auto",
            "a different subscription's label is not stripped"
        );
        assert_eq!(
            display_label("Airport / Auto", None),
            "Airport / Auto",
            "a name with no origin keeps its name"
        );
        assert_eq!(display_label("PROXY", None), "PROXY");
        assert_eq!(display_label("DIRECT", None), "DIRECT");
        assert_eq!(
            display_label("Airport / ", Some("Airport")),
            "Airport / ",
            "an empty remainder is not a label"
        );
        assert_eq!(
            display_label("Airport / A / B", Some("Airport")),
            "A / B",
            "only the leading prefix goes"
        );
    }

    #[test]
    fn a_member_is_described_by_what_it_is() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let mut config = config_with(vec![group("Proxy", "select", &["Node A", "Auto"])], &[]);
        config.proxies.push(starpod_core::Node {
            id: "n1".to_string(),
            name: "Node A".to_string(),
            protocol: "trojan".to_string(),
            server: None,
            port: None,
            tags: Vec::new(),
            raw: Default::default(),
        });
        config.proxy_groups.push(group("Auto", "url-test", &[]));
        assert_eq!(member_protocol(&config, "Node A"), "trojan");
        assert_eq!(member_protocol(&config, "Auto"), "group");
        assert_eq!(member_protocol(&config, "DIRECT"), "direct");
        assert_eq!(member_protocol(&config, "REJECT"), "reject");
        assert_eq!(member_protocol(&config, "Nowhere"), "");
    }

    #[test]
    fn a_subscription_is_titled_by_its_profile_title_or_its_label() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let mut subscription = subscription(SubscriptionMode::AppManaged);
        assert_eq!(display_title(&subscription), "Airport");
        subscription.profile_title = Some("Foxgirls VPN".to_string());
        assert_eq!(display_title(&subscription), "Foxgirls VPN");
    }

    #[test]
    fn connecting_needs_an_installed_engine_first() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let app = subscription(SubscriptionMode::AppManaged);
        assert_eq!(
            connect_blocked_reason(false, &[(&app, Some(3))]).as_deref(),
            Some(ENGINE_MISSING_REASON)
        );
    }

    #[test]
    fn no_subscription_says_to_add_one() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(
            connect_blocked_reason(true, &[]).as_deref(),
            Some(NO_SUBSCRIPTION_REASON)
        );
    }

    #[test]
    fn an_app_managed_subscription_without_a_payload_or_nodes_says_to_refresh() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let app = subscription(SubscriptionMode::AppManaged);
        assert_eq!(
            connect_blocked_reason(true, &[(&app, None)]).as_deref(),
            Some(NO_SERVERS_REASON),
            "a subscription that never fetched successfully"
        );
        assert_eq!(
            connect_blocked_reason(true, &[(&app, Some(0))]).as_deref(),
            Some(NO_SERVERS_REASON),
            "a payload with no nodes"
        );
    }

    #[test]
    fn an_app_managed_subscription_with_nodes_allows_connecting() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let app = subscription(SubscriptionMode::AppManaged);
        assert_eq!(connect_blocked_reason(true, &[(&app, Some(2))]), None);
    }

    #[test]
    fn an_engine_managed_subscription_allows_connecting_without_a_payload() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let engine = subscription(SubscriptionMode::EngineManaged);
        assert_eq!(connect_blocked_reason(true, &[(&engine, None)]), None);
    }

    #[test]
    fn one_usable_subscription_is_enough_beside_unusable_ones() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let empty = subscription(SubscriptionMode::AppManaged);
        let engine = subscription(SubscriptionMode::EngineManaged);
        assert_eq!(
            connect_blocked_reason(true, &[(&empty, None), (&engine, None)]),
            None
        );
    }

    #[test]
    fn the_editor_offers_no_kind_the_app_cannot_supply() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let tokens: Vec<String> = rule_kind_views()
            .into_iter()
            .map(|kind| kind.token)
            .collect();
        assert!(tokens.iter().any(|token| token == "RULE-SET"), "{tokens:?}");
        assert!(
            !tokens.iter().any(|token| token == "SUB-RULE"),
            "{tokens:?}"
        );
        assert!(
            tokens.iter().any(|token| token == "DOMAIN-SUFFIX"),
            "{tokens:?}"
        );
        assert!(tokens.iter().any(|token| token == "MATCH"), "{tokens:?}");
    }

    const NOW: i64 = 1_800_000_000;
    const DAY: i64 = 86_400;

    fn with_quota(total: Option<u64>, expires: Option<i64>) -> Subscription {
        let mut subscription = subscription(SubscriptionMode::AppManaged);
        subscription.quota = Some(SubscriptionQuota {
            used: 30,
            total,
            expires: expires.and_then(starpod_core::timestamp::format),
        });
        subscription
    }

    #[test]
    fn a_card_with_no_quota_or_expiry_reports_neither() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let card = profile_card(&subscription(SubscriptionMode::AppManaged), None, NOW);
        assert!(!card.has_quota);
        assert_eq!(card.expires, None);
        assert_eq!(card.days_remaining, -1, "-1 is the no-expiry marker");
        assert!(!card.expiring_soon);
    }

    #[test]
    fn a_zero_expiry_means_no_end_date() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let card = profile_card(&with_quota(Some(0), Some(0)), None, NOW);
        assert!(card.has_quota);
        assert!(!card.expired, "expire=0 is not a plan that ended in 1970");
        assert!(!card.expiring_soon);
        assert_eq!(card.expires, None);
        assert_eq!(card.days_remaining, -1);
    }

    #[test]
    fn the_quota_carries_its_used_and_total_bytes() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let card = profile_card(&with_quota(Some(100), None), None, NOW);
        assert!(card.has_quota);
        assert_eq!(card.traffic_used_bytes, 30);
        assert_eq!(card.traffic_total_bytes, 100);
    }

    #[test]
    fn a_quota_without_a_total_has_a_zero_total_for_unlimited() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let card = profile_card(&with_quota(None, None), None, NOW);
        assert_eq!(card.traffic_total_bytes, 0);
    }

    #[test]
    fn a_plan_ending_in_five_days_is_not_expiring_soon() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let card = profile_card(&with_quota(None, Some(NOW + 5 * DAY)), None, NOW);
        assert_eq!(card.days_remaining, 5);
        assert!(!card.expiring_soon);
    }

    #[test]
    fn a_plan_ending_within_three_days_is_expiring_soon() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let card = profile_card(&with_quota(None, Some(NOW + 2 * DAY)), None, NOW);
        assert_eq!(card.days_remaining, 2);
        assert!(card.expiring_soon);
        assert!(!card.expired);
    }

    #[test]
    fn an_ended_plan_is_expired_and_its_days_are_negative_but_never_minus_one() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let recent = profile_card(&with_quota(None, Some(NOW - 3600)), None, NOW);
        assert!(recent.expired);
        assert!(recent.expiring_soon, "an ended plan counts as expiring");
        assert_eq!(recent.days_remaining, -2, "less than a day ago");
        assert_ne!(recent.days_remaining, -1);

        let old = profile_card(&with_quota(None, Some(NOW - 3 * DAY)), None, NOW);
        assert_eq!(old.days_remaining, -5, "three full days ago: -2 minus 3");
    }

    #[test]
    fn the_device_limit_and_the_logo_path_travel_with_the_card() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let mut subscription = subscription(SubscriptionMode::AppManaged);
        subscription.device_limit = true;
        let logo = PathBuf::from("/cache/subscription-logos/sub-1.png");
        let card = profile_card(&subscription, Some(logo.clone()), NOW);
        assert!(card.device_limit);
        assert_eq!(card.logo_path, Some(logo));
    }
}
