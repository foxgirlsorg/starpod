//! Redaction: making a secret safe to log or display.
//!
//! GOAL.md §6 requires the app to avoid sensitive information in logs, and
//! ARCHITECTURE §5 says "Redaction is a filter, not a formatting convention.
//! A single `redact()` helper is applied at the logging boundary and to
//! anything displayed, so a new call site cannot accidentally leak a token".
//!
//! This module is that filter. It is deliberately *not* a trait on the
//! secret type — a trait method would have to be remembered at every call
//! site, and forgetting it is exactly the failure the design is trying to
//! prevent. Instead the secret store hands out opaque byte strings and
//! every path that turns one into text goes through [`redact_url`],
//! [`redact_secret`] or [`redact_error`].

/// The placeholder that stands in for a redacted value.
pub const REDACTED: &str = "***";

/// Redact a URL that may carry a token.
///
/// The scheme, the host and the path survive — they are what makes the
/// message useful ("could not reach `example.com/api/v1`") — but the query
/// string and the fragment are replaced wholesale, because that is where
/// subscription tokens live (`?token=…`, `?sub=…`).
///
/// A URL that cannot be parsed is replaced entirely: a half-redacted
/// credential is worse than no credential, because it looks safe.
pub fn redact_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let mut out = format!(
                "{}://{}{}",
                parsed.scheme(),
                parsed.host_str().unwrap_or(""),
                parsed.path()
            );
            if parsed.query().is_some() || parsed.fragment().is_some() {
                out.push('?');
                out.push_str(REDACTED);
            }
            out
        }
        Err(_) => format!("<redacted url: {REDACTED}>"),
    }
}

/// Redact an opaque secret.
///
/// The length is *not* reported: the length of a token is itself a small
/// piece of information about it, and there is no need for it.
pub fn redact_secret(_secret: &[u8]) -> String {
    REDACTED.to_string()
}

/// Redact any embedded URL or token in an error message.
///
/// Error messages from I/O and from parsers routinely quote the thing that
/// failed — a malformed subscription URL, a header that did not parse. Those
/// messages end up in the Logs screen, so they go through here.
///
/// The filter is conservative: it replaces anything that looks like a
/// `scheme://…` run of non-whitespace, and anything after a `token=`,
/// `key=`, `secret=` or `password=` marker.
pub fn redact_error(message: &str) -> String {
    let mut out = String::with_capacity(message.len());
    let mut rest = message;

    while let Some(index) = find_url(rest) {
        out.push_str(&rest[..index]);
        let tail = &rest[index..];
        let end = tail.find(char::is_whitespace).unwrap_or(tail.len());
        out.push_str(&redact_url(&tail[..end]));
        rest = &tail[end..];
    }
    out.push_str(rest);
    redact_keyed_values(&out)
}

/// Find the start of a URL-looking run of text.
fn find_url(text: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b':' {
            // `scheme://` — the scheme is [a-zA-Z][a-zA-Z0-9+.-]*
            let start = index;
            let mut cursor = index;
            while cursor > 0 {
                let c = bytes[cursor - 1];
                if c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.' {
                    cursor -= 1;
                } else {
                    break;
                }
            }
            let scheme = &text[cursor..start];
            let is_scheme = !scheme.is_empty()
                && scheme.as_bytes()[0].is_ascii_alphabetic()
                && scheme
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.');
            if is_scheme && text[index + 1..].starts_with("//") {
                return Some(cursor);
            }
            index += 1;
        } else {
            index += 1;
        }
    }
    None
}

/// Replace `key=value` pairs whose key names a credential.
fn redact_keyed_values(text: &str) -> String {
    const MARKERS: [&str; 5] = ["token=", "secret=", "password=", "apikey=", "api_key="];
    let mut out = text.to_string();
    for marker in MARKERS {
        let mut search_from = 0usize;
        while let Some(found) = out[search_from..].find(marker) {
            let value_start = search_from + found + marker.len();
            let value_end = out[value_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'')
                .map(|offset| value_start + offset)
                .unwrap_or(out.len());
            out.replace_range(value_start..value_end, REDACTED);
            search_from = value_start + REDACTED.len();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_keeps_its_scheme_host_and_path() {
        let out = redact_url("https://example.com/api/v1/client/subscription");
        assert_eq!(out, "https://example.com/api/v1/client/subscription");
    }

    #[test]
    fn a_url_query_is_replaced() {
        let out = redact_url("https://example.com/sub?token=supersecret");
        assert!(!out.contains("supersecret"), "the token survived: {out}");
        assert_eq!(out, "https://example.com/sub?***");
    }

    #[test]
    fn a_url_fragment_is_replaced() {
        let out = redact_url("https://example.com/sub#supersecret");
        assert!(!out.contains("supersecret"), "the fragment survived: {out}");
    }

    #[test]
    fn a_url_with_both_query_and_fragment_is_replaced_once() {
        let out = redact_url("https://example.com/sub?token=a#b");
        assert!(!out.contains("token="), "{out}");
        assert!(!out.contains("secret"), "{out}");
    }

    #[test]
    fn a_malformed_url_is_replaced_entirely() {
        let out = redact_url("https://exa mple.com/?token=abc");
        assert!(!out.contains("abc"), "{out}");
        assert!(out.contains(REDACTED));
    }

    #[test]
    fn a_secret_is_replaced_without_revealing_its_length() {
        let out = redact_secret(b"0123456789abcdef");
        assert_eq!(out, REDACTED);
    }

    #[test]
    fn an_error_message_with_a_url_is_redacted() {
        let out = redact_error("failed to fetch https://example.com/sub?token=abc123");
        assert!(!out.contains("abc123"), "the token survived: {out}");
        assert!(out.contains("example.com"));
    }

    #[test]
    fn an_error_message_with_a_keyed_value_is_redacted() {
        let out = redact_error("the request had token=abc123 and failed");
        assert!(!out.contains("abc123"), "the token survived: {out}");
    }

    #[test]
    fn an_error_message_with_a_password_is_redacted() {
        let out = redact_error("password=hunter2 rejected");
        assert!(!out.contains("hunter2"), "the password survived: {out}");
    }

    #[test]
    fn a_message_without_secrets_is_unchanged() {
        let msg = "the file could not be read: permission denied";
        assert_eq!(redact_error(msg), msg);
    }

    #[test]
    fn a_plain_word_containing_a_colon_is_not_treated_as_a_url() {
        let msg = "error: something went wrong";
        assert_eq!(redact_error(msg), msg);
    }
}
