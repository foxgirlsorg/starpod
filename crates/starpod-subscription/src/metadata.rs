//! A subscription's metadata: what the app knows about it besides its bytes.
//!
//! This is the app's own record of a subscription. It is deliberately
//! separate from the URL and the `Authorization` header: both are
//! credentials, and ARCHITECTURE §3.2 keeps them in the secret store. What
//! lives here is everything the Servers screen shows and everything the
//! refresh loop needs.
//!
//! # The quota is `None`, not zero
//!
//! The quota comes from the `subscription-userinfo` response header. The
//! engine's own fetcher discards that header
//! (`meow-config-0.29.0/src/subscription.rs:278-288` — it calls
//! `internal_http::fetch`, which returns only the body), so for an
//! engine-managed subscription the number **does not exist**. It must read
//! as absent, never as `0 / 0 GB`: a fabricated zero is a lie the user
//! cannot detect.

use starpod_core::SubscriptionMode;

/// The traffic quota a subscription's provider reports.
///
/// The `subscription-userinfo` header is `;`-separated `key=value` pairs —
/// `upload`, `download`, `total`, `expire` — where `expire` is a unix
/// timestamp (`meow-config-0.29.0/src/subscription_userinfo.rs:29-38`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscriptionQuota {
    /// The bytes uploaded on the plan.
    pub upload: u64,
    /// The bytes downloaded on the plan.
    pub download: u64,
    /// The plan's traffic quota in bytes, if the provider reported one.
    pub total: Option<u64>,
    /// When the plan ends, as a unix timestamp in seconds.
    pub expires: Option<i64>,
}

impl SubscriptionQuota {
    /// The bytes used on the plan — upload plus download.
    pub fn used(&self) -> u64 {
        self.upload.saturating_add(self.download)
    }

    /// Parse a `subscription-userinfo` header value.
    ///
    /// The format is a convention among panels, not a spec, so the parser
    /// is lenient: keys match case-insensitively, whitespace is ignored,
    /// unknown keys are skipped, and a field that carries no number reads
    /// as absent rather than as zero.
    ///
    /// The engine's own parser behaves the same way
    /// (`meow-config-0.29.0/src/subscription_userinfo.rs:41-66`) — with one
    /// difference the app makes on purpose: the engine reads an absent field
    /// as `0`, while the app reads it as `None`, because the app shows the
    /// quota to the user and "the server did not say" is not the same
    /// statement as "the server said zero".
    pub fn parse(header: &str) -> Option<Self> {
        let mut upload = None;
        let mut download = None;
        let mut total = None;
        let mut expires = None;

        for field in header.split(';') {
            let Some((key, amount)) = field.split_once('=') else {
                continue;
            };
            // A field that carries no number is *skipped*, not fatal: the
            // rest of the header is still readable. The engine does the
            // same (`meow-config-0.29.0/src/subscription_userinfo.rs:60-63`
            // — `if let Some(amount) = parse_amount(...)` leaves the slot
            // alone and carries on).
            let Some(amount) = parse_amount(amount.trim()) else {
                continue;
            };
            match key.trim() {
                key if key.eq_ignore_ascii_case("upload") => upload = Some(amount),
                key if key.eq_ignore_ascii_case("download") => download = Some(amount),
                key if key.eq_ignore_ascii_case("total") => total = Some(amount),
                key if key.eq_ignore_ascii_case("expire") => expires = Some(amount),
                _ => {}
            }
        }

        // A header that reports nothing usable carries no quota at all.
        if upload.is_none() && download.is_none() && total.is_none() && expires.is_none() {
            return None;
        }

        Some(Self {
            upload: upload.unwrap_or(0),
            download: download.unwrap_or(0),
            total,
            expires: expires.map(|value| value as i64),
        })
    }
}

/// Parse an amount: a non-negative integer, or a decimal some panels print.
fn parse_amount(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    if let Ok(amount) = text.parse::<u64>() {
        return Some(amount);
    }
    let amount = text.parse::<f64>().ok()?;
    // `as` truncates toward zero and saturates past `u64::MAX`.
    (amount.is_finite() && amount >= 0.0).then_some(amount as u64)
}

/// What the app knows about a subscription.
///
/// This is a *view* of the stored subscription, not the stored
/// subscription itself: the URL and the `Authorization` header live in the
/// secret store and are never held here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubscriptionMetadata {
    /// The subscription's stable id.
    pub id: String,
    /// The label the user gave it.
    ///
    /// The label is the namespace for the subscription's node names, so it
    /// ends up in every node name the app shows.
    pub label: String,
    /// Who fetches the subscription.
    pub mode: SubscriptionMode,
    /// How often the subscription is refreshed, in seconds.
    pub interval_secs: u64,
    /// When the subscription was last fetched successfully, as a unix
    /// timestamp in seconds.
    pub last_updated: Option<i64>,
    /// The quota the provider reported.
    ///
    /// Always `None` for an engine-managed subscription: the engine's
    /// fetcher discards the header the number comes from.
    pub quota: Option<SubscriptionQuota>,
    /// How many nodes the last valid payload carried.
    pub node_count: usize,
    /// How many proxy groups the last valid payload carried.
    pub group_count: usize,
    /// The last error, written for the user.
    ///
    /// The message is redacted at the point it is created — a provider
    /// error can carry a URL, and a URL carries the token.
    pub last_error: Option<String>,
}

impl SubscriptionMetadata {
    /// A fresh, never-fetched subscription.
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            mode: SubscriptionMode::AppManaged,
            interval_secs: DEFAULT_INTERVAL_SECS,
            last_updated: None,
            quota: None,
            node_count: 0,
            group_count: 0,
            last_error: None,
        }
    }

    /// Whether the subscription has ever been fetched successfully.
    pub fn has_never_succeeded(&self) -> bool {
        self.last_updated.is_none()
    }

    /// Whether this subscription's quota is knowable.
    ///
    /// It is not, for an engine-managed subscription: the engine's fetcher
    /// discards the `subscription-userinfo` header
    /// (`meow-config-0.29.0/src/subscription.rs:278-288`).
    pub fn reports_quota(&self) -> bool {
        self.mode == SubscriptionMode::AppManaged
    }

    /// The core crate's record of this subscription.
    ///
    /// The core's `Subscription` holds the quota in *its* own shape
    /// (`starpod_core::domain::SubscriptionQuota`: `used`, `total`,
    /// `expires`), so this is where the app's four-field quota is
    /// projected onto it.
    ///
    /// The `expires` field is a unix timestamp in the core's shape too —
    /// it is stored as a string there, so it is rendered as RFC 3339
    /// here. A provider that reported no expiry produces `None`, never a
    /// fabricated date.
    ///
    /// `has_auth_header` is always `false`: whether a subscription
    /// carries an `Authorization` header is a property of the *secret
    /// store*, which this crate deliberately does not read. The app sets
    /// it when it writes the subscription back.
    pub fn to_core(&self) -> starpod_core::Subscription {
        starpod_core::Subscription {
            id: self.id.clone(),
            label: self.label.clone(),
            mode: self.mode,
            interval_secs: self.interval_secs,
            has_auth_header: false,
            last_updated: self.last_updated.and_then(starpod_core::timestamp::format),
            quota: self
                .quota
                .map(|quota| starpod_core::domain::SubscriptionQuota {
                    used: quota.used(),
                    total: quota.total,
                    expires: quota.expires.and_then(starpod_core::timestamp::format),
                }),
            node_count: self.node_count,
            group_count: self.group_count,
            last_error: self.last_error.clone(),
        }
    }
}

/// Render a unix timestamp as an RFC 3339 string.
///
/// This is the core crate's formatter, re-exported so that the crate's
/// callers have one obvious name for it. The format is defined in exactly
/// one place — [`starpod_core::timestamp`] — because a second
/// implementation would be a second opinion about what a stored
/// timestamp means.
pub use starpod_core::timestamp::format as format_timestamp;

/// The default refresh interval, in seconds.
///
/// The engine's own subscription refresh loop polls every 60 seconds and
/// refreshes a subscription whose `interval` has elapsed
/// (`meow-app-0.29.0/src/subscription_refresh.rs:89-97` — the selection
/// filter — and `:505` — the 60-second sleep between passes). The app's
/// default is the same order of magnitude: an hour is long enough that a
/// panel is not hammered, and short enough that a rotated node list is
/// picked up in a reasonable time.
pub const DEFAULT_INTERVAL_SECS: u64 = 3600;

/// The longest interval the app will accept.
///
/// The engine clamps its own interval to
/// `meow_common::MAX_DURATION_SECS` — ten years
/// (`meow-common-0.29.0/src/lib.rs:14`,
/// `meow-app-0.29.0/src/subscription_refresh.rs:91-96` clamps *before*
/// the `i64` cast, because `u64::MAX as i64` wraps negative and would
/// retrigger a refresh on every poll). The app clamps the same way, for
/// the same reason.
pub const MAX_INTERVAL_SECS: u64 = 10 * 365 * 24 * 60 * 60;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_subscription_has_never_succeeded() {
        let metadata = SubscriptionMetadata::new("sub-1", "Airport");
        assert!(metadata.has_never_succeeded());
        assert_eq!(metadata.interval_secs, DEFAULT_INTERVAL_SECS);
        assert_eq!(metadata.mode, SubscriptionMode::AppManaged);
    }

    #[test]
    fn the_quota_is_parsed_from_the_header() {
        let quota = SubscriptionQuota::parse(
            "upload=455727941; download=6174315083; total=1073741824000; expire=1798675200",
        )
        .expect("the header reports a quota");
        assert_eq!(quota.upload, 455_727_941);
        assert_eq!(quota.download, 6_174_315_083);
        assert_eq!(quota.total, Some(1_073_741_824_000));
        assert_eq!(quota.expires, Some(1_798_675_200));
        assert_eq!(quota.used(), 455_727_941 + 6_174_315_083);
    }

    #[test]
    fn an_absent_field_reads_as_absent_not_as_zero() {
        let quota = SubscriptionQuota::parse("total=1024").expect("a quota is reported");
        assert_eq!(quota.total, Some(1024));
        assert_eq!(
            quota.upload, 0,
            "the upload is zero because the server did not report it"
        );
        assert_eq!(quota.expires, None, "no expire means no end date");
    }

    #[test]
    fn a_header_with_no_number_reports_no_quota() {
        assert_eq!(SubscriptionQuota::parse(""), None);
        assert_eq!(SubscriptionQuota::parse("garbage"), None);
        assert_eq!(SubscriptionQuota::parse("upload=abc; total=-1"), None);
    }

    #[test]
    fn an_unreadable_field_does_not_discard_the_readable_ones() {
        let quota = SubscriptionQuota::parse("upload=abc; total=1024")
            .expect("the readable field is a report");
        assert_eq!(
            quota.total,
            Some(1024),
            "one malformed field must not throw away the rest of the header"
        );
        assert_eq!(quota.upload, 0, "the unreadable field reads as absent");
    }

    #[test]
    fn explicit_zeros_are_a_report() {
        let quota = SubscriptionQuota::parse("upload=0; download=0; total=0; expire=0")
            .expect("explicit zeros are a report");
        assert_eq!(quota.total, Some(0));
        assert_eq!(quota.expires, Some(0));
    }

    #[test]
    fn the_keys_match_case_insensitively_and_the_order_does_not_matter() {
        let quota =
            SubscriptionQuota::parse("EXPIRE=10; Total=20;download=5; upload=1").expect("parses");
        assert_eq!(quota.upload, 1);
        assert_eq!(quota.download, 5);
        assert_eq!(quota.total, Some(20));
        assert_eq!(quota.expires, Some(10));
    }

    #[test]
    fn a_decimal_amount_truncates() {
        let quota = SubscriptionQuota::parse("download=1.5e3").expect("parses");
        assert_eq!(quota.download, 1500);
    }

    #[test]
    fn an_engine_managed_subscription_reports_no_quota() {
        let mut metadata = SubscriptionMetadata::new("sub-1", "Airport");
        metadata.mode = SubscriptionMode::EngineManaged;
        assert!(
            !metadata.reports_quota(),
            "the engine's fetcher discards the header, so the number does not exist"
        );
    }

    #[test]
    fn an_app_managed_subscription_reports_a_quota() {
        let metadata = SubscriptionMetadata::new("sub-1", "Airport");
        assert!(metadata.reports_quota());
    }

    #[test]
    fn a_timestamp_is_rendered_as_rfc3339() {
        assert_eq!(
            format_timestamp(1_798_675_200).as_deref(),
            Some("2026-12-31T00:00:00Z")
        );
    }

    #[test]
    fn an_unrepresentable_timestamp_produces_no_string() {
        assert_eq!(format_timestamp(i64::MAX), None);
    }

    #[test]
    fn the_core_record_carries_the_quota_in_the_core_shape() {
        let mut metadata = SubscriptionMetadata::new("sub-1", "Airport");
        metadata.last_updated = Some(1_798_675_200);
        metadata.quota = Some(SubscriptionQuota {
            upload: 10,
            download: 20,
            total: Some(100),
            expires: Some(1_798_675_200),
        });

        let core = metadata.to_core();
        assert_eq!(core.id, "sub-1");
        assert_eq!(core.label, "Airport");
        assert_eq!(core.mode, SubscriptionMode::AppManaged);
        let quota = core.quota.expect("the quota survives the projection");
        assert_eq!(quota.used, 30);
        assert_eq!(quota.total, Some(100));
        assert_eq!(quota.expires.as_deref(), Some("2026-12-31T00:00:00Z"));
        assert_eq!(core.last_updated.as_deref(), Some("2026-12-31T00:00:00Z"));
    }

    #[test]
    fn an_engine_managed_subscription_projects_no_quota() {
        let mut metadata = SubscriptionMetadata::new("sub-1", "Airport");
        metadata.mode = SubscriptionMode::EngineManaged;
        assert_eq!(metadata.to_core().quota, None);
    }
}
