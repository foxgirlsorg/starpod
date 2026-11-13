//! The view state the UI renders.
//!
//! An [`AppSnapshot`] is plain data: it can be cloned, compared and sent to
//! the UI thread. It deliberately holds no URL, no `Authorization` header, no
//! node credential and no raw engine-format node, so nothing in it can leak a
//! secret even when it is printed with `{:?}`.

use std::path::PathBuf;

use starpod_core::rules::{BuiltinTarget, RuleKind, translate};
use starpod_core::{
    ProxyMode, RuleSpec, SubscriptionMode, SubscriptionQuota, UserPreferences, domain::payload_hint,
};
use starpod_engine::EnginePhase;

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

/// Everything the UI renders, as one value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppSnapshot {
    /// The proxy engine.
    pub engine: EngineView,
    /// The effective routing mode.
    pub mode: ProxyMode,
    /// The address the HTTP proxy is expected to listen on, if it has a port.
    pub listen_address: Option<String>,
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
    /// is listed here but replaced by the app's own tail.
    pub subscription_rules: Vec<RuleView>,
    /// The rule kinds the rule editor offers.
    pub rule_kinds: Vec<RuleKindView>,
    /// The targets a rule may send traffic to.
    pub rule_targets: Vec<String>,
    /// The user's own settings.
    pub preferences: UserPreferences,
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
pub(crate) fn rule_kind_views() -> Vec<RuleKindView> {
    RuleKind::ALL
        .iter()
        .map(|kind| RuleKindView {
            token: kind.as_str().to_string(),
            payload_hint: payload_hint(*kind).to_string(),
        })
        .collect()
}

/// The built-in targets, as strings.
pub(crate) fn builtin_target_names() -> Vec<String> {
    BuiltinTarget::ALL
        .iter()
        .map(|target| target.as_str().to_string())
        .collect()
}
