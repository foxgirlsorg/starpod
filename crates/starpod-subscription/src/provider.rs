//! Where the bytes come from.
//!
//! [`SubscriptionProvider`] is the seam GOAL.md §3 asks for: "design an
//! appropriate provider abstraction in the meantime, without pretending
//! that a real integration has been completed". The trait is deliberately
//! narrow — it does one HTTP GET and reads the response headers. Nothing
//! else. That is what makes a mock implementation a three-line struct, and
//! what keeps the tests free of a network.
//!
//! # What the trait deliberately does *not* do
//!
//! It does not parse. It does not validate. It does not know about
//! subscriptions, labels, or the engine. It returns bytes and headers,
//! and the pipeline decides what they mean. A provider that "understood"
//! the body would be impossible to mock with canned bytes of an
//! arbitrary format.
//!
//! # The HTTP implementation
//!
//! [`HttpSubscriptionProvider`] performs a plain `GET`:
//!
//! - the URL may carry its token in the query string — the app never
//!   rewrites it;
//! - an optional `Authorization` header value is sent verbatim;
//! - a timeout bounds the whole request;
//! - redirects are followed, bounded, and the `Authorization` header is
//!   dropped on a cross-origin hop (which `reqwest` does by itself —
//!   `reqwest-0.13.5/src/redirect.rs:239-251` removes `AUTHORIZATION`,
//!   `COOKIE`, `cookie2`, `PROXY_AUTHORIZATION` and `WWW_AUTHENTICATE`
//!   when the host, port or scheme changes);
//! - the `subscription-userinfo` response header is surfaced, because the
//!   engine's own fetcher throws it away
//!   (`meow-config-0.29.0/src/subscription.rs:278-288` calls
//!   `internal_http::fetch`, which returns only the body).

use std::time::Duration;

use async_trait::async_trait;

use crate::error::ProviderError;

/// The response header carrying the plan usage, matched case-insensitively.
///
/// The engine names the same header
/// (`meow-config-0.29.0/src/subscription_userinfo.rs:13`), and the format
/// is `;`-separated `key=value` pairs.
pub const SUBSCRIPTION_USERINFO_HEADER: &str = "subscription-userinfo";

/// What kind of provider this is.
///
/// The kind is informational — it exists so the Servers screen can say
/// where a subscription came from, and so a future vendor API can be
/// distinguished from the plain HTTP fetch without a type change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProviderKind {
    /// A plain HTTP GET against a URL.
    Http,
    /// A file on disk, read without a network.
    ///
    /// Useful for a local profile, and for the tests.
    LocalFile,
}

impl ProviderKind {
    /// A short, stable label for the UI.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::LocalFile => "local-file",
        }
    }
}

/// What to fetch.
///
/// The URL is a plain `String` here rather than a secret wrapper: the
/// secret store hands out the value at the moment of the request, and the
/// request owns it for exactly as long as it needs it. Nothing in this
/// crate logs a [`FetchRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRequest {
    /// The URL to fetch. It may carry a token in its query string.
    pub url: String,
    /// An optional `Authorization` header value, sent verbatim.
    pub authorization: Option<String>,
    /// The bound on the whole request, including reading the body.
    pub timeout: Duration,
    /// The device headers to send, or `None` to send none (the user turned
    /// device identification off).
    pub device: Option<DeviceHeaders>,
}

/// The device identification a provider's panel reads from the request.
///
/// The values are computed by the app, which owns the platform probing. This
/// crate only puts them on the wire, so it stays free of OS calls. Every value
/// must be printable ASCII, because an HTTP header cannot carry anything else;
/// the app sanitizes them before they reach here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceHeaders {
    /// `x-hwid`: a stable, anonymous device id.
    pub hwid: String,
    /// `x-device-os`: `Windows`, `Linux` or `macOS`.
    pub os: String,
    /// `x-ver-os`: the OS version, best effort.
    pub os_version: String,
    /// `x-device-model`: the device model, or the CPU architecture.
    pub model: String,
}

impl FetchRequest {
    /// A request with the given URL and a 30-second bound.
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            authorization: None,
            timeout: Duration::from_secs(30),
            device: None,
        }
    }

    /// The same request with device headers.
    pub fn with_device(mut self, device: DeviceHeaders) -> Self {
        self.device = Some(device);
        self
    }

    /// The same request with an `Authorization` header.
    pub fn with_authorization(mut self, value: impl Into<String>) -> Self {
        self.authorization = Some(value.into());
        self
    }

    /// The same request with a different timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

/// What came back.
///
/// The body is raw bytes — the provider does not decode, decompress beyond
/// what the HTTP stack does, or parse anything. The header map is the raw
/// response header set, so the pipeline can look for
/// `subscription-userinfo` without the provider having to know what that
/// header means.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchResponse {
    /// The response body.
    pub body: Vec<u8>,
    /// The response headers.
    pub headers: Vec<(String, String)>,
}

impl FetchResponse {
    /// A response with a body and no headers.
    pub fn new(body: impl Into<Vec<u8>>) -> Self {
        Self {
            body: body.into(),
            headers: Vec::new(),
        }
    }

    /// The same response with a header.
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    /// The value of the first header with this name, compared
    /// case-insensitively.
    ///
    /// The engine does the same for its own header lookup
    /// (`meow-config-0.29.0/src/internal_http.rs:205-210`).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(field, _)| field.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// The `subscription-userinfo` header, if the server sent one.
    pub fn userinfo(&self) -> Option<&str> {
        self.header(SUBSCRIPTION_USERINFO_HEADER)
    }
}

/// The seam a subscription's bytes come through.
///
/// Implementations perform *one* HTTP GET and return the body and headers.
/// They must not parse, validate, or interpret the payload — that is the
/// pipeline's job, and keeping the split sharp is what lets the tests
/// substitute canned bytes.
#[async_trait]
pub trait SubscriptionProvider: Send + Sync {
    /// Fetch the subscription's bytes.
    async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ProviderError>;

    /// What kind of provider this is.
    fn kind(&self) -> ProviderKind;
}

/// A provider that performs a plain HTTP GET.
#[derive(Debug, Clone)]
pub struct HttpSubscriptionProvider {
    client: reqwest::Client,
    /// The maximum number of redirects to follow.
    max_redirects: usize,
}

impl HttpSubscriptionProvider {
    /// The default bound on redirects.
    pub const DEFAULT_MAX_REDIRECTS: usize = 5;

    /// A provider with a 30-second request timeout and a bounded redirect
    /// chain.
    pub fn new() -> Result<Self, ProviderError> {
        Self::builder().build()
    }

    /// A builder for a provider with a specific timeout and redirect bound.
    ///
    /// The timeout is a *total* request timeout: it covers the connect,
    /// the TLS handshake, the response headers and the body — not each
    /// phase separately. A subscription server that accepts a connection
    /// and then stalls must not hang the refresh forever.
    pub fn builder() -> HttpSubscriptionProviderBuilder {
        HttpSubscriptionProviderBuilder::default()
    }
}

impl Default for HttpSubscriptionProvider {
    /// A provider with the default timeout and redirect bound.
    ///
    /// A `reqwest::Client` can only fail to build when the TLS backend
    /// cannot initialize, which is not a condition a running app can do
    /// anything about. Rather than panic, this falls back to a client
    /// with the same settings, and the failure surfaces on the first
    /// fetch as a [`ProviderError::Network`].
    fn default() -> Self {
        Self::builder().build().unwrap_or_else(|_| {
            let client = crate::tls::configure(reqwest::Client::builder())
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::limited(
                    HttpSubscriptionProvider::DEFAULT_MAX_REDIRECTS,
                ))
                .build()
                .unwrap_or_default();
            HttpSubscriptionProvider {
                client,
                max_redirects: HttpSubscriptionProvider::DEFAULT_MAX_REDIRECTS,
            }
        })
    }
}

/// A builder for [`HttpSubscriptionProvider`].
#[derive(Debug, Clone)]
pub struct HttpSubscriptionProviderBuilder {
    timeout: Duration,
    max_redirects: usize,
    user_agent: String,
}

impl Default for HttpSubscriptionProviderBuilder {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            max_redirects: HttpSubscriptionProvider::DEFAULT_MAX_REDIRECTS,
            user_agent: format!("starpod/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

impl HttpSubscriptionProviderBuilder {
    /// The total request timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// The maximum number of redirects to follow.
    pub fn max_redirects(mut self, max_redirects: usize) -> Self {
        self.max_redirects = max_redirects;
        self
    }

    /// The `User-Agent` the request carries.
    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    /// Build the provider.
    pub fn build(self) -> Result<HttpSubscriptionProvider, ProviderError> {
        let client = crate::tls::configure(reqwest::Client::builder())
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::limited(self.max_redirects))
            .user_agent(self.user_agent)
            .build()
            .map_err(|error| ProviderError::Network(error.to_string()))?;
        Ok(HttpSubscriptionProvider {
            client,
            max_redirects: self.max_redirects,
        })
    }
}

#[async_trait]
impl SubscriptionProvider for HttpSubscriptionProvider {
    async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ProviderError> {
        let mut builder = self.client.get(&request.url);
        if let Some(authorization) = &request.authorization {
            builder = builder.header(reqwest::header::AUTHORIZATION, authorization);
        }
        if let Some(device) = &request.device {
            builder = builder
                .header("x-hwid", device.hwid.as_str())
                .header("x-device-os", device.os.as_str())
                .header("x-ver-os", device.os_version.as_str())
                .header("x-device-model", device.model.as_str());
        }
        // The request-level timeout overrides the client's: the caller
        // decides how long *this* subscription may take.
        builder = builder.timeout(request.timeout);

        let response = builder
            .send()
            .await
            .map_err(|error| ProviderError::Network(error.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(ProviderError::Unauthorized {
                status: status.as_u16(),
            });
        }
        if !status.is_success() {
            return Err(ProviderError::Status {
                status: status.as_u16(),
            });
        }

        // The headers are read before the body so a `subscription-userinfo`
        // survives even a body that turns out to be unreadable.
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_string(), value.to_string()))
            })
            .collect();

        let body = response
            .bytes()
            .await
            .map_err(|error| ProviderError::Body(error.to_string()))?;

        Ok(FetchResponse {
            body: body.to_vec(),
            headers,
        })
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::Http
    }
}

impl HttpSubscriptionProvider {
    /// The redirect bound this provider was built with.
    pub fn max_redirects(&self) -> usize {
        self.max_redirects
    }
}

/// A provider that returns a fixed body.
///
/// This is the mock the tests use, and it is part of the public API on
/// purpose: an embedder that wants to test its own refresh logic without
/// a server needs the same thing.
#[derive(Debug, Clone)]
pub struct StaticSubscriptionProvider {
    response: FetchResponse,
    kind: ProviderKind,
}

impl StaticSubscriptionProvider {
    /// A provider that always answers with this body.
    pub fn new(body: impl Into<Vec<u8>>) -> Self {
        Self {
            response: FetchResponse::new(body),
            kind: ProviderKind::Http,
        }
    }

    /// A provider that always answers with this response.
    pub fn with_response(response: FetchResponse) -> Self {
        Self {
            response,
            kind: ProviderKind::Http,
        }
    }

    /// The same provider, answering with a different body each call.
    pub fn with_kind(mut self, kind: ProviderKind) -> Self {
        self.kind = kind;
        self
    }
}

#[async_trait]
impl SubscriptionProvider for StaticSubscriptionProvider {
    async fn fetch(&self, _request: &FetchRequest) -> Result<FetchResponse, ProviderError> {
        Ok(self.response.clone())
    }

    fn kind(&self) -> ProviderKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_header_is_found_case_insensitively() {
        let response = FetchResponse::new(b"body".to_vec())
            .with_header("Subscription-Userinfo", "upload=1; total=2");
        assert_eq!(
            response.userinfo(),
            Some("upload=1; total=2"),
            "the header name matches case-insensitively, as the engine's own lookup does"
        );
    }

    #[test]
    fn the_first_matching_header_wins() {
        let response = FetchResponse::new(b"body".to_vec())
            .with_header("X-Test", "first")
            .with_header("x-test", "second");
        assert_eq!(response.header("X-TEST"), Some("first"));
    }

    #[test]
    fn a_response_without_the_header_reports_none() {
        let response = FetchResponse::new(b"body".to_vec());
        assert_eq!(response.userinfo(), None);
    }

    #[tokio::test]
    async fn a_static_provider_returns_its_bytes() {
        let provider = StaticSubscriptionProvider::new(b"proxies: []\n".to_vec());
        let response = provider
            .fetch(&FetchRequest::new("https://example.invalid/sub"))
            .await
            .expect("the static provider never fails");
        assert_eq!(response.body, b"proxies: []\n");
        assert_eq!(provider.kind(), ProviderKind::Http);
    }

    #[tokio::test]
    async fn a_static_provider_carries_its_headers() {
        let provider = StaticSubscriptionProvider::with_response(
            FetchResponse::new(b"x".to_vec()).with_header("subscription-userinfo", "total=10"),
        );
        let response = provider
            .fetch(&FetchRequest::new("https://example.invalid/sub"))
            .await
            .expect("the static provider never fails");
        assert_eq!(response.userinfo(), Some("total=10"));
    }

    #[test]
    fn a_fetch_request_carries_its_authorization_and_timeout() {
        let request = FetchRequest::new("https://example.invalid/sub")
            .with_authorization("Bearer abc")
            .with_timeout(Duration::from_secs(5));
        assert_eq!(request.authorization.as_deref(), Some("Bearer abc"));
        assert_eq!(request.timeout, Duration::from_secs(5));
    }

    #[test]
    fn the_default_request_timeout_is_thirty_seconds() {
        assert_eq!(
            FetchRequest::new("https://example.invalid/sub").timeout,
            Duration::from_secs(30)
        );
    }

    #[test]
    fn the_provider_kind_has_a_stable_label() {
        assert_eq!(ProviderKind::Http.as_str(), "http");
        assert_eq!(ProviderKind::LocalFile.as_str(), "local-file");
    }

    #[test]
    fn the_http_provider_reports_its_kind() {
        let provider = HttpSubscriptionProvider::new().expect("the client builds");
        assert_eq!(provider.kind(), ProviderKind::Http);
        assert_eq!(provider.max_redirects(), 5);
    }
}
