use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{path::Path, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
    task::{JoinHandle, JoinSet},
};

// Each request waits for explicit SSE chunks: no real provider and no timing sleeps.
struct ModelServer {
    model: Arc<ChatCompletions>,
    requests: mpsc::UnboundedReceiver<(Value, mpsc::UnboundedSender<String>)>,
    task: JoinHandle<()>,
}
impl ModelServer {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let model = Arc::new(
            ChatCompletions::new(&format!("http://{address}/v1"), ApiKey::new("fake-key")).unwrap(),
        );
        let (sender, requests) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let sender = sender.clone();
                        connections.spawn(async move {
                            let mut bytes = Vec::new();
                            let mut buffer = [0; 4096];
                            let (offset, length) = loop {
                                let count = socket.read(&mut buffer).await.unwrap();
                                assert_ne!(count, 0);
                                bytes.extend_from_slice(&buffer[..count]);
                                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                                    let header = String::from_utf8_lossy(&bytes[..end]);
                                    let length = header.lines().find_map(|line| {
                                        let (name, value) = line.split_once(':')?;
                                        name.eq_ignore_ascii_case("content-length")
                                            .then(|| value.trim().parse::<usize>().unwrap())
                                    }).unwrap();
                                    break (end + 4, length);
                                }
                            };
                            while bytes.len() < offset + length {
                                let count = socket.read(&mut buffer).await.unwrap();
                                assert_ne!(count, 0);
                                bytes.extend_from_slice(&buffer[..count]);
                            }
                            let request = serde_json::from_slice(&bytes[offset..offset + length]).unwrap();
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await.unwrap();
                            let (chunks, mut receiver) = mpsc::unbounded_channel::<String>();
                            sender.send((request, chunks)).unwrap();
                            while let Some(chunk) = receiver.recv().await {
                                let data = format!("{:x}\r\n{chunk}\r\n", chunk.len());
                                if socket.write_all(data.as_bytes()).await.is_err() { break; }
                            }
                            let _ = socket.write_all(b"0\r\n\r\n").await;
                        });
                    }
                    Some(result) = connections.join_next() => { result.unwrap(); }
                }
            }
        });
        Self {
            model,
            requests,
            task,
        }
    }
    async fn request(&mut self) -> (Value, mpsc::UnboundedSender<String>) {
        tokio::time::timeout(Duration::from_secs(10), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn stop(self) {
        self.task.abort();
        assert!(self.task.await.unwrap_err().is_cancelled());
    }
}

fn input(text: &str) -> SessionInputParams {
    serde_json::from_value(json!({
        "input": [{"type":"text", "text":text}], "mode":"queue", "clientName":"test"
    }))
    .unwrap()
}
fn runtime(path: &Path) -> RuntimeContext {
    RuntimeContext {
        environment: [("HOME".into(), path.to_string_lossy().into())].into(),
        ..RuntimeContext::default()
    }
}
async fn create(
    store: Arc<Store>,
    server: &ModelServer,
    path: &Path,
    options: CoreOptions,
) -> Session {
    Session::create(
        store,
        "retract".into(),
        SessionSettings {
            additional_directories: Vec::new(),
            plan_mode: false,
            pre_plan_approval_policy: None,
            plan_enabled: None,
            sandbox_enabled: None,
            cwd: path.to_string_lossy().into(),
            model: "deepseek-flash".into(),
            name: None,
            reasoning_effort: Default::default(),
            approval_policy: PermissionMode::AllowAll,
        },
        runtime(path),
        server.model.clone(),
        options,
    )
    .await
    .unwrap()
    .0
}
fn answer(chunks: &mpsc::UnboundedSender<String>) {
    chunks.send("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"kept answer\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into()).unwrap();
}
async fn idle(events: &mut Subscription) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                events.recv().await.unwrap().unwrap().payload,
                EventPayload::SessionStatus {
                    status: SessionStatus::Idle,
                    ..
                }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
}
async fn until(events: &mut Subscription, predicate: impl Fn(&EventPayload) -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if predicate(&events.recv().await.unwrap().unwrap().payload) {
                break;
            }
        }
    })
    .await
    .unwrap();
}
async fn interrupt(session: &Session, turn: &str, retract: bool) -> Value {
    let params = serde_json::from_value(json!({"turnId":turn,"retract":retract})).unwrap();
    serde_json::to_value(session.interrupt(params).await.unwrap()).unwrap()
}

#[tokio::test]
async fn retract_filters_text_and_images_immediately_and_after_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let mut server = ModelServer::start().await;
    let session = create(store.clone(), &server, temp.path(), CoreOptions::default()).await;
    let mut events = session
        .subscribe(session.snapshot().await.event_seq)
        .await
        .unwrap();
    session
        .input(input("keep earlier conversation"))
        .await
        .unwrap();
    answer(&server.request().await.1);
    idle(&mut events).await;

    let image = temp.path().join("original.png");
    std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
    let mut message = input("withdraw this\nwhole message");
    message.input.push(UserInput::LocalImage {
        path: image.to_string_lossy().into(),
        name: Some("original.png".into()),
    });
    let turn = session.input(message).await.unwrap().turn_id.unwrap();
    let (original, pending) = server.request().await;
    assert!(original.to_string().contains("data:image/png;base64,"));
    session.input(input("queued one")).await.unwrap();
    session.input(input("queued two")).await.unwrap();
    let response = interrupt(&session, &turn, true).await;
    assert_eq!(response["retracted"], true);
    drop(pending);
    let snapshot = session.snapshot().await.session;
    assert_eq!(snapshot.turns.len(), 1);
    assert!(snapshot.queued_inputs.is_empty());
    let stored = store.events_after("retract", 0).unwrap();
    assert!(
        stored
            .iter()
            .any(|e| serde_json::to_value(e).unwrap()["type"] == "turn.retracted")
    );
    assert!(stored.iter().any(|e| matches!(&e.payload, EventPayload::InputRemoved { input_ids, reason: InputRemovalReason::Interrupted, .. } if input_ids.len() == 2)));
    assert!(stored.iter().any(|e| matches!(&e.payload, EventPayload::TurnStarted { turn: started, .. } if started.id == turn)), "old events remain on disk");

    for reopened in [false, true] {
        let current = if reopened {
            session.close().await.unwrap();
            Session::open(
                store.clone(),
                "retract".into(),
                runtime(temp.path()),
                server.model.clone(),
                CoreOptions::default(),
            )
            .await
            .unwrap()
            .0
        } else {
            session.clone()
        };
        let mut events = current
            .subscribe(current.snapshot().await.event_seq)
            .await
            .unwrap();
        current.input(input("new corrected message")).await.unwrap();
        let (request, chunks) = server.request().await;
        let text = request["messages"].to_string();
        assert!(text.contains("keep earlier conversation"));
        assert!(text.contains("new corrected message"));
        for removed in [
            "withdraw this",
            "data:image",
            "queued one",
            "queued two",
            "[This turn was interrupted",
        ] {
            assert!(!text.contains(removed), "{removed}: {text}");
        }
        answer(&chunks);
        idle(&mut events).await;
        assert!(
            !current
                .snapshot()
                .await
                .session
                .turns
                .iter()
                .any(|t| t.id == turn)
        );
        if reopened {
            current.close().await.unwrap();
        }
    }
    server.stop().await;
}

#[tokio::test]
async fn output_closes_retract_window_and_legacy_interrupt_is_unchanged() {
    for delta in [
        None,
        Some(json!({"content":"first answer"})),
        Some(json!({"reasoning_content":"first thought"})),
        Some(
            json!({"tool_calls":[{"index":0,"id":"read","type":"function","function":{"name":"read","arguments":"{\"path\":\"missing\"}"}}]}),
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let mut server = ModelServer::start().await;
        let session = create(store.clone(), &server, temp.path(), CoreOptions::default()).await;
        let mut events = session
            .subscribe(session.snapshot().await.event_seq)
            .await
            .unwrap();
        let turn = session
            .input(input("keep this input"))
            .await
            .unwrap()
            .turn_id
            .unwrap();
        let (_, chunks) = server.request().await;
        if let Some(delta) = &delta {
            let finish = delta.get("tool_calls").map(|_| "tool_calls");
            chunks
                .send(format!(
                    "data: {}\n\n",
                    json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
                ))
                .unwrap();
            if finish.is_some() {
                chunks.send("data: [DONE]\n\n".into()).unwrap();
            }
            until(&mut events, |e| matches!(e, EventPayload::ItemStarted { item, .. } | EventPayload::ItemCompleted { item, .. } if !matches!(item, Item::UserMessage { .. }))).await;
        }
        let params = if delta.is_some() {
            json!({"turnId":turn,"retract":true})
        } else {
            json!({"turnId":turn})
        };
        let reply = serde_json::to_value(
            session
                .interrupt(serde_json::from_value(params).unwrap())
                .await
                .unwrap(),
        )
        .unwrap();
        if delta.is_some() {
            assert_eq!(reply["retracted"], false);
        } else {
            assert_eq!(reply, json!({"eventSeq":reply["eventSeq"]}));
        }
        let snapshot = session.snapshot().await.session;
        assert_eq!(snapshot.turns[0].status, TurnStatus::Interrupted);
        assert_eq!(snapshot.turns[0].input, input("keep this input").input);
        assert!(
            !store
                .events_after("retract", 0)
                .unwrap()
                .iter()
                .any(|e| matches!(e.payload, EventPayload::TurnRetracted { .. }))
        );
        session.close().await.unwrap();
        server.stop().await;
    }
}

#[tokio::test]
async fn first_output_racing_retract_cannot_leave_half_a_turn() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let mut server = ModelServer::start().await;
    let session = create(store.clone(), &server, temp.path(), CoreOptions::default()).await;
    for _ in 0..20 {
        let turn = session
            .input(input("racing input"))
            .await
            .unwrap()
            .turn_id
            .unwrap();
        let (_, chunks) = server.request().await;
        let (_, reply) = tokio::join!(
            async {
                chunks.send("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"racing output\"}}]}\n\n".into()).unwrap();
            },
            interrupt(&session, &turn, true)
        );
        let events = store.events_after("retract", 0).unwrap();
        let snapshot = session.snapshot().await.session;
        if reply["retracted"] == true {
            assert!(!snapshot.turns.iter().any(|t| t.id == turn));
            assert!(!events.iter().any(|e| matches!(&e.payload,
                EventPayload::ItemStarted { turn_id, item } | EventPayload::ItemCompleted { turn_id, item }
                if turn_id == &turn && !matches!(item, Item::UserMessage { .. }))));
            assert!(!events.iter().any(
                |e| matches!(&e.payload, EventPayload::TurnCompleted { turn: t } if t.id == turn)
            ));
        } else {
            assert_eq!(reply["retracted"], false);
            assert_eq!(
                snapshot.turns.last().unwrap().status,
                TurnStatus::Interrupted
            );
            assert!(!events.iter().any(|e| matches!(&e.payload, EventPayload::TurnRetracted { turn_id } if turn_id == &turn)));
        }
    }
    session.close().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn automatic_compaction_can_retract_until_output_but_manual_compaction_cannot() {
    for mode in ["automatic-silent", "automatic-output", "manual"] {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let mut server = ModelServer::start().await;
        let session = create(
            store.clone(),
            &server,
            temp.path(),
            CoreOptions {
                // Leave room for the fixed goal tools even in the short manual setup.
                // The long input below still exceeds the automatic compaction threshold.
                context_window: 28000,
                max_output_tokens: 1024,
                ..CoreOptions::default()
            },
        )
        .await;
        let mut events = session
            .subscribe(session.snapshot().await.event_seq)
            .await
            .unwrap();
        let turn = if mode == "manual" {
            session.input(input("previous conversation")).await.unwrap();
            let (request, chunks) = server.request().await;
            assert!(
                !request["messages"]
                    .to_string()
                    .contains("Summarize the conversation in <transcript> below.")
            );
            answer(&chunks);
            idle(&mut events).await;
            session.compact().await.unwrap().turn_id.unwrap()
        } else {
            session
                .input(input(&"long input ".repeat(2100)))
                .await
                .unwrap()
                .turn_id
                .unwrap()
        };
        let (request, chunks) = server.request().await;
        assert!(
            request["messages"]
                .to_string()
                .contains("Summarize the conversation in <transcript> below.")
        );
        if mode == "automatic-output" {
            // The usage event follows the summary delta on this same stream,
            // making first-output acceptance observable without sleeps.
            chunks.send(format!("data: {}\n\ndata: {}\n\n",
                json!({"choices":[{"index":0,"delta":{"content":"summary started"}}]}),
                json!({"choices":[],"usage":{"prompt_tokens":20,"completion_tokens":3,"total_tokens":23}}))).unwrap();
            until(&mut events, |e| {
                matches!(e, EventPayload::SessionUsage { .. })
            })
            .await;
        }
        let reply = interrupt(&session, &turn, true).await;
        assert_eq!(reply["retracted"], mode == "automatic-silent");
        let history = store.model_history::<Value>("retract").unwrap();
        assert!(!history.iter().any(|row| row["type"] == "compaction"));
        if mode == "automatic-silent" {
            assert!(session.snapshot().await.session.turns.is_empty());
            assert!(
                !store
                    .events_after("retract", 0)
                    .unwrap()
                    .iter()
                    .any(|e| matches!(
                        e.payload,
                        EventPayload::ItemCompleted {
                            item: Item::ContextCompaction { .. },
                            ..
                        }
                    ))
            );
            session.input(input("short replacement")).await.unwrap();
            let (request, chunks) = server.request().await;
            assert!(!request.to_string().contains("long input"));
            answer(&chunks);
            let mut after = session
                .subscribe(reply["eventSeq"].as_u64().unwrap() + 1)
                .await
                .unwrap();
            idle(&mut after).await;
        }
        session.close().await.unwrap();
        server.stop().await;
    }
}

#[tokio::test]
async fn empty_name_clears_a_retracted_prompts_title_and_the_next_name_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let mut server = ModelServer::start().await;
    let session = create(store.clone(), &server, temp.path(), CoreOptions::default()).await;
    let turn = session
        .input(input("mistyped title"))
        .await
        .unwrap()
        .turn_id
        .unwrap();
    let (_, chunks) = server.request().await;
    session
        .update(SessionUpdateParams {
            name: Some("mistyped title".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(interrupt(&session, &turn, true).await["retracted"], true);
    drop(chunks);
    session
        .update(SessionUpdateParams {
            name: Some(String::new()),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(session.snapshot().await.session.settings.name, None);
    session
        .update(SessionUpdateParams {
            name: Some("corrected title".into()),
            ..Default::default()
        })
        .await
        .unwrap();
    session.close().await.unwrap();
    let (resumed, snapshot) = Session::open(
        store,
        "retract".into(),
        runtime(temp.path()),
        server.model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        snapshot.session.settings.name.as_deref(),
        Some("corrected title")
    );
    resumed.close().await.unwrap();
    server.stop().await;
}
