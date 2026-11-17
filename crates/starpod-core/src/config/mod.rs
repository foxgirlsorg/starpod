//! The runtime configuration pipeline.
//!
//! `Subscription + User Preferences → Normalization → Merging → Validation → Runtime Configuration`
//!
//! This module implements steps ①–③ of that pipeline as **pure functions**.
//! There is no I/O, no clock, no randomness, and no knowledge of the GUI or
//! the proxy engine. That is what makes the pipeline independently testable,
//! as GOAL.md §3 requires.
//!
//! The precedence contract — the heart of the pipeline — is:
//!
//! | # | Field | Precedence |
//! |---|-------|------------|
//! | P1 | ports | user > subscription > default |
//! | P1a | `allow_lan`, `bind_address` | user > default (off / loopback); a subscription is never consulted |
//! | P2 | `mode`, `log_level`, `tun`, `dns` | user > subscription > default |
//! | P3 | nodes | union; the user wins on a name collision |
//! | P4 | proxy groups | user > subscription, per group name |
//! | P5 | rules | user rules, then subscription rules, then the `MATCH` tail; the tail's target is the user's selected group, else the first `MATCH`'s target, else `PROXY` |
//! | P6 | the selected node | user > subscription default, by node id |
//!
//! "Never silently overwrite user settings" is enforced structurally: the
//! user's preferences and the subscription-derived settings are separate
//! values, and the merge reads them separately. No code path lets one write
//! the other.

use std::collections::{BTreeSet, HashMap};

use crate::domain::{
    BuildInput, BuildOutcome, EngineManagedSubscription, Finding, Node, ProxyGroup, ProxyMode,
    RuleProvider, RuntimeConfig, Severity, StoredRule, SubscriptionPayload, SubscriptionSettings,
    TunSettings, UserPreferences,
};
use crate::rules::{BuiltinTarget, translate_all};

mod engine_options;

#[cfg(test)]
mod tests;

/// The default configuration, used when neither the user nor any
/// subscription supplies a value.
pub const DEFAULT_MIXED_PORT: u16 = 7890;
/// The default SOCKS port.
pub const DEFAULT_SOCKS_PORT: u16 = 7891;

/// The name of the group the app creates to hold the user's selection.
///
/// It is always created, even when the user has not defined any group, so
/// that there is always something for the engine to select a node from.
pub const DEFAULT_GROUP_NAME: &str = "PROXY";

/// Build a candidate configuration from the given inputs.
///
/// This never touches the running configuration and never mutates its
/// inputs. The caller validates the result (the engine's `meow -t`) and
/// only then applies it.
pub fn build(input: &BuildInput) -> BuildOutcome {
    let mut findings = Vec::new();

    let normalized = normalize(input, &mut findings);
    engine_options::validate_preferences(&input.preferences, &mut findings);
    let (merged, primary_group) = merge(&normalized, &mut findings);
    let validated = validate(&merged, &mut findings);

    let has_errors = findings.iter().any(|f| f.severity == Severity::Error);
    if has_errors {
        return BuildOutcome {
            config: None,
            findings,
            primary_group: None,
        };
    }

    BuildOutcome {
        config: Some(validated),
        findings,
        primary_group,
    }
}

/// Step ① — normalization.
///
/// Node and group names are made unique, deterministically: the first
/// claimant of a name keeps it, and a later claimant gets ` (2)`, ` (3)`,
/// … The order of processing is fixed (user nodes first, then subscriptions
/// in list order), so the same input always produces the same names. That
/// matters: it is what lets a user's group selection survive a subscription
/// refresh.
struct Normalized {
    preferences: UserPreferences,
    user_rules: Vec<StoredRule>,
    nodes: Vec<Node>,
    groups: Vec<ProxyGroup>,
    subscriptions: Vec<SubscriptionPayload>,
    engine_managed: Vec<EngineManagedSubscription>,
    rule_providers: Vec<RuleProvider>,
    /// The renames normalization applied, from the original name to the new
    /// one. Group members follow these, so a member still points at the node
    /// it named before normalization.
    renames: HashMap<String, String>,
}

fn normalize(input: &BuildInput, findings: &mut Vec<Finding>) -> Normalized {
    let mut used_names: BTreeSet<String> = BTreeSet::new();
    let mut renames: HashMap<String, String> = HashMap::new();
    let mut nodes = Vec::new();
    let mut groups = Vec::new();

    // The user's own nodes come first, so they keep their names and a
    // subscription node of the same name is the one that gets renamed.
    for node in &input.user_nodes {
        let original = node.name.clone();
        let node = unique_name(node.clone(), &mut used_names, "node", findings);
        if node.name != original {
            renames.insert(original, node.name.clone());
        }
        nodes.push(node);
    }
    for group in &input.user_groups {
        let original = group.name.clone();
        let group = unique_name(group.clone(), &mut used_names, "group", findings);
        if group.name != original {
            renames.insert(original, group.name.clone());
        }
        groups.push(group);
    }

    // Subscription nodes follow, so a subscription node of the same name as
    // a user's node is the one that gets renamed — which is exactly P3's
    // "the user wins on a name collision".
    //
    // Subscription *groups* follow the same rule (P4: the user's group wins
    // per group name). A subscription group whose name collides with a
    // user's group is dropped by the merge rather than renamed — but it
    // must not consume the name here either, or the user's group would be
    // renamed away from it.
    let mut subscriptions = Vec::new();
    for payload in &input.subscriptions {
        let mut normalized_payload = payload.clone();
        normalized_payload.nodes = payload
            .nodes
            .iter()
            .map(|node| {
                let original = node.name.clone();
                let node = unique_name(node.clone(), &mut used_names, "node", findings);
                if node.name != original {
                    renames.insert(original, node.name.clone());
                }
                node
            })
            .collect();

        // A group that collides with a name already taken is not renamed
        // here: the merge drops it (the user's group of the same name
        // wins), and renaming it would make the drop invisible.
        let mut kept_groups = Vec::new();
        let mut dropped_group_names = Vec::new();
        for group in &payload.groups {
            if used_names.contains(&group.name) {
                dropped_group_names.push(group.name.clone());
            } else {
                used_names.insert(group.name.clone());
                kept_groups.push(group.clone());
            }
        }
        normalized_payload.groups = kept_groups;

        for name in dropped_group_names {
            findings.push(Finding {
                severity: Severity::Info,
                message: format!(
                    "the subscription’s group “{name}” was dropped because a group of the same \
                     name already exists"
                ),
                subject: Some(name),
            });
        }

        subscriptions.push(normalized_payload);
    }

    // A rule's order must be unique. Ties are broken by rule id, so the
    // result is deterministic no matter what order the rules were stored in.
    let mut user_rules = input.user_rules.clone();
    user_rules.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));

    Normalized {
        preferences: input.preferences.clone(),
        user_rules,
        nodes,
        groups,
        subscriptions,
        engine_managed: input.engine_managed.clone(),
        rule_providers: input.rule_providers.clone(),
        renames,
    }
}

/// Give `item` a name that is unique within `used`, suffixing ` (2)`, ` (3)`…
fn unique_name<T: Named>(
    mut item: T,
    used: &mut BTreeSet<String>,
    what: &str,
    findings: &mut Vec<Finding>,
) -> T {
    let original = item.name().to_string();
    if used.insert(original.clone()) {
        item.set_name(original);
        return item;
    }
    let mut counter = 2;
    loop {
        let candidate = format!("{original} ({counter})");
        if used.insert(candidate.clone()) {
            findings.push(Finding {
                severity: Severity::Warning,
                message: format!(
                    "another {what} is already named “{original}”; this one was renamed to \
                     “{candidate}”"
                ),
                subject: Some(candidate.clone()),
            });
            item.set_name(candidate);
            return item;
        }
        counter += 1;
    }
}

/// Anything with a name that must be unique in the configuration.
trait Named {
    fn name(&self) -> &str;
    fn set_name(&mut self, name: String);
}

impl Named for Node {
    fn name(&self) -> &str {
        &self.name
    }
    fn set_name(&mut self, name: String) {
        self.name = name;
    }
}

impl Named for ProxyGroup {
    fn name(&self) -> &str {
        &self.name
    }
    fn set_name(&mut self, name: String) {
        self.name = name;
    }
}

/// Step ② — merging.
///
/// Returns the merged configuration and its primary group (see
/// [`BuildOutcome::primary_group`]).
fn merge(normalized: &Normalized, findings: &mut Vec<Finding>) -> (RuntimeConfig, Option<String>) {
    let prefs = &normalized.preferences;

    // P1 and P2: the user's own settings win; a subscription's settings are
    // the fallback; the app's defaults are the last resort.
    //
    // The distinction that matters here is between "the user chose this" and
    // "the user never said". A user's *explicit* choice always wins. A
    // preference the user has never expressed falls through to the
    // subscription, and then to the app's default.
    let mut mixed_port = prefs.mixed_port;
    let mut socks_port = prefs.socks_port;
    let mut port = prefs.port;
    let allow_lan = prefs.allow_lan;
    let bind_address = prefs.bind_address.clone();
    let mut mode = prefs.mode;
    let mut log_level = prefs.log_level;

    for payload in &normalized.subscriptions {
        let settings: &SubscriptionSettings = &payload.settings;
        if mixed_port.is_none() {
            mixed_port = settings.mixed_port;
        }
        if socks_port.is_none() {
            socks_port = settings.socks_port;
        }
        if port.is_none() {
            port = settings.port;
        }
        // `allow_lan` and `bind_address` are deliberately absent: whether the
        // proxy is reachable from the network is the user's decision alone
        // (P1a). A remote subscription must never be able to expose it.
        if mode.is_none() {
            mode = settings.mode;
        }
        if log_level.is_none() {
            log_level = settings.log_level;
        }
    }

    if mixed_port.is_none() {
        mixed_port = Some(DEFAULT_MIXED_PORT);
    }
    if socks_port.is_none() {
        socks_port = Some(DEFAULT_SOCKS_PORT);
    }
    // The safe default is to *not* expose the proxy to the network.
    let allow_lan = allow_lan.unwrap_or(false);
    let mode = mode.unwrap_or_default();
    let log_level = log_level.unwrap_or_default();

    // P3: the nodes are the union of the user's and the subscriptions'. The
    // user's node wins on a name collision — but normalization has already
    // made every name unique, so a collision here is impossible by
    // construction. The map is kept anyway to make the precedence explicit
    // and to survive a future change to normalization. The nodes keep the
    // order they were written in.
    let mut proxies: OrderedMap<Node> = OrderedMap::new();
    for node in &normalized.nodes {
        proxies.insert_or_replace(node.name.clone(), node.clone());
    }
    for payload in &normalized.subscriptions {
        for node in &payload.nodes {
            proxies.insert_if_absent(node.name.clone(), node.clone());
        }
    }

    // P5: the user's rules come first, then the subscription's rules, then
    // the mandatory `MATCH` tail. The rules are built before the groups,
    // because a subscription's own `MATCH` decides where the groups go. The
    // tail itself is written after the groups, because its target must name
    // a group that exists (see `tail_target`).
    let mut rules: Vec<String> = Vec::new();
    for rule in &normalized.user_rules {
        if !rule.enabled {
            continue;
        }
        rules.extend(translate_all(std::slice::from_ref(&rule.spec)));
    }
    let mut subscription_has_match = false;
    for payload in &normalized.subscriptions {
        let translated = translate_all(&payload.rules);
        subscription_has_match |= translated.iter().any(|rule| rule.starts_with("MATCH,"));
        rules.extend(translated);
    }
    // Rules after the first `MATCH` are unreachable, so they are removed. The
    // `MATCH` tail itself is written once the groups are known.
    let first_match = rules.iter().position(|r| r.starts_with("MATCH,"));
    let first_match_target = first_match
        .and_then(|index| rules[index].split(',').nth(1))
        .map(|target| target.trim().to_string());
    if let Some(index) = first_match {
        if index + 1 < rules.len() {
            findings.push(Finding {
                severity: Severity::Warning,
                message: "rules after the MATCH rule can never match and were removed".to_string(),
                subject: None,
            });
        }
        rules.truncate(index);
    }

    // P4: the groups are kept in the order they are read in:
    //
    //   1. the app's own `PROXY` group, first — or, when a subscription
    //      supplies its own `MATCH`, after the subscriptions' groups;
    //   2. each subscription's groups, in the order its YAML lists them;
    //   3. the user's groups, in the user's order.
    //
    // A group of the same name is taken from the user. It replaces the
    // subscription's group in the position the subscription's group held.
    let default_missing = !normalized
        .groups
        .iter()
        .any(|group| group.name == DEFAULT_GROUP_NAME)
        && !normalized
            .subscriptions
            .iter()
            .any(|payload| payload.groups.iter().any(|g| g.name == DEFAULT_GROUP_NAME));
    let default_group = default_missing.then(|| ProxyGroup {
        id: "builtin-proxy".to_string(),
        name: DEFAULT_GROUP_NAME.to_string(),
        group_type: "select".to_string(),
        members: proxies.keys().map(str::to_string).collect(),
        strategy: None,
        url: None,
        interval: None,
        hidden: false,
        raw: Default::default(),
    });
    let (leading_default, trailing_default) = if subscription_has_match {
        (None, default_group)
    } else {
        (default_group, None)
    };

    let mut proxy_groups: OrderedMap<ProxyGroup> = OrderedMap::new();
    if let Some(group) = leading_default {
        proxy_groups.insert_if_absent(group.name.clone(), group);
    }
    for payload in &normalized.subscriptions {
        for group in &payload.groups {
            proxy_groups.insert_if_absent(group.name.clone(), group.clone());
        }
    }
    if let Some(group) = trailing_default {
        proxy_groups.insert_if_absent(group.name.clone(), group);
    }
    for group in &normalized.groups {
        proxy_groups.insert_or_replace(group.name.clone(), group.clone());
    }

    // A group whose members were renamed by normalization must follow the
    // rename, or it would point at a node that no longer exists.
    if !normalized.renames.is_empty() {
        for group in proxy_groups.values_mut() {
            for member in &mut group.members {
                if let Some(renamed) = normalized.renames.get(member.as_str()) {
                    *member = renamed.clone();
                }
            }
        }
    }

    // The `MATCH` tail, now that the groups exist.
    let tail = tail_target(
        &normalized.preferences,
        first_match_target.as_deref(),
        &proxy_groups,
    );
    rules.push(format!("MATCH,{tail}"));

    // The group the Home page offers first is the tail's target whenever that
    // is a group: the Home page's picker then changes where unmatched traffic
    // goes. Only when the tail is a builtin (DIRECT, REJECT, …) does it fall
    // back to the first select group a subscription supplies, then the first
    // select group, then the first group.
    let subscription_group_names: BTreeSet<&str> = normalized
        .subscriptions
        .iter()
        .flat_map(|payload| payload.groups.iter().map(|group| group.name.as_str()))
        .collect();
    let primary_group = if proxy_groups.contains_key(&tail) {
        Some(tail.clone())
    } else {
        proxy_groups
            .values()
            .find(|group| {
                group.group_type == "select"
                    && subscription_group_names.contains(group.name.as_str())
            })
            .map(|group| group.name.clone())
            .or_else(|| {
                proxy_groups
                    .values()
                    .find(|group| group.group_type == "select")
                    .map(|group| group.name.clone())
            })
            .or_else(|| proxy_groups.values().next().map(|group| group.name.clone()))
    };

    // P6: the selected node is resolved by id, so a rename in the
    // subscription does not lose the user's selection. The selection itself
    // is applied through the engine's own API, not through the config file;
    // what the config carries is the group the engine selects from.
    if resolve_selected_group(normalized, &proxy_groups).is_none()
        && let Some(preferred) = &normalized.preferences.selected_group
    {
        findings.push(Finding {
            severity: Severity::Warning,
            message: format!(
                "the selected group “{preferred}” does not exist in this configuration"
            ),
            subject: Some(preferred.clone()),
        });
    }

    let config = RuntimeConfig {
        mixed_port,
        socks_port,
        port,
        allow_lan,
        bind_address,
        mode,
        log_level,
        tun: prefs.tun.clone(),
        proxies: proxies.into_vec(),
        proxy_groups: proxy_groups.into_vec(),
        rules,
        rule_providers: normalized.rule_providers.clone(),
        subscriptions: normalized.engine_managed.clone(),
        engine: engine_options::resolve(&normalized.preferences, &normalized.subscriptions),
    };
    (config, primary_group)
}

/// A map that remembers the order its keys were first inserted in.
///
/// The configuration lists its nodes and groups in the order the user and the
/// subscriptions wrote them. A `BTreeMap` would sort them by name instead.
struct OrderedMap<V> {
    items: Vec<(String, V)>,
    positions: HashMap<String, usize>,
}

impl<V> OrderedMap<V> {
    fn new() -> Self {
        Self {
            items: Vec::new(),
            positions: HashMap::new(),
        }
    }

    /// Insert `value` under `key` unless the key is already present: the
    /// first insertion wins.
    fn insert_if_absent(&mut self, key: String, value: V) {
        if !self.positions.contains_key(&key) {
            self.push(key, value);
        }
    }

    /// Insert `value` under `key`, or replace the value of an existing key
    /// in the position that key already holds.
    fn insert_or_replace(&mut self, key: String, value: V) {
        match self.positions.get(&key) {
            Some(&index) => self.items[index].1 = value,
            None => self.push(key, value),
        }
    }

    fn push(&mut self, key: String, value: V) {
        self.positions.insert(key.clone(), self.items.len());
        self.items.push((key, value));
    }

    fn contains_key(&self, key: &str) -> bool {
        self.positions.contains_key(key)
    }

    fn keys(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(|(key, _)| key.as_str())
    }

    fn values(&self) -> impl Iterator<Item = &V> {
        self.items.iter().map(|(_, value)| value)
    }

    fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.items.iter_mut().map(|(_, value)| value)
    }

    fn into_vec(self) -> Vec<V> {
        self.items.into_iter().map(|(_, value)| value).collect()
    }
}

/// The rule target the `MATCH` tail sends unmatched traffic to.
///
/// In order of precedence:
///
/// 1. the user's `selected_group`, when it is set and is an existing group;
/// 2. the target of the first `MATCH` rule (the subscription's, or the
///    user's), when it names an existing group or a builtin outbound;
/// 3. the app's `PROXY` group.
///
/// A target that names a group which does not exist is never used.
fn tail_target(
    prefs: &UserPreferences,
    first_match_target: Option<&str>,
    groups: &OrderedMap<ProxyGroup>,
) -> String {
    if let Some(group) = prefs.selected_group.as_deref()
        && !group.is_empty()
        && groups.contains_key(group)
    {
        return group.to_string();
    }
    if let Some(target) = first_match_target
        && (groups.contains_key(target) || BuiltinTarget::from_str_exact(target).is_some())
    {
        return target.to_string();
    }
    DEFAULT_GROUP_NAME.to_string()
}

/// The group the engine selects from.
fn resolve_selected_group(
    normalized: &Normalized,
    groups: &OrderedMap<ProxyGroup>,
) -> Option<String> {
    let preferred = normalized.preferences.selected_group.as_deref()?;
    if groups.contains_key(preferred) {
        Some(preferred.to_string())
    } else {
        None
    }
}

/// Step ③ — validation.
///
/// These are the app-side checks. The engine-side check (`meow -t`) runs
/// separately and is authoritative; this layer catches the problems the
/// engine would report less clearly, and produces the warnings the user
/// needs to see before applying.
fn validate(config: &RuntimeConfig, findings: &mut Vec<Finding>) -> RuntimeConfig {
    // The ports must not collide.
    let mut ports: Vec<(&str, u16)> = Vec::new();
    for (name, port) in [
        ("mixed_port", config.mixed_port),
        ("socks_port", config.socks_port),
        ("port", config.port),
    ] {
        if let Some(port) = port {
            ports.push((name, port));
        }
    }
    for (i, (name_a, port_a)) in ports.iter().enumerate() {
        for (name_b, port_b) in ports.iter().skip(i + 1) {
            if port_a == port_b {
                findings.push(Finding {
                    severity: Severity::Error,
                    message: format!(
                        "{name_a} and {name_b} are both set to port {port_a}; they must differ"
                    ),
                    subject: None,
                });
            }
        }
    }

    // The node names must be unique.
    let mut seen_nodes = BTreeSet::new();
    for node in &config.proxies {
        if !seen_nodes.insert(node.name.as_str()) {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!("more than one node is named “{}”", node.name),
                subject: Some(node.name.clone()),
            });
        }
    }

    // The group names must be unique, and no group may contain itself.
    let mut seen_groups = BTreeSet::new();
    for group in &config.proxy_groups {
        if !seen_groups.insert(group.name.as_str()) {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!("more than one group is named “{}”", group.name),
                subject: Some(group.name.clone()),
            });
        }
        if group.members.iter().any(|m| m == &group.name) {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!("the group “{}” contains itself", group.name),
                subject: Some(group.name.clone()),
            });
        }
    }

    // Every group member must resolve to a node or a group.
    let known: BTreeSet<&str> = config
        .proxies
        .iter()
        .map(|n| n.name.as_str())
        .chain(config.proxy_groups.iter().map(|g| g.name.as_str()))
        .collect();
    for group in &config.proxy_groups {
        for member in &group.members {
            if !known.contains(member.as_str()) {
                findings.push(Finding {
                    severity: Severity::Error,
                    message: format!(
                        "the group “{}” refers to “{member}”, which is not a node or a group",
                        group.name
                    ),
                    subject: Some(group.name.clone()),
                });
            }
        }
        if group.members.is_empty() {
            findings.push(Finding {
                severity: Severity::Warning,
                message: format!("the group “{}” has no members", group.name),
                subject: Some(group.name.clone()),
            });
        }
    }

    // A group must not contain a cycle.
    for cycle in find_group_cycles(&config.proxy_groups) {
        findings.push(Finding {
            severity: Severity::Error,
            message: format!("the groups form a cycle: {}", cycle.join(" → ")),
            subject: None,
        });
    }

    // Every rule's target must resolve.
    let builtins = crate::rules::BuiltinTarget::ALL;
    for rule in &config.rules {
        let Some(target) = rule_target(rule) else {
            continue;
        };
        if builtins.iter().any(|b| b.as_str() == target.as_str()) {
            continue;
        }
        if known.contains(target.as_str()) {
            continue;
        }
        findings.push(Finding {
            severity: Severity::Error,
            message: format!("the rule “{rule}” sends traffic to “{target}”, which does not exist"),
            subject: None,
        });
    }

    // A provider must be one the app can declare: an `http` one or an `inline`
    // one. A `file` provider would point the engine at a local file, which no
    // subscription may do. Each name is declared once.
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for provider in &config.rule_providers {
        if !matches!(provider.provider_type.as_str(), "http" | "inline") {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!(
                    "the rule provider “{}” has the type “{}”, which the app does not declare",
                    provider.name, provider.provider_type
                ),
                subject: Some(provider.name.clone()),
            });
        }
        if !declared.insert(provider.name.as_str()) {
            findings.push(Finding {
                severity: Severity::Error,
                message: format!("two rule providers are named “{}”", provider.name),
                subject: Some(provider.name.clone()),
            });
        }
    }

    // Every `RULE-SET` must name a declared provider. Its target and any
    // trailing flags (`no-resolve`) come after the name and are not checked
    // here: the target is checked above like any other rule's.
    let providers: BTreeSet<&str> = config
        .rule_providers
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    for rule in &config.rules {
        if let Some(rest) = rule.strip_prefix("RULE-SET,") {
            let name = rest.split(',').next().unwrap_or("").trim();
            if !providers.contains(name) {
                findings.push(Finding {
                    severity: Severity::Error,
                    message: format!(
                        "the rule “{rule}” refers to the rule provider “{name}”, which is not \
                         declared"
                    ),
                    subject: None,
                });
            }
        }
    }

    // Geodata: a warning, not an error — the engine resolves the database.
    let needs_geodata = config.rules.iter().any(|rule| {
        ["GEOIP,", "SRC-GEOIP,", "IP-ASN,", "SRC-IP-ASN,"]
            .iter()
            .any(|prefix| rule.starts_with(prefix))
    });
    if needs_geodata {
        findings.push(Finding {
            severity: Severity::Warning,
            message: "this configuration uses GEOIP or IP-ASN rules; the engine will download a \
                      geo database the first time it is needed"
                .to_string(),
            subject: None,
        });
    }

    // The `MATCH` tail must be there, and must be last.
    match config.rules.iter().rposition(|r| r.starts_with("MATCH,")) {
        Some(index) if index + 1 == config.rules.len() => {}
        Some(_) => findings.push(Finding {
            severity: Severity::Error,
            message: "the MATCH rule must be the last rule in the list".to_string(),
            subject: None,
        }),
        None => findings.push(Finding {
            severity: Severity::Error,
            message: "the rule list has no MATCH rule".to_string(),
            subject: None,
        }),
    }

    config.clone()
}

/// The target of an engine rule line, when it has one.
///
/// Read with the rule parser rather than a comma split. A logic rule's
/// payload contains commas, so its target is the field after the balanced
/// group list, and only the parser locates that correctly. A second splitter
/// here would disagree with the parser on exactly those lines.
fn rule_target(rule: &str) -> Option<String> {
    let spec = crate::rules::parse_line(rule).ok()?;
    spec.target().map(|target| target.as_str().to_string())
}

/// Find the group-reference cycles in a group list.
fn find_group_cycles(groups: &[ProxyGroup]) -> Vec<Vec<String>> {
    let names: BTreeSet<&str> = groups.iter().map(|g| g.name.as_str()).collect();
    let mut cycles = Vec::new();
    for group in groups {
        let mut path = Vec::new();
        let mut seen = BTreeSet::new();
        if let Some(cycle) = walk_groups(group, &names, groups, &mut path, &mut seen) {
            cycles.push(cycle);
        }
    }
    cycles
}

fn walk_groups(
    group: &ProxyGroup,
    names: &BTreeSet<&str>,
    groups: &[ProxyGroup],
    path: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
) -> Option<Vec<String>> {
    if path.iter().any(|p| p == &group.name) {
        let mut cycle = path.clone();
        cycle.push(group.name.clone());
        return Some(cycle);
    }
    if !seen.insert(group.name.clone()) {
        return None;
    }
    path.push(group.name.clone());
    for member in &group.members {
        if !names.contains(member.as_str()) {
            continue;
        }
        let Some(next) = groups.iter().find(|g| g.name == *member) else {
            continue;
        };
        if let Some(cycle) = walk_groups(next, names, groups, path, seen) {
            return Some(cycle);
        }
    }
    path.pop();
    None
}

/// The port the app's HTTP proxy listens on, for display.
pub fn http_port(config: &RuntimeConfig) -> Option<u16> {
    config.mixed_port.or(config.port)
}

/// The port the app's SOCKS proxy listens on, for display.
pub fn socks_port(config: &RuntimeConfig) -> Option<u16> {
    config.socks_port
}

/// Whether the configuration exposes the proxy beyond this machine.
pub fn is_lan_exposed(config: &RuntimeConfig) -> bool {
    config.allow_lan
}

/// The TUN settings, with the enabled flag applied.
pub fn tun_settings(prefs: &UserPreferences) -> TunSettings {
    prefs.tun.clone()
}

/// The routing mode as a display string.
pub fn mode_label(mode: ProxyMode) -> &'static str {
    mode.as_str()
}

/// The rules of a payload, translated.
pub fn payload_rules(payload: &SubscriptionPayload) -> Vec<String> {
    translate_all(&payload.rules)
}
