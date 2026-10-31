use crate::session::Shared;
use canter_model::{AssistantContent, ModelEvent, ModelStream, Request};
use canter_protocol::{
    CompactionTrigger, ContextEstimate, EventPayload, ModelRequestOutcome, ModelRequestTelemetry,
    ReasoningEffort, RequestPurpose, RequestSettings,
};
use futures_util::Stream;
use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
    time::Instant,
};
use tokio_util::sync::CancellationToken;

pub(crate) type Pending = Arc<Mutex<Records>>;

#[derive(Default)]
pub(crate) struct Records {
    pub events: Vec<EventPayload>,
    pub private_requests: Vec<ModelRequestTelemetry>,
}

pub(crate) struct Compaction {
    pub turn_id: String,
    pub item_id: String,
    pub trigger: CompactionTrigger,
    pub trigger_occupancy_tokens: Option<u64>,
    pub before: Request,
    pub after: Request,
    pub before_messages: usize,
    pub after_messages: usize,
    pub removed_messages: usize,
    pub succeeded: bool,
}

impl Compaction {
    /// Run on Journal's existing blocking writer, never on the streaming task.
    pub fn event(self) -> EventPayload {
        EventPayload::TelemetryCompaction {
            turn_id: self.turn_id,
            item_id: self.item_id,
            trigger: self.trigger,
            trigger_occupancy_tokens: self.trigger_occupancy_tokens,
            estimated: true,
            before_tokens: crate::context::request_tokens(&self.before),
            after_tokens: crate::context::request_tokens(&self.after),
            before_messages: self.before_messages,
            after_messages: self.after_messages,
            removed_messages: self.removed_messages,
            succeeded: self.succeeded,
        }
    }
}

pub(crate) fn millis(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

/// Only constant-size bookkeeping while polling. Dropping queues one record for
/// the next existing Journal batch; no I/O, history copy or serialization here.
pub(crate) struct TrackedStream<'a> {
    inner: ModelStream<'a>,
    trace: Option<(Pending, ModelRequestTelemetry, Instant)>,
    ended: bool,
}

impl TrackedStream<'_> {
    pub fn request_id(&self) -> Option<&str> {
        self.trace
            .as_ref()
            .map(|(_, record, _)| record.request_id.as_str())
    }
}

pub(crate) async fn stream<'a>(
    shared: &'a Shared,
    turn_id: Option<&str>,
    purpose: RequestPurpose,
    request: Request,
    context: ContextEstimate,
    cancel: &CancellationToken,
) -> TrackedStream<'a> {
    let trace = if shared.options.telemetry {
        let state = shared.state.lock().await;
        let settings = &state.journal.snapshot.settings;
        let effort = match request.options.thinking {
            Some(false) => ReasoningEffort::None,
            _ => match request.options.reasoning_effort.as_deref() {
                Some("low") => ReasoningEffort::Low,
                Some("max") => ReasoningEffort::Max,
                _ => ReasoningEffort::High,
            },
        };
        Some((
            state
                .journal
                .telemetry
                .as_ref()
                .expect("telemetry enabled")
                .clone(),
            ModelRequestTelemetry {
                request_id: crate::session::id("request"),
                turn_id: turn_id.map(Into::into),
                purpose,
                outcome: ModelRequestOutcome::Unknown,
                retry_count: Some(0),
                started_at: (shared.options.request_time)(),
                duration_ms: 0,
                first_response_ms: None,
                settings: RequestSettings {
                    model: request.model.clone(),
                    reasoning_effort: effort,
                    approval_policy: settings.approval_policy,
                    plan_mode: settings.plan_mode,
                },
                context,
                tool_call_ids: Vec::new(),
            },
            Instant::now(),
        ))
    } else {
        None
    };
    TrackedStream {
        inner: shared.model.stream(request, cancel.clone()),
        trace,
        ended: false,
    }
}

impl Stream for TrackedStream<'_> {
    type Item = <ModelStream<'static> as Stream>::Item;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = self.inner.as_mut().poll_next(cx);
        if let Poll::Ready(event) = &result {
            if let Some((_, record, start)) = &mut self.trace {
                if matches!(event, Some(Ok(ModelEvent::Retrying { .. }))) {
                    let count = record.retry_count.get_or_insert(0);
                    *count = count.saturating_add(1);
                }
                let first = match event {
                    Some(Ok(
                        ModelEvent::TextDelta { delta: text, .. }
                        | ModelEvent::ReasoningDelta { delta: text, .. }
                        | ModelEvent::ItemCompleted {
                            content:
                                AssistantContent::Text { text } | AssistantContent::Reasoning { text },
                            ..
                        },
                    )) => !text.is_empty(),
                    Some(Ok(
                        ModelEvent::ToolCall(_)
                        | ModelEvent::ItemCompleted {
                            content: AssistantContent::ToolCall(_),
                            ..
                        },
                    )) => true,
                    _ => false,
                };
                if first && record.first_response_ms.is_none() {
                    record.first_response_ms = Some(millis(*start));
                }
                let call = match event {
                    Some(Ok(
                        ModelEvent::ToolCall(call)
                        | ModelEvent::ItemCompleted {
                            content: AssistantContent::ToolCall(call),
                            ..
                        },
                    )) => Some(call),
                    _ => None,
                };
                if let Some(call) = call
                    && !record.tool_call_ids.contains(&call.id)
                {
                    record.tool_call_ids.push(call.id.clone());
                }
            }
            if !self.ended
                && matches!(
                    event,
                    None | Some(Err(_)) | Some(Ok(ModelEvent::Finished(_)))
                )
            {
                if let Some((_, record, start)) = &mut self.trace {
                    record.duration_ms = millis(*start);
                    record.outcome = if matches!(event, Some(Ok(ModelEvent::Finished(_)))) {
                        ModelRequestOutcome::Completed
                    } else {
                        ModelRequestOutcome::Failed
                    };
                }
                self.ended = true;
            }
        }
        result
    }
}

impl Drop for TrackedStream<'_> {
    fn drop(&mut self) {
        if let Some((pending, mut record, start)) = self.trace.take() {
            if !self.ended {
                record.duration_ms = millis(start);
                record.outcome = ModelRequestOutcome::Cancelled;
            }
            let mut pending = pending.lock().expect("telemetry queue");
            if record.turn_id.is_none() {
                pending.private_requests.push(record);
            } else {
                pending
                    .events
                    .push(EventPayload::TelemetryModelRequest { record });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    #[tokio::test]
    async fn records_completed_failed_eof_and_early_drop_outcomes() {
        for (expected, events, consume) in [
            (
                "completed",
                vec![Ok(ModelEvent::Finished(canter_model::FinishReason::Stop))],
                true,
            ),
            (
                "failed",
                vec![Err(canter_model::ModelError {
                    kind: canter_model::ErrorKind::Network,
                    status: None,
                    retry_after: None,
                    retries: 0,
                })],
                true,
            ),
            ("failed", vec![], true),
            ("cancelled", vec![], false),
        ] {
            let pending = Pending::default();
            let record = serde_json::from_value(serde_json::json!({
                "requestId":"r", "turnId":"t", "purpose":"conversation", "startedAt":1, "durationMs":0,
                "settings":{"model":"fake","approvalPolicy":"allowAll","planMode":false},
                "context":{"estimated":true,"systemTokens":0,"toolDefinitionTokens":0,"conversationTokens":0,"toolResultTokens":0}, "toolCallIds":[]
            })).unwrap();
            let mut stream = TrackedStream {
                inner: Box::pin(futures_util::stream::iter(events)),
                trace: Some((pending.clone(), record, Instant::now())),
                ended: false,
            };
            if consume {
                while stream.next().await.is_some() {}
            }
            drop(stream);
            let json = serde_json::to_value(&pending.lock().unwrap().events[0]).unwrap();
            assert_eq!(json["outcome"], expected);
        }
    }
}
