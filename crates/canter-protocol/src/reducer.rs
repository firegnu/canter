use crate::{Event, EventPayload, Item, ItemDelta, SessionSnapshot, SessionStatus};

#[derive(Debug, thiserror::Error)]
pub enum ReduceError {
    #[error("event belongs to another session")]
    WrongSession,
    #[error("expected event sequence {expected}, received {actual}")]
    Sequence { expected: u64, actual: u64 },
    #[error("event sequence exhausted")]
    SequenceExhausted,
    #[error("event refers to a missing turn")]
    MissingTurn,
    #[error("turn already exists")]
    DuplicateTurn,
    #[error("delta does not match the item or its text part index")]
    InvalidDelta,
}

/// Pure convenience wrapper. Streaming consumers should use `apply_event_mut`.
pub fn apply_event(
    snapshot: &SessionSnapshot,
    event: &Event,
) -> Result<SessionSnapshot, ReduceError> {
    let mut next = snapshot.clone();
    apply_event_mut(&mut next, event)?;
    Ok(next)
}

/// Check every fallible precondition without cloning or changing the snapshot.
/// Storage uses this before committing the event to disk.
pub fn validate_event(snapshot: &SessionSnapshot, event: &Event) -> Result<(), ReduceError> {
    if event.session_id != snapshot.id {
        return Err(ReduceError::WrongSession);
    }
    if event.seq <= snapshot.last_event_seq {
        return Ok(());
    }
    let expected = snapshot
        .last_event_seq
        .checked_add(1)
        .ok_or(ReduceError::SequenceExhausted)?;
    if event.seq != expected {
        return Err(ReduceError::Sequence {
            expected,
            actual: event.seq,
        });
    }
    match &event.payload {
        EventPayload::TurnStarted { turn, .. } => {
            if snapshot.turns.iter().any(|existing| existing.id == turn.id) {
                return Err(ReduceError::DuplicateTurn);
            }
        }
        EventPayload::TurnCompleted { turn } => {
            if !snapshot.turns.iter().any(|existing| existing.id == turn.id) {
                return Err(ReduceError::MissingTurn);
            }
        }
        EventPayload::ItemStarted { turn_id, item }
        | EventPayload::ItemCompleted { turn_id, item }
            if item.id().is_some() =>
        {
            if !snapshot.turns.iter().any(|turn| turn.id == *turn_id) {
                return Err(ReduceError::MissingTurn);
            }
        }
        EventPayload::ItemDelta {
            turn_id,
            item_id,
            delta,
        } if !matches!(delta, ItemDelta::Unknown) => {
            let turn = snapshot
                .turns
                .iter()
                .find(|turn| turn.id == *turn_id)
                .ok_or(ReduceError::MissingTurn)?;
            if let Some(item) = turn.items.iter().find(|item| item.id() == Some(item_id)) {
                let valid = match (item, delta) {
                    (Item::AgentMessage { .. }, ItemDelta::AgentMessage { .. })
                    | (Item::CommandExecution { .. }, ItemDelta::CommandExecution { .. }) => true,
                    (
                        Item::Reasoning { summary, .. },
                        ItemDelta::ReasoningSummary { summary_index, .. },
                    ) => (*summary_index as usize) <= summary.len(),
                    (
                        Item::Reasoning { content, .. },
                        ItemDelta::ReasoningContent { content_index, .. },
                    ) => (*content_index as usize) <= content.len(),
                    _ => false,
                };
                if !valid {
                    return Err(ReduceError::InvalidDelta);
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// In-place replay. All validation precedes mutation, so errors leave state intact.
pub fn apply_event_mut(snapshot: &mut SessionSnapshot, event: &Event) -> Result<(), ReduceError> {
    validate_event(snapshot, event)?;
    if event.seq <= snapshot.last_event_seq {
        return Ok(());
    }
    match &event.payload {
        EventPayload::SessionUpdated { settings } => snapshot.settings = settings.clone(),
        EventPayload::SessionStatus {
            status,
            close_reason,
        } => {
            snapshot.status = *status;
            snapshot.close_reason = if *status == SessionStatus::Closed {
                *close_reason
            } else {
                None
            };
        }
        EventPayload::TurnStarted { turn, input_id } => {
            let mut turn = turn.clone();
            turn.items.retain(|item| item.id().is_some());
            snapshot.turns.push(turn);
            if let Some(id) = input_id {
                snapshot.queued_inputs.retain(|input| input.id != *id);
            }
        }
        EventPayload::InputQueued { input } => {
            if !snapshot
                .queued_inputs
                .iter()
                .any(|existing| existing.id == input.id)
            {
                snapshot.queued_inputs.push(input.clone());
            }
        }
        EventPayload::InputRemoved { input_ids, .. } => {
            snapshot
                .queued_inputs
                .retain(|input| !input_ids.contains(&input.id));
        }
        EventPayload::TurnCompleted { turn } => {
            let existing = snapshot
                .turns
                .iter_mut()
                .find(|existing| existing.id == turn.id)
                .expect("validated turn");
            existing.status = turn.status;
            existing.error = turn.error.clone();
            existing.usage = turn.usage.clone();
        }
        EventPayload::ItemStarted { turn_id, item }
        | EventPayload::ItemCompleted { turn_id, item } => {
            if let Some(id) = item.id() {
                let turn = snapshot
                    .turns
                    .iter_mut()
                    .find(|turn| turn.id == *turn_id)
                    .expect("validated turn");
                if let Some(existing) = turn
                    .items
                    .iter_mut()
                    .find(|existing| existing.id() == Some(id))
                {
                    *existing = item.clone();
                } else {
                    turn.items.push(item.clone());
                }
            }
        }
        EventPayload::ItemDelta {
            turn_id,
            item_id,
            delta,
        } => {
            if !matches!(delta, ItemDelta::Unknown) {
                let turn = snapshot
                    .turns
                    .iter_mut()
                    .find(|turn| turn.id == *turn_id)
                    .expect("validated turn");
                // A future item may have been skipped, along with its deltas.
                if let Some(item) = turn
                    .items
                    .iter_mut()
                    .find(|item| item.id() == Some(item_id))
                {
                    match (item, delta) {
                        (Item::AgentMessage { text, .. }, ItemDelta::AgentMessage { delta }) => {
                            text.push_str(delta)
                        }
                        (
                            Item::CommandExecution {
                                aggregated_output, ..
                            },
                            ItemDelta::CommandExecution { delta },
                        ) => aggregated_output.push_str(delta),
                        (
                            Item::Reasoning { summary, .. },
                            ItemDelta::ReasoningSummary {
                                summary_index,
                                delta,
                            },
                        ) => append_part(summary, *summary_index, delta),
                        (
                            Item::Reasoning { content, .. },
                            ItemDelta::ReasoningContent {
                                content_index,
                                delta,
                            },
                        ) => append_part(content, *content_index, delta),
                        _ => unreachable!("validated delta"),
                    }
                }
            }
        }
        EventPayload::RequestOpened { request } => {
            if !snapshot
                .pending_requests
                .iter()
                .any(|existing| existing.id == request.id)
            {
                snapshot.pending_requests.push(request.clone());
            }
        }
        EventPayload::RequestResolved { request_id, .. } => snapshot
            .pending_requests
            .retain(|request| request.id != *request_id),
        EventPayload::SessionUsage { token_usage } => {
            snapshot.token_usage = Some(token_usage.clone())
        }
        EventPayload::Error { error, .. } => snapshot.error = Some(error.clone()),
        EventPayload::Warning { .. } | EventPayload::Unknown => {}
    }
    if snapshot.last_event_seq == 0 {
        snapshot.created_at = event.timestamp;
    }
    snapshot.last_event_seq = event.seq;
    snapshot.updated_at = event.timestamp;
    Ok(())
}

fn append_part(parts: &mut Vec<String>, index: u32, delta: &str) {
    let index = index as usize;
    if index == parts.len() {
        parts.push(delta.to_owned());
    } else {
        parts[index].push_str(delta);
    }
}
