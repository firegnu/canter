use super::*;
use canter_model::{Message, ToolCall};
use std::sync::Mutex as SyncMutex;

#[derive(Default)]
struct WaitModel {
    child_done: Notify,
    parent_held: Notify,
    parent_release: Notify,
    wait_returned: Notify,
    results: SyncMutex<Vec<String>>,
}

impl Model for WaitModel {
    fn stream(&self, request: Request, cancel: CancellationToken) -> ModelStream<'_> {
        if !request.tools.iter().any(|tool| tool.name == "spawn_agent") {
            return Box::pin(async_stream::stream! {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = self.child_done.notified() => {},
                }
                yield Ok(ModelEvent::TextDelta {
                    item_id: "answer".into(),
                    delta: "Child findings".into(),
                });
                yield Ok(ModelEvent::Finished(FinishReason::Stop));
            });
        }
        if matches!(request.messages.last(), Some(Message::User { text }) if text == "Hold") {
            return Box::pin(async_stream::stream! {
                self.parent_held.notify_one();
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = self.parent_release.notified() => {},
                }
                yield Ok(ModelEvent::TextDelta { item_id: "answer".into(), delta: "Ready".into() });
                yield Ok(ModelEvent::Finished(FinishReason::Stop));
            });
        }
        let spawned = request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::ToolResult {
                    tool_call_id, text, ..
                } if tool_call_id == "spawn_agent" => Some(
                    serde_json::from_str::<serde_json::Value>(text.lines().next().unwrap())
                        .unwrap()["id"]
                        .clone(),
                ),
                _ => None,
            });
        let goal = request
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { text } if text.starts_with("[Goal]")));
        let call = match request.messages.last() {
            Some(Message::User { text })
                if text == "Dispatch and wait" || (goal && spawned.is_none()) =>
            {
                Some((
                    "spawn_agent",
                    json!({"task":"Inspect parser","type":"explorer","name":"Inspect the parser","depth":"medium"}),
                ))
            }
            Some(Message::ToolResult { tool_call_id, .. })
                if tool_call_id == "spawn_agent" && !goal =>
            {
                Some((
                    "wait_agent",
                    json!({"ids":[spawned.unwrap()],"timeoutMs":600000}),
                ))
            }
            Some(Message::User { text }) if text == "Wait again" || goal => Some((
                "wait_agent",
                json!({"ids":[spawned.unwrap()],"timeoutMs":600000}),
            )),
            Some(Message::ToolResult {
                tool_call_id, text, ..
            }) if tool_call_id == "wait_agent" => {
                self.results.lock().unwrap().push(text.clone());
                self.wait_returned.notify_one();
                goal.then(|| ("update_goal", json!({"status":"complete"})))
            }
            _ => None,
        };
        Box::pin(futures_util::stream::iter(match call {
            Some((name, arguments)) => vec![
                Ok(ModelEvent::ToolCall(ToolCall {
                    id: name.into(),
                    name: name.into(),
                    arguments: arguments.to_string(),
                })),
                Ok(ModelEvent::Finished(FinishReason::ToolCalls)),
            ],
            None => vec![
                Ok(ModelEvent::TextDelta {
                    item_id: "answer".into(),
                    delta: "Parent reply".into(),
                }),
                Ok(ModelEvent::Finished(FinishReason::Stop)),
            ],
        }))
    }
}

async fn submit(test: &Harness, url: &str, text: &str, mode: &str, non_interactive: bool) {
    test.client.post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":text}],"mode":mode,"nonInteractive":non_interactive,"clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap();
}

async fn returned(model: &WaitModel) {
    tokio::time::timeout(Duration::from_secs(2), model.wait_returned.notified())
        .await
        .expect("wait_agent should return promptly");
}

fn assert_running_result(model: &WaitModel) {
    let results = model.results.lock().unwrap();
    let result = results.last().unwrap();
    assert!(result.contains("running"), "{result}");
    assert!(result.contains("reports will wake you"), "{result}");
    assert!(
        result.contains("reply to the user now and end this turn"),
        "{result}"
    );
}

#[tokio::test]
async fn interactive_dispatch_wait_returns_while_the_child_is_running() {
    let model = Arc::new(WaitModel::default());
    let test = Harness::with_model(model.clone()).await;
    let url = parent(&test).await;
    submit(&test, &url, "Dispatch and wait", "queue", false).await;
    let returned =
        tokio::time::timeout(Duration::from_secs(2), model.wait_returned.notified()).await;
    let snapshot = test.read(&url).await.session;
    test.stop().await;
    returned.expect("interactive wait_agent must return without waiting for the child");
    assert_running_result(&model);
    assert!(snapshot.turns.last().unwrap().items.iter().any(|item| matches!(
        item,
        Item::CollabAgentToolCall { tool: CollabAgentTool::Wait, status: ItemStatus::Completed, agents, .. }
        if agents[0].status == Some(AgentStatus::Running)
    )));
}

#[tokio::test]
async fn interactive_user_input_wait_returns_for_direct_queued_and_steered_messages() {
    for mode in ["direct", "queue", "steer"] {
        let model = Arc::new(WaitModel::default());
        let test = Harness::with_model(model.clone()).await;
        let url = parent(&test).await;
        submit(&test, &url, "Dispatch and wait", "queue", false).await;
        returned(&model).await;
        idle(&test, &url).await;
        if mode != "direct" {
            submit(&test, &url, "Hold", "queue", false).await;
            tokio::time::timeout(Duration::from_secs(2), model.parent_held.notified())
                .await
                .unwrap();
        }
        submit(
            &test,
            &url,
            "Wait again",
            if mode == "direct" { "queue" } else { mode },
            false,
        )
        .await;
        model.parent_release.notify_one();
        returned(&model).await;
        idle(&test, &url).await;
        assert_running_result(&model);
        assert_eq!(model.results.lock().unwrap().len(), 2);
        test.stop().await;
    }
}

async fn blocking_wait(non_interactive: bool, auto_wake_limit: u32, goal: bool) {
    let model = Arc::new(WaitModel::default());
    let test = Harness::with_options(
        model.clone(),
        CoreOptions {
            auto_wake_limit,
            ..Default::default()
        },
    )
    .await;
    let url = parent(&test).await;
    if goal {
        test.app
            .opened_session(url.rsplit('/').next().unwrap())
            .await
            .unwrap()
            .set_goal(GoalSetParams {
                objective: "Inspect parser".into(),
                budget_usd: Some(0.0),
            })
            .await
            .unwrap();
    } else {
        submit(&test, &url, "Dispatch and wait", "queue", non_interactive).await;
    }
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = test.read(&url).await.session;
            if snapshot.turns.last().unwrap().items.iter().any(|item| {
                matches!(
                    item,
                    Item::CollabAgentToolCall {
                        tool: CollabAgentTool::Wait,
                        status: ItemStatus::InProgress,
                        ..
                    }
                )
            }) {
                if goal {
                    assert_eq!(
                        snapshot.turns.len(),
                        3,
                        "wait in the automatic continuation after the dispatch turn"
                    );
                    assert!(snapshot.turns.last().unwrap().input.is_empty());
                }
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), model.wait_returned.notified())
            .await
            .is_err(),
        "this turn must still wait"
    );
    model.child_done.notify_one();
    returned(&model).await;
    idle(&test, &url).await;
    let result = model.results.lock().unwrap()[0].clone();
    assert!(
        result.contains("completed") && result.contains("Child findings"),
        "{result}"
    );
    assert!(!result.contains("Not waiting"), "{result}");
    test.stop().await;
}

#[tokio::test]
async fn exec_wait_agent_still_waits_for_the_child() {
    blocking_wait(true, 10, false).await;
}

#[tokio::test]
async fn disabled_auto_wake_wait_agent_still_waits_for_the_child() {
    blocking_wait(false, 0, false).await;
}

#[tokio::test]
async fn goal_continuation_wait_agent_still_waits_for_the_child() {
    blocking_wait(false, 10, true).await;
}

#[tokio::test]
async fn interactive_wait_returns_a_completed_childs_final_reply() {
    let model = Arc::new(WaitModel::default());
    let test = Harness::with_model(model.clone()).await;
    let url = parent(&test).await;
    submit(&test, &url, "Dispatch and wait", "queue", false).await;
    returned(&model).await;
    let snapshot = idle(&test, &url).await;
    let child_id = snapshot
        .turns
        .last()
        .unwrap()
        .items
        .iter()
        .find_map(|item| match item {
            Item::CollabAgentToolCall {
                tool: CollabAgentTool::SpawnAgent,
                agents,
                ..
            } => Some(agents[0].id.clone()),
            _ => None,
        })
        .unwrap();
    model.child_done.notify_one();
    idle(&test, &format!("http://localhost/v1/sessions/{child_id}")).await;
    submit(&test, &url, "Wait again", "queue", false).await;
    returned(&model).await;
    idle(&test, &url).await;
    let result = model.results.lock().unwrap().last().unwrap().clone();
    let value: serde_json::Value = serde_json::from_str(&result).unwrap();
    assert_eq!(value["data"][0]["status"], "completed");
    assert_eq!(value["data"][0]["result"], "Child findings");
    test.stop().await;
}
