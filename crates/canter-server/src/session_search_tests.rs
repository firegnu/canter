use super::*;

fn fixture(
    test: &Harness,
    id: &str,
    cwd: &str,
    items: Vec<serde_json::Value>,
    child: bool,
    time: i64,
) {
    let mut settings = SessionSnapshot::empty(id).settings;
    settings.cwd = cwd.into();
    let mut writer = if child {
        test.app
            .store
            .create_child(
                id,
                settings,
                SessionParent {
                    agent_type: Default::default(),
                    session_id: "user".into(),
                    turn_id: "t".into(),
                    task: "search test".into(),
                    depth: None,
                },
                time,
            )
            .unwrap()
    } else {
        test.app.store.create(id, settings, time).unwrap()
    };
    writer.append(serde_json::from_value(json!({
        "type":"turn.started", "turn":{"id":"t","items":[],"status":"inProgress","error":null,"input":[],"clientName":"test","usage":null}
    })).unwrap(), time).unwrap();
    for item in items {
        writer
            .append(
                serde_json::from_value(json!({"type":"item.completed","turnId":"t","item":item}))
                    .unwrap(),
                time,
            )
            .unwrap();
    }
}

fn user(text: &str) -> serde_json::Value {
    json!({"type":"userMessage","id":"u","content":[{"type":"text","text":text}]})
}

fn agent(text: &str) -> serde_json::Value {
    json!({"type":"agentMessage","id":"a","text":text})
}

#[tokio::test]
async fn session_search_matches_only_user_and_agent_messages_in_live_parent_sessions() {
    let test = Harness::new().await;
    fixture(
        &test,
        "user",
        "/work",
        vec![user("Sandbox 默认\n怎么开？")],
        false,
        10,
    );
    fixture(
        &test,
        "agent",
        "/work",
        vec![agent("macOS 默认用 SANDBOX")],
        false,
        20,
    );
    fixture(
        &test,
        "tool",
        "/work",
        vec![
            json!({"type":"commandExecution","id":"c","command":"echo","cwd":"/work","status":"completed","aggregatedOutput":"sandbox 默认","exitCode":0,"durationMs":1}),
        ],
        false,
        30,
    );
    fixture(
        &test,
        "thought",
        "/work",
        vec![json!({"type":"reasoning","id":"r","summary":["sandbox 默认"],"content":[]})],
        false,
        40,
    );
    fixture(
        &test,
        "child",
        "/work",
        vec![user("sandbox 默认")],
        true,
        50,
    );
    fixture(
        &test,
        "trash",
        "/work",
        vec![user("sandbox 默认")],
        false,
        60,
    );
    std::fs::create_dir(test.app.home.join("trash")).unwrap();
    std::fs::rename(
        test.app.store.session_dir("trash").unwrap(),
        test.app.home.join("trash/trash"),
    )
    .unwrap();
    fixture(
        &test,
        "elsewhere",
        "/elsewhere",
        vec![agent("默认 sandbox")],
        false,
        70,
    );
    fixture(
        &test,
        "separate",
        "/work",
        vec![user("sandbox"), agent("默认")],
        false,
        80,
    );
    for (cwd, expected) in [
        (Some("/work"), vec!["agent", "user"]),
        (None, vec!["elsewhere", "agent", "user"]),
    ] {
        let mut params = vec![("query", "默认 sAnDbOx")];
        if let Some(cwd) = cwd {
            params.push(("cwd", cwd));
        }
        let mut url = reqwest::Url::parse("http://localhost/v1/sessions/search").unwrap();
        url.query_pairs_mut().extend_pairs(params);
        let response = test.client.get(url).send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "content search endpoint must exist"
        );
        let body: serde_json::Value = response.json().await.unwrap();
        let rows = body["data"].as_array().unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r["session"]["id"].as_str().unwrap())
                .collect::<Vec<_>>(),
            expected
        );
        let last = rows.last().unwrap();
        assert_eq!(last["snippet"]["source"], "userMessage");
        assert_eq!(last["snippet"]["text"], "Sandbox 默认 怎么开？");
        assert_eq!(rows[0]["snippet"]["source"], "agentMessage");
    }
    for (query, expected) in [("", 6), ("not-found", 0)] {
        let response: serde_json::Value = test
            .client
            .get(format!("http://localhost/v1/sessions/search?query={query}"))
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["data"].as_array().unwrap().len(), expected);
    }
    test.stop().await;
}

/// The task's single manual timing run; excluded from routine package tests.
#[tokio::test]
#[ignore = "manual P2-227 timing with 200 synthetic sessions"]
async fn time_session_search_on_200_delta_heavy_sessions() {
    use std::io::Write;
    let test = Harness::new().await;
    let delta = "x".repeat(1024);
    let mut total_bytes = 0;
    for number in 0..200 {
        let id = format!("synthetic-{number}");
        fixture(
            &test,
            &id,
            "/work",
            vec![user("A synthetic request")],
            false,
            1,
        );
        let path = test
            .app
            .store
            .session_dir(&id)
            .unwrap()
            .join("events.jsonl");
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let mut writer = std::io::BufWriter::new(file);
        writeln!(writer, "{}", json!({"seq":4,"sessionId":id,"timestamp":1,"type":"item.started","turnId":"t","item":agent("")})).unwrap();
        for seq in 5..3077 {
            writeln!(writer, "{{\"seq\":{seq},\"sessionId\":\"{id}\",\"timestamp\":1,\"type\":\"item.delta\",\"turnId\":\"t\",\"itemId\":\"a\",\"delta\":{{\"type\":\"agentMessage\",\"delta\":\"{delta}\"}}}}").unwrap();
        }
        writeln!(writer, "{}", json!({"seq":3077,"sessionId":id,"timestamp":2,"type":"item.completed","turnId":"t","item":agent("A synthetic answer")})).unwrap();
        writer.flush().unwrap();
        total_bytes += std::fs::metadata(path).unwrap().len();
    }
    // Normal startup already prewarms the existing summary cache. No search
    // index or content cache is built, and there is exactly one timed query.
    assert_eq!(test.app.store.list().unwrap().len(), 200);
    let client = reqwest::Client::builder()
        .no_proxy()
        .unix_socket(test.root.path().join("s"))
        .timeout(Duration::from_secs(60))
        .build()
        .unwrap();
    let start = std::time::Instant::now();
    let result: serde_json::Value = client
        .get("http://localhost/v1/sessions/search?query=absent-keyword&cwd=%2Fwork")
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert_eq!(result["data"], json!([]));
    eprintln!(
        "P2-227 single search: 200 sessions, {:.2} MiB/session, {:.2} MiB total, {} ms (debug, summary cache warm, full scan, no matches)",
        total_bytes as f64 / 200.0 / 1048576.0,
        total_bytes as f64 / 1048576.0,
        elapsed.as_millis()
    );
    test.stop().await;
}
