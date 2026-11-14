//! Deep links that ask Starpod to add a subscription.
//!
//! The shape follows Koala Clash and Clash Verge:
//!
//! ```text
//! starpod://install-config?url=<percent-encoded https address>&name=<optional name>
//! ```
//!
//! The schemes `clash`, `mihomo` and `koala-clash` are accepted too, so a
//! link written for another client also works when it is given on the
//! command line. Only `starpod` is registered with the operating system
//! (see `scheme.rs`), so the other schemes reach this app only when a user
//! or another program passes them on purpose.
//!
//! The `url` parameter is decoded once, by the URL parser. A `+` in the query
//! is read as a space, as in a form, so a literal plus sign in the address
//! must be sent as `%2B`.

use url::Url;

/// The schemes a deep link may use. Matching is case-insensitive.
pub const SCHEMES: [&str; 4] = ["starpod", "clash", "mihomo", "koala-clash"];

/// The only action a deep link may ask for.
const ACTION_HOST: &str = "install-config";

/// A request to add a subscription, read from a deep link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallRequest {
    /// The subscription address, trimmed and checked to be http or https.
    /// It is a secret (it usually carries the subscriber's token), so the
    /// UI shows only [`InstallRequest::host`].
    pub url: String,
    /// The name the link suggests, if it gave one.
    pub name: Option<String>,
}

impl InstallRequest {
    /// The host of the subscription address, the only part the UI shows.
    pub fn host(&self) -> String {
        Url::parse(&self.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_string))
            .unwrap_or_default()
    }

    /// The label the subscription gets: the suggested name, or else the host.
    pub fn label(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.host())
    }
}

/// Why a deep link was refused. None of these messages repeats the link.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeepLinkError {
    /// The text is not a URL at all.
    #[error("the link is not a valid address")]
    Malformed,
    /// The scheme is not one Starpod accepts.
    #[error("this is not a Starpod link")]
    UnsupportedScheme,
    /// The host or path names an action Starpod does not have.
    #[error("the link asks for an action Starpod does not know")]
    UnknownAction,
    /// The link has no `url` parameter, or it is empty.
    #[error("the link has no subscription address")]
    MissingUrl,
    /// The subscription address is not an http or https address with a host.
    #[error("the subscription address must be an http:// or https:// address with a host")]
    InvalidAddress,
}

/// Read a deep link into an install request.
pub fn parse(text: &str) -> Result<InstallRequest, DeepLinkError> {
    let link = Url::parse(text.trim()).map_err(|_| DeepLinkError::Malformed)?;
    if !SCHEMES.contains(&link.scheme()) {
        return Err(DeepLinkError::UnsupportedScheme);
    }
    let is_action = link
        .host_str()
        .is_some_and(|host| host.eq_ignore_ascii_case(ACTION_HOST));
    if !is_action || !matches!(link.path(), "" | "/") {
        return Err(DeepLinkError::UnknownAction);
    }

    let mut address = None;
    let mut name = None;
    for (key, value) in link.query_pairs() {
        match key.as_ref() {
            "url" if address.is_none() => address = Some(value.into_owned()),
            "name" if name.is_none() => name = Some(value.into_owned()),
            _ => {}
        }
    }

    let address = address
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or(DeepLinkError::MissingUrl)?;
    let parsed = Url::parse(&address).map_err(|_| DeepLinkError::InvalidAddress)?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(DeepLinkError::InvalidAddress);
    }

    let name = name
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(InstallRequest { url: address, name })
}

/// Whether a command-line argument is a deep link of a supported scheme.
pub fn is_deep_link(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    SCHEMES.iter().any(|scheme| {
        lower
            .strip_prefix(scheme)
            .is_some_and(|rest| rest.starts_with("://"))
    })
}

/// The first argument that is a deep link, if there is one.
pub fn find_in_args<I>(args: I) -> Option<String>
where
    I: IntoIterator<Item = String>,
{
    args.into_iter().find(|arg| is_deep_link(arg))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUB: &str = "https%3A%2F%2Fexample.com%2Fsub";

    #[test]
    fn a_clash_style_link_reads_the_address_and_the_name() {
        let link = format!("clash://install-config?url={SUB}&name=Work");
        let request = parse(&link).unwrap();
        assert_eq!(request.url, "https://example.com/sub");
        assert_eq!(request.name.as_deref(), Some("Work"));
        assert_eq!(request.host(), "example.com");
        assert_eq!(request.label(), "Work");
    }

    #[test]
    fn every_supported_scheme_is_accepted_in_any_case() {
        for scheme in [
            "starpod",
            "clash",
            "mihomo",
            "koala-clash",
            "STARPOD",
            "Koala-Clash",
        ] {
            let link = format!("{scheme}://install-config?url={SUB}");
            assert!(parse(&link).is_ok(), "{scheme} must be accepted");
        }
    }

    #[test]
    fn a_trailing_slash_is_accepted() {
        let link = format!("starpod://install-config/?url={SUB}");
        assert_eq!(parse(&link).unwrap().url, "https://example.com/sub");
    }

    #[test]
    fn the_name_is_optional_and_an_empty_name_is_none() {
        let without = parse(&format!("starpod://install-config?url={SUB}")).unwrap();
        assert_eq!(without.name, None);
        assert_eq!(without.label(), "example.com");

        let empty = parse(&format!("starpod://install-config?url={SUB}&name=%20%20")).unwrap();
        assert_eq!(empty.name, None);
    }

    #[test]
    fn the_address_is_decoded_once() {
        // %2520 is a literal "%20" in the address, not a space.
        let link = "starpod://install-config?url=https%3A%2F%2Fexample.com%2Fsub%3Ft%3Da%2520b";
        assert_eq!(parse(link).unwrap().url, "https://example.com/sub?t=a%20b");
    }

    #[test]
    fn other_hosts_and_schemes_are_refused() {
        assert_eq!(
            parse(&format!("starpod://open?url={SUB}")),
            Err(DeepLinkError::UnknownAction)
        );
        assert_eq!(
            parse(&format!("starpod://install-config/extra?url={SUB}")),
            Err(DeepLinkError::UnknownAction)
        );
        assert_eq!(
            parse(&format!("https://install-config?url={SUB}")),
            Err(DeepLinkError::UnsupportedScheme)
        );
        assert_eq!(
            parse("file://install-config?url=x"),
            Err(DeepLinkError::UnsupportedScheme)
        );
    }

    #[test]
    fn a_missing_or_empty_address_is_refused() {
        assert_eq!(
            parse("starpod://install-config"),
            Err(DeepLinkError::MissingUrl)
        );
        assert_eq!(
            parse("starpod://install-config?url="),
            Err(DeepLinkError::MissingUrl)
        );
        assert_eq!(
            parse("starpod://install-config?name=Work"),
            Err(DeepLinkError::MissingUrl)
        );
    }

    #[test]
    fn only_web_addresses_are_accepted() {
        for address in [
            "javascript%3Aalert(1)",
            "file%3A%2F%2F%2FC%3A%2Fsecret.txt",
            "ftp%3A%2F%2Fexample.com%2Fsub",
            "https%3A%2F%2F",
            "not-a-url",
        ] {
            let link = format!("starpod://install-config?url={address}");
            assert_eq!(
                parse(&link),
                Err(DeepLinkError::InvalidAddress),
                "{address} must be refused"
            );
        }
    }

    #[test]
    fn text_that_is_not_a_url_is_malformed() {
        assert_eq!(parse("starpod:// install"), Err(DeepLinkError::Malformed));
        assert_eq!(parse(""), Err(DeepLinkError::Malformed));
    }

    #[test]
    fn the_first_deep_link_argument_is_found_and_others_are_ignored() {
        let args = [
            "--verbose",
            "C:\\Program Files\\starpod.exe",
            "CLASH://install-config?url=x",
            "starpod://install-config?url=y",
        ]
        .map(str::to_string);
        assert_eq!(
            find_in_args(args),
            Some("CLASH://install-config?url=x".to_string())
        );
        assert_eq!(find_in_args(["--show".to_string()]), None);
    }

    #[test]
    fn a_path_that_only_mentions_a_scheme_is_not_a_deep_link() {
        assert!(!is_deep_link("starpodx://install-config"));
        assert!(!is_deep_link("https://starpod://x"));
        assert!(is_deep_link("mihomo://install-config?url=x"));
    }
}
