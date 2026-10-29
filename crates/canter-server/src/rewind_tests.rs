use super::*;
use canter_model::{Message, ToolCall};
use std::sync::Mutex as SyncMutex;

#[derive(Default)]
struct Replay(SyncMutex<Vec<Request>>);
impl Model for Replay {
    fn stream(&self, request: Request, cancel: CancellationToken) -> ModelStream<'_> {
        let input = match request.messages.last() {
            Some(Message::User { text }) => text.as_str(),
            _ => "",
        };
        let calls = match input {
            "one" => vec![("write", json!({"path":"a.txt","content":"one"}))],
            "two" => vec![
                (
                    "edit",
                    json!({"path":"a.txt","edits":[{"oldText":"one","newText":"two"}]}),
                ),
                ("write", json!({"path":"b.txt","content":"new"})),
            ],
            "three" => vec![
                (
                    "edit",
                    json!({"path":"a.txt","edits":[{"oldText":"two","newText":"three"}]}),
                ),
                (
                    "edit",
                    json!({"path":"b.txt","edits":[{"oldText":"new","newText":"changed"}]}),
                ),
            ],
            "shell" => vec![("shell", json!({"command":"printf external > a.txt"}))],
            _ => vec![],
        };
        let waiting = input == "wait";
        self.0.lock().unwrap().push(request);
        if waiting {
            return Box::pin(async_stream::stream! {
                cancel.cancelled().await;
                yield Ok(ModelEvent::Finished(FinishReason::Stop));
            });
        }
        let mut events: Vec<_> = calls
            .iter()
            .enumerate()
            .map(|(i, (name, args))| {
                Ok(ModelEvent::ToolCall(ToolCall {
                    id: format!("call-{i}"),
                    name: (*name).into(),
                    arguments: args.to_string(),
                }))
            })
            .collect();
        if calls.is_empty() {
            events.push(Ok(ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "done".into(),
            }));
        }
        events.push(Ok(ModelEvent::Finished(if calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCalls
        })));
        Box::pin(futures_util::stream::iter(events))
    }
}

async fn idle(client: &reqwest::Client, url: &str) -> SessionReadResponse {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot: SessionReadResponse =
                client.get(url).send().await.unwrap().json().await.unwrap();
            if snapshot.session.status == SessionStatus::Idle {
                assert!(
                    snapshot
                        .session
                        .turns
                        .iter()
                        .all(|turn| turn.status != TurnStatus::Failed)
                );
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}

async fn say(client: &reqwest::Client, url: &str, text: &str) -> SessionReadResponse {
    let response = client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":text}],"clientName":"test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    idle(client, url).await
}

#[tokio::test]
async fn rewind_restores_files_and_conversation_through_http() {
    check_rewind(true, true, false, None).await;
}

#[tokio::test]
async fn rewind_files_only_preserves_conversation() {
    check_rewind(true, false, false, None).await;
}

#[tokio::test]
async fn rewind_conversation_only_preserves_files() {
    check_rewind(false, true, false, None).await;
}

#[tokio::test]
async fn rewind_requires_confirmation_for_shell_changes() {
    check_rewind(true, true, true, None).await;
}

#[tokio::test]
async fn rewind_rejects_a_file_changed_after_preview() {
    check_rewind(true, true, false, Some(false)).await;
}

#[tokio::test]
async fn rewind_rejects_a_turn_completed_after_preview() {
    check_rewind(true, true, false, Some(true)).await;
}

fn confirmation(params: &serde_json::Value, preview: &serde_json::Value) -> serde_json::Value {
    let mut params = params.clone();
    params["confirmedExternalChanges"] = preview["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|file| file["externalChange"] == true || file["externalChangeUnknown"] == true)
        .map(|file| file["path"].clone())
        .collect();
    assert!(preview["lastEventSeq"].is_u64());
    params["lastEventSeq"] = preview["lastEventSeq"].clone();
    params
}

async fn check_rewind(
    files: bool,
    conversation: bool,
    external: bool,
    turn_after_preview: Option<bool>,
) {
    let root = tempfile::Builder::new()
        .prefix("canter-rewind-")
        .tempdir_in("/tmp")
        .unwrap();
    let project = root.path().join("project");
    let home = root.path().join("home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let model = Arc::new(Replay::default());
    let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
    let app = Arc::new(App {
        mcp: Default::default(),
        lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
        home: root.path().join("state"),
        store: store.clone(),
        model: model.clone(),
        default_model: "fake".into(),
        models: vec!["fake".into()],
        options: CoreOptions::default(),
        sessions: Default::default(),
        registry_changed: Default::default(),
        draining: Default::default(),
        stopped: CancellationToken::new(),
    });
    let server_app = app.clone();
    let socket = root.path().join("server.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let stop = CancellationToken::new();
    let shutdown = stop.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router(server_app))
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
            .unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket.clone())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let created: SessionCreateResponse = client.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":project,"env":{"HOME":home,"CANTER_HOME":root.path().join("state")},"model":"fake","approvalPolicy":"allowAll","clientName":"test"}))
        .send().await.unwrap().json().await.unwrap();
    let url = format!("http://localhost/v1/sessions/{}", created.session.id);
    say(&client, &url, "one").await;
    let two = say(&client, &url, "two").await.session.turns[1].id.clone();
    say(&client, &url, "three").await;
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        "three"
    );
    if external {
        say(&client, &url, "shell").await;
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "external"
        );
    }
    let params = json!({"beforeTurnId":two,"rewindFiles":files,"rewindConversation":conversation});
    // Both list and command reject an active turn, including attempts to rewind an older turn.
    client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":"wait"}],"clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        client
            .post(format!("{url}/rewind"))
            .json(&params)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        client
            .get(format!("{url}/rewind"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    let running: SessionReadResponse = client.get(&url).send().await.unwrap().json().await.unwrap();
    client
        .post(format!("{url}/interrupt"))
        .json(&json!({"turnId":running.session.turns.last().unwrap().id}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let before = idle(&client, &url).await;
    let list: serde_json::Value = client
        .get(format!("{url}/rewind"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let choice = list["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|turn| turn["turnId"] == two)
        .unwrap();
    assert_eq!(choice["fileCount"], 2);
    assert_eq!(
        list["data"][0]["turnId"],
        running.session.turns.last().unwrap().id
    );
    let preview: serde_json::Value = client
        .post(format!("{url}/rewind/preview"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut unconfirmed = params.clone();
    unconfirmed["lastEventSeq"] = preview["lastEventSeq"].clone();
    if external {
        let changed = preview["files"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|file| file["externalChange"] == true)
            .collect::<Vec<_>>();
        assert_eq!(changed.len(), 1);
        assert!(changed[0]["path"].as_str().unwrap().ends_with("/a.txt"));
        assert_eq!(
            client
                .post(format!("{url}/rewind"))
                .json(&unconfirmed)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "external"
        );
    }
    if let Some(new_turn) = turn_after_preview {
        if new_turn {
            say(&client, &url, "one").await;
        } else {
            std::fs::write(project.join("b.txt"), "user saved after preview").unwrap();
        }
        let unchanged = idle(&client, &url).await;
        let a = std::fs::read(project.join("a.txt")).unwrap();
        let b = std::fs::read(project.join("b.txt")).unwrap();
        let response = client
            .post(format!("{url}/rewind"))
            .json(&confirmation(&params, &preview))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CONFLICT,
            "a stale confirmation must not rewind anything"
        );
        assert_eq!(std::fs::read(project.join("a.txt")).unwrap(), a);
        assert_eq!(std::fs::read(project.join("b.txt")).unwrap(), b);
        assert_eq!(idle(&client, &url).await, unchanged);
        client
            .delete(&url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        stop.cancel();
        server.await.unwrap();
        return;
    }
    let second = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket.clone())
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let mut events = second
        .get(format!(
            "http://localhost/v1/events?session={}&after={}",
            created.session.id, before.event_seq
        ))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes_stream();
    let directory = root.path().join("state/sessions").join(&created.session.id);
    let old_events = std::fs::read(directory.join("events.jsonl")).unwrap();
    let old_history = std::fs::read(directory.join("model-history.jsonl")).unwrap();
    let params = confirmation(&params, &preview);
    let response = client
        .post(format!("{url}/rewind"))
        .json(&params)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "rewind command must be supported"
    );
    let response: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        response["input"],
        if conversation {
            json!([{"type":"text","text":"two"}])
        } else {
            json!([])
        }
    );
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        if files { "one" } else { "three" }
    );
    assert_eq!(project.join("b.txt").exists(), !files);
    let mut snapshot = idle(&client, &url).await;
    let count = snapshot
        .session
        .turns
        .iter()
        .filter(|turn| !turn.input.is_empty())
        .count();
    assert_eq!(count, if conversation { 1 } else { 4 });
    assert!(
        std::fs::read(directory.join("events.jsonl"))
            .unwrap()
            .starts_with(&old_events)
    );
    assert!(
        std::fs::read(directory.join("model-history.jsonl"))
            .unwrap()
            .starts_with(&old_history)
    );
    use futures_util::StreamExt;
    let mut observed = before.session;
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        loop {
            bytes.extend(events.next().await.unwrap().unwrap());
            let text = String::from_utf8_lossy(&bytes);
            if let Some(line) = text
                .lines()
                .find(|line| line.starts_with("data:") && line.contains("session.rewound"))
            {
                let event: Event =
                    serde_json::from_str(line.trim_start_matches("data:").trim()).unwrap();
                apply_event_mut(&mut observed, &event).unwrap();
                break;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(
        observed.turns, snapshot.session.turns,
        "second client's event reducer must see the same rewind"
    );
    drop(events);
    let mut expected_after_immediate = None;
    if conversation {
        snapshot = say(&client, &url, "immediate").await;
        let requests = model.0.lock().unwrap();
        let old = requests
            .iter()
            .find(|r| matches!(r.messages.last(), Some(Message::User {text}) if text == "two"))
            .unwrap();
        let immediate = requests.last().unwrap();
        assert_eq!(
            serde_json::to_vec(&old.messages[..old.messages.len() - 1]).unwrap(),
            serde_json::to_vec(&immediate.messages[..immediate.messages.len() - 1]).unwrap()
        );
        let mut expected = immediate.messages.clone();
        expected.push(Message::Assistant {
            content: vec![canter_model::AssistantContent::Text {
                text: "done".into(),
            }],
        });
        expected_after_immediate = Some(expected);
    }
    let session = app
        .sessions
        .lock()
        .await
        .remove(&created.session.id)
        .unwrap();
    session.close_for_shutdown().await.unwrap();
    drop(session);
    stop.cancel();
    server.await.unwrap();
    drop(app);
    std::fs::remove_file(&socket).unwrap();
    // A fresh server registry and newly opened store exercise durable recovery.
    let app = Arc::new(App {
        mcp: Default::default(),
        lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
        home: root.path().join("state"),
        store: Arc::new(Store::new(root.path().join("state/sessions")).unwrap()),
        model: model.clone(),
        default_model: "fake".into(),
        models: vec!["fake".into()],
        options: CoreOptions::default(),
        sessions: Default::default(),
        registry_changed: Default::default(),
        draining: Default::default(),
        stopped: CancellationToken::new(),
    });
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
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let resumed: SessionResumeResponse = client
        .post(format!("{url}/resume"))
        .json(&json!({"env":{"HOME":home,"CANTER_HOME":root.path().join("state")}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resumed.session.turns, snapshot.session.turns);
    assert_eq!(
        std::fs::read_to_string(project.join("a.txt")).unwrap(),
        if files { "one" } else { "three" }
    );
    say(&client, &url, "again").await;
    if conversation {
        let requests = model.0.lock().unwrap();
        let old = requests
            .iter()
            .find(|r| matches!(r.messages.last(), Some(Message::User {text}) if text == "two"))
            .unwrap();
        let new = requests.last().unwrap();
        assert_eq!(
            serde_json::to_vec(&old.messages[..old.messages.len() - 1]).unwrap(),
            serde_json::to_vec(&new.messages[..old.messages.len() - 1]).unwrap()
        );
    }
    if let Some(expected) = expected_after_immediate {
        let requests = model.0.lock().unwrap();
        let new = requests.last().unwrap();
        assert_eq!(
            serde_json::to_vec(&expected).unwrap(),
            serde_json::to_vec(&new.messages[..new.messages.len() - 1]).unwrap()
        );
    }
    client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    stop.cancel();
    server.await.unwrap();
}
