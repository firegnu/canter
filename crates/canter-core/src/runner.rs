use crate::{
    CoreError, compaction, context,
    session::{Done, Shared, id},
};
use canter_model::{
    AssistantContent, ErrorKind, FinishReason, Message, ModelError, ModelEvent, Request, ToolCall,
    Usage,
};
use canter_protocol::*;
use canter_tools::{PermissionCheck, PreparedCall, Tool, ToolOutcome, ToolOutput};
use futures_util::StreamExt;
use std::{sync::Arc, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(crate) async fn run(shared: Arc<Shared>, mut done: Done) {
    // Late steering may re-enter conversation without starting a new turn.
    let mut auto_compaction_failed = false;
    loop {
        let (turn, cancel, manual_compaction, user_shell) = {
            let state = shared.state.lock().await;
            let active = state.active.as_ref().expect("active turn");
            (
                active.turn.clone(),
                active.cancel.clone(),
                active.manual_compaction,
                active.user_shell.clone(),
            )
        };
        let result = if let Some(params) = user_shell {
            crate::user_shell::run(&shared, &turn.id, &params, &cancel).await
        } else if manual_compaction {
            compaction::compact(&shared, &turn.id, &cancel).await
        } else {
            conversation(&shared, &turn.id, &cancel, &mut auto_compaction_failed).await
        };
        let mut state = shared.state.lock().await;
        // An input accepted just as the model stopped still belongs to this turn.
        if result.is_ok()
            && !cancel.is_cancelled()
            && !state.steers.is_empty()
            && !state
                .active
                .as_ref()
                .is_some_and(|active| active.plan_handoff)
        {
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
            if state.active.as_ref().is_some_and(|active| active.retracted) {
                state.discard_queue(InputRemovalReason::Interrupted).await?;
                let event_seq = state.journal.snapshot.last_event_seq + 1;
                state
                    .journal
                    .record(context::HistoryEntry::Retract {
                        turn_id: completed.id.clone(),
                        event_seq,
                    })
                    .await?;
                let (messages, users) = state
                    .rewind
                    .retract(&completed.id)
                    .ok_or_else(CoreError::internal)?;
                let removed = state.history.split_off(messages);
                state.user_messages.truncate(users);
                state.measured_context = None;
                state
                    .journal
                    .append(vec![
                        EventPayload::TurnRetracted {
                            turn_id: completed.id,
                        },
                        EventPayload::SessionStatus {
                            status: SessionStatus::Idle,
                            close_reason: None,
                        },
                    ])
                    .await?;
                state.restate_background_notices(removed).await?;
                return Ok(event_seq);
            }
            state.cancel_requests().await?;
            state
                .complete_missing_tools("No result: interrupted, or the turn did not finish")
                .await?;
            if state
                .active
                .as_ref()
                .is_some_and(|active| active.plan_handoff)
            {
                state.discard_queue(InputRemovalReason::Interrupted).await?;
            }
            if cancel.is_cancelled() {
                state
                    .message(Message::User {
                        text:
                            "[This turn was interrupted; unfinished operations were not continued.]"
                                .into(),
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
                        close_reason: None,
                    },
                ])
                .await
                .map(|events| events[0].seq)
        }
        .await;
        state.active = None;
        let _ = done.send(Some(finish.clone()));
        if finish.is_err() || cancel.is_cancelled() || state.closing {
            state.background_events.interrupt();
            break;
        }
        let Some(input) = state.queue.pop_front() else {
            state.background_events.turn_finished();
            break;
        };
        let params = SessionInputParams {
            review: input.review,
            input: input.input,
            mode: InputMode::Queue,
            client_name: input.client_name,
        };
        match shared.start(&mut state, params, Some(input.id)).await {
            Ok((_, next)) => {
                done = next;
                auto_compaction_failed = false;
            }
            Err(_) => break,
        }
    }
}

async fn conversation(
    shared: &Shared,
    turn_id: &str,
    cancel: &CancellationToken,
    auto_compaction_failed: &mut bool,
) -> Result<(), CoreError> {
    let mut compacted = false;
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        shared.sync_background().await?;
        let request = {
            let mut state = shared.state.lock().await;
            state.consume_steering(turn_id).await?;
            state.background_cards(turn_id).await?;
            context::request(&state, shared.options.max_output_tokens)
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
        if occupancy > compaction::input_limit(shared) && !compacted && !*auto_compaction_failed {
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
                    *auto_compaction_failed = true;
                    shared
                        .state
                        .lock()
                        .await
                        .journal
                        .append(vec![EventPayload::Warning {
                            message: format!(
                                "Auto-compaction failed ({}); the request plus the output limit still fits in the context window, so the conversation continues.",
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
        shared
            .state
            .lock()
            .await
            .consume_background_notices(&request.messages);
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
        let interrupted_with_content =
            error.as_ref().is_some_and(|error| error.is_retryable()) && !content.is_empty();
        if let Some(error) = error.filter(|_| !interrupted_with_content) {
            shared
                .state
                .lock()
                .await
                .complete_missing_tools("No result: the model request did not finish")
                .await?;
            if error.kind == ErrorKind::ContextLength && !compacted && !*auto_compaction_failed {
                compaction::compact(shared, turn_id, cancel).await?;
                compacted = true;
                continue;
            }
            return Err(CoreError {
                code: ErrorCode::Internal,
                message: error.to_string(),
            });
        }
        if reason.is_none() && !interrupted_with_content {
            return Err(CoreError::new(
                ErrorCode::Internal,
                "model stream ended without a finish reason",
            ));
        }
        if reason != Some(FinishReason::ToolCalls) && !interrupted_with_content {
            shared
                .state
                .lock()
                .await
                .complete_missing_tools(
                    "No result: the model reply was incomplete, so the tool was not run",
                )
                .await?;
            if matches!(reason, Some(FinishReason::Length | FinishReason::Other)) {
                shared
                    .state
                    .lock()
                    .await
                    .journal
                    .append(vec![EventPayload::Warning {
                        message: "The model reply was incomplete; its tool calls were not run."
                            .into(),
                    }])
                    .await?;
            }
            return Ok(());
        }
        if calls.is_empty() && !interrupted_with_content {
            return Ok(());
        }
        for call in calls {
            if cancel.is_cancelled() {
                return Ok(());
            }
            execute(shared, turn_id, call, cancel).await?;
            if shared
                .state
                .lock()
                .await
                .active
                .as_ref()
                .is_some_and(|active| active.plan_handoff)
            {
                return Ok(());
            }
        }
        compacted = false;
    }
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
    let request = crate::images::prepare(shared, request).await?;
    let model = request.model.clone();
    let mut started_at = (shared.options.request_time)();
    let mut stream = shared.model.stream(request, cancel.clone());
    let mut response = crate::response::Response::default();
    let mut last_nonempty_response = crate::response::Response::default();
    let mut pending = Vec::new();
    let mut timer = tokio::time::interval(Duration::from_millis(20));
    let mut reason = None;
    let mut error = None;
    let mut usage = None;
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => break,
            _ = timer.tick(), if !pending.is_empty() => flush(shared, &mut pending).await?,
            event = stream.next() => match event {
                Some(Ok(ModelEvent::Retrying { error, attempt, max_retries, delay })) => {
                    finish_items(turn_id, &response, &mut pending, true);
                    flush(shared, &mut pending).await?;
                    if !response.items.is_empty() {
                        last_nonempty_response = std::mem::take(&mut response);
                    }
                    if let Some(usage) = usage.take() {
                        record_usage(shared, usage, None, &model, started_at, true).await?;
                    }
                    record_retry(shared, turn_id, &error, attempt, max_retries, delay).await?;
                    started_at = (shared.options.request_time)();
                }
                Some(Ok(ModelEvent::Usage(value))) => usage = Some(value),
                Some(Ok(ModelEvent::Finished(finish))) => {
                    response.apply(&ModelEvent::Finished(finish.clone()));
                    reason = Some(finish);
                    break;
                }
                Some(Ok(event)) => {
                    if matches!(&event, ModelEvent::TextDelta { delta, .. } | ModelEvent::ReasoningDelta { delta, .. } if delta.is_empty()) { continue; }
                    if response.items.is_empty() && !shared.accept_model_output().await { break; }
                    if let Some((item, new)) = response.apply(&event) {
                        if new && let Some(mut display) = item.display(false) {
                            // Deltas below supply the initial text; a completion-only item
                            // already has its authoritative content.
                            match (&event, &mut display) {
                                (ModelEvent::TextDelta { .. }, Item::AgentMessage { text, .. }) => text.clear(),
                                (ModelEvent::ReasoningDelta { .. }, Item::Reasoning { content, .. }) => content.clear(),
                                _ => {}
                            }
                            pending.push(EventPayload::ItemStarted { turn_id: turn_id.into(), item: display });
                        }
                        let delta = match event {
                            ModelEvent::TextDelta { delta, .. } => Some(ItemDelta::AgentMessage { delta }),
                            ModelEvent::ReasoningDelta { delta, .. } => Some(ItemDelta::ReasoningContent { content_index: 0, delta }),
                            _ => None,
                        };
                        if let Some(delta) = delta {
                            pending.push(EventPayload::ItemDelta { turn_id: turn_id.into(), item_id: item.id.clone(), delta });
                        }
                    }
                }
                Some(Err(value)) => { error = Some(value); break; }
                None => break,
            }
        }
        if pending.len() >= 128 {
            flush(shared, &mut pending).await?;
        }
    }
    // Only completed items may drive continuation. Preserve partial history on
    // user interruption or a terminal error; cleanup repairs unexecuted tools.
    let content = response.completed();
    let interrupted = cancel.is_cancelled();
    let preserve_partial = interrupted
        || error
            .as_ref()
            .is_some_and(|error| !error.is_retryable() || content.is_empty());
    // Empty failed retries may recover the last partial output, but Esc must not.
    if !interrupted && preserve_partial && response.items.is_empty() {
        response = last_nonempty_response;
    }
    let history = if preserve_partial {
        response
            .items
            .iter()
            .map(|item| item.content.clone())
            .collect()
    } else {
        content.clone()
    };
    // Persist history before publishing the final display state.
    if !history.is_empty() {
        shared
            .state
            .lock()
            .await
            .message(Message::Assistant { content: history })
            .await?;
    }
    finish_items(turn_id, &response, &mut pending, !preserve_partial);
    flush(shared, &mut pending).await?;
    if let Some(usage) = usage {
        record_usage(
            shared,
            usage,
            (reason.is_some() || preserve_partial).then_some(estimated),
            &model,
            started_at,
            true,
        )
        .await?;
    }
    Ok((content, reason, error))
}

fn finish_items(
    turn_id: &str,
    response: &crate::response::Response,
    pending: &mut Vec<EventPayload>,
    discarded_incomplete: bool,
) {
    for item in &response.items {
        if let Some(item) = item.display(discarded_incomplete) {
            pending.push(EventPayload::ItemCompleted {
                turn_id: turn_id.into(),
                item,
            });
        }
    }
}

pub(crate) async fn record_retry(
    shared: &Shared,
    turn_id: &str,
    error: &ModelError,
    attempt: u32,
    max_retries: u32,
    delay: Duration,
) -> Result<(), CoreError> {
    let item = Item::ModelRetry {
        id: id("retry"),
        error: ErrorInfo {
            message: error.to_string(),
            additional_details: None,
        },
        will_retry: true,
        attempt,
        max_retries,
        delay_seconds: delay.as_secs_f64(),
    };
    shared
        .state
        .lock()
        .await
        .journal
        .append(vec![
            EventPayload::ItemStarted {
                turn_id: turn_id.into(),
                item: item.clone(),
            },
            EventPayload::ItemCompleted {
                turn_id: turn_id.into(),
                item,
            },
        ])
        .await?;
    Ok(())
}

pub(crate) async fn record_usage(
    shared: &Shared,
    usage: Usage,
    estimated: Option<u64>,
    model: &str,
    started_at: i64,
    in_turn: bool,
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
    let request = crate::usage::request_charge(&shared.options.prices, model, started_at, &last);
    let charge = crate::usage::cost(&request);
    let mut cost = state
        .journal
        .snapshot
        .token_usage
        .as_ref()
        .and_then(|usage| usage.cost.clone())
        .unwrap_or_default();
    cost.add(&charge);
    let token_usage = TokenUsage {
        request: Some(request),
        cost: Some(cost),
        total,
        last: last.clone(),
        context_tokens,
        model_context_window: if in_turn {
            Some(shared.options.context_window)
        } else {
            state
                .journal
                .snapshot
                .token_usage
                .as_ref()
                .and_then(|usage| usage.model_context_window)
        },
        auto_compact_tokens: if in_turn {
            Some(compaction::input_limit(shared))
        } else {
            state
                .journal
                .snapshot
                .token_usage
                .as_ref()
                .and_then(|usage| usage.auto_compact_tokens)
        },
    };
    if let Some(active) = &mut state.active
        && in_turn
    {
        let mut total = active
            .turn
            .usage
            .as_ref()
            .map(|usage| usage.total.clone())
            .unwrap_or_default();
        add_usage(&mut total, &last);
        let mut cost = active
            .turn
            .usage
            .as_ref()
            .and_then(|usage| usage.cost.clone())
            .unwrap_or_default();
        cost.add(&charge);
        active.turn.usage = Some(TokenUsage {
            total,
            cost: Some(cost),
            ..token_usage.clone()
        });
    }
    state
        .journal
        .append(vec![if in_turn {
            EventPayload::SessionUsage { token_usage }
        } else {
            EventPayload::SessionBtwUsage { token_usage }
        }])
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
    let plan_mode = shared
        .state
        .lock()
        .await
        .journal
        .snapshot
        .settings
        .plan_mode;
    let Some(tool) = shared
        .tools
        .find(&call.name)
        .or_else(|| (plan_mode && call.name == "update_plan").then_some(Tool::UpdatePlan))
    else {
        let mut state = shared.state.lock().await;
        state
            .tool_result(&call, "Unknown tool".into(), false)
            .await?;
        state
            .journal
            .append(vec![EventPayload::Warning {
                message: format!("Unknown tool {}; not run.", call.name),
            }])
            .await?;
        return Ok(());
    };
    let review = shared
        .state
        .lock()
        .await
        .active
        .as_ref()
        .is_some_and(|active| active.turn.review.is_some());
    let prepared = if plan_mode
        && !shared
            .tools
            .allowed_in_plan_mode(&call.name, &call.arguments)
    {
        Err(shared
            .tools
            .blocked_in_plan_mode(&call.name, id("tool"), &call.arguments))
    } else if review && matches!(tool, Tool::Write | Tool::Edit) {
        Err(ToolOutput {
            text: "review is read-only".into(),
            item: Item::FileChange {
                id: id("tool"),
                changes: Vec::new(),
                status: ItemStatus::Declined,
            },
            outcome: ToolOutcome::Declined,
        })
    } else {
        shared
            .tools
            .prepare_named(&call.name, id("tool"), &call.arguments, cancel)
            .await
    };
    let output = match prepared {
        Err(mut output) => {
            if review && matches!(tool, Tool::Mcp | Tool::WebFetch) {
                output.text = "review is read-only".into();
                output.outcome = ToolOutcome::Declined;
                match &mut output.item {
                    Item::McpToolCall { status, .. } => *status = ItemStatus::Declined,
                    Item::WebFetch { status, result, .. } => {
                        *status = ItemStatus::Declined;
                        *result = output.text.clone();
                    }
                    _ => {}
                }
            }
            output
        }
        Ok(prepared) if prepared.tool() == Tool::AskUser => {
            ask_user(shared, turn_id, &prepared, cancel).await?
        }
        Ok(prepared) if prepared.tool() == Tool::SubmitPlan => {
            crate::plan_mode::submit(shared, turn_id, &prepared, cancel).await?
        }
        Ok(prepared) => {
            let permission = permission(shared, turn_id, &prepared, cancel).await?;
            match permission {
                None => {
                    let mut item = prepared.started_item();
                    match &mut item {
                        Item::BackgroundCommandAction { status, .. } => {
                            *status = ItemStatus::Failed
                        }
                        Item::Plan { status, .. }
                        | Item::McpToolCall { status, .. }
                        | Item::WebFetch { status, .. }
                        | Item::FileRead { status, .. }
                        | Item::FileChange { status, .. }
                        | Item::CommandExecution { status, .. } => *status = ItemStatus::Failed,
                        _ => unreachable!("prepared tool item"),
                    }
                    ToolOutput {
                        text: "Interrupted; not run".into(),
                        item,
                        outcome: ToolOutcome::Cancelled,
                    }
                }
                Some(PermissionCheck::Deny {
                    cancel_turn,
                    feedback,
                }) => {
                    if cancel_turn {
                        cancel.cancel();
                    }
                    let mut output = prepared.declined_output(feedback.as_deref());
                    if shared
                        .state
                        .lock()
                        .await
                        .journal
                        .snapshot
                        .settings
                        .plan_mode
                        && !shared
                            .tools
                            .allowed_in_plan_mode(&call.name, &call.arguments)
                    {
                        output = shared.tools.blocked_in_plan_mode(
                            &call.name,
                            output.item.id().unwrap().into(),
                            &call.arguments,
                        );
                    } else if review {
                        output.text = "review is read-only".into();
                    }
                    output
                }
                Some(PermissionCheck::Ask(_)) => unreachable!("approval resolved"),
                Some(PermissionCheck::Execute(permit)) => {
                    let checkpoint = shared
                        .state
                        .lock()
                        .await
                        .checkpoint_file(turn_id, &prepared)
                        .await?;
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
                    if output.outcome != ToolOutcome::Completed
                        && let Some(id) = checkpoint
                    {
                        shared.state.lock().await.abort_checkpoint(id).await?;
                    }
                    output
                }
            }
        }
    };
    shared.sync_background().await?;
    let mut state = shared.state.lock().await;
    if output.outcome == ToolOutcome::Cancelled {
        cancel.cancel();
    }
    state
        .tool_result(&call, output.text, output.outcome == ToolOutcome::Completed)
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
    let mut receiver = {
        let mut state = shared.state.lock().await;
        state
            .journal
            .append(vec![EventPayload::ItemStarted {
                turn_id: turn_id.into(),
                item: call.started_item(),
            }])
            .await?;
        let review_denied = state
            .active
            .as_ref()
            .is_some_and(|active| active.turn.review.is_some())
            && matches!(
                state.gate.evaluate_review(call),
                PermissionCheck::Deny { .. }
            );
        let permission = if review_denied {
            state.gate.evaluate_review(call)
        } else if state.journal.snapshot.settings.plan_mode {
            state
                .gate
                .evaluate_plan(call, state.journal.snapshot.settings.approval_policy)
        } else if state
            .active
            .as_ref()
            .is_some_and(|active| active.turn.review.is_some())
        {
            state.gate.evaluate_review(call)
        } else {
            state
                .gate
                .evaluate(call, state.journal.snapshot.settings.approval_policy)
        };
        match permission {
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
                            close_reason: None,
                        },
                    ])
                    .await?;
                state.requests.insert(request.id, sender);
                receiver
            }
            permission => return Ok(Some(permission)),
        }
    };
    let resolution = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            match receiver.try_recv() {
                Ok(decision) => decision,
                Err(_) => {
                    shared.state.lock().await.cancel_requests().await?;
                    return Ok(None);
                }
            }
        },
        decision = &mut receiver => decision.map_err(|_| CoreError::internal())?,
    };
    let ApprovalResolution::Answered { decision, feedback } = resolution else {
        return Err(CoreError::internal());
    };
    let mut state = shared.state.lock().await;
    if state.journal.snapshot.settings.plan_mode
        && matches!(
            state
                .gate
                .evaluate_plan(call, state.journal.snapshot.settings.approval_policy),
            PermissionCheck::Deny { .. }
        )
    {
        return Ok(Some(PermissionCheck::Deny {
            cancel_turn: false,
            feedback: None,
        }));
    }
    state
        .gate
        .resolve(call, decision, feedback)
        .map(Some)
        .map_err(|_| CoreError::new(ErrorCode::InvalidDecision, "invalid approval decision"))
}
async fn ask_user(
    shared: &Shared,
    turn_id: &str,
    call: &PreparedCall,
    cancel: &CancellationToken,
) -> Result<ToolOutput, CoreError> {
    let question = call.user_input_request().expect("user input call");
    let mut receiver = {
        let mut state = shared.state.lock().await;
        let request = PendingRequest {
            id: id("question"),
            turn_id: turn_id.into(),
            created_at: crate::journal::now(),
            kind: RequestKind::UserInput(question.clone()),
        };
        let (sender, receiver) = tokio::sync::oneshot::channel();
        state
            .journal
            .append(vec![
                EventPayload::ItemStarted {
                    turn_id: turn_id.into(),
                    item: call.started_item(),
                },
                EventPayload::RequestOpened {
                    request: request.clone(),
                },
                EventPayload::SessionStatus {
                    status: SessionStatus::WaitingApproval,
                    close_reason: None,
                },
            ])
            .await?;
        state.requests.insert(request.id, sender);
        receiver
    };
    let resolution = tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            match receiver.try_recv() {
                Ok(resolution) => Some(resolution),
                Err(_) => {
                    shared.state.lock().await.cancel_requests().await?;
                    None
                }
            }
        },
        resolution = &mut receiver => Some(resolution.map_err(|_| CoreError::internal())?),
    };
    let answer = match resolution {
        Some(ApprovalResolution::UserInput { answer }) => Some(answer),
        None => None,
        _ => return Err(CoreError::internal()),
    };
    let text = match &answer {
        Some(answer) => {
            serde_json::json!({ "question": question.question, "answer": answer }).to_string()
        }
        None => "Interrupted; no user answer".into(),
    };
    let answered = answer.is_some();
    Ok(ToolOutput {
        text,
        item: Item::UserInput {
            id: question.item_id,
            question: question.question,
            answer,
            status: if answered {
                ItemStatus::Completed
            } else {
                ItemStatus::Failed
            },
        },
        outcome: if answered {
            ToolOutcome::Completed
        } else {
            ToolOutcome::Cancelled
        },
    })
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
