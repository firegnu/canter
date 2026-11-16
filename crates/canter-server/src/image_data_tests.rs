use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};

fn inline(bytes: &[u8]) -> serde_json::Value {
    json!({"type":"image", "url":format!("data:image/png;base64,{}", STANDARD.encode(bytes))})
}

#[tokio::test]
async fn image_data_rejects_invalid_urls_formats_and_limits_without_input_events() {
    let test = BtwTest::new().await;
    let session = test.create().await;
    let before = test.app.store.events_after(&session, 0).unwrap();
    let history = test
        .app
        .store
        .model_history::<serde_json::Value>(&session)
        .unwrap();
    let oversized = vec![0; MAX_IMAGE_BYTES as usize + 1];
    let mut cases = vec![
        (
            json!([{"type":"image","url":"https://example.com/image.png"}]),
            "base64 data URL",
        ),
        (
            json!([{"type":"image","url":"data:image/png;base64,%%%"}]),
            "invalid base64",
        ),
        (
            json!([{"type":"image","url":"data:image/jpeg;base64,iVBORw0KGgo="}]),
            "does not match",
        ),
        (
            json!([{"type":"image","url":"data:text/plain;base64,aGVsbG8="}]),
            "JPEG, PNG, GIF or WebP",
        ),
        (
            json!([{"type":"image","url":"data:image/png;base64,aGVsbG8="}]),
            "JPEG, PNG, GIF or WebP",
        ),
        (json!([inline(&oversized)]), "image exceeds the 8 MiB limit"),
    ];
    let mut large = b"\x89PNG\r\n\x1a\n".to_vec();
    large.resize(MAX_IMAGE_BYTES as usize, 0);
    let mut combined = vec![inline(&large); 8];
    combined.push(inline(b"\x89PNG\r\n\x1a\n"));
    cases.push((json!(combined), "combined images exceed the 64 MiB limit"));
    for (input, message) in cases {
        let response = test
            .client
            .post(format!("{}/input", test.url(&session)))
            .json(&json!({"input":input,"clientName":"test"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: serde_json::Value = response.json().await.unwrap();
        assert!(
            error["message"].as_str().unwrap().contains(message),
            "{error}"
        );
        assert_eq!(test.app.store.events_after(&session, 0).unwrap(), before);
    }
    assert_eq!(
        test.app
            .store
            .model_history::<serde_json::Value>(&session)
            .unwrap(),
        history
    );
    assert!(
        std::fs::read_dir(test.app.store.session_dir(&session).unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("image-"))
    );
    test.finish().await;
}

#[tokio::test]
async fn image_data_http_body_is_still_bounded_on_both_routes() {
    let test = BtwTest::new().await;
    let session = test.create().await;
    for url in [
        "http://localhost/v1/sessions".into(),
        format!("{}/input", test.url(&session)),
    ] {
        let limit = MAX_INPUT_IMAGE_BYTES.div_ceil(3) * 4 + 2 * 1024 * 1024;
        let body = " ".repeat(limit as usize + 1);
        let response = test
            .client
            .post(url)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error = response.text().await.unwrap();
        assert!(
            error.contains("image request body exceeds the limit"),
            "{error}"
        );
    }
    test.finish().await;
}

#[tokio::test]
async fn image_data_is_imported_on_create_and_input_above_the_old_body_limit() {
    let test = BtwTest::new().await;
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.resize(2 * 1024 * 1024, 0);
    let url = format!("data:image/png;base64,{}", STANDARD.encode(&bytes));
    let parts = json!([{"type":"image", "url":url, "name":"shot.png"}]);
    let response = test
        .client
        .post("http://localhost/v1/sessions")
        .json(&json!({
            "cwd":test.root.path(), "env":{}, "model":"deepseek-flash",
            "approvalPolicy":"allowAll", "clientName":"test", "input":parts,
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
    let session = test.app.store.list().unwrap().pop().unwrap().id;
    test.wait_status(&session, SessionStatus::Idle).await;
    let response = test
        .client
        .post(format!("{}/input", test.url(&session)))
        .json(&json!({"input":parts,"clientName":"test"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        response.text().await.unwrap()
    );
    let snapshot = test.wait_status(&session, SessionStatus::Idle).await;
    assert_eq!(snapshot.turns.len(), 2);
    for turn in snapshot.turns {
        let UserInput::LocalImage { path, name } = &turn.input[0] else {
            panic!("events must retain localImage copies");
        };
        assert_eq!(name.as_deref(), Some("shot.png"));
        assert!(!path.contains('/'));
        assert_eq!(
            std::fs::read(test.app.store.image_path(&session, path).unwrap()).unwrap(),
            bytes
        );
    }
    let events = serde_json::to_string(&test.app.store.events_after(&session, 0).unwrap()).unwrap();
    assert!(!events.contains("data:image"));
    let info: serde_json::Value = test
        .client
        .get("http://localhost/v1/info")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        info["features"]
            .as_array()
            .unwrap()
            .contains(&json!("imageData"))
    );
    // Another JSON endpoint keeps its original 2 MiB limit.
    for (padding, status) in [
        (0, StatusCode::OK),
        (2 * 1024 * 1024, StatusCode::BAD_REQUEST),
    ] {
        let response = test
            .client
            .patch(test.url(&session))
            .json(&json!({"name":"valid name", "padding":"x".repeat(padding)}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    test.finish().await;
}
