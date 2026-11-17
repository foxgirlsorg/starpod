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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crate::deeplink::{self, InstallRequest};
use crate::instance::Forwarded;
use crate::update::{self, Release};
use crate::{AppSnapshot, SharedAppService};
use slint::{CloseRequestResponse, ComponentHandle, Timer, TimerMode};
use starpod_core::config::{DEFAULT_MIXED_PORT, DEFAULT_SOCKS_PORT};
use starpod_core::{SubscriptionMode, UserPreferences, timestamp};
use starpod_subscription::RefreshSchedule;
use starpod_ui::{AppTray, AppWindow, Bridge, Design};
use tokio::sync::watch;
use tracing::{info, warn};

use super::callbacks;
use super::demo;
use super::emoji::runs_model;
use super::hotkeys::{self, Action};
use super::mapping::{Banner, ViewState, apply_snapshot, log_items, log_text, notice_banner};
use super::tray;

/// How often the engine's state is polled.
const ENGINE_POLL_INTERVAL: Duration = Duration::from_secs(2);
/// How often engine-managed subscriptions are checked for a refresh (§4.6).
const REASSERT_INTERVAL: Duration = Duration::from_secs(30);
/// How often the auto-refresh schedule is checked.
const AUTO_REFRESH_TICK: Duration = Duration::from_secs(60);
/// How often the filter controls are read.
const FILTER_POLL_INTERVAL: Duration = Duration::from_millis(300);
/// How often the global shortcut presses are read.
const SHORTCUT_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How often the engine's connections are read while the Connections page is open.
const CONNECTIONS_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// The Bridge's `active-page` index of the Connections page.
pub const CONNECTIONS_PAGE: i32 = 6;
/// Whether the Connections page is the active page. Written on the Slint thread
/// by the filter timer, read by the poll, so the poll does nothing while the page
/// is closed.
static CONNECTIONS_VISIBLE: AtomicBool = AtomicBool::new(false);
/// Whether the Connections page is paused. Read by the poll the same way.
static CONNECTIONS_PAUSED: AtomicBool = AtomicBool::new(false);
/// Whether closing the window hides Starpod to the tray. Written on the Slint
/// thread from the saved preferences, read by the window's close handler.
static CLOSE_TO_TRAY: AtomicBool = AtomicBool::new(true);
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
    /// The install dialog's error line.
    Install,
}

/// What this launch was given: a deep link from the command line, and the
/// requests that later launches forward to it.
pub struct Launch {
    /// The deep link this launch was started with, if any. It is queued until
    /// the window exists.
    pub link: Option<String>,
    /// Forwarded requests. `None` when this launch runs without the
    /// single-instance channel.
    pub forwards: Option<Receiver<Forwarded>>,
    /// Whether the window stays hidden at start: a login start with silent
    /// start on. It is ignored when there is no tray, since then the window
    /// is the only way to reach the app.
    pub start_hidden: bool,
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
    #[cfg(not(target_os = "android"))]
    clipboard: Option<arboard::Clipboard>,
    /// `STARPOD_UI_DEMO=1`: fill the window with the demo data after the first push.
    demo_requested: bool,
    /// Set once the demo data is in the Bridge. Later snapshots are not pushed.
    demo_applied: bool,
    /// A deep link waiting for the user to confirm it. The address stays here
    /// and never reaches the Bridge.
    pending_install: Option<InstallRequest>,
    /// The global shortcuts registered with the operating system.
    shortcuts: hotkeys::Shortcuts,
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
            #[cfg(not(target_os = "android"))]
            clipboard: None,
            demo_requested: demo::requested(),
            demo_applied: false,
            pending_install: None,
            shortcuts: hotkeys::Shortcuts::default(),
        }
    }

    /// A request from a later launch, or the link this launch started with.
    /// The window is shown first, so the confirmation is seen.
    pub fn forwarded(&mut self, forward: Forwarded) {
        if let Err(error) = self.window.show() {
            warn!(%error, "could not show the window");
        }
        match forward {
            Forwarded::Show => {}
            Forwarded::Link(text) => match deeplink::parse(&text) {
                Ok(request) => self.ask_install(request),
                Err(error) => {
                    warn!(%error, "a deep link was refused");
                    self.show_banner(Banner::error(crate::i18n::fmt(
                        "the link was not used: {}",
                        &[&error.to_string()],
                    )));
                }
            },
        }
    }

    /// Show the confirmation for a deep link. Only the host is shown.
    fn ask_install(&mut self, request: InstallRequest) {
        let host = request.host();
        let name = request.name.clone().unwrap_or_default();
        self.pending_install = Some(request);
        self.each_bridge(|bridge| {
            bridge.set_install_host(host.as_str().into());
            bridge.set_install_name(name.as_str().into());
            bridge.set_install_error("".into());
            bridge.set_install_pending(true);
        });
    }

    /// The request waiting for confirmation, if any.
    pub fn pending_install(&self) -> Option<InstallRequest> {
        self.pending_install.clone()
    }

    /// Forget the pending request and close the confirmation.
    pub fn end_install(&mut self) {
        self.pending_install = None;
        self.each_bridge(|bridge| {
            bridge.set_install_pending(false);
            bridge.set_install_error("".into());
            bridge.set_install_host("".into());
            bridge.set_install_name("".into());
        });
    }

    /// Run `update` on each bridge: the window's, and the tray's when there
    /// is a tray. They are separate instances (tray.slint).
    fn each_bridge(&self, update: impl Fn(&Bridge<'_>)) {
        update(&self.window.global::<Bridge>());
        if let Some(tray) = &self.tray {
            update(&tray.global::<Bridge>());
        }
    }

    /// Read the filter and list controls. Returns whether any changed.
    fn sync_filters(&mut self) -> bool {
        let bridge = self.window.global::<Bridge>();
        let node_filter = bridge.get_node_filter().to_string();
        let log_filter = bridge.get_log_level_filter().to_string();
        let connections_filter = bridge.get_connections_filter().to_string();
        let connections_sort = bridge.get_connections_sort();
        let proxies_sort = bridge.get_proxies_sort();
        let hide_unavailable = bridge.get_hide_unavailable();
        let lists = self.view.set_list_controls(
            &connections_filter,
            connections_sort,
            proxies_sort,
            hide_unavailable,
        );
        let filters = self.view.set_filters(&node_filter, &log_filter);
        lists || filters
    }

    /// Take the Proxies list settings from the saved preferences the first time
    /// a snapshot arrives, and show them on the page.
    fn seed_list_prefs(&mut self, snap: &AppSnapshot) {
        if self.view.load_list_prefs(&snap.preferences) {
            let sort = self.view.proxies_sort;
            let hide = self.view.hide_unavailable;
            self.each_bridge(|bridge| {
                bridge.set_proxies_sort(sort);
                bridge.set_hide_unavailable(hide);
            });
        }
    }

    /// The Proxies list settings the page shows that the saved preferences do
    /// not hold. `None` when they agree, or before the saved settings are read.
    pub fn list_prefs_to_save(&self) -> Option<(u8, bool)> {
        if !self.view.list_prefs_loaded {
            return None;
        }
        let saved = &self.last.as_ref()?.preferences;
        let sort = u8::try_from(self.view.proxies_sort).ok()?;
        let hide = self.view.hide_unavailable;
        (sort != saved.proxies_sort || hide != saved.hide_unavailable).then_some((sort, hide))
    }

    /// Tell the connections poll whether the Connections page is open and
    /// whether it is paused.
    pub fn sync_page_flags(&self) {
        let bridge = self.window.global::<Bridge>();
        CONNECTIONS_VISIBLE.store(
            bridge.get_active_page() == CONNECTIONS_PAGE,
            Ordering::Relaxed,
        );
        CONNECTIONS_PAUSED.store(bridge.get_connections_paused(), Ordering::Relaxed);
    }

    /// Flip one Proxies group between collapsed and expanded. Returns the
    /// groups now open, for saving.
    pub fn toggle_group(&mut self, group: &str) -> std::collections::BTreeSet<String> {
        self.view.toggle_group(group);
        self.reapply_last();
        self.view.expanded_groups.clone()
    }

    /// Apply a new snapshot from the service.
    pub fn apply(&mut self, snap: AppSnapshot) {
        self.seed_list_prefs(&snap);
        self.sync_filters();
        self.push_snapshot(&snap);
        if self.demo_requested && !self.demo_applied {
            // After the first state push, the demo data replaces what the
            // window shows; the tray's Bridge is left as it is.
            demo::apply(&self.window.global::<Bridge>());
            self.demo_applied = true;
            info!("STARPOD_UI_DEMO: the demo data is in the window");
        }
        self.view.record(&snap);
        self.sync_notice(&snap);
        self.push_preferences(&snap.preferences);
        self.last = Some(snap);
    }

    /// Push the last snapshot again, for example after a filter changed.
    pub fn reapply_last(&self) {
        if let Some(snap) = &self.last {
            self.push_snapshot(snap);
        }
    }

    fn push_snapshot(&self, snap: &AppSnapshot) {
        if self.demo_applied {
            return;
        }
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
            bridge.set_banner_runs(runs_model(&message));
            bridge.set_banner_message(message.as_str().into());
        });
    }

    /// An action started. The busy state holds until every action ends.
    pub fn begin_busy(&mut self, message: &str) {
        self.busy += 1;
        self.busy_message = message.to_string();
        self.push_busy();
    }

    /// Change what the busy indicator says, without starting another task.
    pub fn set_busy_message(&mut self, message: &str) {
        if self.busy > 0 {
            self.busy_message = message.to_string();
            self.push_busy();
        }
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
            bridge.set_busy_message_runs(runs_model(&message));
            bridge.set_busy_message(message.as_str().into());
        });
    }

    /// Show an action's error where its sink says.
    pub fn report(&mut self, sink: Sink, message: String) {
        match sink {
            Sink::Banner => self.show_banner(Banner::error(message)),
            Sink::Rules => self.each_bridge(|bridge| {
                bridge.set_rules_error_runs(runs_model(&message));
                bridge.set_rules_error(message.as_str().into());
            }),
            Sink::Settings => {
                self.each_bridge(|bridge| {
                    bridge.set_settings_error_runs(runs_model(&message));
                    bridge.set_settings_error(message.as_str().into());
                });
            }
            Sink::Install => {
                self.each_bridge(|bridge| bridge.set_install_error(message.as_str().into()));
            }
        }
    }

    /// An action succeeded. Its error line is cleared; a banner is left as
    /// it is.
    pub fn clear_error(&mut self, sink: Sink) {
        match sink {
            Sink::Banner => {}
            Sink::Rules => self.each_bridge(|bridge| {
                bridge.set_rules_error_runs(runs_model(""));
                bridge.set_rules_error("".into());
            }),
            Sink::Settings => self.each_bridge(|bridge| {
                bridge.set_settings_error_runs(runs_model(""));
                bridge.set_settings_error("".into());
            }),
            // A confirmed install that succeeded closes its dialog.
            Sink::Install => self.end_install(),
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
                self.show_banner(Banner::error(crate::i18n::fmt(
                    "could not copy the logs: {}",
                    &[&error.to_string()],
                )));
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    fn copy_text(&mut self, text: String) -> Result<(), String> {
        // The clipboard is kept alive: on some platforms its contents go away
        // with the handle.
        if self.clipboard.is_none() {
            self.clipboard = Some(arboard::Clipboard::new().map_err(|error| error.to_string())?);
        }
        if let Some(clipboard) = self.clipboard.as_mut() {
            clipboard
                .set_text(text)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    /// Android has no clipboard access from native code without the Java
    /// side, so copying says so instead.
    #[cfg(target_os = "android")]
    fn copy_text(&mut self, _text: String) -> Result<(), String> {
        Err("copying is not available on Android yet".to_string())
    }

    /// Show the profile editor with a profile's text.
    pub fn open_profile_editor(&mut self, id: &str, title: &str, text: &str, local: bool) {
        let bridge = self.window.global::<Bridge>();
        bridge.set_editor_id(id.into());
        bridge.set_editor_title(title.into());
        bridge.set_editor_text(text.into());
        bridge.set_editor_local(local);
        bridge.set_editor_error("".into());
        bridge.set_editor_open(true);
    }

    /// Close the editor after a save it accepted, or show why it refused.
    pub fn finish_profile_edit(&mut self, error: Option<String>) {
        let bridge = self.window.global::<Bridge>();
        match error {
            None => bridge.set_editor_open(false),
            Some(message) => bridge.set_editor_error(message.into()),
        }
    }

    /// Put the proxy command for the visible ports on the clipboard.
    pub fn copy_proxy_env(&mut self) {
        let prefs = self
            .last
            .as_ref()
            .map(|snap| snap.preferences.clone())
            .unwrap_or_default();
        let http_port = prefs.mixed_port.unwrap_or(DEFAULT_MIXED_PORT);
        let socks_port = prefs.socks_port.unwrap_or(DEFAULT_SOCKS_PORT);
        let text = tray::proxy_env_command(cfg!(windows), http_port, socks_port);
        match self.copy_text(text) {
            Ok(()) => self.show_banner(Banner::success("the proxy command is on the clipboard")),
            Err(error) => {
                warn!(%error, "the clipboard refused the proxy command");
                self.show_banner(Banner::error(format!(
                    "could not copy the proxy command: {error}"
                )));
            }
        }
    }

    /// Push the preferences the Bridge shows beside the snapshot: the update
    /// setting, the shortcuts and the system proxy. The shortcuts are
    /// registered when they changed.
    fn push_preferences(&mut self, prefs: &UserPreferences) {
        let check_updates = prefs.check_updates.unwrap_or(true);
        let shortcuts = hotkeys::model_from(&prefs.hotkeys);
        CLOSE_TO_TRAY.store(prefs.close_to_tray, Ordering::Relaxed);
        // The Slint strings follow the same language as the Rust-side ones.
        // Both the window and the tray hold the translations, so both switch.
        let language = crate::i18n::effective(&prefs.language);
        if let Err(error) = slint::select_bundled_translation(language.tag()) {
            warn!(?error, "the interface language could not be selected");
        }
        let accent = super::mapping::accent_slint_color(prefs.effective_accent_color());
        self.window.global::<Design>().set_primary(accent);
        if let Some(tray) = &self.tray {
            tray.global::<Design>().set_primary(accent);
        }
        self.each_bridge(|bridge| {
            bridge.set_check_updates(check_updates);
            bridge.set_hotkeys(shortcuts.clone());
        });
        if let Some(message) = self.shortcuts.apply(&prefs.hotkeys) {
            warn!(%message, "the global shortcuts could not all be registered");
            self.report(Sink::Settings, message);
        }
    }

    /// Run the shortcuts pressed since the last poll.
    pub fn run_pressed_shortcuts(&mut self) {
        for action in self.shortcuts.drain_presses() {
            self.run_shortcut(action);
        }
    }

    /// Switch the routing mode through the Bridge callback the menu uses.
    fn set_mode_from_shortcut(&self, mode: &str) {
        self.window.global::<Bridge>().invoke_set_mode(mode.into());
    }

    /// Run one shortcut through the Bridge callback its button uses, so the
    /// shortcut and the button cannot disagree. The callbacks start their work
    /// in the background, so none of them re-enters this state.
    fn run_shortcut(&mut self, action: Action) {
        match action {
            Action::ShowWindow => {
                let result = if self.window.window().is_visible() {
                    self.window.hide()
                } else {
                    self.window.show()
                };
                if let Err(error) = result {
                    warn!(%error, "could not show or hide the window");
                }
            }
            Action::ToggleConnect => {
                let bridge = self.window.global::<Bridge>();
                match bridge.get_engine_phase().as_str() {
                    "running" => bridge.invoke_disconnect(),
                    "stopped" | "failed" => bridge.invoke_connect(),
                    _ => {}
                }
            }
            Action::ToggleSystemProxy => {
                let bridge = self.window.global::<Bridge>();
                bridge.invoke_toggle_system_proxy(!bridge.get_system_proxy_active());
            }
            Action::ModeRule => self.set_mode_from_shortcut("rule"),
            Action::ModeGlobal => self.set_mode_from_shortcut("global"),
            Action::ModeDirect => self.set_mode_from_shortcut("direct"),
        }
    }

    /// Show the outcome of an update check: the update fields on each Bridge,
    /// and a banner when the user asked for the check. A failed startup check
    /// says nothing, since the user did not ask for it.
    fn finish_update_check(&mut self, result: Result<Option<Release>, String>, manual: bool) {
        match result {
            Ok(Some(release)) => {
                let version = release.version.clone();
                let url = release.url;
                self.each_bridge(|bridge| {
                    bridge.set_update_available(true);
                    bridge.set_update_version(version.as_str().into());
                    bridge.set_update_url(url.as_str().into());
                });
                if manual {
                    self.show_banner(Banner::info(crate::i18n::fmt(
                        "Starpod {} is available",
                        &[&version.to_string()],
                    )));
                }
            }
            Ok(None) => {
                self.each_bridge(|bridge| {
                    bridge.set_update_available(false);
                    bridge.set_update_version("".into());
                    bridge.set_update_url("".into());
                });
                if manual {
                    self.show_banner(Banner::success(format!(
                        "Starpod {} is the latest version",
                        update::CURRENT_VERSION
                    )));
                }
            }
            Err(message) => {
                warn!(%message, "the update check failed");
                if manual {
                    self.show_banner(Banner::error(format!(
                        "could not check for updates: {message}"
                    )));
                }
            }
        }
    }

    fn show_tray_availability(&self, available: bool) {
        self.each_bridge(|bridge| bridge.set_tray_available(available));
    }

    /// Show the device-identification switch as saved. Called after its
    /// callback saves, so the switch matches the stored value even when the
    /// save was refused and nothing else changed.
    pub fn push_send_device_id(&self, enabled: bool) {
        self.each_bridge(|bridge| bridge.set_send_device_id(enabled));
    }

    /// Show the update-check switch as saved. See [`Self::push_send_device_id`].
    pub fn push_check_updates(&self, enabled: bool) {
        self.each_bridge(|bridge| bridge.set_check_updates(enabled));
    }
}

/// Check GitHub for a newer release. A manual check always runs. The startup
/// check runs only while the user keeps the setting on.
pub fn spawn_update_check(ctx: &Ctx, manual: bool) {
    // On Android a new version comes as a new APK; the app does not update
    // itself.
    if cfg!(target_os = "android") {
        return;
    }
    let service = ctx.service.clone();
    ctx.runtime.spawn(async move {
        if !manual {
            let enabled = service
                .lock()
                .await
                .user_state()
                .preferences
                .check_updates
                .unwrap_or(true);
            if !enabled {
                return;
            }
        }
        let result = match update::fetch_latest_release().await {
            Ok(release) => update::is_newer(&release.version, update::CURRENT_VERSION)
                .map(|newer| newer.then_some(release)),
            Err(error) => Err(error),
        };
        post(move |ui| ui.finish_update_check(result, manual));
    });
}

/// Download and install the newest release, then quit and start it. Runs
/// only when the user asks. A failure changes nothing and says why.
pub fn spawn_install_update(ctx: &Ctx) {
    let ctx = ctx.clone();
    let service = ctx.service.clone();
    ctx.runtime.clone().spawn(async move {
        post(|ui| ui.begin_busy("Downloading the update…"));
        let staging = service.lock().await.paths().data_dir().join("update");
        let result = async {
            let release = update::fetch_latest_release().await?;
            if !update::is_newer(&release.version, update::CURRENT_VERSION)? {
                return Err(format!(
                    "Starpod {} is the latest version",
                    update::CURRENT_VERSION
                ));
            }
            let new = update::download_update(&release, &staging, |percent| {
                post(move |ui| ui.set_busy_message(&format!("Downloading the update… {percent}%")));
            })
            .await?;
            let current = std::env::current_exe()
                .map_err(|error| format!("the running Starpod could not be found: {error}"))?;
            update::replace_binary(&current, &new)?;
            Ok((current, release.version))
        }
        .await;
        match result {
            Ok((current, version)) => {
                info!(%version, "update installed; restarting");
                update::schedule_relaunch(current);
                post(|ui| ui.end_busy());
                request_quit(&ctx);
            }
            Err(message) => {
                warn!(%message, "the update was not installed");
                post(move |ui| {
                    ui.end_busy();
                    ui.show_banner(Banner::error(format!(
                        "could not update Starpod: {message}"
                    )));
                });
            }
        }
    });
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
        let stop = async {
            let mut svc = service.lock().await;
            let result = svc.disconnect().await;
            // Restored even when the engine did not stop cleanly, so the user's
            // system proxy never outlives the app.
            svc.restore_system_proxy().await;
            result
        };
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

/// Read the engine's open connections every [`CONNECTIONS_POLL_INTERVAL`],
/// but only while the Connections page is open and not paused.
async fn poll_connections(service: SharedAppService) {
    let mut tick = tokio::time::interval(CONNECTIONS_POLL_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if !CONNECTIONS_VISIBLE.load(Ordering::Relaxed)
            || CONNECTIONS_PAUSED.load(Ordering::Relaxed)
        {
            continue;
        }
        service.lock().await.refresh_connections().await;
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
    // The user's own interval wins when they chose one; `0` is manual only.
    // Otherwise each subscription uses its provider's update interval, or the
    // default when the provider sent none.
    let user_minutes = snap.preferences.auto_refresh_minutes;
    if user_minutes == Some(0) {
        return Vec::new();
    }
    snap.subscriptions
        .iter()
        .filter(|subscription| subscription.mode == SubscriptionMode::AppManaged)
        .filter(|subscription| {
            let interval_secs = match user_minutes {
                Some(minutes) => u64::from(minutes) * 60,
                None => subscription.interval_secs,
            };
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

/// Pass each request that a later launch forwards to the Slint thread. The
/// thread ends when the listener does.
fn spawn_forwarder(forwards: Receiver<Forwarded>) {
    let spawned = std::thread::Builder::new()
        .name("starpod-forwards".to_string())
        .spawn(move || {
            for forward in forwards {
                post(move |ui| ui.forwarded(forward));
            }
        });
    if let Err(error) = spawned {
        warn!(%error, "could not start the thread for forwarded requests");
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
    launch: Launch,
) -> Result<(), Box<dyn Error>> {
    let window = AppWindow::new()?;
    info!("window created");
    window
        .global::<Bridge>()
        .set_mobile(cfg!(target_os = "android"));
    // Debug hook for tools/ui-shot.sh: STARPOD_UI_PAGE=0..6 opens that page
    // (6 is Connections).
    if let Some(page) = std::env::var("STARPOD_UI_PAGE")
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .filter(|page| (0..=CONNECTIONS_PAGE).contains(page))
    {
        window.global::<Bridge>().set_active_page(page);
        info!(
            page,
            "STARPOD_UI_PAGE: opened the page given in the environment"
        );
    }
    // Debug hook for tools/ui-shot.sh: STARPOD_UI_SETTINGS=0..10 opens that
    // Settings section instead of the section list.
    if let Some(section) = std::env::var("STARPOD_UI_SETTINGS")
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .filter(|section| (0..=10).contains(section))
    {
        window.global::<Bridge>().set_settings_section(section);
    }
    // Debug hook for tools/ui-shot.sh: STARPOD_UI_NAV=1 opens the drawer.
    if std::env::var("STARPOD_UI_NAV").is_ok_and(|value| value.trim() == "1") {
        window.set_nav_open(true);
    }
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
        // Without a tray there is nowhere to hide to, so closing always quits.
        let hide_to_tray = tray_available && CLOSE_TO_TRAY.load(Ordering::Relaxed);
        if !hide_to_tray {
            request_quit(&close_ctx);
        }
        CloseRequestResponse::HideWindow
    });

    // The window is shown before it moves into the UI state: the state is the
    // only owner the Slint thread needs from here on.
    if launch.start_hidden && tray_available {
        info!("started hidden in the tray");
    } else {
        window.show()?;
        info!("window shown");
    }
    let ui = UiState::new(window, tray);
    ui.show_tray_availability(tray_available);
    UI.with(|cell| *cell.borrow_mut() = Some(ui));

    // The link this launch started with waits until the window exists, which
    // it now does. Requests from later launches are passed on as they come.
    if let Some(link) = launch.link {
        with_ui(|ui| ui.forwarded(Forwarded::Link(link)));
    }
    if let Some(forwards) = launch.forwards {
        spawn_forwarder(forwards);
    }

    runtime.spawn({
        let service = service.clone();
        async move {
            let updates = service.lock().await.subscribe();
            propagate(updates).await;
        }
    });
    runtime.spawn(poll_engine(service.clone()));
    runtime.spawn(poll_connections(service.clone()));
    runtime.spawn(watch_engine_subscriptions(service.clone()));
    runtime.spawn(auto_refresh(service));
    // The startup update check runs only while the setting allows it.
    spawn_update_check(&ctx, false);

    let filters = Timer::default();
    let list_ctx = ctx.clone();
    filters.start(TimerMode::Repeated, FILTER_POLL_INTERVAL, move || {
        with_ui(|ui| {
            if ui.sync_filters() {
                ui.reapply_last();
            }
            if let Some((sort, hide)) = ui.list_prefs_to_save() {
                callbacks::save_list_prefs(&list_ctx, sort, hide);
            }
            ui.sync_page_flags();
        });
    });
    let shortcuts = Timer::default();
    shortcuts.start(TimerMode::Repeated, SHORTCUT_POLL_INTERVAL, || {
        with_ui(|ui| ui.run_pressed_shortcuts());
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
    drop(shortcuts);
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
            active: true,
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
            local: false,
            title: id.to_string(),
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

    fn snapshot(minutes: Option<u32>, subscriptions: Vec<SubscriptionView>) -> AppSnapshot {
        let mut snap = AppSnapshot {
            subscriptions,
            ..AppSnapshot::default()
        };
        snap.preferences.auto_refresh_minutes = minutes;
        snap
    }

    #[test]
    fn a_provider_update_interval_sets_the_schedule_when_the_user_chose_none() {
        let mut slow = subscription(
            "slow",
            SubscriptionMode::AppManaged,
            Some(&stamp(NOW - 2 * 3600)),
        );
        slow.interval_secs = 6 * 3600;
        let snap = snapshot(None, vec![slow]);
        assert!(
            due_subscription_ids(&snap, NOW, &HashMap::new()).is_empty(),
            "two hours into a six-hour provider interval is not due"
        );
    }

    #[test]
    fn the_user_interval_overrides_the_provider_interval_and_zero_is_manual() {
        let mut slow = subscription(
            "slow",
            SubscriptionMode::AppManaged,
            Some(&stamp(NOW - 2 * 3600)),
        );
        slow.interval_secs = 6 * 3600;
        let user = snapshot(Some(60), vec![slow.clone()]);
        assert_eq!(
            due_subscription_ids(&user, NOW, &HashMap::new()),
            vec!["slow".to_string()],
            "the user's hourly setting wins over the provider's six hours"
        );
        let manual = snapshot(Some(0), vec![slow]);
        assert!(due_subscription_ids(&manual, NOW, &HashMap::new()).is_empty());
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
