use super::*;
use canter_model::{ApiKey, ChatCompletions};
use std::sync::Mutex as SyncMutex;

struct Fixture {
    root: tempfile::TempDir,
    app: Arc<App>,
    client: reqwest::Client,
    requests: Arc<SyncMutex<Vec<serde_json::Value>>>,
    pause_compaction: Arc<AtomicBool>,
    release_compaction: Arc<tokio::sync::Notify>,
    release_reply: Arc<tokio::sync::Notify>,
    pause_wakes: Arc<AtomicBool>,
    wake_tool: Arc<SyncMutex<Option<(String, serde_json::Value)>>>,
    stop: CancellationToken,
    servers: Vec<tokio::task::JoinHandle<()>>,
}

impl Fixture {
    async fn new() -> Self {
        Self::with_wake_limit(None).await
    }
    async fn with_wake_limit(limit: Option<u32>) -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-user-shell-")
            .tempdir_in("/tmp")
            .unwrap();
        for directory in ["home", "project", "state"] {
            std::fs::create_dir(root.path().join(directory)).unwrap();
        }
        std::fs::write(
            root.path().join("state/config.json"),
            json!({
                "base_url":"http://127.0.0.1:1/v1", "model":"fake", "api_key":"test-key",
                "context_window":1048576, "auto_wake_limit":limit.unwrap_or(10)
            })
            .to_string(),
        )
        .unwrap();
        let options = crate::config::Config::load(&root.path().join("state"))
            .unwrap()
            .options();
        let requests = Arc::new(SyncMutex::new(Vec::new()));
        let recorded = requests.clone();
        let pause_compaction = Arc::new(AtomicBool::new(false));
        let pause = pause_compaction.clone();
        let release_compaction = Arc::new(tokio::sync::Notify::new());
        let release = release_compaction.clone();
        let release_reply = Arc::new(tokio::sync::Notify::new());
        let reply = release_reply.clone();
        let pause_wakes = Arc::new(AtomicBool::new(false));
        let pause_wake = pause_wakes.clone();
        let wake_tool: Arc<SyncMutex<Option<(String, serde_json::Value)>>> = Arc::default();
        let on_wake = wake_tool.clone();
        let model_router = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(request): Json<serde_json::Value>| {
                let recorded = recorded.clone();
                let pause = pause.clone();
                let release = release.clone();
                let reply = reply.clone();
                let pause_wake = pause_wake.clone();
                let on_wake = on_wake.clone();
                async move {
                    use axum::response::IntoResponse;
                    let last = request["messages"].as_array().unwrap().last().unwrap();
                    let text = last["content"].as_str().unwrap_or_default();
                    let compact = request.get("tools").is_none_or(|tools| tools.as_array().is_some_and(Vec::is_empty));
                    let wake = text.starts_with("[Background command ") && text.contains("[Background event:");
                    let delayed_reply = text == "finish-later";
                    let wait = text == "wait" || (wake && pause_wake.load(Ordering::Relaxed)) || (compact && pause.load(Ordering::Relaxed));
                    let tool = match text {
                        "wake-background" => Some(("shell", json!({"command":"while [ ! -e release-wake ]; do sleep 0.01; done; exit 7", "background":true}))),
                        "background" => Some(("shell", json!({"command":"echo $$; exec sleep 30", "background":true,"timeoutMs":1}))),
                        "foreground" => Some(("shell", json!({"command":"echo $$; exec sleep 30"}))),
                        "wake-foreground" => Some(("shell", json!({"command":"printf ready; while [ ! -e release-wake ]; do sleep 0.01; done; exit 7"}))),
                        "background-denied" => Some(("shell", json!({"command":"touch must-not-run; exec sleep 30", "background":true}))),
                        "background-readonly" => Some(("shell", json!({"command":"pwd", "background":true}))),
                        "output" => Some(("shell_output", json!({"id":1}))),
                        "monitor-errors" => Some(("monitor", json!({"id":1,"pattern":"error"}))),
                        "monitor-warnings" => Some(("monitor", json!({"id":1,"pattern":"warning"}))),
                        "monitor-off" => Some(("monitor", json!({"id":1}))),
                        "monitor-invalid" => Some(("monitor", json!({"id":1,"pattern":"["}))),
                        "stop" => Some(("shell_stop", json!({"id":1}))),
                        "approve" => Some(("shell", json!({"command":"printf model-command > model-result.txt"}))),
                        "question" => Some(("ask_user", json!({"question":"Choose?","options":[{"label":"A","description":"A"},{"label":"B","description":"B"}]}))),
                        _ => None,
                    }.map(|(name, args)| (name.to_owned(), args))
                        .or_else(|| if wake { on_wake.lock().unwrap().clone() } else { None });
                    recorded.lock().unwrap().push(request);
                    let done = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"done\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
                    let body = if delayed_reply {
                        use futures_util::StreamExt;
                        let started = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"working\"},\"finish_reason\":null}]}\n\n";
                        axum::body::Body::from_stream(futures_util::stream::once(async move {
                            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(started.as_bytes()))
                        }).chain(futures_util::stream::once(async move {
                            reply.notified().await;
                            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(done.as_bytes()))
                        })))
                    } else if compact && wait {
                        axum::body::Body::from_stream(futures_util::stream::once(async move {
                            release.notified().await;
                            Ok::<_, std::convert::Infallible>(axum::body::Bytes::from_static(done.as_bytes()))
                        }))
                    } else if wait {
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
            lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
            home: root.path().join("state"),
            store: Arc::new(Store::new(root.path().join("state/sessions")).unwrap()),
            model: Arc::new(model),
            default_model: "fake".into(),
            models: vec!["fake".into()],
            options,
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
            release_compaction,
            release_reply,
            pause_wakes,
            wake_tool,
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

    async fn until(
        &self,
        url: &str,
        predicate: impl Fn(&SessionSnapshot) -> bool,
    ) -> SessionSnapshot {
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
                if predicate(&snapshot.session) {
                    return snapshot.session;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("session did not reach expected state")
    }

    async fn input(&self, url: &str, text: &str) {
        self.client
            .post(format!("{url}/input"))
            .json(&json!({"input":[{"type":"text","text":text}],"clientName":"test"}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
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

    async fn reopen(&self, url: &str, id: &str) {
        let session = self.app.sessions.lock().await.remove(id).unwrap();
        session.close_for_shutdown().await.unwrap();
        drop(session);
        self.client
            .post(format!("{url}/resume"))
            .json(&json!({"env":self.env()}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }

    async fn assert_background_notice(&self, url: &str, id: &str) {
        for reopened in [false, true] {
            if reopened {
                self.reopen(url, id).await;
            }
            self.say(url, "check completion").await;
            let requests = self.requests.lock().unwrap();
            let messages = requests.last().unwrap()["messages"].as_array().unwrap();
            assert_eq!(
                messages
                    .iter()
                    .filter(|message| message["content"]
                        .as_str()
                        .is_some_and(|text| text.starts_with("[Background command 1 finished:")))
                    .count(),
                1,
                "reopened={reopened}: {messages:?}"
            );
        }
    }
}

#[path = "wake_tests.rs"]
mod wake;

#[tokio::test]
async fn auto_wake_zero_stays_silent_and_preserves_event_for_user_input() {
    let fixture = Fixture::with_wake_limit(Some(0)).await;
    let url = fixture.create("allowAll").await;
    fixture.say(&url, "wake-background").await;
    std::fs::write(fixture.root.path().join("project/release-wake"), "").unwrap();
    fixture
        .until(&url, |session| !session.background_commands[0].running)
        .await;
    tokio::time::sleep(Duration::from_millis(2300)).await;
    let snapshot = fixture.idle(&url).await.session;
    assert_eq!(snapshot.turns.len(), 1);
    assert!(
        !serde_json::to_string(&snapshot)
            .unwrap()
            .contains("Auto wake paused")
    );
    assert_eq!(fixture.requests.lock().unwrap().len(), 2);
    fixture.say(&url, "check completion").await;
    let requests = fixture.requests.lock().unwrap().clone();
    let messages = requests.last().unwrap()["messages"].as_array().unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("[Background command 1 finished:")))
            .count(),
        1
    );
    fixture.close().await;
}

#[tokio::test]
async fn wake_turn_can_be_rewound_and_event_is_replayed_once() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.say(&url, "wake-background").await;
    std::fs::write(fixture.root.path().join("project/release-wake"), "").unwrap();
    let snapshot = fixture
        .until(&url, |s| {
            s.turns.len() == 2 && s.status == SessionStatus::Idle
        })
        .await;
    let wake_id = &snapshot.turns[1].id;
    let candidates: serde_json::Value = fixture
        .client
        .get(format!("{url}/rewind"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        candidates["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|candidate| candidate["turnId"] == *wake_id),
        "{candidates}"
    );
    let params = json!({"beforeTurnId":wake_id,"rewindFiles":false,"rewindConversation":true,"lastEventSeq":snapshot.last_event_seq});
    fixture
        .client
        .post(format!("{url}/rewind"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.assert_background_notice(&url, &snapshot.id).await;
    fixture.close().await;
}

#[tokio::test]
async fn monitor_matches_new_output_and_wakes_user_command() {
    let fixture = Fixture::new().await;
    let url = fixture.create("alwaysAsk").await;
    fixture.shell(&url, "printf 'error old\n'; while [ ! -e release-match ]; do sleep 0.01; done; printf 'error new\n'; exec sleep 30 &").await;
    let monitored = fixture.say(&url, "monitor-errors").await;
    assert_eq!(monitored.session.turns.len(), 2);
    std::fs::write(fixture.root.path().join("project/release-match"), "").unwrap();
    let snapshot = fixture
        .until(&url, |session| {
            session.turns.len() == 3 && session.status == SessionStatus::Idle
        })
        .await;
    let card = serde_json::to_value(&snapshot.turns[2].items[0]).unwrap();
    assert_eq!(card["type"], "backgroundEvent");
    let text = card["text"].as_str().unwrap();
    assert!(
        text.contains("error new") && text.contains("shell_output"),
        "{text}"
    );
    assert!(!text.contains("error old"), "{text}");
    fixture.close().await;
}

#[tokio::test]
async fn model_background_completion_wakes_idle_session_without_user_message() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    let started = fixture.say(&url, "wake-background").await;
    assert!(started.session.background_commands[0].running);
    std::fs::write(fixture.root.path().join("project/release-wake"), "").unwrap();
    let snapshot = fixture
        .until(&url, |session| {
            session.turns.len() == 2 && session.status == SessionStatus::Idle
        })
        .await;
    let turn = &snapshot.turns[1];
    assert!(turn.input.is_empty());
    assert!(
        !turn
            .items
            .iter()
            .any(|item| matches!(item, Item::UserMessage { .. }))
    );
    let card = serde_json::to_value(&turn.items[0]).unwrap();
    assert_eq!(card["type"], "backgroundEvent");
    assert!(card["text"].as_str().unwrap().contains("Exit code: 7"));
    assert_eq!(fixture.requests.lock().unwrap().len(), 3);
    fixture.reopen(&url, &snapshot.id).await;
    let reopened = fixture.idle(&url).await;
    assert_eq!(reopened.session.turns, snapshot.turns);
    fixture.close().await;
}

#[tokio::test]
async fn background_completion_survives_rewind_and_retraction() {
    for retract in [false, true] {
        let fixture = Fixture::new().await;
        let url = fixture.create("allowAll").await;
        let started = fixture
            .shell(
                &url,
                "while [ ! -e release ]; do sleep 0.01; done; printf final &",
            )
            .await;
        fixture.input(&url, "wait").await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while fixture.requests.lock().unwrap().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        std::fs::write(fixture.root.path().join("project/release"), "").unwrap();
        let finished = fixture
            .until(&url, |session| !session.background_commands[0].running)
            .await;
        let turn_id = &finished.turns.last().unwrap().id;
        let response: serde_json::Value = fixture
            .client
            .post(format!("{url}/interrupt"))
            .json(&json!({"turnId":turn_id,"retract":retract}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["retracted"], retract);
        fixture.idle(&url).await;
        if !retract {
            let mut params =
                json!({"beforeTurnId":turn_id,"rewindFiles":false,"rewindConversation":true});
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
        }
        fixture
            .assert_background_notice(&url, &started.session.id)
            .await;
        fixture.close().await;
    }
}

#[tokio::test]
async fn background_completion_during_compaction_survives_replacement_and_reopen() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    let started = fixture
        .shell(
            &url,
            "while [ ! -e release ]; do sleep 0.01; done; printf final &",
        )
        .await;
    fixture.pause_compaction.store(true, Ordering::Relaxed);
    fixture
        .client
        .post(format!("{url}/compact"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture.requests.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    std::fs::write(fixture.root.path().join("project/release"), "").unwrap();
    fixture
        .until(&url, |session| !session.background_commands[0].running)
        .await;
    fixture.release_compaction.notify_one();
    fixture.idle(&url).await;
    fixture
        .assert_background_notice(&url, &started.session.id)
        .await;
    fixture.close().await;
}

#[tokio::test]
async fn user_shell_background_survives_turn_and_is_visible_to_another_client() {
    let fixture = Fixture::new().await;
    let url = fixture.create("alwaysAsk").await;
    let snapshot = fixture.shell(&url, "printf ready; exec sleep 30 &  ").await;
    assert_eq!(
        snapshot.session.background_commands.len(),
        1,
        "a trailing single & must register a background command"
    );
    assert!(snapshot.session.background_commands[0].running);
    assert_eq!(snapshot.session.background_commands[0].id, 1);
    let other: SessionReadResponse = fixture
        .client
        .get(&url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        other.session.background_commands,
        snapshot.session.background_commands
    );
    assert!(fixture.requests.lock().unwrap().is_empty());
    fixture.close().await;
}

#[tokio::test]
async fn background_completion_appends_history_without_waking_the_model() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    fixture.say(&url, "before").await;
    let previous = fixture.requests.lock().unwrap().last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    let started = fixture
        .shell(
            &url,
            "printf initial; while [ ! -e release ]; do sleep 0.01; done; printf final; exit 7 &",
        )
        .await;
    let history_path = fixture
        .root
        .path()
        .join("state/sessions")
        .join(&started.session.id)
        .join("model-history.jsonl");
    let history_before = std::fs::read(&history_path).unwrap();
    let count = fixture.requests.lock().unwrap().len();
    std::fs::write(fixture.root.path().join("project/release"), "").unwrap();
    fixture
        .until(&url, |session| {
            session.background_commands[0].exit_code == Some(7)
        })
        .await;
    assert_eq!(
        fixture.requests.lock().unwrap().len(),
        count,
        "completion must not start a model turn"
    );
    assert!(
        std::fs::read(&history_path)
            .unwrap()
            .starts_with(&history_before)
    );
    fixture.say(&url, "output").await;
    let messages = fixture.requests.lock().unwrap().last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(&messages[..previous.len()], &previous);
    assert!(
        messages.iter().any(|message| message["content"]
            .as_str()
            .is_some_and(|text| text.contains("[Background command 1 finished:")
                && text.contains("Exit code: 7")
                && text.contains("shell_output")))
    );
    assert!(messages.iter().any(|message| {
        message["role"] == "tool"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.ends_with("final"))
    }));
    fixture.close().await;
}

#[tokio::test]
async fn restarted_background_tools_report_unavailable_output_instead_of_unknown() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    let started = fixture.say(&url, "background").await;
    fixture
        .app
        .sessions
        .lock()
        .await
        .remove(&started.session.id);
    tokio::task::yield_now().await;
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
    assert!(!resumed.session.background_commands[0].running);
    for tool in ["output", "stop"] {
        let snapshot = fixture.say(&url, tool).await;
        let result = snapshot
            .session
            .turns
            .last()
            .unwrap()
            .items
            .iter()
            .find_map(|item| match item {
                Item::BackgroundCommandAction { result, status, .. } => Some((result, status)),
                _ => None,
            })
            .unwrap();
        assert_eq!(*result.1, ItemStatus::Completed);
        assert!(
            result
                .0
                .contains("Output unavailable after server restart."),
            "{}",
            result.0
        );
    }
    let messages = fixture.requests.lock().unwrap().last().unwrap()["messages"].clone();
    let notice = messages
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_str())
        .find(|text| text.starts_with("[Background command 1 finished:"))
        .unwrap();
    assert!(notice.contains("Output unavailable after server restart."));
    assert!(!notice.contains("Use shell_output"));
    fixture.close().await;
}

#[tokio::test]
async fn model_can_start_read_stop_and_shutdown_stops_all_groups() {
    let fixture = Fixture::new().await;
    let url = fixture.create("allowAll").await;
    let started = fixture.say(&url, "background").await;
    assert!(started.session.background_commands[0].running);
    fixture.say(&url, "output").await;
    let stopped = fixture.say(&url, "stop").await;
    assert!(!stopped.session.background_commands[0].running);
    fixture.say(&url, "background").await;
    let output: BackgroundCommandOutput = fixture
        .client
        .get(format!("{url}/backgroundCommands/2"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pid: i32 = output.output.trim().parse().unwrap();
    let session = fixture
        .app
        .sessions
        .lock()
        .await
        .get(&started.session.id)
        .unwrap()
        .clone();
    session.close_for_shutdown().await.unwrap();
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH)
    );
    fixture
        .app
        .sessions
        .lock()
        .await
        .remove(&started.session.id);
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
    assert!(
        resumed
            .session
            .background_commands
            .iter()
            .all(|command| !command.running)
    );
    fixture.say(&url, "background-readonly").await;
    assert_eq!(
        fixture
            .idle(&url)
            .await
            .session
            .background_commands
            .last()
            .unwrap()
            .id,
        3
    );
    fixture.close().await;
}

#[tokio::test]
async fn both_user_and_model_foreground_shells_can_be_backgrounded_and_stopped_over_http() {
    let fixture = Fixture::new().await;
    for model in [false, true] {
        let url = fixture.create("allowAll").await;
        if model {
            fixture.input(&url, "foreground").await;
        } else {
            fixture
                .client
                .post(format!("{url}/shellCommand"))
                .json(&json!({"command":"echo $$; exec sleep 30"}))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        let snapshot = fixture.until(&url, |session| session.turns.last().is_some_and(|turn| turn.items.iter().any(|item| matches!(item, Item::CommandExecution { aggregated_output, .. } if !aggregated_output.is_empty())))).await;
        let item = snapshot
            .turns
            .last()
            .unwrap()
            .items
            .iter()
            .find(|item| matches!(item, Item::CommandExecution { .. }))
            .unwrap();
        let moved: ShellBackgroundResponse = fixture
            .client
            .post(format!("{url}/shellBackground"))
            .json(&json!({"itemId":item.id().unwrap()}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(moved.command_id, Some(1));
        let idle = fixture.idle(&url).await;
        assert!(idle.session.background_commands[0].running);
        let output: BackgroundCommandOutput = fixture
            .client
            .get(format!("{url}/backgroundCommands/1"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let pid: i32 = output.output.trim().parse().unwrap();
        let other = fixture.create("allowAll").await;
        fixture.shell(&other, "printf elsewhere").await;
        let resumed: SessionResumeResponse = fixture
            .client
            .post(format!("{url}/resume"))
            .json(&json!({"env":fixture.env()}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(resumed.session.background_commands[0].running);
        let stopped: BackgroundCommandListResponse = fixture
            .client
            .post(format!("{url}/backgroundCommands/stop"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(!stopped.data[0].running);
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn background_shell_is_not_started_after_decline_or_plan_mode_block() {
    let fixture = Fixture::new().await;
    let url = fixture.create("alwaysAsk").await;
    fixture.input(&url, "background-denied").await;
    let pending = fixture
        .until(&url, |session| !session.pending_requests.is_empty())
        .await;
    assert!(pending.background_commands.is_empty());
    let request = &pending.pending_requests[0];
    fixture
        .client
        .post(format!("{url}/requests/{}", request.id))
        .json(&json!({"decision":"decline","clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(
        fixture
            .idle(&url)
            .await
            .session
            .background_commands
            .is_empty()
    );
    assert!(!fixture.root.path().join("project/must-not-run").exists());
    fixture
        .client
        .patch(&url)
        .json(&json!({"planMode":true,"approvalPolicy":"allowAll"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let blocked = fixture.say(&url, "background-denied").await;
    assert!(blocked.session.background_commands.is_empty());
    assert!(
        blocked
            .session
            .turns
            .last()
            .unwrap()
            .items
            .iter()
            .any(|item| matches!(
                item,
                Item::CommandExecution {
                    status: ItemStatus::Blocked,
                    ..
                }
            ))
    );
    let readonly = fixture.say(&url, "background-readonly").await;
    assert_eq!(readonly.session.background_commands[0].exit_code, Some(0));
    fixture.say(&url, "output").await;
    fixture.say(&url, "stop").await;
    fixture.close().await;
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
