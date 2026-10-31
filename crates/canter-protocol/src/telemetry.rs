use crate::{PermissionMode, ReasoningEffort};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Same byte heuristic as context compaction, including message overhead.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextEstimate {
    pub estimated: bool,
    #[ts(type = "number")]
    pub system_tokens: u64,
    #[ts(type = "number")]
    pub tool_definition_tokens: u64,
    #[ts(type = "number")]
    pub conversation_tokens: u64,
    #[ts(type = "number")]
    pub tool_result_tokens: u64,
}

impl ContextEstimate {
    pub fn total(&self) -> u64 {
        self.system_tokens
            + self.tool_definition_tokens
            + self.conversation_tokens
            + self.tool_result_tokens
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestSettings {
    pub model: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub reasoning_effort: ReasoningEffort,
    pub approval_policy: PermissionMode,
    pub plan_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelRequestTelemetry {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
    pub purpose: String,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number")]
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub first_response_ms: Option<u64>,
    pub settings: RequestSettings,
    pub context: ContextEstimate,
    pub tool_call_ids: Vec<String>,
}
