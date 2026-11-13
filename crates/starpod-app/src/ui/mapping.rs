//! Pure mapping from the app's view state to the Slint `Bridge` contract.
//!
//! Nothing here decides anything about the proxy. The functions turn
//! [`AppSnapshot`] values and UI filters into the strings, numbers and lists
//! the `.slint` files render (ARCHITECTURE §6.3). Only [`apply_snapshot`]
//! touches a `Bridge`, and it only sets properties. The rest is testable
//! without a display.

use crate::{
    AppError, AppSnapshot, NodeView, NoticeKind, RuleView, SubscriptionStatus, SubscriptionView,
};
use slint::{Model, ModelRc, SharedString, VecModel};
use starpod_core::rules::parse_line;
use starpod_core::{
    LogLevel, ProxyMode, RuleSpec, SubscriptionMode, SubscriptionQuota, TunSettings,
    UserPreferences,
};
use starpod_engine::EnginePhase;
use starpod_subscription::metadata::DEFAULT_INTERVAL_SECS;
use starpod_ui::{Bridge, LogItem, NodeItem, RuleItem, SettingsModel, SubscriptionItem};

/// The most log lines the Logs screen keeps.
pub const LOG_LINE_LIMIT: usize = 2000;
/// The auto-refresh interval used when the user has not chosen one.
pub const DEFAULT_REFRESH_MINUTES: u32 = (DEFAULT_INTERVAL_SECS / 60) as u32;
/// The smallest TUN MTU the engine accepts.
const MIN_TUN_MTU: u16 = 1280;

/// A banner the shell shows above the active page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// `info`, `success` or `error`.
    pub kind: &'static str,
    /// The text, already redacted.
    pub message: String,
}

impl Banner {
    /// A neutral banner.
    pub fn info(message: impl Into<String>) -> Self {
        Self {
            kind: "info",
            message: message.into(),
        }
    }

    /// A banner for something that worked.
    pub fn success(message: impl Into<String>) -> Self {
        Self {
            kind: "success",
            message: message.into(),
        }
    }

    /// A banner for something that failed.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            kind: "error",
            message: message.into(),
        }
    }
}

/// The banner for a notice the service raised.
pub fn notice_banner(notice: &(NoticeKind, String)) -> Banner {
    let kind = match notice.0 {
        NoticeKind::Error => "error",
        NoticeKind::Info | NoticeKind::Warning => "info",
    };
    Banner {
        kind,
        message: notice.1.clone(),
    }
}

/// The engine phase as the `.slint` side spells it.
pub fn phase_str(phase: EnginePhase) -> &'static str {
    match phase {
        EnginePhase::Stopped => "stopped",
        EnginePhase::Starting => "starting",
        EnginePhase::Running => "running",
        EnginePhase::Stopping => "stopping",
        EnginePhase::Failed => "failed",
    }
}

/// A subscription's status as the `.slint` side spells it. An engine-managed
/// subscription cannot be seen from here, so it is not shown as failing.
pub fn status_str(status: SubscriptionStatus) -> &'static str {
    match status {
        SubscriptionStatus::NeverFetched => "never",
        SubscriptionStatus::Current | SubscriptionStatus::EngineManaged => "ok",
        SubscriptionStatus::Failing => "error",
    }
}

/// `1h 02m`, `4m 09s` or `42s`.
pub fn format_uptime(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;
    if hours > 0 {
        format!("{hours}h {minutes:02}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

/// A byte count in decimal units: `12.3 GB`, `100 GB`, `512 B`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    let text = format!("{value:.1}");
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", UNITS[unit])
}

/// The share of the quota that is used, from 0 to 1. Zero when the total is
/// unknown, so an unknown total never draws a full bar.
pub fn quota_fraction(quota: &SubscriptionQuota) -> f32 {
    match quota.total {
        Some(total) if total > 0 => (quota.used as f64 / total as f64).clamp(0.0, 1.0) as f32,
        _ => 0.0,
    }
}

/// `12.3 GB of 100 GB`, or `12.3 GB used` when the provider gave no total.
pub fn quota_text(quota: &SubscriptionQuota) -> String {
    match quota.total {
        Some(total) => format!("{} of {}", format_bytes(quota.used), format_bytes(total)),
        None => format!("{} used", format_bytes(quota.used)),
    }
}

/// An RFC 3339 instant as `2026-11-13 10:00`. Anything else is returned as it
/// was.
pub fn display_time(text: &str) -> String {
    match text.split_once('T') {
        Some((date, time)) => format!("{date} {}", time.get(..5).unwrap_or(time)),
        None => text.to_string(),
    }
}

/// The date part of an RFC 3339 instant. Anything else is returned as it was.
pub fn short_date(text: &str) -> String {
    text.split_once('T')
        .map_or(text, |(date, _)| date)
        .to_string()
}

/// Convert a count to `i32`, saturating rather than wrapping.
pub fn saturating_i32<T: TryInto<i32>>(value: T) -> i32 {
    value.try_into().unwrap_or(i32::MAX)
}

/// One subscription row.
pub fn subscription_item(view: &SubscriptionView) -> SubscriptionItem {
    let (has_quota, fraction, text, expires) = match &view.quota {
        Some(quota) => (
            true,
            quota_fraction(quota),
            quota_text(quota),
            quota.expires.as_deref().map(short_date).unwrap_or_default(),
        ),
        None => (false, 0.0, String::new(), String::new()),
    };
    SubscriptionItem {
        id: view.id.as_str().into(),
        label: view.label.as_str().into(),
        mode: view.mode.as_str().into(),
        status: status_str(view.status).into(),
        last_updated: view
            .last_updated
            .as_deref()
            .map(display_time)
            .unwrap_or_default()
            .into(),
        error: view.error.clone().unwrap_or_default().into(),
        node_count: saturating_i32(view.node_count),
        has_quota,
        quota_used_fraction: fraction,
        quota_text: text.into(),
        expires: expires.into(),
    }
}

/// Whether a node matches the Servers filter: a case-insensitive substring of
/// its name, group or protocol. An empty filter matches everything.
pub fn node_matches(node: &NodeView, filter: &str) -> bool {
    let needle = filter.trim().to_lowercase();
    if needle.is_empty() {
        return true;
    }
    [
        node.name.as_str(),
        node.protocol.as_str(),
        node.group.as_deref().unwrap_or(""),
    ]
    .iter()
    .any(|field| field.to_lowercase().contains(&needle))
}

/// One node row. Latency is `-1` until the node has been tested.
pub fn node_item(node: &NodeView) -> NodeItem {
    NodeItem {
        id: node.id.as_str().into(),
        name: node.name.as_str().into(),
        protocol: node.protocol.as_str().into(),
        group: node.group.clone().unwrap_or_default().into(),
        latency_ms: node.latency_ms.map_or(-1, saturating_i32),
        selected: node.selected,
    }
}

/// One rule row.
pub fn rule_item(rule: &RuleView) -> RuleItem {
    RuleItem {
        index: saturating_i32(rule.index),
        kind: rule.kind.as_str().into(),
        payload: rule.payload.as_str().into(),
        target: rule.target.as_str().into(),
        line: rule.line.as_str().into(),
        from_subscription: rule.from_subscription,
        error: SharedString::default(),
    }
}

/// The user's rules as engine lines, one per line, in the user's order.
pub fn rules_raw_text(rules: &[RuleView]) -> String {
    rules
        .iter()
        .map(|rule| rule.line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The settings form, from the saved preferences. A port that is off is `0`.
pub fn settings_model(prefs: &UserPreferences) -> SettingsModel {
    SettingsModel {
        mixed_port: prefs.mixed_port.map_or(0, i32::from),
        socks_port: prefs.socks_port.map_or(0, i32::from),
        http_port: prefs.port.map_or(0, i32::from),
        allow_lan: prefs.allow_lan.unwrap_or(false),
        bind_address: prefs.bind_address.clone().unwrap_or_default().into(),
        log_level: prefs.log_level.unwrap_or_default().as_str().into(),
        mode: prefs.mode.unwrap_or_default().as_str().into(),
        tun_enabled: prefs.tun.enabled,
        tun_device: prefs.tun.device.clone().unwrap_or_default().into(),
        tun_mtu: prefs.tun.mtu.map_or(0, i32::from),
        auto_refresh_minutes: prefs
            .auto_refresh_minutes
            .unwrap_or(DEFAULT_REFRESH_MINUTES)
            .try_into()
            .unwrap_or(i32::MAX),
    }
}

/// The settings form as plain Rust values, so it can cross into a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsInput {
    /// `0` turns the port off.
    pub mixed_port: i32,
    /// `0` turns the port off.
    pub socks_port: i32,
    /// `0` turns the port off.
    pub http_port: i32,
    /// Whether the proxy listens on the LAN.
    pub allow_lan: bool,
    /// Empty means the default bind address.
    pub bind_address: String,
    /// `silent`, `error`, `warning`, `info` or `debug`.
    pub log_level: String,
    /// `rule`, `global` or `direct`.
    pub mode: String,
    /// Whether the TUN inbound is on.
    pub tun_enabled: bool,
    /// Empty means the engine's default device.
    pub tun_device: String,
    /// `0` means the engine default.
    pub tun_mtu: i32,
    /// `0` turns automatic refresh off.
    pub auto_refresh_minutes: i32,
}

impl SettingsInput {
    /// Copy the form out of the Slint model. Call this on the UI thread.
    pub fn from_model(model: &SettingsModel) -> Self {
        Self {
            mixed_port: model.mixed_port,
            socks_port: model.socks_port,
            http_port: model.http_port,
            allow_lan: model.allow_lan,
            bind_address: model.bind_address.to_string(),
            log_level: model.log_level.to_string(),
            mode: model.mode.to_string(),
            tun_enabled: model.tun_enabled,
            tun_device: model.tun_device.to_string(),
            tun_mtu: model.tun_mtu,
            auto_refresh_minutes: model.auto_refresh_minutes,
        }
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::Invalid(message.into())
}

fn port_field(name: &str, value: i32) -> Result<Option<u16>, AppError> {
    match value {
        0 => Ok(None),
        _ => u16::try_from(value).map(Some).map_err(|_| {
            invalid(format!(
                "the {name} must be between 1 and 65535, or 0 to turn it off"
            ))
        }),
    }
}

fn mtu_field(value: i32) -> Result<Option<u16>, AppError> {
    match value {
        0 => Ok(None),
        _ => u16::try_from(value)
            .ok()
            .filter(|mtu| *mtu >= MIN_TUN_MTU)
            .map(Some)
            .ok_or_else(|| {
                invalid(format!(
                    "the TUN MTU must be {MIN_TUN_MTU} or more, or 0 for the engine default"
                ))
            }),
    }
}

fn non_empty(text: &str) -> Option<String> {
    (!text.is_empty()).then(|| text.to_string())
}

/// A field the form shows as its effective value. When the user did not
/// change it, the saved value (including "not chosen") is kept, so saving
/// the form never turns a default into an explicit choice.
fn keep_unless_edited<T>(
    current: Option<T>,
    shown: impl FnOnce(&Option<T>) -> bool,
    edited: impl FnOnce() -> Result<Option<T>, AppError>,
) -> Result<Option<T>, AppError> {
    if shown(&current) {
        Ok(current)
    } else {
        edited()
    }
}

/// Build the preferences to save from the current ones and the form. Fields
/// the form does not edit (selection, the TUN routing details) are kept.
pub fn build_preferences(
    current: &UserPreferences,
    input: &SettingsInput,
) -> Result<UserPreferences, AppError> {
    let mixed_port = keep_unless_edited(
        current.mixed_port,
        |value| input.mixed_port == value.map_or(0, i32::from),
        || port_field("mixed port", input.mixed_port),
    )?;
    let socks_port = keep_unless_edited(
        current.socks_port,
        |value| input.socks_port == value.map_or(0, i32::from),
        || port_field("SOCKS port", input.socks_port),
    )?;
    let port = keep_unless_edited(
        current.port,
        |value| input.http_port == value.map_or(0, i32::from),
        || port_field("HTTP port", input.http_port),
    )?;
    let allow_lan = keep_unless_edited(
        current.allow_lan,
        |value| input.allow_lan == value.unwrap_or(false),
        || Ok(Some(input.allow_lan)),
    )?;
    let bind_address = keep_unless_edited(
        current.bind_address.clone(),
        |value| input.bind_address.trim() == value.as_deref().unwrap_or(""),
        || Ok(non_empty(input.bind_address.trim())),
    )?;
    let mode = keep_unless_edited(
        current.mode,
        |value| input.mode == value.unwrap_or_default().as_str(),
        || parse_proxy_mode(&input.mode).map(Some),
    )?;
    let log_level = keep_unless_edited(
        current.log_level,
        |value| input.log_level == value.unwrap_or_default().as_str(),
        || {
            LogLevel::from_str_exact(&input.log_level)
                .map(Some)
                .ok_or_else(|| invalid(format!("“{}” is not a log level", input.log_level)))
        },
    )?;
    let mtu = keep_unless_edited(
        current.tun.mtu,
        |value| input.tun_mtu == value.map_or(0, i32::from),
        || mtu_field(input.tun_mtu),
    )?;
    let device = keep_unless_edited(
        current.tun.device.clone(),
        |value| input.tun_device.trim() == value.as_deref().unwrap_or(""),
        || Ok(non_empty(input.tun_device.trim())),
    )?;
    let auto_refresh_minutes = keep_unless_edited(
        current.auto_refresh_minutes,
        |value| {
            input.auto_refresh_minutes
                == i32::try_from(value.unwrap_or(DEFAULT_REFRESH_MINUTES)).unwrap_or(i32::MAX)
        },
        || {
            u32::try_from(input.auto_refresh_minutes)
                .map(Some)
                .map_err(|_| invalid("the automatic refresh interval cannot be negative"))
        },
    )?;
    Ok(UserPreferences {
        mixed_port,
        socks_port,
        port,
        allow_lan,
        bind_address,
        mode,
        log_level,
        tun: TunSettings {
            enabled: input.tun_enabled,
            device,
            mtu,
            auto_route: current.tun.auto_route,
            dns_hijack: current.tun.dns_hijack.clone(),
        },
        auto_refresh_minutes,
        ..current.clone()
    })
}

/// Parse a routing mode from the UI.
pub fn parse_proxy_mode(text: &str) -> Result<ProxyMode, AppError> {
    ProxyMode::from_str_exact(text)
        .ok_or_else(|| invalid(format!("“{text}” is not a routing mode")))
}

/// Parse a subscription mode from the UI.
pub fn parse_subscription_mode(text: &str) -> Result<SubscriptionMode, AppError> {
    match text {
        "app-managed" => Ok(SubscriptionMode::AppManaged),
        "engine-managed" => Ok(SubscriptionMode::EngineManaged),
        other => Err(invalid(format!("“{other}” is not a subscription mode"))),
    }
}

/// The engine line for a rule the editor produced. `MATCH` takes only a
/// target, and a rule with no target (such as `SUB-RULE`) takes only a
/// payload. The parser decides whether the result is valid.
pub fn rule_line(kind: &str, payload: &str, target: &str) -> String {
    if kind == "MATCH" {
        format!("MATCH,{target}")
    } else if target.is_empty() {
        format!("{kind},{payload}")
    } else {
        format!("{kind},{payload},{target}")
    }
}

/// Parse one engine rule line. A rejected line is reported as the reason.
pub fn parse_rule_line(line: &str) -> Result<RuleSpec, AppError> {
    parse_line(line).map_err(AppError::Invalid)
}

/// The position a rule lands on after moving by `delta`, clamped to the list.
/// `None` when `index` is not a rule in the list.
pub fn moved_position(index: i32, delta: i32, len: usize) -> Option<usize> {
    let last = len.checked_sub(1)?;
    let from = usize::try_from(index).ok().filter(|from| *from <= last)?;
    let to = (from as i64 + i64::from(delta)).clamp(0, last as i64);
    usize::try_from(to).ok()
}

/// The level a log line is shown with, from the words in it.
pub fn detect_level(text: &str) -> &'static str {
    let lower = text.to_ascii_lowercase();
    let has = |words: &[&str]| {
        lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|token| words.contains(&token))
    };
    if has(&["error", "err", "fatal", "panic"]) {
        "error"
    } else if has(&["warn", "warning"]) {
        "warning"
    } else if has(&["debug", "trace"]) {
        "debug"
    } else {
        "info"
    }
}

/// A log line the service stamped: `<rfc3339>  <text>`.
fn app_item(line: &str) -> LogItem {
    let (time, message) = line.split_once("  ").unwrap_or(("", line));
    LogItem {
        time: time.into(),
        level: detect_level(message).into(),
        source: "app".into(),
        message: message.into(),
    }
}

/// An engine line. The engine's text carries no timestamp of its own here.
fn engine_item(line: &str) -> LogItem {
    LogItem {
        time: SharedString::default(),
        level: detect_level(line).into(),
        source: "engine".into(),
        message: line.into(),
    }
}

/// Whether a line of level `level` passes the Logs filter. `all` and the
/// empty filter pass everything.
pub fn level_passes(filter: &str, level: &str) -> bool {
    filter.is_empty() || filter == "all" || filter == level
}

/// The lines in a rolling window that have not been cleared. `start` is the
/// absolute index of `lines[0]`.
fn visible<'a>(
    lines: &'a [String],
    start: u64,
    cleared_before: u64,
) -> impl Iterator<Item = &'a str> + 'a {
    lines
        .iter()
        .enumerate()
        .filter(move |(index, _)| start + *index as u64 >= cleared_before)
        .map(|(_, line)| line.as_str())
}

/// The Logs screen: app and engine lines, oldest first, filtered by level,
/// minus anything cleared, keeping the last [`LOG_LINE_LIMIT`].
pub fn log_items(snap: &AppSnapshot, view: &ViewState) -> Vec<LogItem> {
    let app_start = view.app_lines.first_index(&snap.log_lines);
    let engine_start = view.engine_lines.first_index(&snap.engine_log_lines);
    let items: Vec<LogItem> = visible(&snap.log_lines, app_start, view.app_cleared_before)
        .map(app_item)
        .chain(
            visible(
                &snap.engine_log_lines,
                engine_start,
                view.engine_cleared_before,
            )
            .map(engine_item),
        )
        .filter(|item| level_passes(&view.log_filter, &item.level))
        .collect();
    let skip = items.len().saturating_sub(LOG_LINE_LIMIT);
    items.into_iter().skip(skip).collect()
}

/// A log screen as plain text, one entry per line.
pub fn log_text(items: &[LogItem]) -> String {
    items
        .iter()
        .map(|item| {
            format!(
                "{} [{}] {}: {}",
                item.time, item.level, item.source, item.message
            )
            .trim_start()
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Follows a rolling window of lines (the service keeps only the newest
/// ones) so that every line gets a stable absolute index. Used to hide lines
/// the user cleared.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineTracker {
    seen: u64,
    last: Option<String>,
}

impl LineTracker {
    /// How many lines have been seen in total.
    pub fn seen(&self) -> u64 {
        self.seen
    }

    /// The absolute index of `lines[0]`, given the window last seen.
    pub fn first_index(&self, lines: &[String]) -> u64 {
        let Some(last) = &self.last else {
            return self.seen;
        };
        match lines.iter().rposition(|line| line == last) {
            Some(position) => self.seen.saturating_sub(1 + position as u64),
            // Every line seen before has rolled out, so the window is all new.
            None => self.seen,
        }
    }

    /// Record the window now on screen.
    pub fn advance(&mut self, lines: &[String]) {
        let Some(newest) = lines.last() else {
            return;
        };
        self.seen = self.first_index(lines) + lines.len() as u64;
        self.last = Some(newest.clone());
    }
}

/// What the Rust side remembers between pushes, so that it pushes only what
/// changed and keeps the user's edits to the settings form and the raw rules.
#[derive(Debug, Clone, Default)]
pub struct ViewState {
    /// The Servers filter, as the UI last reported it.
    pub node_filter: String,
    /// The Logs level filter, as the UI last reported it.
    pub log_filter: String,
    /// The app's log window.
    pub app_lines: LineTracker,
    /// The engine's log window.
    pub engine_lines: LineTracker,
    /// Absolute index of the first app line still shown after a clear.
    pub app_cleared_before: u64,
    /// Absolute index of the first engine line still shown after a clear.
    pub engine_cleared_before: u64,
    applied_prefs: Option<UserPreferences>,
    applied_rules: Option<String>,
}

impl ViewState {
    /// Store the filters the UI shows. Returns whether either changed.
    pub fn set_filters(&mut self, node_filter: &str, log_filter: &str) -> bool {
        if self.node_filter == node_filter && self.log_filter == log_filter {
            return false;
        }
        self.node_filter = node_filter.to_string();
        self.log_filter = log_filter.to_string();
        true
    }

    /// Hide every line seen so far.
    pub fn clear_logs(&mut self) {
        self.app_cleared_before = self.app_lines.seen();
        self.engine_cleared_before = self.engine_lines.seen();
    }

    /// Remember that `snap` is on screen. Call it after pushing to every
    /// bridge, so the pushes all compare against the same previous state.
    pub fn record(&mut self, snap: &AppSnapshot) {
        self.app_lines.advance(&snap.log_lines);
        self.engine_lines.advance(&snap.engine_log_lines);
        self.applied_prefs = Some(snap.preferences.clone());
        self.applied_rules = Some(rules_raw_text(&snap.rules));
    }
}

/// Whether the model already holds exactly `next`.
fn matches_model<T: Clone + PartialEq + 'static>(model: &ModelRc<T>, next: &[T]) -> bool {
    model.row_count() == next.len()
        && next
            .iter()
            .enumerate()
            .all(|(row, item)| model.row_data(row).as_ref() == Some(item))
}

/// A new model holding `next`, or `None` when `current` already holds it.
fn changed_model<T: Clone + PartialEq + 'static>(
    current: ModelRc<T>,
    next: Vec<T>,
) -> Option<ModelRc<T>> {
    if matches_model(&current, &next) {
        None
    } else {
        Some(ModelRc::new(VecModel::from(next)))
    }
}

/// Push every snapshot field to a `Bridge`, setting only what changed.
///
/// The Bridge is one of two (the window's and the tray's), and both get the
/// same values. Pushing the settings form and the raw rules is decided by
/// `view`, not by the bridge, because the user edits those fields in place.
pub fn apply_snapshot(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    set_engine(bridge, snap);
    set_servers(bridge, snap, view);
    set_rules(bridge, snap, view);
    set_logs(bridge, snap, view);
    set_settings(bridge, snap, view);
    set_shell(bridge, snap);
}

fn set_engine(bridge: &Bridge<'_>, snap: &AppSnapshot) {
    bridge.set_loaded(true);
    bridge.set_engine_phase(phase_str(snap.engine.phase).into());
    bridge.set_engine_error(snap.engine.error.clone().unwrap_or_default().into());
    bridge.set_engine_installed(snap.engine.installed);
    bridge.set_engine_version(snap.engine.version.clone().unwrap_or_default().into());
    bridge.set_mode(snap.mode.as_str().into());
    bridge.set_current_node(
        snap.current_node
            .as_ref()
            .map(|node| node.name.clone())
            .unwrap_or_default()
            .into(),
    );
    bridge.set_uptime(
        snap.engine
            .uptime_secs
            .map(format_uptime)
            .unwrap_or_default()
            .into(),
    );
    bridge.set_listen_address(snap.listen_address.clone().unwrap_or_default().into());
}

fn set_servers(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    let subscriptions: Vec<SubscriptionItem> =
        snap.subscriptions.iter().map(subscription_item).collect();
    if let Some(model) = changed_model(bridge.get_subscriptions(), subscriptions) {
        bridge.set_subscriptions(model);
    }
    let nodes: Vec<NodeItem> = snap
        .nodes
        .iter()
        .filter(|node| node_matches(node, &view.node_filter))
        .map(node_item)
        .collect();
    if let Some(model) = changed_model(bridge.get_nodes(), nodes) {
        bridge.set_nodes(model);
    }
}

fn set_rules(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    let user: Vec<RuleItem> = snap.rules.iter().map(rule_item).collect();
    if let Some(model) = changed_model(bridge.get_user_rules(), user) {
        bridge.set_user_rules(model);
    }
    let subscription: Vec<RuleItem> = snap.subscription_rules.iter().map(rule_item).collect();
    if let Some(model) = changed_model(bridge.get_subscription_rules(), subscription) {
        bridge.set_subscription_rules(model);
    }
    let kinds: Vec<SharedString> = snap
        .rule_kinds
        .iter()
        .map(|kind| SharedString::from(kind.token.as_str()))
        .collect();
    if let Some(model) = changed_model(bridge.get_rule_kinds(), kinds) {
        bridge.set_rule_kinds(model);
    }
    let targets: Vec<SharedString> = snap
        .rule_targets
        .iter()
        .map(|target| SharedString::from(target.as_str()))
        .collect();
    if let Some(model) = changed_model(bridge.get_rule_targets(), targets) {
        bridge.set_rule_targets(model);
    }
    let raw = rules_raw_text(&snap.rules);
    if view.applied_rules.as_deref() != Some(raw.as_str()) {
        bridge.set_rules_raw_text(raw.into());
    }
}

fn set_logs(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    if let Some(model) = changed_model(bridge.get_logs(), log_items(snap, view)) {
        bridge.set_logs(model);
    }
}

fn set_settings(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    if view.applied_prefs.as_ref() != Some(&snap.preferences) {
        bridge.set_settings(settings_model(&snap.preferences));
    }
}

fn set_shell(bridge: &Bridge<'_>, snap: &AppSnapshot) {
    bridge.set_secret_backend(snap.secret_backend.as_str().into());
    bridge.set_secrets_encrypted(snap.secret_encrypted);
    bridge.set_data_dir(snap.data_dir.display().to_string().into());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EngineView;

    fn node(name: &str, group: Option<&str>, protocol: &str) -> NodeView {
        NodeView {
            id: format!("id-{name}"),
            name: name.to_string(),
            protocol: protocol.to_string(),
            group: group.map(str::to_string),
            selected: false,
            latency_ms: None,
        }
    }

    fn subscription(quota: Option<SubscriptionQuota>) -> SubscriptionView {
        SubscriptionView {
            id: "sub-1".to_string(),
            label: "Main".to_string(),
            mode: SubscriptionMode::AppManaged,
            status: SubscriptionStatus::Failing,
            last_updated: Some("2026-11-13T10:05:33Z".to_string()),
            error: Some("the provider refused".to_string()),
            node_count: 12,
            group_count: 2,
            quota,
            has_auth_header: false,
        }
    }

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn uptime_reads_as_hours_minutes_and_seconds() {
        assert_eq!(format_uptime(3720), "1h 02m");
        assert_eq!(format_uptime(249), "4m 09s");
        assert_eq!(format_uptime(42), "42s");
    }

    #[test]
    fn bytes_use_decimal_units_without_a_trailing_point_zero() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(12_300_000_000), "12.3 GB");
        assert_eq!(format_bytes(100_000_000_000), "100 GB");
        assert_eq!(format_bytes(1_500), "1.5 KB");
    }

    #[test]
    fn quota_shows_a_fraction_and_text_only_for_a_known_total() {
        let known = SubscriptionQuota {
            used: 12_300_000_000,
            total: Some(100_000_000_000),
            expires: Some("2026-12-01T00:00:00Z".to_string()),
        };
        assert_eq!(quota_text(&known), "12.3 GB of 100 GB");
        assert!((quota_fraction(&known) - 0.123).abs() < 1e-6);

        let unknown = SubscriptionQuota {
            used: 5_000_000_000,
            total: None,
            expires: None,
        };
        assert_eq!(quota_text(&unknown), "5 GB used");
        assert_eq!(quota_fraction(&unknown), 0.0);
    }

    #[test]
    fn a_subscription_row_carries_its_quota_and_error() {
        let item = subscription_item(&subscription(Some(SubscriptionQuota {
            used: 1_000_000_000,
            total: Some(4_000_000_000),
            expires: Some("2026-12-01T00:00:00Z".to_string()),
        })));
        assert_eq!(item.status, "error");
        assert_eq!(item.mode, "app-managed");
        assert_eq!(item.last_updated, "2026-11-13 10:05");
        assert_eq!(item.error, "the provider refused");
        assert_eq!(item.node_count, 12);
        assert!(item.has_quota);
        assert_eq!(item.quota_text, "1 GB of 4 GB");
        assert_eq!(item.expires, "2026-12-01");
    }

    #[test]
    fn a_subscription_without_quota_has_no_quota_fields() {
        let item = subscription_item(&subscription(None));
        assert!(!item.has_quota);
        assert_eq!(item.quota_text, "");
        assert_eq!(item.expires, "");
        assert_eq!(item.quota_used_fraction, 0.0);
    }

    #[test]
    fn engine_phases_and_statuses_use_the_contract_words() {
        assert_eq!(phase_str(EnginePhase::Stopped), "stopped");
        assert_eq!(phase_str(EnginePhase::Failed), "failed");
        assert_eq!(status_str(SubscriptionStatus::NeverFetched), "never");
        assert_eq!(status_str(SubscriptionStatus::EngineManaged), "ok");
    }

    #[test]
    fn node_filter_is_a_case_insensitive_substring_match() {
        let tokyo = node("Tokyo-01", Some("Asia"), "vless");
        assert!(node_matches(&tokyo, ""));
        assert!(node_matches(&tokyo, "   "));
        assert!(node_matches(&tokyo, "tokyo"));
        assert!(node_matches(&tokyo, "ASIA"));
        assert!(node_matches(&tokyo, "VLE"));
        assert!(!node_matches(&tokyo, "paris"));
    }

    #[test]
    fn an_untested_node_has_latency_minus_one() {
        let mut fast = node("a", None, "ss");
        assert_eq!(node_item(&fast).latency_ms, -1);
        assert_eq!(node_item(&fast).group, "");
        fast.latency_ms = Some(87);
        assert_eq!(node_item(&fast).latency_ms, 87);
    }

    #[test]
    fn rule_raw_text_joins_the_lines_in_order() {
        let rule = |index: usize, line: &str| RuleView {
            index,
            id: Some(format!("r{index}")),
            enabled: true,
            kind: "DOMAIN".to_string(),
            payload: String::new(),
            target: String::new(),
            line: line.to_string(),
            from_subscription: false,
        };
        let rules = vec![rule(0, "DOMAIN,a.test,DIRECT"), rule(1, "MATCH,PROXY")];
        assert_eq!(rules_raw_text(&rules), "DOMAIN,a.test,DIRECT\nMATCH,PROXY");
        assert_eq!(rules_raw_text(&[]), "");
    }

    #[test]
    fn settings_show_zero_for_ports_that_are_off() {
        let prefs = UserPreferences {
            mixed_port: None,
            socks_port: Some(7891),
            port: None,
            ..UserPreferences::default()
        };
        let model = settings_model(&prefs);
        assert_eq!(model.mixed_port, 0);
        assert_eq!(model.socks_port, 7891);
        assert_eq!(model.http_port, 0);
        assert_eq!(model.auto_refresh_minutes, 60);
        assert_eq!(model.log_level, "info");
    }

    fn input_from(prefs: &UserPreferences) -> SettingsInput {
        SettingsInput::from_model(&settings_model(prefs))
    }

    #[test]
    fn saving_the_form_round_trips_and_keeps_selection() {
        let current = UserPreferences {
            mixed_port: Some(7890),
            selected_node_id: Some("node-7".to_string()),
            selected_group: Some("Auto".to_string()),
            ..UserPreferences::default()
        };
        let saved = build_preferences(&current, &input_from(&current)).unwrap();
        assert_eq!(saved.mixed_port, Some(7890));
        assert_eq!(saved.selected_node_id.as_deref(), Some("node-7"));
        assert_eq!(saved.selected_group.as_deref(), Some("Auto"));
        // Untouched fields keep "not chosen", so a subscription default still applies.
        assert_eq!(saved.allow_lan, None);
        assert_eq!(saved.mode, None);
        assert_eq!(saved.log_level, None);
        assert_eq!(saved.auto_refresh_minutes, None);
    }

    #[test]
    fn an_edited_field_is_saved_and_an_untouched_one_is_not() {
        let current = UserPreferences::default();
        let mut input = input_from(&current);
        input.allow_lan = true;
        input.mode = "global".to_string();
        let saved = build_preferences(&current, &input).unwrap();
        assert_eq!(saved.allow_lan, Some(true));
        assert_eq!(saved.mode, Some(ProxyMode::Global));
        assert_eq!(saved.mixed_port, current.mixed_port);
    }

    #[test]
    fn a_zero_port_saves_as_off() {
        let current = UserPreferences {
            socks_port: Some(7891),
            ..UserPreferences::default()
        };
        let mut input = input_from(&current);
        input.socks_port = 0;
        let saved = build_preferences(&current, &input).unwrap();
        assert_eq!(saved.socks_port, None);
    }

    #[test]
    fn out_of_range_settings_are_rejected_with_a_reason() {
        let current = UserPreferences::default();

        let mut input = input_from(&current);
        input.mixed_port = 70_000;
        let error = build_preferences(&current, &input).unwrap_err();
        assert!(error.to_string().contains("mixed port"));

        let mut input = input_from(&current);
        input.tun_mtu = 900;
        assert!(build_preferences(&current, &input).is_err());

        let mut input = input_from(&current);
        input.log_level = "verbose".to_string();
        assert!(build_preferences(&current, &input).is_err());

        let mut input = input_from(&current);
        input.auto_refresh_minutes = -5;
        assert!(build_preferences(&current, &input).is_err());
    }

    #[test]
    fn rule_lines_are_built_from_the_editor_fields() {
        assert_eq!(
            rule_line("DOMAIN-SUFFIX", "example.com", "DIRECT"),
            "DOMAIN-SUFFIX,example.com,DIRECT"
        );
        assert_eq!(rule_line("MATCH", "", "PROXY"), "MATCH,PROXY");
        assert_eq!(rule_line("SUB-RULE", "block", ""), "SUB-RULE,block");
    }

    #[test]
    fn a_valid_rule_line_parses_and_a_bad_one_reports_why() {
        assert!(parse_rule_line("DOMAIN-SUFFIX,example.com,DIRECT").is_ok());
        assert!(parse_rule_line("MATCH,DIRECT").is_ok());
        assert!(matches!(
            parse_rule_line("NOT-A-KIND,x,DIRECT"),
            Err(AppError::Invalid(_))
        ));
        assert!(parse_rule_line("").is_err());
    }

    #[test]
    fn moving_a_rule_clamps_to_the_list() {
        assert_eq!(moved_position(0, 1, 3), Some(1));
        assert_eq!(moved_position(2, 1, 3), Some(2));
        assert_eq!(moved_position(0, -1, 3), Some(0));
        assert_eq!(moved_position(1, -5, 3), Some(0));
        assert_eq!(moved_position(5, 1, 3), None);
        assert_eq!(moved_position(0, 1, 0), None);
        assert_eq!(moved_position(-1, 1, 3), None);
    }

    #[test]
    fn levels_are_read_from_the_words_in_a_line() {
        assert_eq!(detect_level("level=error dial failed"), "error");
        assert_eq!(detect_level("WARN slow handshake"), "warning");
        assert_eq!(detect_level("debug: tick"), "debug");
        assert_eq!(detect_level("the engine started"), "info");
        assert!(level_passes("all", "error"));
        assert!(level_passes("", "info"));
        assert!(!level_passes("error", "info"));
    }

    #[test]
    fn a_line_tracker_gives_stable_indexes_as_the_window_rolls() {
        let mut tracker = LineTracker::default();
        tracker.advance(&lines(&["a", "b", "c"]));
        assert_eq!(tracker.seen(), 3);
        assert_eq!(tracker.first_index(&lines(&["a", "b", "c", "d"])), 0);

        // The window rolls: "a" is gone, "d" arrives.
        assert_eq!(tracker.first_index(&lines(&["b", "c", "d"])), 1);
        tracker.advance(&lines(&["b", "c", "d"]));
        assert_eq!(tracker.seen(), 4);

        // Everything seen so far has rolled out.
        assert_eq!(tracker.first_index(&lines(&["x", "y"])), 4);
    }

    fn snapshot_with_logs(app: &[&str], engine: &[&str]) -> AppSnapshot {
        AppSnapshot {
            log_lines: lines(app),
            engine_log_lines: lines(engine),
            engine: EngineView::default(),
            ..AppSnapshot::default()
        }
    }

    #[test]
    fn cleared_logs_stay_hidden_and_newer_lines_show() {
        let mut view = ViewState::default();
        let first = snapshot_with_logs(
            &["2026-01-01T00:00:00Z  one", "2026-01-01T00:00:01Z  two"],
            &[],
        );
        view.record(&first);
        view.clear_logs();
        assert!(log_items(&first, &view).is_empty());

        let next = snapshot_with_logs(
            &[
                "2026-01-01T00:00:00Z  one",
                "2026-01-01T00:00:01Z  two",
                "2026-01-01T00:00:02Z  three",
            ],
            &[],
        );
        let shown = log_items(&next, &view);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].message, "three");
        assert_eq!(shown[0].time, "2026-01-01T00:00:02Z");
        assert_eq!(shown[0].source, "app");
    }

    #[test]
    fn the_logs_keep_only_the_last_two_thousand_lines() {
        let many: Vec<String> = (0..2500)
            .map(|i| format!("2026-01-01T00:00:00Z  line {i}"))
            .collect();
        let snap = AppSnapshot {
            log_lines: many,
            ..AppSnapshot::default()
        };
        let view = ViewState::default();
        let shown = log_items(&snap, &view);
        assert_eq!(shown.len(), LOG_LINE_LIMIT);
        assert_eq!(shown[0].message, "line 500");
        assert_eq!(shown.last().unwrap().message, "line 2499");
    }

    #[test]
    fn the_level_filter_applies_to_both_sources() {
        let snap = snapshot_with_logs(
            &["2026-01-01T00:00:00Z  connection error"],
            &["info: ready"],
        );
        let mut view = ViewState::default();
        assert_eq!(log_items(&snap, &view).len(), 2);
        view.log_filter = "error".to_string();
        let shown = log_items(&snap, &view);
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].source, "app");
    }

    #[test]
    fn log_text_keeps_every_visible_entry() {
        let items = vec![
            LogItem {
                time: "t".into(),
                level: "info".into(),
                source: "app".into(),
                message: "hello".into(),
            },
            LogItem {
                time: "".into(),
                level: "error".into(),
                source: "engine".into(),
                message: "boom".into(),
            },
        ];
        assert_eq!(
            log_text(&items),
            "t [info] app: hello\n[error] engine: boom"
        );
    }

    #[test]
    fn a_model_is_replaced_only_when_its_contents_differ() {
        let model: ModelRc<i32> = ModelRc::new(VecModel::from(vec![1, 2]));
        assert!(matches_model(&model, &[1, 2]));
        assert!(changed_model(model.clone(), vec![1, 2]).is_none());
        assert!(changed_model(model.clone(), vec![1, 2, 3]).is_some());
        assert!(changed_model(model, vec![2, 1]).is_some());
    }

    #[test]
    fn a_notice_becomes_a_banner_of_the_matching_kind() {
        assert_eq!(
            notice_banner(&(NoticeKind::Error, "x".into())).kind,
            "error"
        );
        assert_eq!(
            notice_banner(&(NoticeKind::Warning, "x".into())).kind,
            "info"
        );
        assert_eq!(Banner::success("ok").kind, "success");
    }

    #[test]
    fn a_date_is_shortened_to_the_day() {
        assert_eq!(short_date("2026-12-01T00:00:00Z"), "2026-12-01");
        assert_eq!(short_date("next month"), "next month");
        assert_eq!(display_time("2026-11-13T10:05:33Z"), "2026-11-13 10:05");
    }
}
