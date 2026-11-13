//! Every `Bridge` callback, mapped to a service call (ARCHITECTURE §6.3).
//!
//! A callback does one of two things. Either it runs an action on the
//! service, through [`act`], which shows busy and reports errors. Or it
//! changes view state only (filters, banners, the clipboard), which runs
//! directly on the Slint thread. The same wiring is applied to the window's
//! Bridge and to the tray's, since they are separate instances.

use std::future::Future;

use crate::{AppError, AppService, SharedAppService};
use starpod_core::RuleSpec;
use starpod_ui::{AppWindow, Bridge, SettingsModel};
use tracing::warn;

use super::mapping::{
    SettingsInput, build_preferences, moved_position, parse_proxy_mode, parse_rule_line,
    parse_subscription_mode, rule_line,
};
use super::runtime::{Ctx, Sink, request_quit, with_ui};

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

/// Attach every callback to one `Bridge`.
pub fn wire(bridge: &Bridge<'_>, ctx: &Ctx, window: slint::Weak<AppWindow>) {
    wire_engine(bridge, ctx);
    wire_servers(bridge, ctx);
    wire_rules(bridge, ctx);
    wire_settings(bridge, ctx);
    wire_shell(bridge, ctx, window);
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
    bridge.on_test_latency(move || {
        act(&c, "Testing the nodes…", Sink::Banner, |s| async move {
            s.lock().await.test_latency().await
        });
    });
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
    Ok(())
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
    bridge.on_clear_cache(move || {
        act(&c, "Clearing the cache…", Sink::Banner, |s| async move {
            s.lock().await.clear_cache().await
        });
    });
}

fn wire_shell(bridge: &Bridge<'_>, ctx: &Ctx, window: slint::Weak<AppWindow>) {
    bridge.on_copy_logs(|| with_ui(|ui| ui.copy_logs()));
    bridge.on_clear_logs(|| with_ui(|ui| ui.clear_logs()));
    bridge.on_dismiss_banner(|| with_ui(|ui| ui.dismiss_banner()));

    let shown = window.clone();
    bridge.on_show_window(move || super::runtime::show_window(&shown));
    let hidden = window.clone();
    bridge.on_hide_window(move || super::runtime::hide_window(&hidden));

    let c = ctx.clone();
    bridge.on_quit(move || request_quit(&c));
}
