//! Re-asserting the rule order that an engine refresh inverts (ARCHITECTURE
//! §4.6, Q9).
//!
//! When meow applies an engine-managed subscription it puts that
//! subscription's rules *ahead* of the local rules. This module holds the
//! pure part of the fix: given the rules the user wants, and the rules the
//! engine currently has, it computes the order the engine should be given.
//! It does no I/O. The app decides when to call it and sends the result with
//! `replace_rules`.
//!
//! # Rules
//!
//! The desired order is:
//!
//! 1. the user's own rules, in the user's order (every occurrence, so a rule
//!    the user wrote twice stays twice);
//! 2. the rules the engine holds that the user did not write, in the
//!    engine's order (these are the subscription's);
//! 3. the `MATCH` rules, last. The user's `MATCH` rules come before any
//!    subscription `MATCH`, so the terminal rule is always the last line.
//!
//! "The user wrote it" is decided by exact text, as a multiset: if the user
//! wrote `X` once and the engine holds `X` twice, one copy is the user's and
//! the other belongs to the subscription.

use std::collections::HashMap;

/// A rule of an engine-managed subscription, as the engine reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct EngineSubscription {
    /// The subscription's name, which is the app's label for it.
    pub name: String,
    /// When the engine last refreshed the subscription. The engine's own
    /// representation, compared for change and never interpreted.
    pub last_updated: Option<serde_json::Value>,
}

/// A proxy the engine has in its live configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveProxy {
    /// The proxy's name.
    pub name: String,
    /// The server address, when the engine reports one.
    pub server: Option<String>,
    /// The server port, when the engine reports one.
    pub port: Option<u16>,
    /// For a group, the member it is using right now (the engine's `now`).
    /// `None` for a node, and for a group the engine does not report on.
    pub now: Option<String>,
}

/// Whether a rule is a `MATCH` rule, the terminal catch-all.
///
/// The type keyword is compared case-insensitively, as the engine's parser
/// does, and surrounding space is ignored.
pub fn is_match_rule(rule: &str) -> bool {
    rule.split(',')
        .next()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("MATCH"))
}

/// The rule order the engine should have.
///
/// `user_rules` are the user's rules in the user's order, as the engine
/// spells them. `live_rules` is the engine's current list, in evaluation
/// order. The result is what to send to the engine. If it equals
/// `live_rules`, nothing needs to change.
pub fn desired_rule_order(user_rules: &[String], live_rules: &[String]) -> Vec<String> {
    // How many copies of each user rule are still unaccounted for in the
    // live list.
    let mut owned_by_user: HashMap<&str, usize> = HashMap::new();
    for rule in user_rules {
        *owned_by_user.entry(rule.as_str()).or_insert(0) += 1;
    }

    let mut subscription_rules: Vec<&str> = Vec::new();
    let mut subscription_match_rules: Vec<&str> = Vec::new();
    for rule in live_rules {
        match owned_by_user.get_mut(rule.as_str()) {
            Some(count) if *count > 0 => *count -= 1,
            _ if is_match_rule(rule) => subscription_match_rules.push(rule),
            _ => subscription_rules.push(rule),
        }
    }

    let (user_match_rules, user_plain_rules): (Vec<&String>, Vec<&String>) =
        user_rules.iter().partition(|rule| is_match_rule(rule));

    let mut desired: Vec<String> = Vec::with_capacity(live_rules.len().max(user_rules.len()));
    desired.extend(user_plain_rules.into_iter().cloned());
    desired.extend(subscription_rules.into_iter().map(str::to_string));
    desired.extend(user_match_rules.into_iter().cloned());
    desired.extend(subscription_match_rules.into_iter().map(str::to_string));
    desired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    /// Each case: (name, user rules, live rules, expected order).
    #[test]
    fn the_desired_order_table() {
        // (name, user rules, live rules, expected order)
        type Case = (&'static str, Vec<String>, Vec<String>, Vec<String>);
        let cases: Vec<Case> = vec![
            (
                "user rules go first in the user's order, subscription rules keep theirs, MATCH is last",
                rules(&["DOMAIN,a.com,DIRECT", "DOMAIN,b.com,PROXY"]),
                rules(&[
                    "DOMAIN,s1.com,PROXY",
                    "DOMAIN,a.com,DIRECT",
                    "DOMAIN,s2.com,PROXY",
                    "DOMAIN,b.com,PROXY",
                    "MATCH,PROXY",
                ]),
                rules(&[
                    "DOMAIN,a.com,DIRECT",
                    "DOMAIN,b.com,PROXY",
                    "DOMAIN,s1.com,PROXY",
                    "DOMAIN,s2.com,PROXY",
                    "MATCH,PROXY",
                ]),
            ),
            (
                "an order that already holds is returned unchanged",
                rules(&["DOMAIN,a.com,DIRECT"]),
                rules(&["DOMAIN,a.com,DIRECT", "DOMAIN,s.com,PROXY", "MATCH,PROXY"]),
                rules(&["DOMAIN,a.com,DIRECT", "DOMAIN,s.com,PROXY", "MATCH,PROXY"]),
            ),
            (
                "a user rule the engine has dropped is still asserted",
                rules(&["DOMAIN,a.com,DIRECT", "DOMAIN,gone.com,DIRECT"]),
                rules(&["DOMAIN,s.com,PROXY", "DOMAIN,a.com,DIRECT", "MATCH,PROXY"]),
                rules(&[
                    "DOMAIN,a.com,DIRECT",
                    "DOMAIN,gone.com,DIRECT",
                    "DOMAIN,s.com,PROXY",
                    "MATCH,PROXY",
                ]),
            ),
            (
                "a rule the user wrote twice stays twice",
                rules(&["DOMAIN,a.com,DIRECT", "DOMAIN,a.com,DIRECT"]),
                rules(&["DOMAIN,s.com,PROXY", "DOMAIN,a.com,DIRECT", "MATCH,PROXY"]),
                rules(&[
                    "DOMAIN,a.com,DIRECT",
                    "DOMAIN,a.com,DIRECT",
                    "DOMAIN,s.com,PROXY",
                    "MATCH,PROXY",
                ]),
            ),
            (
                "a subscription copy of a user rule is kept as a subscription rule",
                rules(&["DOMAIN,a.com,DIRECT"]),
                rules(&["DOMAIN,a.com,DIRECT", "DOMAIN,a.com,DIRECT", "MATCH,PROXY"]),
                rules(&["DOMAIN,a.com,DIRECT", "DOMAIN,a.com,DIRECT", "MATCH,PROXY"]),
            ),
            (
                "the user's MATCH comes last, and a subscription MATCH follows it",
                rules(&["MATCH,DIRECT"]),
                rules(&["MATCH,PROXY", "DOMAIN,a.com,DIRECT", "MATCH,DIRECT"]),
                rules(&["DOMAIN,a.com,DIRECT", "MATCH,DIRECT", "MATCH,PROXY"]),
            ),
            (
                "with no user rules the engine's rules keep their order, MATCH last",
                rules(&[]),
                rules(&["MATCH,PROXY", "DOMAIN,s.com,PROXY"]),
                rules(&["DOMAIN,s.com,PROXY", "MATCH,PROXY"]),
            ),
            (
                "with no engine rules the user's rules are all there is",
                rules(&["DOMAIN,a.com,DIRECT", "MATCH,PROXY"]),
                rules(&[]),
                rules(&["DOMAIN,a.com,DIRECT", "MATCH,PROXY"]),
            ),
            (
                "MATCH is recognised whatever its case and spacing",
                rules(&[]),
                rules(&["DOMAIN,s.com,PROXY", " match ,PROXY"]),
                rules(&["DOMAIN,s.com,PROXY", " match ,PROXY"]),
            ),
        ];

        for (name, user, live, expected) in cases {
            assert_eq!(desired_rule_order(&user, &live), expected, "case: {name}");
        }
    }

    #[test]
    fn a_no_op_is_detected_by_equality_with_the_live_list() {
        let user = rules(&["DOMAIN,a.com,DIRECT"]);
        let live = rules(&["DOMAIN,a.com,DIRECT", "DOMAIN,s.com,PROXY", "MATCH,PROXY"]);
        assert_eq!(desired_rule_order(&user, &live), live);
    }

    #[test]
    fn a_match_rule_is_found_by_its_keyword_only() {
        assert!(is_match_rule("MATCH,PROXY"));
        assert!(is_match_rule("match"));
        assert!(!is_match_rule("DOMAIN,MATCH.example,PROXY"));
        assert!(!is_match_rule("MATCHES,PROXY"));
    }
}
