use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{
    AssistantContent, ErrorKind, FinishReason, Message, Model, ModelError, ModelEvent, ModelStream,
    Request, ToolCall, Usage,
};
use canter_protocol::*;
use canter_store::Store;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

enum Step {
    Event(ModelEvent),
    Error(ErrorKind),
    Wait(Arc<Notify>),
}
struct ScriptModel {
    scripts: Mutex<VecDeque<Vec<Step>>>,
    requests: Mutex<Vec<Request>>,
}
impl ScriptModel {
    fn new(scripts: Vec<Vec<Step>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
}
impl Model for ScriptModel {
    fn stream(&self, request: Request, cancel: CancellationToken) -> ModelStream<'_> {
        self.requests.lock().unwrap().push(request);
        let steps = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected model request");
        Box::pin(async_stream::stream! {
            for step in steps {
                match step {
                    Step::Event(event) => yield Ok(event),
                    Step::Error(kind) => yield Err(ModelError { kind, status: None, retry_after: None, retries: 0 }),
                    Step::Wait(notify) => tokio::select! {
                        _ = cancel.cancelled() => break,
                        _ = notify.notified() => {},
                    },
                }
            }
        })
    }
}
fn text(value: &str) -> Step {
    Step::Event(ModelEvent::TextDelta {
        item_id: "text".into(),
        delta: value.into(),
    })
}
fn finish() -> Step {
    Step::Event(ModelEvent::Finished(FinishReason::Stop))
}
fn read_fixture(id: &str) -> Vec<Step> {
    vec![
        Step::Event(ModelEvent::ToolCall(ToolCall {
            id: id.into(),
            name: "read".into(),
            arguments: r#"{"path":"fixture.txt"}"#.into(),
        })),
        Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
    ]
}
fn input(value: &str) -> SessionInputParams {
    SessionInputParams {
        review: None,
        input: vec![UserInput::Text { text: value.into() }],
        mode: InputMode::Queue,
        client_name: "test".into(),
    }
}
async fn create(
    model: Arc<dyn Model>,
    options: CoreOptions,
) -> (tempfile::TempDir, Arc<Store>, Session) {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let (session, _) = Session::create(
        store.clone(),
        "s1".into(),
        SessionSettings {
            additional_directories: Vec::new(),
            plan_mode: false,
            pre_plan_approval_policy: None,
            plan_enabled: None,
            cwd: temp.path().to_string_lossy().into(),
            model: "fake".into(),
            reasoning_effort: Default::default(),
            name: None,
            approval_policy: PermissionMode::AskForExecution,
        },
        RuntimeContext::default(),
        model,
        options,
    )
    .await
    .unwrap();
    (temp, store, session)
}
async fn until(events: &mut Subscription, predicate: impl Fn(&EventPayload) -> bool) -> Vec<Event> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut seen = Vec::new();
        loop {
            let event = events
                .recv()
                .await
                .unwrap()
                .expect("subscription ended early");
            let found = predicate(&event.payload);
            seen.push(event);
            if found {
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
async fn review_turn_is_readonly_even_with_session_grants_and_restores_permissions() {
    use serde_json::json;
    for mode in [
        PermissionMode::AllowAll,
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
    ] {
        for granted in [false, true] {
            let calls = || {
                vec![
                    ("write", json!({"path":"written.txt", "content":"written"})),
                    (
                        "edit",
                        json!({"path":"fixture.txt", "edits":[{"oldText":"original", "newText":"edited"}]}),
                    ),
                    ("shell", json!({"command":"touch shell-ran"})),
                    ("read", json!({"path":"fixture.txt"})),
                    ("shell", json!({"command":"echo allowed"})),
                ]
            };
            let tools = || {
                let mut steps: Vec<_> = calls()
                    .into_iter()
                    .enumerate()
                    .map(|(i, (name, args))| {
                        Step::Event(ModelEvent::ToolCall(ToolCall {
                            id: format!("call-{i}"),
                            name: name.into(),
                            arguments: args.to_string(),
                        }))
                    })
                    .collect();
                steps.push(Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)));
                steps
            };
            let mut scripts = Vec::new();
            if granted {
                scripts.extend([tools(), vec![finish()]]);
            }
            scripts.extend([
                tools(),
                vec![text("No issues found. Safe to merge."), finish()],
                tools(),
                vec![finish()],
            ]);
            let model = ScriptModel::new(scripts);
            let (temp, _, session) = create(model.clone(), CoreOptions::default()).await;
            std::fs::write(temp.path().join("fixture.txt"), "original").unwrap();
            let mut events = session.subscribe(0).await.unwrap();
            if granted {
                session
                    .update(SessionUpdateParams {
                        approval_policy: Some(PermissionMode::AlwaysAsk),
                        ..Default::default()
                    })
                    .await
                    .unwrap();
                session.input(input("grant tools")).await.unwrap();
                loop {
                    let batch = until(&mut events, |e| {
                        matches!(
                            e,
                            EventPayload::RequestOpened { .. } | EventPayload::TurnCompleted { .. }
                        )
                    })
                    .await;
                    match &batch.last().unwrap().payload {
                        EventPayload::RequestOpened { request } => {
                            session
                                .resolve(
                                    &request.id,
                                    RequestResolveParams {
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
                        _ => break,
                    }
                }
                std::fs::remove_file(temp.path().join("written.txt")).unwrap();
                std::fs::remove_file(temp.path().join("shell-ran")).unwrap();
                std::fs::write(temp.path().join("fixture.txt"), "original").unwrap();
            }
            session
                .update(SessionUpdateParams {
                    approval_policy: Some(mode),
                    ..Default::default()
                })
                .await
                .unwrap();
            let params = serde_json::from_value(json!({
                "input":[{"type":"text","text":"Review this diff"}],
                "clientName":"test", "review":"Review: uncommitted changes"
            }))
            .unwrap();
            session.input(params).await.unwrap();
            let batch = until(&mut events, |e| {
                matches!(
                    e,
                    EventPayload::RequestOpened { .. } | EventPayload::TurnCompleted { .. }
                )
            })
            .await;
            assert!(
                !batch
                    .iter()
                    .any(|e| matches!(e.payload, EventPayload::RequestOpened { .. })),
                "review must not ask for approval"
            );
            assert!(
                !temp.path().join("written.txt").exists(),
                "review wrote a file: {mode:?}, granted={granted}"
            );
            assert!(!temp.path().join("shell-ran").exists());
            assert_eq!(
                std::fs::read_to_string(temp.path().join("fixture.txt")).unwrap(),
                "original"
            );
            {
                let requests = model.requests.lock().unwrap();
                let messages = &requests.last().unwrap().messages;
                for i in 0..3 {
                    assert!(messages.iter().any(|m| matches!(m, Message::ToolResult { tool_call_id, text } if tool_call_id == &format!("call-{i}") && text.contains("review is read-only"))));
                }
                assert!(messages.iter().any(|m| matches!(m, Message::ToolResult { tool_call_id, text } if tool_call_id == "call-3" && text.contains("original"))));
                assert!(messages.iter().any(|m| matches!(m, Message::ToolResult { tool_call_id, text } if tool_call_id == "call-4" && text.contains("allowed"))));
            }
            assert_eq!(
                session.snapshot().await.session.settings.approval_policy,
                mode
            );
            session.input(input("make the changes now")).await.unwrap();
            let mut approvals = 0;
            loop {
                let batch = until(&mut events, |e| {
                    matches!(
                        e,
                        EventPayload::RequestOpened { .. } | EventPayload::TurnCompleted { .. }
                    )
                })
                .await;
                match &batch.last().unwrap().payload {
                    EventPayload::RequestOpened { request } => {
                        approvals += 1;
                        session
                            .resolve(
                                &request.id,
                                RequestResolveParams {
                                    plan: None,
                                    feedback: None,
                                    decision: Some(ApprovalDecision::Accept),
                                    answer: None,
                                    client_name: "test".into(),
                                },
                            )
                            .await
                            .unwrap();
                    }
                    _ => break,
                }
            }
            assert_eq!(
                approvals,
                if granted {
                    0
                } else {
                    match mode {
                        PermissionMode::AlwaysAsk => 3,
                        PermissionMode::AskForExecution => 1,
                        _ => 0,
                    }
                }
            );
            assert!(temp.path().join("written.txt").exists());
            assert!(temp.path().join("shell-ran").exists());
            assert_eq!(
                std::fs::read_to_string(temp.path().join("fixture.txt")).unwrap(),
                "edited"
            );
            session.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn review_rejects_changes_before_reading_or_preparing_the_target() {
    let model = ScriptModel::new(vec![
        vec![
            Step::Event(ModelEvent::ToolCall(ToolCall {
                id: "missing-edit".into(),
                name: "edit".into(),
                arguments: r#"{"path":"missing.txt","edits":[{"oldText":"old","newText":"new"}]}"#
                    .into(),
            })),
            Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
        ],
        vec![finish()],
    ]);
    let (_temp, _, session) = create(model.clone(), CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    let mut params = input("Review changes");
    params.review = Some("Review: uncommitted changes".into());
    session.input(params).await.unwrap();
    finished(&mut events).await;
    assert!(model.requests.lock().unwrap().last().unwrap().messages.iter().any(|m| matches!(m, Message::ToolResult { text, .. } if text.contains("review is read-only"))));
    session.close().await.unwrap();
}

#[tokio::test]
async fn review_queues_as_its_own_turn_and_keeps_prompt_and_answer_in_history() {
    let release = Arc::new(Notify::new());
    let model = ScriptModel::new(vec![
        vec![Step::Wait(release.clone()), text("first answer"), finish()],
        vec![
            Step::Event(ModelEvent::ToolCall(ToolCall {
                id: "write".into(),
                name: "write".into(),
                arguments: r#"{"path":"forbidden","content":"no"}"#.into(),
            })),
            Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
        ],
        vec![text("No findings. Safe to merge."), finish()],
    ]);
    let (temp, _, session) = create(model.clone(), CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("ordinary turn")).await.unwrap();
    let mut review = input("review instructions and diff");
    review.review = Some("Review: uncommitted changes".into());
    review.mode = InputMode::Steer;
    assert!(session.input(review).await.unwrap().turn_id.is_none());
    let queued = session.snapshot().await.session.queued_inputs;
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].review.as_deref(),
        Some("Review: uncommitted changes")
    );
    release.notify_one();
    finished(&mut events).await;
    let review_events = finished(&mut events).await;
    assert!(
        !review_events
            .iter()
            .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
    );
    assert!(!temp.path().join("forbidden").exists());
    let snapshot = session.snapshot().await.session;
    assert!(snapshot.queued_inputs.is_empty());
    assert_eq!(snapshot.turns.len(), 2);
    assert!(snapshot.turns[0].review.is_none());
    assert_eq!(
        snapshot.turns[1].review.as_deref(),
        Some("Review: uncommitted changes")
    );
    assert!(snapshot.turns[1].items.iter().any(|item| matches!(item, Item::AgentMessage { text, .. } if text == "No findings. Safe to merge.")));
    {
        let requests = model.requests.lock().unwrap();
        assert!(
            !serde_json::to_string(&requests[0])
                .unwrap()
                .contains("review instructions and diff")
        );
        assert!(
            serde_json::to_string(requests.last().unwrap())
                .unwrap()
                .contains("review instructions and diff")
        );
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn review_stays_readonly_after_ordinary_steering() {
    let release = Arc::new(Notify::new());
    let model = ScriptModel::new(vec![
        vec![
            text("review waiting"),
            Step::Wait(release.clone()),
            finish(),
        ],
        vec![
            Step::Event(ModelEvent::ToolCall(ToolCall {
                id: "steered-write".into(),
                name: "write".into(),
                arguments: r#"{"path":"forbidden","content":"changed"}"#.into(),
            })),
            Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
        ],
        vec![text("review finished"), finish()],
    ]);
    let (temp, _, session) = create(model.clone(), CoreOptions::default()).await;
    session
        .update(SessionUpdateParams {
            approval_policy: Some(PermissionMode::AlwaysAsk),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let mut review = input("review changes");
    review.review = Some("Review: uncommitted changes".into());
    let turn = session.input(review).await.unwrap().turn_id;
    until(&mut events, |event| matches!(event, EventPayload::ItemDelta { delta: ItemDelta::AgentMessage { delta }, .. } if delta == "review waiting")).await;
    let mut steer = input("please write the fix");
    steer.mode = InputMode::Steer;
    assert_eq!(session.input(steer).await.unwrap().turn_id, turn);
    release.notify_one();
    let seen = until(&mut events, |event| {
        matches!(
            event,
            EventPayload::RequestOpened { .. } | EventPayload::TurnCompleted { .. }
        )
    })
    .await;
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
    );
    assert!(!temp.path().join("forbidden").exists());
    let snapshot = session.snapshot().await.session;
    assert_eq!(snapshot.turns.len(), 1);
    assert!(snapshot.turns[0].review.is_some());
    {
        let requests = model.requests.lock().unwrap();
        assert!(
            serde_json::to_string(&requests[1].messages)
                .unwrap()
                .contains("please write the fix")
        );
        assert!(requests.last().unwrap().messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "steered-write" && text == "review is read-only")));
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn interrupted_review_restores_the_next_inputs_original_permissions() {
    let model = ScriptModel::new(vec![
        vec![
            text("review waiting"),
            Step::Wait(Arc::new(Notify::new())),
            finish(),
        ],
        vec![
            Step::Event(ModelEvent::ToolCall(ToolCall {
                id: "ordinary-write".into(),
                name: "write".into(),
                arguments: r#"{"path":"allowed","content":"changed"}"#.into(),
            })),
            Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
        ],
        vec![text("ordinary finished"), finish()],
    ]);
    let (temp, _, session) = create(model, CoreOptions::default()).await;
    session
        .update(SessionUpdateParams {
            approval_policy: Some(PermissionMode::AlwaysAsk),
            ..Default::default()
        })
        .await
        .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let mut review = input("review changes");
    review.review = Some("Review: uncommitted changes".into());
    let turn_id = session.input(review).await.unwrap().turn_id.unwrap();
    until(&mut events, |event| matches!(event, EventPayload::ItemDelta { delta: ItemDelta::AgentMessage { delta }, .. } if delta == "review waiting")).await;
    session
        .interrupt(SessionInterruptParams {
            turn_id,
            retract: None,
        })
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)));
    session.input(input("now write the fix")).await.unwrap();
    let seen = until(&mut events, |event| {
        matches!(
            event,
            EventPayload::RequestOpened { .. } | EventPayload::TurnCompleted { .. }
        )
    })
    .await;
    let EventPayload::RequestOpened { request } = &seen.last().unwrap().payload else {
        panic!("ordinary write must request approval under alwaysAsk");
    };
    assert!(!temp.path().join("allowed").exists());
    session
        .resolve(
            &request.id,
            RequestResolveParams {
                plan: None,
                feedback: None,
                decision: Some(ApprovalDecision::Accept),
                answer: None,
                client_name: "test".into(),
            },
        )
        .await
        .unwrap();
    finished(&mut events).await;
    assert_eq!(
        std::fs::read_to_string(temp.path().join("allowed")).unwrap(),
        "changed"
    );
    let snapshot = session.snapshot().await.session;
    assert_eq!(snapshot.settings.approval_policy, PermissionMode::AlwaysAsk);
    assert!(snapshot.turns[1].review.is_none());
    session.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn rejected_image_during_stopping_does_not_leave_a_copy() {
    // Hold the model boundary while interrupt is pending, without holding the
    // session lock. This makes the stopping state deterministic.
    struct HeldModel {
        entered: Notify,
        cancellation: Mutex<Option<CancellationToken>>,
        release: (Mutex<bool>, std::sync::Condvar),
    }
    impl Model for HeldModel {
        fn stream(&self, _: Request, cancel: CancellationToken) -> ModelStream<'_> {
            *self.cancellation.lock().unwrap() = Some(cancel);
            self.entered.notify_one();
            let (released, _) = self
                .release
                .1
                .wait_timeout_while(
                    self.release.0.lock().unwrap(),
                    Duration::from_secs(15),
                    |released| !*released,
                )
                .unwrap();
            assert!(*released, "test must release its model");
            Box::pin(futures_util::stream::empty())
        }
    }
    let model = Arc::new(HeldModel {
        entered: Notify::new(),
        cancellation: Mutex::new(None),
        release: (Mutex::new(false), std::sync::Condvar::new()),
    });
    let (temp, _, session) = create(
        model.clone(),
        CoreOptions {
            image_models: vec!["fake".into()],
            ..CoreOptions::default()
        },
    )
    .await;
    let path = temp.path().join("source.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
    let started = session.input(input("start")).await.unwrap();
    model.entered.notified().await;
    let cancellation = model.cancellation.lock().unwrap().clone().unwrap();
    let owner = session.clone();
    let interrupt = tokio::spawn(async move {
        owner
            .interrupt(SessionInterruptParams {
                retract: None,
                turn_id: started.turn_id.unwrap(),
            })
            .await
    });
    cancellation.cancelled().await;
    let before = session.snapshot().await;
    let result = session
        .input(SessionInputParams {
            review: None,
            input: vec![UserInput::LocalImage {
                path: path.to_string_lossy().into_owned(),
                name: None,
            }],
            ..input("")
        })
        .await;
    let after = session.snapshot().await;
    let copies: Vec<_> = std::fs::read_dir(temp.path().join("sessions/s1"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("image-"))
        .collect();
    *model.release.0.lock().unwrap() = true;
    model.release.1.notify_one();
    interrupt.await.unwrap().unwrap();
    assert_eq!(result.unwrap_err().message, "turn is stopping");
    assert_eq!(after, before);
    assert!(copies.is_empty(), "rejected image left copies: {copies:?}");
    session.close().await.unwrap();
}

#[tokio::test]
async fn rejected_images_are_removed_when_history_or_event_writes_fail() {
    // Resource limits are process-wide: change them only in this test's child.
    const CHILD: &str = "CANTER_IMAGE_WRITE_FAILURE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rejected_images_are_removed_when_history_or_event_writes_fail",
                "--nocapture",
            ])
            .env_clear()
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    struct FileLimit(nix::libc::rlimit, nix::libc::sighandler_t);
    impl FileLimit {
        fn small() -> Self {
            let mut previous = nix::libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: valid rlimit pointers; this isolated child owns the limit
            // and signal handler, and Drop restores both before cleanup.
            unsafe {
                assert_eq!(
                    nix::libc::getrlimit(nix::libc::RLIMIT_FSIZE, &mut previous),
                    0
                );
                let handler = nix::libc::signal(nix::libc::SIGXFSZ, nix::libc::SIG_IGN);
                assert_ne!(handler, nix::libc::SIG_ERR);
                let limit = nix::libc::rlimit {
                    rlim_cur: 128,
                    rlim_max: previous.rlim_max,
                };
                assert_eq!(nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &limit), 0);
                Self(previous, handler)
            }
        }
    }
    impl Drop for FileLimit {
        fn drop(&mut self) {
            // SAFETY: restore the values read before this test changed them.
            unsafe {
                nix::libc::setrlimit(nix::libc::RLIMIT_FSIZE, &self.0);
                nix::libc::signal(nix::libc::SIGXFSZ, self.1);
            }
        }
    }
    for filename in ["model-history.jsonl", "events.jsonl"] {
        let model = ScriptModel::new(vec![vec![
            text("ready"),
            Step::Wait(Arc::new(Notify::new())),
        ]]);
        let (temp, _, session) = create(
            model,
            CoreOptions {
                image_models: vec!["fake".into()],
                ..CoreOptions::default()
            },
        )
        .await;
        let directory = temp.path().join("sessions/s1");
        if filename == "events.jsonl" {
            let mut events = session.subscribe(0).await.unwrap();
            session.input(input("keep this turn active")).await.unwrap();
            until(&mut events, |event| matches!(event, EventPayload::ItemDelta { delta: ItemDelta::AgentMessage { delta }, .. } if delta == "ready")).await;
        }
        let source = temp.path().join("source.png");
        std::fs::write(&source, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(std::fs::metadata(directory.join(filename)).unwrap().len() > 128);
        // The 8-byte copy can be written; appending to the journal cannot.
        let guard = FileLimit::small();
        let result = session
            .input(SessionInputParams {
                review: None,
                input: vec![UserInput::LocalImage {
                    path: source.to_string_lossy().into_owned(),
                    name: None,
                }],
                ..input("")
            })
            .await;
        drop(guard);
        let copies: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with("image-"))
            .collect();
        let _ = session.close().await;
        assert_eq!(result.unwrap_err().code, ErrorCode::Internal);
        assert!(
            copies.is_empty(),
            "{filename} write failure left copies: {copies:?}"
        );
    }
}

#[tokio::test]
async fn image_requests_and_compaction_keep_the_newest_images_within_40_mib() {
    use canter_model::UserContent;
    let model = ScriptModel::new(vec![
        vec![text("first"), finish()],
        vec![text("second"), finish()],
        vec![text("summary"), finish()],
    ]);
    let (temp, store, session) = create(
        model.clone(),
        CoreOptions {
            image_models: vec!["fake".into()],
            ..CoreOptions::default()
        },
    )
    .await;
    let mut events = session.subscribe(0).await.unwrap();
    let mut inputs = Vec::new();
    for index in 0..4 {
        let path = temp.path().join(format!("large-{index}.png"));
        let mut bytes = vec![index as u8; 8 * 1024 * 1024];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        std::fs::write(&path, bytes).unwrap();
        inputs.push(UserInput::LocalImage {
            path: path.to_string_lossy().into_owned(),
            name: None,
        });
    }
    for batch in inputs.chunks(2) {
        session
            .input(SessionInputParams {
                review: None,
                input: batch.to_vec(),
                ..input("")
            })
            .await
            .unwrap();
        finished(&mut events).await;
    }
    session.compact().await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        for request in &requests[1..] {
            let parts: Vec<_> = request
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::UserParts { content } => Some(content.iter()),
                    _ => None,
                })
                .flatten()
                .collect();
            assert!(parts.iter().any(|part| matches!(part, UserContent::Text { text } if text == "[image omitted: large-0.png]")), "oldest image must be omitted");
            let images: Vec<_> = parts
                .iter()
                .filter_map(|part| match part {
                    UserContent::Image { name, data_url, .. } => {
                        Some((name.as_str(), data_url.as_str()))
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(
                images.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
                ["large-1.png", "large-2.png", "large-3.png"]
            );
            assert!(
                images
                    .iter()
                    .all(|(_, url)| url.starts_with("data:image/png;base64,"))
            );
            assert!(
                images
                    .iter()
                    .map(|(_, url)| url.split_once(',').unwrap().1.len())
                    .sum::<usize>()
                    <= 40 * 1024 * 1024
            );
        }
    }
    let history = store.model_history::<serde_json::Value>("s1").unwrap();
    assert!(
        !serde_json::to_string(&history)
            .unwrap()
            .contains("image omitted")
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn image_references_survive_moving_the_store_and_missing_copies_only_warn() {
    use canter_model::UserContent;
    let model = ScriptModel::new((0..4).map(|_| vec![text("continued"), finish()]).collect());
    let options = CoreOptions {
        image_models: vec!["fake".into()],
        ..CoreOptions::default()
    };
    let (temp, store, session) = create(model.clone(), options.clone()).await;
    let source = temp.path().join("source.png");
    std::fs::write(&source, b"\x89PNG\r\n\x1a\n").unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session
        .input(SessionInputParams {
            review: None,
            input: vec![UserInput::LocalImage {
                path: source.to_string_lossy().into_owned(),
                name: None,
            }],
            ..input("")
        })
        .await
        .unwrap();
    finished(&mut events).await;
    let snapshot = session.snapshot().await;
    let UserInput::LocalImage { path: saved, .. } = &snapshot.session.turns[0].input[0] else {
        panic!("image reference");
    };
    assert_eq!(
        std::path::Path::new(saved).file_name().unwrap(),
        saved.as_str(),
        "events must store only the filename"
    );
    let history = store.model_history::<serde_json::Value>("s1").unwrap();
    let image = history
        .iter()
        .find_map(|entry| entry.pointer("/message/content/0/path"))
        .unwrap();
    assert_eq!(image, saved);
    let saved = saved.clone();
    session.close().await.unwrap();
    drop(session);
    let moved_root = temp.path().join("moved-sessions");
    std::fs::rename(temp.path().join("sessions"), &moved_root).unwrap();
    std::fs::remove_file(source).unwrap();
    let moved_store = Arc::new(Store::new(&moved_root).unwrap());
    let (session, opened) = Session::open(
        moved_store,
        "s1".into(),
        RuntimeContext::default(),
        model.clone(),
        options,
    )
    .await
    .unwrap();
    let mut events = session.subscribe(opened.event_seq).await.unwrap();
    session.input(input("after moving")).await.unwrap();
    finished(&mut events).await;
    assert!(model.requests.lock().unwrap()[1].messages.iter().any(|message| matches!(message, Message::UserParts { content } if content.iter().any(|part| matches!(part, UserContent::Image { data_url, .. } if data_url == "data:image/png;base64,iVBORw0KGgo=")))));
    std::fs::remove_file(moved_root.join("s1").join(saved)).unwrap();
    for compact in [false, true] {
        if compact {
            session.compact().await.unwrap();
        } else {
            session.input(input("after losing the copy")).await.unwrap();
        }
        let seen = finished(&mut events).await;
        assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message.contains("image unavailable: source.png"))), "missing copy must produce a warning");
        assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)), "missing copy must not fail the turn");
    }
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        for request in &requests[2..] {
            assert!(request.messages.iter().any(|message| matches!(message, Message::UserParts { content } if content.iter().any(|part| matches!(part, UserContent::Text { text } if text == "[image unavailable: source.png]")))));
        }
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn manual_compaction_returns_at_start_and_steering_runs_in_a_separate_turn() {
    for needs_approval in [false, true] {
        let release_summary = Arc::new(Notify::new());
        let follow_up = if needs_approval {
            vec![
                Step::Event(ModelEvent::ToolCall(ToolCall {
                    id: "shell".into(),
                    name: "shell".into(),
                    arguments: r#"{"command":"touch must-not-exist"}"#.into(),
                })),
                Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
            ]
        } else {
            vec![Step::Error(ErrorKind::Server)]
        };
        let model = ScriptModel::new(vec![
            vec![
                Step::Wait(release_summary.clone()),
                text("summary complete"),
                finish(),
            ],
            follow_up,
        ]);
        let (temp, _, session) = create(model, CoreOptions::default()).await;
        let mut events = session.subscribe(0).await.unwrap();
        let owner = session.clone();
        let mut compact = tokio::spawn(async move { owner.compact().await });
        let started = until(&mut events, |event| {
            matches!(
                event,
                EventPayload::ItemStarted {
                    item: Item::ContextCompaction { .. },
                    ..
                }
            )
        })
        .await;
        let EventPayload::ItemStarted {
            turn_id: compact_turn,
            ..
        } = &started.last().unwrap().payload
        else {
            unreachable!()
        };
        let early = tokio::time::timeout(Duration::from_secs(2), &mut compact).await;
        if early.is_err() {
            session.close().await.unwrap();
        }
        let response = early
            .expect("compact must return while the summary model is blocked")
            .unwrap()
            .unwrap();
        let response = serde_json::to_value(response).unwrap();
        assert_eq!(response["turnId"], compact_turn.as_str());
        assert!(
            started
                .iter()
                .any(|event| event.seq == response["eventSeq"].as_u64().unwrap()
                    && matches!(event.payload, EventPayload::TurnStarted { .. }))
        );
        let queued = session
            .input(SessionInputParams {
                review: None,
                mode: InputMode::Steer,
                ..input("follow-up")
            })
            .await
            .unwrap();
        assert!(
            queued.turn_id.is_none(),
            "steering during compaction must queue a new turn"
        );
        release_summary.notify_one();
        let completed = finished(&mut events).await;
        assert!(
            matches!(&completed.last().unwrap().payload, EventPayload::TurnCompleted { turn } if &turn.id == compact_turn && turn.status == TurnStatus::Completed && turn.error.is_none())
        );
        assert!(completed.iter().any(|event| matches!(&event.payload, EventPayload::ItemCompleted { item: Item::ContextCompaction { summary, .. }, .. } if summary == "summary complete")));
        if needs_approval {
            let waiting = until(&mut events, |event| {
                matches!(event, EventPayload::RequestOpened { .. })
            })
            .await;
            assert!(
                matches!(&waiting.last().unwrap().payload, EventPayload::RequestOpened { request } if &request.turn_id != compact_turn)
            );
        } else {
            let failed = finished(&mut events).await;
            assert!(
                matches!(&failed.last().unwrap().payload, EventPayload::TurnCompleted { turn } if &turn.id != compact_turn && turn.status == TurnStatus::Failed)
            );
        }
        session.close().await.unwrap();
        assert!(!temp.path().join("must-not-exist").exists());
    }
}

#[tokio::test]
async fn interrupting_compaction_does_not_report_an_error() {
    let model = ScriptModel::new(vec![vec![Step::Wait(Arc::new(Notify::new()))]]);
    let (_temp, _store, session) = create(model, CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    let response = session.compact().await.unwrap();
    until(&mut events, |event| {
        matches!(
            event,
            EventPayload::ItemStarted {
                item: Item::ContextCompaction { .. },
                ..
            }
        )
    })
    .await;
    session
        .interrupt(SessionInterruptParams {
            retract: None,
            turn_id: response.turn_id.unwrap(),
        })
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(
        !seen
            .iter()
            .any(|event| matches!(event.payload, EventPayload::Error { .. })),
        "interrupting a compaction is not a model error"
    );
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted && turn.error.is_none())
    );
    assert!(session.snapshot().await.session.error.is_none());
    session.close().await.unwrap();
}

fn history_messages(store: &Store) -> Vec<Message> {
    store
        .model_history::<serde_json::Value>("s1")
        .unwrap()
        .into_iter()
        .filter(|entry| entry["type"] == "message")
        .map(|entry| serde_json::from_value(entry["message"].clone()).unwrap())
        .collect()
}

#[tokio::test]
async fn unreadable_project_instructions_warn_and_do_not_block_the_model() {
    for kind in ["directory", "utf8", "oversize"] {
        let model = ScriptModel::new(vec![
            vec![text("first"), finish()],
            vec![text("second"), finish()],
        ]);
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("AGENTS.md");
        match kind {
            "directory" => std::fs::create_dir(&path).unwrap(),
            "utf8" => std::fs::write(&path, [0xff, 0xfe]).unwrap(),
            _ => std::fs::write(&path, "x".repeat(65_537)).unwrap(),
        }
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let (session, created) = Session::create(
            store.clone(),
            "s1".into(),
            SessionSettings {
                additional_directories: Vec::new(),
                plan_mode: false,
                pre_plan_approval_policy: None,
                plan_enabled: None,
                cwd: temp.path().to_string_lossy().into(),
                model: "fake".into(),
                reasoning_effort: Default::default(),
                name: None,
                approval_policy: PermissionMode::AskForExecution,
            },
            RuntimeContext::default(),
            model.clone(),
            CoreOptions::default(),
        )
        .await
        .unwrap();
        assert_eq!(
            store
                .events_after("s1", 0)
                .unwrap()
                .iter()
                .filter(|event| event.seq <= created.event_seq
                    && matches!(&event.payload, EventPayload::Warning { message }
                if message.contains(path.to_str().unwrap())))
                .count(),
            1,
            "{kind} must warn during creation, before any model request"
        );
        let mut events = session.subscribe(0).await.unwrap();
        for round in 0..2 {
            session.input(input("continue")).await.unwrap();
            let seen = finished(&mut events).await;
            let warnings = seen.iter().filter(|event| matches!(&event.payload, EventPayload::Warning { message } if message.contains(path.to_str().unwrap()))).count();
            assert_eq!(
                warnings,
                usize::from(round == 0),
                "warn once for unchanged {kind}"
            );
            assert!(
                matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
            );
        }
        assert_eq!(model.requests.lock().unwrap().len(), 2);
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn completed_assistant_items_already_have_durable_model_history() {
    let model = ScriptModel::new(vec![vec![
        Step::Event(ModelEvent::ReasoningDelta {
            item_id: "reasoning".into(),
            delta: "reasoning".into(),
        }),
        text("answer"),
        Step::Event(ModelEvent::Usage(Usage {
            input_tokens: 10,
            output_tokens: 2,
            total_tokens: 12,
            ..Usage::default()
        })),
        finish(),
    ]]);
    let (_temp, store, session) = create(model, CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("hello")).await.unwrap();
    until(&mut events, |event| {
        matches!(
            event,
            EventPayload::ItemCompleted {
                item: Item::AgentMessage { .. },
                ..
            }
        )
    })
    .await;
    assert!(history_messages(&store).iter().any(|message| matches!(message, Message::Assistant { content } if content == &vec![AssistantContent::Reasoning { text: "reasoning".into() }, AssistantContent::Text { text: "answer".into() } ])), "item.completed must not precede the assistant history write");
    finished(&mut events).await;
    session.close().await.unwrap();
}

#[tokio::test]
async fn unknown_tool_warns_with_its_name_and_returns_a_result_to_the_model() {
    let model = ScriptModel::new(vec![
        vec![
            Step::Event(ModelEvent::ToolCall(ToolCall {
                id: "unknown".into(),
                name: "missing_tool".into(),
                arguments: "{}".into(),
            })),
            Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
        ],
        vec![text("recovered"), finish()],
    ]);
    let (_temp, _store, session) = create(model.clone(), CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("run")).await.unwrap();
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message.contains("missing_tool"))));
    assert!(model.requests.lock().unwrap()[1].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "unknown" && text.contains("Unknown tool"))));
    session.close().await.unwrap();
}

#[tokio::test]
async fn interrupted_approval_is_resolved_before_the_failed_tool_item() {
    let model = ScriptModel::new(vec![vec![
        Step::Event(ModelEvent::ToolCall(ToolCall {
            id: "pending".into(),
            name: "shell".into(),
            arguments: r#"{"command":"touch must-not-exist"}"#.into(),
        })),
        Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
    ]]);
    let (temp, store, session) = create(model, CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    let response = session.input(input("run")).await.unwrap();
    until(&mut events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    session
        .interrupt(SessionInterruptParams {
            retract: None,
            turn_id: response.turn_id.unwrap(),
        })
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    let resolved = seen
        .iter()
        .position(|event| {
            matches!(
                event.payload,
                EventPayload::RequestResolved {
                    resolution: ApprovalResolution::Cancelled,
                    ..
                }
            )
        })
        .unwrap();
    let completed = seen
        .iter()
        .position(|event| {
            matches!(
                event.payload,
                EventPayload::ItemCompleted {
                    item: Item::CommandExecution { .. },
                    ..
                }
            )
        })
        .unwrap();
    assert!(
        resolved < completed,
        "cancel the approval request before completing its tool item"
    );
    assert!(matches!(
        seen[completed].payload,
        EventPayload::ItemCompleted {
            item: Item::CommandExecution {
                status: ItemStatus::Failed,
                ..
            },
            ..
        }
    ));
    assert!(history_messages(&store).iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "pending" && text.contains("Interrupted; not run"))));
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn cancel_approval_keeps_the_declined_result_and_stops_the_turn() {
    let model = ScriptModel::new(vec![vec![
        Step::Event(ModelEvent::ToolCall(ToolCall {
            id: "declined".into(),
            name: "shell".into(),
            arguments: r#"{"command":"touch must-not-exist"}"#.into(),
        })),
        Step::Event(ModelEvent::Finished(FinishReason::ToolCalls)),
    ]]);
    let (temp, store, session) = create(model.clone(), CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("run")).await.unwrap();
    let opened = until(&mut events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = &opened.last().unwrap().payload else {
        unreachable!()
    };
    let answer = session
        .resolve(
            &request.id,
            RequestResolveParams {
                plan: None,
                feedback: None,
                decision: Some(ApprovalDecision::Cancel),
                answer: None,
                client_name: "test".into(),
            },
        )
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| event.seq == answer.event_seq
        && matches!(
            event.payload,
            EventPayload::RequestResolved {
                resolution: ApprovalResolution::Answered {
                    decision: ApprovalDecision::Cancel,
                    ..
                },
                ..
            }
        )));
    assert!(
        seen.iter().any(|event| event.seq > answer.event_seq
            && matches!(
                event.payload,
                EventPayload::ItemCompleted {
                    item: Item::CommandExecution {
                        status: ItemStatus::Declined,
                        ..
                    },
                    ..
                }
            )),
        "an explicit cancel answer must remain a declined tool call"
    );
    assert!(history_messages(&store).iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "declined" && text == "Tool call declined.")));
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted && turn.error.is_none())
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn project_instruction_warnings_repeat_only_when_reopening() {
    let mut scripts = vec![read_fixture("first"), read_fixture("second")];
    scripts.extend((0..5).map(|_| vec![text("done"), finish()]));
    let model = ScriptModel::new(scripts);
    let (temp, store, session) = create(model.clone(), CoreOptions::default()).await;
    std::fs::write(temp.path().join("fixture.txt"), "fixture").unwrap();
    let path = temp.path().join("AGENTS.md");
    std::fs::create_dir(&path).unwrap();
    let closed = session.close().await.unwrap();
    drop(session);
    let (session, opened) = Session::open(
        store.clone(),
        "s1".into(),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(closed.event_seq).await.unwrap();
    assert!(
        store
            .events_after("s1", closed.event_seq)
            .unwrap()
            .iter()
            .any(|event| event.seq <= opened.event_seq
                && matches!(&event.payload, EventPayload::Warning { message }
            if message.contains(path.to_str().unwrap())))
    );
    for (round, expected_warnings) in [1, 0, 0, 0, 0].into_iter().enumerate() {
        match round {
            2 => {
                std::fs::remove_dir(&path).unwrap();
                std::fs::write(&path, [0xff]).unwrap();
            }
            3 => std::fs::write(&path, "valid instructions").unwrap(),
            4 => std::fs::write(&path, [0xff]).unwrap(),
            _ => {}
        }
        session.input(input("continue")).await.unwrap();
        let seen = finished(&mut events).await;
        let warnings: Vec<_> = seen
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::Warning { message } if message.contains(path.to_str().unwrap()) => {
                    Some(message)
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            warnings.len(),
            expected_warnings,
            "duplicate instruction warning in round {round}"
        );
        assert!(
            matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
        );
    }
    assert_eq!(model.requests.lock().unwrap().len(), 7);
    let closed = session.close().await.unwrap();
    drop(session);
    let (session, _) = Session::open(
        store.clone(),
        "s1".into(),
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let warnings: Vec<_> = store
        .events_after("s1", closed.event_seq)
        .unwrap()
        .into_iter()
        .filter_map(|event| match event.payload {
            EventPayload::Warning { message } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("not valid UTF-8"));
    session.close().await.unwrap();
}

#[tokio::test]
async fn failed_automatic_compaction_waits_until_a_new_turn_or_manual_request() {
    let release_reply = Arc::new(Notify::new());
    let model = ScriptModel::new(vec![
        vec![text(&"x".repeat(175_000)), finish()],
        vec![Step::Error(ErrorKind::Server)],
        read_fixture("first"),
        read_fixture("second"),
        vec![
            text("ready for steering"),
            Step::Wait(release_reply.clone()),
            finish(),
        ],
        vec![text("steered reply"), finish()],
        vec![Step::Error(ErrorKind::Server)],
        vec![text("next turn"), finish()],
        vec![Step::Error(ErrorKind::Server)],
    ]);
    let (temp, _store, session) = create(
        model.clone(),
        CoreOptions {
            context_window: 100_000,
            max_output_tokens: 32_768,
            ..CoreOptions::default()
        },
    )
    .await;
    std::fs::write(temp.path().join("fixture.txt"), "fixture").unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("seed")).await.unwrap();
    finished(&mut events).await;
    let response = session.input(input("use two tools")).await.unwrap();
    let mut seen = until(&mut events, |event| matches!(event, EventPayload::ItemDelta { delta: ItemDelta::AgentMessage { delta }, .. } if delta == "ready for steering")).await;
    let steered = session
        .input(SessionInputParams {
            review: None,
            mode: InputMode::Steer,
            ..input("continue in this turn")
        })
        .await
        .unwrap();
    assert_eq!(steered.turn_id, response.turn_id);
    let queued = session.input(input("new turn")).await.unwrap();
    assert!(queued.turn_id.is_none());
    release_reply.notify_one();
    seen.extend(finished(&mut events).await);
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(
                event.payload,
                EventPayload::ItemStarted {
                    item: Item::ContextCompaction { .. },
                    ..
                }
            ))
            .count(),
        1,
        "a failed automatic compaction must not repeat after tools or late steering"
    );
    assert_eq!(seen.iter().filter(|event| matches!(&event.payload, EventPayload::Warning { message } if message.contains("Auto-compaction failed"))).count(), 1);
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
    );
    let seen = finished(&mut events).await;
    assert_eq!(
        model
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.tools.is_empty())
            .count(),
        2,
        "a new turn may retry compaction"
    );
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
    );
    session.compact().await.unwrap();
    let seen = finished(&mut events).await;
    assert_eq!(
        model
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.tools.is_empty())
            .count(),
        3,
        "manual compaction may retry"
    );
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Failed)
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn duplicate_session_id_is_a_conflict() {
    let model = ScriptModel::new(vec![]);
    let (_temp, store, session) = create(model.clone(), CoreOptions::default()).await;
    let settings = session.snapshot().await.session.settings;
    let error = Session::create(
        store,
        "s1".into(),
        settings,
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .err()
    .expect("duplicate creation must fail");
    assert_eq!(error.code, ErrorCode::Conflict);
    session.close().await.unwrap();
}

#[tokio::test]
async fn failed_automatic_compaction_continues_only_when_the_request_still_fits() {
    for fits in [true, false] {
        for length in [true, false] {
            let old_reply = "x".repeat(if fits { 175_000 } else { 230_000 });
            let failed_summary = if length {
                vec![
                    text("incomplete summary"),
                    Step::Event(ModelEvent::Finished(FinishReason::Length)),
                ]
            } else {
                vec![Step::Error(ErrorKind::Server)]
            };
            let model = ScriptModel::new(vec![
                vec![text(&old_reply), finish()],
                failed_summary,
                vec![text("continued"), finish()],
            ]);
            let (_temp, store, session) = create(
                model.clone(),
                CoreOptions {
                    context_window: 100_000,
                    max_output_tokens: 32_768,
                    ..CoreOptions::default()
                },
            )
            .await;
            let mut events = session.subscribe(0).await.unwrap();
            session.input(input("seed")).await.unwrap();
            finished(&mut events).await;
            session.input(input("continue")).await.unwrap();
            let seen = finished(&mut events).await;
            let EventPayload::TurnCompleted { turn } = &seen.last().unwrap().payload else {
                unreachable!()
            };
            if fits {
                assert_eq!(
                    turn.status,
                    TurnStatus::Completed,
                    "failed summary must not prevent a request that fits"
                );
                assert!(
                    seen.iter()
                        .any(|event| matches!(event.payload, EventPayload::Warning { .. }))
                );
                let requests = model.requests.lock().unwrap();
                assert_eq!(requests.len(), 3);
                assert!(!requests[2].tools.is_empty());
                assert!(requests[2].messages.iter().any(|message| matches!(message, Message::Assistant { content } if content == &vec![AssistantContent::Text { text: old_reply.clone() }])));
            } else {
                assert_eq!(turn.status, TurnStatus::Failed);
                assert!(
                    turn.error
                        .as_ref()
                        .unwrap()
                        .message
                        .contains("context window")
                );
                assert_eq!(model.requests.lock().unwrap().len(), 2);
            }
            assert!(
                !store
                    .model_history::<serde_json::Value>("s1")
                    .unwrap()
                    .iter()
                    .any(|entry| entry["type"] == "compaction")
            );
            session.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn compaction_prompt_wraps_each_chunk_and_fits_the_context_budget() {
    let old_reply = "old中文\n\"quoted\"\\".repeat(40_000);
    let mut scripts = vec![vec![text(&old_reply), finish()]];
    scripts.extend((0..100).map(|_| vec![text("previous chunk summary"), finish()]));
    let model = ScriptModel::new(scripts);
    let options = CoreOptions {
        context_window: 100_000,
        ..CoreOptions::default()
    };
    let (_temp, _store, session) = create(model.clone(), options).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("seed")).await.unwrap();
    finished(&mut events).await;
    session.compact().await.unwrap();
    let seen = finished(&mut events).await;
    assert!(matches!(&seen.last().unwrap().payload,
        EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed));
    {
        let requests = model.requests.lock().unwrap();
        assert!(
            requests.len() > 2,
            "the transcript must span multiple requests"
        );
        let mut transcript = String::new();
        for (index, request) in requests[1..].iter().enumerate() {
            assert!(request.tools.is_empty());
            assert_eq!(request.options.thinking, Some(false));
            let Message::User { text } = request.messages.last().unwrap() else {
                panic!("compaction instructions");
            };
            let chunk = text
                .split_once("\n<transcript>\n")
                .expect("transcript opening tag")
                .1
                .split_once("\n</transcript>\n")
                .expect("transcript closing tag")
                .0;
            let earlier =
                "<earlier_summary>\n…上一段摘要，分段压缩到第二段以后才有…\n</earlier_summary>\n\n";
            let expected = prompt_spec("### 6.2 压缩")
                .replace(
                    earlier,
                    if index == 0 {
                        ""
                    } else {
                        "<earlier_summary>\nprevious chunk summary\n</earlier_summary>\n\n"
                    },
                )
                .replace(
                    "…转写，格式照现在：User: / Assistant: / Tool call … / Tool result …",
                    chunk,
                );
            assert_eq!(text, &expected);
            transcript.push_str(chunk);
            // The documented byte/3 estimate includes each message's JSON and
            // eight-token overhead, tool definitions, output, and reserve.
            let input_tokens: u64 = request
                .messages
                .iter()
                .map(|message| {
                    (serde_json::to_string(message).unwrap().len() as u64).div_ceil(3) + 8
                })
                .sum::<u64>()
                + (serde_json::to_string(&request.tools).unwrap().len() as u64).div_ceil(3);
            assert!(
                input_tokens + u64::from(request.options.max_output_tokens.unwrap()) + 16_384
                    <= 100_000,
                "chunk {index} exceeds the context budget"
            );
        }
        assert_eq!(
            transcript,
            format!("\nUser: seed\n\nAssistant: {old_reply}\n")
        );
    }
    session.close().await.unwrap();
}

fn prompt_spec(section: &str) -> String {
    let spec = include_str!("../../../docs/调研/P2-57-系统提示词方案.md");
    let block = |heading| {
        spec.split_once(heading)
            .unwrap()
            .1
            .split_once("```text\n")
            .unwrap()
            .1
            .split_once("\n```")
            .unwrap()
            .0
    };
    block(section)
        .replace("{SECTIONS}", block("`{SECTIONS}`："))
        .replace("{RULES}", block("`{RULES}`："))
}

#[tokio::test]
async fn handoff_prompt_requests_task_and_ends_with_explicit_or_default_direction() {
    let model = ScriptModel::new(vec![
        vec![text("seed reply"), finish()],
        vec![text("handoff note"), finish()],
        vec![text("handoff note"), finish()],
    ]);
    let (_temp, _store, session) = create(model.clone(), CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("seed")).await.unwrap();
    finished(&mut events).await;
    for direction in [Some("继续修复测试，先检查 git status。"), None] {
        let mut answer = session.handoff(direction.map(str::to_owned)).await.unwrap();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                match answer.recv().await.expect("handoff stream ended early") {
                    BtwEvent::Delta { .. } => {}
                    BtwEvent::Completed => break,
                    event => panic!("unexpected handoff event: {event:?}"),
                }
            }
        })
        .await
        .unwrap();
        let requests = model.requests.lock().unwrap();
        let request = requests.last().unwrap();
        let Message::User { text } = request.messages.last().unwrap() else {
            panic!("handoff instructions");
        };
        let direction = direction.unwrap_or("Continue the current task.");
        assert_eq!(
            text,
            &prompt_spec("### 6.3 `/handoff`").replace("{DIRECTION}", direction)
        );
        assert!(text.ends_with(&format!("Task for the new session:\n{direction}")));
        assert_eq!(request.tools, requests[0].tools);
        assert!(!request.tools.is_empty());
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn oversized_summary_request_retries_smaller_chunks_without_losing_transcript() {
    let old_reply = "old中文".repeat(256);
    let model = ScriptModel::new(vec![
        vec![text(&old_reply), finish()],
        vec![Step::Error(ErrorKind::ContextLength)],
        vec![text("first half"), finish()],
        vec![text("complete summary"), finish()],
        vec![text("continued"), finish()],
    ]);
    let (_temp, _store, session) = create(model.clone(), CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("seed")).await.unwrap();
    finished(&mut events).await;
    session.compact().await.unwrap();
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed),
        "ContextLength on a summary must retry with a smaller chunk"
    );
    {
        let requests = model.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        let transcript = |index: usize| {
            let Message::User { text } = requests[index].messages.last().unwrap() else {
                unreachable!()
            };
            text.split_once("\n<transcript>\n")
                .unwrap()
                .1
                .split_once("\n</transcript>\n")
                .unwrap()
                .0
                .to_owned()
        };
        assert!(transcript(2).len() < transcript(1).len());
        assert_eq!(
            transcript(2) + &transcript(3),
            format!("\nUser: seed\n\nAssistant: {old_reply}\n")
        );
        assert!(
            matches!(requests[3].messages.last().unwrap(), Message::User { text } if text.contains("<earlier_summary>\nfirst half\n</earlier_summary>"))
        );
    }
    session.input(input("continue")).await.unwrap();
    finished(&mut events).await;
    assert!(model.requests.lock().unwrap()[4].messages.iter().any(|message| matches!(message, Message::User { text } if text == "[Context summary]\ncomplete summary")));
    session.close().await.unwrap();
}

#[tokio::test]
async fn repeatedly_oversized_summary_stops_and_preserves_the_original_history() {
    let mut scripts = vec![vec![text("original answer"), finish()]];
    scripts.extend((0..32).map(|_| vec![Step::Error(ErrorKind::ContextLength)]));
    let model = ScriptModel::new(scripts);
    let (_temp, store, session) = create(model.clone(), CoreOptions::default()).await;
    let mut events = session.subscribe(0).await.unwrap();
    session.input(input("seed")).await.unwrap();
    finished(&mut events).await;
    let original = history_messages(&store);
    session.compact().await.unwrap();
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Failed)
    );
    let attempts = model.requests.lock().unwrap().len() - 1;
    assert!(
        (2..=10).contains(&attempts),
        "summary retries must shrink to a bounded failure: {attempts}"
    );
    assert_eq!(history_messages(&store), original);
    assert!(
        !store
            .model_history::<serde_json::Value>("s1")
            .unwrap()
            .iter()
            .any(|entry| entry["type"] == "compaction")
    );
    *model.scripts.lock().unwrap() = VecDeque::from([vec![text("continued"), finish()]]);
    session.input(input("continue")).await.unwrap();
    finished(&mut events).await;
    assert!(
        model
            .requests
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .messages
            .windows(original.len())
            .any(|messages| messages == original)
    );
    session.close().await.unwrap();
}
