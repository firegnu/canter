use super::*;
use crate::{
    AssistantContent, Message, ToolDefinition,
    test_support::{Reply, Server},
};
use serde_json::json;

fn event(value: Value) -> String {
    format!("data: {value}\n\n")
}

#[test]
fn prompt_cache_key_truncates_at_64_unicode_characters() {
    for (session_id, expected) in [
        ("a".repeat(65), "a".repeat(64)),
        (
            format!("{}界🦀尾", "a".repeat(62)),
            format!("{}界🦀", "a".repeat(62)),
        ),
        ("界".repeat(64), "界".repeat(64)),
    ] {
        let body = wire::request_body(
            &Request {
                session_id: Some(session_id),
                ..Default::default()
            },
            "test-account-fingerprint",
            true,
        );
        let key = body["prompt_cache_key"].as_str().unwrap();
        assert_eq!(key, expected);
        assert_eq!(key.chars().count(), 64);
    }
}

#[test]
fn prompt_cache_key_is_omitted_without_a_session_id() {
    for session_id in [None, Some(String::new())] {
        let body = wire::request_body(
            &Request {
                session_id,
                ..Default::default()
            },
            "test-account-fingerprint",
            true,
        );
        assert!(body.get("prompt_cache_key").is_none());
    }
}

#[tokio::test]
async fn cache_diagnostics_cover_failures_retries_and_cancellation() {
    const CHILD: &str = "CANTER_CACHE_FAILURE_TEST";
    if std::env::var_os(CHILD).is_none() {
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "responses::cache_tests::cache_diagnostics_cover_failures_retries_and_cancellation",
                "--nocapture",
            ])
            .env_clear()
            .env(CHILD, "1")
            .kill_on_drop(true)
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let log = String::from_utf8(output.stderr).unwrap();
        let rows: Vec<Value> = log
            .lines()
            .filter_map(|line| line.strip_prefix("Responses cache: "))
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 9, "{log}");
        for (row, kind) in rows.iter().zip([
            "Server",
            "missing",
            "ChatgptUsageLimit",
            "IncompleteResponse",
            "Network",
            "Other",
            "missing",
            "Authentication",
            "ChatgptNotLoggedIn",
        ]) {
            assert_eq!(row["error_kind"], kind);
            assert_eq!(row["prompt_cache_key_present"], false);
            assert_eq!(row["session_header_present"], false);
        }
        assert_eq!(rows[0]["outcome"], "failure");
        assert_eq!(rows[1]["outcome"], "success");
        assert_eq!(rows[1]["input_prefix_equal"], true);
        assert_eq!(rows[2]["usage"]["input_tokens"], 100);
        assert_eq!(rows[5]["usage"]["input_tokens"], "invalid");
        assert_eq!(
            rows[5]["usage"]["output_tokens_details.reasoning_tokens"],
            "invalid"
        );
        assert_eq!(rows[6]["outcome"], "cancelled");
        assert_eq!(rows[6]["done"][0]["type"], "reasoning");
        assert_eq!(rows[7]["sent_unix_ms"], "missing");
        assert_eq!(rows[8]["sent_unix_ms"], "missing");
        for private in [
            "private-error",
            "private-id",
            "private-text",
            "private-cipher",
            crate::test_support::KEY,
        ] {
            assert!(!log.contains(private), "leaked fixture marker");
        }
        return;
    }
    let terminal =
        event(json!({"type":"response.completed","response":{"status":"completed","output":[]}}));
    let server = Server::start(vec![
        Reply {status:reqwest::StatusCode::INTERNAL_SERVER_ERROR, ..Reply::sse("private-error")},
        Reply::sse(terminal),
        Reply::sse(event(json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":"private-error"},"usage":{"input_tokens":100,"output_tokens":10,"total_tokens":110}}}))),
        Reply::sse(event(json!({"type":"response.incomplete","response":{"error":{"message":"private-error"}}}))),
        Reply::sse(""),
        Reply::sse(event(json!({"type":"response.completed","response":{"status":"completed","usage":{"input_tokens":"private-error","output_tokens":10,"total_tokens":110,"output_tokens_details":{"reasoning_tokens":"private-error"}}}}))),
        Reply {hang:true,..Reply::sse(
            event(json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","id":"private-id","summary":[],"encrypted_content":"private-cipher"}}))
            + &event(json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"private-id-message","content":[]}}))
            + &event(json!({"type":"response.output_text.delta","output_index":1,"item_id":"private-id-message","content_index":0,"delta":"private-text"}))
        )},
    ]).await;
    let endpoint: reqwest::Url = format!("{}responses", server.base_url).parse().unwrap();
    let model = Responses::for_test(Arc::new(super::tests::Auth), endpoint.clone())
        .unwrap()
        .with_retry_policy(crate::RetryPolicy {
            sleep: |_| Box::pin(async {}),
            ..Default::default()
        });
    let events: Vec<_> = model
        .stream(Request::default(), CancellationToken::new())
        .collect()
        .await;
    assert!(matches!(events.last(), Some(Ok(ModelEvent::Finished(_)))));
    for _ in 0..4 {
        let mut request = Request::default();
        request.options.max_retries = Some(0);
        let events: Vec<_> = model
            .stream(request, CancellationToken::new())
            .collect()
            .await;
        assert!(events.last().unwrap().is_err());
    }
    let cancellation = CancellationToken::new();
    let mut stream = model.stream(Request::default(), cancellation.clone());
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelEvent::TextDelta { .. }))
    ));
    cancellation.cancel();
    assert!(stream.next().await.is_none());
    drop(stream);
    for fail_selection in [false, true] {
        let model =
            Responses::for_test(Arc::new(FailingAuth(fail_selection)), endpoint.clone()).unwrap();
        let events: Vec<_> = model
            .stream(Request::default(), CancellationToken::new())
            .collect()
            .await;
        assert!(events.last().unwrap().is_err());
    }
    assert_eq!(server.requests.lock().unwrap().len(), 7);
    assert!(
        server
            .headers
            .lock()
            .unwrap()
            .iter()
            .all(|headers| !headers.contains_key("session-id"))
    );
    server.stop().await;
}

struct FailingAuth(bool);
impl ResponsesAuth for FailingAuth {
    fn credential<'a>(
        &'a self,
        _: &'a Request,
        _: Option<&'a ModelContext>,
    ) -> BoxFuture<'a, Result<Box<dyn ResponsesCredential>, ModelError>> {
        Box::pin(async move {
            if self.0 {
                Err(ModelError::new(ErrorKind::ChatgptNotLoggedIn))
            } else {
                Ok(Box::new(FailingAuth(false)) as Box<dyn ResponsesCredential>)
            }
        })
    }
}
impl ResponsesCredential for FailingAuth {
    fn authorization(&mut self) -> BoxFuture<'_, Result<HeaderValue, ModelError>> {
        Box::pin(async { Err(ModelError::new(ErrorKind::Authentication)) })
    }
    fn recover_unauthorized(&mut self) -> BoxFuture<'_, Result<(), ModelError>> {
        Box::pin(async { Err(ModelError::new(ErrorKind::Authentication)) })
    }
    fn replay_account(&self) -> String {
        "test-account-fingerprint".into()
    }
}

#[tokio::test]
async fn cache_diagnostics_cover_tool_round_and_redact_content() {
    const CHILD: &str = "CANTER_CACHE_LOG_TEST";
    let Ok(mode) = std::env::var(CHILD) else {
        for mode in ["stable", "tools", "developer"] {
            let output = tokio::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "responses::cache_tests::cache_diagnostics_cover_tool_round_and_redact_content",
                    "--nocapture",
                ])
                .env_clear()
                .env(CHILD, mode)
                .kill_on_drop(true)
                .output()
                .await
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let log = String::from_utf8(output.stderr).unwrap();
            let rows: Vec<Value> = log
                .lines()
                .filter_map(|line| line.strip_prefix("Responses cache: "))
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
            assert_eq!(rows.len(), 3, "{log}");
            for (index, row) in rows.iter().enumerate() {
                assert_eq!(row["request"], index + 1);
                assert_eq!(row["outcome"], "success");
                assert_eq!(row["prompt_cache_key_present"], true);
                assert_eq!(row["session_header_present"], true);
                assert_eq!(
                    row["reasoning.effort"],
                    if index == 0 { "missing" } else { "high" }
                );
                assert!(row["sent_unix_ms"].as_u64().unwrap() > 0);
                assert!(row["elapsed_ms"].is_number());
                assert_eq!(row["model_hash"], rows[0]["model_hash"]);
                assert_eq!(row["input_count"], 2 + index * 4);
                assert_eq!(row["usage"]["input_tokens"], 100);
                assert_eq!(row["usage"]["output_tokens"], 10);
                if index > 0 {
                    assert_eq!(row["previous_request"], index);
                    assert_eq!(row["input_prefix_equal"], mode != "developer" || index != 1);
                    assert_eq!(row["tools_equal"], mode != "tools" || index != 1);
                    assert_eq!(row["developer_equal"], mode != "developer" || index != 1);
                }
            }
            assert_eq!(rows[0]["input_prefix_equal"], "missing");
            assert_eq!(
                rows[1]["first_different_item"],
                if mode == "developer" {
                    json!(0)
                } else {
                    json!("missing")
                }
            );
            assert_eq!(
                rows[0]["usage"]["input_tokens_details.cached_tokens"],
                "missing"
            );
            assert_eq!(rows[1]["usage"]["input_tokens_details.cached_tokens"], 0);
            assert_eq!(rows[2]["usage"]["input_tokens_details.cached_tokens"], 80);
            assert_eq!(
                rows[0]["usage"]["output_tokens_details.reasoning_tokens"],
                "missing"
            );
            assert_eq!(
                rows[1]["usage"]["output_tokens_details.reasoning_tokens"],
                0
            );
            assert_eq!(
                rows[2]["usage"]["output_tokens_details.reasoning_tokens"],
                7
            );
            assert_eq!(
                rows[0]["usage"]["input_tokens_details.cache_write_tokens"],
                "missing"
            );
            assert_eq!(
                rows[1]["usage"]["input_tokens_details.cache_write_tokens"],
                0
            );
            assert_eq!(rows[2]["usage"]["cache_write_tokens"], 20);
            assert_eq!(rows[0]["completed_output"], "empty");
            assert_eq!(rows[1]["completed_output"], "equal");
            assert_eq!(rows[2]["completed_output"], "different");
            assert_eq!(
                rows[0]["done"],
                json!([
                    {"index":0,"type":"reasoning","encrypted_content":true,"summary_count":0,"summary_chars":0},
                    {"index":1,"type":"message","encrypted_content":true},
                    {"index":2,"type":"function_call","encrypted_content":true,"encrypted_function_args":true}
                ])
            );
            assert_eq!(rows[0]["completed"], json!([]));
            assert_eq!(
                rows[1]["done"][0]["summary_count"],
                if mode == "tools" { 2 } else { 1 }
            );
            assert_eq!(
                rows[1]["done"][0]["summary_chars"],
                if mode == "tools" { 35 } else { 18 }
            );
            assert_eq!(rows[1]["completed"], rows[1]["done"]);
            assert_eq!(rows[2]["completed"][0]["encrypted_content"], false);
            for private in [
                "private-text",
                "private-arguments",
                "private-id",
                "private-call",
                "private-cipher",
                "private-model",
                "private-developer",
                "private-tool",
                "private-session",
                "private-summary",
                "界🦀",
                "尾",
                crate::test_support::KEY,
            ] {
                assert!(!log.contains(private), "leaked fixture marker");
            }
        }
        return;
    };
    let reasoning = json!({"type":"reasoning","id":"private-id-r","summary":[],"encrypted_content":"private-cipher-r"});
    let message = json!({"type":"message","id":"private-id-m","status":"completed","role":"assistant","content":[{"type":"output_text","text":"private-text"}],"encrypted_content":"private-cipher-m"});
    let call = json!({"type":"function_call","id":"private-id-f","call_id":"private-call","namespace":"canter","name":"read","arguments":"{\"path\":\"private-arguments\"}","encrypted_content":"private-cipher-f","encrypted_function_args":"private-cipher-args"});
    let mut replies = Vec::new();
    for index in 0..3 {
        let mut reasoning = reasoning.clone();
        if index == 1 {
            reasoning["summary"] = json!([
                {"type":"summary_text","text":"private-summary-界🦀"}
            ]);
            if mode == "tools" {
                reasoning["summary"].as_array_mut().unwrap().push(json!(
                    {"type":"summary_text","text":"private-summary-尾"}
                ));
            }
        }
        let items = if index < 2 {
            vec![reasoning.clone(), message.clone(), call.clone()]
        } else {
            vec![message.clone()]
        };
        let mut body = String::new();
        // Arrival order differs from output_index order.
        for (index, item) in items.iter().enumerate().rev() {
            body.push_str(&event(
                json!({"type":"response.output_item.done","output_index":index,"item":item}),
            ));
        }
        let mut usage = json!({"input_tokens":100,"output_tokens":10,"total_tokens":110});
        if index > 0 {
            usage["input_tokens_details"] =
                json!({"cached_tokens":if index == 1 {0} else {80},"cache_write_tokens":0});
            usage["output_tokens_details"] = json!({"reasoning_tokens":if index == 1 {0} else {7}});
        }
        if index == 2 {
            usage["cache_write_tokens"] = json!(20);
        }
        let completed = match index {
            0 => json!([]),
            1 => json!(items),
            _ => {
                let mut item = message.clone();
                item.as_object_mut().unwrap().remove("encrypted_content");
                json!([item])
            }
        };
        body.push_str(&event(json!({"type":"response.completed","response":{"status":"completed","output":completed,"usage":usage}})));
        replies.push(Reply::sse(body));
    }
    let server = Server::start(replies).await;
    let mut request = Request {
        session_id: Some("private-session".into()),
        model: "private-model".into(),
        messages: vec![
            Message::System {
                text: "private-developer".into(),
            },
            Message::User {
                text: "private-text".into(),
            },
        ],
        tools: vec![ToolDefinition {
            name: "read".into(),
            description: "private-tool".into(),
            parameters: json!({}),
        }],
        ..Default::default()
    };
    for index in 0..3 {
        if index == 1 {
            request.options.reasoning_effort = Some("high".into());
            if mode == "tools" {
                request.tools[0].description.push('!');
            }
            if mode == "developer" {
                request.messages[0] = Message::System {
                    text: "private-developer changed".into(),
                };
            }
        }
        // Separate adapter instances must still compare process-wide requests.
        let model = Responses::for_test(
            Arc::new(super::tests::Auth),
            format!("{}responses", server.base_url).parse().unwrap(),
        )
        .unwrap();
        let events: Vec<_> = model
            .stream(request.clone(), CancellationToken::new())
            .collect()
            .await;
        assert!(events.iter().all(Result::is_ok));
        let mut content: Vec<AssistantContent> = Vec::new();
        let mut replay = None;
        for event in events.into_iter().map(Result::unwrap) {
            match event {
                ModelEvent::ItemCompleted { content: part, .. } => content.push(part),
                ModelEvent::Replay(value) => replay = Some(value),
                _ => {}
            }
        }
        request
            .messages
            .push(Message::Assistant { content, replay });
        if index < 2 {
            request.messages.push(Message::ToolResult {
                tool_call_id: "private-call".into(),
                text: "private-text tool result".into(),
            });
        }
    }
    let bodies = server.requests.lock().unwrap().clone();
    assert_eq!(bodies.len(), 3);
    for (index, body) in bodies.into_iter().enumerate() {
        assert_eq!(body["prompt_cache_key"], "private-session");
        if index == 0 {
            assert!(body["reasoning"].get("effort").is_none());
        } else {
            assert_eq!(body["reasoning"]["effort"], "high");
        }
    }
    let headers = server.headers.lock().unwrap().clone();
    assert_eq!(headers.len(), 3);
    for headers in headers {
        assert_eq!(headers["session-id"], "private-session");
    }
    server.stop().await;
}
