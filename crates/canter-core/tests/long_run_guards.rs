#[path = "../../canter-model/tests/support/mod.rs"]
#[allow(dead_code)]
mod support;

use canter_core::{CoreOptions, RuntimeContext, Session, Subscription};
use canter_model::{
    ApiKey, ChatCompletions, Model, ModelContext, ModelError, Request, Responses, ResponsesAuth,
    ResponsesCredential,
};
use canter_protocol::*;
use canter_store::Store;
use futures_util::future::BoxFuture;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use support::{KEY, Reply, Server};

fn reply(delta: Value, reason: &str) -> Reply {
    Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":delta,"finish_reason":reason}]})
    ))
}

fn answer(text: &str) -> Reply {
    reply(json!({"content":text}), "stop")
}

fn input(text: &str) -> SessionInputParams {
    serde_json::from_value(json!({"input":[{"type":"text","text":text}],"clientName":"test"}))
        .unwrap()
}

struct Fixture {
    root: tempfile::TempDir,
    store: Arc<Store>,
    session: Session,
    server: Server,
}

impl Fixture {
    async fn new(replies: Vec<Reply>) -> Self {
        Self::with_api(replies, false).await
    }

    async fn with_api(replies: Vec<Reply>, responses: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let server = Server::start(replies).await;
        let store = Arc::new(Store::new(root.path().join("sessions")).unwrap());
        let model: Arc<dyn Model> = if responses {
            Arc::new(
                Responses::for_test(
                    Arc::new(Auth),
                    format!("{}responses", server.base_url).parse().unwrap(),
                )
                .unwrap(),
            )
        } else {
            Arc::new(ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap())
        };
        let (session, _) = Session::create(
            store.clone(),
            "guards".into(),
            serde_json::from_value(
                json!({"cwd":root.path(),"model":"fake","approvalPolicy":"allowAll"}),
            )
            .unwrap(),
            RuntimeContext {
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
            },
            model,
            CoreOptions {
                prompt_suggestions: false,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        Self {
            root,
            store,
            session,
            server,
        }
    }

    async fn send(&self, text: &str) -> Turn {
        let mut events = self
            .session
            .subscribe(self.session.snapshot().await.event_seq)
            .await
            .unwrap();
        self.session.input(input(text)).await.unwrap();
        finished(&mut events).await
    }

    fn requests(&self) -> Vec<Value> {
        self.server.requests.lock().unwrap().clone()
    }

    async fn close(self) {
        self.session.close().await.unwrap();
        self.server.stop().await;
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

fn assert_prefix(before: &Value, after: &Value) {
    let before = before["messages"].as_array().unwrap();
    let after = after["messages"].as_array().unwrap();
    assert_eq!(
        serde_json::to_vec(before).unwrap(),
        serde_json::to_vec(&after[..before.len()]).unwrap()
    );
}

#[tokio::test]
async fn truncated_text_continues_without_changing_the_request_prefix() {
    let fixture = Fixture::new(vec![
        reply(json!({"content":"Half a"}), "length"),
        answer(" sentence."),
    ])
    .await;
    let turn = fixture.send("Explain.").await;
    assert_eq!(turn.status, TurnStatus::Completed);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    assert_prefix(&requests[0], &requests[1]);
    let messages = requests[1]["messages"].as_array().unwrap();
    assert_eq!(messages[messages.len() - 2]["content"], "Half a");
    assert!(
        messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("Continue from where")
    );
    fixture.close().await;
}

fn tool(id: usize, name: &str, arguments: &str, reason: &str) -> Reply {
    reply(
        json!({"tool_calls":[{"index":0,"id":format!("call-{id}"),"function":{"name":name,"arguments":arguments}}]}),
        reason,
    )
}

fn plan(id: usize) -> Reply {
    tool(
        id,
        "update_plan",
        r#"{"plan":[{"step":"Inspect","status":"in_progress"},{"step":"Verify","status":"pending"}]}"#,
        "tool_calls",
    )
}

#[tokio::test]
async fn unfinished_plan_gets_three_reminders_then_stops() {
    let fixture = Fixture::new(vec![
        plan(0),
        answer("Stopping."),
        answer("Blocked."),
        answer("Need input."),
        answer("Still blocked."),
    ])
    .await;
    assert_eq!(
        fixture.send("Do the task.").await.status,
        TurnStatus::Completed
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 5);
    for pair in requests.windows(2) {
        assert_prefix(&pair[0], &pair[1]);
    }
    for request in &requests[2..] {
        let text = request["messages"].as_array().unwrap().last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(text.contains("2 unfinished plan steps"), "{text}");
        assert!(
            text.contains("Inspect") && text.contains("Verify") && text.contains("update_plan")
        );
        assert!(text.contains("blocked") && text.contains("user decision"));
    }
    let events = fixture.store.events_after("guards", 0).unwrap();
    assert_eq!(events.iter().filter(|event| matches!(&event.payload, EventPayload::Warning { message } if message.contains("unfinished plan"))).count(), 3);
    fixture.close().await;
}

#[tokio::test]
async fn truncated_tool_calls_are_not_executed_and_share_the_three_continuation_limit() {
    let fixture = Fixture::new(vec![
        reply(json!({"content":"Start"}), "length"),
        tool(
            1,
            "write",
            r#"{"path":"must-not-exist","content":"partial"}"#,
            "length",
        ),
        tool(
            2,
            "write",
            r#"{"path":"must-not-exist","content":"#,
            "length",
        ),
        reply(json!({"content":"Still too long"}), "length"),
    ])
    .await;
    assert_eq!(fixture.send("Write.").await.status, TurnStatus::Completed);
    assert!(!fixture.root.path().join("must-not-exist").exists());
    let requests = fixture.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests[2..] {
        let messages = request["messages"].as_array().unwrap();
        // This adapter has always withheld truncated calls from history.
        assert!(
            !messages
                .iter()
                .any(|message| message.get("tool_calls").is_some())
        );
        assert!(
            messages.last().unwrap()["content"]
                .as_str()
                .unwrap()
                .contains("Split them into smaller steps")
        );
    }
    let events = fixture.store.events_after("guards", 0).unwrap();
    assert!(events.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message == "The model reply was incomplete; its tool calls were not run.")));
    fixture.close().await;
}

fn read(id: usize) -> Reply {
    tool(id, "read", r#"{"path":"note.txt"}"#, "tool_calls")
}

#[tokio::test]
async fn identical_calls_warn_on_third_result_and_stop_before_fifth_execution() {
    let fixture = Fixture::new(
        (0..5)
            .map(read)
            .chain([answer("Should not be requested.")])
            .collect(),
    )
    .await;
    std::fs::write(fixture.root.path().join("note.txt"), "Facts.\n").unwrap();
    assert_eq!(
        fixture.send("Inspect.").await.status,
        TurnStatus::Interrupted
    );
    let requests = fixture.requests();
    assert_eq!(requests.len(), 5);
    let result = |index: usize| {
        requests[index]["messages"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["content"]
            .as_str()
            .unwrap()
    };
    assert!(!result(2).contains("[Repeated call]"));
    assert!(result(3).contains("[Repeated call]") && result(3).contains("3 consecutive times"));
    assert_eq!(result(1), result(4));
    for pair in requests.windows(2) {
        assert_prefix(&pair[0], &pair[1]);
    }
    let events = fixture.store.events_after("guards", 0).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                &event.payload,
                EventPayload::ItemCompleted {
                    item: Item::FileRead { .. },
                    ..
                }
            ))
            .count(),
        4
    );
    assert!(events.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message.contains("repeated tool call"))));
    fixture.close().await;
}

#[tokio::test]
async fn identical_calls_with_changing_results_do_not_stop() {
    let command =
        r#"{"command":"n=$(cat count); n=$((n + 1)); printf '%s' \"$n\" > count; cat count"}"#;
    let fixture = Fixture::new(
        (0..6)
            .map(|id| tool(id, "shell", command, "tool_calls"))
            .chain([answer("Done.")])
            .collect(),
    )
    .await;
    std::fs::write(fixture.root.path().join("count"), "0").unwrap();
    assert_eq!(
        fixture.send("Check progress.").await.status,
        TurnStatus::Completed
    );
    assert_eq!(fixture.requests().len(), 7);
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("count")).unwrap(),
        "6"
    );
    assert!(
        !fixture.requests().last().unwrap()["messages"]
            .to_string()
            .contains("[Repeated call]")
    );
    fixture.close().await;
}

#[tokio::test]
async fn plan_mode_and_review_disable_all_three_guards() {
    for plan_mode in [true, false] {
        let fixture = Fixture::new(
            (0..5)
                .map(read)
                .chain([
                    plan(5),
                    answer("Stopped."),
                    reply(json!({"content":"Cut off"}), "length"),
                ])
                .collect(),
        )
        .await;
        std::fs::write(fixture.root.path().join("note.txt"), "Facts.\n").unwrap();
        fixture
            .session
            .update(SessionUpdateParams {
                plan_mode: Some(plan_mode),
                ..Default::default()
            })
            .await
            .unwrap();
        for _ in 0..2 {
            let mut events = fixture
                .session
                .subscribe(fixture.session.snapshot().await.event_seq)
                .await
                .unwrap();
            let mut params = input("Inspect.");
            params.review = (!plan_mode).then(|| "Review files".into());
            fixture.session.input(params).await.unwrap();
            assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
        }
        assert_eq!(fixture.requests().len(), 8);
        assert!(
            !fixture.requests().last().unwrap()["messages"]
                .to_string()
                .contains("[Repeated call]")
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn repeated_calls_pause_an_active_goal() {
    let fixture = Fixture::new((0..5).map(read).collect()).await;
    std::fs::write(fixture.root.path().join("note.txt"), "Facts.\n").unwrap();
    let mut events = fixture.session.subscribe(0).await.unwrap();
    fixture
        .session
        .set_goal(GoalSetParams {
            objective: "Inspect.".into(),
            budget_usd: Some(0.0),
        })
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Interrupted);
    assert_eq!(
        fixture.session.goal().await.goal.unwrap().status,
        GoalStatus::Paused
    );
    assert_eq!(fixture.requests().len(), 5);
    assert!(fixture.session.running_work().await.is_none());
    fixture.close().await;
}

#[tokio::test]
async fn active_goal_continues_in_its_own_turn_without_a_plan_reminder() {
    let fixture = Fixture::new(vec![
        plan(0), answer("First goal turn."),
        tool(1, "update_plan", r#"{"plan":[{"step":"Inspect","status":"completed"},{"step":"Verify","status":"completed"}]}"#, "tool_calls"),
        tool(2, "update_goal", r#"{"status":"complete"}"#, "tool_calls"),
        answer("Done."),
    ]).await;
    let mut events = fixture.session.subscribe(0).await.unwrap();
    fixture
        .session
        .set_goal(GoalSetParams {
            objective: "Inspect.".into(),
            budget_usd: Some(0.0),
        })
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(fixture.requests().len(), 5);
    assert!(
        !fixture.requests().last().unwrap()["messages"]
            .to_string()
            .contains("[Unfinished plan]")
    );
    assert_eq!(
        fixture.session.goal().await.goal.unwrap().status,
        GoalStatus::Complete
    );
    fixture.close().await;
}

#[tokio::test]
async fn goal_final_summary_does_not_continue_a_truncated_reply_or_execute_tools() {
    let charged = Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices":[{"index":0,"delta":{"content":"Budget spent."},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":5,"total_tokens":105}
        })
    ));
    let fixture = Fixture::new(vec![
        charged,
        tool(
            0,
            "write",
            r#"{"path":"must-not-exist","content":"No"}"#,
            "length",
        ),
    ])
    .await;
    let mut events = fixture.session.subscribe(0).await.unwrap();
    fixture
        .session
        .set_goal(GoalSetParams {
            objective: "Inspect.".into(),
            budget_usd: Some(1.0),
        })
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(fixture.requests().len(), 2);
    assert!(!fixture.root.path().join("must-not-exist").exists());
    assert_eq!(
        fixture.session.goal().await.goal.unwrap().status,
        GoalStatus::BudgetLimited
    );
    fixture.close().await;
}

#[tokio::test]
async fn escape_during_a_continuation_stops_without_another_request() {
    for unfinished_plan in [false, true] {
        let mut hanging = Reply::sse(format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"Working"}}]})
        ));
        hanging.hang = true;
        let replies = if unfinished_plan {
            vec![plan(0), answer("Stopping."), hanging]
        } else {
            vec![reply(json!({"content":"Half"}), "length"), hanging]
        };
        let fixture = Fixture::new(replies).await;
        let mut events = fixture.session.subscribe(0).await.unwrap();
        let started = fixture.session.input(input("Work.")).await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if matches!(events.recv().await.unwrap().unwrap().payload, EventPayload::ItemDelta { delta: ItemDelta::AgentMessage { ref delta }, .. } if delta == "Working") {
                    break;
                }
            }
        }).await.unwrap();
        fixture
            .session
            .interrupt(SessionInterruptParams {
                turn_id: started.turn_id.unwrap(),
                retract: None,
            })
            .await
            .unwrap();
        assert_eq!(finished(&mut events).await.status, TurnStatus::Interrupted);
        assert_eq!(
            fixture.requests().len(),
            if unfinished_plan { 3 } else { 2 }
        );
        fixture.close().await;
    }
}

#[tokio::test]
async fn user_steering_resets_the_plan_reminder_budget() {
    let fixture = Fixture::new(vec![
        plan(0), answer("Stop 1."), answer("Stop 2."), answer("Stop 3."),
        tool(1, "ask_user", r#"{"question":"Which direction?","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]}"#, "tool_calls"),
        answer("New stop 1."), answer("New stop 2."), answer("New stop 3."), answer("New stop 4."),
    ]).await;
    let mut events = fixture.session.subscribe(0).await.unwrap();
    fixture.session.input(input("Work.")).await.unwrap();
    let question = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let EventPayload::RequestOpened { request } =
                events.recv().await.unwrap().unwrap().payload
            {
                break request;
            }
        }
    })
    .await
    .unwrap();
    let mut steer = input("Focus on A.");
    steer.mode = InputMode::Steer;
    fixture.session.input(steer).await.unwrap();
    fixture
        .session
        .resolve(
            &question.id,
            serde_json::from_value(
                json!({"answer":{"type":"option","label":"A"},"clientName":"test"}),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 9);
    assert_eq!(
        requests[5]["messages"].as_array().unwrap().last().unwrap()["content"],
        "Focus on A."
    );
    let events = fixture.store.events_after("guards", 0).unwrap();
    assert_eq!(events.iter().filter(|event| matches!(&event.payload, EventPayload::Warning { message } if message.contains("unfinished plan"))).count(), 6);
    fixture.close().await;
}

#[tokio::test]
async fn each_new_user_turn_gets_a_fresh_continuation_budget() {
    let fixture = Fixture::new(
        (0..8)
            .map(|_| reply(json!({"content":"Partial"}), "length"))
            .collect(),
    )
    .await;
    for turn in 1..=2 {
        assert_eq!(
            fixture.send("Continue.").await.status,
            TurnStatus::Completed
        );
        assert_eq!(fixture.requests().len(), turn * 4);
    }
    fixture.close().await;
}

#[tokio::test]
async fn subagents_are_reminded_about_unfinished_plans() {
    let fixture = Fixture::new(vec![
        answer("Parent ready."),
        plan(0),
        answer("Stopped."),
        answer("Blocked."),
        answer("Need input."),
        answer("Still blocked."),
    ])
    .await;
    fixture.send("Prepare.").await;
    let (child, _) = fixture
        .session
        .spawn_agent("child".into(), "Do the task.".into())
        .await
        .unwrap();
    let mut events = child.subscribe(0).await.unwrap();
    assert_eq!(finished(&mut events).await.status, TurnStatus::Completed);
    assert_eq!(fixture.requests().len(), 6);
    assert!(
        fixture.requests()[3]["messages"]
            .to_string()
            .contains("[Unfinished plan]")
    );
    child.close().await.unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn ordinary_turn_has_one_request_and_only_the_existing_conversation_events() {
    let fixture = Fixture::new(vec![answer("Done.")]).await;
    let turn = fixture.send("Exact user words.").await;
    assert_eq!(turn.status, TurnStatus::Completed);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    let messages = requests[0]["messages"].as_array().unwrap();
    assert_eq!(
        messages.last().unwrap(),
        &json!({"role":"user","content":"Exact user words."})
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message["role"] == "user")
            .count(),
        1
    );
    let events = fixture.store.events_after("guards", 0).unwrap();
    assert!(events.iter().any(|event| matches!(&event.payload, EventPayload::ItemCompleted { item: Item::AgentMessage { text, .. }, .. } if text == "Done.")));
    assert!(
        !events
            .iter()
            .any(|event| matches!(&event.payload, EventPayload::Warning { .. }))
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.payload, EventPayload::TurnStarted { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(&event.payload, EventPayload::TurnCompleted { .. }))
            .count(),
        1
    );
    let history = fixture.store.model_history::<Value>("guards").unwrap();
    let messages: Vec<_> = history
        .iter()
        .filter(|entry| entry["type"] == "message")
        .collect();
    assert_eq!(messages.len(), 2);
    fixture.close().await;
}

#[tokio::test]
async fn a_truncated_tool_call_can_be_replaced_with_a_complete_smaller_call() {
    let fixture = Fixture::new(vec![
        tool(
            0,
            "write",
            r#"{"path":"must-not-exist","content":"#,
            "length",
        ),
        tool(
            1,
            "write",
            r#"{"path":"small.txt","content":"Done."}"#,
            "tool_calls",
        ),
        answer("Finished."),
    ])
    .await;
    assert_eq!(
        fixture.send("Write a file.").await.status,
        TurnStatus::Completed
    );
    assert_eq!(fixture.requests().len(), 3);
    assert!(!fixture.root.path().join("must-not-exist").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("small.txt")).unwrap(),
        "Done."
    );
    fixture.close().await;
}

#[tokio::test]
async fn polling_a_running_silent_background_command_does_not_trigger_the_repeat_guard() {
    let fixture = Fixture::new(
        [tool(
            0,
            "shell",
            r#"{"command":"sleep 60","background":true}"#,
            "tool_calls",
        )]
        .into_iter()
        .chain((1..=6).map(|id| tool(id, "shell_output", r#"{"id":1}"#, "tool_calls")))
        .chain([answer("Still waiting.")])
        .collect(),
    )
    .await;
    let turn = fixture.send("Wait for the command.").await;
    let requests = fixture.requests();
    // Always stop the isolated session's own background process group.
    fixture.close().await;
    assert_eq!(turn.status, TurnStatus::Completed);
    assert_eq!(requests.len(), 8);
    let results: Vec<_> = requests[2..]
        .iter()
        .map(|request| {
            request["messages"].as_array().unwrap().last().unwrap()["content"]
                .as_str()
                .unwrap()
        })
        .collect();
    assert!(results[0].contains("running"), "{}", results[0]);
    assert!(results.iter().all(|result| *result == results[0]));
    assert!(
        !requests.last().unwrap()["messages"]
            .to_string()
            .contains("[Repeated call]")
    );
}

struct Auth;
impl ResponsesAuth for Auth {
    fn credential<'a>(
        &'a self,
        _: &'a Request,
        _: Option<&'a ModelContext>,
    ) -> BoxFuture<'a, Result<Box<dyn ResponsesCredential>, ModelError>> {
        Box::pin(async { Ok(Box::new(Credential) as Box<dyn ResponsesCredential>) })
    }
}
struct Credential;
impl ResponsesCredential for Credential {
    fn authorization(&mut self) -> BoxFuture<'_, Result<axum::http::HeaderValue, ModelError>> {
        Box::pin(async { Ok(axum::http::HeaderValue::from_str(&format!("Bearer {KEY}")).unwrap()) })
    }
    fn recover_unauthorized(&mut self) -> BoxFuture<'_, Result<(), ModelError>> {
        Box::pin(async {
            Err(ModelError {
                kind: canter_model::ErrorKind::Authentication,
                status: None,
                retry_after: None,
                retries: 0,
            })
        })
    }
    fn replay_account(&self) -> String {
        "test-account".into()
    }
}

fn response_events(events: impl IntoIterator<Item = Value>) -> Reply {
    Reply::sse(
        events
            .into_iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect::<String>(),
    )
}

fn response_truncated(kind: &str, reason: &str) -> Reply {
    let delta = match kind {
        "message" => {
            json!({"type":"response.output_text.delta","output_index":0,"item_id":"partial","content_index":0,"delta":"Half a reply"})
        }
        "function_call" => {
            json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"partial","delta":"{\"path\":\"must-not-exist\",\"content\":"})
        }
        _ => {
            json!({"type":"response.custom_tool_call_input.delta","output_index":0,"item_id":"partial","delta":"*** Begin Patch\n*** Add File: must-not-exist\n"})
        }
    };
    response_events([
        json!({"type":"response.output_item.added","output_index":0,"item":{"type":kind,"id":"partial","name":if kind == "custom_tool_call" { "apply_patch" } else { "write" },"namespace":"canter","call_id":"call-partial"}}),
        delta,
        json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":reason}}}),
    ])
}

fn response_complete(item: Value) -> Reply {
    response_events([
        json!({"type":"response.output_item.done","output_index":0,"item":item}),
        json!({"type":"response.completed","response":{"status":"completed"}}),
    ])
}

fn response_answer() -> Reply {
    response_complete(
        json!({"id":"message-done","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Finished."}]}),
    )
}

#[tokio::test]
async fn responses_output_limit_continues_text_without_changing_the_input_prefix() {
    let fixture = Fixture::with_api(
        vec![
            response_truncated("message", "max_output_tokens"),
            response_answer(),
        ],
        true,
    )
    .await;
    assert_eq!(fixture.send("Explain.").await.status, TurnStatus::Completed);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 2);
    let before = requests[0]["input"].as_array().unwrap();
    let after = requests[1]["input"].as_array().unwrap();
    assert_eq!(
        serde_json::to_vec(before).unwrap(),
        serde_json::to_vec(&after[..before.len()]).unwrap()
    );
    assert!(requests[1]["input"].to_string().contains("Half a reply"));
    assert!(
        requests[1]["input"]
            .to_string()
            .contains("Continue from where")
    );
    fixture.close().await;
}

#[tokio::test]
async fn responses_truncated_tools_are_not_executed_and_share_the_three_continuation_limit() {
    let fixture = Fixture::with_api(
        ["message", "function_call", "custom_tool_call", "message"]
            .into_iter()
            .map(|kind| response_truncated(kind, "max_output_tokens"))
            .collect(),
        true,
    )
    .await;
    assert_eq!(fixture.send("Write.").await.status, TurnStatus::Completed);
    let requests = fixture.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests[2..] {
        assert!(
            request["input"]
                .to_string()
                .contains("Split them into smaller steps")
        );
        assert!(!request["input"].to_string().contains("call-partial"));
    }
    assert!(!fixture.root.path().join("must-not-exist").exists());
    let events = fixture.store.events_after("guards", 0).unwrap();
    assert!(events.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message == "The model reply was incomplete; its tool calls were not run.")));
    fixture.close().await;
}

#[tokio::test]
async fn responses_can_replace_a_truncated_call_with_a_smaller_complete_call() {
    let fixture = Fixture::with_api(vec![
        response_truncated("function_call", "max_output_tokens"),
        response_complete(json!({"id":"small-write","type":"function_call","call_id":"small-call","name":"write","namespace":"canter","arguments":r#"{"path":"small.txt","content":"Done."}"#,"status":"completed"})),
        response_answer(),
    ], true).await;
    assert_eq!(
        fixture.send("Write a file.").await.status,
        TurnStatus::Completed
    );
    assert_eq!(fixture.requests().len(), 3);
    assert!(!fixture.root.path().join("must-not-exist").exists());
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("small.txt")).unwrap(),
        "Done."
    );
    fixture.close().await;
}

#[tokio::test]
async fn responses_content_filter_does_not_auto_continue() {
    let fixture =
        Fixture::with_api(vec![response_truncated("message", "content_filter")], true).await;
    assert_eq!(fixture.send("Explain.").await.status, TurnStatus::Failed);
    assert_eq!(fixture.requests().len(), 1);
    let events = fixture.store.events_after("guards", 0).unwrap();
    assert!(!events.iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message.contains("continuing automatically"))));
    fixture.close().await;
}
