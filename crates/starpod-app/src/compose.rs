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

use std::collections::BTreeMap;

use starpod_core::config::DEFAULT_GROUP_NAME;
use starpod_core::{
    BuildInput, BuildOutcome, EngineManagedSubscription, RuntimeConfig, Severity, SubscriptionMode,
    UserPreferences,
};
use starpod_store::{SubscriptionMeta, UserState};
use starpod_subscription::SubscriptionCacheSet;

/// A candidate configuration and the warnings the build produced for it.
#[derive(Debug, Clone)]
pub(crate) struct Composed {
    /// The candidate, ready to validate.
    pub config: RuntimeConfig,
    /// Non-blocking findings the user should see.
    pub warnings: Vec<String>,
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

    let input = BuildInput {
        preferences: state.preferences.clone(),
        user_rules: state.rules.clone(),
        user_nodes: state.nodes.clone(),
        user_groups: state.groups.clone(),
        subscriptions,
        engine_managed,
        rule_providers: Vec::new(),
    };

    let BuildOutcome { config, findings } = starpod_core::build(&input);
    let Some(mut config) = config else {
        let errors = findings
            .iter()
            .filter(|finding| finding.severity == Severity::Error)
            .map(|finding| finding.message.clone())
            .collect();
        return Err(errors);
    };

    let mut warnings: Vec<String> = findings
        .iter()
        .filter(|finding| finding.severity == Severity::Warning)
        .map(|finding| finding.message.clone())
        .collect();
    select_node(&mut config, &state.preferences, &mut warnings);

    Ok(Composed { config, warnings })
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
