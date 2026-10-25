use canter_core::{CoreOptions, RuntimeContext, Session};
use canter_model::{
    AssistantContent, FinishReason, Message, Model, ModelEvent, ModelStream, Request, ToolCall,
};
use canter_protocol::*;
use canter_store::Store;
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct ScriptModel {
    scripts: Mutex<VecDeque<Vec<ModelEvent>>>,
    requests: Mutex<Vec<Request>>,
}
impl ScriptModel {
    fn new(scripts: Vec<Vec<ModelEvent>>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into()),
            requests: Mutex::new(Vec::new()),
        })
    }
}
impl Model for ScriptModel {
    fn stream(&self, request: Request, cancel: CancellationToken) -> ModelStream<'_> {
        self.requests.lock().unwrap().push(request);
        let events = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected model request");
        Box::pin(async_stream::stream! {
            let complete = events.iter().any(|event| matches!(event, ModelEvent::Finished(_)));
            for event in events { yield Ok(event); }
            if !complete { cancel.cancelled().await; }
        })
    }
}

fn assert_request_extension(previous: &Request, next: &Request) {
    assert!(next.messages.len() > previous.messages.len());
    let mut prefix = next.clone();
    prefix.messages.truncate(previous.messages.len());
    assert_eq!(
        serde_json::to_string(&prefix).unwrap(),
        serde_json::to_string(previous).unwrap(),
        "messages, tools, model and options must stay byte-for-byte unchanged"
    );
}

#[tokio::test]
async fn request_prefix_is_frozen_until_reopening_and_only_compaction_replaces_history() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("AGENTS.md");
    std::fs::write(&path, "original instructions").unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let reply = || {
        vec![
            ModelEvent::TextDelta("answer".into()),
            ModelEvent::Finished(FinishReason::Stop),
        ]
    };
    let model = ScriptModel::new(vec![
        reply(),
        reply(),
        reply(),
        reply(),
        reply(),
        vec![
            call("denied", "shell", r#"{"command":"touch must-not-exist"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        reply(),
        reply(),
        reply(),
        reply(),
    ]);
    let mut config = settings(temp.path());
    config.approval_policy = PermissionMode::AlwaysAsk;
    let (mut session, _) = Session::create(
        store.clone(),
        "s1".into(),
        config,
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    std::fs::write(&path, "changed before first request").unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    for text in ["first", "second"] {
        session.input(input(text)).await.unwrap();
        finished(&mut events).await;
    }
    std::fs::write(&path, "changed instructions").unwrap();
    session
        .input(input("after editing AGENTS.md"))
        .await
        .unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        assert!(matches!(&requests[0].messages[0], Message::System { text }
            if text.contains("original instructions") && !text.contains("changed before first request")));
        for pair in requests.windows(2) {
            assert_request_extension(&pair[0], &pair[1]);
        }
    }
    // Reopening with unchanged instructions extends the last request too.
    std::fs::write(&path, "original instructions").unwrap();
    let closed = session.close().await.unwrap();
    drop(session);
    (session, _) = Session::open(
        store.clone(),
        "s1".into(),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    events = session.subscribe(closed.event_seq).await.unwrap();
    session.input(input("reopened unchanged")).await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        assert_request_extension(&requests[2], &requests[3]);
    }
    assert_eq!(
        store
            .model_history::<serde_json::Value>("s1")
            .unwrap()
            .iter()
            .filter(|entry| entry["type"] == "context")
            .count(),
        1
    );

    std::fs::write(&path, "changed instructions").unwrap();
    let closed = session.close().await.unwrap();
    drop(session);
    (session, _) = Session::open(
        store.clone(),
        "s1".into(),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    events = session.subscribe(closed.event_seq).await.unwrap();
    // The read happens on open, not on the first request.
    std::fs::write(&path, "later instructions").unwrap();
    session.input(input("reopened changed")).await.unwrap();
    finished(&mut events).await;
    {
        let requests = model.requests.lock().unwrap();
        assert!(matches!(&requests[4].messages[0], Message::System { text }
            if text.contains("changed instructions") && !text.contains("later instructions")));
        let mut previous = requests[3].clone();
        previous.messages[0] = requests[4].messages[0].clone();
        assert_request_extension(&previous, &requests[4]);
    }
    assert_eq!(
        store
            .model_history::<serde_json::Value>("s1")
            .unwrap()
            .iter()
            .filter(|entry| entry["type"] == "context")
            .count(),
        2
    );

    session.input(input("ask for approval")).await.unwrap();
    let opened = until(&mut events, |event| {
        matches!(event, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = opened.payload else {
        unreachable!()
    };
    session
        .resolve(
            &request.id,
            RequestResolveParams {
                decision: ApprovalDecision::Decline,
                client_name: "terminal".into(),
            },
        )
        .await
        .unwrap();
    finished(&mut events).await;
    assert!(!temp.path().join("must-not-exist").exists());
    session.compact().await.unwrap();
    finished(&mut events).await;
    for text in ["after compaction", "continue"] {
        session.input(input(text)).await.unwrap();
        finished(&mut events).await;
    }
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 10);
    for pair in requests[4..7].windows(2) {
        assert_request_extension(&pair[0], &pair[1]);
    }
    assert!(requests[6].messages.iter().any(|message|
        matches!(message, Message::ToolResult { tool_call_id, .. } if tool_call_id == "denied")));
    assert!(requests[7].tools.is_empty(), "independent summary request");
    assert_eq!(requests[7].messages[0], requests[6].messages[0]);
    assert_eq!(requests[8].messages[0], requests[6].messages[0]);
    assert!(requests[8].messages.iter().any(
        |message| matches!(message, Message::User { text } if text.contains("[Context summary]"))
    ));
    assert_request_extension(&requests[8], &requests[9]);
    session.close().await.unwrap();
}

async fn until(
    events: &mut canter_core::Subscription,
    predicate: impl Fn(&EventPayload) -> bool,
) -> Event {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let event = events.recv().await.unwrap().unwrap();
            if predicate(&event.payload) {
                return event;
            }
            assert!(
                !matches!(event.payload, EventPayload::TurnCompleted { .. }),
                "turn ended before expected event: {event:?}"
            );
        }
    })
    .await
    .expect("expected event did not arrive")
}

#[tokio::test]
async fn client_close_conflicts_only_when_shutdown_closed_first() {
    for (shutdown_first, expected_reason) in [
        (false, SessionCloseReason::ClientRequest),
        (true, SessionCloseReason::ServerShutdown),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
        let (session, _) = Session::create(
            store,
            "s1".into(),
            settings(temp.path()),
            RuntimeContext::default(),
            ScriptModel::new(vec![]),
            CoreOptions::default(),
        )
        .await
        .unwrap();

        let closed = if shutdown_first {
            session.close_for_shutdown().await.unwrap()
        } else {
            session.close().await.unwrap()
        };
        if shutdown_first {
            let error = session.close().await.expect_err(
                "a client close must not report success for a session closed by shutdown",
            );
            assert_eq!(error.code, ErrorCode::Conflict);
            assert_eq!(error.message, "server is stopping");
        } else {
            let shutdown = session.close_for_shutdown().await.unwrap();
            assert_eq!(shutdown.event_seq, closed.event_seq);
        }
        let snapshot = session.snapshot().await.session;
        assert_eq!(snapshot.status, SessionStatus::Closed);
        assert_eq!(snapshot.close_reason, Some(expected_reason));
        assert_eq!(snapshot.last_event_seq, closed.event_seq);
    }
}

#[tokio::test]
async fn interrupt_during_model_stream_repairs_history_and_discards_queued_input() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            ModelEvent::TextDelta("partial".into()),
            call(
                "unfinished",
                "shell",
                r#"{"command":"touch must-not-exist"}"#,
            ),
        ],
        vec![
            ModelEvent::TextDelta("continued".into()),
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let (session, _) = Session::create(
        store.clone(),
        "s1".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let first = session.input(input("start")).await.unwrap();
    until(&mut events, |payload| {
        matches!(payload, EventPayload::ItemDelta { .. })
    })
    .await;
    session.input(input("discard this")).await.unwrap();
    let interrupted = session
        .interrupt(SessionInterruptParams {
            turn_id: first.turn_id.unwrap(),
        })
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)
    );
    assert_eq!(seen.last().unwrap().seq, interrupted.event_seq);
    assert!(seen.iter().any(|event| matches!(
        &event.payload,
        EventPayload::InputRemoved {
            reason: InputRemovalReason::Interrupted,
            ..
        }
    )));
    let snapshot = session.snapshot().await.session;
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert!(snapshot.queued_inputs.is_empty());
    session.input(input("continue")).await.unwrap();
    finished(&mut events).await;
    let requests = model.requests.lock().unwrap().clone();
    assert_request_extension(&requests[0], &requests[1]);
    assert!(
        requests[1].messages.iter().any(
            |message| matches!(message, Message::User { text } if text.contains("interrupted"))
        )
    );
    assert!(requests[1].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "unfinished" && text.contains("interrupted"))));
    assert!(
        !requests[1]
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { text } if text == "discard this"))
    );
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn interrupt_while_waiting_for_approval_cancels_the_request_without_running_tools() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![vec![
        call("pending", "shell", r#"{"command":"touch must-not-exist"}"#),
        ModelEvent::Finished(FinishReason::ToolCalls),
    ]]);
    let mut config = settings(temp.path());
    config.approval_policy = PermissionMode::AlwaysAsk;
    let (session, _) = Session::create(
        store,
        "s1".into(),
        config,
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let response = session.input(input("run")).await.unwrap();
    until(&mut events, |payload| {
        matches!(payload, EventPayload::RequestOpened { .. })
    })
    .await;
    assert_eq!(
        session.snapshot().await.session.status,
        SessionStatus::WaitingApproval
    );
    session
        .interrupt(SessionInterruptParams {
            turn_id: response.turn_id.unwrap(),
        })
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(
        event.payload,
        EventPayload::RequestResolved {
            resolution: ApprovalResolution::Cancelled,
            ..
        }
    )));
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)
    );
    assert!(session.snapshot().await.session.pending_requests.is_empty());
    assert!(!temp.path().join("must-not-exist").exists());
    session.close().await.unwrap();
}

#[tokio::test]
async fn two_clients_resolve_once_and_steering_precedes_the_next_model_request() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            call("run", "shell", r#"{"command":"printf x >> count"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            ModelEvent::TextDelta("steered answer".into()),
            ModelEvent::Finished(FinishReason::Stop),
        ],
        vec![
            ModelEvent::TextDelta("queued answer".into()),
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let mut config = settings(temp.path());
    config.approval_policy = PermissionMode::AskForExecution;
    let (session, _) = Session::create(
        store.clone(),
        "s1".into(),
        config,
        RuntimeContext::default(),
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let first = session.input(input("run")).await.unwrap();
    let opened = until(&mut events, |payload| {
        matches!(payload, EventPayload::RequestOpened { .. })
    })
    .await;
    let EventPayload::RequestOpened { request } = opened.payload else {
        unreachable!()
    };
    let queued = session.input(input("queued follow-up")).await.unwrap();
    let steer = session
        .input(SessionInputParams {
            mode: InputMode::Steer,
            ..input("change plan")
        })
        .await
        .unwrap();
    assert_eq!(steer.turn_id, first.turn_id);
    assert!(queued.turn_id.is_none());
    let params = RequestResolveParams {
        decision: ApprovalDecision::Accept,
        client_name: "client-a".into(),
    };
    let (a, b) = tokio::join!(
        session.resolve(&request.id, params.clone()),
        session.resolve(
            &request.id,
            RequestResolveParams {
                client_name: "client-b".into(),
                ..params
            }
        )
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!(a.event_seq, b.event_seq);
    assert_ne!(a.already_resolved, b.already_resolved);
    let mut seen = finished(&mut events).await;
    seen.extend(finished(&mut events).await);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("count")).unwrap(),
        "x"
    );
    assert_eq!(
        seen.iter()
            .filter(|event| matches!(event.payload, EventPayload::RequestResolved { .. }))
            .count(),
        1
    );
    assert!(seen.iter().any(|event| event.seq == steer.event_seq
        && matches!(event.payload, EventPayload::InputQueued { .. })));
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::InputRemoved { reason: InputRemovalReason::Steered, turn_id, .. } if turn_id == &first.turn_id)));
    assert!(session.snapshot().await.session.queued_inputs.is_empty());
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    for pair in requests.windows(2) {
        assert_request_extension(&pair[0], &pair[1]);
    }
    assert!(
        requests[1]
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { text } if text == "change plan"))
    );
    assert!(
        !requests[1]
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { text } if text == "queued follow-up"))
    );
    assert!(
        matches!(requests[2].messages.last(), Some(Message::User { text }) if text == "queued follow-up")
    );
    session.close().await.unwrap();
}

#[tokio::test]
async fn interrupt_during_a_command_waits_until_its_process_group_is_gone() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![vec![
        call(
            "running",
            "shell",
            r#"{"command":"sleep 60 & printf '%s %s\\n' \"$$\" \"$!\"; wait"}"#,
        ),
        ModelEvent::Finished(FinishReason::ToolCalls),
    ]]);
    let (session, _) = Session::create(
        store,
        "s1".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model,
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let response = session.input(input("run")).await.unwrap();
    let output = until(&mut events, |payload| {
        matches!(
            payload,
            EventPayload::ItemDelta {
                delta: ItemDelta::CommandExecution { .. },
                ..
            }
        )
    })
    .await;
    let EventPayload::ItemDelta {
        delta: ItemDelta::CommandExecution { delta },
        ..
    } = output.payload
    else {
        unreachable!()
    };
    let pids: Vec<i32> = delta
        .split_whitespace()
        .map(|pid| pid.parse().unwrap())
        .collect();
    assert_eq!(pids.len(), 2);
    tokio::time::timeout(
        Duration::from_secs(5),
        session.interrupt(SessionInterruptParams {
            turn_id: response.turn_id.unwrap(),
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let seen = finished(&mut events).await;
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Interrupted)
    );
    for pid in pids {
        tokio::time::timeout(Duration::from_secs(5), async {
            while nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("command process survived interruption");
    }
    session.close().await.unwrap();
}

#[tokio::test]
async fn reopening_restores_the_exact_model_context_and_skips_future_history_entries() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("project/subdir");
    std::fs::create_dir_all(&cwd).unwrap();
    std::fs::create_dir(temp.path().join("project/.git")).unwrap();
    std::fs::write(temp.path().join("project/AGENTS.md"), "root instruction").unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "nested instruction").unwrap();
    let mut contexts = Vec::new();
    for restore in [false, true] {
        let root = temp
            .path()
            .join(if restore { "restored" } else { "continuous" });
        let store = Arc::new(Store::new(&root).unwrap());
        let model = ScriptModel::new(vec![
            vec![
                ModelEvent::ReasoningDelta("read instructions".into()),
                call("read", "read", "{ \"path\": \"AGENTS.md\" }"),
                ModelEvent::Finished(FinishReason::ToolCalls),
            ],
            vec![
                ModelEvent::TextDelta("first answer".into()),
                ModelEvent::Finished(FinishReason::Stop),
            ],
            vec![
                ModelEvent::TextDelta("second answer".into()),
                ModelEvent::Finished(FinishReason::Stop),
            ],
        ]);
        let runtime = || RuntimeContext {
            environment: BTreeMap::from([("PRIVATE_FIXTURE".into(), "not-for-disk".into())]),
            protected_paths: vec![root.clone()],
        };
        let (mut session, _) = Session::create(
            store.clone(),
            "s1".into(),
            settings(&cwd),
            runtime(),
            model.clone(),
            CoreOptions::default(),
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session.input(input("read instructions")).await.unwrap();
        finished(&mut events).await;
        if restore {
            let closed = session.close().await.unwrap();
            drop(session);
            let mut writer = store.open("s1").unwrap();
            writer
                .append_model_history(
                    &serde_json::json!({"type":"future_history_kind", "payload":{"new":true}}),
                )
                .unwrap();
            writer.append_model_history(&serde_json::json!({"type":"message", "message":{"type":"future_message_kind"}})).unwrap();
            drop(writer);
            (session, _) = Session::open(
                store,
                "s1".into(),
                runtime(),
                model.clone(),
                CoreOptions::default(),
            )
            .await
            .unwrap();
            events = session.subscribe(closed.event_seq).await.unwrap();
        }
        session.input(input("continue")).await.unwrap();
        finished(&mut events).await;
        contexts.push(model.requests.lock().unwrap()[2].clone());
        assert!(contexts.last().unwrap().messages.iter().any(|message| matches!(message, Message::System { text } if text.contains("root instruction") && text.contains("nested instruction"))));
        session.close().await.unwrap();
        for file in ["events.jsonl", "model-history.jsonl"] {
            assert!(
                !std::fs::read_to_string(root.join("s1").join(file))
                    .unwrap()
                    .contains("not-for-disk")
            );
        }
        assert!(!root.join("s1/private.json").exists());
    }
    assert_eq!(contexts[0], contexts[1]);
}

#[tokio::test]
async fn automatic_compaction_keeps_recent_user_words_and_can_continue_after_reopening() {
    let temp = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            ModelEvent::TextDelta("old output ".repeat(16_000)),
            ModelEvent::Finished(FinishReason::Stop),
        ],
        vec![
            ModelEvent::TextDelta(
                "Summary: first task completed; continue with the second task.".into(),
            ),
            ModelEvent::Finished(FinishReason::Stop),
        ],
        vec![
            ModelEvent::TextDelta("after compaction".into()),
            ModelEvent::Finished(FinishReason::Stop),
        ],
        vec![
            ModelEvent::TextDelta("resumed".into()),
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let options = CoreOptions {
        context_window: 100_000,
        max_output_tokens: 32_768,
        thinking: false,
    };
    let (session, _) = Session::create(
        store.clone(),
        "s1".into(),
        settings(temp.path()),
        RuntimeContext::default(),
        model.clone(),
        options.clone(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    session
        .input(input("first task, preserve these exact words"))
        .await
        .unwrap();
    finished(&mut events).await;
    std::fs::write(
        temp.path().join("AGENTS.md"),
        "changed before automatic compaction",
    )
    .unwrap();
    session
        .input(input("continue with the second task"))
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert!(seen.iter().any(|event| matches!(&event.payload, EventPayload::ItemCompleted { item: Item::ContextCompaction { summary, .. }, .. } if summary.contains("first task completed"))));
    let requests = model.requests.lock().unwrap().clone();
    assert!(
        requests[1].tools.is_empty(),
        "summarization must not execute tools"
    );
    let compacted = &requests[2].messages;
    assert!(compacted.iter().any(|message| matches!(message, Message::User { text } if text == "first task, preserve these exact words")));
    assert!(compacted.iter().any(|message| matches!(message, Message::User { text } if text == "continue with the second task")));
    assert!(!format!("{compacted:?}").contains("old output old output"));
    std::fs::remove_file(temp.path().join("AGENTS.md")).unwrap();
    let closed = session.close().await.unwrap();
    drop(session);
    let (session, _) = Session::open(
        store,
        "s1".into(),
        RuntimeContext::default(),
        model.clone(),
        options,
    )
    .await
    .unwrap();
    let mut events = session.subscribe(closed.event_seq).await.unwrap();
    session.input(input("resume")).await.unwrap();
    finished(&mut events).await;
    let requests = model.requests.lock().unwrap().clone();
    assert_request_extension(&requests[2], &requests[3]);
    assert_eq!(requests[0].messages[0], requests[1].messages[0]);
    assert_eq!(requests[0].messages[0], requests[2].messages[0]);
    let resumed = requests[3].messages.clone();
    assert_eq!(&resumed[..compacted.len()], compacted);
    session.close().await.unwrap();
}
fn call(id: &str, name: &str, arguments: &str) -> ModelEvent {
    ModelEvent::ToolCall(ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
    })
}
fn input(text: &str) -> SessionInputParams {
    SessionInputParams {
        input: vec![UserInput::Text { text: text.into() }],
        mode: InputMode::Queue,
        client_name: "terminal".into(),
    }
}
fn settings(cwd: &std::path::Path) -> SessionSettings {
    SessionSettings {
        cwd: cwd.to_string_lossy().into(),
        name: None,
        model: "fake".into(),
        approval_policy: PermissionMode::AllowAll,
    }
}
async fn finished(events: &mut canter_core::Subscription) -> Vec<Event> {
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut seen = Vec::new();
        loop {
            let event = events
                .recv()
                .await
                .unwrap()
                .expect("event stream closed early");
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

#[tokio::test]
async fn one_turn_runs_multiple_real_tools_and_publishes_the_durable_events() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("work");
    std::fs::create_dir(&cwd).unwrap();
    std::fs::write(cwd.join("AGENTS.md"), "Keep edits small.").unwrap();
    let store = Arc::new(Store::new(temp.path().join("sessions")).unwrap());
    let model = ScriptModel::new(vec![
        vec![
            ModelEvent::ReasoningDelta("Inspect and edit.".into()),
            call(
                "w",
                "write",
                "{ \"path\": \"file\", \"content\": \"before\" }",
            ),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            call(
                "e",
                "edit",
                r#"{"path":"file","edits":[{"oldText":"before","newText":"after"}]}"#,
            ),
            call("r", "read", r#"{"path":"file"}"#),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            call(
                "s",
                "shell",
                r#"{"command":"printf '%s' \"$CORE_TEST_VALUE\""}"#,
            ),
            ModelEvent::Finished(FinishReason::ToolCalls),
        ],
        vec![
            ModelEvent::TextDelta("Done".into()),
            ModelEvent::Finished(FinishReason::Stop),
        ],
    ]);
    let runtime = RuntimeContext {
        environment: BTreeMap::from([("CORE_TEST_VALUE".into(), "isolated".into())]),
        protected_paths: vec![temp.path().join("sessions")],
    };
    let (session, created) = Session::create(
        store.clone(),
        "s1".into(),
        settings(&cwd),
        runtime,
        model.clone(),
        CoreOptions::default(),
    )
    .await
    .unwrap();
    let mut events = session.subscribe(0).await.unwrap();
    let response = session
        .input(input("edit then read and run a command"))
        .await
        .unwrap();
    let seen = finished(&mut events).await;
    assert_eq!(seen[created.event_seq as usize - 1].seq, created.event_seq);
    assert!(seen.iter().any(|event| event.seq == response.event_seq
        && matches!(event.payload, EventPayload::TurnStarted { .. })));
    assert_eq!(std::fs::read_to_string(cwd.join("file")).unwrap(), "after");
    assert!(
        matches!(&seen.last().unwrap().payload, EventPayload::TurnCompleted { turn } if turn.status == TurnStatus::Completed)
    );
    let disk = store.events_after("s1", 0).unwrap();
    assert_eq!(&disk[..seen.len()], seen.as_slice());
    assert!(seen.windows(2).all(|pair| pair[1].seq == pair[0].seq + 1));
    let requests = model.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 4);
    for pair in requests.windows(2) {
        assert_request_extension(&pair[0], &pair[1]);
    }
    assert!(requests[0].messages.iter().any(|message| matches!(message, Message::System { text } if text.contains("Keep edits small.") && text.contains(&*cwd.to_string_lossy()))));
    assert!(requests[1].messages.iter().any(|message| matches!(message, Message::Assistant { content } if content.contains(&AssistantContent::Reasoning { text: "Inspect and edit.".into() }))));
    assert!(requests[2].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "r" && text.contains("1: after"))));
    assert!(requests[3].messages.iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, text } if tool_call_id == "s" && text.contains("isolated"))));
    assert!(!temp.path().join("sessions/s1/private.json").exists());
    session.close().await.unwrap();
}
