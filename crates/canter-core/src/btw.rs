use canter_model::{AssistantContent, Message, ModelEvent, Request};
use canter_protocol::{BtwEvent, ErrorCode};
use futures_util::StreamExt;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::{CoreError, Session, context, runner, session::Shared};

pub(crate) struct ActiveBtw {
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
        let mut state = self.shared.state.lock().await;
        state.available()?;
        if state.btw.is_some() {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "a btw answer is already in progress",
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
            cancel: cancel.clone(),
            done: done_receiver,
        });
        let shared = self.shared.clone();
        let run_cancel = cancel.clone();
        tokio::spawn(async move {
            let result = answer(&shared, request, &run_cancel, &sender).await;
            let mut state = shared.state.lock().await;
            state.btw = None;
            // Provider errors belong only to the private answer. Close checks
            // journal failures separately, after this worker has finished.
            let _ = done.send(Some(Ok(state.journal.snapshot.last_event_seq)));
            let event = match result {
                Err(error) => BtwEvent::Error {
                    message: error.message,
                },
                Ok(()) if run_cancel.is_cancelled() => BtwEvent::Cancelled,
                Ok(()) => BtwEvent::Completed,
            };
            let _ = sender.send(event);
        });
        Ok(BtwStream { receiver, cancel })
    }
}

async fn answer(
    shared: &Shared,
    request: Request,
    cancel: &CancellationToken,
    sender: &mpsc::UnboundedSender<BtwEvent>,
) -> Result<(), CoreError> {
    let (request, _) = crate::images::prepare_request(shared, request).await?;
    if cancel.is_cancelled() {
        return Ok(());
    }
    let model = request.model.clone();
    let started_at = (shared.options.request_time)();
    let mut stream = shared.model.stream(request, cancel.clone());
    let mut usage = None;
    let result = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break Ok(()),
            event = stream.next() => match event {
                Some(Ok(ModelEvent::TextDelta(delta))) => {
                    if sender.send(BtwEvent::Delta { delta }).is_err() {
                        cancel.cancel();
                    }
                }
                Some(Ok(ModelEvent::Usage(value))) => usage = Some(value),
                // Tool definitions stay in the prefix, but no tool is ever executed.
                Some(Ok(ModelEvent::Finished(_))) => break Ok(()),
                Some(Ok(ModelEvent::ToolCall(_) | ModelEvent::ReasoningDelta(_))) => {},
                Some(Err(error)) => break Err(CoreError::new(ErrorCode::Internal, &error.to_string())),
                None => break Err(CoreError::new(ErrorCode::Internal, "model stream ended without a finish reason")),
            }
        }
    };
    if let Some(usage) = usage {
        runner::record_usage(shared, usage, None, &model, started_at, false).await?;
    }
    result
}
