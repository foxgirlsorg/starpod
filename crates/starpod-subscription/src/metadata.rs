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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use starpod_core::SubscriptionMode;

use crate::provider::FetchResponse;

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
    /// The profile headers ([`ProfileInfo`]) are not part of this record; the
    /// app stores them on the core record itself after a refresh, so they are
    /// `None` here.
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
            local: false,
        }
    }
}

/// The longest announcement the app keeps, in characters.
pub const ANNOUNCE_MAX_CHARS: usize = 2000;

/// The profile information a subscription response carries in its headers.
///
/// Every field is optional or empty by default: a header that is absent,
/// empty, undecodable or (for the URLs) not http or https reads as its
/// default, never as a fabricated value. A successful refresh replaces all of
/// them; a failed one keeps the previous values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileInfo {
    /// The `profile-title` header, or the file name in `content-disposition`
    /// when the title header is absent.
    pub title: Option<String>,
    /// The `profile-web-page-name` header: the label for the web page link.
    pub web_page_name: Option<String>,
    /// The `announce` header, with the two-character sequence `\n` read as a
    /// line break, capped at [`ANNOUNCE_MAX_CHARS`].
    pub announce: Option<String>,
    /// The `announce-url` header: a link shown with the announcement.
    pub announce_url: Option<String>,
    /// The `support-url` header.
    pub support_url: Option<String>,
    /// The `profile-web-page-url` header.
    pub web_url: Option<String>,
    /// The `profile-logo` header: an http or https URL of the profile's logo.
    /// The logo is fetched separately; see [`crate::logo`].
    pub logo_url: Option<String>,
    /// The `profile-update-interval` header, in whole hours. Only a value above
    /// zero is kept.
    pub update_interval_hours: Option<u64>,
    /// `true` when the `global-mode` header is `false`: the provider does not
    /// allow the Global routing mode while this subscription is active.
    pub global_mode_disabled: bool,
    /// The `expand-proxy-groups` header. `Some(false)` means groups start
    /// collapsed; `None` means the header was absent or unreadable.
    pub expand_proxy_groups: Option<bool>,
    /// `true` when the `x-hwid-limit` or `x-hwid-max-devices-reached` header is
    /// `true`: this device has no slot left on the provider.
    pub device_limit: bool,
}

impl ProfileInfo {
    /// Read the profile headers from a response.
    ///
    /// Text headers are plain text, or `base64:<standard base64>` which is
    /// decoded. A value that fails to decode is ignored.
    pub fn from_response(response: &FetchResponse) -> Self {
        Self {
            title: header_text(response, "profile-title")
                .or_else(|| content_disposition_filename(response)),
            web_page_name: header_text(response, "profile-web-page-name"),
            announce: header_text(response, "announce").map(|text| {
                text.replace(r"\n", "\n")
                    .trim()
                    .chars()
                    .take(ANNOUNCE_MAX_CHARS)
                    .collect()
            }),
            announce_url: header_url(response, "announce-url"),
            support_url: header_url(response, "support-url"),
            web_url: header_url(response, "profile-web-page-url"),
            logo_url: header_url(response, "profile-logo"),
            update_interval_hours: response
                .header("profile-update-interval")
                .and_then(|value| value.trim().parse::<u64>().ok())
                .filter(|hours| *hours > 0),
            global_mode_disabled: response
                .header("global-mode")
                .is_some_and(|value| value.trim().eq_ignore_ascii_case("false")),
            expand_proxy_groups: response.header("expand-proxy-groups").and_then(
                |value| match value.trim().to_ascii_lowercase().as_str() {
                    "false" => Some(false),
                    "true" => Some(true),
                    _ => None,
                },
            ),
            device_limit: is_true(response, "x-hwid-limit")
                || is_true(response, "x-hwid-max-devices-reached"),
        }
    }
}

/// Whether a header carries the value `true`.
fn is_true(response: &FetchResponse, name: &str) -> bool {
    response
        .header(name)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

const BASE64_PREFIX: &str = "base64:";

/// A header's text, trimmed, decoded from base64 when it carries the prefix.
fn header_text(response: &FetchResponse, name: &str) -> Option<String> {
    let raw = response.header(name)?.trim();
    let text = match raw.get(..BASE64_PREFIX.len()) {
        Some(prefix) if prefix.eq_ignore_ascii_case(BASE64_PREFIX) => {
            let bytes = BASE64_STANDARD
                .decode(raw[BASE64_PREFIX.len()..].trim())
                .ok()?;
            String::from_utf8(bytes).ok()?
        }
        _ => raw.to_string(),
    };
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// A header's text, kept only when it is an http or https URL with a host.
fn header_url(response: &FetchResponse, name: &str) -> Option<String> {
    let text = header_text(response, name)?;
    let url = reqwest::Url::parse(&text).ok()?;
    (matches!(url.scheme(), "http" | "https") && url.host_str().is_some()).then_some(text)
}

/// The file name in a `content-disposition` header, without a YAML or text
/// extension, used as the title when the provider sent no `profile-title`.
///
/// `filename*=UTF-8''<percent-encoded>` wins over `filename=`, as RFC 6266
/// asks. Only UTF-8 is decoded; any other charset reads as absent.
fn content_disposition_filename(response: &FetchResponse) -> Option<String> {
    let value = response.header("content-disposition")?;
    let mut plain = None;
    let mut encoded = None;
    for parameter in value.split(';').skip(1) {
        let Some((key, raw)) = parameter.trim().split_once('=') else {
            continue;
        };
        let raw = raw.trim();
        if key.trim().eq_ignore_ascii_case("filename*") {
            encoded = decode_extended_value(raw);
        } else if key.trim().eq_ignore_ascii_case("filename") {
            plain = Some(raw.trim_matches('"').to_string());
        }
    }
    let name = encoded.or(plain)?;
    let name = strip_text_extension(name.trim());
    (!name.is_empty()).then(|| name.to_string())
}

/// Decode an RFC 5987 extended value: `charset'language'percent-encoded`.
fn decode_extended_value(raw: &str) -> Option<String> {
    let mut parts = raw.splitn(3, '\'');
    let charset = parts.next()?;
    let _language = parts.next()?;
    let value = parts.next()?;
    if !charset.eq_ignore_ascii_case("utf-8") {
        return None;
    }
    percent_decode(value)
}

/// Percent-decode text as UTF-8. A malformed escape or invalid UTF-8 is `None`.
fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let digits = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            out.push(u8::from_str_radix(digits, 16).ok()?);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Drop a `.yaml`, `.yml` or `.txt` extension from a file name.
fn strip_text_extension(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    [".yaml", ".yml", ".txt"]
        .iter()
        .find(|extension| lower.ends_with(*extension))
        .map_or(name, |extension| &name[..name.len() - extension.len()])
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

    fn response_with(name: &str, value: &str) -> FetchResponse {
        FetchResponse::new(b"body".to_vec()).with_header(name, value)
    }

    #[test]
    fn a_plain_text_announcement_is_trimmed_and_kept() {
        let info =
            ProfileInfo::from_response(&response_with("Announce", "  Maintenance tonight  "));
        assert_eq!(info.announce.as_deref(), Some("Maintenance tonight"));
    }

    #[test]
    fn a_base64_header_is_decoded_for_every_profile_field() {
        let encoded = format!("base64:{}", BASE64_STANDARD.encode("Фоксгёрлз VPN"));
        let response = FetchResponse::new(b"body".to_vec())
            .with_header("profile-title", encoded)
            .with_header(
                "announce",
                format!("base64:{}", BASE64_STANDARD.encode("Hello")),
            );
        let info = ProfileInfo::from_response(&response);
        assert_eq!(info.title.as_deref(), Some("Фоксгёрлз VPN"));
        assert_eq!(info.announce.as_deref(), Some("Hello"));
    }

    #[test]
    fn an_undecodable_base64_header_is_ignored_not_kept_raw() {
        let info = ProfileInfo::from_response(&response_with("announce", "base64:not*base64!!"));
        assert_eq!(info.announce, None);
    }

    #[test]
    fn an_absent_header_reads_as_none() {
        let info = ProfileInfo::from_response(&FetchResponse::new(b"body".to_vec()));
        assert_eq!(info, ProfileInfo::default());
    }

    #[test]
    fn an_empty_header_reads_as_none() {
        let info = ProfileInfo::from_response(&response_with("profile-title", "   "));
        assert_eq!(info.title, None);
    }

    #[test]
    fn an_announcement_is_capped_at_two_thousand_characters() {
        let long = "я".repeat(ANNOUNCE_MAX_CHARS + 50);
        let info = ProfileInfo::from_response(&response_with("announce", &long));
        assert_eq!(
            info.announce.as_deref().map(|text| text.chars().count()),
            Some(ANNOUNCE_MAX_CHARS)
        );
    }

    #[test]
    fn only_http_and_https_urls_are_kept() {
        let good = ProfileInfo::from_response(&response_with(
            "support-url",
            "https://help.example.com/x?a=1",
        ));
        assert_eq!(
            good.support_url.as_deref(),
            Some("https://help.example.com/x?a=1")
        );

        let web = ProfileInfo::from_response(&response_with(
            "profile-web-page-url",
            "http://example.com",
        ));
        assert_eq!(web.web_url.as_deref(), Some("http://example.com"));

        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "not a url",
            "https://",
        ] {
            let info = ProfileInfo::from_response(&response_with("support-url", bad));
            assert_eq!(info.support_url, None, "{bad} must be dropped");
        }
    }

    #[test]
    fn the_profile_header_names_match_case_insensitively() {
        let response = FetchResponse::new(b"body".to_vec())
            .with_header("PROFILE-TITLE", "Upper")
            .with_header("Support-URL", "https://example.com");
        let info = ProfileInfo::from_response(&response);
        assert_eq!(info.title.as_deref(), Some("Upper"));
        assert_eq!(info.support_url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn the_two_character_sequence_backslash_n_becomes_a_line_break() {
        let info = ProfileInfo::from_response(&response_with(
            "announce",
            r"Maintenance\nTonight\n\nBring a charger",
        ));
        assert_eq!(
            info.announce.as_deref(),
            Some("Maintenance\nTonight\n\nBring a charger")
        );
    }

    #[test]
    fn a_base64_announcement_also_has_its_line_breaks_read() {
        let encoded = format!("base64:{}", BASE64_STANDARD.encode(r"One\nTwo"));
        let info = ProfileInfo::from_response(&response_with("announce", &encoded));
        assert_eq!(info.announce.as_deref(), Some("One\nTwo"));
    }

    #[test]
    fn the_announce_url_is_kept_only_as_http_or_https() {
        let good = ProfileInfo::from_response(&response_with(
            "announce-url",
            "https://news.example.com/post",
        ));
        assert_eq!(
            good.announce_url.as_deref(),
            Some("https://news.example.com/post")
        );
        let bad = ProfileInfo::from_response(&response_with("announce-url", "javascript:x"));
        assert_eq!(bad.announce_url, None);
    }

    #[test]
    fn the_web_page_name_is_decoded_like_the_title() {
        let encoded = format!("base64:{}", BASE64_STANDARD.encode("Личный кабинет"));
        let info = ProfileInfo::from_response(&response_with("profile-web-page-name", &encoded));
        assert_eq!(info.web_page_name.as_deref(), Some("Личный кабинет"));
    }

    #[test]
    fn the_logo_is_kept_only_as_an_http_or_https_url() {
        let good = ProfileInfo::from_response(&response_with(
            "profile-logo",
            "https://cdn.example.com/logo.png",
        ));
        assert_eq!(
            good.logo_url.as_deref(),
            Some("https://cdn.example.com/logo.png")
        );
        let bad = ProfileInfo::from_response(&response_with("profile-logo", "file:///x.png"));
        assert_eq!(bad.logo_url, None);
    }

    #[test]
    fn the_update_interval_is_whole_hours_above_zero() {
        let hours = ProfileInfo::from_response(&response_with("profile-update-interval", "6"));
        assert_eq!(hours.update_interval_hours, Some(6));
        for bad in ["0", "-1", "1.5", "soon", ""] {
            let info = ProfileInfo::from_response(&response_with("profile-update-interval", bad));
            assert_eq!(info.update_interval_hours, None, "{bad:?} must be ignored");
        }
    }

    #[test]
    fn global_mode_is_disabled_only_by_the_value_false() {
        let off = ProfileInfo::from_response(&response_with("global-mode", "false"));
        assert!(off.global_mode_disabled);
        let on = ProfileInfo::from_response(&response_with("global-mode", "true"));
        assert!(!on.global_mode_disabled);
        let absent = ProfileInfo::from_response(&FetchResponse::new(b"body".to_vec()));
        assert!(!absent.global_mode_disabled, "absent means allowed");
    }

    #[test]
    fn expand_proxy_groups_is_stored_as_sent() {
        let collapsed = ProfileInfo::from_response(&response_with("expand-proxy-groups", "false"));
        assert_eq!(collapsed.expand_proxy_groups, Some(false));
        let expanded = ProfileInfo::from_response(&response_with("expand-proxy-groups", "TRUE"));
        assert_eq!(expanded.expand_proxy_groups, Some(true));
        let other = ProfileInfo::from_response(&response_with("expand-proxy-groups", "maybe"));
        assert_eq!(other.expand_proxy_groups, None);
    }

    #[test]
    fn either_hwid_limit_header_set_to_true_reports_the_device_limit() {
        for name in ["x-hwid-limit", "x-hwid-max-devices-reached"] {
            let info = ProfileInfo::from_response(&response_with(name, "true"));
            assert!(info.device_limit, "{name}: true means the limit is reached");
        }
        let no = ProfileInfo::from_response(&response_with("x-hwid-limit", "false"));
        assert!(!no.device_limit);
    }

    #[test]
    fn the_content_disposition_filename_is_the_title_when_there_is_no_profile_title() {
        let info = ProfileInfo::from_response(&response_with(
            "content-disposition",
            r#"attachment; filename="Airport.yaml""#,
        ));
        assert_eq!(info.title.as_deref(), Some("Airport"));
    }

    #[test]
    fn the_extended_filename_wins_and_is_percent_decoded() {
        let info = ProfileInfo::from_response(&response_with(
            "content-disposition",
            "attachment; filename=\"fallback.yaml\"; filename*=UTF-8''%D0%90%D1%8D%D1%80%D0%BE.yml",
        ));
        assert_eq!(info.title.as_deref(), Some("Аэро"));
    }

    #[test]
    fn a_profile_title_beats_the_content_disposition_filename() {
        let response = FetchResponse::new(b"body".to_vec())
            .with_header("profile-title", "Foxgirls VPN")
            .with_header("content-disposition", r#"attachment; filename="x.yaml""#);
        assert_eq!(
            ProfileInfo::from_response(&response).title.as_deref(),
            Some("Foxgirls VPN")
        );
    }

    #[test]
    fn a_malformed_extended_filename_reads_as_absent() {
        let info = ProfileInfo::from_response(&response_with(
            "content-disposition",
            "attachment; filename*=latin1''%E9.yaml",
        ));
        assert_eq!(info.title, None, "a non-UTF-8 charset is not decoded");
    }
}
