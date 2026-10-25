use super::*;
use canter_model::{FinishReason, ModelEvent, ModelStream, Request};
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};

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
            Ok(ModelEvent::TextDelta("recovered".into())),
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
                    ModelEvent::TextDelta(if compact { "saved summary" } else { "done" }.into()),
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
    std::fs::write(home.join(".agents/AGENTS.md"), "global instructions").unwrap();
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
            "projectInstructions",
            "projectInstructions",
            "skills"
        ]
    );
    assert_eq!(
        sections[1]["path"],
        home.join(".agents/AGENTS.md").to_str().unwrap()
    );
    assert_eq!(
        sections[2]["path"],
        project.join("AGENTS.md").to_str().unwrap()
    );
    assert_eq!(sections[3]["path"], cwd.join("AGENTS.md").to_str().unwrap());
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
        text.find("\nGlobal instructions (").unwrap(),
        text.find(&format!(
            "\nProject instructions ({}):",
            project.join("AGENTS.md").display()
        ))
        .unwrap(),
        text.find(&format!(
            "\nProject instructions ({}):",
            cwd.join("AGENTS.md").display()
        ))
        .unwrap(),
        text.find("\nAvailable skills:\n").unwrap(),
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
