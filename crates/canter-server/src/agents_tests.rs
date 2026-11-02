use super::*;

#[allow(dead_code)] // Shared fake server also records request timing for retry tests.
#[path = "../../canter-model/tests/support/mod.rs"]
mod model_server;

pub(super) async fn harness(hold_child: bool) -> (Harness, model_server::Server) {
    let replies = (0..8).map(|index| {
        if hold_child && index == 1 {
            model_server::Reply { hang: true, ..model_server::Reply::sse("") }
        } else {
            model_server::Reply::sse(format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{"content":"recovered"},"finish_reason":"stop"}]})))
        }
    }).collect();
    let fake = model_server::Server::start(replies).await;
    let model = canter_model::ChatCompletions::new(
        &fake.base_url,
        canter_model::ApiKey::new(model_server::KEY),
    )
    .unwrap();
    (Harness::with_model(Arc::new(model)).await, fake)
}

pub(super) async fn parent(test: &Harness) -> String {
    let home = test.root.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    test.client
        .post("http://localhost/v1/folderTrust")
        .json(&json!({"cwd":test.root.path()}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let created: SessionCreateResponse = test.client.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":test.root.path(),"env":{"HOME":home,"CANTER_HOME":test.app.home,"PATH":"/usr/bin:/bin"},"model":"fake-model","approvalPolicy":"alwaysAsk","clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let url = format!("http://localhost/v1/sessions/{}", created.session.id);
    say(test, &url, "Parent task").await;
    url
}

async fn say(test: &Harness, url: &str, text: &str) -> SessionSnapshot {
    test.client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":text}],"mode":"queue","clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    idle(test, url).await
}

pub(super) async fn idle(test: &Harness, url: &str) -> SessionSnapshot {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = test.read(url).await.session;
            if matches!(snapshot.status, SessionStatus::Idle | SessionStatus::Error) {
                return snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

// Exercise the same server entry point used by the model tool host.
pub(super) async fn spawn(test: &Harness, parent: &str, task: &str) -> String {
    let id = parent.rsplit('/').next().unwrap();
    let response = test
        .app
        .spawn_agent(id.into(), task.into())
        .await
        .unwrap_or_else(|error| panic!("{}", error.0.message));
    format!("http://localhost/v1/sessions/{}", response.session.id)
}

#[tokio::test]
async fn agents_are_listed_under_parent_and_use_ordinary_read_and_events() {
    let (test, fake) = harness(false).await;
    let parent = parent(&test).await;
    let child = spawn(&test, &parent, "Inspect parser").await;
    let snapshot = idle(&test, &child).await;
    let response = test
        .client
        .get(format!("{parent}/agents"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let agents: AgentListResponse = response.json().await.unwrap();
    assert_eq!(agents.data.len(), 1);
    assert_eq!(agents.data[0].task, "Inspect parser");
    assert_eq!(agents.data[0].status, AgentStatus::Completed);
    assert_eq!(agents.data[0].result.as_deref(), Some("recovered"));
    assert_eq!(agents.data[0].created_at, snapshot.created_at);
    let list: SessionListResponse = test
        .client
        .get("http://localhost/v1/sessions")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.data.len(), 1);
    assert_eq!(list.data[0].id, parent.rsplit('/').next().unwrap());
    let info: InfoResponse = test
        .client
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(info.features.unwrap().contains(&"subagentSessions".into()));
    let mut events = test
        .client
        .get(format!(
            "http://localhost/v1/events?session={}&after=0",
            snapshot.id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(events.status(), StatusCode::OK);
    let mut received = String::new();
    while !received.contains("session.parent") {
        received.push_str(&String::from_utf8_lossy(
            &events.chunk().await.unwrap().unwrap(),
        ));
    }
    drop(events);
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn trash_parent_moves_children_and_direct_child_trash_keeps_parent() {
    let (test, fake) = harness(false).await;
    let parent = parent(&test).await;
    let first = spawn(&test, &parent, "First task").await;
    idle(&test, &first).await;
    let second = spawn(&test, &parent, "Second task").await;
    idle(&test, &second).await;
    assert_eq!(
        test.client
            .post(format!("{first}/trash"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        test.client.get(&parent).send().await.unwrap().status(),
        StatusCode::OK
    );
    // Completion notices now wake the interactive parent. Finish that turn
    // before testing that an idle family can move to trash.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let snapshot = test.read(&parent).await.session;
            if snapshot.turns.len() >= 2 && snapshot.status == SessionStatus::Idle {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        test.client
            .post(format!("{parent}/trash"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    for url in [&parent, &first, &second] {
        let id = url.rsplit('/').next().unwrap();
        assert!(
            test.app
                .home
                .join("trash")
                .join(id)
                .join("events.jsonl")
                .exists(),
            "{id} missing from trash"
        );
        assert_eq!(
            test.client.get(url.as_str()).send().await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }
    test.stop().await;
    fake.stop().await;
}

async fn rewind_params(test: &Harness, url: &str, turn: &str) -> serde_json::Value {
    let mut params = json!({"beforeTurnId":turn,"rewindFiles":false,"rewindConversation":true});
    let preview: RewindPreviewResponse = test
        .client
        .post(format!("{url}/rewind/preview"))
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    params["lastEventSeq"] = json!(preview.last_event_seq);
    params
}

#[tokio::test]
async fn rewind_trashes_only_children_created_in_removed_turns() {
    let (test, fake) = harness(false).await;
    let parent = parent(&test).await;
    let kept = spawn(&test, &parent, "Keep this task").await;
    idle(&test, &kept).await;
    let second_turn = say(&test, &parent, "Second parent turn")
        .await
        .turns
        .last()
        .unwrap()
        .id
        .clone();
    let removed = spawn(&test, &parent, "Remove this task").await;
    idle(&test, &removed).await;
    let params = rewind_params(&test, &parent, &second_turn).await;
    assert_eq!(
        test.client
            .post(format!("{parent}/rewind"))
            .json(&params)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        test.client.get(&kept).send().await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        test.client.get(&removed).send().await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    let removed_id = removed.rsplit('/').next().unwrap();
    assert!(test.app.home.join("trash").join(removed_id).exists());
    assert!(
        !test
            .read(&parent)
            .await
            .session
            .turns
            .iter()
            .any(|turn| turn.id == second_turn)
    );
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn running_child_blocks_trash_and_rewind_without_changing_parent() {
    let (test, fake) = harness(true).await;
    let parent = parent(&test).await;
    let before = test.read(&parent).await;
    let params = rewind_params(&test, &parent, &before.session.turns[0].id).await;
    let child = spawn(&test, &parent, "Hold child").await;
    assert_eq!(
        test.read(&child).await.session.agent.unwrap().status,
        AgentStatus::Running
    );
    assert_eq!(
        test.client
            .post(format!("{parent}/trash"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        test.client
            .post(format!("{parent}/rewind"))
            .json(&params)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(test.read(&parent).await, before);
    assert_eq!(
        test.read(&child).await.session.agent.unwrap().status,
        AgentStatus::Running
    );
    test.client
        .delete(&child)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(
        test.read(&child).await.session.agent.unwrap().status,
        AgentStatus::Closed
    );
    test.client
        .post(format!("{parent}/trash"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn forking_parent_does_not_copy_children_and_forking_child_detaches_it() {
    let (test, fake) = harness(false).await;
    let parent = parent(&test).await;
    let child = spawn(&test, &parent, "Fork source").await;
    idle(&test, &child).await;
    for source in [&parent, &child] {
        let copied: SessionCreateResponse = test
            .client
            .post(format!("{source}/fork"))
            .json(
                &json!({"env":{"HOME":test.root.path().join("home"),"CANTER_HOME":test.app.home}}),
            )
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(copied.session.parent.is_none());
        assert!(copied.session.agent.is_none());
        let list: AgentListResponse = test
            .client
            .get(format!(
                "http://localhost/v1/sessions/{}/agents",
                copied.session.id
            ))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(list.data.is_empty());
    }
    assert!(test.read(&child).await.session.parent.is_some());
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn child_trash_conflict_and_stale_rewind_leave_family_intact() {
    let (test, fake) = harness(false).await;
    let parent = parent(&test).await;
    let child = spawn(&test, &parent, "Keep on conflict").await;
    idle(&test, &child).await;
    let before = test.read(&parent).await;
    let mut params = rewind_params(&test, &parent, &before.session.turns[0].id).await;
    params["lastEventSeq"] = json!(0);
    assert_eq!(
        test.client
            .post(format!("{parent}/rewind"))
            .json(&params)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(test.read(&parent).await, before);
    assert_eq!(
        test.client.get(&child).send().await.unwrap().status(),
        StatusCode::OK
    );
    std::fs::create_dir_all(
        test.app
            .home
            .join("trash")
            .join(child.rsplit('/').next().unwrap()),
    )
    .unwrap();
    assert_eq!(
        test.client
            .post(format!("{parent}/trash"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(test.read(&parent).await, before);
    assert_eq!(test.read(&child).await.session.status, SessionStatus::Idle);
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn model_can_spawn_a_named_child_without_waiting_for_its_result() {
    let spawn = model_server::Reply::sse(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"spawn-1","type":"function","function":{"name":"spawn_agent","arguments":json!({"task":"Inspect the parser","type":"explorer"}).to_string()}}]},"finish_reason":"tool_calls"}]})
    ));
    let mut replies = vec![spawn];
    replies.extend((0..8).map(|_| model_server::Reply::sse(format!("data: {}\n\ndata: [DONE]\n\n", json!({"choices":[{"index":0,"delta":{"content":"Inspection finished"},"finish_reason":"stop"}]})))));
    let fake = model_server::Server::start(replies).await;
    let model = canter_model::ChatCompletions::new(
        &fake.base_url,
        canter_model::ApiKey::new(model_server::KEY),
    )
    .unwrap();
    let test = Harness::with_model(Arc::new(model)).await;
    let url = parent(&test).await;
    let agents: AgentListResponse = test
        .client
        .get(format!("{url}/agents"))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(agents.data.len(), 1);
    let child = test
        .read(&format!(
            "http://localhost/v1/sessions/{}",
            agents.data[0].id
        ))
        .await
        .session;
    assert!(!child.settings.name.as_ref().unwrap().is_empty());
    assert_eq!(child.settings.model, "fake-model");
    let requests = fake.requests.lock().unwrap().clone();
    assert!(
        requests[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["function"]["name"] == "spawn_agent")
    );
    assert!(requests.iter().any(|request| {
        request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| {
                message["role"] == "tool"
                    && message["content"]
                        .as_str()
                        .unwrap_or_default()
                        .contains(&agents.data[0].id)
            })
    }));
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn fifth_running_agent_is_rejected_but_finished_agents_release_the_slot() {
    let mut replies = vec![model_server::Reply::sse(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Ready\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
    )];
    replies.extend((0..6).map(|_| model_server::Reply {
        hang: true,
        ..model_server::Reply::sse("")
    }));
    let fake = model_server::Server::start(replies).await;
    let model = canter_model::ChatCompletions::new(
        &fake.base_url,
        canter_model::ApiKey::new(model_server::KEY),
    )
    .unwrap();
    let test = Harness::with_model(Arc::new(model)).await;
    let url = parent(&test).await;
    let id = url.rsplit('/').next().unwrap();
    let mut children = Vec::new();
    for _ in 0..4 {
        children.push(
            test.app
                .spawn_agent(id.into(), "Wait for work".into())
                .await
                .unwrap_or_else(|error| panic!("{}", error.0.message))
                .session
                .id,
        );
    }
    let refused = test.app.spawn_agent(id.into(), "Fifth task".into()).await;
    assert!(
        refused.is_err(),
        "a fifth running child must not be created"
    );
    test.app
        .opened_session(&children[0])
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    assert!(
        test.app
            .spawn_agent(id.into(), "Replacement task".into())
            .await
            .is_ok()
    );
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn agent_tools_wait_continue_interrupted_and_close_without_losing_results() {
    use canter_core::{AgentCommand, AgentHost};
    let (test, fake) = harness(true).await;
    let url = parent(&test).await;
    let child_url = spawn(&test, &url, "First task").await;
    let parent_id = url.rsplit('/').next().unwrap().to_owned();
    let child_id = child_url.rsplit('/').next().unwrap().to_owned();
    let host = super::super::agent_sessions::Host(Arc::downgrade(&test.app));
    let result = host
        .execute(
            parent_id.clone(),
            AgentCommand::WaitAgent {
                ids: vec![child_id.clone()],
                timeout_ms: 10,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["data"][0]["status"], "running");
    let child = test.app.opened_session(&child_id).await.unwrap();
    let turn = child
        .snapshot()
        .await
        .session
        .turns
        .last()
        .unwrap()
        .id
        .clone();
    child
        .interrupt(SessionInterruptParams {
            turn_id: turn,
            retract: None,
        })
        .await
        .unwrap();
    let result = host
        .execute(
            parent_id.clone(),
            AgentCommand::WaitAgent {
                ids: vec![child_id.clone()],
                timeout_ms: 1000,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["data"][0]["status"], "interrupted");
    host.execute(
        parent_id.clone(),
        AgentCommand::SendInput {
            id: child_id.clone(),
            message: "Continue".into(),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let result = host
        .execute(
            parent_id.clone(),
            AgentCommand::WaitAgent {
                ids: vec![child_id.clone()],
                timeout_ms: 1000,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["data"][0]["status"], "completed");
    assert_eq!(result["data"][0]["result"], "recovered");
    host.execute(
        parent_id,
        AgentCommand::CloseAgent { id: child_id },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        test.read(&child_url)
            .await
            .session
            .agent
            .unwrap()
            .result
            .as_deref(),
        Some("recovered")
    );
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn child_approval_is_forwarded_and_declining_through_child_endpoint_continues() {
    let reply = |delta: serde_json::Value, finish: &str| {
        model_server::Reply::sse(format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
        ))
    };
    let fake = model_server::Server::start(vec![
        reply(json!({"content":"Ready"}), "stop"),
        reply(json!({"tool_calls":[{"index":0,"id":"shell-approval","type":"function","function":{"name":"shell","arguments":"{\"command\":\"touch declined-file\"}"}}]}), "tool_calls"),
        reply(json!({"content":"Used another approach"}), "stop"),
    ]).await;
    let model = canter_model::ChatCompletions::new(
        &fake.base_url,
        canter_model::ApiKey::new(model_server::KEY),
    )
    .unwrap();
    let test = Harness::with_model(Arc::new(model)).await;
    let url = parent(&test).await;
    let child_url = spawn(&test, &url, "Check with approval").await;
    let parent_session = test
        .app
        .opened_session(url.rsplit('/').next().unwrap())
        .await
        .unwrap();
    let mut events = parent_session.subscribe(0).await.unwrap();
    let forwarded = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = serde_json::to_value(events.recv().await.unwrap().unwrap()).unwrap();
            if event["type"] == "agent.request.opened" {
                break event;
            }
        }
    })
    .await
    .expect("the parent must receive the child's approval request");
    let request_id = forwarded["request"]["id"].as_str().unwrap();
    assert_eq!(forwarded["agentId"], child_url.rsplit('/').next().unwrap());
    assert!(!forwarded["agentName"].as_str().unwrap().is_empty());
    assert_eq!(
        forwarded["request"]["kind"]["arguments"]["command"],
        "touch declined-file"
    );
    test.client
        .post(format!("{child_url}/requests/{request_id}"))
        .json(&json!({"decision":"decline","clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let resolved = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let event = serde_json::to_value(events.recv().await.unwrap().unwrap()).unwrap();
            if event["type"] == "agent.request.resolved" {
                break event;
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(resolved["requestId"], request_id);
    let child = idle(&test, &child_url).await;
    assert_eq!(
        child.agent.unwrap().result.as_deref(),
        Some("Used another approach")
    );
    assert!(!test.root.path().join("declined-file").exists());
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn usage_includes_child_model_requests_once() {
    let reply = || {
        model_server::Reply::sse(format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"Done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":10,"total_tokens":110}})
        ))
    };
    let fake = model_server::Server::start(vec![reply(), reply()]).await;
    let model = canter_model::ChatCompletions::new(
        &fake.base_url,
        canter_model::ApiKey::new(model_server::KEY),
    )
    .unwrap();
    let options = CoreOptions {
        prices: serde_json::from_value(
            json!({"fake-model":{"peak":{"cache_hit":0,"cache_miss":0,"output":1000000}}}),
        )
        .unwrap(),
        ..Default::default()
    };
    let test = Harness::with_options(Arc::new(model), options).await;
    let url = parent(&test).await;
    let child = spawn(&test, &url, "Count this work").await;
    idle(&test, &child).await;
    let report: UsageResponse = test
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
    assert_eq!(report.total.output_tokens, 20);
    assert_eq!(report.total.cost.usd, 20.0);
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn wait_returns_all_states_on_any_completion_and_bounds_the_reply() {
    use canter_core::{AgentCommand, AgentHost};
    let reply = |text: &str| {
        model_server::Reply::sse(format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
        ))
    };
    let long = format!("Start{}End", "finding ".repeat(10_000));
    let mut streamed = String::new();
    for part in long.as_bytes().chunks(1024) {
        streamed.push_str(&format!("data: {}\n\n", json!({"choices":[{"index":0,"delta":{"content":std::str::from_utf8(part).unwrap()},"finish_reason":null}]})));
    }
    streamed.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");
    let fake = model_server::Server::start(vec![
        reply("Ready"),
        model_server::Reply::sse(streamed),
        model_server::Reply {
            hang: true,
            ..model_server::Reply::sse("")
        },
    ])
    .await;
    let model = canter_model::ChatCompletions::new(
        &fake.base_url,
        canter_model::ApiKey::new(model_server::KEY),
    )
    .unwrap();
    let test = Harness::with_model(Arc::new(model)).await;
    let url = parent(&test).await;
    let done = spawn(&test, &url, "Inspect code").await;
    idle(&test, &done).await;
    let running = spawn(&test, &url, "Continue working").await;
    let host = super::super::agent_sessions::Host(Arc::downgrade(&test.app));
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        host.execute(
            url.rsplit('/').next().unwrap().into(),
            AgentCommand::WaitAgent {
                ids: vec![
                    done.rsplit('/').next().unwrap().into(),
                    running.rsplit('/').next().unwrap().into(),
                ],
                timeout_ms: 600_000,
            },
            CancellationToken::new(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["data"].as_array().unwrap().len(), 2);
    assert_eq!(result["data"][0]["status"], "completed");
    assert_eq!(result["data"][1]["status"], "running");
    let bounded = result["data"][0]["result"].as_str().unwrap();
    assert!(bounded.len() <= canter_tools::MAX_MODEL_OUTPUT_BYTES);
    assert!(
        bounded.starts_with("Start") && bounded.ends_with("End") && bounded.contains("[truncated ")
    );
    assert_eq!(
        test.read(&done)
            .await
            .session
            .agent
            .unwrap()
            .result
            .as_deref(),
        Some(long.as_str())
    );
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn exec_result_survives_reopening_the_parent_before_its_next_input() {
    let (test, fake) = harness(false).await;
    let url = parent(&test).await;
    test.client.post(format!("{url}/input")).json(&json!({"input":[{"type":"text","text":"Exec turn"}],"mode":"queue","nonInteractive":true,"clientName":"exec"})).send().await.unwrap().error_for_status().unwrap();
    idle(&test, &url).await;
    let child = spawn(&test, &url, "Inspect code").await;
    idle(&test, &child).await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert_eq!(test.read(&url).await.session.turns.len(), 2);
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client.post(format!("{url}/resume")).json(&json!({"env":{"HOME":test.root.path().join("home"),"CANTER_HOME":test.app.home,"PATH":"/usr/bin:/bin"}})).send().await.unwrap().error_for_status().unwrap();
    say(&test, &url, "Use the child result").await;
    let messages = fake.requests.lock().unwrap().last().unwrap()["messages"].to_string();
    assert!(
        messages.contains("[Sub-agent ")
            && messages.contains("completed")
            && messages.contains(child.rsplit('/').next().unwrap())
    );
    assert_eq!(test.read(&url).await.session.turns.len(), 3);
    test.stop().await;
    fake.stop().await;
}

#[tokio::test]
async fn close_agent_stops_its_background_command_and_keeps_its_final_reply() {
    use canter_core::{AgentCommand, AgentHost};
    let reply = |delta: serde_json::Value, finish: &str| {
        model_server::Reply::sse(format!(
            "data: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
        ))
    };
    let fake = model_server::Server::start(vec![
        reply(json!({"content":"Ready"}), "stop"),
        reply(json!({"tool_calls":[{"index":0,"id":"background","type":"function","function":{"name":"shell","arguments":"{\"command\":\"sleep 30\",\"background\":true}"}}]}), "tool_calls"),
        reply(json!({"content":"Started the command"}), "stop"),
    ]).await;
    let model = canter_model::ChatCompletions::new(
        &fake.base_url,
        canter_model::ApiKey::new(model_server::KEY),
    )
    .unwrap();
    let test = Harness::with_model(Arc::new(model)).await;
    let url = parent(&test).await;
    test.patch(&url, json!({"approvalPolicy":"allowAll"})).await;
    let child = spawn(&test, &url, "Run a background command").await;
    assert!(
        idle(&test, &child)
            .await
            .background_commands
            .iter()
            .any(|command| command.running)
    );
    let host = super::super::agent_sessions::Host(Arc::downgrade(&test.app));
    let result = host
        .execute(
            url.rsplit('/').next().unwrap().into(),
            AgentCommand::CloseAgent {
                id: child.rsplit('/').next().unwrap().into(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "closed");
    assert_eq!(result["result"], "Started the command");
    assert!(
        test.read(&child)
            .await
            .session
            .background_commands
            .iter()
            .all(|command| !command.running)
    );
    test.stop().await;
    fake.stop().await;
}
