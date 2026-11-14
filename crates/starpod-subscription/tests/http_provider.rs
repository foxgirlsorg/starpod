//! The HTTP provider, exercised against a real (local) HTTP server.
//!
//! These tests use `wiremock` to stand up a server on the loopback
//! interface. Nothing here touches the public internet: the URL is always
//! the mock server's own address.
//!
//! The point of these tests is the part the mock provider cannot check:
//! that the header actually arrives, that a `401` is reported as
//! *unauthorised* rather than as a generic status, and that the
//! `Authorization` header is sent when one was asked for.

use starpod_subscription::error::ProviderError;
use starpod_subscription::provider::{
    FetchRequest, HttpSubscriptionProvider, SubscriptionProvider,
};
use wiremock::matchers::{header, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BODY: &str = "proxies:\n  - { name: A, type: http, server: a.example.com, port: 8080 }\n";

/// A provider pointed at the mock server, with a short timeout.
fn provider() -> HttpSubscriptionProvider {
    HttpSubscriptionProvider::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .expect("the client builds")
}

#[tokio::test]
async fn a_successful_fetch_returns_the_body() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .mount(&server)
        .await;

    let provider = provider();
    let response = provider
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())))
        .await
        .expect("the fetch succeeds");

    assert_eq!(response.body, BODY.as_bytes());
}

#[tokio::test]
async fn the_userinfo_header_is_surfaced() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(BODY)
                .insert_header(
                    "subscription-userinfo",
                    "upload=455727941; download=6174315083; total=1073741824000; expire=1798675200",
                ),
        )
        .mount(&server)
        .await;

    let provider = provider();
    let response = provider
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())))
        .await
        .expect("the fetch succeeds");

    assert_eq!(
        response.userinfo(),
        Some("upload=455727941; download=6174315083; total=1073741824000; expire=1798675200"),
        "the header the engine's own fetcher discards must reach the app"
    );
}

#[tokio::test]
async fn a_response_without_the_header_reports_none() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .mount(&server)
        .await;

    let provider = provider();
    let response = provider
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())))
        .await
        .expect("the fetch succeeds");

    assert_eq!(response.userinfo(), None);
}

#[tokio::test]
async fn the_authorization_header_is_sent_when_one_was_asked_for() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .and(header("authorization", "Bearer secret-token"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .mount(&server)
        .await;

    let provider = provider();
    let request = FetchRequest::new(format!("{}/sub", server.uri()))
        .with_authorization("Bearer secret-token");
    provider
        .fetch(&request)
        .await
        .expect("the fetch succeeds — the mock only matches with the header");

    let requests = server
        .received_requests()
        .await
        .expect("requests were recorded");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]
            .headers
            .get("authorization")
            .map(|value| value.to_str().unwrap_or_default()),
        Some("Bearer secret-token")
    );
}

#[tokio::test]
async fn a_url_carrying_a_token_is_fetched_verbatim() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .mount(&server)
        .await;

    let provider = provider();
    let url = format!("{}/sub?token=abc123&uuid=xyz", server.uri());
    provider
        .fetch(&FetchRequest::new(url))
        .await
        .expect("the fetch succeeds");

    let requests = server
        .received_requests()
        .await
        .expect("requests were recorded");
    assert_eq!(
        requests[0].url.query(),
        Some("token=abc123&uuid=xyz"),
        "the app must not rewrite a URL that carries the token"
    );
}

#[tokio::test]
async fn a_401_is_reported_as_unauthorised() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let provider = provider();
    let error = provider
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())))
        .await
        .expect_err("the fetch fails");

    match error {
        ProviderError::Unauthorized { status } => assert_eq!(status, 401),
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(
        error.is_fatal(),
        "an expired token is not a transient fault"
    );
}

#[tokio::test]
async fn a_403_is_reported_as_unauthorised() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let provider = provider();
    let error = provider
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())))
        .await
        .expect_err("the fetch fails");

    match error {
        ProviderError::Unauthorized { status } => assert_eq!(status, 403),
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn a_500_is_reported_as_a_plain_status() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let provider = provider();
    let error = provider
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())))
        .await
        .expect_err("the fetch fails");

    match error {
        ProviderError::Status { status } => assert_eq!(status, 500),
        other => panic!("unexpected error: {other:?}"),
    }
    assert!(!error.is_fatal(), "a server error is transient");
}

#[tokio::test]
async fn a_connection_failure_is_a_network_error() {
    // Port 1 on the loopback interface: nothing listens there.
    let provider = provider();
    let error = provider
        .fetch(&FetchRequest::new("http://127.0.0.1:1/sub"))
        .await
        .expect_err("the fetch fails");

    assert!(
        matches!(error, ProviderError::Network(_)),
        "unexpected error: {error:?}"
    );
    assert!(!error.is_fatal());
}

#[tokio::test]
async fn a_redirect_is_followed() {
    let server = MockServer::start().await;
    Mock::given(path("/old"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/new"))
        .mount(&server)
        .await;
    Mock::given(path("/new"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .mount(&server)
        .await;

    let provider = provider();
    let response = provider
        .fetch(&FetchRequest::new(format!("{}/old", server.uri())))
        .await
        .expect("the fetch succeeds");

    assert_eq!(response.body, BODY.as_bytes());
}

fn device() -> starpod_subscription::DeviceHeaders {
    starpod_subscription::DeviceHeaders {
        hwid: "0123456789abcdef0123456789abcdef".to_string(),
        os: "Windows".to_string(),
        os_version: "10.0.26200".to_string(),
        model: "x86_64".to_string(),
    }
}

#[tokio::test]
async fn a_request_with_device_headers_carries_all_four_of_them() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .and(header("x-hwid", "0123456789abcdef0123456789abcdef"))
        .and(header("x-device-os", "Windows"))
        .and(header("x-ver-os", "10.0.26200"))
        .and(header("x-device-model", "x86_64"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .expect(1)
        .mount(&server)
        .await;

    let response = provider()
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())).with_device(device()))
        .await
        .expect("the request matched every device header");
    assert_eq!(response.body, BODY.as_bytes());
}

#[tokio::test]
async fn a_request_without_device_headers_sends_none_of_them() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .mount(&server)
        .await;

    provider()
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())))
        .await
        .expect("the fetch succeeds");

    let received = server.received_requests().await.expect("recording is on");
    let request = received.last().expect("one request arrived");
    for name in ["x-hwid", "x-device-os", "x-ver-os", "x-device-model"] {
        assert!(
            request.headers.get(name).is_none(),
            "{name} must be omitted when device identification is off"
        );
    }
}

#[tokio::test]
async fn the_user_agent_is_sent_on_every_fetch() {
    let server = MockServer::start().await;
    Mock::given(path("/sub"))
        .respond_with(ResponseTemplate::new(200).set_body_string(BODY))
        .mount(&server)
        .await;

    provider()
        .fetch(&FetchRequest::new(format!("{}/sub", server.uri())))
        .await
        .expect("the fetch succeeds");

    let received = server.received_requests().await.expect("recording is on");
    let agent = received
        .last()
        .and_then(|request| request.headers.get("user-agent"))
        .and_then(|value| value.to_str().ok())
        .expect("a user agent is sent");
    assert!(agent.starts_with("starpod/"), "{agent}");
}
