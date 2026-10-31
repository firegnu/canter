use super::*;
use serde_json::Value;

const RULES_URL: &str = "http://localhost/v1/commandRules";
const DELETE_URL: &str = "http://localhost/v1/commandRules/delete";

#[tokio::test]
async fn command_rules_delete_preserves_broken_files_and_does_not_create_missing_files() {
    let test = BtwTest::new().await;
    let path = test.app.home.join("rules.json");
    let rule = json!({"pattern":["cargo","test"],"decision":"allow"});
    let response = test
        .client
        .post(DELETE_URL)
        .json(&rule)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["removed"], 0);
    assert!(!path.exists());
    for broken in [
        "unfinished JSON",
        r#"{"rules":[{"pattern":[],"decision":"allow"}]}"#,
    ] {
        std::fs::write(&path, broken).unwrap();
        let response = test
            .client
            .post(DELETE_URL)
            .json(&rule)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            response
                .json::<ErrorResponse>()
                .await
                .unwrap()
                .message
                .contains("rules.json")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let response = test
        .client
        .post(DELETE_URL)
        .json(&rule)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(path.is_dir());
    test.finish().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn command_rules_delete_and_permanent_approvals_preserve_concurrent_writes() {
    use canter_tools::PermissionGate;
    use std::os::unix::fs::PermissionsExt;
    let test = BtwTest::new().await;
    let path = test.app.home.join("rules.json");
    let rule = json!({"pattern":["cargo","test"],"decision":"allow"});
    // Use the same public entry point as "Always allow" while HTTP deletions run.
    for _ in 0..12 {
        std::fs::write(&path, json!({"rules":[rule]}).to_string()).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let writer = tokio::task::spawn_blocking({
            let barrier = barrier.clone();
            let path = path.clone();
            move || {
                barrier.wait();
                PermissionGate::default()
                    .with_rules_file(path)
                    .save_rules(&[
                        vec!["git".into(), "status".into()],
                        vec!["cargo".into(), "build".into()],
                    ])
                    .unwrap();
            }
        });
        barrier.wait();
        let response = test
            .client
            .post(DELETE_URL)
            .json(&rule)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["removed"], 1);
        writer.await.unwrap();
        let listed = test
            .client
            .get(RULES_URL)
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap();
        assert_eq!(
            listed["data"],
            json!([
                {"pattern":["git","status"],"decision":"allow"},
                {"pattern":["cargo","build"],"decision":"allow"}
            ])
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    test.finish().await;
}

#[tokio::test]
async fn command_rules_do_not_require_changes_to_old_session_requests() {
    let test = BtwTest::new().await;
    let response = test
        .client
        .post("http://localhost/v1/sessions")
        .json(&json!({
            "cwd":test.root.path(),
            "env":{"HOME":test.root.path().join("home"),"CANTER_HOME":test.app.home},
            "model":"deepseek-flash","approvalPolicy":"alwaysAsk","clientName":"old-client"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let created = response.json::<SessionCreateResponse>().await.unwrap();
    let info = test
        .client
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(info["protocolVersion"], 1);
    let read = test
        .client
        .get(test.url(&created.session.id))
        .send()
        .await
        .unwrap();
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(
        read.json::<SessionReadResponse>().await.unwrap().session,
        created.session
    );
    assert_eq!(
        test.client
            .delete(test.url(&created.session.id))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    test.finish().await;
}

#[tokio::test]
async fn command_rules_delete_matches_content_and_changes_next_permission_check() {
    use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, Tools};
    use std::os::unix::fs::PermissionsExt;

    let test = BtwTest::new().await;
    let path = test.app.home.join("rules.json");
    let allow = json!({"pattern":["cargo","test"],"decision":"allow"});
    let prompt = json!({"pattern":["cargo","test"],"decision":"prompt"});
    let keep = json!({"pattern":["cargo","publish"],"decision":"forbidden"});
    let file_rule = json!({"pattern":".env","decision":"forbidden"});
    std::fs::write(
        &path,
        json!({"rules":[allow, prompt, keep, allow],"fileRules":[file_rule, file_rule]})
            .to_string(),
    )
    .unwrap();
    let gate = PermissionGate::default().with_rules_file(path.clone());
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            test.root.path().into(),
            BTreeMap::from([
                (
                    "HOME".into(),
                    test.root.path().join("home").to_string_lossy().into_owned(),
                ),
                (
                    "CANTER_HOME".into(),
                    test.app.home.to_string_lossy().into_owned(),
                ),
            ]),
            vec![],
        )
        .unwrap(),
    ));
    let call = tools
        .prepare(
            Tool::Shell,
            "test",
            r#"{"command":"cargo test"}"#,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    let response = test
        .client
        .post(DELETE_URL)
        .json(&prompt)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["removed"], 1);
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));

    // Another approval adds a rule after the list was read; deletion still uses content.
    gate.save_rules(&[vec!["git".into(), "status".into()]])
        .unwrap();
    let response = test
        .client
        .post(DELETE_URL)
        .json(&allow)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let deleted = response.json::<Value>().await.unwrap();
    assert_eq!(deleted["removed"], 2);
    assert!(!deleted["message"].as_str().unwrap().is_empty());
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    let listed = test
        .client
        .get(RULES_URL)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(
        listed["data"],
        json!([keep, {"pattern":["git","status"],"decision":"allow"}])
    );
    assert_eq!(listed["fileRules"], json!([file_rule, file_rule]));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let before = std::fs::read(&path).unwrap();
    let response = test
        .client
        .post(DELETE_URL)
        .json(&allow)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let absent = response.json::<Value>().await.unwrap();
    assert_eq!(absent["removed"], 0);
    assert!(
        absent["message"]
            .as_str()
            .unwrap()
            .contains("no longer exists")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    std::fs::write(test.root.path().join(".env"), "fake").unwrap();
    let read = tools
        .prepare(
            Tool::Read,
            "read",
            r#"{"path":".env"}"#,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        gate.evaluate(&read, PermissionMode::AllowAll),
        PermissionCheck::Forbidden { .. }
    ));
    for removed in [2, 0] {
        let response = test
            .client
            .post("http://localhost/v1/fileRules/delete")
            .json(&file_rule)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["removed"], removed);
    }
    assert!(matches!(
        gate.evaluate(&read, PermissionMode::AllowAll),
        PermissionCheck::Execute(_)
    ));
    let after = test
        .client
        .get(RULES_URL)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(after["data"], listed["data"]);
    assert!(after.get("fileRules").is_none());
    test.finish().await;
}

#[tokio::test]
async fn command_rules_list_reads_the_server_file_and_reports_broken_files() {
    let test = BtwTest::new().await;
    let path = test.app.home.join("rules.json");
    let response = test.client.get(RULES_URL).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"path":path,"data":[]})
    );

    let rules = json!([
        {"pattern":["cargo","test"],"decision":"allow"},
        {"pattern":["git","push"],"decision":"prompt"},
        {"pattern":["cargo","publish"],"decision":"forbidden"}
    ]);
    std::fs::write(&path, json!({"rules":rules}).to_string()).unwrap();
    let response = test.client.get(RULES_URL).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"path":path,"data":rules})
    );

    std::fs::write(&path, "unfinished JSON").unwrap();
    let response = test.client.get(RULES_URL).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error = response.json::<ErrorResponse>().await.unwrap();
    assert!(error.message.contains("invalid command rules JSON"));
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let response = test.client.get(RULES_URL).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        response
            .json::<ErrorResponse>()
            .await
            .unwrap()
            .message
            .contains("rules.json")
    );
    test.finish().await;
}
