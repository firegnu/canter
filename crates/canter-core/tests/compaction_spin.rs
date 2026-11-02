#[path = "../../canter-model/tests/support/mod.rs"]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{ApiKey, ChatCompletions};
use canter_protocol::*;
use canter_store::Store;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn reply(delta: Value, reason: &str, input_tokens: u64) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}]}),
        json!({"choices":[],"usage":{"prompt_tokens":input_tokens,"completion_tokens":1,"total_tokens":input_tokens+1}}),
    ))
}

fn answer(text: &str) -> Reply {
    reply(json!({"content":text}), "stop", 100)
}

fn tool(name: &str, args: Value, tokens: u64) -> Reply {
    reply(
        json!({"tool_calls":[{"index":0,"id":format!("call-{tokens}"),"function":{"name":name,"arguments":args.to_string()}}]}),
        "tool_calls",
        tokens,
    )
}

fn read(tokens: u64) -> Reply {
    tool("read", json!({"path":"note.txt"}), tokens)
}

fn question() -> Reply {
    tool(
        "ask_user",
        json!({"question":"Continue?","options":[{"label":"Yes","description":"Continue work"},{"label":"No","description":"Stop work"}]}),
        100,
    )
}

struct Fixture {
    root: tempfile::TempDir,
    store: Arc<Store>,
    session: Session,
    server: Server,
    options: CoreOptions,
    runtime: RuntimeContext,
}

impl Fixture {
    async fn new(replies: Vec<Reply>, limit: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("note.txt"), "File facts.\n").unwrap();
        let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
        let server = Server::start(replies).await;
        let runtime = RuntimeContext {
            environment: [
                (
                    "HOME".into(),
                    root.path().join("home").display().to_string(),
                ),
                (
                    "CANTER_HOME".into(),
                    root.path().join("state").display().to_string(),
                ),
            ]
            .into(),
            ..Default::default()
        };
        let options = CoreOptions {
            auto_compact_tokens: Some(limit),
            ..Default::default()
        };
        let settings = serde_json::from_value(json!({"cwd":root.path(),"name":null,"model":"fake","reasoningEffort":"high","approvalPolicy":"allowAll"})).unwrap();
        drop(store.create("spin", settings, 1).unwrap());
        let (session, _) = Session::open(
            store.clone(),
            "spin".into(),
            runtime.clone(),
            Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap()),
            options.clone(),
        )
        .await
        .unwrap();
        Self {
            root,
            store,
            session,
            server,
            options,
            runtime,
        }
    }

    async fn events(&self) -> Subscription {
        self.session
            .subscribe(self.session.snapshot().await.event_seq)
            .await
            .unwrap()
    }

    async fn send(&self, text: &str) -> Turn {
        let mut events = self.events().await;
        self.session
            .input(SessionInputParams {
                input: vec![UserInput::Text { text: text.into() }],
                mode: InputMode::Queue,
                client_name: "test".into(),
                non_interactive: false,
                approval_policy: None,
                review: None,
            })
            .await
            .unwrap();
        finished(&mut events).await
    }

    async fn close(self) {
        self.session.close().await.unwrap();
        self.server.stop().await;
    }

    async fn compact(&self) -> Turn {
        let mut events = self.events().await;
        self.session.compact().await.unwrap();
        finished(&mut events).await
    }

    async fn reopen(&mut self) {
        self.session.close().await.unwrap();
        self.session = Session::open(
            self.store.clone(),
            "spin".into(),
            self.runtime.clone(),
            Arc::new(ChatCompletions::new(&self.server.base_url, ApiKey::new(KEY)).unwrap()),
            self.options.clone(),
        )
        .await
        .unwrap()
        .0;
    }
}

async fn finished(events: &mut Subscription) -> Turn {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let EventPayload::TurnCompleted { turn } =
                events.recv().await.unwrap().unwrap().payload
            {
                return turn;
            }
        }
    })
    .await
    .unwrap()
}

async fn pending_question(events: &mut Subscription) -> PendingRequest {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match events.recv().await.unwrap().unwrap().payload {
                EventPayload::RequestOpened { request } => return request,
                EventPayload::TurnCompleted { turn } => {
                    panic!("turn ended before question: {:?}", turn.status)
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}

fn notes(request: &Value) -> Vec<String> {
    request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_str())
        .filter(|text| text.starts_with("[Context budget]"))
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn budget_notices_follow_both_thresholds_once_at_each_limit() {
    for limit in [40_000, 60_000] {
        let fixture = Fixture::new(
            vec![
                read(limit * 70 / 100),
                read(limit * 76 / 100),
                read(limit * 80 / 100),
                read(limit * 91 / 100),
                read(limit * 95 / 100),
                answer("Done."),
            ],
            limit,
        )
        .await;
        assert_eq!(
            fixture.send("Read the file.").await.status,
            TurnStatus::Completed
        );
        let requests = fixture.server.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 6);
        assert_eq!(
            requests.iter().map(|r| notes(r).len()).collect::<Vec<_>>(),
            [0, 0, 1, 1, 2, 2]
        );
        for (index, percent) in [(2, 76), (4, 91)] {
            let note = notes(&requests[index]).pop().unwrap();
            let used = note
                .strip_prefix("[Context budget] ")
                .and_then(|rest| rest.split(' ').next())
                .and_then(|word| word.parse::<u64>().ok())
                .unwrap_or_else(|| panic!("{note}"));
            assert!(
                (limit * percent / 100..limit * percent / 100 + 500).contains(&used),
                "{note}"
            );
            assert_eq!(
                note,
                format!(
                    "[Context budget] {used} of {limit} tokens used before automatic compaction; about {} left. Compaction will then free space; this is not a limit on the task.",
                    limit - used
                )
            );
            assert_eq!(
                requests[index]["messages"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()["content"],
                note
            );
        }
        fixture.close().await;
    }
}

const STOP_REASON: &str = "Stopped: context was compacted 3 times in a row with no file edits or completed plan steps. Send a message to continue, or narrow the task.";

#[tokio::test]
async fn three_read_only_compactions_stop_the_turn_and_pause_a_running_goal() {
    for goal in [false, true] {
        let fixture = Fixture::new(
            vec![
                read(41_000),
                answer("Summary 1."),
                read(41_000),
                answer("Summary 2."),
                read(41_000),
                answer("Summary 3."),
                answer("Continued."),
            ],
            40_000,
        )
        .await;
        let turn = if goal {
            let mut events = fixture.events().await;
            fixture
                .session
                .set_goal(GoalSetParams {
                    objective: "Inspect the file.".into(),
                    budget_usd: Some(0.0),
                })
                .await
                .unwrap();
            finished(&mut events).await
        } else {
            fixture.send("Inspect the file.").await
        };
        assert_eq!(turn.status, TurnStatus::Interrupted);
        assert_eq!(fixture.server.requests.lock().unwrap().len(), 6);
        assert_eq!(fixture.server.arrivals.lock().unwrap().len(), 6);
        let events = fixture.store.events_after("spin", 0).unwrap();
        assert!(events.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message == STOP_REASON)));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    EventPayload::TelemetryCompaction {
                        succeeded: true,
                        trigger: CompactionTrigger::Auto,
                        ..
                    }
                ))
                .count(),
            3
        );
        if goal {
            assert_eq!(
                fixture.session.goal().await.goal.unwrap().status,
                GoalStatus::Paused
            );
        }
        let next = fixture.send("Continue with a smaller task.").await;
        assert_eq!(next.status, TurnStatus::Completed);
        assert!(
            fixture
                .session
                .snapshot()
                .await
                .session
                .turns
                .last()
                .unwrap()
                .items
                .iter()
                .any(
                    |item| matches!(item, Item::AgentMessage { text, .. } if text == "Continued.")
                )
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn successful_file_changes_and_newly_completed_steps_reset_the_count() {
    for (name, args) in [
        (
            "edit",
            json!({"path":"note.txt","edits":[{"oldText":"File facts.","newText":"Updated facts."}]}),
        ),
        (
            "write",
            json!({"path":"note.txt","content":"Updated facts.\n"}),
        ),
        (
            "update_plan",
            json!({"plan":[{"step":"Inspect","status":"completed"}]}),
        ),
    ] {
        let fixture = Fixture::new(
            vec![
                tool(
                    "update_plan",
                    json!({"plan":[{"step":"Inspect","status":"in_progress"}]}),
                    100,
                ),
                read(41_000),
                answer("Summary 1."),
                read(41_000),
                answer("Summary 2."),
                tool(name, args, 41_000),
                answer("Summary 3."),
                read(41_000),
                answer("Summary 4."),
                answer("Done."),
            ],
            40_000,
        )
        .await;
        let turn = fixture.send("Make progress between compactions.").await;
        let snapshot = fixture.session.snapshot().await;
        assert!(
            snapshot
                .session
                .turns
                .last()
                .unwrap()
                .items
                .iter()
                .any(|item| match item {
                    Item::FileChange {
                        status: ItemStatus::Completed,
                        ..
                    } => name != "update_plan",
                    Item::Plan {
                        plan,
                        status: ItemStatus::Completed,
                        ..
                    } =>
                        name == "update_plan"
                            && plan
                                .iter()
                                .any(|step| step.status == PlanStepStatus::Completed),
                    _ => false,
                }),
            "progress tool must succeed: {name}"
        );
        assert_eq!(turn.status, TurnStatus::Completed, "{name}");
        assert_eq!(fixture.server.requests.lock().unwrap().len(), 10, "{name}");
        if name != "update_plan" {
            assert_eq!(
                std::fs::read_to_string(fixture.root.path().join("note.txt")).unwrap(),
                "Updated facts.\n"
            );
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn a_new_user_message_resets_the_compaction_count() {
    let fixture = Fixture::new(
        vec![
            read(41_000),
            answer("Summary 1."),
            read(41_000),
            answer("Summary 2."),
            answer("Waiting."),
            read(41_000),
            answer("Summary 3."),
            answer("Done."),
        ],
        40_000,
    )
    .await;
    assert_eq!(fixture.send("Inspect.").await.status, TurnStatus::Completed);
    assert_eq!(
        fixture.send("Now narrow the task.").await.status,
        TurnStatus::Completed
    );
    assert_eq!(fixture.server.requests.lock().unwrap().len(), 8);
    fixture.close().await;
}

#[tokio::test]
async fn shell_failed_edits_and_unchanged_files_or_completed_steps_are_not_progress() {
    for (name, args) in [
        ("shell", json!({"command":"printf changed > shell.txt"})),
        (
            "edit",
            json!({"path":"note.txt","edits":[{"oldText":"Missing text","newText":"Updated"}]}),
        ),
        (
            "write",
            json!({"path":"note.txt","content":"File facts.\n"}),
        ),
        (
            "update_plan",
            json!({"plan":[{"step":"Inspect","status":"completed"}]}),
        ),
    ] {
        let fixture = Fixture::new(
            vec![
                tool(
                    "update_plan",
                    json!({"plan":[{"step":"Inspect","status":"completed"}]}),
                    100,
                ),
                read(41_000),
                answer("Summary 1."),
                read(41_000),
                answer("Summary 2."),
                tool(name, args, 41_000),
                answer("Summary 3."),
                answer("Must not be requested."),
            ],
            40_000,
        )
        .await;
        assert_eq!(
            fixture.send("Inspect.").await.status,
            TurnStatus::Interrupted,
            "{name}"
        );
        assert_eq!(fixture.server.requests.lock().unwrap().len(), 7, "{name}");
        if name == "shell" {
            assert_eq!(
                std::fs::read_to_string(fixture.root.path().join("shell.txt")).unwrap(),
                "changed"
            );
        }
        fixture.close().await;
    }
}

#[tokio::test]
async fn review_and_plan_mode_turns_can_compact_three_times_without_stopping() {
    for plan_mode in [false, true] {
        let mut replies = Vec::new();
        for _ in 0..3 {
            replies.extend([read(37_000), read(41_000), answer("Summary.")]);
        }
        replies.push(answer("Done."));
        let fixture = Fixture::new(replies, 40_000).await;
        fixture
            .session
            .update(SessionUpdateParams {
                plan_mode: Some(plan_mode),
                ..Default::default()
            })
            .await
            .unwrap();
        let mut events = fixture.events().await;
        fixture
            .session
            .input(SessionInputParams {
                input: vec![UserInput::Text {
                    text: "Inspect.".into(),
                }],
                mode: InputMode::Queue,
                client_name: "test".into(),
                non_interactive: false,
                approval_policy: None,
                review: (!plan_mode).then(|| "Review files".into()),
            })
            .await
            .unwrap();
        assert_eq!(
            finished(&mut events).await.status,
            TurnStatus::Completed,
            "plan mode: {plan_mode}"
        );
        let requests = fixture.server.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 10);
        // Plan mode keeps its existing notices; review turns still omit them.
        assert_eq!(
            requests.iter().any(|request| !notes(request).is_empty()),
            plan_mode
        );
        let events = fixture.store.events_after("spin", 0).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event.payload,
                    EventPayload::TelemetryCompaction {
                        succeeded: true,
                        trigger: CompactionTrigger::Auto,
                        ..
                    }
                ))
                .count(),
            3
        );
        assert!(!events.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message == STOP_REASON)));
        fixture.close().await;
    }
}

#[tokio::test]
async fn plan_mode_compactions_preserve_the_earlier_modifying_turn_count() {
    let fixture = Fixture::new(
        vec![
            read(41_000),
            answer("Modification summary 1."),
            question(),
            read(41_000),
            answer("Plan summary 1."),
            read(41_000),
            answer("Plan summary 2."),
            read(41_000),
            answer("Plan summary 3."),
            question(),
            read(41_000),
            answer("Modification summary 2."),
            read(41_000),
            answer("Modification summary 3."),
            answer("Must not be requested."),
        ],
        40_000,
    )
    .await;
    let mut events = fixture.events().await;
    fixture
        .session
        .input(
            serde_json::from_value(
                json!({"input":[{"type":"text","text":"Inspect."}],"clientName":"test"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    for plan_mode in [true, false] {
        let request = pending_question(&mut events).await;
        fixture
            .session
            .update(SessionUpdateParams {
                plan_mode: Some(plan_mode),
                ..Default::default()
            })
            .await
            .unwrap();
        fixture
            .session
            .resolve(
                &request.id,
                serde_json::from_value(
                    json!({"answer":{"type":"option","label":"Yes"},"clientName":"test"}),
                )
                .unwrap(),
            )
            .await
            .unwrap();
    }
    assert_eq!(finished(&mut events).await.status, TurnStatus::Interrupted);
    assert_eq!(fixture.server.requests.lock().unwrap().len(), 14);
    fixture.close().await;
}

#[tokio::test]
async fn new_goal_starts_a_fresh_count_that_persists_across_automatic_turns() {
    let fixture = Fixture::new(
        vec![
            read(41_000),
            answer("Summary 1."),
            answer("First turn."),
            answer("Manual summary."),
            read(41_000),
            answer("Summary 2."),
            answer("Next goal turn."),
            read(41_000),
            answer("Summary 3."),
            read(41_000),
            answer("Summary 4."),
            answer("Must not be requested."),
        ],
        40_000,
    )
    .await;
    assert_eq!(fixture.send("Inspect.").await.status, TurnStatus::Completed);
    assert_eq!(fixture.compact().await.status, TurnStatus::Completed);
    let mut events = fixture.events().await;
    fixture
        .session
        .set_goal(GoalSetParams {
            objective: "Continue inspection.".into(),
            budget_usd: Some(0.0),
        })
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(finished(&mut events).await.status, TurnStatus::Interrupted);
    assert_eq!(fixture.server.requests.lock().unwrap().len(), 11);
    assert_eq!(
        fixture.session.goal().await.goal.unwrap().status,
        GoalStatus::Paused
    );
    fixture.close().await;
}

async fn goal_instruction_starts_a_fresh_compaction_count(resume: bool) {
    let fixture = Fixture::new(
        vec![
            read(41_000),
            answer("Old summary 1."),
            read(41_000),
            answer("Old summary 2."),
            if resume {
                question()
            } else {
                answer("Initial round.")
            },
            read(41_000),
            answer("New summary 1."),
            answer("First goal turn."),
            read(41_000),
            answer("New summary 2."),
            answer("Second goal turn."),
            read(41_000),
            answer("New summary 3."),
            answer("Must not be requested."),
        ],
        40_000,
    )
    .await;
    let goal = || GoalSetParams {
        objective: "Inspect the file.".into(),
        budget_usd: Some(0.0),
    };
    if resume {
        let mut events = fixture.events().await;
        fixture.session.set_goal(goal()).await.unwrap();
        pending_question(&mut events).await;
        fixture.session.control_goal("pause").await.unwrap();
        assert_eq!(
            fixture.session.goal().await.goal.unwrap().status,
            GoalStatus::Paused
        );
    } else {
        assert_eq!(fixture.send("Inspect.").await.status, TurnStatus::Completed);
    }
    assert_eq!(fixture.server.requests.lock().unwrap().len(), 5);
    let mut events = fixture.events().await;
    if resume {
        fixture.session.control_goal("resume").await.unwrap();
    } else {
        fixture.session.set_goal(goal()).await.unwrap();
    }
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(finished(&mut events).await.status, TurnStatus::Interrupted);
    assert_eq!(fixture.server.requests.lock().unwrap().len(), 13);
    assert_eq!(
        fixture.session.goal().await.goal.unwrap().status,
        GoalStatus::Paused
    );
    fixture.close().await;
}

#[tokio::test]
async fn setting_a_new_goal_resets_the_compaction_count() {
    goal_instruction_starts_a_fresh_compaction_count(false).await;
}

#[tokio::test]
async fn resuming_a_paused_goal_resets_the_compaction_count() {
    goal_instruction_starts_a_fresh_compaction_count(true).await;
}

#[tokio::test]
async fn jumping_past_both_thresholds_only_warns_once_and_compaction_rearms_it() {
    let fixture = Fixture::new(
        vec![
            read(37_000),
            read(38_000),
            read(41_000),
            answer("Summary."),
            read(37_000),
            answer("Done."),
        ],
        40_000,
    )
    .await;
    assert_eq!(
        fixture.send("Exact user words.").await.status,
        TurnStatus::Completed
    );
    let requests = fixture.server.requests.lock().unwrap().clone();
    assert_eq!(
        requests
            .iter()
            .map(|request| notes(request).len())
            .collect::<Vec<_>>(),
        [0, 1, 1, 1, 0, 1]
    );
    assert!(requests[3]["messages"].as_array().unwrap().last().unwrap()["content"].as_str().unwrap().contains("Context budget notices ([Context budget]) are internal context, not the user's own words"));
    let history = fixture.store.model_history::<Value>("spin").unwrap();
    let compact = history
        .iter()
        .find(|entry| entry["type"] == "compaction")
        .unwrap();
    assert_eq!(compact["user_messages"], json!(["Exact user words."]));
    assert!(!compact["messages"].to_string().contains("[Context budget]"));
    fixture.close().await;
}

#[tokio::test]
async fn rewind_and_reopen_preserve_request_bytes_and_notice_thresholds() {
    let mut fixture = Fixture::new(
        vec![
            read(31_000),
            answer("First."),
            read(37_000),
            answer("Second."),
            read(37_000),
            answer("Second."),
        ],
        40_000,
    )
    .await;
    fixture.send("Inspect.").await;
    let turn = fixture.send("Continue.").await;
    let mut params: SessionRewindParams = serde_json::from_value(json!({
        "beforeTurnId":turn.id,"rewindFiles":false,"rewindConversation":true
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
    fixture.reopen().await;
    assert_eq!(
        fixture.send("Continue.").await.status,
        TurnStatus::Completed
    );
    let requests = fixture.server.requests.lock().unwrap().clone();
    for (before, after) in [(2, 4), (3, 5)] {
        assert_eq!(
            serde_json::to_vec(&requests[before]).unwrap(),
            serde_json::to_vec(&requests[after]).unwrap()
        );
    }
    assert_eq!(notes(&requests[4]).len(), 1);
    assert_eq!(notes(&requests[5]).len(), 2);
    fixture.close().await;
}

fn context_too_long() -> Reply {
    Reply {
        status: axum::http::StatusCode::BAD_REQUEST,
        body: json!({"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}).to_string(),
        retry_after: None, hang: false,
    }
}

#[tokio::test]
async fn provider_context_errors_count_as_automatic_compactions() {
    let fixture = Fixture::new(
        vec![
            context_too_long(),
            answer("Summary 1."),
            read(100),
            context_too_long(),
            answer("Summary 2."),
            read(100),
            context_too_long(),
            answer("Summary 3."),
            answer("Must not be requested."),
        ],
        40_000,
    )
    .await;
    assert_eq!(
        fixture.send("Inspect.").await.status,
        TurnStatus::Interrupted
    );
    assert_eq!(fixture.server.requests.lock().unwrap().len(), 8);
    fixture.close().await;
}

#[tokio::test]
async fn steering_resets_the_count_before_the_next_compaction() {
    let fixture = Fixture::new(vec![
        read(41_000), answer("Summary 1."),
        read(41_000), answer("Summary 2."),
        tool("ask_user", json!({"question":"Which file?","options":[{"label":"A","description":"First file"},{"label":"B","description":"Second file"}]}), 41_000),
        answer("Summary 3."), answer("Done."),
    ], 40_000).await;
    let mut events = fixture.events().await;
    fixture
        .session
        .input(
            serde_json::from_value(
                json!({"input":[{"type":"text","text":"Inspect."}],"clientName":"test"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let request = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let EventPayload::RequestOpened { request } =
                events.recv().await.unwrap().unwrap().payload
            {
                return request;
            }
        }
    })
    .await
    .unwrap();
    fixture.session.input(serde_json::from_value(json!({"input":[{"type":"text","text":"Focus on file A."}],"mode":"steer","clientName":"test"})).unwrap()).await.unwrap();
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
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(fixture.server.requests.lock().unwrap().len(), 7);
    fixture.close().await;
}

#[tokio::test]
async fn private_answers_reviews_and_manual_compaction_do_not_add_budget_notices() {
    let fixture = Fixture::new(
        vec![
            reply(json!({"content":"First."}), "stop", 37_000),
            answer("Side answer."),
            read(37_000),
            answer("Review."),
            answer("Manual summary."),
        ],
        40_000,
    )
    .await;
    fixture.send("Inspect.").await;
    let mut btw = fixture.session.btw("Side question.".into()).await.unwrap();
    while btw.recv().await.is_some() {}
    let mut events = fixture.events().await;
    fixture.session.input(serde_json::from_value(json!({"input":[{"type":"text","text":"Review."}],"review":"Review files","clientName":"test"})).unwrap()).await.unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(fixture.compact().await.status, TurnStatus::Completed);
    let requests = fixture.server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 5);
    assert!(requests.iter().all(|request| notes(request).is_empty()));
    fixture.close().await;
}

#[tokio::test]
async fn transcript_fallback_attributes_budget_notices_to_internal_context() {
    let fixture = Fixture::new(
        vec![
            read(37_000),
            answer("Done."),
            context_too_long(),
            answer("Summary."),
        ],
        40_000,
    )
    .await;
    fixture.send("My words.").await;
    assert_eq!(fixture.compact().await.status, TurnStatus::Completed);
    let requests = fixture.server.requests.lock().unwrap().clone();
    let prompt = requests[3]["messages"].as_array().unwrap().last().unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(prompt.contains("\nInternal context budget: [Context budget]"));
    assert!(!prompt.contains("\nUser: [Context budget]"));
    fixture.close().await;
}

#[tokio::test]
async fn forks_keep_budget_notes_without_repeating_the_same_threshold() {
    let fixture = Fixture::new(
        vec![
            read(37_000),
            answer("First."),
            read(38_000),
            answer("Fork answer."),
        ],
        40_000,
    )
    .await;
    fixture.send("Inspect.").await;
    let snapshot = fixture.session.fork_snapshot().await.unwrap();
    let (fork, _) = snapshot
        .create(
            fixture.store.clone(),
            "fork".into(),
            fixture.runtime.clone(),
            Arc::new(ChatCompletions::new(&fixture.server.base_url, ApiKey::new(KEY)).unwrap()),
            fixture.options.clone(),
        )
        .await
        .unwrap();
    let mut events = fork
        .subscribe(fork.snapshot().await.event_seq)
        .await
        .unwrap();
    fork.input(
        serde_json::from_value(
            json!({"input":[{"type":"text","text":"Continue."}],"clientName":"test"}),
        )
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    let requests = fixture.server.requests.lock().unwrap().clone();
    assert_eq!(notes(&requests[1]), notes(&requests[2]));
    assert_eq!(notes(&requests[1]), notes(&requests[3]));
    fork.close().await.unwrap();
    fixture.close().await;
}
