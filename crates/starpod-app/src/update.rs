//! The update check: ask GitHub for the latest release and compare its tag
//! with the running version (ARCHITECTURE §6.4).
//!
//! The check only reads. It never downloads or installs anything; the UI
//! offers the release page and the user decides.

use std::time::Duration;

use reqwest::header::{ACCEPT, USER_AGENT};
use semver::Version;
use serde::Deserialize;

/// The GitHub repository the releases come from. Hard-coded: the update check
/// must not follow a link a settings file could change.
pub const RELEASE_REPO: &str = "foxgirlsorg/starpod";

/// The endpoint for the newest published (non-draft, non-prerelease) release.
pub const LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/foxgirlsorg/starpod/releases/latest";

/// The version this binary was built as.
pub const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// How long the request may take before the check gives up.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// A release the check found: its version without the leading `v`, and the
/// page to show the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The version, for example `0.2.0`.
    pub version: String,
    /// The release's web page (`html_url`).
    pub url: String,
}

/// The fields of the GitHub response the check reads.
#[derive(Deserialize)]
struct ReleaseBody {
    tag_name: String,
    html_url: String,
}

/// Parse a version from a release tag. A leading `v` is allowed. The tag must
/// be a full semver version (`1.2.3`, optionally with a pre-release part).
pub fn parse_version(tag: &str) -> Result<Version, String> {
    let text = tag.trim().trim_start_matches('v');
    Version::parse(text).map_err(|error| format!("“{tag}” is not a version: {error}"))
}

/// Whether `latest` is a newer version than `current`. Both may carry a
/// leading `v`. A pre-release is older than its release (`0.2.0-rc1` is older
/// than `0.2.0`), as semver defines it.
pub fn is_newer(latest: &str, current: &str) -> Result<bool, String> {
    Ok(parse_version(latest)? > parse_version(current)?)
}

/// Read a release from the GitHub response body.
pub fn parse_release(body: &str) -> Result<Release, String> {
    let parsed: ReleaseBody = serde_json::from_str(body)
        .map_err(|error| format!("the release could not be read: {error}"))?;
    let version = parse_version(&parsed.tag_name)?.to_string();
    Ok(Release {
        version,
        url: parsed.html_url,
    })
}

/// Fetch the latest release from GitHub. The request carries a User-Agent,
/// which the GitHub API requires.
pub async fn fetch_latest_release() -> Result<Release, String> {
    let client = reqwest::Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| format!("the request could not be made: {error}"))?;
    let response = client
        .get(LATEST_RELEASE_URL)
        .header(USER_AGENT, format!("starpod/{CURRENT_VERSION}"))
        .header(ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|error| format!("GitHub could not be reached: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("GitHub answered {status}"));
    }
    let body = response
        .text()
        .await
        .map_err(|error| format!("the answer could not be read: {error}"))?;
    parse_release(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leading_v_is_ignored_when_comparing() {
        assert_eq!(is_newer("v0.2.0", "0.1.0"), Ok(true));
        assert_eq!(is_newer("0.2.0", "v0.1.0"), Ok(true));
        assert_eq!(is_newer("v0.1.0", "0.1.0"), Ok(false));
    }

    #[test]
    fn an_older_or_equal_release_is_not_newer() {
        assert_eq!(is_newer("0.1.0", "0.2.0"), Ok(false));
        assert_eq!(is_newer("0.1.0", "0.1.0"), Ok(false));
    }

    #[test]
    fn each_part_is_compared_as_a_number() {
        assert_eq!(is_newer("0.10.0", "0.9.0"), Ok(true));
        assert_eq!(is_newer("1.0.0", "0.99.99"), Ok(true));
        assert_eq!(is_newer("0.1.1", "0.1.0"), Ok(true));
    }

    #[test]
    fn a_prerelease_is_older_than_its_release() {
        assert_eq!(is_newer("0.2.0-rc1", "0.1.0"), Ok(true));
        assert_eq!(is_newer("0.2.0-rc1", "0.2.0"), Ok(false));
        assert_eq!(is_newer("0.2.0", "0.2.0-rc1"), Ok(true));
    }

    #[test]
    fn a_tag_that_is_not_a_version_is_an_error() {
        assert!(is_newer("latest", "0.1.0").is_err());
        assert!(is_newer("v1.2", "0.1.0").is_err());
        assert!(is_newer("0.2.0", "not-a-version").is_err());
    }

    #[test]
    fn a_release_body_gives_its_version_and_page() {
        let body = r#"{
            "tag_name": "v0.3.1",
            "html_url": "https://github.com/foxgirlsorg/starpod/releases/tag/v0.3.1",
            "name": "ignored",
            "assets": []
        }"#;
        let release = parse_release(body).unwrap();
        assert_eq!(release.version, "0.3.1");
        assert_eq!(
            release.url,
            "https://github.com/foxgirlsorg/starpod/releases/tag/v0.3.1"
        );
    }

    #[test]
    fn a_release_body_without_a_tag_is_an_error() {
        assert!(parse_release(r#"{"html_url": "https://example.test"}"#).is_err());
        assert!(parse_release("not json").is_err());
    }
}
