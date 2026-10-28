#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session};
use canter_model::{ApiKey, ChatCompletions, RetryPolicy};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn chunk(delta: Value, reason: Option<&str>) -> String {
    format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}]})
    )
}
fn complete(text: &str) -> Reply {
    Reply::sse(format!(
        "{}data: [DONE]\n\n",
        chunk(json!({"content":text}), Some("stop"))
    ))
}
fn model(server: &Server) -> Arc<ChatCompletions> {
    Arc::new(
        ChatCompletions::new(&server.base_url, ApiKey::new(KEY))
            .unwrap()
            .with_retry_policy(RetryPolicy {
                sleep: |_| Box::pin(tokio::time::sleep(Duration::from_millis(1))),
                ..Default::default()
            }),
    )
}
fn runtime(root: &std::path::Path) -> RuntimeContext {
    RuntimeContext {
        environment: [
            (
                "HOME".into(),
                root.join("home").to_string_lossy().into_owned(),
            ),
            (
                "CANTER_HOME".into(),
                root.join("state").to_string_lossy().into_owned(),
            ),
        ]
        .into(),
        ..Default::default()
    }
}
async fn create(root: &std::path::Path, store: Arc<Store>, model: Arc<ChatCompletions>) -> Session {
    Session::create(
        store,
        "retry".into(),
        SessionSettings {
            cwd: root.to_string_lossy().into(),
            name: None,
            model: "fake".into(),
            reasoning_effort: Default::default(),
            approval_policy: PermissionMode::AllowAll,
            plan_enabled: None,
            plan_mode: false,
            pre_plan_approval_policy: None,
        },
        runtime(root),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap()
    .0
}
fn input() -> SessionInputParams {
    SessionInputParams {
        input: vec![UserInput::Text {
            text: "continue".into(),
        }],
        mode: InputMode::Queue,
        client_name: "test".into(),
        review: None,
    }
}
async fn finished(events: &mut canter_core::Subscription) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if let EventPayload::TurnCompleted { turn } = event.payload {
                assert_eq!(turn.status, TurnStatus::Completed, "{:?}", turn.error);
                break;
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn disconnected_reply_retries_without_partial_history_or_duplicate_tools_and_replays() {
    let root = tempfile::tempdir().unwrap();
    let call = json!({"index":0,"id":"once","function":{"name":"shell","arguments":r#"{"command":"printf x >> executions"}"#}});
    let partial = chunk(
        json!({"content":"partial answer","reasoning_content":"partial thought","tool_calls":[call.clone()]}),
        None,
    );
    let good = format!(
        "{}data: [DONE]\n\n",
        chunk(
            json!({"content":"complete answer","reasoning_content":"complete thought","tool_calls":[call]}),
            Some("tool_calls")
        )
    );
    let server = Server::start(vec![
        Reply::sse(partial),
        Reply::sse(good),
        complete("done"),
        complete("reopened"),
    ])
    .await;
    let model = model(&server);
    let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
    let session = create(root.path(), store.clone(), model.clone()).await;
    let mut events = session.subscribe(0).await.unwrap();
    let mut other = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    finished(&mut events).await;
    finished(&mut other).await;
    let before = session.snapshot().await.session;
    assert_eq!(
        std::fs::read_to_string(root.path().join("executions")).unwrap(),
        "x"
    );
    let items = &before.turns[0].items;
    assert!(items.iter().any(|item| matches!(item, Item::AgentMessage { text, status: Some(ItemStatus::Interrupted), .. } if text == "partial answer")));
    assert!(items.iter().any(|item| matches!(item, Item::Reasoning { content, status: Some(ItemStatus::Interrupted), .. } if content == &vec!["partial thought".to_string()])));
    assert_eq!(
        items
            .iter()
            .filter(|item| matches!(
                item,
                Item::ModelRetry {
                    attempt: 1,
                    max_retries: 10,
                    will_retry: true,
                    ..
                }
            ))
            .count(),
        1
    );
    let history = serde_json::to_string(&store.model_history::<Value>("retry").unwrap()).unwrap();
    assert!(!history.contains("partial answer") && !history.contains("partial thought"));
    assert!(history.contains("complete answer") && history.contains("complete thought"));
    session.close().await.unwrap();
    let (reopened, snapshot) = Session::open(
        store,
        "retry".into(),
        runtime(root.path()),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(snapshot.session.turns, before.turns);
    let mut events = reopened
        .subscribe(snapshot.session.last_event_seq)
        .await
        .unwrap();
    reopened.input(input()).await.unwrap();
    finished(&mut events).await;
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    assert_eq!(requests[0], requests[1]);
    assert!(!requests[3].to_string().contains("partial answer"));
    assert_eq!(server.arrivals.lock().unwrap().len(), 4);
    reopened.close().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn interruption_cancels_a_long_retry_wait_without_another_request() {
    let server = Server::start(vec![Reply {
        status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
        body: "{}".into(),
        retry_after: Some("60".into()),
        hang: false,
    }])
    .await;
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
    let model = Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap());
    let session = create(root.path(), store, model).await;
    let mut events = session.subscribe(0).await.unwrap();
    let turn = session.input(input()).await.unwrap().turn_id.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if matches!(
                event.payload,
                EventPayload::ItemCompleted {
                    item: Item::ModelRetry { .. },
                    ..
                }
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        session.interrupt(SessionInterruptParams {
            turn_id: turn,
            retract: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        session.snapshot().await.session.turns[0].status,
        TurnStatus::Interrupted
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    session.close().await.unwrap();
    server.stop().await;
}

#[tokio::test]
async fn compression_and_private_answers_share_retries_and_discard_partial_text() {
    for operation in ["compact", "btw", "handoff"] {
        let server = Server::start(vec![
            complete("initial"),
            Reply::sse(chunk(json!({"content":"discard this half"}), None)),
            complete("complete summary"),
        ])
        .await;
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
        let session = create(root.path(), store.clone(), model(&server)).await;
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input()).await.unwrap();
        finished(&mut events).await;
        if operation == "compact" {
            session.compact().await.unwrap();
            finished(&mut events).await;
            let snapshot = session.snapshot().await.session;
            assert!(snapshot.turns.iter().flat_map(|turn| &turn.items).any(|item| matches!(item, Item::ContextCompaction { summary, .. } if summary == "complete summary")));
            assert!(
                snapshot
                    .turns
                    .iter()
                    .flat_map(|turn| &turn.items)
                    .any(|item| matches!(item, Item::ModelRetry { .. }))
            );
        } else {
            let before = store.events_after("retry", 0).unwrap();
            let mut stream = if operation == "btw" {
                session.btw("question".into()).await.unwrap()
            } else {
                session.handoff(None).await.unwrap()
            };
            let mut text = String::new();
            let mut retry = false;
            loop {
                match tokio::time::timeout(Duration::from_secs(5), stream.recv())
                    .await
                    .unwrap()
                    .unwrap()
                {
                    BtwEvent::Delta { delta } => text.push_str(&delta),
                    BtwEvent::Replace {
                        text: replacement, ..
                    } => text = replacement,
                    BtwEvent::Retrying { message } => {
                        assert!(message.contains("(1/10)"));
                        retry = true;
                    }
                    BtwEvent::Completed => break,
                    event => panic!("unexpected event: {event:?}"),
                }
            }
            assert!(retry);
            assert_eq!(text, "complete summary");
            assert_eq!(store.events_after("retry", 0).unwrap(), before);
        }
        let history =
            serde_json::to_string(&store.model_history::<Value>("retry").unwrap()).unwrap();
        assert!(!history.contains("discard this half"));
        assert_eq!(server.requests.lock().unwrap().len(), 3);
        session.close().await.unwrap();
        server.stop().await;
    }
}
