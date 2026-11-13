//! The window, the tray, the Slint event loop, and the bridge between the
//! tokio runtime and the Slint thread (ARCHITECTURE §6.4, §7).
//!
//! Threading model. The tokio runtime runs on its own worker threads and
//! owns every service call. The Slint event loop runs on the main thread,
//! and only that thread touches a `Bridge`, a window or a tray. A task that
//! has a result for the UI calls [`post`], which runs a closure on the Slint
//! thread. The UI-side state lives in a thread-local [`UiState`], so the
//! closures posted to the event loop can reach it without being `Send`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::error::Error;
use std::time::Duration;

use crate::{AppSnapshot, SharedAppService};
use slint::{CloseRequestResponse, ComponentHandle, Timer, TimerMode};
use starpod_core::{SubscriptionMode, timestamp};
use starpod_subscription::RefreshSchedule;
use starpod_ui::{AppTray, AppWindow, Bridge};
use tokio::sync::watch;
use tracing::{info, warn};

use super::callbacks;
use super::mapping::{
    Banner, DEFAULT_REFRESH_MINUTES, ViewState, apply_snapshot, log_items, log_text, notice_banner,
};

/// How often the engine's state is polled.
const ENGINE_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often engine-managed subscriptions are checked for a refresh (§4.6).
const REASSERT_INTERVAL: Duration = Duration::from_secs(30);
/// How often the auto-refresh schedule is checked.
const AUTO_REFRESH_TICK: Duration = Duration::from_secs(60);
/// How often the filter controls are read.
const FILTER_POLL_INTERVAL: Duration = Duration::from_millis(300);
/// How long quitting waits for the engine to stop.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// What a task needs to reach the service and the runtime. It is cheap to
/// clone and is used only on the Slint thread and in tasks it spawns.
#[derive(Clone)]
pub struct Ctx {
    /// The tokio runtime to spawn on.
    pub runtime: tokio::runtime::Handle,
    /// The application service.
    pub service: SharedAppService,
}

/// Where an action's error is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sink {
    /// The shell banner.
    Banner,
    /// The raw-rules and rule-editor error line.
    Rules,
    /// The settings error line.
    Settings,
}

/// Everything the Slint thread owns: the windows, the view state, and what
/// was last pushed.
pub struct UiState {
    window: AppWindow,
    tray: Option<AppTray>,
    view: ViewState,
    last: Option<AppSnapshot>,
    notice_seen: Option<Option<(crate::NoticeKind, String)>>,
    banner: Option<Banner>,
    busy: u32,
    busy_message: String,
    clipboard: Option<arboard::Clipboard>,
}

thread_local! {
    static UI: RefCell<Option<UiState>> = const { RefCell::new(None) };
}

/// Run `update` on the Slint thread, against the UI state. Safe to call from
/// any thread; the update runs later, on the event loop.
pub fn post(update: impl FnOnce(&mut UiState) + Send + 'static) {
    if let Err(error) = slint::invoke_from_event_loop(move || with_ui(update)) {
        warn!(%error, "the event loop has stopped; a UI update was dropped");
    }
}

/// Run `update` against the UI state now. Only call this on the Slint thread.
pub fn with_ui(update: impl FnOnce(&mut UiState)) {
    UI.with(|cell| match cell.borrow_mut().as_mut() {
        Some(ui) => update(ui),
        None => warn!("a UI update ran before the window existed"),
    });
}

impl UiState {
    fn new(window: AppWindow, tray: Option<AppTray>) -> Self {
        Self {
            window,
            tray,
            view: ViewState::default(),
            last: None,
            notice_seen: None,
            banner: None,
            busy: 0,
            busy_message: String::new(),
            clipboard: None,
        }
    }

    /// Run `update` on each bridge: the window's, and the tray's when there
    /// is a tray. They are separate instances (tray.slint).
    fn each_bridge(&self, update: impl Fn(&Bridge<'_>)) {
        update(&self.window.global::<Bridge>());
        if let Some(tray) = &self.tray {
            update(&tray.global::<Bridge>());
        }
    }

    /// Read the two filter controls. Returns whether either changed.
    fn sync_filters(&mut self) -> bool {
        let bridge = self.window.global::<Bridge>();
        let node_filter = bridge.get_node_filter().to_string();
        let log_filter = bridge.get_log_level_filter().to_string();
        self.view.set_filters(&node_filter, &log_filter)
    }

    /// Apply a new snapshot from the service.
    pub fn apply(&mut self, snap: AppSnapshot) {
        self.sync_filters();
        self.push_snapshot(&snap);
        self.view.record(&snap);
        self.sync_notice(&snap);
        self.last = Some(snap);
    }

    /// Push the last snapshot again, for example after a filter changed.
    pub fn reapply_last(&self) {
        if let Some(snap) = &self.last {
            self.push_snapshot(snap);
        }
    }

    fn push_snapshot(&self, snap: &AppSnapshot) {
        self.each_bridge(|bridge| apply_snapshot(bridge, snap, &self.view));
    }

    /// A service notice becomes the banner when it changes. Otherwise the
    /// banner is left alone, so a dismissed notice stays dismissed.
    fn sync_notice(&mut self, snap: &AppSnapshot) {
        if self.notice_seen.as_ref() == Some(&snap.notice) {
            return;
        }
        self.notice_seen = Some(snap.notice.clone());
        self.banner = snap.notice.as_ref().map(notice_banner);
        self.push_banner();
    }

    fn show_banner(&mut self, banner: Banner) {
        self.banner = Some(banner);
        self.push_banner();
    }

    pub fn dismiss_banner(&mut self) {
        self.banner = None;
        self.push_banner();
    }

    fn push_banner(&self) {
        let (kind, message) = self
            .banner
            .as_ref()
            .map_or(("info", String::new()), |banner| {
                (banner.kind, banner.message.clone())
            });
        self.each_bridge(|bridge| {
            bridge.set_banner_kind(kind.into());
            bridge.set_banner_message(message.as_str().into());
        });
    }

    /// An action started. The busy state holds until every action ends.
    pub fn begin_busy(&mut self, message: &str) {
        self.busy += 1;
        self.busy_message = message.to_string();
        self.push_busy();
    }

    pub fn end_busy(&mut self) {
        self.busy = self.busy.saturating_sub(1);
        self.push_busy();
    }

    fn push_busy(&self) {
        let busy = self.busy > 0;
        let message = if busy {
            self.busy_message.clone()
        } else {
            String::new()
        };
        self.each_bridge(|bridge| {
            bridge.set_busy(busy);
            bridge.set_busy_message(message.as_str().into());
        });
    }

    /// Show an action's error where its sink says.
    pub fn report(&mut self, sink: Sink, message: String) {
        match sink {
            Sink::Banner => self.show_banner(Banner::error(message)),
            Sink::Rules => {
                self.each_bridge(|bridge| bridge.set_rules_error(message.as_str().into()))
            }
            Sink::Settings => {
                self.each_bridge(|bridge| bridge.set_settings_error(message.as_str().into()));
            }
        }
    }

    /// An action succeeded. Its error line is cleared; a banner is left as
    /// it is.
    pub fn clear_error(&mut self, sink: Sink) {
        match sink {
            Sink::Banner => {}
            Sink::Rules => self.each_bridge(|bridge| bridge.set_rules_error("".into())),
            Sink::Settings => self.each_bridge(|bridge| bridge.set_settings_error("".into())),
        }
    }

    /// Hide the log lines on screen. The service keeps them.
    pub fn clear_logs(&mut self) {
        self.view.clear_logs();
        self.reapply_last();
    }

    /// Put the visible log lines on the clipboard.
    pub fn copy_logs(&mut self) {
        let text = match self.last.as_ref() {
            Some(snap) => log_text(&log_items(snap, &self.view)),
            None => {
                self.show_banner(Banner::info("there are no log lines to copy yet"));
                return;
            }
        };
        match self.copy_text(text) {
            Ok(()) => self.show_banner(Banner::success(
                "the visible log lines are on the clipboard",
            )),
            Err(error) => {
                warn!(%error, "the clipboard refused the log lines");
                self.show_banner(Banner::error(format!("could not copy the logs: {error}")));
            }
        }
    }

    fn copy_text(&mut self, text: String) -> Result<(), arboard::Error> {
        // The clipboard is kept alive: on some platforms its contents go away
        // with the handle.
        if self.clipboard.is_none() {
            self.clipboard = Some(arboard::Clipboard::new()?);
        }
        if let Some(clipboard) = self.clipboard.as_mut() {
            clipboard.set_text(text)?;
        }
        Ok(())
    }

    fn show_tray_availability(&self, available: bool) {
        self.each_bridge(|bridge| bridge.set_tray_available(available));
    }
}

/// Show the window, if it still exists.
pub fn show_window(window: &slint::Weak<AppWindow>) {
    match window.upgrade() {
        Some(window) => {
            if let Err(error) = window.show() {
                warn!(%error, "could not show the window");
            }
        }
        None => warn!("the window no longer exists"),
    }
}

/// Hide the window, if it still exists. The engine keeps running.
pub fn hide_window(window: &slint::Weak<AppWindow>) {
    match window.upgrade() {
        Some(window) => {
            if let Err(error) = window.hide() {
                warn!(%error, "could not hide the window");
            }
        }
        None => warn!("the window no longer exists"),
    }
}

/// Stop the engine gracefully, then end the event loop. Used by the tray's
/// Quit, the Quit button, and closing the window when there is no tray.
pub fn request_quit(ctx: &Ctx) {
    let service = ctx.service.clone();
    ctx.runtime.spawn(async move {
        info!("quitting: stopping the engine");
        let stop = async { service.lock().await.disconnect().await };
        match tokio::time::timeout(SHUTDOWN_TIMEOUT, stop).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(%error, "the engine did not stop cleanly"),
            Err(_) => warn!("timed out waiting for the engine to stop"),
        }
        if let Err(error) = slint::quit_event_loop() {
            warn!(%error, "could not end the event loop");
        }
    });
}

/// Build the tray. It is optional: a Linux desktop without a StatusNotifier
/// host cannot show one, and then the app quits when its window is closed.
fn create_tray() -> Option<AppTray> {
    match AppTray::new().and_then(|tray| tray.show().map(|()| tray)) {
        Ok(tray) => {
            info!("tray created");
            Some(tray)
        }
        Err(error) => {
            warn!(
                %error,
                "the system tray is unavailable; closing the window will stop the engine and quit"
            );
            None
        }
    }
}

/// Follow the service's snapshots and post each one to the Slint thread.
async fn propagate(mut updates: watch::Receiver<AppSnapshot>) {
    let mut snapshot = updates.borrow_and_update().clone();
    loop {
        post(move |ui| ui.apply(snapshot));
        if updates.changed().await.is_err() {
            break;
        }
        snapshot = updates.borrow_and_update().clone();
    }
}

/// Check the engine-managed subscriptions every [`REASSERT_INTERVAL`]. The
/// service does nothing on a tick when the engine is not running or no
/// subscription is engine-managed.
async fn watch_engine_subscriptions(service: SharedAppService) {
    let mut tick = tokio::time::interval(REASSERT_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        service.lock().await.check_engine_subscriptions().await;
    }
}

/// Read the engine's state every [`ENGINE_POLL_INTERVAL`].
async fn poll_engine(service: SharedAppService) {
    let mut tick = tokio::time::interval(ENGINE_POLL_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        service.lock().await.refresh_engine_status().await;
    }
}

fn unix_now() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// The app-managed subscriptions that are due for an automatic refresh.
///
/// A subscription is due when its interval has passed since its last good
/// fetch, and it was not already tried in the same interval, so a failing
/// provider is retried once per interval rather than once per tick. A
/// subscription that was never fetched counts as due. An interval of `0`
/// turns automatic refresh off.
pub fn due_subscription_ids(
    snap: &AppSnapshot,
    now: i64,
    last_attempt: &HashMap<String, i64>,
) -> Vec<String> {
    let minutes = snap
        .preferences
        .auto_refresh_minutes
        .unwrap_or(DEFAULT_REFRESH_MINUTES);
    if minutes == 0 {
        return Vec::new();
    }
    let interval_secs = u64::from(minutes) * 60;
    snap.subscriptions
        .iter()
        .filter(|subscription| subscription.mode == SubscriptionMode::AppManaged)
        .filter(|subscription| {
            let last_success = subscription
                .last_updated
                .as_deref()
                .and_then(timestamp::parse);
            let schedule = RefreshSchedule::new(interval_secs, last_success);
            schedule.is_due(now)
                && last_attempt
                    .get(&subscription.id)
                    .is_none_or(|attempt| now - attempt >= interval_secs as i64)
        })
        .map(|subscription| subscription.id.clone())
        .collect()
}

/// Refresh due subscriptions every [`AUTO_REFRESH_TICK`]. Each refresh takes
/// the service lock on its own, so a user action is never queued behind a
/// whole batch.
async fn auto_refresh(service: SharedAppService) {
    let mut tick = tokio::time::interval(AUTO_REFRESH_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_attempt: HashMap<String, i64> = HashMap::new();
    loop {
        tick.tick().await;
        let now = unix_now();
        let snapshot = service.lock().await.snapshot();
        for id in due_subscription_ids(&snapshot, now, &last_attempt) {
            last_attempt.insert(id.clone(), now);
            let outcome = service.lock().await.refresh_subscription(&id).await;
            if let Err(error) = outcome {
                warn!(%error, "an automatic subscription refresh failed");
            }
        }
    }
}

/// Build the window and the tray, wire every callback, start the background
/// loops, and run the Slint event loop until the app quits.
///
/// The service must already be loaded. The tokio runtime behind `runtime`
/// must outlive this call.
pub fn run(
    runtime: tokio::runtime::Handle,
    service: SharedAppService,
) -> Result<(), Box<dyn Error>> {
    let window = AppWindow::new()?;
    info!("window created");
    let tray = create_tray();
    let tray_available = tray.is_some();
    let weak = window.as_weak();
    let ctx = Ctx {
        runtime: runtime.clone(),
        service: service.clone(),
    };

    callbacks::wire(&window.global::<Bridge>(), &ctx, weak.clone());
    if let Some(tray) = &tray {
        callbacks::wire(&tray.global::<Bridge>(), &ctx, weak.clone());
    }
    let close_ctx = ctx.clone();
    window.window().on_close_requested(move || {
        if !tray_available {
            request_quit(&close_ctx);
        }
        CloseRequestResponse::HideWindow
    });

    // The window is shown before it moves into the UI state: the state is the
    // only owner the Slint thread needs from here on.
    window.show()?;
    info!("window shown");
    let ui = UiState::new(window, tray);
    ui.show_tray_availability(tray_available);
    UI.with(|cell| *cell.borrow_mut() = Some(ui));

    runtime.spawn({
        let service = service.clone();
        async move {
            let updates = service.lock().await.subscribe();
            propagate(updates).await;
        }
    });
    runtime.spawn(poll_engine(service.clone()));
    runtime.spawn(watch_engine_subscriptions(service.clone()));
    runtime.spawn(auto_refresh(service));

    let filters = Timer::default();
    filters.start(TimerMode::Repeated, FILTER_POLL_INTERVAL, || {
        with_ui(|ui| {
            if ui.sync_filters() {
                ui.reapply_last();
            }
        });
    });

    // Queued before the loop starts, so it runs only once the loop is
    // dispatching events.
    if let Err(error) = slint::invoke_from_event_loop(|| info!("event loop entered")) {
        warn!(%error, "could not queue the event-loop marker");
    }
    let outcome = slint::run_event_loop_until_quit();
    match &outcome {
        Ok(()) => info!("event loop exited: quit requested"),
        Err(error) => warn!(%error, "event loop exited with an error"),
    }
    drop(filters);
    let state = UI.with(|cell| cell.borrow_mut().take());
    drop(state);
    outcome?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SubscriptionStatus, SubscriptionView};

    fn subscription(id: &str, mode: SubscriptionMode, last: Option<&str>) -> SubscriptionView {
        SubscriptionView {
            id: id.to_string(),
            label: id.to_string(),
            mode,
            status: SubscriptionStatus::Current,
            last_updated: last.map(str::to_string),
            error: None,
            node_count: 0,
            group_count: 0,
            quota: None,
            has_auth_header: false,
        }
    }

    fn snapshot(minutes: Option<u32>, subscriptions: Vec<SubscriptionView>) -> AppSnapshot {
        let mut snap = AppSnapshot {
            subscriptions,
            ..AppSnapshot::default()
        };
        snap.preferences.auto_refresh_minutes = minutes;
        snap
    }

    const NOW: i64 = 1_800_000_000;

    fn stamp(unix: i64) -> String {
        timestamp::format(unix).unwrap()
    }

    #[test]
    fn a_fresh_subscription_is_not_refreshed_yet() {
        let snap = snapshot(
            Some(60),
            vec![subscription(
                "a",
                SubscriptionMode::AppManaged,
                Some(&stamp(NOW - 60)),
            )],
        );
        assert!(due_subscription_ids(&snap, NOW, &HashMap::new()).is_empty());
    }

    #[test]
    fn a_stale_subscription_is_due_and_engine_managed_ones_are_not() {
        let snap = snapshot(
            Some(60),
            vec![
                subscription(
                    "stale",
                    SubscriptionMode::AppManaged,
                    Some(&stamp(NOW - 7200)),
                ),
                subscription(
                    "engine",
                    SubscriptionMode::EngineManaged,
                    Some(&stamp(NOW - 7200)),
                ),
            ],
        );
        assert_eq!(
            due_subscription_ids(&snap, NOW, &HashMap::new()),
            vec!["stale".to_string()]
        );
    }

    #[test]
    fn a_never_fetched_subscription_is_due() {
        let snap = snapshot(
            None,
            vec![subscription("new", SubscriptionMode::AppManaged, None)],
        );
        assert_eq!(
            due_subscription_ids(&snap, NOW, &HashMap::new()),
            vec!["new".to_string()]
        );
    }

    #[test]
    fn a_failing_subscription_is_retried_once_per_interval() {
        let snap = snapshot(
            Some(60),
            vec![subscription("bad", SubscriptionMode::AppManaged, None)],
        );
        let mut attempts = HashMap::new();
        attempts.insert("bad".to_string(), NOW - 30);
        assert!(due_subscription_ids(&snap, NOW, &attempts).is_empty());
        attempts.insert("bad".to_string(), NOW - 3600);
        assert_eq!(
            due_subscription_ids(&snap, NOW, &attempts),
            vec!["bad".to_string()]
        );
    }

    #[test]
    fn a_zero_interval_turns_automatic_refresh_off() {
        let snap = snapshot(
            Some(0),
            vec![subscription("a", SubscriptionMode::AppManaged, None)],
        );
        assert!(due_subscription_ids(&snap, NOW, &HashMap::new()).is_empty());
    }
}
