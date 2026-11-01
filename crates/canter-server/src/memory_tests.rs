use super::*;

#[tokio::test]
async fn memories_list_global_files_through_the_session_protocol() {
    let test = Harness::new().await;
    let directory = test.app.home.join("memory");
    std::fs::create_dir(&directory).unwrap();
    std::fs::write(
        directory.join("testing.md"),
        "# Testing preference\nUse isolated fixtures.\n\nNever use live credentials.\n",
    )
    .unwrap();
    let url = test.create().await;
    let response = test
        .client
        .get(format!("{url}/memories"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let listed: serde_json::Value = response.json().await.unwrap();
    assert_eq!(
        listed["data"],
        json!([{
            "scope":"global", "file":"testing.md", "name":"Testing preference",
            "description":"Use isolated fixtures.",
            "path":directory.canonicalize().unwrap().join("testing.md")
        }])
    );
    test.stop().await;
}

#[tokio::test]
async fn memory_delete_uses_scope_and_filename_and_restricted_sessions_cannot_manage_memory() {
    let test = Harness::new().await;
    let url = test.create().await;
    let file = test.app.home.join("memory/preference.md");
    std::fs::write(&file, "# Preference\nKeep this until forgotten.\n\nBody.\n").unwrap();
    let response = test
        .client
        .post(format!("{url}/memories/delete"))
        .json(&json!({"scope":"global","file":"preference.md"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!file.exists());
    let listed: serde_json::Value = test
        .client
        .get(format!("{url}/memories"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["data"], json!([]));
    std::fs::write(&file, "# Preference\nKeep this until forgotten.\n\nBody.\n").unwrap();
    test.client
        .post(format!("{url}/resume"))
        .json(&json!({"env":{},"restrictedMode":true}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    for request in [
        test.client.get(format!("{url}/memories")),
        test.client
            .post(format!("{url}/memories/delete"))
            .json(&json!({"scope":"global","file":"preference.md"})),
    ] {
        assert_eq!(
            request.send().await.unwrap().status(),
            StatusCode::BAD_REQUEST
        );
    }
    assert!(file.exists());
    test.stop().await;
}
