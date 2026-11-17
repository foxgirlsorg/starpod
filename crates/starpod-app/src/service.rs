//! The application service: the one place the UI's actions are implemented.
//!
//! The UI calls methods on [`AppService`] and renders [`AppSnapshot`] values
//! it receives over a watch channel. It never touches the store, the secret
//! store or the engine directly.
//!
//! # The invariants this file enforces
//!
//! 1. A subscription refresh never changes [`UserState`]. The refresh path
//!    only ever touches subscription metadata and the cached payload, and
//!    persists only those.
//! 2. A failed refresh keeps the last valid cached payload.
//! 3. A build with errors applies nothing, and the errors reach the caller.
//! 4. A candidate the engine rejects does not start or reload the engine.
//! 5. Every change to preferences, rules or selection goes through
//!    [`AppService::commit`]: it builds, validates and reloads a running
//!    engine, and only then persists. A change that fails any step leaves
//!    both the running proxy and the saved state as they were.
//! 6. Secrets never reach the snapshot, the log or an error. Everything that
//!    becomes text is scrubbed against every secret the service has read.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use starpod_core::config::{DEFAULT_GROUP_NAME, DEFAULT_MIXED_PORT};
use starpod_core::rules::{parse_line, translate_all};
use starpod_core::timestamp;
use starpod_core::{
    Node, ProxyGroup, ProxyMode, RuleSpec, RuntimeConfig, StoredRule, Subscription,
    SubscriptionMode, SubscriptionPayload, SubscriptionQuota as CoreQuota, UserPreferences,
};
use starpod_engine::log::redact_urls;
use starpod_engine::reassert::{desired_rule_order, is_match_rule};
use starpod_engine::{
    EngineError, EngineHandle, EnginePhase, EngineStatus, ExitInfo, MeowEngine, ProxyEngine,
    Redactor, ValidationReport,
};
use starpod_store::redact::redact_error;
use starpod_store::secret::{subscription_header_key, subscription_url_key};
use starpod_store::{
    AppPaths, LoadOutcome, SecretRef, SecretStore, SelectedStore, Store, SubscriptionMeta,
    UserState, open_secret_store,
};
use starpod_subscription::logo::fetch_logo;
use starpod_subscription::metadata::DEFAULT_INTERVAL_SECS;
use starpod_subscription::pipeline::parse_payload;
use starpod_subscription::{
    FetchRequest, HttpSubscriptionProvider, RefreshOutcome, SubscriptionCacheSet,
    SubscriptionError, SubscriptionMetadata, SubscriptionProvider, refresh,
};
use tokio::sync::{Mutex, watch};
use url::Url;
use uuid::Uuid;

use crate::reassert::{OwnNode, RefreshWatch, shadow_warnings};

use crate::autostart;
use crate::compose::{Composed, active_subscription_id, compose, global_mode_blocked, only_active};
use crate::connections::ConnectionTracker;
use crate::device::DeviceProfile;
use crate::error::{AppError, RuleLineError};
use crate::snapshot::{
    AnnouncementView, AppSnapshot, DEVICE_LIMIT_MESSAGE, EngineView, GroupView, Latency,
    MemberView, NodeView, NoticeKind, RuleView, SubscriptionStatus, SubscriptionView,
    builtin_target_names, connect_blocked_reason, display_label, display_title, member_protocol,
    profile_card, rule_kind_views, rule_view,
};
use crate::sysproxy::{self, Desired, SystemProxy};

/// The notice shown when the proxy is stopped because the last usable
/// subscription went away.
const STOPPED_FOR_NO_SERVERS: &str = "The proxy was stopped because no subscription has servers any more. Add or refresh a subscription to connect again.";

/// How many lines of the app's own log are kept.
const LOG_CAPACITY: usize = 500;
/// The longest subscription name the app accepts.
const MAX_LABEL_CHARS: usize = 80;
/// Node raw fields that hold credentials, so their values are scrubbed.
const SECRET_NODE_KEYS: &[&str] = &[
    "password",
    "uuid",
    "psk",
    "private-key",
    "auth-str",
    "obfs-password",
    "token",
    "secret",
];
/// Shorter credential values would match ordinary text, so they are not
/// learned as secrets.
const MIN_SECRET_LEN: usize = 4;
/// The geodata sources used when the user has not set their own.
const DEFAULT_GEO_MMDB_URL: &str =
    "https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/country.mmdb";
const DEFAULT_GEOSITE_URL: &str =
    "https://github.com/MetaCubeX/meta-rules-dat/releases/latest/download/geosite.dat";
const DEFAULT_GEO_ASN_URL: &str =
    "https://github.com/P3TERX/GeoLite.mmdb/releases/latest/download/GeoLite2-ASN.mmdb";

/// The dependencies an [`AppService`] is built from.
///
/// Tests inject a fake engine and a static provider here. The app's real
/// dependencies come from [`AppService::open_default`].
pub struct ServiceDeps {
    /// The on-disk store.
    pub store: Store,
    /// The secret store, with the reason the platform store was not used if
    /// the fallback was chosen.
    pub secrets: SelectedStore,
    /// The proxy core.
    pub engine: Box<dyn ProxyEngine>,
    /// Where subscription bytes come from.
    pub provider: Arc<dyn SubscriptionProvider>,
    /// Whether the engine binary is installed.
    pub engine_installed: bool,
}

/// The service behind a lock, for a UI that calls it from another thread.
pub type SharedAppService = Arc<Mutex<AppService>>;

/// The result of checking the candidate configuration without applying it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigCheck {
    /// Whether the engine accepted the candidate.
    pub accepted: bool,
    /// The engine's message when it did not, redacted.
    pub engine_error: Option<String>,
    /// Warnings from the app's own build and from the engine.
    pub warnings: Vec<String>,
}

/// The text of one profile, as the profile editor shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileSource {
    /// The configuration text: the pasted source for a local profile, or the
    /// cached payload as JSON for a fetched one.
    pub text: String,
    /// Whether the profile is local, which means `text` is the only copy of it.
    pub local: bool,
}

/// A phone's VPN (Android's `VpnService`): it captures the device's traffic
/// and hands it to the engine's local SOCKS port.
///
/// Connecting asks for the VPN permission first ([`PhoneTunnel::prepare`]),
/// then starts the engine, then the tunnel. The proxy counts as connected
/// once the tunnel reports it is up ([`AppService::phone_tunnel_ready`]).
/// A tunnel that cannot start, or that the system closes later, reports it
/// with [`AppService::phone_tunnel_failed`].
#[async_trait::async_trait]
pub trait PhoneTunnel: Send + Sync {
    /// Make sure the app may open the tunnel, asking the user if it may
    /// not yet. An error says why it may not.
    async fn prepare(&self) -> Result<(), String>;
    /// Capture the device's traffic and send it to `127.0.0.1:socks_port`.
    /// Returns at once.
    fn start(&self, socks_port: u16);
    /// Stop capturing traffic.
    fn stop(&self);
}

/// The application service.
///
/// Every mutating method leaves the snapshot up to date and publishes it.
/// Read it with [`AppService::snapshot`], or subscribe with
/// [`AppService::subscribe`] and wait for changes.
pub struct AppService {
    store: Store,
    secrets: Box<dyn SecretStore>,
    secret_fallback_reason: Option<String>,
    engine: Box<dyn ProxyEngine>,
    provider: Arc<dyn SubscriptionProvider>,
    engine_installed: bool,
    state: UserState,
    meta: SubscriptionMeta,
    caches: SubscriptionCacheSet,
    /// `false` when the user-state file is from a newer build, so a write
    /// would destroy data this build cannot read.
    state_writable: bool,
    /// As `state_writable`, for the subscription-metadata file.
    meta_writable: bool,
    /// The configuration the state last built into.
    built: Option<RuntimeConfig>,
    /// The configuration the running engine was started or last reloaded
    /// with, when this run knows it. A change that builds the same one
    /// leaves the engine alone.
    running_config: Option<RuntimeConfig>,
    /// The primary group of the configuration in `built`.
    built_primary: Option<String>,
    /// The result of the last latency test, by node id. `None` means the
    /// probe failed or timed out.
    latencies: BTreeMap<String, Option<u32>>,
    /// The member each group uses now, as the running engine reports it
    /// (group name to member name). Empty while the engine is not running.
    live_selected: BTreeMap<String, String>,
    warnings: Vec<String>,
    /// Warnings about engine-managed subscriptions, from the last engine
    /// refresh check. Kept apart from `warnings`, which each build replaces.
    engine_warnings: Vec<String>,
    /// What the engine last reported for its managed subscriptions.
    refresh_watch: RefreshWatch,
    engine_status: EngineStatus,
    engine_error: Option<String>,
    handle: Option<EngineHandle>,
    notice: Option<(NoticeKind, String)>,
    app_log: VecDeque<String>,
    /// The engine release installed, by label. `None` when none is installed.
    installed_engine: Option<String>,
    /// The newest release of the chosen channel, as of the last check.
    latest_engine: Option<String>,
    redactor: Redactor,
    /// The engine's open connections and the history of closed ones.
    connections: ConnectionTracker,
    /// This device, as provider headers describe it. Probed once at start.
    device: DeviceProfile,
    /// The operating system's proxy, as Starpod has set it.
    system_proxy: SystemProxy,
    /// The platform code that reads and writes the system proxy.
    system_proxy_backend: Box<dyn sysproxy::Backend>,
    /// On a phone, the VPN that carries the device's traffic to the engine.
    /// When set, it takes the system proxy's place: it is on exactly while
    /// the engine runs, whatever the system proxy setting says.
    phone_tunnel: Option<Arc<dyn PhoneTunnel>>,
    /// The engine port the phone tunnel was started for, while it is on.
    phone_tunnel_port: Option<u16>,
    /// Whether that tunnel is up and carrying traffic.
    phone_tunnel_up: bool,
    updates: watch::Sender<AppSnapshot>,
    /// A subscription the user just added, to be put in use once its first
    /// refresh has servers. Putting it in use before then would leave the
    /// running proxy with no servers, and stop it.
    activate_after_refresh: Option<String>,
}

impl AppService {
    /// Build the service from explicit dependencies. Nothing is read from
    /// disk until [`AppService::load`] is called.
    pub fn new(deps: ServiceDeps) -> Self {
        let ServiceDeps {
            store,
            secrets,
            engine,
            provider,
            engine_installed,
        } = deps;
        let installed_engine =
            starpod_engine::download::installed_version_in(store.paths().data_dir());
        let SelectedStore {
            store: secret_store,
            fallback_reason,
        } = secrets;
        let device = DeviceProfile::detect(&store.paths().device_id_file());
        let service = Self {
            store,
            secrets: secret_store,
            secret_fallback_reason: fallback_reason,
            engine,
            provider,
            engine_installed,
            state: UserState::default(),
            meta: SubscriptionMeta::default(),
            caches: SubscriptionCacheSet::new(),
            state_writable: true,
            meta_writable: true,
            built: None,
            running_config: None,
            built_primary: None,
            latencies: BTreeMap::new(),
            live_selected: BTreeMap::new(),
            warnings: Vec::new(),
            engine_warnings: Vec::new(),
            refresh_watch: RefreshWatch::default(),
            engine_status: EngineStatus::default(),
            engine_error: None,
            handle: None,
            notice: None,
            app_log: VecDeque::new(),
            installed_engine,
            latest_engine: None,
            redactor: Redactor::default(),
            connections: ConnectionTracker::new(),
            device,
            system_proxy: SystemProxy::default(),
            system_proxy_backend: sysproxy::os_backend(),
            phone_tunnel: None,
            phone_tunnel_port: None,
            phone_tunnel_up: false,
            updates: watch::channel(AppSnapshot::default()).0,
            activate_after_refresh: None,
        };
        service.publish();
        service
    }

    /// Build the service with the app's real dependencies: the platform
    /// directories, the platform secret store, the installed meow engine and
    /// the HTTP subscription provider.
    ///
    /// The result has not loaded anything yet. Call [`AppService::load`]
    /// before use.
    pub fn open_default() -> Result<Self, AppError> {
        let paths = AppPaths::resolve().map_err(|error| AppError::Storage(error.to_string()))?;
        let secrets = open_secret_store(paths.fallback_secret_file());
        // The engine's controller file lives in the app's own directory, not
        // the shared default, so this app and another meow never collide.
        let engine = MeowEngine::installed(paths.runtime_config_file())
            .with_controller_dir(paths.controller_dir());
        let provider = HttpSubscriptionProvider::new()
            .map_err(|error| AppError::Subscription(error.to_string()))?;
        let engine_installed = starpod_engine::download::is_installed_in(paths.data_dir());
        Ok(Self::new(ServiceDeps {
            store: Store::new(paths),
            secrets,
            engine: Box::new(engine),
            provider: Arc::new(provider),
            engine_installed,
        }))
    }

    /// Move the service behind a lock, for a UI on another thread.
    /// Use `tunnel` as the way traffic reaches the engine (Android's VPN)
    /// instead of the system proxy.
    pub fn set_phone_tunnel(&mut self, tunnel: Arc<dyn PhoneTunnel>) {
        self.phone_tunnel = Some(tunnel);
    }

    /// The phone tunnel could not start, or the system closed it (the user
    /// turned the VPN off in the system settings, or another VPN app took
    /// over). The proxy is stopped, since nothing reaches it any more, and
    /// `reason` is shown.
    pub async fn phone_tunnel_failed(&mut self, reason: &str) {
        self.phone_tunnel_port = None;
        self.phone_tunnel_up = false;
        self.log(format!("the VPN stopped: {reason}"));
        let result = self
            .disconnect_inner()
            .await
            .and(Err::<(), _>(AppError::Invalid(reason.to_string())));
        let _ = self.finish(result);
    }

    /// The phone tunnel is up: the proxy now counts as connected.
    pub fn phone_tunnel_ready(&mut self) {
        if self.phone_tunnel_port.is_some() && !self.phone_tunnel_up {
            self.phone_tunnel_up = true;
            self.log("the VPN is up");
            self.publish();
        }
    }

    pub fn into_shared(self) -> SharedAppService {
        Arc::new(Mutex::new(self))
    }

    /// A receiver that is updated after every change.
    pub fn subscribe(&self) -> watch::Receiver<AppSnapshot> {
        self.updates.subscribe()
    }

    /// The current view state.
    pub fn snapshot(&self) -> AppSnapshot {
        let prefs = &self.state.preferences;
        // The Rust-side strings of this snapshot follow the saved language.
        crate::i18n::set(crate::i18n::effective(&prefs.language));
        let built = self.built.as_ref();
        let selected_node = prefs.selected_node_id.as_deref();

        let nodes: Vec<NodeView> = built
            .map(|config| {
                config
                    .proxies
                    .iter()
                    .map(|node| NodeView {
                        latency_ms: self.latencies.get(&node.id).copied().flatten(),
                        id: node.id.clone(),
                        name: node.name.clone(),
                        protocol: node.protocol.clone(),
                        group: config
                            .proxy_groups
                            .iter()
                            .find(|group| group.members.contains(&node.name))
                            .map(|group| group.name.clone()),
                        selected: selected_node == Some(node.id.as_str()),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let current_node = nodes.iter().find(|node| node.selected).cloned();

        let logo_path = |subscription: &Subscription| {
            subscription
                .logo_file
                .as_ref()
                .map(|name| self.store.paths().subscription_logo_dir().join(name))
                .filter(|path| path.is_file())
        };

        let active_id = active_subscription_id(&self.state, &self.meta);
        let subscriptions = self
            .meta
            .subscriptions
            .iter()
            .map(|subscription| SubscriptionView {
                active: active_id == Some(subscription.id.as_str()),
                id: subscription.id.clone(),
                label: subscription.label.clone(),
                mode: subscription.mode,
                status: subscription_status(subscription),
                last_updated: subscription.last_updated.clone(),
                error: subscription.last_error.clone(),
                node_count: subscription.node_count,
                group_count: subscription.group_count,
                quota: subscription.quota.clone(),
                has_auth_header: subscription.has_auth_header,
                local: subscription.local,
                title: display_title(subscription),
                announce: subscription.announce.clone(),
                support_url: subscription.support_url.clone(),
                web_url: subscription.web_url.clone(),
                web_page_name: subscription.web_page_name.clone(),
                announce_url: subscription.announce_url.clone(),
                logo_path: logo_path(subscription),
                interval_secs: subscription.interval_secs,
                global_mode_disabled: subscription.global_mode_disabled,
                expand_proxy_groups: subscription.expand_proxy_groups,
                device_limit: subscription.device_limit,
            })
            .collect();

        // The announcement of the subscription in use, or its device-limit
        // notice, carrying the support link. The others are not in use, so
        // their news is not shown.
        let announcements: Vec<AnnouncementView> = self
            .meta
            .subscriptions
            .iter()
            .filter(|subscription| active_id == Some(subscription.id.as_str()))
            .filter_map(|subscription| {
                let text = match subscription
                    .announce
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                {
                    Some(text) => text.to_string(),
                    None if subscription.device_limit => {
                        crate::i18n::t(DEVICE_LIMIT_MESSAGE).to_string()
                    }
                    None => return None,
                };
                Some(AnnouncementView {
                    subscription_id: subscription.id.clone(),
                    title: display_title(subscription),
                    text,
                    announce_url: subscription.announce_url.clone(),
                    support_url: subscription.support_url.clone(),
                    web_url: subscription.web_url.clone(),
                })
            })
            .collect();

        // The Home page card is for the subscription in use.
        let active = self
            .meta
            .subscriptions
            .iter()
            .find(|subscription| active_id == Some(subscription.id.as_str()));
        let card = active
            .map(|subscription| profile_card(subscription, logo_path(subscription), now_unix()));
        let global_mode_allowed =
            !global_mode_blocked(&only_active(&self.state, &self.meta), &self.caches);

        let rules: Vec<RuleView> = self
            .state
            .rules
            .iter()
            .enumerate()
            .map(|(index, rule)| rule_view(index, Some(&rule.id), rule.enabled, &rule.spec, false))
            .collect();

        let subscription_rules: Vec<RuleView> = self
            .meta
            .subscriptions
            .iter()
            .filter(|subscription| subscription.mode == SubscriptionMode::AppManaged)
            .filter_map(|subscription| self.caches.payload(&subscription.id))
            .flat_map(|payload| payload.rules.iter())
            .enumerate()
            .map(|(index, spec)| rule_view(index, None, true, spec, true))
            .collect();

        let mut rule_targets = builtin_target_names();
        if let Some(config) = built {
            rule_targets.extend(config.proxy_groups.iter().map(|group| group.name.clone()));
            rule_targets.extend(config.proxies.iter().map(|node| node.name.clone()));
        }

        let origins = self.name_origins();
        let groups: Vec<GroupView> = built
            .map(|config| self.group_views(config, prefs, &origins))
            .unwrap_or_default();
        let primary_group = built.and(self.built_primary.clone());

        let mode = built
            .map(|config| config.mode)
            .unwrap_or_else(|| prefs.mode.unwrap_or_default());

        let listen_address = built.and_then(|config| {
            config.mixed_port.or(config.port).map(|port| {
                let bind = config.bind_address.clone().unwrap_or_else(|| {
                    if config.allow_lan {
                        "0.0.0.0".to_string()
                    } else {
                        "127.0.0.1".to_string()
                    }
                });
                format!("{bind}:{port}")
            })
        });

        // On a phone the proxy is connected once its VPN is up; until then
        // it is still connecting.
        let tunnel_pending = self.phone_tunnel_port.is_some() && !self.phone_tunnel_up;
        let engine = EngineView {
            phase: if tunnel_pending && self.engine_status.phase.is_running() {
                EnginePhase::Starting
            } else {
                self.engine_status.phase
            },
            installed: self.engine_installed,
            version: self.engine_status.version.clone(),
            uptime_secs: self.engine_status.uptime_secs,
            elevated: self.engine_status.elevated,
            error: self.engine_error.clone().or_else(|| {
                self.engine_status
                    .last_error
                    .as_deref()
                    .map(|error| self.scrub(error))
            }),
        };

        let retention = self.state.preferences.effective_log_retention_lines() as usize;
        let engine_log_lines = self
            .engine
            .logs()
            .tail(retention)
            .into_iter()
            .map(|line| self.scrub(&redact_urls(&line.text)))
            .collect();

        AppSnapshot {
            engine_update: self.engine_update_view(),
            geo_updated_at: prefs.geo_updated_at,
            engine,
            mode,
            listen_address,
            current_node,
            subscriptions,
            nodes,
            rules,
            subscription_rules,
            rule_kinds: rule_kind_views(),
            rule_targets,
            groups,
            primary_group,
            announcements,
            profile_card: card,
            global_mode_allowed,
            preferences: prefs.clone(),
            connections: self.connections.view(),
            warnings: self
                .warnings
                .iter()
                .chain(&self.engine_warnings)
                .cloned()
                .collect(),
            log_lines: self.app_log.iter().cloned().collect(),
            engine_log_lines,
            secret_backend: self.secrets.backend_name().to_string(),
            secret_encrypted: self.secrets.is_encrypted(),
            secret_fallback_reason: self.secret_fallback_reason.clone(),
            data_dir: self.store.paths().data_dir().to_path_buf(),
            notice: self.notice.clone(),
            connect_blocked: self.connect_block(),
            system_proxy_on: prefs.system_proxy,
            system_proxy_active: self.system_proxy.is_active(),
            system_proxy_error: self.system_proxy.error.clone(),
        }
    }

    /// The `set_system_proxy` action's body.
    async fn set_system_proxy_inner(&mut self, enabled: bool) -> Result<(), AppError> {
        if self.state.preferences.system_proxy == enabled {
            // The setting is already saved. Enabling it again retries a change
            // that failed earlier; the sync does nothing when it is in effect.
            if enabled {
                self.sync_system_proxy().await;
            }
            return Ok(());
        }
        let mut state = self.state.clone();
        state.preferences.system_proxy = enabled;
        self.check_writable(&state, &self.meta)?;
        self.state = state;
        let save = self.store.save_user_state(&self.state);
        self.sync_system_proxy().await;
        save.map_err(|error| {
            AppError::Storage(format!(
                "the change is in effect but could not be saved: {}",
                self.scrub(&error.to_string())
            ))
        })
    }

    /// Make the operating system's proxy match the saved settings and the
    /// engine's state. It is set exactly when the setting is on and the engine
    /// runs, and restored otherwise. A failure is recorded, not returned, so a
    /// proxy problem never fails the action that caused the sync.
    async fn sync_system_proxy(&mut self) {
        if let Some(tunnel) = self.phone_tunnel.clone() {
            let port = self.current_mixed_port();
            let wanted = self.engine_status.phase.is_running().then_some(port);
            if wanted != self.phone_tunnel_port {
                if self.phone_tunnel_port.take().is_some() {
                    tunnel.stop();
                }
                self.phone_tunnel_up = false;
                if let Some(port) = wanted {
                    tunnel.start(port);
                    self.phone_tunnel_port = Some(port);
                }
            }
            return;
        }
        let prefs = &self.state.preferences;
        let want = Desired {
            mode: prefs.sysproxy_mode,
            port: self.current_mixed_port(),
            bypass: prefs.sysproxy_bypass.clone(),
            pac_script: prefs.pac_script.clone(),
        };
        let wanted = prefs.system_proxy && self.engine_status.phase.is_running();
        let path = self.system_proxy_record();
        if wanted {
            // Do not point the system at a port nothing listens on: the
            // browsers would then fail every request.
            if !Self::mixed_port_listens(want.port).await {
                let text = format!(
                    "Nothing is listening on 127.0.0.1:{}; another program may be using that port",
                    want.port
                );
                self.log(format!("the system proxy was not changed: {text}"));
                self.system_proxy.error = Some(text);
                return;
            }
            let result = self
                .system_proxy
                .apply(self.system_proxy_backend.as_ref(), &path, want)
                .await;
            self.finish_system_proxy(result);
        } else if self.system_proxy.is_active() || path.exists() {
            let result = self
                .system_proxy
                .release(self.system_proxy_backend.as_ref(), &path);
            self.finish_system_proxy(result);
        }
    }

    /// Record the outcome of a system proxy change. Warnings go to the log;
    /// an error stays visible in the snapshot until a later change succeeds.
    fn finish_system_proxy(&mut self, result: Result<Vec<String>, sysproxy::SysproxyError>) {
        match result {
            Ok(warnings) => {
                for warning in warnings {
                    self.log(warning);
                }
                self.system_proxy.error = None;
            }
            Err(error) => {
                let text = self.scrub(&error.0);
                self.log(format!("the system proxy could not be changed: {text}"));
                self.system_proxy.error = Some(text);
            }
        }
    }

    /// Where the saved proxy settings are kept, beside the other app data.
    fn system_proxy_record(&self) -> std::path::PathBuf {
        self.store.paths().data_dir().join(sysproxy::RECORD_FILE)
    }

    /// The mixed port the engine listens on, which the system proxy points at.
    fn current_mixed_port(&self) -> u16 {
        // The running engine's port. Before it is built, the saved preference
        // is the port it will listen on.
        self.built
            .as_ref()
            .and_then(|config| config.mixed_port.or(config.port))
            .or(self.state.preferences.mixed_port)
            .unwrap_or(DEFAULT_MIXED_PORT)
    }

    /// Whether something accepts connections on 127.0.0.1:`port`, within half
    /// a second. A port another program holds, or a listener that never
    /// started, does not.
    async fn mixed_port_listens(port: u16) -> bool {
        tokio::time::timeout(
            std::time::Duration::from_millis(500),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await
        .is_ok_and(|connected| connected.is_ok())
    }

    /// Restore the proxy settings a crashed run left behind. Returns a
    /// problem to report when the restore failed; the record is kept then, so
    /// the next start tries again.
    fn recover_system_proxy(&mut self) -> Option<String> {
        let path = self.system_proxy_record();
        if !path.exists() {
            return None;
        }
        match sysproxy::recover(self.system_proxy_backend.as_ref(), &path) {
            Ok(warnings) => {
                for warning in warnings {
                    self.log(warning);
                }
                self.log("restored the system proxy that an earlier run had changed");
                None
            }
            Err(error) => Some(format!(
                "the system proxy left by an earlier run could not be restored: {}",
                self.scrub(&error.0)
            )),
        }
    }

    /// Write or remove the login entry, and say so in the log when it fails.
    fn apply_autostart(&mut self, enabled: bool) {
        if let Err(error) = autostart::set_enabled(enabled) {
            self.log(error.clone());
            self.notice = Some((NoticeKind::Warning, error));
        }
    }

    /// Why connecting is not allowed right now, or `None` when it is.
    ///
    /// This is the only place the connect rule is applied; see
    /// [`connect_blocked_reason`]. It reads the saved subscriptions and their
    /// cached payloads, never the engine.
    fn connect_block(&self) -> Option<String> {
        self.connect_block_for(&self.state, &self.meta, &self.caches)
    }

    /// As [`AppService::connect_block`], for a candidate set of subscriptions
    /// and caches that has not been saved yet.
    fn connect_block_for(
        &self,
        state: &UserState,
        meta: &SubscriptionMeta,
        caches: &SubscriptionCacheSet,
    ) -> Option<String> {
        // Only the subscription in use counts: the others take no part in
        // the build, so their servers cannot carry traffic.
        let meta = only_active(state, meta);
        let rows: Vec<(&Subscription, Option<usize>)> = meta
            .subscriptions
            .iter()
            .map(|subscription| {
                let cached_nodes = caches
                    .payload(&subscription.id)
                    .map(|payload| payload.nodes.len());
                (subscription, cached_nodes)
            })
            .collect();
        connect_blocked_reason(self.engine_installed, &rows)
    }

    /// Refuse to start the proxy when the connect rule blocks it. Checked
    /// before the engine is touched.
    fn ensure_connectable(&self) -> Result<(), AppError> {
        match self.connect_block() {
            Some(reason) => Err(AppError::Invalid(reason)),
            None => Ok(()),
        }
    }

    /// The user's saved state, as the service holds it.
    pub fn user_state(&self) -> &UserState {
        &self.state
    }

    /// The directories the app keeps its files in.
    pub fn paths(&self) -> &AppPaths {
        self.store.paths()
    }

    /// Load the saved state and the cached subscription payloads.
    ///
    /// A file that cannot be read is moved aside and the app starts from
    /// defaults. The problem is reported as a notice, never as a failure, so
    /// the app always starts. A file from a newer build is left untouched and
    /// makes the affected state read-only.
    pub async fn load(&mut self) -> Result<(), AppError> {
        let mut problems: Vec<String> = Vec::new();

        match self.store.load_user_state() {
            Ok(outcome) => {
                self.state_writable = !matches!(
                    outcome,
                    LoadOutcome::Future { .. } | LoadOutcome::Unmigratable { .. }
                );
                if let Some(text) = outcome.describe() {
                    problems.push(self.scrub(&text));
                }
                self.state = match outcome {
                    LoadOutcome::Loaded(state) => state,
                    _ => UserState::default(),
                };
            }
            Err(error) => {
                self.state_writable = false;
                problems.push(format!(
                    "the preferences could not be read ({}); they will not be changed until this is resolved",
                    self.scrub(&error.to_string())
                ));
            }
        }

        match self.store.load_subscription_meta() {
            Ok(outcome) => {
                self.meta_writable = !matches!(
                    outcome,
                    LoadOutcome::Future { .. } | LoadOutcome::Unmigratable { .. }
                );
                if let Some(text) = outcome.describe() {
                    problems.push(self.scrub(&text));
                }
                self.meta = match outcome {
                    LoadOutcome::Loaded(meta) => meta,
                    _ => SubscriptionMeta::default(),
                };
            }
            Err(error) => {
                self.meta_writable = false;
                problems.push(format!(
                    "the subscription list could not be read ({}); it will not be changed until this is resolved",
                    self.scrub(&error.to_string())
                ));
            }
        }

        self.caches = SubscriptionCacheSet::new();
        let subscriptions = self.meta.subscriptions.clone();
        for subscription in subscriptions
            .iter()
            .filter(|subscription| subscription.mode == SubscriptionMode::AppManaged)
        {
            match self.store.read_subscription_cache(&subscription.id) {
                Ok(Some(bytes)) => match serde_json::from_slice::<SubscriptionPayload>(&bytes) {
                    Ok(mut payload) => {
                        // A cache written by an earlier build lacks later
                        // parser fixes; apply them now, not at the next refresh.
                        starpod_subscription::upgrade_cached_payload(
                            &mut payload,
                            &subscription.label,
                        );
                        let stored_at = subscription
                            .last_updated
                            .as_deref()
                            .and_then(timestamp::parse)
                            .unwrap_or(0);
                        self.caches.store(&subscription.id, payload, stored_at);
                    }
                    Err(_) => problems.push(format!(
                        "the saved nodes of “{}” could not be read; refresh the subscription",
                        subscription.label
                    )),
                },
                Ok(None) => {}
                Err(error) => problems.push(format!(
                    "the saved nodes of “{}” could not be read ({})",
                    subscription.label,
                    self.scrub(&error.to_string())
                )),
            }
        }

        // An engine an earlier run left running is taken over, not restarted.
        match self.engine.adopt().await {
            Ok(true) => self.log("reattached to the proxy engine that an earlier run started"),
            Ok(false) => {}
            Err(error) => problems.push(self.scrub(&error.to_string())),
        }
        self.refresh_engine_status().await;
        self.notice = None;
        match self.compose_current() {
            Ok(composed) => self.adopt(&composed),
            Err(error) => problems.push(error.to_string()),
        }
        // A system proxy an earlier run left changed is restored first, unless
        // an engine it started is still running and still uses it.
        if !self.engine_status.phase.is_running()
            && let Some(problem) = self.recover_system_proxy()
        {
            problems.push(problem);
        }
        self.sync_system_proxy().await;

        if !problems.is_empty() {
            for problem in &problems {
                self.log(problem.clone());
            }
            self.notice = Some((NoticeKind::Warning, problems.join(" ")));
        }
        self.publish();
        Ok(())
    }

    /// Ask the engine for its state and reap a process that has exited.
    pub async fn refresh_engine_status(&mut self) -> EnginePhase {
        match self.engine.status().await {
            Ok(status) => self.engine_status = status,
            Err(error) => {
                self.engine_status.phase = EnginePhase::Failed;
                self.engine_error = Some(self.scrub(&error.to_string()));
            }
        }
        let exited: Option<ExitInfo> = self
            .handle
            .as_ref()
            .and_then(|handle| (*handle.exited.borrow()).clone());
        if let Some(info) = exited {
            self.handle = None;
            self.log(format!("the engine stopped (exit code {:?})", info.code));
        }
        // An engine that stopped on its own takes its system proxy with it.
        if !self.engine_status.phase.is_running() && self.system_proxy.is_active() {
            self.sync_system_proxy().await;
        }
        self.refresh_live_selections().await;
        self.publish();
        self.engine_status.phase
    }

    /// Read which member each group uses now, from the running engine. When
    /// the engine is not running, or does not answer, nothing is kept and the
    /// saved choices are shown instead.
    async fn refresh_live_selections(&mut self) {
        if !self.engine_status.phase.is_running() {
            self.live_selected.clear();
            return;
        }
        match self.engine.proxies().await {
            Ok(proxies) => {
                self.live_selected = proxies
                    .into_iter()
                    .filter_map(|proxy| proxy.now.map(|now| (proxy.name, now)))
                    .collect();
            }
            Err(_) => self.live_selected.clear(),
        }
    }

    /// Add a subscription. Its URL and optional header go into the secret
    /// store; the metadata file holds only their keys.
    ///
    /// This does not fetch the subscription. Call
    /// [`AppService::refresh_subscription`] for that.
    pub async fn add_subscription(
        &mut self,
        label: String,
        url: String,
        header: Option<String>,
        mode: SubscriptionMode,
    ) -> Result<String, AppError> {
        let result = self.add_subscription_inner(label, url, header, mode).await;
        self.finish(result)
    }

    /// Add a profile from pasted configuration text instead of a URL.
    ///
    /// The text is parsed before anything is stored, so text the parser
    /// rejects adds nothing. A local profile is never fetched: its source is
    /// kept in the data directory and a refresh re-parses it. The new profile
    /// is put in use at once. Returns its id.
    pub async fn add_local_subscription(
        &mut self,
        label: String,
        source: String,
    ) -> Result<String, AppError> {
        let result = self.add_local_inner(label, source).await;
        self.finish(result)
    }

    /// The text of a profile, for the editor. A local profile gives its pasted
    /// source; a fetched one gives its cached payload as JSON. Fails when a
    /// fetched profile has not been downloaded yet.
    pub async fn profile_source(&mut self, id: &str) -> Result<ProfileSource, AppError> {
        let subscription = self
            .meta
            .subscriptions
            .iter()
            .find(|subscription| subscription.id == id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("that subscription".to_string()))?;
        if subscription.local {
            let text = self
                .store
                .read_profile_source(id)
                .map_err(|error| AppError::Storage(self.scrub(&error.to_string())))?
                .ok_or_else(|| missing_source(&subscription.label))?;
            return Ok(ProfileSource { text, local: true });
        }
        let payload = self.caches.payload(id).ok_or_else(|| {
            AppError::Invalid("this profile has not been downloaded yet".to_string())
        })?;
        let text = serde_json::to_string_pretty(payload)
            .map_err(|error| AppError::Storage(error.to_string()))?;
        Ok(ProfileSource { text, local: false })
    }

    /// Replace the content of a profile with edited text.
    ///
    /// A local profile takes a configuration, which is parsed first; a fetched
    /// profile takes either its cached JSON or a configuration, which is
    /// stored as its payload. The text is checked before anything changes.
    /// A running proxy is reloaded the same way a refresh reloads it.
    pub async fn save_profile_source(&mut self, id: &str, text: String) -> Result<(), AppError> {
        let result = self.save_profile_source_inner(id, text).await;
        self.finish(result)
    }

    /// The id of the subscription whose stored address is `url`, if there is
    /// one. A deep link for a subscription that is already added refreshes it
    /// rather than adding a copy.
    pub fn subscription_id_for_url(&mut self, url: &str) -> Option<String> {
        let url = url.trim();
        let ids: Vec<String> = self
            .meta
            .subscriptions
            .iter()
            .filter(|subscription| !subscription.local)
            .map(|subscription| subscription.id.clone())
            .collect();
        ids.into_iter().find(|id| {
            self.read_secret(&subscription_url_key(id))
                .is_ok_and(|stored| stored == url)
        })
    }

    /// Remove a subscription, its secrets and its cached payload.
    ///
    /// If that leaves no subscription with servers while the proxy runs, the
    /// proxy is stopped and a notice says why (see [`AppService::commit`]).
    pub async fn remove_subscription(&mut self, id: &str) -> Result<(), AppError> {
        let result = self.remove_subscription_inner(id).await;
        self.finish(result)
    }

    /// Change who fetches a subscription.
    pub async fn set_subscription_mode(
        &mut self,
        id: &str,
        mode: SubscriptionMode,
    ) -> Result<(), AppError> {
        let result = self.set_subscription_mode_inner(id, mode).await;
        self.finish(result)
    }

    /// Use the subscription `id`, and only it. The configuration is rebuilt
    /// from it, and a running proxy is reloaded onto it.
    pub async fn set_active_subscription(&mut self, id: &str) -> Result<(), AppError> {
        let result = self.set_active_subscription_inner(id).await;
        self.finish(result)
    }

    /// Fetch one app-managed subscription.
    ///
    /// On failure the previous payload stays in use and the error is
    /// recorded against the subscription.
    pub async fn refresh_subscription(&mut self, id: &str) -> Result<(), AppError> {
        let result = self.refresh_inner(id).await;
        self.finish(result)
    }

    /// Fetch every app-managed subscription. Returns how many succeeded.
    ///
    /// The error is returned only when every attempt failed. A partial
    /// failure is shown as a notice and in each subscription's status.
    pub async fn refresh_all(&mut self) -> Result<usize, AppError> {
        let result = self.refresh_all_inner().await;
        self.finish(result)
    }

    /// Forget every cached payload and its timestamps. The user's own
    /// settings are kept. The running proxy is not changed until the next
    /// apply.
    pub async fn clear_cache(&mut self) -> Result<(), AppError> {
        let result = self.clear_cache_inner().await;
        self.finish(result)
    }

    /// Build and validate the candidate configuration without applying it.
    pub async fn check_configuration(&mut self) -> Result<ConfigCheck, AppError> {
        let result = self.check_inner().await;
        self.finish(result)
    }

    /// Start the proxy with the current configuration. Does nothing if it is
    /// already running.
    ///
    /// Fails with [`AppError::Invalid`], without touching the engine, when
    /// the connect rule blocks it (no subscription supplies servers, or the
    /// engine is not installed).
    pub async fn connect(&mut self) -> Result<(), AppError> {
        let result = self.connect_inner().await;
        self.finish(result)
    }

    /// Stop the proxy. Does nothing if it is stopped.
    pub async fn disconnect(&mut self) -> Result<(), AppError> {
        let result = self.disconnect_inner().await;
        self.finish(result)
    }

    /// Stop the proxy and start it again with the current configuration.
    ///
    /// Refused with [`AppError::Invalid`] while the connect rule blocks it,
    /// before the proxy is stopped.
    pub async fn restart(&mut self) -> Result<(), AppError> {
        let result = self.restart_inner().await;
        self.finish(result)
    }

    /// Set the routing mode.
    pub async fn set_mode(&mut self, mode: ProxyMode) -> Result<(), AppError> {
        let result = self.set_mode_inner(mode).await;
        self.finish(result)
    }

    /// Replace the user's preferences.
    ///
    /// A change to the login start is applied to the operating system here,
    /// since it is not part of the engine's configuration.
    pub async fn save_preferences(&mut self, prefs: UserPreferences) -> Result<(), AppError> {
        let autostart = prefs.autostart;
        let was_autostart = self.state.preferences.autostart;
        let result = self.save_preferences_inner(prefs).await;
        if autostart != was_autostart && self.state.preferences.autostart == autostart {
            self.apply_autostart(autostart);
        }
        self.finish(result)
    }

    /// Turn the system proxy on or off. When it is on and the engine runs, the
    /// operating system is pointed at the engine at once. The engine is not
    /// reloaded, because the proxy settings are not part of its configuration.
    pub async fn set_system_proxy(&mut self, enabled: bool) -> Result<(), AppError> {
        let result = self.set_system_proxy_inner(enabled).await;
        self.finish(result)
    }

    /// Restore the operating system's proxy if Starpod changed it. Called when
    /// the app quits and from the panic hook. The engine is not stopped here.
    pub async fn restore_system_proxy(&mut self) {
        let path = self.system_proxy_record();
        let result = self
            .system_proxy
            .release(self.system_proxy_backend.as_ref(), &path);
        self.finish_system_proxy(result);
        self.publish();
    }

    /// Make the login entry match the saved setting. Called at startup, since
    /// the executable's path may have moved since the entry was written.
    pub fn sync_autostart(&mut self) {
        let enabled = self.state.preferences.autostart;
        self.apply_autostart(enabled);
        self.publish();
    }

    /// Replace the backend that changes the operating system's proxy. The app
    /// uses the platform's own backend; tests substitute a fake one.
    pub fn set_system_proxy_backend(&mut self, backend: Box<dyn sysproxy::Backend>) {
        self.system_proxy_backend = backend;
    }

    /// Select a node by id.
    ///
    /// The choice is saved and takes effect at the next start. When the
    /// engine is running, it is also applied to the running engine at once,
    /// without a reload, so open connections are not closed.
    pub async fn select_node(&mut self, node_id: &str) -> Result<(), AppError> {
        let result = self.select_node_inner(node_id).await;
        self.finish(result)
    }

    /// Choose the member a `select` group uses.
    ///
    /// The choice is saved for the group and applied on every build. When the
    /// engine is running it switches live, without a reload. Fails with
    /// [`AppError::Invalid`] when the group is not a `select` group, the member
    /// is not in it, or the group does not exist.
    pub async fn select_member(&mut self, group: &str, member: &str) -> Result<(), AppError> {
        let result = self.select_member_inner(group, member).await;
        self.finish(result)
    }

    /// Measure every node's latency against the running engine.
    ///
    /// Probes run one after another, each bounded by a short timeout, so a
    /// long node list takes a while. A node whose probe fails is recorded as
    /// untested. Fails with [`AppError::Invalid`] when the proxy is stopped.
    pub async fn test_latency(&mut self) -> Result<(), AppError> {
        let result = self.test_latency_inner().await;
        self.finish(result)
    }

    /// Read the engine's open connections into the Connections page's data.
    ///
    /// Called on each poll while the page is open. While the engine is not
    /// running, every connection is taken to have ended. A failed read keeps
    /// the last list: the engine's own status says what is wrong.
    pub async fn refresh_connections(&mut self) {
        if !self.engine_status.phase.is_running() {
            if self.connections.has_open() {
                self.connections.end_session();
                self.publish();
            }
            return;
        }
        if let Ok(reply) = self.engine.connections().await {
            self.connections.observe(&reply, now_unix());
            self.publish();
        }
    }

    /// Close one open connection by its id.
    pub async fn close_connection(&mut self, id: &str) -> Result<(), AppError> {
        let result = self.close_connection_inner(id).await;
        self.finish(result)
    }

    /// Close every open connection.
    pub async fn close_all_connections(&mut self) -> Result<(), AppError> {
        let result = self.close_all_connections_inner().await;
        self.finish(result)
    }

    /// Forget the closed connections the page lists. The open ones are kept.
    pub fn clear_closed_connections(&mut self) {
        self.connections.clear_closed();
        self.publish();
    }

    async fn close_connection_inner(&mut self, id: &str) -> Result<(), AppError> {
        if !self.refresh_engine_status().await.is_running() {
            return Err(AppError::Invalid("the proxy is not running".to_string()));
        }
        self.engine
            .close_connection(id)
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.refresh_connections().await;
        Ok(())
    }

    async fn close_all_connections_inner(&mut self) -> Result<(), AppError> {
        if !self.refresh_engine_status().await.is_running() {
            return Err(AppError::Invalid("the proxy is not running".to_string()));
        }
        self.engine
            .close_all_connections()
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.refresh_connections().await;
        Ok(())
    }

    /// Close the connections whose chain holds `group`, so they open again on
    /// the group's new member. Best effort: the switch has already worked, so
    /// a close that fails is logged and not returned.
    async fn close_connections_through(&mut self, group: &str) {
        if !self.engine_status.phase.is_running() {
            return;
        }
        self.refresh_connections().await;
        let ids = self.connections.ids_through(group);
        let mut closed = 0usize;
        for id in &ids {
            match self.engine.close_connection(id).await {
                Ok(()) => closed += 1,
                Err(error) => {
                    let text = error.to_string();
                    self.log(format!("could not close a connection: {text}"));
                }
            }
        }
        if closed > 0 {
            self.log(format!(
                "closed {closed} connections through “{group}” after the switch"
            ));
        }
        self.refresh_connections().await;
        self.publish();
    }

    /// Add a rule at the end of the user's list. Returns its id.
    pub async fn add_rule(&mut self, spec: RuleSpec) -> Result<String, AppError> {
        let result = self.add_rule_inner(spec).await;
        self.finish(result)
    }

    /// Replace a rule's shape and enabled flag.
    pub async fn update_rule(
        &mut self,
        id: &str,
        spec: RuleSpec,
        enabled: bool,
    ) -> Result<(), AppError> {
        let result = self.update_rule_inner(id, spec, enabled).await;
        self.finish(result)
    }

    /// Delete a rule.
    pub async fn delete_rule(&mut self, id: &str) -> Result<(), AppError> {
        let result = self.delete_rule_inner(id).await;
        self.finish(result)
    }

    /// Move a rule to a position in the user's list. Positions past the end
    /// move it to the end.
    pub async fn move_rule(&mut self, id: &str, to: usize) -> Result<(), AppError> {
        let result = self.move_rule_inner(id, to).await;
        self.finish(result)
    }

    /// Replace all the user's rules with the rules in `text`, one per line.
    /// Blank lines and `#` comments are skipped. If any line is invalid,
    /// nothing is saved and the error lists the line numbers. Returns how
    /// many rules were saved.
    pub async fn apply_raw_rules(&mut self, text: &str) -> Result<usize, AppError> {
        let result = self.apply_raw_rules_inner(text).await;
        self.finish(result)
    }

    /// Download and install the engine binary.
    pub async fn install_engine(&mut self) -> Result<(), AppError> {
        let result = self.install_engine_inner().await;
        self.finish(result)
    }

    // ----- implementations ---------------------------------------------------

    async fn add_subscription_inner(
        &mut self,
        label: String,
        url: String,
        header: Option<String>,
        mode: SubscriptionMode,
    ) -> Result<String, AppError> {
        let label = label.trim().to_string();
        if label.is_empty() {
            return Err(AppError::Invalid(
                "give the subscription a name".to_string(),
            ));
        }
        if label.chars().count() > MAX_LABEL_CHARS {
            return Err(AppError::Invalid(format!(
                "the name is longer than {MAX_LABEL_CHARS} characters"
            )));
        }
        let url = url.trim().to_string();
        if !is_subscription_url(&url) {
            return Err(AppError::Invalid(
                "the subscription address must be an http:// or https:// address with a host"
                    .to_string(),
            ));
        }
        let header = header
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let id = new_id();
        let url_key = subscription_url_key(&id);
        let header_key = subscription_header_key(&id);

        self.write_secret(&url_key, &url)?;
        self.redactor.add_secret(url);
        if let Some(value) = &header {
            if let Err(error) = self.write_secret(&header_key, value) {
                let _ = self.secrets.remove(&url_key);
                return Err(error);
            }
            self.redactor.add_secret(value.clone());
        }

        let mut meta = self.meta.clone();
        meta.subscriptions.push(Subscription {
            id: id.clone(),
            label: label.clone(),
            mode,
            interval_secs: DEFAULT_INTERVAL_SECS,
            has_auth_header: header.is_some(),
            last_updated: None,
            quota: None,
            node_count: 0,
            group_count: 0,
            last_error: None,
            profile_title: None,
            announce: None,
            support_url: None,
            web_url: None,
            web_page_name: None,
            announce_url: None,
            logo_file: None,
            global_mode_disabled: false,
            expand_proxy_groups: None,
            device_limit: false,
            local: false,
        });
        meta.url_keys.push(SecretRef {
            subscription_id: id.clone(),
            key: url_key,
        });
        if header.is_some() {
            meta.header_keys.push(SecretRef {
                subscription_id: id.clone(),
                key: header_key,
            });
        }

        // A new subscription is the one the user wants to use. An app-managed
        // one has no servers until its first refresh, so while another one
        // is usable it is put in use only then (see `refresh_inner`).
        let mut state = self.state.clone();
        if mode == SubscriptionMode::EngineManaged || self.connect_block().is_some() {
            state.selected_subscription = Some(id.clone());
        } else {
            self.activate_after_refresh = Some(id.clone());
        }
        let caches = self.caches.clone();
        if let Err(error) = self.commit(state, meta, caches).await {
            // A storage failure leaves the subscription recorded, so its
            // secrets must stay. Any other failure means it was never added.
            if !matches!(error, AppError::Storage(_)) {
                self.forget_secrets(&id);
            }
            return Err(error);
        }
        self.log(format!("added the subscription “{label}”"));
        Ok(id)
    }

    async fn add_local_inner(&mut self, label: String, source: String) -> Result<String, AppError> {
        let label = label.trim().to_string();
        let label = if label.is_empty() {
            "My profile".to_string()
        } else {
            label
        };
        if label.chars().count() > MAX_LABEL_CHARS {
            return Err(AppError::Invalid(format!(
                "the name is longer than {MAX_LABEL_CHARS} characters"
            )));
        }
        if source.trim().is_empty() {
            return Err(AppError::Invalid("paste a configuration first".to_string()));
        }

        let id = new_id();
        let payload = parse_payload(source.as_bytes(), &id, &label)
            .map_err(invalid_config)?
            .payload;
        let now = now_unix();
        self.store
            .write_profile_source(&id, &source)
            .map_err(|error| AppError::Storage(self.scrub(&error.to_string())))?;

        let mut meta = self.meta.clone();
        meta.subscriptions.push(Subscription {
            id: id.clone(),
            label: label.clone(),
            mode: SubscriptionMode::AppManaged,
            interval_secs: DEFAULT_INTERVAL_SECS,
            has_auth_header: false,
            last_updated: timestamp::format(now),
            quota: None,
            node_count: payload.nodes.len(),
            group_count: payload.groups.len(),
            last_error: None,
            profile_title: None,
            announce: None,
            support_url: None,
            web_url: None,
            web_page_name: None,
            announce_url: None,
            logo_file: None,
            global_mode_disabled: false,
            expand_proxy_groups: None,
            device_limit: false,
            local: true,
        });

        let mut state = self.state.clone();
        state.selected_subscription = Some(id.clone());
        let mut caches = self.caches.clone();
        caches.store(&id, payload.clone(), now);
        if let Err(error) = self.commit(state, meta, caches).await {
            // A storage failure leaves the profile recorded, so its source must
            // stay. Any other failure means it was never added.
            if !matches!(error, AppError::Storage(_)) {
                let _ = self.store.remove_profile_source(&id);
            }
            return Err(error);
        }
        self.persist_payload_cache(&id, &payload);
        self.log(format!(
            "added the pasted configuration “{label}”: {} nodes",
            payload.nodes.len()
        ));
        Ok(id)
    }

    async fn save_profile_source_inner(&mut self, id: &str, text: String) -> Result<(), AppError> {
        let subscription = self
            .meta
            .subscriptions
            .iter()
            .find(|subscription| subscription.id == id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("that subscription".to_string()))?;
        let label = subscription.label.clone();
        if text.trim().is_empty() {
            return Err(AppError::Invalid("the configuration is empty".to_string()));
        }

        let mut payload = if subscription.local {
            parse_payload(text.as_bytes(), id, &label)
                .map_err(invalid_config)?
                .payload
        } else {
            // The app's own cached JSON is taken as it is; anything else is
            // read as a configuration, as a refresh would read it.
            match serde_json::from_str::<SubscriptionPayload>(&text) {
                Ok(payload) => payload,
                Err(_) => {
                    parse_payload(text.as_bytes(), id, &label)
                        .map_err(invalid_config)?
                        .payload
                }
            }
        };
        payload.subscription_id = id.to_string();
        if payload.nodes.is_empty() {
            return Err(AppError::Invalid(
                "the configuration has no servers".to_string(),
            ));
        }

        let now = now_unix();
        let mut meta = self.meta.clone();
        if let Some(entry) = meta.subscriptions.iter_mut().find(|entry| entry.id == id) {
            record_payload_counts(entry, &payload, now);
        }
        let state = self.state.clone();
        let mut caches = self.caches.clone();
        caches.store(id, payload.clone(), now);
        // Committed before the source is written: a rejected candidate must
        // leave the saved source as it was.
        self.commit(state, meta, caches).await?;
        self.persist_payload_cache(id, &payload);
        if subscription.local {
            self.store
                .write_profile_source(id, &text)
                .map_err(|error| {
                    AppError::Storage(format!(
                        "the change is in effect but could not be saved: {}",
                        self.scrub(&error.to_string())
                    ))
                })?;
        }
        self.log(format!(
            "saved the configuration of “{label}”: {} nodes",
            payload.nodes.len()
        ));
        Ok(())
    }

    /// Re-read and re-parse the pasted source of a local profile. Nothing is
    /// fetched, so a cleared cache is rebuilt from the source file.
    async fn refresh_local_inner(&mut self, subscription: Subscription) -> Result<(), AppError> {
        let id = subscription.id.clone();
        let label = subscription.label.clone();
        let source = match self.store.read_profile_source(&id) {
            Ok(Some(text)) => text,
            Ok(None) => return Err(self.record_refresh_failure(&id, missing_source(&label))),
            Err(error) => {
                let error = AppError::Storage(self.scrub(&error.to_string()));
                return Err(self.record_refresh_failure(&id, error));
            }
        };
        let payload = match parse_payload(source.as_bytes(), &id, &label) {
            Ok(parsed) => parsed.payload,
            Err(error) => {
                let error = AppError::Subscription(format!("“{label}” could not be read: {error}"));
                return Err(self.record_refresh_failure(&id, error));
            }
        };

        let now = now_unix();
        let mut meta = self.meta.clone();
        if let Some(entry) = meta.subscriptions.iter_mut().find(|entry| entry.id == id) {
            record_payload_counts(entry, &payload, now);
        }
        let state = self.state.clone();
        let mut caches = self.caches.clone();
        caches.store(&id, payload.clone(), now);
        if let Err(error) = self.commit(state, meta, caches).await {
            return Err(self.record_refresh_failure(&id, error));
        }
        self.persist_payload_cache(&id, &payload);
        self.log(format!(
            "re-read “{label}”: {} nodes, {} groups",
            payload.nodes.len(),
            payload.groups.len()
        ));
        Ok(())
    }

    /// Write the cached payload of a subscription after its commit. A failed
    /// write is logged, not returned: the cache can always be rebuilt, and a
    /// local profile is rebuilt from its source on the next refresh.
    fn persist_payload_cache(&mut self, id: &str, payload: &SubscriptionPayload) {
        let written = serde_json::to_vec(payload)
            .map_err(|error| error.to_string())
            .and_then(|bytes| {
                self.store
                    .write_subscription_cache(id, &bytes)
                    .map_err(|error| error.to_string())
            });
        if let Err(message) = written {
            let text = self.scrub(&message);
            self.log(format!("could not save the parsed nodes: {text}"));
        }
    }

    async fn remove_subscription_inner(&mut self, id: &str) -> Result<(), AppError> {
        let mut meta = self.meta.clone();
        let position = meta
            .subscriptions
            .iter()
            .position(|subscription| subscription.id == id)
            .ok_or_else(|| AppError::NotFound("that subscription".to_string()))?;
        let removed = meta.subscriptions.remove(position);
        meta.url_keys.retain(|key| key.subscription_id != id);
        meta.header_keys.retain(|key| key.subscription_id != id);

        let mut state = self.state.clone();
        if state.selected_subscription.as_deref() == Some(id) {
            state.selected_subscription = None;
        }
        if let Some(selected) = state.preferences.selected_node_id.clone() {
            let from_removed = self
                .caches
                .payload(id)
                .is_some_and(|payload| payload.nodes.iter().any(|node| node.id == selected));
            if from_removed {
                state.preferences.selected_node_id = None;
            }
        }

        let mut caches = self.caches.clone();
        caches.remove(id);

        self.commit(state, meta, caches).await?;

        if let Err(error) = self.store.remove_subscription_cache(id) {
            let text = self.scrub(&error.to_string());
            self.log(format!(
                "could not delete the saved nodes of “{}”: {text}",
                removed.label
            ));
        }
        if let Err(error) = self.store.remove_profile_source(id) {
            let text = self.scrub(&error.to_string());
            self.log(format!(
                "could not delete the pasted configuration of “{}”: {text}",
                removed.label
            ));
        }
        self.forget_secrets(id);
        self.log(format!("removed the subscription “{}”", removed.label));
        Ok(())
    }

    async fn set_active_subscription_inner(&mut self, id: &str) -> Result<(), AppError> {
        let subscription = self
            .meta
            .subscriptions
            .iter()
            .find(|subscription| subscription.id == id)
            .ok_or_else(|| AppError::NotFound("that subscription".to_string()))?;
        let label = subscription.label.clone();
        if active_subscription_id(&self.state, &self.meta) == Some(id) {
            return Ok(());
        }
        let mut state = self.state.clone();
        state.selected_subscription = Some(id.to_string());
        let meta = self.meta.clone();
        let caches = self.caches.clone();
        self.commit(state, meta, caches).await?;
        self.log(format!("now using the subscription “{label}”"));
        Ok(())
    }

    async fn set_subscription_mode_inner(
        &mut self,
        id: &str,
        mode: SubscriptionMode,
    ) -> Result<(), AppError> {
        let mut meta = self.meta.clone();
        let subscription = meta
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == id)
            .ok_or_else(|| AppError::NotFound("that subscription".to_string()))?;
        subscription.mode = mode;
        if mode == SubscriptionMode::EngineManaged {
            // The app cannot see an engine-managed payload, so it must not
            // report a quota or a node count for one.
            subscription.quota = None;
            subscription.node_count = 0;
            subscription.group_count = 0;
            subscription.profile_title = None;
            subscription.announce = None;
            subscription.support_url = None;
            subscription.web_url = None;
            subscription.web_page_name = None;
            subscription.announce_url = None;
            subscription.logo_file = None;
            subscription.global_mode_disabled = false;
            subscription.expand_proxy_groups = None;
            subscription.device_limit = false;
        }
        let state = self.state.clone();
        let caches = self.caches.clone();
        self.commit(state, meta, caches).await
    }

    async fn refresh_inner(&mut self, id: &str) -> Result<(), AppError> {
        let subscription = self
            .meta
            .subscriptions
            .iter()
            .find(|subscription| subscription.id == id)
            .cloned()
            .ok_or_else(|| AppError::NotFound("that subscription".to_string()))?;
        if subscription.mode != SubscriptionMode::AppManaged {
            return Err(AppError::Invalid(format!(
                "“{}” is managed by the engine, which refreshes it itself",
                subscription.label
            )));
        }

        if subscription.local {
            return self.refresh_local_inner(subscription).await;
        }

        let url = self.read_secret(&subscription_url_key(id))?;
        let mut request = FetchRequest::new(url);
        if subscription.has_auth_header {
            let header = self.read_secret(&subscription_header_key(id))?;
            request = request.with_authorization(header);
        }
        // Device headers go out only when the user has not turned them off.
        if self.state.preferences.send_device_id {
            request = request.with_device(self.device.headers());
        }

        let mut metadata =
            SubscriptionMetadata::new(subscription.id.clone(), subscription.label.clone());
        metadata.mode = subscription.mode;
        metadata.interval_secs = subscription.interval_secs;
        metadata.last_updated = subscription
            .last_updated
            .as_deref()
            .and_then(timestamp::parse);

        let now = now_unix();
        let mut scratch = self.caches.get(id).cloned().unwrap_or_default();
        let provider = Arc::clone(&self.provider);
        let outcome = refresh(provider.as_ref(), &request, &metadata, &mut scratch, now).await;

        match outcome {
            RefreshOutcome::Refreshed {
                payload,
                quota,
                profile,
                ..
            } => {
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|error| AppError::Storage(error.to_string()))?;
                let node_count = payload.nodes.len();
                let group_count = payload.groups.len();

                let mut caches = self.caches.clone();
                caches.store(id, *payload, now);

                let logo_file = self
                    .keep_logo(
                        id,
                        profile.logo_url.as_deref(),
                        subscription.logo_file.as_deref(),
                    )
                    .await;

                let mut meta = self.meta.clone();
                if let Some(entry) = meta.subscriptions.iter_mut().find(|entry| entry.id == id) {
                    entry.last_updated = timestamp::format(now);
                    entry.last_error = None;
                    entry.quota = quota.as_ref().map(core_quota);
                    entry.node_count = node_count;
                    entry.group_count = group_count;
                    // A successful refresh replaces the profile headers; an
                    // absent header clears its field.
                    entry.profile_title = profile.title;
                    entry.announce = profile.announce;
                    entry.support_url = profile.support_url;
                    entry.web_url = profile.web_url;
                    entry.web_page_name = profile.web_page_name;
                    entry.announce_url = profile.announce_url;
                    entry.global_mode_disabled = profile.global_mode_disabled;
                    entry.expand_proxy_groups = profile.expand_proxy_groups;
                    // The provider's update interval, when it sent one, becomes
                    // this subscription's interval. A user's own auto-refresh
                    // setting still takes precedence when it is set.
                    entry.interval_secs = profile
                        .update_interval_hours
                        .map_or(DEFAULT_INTERVAL_SECS, |hours| hours.saturating_mul(3600));
                    entry.device_limit = false;
                    entry.logo_file = logo_file;
                }

                // The refresh never writes user state: the state passed here
                // is the one it already holds, so `commit` leaves it alone.
                let state = self.state.clone();
                if let Err(error) = self.commit(state, meta, caches).await {
                    return Err(self.record_refresh_failure(id, error));
                }

                self.store
                    .write_subscription_cache(id, &bytes)
                    .map_err(|error| AppError::Storage(self.scrub(&error.to_string())))?;
                self.log(format!(
                    "refreshed “{}”: {node_count} nodes, {group_count} groups",
                    subscription.label
                ));
                // The first refresh of a subscription the user just added puts
                // it in use. This is the user's choice made when adding it, not
                // the refresh writing user state.
                if self.activate_after_refresh.as_deref() == Some(id) && node_count > 0 {
                    self.activate_after_refresh = None;
                    self.set_active_subscription_inner(id).await?;
                }
                Ok(())
            }
            RefreshOutcome::Failed { error, profile, .. } => {
                let message = self.scrub(&error.to_string());
                let app_error = AppError::Subscription(format!(
                    "“{}” could not be refreshed: {message}",
                    subscription.label
                ));
                // A device-limit refusal keeps the cached payload, but the
                // provider's support link must be shown, so the headers it sent
                // are recorded.
                if matches!(error, SubscriptionError::DeviceLimitReached) {
                    let mut meta = self.meta.clone();
                    if let Some(entry) = meta.subscriptions.iter_mut().find(|entry| entry.id == id)
                    {
                        entry.device_limit = true;
                        if let Some(profile) = profile.as_deref()
                            && profile.support_url.is_some()
                        {
                            entry.support_url = profile.support_url.clone();
                        }
                    }
                    let state = self.state.clone();
                    let caches = self.caches.clone();
                    if let Err(error) = self.commit(state, meta, caches).await {
                        return Err(self.record_refresh_failure(id, error));
                    }
                }
                Err(self.record_refresh_failure(id, app_error))
            }
        }
    }

    /// Download the profile logo and keep it in place of the previous one.
    ///
    /// Returns the file name to record. A provider that sends no logo clears
    /// it. A logo that cannot be kept leaves the previous one in place: a
    /// failed download never blocks the refresh. The URL is never logged,
    /// because it may carry a token.
    async fn keep_logo(
        &mut self,
        id: &str,
        url: Option<&str>,
        previous: Option<&str>,
    ) -> Option<String> {
        let Some(url) = url else {
            if let Err(error) = self.store.remove_subscription_logos(id) {
                let text = self.scrub(&error.to_string());
                self.log(format!("could not delete a cached logo: {text}"));
            }
            return None;
        };
        match fetch_logo(url).await {
            Ok(logo) => {
                match self
                    .store
                    .write_subscription_logo(id, logo.format.extension(), &logo.bytes)
                {
                    Ok(path) => path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned()),
                    Err(error) => {
                        let text = self.scrub(&error.to_string());
                        self.log(format!("could not keep the profile logo: {text}"));
                        previous.map(str::to_string)
                    }
                }
            }
            Err(_) => {
                self.log("the profile logo could not be downloaded; the previous one is kept");
                previous.map(str::to_string)
            }
        }
    }

    async fn refresh_all_inner(&mut self) -> Result<usize, AppError> {
        // A local profile is re-read from its source here too, so a cleared
        // cache is rebuilt, but it is never counted: nothing is fetched, and
        // its own status records any problem.
        let ids: Vec<(String, bool)> = self
            .meta
            .subscriptions
            .iter()
            .filter(|subscription| subscription.mode == SubscriptionMode::AppManaged)
            .map(|subscription| (subscription.id.clone(), subscription.local))
            .collect();
        let attempted = ids.iter().filter(|(_, local)| !local).count();
        let mut succeeded = 0usize;
        let mut last_error = None;
        for (id, local) in ids {
            match self.refresh_inner(&id).await {
                Ok(()) if !local => succeeded += 1,
                Err(error) if !local => last_error = Some(error),
                _ => {}
            }
        }
        let failed = attempted - succeeded;
        if failed > 0 {
            self.notice = Some((
                NoticeKind::Warning,
                format!(
                    "{failed} of {attempted} subscriptions could not be refreshed; their last working data is still in use"
                ),
            ));
        }
        if succeeded == 0
            && let Some(error) = last_error
        {
            return Err(error);
        }
        Ok(succeeded)
    }

    async fn clear_cache_inner(&mut self) -> Result<(), AppError> {
        let mut meta = self.meta.clone();
        for subscription in &mut meta.subscriptions {
            subscription.last_updated = None;
            subscription.node_count = 0;
            subscription.group_count = 0;
            subscription.last_error = None;
        }
        let state = self.state.clone();
        self.commit(state, meta, SubscriptionCacheSet::new())
            .await?;

        let ids = self
            .store
            .cached_subscription_ids()
            .map_err(|error| AppError::Storage(self.scrub(&error.to_string())))?;
        for id in ids {
            if let Err(error) = self.store.remove_subscription_cache(&id) {
                let text = self.scrub(&error.to_string());
                self.log(format!("could not delete a cached payload: {text}"));
            }
        }
        self.log("cleared the cached subscription data");
        Ok(())
    }

    async fn check_inner(&mut self) -> Result<ConfigCheck, AppError> {
        let composed = self.compose_current()?;
        let report = self
            .engine
            .validate(&composed.config)
            .await
            .map_err(|error| self.engine_failure(error))?;
        let mut warnings = composed.warnings;
        warnings.extend(report.warnings.iter().map(|warning| self.scrub(warning)));
        Ok(ConfigCheck {
            accepted: report.ok,
            engine_error: report
                .engine_error
                .as_deref()
                .map(|message| self.scrub(message)),
            warnings,
        })
    }

    async fn connect_inner(&mut self) -> Result<(), AppError> {
        self.ensure_connectable()?;
        if self.refresh_engine_status().await.is_running() {
            return Ok(());
        }
        // On a phone the VPN permission comes first: without it nothing
        // would reach the engine.
        if let Some(tunnel) = self.phone_tunnel.clone() {
            tunnel.prepare().await.map_err(AppError::Invalid)?;
        }
        let composed = self.compose_current()?;
        self.adopt(&composed);

        let report = self
            .engine
            .validate(&composed.config)
            .await
            .map_err(|error| self.engine_failure(error))?;
        if !report.ok {
            return Err(self.rejected(&report));
        }

        let handle = self
            .engine
            .start(&composed.config)
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.handle = Some(handle);
        self.running_config = Some(composed.config.clone());
        self.engine_error = None;
        self.refresh_engine_status().await;
        self.assert_default_selection(composed.default_selection.as_ref())
            .await;
        self.log("the proxy started");
        self.sync_system_proxy().await;
        Ok(())
    }

    /// Disconnect is never blocked by the connect rule: stopping a proxy is
    /// always allowed.
    async fn disconnect_inner(&mut self) -> Result<(), AppError> {
        if self.refresh_engine_status().await == EnginePhase::Stopped {
            return Ok(());
        }
        self.stop_engine().await?;
        self.log("the proxy stopped");
        Ok(())
    }

    /// Stop the engine and forget what it was doing. Shared by Disconnect and
    /// by the path that stops a proxy whose last usable subscription is gone.
    async fn stop_engine(&mut self) -> Result<(), AppError> {
        self.engine
            .stop()
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.handle = None;
        self.running_config = None;
        self.engine_error = None;
        self.refresh_engine_status().await;
        // Latencies were measured against a proxy that is now gone.
        self.latencies.clear();
        self.sync_system_proxy().await;
        Ok(())
    }

    async fn restart_inner(&mut self) -> Result<(), AppError> {
        // Checked before the stop, so a blocked restart leaves the proxy as it was.
        self.ensure_connectable()?;
        self.disconnect_inner().await?;
        self.connect_inner().await
    }

    async fn set_mode_inner(&mut self, mode: ProxyMode) -> Result<(), AppError> {
        if mode == ProxyMode::Global
            && global_mode_blocked(&only_active(&self.state, &self.meta), &self.caches)
        {
            return Err(AppError::Invalid(
                "your subscription does not allow the Global mode".to_string(),
            ));
        }
        let mut state = self.state.clone();
        state.preferences.mode = Some(mode);
        self.commit_user(state).await
    }

    async fn save_preferences_inner(&mut self, prefs: UserPreferences) -> Result<(), AppError> {
        let mut state = self.state.clone();
        state.preferences = prefs;
        self.commit_user(state).await
    }

    async fn select_node_inner(&mut self, node_id: &str) -> Result<(), AppError> {
        let current = self.compose_current()?;
        let node_name = current
            .config
            .proxies
            .iter()
            .find(|node| node.id == node_id)
            .map(|node| node.name.clone())
            .ok_or_else(|| AppError::NotFound("that node".to_string()))?;

        let mut state = self.state.clone();
        state.preferences.selected_node_id = Some(node_id.to_string());
        // The choice is also recorded for the group it is made in, so it
        // survives a rebuild the same way a group pick does. This is the group
        // the build moves the node to the front of.
        let legacy_group = state
            .preferences
            .selected_group
            .clone()
            .unwrap_or_else(|| DEFAULT_GROUP_NAME.to_string());
        if current
            .config
            .proxy_groups
            .iter()
            .any(|group| group.name == legacy_group && group.members.contains(&node_name))
        {
            state
                .preferences
                .group_selections
                .insert(legacy_group, node_name.clone());
        }
        let meta = self.meta.clone();
        let caches = self.caches.clone();
        self.check_writable(&state, &meta)?;
        let composed = self.compose_for(&state, &meta, &caches)?;

        // The running engine switches now, through its own selection call, so
        // no reload is needed and open connections survive. A failed switch
        // saves nothing, so the saved choice and the running one never differ.
        if self.refresh_engine_status().await.is_running() {
            let group = live_group(
                &composed.config,
                &node_name,
                state.preferences.selected_group.as_deref(),
            );
            if let Some(group) = group {
                self.engine
                    .select(&group, &node_name)
                    .await
                    .map_err(|error| self.engine_failure(error))?;
            }
        }

        self.persist_and_adopt(state, meta, caches, &composed)
    }

    async fn select_member_inner(&mut self, group: &str, member: &str) -> Result<(), AppError> {
        let current = self.compose_current()?;
        let proxy_group = current
            .config
            .proxy_groups
            .iter()
            .find(|candidate| candidate.name == group)
            .ok_or_else(|| AppError::Invalid(format!("“{group}” is not in the configuration")))?;
        if proxy_group.group_type != "select" {
            return Err(AppError::Invalid(format!(
                "“{group}” chooses its server automatically"
            )));
        }
        if !proxy_group.members.iter().any(|name| name == member) {
            return Err(AppError::Invalid(format!(
                "“{member}” is not a server in “{group}”"
            )));
        }

        let mut state = self.state.clone();
        state
            .preferences
            .group_selections
            .insert(group.to_string(), member.to_string());
        let meta = self.meta.clone();
        let caches = self.caches.clone();
        self.check_writable(&state, &meta)?;
        let composed = self.compose_for(&state, &meta, &caches)?;

        // As for a node: the running engine switches now, and a failed switch
        // saves nothing.
        if self.refresh_engine_status().await.is_running() {
            self.engine
                .select(group, member)
                .await
                .map_err(|error| self.engine_failure(error))?;
            self.refresh_engine_status().await;
        }

        let saved = self.persist_and_adopt(state, meta, caches, &composed);
        if saved.is_ok() && self.state.preferences.auto_close_connections_on_switch {
            self.close_connections_through(group).await;
        }
        saved
    }

    async fn test_latency_inner(&mut self) -> Result<(), AppError> {
        if !self.refresh_engine_status().await.is_running() {
            // Not connected: ping each server directly instead.
            let nodes = self.compose_current()?.config.proxies;
            let total = nodes.len();
            let timeout = self.state.preferences.effective_delay_test_timeout_ms();
            let results = tcp_ping(nodes, timeout).await;
            let answered = results.values().filter(|latency| latency.is_some()).count();
            self.latencies = results;
            self.log(format!(
                "latency test (direct ping, not connected): {answered} of {total} nodes answered"
            ));
            return Ok(());
        }
        let nodes: Vec<(String, String)> = self
            .built
            .as_ref()
            .map(|config| {
                config
                    .proxies
                    .iter()
                    .map(|node| (node.id.clone(), node.name.clone()))
                    .collect()
            })
            .unwrap_or_default();

        let url = self
            .state
            .preferences
            .effective_delay_test_url()
            .to_string();
        let timeout = self.state.preferences.effective_delay_test_timeout_ms();
        let mut results = BTreeMap::new();
        for (id, name) in nodes {
            let latency = self.engine.delay(&name, &url, timeout).await.ok();
            results.insert(id, latency);
        }
        let answered = results.values().filter(|latency| latency.is_some()).count();
        let total = results.len();
        self.latencies = results;
        self.log(format!(
            "latency test: {answered} of {total} nodes answered"
        ));
        Ok(())
    }

    /// Measure every member of a proxy group against the running engine, and
    /// merge the results into the node latencies. Returns how many members
    /// answered.
    pub async fn test_group(&mut self, group: &str) -> Result<usize, AppError> {
        let result = self.test_group_inner(group).await;
        self.finish(result)
    }

    async fn test_group_inner(&mut self, group: &str) -> Result<usize, AppError> {
        if !self.refresh_engine_status().await.is_running() {
            // Not connected: ping the group's servers directly instead.
            let config = self.compose_current()?.config;
            let members = config
                .proxy_groups
                .iter()
                .find(|candidate| candidate.name == group)
                .map(|candidate| candidate.members.clone())
                .ok_or_else(|| AppError::Invalid(format!("there is no group named “{group}”")))?;
            let nodes: Vec<Node> = config
                .proxies
                .into_iter()
                .filter(|node| members.contains(&node.name))
                .collect();
            let timeout = self.state.preferences.effective_delay_test_timeout_ms();
            let results = tcp_ping(nodes, timeout).await;
            let count = results.values().filter(|latency| latency.is_some()).count();
            self.latencies.extend(results);
            self.log(format!(
                "group latency test “{group}” (direct ping, not connected): {count} of {} members answered",
                members.len()
            ));
            return Ok(count);
        }
        let (members, proxies) = {
            let Some(config) = self.built.as_ref() else {
                return Err(AppError::Invalid(
                    "Connect first to test latency".to_string(),
                ));
            };
            let members = config
                .proxy_groups
                .iter()
                .find(|candidate| candidate.name == group)
                .map(|candidate| candidate.members.clone())
                .ok_or_else(|| AppError::Invalid(format!("there is no group named “{group}”")))?;
            let proxies: Vec<(String, String)> = config
                .proxies
                .iter()
                .map(|node| (node.id.clone(), node.name.clone()))
                .collect();
            (members, proxies)
        };
        let url = self
            .state
            .preferences
            .effective_delay_test_url()
            .to_string();
        let timeout = self.state.preferences.effective_delay_test_timeout_ms();
        let answered = self
            .engine
            .group_delay(group, &url, timeout)
            .await
            .map_err(|error| self.engine_failure(error))?;

        let mut count = 0;
        for member in &members {
            // A group can name another group; only nodes carry a latency.
            let Some((id, _)) = proxies.iter().find(|(_, name)| name == member) else {
                continue;
            };
            let latency = answered.get(member).copied();
            if latency.is_some() {
                count += 1;
            }
            self.latencies.insert(id.clone(), latency);
        }
        self.log(format!(
            "group latency test “{group}”: {count} of {} members answered",
            members.len()
        ));
        Ok(count)
    }

    /// The installed engine, the newest one on the chosen channel, and whether
    /// they differ.
    fn engine_update_view(&self) -> crate::snapshot::EngineUpdateView {
        let latest = self.latest_engine.clone();
        let update_available = match (&latest, &self.installed_engine) {
            (Some(latest), Some(installed)) => latest != installed,
            (Some(_), None) => true,
            _ => false,
        };
        crate::snapshot::EngineUpdateView {
            installed_version: self.installed_engine.clone(),
            latest_version: latest,
            update_available,
            channel: self.state.preferences.effective_engine_channel(),
        }
    }

    /// Ask the release feed for the newest engine on the chosen channel.
    pub async fn check_engine_update(&mut self) -> Result<(), AppError> {
        let result = self.check_engine_update_inner().await;
        self.finish(result)
    }

    async fn check_engine_update_inner(&mut self) -> Result<(), AppError> {
        if starpod_engine::download::is_bundled() {
            return Err(AppError::Invalid(
                "the engine comes with the app and is updated with it".to_string(),
            ));
        }
        let channel = self.state.preferences.effective_engine_channel();
        let latest = starpod_engine::download::latest_release(channel)
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.log(format!(
            "the newest engine on this channel is {}",
            latest.version
        ));
        self.latest_engine = Some(latest.version);
        Ok(())
    }

    /// Install the newest engine on the chosen channel.
    ///
    /// The archive is verified against its checksum before anything is
    /// replaced. A running proxy is stopped for the install and started again
    /// on the new engine; if the install fails, it is started again on the
    /// old one.
    pub async fn upgrade_engine(&mut self) -> Result<(), AppError> {
        let result = self.upgrade_engine_inner().await;
        self.finish(result)
    }

    async fn upgrade_engine_inner(&mut self) -> Result<(), AppError> {
        if starpod_engine::download::is_bundled() {
            return Err(AppError::Invalid(
                "the engine comes with the app and is updated with it".to_string(),
            ));
        }
        let channel = self.state.preferences.effective_engine_channel();
        let release = starpod_engine::download::latest_release(channel)
            .await
            .map_err(|error| self.engine_failure(error))?;
        if self.installed_engine.as_deref() == Some(release.version.as_str()) {
            self.latest_engine = Some(release.version.clone());
            self.log(format!("the engine is already {}", release.version));
            return Ok(());
        }

        let was_running = self.refresh_engine_status().await.is_running();
        if was_running {
            self.log("stopping the proxy to install the engine");
            self.stop_engine().await?;
        }
        self.log(format!("installing the engine {}", release.version));
        let installed = starpod_engine::download::install_asset_in(
            self.paths().data_dir(),
            &release.asset,
            &release.version,
        )
        .await;
        match installed {
            Ok(path) => {
                self.engine_installed = true;
                self.installed_engine = Some(release.version.clone());
                self.latest_engine = Some(release.version.clone());
                self.log(format!(
                    "installed the engine {} at {}",
                    release.version,
                    path.display()
                ));
            }
            Err(error) => {
                let error = self.engine_failure(error);
                if was_running {
                    // The old binary is still in place: the install writes only
                    // after the checksum passes.
                    let _ = self.connect_inner().await;
                }
                return Err(error);
            }
        }
        if was_running {
            self.connect_inner().await?;
        }
        Ok(())
    }

    /// Download the geodata databases and reload the engine so it reads them.
    ///
    /// The databases are written to the app's data directory. The refresh
    /// time is saved with the preferences, through the same path as any other
    /// change, so a running engine is reloaded.
    pub async fn update_geo_data(&mut self) -> Result<(), AppError> {
        let result = self.update_geo_data_inner().await;
        self.finish(result)
    }

    async fn update_geo_data_inner(&mut self) -> Result<(), AppError> {
        let sources = self.state.preferences.engine.geo.geox_url.clone();
        let directory = self.paths().data_dir().to_path_buf();
        let downloads = [
            (
                sources
                    .mmdb
                    .clone()
                    .unwrap_or_else(|| DEFAULT_GEO_MMDB_URL.to_string()),
                "country.mmdb",
            ),
            (
                sources
                    .geosite
                    .clone()
                    .unwrap_or_else(|| DEFAULT_GEOSITE_URL.to_string()),
                "geosite.dat",
            ),
            (
                sources
                    .asn
                    .clone()
                    .unwrap_or_else(|| DEFAULT_GEO_ASN_URL.to_string()),
                "GeoLite2-ASN.mmdb",
            ),
        ];
        self.log("updating the geodata");
        for (url, file) in downloads {
            starpod_engine::download::download_file(&url, &directory.join(file))
                .await
                .map_err(|error| self.engine_failure(error))?;
            self.log(format!("downloaded {file}"));
        }
        let mut state = self.state.clone();
        state.preferences.geo_updated_at = Some(now_unix());
        self.commit_user(state).await?;
        self.log("the geodata is updated");
        Ok(())
    }

    async fn add_rule_inner(&mut self, spec: RuleSpec) -> Result<String, AppError> {
        let id = new_id();
        let mut state = self.state.clone();
        state.rules.push(StoredRule {
            id: id.clone(),
            spec,
            enabled: true,
            order: 0,
        });
        renumber(&mut state.rules);
        self.commit_user(state).await?;
        Ok(id)
    }

    async fn update_rule_inner(
        &mut self,
        id: &str,
        spec: RuleSpec,
        enabled: bool,
    ) -> Result<(), AppError> {
        let mut state = self.state.clone();
        let rule = state
            .rules
            .iter_mut()
            .find(|rule| rule.id == id)
            .ok_or_else(|| AppError::NotFound("that rule".to_string()))?;
        rule.spec = spec;
        rule.enabled = enabled;
        self.commit_user(state).await
    }

    async fn delete_rule_inner(&mut self, id: &str) -> Result<(), AppError> {
        let mut state = self.state.clone();
        let position = state
            .rules
            .iter()
            .position(|rule| rule.id == id)
            .ok_or_else(|| AppError::NotFound("that rule".to_string()))?;
        state.rules.remove(position);
        renumber(&mut state.rules);
        self.commit_user(state).await
    }

    async fn move_rule_inner(&mut self, id: &str, to: usize) -> Result<(), AppError> {
        let mut state = self.state.clone();
        let position = state
            .rules
            .iter()
            .position(|rule| rule.id == id)
            .ok_or_else(|| AppError::NotFound("that rule".to_string()))?;
        let rule = state.rules.remove(position);
        let target = to.min(state.rules.len());
        state.rules.insert(target, rule);
        renumber(&mut state.rules);
        self.commit_user(state).await
    }

    async fn apply_raw_rules_inner(&mut self, text: &str) -> Result<usize, AppError> {
        let mut specs = Vec::new();
        let mut errors = Vec::new();
        for (number, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match parse_line(line) {
                Ok(spec) => specs.push(spec),
                Err(reason) => errors.push(RuleLineError {
                    line: number + 1,
                    reason: self.scrub(&reason),
                }),
            }
        }
        if !errors.is_empty() {
            return Err(AppError::RuleLines(errors));
        }

        let count = specs.len();
        let mut state = self.state.clone();
        state.rules = specs
            .into_iter()
            .enumerate()
            .map(|(index, spec)| StoredRule {
                id: new_id(),
                spec,
                enabled: true,
                order: u32::try_from(index).unwrap_or(u32::MAX),
            })
            .collect();
        self.commit_user(state).await?;
        Ok(count)
    }

    async fn install_engine_inner(&mut self) -> Result<(), AppError> {
        self.log("installing the proxy engine");
        let path = starpod_engine::download::install_in(self.paths().data_dir())
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.engine_installed = true;
        self.installed_engine = Some(starpod_engine::download::ENGINE_VERSION.to_string());
        self.log(format!("installed the proxy engine at {}", path.display()));
        Ok(())
    }

    // ----- the single write path ---------------------------------------------

    /// Apply and persist a change to the user's state.
    async fn commit_user(&mut self, state: UserState) -> Result<(), AppError> {
        let meta = self.meta.clone();
        let caches = self.caches.clone();
        self.commit(state, meta, caches).await
    }

    /// The one path every change takes.
    ///
    /// 1. Refuse if a changed file is one this build must not overwrite.
    /// 2. Build the candidate. Errors apply nothing.
    /// 3. If the engine runs, validate the candidate. A rejection applies
    ///    nothing. An accepted candidate is reloaded.
    /// 4. Persist whatever changed, then make the new state current.
    ///
    /// A failure at step 4 leaves the new state in effect and reports that
    /// it could not be saved. The running proxy already uses it.
    ///
    /// A running proxy that loses its last usable subscription is stopped
    /// instead of reloaded: a reload would leave it routing everything
    /// direct. A proxy that was already without a usable subscription when
    /// this change arrived (one an earlier run left running, for example) is
    /// not stopped, so the user can still disconnect it.
    async fn commit(
        &mut self,
        state: UserState,
        meta: SubscriptionMeta,
        caches: SubscriptionCacheSet,
    ) -> Result<(), AppError> {
        self.check_writable(&state, &meta)?;
        let composed = self.compose_for(&state, &meta, &caches)?;
        let was_connectable = self.connect_block().is_none();
        let now_connectable = self.connect_block_for(&state, &meta, &caches).is_none();
        let stop_proxy =
            was_connectable && !now_connectable && self.refresh_engine_status().await.is_running();
        if stop_proxy {
            self.stop_engine().await?;
            self.log("stopped the proxy: no subscription has servers any more");
        }
        self.apply_if_running(&composed).await?;
        let result = self.persist_and_adopt(state, meta, caches, &composed);
        self.sync_system_proxy().await;
        if stop_proxy {
            self.notice = Some((NoticeKind::Warning, STOPPED_FOR_NO_SERVERS.to_string()));
        }
        result
    }

    /// Refuse a change to a file this build must not overwrite.
    fn check_writable(&self, state: &UserState, meta: &SubscriptionMeta) -> Result<(), AppError> {
        let state_changed = *state != self.state;
        let meta_changed = *meta != self.meta;
        if (state_changed && !self.state_writable) || (meta_changed && !self.meta_writable) {
            return Err(AppError::ReadOnly);
        }
        Ok(())
    }

    /// Save whatever changed and make the new state current.
    fn persist_and_adopt(
        &mut self,
        state: UserState,
        meta: SubscriptionMeta,
        caches: SubscriptionCacheSet,
        composed: &Composed,
    ) -> Result<(), AppError> {
        let state_changed = state != self.state;
        let meta_changed = meta != self.meta;

        let mut failures = Vec::new();
        if state_changed && let Err(error) = self.store.save_user_state(&state) {
            failures.push(error.to_string());
        }
        if meta_changed && let Err(error) = self.store.save_subscription_meta(&meta) {
            failures.push(error.to_string());
        }

        self.state = state;
        self.meta = meta;
        self.caches = caches;
        self.adopt(composed);
        self.engine_error = None;

        if failures.is_empty() {
            return Ok(());
        }
        Err(AppError::Storage(format!(
            "the change is in effect but could not be saved: {}",
            self.scrub(&failures.join("; "))
        )))
    }

    /// Validate and reload the candidate, if and only if the engine runs.
    async fn apply_if_running(&mut self, composed: &Composed) -> Result<(), AppError> {
        // A change the engine's configuration does not see (the theme, the
        // background, the accent color) leaves the engine alone.
        if self.running_config.as_ref() == Some(&composed.config) {
            return Ok(());
        }
        if !self.refresh_engine_status().await.is_running() {
            return Ok(());
        }
        let report = self
            .engine
            .validate(&composed.config)
            .await
            .map_err(|error| self.engine_failure(error))?;
        if !report.ok {
            return Err(self.rejected(&report));
        }
        self.engine
            .reload(&composed.config)
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.running_config = Some(composed.config.clone());
        // The engine applies its managed subscriptions again on a reload,
        // which puts their rules ahead of the user's (ARCHITECTURE §4.6).
        if !self.engine_managed_names().is_empty() {
            self.reassert_rule_order().await;
        }
        self.assert_default_selection(composed.default_selection.as_ref())
            .await;
        Ok(())
    }

    /// Select the primary group's default member on the running engine.
    ///
    /// The configuration already lists that member first, so a fresh start
    /// would use it anyway; the explicit selection makes the engine's choice
    /// match the one the app shows after a reload too. A refusal is logged,
    /// not returned: the proxy runs, on the engine's own first member.
    async fn assert_default_selection(&mut self, default: Option<&(String, String)>) {
        let Some((group, member)) = default else {
            return;
        };
        if !self.engine_status.phase.is_running() {
            return;
        }
        if let Err(error) = self.engine.select(group, member).await {
            let text = self.scrub(&error.to_string());
            self.log(format!("could not select the default server: {text}"));
            return;
        }
        self.refresh_live_selections().await;
    }

    // ----- engine-managed subscriptions (ARCHITECTURE §4.6, R12) ---------------

    /// Check the engine-managed subscriptions, as the background loop does
    /// every 30 seconds.
    ///
    /// Does nothing while the engine is not running or no subscription is
    /// engine-managed. When the engine reports a refresh, or this is the
    /// first look since it started, the user's rule order is put back in
    /// front and the shadow check is run again. Errors are logged, not
    /// returned: the loop has no one to report them to, and the next tick
    /// tries again.
    pub async fn check_engine_subscriptions(&mut self) {
        let managed = self.engine_managed_names();
        if managed.is_empty() || !self.engine_status.phase.is_running() {
            self.refresh_watch.reset();
            if !self.engine_warnings.is_empty() {
                self.engine_warnings.clear();
                self.publish();
            }
            return;
        }
        let Ok(listed) = self.engine.engine_subscriptions().await else {
            return;
        };
        if !self.refresh_watch.observe(&listed, &managed) {
            return;
        }
        self.reassert_rule_order().await;
        self.check_shadowing(&managed).await;
        self.publish();
    }

    /// The labels of the engine-managed subscriptions. The engine names each
    /// one by its label, which is how they are matched.
    fn engine_managed_names(&self) -> Vec<String> {
        self.meta
            .subscriptions
            .iter()
            .filter(|subscription| subscription.mode == SubscriptionMode::EngineManaged)
            .map(|subscription| subscription.label.clone())
            .collect()
    }

    /// Put the user's rules first, in the user's order, and `MATCH` last.
    /// The engine is given a new list only when the order differs.
    async fn reassert_rule_order(&mut self) {
        let user = self.user_rule_lines();
        let live = match self.engine.rules().await {
            Ok(live) => live,
            Err(error) => {
                let text = self.scrub(&error.to_string());
                self.log(format!("could not read the engine's rules: {text}"));
                return;
            }
        };
        let desired = desired_rule_order(&user, &live);
        if desired == live {
            return;
        }
        if let Err(error) = self.engine.replace_rules(&desired).await {
            let text = self.scrub(&error.to_string());
            self.log(format!("could not restore the rule order: {text}"));
            return;
        }
        match self.engine.rules().await {
            Ok(after) if after == desired => {
                self.log("restored the user's rule order after an engine-managed refresh");
            }
            Ok(_) => self.log(
                "the engine kept a different rule order after the restore; an engine-managed subscription's rules may be ahead of yours",
            ),
            Err(error) => {
                let text = self.scrub(&error.to_string());
                self.log(format!("could not verify the restored rule order: {text}"));
            }
        }
    }

    /// The user's enabled rules as the engine spells them, in the order the
    /// build uses. Like the build, it stops at the first `MATCH` the user
    /// wrote, and it leaves out the app's own `MATCH` tail, which the engine
    /// order check keeps last.
    fn user_rule_lines(&self) -> Vec<String> {
        let mut rules: Vec<&StoredRule> = self.state.rules.iter().collect();
        rules.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id)));
        rules
            .into_iter()
            .filter(|rule| rule.enabled)
            .flat_map(|rule| translate_all(std::slice::from_ref(&rule.spec)))
            .take_while(|line| !is_match_rule(line))
            .collect()
    }

    /// Record the user's own nodes that an engine-managed subscription
    /// shadows, from the engine's live proxy list.
    async fn check_shadowing(&mut self, managed: &[String]) {
        let live = match self.engine.proxies().await {
            Ok(live) => live,
            Err(error) => {
                let text = self.scrub(&error.to_string());
                self.log(format!("could not read the engine's proxies: {text}"));
                return;
            }
        };
        let own: Vec<OwnNode> = self
            .state
            .nodes
            .iter()
            .map(|node| OwnNode {
                name: node.name.clone(),
                server: node.server.clone(),
                port: node.port,
            })
            .collect();
        self.engine_warnings = shadow_warnings(&own, &live, managed);
    }

    // ----- groups -------------------------------------------------------------

    /// The groups of a configuration, with what each one uses and how each
    /// member measured.
    /// The subscription label each subscription node and group name came
    /// from. Only app-managed subscriptions with a cached payload appear: an
    /// engine-managed one's nodes are not known to the app.
    fn name_origins(&self) -> BTreeMap<String, String> {
        let mut origins = BTreeMap::new();
        for subscription in self
            .meta
            .subscriptions
            .iter()
            .filter(|subscription| subscription.mode == SubscriptionMode::AppManaged)
        {
            let Some(payload) = self.caches.payload(&subscription.id) else {
                continue;
            };
            let names = payload
                .nodes
                .iter()
                .map(|node| &node.name)
                .chain(payload.groups.iter().map(|group| &group.name));
            for name in names {
                origins
                    .entry(name.clone())
                    .or_insert_with(|| subscription.label.clone());
            }
        }
        origins
    }

    fn group_views(
        &self,
        config: &RuntimeConfig,
        prefs: &UserPreferences,
        origins: &BTreeMap<String, String>,
    ) -> Vec<GroupView> {
        let label_of = |name: &str| display_label(name, origins.get(name).map(String::as_str));
        config
            .proxy_groups
            .iter()
            .map(|group| GroupView {
                name: group.name.clone(),
                label: label_of(&group.name),
                kind: group.group_type.clone(),
                selected: self.selected_member(group, prefs),
                members: group
                    .members
                    .iter()
                    .map(|member| MemberView {
                        name: member.clone(),
                        label: label_of(member),
                        protocol: member_protocol(config, member),
                        latency: self.member_latency(config, member),
                    })
                    .collect(),
            })
            .collect()
    }

    /// The member a group uses: the engine's live choice while it runs, else
    /// the saved choice for the group, else the first member.
    fn selected_member(&self, group: &ProxyGroup, prefs: &UserPreferences) -> Option<String> {
        if self.engine_status.phase.is_running()
            && let Some(now) = self.live_selected.get(&group.name)
        {
            return Some(now.clone());
        }
        prefs
            .group_selections
            .get(&group.name)
            .filter(|saved| group.members.contains(saved))
            .or_else(|| group.members.first())
            .cloned()
    }

    /// The last latency test's result for a member, looked up by the node's
    /// name. A group or a built-in has no test of its own.
    fn member_latency(&self, config: &RuntimeConfig, member: &str) -> Latency {
        let Some(node) = config.proxies.iter().find(|node| node.name == member) else {
            return Latency::Untested;
        };
        match self.latencies.get(&node.id) {
            None => Latency::Untested,
            Some(None) => Latency::TimedOut,
            Some(Some(milliseconds)) => Latency::Measured(*milliseconds),
        }
    }

    // ----- composition --------------------------------------------------------

    fn compose_current(&mut self) -> Result<Composed, AppError> {
        let state = self.state.clone();
        let meta = self.meta.clone();
        let caches = self.caches.clone();
        self.compose_for(&state, &meta, &caches)
    }

    fn compose_for(
        &mut self,
        state: &UserState,
        meta: &SubscriptionMeta,
        caches: &SubscriptionCacheSet,
    ) -> Result<Composed, AppError> {
        let mut engine_urls = BTreeMap::new();
        for subscription in meta
            .subscriptions
            .iter()
            .filter(|subscription| subscription.mode == SubscriptionMode::EngineManaged)
        {
            let url = self.read_secret(&subscription_url_key(&subscription.id))?;
            engine_urls.insert(subscription.id.clone(), url);
        }
        let composed = compose(state, meta, caches, &engine_urls)
            .map_err(|messages| self.build_error(messages))?;
        self.learn_config_secrets(&composed.config);
        Ok(composed)
    }

    fn adopt(&mut self, composed: &Composed) {
        self.built = Some(composed.config.clone());
        self.built_primary = composed.primary_group.clone();
        self.warnings = composed.warnings.clone();
    }

    // ----- secrets and redaction ----------------------------------------------

    fn read_secret(&mut self, key: &str) -> Result<String, AppError> {
        let bytes = self
            .secrets
            .get(key)
            .map_err(|error| AppError::Secret(self.scrub(&error.to_string())))?;
        let value = String::from_utf8(bytes).map_err(|_| {
            AppError::Secret("a stored subscription secret could not be read".to_string())
        })?;
        self.redactor.add_secret(value.clone());
        Ok(value)
    }

    fn write_secret(&self, key: &str, value: &str) -> Result<(), AppError> {
        self.secrets
            .set(key, value.as_bytes())
            .map_err(|error| AppError::Secret(self.scrub(&error.to_string())))
    }

    /// Remove a subscription's secrets. Failures are logged; the subscription
    /// is already gone from the saved list, so nothing else can be done.
    fn forget_secrets(&mut self, id: &str) {
        for key in [subscription_url_key(id), subscription_header_key(id)] {
            if let Err(error) = self.secrets.remove(&key) {
                let text = self.scrub(&error.to_string());
                self.log(format!("could not remove a stored secret: {text}"));
            }
        }
    }

    fn learn_config_secrets(&mut self, config: &RuntimeConfig) {
        for node in &config.proxies {
            for key in SECRET_NODE_KEYS {
                if let Some(value) = node.raw.get(*key).and_then(|value| value.as_str())
                    && value.len() >= MIN_SECRET_LEN
                {
                    self.redactor.add_secret(value);
                }
            }
        }
    }

    /// Remove every known secret from a piece of text, then any URL or
    /// keyed credential the generic filter recognizes.
    fn scrub(&self, text: &str) -> String {
        redact_error(&self.redactor.redact(text))
    }

    fn build_error(&self, messages: Vec<String>) -> AppError {
        AppError::Build(messages.iter().map(|message| self.scrub(message)).collect())
    }

    fn engine_failure(&self, error: EngineError) -> AppError {
        match error {
            EngineError::EngineRejected(message) => AppError::EngineRejected(self.scrub(&message)),
            other => AppError::Engine(self.scrub(&other.to_string())),
        }
    }

    fn rejected(&self, report: &ValidationReport) -> AppError {
        let message = report
            .engine_error
            .clone()
            .unwrap_or_else(|| "the engine did not accept the configuration".to_string());
        AppError::EngineRejected(self.scrub(&message))
    }

    /// A refresh failed. Record it against the subscription and keep the
    /// last valid payload in use.
    fn record_refresh_failure(&mut self, id: &str, error: AppError) -> AppError {
        let message = error.to_string();
        if let Some(subscription) = self
            .meta
            .subscriptions
            .iter_mut()
            .find(|subscription| subscription.id == id)
        {
            subscription.last_error = Some(message.clone());
        }
        if self.meta_writable
            && let Err(error) = self.store.save_subscription_meta(&self.meta)
        {
            let text = self.scrub(&error.to_string());
            self.log(format!("could not save the subscription status: {text}"));
        }
        self.log(message);
        error
    }

    // ----- bookkeeping --------------------------------------------------------

    /// Record the outcome of an action for the UI and publish the snapshot.
    fn finish<T>(&mut self, result: Result<T, AppError>) -> Result<T, AppError> {
        match &result {
            Ok(_) => {
                if matches!(self.notice, Some((NoticeKind::Error, _))) {
                    self.notice = None;
                }
            }
            Err(error) => self.notice = Some((NoticeKind::Error, error.to_string())),
        }
        self.publish();
        result
    }

    fn publish(&self) {
        let _ = self.updates.send_replace(self.snapshot());
    }

    fn log(&mut self, text: impl Into<String>) {
        let text = self.scrub(&text.into());
        let stamp = timestamp::now().unwrap_or_default();
        self.app_log.push_back(format!("{stamp}  {text}"));
        while self.app_log.len() > LOG_CAPACITY {
            self.app_log.pop_front();
        }
    }
}

/// The group a node is selected in on the running engine.
///
/// The user's selected group when it holds the node; otherwise the first
/// group that does. `None` when no group holds the node.
fn live_group(config: &RuntimeConfig, node_name: &str, preferred: Option<&str>) -> Option<String> {
    let holders: Vec<&str> = config
        .proxy_groups
        .iter()
        .filter(|group| group.members.iter().any(|member| member == node_name))
        .map(|group| group.name.as_str())
        .collect();
    preferred
        .filter(|name| holders.contains(name))
        .or_else(|| holders.first().copied())
        .map(str::to_string)
}

/// Give rules consecutive positions, matching their order in the list.
fn renumber(rules: &mut [StoredRule]) {
    for (index, rule) in rules.iter_mut().enumerate() {
        rule.order = u32::try_from(index).unwrap_or(u32::MAX);
    }
}

fn subscription_status(subscription: &Subscription) -> SubscriptionStatus {
    if subscription.mode == SubscriptionMode::EngineManaged {
        SubscriptionStatus::EngineManaged
    } else if subscription.last_error.is_some() {
        SubscriptionStatus::Failing
    } else if subscription.last_updated.is_some() {
        SubscriptionStatus::Current
    } else {
        SubscriptionStatus::NeverFetched
    }
}

/// The error for a configuration the parser rejected. The parser's message
/// is shown as it is: it names what is wrong with the text.
fn invalid_config(error: starpod_subscription::SubscriptionError) -> AppError {
    AppError::Invalid(error.to_string())
}

/// The error for a local profile whose source file has gone missing.
fn missing_source(label: &str) -> AppError {
    AppError::Invalid(format!("the pasted configuration of “{label}” is missing"))
}

/// The counts and the timestamp a successful payload records on its
/// subscription. Shared by every path that stores a payload, so they agree.
fn record_payload_counts(entry: &mut Subscription, payload: &SubscriptionPayload, now: i64) {
    entry.last_updated = timestamp::format(now);
    entry.last_error = None;
    entry.node_count = payload.nodes.len();
    entry.group_count = payload.groups.len();
}

fn is_subscription_url(text: &str) -> bool {
    Url::parse(text)
        .map(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
        .unwrap_or(false)
}

fn core_quota(quota: &starpod_subscription::SubscriptionQuota) -> CoreQuota {
    CoreQuota {
        used: quota.used(),
        total: quota.total,
        expires: quota.expires.and_then(timestamp::format),
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

/// Protocols that carry traffic over UDP only. A TCP connection says nothing
/// about them, so a direct ping leaves them untested.
const UDP_ONLY_PROTOCOLS: &[&str] = &["hysteria", "hysteria2", "tuic", "wireguard"];

/// Ping each node's server directly, without the engine: the time to open a
/// TCP connection to its address and port, in milliseconds, or `None` when it
/// does not answer within `timeout_ms`. The name lookup is done first and is
/// not counted. A node without an address (`direct`) or on a UDP-only
/// protocol gets no entry, so it shows as untested rather than failed.
///
/// This is what the latency test does while the proxy is off. It measures the
/// route to the server, not a request through it, so it says a server is
/// reachable, not that the proxy works.
pub(crate) async fn tcp_ping(nodes: Vec<Node>, timeout_ms: u32) -> BTreeMap<String, Option<u32>> {
    let timeout = std::time::Duration::from_millis(u64::from(timeout_ms));
    let mut set = tokio::task::JoinSet::new();
    for node in nodes {
        let (Some(server), Some(port)) = (node.server.clone(), node.port) else {
            continue;
        };
        if UDP_ONLY_PROTOCOLS.contains(&node.protocol.to_ascii_lowercase().as_str()) {
            continue;
        }
        set.spawn(async move {
            let latency = tokio::time::timeout(timeout, async {
                let address = tokio::net::lookup_host((server.as_str(), port))
                    .await
                    .ok()?
                    .next()?;
                let started = std::time::Instant::now();
                tokio::net::TcpStream::connect(address).await.ok()?;
                Some(
                    u32::try_from(started.elapsed().as_millis())
                        .unwrap_or(u32::MAX)
                        .max(1),
                )
            })
            .await
            .ok()
            .flatten();
            (node.id, latency)
        });
    }
    let mut results = BTreeMap::new();
    while let Some(joined) = set.join_next().await {
        if let Ok((id, latency)) = joined {
            results.insert(id, latency);
        }
    }
    results
}

#[cfg(test)]
mod tcp_ping_tests {
    use super::*;

    fn node(id: &str, protocol: &str, server: Option<&str>, port: Option<u16>) -> Node {
        Node {
            id: id.to_string(),
            name: id.to_string(),
            protocol: protocol.to_string(),
            server: server.map(str::to_string),
            port,
            tags: Vec::new(),
            raw: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn a_direct_ping_times_open_ports_and_skips_what_tcp_cannot_test() {
        let open = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let open_port = open.local_addr().unwrap().port();
        // A port that was free a moment ago: nothing listens on it.
        let closed_port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            probe.local_addr().unwrap().port()
        };
        let results = tcp_ping(
            vec![
                node("up", "vless", Some("127.0.0.1"), Some(open_port)),
                node("down", "trojan", Some("127.0.0.1"), Some(closed_port)),
                node("direct", "direct", None, None),
                node("udp", "hysteria2", Some("127.0.0.1"), Some(open_port)),
            ],
            2_000,
        )
        .await;
        assert!(
            matches!(results.get("up"), Some(Some(ms)) if *ms >= 1),
            "{results:?}"
        );
        assert_eq!(results.get("down"), Some(&None));
        assert!(!results.contains_key("direct"), "no address, not tested");
        assert!(
            !results.contains_key("udp"),
            "TCP says nothing about a UDP protocol"
        );
    }
}
