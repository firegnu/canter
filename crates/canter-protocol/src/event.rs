use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{
    ApprovalResolution, ErrorInfo, Item, PendingRequest, QueuedInput, SessionCloseReason,
    SessionSettings, SessionStatus, TokenUsage, Turn,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Event {
    #[ts(type = "number")]
    pub seq: u64,
    pub session_id: String,
    /// Unix time in milliseconds.
    #[ts(type = "number")]
    pub timestamp: i64,
    #[serde(flatten)]
    pub payload: EventPayload,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum EventPayload {
    /// Live only; seq is zero and does not advance the durable cursor.
    #[serde(rename = "session.promptSuggestion")]
    SessionPromptSuggestion {
        turn_id: Option<String>,
        text: String,
    },
    #[serde(rename = "session.promptSuggestionsReduced")]
    SessionPromptSuggestionsReduced,
    #[serde(rename = "session.parent")]
    SessionParent { parent: crate::SessionParent },
    #[serde(rename = "telemetry.compaction")]
    TelemetryCompaction {
        turn_id: String,
        item_id: String,
        #[serde(default)]
        #[ts(optional = nullable)]
        trigger: crate::CompactionTrigger,
        /// Summary request shape: `prefix` or `transcript`; absent in older events.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        method: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        trigger_occupancy_tokens: Option<u64>,
        estimated: bool,
        #[ts(type = "number")]
        before_tokens: u64,
        #[ts(type = "number")]
        after_tokens: u64,
        before_messages: usize,
        after_messages: usize,
        removed_messages: usize,
        succeeded: bool,
    },
    #[serde(rename = "telemetry.toolResult")]
    TelemetryToolResult {
        turn_id: String,
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        item_id: Option<String>,
        estimated: bool,
        #[ts(type = "number")]
        result_tokens: u64,
        truncated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        original_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        source_file_bytes: Option<u64>,
    },
    #[serde(rename = "telemetry.approval")]
    TelemetryApproval {
        turn_id: String,
        request_id: String,
        #[ts(type = "number")]
        duration_ms: u64,
        #[serde(default)]
        #[ts(optional = nullable)]
        outcome: crate::ApprovalOutcome,
    },
    #[serde(rename = "telemetry.modelRequest")]
    TelemetryModelRequest {
        #[serde(flatten)]
        record: crate::ModelRequestTelemetry,
    },
    #[serde(rename = "languageServer.updated")]
    LanguageServerUpdated { server: crate::LanguageServer },
    #[serde(rename = "session.goal.updated")]
    SessionGoalUpdated { goal: Option<crate::SessionGoal> },
    #[serde(rename = "backgroundCommand.updated")]
    BackgroundCommandUpdated { command: crate::BackgroundCommand },
    #[serde(rename = "session.rewound")]
    SessionRewound {
        before_turn_id: String,
        rewind_conversation: bool,
        record: Turn,
    },
    #[serde(rename = "session.updated")]
    SessionUpdated { settings: SessionSettings },
    #[serde(rename = "session.status")]
    SessionStatus {
        status: SessionStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        close_reason: Option<SessionCloseReason>,
    },
    #[serde(rename = "turn.started")]
    TurnStarted {
        turn: Turn,
        #[serde(default)]
        #[ts(optional = nullable)]
        input_id: Option<String>,
    },
    #[serde(rename = "input.queued")]
    InputQueued { input: QueuedInput },
    #[serde(rename = "input.removed")]
    InputRemoved {
        input_ids: Vec<String>,
        reason: InputRemovalReason,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        turn_id: Option<String>,
    },
    #[serde(rename = "turn.completed")]
    TurnCompleted { turn: Turn },
    #[serde(rename = "turn.retracted")]
    TurnRetracted { turn_id: String },
    #[serde(rename = "item.started")]
    ItemStarted { turn_id: String, item: Item },
    #[serde(rename = "item.delta")]
    ItemDelta {
        turn_id: String,
        item_id: String,
        delta: ItemDelta,
    },
    #[serde(rename = "item.completed")]
    ItemCompleted { turn_id: String, item: Item },
    #[serde(rename = "request.opened")]
    RequestOpened { request: PendingRequest },
    #[serde(rename = "agent.request.opened")]
    AgentRequestOpened {
        #[serde(flatten)]
        forwarded: crate::AgentRequest,
    },
    /// P2-179, on a child session: the user stopped this agent turn.
    #[serde(rename = "agent.stopped")]
    AgentStopped { turn_id: String },
    /// P2-179, on the parent: a child's list entry after its status, name or
    /// latest turn changed.
    #[serde(rename = "agent.updated")]
    AgentUpdated { agent: crate::AgentSummary },
    #[serde(rename = "agent.request.resolved")]
    AgentRequestResolved {
        agent_id: String,
        request_id: String,
        resolution: ApprovalResolution,
        client_name: Option<String>,
    },
    #[serde(rename = "request.resolved")]
    RequestResolved {
        request_id: String,
        resolution: ApprovalResolution,
        client_name: Option<String>,
    },
    #[serde(rename = "session.usage")]
    SessionUsage { token_usage: TokenUsage },
    /// Session-only usage; never belongs to the active conversation turn.
    #[serde(rename = "session.btwUsage")]
    SessionBtwUsage { token_usage: TokenUsage },
    #[serde(rename = "warning")]
    Warning { message: String },
    #[serde(rename = "error")]
    Error {
        error: ErrorInfo,
        will_retry: bool,
        turn_id: Option<String>,
    },
    #[serde(rename = "unknown")]
    #[serde(other)]
    Unknown,
}

impl EventPayload {
    pub fn is_ephemeral(&self) -> bool {
        matches!(
            self,
            Self::SessionPromptSuggestion { .. } | Self::SessionPromptSuggestionsReduced
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum InputRemovalReason {
    Blocked,
    Steered,
    Merged,
    Interrupted,
    Closed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ItemDelta {
    AgentMessage {
        delta: String,
    },
    ReasoningSummary {
        summary_index: u32,
        delta: String,
    },
    ReasoningContent {
        content_index: u32,
        delta: String,
    },
    CommandExecution {
        delta: String,
    },
    #[serde(other)]
    Unknown,
}
