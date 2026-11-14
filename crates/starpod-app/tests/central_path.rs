//! The central path, end to end, against a fake engine and canned payloads.
//!
//! Everything here is deterministic: no network, no real engine, no clock
//! dependence beyond the timestamps the service writes itself.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use starpod_app::{AppError, AppService, NoticeKind, ServiceDeps, SubscriptionStatus};
use starpod_core::rules::parse_line;
use starpod_core::{ProxyMode, RuleSpec, RuntimeConfig, SubscriptionMode, TunSettings};
use starpod_engine::reassert::{EngineSubscription, LiveProxy};
use starpod_engine::{
    EngineError, EngineHandle, EnginePhase, EngineStatus, LogBuffer, LogStream, ProxyEngine,
    StopRequest, ValidationReport,
};
use starpod_store::secret::MockSecretStore;
use starpod_store::{AppPaths, SelectedStore, Store};
use starpod_subscription::{
    FetchRequest, FetchResponse, ProviderError, ProviderKind, StaticSubscriptionProvider,
    SubscriptionProvider,
};
use tempfile::TempDir;
use tokio::sync::watch;

/// A realistic Clash/Mihomo subscription: two nodes, one group, one rule,
/// and a top-level port the subscription suggests.
const FIXTURE: &str = "\
mixed-port: 7899
proxies:
  - name: \"Node A\"
    type: trojan
    server: a.example.com
    port: 443
    password: hunter2-trojan
  - name: \"Node B\"
    type: vmess
    server: b.example.com
    port: 8443
    uuid: 00000000-0000-0000-0000-000000000001
    alterId: 0
    cipher: auto
proxy-groups:
  - name: \"Auto\"
    type: url-test
    proxies:
      - \"Node A\"
      - \"Node B\"
    url: https://example.com/generate_204
    interval: 300
rules:
  - DOMAIN,example.com,DIRECT
  - MATCH,DIRECT
";

/// A later payload from the same subscription: different ports, a different
/// LAN setting, and a different node set.
const SECOND: &str = "\
mixed-port: 7001
allow-lan: true
proxies:
  - name: \"Node C\"
    type: trojan
    server: c.example.com
    port: 443
    password: hunter3-trojan
proxy-groups:
  - name: \"Auto\"
    type: url-test
    proxies:
      - \"Node C\"
    url: https://example.com/generate_204
    interval: 300
rules:
  - DOMAIN,example.com,DIRECT
";

/// A subscription address with a token in its query string.
const SUB_URL: &str = "https://sub.example.test/api/client/subscribe?token=TOKEN-SECRET-42";
/// An authorization header value that must never be shown.
const HEADER: &str = "Bearer HEADER-SECRET-7";

// ----- the fake engine -------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Validate(RuntimeConfig),
    Start(RuntimeConfig),
    Reload(RuntimeConfig),
    Stop,
    Select { group: String, member: String },
    ReplaceRules(Vec<String>),
}

#[derive(Default)]
struct EngineState {
    calls: Vec<Call>,
    phase: EnginePhase,
    reject: bool,
    /// Latency per proxy name. A proxy with no entry times out.
    delays: BTreeMap<String, u32>,
    /// Whether a live selection is refused.
    fail_selects: bool,
    /// The engine's rule list, as the engine currently holds it.
    live_rules: Vec<String>,
    /// The engine-managed subscriptions the engine reports.
    engine_subs: Vec<EngineSubscription>,
    /// The proxies the engine reports as live.
    live_proxies: Vec<LiveProxy>,
}

/// A `ProxyEngine` that records every call and can be told to reject a
/// candidate. Clones share the same record.
#[derive(Clone, Default)]
struct FakeEngine {
    inner: Arc<Mutex<EngineState>>,
}

impl FakeEngine {
    fn lock(&self) -> MutexGuard<'_, EngineState> {
        self.inner
            .lock()
            .expect("the fake engine's lock is not poisoned")
    }

    fn calls(&self) -> Vec<Call> {
        self.lock().calls.clone()
    }

    fn reject_validations(&self, reject: bool) {
        self.lock().reject = reject;
    }

    fn fail_selects(&self, fail: bool) {
        self.lock().fail_selects = fail;
    }

    fn set_delay(&self, proxy: &str, ms: u32) {
        self.lock().delays.insert(proxy.to_string(), ms);
    }
}

#[async_trait]
impl ProxyEngine for FakeEngine {
    async fn start(&self, cfg: &RuntimeConfig) -> Result<EngineHandle, EngineError> {
        {
            let mut state = self.lock();
            state.calls.push(Call::Start(cfg.clone()));
            state.phase = EnginePhase::Running;
        }
        let (_sender, exited) = watch::channel(None);
        Ok(EngineHandle {
            exited,
            stop: StopRequest::new(|| {}),
        })
    }

    async fn stop(&self) -> Result<(), EngineError> {
        let mut state = self.lock();
        state.calls.push(Call::Stop);
        state.phase = EnginePhase::Stopped;
        Ok(())
    }

    async fn reload(&self, cfg: &RuntimeConfig) -> Result<(), EngineError> {
        self.lock().calls.push(Call::Reload(cfg.clone()));
        Ok(())
    }

    async fn status(&self) -> Result<EngineStatus, EngineError> {
        let state = self.lock();
        Ok(EngineStatus {
            phase: state.phase,
            controller_ready: state.phase.is_running(),
            ..EngineStatus::default()
        })
    }

    async fn validate(&self, cfg: &RuntimeConfig) -> Result<ValidationReport, EngineError> {
        let mut state = self.lock();
        state.calls.push(Call::Validate(cfg.clone()));
        if state.reject {
            Ok(ValidationReport {
                ok: false,
                engine_error: Some("line 7: unknown outbound".to_string()),
                warnings: Vec::new(),
            })
        } else {
            Ok(ValidationReport {
                ok: true,
                ..ValidationReport::default()
            })
        }
    }

    async fn set_tun(&self, _tun: &TunSettings) -> Result<(), EngineError> {
        Ok(())
    }

    async fn select(&self, group: &str, member: &str) -> Result<(), EngineError> {
        let mut state = self.lock();
        if state.phase != EnginePhase::Running {
            return Err(EngineError::EngineDown);
        }
        state.calls.push(Call::Select {
            group: group.to_string(),
            member: member.to_string(),
        });
        if state.fail_selects {
            return Err(EngineError::BadRequest("no such member".to_string()));
        }
        Ok(())
    }

    async fn delay(&self, proxy: &str, _url: &str, _timeout_ms: u32) -> Result<u32, EngineError> {
        let state = self.lock();
        if state.phase != EnginePhase::Running {
            return Err(EngineError::EngineDown);
        }
        state.delays.get(proxy).copied().ok_or(EngineError::Timeout)
    }

    fn logs(&self) -> LogStream {
        LogStream::new(LogBuffer::new(16))
    }

    async fn rules(&self) -> Result<Vec<String>, EngineError> {
        let state = self.lock();
        if state.phase != EnginePhase::Running {
            return Err(EngineError::EngineDown);
        }
        Ok(state.live_rules.clone())
    }

    async fn replace_rules(&self, rules: &[String]) -> Result<(), EngineError> {
        let mut state = self.lock();
        if state.phase != EnginePhase::Running {
            return Err(EngineError::EngineDown);
        }
        state.calls.push(Call::ReplaceRules(rules.to_vec()));
        state.live_rules = rules.to_vec();
        Ok(())
    }

    async fn engine_subscriptions(&self) -> Result<Vec<EngineSubscription>, EngineError> {
        let state = self.lock();
        if state.phase != EnginePhase::Running {
            return Err(EngineError::EngineDown);
        }
        Ok(state.engine_subs.clone())
    }

    async fn proxies(&self) -> Result<Vec<LiveProxy>, EngineError> {
        let state = self.lock();
        if state.phase != EnginePhase::Running {
            return Err(EngineError::EngineDown);
        }
        Ok(state.live_proxies.clone())
    }
}

// ----- a provider that answers a script ---------------------------------------

/// A provider that returns scripted responses in order, and records the
/// requests it was asked for.
#[derive(Clone)]
struct Scripted {
    steps: Arc<Mutex<VecDeque<Result<FetchResponse, ProviderError>>>>,
    requests: Arc<Mutex<Vec<FetchRequest>>>,
}

impl Scripted {
    fn new(steps: Vec<Result<FetchResponse, ProviderError>>) -> Self {
        Self {
            steps: Arc::new(Mutex::new(steps.into())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn requests(&self) -> Vec<FetchRequest> {
        self.requests
            .lock()
            .expect("the scripted provider's lock is not poisoned")
            .clone()
    }
}

#[async_trait]
impl SubscriptionProvider for Scripted {
    async fn fetch(&self, request: &FetchRequest) -> Result<FetchResponse, ProviderError> {
        self.requests
            .lock()
            .expect("the scripted provider's lock is not poisoned")
            .push(request.clone());
        self.steps
            .lock()
            .expect("the scripted provider's lock is not poisoned")
            .pop_front()
            .unwrap_or_else(|| {
                Err(ProviderError::Network(
                    "the scripted provider has no more responses".to_string(),
                ))
            })
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::Http
    }
}

// ----- helpers ----------------------------------------------------------------

fn paths_in(dir: &TempDir) -> AppPaths {
    AppPaths::from_roots(
        dir.path().join("config"),
        dir.path().join("data"),
        dir.path().join("cache"),
    )
}

fn build_service(
    dir: &TempDir,
    engine: &FakeEngine,
    provider: Arc<dyn SubscriptionProvider>,
) -> AppService {
    AppService::new(ServiceDeps {
        store: Store::new(paths_in(dir)),
        secrets: SelectedStore {
            store: Box::new(MockSecretStore::new()),
            fallback_reason: Some("the tests use an in-memory secret store".to_string()),
        },
        engine: Box::new(engine.clone()),
        provider,
        engine_installed: true,
    })
}

/// Saves the English language preference, so the Rust-side strings a snapshot
/// builds do not follow the machine's locale.
async fn pin_english(service: &mut AppService) {
    let mut prefs = service.user_state().preferences.clone();
    prefs.language = "en".to_string();
    service.save_preferences(prefs).await.unwrap();
}

fn rule(line: &str) -> RuleSpec {
    parse_line(line).expect("the test rule parses")
}

async fn add_airport(service: &mut AppService, header: Option<&str>) -> String {
    service
        .add_subscription(
            "Airport".to_string(),
            SUB_URL.to_string(),
            header.map(str::to_string),
            SubscriptionMode::AppManaged,
        )
        .await
        .expect("the subscription is added")
}

fn node_names(config: &RuntimeConfig) -> Vec<String> {
    config
        .proxies
        .iter()
        .map(|node| node.name.clone())
        .collect()
}

/// The configuration the engine was most recently given.
fn last_config(calls: &[Call]) -> RuntimeConfig {
    calls
        .iter()
        .rev()
        .find_map(|call| match call {
            Call::Validate(config) | Call::Start(config) | Call::Reload(config) => {
                Some(config.clone())
            }
            Call::Stop | Call::Select { .. } | Call::ReplaceRules(_) => None,
        })
        .expect("the engine was given a configuration")
}

fn node_id(service: &AppService, name: &str) -> String {
    service
        .snapshot()
        .nodes
        .iter()
        .find(|node| node.name == name)
        .unwrap_or_else(|| panic!("{name} is in the configuration"))
        .id
        .clone()
}

// ----- the central path --------------------------------------------------------

#[tokio::test]
async fn connect_validates_then_starts_with_namespaced_nodes_and_user_rules_in_order() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();

    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service
        .add_rule(rule("DOMAIN-SUFFIX,example.org,DIRECT"))
        .await
        .unwrap();
    service
        .add_rule(rule("DOMAIN,api.example.net,Airport / Auto"))
        .await
        .unwrap();

    service.connect().await.unwrap();

    let calls = engine.calls();
    assert_eq!(calls.len(), 2, "one validation, then one start: {calls:?}");
    let Call::Validate(validated) = &calls[0] else {
        panic!("the first engine call is a validation: {calls:?}");
    };
    let Call::Start(started) = &calls[1] else {
        panic!("the second engine call is a start: {calls:?}");
    };
    assert_eq!(
        validated, started,
        "the engine starts the configuration it validated"
    );

    let names = node_names(started);
    assert!(names.contains(&"Airport / Node A".to_string()), "{names:?}");
    assert!(names.contains(&"Airport / Node B".to_string()), "{names:?}");
    assert_eq!(
        started.rules,
        vec![
            "DOMAIN-SUFFIX,example.org,DIRECT".to_string(),
            "DOMAIN,api.example.net,Airport / Auto".to_string(),
            "DOMAIN,example.com,DIRECT".to_string(),
            "MATCH,DIRECT".to_string(),
        ],
        "the user's rules come first, in the user's order, then the subscription's, then the subscription's MATCH target as the tail"
    );
    assert_eq!(
        started.mixed_port,
        Some(7899),
        "with no port chosen by the user, the subscription's port applies"
    );

    let snapshot = service.snapshot();
    assert_eq!(snapshot.engine.phase, EnginePhase::Running);
    assert_eq!(
        snapshot.subscriptions[0].status,
        SubscriptionStatus::Current
    );
    assert_eq!(snapshot.subscriptions[0].node_count, 2);
    assert_eq!(snapshot.rules.len(), 2);
    assert_eq!(
        snapshot.subscription_rules.len(),
        2,
        "every rule the subscription supplies is listed, including its MATCH"
    );
}

#[tokio::test]
async fn a_user_preference_survives_a_refresh_with_a_different_payload() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let scripted = Scripted::new(vec![
        Ok(FetchResponse::new(FIXTURE.as_bytes().to_vec())),
        Ok(FetchResponse::new(SECOND.as_bytes().to_vec())),
    ]);
    let provider: Arc<dyn SubscriptionProvider> = Arc::new(scripted);
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();

    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let mut prefs = service.user_state().preferences.clone();
    prefs.mixed_port = Some(7897);
    prefs.allow_lan = Some(false);
    service.save_preferences(prefs).await.unwrap();
    service
        .add_rule(rule("DOMAIN,mine.example,DIRECT"))
        .await
        .unwrap();
    service.connect().await.unwrap();

    let state_before = service.user_state().clone();
    let state_file = paths_in(&dir).user_state_file();
    let file_before = fs::read(&state_file).expect("the user state was saved");

    service.refresh_subscription(&id).await.unwrap();

    assert_eq!(
        service.user_state(),
        &state_before,
        "a refresh never changes the user's state"
    );
    assert_eq!(
        fs::read(&state_file).unwrap(),
        file_before,
        "a refresh never rewrites the user's state file"
    );

    let calls = engine.calls();
    let Some(Call::Reload(reloaded)) = calls.last() else {
        panic!("a refresh of a running proxy reloads it: {calls:?}");
    };
    assert_eq!(
        reloaded.mixed_port,
        Some(7897),
        "the user's port beats the subscription's"
    );
    assert!(
        !reloaded.allow_lan,
        "an explicit user 'off' beats the subscription's 'on'"
    );
    assert_eq!(node_names(reloaded), vec!["Airport / Node C".to_string()]);
    assert_eq!(
        reloaded.rules.first().map(String::as_str),
        Some("DOMAIN,mine.example,DIRECT"),
        "the user's rule still comes first"
    );
}

#[tokio::test]
async fn a_failing_refresh_keeps_the_previous_payload_and_still_builds() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let scripted = Scripted::new(vec![
        Ok(FetchResponse::new(FIXTURE.as_bytes().to_vec())),
        Err(ProviderError::Network(format!(
            "error sending request for url ({SUB_URL})"
        ))),
    ]);
    let provider: Arc<dyn SubscriptionProvider> = Arc::new(scripted);
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();

    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let failure = service.refresh_subscription(&id).await;
    assert!(
        matches!(failure, Err(AppError::Subscription(_))),
        "the refresh reports its failure: {failure:?}"
    );
    let snapshot = service.snapshot();
    assert_eq!(
        snapshot.subscriptions[0].status,
        SubscriptionStatus::Failing
    );
    assert_eq!(
        snapshot.subscriptions[0].node_count, 2,
        "the last valid payload's node count is kept"
    );

    service.connect().await.unwrap();
    let calls = engine.calls();
    let started = last_config(&calls);
    assert!(
        node_names(&started).contains(&"Airport / Node A".to_string()),
        "the proxy still starts with the last valid payload"
    );
}

#[tokio::test]
async fn a_rejected_candidate_does_not_start_the_engine() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    engine.reject_validations(true);
    let result = service.connect().await;

    assert!(
        matches!(result, Err(AppError::EngineRejected(ref text)) if text.contains("unknown outbound")),
        "the engine's own message surfaces: {result:?}"
    );
    let calls = engine.calls();
    assert_eq!(calls.len(), 1, "only the validation ran: {calls:?}");
    assert!(matches!(calls[0], Call::Validate(_)));
    assert_eq!(service.snapshot().engine.phase, EnginePhase::Stopped);
}

#[tokio::test]
async fn a_rejected_change_to_a_running_proxy_neither_reloads_nor_saves() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();

    engine.reject_validations(true);
    let state_before = service.user_state().clone();
    let mut prefs = state_before.preferences.clone();
    prefs.mode = Some(ProxyMode::Global);
    let result = service.save_preferences(prefs).await;

    assert!(
        matches!(result, Err(AppError::EngineRejected(_))),
        "{result:?}"
    );
    let calls = engine.calls();
    assert!(
        !calls.iter().any(|call| matches!(call, Call::Reload(_))),
        "a rejected candidate is never reloaded: {calls:?}"
    );
    assert_eq!(
        service.user_state(),
        &state_before,
        "the change was not kept"
    );
    assert!(
        !paths_in(&dir).user_state_file().exists() || {
            let saved: serde_json::Value =
                serde_json::from_slice(&fs::read(paths_in(&dir).user_state_file()).unwrap())
                    .unwrap();
            saved["preferences"]["mode"].is_null()
        },
        "the rejected mode was not saved"
    );
    assert_eq!(service.snapshot().engine.phase, EnginePhase::Running);
}

#[tokio::test]
async fn a_build_error_applies_nothing_and_surfaces_the_error() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();
    let calls_before = engine.calls().len();

    let result = service
        .add_rule(rule("DOMAIN,example.org,NoSuchGroup"))
        .await;

    match result {
        Err(AppError::Build(messages)) => assert!(
            messages
                .iter()
                .any(|message| message.contains("NoSuchGroup")),
            "the error names the missing target: {messages:?}"
        ),
        other => panic!("expected a build error, got {other:?}"),
    }
    assert!(
        service.user_state().rules.is_empty(),
        "the rule was not kept"
    );
    assert_eq!(
        engine.calls().len(),
        calls_before,
        "a build error does not reach the engine"
    );
    let notice = service.snapshot().notice;
    assert!(
        matches!(notice, Some((NoticeKind::Error, ref text)) if text.contains("NoSuchGroup")),
        "the error is shown as a notice: {notice:?}"
    );
}

#[tokio::test]
async fn a_corrupt_user_state_is_quarantined_and_the_app_starts_with_defaults() {
    let dir = TempDir::new().unwrap();
    let paths = paths_in(&dir);
    fs::create_dir_all(paths.config_dir()).unwrap();
    fs::write(paths.user_state_file(), b"{ this is not json").unwrap();

    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service
        .load()
        .await
        .expect("a corrupt file never stops the app");

    let snapshot = service.snapshot();
    let (kind, text) = snapshot
        .notice
        .clone()
        .expect("a notice explains what happened");
    assert_eq!(kind, NoticeKind::Warning);
    assert!(
        text.contains("moved"),
        "the notice says the file was moved: {text}"
    );
    assert!(
        snapshot.preferences.mixed_port.is_none(),
        "defaults are in use"
    );
    assert!(service.user_state().rules.is_empty());
    assert!(
        !paths.user_state_file().exists(),
        "the corrupt file is no longer in place"
    );
    let quarantined = fs::read_dir(paths.config_dir())
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("user-state.json.corrupt-")
        });
    assert!(quarantined, "the corrupt file was kept, not deleted");

    service
        .add_rule(rule("DOMAIN,example.org,DIRECT"))
        .await
        .expect("the app keeps working on defaults");
}

#[tokio::test]
async fn the_url_token_never_appears_in_the_snapshot_the_log_or_an_error() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let scripted = Scripted::new(vec![
        Err(ProviderError::Network(format!(
            "error sending request for url ({SUB_URL})"
        ))),
        Ok(FetchResponse::new(FIXTURE.as_bytes().to_vec())),
    ]);
    let provider: Arc<dyn SubscriptionProvider> = Arc::new(scripted.clone());
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();

    let id = add_airport(&mut service, Some(HEADER)).await;
    let failure = service.refresh_subscription(&id).await.unwrap_err();
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();

    let requests = scripted.requests();
    assert_eq!(
        requests[0].url, SUB_URL,
        "the stored URL is what gets fetched"
    );
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some(HEADER),
        "the stored header is what gets sent"
    );

    let snapshot = service.snapshot();
    let mut visible = format!("{snapshot:?} {failure:?} {failure}");
    for line in &snapshot.log_lines {
        visible.push_str(line);
    }
    for secret in ["TOKEN-SECRET-42", "HEADER-SECRET-7", "hunter2-trojan"] {
        assert!(
            !visible.contains(secret),
            "{secret} leaked into a snapshot, log line or error:\n{visible}"
        );
    }
    assert!(
        snapshot.subscriptions[0].error.is_none(),
        "the successful refresh cleared the recorded error"
    );
    assert!(snapshot.subscriptions[0].has_auth_header);
}

#[tokio::test]
async fn a_selected_node_is_moved_to_the_front_of_its_group() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let mut prefs = service.user_state().preferences.clone();
    prefs.selected_group = Some("Airport / Auto".to_string());
    service.save_preferences(prefs).await.unwrap();

    let node_b = service
        .snapshot()
        .nodes
        .iter()
        .find(|node| node.name == "Airport / Node B")
        .expect("Node B is in the configuration")
        .id
        .clone();
    service.select_node(&node_b).await.unwrap();

    service.connect().await.unwrap();
    let started = last_config(&engine.calls());
    let auto = started
        .proxy_groups
        .iter()
        .find(|group| group.name == "Airport / Auto")
        .expect("the subscription's group is in the configuration");
    assert_eq!(
        auto.members.first().map(String::as_str),
        Some("Airport / Node B"),
        "the engine starts on the chosen node"
    );
    let snapshot = service.snapshot();
    assert_eq!(
        snapshot.current_node.map(|node| node.name),
        Some("Airport / Node B".to_string())
    );
}

#[tokio::test]
async fn raw_rules_report_the_line_numbers_of_every_bad_line_and_save_nothing() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();

    let text = "DOMAIN,example.org,DIRECT\n# a comment\n\nNOT-A-RULE,x,DIRECT\nDOMAIN-SUFFIX,example.net,DIRECT\n";
    match service.apply_raw_rules(text).await {
        Err(AppError::RuleLines(lines)) => {
            assert_eq!(lines.len(), 1, "{lines:?}");
            assert_eq!(lines[0].line, 4, "the line number is the one the user sees");
        }
        other => panic!("expected the bad line to be reported, got {other:?}"),
    }
    assert!(service.user_state().rules.is_empty(), "nothing was saved");

    let saved = service
        .apply_raw_rules("DOMAIN,example.org,DIRECT\nDOMAIN-SUFFIX,example.net,DIRECT")
        .await
        .unwrap();
    assert_eq!(saved, 2);
    assert_eq!(service.snapshot().rules.len(), 2);
}

#[tokio::test]
async fn moving_and_disabling_rules_keeps_their_order_consistent() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();

    let first = service
        .add_rule(rule("DOMAIN,one.example,DIRECT"))
        .await
        .unwrap();
    let second = service
        .add_rule(rule("DOMAIN,two.example,DIRECT"))
        .await
        .unwrap();
    service.move_rule(&second, 0).await.unwrap();
    service
        .update_rule(&first, rule("DOMAIN,one.example,DIRECT"), false)
        .await
        .unwrap();

    let snapshot = service.snapshot();
    let order: Vec<&str> = snapshot.rules.iter().map(|r| r.payload.as_str()).collect();
    assert_eq!(order, vec!["two.example", "one.example"]);
    assert!(!snapshot.rules[1].enabled);
    let orders: Vec<u32> = service.user_state().rules.iter().map(|r| r.order).collect();
    assert_eq!(orders, vec![0, 1], "the stored positions are consecutive");
}

#[tokio::test]
async fn removing_a_subscription_removes_its_cache_and_its_secrets() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, Some(HEADER)).await;
    service.refresh_subscription(&id).await.unwrap();
    let cache_file: PathBuf = paths_in(&dir).subscription_cache_file(&id).unwrap();
    assert!(cache_file.exists(), "the payload was cached");

    service.remove_subscription(&id).await.unwrap();

    assert!(service.snapshot().subscriptions.is_empty());
    assert!(!cache_file.exists(), "the cached payload is gone");
    assert!(
        service.snapshot().nodes.is_empty(),
        "its nodes left the configuration"
    );
}

#[tokio::test]
async fn the_snapshot_publishes_every_change_to_subscribers() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    let mut updates = service.subscribe();

    service.connect().await.unwrap();

    assert!(updates.has_changed().unwrap(), "a change was published");
    assert_eq!(
        updates.borrow_and_update().engine.phase,
        EnginePhase::Running
    );
}

#[tokio::test]
async fn the_mode_and_the_stop_path_reach_the_engine() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();

    service.set_mode(ProxyMode::Direct).await.unwrap();
    assert_eq!(service.snapshot().mode, ProxyMode::Direct);
    let calls = engine.calls();
    assert!(matches!(calls.last(), Some(Call::Reload(config)) if config.mode == ProxyMode::Direct));

    service.disconnect().await.unwrap();
    assert!(matches!(engine.calls().last(), Some(Call::Stop)));
    assert_eq!(service.snapshot().engine.phase, EnginePhase::Stopped);
}

// Regression: a new user pinned 7890/7891 in the defaults, so a subscription's
// own port never reached the engine. The port must come from the subscription.
#[tokio::test]
async fn a_new_users_subscription_port_reaches_the_engine() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    service.connect().await.unwrap();

    assert_eq!(last_config(&engine.calls()).mixed_port, Some(7899));
}

#[tokio::test]
async fn selecting_a_node_while_running_switches_it_live_without_a_reload() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();
    let before = engine.calls().len();

    let node_b = node_id(&service, "Airport / Node B");
    service.select_node(&node_b).await.unwrap();

    assert_eq!(
        engine.calls()[before..].to_vec(),
        vec![Call::Select {
            group: "Airport / Auto".to_string(),
            member: "Airport / Node B".to_string(),
        }],
        "the running engine is switched, and nothing is reloaded"
    );
    assert_eq!(
        service.user_state().preferences.selected_node_id.as_deref(),
        Some(node_b.as_str())
    );
}

#[tokio::test]
async fn selecting_a_node_while_stopped_only_saves_the_choice() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let node_b = node_id(&service, "Airport / Node B");
    service.select_node(&node_b).await.unwrap();

    assert!(
        engine
            .calls()
            .iter()
            .all(|call| !matches!(call, Call::Select { .. })),
        "a stopped engine is not called"
    );
    assert_eq!(
        service.user_state().preferences.selected_node_id.as_deref(),
        Some(node_b.as_str())
    );
}

#[tokio::test]
async fn a_refused_live_selection_saves_nothing() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();
    engine.fail_selects(true);

    let node_b = node_id(&service, "Airport / Node B");
    assert!(service.select_node(&node_b).await.is_err());

    assert_eq!(
        service.user_state().preferences.selected_node_id,
        None,
        "the saved choice and the running engine must not differ"
    );
}

#[tokio::test]
async fn latency_is_recorded_per_node_and_untested_nodes_stay_empty() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();
    engine.set_delay("Airport / Node A", 57);
    // Node B has no delay set, so its probe times out.

    service.test_latency().await.unwrap();

    let nodes = service.snapshot().nodes;
    let latency = |name: &str| {
        nodes
            .iter()
            .find(|node| node.name == name)
            .expect("the node is listed")
            .latency_ms
    };
    assert_eq!(latency("Airport / Node A"), Some(57));
    assert_eq!(latency("Airport / Node B"), None);
}

#[tokio::test]
async fn latency_is_forgotten_when_the_proxy_stops() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();
    engine.set_delay("Airport / Node A", 57);
    service.test_latency().await.unwrap();

    service.disconnect().await.unwrap();
    service.connect().await.unwrap();

    assert!(
        service
            .snapshot()
            .nodes
            .iter()
            .all(|node| node.latency_ms.is_none()),
        "a measurement from a stopped proxy is not shown"
    );
}

#[tokio::test]
async fn latency_refuses_with_a_clear_message_when_the_proxy_is_stopped() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    match service.test_latency().await {
        Err(AppError::Invalid(message)) => {
            assert_eq!(message, "Connect first to test latency");
        }
        other => panic!("expected an invalid-request error, got {other:?}"),
    }
}

// ----- the connect rule ------------------------------------------------------

/// The reasons the connect rule gives, as the snapshot and the errors carry them.
const NO_SUBSCRIPTION: &str = "Add a subscription first";
const NO_SERVERS: &str = "Refresh your subscription — it has no servers yet";
const NO_ENGINE: &str = "Install the engine first.";

fn is_invalid(result: &Result<(), AppError>, reason: &str) -> bool {
    matches!(result, Err(AppError::Invalid(text)) if text == reason)
}

#[tokio::test]
async fn connecting_without_a_subscription_is_refused_before_the_engine_is_touched() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    pin_english(&mut service).await;
    assert_eq!(
        service.snapshot().connect_blocked.as_deref(),
        Some(NO_SUBSCRIPTION)
    );

    let connect = service.connect().await;
    assert!(is_invalid(&connect, NO_SUBSCRIPTION), "{connect:?}");
    let restart = service.restart().await;
    assert!(is_invalid(&restart, NO_SUBSCRIPTION), "{restart:?}");

    assert!(
        engine.calls().is_empty(),
        "no validation or start reached the engine: {:?}",
        engine.calls()
    );
    assert_eq!(service.snapshot().engine.phase, EnginePhase::Stopped);
}

#[tokio::test]
async fn a_subscription_that_never_fetched_blocks_connecting_until_it_has_servers() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    pin_english(&mut service).await;
    let id = add_airport(&mut service, None).await;
    assert_eq!(
        service.snapshot().connect_blocked.as_deref(),
        Some(NO_SERVERS)
    );

    let connect = service.connect().await;
    assert!(is_invalid(&connect, NO_SERVERS), "{connect:?}");
    assert!(engine.calls().is_empty(), "{:?}", engine.calls());

    service.refresh_subscription(&id).await.unwrap();
    assert_eq!(service.snapshot().connect_blocked, None);
    service.connect().await.unwrap();
    assert_eq!(service.snapshot().engine.phase, EnginePhase::Running);
}

#[tokio::test]
async fn an_engine_managed_subscription_allows_connecting() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    service
        .add_subscription(
            "Airport".to_string(),
            SUB_URL.to_string(),
            None,
            SubscriptionMode::EngineManaged,
        )
        .await
        .unwrap();
    assert_eq!(service.snapshot().connect_blocked, None);

    service.connect().await.unwrap();
    assert_eq!(service.snapshot().engine.phase, EnginePhase::Running);
}

#[tokio::test]
async fn connecting_is_refused_while_the_engine_is_not_installed() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = AppService::new(ServiceDeps {
        store: Store::new(paths_in(&dir)),
        secrets: SelectedStore {
            store: Box::new(MockSecretStore::new()),
            fallback_reason: None,
        },
        engine: Box::new(engine.clone()),
        provider,
        engine_installed: false,
    });
    service.load().await.unwrap();
    pin_english(&mut service).await;
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    assert_eq!(
        service.snapshot().connect_blocked.as_deref(),
        Some(NO_ENGINE)
    );
    let connect = service.connect().await;
    assert!(is_invalid(&connect, NO_ENGINE), "{connect:?}");
    assert!(engine.calls().is_empty(), "{:?}", engine.calls());
}

#[tokio::test]
async fn removing_the_last_usable_subscription_stops_a_running_proxy_and_says_why() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();
    let before = engine.calls().len();

    service.remove_subscription(&id).await.unwrap();

    assert_eq!(
        engine.calls()[before..].to_vec(),
        vec![Call::Stop],
        "the proxy is stopped, not reloaded with no servers"
    );
    let snapshot = service.snapshot();
    assert_eq!(snapshot.engine.phase, EnginePhase::Stopped);
    assert_eq!(snapshot.connect_blocked.as_deref(), Some(NO_SUBSCRIPTION));
    let (kind, text) = snapshot.notice.clone().expect("a notice explains the stop");
    assert_eq!(kind, NoticeKind::Warning);
    assert!(text.contains("no subscription has servers"), "{text}");

    let connect = service.connect().await;
    assert!(is_invalid(&connect, NO_SUBSCRIPTION), "{connect:?}");
}

#[tokio::test]
async fn removing_one_of_two_subscriptions_keeps_a_running_proxy_on_the_other() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let first = add_airport(&mut service, None).await;
    service.refresh_subscription(&first).await.unwrap();
    let second = service
        .add_subscription(
            "Second".to_string(),
            SUB_URL.to_string(),
            None,
            SubscriptionMode::AppManaged,
        )
        .await
        .unwrap();
    service.refresh_subscription(&second).await.unwrap();
    service.connect().await.unwrap();

    service.remove_subscription(&first).await.unwrap();

    let snapshot = service.snapshot();
    assert_eq!(snapshot.engine.phase, EnginePhase::Running);
    assert_eq!(snapshot.connect_blocked, None);
    assert!(
        matches!(engine.calls().last(), Some(Call::Reload(_))),
        "the running proxy is reloaded on the remaining subscription"
    );
    assert!(
        snapshot
            .nodes
            .iter()
            .all(|node| node.name.starts_with("Second / ")),
        "{:?}",
        snapshot.nodes
    );
}

#[tokio::test]
async fn clearing_the_cache_of_the_last_usable_subscription_stops_a_running_proxy() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();

    service.clear_cache().await.unwrap();

    assert!(matches!(engine.calls().last(), Some(Call::Stop)));
    let snapshot = service.snapshot();
    assert_eq!(snapshot.engine.phase, EnginePhase::Stopped);
    assert_eq!(snapshot.connect_blocked.as_deref(), Some(NO_SERVERS));
}

#[tokio::test]
async fn a_proxy_left_running_without_a_usable_subscription_can_be_disconnected_but_not_restarted()
{
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    // An engine an earlier run left running.
    engine.lock().phase = EnginePhase::Running;
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    assert_eq!(service.snapshot().engine.phase, EnginePhase::Running);

    // It is left alone at load, and a restart is refused without stopping it.
    let restart = service.restart().await;
    assert!(is_invalid(&restart, NO_SUBSCRIPTION), "{restart:?}");
    assert!(
        engine
            .calls()
            .iter()
            .all(|call| !matches!(call, Call::Stop)),
        "a refused restart does not stop the proxy: {:?}",
        engine.calls()
    );

    service.disconnect().await.unwrap();
    assert!(matches!(engine.calls().last(), Some(Call::Stop)));
    assert_eq!(service.snapshot().engine.phase, EnginePhase::Stopped);
}

#[tokio::test]
#[ignore = "needs the meow engine installed; run with --ignored"]
async fn the_installed_meow_engine_accepts_the_central_fixture() {
    if !starpod_engine::download::is_installed() {
        eprintln!("skipped: the meow engine is not installed");
        return;
    }
    let dir = TempDir::new().unwrap();
    let paths = paths_in(&dir);
    let engine = starpod_engine::MeowEngine::installed(paths.runtime_config_file());
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = AppService::new(ServiceDeps {
        store: Store::new(paths),
        secrets: SelectedStore {
            store: Box::new(MockSecretStore::new()),
            fallback_reason: None,
        },
        engine: Box::new(engine),
        provider,
        engine_installed: true,
    });
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let check = service.check_configuration().await.unwrap();

    assert!(
        check.accepted,
        "the real engine rejected the central fixture: {:?}",
        check.engine_error
    );
}

// ----- engine-managed subscriptions (ARCHITECTURE §4.6, R12) ------------------

/// The user's rule as the engine spells it, and the app's terminal rule.
const USER_RULE: &str = "DOMAIN,example.org,DIRECT";
const TAIL: &str = "MATCH,PROXY";
/// A rule the engine's engine-managed subscription supplies.
const SUB_RULE: &str = "DOMAIN,sub.example,PROXY";

impl FakeEngine {
    fn set_live_rules(&self, rules: &[&str]) {
        self.lock().live_rules = rules.iter().map(|rule| rule.to_string()).collect();
    }

    fn set_engine_subs(&self, last_updated: i64) {
        self.lock().engine_subs = vec![EngineSubscription {
            name: "Airport".to_string(),
            last_updated: Some(serde_json::json!(last_updated)),
        }];
    }

    fn set_live_proxies(&self, proxies: Vec<LiveProxy>) {
        self.lock().live_proxies = proxies;
    }

    fn replace_calls(&self) -> Vec<Vec<String>> {
        self.calls()
            .into_iter()
            .filter_map(|call| match call {
                Call::ReplaceRules(rules) => Some(rules),
                _ => None,
            })
            .collect()
    }
}

/// A running service with one engine-managed subscription and one user rule.
async fn running_with_engine_managed(dir: &TempDir, engine: &FakeEngine) -> AppService {
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(dir, engine, provider);
    service.load().await.unwrap();
    service
        .add_subscription(
            "Airport".to_string(),
            SUB_URL.to_string(),
            None,
            SubscriptionMode::EngineManaged,
        )
        .await
        .expect("the engine-managed subscription is added");
    service.add_rule(rule(USER_RULE)).await.unwrap();
    service.connect().await.unwrap();
    service
}

#[tokio::test]
async fn an_engine_refresh_puts_the_user_rules_back_in_front_and_only_when_it_refreshed() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let mut service = running_with_engine_managed(&dir, &engine).await;

    // The engine applied the subscription's rule ahead of the user's.
    engine.set_engine_subs(1);
    engine.set_live_rules(&[SUB_RULE, USER_RULE, TAIL]);

    // The first look after a start counts as a refresh.
    service.check_engine_subscriptions().await;
    assert_eq!(
        engine.replace_calls(),
        vec![vec![
            USER_RULE.to_string(),
            SUB_RULE.to_string(),
            TAIL.to_string()
        ]],
        "the user's rule comes first, the subscription's follows, MATCH is last"
    );

    // Nothing changed: no second write.
    service.check_engine_subscriptions().await;
    assert_eq!(engine.replace_calls().len(), 1);

    // A refresh (new last_updated) with the order inverted again is corrected.
    engine.set_engine_subs(2);
    engine.set_live_rules(&[SUB_RULE, USER_RULE, TAIL]);
    service.check_engine_subscriptions().await;
    assert_eq!(engine.replace_calls().len(), 2);

    // The order is inverted but nothing refreshed: the watch does not fire.
    engine.set_live_rules(&[SUB_RULE, USER_RULE, TAIL]);
    service.check_engine_subscriptions().await;
    assert_eq!(engine.replace_calls().len(), 2);

    let log = service.snapshot().log_lines.join("\n");
    assert!(
        log.contains("restored the user's rule order"),
        "the restore is logged: {log}"
    );
}

#[tokio::test]
async fn without_an_engine_managed_subscription_or_a_running_engine_the_engine_is_not_asked() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    // An app-managed subscription with servers, so the connect rule allows
    // the start. It is not engine-managed, which is what this test checks.
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.add_rule(rule(USER_RULE)).await.unwrap();
    service.connect().await.unwrap();

    engine.set_engine_subs(1);
    engine.set_live_rules(&[SUB_RULE, USER_RULE, TAIL]);
    service.check_engine_subscriptions().await;
    assert!(
        engine.replace_calls().is_empty(),
        "no engine-managed subscription, so no rule write"
    );

    service.disconnect().await.unwrap();
    service.check_engine_subscriptions().await;
    assert!(
        engine.replace_calls().is_empty(),
        "a stopped engine is not asked"
    );
}

#[tokio::test]
async fn an_engine_managed_node_that_shadows_a_users_node_is_reported_in_the_snapshot() {
    let dir = TempDir::new().unwrap();
    let mut state = starpod_store::UserState::default();
    state.nodes.push(starpod_core::domain::Node {
        id: "home".to_string(),
        name: "Home".to_string(),
        protocol: "trojan".to_string(),
        server: Some("home.example".to_string()),
        port: Some(443),
        tags: Vec::new(),
        raw: serde_json::Map::new(),
    });
    Store::new(paths_in(&dir)).save_user_state(&state).unwrap();

    let engine = FakeEngine::default();
    let mut service = running_with_engine_managed(&dir, &engine).await;

    // The subscription's refresh brought a node named "Home" at another server.
    engine.set_engine_subs(1);
    engine.set_live_proxies(vec![LiveProxy {
        name: "Home".to_string(),
        server: Some("remote.example".to_string()),
        port: Some(443),
        now: None,
    }]);
    service.check_engine_subscriptions().await;

    let warnings = service.snapshot().warnings;
    let shadow = warnings
        .iter()
        .find(|warning| warning.contains("“Home”"))
        .unwrap_or_else(|| panic!("the shadow is reported: {warnings:?}"));
    assert!(
        shadow.contains("“Airport”"),
        "it names the subscription: {shadow}"
    );
    assert!(
        shadow.contains("app-managed"),
        "it suggests the switch: {shadow}"
    );

    // The next refresh brings the user's own server back: the warning clears.
    engine.set_engine_subs(2);
    engine.set_live_proxies(vec![LiveProxy {
        name: "Home".to_string(),
        server: Some("home.example".to_string()),
        port: Some(443),
        now: None,
    }]);
    service.check_engine_subscriptions().await;
    assert!(
        !service
            .snapshot()
            .warnings
            .iter()
            .any(|warning| warning.contains("“Home”")),
        "a resolved shadow is no longer reported"
    );
}

/// A subscription with a `select` group the user can choose a member of.
const PICK_FIXTURE: &str = "\
proxies:
  - name: \"Node A\"
    type: trojan
    server: a.example.com
    port: 443
    password: hunter2-trojan
  - name: \"Node B\"
    type: trojan
    server: b.example.com
    port: 443
    password: hunter2-trojan
proxy-groups:
  - name: \"Pick\"
    type: select
    proxies:
      - \"Node A\"
      - \"Node B\"
rules:
  - MATCH,Pick
";

#[tokio::test]
async fn a_chosen_member_of_a_select_group_is_saved_and_shown() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> = Arc::new(StaticSubscriptionProvider::new(
        PICK_FIXTURE.as_bytes().to_vec(),
    ));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let snap = service.snapshot();
    let group = snap
        .groups
        .iter()
        .find(|group| group.name == "Airport / Pick")
        .expect("the select group is listed");
    assert_eq!(group.kind, "select");
    assert_eq!(
        group.selected.as_deref(),
        Some("Airport / Node A"),
        "first member by default"
    );
    assert_eq!(
        snap.primary_group.as_deref(),
        Some("Airport / Pick"),
        "the subscription's own MATCH names the group the Home page offers first"
    );
    let shown = snap
        .groups
        .iter()
        .find(|group| group.name == "Airport / Pick")
        .expect("the group is listed");
    assert_eq!(
        shown.label, "Pick",
        "the subscription's prefix is not shown"
    );
    assert_eq!(shown.members[0].label, "Node A");
    assert_eq!(
        shown.members[0].name, "Airport / Node A",
        "the engine name is kept"
    );

    service
        .select_member("Airport / Pick", "Airport / Node B")
        .await
        .unwrap();
    let snap = service.snapshot();
    let group = snap
        .groups
        .iter()
        .find(|group| group.name == "Airport / Pick")
        .unwrap();
    assert_eq!(group.selected.as_deref(), Some("Airport / Node B"));
    assert_eq!(
        group.members.first().map(|member| member.name.as_str()),
        Some("Airport / Node B"),
        "the chosen member is first in the built configuration"
    );
    assert_eq!(
        service
            .user_state()
            .preferences
            .group_selections
            .get("Airport / Pick")
            .map(String::as_str),
        Some("Airport / Node B")
    );
}

#[tokio::test]
async fn a_member_choice_is_refused_for_an_auto_group_or_an_unknown_member() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(FIXTURE.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    assert!(matches!(
        service
            .select_member("Airport / Auto", "Airport / Node A")
            .await,
        Err(AppError::Invalid(_))
    ));
    assert!(matches!(
        service.select_member("Nowhere", "Airport / Node A").await,
        Err(AppError::Invalid(_))
    ));
    assert!(service.user_state().preferences.group_selections.is_empty());
}

#[tokio::test]
async fn a_running_engines_live_choice_is_what_a_group_shows() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> = Arc::new(StaticSubscriptionProvider::new(
        PICK_FIXTURE.as_bytes().to_vec(),
    ));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    service.connect().await.unwrap();

    engine.set_live_proxies(vec![LiveProxy {
        name: "Airport / Pick".to_string(),
        server: None,
        port: None,
        now: Some("Airport / Node B".to_string()),
    }]);
    service.refresh_engine_status().await;

    let group = service
        .snapshot()
        .groups
        .into_iter()
        .find(|group| group.name == "Airport / Pick")
        .unwrap();
    assert_eq!(group.selected.as_deref(), Some("Airport / Node B"));
}

#[tokio::test]
async fn a_subscriptions_announcement_and_profile_title_reach_the_snapshot() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::with_response(
            FetchResponse::new(PICK_FIXTURE.as_bytes().to_vec())
                .with_header("profile-title", "Foxgirls VPN")
                .with_header("announce", "Maintenance on Nov 20")
                .with_header("support-url", "https://help.example.com"),
        ));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let snap = service.snapshot();
    assert_eq!(snap.subscriptions[0].title, "Foxgirls VPN");
    assert_eq!(snap.announcements.len(), 1);
    let announcement = &snap.announcements[0];
    assert_eq!(announcement.title, "Foxgirls VPN");
    assert_eq!(announcement.text, "Maintenance on Nov 20");
    assert_eq!(
        announcement.support_url.as_deref(),
        Some("https://help.example.com")
    );
    assert_eq!(announcement.web_url, None);
}

#[tokio::test]
async fn a_refresh_sends_the_device_headers_until_the_user_turns_them_off() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider = Arc::new(Scripted::new(vec![
        Ok(FetchResponse::new(PICK_FIXTURE.as_bytes().to_vec())),
        Ok(FetchResponse::new(PICK_FIXTURE.as_bytes().to_vec())),
    ]));
    let mut service = build_service(&dir, &engine, provider.clone());
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let mut prefs = service.user_state().preferences.clone();
    assert!(
        prefs.send_device_id,
        "device identification is on by default"
    );
    prefs.send_device_id = false;
    service.save_preferences(prefs).await.unwrap();
    service.refresh_subscription(&id).await.unwrap();

    let requests = provider.requests();
    let device = requests[0]
        .device
        .as_ref()
        .expect("the first refresh carries the device headers");
    assert_eq!(device.hwid.len(), 32);
    assert!(!device.os.is_empty() && !device.os_version.is_empty() && !device.model.is_empty());
    assert!(
        requests[1].device.is_none(),
        "with the setting off, no device header is sent"
    );
}

#[tokio::test]
async fn a_device_limit_keeps_the_payload_and_shows_the_support_link() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> = Arc::new(Scripted::new(vec![
        Ok(FetchResponse::new(PICK_FIXTURE.as_bytes().to_vec())),
        Ok(FetchResponse::new(PICK_FIXTURE.as_bytes().to_vec())
            .with_header("x-hwid-limit", "true")
            .with_header("support-url", "https://help.example.com")),
    ]));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    let nodes_before = service.snapshot().subscriptions[0].node_count;

    let error = service
        .refresh_subscription(&id)
        .await
        .expect_err("the device limit fails the refresh");
    assert!(error.to_string().contains("device limit"), "{error}");

    let snap = service.snapshot();
    assert!(snap.subscriptions[0].device_limit);
    assert_eq!(
        snap.subscriptions[0].node_count, nodes_before,
        "the payload from before the limit is still in use"
    );
    assert!(snap.announcements.iter().any(|announcement| {
        announcement.text == starpod_app::snapshot::DEVICE_LIMIT_MESSAGE
            && announcement.support_url.as_deref() == Some("https://help.example.com")
    }));
    let card = snap
        .profile_card
        .expect("the active subscription has a card");
    assert!(card.device_limit);
}

#[tokio::test]
async fn a_subscription_that_forbids_global_mode_blocks_it_and_turns_a_saved_global_into_rule() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::with_response(
            FetchResponse::new(PICK_FIXTURE.as_bytes().to_vec())
                .with_header("global-mode", "false"),
        ));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    assert!(!service.snapshot().global_mode_allowed);
    assert!(
        service
            .set_mode(starpod_core::ProxyMode::Global)
            .await
            .is_err(),
        "the Global mode is refused while the subscription forbids it"
    );

    let mut prefs = service.user_state().preferences.clone();
    prefs.mode = Some(starpod_core::ProxyMode::Global);
    service.save_preferences(prefs).await.unwrap();
    assert_eq!(
        service.snapshot().mode,
        starpod_core::ProxyMode::Rule,
        "a saved Global choice runs as Rule while the subscription forbids it"
    );
}

#[tokio::test]
async fn the_profile_headers_reach_the_card_and_the_provider_interval_becomes_the_schedule() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::with_response(
            FetchResponse::new(PICK_FIXTURE.as_bytes().to_vec())
                .with_header("profile-title", "Foxgirls VPN")
                .with_header("profile-web-page-name", "Cabinet")
                .with_header("profile-web-page-url", "https://example.com/account")
                .with_header("announce-url", "https://example.com/news")
                .with_header("profile-update-interval", "6"),
        ));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();

    let snap = service.snapshot();
    assert_eq!(snap.subscriptions[0].interval_secs, 6 * 3600);
    assert_eq!(
        snap.subscriptions[0].web_page_name.as_deref(),
        Some("Cabinet")
    );
    let card = snap
        .profile_card
        .expect("the active subscription has a card");
    assert_eq!(card.title, "Foxgirls VPN");
    assert_eq!(card.web_url.as_deref(), Some("https://example.com/account"));
    assert_eq!(card.web_page_name.as_deref(), Some("Cabinet"));
    assert_eq!(
        card.announce_url.as_deref(),
        Some("https://example.com/news")
    );
}

// ----- the default server of the primary group ---------------------------------

/// A subscription whose own select group is its MATCH target. The group lists
/// a fallback group first and the "auto" url-test group last, so the default
/// has to pick the url-test group by its name, not by its position.
const CHOOSER: &str = "\
mixed-port: 7899
proxies:
  - name: \"Node A\"
    type: trojan
    server: a.example.com
    port: 443
    password: hunter2-trojan
  - name: \"Node B\"
    type: vmess
    server: b.example.com
    port: 8443
    uuid: 00000000-0000-0000-0000-000000000001
    alterId: 0
    cipher: auto
proxy-groups:
  - name: \"Pick\"
    type: select
    proxies:
      - \"Steady\"
      - \"Node A\"
      - \"Node B\"
      - \"⚡ Auto\"
  - name: \"Steady\"
    type: fallback
    proxies:
      - \"Node A\"
      - \"Node B\"
    url: https://example.com/generate_204
    interval: 300
  - name: \"⚡ Auto\"
    type: url-test
    proxies:
      - \"Node B\"
      - \"Node A\"
    url: https://example.com/generate_204
    interval: 300
rules:
  - MATCH,Pick
";

/// The primary group of the chooser subscription, and its automatic member,
/// both as the app names them.
fn chooser_names(service: &AppService) -> (String, String) {
    let primary = service
        .snapshot()
        .primary_group
        .expect("the subscription has a primary group");
    (primary, "Airport / ⚡ Auto".to_string())
}

#[tokio::test]
async fn the_primary_group_starts_on_its_auto_member_until_the_user_chooses() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(CHOOSER.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    let (primary, auto) = chooser_names(&service);

    // Stopped: the snapshot already shows the auto member.
    let shown = service
        .snapshot()
        .groups
        .iter()
        .find(|group| group.name == primary)
        .and_then(|group| group.selected.clone());
    assert_eq!(shown.as_deref(), Some(auto.as_str()));

    service.connect().await.unwrap();

    let calls = engine.calls();
    assert_eq!(
        calls.last(),
        Some(&Call::Select {
            group: primary.clone(),
            member: auto.clone(),
        }),
        "the running engine is told the default member: {calls:?}"
    );
    assert!(
        service.user_state().preferences.group_selections.is_empty(),
        "a default is not saved as a choice"
    );
    let shown = service
        .snapshot()
        .groups
        .iter()
        .find(|group| group.name == primary)
        .and_then(|group| group.selected.clone());
    assert_eq!(shown.as_deref(), Some(auto.as_str()));
}

#[tokio::test]
async fn a_user_choice_in_the_primary_group_replaces_the_default_on_every_build() {
    let dir = TempDir::new().unwrap();
    let engine = FakeEngine::default();
    let provider: Arc<dyn SubscriptionProvider> =
        Arc::new(StaticSubscriptionProvider::new(CHOOSER.as_bytes().to_vec()));
    let mut service = build_service(&dir, &engine, provider);
    service.load().await.unwrap();
    let id = add_airport(&mut service, None).await;
    service.refresh_subscription(&id).await.unwrap();
    let (primary, auto) = chooser_names(&service);
    service.connect().await.unwrap();

    service
        .select_member(&primary, "Airport / Node B")
        .await
        .unwrap();
    let shown = service
        .snapshot()
        .groups
        .iter()
        .find(|group| group.name == primary)
        .and_then(|group| group.selected.clone());
    assert_eq!(shown.as_deref(), Some("Airport / Node B"));

    // A rebuild while running must not switch back to the default.
    let before = engine.calls().len();
    service
        .add_rule(rule("DOMAIN-SUFFIX,example.net,DIRECT"))
        .await
        .unwrap();
    let later = engine.calls()[before..].to_vec();
    assert!(
        !later.contains(&Call::Select {
            group: primary.clone(),
            member: auto.clone(),
        }),
        "the user's choice wins over the default: {later:?}"
    );
    assert_eq!(
        service
            .user_state()
            .preferences
            .group_selections
            .get(&primary)
            .map(String::as_str),
        Some("Airport / Node B")
    );
}
