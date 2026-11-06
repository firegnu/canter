use super::*;

async fn configured() -> Fixture {
    Fixture::new(json!({
        "headers":{"x-test-session":"{session_id}"},
        "providers":{"local":{
            "base_url":"http://127.0.0.1:9", "api_key":"test-only-key",
            "models":{
                "selected":{"context_window":200000,
                    "supported_reasoning_efforts":["low","medium","high","max"],
                    "default_reasoning_effort":"medium"},
                "configured":{"context_window":200000,
                    "supported_reasoning_efforts":["low","high"],
                    "default_reasoning_effort":"low"}
            }
        }}
    }))
    .await
}

async fn patch(fixture: &Fixture, url: &str, params: Value) {
    fixture
        .client
        .patch(url)
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
}

async fn create(fixture: &Fixture, model: &str) -> String {
    let response: SessionCreateResponse = fixture.client.post("http://localhost/v1/sessions")
        .json(&json!({"cwd":fixture.root.path(),"model":model,
            "env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},
            "approvalPolicy":"allowAll","clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    format!("http://localhost/v1/sessions/{}", response.session.id)
}

async fn assert_default(fixture: &Fixture, model: &str, effort: &str) {
    let url = create(fixture, "").await;
    let settings = fixture.snapshot(&url).await.session.settings;
    assert_eq!(settings.model, model);
    assert_eq!(settings.reasoning_effort.as_str(), effort);
}

async fn remember(fixture: &Fixture) -> String {
    let url = fixture.create().await;
    patch(
        fixture,
        &url,
        json!({"model":"local/selected","reasoningEffort":"max","rememberSelection":true}),
    )
    .await;
    assert_default(fixture, "local/selected", "max").await;
    url
}

async fn stop(fixture: &mut Fixture) {
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(fixture.child.id().unwrap() as i32),
        nix::sys::signal::Signal::SIGTERM,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_secs(15), fixture.child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

async fn start(fixture: &mut Fixture) {
    let log = fs::File::create(fixture.root.path().join("server.log")).unwrap();
    fixture.child = tokio::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "reload_tests::server_process", "--nocapture"])
        .env_clear()
        .env("HOME", fixture.root.path().join("home"))
        .env("CANTER_HOME", fixture.root.path().join("state"))
        .env("CANTER_RELOAD_TEST", "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let connection = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                fixture.child.try_wait().unwrap().is_none(),
                "{}",
                fs::read_to_string(fixture.root.path().join("server.log")).unwrap()
            );
            if let Ok(Some(connection)) = discover(&fixture.root.path().join("state")).await {
                break connection;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    fixture.client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(connection.registration.socket_path)
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();
}

#[tokio::test]
async fn model_selection_becomes_the_new_session_default_only_after_an_explicit_change() {
    let fixture = configured().await;
    let config = fs::read(fixture.path()).unwrap();
    let first = fixture.create().await;
    // Switching away from an unsupported effort must remember the final, corrected effort.
    patch(&fixture, &first, json!({"reasoningEffort":"none"})).await;
    patch(
        &fixture,
        &first,
        json!({"model":"local/selected","rememberSelection":true}),
    )
    .await;
    assert_default(&fixture, "local/selected", "medium").await;
    let second = create(&fixture, "fake").await;
    patch(
        &fixture,
        &second,
        json!({"model":"local/configured","rememberSelection":true}),
    )
    .await;
    assert_default(&fixture, "local/configured", "high").await;
    // A no-op selection, an unrelated edit, a rejected choice and a legacy caller do not remember.
    patch(
        &fixture,
        &first,
        json!({"model":"local/selected","rememberSelection":true}),
    )
    .await;
    patch(
        &fixture,
        &first,
        json!({"name":"renamed","rememberSelection":true}),
    )
    .await;
    let rejected = fixture
        .client
        .patch(&first)
        .json(&json!({"reasoningEffort":"none","rememberSelection":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    patch(&fixture, &first, json!({"model":"fake"})).await;
    patch(
        &fixture,
        &first,
        json!({"model":"local/selected","rememberSelection":false}),
    )
    .await;
    assert_default(&fixture, "local/configured", "high").await;
    assert_eq!(fs::read(fixture.path()).unwrap(), config);
    fixture.shutdown().await;
}

#[tokio::test]
async fn thinking_selection_remembers_both_model_and_effort() {
    let fixture = configured().await;
    let url = create(&fixture, "local/selected").await;
    patch(
        &fixture,
        &url,
        json!({"reasoningEffort":"max","rememberSelection":true}),
    )
    .await;
    assert_default(&fixture, "local/selected", "max").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn explicit_startup_and_resume_models_do_not_change_the_remembered_selection() {
    let fixture = configured().await;
    let remembered = remember(&fixture).await;
    for model in ["fake", "local/selected"] {
        let url = create(&fixture, model).await;
        let settings = fixture.snapshot(&url).await.session.settings;
        assert_eq!(settings.model, model);
        assert_eq!(
            settings.reasoning_effort.as_str(),
            if model == "fake" { "high" } else { "medium" }
        );
        // --resume -m, -c -m and exec --resume -m use this same unmarked PATCH.
        patch(&fixture, &remembered, json!({"model":model})).await;
        assert_default(&fixture, "local/selected", "max").await;
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn child_agent_models_do_not_change_the_remembered_selection() {
    let mut fixture = configured().await;
    remember(&fixture).await;
    let parent = create(&fixture, "fake").await;
    fixture.input(&parent).await;
    fixture.request().await.tool(
        "spawn_agent",
        json!({"task":"Inspect only","type":"explorer"}),
    );
    let first = fixture.request().await;
    let next = fixture.request().await;
    let parent_id = parent.rsplit('/').next().unwrap();
    let (parent_request, child) = if first.headers["x-test-session"] == parent_id {
        (first, next)
    } else {
        (next, first)
    };
    let child_url = format!(
        "http://localhost/v1/sessions/{}",
        child.headers["x-test-session"].to_str().unwrap()
    );
    assert_eq!(child.body["model"], "fake");
    child.stop();
    fixture.idle(&child_url).await;
    parent_request.stop();
    fixture.idle(&parent).await;
    patch(
        &fixture,
        &child_url,
        json!({"reasoningEffort":"low","rememberSelection":true}),
    )
    .await;
    assert_default(&fixture, "local/selected", "max").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn removed_remembered_model_falls_back_to_configured_model_and_effort() {
    let mut fixture = configured().await;
    remember(&fixture).await;
    fixture.config["providers"]["local"]["models"]
        .as_object_mut()
        .unwrap()
        .remove("selected");
    fixture.applied("removed-model").await;
    assert_default(&fixture, "fake", "high").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn unsupported_remembered_effort_falls_back_to_that_models_default() {
    let mut fixture = configured().await;
    remember(&fixture).await;
    let selected = &mut fixture.config["providers"]["local"]["models"]["selected"];
    selected["supported_reasoning_efforts"] = json!(["low", "medium"]);
    selected["default_reasoning_effort"] = json!("low");
    fixture.applied("removed-effort").await;
    assert_default(&fixture, "local/selected", "low").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn hot_reloading_configured_model_clears_the_selection_until_the_next_user_choice() {
    let mut fixture = configured().await;
    let url = remember(&fixture).await;
    fixture.config["model"] = json!("local/configured");
    fixture.applied("changed-default").await;
    assert_default(&fixture, "local/configured", "low").await;
    fixture.config["model"] = json!("fake");
    fixture.applied("restored-default").await;
    assert_default(&fixture, "fake", "high").await;
    patch(
        &fixture,
        &url,
        json!({"reasoningEffort":"low","rememberSelection":true}),
    )
    .await;
    assert_default(&fixture, "local/selected", "low").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn restarting_with_a_changed_configured_model_discards_the_old_selection() {
    let mut fixture = configured().await;
    remember(&fixture).await;
    stop(&mut fixture).await;
    fixture.config["model"] = json!("local/configured");
    fixture.save();
    start(&mut fixture).await;
    assert_default(&fixture, "local/configured", "low").await;
    stop(&mut fixture).await;
    fixture.config["model"] = json!("fake");
    fixture.save();
    start(&mut fixture).await;
    assert_default(&fixture, "fake", "high").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn broken_selection_file_does_not_block_startup_or_session_creation() {
    let mut fixture = configured().await;
    remember(&fixture).await;
    stop(&mut fixture).await;
    let path = fixture.root.path().join("state/last_model.json");
    fs::write(&path, b"{broken").unwrap();
    start(&mut fixture).await;
    assert_default(&fixture, "fake", "high").await;
    assert!(
        fs::read_to_string(fixture.root.path().join("server.log"))
            .unwrap()
            .contains("last_model.json")
    );
    stop(&mut fixture).await;
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    start(&mut fixture).await;
    assert_default(&fixture, "fake", "high").await;
    assert!(
        fs::read_to_string(fixture.root.path().join("server.log"))
            .unwrap()
            .contains("last_model.json")
    );
    fixture.shutdown().await;
}

#[tokio::test]
async fn models_is_default_matches_the_new_session_selection() {
    let fixture = configured().await;
    let url = fixture.create().await;
    patch(
        &fixture,
        &url,
        json!({"model":"local/selected","rememberSelection":true}),
    )
    .await;
    let models: canter_protocol::ModelListResponse = fixture
        .client
        .get("http://localhost/v1/models")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let defaults: Vec<_> = models
        .data
        .iter()
        .filter(|model| model.is_default)
        .map(|model| model.id.as_str())
        .collect();
    assert_eq!(defaults, ["local/selected"]);
    assert_default(&fixture, "local/selected", "high").await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn server_restart_preserves_selection_and_resuming_old_sessions_does_not_replace_it() {
    let mut fixture = configured().await;
    let old = create(&fixture, "fake").await;
    remember(&fixture).await;
    stop(&mut fixture).await;
    start(&mut fixture).await;
    assert_default(&fixture, "local/selected", "max").await;
    let resumed: SessionReadResponse = fixture.client.post(format!("{old}/resume"))
        .json(&json!({"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},"clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
    assert_eq!(resumed.session.settings.model, "fake");
    assert_default(&fixture, "local/selected", "max").await;
    fixture.shutdown().await;
}
