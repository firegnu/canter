use super::*;

async fn sandbox_session(fixture: &Fixture) -> String {
    let work = fixture.root.path().join("work");
    fs::create_dir_all(&work).unwrap();
    let created: SessionCreateResponse = fixture.client.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":work,"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},"model":"fake","approvalPolicy":"allowAll","clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    format!("http://localhost/v1/sessions/{}", created.session.id)
}

#[tokio::test]
async fn enabling_sandbox_leaves_existing_background_command_in_its_original_scope() {
    let mut fixture = Fixture::new(json!({"sandbox":false})).await;
    let url = sandbox_session(&fixture).await;
    fixture.input(&url).await;
    fixture.request().await.tool("shell", json!({"command":"while [ ! -f release ]; do sleep 0.01; done; printf background > ../background-output","background":true}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert!(
        fixture
            .snapshot(&url)
            .await
            .session
            .background_commands
            .iter()
            .any(|command| command.running)
    );
    fixture
        .client
        .post(format!("{url}/sandbox"))
        .json(&json!({"enabled":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.input(&url).await;
    fixture.request().await.tool(
        "shell",
        json!({"command":"printf new > ../foreground-output"}),
    );
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert!(!fixture.root.path().join("foreground-output").exists());
    fs::write(fixture.root.path().join("work/release"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while fixture
            .snapshot(&url)
            .await
            .session
            .background_commands
            .iter()
            .any(|command| command.running)
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("background-output")).unwrap(),
        "background"
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn sandbox_on_overrides_disabled_config_and_resume_clears_the_choice() {
    let mut fixture = Fixture::new(json!({"sandbox":false})).await;
    let url = sandbox_session(&fixture).await;
    let config = fs::read(fixture.path()).unwrap();
    fixture
        .client
        .post(format!("{url}/sandbox"))
        .json(&json!({"enabled":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.input(&url).await;
    fixture
        .request()
        .await
        .tool("shell", json!({"command":"printf blocked > ../on-output"}));
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(
        fixture
            .snapshot(&url)
            .await
            .session
            .settings
            .sandbox_enabled,
        Some(true)
    );
    assert!(!fixture.root.path().join("on-output").exists());
    // Configuration reload cannot undo a forced-on selection.
    fixture.applied("still-forced-on").await;
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(
        fixture
            .snapshot(&url)
            .await
            .session
            .settings
            .sandbox_enabled,
        Some(true)
    );
    for close in [false, true] {
        if close {
            fixture
                .client
                .delete(&url)
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap();
        }
        let resumed: SessionReadResponse = fixture.client.post(format!("{url}/resume"))
            .json(&json!({"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},"clientName":"reopened"}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(resumed.session.settings.sandbox_enabled, Some(false));
        assert!(resumed.session.sandbox_pending.is_none());
        fixture.input(&url).await;
        fixture
            .request()
            .await
            .tool("shell", json!({"command":"printf resumed > ../on-output"}));
        fixture.request().await.stop();
        fixture.idle(&url).await;
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("on-output")).unwrap(),
            "resumed"
        );
        // Reopen while a new choice is pending, too.
        fixture
            .client
            .post(format!("{url}/sandbox"))
            .json(&json!({"enabled":true}))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
    }
    // Only the test's explicit configuration reload changed this file.
    assert_eq!(
        serde_json::from_slice::<Value>(&config).unwrap()["sandbox"],
        fixture.config["sandbox"]
    );
    let id = url.rsplit('/').next().unwrap();
    let events = fs::read_to_string(
        fixture
            .root
            .path()
            .join(format!("state/sessions/{id}/events.jsonl")),
    )
    .unwrap();
    assert!(!events.contains("sandboxPending"));
    assert!(!events.contains("session.sandboxUpdated"));
    fixture.shutdown().await;
}

#[tokio::test]
async fn sandbox_off_requires_confirmation_and_broadcasts_pending_to_both_terminals() {
    let mut fixture = Fixture::new(json!({"sandbox":true})).await;
    let url = sandbox_session(&fixture).await;
    for params in [
        json!({"enabled":false}),
        json!({"enabled":false,"confirmed":false}),
    ] {
        let response = fixture
            .client
            .post(format!("{url}/sandbox"))
            .json(&params)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(
            fixture
                .snapshot(&url)
                .await
                .session
                .settings
                .sandbox_enabled,
            Some(true)
        );
        assert!(
            fixture
                .snapshot(&url)
                .await
                .session
                .sandbox_pending
                .is_none()
        );
    }
    fixture
        .client
        .patch(&url)
        .json(&json!({"approvalPolicy":"alwaysAsk"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let id = url.rsplit('/').next().unwrap();
    let after = fixture.snapshot(&url).await.event_seq;
    let endpoint = format!("http://localhost/v1/events?session={id}&after={after}");
    let mut first = fixture
        .client
        .get(&endpoint)
        .send()
        .await
        .unwrap()
        .bytes_stream()
        .eventsource();
    let mut second = fixture
        .client
        .get(&endpoint)
        .send()
        .await
        .unwrap()
        .bytes_stream()
        .eventsource();
    fixture
        .client
        .post(format!("{url}/sandbox"))
        .json(&json!({"enabled":false,"confirmed":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for stream in [&mut first, &mut second] {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let message = stream.next().await.unwrap().unwrap();
                let event: Event = serde_json::from_str(&message.data).unwrap();
                if matches!(
                    event.payload,
                    EventPayload::SessionSandboxUpdated {
                        pending_enabled: Some(false)
                    }
                ) {
                    assert_eq!(event.seq, 0);
                    assert!(message.id.is_empty());
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    let scope: Value = fixture
        .client
        .get(format!("{url}/sandbox"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(scope["enabled"], true);
    assert_eq!(scope["pendingEnabled"], false);
    // A reconnect after the durable cursor receives the current live choice.
    let mut reconnect = fixture
        .client
        .get(&endpoint)
        .send()
        .await
        .unwrap()
        .bytes_stream()
        .eventsource();
    let message = tokio::time::timeout(Duration::from_secs(5), reconnect.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let event: Event = serde_json::from_str(&message.data).unwrap();
    assert!(matches!(
        event.payload,
        EventPayload::SessionSandboxUpdated {
            pending_enabled: Some(false)
        }
    ));
    fixture.input(&url).await;
    fixture.request().await.stop();
    fixture.idle(&url).await;
    assert_eq!(
        fixture
            .snapshot(&url)
            .await
            .session
            .settings
            .approval_policy,
        canter_protocol::PermissionMode::AlwaysAsk
    );
    drop((first, second, reconnect));
    fixture.shutdown().await;
}

#[tokio::test]
async fn sandbox_switch_waits_for_next_turn_and_leaves_other_running_session_unchanged() {
    let mut fixture = Fixture::new(json!({"sandbox":true})).await;
    let first = sandbox_session(&fixture).await;
    let second = sandbox_session(&fixture).await;
    let original_config = fs::read(fixture.path()).unwrap();
    fixture.input(&first).await;
    let first_request = fixture.request().await;
    fixture.input(&second).await;
    let second_request = fixture.request().await;
    fixture
        .client
        .post(format!("{first}/sandbox"))
        .json(&json!({"enabled":false,"confirmed":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let snapshot: Value = fixture
        .client
        .get(&first)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["session"]["settings"]["sandboxEnabled"], true);
    assert_eq!(snapshot["session"]["sandboxPending"], false);
    assert_eq!(
        fixture
            .snapshot(&second)
            .await
            .session
            .settings
            .sandbox_enabled,
        Some(true)
    );
    first_request.tool("shell", json!({"command":"printf first > ../first-output"}));
    fixture.request().await.stop();
    fixture.idle(&first).await;
    second_request.tool(
        "shell",
        json!({"command":"printf second > ../second-output"}),
    );
    fixture.request().await.stop();
    fixture.idle(&second).await;
    assert!(!fixture.root.path().join("first-output").exists());
    assert!(!fixture.root.path().join("second-output").exists());
    fixture.input(&first).await;
    fixture
        .request()
        .await
        .tool("shell", json!({"command":"printf next > ../first-output"}));
    fixture.request().await.stop();
    fixture.idle(&first).await;
    assert_eq!(
        fs::read_to_string(fixture.root.path().join("first-output")).unwrap(),
        "next"
    );
    let snapshot: Value = fixture
        .client
        .get(&first)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(snapshot["session"]["settings"]["sandboxEnabled"], false);
    assert!(
        snapshot["session"]
            .get("sandboxPending")
            .is_none_or(Value::is_null)
    );
    assert_eq!(fs::read(fixture.path()).unwrap(), original_config);
    fixture.shutdown().await;
}

#[tokio::test]
async fn sandbox_reload_preserves_yolo_openings_and_updates_ordinary_sessions() {
    let mut fixture = Fixture::new(json!({"sandbox":true})).await;
    let ordinary = fixture.create().await;
    let work = fixture.root.path().join("work");
    fs::create_dir(&work).unwrap();
    let created: SessionCreateResponse = fixture.client.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":work,"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},"model":"fake","approvalPolicy":"allowAll","clientName":"test","sandboxDisabled":true}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(created.session.settings.sandbox_enabled, Some(false));
    let yolo = format!("http://localhost/v1/sessions/{}", created.session.id);
    for (index, enabled) in [true, false, true].into_iter().enumerate() {
        fixture.config["sandbox"] = json!(enabled);
        fixture.applied(&format!("sandbox-revision-{index}")).await;
        fixture.input(&yolo).await;
        fixture.request().await.tool(
            "shell",
            json!({"command":format!("printf {index} > ../outside-project")}),
        );
        fixture.request().await.stop();
        fixture.idle(&yolo).await;
        assert_eq!(
            fs::read_to_string(fixture.root.path().join("outside-project")).unwrap(),
            index.to_string()
        );
        assert_eq!(
            fixture
                .snapshot(&yolo)
                .await
                .session
                .settings
                .sandbox_enabled,
            Some(false)
        );
        fixture.input(&ordinary).await;
        fixture.request().await.stop();
        fixture.idle(&ordinary).await;
        assert_eq!(
            fixture
                .snapshot(&ordinary)
                .await
                .session
                .settings
                .sandbox_enabled,
            Some(enabled)
        );
        for (url, expected) in [(&yolo, false), (&ordinary, enabled)] {
            let scope: canter_protocol::SessionSandboxResponse = fixture
                .client
                .get(format!("{url}/sandbox"))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(scope.enabled, expected);
        }
    }
    fixture.shutdown().await;
}
