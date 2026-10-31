//! Read-only telemetry reports. Null means unobserved, never an inferred zero.
use crate::*;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct StatsListParams {
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct StatsListResponse {
    pub data: Vec<StatsSession>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsSession {
    pub number: usize,
    pub session_id: String,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub cwd: String,
    pub first_message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsResponse {
    pub session_id: String,
    #[ts(type = "number")]
    pub event_seq: u64,
    pub overview: StatsOverview,
    pub requests: Vec<StatsRequest>,
    pub compactions: Vec<StatsCompaction>,
    pub tools: Vec<StatsTool>,
    pub tool_calls: Vec<StatsToolCall>,
    pub approvals: StatsApprovals,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsOverview {
    pub cwd: String,
    pub model: String,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub tags: BTreeMap<String, String>,
    pub turn_count: usize,
    pub request_count: usize,
    pub usage: Option<UsageResponse>,
    #[ts(type = "number | null")]
    pub retry_count: Option<u64>,
    pub recorded_retry_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsRequest {
    pub request_id: Option<String>,
    pub turn_id: Option<String>,
    #[ts(type = "number | null")]
    pub event_seq: Option<u64>,
    #[ts(type = "number")]
    pub started_at: i64,
    pub purpose: RequestPurpose,
    pub outcome: ModelRequestOutcome,
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
    #[ts(type = "number | null")]
    pub first_response_ms: Option<u64>,
    pub retry_count: Option<u32>,
    pub settings: Option<RequestSettings>,
    pub context: Option<ContextEstimate>,
    #[ts(type = "number | null")]
    pub context_total_tokens: Option<u64>,
    pub tool_call_ids: Vec<String>,
    pub usage: Option<TokenUsageBreakdown>,
    pub cost: Option<UsageCost>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsCompaction {
    pub turn_id: String,
    pub item_id: String,
    #[ts(type = "number")]
    pub event_seq: u64,
    #[ts(type = "number")]
    pub timestamp: i64,
    pub summary: Option<String>,
    pub trigger: CompactionTrigger,
    #[ts(type = "number | null")]
    pub trigger_occupancy_tokens: Option<u64>,
    pub estimated: Option<bool>,
    #[ts(type = "number | null")]
    pub before_tokens: Option<u64>,
    #[ts(type = "number | null")]
    pub after_tokens: Option<u64>,
    pub before_messages: Option<usize>,
    pub after_messages: Option<usize>,
    pub removed_messages: Option<usize>,
    pub succeeded: Option<bool>,
    pub signals: CompactionSignals,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct CompactionSignals {
    pub reread_files: Option<Vec<String>>,
    pub unmentioned_files: Option<Vec<String>>,
    pub plan: PlanSignal,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanSignal {
    pub before: Option<Vec<PlanStep>>,
    pub after: Option<Vec<PlanStep>>,
    pub removed: Option<Vec<PlanStep>>,
    pub status_changed: Option<Vec<PlanStatusChange>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct PlanStatusChange {
    pub step: String,
    pub before: PlanStepStatus,
    pub after: PlanStepStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsToolCall {
    pub tool_call_id: Option<String>,
    pub turn_id: Option<String>,
    pub item_id: Option<String>,
    #[ts(type = "number | null")]
    pub event_seq: Option<u64>,
    #[ts(type = "number | null")]
    pub timestamp: Option<i64>,
    pub name: String,
    pub status: ItemStatus,
    #[ts(type = "number | null")]
    pub result_tokens: Option<u64>,
    pub estimated: Option<bool>,
    pub truncated: Option<bool>,
    #[ts(type = "number | null")]
    pub original_tokens: Option<u64>,
    #[ts(type = "number | null")]
    pub source_file_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsTool {
    pub name: String,
    pub call_count: usize,
    pub failure_count: Option<usize>,
    pub truncated_count: Option<usize>,
    pub average_result_tokens: Option<f64>,
    #[ts(type = "number | null")]
    pub max_result_tokens: Option<u64>,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub unknown_status_count: usize,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub unknown_truncated_count: usize,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub unknown_result_size_count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsApprovals {
    pub count: usize,
    #[ts(type = "number | null")]
    pub total_duration_ms: Option<u64>,
    #[ts(type = "number | null")]
    pub max_duration_ms: Option<u64>,
    pub data: Vec<StatsApproval>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct StatsApproval {
    pub request_id: String,
    pub turn_id: String,
    #[ts(type = "number")]
    pub timestamp: i64,
    #[ts(type = "number | null")]
    pub duration_ms: Option<u64>,
    pub outcome: ApprovalOutcome,
}
