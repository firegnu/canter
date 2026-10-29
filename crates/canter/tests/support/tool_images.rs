use super::*;

const PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 4, 0,
    0, 0, 181, 28, 12, 2, 0, 0, 0, 11, 73, 68, 65, 84, 120, 218, 99, 100, 248, 15, 0, 1, 5, 1, 1,
    39, 24, 227, 102, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];
const PNG_URL: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";

fn read_reply(paths: &[&str]) -> String {
    let calls: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(i, path)| {
            json!({
                "index":i,"id":format!("read-{i}"),"type":"function",
                "function":{"name":"read","arguments":json!({"path":path}).to_string()}
            })
        })
        .collect();
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}]})
    )
}

async fn create(fixture: &Fixture) -> SessionCreateResponse {
    let response = fixture
        .request(
            Method::POST,
            "/v1/sessions",
            json!({
                "cwd":fixture.root.path().join("work"),"env":{},"model":"deepseek-flash",
                "approvalPolicy":"allowAll","clientName":"test"
            }),
        )
        .await;
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

async fn complete(fixture: &Fixture, id: &str, after: u64) {
    fixture
        .events_until(id, after, |event| {
            matches!(event.payload, EventPayload::TurnCompleted { .. })
        })
        .await;
}

#[tokio::test]
async fn read_images_follow_the_complete_tool_batch_in_a_user_message() {
    let mut fixture = Fixture::start_with_models(
        vec![
            read_reply(&["picture.bin", "note.txt"]),
            text_reply(),
            text_reply(),
        ],
        "deepseek-flash",
        Some(&["deepseek-flash", "deepseek-v4-pro"]),
    )
    .await;
    fs::write(fixture.root.path().join("work/picture.bin"), PNG).unwrap();
    fs::write(fixture.root.path().join("work/note.txt"), "plain text\n").unwrap();
    let created = create(&fixture).await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    let snapshot: Value = fixture
        .request(
            Method::GET,
            &format!("/v1/sessions/{}", created.session.id),
            json!({}),
        )
        .await
        .json()
        .await
        .unwrap();
    let attachment = &snapshot["session"]["turns"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["type"] == "fileRead")
        .unwrap()["contentItems"][0];
    assert_eq!(attachment["name"], "picture.bin");
    assert_eq!(attachment["mimeType"], "image/png");
    assert_eq!(attachment["sizeBytes"], PNG.len());
    let saved = fixture
        .root
        .path()
        .join("state/sessions")
        .join(&created.session.id)
        .join(attachment["path"].as_str().unwrap());
    assert_eq!(fs::read(&saved).unwrap(), PNG);
    assert_eq!(
        fs::metadata(saved).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let response = fixture
        .request(
            Method::PATCH,
            &format!("/v1/sessions/{}", created.session.id),
            json!({"model":"deepseek-v4-pro"}),
        )
        .await;
    assert_eq!(response.status(), 200);
    fixture.input(&created.session.id).await;
    complete(
        &fixture,
        &created.session.id,
        snapshot["session"]["lastEventSeq"].as_u64().unwrap(),
    )
    .await;
    fixture.stop().await;
    let requests = &fixture.script.lock().unwrap().requests;
    let messages = requests[1]["messages"].as_array().unwrap();
    let first_tool = messages.iter().position(|m| m["role"] == "tool").unwrap();
    assert_eq!(
        messages[first_tool]["content"],
        "Image \"picture.bin\" is attached in the next user message after this tool-result batch."
    );
    assert_eq!(messages[first_tool + 1]["content"], "1: plain text\n");
    assert_eq!(messages[first_tool + 1]["role"], "tool");
    assert_eq!(messages[first_tool + 2]["role"], "user");
    assert!(
        messages[first_tool + 2]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part == &json!({"type":"image_url","image_url":{"url":PNG_URL}}))
    );
    let switched = requests[2]["messages"].to_string();
    assert!(switched.contains("[image omitted: picture.bin]"));
    assert!(!switched.contains("image_url"));
}

#[tokio::test]
async fn read_rejects_oversized_images_and_other_binary_files_without_attaching_them() {
    let mut fixture = Fixture::start_with_models(
        vec![read_reply(&["large.png", "binary.dat"]), text_reply()],
        "deepseek-flash",
        None,
    )
    .await;
    let mut large = PNG.to_vec();
    large.resize(8 * 1024 * 1024 + 1, 0);
    fs::write(fixture.root.path().join("work/large.png"), large).unwrap();
    fs::write(fixture.root.path().join("work/binary.dat"), b"\0binary").unwrap();
    let created = create(&fixture).await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    fixture.stop().await;
    let requests = &fixture.script.lock().unwrap().requests;
    let messages = requests[1]["messages"].as_array().unwrap();
    let results: Vec<_> = messages.iter().filter(|m| m["role"] == "tool").collect();
    assert_eq!(results[0]["content"], "image exceeds the 8 MiB limit");
    assert_eq!(
        results[1]["content"],
        "binary file: read supports UTF-8 text only"
    );
    assert!(messages.iter().all(|m| m["content"].is_string()));
}

#[tokio::test]
async fn text_only_read_keeps_the_original_model_messages_byte_for_byte() {
    let mut fixture = Fixture::start_with_models(
        vec![read_reply(&["note.txt"]), text_reply()],
        "deepseek-flash",
        None,
    )
    .await;
    fs::write(fixture.root.path().join("work/note.txt"), "first\nsecond\n").unwrap();
    let created = create(&fixture).await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    fixture.stop().await;
    let requests = &fixture.script.lock().unwrap().requests;
    let mut expected = requests[0]["messages"].as_array().unwrap().clone();
    expected.extend([
        json!({"role":"assistant","content":"","tool_calls":[{"id":"read-0","type":"function","function":{"name":"read","arguments":"{\"path\":\"note.txt\"}"}}]}),
        json!({"role":"tool","tool_call_id":"read-0","content":"1: first\n2: second\n"}),
    ]);
    assert_eq!(
        serde_json::to_string(&requests[1]["messages"]).unwrap(),
        serde_json::to_string(&expected).unwrap()
    );
}
