//! The Connections page's data: the engine's open connections, the history
//! of closed ones, the speeds, and the filter and sort the page applies.
//!
//! The tracker is pure. It is fed the engine's answer and a clock reading,
//! so the diff, the speeds and the history can be tested without an engine.
//! The service owns one and feeds it on each poll of the page.

use std::collections::BTreeMap;
use std::time::Instant;

use starpod_core::timestamp;
use starpod_engine::{ConnectionsReply, EngineConnection};

/// How many closed connections the history keeps, newest first.
pub const CLOSED_HISTORY_LIMIT: usize = 200;

/// A speed is measured between two polls. A gap longer than this (the page
/// was hidden, or the engine was busy) is averaged over nothing useful, so
/// the speed reads as zero rather than as a misleading average.
const MAX_SPEED_GAP_SECS: f64 = 10.0;

/// Sort by when each connection opened, newest first.
pub const SORT_NEWEST: i32 = 0;
/// Sort by upload, largest first.
pub const SORT_UPLOAD: i32 = 1;
/// Sort by download, largest first.
pub const SORT_DOWNLOAD: i32 = 2;
/// Sort by host, A to Z.
pub const SORT_HOST: i32 = 3;

/// One connection, as the Connections page shows it. Byte counts are raw; the
/// UI layer formats them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionView {
    /// The engine's id. A close request names it.
    pub id: String,
    /// The host the connection asked for, or its IP when it had no host.
    pub host: String,
    /// `ip:port` of the destination. Empty when the engine gave no IP.
    pub destination: String,
    /// `tcp` or `udp`.
    pub network: String,
    /// The inbound type, such as `HTTPS`.
    pub kind: String,
    /// The process name, or empty when the engine could not tell it.
    pub process: String,
    /// The outbounds in the engine's order. Joined with arrows for display.
    pub chains: Vec<String>,
    /// The matched rule as `RULE,payload`, or just `RULE` with no payload.
    pub rule: String,
    /// Bytes uploaded so far.
    pub upload: u64,
    /// Bytes downloaded so far.
    pub download: u64,
    /// Bytes per second uploaded since the previous poll. Zero when new.
    pub upload_speed: u64,
    /// Bytes per second downloaded since the previous poll. Zero when new.
    pub download_speed: u64,
    /// When the connection opened, as the engine sent it (RFC 3339).
    pub started_at: Option<String>,
    /// [`ConnectionView::started_at`] as unix seconds, when it parses.
    pub started_unix: Option<i64>,
    /// How long the connection has been open, in seconds, as of its last poll.
    pub duration_secs: u64,
    /// Whether the connection has ended. A closed one keeps its last figures.
    pub closed: bool,
}

impl ConnectionView {
    /// The chain as one line: `A → B → C`.
    pub fn chain_text(&self) -> String {
        self.chains.join(" → ")
    }

    /// The rule as one line: `RULE,payload`, or `RULE` when there is no payload.
    pub fn rule_text(&self) -> &str {
        &self.rule
    }
}

/// The Connections page's data, as one snapshot value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionsView {
    /// The open connections, in the order they were last polled.
    pub active: Vec<ConnectionView>,
    /// The connections that have ended, newest first, at most
    /// [`CLOSED_HISTORY_LIMIT`].
    pub closed: Vec<ConnectionView>,
    /// Bytes the engine has uploaded since it started.
    pub upload_total: u64,
    /// Bytes the engine has downloaded since it started.
    pub download_total: u64,
}

/// Bytes per second between two readings of a running total.
///
/// Zero when the interval is empty or too long to measure, or when the total
/// went down (a counter that restarted). Otherwise the rounded rate.
pub fn speed(previous: u64, current: u64, elapsed_secs: f64) -> u64 {
    if elapsed_secs <= 0.0 || elapsed_secs > MAX_SPEED_GAP_SECS || current < previous {
        return 0;
    }
    ((current - previous) as f64 / elapsed_secs).round() as u64
}

/// The rule as one line: `RULE,payload`, or `RULE` when there is no payload.
pub fn rule_text(rule: &str, payload: &str) -> String {
    if payload.is_empty() {
        rule.to_string()
    } else {
        format!("{rule},{payload}")
    }
}

/// Build the view of one engine connection, without speeds.
fn view_of(connection: &EngineConnection, now_unix: i64) -> ConnectionView {
    let started_unix = timestamp::parse(&connection.start);
    let destination = if connection.destination_ip.is_empty() {
        String::new()
    } else if connection.destination_port.is_empty() {
        connection.destination_ip.clone()
    } else {
        format!(
            "{}:{}",
            connection.destination_ip, connection.destination_port
        )
    };
    let host = if connection.host.is_empty() {
        connection.destination_ip.clone()
    } else {
        connection.host.clone()
    };
    ConnectionView {
        id: connection.id.clone(),
        host,
        destination,
        network: connection.network.clone(),
        kind: connection.kind.clone(),
        process: connection.process.clone(),
        chains: connection.chains.clone(),
        rule: rule_text(&connection.rule, &connection.rule_payload),
        upload: connection.upload,
        download: connection.download,
        upload_speed: 0,
        download_speed: 0,
        started_at: (!connection.start.is_empty()).then(|| connection.start.clone()),
        started_unix,
        duration_secs: started_unix.map_or(0, |started| (now_unix - started).max(0) as u64),
        closed: false,
    }
}

/// The connections seen so far, and the history of the ones that ended.
///
/// Connections are matched by id between polls. A connection that was open
/// and is missing from the next answer has closed; it moves to the history
/// with its last figures and no speed.
#[derive(Debug, Clone)]
pub struct ConnectionTracker {
    /// The open connections, by id, with the poll time they were last seen.
    active: BTreeMap<String, Tracked>,
    /// The ended connections, newest first.
    closed: Vec<ConnectionView>,
    upload_total: u64,
    download_total: u64,
    /// The clock every poll time is measured from.
    epoch: Instant,
}

#[derive(Debug, Clone)]
struct Tracked {
    view: ConnectionView,
    /// Seconds since the tracker's epoch, at the poll that last saw it.
    seen_at: f64,
}

impl Default for ConnectionTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionTracker {
    /// An empty tracker, with its clock starting now.
    pub fn new() -> Self {
        Self {
            active: BTreeMap::new(),
            closed: Vec::new(),
            upload_total: 0,
            download_total: 0,
            epoch: Instant::now(),
        }
    }

    /// Record one poll of the engine, measured against the tracker's own clock.
    ///
    /// `now_unix` is the wall clock, used for durations.
    pub fn observe(&mut self, reply: &ConnectionsReply, now_unix: i64) {
        let at = self.epoch.elapsed().as_secs_f64();
        self.observe_at(reply, now_unix, at);
    }

    /// As [`ConnectionTracker::observe`], with the poll time given in seconds
    /// since the tracker's epoch. Tests use it to control the clock.
    pub fn observe_at(&mut self, reply: &ConnectionsReply, now_unix: i64, at: f64) {
        let mut next = BTreeMap::new();
        for connection in &reply.connections {
            let mut view = view_of(connection, now_unix);
            if let Some(previous) = self.active.get(&connection.id) {
                let elapsed = at - previous.seen_at;
                view.upload_speed = speed(previous.view.upload, view.upload, elapsed);
                view.download_speed = speed(previous.view.download, view.download, elapsed);
            }
            next.insert(connection.id.clone(), Tracked { view, seen_at: at });
        }
        for (id, tracked) in &self.active {
            if !next.contains_key(id) {
                self.closed.insert(0, ended(&tracked.view));
            }
        }
        self.closed.truncate(CLOSED_HISTORY_LIMIT);
        self.active = next;
        self.upload_total = reply.upload_total;
        self.download_total = reply.download_total;
    }

    /// The engine is not running: every open connection has ended, and the
    /// totals start again from zero when it next starts.
    pub fn end_session(&mut self) {
        for tracked in std::mem::take(&mut self.active).into_values() {
            self.closed.insert(0, ended(&tracked.view));
        }
        self.closed.truncate(CLOSED_HISTORY_LIMIT);
        self.upload_total = 0;
        self.download_total = 0;
    }

    /// Whether any connection is open as of the last poll.
    pub fn has_open(&self) -> bool {
        !self.active.is_empty()
    }

    /// Forget the history of closed connections. The open ones are kept.
    pub fn clear_closed(&mut self) {
        self.closed.clear();
    }

    /// The ids of the open connections whose chain holds `group`.
    pub fn ids_through(&self, group: &str) -> Vec<String> {
        self.active
            .iter()
            .filter(|(_, tracked)| tracked.view.chains.iter().any(|chain| chain == group))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The view the snapshot carries.
    pub fn view(&self) -> ConnectionsView {
        ConnectionsView {
            active: self
                .active
                .values()
                .map(|tracked| tracked.view.clone())
                .collect(),
            closed: self.closed.clone(),
            upload_total: self.upload_total,
            download_total: self.download_total,
        }
    }
}

/// A connection as it stands once it has ended.
fn ended(view: &ConnectionView) -> ConnectionView {
    ConnectionView {
        closed: true,
        upload_speed: 0,
        download_speed: 0,
        ..view.clone()
    }
}

/// Whether a connection matches the page's filter: a case-insensitive
/// substring of its host, process, chain or rule. An empty filter matches all.
pub fn matches(view: &ConnectionView, filter: &str) -> bool {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    let chain = view.chain_text();
    [
        view.host.as_str(),
        view.process.as_str(),
        chain.as_str(),
        view.rule_text(),
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(&needle))
}

/// Sort the connections in place. Ties keep a stable order by id.
pub fn sort(items: &mut [ConnectionView], mode: i32) {
    match mode {
        SORT_UPLOAD => items.sort_by(|a, b| b.upload.cmp(&a.upload).then(a.id.cmp(&b.id))),
        SORT_DOWNLOAD => items.sort_by(|a, b| b.download.cmp(&a.download).then(a.id.cmp(&b.id))),
        SORT_HOST => items.sort_by(|a, b| {
            a.host
                .to_lowercase()
                .cmp(&b.host.to_lowercase())
                .then(a.id.cmp(&b.id))
        }),
        // Newest first. `None` (a start time that did not parse) sorts last.
        _ => items.sort_by(|a, b| b.started_unix.cmp(&a.started_unix).then(a.id.cmp(&b.id))),
    }
}

/// The clock part of an RFC 3339 instant, `12:03:44`, in the engine's own
/// offset. Empty when the instant has no time part.
pub fn clock_text(started_at: Option<&str>) -> String {
    started_at
        .and_then(|text| text.split_once('T'))
        .and_then(|(_, time)| time.get(..8))
        .unwrap_or_default()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000;

    fn stamp(unix: i64) -> String {
        timestamp::format(unix).expect("a representable time")
    }

    fn connection(id: &str, upload: u64, download: u64) -> EngineConnection {
        EngineConnection {
            id: id.to_string(),
            network: "tcp".to_string(),
            kind: "HTTPS".to_string(),
            host: format!("{id}.example"),
            destination_ip: "203.0.113.7".to_string(),
            destination_port: "443".to_string(),
            process: "chrome.exe".to_string(),
            upload,
            download,
            start: stamp(T0 - 30),
            chains: vec!["Node A".to_string(), "Proxy".to_string()],
            rule: "DOMAIN-SUFFIX".to_string(),
            rule_payload: "example.com".to_string(),
            ..EngineConnection::default()
        }
    }

    fn reply(connections: Vec<EngineConnection>) -> ConnectionsReply {
        ConnectionsReply {
            upload_total: 0,
            download_total: 0,
            connections,
        }
    }

    #[test]
    fn a_connection_shows_its_destination_chain_rule_and_start() {
        let mut tracker = ConnectionTracker::new();
        tracker.observe_at(&reply(vec![connection("a", 10, 20)]), T0, 0.0);
        let view = tracker.view();
        let first = &view.active[0];
        assert_eq!(first.host, "a.example");
        assert_eq!(first.destination, "203.0.113.7:443");
        assert_eq!(first.chain_text(), "Node A → Proxy");
        assert_eq!(first.rule_text(), "DOMAIN-SUFFIX,example.com");
        assert_eq!(first.duration_secs, 30);
        assert!(!first.closed);
        assert_eq!(clock_text(first.started_at.as_deref()).len(), 8);
    }

    #[test]
    fn an_empty_host_falls_back_to_the_destination_ip_and_a_missing_payload_is_dropped() {
        let mut bare = connection("b", 0, 0);
        bare.host.clear();
        bare.rule_payload.clear();
        let mut tracker = ConnectionTracker::new();
        tracker.observe_at(&reply(vec![bare]), T0, 0.0);
        let view = tracker.view();
        assert_eq!(view.active[0].host, "203.0.113.7");
        assert_eq!(view.active[0].rule_text(), "DOMAIN-SUFFIX");
    }

    #[test]
    fn a_connection_that_leaves_the_list_moves_to_the_closed_history() {
        let mut tracker = ConnectionTracker::new();
        tracker.observe_at(
            &reply(vec![connection("a", 10, 20), connection("b", 1, 2)]),
            T0,
            0.0,
        );
        tracker.observe_at(&reply(vec![connection("a", 15, 25)]), T0 + 1, 1.0);
        let view = tracker.view();
        assert_eq!(view.active.len(), 1);
        assert_eq!(view.active[0].id, "a");
        assert_eq!(view.closed.len(), 1);
        assert_eq!(view.closed[0].id, "b");
        assert!(view.closed[0].closed);
        assert_eq!(
            view.closed[0].upload, 1,
            "a closed connection keeps its figures"
        );
        assert_eq!(view.closed[0].upload_speed, 0);
    }

    #[test]
    fn the_closed_history_keeps_the_newest_two_hundred() {
        let mut tracker = ConnectionTracker::new();
        for round in 0..(CLOSED_HISTORY_LIMIT + 5) {
            let id = format!("c{round}");
            tracker.observe_at(&reply(vec![connection(&id, 1, 1)]), T0, round as f64);
            tracker.observe_at(&reply(vec![]), T0, round as f64 + 0.5);
        }
        let view = tracker.view();
        assert_eq!(view.closed.len(), CLOSED_HISTORY_LIMIT);
        assert_eq!(view.closed[0].id, format!("c{}", CLOSED_HISTORY_LIMIT + 4));
    }

    #[test]
    fn ending_the_session_closes_everything_and_resets_the_totals() {
        let mut tracker = ConnectionTracker::new();
        let mut first = reply(vec![connection("a", 5, 5)]);
        first.upload_total = 500;
        first.download_total = 900;
        tracker.observe_at(&first, T0, 0.0);
        tracker.end_session();
        let view = tracker.view();
        assert!(view.active.is_empty());
        assert_eq!(view.closed.len(), 1);
        assert_eq!((view.upload_total, view.download_total), (0, 0));
    }

    #[test]
    fn the_totals_come_from_the_engine_answer() {
        let mut tracker = ConnectionTracker::new();
        let mut answer = reply(vec![]);
        answer.upload_total = 1024;
        answer.download_total = 4096;
        tracker.observe_at(&answer, T0, 0.0);
        let view = tracker.view();
        assert_eq!((view.upload_total, view.download_total), (1024, 4096));
    }

    #[test]
    fn a_closed_connection_can_be_cleared_without_touching_the_open_ones() {
        let mut tracker = ConnectionTracker::new();
        tracker.observe_at(&reply(vec![connection("a", 1, 1)]), T0, 0.0);
        tracker.observe_at(
            &reply(vec![connection("a", 1, 1), connection("b", 1, 1)]),
            T0,
            1.0,
        );
        tracker.observe_at(&reply(vec![connection("a", 1, 1)]), T0, 2.0);
        tracker.clear_closed();
        let view = tracker.view();
        assert!(view.closed.is_empty());
        assert_eq!(view.active.len(), 1);
    }

    #[test]
    fn the_speed_is_the_growth_per_second_between_polls() {
        assert_eq!(speed(1000, 3000, 2.0), 1000);
        assert_eq!(speed(0, 1, 3.0), 0, "a third of a byte rounds to zero");
        assert_eq!(speed(0, 2, 3.0), 1, "two thirds of a byte rounds to one");
        assert_eq!(
            speed(5000, 4000, 1.0),
            0,
            "a total that went down reads as zero"
        );
        assert_eq!(speed(0, 100, 0.0), 0, "an empty interval reads as zero");
        assert_eq!(speed(0, 100, 60.0), 0, "a stale gap reads as zero");
    }

    #[test]
    fn a_connections_speed_comes_from_its_previous_poll_and_is_zero_when_new() {
        let mut tracker = ConnectionTracker::new();
        tracker.observe_at(&reply(vec![connection("a", 0, 0)]), T0, 0.0);
        tracker.observe_at(&reply(vec![connection("a", 2000, 4000)]), T0, 2.0);
        let view = tracker.view();
        assert_eq!(view.active[0].upload_speed, 1000);
        assert_eq!(view.active[0].download_speed, 2000);

        tracker.observe_at(&reply(vec![connection("b", 9999, 9999)]), T0, 3.0);
        let view = tracker.view();
        let fresh = view.active.iter().find(|c| c.id == "b").unwrap();
        assert_eq!(
            fresh.upload_speed, 0,
            "a connection with no earlier poll has no speed"
        );
    }

    #[test]
    fn the_filter_matches_host_process_chain_and_rule_case_insensitively() {
        let mut tracker = ConnectionTracker::new();
        let mut other = connection("b", 0, 0);
        other.host = "video.test".to_string();
        other.process = "Spotify.exe".to_string();
        other.chains = vec!["DIRECT".to_string()];
        other.rule = "MATCH".to_string();
        other.rule_payload.clear();
        tracker.observe_at(&reply(vec![connection("a", 0, 0), other]), T0, 0.0);
        let view = tracker.view();
        let by = |filter: &str| -> Vec<String> {
            view.active
                .iter()
                .filter(|c| matches(c, filter))
                .map(|c| c.id.clone())
                .collect()
        };
        assert_eq!(by(""), vec!["a".to_string(), "b".to_string()]);
        assert_eq!(by("EXAMPLE"), vec!["a".to_string()]);
        assert_eq!(by("spotify"), vec!["b".to_string()]);
        assert_eq!(by("node a → proxy"), vec!["a".to_string()]);
        assert_eq!(by("match"), vec!["b".to_string()]);
        assert_eq!(
            by("  video  "),
            vec!["b".to_string()],
            "the filter is trimmed"
        );
        assert!(by("nothing matches this").is_empty());
    }

    #[test]
    fn the_sort_orders_by_newest_upload_download_and_host() {
        let mut newest = connection("old", 9, 1);
        newest.start = stamp(T0 - 600);
        let mut fresh = connection("new", 1, 9);
        fresh.start = stamp(T0 - 5);
        let mut zed = connection("z", 5, 5);
        zed.host = "Zed.test".to_string();
        let mut alpha = connection("a", 5, 5);
        alpha.host = "alpha.test".to_string();
        let mut tracker = ConnectionTracker::new();
        tracker.observe_at(&reply(vec![newest, fresh, zed, alpha]), T0, 0.0);
        let view = tracker.view();
        let ids = |mode: i32| -> Vec<String> {
            let mut items = view.active.clone();
            sort(&mut items, mode);
            items.into_iter().map(|c| c.id).collect()
        };
        // Newest: "new" (5 s ago), then the two opened 30 s ago in id order, then "old".
        assert_eq!(ids(SORT_NEWEST), vec!["new", "a", "z", "old"]);
        assert_eq!(ids(SORT_UPLOAD)[0], "old");
        assert_eq!(ids(SORT_DOWNLOAD)[0], "new");
        assert_eq!(ids(SORT_HOST), vec!["a", "new", "old", "z"]);
    }

    #[test]
    fn a_newest_sort_puts_the_latest_start_first() {
        let mut older = connection("old", 0, 0);
        older.start = stamp(T0 - 600);
        let mut newer = connection("new", 0, 0);
        newer.start = stamp(T0 - 5);
        let mut tracker = ConnectionTracker::new();
        tracker.observe_at(&reply(vec![older, newer]), T0, 0.0);
        let mut items = tracker.view().active;
        sort(&mut items, SORT_NEWEST);
        assert_eq!(items[0].id, "new");
        assert_eq!(items[1].id, "old");
    }

    #[test]
    fn the_connections_through_a_group_are_found_by_their_chain() {
        let mut through = connection("a", 0, 0);
        through.chains = vec!["Node A".to_string(), "Proxy".to_string()];
        let mut elsewhere = connection("b", 0, 0);
        elsewhere.chains = vec!["DIRECT".to_string()];
        let mut tracker = ConnectionTracker::new();
        tracker.observe_at(&reply(vec![through, elsewhere]), T0, 0.0);
        assert_eq!(tracker.ids_through("Proxy"), vec!["a".to_string()]);
        assert!(tracker.ids_through("Missing").is_empty());
    }

    #[test]
    fn a_clock_reads_the_time_part_of_the_start() {
        assert_eq!(
            clock_text(Some("2026-11-14T12:03:44.123+03:00")),
            "12:03:44"
        );
        assert_eq!(clock_text(Some("nonsense")), "");
        assert_eq!(clock_text(None), "");
    }
}
