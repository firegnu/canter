use crate::{CoreOptions, RuntimeContext, Session, Subscription};
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

async fn until(events: &mut Subscription, predicate: impl Fn(&EventPayload) -> bool) -> Event {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if predicate(&event.payload) {
                return event;
            }
        }
    })
    .await
    .unwrap()
}

fn runtime(path: &Path) -> RuntimeContext {
    RuntimeContext {
        environment: [
            ("HOME".into(), path.to_string_lossy().into()),
            (
                "CANTER_HOME".into(),
                path.join("home").to_string_lossy().into(),
            ),
        ]
        .into(),
        ..RuntimeContext::default()
    }
}

async fn pending(
    store: Arc<Store>,
    server: &mut ModelServer,
    path: &Path,
) -> (Session, Subscription, PendingRequest) {
    pending_call(
        store,
        server,
        path,
        "write",
        r#"{"path":"must-not-exist","content":"changed"}"#,
    )
    .await
}

async fn pending_call(
    store: Arc<Store>,
    server: &mut ModelServer,
    path: &Path,
    tool: &str,
    arguments: &str,
) -> (Session, Subscription, PendingRequest) {
    let (session, _) = Session::create(
        store,
        "feedback".into(),
        SessionSettings {
            plan_enabled: None,
            cwd: path.to_string_lossy().into(),
            model: "deepseek-flash".into(),
            name: None,
            reasoning_effort: Default::default(),
            approval_policy: PermissionMode::AlwaysAsk,
        },
        runtime(path),
        server.model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(serde_json::from_value(json!({
        "input": [{"type":"text", "text":"write the file"}], "mode":"queue", "clientName":"test"
    })).unwrap()).await.unwrap();
    let (_, chunks) = server.request().await;
    let chunk = json!({"choices":[{"index":0,"delta":{"tool_calls":[{
        "index":0,"id":"tool-call","type":"function","function":{"name":tool,
        "arguments":arguments}
    }]},"finish_reason":"tool_calls"}]});
    chunks
        .send(format!("data: {chunk}\n\ndata: [DONE]\n\n"))
        .unwrap();
    drop(chunks);
    let event = until(&mut events, |e| {
        matches!(e, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = event.payload else {
        unreachable!()
    };
    (session, events, request)
}

#[tokio::test]
async fn decline_feedback_does_not_execute_continues_and_survives_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let mut server = ModelServer::start().await;
    let (session, mut events, request) = pending(store.clone(), &mut server, temp.path()).await;
    let feedback = "  改用只读命令\n不要写文件  ";
    session
        .resolve(
            &request.id,
            serde_json::from_value(json!({
                "decision":"declineWithFeedback", "feedback":feedback, "clientName":"test"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let (next, chunks) = server.request().await;
    let result = next["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert_eq!(
        result["content"],
        format!("Tool call declined. The user says: {feedback}")
    );
    assert!(!temp.path().join("must-not-exist").exists());
    chunks.send("data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Use a read instead\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".into()).unwrap();
    drop(chunks);
    until(&mut events, |e| {
        matches!(
            e,
            EventPayload::SessionStatus {
                status: SessionStatus::Idle,
                ..
            }
        )
    })
    .await;
    let snapshot = session.snapshot().await.session;
    assert_eq!(snapshot.turns[0].status, TurnStatus::Completed);
    assert!(snapshot.turns[0].items.iter().any(|i| matches!(
        i,
        Item::FileChange {
            status: ItemStatus::Declined,
            ..
        }
    )));
    let RequestKind::Approval(approval) = &request.kind else {
        unreachable!()
    };
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap()["approvalFeedback"][&approval.item_id],
        feedback
    );
    let stored = store.events_after("feedback", 0).unwrap();
    assert!(stored.iter().any(|e| matches!(&e.payload, EventPayload::RequestResolved {
        resolution: ApprovalResolution::Answered { decision: ApprovalDecision::DeclineWithFeedback, feedback: Some(text) }, ..
    } if text == feedback)));
    session.close().await.unwrap();
    drop(session);
    let (reopened, response) = Session::open(
        store,
        "feedback".into(),
        runtime(temp.path()),
        server.model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::to_value(response.session).unwrap()["approvalFeedback"][&approval.item_id],
        feedback
    );
    reopened.close().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn decline_feedback_invalid_answers_leave_the_tool_pending() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let mut server = ModelServer::start().await;
    let (session, _, request) = pending(store, &mut server, temp.path()).await;
    for mut params in [
        json!({"decision":"declineWithFeedback"}),
        json!({"decision":"declineWithFeedback","feedback":" \n\t "}),
        json!({"decision":"accept","feedback":"do something else"}),
        json!({"decision":"acceptForSession","feedback":"do something else"}),
        json!({"decision":"decline","feedback":"do something else"}),
        json!({"decision":"cancel","feedback":"do something else"}),
    ] {
        params["clientName"] = json!("test");
        let before = session.snapshot().await;
        let error = session
            .resolve(&request.id, serde_json::from_value(params).unwrap())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidDecision);
        assert_eq!(session.snapshot().await, before);
        assert!(!temp.path().join("must-not-exist").exists());
    }
    session.close().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn decline_feedback_not_offered_leaves_the_tool_pending() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let mut server = ModelServer::start().await;
    let (session, _, request) = pending(store, &mut server, temp.path()).await;
    // Arrange an approval from a producer that does not offer this decision.
    // Resolution still goes through the normal session API and waiting runner.
    {
        let mut state = session.shared.state.lock().await;
        let RequestKind::Approval(approval) = &mut state.journal.snapshot.pending_requests[0].kind
        else {
            unreachable!()
        };
        approval
            .available_decisions
            .retain(|d| *d != ApprovalDecision::DeclineWithFeedback);
    }
    let before = session.snapshot().await;
    let error = session
        .resolve(
            &request.id,
            serde_json::from_value(json!({
                "decision":"declineWithFeedback", "feedback":"try reading", "clientName":"test"
            }))
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDecision);
    assert_eq!(session.snapshot().await, before);
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn question_feedback_is_rejected_without_changing_the_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let mut server = ModelServer::start().await;
    let (session, _, request) = pending_call(
        store, &mut server, temp.path(), "ask_user",
        r#"{"question":"Which storage?","options":[{"label":"Files","description":"Local files"},{"label":"Database","description":"Shared database"}]}"#,
    ).await;
    assert!(matches!(request.kind, RequestKind::UserInput(_)));
    let before = session.snapshot().await;
    let error = session
        .resolve(
            &request.id,
            serde_json::from_value(json!({
                "answer": {"type":"option", "label":"Files"},
                "feedback":"read instead", "clientName":"test"
            }))
            .unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidDecision);
    assert_eq!(session.snapshot().await, before);
    session.close().await.unwrap();
    server.stop().await;
}
