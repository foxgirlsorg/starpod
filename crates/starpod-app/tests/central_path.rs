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
            Call::Stop | Call::Select { .. } => None,
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
            "MATCH,PROXY".to_string(),
        ],
        "the user's rules come first, in the user's order, then the subscription's"
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
        "every rule the subscription supplies is listed, including the MATCH the app replaces"
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
