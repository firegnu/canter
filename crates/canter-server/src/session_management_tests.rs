use super::*;

#[path = "registry_tests.rs"]
pub(crate) mod registry;

struct Harness {
    app: Arc<App>,
    client: reqwest::Client,
    server: tokio::task::JoinHandle<()>,
    root: tempfile::TempDir,
}

impl Harness {
    async fn new() -> Self {
        Self::with_model(Arc::new(PanicOnce {
            panicked: AtomicBool::new(true),
            reached: Notify::new(),
        }))
        .await
    }

    async fn with_model(model: Arc<dyn Model>) -> Self {
        Self::with_options(model, CoreOptions::default()).await
    }

    async fn with_options(model: Arc<dyn Model>, options: CoreOptions) -> Self {
        let root = tempfile::Builder::new()
            .prefix("canter-sessions-")
            .tempdir_in("/tmp")
            .unwrap();
        let home = root.path().join("state");
        let app = Arc::new(App {
            reload: None,
            home: home.clone(),
            store: Arc::new(Store::new(home.join("sessions")).unwrap()),
            model,
            default_model: "fake-model".into(),
            models: vec!["fake-model".into()],
            options,
            mcp: Default::default(),
            lsp: canter_tools::lsp::LspService::new(Default::default()).unwrap(),
            sessions: Default::default(),
            session_locks: Default::default(),
            registry_changed: Default::default(),
            #[cfg(test)]
            pauses: Default::default(),
            draining: Default::default(),
            stopped: CancellationToken::new(),
        });
        let socket = root.path().join("s");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let served = app.clone();
        let server =
            tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
        let client = reqwest::Client::builder()
            .no_proxy()
            .unix_socket(socket)
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            app,
            client,
            server,
            root,
        }
    }

    async fn create(&self) -> String {
        self.client
            .post("http://localhost/v1/folderTrust")
            .json(&json!({"cwd": self.root.path()}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        let response: SessionCreateResponse = self.client.post("http://localhost/v1/sessions")
            .json(&json!({"cwd": self.root.path(), "env": {}, "model": "fake-model", "approvalPolicy": "alwaysAsk", "clientName": "test"}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        format!("http://localhost/v1/sessions/{}", response.session.id)
    }

    async fn read(&self, url: &str) -> SessionReadResponse {
        self.client
            .get(url)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn patch(&self, url: &str, params: serde_json::Value) -> SessionUpdateResponse {
        self.client
            .patch(url)
            .json(&params)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn stop(self) {
        for session in self.app.sessions.lock().await.values() {
            session.close().await.unwrap();
        }
        self.server.abort();
        assert!(self.server.await.unwrap_err().is_cancelled());
    }
}

struct PausedModel(Option<canter_model::ToolCall>);
impl Model for PausedModel {
    fn stream(&self, _: Request, cancel: CancellationToken) -> ModelStream<'_> {
        let call = self.0.clone();
        Box::pin(async_stream::stream! {
            yield Ok(ModelEvent::TextDelta { item_id: "text".into(), delta: "working".into() });
            if let Some(call) = call {
                yield Ok(ModelEvent::ToolCall(call));
                yield Ok(ModelEvent::Finished(FinishReason::ToolCalls));
            } else {
                cancel.cancelled().await;
            }
        })
    }
}

#[tokio::test]
async fn trash_refuses_work_approval_answers_queued_input_and_btw() {
    for kind in ["work", "approval", "answer", "queued", "btw"] {
        let call = match kind {
            "approval" => Some(canter_model::ToolCall { id: "write".into(), name: "write".into(), arguments: json!({"path":"file.txt", "content":"changed"}).to_string() }),
            "answer" => Some(canter_model::ToolCall { id: "ask".into(), name: "ask_user".into(), arguments: json!({"question":"Which way?", "options":[{"label":"A", "description":"First"}, {"label":"B", "description":"Second"}]}).to_string() }),
            _ => None,
        };
        let test = Harness::with_model(Arc::new(PausedModel(call))).await;
        let url = test.create().await;
        let mut btw = None;
        if kind == "btw" {
            btw = Some(
                test.client
                    .post(format!("{url}/btw"))
                    .json(&json!({"question":"side question"}))
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap(),
            );
        } else {
            let input = json!({"input":[{"type":"text","text":"hello"}], "clientName":"test", "mode":"queue"});
            test.client
                .post(format!("{url}/input"))
                .json(&input)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
            if kind == "queued" {
                test.client
                    .post(format!("{url}/input"))
                    .json(&input)
                    .send()
                    .await
                    .unwrap()
                    .error_for_status()
                    .unwrap();
            }
            if matches!(kind, "approval" | "answer") {
                tokio::time::timeout(Duration::from_secs(3), async {
                    while test.read(&url).await.session.status != SessionStatus::WaitingApproval {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
            }
        }
        let response = test
            .client
            .post(format!("{url}/trash"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT, "{kind}");
        assert!(
            !response
                .json::<ErrorResponse>()
                .await
                .unwrap()
                .message
                .is_empty()
        );
        assert_ne!(
            test.read(&url).await.session.status,
            SessionStatus::Closed,
            "{kind}"
        );
        drop(btw);
        test.stop().await;
    }
}

#[tokio::test]
async fn conditional_names_preserve_manual_rename_across_resume() {
    let test = Harness::new().await;
    let url = test.create().await;
    test.patch(&url, json!({"name": "automatic", "expectedName": ""}))
        .await;
    test.patch(&url, json!({"name": "manual"})).await;
    assert_eq!(
        test.app.store.list().unwrap()[0].name.as_deref(),
        Some("manual")
    );
    let before = test.read(&url).await;
    let ignored = test
        .patch(&url, json!({"name": "", "expectedName": "automatic"}))
        .await;
    assert_eq!(
        test.read(&url).await.session.settings.name.as_deref(),
        Some("manual")
    );
    assert_eq!(
        ignored.event_seq, before.event_seq,
        "a stale automatic update is a successful no-op"
    );
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .post(format!("{url}/resume"))
        .json(&json!({"env": {}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.patch(&url, json!({"name": "late automatic", "expectedName": ""}))
        .await;
    assert_eq!(
        test.read(&url).await.session.settings.name.as_deref(),
        Some("manual")
    );
    test.patch(&url, json!({"name": ""})).await;
    assert_eq!(test.read(&url).await.session.settings.name, None);
    test.patch(&url, json!({"name": "next", "expectedName": ""}))
        .await;
    test.patch(&url, json!({"name": "", "expectedName": "next"}))
        .await;
    assert_eq!(test.read(&url).await.session.settings.name, None);
    test.stop().await;
}

#[tokio::test]
async fn trash_moves_the_whole_session_and_hides_it_from_readers_and_lists() {
    use std::os::unix::fs::PermissionsExt;
    let test = Harness::new().await;
    let url = test.create().await;
    let id = url.rsplit('/').next().unwrap();
    let directory = test.app.store.session_dir(id).unwrap();
    std::fs::write(directory.join("attachment"), b"keep this").unwrap();
    // Populate the summary cache before moving the directory.
    assert_eq!(test.app.store.list().unwrap().len(), 1);
    test.client
        .post(format!("{url}/trash"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert!(!directory.exists());
    let trash = test.app.home.join("trash");
    assert_eq!(
        std::fs::metadata(&trash).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::read(trash.join(id).join("attachment")).unwrap(),
        b"keep this"
    );
    assert!(test.app.sessions.lock().await.is_empty());
    assert!(
        Store::new(test.app.home.join("sessions"))
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
    let info: InfoResponse = test
        .client
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(info.sessions.is_empty());
    assert!(
        info.features
            .unwrap()
            .iter()
            .any(|feature| feature == "sessionTrash")
    );
    let list: SessionListResponse = test
        .client
        .get("http://localhost/v1/sessions")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(list.data.is_empty());
    for request in [
        test.client.get(&url),
        test.client
            .post(format!("{url}/resume"))
            .json(&json!({"env": {}})),
        test.client.patch(&url).json(&json!({"name": "resurrect"})),
        test.client
            .get(format!("http://localhost/v1/events?session={id}&after=0")),
    ] {
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            response.json::<ErrorResponse>().await.unwrap().code,
            ErrorCode::SessionNotFound
        );
    }
    let usage: UsageResponse = test
        .client
        .get("http://localhost/v1/usage")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(usage.data.is_empty());
    assert_eq!(usage.total, UsageRow::default());
    let stats: StatsListResponse = test
        .client
        .get("http://localhost/v1/stats?all=true")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(stats.data.is_empty());
    test.stop().await;
}

#[tokio::test]
async fn trash_refuses_attached_clients_without_closing_the_session() {
    let test = Harness::new().await;
    let url = test.create().await;
    let id = url.rsplit('/').next().unwrap();
    let reader = test
        .client
        .get(format!("http://localhost/v1/events?session={id}&after=0"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let response = test
        .client
        .post(format!("{url}/trash"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        response
            .json::<ErrorResponse>()
            .await
            .unwrap()
            .message
            .contains("attached")
    );
    assert_eq!(test.read(&url).await.session.status, SessionStatus::Idle);
    assert!(test.app.store.session_dir(id).unwrap().exists());
    drop(reader);
    test.stop().await;
}

#[tokio::test]
async fn trash_collision_preserves_both_directories_and_does_not_close() {
    let test = Harness::new().await;
    let url = test.create().await;
    let id = url.rsplit('/').next().unwrap();
    let target = test.app.home.join("trash").join(id);
    std::fs::create_dir_all(&target).unwrap();
    // An empty directory matters too: ordinary rename would overwrite it.
    let before = test.read(&url).await;
    let response = test
        .client
        .post(format!("{url}/trash"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(
        response
            .json::<ErrorResponse>()
            .await
            .unwrap()
            .message
            .contains("already exists")
    );
    assert_eq!(test.read(&url).await, before);
    assert_eq!(std::fs::read_dir(target).unwrap().count(), 0);
    test.stop().await;
}

#[tokio::test]
async fn trash_refuses_pending_goal_continuation() {
    let test = Harness::new().await;
    let url = test.create().await;
    let id = url.rsplit('/').next().unwrap();
    let session = test.app.sessions.lock().await[id].clone();
    let mut events = session
        .subscribe(session.snapshot().await.event_seq)
        .await
        .unwrap();
    session
        .set_goal(GoalSetParams {
            objective: "Keep working".into(),
            budget_usd: Some(0.0),
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(event) = events.recv().await.unwrap() {
            if matches!(
                event.payload,
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
    // Test the Core boundary immediately at the end-of-turn notification,
    // before the ten-millisecond goal continuation delay expires.
    let error = session.close_for_trash().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Conflict);
    assert!(error.message.contains("goal"));
    test.stop().await;
}

#[tokio::test]
async fn trash_waits_for_an_accepted_resume_then_leaves_no_writable_session() {
    use std::io::Write;
    let test = Harness::new().await;
    let url = test.create().await;
    let id = url.rsplit('/').next().unwrap();
    let stale = test.app.sessions.lock().await[id].clone();
    // Stall the real trust read after resume has found the loaded session.
    let trust = test.app.home.join("trusted_folders.json");
    let contents = std::fs::read(&trust).unwrap();
    std::fs::remove_file(&trust).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&trust)
            .status()
            .unwrap()
            .success()
    );
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let (release, released) = std::sync::mpsc::channel();
    let writer = tokio::task::spawn_blocking(move || {
        let mut pipe = std::fs::OpenOptions::new().write(true).open(trust).unwrap();
        ready.send(()).unwrap();
        released.recv().unwrap();
        pipe.write_all(&contents).unwrap();
    });
    let request = test
        .client
        .post(format!("{url}/resume"))
        .json(&json!({"env": {}}));
    let resumed = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), waiting)
        .await
        .unwrap()
        .unwrap();
    let request = test.client.post(format!("{url}/trash"));
    let mut deleted = tokio::spawn(async move { request.send().await.unwrap() });
    let early = tokio::time::timeout(Duration::from_millis(100), &mut deleted).await;
    release.send(()).unwrap();
    writer.await.unwrap();
    let resumed = resumed.await.unwrap();
    let raced = early.is_ok();
    let deleted = match early {
        Ok(reply) => reply.unwrap(),
        Err(_) => deleted.await.unwrap(),
    };
    assert!(
        !raced,
        "deletion must not overtake a resume that is still applying its opening settings"
    );
    assert_eq!(resumed.status(), StatusCode::OK);
    assert_ne!(
        resumed
            .json::<SessionResumeResponse>()
            .await
            .unwrap()
            .session
            .status,
        SessionStatus::Closed
    );
    assert_eq!(deleted.status(), StatusCode::NO_CONTENT);
    assert!(stale.attach_client().await.is_none());
    assert!(
        stale
            .update(SessionUpdateParams {
                name: Some("too late".into()),
                ..Default::default()
            })
            .await
            .is_err()
    );
    assert!(
        stale
            .input(
                serde_json::from_value(
                    json!({"input":[{"type":"text","text":"too late"}], "clientName":"test"})
                )
                .unwrap()
            )
            .await
            .is_err()
    );
    assert!(!test.app.store.session_dir(id).unwrap().exists());
    test.stop().await;
}

#[tokio::test]
async fn trash_racing_event_attachment_and_input_has_one_winner() {
    for operation in ["events", "input"] {
        let test = Harness::with_model(Arc::new(PausedModel(None))).await;
        for _ in 0..12 {
            let url = test.create().await;
            let id = url.rsplit('/').next().unwrap();
            let other = if operation == "events" {
                test.client
                    .get(format!("http://localhost/v1/events?session={id}&after=0"))
            } else {
                test.client
                    .post(format!("{url}/input"))
                    .json(&json!({"input":[{"type":"text","text":"hello"}], "clientName":"test"}))
            };
            let (deleted, other) = tokio::join!(
                test.client.post(format!("{url}/trash")).send(),
                other.send()
            );
            let deleted = deleted.unwrap();
            let other = other.unwrap();
            if deleted.status() == StatusCode::NO_CONTENT {
                assert!(
                    !other.status().is_success(),
                    "{operation} attached to a deleted session"
                );
                assert!(!test.app.store.session_dir(id).unwrap().exists());
                assert_eq!(
                    test.client.get(&url).send().await.unwrap().status(),
                    StatusCode::NOT_FOUND
                );
            } else {
                assert_eq!(deleted.status(), StatusCode::CONFLICT);
                assert!(other.status().is_success());
            }
        }
        test.stop().await;
    }
}
