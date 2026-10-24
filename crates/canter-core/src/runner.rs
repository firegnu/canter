use crate::{
    CoreError, compaction,
    context::{self, HistoryEntry},
    session::{Done, Shared, id},
};
use canter_model::{
    AssistantContent, ErrorKind, FinishReason, Message, ModelError, ModelEvent, Request,
    RequestOptions, ToolCall, ToolDefinition, Usage,
};
use canter_protocol::*;
use canter_tools::{PermissionCheck, PreparedCall, Tool, ToolOutcome, ToolOutput};
use futures_util::StreamExt;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(shared: Arc<Shared>, mut done: Done) {
    loop {
        let (turn, cancel, manual_compaction) = {
            let state = shared.state.lock().await;
            let active = state.active.as_ref().expect("active turn");
            (
                active.turn.clone(),
                active.cancel.clone(),
                active.manual_compaction,
            )
        };
        let result = if manual_compaction {
            match refresh_prefix(&shared).await {
                Ok(()) => compaction::compact(&shared, &turn.id, &cancel).await,
                Err(error) => Err(error),
            }
        } else {
            conversation(&shared, &turn.id, &cancel).await
        };
        let mut state = shared.state.lock().await;
        // An input accepted just as the model stopped still belongs to this turn.
        if result.is_ok() && !cancel.is_cancelled() && !state.steers.is_empty() {
            continue;
        }
        let mut completed = turn;
        completed.usage = state
            .active
            .as_ref()
            .and_then(|active| active.turn.usage.clone());
        completed.status = if cancel.is_cancelled() {
            TurnStatus::Interrupted
        } else if result.is_err() {
            TurnStatus::Failed
        } else {
            TurnStatus::Completed
        };
        completed.error = if cancel.is_cancelled() {
            None
        } else {
            result.err().map(|error| ErrorInfo {
                message: error.message,
                additional_details: None,
            })
        };
        let finish = async {
            state.cancel_requests().await?;
            state
                .complete_missing_tools("没有结果：被打断或本轮未完成")
                .await?;
            if cancel.is_cancelled() {
                state
                    .message(Message::User {
                        text: "[本轮被打断；尚未完成的操作没有继续执行。]".into(),
                    })
                    .await?;
                let reason = if state.closing {
                    InputRemovalReason::Closed
                } else {
                    InputRemovalReason::Interrupted
                };
                state.discard_queue(reason).await?;
            } else if completed.status == TurnStatus::Failed {
                // Accepted steering belongs to this turn even if the model
                // failed; preserve it in context before exposing the failure.
                state.consume_steering(&completed.id).await?;
            }
            if let Some(error) = &completed.error {
                state
                    .journal
                    .append(vec![EventPayload::Error {
                        error: error.clone(),
                        will_retry: false,
                        turn_id: Some(completed.id.clone()),
                    }])
                    .await?;
            }
            state
                .journal
                .append(vec![
                    EventPayload::TurnCompleted { turn: completed },
                    EventPayload::SessionStatus {
                        status: SessionStatus::Idle,
                    },
                ])
                .await
                .map(|events| events[0].seq)
        }
        .await;
        state.active = None;
        let _ = done.send(Some(finish.clone()));
        if finish.is_err() || cancel.is_cancelled() || state.closing {
            break;
        }
        let Some(input) = state.queue.pop_front() else {
            break;
        };
        let params = SessionInputParams {
            input: input.input,
            mode: InputMode::Queue,
            client_name: input.client_name,
        };
        match shared.start(&mut state, params, Some(input.id)).await {
            Ok((_, next)) => done = next,
            Err(_) => break,
        }
    }
}

async fn conversation(
    shared: &Shared,
    turn_id: &str,
    cancel: &CancellationToken,
) -> Result<(), CoreError> {
    let mut compacted = false;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        refresh_prefix(shared).await?;
        let request = {
            let mut state = shared.state.lock().await;
            state.consume_steering(turn_id).await?;
            Request {
                model: state.journal.snapshot.settings.model.clone(),
                messages: state.prefix.iter().chain(&state.history).cloned().collect(),
                tools: Tool::ALL
                    .into_iter()
                    .map(|tool| {
                        let definition = tool.definition();
                        ToolDefinition {
                            name: definition.name.into(),
                            description: definition.description.into(),
                            parameters: definition.parameters,
                        }
                    })
                    .collect(),
                options: RequestOptions {
                    thinking: Some(shared.options.thinking),
                    max_output_tokens: Some(shared.options.max_output_tokens),
                },
            }
        };
        let estimated = context::request_tokens(&request);
        let occupancy =
            shared
                .state
                .lock()
                .await
                .measured_context
                .map_or(estimated, |(actual, before)| {
                    estimated.max(actual.saturating_add(estimated.saturating_sub(before)))
                });
        let fits = occupancy.saturating_add(u64::from(shared.options.max_output_tokens))
            <= shared.options.context_window;
        if occupancy > compaction::input_limit(shared) && !compacted {
            compacted = true;
            match compaction::compact(shared, turn_id, cancel).await {
                Ok(()) => continue,
                Err(_) if cancel.is_cancelled() => return Ok(()),
                Err(error) if !fits => {
                    return Err(CoreError::new(
                        ErrorCode::InvalidRequest,
                        &format!(
                            "automatic compaction failed ({}); request plus output limit exceeds the context window",
                            error.message
                        ),
                    ));
                }
                Err(error) => {
                    shared
                        .state
                        .lock()
                        .await
                        .journal
                        .append(vec![EventPayload::Warning {
                            message: format!(
                                "自动压缩失败（{}）；原请求加输出上限仍在上下文窗口内，继续对话。",
                                error.message
                            ),
                        }])
                        .await?;
                }
            }
        }
        if !fits {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "context remains too large after compaction",
            ));
        }
        let (content, reason, error) = model_response(shared, turn_id, request, cancel).await?;
        let calls: Vec<_> = content
            .iter()
            .filter_map(|part| {
                if let AssistantContent::ToolCall(call) = part {
                    Some(call.clone())
                } else {
                    None
                }
            })
            .collect();
        if cancel.is_cancelled() {
            return Ok(());
        }
        if let Some(error) = error {
            shared
                .state
                .lock()
                .await
                .complete_missing_tools("没有结果：模型请求未完成")
                .await?;
            if error.kind == ErrorKind::ContextLength && !compacted {
                compaction::compact(shared, turn_id, cancel).await?;
                compacted = true;
                continue;
            }
            return Err(CoreError {
                code: ErrorCode::Internal,
                message: error.to_string(),
            });
        }
        if reason.is_none() {
            return Err(CoreError::new(
                ErrorCode::Internal,
                "model stream ended without a finish reason",
            ));
        }
        if reason != Some(FinishReason::ToolCalls) {
            shared
                .state
                .lock()
                .await
                .complete_missing_tools("没有结果：模型回复不完整，未执行工具")
                .await?;
            if matches!(reason, Some(FinishReason::Length | FinishReason::Other)) {
                shared
                    .state
                    .lock()
                    .await
                    .journal
                    .append(vec![EventPayload::Warning {
                        message: "模型回复不完整，未执行其中的工具调用。".into(),
                    }])
                    .await?;
            }
            return Ok(());
        }
        if calls.is_empty() {
            return Ok(());
        }
        for call in calls {
            if cancel.is_cancelled() {
                return Ok(());
            }
            execute(shared, turn_id, call, cancel).await?;
        }
        compacted = false;
    }
}

async fn refresh_prefix(shared: &Shared) -> Result<(), CoreError> {
    let cwd = shared
        .state
        .lock()
        .await
        .journal
        .snapshot
        .settings
        .cwd
        .clone();
    let (prefix, warnings) = context::prefix(cwd).await?;
    let mut state = shared.state.lock().await;
    if state.prefix != prefix {
        state
            .journal
            .record(HistoryEntry::Context {
                messages: prefix.clone(),
            })
            .await?;
        state.prefix = prefix;
    }
    if !warnings.is_empty() {
        state
            .journal
            .append(
                warnings
                    .into_iter()
                    .map(|message| EventPayload::Warning { message })
                    .collect(),
            )
            .await?;
    }
    Ok(())
}

async fn model_response(
    shared: &Shared,
    turn_id: &str,
    request: Request,
    cancel: &CancellationToken,
) -> Result<
    (
        Vec<AssistantContent>,
        Option<FinishReason>,
        Option<ModelError>,
    ),
    CoreError,
> {
    let estimated = context::request_tokens(&request);
    let mut stream = shared.model.stream(request, cancel.clone());
    let mut content = Vec::new();
    let mut text = String::new();
    let mut reasoning = String::new();
    let text_id = id("message");
    let reasoning_id = id("reasoning");
    let mut pending = Vec::new();
    let mut timer = tokio::time::interval(Duration::from_millis(20));
    let mut reason = None;
    let mut error = None;
    let mut usage = None;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = timer.tick(), if !pending.is_empty() => { flush(shared, &mut pending).await?; }
            event = stream.next() => match event {
                Some(Ok(ModelEvent::TextDelta(delta))) => {
                    if delta.is_empty() { continue; }
                    if text.is_empty() { pending.push(EventPayload::ItemStarted { turn_id: turn_id.into(), item: Item::AgentMessage { id: text_id.clone(), text: String::new() } }); }
                    text.push_str(&delta);
                    if let Some(AssistantContent::Text { text }) = content.last_mut() { text.push_str(&delta); }
                    else { content.push(AssistantContent::Text { text: delta.clone() }); }
                    pending.push(EventPayload::ItemDelta { turn_id: turn_id.into(), item_id: text_id.clone(), delta: ItemDelta::AgentMessage { delta } });
                }
                Some(Ok(ModelEvent::ReasoningDelta(delta))) => {
                    if delta.is_empty() { continue; }
                    if reasoning.is_empty() { pending.push(EventPayload::ItemStarted { turn_id: turn_id.into(), item: Item::Reasoning { id: reasoning_id.clone(), summary: Vec::new(), content: Vec::new() } }); }
                    reasoning.push_str(&delta);
                    if let Some(AssistantContent::Reasoning { text }) = content.last_mut() { text.push_str(&delta); }
                    else { content.push(AssistantContent::Reasoning { text: delta.clone() }); }
                    pending.push(EventPayload::ItemDelta { turn_id: turn_id.into(), item_id: reasoning_id.clone(), delta: ItemDelta::ReasoningContent { content_index: 0, delta } });
                }
                Some(Ok(ModelEvent::ToolCall(call))) => content.push(AssistantContent::ToolCall(call)),
                Some(Ok(ModelEvent::Usage(value))) => usage = Some(value),
                Some(Ok(ModelEvent::Finished(finish))) => { reason = Some(finish); break; }
                Some(Err(value)) => { error = Some(value); break; }
                None => break,
            }
        }
        if pending.len() >= 128 {
            flush(shared, &mut pending).await?;
        }
    }
    // Stream deltas immediately, but make the full assistant message durable
    // before publishing either final text or final reasoning items.
    if !content.is_empty() {
        shared
            .state
            .lock()
            .await
            .message(Message::Assistant {
                content: content.clone(),
            })
            .await?;
    }
    if !text.is_empty() {
        pending.push(EventPayload::ItemCompleted {
            turn_id: turn_id.into(),
            item: Item::AgentMessage { id: text_id, text },
        });
    }
    if !reasoning.is_empty() {
        pending.push(EventPayload::ItemCompleted {
            turn_id: turn_id.into(),
            item: Item::Reasoning {
                id: reasoning_id,
                summary: Vec::new(),
                content: vec![reasoning],
            },
        });
    }
    flush(shared, &mut pending).await?;
    if let Some(usage) = usage {
        record_usage(shared, usage, Some(estimated)).await?;
    }
    Ok((content, reason, error))
}

pub(crate) async fn record_usage(
    shared: &Shared,
    usage: Usage,
    estimated: Option<u64>,
) -> Result<(), CoreError> {
    let mut state = shared.state.lock().await;
    let last = TokenUsageBreakdown {
        input_tokens: usage.input_tokens,
        cached_input_tokens: usage.cache_read_tokens.unwrap_or(0),
        output_tokens: usage.output_tokens,
        reasoning_output_tokens: usage.reasoning_tokens.unwrap_or(0),
        total_tokens: usage.total_tokens,
    };
    let mut total = state
        .journal
        .snapshot
        .token_usage
        .as_ref()
        .map(|usage| usage.total.clone())
        .unwrap_or_default();
    add_usage(&mut total, &last);
    let context_tokens = if let Some(estimated) = estimated {
        state.measured_context = Some((usage.input_tokens, estimated));
        usage.input_tokens.saturating_add(usage.output_tokens)
    } else {
        state
            .journal
            .snapshot
            .token_usage
            .as_ref()
            .map_or(0, |usage| usage.context_tokens)
    };
    let token_usage = TokenUsage {
        total,
        last: last.clone(),
        context_tokens,
        model_context_window: Some(shared.options.context_window),
    };
    if let Some(active) = &mut state.active {
        let mut total = active
            .turn
            .usage
            .as_ref()
            .map(|usage| usage.total.clone())
            .unwrap_or_default();
        add_usage(&mut total, &last);
        active.turn.usage = Some(TokenUsage {
            total,
            ..token_usage.clone()
        });
    }
    state
        .journal
        .append(vec![EventPayload::SessionUsage { token_usage }])
        .await?;
    Ok(())
}
fn add_usage(total: &mut TokenUsageBreakdown, last: &TokenUsageBreakdown) {
    total.input_tokens = total.input_tokens.saturating_add(last.input_tokens);
    total.cached_input_tokens = total
        .cached_input_tokens
        .saturating_add(last.cached_input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(last.output_tokens);
    total.reasoning_output_tokens = total
        .reasoning_output_tokens
        .saturating_add(last.reasoning_output_tokens);
    total.total_tokens = total.total_tokens.saturating_add(last.total_tokens);
}

async fn execute(
    shared: &Shared,
    turn_id: &str,
    call: ToolCall,
    cancel: &CancellationToken,
) -> Result<(), CoreError> {
    let Some(tool) = Tool::from_name(&call.name) else {
        let mut state = shared.state.lock().await;
        state
            .message(Message::ToolResult {
                tool_call_id: call.id,
                text: "Unknown tool".into(),
            })
            .await?;
        state
            .journal
            .append(vec![EventPayload::Warning {
                message: format!("未知工具 {}，没有执行。", call.name),
            }])
            .await?;
        return Ok(());
    };
    let output = match shared
        .tools
        .prepare(tool, id("tool"), &call.arguments, cancel)
        .await
    {
        Err(output) => output,
        Ok(prepared) => {
            let permission = permission(shared, turn_id, &prepared, cancel).await?;
            match permission {
                None => {
                    let mut item = prepared.started_item();
                    match &mut item {
                        Item::FileRead { status, .. }
                        | Item::FileChange { status, .. }
                        | Item::CommandExecution { status, .. } => *status = ItemStatus::Failed,
                        _ => unreachable!("prepared tool item"),
                    }
                    ToolOutput {
                        text: "被打断，没有执行".into(),
                        item,
                        outcome: ToolOutcome::Cancelled,
                    }
                }
                Some(PermissionCheck::Deny { cancel_turn }) => {
                    if cancel_turn {
                        cancel.cancel();
                    }
                    prepared.declined_output()
                }
                Some(PermissionCheck::Ask(_)) => unreachable!("approval resolved"),
                Some(PermissionCheck::Execute(permit)) => {
                    let (sender, mut receiver) = mpsc::unbounded_channel();
                    let mut on_output = |text: &str| {
                        let _ = sender.send(text.to_owned());
                    };
                    let execute = shared.tools.execute(permit, cancel, &mut on_output);
                    tokio::pin!(execute);
                    let mut pending = Vec::new();
                    let mut timer = tokio::time::interval(Duration::from_millis(20));
                    let mut write_error = None;
                    let output = loop {
                        tokio::select! {
                            output = &mut execute => break output,
                            Some(delta) = receiver.recv() => pending.push(EventPayload::ItemDelta { turn_id: turn_id.into(), item_id: prepared.started_item().id().unwrap().into(), delta: ItemDelta::CommandExecution { delta } }),
                            _ = timer.tick(), if !pending.is_empty() && write_error.is_none() => {
                                if let Err(error) = flush(shared, &mut pending).await { cancel.cancel(); write_error = Some(error); }
                            }
                        }
                    };
                    if let Some(error) = write_error {
                        return Err(error);
                    }
                    while let Ok(delta) = receiver.try_recv() {
                        pending.push(EventPayload::ItemDelta {
                            turn_id: turn_id.into(),
                            item_id: output.item.id().unwrap().into(),
                            delta: ItemDelta::CommandExecution { delta },
                        });
                    }
                    flush(shared, &mut pending).await?;
                    output
                }
            }
        }
    };
    let mut state = shared.state.lock().await;
    if output.outcome == ToolOutcome::Cancelled {
        cancel.cancel();
    }
    state
        .message(Message::ToolResult {
            tool_call_id: call.id,
            text: output.text,
        })
        .await?;
    state
        .journal
        .append(vec![EventPayload::ItemCompleted {
            turn_id: turn_id.into(),
            item: output.item,
        }])
        .await?;
    Ok(())
}
async fn permission<'a>(
    shared: &Shared,
    turn_id: &str,
    call: &'a PreparedCall,
    cancel: &CancellationToken,
) -> Result<Option<PermissionCheck<'a>>, CoreError> {
    let receiver = {
        let mut state = shared.state.lock().await;
        state
            .journal
            .append(vec![EventPayload::ItemStarted {
                turn_id: turn_id.into(),
                item: call.started_item(),
            }])
            .await?;
        match state
            .gate
            .evaluate(call, state.journal.snapshot.settings.approval_policy)
        {
            PermissionCheck::Ask(approval) => {
                let request = PendingRequest {
                    id: id("approval"),
                    turn_id: turn_id.into(),
                    created_at: crate::journal::now(),
                    kind: RequestKind::Approval(approval),
                };
                let (sender, receiver) = tokio::sync::oneshot::channel();
                state
                    .journal
                    .append(vec![
                        EventPayload::RequestOpened {
                            request: request.clone(),
                        },
                        EventPayload::SessionStatus {
                            status: SessionStatus::WaitingApproval,
                        },
                    ])
                    .await?;
                state.approvals.insert(request.id, sender);
                receiver
            }
            permission => return Ok(Some(permission)),
        }
    };
    let decision = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            shared.state.lock().await.cancel_requests().await?;
            return Ok(None);
        },
        decision = receiver => decision.map_err(|_| CoreError::internal())?,
    };
    shared
        .state
        .lock()
        .await
        .gate
        .resolve(call, decision)
        .map(Some)
        .map_err(|_| CoreError::new(ErrorCode::InvalidDecision, "invalid approval decision"))
}
async fn flush(shared: &Shared, pending: &mut Vec<EventPayload>) -> Result<(), CoreError> {
    if !pending.is_empty() {
        shared
            .state
            .lock()
            .await
            .journal
            .append(std::mem::take(pending))
            .await?;
    }
    Ok(())
}
