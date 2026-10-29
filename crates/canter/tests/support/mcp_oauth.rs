use super::*;
use axum::{
    extract::Form,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path, process::Stdio};
use tokio::io::{AsyncBufReadExt, BufReader};

const ACCESS: &str = "fixture-oauth-access";
const REFRESH: &str = "fixture-oauth-refresh";
const RENEWED: &str = "fixture-oauth-renewed";

#[derive(Default)]
struct AuthState {
    base: String,
    registration: Option<Value>,
    challenge: String,
    exchanges: usize,
    refreshes: usize,
    reject_refresh: bool,
    no_registration: bool,
    echo_metadata: bool,
    seen: Vec<String>,
}
struct AuthServer {
    state: Arc<Mutex<AuthState>>,
    task: JoinHandle<()>,
}
impl Drop for AuthServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl AuthServer {
    async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let state = Arc::new(Mutex::new(AuthState {
            base: format!("http://{}", listener.local_addr().unwrap()),
            ..Default::default()
        }));
        let app = Router::new()
            .route("/.well-known/oauth-protected-resource/mcp", get(resource))
            .route("/.well-known/oauth-authorization-server", get(metadata))
            .route("/register", post(register))
            .route("/token", post(token))
            .route("/mcp", post(rpc).get(challenge))
            .route("/public", post(rpc))
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { state, task }
    }
    fn config(&self) -> Value {
        json!({"transport":"http", "url":format!("{}/mcp", self.state.lock().unwrap().base)})
    }
}
async fn resource(State(state): State<Arc<Mutex<AuthState>>>) -> Json<Value> {
    let state = state.lock().unwrap();
    Json(
        json!({"resource":format!("{}/mcp",state.base),"authorization_servers":[state.base],"scopes_supported":["mcp:read"]}),
    )
}
async fn metadata(State(state): State<Arc<Mutex<AuthState>>>) -> Json<Value> {
    let state = state.lock().unwrap();
    let mut value = json!({"issuer":state.base,"authorization_endpoint":format!("{}/authorize",state.base),"token_endpoint":format!("{}/token",state.base),"response_types_supported":["code"],"code_challenge_methods_supported":["S256"],"scopes_supported":["mcp:read"]});
    if !state.no_registration {
        value["registration_endpoint"] = format!("{}/register", state.base).into();
    }
    Json(value)
}
async fn register(
    State(state): State<Arc<Mutex<AuthState>>>,
    Json(body): Json<Value>,
) -> Json<Value> {
    assert_eq!(body["token_endpoint_auth_method"], "none");
    assert_eq!(body["application_type"], "native");
    let redirect = reqwest::Url::parse(body["redirect_uris"][0].as_str().unwrap()).unwrap();
    assert!(
        tokio::net::TcpListener::bind(("127.0.0.1", redirect.port().unwrap()))
            .await
            .is_err(),
        "callback port must already be reserved before registration"
    );
    state.lock().unwrap().registration = Some(body.clone());
    Json(json!({"client_id":"canter-fixture", "redirect_uris":body["redirect_uris"]}))
}
async fn token(
    State(state): State<Arc<Mutex<AuthState>>>,
    Form(body): Form<HashMap<String, String>>,
) -> Response {
    let mut state = state.lock().unwrap();
    assert_eq!(body["client_id"], "canter-fixture");
    assert_eq!(body["resource"], format!("{}/mcp", state.base));
    if body["grant_type"] == "authorization_code" {
        assert_eq!(body["code"], "fixture-code");
        assert_eq!(
            URL_SAFE_NO_PAD.encode(Sha256::digest(body["code_verifier"].as_bytes())),
            state.challenge
        );
        assert_eq!(
            body["redirect_uri"],
            state.registration.as_ref().unwrap()["redirect_uris"][0]
        );
        state.exchanges += 1;
        Json(json!({"access_token":ACCESS,"refresh_token":REFRESH,"token_type":"Bearer","expires_in":3600})).into_response()
    } else {
        assert_eq!(body["grant_type"], "refresh_token");
        assert_eq!(body["refresh_token"], REFRESH);
        state.refreshes += 1;
        if state.reject_refresh {
            (
                StatusCode::BAD_REQUEST,
                Json(json!({"error":"invalid_grant","error_description":REFRESH})),
            )
                .into_response()
        } else {
            Json(json!({"access_token":RENEWED,"token_type":"Bearer","expires_in":3600}))
                .into_response()
        }
    }
}
async fn challenge() -> Response {
    (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer resource_metadata=\"/.well-known/oauth-protected-resource/mcp\", scope=\"mcp:read\"")]).into_response()
}
async fn rpc(
    State(state): State<Arc<Mutex<AuthState>>>,
    axum::extract::OriginalUri(uri): axum::extract::OriginalUri,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    let authorization = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    state.lock().unwrap().seen.push(authorization.clone());
    if uri.path() == "/mcp"
        && ![format!("Bearer {ACCESS}"), format!("Bearer {RENEWED}")].contains(&authorization)
    {
        return challenge().await;
    }
    if request.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = match request["method"].as_str().unwrap() {
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
        }
        "tools/list" => {
            if state.lock().unwrap().echo_metadata {
                json!({"tools":[{"name":"echo","description":ACCESS,"inputSchema":{"type":"object","description":REFRESH}}]})
            } else {
                json!({"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]})
            }
        }
        "tools/call" => {
            json!({"content":[{"type":"text","text":format!("{ACCESS} {REFRESH} {RENEWED}")}]})
        }
        _ => json!({}),
    };
    Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
}

fn cli(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_canter"));
    cmd.env_clear()
        .env("HOME", home)
        .env("CANTER_HOME", home)
        .env("PATH", home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}
async fn login(home: &Path, auth: &AuthServer, denied: bool) -> std::process::Output {
    let mut child = cli(home).args(["mcp", "login", "remote"]).spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(15), lines.next_line())
        .await
        .unwrap()
        .unwrap();
    assert!(
        line.is_some(),
        "login must print an authorization URL; {}",
        String::from_utf8_lossy(&child.wait_with_output().await.unwrap().stderr)
    );
    let url = reqwest::Url::parse(line.as_deref().unwrap()).expect("authorization URL");
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(params["code_challenge_method"], "S256");
    assert_eq!(params["scope"], "mcp:read");
    auth.state.lock().unwrap().challenge = params["code_challenge"].clone();
    let mut callback = reqwest::Url::parse(&params["redirect_uri"]).unwrap();
    assert_eq!(callback.host_str(), Some("127.0.0.1"));
    callback
        .query_pairs_mut()
        .append_pair("state", &params["state"]);
    if denied {
        callback
            .query_pairs_mut()
            .append_pair("error", "access_denied");
    } else {
        callback
            .query_pairs_mut()
            .append_pair("code", "fixture-code");
    }
    Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .get(callback)
        .send()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}
fn token_file(home: &Path) -> std::path::PathBuf {
    home.join("mcp-auth/72656d6f7465.json")
}

#[tokio::test]
async fn oauth_login_discovers_registers_pkce_and_persists_without_daemon() {
    let auth = AuthServer::start().await;
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("config.json"),
        json!({"mcp_servers":{"remote":auth.config()}}).to_string(),
    )
    .unwrap();
    let output = login(home.path(), &auth, false).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let credentials = fs::read_to_string(token_file(home.path())).unwrap();
    assert!(credentials.contains(ACCESS));
    assert!(credentials.contains(REFRESH));
    assert_eq!(
        fs::metadata(token_file(home.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(
        !fs::read_to_string(home.path().join("config.json"))
            .unwrap()
            .contains(ACCESS)
    );
    assert!(!home.path().join("server.json").exists());
    assert_eq!(auth.state.lock().unwrap().exchanges, 1);
}

async fn stop_daemon(fixture: &mut Fixture) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(fixture.child.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(15), fixture.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}
fn expire(home: &Path) {
    let path = token_file(home);
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved["credentials"]["token_received_at"] = 0.into();
    fs::write(path, saved.to_string()).unwrap();
}
async fn status(fixture: &Fixture) -> Value {
    fixture
        .request(Method::GET, "/v1/mcp", Value::Null)
        .await
        .json()
        .await
        .unwrap()
}
fn assert_private(text: &str) {
    for secret in [ACCESS, REFRESH, RENEWED] {
        assert!(!text.contains(secret), "credential leaked");
    }
}
fn assert_private_files(path: &Path) {
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_private_files(&path);
        } else {
            assert_private(&String::from_utf8_lossy(&fs::read(path).unwrap()));
        }
    }
}
#[tokio::test]
async fn oauth_runtime_refreshes_redacts_and_logout_stops_authentication() {
    let auth = AuthServer::start().await;
    let call = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"oauth-call","type":"function","function":{"name":"mcp__remote__echo","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})
    );
    let mut fixture = Fixture::start_with_mcp(
        vec![call.clone(), text_reply(), call, text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"remote":auth.config()}),
    )
    .await;
    let initial = status(&fixture).await;
    assert_eq!(
        initial["data"][0]["error"],
        "需要登录：运行 `canter mcp login remote`"
    );
    stop_daemon(&mut fixture).await;
    let home = fixture.root.path().join("state");
    assert!(login(&home, &auth, false).await.status.success());
    auth.state.lock().unwrap().echo_metadata = true;
    expire(&home);
    fixture.child = Fixture::spawn(fixture.root.path());
    fixture.ready().await;
    assert_eq!(status(&fixture).await["data"][0]["connected"], true);
    assert_eq!(auth.state.lock().unwrap().refreshes, 1);
    let created: SessionCreateResponse = fixture.request(Method::POST,"/v1/sessions",json!({"cwd":fixture.root.path().join("work"),"model":"fake-model","env":{},"approvalPolicy":"allowAll","clientName":"test"})).await.json().await.unwrap();
    fixture.input(&created.session.id).await;
    let events = fixture
        .events_until(&created.session.id, created.event_seq, |e| {
            matches!(e.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    assert!(
        serde_json::to_string(&events)
            .unwrap()
            .contains("[redacted] [redacted] [redacted]")
    );
    assert_private(&serde_json::to_string(&events).unwrap());
    assert_private(&status(&fixture).await.to_string());
    assert_private(&serde_json::to_string(&fixture.script.lock().unwrap().requests).unwrap());
    assert!(
        cli(&home)
            .args(["mcp", "logout", "remote"])
            .output()
            .await
            .unwrap()
            .status
            .success()
    );
    assert!(!token_file(&home).exists());
    auth.state.lock().unwrap().seen.clear();
    fixture.input(&created.session.id).await;
    fixture
        .events_until(&created.session.id, events.last().unwrap().seq, |e| {
            matches!(e.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    assert_eq!(
        status(&fixture).await["data"][0]["error"],
        "需要登录：运行 `canter mcp login remote`"
    );
    assert!(
        auth.state
            .lock()
            .unwrap()
            .seen
            .iter()
            .all(|header| header.is_empty())
    );
    fixture.stop().await;
    assert_private_files(&home.join("sessions"));
    assert_private(&fs::read_to_string(fixture.root.path().join("server.log")).unwrap());
}

#[tokio::test]
async fn oauth_denial_and_missing_dynamic_registration_leave_no_tokens() {
    let auth = AuthServer::start().await;
    let home = tempfile::tempdir().unwrap();
    fs::write(
        home.path().join("config.json"),
        json!({"mcp_servers":{"remote":auth.config()}}).to_string(),
    )
    .unwrap();
    // An already occupied loopback port cannot block a port-zero callback.
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let output = login(home.path(), &auth, true).await;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("denied"));
    assert!(!token_file(home.path()).exists());
    let redirect = auth.state.lock().unwrap().registration.as_ref().unwrap()["redirect_uris"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let redirect = reqwest::Url::parse(&redirect).unwrap();
    assert_ne!(
        redirect.port().unwrap(),
        occupied.local_addr().unwrap().port()
    );
    assert!(
        tokio::net::TcpListener::bind(("127.0.0.1", redirect.port().unwrap()))
            .await
            .is_ok(),
        "callback listener released after denial"
    );
    auth.state.lock().unwrap().no_registration = true;
    let output = cli(home.path())
        .args(["mcp", "login", "remote"])
        .output()
        .await
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("does not support dynamic client registration")
    );
    assert!(!token_file(home.path()).exists());
    assert_eq!(auth.state.lock().unwrap().exchanges, 0);
}

#[tokio::test]
async fn oauth_failed_refresh_only_disconnects_that_service() {
    let auth = AuthServer::start().await;
    let public = format!("{}/public", auth.state.lock().unwrap().base);
    let mut fixture = Fixture::start_with_mcp(
        vec![text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"remote":auth.config(),"healthy":{"transport":"http","url":public}}),
    )
    .await;
    stop_daemon(&mut fixture).await;
    let home = fixture.root.path().join("state");
    assert!(login(&home, &auth, false).await.status.success());
    expire(&home);
    auth.state.lock().unwrap().reject_refresh = true;
    fixture.child = Fixture::spawn(fixture.root.path());
    fixture.ready().await;
    let report = status(&fixture).await;
    assert_eq!(report["data"][0]["connected"], true);
    assert_eq!(report["data"][1]["connected"], false);
    assert_eq!(
        report["data"][1]["error"],
        "需要登录：运行 `canter mcp login remote`"
    );
    assert_private(&report.to_string());
    assert_eq!(auth.state.lock().unwrap().refreshes, 1);
    fixture.stop().await;
    assert_private(&fs::read_to_string(fixture.root.path().join("server.log")).unwrap());
}

#[tokio::test]
async fn oauth_refreshes_during_a_running_connection_and_reports_later_rejection() {
    let auth = AuthServer::start().await;
    let call = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"oauth-call","type":"function","function":{"name":"mcp__remote__echo","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})
    );
    let mut fixture = Fixture::start_with_mcp(
        vec![call.clone(), text_reply(), call, text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"remote":auth.config()}),
    )
    .await;
    stop_daemon(&mut fixture).await;
    let home = fixture.root.path().join("state");
    assert!(login(&home, &auth, false).await.status.success());
    fixture.child = Fixture::spawn(fixture.root.path());
    fixture.ready().await;
    assert_eq!(auth.state.lock().unwrap().refreshes, 0);
    expire(&home);
    let created: SessionCreateResponse = fixture.request(Method::POST,"/v1/sessions",json!({"cwd":fixture.root.path().join("work"),"model":"fake-model","env":{},"approvalPolicy":"allowAll","clientName":"test"})).await.json().await.unwrap();
    fixture.input(&created.session.id).await;
    let events = fixture
        .events_until(&created.session.id, created.event_seq, |e| {
            matches!(e.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    assert_eq!(auth.state.lock().unwrap().refreshes, 1);
    assert!(
        auth.state
            .lock()
            .unwrap()
            .seen
            .contains(&format!("Bearer {RENEWED}"))
    );
    let saved: Value = serde_json::from_slice(&fs::read(token_file(&home)).unwrap()).unwrap();
    assert_eq!(
        saved["credentials"]["token_response"]["access_token"],
        RENEWED
    );
    assert_eq!(
        saved["credentials"]["token_response"]["refresh_token"],
        REFRESH
    );
    expire(&home);
    auth.state.lock().unwrap().reject_refresh = true;
    fixture.input(&created.session.id).await;
    let events = fixture
        .events_until(&created.session.id, events.last().unwrap().seq, |e| {
            matches!(e.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    assert_eq!(
        status(&fixture).await["data"][0]["error"],
        "需要登录：运行 `canter mcp login remote`"
    );
    assert_private(&serde_json::to_string(&events).unwrap());
    fixture.stop().await;
    assert_private_files(&home.join("sessions"));
}
