//! Live end-to-end check against a real subscription and the real engine.
//!
//! Ignored by default: it needs the network, downloads the engine and starts
//! it. Run with
//!   STARPOD_LIVE_SUB=<url> cargo test -p starpod-app --test live_windows -- --ignored --nocapture
//! Everything is written under a temporary directory; the user's own Starpod
//! data is never read or written.

use std::sync::Arc;
use std::time::Duration;

use starpod_app::{AppService, ServiceDeps, SubscriptionStatus};
use starpod_core::rules::parse_line;
use starpod_core::{ProxyMode, SubscriptionMode};
use starpod_engine::MeowEngine;
use starpod_store::secret::open_secret_store;
use starpod_store::{AppPaths, Store};
use starpod_subscription::HttpSubscriptionProvider;

macro_rules! step {
    ($name:expr, $body:expr) => {{
        let started = std::time::Instant::now();
        let result = $body;
        match &result {
            Ok(_) => println!("PASS  {:<44} ({} ms)", $name, started.elapsed().as_millis()),
            Err(error) => println!("FAIL  {:<44} {error}", $name),
        }
        result
    }};
}

fn check(name: &str, ok: bool, detail: impl std::fmt::Display) -> bool {
    println!("{}  {name:<44} {detail}", if ok { "PASS" } else { "FAIL" });
    ok
}

/// Send one HTTP request through the engine's mixed port and return the
/// status code.
async fn via_proxy(port: u16, url: &str) -> Result<u16, String> {
    let proxy =
        reqwest::Proxy::all(format!("http://127.0.0.1:{port}")).map_err(|e| e.to_string())?;
    let client = reqwest::Client::builder()
        .proxy(proxy)
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client.get(url).send().await.map_err(|e| e.to_string())?;
    Ok(response.status().as_u16())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "live: needs network, a real subscription and the real engine"]
async fn live_subscription_end_to_end() {
    let url =
        std::env::var("STARPOD_LIVE_SUB").expect("set STARPOD_LIVE_SUB to a subscription URL");
    let tmp = tempfile::tempdir().unwrap();
    let paths = AppPaths::from_roots(
        tmp.path().join("cfg"),
        tmp.path().join("data"),
        tmp.path().join("cache"),
    );
    let data_dir = paths.data_dir().to_path_buf();
    std::fs::create_dir_all(&data_dir).unwrap();
    let engine = MeowEngine::new(
        starpod_engine::download::installed_binary_path_in(&data_dir),
        paths.runtime_config_file(),
    )
    .with_controller_dir(paths.controller_dir());
    let mut failures = 0usize;
    let mut fail = |ok: bool| {
        if !ok {
            failures += 1;
        }
    };

    let mut service = AppService::new(ServiceDeps {
        secrets: open_secret_store(paths.fallback_secret_file()),
        store: Store::new(paths),
        engine: Box::new(engine),
        provider: Arc::new(HttpSubscriptionProvider::new().unwrap()),
        engine_installed: false,
    });
    fail(step!("load empty state", service.load().await).is_ok());
    let snap = service.snapshot();
    fail(check(
        "secret store is DPAPI and encrypted",
        snap.secret_encrypted,
        &snap.secret_backend,
    ));
    fail(check(
        "connect blocked without subscription",
        snap.connect_blocked.is_some(),
        format!("{:?}", snap.connect_blocked),
    ));

    // Keep clear of any proxy the user already runs on 7890/7891.
    let mut prefs = service.user_state().preferences.clone();
    prefs.mixed_port = Some(17890);
    prefs.socks_port = Some(17891);
    fail(
        step!(
            "save preferences (ports)",
            service.save_preferences(prefs).await
        )
        .is_ok(),
    );

    fail(
        step!(
            "install engine (download + sha256)",
            service.install_engine().await
        )
        .is_ok(),
    );
    let snap = service.snapshot();
    fail(check(
        "engine installed",
        snap.engine.installed,
        format!("{:?}", snap.engine.version),
    ));

    let id = step!(
        "add subscription",
        service
            .add_subscription(
                "Live".into(),
                url.clone(),
                None,
                SubscriptionMode::AppManaged
            )
            .await
    );
    let Ok(id) = id else {
        panic!("cannot continue")
    };
    fail(
        step!(
            "refresh subscription",
            service.refresh_subscription(&id).await
        )
        .is_ok(),
    );
    let snap = service.snapshot();
    let sub = snap.subscriptions.iter().find(|s| s.id == id).unwrap();
    println!(
        "      sub: status={:?} nodes={} groups={} title={:?} interval={}s quota={:?} support={:?} web={:?} error={:?}",
        sub.status,
        sub.node_count,
        sub.group_count,
        sub.title,
        sub.interval_secs,
        sub.quota,
        sub.support_url,
        sub.web_url,
        sub.error
    );
    println!("      announce: {:?}", sub.announce);
    fail(check(
        "subscription ok",
        matches!(sub.status, SubscriptionStatus::Current),
        format!("{:?}", sub.status),
    ));
    fail(check("nodes parsed", sub.node_count > 0, sub.node_count));
    fail(check("groups parsed", sub.group_count > 0, sub.group_count));
    fail(check(
        "profile-title decoded",
        sub.title != "Live",
        &sub.title,
    ));
    fail(check(
        "announce decoded",
        sub.announce
            .as_deref()
            .is_some_and(|a| !a.starts_with("base64:")),
        "",
    ));
    fail(check("support url kept", sub.support_url.is_some(), ""));
    fail(check(
        "update interval 6h",
        sub.interval_secs == 6 * 3600,
        sub.interval_secs,
    ));
    fail(check("profile card shown", snap.profile_card.is_some(), ""));
    fail(check(
        "primary group found",
        snap.primary_group.is_some(),
        format!("{:?}", snap.primary_group),
    ));
    println!(
        "      groups: {}",
        snap.groups
            .iter()
            .map(|g| format!("{}[{}:{}]", g.name, g.kind, g.members.len()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("      warnings: {:?}", snap.warnings);
    println!(
        "      subscription rules: {}",
        snap.subscription_rules.len()
    );
    fail(check(
        "no secret in app log",
        !snap.log_lines.iter().any(|l| l.contains(&url)),
        "",
    ));

    match step!(
        "check configuration (meow -t)",
        service.check_configuration().await
    ) {
        Ok(check_result) => println!("      {check_result:?}"),
        Err(_) => fail(false),
    }

    fail(step!("connect", service.connect().await).is_ok());
    tokio::time::sleep(Duration::from_secs(3)).await;
    let snap = service.snapshot();
    fail(check(
        "engine running",
        snap.engine.phase.is_running(),
        format!("{:?} {:?}", snap.engine.phase, snap.engine.error),
    ));
    let runtime =
        std::fs::read_to_string(service.paths().runtime_config_file()).unwrap_or_default();
    fail(check("user port used (P1)", runtime.contains("17890"), ""));
    fail(check(
        "allow-lan not taken from sub (P1a)",
        !runtime.contains("allow-lan: true"),
        "",
    ));

    match via_proxy(17890, "https://www.gstatic.com/generate_204").await {
        Ok(code) => fail(check("HTTP through proxy (rule mode)", code == 204, code)),
        Err(e) => fail(check("HTTP through proxy (rule mode)", false, e)),
    }

    fail(step!("latency test all nodes", service.test_latency().await).is_ok());
    let snap = service.snapshot();
    let measured = snap.nodes.iter().filter(|n| n.latency_ms.is_some()).count();
    fail(check(
        "some nodes answered latency",
        measured > 0,
        format!("{measured}/{}", snap.nodes.len()),
    ));

    if let Some(primary) = snap.primary_group.clone() {
        fail(step!("test group latency", service.test_group(&primary).await).is_ok());
        let group = snap.groups.iter().find(|g| g.name == primary).unwrap();
        if let Some(member) = group
            .members
            .iter()
            .find(|m| Some(&m.name) != group.selected.as_ref() && m.protocol != "direct")
        {
            fail(
                step!(
                    "select member in primary group",
                    service.select_member(&primary, &member.name).await
                )
                .is_ok(),
            );
            let after = service.snapshot();
            let selected = after
                .groups
                .iter()
                .find(|g| g.name == primary)
                .and_then(|g| g.selected.clone());
            fail(check(
                "selection applied",
                selected.as_deref() == Some(member.name.as_str()),
                format!("{selected:?}"),
            ));
            fail(check(
                "selection saved",
                after.preferences.group_selections.get(&primary) == Some(&member.name),
                "",
            ));
            match via_proxy(17890, "https://www.gstatic.com/generate_204").await {
                Ok(code) => fail(check("HTTP through new member", code == 204, code)),
                Err(e) => fail(check("HTTP through new member", false, e)),
            }
        }
    }

    for mode in [ProxyMode::Global, ProxyMode::Direct, ProxyMode::Rule] {
        let name = format!("mode {mode:?}");
        fail(step!(name.as_str(), service.set_mode(mode).await).is_ok());
        match via_proxy(17890, "https://www.gstatic.com/generate_204").await {
            Ok(code) => fail(check(&format!("HTTP in {mode:?}"), code == 204, code)),
            Err(e) => fail(check(&format!("HTTP in {mode:?}"), false, e)),
        }
    }

    // Connections view.
    let _ = via_proxy(17890, "https://www.gstatic.com/generate_204").await;
    service.refresh_connections().await;
    println!(
        "      connections: {:?}",
        service.snapshot().connections.active.len()
    );
    fail(
        step!(
            "close all connections",
            service.close_all_connections().await
        )
        .is_ok(),
    );

    // Rules: structured add, update, disable, move, delete, raw text.
    let rule = parse_line("DOMAIN-SUFFIX,example.org,DIRECT").unwrap();
    let rule_id = step!("add rule", service.add_rule(rule).await);
    if let Ok(rule_id) = &rule_id {
        let updated = parse_line("DOMAIN-SUFFIX,example.net,REJECT").unwrap();
        fail(
            step!(
                "update rule",
                service.update_rule(rule_id, updated.clone(), true).await
            )
            .is_ok(),
        );
        fail(
            step!(
                "disable rule",
                service.update_rule(rule_id, updated.clone(), false).await
            )
            .is_ok(),
        );
        fail(check(
            "rule shown disabled",
            service.snapshot().rules.iter().any(|r| !r.enabled),
            "",
        ));
        fail(
            step!(
                "enable rule",
                service.update_rule(rule_id, updated, true).await
            )
            .is_ok(),
        );
        fail(step!("move rule", service.move_rule(rule_id, 0).await).is_ok());
        fail(step!("delete rule", service.delete_rule(rule_id).await).is_ok());
    } else {
        fail(false);
    }
    let raw = "DOMAIN,a.example,DIRECT\n# comment\nDOMAIN-KEYWORD,tracker,REJECT\nIP-CIDR,10.0.0.0/8,DIRECT,no-resolve\n";
    fail(step!("apply raw rules", service.apply_raw_rules(raw).await).is_ok());
    fail(check(
        "raw rules stored",
        service.snapshot().rules.len() == 3,
        service.snapshot().rules.len(),
    ));
    fail(check(
        "bad raw rules refused",
        service
            .apply_raw_rules("DOMAIN,x\nFOO,bar,DIRECT")
            .await
            .is_err(),
        "",
    ));
    fail(check(
        "unknown target refused",
        service
            .apply_raw_rules("DOMAIN,x.example,NoSuchGroup")
            .await
            .is_err(),
        "",
    ));
    match via_proxy(17890, "https://www.gstatic.com/generate_204").await {
        Ok(code) => fail(check("HTTP after rules reload", code == 204, code)),
        Err(e) => fail(check("HTTP after rules reload", false, e)),
    }

    // System proxy (Windows per-user settings), then restore.
    fail(step!("system proxy on", service.set_system_proxy(true).await).is_ok());
    let snap = service.snapshot();
    fail(check(
        "system proxy active",
        snap.system_proxy_active,
        format!("{:?}", snap.system_proxy_error),
    ));
    fail(step!("system proxy off", service.set_system_proxy(false).await).is_ok());
    fail(check(
        "system proxy inactive",
        !service.snapshot().system_proxy_active,
        "",
    ));

    fail(step!("engine update check", service.check_engine_update().await).is_ok());
    println!("      {:?}", service.snapshot().engine_update);
    fail(step!("update geodata", service.update_geo_data().await).is_ok());

    fail(step!("restart", service.restart().await).is_ok());
    tokio::time::sleep(Duration::from_secs(2)).await;
    match via_proxy(17890, "https://www.gstatic.com/generate_204").await {
        Ok(code) => fail(check("HTTP after restart", code == 204, code)),
        Err(e) => fail(check("HTTP after restart", false, e)),
    }
    fail(
        step!(
            "refresh while running (reload)",
            service.refresh_subscription(&id).await
        )
        .is_ok(),
    );

    fail(step!("disconnect", service.disconnect().await).is_ok());
    fail(check(
        "engine stopped",
        !service.snapshot().engine.phase.is_running(),
        "",
    ));

    // Engine-managed mode on the same subscription.
    fail(
        step!(
            "switch to engine-managed",
            service
                .set_subscription_mode(&id, SubscriptionMode::EngineManaged)
                .await
        )
        .is_ok(),
    );
    fail(step!("connect engine-managed", service.connect().await).is_ok());
    tokio::time::sleep(Duration::from_secs(5)).await;
    match via_proxy(17890, "https://www.gstatic.com/generate_204").await {
        Ok(code) => fail(check("HTTP engine-managed", code == 204, code)),
        Err(e) => fail(check("HTTP engine-managed", false, e)),
    }
    let _ = service.disconnect().await;
    fail(
        step!(
            "back to app-managed",
            service
                .set_subscription_mode(&id, SubscriptionMode::AppManaged)
                .await
        )
        .is_ok(),
    );

    // Reload from disk: settings and selection survive.
    let saved_selections = service.snapshot().preferences.group_selections.clone();
    fail(step!("reload state from disk", service.load().await).is_ok());
    fail(check(
        "selections persisted",
        service.snapshot().preferences.group_selections == saved_selections,
        "",
    ));

    fail(step!("clear cache", service.clear_cache().await).is_ok());
    fail(
        step!(
            "remove subscription",
            service.remove_subscription(&id).await
        )
        .is_ok(),
    );
    fail(check(
        "no subscriptions left",
        service.snapshot().subscriptions.is_empty(),
        "",
    ));

    println!("\n{failures} failure(s)");
    for line in service.snapshot().log_lines.iter().rev().take(15).rev() {
        println!("  log: {line}");
    }
    assert_eq!(failures, 0, "live checks failed");
}
