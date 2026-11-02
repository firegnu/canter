use super::*;

#[allow(dead_code)] // Shared fake server also records request timing for retry tests.
#[path = "../../canter-model/tests/support/mod.rs"]
mod model_server;

async fn harness(hold_child: bool) -> (Harness, model_server::Server) {
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

async fn parent(test: &Harness) -> String {
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

async fn idle(test: &Harness, url: &str) -> SessionSnapshot {
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

// P2-178 will invoke this same server entry point from its tool host. This phase
// deliberately adds no model tool or HTTP spawn endpoint.
async fn spawn(test: &Harness, parent: &str, task: &str) -> String {
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
