use super::*;

async fn create(fixture: &Fixture, environment: Option<Value>) -> String {
    let mut params = json!({"cwd":fixture.root.path(),"model":"","approvalPolicy":"allowAll","clientName":"test"});
    if let Some(environment) = environment {
        params["env"] = environment;
    }
    let created: SessionCreateResponse = fixture
        .client
        .post("http://localhost/v1/sessions")
        .json(&params)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    format!("http://localhost/v1/sessions/{}", created.session.id)
}

async fn command_value(fixture: &mut Fixture, url: &str, expected: &str) {
    fixture.input(url).await;
    fixture.request().await.tool(
        "shell",
        json!({"command":"printf '%s' \"${CANTER_ENV_TEST_VALUE-unset}\""}),
    );
    let request = fixture.request().await;
    let result = request.body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|message| message["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap();
    assert!(result.contains(expected), "{result}");
    request.stop();
    fixture.idle(url).await;
}

#[tokio::test]
async fn default_environment_config_reloads_for_new_sessions_only() {
    let mut fixture =
        Fixture::new(json!({"session_env":{"CANTER_ENV_TEST_VALUE":"p2-295-first"}})).await;
    fixture.trust(fixture.root.path()).await;
    let info: Value = fixture
        .client
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let registration: Value =
        serde_json::from_slice(&fs::read(fixture.root.path().join("state/server.json")).unwrap())
            .unwrap();
    assert_eq!(info["instance"], registration["instance"]);
    assert_eq!(
        info["home"],
        json!(fixture.root.path().join("state/test-session-home"))
    );
    let first = create(&fixture, None).await;
    command_value(&mut fixture, &first, "p2-295-first").await;
    fixture.config["session_env"]["CANTER_ENV_TEST_VALUE"] = json!("p2-295-second");
    fixture.applied("environment-reloaded").await;
    for (environment, expected) in [
        (None, "p2-295-second"),
        (Some(json!({})), "unset"),
        (
            Some(json!({"CANTER_ENV_TEST_VALUE":"p2-295-client"})),
            "p2-295-client",
        ),
    ] {
        let url = create(&fixture, environment).await;
        command_value(&mut fixture, &url, expected).await;
    }
    command_value(&mut fixture, &first, "p2-295-first").await;
    fixture.shutdown().await;
}
