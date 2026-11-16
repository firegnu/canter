use super::*;

#[tokio::test]
async fn workspace_summary_uses_the_session_repository_and_environment() {
    let test = BtwTest::new().await;
    let repo = test.root.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let git = |args: &[&str]| {
        let result = std::process::Command::new("git")
            .current_dir(&repo)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", test.root.path().join("home"))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(args)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    };
    git(&["init", "-q", "-b", "workspace-test"]);
    std::fs::write(repo.join("file.txt"), "one\ntwo\n").unwrap();
    git(&["add", "."]);
    let response = test.client.post("http://localhost/v1/sessions").json(&json!({
        "cwd":repo, "env":{"HOME":test.root.path().join("home"),"PATH":"/usr/bin:/bin"},
        "model":"deepseek-flash", "reasoningEffort":"low", "approvalPolicy":"allowAll", "clientName":"test"
    })).send().await.unwrap().error_for_status().unwrap();
    let created: SessionCreateResponse = response.json().await.unwrap();
    let response = test
        .client
        .get(format!(
            "{}/workspace/summary",
            test.url(&created.session.id)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let summary: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        summary,
        json!({"branch":"workspace-test", "changedLines":[2,0]})
    );
    std::fs::write(repo.join("file.txt"), "three\n").unwrap();
    std::fs::write(repo.join("new.txt"), "untracked content").unwrap();
    let response = test
        .client
        .get(format!("{}/workspace/diff", test.url(&created.session.id)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let diff: serde_json::Value = response.json().await.unwrap();
    assert!(
        diff["staged"]["text"]
            .as_str()
            .unwrap()
            .contains("+one\n+two")
    );
    assert!(
        diff["unstaged"]["text"]
            .as_str()
            .unwrap()
            .contains("+three")
    );
    assert_eq!(diff["untracked"]["text"], "new.txt\n");
    let response = test
        .client
        .get(format!(
            "{}/workspace/status",
            test.url(&created.session.id)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let status: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        status,
        json!({"type":"repository","branch":"workspace-test","entries":[["AM","file.txt"],["??","new.txt"]]})
    );
    let response = test
        .client
        .get(format!(
            "{}/workspace/review?clientName=test",
            test.url(&created.session.id)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let review: SessionInputParams = response.json().await.unwrap();
    assert_eq!(
        review.review.as_deref(),
        Some("Review: uncommitted changes")
    );
    assert_eq!(review.mode, InputMode::Queue);
    let UserInput::Text { text } = &review.input[0] else {
        panic!("review text")
    };
    assert!(text.contains("STAGED_DIFF") && text.contains("+one\n+two") && text.contains("+three"));
    assert!(text.contains("new.txt") && !text.contains("untracked content"));
    test.finish().await;
}
