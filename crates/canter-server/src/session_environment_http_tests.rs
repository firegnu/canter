use super::*;
use serde_json::Value;

#[tokio::test]
async fn info_and_doctor_describe_the_default_session_environment() {
    let test = BtwTest::new().await;
    let info: serde_json::Value = test
        .client
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["home"], json!(test.root.path().join("home")));
    assert!(
        info["instance"]
            .as_str()
            .is_some_and(|value| !value.is_empty())
    );
    assert!(
        info["features"]
            .as_array()
            .unwrap()
            .contains(&json!("optionalSessionEnv"))
    );
    let report: DoctorResponse = test
        .client
        .get(format!(
            "http://localhost/v1/doctor?clientVersion={}&clientProtocolVersion=1",
            crate::VERSION
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let environment = report
        .data
        .iter()
        .find(|check| check.id == "session.environment")
        .unwrap();
    assert_eq!(environment.status, DoctorStatus::Ok);
    assert!(
        environment
            .reason
            .as_deref()
            .unwrap()
            .contains("login shell")
    );
    test.finish().await;
}

#[tokio::test]
async fn omitted_environment_protects_the_server_home() {
    let test = BtwTest::build(None, true).await;
    let response = test
        .client
        .post("http://localhost/v1/sessions")
        .json(&json!({
            "cwd": test.root.path(), "model":"deepseek-flash",
            "approvalPolicy":"allowAll", "clientName":"test"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    let created: SessionCreateResponse = response.json().await.unwrap();
    let sandbox: serde_json::Value = test
        .client
        .get(format!("{}/sandbox", test.url(&created.session.id)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for name in [".ssh", ".aws", ".gnupg", ".canter"] {
        let path = test.root.path().join("home").join(name);
        assert!(
            sandbox["noAccess"]
                .as_array()
                .unwrap()
                .contains(&json!(path)),
            "{sandbox}"
        );
    }
    test.finish().await;
}

async fn post(test: &BtwTest, url: &str, body: Value) -> SessionCreateResponse {
    test.client
        .post(url)
        .json(&body)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn assert_home(test: &BtwTest, id: &str, expected: Option<(&std::path::Path, &str)>) {
    let context: Value = test
        .client
        .get(format!("{}/context", test.url(id)))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let global = context["prefix"]
        .as_array()
        .unwrap()
        .iter()
        .find(|section| section["kind"] == "globalInstructions");
    let skills: Value = test
        .client
        .get(format!("{}/skills", test.url(id)))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    match expected {
        Some((home, name)) => {
            assert_eq!(
                global.unwrap()["path"],
                json!(home.join(".canter/AGENTS.md"))
            );
            assert_eq!(skills["data"].as_array().unwrap().len(), 1);
            assert_eq!(skills["data"][0]["name"], name);
            assert_eq!(
                skills["data"][0]["path"],
                json!(home.join(format!(".agents/skills/{name}/SKILL.md")))
            );
        }
        None => {
            assert!(global.is_none());
            assert_eq!(skills["data"], json!([]));
        }
    }
}

#[tokio::test]
async fn session_opening_uses_default_or_explicit_environment_and_children_inherit_it() {
    let test = BtwTest::new().await;
    let server_home = test.root.path().join("home");
    let client_home = test.root.path().join("client-home");
    for (home, name) in [
        (&server_home, "server-skill"),
        (&client_home, "client-skill"),
    ] {
        std::fs::create_dir_all(home.join(".canter")).unwrap();
        std::fs::write(
            home.join(".canter/AGENTS.md"),
            format!("Global instructions for {name}"),
        )
        .unwrap();
        let skill = home.join(format!(".agents/skills/{name}"));
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(
            skill.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: test skill\n---\nTest body"),
        )
        .unwrap();
    }
    test.client
        .post("http://localhost/v1/folderTrust")
        .json(&json!({"cwd":test.root.path()}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for (environment, expected) in [
        (None, Some((server_home.as_path(), "server-skill"))),
        (
            Some(json!({"HOME":client_home})),
            Some((client_home.as_path(), "client-skill")),
        ),
        (Some(json!({})), None),
    ] {
        let mut create = json!({"cwd":test.root.path(),"model":"deepseek-flash","approvalPolicy":"allowAll","clientName":"test"});
        let mut opening = json!({});
        if let Some(environment) = environment {
            create["env"] = environment.clone();
            opening["env"] = environment;
        }
        let created = post(&test, "http://localhost/v1/sessions", create).await;
        let id = created.session.id;
        assert_home(&test, &id, expected).await;
        let forked = post(&test, &format!("{}/fork", test.url(&id)), opening.clone()).await;
        assert_home(&test, &forked.session.id, expected).await;
        // Reattaching to an already open session never replaces its environment.
        post(
            &test,
            &format!("{}/resume", test.url(&id)),
            json!({"env":{"HOME":test.root.path().join("unused")}}),
        )
        .await;
        assert_home(&test, &id, expected).await;
        test.client
            .delete(test.url(&id))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap();
        post(&test, &format!("{}/resume", test.url(&id)), opening).await;
        assert_home(&test, &id, expected).await;
        test.input(&id, "environment-main").await;
        test.wait_status(&id, SessionStatus::Idle).await;
        let child = test
            .app
            .spawn_agent(id, "test child".into())
            .await
            .unwrap_or_else(|error| panic!("{}", error.0.message));
        test.wait_status(&child.session.id, SessionStatus::Idle)
            .await;
        assert_home(&test, &child.session.id, expected).await;
    }
    test.finish().await;
}
