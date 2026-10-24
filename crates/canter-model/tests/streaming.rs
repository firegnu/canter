mod support;

use std::time::Duration;

use canter_model::{
    ApiKey, AssistantContent, ChatCompletions, ErrorKind, FinishReason, Message, Model, ModelEvent,
    Request, RequestOptions, ToolCall, ToolDefinition, Usage,
};
use futures_util::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use support::{KEY, Reply, Server};

const STOP: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";

fn request() -> Request {
    Request {
        model: "test-model".into(),
        messages: vec![Message::User {
            text: "Hello".into(),
        }],
        ..Request::default()
    }
}

#[tokio::test]
async fn cancellation_stops_pending_reads_and_discards_buffered_events() {
    for tail in [STOP, ""] {
        let mut reply = Reply::sse(format!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"first\"}}}}]}}\n\n{tail}"
        ));
        reply.hang = true;
        let server = Server::start(vec![reply]).await;
        let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
        let cancellation = CancellationToken::new();
        let mut events = client.stream(request(), cancellation.clone());
        assert_eq!(
            events.next().await,
            Some(Ok(ModelEvent::TextDelta("first".into())))
        );
        if tail.is_empty() {
            // Exercise a read that is already waiting when cancellation arrives.
            tokio::select! {
                event = events.next() => panic!("unexpected event: {event:?}"),
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
        cancellation.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), events.next())
                .await
                .unwrap(),
            None
        );
        drop(events);
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn cancellation_interrupts_retry_after_and_local_backoff() {
    let date = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(60));
    for retry_after in [Some("60".into()), Some(date), None] {
        let server = Server::start(vec![Reply {
            status: axum::http::StatusCode::TOO_MANY_REQUESTS,
            body: "{}".into(),
            retry_after,
            hang: false,
        }])
        .await;
        let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
        let cancellation = CancellationToken::new();
        let mut events = client.stream(request(), cancellation.clone());
        tokio::select! {
            event = events.next() => panic!("unexpected event before cancellation: {event:?}"),
            _ = tokio::time::sleep(Duration::from_millis(50)) => {}
        }
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        cancellation.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), events.next())
                .await
                .unwrap(),
            None
        );
        drop(events);
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn streams_content_tools_and_usage_and_replays_reasoning() {
    let body = include_str!("fixtures/full.sse").replace('\n', "\r\n");
    let server = Server::start(vec![Reply::sse(body)]).await;
    let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
    let request = Request {
        model: "caller-model".into(),
        messages: vec![
            Message::System {
                text: "Be helpful.".into(),
            },
            Message::User {
                text: "First question".into(),
            },
            Message::Assistant {
                content: vec![
                    AssistantContent::Reasoning {
                        text: "Earlier reasoning without tool calls.".into(),
                    },
                    AssistantContent::Text {
                        text: "Earlier answer.".into(),
                    },
                ],
            },
            Message::User {
                text: "Read files".into(),
            },
            Message::Unknown,
            Message::Assistant {
                content: vec![
                    AssistantContent::Reasoning {
                        text: "  original\nreasoning".into(),
                    },
                    AssistantContent::ToolCall(ToolCall {
                        id: "previous".into(),
                        name: "read".into(),
                        arguments: " {broken: [".into(),
                    }),
                ],
            },
            Message::ToolResult {
                tool_call_id: "previous".into(),
                text: "Invalid arguments".into(),
            },
        ],
        tools: vec![ToolDefinition {
            name: "read".into(),
            description: "Read a file".into(),
            parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
        }],
        options: RequestOptions {
            thinking: Some(true),
            max_output_tokens: Some(128),
        },
    };
    let events: Vec<_> = tokio::time::timeout(
        Duration::from_secs(5),
        client.stream(request, CancellationToken::new()).collect(),
    )
    .await
    .unwrap();
    assert_eq!(
        events,
        vec![
            Ok(ModelEvent::ReasoningDelta("想一想".into())),
            Ok(ModelEvent::TextDelta("Reading.".into())),
            Ok(ModelEvent::Usage(Usage {
                input_tokens: 10,
                output_tokens: 7,
                total_tokens: 17,
                cache_read_tokens: Some(4),
                cache_miss_tokens: Some(6),
                reasoning_tokens: Some(3)
            })),
            Ok(ModelEvent::ToolCall(ToolCall {
                id: "a".into(),
                name: "read".into(),
                arguments: "{}".into()
            })),
            Ok(ModelEvent::ToolCall(ToolCall {
                id: "b".into(),
                name: "read".into(),
                arguments: "{\"path\":\"b\"}".into()
            })),
            Ok(ModelEvent::Finished(FinishReason::ToolCalls))
        ]
    );
    assert_eq!(
        server.requests.lock().unwrap().as_slice(),
        &[json!({
            "model":"caller-model", "stream":true, "stream_options":{"include_usage":true},
            "thinking":{"type":"enabled"}, "max_tokens":128,
            "tools":[{"type":"function","function":{"name":"read","description":"Read a file","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}}],
            "messages":[
                {"role":"system","content":"Be helpful."},
                {"role":"user","content":"First question"},
                {"role":"assistant","content":"Earlier answer.","reasoning_content":"Earlier reasoning without tool calls."},
                {"role":"user","content":"Read files"},
                {"role":"assistant","content":"","reasoning_content":"  original\nreasoning","tool_calls":[{"id":"previous","type":"function","function":{"name":"read","arguments":" {broken: ["}}]},
                {"role":"tool","tool_call_id":"previous","content":"Invalid arguments"}
            ]
        })]
    );
    server.stop().await;
}

#[tokio::test]
async fn retries_rate_limit_server_and_early_disconnect_honoring_retry_after() {
    use axum::http::StatusCode;
    let server = Server::start(vec![
        Reply {
            status: StatusCode::TOO_MANY_REQUESTS,
            body: "{}".into(),
            retry_after: Some("2".into()),
            hang: false,
        },
        Reply {
            status: StatusCode::SERVICE_UNAVAILABLE,
            body: "{}".into(),
            retry_after: None,
            hang: false,
        },
        // A role-only chunk emits nothing, so its disconnect is still retryable.
        Reply::sse("data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n"),
        Reply::sse(STOP),
    ])
    .await;
    let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
    let events: Vec<_> = tokio::time::timeout(
        Duration::from_secs(10),
        client.stream(request(), CancellationToken::new()).collect(),
    )
    .await
    .unwrap();
    assert_eq!(events, vec![Ok(ModelEvent::Finished(FinishReason::Stop))]);
    {
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 4);
        assert!(requests.iter().all(|request| request == &requests[0]));
        let arrivals = server.arrivals.lock().unwrap();
        // Provider's delay takes precedence over the local one-second cap.
        assert!(arrivals[1].duration_since(arrivals[0]) >= Duration::from_secs(2));
    }
    server.stop().await;
}

#[tokio::test]
async fn errors_are_classified_retries_are_bounded_and_credentials_are_redacted() {
    use axum::http::StatusCode;
    for (status, code, message, expected) in [
        (
            401,
            "invalid_api_key",
            "Invalid key",
            ErrorKind::Authentication,
        ),
        (403, "forbidden", "Forbidden", ErrorKind::Authentication),
        (
            400,
            "invalid_request_error",
            "Bad request",
            ErrorKind::InvalidRequest,
        ),
        (
            422,
            "invalid_parameters",
            "Bad parameters",
            ErrorKind::InvalidRequest,
        ),
        (
            400,
            "context_length_exceeded",
            "Too long",
            ErrorKind::ContextLength,
        ),
        (
            400,
            "invalid_request_error",
            "This model's maximum context length is 131072 tokens. You requested 140000 tokens.",
            ErrorKind::ContextLength,
        ),
        (
            402,
            "insufficient_balance",
            "Insufficient balance",
            ErrorKind::Other,
        ),
        (429, "rate_limit", "Slow down", ErrorKind::RateLimited),
        (503, "overloaded", "Try later", ErrorKind::Server),
        (408, "timeout", "Request timeout", ErrorKind::Timeout),
    ] {
        let retryable = matches!(
            expected,
            ErrorKind::RateLimited | ErrorKind::Server | ErrorKind::Timeout
        );
        let attempts = if retryable { 4 } else { 1 };
        let body = json!({"error":{"code":code,"message":format!("{message}; Authorization: Bearer {KEY}"),"type":KEY}}).to_string();
        let replies = (0..attempts)
            .map(|_| Reply {
                status: StatusCode::from_u16(status).unwrap(),
                body: body.clone(),
                retry_after: Some("0".into()),
                hang: false,
            })
            .collect();
        let server = Server::start(replies).await;
        let key = ApiKey::new(KEY);
        let client = ChatCompletions::new(&server.base_url, key.clone()).unwrap();
        let events: Vec<_> = client
            .stream(request(), CancellationToken::new())
            .collect()
            .await;
        assert_eq!(events.len(), 1);
        let error = events[0].as_ref().unwrap_err();
        assert_eq!(error.kind, expected);
        assert_eq!(error.status, Some(status));
        assert_eq!(error.is_retryable(), retryable);
        let diagnostics = format!("{key:?} {key} {client:?} {error:?} {error}");
        assert!(!diagnostics.contains(KEY));
        assert!(!diagnostics.contains("Authorization"));
        assert!(!diagnostics.contains("Bearer"));
        assert!(std::error::Error::source(error).is_none());
        assert_eq!(server.requests.lock().unwrap().len(), attempts);
        server.stop().await;
    }
    // Malformed SSE must not echo its contents into parser diagnostics either.
    let server = Server::start(vec![Reply::sse(format!("data: {KEY}\n\n"))]).await;
    let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
    let events: Vec<_> = client
        .stream(request(), CancellationToken::new())
        .collect()
        .await;
    assert!(events[0].is_err());
    assert!(!format!("{events:?}").contains(KEY));
    server.stop().await;
}

#[tokio::test]
async fn disconnect_after_any_emitted_event_returns_error_without_retry() {
    for chunk in [
        json!({"choices":[{"index":0,"delta":{"content":"partial"}}]}),
        json!({"choices":[{"index":0,"delta":{"reasoning_content":"partial"}}]}),
        json!({"choices":[],"usage":{"prompt_tokens":1,"completion_tokens":2,"total_tokens":3}}),
        json!({"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":"stop"}]}),
    ] {
        // Both HTTP EOF and a partially received SSE frame are incomplete without [DONE].
        let server = Server::start(vec![
            Reply::sse(format!("data: {chunk}\n\ndata: {{")),
            Reply::sse(STOP),
        ])
        .await;
        let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
        let events: Vec<_> = client
            .stream(request(), CancellationToken::new())
            .collect()
            .await;
        assert_eq!(events.len(), 2);
        assert!(events[0].is_ok());
        assert_eq!(events[1].as_ref().unwrap_err().kind, ErrorKind::Network);
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn fragmented_invalid_arguments_are_preserved_and_truncated_calls_are_not_emitted() {
    for (reason, expected) in [
        ("tool_calls", FinishReason::ToolCalls),
        ("length", FinishReason::Length),
        ("content_filter", FinishReason::Other),
    ] {
        let body = format!(
            "{}data: {}\n\ndata: [DONE]\n\n",
            include_str!("fixtures/invalid-arguments.sse"),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":reason}]})
        );
        let server = Server::start(vec![Reply::sse(body)]).await;
        let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
        let events: Vec<_> = client
            .stream(request(), CancellationToken::new())
            .collect()
            .await;
        let mut expected_events = Vec::new();
        if reason == "tool_calls" {
            expected_events.push(Ok(ModelEvent::ToolCall(ToolCall {
                id: "raw".into(),
                name: "read".into(),
                arguments: " {\"path\": invalid }\n".into(),
            })));
        }
        expected_events.push(Ok(ModelEvent::Finished(expected)));
        assert_eq!(events, expected_events);
        server.stop().await;
    }
}
