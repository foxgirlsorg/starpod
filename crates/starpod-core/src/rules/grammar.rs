//! The rule grammar, mirrored from the meow engine.
//!
//! Every token in [`RuleKind`] corresponds to a token meow's parser accepts.
//! The mirror lives in exactly one file so it is easy to re-verify against the
//! engine when meow is upgraded.
//!
//! Engine sources this table was derived from:
//! - `meow-common-0.29.0/src/rule.rs` — `RuleType::as_str`
//! - `meow-rules-0.29.0/src/parser.rs:95-240` — the token dispatch
//! - `meow-config-0.29.0/src/rule_parser.rs:104-177` — `RULE-SET` / `SUB-RULE`
//!
//! The engine's own `-t` flag validates any candidate configuration this
//! crate produces, so a divergence between this table and the engine is caught
//! before a configuration is ever applied — never at runtime.

use serde::{Deserialize, Serialize};

/// The kind of a rule — the first field of a rule line.
///
/// The variants map one-to-one onto meow's rule tokens. There is deliberately
/// no "unknown" variant: a rule kind that the engine does not support cannot
/// be constructed in the first place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuleKind {
    Domain,
    DomainSuffix,
    DomainKeyword,
    DomainRegex,
    DomainWildcard,
    GeoSite,
    GeoIp,
    SrcGeoIp,
    IpCidr,
    SrcIpCidr,
    IpSuffix,
    SrcIpSuffix,
    IpAsn,
    SrcIpAsn,
    SrcPort,
    DstPort,
    InPort,
    Network,
    Dscp,
    ProcessName,
    ProcessPath,
    Uid,
    InName,
    InType,
    InUser,
    RuleSet,
    SubRule,
    And,
    Or,
    Not,
    Match,
}

impl RuleKind {
    /// The token meow's parser expects for this kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Domain => "DOMAIN",
            Self::DomainSuffix => "DOMAIN-SUFFIX",
            Self::DomainKeyword => "DOMAIN-KEYWORD",
            Self::DomainRegex => "DOMAIN-REGEX",
            Self::DomainWildcard => "DOMAIN-WILDCARD",
            Self::GeoSite => "GEOSITE",
            Self::GeoIp => "GEOIP",
            Self::SrcGeoIp => "SRC-GEOIP",
            Self::IpCidr => "IP-CIDR",
            Self::SrcIpCidr => "SRC-IP-CIDR",
            Self::IpSuffix => "IP-SUFFIX",
            Self::SrcIpSuffix => "SRC-IP-SUFFIX",
            Self::IpAsn => "IP-ASN",
            Self::SrcIpAsn => "SRC-IP-ASN",
            Self::SrcPort => "SRC-PORT",
            Self::DstPort => "DST-PORT",
            Self::InPort => "IN-PORT",
            Self::Network => "NETWORK",
            Self::Dscp => "DSCP",
            Self::ProcessName => "PROCESS-NAME",
            Self::ProcessPath => "PROCESS-PATH",
            Self::Uid => "UID",
            Self::InName => "IN-NAME",
            Self::InType => "IN-TYPE",
            Self::InUser => "IN-USER",
            Self::RuleSet => "RULE-SET",
            Self::SubRule => "SUB-RULE",
            Self::And => "AND",
            Self::Or => "OR",
            Self::Not => "NOT",
            Self::Match => "MATCH",
        }
    }

    /// Parse a token back into a kind.
    ///
    /// The engine compares the rule type **verbatim** — only `AND`, `OR` and
    /// `NOT` are matched case-insensitively
    /// (`meow-rules-0.29.0/src/parser.rs:106-111`), and `MATCH` is compared
    /// exactly as well. This mirrors that: the logic tokens accept any
    /// casing, everything else must be spelled exactly as the engine spells
    /// it.
    pub fn from_token(token: &str) -> Option<Self> {
        let trimmed = token.trim();
        // The logic tokens are the only ones the engine upper-cases first.
        let upper = trimmed.to_ascii_uppercase();
        if matches!(upper.as_str(), "AND" | "OR" | "NOT") {
            return Some(match upper.as_str() {
                "AND" => Self::And,
                "OR" => Self::Or,
                _ => Self::Not,
            });
        }
        Some(match trimmed {
            "DOMAIN" => Self::Domain,
            "DOMAIN-SUFFIX" => Self::DomainSuffix,
            "DOMAIN-KEYWORD" => Self::DomainKeyword,
            "DOMAIN-REGEX" => Self::DomainRegex,
            "DOMAIN-WILDCARD" => Self::DomainWildcard,
            "GEOSITE" => Self::GeoSite,
            "GEOIP" => Self::GeoIp,
            "SRC-GEOIP" => Self::SrcGeoIp,
            "IP-CIDR" | "IP-CIDR6" => Self::IpCidr,
            "SRC-IP-CIDR" => Self::SrcIpCidr,
            "IP-SUFFIX" => Self::IpSuffix,
            "SRC-IP-SUFFIX" => Self::SrcIpSuffix,
            "IP-ASN" => Self::IpAsn,
            "SRC-IP-ASN" => Self::SrcIpAsn,
            "SRC-PORT" => Self::SrcPort,
            "DST-PORT" => Self::DstPort,
            "IN-PORT" => Self::InPort,
            "NETWORK" => Self::Network,
            "DSCP" => Self::Dscp,
            "PROCESS-NAME" => Self::ProcessName,
            "PROCESS-PATH" => Self::ProcessPath,
            "UID" => Self::Uid,
            "IN-NAME" => Self::InName,
            "IN-TYPE" => Self::InType,
            "IN-USER" => Self::InUser,
            "RULE-SET" => Self::RuleSet,
            "SUB-RULE" => Self::SubRule,
            "MATCH" => Self::Match,
            _ => return None,
        })
    }

    /// Every kind, in the order they are offered in the structured editor.
    pub const ALL: &'static [Self] = &[
        Self::Domain,
        Self::DomainSuffix,
        Self::DomainKeyword,
        Self::DomainRegex,
        Self::DomainWildcard,
        Self::GeoSite,
        Self::GeoIp,
        Self::SrcGeoIp,
        Self::IpCidr,
        Self::SrcIpCidr,
        Self::IpSuffix,
        Self::SrcIpSuffix,
        Self::IpAsn,
        Self::SrcIpAsn,
        Self::SrcPort,
        Self::DstPort,
        Self::InPort,
        Self::Network,
        Self::Dscp,
        Self::ProcessName,
        Self::ProcessPath,
        Self::Uid,
        Self::InName,
        Self::InType,
        Self::InUser,
        Self::RuleSet,
        Self::SubRule,
        Self::And,
        Self::Or,
        Self::Not,
        Self::Match,
    ];

    /// Whether this kind takes a payload in its second field.
    ///
    /// `MATCH` has no payload — it is `MATCH,<target>`, two fields — while
    /// every other kind is `TYPE,payload,target[,flags]`.
    pub const fn has_payload(self) -> bool {
        !matches!(self, Self::Match)
    }

    /// Whether this kind takes a target.
    ///
    /// The logic rules carry their target *after* the parenthesized payload
    /// group, and `MATCH` carries it directly — both have a target, just not
    /// in the second field.
    pub const fn has_target(self) -> bool {
        true
    }

    /// Whether this kind's payload is a nested list of sub-rules.
    pub const fn is_logic(self) -> bool {
        matches!(self, Self::And | Self::Or | Self::Not)
    }

    /// Whether this kind's payload is a rule-provider name.
    pub const fn references_provider(self) -> bool {
        matches!(self, Self::RuleSet | Self::SubRule)
    }

    /// Whether this kind needs a geo database that the engine downloads on
    /// first use.
    ///
    /// `GEOIP` and `SRC-GEOIP` hard-error when the GeoIP database is absent;
    /// `IP-ASN` and `SRC-IP-ASN` hard-error when the ASN database is absent.
    /// `GEOSITE` is deliberately *not* listed — the engine tolerates an
    /// absent geosite database (the rule parses and simply never matches).
    pub const fn needs_geodata(self) -> bool {
        matches!(
            self,
            Self::GeoIp | Self::SrcGeoIp | Self::IpAsn | Self::SrcIpAsn
        )
    }

    /// The geodata database this kind needs, if any.
    pub const fn geodata_kind(self) -> Option<GeoDataKind> {
        match self {
            Self::GeoIp | Self::SrcGeoIp => Some(GeoDataKind::GeoIp),
            Self::IpAsn | Self::SrcIpAsn => Some(GeoDataKind::Asn),
            _ => None,
        }
    }

    /// Whether this kind's payload is a rule-provider name whose `behavior`
    /// supplies the match semantics.
    pub const fn is_rule_set(self) -> bool {
        matches!(self, Self::RuleSet)
    }

    /// Whether this kind may carry the `src` flag.
    ///
    /// The engine only consults the flag field for `IP-CIDR`, `IP-SUFFIX`,
    /// `GEOIP`, `IP-ASN` and `RULE-SET`; the `SRC-*` variants already imply
    /// it, and the domain rules ignore the field entirely.
    pub const fn accepts_src_flag(self) -> bool {
        matches!(
            self,
            Self::IpCidr | Self::IpSuffix | Self::GeoIp | Self::IpAsn | Self::RuleSet
        )
    }

    /// Whether this kind may carry the `no-resolve` flag.
    ///
    /// `RULE-SET` consults it too, but only together with the provider's own
    /// behavior (`RuleSetRule::should_resolve_ip`), so it is not listed
    /// here — see [`RuleKind::accepts_src_flag`] for the flag-bearing kinds.
    pub const fn accepts_no_resolve_flag(self) -> bool {
        matches!(
            self,
            Self::IpCidr | Self::IpSuffix | Self::GeoIp | Self::IpAsn
        )
    }
}

/// Which geodata database a rule kind requires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GeoDataKind {
    /// The GeoIP country database (`GeoLite2-Country.mmdb`).
    GeoIp,
    /// The ASN database (`GeoLite2-ASN.mmdb`).
    Asn,
}

/// The optional flags a rule line may carry after the target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct RuleFlags {
    /// `no-resolve` — do not resolve the domain to an IP before matching.
    pub no_resolve: bool,
    /// `src` — match the source axis instead of the destination.
    ///
    /// The engine sets `no_resolve` whenever `src` is set
    /// (`flags.no_resolve |= flags.is_src`), so this type mirrors that.
    pub is_src: bool,
}

impl RuleFlags {
    /// Parse the trailing flag field the way the engine does.
    pub fn parse(extra: Option<&str>) -> Self {
        let mut flags = Self::default();
        if let Some(extra) = extra {
            for flag in extra.split(',').map(str::trim) {
                if flag.eq_ignore_ascii_case("no-resolve") {
                    flags.no_resolve = true;
                } else if flag.eq_ignore_ascii_case("src") {
                    flags.is_src = true;
                }
            }
        }
        flags.no_resolve |= flags.is_src;
        flags
    }

    /// The flag field as the engine would write it, or `None` when empty.
    pub fn to_suffix(self) -> Option<String> {
        let mut parts = Vec::new();
        if self.no_resolve {
            parts.push("no-resolve");
        }
        if self.is_src {
            parts.push("src");
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(","))
        }
    }
}

/// The built-in outbounds the engine registers itself, which are always valid
/// rule targets.
///
/// Source: `meow-config-0.29.0/src/lib.rs:2332-2411`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BuiltinTarget {
    Direct,
    Reject,
    RejectDrop,
    Compatible,
    Pass,
    PassRule,
}

impl BuiltinTarget {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "DIRECT",
            Self::Reject => "REJECT",
            Self::RejectDrop => "REJECT-DROP",
            Self::Compatible => "COMPATIBLE",
            Self::Pass => "PASS",
            Self::PassRule => "PASS-RULE",
        }
    }

    pub fn from_str_exact(value: &str) -> Option<Self> {
        Some(match value {
            "DIRECT" => Self::Direct,
            "REJECT" => Self::Reject,
            "REJECT-DROP" => Self::RejectDrop,
            "COMPATIBLE" => Self::Compatible,
            "PASS" => Self::Pass,
            "PASS-RULE" => Self::PassRule,
            _ => return None,
        })
    }

    pub const ALL: &'static [Self] = &[
        Self::Direct,
        Self::Reject,
        Self::RejectDrop,
        Self::Compatible,
        Self::Pass,
        Self::PassRule,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_token() {
        for kind in RuleKind::ALL {
            assert_eq!(
                RuleKind::from_token(kind.as_str()),
                Some(*kind),
                "{} did not round-trip",
                kind.as_str()
            );
        }
    }

    #[test]
    fn the_token_table_is_complete_and_unique() {
        let mut tokens: Vec<&str> = RuleKind::ALL.iter().map(|k| k.as_str()).collect();
        tokens.sort_unstable();
        let count = tokens.len();
        tokens.dedup();
        assert_eq!(tokens.len(), count, "duplicate token in the grammar table");
    }

    #[test]
    fn ip_cidr6_is_an_alias_of_ip_cidr() {
        assert_eq!(RuleKind::from_token("IP-CIDR6"), Some(RuleKind::IpCidr));
    }

    #[test]
    fn the_logic_tokens_are_case_insensitive() {
        assert_eq!(RuleKind::from_token("and"), Some(RuleKind::And));
        assert_eq!(RuleKind::from_token("Not"), Some(RuleKind::Not));
    }

    #[test]
    fn unknown_tokens_are_rejected() {
        assert_eq!(RuleKind::from_token("DOMAIN-FOO"), None);
        assert_eq!(RuleKind::from_token(""), None);
    }

    #[test]
    fn src_implies_no_resolve_exactly_as_the_engine_does() {
        let flags = RuleFlags::parse(Some("src"));
        assert!(flags.is_src);
        assert!(
            flags.no_resolve,
            "the engine sets no_resolve when src is set"
        );

        let flags = RuleFlags::parse(Some("no-resolve"));
        assert!(flags.no_resolve);
        assert!(!flags.is_src);

        let flags = RuleFlags::parse(Some("no-resolve,src"));
        assert!(flags.no_resolve);
        assert!(flags.is_src);
    }

    #[test]
    fn flags_parse_ignores_unknown_entries_like_the_engine() {
        let flags = RuleFlags::parse(Some("bogus,src"));
        assert!(flags.is_src);
        assert!(flags.no_resolve);
    }

    #[test]
    fn an_empty_flag_field_produces_no_flags() {
        assert_eq!(RuleFlags::parse(Some("")).to_suffix(), None);
        assert_eq!(RuleFlags::parse(None).to_suffix(), None);
    }

    #[test]
    fn builtin_targets_round_trip() {
        for target in BuiltinTarget::ALL {
            assert_eq!(
                BuiltinTarget::from_str_exact(target.as_str()),
                Some(*target)
            );
        }
        assert_eq!(BuiltinTarget::from_str_exact("direct"), None);
    }
}
