//! Real routing, OAuth, tools, private storage and public SSE, all on loopback.
use crate::{
    chatgpt_auth::{ChatgptAuth, test_support::Fake},
    config::Config,
    model_auth::ModelAuth,
};
use canter_core::{RuntimeContext, Session};
use canter_model::{ModelContext, ModelEvent, Request};
use canter_protocol::*;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, atomic::Ordering},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

mod reasoning;

struct Inference {
    endpoint: reqwest::Url,
    catalog: Arc<Mutex<Value>>,
    requests: tokio::sync::mpsc::UnboundedReceiver<(Value, String)>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Inference {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Inference {
    async fn new(replies: Vec<(u16, String)>) -> Self {
        Self::with_catalog(replies, json!({"models":[]})).await
    }

    async fn with_catalog(replies: Vec<(u16, String)>, catalog: Value) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap())
            .parse()
            .unwrap();
        let (sender, requests) = tokio::sync::mpsc::unbounded_channel();
        let replies = Arc::new(Mutex::new(VecDeque::from(replies)));
        let catalog = Arc::new(Mutex::new(catalog));
        let route_catalog = catalog.clone();
        let app = axum::Router::new()
            .route(
                "/v1/responses",
                axum::routing::post(
                    move |headers: axum::http::HeaderMap, axum::Json(body): axum::Json<Value>| {
                        let replies = replies.clone();
                        let sender = sender.clone();
                        async move {
                            sender
                                .send((body, headers["authorization"].to_str().unwrap().to_owned()))
                                .unwrap();
                            let (status, body) = replies
                                .lock()
                                .unwrap()
                                .pop_front()
                                .expect("unexpected inference");
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                [("content-type", "text/event-stream")],
                                body,
                            )
                        }
                    },
                ),
            )
            .route(
                "/v1/models",
                axum::routing::get(move || {
                    let catalog = route_catalog.clone();
                    async move { axum::Json(catalog.lock().unwrap().clone()) }
                }),
            );
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            endpoint,
            catalog,
            requests,
            task,
        }
    }
    async fn next(&mut self) -> (Value, String) {
        tokio::time::timeout(Duration::from_secs(10), self.requests.recv())
            .await
            .expect("no inference")
            .unwrap()
    }
}
fn completed(output: Value) -> (u16, String) {
    let items: String = output
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, item)| {
            format!(
                "data: {}\n\n",
                json!({"type":"response.output_item.done","output_index":index,"item":item})
            )
        })
        .collect();
    (
        200,
        format!(
            "{items}data: {}\n\n",
            json!({"type":"response.completed","response":{"status":"completed","output":[]}})
        ),
    )
}
async fn login(home: &std::path::Path, fake: &Fake, consent: bool) -> (ChatgptAuth, String) {
    let now = fake.state.lock().unwrap().now.clone();
    let auth = ChatgptAuth::for_test(
        home,
        fake.origin.clone(),
        Arc::new(move || now.load(Ordering::SeqCst)),
    );
    let attempt = auth.start_login(None).await.unwrap();
    assert!(fake.callback(&attempt).await.status().is_success());
    let account = attempt.wait().await.unwrap();
    auth.complete_login(&account.id, consent).await.unwrap();
    (auth, account.id)
}
fn config(home: &std::path::Path) -> Config {
    std::fs::write(home.join("config.json"),json!({
        "model":"chatgpt/test-model","providers":{"chatgpt":{"api":"responses","auth":"chatgpt","models":{"test-model":{"context_window":200000,"image":true}}}},
        "prompt_suggestions":false,"sandbox":false,"model_max_retries":1
    }).to_string()).unwrap();
    Config::load(home).unwrap()
}
fn input() -> SessionInputParams {
    serde_json::from_value(json!({"input":[{"type":"text","text":"Run the two commands"}],"mode":"queue","clientName":"test"})).unwrap()
}
async fn finished(events: &mut canter_core::Subscription) -> Vec<Event> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut seen = Vec::new();
        loop {
            let event = events.recv().await.unwrap().unwrap();
            let done = matches!(event.payload, EventPayload::TurnCompleted { .. });
            seen.push(event);
            if done {
                return seen;
            }
        }
    })
    .await
    .expect("turn did not finish")
}
async fn file_exists(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("tool did not reach marker");
}

#[tokio::test]
async fn configured_efforts_reach_responses_through_the_session_and_router() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, _) = login(&home, &fake, true).await;
    let mut inference = Inference::new(vec![completed(json!([])); 3]).await;
    let value = json!({
        "model":"configured/solver","prompt_suggestions":false,"sandbox":false,
        "providers":{"configured":{"api":"responses","auth":"chatgpt","models":{
            "solver":{"context_window":200000,"supported_reasoning_efforts":["medium","high","xhigh"],"default_reasoning_effort":"medium"}
        }}}
    });
    // The route name is arbitrary: reasoning capabilities must come from config.
    std::fs::write(home.join("config.json"), value.to_string()).unwrap();
    let config = Config::load(&home).unwrap();
    let model = config
        .model_client(Some(ModelAuth::for_test(
            home.clone(),
            auth,
            inference.endpoint.clone(),
        )))
        .unwrap();
    let store = Arc::new(canter_store::Store::new(home.join("sessions")).unwrap());
    let settings: SessionSettings = serde_json::from_value(json!({"cwd":root.path(),"model":"configured/solver","reasoningEffort":"medium","approvalPolicy":"allowAll"})).unwrap();
    let (session, _) = Session::create(
        store,
        "efforts".into(),
        settings,
        RuntimeContext {
            environment: BTreeMap::from([
                (
                    "HOME".into(),
                    root.path().join("home").to_string_lossy().into_owned(),
                ),
                ("CANTER_HOME".into(), home.to_string_lossy().into_owned()),
            ]),
            ..Default::default()
        },
        model,
        config.options(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    for effort in ["medium", "high", "xhigh"] {
        session
            .update(serde_json::from_value(json!({"reasoningEffort":effort})).unwrap())
            .await
            .unwrap();
        session.input(input()).await.unwrap();
        let (body, _) = inference.next().await;
        assert_eq!(body["model"], "solver");
        assert_eq!(body["reasoning"], json!({"summary":"auto","effort":effort}));
        let seen = finished(&mut events).await;
        assert!(
            matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
        );
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn two_tool_loops_pause_at_last_disconnect_replay_privately_and_resume() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::Builder::new()
        .prefix("canter-responses-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = root.path().join("state");
    let log = tempfile::NamedTempFile::new_in(root.path()).unwrap();
    let writer = log.reopen().unwrap();
    let _trace = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer.try_clone().unwrap())
            .finish(),
    );
    let fake = Fake::new().await;
    let (auth, _) = login(&home, &fake, false).await;
    let outputs:Vec<Value>=(0..2).map(|i| json!([
        {"type":"reasoning","id":format!("r{i}"),"summary":[],"encrypted_content":format!("private-test-cipher-{i}")},
        {"type":"message","id":format!("m{i}"),"role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"Working","annotations":[]}]},
        {"type":"function_call","id":format!("fc{i}"),"call_id":format!("call{i}"),"namespace":"canter","name":"shell","arguments":json!({"command":format!("touch started{i}; while [ ! -f release{i} ]; do /bin/sleep 0.02; done; touch finished{i}")}).to_string(),"status":"completed"}
    ])).collect();
    let mut inference = Inference::new(vec![
        completed(outputs[0].clone()),
        completed(outputs[1].clone()),
        completed(json!([])),
        completed(json!([])),
    ])
    .await;
    let config = config(&home);
    let model = config
        .model_client(Some(ModelAuth::for_test(
            home.clone(),
            auth,
            inference.endpoint.clone(),
        )))
        .unwrap();
    let store = Arc::new(canter_store::Store::new(home.join("sessions")).unwrap());
    let runtime = RuntimeContext {
        environment: BTreeMap::from([
            (
                "HOME".into(),
                root.path().join("home").to_string_lossy().into_owned(),
            ),
            ("CANTER_HOME".into(), home.to_string_lossy().into_owned()),
        ]),
        ..Default::default()
    };
    let settings:SessionSettings=serde_json::from_value(json!({"cwd":root.path(),"model":"chatgpt/test-model","approvalPolicy":"allowAll","reasoningEffort":"low"})).unwrap();
    let (session, _) = Session::create(
        store.clone(),
        "responses".into(),
        settings,
        runtime.clone(),
        model.clone(),
        config.options(),
    )
    .await
    .unwrap();
    let first = session.attach_client().await.unwrap();
    let second = session.attach_client().await.unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    let (_, token1) = inference.next().await;
    file_exists(&root.path().join("started0")).await;
    drop(first);
    std::fs::write(root.path().join("release0"), "").unwrap();
    inference.next().await;
    file_exists(&root.path().join("started1")).await;
    drop(second);
    std::fs::write(root.path().join("release1"), "").unwrap();
    file_exists(&root.path().join("finished1")).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), inference.requests.recv())
            .await
            .is_err()
    );
    let resumed = session.attach_client().await.unwrap();
    let (third, token3) = inference.next().await;
    assert!(token1 == token3);
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload,EventPayload::TurnCompleted {turn} if turn.status==TurnStatus::Completed)
    );
    let replayed: Vec<_> = third["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item.get("id").is_some())
        .cloned()
        .collect();
    assert_eq!(
        replayed,
        outputs
            .iter()
            .flat_map(|output| output.as_array().unwrap().clone())
            .collect::<Vec<_>>()
    );
    let history =
        serde_json::to_string(&store.model_history::<Value>("responses").unwrap()).unwrap();
    assert!(history.contains("private-test-cipher-0") && history.contains("private-test-cipher-1"));
    assert_eq!(
        std::fs::metadata(home.join("sessions/responses/model-history.jsonl"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    // Replay the same public journal through the real Unix-socket SSE route.
    let app = Arc::new(crate::http::App {
        reload: None,
        home: home.clone(),
        store: store.clone(),
        model: model.clone(),
        default_model: config.model.clone(),
        models: config.models.clone(),
        options: config.options(),
        mcp: Default::default(),
        lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
        sessions: Default::default(),
        session_locks: Default::default(),
        registry_changed: Default::default(),
        managed: Default::default(),
        pauses: Default::default(),
        draining: Default::default(),
        stopped: CancellationToken::new(),
    });
    app.sessions
        .lock()
        .await
        .insert("responses".into(), session.clone());
    let socket = root.path().join("server.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let router = crate::http::router(app);
    let http = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(socket)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    use eventsource_stream::Eventsource;
    let response = client
        .get("http://localhost/v1/events?session=responses&after=0&observe=true")
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let mut stream = response.bytes_stream().eventsource();
    let mut public = serde_json::to_string(&seen).unwrap();
    while let Some(event) = stream.next().await {
        let data = event.unwrap().data;
        let event: Event = serde_json::from_str(&data).unwrap();
        public.push_str(&data);
        if matches!(event.payload, EventPayload::TurnCompleted { .. }) {
            break;
        }
    }
    drop(stream);
    http.abort();
    let _ = http.await;
    public.push_str(&std::fs::read_to_string(log.path()).unwrap());
    assert!(
        !public.contains("private-test-cipher-")
            && !public.contains(token1.strip_prefix("Bearer ").unwrap())
    );
    // Reopening is outside the live turn: stored ciphertext is never replayed.
    drop(resumed);
    session.close().await.unwrap();
    drop(session);
    let (session, _) = Session::open(store, "responses".into(), runtime, model, config.options())
        .await
        .unwrap();
    let _attached = session.attach_client().await.unwrap();
    let mut events = session
        .subscribe(session.snapshot().await.session.last_event_seq)
        .await
        .unwrap();
    session.input(input()).await.unwrap();
    let (after_restart, _) = inference.next().await;
    assert!(!after_restart.to_string().contains("private-test-cipher-"));
    finished(&mut events).await;
    session.close().await.unwrap();
    fake.stop().await;
}

#[tokio::test]
async fn unauthorized_recovers_once_with_real_account_refresh() {
    for rejected_twice in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("state");
        let fake = Fake::new().await;
        let (auth, _) = login(&home, &fake, true).await;
        let unauthorized = (401, json!({"detail":"private-test-error-body"}).to_string());
        let mut inference = Inference::new(vec![
            unauthorized.clone(),
            if rejected_twice {
                unauthorized
            } else {
                completed(json!([]))
            },
        ])
        .await;
        let model = config(&home)
            .model_client(Some(ModelAuth::for_test(
                home,
                auth,
                inference.endpoint.clone(),
            )))
            .unwrap();
        let events: Vec<_> = model
            .stream_with_context(
                Request {
                    model: "chatgpt/test-model".into(),
                    ..Default::default()
                },
                CancellationToken::new(),
                ModelContext {
                    foreground: tokio::sync::watch::channel(true).1,
                    credential_binding: Default::default(),
                },
            )
            .collect()
            .await;
        let (_, first) = inference.next().await;
        let (_, second) = inference.next().await;
        assert!(first != second);
        assert!(inference.requests.try_recv().is_err());
        assert_eq!(
            fake.state
                .lock()
                .unwrap()
                .grants
                .iter()
                .filter(|grant| grant
                    .get("grant_type")
                    .is_some_and(|kind| kind == "refresh_token"))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .any(|event| matches!(event, Ok(ModelEvent::Finished(_)))),
            !rejected_twice
        );
        assert!(!format!("{events:?}").contains("private-test-error-body"));
        fake.stop().await;
    }
}

#[tokio::test]
async fn missing_plan_permission_and_expired_login_have_actionable_private_errors() {
    for plan_missing in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("state");
        let fake = Fake::new().await;
        if plan_missing {
            fake.state.lock().unwrap().scopes = "openid profile email".into();
        }
        let (auth, _) = login(&home, &fake, true).await;
        if !plan_missing {
            let mut state = fake.state.lock().unwrap();
            state.now.store(
                crate::chatgpt_auth::test_support::NOW + 3600,
                Ordering::SeqCst,
            );
            state.error = Some((400, "invalid_grant".into()));
        }
        let mut inference = Inference::new(vec![]).await;
        let model = config(&home)
            .model_client(Some(ModelAuth::for_test(
                home,
                auth,
                inference.endpoint.clone(),
            )))
            .unwrap();
        let events: Vec<_> = model
            .stream(
                Request {
                    model: "chatgpt/test-model".into(),
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .collect()
            .await;
        assert_eq!(events.len(), 1);
        let error = events[0].as_ref().unwrap_err();
        assert_eq!(
            error.kind,
            if plan_missing {
                canter_model::ErrorKind::ChatgptPlanNotAuthorized
            } else {
                canter_model::ErrorKind::ChatgptNeedsLogin
            }
        );
        assert!(error.to_string().contains("canter auth login chatgpt"));
        assert!(!format!("{events:?}").contains("invalid_grant"));
        assert!(inference.requests.try_recv().is_err());
        fake.stop().await;
    }
}

#[tokio::test]
async fn disconnect_also_gates_retries_and_waiting_requests_can_be_cancelled() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, _) = login(&home, &fake, false).await;
    let mut inference = Inference::new(vec![
        (503, json!({"detail":"private retry failure"}).to_string()),
        completed(json!([])),
    ])
    .await;
    let model = config(&home)
        .model_client(Some(ModelAuth::for_test(
            home,
            auth,
            inference.endpoint.clone(),
        )))
        .unwrap();
    let (presence, foreground) = tokio::sync::watch::channel(true);
    let context = ModelContext {
        foreground,
        credential_binding: Default::default(),
    };
    let request = Request {
        model: "chatgpt/test-model".into(),
        ..Default::default()
    };
    let mut stream =
        model.stream_with_context(request.clone(), CancellationToken::new(), context.clone());
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelEvent::Retrying { .. }))
    ));
    inference.next().await;
    presence.send_replace(false);
    assert!(
        tokio::time::timeout(Duration::from_millis(600), stream.next())
            .await
            .is_err()
    );
    assert!(inference.requests.try_recv().is_err());
    presence.send_replace(true);
    let events: Vec<_> = stream.collect().await;
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Ok(ModelEvent::Finished(_))))
    );
    inference.next().await;
    presence.send_replace(false);
    let cancel = CancellationToken::new();
    let mut stream = model.stream_with_context(request, cancel.clone(), context);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .is_err()
    );
    cancel.cancel();
    assert!(stream.next().await.is_none());
    assert!(inference.requests.try_recv().is_err());
    fake.stop().await;
}

#[tokio::test]
async fn unverified_catalog_model_uses_medium_and_retired_model_restores_with_last_details() {
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let home = root.path().join("state");
    let fake = Fake::new().await;
    let (auth, _) = login(&home, &fake, true).await;
    let mut inference = Inference::with_catalog(
        vec![completed(json!([])), completed(json!([]))],
        json!({"models":[{"slug":"discovered","display_name":"Discovered","visibility":"list"}]}),
    )
    .await;
    let source = ModelAuth::for_test(home.clone(), auth.clone(), inference.endpoint.clone());
    source.catalog().account_changed(auth).await;
    let config = config(&home);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (models, _) = source.catalog().apply(
                config.catalog_provider(),
                config.models.clone(),
                config.options(),
            );
            if models.contains(&"chatgpt/discovered".into()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (_, options) = source.catalog().apply(
        config.catalog_provider(),
        config.models.clone(),
        config.options(),
    );
    assert_eq!(options.limits("chatgpt/discovered").context_window, 128000);
    assert!(!options.image_models.contains(&"chatgpt/discovered".into()));
    let model = config.model_client(Some(source.clone())).unwrap();
    let store = Arc::new(canter_store::Store::new(home.join("sessions")).unwrap());
    let runtime = RuntimeContext {
        environment: BTreeMap::from([
            ("HOME".into(), root.path().to_string_lossy().into_owned()),
            ("CANTER_HOME".into(), home.to_string_lossy().into_owned()),
        ]),
        ..Default::default()
    };
    let settings: SessionSettings = serde_json::from_value(json!({
        "cwd":root.path(),"model":"chatgpt/discovered","approvalPolicy":"allowAll",
        "reasoningEffort":"medium"
    }))
    .unwrap();
    let (session, _) = Session::create(
        store.clone(),
        "catalog".into(),
        settings,
        runtime.clone(),
        model.clone(),
        options,
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input()).await.unwrap();
    let (body, _) = inference.next().await;
    assert_eq!(body["model"], "discovered");
    assert_eq!(
        body["reasoning"],
        json!({"summary":"auto","effort":"medium"})
    );
    finished(&mut events).await;
    session.close().await.unwrap();
    drop(session);

    *inference.catalog.lock().unwrap() = json!({"models":[]});
    source
        .catalog()
        .account_changed(source.get().await.unwrap())
        .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (models, _) = source.catalog().apply(
                config.catalog_provider(),
                config.models.clone(),
                config.options(),
            );
            if !models.contains(&"chatgpt/discovered".into()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let (models, options) = source.catalog().apply(
        config.catalog_provider(),
        config.models.clone(),
        config.options(),
    );
    assert!(!models.contains(&"chatgpt/discovered".into()));
    assert_eq!(options.limits("chatgpt/discovered").context_window, 128000);
    let (session, _) = Session::open(store, "catalog".into(), runtime, model, options)
        .await
        .unwrap();
    let mut events = session
        .subscribe(session.snapshot().await.session.last_event_seq)
        .await
        .unwrap();
    session.input(input()).await.unwrap();
    let (body, _) = inference.next().await;
    assert_eq!(body["model"], "discovered");
    assert_eq!(
        body["reasoning"],
        json!({"summary":"auto","effort":"medium"})
    );
    finished(&mut events).await;
    session.close().await.unwrap();
    fake.stop().await;
}
