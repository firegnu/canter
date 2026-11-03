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
