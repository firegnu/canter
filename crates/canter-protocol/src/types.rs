use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    AlwaysAsk,
    AskForExecution,
    AllowAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Starting,
    Idle,
    Working,
    WaitingApproval,
    Error,
    Closed,
}

/// Public settings. Working directory and environment never belong here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSettings {
    pub name: Option<String>,
    pub model: String,
    pub approval_policy: PermissionMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub id: String,
    pub settings: SessionSettings,
    pub status: SessionStatus,
    /// Unix time in milliseconds; zero until the first event is applied.
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub turns: Vec<Turn>,
    pub pending_requests: Vec<ApprovalRequest>,
    pub token_usage: Option<TokenUsage>,
    pub error: Option<ErrorInfo>,
    #[ts(type = "number")]
    pub last_event_seq: u64,
}

impl SessionSnapshot {
    /// Empty replay state. The first durable event supplies public settings.
    pub fn empty(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            settings: SessionSettings {
                name: None,
                model: String::new(),
                approval_policy: PermissionMode::AlwaysAsk,
            },
            status: SessionStatus::Starting,
            created_at: 0,
            updated_at: 0,
            turns: Vec::new(),
            pending_requests: Vec::new(),
            token_usage: None,
            error: None,
            last_event_seq: 0,
        }
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            name: self.settings.name.clone(),
            status: self.status,
            updated_at: self.updated_at,
            last_event_seq: self.last_event_seq,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub name: Option<String>,
    pub status: SessionStatus,
    #[ts(type = "number")]
    pub updated_at: i64,
    #[ts(type = "number")]
    pub last_event_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum TurnStatus {
    InProgress,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub id: String,
    pub items: Vec<Item>,
    pub status: TurnStatus,
    pub error: Option<ErrorInfo>,
    pub input: Vec<UserInput>,
    pub client_name: String,
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput {
    Text { text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Item {
    UserMessage {
        id: String,
        content: Vec<UserInput>,
    },
    AgentMessage {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        summary: Vec<String>,
        content: Vec<String>,
    },
    CommandExecution {
        id: String,
        command: String,
        cwd: String,
        status: ItemStatus,
        aggregated_output: String,
        exit_code: Option<i32>,
        #[ts(type = "number | null")]
        duration_ms: Option<u64>,
    },
    FileChange {
        id: String,
        changes: Vec<FileUpdateChange>,
        status: ItemStatus,
    },
    FileRead {
        id: String,
        path: String,
        start_line: u32,
        content: String,
        status: ItemStatus,
    },
    ContextCompaction {
        id: String,
        summary: String,
    },
    #[serde(other)]
    Unknown,
}

impl Item {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::UserMessage { id, .. }
            | Self::AgentMessage { id, .. }
            | Self::Reasoning { id, .. }
            | Self::CommandExecution { id, .. }
            | Self::FileChange { id, .. }
            | Self::FileRead { id, .. }
            | Self::ContextCompaction { id, .. } => Some(id),
            Self::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct FileUpdateChange {
    pub path: String,
    pub kind: PatchChangeKind,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PatchChangeKind {
    Add,
    Delete,
    Update { move_path: Option<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    Decline,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub id: String,
    pub turn_id: String,
    pub item_id: String,
    pub tool: String,
    /// Full tool arguments, including a command or the proposed file changes.
    pub arguments: serde_json::Value,
    pub available_decisions: Vec<ApprovalDecision>,
    #[ts(type = "number")]
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ApprovalResolution {
    Answered { decision: ApprovalDecision },
    Cancelled,
    TimedOut,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    pub total: TokenUsageBreakdown,
    pub last: TokenUsageBreakdown,
    /// Current context occupancy, distinct from cumulative billed input tokens.
    #[ts(type = "number")]
    pub context_tokens: u64,
    #[ts(type = "number | null")]
    pub model_context_window: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageBreakdown {
    #[ts(type = "number")]
    pub input_tokens: u64,
    #[ts(type = "number")]
    pub cached_input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    #[ts(type = "number")]
    pub reasoning_output_tokens: u64,
    #[ts(type = "number")]
    pub total_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ErrorInfo {
    pub message: String,
    pub additional_details: Option<String>,
}
