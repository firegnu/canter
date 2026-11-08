use super::*;

const OWNER: &str = "http://localhost/v1/managedClients/test-owner";
const HEADER: &str = "x-canter-managed-client";

async fn lease(test: &Harness) -> reqwest::Response {
    let response = test
        .client
        .get(OWNER)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "managed lease must exist"
    );
    response
}

async fn claim(test: &Harness, url: &str) {
    test.client
        .post(format!("{url}/resume"))
        .header(HEADER, "test-owner")
        .json(&json!({"env": {}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

async fn closed(test: &Harness, url: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while test.read(url).await.session.status != SessionStatus::Closed {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("managed session must close");
    assert_eq!(
        test.read(url).await.session.close_reason,
        Some(SessionCloseReason::ClientRequest)
    );
}

#[tokio::test]
async fn managed_exit_closes_all_claimed_sessions_and_leaves_other_sessions() {
    let test = Harness::new().await;
    let connection = lease(&test).await;
    let first = test.create().await;
    let second = test.create().await;
    let other = test.create().await;
    claim(&test, &first).await;
    claim(&test, &second).await;
    let response = test.client.delete(OWNER).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    closed(&test, &first).await;
    closed(&test, &second).await;
    assert_eq!(test.read(&other).await.session.status, SessionStatus::Idle);
    assert_eq!(
        test.client.get(OWNER).send().await.unwrap().status(),
        StatusCode::CONFLICT
    );
    drop(connection);
    test.stop().await;
}

async fn subscribe(test: &Harness, url: &str, observe: bool) -> reqwest::Response {
    let id = url.rsplit('/').next().unwrap();
    test.client
        .get(format!(
            "http://localhost/v1/events?session={id}&observe={observe}"
        ))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
}

#[tokio::test]
async fn managed_disconnect_waits_ten_seconds_despite_ordinary_and_observer_connections() {
    let test = Harness::with_model(Arc::new(PausedModel(None))).await;
    let connection = lease(&test).await;
    let url = test.create().await;
    claim(&test, &url).await;
    let ordinary = subscribe(&test, &url, false).await;
    let observer = subscribe(&test, &url, true).await;
    test.client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":"keep working"}], "clientName":"test"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let disconnected = std::time::Instant::now();
    drop(connection);
    tokio::time::sleep(Duration::from_secs(9)).await;
    assert_ne!(test.read(&url).await.session.status, SessionStatus::Closed);
    closed(&test, &url).await;
    assert!(disconnected.elapsed() >= Duration::from_secs(10));
    assert_eq!(
        test.read(&url).await.session.turns[0].status,
        TurnStatus::Interrupted
    );
    drop((ordinary, observer));
    test.stop().await;
}

#[tokio::test]
async fn managed_reconnect_cancels_old_deadlines_and_only_last_connection_counts() {
    let test = Harness::new().await;
    let first = lease(&test).await;
    let url = test.create().await;
    claim(&test, &url).await;
    drop(first);
    tokio::time::sleep(Duration::from_secs(8)).await;
    let second = lease(&test).await;
    // Overlap and immediately disconnect an old connection, as a reconnect can.
    let third = lease(&test).await;
    drop(second);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(test.read(&url).await.session.status, SessionStatus::Idle);
    drop(third);
    tokio::time::sleep(Duration::from_secs(9)).await;
    assert_eq!(test.read(&url).await.session.status, SessionStatus::Idle);
    closed(&test, &url).await;
    test.stop().await;
}

#[tokio::test]
async fn managed_create_and_fork_claim_new_instances_without_claiming_fork_source() {
    let test = Harness::new().await;
    let connection = lease(&test).await;
    let source = test.create().await;
    let created: SessionCreateResponse = test.client.post("http://localhost/v1/sessions")
        .header(HEADER, "test-owner")
        .json(&json!({"cwd":test.root.path(), "env":{}, "model":"fake-model", "approvalPolicy":"alwaysAsk", "clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    let fork: SessionCreateResponse = test
        .client
        .post(format!("{source}/fork"))
        .header(HEADER, "test-owner")
        .json(&json!({"env":{}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    test.client
        .delete(OWNER)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for id in [&created.session.id, &fork.session.id] {
        closed(&test, &format!("http://localhost/v1/sessions/{id}")).await;
    }
    assert_eq!(test.read(&source).await.session.status, SessionStatus::Idle);
    drop(connection);
    test.stop().await;
}

#[tokio::test]
async fn managed_owner_cannot_close_a_later_unmanaged_resume_of_the_same_id() {
    let test = Harness::new().await;
    let connection = lease(&test).await;
    let url = test.create().await;
    claim(&test, &url).await;
    test.client
        .delete(&url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .post(format!("{url}/resume"))
        .json(&json!({"env":{}}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    test.client
        .delete(OWNER)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(test.read(&url).await.session.status, SessionStatus::Idle);
    drop(connection);
    test.stop().await;
}

struct ResumedChildModel(std::sync::atomic::AtomicUsize);

impl Model for ResumedChildModel {
    fn stream(&self, _: Request, cancel: CancellationToken) -> ModelStream<'_> {
        let request = self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(async_stream::stream! {
            match request {
                0 | 1 => {
                    yield Ok(ModelEvent::TextDelta { item_id: "ready".into(), delta: "Ready".into() });
                    yield Ok(ModelEvent::Finished(FinishReason::Stop));
                }
                2 => {
                    yield Ok(ModelEvent::ToolCall(canter_model::ToolCall {
                        id: "background".into(), name: "shell".into(),
                        arguments: json!({"command":"sleep 60", "background":true}).to_string(),
                    }));
                    yield Ok(ModelEvent::Finished(FinishReason::ToolCalls));
                }
                _ => {
                    yield Ok(ModelEvent::TextDelta { item_id: "working".into(), delta: "Child is still working".into() });
                    cancel.cancelled().await;
                }
            }
        })
    }
}

#[tokio::test]
async fn managed_stale_parent_does_not_interrupt_reopened_parents_child_work() {
    stale_parent_keeps_child_work(true).await;
}

#[tokio::test]
async fn managed_closed_parent_still_registered_does_not_interrupt_child_work() {
    stale_parent_keeps_child_work(false).await;
}

async fn stale_parent_keeps_child_work(reopen: bool) {
    let test = Harness::with_options(
        Arc::new(ResumedChildModel(Default::default())),
        CoreOptions {
            auto_wake_limit: 0,
            ..CoreOptions::default()
        },
    )
    .await;
    let connection = lease(&test).await;
    let parent = agents::parent(&test).await;
    test.patch(&parent, json!({"approvalPolicy":"allowAll"}))
        .await;
    claim(&test, &parent).await;
    let child = agents::spawn(&test, &parent, "Prepare work").await;
    agents::idle(&test, &child).await;
    let removing = (!reopen).then(|| {
        test.app
            .pauses
            .block(parent.rsplit('/').next().unwrap(), "close_remove")
    });
    let request = test.client.delete(&parent);
    let mut deleting = Some(tokio::spawn(async move { request.send().await.unwrap() }));
    if let Some(removing) = &removing {
        removing.reached().await;
    } else {
        deleting
            .take()
            .unwrap()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        test.client
            .post(format!("{parent}/resume"))
            .json(&json!({"env":{"HOME":test.root.path().join("home"),"PATH":"/usr/bin:/bin"}}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    test.client.post(format!("{child}/input"))
        .json(&json!({"input":[{"type":"text","text":"Continue the child task"}], "clientName":"ordinary"}))
        .send().await.unwrap().error_for_status().unwrap();
    let running = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let snapshot = test.read(&child).await.session;
            if snapshot.background_commands.iter().any(|command| command.running)
                && snapshot.turns.last().is_some_and(|turn| turn.items.iter().any(|item| {
                    matches!(item, Item::AgentMessage { text, .. } if text == "Child is still working")
                })) {
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.unwrap();
    assert_eq!(running.turns.last().unwrap().status, TurnStatus::InProgress);
    let finished = test
        .app
        .pauses
        .block(parent.rsplit('/').next().unwrap(), "managed_close_done");
    assert_eq!(
        test.client.delete(OWNER).send().await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    // Wait for the actual cleanup attempt, not a guessed scheduling delay.
    finished.reached().await;
    let after = test.read(&child).await.session;
    let parent_status = test.read(&parent).await.session.status;
    drop(finished);
    drop(removing);
    if let Some(deleting) = deleting {
        deleting.await.unwrap().error_for_status().unwrap();
    }
    drop(connection);
    test.stop().await;
    assert_eq!(
        parent_status,
        if reopen {
            SessionStatus::Idle
        } else {
            SessionStatus::Closed
        }
    );
    assert_eq!(
        after.turns.last().unwrap().id,
        running.turns.last().unwrap().id
    );
    assert_eq!(
        after.turns.last().unwrap().status,
        TurnStatus::InProgress,
        "the stale owner's cleanup must not interrupt the child's new turn"
    );
    assert_eq!(
        after.background_commands, running.background_commands,
        "the child background command must keep running"
    );
}

struct SpawnAfterReady(std::sync::atomic::AtomicUsize);

impl Model for SpawnAfterReady {
    fn stream(&self, _: Request, _: CancellationToken) -> ModelStream<'_> {
        let request = self.0.fetch_add(1, Ordering::Relaxed);
        Box::pin(futures_util::stream::iter(if request == 0 {
            vec![Ok(ModelEvent::Finished(FinishReason::Stop))]
        } else {
            vec![
                Ok(ModelEvent::ToolCall(canter_model::ToolCall {
                    id: "spawn".into(), name: "spawn_agent".into(),
                    arguments: json!({"task":"Inspect the code", "type":"general", "name":"Inspect Local Code", "depth":"medium"}).to_string(),
                })),
                Ok(ModelEvent::Finished(FinishReason::ToolCalls)),
            ]
        }))
    }
}

#[tokio::test]
async fn managed_close_finishes_when_an_agent_tool_waits_for_the_session_lock() {
    let test = Harness::with_model(Arc::new(SpawnAfterReady(Default::default()))).await;
    let connection = lease(&test).await;
    let parent = agents::parent(&test).await;
    claim(&test, &parent).await;
    let id = parent.rsplit('/').next().unwrap();
    let tool = test.app.pauses.block(id, "agent_execute");
    test.client.post(format!("{parent}/input"))
        .json(&json!({"input":[{"type":"text","text":"Start a sub-agent to inspect the code"}], "clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap();
    tool.reached().await;
    let closing = test.app.pauses.block(id, "managed_close_locked");
    assert_eq!(
        test.client.delete(OWNER).send().await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    closing.reached().await;
    let finished = test.app.pauses.block(id, "managed_close_done");
    // The tool passed its cancellation check; now the close owns its session lock.
    drop(tool);
    drop(closing);
    finished.reached().await;
    drop(finished);
    closed(&test, &parent).await;
    drop(connection);
    test.stop().await;
}

#[tokio::test]
async fn managed_cleanup_rejects_reconnect_and_new_turn_while_hook_is_running() {
    // Use the existing close path's slow notification as an observable cleanup barrier.
    let config = json!({"hooks":{"SessionEnd":[{"hooks":[{"type":"command", "command":"cat >/dev/null; touch closing; while [ ! -f release ]; do sleep 0.05; done", "timeout":10}]}]}});
    let options = CoreOptions {
        hooks: serde_json::from_value(config["hooks"].clone()).unwrap(),
        ..CoreOptions::default()
    };
    let test = Harness::with_options(
        Arc::new(PanicOnce {
            panicked: AtomicBool::new(true),
            reached: Notify::new(),
        }),
        options,
    )
    .await;
    let connection = lease(&test).await;
    let url = test.create().await;
    claim(&test, &url).await;
    let started = std::time::Instant::now();
    assert_eq!(
        test.client.delete(OWNER).send().await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    assert!(started.elapsed() < Duration::from_secs(1));
    tokio::time::timeout(Duration::from_secs(3), async {
        while !test.root.path().join("closing").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        test.client.get(OWNER).send().await.unwrap().status(),
        StatusCode::CONFLICT
    );
    let response = test
        .client
        .post(format!("{url}/input"))
        .json(&json!({"input":[{"type":"text","text":"too late"}],"clientName":"test"}))
        .send()
        .await
        .unwrap();
    assert!(!response.status().is_success());
    std::fs::write(test.root.path().join("release"), "").unwrap();
    closed(&test, &url).await;
    drop(connection);
    test.stop().await;
}

#[tokio::test]
async fn managed_exit_also_closes_an_opening_request_that_finishes_later() {
    let test = Harness::new().await;
    let connection = lease(&test).await;
    let block = test.app.pauses.block("*", "configure");
    let request = test.client.post("http://localhost/v1/sessions")
        .header(HEADER, "test-owner")
        .json(&json!({"cwd":test.root.path(), "env":{}, "model":"fake-model", "approvalPolicy":"alwaysAsk", "clientName":"test"}));
    let opening = tokio::spawn(async move { request.send().await.unwrap() });
    block.reached().await;
    assert_eq!(
        test.client.delete(OWNER).send().await.unwrap().status(),
        StatusCode::ACCEPTED
    );
    drop(block);
    assert_eq!(opening.await.unwrap().status(), StatusCode::CONFLICT);
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
    closed(
        &test,
        &format!("http://localhost/v1/sessions/{}", list.data[0].id),
    )
    .await;
    drop(connection);
    test.stop().await;
}

async fn hooks_once(earlier_client_exit: bool) {
    let options = CoreOptions {
        hooks: serde_json::from_value(
            json!({"SessionEnd":[{"hooks":[{"type":"command","command":"cat >> ends.jsonl"}]}]}),
        )
        .unwrap(),
        ..CoreOptions::default()
    };
    let test = Harness::with_options(
        Arc::new(PanicOnce {
            panicked: AtomicBool::new(true),
            reached: Notify::new(),
        }),
        options,
    )
    .await;
    let connection = lease(&test).await;
    let url = test.create().await;
    claim(&test, &url).await;
    drop(subscribe(&test, &url, false).await);
    if earlier_client_exit {
        tokio::time::timeout(Duration::from_secs(15), async {
            while !test.root.path().join("ends.jsonl").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        test.client
            .delete(OWNER)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        drop(connection);
    } else {
        drop(connection);
        tokio::time::sleep(Duration::from_secs(9)).await;
    }
    closed(&test, &url).await;
    let text = std::fs::read_to_string(test.root.path().join("ends.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "SessionEnd must run only once: {text}");
    if earlier_client_exit {
        assert_eq!(lines[0]["reason"], "client_exit");
    } else {
        assert!(matches!(
            lines[0]["reason"].as_str(),
            Some("client_exit" | "client_request")
        ));
    }
    test.stop().await;
}

#[tokio::test]
async fn managed_cleanup_does_not_repeat_an_earlier_client_exit_hook() {
    hooks_once(true).await;
}

#[tokio::test]
async fn managed_and_ordinary_disconnect_together_emit_one_end_hook() {
    hooks_once(false).await;
}
