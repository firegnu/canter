use std::sync::Arc;

use eventsource_stream::Eventsource;
use futures_util::StreamExt;
use futures_util::future::BoxFuture;
use reqwest::header::HeaderValue;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{ErrorKind, Model, ModelContext, ModelError, ModelEvent, ModelStream, Request};

#[cfg(test)]
#[path = "responses/cache_tests.rs"]
mod cache_tests;
mod diagnostics;
#[cfg(test)]
#[path = "responses/edge_tests.rs"]
mod edge_tests;
mod error;
mod sse;
mod wire;

/// Supplied by the daemon; the adapter never opens account files.
pub trait ResponsesAuth: Send + Sync {
    fn credential<'a>(
        &'a self,
        request: &'a Request,
        context: Option<&'a ModelContext>,
    ) -> BoxFuture<'a, Result<Box<dyn ResponsesCredential>, ModelError>>;
}

/// A selected account for one inference request, including all its attempts.
pub trait ResponsesCredential: Send {
    fn authorization(&mut self) -> BoxFuture<'_, Result<HeaderValue, ModelError>>;
    fn recover_unauthorized(&mut self) -> BoxFuture<'_, Result<(), ModelError>>;
}

pub struct Responses {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    auth: Arc<dyn ResponsesAuth>,
    retry: crate::RetryPolicy,
}

impl Responses {
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn for_test(
        auth: Arc<dyn ResponsesAuth>,
        endpoint: reqwest::Url,
    ) -> Result<Self, ModelError> {
        let local = endpoint.host_str().is_some_and(|host| {
            host == "localhost"
                || host
                    .trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        });
        if !local
            || !matches!(endpoint.scheme(), "http" | "https")
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ModelError::new(ErrorKind::InvalidRequest));
        }
        Self::with_endpoint(auth, endpoint)
    }

    pub fn new(auth: Arc<dyn ResponsesAuth>) -> Result<Self, ModelError> {
        Self::with_endpoint(
            auth,
            "https://api.openai.com/v1/responses"
                .parse()
                .expect("official endpoint"),
        )
    }
    fn with_endpoint(
        auth: Arc<dyn ResponsesAuth>,
        endpoint: reqwest::Url,
    ) -> Result<Self, ModelError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("canter/", env!("CARGO_PKG_VERSION")))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(std::time::Duration::from_secs(30))
            .read_timeout(std::time::Duration::from_secs(300))
            .build()
            .map_err(|_| ModelError::new(ErrorKind::Other))?;
        Ok(Self {
            client,
            endpoint,
            auth,
            retry: crate::RetryPolicy::default(),
        })
    }

    pub fn with_retry_policy(mut self, retry: crate::RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    fn attempt<'a>(
        &'a self,
        request: &'a Request,
        credential: &'a mut dyn ResponsesCredential,
        observed: &'a mut bool,
    ) -> ModelStream<'a> {
        Box::pin(async_stream::stream! {
            let body = wire::request_body(request);
            let mut diagnostic = diagnostics::Diagnostic::new(&body, wire::session_id(request).is_some());
            let mut stream = self.attempt_inner(request, credential, observed, &body, &mut diagnostic);
            let terminal = loop {
                match stream.next().await {
                    Some(event) if event.is_err() || matches!(event, Ok(ModelEvent::Finished(_))) => break Some(event),
                    Some(event) => yield event,
                    None => break None,
                }
            };
            drop(stream);
            diagnostic.finish(terminal.as_ref());
            drop(diagnostic);
            if let Some(event) = terminal { yield event; }
        })
    }

    fn attempt_inner<'a>(
        &'a self,
        request: &'a Request,
        credential: &'a mut dyn ResponsesCredential,
        observed: &'a mut bool,
        body: &'a Value,
        diagnostic: &'a mut diagnostics::Diagnostic,
    ) -> ModelStream<'a> {
        Box::pin(async_stream::try_stream! {
            let mut authorization = credential.authorization().await?;
            authorization.set_sensitive(true);
            diagnostic.sent();
            let mut post = self.client.post(self.endpoint.clone()).header("authorization", authorization);
            if let Some(session_id) = wire::session_id(request) { post = post.header("session-id", session_id); }
            let response = post
                .json(body).send().await.map_err(error::transport)?;
            if !response.status().is_success() { Err(error::http(response).await)?; return; }
            let mut events = response.bytes_stream().eventsource();
            let mut state = sse::State::default();
            loop {
                let event = events.next().await.ok_or_else(|| ModelError::new(ErrorKind::Network))?
                    .map_err(|error| match error {
                        eventsource_stream::EventStreamError::Transport(error) => error::transport(error),
                        _ => ModelError::new(ErrorKind::Other),
                    })?;
                let event: Value = serde_json::from_str(&event.data).map_err(|_| ModelError::new(ErrorKind::Other))?;
                diagnostic.observe(&event);
                // Even provisional function arguments count as output, although
                // they cannot enter the public execution path until completed.
                *observed |= sse::has_output(&event);
                let usage = match event["type"].as_str() {
                    Some("response.completed" | "response.failed" | "response.incomplete") => sse::usage(&event["response"])?,
                    _ => None,
                };
                if let Some(usage) = usage {
                    *observed = true;
                    yield ModelEvent::Usage(usage);
                }
                for output_event in state.accept(&event, &request.tools)? {
                    let finished = matches!(output_event, ModelEvent::Finished(_));
                    if finished {
                        yield ModelEvent::Replay(crate::Replay::Responses {
                            model:request.model.clone(), scope:None,
                            output:state.replay_output(),
                        });
                    }
                    yield output_event;
                    if finished { return; }
                }
            }
        })
    }
}

impl Responses {
    fn stream_inner(
        &self,
        request: Request,
        cancellation: CancellationToken,
        context: Option<ModelContext>,
    ) -> ModelStream<'_> {
        Box::pin(async_stream::try_stream! {
            let credential = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return,
                credential = self.auth.credential(&request, context.as_ref()) => credential,
            };
            if let Err(error) = &credential {
                let mut diagnostic = diagnostics::Diagnostic::new(&wire::request_body(&request), wire::session_id(&request).is_some());
                diagnostic.finish(Some(&Err(error.clone())));
            }
            let mut credential = credential?;
            let limit = request.options.max_retries.map_or(self.retry.max_retries, |limit| limit.min(self.retry.max_retries));
            let mut retries = 0;
            let mut recovered = false;
            loop {
                let mut observed = false;
                let mut attempt = self.attempt(&request, &mut *credential, &mut observed);
                let mut failure = loop {
                    let event = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        event = attempt.next() => event,
                    };
                    match event {
                        Some(Ok(event)) => {
                            let finished = matches!(event, ModelEvent::Finished(_));
                            yield event;
                            if finished { return; }
                        }
                        Some(Err(error)) => break error,
                        None => return,
                    }
                };
                drop(attempt);
                if !observed && failure.status == Some(401) && !recovered {
                    recovered = true;
                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        recovery = credential.recover_unauthorized() => recovery,
                    }?;
                    continue;
                }
                failure.retries = retries;
                if observed || !failure.is_retryable() || retries == limit {
                    Err(failure)?;
                    return;
                }
                let delay = self.retry.delay(retries, &failure);
                retries += 1;
                yield ModelEvent::Retrying {error:failure, attempt:retries, max_retries:limit, delay};
                tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return,
                    _ = (self.retry.sleep)(delay) => {},
                }
            }
        })
    }
}

impl Model for Responses {
    fn stream(&self, request: Request, cancellation: CancellationToken) -> ModelStream<'_> {
        self.stream_inner(request, cancellation, None)
    }

    fn stream_with_context(
        &self,
        request: Request,
        cancellation: CancellationToken,
        context: ModelContext,
    ) -> ModelStream<'_> {
        self.stream_inner(request, cancellation, Some(context))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinishReason, Message, ModelEvent};
    use axum::{Router, routing::post};
    use futures_util::StreamExt;
    use serde_json::json;

    pub(super) struct Auth;
    impl ResponsesAuth for Auth {
        fn credential<'a>(
            &'a self,
            _: &'a Request,
            _: Option<&'a ModelContext>,
        ) -> BoxFuture<'a, Result<Box<dyn ResponsesCredential>, ModelError>> {
            Box::pin(async { Ok(Box::new(Credential) as Box<dyn ResponsesCredential>) })
        }
    }
    struct Credential;
    impl ResponsesCredential for Credential {
        fn authorization(&mut self) -> BoxFuture<'_, Result<HeaderValue, ModelError>> {
            Box::pin(async {
                Ok(HeaderValue::from_str(&format!("Bearer {}", crate::test_support::KEY)).unwrap())
            })
        }
        fn recover_unauthorized(&mut self) -> BoxFuture<'_, Result<(), ModelError>> {
            Box::pin(async { Err(ModelError::new(ErrorKind::Authentication)) })
        }
    }

    #[tokio::test]
    async fn complete_response_requires_the_terminal_event() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/responses", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/v1/responses", post(|| async {
                ([("content-type", "text/event-stream")],
                 "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"id\":\"m1\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"hello\",\"annotations\":[]}] }]}}\n\n")
            }))).await.unwrap();
        });
        let model = Responses::with_endpoint(Arc::new(Auth), endpoint.parse().unwrap()).unwrap();
        let events: Vec<_> = model
            .stream(
                Request {
                    model: "test-model".into(),
                    messages: vec![Message::User {
                        text: "hello".into(),
                    }],
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .collect()
            .await;
        server.abort();
        let _ = server.await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Ok(ModelEvent::Finished(FinishReason::Stop))))
        );
    }

    #[tokio::test]
    async fn request_preserves_prompt_images_and_tools_using_only_allowed_fields() {
        use crate::test_support::{Reply, Server};
        use crate::{RequestOptions, ToolDefinition, UserContent};
        let server = Server::start(vec![Reply::sse("data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[]}}\n\n")]).await;
        let model = Responses::with_endpoint(
            Arc::new(Auth),
            format!("{}responses", server.base_url).parse().unwrap(),
        )
        .unwrap();
        let prompt = "Fixed core.\n\n<project_instructions path=\"a\">\n unchanged  \n</project_instructions>";
        let request = Request {
            session_id: Some("test-session".into()),
            model: "test-model".into(),
            messages: vec![
                Message::System {
                    text: prompt.into(),
                },
                Message::UserParts {
                    content: vec![
                        UserContent::Text {
                            text: "look".into(),
                        },
                        UserContent::Image {
                            path: "a.png".into(),
                            name: "a.png".into(),
                            data_url: "data:image/png;base64,AAAA".into(),
                        },
                    ],
                },
            ],
            tools: vec!["read", "mcp__local__lookup"]
                .into_iter()
                .map(|name| ToolDefinition {
                    name: name.into(),
                    description: format!("{name} description"),
                    parameters: json!({"type":"object","properties":{"path":{"type":"string"}}}),
                })
                .collect(),
            options: RequestOptions {
                thinking: Some(true),
                reasoning_effort: Some("max".into()),
                max_output_tokens: Some(123),
                max_retries: Some(0),
            },
        };
        let _: Vec<_> = model
            .stream(request, CancellationToken::new())
            .collect()
            .await;
        let body = server.requests.lock().unwrap()[0].clone();
        server.stop().await;
        assert_eq!(
            body,
            json!({
                "model":"test-model", "store":false, "stream":true,
                "prompt_cache_key":"test-session",
                "reasoning":{"summary":"auto"},
                "input":[
                    {"role":"developer","content":[{"type":"input_text","text":prompt}]},
                    {"role":"user","content":[{"type":"input_text","text":"look"},{"type":"input_image","image_url":"data:image/png;base64,AAAA"}]}
                ],
                "tools":[
                    {"type":"namespace","name":"canter","description":"Tools in the canter namespace.","tools":[{"type":"function","name":"read","description":"read description","parameters":{"type":"object","properties":{"path":{"type":"string"}}},"strict":false}]},
                    {"type":"namespace","name":"mcp","description":"Tools in the mcp namespace.","tools":[{"type":"function","name":"mcp__local__lookup","description":"mcp__local__lookup description","parameters":{"type":"object","properties":{"path":{"type":"string"}}},"strict":false}]}
                ]
            })
        );
    }

    #[tokio::test]
    async fn ordinary_history_uses_output_text_and_keeps_function_call_shapes() {
        use crate::test_support::{Reply, Server};
        use crate::{AssistantContent, ToolCall};
        let server = Server::start(vec![Reply::sse(sse(vec![json!({
            "type":"response.completed","response":{"status":"completed","output":[]}
        })]))])
        .await;
        let model = Responses::with_endpoint(
            Arc::new(Auth),
            format!("{}responses", server.base_url).parse().unwrap(),
        )
        .unwrap();
        let events: Vec<_> = model
            .stream(
                Request {
                    model: "test-model".into(),
                    messages: vec![
                        Message::System {
                            text: "Keep the existing instructions.\n".into(),
                        },
                        Message::User {
                            text: "Read and look up the file.".into(),
                        },
                        Message::Assistant {
                            content: vec![
                                AssistantContent::Reasoning {
                                    text: "Earlier ordinary reasoning".into(),
                                },
                                AssistantContent::Text {
                                    text: "I will inspect it.\n".into(),
                                },
                                AssistantContent::ToolCall(ToolCall {
                                    id: "call-read".into(),
                                    name: "read".into(),
                                    arguments: "{ \"path\" : \"a.txt\" }".into(),
                                }),
                                AssistantContent::ToolCall(ToolCall {
                                    id: "call-lookup".into(),
                                    name: "mcp__local__lookup".into(),
                                    arguments: "{\"name\":\"a.txt\"}".into(),
                                }),
                            ],
                            replay: None,
                        },
                        Message::ToolResult {
                            tool_call_id: "call-read".into(),
                            text: "1: Contents\n".into(),
                        },
                        Message::ToolResult {
                            tool_call_id: "call-lookup".into(),
                            text: "{\"found\":true}".into(),
                        },
                        Message::Assistant {
                            content: vec![AssistantContent::Text {
                                text: "The file was found.".into(),
                            }],
                            replay: None,
                        },
                        Message::User {
                            text: "Now explain it.".into(),
                        },
                    ],
                    ..Default::default()
                },
                CancellationToken::new(),
            )
            .collect()
            .await;
        assert!(events.iter().all(Result::is_ok));
        let body = server.requests.lock().unwrap()[0].clone();
        server.stop().await;
        // Literal wire items from the Responses contract, including every key
        // and the distinction between assistant output and user/developer input.
        assert_eq!(
            body["input"],
            json!([
                {"role":"developer","content":[{"type":"input_text","text":"Keep the existing instructions.\n"}]},
                {"role":"user","content":[{"type":"input_text","text":"Read and look up the file."}]},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"I will inspect it.\n"}]},
                {"type":"function_call","call_id":"call-read","namespace":"canter","name":"read","arguments":"{ \"path\" : \"a.txt\" }"},
                {"type":"function_call","call_id":"call-lookup","namespace":"mcp","name":"mcp__local__lookup","arguments":"{\"name\":\"a.txt\"}"},
                {"type":"function_call_output","call_id":"call-read","output":"1: Contents\n"},
                {"type":"function_call_output","call_id":"call-lookup","output":"{\"found\":true}"},
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"The file was found."}]},
                {"role":"user","content":[{"type":"input_text","text":"Now explain it."}]}
            ])
        );
    }

    fn sse(events: Vec<Value>) -> String {
        events
            .into_iter()
            .map(|event| {
                // Multi-line data, with transport fragments cutting across UTF-8.
                let json = serde_json::to_string_pretty(&event).unwrap();
                format!(
                    "{}\n",
                    json.lines()
                        .map(|line| format!("data: {line}\n"))
                        .collect::<String>()
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn interleaved_items_preserve_text_arguments_and_usage_until_completed() {
        use crate::test_support::{Reply, Server};
        use crate::{AssistantContent, ToolDefinition, Usage};
        let reasoning = json!({"type":"reasoning","id":"r1","summary":[{"type":"summary_text","text":"Check"}],"encrypted_content":"test-only-ciphertext"});
        let message = json!({"type":"message","id":"m1","role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":"你好","annotations":[]}]});
        let arguments = "{ \"path\" : \"a\" }";
        let call = json!({"type":"function_call","id":"fc1","call_id":"call1","namespace":"canter","name":"read","arguments":arguments,"status":"completed"});
        for output in [
            Some(json!([reasoning, message, call])),
            Some(json!([])),
            Some(json!([message])),
            Some(json!([{"type":"unknown","id":"unrelated"}])),
            None,
        ] {
            let mut response = json!({"status":"completed","usage":{"input_tokens":100,"input_tokens_details":{"cached_tokens":60},"output_tokens":30,"output_tokens_details":{"reasoning_tokens":20},"total_tokens":130}});
            if let Some(output) = output {
                response["output"] = output;
            }
            let server = Server::start(vec![Reply::sse(sse(vec![
            json!({"type":"response.output_item.added","output_index":0,"item":{"type":"reasoning","id":"r1","summary":[]}}),
            json!({"type":"response.output_item.added","output_index":1,"item":{"type":"message","id":"m1","role":"assistant","content":[]}}),
            json!({"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc1","call_id":"call1","namespace":"canter","name":"read","arguments":""}}),
            json!({"type":"response.output_text.delta","output_index":1,"item_id":"m1","content_index":0,"delta":"你"}),
            json!({"type":"response.reasoning_summary_text.delta","output_index":0,"item_id":"r1","summary_index":0,"delta":"Check"}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"item_id":"fc1","delta":"{ \"path\""}),
            json!({"type":"response.output_text.delta","output_index":1,"item_id":"m1","content_index":0,"delta":"好"}),
            json!({"type":"response.function_call_arguments.delta","output_index":2,"item_id":"fc1","delta":" : \"a\" }"}),
            json!({"type":"response.output_item.done","output_index":2,"item":call}),
            json!({"type":"response.output_item.done","output_index":0,"item":reasoning}),
            json!({"type":"response.output_item.done","output_index":1,"item":message}),
            json!({"type":"response.completed","response":response}),
        ]))]).await;
            let model = Responses::with_endpoint(
                Arc::new(Auth),
                format!("{}responses", server.base_url).parse().unwrap(),
            )
            .unwrap();
            let events: Vec<_> = model
                .stream(
                    Request {
                        model: "test-model".into(),
                        tools: vec![ToolDefinition {
                            name: "read".into(),
                            description: "Read".into(),
                            parameters: json!({}),
                        }],
                        ..Default::default()
                    },
                    CancellationToken::new(),
                )
                .collect()
                .await;
            server.stop().await;
            assert!(events.iter().all(Result::is_ok));
            let text: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    Ok(ModelEvent::TextDelta { delta, .. }) => Some(delta.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(text, ["你", "好"]);
            assert!(events.iter().any(
            |event| matches!(event, Ok(ModelEvent::ReasoningDelta {delta,..}) if delta == "Check")
        ));
            assert!(events.iter().any(|event| matches!(event, Ok(ModelEvent::ItemCompleted {content:AssistantContent::ToolCall(call),..}) if call.id == "call1" && call.name == "read" && call.arguments == arguments)));
            assert!(events.iter().any(|event| matches!(event, Ok(ModelEvent::Usage(usage)) if usage == &Usage {input_tokens:100,output_tokens:30,total_tokens:130,cache_read_tokens:Some(60),cache_miss_tokens:Some(40),reasoning_tokens:Some(20)})));
            assert_eq!(
                events.last(),
                Some(&Ok(ModelEvent::Finished(FinishReason::ToolCalls)))
            );
            assert!(!format!("{events:?}").contains("test-only-ciphertext"));
            assert!(events.iter().any(|event| matches!(event, Ok(ModelEvent::Replay(crate::Replay::Responses {output,..})) if output == &vec![reasoning.clone(), message.clone(), call.clone()])));
            let completed: Vec<_> = events
                .iter()
                .filter_map(|event| match event {
                    Ok(ModelEvent::ItemCompleted { item_id, .. }) => Some(item_id.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(completed, ["r1:0", "m1:0", "fc1"]);
        }
    }

    #[tokio::test]
    async fn failed_incomplete_and_eof_never_confirm_tools_and_keep_usage() {
        use crate::test_support::{Reply, Server};
        use crate::{AssistantContent, ToolDefinition};
        for terminal in [
            Some(
                json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_limit_exceeded","message":crate::test_support::KEY}}}),
            ),
            Some(
                json!({"type":"error","code":"subscription_sharing_usage_limit_exceeded","message":crate::test_support::KEY}),
            ),
            Some(
                json!({"type":"response.incomplete","response":{"status":"incomplete","usage":{"input_tokens":10,"output_tokens":4,"total_tokens":14}}}),
            ),
            None,
        ] {
            let incomplete = terminal
                .as_ref()
                .is_some_and(|value| value["type"] == "response.incomplete");
            let quota = terminal.as_ref().is_some_and(|value| {
                value["type"] == "response.failed" || value["type"] == "error"
            });
            let call = json!({"type":"function_call","id":"fc1","call_id":"call1","namespace":"canter","name":"read","arguments":"{}","status":"completed"});
            let mut events = vec![
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"m1","role":"assistant","content":[]}}),
                json!({"type":"response.output_text.delta","output_index":0,"item_id":"m1","content_index":0,"delta":"Visible"}),
                json!({"type":"response.output_item.done","output_index":1,"item":call}),
            ];
            events.extend(terminal);
            let server = Server::start(vec![Reply::sse(sse(events))]).await;
            let model = Responses::with_endpoint(
                Arc::new(Auth),
                format!("{}responses", server.base_url).parse().unwrap(),
            )
            .unwrap();
            let events: Vec<_> = model
                .stream(
                    Request {
                        model: "test-model".into(),
                        tools: vec![ToolDefinition {
                            name: "read".into(),
                            description: "Read".into(),
                            parameters: json!({}),
                        }],
                        ..Default::default()
                    },
                    CancellationToken::new(),
                )
                .collect()
                .await;
            assert_eq!(server.requests.lock().unwrap().len(), 1);
            server.stop().await;
            assert!(events.iter().any(
                |event| matches!(event,Ok(ModelEvent::TextDelta {delta,..}) if delta == "Visible")
            ));
            assert!(!events.iter().any(|event| matches!(
                event,
                Ok(ModelEvent::Finished(_)
                    | ModelEvent::ToolCall(_)
                    | ModelEvent::ItemCompleted {
                        content: AssistantContent::ToolCall(_),
                        ..
                    })
            )));
            assert!(events.last().is_some_and(Result::is_err));
            assert!(!format!("{events:?}").contains(crate::test_support::KEY));
            if incomplete {
                assert!(events.iter().any(
                    |event| matches!(event,Ok(ModelEvent::Usage(usage)) if usage.total_tokens == 14)
                ));
            }
            if quota {
                assert!(
                    events
                        .last()
                        .unwrap()
                        .as_ref()
                        .unwrap_err()
                        .to_string()
                        .contains("https://chatgpt.com/settings/usage")
                );
            }
        }
    }

    #[tokio::test]
    async fn temporary_errors_retry_only_before_output_or_usage() {
        use crate::test_support::{Reply, Server};
        for prefix in [
            vec![],
            vec![
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"message","id":"m1","content":[]}}),
                json!({"type":"response.output_text.delta","output_index":0,"item_id":"m1","content_index":0,"delta":"partial"}),
            ],
            vec![
                json!({"type":"response.output_item.added","output_index":0,"item":{"type":"function_call","id":"f1","arguments":""}}),
                json!({"type":"response.function_call_arguments.delta","output_index":0,"item_id":"f1","delta":"{"}),
            ],
        ] {
            let retryable = prefix.is_empty();
            let mut events = prefix;
            events.push(json!({"type":"response.failed","response":{"error":{"code":"subscription_sharing_usage_unavailable","message":crate::test_support::KEY}}}));
            let server = Server::start(vec![Reply::sse(sse(events)),Reply::sse(sse(vec![json!({"type":"response.completed","response":{"status":"completed","output":[]}})]))]).await;
            let model = Responses::with_endpoint(
                Arc::new(Auth),
                format!("{}responses", server.base_url).parse().unwrap(),
            )
            .unwrap();
            let events: Vec<_> = model
                .stream(
                    Request {
                        model: "test-model".into(),
                        options: crate::RequestOptions {
                            max_retries: Some(1),
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    CancellationToken::new(),
                )
                .collect()
                .await;
            let count = server.requests.lock().unwrap().len();
            server.stop().await;
            assert_eq!(count, if retryable { 2 } else { 1 });
            assert_eq!(
                events
                    .iter()
                    .any(|event| matches!(event, Ok(ModelEvent::Retrying { .. }))),
                retryable
            );
            assert!(!format!("{events:?}").contains(crate::test_support::KEY));
        }
    }

    #[tokio::test]
    async fn two_tool_rounds_replay_order_phase_ciphertext_and_raw_arguments() {
        use crate::test_support::{Reply, Server};
        use crate::{AssistantContent, ToolDefinition};
        let outputs:Vec<_> = (0..2).map(|index| json!([
            {"type":"reasoning","id":format!("r{index}"),"summary":[],"encrypted_content":format!("test-only-cipher-{index}")},
            {"type":"message","id":format!("m{index}"),"role":"assistant","status":"completed","phase":"commentary","content":[{"type":"output_text","text":format!("read {index}"),"annotations":[]}]},
            {"type":"function_call","id":format!("fc{index}"),"call_id":format!("call{index}"),"namespace":"canter","name":"read","arguments":"{ \"path\" : \"a\" }","status":"completed"}
        ])).collect();
        let replies = outputs.iter().chain(std::iter::once(&json!([]))).map(|output| {
            let mut events: Vec<_> = output.as_array().unwrap().iter().enumerate().map(|(index, item)| {
                json!({"type":"response.output_item.done","output_index":index,"item":item})
            }).collect();
            events.push(json!({"type":"response.completed","response":{"status":"completed","output":[]}}));
            Reply::sse(sse(events))
        }).collect();
        let server = Server::start(replies).await;
        let model = Responses::with_endpoint(
            Arc::new(Auth),
            format!("{}responses", server.base_url).parse().unwrap(),
        )
        .unwrap();
        let mut request = Request {
            model: "test-model".into(),
            messages: vec![Message::User {
                text: "Read twice".into(),
            }],
            tools: vec![ToolDefinition {
                name: "read".into(),
                description: "Read".into(),
                parameters: json!({}),
            }],
            ..Default::default()
        };
        for round in 0..3 {
            let events: Vec<_> = model
                .stream(request.clone(), CancellationToken::new())
                .collect()
                .await;
            assert!(events.iter().all(Result::is_ok));
            assert!(!format!("{events:?}").contains("test-only-cipher-"));
            let mut content = Vec::new();
            let mut replay = None;
            for event in events.into_iter().map(Result::unwrap) {
                match event {
                    ModelEvent::ItemCompleted { content: part, .. } => content.push(part),
                    ModelEvent::Replay(value) => replay = Some(value),
                    _ => {}
                }
            }
            if round < 2 {
                assert!(replay.is_some());
                assert!(content.iter().any(|part| matches!(part, AssistantContent::ToolCall(call) if call.arguments == "{ \"path\" : \"a\" }")));
                request
                    .messages
                    .push(Message::Assistant { content, replay });
                request.messages.push(Message::ToolResult {
                    tool_call_id: format!("call{round}"),
                    text: format!("result {round}"),
                });
            }
        }
        let bodies = server.requests.lock().unwrap().clone();
        server.stop().await;
        let input = bodies[2]["input"].as_array().unwrap();
        assert!(input[1..4] == outputs[0].as_array().unwrap()[..]);
        assert!(input[5..8] == outputs[1].as_array().unwrap()[..]);
        assert_eq!(
            input[4],
            json!({"type":"function_call_output","call_id":"call0","output":"result 0"})
        );
        assert_eq!(
            input[8],
            json!({"type":"function_call_output","call_id":"call1","output":"result 1"})
        );
    }
}
