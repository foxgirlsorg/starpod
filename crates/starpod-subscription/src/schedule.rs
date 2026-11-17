//! The refresh schedule: when is a subscription next due?
//!
//! This is a pure function of the interval and the last successful
//! fetch. The clock is a *parameter*, never a call to
//! `SystemTime::now()` — that is what makes the schedule testable
//! without waiting, and what stops a stray clock read from making the
//! crate's behavior depend on when it happens to run.
//!
//! # The rule
//!
//! A subscription is due when
//!
//! ```text
//! now - last_updated >= interval
//! ```
//!
//! This mirrors the engine's own refresh loop, which polls every 60
//! seconds and refreshes a subscription whose interval has elapsed
//! (`meow-app-0.29.0/src/subscription_refresh.rs:89-97` — the `filter`
//! that selects the subscriptions to refresh — and `:505` — the 60-second
//! sleep between passes). A subscription that has *never* been fetched is
//! always due — the engine treats a missing `last_updated` as due for the
//! same reason (`subscription_refresh.rs:90`).
//!
//! # The clamp
//!
//! The interval is clamped to [`MAX_INTERVAL_SECS`] before it is used in
//! the comparison. The engine clamps the same way and for the same
//! reason: `u64::MAX as i64` wraps negative, which would make
//! `now - last_updated >= interval` true on every poll and turn the
//! refresh loop into a busy loop
//! (`meow-app-0.29.0/src/subscription_refresh.rs:91-96` — the comment
//! there names issue #648; the ceiling itself is
//! `meow-common-0.29.0/src/lib.rs:14`).
//!
//! # The failure case
//!
//! A *failed* refresh does not update `last_updated`. That is the
//! engine's rule too, and it is the right one: the engine stamps
//! `last_updated` on a payload defect (so a garbled body is not
//! re-downloaded every 60 seconds) but leaves it unstamped on a
//! transport failure, so a flaky network retries
//! (`meow-app-0.29.0/src/subscription_refresh.rs:473-499` — the comment
//! there names issue #533). The app's rule is simpler and stricter: a
//! failed refresh never changes the schedule, so the next attempt
//! happens at the next tick.

use crate::metadata::MAX_INTERVAL_SECS;

/// When a subscription is next due.
///
/// A `None` `last_updated` means the subscription has never been fetched
/// successfully and is due immediately.
pub fn next_due_at(interval_secs: u64, last_updated: Option<i64>, now: i64) -> Option<i64> {
    let Some(last_updated) = last_updated else {
        return Some(now);
    };
    let interval = interval_secs.min(MAX_INTERVAL_SECS) as i64;
    let elapsed = now.saturating_sub(last_updated);
    if elapsed >= interval {
        return Some(now);
    }
    Some(last_updated.saturating_add(interval))
}

/// The schedule of one subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshSchedule {
    /// The refresh interval, in seconds, before it is clamped.
    pub interval_secs: u64,
    /// When the subscription was last fetched successfully, as a unix
    /// timestamp in seconds.
    pub last_updated: Option<i64>,
}

impl RefreshSchedule {
    /// A schedule for a subscription with the given interval and
    /// last-successful-fetch time.
    pub fn new(interval_secs: u64, last_updated: Option<i64>) -> Self {
        Self {
            interval_secs,
            last_updated,
        }
    }

    /// Whether the subscription is due at `now`.
    pub fn is_due(&self, now: i64) -> bool {
        let Some(last_updated) = self.last_updated else {
            return true;
        };
        let interval = self.interval_secs.min(MAX_INTERVAL_SECS) as i64;
        now.saturating_sub(last_updated) >= interval
    }

    /// When the subscription is next due, as a unix timestamp in seconds.
    ///
    /// The value is the *earliest* moment the subscription may be
    /// refreshed — the caller decides whether to actually do it now.
    pub fn next_due_at(&self, now: i64) -> i64 {
        next_due_at(self.interval_secs, self.last_updated, now).unwrap_or(now)
    }

    /// The seconds until the subscription is next due.
    ///
    /// Zero when it is already due.
    pub fn seconds_until_due(&self, now: i64) -> i64 {
        self.next_due_at(now).saturating_sub(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: u64 = 3600;

    #[test]
    fn a_never_fetched_subscription_is_due_immediately() {
        let schedule = RefreshSchedule::new(HOUR, None);
        assert!(schedule.is_due(1_000_000));
        assert_eq!(schedule.next_due_at(1_000_000), 1_000_000);
        assert_eq!(schedule.seconds_until_due(1_000_000), 0);
    }

    #[test]
    fn a_fresh_subscription_is_not_due() {
        let schedule = RefreshSchedule::new(HOUR, Some(1_000_000));
        assert!(!schedule.is_due(1_000_000));
        assert!(!schedule.is_due(1_000_000 + HOUR as i64 - 1));
        assert_eq!(schedule.next_due_at(1_000_000), 1_000_000 + HOUR as i64);
    }

    #[test]
    fn a_subscription_is_due_once_the_interval_has_elapsed() {
        let schedule = RefreshSchedule::new(HOUR, Some(1_000_000));
        assert!(schedule.is_due(1_000_000 + HOUR as i64));
        assert!(schedule.is_due(1_000_000 + 2 * HOUR as i64));
    }

    #[test]
    fn a_zero_interval_is_always_due() {
        let schedule = RefreshSchedule::new(0, Some(1_000_000));
        assert!(schedule.is_due(1_000_000));
    }

    #[test]
    fn an_interval_beyond_the_ceiling_is_clamped() {
        // The engine clamps for the same reason: `u64::MAX as i64` wraps
        // negative and would make the comparison true on every poll.
        let schedule = RefreshSchedule::new(u64::MAX, Some(1_000_000));
        assert!(
            !schedule.is_due(1_000_000 + MAX_INTERVAL_SECS as i64 - 1),
            "an absurd interval must not become a busy loop"
        );
        assert!(schedule.is_due(1_000_000 + MAX_INTERVAL_SECS as i64));
    }

    #[test]
    fn a_clock_that_went_backwards_does_not_make_it_due() {
        let schedule = RefreshSchedule::new(HOUR, Some(1_000_000));
        assert!(
            !schedule.is_due(999_999),
            "a clock that moved backwards must not trigger a refresh"
        );
    }

    #[test]
    fn the_next_due_time_is_the_last_success_plus_the_interval() {
        assert_eq!(
            next_due_at(HOUR, Some(1_000_000), 1_000_500),
            Some(1_000_000 + HOUR as i64)
        );
    }

    #[test]
    fn the_next_due_time_is_now_when_the_subscription_is_already_due() {
        assert_eq!(
            next_due_at(HOUR, Some(1_000_000), 2_000_000),
            Some(2_000_000)
        );
    }

    #[test]
    fn the_next_due_time_is_now_when_it_was_never_fetched() {
        assert_eq!(next_due_at(HOUR, None, 2_000_000), Some(2_000_000));
    }

    #[test]
    fn the_seconds_until_due_counts_down() {
        let schedule = RefreshSchedule::new(HOUR, Some(1_000_000));
        assert_eq!(schedule.seconds_until_due(1_000_000), HOUR as i64);
        assert_eq!(schedule.seconds_until_due(1_000_000 + 60), HOUR as i64 - 60);
    }
}
