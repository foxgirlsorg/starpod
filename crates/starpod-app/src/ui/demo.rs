//! Fixed demo data for the screenshots (ui-spec.md section 7).
//!
//! `STARPOD_UI_DEMO=1` makes the window fill its Bridge with this data once,
//! after the first state push; later snapshots are then not pushed, so the
//! screens keep the demo values. The tray has its own Bridge and is left alone.
//!
//! The data is built as the service's view types and goes through the same
//! mapping helpers as a real snapshot, so emoji runs and images come out the
//! same way.

use slint::{ModelRc, SharedString, VecModel};
use starpod_core::{SubscriptionMode, SubscriptionQuota};
use starpod_ui::{Bridge, LogItem, SettingsModel, SubscriptionItem};

use crate::ui::emoji::runs_model;
use crate::ui::mapping::{
    announcement_item, group_choices, group_item, log_item, node_item, rule_item, subscription_item,
};
use crate::{
    AnnouncementView, GroupView, Latency, MemberView, NodeView, RuleView, SubscriptionStatus,
    SubscriptionView,
};

/// Whether the demo data is requested by the environment.
pub fn requested() -> bool {
    std::env::var("STARPOD_UI_DEMO").is_ok_and(|value| value.trim() == "1")
}

/// The 24 servers of the profile: name, protocol and latency in ms.
/// -1 is untested, 0 is a timeout.
const SERVERS: [(&str, &str, i32); 24] = [
    ("🇩🇪 Frankfurt 01", "vless", 38),
    ("🇳🇱 Amsterdam 02", "vless", 45),
    ("🇫🇮 Helsinki 01", "hysteria2", 61),
    ("🇸🇪 Stockholm 01", "trojan", 72),
    ("🇬🇧 London 01", "vless", 96),
    ("🇫🇷 Paris 02", "ss", 118),
    ("🇵🇱 Warsaw 01", "trojan", 142),
    ("🇨🇭 Zurich 01", "vless", 176),
    ("🇮🇹 Milan 01", "hysteria2", 210),
    ("🇺🇸 New York 01", "vless", 263),
    ("🇺🇸 Los Angeles 02", "trojan", 318),
    ("🇨🇦 Toronto 01", "ss", 371),
    ("🇯🇵 Tokyo 01", "vless", 412),
    ("🇸🇬 Singapore 01", "hysteria2", 455),
    ("🇰🇷 Seoul 01", "vless", 498),
    ("🇭🇰 Hong Kong 03", "trojan", 534),
    ("🇦🇪 Dubai 01", "vless", 612),
    ("🇮🇳 Mumbai 01", "ss", 689),
    ("🇦🇺 Sydney 01", "hysteria2", 741),
    ("🇧🇷 São Paulo 01", "vless", 823),
    ("🇹🇷 Istanbul 01", "trojan", 0),
    ("🇪🇸 Madrid 01", "vless", -1),
    ("🇨🇿 Prague 01", "ss", -1),
    ("🇦🇹 Vienna 01", "vless", -1),
];

/// The first twelve servers are the members of "🚀 Proxy", after its "⚡ Auto".
const PROXY_MEMBERS: usize = 12;

const PROXY: &str = "🚀 Proxy";
const AUTO: &str = "⚡ Auto";
const BLOCKED: &str = "🚫 Blocked sites";
const FRANKFURT: &str = "🇩🇪 Frankfurt 01";

/// Fill the window's Bridge with the demo data set.
pub fn apply(bridge: &Bridge<'_>) {
    bridge.set_loaded(true);

    // Engine: running, with the connection details.
    bridge.set_engine_phase("running".into());
    bridge.set_engine_error("".into());
    bridge.set_engine_installed(true);
    bridge.set_connect_blocked_reason("".into());
    bridge.set_engine_version("v0.29.0".into());
    bridge.set_busy(false);
    bridge.set_busy_message("".into());
    bridge.set_mode("rule".into());
    bridge.set_current_node(FRANKFURT.into());
    bridge.set_current_node_runs(runs_model(FRANKFURT));
    bridge.set_uptime("01:12:09".into());
    bridge.set_listen_address("127.0.0.1:7890".into());
    bridge.set_banner_message("".into());
    bridge.set_banner_kind("info".into());

    // One profile, with its quota and expiry.
    let profile = SubscriptionView {
        active: true,
        id: "sub-foxgirls".into(),
        label: "Foxgirls VPN".into(),
        mode: SubscriptionMode::AppManaged,
        status: SubscriptionStatus::Current,
        last_updated: Some("2026-11-14T01:58:00Z".into()),
        error: None,
        node_count: SERVERS.len(),
        group_count: 3,
        quota: Some(SubscriptionQuota {
            used: 34_200_000_000,
            total: Some(200_000_000_000),
            expires: Some("2026-11-16T00:00:00Z".into()),
        }),
        has_auth_header: false,
        title: "Foxgirls VPN".into(),
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
    };
    let subscriptions: Vec<SubscriptionItem> = vec![subscription_item(&profile)];
    bridge.set_subscriptions(ModelRc::new(VecModel::from(subscriptions)));

    // The servers, for the Proxies page.
    let nodes: Vec<_> = SERVERS
        .iter()
        .enumerate()
        .map(|(i, (name, protocol, latency))| {
            node_item(&NodeView {
                id: format!("node-{i}"),
                name: (*name).into(),
                protocol: (*protocol).into(),
                group: Some(PROXY.into()),
                selected: i == 0,
                latency_ms: (*latency >= 0).then_some(*latency as u32),
            })
        })
        .collect();
    bridge.set_nodes(ModelRc::new(VecModel::from(nodes)));

    // Groups: a manual one, an automatic one and a blocked-sites one.
    // "🚀 Proxy" starts on its automatic member, as a fresh profile does.
    let mut proxy_members = vec![MemberView {
        name: AUTO.into(),
        label: AUTO.into(),
        protocol: "group".into(),
        latency: Latency::Untested,
    }];
    proxy_members.extend(members(&SERVERS[..PROXY_MEMBERS]));
    let auto_members = members(&SERVERS[..PROXY_MEMBERS]);
    let blocked_members = vec![
        MemberView {
            name: "DIRECT".into(),
            label: "DIRECT".into(),
            protocol: "direct".into(),
            latency: Latency::Untested,
        },
        MemberView {
            name: PROXY.into(),
            label: PROXY.into(),
            protocol: "group".into(),
            latency: Latency::Untested,
        },
    ];
    let views = vec![
        GroupView {
            name: PROXY.into(),
            label: PROXY.into(),
            kind: "select".into(),
            selected: Some(AUTO.into()),
            members: proxy_members,
        },
        GroupView {
            name: AUTO.into(),
            label: AUTO.into(),
            kind: "url-test".into(),
            selected: Some(FRANKFURT.into()),
            members: auto_members,
        },
        GroupView {
            name: BLOCKED.into(),
            label: BLOCKED.into(),
            kind: "select".into(),
            selected: Some(PROXY.into()),
            members: blocked_members,
        },
    ];
    let groups: Vec<_> = views.iter().map(group_item).collect();
    bridge.set_groups(ModelRc::new(VecModel::from(groups)));
    bridge.set_group_choices(ModelRc::new(VecModel::from(group_choices(&views))));
    bridge.set_primary_group(PROXY.into());
    bridge.set_primary_group_runs(runs_model(PROXY));

    let announcements = vec![announcement_item(&AnnouncementView {
        subscription_id: "sub-foxgirls".into(),
        title: "Foxgirls VPN".into(),
        text: "Maintenance on Nov 20, 02:00–04:00 UTC. Servers in Frankfurt will restart once."
            .into(),
        announce_url: None,
        support_url: Some("https://example.com/support".into()),
        web_url: Some("https://example.com".into()),
    })];
    bridge.set_announcements(ModelRc::new(VecModel::from(announcements)));

    // Rules: six of the user's own, 300 from the profile.
    let user_rules = vec![
        rule(0, "DOMAIN-SUFFIX", "example.org", "DIRECT", true, false),
        rule(1, "DOMAIN-KEYWORD", "ads", "REJECT", true, false),
        rule(2, "IP-CIDR", "192.168.0.0/16", "DIRECT", true, false),
        rule(3, "DOMAIN", "intranet.example.com", "DIRECT", false, false),
        rule(4, "PROCESS-NAME", "steam.exe", "DIRECT", true, false),
        rule(5, "DOMAIN-SUFFIX", "github.com", PROXY, true, false),
    ];
    let subscription_rules: Vec<_> = (0..300).map(profile_rule).collect();
    bridge.set_user_rules(ModelRc::new(VecModel::from(user_rules)));
    bridge.set_subscription_rules(ModelRc::new(VecModel::from(subscription_rules)));
    bridge.set_rules_raw_text(
        "DOMAIN-SUFFIX,example.org,DIRECT\nDOMAIN-KEYWORD,ads,REJECT\nIP-CIDR,192.168.0.0/16,DIRECT"
            .into(),
    );
    bridge.set_rules_error("".into());
    bridge.set_rule_kinds(string_model(&[
        "DOMAIN",
        "DOMAIN-SUFFIX",
        "DOMAIN-KEYWORD",
        "IP-CIDR",
        "PROCESS-NAME",
    ]));
    bridge.set_rule_targets(string_model(&["DIRECT", "REJECT", PROXY, AUTO]));

    // Logs: 40 lines of every level, oldest first.
    let logs: Vec<LogItem> = (0..40).map(log_line).collect();
    bridge.set_logs(ModelRc::new(VecModel::from(logs)));
    bridge.set_log_level_filter("all".into());

    // Settings: the mixed port is set, the rest is at its default.
    bridge.set_settings(SettingsModel {
        mixed_port: 7890,
        socks_port: -1,
        http_port: -1,
        allow_lan: false,
        bind_address: "".into(),
        log_level: "info".into(),
        mode: "rule".into(),
        tun_enabled: false,
        tun_device: "".into(),
        tun_mtu: -1,
        tun_auto_route: true,
        tun_dns_hijack: "".into(),
        auto_refresh_minutes: 60,
        auto_close_connections_on_switch: true,
        system_proxy: false,
        sysproxy_mode: "manual".into(),
        sysproxy_bypass: "localhost".into(),
        pac_script: "".into(),
        autostart: false,
        silent_start: false,
        auto_connect: false,
        close_to_tray: true,
        language: String::new().into(),
        accent_color: String::new().into(),
        home_background: std::env::var("STARPOD_UI_BACKGROUND")
            .unwrap_or_default()
            .into(),
    });
    bridge.set_settings_error("".into());
    bridge.set_secret_backend("Windows Credential Manager".into());
    bridge.set_secrets_encrypted(true);
    bridge.set_secret_fallback_reason("".into());
    bridge.set_data_dir("C:\\Users\\demo\\AppData\\Roaming\\Starpod".into());
    bridge.set_warnings(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));
    apply_session_and_card(bridge);
    apply_connections(bridge);
}

/// The demo servers as group members, in order.
fn members(servers: &[(&str, &str, i32)]) -> Vec<MemberView> {
    servers
        .iter()
        .map(|(name, protocol, latency)| MemberView {
            name: (*name).into(),
            label: (*name).into(),
            protocol: (*protocol).into(),
            latency: match *latency {
                -1 => Latency::Untested,
                0 => Latency::TimedOut,
                ms => Latency::Measured(ms as u32),
            },
        })
        .collect()
}

fn string_model(values: &[&str]) -> ModelRc<SharedString> {
    let items: Vec<SharedString> = values.iter().map(|value| (*value).into()).collect();
    ModelRc::new(VecModel::from(items))
}

fn rule(
    index: usize,
    kind: &str,
    payload: &str,
    target: &str,
    enabled: bool,
    from_sub: bool,
) -> starpod_ui::RuleItem {
    rule_item(&RuleView {
        index,
        id: None,
        enabled,
        kind: kind.into(),
        payload: payload.into(),
        target: target.into(),
        line: format!("{kind},{payload},{target}"),
        from_subscription: from_sub,
    })
}

/// The n-th rule of the profile. Mostly domains and rule sets, a few IP rules.
fn profile_rule(n: usize) -> starpod_ui::RuleItem {
    const TARGETS: [&str; 3] = [PROXY, "DIRECT", AUTO];
    let target = TARGETS[n % TARGETS.len()];
    let (kind, payload) = match n % 6 {
        0 => ("RULE-SET", format!("geosite-category-{}", n % 40)),
        1 => ("DOMAIN-SUFFIX", format!("site{n}.example.net")),
        2 => ("RULE-SET", format!("geoip-region-{}", n % 25)),
        3 => ("DOMAIN-KEYWORD", format!("keyword{n}")),
        4 => ("IP-CIDR", format!("10.{}.{}.0/24", n % 255, (n * 7) % 255)),
        _ => ("DOMAIN", format!("host{n}.example.com")),
    };
    rule(n, kind, &payload, target, true, true)
}

/// Log line n: a fixed mix of info, warning, error and debug lines.
fn log_line(n: usize) -> LogItem {
    let (level, source, message) = match n % 8 {
        0 => (
            "info",
            "app",
            "profile “Foxgirls VPN” updated: 24 servers".to_string(),
        ),
        1 => (
            "info",
            "engine",
            "engine started, listening on 127.0.0.1:7890".to_string(),
        ),
        2 => (
            "debug",
            "engine",
            "dns: resolved proxy host in 12 ms".to_string(),
        ),
        3 => (
            "info",
            "app",
            format!("switched Proxy group to {FRANKFURT}"),
        ),
        4 => (
            "warning",
            "subscription",
            "profile rule 212 skipped: unknown rule type".to_string(),
        ),
        5 => (
            "info",
            "engine",
            "connection to 🇳🇱 Amsterdam 02 closed".to_string(),
        ),
        6 => (
            "error",
            "engine",
            "tls handshake failed: certificate expired".to_string(),
        ),
        _ => (
            "debug",
            "app",
            "latency test finished: 24 servers".to_string(),
        ),
    };
    let time = format!("12:{:02}:{:02}", 4 + n / 10, (n * 13) % 60);
    log_item(&time, level, source, &message)
}

// --- Connections page (demo) ------------------------------------------------
// The demo data of the Connections page. `apply_connections` fills it; it is
// called from `apply`, after the rest of the demo data.

/// The demo hosts, processes and rules; the n-th connection takes the n-th of each.
const DEMO_HOSTS: [&str; 12] = [
    "www.youtube.com",
    "api.github.com",
    "ocsp.digicert.com",
    "telemetry.vendor.net",
    "video.twimg.com",
    "registry.npmjs.org",
    "steamcommunity.com",
    "cdn.jsdelivr.net",
    "discord.gg",
    "ipinfo.io",
    "gateway.icloud.com",
    "www.wikipedia.org",
];
const DEMO_PROCESSES: [&str; 12] = [
    "chrome.exe",
    "code.exe",
    "",
    "svchost.exe",
    "chrome.exe",
    "node.exe",
    "steam.exe",
    "",
    "Discord.exe",
    "",
    "",
    "firefox.exe",
];
const DEMO_RULES: [&str; 6] = [
    "DOMAIN-SUFFIX,youtube.com",
    "DOMAIN-SUFFIX,github.com",
    "RULE-SET,geosite-category-ads-all",
    "DOMAIN-KEYWORD,steam",
    "GEOIP,private",
    "MATCH",
];

/// The n-th demo connection. Every fourth one goes direct and the rest use the
/// proxy and the Frankfurt server. A closed one keeps its last figures.
fn demo_connection(n: usize, closed: bool) -> starpod_ui::ConnectionItem {
    let chains = if n % 4 == 3 {
        vec!["DIRECT".to_string()]
    } else {
        vec![PROXY.to_string(), FRANKFURT.to_string()]
    };
    let scale = n as u64 + 1;
    let view = crate::connections::ConnectionView {
        id: format!("{}-{n}", if closed { "closed" } else { "conn" }),
        host: DEMO_HOSTS[n % DEMO_HOSTS.len()].into(),
        destination: format!("203.0.113.{}:443", 10 + n),
        network: "tcp".into(),
        kind: "HTTPS".into(),
        process: DEMO_PROCESSES[n % DEMO_PROCESSES.len()].into(),
        chains,
        rule: DEMO_RULES[n % DEMO_RULES.len()].into(),
        upload: scale * 180_000,
        download: scale * 2_400_000 + 512_000,
        upload_speed: if closed { 0 } else { (n as u64 % 5) * 1_024 },
        download_speed: if closed {
            0
        } else {
            ((n as u64 * 37) % 19) * 40_960
        },
        started_at: Some("2026-11-14T12:03:44Z".into()),
        started_unix: None,
        duration_secs: 240 + scale * 91,
        closed,
    };
    crate::ui::mapping::connection_item(&view)
}

/// Fill the Connections page with twelve open connections and six closed ones.
pub fn apply_connections(bridge: &Bridge<'_>) {
    let active: Vec<_> = (0..12).map(|n| demo_connection(n, false)).collect();
    let closed: Vec<_> = (0..6).map(|n| demo_connection(n, true)).collect();
    bridge.set_connections_active_count(active.len() as i32);
    bridge.set_connections(ModelRc::new(VecModel::from(active)));
    bridge.set_closed_connections(ModelRc::new(VecModel::from(closed)));
    bridge.set_connections_total_upload("312.4 MB".into());
    bridge.set_connections_total_download("1.8 GB".into());
    bridge.set_connections_filter("".into());
    bridge.set_connections_sort(0);
    bridge.set_connections_paused(false);
}

/// The profile card, the live traffic and the system proxy: the Home page's
/// demo. The card's plan ends in two days, so the expiry warning shows.
fn apply_session_and_card(bridge: &Bridge<'_>) {
    let card = crate::snapshot::ProfileCard {
        subscription_id: "sub-foxgirls".into(),
        title: "🇩🇪 Foxgirls VPN".into(),
        logo_path: None,
        announce: Some(
            "🎁 Double traffic on weekends until Nov 30. Thanks for staying with us!".into(),
        ),
        announce_url: Some("https://example.com/news".into()),
        support_url: Some("https://example.com/support".into()),
        web_url: Some("https://example.com".into()),
        web_page_name: Some("example.com".into()),
        has_quota: true,
        traffic_used_bytes: 34_200_000_000,
        traffic_total_bytes: 100_000_000_000,
        expires: Some("2026-11-16T00:00:00Z".into()),
        expired: false,
        days_remaining: 2,
        expiring_soon: true,
        device_limit: false,
    };
    let card_item = crate::ui::mapping::profile_card_item(Some(&card), &Default::default());
    bridge.set_profile_card(card_item);
    bridge.set_session_traffic_visible(true);
    bridge.set_session_upload("12.3 MB".into());
    bridge.set_session_download("1.8 GB".into());
    bridge.set_system_proxy_active(true);
    bridge.set_global_mode_allowed(true);
}
