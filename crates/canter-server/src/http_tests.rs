use super::*;
use canter_model::{FinishReason, ModelEvent, ModelStream, Request};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

#[path = "rewind_tests.rs"]
mod rewind;

#[path = "user_shell_tests.rs"]
mod user_shell;

#[path = "plan_tests.rs"]
mod plan;

#[path = "goal_tests.rs"]
mod goal;

// A failed model task is a real Core close-error path, without modifying Core or the store.
struct PanicOnce {
    panicked: AtomicBool,
    reached: Notify,
}
impl Model for PanicOnce {
    fn stream(&self, _request: Request, _cancel: CancellationToken) -> ModelStream<'_> {
        if !self.panicked.swap(true, Ordering::Relaxed) {
            self.reached.notify_one();
            panic!("test-only failed model task");
        }
        Box::pin(futures_util::stream::iter([
            Ok(ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "recovered".into(),
            }),
            Ok(ModelEvent::Finished(FinishReason::Stop)),
        ]))
    }
}

#[tokio::test]
async fn failed_close_can_be_resumed_through_http() {
    let root = tempfile::Builder::new()
        .prefix("canter-close-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("state");
    let directory = home.clone();
    let store = tokio::task::spawn_blocking(move || Store::new(directory.join("sessions")))
        .await
        .unwrap()
        .unwrap();
    let model = Arc::new(PanicOnce {
        panicked: AtomicBool::new(false),
        reached: Notify::new(),
    });
    let app = Arc::new(App {
        mcp: Default::default(),
        lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
        home,
        store: Arc::new(store),
        model: model.clone(),
        default_model: "fake-model".into(),
        models: vec!["fake-model".into()],
        options: CoreOptions::default(),
        sessions: Default::default(),
        registry_changed: Default::default(),
        draining: Default::default(),
        stopped: CancellationToken::new(),
    });
    let socket = root.path().join("server.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(app))
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let created = client.post("http://localhost/v1/sessions").json(&json!({
        "cwd":root.path(), "env":{}, "model":"fake-model", "approvalPolicy":"allowAll", "clientName":"test"
    })).send().await.unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let created: SessionCreateResponse = created.json().await.unwrap();
    let url = format!("http://localhost/v1/sessions/{}", created.session.id);
    let input = json!({"input":[{"type":"text","text":"hello"}], "clientName":"test"});
    assert_eq!(
        client
            .post(format!("{url}/input"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(5), model.reached.notified())
        .await
        .unwrap();
    let failed = client.delete(&url).send().await.unwrap();
    assert_eq!(failed.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        failed.json::<ErrorResponse>().await.unwrap().code,
        ErrorCode::Internal
    );
    let resumed = client
        .post(format!("{url}/resume"))
        .json(&json!({"env":{}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), StatusCode::OK);
    let resumed: SessionResumeResponse = resumed.json().await.unwrap();
    assert_eq!(
        resumed.session.status,
        SessionStatus::Idle,
        "resume must reopen the failed session instead of returning its stale snapshot"
    );
    assert_eq!(resumed.session.turns[0].status, TurnStatus::Interrupted);
    assert_eq!(
        client
            .post(format!("{url}/input"))
            .json(&input)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot: SessionReadResponse =
                client.get(&url).send().await.unwrap().json().await.unwrap();
            if snapshot.session.status == SessionStatus::Idle {
                assert_eq!(
                    snapshot.session.turns.last().unwrap().status,
                    TurnStatus::Completed
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        client.delete(url).send().await.unwrap().status(),
        StatusCode::OK
    );
    stop.cancel();
    server.await.unwrap();
}

#[tokio::test]
async fn context_report_is_read_only_and_describes_the_next_request() {
    use canter_model::{Message, ToolCall};
    use std::sync::Mutex;
    struct Replay(Mutex<Vec<Request>>);
    impl Model for Replay {
        fn stream(&self, request: Request, _: CancellationToken) -> ModelStream<'_> {
            let compact = request.tools.is_empty();
            let needs_tools = matches!(request.messages.last(), Some(Message::User { text }) if text == "inspect");
            self.0.lock().unwrap().push(request);
            let events = if needs_tools {
                vec![
                    ModelEvent::ToolCall(ToolCall {
                        id: "read".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"file.txt"}"#.into(),
                    }),
                    ModelEvent::ToolCall(ToolCall {
                        id: "write".into(),
                        name: "write".into(),
                        arguments: r#"{"path":"file.txt","content":"changed"}"#.into(),
                    }),
                    ModelEvent::Finished(FinishReason::ToolCalls),
                ]
            } else {
                vec![
                    ModelEvent::TextDelta {
                        item_id: "text".into(),
                        delta: if compact { "saved summary" } else { "done" }.into(),
                    },
                    ModelEvent::Usage(canter_model::Usage {
                        input_tokens: 1234,
                        ..Default::default()
                    }),
                    ModelEvent::Finished(FinishReason::Stop),
                ]
            };
            Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
        }
    }
    let root = tempfile::Builder::new()
        .prefix("canter-context-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = root.path().join("project");
    let cwd = project.join("src");
    let home = root.path().join("home");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir_all(project.join(".git")).unwrap();
    std::fs::write(project.join("AGENTS.md"), "root instructions").unwrap();
    std::fs::create_dir_all(home.join(".agents/skills/demo")).unwrap();
    std::fs::create_dir_all(root.path().join("state")).unwrap();
    std::fs::write(root.path().join("state/AGENTS.md"), "global instructions").unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "project instructions").unwrap();
    std::fs::write(
        home.join(".agents/skills/demo/SKILL.md"),
        "---\nname: demo\ndescription: demo skill\n---\nbody",
    )
    .unwrap();
    std::fs::write(cwd.join("file.txt"), "original").unwrap();
    let model = Arc::new(Replay(Mutex::new(Vec::new())));
    let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
    let app = Arc::new(App {
        mcp: Default::default(),
        lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
        home: root.path().join("state"),
        store,
        model: model.clone(),
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
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(app))
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let created: SessionCreateResponse = client.post("http://localhost/v1/sessions").json(&json!({"cwd":cwd,"env":{"HOME":home,"CANTER_HOME":root.path().join("state")},"model":"fake","approvalPolicy":"allowAll","clientName":"test"})).send().await.unwrap().json().await.unwrap();
    let url = format!("http://localhost/v1/sessions/{}", created.session.id);
    let response = client.get(format!("{url}/context")).send().await.unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "read-only context endpoint must exist"
    );
    let initial: serde_json::Value = response.json().await.unwrap();
    assert!(initial["contextTokens"].is_null());
    assert!(initial["compactionTokens"].is_null());
    assert_eq!(initial["skillCount"], 1);
    assert_eq!(initial["messages"]["count"], 0);
    let sections = initial["prefix"].as_array().unwrap();
    assert_eq!(
        sections
            .iter()
            .map(|s| s["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "system",
            "globalInstructions",
            "skills",
            "projectInstructions",
            "projectInstructions",
            "environment"
        ]
    );
    assert_eq!(
        sections[1]["path"],
        root.path().join("state/AGENTS.md").to_str().unwrap()
    );
    assert_eq!(
        sections[3]["path"],
        project.join("AGENTS.md").to_str().unwrap()
    );
    assert_eq!(sections[4]["path"], cwd.join("AGENTS.md").to_str().unwrap());
    for (endpoint, body) in [
        (
            "input",
            json!({"input":[{"type":"text","text":"inspect"}],"clientName":"test"}),
        ),
        ("compact", json!({})),
        (
            "input",
            json!({"input":[{"type":"text","text":"inspect"}],"clientName":"test"}),
        ),
    ] {
        assert_eq!(
            client
                .post(format!("{url}/{endpoint}"))
                .json(&body)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let snapshot: SessionReadResponse =
                    client.get(&url).send().await.unwrap().json().await.unwrap();
                if snapshot.session.status == SessionStatus::Idle {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    let before: SessionReadResponse = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert!(
        before
            .session
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .any(|i| matches!(
                i,
                Item::FileRead {
                    status: ItemStatus::Completed,
                    ..
                }
            ))
    );
    assert!(
        before
            .session
            .turns
            .iter()
            .flat_map(|t| &t.items)
            .any(|i| matches!(
                i,
                Item::FileChange {
                    status: ItemStatus::Completed,
                    ..
                }
            ))
    );
    let report: serde_json::Value = client
        .get(format!("{url}/context"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let after: SessionReadResponse = client.get(&url).send().await.unwrap().json().await.unwrap();
    assert_eq!(before, after);
    assert_eq!(report["contextTokens"], 1234);
    assert!(report["compactionTokens"].as_u64().unwrap() > 0);
    for category in ["user", "assistant", "toolResults"] {
        assert!(report["messages"][category].as_u64().unwrap() > 0);
    }
    assert_eq!(report["messages"]["images"], 0);
    // The next request must retain the exact prior request, then its reply and new input.
    let previous = model.0.lock().unwrap().last().unwrap().clone();
    client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":"next"}],"clientName":"test"}))
        .send()
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if model.0.lock().unwrap().last().unwrap().messages.len() > previous.messages.len() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let next = model.0.lock().unwrap().last().unwrap().clone();
    let mut expected_request = previous.clone();
    expected_request.messages.push(Message::Assistant {
        content: vec![canter_model::AssistantContent::Text {
            text: "done".into(),
        }],
    });
    expected_request.messages.push(Message::User {
        text: "next".into(),
    });
    assert_eq!(next, expected_request);
    let estimate_message =
        |message: &Message| (serde_json::to_string(message).unwrap().len() as u64).div_ceil(3) + 8;
    let expected = next.messages[..next.messages.len() - 1]
        .iter()
        .map(estimate_message)
        .sum::<u64>()
        + (serde_json::to_string(&next.tools).unwrap().len() as u64).div_ceil(3);
    assert_eq!(report["estimatedTokens"], expected);
    let prefix_tokens = report["prefix"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["tokens"].as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(prefix_tokens, estimate_message(&next.messages[0]));
    let Message::System { text } = &next.messages[0] else {
        panic!("system prefix")
    };
    let boundaries = [
        text.find("\n\n<global_instructions path=\"").unwrap(),
        text.find("\n\n<skills>\n").unwrap(),
        text.find(&format!(
            "\n\n<project_instructions path=\"{}\">",
            project.join("AGENTS.md").display()
        ))
        .unwrap(),
        text.find(&format!(
            "\n\n<project_instructions path=\"{}\">",
            cwd.join("AGENTS.md").display()
        ))
        .unwrap(),
        text.find("\n\n<environment>\n").unwrap(),
        text.len(),
    ];
    let mut prior = 0;
    for (section, end) in report["prefix"].as_array().unwrap().iter().zip(boundaries) {
        let cumulative = estimate_message(&Message::System {
            text: text[..end].into(),
        });
        assert_eq!(section["tokens"], cumulative - prior);
        prior = cumulative;
    }
    assert_eq!(report["toolCount"], next.tools.len());
    let summary = Message::User {
        text: "[Context summary]\nsaved summary".into(),
    };
    assert_eq!(report["compactionTokens"], estimate_message(&summary));
    let mut user = 0;
    let mut assistant = 0;
    let mut tools = 0;
    for message in &next.messages[1..next.messages.len() - 1] {
        match message {
            Message::User { .. } if message != &summary => user += estimate_message(message),
            Message::Assistant { .. } => assistant += estimate_message(message),
            Message::ToolResult { .. } => tools += estimate_message(message),
            _ => {}
        }
    }
    assert_eq!(report["messages"]["user"], user);
    assert_eq!(report["messages"]["assistant"], assistant);
    assert_eq!(report["messages"]["toolResults"], tools);
    assert_eq!(
        report["freeTokens"].as_u64().unwrap() + expected,
        CoreOptions::default().context_window
    );
    assert_eq!(
        report["messages"]["tokens"].as_u64().unwrap(),
        ["user", "assistant", "toolResults", "images"]
            .iter()
            .map(|key| report["messages"][key].as_u64().unwrap())
            .sum::<u64>()
    );
    client.delete(&url).send().await.unwrap();
    stop.cancel();
    server.await.unwrap();
}

struct BtwModel(std::sync::Mutex<Vec<Request>>);
impl Model for BtwModel {
    fn stream(&self, request: Request, _: CancellationToken) -> ModelStream<'_> {
        use canter_model::{Message, ToolCall, Usage};
        let question = match request.messages.last() {
            Some(Message::User { text }) => text.clone(),
            _ => String::new(),
        };
        self.0.lock().unwrap().push(request);
        if question == "btw-held" || question.ends_with("handoff-held") {
            return Box::pin(async_stream::stream! {
                yield Ok(ModelEvent::Usage(Usage { input_tokens: 1000, cache_read_tokens: Some(800), output_tokens: 100, total_tokens: 1100, ..Default::default() }));
                yield Ok(ModelEvent::TextDelta { item_id: "text".into(), delta: "held answer".into() });
                std::future::pending::<()>().await;
            });
        }
        let events = if question == "tool-main" {
            vec![
                ModelEvent::ToolCall(ToolCall {
                    id: "read-first".into(), name: "read".into(),
                    arguments: r#"{"path":"file.txt"}"#.into(),
                }),
                ModelEvent::ToolCall(ToolCall {
                    id: "wait-for-user".into(), name: "ask_user".into(),
                    arguments: r#"{"question":"Continue?","options":[{"label":"yes","description":"continue"},{"label":"no","description":"stop"}]}"#.into(),
                }),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ]
        } else if question.starts_with("btw-only")
            || question.starts_with("Write a handoff note for a new session")
        {
            vec![
                ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: "private **btw answer**".into(),
                },
                ModelEvent::Usage(Usage {
                    input_tokens: 1000,
                    cache_read_tokens: Some(800),
                    output_tokens: 100,
                    total_tokens: 1100,
                    ..Default::default()
                }),
                ModelEvent::ToolCall(ToolCall {
                    id: "must-not-run".into(),
                    name: "write".into(),
                    arguments: r#"{"path":"btw-tool.txt","content":"wrong"}"#.into(),
                }),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ]
        } else {
            vec![
                ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: "main answer".into(),
                },
                ModelEvent::Usage(Usage {
                    input_tokens: 100,
                    cache_read_tokens: Some(80),
                    output_tokens: 20,
                    total_tokens: 120,
                    ..Default::default()
                }),
                ModelEvent::Finished(FinishReason::Stop),
            ]
        };
        Box::pin(futures_util::stream::iter(events.into_iter().map(Ok)))
    }
}

#[tokio::test]
async fn handoff_appends_instructions_without_changing_history_and_charges_source() {
    use canter_model::Message;
    let test = BtwTest::new().await;
    let control = test.create().await;
    let source = test.create().await;
    for id in [&control, &source] {
        test.input(id, "hello").await;
        test.wait_status(id, SessionStatus::Idle).await;
    }
    test.input(&control, "next").await;
    test.wait_status(&control, SessionStatus::Idle).await;
    let baseline = test.model.0.lock().unwrap().last().unwrap().clone();
    let history = test
        .app
        .store
        .model_history::<serde_json::Value>(&source)
        .unwrap();
    for direction in [None, Some("接着实现测试")] {
        let before = test.wait_status(&source, SessionStatus::Idle).await;
        let response = test
            .client
            .post(format!("{}/handoff", test.url(&source)))
            .json(&direction.map_or(json!({}), |direction| json!({"direction":direction})))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "handoff must stream a private note"
        );
        let answer = response.text().await.unwrap();
        assert!(answer.contains("private **btw answer**"));
        assert!(answer.contains("completed"));
        let request = test.model.0.lock().unwrap().last().unwrap().clone();
        let Message::User { text } = request.messages.last().unwrap() else {
            panic!("handoff instructions");
        };
        assert!(text.starts_with("Write a handoff note for a new session"));
        for required in [
            "\"## Task\"",
            "## Goal",
            "## User's words",
            "## Constraints and preferences",
            "## Decisions",
            "## Files",
            "## Commands and results",
            "## Where it stopped",
            "## Open issues",
            "## Next steps",
            "the language the user mostly writes in",
            "only the note",
        ] {
            assert!(text.contains(required), "missing {required}: {text}");
        }
        assert!(text.ends_with(direction.unwrap_or("Continue the current task.")));
        let mut expected = baseline.clone();
        *expected.messages.last_mut().unwrap() = Message::User { text: text.clone() };
        assert_eq!(
            request, expected,
            "keep every part of the main request before the final user message"
        );
        assert_eq!(
            test.app
                .store
                .model_history::<serde_json::Value>(&source)
                .unwrap(),
            history
        );
        let events = test
            .app
            .store
            .events_after(&source, before.last_event_seq)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0].payload,
            EventPayload::SessionBtwUsage { .. }
        ));
        let after = test.wait_status(&source, SessionStatus::Idle).await;
        assert_eq!(after.turns, before.turns);
        let before_usage = before.token_usage.unwrap();
        let after_usage = after.token_usage.unwrap();
        assert_eq!(after_usage.context_tokens, before_usage.context_tokens);
        assert_eq!(
            after_usage.total.total_tokens,
            before_usage.total.total_tokens + 1100
        );
        assert!(
            (after_usage.cost.unwrap().usd - before_usage.cost.unwrap().usd - 0.0001848).abs()
                < 1e-10
        );
        assert!(!test.root.path().join("btw-tool.txt").exists());
    }
    test.finish().await;
}

struct BtwTest {
    root: tempfile::TempDir,
    app: Arc<App>,
    model: Arc<BtwModel>,
    client: reqwest::Client,
    stop: CancellationToken,
    server: tokio::task::JoinHandle<()>,
}

#[tokio::test]
async fn handoff_requires_idle_conversation_and_excludes_concurrent_work() {
    let test = BtwTest::new().await;
    let empty = test.create().await;
    let working = test.create().await;
    test.input(&working, "btw-held").await;
    test.wait_status(&working, SessionStatus::Working).await;
    let waiting = test.create().await;
    test.input(&waiting, "tool-main").await;
    test.wait_status(&waiting, SessionStatus::WaitingApproval)
        .await;
    let source = test.create().await;
    test.input(&source, "hello").await;
    test.wait_status(&source, SessionStatus::Idle).await;
    let mut btw = test
        .client
        .post(format!("{}/btw", test.url(&source)))
        .json(&json!({"question":"btw-held"}))
        .send()
        .await
        .unwrap();
    while !String::from_utf8_lossy(&btw.chunk().await.unwrap().unwrap()).contains("held answer") {}
    let count = test.model.0.lock().unwrap().len();
    for (id, status) in [
        (&empty, StatusCode::BAD_REQUEST),
        (&working, StatusCode::CONFLICT),
        (&waiting, StatusCode::CONFLICT),
        (&source, StatusCode::CONFLICT),
    ] {
        let response = test
            .client
            .post(format!("{}/handoff", test.url(id)))
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    assert_eq!(test.model.0.lock().unwrap().len(), count);
    drop(btw);
    // Closing waits for the cancelled private worker and its usage to finish.
    test.client.delete(test.url(&source)).send().await.unwrap();
    test.client
        .post(format!("{}/resume", test.url(&source)))
        .json(&json!({"env":{"HOME":test.root.path().join("home")}}))
        .send()
        .await
        .unwrap();
    let before = test.wait_status(&source, SessionStatus::Idle).await;
    let mut handoff = test
        .client
        .post(format!("{}/handoff", test.url(&source)))
        .json(&json!({"direction":"handoff-held"}))
        .send()
        .await
        .unwrap();
    assert_eq!(handoff.status(), StatusCode::OK);
    while !String::from_utf8_lossy(&handoff.chunk().await.unwrap().unwrap()).contains("held answer")
    {
    }
    for (method, suffix, body) in [
        (
            reqwest::Method::POST,
            "/input",
            json!({"input":[{"type":"text","text":"overlap"}],"clientName":"test"}),
        ),
        (reqwest::Method::POST, "/compact", json!({})),
        (reqwest::Method::PATCH, "", json!({"reasoningEffort":"max"})),
        (reqwest::Method::POST, "/btw", json!({"question":"overlap"})),
    ] {
        let response = test
            .client
            .request(method, format!("{}{suffix}", test.url(&source)))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "{suffix} must wait for handoff"
        );
    }
    assert_eq!(test.wait_status(&source, SessionStatus::Idle).await, before);
    drop(handoff);
    test.finish().await;
}
impl BtwTest {
    async fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-btw-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::write(root.path().join("file.txt"), "test content").unwrap();
        let model = Arc::new(BtwModel(Default::default()));
        let app = Arc::new(App {
            mcp: Default::default(),
            lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
            home: root.path().join("state"),
            store: Arc::new(Store::new(root.path().join("state/sessions")).unwrap()),
            model: model.clone(),
            default_model: "deepseek-flash".into(),
            models: vec!["deepseek-flash".into()],
            options: CoreOptions {
                request_time: || 349_200_000,
                ..Default::default()
            },
            sessions: Default::default(),
            registry_changed: Default::default(),
            draining: Default::default(),
            stopped: CancellationToken::new(),
        });
        let socket = root.path().join("server.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let router = router(app.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
                .unwrap();
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .unix_socket(socket)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        Self {
            root,
            app,
            model,
            client,
            stop,
            server,
        }
    }
    async fn create(&self) -> String {
        let response: SessionCreateResponse = self.client.post("http://localhost/v1/sessions").json(&json!({
            "cwd": self.root.path(), "env": {"HOME":self.root.path().join("home"),"CANTER_HOME":self.app.home},
            "model":"deepseek-flash", "reasoningEffort":"low", "approvalPolicy":"allowAll", "clientName":"test"
        })).send().await.unwrap().json().await.unwrap();
        response.session.id
    }
    fn url(&self, id: &str) -> String {
        format!("http://localhost/v1/sessions/{id}")
    }
    async fn input(&self, id: &str, text: &str) {
        let response = self
            .client
            .post(format!("{}/input", self.url(id)))
            .json(&json!({"input":[{"type":"text","text":text}],"clientName":"test"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    async fn wait_status(&self, id: &str, status: SessionStatus) -> SessionSnapshot {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let response: SessionReadResponse = self
                    .client
                    .get(self.url(id))
                    .send()
                    .await
                    .unwrap()
                    .json()
                    .await
                    .unwrap();
                if response.session.status == status {
                    break response.session;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn finish(self) {
        for session in self.app.sessions.lock().await.values() {
            session.close().await.unwrap();
        }
        self.stop.cancel();
        self.server.await.unwrap();
    }
}

#[tokio::test]
async fn context_has_no_measured_usage_after_only_btw() {
    let test = BtwTest::new().await;
    let id = test.create().await;
    let response = test
        .client
        .post(format!("{}/btw", test.url(&id)))
        .json(&json!({"question":"btw-only before the main turn"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().await.unwrap().contains("completed"));
    let report: serde_json::Value = test
        .client
        .get(format!("{}/context", test.url(&id)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(report["contextTokens"].is_null(), "{report}");
    test.finish().await;
}

#[tokio::test]
async fn btw_preserves_main_requests_history_and_context_but_records_cost() {
    use canter_model::Message;
    let test = BtwTest::new().await;
    let control = test.create().await;
    let session = test.create().await;
    for id in [&control, &session] {
        test.input(id, "hello").await;
        test.wait_status(id, SessionStatus::Idle).await;
    }
    test.input(&control, "next").await;
    test.wait_status(&control, SessionStatus::Idle).await;
    let baseline = test.model.0.lock().unwrap().last().unwrap().clone();
    let history = test
        .app
        .store
        .model_history::<serde_json::Value>(&session)
        .unwrap();
    let before = test.wait_status(&session, SessionStatus::Idle).await;
    let question = "btw-only question\n  keep original spaces  ";
    let response = test
        .client
        .post(format!("{}/btw", test.url(&session)))
        .json(&json!({"question":question}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "btw must stream without starting a turn"
    );
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let answer = response.text().await.unwrap();
    assert!(answer.contains("private **btw answer**"));
    assert!(answer.contains("completed"));
    let mut expected = baseline.clone();
    *expected.messages.last_mut().unwrap() = Message::User {
        text: question.into(),
    };
    let request = test.model.0.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        serde_json::to_string(&request).unwrap(),
        serde_json::to_string(&expected).unwrap()
    );
    assert_eq!(
        test.app
            .store
            .model_history::<serde_json::Value>(&session)
            .unwrap(),
        history
    );
    let after = test.wait_status(&session, SessionStatus::Idle).await;
    assert_eq!(after.turns, before.turns);
    let before_usage = before.token_usage.unwrap();
    let after_usage = after.token_usage.unwrap();
    assert_eq!(after_usage.context_tokens, before_usage.context_tokens);
    assert_eq!(
        after_usage.total.total_tokens,
        before_usage.total.total_tokens + 1100
    );
    assert!(
        (after_usage.cost.unwrap().usd - before_usage.cost.unwrap().usd - 0.0001848).abs() < 1e-10
    );
    for endpoint in [
        format!("{}/usage", test.url(&session)),
        "http://localhost/v1/usage".into(),
    ] {
        let report: UsageResponse = test
            .client
            .get(endpoint)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(report.total.cost.usd >= 0.0001848);
    }
    let events = serde_json::to_string(&test.app.store.events_after(&session, 0).unwrap()).unwrap();
    assert!(!events.contains("btw-only"));
    assert!(!events.contains("btw answer"));
    assert!(!test.root.path().join("btw-tool.txt").exists());
    test.input(&session, "next").await;
    test.wait_status(&session, SessionStatus::Idle).await;
    let actual = test.model.0.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        serde_json::to_string(&actual).unwrap(),
        serde_json::to_string(&baseline).unwrap()
    );
    let snapshot = test.wait_status(&session, SessionStatus::Idle).await;
    assert_eq!(
        test.client
            .delete(test.url(&session))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let restored: SessionReadResponse = test
        .client
        .post(format!("{}/resume", test.url(&session)))
        .json(&json!({"env":{"HOME":test.root.path().join("home")}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let restored_usage = restored.session.token_usage.unwrap();
    assert_eq!(restored_usage.total, snapshot.token_usage.unwrap().total);
    assert_eq!(restored_usage.context_tokens, 120);
    assert!((restored_usage.cost.unwrap().usd - 0.00024576).abs() < 1e-10);
    assert_eq!(restored.session.turns, snapshot.turns);
    test.finish().await;
}

#[tokio::test]
async fn btw_during_tools_omits_the_whole_unfinished_group_and_does_not_charge_the_turn() {
    use canter_model::Message;
    let test = BtwTest::new().await;
    let id = test.create().await;
    test.input(&id, "tool-main").await;
    let before = test.wait_status(&id, SessionStatus::WaitingApproval).await;
    assert!(before.turns[0].items.iter().any(|item| matches!(
        item,
        Item::FileRead {
            status: ItemStatus::Completed,
            ..
        }
    )));
    let mut expected = test.model.0.lock().unwrap().last().unwrap().clone();
    expected.messages.push(Message::User {
        text: "btw-only during tools".into(),
    });
    let history = test
        .app
        .store
        .model_history::<serde_json::Value>(&id)
        .unwrap();
    let response = test
        .client
        .post(format!("{}/btw", test.url(&id)))
        .json(&json!({"question":"btw-only during tools"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("private **btw answer**")
    );
    let actual = test.model.0.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        actual, expected,
        "omit the assistant tool calls and partial results together"
    );
    let after = test.wait_status(&id, SessionStatus::WaitingApproval).await;
    // The turn is still running, so its time goes on; nothing else changes.
    let untimed = |turns: &[Turn]| {
        turns
            .iter()
            .map(|turn| Turn {
                duration_ms: None,
                ..turn.clone()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(untimed(&after.turns), untimed(&before.turns));
    let time = |turns: &[Turn]| turns[0].duration_ms.expect("a running turn is timed");
    assert!(time(&after.turns) >= time(&before.turns));
    assert_eq!(after.pending_requests, before.pending_requests);
    assert_eq!(
        test.app
            .store
            .model_history::<serde_json::Value>(&id)
            .unwrap(),
        history
    );
    let usage = after.token_usage.unwrap();
    assert_eq!(usage.context_tokens, 0);
    assert_eq!(
        usage.model_context_window, None,
        "btw must not introduce a ctx display before the first main usage"
    );
    let request_id = &after.pending_requests[0].id;
    let response = test
        .client
        .post(format!("{}/requests/{request_id}", test.url(&id)))
        .json(&json!({"answer":{"type":"option","label":"yes"},"clientName":"test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let finished = test.wait_status(&id, SessionStatus::Idle).await;
    assert_eq!(finished.turns[0].status, TurnStatus::Completed);
    assert_eq!(
        finished.turns[0].usage.as_ref().unwrap().total.total_tokens,
        120
    );
    let report: UsageResponse = test
        .client
        .get(format!("{}/usage", test.url(&id)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!((report.total.cost.usd - 0.00021528).abs() < 1e-10);
    assert!((report.last_turn_cost.unwrap().usd - 0.00003048).abs() < 1e-10);
    test.finish().await;
}

#[tokio::test]
async fn btw_disconnect_keeps_received_usage_and_never_blocks_the_main_turn() {
    let test = BtwTest::new().await;
    let id = test.create().await;
    let endpoint = format!("{}/btw", test.url(&id));
    assert_eq!(
        test.client
            .post(&endpoint)
            .json(&json!({"question":" \n "}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert!(test.model.0.lock().unwrap().is_empty());
    let mut response = test
        .client
        .post(&endpoint)
        .json(&json!({"question":"btw-held"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut text = String::new();
    while !text.contains("held answer") {
        text += std::str::from_utf8(&response.chunk().await.unwrap().unwrap()).unwrap();
    }
    assert_eq!(
        test.client
            .post(&endpoint)
            .json(&json!({"question":"overlap"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    test.input(&id, "hello").await;
    let before = test.wait_status(&id, SessionStatus::Idle).await;
    assert_eq!(
        before.turns[0].status,
        TurnStatus::Completed,
        "main completes while btw is still streaming"
    );
    drop(response);
    let after = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = test.wait_status(&id, SessionStatus::Idle).await;
            if snapshot.token_usage.as_ref().unwrap().total.total_tokens == 1220 {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(after.turns, before.turns);
    assert_eq!(after.token_usage.unwrap().context_tokens, 120);
    let ledger = canter_core::usage::Ledger::replay(
        &test.app.store.events_after(&id, 0).unwrap(),
        &test.app.options.prices,
    );
    assert!((ledger.report.total.cost.usd - 0.00021528).abs() < 1e-10);
    assert!((ledger.report.last_turn_cost.unwrap().usd - 0.00003048).abs() < 1e-10);
    test.finish().await;
}

#[tokio::test]
async fn lsp_routes_are_advertised_and_disabled_sessions_remain_empty() {
    let test = BtwTest::new().await;
    let id = test.create().await;
    let info: InfoResponse = test
        .client
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(info.features.unwrap().contains(&"lsp".into()));
    for endpoint in ["languageServers", "diagnostics"] {
        let reply = test
            .client
            .get(format!("{}/{}", test.url(&id), endpoint))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), StatusCode::OK);
        assert_eq!(
            reply.json::<serde_json::Value>().await.unwrap(),
            json!({"data":[]})
        );
    }
    for action in ["stop", "retry"] {
        let reply = test
            .client
            .post(format!(
                "{}/languageServers/missing/{action}",
                test.url(&id)
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(reply.status(), StatusCode::BAD_REQUEST);
        assert!(reply.text().await.unwrap().contains("LSP is disabled"));
    }
    test.finish().await;
}
