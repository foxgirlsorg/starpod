//! The refresh: fetch, parse, validate, and keep the old payload on failure.
//!
//! This is where the pieces meet. It is the *only* place that writes to a
//! cache, and it writes only after the pipeline has produced a validated
//! payload. A failure at any step — the network, the status code, the
//! body, the validation — leaves the cache exactly as it was.
//!
//! # The clock is a parameter
//!
//! `now` is passed in. Nothing in this crate reads the system clock, so
//! the whole refresh path is deterministic under test.
//!
//! # What the refresh does with the quota
//!
//! The quota comes from the `subscription-userinfo` header, which only an
//! app-managed subscription's own fetch can see: the engine's fetcher
//! discards it (`meow-config-0.29.0/src/subscription.rs:278-288`). So the
//! quota is set on success and *left alone* on failure — a failed refresh
//! does not clear a previously known quota, because the previous number is
//! still the last thing the server told us.

use starpod_core::SubscriptionPayload;

use crate::cache::SubscriptionCache;
use crate::error::SubscriptionError;
use crate::metadata::{ProfileInfo, SubscriptionMetadata, SubscriptionQuota};
use crate::pipeline::{PayloadFormat, parse_payload};
use crate::provider::{FetchRequest, FetchResponse, SubscriptionProvider};

/// What a refresh did.
#[derive(Debug, Clone, PartialEq)]
pub enum RefreshOutcome {
    /// The subscription was fetched, parsed and validated.
    Refreshed {
        /// The validated payload.
        payload: Box<SubscriptionPayload>,
        /// The format the body was in.
        format: PayloadFormat,
        /// The quota the server reported, if it reported one.
        quota: Option<SubscriptionQuota>,
        /// The profile headers the server sent. Each field is `None` when its
        /// header was absent, so a successful refresh replaces all of them.
        profile: Box<ProfileInfo>,
    },
    /// The refresh failed.
    ///
    /// The cache was not touched — the previously cached payload, if there
    /// was one, is still there.
    Failed {
        /// Why it failed.
        error: SubscriptionError,
        /// Whether a previously cached payload is still available.
        kept_previous: bool,
        /// The profile headers, when the server answered with them before the
        /// refresh failed. Set for a device-limit refusal, so the support link
        /// can still be shown; `None` otherwise.
        profile: Option<Box<ProfileInfo>>,
    },
}

impl RefreshOutcome {
    /// The outcome's error, if it failed.
    pub fn error(&self) -> Option<&SubscriptionError> {
        match self {
            Self::Failed { error, .. } => Some(error),
            Self::Refreshed { .. } => None,
        }
    }

    /// Whether the refresh succeeded.
    pub fn is_ok(&self) -> bool {
        matches!(self, Self::Refreshed { .. })
    }

    /// The payload, if the refresh succeeded.
    pub fn payload(&self) -> Option<&SubscriptionPayload> {
        match self {
            Self::Refreshed { payload, .. } => Some(payload.as_ref()),
            Self::Failed { .. } => None,
        }
    }
}

/// Refresh one subscription.
///
/// `provider` is the seam; `request` carries the URL, the optional
/// `Authorization` header and the timeout; `metadata` carries the id and
/// the label the payload is namespaced with; `cache` is updated only on
/// success; `now` is the unix timestamp, in seconds, the refresh is
/// stamped with.
///
/// The function never panics and never leaves the cache in a half-written
/// state: the cache is written in one place, after the pipeline returned a
/// validated payload.
pub async fn refresh(
    provider: &dyn SubscriptionProvider,
    request: &FetchRequest,
    metadata: &SubscriptionMetadata,
    cache: &mut SubscriptionCache,
    now: i64,
) -> RefreshOutcome {
    let response = match provider.fetch(request).await {
        Ok(response) => response,
        Err(error) => {
            return RefreshOutcome::Failed {
                error: SubscriptionError::Provider(error),
                kept_previous: !cache.is_empty(),
                profile: None,
            };
        }
    };

    let quota = response.userinfo().and_then(SubscriptionQuota::parse);
    let profile = Box::new(ProfileInfo::from_response(&response));

    // A device-limit refusal is checked first: the provider may answer it with
    // a page, and the page must not be reported as the wrong thing.
    if profile.device_limit {
        return RefreshOutcome::Failed {
            error: SubscriptionError::DeviceLimitReached,
            kept_previous: !cache.is_empty(),
            profile: Some(profile),
        };
    }
    if is_web_page(&response) {
        return RefreshOutcome::Failed {
            error: SubscriptionError::ServedWebPage,
            kept_previous: !cache.is_empty(),
            profile: None,
        };
    }

    match parse_payload(&response.body, &metadata.id, &metadata.label) {
        Ok(parsed) => {
            cache.store(parsed.payload.clone(), now);
            RefreshOutcome::Refreshed {
                payload: Box::new(parsed.payload),
                format: parsed.format,
                quota,
                profile,
            }
        }
        Err(error) => RefreshOutcome::Failed {
            error,
            kept_previous: !cache.is_empty(),
            profile: None,
        },
    }
}

/// Whether the response is an HTML or XML page rather than a subscription.
fn is_web_page(response: &FetchResponse) -> bool {
    response.header("content-type").is_some_and(|value| {
        let media_type = value.split(';').next().unwrap_or_default().trim();
        media_type.eq_ignore_ascii_case("text/html") || media_type.eq_ignore_ascii_case("text/xml")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{FetchResponse, StaticSubscriptionProvider};
    use starpod_core::Node;

    const BODY: &str = "\
proxies:
  - name: \"Node A\"
    type: trojan
    server: a.example.com
    port: 443
    password: secret
";

    fn metadata() -> SubscriptionMetadata {
        SubscriptionMetadata::new("sub-1", "Airport")
    }

    fn request() -> FetchRequest {
        FetchRequest::new("https://example.invalid/sub")
    }

    fn cached_payload(name: &str) -> SubscriptionPayload {
        SubscriptionPayload {
            subscription_id: "sub-1".to_string(),
            nodes: vec![Node {
                id: "old".to_string(),
                name: name.to_string(),
                protocol: "trojan".to_string(),
                server: Some("a.example.com".to_string()),
                port: Some(443),
                tags: Vec::new(),
                raw: Default::default(),
            }],
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_good_refresh_stores_the_payload() {
        let provider = StaticSubscriptionProvider::new(BODY.as_bytes().to_vec());
        let mut cache = SubscriptionCache::new();
        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        assert!(outcome.is_ok());
        let payload = outcome.payload().expect("a payload");
        assert_eq!(payload.nodes.len(), 1);
        assert_eq!(payload.nodes[0].name, "Airport / Node A");
        assert_eq!(cache.stored_at(), Some(1_000));
    }

    #[tokio::test]
    async fn a_good_refresh_reports_the_quota_from_the_header() {
        let provider = StaticSubscriptionProvider::with_response(
            FetchResponse::new(BODY.as_bytes().to_vec())
                .with_header("subscription-userinfo", "upload=10; download=20; total=100"),
        );
        let mut cache = SubscriptionCache::new();
        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        let RefreshOutcome::Refreshed { quota, .. } = outcome else {
            panic!("the refresh succeeded");
        };
        let quota = quota.expect("the header carried a quota");
        assert_eq!(quota.used(), 30);
        assert_eq!(quota.total, Some(100));
    }

    #[tokio::test]
    async fn a_refresh_without_the_header_reports_no_quota() {
        let provider = StaticSubscriptionProvider::new(BODY.as_bytes().to_vec());
        let mut cache = SubscriptionCache::new();
        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        let RefreshOutcome::Refreshed { quota, .. } = outcome else {
            panic!("the refresh succeeded");
        };
        assert_eq!(quota, None, "no header means no quota, not a zero quota");
    }

    #[tokio::test]
    async fn a_corrupt_body_leaves_the_cached_payload_alone() {
        let provider = StaticSubscriptionProvider::new(b"this is not a subscription".to_vec());
        let mut cache = SubscriptionCache::new();
        cache.store(cached_payload("Old Node"), 500);

        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        assert!(!outcome.is_ok());
        assert!(matches!(
            outcome.error(),
            Some(SubscriptionError::InvalidBody(_))
        ));
        assert_eq!(
            cache.payload(),
            Some(&cached_payload("Old Node")),
            "the corrupt body did not overwrite the good payload"
        );
        assert_eq!(cache.stored_at(), Some(500), "the timestamp did not move");
    }

    #[tokio::test]
    async fn a_failed_refresh_reports_that_the_previous_payload_was_kept() {
        let provider = StaticSubscriptionProvider::new(b"garbage".to_vec());
        let mut cache = SubscriptionCache::new();
        cache.store(cached_payload("Old Node"), 500);

        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;
        let RefreshOutcome::Failed { kept_previous, .. } = &outcome else {
            panic!("the refresh failed");
        };
        assert!(kept_previous);
    }

    #[tokio::test]
    async fn a_failed_refresh_with_no_cache_says_so() {
        let provider = StaticSubscriptionProvider::new(b"garbage".to_vec());
        let mut cache = SubscriptionCache::new();
        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;
        let RefreshOutcome::Failed { kept_previous, .. } = &outcome else {
            panic!("the refresh failed");
        };
        assert!(!kept_previous);
    }

    #[tokio::test]
    async fn a_subscription_whose_every_node_is_invalid_fails_without_touching_the_cache() {
        let body = "proxies:\n  - { name: A, type: wireguard, server: a.example.com, port: 443 }\n";
        let provider = StaticSubscriptionProvider::new(body.as_bytes().to_vec());
        let mut cache = SubscriptionCache::new();
        cache.store(cached_payload("Old Node"), 500);

        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        assert!(matches!(
            outcome.error(),
            Some(SubscriptionError::EveryNodeDropped { .. })
        ));
        assert_eq!(cache.payload(), Some(&cached_payload("Old Node")));
    }

    #[tokio::test]
    async fn a_provider_failure_is_reported_as_a_provider_error() {
        struct FailingProvider;
        #[async_trait::async_trait]
        impl SubscriptionProvider for FailingProvider {
            async fn fetch(
                &self,
                _request: &FetchRequest,
            ) -> Result<FetchResponse, crate::error::ProviderError> {
                Err(crate::error::ProviderError::Unauthorized { status: 401 })
            }
            fn kind(&self) -> crate::provider::ProviderKind {
                crate::provider::ProviderKind::Http
            }
        }

        let mut cache = SubscriptionCache::new();
        cache.store(cached_payload("Old Node"), 500);

        let outcome = refresh(&FailingProvider, &request(), &metadata(), &mut cache, 1_000).await;

        let error = outcome.error().expect("the refresh failed");
        assert!(
            error.is_fatal(),
            "an expired token is not a transient fault"
        );
        assert_eq!(cache.payload(), Some(&cached_payload("Old Node")));
    }

    #[tokio::test]
    async fn a_base64_body_is_refreshed_successfully() {
        use base64::Engine as _;
        use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;

        let provider = StaticSubscriptionProvider::new(BASE64_STANDARD.encode(BODY).into_bytes());
        let mut cache = SubscriptionCache::new();
        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        assert!(outcome.is_ok());
        let RefreshOutcome::Refreshed { format, .. } = &outcome else {
            panic!("the refresh succeeded");
        };
        assert_eq!(*format, PayloadFormat::Base64Yaml);
    }

    #[tokio::test]
    async fn a_refresh_reports_the_profile_headers_it_was_sent() {
        let provider = StaticSubscriptionProvider::with_response(
            FetchResponse::new(BODY.as_bytes().to_vec())
                .with_header("announce", "Maintenance tonight")
                .with_header("profile-title", "Foxgirls VPN"),
        );
        let mut cache = SubscriptionCache::new();
        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        let RefreshOutcome::Refreshed { profile, .. } = outcome else {
            panic!("the refresh succeeded");
        };
        assert_eq!(profile.announce.as_deref(), Some("Maintenance tonight"));
        assert_eq!(profile.title.as_deref(), Some("Foxgirls VPN"));
        assert_eq!(profile.support_url, None, "an absent header reads as none");
    }

    #[tokio::test]
    async fn a_device_limit_refusal_keeps_the_cached_payload_and_the_support_link() {
        let provider = StaticSubscriptionProvider::with_response(
            FetchResponse::new(BODY.as_bytes().to_vec())
                .with_header("x-hwid-max-devices-reached", "true")
                .with_header("support-url", "https://help.example.com"),
        );
        let mut cache = SubscriptionCache::new();
        cache.store(cached_payload("Old Node"), 500);

        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        let RefreshOutcome::Failed {
            error,
            kept_previous,
            profile,
        } = outcome
        else {
            panic!("the device limit fails the refresh");
        };
        assert_eq!(error, SubscriptionError::DeviceLimitReached);
        assert!(kept_previous);
        let profile = profile.expect("the headers are kept for the support link");
        assert_eq!(
            profile.support_url.as_deref(),
            Some("https://help.example.com")
        );
        assert_eq!(
            cache.payload(),
            Some(&cached_payload("Old Node")),
            "the payload from before the limit is still in use"
        );
    }

    #[test]
    fn the_device_limit_message_is_the_one_the_user_is_shown() {
        assert_eq!(
            SubscriptionError::DeviceLimitReached.to_string(),
            "This subscription has reached its device limit. Contact support to free a slot."
        );
    }

    #[tokio::test]
    async fn a_web_page_is_refused_with_a_clear_error_and_keeps_the_cache() {
        let provider = StaticSubscriptionProvider::with_response(
            FetchResponse::new(BODY.as_bytes().to_vec())
                .with_header("content-type", "text/html; charset=utf-8"),
        );
        let mut cache = SubscriptionCache::new();
        cache.store(cached_payload("Old Node"), 500);

        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;

        assert_eq!(outcome.error(), Some(&SubscriptionError::ServedWebPage));
        assert_eq!(
            SubscriptionError::ServedWebPage.to_string(),
            "The server returned a web page, not a subscription"
        );
        assert_eq!(cache.payload(), Some(&cached_payload("Old Node")));
    }

    #[tokio::test]
    async fn an_xml_page_is_refused_too_but_a_yaml_content_type_is_not() {
        for content_type in ["text/xml", "TEXT/XML; charset=utf-8"] {
            let provider = StaticSubscriptionProvider::with_response(
                FetchResponse::new(BODY.as_bytes().to_vec())
                    .with_header("content-type", content_type),
            );
            let mut cache = SubscriptionCache::new();
            let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;
            assert_eq!(
                outcome.error(),
                Some(&SubscriptionError::ServedWebPage),
                "{content_type}"
            );
        }

        let provider = StaticSubscriptionProvider::with_response(
            FetchResponse::new(BODY.as_bytes().to_vec())
                .with_header("content-type", "application/yaml"),
        );
        let mut cache = SubscriptionCache::new();
        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;
        assert!(outcome.is_ok(), "a YAML body is a subscription");
    }

    #[tokio::test]
    async fn a_refresh_reports_the_update_interval_for_the_schedule() {
        let provider = StaticSubscriptionProvider::with_response(
            FetchResponse::new(BODY.as_bytes().to_vec())
                .with_header("profile-update-interval", "2"),
        );
        let mut cache = SubscriptionCache::new();
        let outcome = refresh(&provider, &request(), &metadata(), &mut cache, 1_000).await;
        let RefreshOutcome::Refreshed { profile, .. } = outcome else {
            panic!("the refresh succeeded");
        };
        assert_eq!(profile.update_interval_hours, Some(2));
    }
}
