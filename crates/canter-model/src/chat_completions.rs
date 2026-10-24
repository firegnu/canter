use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, SystemTime};

use eventsource_stream::{EventStreamError, Eventsource};
use futures_util::StreamExt;
use reqwest::{
    Client, Url,
    header::{AUTHORIZATION, HeaderValue},
};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{
    ErrorKind, FinishReason, Model, ModelError, ModelEvent, ModelStream, Request, ToolCall, wire,
};

/// A local credential. No serialization or public access to the secret.
#[derive(Clone)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey([REDACTED])")
    }
}

impl fmt::Display for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

pub struct ChatCompletions {
    client: Client,
    endpoint: Url,
    authorization: HeaderValue,
}

impl fmt::Debug for ChatCompletions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChatCompletions").finish_non_exhaustive()
    }
}

impl ChatCompletions {
    pub fn new(base_url: &str, key: ApiKey) -> Result<Self, ModelError> {
        let endpoint = Url::parse(&format!(
            "{}/chat/completions",
            base_url.trim_end_matches('/')
        ))
        .map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(ModelError::new(ErrorKind::InvalidRequest));
        }
        let mut authorization = HeaderValue::from_str(&format!("Bearer {}", key.0))
            .map_err(|_| ModelError::new(ErrorKind::Authentication))?;
        authorization.set_sensitive(true);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(300))
            .build()
            .map_err(|_| ModelError::new(ErrorKind::Other))?;
        Ok(Self {
            client,
            endpoint,
            authorization,
        })
    }

    fn attempt(&self, body: Value) -> ModelStream<'_> {
        Box::pin(async_stream::try_stream! {
            let response = self.client.post(self.endpoint.clone())
                .header(AUTHORIZATION, self.authorization.clone())
                .json(&body).send().await.map_err(transport_error)?;
            let response = if response.status().is_success() { response }
                else { Err(http_error(response).await)? };
            let mut events = response.bytes_stream().eventsource();
            let mut calls = BTreeMap::<u32, ToolCall>::new();
            let mut finish = None;
            loop {
                let event = events.next().await.ok_or_else(|| ModelError::new(ErrorKind::Network))?
                    .map_err(|error| match error {
                        EventStreamError::Transport(error) => transport_error(error),
                        _ => ModelError::new(ErrorKind::Other),
                    })?;
                if event.data == "[DONE]" {
                    let reason = finish.ok_or_else(|| ModelError::new(ErrorKind::Network))?;
                    if reason == FinishReason::ToolCalls {
                        for call in calls.into_values() {
                            if call.id.is_empty() || call.name.is_empty() {
                                Err(ModelError::new(ErrorKind::Other))?;
                            }
                            yield ModelEvent::ToolCall(call);
                        }
                    }
                    yield ModelEvent::Finished(reason);
                    break;
                }
                let chunk: wire::Chunk = serde_json::from_str(&event.data)
                    .map_err(|_| ModelError::new(ErrorKind::Other))?;
                for choice in chunk.choices {
                    if choice.index != 0 { continue; }
                    if let Some(text) = choice.delta.reasoning_content.filter(|s| !s.is_empty()) {
                        yield ModelEvent::ReasoningDelta(text);
                    }
                    if let Some(text) = choice.delta.content.filter(|s| !s.is_empty()) {
                        yield ModelEvent::TextDelta(text);
                    }
                    for delta in choice.delta.tool_calls.into_iter().flatten() {
                        let call = calls.entry(delta.index).or_insert_with(|| ToolCall {
                            id: String::new(), name: String::new(), arguments: String::new()
                        });
                        if call.id.is_empty() && let Some(id) = delta.id {
                            call.id = id;
                        }
                        if let Some(function) = delta.function {
                            if call.name.is_empty() && let Some(name) = function.name {
                                call.name = name;
                            }
                            if let Some(arguments) = function.arguments { call.arguments.push_str(&arguments); }
                        }
                    }
                    if let Some(reason) = choice.finish_reason {
                        finish = Some(match reason.as_str() {
                            "stop" => FinishReason::Stop,
                            "tool_calls" => FinishReason::ToolCalls,
                            "length" => FinishReason::Length,
                            "insufficient_system_resource" | "aborted" => Err(ModelError::new(ErrorKind::Server))?,
                            "network_error" => Err(ModelError::new(ErrorKind::Network))?,
                            _ => FinishReason::Other,
                        });
                    }
                }
                if let Some(usage) = chunk.usage {
                    yield ModelEvent::Usage(usage.into());
                }
            }
        })
    }
}

impl Model for ChatCompletions {
    fn stream(&self, request: Request, cancellation: CancellationToken) -> ModelStream<'_> {
        Box::pin(async_stream::stream! {
            if cancellation.is_cancelled() { return; }
            let body = wire::request_body(&request);
            let mut emitted = false;
            for retry in 0..=3 {
                let mut attempt = self.attempt(body.clone());
                let mut retry_delay = None;
                loop {
                    let event = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        event = attempt.next() => event,
                    };
                    if cancellation.is_cancelled() { return; }
                    let Some(event) = event else { break; };
                    match event {
                        Ok(event) => {
                            emitted = true;
                            yield Ok(event);
                        }
                        Err(error) => {
                            if !emitted && error.is_retryable() && retry < 3
                                && error.retry_after.is_none_or(|delay| delay <= Duration::from_secs(60)) {
                                retry_delay = Some(error.retry_after.unwrap_or_else(|| {
                                    Duration::from_millis(250 * (1 << retry)).min(Duration::from_secs(1))
                                }));
                                break;
                            }
                            yield Err(error);
                            return;
                        }
                    }
                }
                drop(attempt);
                match retry_delay {
                    Some(delay) => tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return,
                        _ = tokio::time::sleep(delay) => {}
                    },
                    None => return,
                }
            }
        })
    }
}

fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()
                .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
        })
}

async fn http_error(response: reqwest::Response) -> ModelError {
    let status = response.status().as_u16();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(parse_retry_after);
    let mut kind = match status {
        401 | 403 => ErrorKind::Authentication,
        400 | 422 => ErrorKind::InvalidRequest,
        408 => ErrorKind::Timeout,
        429 => ErrorKind::RateLimited,
        500..=599 => ErrorKind::Server,
        _ => ErrorKind::Other,
    };
    if matches!(status, 400 | 422) {
        // Inspect only for classification. Never retain or print a remote error,
        // which may echo the credential or arbitrary request headers.
        if let Ok(body) = response.json::<Value>().await {
            let error = &body["error"];
            let code = error["code"].as_str().unwrap_or_default();
            let message = error["message"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if code == "context_length_exceeded"
                || code == "context_window_exceeded"
                || message.contains("maximum context length")
                || message.contains("context length exceeded")
                || message.contains("context window exceeded")
            {
                kind = ErrorKind::ContextLength;
            }
        }
    }
    ModelError {
        kind,
        status: Some(status),
        retry_after,
    }
}

fn transport_error(error: reqwest::Error) -> ModelError {
    ModelError::new(if error.is_timeout() {
        ErrorKind::Timeout
    } else if error.is_builder() {
        ErrorKind::InvalidRequest
    } else {
        ErrorKind::Network
    })
}
