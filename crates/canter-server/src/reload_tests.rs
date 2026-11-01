use super::*;
use canter_protocol::{
    ContextResponse, Event, EventPayload, SessionCreateResponse, SessionReadResponse, SessionStatus,
};
use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{fs, process::Stdio};
use tokio::sync::{mpsc, oneshot};
mod hooks;

#[tokio::test]
async fn server_process() {
    if std::env::var_os("CANTER_RELOAD_TEST").is_some() {
        serve(false, "reload-test").await.unwrap();
    }
}

struct Request {
    body: Value,
    reply: oneshot::Sender<String>,
}
impl Request {
    fn stop(self) {
        let chunk =
            json!({"choices":[{"index":0,"delta":{"content":"done"},"finish_reason":"stop"}]});
        self.reply
            .send(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
            .unwrap();
    }
    fn tool(self, name: &str, arguments: Value) {
        let chunk = json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call-test","type":"function","function":{"name":name,"arguments":arguments.to_string()}}]},"finish_reason":"tool_calls"}]});
        self.reply
            .send(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
            .unwrap();
    }
    fn tools(&self) -> Vec<&str> {
        self.body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str())
            .collect()
    }
}
struct Fixture {
    root: tempfile::TempDir,
    config: Value,
    child: tokio::process::Child,
    client: reqwest::Client,
    requests: mpsc::UnboundedReceiver<Request>,
    model: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn new(extra: Value) -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-reload-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = root.path().join("home");
        let state = root.path().join("state");
        fs::create_dir(&home).unwrap();
        fs::create_dir(&state).unwrap();
        let endpoint = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = endpoint.local_addr().unwrap();
        let (requests, receiver) = mpsc::unbounded_channel();
        let router = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let requests = requests.clone();
                async move {
                    let (reply, response) = oneshot::channel();
                    let _ = requests.send(Request { body, reply });
                    (
                        [("content-type", "text/event-stream")],
                        response.await.unwrap_or_default(),
                    )
                }
            }),
        );
        let model = tokio::spawn(async move {
            axum::serve(endpoint, router).await.unwrap();
        });
        let mut config = json!({"base_url":format!("http://{address}"),"model":"fake","models":["fake"],"api_key":"test-only-key","context_window":1048576,"sandbox":false});
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        fs::write(state.join("config.json"), config.to_string()).unwrap();
        let log = fs::File::create(root.path().join("server.log")).unwrap();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "reload_tests::server_process", "--nocapture"])
            .env_clear()
            .env("HOME", &home)
            .env("CANTER_HOME", &state)
            .env("CANTER_RELOAD_TEST", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let connection = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "{}",
                    fs::read_to_string(root.path().join("server.log")).unwrap()
                );
                if let Ok(Some(connection)) = discover(&state).await {
                    break connection;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .unix_socket(connection.registration.socket_path)
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();
        Self {
            root,
            config,
            child,
            client,
            requests: receiver,
            model,
        }
    }
    fn path(&self) -> PathBuf {
        self.root.path().join("state/config.json")
    }
    fn save(&self) {
        fs::write(self.path(), self.config.to_string()).unwrap();
    }
    async fn create(&self) -> String {
        self.create_with_env(json!({})).await
    }
    async fn trust(&self, directory: &std::path::Path) {
        self.client
            .post("http://localhost/v1/folderTrust")
            .json(&json!({"cwd":directory}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    async fn create_with_env(&self, extra: Value) -> String {
        let mut environment = json!({"HOME":self.root.path().join("home"),"CANTER_HOME":self.root.path().join("state")});
        environment
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let created: SessionCreateResponse = self.client.post("http://localhost/v1/sessions")
            .json(&json!({"cwd":self.root.path(),"env":environment,"model":"","approvalPolicy":"allowAll","clientName":"test"}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        format!("http://localhost/v1/sessions/{}", created.session.id)
    }
    async fn input(&self, url: &str) {
        self.client
            .post(format!("{url}/input"))
            .json(&json!({"input":[{"type":"text","text":"hello"}],"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    async fn request(&mut self) -> Request {
        tokio::time::timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn context(&self, url: &str) -> ContextResponse {
        self.client
            .get(format!("{url}/context"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    async fn snapshot(&self, url: &str) -> SessionReadResponse {
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
    async fn idle(&self, url: &str) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while self.snapshot(url).await.session.status != SessionStatus::Idle {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            self.snapshot(url)
                .await
                .session
                .turns
                .last()
                .unwrap()
                .status,
            canter_protocol::TurnStatus::Completed
        );
    }
    async fn applied(&mut self, marker: &str) {
        self.config["models"] = json!(["fake", marker]);
        self.save();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let models: canter_protocol::ModelListResponse = self
                    .client
                    .get("http://localhost/v1/models")
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if models.data.iter().any(|model| model.id == marker) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    }
    async fn warnings(&self, url: &str) -> Warnings {
        let after = self.snapshot(url).await.event_seq;
        self.warnings_after(url, after).await
    }
    async fn warnings_after(&self, url: &str, after: u64) -> Warnings {
        let response = self
            .client
            .get(format!(
                "http://localhost/v1/events?session={}&after={after}",
                url.rsplit('/').next().unwrap()
            ))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let (sender, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut stream = response.bytes_stream().eventsource();
            while let Some(Ok(event)) = stream.next().await {
                let event: Event = serde_json::from_str(&event.data).unwrap();
                if let EventPayload::Warning { message } = event.payload {
                    let _ = sender.send(message);
                }
            }
        });
        Warnings { receiver, task }
    }
    async fn shutdown(mut self) {
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.child.id().unwrap() as i32),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(15), self.child.wait())
            .await
            .unwrap()
            .unwrap();
        self.model.abort();
        assert!(
            status.success(),
            "{}",
            fs::read_to_string(self.root.path().join("server.log")).unwrap()
        );
    }
}
struct Warnings {
    receiver: mpsc::UnboundedReceiver<String>,
    task: tokio::task::JoinHandle<()>,
}

#[tokio::test]
async fn hooks_format_written_files_and_return_feedback_to_the_model() {
    let mut fixture = Fixture::new(json!({"hooks":{"PostToolUse":[{
        "matcher":"write|edit", "hooks":[{"type":"command", "command":
        "cat > hook-input.json; printf 'formatted\\n' > sample.txt; printf 'formatter feedback' >&2; exit 2"}]
    }]}})).await;
    fixture.trust(fixture.root.path()).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    fixture.request().await.tool(
        "write",
        json!({"path":"sample.txt","content":"unformatted"}),
    );
    let next = fixture.request().await;
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("sample.txt")).unwrap(),
        "formatted\n"
    );
    let input: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("hook-input.json")).unwrap())
            .unwrap();
    assert_eq!(input["hook_event_name"], "PostToolUse");
    assert_eq!(input["tool_name"], "write");
    assert_eq!(
        input["tool_input"],
        json!({"path":"sample.txt","content":"unformatted"})
    );
    assert_eq!(input["session_id"], url.rsplit('/').next().unwrap());
    assert_eq!(input["cwd"], fixture.root.path().to_str().unwrap());
    assert!(input.get("permission_mode").is_some());
    assert!(input.get("tool_response").is_some());
    let messages = next.body["messages"].as_array().unwrap();
    assert!(messages.iter().any(|m| {
        m["role"] == "tool"
            && m["content"]
                .as_str()
                .unwrap()
                .contains("formatter feedback")
    }));
    assert!(messages.iter().any(|m| {
        m["role"] == "tool"
            && m["content"].as_str().unwrap().ends_with(
                "sample.txt was changed by a PostToolUse hook after this call; read it again before editing it.",
            )
    }));
    next.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
}
impl Warnings {
    async fn next(&mut self) -> String {
        tokio::time::timeout(Duration::from_secs(5), self.receiver.recv())
            .await
            .unwrap()
            .unwrap()
    }
}
impl Drop for Warnings {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn existing_sessions_keep_the_running_turn_config_and_reload_before_the_next_turn() {
    let mut fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    let first = fixture.request().await;
    fixture.config["context_window"] = json!(524288);
    fixture.applied("next").await;
    assert_eq!(fixture.context(&url).await.model_context_window, 1048576);
    first.stop();
    fixture.idle(&url).await;
    fixture.input(&url).await;
    let next = fixture.request().await;
    let context = fixture.context(&url).await;
    next.stop();
    fixture.shutdown().await;
    assert_eq!(context.model_context_window, 524288);
}

#[tokio::test]
async fn idle_btw_and_next_turn_do_not_deadlock_during_lsp_reload() {
    let mut fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    let client = fixture.client.clone();
    let btw_url = url.clone();
    let btw = tokio::spawn(async move {
        client
            .post(format!("{btw_url}/btw"))
            .json(&json!({"question":"private question"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .text()
            .await
            .unwrap()
    });
    let private = fixture.request().await;
    fixture.config["lsp"] = json!({"enabled":true});
    fixture.applied("btw-reload").await;
    let client = fixture.client.clone();
    let input_url = url.clone();
    let input = tokio::spawn(async move {
        client
            .post(format!("{input_url}/input"))
            .json(&json!({"input":[{"type":"text","text":"next"}],"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    private.stop();
    tokio::time::timeout(Duration::from_secs(3), input)
        .await
        .expect("the next turn must not deadlock with the private answer")
        .unwrap();
    fixture.request().await.stop();
    fixture.idle(&url).await;
    let answer = tokio::time::timeout(Duration::from_secs(3), btw)
        .await
        .unwrap()
        .unwrap();
    assert!(answer.contains("completed"), "{answer}");
    fixture.shutdown().await;
}

#[tokio::test]
async fn waiting_turn_keeps_its_snapshot_interrupt_and_close_responsive() {
    for close in [false, true] {
        let mut fixture = Fixture::new(json!({})).await;
        let a = fixture.create().await;
        let b = fixture.create().await;
        fixture.input(&a).await;
        let running = fixture.request().await;
        fixture.config["lsp"] = json!({"enabled":true});
        fixture.applied("waiting-for-old-turn").await;
        let client = fixture.client.clone();
        let input_url = b.clone();
        let pending = tokio::spawn(async move {
            client
                .post(format!("{input_url}/input"))
                .json(&json!({"input":[{"type":"text","text":"next"}],"clientName":"test"}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let snapshot = tokio::time::timeout(Duration::from_secs(2), fixture.snapshot(&b))
            .await
            .expect("the waiting session's snapshot must remain available");
        assert_eq!(snapshot.session.status, SessionStatus::Working);
        assert!(fixture.requests.try_recv().is_err());
        let command = if close {
            fixture.client.delete(&b).json(&json!({}))
        } else {
            fixture
                .client
                .post(format!("{b}/interrupt"))
                .json(&json!({"turnId":snapshot.session.turns.last().unwrap().id}))
        };
        tokio::time::timeout(Duration::from_secs(2), command.send())
            .await
            .expect("Esc and close must cancel service waiting")
            .unwrap()
            .error_for_status()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), pending)
            .await
            .unwrap()
            .unwrap();
        let stopped = fixture.snapshot(&b).await;
        assert_eq!(
            stopped.session.turns.last().unwrap().status,
            canter_protocol::TurnStatus::Interrupted
        );
        assert_eq!(
            stopped.session.status,
            if close {
                SessionStatus::Closed
            } else {
                SessionStatus::Idle
            }
        );
        assert_eq!(
            fixture.snapshot(&a).await.session.status,
            SessionStatus::Working
        );
        running.stop();
        fixture.idle(&a).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(200), fixture.requests.recv())
                .await
                .is_err(),
            "a cancelled waiting turn must not start after the service becomes ready"
        );
        fixture.shutdown().await;
    }
}

#[tokio::test]
async fn opening_sessions_and_listing_mcp_do_not_wait_for_service_handover() {
    let mut fixture = Fixture::new(json!({"mcp_tool_search":false,"mcp_servers":{
        "local":{"transport":"stdio","command":mcp_executable(),"args":["--fixture"]}
    }}))
    .await;
    let a = fixture.create().await;
    let reopen = fixture.create().await;
    fixture
        .client
        .delete(&reopen)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.input(&a).await;
    let running = fixture.request().await;
    fixture.config["mcp_servers"]["local"]["env"] = json!({"RELOAD_MARKER":"changed"});
    fixture.config["lsp"] = json!({"enabled":true});
    fixture.applied("opening-during-handover").await;
    let created = tokio::time::timeout(Duration::from_secs(2), fixture.create())
        .await
        .expect("creating a session must not wait for another session's old turn");
    let resumed = tokio::time::timeout(Duration::from_secs(2), fixture.client.post(format!("{reopen}/resume"))
        .json(&json!({"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")}})).send()).await
        .expect("opening a saved session must not wait for handover").unwrap().error_for_status().unwrap();
    assert_eq!(
        resumed
            .json::<SessionReadResponse>()
            .await
            .unwrap()
            .session
            .status,
        SessionStatus::Idle
    );
    let mcp = tokio::time::timeout(
        Duration::from_secs(2),
        fixture.client.get("http://localhost/v1/mcp").send(),
    )
    .await
    .expect("MCP status must report pending services without waiting")
    .unwrap()
    .error_for_status()
    .unwrap();
    let mcp: canter_protocol::McpListResponse = mcp.json().await.unwrap();
    assert_eq!(mcp.data.len(), 1);
    assert_eq!(mcp.data[0].name, "local");
    assert!(!mcp.data[0].connected);
    assert!(mcp.data[0].error.as_ref().unwrap().contains("Starting"));
    assert_eq!(
        fixture.snapshot(&a).await.session.status,
        SessionStatus::Working
    );
    fixture.input(&created).await;
    assert!(fixture.requests.try_recv().is_err());
    running.stop();
    let first = fixture.request().await;
    assert!(first.tools().contains(&"mcp__local__echo"));
    first.stop();
    fixture.idle(&created).await;
    fixture.input(&reopen).await;
    let first = fixture.request().await;
    assert!(first.tools().contains(&"mcp__local__echo"));
    first.stop();
    fixture.idle(&reopen).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn invalid_json_keeps_the_old_config_reports_location_and_recovers_over_sse() {
    let mut fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    let mut warnings = fixture.warnings(&url).await;
    fixture.input(&url).await;
    let running = fixture.request().await;
    fs::write(
        fixture.path(),
        "{\n\"api_key\":\"test-only-secret-do-not-report\",\n\"context_window\":",
    )
    .unwrap();
    let error = warnings.next().await;
    assert!(
        error.contains("config.json") && error.contains("line 3") && error.contains("column"),
        "{error}"
    );
    assert!(!error.contains("test-only-secret-do-not-report"));
    assert_eq!(fixture.context(&url).await.model_context_window, 1048576);
    assert_eq!(
        fixture.snapshot(&url).await.session.status,
        SessionStatus::Working
    );
    running.stop();
    fixture.idle(&url).await;
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    fixture.config["context_window"] = json!(524288);
    fixture.applied("fixed").await;
    assert!(warnings.next().await.contains("recovered"));
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(fixture.context(&url).await.model_context_window, 524288);
    fixture.shutdown().await;
}

#[tokio::test]
async fn missing_or_renamed_config_keeps_the_previous_settings_and_recovers() {
    let mut fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    let mut warnings = fixture.warnings(&url).await;
    let backup = fixture.path().with_extension("saved");
    fs::rename(fixture.path(), &backup).unwrap();
    assert!(warnings.next().await.contains("cannot read config.json"));
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(fixture.context(&url).await.model_context_window, 1048576);
    fs::rename(&backup, fixture.path()).unwrap();
    assert!(warnings.next().await.contains("recovered"));
    fs::remove_file(fixture.path()).unwrap();
    assert!(warnings.next().await.contains("cannot read config.json"));
    fixture.config["context_window"] = json!(524288);
    fixture.save();
    assert!(warnings.next().await.contains("recovered"));
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(fixture.context(&url).await.model_context_window, 524288);
    fixture.shutdown().await;
}

#[tokio::test]
async fn rapid_saves_publish_only_the_last_value() {
    let mut fixture = Fixture::new(json!({})).await;
    let url = fixture.create().await;
    let mut warnings = fixture.warnings(&url).await;
    // Watch the public model list throughout the burst, including between saves.
    let client = fixture.client.clone();
    let finished = CancellationToken::new();
    let stop = finished.clone();
    let observer = tokio::spawn(async move {
        let mut seen = Vec::new();
        while !stop.is_cancelled() {
            let models: canter_protocol::ModelListResponse = client
                .get("http://localhost/v1/models")
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            seen.extend(models.data.into_iter().map(|model| model.id));
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        seen
    });
    for n in 1..=3 {
        fixture.config["context_window"] = json!(200000 + n);
        fixture.config["models"] = json!(["fake", format!("intermediate-{n}")]);
        fixture.save();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    fs::write(fixture.path(), "{").unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    fixture.config["context_window"] = json!(524288);
    fixture.applied("final").await;
    finished.cancel();
    let seen = observer.await.unwrap();
    assert!(
        seen.iter().all(|model| !model.starts_with("intermediate-")),
        "{seen:?}"
    );
    assert!(warnings.receiver.try_recv().is_err());
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(fixture.context(&url).await.model_context_window, 524288);
    fixture.shutdown().await;
}

#[tokio::test]
async fn simultaneous_sessions_change_configuration_at_their_own_turn_boundaries() {
    let mut fixture = Fixture::new(json!({})).await;
    let a = fixture.create().await;
    let b = fixture.create().await;
    fixture.input(&a).await;
    let a_old = fixture.request().await;
    fixture.input(&b).await;
    let b_old = fixture.request().await;
    fixture.config["context_window"] = json!(524288);
    fixture.config["plan_enabled"] = json!(false);
    fixture.applied("parallel").await;
    assert_eq!(fixture.context(&a).await.model_context_window, 1048576);
    assert_eq!(fixture.context(&b).await.model_context_window, 1048576);
    assert_eq!(
        fixture.snapshot(&a).await.session.settings.plan_enabled,
        Some(true)
    );
    a_old.stop();
    fixture.idle(&a).await;
    fixture.input(&a).await;
    let a_new = fixture.request().await;
    assert_eq!(fixture.context(&a).await.model_context_window, 524288);
    assert_eq!(fixture.context(&b).await.model_context_window, 1048576);
    assert_eq!(
        fixture.snapshot(&a).await.session.settings.plan_enabled,
        Some(false)
    );
    assert!(!a_new.tools().contains(&"update_plan"));
    // The to-do list switch does not disable Plan-mode proposal approval.
    assert!(a_new.tools().contains(&"submit_plan"));
    b_old.stop();
    fixture.idle(&b).await;
    fixture.input(&b).await;
    fixture.request().await.stop();
    a_new.stop();
    fixture.idle(&a).await;
    fixture.idle(&b).await;
    assert_eq!(fixture.context(&b).await.model_context_window, 524288);
    fixture.shutdown().await;
}

fn mcp_executable() -> PathBuf {
    static EXECUTABLE: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    EXECUTABLE
        .get_or_init(|| {
            let dir = tempfile::tempdir().unwrap();
            let result = std::process::Command::new("rustc")
                .args([
                    "--edition=2024",
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/../canter/tests/fixtures/mcp_stdio.rs"
                    ),
                    "-o",
                ])
                .arg(dir.path().join("mcp-fixture"))
                .status()
                .unwrap();
            assert!(result.success());
            dir
        })
        .path()
        .join("mcp-fixture")
}
async fn until(mut condition: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}
fn gone(pid: u32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None)
        == Err(nix::errno::Errno::ESRCH)
}

#[tokio::test]
async fn mcp_removal_allows_stdio_to_read_eof_before_exiting() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("mcp.pid");
    let eof = directory.path().join("stdin.eof");
    let mut fixture = Fixture::new(json!({"mcp_servers":{
        "removed":{"transport":"stdio","command":mcp_executable(),"args":["--fixture"],
            "env":{"PID_FILE":pid_file,"STDIN_EOF_FILE":eof}}
    }}))
    .await;
    let pid: u32 = fs::read_to_string(pid_file).unwrap().parse().unwrap();
    fixture.config["mcp_servers"] = json!({});
    fixture.applied("mcp-removed").await;
    until(|| gone(pid)).await;
    assert_eq!(
        fs::read_to_string(eof).ok().as_deref(),
        Some("eof"),
        "retiring MCP must close stdin and allow the process to exit"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_retirement_waits_for_refresh_then_closes_stdio() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("mcp.pid");
    let started = directory.path().join("refresh.started");
    let release = directory.path().join("refresh.release");
    let eof = directory.path().join("stdin.eof");
    let mut fixture = Fixture::new(json!({"mcp_servers":{
        "removed":{"transport":"stdio","command":mcp_executable(),"args":["--fixture"],
            "startup_timeout_sec":600,"env":{"PID_FILE":pid_file,"REFRESH_STARTED_FILE":started,
                "REFRESH_RELEASE_FILE":release,"STDIN_EOF_FILE":eof}}
    }}))
    .await;
    until(|| started.exists()).await;
    let pid: u32 = fs::read_to_string(pid_file).unwrap().parse().unwrap();
    fixture.config["mcp_servers"] = json!({});
    fixture.applied("mcp-removed").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(
        !gone(pid),
        "retirement must allow the in-flight refresh to finish"
    );
    fs::write(release, "release").unwrap();
    until(|| gone(pid)).await;
    assert_eq!(fs::read_to_string(eof).unwrap(), "eof");
    fixture.shutdown().await;
}

#[tokio::test]
async fn replacement_drain_keeps_mcp_available_to_the_active_turn() {
    let mut fixture = Fixture::new(json!({"mcp_tool_search":false,"mcp_servers":{
        "kept":{"transport":"stdio","command":mcp_executable(),"args":["--fixture"]}
    }}))
    .await;
    let url = fixture.create().await;
    fixture.input(&url).await;
    let running = fixture.request().await;
    let state = fixture.root.path().join("state");
    fs::copy(state.join("server.json"), state.join("replace.json")).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = fixture
                .client
                .patch(&url)
                .json(&json!({"name":"drain probe"}))
                .send()
                .await
                .unwrap();
            if response.status() == reqwest::StatusCode::CONFLICT {
                break;
            }
            response.error_for_status().unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    running.tool(
        "mcp__kept__echo",
        json!({"text":"finish the accepted turn"}),
    );
    let next = fixture.request().await;
    assert!(next.body["messages"].to_string().contains("stdio result"));
    next.stop();
    let status = tokio::time::timeout(Duration::from_secs(10), fixture.child.wait())
        .await
        .unwrap()
        .unwrap();
    fixture.model.abort();
    assert!(status.success());
}

#[tokio::test]
async fn shutdown_cancels_a_stalled_mcp_tool_list_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("mcp.pid");
    let started = directory.path().join("refresh.started");
    let fixture = Fixture::new(json!({"mcp_servers":{
        "refresh":{"transport":"stdio","command":mcp_executable(),"args":["--fixture"],
            "startup_timeout_sec":600,"env":{"PID_FILE":pid_file,"REFRESH_STARTED_FILE":started}}
    }}))
    .await;
    until(|| started.exists()).await;
    let pid: u32 = fs::read_to_string(pid_file).unwrap().parse().unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(5), fixture.shutdown()).await;
    if stopped.is_err() {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    assert!(
        stopped.is_ok(),
        "shutdown waited for the stalled MCP refresh"
    );
    until(|| gone(pid)).await;
}

#[tokio::test]
async fn shutdown_releases_a_retirement_waiting_for_stalled_mcp_refresh() {
    let directory = tempfile::tempdir().unwrap();
    let started = directory.path().join("refresh.started");
    let mut fixture = Fixture::new(json!({"mcp_servers":{
        "removed":{"transport":"stdio","command":mcp_executable(),"args":["--fixture"],
            "startup_timeout_sec":600,"env":{"REFRESH_STARTED_FILE":started}}
    }}))
    .await;
    until(|| started.exists()).await;
    fixture.config["mcp_servers"] = json!({});
    fixture.applied("mcp-removed").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn mcp_removal_keeps_an_in_flight_call_alive_then_the_next_turn_sees_the_added_service() {
    let directory = tempfile::tempdir().unwrap();
    let old_pid = directory.path().join("old.pid");
    let new_pid = directory.path().join("new.pid");
    let started = directory.path().join("started");
    let release = directory.path().join("release");
    let executable = mcp_executable();
    let mut fixture = Fixture::new(json!({"mcp_tool_search":false,"mcp_servers":{
        "old":{"transport":"stdio","command":executable,"args":["--fixture"],"env":{"PID_FILE":old_pid,"CALL_STARTED_FILE":started,"CALL_RELEASE_FILE":release}}
    }})).await;
    let url = fixture.create().await;
    let pid: u32 = fs::read_to_string(&old_pid).unwrap().parse().unwrap();
    fixture.input(&url).await;
    let request = fixture.request().await;
    assert!(request.tools().contains(&"mcp__old__echo"));
    request.tool("mcp__old__echo", json!({"text":"keep running"}));
    until(|| started.exists()).await;
    fixture.config["mcp_servers"] = json!({"new":{"transport":"stdio","command":executable,"args":["--fixture"],"env":{"PID_FILE":new_pid}}});
    fixture.applied("mcp-new").await;
    assert!(!gone(pid), "old MCP must remain alive throughout its turn");
    fs::write(&release, "continue").unwrap();
    let same_turn = fixture.request().await;
    assert!(same_turn.tools().contains(&"mcp__old__echo"));
    assert!(
        same_turn.body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message["role"] == "tool"
                && message["content"]
                    .as_str()
                    .is_some_and(|text| text.contains("stdio result")))
    );
    assert!(!gone(pid));
    same_turn.stop();
    fixture.idle(&url).await;
    until(|| gone(pid)).await;
    fixture.input(&url).await;
    let next = fixture.request().await;
    assert!(!next.tools().contains(&"mcp__old__echo"));
    assert!(next.tools().contains(&"mcp__new__echo"));
    let new: u32 = fs::read_to_string(&new_pid).unwrap().parse().unwrap();
    next.stop();
    fixture.idle(&url).await;
    fixture.shutdown().await;
    until(|| gone(new)).await;
}

#[tokio::test]
async fn adding_mcp_does_not_wait_for_other_sessions_running_with_the_previous_catalog() {
    let mut fixture = Fixture::new(json!({"mcp_tool_search":false})).await;
    let a = fixture.create().await;
    let b = fixture.create().await;
    fixture.input(&a).await;
    let running = fixture.request().await;
    fixture.config["mcp_servers"] =
        json!({"added":{"transport":"stdio","command":mcp_executable(),"args":["--fixture"]}});
    fixture.applied("added").await;
    tokio::time::timeout(Duration::from_secs(5), fixture.input(&b)).await.expect("a new MCP service must be usable by the next turn while another session is still running");
    let next = fixture.request().await;
    assert!(next.tools().contains(&"mcp__added__echo"));
    assert_eq!(
        fixture.snapshot(&a).await.session.status,
        SessionStatus::Working
    );
    next.stop();
    running.stop();
    fixture.idle(&a).await;
    fixture.idle(&b).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn changed_mcp_waits_for_its_old_turn_without_restarting_unchanged_services() {
    let directory = tempfile::tempdir().unwrap();
    let old_pid = directory.path().join("old.pid");
    let new_pid = directory.path().join("new.pid");
    let keep_pid = directory.path().join("keep.pid");
    let config = |path: &std::path::Path| json!({"transport":"stdio","command":mcp_executable(),"args":["--fixture"],"env":{"PID_FILE":path}});
    let mut fixture = Fixture::new(json!({"mcp_tool_search":false,"mcp_servers":{"changed":config(&old_pid),"kept":config(&keep_pid)}})).await;
    let a = fixture.create().await;
    let b = fixture.create().await;
    let old: u32 = fs::read_to_string(&old_pid).unwrap().parse().unwrap();
    let kept: u32 = fs::read_to_string(&keep_pid).unwrap().parse().unwrap();
    fixture.input(&a).await;
    let running = fixture.request().await;
    fixture.config["mcp_servers"]["changed"] = config(&new_pid);
    fixture.applied("changed-service").await;
    let client = fixture.client.clone();
    let pending = tokio::spawn(async move {
        client
            .post(format!("{b}/input"))
            .json(&json!({"input":[{"type":"text","text":"next"}],"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        fixture.requests.try_recv().is_err(),
        "no model work before handover"
    );
    assert!(!new_pid.exists());
    assert!(!gone(old));
    // Waiting for a replacement must not hold the server's session registry lock.
    assert_eq!(
        fixture.snapshot(&a).await.session.status,
        SessionStatus::Working
    );
    running.stop();
    tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    let next = fixture.request().await;
    assert!(next.tools().contains(&"mcp__changed__echo"));
    assert!(next.tools().contains(&"mcp__kept__echo"));
    until(|| gone(old)).await;
    assert_eq!(
        fs::read_to_string(&keep_pid)
            .unwrap()
            .parse::<u32>()
            .unwrap(),
        kept
    );
    assert!(!gone(kept));
    let new: u32 = fs::read_to_string(&new_pid).unwrap().parse().unwrap();
    next.stop();
    fixture.shutdown().await;
    until(|| gone(new) && gone(kept)).await;
}

#[tokio::test]
async fn trusted_cwd_does_not_authorize_lsp_in_untrusted_added_directory() {
    for in_git in [true, false] {
        let config = json!({"enabled":true,"servers":{"fake":{"command":super::lsp_tests::fixture_executable(),"extensions":{"fake":"fake"},"root_markers":["project.marker"]}}});
        let mut fixture = Fixture::new(json!({"lsp":config})).await;
        let extra = tempfile::tempdir().unwrap();
        fs::create_dir(extra.path().join("nested")).unwrap();
        for directory in [fixture.root.path(), extra.path()] {
            fs::write(directory.join("project.marker"), "").unwrap();
            fs::write(directory.join("main.fake"), "").unwrap();
            if directory == fixture.root.path() || in_git {
                assert!(
                    std::process::Command::new("/usr/bin/git")
                        .args(["init", "--quiet"])
                        .arg(directory)
                        .status()
                        .unwrap()
                        .success()
                );
            }
        }
        fixture.trust(fixture.root.path()).await;
        let url = fixture
            .create_with_env(json!({"LSP_TEST_DIR":fixture.root.path(),"LSP_TEST_MODE":"sync"}))
            .await;
        fixture
            .client
            .patch(&url)
            .json(&json!({"addDirectories":[extra.path()]}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        fixture.input(&url).await;
        fixture
            .request()
            .await
            .tool("write", json!({"path":"main.fake","content":"bad A\n"}));
        let request = fixture.request().await;
        let starts = fixture.root.path().join("starts");
        until(|| starts.exists()).await;
        request.tool(
            "write",
            json!({"path":extra.path().join("nested/main.fake"),"content":"bad B\n"}),
        );
        let request = fixture.request().await;
        let extra_started = tokio::time::timeout(Duration::from_secs(1), async {
            until(|| fs::read_to_string(&starts).unwrap().lines().count() > 1).await;
        })
        .await
        .is_ok();
        request.stop();
        fixture.idle(&url).await;
        // Check the positive path too: a previously trusted B needs no add-directory prompt.
        fixture.trust(extra.path()).await;
        fixture.input(&url).await;
        fixture.request().await.tool(
            "write",
            json!({"path":extra.path().join("nested/main.fake"),"content":"trusted B\n"}),
        );
        let request = fixture.request().await;
        until(|| fs::read_to_string(&starts).unwrap().lines().count() == 2).await;
        request.stop();
        fixture.idle(&url).await;
        fixture.shutdown().await;
        assert!(
            !extra_started,
            "trusting A must not authorize B's language server"
        );
    }
}

#[tokio::test]
async fn folder_trust_lost_on_resume_detaches_running_lsp() {
    let config = json!({"enabled":true,"servers":{"fake":{"command":super::lsp_tests::fixture_executable(),"extensions":{"fake":"fake"},"root_markers":["project.marker"]}}});
    let mut fixture = Fixture::new(json!({"lsp":config})).await;
    fixture.trust(fixture.root.path()).await;
    fs::write(fixture.root.path().join("project.marker"), "").unwrap();
    let url = fixture
        .create_with_env(json!({"LSP_TEST_DIR":fixture.root.path(),"LSP_TEST_MODE":"sync"}))
        .await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"main.fake","content":"first\n"}));
    let running = fixture.request().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fixture
                .snapshot(&url)
                .await
                .session
                .language_servers
                .iter()
                .any(|server| server.state == canter_protocol::LanguageServerState::Running)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fs::write(fixture.root.path().join("state/trusted_folders.json"), "[").unwrap();
    let resumed: SessionReadResponse = fixture
        .client
        .post(format!("{url}/resume"))
        .json(&json!({"env":{}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed.session.settings.restricted_mode, Some(true));
    assert!(
        resumed
            .session
            .language_servers
            .iter()
            .all(|server| server.state == canter_protocol::LanguageServerState::Stopped)
    );
    running.tool(
        "write",
        json!({"path":"main.fake","content":"restricted\n"}),
    );
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("starts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn folder_trust_clients_can_request_restricted_but_cannot_grant_trust() {
    let fixture = Fixture::new(json!({})).await;
    let params = json!({"cwd":fixture.root.path(),"env":{},"model":"","approvalPolicy":"allowAll","clientName":"test","restrictedMode":false});
    let created: SessionCreateResponse = fixture
        .client
        .post("http://localhost/v1/sessions")
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created.session.settings.restricted_mode, Some(true));
    fixture.trust(fixture.root.path()).await;
    let url = format!("http://localhost/v1/sessions/{}/resume", created.session.id);
    for restricted in [false, true] {
        let response: SessionReadResponse = fixture
            .client
            .post(&url)
            .json(&json!({"env":{},"restrictedMode":restricted}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response.session.settings.restricted_mode, Some(restricted));
    }
    let mut params = params;
    params["restrictedMode"] = json!(true);
    let created: SessionCreateResponse = fixture
        .client
        .post("http://localhost/v1/sessions")
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created.session.settings.restricted_mode, Some(true));
    fixture.shutdown().await;
}

#[tokio::test]
async fn folder_trust_corrupt_record_opens_restricted_with_warning_and_preserves_record() {
    let fixture = Fixture::new(json!({})).await;
    fixture.trust(fixture.root.path()).await;
    let existing = fixture.create().await;
    let path = fixture.root.path().join("state/trusted_folders.json");
    fs::write(&path, "[\"/tmp/a\", ]").unwrap();
    let query = reqwest::Url::parse_with_params(
        "http://localhost/v1/folderTrust",
        &[("cwd", fixture.root.path().to_str().unwrap())],
    )
    .unwrap();
    let result: Value = fixture
        .client
        .get(query)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["trusted"], false);
    assert!(
        result["warning"]
            .as_str()
            .unwrap()
            .contains("trailing comma")
    );
    let created: SessionCreateResponse = fixture.client.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":fixture.root.path(),"env":{},"model":"","approvalPolicy":"allowAll","clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(created.session.settings.restricted_mode, Some(true));
    let url = format!("http://localhost/v1/sessions/{}", created.session.id);
    let warning = fixture
        .warnings_after(&url, created.event_seq)
        .await
        .next()
        .await;
    assert!(warning.contains(path.to_str().unwrap()), "{warning}");
    assert!(warning.contains("trailing comma"), "{warning}");
    let resumed: SessionReadResponse = fixture
        .client
        .post(format!("{existing}/resume"))
        .json(&json!({"env":{}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed.session.settings.restricted_mode, Some(true));
    let warning = fixture
        .warnings_after(&existing, resumed.event_seq)
        .await
        .next()
        .await;
    assert!(warning.contains(path.to_str().unwrap()), "{warning}");
    let result = fixture
        .client
        .post("http://localhost/v1/folderTrust")
        .json(&json!({"cwd":fixture.root.path()}))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(fs::read_to_string(path).unwrap(), "[\"/tmp/a\", ]");
    fixture.shutdown().await;
}

#[tokio::test]
async fn folder_trust_missing_directory_resumes_restricted() {
    let fixture = Fixture::new(json!({})).await;
    let cwd = fixture.root.path().join("deleted-worktree");
    fs::create_dir(&cwd).unwrap();
    fixture.trust(&cwd).await;
    let created: SessionCreateResponse = fixture
        .client
        .post("http://localhost/v1/sessions")
        .json(
            &json!({"cwd":cwd,"env":{},"model":"","approvalPolicy":"allowAll","clientName":"test"}),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(created.session.settings.restricted_mode, Some(false));
    fs::remove_dir(&cwd).unwrap();
    let query = reqwest::Url::parse_with_params(
        "http://localhost/v1/folderTrust",
        &[("cwd", cwd.to_str().unwrap())],
    )
    .unwrap();
    let result: Value = fixture
        .client
        .get(query)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["trusted"], false);
    let url = format!("http://localhost/v1/sessions/{}", created.session.id);
    let resumed: SessionReadResponse = fixture
        .client
        .post(format!("{url}/resume"))
        .json(&json!({"env":{}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed.session.settings.restricted_mode, Some(true));
    let result = fixture
        .client
        .post("http://localhost/v1/sessions")
        .json(
            &json!({"cwd":cwd,"env":{},"model":"","approvalPolicy":"allowAll","clientName":"test"}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(
        result
            .text()
            .await
            .unwrap()
            .contains("Directory does not exist")
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn folder_trust_outside_git_is_exact_and_restricted_open_leaves_no_record() {
    let fixture = Fixture::new(json!({})).await;
    let folder = fixture.root.path().join("folder");
    let child = folder.join("child");
    fs::create_dir_all(&child).unwrap();
    let url = fixture.create().await;
    assert_eq!(
        fixture
            .snapshot(&url)
            .await
            .session
            .settings
            .restricted_mode,
        Some(true)
    );
    assert!(
        !fixture
            .root
            .path()
            .join("state/trusted_folders.json")
            .exists()
    );
    fixture.trust(&folder).await;
    for (directory, expected) in [
        (&folder, true),
        (&child, false),
        (&fixture.root.path().to_path_buf(), false),
    ] {
        let url = reqwest::Url::parse_with_params(
            "http://localhost/v1/folderTrust",
            &[("cwd", directory.to_str().unwrap())],
        )
        .unwrap();
        let response: Value = fixture
            .client
            .get(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["trusted"], expected);
        assert_eq!(
            response["path"],
            directory.canonicalize().unwrap().to_str().unwrap()
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn folder_trust_worktree_shares_main_only_with_valid_reverse_links() {
    let fixture = Fixture::new(json!({})).await;
    let main = fixture.root.path().join("main");
    let worktree = fixture.root.path().join("worktree");
    fs::create_dir(&main).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "--allow-empty",
            "--no-gpg-sign",
            "-m",
            "initial",
        ],
        vec![
            "worktree",
            "add",
            "--quiet",
            "--detach",
            worktree.to_str().unwrap(),
        ],
    ] {
        assert!(
            std::process::Command::new("/usr/bin/git")
                .current_dir(&main)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    fixture.trust(&main).await;
    let query = reqwest::Url::parse_with_params(
        "http://localhost/v1/folderTrust",
        &[("cwd", worktree.to_str().unwrap())],
    )
    .unwrap();
    let result: Value = fixture
        .client
        .get(query.clone())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["trusted"], true);
    assert_eq!(
        result["path"],
        main.canonicalize().unwrap().to_str().unwrap()
    );
    // The same resolver controls per-file LSP access, including an added worktree.
    let file = worktree.join("main.fake");
    let context = canter_tools::lsp::FileContext {
        cwd: main.clone(),
        additional_directories: vec![worktree.clone()],
        environment: Default::default(),
        round: canter_tools::lsp::Round::Modify,
    };
    let access = crate::trust::file_access(fixture.root.path().join("state"));
    assert!(access(&file, &context));
    let metadata = main.join(".git/worktrees/worktree");
    let original_gitdir = fs::read(metadata.join("gitdir")).unwrap();
    for broken in ["gitdir", "commondir"] {
        let path = metadata.join(broken);
        let original = fs::read(&path).unwrap();
        fs::write(
            &path,
            if broken == "gitdir" {
                main.join(".git")
            } else {
                main.clone()
            }
            .to_str()
            .unwrap(),
        )
        .unwrap();
        let result: Value = fixture
            .client
            .get(query.clone())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["trusted"], false, "{broken}");
        assert_eq!(
            result["path"],
            worktree.canonicalize().unwrap().to_str().unwrap()
        );
        assert!(!access(&file, &context), "{broken}");
        fs::write(path, original).unwrap();
    }
    assert_eq!(fs::read(metadata.join("gitdir")).unwrap(), original_gitdir);
    // Copying a valid worktree pointer or using a symlink cannot borrow its trust.
    let impostor = fixture.root.path().join("impostor");
    fs::create_dir(&impostor).unwrap();
    fs::copy(worktree.join(".git"), impostor.join(".git")).unwrap();
    let query = reqwest::Url::parse_with_params(
        "http://localhost/v1/folderTrust",
        &[("cwd", impostor.to_str().unwrap())],
    )
    .unwrap();
    for symlink in [false, true] {
        if symlink {
            fs::remove_file(impostor.join(".git")).unwrap();
            std::os::unix::fs::symlink(worktree.join(".git"), impostor.join(".git")).unwrap();
        }
        let result: Value = fixture
            .client
            .get(query.clone())
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["trusted"], false);
        assert_eq!(
            result["path"],
            impostor.canonicalize().unwrap().to_str().unwrap()
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn folder_trust_is_shared_by_repository_subdirectories() {
    let fixture = Fixture::new(json!({})).await;
    let repo = fixture.root.path().join("repo");
    fs::create_dir_all(repo.join("one/two")).unwrap();
    assert!(
        std::process::Command::new("/usr/bin/git")
            .args(["init", "--quiet"])
            .arg(&repo)
            .status()
            .unwrap()
            .success()
    );
    let trust = "http://localhost/v1/folderTrust";
    let query = fixture
        .client
        .get(
            reqwest::Url::parse_with_params(trust, &[("cwd", repo.join("one").to_str().unwrap())])
                .unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(query.status(), reqwest::StatusCode::OK);
    assert_eq!(query.json::<Value>().await.unwrap()["trusted"], false);
    fixture
        .client
        .post(trust)
        .json(&json!({"cwd":repo.join("one")}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let result: Value = fixture
        .client
        .get(
            reqwest::Url::parse_with_params(
                trust,
                &[("cwd", repo.join("one/two").to_str().unwrap())],
            )
            .unwrap(),
        )
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["trusted"], true);
    assert_eq!(
        result["path"],
        repo.canonicalize().unwrap().to_str().unwrap()
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn untrusted_session_does_not_start_lsp_even_when_globally_enabled() {
    let config = json!({"enabled":true,"servers":{"fake":{"command":super::lsp_tests::fixture_executable(),"extensions":{"fake":"fake"},"root_markers":["project.marker"]}}});
    let mut fixture = Fixture::new(json!({"lsp":config})).await;
    fs::write(fixture.root.path().join("project.marker"), "").unwrap();
    fs::write(fixture.root.path().join("main.fake"), "").unwrap();
    fs::write(
        fixture.root.path().join("AGENTS.md"),
        "untrusted instruction marker",
    )
    .unwrap();
    let skill = fixture
        .root
        .path()
        .join(".agents/skills/trust-check/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(
        skill,
        "---\nname: trust-check\ndescription: untrusted skill marker\n---\nUse this skill.",
    )
    .unwrap();
    let url = fixture
        .create_with_env(json!({"LSP_TEST_DIR":fixture.root.path(),"LSP_TEST_MODE":"sync"}))
        .await;
    fixture.input(&url).await;
    let request = fixture.request().await;
    let prompt = request.body["messages"].to_string();
    assert!(prompt.contains("untrusted instruction marker"));
    assert!(prompt.contains("untrusted skill marker"));
    request.tool("write", json!({"path":"main.fake","content":"bad\n"}));
    let running = fixture.request().await;
    let started = tokio::time::timeout(Duration::from_secs(1), async {
        until(|| fixture.root.path().join("starts").exists()).await;
    })
    .await
    .is_ok();
    running.stop();
    fixture.idle(&url).await;
    assert_eq!(
        fixture
            .snapshot(&url)
            .await
            .session
            .settings
            .restricted_mode,
        Some(true)
    );
    assert!(
        fixture
            .snapshot(&url)
            .await
            .session
            .language_servers
            .is_empty()
    );
    fixture.config["lsp"] = json!({"enabled":false});
    fixture.applied("restricted-off").await;
    fixture.config["lsp"] = config;
    fixture.applied("restricted-on").await;
    fixture.input(&url).await;
    fixture.request().await.tool(
        "write",
        json!({"path":"main.fake","content":"still restricted\n"}),
    );
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert!(
        fixture
            .snapshot(&url)
            .await
            .session
            .language_servers
            .is_empty()
    );
    assert!(!fixture.root.path().join("starts").exists());
    fixture.shutdown().await;
    assert!(
        !started,
        "untrusted sessions must not start a language server"
    );
}

#[tokio::test]
async fn lsp_configuration_keeps_the_old_process_until_turn_end_and_restarts_on_the_next_write() {
    let config = json!({"enabled":true,"servers":{"fake":{"command":super::lsp_tests::fixture_executable(),"extensions":{"fake":"fake"},"root_markers":["project.marker"]}}});
    let mut fixture = Fixture::new(json!({"lsp":config})).await;
    fixture.trust(fixture.root.path()).await;
    fs::write(fixture.root.path().join("project.marker"), "").unwrap();
    fs::write(fixture.root.path().join("main.fake"), "").unwrap();
    let url = fixture
        .create_with_env(json!({"LSP_TEST_DIR":fixture.root.path(),"LSP_TEST_MODE":"sync"}))
        .await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"main.fake","content":"bad\n"}));
    let running = fixture.request().await;
    let starts = fixture.root.path().join("starts");
    until(|| starts.exists()).await;
    let old: u32 = fs::read_to_string(&starts).unwrap().trim().parse().unwrap();
    fixture.config["lsp"] = json!({"enabled":false});
    fixture.applied("lsp-off").await;
    assert!(!gone(old));
    running.stop();
    fixture.idle(&url).await;
    until(|| gone(old)).await;
    fixture.input(&url).await;
    fixture.request().await.tool(
        "write",
        json!({"path":"main.fake","content":"without lsp\n"}),
    );
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(fs::read_to_string(&starts).unwrap().lines().count(), 1);
    fixture.config["lsp"] = config;
    fixture.applied("lsp-on").await;
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"main.fake","content":"bad again\n"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    until(|| fs::read_to_string(&starts).unwrap().lines().count() == 2).await;
    let pids: Vec<u32> = fs::read_to_string(&starts)
        .unwrap()
        .lines()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    assert_ne!(pids[0], pids[1]);
    fixture.shutdown().await;
    until(|| gone(pids[1])).await;
}

#[tokio::test]
async fn disabling_then_enabling_lsp_still_waits_for_the_original_turn() {
    let config = json!({"enabled":true,"servers":{"fake":{"command":super::lsp_tests::fixture_executable(),"extensions":{"fake":"fake"},"root_markers":["project.marker"]}}});
    let mut fixture = Fixture::new(json!({"lsp":config})).await;
    fixture.trust(fixture.root.path()).await;
    fs::write(fixture.root.path().join("project.marker"), "").unwrap();
    fs::write(fixture.root.path().join("main.fake"), "").unwrap();
    let env = json!({"LSP_TEST_DIR":fixture.root.path(),"LSP_TEST_MODE":"sync"});
    let a = fixture.create_with_env(env.clone()).await;
    let b = fixture.create_with_env(env).await;
    fixture.input(&a).await;
    fixture
        .request()
        .await
        .tool("write", json!({"path":"main.fake","content":"first\n"}));
    let running = fixture.request().await;
    let starts = fixture.root.path().join("starts");
    until(|| starts.exists()).await;
    let old: u32 = fs::read_to_string(&starts).unwrap().trim().parse().unwrap();
    fixture.config["lsp"] = json!({"enabled":false});
    fixture.applied("disabled").await;
    fixture.config["lsp"] = config;
    fixture.applied("enabled-again").await;
    let client = fixture.client.clone();
    let pending = tokio::spawn(async move {
        client
            .post(format!("{b}/input"))
            .json(&json!({"input":[{"type":"text","text":"next"}],"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        fixture.requests.try_recv().is_err(),
        "no model work before handover"
    );
    assert!(!gone(old));
    running.stop();
    tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    fixture.request().await.stop();
    until(|| gone(old)).await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn waiting_for_services_does_not_lock_other_sessions_for_readers_or_close() {
    use reqwest::Method;
    for (method, suffix) in [
        (Method::GET, "/usage"),
        (Method::GET, "/events"),
        (Method::POST, "/resume"),
        (Method::DELETE, ""),
    ] {
        let mut fixture = Fixture::new(json!({})).await;
        let a = fixture.create().await;
        let b = fixture.create().await;
        fixture.input(&a).await;
        let running = fixture.request().await;
        fixture.config["lsp"] = json!({"enabled":true});
        fixture.applied("waiting").await;
        let client = fixture.client.clone();
        let input_url = b.clone();
        let pending = tokio::spawn(async move {
            client
                .post(format!("{input_url}/input"))
                .json(&json!({"input":[{"type":"text","text":"next"}],"clientName":"test"}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            fixture.requests.try_recv().is_err(),
            "no model work before handover"
        );
        let client = fixture.client.clone();
        let reader = tokio::spawn(async move {
            let url = if suffix == "/events" {
                format!(
                    "http://localhost/v1/events?session={}&after=0",
                    b.rsplit('/').next().unwrap()
                )
            } else {
                format!("{b}{suffix}")
            };
            client
                .request(method, url)
                .json(&json!({"env":{}}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), fixture.snapshot(&a))
                .await
                .expect("another session's API must remain available")
                .session
                .status,
            SessionStatus::Working
        );
        pending.await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), reader)
            .await
            .expect("the waiting session's readers and close must also remain available")
            .unwrap();
        running.stop();
        fixture.shutdown().await;
    }
}
