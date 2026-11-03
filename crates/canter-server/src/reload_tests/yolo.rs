use super::*;

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
