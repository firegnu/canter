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
                    Step::Error(kind) => yield Err(ModelError { kind, status: None, retry_after: None }),
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
    Step::Event(ModelEvent::TextDelta(value.into()))
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
        Step::Event(ModelEvent::ReasoningDelta("reasoning".into())),
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
                    decision: ApprovalDecision::Cancel
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
            text.split_once("\n\nTranscript:\n").unwrap().1.to_owned()
        };
        assert!(transcript(2).len() < transcript(1).len());
        assert_eq!(
            transcript(2) + &transcript(3),
            format!("\nUser: seed\n\nAssistant: {old_reply}\n")
        );
        assert!(
            matches!(requests[3].messages.last().unwrap(), Message::User { text } if text.contains("Earlier summary:\nfirst half"))
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
