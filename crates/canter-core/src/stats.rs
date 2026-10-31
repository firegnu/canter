//! Read-only projections of durable work, including work later rewound.
use crate::{context::HistoryEntry, usage};
use canter_protocol::*;
use canter_store::{Store, StoreError};
use std::collections::{BTreeMap, BTreeSet};

mod signals;
mod tools;

pub(super) struct SavedItem {
    turn: String,
    seq: u64,
    timestamp: i64,
    item: Item,
}

pub fn list(store: &Store, all: bool) -> Result<StatsListResponse, StoreError> {
    let mut sessions = store.list()?;
    if !all {
        sessions.truncate(20);
    }
    let mut data = Vec::with_capacity(sessions.len());
    for (index, session) in sessions.into_iter().enumerate() {
        let events = store.events_after(&session.id, 0)?;
        let first_message = events.iter().find_map(|event| {
            let input = match &event.payload {
                EventPayload::TurnStarted { turn, .. } => &turn.input,
                EventPayload::ItemStarted {
                    item: Item::UserMessage { content, .. },
                    ..
                }
                | EventPayload::ItemCompleted {
                    item: Item::UserMessage { content, .. },
                    ..
                } => content,
                _ => return None,
            };
            let text = input
                .iter()
                .filter_map(|part| match part {
                    UserInput::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(" ");
            (!text.trim().is_empty()).then_some(text)
        });
        data.push(StatsSession {
            number: index + 1,
            session_id: session.id,
            updated_at: session.updated_at,
            cwd: session.cwd,
            first_message,
        });
    }
    Ok(StatsListResponse { data })
}

pub fn report(
    store: &Store,
    id: &str,
    prices: &usage::PriceBook,
) -> Result<StatsResponse, StoreError> {
    let events = store.events_after(id, 0)?;
    if events.is_empty() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "session not found").into());
    }
    let history = match store.model_history::<HistoryEntry>(id) {
        Ok(history) => history,
        Err(StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    let ledger = usage::Ledger::replay(&events, prices);
    let mut report = StatsResponse {
        session_id: id.into(),
        event_seq: events.last().unwrap().seq,
        overview: StatsOverview {
            started_at: events[0].timestamp,
            updated_at: events.last().unwrap().timestamp,
            usage: (!ledger.records.is_empty()).then_some(ledger.report),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut requests = BTreeMap::<String, usize>::new();
    let mut compactions = BTreeMap::<String, StatsCompaction>::new();
    let mut items = BTreeMap::<String, SavedItem>::new();
    let mut approvals = BTreeMap::<String, StatsApproval>::new();
    let mut retries = BTreeSet::new();
    let mut turns = BTreeSet::new();
    let mut turn_id = None;
    let mut compacting = false;
    for event in &events {
        match &event.payload {
            EventPayload::SessionUpdated { settings } => {
                report.overview.cwd.clone_from(&settings.cwd);
                report.overview.model.clone_from(&settings.model);
                report.overview.tags.clone_from(&settings.tags);
            }
            EventPayload::TurnStarted { turn, .. } => {
                turns.insert(turn.id.clone());
                turn_id = Some(turn.id.clone());
            }
            EventPayload::TurnCompleted { .. } => turn_id = None,
            EventPayload::ItemStarted { turn_id, item }
            | EventPayload::ItemCompleted { turn_id, item } => {
                if let Item::ModelRetry { id, .. } = item {
                    retries.insert(id.clone());
                }
                if let Item::ContextCompaction { id, summary } = item {
                    let completed = matches!(event.payload, EventPayload::ItemCompleted { .. });
                    compacting = !completed;
                    let row = compactions.entry(id.clone()).or_default();
                    row.turn_id.clone_from(turn_id);
                    row.item_id.clone_from(id);
                    row.event_seq = event.seq;
                    row.timestamp = event.timestamp;
                    if completed {
                        row.summary = Some(summary.clone());
                    }
                }
                if let Some(id) = item.id() {
                    items.insert(
                        id.into(),
                        SavedItem {
                            turn: turn_id.clone(),
                            seq: event.seq,
                            timestamp: event.timestamp,
                            item: item.clone(),
                        },
                    );
                }
            }
            EventPayload::TelemetryCompaction {
                turn_id,
                item_id,
                trigger,
                trigger_occupancy_tokens,
                estimated,
                before_tokens,
                after_tokens,
                before_messages,
                after_messages,
                removed_messages,
                succeeded,
            } => {
                let row = compactions.entry(item_id.clone()).or_default();
                row.turn_id.clone_from(turn_id);
                row.item_id.clone_from(item_id);
                if row.event_seq == 0 {
                    row.event_seq = event.seq;
                    row.timestamp = event.timestamp;
                }
                row.trigger = *trigger;
                row.trigger_occupancy_tokens = *trigger_occupancy_tokens;
                row.estimated = Some(*estimated);
                row.before_tokens = Some(*before_tokens);
                row.after_tokens = Some(*after_tokens);
                row.before_messages = Some(*before_messages);
                row.after_messages = Some(*after_messages);
                row.removed_messages = Some(*removed_messages);
                row.succeeded = Some(*succeeded);
            }
            EventPayload::TelemetryModelRequest { record } => {
                merge_request(
                    &mut report.requests,
                    &mut requests,
                    record.clone(),
                    Some(event.seq),
                );
            }
            EventPayload::SessionUsage { token_usage }
            | EventPayload::SessionBtwUsage { token_usage } => {
                let private = matches!(event.payload, EventPayload::SessionBtwUsage { .. });
                let charge = token_usage.request.clone().unwrap_or_else(|| {
                    usage::request_charge(
                        prices,
                        &report.overview.model,
                        event.timestamp,
                        &token_usage.last,
                    )
                });
                let index = request_index(
                    &mut report.requests,
                    &mut requests,
                    charge.request_id.as_deref(),
                );
                let row = &mut report.requests[index];
                if row.started_at == 0 {
                    row.started_at = charge.started_at;
                }
                if row.settings.is_none() {
                    row.purpose = if private {
                        RequestPurpose::Unknown
                    } else if compacting {
                        RequestPurpose::Compaction
                    } else {
                        RequestPurpose::Conversation
                    };
                }
                row.event_seq.get_or_insert(event.seq);
                if !private {
                    row.turn_id = turn_id.clone();
                }
                add_tokens(row.usage.get_or_insert_default(), &token_usage.last);
                row.cost.get_or_insert_default().add(&usage::cost(&charge));
            }
            EventPayload::RequestOpened { request }
                if matches!(
                    request.kind,
                    RequestKind::Approval(_)
                        | RequestKind::SandboxApproval(_)
                        | RequestKind::PlanApproval(_)
                ) =>
            {
                approvals
                    .entry(request.id.clone())
                    .or_insert_with(|| StatsApproval {
                        request_id: request.id.clone(),
                        turn_id: request.turn_id.clone(),
                        timestamp: request.created_at,
                        duration_ms: None,
                        outcome: ApprovalOutcome::Unknown,
                    });
            }
            EventPayload::TelemetryApproval {
                turn_id,
                request_id,
                duration_ms,
                outcome,
            } => {
                let row = approvals
                    .entry(request_id.clone())
                    .or_insert_with(|| StatsApproval {
                        request_id: request_id.clone(),
                        turn_id: turn_id.clone(),
                        timestamp: event.timestamp,
                        duration_ms: None,
                        outcome: ApprovalOutcome::Unknown,
                    });
                row.duration_ms = Some(*duration_ms);
                row.outcome = *outcome;
            }
            _ => {}
        }
    }
    for record in store.private_telemetry(id)? {
        merge_request(&mut report.requests, &mut requests, record, None);
    }
    report
        .requests
        .sort_by_key(|row| (row.started_at, row.event_seq));
    report.overview.turn_count = turns.len();
    report.overview.request_count = report.requests.len();
    report.overview.recorded_retry_count = retries.len();
    let mut main_retries = 0_u64;
    let mut private_retries = Some(0_u64);
    for row in &report.requests {
        if row.turn_id.is_some() {
            main_retries += u64::from(row.retry_count.unwrap_or(0));
        } else {
            private_retries = private_retries
                .zip(row.retry_count)
                .map(|(a, b)| a + u64::from(b));
        }
    }
    report.overview.retry_count =
        private_retries.map(|n| n + main_retries.max(retries.len() as u64));
    report.compactions = compactions.into_values().collect();
    report.compactions.sort_by_key(|row| row.event_seq);
    let mut items: Vec<_> = items.into_values().collect();
    items.sort_by_key(|row| row.seq);
    signals::fill(&mut report.compactions, &items, &report.overview.cwd);
    report.tool_calls = tools::calls(&events, &history, &items);
    report.tools = tools::aggregate(&report.tool_calls);
    report.approvals.data = approvals.into_values().collect();
    report.approvals.data.sort_by_key(|row| row.timestamp);
    report.approvals.count = report.approvals.data.len();
    let durations: Option<Vec<u64>> = report
        .approvals
        .data
        .iter()
        .map(|row| row.duration_ms)
        .collect();
    report.approvals.total_duration_ms = durations.as_ref().map(|values| values.iter().sum());
    report.approvals.max_duration_ms =
        durations.map(|values| values.into_iter().max().unwrap_or(0));
    Ok(report)
}

fn request_index(
    rows: &mut Vec<StatsRequest>,
    indices: &mut BTreeMap<String, usize>,
    id: Option<&str>,
) -> usize {
    if let Some(index) = id.and_then(|id| indices.get(id)) {
        return *index;
    }
    let index = rows.len();
    rows.push(StatsRequest {
        request_id: id.map(Into::into),
        ..Default::default()
    });
    if let Some(id) = id {
        indices.insert(id.into(), index);
    }
    index
}

fn merge_request(
    rows: &mut Vec<StatsRequest>,
    indices: &mut BTreeMap<String, usize>,
    record: ModelRequestTelemetry,
    seq: Option<u64>,
) {
    let index = request_index(rows, indices, Some(&record.request_id));
    let row = &mut rows[index];
    row.turn_id = record.turn_id;
    row.event_seq = row.event_seq.or(seq);
    row.started_at = record.started_at;
    row.purpose = record.purpose;
    row.outcome = record.outcome;
    row.duration_ms = Some(record.duration_ms);
    row.first_response_ms = record.first_response_ms;
    row.retry_count = record.retry_count;
    row.context_total_tokens = Some(record.context.total());
    row.context = Some(record.context);
    row.settings = Some(record.settings);
    row.tool_call_ids = record.tool_call_ids;
}

fn add_tokens(total: &mut TokenUsageBreakdown, part: &TokenUsageBreakdown) {
    total.input_tokens = total.input_tokens.saturating_add(part.input_tokens);
    total.cached_input_tokens = total
        .cached_input_tokens
        .saturating_add(part.cached_input_tokens);
    total.output_tokens = total.output_tokens.saturating_add(part.output_tokens);
    total.reasoning_output_tokens = total
        .reasoning_output_tokens
        .saturating_add(part.reasoning_output_tokens);
    total.total_tokens = total.total_tokens.saturating_add(part.total_tokens);
}
