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

use starpod_core::rules::parse_line;
use starpod_core::timestamp;
use starpod_core::{
    ProxyMode, RuleSpec, RuntimeConfig, StoredRule, Subscription, SubscriptionMode,
    SubscriptionPayload, SubscriptionQuota as CoreQuota, UserPreferences,
};
use starpod_engine::log::redact_urls;
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
use starpod_subscription::metadata::DEFAULT_INTERVAL_SECS;
use starpod_subscription::{
    FetchRequest, HttpSubscriptionProvider, RefreshOutcome, SubscriptionCacheSet,
    SubscriptionMetadata, SubscriptionProvider, refresh,
};
use tokio::sync::{Mutex, watch};
use url::Url;
use uuid::Uuid;

use crate::compose::{Composed, compose};
use crate::error::{AppError, RuleLineError};
use crate::snapshot::{
    AppSnapshot, EngineView, NodeView, NoticeKind, RuleView, SubscriptionStatus, SubscriptionView,
    builtin_target_names, rule_kind_views, rule_view,
};

/// How many lines of the app's own log are kept.
const LOG_CAPACITY: usize = 500;
/// How many lines of the engine's output the snapshot carries.
const ENGINE_LOG_TAIL: usize = 200;
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
/// The address a latency test measures against.
const LATENCY_URL: &str = "https://www.gstatic.com/generate_204";
/// How long one latency probe may take, in milliseconds.
const LATENCY_TIMEOUT_MS: u32 = 5000;

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
    /// The result of the last latency test, by node id. `None` means the
    /// probe failed or timed out.
    latencies: BTreeMap<String, Option<u32>>,
    warnings: Vec<String>,
    engine_status: EngineStatus,
    engine_error: Option<String>,
    handle: Option<EngineHandle>,
    notice: Option<(NoticeKind, String)>,
    app_log: VecDeque<String>,
    redactor: Redactor,
    updates: watch::Sender<AppSnapshot>,
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
        let SelectedStore {
            store: secret_store,
            fallback_reason,
        } = secrets;
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
            latencies: BTreeMap::new(),
            warnings: Vec::new(),
            engine_status: EngineStatus::default(),
            engine_error: None,
            handle: None,
            notice: None,
            app_log: VecDeque::new(),
            redactor: Redactor::default(),
            updates: watch::channel(AppSnapshot::default()).0,
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
        Ok(Self::new(ServiceDeps {
            store: Store::new(paths),
            secrets,
            engine: Box::new(engine),
            provider: Arc::new(provider),
            engine_installed: starpod_engine::download::is_installed(),
        }))
    }

    /// Move the service behind a lock, for a UI on another thread.
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

        let subscriptions = self
            .meta
            .subscriptions
            .iter()
            .map(|subscription| SubscriptionView {
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
            })
            .collect();

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

        let engine = EngineView {
            phase: self.engine_status.phase,
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

        let engine_log_lines = self
            .engine
            .logs()
            .tail(ENGINE_LOG_TAIL)
            .into_iter()
            .map(|line| self.scrub(&redact_urls(&line.text)))
            .collect();

        AppSnapshot {
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
            preferences: prefs.clone(),
            warnings: self.warnings.clone(),
            log_lines: self.app_log.iter().cloned().collect(),
            engine_log_lines,
            secret_backend: self.secrets.backend_name().to_string(),
            secret_encrypted: self.secrets.is_encrypted(),
            secret_fallback_reason: self.secret_fallback_reason.clone(),
            data_dir: self.store.paths().data_dir().to_path_buf(),
            notice: self.notice.clone(),
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
                    Ok(payload) => {
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

        self.refresh_engine_status().await;
        self.notice = None;
        match self.compose_current() {
            Ok(composed) => self.adopt(&composed),
            Err(error) => problems.push(error.to_string()),
        }

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
        self.publish();
        self.engine_status.phase
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

    /// Remove a subscription, its secrets and its cached payload.
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
    pub async fn save_preferences(&mut self, prefs: UserPreferences) -> Result<(), AppError> {
        let result = self.save_preferences_inner(prefs).await;
        self.finish(result)
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

    /// Measure every node's latency against the running engine.
    ///
    /// Probes run one after another, each bounded by a short timeout, so a
    /// long node list takes a while. A node whose probe fails is recorded as
    /// untested. Fails with [`AppError::Invalid`] when the proxy is stopped.
    pub async fn test_latency(&mut self) -> Result<(), AppError> {
        let result = self.test_latency_inner().await;
        self.finish(result)
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

        let state = self.state.clone();
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
        self.forget_secrets(id);
        self.log(format!("removed the subscription “{}”", removed.label));
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

        let url = self.read_secret(&subscription_url_key(id))?;
        let mut request = FetchRequest::new(url);
        if subscription.has_auth_header {
            let header = self.read_secret(&subscription_header_key(id))?;
            request = request.with_authorization(header);
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
            RefreshOutcome::Refreshed { payload, quota, .. } => {
                let bytes = serde_json::to_vec(&payload)
                    .map_err(|error| AppError::Storage(error.to_string()))?;
                let node_count = payload.nodes.len();
                let group_count = payload.groups.len();

                let mut caches = self.caches.clone();
                caches.store(id, payload, now);

                let mut meta = self.meta.clone();
                if let Some(entry) = meta.subscriptions.iter_mut().find(|entry| entry.id == id) {
                    entry.last_updated = timestamp::format(now);
                    entry.last_error = None;
                    entry.quota = quota.as_ref().map(core_quota);
                    entry.node_count = node_count;
                    entry.group_count = group_count;
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
                Ok(())
            }
            RefreshOutcome::Failed { error, .. } => {
                let message = self.scrub(&error.to_string());
                let app_error = AppError::Subscription(format!(
                    "“{}” could not be refreshed: {message}",
                    subscription.label
                ));
                Err(self.record_refresh_failure(id, app_error))
            }
        }
    }

    async fn refresh_all_inner(&mut self) -> Result<usize, AppError> {
        let ids: Vec<String> = self
            .meta
            .subscriptions
            .iter()
            .filter(|subscription| subscription.mode == SubscriptionMode::AppManaged)
            .map(|subscription| subscription.id.clone())
            .collect();
        let attempted = ids.len();
        let mut succeeded = 0usize;
        let mut last_error = None;
        for id in ids {
            match self.refresh_inner(&id).await {
                Ok(()) => succeeded += 1,
                Err(error) => last_error = Some(error),
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
        if self.refresh_engine_status().await.is_running() {
            return Ok(());
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
        self.engine_error = None;
        self.refresh_engine_status().await;
        self.log("the proxy started");
        Ok(())
    }

    async fn disconnect_inner(&mut self) -> Result<(), AppError> {
        if self.refresh_engine_status().await == EnginePhase::Stopped {
            return Ok(());
        }
        self.engine
            .stop()
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.handle = None;
        self.engine_error = None;
        self.refresh_engine_status().await;
        // Latencies were measured against a proxy that is now gone.
        self.latencies.clear();
        self.log("the proxy stopped");
        Ok(())
    }

    async fn restart_inner(&mut self) -> Result<(), AppError> {
        self.disconnect_inner().await?;
        self.connect_inner().await
    }

    async fn set_mode_inner(&mut self, mode: ProxyMode) -> Result<(), AppError> {
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

    async fn test_latency_inner(&mut self) -> Result<(), AppError> {
        if !self.refresh_engine_status().await.is_running() {
            return Err(AppError::Invalid(
                "Connect first to test latency".to_string(),
            ));
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

        let mut results = BTreeMap::new();
        for (id, name) in nodes {
            let latency = self
                .engine
                .delay(&name, LATENCY_URL, LATENCY_TIMEOUT_MS)
                .await
                .ok();
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
        let path = starpod_engine::download::install()
            .await
            .map_err(|error| self.engine_failure(error))?;
        self.engine_installed = true;
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
    async fn commit(
        &mut self,
        state: UserState,
        meta: SubscriptionMeta,
        caches: SubscriptionCacheSet,
    ) -> Result<(), AppError> {
        self.check_writable(&state, &meta)?;
        let composed = self.compose_for(&state, &meta, &caches)?;
        self.apply_if_running(&composed.config).await?;
        self.persist_and_adopt(state, meta, caches, &composed)
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
    async fn apply_if_running(&mut self, config: &RuntimeConfig) -> Result<(), AppError> {
        if !self.refresh_engine_status().await.is_running() {
            return Ok(());
        }
        let report = self
            .engine
            .validate(config)
            .await
            .map_err(|error| self.engine_failure(error))?;
        if !report.ok {
            return Err(self.rejected(&report));
        }
        self.engine
            .reload(config)
            .await
            .map_err(|error| self.engine_failure(error))?;
        Ok(())
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
    /// keyed credential the generic filter recognises.
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
