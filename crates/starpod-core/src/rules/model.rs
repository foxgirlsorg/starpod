//! The user-facing rule model.
//!
//! This is the model the Rules screen edits. It is deliberately *not* the
//! engine's raw rule text: the structured editor manipulates a tree of
//! [`RuleSpec`] values, and only [`translate`](super::translate) turns them
//! into the flat `TYPE,payload,target[,flags]` lines the engine reads.
//!
//! # Why there are two rule types
//!
//! The engine has two different rule *shapes*, and they differ in a way the
//! user must not have to know about but the app must respect:
//!
//! - A **top-level** rule carries a target: `TYPE,payload,target[,flags]`.
//!   The target names the outbound the matching traffic is sent to.
//! - A rule **inside a logic group** carries no target. The engine splices a
//!   placeholder adapter into every inner entry and discards it
//!   (`meow-rules-0.29.0/src/parser.rs:445-472`, `splice_inner_adapter`), and
//!   the third comma-separated field of an inner entry is read as the *flags*
//!   field, not as a target. The enclosing `AND`/`OR`/`NOT` rule owns the
//!   target.
//!
//! Representing that with one type would mean a target on inner rules that
//! the engine silently ignores — a rule the user wrote that does not do what
//! it says. So there are two types: [`RuleSpec`] for top-level rules and
//! [`InnerRule`] for the members of a logic group.
//!
//! The separation also exists so that the engine's raw format never leaks
//! into the UI layer.

use serde::{Deserialize, Serialize};

use super::grammar::{BuiltinTarget, RuleFlags, RuleKind};

/// Where a rule sends matching traffic.
///
/// A target is either one of the engine's built-in outbounds or the name of a
/// proxy group or node in the generated configuration. The app cannot know
/// which names exist at the time a rule is authored, so the target is
/// validated against the *candidate* configuration, not at edit time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleTarget {
    name: String,
}

impl RuleTarget {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    pub fn builtin(target: BuiltinTarget) -> Self {
        Self {
            name: target.as_str().to_string(),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }

    /// Whether this target is one of the engine's built-in outbounds.
    pub fn builtin_kind(&self) -> Option<BuiltinTarget> {
        BuiltinTarget::from_str_exact(&self.name)
    }

    /// Whether this target is a plain name — a proxy group or a node.
    pub fn is_named(&self) -> bool {
        self.builtin_kind().is_none()
    }
}

impl From<BuiltinTarget> for RuleTarget {
    fn from(value: BuiltinTarget) -> Self {
        Self::builtin(value)
    }
}

impl std::fmt::Display for RuleTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

/// A top-level rule the user has written.
///
/// The variants mirror the *shapes* the engine accepts at the top level of
/// the `rules:` list, not its syntax.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "kebab-case")]
pub enum RuleSpec {
    /// `TYPE,payload,target[,flags]`
    Simple {
        kind: RuleKind,
        payload: String,
        target: RuleTarget,
        flags: RuleFlags,
    },
    /// `MATCH,target` — the catch-all tail of a rule list.
    Match { target: RuleTarget },
    /// `AND`/`OR`/`NOT` over a group of sub-rules.
    Logic {
        kind: LogicKind,
        sub_rules: Vec<InnerRule>,
        target: RuleTarget,
    },
    /// `RULE-SET,provider,target[,flags]`
    RuleSet {
        provider: String,
        target: RuleTarget,
        flags: RuleFlags,
    },
    /// `SUB-RULE,block` — a named block of rules defined elsewhere.
    SubRule { block: String },
}

/// A rule inside a logic group.
///
/// Unlike a [`RuleSpec`], an inner rule has **no target**: the enclosing
/// logic rule owns it. The engine enforces this by splicing a placeholder
/// adapter into every inner entry and discarding it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "kebab-case")]
pub enum InnerRule {
    /// `TYPE,payload[,flags]` — the third field is the *flags* field.
    Simple {
        kind: RuleKind,
        payload: String,
        flags: RuleFlags,
    },
    /// A nested `AND`/`OR`/`NOT` group.
    ///
    /// The engine drops a nested group's own trailing target (it takes the
    /// last comma-separated field of the tail, which is the placeholder it
    /// spliced in), so this variant has no target either.
    Logic {
        kind: LogicKind,
        sub_rules: Vec<InnerRule>,
    },
}

/// The three logic rule kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LogicKind {
    And,
    Or,
    Not,
}

impl LogicKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::And => "AND",
            Self::Or => "OR",
            Self::Not => "NOT",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token.trim().to_ascii_uppercase().as_str() {
            "AND" => Some(Self::And),
            "OR" => Some(Self::Or),
            "NOT" => Some(Self::Not),
            _ => None,
        }
    }

    /// Whether this logic kind takes exactly one sub-rule.
    ///
    /// The engine rejects a `NOT` with more or fewer than one inner rule
    /// (`meow-rules-0.29.0/src/parser.rs:381-390`).
    pub const fn takes_single_sub_rule(self) -> bool {
        matches!(self, Self::Not)
    }
}

impl RuleSpec {
    /// The target this rule sends matching traffic to, if it has one.
    ///
    /// A `SUB-RULE` has no target of its own: the block's own rules carry
    /// theirs.
    pub fn target(&self) -> Option<&RuleTarget> {
        match self {
            Self::Simple { target, .. }
            | Self::Match { target }
            | Self::Logic { target, .. }
            | Self::RuleSet { target, .. } => Some(target),
            Self::SubRule { .. } => None,
        }
    }

    /// Whether this rule is the catch-all at the end of a rule list.
    pub fn is_match(&self) -> bool {
        matches!(self, Self::Match { .. })
    }

    /// Whether this rule needs a geo database the engine must have.
    pub fn needs_geodata(&self) -> bool {
        match self {
            Self::Simple { kind, .. } => kind.needs_geodata(),
            Self::Logic { sub_rules, .. } => sub_rules.iter().any(InnerRule::needs_geodata),
            _ => false,
        }
    }

    /// The named providers this rule references.
    ///
    /// Only a top-level `RULE-SET` can name a provider — the engine resolves
    /// providers at the configuration level, not inside a logic group.
    pub fn provider_references(&self) -> Vec<&str> {
        match self {
            Self::RuleSet { provider, .. } => vec![provider],
            _ => Vec::new(),
        }
    }
}

impl InnerRule {
    /// Whether this rule needs a geo database the engine must have.
    pub fn needs_geodata(&self) -> bool {
        match self {
            Self::Simple { kind, .. } => kind.needs_geodata(),
            Self::Logic { sub_rules, .. } => sub_rules.iter().any(InnerRule::needs_geodata),
        }
    }

    /// The kind of this inner rule, for the structured editor's dropdown.
    pub fn kind(&self) -> RuleKind {
        match self {
            Self::Simple { kind, .. } => *kind,
            Self::Logic { kind, .. } => match kind {
                LogicKind::And => RuleKind::And,
                LogicKind::Or => RuleKind::Or,
                LogicKind::Not => RuleKind::Not,
            },
        }
    }
}
