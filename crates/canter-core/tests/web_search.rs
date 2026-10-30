use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{FinishReason, Message, Model, ModelEvent, ModelStream, Request, ToolCall};
use canter_protocol::*;
use canter_store::Store;
use canter_tools::web_search::{WebSearch, deepseek::DeepSeek};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

const KEY: &str = "test-only-search-credential";

struct ScriptModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
    requests: Mutex<Vec<Request>>,
}
impl ScriptModel {
    fn new(queries: &[&str]) -> Arc<Self> {
        let mut calls: Vec<_> = queries
            .iter()
            .enumerate()
            .map(|(i, query)| {
                ModelEvent::ToolCall(ToolCall {
                    id: format!("call-{i}"),
                    name: "web_search".into(),
                    arguments: json!({"query":query}).to_string(),
                })
            })
            .collect();
        calls.push(ModelEvent::Finished(FinishReason::ToolCalls));
        Arc::new(Self {
            scripts: Mutex::new(
                vec![
                    calls,
                    vec![
                        ModelEvent::TextDelta {
                            item_id: "text".into(),
                            delta: "continued".into(),
                        },
                        ModelEvent::Finished(FinishReason::Stop),
                    ],
                ]
                .into(),
            ),
            requests: Mutex::new(Vec::new()),
        })
    }
    fn results(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResult { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}
impl Model for ScriptModel {
    fn stream(&self, request: Request, _cancel: CancellationToken) -> ModelStream<'_> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected model request");
        Box::pin(async_stream::stream! { for event in events { yield Ok(event); } })
    }
}

struct SearchServer {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
    closed: Arc<tokio::sync::Notify>,
}
impl SearchServer {
    async fn new(response: impl Fn(&Value) -> String + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let closed = Arc::new(tokio::sync::Notify::new());
        let disconnected = closed.clone();
        let response = Arc::new(response);
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted.unwrap();
                        let captured = captured.clone();
                        let response = response.clone();
                        let disconnected = disconnected.clone();
                        connections.spawn(async move {
                            let mut headers = Vec::new();
                            while !headers.ends_with(b"\r\n\r\n") {
                                let Ok(byte) = socket.read_u8().await else { return; };
                                headers.push(byte);
                            }
                            let headers = String::from_utf8(headers).unwrap().to_lowercase();
                            let length: usize = headers.lines().find_map(|line| line.strip_prefix("content-length: ")).unwrap().parse().unwrap();
                            let mut body = vec![0; length];
                            socket.read_exact(&mut body).await.unwrap();
                            let request: Value = serde_json::from_slice(&body).unwrap();
                            captured.lock().unwrap().push(request.clone());
                            let reply = response(&request);
                            let _ = socket.write_all(reply.as_bytes()).await;
                            if !reply.contains("Connection: close\r\n") {
                                let _ = socket.read_u8().await;
                                disconnected.notify_one();
                            }
                        });
                    }
                    Some(result) = connections.join_next(), if !connections.is_empty() => { result.unwrap(); }
                }
            }
        });
        Self {
            address,
            requests,
            task,
            closed,
        }
    }
    fn service(&self) -> WebSearch {
        WebSearch::new(Arc::new(DeepSeek::new(
            KEY.into(),
            format!("http://{}/v1/tools/search", self.address)
                .parse()
                .unwrap(),
        )))
    }
}
impl Drop for SearchServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn reply(status: u16, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn results() -> String {
    json!({"results":[
        {"title":"Rust book","url":"https://doc.rust-lang.org/book/","date":"2026-10-01T00:00:00Z","summary":"Learn Rust"},
        {"title":"Rust reference","url":"https://doc.rust-lang.org/reference/","date":null,"summary":"Language details"}
    ]}).to_string()
}
async fn create(
    model: Arc<ScriptModel>,
    mode: PermissionMode,
    plan: bool,
    search: Option<WebSearch>,
) -> (tempfile::TempDir, Session, Subscription) {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
    let (session, _) = Session::create(
        store,
        "search".into(),
        SessionSettings {
            additional_directories: Vec::new(),
            plan_mode: plan,
            pre_plan_approval_policy: None,
            plan_enabled: None,
            sandbox_enabled: None,
            cwd: root.path().to_string_lossy().into(),
            name: None,
            model: "fake".into(),
            reasoning_effort: Default::default(),
            approval_policy: mode,
        },
        RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    root.path().join("home").to_string_lossy().into(),
                ),
                (
                    "CANTER_HOME".into(),
                    root.path().join("state").to_string_lossy().into(),
                ),
            ]
            .into(),
            ..Default::default()
        },
        model,
        CoreOptions {
            context_window: 64 * 1024 * 1024,
            web_search: search,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let events = session.subscribe(0).await.unwrap();
    (root, session, events)
}
fn input(review: bool) -> SessionInputParams {
    SessionInputParams {
        input: vec![UserInput::Text {
            text: "search fixtures".into(),
        }],
        mode: InputMode::Queue,
        review: review.then(|| "Review fixtures".into()),
        client_name: "test".into(),
    }
}
async fn until(events: &mut Subscription, predicate: impl Fn(&EventPayload) -> bool) -> Vec<Event> {
    tokio::time::timeout(Duration::from_secs(45), async {
        let mut seen = Vec::new();
        loop {
            let event = events.recv().await.unwrap().expect("event stream ended");
            let done = predicate(&event.payload);
            seen.push(event);
            if done {
                return seen;
            }
        }
    })
    .await
    .expect("expected event did not arrive")
}
async fn finished(events: &mut Subscription) -> Vec<Event> {
    until(events, |event| {
        matches!(event, EventPayload::TurnCompleted { .. })
    })
    .await
}

#[tokio::test]
async fn web_search_reaches_the_model_with_frozen_tools_and_conditional_prompt() {
    let server = SearchServer::new(|_| reply(200, &results())).await;
    let model = ScriptModel::new(&["Rust docs"]);
    let (_root, session, mut events) = create(
        model.clone(),
        PermissionMode::AllowAll,
        false,
        Some(server.service()),
    )
    .await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::ItemCompleted { item: Item::WebSearch { results, status: ItemStatus::Completed, .. }, .. } if results.len() == 2)));
    let result = &model.results()[0];
    for text in [
        "Rust book",
        "https://doc.rust-lang.org/book/",
        "2026-10-01",
        "Learn Rust",
        "Rust reference",
        "untrusted",
    ] {
        assert!(result.contains(text), "{result}");
    }
    {
        let requests = model.requests.lock().unwrap();
        let names: Vec<_> = requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect();
        let fetch = names.iter().position(|name| *name == "web_fetch").unwrap();
        assert_eq!(names[fetch + 1], "web_search");
        assert_eq!(
            serde_json::to_value(&requests[0].tools).unwrap(),
            serde_json::to_value(&requests[1].tools).unwrap()
        );
        let prompt = format!("{:?}", requests[0].messages);
        assert!(prompt.contains("web searches"));
        assert!(prompt.contains("search results"));
    }
    assert_eq!(
        *server.requests.lock().unwrap(),
        [json!({"query":"Rust docs","count":10})]
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn web_search_errors_are_safe_do_not_retry_and_let_the_turn_continue() {
    let server = SearchServer::new(|request| match request["query"].as_str().unwrap() {
        "empty" => reply(200, r#"{"results":[]}"#),
        "invalid json" => reply(200, KEY),
        "invalid shape" => reply(200, &json!({"results":[{"title":KEY}]}).to_string()),
        "credential echo" => reply(200, &json!({"results":[{"title":KEY,"url":format!("https://example.com/{KEY}"),"date":KEY,"summary":KEY}]}).to_string()),
        status => reply(status.parse().unwrap(), KEY),
    }).await;
    let model = ScriptModel::new(&[
        "empty",
        "401",
        "402",
        "429",
        "500",
        "504",
        "invalid json",
        "invalid shape",
        "credential echo",
    ]);
    let (root, session, mut events) = create(
        model.clone(),
        PermissionMode::AllowAll,
        false,
        Some(server.service()),
    )
    .await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let results = model.results();
    for (result, expected) in results.iter().zip([
        "No results found",
        "401",
        "insufficient balance",
        "429",
        "500",
        "504",
        "invalid response format",
        "invalid response format",
        "[redacted]",
    ]) {
        assert!(result.contains(expected), "{result}");
        assert!(!result.contains(KEY));
    }
    assert!(results[2].contains("top up"));
    assert_eq!(server.requests.lock().unwrap().len(), 9);
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
    );
    assert!(!serde_json::to_string(&seen).unwrap().contains(KEY));
    assert!(!format!("{:?}", model.requests.lock().unwrap()).contains(KEY));
    session.close().await.unwrap();
    assert_files_have_no_key(&root.path().join("state"));
}

fn assert_files_have_no_key(directory: &std::path::Path) {
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            assert_files_have_no_key(&path);
        } else {
            assert!(!String::from_utf8_lossy(&std::fs::read(path).unwrap()).contains(KEY));
        }
    }
}

#[tokio::test]
async fn web_search_total_timeout_covers_headers_and_body_and_continues() {
    for body in [false, true] {
        let server = SearchServer::new(move |_| {
            if body {
                "HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n{\"results\":".into()
            } else {
                String::new()
            }
        })
        .await;
        let model = ScriptModel::new(&["slow"]);
        let (_root, session, mut events) = create(
            model.clone(),
            PermissionMode::AllowAll,
            false,
            Some(server.service().with_timeout(Duration::from_secs(1))),
        )
        .await;
        session.input(input(false)).await.unwrap();
        let seen = finished(&mut events).await;
        assert!(model.results()[0].contains("timed out"));
        assert!(
            matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        tokio::time::timeout(Duration::from_secs(5), server.closed.notified())
            .await
            .unwrap();
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn web_search_interrupt_cancels_the_in_flight_response() {
    let server = SearchServer::new(|_| {
        "HTTP/1.1 200 OK\r\nContent-Length: 1024\r\n\r\n{\"results\":".into()
    })
    .await;
    let model = ScriptModel::new(&["slow"]);
    let (_root, session, mut events) = create(
        model.clone(),
        PermissionMode::AllowAll,
        false,
        Some(server.service()),
    )
    .await;
    let turn_id = session.input(input(false)).await.unwrap().turn_id.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::timeout(
        Duration::from_secs(5),
        session.interrupt(SessionInterruptParams {
            turn_id,
            retract: None,
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)
    );
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::ItemCompleted { item: Item::WebSearch { status: ItemStatus::Failed, error: Some(error), .. }, .. } if error.contains("cancelled"))));
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    tokio::time::timeout(Duration::from_secs(5), server.closed.notified())
        .await
        .unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn web_search_session_grant_applies_to_later_queries_in_regular_and_plan_modes() {
    for plan in [false, true] {
        for mode in [
            PermissionMode::AlwaysAsk,
            PermissionMode::AskForExecution,
            PermissionMode::AllowAll,
        ] {
            let server = SearchServer::new(|_| reply(200, &results())).await;
            let model = ScriptModel::new(&["Rust docs", "Other docs"]);
            let (_root, session, mut events) =
                create(model, mode, plan, Some(server.service())).await;
            session.input(input(false)).await.unwrap();
            let mut asked = 0;
            loop {
                let seen = until(&mut events, |event| {
                    matches!(
                        event,
                        EventPayload::RequestOpened { .. } | EventPayload::TurnCompleted { .. }
                    )
                })
                .await;
                let EventPayload::RequestOpened { request } = &seen.last().unwrap().payload else {
                    break;
                };
                let RequestKind::Approval(approval) = &request.kind else {
                    panic!("expected approval");
                };
                assert_eq!(approval.tool, "web_search");
                assert_eq!(approval.arguments["query"], "Rust docs");
                assert!(server.requests.lock().unwrap().is_empty());
                asked += 1;
                session
                    .resolve(
                        &request.id,
                        RequestResolveParams {
                            execpolicy_amendment: None,
                            plan: None,
                            feedback: None,
                            decision: Some(ApprovalDecision::AcceptForSession),
                            answer: None,
                            client_name: "test".into(),
                        },
                    )
                    .await
                    .unwrap();
            }
            assert_eq!(asked, usize::from(mode != PermissionMode::AllowAll));
            assert_eq!(server.requests.lock().unwrap().len(), 2);
            session.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn web_search_review_denies_valid_and_invalid_queries_without_approval_or_network() {
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
    ] {
        let server = SearchServer::new(|_| reply(200, &results())).await;
        let model = ScriptModel::new(&["Rust docs", ""]);
        let (_root, session, mut events) =
            create(model.clone(), mode, false, Some(server.service())).await;
        session.input(input(true)).await.unwrap();
        let seen = finished(&mut events).await;
        assert!(
            !seen
                .iter()
                .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
        );
        assert_eq!(
            model.results(),
            ["review is read-only", "review is read-only"]
        );
        assert!(server.requests.lock().unwrap().is_empty());
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn web_search_is_absent_from_unconfigured_session_requests() {
    let model = ScriptModel::new(&["Rust docs"]);
    let (_root, session, mut events) =
        create(model.clone(), PermissionMode::AllowAll, false, None).await;
    session.input(input(false)).await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        let names: Vec<_> = requests[0]
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
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
                "update_goal"
            ]
        );
        let prompt = format!("{:?}", requests[0].messages);
        assert!(!prompt.contains("web searches"));
        assert!(!prompt.contains("search results"));
        assert_eq!(
            serde_json::to_value(&requests[0].tools).unwrap(),
            serde_json::to_value(&requests[1].tools).unwrap()
        );
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn web_search_bounds_model_and_client_output_without_losing_result_structure() {
    let server = SearchServer::new(|_| reply(200, &json!({"results":[
        {"title":"Large result","url":"https://example.com/large","summary":format!("HEAD{}TAIL", "字".repeat(500_000))}
    ]}).to_string())).await;
    let model = ScriptModel::new(&["large"]);
    let (_root, session, mut events) = create(
        model.clone(),
        PermissionMode::AllowAll,
        false,
        Some(server.service()),
    )
    .await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let output = &model.results()[0];
    assert!(output.len() <= canter_tools::MAX_MODEL_OUTPUT_BYTES + 64);
    assert!(output.contains("[truncated ") && output.contains("HEAD") && output.contains("TAIL"));
    assert!(!output.contains('\u{fffd}'));
    let entry = seen
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ItemCompleted {
                item:
                    Item::WebSearch {
                        results,
                        status: ItemStatus::Completed,
                        ..
                    },
                ..
            } => results.first(),
            _ => None,
        })
        .unwrap();
    assert_eq!(entry.title, "Large result");
    assert_eq!(entry.url, "https://example.com/large");
    assert!(
        entry.summary.starts_with("HEAD")
            && entry.summary.ends_with("TAIL")
            && entry.summary.contains("[truncated ")
    );
    assert!(
        entry.title.len() + entry.url.len() + entry.summary.len()
            <= canter_tools::MAX_CLIENT_OUTPUT_BYTES
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn web_search_rejects_oversized_responses_with_and_without_content_length() {
    for length in [true, false] {
        let server = SearchServer::new(move |_| {
            let body = json!({"results":[{"title":"Large result","url":"https://example.com/large","summary":"a".repeat(5 * 1024 * 1024)}]}).to_string();
            if length { reply(200, &body) }
            else { format!("HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n{body}") }
        }).await;
        let model = ScriptModel::new(&["large"]);
        let (_root, session, mut events) = create(
            model.clone(),
            PermissionMode::AllowAll,
            false,
            Some(server.service()),
        )
        .await;
        session.input(input(false)).await.unwrap();
        finished(&mut events).await;
        assert_eq!(model.results(), ["web_search response exceeds 5 MiB"]);
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        session.close().await.unwrap();
    }
}
