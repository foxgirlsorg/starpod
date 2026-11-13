//! The engine's log stream.
//!
//! The engine's stdout and stderr are piped and drained into a bounded ring
//! buffer. Two properties matter:
//!
//! - **The pipe must never fill.** If the app stopped reading, the engine
//!   would block on its next write and hang. The drain runs in its own task
//!   for the engine's whole lifetime, so it cannot be forgotten.
//! - **The buffer must be bounded.** An engine in a log loop would otherwise
//!   grow the app's memory without limit.
//!
//! # Redaction
//!
//! GOAL.md §6: "Avoid sensitive information in logs." Two things are redacted
//! before a line reaches the buffer:
//!
//! - the controller secret, which the app knows and can match exactly;
//! - the query string of a subscription URL, which carries the token that
//!   authenticates the subscription. A URL is redacted to its scheme, host
//!   and path, with the query replaced by `?<redacted>`.
//!
//! The redaction is applied to the *text*, not to the structured data, because
//! the engine's log lines are free text and there is no other place to
//! intervene.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A bounded ring buffer of log lines.
#[derive(Debug, Clone)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<LogLine>>>,
    capacity: usize,
}

/// One line of engine output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// When the line was captured, as milliseconds since the unix epoch.
    pub timestamp_ms: i64,
    /// The stream the line came from.
    pub stream: LogStreamKind,
    /// The line's text, with secrets redacted.
    pub text: String,
}

/// Which stream a log line came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogStreamKind {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

impl LogStreamKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

impl LogBuffer {
    /// Create a buffer holding at most `capacity` lines.
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: Arc::new(Mutex::new(VecDeque::with_capacity(capacity.min(1024)))),
            capacity: capacity.max(1),
        }
    }

    /// Append a line, dropping the oldest if the buffer is full.
    pub fn push(&self, stream: LogStreamKind, text: impl Into<String>) {
        let mut lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() >= self.capacity {
            lines.pop_front();
        }
        lines.push_back(LogLine {
            timestamp_ms: now_millis(),
            stream,
            text: text.into(),
        });
    }

    /// The lines currently held, oldest first.
    pub fn lines(&self) -> Vec<LogLine> {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        lines.iter().cloned().collect()
    }

    /// The last `count` lines, oldest first.
    pub fn tail(&self, count: usize) -> Vec<LogLine> {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        let skip = lines.len().saturating_sub(count);
        lines.iter().skip(skip).cloned().collect()
    }

    /// How many lines are held.
    pub fn len(&self) -> usize {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Discard every line.
    pub fn clear(&self) {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    /// The capacity of the buffer.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// The engine's log output, as a consumer sees it.
pub struct LogStream {
    buffer: LogBuffer,
}

impl LogStream {
    /// Wrap a buffer as a stream.
    pub fn new(buffer: LogBuffer) -> Self {
        Self { buffer }
    }

    /// The underlying buffer.
    pub fn buffer(&self) -> &LogBuffer {
        &self.buffer
    }

    /// The lines currently held, oldest first.
    pub fn lines(&self) -> Vec<LogLine> {
        self.buffer.lines()
    }

    /// The last `count` lines, oldest first.
    pub fn tail(&self, count: usize) -> Vec<LogLine> {
        self.buffer.tail(count)
    }
}

impl std::fmt::Debug for LogStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogStream")
            .field("lines", &self.buffer.len())
            .finish()
    }
}

/// A redactor for the values that must not reach a log file.
///
/// The redactor holds the exact strings to replace, so it is cheap and
/// exact — it never guesses at the shape of a secret.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// Literal strings to replace, longest first so a secret that contains
    /// another is replaced whole.
    secrets: Vec<String>,
    /// The replacement text.
    replacement: String,
}

impl Redactor {
    /// Build a redactor for the given literal secrets.
    pub fn new(secrets: impl IntoIterator<Item = String>) -> Self {
        let mut secrets: Vec<String> = secrets.into_iter().filter(|s| !s.is_empty()).collect();
        // Longest first, so a longer secret is replaced before a shorter
        // one that is a prefix of it.
        secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
        secrets.dedup();
        Self {
            secrets,
            replacement: "<redacted>".to_string(),
        }
    }

    /// Add a secret to redact.
    pub fn add_secret(&mut self, secret: impl Into<String>) {
        let secret = secret.into();
        if secret.is_empty() || self.secrets.contains(&secret) {
            return;
        }
        self.secrets.push(secret);
        self.secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));
    }

    /// Whether this redactor would change anything.
    pub fn is_empty(&self) -> bool {
        self.secrets.is_empty()
    }

    /// Redact a line of text.
    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for secret in &self.secrets {
            if out.contains(secret.as_str()) {
                out = out.replace(secret.as_str(), &self.replacement);
            }
        }
        redact_urls(&out)
    }
}

/// Replace the query string of any URL in the text.
///
/// A subscription URL carries the token that authenticates it — usually in
/// the query string, sometimes as a path segment. This keeps the scheme,
/// the host and the path, and replaces the query. The path is kept because
/// it is what identifies *which* subscription the line refers to, which is
/// what makes the log useful.
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find("http://").or_else(|| rest.find("https://")) {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let end = tail
            .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
            .unwrap_or(tail.len());
        let url = &tail[..end];
        out.push_str(&redact_one_url(url));
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

/// Replace the query string of a single URL.
fn redact_one_url(url: &str) -> String {
    // The URL ends at the first `?` or `#`.
    let cut = url.find(['?', '#']).unwrap_or(url.len());
    let (base, tail) = url.split_at(cut);
    if tail.is_empty() {
        return url.to_string();
    }
    // Keep the fragment — it is not a secret — but drop the query.
    let fragment = tail.find('#').map(|i| &tail[i..]).unwrap_or("");
    format!("{base}?{REDACTED_QUERY}{fragment}")
}

/// The text that replaces a redacted query string.
const REDACTED_QUERY: &str = "<redacted>";

/// The current time in milliseconds since the unix epoch.
///
/// This is the one place the crate reads the clock, and it is used only to
/// stamp log lines. The configuration pipeline itself never reads a clock.
fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_drops_the_oldest_line_when_it_is_full() {
        let buffer = LogBuffer::new(3);
        for i in 0..5 {
            buffer.push(LogStreamKind::Stdout, format!("line {i}"));
        }
        assert_eq!(buffer.len(), 3);
        let lines = buffer.lines();
        assert_eq!(lines[0].text, "line 2");
        assert_eq!(lines[2].text, "line 4");
    }

    #[test]
    fn the_tail_returns_the_last_lines() {
        let buffer = LogBuffer::new(10);
        for i in 0..5 {
            buffer.push(LogStreamKind::Stdout, format!("line {i}"));
        }
        let tail = buffer.tail(2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].text, "line 3");
        assert_eq!(tail[1].text, "line 4");
    }

    #[test]
    fn the_tail_of_a_short_buffer_is_the_whole_buffer() {
        let buffer = LogBuffer::new(10);
        buffer.push(LogStreamKind::Stdout, "only");
        assert_eq!(buffer.tail(5).len(), 1);
    }

    #[test]
    fn a_secret_is_redacted_exactly() {
        let redactor = Redactor::new(["s3cret-token".to_string()]);
        let text = "connecting to https://example.invalid/sub with s3cret-token";
        let redacted = redactor.redact(text);
        assert!(!redacted.contains("s3cret-token"));
        assert!(redacted.contains("<redacted>"));
    }

    #[test]
    fn an_empty_secret_is_ignored() {
        let redactor = Redactor::new(["".to_string()]);
        assert!(redactor.is_empty());
        assert_eq!(redactor.redact("unchanged"), "unchanged");
    }

    #[test]
    fn a_url_query_string_is_redacted_but_the_path_is_kept() {
        let redacted = redact_urls("GET https://example.invalid/sub?token=abc123 HTTP/1.1");
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains("example.invalid/sub"));
        assert!(redacted.contains("<redacted>"));
    }

    #[test]
    fn a_url_without_a_query_is_unchanged() {
        let text = "GET https://example.invalid/sub HTTP/1.1";
        assert_eq!(redact_urls(text), text);
    }

    #[test]
    fn several_urls_in_one_line_are_all_redacted() {
        let text = "a https://one.invalid/x?t=1 b https://two.invalid/y?t=2 c";
        let redacted = redact_urls(text);
        assert!(!redacted.contains("t=1"));
        assert!(!redacted.contains("t=2"));
        assert!(redacted.contains("one.invalid/x"));
        assert!(redacted.contains("two.invalid/y"));
    }

    #[test]
    fn a_url_at_the_end_of_a_line_is_redacted() {
        let redacted = redact_urls("see https://example.invalid/sub?token=abc");
        assert!(!redacted.contains("abc"));
    }

    #[test]
    fn a_url_followed_by_a_quote_ends_at_the_quote() {
        let redacted = redact_urls(r#"url="https://example.invalid/sub?token=abc""#);
        assert!(!redacted.contains("abc"));
        assert!(redacted.ends_with(r#"""#));
    }

    #[test]
    fn a_longer_secret_is_replaced_before_a_shorter_one() {
        let mut redactor = Redactor::new(["abc".to_string()]);
        redactor.add_secret("abcdef".to_string());
        let redacted = redactor.redact("value abcdef here");
        assert_eq!(redacted, "value <redacted> here");
    }

    #[test]
    fn the_buffer_can_be_cleared() {
        let buffer = LogBuffer::new(10);
        buffer.push(LogStreamKind::Stderr, "boom");
        buffer.clear();
        assert!(buffer.is_empty());
    }
}
