use super::*;

async fn configured() -> Fixture {
    Fixture::new(json!({
        "providers":{"chatgpt":{"api":"responses","auth":"chatgpt","models":{
            "solver":{"context_window":200000,
                "supported_reasoning_efforts":["low","medium","high","xhigh","max"],
                "default_reasoning_effort":"medium"}
        }}}
    }))
    .await
}

async fn resume(fixture: &Fixture, url: &str) -> SessionReadResponse {
    fixture.client.post(format!("{url}/resume"))
        .json(&json!({"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},"clientName":"test"}))
        .send().await.unwrap().error_for_status().unwrap().json().await.unwrap()
}

#[tokio::test]
async fn resuming_saved_reasoning_keeps_the_correction_warning_after_the_snapshot() {
    let fixture = configured().await;
    let store = canter_store::Store::new(fixture.root.path().join("state/sessions")).unwrap();
    let settings = serde_json::from_value(json!({"cwd":fixture.root.path(),"model":"chatgpt/solver","reasoningEffort":"none","approvalPolicy":"allowAll"})).unwrap();
    drop(store.create("old-effort", settings, 0).unwrap());
    let url = "http://localhost/v1/sessions/old-effort";
    let response = resume(&fixture, url).await;
    assert_eq!(
        response.session.settings.reasoning_effort,
        canter_protocol::ReasoningEffort::Medium
    );
    let mut warnings = fixture
        .warnings_after(url, response.session.last_event_seq)
        .await;
    assert_eq!(
        warnings.next().await,
        "Thinking effort changed from none to medium for chatgpt/solver."
    );
    drop(warnings);
    fixture.shutdown().await;
}

#[tokio::test]
async fn resuming_an_open_session_reconciles_reloaded_reasoning() {
    let mut fixture = configured().await;
    let url = fixture.create().await;
    fixture
        .client
        .patch(&url)
        .json(&json!({"model":"chatgpt/solver","reasoningEffort":"xhigh"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.config["providers"]["chatgpt"]["models"]["solver"]["supported_reasoning_efforts"] =
        json!(["medium", "high"]);
    fixture.applied("reasoning-reloaded").await;
    let response = resume(&fixture, &url).await;
    assert_eq!(
        response.session.settings.reasoning_effort,
        canter_protocol::ReasoningEffort::Medium
    );
    let mut warnings = fixture
        .warnings_after(&url, response.session.last_event_seq)
        .await;
    assert_eq!(
        warnings.next().await,
        "Thinking effort changed from xhigh to medium for chatgpt/solver."
    );
    drop(warnings);
    fixture.shutdown().await;
}

#[tokio::test]
async fn resuming_during_a_turn_keeps_reasoning_frozen_until_the_next_turn() {
    let mut fixture = Fixture::new(json!({})).await;
    fixture.config["providers"] = json!({"local":{
        "api":"chat_completions","base_url":fixture.config["base_url"],
        "api_key":"test-only-key","models":{"solver":{
            "context_window":200000,"supported_reasoning_efforts":["high","max"],
            "default_reasoning_effort":"high"
        }}
    }});
    fixture.applied("reasoning-initial").await;
    let url = fixture.create().await;
    fixture
        .client
        .patch(&url)
        .json(&json!({"model":"local/solver","reasoningEffort":"max"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    fixture.input(&url).await;
    let running = fixture.request().await;
    assert_eq!(running.body["reasoning_effort"], "max");
    fixture.config["providers"]["local"]["models"]["solver"]["supported_reasoning_efforts"] =
        json!(["high"]);
    fixture.applied("reasoning-next-turn").await;
    let response = resume(&fixture, &url).await;
    assert_eq!(
        response.session.settings.reasoning_effort,
        canter_protocol::ReasoningEffort::Max
    );
    running.tool(
        "update_plan",
        json!({"plan":[{"step":"Checked","status":"completed"}]}),
    );
    let continuation = fixture.request().await;
    assert_eq!(continuation.body["reasoning_effort"], "max");
    continuation.stop();
    fixture.idle(&url).await;
    let mut warnings = fixture.warnings_after(&url, response.event_seq).await;
    fixture.input(&url).await;
    let next = fixture.request().await;
    assert_eq!(next.body["reasoning_effort"], "high");
    assert_eq!(
        warnings.next().await,
        "Thinking effort changed from max to high for local/solver."
    );
    next.stop();
    fixture.idle(&url).await;
    drop(warnings);
    fixture.shutdown().await;
}

#[tokio::test]
async fn model_reasoning_metadata_and_creation_use_configured_defaults() {
    let fixture = configured().await;
    let models: Value = fixture
        .client
        .get("http://localhost/v1/models")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let data = models["data"].as_array().unwrap();
    let solver = data
        .iter()
        .find(|model| model["id"] == "chatgpt/solver")
        .unwrap();
    assert_eq!(
        solver["supportedReasoningEfforts"],
        json!(["low", "medium", "high", "xhigh", "max"])
    );
    assert_eq!(solver["defaultReasoningEffort"], "medium");
    let legacy = data.iter().find(|model| model["id"] == "fake").unwrap();
    assert_eq!(
        legacy["supportedReasoningEfforts"],
        json!(["none", "low", "high", "max"])
    );
    assert_eq!(legacy["defaultReasoningEffort"], "high");
    for (model, effort) in [("chatgpt/solver", "medium"), ("fake", "high")] {
        let response: SessionCreateResponse = fixture.client.post("http://localhost/v1/sessions")
            .json(&json!({"cwd":fixture.root.path(),"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},"model":model,"approvalPolicy":"allowAll","clientName":"test"}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(response.session.settings.reasoning_effort.as_str(), effort);
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn unsupported_reasoning_is_rejected_on_create_and_update_without_changes() {
    let fixture = configured().await;
    for (model, effort) in [
        ("chatgpt/solver", "none"),
        ("fake", "xhigh"),
        ("chatgpt/solver", "ultra"),
    ] {
        let response = fixture.client.post("http://localhost/v1/sessions")
            .json(&json!({"cwd":fixture.root.path(),"env":{"HOME":fixture.root.path().join("home"),"CANTER_HOME":fixture.root.path().join("state")},"model":model,"reasoningEffort":effort,"approvalPolicy":"allowAll","clientName":"test"}))
            .send().await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        let error: Value = response.json().await.unwrap();
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("Supported efforts:")
        );
    }
    let url = fixture.create().await;
    let before = fixture.snapshot(&url).await;
    for patch in [
        json!({"reasoningEffort":"xhigh","name":"must not change"}),
        json!({"model":"chatgpt/solver","reasoningEffort":"none","name":"must not change"}),
        json!({"reasoningEffort":"ultra"}),
    ] {
        let response = fixture
            .client
            .patch(&url)
            .json(&patch)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        let error: Value = response.json().await.unwrap();
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("Supported efforts:")
        );
        assert_eq!(
            fixture.snapshot(&url).await.session.settings,
            before.session.settings
        );
    }
    fixture.shutdown().await;
}

#[tokio::test]
async fn switching_model_resets_only_unsupported_reasoning_and_reports_the_change() {
    let fixture = configured().await;
    let url = fixture.create().await;
    let mut warnings = fixture.warnings(&url).await;
    for (patch, expected, notice) in [
        (json!({"reasoningEffort":"none"}), "none", None),
        (
            json!({"model":"chatgpt/solver"}),
            "medium",
            Some("Thinking effort changed from none to medium for chatgpt/solver."),
        ),
        (json!({"reasoningEffort":"xhigh"}), "xhigh", None),
        (
            json!({"model":"fake"}),
            "high",
            Some("Thinking effort changed from xhigh to high for fake."),
        ),
        (json!({"model":"chatgpt/solver"}), "high", None),
        (json!({"model":"fake","reasoningEffort":"low"}), "low", None),
    ] {
        fixture
            .client
            .patch(&url)
            .json(&patch)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        assert_eq!(
            fixture
                .snapshot(&url)
                .await
                .session
                .settings
                .reasoning_effort
                .as_str(),
            expected
        );
        if let Some(notice) = notice {
            assert_eq!(warnings.next().await, notice);
        }
    }
    assert!(warnings.receiver.try_recv().is_err());
    drop(warnings);
    fixture.shutdown().await;
}
