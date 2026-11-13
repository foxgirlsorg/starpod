//! The timestamp format the app stores and displays.
//!
//! Timestamps cross the app's boundaries as RFC 3339 strings — in the
//! subscription metadata file, in the persisted user state, and in the
//! records the UI shows. They are stored as *strings*, not as epoch
//! numbers, because the files they live in are meant to be inspectable and
//! because a stored RFC 3339 string survives a change of the app's
//! timezone handling without rewriting every file.
//!
//! One format, one place: the parser and the formatter live here so that
//! every crate agrees on what a stored timestamp means. A timestamp that
//! cannot be represented — one beyond the year 9999, say — reads as
//! absent rather than as a wrong date.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Format a unix timestamp (in seconds) as RFC 3339.
///
/// Returns `None` when the timestamp is not representable.
pub fn format(timestamp: i64) -> Option<String> {
    let datetime = OffsetDateTime::from_unix_timestamp(timestamp).ok()?;
    datetime.format(&Rfc3339).ok()
}

/// Parse an RFC 3339 timestamp into unix seconds.
///
/// The format is deliberately strict: RFC 3339 requires an explicit offset
/// (`Z` or `±HH:MM`), so a bare local time is rejected rather than
/// silently interpreted in the machine's timezone. A value that does not
/// parse reads as absent.
pub fn parse(text: &str) -> Option<i64> {
    OffsetDateTime::parse(text, &Rfc3339)
        .ok()
        .map(|dt| dt.unix_timestamp())
}

/// The current time, as RFC 3339.
///
/// This is the one place the app reads the clock for display purposes. It
/// is deliberately not used by the configuration pipeline, which stays
/// pure and takes its times as arguments.
pub fn now() -> Option<String> {
    OffsetDateTime::now_utc().format(&Rfc3339).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_timestamp_round_trips() {
        let text = format(1_798_675_200).unwrap();
        assert_eq!(text, "2026-12-31T00:00:00Z");
        assert_eq!(parse(&text), Some(1_798_675_200));
    }

    #[test]
    fn a_timestamp_beyond_the_representable_range_is_absent() {
        // `i64::MAX` seconds is far beyond the year 9999.
        assert_eq!(format(i64::MAX), None);
    }

    #[test]
    fn a_bare_local_time_is_rejected() {
        // RFC 3339 requires an offset. Reading "2026-12-31T00:00:00" as
        // local time would make a stored value mean different instants on
        // different machines, so it is rejected.
        assert_eq!(parse("2026-12-31T00:00:00"), None);
    }

    #[test]
    fn garbage_is_absent_rather_than_an_error() {
        assert_eq!(parse("not a timestamp"), None);
        assert_eq!(parse(""), None);
    }

    #[test]
    fn an_offset_timestamp_is_read_as_its_instant() {
        assert_eq!(parse("2026-12-31T08:00:00+08:00"), Some(1_798_675_200));
    }
}
