//! Tests for the runtime configuration pipeline.
//!
//! These are the tests GOAL.md §3 asks for: "the configuration builder must
//! be independently testable, not coupled to the GUI or the engine". Every
//! test here runs with no window, no engine and no network — the pipeline is
//! a pure function, and this file is the proof.

use crate::config::{DEFAULT_GROUP_NAME, build};
use crate::domain::{
    BuildInput, Node, ProxyGroup, ProxyMode, RuleProvider, RuleProviderBehavior, StoredRule,
    SubscriptionPayload, SubscriptionSettings, UserPreferences,
};
use crate::rules::{RuleFlags, RuleKind, RuleSpec, RuleTarget};

/// A node with the given name.
fn node(name: &str) -> Node {
    Node {
        id: format!("id-{name}"),
        name: name.to_string(),
        protocol: "vmess".to_string(),
        server: Some("example.invalid".to_string()),
        port: Some(443),
        tags: Vec::new(),
        raw: Default::default(),
    }
}

/// A group with the given name and members.
fn group(name: &str, members: &[&str]) -> ProxyGroup {
    ProxyGroup {
        id: format!("id-{name}"),
        name: name.to_string(),
        group_type: "select".to_string(),
        members: members.iter().map(|m| m.to_string()).collect(),
        strategy: None,
        url: None,
        interval: None,
        hidden: false,
        raw: Default::default(),
    }
}

/// A simple top-level rule.
fn rule(id: &str, order: u32, kind: RuleKind, payload: &str, target: &str) -> StoredRule {
    StoredRule {
        id: id.to_string(),
        spec: RuleSpec::Simple {
            kind,
            payload: payload.to_string(),
            target: RuleTarget::new(target),
            flags: RuleFlags::default(),
        },
        enabled: true,
        order,
    }
}

/// A subscription payload carrying nodes and rules.
fn payload(subscription_id: &str, nodes: Vec<Node>, rules: Vec<RuleSpec>) -> SubscriptionPayload {
    SubscriptionPayload {
        subscription_id: subscription_id.to_string(),
        nodes,
        groups: Vec::new(),
        rules,
        rule_providers: Vec::new(),
        settings: SubscriptionSettings::default(),
        dropped_entries: 0,
        dns: None,
    }
}

fn base_input() -> BuildInput {
    BuildInput {
        preferences: UserPreferences::default(),
        user_rules: Vec::new(),
        user_nodes: Vec::new(),
        user_groups: Vec::new(),
        subscriptions: Vec::new(),
        engine_managed: Vec::new(),
        rule_providers: Vec::new(),
    }
}

// --- P1: ports, allow_lan, bind_address ------------------------------------

#[test]
fn the_users_port_wins_over_the_subscriptions_port() {
    let mut input = base_input();
    input.preferences.mixed_port = Some(1080);
    let mut sub = payload("s1", Vec::new(), Vec::new());
    sub.settings.mixed_port = Some(7890);
    input.subscriptions.push(sub);

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(config.mixed_port, Some(1080));
}

#[test]
fn the_subscriptions_port_is_used_when_the_user_set_none() {
    let mut input = base_input();
    input.preferences.mixed_port = None;
    let mut sub = payload("s1", Vec::new(), Vec::new());
    sub.settings.mixed_port = Some(7899);
    input.subscriptions.push(sub);

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(config.mixed_port, Some(7899));
}

#[test]
fn the_default_port_is_used_when_nobody_set_one() {
    let mut input = base_input();
    input.preferences.mixed_port = None;

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(config.mixed_port, Some(crate::config::DEFAULT_MIXED_PORT));
}

// Regression: `UserPreferences::default()` used to pin the ports to 7890 and
// 7891, which the merge read as the user's explicit choice, so a new user's
// subscription ports never applied. A new user has chosen nothing.
#[test]
fn a_new_users_subscription_ports_apply_because_the_default_pins_none() {
    let mut input = base_input();
    let mut sub = payload("s1", Vec::new(), Vec::new());
    sub.settings.mixed_port = Some(7899);
    sub.settings.socks_port = Some(7898);
    input.subscriptions.push(sub);

    let config = build(&input).config.expect("the build should succeed");
    assert_eq!(config.mixed_port, Some(7899));
    assert_eq!(config.socks_port, Some(7898));
}

#[test]
fn a_new_user_with_no_subscription_port_gets_the_documented_defaults() {
    let config = build(&base_input())
        .config
        .expect("the build should succeed");
    assert_eq!(config.mixed_port, Some(crate::config::DEFAULT_MIXED_PORT));
    assert_eq!(config.socks_port, Some(crate::config::DEFAULT_SOCKS_PORT));
}

#[test]
fn the_users_allow_lan_choice_is_never_overridden_by_a_subscription() {
    let mut input = base_input();
    input.preferences.allow_lan = Some(false);
    let mut sub = payload("s1", Vec::new(), Vec::new());
    sub.settings.allow_lan = Some(true);
    input.subscriptions.push(sub);

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert!(
        !config.allow_lan,
        "a subscription must not be able to expose the proxy to the LAN"
    );
}

#[test]
fn the_users_explicit_lan_choice_survives_a_subscription() {
    let mut input = base_input();
    input.preferences.allow_lan = Some(true);
    let mut sub = payload("s1", Vec::new(), Vec::new());
    sub.settings.allow_lan = Some(false);
    input.subscriptions.push(sub);

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert!(config.allow_lan, "the user's explicit choice must win");
}

#[test]
fn lan_exposure_is_off_by_default() {
    let outcome = build(&base_input());
    let config = outcome.config.expect("the build should succeed");
    assert!(!config.allow_lan, "the default must not expose the proxy");
}

// --- P2: mode, log_level ----------------------------------------------------

#[test]
fn the_subscriptions_mode_is_used_when_the_user_did_not_choose_one() {
    let mut input = base_input();
    let mut sub = payload("s1", Vec::new(), Vec::new());
    sub.settings.mode = Some(ProxyMode::Global);
    input.subscriptions.push(sub);

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(config.mode, ProxyMode::Global);
}

// --- P3: nodes --------------------------------------------------------------

#[test]
fn the_nodes_are_the_union_of_the_users_and_the_subscriptions() {
    let mut input = base_input();
    input.user_nodes.push(node("mine"));
    input
        .subscriptions
        .push(payload("s1", vec![node("theirs")], Vec::new()));

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    let names: Vec<&str> = config.proxies.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"mine"), "{names:?}");
    assert!(names.contains(&"theirs"), "{names:?}");
}

#[test]
fn the_users_node_keeps_its_name_when_a_subscription_carries_the_same_one() {
    let mut input = base_input();
    let mut mine = node("shared");
    mine.server = Some("my-server.invalid".to_string());
    input.user_nodes.push(mine);
    input
        .subscriptions
        .push(payload("s1", vec![node("shared")], Vec::new()));

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    let shared: Vec<&Node> = config
        .proxies
        .iter()
        .filter(|n| n.name == "shared")
        .collect();
    assert_eq!(
        shared.len(),
        1,
        "the two nodes must not both be named “shared”"
    );
    assert_eq!(
        shared[0].server.as_deref(),
        Some("my-server.invalid"),
        "the user's node must win the name collision"
    );
}

#[test]
fn the_renamed_node_is_reported_to_the_user() {
    let mut input = base_input();
    input.user_nodes.push(node("shared"));
    input
        .subscriptions
        .push(payload("s1", vec![node("shared")], Vec::new()));

    let outcome = build(&input);
    assert!(
        outcome
            .findings
            .iter()
            .any(|f| f.message.contains("renamed")),
        "the rename should be reported: {:?}",
        outcome.findings
    );
}

// --- P4: proxy groups -------------------------------------------------------

#[test]
fn the_users_group_wins_over_a_subscription_group_of_the_same_name() {
    let mut input = base_input();
    input.user_nodes.push(node("mine"));
    input.user_nodes.push(node("theirs"));
    input.user_groups.push(group("PROXY", &["mine"]));
    let mut sub = payload("s1", Vec::new(), Vec::new());
    sub.groups.push(group("PROXY", &["theirs"]));
    input.subscriptions.push(sub);

    let outcome = build(&input);
    let config = outcome
        .config
        .unwrap_or_else(|| panic!("the build should succeed: {:?}", outcome.findings));
    let proxy_groups: Vec<&ProxyGroup> = config
        .proxy_groups
        .iter()
        .filter(|g| g.name == "PROXY")
        .collect();
    assert_eq!(proxy_groups.len(), 1);
    assert_eq!(proxy_groups[0].members, vec!["mine".to_string()]);
}

#[test]
fn a_default_group_is_always_created() {
    let outcome = build(&base_input());
    let config = outcome.config.expect("the build should succeed");
    assert!(
        config
            .proxy_groups
            .iter()
            .any(|g| g.name == DEFAULT_GROUP_NAME),
        "there must always be a group to select a node from"
    );
}

/// A group of the given type with the given members.
fn typed_group(name: &str, group_type: &str, members: &[&str]) -> ProxyGroup {
    ProxyGroup {
        group_type: group_type.to_string(),
        ..group(name, members)
    }
}

/// The names of the groups in configuration order.
fn group_names(config: &crate::domain::RuntimeConfig) -> Vec<&str> {
    config
        .proxy_groups
        .iter()
        .map(|g| g.name.as_str())
        .collect()
}

#[test]
fn groups_are_listed_in_the_order_they_were_written_not_by_name() {
    let mut input = base_input();
    input.user_nodes.push(node("u1"));
    input.user_groups.push(group("Bravo", &["u1"]));
    let mut sub = payload("s1", vec![node("n2"), node("n1")], Vec::new());
    sub.groups.push(group("Zeta", &["n2", "n1"]));
    sub.groups.push(typed_group("Alpha", "url-test", &["n1"]));
    sub.groups.push(group("Mid", &["n1", "n2"]));
    input.subscriptions.push(sub);

    let config = build(&input).config.expect("the build should succeed");
    assert_eq!(
        group_names(&config),
        vec!["PROXY", "Zeta", "Alpha", "Mid", "Bravo"],
        "the app's default first, then the subscription's groups in YAML order, then the user's"
    );
    let zeta = config
        .proxy_groups
        .iter()
        .find(|g| g.name == "Zeta")
        .unwrap();
    assert_eq!(
        zeta.members,
        vec!["n2", "n1"],
        "members keep their YAML order"
    );
    let proxy = config
        .proxy_groups
        .iter()
        .find(|g| g.name == "PROXY")
        .unwrap();
    assert_eq!(
        proxy.members,
        vec!["u1", "n2", "n1"],
        "the default group lists the nodes in the order they were written"
    );
}

#[test]
fn a_subscription_match_moves_the_default_group_after_the_subscriptions_groups() {
    let mut input = base_input();
    let mut sub = payload(
        "s1",
        vec![node("n1")],
        vec![RuleSpec::Match {
            target: RuleTarget::new("Alpha"),
        }],
    );
    sub.groups.push(group("Zeta", &["n1"]));
    sub.groups.push(typed_group("Alpha", "url-test", &["n1"]));
    input.subscriptions.push(sub);

    let config = build(&input).config.expect("the build should succeed");
    assert_eq!(
        group_names(&config),
        vec!["Zeta", "Alpha", "PROXY"],
        "PROXY is listed after the subscription's groups when it supplies a MATCH"
    );
}

#[test]
fn user_groups_come_after_the_subscriptions_groups() {
    let mut input = base_input();
    input.user_nodes.push(node("u1"));
    input.user_groups.push(group("Mine", &["u1"]));
    let mut sub = payload("s1", vec![node("n1")], Vec::new());
    sub.groups.push(group("Theirs", &["n1"]));
    input.subscriptions.push(sub);

    let config = build(&input).config.expect("the build should succeed");
    let names = group_names(&config);
    let theirs = names.iter().position(|n| *n == "Theirs").unwrap();
    let mine = names.iter().position(|n| *n == "Mine").unwrap();
    assert!(
        theirs < mine,
        "subscription groups come before the user's: {names:?}"
    );
}

// --- primary group ----------------------------------------------------------

#[test]
fn the_primary_group_is_the_target_of_a_subscriptions_own_match() {
    let mut input = base_input();
    let mut sub = payload(
        "s1",
        vec![node("n1")],
        vec![RuleSpec::Match {
            target: RuleTarget::new("Alpha"),
        }],
    );
    sub.groups.push(group("Zeta", &["n1"]));
    sub.groups.push(typed_group("Alpha", "url-test", &["n1"]));
    input.subscriptions.push(sub);

    let outcome = build(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.findings);
    assert_eq!(outcome.primary_group.as_deref(), Some("Alpha"));
}

#[test]
fn without_a_match_the_primary_group_is_the_tail_group_the_app_default() {
    let mut input = base_input();
    input.user_nodes.push(node("u1"));
    input.user_groups.push(group("Bravo", &["u1"]));
    let mut sub = payload("s1", vec![node("n1")], Vec::new());
    sub.groups.push(typed_group("Alpha", "url-test", &["n1"]));
    sub.groups.push(group("Zeta", &["n1"]));
    input.subscriptions.push(sub);

    let outcome = build(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.findings);
    assert_eq!(
        outcome.primary_group.as_deref(),
        Some(DEFAULT_GROUP_NAME),
        "with no MATCH the tail is PROXY, so the picker must offer PROXY"
    );
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(config.rules.last().map(String::as_str), Some("MATCH,PROXY"));
}

// --- the MATCH tail's target -----------------------------------------------

/// The tail rule of a built configuration.
fn tail(config: &crate::domain::RuntimeConfig) -> &str {
    config.rules.last().expect("there is a tail rule")
}

#[test]
fn a_subscriptions_own_match_is_the_tail_target() {
    let mut input = base_input();
    let mut sub = payload(
        "s1",
        vec![node("n1")],
        vec![RuleSpec::Match {
            target: RuleTarget::new("Airport / Main"),
        }],
    );
    sub.groups.push(group("Airport / Main", &["n1"]));
    sub.groups.push(group("Airport / Other", &["n1"]));
    input.subscriptions.push(sub);

    let outcome = build(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.findings);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(tail(&config), "MATCH,Airport / Main");
    assert_eq!(outcome.primary_group.as_deref(), Some("Airport / Main"));
}

#[test]
fn the_users_selected_group_is_the_tail_target_over_a_subscriptions_match() {
    let mut input = base_input();
    input.preferences.selected_group = Some("Pick".to_string());
    input.user_nodes.push(node("u1"));
    input.user_groups.push(group("Pick", &["u1"]));
    let mut sub = payload(
        "s1",
        vec![node("n1")],
        vec![RuleSpec::Match {
            target: RuleTarget::new("Alpha"),
        }],
    );
    sub.groups.push(group("Alpha", &["n1"]));
    input.subscriptions.push(sub);

    let outcome = build(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.findings);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(tail(&config), "MATCH,Pick");
    assert_eq!(outcome.primary_group.as_deref(), Some("Pick"));
}

#[test]
fn a_selected_group_that_does_not_exist_falls_back_to_the_subscriptions_match() {
    let mut input = base_input();
    input.preferences.selected_group = Some("Gone".to_string());
    let mut sub = payload(
        "s1",
        vec![node("n1")],
        vec![RuleSpec::Match {
            target: RuleTarget::new("Alpha"),
        }],
    );
    sub.groups.push(group("Alpha", &["n1"]));
    input.subscriptions.push(sub);

    let outcome = build(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.findings);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(tail(&config), "MATCH,Alpha");
    assert!(
        outcome.findings.iter().any(|f| f.message.contains("Gone")),
        "the missing selection is still reported"
    );
}

#[test]
fn a_match_naming_a_builtin_outbound_is_the_tail_target() {
    let mut input = base_input();
    input.subscriptions.push(payload(
        "s1",
        vec![node("n1")],
        vec![RuleSpec::Match {
            target: RuleTarget::new("DIRECT"),
        }],
    ));

    let outcome = build(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.findings);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(tail(&config), "MATCH,DIRECT");
    assert_eq!(
        outcome.primary_group.as_deref(),
        Some(DEFAULT_GROUP_NAME),
        "a builtin tail is not a group, so the picker falls back to the first select group"
    );
}

#[test]
fn a_match_naming_a_missing_group_falls_back_to_the_app_default() {
    let mut input = base_input();
    input.subscriptions.push(payload(
        "s1",
        vec![node("n1")],
        vec![RuleSpec::Match {
            target: RuleTarget::new("Ghost"),
        }],
    ));

    let outcome = build(&input);
    assert!(outcome.is_ok(), "{:?}", outcome.findings);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(tail(&config), "MATCH,PROXY");
    assert_eq!(outcome.primary_group.as_deref(), Some(DEFAULT_GROUP_NAME));
}

#[test]
fn without_a_subscription_group_the_primary_group_is_the_first_select_group() {
    let mut input = base_input();
    input.user_nodes.push(node("u1"));
    input
        .user_groups
        .push(typed_group("Auto", "url-test", &["u1"]));
    input.user_groups.push(group("Pick", &["u1"]));

    let outcome = build(&input);
    assert_eq!(
        outcome.primary_group.as_deref(),
        Some(DEFAULT_GROUP_NAME),
        "the app's PROXY is the first select group"
    );
}

#[test]
fn a_build_that_fails_has_no_primary_group() {
    let mut input = base_input();
    input.preferences.mixed_port = Some(7890);
    input.preferences.socks_port = Some(7890);

    let outcome = build(&input);
    assert!(!outcome.is_ok());
    assert_eq!(outcome.primary_group, None);
}

// --- P5: rules --------------------------------------------------------------

#[test]
fn the_users_rules_come_before_the_subscriptions_rules() {
    let mut input = base_input();
    input
        .user_rules
        .push(rule("u1", 0, RuleKind::Domain, "mine.com", "DIRECT"));
    input.subscriptions.push(payload(
        "s1",
        Vec::new(),
        vec![RuleSpec::Simple {
            kind: RuleKind::Domain,
            payload: "theirs.com".to_string(),
            target: RuleTarget::new("DIRECT"),
            flags: RuleFlags::default(),
        }],
    ));

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    let mine = config
        .rules
        .iter()
        .position(|r| r == "DOMAIN,mine.com,DIRECT")
        .expect("the user's rule is present");
    let theirs = config
        .rules
        .iter()
        .position(|r| r == "DOMAIN,theirs.com,DIRECT")
        .expect("the subscription's rule is present");
    assert!(mine < theirs, "the user's rule must be evaluated first");
}

#[test]
fn the_match_tail_is_always_last() {
    let mut input = base_input();
    input
        .user_rules
        .push(rule("u1", 0, RuleKind::Domain, "mine.com", "DIRECT"));

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert!(config.rules.last().unwrap().starts_with("MATCH,"));
    assert_eq!(
        config
            .rules
            .iter()
            .filter(|r| r.starts_with("MATCH,"))
            .count(),
        1,
        "there must be exactly one MATCH rule"
    );
}

#[test]
fn a_rules_after_the_match_are_removed_and_reported() {
    let mut input = base_input();
    input
        .user_rules
        .push(rule("u1", 0, RuleKind::Domain, "a.com", "DIRECT"));
    input
        .user_rules
        .push(rule("u2", 1, RuleKind::Match, "", "DIRECT"));
    input
        .user_rules
        .push(rule("u3", 2, RuleKind::Domain, "b.com", "DIRECT"));

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert!(
        !config.rules.iter().any(|r| r == "DOMAIN,b.com,DIRECT"),
        "a rule after MATCH can never match"
    );
    assert!(
        outcome
            .findings
            .iter()
            .any(|f| f.message.contains("after the MATCH")),
        "the removal should be reported"
    );
}

#[test]
fn a_disabled_rule_is_not_emitted() {
    let mut input = base_input();
    let mut disabled = rule("u1", 0, RuleKind::Domain, "a.com", "DIRECT");
    disabled.enabled = false;
    input.user_rules.push(disabled);

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert!(!config.rules.iter().any(|r| r.contains("a.com")));
}

#[test]
fn the_rules_are_ordered_by_their_order_field() {
    let mut input = base_input();
    input
        .user_rules
        .push(rule("u2", 1, RuleKind::Domain, "second.com", "DIRECT"));
    input
        .user_rules
        .push(rule("u1", 0, RuleKind::Domain, "first.com", "DIRECT"));

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    let first = config
        .rules
        .iter()
        .position(|r| r.contains("first.com"))
        .unwrap();
    let second = config
        .rules
        .iter()
        .position(|r| r.contains("second.com"))
        .unwrap();
    assert!(first < second);
}

// --- Validation -------------------------------------------------------------

#[test]
fn a_rule_targeting_a_missing_group_is_an_error() {
    let mut input = base_input();
    input
        .user_rules
        .push(rule("u1", 0, RuleKind::Domain, "a.com", "Nonexistent"));

    let outcome = build(&input);
    assert!(outcome.config.is_none(), "the build must not succeed");
    assert!(
        outcome.errors().any(|f| f.message.contains("Nonexistent")),
        "the missing target should be reported: {:?}",
        outcome.findings
    );
}

/// A logic rule as the user would write it, with the given target.
fn logic_rule(id: &str, target: &str) -> StoredRule {
    StoredRule {
        id: id.to_string(),
        spec: crate::rules::parse_line(&format!(
            "AND,((DOMAIN,a.example.com),(DST-PORT,443)),{target}"
        ))
        .expect("the logic rule parses"),
        enabled: true,
        order: 0,
    }
}

// Regression: the target of a logic rule was read with a naive comma split,
// so `AND,((DOMAIN,a.example.com),(DST-PORT,443)),Auto` yielded the target
// `a.example.com)` and the build rejected a perfectly valid rule.
#[test]
fn a_logic_rule_targeting_an_existing_group_is_accepted() {
    let mut input = base_input();
    input.user_nodes.push(node("n1"));
    input.user_groups.push(group("Auto", &["n1"]));
    input.user_rules.push(logic_rule("u1", "Auto"));

    let outcome = build(&input);
    assert!(outcome.config.is_some(), "{:?}", outcome.findings);
}

#[test]
fn a_logic_rule_targeting_a_missing_group_is_an_error_naming_that_group() {
    let mut input = base_input();
    input.user_rules.push(logic_rule("u1", "Nowhere"));

    let outcome = build(&input);
    assert!(outcome.config.is_none(), "the build must not succeed");
    assert!(
        outcome.errors().any(|f| f.message.contains("“Nowhere”")),
        "the missing target should be reported by name: {:?}",
        outcome.findings
    );
}

#[test]
fn a_logic_rule_targeting_a_builtin_outbound_is_accepted() {
    let mut input = base_input();
    input.user_rules.push(logic_rule("u1", "DIRECT"));

    let outcome = build(&input);
    assert!(outcome.config.is_some(), "{:?}", outcome.findings);
}

#[test]
fn a_rule_targeting_a_builtin_outbound_is_accepted() {
    let mut input = base_input();
    input
        .user_rules
        .push(rule("u1", 0, RuleKind::Domain, "a.com", "DIRECT"));

    let outcome = build(&input);
    assert!(outcome.config.is_some(), "{:?}", outcome.findings);
}

#[test]
fn two_ports_on_the_same_number_is_an_error() {
    let mut input = base_input();
    input.preferences.mixed_port = Some(7890);
    input.preferences.socks_port = Some(7890);

    let outcome = build(&input);
    assert!(outcome.config.is_none());
    assert!(
        outcome.errors().any(|f| f.message.contains("must differ")),
        "{:?}",
        outcome.findings
    );
}

#[test]
fn a_group_containing_itself_is_an_error() {
    let mut input = base_input();
    input.user_groups.push(group("Loop", &["Loop"]));

    let outcome = build(&input);
    assert!(outcome.config.is_none());
    assert!(
        outcome
            .errors()
            .any(|f| f.message.contains("contains itself")),
        "{:?}",
        outcome.findings
    );
}

#[test]
fn a_cycle_between_groups_is_an_error() {
    let mut input = base_input();
    input.user_groups.push(group("A", &["B"]));
    input.user_groups.push(group("B", &["A"]));

    let outcome = build(&input);
    assert!(outcome.config.is_none());
    assert!(
        outcome.errors().any(|f| f.message.contains("cycle")),
        "{:?}",
        outcome.findings
    );
}

#[test]
fn a_rule_set_referencing_an_undeclared_provider_is_an_error() {
    let mut input = base_input();
    input.user_rules.push(StoredRule {
        id: "u1".to_string(),
        spec: RuleSpec::RuleSet {
            provider: "missing".to_string(),
            target: RuleTarget::new("DIRECT"),
            flags: RuleFlags::default(),
        },
        enabled: true,
        order: 0,
    });

    let outcome = build(&input);
    assert!(outcome.config.is_none());
    assert!(
        outcome.errors().any(|f| f.message.contains("not declared")),
        "{:?}",
        outcome.findings
    );
}

#[test]
fn a_rule_set_referencing_a_declared_provider_is_accepted() {
    let mut input = base_input();
    input.user_rules.push(StoredRule {
        id: "u1".to_string(),
        spec: RuleSpec::RuleSet {
            provider: "present".to_string(),
            target: RuleTarget::new("DIRECT"),
            flags: RuleFlags::default(),
        },
        enabled: true,
        order: 0,
    });
    input.rule_providers.push(RuleProvider {
        name: "present".to_string(),
        provider_type: "http".to_string(),
        url: Some("https://example.invalid/rules.dat".to_string()),
        path: None,
        behavior: RuleProviderBehavior::Domain,
        interval: None,
        format: None,
        payload: None,
        proxy: None,
    });

    let outcome = build(&input);
    assert!(outcome.config.is_some(), "{:?}", outcome.findings);
}

#[test]
fn a_rule_set_with_no_resolve_is_accepted_when_its_provider_is_declared() {
    let mut input = base_input();
    input.rule_providers.push(RuleProvider {
        name: "present".to_string(),
        provider_type: "http".to_string(),
        url: Some("https://example.invalid/rules.dat".to_string()),
        path: Some("rule-providers/present.yaml".to_string()),
        behavior: RuleProviderBehavior::IpCidr,
        interval: None,
        format: Some("yaml".to_string()),
        payload: None,
        proxy: None,
    });
    input.user_rules.push(StoredRule {
        id: "u1".to_string(),
        spec: RuleSpec::RuleSet {
            provider: "present".to_string(),
            target: RuleTarget::new("DIRECT"),
            flags: RuleFlags::parse(Some("no-resolve")),
        },
        enabled: true,
        order: 0,
    });

    let outcome = build(&input);
    let config = outcome.config.expect("the build succeeds");
    assert!(
        config
            .rules
            .contains(&"RULE-SET,present,DIRECT,no-resolve".to_string()),
        "{:?}",
        config.rules
    );
}

#[test]
fn a_file_rule_provider_is_refused_by_the_build() {
    let mut input = base_input();
    input.rule_providers.push(RuleProvider {
        name: "local".to_string(),
        provider_type: "file".to_string(),
        url: None,
        path: Some("/etc/passwd".to_string()),
        behavior: RuleProviderBehavior::Domain,
        interval: None,
        format: None,
        payload: None,
        proxy: None,
    });

    let outcome = build(&input);
    assert!(outcome.config.is_none());
    assert!(
        outcome.errors().any(|f| f.message.contains("“file”")),
        "{:?}",
        outcome.findings
    );
}

#[test]
fn a_geoip_rule_produces_a_warning_but_not_an_error() {
    let mut input = base_input();
    input
        .user_rules
        .push(rule("u1", 0, RuleKind::GeoIp, "CN", "DIRECT"));

    let outcome = build(&input);
    assert!(
        outcome.config.is_some(),
        "a geo rule must not block the configuration: {:?}",
        outcome.findings
    );
    assert!(
        outcome
            .findings
            .iter()
            .any(|f| f.message.contains("geo database")),
        "the geodata download should be warned about"
    );
}

// --- Determinism ------------------------------------------------------------

#[test]
fn the_same_input_always_produces_the_same_configuration() {
    let mut input = base_input();
    input.user_nodes.push(node("a"));
    input.user_nodes.push(node("b"));
    input
        .subscriptions
        .push(payload("s1", vec![node("c"), node("a")], Vec::new()));

    let first = build(&input);
    let second = build(&input);
    assert_eq!(first.config, second.config);
    assert_eq!(first.findings, second.findings);
}

#[test]
fn the_build_does_not_mutate_its_input() {
    let mut input = base_input();
    input.user_nodes.push(node("shared"));
    input
        .subscriptions
        .push(payload("s1", vec![node("shared")], Vec::new()));

    let before = input.clone();
    let _ = build(&input);
    assert_eq!(input, before, "the pipeline must not mutate its input");
}

// --- Engine-managed subscriptions -------------------------------------------

#[test]
fn an_engine_managed_subscription_is_declared_but_not_merged() {
    let mut input = base_input();
    input
        .engine_managed
        .push(crate::domain::EngineManagedSubscription {
            name: "remote".to_string(),
            url: "https://example.invalid/sub?token=secret".to_string(),
            interval_secs: 3600,
            proxy: None,
        });

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert_eq!(config.subscriptions.len(), 1);
    assert_eq!(config.subscriptions[0].name, "remote");
    assert!(
        config.proxies.is_empty(),
        "an engine-managed subscription contributes no nodes to the app's pipeline"
    );
}

// --- P1a: LAN exposure is the user's decision alone ---------------------------

#[test]
fn a_subscription_cannot_expose_the_proxy_when_the_user_never_chose() {
    let mut input = base_input();
    assert_eq!(input.preferences.allow_lan, None);
    let mut sub = payload("s1", Vec::new(), Vec::new());
    sub.settings.allow_lan = Some(true);
    sub.settings.bind_address = Some("0.0.0.0".to_string());
    input.subscriptions.push(sub);

    let outcome = build(&input);
    let config = outcome.config.expect("the build should succeed");
    assert!(
        !config.allow_lan,
        "an unset preference must fall back to the safe default, not to the subscription"
    );
    assert_ne!(
        config.bind_address.as_deref(),
        Some("0.0.0.0"),
        "a subscription must not choose the listen address"
    );
}
