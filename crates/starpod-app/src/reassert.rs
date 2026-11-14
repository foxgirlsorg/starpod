//! Engine-managed subscriptions after an engine refresh (ARCHITECTURE §4.6,
//! R12).
//!
//! The engine refreshes an engine-managed subscription on its own schedule,
//! and each refresh can invert the rule order and replace a node the user
//! owns. The service asks two questions here, and both are pure:
//!
//! - did a managed subscription refresh since the last look? ([`RefreshWatch`])
//! - which of the user's own nodes does a subscription now shadow?
//!   ([`shadow_warnings`])
//!
//! The I/O (reading the engine's state, posting the rule order) stays in the
//! service, which holds the lock on the engine.

use std::collections::BTreeMap;

use serde_json::Value;
use starpod_engine::reassert::{EngineSubscription, LiveProxy};

/// A node the user defined, as the shadow check needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnNode {
    /// The node's name, which is what a subscription would shadow.
    pub name: String,
    /// The server the user configured.
    pub server: Option<String>,
    /// The port the user configured.
    pub port: Option<u16>,
}

/// What the engine last reported for each engine-managed subscription.
///
/// The first observation after a reset counts as a change. The engine
/// applies a subscription when it starts, and that application already puts
/// the subscription's rules ahead of the user's, so the first look needs the
/// same correction as a refresh.
#[derive(Debug, Default)]
pub struct RefreshWatch {
    seen: Option<BTreeMap<String, Option<Value>>>,
}

impl RefreshWatch {
    /// Forget what was seen. Called when the engine stops, so the next start
    /// counts as a first observation.
    pub fn reset(&mut self) {
        self.seen = None;
    }

    /// Record what the engine reports and say whether a refresh happened.
    ///
    /// Only the subscriptions named in `managed` are watched. A change is a
    /// different `last_updated` for any of them, a managed subscription that
    /// appeared or disappeared, or the first observation since a reset.
    pub fn observe(&mut self, listed: &[EngineSubscription], managed: &[String]) -> bool {
        let current: BTreeMap<String, Option<Value>> = listed
            .iter()
            .filter(|subscription| managed.contains(&subscription.name))
            .map(|subscription| (subscription.name.clone(), subscription.last_updated.clone()))
            .collect();
        let changed = match &self.seen {
            None => true,
            Some(previous) => *previous != current,
        };
        self.seen = Some(current);
        changed
    }
}

/// Warnings for user nodes that an engine-managed subscription shadows.
///
/// A live proxy with the same name as a user node is a shadow only when the
/// engine reports a server or port that differs from the user's. A live
/// proxy that reports neither cannot be told apart from the user's own node,
/// so it is not reported; the warning is therefore a lower bound.
///
/// The engine does not say which subscription supplied a node, so the
/// warning names every managed subscription when there is more than one.
pub fn shadow_warnings(own: &[OwnNode], live: &[LiveProxy], managed: &[String]) -> Vec<String> {
    if managed.is_empty() {
        return Vec::new();
    }
    let owners = describe_owners(managed);
    own.iter()
        .filter(|node| {
            live.iter()
                .filter(|proxy| proxy.name == node.name)
                .any(|proxy| differs(node, proxy))
        })
        .map(|node| {
            format!(
                "{owners} is engine-managed and has a node named “{}” that replaces your own node of the same name. The engine does not restore your node when the subscription drops it. Switch {owners} to app-managed to keep your node.",
                node.name
            )
        })
        .collect()
}

fn differs(own: &OwnNode, live: &LiveProxy) -> bool {
    let server = matches!((&own.server, &live.server), (Some(a), Some(b)) if a != b);
    let port = matches!((own.port, live.port), (Some(a), Some(b)) if a != b);
    server || port
}

fn describe_owners(managed: &[String]) -> String {
    match managed {
        [only] => format!("the subscription “{only}”"),
        many => {
            let names: Vec<String> = many.iter().map(|name| format!("“{name}”")).collect();
            format!("one of the subscriptions {}", names.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(name: &str, last: Option<Value>) -> EngineSubscription {
        EngineSubscription {
            name: name.to_string(),
            last_updated: last,
        }
    }

    fn own(name: &str, server: Option<&str>, port: Option<u16>) -> OwnNode {
        OwnNode {
            name: name.to_string(),
            server: server.map(str::to_string),
            port,
        }
    }

    fn live(name: &str, server: Option<&str>, port: Option<u16>) -> LiveProxy {
        LiveProxy {
            name: name.to_string(),
            server: server.map(str::to_string),
            port,
            now: None,
        }
    }

    #[test]
    fn the_first_observation_and_every_refresh_count_as_a_change() {
        let mut watch = RefreshWatch::default();
        let managed = vec!["Sub".to_string()];
        assert!(watch.observe(&[sub("Sub", Some(Value::from(1)))], &managed));
        assert!(!watch.observe(&[sub("Sub", Some(Value::from(1)))], &managed));
        assert!(watch.observe(&[sub("Sub", Some(Value::from(2)))], &managed));
        assert!(!watch.observe(&[sub("Sub", Some(Value::from(2)))], &managed));
    }

    #[test]
    fn a_reset_makes_the_next_start_count_as_a_change_again() {
        let mut watch = RefreshWatch::default();
        let managed = vec!["Sub".to_string()];
        watch.observe(&[sub("Sub", Some(Value::from(1)))], &managed);
        watch.reset();
        assert!(watch.observe(&[sub("Sub", Some(Value::from(1)))], &managed));
    }

    #[test]
    fn only_managed_subscriptions_are_watched() {
        let mut watch = RefreshWatch::default();
        let managed = vec!["Sub".to_string()];
        watch.observe(&[sub("Sub", Some(Value::from(1)))], &managed);
        assert!(!watch.observe(
            &[
                sub("Sub", Some(Value::from(1))),
                sub("Unrelated", Some(Value::from(9)))
            ],
            &managed
        ));
    }

    #[test]
    fn a_managed_subscription_that_disappears_is_a_change() {
        let mut watch = RefreshWatch::default();
        let managed = vec!["Sub".to_string()];
        watch.observe(&[sub("Sub", Some(Value::from(1)))], &managed);
        assert!(watch.observe(&[], &managed));
    }

    #[test]
    fn a_shadowing_node_is_named_with_its_subscription() {
        let warnings = shadow_warnings(
            &[own("Home", Some("home.example"), Some(443))],
            &[live("Home", Some("remote.example"), Some(443))],
            &["Airport".to_string()],
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("“Airport”"), "{warnings:?}");
        assert!(warnings[0].contains("“Home”"), "{warnings:?}");
        assert!(warnings[0].contains("app-managed"), "{warnings:?}");
    }

    #[test]
    fn a_node_that_matches_the_users_own_is_not_a_shadow() {
        let warnings = shadow_warnings(
            &[own("Home", Some("home.example"), Some(443))],
            &[live("Home", Some("home.example"), Some(443))],
            &["Airport".to_string()],
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_port_difference_alone_is_a_shadow() {
        let warnings = shadow_warnings(
            &[own("Home", Some("home.example"), Some(443))],
            &[live("Home", Some("home.example"), Some(8443))],
            &["Airport".to_string()],
        );
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn a_live_proxy_that_reports_no_address_is_not_called_a_shadow() {
        let warnings = shadow_warnings(
            &[own("Home", Some("home.example"), Some(443))],
            &[live("Home", None, None)],
            &["Airport".to_string()],
        );
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn a_node_with_no_live_counterpart_is_not_reported_here() {
        let warnings = shadow_warnings(
            &[own("Home", Some("home.example"), Some(443))],
            &[live("Other", Some("x"), Some(1))],
            &["Airport".to_string()],
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn with_several_managed_subscriptions_each_is_named_as_a_candidate() {
        let warnings = shadow_warnings(
            &[own("Home", Some("home.example"), None)],
            &[live("Home", Some("remote.example"), None)],
            &["A".to_string(), "B".to_string()],
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("“A”") && warnings[0].contains("“B”"));
    }

    #[test]
    fn no_managed_subscription_means_no_warning() {
        let warnings = shadow_warnings(
            &[own("Home", Some("home.example"), None)],
            &[live("Home", Some("remote.example"), None)],
            &[],
        );
        assert!(warnings.is_empty());
    }
}
