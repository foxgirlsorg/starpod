//! Composing the candidate configuration.
//!
//! This is the seam between the app's stored state and the pure
//! `starpod_core::build`. It gathers the inputs, runs the core, and then
//! applies the one step the core leaves to the caller: moving the user's
//! selected node to the front of its group.
//!
//! The engine has no "select this member" call on the `ProxyEngine` trait,
//! so the selection travels inside the configuration. A select group starts
//! on its first member, so putting the chosen node first makes the engine
//! start on it.

use std::collections::{BTreeMap, BTreeSet};

use starpod_core::config::DEFAULT_GROUP_NAME;
use starpod_core::rules::translate;
use starpod_core::{
    BuildInput, BuildOutcome, EngineManagedSubscription, Finding, ProxyMode, RuleProvider,
    RuleSpec, RuntimeConfig, Severity, SubscriptionMode, UserPreferences,
};
use starpod_store::{SubscriptionMeta, UserState};
use starpod_subscription::SubscriptionCacheSet;

/// Why a rule cannot be used, if its kind needs something the app cannot
/// supply.
///
/// `SUB-RULE` names a block of rules, and nothing in the app defines blocks,
/// so such a rule can never validate. The reason is given here rather than
/// the core's "not declared" finding, which does not say the feature is
/// missing. A `RULE-SET` is not refused here: it is refused only when no
/// declared provider has its name (see [`unresolved_rule_set`]).
pub(crate) fn unsupported_rule(spec: &RuleSpec) -> Option<String> {
    match spec {
        RuleSpec::SubRule { .. } => Some(format!(
            "SUB-RULE rules are not supported yet: the app defines no named rule blocks, so “{}” cannot be used",
            translate(spec)
        )),
        _ => None,
    }
}

/// Why a `RULE-SET` cannot be used: no declared rule provider has its name.
///
/// `declared` holds the names of every provider the subscriptions declare.
/// A rule naming one of them is fine, whichever profile it comes from.
pub(crate) fn unresolved_rule_set(spec: &RuleSpec, declared: &BTreeSet<&str>) -> Option<String> {
    match spec {
        RuleSpec::RuleSet { provider, .. } if !declared.contains(provider.as_str()) => {
            Some(format!(
                "the rule “{}” uses a rule set named “{provider}” that no profile defines",
                translate(spec)
            ))
        }
        _ => None,
    }
}

/// Gather the rule providers the app-managed subscriptions declare.
///
/// The first declaration of a name wins. A later subscription that declares
/// the same name differently is left out, with a warning: the engine has one
/// provider per name, so the two cannot both be kept.
fn collect_rule_providers(
    subscriptions: &[starpod_core::SubscriptionPayload],
) -> (Vec<RuleProvider>, Vec<String>) {
    let mut kept: Vec<RuleProvider> = Vec::new();
    let mut warnings = Vec::new();
    for payload in subscriptions {
        for provider in &payload.rule_providers {
            match kept.iter().find(|known| known.name == provider.name) {
                None => kept.push(provider.clone()),
                Some(known) if known == provider => {}
                Some(_) => warnings.push(format!(
                    "the rule provider “{}” is declared differently by another subscription; the first declaration is used",
                    provider.name
                )),
            }
        }
    }
    (kept, warnings)
}

/// A candidate configuration and the warnings the build produced for it.
#[derive(Debug, Clone)]
pub(crate) struct Composed {
    /// The candidate, ready to validate.
    pub config: RuntimeConfig,
    /// Non-blocking findings the user should see.
    pub warnings: Vec<String>,
    /// The group the Home page offers first. See
    /// [`starpod_core::BuildOutcome::primary_group`].
    pub primary_group: Option<String>,
}

/// Compose the candidate for the given state.
///
/// `engine_urls` maps the id of each engine-managed subscription to its URL.
/// The caller reads them from the secret store, so this function stays free
/// of I/O.
///
/// On failure the error messages are the core's error-severity findings,
/// and nothing is produced: a build with errors has no configuration.
pub(crate) fn compose(
    state: &UserState,
    meta: &SubscriptionMeta,
    caches: &SubscriptionCacheSet,
    engine_urls: &BTreeMap<String, String>,
) -> Result<Composed, Vec<String>> {
    let mut subscriptions = Vec::new();
    let mut engine_managed = Vec::new();
    for subscription in &meta.subscriptions {
        match subscription.mode {
            SubscriptionMode::AppManaged => {
                if let Some(payload) = caches.payload(&subscription.id) {
                    subscriptions.push(payload.clone());
                }
            }
            SubscriptionMode::EngineManaged => {
                if let Some(url) = engine_urls.get(&subscription.id) {
                    engine_managed.push(EngineManagedSubscription {
                        name: subscription.label.clone(),
                        url: url.clone(),
                        interval_secs: subscription.interval_secs,
                        proxy: None,
                    });
                }
            }
        }
    }

    let (rule_providers, provider_warnings) = collect_rule_providers(&subscriptions);

    // Disabled rules take no part in routing, so they cannot break the build.
    let problems: Vec<String> = {
        let declared: BTreeSet<&str> = rule_providers
            .iter()
            .map(|provider| provider.name.as_str())
            .collect();
        state
            .rules
            .iter()
            .filter(|rule| rule.enabled)
            .map(|rule| &rule.spec)
            .chain(
                subscriptions
                    .iter()
                    .flat_map(|payload| payload.rules.iter()),
            )
            .filter_map(|spec| {
                unsupported_rule(spec).or_else(|| unresolved_rule_set(spec, &declared))
            })
            .collect()
    };
    if !problems.is_empty() {
        return Err(problems);
    }

    // With no subscription to supply nodes, the core's empty-group finding is
    // only noise: connecting is already refused with "Add a subscription to
    // connect", so the warning would say nothing the user can act on.
    let no_subscriptions = subscriptions.is_empty() && engine_managed.is_empty();

    let input = BuildInput {
        preferences: state.preferences.clone(),
        user_rules: state.rules.clone(),
        user_nodes: state.nodes.clone(),
        user_groups: state.groups.clone(),
        subscriptions,
        engine_managed,
        rule_providers,
    };

    let BuildOutcome {
        config,
        findings,
        primary_group,
    } = starpod_core::build(&input);
    let Some(mut config) = config else {
        let errors = findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
            .map(|finding| finding.message.clone())
            .collect();
        return Err(errors);
    };

    let mut warnings: Vec<String> = provider_warnings;
    warnings.extend(
        findings
            .iter()
            .filter(|finding| finding.severity == Severity::Warning)
            .filter(|finding| !(no_subscriptions && is_empty_group(finding, &config)))
            .map(|finding| finding.message.clone()),
    );
    apply_group_selections(&mut config, &state.preferences);
    select_node(&mut config, &state.preferences, &mut warnings);
    if config.mode == ProxyMode::Global && global_mode_blocked(meta, caches) {
        config.mode = ProxyMode::Rule;
        warnings.push(
            "your subscription does not allow the Global mode, so the mode is Rule".to_string(),
        );
    }

    Ok(Composed {
        config,
        warnings,
        primary_group,
    })
}

/// Whether an active subscription forbids the Global routing mode.
///
/// A subscription is active when its payload is cached, the same condition
/// under which the build uses it. The provider's `global-mode: false` header
/// is the only thing that forbids Global, and it applies while any active
/// subscription carries it.
pub(crate) fn global_mode_blocked(meta: &SubscriptionMeta, caches: &SubscriptionCacheSet) -> bool {
    meta.subscriptions.iter().any(|subscription| {
        subscription.mode == SubscriptionMode::AppManaged
            && subscription.global_mode_disabled
            && caches.payload(&subscription.id).is_some()
    })
}

/// Move the chosen member of every group to the front of that group.
///
/// A group, or a member, that is no longer there is skipped silently: the
/// choice stays saved and applies again when it comes back.
fn apply_group_selections(config: &mut RuntimeConfig, prefs: &UserPreferences) {
    for (group_name, member) in &prefs.group_selections {
        let Some(group) = config
            .proxy_groups
            .iter_mut()
            .find(|group| group.name == *group_name)
        else {
            continue;
        };
        if let Some(index) = group.members.iter().position(|name| name == member) {
            let chosen = group.members.remove(index);
            group.members.insert(0, chosen);
        }
    }
}

/// Whether a finding is the core's "the group has no members" warning for a
/// group that is in the built configuration with nothing in it.
fn is_empty_group(finding: &Finding, config: &RuntimeConfig) -> bool {
    finding.message.ends_with("has no members")
        && finding.subject.as_deref().is_some_and(|name| {
            config
                .proxy_groups
                .iter()
                .any(|group| group.name == name && group.members.is_empty())
        })
}

/// Move the user's selected node to the front of the selected group.
fn select_node(config: &mut RuntimeConfig, prefs: &UserPreferences, warnings: &mut Vec<String>) {
    let Some(node_id) = prefs.selected_node_id.as_deref() else {
        return;
    };
    let group_name = prefs
        .selected_group
        .as_deref()
        .unwrap_or(DEFAULT_GROUP_NAME)
        .to_string();

    let Some(node_name) = config
        .proxies
        .iter()
        .find(|node| node.id == node_id)
        .map(|node| node.name.clone())
    else {
        warnings.push(
            "the selected node is no longer in the configuration; the group's first node is used"
                .to_string(),
        );
        return;
    };

    let Some(group) = config
        .proxy_groups
        .iter_mut()
        .find(|group| group.name == group_name)
    else {
        return;
    };

    match group.members.iter().position(|member| *member == node_name) {
        Some(index) => {
            let member = group.members.remove(index);
            group.members.insert(0, member);
        }
        None => warnings.push(format!(
            "the selected node “{node_name}” is not a member of the group “{group_name}”"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use starpod_core::Subscription;

    fn engine_managed(id: &str) -> Subscription {
        Subscription {
            id: id.to_string(),
            label: id.to_string(),
            mode: SubscriptionMode::EngineManaged,
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
        }
    }

    fn compose_with(meta: &SubscriptionMeta, engine_urls: &BTreeMap<String, String>) -> Composed {
        compose(
            &UserState::default(),
            meta,
            &SubscriptionCacheSet::new(),
            engine_urls,
        )
        .expect("an empty configuration still builds")
    }

    #[test]
    fn empty_group_warning_is_dropped_when_there_is_no_subscription() {
        let composed = compose_with(&SubscriptionMeta::default(), &BTreeMap::new());

        assert!(
            !composed
                .warnings
                .iter()
                .any(|warning| warning.contains("has no members")),
            "{:?}",
            composed.warnings
        );
    }

    #[test]
    fn empty_group_warning_is_kept_when_a_subscription_is_configured() {
        let meta = SubscriptionMeta {
            subscriptions: vec![engine_managed("sub-1")],
            ..SubscriptionMeta::default()
        };
        let urls = BTreeMap::from([("sub-1".to_string(), "https://example.test/sub".to_string())]);

        let composed = compose_with(&meta, &urls);

        assert!(
            composed
                .warnings
                .iter()
                .any(|warning| warning.contains("has no members")),
            "{:?}",
            composed.warnings
        );
    }

    #[test]
    fn a_saved_group_choice_moves_its_member_to_the_front_and_missing_ones_are_skipped() {
        let node = |id: &str| starpod_core::Node {
            id: id.to_string(),
            name: id.to_string(),
            protocol: "trojan".to_string(),
            server: Some("a.example.com".to_string()),
            port: Some(443),
            tags: Vec::new(),
            raw: Default::default(),
        };
        let mut state = UserState {
            nodes: vec![node("A"), node("B")],
            ..UserState::default()
        };
        state.groups.push(starpod_core::ProxyGroup {
            id: "user-pick".to_string(),
            name: "Pick".to_string(),
            group_type: "select".to_string(),
            members: vec!["A".to_string(), "B".to_string()],
            strategy: None,
            url: None,
            interval: None,
            raw: Default::default(),
        });
        state.preferences.group_selections = BTreeMap::from([
            ("Pick".to_string(), "B".to_string()),
            ("Gone".to_string(), "A".to_string()),
        ]);

        let composed = compose(
            &state,
            &SubscriptionMeta::default(),
            &SubscriptionCacheSet::new(),
            &BTreeMap::new(),
        )
        .expect("the configuration builds");

        let pick = composed
            .config
            .proxy_groups
            .iter()
            .find(|group| group.name == "Pick")
            .expect("the group is built");
        assert_eq!(pick.members.first().map(String::as_str), Some("B"));
        assert!(
            composed.warnings.iter().all(|w| !w.contains("Gone")),
            "{:?}",
            composed.warnings
        );
    }

    #[test]
    fn other_warnings_survive_when_there_is_no_subscription() {
        // Two user groups with the same name: the core renames the second and
        // warns about it. That warning is not about an empty group, so it stays.
        let spare = |id: &str| starpod_core::ProxyGroup {
            id: id.to_string(),
            name: "Spare".to_string(),
            group_type: "select".to_string(),
            members: Vec::new(),
            strategy: None,
            url: None,
            interval: None,
            raw: Default::default(),
        };
        let mut state = UserState::default();
        state.groups.push(spare("user-spare-1"));
        state.groups.push(spare("user-spare-2"));

        let composed = compose(
            &state,
            &SubscriptionMeta::default(),
            &SubscriptionCacheSet::new(),
            &BTreeMap::new(),
        )
        .expect("an empty configuration still builds");

        assert!(
            composed
                .warnings
                .iter()
                .any(|warning| warning.contains("already named “Spare”")),
            "{:?}",
            composed.warnings
        );
        assert!(
            !composed
                .warnings
                .iter()
                .any(|warning| warning.contains("has no members")),
            "{:?}",
            composed.warnings
        );
    }
}
