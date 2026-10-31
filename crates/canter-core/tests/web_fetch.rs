use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{FinishReason, Message, Model, ModelEvent, ModelStream, Request, ToolCall};
use canter_protocol::*;
use canter_store::Store;
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

struct ScriptModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
    requests: Mutex<Vec<Request>>,
}
impl ScriptModel {
    fn new(calls: Vec<Value>) -> Arc<Self> {
        let mut events: Vec<_> = calls
            .into_iter()
            .enumerate()
            .map(|(i, args)| {
                ModelEvent::ToolCall(ToolCall {
                    id: format!("call-{i}"),
                    name: "web_fetch".into(),
                    arguments: args.to_string(),
                })
            })
            .collect();
        events.push(ModelEvent::Finished(FinishReason::ToolCalls));
        Arc::new(Self {
            scripts: Mutex::new(
                vec![
                    events,
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
            .filter_map(|m| match m {
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

struct WebServer {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
    closed: Arc<tokio::sync::Notify>,
}
impl WebServer {
    async fn new(response: impl Fn(&str) -> String + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = requests.clone();
        let response = Arc::new(response);
        let closed = Arc::new(tokio::sync::Notify::new());
        let disconnected = closed.clone();
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
                            let mut request = Vec::new();
                            loop {
                                let byte = match socket.read_u8().await { Ok(byte) => byte, Err(_) => return };
                                request.push(byte);
                                if request.ends_with(b"\r\n\r\n") { break; }
                            }
                            let request = String::from_utf8(request).unwrap();
                            captured.lock().unwrap().push(request.clone());
                            let reply = response(&request);
                            let _ = socket.write_all(reply.as_bytes()).await;
                            if !reply.contains("Connection: close\r\n") {
                                let mut byte = [0];
                                let _ = socket.read(&mut byte).await;
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
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.address)
    }
}
impl Drop for WebServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn response(content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
async fn create(
    model: Arc<ScriptModel>,
    mode: PermissionMode,
) -> (tempfile::TempDir, Session, Subscription) {
    create_with(model, mode, CoreOptions::default()).await
}
async fn create_with(
    model: Arc<ScriptModel>,
    mode: PermissionMode,
    options: CoreOptions,
) -> (tempfile::TempDir, Session, Subscription) {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("state/sessions")).unwrap());
    let (session, _) = Session::create(
        store,
        "fetch".into(),
        SessionSettings {
            non_interactive: false,
            tags: Default::default(),
            additional_directories: Vec::new(),
            plan_mode: false,
            pre_plan_approval_policy: None,
            plan_enabled: None,
            sandbox_enabled: None,
            cwd: temp.path().to_string_lossy().into(),
            name: None,
            model: "fake".into(),
            reasoning_effort: Default::default(),
            approval_policy: mode,
        },
        RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    temp.path().join("home").to_string_lossy().into(),
                ),
                (
                    "CANTER_HOME".into(),
                    temp.path().join("state").to_string_lossy().into(),
                ),
                ("API_KEY".into(), "fixture-secret-not-for-web".into()),
                ("HTTP_PROXY".into(), "http://127.0.0.1:1".into()),
            ]
            .into(),
            ..Default::default()
        },
        model,
        CoreOptions {
            context_window: 64 * 1024 * 1024,
            ..options
        },
    )
    .await
    .unwrap();
    let events = session.subscribe(0).await.unwrap();
    (temp, session, events)
}
fn input(review: bool) -> SessionInputParams {
    SessionInputParams {
        input: vec![UserInput::Text {
            text: "fetch fixtures".into(),
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
    until(events, |e| matches!(e, EventPayload::TurnCompleted { .. })).await
}

#[tokio::test]
async fn model_fetches_html_as_markdown_and_continues_with_a_fixed_tool_list() {
    let web = WebServer::new(|_| response("text/html; charset=utf-8", "<h1>Hello</h1><p>A <strong>bold</strong> link to <a href=\"/docs\">docs</a>.</p><script>secret script</script>")).await;
    let url = web.url("/page");
    let model = ScriptModel::new(vec![json!({"url":url})]);
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let results = model.results();
    assert_eq!(results, ["# Hello\n\nA **bold** link to [docs](/docs)."]);
    let completed = seen
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ItemCompleted { item, .. }
                if serde_json::to_value(item).unwrap()["type"] == "webFetch" =>
            {
                Some(serde_json::to_value(item).unwrap())
            }
            _ => None,
        })
        .expect("webFetch completion");
    assert_eq!(completed["url"], url);
    assert_eq!(completed["result"], results[0]);
    assert_eq!(completed["status"], "completed");
    {
        let requests = model.requests.lock().unwrap();
        let tools = serde_json::to_value(&requests[0].tools).unwrap();
        let names: Vec<_> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
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
        assert_eq!(tools, serde_json::to_value(&requests[1].tools).unwrap());
    }
    assert_eq!(web.requests.lock().unwrap().len(), 1);
    session.close().await.unwrap();
}

#[tokio::test]
async fn text_and_json_are_unchanged_and_other_types_are_only_placeholders() {
    let cases = [
        ("text/plain", "<b>literal</b>\n  spaces\n"),
        ("text/markdown", "# keep\n\n[link](/path)\n"),
        (
            "application/json; charset=utf-8",
            "{\n  \"html\": \"<b>literal</b>\"\n}\n",
        ),
        ("application/problem+json", "{\"error\":\"fixture\"}"),
        ("image/png", "binary-not-for-model"),
        ("application/pdf", "binary-not-for-model"),
        ("application/zip", "binary-not-for-model"),
    ];
    for (kind, body) in cases {
        let web = WebServer::new(move |_| response(kind, body)).await;
        let model = ScriptModel::new(vec![json!({"url":web.url("/")})]);
        let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
        session.input(input(false)).await.unwrap();
        finished(&mut events).await;
        let result = model.results().remove(0);
        if kind.starts_with("text/") || kind.contains("json") {
            assert_eq!(result, body, "{kind}");
        } else {
            assert_eq!(result.lines().count(), 1);
            assert!(
                result.contains(kind) && result.contains("20 bytes"),
                "{result}"
            );
            assert!(!result.contains(body));
        }
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn output_is_bounded_and_download_stops_at_the_byte_limit() {
    let web = WebServer::new(|request| {
        if request.starts_with("GET /large ") {
            return response("text/plain", &format!("HEAD{}TAIL", "汉".repeat(400_000)));
        }
        if request.starts_with("GET /declared ") {
            // The declared body never arrives; it must be rejected from headers alone.
            return "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 5242881\r\n\r\n".into();
        }
        let body = "x".repeat(5 * 1024 * 1024 + 1);
        // No terminating chunk or EOF: the downloader must stop on its own.
        format!("HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{body}\r\n", body.len())
    }).await;
    let model = ScriptModel::new(
        ["/large", "/declared", "/chunked"]
            .map(|path| json!({"url":web.url(path)}))
            .into(),
    );
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let results = model.results();
    assert!(results[0].starts_with("HEAD") && results[0].ends_with("TAIL"));
    assert!(results[0].contains("[truncated "));
    assert!(results[0].len() <= 32768 + 64);
    assert!(!results[0].contains('\u{fffd}'));
    for result in &results[1..] {
        assert_eq!(result, "Download exceeds 5242880 bytes; content omitted.");
    }
    let output = seen
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::ItemCompleted {
                item: Item::WebFetch { result, url, .. },
                ..
            } if url.ends_with("/large") => Some(result),
            _ => None,
        })
        .unwrap();
    assert!(output.len() <= 1024 * 1024);
    assert!(output.starts_with("HEAD") && output.ends_with("TAIL"));
    assert!(output.contains("[truncated ") && !output.contains('\u{fffd}'));
    session.close().await.unwrap();
}

async fn approve(session: &Session, request: &PendingRequest) {
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

#[tokio::test]
async fn permission_modes_and_session_grants_are_scoped_to_host_and_port() {
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
    ] {
        let web = WebServer::new(|_| response("text/plain", "ok")).await;
        let other = WebServer::new(|_| response("text/plain", "other")).await;
        let first = web.url("/first?query=full");
        let alias = format!("http://localhost:{}/alias", web.address.port());
        let different_port = other.url("/other");
        let urls = [
            first.clone(),
            web.url("/second"),
            alias.clone(),
            different_port.clone(),
        ];
        let model = ScriptModel::new(urls.iter().map(|url| json!({"url":url})).collect());
        let (_temp, session, mut events) = create(model, mode).await;
        session.input(input(false)).await.unwrap();
        let mut asked = Vec::new();
        loop {
            let seen = until(&mut events, |e| {
                matches!(
                    e,
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
            assert_eq!(approval.tool, "web_fetch");
            if asked.is_empty() {
                assert!(web.requests.lock().unwrap().is_empty());
            }
            asked.push(approval.arguments["url"].as_str().unwrap().to_owned());
            approve(&session, request).await;
        }
        assert_eq!(
            asked,
            if mode == PermissionMode::AllowAll {
                vec![]
            } else {
                vec![first, alias, different_port]
            }
        );
        assert_eq!(web.requests.lock().unwrap().len(), 3);
        assert_eq!(other.requests.lock().unwrap().len(), 1);
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn review_denies_fetch_without_approval_even_after_a_session_grant() {
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
    ] {
        let web = WebServer::new(|_| response("text/plain", "ok")).await;
        let model = ScriptModel::new(vec![json!({"url":web.url("/")})]);
        let review = ScriptModel::new(vec![
            json!({"url":web.url("/review")}),
            json!({"url":"file:///no-request"}),
        ]);
        model
            .scripts
            .lock()
            .unwrap()
            .extend(review.scripts.lock().unwrap().drain(..));
        let (_temp, session, mut events) = create(model.clone(), PermissionMode::AlwaysAsk).await;
        session.input(input(false)).await.unwrap();
        let seen = until(&mut events, |e| {
            matches!(e, EventPayload::RequestOpened { .. })
        })
        .await;
        let EventPayload::RequestOpened { request } = &seen.last().unwrap().payload else {
            unreachable!()
        };
        approve(&session, request).await;
        finished(&mut events).await;
        session
            .update(SessionUpdateParams {
                approval_policy: Some(mode),
                ..Default::default()
            })
            .await
            .unwrap();
        session.input(input(true)).await.unwrap();
        let seen = finished(&mut events).await;
        assert!(
            !seen
                .iter()
                .any(|e| matches!(e.payload, EventPayload::RequestOpened { .. }))
        );
        let results = model.results();
        assert_eq!(
            &results[1..],
            ["review is read-only", "review is read-only"]
        );
        assert_eq!(web.requests.lock().unwrap().len(), 1);
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn follows_only_same_host_and_port_redirects_with_a_limit_and_no_credentials() {
    let other = WebServer::new(|_| response("text/plain", "must not be reached")).await;
    let target = other.url("/destination");
    let target_for_response = target.clone();
    let web = WebServer::new(move |request| {
        let location = if request.starts_with("GET /relative ") { "/text".into() }
        else if request.starts_with("GET /loop ") { "/loop".into() }
        else if request.starts_with("GET /port ") { target_for_response.clone() }
        else if request.starts_with("GET /cross ") {
            let host = request.lines().find_map(|line| line.strip_prefix("host: ")).unwrap();
            format!("http://{}/destination", host.replace("127.0.0.1", "localhost"))
        } else { return response("text/plain", "final text"); };
        format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nSet-Cookie: fixture=private\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    }).await;
    let model = ScriptModel::new(
        ["/relative", "/cross", "/port", "/loop"]
            .map(|path| json!({"url":web.url(path)}))
            .into(),
    );
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    finished(&mut events).await;
    let results = model.results();
    assert_eq!(results[0], "final text");
    assert_eq!(
        results[1],
        format!(
            "Redirected to http://localhost:{}/destination",
            web.address.port()
        )
    );
    assert_eq!(results[2], format!("Redirected to {target}"));
    assert!(results[3].contains("redirect limit"), "{}", results[3]);
    assert!(other.requests.lock().unwrap().is_empty());
    {
        let captured = web.requests.lock().unwrap();
        assert_eq!(
            captured
                .iter()
                .filter(|r| r.starts_with("GET /loop "))
                .count(),
            6
        );
        assert!(!captured.iter().any(|r| r.contains("GET /destination ")));
        for request in captured.iter() {
            let lower = request.to_ascii_lowercase();
            assert!(
                lower.contains("user-agent: canter/0.1 web_fetch\r\n"),
                "{request}"
            );
            for forbidden in [
                "authorization:",
                "cookie:",
                "proxy-authorization:",
                "referer:",
                "fixture-secret-not-for-web",
            ] {
                assert!(!lower.contains(forbidden), "unexpected header: {forbidden}");
            }
        }
    }
    session.close().await.unwrap();
}

#[test]
fn the_default_web_fetch_timeout_is_thirty_seconds() {
    assert_eq!(
        CoreOptions::default().web_fetch_timeout,
        Duration::from_secs(30)
    );
}

#[tokio::test]
async fn bad_status_connection_and_timeout_are_tool_errors_and_the_turn_continues() {
    let web = WebServer::new(|request| {
        if request.starts_with("GET /slow ") {
            return String::new();
        }
        let status = if request.starts_with("GET /404 ") {
            "404 Not Found"
        } else {
            "503 Service Unavailable"
        };
        format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    })
    .await;
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let refused = format!("http://{}/", unused.local_addr().unwrap());
    drop(unused);
    let model = ScriptModel::new(
        [web.url("/404"), web.url("/503"), refused, web.url("/slow")]
            .map(|url| json!({"url":url}))
            .into(),
    );
    // Same timeout path as the 30 second default, without waiting for it.
    let (_temp, session, mut events) = create_with(
        model.clone(),
        PermissionMode::AllowAll,
        CoreOptions {
            web_fetch_timeout: Duration::from_secs(1),
            ..CoreOptions::default()
        },
    )
    .await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let results = model.results();
    for (result, expected) in results
        .iter()
        .zip(["404", "503", "connection", "timed out"])
    {
        assert!(result.contains(expected), "{result}");
    }
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(
                &e.payload,
                EventPayload::ItemCompleted {
                    item: Item::WebFetch {
                        status: ItemStatus::Failed,
                        ..
                    },
                    ..
                }
            ))
            .count(),
        4
    );
    assert!(seen.iter().any(|e| matches!(&e.payload, EventPayload::ItemCompleted { item: Item::AgentMessage { text, .. }, .. } if text == "continued")));
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
    );
    tokio::time::timeout(Duration::from_secs(5), web.closed.notified())
        .await
        .unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn interrupt_cancels_the_in_flight_response_body() {
    let web = WebServer::new(|_| {
        "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 1024\r\n\r\npartial".into()
    })
    .await;
    let model = ScriptModel::new(vec![json!({"url":web.url("/cancel")})]);
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    let turn_id = session.input(input(false)).await.unwrap().turn_id.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while web.requests.lock().unwrap().is_empty() {
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
    .expect("interrupt must cancel the HTTP body")
    .unwrap();
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)
    );
    assert!(seen.iter().any(|e| matches!(&e.payload, EventPayload::ItemCompleted { item: Item::WebFetch { status: ItemStatus::Failed, result, .. }, .. } if result.contains("cancelled"))));
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    tokio::time::timeout(Duration::from_secs(5), web.closed.notified())
        .await
        .unwrap();
    session.close().await.unwrap();
}

#[tokio::test]
async fn requires_explicit_http_scheme_and_rejects_url_credentials() {
    let web = WebServer::new(|_| response("text/plain", "must not be reached")).await;
    let urls = [
        format!("http:{}", web.address),
        format!("http://user:password@{}/", web.address),
        "file:///tmp/fixture".into(),
        "ftp://127.0.0.1/".into(),
    ];
    let model = ScriptModel::new(urls.map(|url| json!({"url":url})).into());
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    assert!(web.requests.lock().unwrap().is_empty());
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(
                &e.payload,
                EventPayload::ItemCompleted {
                    item: Item::WebFetch {
                        status: ItemStatus::Failed,
                        ..
                    },
                    ..
                }
            ))
            .count(),
        4
    );
    assert_eq!(model.results().len(), 4);
    session.close().await.unwrap();
}

#[tokio::test]
async fn binary_placeholders_report_declared_or_streamed_size_without_content() {
    let web = WebServer::new(|request| {
        if request.starts_with("GET /large ") {
            // No body is sent, even though the binary's declared size exceeds the download cap.
            "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: 9000000\r\n\r\n".into()
        } else {
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\nsecret\r\n0\r\n\r\n".into()
        }
    }).await;
    let model = ScriptModel::new(
        ["/large", "/stream"]
            .map(|path| json!({"url":web.url(path)}))
            .into(),
    );
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    finished(&mut events).await;
    assert_eq!(
        model.results(),
        [
            "[Content omitted: application/pdf, 9000000 bytes]",
            "[Content omitted: image/png, 6 bytes]"
        ]
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn deeply_nested_html_is_returned_raw_and_the_process_and_turn_continue() {
    for body in [
        "<div>".repeat(3_000),
        "<div>".repeat(1_048_000),
        "<div></missing>".repeat(3_000),
        "<div/>".repeat(3_000),
        format!("<svg>{}", "<source>".repeat(3_000)),
    ] {
        assert!(body.len() < 5 * 1024 * 1024);
        let raw_start = body[..80].to_owned();
        let raw_end = body[body.len() - 80..].to_owned();
        let truncated = body.len() > 32768;
        let web = WebServer::new(move |_| response("text/html", &body)).await;
        let model = ScriptModel::new(vec![json!({"url":web.url("/deep")})]);
        let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
        session.input(input(false)).await.unwrap();
        let seen = finished(&mut events).await;
        let results = model.results();
        assert!(results[0].starts_with(
            "[HTML not converted: HTML nesting limit exceeded (256); showing raw HTML.]\n\n"
        ));
        assert!(results[0].contains(&raw_start) && results[0].ends_with(&raw_end));
        assert!(results[0].len() <= 32768 + 64);
        assert_eq!(results[0].contains("[truncated "), truncated);
        let (result, status) = seen
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::ItemCompleted {
                    item: Item::WebFetch { result, status, .. },
                    ..
                } => Some((result, status)),
                _ => None,
            })
            .unwrap();
        assert_eq!(*status, ItemStatus::Completed);
        assert!(result.len() <= 1024 * 1024);
        assert!(result.contains(&raw_start) && result.ends_with(&raw_end));
        assert!(seen.iter().any(|e| matches!(&e.payload, EventPayload::ItemCompleted { item: Item::AgentMessage { text, .. }, .. } if text == "continued")));
        assert!(
            matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
        );
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn attribute_heavy_html_is_returned_raw_and_the_turn_continues() {
    let mut body = "<div".to_owned();
    for i in 0..20_000 {
        body.push_str(&format!(" a{i}=x"));
    }
    body.push_str(">content</div>");
    let web = WebServer::new(move |_| response("text/html", &body)).await;
    let model = ScriptModel::new(vec![json!({"url":web.url("/attributes")})]);
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let results = model.results();
    assert!(results[0].starts_with("[HTML not converted: HTML tag length limit exceeded (4096 bytes); showing raw HTML.]\n\n<div"));
    assert!(results[0].ends_with(">content</div>"));
    assert!(seen.iter().any(|e| matches!(
        &e.payload,
        EventPayload::ItemCompleted {
            item: Item::WebFetch {
                status: ItemStatus::Completed,
                ..
            },
            ..
        }
    )));
    assert!(seen.iter().any(|e| matches!(&e.payload, EventPayload::ItemCompleted { item: Item::AgentMessage { text, .. }, .. } if text == "continued")));
    session.close().await.unwrap();
}

#[tokio::test]
async fn html_at_the_depth_limit_still_converts() {
    let body = format!("{}visible{}", "<div>".repeat(256), "</div>".repeat(256));
    let web = WebServer::new(move |_| response("text/html", &body)).await;
    let model = ScriptModel::new(vec![json!({"url":web.url("/bounded")})]);
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    assert_eq!(model.results(), ["visible"]);
    assert!(seen.iter().any(|e| matches!(
        &e.payload,
        EventPayload::ItemCompleted {
            item: Item::WebFetch {
                status: ItemStatus::Completed,
                ..
            },
            ..
        }
    )));
    session.close().await.unwrap();
}

#[tokio::test]
async fn raw_text_comments_are_returned_as_raw_html() {
    let body = format!(
        "<script><!--</script>{}visible{}",
        "<div>".repeat(257),
        "</div>".repeat(257)
    );
    let web = WebServer::new(move |_| response("text/html", &body)).await;
    let model = ScriptModel::new(vec![json!({"url":web.url("/raw-text")})]);
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let results = model.results();
    assert!(results[0].starts_with("[HTML not converted:"));
    assert!(results[0].contains("showing raw HTML.]\n\n<script><!--</script>"));
    assert!(results[0].contains("visible"));
    assert!(seen.iter().any(|e| matches!(
        &e.payload,
        EventPayload::ItemCompleted {
            item: Item::WebFetch {
                status: ItemStatus::Completed,
                ..
            },
            ..
        }
    )));
    session.close().await.unwrap();
}

#[tokio::test]
async fn malformed_attribute_quotes_are_returned_as_raw_html() {
    let body = format!("<div data=x\">{}visible", "<div>".repeat(257));
    let web = WebServer::new(move |_| response("text/html", &body)).await;
    let model = ScriptModel::new(vec![json!({"url":web.url("/malformed-attribute")})]);
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let results = model.results();
    assert!(results[0].starts_with(
        "[HTML not converted: HTML tag contains nested markup; showing raw HTML.]\n\n<div data=x\">"
    ));
    assert!(results[0].ends_with("visible"));
    assert!(seen.iter().any(|e| matches!(
        &e.payload,
        EventPayload::ItemCompleted {
            item: Item::WebFetch {
                status: ItemStatus::Completed,
                ..
            },
            ..
        }
    )));
    assert!(seen.iter().any(|e| matches!(&e.payload, EventPayload::ItemCompleted { item: Item::AgentMessage { text, .. }, .. } if text == "continued")));
    session.close().await.unwrap();
}

#[tokio::test]
async fn legacy_script_comments_preserve_the_page_content_and_the_turn_continues() {
    let body = "<html><head><script><!--\nvar legacy = true;\n//--></script></head><body><h1>Legacy page content</h1></body></html>";
    let web = WebServer::new(move |_| response("text/html", body)).await;
    let model = ScriptModel::new(vec![json!({"url":web.url("/legacy")})]);
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    let seen = finished(&mut events).await;
    let results = model.results();
    assert!(results[0].starts_with("[HTML not converted:"));
    assert!(results[0].ends_with(body));
    assert!(seen.iter().any(|e| matches!(
        &e.payload,
        EventPayload::ItemCompleted {
            item: Item::WebFetch {
                status: ItemStatus::Completed,
                ..
            },
            ..
        }
    )));
    assert!(seen.iter().any(|e| matches!(&e.payload, EventPayload::ItemCompleted { item: Item::AgentMessage { text, .. }, .. } if text == "continued")));
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn encoded_responses_are_omitted_and_non_utf8_declarations_are_reported() {
    let web = WebServer::new(|request| {
        if request.starts_with("GET /encoding ") {
            // A server may ignore Accept-Encoding. No body or EOF will arrive.
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: gzip\r\nContent-Length: 100\r\n\r\n".into()
        } else if request.starts_with("GET /multiple ") {
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Encoding: identity\r\nContent-Encoding: br\r\nContent-Length: 100\r\n\r\n".into()
        } else if request.starts_with("GET /html ") {
            response("text/html; charset=GBK", "<h1>heading</h1>")
        } else if request.starts_with("GET /text ") {
            response("text/plain; CHARSET=\"ISO-8859-1\"", "<b>literal</b>")
        } else {
            response("text/plain; charset=UTF-8", "unchanged").replace("Content-Type:", "Content-Encoding: Identity\r\nContent-Type:")
        }
    }).await;
    let model = ScriptModel::new(
        ["/encoding", "/multiple", "/html", "/text", "/identity"]
            .map(|path| json!({"url":web.url(path)}))
            .into(),
    );
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AllowAll).await;
    session.input(input(false)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), finished(&mut events))
        .await
        .expect("encoded body must not be awaited");
    let results = model.results();
    for (result, encoding) in results[..2].iter().zip(["gzip", "br"]) {
        assert!(
            result.contains("Content-Encoding") && result.contains(encoding),
            "{result}"
        );
        assert_eq!(result.lines().count(), 1);
    }
    assert_eq!(results[2], "# heading");
    assert_eq!(results[3], "<b>literal</b>");
    assert_eq!(results[4], "unchanged");
    session.close().await.unwrap();
}

#[tokio::test]
async fn normalized_url_is_preserved_through_the_completed_fetch_item() {
    let web = WebServer::new(|_| response("text/plain", "ok")).await;
    let actual = web.url("/normalized");
    let original = actual.replacen("http://", "HTTP://", 1);
    let model = ScriptModel::new(vec![json!({"url":original})]);
    let (_temp, session, mut events) = create(model.clone(), PermissionMode::AlwaysAsk).await;
    session.input(input(false)).await.unwrap();
    let seen = until(&mut events, |e| {
        matches!(e, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = &seen.last().unwrap().payload else {
        unreachable!()
    };
    let RequestKind::Approval(approval) = &request.kind else {
        unreachable!()
    };
    assert_eq!(
        approval.arguments,
        json!({"url":original,"resolvedUrl":actual})
    );
    approve(&session, request).await;
    let seen = finished(&mut events).await;
    let item = seen
        .iter()
        .find_map(|event| match &event.payload {
            EventPayload::ItemCompleted {
                item: item @ Item::WebFetch { .. },
                ..
            } => Some(serde_json::to_value(item).unwrap()),
            _ => None,
        })
        .unwrap();
    assert_eq!(item["url"], original);
    assert_eq!(item["resolvedUrl"], actual);
    assert_eq!(model.results(), ["ok"]);
    assert_eq!(web.requests.lock().unwrap().len(), 1);
    session.close().await.unwrap();
}
