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
async fn default_backoff_is_exponential_jittered_and_capped() {
    let server = Server::start(
        (0..11)
            .map(|_| Reply {
                status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
                body: "{}".into(),
                retry_after: None,
                hang: false,
            })
            .collect(),
    )
    .await;
    let model = ChatCompletions::new(&server.base_url, ApiKey::new(KEY))
        .unwrap()
        .with_retry_policy(canter_model::RetryPolicy {
            sleep: |_| Box::pin(tokio::time::sleep(Duration::from_millis(1))),
            ..Default::default()
        });
    let events: Vec<_> = model
        .stream(request(), CancellationToken::new())
        .collect()
        .await;
    let expected = [
        500, 1000, 2000, 4000, 8000, 16000, 32000, 32000, 32000, 32000,
    ];
    for (event, max) in events.iter().zip(expected) {
        let Ok(ModelEvent::Retrying { delay, .. }) = event else {
            panic!("expected retry: {event:?}")
        };
        assert!((max * 9 / 10..=max).contains(&delay.as_millis()));
    }
    assert_eq!(events.len(), 11);
    assert_eq!(events.last().unwrap().as_ref().unwrap_err().retries, 10);
    assert_eq!(server.requests.lock().unwrap().len(), 11);
    server.stop().await;
}

#[tokio::test]
async fn request_repairs_tool_pairs_after_unknown_history_or_interruption() {
    let messages = serde_json::from_value(json!([
        {"type":"user","text":"Read the files"},
        {"type":"tool_result","tool_call_id":"missing","text":"An orphan before its call"},
        {"type":"assistant","content":[
            {"type":"custom_tool_call","id":"future","name":"apply_patch","input":"patch"}
        ]},
        {"type":"tool_result","tool_call_id":"future","text":"Unknown call's result"},
        {"type":"assistant","content":[
            {"type":"reasoning","text":"Keep this reasoning"},
            {"type":"text","text":"Reading"},
            {"type":"tool_call","id":"missing","name":"read","arguments":"{raw"},
            {"type":"tool_call","id":"kept","name":"read","arguments":"{}"},
            {"type":"tool_call","id":"interrupted","name":"read","arguments":"{}"}
        ]},
        {"type":"future_tool_result","tool_call_id":"missing","parts":["image"]},
        {"type":"tool_result","tool_call_id":"kept","text":"Original result"},
        {"type":"user","text":"Continue"},
        {"type":"tool_result","tool_call_id":"interrupted","text":"Too late for that batch"},
        {"type":"assistant","content":[
            {"type":"tool_call","id":"last","name":"read","arguments":"{}"}
        ]}
    ]))
    .unwrap();
    let server = Server::start(vec![Reply::sse(STOP)]).await;
    let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
    let events: Vec<_> = client
        .stream(
            Request {
                messages,
                ..request()
            },
            CancellationToken::new(),
        )
        .collect()
        .await;
    assert_eq!(events, vec![Ok(ModelEvent::Finished(FinishReason::Stop))]);
    assert_eq!(
        server.requests.lock().unwrap()[0]["messages"],
        json!([
            {"role":"user","content":"Read the files"},
            {"role":"assistant","content":"Reading","reasoning_content":"Keep this reasoning","tool_calls":[
                {"id":"missing","type":"function","function":{"name":"read","arguments":"{raw"}},
                {"id":"kept","type":"function","function":{"name":"read","arguments":"{}"}},
                {"id":"interrupted","type":"function","function":{"name":"read","arguments":"{}"}}
            ]},
            {"role":"tool","tool_call_id":"kept","content":"Original result"},
            {"role":"tool","tool_call_id":"missing","content":"No result: interrupted, or not understood by this version"},
            {"role":"tool","tool_call_id":"interrupted","content":"No result: interrupted, or not understood by this version"},
            {"role":"user","content":"Continue"},
            {"role":"assistant","content":"","tool_calls":[
                {"id":"last","type":"function","function":{"name":"read","arguments":"{}"}}
            ]},
            {"role":"tool","tool_call_id":"last","content":"No result: interrupted, or not understood by this version"}
        ])
    );
    server.stop().await;
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
            Some(Ok(ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "first".into()
            }))
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
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(event, ModelEvent::Retrying { .. }));
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
async fn retry_after_over_sixty_seconds_is_capped_and_cancellable() {
    let far_date =
        httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(86400));
    for header in ["61".to_owned(), "86400".to_owned(), far_date] {
        let server = Server::start(vec![Reply {
            status: axum::http::StatusCode::TOO_MANY_REQUESTS,
            body: "{}".into(),
            retry_after: Some(header),
            hang: false,
        }])
        .await;
        let model = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
        let cancel = CancellationToken::new();
        let mut stream = model.stream(request(), cancel.clone());
        let event = stream.next().await.unwrap().unwrap();
        assert!(
            matches!(event, ModelEvent::Retrying { delay, attempt: 1, .. } if delay == Duration::from_secs(60))
        );
        cancel.cancel();
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(250), stream.next())
                .await
                .unwrap(),
            None
        );
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        drop(stream);
        server.stop().await;
    }
}

#[tokio::test]
async fn interrupted_finish_reasons_retry_with_or_without_partial_content() {
    for reason in ["insufficient_system_resource", "aborted"] {
        for emitted_content in [false, true] {
            let chunk = json!({"choices":[{"index":0,"delta":{
                "content": if emitted_content { Some("partial") } else { None },
                "tool_calls":[{"index":0,"id":"unfinished","type":"function","function":{"name":"read","arguments":"{}"}}]
            },"finish_reason":reason}]});
            let server = Server::start(vec![
                Reply::sse(format!("data: {chunk}\n\ndata: [DONE]\n\n")),
                Reply::sse(STOP),
            ])
            .await;
            let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
            let events: Vec<_> = client
                .stream(request(), CancellationToken::new())
                .collect()
                .await;
            assert!(events.iter().any(|event| matches!(event, Ok(ModelEvent::Retrying { error, attempt: 1, .. }) if error.kind == ErrorKind::Server)));
            assert_eq!(
                events.last(),
                Some(&Ok(ModelEvent::Finished(FinishReason::Stop)))
            );
            assert_eq!(server.requests.lock().unwrap().len(), 2);
            assert_eq!(
                events
                    .iter()
                    .filter(|event| matches!(event, Ok(ModelEvent::TextDelta { .. })))
                    .count(),
                usize::from(emitted_content)
            );
            server.stop().await;
        }
    }
}

#[tokio::test]
async fn tool_call_uses_first_nonempty_id_and_name_while_appending_arguments() {
    let mut body: String = [
        json!({"index":0,"id":"","function":{"name":"","arguments":" {"}}),
        json!({"index":0,"id":"call_1","function":{"name":"read","arguments":"bad:"}}),
        json!({"index":0,"id":"call_1","function":{"name":"read","arguments":" value"}}),
        json!({"index":0,"id":"replacement","function":{"name":"other","arguments":"}\n"}}),
    ]
    .into_iter()
    .map(|call| {
        format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"tool_calls":[call]}}]})
        )
    })
    .collect();
    body.push_str("data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\ndata: [DONE]\n\n");
    let server = Server::start(vec![Reply::sse(body)]).await;
    let client = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
    let events: Vec<_> = client
        .stream(request(), CancellationToken::new())
        .collect()
        .await;
    assert_eq!(
        events,
        vec![
            Ok(ModelEvent::ToolCall(ToolCall {
                id: "call_1".into(),
                name: "read".into(),
                arguments: " {bad: value}\n".into()
            })),
            Ok(ModelEvent::Finished(FinishReason::ToolCalls))
        ]
    );
    server.stop().await;
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
            max_retries: None,
            thinking: Some(true),
            reasoning_effort: None,
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
            Ok(ModelEvent::ReasoningDelta {
                item_id: "reasoning".into(),
                delta: "想一想".into()
            }),
            Ok(ModelEvent::TextDelta {
                item_id: "text".into(),
                delta: "Reading.".into()
            }),
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
async fn zero_retries_and_exhaustion_report_the_actual_retry_count() {
    for limit in [0, 2, 10] {
        let server = Server::start(
            (0..=limit)
                .map(|_| Reply {
                    status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    body: "{}".into(),
                    retry_after: Some("0".into()),
                    hang: false,
                })
                .collect(),
        )
        .await;
        let model = ChatCompletions::new(&server.base_url, ApiKey::new(KEY))
            .unwrap()
            .with_retry_policy(canter_model::RetryPolicy {
                max_retries: limit,
                ..Default::default()
            });
        let events: Vec<_> = model
            .stream(request(), CancellationToken::new())
            .collect()
            .await;
        let error = events.last().unwrap().as_ref().unwrap_err();
        assert_eq!(error.retries, limit);
        assert!(
            error
                .to_string()
                .contains(&format!("retried {limit} times"))
        );
        assert_eq!(server.requests.lock().unwrap().len(), limit as usize + 1);
        server.stop().await;
    }
}

#[tokio::test]
async fn retries_more_than_three_server_errors_before_success() {
    let mut replies = (0..5)
        .map(|_| Reply {
            status: axum::http::StatusCode::SERVICE_UNAVAILABLE,
            body: "{}".into(),
            retry_after: Some("0".into()),
            hang: false,
        })
        .collect::<Vec<_>>();
    replies.push(Reply::sse(STOP));
    let server = Server::start(replies).await;
    let model = ChatCompletions::new(&server.base_url, ApiKey::new(KEY)).unwrap();
    let events: Vec<_> = model
        .stream(request(), CancellationToken::new())
        .collect()
        .await;
    assert_eq!(
        events.last(),
        Some(&Ok(ModelEvent::Finished(FinishReason::Stop)))
    );
    assert_eq!(server.requests.lock().unwrap().len(), 6);
    let retries: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            Ok(ModelEvent::Retrying {
                attempt,
                max_retries,
                delay,
                error,
            }) => {
                assert_eq!(*max_retries, 10);
                assert_eq!(*delay, Duration::ZERO);
                assert_eq!(error.status, Some(503));
                Some(*attempt)
            }
            _ => None,
        })
        .collect();
    assert_eq!(retries, vec![1, 2, 3, 4, 5]);
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
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, Ok(ModelEvent::Retrying { .. })))
            .count(),
        3
    );
    assert_eq!(
        events.last(),
        Some(&Ok(ModelEvent::Finished(FinishReason::Stop)))
    );
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
        let attempts = if retryable { 11 } else { 1 };
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
        assert_eq!(events.len(), attempts);
        let error = events.last().unwrap().as_ref().unwrap_err();
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
async fn disconnect_after_any_emitted_event_retries_the_same_request() {
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
        assert_eq!(events.len(), 3);
        assert!(events[0].is_ok());
        assert!(
            matches!(&events[1], Ok(ModelEvent::Retrying { error, attempt: 1, .. }) if error.kind == ErrorKind::Network)
        );
        assert_eq!(events[2], Ok(ModelEvent::Finished(FinishReason::Stop)));
        let requests = server.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0], requests[1]);

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
