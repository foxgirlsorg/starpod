//! Pure mapping from the app's view state to the Slint `Bridge` contract.
//!
//! Nothing here decides anything about the proxy. The functions turn
//! [`AppSnapshot`] values and UI filters into the strings, numbers and lists
//! the `.slint` files render (ARCHITECTURE §6.3). Only [`apply_snapshot`]
//! touches a `Bridge`, and it only sets properties. The rest is testable
//! without a display.

use std::collections::BTreeSet;

use crate::compose::unsupported_rule;
use crate::connections::{self, ConnectionView};
use crate::engine_form::EngineForm;
use crate::snapshot::ProfileCard;
use crate::ui::emoji::runs_model;
use crate::{
    AnnouncementView, AppError, AppSnapshot, GroupView, Latency, MemberView, NodeView, NoticeKind,
    RuleView, SubscriptionStatus, SubscriptionView,
};
use slint::{Color, Image, Model, ModelRc, SharedString, VecModel};
use starpod_core::domain::SystemProxyMode;
use starpod_core::rules::parse_line;
use starpod_core::{
    LogLevel, ProxyMode, RuleSpec, SubscriptionMode, SubscriptionQuota, TunSettings,
    UserPreferences,
};
use starpod_engine::EnginePhase;
use starpod_subscription::metadata::DEFAULT_INTERVAL_SECS;
use starpod_ui::{
    AnnouncementItem, Bridge, ConnectionItem, EngineSettingsModel, GroupItem, LogItem, MemberItem,
    NodeItem, ProfileCardItem, RuleItem, SelectItem, SettingsModel, SubscriptionItem, WarningItem,
};

/// The most log lines the Logs screen keeps.
pub const LOG_LINE_LIMIT: usize = 2000;
/// The auto-refresh interval used when the user has not chosen one.
pub const DEFAULT_REFRESH_MINUTES: u32 = (DEFAULT_INTERVAL_SECS / 60) as u32;
/// The smallest TUN MTU the app accepts: the IPv6 minimum (RFC 8200). TUN
/// carries IPv6 as well as IPv4, and an IPv6 packet cannot be fragmented by
/// a router, so a smaller MTU breaks IPv6 traffic. The Settings screen uses
/// the same bound; keep the two in step.
const MIN_TUN_MTU: u16 = 1280;
/// The largest TUN MTU the app accepts. The Settings screen uses the same bound.
const MAX_TUN_MTU: u16 = 9000;
/// How the settings form shows a number that is not set: `-1`, which shows as
/// an empty field. Every optional number on the form uses it. `0` is a real
/// value for the auto refresh (manual only), and an invalid port or MTU, so
/// it cannot stand for "not set".
pub const NOT_SET: i32 = -1;

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
        crate::i18n::fmt("{}h {}m", &[&hours.to_string(), &format!("{minutes:02}")])
    } else if minutes > 0 {
        crate::i18n::fmt("{}m {}s", &[&minutes.to_string(), &format!("{seconds:02}")])
    } else {
        crate::i18n::fmt("{}s", &[&seconds.to_string()])
    }
}

/// A byte count in decimal units: `12.3 GB`, `100 GB`, `512 B`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} {}", crate::i18n::t("B"));
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    let text = format!("{value:.1}");
    let text = text.strip_suffix(".0").unwrap_or(&text);
    format!("{text} {}", crate::i18n::t(UNITS[unit]))
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
/// A total of `0` is the providers' way of saying there is no limit.
pub fn quota_text(quota: &SubscriptionQuota) -> String {
    match quota.total {
        Some(total) if total > 0 => crate::i18n::fmt(
            "{} of {}",
            &[&format_bytes(quota.used), &format_bytes(total)],
        ),
        _ => crate::i18n::fmt("{} used", &[&format_bytes(quota.used)]),
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
            // `expire=0` is a plan with no end date, not one that ended in 1970.
            quota
                .expires
                .as_deref()
                .filter(|text| starpod_core::timestamp::parse(text).is_some_and(|unix| unix > 0))
                .map(short_date)
                .unwrap_or_default(),
        ),
        None => (false, 0.0, String::new(), String::new()),
    };
    let error = view.error.clone().unwrap_or_default();
    SubscriptionItem {
        active: view.active,
        id: view.id.as_str().into(),
        label: view.label.as_str().into(),
        title: view.title.as_str().into(),
        mode: view.mode.as_str().into(),
        status: status_str(view.status).into(),
        last_updated: view
            .last_updated
            .as_deref()
            .map(display_time)
            .unwrap_or_default()
            .into(),
        error: error.as_str().into(),
        error_runs: runs_model(&error),
        title_runs: runs_model(&view.title),
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
        name_runs: runs_model(&node.name),
        group_runs: runs_model(node.group.as_deref().unwrap_or_default()),
    }
}

/// A latency as the Bridge shows it: `-1` untested, `0` timed out, otherwise
/// the milliseconds.
pub fn latency_ms(latency: Latency) -> i32 {
    match latency {
        Latency::Untested => -1,
        Latency::TimedOut => 0,
        Latency::Measured(milliseconds) => saturating_i32(milliseconds),
    }
}

/// A latency's text: `—` untested, `timeout`, otherwise `<n> ms`.
pub fn latency_text(ms: i32) -> String {
    match ms {
        -1 => "—".to_string(),
        0 => crate::i18n::t("timeout").to_string(),
        _ => crate::i18n::fmt("{} ms", &[&ms.to_string()]),
    }
}

/// A latency's colour: grey untested, red timed out, green under 200 ms,
/// amber under 500 ms, red from 500 ms.
pub fn latency_color(ms: i32) -> Color {
    match ms {
        -1 => Color::from_rgb_u8(0x71, 0x71, 0x7a),
        0 => Color::from_rgb_u8(0xef, 0x44, 0x44),
        ms if ms < 200 => Color::from_rgb_u8(0x4a, 0xde, 0x80),
        ms if ms < 500 => Color::from_rgb_u8(0xfb, 0xbf, 0x24),
        _ => Color::from_rgb_u8(0xef, 0x44, 0x44),
    }
}

/// The text to show for a row: the label, or the engine name when there is
/// no label.
fn shown_text(label: &str, name: &str) -> String {
    if label.is_empty() { name } else { label }.to_string()
}

/// The Select row for one member: its label, with its latency as the detail.
pub fn member_choice(member: &MemberView) -> SelectItem {
    let ms = latency_ms(member.latency);
    let shown = shown_text(&member.label, &member.name);
    SelectItem {
        text_runs: runs_model(&shown),
        text: shown.into(),
        detail: latency_text(ms).into(),
        detail_color: latency_color(ms),
    }
}

/// The Select rows for a group's members, in configuration order.
pub fn member_choices(members: &[MemberView]) -> ModelRc<SelectItem> {
    ModelRc::new(VecModel::from(
        members.iter().map(member_choice).collect::<Vec<_>>(),
    ))
}

/// The Select rows for the groups: the label of each, with no detail.
pub fn group_choices(groups: &[GroupView]) -> Vec<SelectItem> {
    groups
        .iter()
        .map(|group| {
            let shown = shown_text(&group.label, &group.name);
            SelectItem {
                text_runs: runs_model(&shown),
                text: shown.into(),
                detail: SharedString::default(),
                detail_color: Color::from_argb_u8(0, 0, 0, 0),
            }
        })
        .collect()
}

/// One member of a group.
pub fn member_item(member: &MemberView) -> MemberItem {
    let shown = shown_text(&member.label, &member.name);
    MemberItem {
        name: member.name.as_str().into(),
        protocol: member.protocol.as_str().into(),
        latency_ms: latency_ms(member.latency),
        label_runs: runs_model(&shown),
        label: shown.into(),
    }
}

/// How a group's members are listed on the Proxies page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemberOrder {
    /// `0` as configured, `1` by latency, `2` by name. See [`ordered_members`].
    pub sort: i32,
    /// Whether the members whose last latency test timed out are left out.
    pub hide_unavailable: bool,
}

/// The rank a latency sorts by: measured fastest first, then untested, then
/// timed out, so the members that did not answer come last.
fn latency_rank(latency: Latency) -> (u8, u32) {
    match latency {
        Latency::Measured(milliseconds) => (0, milliseconds),
        Latency::Untested => (1, 0),
        Latency::TimedOut => (2, 0),
    }
}

/// A group's members in the order the page shows them: as configured (`0`),
/// by latency (`1`), or by the name shown (`2`). Timed-out members are left
/// out when `hide_unavailable` is set. Each sort is stable, so members that
/// compare equal keep their configured order.
pub fn ordered_members(members: &[MemberView], order: MemberOrder) -> Vec<MemberView> {
    let mut shown: Vec<MemberView> = members
        .iter()
        .filter(|member| !(order.hide_unavailable && member.latency == Latency::TimedOut))
        .cloned()
        .collect();
    match order.sort {
        1 => shown.sort_by_key(|member| latency_rank(member.latency)),
        2 => shown
            .sort_by_cached_key(|member| shown_text(&member.label, &member.name).to_lowercase()),
        _ => {}
    }
    shown
}

/// One proxy group, its members in configuration order and expanded.
pub fn group_item(group: &GroupView) -> GroupItem {
    group_item_in(group, MemberOrder::default(), true)
}

/// One proxy group as the Proxies page shows it: the members ordered and
/// filtered by `order`, and `expanded` as the page keeps it. `selected_index`
/// is the chosen member's place in the shown order, or `-1` when no member is
/// chosen or the chosen one is hidden. `selected` is `""` when no member is
/// known. The label is the shown name: the label, or the engine name when
/// there is none.
pub fn group_item_in(group: &GroupView, order: MemberOrder, expanded: bool) -> GroupItem {
    let shown = shown_text(&group.label, &group.name);
    let selected = group.selected.clone().unwrap_or_default();
    let members = ordered_members(&group.members, order);
    let selected_index = if selected.is_empty() {
        -1
    } else {
        members
            .iter()
            .position(|member| member.name == selected)
            .map_or(-1, saturating_i32)
    };
    GroupItem {
        name: group.name.as_str().into(),
        kind: group.kind.as_str().into(),
        selected_runs: runs_model(&selected),
        selected: selected.into(),
        members: ModelRc::new(VecModel::from(
            members.iter().map(member_item).collect::<Vec<_>>(),
        )),
        label_runs: runs_model(&shown),
        label: shown.into(),
        member_choices: member_choices(&members),
        expanded,
        selected_index,
    }
}

/// A rate as the Connections page shows it: `12 KB/s`.
pub fn speed_text(bytes_per_second: u64) -> String {
    crate::i18n::fmt("{}/s", &[&format_bytes(bytes_per_second)])
}

/// One connection row.
pub fn connection_item(view: &ConnectionView) -> ConnectionItem {
    let chain = view.chain_text();
    ConnectionItem {
        id: view.id.as_str().into(),
        host: view.host.as_str().into(),
        host_runs: runs_model(&view.host),
        destination: view.destination.as_str().into(),
        network: view.network.as_str().into(),
        kind: view.kind.as_str().into(),
        process: view.process.as_str().into(),
        chain_runs: runs_model(&chain),
        chain: chain.into(),
        rule: view.rule_text().into(),
        upload: format_bytes(view.upload).into(),
        download: format_bytes(view.download).into(),
        upload_speed: speed_text(view.upload_speed).into(),
        download_speed: speed_text(view.download_speed).into(),
        start: connections::clock_text(view.started_at.as_deref()).into(),
        duration: format_uptime(view.duration_secs).into(),
        closed: view.closed,
    }
}

/// The connection rows the page shows: those matching `filter`, sorted by
/// `sort`.
pub fn connection_rows(views: &[ConnectionView], filter: &str, sort: i32) -> Vec<ConnectionItem> {
    let mut shown: Vec<ConnectionView> = views
        .iter()
        .filter(|view| connections::matches(view, filter))
        .cloned()
        .collect();
    connections::sort(&mut shown, sort);
    shown.iter().map(connection_item).collect()
}

/// Whether two connection rows hold the same values.
fn same_connection(a: &ConnectionItem, b: &ConnectionItem) -> bool {
    a.id == b.id
        && a.host == b.host
        && a.destination == b.destination
        && a.network == b.network
        && a.kind == b.kind
        && a.process == b.process
        && a.chain == b.chain
        && a.rule == b.rule
        && a.upload == b.upload
        && a.download == b.download
        && a.upload_speed == b.upload_speed
        && a.download_speed == b.download_speed
        && a.start == b.start
        && a.duration == b.duration
        && a.closed == b.closed
}

/// One announcement card. Missing links are `""`.
pub fn announcement_item(announcement: &AnnouncementView) -> AnnouncementItem {
    AnnouncementItem {
        subscription_id: announcement.subscription_id.as_str().into(),
        title: announcement.title.as_str().into(),
        title_runs: runs_model(&announcement.title),
        text: announcement.text.as_str().into(),
        text_runs: runs_model(&announcement.text),
        announce_url: announcement.announce_url.clone().unwrap_or_default().into(),
        support_url: announcement.support_url.clone().unwrap_or_default().into(),
        web_url: announcement.web_url.clone().unwrap_or_default().into(),
    }
}

// The diff compares the source strings of each row, never the runs or the
// images. The runs are made from those strings, so equal strings give equal
// runs, and `Image` equality (two empty images differ) is not needed.

/// Whether two group rows hold the same source values.
fn same_group(current: &GroupItem, next: &GroupItem) -> bool {
    current.name == next.name
        && current.kind == next.kind
        && current.selected == next.selected
        && current.label == next.label
        && same_model(&current.members, &next.members, same_member)
        && same_model(&current.member_choices, &next.member_choices, same_select)
        && current.expanded == next.expanded
        && current.selected_index == next.selected_index
}

fn same_member(a: &MemberItem, b: &MemberItem) -> bool {
    a.name == b.name
        && a.protocol == b.protocol
        && a.latency_ms == b.latency_ms
        && a.label == b.label
}

fn same_select(a: &SelectItem, b: &SelectItem) -> bool {
    a.text == b.text && a.detail == b.detail && a.detail_color == b.detail_color
}

fn same_node(a: &NodeItem, b: &NodeItem) -> bool {
    a.id == b.id
        && a.name == b.name
        && a.protocol == b.protocol
        && a.group == b.group
        && a.latency_ms == b.latency_ms
        && a.selected == b.selected
}

fn same_subscription(a: &SubscriptionItem, b: &SubscriptionItem) -> bool {
    a.id == b.id
        && a.label == b.label
        && a.title == b.title
        && a.mode == b.mode
        && a.status == b.status
        && a.last_updated == b.last_updated
        && a.error == b.error
        && a.node_count == b.node_count
        && a.has_quota == b.has_quota
        && a.quota_used_fraction == b.quota_used_fraction
        && a.quota_text == b.quota_text
        && a.expires == b.expires
}

fn same_rule(a: &RuleItem, b: &RuleItem) -> bool {
    a.index == b.index
        && a.kind == b.kind
        && a.payload == b.payload
        && a.target == b.target
        && a.line == b.line
        && a.enabled == b.enabled
        && a.from_subscription == b.from_subscription
        && a.error == b.error
}

fn same_announcement(a: &AnnouncementItem, b: &AnnouncementItem) -> bool {
    a.subscription_id == b.subscription_id
        && a.title == b.title
        && a.text == b.text
        && a.support_url == b.support_url
        && a.web_url == b.web_url
}

fn same_log(a: &LogItem, b: &LogItem) -> bool {
    a.time == b.time && a.level == b.level && a.source == b.source && a.message == b.message
}

fn same_warning(a: &WarningItem, b: &WarningItem) -> bool {
    a.text == b.text
}

/// Whether two models hold the same rows in the same order, under `same`.
fn same_model<T: Clone + 'static>(
    current: &ModelRc<T>,
    next: &ModelRc<T>,
    same: fn(&T, &T) -> bool,
) -> bool {
    current.row_count() == next.row_count()
        && (0..current.row_count()).all(|row| match (current.row_data(row), next.row_data(row)) {
            (Some(a), Some(b)) => same(&a, &b),
            _ => false,
        })
}

/// Whether the groups model must be replaced to show `next`.
fn groups_changed(current: &ModelRc<GroupItem>, next: &[GroupItem]) -> bool {
    current.row_count() != next.len()
        || next
            .iter()
            .enumerate()
            .any(|(row, item)| match current.row_data(row) {
                Some(old) => !same_group(&old, item),
                None => true,
            })
}

/// One rule row.
pub fn rule_item(rule: &RuleView) -> RuleItem {
    RuleItem {
        index: saturating_i32(rule.index),
        kind: rule.kind.as_str().into(),
        payload: rule.payload.as_str().into(),
        payload_runs: runs_model(&rule.payload),
        target: rule.target.as_str().into(),
        target_runs: runs_model(&rule.target),
        line: rule.line.as_str().into(),
        line_runs: runs_model(&rule.line),
        enabled: rule.enabled,
        from_subscription: rule.from_subscription,
        error: SharedString::default(),
        error_runs: runs_model(""),
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

/// The engine settings form, from its plain-value twin. Field for field: the
/// Bridge model and [`EngineForm`] use the same conventions.
pub fn engine_settings_model(form: &EngineForm) -> EngineSettingsModel {
    EngineSettingsModel {
        tun_stack: form.tun_stack.clone().into(),
        tun_strict_route: form.tun_strict_route,
        tun_auto_redirect: form.tun_auto_redirect,
        tun_auto_detect_interface: form.tun_auto_detect_interface,
        tun_endpoint_independent_nat: form.tun_endpoint_independent_nat,
        tun_route_address: form.tun_route_address.clone().into(),
        tun_route_exclude_address: form.tun_route_exclude_address.clone().into(),
        ipv6: form.ipv6,
        unified_delay: form.unified_delay,
        tcp_concurrent: form.tcp_concurrent,
        find_process_mode: form.find_process_mode.clone().into(),
        keep_alive_interval: form.keep_alive_interval,
        override_dns: form.override_dns,
        dns_enable: form.dns_enable,
        dns_enhanced_mode: form.dns_enhanced_mode.clone().into(),
        dns_fake_ip_range: form.dns_fake_ip_range.clone().into(),
        dns_fake_ip_filter: form.dns_fake_ip_filter.clone().into(),
        dns_nameserver: form.dns_nameserver.clone().into(),
        dns_fallback: form.dns_fallback.clone().into(),
        dns_default_nameserver: form.dns_default_nameserver.clone().into(),
        dns_proxy_server_nameserver: form.dns_proxy_server_nameserver.clone().into(),
        dns_nameserver_policy: form.dns_nameserver_policy.clone().into(),
        dns_use_hosts: form.dns_use_hosts,
        hosts: form.hosts.clone().into(),
        sniffer_enable: form.sniffer_enable,
        sniffer_force_dns_mapping: form.sniffer_force_dns_mapping,
        sniffer_parse_pure_ip: form.sniffer_parse_pure_ip,
        sniffer_override_destination: form.sniffer_override_destination,
        sniffer_sniff: form.sniffer_sniff.clone().into(),
        sniffer_force_domain: form.sniffer_force_domain.clone().into(),
        sniffer_skip_domain: form.sniffer_skip_domain.clone().into(),
        geodata_mode: form.geodata_mode.clone().into(),
        geox_geoip: form.geox_geoip.clone().into(),
        geox_geosite: form.geox_geosite.clone().into(),
        geox_mmdb: form.geox_mmdb.clone().into(),
        geox_asn: form.geox_asn.clone().into(),
        geodata_auto_update: form.geodata_auto_update,
        geodata_update_interval_hours: form.geodata_update_interval_hours,
        delay_test_url: form.delay_test_url.clone().into(),
        delay_test_timeout_ms: form.delay_test_timeout_ms,
        log_retention_lines: form.log_retention_lines,
        engine_channel: form.engine_channel.clone().into(),
    }
}

/// The plain-value form from what the engine settings page sent back.
pub fn engine_form_from_model(model: &EngineSettingsModel) -> EngineForm {
    EngineForm {
        tun_stack: model.tun_stack.to_string(),
        tun_strict_route: model.tun_strict_route,
        tun_auto_redirect: model.tun_auto_redirect,
        tun_auto_detect_interface: model.tun_auto_detect_interface,
        tun_endpoint_independent_nat: model.tun_endpoint_independent_nat,
        tun_route_address: model.tun_route_address.to_string(),
        tun_route_exclude_address: model.tun_route_exclude_address.to_string(),
        ipv6: model.ipv6,
        unified_delay: model.unified_delay,
        tcp_concurrent: model.tcp_concurrent,
        find_process_mode: model.find_process_mode.to_string(),
        keep_alive_interval: model.keep_alive_interval,
        override_dns: model.override_dns,
        dns_enable: model.dns_enable,
        dns_enhanced_mode: model.dns_enhanced_mode.to_string(),
        dns_fake_ip_range: model.dns_fake_ip_range.to_string(),
        dns_fake_ip_filter: model.dns_fake_ip_filter.to_string(),
        dns_nameserver: model.dns_nameserver.to_string(),
        dns_fallback: model.dns_fallback.to_string(),
        dns_default_nameserver: model.dns_default_nameserver.to_string(),
        dns_proxy_server_nameserver: model.dns_proxy_server_nameserver.to_string(),
        dns_nameserver_policy: model.dns_nameserver_policy.to_string(),
        dns_use_hosts: model.dns_use_hosts,
        hosts: model.hosts.to_string(),
        sniffer_enable: model.sniffer_enable,
        sniffer_force_dns_mapping: model.sniffer_force_dns_mapping,
        sniffer_parse_pure_ip: model.sniffer_parse_pure_ip,
        sniffer_override_destination: model.sniffer_override_destination,
        sniffer_sniff: model.sniffer_sniff.to_string(),
        sniffer_force_domain: model.sniffer_force_domain.to_string(),
        sniffer_skip_domain: model.sniffer_skip_domain.to_string(),
        geodata_mode: model.geodata_mode.to_string(),
        geox_geoip: model.geox_geoip.to_string(),
        geox_geosite: model.geox_geosite.to_string(),
        geox_mmdb: model.geox_mmdb.to_string(),
        geox_asn: model.geox_asn.to_string(),
        geodata_auto_update: model.geodata_auto_update,
        geodata_update_interval_hours: model.geodata_update_interval_hours,
        delay_test_url: model.delay_test_url.to_string(),
        delay_test_timeout_ms: model.delay_test_timeout_ms,
        log_retention_lines: model.log_retention_lines,
        engine_channel: model.engine_channel.to_string(),
    }
}

/// The settings form, from the saved preferences. A value that is not set is
/// [`NOT_SET`]; the form shows it as an empty field with the default as its
/// placeholder.
pub fn settings_model(prefs: &UserPreferences) -> SettingsModel {
    SettingsModel {
        mixed_port: prefs.mixed_port.map_or(NOT_SET, i32::from),
        socks_port: prefs.socks_port.map_or(NOT_SET, i32::from),
        http_port: prefs.port.map_or(NOT_SET, i32::from),
        allow_lan: prefs.allow_lan.unwrap_or(false),
        bind_address: prefs.bind_address.clone().unwrap_or_default().into(),
        log_level: prefs.log_level.unwrap_or_default().as_str().into(),
        mode: prefs.mode.unwrap_or_default().as_str().into(),
        tun_enabled: prefs.tun.enabled,
        tun_device: prefs.tun.device.clone().unwrap_or_default().into(),
        tun_mtu: prefs.tun.mtu.map_or(NOT_SET, i32::from),
        tun_auto_route: prefs.tun.auto_route.unwrap_or(true),
        tun_dns_hijack: dns_hijack_text(prefs.tun.dns_hijack.as_deref()).into(),
        auto_refresh_minutes: prefs.auto_refresh_minutes.map_or(NOT_SET, saturating_i32),
        auto_close_connections_on_switch: prefs.auto_close_connections_on_switch,
        system_proxy: prefs.system_proxy,
        sysproxy_mode: prefs.sysproxy_mode.as_str().into(),
        sysproxy_bypass: prefs.sysproxy_bypass.clone().into(),
        pac_script: prefs.pac_script.clone().into(),
        autostart: prefs.autostart,
        silent_start: prefs.silent_start,
        auto_connect: prefs.auto_connect_on_start,
        close_to_tray: prefs.close_to_tray,
        language: prefs.language.as_str().into(),
        accent_color: prefs.accent_color.as_str().into(),
    }
}

/// The DNS hijack targets as the form shows them: one per line.
fn dns_hijack_text(targets: Option<&[String]>) -> String {
    targets
        .map(|targets| targets.join("\n"))
        .unwrap_or_default()
}

/// The DNS hijack targets from the form. Blank lines are dropped, and an
/// empty form is "not set", which the engine reads as its default.
fn dns_hijack_lines(text: &str) -> Option<Vec<String>> {
    let targets: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    (!targets.is_empty()).then_some(targets)
}

/// The settings form as plain Rust values, so it can cross into a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsInput {
    /// [`NOT_SET`] uses the default; `1` to `65535` is a port.
    pub mixed_port: i32,
    /// [`NOT_SET`] uses the default; `1` to `65535` is a port.
    pub socks_port: i32,
    /// [`NOT_SET`] turns HTTP off; `1` to `65535` is a port.
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
    /// [`NOT_SET`] means the engine default; otherwise `1280` to `9000`.
    pub tun_mtu: i32,
    /// Whether the engine installs the TUN routes on startup.
    pub tun_auto_route: bool,
    /// The DNS hijack targets, one `host:port` per line. Empty is not set.
    pub tun_dns_hijack: String,
    /// [`NOT_SET`] means the app default; `0` is manual refresh only.
    pub auto_refresh_minutes: i32,
    /// Whether choosing a group's member closes the connections through it.
    pub auto_close_connections_on_switch: bool,
    /// Whether the operating system's proxy points at the engine.
    pub system_proxy: bool,
    /// `manual` or `pac`.
    pub sysproxy_mode: String,
    /// The bypass list, in the operating system's syntax.
    pub sysproxy_bypass: String,
    /// The PAC script, with `%mixed-port%` for the engine's port.
    pub pac_script: String,
    /// Whether Starpod starts at login.
    pub autostart: bool,
    /// Whether a login start stays in the tray.
    pub silent_start: bool,
    /// Whether Starpod connects once its state has loaded.
    pub auto_connect: bool,
    /// Whether closing the window hides Starpod to the tray.
    pub close_to_tray: bool,
    /// The saved language: `""` follows the system, `"en"` or `"ru"` pins it.
    pub language: String,
    /// The accent colour typed or picked: `#rrggbb`, or `""` for the default.
    pub accent_color: String,
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
            tun_auto_route: model.tun_auto_route,
            tun_dns_hijack: model.tun_dns_hijack.to_string(),
            auto_refresh_minutes: model.auto_refresh_minutes,
            auto_close_connections_on_switch: model.auto_close_connections_on_switch,
            system_proxy: model.system_proxy,
            sysproxy_mode: model.sysproxy_mode.to_string(),
            sysproxy_bypass: model.sysproxy_bypass.to_string(),
            pac_script: model.pac_script.to_string(),
            autostart: model.autostart,
            silent_start: model.silent_start,
            auto_connect: model.auto_connect,
            close_to_tray: model.close_to_tray,
            language: model.language.to_string(),
            accent_color: model.accent_color.to_string(),
        }
    }
}

/// The saved accent colour for a form value: `#rrggbb` in lower case, or
/// `""` for the default. The `#` is optional and `#rgb` is widened. The
/// default colour itself is saved as `""`, so a later change of the default
/// reaches users who never picked one.
pub fn normalize_accent_color(text: &str) -> Result<String, AppError> {
    let hex = text.trim().trim_start_matches('#').to_ascii_lowercase();
    if hex.is_empty() {
        return Ok(String::new());
    }
    let hex = match hex.len() {
        3 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 => hex,
        _ => {
            return Err(invalid(crate::i18n::t(
                "The accent colour must be written as #rrggbb, for example #de6161",
            )));
        }
    };
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid(crate::i18n::t(
            "The accent colour must be written as #rrggbb, for example #de6161",
        )));
    }
    let color = format!("#{hex}");
    Ok(if color == starpod_core::domain::DEFAULT_ACCENT_COLOR {
        String::new()
    } else {
        color
    })
}

/// The accent colour as a Slint colour. Anything unreadable is the default.
pub fn accent_slint_color(text: &str) -> slint::Color {
    let hex = text.trim_start_matches('#');
    let parsed = (hex.len() == 6)
        .then(|| u32::from_str_radix(hex, 16).ok())
        .flatten()
        .unwrap_or(0x00de_6161);
    slint::Color::from_rgb_u8((parsed >> 16) as u8, (parsed >> 8) as u8, parsed as u8)
}

/// The saved language for a form value: `"en"` or `"ru"`, or `""` for the
/// system language. Anything else is the system language.
pub fn normalize_language(text: &str) -> String {
    match text.trim() {
        "en" => "en",
        "ru" => "ru",
        _ => "",
    }
    .to_string()
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::Invalid(message.into())
}

/// A port from the form. [`NOT_SET`] is `None`; `0` is refused, because the
/// engine and the port checks treat `0` as a real port, not as "off".
fn port_field(name: &str, value: i32) -> Result<Option<u16>, AppError> {
    if value == NOT_SET {
        return Ok(None);
    }
    u16::try_from(value)
        .ok()
        .filter(|port| *port >= 1)
        .map(Some)
        .ok_or_else(|| invalid(format!("the {name} must be between 1 and 65535, or empty")))
}

/// A TUN MTU from the form. Only a value in range is accepted; [`NOT_SET`]
/// means the engine default.
fn mtu_field(value: i32) -> Result<Option<u16>, AppError> {
    if value == NOT_SET {
        return Ok(None);
    }
    u16::try_from(value)
        .ok()
        .filter(|mtu| (MIN_TUN_MTU..=MAX_TUN_MTU).contains(mtu))
        .map(Some)
        .ok_or_else(|| {
            invalid(format!(
                "the TUN MTU must be from {MIN_TUN_MTU} to {MAX_TUN_MTU}, or empty for the engine default"
            ))
        })
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
        |value| input.mixed_port == value.map_or(NOT_SET, i32::from),
        || port_field("mixed port", input.mixed_port),
    )?;
    let socks_port = keep_unless_edited(
        current.socks_port,
        |value| input.socks_port == value.map_or(NOT_SET, i32::from),
        || port_field("SOCKS port", input.socks_port),
    )?;
    let port = keep_unless_edited(
        current.port,
        |value| input.http_port == value.map_or(NOT_SET, i32::from),
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
        |value| input.tun_mtu == value.map_or(NOT_SET, i32::from),
        || mtu_field(input.tun_mtu),
    )?;
    let device = keep_unless_edited(
        current.tun.device.clone(),
        |value| input.tun_device.trim() == value.as_deref().unwrap_or(""),
        || Ok(non_empty(input.tun_device.trim())),
    )?;
    let auto_route = keep_unless_edited(
        current.tun.auto_route,
        |value| input.tun_auto_route == value.unwrap_or(true),
        || Ok(Some(input.tun_auto_route)),
    )?;
    let dns_hijack = keep_unless_edited(
        current.tun.dns_hijack.clone(),
        |value| dns_hijack_lines(&input.tun_dns_hijack) == *value,
        || Ok(dns_hijack_lines(&input.tun_dns_hijack)),
    )?;
    let auto_refresh_minutes = keep_unless_edited(
        current.auto_refresh_minutes,
        |value| input.auto_refresh_minutes == value.map_or(NOT_SET, saturating_i32),
        || {
            if input.auto_refresh_minutes == NOT_SET {
                return Ok(None);
            }
            u32::try_from(input.auto_refresh_minutes)
                .map(Some)
                .map_err(|_| invalid("the automatic refresh interval cannot be negative"))
        },
    )?;
    let (sysproxy_mode, pac_script) = sysproxy_fields(input)?;
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
            auto_route,
            dns_hijack,
        },
        auto_refresh_minutes,
        auto_close_connections_on_switch: input.auto_close_connections_on_switch,
        system_proxy: input.system_proxy,
        sysproxy_mode,
        sysproxy_bypass: input.sysproxy_bypass.trim().to_string(),
        pac_script,
        autostart: input.autostart,
        silent_start: input.silent_start,
        auto_connect_on_start: input.auto_connect,
        close_to_tray: input.close_to_tray,
        language: normalize_language(&input.language),
        accent_color: normalize_accent_color(&input.accent_color)?,
        ..current.clone()
    })
}

/// The PAC mode and script from the form. A script that does not define
/// `FindProxyForURL` would make the system refuse every request, so it is
/// refused here instead.
fn sysproxy_fields(input: &SettingsInput) -> Result<(SystemProxyMode, String), AppError> {
    let mode = SystemProxyMode::from_str_exact(&input.sysproxy_mode).ok_or_else(|| {
        invalid(format!(
            "“{}” is not a system proxy mode",
            input.sysproxy_mode
        ))
    })?;
    if !input.pac_script.contains("FindProxyForURL") {
        return Err(invalid(
            "the PAC script must define FindProxyForURL, or it cannot be used",
        ));
    }
    Ok((mode, input.pac_script.clone()))
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
///
/// A line the grammar accepts but the app cannot supply the target of
/// (`SUB-RULE`) is rejected here with the reason, so the editor shows it at
/// once rather than after a save. A `RULE-SET` is accepted: whether its
/// provider exists is checked when the configuration is composed.
pub fn parse_rule_line(line: &str) -> Result<RuleSpec, AppError> {
    let spec = parse_line(line).map_err(AppError::Invalid)?;
    match unsupported_rule(&spec) {
        Some(reason) => Err(invalid(reason)),
        None => Ok(spec),
    }
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
    log_item(time, detect_level(message), "app", message)
}

/// One Logs row. The message and the source carry their runs.
pub fn log_item(time: &str, level: &str, source: &str, message: &str) -> LogItem {
    LogItem {
        time: time.into(),
        level: level.into(),
        source: source.into(),
        source_runs: runs_model(source),
        message: message.into(),
        message_runs: runs_model(message),
    }
}

/// An engine line. The engine's text carries no timestamp of its own here.
fn engine_item(line: &str) -> LogItem {
    log_item("", detect_level(line), "engine", line)
}

/// Whether a line of level `level` passes the Logs filter. `all` and the
/// empty filter pass everything.
pub fn level_passes(filter: &str, level: &str) -> bool {
    filter.is_empty() || filter == "all" || canonical_level(filter) == canonical_level(level)
}

/// One spelling for each level: `warn` is `warning`, the spelling the Logs
/// screen and [`detect_level`] use.
fn canonical_level(level: &str) -> &str {
    match level {
        "warn" => "warning",
        other => other,
    }
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
    /// The Connections page's filter, as the UI last reported it.
    pub connections_filter: String,
    /// The Connections page's sort (see [`connections::SORT_NEWEST`]).
    pub connections_sort: i32,
    /// The Proxies page's member sort (`0`, `1` or `2`), as the UI last reported it.
    pub proxies_sort: i32,
    /// Whether the Proxies page hides timed-out members, as the UI last reported it.
    pub hide_unavailable: bool,
    /// The groups the Proxies page has open. Every other group is collapsed.
    pub expanded_groups: BTreeSet<String>,
    /// Whether the list settings were taken from the saved preferences. Until
    /// they are, the page's own values are not saved over the saved ones.
    pub list_prefs_loaded: bool,
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

    /// Store the Connections and Proxies list controls the UI shows. Returns
    /// whether any changed.
    pub fn set_list_controls(
        &mut self,
        connections_filter: &str,
        connections_sort: i32,
        proxies_sort: i32,
        hide_unavailable: bool,
    ) -> bool {
        let changed = self.connections_filter != connections_filter
            || self.connections_sort != connections_sort
            || self.proxies_sort != proxies_sort
            || self.hide_unavailable != hide_unavailable;
        if changed {
            self.connections_filter = connections_filter.to_string();
            self.connections_sort = connections_sort;
            self.proxies_sort = proxies_sort;
            self.hide_unavailable = hide_unavailable;
        }
        changed
    }

    /// Take the Proxies list settings from the saved preferences, once.
    /// Returns whether they were taken now.
    pub fn load_list_prefs(&mut self, prefs: &UserPreferences) -> bool {
        if self.list_prefs_loaded {
            return false;
        }
        self.list_prefs_loaded = true;
        self.proxies_sort = i32::from(prefs.proxies_sort);
        self.hide_unavailable = prefs.hide_unavailable;
        self.expanded_groups = prefs.expanded_groups.clone();
        true
    }

    /// Flip one group between collapsed and expanded.
    pub fn toggle_group(&mut self, group: &str) {
        if !self.expanded_groups.remove(group) {
            self.expanded_groups.insert(group.to_string());
        }
    }

    /// The order the Proxies page shows each group's members in.
    pub fn member_order(&self) -> MemberOrder {
        MemberOrder {
            sort: self.proxies_sort,
            hide_unavailable: self.hide_unavailable,
        }
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

/// Whether the model already holds exactly `next`, row by row under `same`.
fn matches_model<T: Clone + 'static>(
    model: &ModelRc<T>,
    next: &[T],
    same: fn(&T, &T) -> bool,
) -> bool {
    model.row_count() == next.len()
        && next
            .iter()
            .enumerate()
            .all(|(row, item)| model.row_data(row).is_some_and(|old| same(&old, item)))
}

/// A new model holding `next`, or `None` when `current` already holds it.
fn changed_model<T: Clone + 'static>(
    current: ModelRc<T>,
    next: Vec<T>,
    same: fn(&T, &T) -> bool,
) -> Option<ModelRc<T>> {
    if matches_model(&current, &next, same) {
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
    set_connections(bridge, snap, view);
    set_settings(bridge, snap, view);
    set_shell(bridge, snap);
    set_profile(bridge, snap);
}

/// The Home page profile card, the Global-mode gate, the device-header switch
/// and the live session traffic.
fn set_profile(bridge: &Bridge<'_>, snap: &AppSnapshot) {
    let previous = bridge.get_profile_card();
    let card = profile_card_item(snap.profile_card.as_ref(), &previous);
    if !same_profile_card(&previous, &card) {
        bridge.set_profile_card(card);
    }
    bridge.set_global_mode_allowed(snap.global_mode_allowed);
    bridge.set_send_device_id(snap.preferences.send_device_id);
    // The engine's totals since it started. They come from the connections
    // poll, so they are shown only while the engine runs.
    let running = snap.engine.phase.is_running();
    bridge.set_session_traffic_visible(running);
    let (upload, download) = if running {
        (
            format_bytes(snap.connections.upload_total),
            format_bytes(snap.connections.download_total),
        )
    } else {
        (String::new(), String::new())
    };
    bridge.set_session_upload(upload.into());
    bridge.set_session_download(download.into());
}

/// The profile card as the Bridge shows it. The logo is loaded from its file
/// only when its path changed, so a snapshot does not reread the image.
pub fn profile_card_item(
    card: Option<&ProfileCard>,
    previous: &ProfileCardItem,
) -> ProfileCardItem {
    let Some(card) = card else {
        return ProfileCardItem::default();
    };
    let logo_path = card
        .logo_path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let (logo, has_logo) = if logo_path.is_empty() {
        (Image::default(), false)
    } else if previous.has_logo && previous.logo_path.as_str() == logo_path {
        (previous.logo.clone(), true)
    } else {
        match Image::load_from_path(std::path::Path::new(&logo_path)) {
            Ok(image) => (image, true),
            Err(_) => (Image::default(), false),
        }
    };

    let used = card.traffic_used_bytes;
    let total = card.traffic_total_bytes;
    let (traffic_text, remaining_text, quota_used_fraction) = if !card.has_quota {
        (String::new(), String::new(), 0.0)
    } else if total > 0 {
        (
            crate::i18n::fmt("{} of {}", &[&format_bytes(used), &format_bytes(total)]),
            crate::i18n::fmt("{} left", &[&format_bytes(total.saturating_sub(used))]),
            (used as f64 / total as f64).clamp(0.0, 1.0) as f32,
        )
    } else {
        (
            crate::i18n::fmt("{} used", &[&format_bytes(used)]),
            crate::i18n::t("Unlimited").to_string(),
            0.0,
        )
    };
    let expires_text = card.expires.as_deref().map_or_else(String::new, |text| {
        let date = text.get(..10).unwrap_or(text);
        if card.expired {
            crate::i18n::fmt("Expired {}", &[date])
        } else {
            crate::i18n::fmt("Expires {}", &[date])
        }
    });
    let announce = card.announce.clone().unwrap_or_default();

    ProfileCardItem {
        present: true,
        subscription_id: card.subscription_id.as_str().into(),
        title: card.title.as_str().into(),
        title_runs: runs_model(&card.title),
        has_logo,
        logo,
        logo_path: logo_path.into(),
        announce: announce.as_str().into(),
        announce_runs: runs_model(&announce),
        announce_url: card.announce_url.clone().unwrap_or_default().into(),
        support_url: card.support_url.clone().unwrap_or_default().into(),
        web_url: card.web_url.clone().unwrap_or_default().into(),
        web_page_name: card.web_page_name.clone().unwrap_or_default().into(),
        traffic_text: traffic_text.into(),
        remaining_text: remaining_text.into(),
        expires_text: expires_text.into(),
        days_remaining: i32::try_from(card.days_remaining).unwrap_or(-1),
        expiring_soon: card.expiring_soon,
        device_limit: card.device_limit,
        has_quota: card.has_quota,
        quota_used_fraction,
    }
}

/// Whether two cards show the same thing. The logo image itself is not
/// compared: its path and `has_logo` stand for it.
fn same_profile_card(a: &ProfileCardItem, b: &ProfileCardItem) -> bool {
    a.present == b.present
        && a.subscription_id == b.subscription_id
        && a.title == b.title
        && a.has_logo == b.has_logo
        && a.logo_path == b.logo_path
        && a.announce == b.announce
        && a.announce_url == b.announce_url
        && a.support_url == b.support_url
        && a.web_url == b.web_url
        && a.web_page_name == b.web_page_name
        && a.traffic_text == b.traffic_text
        && a.remaining_text == b.remaining_text
        && a.expires_text == b.expires_text
        && a.days_remaining == b.days_remaining
        && a.expiring_soon == b.expiring_soon
        && a.device_limit == b.device_limit
        && a.has_quota == b.has_quota
        && a.quota_used_fraction == b.quota_used_fraction
}

/// The text the Bridge shows when Connect is blocked. Empty when it is allowed.
pub fn connect_blocked_text(snap: &AppSnapshot) -> String {
    snap.connect_blocked.clone().unwrap_or_default()
}

fn set_engine(bridge: &Bridge<'_>, snap: &AppSnapshot) {
    bridge.set_loaded(true);
    bridge.set_engine_phase(phase_str(snap.engine.phase).into());
    let engine_error = snap.engine.error.clone().unwrap_or_default();
    bridge.set_engine_error_runs(runs_model(&engine_error));
    bridge.set_engine_error(engine_error.into());
    bridge.set_engine_installed(snap.engine.installed);
    let update = &snap.engine_update;
    bridge
        .set_engine_installed_version(update.installed_version.clone().unwrap_or_default().into());
    bridge.set_engine_latest_version(update.latest_version.clone().unwrap_or_default().into());
    bridge.set_engine_update_available(update.update_available);
    // Unix seconds as the Bridge's `int`; 0 means never.
    let geo = snap.geo_updated_at.and_then(|at| i32::try_from(at).ok());
    bridge.set_geo_updated_at(geo.unwrap_or(0));
    let blocked = connect_blocked_text(snap);
    bridge.set_connect_blocked_reason_runs(runs_model(&blocked));
    bridge.set_connect_blocked_reason(blocked.into());
    bridge.set_engine_version(snap.engine.version.clone().unwrap_or_default().into());
    bridge.set_mode(snap.mode.as_str().into());
    let current = snap
        .current_node
        .as_ref()
        .map(|node| node.name.clone())
        .unwrap_or_default();
    bridge.set_current_node_runs(runs_model(&current));
    bridge.set_current_node(current.into());
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
    if let Some(model) = changed_model(bridge.get_subscriptions(), subscriptions, same_subscription)
    {
        bridge.set_subscriptions(model);
    }
    let nodes: Vec<NodeItem> = snap
        .nodes
        .iter()
        .filter(|node| node_matches(node, &view.node_filter))
        .map(node_item)
        .collect();
    if let Some(model) = changed_model(bridge.get_nodes(), nodes, same_node) {
        bridge.set_nodes(model);
    }
    let order = view.member_order();
    let groups: Vec<GroupItem> = snap
        .groups
        .iter()
        .map(|group| group_item_in(group, order, view.expanded_groups.contains(&group.name)))
        .collect();
    if groups_changed(&bridge.get_groups(), &groups) {
        bridge.set_groups(ModelRc::new(VecModel::from(groups)));
    }
    let primary = snap.primary_group.clone().unwrap_or_default();
    bridge.set_primary_group_runs(runs_model(&primary));
    bridge.set_primary_group(primary.into());
    let group_rows = group_choices(&snap.groups);
    if let Some(model) = changed_model(bridge.get_group_choices(), group_rows, same_select) {
        bridge.set_group_choices(model);
    }
    let announcements: Vec<AnnouncementItem> =
        snap.announcements.iter().map(announcement_item).collect();
    if let Some(model) = changed_model(bridge.get_announcements(), announcements, same_announcement)
    {
        bridge.set_announcements(model);
    }
}

fn set_rules(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    let user: Vec<RuleItem> = snap.rules.iter().map(rule_item).collect();
    if let Some(model) = changed_model(bridge.get_user_rules(), user, same_rule) {
        bridge.set_user_rules(model);
    }
    let subscription: Vec<RuleItem> = snap.subscription_rules.iter().map(rule_item).collect();
    if let Some(model) = changed_model(bridge.get_subscription_rules(), subscription, same_rule) {
        bridge.set_subscription_rules(model);
    }
    let kinds: Vec<SharedString> = snap
        .rule_kinds
        .iter()
        .map(|kind| SharedString::from(kind.token.as_str()))
        .collect();
    if let Some(model) = changed_model(bridge.get_rule_kinds(), kinds, |a, b| a == b) {
        bridge.set_rule_kinds(model);
    }
    let targets: Vec<SharedString> = snap
        .rule_targets
        .iter()
        .map(|target| SharedString::from(target.as_str()))
        .collect();
    if let Some(model) = changed_model(bridge.get_rule_targets(), targets, |a, b| a == b) {
        bridge.set_rule_targets(model);
    }
    let raw = rules_raw_text(&snap.rules);
    if view.applied_rules.as_deref() != Some(raw.as_str()) {
        bridge.set_rules_raw_text(raw.into());
    }
}

fn set_logs(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    if let Some(model) = changed_model(bridge.get_logs(), log_items(snap, view), same_log) {
        bridge.set_logs(model);
    }
}

/// The Connections page: the open connections and the closed history, each
/// filtered and sorted by the page's controls, and the engine's totals.
fn set_connections(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    let active = connection_rows(
        &snap.connections.active,
        &view.connections_filter,
        view.connections_sort,
    );
    if let Some(model) = changed_model(bridge.get_connections(), active, same_connection) {
        bridge.set_connections(model);
    }
    let closed = connection_rows(
        &snap.connections.closed,
        &view.connections_filter,
        view.connections_sort,
    );
    if let Some(model) = changed_model(bridge.get_closed_connections(), closed, same_connection) {
        bridge.set_closed_connections(model);
    }
    bridge.set_connections_active_count(saturating_i32(snap.connections.active.len()));
    bridge.set_connections_total_upload(format_bytes(snap.connections.upload_total).into());
    bridge.set_connections_total_download(format_bytes(snap.connections.download_total).into());
}

fn set_settings(bridge: &Bridge<'_>, snap: &AppSnapshot, view: &ViewState) {
    if view.applied_prefs.as_ref() != Some(&snap.preferences) {
        bridge.set_settings(settings_model(&snap.preferences));
        let engine = EngineForm::from_prefs(&snap.preferences);
        bridge.set_engine_settings(engine_settings_model(&engine));
    }
}

fn set_shell(bridge: &Bridge<'_>, snap: &AppSnapshot) {
    bridge.set_secret_backend(snap.secret_backend.as_str().into());
    bridge.set_secrets_encrypted(snap.secret_encrypted);
    bridge.set_secret_fallback_reason(
        snap.secret_fallback_reason
            .clone()
            .unwrap_or_default()
            .into(),
    );
    bridge.set_data_dir(snap.data_dir.display().to_string().into());
    bridge.set_system_proxy_active(snap.system_proxy_active);
    bridge.set_system_proxy_error(snap.system_proxy_error.clone().unwrap_or_default().into());
    let warnings: Vec<SharedString> = snap
        .warnings
        .iter()
        .map(|warning| SharedString::from(warning.as_str()))
        .collect();
    if let Some(model) = changed_model(bridge.get_warnings(), warnings, |a, b| a == b) {
        bridge.set_warnings(model);
    }
    let warning_items: Vec<WarningItem> = snap
        .warnings
        .iter()
        .map(|warning| WarningItem {
            text: warning.as_str().into(),
            runs: runs_model(warning),
        })
        .collect();
    if let Some(model) = changed_model(bridge.get_warning_items(), warning_items, same_warning) {
        bridge.set_warning_items(model);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_colours_are_normalized_and_the_default_saves_as_empty() {
        assert_eq!(normalize_accent_color("").unwrap(), "");
        assert_eq!(normalize_accent_color("#DE6161").unwrap(), "");
        assert_eq!(normalize_accent_color("5b9be0").unwrap(), "#5b9be0");
        assert_eq!(normalize_accent_color(" #abc ").unwrap(), "#aabbcc");
        assert!(normalize_accent_color("#12345").is_err());
        assert!(normalize_accent_color("#zzzzzz").is_err());
        assert_eq!(
            accent_slint_color("#5b9be0"),
            slint::Color::from_rgb_u8(0x5b, 0x9b, 0xe0)
        );
        assert_eq!(
            accent_slint_color("nonsense"),
            slint::Color::from_rgb_u8(0xde, 0x61, 0x61)
        );
    }
    use crate::EngineView;

    #[test]
    fn the_engine_form_survives_a_trip_through_the_bridge_model() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let form = EngineForm::from_prefs(&UserPreferences::default());
        let model = engine_settings_model(&form);
        assert_eq!(engine_form_from_model(&model), form);
    }

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
            active: true,
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
            title: "Main".to_string(),
            announce: None,
            support_url: None,
            web_url: None,
            web_page_name: None,
            announce_url: None,
            logo_path: None,
            interval_secs: 3600,
            global_mode_disabled: false,
            expand_proxy_groups: None,
            device_limit: false,
        }
    }

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn a_blocked_connect_reaches_the_bridge_as_its_reason_and_an_allowed_one_as_empty() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let blocked = AppSnapshot {
            connect_blocked: Some(crate::snapshot::NO_SUBSCRIPTION_REASON.to_string()),
            ..AppSnapshot::default()
        };
        assert_eq!(connect_blocked_text(&blocked), "Add a subscription first");
        assert_eq!(connect_blocked_text(&AppSnapshot::default()), "");
    }

    #[test]
    fn uptime_reads_as_hours_minutes_and_seconds() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(format_uptime(3720), "1h 02m");
        assert_eq!(format_uptime(249), "4m 09s");
        assert_eq!(format_uptime(42), "42s");
    }

    #[test]
    fn bytes_use_decimal_units_without_a_trailing_point_zero() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(12_300_000_000), "12.3 GB");
        assert_eq!(format_bytes(100_000_000_000), "100 GB");
        assert_eq!(format_bytes(1_500), "1.5 KB");
    }

    #[test]
    fn quota_shows_a_fraction_and_text_only_for_a_known_total() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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

        let unlimited = SubscriptionQuota {
            used: 5_000_000_000,
            total: Some(0),
            expires: Some("1970-01-01T00:00:00Z".to_string()),
        };
        assert_eq!(
            quota_text(&unlimited),
            "5 GB used",
            "a total of 0 is no limit"
        );
        let item = subscription_item(&SubscriptionView {
            quota: Some(unlimited),
            ..subscription(None)
        });
        assert_eq!(item.expires, "", "expire=0 is no end date");
    }

    #[test]
    fn a_subscription_row_carries_its_quota_and_error() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let item = subscription_item(&subscription(None));
        assert!(!item.has_quota);
        assert_eq!(item.quota_text, "");
        assert_eq!(item.expires, "");
        assert_eq!(item.quota_used_fraction, 0.0);
    }

    #[test]
    fn engine_phases_and_statuses_use_the_contract_words() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(phase_str(EnginePhase::Stopped), "stopped");
        assert_eq!(phase_str(EnginePhase::Failed), "failed");
        assert_eq!(status_str(SubscriptionStatus::NeverFetched), "never");
        assert_eq!(status_str(SubscriptionStatus::EngineManaged), "ok");
    }

    #[test]
    fn node_filter_is_a_case_insensitive_substring_match() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let mut fast = node("a", None, "ss");
        assert_eq!(node_item(&fast).latency_ms, -1);
        assert_eq!(node_item(&fast).group, "");
        fast.latency_ms = Some(87);
        assert_eq!(node_item(&fast).latency_ms, 87);
    }

    #[test]
    fn a_member_latency_is_minus_one_untested_and_zero_timed_out() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(latency_ms(Latency::Untested), -1);
        assert_eq!(latency_ms(Latency::TimedOut), 0);
        assert_eq!(latency_ms(Latency::Measured(131)), 131);
    }

    #[test]
    fn a_group_carries_its_selection_and_members_in_order() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let view = GroupView {
            name: "Airport / Proxy".to_string(),
            label: "Proxy".to_string(),
            kind: "select".to_string(),
            selected: None,
            members: vec![
                MemberView {
                    name: "Airport / Node A".to_string(),
                    label: "Node A".to_string(),
                    protocol: "trojan".to_string(),
                    latency: Latency::TimedOut,
                },
                MemberView {
                    name: "DIRECT".to_string(),
                    label: "DIRECT".to_string(),
                    protocol: "direct".to_string(),
                    latency: Latency::Untested,
                },
            ],
        };
        let item = group_item(&view);
        assert_eq!(item.name, "Airport / Proxy", "the engine name is kept");
        assert_eq!(item.label, "Proxy", "the shown name drops the prefix");
        assert_eq!(item.selected, "", "an unknown selection is empty");
        let members: Vec<MemberItem> = item.members.iter().collect();
        assert_eq!(members[0].name, "Airport / Node A");
        assert_eq!(members[0].label, "Node A");
        assert_eq!(members[0].latency_ms, 0);
        assert_eq!(members[1].latency_ms, -1);
    }

    #[test]
    fn a_group_row_is_unchanged_only_when_every_field_is() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let view = GroupView {
            name: "Proxy".to_string(),
            label: "Proxy".to_string(),
            kind: "select".to_string(),
            selected: Some("A".to_string()),
            members: vec![MemberView {
                name: "A".to_string(),
                label: "A".to_string(),
                protocol: "ss".to_string(),
                latency: Latency::Measured(10),
            }],
        };
        let model = ModelRc::new(VecModel::from(vec![group_item(&view)]));
        assert!(!groups_changed(&model, &[group_item(&view)]));

        let mut moved = view.clone();
        moved.members[0].latency = Latency::Measured(11);
        assert!(groups_changed(&model, &[group_item(&moved)]));
    }

    #[test]
    fn a_subscription_row_carries_its_title() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let mut view = subscription(None);
        view.title = "Foxgirls VPN".to_string();
        assert_eq!(subscription_item(&view).title, "Foxgirls VPN");
    }

    #[test]
    fn rule_raw_text_joins_the_lines_in_order() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
    fn unset_values_show_as_not_set_and_chosen_values_as_numbers() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let prefs = UserPreferences {
            mixed_port: None,
            socks_port: Some(7891),
            port: None,
            auto_refresh_minutes: Some(0),
            ..UserPreferences::default()
        };
        let model = settings_model(&prefs);
        assert_eq!(model.mixed_port, NOT_SET);
        assert_eq!(model.socks_port, 7891);
        assert_eq!(model.http_port, NOT_SET);
        assert_eq!(model.tun_mtu, NOT_SET);
        assert_eq!(model.auto_refresh_minutes, 0);
        assert_eq!(model.bind_address, "");
        assert_eq!(model.log_level, "info");

        let unset = settings_model(&UserPreferences::default());
        assert_eq!(unset.auto_refresh_minutes, NOT_SET);
    }

    #[test]
    fn default_preferences_round_trip_to_identity() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let current = UserPreferences::default();
        let saved = build_preferences(&current, &input_from(&current)).unwrap();
        assert_eq!(saved, current);
    }

    #[test]
    fn populated_preferences_round_trip_to_identity() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let current = UserPreferences {
            mixed_port: Some(7890),
            socks_port: Some(1080),
            port: Some(8080),
            allow_lan: Some(true),
            bind_address: Some("0.0.0.0".to_string()),
            mode: Some(ProxyMode::Global),
            log_level: Some(LogLevel::Debug),
            tun: TunSettings {
                enabled: true,
                device: Some("starpod-tun".to_string()),
                mtu: Some(1500),
                ..TunSettings::default()
            },
            auto_refresh_minutes: Some(0),
            ..UserPreferences::default()
        };
        let saved = build_preferences(&current, &input_from(&current)).unwrap();
        assert_eq!(saved, current);
    }

    #[test]
    fn cleared_fields_save_as_not_set() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let current = UserPreferences {
            mixed_port: Some(7890),
            socks_port: Some(1080),
            port: Some(8080),
            bind_address: Some("127.0.0.1".to_string()),
            tun: TunSettings {
                mtu: Some(1500),
                device: Some("starpod-tun".to_string()),
                ..TunSettings::default()
            },
            auto_refresh_minutes: Some(30),
            ..UserPreferences::default()
        };
        let mut input = input_from(&current);
        input.mixed_port = NOT_SET;
        input.socks_port = NOT_SET;
        input.http_port = NOT_SET;
        input.bind_address = String::new();
        input.tun_device = String::new();
        input.tun_mtu = NOT_SET;
        input.auto_refresh_minutes = NOT_SET;
        let saved = build_preferences(&current, &input).unwrap();
        assert_eq!(saved.mixed_port, None);
        assert_eq!(saved.socks_port, None);
        assert_eq!(saved.port, None);
        assert_eq!(saved.bind_address, None);
        assert_eq!(saved.tun.device, None);
        assert_eq!(saved.tun.mtu, None);
        assert_eq!(saved.auto_refresh_minutes, None);
    }

    #[test]
    fn an_explicit_refresh_of_sixty_is_saved_not_dropped() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let current = UserPreferences::default();
        let mut input = input_from(&current);
        input.auto_refresh_minutes = 60;
        let saved = build_preferences(&current, &input).unwrap();
        assert_eq!(saved.auto_refresh_minutes, Some(60));
    }

    #[test]
    fn the_tun_mtu_is_checked_only_when_a_value_is_entered() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let current = UserPreferences::default();
        let mut input = input_from(&current);
        input.tun_mtu = NOT_SET;
        assert_eq!(build_preferences(&current, &input).unwrap().tun.mtu, None);

        for (mtu, ok) in [
            (1279, false),
            (1280, true),
            (9000, true),
            (9001, false),
            (0, false),
        ] {
            let mut input = input_from(&current);
            input.tun_mtu = mtu;
            assert_eq!(build_preferences(&current, &input).is_ok(), ok, "mtu {mtu}");
        }
    }

    fn input_from(prefs: &UserPreferences) -> SettingsInput {
        SettingsInput::from_model(&settings_model(prefs))
    }

    #[test]
    fn saving_the_form_round_trips_and_keeps_selection() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
    fn an_empty_port_is_not_set_and_an_explicit_zero_is_refused() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let current = UserPreferences {
            socks_port: Some(7891),
            ..UserPreferences::default()
        };
        let mut input = input_from(&current);
        input.socks_port = NOT_SET;
        let saved = build_preferences(&current, &input).unwrap();
        assert_eq!(saved.socks_port, None);

        // 0 is not "off": the engine would listen on it, so the form refuses it.
        let mut input = input_from(&current);
        input.socks_port = 0;
        let error = build_preferences(&current, &input).unwrap_err();
        assert!(error.to_string().contains("SOCKS port"));
    }

    #[test]
    fn out_of_range_settings_are_rejected_with_a_reason() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let current = UserPreferences::default();

        let mut input = input_from(&current);
        input.mixed_port = 70_000;
        let error = build_preferences(&current, &input).unwrap_err();
        assert!(error.to_string().contains("mixed port"));

        let mut input = input_from(&current);
        input.tun_mtu = 900;
        assert!(build_preferences(&current, &input).is_err());

        // 1279 is below the IPv6 minimum and is refused; 1280 is the floor.
        let mut input = input_from(&current);
        input.tun_mtu = 1279;
        assert!(build_preferences(&current, &input).is_err());
        input.tun_mtu = 1280;
        assert_eq!(
            build_preferences(&current, &input).unwrap().tun.mtu,
            Some(1280)
        );

        let mut input = input_from(&current);
        input.log_level = "verbose".to_string();
        assert!(build_preferences(&current, &input).is_err());

        let mut input = input_from(&current);
        input.auto_refresh_minutes = -5;
        assert!(build_preferences(&current, &input).is_err());
    }

    #[test]
    fn rule_lines_are_built_from_the_editor_fields() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(
            rule_line("DOMAIN-SUFFIX", "example.com", "DIRECT"),
            "DOMAIN-SUFFIX,example.com,DIRECT"
        );
        assert_eq!(rule_line("MATCH", "", "PROXY"), "MATCH,PROXY");
        assert_eq!(rule_line("SUB-RULE", "block", ""), "SUB-RULE,block");
    }

    #[test]
    fn a_valid_rule_line_parses_and_a_bad_one_reports_why() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert!(parse_rule_line("DOMAIN-SUFFIX,example.com,DIRECT").is_ok());
        assert!(parse_rule_line("MATCH,DIRECT").is_ok());
        assert!(matches!(
            parse_rule_line("NOT-A-KIND,x,DIRECT"),
            Err(AppError::Invalid(_))
        ));
        assert!(parse_rule_line("").is_err());
    }

    #[test]
    fn a_rule_set_is_accepted_and_a_sub_rule_block_says_the_feature_is_missing() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert!(parse_rule_line("RULE-SET,my-set,DIRECT,no-resolve").is_ok());
        let block = parse_rule_line("SUB-RULE,my-block").unwrap_err();
        assert!(block.to_string().contains("not supported yet"), "{block}");
    }

    #[test]
    fn the_rule_item_carries_the_enabled_flag() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let rule = RuleView {
            index: 2,
            id: Some("r2".to_string()),
            enabled: false,
            kind: "DOMAIN".to_string(),
            payload: "a.test".to_string(),
            target: "DIRECT".to_string(),
            line: "DOMAIN,a.test,DIRECT".to_string(),
            from_subscription: false,
        };
        let item = rule_item(&rule);
        assert!(!item.enabled);
        assert_eq!(item.index, 2);
    }

    #[test]
    fn moving_a_rule_clamps_to_the_list() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(detect_level("level=error dial failed"), "error");
        assert_eq!(detect_level("WARN slow handshake"), "warning");
        assert_eq!(detect_level("debug: tick"), "debug");
        assert_eq!(detect_level("the engine started"), "info");
        assert!(level_passes("all", "error"));
        assert!(level_passes("", "info"));
        assert!(!level_passes("error", "info"));
    }

    #[test]
    fn the_warn_and_warning_filters_both_pass_warning_lines() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert!(level_passes("warn", "warning"));
        assert!(level_passes("warning", "warning"));
        assert!(level_passes("warning", "warn"));
        assert!(!level_passes("warn", "error"));
        assert!(!level_passes("warning", "info"));
    }

    #[test]
    fn a_member_choice_shows_its_label_and_latency() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let member = |latency| MemberView {
            name: "Airport / Node A".to_string(),
            label: "Node A".to_string(),
            protocol: "trojan".to_string(),
            latency,
        };
        let untested = member_choice(&member(Latency::Untested));
        assert_eq!(untested.text, "Node A");
        assert_eq!(untested.detail, "—");
        let timed_out = member_choice(&member(Latency::TimedOut));
        assert_eq!(timed_out.detail, "timeout");
        assert_eq!(timed_out.detail_color, latency_color(0));
        let fast = member_choice(&member(Latency::Measured(120)));
        assert_eq!(fast.detail, "120 ms");
        assert_eq!(fast.detail_color, latency_color(120));
        assert_ne!(latency_color(120), latency_color(300));
        assert_ne!(latency_color(300), latency_color(500));
    }

    #[test]
    fn a_group_choice_shows_the_label_without_detail() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let view = GroupView {
            name: "Airport / Pick".to_string(),
            label: "Pick".to_string(),
            kind: "select".to_string(),
            selected: None,
            members: Vec::new(),
        };
        let rows = group_choices(&[view]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "Pick");
        assert_eq!(rows[0].detail, "");
    }

    #[test]
    fn a_line_tracker_gives_stable_indexes_as_the_window_rolls() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let items = vec![
            log_item("t", "info", "app", "hello"),
            log_item("", "error", "engine", "boom"),
        ];
        assert_eq!(
            log_text(&items),
            "t [info] app: hello\n[error] engine: boom"
        );
    }

    #[test]
    fn a_model_is_replaced_only_when_its_contents_differ() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let model: ModelRc<i32> = ModelRc::new(VecModel::from(vec![1, 2]));
        let same = |a: &i32, b: &i32| a == b;
        assert!(matches_model(&model, &[1, 2], same));
        assert!(changed_model(model.clone(), vec![1, 2], same).is_none());
        assert!(changed_model(model.clone(), vec![1, 2, 3], same).is_some());
        assert!(changed_model(model, vec![2, 1], same).is_some());
    }

    #[test]
    fn a_notice_becomes_a_banner_of_the_matching_kind() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
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

    fn listed(name: &str, latency: Latency) -> MemberView {
        MemberView {
            name: name.to_string(),
            label: name.to_string(),
            protocol: "trojan".to_string(),
            latency,
        }
    }

    fn proxy_group(members: Vec<MemberView>, selected: Option<&str>) -> GroupView {
        GroupView {
            name: "Proxy".to_string(),
            label: "Proxy".to_string(),
            kind: "select".to_string(),
            selected: selected.map(str::to_string),
            members,
        }
    }

    fn names(members: &[MemberView]) -> Vec<&str> {
        members.iter().map(|member| member.name.as_str()).collect()
    }

    #[test]
    fn members_sort_by_latency_with_untested_and_timed_out_last() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let members = vec![
            listed("slow", Latency::Measured(300)),
            listed("dead", Latency::TimedOut),
            listed("new", Latency::Untested),
            listed("fast", Latency::Measured(40)),
        ];
        let by_latency = MemberOrder {
            sort: 1,
            hide_unavailable: false,
        };
        let shown = ordered_members(&members, by_latency);
        assert_eq!(names(&shown), vec!["fast", "slow", "new", "dead"]);
    }

    #[test]
    fn members_sort_by_name_and_stay_as_configured_by_default() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let members = vec![
            listed("beta", Latency::Untested),
            listed("Alpha", Latency::Untested),
        ];
        let by_name = MemberOrder {
            sort: 2,
            hide_unavailable: false,
        };
        assert_eq!(
            names(&ordered_members(&members, by_name)),
            vec!["Alpha", "beta"]
        );
        assert_eq!(
            names(&ordered_members(&members, MemberOrder::default())),
            vec!["beta", "Alpha"]
        );
    }

    #[test]
    fn hiding_leaves_out_only_the_timed_out_members() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let members = vec![
            listed("dead", Latency::TimedOut),
            listed("new", Latency::Untested),
            listed("fast", Latency::Measured(40)),
        ];
        let hide = MemberOrder {
            sort: 0,
            hide_unavailable: true,
        };
        assert_eq!(names(&ordered_members(&members, hide)), vec!["new", "fast"]);
    }

    #[test]
    fn the_selected_index_follows_the_shown_order_and_is_minus_one_when_hidden() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let members = vec![
            listed("a", Latency::Measured(300)),
            listed("b", Latency::TimedOut),
            listed("c", Latency::Measured(40)),
        ];
        let by_latency = MemberOrder {
            sort: 1,
            hide_unavailable: false,
        };
        let on_a = group_item_in(&proxy_group(members.clone(), Some("a")), by_latency, true);
        assert_eq!(
            on_a.selected_index, 1,
            "a is second after sorting by latency"
        );

        let hidden = MemberOrder {
            sort: 1,
            hide_unavailable: true,
        };
        let on_b = group_item_in(&proxy_group(members.clone(), Some("b")), hidden, true);
        assert_eq!(
            on_b.selected_index, -1,
            "a hidden member is not in the list"
        );

        let none = group_item_in(&proxy_group(members, None), by_latency, true);
        assert_eq!(none.selected_index, -1);
    }

    #[test]
    fn the_member_choices_follow_the_members_order_and_a_group_can_be_collapsed() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let members = vec![
            listed("slow", Latency::Measured(300)),
            listed("fast", Latency::Measured(40)),
        ];
        let by_latency = MemberOrder {
            sort: 1,
            hide_unavailable: false,
        };
        let item = group_item_in(&proxy_group(members, Some("fast")), by_latency, false);
        assert!(!item.expanded);
        assert_eq!(
            item.members
                .row_data(0)
                .map(|m| m.name.to_string())
                .as_deref(),
            Some("fast")
        );
        assert_eq!(
            item.member_choices
                .row_data(0)
                .map(|choice| choice.text.to_string())
                .as_deref(),
            Some("fast"),
            "the Select rows use the same order, so the selected index is valid for both"
        );
        assert_eq!(item.selected_index, 0);
    }

    #[test]
    fn the_page_controls_load_once_and_collapse_and_expand_groups() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let mut view = ViewState::default();
        let prefs = UserPreferences {
            proxies_sort: 2,
            hide_unavailable: true,
            ..UserPreferences::default()
        };
        assert!(
            view.load_list_prefs(&prefs),
            "the saved settings load the first time"
        );
        assert!(!view.load_list_prefs(&prefs), "and not again");
        assert_eq!(
            view.member_order(),
            MemberOrder {
                sort: 2,
                hide_unavailable: true
            }
        );

        assert!(
            view.expanded_groups.is_empty(),
            "every group starts collapsed"
        );
        view.toggle_group("Proxy");
        assert!(view.expanded_groups.contains("Proxy"));
        view.toggle_group("Proxy");
        assert!(!view.expanded_groups.contains("Proxy"));

        // The groups the user left open come back from the saved preferences.
        let mut prefs = UserPreferences::default();
        prefs.expanded_groups.insert("Streaming".to_string());
        let mut fresh = ViewState::default();
        assert!(fresh.load_list_prefs(&prefs));
        assert!(fresh.expanded_groups.contains("Streaming"));
        assert!(!fresh.expanded_groups.contains("Proxy"));
    }

    #[test]
    fn a_connection_row_shows_its_formatted_figures_chain_and_rule() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let view = ConnectionView {
            id: "c1".to_string(),
            host: "example.com".to_string(),
            destination: "203.0.113.7:443".to_string(),
            chains: vec!["Node A".to_string(), "Proxy".to_string()],
            rule: "DOMAIN-SUFFIX,example.com".to_string(),
            upload: 1500,
            download: 2_000_000,
            upload_speed: 2000,
            download_speed: 0,
            started_at: Some("2026-11-14T12:03:44+03:00".to_string()),
            duration_secs: 270,
            ..ConnectionView::default()
        };
        let row = connection_item(&view);
        assert_eq!(row.chain.as_str(), "Node A → Proxy");
        assert_eq!(row.rule.as_str(), "DOMAIN-SUFFIX,example.com");
        assert_eq!(row.upload.as_str(), "1.5 KB");
        assert_eq!(row.download.as_str(), "2 MB");
        assert_eq!(row.upload_speed.as_str(), "2 KB/s");
        assert_eq!(row.download_speed.as_str(), "0 B/s");
        assert_eq!(row.start.as_str(), "12:03:44");
        assert_eq!(row.duration.as_str(), "4m 30s");
        assert!(!row.closed);
    }

    #[test]
    fn the_connection_rows_are_filtered_then_sorted_for_the_page() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let view = |id: &str, host: &str, upload: u64| ConnectionView {
            id: id.to_string(),
            host: host.to_string(),
            upload,
            ..ConnectionView::default()
        };
        let views = vec![
            view("a", "a.test", 1),
            view("b", "b.test", 9),
            view("c", "other.test", 5),
        ];
        let rows = connection_rows(&views, "test", connections::SORT_UPLOAD);
        let ids: Vec<String> = rows.iter().map(|row| row.id.to_string()).collect();
        assert_eq!(ids, vec!["b", "c", "a"]);

        let filtered = connection_rows(&views, "other", connections::SORT_UPLOAD);
        assert_eq!(filtered.len(), 1);
    }

    #[test]
    fn the_auto_close_setting_is_on_by_default_and_saved_from_the_form() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        let current = UserPreferences::default();
        assert!(settings_model(&current).auto_close_connections_on_switch);

        let mut model = settings_model(&current);
        model.auto_close_connections_on_switch = false;
        let saved = build_preferences(&current, &SettingsInput::from_model(&model))
            .expect("the form saves");
        assert!(!saved.auto_close_connections_on_switch);
        assert_eq!(
            saved.proxies_sort, current.proxies_sort,
            "the list settings are kept"
        );
    }

    #[test]
    fn a_date_is_shortened_to_the_day() {
        let _lang = crate::i18n::pin(crate::i18n::Lang::En);
        assert_eq!(short_date("2026-12-01T00:00:00Z"), "2026-12-01");
        assert_eq!(short_date("next month"), "next month");
        assert_eq!(display_time("2026-11-13T10:05:33Z"), "2026-11-13 10:05");
    }
}
