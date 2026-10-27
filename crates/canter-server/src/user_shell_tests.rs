use super::*;
use canter_model::{ApiKey, ChatCompletions};
use std::sync::Mutex as SyncMutex;

struct Fixture {
    root: tempfile::TempDir,
    app: Arc<App>,
    client: reqwest::Client,
    requests: Arc<SyncMutex<Vec<serde_json::Value>>>,
    pause_compaction: Arc<AtomicBool>,
    stop: CancellationToken,
    servers: Vec<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-user-shell-")
            .tempdir_in("/tmp")
            .unwrap();
        for directory in ["home", "project", "state"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
        }
        let requests = Arc::new(SyncMutex::new(Vec::new()));
        let recorded = requests.clone();
        let pause_compaction = Arc::new(AtomicBool::new(false));
        let pause = pause_compaction.clone();
        let model_router = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(request): Json<serde_json::Value>| {
                let recorded = recorded.clone();
                let pause = pause.clone();
                async move {
                    use axum::response::IntoResponse;
                    let last = request["messages"].as_array().unwrap().last().unwrap();
                    let text = last["content"].as_str().unwrap_or_default();
                    let compact = request.get("tools").is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty));
                    let wait = text == "wait" || (compact && pause.load(Ordering::Relaxed));
                    let tool = match text {
                        "approve" => Some(("shell", json!({"command":"printf model-command > model-result.txt"}))),
                        "question" => Some(("ask_user", json!({"question":"Choose?","options":[{"label":"A","description":"A"},{"label":"B","description":"B"}]}))),
                        _ => None,
                    };
                    recorded.lock().unwrap().push(request);
                    let body = if wait {
                        axum::body::Body::from_stream(futures_util::stream::pending::<Result<axum::body::Bytes, std::convert::Infallible>>())
                    } else if let Some((name, args)) = tool {
                        axum::body::Body::from(format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"model-call","type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})))
                    } else {
                        axum::body::Body::from("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
                    };
                    ([("content-type", "text/event-stream")], body).into_response()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let model = ChatCompletions::new(
            &format!("http://{}/v1", listener.local_addr().unwrap()),
            ApiKey::new("test-key"),
        )
        .unwrap();
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let model_server = tokio::spawn(async move {
            axum::serve(listener, model_router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let app = Arc::new(App {
            mcp: Default::default(),
            home: root.path().join("state"),
            store: Arc::new(Store::new(root.path().join("state/sessions")).unwrap()),
            model: Arc::new(model),
            default_model: "fake".into(),
            models: vec!["fake".into()],
            options: CoreOptions::default(),
            sessions: Default::default(),
            registry_changed: Default::default(),
            draining: Default::default(),
            stopped: CancellationToken::new(),
        });
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let shutdown = stop.clone();
        let server_app = app.clone();
        let server = tokio::spawn(async move {
            axum::serve(listener, router(server_app))
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .unix_socket(socket)
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            root,
            app,
            client,
            requests,
            pause_compaction,
            stop,
            servers: vec![server, model_server],
        }
    }

    fn env(&self) -> serde_json::Value {
        json!({"HOME":self.root.path().join("home"),
            "CANTER_HOME":self.root.path().join("state"), "SHELL_TEST":"session-env",
            "PATH":"/usr/bin:/bin"})
    }

    async fn create(&self, policy: &str) -> String {
        let response: SessionCreateResponse = self
            .client
            .post("http://localhost/v1/sessions")
            .json(
                &json!({"cwd":self.root.path().join("project"), "env":self.env(),
                "model":"fake", "approvalPolicy":policy, "clientName":"test"}),
            )
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        format!("http://localhost/v1/sessions/{}", response.session.id)
    }

    async fn idle(&self, url: &str) -> SessionReadResponse {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot: SessionReadResponse = self
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
                assert!(
                    snapshot.session.pending_requests.is_empty(),
                    "user commands never ask approval"
                );
                if snapshot.session.status == SessionStatus::Idle {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    async fn say(&self, url: &str, text: &str) -> SessionReadResponse {
        self.client
            .post(format!("{url}/input"))
            .json(&json!({"input":[{"type":"text","text":text}],"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        self.idle(url).await
    }

    async fn shell(&self, url: &str, command: &str) -> SessionReadResponse {
        self.client
            .post(format!("{url}/shellCommand"))
            .json(&json!({"command":command}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        self.idle(url).await
    }

    async fn close(self) {
        for session in self.app.sessions.lock().await.values() {
            session.close_for_shutdown().await.unwrap();
        }
        self.stop.cancel();
        for server in self.servers {
            server.await.unwrap();
        }
    }
}

#[tokio::test]
async fn user_shell_runs_without_approval_or_model_and_appends_next_request() {
    let fixture = Fixture::new().await;
    for policy in ["alwaysAsk", "askForExecution", "allowAll"] {
        let url = fixture.create(policy).await;
        fixture.say(&url, "before").await;
        let previous = fixture.requests.lock().unwrap().last().unwrap()["messages"].clone();
        let count = fixture.requests.lock().unwrap().len();
        let command = "printf '%s' \"$SHELL_TEST\" | cat > result.txt && cat result.txt && pwd";
        let response = fixture
            .client
            .post(format!("{url}/shellCommand"))
            .json(&json!({"command":command}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "shellCommand must run without the model"
        );
        let response: serde_json::Value = response.json().await.unwrap();
        let snapshot = fixture.idle(&url).await;
        assert_eq!(fixture.requests.lock().unwrap().len(), count);
        let turn = snapshot.session.turns.last().unwrap();
        assert_eq!(turn.status, TurnStatus::Completed);
        assert_eq!(response["turnId"], turn.id);
        let item = serde_json::to_value(&turn.items[0]).unwrap();
        assert_eq!(item["source"], "userShell");
        assert_eq!(item["command"], command);
        assert_eq!(item["exitCode"], 0);
        assert!(
            item["aggregatedOutput"]
                .as_str()
                .unwrap()
                .contains("session-env")
        );
        assert!(
            item["aggregatedOutput"]
                .as_str()
                .unwrap()
                .contains("/project")
        );
        fixture.say(&url, "after").await;
        let requests = fixture.requests.lock().unwrap();
        let messages = requests.last().unwrap()["messages"].as_array().unwrap();
        let previous = previous.as_array().unwrap();
        assert_eq!(&messages[..previous.len()], previous);
        let history = &messages[messages.len() - 2];
        assert_eq!(history["role"], "user");
        let text = history["content"].as_str().unwrap();
        assert!(text.starts_with(&format!("<user_shell_command>\n<command>\n{command}\n</command>\n<result>\nExit code: 0\nDuration: ")));
        assert!(text.contains(" seconds\nOutput:\nsession-env"));
        assert!(text.ends_with("\n</result>\n</user_shell_command>"));
    }
    fixture.close().await;
}

#[tokio::test]
async fn user_shell_resumes_rewinds_and_compacts_as_conversation() {
    let fixture = Fixture::new().await;
    let url = fixture.create("alwaysAsk").await;
    fixture.say(&url, "keep this").await;
    let snapshot = fixture
        .shell(&url, "printf shell-marker > changed.txt && cat changed.txt")
        .await;
    let turn = snapshot.session.turns.last().unwrap();
    let turn_id = turn.id.clone();
    let directory = fixture
        .root
        .path()
        .join("state/sessions")
        .join(&snapshot.session.id);
    let old_history = std::fs::read(directory.join("model-history.jsonl")).unwrap();
    let session = fixture
        .app
        .sessions
        .lock()
        .await
        .remove(&snapshot.session.id)
        .unwrap();
    session.close_for_shutdown().await.unwrap();
    drop(session);
    let resumed: SessionResumeResponse = fixture
        .client
        .post(format!("{url}/resume"))
        .json(&json!({"env":fixture.env()}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed.session.turns, snapshot.session.turns);
    fixture.say(&url, "after resume").await;
    assert!(
        fixture.requests.lock().unwrap().last().unwrap()["messages"]
            .to_string()
            .contains("shell-marker")
    );
    let list: RewindListResponse = fixture
        .client
        .get(format!("{url}/rewind"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        list.data
            .iter()
            .any(|turn| turn.turn_id == turn_id && turn.file_count == 0),
        "user command must be a rewind candidate"
    );
    let mut params = json!({"beforeTurnId":turn_id,"rewindFiles":false,"rewindConversation":true});
    let preview: RewindPreviewResponse = fixture
        .client
        .post(format!("{url}/rewind/preview"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(preview.files.is_empty());
    params["lastEventSeq"] = json!(preview.last_event_seq);
    fixture
        .client
        .post(format!("{url}/rewind"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.say(&url, "after rewind").await;
    let messages = fixture.requests.lock().unwrap().last().unwrap()["messages"].to_string();
    assert!(!messages.contains("shell-marker"));
    assert!(messages.contains("keep this"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("project/changed.txt")).unwrap(),
        "shell-marker"
    );
    assert!(
        std::fs::read(directory.join("model-history.jsonl"))
            .unwrap()
            .starts_with(&old_history)
    );
    fixture.shell(&url, "printf compact-shell-marker").await;
    fixture
        .client
        .post(format!("{url}/compact"))
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let compacted = fixture.idle(&url).await;
    assert_eq!(
        compacted.session.turns.last().unwrap().status,
        TurnStatus::Completed
    );
    assert!(
        fixture.requests.lock().unwrap().last().unwrap()["messages"]
            .to_string()
            .contains("compact-shell-marker")
    );
    fixture.close().await;
}

#[tokio::test]
async fn user_shell_rejects_work_approval_questions_and_compaction() {
    let fixture = Fixture::new().await;
    for mode in ["wait", "approve", "question", "compact"] {
        let url = fixture.create("alwaysAsk").await;
        let request_count = fixture.requests.lock().unwrap().len();
        if mode == "compact" {
            fixture.shell(&url, "printf context").await;
            fixture.pause_compaction.store(true, Ordering::Relaxed);
            fixture
                .client
                .post(format!("{url}/compact"))
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        } else {
            fixture
                .client
                .post(format!("{url}/input"))
                .json(&json!({"input":[{"type":"text","text":mode}],"clientName":"test"}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        let waiting = matches!(mode, "approve" | "question");
        let snapshot = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let snapshot: SessionReadResponse = fixture
                    .client
                    .get(&url)
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if fixture.requests.lock().unwrap().len() > request_count
                    && snapshot.session.status
                        == if waiting {
                            SessionStatus::WaitingApproval
                        } else {
                            SessionStatus::Working
                        }
                {
                    return snapshot;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{mode}: expected busy state"));
        let response = fixture
            .client
            .post(format!("{url}/shellCommand"))
            .json(&json!({"command":"printf wrong > should-not-run"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{mode}");
        assert_eq!(
            response.json::<ErrorResponse>().await.unwrap().code,
            ErrorCode::Conflict
        );
        assert!(!fixture.root.path().join("project/should-not-run").exists());
        fixture
            .client
            .post(format!("{url}/interrupt"))
            .json(&json!({"turnId":snapshot.session.turns.last().unwrap().id}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        fixture.idle(&url).await;
    }
    fixture.close().await;
}

#[tokio::test]
async fn user_shell_interrupt_stops_process_and_preserves_command_history() {
    let fixture = Fixture::new().await;
    let url = fixture.create("alwaysAsk").await;
    let response: serde_json::Value = fixture
        .client
        .post(format!("{url}/shellCommand"))
        .json(&json!({"command":"printf '%s\\n' \"$$\"; exec sleep 30"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot: SessionReadResponse = fixture
                .client
                .get(&url)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if let Some(Item::CommandExecution {
                aggregated_output, ..
            }) = snapshot
                .session
                .turns
                .last()
                .and_then(|turn| turn.items.first())
                && let Ok(pid) = aggregated_output.trim().parse::<i32>()
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let interrupted: SessionInterruptResponse = fixture
        .client
        .post(format!("{url}/interrupt"))
        .json(&json!({"turnId":response["turnId"],"retract":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(interrupted.retracted, Some(false));
    let snapshot = fixture.idle(&url).await;
    assert_eq!(
        snapshot.session.turns.last().unwrap().status,
        TurnStatus::Interrupted
    );
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
    fixture.say(&url, "after interrupt").await;
    let history = fixture.requests.lock().unwrap().last().unwrap()["messages"].to_string();
    assert!(history.contains("<user_shell_command>"));
    assert!(history.contains("Command cancelled"));
    fixture.close().await;
}
