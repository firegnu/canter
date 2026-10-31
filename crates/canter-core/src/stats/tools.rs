use super::{HistoryEntry, SavedItem};
use canter_model::{AssistantContent, Message};
use canter_protocol::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

fn empty(id: &str, turn: Option<String>, name: String) -> StatsToolCall {
    StatsToolCall {
        tool_call_id: Some(id.into()),
        turn_id: turn,
        item_id: None,
        event_seq: None,
        timestamp: None,
        name,
        status: ItemStatus::Unknown,
        result_tokens: None,
        estimated: None,
        truncated: None,
        original_tokens: None,
        source_file_bytes: None,
    }
}

pub(super) fn calls(
    events: &[Event],
    history: &[HistoryEntry],
    items: &[SavedItem],
) -> Vec<StatsToolCall> {
    let mut rows = Vec::new();
    let mut indices = BTreeMap::<String, usize>::new();
    let mut turn = None;
    for entry in history {
        match entry {
            HistoryEntry::TurnBoundary { turn_id } => turn = Some(turn_id.clone()),
            HistoryEntry::Message {
                message: Message::Assistant { content },
                ..
            } => {
                for part in content {
                    if let AssistantContent::ToolCall(call) = part {
                        let index = *indices.entry(call.id.clone()).or_insert_with(|| {
                            rows.push(empty(&call.id, turn.clone(), call.name.clone()));
                            rows.len() - 1
                        });
                        rows[index].name.clone_from(&call.name);
                    }
                }
            }
            HistoryEntry::Message {
                message: Message::ToolResult { tool_call_id, text },
                ..
            } => {
                if let Some(index) = indices.get(tool_call_id) {
                    rows[*index].result_tokens = Some((text.len() as u64).div_ceil(3));
                    rows[*index].estimated = Some(true);
                }
            }
            _ => {}
        }
    }
    for event in events {
        if let EventPayload::TelemetryToolResult {
            turn_id,
            tool_call_id,
            item_id,
            estimated,
            result_tokens,
            truncated,
            original_tokens,
            source_file_bytes,
        } = &event.payload
        {
            let index = *indices.entry(tool_call_id.clone()).or_insert_with(|| {
                rows.push(empty(tool_call_id, Some(turn_id.clone()), "unknown".into()));
                rows.len() - 1
            });
            let row = &mut rows[index];
            row.item_id.clone_from(item_id);
            row.event_seq = Some(event.seq);
            row.timestamp = Some(event.timestamp);
            row.result_tokens = Some(*result_tokens);
            row.estimated = Some(*estimated);
            row.truncated = Some(*truncated);
            row.original_tokens = *original_tokens;
            row.source_file_bytes = *source_file_bytes;
        }
    }
    let by_id: BTreeMap<_, _> = items
        .iter()
        .filter_map(|item| Some((item.item.id()?, item)))
        .collect();
    let mut used: BTreeSet<String> = rows.iter().filter_map(|row| row.item_id.clone()).collect();
    let mut legacy = BTreeMap::<(&str, String), VecDeque<&SavedItem>>::new();
    for item in items {
        if let Some(name) = name(&item.item)
            && !used.contains(item.item.id().unwrap())
        {
            legacy
                .entry((&item.turn, family(&name).into()))
                .or_default()
                .push_back(item);
        }
    }
    for row in &mut rows {
        let item = if let Some(id) = row.item_id.as_deref() {
            by_id.get(id).copied()
        } else {
            legacy
                .get_mut(&(
                    row.turn_id.as_deref().unwrap_or(""),
                    family(&row.name).into(),
                ))
                .and_then(VecDeque::pop_front)
        };
        if let Some(item) = item {
            row.item_id = item.item.id().map(Into::into);
            used.extend(row.item_id.clone());
            if row.name == "unknown" {
                row.name = name(&item.item).unwrap_or_else(|| "unknown".into());
            }
            row.status = status(&item.item).unwrap_or(ItemStatus::Unknown);
            row.event_seq = Some(item.seq);
            row.timestamp = Some(item.timestamp);
        }
    }
    // Old or currently incomplete history can still have observable tool cards.
    for item in items {
        if let Some(name) = name(&item.item)
            && !used.contains(item.item.id().unwrap())
        {
            let mut row = empty("", Some(item.turn.clone()), name);
            row.tool_call_id = None;
            row.item_id = item.item.id().map(Into::into);
            row.event_seq = Some(item.seq);
            row.timestamp = Some(item.timestamp);
            row.status = status(&item.item).unwrap_or(ItemStatus::Unknown);
            rows.push(row);
        }
    }
    rows
}

fn family(name: &str) -> &str {
    match name {
        "write" | "edit" => "write/edit",
        name if name.starts_with("mcp__") => "mcp_call",
        name => name,
    }
}

fn name(item: &Item) -> Option<String> {
    Some(match item {
        Item::FileRead { .. } => "read".into(),
        Item::FileChange { .. } => "write/edit".into(),
        Item::CommandExecution {
            source: ExecCommandSource::Agent,
            ..
        } => "shell".into(),
        Item::WebFetch { .. } => "web_fetch".into(),
        Item::WebSearch { .. } => "web_search".into(),
        Item::McpToolCall { server, tool, .. } => format!("mcp__{server}__{tool}"),
        Item::McpToolSearch { .. } => "mcp_search".into(),
        Item::Plan { .. } => "update_plan".into(),
        Item::PlanProposal { .. } => "submit_plan".into(),
        Item::UserInput { .. } => "ask_user".into(),
        Item::BackgroundCommandAction { action, .. } if action == "monitor" => "monitor".into(),
        Item::BackgroundCommandAction { action, .. } => format!("shell_{action}"),
        _ => return None,
    })
}

fn status(item: &Item) -> Option<ItemStatus> {
    match item {
        Item::FileRead { status, .. }
        | Item::FileChange { status, .. }
        | Item::CommandExecution { status, .. }
        | Item::WebFetch { status, .. }
        | Item::WebSearch { status, .. }
        | Item::McpToolCall { status, .. }
        | Item::McpToolSearch { status, .. }
        | Item::Plan { status, .. }
        | Item::PlanProposal { status, .. }
        | Item::UserInput { status, .. }
        | Item::BackgroundCommandAction { status, .. } => Some(*status),
        _ => None,
    }
}

pub(super) fn aggregate(calls: &[StatsToolCall]) -> Vec<StatsTool> {
    let mut groups = BTreeMap::<&str, Vec<&StatsToolCall>>::new();
    for call in calls {
        groups.entry(&call.name).or_default().push(call);
    }
    groups
        .into_iter()
        .map(|(name, calls)| {
            let sizes: Vec<u64> = calls.iter().filter_map(|c| c.result_tokens).collect();
            StatsTool {
                name: name.into(),
                call_count: calls.len(),
                failure_count: Some(
                    calls
                        .iter()
                        .filter(|c| c.status == ItemStatus::Failed)
                        .count(),
                ),
                truncated_count: Some(calls.iter().filter(|c| c.truncated == Some(true)).count()),
                average_result_tokens: (!sizes.is_empty())
                    .then(|| sizes.iter().map(|v| *v as f64).sum::<f64>() / sizes.len() as f64),
                max_result_tokens: sizes.iter().copied().max(),
                unknown_status_count: calls
                    .iter()
                    .filter(|c| matches!(c.status, ItemStatus::Unknown | ItemStatus::InProgress))
                    .count(),
                unknown_truncated_count: calls.iter().filter(|c| c.truncated.is_none()).count(),
                unknown_result_size_count: calls.len() - sizes.len(),
            }
        })
        .collect()
}
