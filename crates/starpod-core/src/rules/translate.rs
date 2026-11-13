//! Translating the user-facing rule model into the engine's raw rule lines.
//!
//! The engine reads a flat list of strings from the `rules:` block. Each
//! string is one rule, in one of four shapes:
//!
//! | shape        | form                                       |
//! |--------------|--------------------------------------------|
//! | simple       | `TYPE,payload,target[,flags]`              |
//! | catch-all    | `MATCH,target`                             |
//! | logic        | `AND\|OR\|NOT,((sub),(sub)),target`        |
//! | provider     | `RULE-SET,provider,target[,flags]`         |
//! | named block  | `SUB-RULE,block`                           |
//!
//! **Inner rules carry no target.** Inside a logic group the engine splices
//! a placeholder adapter into every entry and discards it, and reads the
//! third comma-separated field of an inner entry as the *flags* field. The
//! enclosing logic rule owns the target. This module emits inner rules in
//! exactly that shape — `DOMAIN,a.com` and `DOMAIN,a.com,no-resolve`, never
//! `DOMAIN,a.com,DIRECT`.
//!
//! Verification of each form is in `docs/research/ARCHITECTURE.md` §1.5 and
//! the engine sources cited there. This module is the only place in the app
//! that writes engine rule syntax.

use super::grammar::RuleFlags;
use super::model::{InnerRule, LogicKind, RuleSpec, RuleTarget};

/// Translate one top-level rule into its engine line.
pub fn translate(rule: &RuleSpec) -> String {
    match rule {
        RuleSpec::Simple {
            kind,
            payload,
            target,
            flags,
        } => {
            let mut line = format!("{},{},{}", kind.as_str(), payload, target.as_str());
            if let Some(suffix) = flags.to_suffix() {
                line.push(',');
                line.push_str(&suffix);
            }
            line
        }
        RuleSpec::Match { target } => format!("MATCH,{}", target.as_str()),
        RuleSpec::Logic {
            kind,
            sub_rules,
            target,
        } => {
            let mut line = String::from(kind.as_str());
            line.push(',');
            translate_group(sub_rules, &mut line);
            line.push(',');
            line.push_str(target.as_str());
            line
        }
        RuleSpec::RuleSet {
            provider,
            target,
            flags,
        } => {
            let mut line = format!("RULE-SET,{provider},{}", target.as_str());
            if let Some(suffix) = flags.to_suffix() {
                line.push(',');
                line.push_str(&suffix);
            }
            line
        }
        RuleSpec::SubRule { block } => format!("SUB-RULE,{block}"),
    }
}

/// Translate an inner rule — one that has no target of its own.
pub fn translate_inner(rule: &InnerRule) -> String {
    match rule {
        InnerRule::Simple {
            kind,
            payload,
            flags,
        } => {
            let mut line = format!("{},{}", kind.as_str(), payload);
            if let Some(suffix) = flags.to_suffix() {
                line.push(',');
                line.push_str(&suffix);
            }
            line
        }
        InnerRule::Logic { kind, sub_rules } => {
            let mut line = String::from(kind.as_str());
            line.push(',');
            translate_group(sub_rules, &mut line);
            line
        }
    }
}

/// Write a logic group's parenthesised body: `((sub),(sub))`.
fn translate_group(sub_rules: &[InnerRule], out: &mut String) {
    out.push('(');
    for (i, sub) in sub_rules.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('(');
        out.push_str(&translate_inner(sub));
        out.push(')');
    }
    out.push(')');
}

/// Translate a whole rule list.
///
/// The engine reads `rules:` as an ordered list; order *is* the routing
/// priority, so the list is translated in the order given — never sorted,
/// never reordered.
pub fn translate_all(rules: &[RuleSpec]) -> Vec<String> {
    rules.iter().map(translate).collect()
}

/// Whether the given rule list has rules after the catch-all.
///
/// The engine's `MATCH` rule always matches, so any rule after it is
/// unreachable. The app warns about this rather than silently dropping the
/// dead rules.
pub fn has_unreachable_tail(rules: &[RuleSpec]) -> bool {
    match rules.iter().position(RuleSpec::is_match) {
        Some(index) => index + 1 < rules.len(),
        None => false,
    }
}

/// The index of the catch-all rule, if there is one.
pub fn match_index(rules: &[RuleSpec]) -> Option<usize> {
    rules.iter().position(RuleSpec::is_match)
}

/// A rule the app considers dead — one that can never match.
///
/// The engine performs the same check on `DOMAIN`, `DOMAIN-SUFFIX`,
/// `DOMAIN-WILDCARD` and `DOMAIN-KEYWORD` payloads and warns about them
/// (or fails the load in strict mode). The app performs the check itself so
/// it can point at the problem in its own UI, and so the rule survives in the
/// user's model — the app never silently rewrites what the user wrote.
pub fn dead_payload_reason(rule: &RuleSpec) -> Option<&'static str> {
    let RuleSpec::Simple { kind, payload, .. } = rule else {
        return None;
    };
    dead_payload_for(*kind, payload)
}

/// The same check, for a rule inside a logic group.
pub fn dead_inner_payload_reason(rule: &InnerRule) -> Option<&'static str> {
    match rule {
        InnerRule::Simple { kind, payload, .. } => dead_payload_for(*kind, payload),
        InnerRule::Logic { .. } => None,
    }
}

fn dead_payload_for(kind: super::grammar::RuleKind, payload: &str) -> Option<&'static str> {
    use super::grammar::RuleKind;
    let payload = payload.trim();
    match kind {
        RuleKind::Domain | RuleKind::DomainSuffix => {
            if payload.is_empty() {
                Some("the payload is empty")
            } else if payload.starts_with('.') || payload.ends_with('.') {
                Some("a leading or trailing dot can never match a normalised host")
            } else if payload.contains(['*', '+']) {
                Some("a `*` or `+` belongs on DOMAIN-WILDCARD, not on this rule type")
            } else if payload.contains("..") {
                Some("an empty label can never match a normalised host")
            } else {
                None
            }
        }
        RuleKind::DomainWildcard => {
            if payload.is_empty() {
                Some("the payload is empty")
            } else if payload.contains('+') {
                Some("the wildcard matcher globs `*` only — `+` is a literal byte no host carries")
            } else {
                let body = payload.strip_prefix("*.").unwrap_or(payload);
                if body.is_empty() || body.split('.').any(str::is_empty) {
                    Some("an empty label can never match a normalised host")
                } else {
                    None
                }
            }
        }
        RuleKind::DomainKeyword => {
            if payload.contains("..") {
                Some("an empty label can never match a normalised host")
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The logic kind of a rule, if it is a logic rule.
pub fn logic_kind(rule: &RuleSpec) -> Option<LogicKind> {
    match rule {
        RuleSpec::Logic { kind, .. } => Some(*kind),
        _ => None,
    }
}

/// The flags of a top-level rule, defaulting to none.
pub fn flags_of(rule: &RuleSpec) -> RuleFlags {
    match rule {
        RuleSpec::Simple { flags, .. } | RuleSpec::RuleSet { flags, .. } => *flags,
        _ => RuleFlags::default(),
    }
}

/// The flags of an inner rule, defaulting to none.
pub fn inner_flags_of(rule: &InnerRule) -> RuleFlags {
    match rule {
        InnerRule::Simple { flags, .. } => *flags,
        InnerRule::Logic { .. } => RuleFlags::default(),
    }
}

/// Whether the target of a rule is a built-in outbound.
pub fn target_is_builtin(target: &RuleTarget) -> bool {
    target.builtin_kind().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::grammar::{BuiltinTarget, RuleKind};

    fn simple(kind: RuleKind, payload: &str, target: &str) -> RuleSpec {
        RuleSpec::Simple {
            kind,
            payload: payload.to_string(),
            target: RuleTarget::new(target),
            flags: RuleFlags::default(),
        }
    }

    fn inner(kind: RuleKind, payload: &str) -> InnerRule {
        InnerRule::Simple {
            kind,
            payload: payload.to_string(),
            flags: RuleFlags::default(),
        }
    }

    #[test]
    fn a_simple_rule_translates_to_three_fields() {
        let rule = simple(RuleKind::Domain, "example.com", "Proxy");
        assert_eq!(translate(&rule), "DOMAIN,example.com,Proxy");
    }

    #[test]
    fn flags_are_appended_after_the_target() {
        let rule = RuleSpec::Simple {
            kind: RuleKind::IpCidr,
            payload: "192.0.2.0/24".to_string(),
            target: RuleTarget::new("DIRECT"),
            flags: RuleFlags {
                no_resolve: true,
                is_src: false,
            },
        };
        assert_eq!(translate(&rule), "IP-CIDR,192.0.2.0/24,DIRECT,no-resolve");
    }

    #[test]
    fn a_match_rule_translates_to_two_fields() {
        let rule = RuleSpec::Match {
            target: RuleTarget::builtin(BuiltinTarget::Direct),
        };
        assert_eq!(translate(&rule), "MATCH,DIRECT");
    }

    #[test]
    fn a_rule_set_rule_carries_the_provider_and_target() {
        let rule = RuleSpec::RuleSet {
            provider: "my-set".to_string(),
            target: RuleTarget::new("Proxy"),
            flags: RuleFlags::default(),
        };
        assert_eq!(translate(&rule), "RULE-SET,my-set,Proxy");
    }

    #[test]
    fn a_logic_rule_translates_to_the_parenthesised_group_form() {
        let rule = RuleSpec::Logic {
            kind: LogicKind::And,
            sub_rules: vec![
                inner(RuleKind::DomainSuffix, "example.com"),
                inner(RuleKind::DstPort, "443"),
            ],
            target: RuleTarget::new("Proxy"),
        };
        assert_eq!(
            translate(&rule),
            "AND,((DOMAIN-SUFFIX,example.com),(DST-PORT,443)),Proxy"
        );
    }

    #[test]
    fn an_inner_rule_carries_no_target() {
        // The engine splices a placeholder adapter into inner entries and
        // reads the third field as the *flags* field. Emitting a target
        // there would make the flag silently become a target.
        assert_eq!(
            translate_inner(&inner(RuleKind::Domain, "a.com")),
            "DOMAIN,a.com"
        );
    }

    #[test]
    fn an_inner_rule_carries_its_flags_in_the_third_field() {
        let rule = InnerRule::Simple {
            kind: RuleKind::IpCidr,
            payload: "192.0.2.0/24".to_string(),
            flags: RuleFlags {
                no_resolve: true,
                is_src: false,
            },
        };
        assert_eq!(translate_inner(&rule), "IP-CIDR,192.0.2.0/24,no-resolve");
    }

    #[test]
    fn a_nested_logic_rule_translates_recursively() {
        let inner_group = InnerRule::Logic {
            kind: LogicKind::Or,
            sub_rules: vec![inner(RuleKind::Domain, "a.com")],
        };
        let outer = RuleSpec::Logic {
            kind: LogicKind::And,
            sub_rules: vec![inner_group, inner(RuleKind::DstPort, "443")],
            target: RuleTarget::new("Proxy"),
        };
        assert_eq!(
            translate(&outer),
            "AND,((OR,((DOMAIN,a.com))),(DST-PORT,443)),Proxy"
        );
    }

    #[test]
    fn a_sub_rule_rule_translates_to_a_single_field_reference() {
        let rule = RuleSpec::SubRule {
            block: "my-block".to_string(),
        };
        assert_eq!(translate(&rule), "SUB-RULE,my-block");
    }

    #[test]
    fn the_rule_order_is_preserved_exactly() {
        let rules = vec![
            simple(RuleKind::Domain, "a.com", "DIRECT"),
            simple(RuleKind::Domain, "b.com", "DIRECT"),
            RuleSpec::Match {
                target: RuleTarget::new("Proxy"),
            },
        ];
        let lines = translate_all(&rules);
        assert_eq!(
            lines,
            vec!["DOMAIN,a.com,DIRECT", "DOMAIN,b.com,DIRECT", "MATCH,Proxy"]
        );
    }

    #[test]
    fn a_match_rule_makes_everything_after_it_unreachable() {
        let rules = vec![
            simple(RuleKind::Domain, "a.com", "DIRECT"),
            RuleSpec::Match {
                target: RuleTarget::new("Proxy"),
            },
            simple(RuleKind::Domain, "b.com", "DIRECT"),
        ];
        assert!(has_unreachable_tail(&rules));
        assert_eq!(match_index(&rules), Some(1));
    }

    #[test]
    fn a_list_ending_in_match_has_no_unreachable_tail() {
        let rules = vec![
            simple(RuleKind::Domain, "a.com", "DIRECT"),
            RuleSpec::Match {
                target: RuleTarget::new("Proxy"),
            },
        ];
        assert!(!has_unreachable_tail(&rules));
    }

    #[test]
    fn a_list_without_a_match_has_no_unreachable_tail() {
        let rules = vec![simple(RuleKind::Domain, "a.com", "DIRECT")];
        assert!(!has_unreachable_tail(&rules));
        assert_eq!(match_index(&rules), None);
    }

    #[test]
    fn dead_domain_payloads_are_detected() {
        assert!(dead_payload_reason(&simple(RuleKind::Domain, "", "DIRECT")).is_some());
        assert!(dead_payload_reason(&simple(RuleKind::Domain, ".x.com", "DIRECT")).is_some());
        assert!(dead_payload_reason(&simple(RuleKind::Domain, "x.com.", "DIRECT")).is_some());
        assert!(dead_payload_reason(&simple(RuleKind::Domain, "*.x.com", "DIRECT")).is_some());
        assert!(dead_payload_reason(&simple(RuleKind::Domain, "a..b", "DIRECT")).is_some());
        assert!(dead_payload_reason(&simple(RuleKind::Domain, "x.com", "DIRECT")).is_none());
    }

    #[test]
    fn a_wildcard_rule_accepts_a_star_but_not_a_plus() {
        assert!(
            dead_payload_reason(&simple(RuleKind::DomainWildcard, "*.x.com", "DIRECT")).is_none()
        );
        assert!(
            dead_payload_reason(&simple(RuleKind::DomainWildcard, "+.x.com", "DIRECT")).is_some()
        );
        assert!(dead_payload_reason(&simple(RuleKind::DomainWildcard, "a..b", "DIRECT")).is_some());
    }

    #[test]
    fn a_keyword_rule_only_dies_on_an_empty_label() {
        assert!(dead_payload_reason(&simple(RuleKind::DomainKeyword, "foo.", "DIRECT")).is_none());
        assert!(dead_payload_reason(&simple(RuleKind::DomainKeyword, "a..b", "DIRECT")).is_some());
    }

    #[test]
    fn other_rule_types_are_never_dead_by_payload() {
        assert!(dead_payload_reason(&simple(RuleKind::IpCidr, "not-an-ip", "DIRECT")).is_none());
        assert!(dead_payload_reason(&simple(RuleKind::DomainRegex, "[", "DIRECT")).is_none());
    }

    #[test]
    fn an_inner_dead_payload_is_detected_too() {
        assert!(dead_inner_payload_reason(&inner(RuleKind::Domain, "*.x.com")).is_some());
        assert!(dead_inner_payload_reason(&inner(RuleKind::Domain, "x.com")).is_none());
    }
}
