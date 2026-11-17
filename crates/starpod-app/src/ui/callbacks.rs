//! Every `Bridge` callback, mapped to a service call (ARCHITECTURE §6.3).
//!
//! A callback does one of two things. Either it runs an action on the
//! service, through [`act`], which shows busy and reports errors. Or it
//! changes view state only (filters, banners, the clipboard), which runs
//! directly on the Slint thread. The same wiring is applied to the window's
//! Bridge and to the tray's, since they are separate instances.

use std::future::Future;

use crate::deeplink::InstallRequest;
use crate::{AppError, AppService, SharedAppService};
use starpod_core::{RuleSpec, SubscriptionMode};
use starpod_ui::{AppWindow, Bridge, EngineSettingsModel, HotkeyModel, SettingsModel};
use tracing::warn;

use super::hotkeys;
use super::mapping::{
    SettingsInput, build_preferences, engine_form_from_model, moved_position, parse_proxy_mode,
    parse_rule_line, parse_subscription_mode, rule_line,
};
use super::runtime::{Ctx, Sink, request_quit, spawn_update_check, with_ui};

/// Run one service action in the background. `busy` shows while it runs.
/// Its error, if any, goes to `sink`. A success clears that sink's error.
pub fn act<T, Fut>(
    ctx: &Ctx,
    busy: &'static str,
    sink: Sink,
    op: impl FnOnce(SharedAppService) -> Fut + Send + 'static,
) where
    T: Send + 'static,
    Fut: Future<Output = Result<T, AppError>> + Send + 'static,
{
    let service = ctx.service.clone();
    ctx.runtime.spawn(async move {
        super::runtime::post(move |ui| ui.begin_busy(busy));
        let failure = op(service).await.err().map(|error| {
            warn!(%error, action = busy, "an action failed");
            error.to_string()
        });
        super::runtime::post(move |ui| {
            ui.end_busy();
            match failure {
                Some(message) => ui.report(sink, message),
                None => ui.clear_error(sink),
            }
        });
    });
}

/// Save which Proxies groups are open, in the background, so they open the
/// same way next time.
pub fn save_expanded_groups(ctx: &Ctx, expanded: std::collections::BTreeSet<String>) {
    let service = ctx.service.clone();
    ctx.runtime.spawn(async move {
        let mut svc = service.lock().await;
        let mut prefs = svc.user_state().preferences.clone();
        if prefs.expanded_groups == expanded {
            return;
        }
        prefs.expanded_groups = expanded;
        if let Err(error) = svc.save_preferences(prefs).await {
            warn!(%error, "could not save the open proxy groups");
        }
    });
}

/// Save the Proxies list settings the page shows (member sort and hiding), in
/// the background. It is quiet: the page already shows the new order, so no
/// busy message is raised.
pub fn save_list_prefs(ctx: &Ctx, sort: u8, hide_unavailable: bool) {
    let service = ctx.service.clone();
    ctx.runtime.spawn(async move {
        let mut svc = service.lock().await;
        let mut prefs = svc.user_state().preferences.clone();
        prefs.proxies_sort = sort;
        prefs.hide_unavailable = hide_unavailable;
        if let Err(error) = svc.save_preferences(prefs).await {
            warn!(%error, "could not save the proxy list settings");
        }
    });
}

/// Attach every callback to one `Bridge`.
pub fn wire(bridge: &Bridge<'_>, ctx: &Ctx, window: slint::Weak<AppWindow>) {
    wire_engine(bridge, ctx);
    wire_servers(bridge, ctx);
    wire_rules(bridge, ctx);
    wire_settings(bridge, ctx);
    wire_install(bridge, ctx);
    wire_tray_actions(bridge, ctx);
    wire_shell(bridge, ctx, window);
}

/// The confirmation for a `starpod://` link. Nothing is added until the user
/// confirms it.
fn wire_install(bridge: &Bridge<'_>, ctx: &Ctx) {
    let c = ctx.clone();
    bridge.on_confirm_install(move || {
        let mut pending = None;
        with_ui(|ui| pending = ui.pending_install());
        match pending {
            Some(request) => act(&c, "Adding the subscription…", Sink::Install, move |s| {
                install_subscription_flow(s, request)
            }),
            None => with_ui(|ui| ui.end_install()),
        }
    });

    bridge.on_cancel_install(|| with_ui(|ui| ui.end_install()));
}

/// Add the subscription a link asked for, or refresh the one that already
/// has its address. The subscription is fetched at once, as in the Profiles
/// form, so the result is visible when the dialog closes.
async fn install_subscription_flow(
    s: SharedAppService,
    request: InstallRequest,
) -> Result<(), AppError> {
    let mut svc = s.lock().await;
    if let Some(id) = svc.subscription_id_for_url(&request.url) {
        return svc.refresh_subscription(&id).await;
    }
    let label = request.label();
    let id = svc
        .add_subscription(label, request.url, None, SubscriptionMode::AppManaged)
        .await?;
    svc.refresh_subscription(&id).await
}

fn wire_engine(bridge: &Bridge<'_>, ctx: &Ctx) {
    let c = ctx.clone();
    bridge.on_connect(move || {
        act(&c, "Connecting…", Sink::Banner, |s| async move {
            s.lock().await.connect().await
        });
    });

    let c = ctx.clone();
    bridge.on_disconnect(move || {
        act(&c, "Disconnecting…", Sink::Banner, |s| async move {
            s.lock().await.disconnect().await
        });
    });

    let c = ctx.clone();
    bridge.on_restart(move || {
        act(&c, "Restarting the proxy…", Sink::Banner, |s| async move {
            s.lock().await.restart().await
        });
    });

    let c = ctx.clone();
    bridge.on_set_mode(move |mode| {
        let mode = mode.to_string();
        act(
            &c,
            "Changing the mode…",
            Sink::Banner,
            move |s| async move {
                let mode = parse_proxy_mode(&mode)?;
                s.lock().await.set_mode(mode).await
            },
        );
    });

    let c = ctx.clone();
    bridge.on_install_engine(move || {
        act(&c, "Installing the engine…", Sink::Banner, |s| async move {
            s.lock().await.install_engine().await
        });
    });

    let c = ctx.clone();
    bridge.on_check_engine_update(move || {
        act(
            &c,
            "Checking for an engine update…",
            Sink::Banner,
            |s| async move { s.lock().await.check_engine_update().await },
        );
    });

    let c = ctx.clone();
    bridge.on_upgrade_engine(move || {
        act(&c, "Updating the engine…", Sink::Banner, |s| async move {
            s.lock().await.upgrade_engine().await
        });
    });

    let c = ctx.clone();
    bridge.on_update_geo_data(move || {
        act(&c, "Updating the geodata…", Sink::Banner, |s| async move {
            s.lock().await.update_geo_data().await
        });
    });

    let c = ctx.clone();
    bridge.on_test_group(move |group| {
        let group = group.to_string();
        act(
            &c,
            "Testing the group…",
            Sink::Banner,
            move |s| async move { s.lock().await.test_group(&group).await },
        );
    });
}

fn wire_servers(bridge: &Bridge<'_>, ctx: &Ctx) {
    let c = ctx.clone();
    bridge.on_add_subscription(move |label, url, header, mode| {
        let label = label.to_string();
        let url = url.to_string();
        let header = header.to_string();
        let mode = mode.to_string();
        act(&c, "Adding the subscription…", Sink::Banner, move |s| {
            add_subscription_flow(s, label, url, header, mode)
        });
    });

    let c = ctx.clone();
    bridge.on_remove_subscription(move |id| {
        let id = id.to_string();
        act(
            &c,
            "Removing the subscription…",
            Sink::Banner,
            move |s| async move { s.lock().await.remove_subscription(&id).await },
        );
    });

    let c = ctx.clone();
    bridge.on_refresh_subscription(move |id| {
        let id = id.to_string();
        act(
            &c,
            "Refreshing the subscription…",
            Sink::Banner,
            move |s| async move { s.lock().await.refresh_subscription(&id).await },
        );
    });

    let c = ctx.clone();
    bridge.on_activate_subscription(move |id| {
        let id = id.to_string();
        act(
            &c,
            "Switching the subscription…",
            Sink::Banner,
            move |s| async move {
                let mut svc = s.lock().await;
                svc.set_active_subscription(&id).await?;
                ping_profile(&mut svc).await;
                Ok(())
            },
        );
    });

    let c = ctx.clone();
    bridge.on_add_local_subscription(move |label, text| {
        let label = label.to_string();
        let text = text.to_string();
        act(
            &c,
            "Adding the profile…",
            Sink::Banner,
            move |s| async move {
                let mut svc = s.lock().await;
                svc.add_local_subscription(label, text).await?;
                ping_profile(&mut svc).await;
                Ok(())
            },
        );
    });

    let c = ctx.clone();
    bridge.on_open_profile_editor(move |id| {
        let id = id.to_string();
        let service = c.service.clone();
        c.runtime.spawn(async move {
            let mut svc = service.lock().await;
            let title = svc
                .snapshot()
                .subscriptions
                .iter()
                .find(|view| view.id == id)
                .map(|view| view.title.clone())
                .unwrap_or_default();
            let result = svc.profile_source(&id).await;
            drop(svc);
            super::runtime::post(move |ui| match result {
                Ok(source) => ui.open_profile_editor(&id, &title, &source.text, source.local),
                Err(error) => ui.report(Sink::Banner, error.to_string()),
            });
        });
    });

    let c = ctx.clone();
    bridge.on_save_profile_source(move |id, text| {
        let id = id.to_string();
        let text = text.to_string();
        let service = c.service.clone();
        c.runtime.spawn(async move {
            super::runtime::post(|ui| ui.begin_busy("Saving the profile…"));
            let result = service.lock().await.save_profile_source(&id, text).await;
            super::runtime::post(move |ui| {
                ui.end_busy();
                ui.finish_profile_edit(result.err().map(|error| error.to_string()));
            });
        });
    });

    let c = ctx.clone();
    bridge.on_refresh_all(move || {
        act(
            &c,
            "Refreshing the subscriptions…",
            Sink::Banner,
            |s| async move { s.lock().await.refresh_all().await.map(|_| ()) },
        );
    });

    let c = ctx.clone();
    bridge.on_set_subscription_mode(move |id, mode| {
        let id = id.to_string();
        let mode = mode.to_string();
        act(
            &c,
            "Changing the subscription…",
            Sink::Banner,
            move |s| async move {
                let mode = parse_subscription_mode(&mode)?;
                s.lock().await.set_subscription_mode(&id, mode).await
            },
        );
    });

    let c = ctx.clone();
    bridge.on_select_node(move |id| {
        let id = id.to_string();
        act(
            &c,
            "Selecting the node…",
            Sink::Banner,
            move |s| async move { s.lock().await.select_node(&id).await },
        );
    });

    let c = ctx.clone();
    bridge.on_select_member(move |group, member| {
        let group = group.to_string();
        let member = member.to_string();
        act(
            &c,
            "Choosing the server…",
            Sink::Banner,
            move |s| async move { s.lock().await.select_member(&group, &member).await },
        );
    });

    let c = ctx.clone();
    bridge.on_test_latency(move || {
        act(&c, "Testing the nodes…", Sink::Banner, |s| async move {
            s.lock().await.test_latency().await
        });
    });

    let c = ctx.clone();
    bridge.on_toggle_group(move |group| {
        let mut expanded = None;
        with_ui(|ui| expanded = Some(ui.toggle_group(group.as_str())));
        if let Some(expanded) = expanded {
            save_expanded_groups(&c, expanded);
        }
    });

    let c = ctx.clone();
    bridge.on_close_connection(move |id| {
        let id = id.to_string();
        act(
            &c,
            "Closing the connection…",
            Sink::Banner,
            move |s| async move { s.lock().await.close_connection(&id).await },
        );
    });

    let c = ctx.clone();
    bridge.on_close_all_connections(move || {
        act(
            &c,
            "Closing the connections…",
            Sink::Banner,
            |s| async move { s.lock().await.close_all_connections().await },
        );
    });

    let c = ctx.clone();
    bridge.on_clear_closed_connections(move || {
        let service = c.service.clone();
        c.runtime.spawn(async move {
            service.lock().await.clear_closed_connections();
        });
    });

    bridge.on_open_url(|link| {
        if let Err(message) = open_link(&link) {
            with_ui(|ui| ui.report(Sink::Banner, message));
        }
    });
}

/// Open a web link in the user's browser.
///
/// Only `http` and `https` links are opened: a subscription supplies these
/// addresses, so anything else is refused. The browser is started without a
/// shell, with the link as a single argument, so no character of the link is
/// read as a command.
pub fn open_link(text: &str) -> Result<(), String> {
    let url =
        url::Url::parse(text.trim()).map_err(|_| "that link is not a valid address".to_string())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only web links can be opened".to_string());
    }
    let program = if cfg!(windows) {
        "explorer.exe"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(url.as_str())
        .spawn()
        .map(|_| ())
        .map_err(|error| {
            crate::i18n::fmt("the link could not be opened: {}", &[&error.to_string()])
        })
}

/// Add the subscription, then fetch it at once if the app fetches it. The
/// service validates the label and the address.
async fn add_subscription_flow(
    s: SharedAppService,
    label: String,
    url: String,
    header: String,
    mode: String,
) -> Result<(), AppError> {
    let mode = parse_subscription_mode(&mode)?;
    let header = (!header.trim().is_empty()).then_some(header);
    let mut svc = s.lock().await;
    let id = svc.add_subscription(label, url, header, mode).await?;
    if mode == starpod_core::SubscriptionMode::AppManaged {
        svc.refresh_subscription(&id).await?;
    }
    ping_profile(&mut svc).await;
    Ok(())
}

/// Test every server of the profile in use once, so a new or newly chosen
/// profile shows its latencies without the user asking. A failed test is
/// logged, not reported: the change itself worked.
async fn ping_profile(svc: &mut AppService) {
    if let Err(error) = svc.test_latency().await {
        warn!(%error, "the latency test after a profile change failed");
    }
}

fn wire_rules(bridge: &Bridge<'_>, ctx: &Ctx) {
    let c = ctx.clone();
    bridge.on_add_rule(move |kind, payload, target| {
        let line = rule_line(&kind, &payload, &target);
        act(&c, "Adding the rule…", Sink::Rules, move |s| async move {
            let spec = parse_rule_line(&line)?;
            s.lock().await.add_rule(spec).await.map(|_| ())
        });
    });

    let c = ctx.clone();
    bridge.on_update_rule(move |index, kind, payload, target| {
        let line = rule_line(&kind, &payload, &target);
        act(&c, "Saving the rule…", Sink::Rules, move |s| async move {
            let spec = parse_rule_line(&line)?;
            let mut svc = s.lock().await;
            let (id, enabled) = stored_rule(&svc, index)?;
            svc.update_rule(&id, spec, enabled).await
        });
    });

    let c = ctx.clone();
    bridge.on_delete_rule(move |index| {
        act(&c, "Deleting the rule…", Sink::Rules, move |s| async move {
            let mut svc = s.lock().await;
            let (id, _) = stored_rule(&svc, index)?;
            svc.delete_rule(&id).await
        });
    });

    let c = ctx.clone();
    bridge.on_set_rule_enabled(move |index, enabled| {
        let busy = if enabled {
            "Enabling the rule…"
        } else {
            "Disabling the rule…"
        };
        act(&c, busy, Sink::Rules, move |s| async move {
            let mut svc = s.lock().await;
            let (id, spec) = stored_rule_spec(&svc, index)?;
            svc.update_rule(&id, spec, enabled).await
        });
    });

    let c = ctx.clone();
    bridge.on_move_rule(move |index, delta| {
        act(&c, "Moving the rule…", Sink::Rules, move |s| async move {
            let mut svc = s.lock().await;
            let (id, _) = stored_rule(&svc, index)?;
            let len = svc.user_state().rules.len();
            let to = moved_position(index, delta, len)
                .ok_or_else(|| AppError::NotFound("that rule".to_string()))?;
            svc.move_rule(&id, to).await
        });
    });

    let c = ctx.clone();
    bridge.on_apply_raw_rules(move |text| {
        let text = text.to_string();
        act(&c, "Saving the rules…", Sink::Rules, move |s| async move {
            s.lock().await.apply_raw_rules(&text).await.map(|_| ())
        });
    });
}

/// The id and enabled flag of the user's rule at a list position.
fn stored_rule(svc: &AppService, index: i32) -> Result<(String, bool), AppError> {
    usize::try_from(index)
        .ok()
        .and_then(|position| svc.user_state().rules.get(position))
        .map(|rule| (rule.id.clone(), rule.enabled))
        .ok_or_else(|| AppError::NotFound("that rule".to_string()))
}

/// The id and shape of the user's rule at a list position.
fn stored_rule_spec(svc: &AppService, index: i32) -> Result<(String, RuleSpec), AppError> {
    usize::try_from(index)
        .ok()
        .and_then(|position| svc.user_state().rules.get(position))
        .map(|rule| (rule.id.clone(), rule.spec.clone()))
        .ok_or_else(|| AppError::NotFound("that rule".to_string()))
}

fn wire_settings(bridge: &Bridge<'_>, ctx: &Ctx) {
    let c = ctx.clone();
    bridge.on_toggle_system_proxy(move |enabled: bool| {
        act(
            &c,
            "Changing the system proxy…",
            Sink::Settings,
            move |s| async move { s.lock().await.set_system_proxy(enabled).await },
        );
    });

    let c = ctx.clone();
    bridge.on_save_settings(move |model: SettingsModel| {
        let input = SettingsInput::from_model(&model);
        act(
            &c,
            "Saving the settings…",
            Sink::Settings,
            move |s| async move {
                let mut svc = s.lock().await;
                let prefs = build_preferences(&svc.user_state().preferences, &input)?;
                svc.save_preferences(prefs).await
            },
        );
    });

    let c = ctx.clone();
    bridge.on_save_engine_settings(move |model: EngineSettingsModel| {
        let form = engine_form_from_model(&model);
        act(
            &c,
            "Saving the engine settings…",
            Sink::Settings,
            move |s| async move {
                let mut svc = s.lock().await;
                let prefs = form
                    .apply(&svc.user_state().preferences)
                    .map_err(AppError::Invalid)?;
                svc.save_preferences(prefs).await
            },
        );
    });

    let c = ctx.clone();
    bridge.on_clear_cache(move || {
        act(&c, "Clearing the cache…", Sink::Banner, |s| async move {
            s.lock().await.clear_cache().await
        });
    });
}

/// The callbacks the tray menu and the settings for shortcuts and updates use.
fn wire_tray_actions(bridge: &Bridge<'_>, ctx: &Ctx) {
    let c = ctx.clone();
    bridge.on_toggle_tun(move |enabled| {
        act(
            &c,
            "Changing the TUN mode…",
            Sink::Banner,
            move |s| async move {
                let mut svc = s.lock().await;
                let mut prefs = svc.user_state().preferences.clone();
                prefs.tun.enabled = enabled;
                svc.save_preferences(prefs).await
            },
        );
    });

    let c = ctx.clone();
    bridge.on_set_send_device_id(move |enabled| {
        act(
            &c,
            "Saving the setting…",
            Sink::Settings,
            move |s| async move {
                let mut svc = s.lock().await;
                let mut prefs = svc.user_state().preferences.clone();
                prefs.send_device_id = enabled;
                let saved = svc.save_preferences(prefs).await;
                // Push back what is stored, saved or not, so the switch shows it.
                let shown = svc.user_state().preferences.send_device_id;
                super::runtime::post(move |ui| ui.push_send_device_id(shown));
                saved
            },
        );
    });

    let c = ctx.clone();
    bridge.on_save_hotkeys(move |model: HotkeyModel| {
        let input = hotkeys::bindings_from_model(&model);
        act(
            &c,
            "Saving the shortcuts…",
            Sink::Settings,
            move |s| async move {
                let bindings = hotkeys::validate(&input)?;
                let mut svc = s.lock().await;
                let mut prefs = svc.user_state().preferences.clone();
                prefs.hotkeys = bindings;
                svc.save_preferences(prefs).await
            },
        );
    });

    let c = ctx.clone();
    bridge.on_set_check_updates(move |enabled| {
        act(
            &c,
            "Saving the setting…",
            Sink::Settings,
            move |s| async move {
                let mut svc = s.lock().await;
                let mut prefs = svc.user_state().preferences.clone();
                prefs.check_updates = Some(enabled);
                let saved = svc.save_preferences(prefs).await;
                // Push back what is stored, saved or not, so the switch shows it.
                let shown = svc.user_state().preferences.check_updates.unwrap_or(true);
                super::runtime::post(move |ui| ui.push_check_updates(shown));
                saved
            },
        );
    });

    let c = ctx.clone();
    bridge.on_check_for_updates(move || spawn_update_check(&c, true));
    let c = ctx.clone();
    bridge.on_install_update(move || super::runtime::spawn_install_update(&c));
}

fn wire_shell(bridge: &Bridge<'_>, ctx: &Ctx, window: slint::Weak<AppWindow>) {
    bridge.on_copy_logs(|| with_ui(|ui| ui.copy_logs()));
    bridge.on_copy_proxy_env(|| with_ui(|ui| ui.copy_proxy_env()));
    bridge.on_clear_logs(|| with_ui(|ui| ui.clear_logs()));
    bridge.on_dismiss_banner(|| with_ui(|ui| ui.dismiss_banner()));

    let shown = window.clone();
    bridge.on_show_window(move || super::runtime::show_window(&shown));
    let hidden = window.clone();
    bridge.on_hide_window(move || super::runtime::hide_window(&hidden));

    let c = ctx.clone();
    bridge.on_quit(move || request_quit(&c));
}

#[cfg(test)]
mod tests {
    use super::open_link;

    #[test]
    fn only_web_links_are_refused_or_accepted_before_anything_starts() {
        for refused in [
            "file:///C:/Windows/system32/calc.exe",
            "javascript:alert(1)",
            "ms-settings:network",
            "not a link",
            "",
        ] {
            assert!(open_link(refused).is_err(), "{refused} must be refused");
        }
    }
}
