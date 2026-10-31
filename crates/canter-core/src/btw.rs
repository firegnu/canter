use canter_model::{AssistantContent, Message, ModelEvent, Request};
use canter_protocol::{BtwEvent, ErrorCode};
use futures_util::StreamExt;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::{
    CoreError, Session,
    compaction::{RULES, SECTIONS},
    context, runner,
    session::Shared,
};

const HANDOFF_PROMPT: &str = r###"Write a handoff note for a new session that will continue this work. The new session starts in the same project with the same instructions but sees none of this conversation. The user will read the note, may edit it, and then sends it as the new session's first message.

Start with a "## Task" heading that states the task below as an instruction to the new session. Then use exactly these headings, in this order:"###;

pub(crate) struct ActiveBtw {
    pub handoff: bool,
    pub cancel: CancellationToken,
    pub done: watch::Receiver<Option<Result<u64, CoreError>>>,
}

/// Dropping the private response cancels only this question, including on disconnect.
pub struct BtwStream {
    receiver: mpsc::UnboundedReceiver<BtwEvent>,
    cancel: CancellationToken,
}
impl BtwStream {
    pub async fn recv(&mut self) -> Option<BtwEvent> {
        self.receiver.recv().await
    }
}
impl Drop for BtwStream {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

impl Session {
    pub async fn btw(&self, question: String) -> Result<BtwStream, CoreError> {
        if question.trim().is_empty() {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "usage: /btw <question>",
            ));
        }
        self.private_answer(question, false).await
    }

    pub async fn handoff(&self, direction: Option<String>) -> Result<BtwStream, CoreError> {
        let direction = direction
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("Continue the current task.");
        let prompt = format!(
            r#"{HANDOFF_PROMPT}

{SECTIONS}

{RULES}
- Under "Where it stopped", describe the state of the working tree: what is changed, uncommitted, or committed.
- Under "Next steps", put first what the new session should check before changing anything, such as git status or rerunning a failing test.
- Output only the note, without a preamble.

Task for the new session:
{direction}"#
        );
        self.private_answer(prompt, true).await
    }

    async fn private_answer(
        &self,
        question: String,
        handoff: bool,
    ) -> Result<BtwStream, CoreError> {
        let mut state = self.shared.state.lock().await;
        state.available()?;
        if state.btw.is_some() {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "a btw answer is already in progress",
            ));
        }
        if handoff && (state.active.is_some() || !state.requests.is_empty()) {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Wait for this turn to finish or press Esc to interrupt before /handoff",
            ));
        }
        if handoff && state.history.is_empty() {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "No conversation to hand off",
            ));
        }
        let mut request = context::request(&state, self.shared.options.max_output_tokens);
        // A tool group is indivisible: keep it only once every call has a result.
        if let Some(index) = request
            .messages
            .iter()
            .rposition(|message| matches!(message, Message::Assistant { .. }))
        {
            let Message::Assistant { content } = &request.messages[index] else {
                unreachable!()
            };
            let missing = content.iter().any(|part| {
                let AssistantContent::ToolCall(call) = part else { return false; };
                !request.messages[index + 1..].iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, .. } if tool_call_id == &call.id))
            });
            if missing {
                request.messages.truncate(index);
            }
        }
        request.messages.push(Message::User { text: question });
        let cancel = self.shared.lifetime.child_token();
        let (sender, receiver) = mpsc::unbounded_channel();
        let (done, done_receiver) = watch::channel(None);
        state.btw = Some(ActiveBtw {
            handoff,
            cancel: cancel.clone(),
            done: done_receiver,
        });
        let shared = self.shared.clone();
        let run_cancel = cancel.clone();
        tokio::spawn(async move {
            let result = answer(&shared, request, handoff, &run_cancel, &sender).await;
            let mut state = shared.state.lock().await;
            state.btw = None;
            let event = match result {
                Err(error) => BtwEvent::Error {
                    message: error.message,
                },
                Ok(()) if run_cancel.is_cancelled() => BtwEvent::Cancelled,
                Ok(()) => BtwEvent::Completed,
            };
            let _ = sender.send(event);
            // End the private stream before telemetry I/O, so a final answer
            // does not keep the client's "btw in progress" guard active.
            // Hold the state lock until the write finishes: new work and close
            // cannot release the writer early. Close reports journal failures.
            let _ = state.journal.flush_private_telemetry().await;
            let _ = done.send(Some(Ok(state.journal.snapshot.last_event_seq)));
        });
        Ok(BtwStream { receiver, cancel })
    }
}

async fn answer(
    shared: &Shared,
    request: Request,
    handoff: bool,
    cancel: &CancellationToken,
    sender: &mpsc::UnboundedSender<BtwEvent>,
) -> Result<(), CoreError> {
    let (mut request, _) = crate::images::prepare_request(shared, request).await?;
    let model = request.model.clone();
    let mut confirmed = String::new();
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let mut started_at = (shared.options.request_time)();
        let mut goal_id = shared.state.lock().await.charge_goal();
        let (prepared, estimate) = if shared.options.telemetry {
            context::measure(request.clone()).await?
        } else {
            (request.clone(), Default::default())
        };
        let mut stream = crate::telemetry::stream(
            shared,
            None,
            if handoff { "handoff" } else { "btw" },
            prepared,
            estimate,
            cancel,
        )
        .await;
        let request_id = stream.request_id().map(str::to_owned);
        let mut response = crate::response::Response::default();
        let mut usage = None;
        let result = loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => break Ok(false),
                event = stream.next() => event,
            };
            match event {
                Some(Ok(event @ ModelEvent::TextDelta { .. })) => {
                    response.apply(&event);
                    let ModelEvent::TextDelta { delta, .. } = event else {
                        unreachable!()
                    };
                    if sender.send(BtwEvent::Delta { delta }).is_err() {
                        cancel.cancel();
                    }
                }
                Some(Ok(event @ ModelEvent::ItemCompleted { .. })) => {
                    response.apply(&event);
                    let _ = sender.send(BtwEvent::Replace {
                        text: format!("{confirmed}{}", response.visible_text()),
                        interrupted: false,
                    });
                }
                Some(Ok(ModelEvent::Retrying {
                    error,
                    attempt,
                    max_retries,
                    delay,
                })) => {
                    response = crate::response::Response::default();
                    let _ = sender.send(BtwEvent::Replace {
                        text: confirmed.clone(),
                        interrupted: true,
                    });
                    let _ = sender.send(BtwEvent::Retrying {
                        message: canter_protocol::model_retry_message(
                            &error.retry_reason(),
                            attempt,
                            max_retries,
                            delay.as_secs_f64(),
                        ),
                    });
                    if let Some(usage) = usage.take() {
                        runner::record_usage(
                            shared,
                            usage,
                            None,
                            &model,
                            started_at,
                            false,
                            goal_id.as_deref(),
                            request_id.as_deref(),
                        )
                        .await?;
                    }
                    if shared.state.lock().await.goal_summary.is_some() {
                        break Ok(false);
                    }
                    started_at = (shared.options.request_time)();
                    goal_id = shared.state.lock().await.charge_goal();
                }
                Some(Ok(ModelEvent::Usage(value))) => usage = Some(value),
                Some(Ok(ModelEvent::Finished(_))) => break Ok(false),
                // Definitions stay in the prefix; side requests never execute tools.
                Some(Ok(event @ (ModelEvent::ToolCall(_) | ModelEvent::ReasoningDelta { .. }))) => {
                    response.apply(&event);
                }
                Some(Err(error)) if error.is_retryable() && !response.completed().is_empty() => {
                    break Ok(true);
                }
                Some(Err(error)) => {
                    break Err(CoreError::new(ErrorCode::Internal, &error.to_string()));
                }
                None => {
                    break Err(CoreError::new(
                        ErrorCode::Internal,
                        "model stream ended without a finish reason",
                    ));
                }
            }
        };
        drop(stream);
        if let Some(usage) = usage {
            runner::record_usage(
                shared,
                usage,
                None,
                &model,
                started_at,
                false,
                goal_id.as_deref(),
                request_id.as_deref(),
            )
            .await?;
        }
        if !result? || shared.state.lock().await.goal_summary.is_some() {
            return Ok(());
        }
        let content = response.completed();
        // A side request must not leave a tool call without a result in its
        // follow-up request. Preserve its completed text, but never run tools.
        if content
            .iter()
            .any(|part| matches!(part, AssistantContent::ToolCall(_)))
        {
            return Err(CoreError::new(
                ErrorCode::Internal,
                "side request returned a tool call",
            ));
        }
        confirmed.push_str(&response.text());
        let _ = sender.send(BtwEvent::Replace {
            text: confirmed.clone(),
            interrupted: true,
        });
        request.messages.push(Message::Assistant { content });
    }
}
