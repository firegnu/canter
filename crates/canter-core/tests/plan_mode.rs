use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{FinishReason, Model, ModelEvent, ModelStream, Request, ToolCall};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
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
    fn stream(&self, request: Request, _: CancellationToken) -> ModelStream<'_> {
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
fn calls(calls: &[(&str, Value)]) -> Vec<ModelEvent> {
    let mut events: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            ModelEvent::ToolCall(ToolCall {
                id: format!("call-{i}"),
                name: (*name).into(),
                arguments: args.to_string(),
            })
        })
        .collect();
    events.push(ModelEvent::Finished(FinishReason::ToolCalls));
    events
}
fn done() -> Vec<ModelEvent> {
    vec![ModelEvent::Finished(FinishReason::Stop)]
}
struct Fixture {
    root: tempfile::TempDir,
    store: Arc<Store>,
    session: Session,
}
impl Fixture {
    async fn reopen(&self, model: Arc<dyn Model>) -> Session {
        let runtime = RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    self.root.path().join("home").to_string_lossy().into_owned(),
                ),
                (
                    "CANTER_HOME".into(),
                    self.root
                        .path()
                        .join("state")
                        .to_string_lossy()
                        .into_owned(),
                ),
            ]
            .into(),
            ..Default::default()
        };
        Session::open(
            self.store.clone(),
            "s1".into(),
            runtime,
            model,
            CoreOptions::default(),
        )
        .await
        .unwrap()
        .0
    }

    async fn new(model: Arc<dyn Model>) -> Self {
        let root = tempfile::tempdir().unwrap();
        for name in ["home", "project", "state"] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        let store = Arc::new(Store::new(root.path().join("state/sessions")).unwrap());
        let settings = serde_json::from_value(json!({"cwd":root.path().join("project"),"model":"fake","name":null,"approvalPolicy":"allowAll"})).unwrap();
        let runtime = RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    root.path().join("home").to_string_lossy().into_owned(),
                ),
                (
                    "CANTER_HOME".into(),
                    root.path().join("state").to_string_lossy().into_owned(),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let (session, _) = Session::create(
            store.clone(),
            "s1".into(),
            settings,
            runtime,
            model,
            CoreOptions::default(),
        )
        .await
        .unwrap();
        Self {
            root,
            store,
            session,
        }
    }
    async fn update(&self, params: Value) {
        self.session
            .update(serde_json::from_value(params).unwrap())
            .await
            .unwrap();
    }
    async fn start(&self) -> Subscription {
        let mut events = self
            .session
            .subscribe(self.session.snapshot().await.event_seq)
            .await
            .unwrap();
        self.session
            .input(
                serde_json::from_value(
                    json!({"input":[{"type":"text","text":"work"}],"clientName":"test"}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        until(&mut events, |payload| {
            matches!(payload, EventPayload::TurnStarted { .. })
        })
        .await;
        events
    }
}
async fn until(events: &mut Subscription, predicate: impl Fn(&EventPayload) -> bool) -> Vec<Event> {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut seen = Vec::new();
        loop {
            let event = events.recv().await.unwrap().unwrap();
            let found = predicate(&event.payload);
            seen.push(event);
            if found {
                return seen;
            }
        }
    })
    .await
    .expect("expected event")
}

#[tokio::test]
async fn permission_updates_in_plan_mode_set_the_resume_and_proposal_default_policy() {
    for policy in ["alwaysAsk", "askForExecution", "allowAll"] {
        let model = ScriptModel::new(vec![
            calls(&[("submit_plan", json!({"plan":"Investigate and implement."}))]),
            done(),
        ]);
        let fixture = Fixture::new(model).await;
        fixture.update(json!({"planMode":true})).await;
        fixture.update(json!({"approvalPolicy":policy})).await;
        let settings = fixture.session.snapshot().await.session.settings;
        assert!(settings.plan_mode);
        assert_eq!(
            serde_json::to_value(settings.pre_plan_approval_policy).unwrap(),
            policy,
            "the latest permission selection must survive leaving Plan mode"
        );
        let mut events = fixture.start().await;
        let seen = until(&mut events, |p| {
            matches!(p, EventPayload::RequestOpened { .. })
        })
        .await;
        let request = seen
            .iter()
            .find_map(|event| match &event.payload {
                EventPayload::RequestOpened { request } => Some(request),
                _ => None,
            })
            .unwrap();
        let RequestKind::PlanApproval(proposal) = &request.kind else {
            panic!("plan approval");
        };
        assert_eq!(
            serde_json::to_value(proposal.approval_policy).unwrap(),
            if policy == "allowAll" {
                "alwaysAsk"
            } else {
                policy
            }
        );
        fixture.update(json!({"planMode":false})).await;
        until(&mut events, |p| {
            matches!(p, EventPayload::TurnCompleted { .. })
        })
        .await;
        assert_eq!(
            serde_json::to_value(
                fixture
                    .session
                    .snapshot()
                    .await
                    .session
                    .settings
                    .approval_policy
            )
            .unwrap(),
            policy
        );
        fixture.session.close().await.unwrap();
    }
}

#[tokio::test]
async fn compacting_omits_mode_notices_and_restates_current_mode_after_replay() {
    for enabled in [false, true] {
        let model = ScriptModel::new(vec![
            done(),
            vec![
                ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: "Summary: investigate the requested change.".into(),
                },
                ModelEvent::Finished(FinishReason::Stop),
            ],
            done(),
            done(),
        ]);
        let mut fixture = Fixture::new(model.clone()).await;
        let mut events = fixture.start().await;
        until(&mut events, |p| {
            matches!(p, EventPayload::TurnCompleted { .. })
        })
        .await;
        fixture.update(json!({"planMode":true})).await;
        if !enabled {
            fixture.update(json!({"planMode":false})).await;
        }
        let mut events = fixture
            .session
            .subscribe(fixture.session.snapshot().await.event_seq)
            .await
            .unwrap();
        fixture.session.compact().await.unwrap();
        until(&mut events, |p| {
            matches!(p, EventPayload::TurnCompleted { .. })
        })
        .await;
        {
            let requests = model.requests.lock().unwrap();
            let transcript = format!("{:?}", requests[1].messages);
            assert!(transcript.contains("User: work"));
            assert!(
                !transcript.contains("[Plan mode"),
                "hidden mode notices are not user speech for compaction"
            );
        }
        let mut events = fixture.start().await;
        until(&mut events, |p| {
            matches!(p, EventPayload::TurnCompleted { .. })
        })
        .await;
        let expected = model.requests.lock().unwrap()[2].messages.clone();
        let notice = matches!(&expected[expected.len() - 2], canter_model::Message::User { text } if text.contains("[Plan mode enabled]"));
        assert_eq!(
            notice, enabled,
            "compaction must restate the current planning mode independently of the summary"
        );
        fixture.session.close().await.unwrap();
        fixture.session = fixture.reopen(model.clone()).await;
        let mut events = fixture.start().await;
        until(&mut events, |p| {
            matches!(p, EventPayload::TurnCompleted { .. })
        })
        .await;
        assert_eq!(
            &model.requests.lock().unwrap()[3].messages[..expected.len()],
            expected
        );
        fixture.session.close().await.unwrap();
    }
}

#[tokio::test]
async fn rewinding_in_plan_mode_restates_the_mode_and_replays_the_same_history() {
    let model = ScriptModel::new(vec![done(), done(), done()]);
    let mut fixture = Fixture::new(model.clone()).await;
    let mut events = fixture.start().await;
    until(&mut events, |p| {
        matches!(p, EventPayload::TurnCompleted { .. })
    })
    .await;
    let target = fixture.session.snapshot().await.session.turns[0].id.clone();
    fixture.update(json!({"planMode":true})).await;
    let mut params: SessionRewindParams = serde_json::from_value(json!({
        "beforeTurnId":target,"rewindFiles":false,"rewindConversation":true
    }))
    .unwrap();
    params.last_event_seq = Some(
        fixture
            .session
            .rewind_preview(params.clone())
            .await
            .unwrap()
            .last_event_seq,
    );
    fixture.session.rewind(params).await.unwrap();
    let mut events = fixture.start().await;
    until(&mut events, |p| {
        matches!(p, EventPayload::TurnCompleted { .. })
    })
    .await;
    let expected = model.requests.lock().unwrap()[1].messages.clone();
    assert!(
        matches!(&expected[expected.len() - 2], canter_model::Message::User { text } if text.contains("[Plan mode enabled]")),
        "rewind must restate the current mode before the next input"
    );
    fixture.session.close().await.unwrap();
    fixture.session = fixture.reopen(model.clone()).await;
    assert!(fixture.session.snapshot().await.session.settings.plan_mode);
    let mut events = fixture.start().await;
    until(&mut events, |p| {
        matches!(p, EventPayload::TurnCompleted { .. })
    })
    .await;
    assert_eq!(
        &model.requests.lock().unwrap()[2].messages[..expected.len()],
        expected
    );
    fixture.session.close().await.unwrap();
}

#[tokio::test]
async fn clear_context_with_queued_input_and_steering_leaves_the_original_idle() {
    let model = ScriptModel::new(vec![
        calls(&[(
            "submit_plan",
            json!({"plan":"Implement the approved change."}),
        )]),
        calls(&[(
            "write",
            json!({"path":"unexpected","content":"must not run"}),
        )]),
        done(),
    ]);
    let fixture = Fixture::new(model.clone()).await;
    fixture.update(json!({"planMode":true})).await;
    let mut events = fixture.start().await;
    let seen = until(&mut events, |p| {
        matches!(p, EventPayload::RequestOpened { .. })
    })
    .await;
    let request = seen
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RequestOpened { request } => Some(request),
            _ => None,
        })
        .unwrap();
    for (mode, text) in [
        ("queue", "Queued follow-up"),
        ("steer", "Steering correction"),
    ] {
        fixture
            .session
            .input(
                serde_json::from_value(json!({
                    "input":[{"type":"text","text":text}],"mode":mode,"clientName":"other terminal"
                }))
                .unwrap(),
            )
            .await
            .unwrap();
    }
    let queued = fixture.session.snapshot().await.session.queued_inputs;
    assert_eq!(queued.len(), 2);
    fixture
        .session
        .resolve(
            &request.id,
            serde_json::from_value(json!({
                "plan":{"decision":"clearContext","approvalPolicy":"allowAll"},"clientName":"test"
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let seen = until(&mut events, |p| {
        matches!(
            p,
            EventPayload::SessionStatus {
                status: SessionStatus::Idle,
                ..
            }
        )
    })
    .await;
    let snapshot = fixture.session.snapshot().await.session;
    assert!(
        snapshot.queued_inputs.is_empty(),
        "clearContext must withdraw both queued input and steering"
    );
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert_eq!(snapshot.turns.len(), 1);
    assert_eq!(snapshot.turns[0].status, TurnStatus::Completed);
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert!(!fixture.root.path().join("project/unexpected").exists());
    let removed: Vec<_> = seen
        .iter()
        .filter_map(|event| match &event.payload {
            EventPayload::InputRemoved {
                input_ids,
                reason: InputRemovalReason::Interrupted,
                ..
            } => Some(input_ids),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(
        removed,
        queued.iter().map(|input| &input.id).collect::<Vec<_>>()
    );
    fixture.session.close().await.unwrap();
}

#[tokio::test]
async fn plan_mode_blocks_mutation_without_approval_and_preserves_read_tools() {
    let model = ScriptModel::new(vec![
        calls(&[
            ("write", json!({"path":"fixture.txt","content":"changed"})),
            (
                "edit",
                json!({"path":"fixture.txt","edits":[{"oldText":"original","newText":"changed"}]}),
            ),
            ("shell", json!({"command":"touch changed"})),
            (
                "update_plan",
                json!({"plan":[{"step":"work","status":"pending"}]}),
            ),
            ("read", json!({"path":"fixture.txt"})),
            ("shell", json!({"command":"pwd"})),
        ]),
        done(),
    ]);
    let fixture = Fixture::new(model.clone()).await;
    std::fs::write(fixture.root.path().join("project/fixture.txt"), "original").unwrap();
    fixture.update(json!({"planMode":true})).await;
    let settings = serde_json::to_value(fixture.session.snapshot().await.session.settings).unwrap();
    assert_eq!(
        settings["planMode"], true,
        "plan mode must be a persisted session setting"
    );
    let mut events = fixture.start().await;
    let seen = until(&mut events, |p| {
        matches!(p, EventPayload::TurnCompleted { .. })
    })
    .await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RequestOpened { .. }))
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("project/fixture.txt")).unwrap(),
        "original"
    );
    assert!(!fixture.root.path().join("project/changed").exists());
    let items: Vec<_> = seen
        .iter()
        .filter_map(|e| match &e.payload {
            EventPayload::ItemCompleted { item, .. } => Some(serde_json::to_value(item).unwrap()),
            _ => None,
        })
        .collect();
    assert_eq!(
        items
            .iter()
            .filter(|item| item["status"] == "blocked")
            .count(),
        4
    );
    assert_eq!(
        items
            .iter()
            .filter(|item| item["status"] == "completed")
            .count(),
        2
    );
    let requests = model.requests.lock().unwrap();
    assert_eq!(requests[0].tools, requests[1].tools);
    assert!(
        requests[1]
            .messages
            .iter()
            .filter_map(|m| match m {
                canter_model::Message::ToolResult { text, .. } => Some(text),
                _ => None,
            })
            .take(4)
            .all(|text| text.contains("Plan mode"))
    );
    assert!(!fixture.store.events_after("s1", 0).unwrap().is_empty());
}

#[tokio::test]
async fn submit_plan_outside_plan_mode_is_explained_and_always_advertised() {
    let model = ScriptModel::new(vec![
        calls(&[("submit_plan", json!({"plan":"# Plan\n\nDo the work."}))]),
        done(),
    ]);
    let fixture = Fixture::new(model.clone()).await;
    let mut events = fixture.start().await;
    let seen = until(&mut events, |p| {
        matches!(p, EventPayload::TurnCompleted { .. })
    })
    .await;
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e.payload, EventPayload::RequestOpened { .. }))
    );
    let requests = model.requests.lock().unwrap();
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == "submit_plan"),
        "submit_plan is always in the tool list"
    );
    assert!(requests[1].messages.iter().any(|message| matches!(message, canter_model::Message::ToolResult { text, .. } if text.contains("not in Plan mode"))));
}

#[tokio::test]
async fn plan_approval_choices_select_permissions_and_continue_or_finish_the_turn() {
    for (decision, feedback, expected_mode, requests, prior, selected) in [
        ("implement", None, false, 2, "allowAll", "askForExecution"),
        (
            "clearContext",
            None,
            false,
            1,
            "askForExecution",
            "allowAll",
        ),
        (
            "keepPlanning",
            Some("Add a regression test."),
            true,
            2,
            "alwaysAsk",
            "alwaysAsk",
        ),
    ] {
        let model = ScriptModel::new(vec![
            calls(&[(
                "submit_plan",
                json!({"plan":"# Change\n\n1. Implement.\n2. Verify."}),
            )]),
            done(),
        ]);
        let fixture = Fixture::new(model.clone()).await;
        fixture.update(json!({"approvalPolicy":prior})).await;
        fixture.update(json!({"planMode":true})).await;
        let mut events = fixture.start().await;
        let seen = until(&mut events, |p| {
            matches!(
                p,
                EventPayload::RequestOpened { .. } | EventPayload::TurnCompleted { .. }
            )
        })
        .await;
        let request = seen
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RequestOpened { request } => Some(request),
                _ => None,
            })
            .expect("submit_plan must request approval in Plan mode");
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["kind"]["type"], "planApproval");
        assert_eq!(
            value["kind"]["approvalPolicy"],
            if prior == "allowAll" {
                "alwaysAsk"
            } else {
                prior
            },
            "default uses the prior policy except allow-all"
        );
        fixture.session.resolve(&request.id, serde_json::from_value(json!({
            "plan":{"decision":decision,"approvalPolicy":selected,"feedback":feedback},"clientName":"test"
        })).unwrap()).await.unwrap();
        until(&mut events, |p| {
            matches!(p, EventPayload::TurnCompleted { .. })
        })
        .await;
        let snapshot = fixture.session.snapshot().await.session;
        assert_eq!(snapshot.settings.plan_mode, expected_mode);
        if !expected_mode {
            assert_eq!(
                serde_json::to_value(snapshot.settings.approval_policy).unwrap(),
                selected
            );
        }
        let proposal = snapshot.turns[0]
            .items
            .iter()
            .find_map(|item| match item {
                Item::PlanProposal { answer, .. } => answer.as_ref(),
                _ => None,
            })
            .unwrap();
        assert_eq!(
            serde_json::to_value(proposal).unwrap()["decision"],
            decision
        );
        assert_eq!(model.requests.lock().unwrap().len(), requests);
        let history = fixture.store.model_history::<Value>("s1").unwrap();
        let result = history
            .iter()
            .filter_map(|entry| entry["message"]["text"].as_str())
            .next_back()
            .unwrap();
        if decision == "implement" {
            assert!(result.contains("update_plan"));
        }
        if let Some(feedback) = feedback {
            assert!(result.contains(feedback));
        }
    }
}

#[tokio::test]
async fn entering_plan_mode_while_a_tool_waits_prevents_stale_approval_execution() {
    let model = ScriptModel::new(vec![
        calls(&[("write", json!({"path":"changed","content":"bad"}))]),
        done(),
    ]);
    let fixture = Fixture::new(model).await;
    fixture.update(json!({"approvalPolicy":"alwaysAsk"})).await;
    let mut events = fixture.start().await;
    let seen = until(&mut events, |p| {
        matches!(p, EventPayload::RequestOpened { .. })
    })
    .await;
    let request = seen
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RequestOpened { request } => Some(request),
            _ => None,
        })
        .unwrap();
    fixture.update(json!({"planMode":true})).await;
    fixture
        .session
        .resolve(
            &request.id,
            serde_json::from_value(json!({"decision":"accept","clientName":"test"})).unwrap(),
        )
        .await
        .unwrap();
    until(&mut events, |p| {
        matches!(p, EventPayload::TurnCompleted { .. })
    })
    .await;
    assert!(
        !fixture.root.path().join("project/changed").exists(),
        "approval cannot override the current Plan mode restriction"
    );
}

#[tokio::test]
async fn mode_changes_append_hidden_history_restore_permissions_and_survive_resume() {
    for policy in ["alwaysAsk", "askForExecution", "allowAll"] {
        let model = ScriptModel::new(vec![done(), done()]);
        let fixture = Fixture::new(model.clone()).await;
        let mut events = fixture.start().await;
        until(&mut events, |p| {
            matches!(p, EventPayload::TurnCompleted { .. })
        })
        .await;
        fixture.update(json!({"approvalPolicy":policy})).await;
        let before = fixture.store.model_history::<Value>("s1").unwrap();
        fixture.update(json!({"planMode":true})).await;
        let entered = fixture.store.model_history::<Value>("s1").unwrap();
        assert_eq!(&entered[..before.len()], before);
        assert_eq!(entered.len(), before.len() + 1);
        assert!(
            entered.last().unwrap()["message"]["text"]
                .as_str()
                .unwrap()
                .contains("[Plan mode enabled]")
        );
        fixture.session.close().await.unwrap();
        let runtime = RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    fixture
                        .root
                        .path()
                        .join("home")
                        .to_string_lossy()
                        .into_owned(),
                ),
                (
                    "CANTER_HOME".into(),
                    fixture
                        .root
                        .path()
                        .join("state")
                        .to_string_lossy()
                        .into_owned(),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let (resumed, response) = Session::open(
            fixture.store.clone(),
            "s1".into(),
            runtime,
            model.clone(),
            CoreOptions::default(),
        )
        .await
        .unwrap();
        assert!(response.session.settings.plan_mode);
        assert_eq!(
            serde_json::to_value(response.session.settings.pre_plan_approval_policy).unwrap(),
            policy
        );
        let before_exit = fixture.store.model_history::<Value>("s1").unwrap();
        resumed
            .update(serde_json::from_value(json!({"planMode":false})).unwrap())
            .await
            .unwrap();
        let exited = fixture.store.model_history::<Value>("s1").unwrap();
        assert_eq!(&exited[..before_exit.len()], before_exit);
        assert_eq!(exited.len(), before_exit.len() + 1);
        assert!(
            exited.last().unwrap()["message"]["text"]
                .as_str()
                .unwrap()
                .contains("[Plan mode disabled]")
        );
        assert_eq!(
            serde_json::to_value(resumed.snapshot().await.session.settings.approval_policy)
                .unwrap(),
            policy
        );
        assert!(
            !serde_json::to_string(&fixture.store.events_after("s1", 0).unwrap())
                .unwrap()
                .contains("[Plan mode")
        );
        resumed.close().await.unwrap();
    }
}

#[tokio::test]
async fn leaving_plan_mode_invalidates_an_open_proposal_and_applies_to_the_next_tool() {
    for explicit_policy in [None, Some("alwaysAsk")] {
        let model = ScriptModel::new(vec![
            calls(&[
                ("submit_plan", json!({"plan":"Do the work."})),
                ("write", json!({"path":"after-exit","content":"done"})),
            ]),
            done(),
        ]);
        let fixture = Fixture::new(model).await;
        fixture.update(json!({"planMode":true})).await;
        let mut events = fixture.start().await;
        let seen = until(&mut events, |p| {
            matches!(p, EventPayload::RequestOpened { .. })
        })
        .await;
        let request = seen
            .iter()
            .find_map(|e| match &e.payload {
                EventPayload::RequestOpened { request } => Some(request),
                _ => None,
            })
            .unwrap();
        fixture
            .update(json!({"planMode":false,"approvalPolicy":explicit_policy}))
            .await;
        let late = fixture.session.resolve(&request.id, serde_json::from_value(json!({"plan":{"decision":"implement","approvalPolicy":"allowAll"},"clientName":"late"})).unwrap()).await.unwrap();
        assert!(late.already_resolved);
        if explicit_policy.is_some() {
            let seen = until(&mut events, |p| {
                matches!(p, EventPayload::RequestOpened { .. })
            })
            .await;
            let request = seen
                .iter()
                .find_map(|e| match &e.payload {
                    EventPayload::RequestOpened { request } => Some(request),
                    _ => None,
                })
                .unwrap();
            assert!(matches!(request.kind, RequestKind::Approval(_)));
            fixture
                .session
                .resolve(
                    &request.id,
                    serde_json::from_value(json!({"decision":"accept","clientName":"test"}))
                        .unwrap(),
                )
                .await
                .unwrap();
        }
        until(&mut events, |p| {
            matches!(p, EventPayload::TurnCompleted { .. })
        })
        .await;
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("project/after-exit")).unwrap(),
            "done"
        );
    }
}

#[tokio::test]
async fn plan_mode_blocks_shell_compositions_and_allows_questions_in_a_batch() {
    let model = ScriptModel::new(vec![
        calls(&[
            ("shell", json!({"command":"pwd; touch changed"})),
            ("shell", json!({"command":"pwd && touch changed"})),
            ("shell", json!({"command":"pwd | tee changed"})),
            ("shell", json!({"command":"pwd > changed"})),
            (
                "ask_user",
                json!({"question":"Which approach?","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]}),
            ),
            ("read", json!({"path":"fixture"})),
        ]),
        done(),
    ]);
    let fixture = Fixture::new(model.clone()).await;
    std::fs::write(fixture.root.path().join("project/fixture"), "readable").unwrap();
    fixture.update(json!({"planMode":true})).await;
    let mut events = fixture.start().await;
    let seen = until(&mut events, |p| {
        matches!(p, EventPayload::RequestOpened { .. })
    })
    .await;
    let request = seen
        .iter()
        .find_map(|e| match &e.payload {
            EventPayload::RequestOpened { request } => Some(request),
            _ => None,
        })
        .unwrap();
    assert!(matches!(request.kind, RequestKind::UserInput(_)));
    assert_eq!(
        seen.iter()
            .filter(|e| matches!(
                e.payload,
                EventPayload::ItemCompleted {
                    item: Item::CommandExecution {
                        status: ItemStatus::Blocked,
                        ..
                    },
                    ..
                }
            ))
            .count(),
        4
    );
    fixture
        .session
        .resolve(
            &request.id,
            serde_json::from_value(
                json!({"answer":{"type":"option","label":"A"},"clientName":"test"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    until(&mut events, |p| {
        matches!(p, EventPayload::TurnCompleted { .. })
    })
    .await;
    assert!(!fixture.root.path().join("project/changed").exists());
    assert!(
        fixture.session.snapshot().await.session.turns[0]
            .items
            .iter()
            .any(|item| matches!(
                item,
                Item::FileRead {
                    status: ItemStatus::Completed,
                    ..
                }
            ))
    );
}
