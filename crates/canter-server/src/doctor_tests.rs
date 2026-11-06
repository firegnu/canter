use super::*;
use canter_protocol::{DoctorResponse, DoctorStatus};
use serde_json::json;

struct Harness {
    client: reqwest::Client,
    server: tokio::task::JoinHandle<()>,
    root: tempfile::TempDir,
}

impl Harness {
    async fn new(config: serde_json::Value) -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-doctor-")
            .tempdir_in("/tmp")
            .unwrap();
        Self::build(root, config, None).await
    }

    async fn chatgpt(
        config: serde_json::Value,
        catalog: &str,
        fake: &crate::chatgpt_auth::test_support::Fake,
    ) -> (Self, crate::chatgpt_auth::ChatgptAuth) {
        let root = tempfile::Builder::new()
            .prefix("canter-doctor-chatgpt-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = root.path().join("state");
        let now = fake.state.lock().unwrap().now.clone();
        let auth = crate::chatgpt_auth::ChatgptAuth::for_test(
            &home,
            fake.origin.clone(),
            Arc::new(move || now.load(std::sync::atomic::Ordering::SeqCst)),
        );
        let attempt = auth.start_login(None).await.unwrap();
        assert!(fake.callback(&attempt).await.status().is_success());
        let account = attempt.wait().await.unwrap();
        auth.complete_login(&account.id, true).await.unwrap();
        let source = crate::model_auth::ModelAuth::for_test(
            home,
            auth.clone(),
            reqwest::Url::parse(catalog).unwrap(),
        );
        let test = Self::build(root, config, Some(source)).await;
        (test, auth)
    }

    async fn build(
        root: tempfile::TempDir,
        config: serde_json::Value,
        auth: Option<Arc<crate::model_auth::ModelAuth>>,
    ) -> Self {
        let home = root.path().join("state");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("config.json"), config.to_string()).unwrap();
        let loaded = crate::config::Config::load_read_only(&home).unwrap();
        let mcp = canter_tools::mcp::Registry::connect(
            loaded.mcp_servers.clone(),
            home.clone(),
            CancellationToken::new(),
            CancellationToken::new(),
        )
        .await;
        let model: Arc<dyn Model> = match auth.clone() {
            Some(auth) => loaded.model_client(Some(auth)).unwrap(),
            None => Arc::new(
                canter_model::ChatCompletions::new(
                    "http://127.0.0.1:9",
                    canter_model::ApiKey::new("test-only-unused"),
                )
                .unwrap(),
            ),
        };
        let lsp = canter_tools::lsp::LspService::new(Default::default()).unwrap();
        let reload = match auth {
            Some(auth) => Some(
                crate::reload::LiveConfig::connect(
                    home.clone(),
                    loaded.clone(),
                    model.clone(),
                    lsp.clone(),
                    CancellationToken::new(),
                    auth,
                )
                .await,
            ),
            None => None,
        };
        let app = Arc::new(App {
            reload,
            home: home.clone(),
            store: Arc::new(Store::new(home.join("sessions")).unwrap()),
            model,
            default_model: loaded.model.clone(),
            models: loaded.models.clone(),
            options: loaded.options(),
            mcp,
            lsp,
            sessions: Default::default(),
            session_locks: Default::default(),
            registry_changed: Default::default(),
            managed: Default::default(),
            pauses: Default::default(),
            draining: Default::default(),
            stopped: CancellationToken::new(),
        });
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(app)).await.unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .unix_socket(socket)
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        Self {
            client,
            server,
            root,
        }
    }

    async fn get(&self, version: &str, protocol: u32) -> DoctorResponse {
        let mut url = reqwest::Url::parse("http://localhost/v1/doctor").unwrap();
        url.query_pairs_mut()
            .append_pair("clientVersion", version)
            .append_pair("clientProtocolVersion", &protocol.to_string());
        self.client
            .get(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    fn home(&self) -> PathBuf {
        self.root.path().join("state")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn valid_config() -> serde_json::Value {
    json!({
        "base_url": "http://127.0.0.1:9",
        "api_key": "test-only-doctor-key",
        "model": "fake",
        "context_window": 1048576,
        "sandbox": false
    })
}

fn check<'a>(report: &'a DoctorResponse, id: &str) -> &'a canter_protocol::DoctorCheck {
    report
        .data
        .iter()
        .find(|check| check.id == id)
        .unwrap_or_else(|| panic!("missing {id}: {report:?}"))
}

#[tokio::test]
async fn doctor_reports_valid_configuration_and_matching_versions() {
    let test = Harness::new(valid_config()).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(check(&report, "config").status, DoctorStatus::Ok);
    assert_eq!(check(&report, "rules").status, DoctorStatus::Ok);
    assert_eq!(check(&report, "tui").status, DoctorStatus::Ok);
    assert_eq!(check(&report, "agents").status, DoctorStatus::Ok);
    assert_eq!(check(&report, "state").status, DoctorStatus::Ok);
    assert_eq!(check(&report, "version").status, DoctorStatus::Ok);
    assert_eq!(report.server_version, crate::VERSION);
    assert_eq!(report.server_protocol_version, PROTOCOL_VERSION);
}

#[tokio::test]
async fn doctor_reports_invalid_files_unwritable_state_and_version_mismatch() {
    let test = Harness::new(valid_config()).await;
    std::fs::write(test.home().join("config.json"), "[").unwrap();
    std::fs::write(
        test.home().join("rules.json"),
        json!({"rules":[{"pattern":[],"decision":"allow"}]}).to_string(),
    )
    .unwrap();
    std::fs::write(test.home().join("tui.json"), "{").unwrap();
    std::fs::write(test.home().join("agents"), "not a directory").unwrap();
    let report = test.get("0.0.0 · mismatch", 99).await;
    for id in ["config", "rules", "tui", "agents", "version"] {
        assert_eq!(check(&report, id).status, DoctorStatus::Fail, "{id}");
        assert!(check(&report, id).reason.is_some(), "{id}");
        assert!(check(&report, id).fix.is_some(), "{id}");
    }
    assert!(
        check(&report, "config")
            .reason
            .as_ref()
            .unwrap()
            .contains("line")
    );
    assert!(
        check(&report, "rules")
            .reason
            .as_ref()
            .unwrap()
            .contains("rules[0]")
    );
    assert!(
        check(&report, "version")
            .reason
            .as_ref()
            .unwrap()
            .contains("0.0.0 · mismatch")
    );

    std::fs::write(test.home().join("config.json"), valid_config().to_string()).unwrap();
    std::fs::remove_file(test.home().join("agents")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(test.home(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    std::fs::set_permissions(test.home(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(check(&report, "state").status, DoctorStatus::Fail);
}

#[tokio::test]
async fn doctor_reports_rule_fields_without_returning_the_configured_pattern() {
    let test = Harness::new(valid_config()).await;
    std::fs::write(
        test.home().join("rules.json"),
        json!({"fileRules":[{"pattern":"test-only-secret-[","decision":"forbidden"}]}).to_string(),
    )
    .unwrap();
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    let rules = check(&report, "rules");
    assert_eq!(rules.status, DoctorStatus::Fail);
    let reason = rules.reason.as_ref().unwrap();
    assert!(reason.contains("fileRules[0]"), "{reason}");
    assert!(reason.contains("invalid pattern"), "{reason}");
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("test-only-secret-")
    );
}

#[tokio::test]
async fn doctor_uses_existing_role_file_validation() {
    let test = Harness::new(valid_config()).await;
    std::fs::create_dir(test.home().join("agents")).unwrap();
    std::fs::write(
        test.home().join("agents/broken.md"),
        "---\nname: broken\ndescription: Broken.\nunexpected: field\n---\nText.",
    )
    .unwrap();
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    let agents = check(&report, "agents");
    assert_eq!(agents.status, DoctorStatus::Fail);
    let reason = agents.reason.as_ref().unwrap();
    assert!(reason.contains("broken.md"), "{reason}");
    assert!(reason.contains("Unknown role field"), "{reason}");
}

struct ProviderServer {
    base_url: String,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl ProviderServer {
    async fn new(status: u16, body: serde_json::Value, delay: Duration) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = requests.clone();
        let app = axum::Router::new().route(
            "/v1/models",
            axum::routing::get(move |headers: axum::http::HeaderMap| {
                let counted = counted.clone();
                let body = body.clone();
                async move {
                    assert!(headers.get("authorization").is_some_and(|value| {
                        value
                            .to_str()
                            .is_ok_and(|value| value.starts_with("Bearer "))
                    }));
                    counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(delay).await;
                    (
                        axum::http::StatusCode::from_u16(status).unwrap(),
                        axum::Json(body),
                    )
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base_url,
            requests,
            task,
        }
    }

    fn count(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for ProviderServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn legacy_at(base_url: &str) -> serde_json::Value {
    let mut config = valid_config();
    config["base_url"] = json!(base_url);
    config
}

#[tokio::test]
async fn doctor_checks_a_chat_completions_model_list_once() {
    let server = ProviderServer::new(200, json!({"data":[]}), Duration::ZERO).await;
    let test = Harness::new(legacy_at(&server.base_url)).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(check(&report, "provider.default").status, DoctorStatus::Ok);
    assert_eq!(server.count(), 1);
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("test-only-doctor-key"));
}

#[tokio::test]
async fn doctor_distinguishes_invalid_keys_and_missing_model_lists() {
    let unauthorized =
        ProviderServer::new(401, json!({"error":"private-response"}), Duration::ZERO).await;
    let test = Harness::new(legacy_at(&unauthorized.base_url)).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    let provider = check(&report, "provider.default");
    assert_eq!(provider.status, DoctorStatus::Fail);
    assert!(provider.reason.as_ref().unwrap().contains("401"));
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("private-response")
    );

    let missing = ProviderServer::new(404, json!({}), Duration::ZERO).await;
    let test = Harness::new(legacy_at(&missing.base_url)).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(
        check(&report, "provider.default").status,
        DoctorStatus::Skip
    );

    let invalid = ProviderServer::new(200, json!({}), Duration::ZERO).await;
    let test = Harness::new(legacy_at(&invalid.base_url)).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    let provider = check(&report, "provider.default");
    assert_eq!(provider.status, DoctorStatus::Skip);
    assert!(provider.reason.as_ref().unwrap().contains("unrecognized"));
    assert_eq!(invalid.count(), 1);
}

#[tokio::test]
async fn doctor_reports_unreachable_addresses() {
    let test = Harness::new(valid_config()).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    let provider = check(&report, "provider.default");
    assert_eq!(provider.status, DoctorStatus::Fail);
    assert!(provider.reason.as_ref().unwrap().contains("connect"));
}

#[tokio::test]
async fn doctor_times_out_one_provider_without_delaying_the_others() {
    let slow = ProviderServer::new(200, json!({"data":[]}), Duration::from_secs(6)).await;
    let fast = ProviderServer::new(200, json!({"data":[]}), Duration::ZERO).await;
    let config = json!({
        "model": "fast/test",
        "sandbox": false,
        "providers": {
            "slow": {"base_url": slow.base_url, "api_key": "test-only-doctor-key", "models": {"test": {"context_window": 200000}}},
            "fast": {"base_url": fast.base_url, "api_key": "test-only-doctor-key", "models": {"test": {"context_window": 200000}}}
        }
    });
    let test = Harness::new(config).await;
    let started = tokio::time::Instant::now();
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert!(started.elapsed() < Duration::from_secs(8));
    assert_eq!(check(&report, "provider.slow").status, DoctorStatus::Fail);
    assert!(
        check(&report, "provider.slow")
            .reason
            .as_ref()
            .unwrap()
            .contains("timed out")
    );
    assert_eq!(check(&report, "provider.fast").status, DoctorStatus::Ok);
    assert_eq!(slow.count(), 1);
    assert_eq!(fast.count(), 1);
}

struct McpServer {
    url: String,
    initializations: Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

impl McpServer {
    async fn new() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let initializations = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = initializations.clone();
        let app = axum::Router::new().route(
            "/mcp",
            axum::routing::post(move |axum::Json(request): axum::Json<serde_json::Value>| {
                let counted = counted.clone();
                async move {
                    let result = match request["method"].as_str().unwrap() {
                        "initialize" => {
                            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
                        }
                        "tools/list" => json!({"tools":[]}),
                        _ => json!({}),
                    };
                    (
                        [("mcp-session-id", "doctor-fixture")],
                        axum::Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})),
                    )
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            url,
            initializations,
            task,
        }
    }
}

impl Drop for McpServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn doctor_checks_enabled_and_disabled_sandbox() {
    let mut enabled = valid_config();
    enabled["sandbox"] = json!(true);
    let test = Harness::new(enabled).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(check(&report, "sandbox").status, DoctorStatus::Ok);

    let test = Harness::new(valid_config()).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(check(&report, "sandbox").status, DoctorStatus::Skip);
}

#[tokio::test]
async fn doctor_reads_mcp_connection_state_without_reconnecting() {
    let server = McpServer::new().await;
    let test = Harness::new(json!({
        "base_url": "http://127.0.0.1:9",
        "api_key": "test-only-doctor-key",
        "model": "fake",
        "context_window": 1048576,
        "sandbox": false,
        "mcp_servers": {"connected": {"transport":"http","url":server.url}}
    }))
    .await;
    assert_eq!(
        server
            .initializations
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(check(&report, "mcp.connected").status, DoctorStatus::Ok);
    assert_eq!(
        server
            .initializations
            .load(std::sync::atomic::Ordering::SeqCst),
        1
    );

    let test = Harness::new(json!({
        "base_url": "http://127.0.0.1:9",
        "api_key": "test-only-doctor-key",
        "model": "fake",
        "context_window": 1048576,
        "sandbox": false,
        "mcp_servers": {"broken": {"transport":"stdio","command":"/no/such/doctor-command"}}
    }))
    .await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    let mcp = check(&report, "mcp.broken");
    assert_eq!(mcp.status, DoctorStatus::Fail);
    assert!(mcp.fix.as_ref().unwrap().contains("config.json"));
}

fn chatgpt_config() -> serde_json::Value {
    json!({
        "model": "chatgpt/test",
        "sandbox": false,
        "providers": {"chatgpt": {
            "api": "responses",
            "auth": "chatgpt",
            "models": {"test": {"context_window": 200000}}
        }}
    })
}

#[tokio::test]
async fn doctor_checks_chatgpt_login_and_catalog_without_publishing_or_retrying() {
    let catalog = ProviderServer::new(200, json!({"models":[]}), Duration::ZERO).await;
    let fake = crate::chatgpt_auth::test_support::Fake::new().await;
    let (test, _auth) = Harness::chatgpt(chatgpt_config(), &catalog.base_url, &fake).await;
    let config_before = std::fs::read(test.home().join("config.json")).unwrap();
    let auth_before = std::fs::read_dir(test.home().join("auth/chatgpt"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(path).unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(check(&report, "config.active").status, DoctorStatus::Ok);
    assert_eq!(check(&report, "auth.chatgpt").status, DoctorStatus::Ok);
    assert_eq!(check(&report, "provider.chatgpt").status, DoctorStatus::Ok);
    assert_eq!(catalog.count(), 1);
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("fake-access"));
    assert!(!encoded.contains("fake-refresh"));
    assert_eq!(
        std::fs::read(test.home().join("config.json")).unwrap(),
        config_before
    );
    let auth_after = std::fs::read_dir(test.home().join("auth/chatgpt"))
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(path).unwrap(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(auth_after, auth_before);
    drop(test);
    fake.stop().await;
}

#[tokio::test]
async fn doctor_reports_chatgpt_unauthorized_and_needs_login() {
    let catalog =
        ProviderServer::new(401, json!({"error":"private-response"}), Duration::ZERO).await;
    let fake = crate::chatgpt_auth::test_support::Fake::new().await;
    let (test, auth) = Harness::chatgpt(chatgpt_config(), &catalog.base_url, &fake).await;
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(check(&report, "auth.chatgpt").status, DoctorStatus::Ok);
    let provider = check(&report, "provider.chatgpt");
    assert_eq!(provider.status, DoctorStatus::Fail);
    assert!(provider.reason.as_ref().unwrap().contains("401"));
    assert_eq!(catalog.count(), 1);
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("private-response")
    );

    let account = auth.current_record().await.unwrap();
    auth.logout(&account).await.unwrap();
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    assert_eq!(check(&report, "auth.chatgpt").status, DoctorStatus::Fail);
    assert!(
        check(&report, "auth.chatgpt")
            .fix
            .as_ref()
            .unwrap()
            .contains("canter auth login chatgpt")
    );
    assert_eq!(
        check(&report, "provider.chatgpt").status,
        DoctorStatus::Fail
    );
    assert_eq!(catalog.count(), 1);
    drop(test);
    fake.stop().await;
}

#[tokio::test]
async fn doctor_reports_a_failed_sandbox_probe() {
    let mut config = valid_config();
    config["sandbox"] = json!(true);
    let test = Harness::new(config).await;
    super::doctor::fail_sandbox_for_test(test.home());
    let report = test.get(crate::VERSION, PROTOCOL_VERSION).await;
    let sandbox = check(&report, "sandbox");
    assert_eq!(sandbox.status, DoctorStatus::Fail);
    assert!(sandbox.reason.as_ref().unwrap().contains("probe"));
    assert!(sandbox.fix.is_some());
}
