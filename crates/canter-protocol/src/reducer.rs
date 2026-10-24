use crate::{Event, EventPayload, Item, ItemDelta, SessionSnapshot};

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

/// Pure replay operation, shared by storage and clients.
pub fn apply_event(
    snapshot: &SessionSnapshot,
    event: &Event,
) -> Result<SessionSnapshot, ReduceError> {
    if event.session_id != snapshot.id {
        return Err(ReduceError::WrongSession);
    }
    if event.seq <= snapshot.last_event_seq {
        return Ok(snapshot.clone());
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
    let mut next = snapshot.clone();
    match &event.payload {
        EventPayload::SessionUpdated { settings } => next.settings = settings.clone(),
        EventPayload::SessionStatus { status } => next.status = *status,
        EventPayload::TurnStarted { turn } => {
            if next.turns.iter().any(|existing| existing.id == turn.id) {
                return Err(ReduceError::DuplicateTurn);
            }
            let mut turn = turn.clone();
            turn.items.retain(|item| item.id().is_some());
            next.turns.push(turn);
        }
        EventPayload::TurnCompleted { turn } => {
            let existing = next
                .turns
                .iter_mut()
                .find(|existing| existing.id == turn.id)
                .ok_or(ReduceError::MissingTurn)?;
            *existing = turn.clone();
            existing.items.retain(|item| item.id().is_some());
        }
        EventPayload::ItemStarted { turn_id, item }
        | EventPayload::ItemCompleted { turn_id, item } => {
            if let Some(id) = item.id() {
                let turn = next
                    .turns
                    .iter_mut()
                    .find(|turn| turn.id == *turn_id)
                    .ok_or(ReduceError::MissingTurn)?;
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
                let turn = next
                    .turns
                    .iter_mut()
                    .find(|turn| turn.id == *turn_id)
                    .ok_or(ReduceError::MissingTurn)?;
                // A future item may have been skipped, so its subsequent deltas are too.
                if let Some(item) = turn
                    .items
                    .iter_mut()
                    .find(|item| item.id() == Some(item_id))
                {
                    apply_delta(item, delta)?;
                }
            }
        }
        EventPayload::RequestOpened { request } => {
            if !next
                .pending_requests
                .iter()
                .any(|existing| existing.id == request.id)
            {
                next.pending_requests.push(request.clone());
            }
        }
        EventPayload::RequestResolved { request_id, .. } => {
            next.pending_requests
                .retain(|request| request.id != *request_id);
        }
        EventPayload::SessionUsage { token_usage } => next.token_usage = Some(token_usage.clone()),
        EventPayload::Error { error, .. } => next.error = Some(error.clone()),
        EventPayload::Warning { .. } | EventPayload::Unknown => {}
    }
    if next.last_event_seq == 0 {
        next.created_at = event.timestamp;
    }
    next.last_event_seq = event.seq;
    next.updated_at = event.timestamp;
    Ok(next)
}

fn apply_delta(item: &mut Item, delta: &ItemDelta) -> Result<(), ReduceError> {
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
        ) => append_part(summary, *summary_index, delta)?,
        (
            Item::Reasoning { content, .. },
            ItemDelta::ReasoningContent {
                content_index,
                delta,
            },
        ) => append_part(content, *content_index, delta)?,
        _ => return Err(ReduceError::InvalidDelta),
    }
    Ok(())
}

fn append_part(parts: &mut Vec<String>, index: u32, delta: &str) -> Result<(), ReduceError> {
    let index = index as usize;
    if index == parts.len() {
        parts.push(delta.to_owned());
    } else {
        parts
            .get_mut(index)
            .ok_or(ReduceError::InvalidDelta)?
            .push_str(delta);
    }
    Ok(())
}
