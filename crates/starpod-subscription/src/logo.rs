//! The profile logo: a provider's picture, downloaded within limits.
//!
//! A subscription may name its logo in the `profile-logo` header. The app
//! fetches it once per successful refresh, keeps the bytes next to the cached
//! payload, and shows it on the Home page. Any failure here is ignored by the
//! caller: a missing logo never blocks a subscription.
//!
//! # The limits
//!
//! - At most [`LOGO_MAX_BYTES`] (512 KiB) are read. A larger logo is refused,
//!   and so is one whose `Content-Length` says it is larger.
//! - The whole download is bounded by [`LOGO_TIMEOUT`] (10 seconds).
//! - Only PNG, JPEG and SVG are kept. A format is recognised by its magic
//!   bytes, or by a supported `Content-Type` when the bytes are not sniffable.
//! - No `Authorization` header is sent: the logo host is not the subscription.

use std::time::Duration;

use thiserror::Error;

/// The most logo bytes the app reads.
pub const LOGO_MAX_BYTES: usize = 512 * 1024;

/// The bound on the whole logo download.
pub const LOGO_TIMEOUT: Duration = Duration::from_secs(10);

/// An image format the app keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogoFormat {
    /// A PNG image.
    Png,
    /// A JPEG image.
    Jpeg,
    /// An SVG image.
    Svg,
}

impl LogoFormat {
    /// The file extension the logo is cached under.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Svg => "svg",
        }
    }

    /// The format a `Content-Type` names, if it is one the app keeps.
    fn from_content_type(value: &str) -> Option<Self> {
        let media_type = value.split(';').next().unwrap_or_default().trim();
        if media_type.eq_ignore_ascii_case("image/png") {
            Some(Self::Png)
        } else if media_type.eq_ignore_ascii_case("image/jpeg")
            || media_type.eq_ignore_ascii_case("image/jpg")
        {
            Some(Self::Jpeg)
        } else if media_type.eq_ignore_ascii_case("image/svg+xml") {
            Some(Self::Svg)
        } else {
            None
        }
    }
}

/// Recognise a supported image by its leading bytes.
pub fn sniff(bytes: &[u8]) -> Option<LogoFormat> {
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.starts_with(&PNG_SIGNATURE) {
        return Some(LogoFormat::Png);
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return Some(LogoFormat::Jpeg);
    }
    let head = &bytes[..bytes.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    if trimmed.starts_with("<svg") || (trimmed.starts_with("<?xml") && text.contains("<svg")) {
        return Some(LogoFormat::Svg);
    }
    None
}

/// A downloaded logo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Logo {
    /// The recognised format.
    pub format: LogoFormat,
    /// The image bytes, at most [`LOGO_MAX_BYTES`].
    pub bytes: Vec<u8>,
}

/// Why a logo could not be kept. The caller ignores every variant.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LogoError {
    /// The download failed, or the server answered with a non-success status.
    #[error("the logo could not be downloaded: {0}")]
    Network(String),
    /// The logo is larger than [`LOGO_MAX_BYTES`].
    #[error("the logo is larger than 512 KiB")]
    TooLarge,
    /// The bytes are not a PNG, JPEG or SVG image.
    #[error("the logo is not a PNG, JPEG or SVG image")]
    UnsupportedFormat,
}

/// Download a logo with the app's limits.
pub async fn fetch_logo(url: &str) -> Result<Logo, LogoError> {
    let client = reqwest::Client::builder()
        .timeout(LOGO_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| LogoError::Network(error.to_string()))?;
    fetch_logo_with(&client, url).await
}

/// Download a logo with a given client. The client's own timeout applies.
pub async fn fetch_logo_with(client: &reqwest::Client, url: &str) -> Result<Logo, LogoError> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| LogoError::Network(error.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        return Err(LogoError::Network(format!("HTTP {}", status.as_u16())));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if response
        .content_length()
        .is_some_and(|length| length > LOGO_MAX_BYTES as u64)
    {
        return Err(LogoError::TooLarge);
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| LogoError::Network(error.to_string()))?
    {
        if bytes.len() + chunk.len() > LOGO_MAX_BYTES {
            return Err(LogoError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }

    let format = sniff(&bytes)
        .or_else(|| {
            content_type
                .as_deref()
                .and_then(LogoFormat::from_content_type)
        })
        .ok_or(LogoError::UnsupportedFormat)?;
    Ok(Logo { format, bytes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::path;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const PNG_HEAD: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];

    #[test]
    fn magic_bytes_identify_png_jpeg_and_svg() {
        assert_eq!(sniff(PNG_HEAD), Some(LogoFormat::Png));
        assert_eq!(sniff(&[0xff, 0xd8, 0xff, 0xe0]), Some(LogoFormat::Jpeg));
        assert_eq!(sniff(b"<svg xmlns=\"x\"/>"), Some(LogoFormat::Svg));
        assert_eq!(
            sniff(b"\xef\xbb\xbf<?xml version=\"1.0\"?><svg/>"),
            Some(LogoFormat::Svg),
            "a byte-order mark and an XML prolog do not hide the SVG"
        );
        assert_eq!(sniff(b"<html><body>no</body></html>"), None);
        assert_eq!(sniff(b""), None);
    }

    #[test]
    fn a_supported_content_type_names_its_format() {
        assert_eq!(
            LogoFormat::from_content_type("image/png"),
            Some(LogoFormat::Png)
        );
        assert_eq!(
            LogoFormat::from_content_type("IMAGE/JPG; charset=x"),
            Some(LogoFormat::Jpeg)
        );
        assert_eq!(
            LogoFormat::from_content_type("image/svg+xml"),
            Some(LogoFormat::Svg)
        );
        assert_eq!(LogoFormat::from_content_type("image/gif"), None);
        assert_eq!(LogoFormat::from_content_type("text/html"), None);
    }

    #[tokio::test]
    async fn a_png_logo_is_downloaded_and_kept() {
        let server = MockServer::start().await;
        Mock::given(path("/logo.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(PNG_HEAD.to_vec()))
            .mount(&server)
            .await;

        let logo = fetch_logo(&format!("{}/logo.png", server.uri()))
            .await
            .expect("a PNG is accepted");
        assert_eq!(logo.format, LogoFormat::Png);
        assert_eq!(logo.bytes, PNG_HEAD);
    }

    #[tokio::test]
    async fn an_svg_is_accepted_by_its_content_type_when_its_bytes_do_not_sniff() {
        let server = MockServer::start().await;
        Mock::given(path("/logo"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "image/svg+xml")
                    .set_body_bytes(b"not sniffable but declared".to_vec()),
            )
            .mount(&server)
            .await;

        let logo = fetch_logo(&format!("{}/logo", server.uri()))
            .await
            .expect("the declared SVG is accepted");
        assert_eq!(logo.format, LogoFormat::Svg);
    }

    #[tokio::test]
    async fn a_non_image_is_refused() {
        let server = MockServer::start().await;
        Mock::given(path("/logo"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/html")
                    .set_body_bytes(b"<html>login</html>".to_vec()),
            )
            .mount(&server)
            .await;

        let error = fetch_logo(&format!("{}/logo", server.uri()))
            .await
            .expect_err("a web page is not a logo");
        assert_eq!(error, LogoError::UnsupportedFormat);
    }

    #[tokio::test]
    async fn a_logo_over_the_limit_is_refused() {
        let server = MockServer::start().await;
        let mut body = PNG_HEAD.to_vec();
        body.resize(LOGO_MAX_BYTES + 1, 0);
        Mock::given(path("/big.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let error = fetch_logo(&format!("{}/big.png", server.uri()))
            .await
            .expect_err("a logo over 512 KiB is refused");
        assert_eq!(error, LogoError::TooLarge);
    }

    #[tokio::test]
    async fn a_logo_exactly_at_the_limit_is_accepted() {
        let server = MockServer::start().await;
        let mut body = PNG_HEAD.to_vec();
        body.resize(LOGO_MAX_BYTES, 0);
        Mock::given(path("/edge.png"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .mount(&server)
            .await;

        let logo = fetch_logo(&format!("{}/edge.png", server.uri()))
            .await
            .expect("the limit is inclusive");
        assert_eq!(logo.bytes.len(), LOGO_MAX_BYTES);
    }

    #[tokio::test]
    async fn an_error_status_is_a_network_failure() {
        let server = MockServer::start().await;
        Mock::given(path("/gone.png"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let error = fetch_logo(&format!("{}/gone.png", server.uri()))
            .await
            .expect_err("a 404 has no logo");
        assert!(matches!(error, LogoError::Network(_)));
    }
}
