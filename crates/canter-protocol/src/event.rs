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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum InputRemovalReason {
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
