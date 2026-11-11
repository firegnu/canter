use super::*;
use base64::Engine;

pub(super) fn large_png() -> Vec<u8> {
    let pixels = image::RgbImage::from_fn(2048, 1024, |x, y| {
        image::Rgb([(x / 8) as u8, (y / 8) as u8, ((x + y) / 16) as u8])
    });
    let mut bytes = std::io::Cursor::new(Vec::new());
    pixels
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

pub(super) async fn downscaled_attachments(fixture: &Fixture, id: &str) -> Vec<String> {
    let snapshot: Value = fixture
        .request(Method::GET, &format!("/v1/sessions/{id}"), json!({}))
        .await
        .json()
        .await
        .unwrap();
    snapshot["session"]["turns"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item["contentItems"].as_array())
        .flatten()
        .map(|attachment| {
            let saved = fixture
                .root
                .path()
                .join("state/sessions")
                .join(id)
                .join(attachment["path"].as_str().unwrap());
            let bytes = fs::read(&saved).unwrap();
            let image = image::load_from_memory(&bytes).unwrap();
            assert_eq!((image.width(), image.height()), (1568, 784));
            assert_eq!(attachment["sizeBytes"], bytes.len());
            assert_eq!(attachment["mimeType"], "image/png");
            assert_eq!(
                fs::metadata(saved).unwrap().permissions().mode() & 0o777,
                0o600
            );
            format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        })
        .collect()
}

// A synthetic landscape page, rendered by the same poppler path as ordinary PDFs.
fn large_pdf() -> Vec<u8> {
    let drawing = (0..80)
        .map(|i| {
            format!(
                "{} 0.4 0.8 rg {} {} 400 3 re f\n",
                i as f32 / 80.0,
                i * 4,
                i * 6
            )
        })
        .collect::<String>();
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Count 1 /Kids [3 0 R] >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 960 480] /Resources << >> /Contents 4 0 R >>"
            .to_owned(),
        format!(
            "<< /Length {} >>\nstream\n{drawing}endstream",
            drawing.len()
        ),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend(
        format!("trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    bytes
}

#[tokio::test]
async fn image_downscale_read_and_pdf_use_saved_bytes_and_copy_sizes() {
    if std::process::Command::new("pdftoppm")
        .arg("-v")
        .output()
        .is_err()
    {
        eprintln!("Skipping PDF rendering: pdftoppm is not installed");
        return;
    }
    let calls = [
        json!({"path":"picture.png"}),
        json!({"path":"chart.pdf","page":1}),
    ];
    let replies: Vec<_> = calls
        .iter()
        .enumerate()
        .map(|(i, args)| {
            json!({
                "index":i,"id":format!("read-{i}"),"type":"function",
                "function":{"name":"read","arguments":args.to_string()}
            })
        })
        .collect();
    let reply = format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({
            "choices":[{"index":0,"delta":{"tool_calls":replies},"finish_reason":"tool_calls"}]
        })
    );
    let path = std::env::var("PATH").unwrap();
    let mut fixture = Fixture::start_with_mcp_env(
        vec![reply, text_reply(), text_reply()],
        "deepseek-flash",
        None,
        json!({}),
        json!({}),
        &[("PATH", &path)],
    )
    .await;
    fs::write(fixture.root.path().join("work/picture.png"), large_png()).unwrap();
    fs::write(fixture.root.path().join("work/chart.pdf"), large_pdf()).unwrap();
    let created = create(&fixture).await;
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, created.event_seq).await;
    let expected = downscaled_attachments(&fixture, &created.session.id).await;
    assert_eq!(expected.len(), 2, "both read and PDF must attach an image");
    let snapshot: SessionReadResponse = fixture
        .request(
            Method::GET,
            &format!("/v1/sessions/{}", created.session.id),
            json!({}),
        )
        .await
        .json()
        .await
        .unwrap();
    fixture.input(&created.session.id).await;
    complete(&fixture, &created.session.id, snapshot.event_seq).await;
    fixture.stop().await;
    let requests = &fixture.script.lock().unwrap().requests;
    for request in &requests[1..] {
        let actual: Vec<_> = request["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|message| message["content"].as_array())
            .flatten()
            .filter_map(|part| part["image_url"]["url"].as_str())
            .collect();
        assert_eq!(actual, expected);
    }
}

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
                "cwd":fixture.root.path().join("work"),
                "env":{"HOME":fixture.root.path().join("home")},"model":"deepseek-flash",
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
