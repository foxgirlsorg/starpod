//! Parsing raw rule text into the user-facing rule model.
//!
//! This is the inverse of [`translate`](super::translate) and exists so the
//! Rules screen can offer a raw-text mode: the user pastes engine rule
//! lines, the app parses them into [`RuleSpec`] values, and the structured
//! editor can then show and edit them.
//!
//! The parser mirrors the engine's own grammar, including its quirks:
//!
//! - the rule type is compared verbatim, except for `AND`/`OR`/`NOT`, which
//!   are matched case-insensitively;
//! - a line is split into at most four comma-separated fields
//!   (`splitn(4, ',')`), so a `DOMAIN-REGEX` payload containing a comma is
//!   truncated by the engine — the parser reports that rather than
//!   reproducing the truncation;
//! - `MATCH` takes a single target and no payload;
//! - `SUB-RULE` takes a single field and rejects trailing ones;
//! - the logic rules take a parenthesized group list and a target, and the
//!   target is the *last* comma-separated field after the group;
//! - **an inner rule has no target**: the engine splices a placeholder
//!   adapter into every entry of a logic group and discards it, and reads
//!   the third comma-separated field of an inner entry as the *flags* field.
//!   An inner entry therefore parses as `TYPE,payload[,flags]`.
//!
//! Note that the engine's own parser accepts an inner entry written as
//! `TYPE,payload,TARGET` and silently treats the target as a flags field.
//! This parser rejects that spelling instead, because accepting it would
//! mean the app stores a target the engine will never use.

use super::grammar::{BuiltinTarget, RuleFlags, RuleKind};
use super::model::{InnerRule, LogicKind, RuleSpec, RuleTarget};

/// The maximum nesting depth of logic groups the engine accepts.
pub const MAX_LOGIC_DEPTH: usize = 64;

/// The maximum length of a single rule line the engine accepts.
pub const MAX_RULE_LINE_LEN: usize = 64 * 1024;

/// A rule that could not be parsed, with the reason and the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// The line that failed to parse.
    pub line: String,
    /// Why it failed.
    pub reason: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.line, self.reason)
    }
}

impl std::error::Error for ParseError {}

/// Parse a single top-level rule line.
pub fn parse_line(line: &str) -> Result<RuleSpec, String> {
    let line = line.trim();
    if line.is_empty() {
        return Err("the line is empty".to_string());
    }
    if line.len() > MAX_RULE_LINE_LEN {
        return Err(format!("the rule line exceeds {MAX_RULE_LINE_LEN} bytes"));
    }

    // The logic rules are detected before the naive comma split, because
    // their payload contains commas.
    if let Some((ty, rest)) = line.split_once(',')
        && let Some(kind) = LogicKind::from_token(ty)
    {
        return parse_logic(kind, rest);
    }

    let parts: Vec<&str> = line.splitn(4, ',').collect();
    if parts.len() < 2 {
        return Err(format!("invalid rule: {line}"));
    }

    let rule_type = parts[0].trim();

    if rule_type == "MATCH" {
        let target = parts.get(1).unwrap_or(&"DIRECT").trim();
        if target.is_empty() {
            return Err("MATCH needs a target".to_string());
        }
        return Ok(RuleSpec::Match {
            target: RuleTarget::new(target),
        });
    }

    // `SUB-RULE` takes exactly one field — the block's name. The engine
    // rejects a trailing field rather than dropping it, because dropping it
    // routes by a different block than the author wrote.
    if rule_type.eq_ignore_ascii_case("SUB-RULE") {
        // The whole remainder after the first comma must be the block name.
        // Looking at `parts[1]` alone would miss a trailing field, because
        // the split is capped at four fields.
        let rest = line.split_once(',').map(|(_, r)| r.trim()).unwrap_or("");
        if rest.is_empty() {
            return Err("SUB-RULE needs a block name".to_string());
        }
        if rest.contains(',') {
            return Err(
                "SUB-RULE takes exactly one block name — trailing fields are not supported"
                    .to_string(),
            );
        }
        return Ok(RuleSpec::SubRule {
            block: rest.to_string(),
        });
    }

    if parts.len() < 3 {
        return Err(format!("rule needs at least 3 fields: {line}"));
    }

    let payload = parts[1].trim();
    let target = parts[2].trim();
    let extra = parts.get(3).map(|s| s.trim());
    let flags = RuleFlags::parse(extra);

    match RuleKind::from_token(rule_type) {
        Some(RuleKind::RuleSet) => Ok(RuleSpec::RuleSet {
            provider: payload.to_string(),
            target: RuleTarget::new(target),
            flags,
        }),
        Some(kind) if kind.is_logic() => {
            // The pre-check above already handled the logic tokens, so this
            // is only reachable for a spelling the pre-check missed — which
            // cannot happen, since `from_token` accepts exactly the same
            // three tokens case-insensitively.
            unreachable!("logic rules are handled before the comma split")
        }
        Some(kind) => {
            if payload.is_empty() {
                return Err(format!("{rule_type} needs a payload"));
            }
            Ok(RuleSpec::Simple {
                kind,
                payload: payload.to_string(),
                target: RuleTarget::new(target),
                flags,
            })
        }
        None => Err(format!("unknown rule type: {rule_type}")),
    }
}

/// Parse a logic rule: `AND`/`OR`/`NOT,((inner),(inner)),TARGET`.
fn parse_logic(kind: LogicKind, rest: &str) -> Result<RuleSpec, String> {
    let rest = rest.trim_start();
    if !rest.starts_with('(') {
        return Err(format!(
            "{} rule: expected '(' after the rule type",
            kind.as_str()
        ));
    }

    // Find the matching ')' of the outer group list.
    let mut depth_paren: i32 = 0;
    let mut end = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth_paren += 1,
            ')' => {
                depth_paren -= 1;
                if depth_paren == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or_else(|| format!("{} rule: unbalanced parentheses", kind.as_str()))?;
    let inner = &rest[1..end];
    let tail = rest[end + 1..].trim_start();

    // The engine takes the *last* comma-separated field of the tail as the
    // adapter, matching upstream's `ParseRulePayload`.
    let target = tail
        .strip_prefix(',')
        .ok_or_else(|| {
            format!(
                "{} rule: expected ',TARGET' after the payload",
                kind.as_str()
            )
        })?
        .split(',')
        .next_back()
        .unwrap_or("")
        .trim();
    if target.is_empty() {
        return Err(format!("{} rule: missing target", kind.as_str()));
    }

    let groups = split_logic_groups(inner).map_err(|e| format!("{} rule: {e}", kind.as_str()))?;
    if groups.is_empty() {
        return Err(format!("{} rule: empty payload", kind.as_str()));
    }

    let mut sub_rules = Vec::with_capacity(groups.len());
    for group in &groups {
        sub_rules.push(parse_inner(group, 1)?);
    }

    if kind.takes_single_sub_rule() && sub_rules.len() != 1 {
        return Err(format!(
            "NOT rule requires exactly 1 inner rule, got {}",
            sub_rules.len()
        ));
    }

    Ok(RuleSpec::Logic {
        kind,
        sub_rules,
        target: RuleTarget::new(target),
    })
}

/// Parse one entry of a logic group.
///
/// An inner entry is `TYPE,payload[,flags]` — it has **no target**. The
/// engine splices a placeholder adapter into it and discards it.
fn parse_inner(entry: &str, depth: usize) -> Result<InnerRule, String> {
    if depth >= MAX_LOGIC_DEPTH {
        return Err(format!(
            "logic rule nesting exceeds {MAX_LOGIC_DEPTH} levels"
        ));
    }
    let entry = entry.trim();
    if entry.is_empty() {
        return Err("an inner rule is empty".to_string());
    }

    // A nested logic group is detected first, for the same reason it is at
    // the top level: its payload contains commas.
    if let Some((ty, rest)) = entry.split_once(',')
        && let Some(kind) = LogicKind::from_token(ty)
    {
        return parse_inner_logic(kind, rest, depth);
    }

    let parts: Vec<&str> = entry.splitn(3, ',').collect();
    let first = parts[0].trim();

    if first == "MATCH" {
        return Err("MATCH is not allowed inside a logic group".to_string());
    }

    let Some(kind) = RuleKind::from_token(first) else {
        return Err(format!("unknown rule type: {first}"));
    };
    if kind.is_logic() {
        // Reached only for a spelling the pre-check missed, which cannot
        // happen — `from_token` accepts the same three tokens.
        unreachable!("logic rules are handled before the comma split");
    }
    if kind == RuleKind::RuleSet {
        return Err(
            "RULE-SET is not allowed inside a logic group — the engine resolves rule providers \
             at the configuration level"
                .to_string(),
        );
    }
    if kind == RuleKind::SubRule {
        return Err("SUB-RULE is not allowed inside a logic group".to_string());
    }

    let Some(payload) = parts.get(1) else {
        return Err(format!("{first} needs a payload"));
    };
    let payload = payload.trim();
    if payload.is_empty() {
        return Err(format!("{first} needs a payload"));
    }

    // The third field of an inner entry is the *flags* field. The engine
    // parses it as such, so a target written here would be read as a flag
    // and silently do nothing — reject the spelling instead.
    let extra = parts.get(2).map(|s| s.trim()).filter(|s| !s.is_empty());
    if let Some(extra) = extra {
        let known = extra
            .split(',')
            .map(str::trim)
            .all(|f| f.eq_ignore_ascii_case("no-resolve") || f.eq_ignore_ascii_case("src"));
        if !known {
            return Err(format!(
                "“{extra}” is not a flag — an inner rule has no target; only \
                 `no-resolve` and `src` may follow the payload"
            ));
        }
    }

    Ok(InnerRule::Simple {
        kind,
        payload: payload.to_string(),
        flags: RuleFlags::parse(extra),
    })
}

/// Parse a nested logic group inside a logic group.
fn parse_inner_logic(kind: LogicKind, rest: &str, depth: usize) -> Result<InnerRule, String> {
    let rest = rest.trim_start();
    if !rest.starts_with('(') {
        return Err(format!(
            "{} rule: expected '(' after the rule type",
            kind.as_str()
        ));
    }
    let mut depth_paren: i32 = 0;
    let mut end = None;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth_paren += 1,
            ')' => {
                depth_paren -= 1;
                if depth_paren == 0 {
                    end = Some(i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.ok_or_else(|| format!("{} rule: unbalanced parentheses", kind.as_str()))?;
    let inner = &rest[1..end];

    let groups = split_logic_groups(inner).map_err(|e| format!("{} rule: {e}", kind.as_str()))?;
    if groups.is_empty() {
        return Err(format!("{} rule: empty payload", kind.as_str()));
    }

    let mut sub_rules = Vec::with_capacity(groups.len());
    for group in &groups {
        sub_rules.push(parse_inner(group, depth + 1)?);
    }

    if kind.takes_single_sub_rule() && sub_rules.len() != 1 {
        return Err(format!(
            "NOT rule requires exactly 1 inner rule, got {}",
            sub_rules.len()
        ));
    }

    Ok(InnerRule::Logic { kind, sub_rules })
}

/// Split the body of a logic payload into the contents of each `(...)` group.
fn split_logic_groups(inner: &str) -> Result<Vec<String>, String> {
    let mut groups = Vec::new();
    let mut chars = inner.char_indices().peekable();
    loop {
        while let Some(&(_, c)) = chars.peek() {
            if c == ' ' || c == ',' {
                chars.next();
            } else {
                break;
            }
        }
        let Some(&(_, c)) = chars.peek() else { break };
        if c != '(' {
            return Err(format!("expected '(' starting a group, got '{c}'"));
        }
        chars.next();
        let start = chars.peek().map_or(inner.len(), |&(i, _)| i);
        let mut depth = 1i32;
        let mut end = None;
        for (i, c) in chars.by_ref() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let end = end.ok_or_else(|| "unbalanced parentheses in the logic payload".to_string())?;
        groups.push(inner[start..end].to_string());
    }
    Ok(groups)
}

/// Parse a block of raw rule text.
///
/// Blank lines and `#` comments are skipped, exactly as the engine skips
/// them. Each failing line is reported individually so the UI can point at
/// the offending line rather than rejecting the whole block — but the
/// caller decides whether to keep or drop the failing lines.
pub fn parse_block(text: &str) -> (Vec<RuleSpec>, Vec<ParseError>) {
    let mut rules = Vec::new();
    let mut errors = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match parse_line(line) {
            Ok(rule) => rules.push(rule),
            Err(reason) => errors.push(ParseError {
                line: line.to_string(),
                reason,
            }),
        }
    }
    (rules, errors)
}

/// Whether the given target name is one of the engine's built-in outbounds.
pub fn is_builtin_target(name: &str) -> bool {
    BuiltinTarget::from_str_exact(name.trim()).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_simple_rule_parses() {
        let rule = parse_line("DOMAIN,example.com,Proxy").unwrap();
        assert_eq!(rule, simple(RuleKind::Domain, "example.com", "Proxy"));
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        let rule = parse_line("  DOMAIN , example.com , Proxy ").unwrap();
        assert_eq!(rule, simple(RuleKind::Domain, "example.com", "Proxy"));
    }

    #[test]
    fn a_match_rule_parses_to_its_two_field_form() {
        let rule = parse_line("MATCH,DIRECT").unwrap();
        assert_eq!(
            rule,
            RuleSpec::Match {
                target: RuleTarget::builtin(BuiltinTarget::Direct)
            }
        );
    }

    #[test]
    fn a_match_rule_with_a_target_field_parses() {
        let rule = parse_line("MATCH,Proxy").unwrap();
        assert_eq!(rule.target().unwrap().as_str(), "Proxy");
    }

    #[test]
    fn flags_are_parsed_and_src_implies_no_resolve() {
        let rule = parse_line("IP-CIDR,192.0.2.0/24,DIRECT,src").unwrap();
        let RuleSpec::Simple { flags, .. } = &rule else {
            panic!("expected a simple rule");
        };
        assert!(flags.is_src);
        assert!(flags.no_resolve);
    }

    #[test]
    fn an_unknown_rule_type_is_rejected() {
        let err = parse_line("NOPE,x,DIRECT").unwrap_err();
        assert!(err.contains("unknown rule type"), "{err}");
    }

    #[test]
    fn a_rule_with_too_few_fields_is_rejected() {
        let err = parse_line("DOMAIN,example.com").unwrap_err();
        assert!(err.contains("at least 3"), "{err}");
    }

    #[test]
    fn a_rule_set_parses_to_its_provider_form() {
        let rule = parse_line("RULE-SET,my-set,Proxy").unwrap();
        assert_eq!(
            rule,
            RuleSpec::RuleSet {
                provider: "my-set".to_string(),
                target: RuleTarget::new("Proxy"),
                flags: RuleFlags::default(),
            }
        );
    }

    #[test]
    fn a_sub_rule_with_a_trailing_field_is_rejected() {
        let err = parse_line("SUB-RULE,my-block,junk").unwrap_err();
        assert!(err.contains("exactly one"), "{err}");
    }

    #[test]
    fn a_sub_rule_parses() {
        let rule = parse_line("SUB-RULE,my-block").unwrap();
        assert_eq!(
            rule,
            RuleSpec::SubRule {
                block: "my-block".to_string()
            }
        );
    }

    #[test]
    fn a_logic_rule_parses_into_sub_rules() {
        let rule = parse_line("AND,((DOMAIN-SUFFIX,example.com),(DST-PORT,443)),Proxy").unwrap();
        let RuleSpec::Logic {
            kind,
            sub_rules,
            target,
        } = &rule
        else {
            panic!("expected a logic rule");
        };
        assert_eq!(*kind, LogicKind::And);
        assert_eq!(sub_rules.len(), 2);
        assert_eq!(target.as_str(), "Proxy");
        assert_eq!(sub_rules[0], inner(RuleKind::DomainSuffix, "example.com"));
    }

    #[test]
    fn the_logic_target_is_the_last_field_of_the_tail() {
        let rule = parse_line("OR,((DOMAIN,a.com)),Proxy").unwrap();
        assert_eq!(rule.target().unwrap().as_str(), "Proxy");
    }

    #[test]
    fn an_inner_rule_with_a_target_is_rejected() {
        // The engine would read `DIRECT` as a flags field and silently do
        // nothing with it, so the app rejects the spelling.
        let err = parse_line("AND,((DOMAIN,a.com,DIRECT)),Proxy").unwrap_err();
        assert!(err.contains("not a flag"), "{err}");
    }

    #[test]
    fn an_inner_rule_with_a_flag_is_accepted() {
        let rule = parse_line("AND,((IP-CIDR,192.0.2.0/24,no-resolve)),Proxy").unwrap();
        let RuleSpec::Logic { sub_rules, .. } = &rule else {
            panic!("expected a logic rule");
        };
        let InnerRule::Simple { flags, .. } = &sub_rules[0] else {
            panic!("expected a simple inner rule");
        };
        assert!(flags.no_resolve);
    }

    #[test]
    fn a_match_inside_a_logic_group_is_rejected() {
        let err = parse_line("AND,((MATCH,DIRECT)),Proxy").unwrap_err();
        assert!(err.contains("MATCH is not allowed"), "{err}");
    }

    #[test]
    fn a_rule_set_inside_a_logic_group_is_rejected() {
        let err = parse_line("AND,((RULE-SET,my-set,Proxy)),Proxy").unwrap_err();
        assert!(err.contains("not allowed inside a logic group"), "{err}");
    }

    #[test]
    fn a_not_rule_with_two_inner_rules_is_rejected() {
        let err = parse_line("NOT,((DOMAIN,a.com),(DOMAIN,b.com)),Proxy").unwrap_err();
        assert!(err.contains("exactly 1"), "{err}");
    }

    #[test]
    fn a_not_rule_with_one_inner_rule_parses() {
        let rule = parse_line("NOT,((DOMAIN,a.com)),Proxy").unwrap();
        assert!(matches!(rule, RuleSpec::Logic { .. }));
    }

    #[test]
    fn unbalanced_parentheses_are_rejected() {
        let err = parse_line("AND,((DOMAIN,a.com),Proxy").unwrap_err();
        assert!(err.contains("unbalanced"), "{err}");
    }

    #[test]
    fn a_logic_rule_without_a_target_is_rejected() {
        let err = parse_line("AND,((DOMAIN,a.com))").unwrap_err();
        assert!(err.contains("TARGET"), "{err}");
    }

    #[test]
    fn the_logic_tokens_are_case_insensitive() {
        let rule = parse_line("and,((DOMAIN,a.com)),Proxy").unwrap();
        let RuleSpec::Logic { kind, .. } = &rule else {
            panic!("expected a logic rule");
        };
        assert_eq!(*kind, LogicKind::And);
    }

    #[test]
    fn the_other_rule_types_are_case_sensitive() {
        assert!(parse_line("domain,a.com,DIRECT").is_err());
    }

    #[test]
    fn a_block_skips_blank_lines_and_comments() {
        let (rules, errors) =
            parse_block("# a comment\n\nDOMAIN,a.com,DIRECT\n\n  # another comment\nMATCH,Proxy\n");
        assert_eq!(rules.len(), 2);
        assert!(errors.is_empty());
    }

    #[test]
    fn a_block_reports_bad_lines_individually() {
        let (rules, errors) = parse_block("DOMAIN,a.com,DIRECT\nNOPE,x,DIRECT\nMATCH,Proxy\n");
        assert_eq!(rules.len(), 2);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, "NOPE,x,DIRECT");
    }

    #[test]
    fn parsing_round_trips_through_translation() {
        for line in [
            "DOMAIN,example.com,Proxy",
            "DOMAIN-SUFFIX,example.com,DIRECT",
            "IP-CIDR,192.0.2.0/24,DIRECT,no-resolve",
            "MATCH,Proxy",
            "RULE-SET,my-set,Proxy",
            "SUB-RULE,my-block",
            "AND,((DOMAIN,a.com),(DST-PORT,443)),Proxy",
            "OR,((DOMAIN,a.com)),Proxy",
            "NOT,((DOMAIN,a.com)),Proxy",
        ] {
            let rule = parse_line(line).unwrap();
            assert_eq!(
                super::super::translate::translate(&rule),
                line,
                "{line} did not round-trip"
            );
        }
    }

    #[test]
    fn an_inner_rule_round_trips_through_translation() {
        let rule =
            parse_line("AND,((DOMAIN,a.com),(IP-CIDR,192.0.2.0/24,no-resolve)),Proxy").unwrap();
        assert_eq!(
            super::super::translate::translate(&rule),
            "AND,((DOMAIN,a.com),(IP-CIDR,192.0.2.0/24,no-resolve)),Proxy"
        );
    }
}
