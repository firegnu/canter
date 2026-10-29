use super::*;
use axum::{
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

#[derive(Default)]
struct McpState {
    tools: Vec<Value>,
    calls: Vec<Value>,
    cancelled: Vec<Value>,
    fail_next_call: bool,
    slow_listing: bool,
}

struct HttpMcp {
    state: Arc<Mutex<McpState>>,
    notifications: tokio::sync::broadcast::Sender<String>,
    task: JoinHandle<()>,
    url: String,
}
impl HttpMcp {
    async fn start(names: &[&str]) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(McpState {
            tools: names.iter().map(|name| definition(name)).collect(),
            ..Default::default()
        }));
        let (notifications, _) = tokio::sync::broadcast::channel::<String>(16);
        let broadcast = notifications.clone();
        let app = Router::new()
            .route(
                "/mcp",
                post(rpc)
                    .get(move || {
                        let rx = broadcast.subscribe();
                        async move {
                            axum::response::Sse::new(futures_util::stream::unfold(
                                rx,
                                |mut rx| async move {
                                    rx.recv().await.ok().map(|message| {
                                        (
                                            Ok::<_, std::convert::Infallible>(
                                                axum::response::sse::Event::default().data(message),
                                            ),
                                            rx,
                                        )
                                    })
                                },
                            ))
                        }
                    })
                    .delete(|| async { StatusCode::OK }),
            )
            .with_state(state.clone());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            state,
            notifications,
            task,
            url,
        }
    }
    fn config(&self) -> Value {
        json!({"transport":"http","url":self.url,"headers":{"x-test-key":"test-only-http-mcp-key"}})
    }
    async fn change_tools(&self, names: &[&str]) {
        until(|| self.notifications.receiver_count() > 0).await;
        self.state.lock().unwrap().tools = names.iter().map(|name| definition(name)).collect();
        self.notifications
            .send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"}).to_string())
            .unwrap();
    }
}
impl Drop for HttpMcp {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn definition(name: &str) -> Value {
    json!({"name":name,"description":format!("http {name}"),"inputSchema":{"type":"object","properties":{"text":{"type":"string"}}},"annotations":{"readOnlyHint":true}})
}
async fn until(test: impl Fn() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !test() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("observable progress");
}
async fn rpc(
    State(state): State<Arc<Mutex<McpState>>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    assert_eq!(headers["x-test-key"], "test-only-http-mcp-key");
    let method = request["method"].as_str().unwrap();
    if method == "notifications/cancelled" {
        state.lock().unwrap().cancelled.push(request);
        return StatusCode::ACCEPTED.into_response();
    }
    if request.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    let result = match method {
        "initialize" => {
            let slow = state.lock().unwrap().slow_listing;
            if slow {
                tokio::time::sleep(Duration::from_secs(7)).await;
            }
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"fixture","version":"1"}})
        }
        "tools/list" => {
            let slow = state.lock().unwrap().slow_listing;
            if slow {
                std::future::pending::<()>().await;
            }
            let state = state.lock().unwrap();
            if state.tools.len() > 1 && request["params"]["cursor"].is_null() {
                json!({"tools":state.tools[..1],"nextCursor":"rest"})
            } else if request["params"]["cursor"] == "rest" {
                json!({"tools":state.tools[1..]})
            } else {
                json!({"tools":state.tools})
            }
        }
        "tools/call" => {
            let fail = {
                let mut state = state.lock().unwrap();
                state.calls.push(request.clone());
                std::mem::take(&mut state.fail_next_call)
            };
            if fail {
                return (StatusCode::SERVICE_UNAVAILABLE, "test-only-http-mcp-key").into_response();
            }
            match request["params"]["name"].as_str().unwrap() {
                "pending" => { std::future::pending::<()>().await; unreachable!() }
                "content" => json!({"content":[{"type":"text","text":format!("HEAD{}TAIL","界".repeat(20_000))},{"type":"image","data":"aGVsbG8=","mimeType":"image/png"},{"type":"text","text":"end"}]}),
                "error" => json!({"isError":true,"content":[{"type":"text","text":"operation failed test-only-http-mcp-key"}]}),
                "rpc_error" => return Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32603,"message":"test-only-http-mcp-key"}})).into_response(),
                "bearer" => json!({"content":[{"type":"text","text":headers["authorization"].to_str().unwrap().strip_prefix("Bearer ").unwrap()}]}),
                _ => json!({"content":[{"type":"text","text":"http result"}]}),
            }
        }
        _ => json!({}),
    };
    (
        [("mcp-session-id", "fixture-session")],
        Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})),
    )
        .into_response()
}

async fn start(http: &HttpMcp, replies: Vec<String>) -> Fixture {
    Fixture::start_with_mcp(
        replies,
        "fake-model",
        None,
        json!({}),
        json!({"remote":http.config(),"second":http.config()}),
    )
    .await
}
async fn create(fixture: &Fixture, mode: &str) -> SessionCreateResponse {
    fixture.request(Method::POST,"/v1/sessions",json!({"cwd":fixture.root.path().join("work"),"env":{},"model":"fake-model","approvalPolicy":mode,"clientName":"test"})).await.json().await.unwrap()
}
async fn complete(fixture: &Fixture, id: &str, after: u64) -> Vec<Event> {
    fixture
        .events_until(id, after, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await
}
fn tool_results(fixture: &Fixture) -> Vec<String> {
    fixture.script.lock().unwrap().requests.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap().into())
        .collect()
}

fn call(names: &[&str]) -> String {
    call_with_arguments(names, "{\"text\":\"hello\"}")
}
fn call_with_arguments(names: &[&str], arguments: &str) -> String {
    let calls: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            json!({
                "index":index,"id":format!("call-{index}"),"type":"function",
                "function":{"name":name,"arguments":arguments}
            })
        })
        .collect();
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]})
    )
}

async fn compile_stdio(temp: &std::path::Path) -> std::path::PathBuf {
    let executable = temp.join("fake-mcp");
    let output = Command::new("rustc")
        .args(["--edition=2024", "tests/fixtures/mcp_stdio.rs", "-o"])
        .arg(&executable)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}

#[tokio::test]
async fn redaction_preserves_results_with_short_env_values() {
    let temp = tempfile::tempdir().unwrap();
    let executable = compile_stdio(temp.path()).await;
    let mut fixture = Fixture::start_with_mcp(
        vec![call(&["mcp__local__echo"]), text_reply()],
        "fake-model", None, json!({}),
        json!({"local":{"transport":"stdio","command":executable,"args":["--fixture"],"env":{"DEBUG":"t","LANG":"C","RUST_LOG":"info","MODE":"result"}}}),
    ).await;
    let created = create(&fixture, "allowAll").await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    fixture.stop().await;
    assert_eq!(tool_results(&fixture), ["stdio result"]);
}

#[tokio::test]
async fn redaction_preserves_paths_from_configured_home() {
    let temp = tempfile::tempdir().unwrap();
    let executable = compile_stdio(temp.path()).await;
    let mut fixture = Fixture::start_with_mcp(
        vec![call(&["mcp__local__echo"]), text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"local":{"transport":"stdio","command":executable,"args":["--fixture"],"env":{
            "HOME":"/mcp/test-home","RETURN_HOME_PATH":"1"
        }}}),
    )
    .await;
    let created = create(&fixture, "allowAll").await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    fixture.stop().await;
    assert_eq!(tool_results(&fixture), ["/mcp/test-home/project/file.txt"]);
}

#[tokio::test]
async fn redaction_removes_bearer_token_echoed_without_prefix() {
    let http = HttpMcp::start(&["bearer"]).await;
    let mut config = http.config();
    config["headers"]["authorization"] = json!("Bearer test-only-bearer-token");
    let mut fixture = Fixture::start_with_mcp(
        vec![call(&["mcp__remote__bearer"]), text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"remote":config}),
    )
    .await;
    let created = create(&fixture, "allowAll").await;
    fixture.input(&created.session.id).await;
    let events = complete(&fixture, &created.session.id, created.event_seq).await;
    fixture.stop().await;
    assert_eq!(tool_results(&fixture), ["[redacted]"]);
    assert!(
        !serde_json::to_string(&events)
            .unwrap()
            .contains("test-only-bearer-token")
    );
}

#[tokio::test]
async fn stdio_environment_only_inherits_basics_and_applies_config() {
    let temp = tempfile::tempdir().unwrap();
    let executable = compile_stdio(temp.path()).await;
    let environment_file = temp.path().join("environment");
    let mut fixture = Fixture::start_with_mcp_env(
        vec![call(&["mcp__local__echo"]), text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"local":{"transport":"stdio","command":executable,"args":["--fixture"],"env":{
            "ENV_FILE":environment_file,"HOME":"/mcp/configured-home","EXPLICIT_MARKER":"configured"
        }}}),
        &[
            ("MCP_UNRELATED_SECRET", "test-only-unrelated-secret"),
            ("PATH", "/server/bin"),
            ("LANG", "C"),
        ],
    )
    .await;
    let created = create(&fixture, "allowAll").await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    fixture.stop().await;
    assert_eq!(tool_results(&fixture), ["stdio result"]);
    let contents = fs::read_to_string(environment_file).unwrap();
    let environment: std::collections::BTreeMap<_, _> = contents
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();
    assert!(!environment.contains_key("MCP_UNRELATED_SECRET"));
    assert!(!environment.contains_key("CANTER_HOME"));
    assert!(
        !environment.contains_key("USER"),
        "missing base variables stay absent"
    );
    assert_eq!(environment["PATH"], "/server/bin");
    assert_eq!(environment["LANG"], "C");
    assert_eq!(environment["HOME"], "/mcp/configured-home");
    assert_eq!(environment["EXPLICIT_MARKER"], "configured");
}

#[tokio::test]
async fn stdio_and_http_tools_reach_model_in_fixed_order_and_return_results() {
    let temp = tempfile::tempdir().unwrap();
    let executable = temp.path().join("fake-mcp");
    let output = Command::new("rustc")
        .args(["--edition=2024", "tests/fixtures/mcp_stdio.rs", "-o"])
        .arg(&executable)
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let http = HttpMcp::start(&["echo"]).await;
    let mut fixture = Fixture::start_with_mcp(vec![call(&["mcp__local__echo","mcp__remote__echo"]),text_reply()], "fake-model",None,json!({}),json!({
        "remote":http.config(),
        "local":{"transport":"stdio","command":executable,"args":["--fixture"],"env":{"PID_FILE":temp.path().join("pid"),"CHILD_PID_FILE":temp.path().join("child-pid"),"MCP_TEST_KEY":"test-only-stdio-mcp-key"}}
    })).await;
    let created = fixture.request(Method::POST,"/v1/sessions",json!({"cwd":fixture.root.path().join("work"),"env":{},"model":"fake-model","approvalPolicy":"allowAll","clientName":"test"})).await.json::<SessionCreateResponse>().await.unwrap();
    fixture.input(&created.session.id).await;
    fixture
        .events_until(&created.session.id, created.event_seq, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
    {
        let script = fixture.script.lock().unwrap();
        let names: Vec<_> = script.requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "read",
                "write",
                "edit",
                "shell",
                "ask_user",
                "web_fetch",
                "update_plan",
                "submit_plan",
                "shell_output",
                "shell_stop",
                "monitor",
                "get_goal",
                "update_goal",
                "mcp__local__echo",
                "mcp__remote__echo"
            ]
        );
        assert_eq!(script.requests[0]["tools"], script.requests[1]["tools"]);
        assert_eq!(
            script.requests[0]["tools"][14]["function"]["description"],
            "http echo"
        );
        assert_eq!(
            script.requests[0]["tools"][14]["function"]["parameters"],
            definition("echo")["inputSchema"]
        );
        let results: Vec<_> = script.requests[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap())
            .collect();
        assert_eq!(results, ["stdio result", "http result"]);
    }
    let status: Value = fixture
        .request(Method::GET, "/v1/mcp", json!(null))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status["data"][0]["name"], "local");
    assert_eq!(status["data"][0]["connected"], true);
    assert_eq!(status["data"][1]["tools"][0]["name"], "mcp__remote__echo");
    let old = OwnedPid::read(&temp.path().join("pid"));
    let old_child = OwnedPid::read(&temp.path().join("child-pid"));
    let replacement =
        Fixture::spawn_args(fixture.root.path(), &["serve", "--replace", "--foreground"]);
    let mut previous = std::mem::replace(&mut fixture.child, replacement);
    assert!(
        tokio::time::timeout(Duration::from_secs(15), previous.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    fixture.ready().await;
    let new = OwnedPid::read(&temp.path().join("pid"));
    let new_child = OwnedPid::read(&temp.path().join("child-pid"));
    until(|| !old.alive() && !old_child.alive()).await;
    assert!(
        !fs::read_to_string(fixture.root.path().join("server.log"))
            .unwrap()
            .contains("test-only-stdio-mcp-key")
    );
    fixture.stop().await;
    until(|| !new.alive() && !new_child.alive()).await;
}

#[tokio::test]
async fn permissions_grant_only_one_tool_and_review_never_asks() {
    use canter_protocol::RequestKind;
    for mode in ["alwaysAsk", "askForExecution", "allowAll"] {
        let http = HttpMcp::start(&["echo", "other"]).await;
        let names = [
            "mcp__remote__echo",
            "mcp__remote__echo",
            "mcp__remote__other",
            "mcp__second__echo",
        ];
        let mut fixture = start(
            &http,
            vec![
                call(&names),
                text_reply(),
                if mode == "alwaysAsk" {
                    call_with_arguments(&names, "invalid JSON")
                } else {
                    call(&names)
                },
                text_reply(),
            ],
        )
        .await;
        let created = create(&fixture, mode).await;
        let id = &created.session.id;
        fixture.input(id).await;
        let mut after = created.event_seq;
        if mode != "allowAll" {
            for (name, decision) in [
                (names[0], "acceptForSession"),
                (names[2], "decline"),
                (names[3], "decline"),
            ] {
                let events = fixture
                    .events_until(id, after, |event| {
                        matches!(
                            event.payload,
                            EventPayload::RequestOpened { .. } | EventPayload::TurnCompleted { .. }
                        )
                    })
                    .await;
                let opened = events.last().unwrap();
                after = opened.seq;
                let EventPayload::RequestOpened { request } = &opened.payload else {
                    panic!("MCP must request approval")
                };
                let RequestKind::Approval(approval) = &request.kind else {
                    panic!("approval expected")
                };
                assert_eq!(approval.tool, name);
                assert_eq!(approval.arguments, json!({"text":"hello"}));
                assert_eq!(
                    fixture
                        .request(
                            Method::POST,
                            &format!("/v1/sessions/{id}/requests/{}", request.id),
                            json!({"decision":decision,"clientName":"test"})
                        )
                        .await
                        .status(),
                    200
                );
            }
        }
        let events = complete(&fixture, id, after).await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::RequestOpened { .. }))
        );
        let expected = if mode == "allowAll" { 4 } else { 2 };
        assert_eq!(http.state.lock().unwrap().calls.len(), expected);
        let after = events.last().unwrap().seq;
        assert_eq!(fixture.request(Method::POST,&format!("/v1/sessions/{id}/input"),json!({"input":[{"type":"text","text":"review"}],"review":"Review MCP","clientName":"test"})).await.status(),200);
        let events = complete(&fixture, id, after).await;
        assert!(
            !events
                .iter()
                .any(|e| matches!(e.payload, EventPayload::RequestOpened { .. }))
        );
        assert_eq!(http.state.lock().unwrap().calls.len(), expected);
        assert!(
            tool_results(&fixture)
                .iter()
                .rev()
                .take(4)
                .all(|text| text == "review is read-only")
        );
        fixture.stop().await;
    }
}

#[tokio::test]
async fn content_limits_placeholders_and_errors_do_not_expose_keys() {
    let http = HttpMcp::start(&["content", "error", "rpc_error"]).await;
    let mut fixture = start(
        &http,
        vec![
            call(&[
                "mcp__remote__content",
                "mcp__remote__error",
                "mcp__remote__rpc_error",
            ]),
            text_reply(),
        ],
    )
    .await;
    let created = create(&fixture, "allowAll").await;
    fixture.input(&created.session.id).await;
    let events = complete(&fixture, &created.session.id, created.event_seq).await;
    let results = tool_results(&fixture);
    assert!(results[0].starts_with("HEAD"));
    assert!(results[0].contains("[truncated "));
    assert!(results[0].contains("TAIL\n[MCP non-text content omitted]\nend"));
    assert!(results[0].len() < 33_000);
    assert_eq!(results[1], "MCP tool error: operation failed [redacted]");
    assert!(results[2].contains("failed"));
    let events = serde_json::to_string(&events).unwrap();
    assert!(!events.contains("test-only-http-mcp-key"));
    assert!(events.contains("\"status\":\"failed\""));
    assert!(!events.contains("aGVsbG8="));
    assert!(
        !fs::read_to_string(fixture.root.path().join("server.log"))
            .unwrap()
            .contains("test-only-http-mcp-key")
    );
    fixture.stop().await;
}

#[tokio::test]
async fn notifications_only_change_tools_after_session_is_reopened() {
    let http = HttpMcp::start(&["zeta", "echo"]).await;
    let mut fixture = start(&http, vec![text_reply(), text_reply(), text_reply()]).await;
    let created = create(&fixture, "allowAll").await;
    let id = &created.session.id;
    fixture.input(id).await;
    let events = complete(&fixture, id, created.event_seq).await;
    http.change_tools(&["new", "echo"]).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status: Value = fixture
                .request(Method::GET, "/v1/mcp", json!(null))
                .await
                .json()
                .await
                .unwrap();
            if status["data"]
                .as_array()
                .unwrap()
                .iter()
                .all(|server| server["tools"].to_string().contains("__new"))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fixture.input(id).await;
    complete(&fixture, id, events.last().unwrap().seq).await;
    assert_eq!(
        fixture
            .request(Method::DELETE, &format!("/v1/sessions/{id}"), json!({}))
            .await
            .status(),
        200
    );
    let reopened: Value = fixture
        .request(
            Method::POST,
            &format!("/v1/sessions/{id}/resume"),
            json!({"env":{}}),
        )
        .await
        .json()
        .await
        .unwrap();
    fixture.input(id).await;
    complete(&fixture, id, reopened["eventSeq"].as_u64().unwrap()).await;
    {
        let script = fixture.script.lock().unwrap();
        assert_eq!(script.requests[0]["tools"], script.requests[1]["tools"]);
        assert!(script.requests[0]["tools"].to_string().contains("__zeta"));
        assert!(script.requests[2]["tools"].to_string().contains("__new"));
        assert!(!script.requests[2]["tools"].to_string().contains("__zeta"));
    }
    fixture.stop().await;
}

#[tokio::test]
async fn connection_failure_invalid_names_and_disconnect_are_visible_without_stalling() {
    let temp = tempfile::tempdir().unwrap();
    let executable = compile_stdio(temp.path()).await;
    let http = HttpMcp::start(&["echo", "echo", "invalid.name"]).await;
    let mut fixture=Fixture::start_with_mcp(vec![call(&["mcp__local__echo"]),text_reply()],"fake-model",None,json!({}),json!({
        "bad":{"transport":"stdio","command":"/nonexistent/canter-mcp-fixture","env":{"KEY":"test-only-secret"}},
        "local":{"transport":"stdio","command":executable,"args":["--fixture"],"env":{"PID_FILE":temp.path().join("pid")}},
        "remote":http.config()
    })).await;
    let status: Value = fixture
        .request(Method::GET, "/v1/mcp", json!(null))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status["data"][0]["connected"], false);
    assert!(!status["data"][0]["error"].as_str().unwrap().is_empty());
    assert_eq!(status["data"][1]["connected"], true);
    assert_eq!(status["data"][2]["connected"], true);
    assert_eq!(status["data"][2]["tools"].as_array().unwrap().len(), 1);
    assert!(!status["data"][2]["warnings"].as_array().unwrap().is_empty());
    let created = create(&fixture, "allowAll").await;
    let process = OwnedPid::read(&temp.path().join("pid"));
    nix::sys::signal::kill(process.0, nix::sys::signal::Signal::SIGKILL).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let status: Value = fixture
                .request(Method::GET, "/v1/mcp", json!(null))
                .await
                .json()
                .await
                .unwrap();
            if status["data"][1]["connected"] == false {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fixture.input(&created.session.id).await;
    let events = complete(&fixture, &created.session.id, created.event_seq).await;
    assert_eq!(tool_results(&fixture), ["MCP service disconnected"]);
    let EventPayload::TurnCompleted { turn } = &events.last().unwrap().payload else {
        unreachable!()
    };
    assert_eq!(turn.status, canter_protocol::TurnStatus::Completed);
    {
        let script = fixture.script.lock().unwrap();
        assert_eq!(script.requests[0]["tools"], script.requests[1]["tools"]);
    }
    let status: Value = fixture
        .request(Method::GET, "/v1/mcp", json!(null))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status["data"][1]["connected"], false);
    assert_eq!(status["data"][2]["connected"], true);
    fixture.stop().await;
    until(|| !process.alive()).await;
}

#[tokio::test]
async fn one_http_503_keeps_connection_and_tools_for_new_sessions() {
    let http = HttpMcp::start(&["echo"]).await;
    let mut fixture = Fixture::start_with_mcp(
        vec![
            call(&["mcp__remote__echo"]),
            text_reply(),
            call(&["mcp__remote__echo"]),
            text_reply(),
        ],
        "fake-model",
        None,
        json!({}),
        json!({"remote":http.config()}),
    )
    .await;
    let first = create(&fixture, "allowAll").await;
    http.state.lock().unwrap().fail_next_call = true;
    fixture.input(&first.session.id).await;
    complete(&fixture, &first.session.id, first.event_seq).await;
    let failed_result = tool_results(&fixture);
    let status: Value = fixture
        .request(Method::GET, "/v1/mcp", json!(null))
        .await
        .json()
        .await
        .unwrap();
    let second = create(&fixture, "allowAll").await;
    fixture.input(&second.session.id).await;
    complete(&fixture, &second.session.id, second.event_seq).await;
    fixture.stop().await;
    assert_eq!(failed_result, ["MCP tool call failed"]);
    assert_eq!(status["data"][0]["connected"], true);
    assert!(status["data"][0]["error"].is_null());
    let script = fixture.script.lock().unwrap();
    assert_eq!(script.requests[0]["tools"], script.requests[2]["tools"]);
    assert!(
        script.requests[2]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "mcp__remote__echo")
    );
    drop(script);
    assert_eq!(tool_results(&fixture), ["http result"]);
    assert_eq!(http.state.lock().unwrap().calls.len(), 2);
}

struct OwnedPid(nix::unistd::Pid);
impl OwnedPid {
    fn read(path: &std::path::Path) -> Self {
        Self(nix::unistd::Pid::from_raw(
            fs::read_to_string(path).unwrap().parse().unwrap(),
        ))
    }
    fn alive(&self) -> bool {
        nix::sys::signal::kill(self.0, None).is_ok()
    }
}
impl Drop for OwnedPid {
    fn drop(&mut self) {
        if self.alive() {
            let _ = nix::sys::signal::kill(self.0, nix::sys::signal::Signal::SIGKILL);
        }
    }
}

#[tokio::test]
async fn interrupt_cancels_the_mcp_request_and_next_turn_can_continue() {
    let http = HttpMcp::start(&["pending", "echo"]).await;
    let mut fixture = start(
        &http,
        vec![
            call(&["mcp__remote__pending"]),
            call(&["mcp__remote__echo"]),
            text_reply(),
        ],
    )
    .await;
    let created = create(&fixture, "allowAll").await;
    let id = &created.session.id;
    fixture.input(id).await;
    until(|| !http.state.lock().unwrap().calls.is_empty()).await;
    let snapshot: Value = fixture
        .request(Method::GET, &format!("/v1/sessions/{id}"), json!(null))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        fixture
            .request(
                Method::POST,
                &format!("/v1/sessions/{id}/interrupt"),
                json!({"turnId":snapshot["session"]["turns"][0]["id"]})
            )
            .await
            .status(),
        200
    );
    let events = complete(&fixture, id, created.event_seq).await;
    let EventPayload::TurnCompleted { turn } = &events.last().unwrap().payload else {
        unreachable!()
    };
    assert_eq!(turn.status, canter_protocol::TurnStatus::Interrupted);
    until(|| !http.state.lock().unwrap().cancelled.is_empty()).await;
    fixture.input(id).await;
    complete(&fixture, id, events.last().unwrap().seq).await;
    assert_eq!(tool_results(&fixture).last().unwrap(), "http result");
    fixture.stop().await;
}

#[tokio::test]
async fn connection_and_call_timeouts_are_bounded_and_cancel_the_request() {
    let slow = HttpMcp::start(&["echo"]).await;
    slow.state.lock().unwrap().slow_listing = true;
    let http = HttpMcp::start(&["pending"]).await;
    // Same behaviour as the 10 s / 120 s defaults, without waiting minutes.
    let mut slow_config = slow.config();
    slow_config["startup_timeout_sec"] = 1.into();
    let mut remote_config = http.config();
    remote_config["tool_timeout_sec"] = 1.into();
    let mut fixture = Fixture::start_with_mcp(
        vec![call(&["mcp__remote__pending"]), text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"slow":slow_config,"remote":remote_config}),
    )
    .await;
    let status: Value = fixture
        .request(Method::GET, "/v1/mcp", json!(null))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status["data"][1]["connected"], false);
    assert!(
        status["data"][1]["error"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
    let created = create(&fixture, "allowAll").await;
    let id = &created.session.id;
    fixture.input(id).await;
    until(|| !http.state.lock().unwrap().calls.is_empty()).await;
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let snapshot: Value = fixture
                .request(Method::GET, &format!("/v1/sessions/{id}"), json!(null))
                .await
                .json()
                .await
                .unwrap();
            if snapshot["session"]["turns"][0]["status"] == "completed" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("MCP call must time out and finish the turn");
    assert_eq!(tool_results(&fixture)[0], "MCP call timed out");
    until(|| !http.state.lock().unwrap().cancelled.is_empty()).await;
    fixture.stop().await;
}

#[tokio::test]
async fn shutdown_during_stdio_initialization_reaps_the_process_group() {
    let root = tempfile::Builder::new()
        .prefix("canter-mcp-")
        .tempdir_in("/tmp")
        .unwrap();
    for directory in ["state", "home", "work"] {
        fs::create_dir(root.path().join(directory)).unwrap();
    }
    let executable = root.path().join("fake-mcp");
    assert!(
        Command::new("rustc")
            .args(["--edition=2024", "tests/fixtures/mcp_stdio.rs", "-o"])
            .arg(&executable)
            .status()
            .await
            .unwrap()
            .success()
    );
    fs::write(root.path().join("state/config.json"),json!({
        "base_url":"http://127.0.0.1:1","model":"fake-model","api_key":"test-only-model-key","context_window":1048576,
        "mcp_servers":{"pending":{"transport":"stdio","command":executable,"args":["--fixture"],"env":{
            "PID_FILE":root.path().join("pid"),"CHILD_PID_FILE":root.path().join("child-pid"),"HANG_INITIALIZE":"true"
        }}}
    }).to_string()).unwrap();
    let mut daemon = Fixture::spawn(root.path());
    // fs::write creates the file before writing it, so wait for the content.
    until(|| {
        ["pid", "child-pid"].iter().all(|name| {
            fs::read_to_string(root.path().join(name)).is_ok_and(|text| text.parse::<i32>().is_ok())
        })
    })
    .await;
    let child = OwnedPid::read(&root.path().join("pid"));
    let grandchild = OwnedPid::read(&root.path().join("child-pid"));
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(daemon.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(15), daemon.wait())
            .await
            .unwrap()
            .unwrap()
            .success(),
        "initialization must handle shutdown gracefully"
    );
    until(|| !child.alive() && !grandchild.alive()).await;
}

#[tokio::test]
async fn colliding_qualified_names_are_skipped_and_reported() {
    let first = HttpMcp::start(&["b__c"]).await;
    let second = HttpMcp::start(&["c"]).await;
    let mut fixture = Fixture::start_with_mcp(
        vec![text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"a":first.config(),"a__b":second.config()}),
    )
    .await;
    let created = create(&fixture, "allowAll").await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    let status: Value = fixture
        .request(Method::GET, "/v1/mcp", json!(null))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(status["data"][1]["tools"], json!([]));
    assert!(!status["data"][1]["warnings"].as_array().unwrap().is_empty());
    assert_eq!(
        fixture.script.lock().unwrap().requests[0]["tools"]
            .as_array()
            .unwrap()
            .len(),
        14 // Thirteen built-ins (including get_goal/update_goal) and one non-colliding MCP tool.
    );
    assert!(
        fs::read_to_string(fixture.root.path().join("server.log"))
            .unwrap()
            .contains("duplicate model tool name")
    );
    fixture.stop().await;
}

#[tokio::test]
async fn http_mcp_ignores_server_proxy_environment() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncWriteExt;

    let http = HttpMcp::start(&["echo"]).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", listener.local_addr().unwrap());
    let connections = Arc::new(AtomicUsize::new(0));
    let received = connections.clone();
    let proxy = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            received.fetch_add(1, Ordering::SeqCst);
            let _ = socket.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
        }
    });
    let mut fixture = Fixture::start_with_mcp_env(
        vec![call(&["mcp__remote__echo"]), text_reply()],
        "fake-model",
        None,
        json!({}),
        json!({"remote":http.config()}),
        &[("HTTP_PROXY", &proxy_url)],
    )
    .await;
    let created = create(&fixture, "allowAll").await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    let status: Value = fixture
        .request(Method::GET, "/v1/mcp", json!(null))
        .await
        .json()
        .await
        .unwrap();
    fixture.stop().await;
    proxy.abort();
    let _ = proxy.await;
    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "MCP must never connect to the environment proxy"
    );
    assert_eq!(status["data"][0]["connected"], true);
    assert_eq!(tool_results(&fixture), ["http result"]);
}

#[tokio::test]
async fn plan_mode_blocks_all_mcp_tools_even_readonly_hints_without_approval() {
    for policy in ["alwaysAsk", "askForExecution", "allowAll"] {
        let http = HttpMcp::start(&["echo", "content"]).await;
        let mut fixture = start(
            &http,
            vec![
                call(&["mcp__remote__echo", "mcp__second__content"]),
                text_reply(),
            ],
        )
        .await;
        let created = create(&fixture, policy).await;
        let id = &created.session.id;
        let response = fixture
            .request(
                Method::PATCH,
                &format!("/v1/sessions/{id}"),
                json!({"planMode":true}),
            )
            .await;
        assert_eq!(response.status(), 200);
        fixture.input(id).await;
        let events = complete(&fixture, id, created.event_seq).await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
        );
        let blocked: Vec<_> = events
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::ItemCompleted {
                    item:
                        canter_protocol::Item::McpToolCall {
                            server,
                            tool,
                            status: canter_protocol::ItemStatus::Blocked,
                            ..
                        },
                    ..
                } => Some((server.as_str(), tool.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(blocked, [("remote", "echo"), ("second", "content")]);
        assert!(http.state.lock().unwrap().calls.is_empty());
        assert!(
            tool_results(&fixture)
                .iter()
                .all(|result| result.contains("Plan mode"))
        );
        {
            let script = fixture.script.lock().unwrap();
            assert_eq!(script.requests[0]["tools"], script.requests[1]["tools"]);
            let mut next = script.requests[1].clone();
            next["messages"]
                .as_array_mut()
                .unwrap()
                .truncate(script.requests[0]["messages"].as_array().unwrap().len());
            assert_eq!(next, script.requests[0]);
        }
        fixture.stop().await;
    }
}
