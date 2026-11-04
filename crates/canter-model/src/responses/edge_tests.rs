use super::*;
use crate::{
    AssistantContent, ToolDefinition,
    test_support::{Reply, Server},
};
use serde_json::json;

fn event(value: Value) -> String {
    format!("data: {value}\n\n")
}
fn complete(output: Value) -> String {
    event(json!({"type":"response.completed","response":{"status":"completed","output":output}}))
}
fn model(server: &Server) -> Responses {
    Responses::with_endpoint(
        Arc::new(super::tests::Auth),
        format!("{}responses", server.base_url).parse().unwrap(),
    )
    .unwrap()
    .with_retry_policy(crate::RetryPolicy {
        sleep: |_| Box::pin(async {}),
        ..Default::default()
    })
}

#[tokio::test]
async fn added_content_and_terminal_usage_prevent_retry() {
    for prefix in [
        event(
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"m","content":[{"type":"output_text","text":"already produced"}]}}),
        ),
        event(
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"r","summary":[],"encrypted_content":"test-cipher"}}),
        ),
        event(
            json!({"type":"response.function_call_arguments.done","output_index":0,"item_id":"f","arguments":"{}"}),
        ),
        event(
            json!({"type":"response.failed","response":{"error":{"code":"server_error"},"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
        ),
    ] {
        let server = Server::start(vec![
            Reply::sse(prefix + &event(json!({"type":"error","code":"server_error"}))),
            Reply::sse(complete(json!([]))),
        ])
        .await;
        let events: Vec<_> = model(&server)
            .stream(Request::default(), CancellationToken::new())
            .collect()
            .await;
        assert_eq!(server.requests.lock().unwrap().len(), 1);
        assert!(events.last().unwrap().is_err());
        server.stop().await;
    }
}

#[tokio::test]
async fn cancellation_and_drop_never_release_provisional_tools() {
    let call = json!({"type":"function_call","id":"f","call_id":"c","namespace":"canter","name":"read","arguments":"{}","status":"completed"});
    let server=Server::start(vec![Reply {hang:true,..Reply::sse(
        event(json!({"type":"response.output_item.done","output_index":0,"item":call}))+
        &event(json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"m","content":[]}}))+
        &event(json!({"type":"response.output_text.delta","output_index":1,"item_id":"m","content_index":0,"delta":"Visible"}))
    )}]).await;
    let model = model(&server);
    let cancel = CancellationToken::new();
    let mut stream = model.stream(
        Request {
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: String::new(),
                parameters: json!({}),
            }],
            ..Default::default()
        },
        cancel.clone(),
    );
    assert!(matches!(
        stream.next().await,
        Some(Ok(ModelEvent::TextDelta { .. }))
    ));
    cancel.cancel();
    assert!(stream.next().await.is_none());
    drop(stream);
    server.stop().await;
}

#[tokio::test]
async fn unknown_items_or_unadvertised_calls_fail_atomically_but_unknown_events_are_skipped() {
    let message = json!({"type":"message","id":"m","role":"assistant","status":"completed","content":[{"type":"refusal","refusal":"Cannot comply"}]});
    for bad in [
        json!({"type":"new_hosted_tool","id":"x"}),
        json!({"type":"function_call","id":"f","call_id":"c","namespace":"canter","name":"not_advertised","arguments":"{}","status":"completed"}),
    ] {
        let server = Server::start(vec![Reply::sse(complete(json!([message, bad])))]).await;
        let events: Vec<_> = model(&server)
            .stream(Request::default(), CancellationToken::new())
            .collect()
            .await;
        assert_eq!(
            events,
            vec![Err(ModelError::new(ErrorKind::UnsupportedOutput))]
        );
        server.stop().await;
    }
    let server = Server::start(vec![Reply::sse(
        event(json!({"type":"response.future_noncritical","extra":"ignored"}))
            + &complete(json!([message])),
    )])
    .await;
    let events: Vec<_> = model(&server)
        .stream(Request::default(), CancellationToken::new())
        .collect()
        .await;
    assert!(events.iter().any(|event|matches!(event,Ok(ModelEvent::ItemCompleted {content:AssistantContent::Text {text},..}) if text=="Cannot comply")));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(ModelEvent::Usage(_))))
    );
    server.stop().await;
}

#[tokio::test]
async fn quota_without_output_never_retries_in_http_or_sse_and_detail_is_private() {
    for reply in [
        Reply {status:reqwest::StatusCode::TOO_MANY_REQUESTS,..Reply::sse(json!({"error":{"code":"subscription_sharing_usage_limit_exceeded","message":crate::test_support::KEY}}).to_string())},
        Reply::sse(event(json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":crate::test_support::KEY}}}))),
    ] {
        let server=Server::start(vec![reply]).await;
        let events:Vec<_>=model(&server).stream(Request::default(),CancellationToken::new()).collect().await;
        assert_eq!(server.requests.lock().unwrap().len(),1);
        assert!(events.last().unwrap().as_ref().unwrap_err().to_string().contains("https://chatgpt.com/settings/usage"));
        assert!(!format!("{events:?}").contains(crate::test_support::KEY));
        server.stop().await;
    }
    for (status, kind) in [
        (403, ErrorKind::ChatgptPermission),
        (503, ErrorKind::Server),
    ] {
        let server = Server::start(vec![Reply {
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            ..Reply::sse(json!({"detail":crate::test_support::KEY}).to_string())
        }])
        .await;
        let events: Vec<_> = model(&server)
            .stream(
                Request {
                    options: crate::RequestOptions {
                        max_retries: Some(0),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .collect()
            .await;
        assert_eq!(events.last().unwrap().as_ref().unwrap_err().kind, kind);
        assert!(!format!("{events:?}").contains(crate::test_support::KEY));
        server.stop().await;
    }
}

#[tokio::test]
async fn incomplete_tool_item_in_completed_batch_is_not_executable() {
    let server=Server::start(vec![Reply::sse(complete(json!([{"type":"function_call","id":"f","call_id":"c","namespace":"canter","name":"read","arguments":"{}","status":"in_progress"}])))]).await;
    let events: Vec<_> = model(&server)
        .stream(
            Request {
                tools: vec![ToolDefinition {
                    name: "read".into(),
                    description: String::new(),
                    parameters: json!({}),
                }],
                ..Default::default()
            },
            CancellationToken::new(),
        )
        .collect()
        .await;
    assert!(events.last().unwrap().is_err());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Ok(ModelEvent::ItemCompleted { .. })))
    );
    server.stop().await;
}
