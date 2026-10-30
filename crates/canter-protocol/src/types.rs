use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum PermissionMode {
    AlwaysAsk,
    AskForExecution,
    AllowAll,
    #[serde(other)]
    Unknown,
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
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum SessionCloseReason {
    ClientRequest,
    ServerShutdown,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Low,
    #[default]
    High,
    Max,
    #[serde(other)]
    Unknown,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Low => "low",
            Self::High => "high",
            Self::Max => "max",
            Self::Unknown => "unknown",
        }
    }
}

/// Public settings. Environment variables never belong here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSettings {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional = nullable)]
    pub additional_directories: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    #[ts(optional = nullable)]
    pub plan_mode: bool,
    /// Permission mode to restore when the user leaves planning without a selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pre_plan_approval_policy: Option<PermissionMode>,
    /// Effective tool switch, frozen when the server opens this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan_enabled: Option<bool>,
    /// Whether this session's model commands run in the sandbox, as the server
    /// opened it; absent from servers that do not report it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sandbox_enabled: Option<bool>,
    #[serde(default)]
    pub cwd: String,
    pub name: Option<String>,
    pub model: String,
    #[serde(default)]
    #[ts(optional = nullable)]
    pub reasoning_effort: ReasoningEffort,
    pub approval_policy: PermissionMode,
}

fn is_false(value: &bool) -> bool {
    !value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional = nullable)]
    pub language_servers: Vec<crate::LanguageServer>,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub goal: Option<SessionGoal>,
    pub settings: SessionSettings,
    pub status: SessionStatus,
    /// Why the session is closed; absent for open sessions and legacy data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub close_reason: Option<SessionCloseReason>,
    /// Unix time in milliseconds; zero until the first event is applied.
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
    pub turns: Vec<Turn>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional = nullable)]
    pub background_commands: Vec<BackgroundCommand>,
    pub pending_requests: Vec<PendingRequest>,
    /// User feedback from declined approvals, keyed by tool item ID.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    #[ts(optional = nullable)]
    pub approval_feedback: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub queued_inputs: Vec<QueuedInput>,
    pub token_usage: Option<TokenUsage>,
    pub error: Option<ErrorInfo>,
    #[ts(type = "number")]
    pub last_event_seq: u64,
}

impl SessionSnapshot {
    /// Empty replay state. The first durable event supplies public settings.
    pub fn empty(id: impl Into<String>) -> Self {
        Self {
            language_servers: Vec::new(),
            id: id.into(),
            goal: None,
            settings: SessionSettings {
                additional_directories: Vec::new(),
                plan_mode: false,
                pre_plan_approval_policy: None,
                plan_enabled: None,
                sandbox_enabled: None,
                cwd: String::new(),
                name: None,
                model: String::new(),
                reasoning_effort: ReasoningEffort::default(),
                approval_policy: PermissionMode::AlwaysAsk,
            },
            status: SessionStatus::Starting,
            close_reason: None,
            created_at: 0,
            updated_at: 0,
            turns: Vec::new(),
            background_commands: Vec::new(),
            pending_requests: Vec::new(),
            approval_feedback: Default::default(),
            queued_inputs: Vec::new(),
            token_usage: None,
            error: None,
            last_event_seq: 0,
        }
    }

    pub fn summary(&self) -> SessionSummary {
        SessionSummary {
            id: self.id.clone(),
            cwd: self.settings.cwd.clone(),
            name: self.settings.name.clone(),
            status: self.status,
            updated_at: self.updated_at,
            last_event_seq: self.last_event_seq,
        }
    }

    /// The latest successful replacement, including an explicitly empty plan.
    /// Deriving it from retained turns also handles conversation rewinds.
    pub fn current_plan(&self) -> Option<&[PlanStep]> {
        self.turns
            .iter()
            .rev()
            .flat_map(|turn| turn.items.iter().rev())
            .find_map(|item| match item {
                Item::Plan {
                    plan,
                    status: ItemStatus::Completed,
                    ..
                } => Some(plan.as_slice()),
                _ => None,
            })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionGoal {
    pub id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub budget_usd: f64,
    pub cost: UsageCost,
    #[ts(type = "number")]
    pub created_at: i64,
    #[ts(type = "number")]
    pub updated_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum GoalStatus {
    Active,
    Paused,
    Complete,
    Blocked,
    BudgetLimited,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    #[serde(default)]
    pub cwd: String,
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
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review: Option<String>,
    pub id: String,
    pub items: Vec<Item>,
    pub status: TurnStatus,
    pub error: Option<ErrorInfo>,
    pub input: Vec<UserInput>,
    pub client_name: String,
    pub usage: Option<TokenUsage>,
    /// Unix seconds of `turn.started`. The reducer fills the three times from
    /// event timestamps; absent means unknown (old data).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub started_at: Option<i64>,
    /// Unix seconds of `turn.completed`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub completed_at: Option<i64>,
    /// Time worked so far: up to the latest event while the turn runs, the
    /// whole turn once it has ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct QueuedInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review: Option<String>,
    pub id: String,
    pub input: Vec<UserInput>,
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInput {
    Skill {
        name: String,
    },
    Text {
        text: String,
    },
    LocalImage {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        name: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    Interrupted,
    Blocked,
    InProgress,
    Completed,
    Failed,
    Declined,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct PlanStep {
    pub step: String,
    pub status: PlanStepStatus,
}

/// A search result shared by all search providers. Dates are RFC 3339 when known.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<String>,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Item {
    Diagnostics {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        source_item_id: Option<String>,
        status: crate::DiagnosticStatus,
        files: Vec<crate::DiagnosticFile>,
        total_count: u32,
        shown_count: u32,
        omitted_count: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        resolved_count: Option<u32>,
        /// Errors and warnings in `totalCount`; missing in items saved before P2-135.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        error_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        warning_count: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        delivery: Option<crate::DiagnosticDelivery>,
    },
    ModelRetry {
        id: String,
        error: ErrorInfo,
        will_retry: bool,
        attempt: u32,
        max_retries: u32,
        delay_seconds: f64,
    },
    BackgroundEvent {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        command_id: Option<u64>,
        text: String,
    },
    BackgroundCommandAction {
        id: String,
        #[ts(type = "number")]
        command_id: u64,
        action: String,
        result: String,
        status: ItemStatus,
    },
    PlanProposal {
        id: String,
        plan: String,
        answer: Option<PlanAnswer>,
        status: ItemStatus,
    },
    Plan {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        explanation: Option<String>,
        plan: Vec<PlanStep>,
        status: ItemStatus,
    },
    UserMessage {
        id: String,
        content: Vec<UserInput>,
    },
    AgentMessage {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<ItemStatus>,
    },
    Reasoning {
        id: String,
        summary: Vec<String>,
        content: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        status: Option<ItemStatus>,
        /// Thinking time from `item.started` up to its latest delta, filled by
        /// the reducer; absent means unknown. Not in Codex.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional, type = "number")]
        duration_ms: Option<u64>,
    },
    /// Provider-independent search results; only search is supported, so there is no action.
    WebSearch {
        id: String,
        query: String,
        results: Vec<WebSearchResult>,
        status: ItemStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        error: Option<String>,
    },
    WebFetch {
        id: String,
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        resolved_url: Option<String>,
        result: String,
        status: ItemStatus,
    },
    McpToolSearch {
        id: String,
        query: String,
        tools: Vec<String>,
        status: ItemStatus,
    },
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        arguments: serde_json::Value,
        result: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(optional = nullable)]
        content_items: Vec<ToolContentItem>,
        status: ItemStatus,
    },
    CommandExecution {
        id: String,
        #[serde(default, skip_serializing_if = "ExecCommandSource::is_agent")]
        #[ts(optional = nullable)]
        source: ExecCommandSource,
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
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(optional = nullable)]
        content_items: Vec<ToolContentItem>,
        status: ItemStatus,
    },
    ContextCompaction {
        id: String,
        summary: String,
    },
    UserInput {
        id: String,
        question: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        answer: Option<UserInputAnswer>,
        status: ItemStatus,
    },
    #[serde(other)]
    Unknown,
}

/// Tool attachments refer to durable copies in the owning session directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ToolContentItem {
    #[serde(rename_all = "camelCase")]
    LocalImage {
        path: String,
        name: String,
        mime_type: String,
        #[ts(type = "number")]
        size_bytes: u64,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ExecCommandSource {
    #[default]
    Agent,
    UserShell,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundCommand {
    #[ts(type = "number")]
    pub id: u64,
    pub item_id: String,
    pub command: String,
    #[ts(type = "number")]
    pub started_at: i64,
    #[ts(type = "number | null")]
    pub completed_at: Option<i64>,
    pub running: bool,
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundCommandOutput {
    pub command: BackgroundCommand,
    pub output: String,
    #[ts(type = "number")]
    pub skipped_bytes: u64,
}

impl ExecCommandSource {
    fn is_agent(&self) -> bool {
        *self == Self::Agent
    }
}

impl Item {
    pub fn retry_message(&self) -> Option<String> {
        match self {
            Self::ModelRetry {
                error,
                attempt,
                max_retries,
                delay_seconds,
                ..
            } => Some(model_retry_message(
                &error.message,
                *attempt,
                *max_retries,
                *delay_seconds,
            )),
            _ => None,
        }
    }
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Diagnostics { id, .. } => Some(id),
            Self::ModelRetry { id, .. } => Some(id),
            Self::BackgroundCommandAction { id, .. } | Self::BackgroundEvent { id, .. } => Some(id),
            Self::UserMessage { id, .. }
            | Self::Plan { id, .. }
            | Self::PlanProposal { id, .. }
            | Self::AgentMessage { id, .. }
            | Self::Reasoning { id, .. }
            | Self::McpToolCall { id, .. }
            | Self::McpToolSearch { id, .. }
            | Self::WebFetch { id, .. }
            | Self::WebSearch { id, .. }
            | Self::CommandExecution { id, .. }
            | Self::FileChange { id, .. }
            | Self::FileRead { id, .. }
            | Self::UserInput { id, .. }
            | Self::ContextCompaction { id, .. } => Some(id),
            Self::Unknown => None,
        }
    }
}

pub fn model_retry_message(
    reason: &str,
    attempt: u32,
    max_retries: u32,
    delay_seconds: f64,
) -> String {
    format!("Retrying in {delay_seconds:.1}s ({attempt}/{max_retries}): {reason}")
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
    Update {
        move_path: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    Accept,
    AcceptForSession,
    AcceptWithExecpolicyAmendment,
    Decline,
    DeclineWithFeedback,
    Cancel,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct PendingRequest {
    pub id: String,
    pub turn_id: String,
    #[ts(type = "number")]
    pub created_at: i64,
    pub kind: RequestKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RequestKind {
    PlanApproval(PlanApprovalRequest),
    Approval(ApprovalRequest),
    SandboxApproval(ApprovalRequest),
    UserInput(UserInputRequest),
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum PlanDecision {
    Implement,
    ClearContext,
    KeepPlanning,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanAnswer {
    pub decision: PlanDecision,
    pub approval_policy: PermissionMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feedback: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct PlanApprovalRequest {
    pub item_id: String,
    pub plan: String,
    pub approval_policy: PermissionMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub item_id: String,
    pub tool: String,
    /// Full tool arguments, including a command or the proposed file changes.
    pub arguments: serde_json::Value,
    pub available_decisions: Vec<ApprovalDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposed_execpolicy_amendment: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub session_prefixes: Option<Vec<Vec<String>>>,
}

/// Extra scope for one model command; it never disables the sandbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SandboxRequest {
    #[serde(default)]
    pub network: bool,
    #[serde(default)]
    pub write_paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct UserInputRequest {
    pub item_id: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserInputAnswer {
    Option {
        label: String,
    },
    Text {
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ApprovalResolution {
    Plan {
        answer: PlanAnswer,
    },
    Answered {
        decision: ApprovalDecision,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        feedback: Option<String>,
    },
    UserInput {
        answer: UserInputAnswer,
    },
    Cancelled,
    TimedOut,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// Absent in old events; present with a null cost for an unpriced request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub request: Option<RequestCharge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cost: Option<UsageCost>,
    pub total: TokenUsageBreakdown,
    pub last: TokenUsageBreakdown,
    /// Current context occupancy, distinct from cumulative billed input tokens.
    #[ts(type = "number")]
    pub context_tokens: u64,
    #[ts(type = "number | null")]
    pub model_context_window: Option<u64>,
    /// The context occupancy past which the server compacts automatically.
    /// Absent in old events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub auto_compact_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestCharge {
    pub model: String,
    #[ts(type = "number")]
    pub started_at: i64,
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct UsageCost {
    pub usd: f64,
    pub unknown: bool,
}

impl UsageCost {
    pub fn add(&mut self, other: &Self) {
        self.usd += other.usd;
        self.unknown |= other.unknown;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub model: Option<String>,
    #[ts(type = "number")]
    pub cached_input_tokens: u64,
    #[ts(type = "number")]
    pub uncached_input_tokens: u64,
    #[ts(type = "number")]
    pub output_tokens: u64,
    pub cost: UsageCost,
}

impl UsageRow {
    pub fn add(&mut self, other: &Self) {
        self.cached_input_tokens = self
            .cached_input_tokens
            .saturating_add(other.cached_input_tokens);
        self.uncached_input_tokens = self
            .uncached_input_tokens
            .saturating_add(other.uncached_input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(other.output_tokens);
        self.cost.add(&other.cost);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct UsageResponse {
    pub data: Vec<UsageRow>,
    pub total: UsageRow,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_turn_cost: Option<UsageCost>,
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

/// GET /v1/sessions/{id}/sandbox: what this session's model commands may touch
/// now, worked out on each request. Only `enabled` is meaningful when false.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionSandboxResponse {
    pub enabled: bool,
    /// cwd, added directories, the session's private temporary directory and caches.
    #[serde(default)]
    pub writable: Vec<String>,
    /// Readable but never writable, besides Git config and hooks.
    #[serde(default)]
    pub read_only: Vec<String>,
    /// Neither readable nor writable.
    #[serde(default)]
    pub no_access: Vec<String>,
    /// Whether commands reach the network without a one-command approval,
    /// under the current permission mode.
    #[serde(default)]
    pub network: bool,
}

/// Read-only estimate of the currently loaded context, using the compaction heuristic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextResponse {
    pub prefix: Vec<ContextSection>,
    pub skill_count: usize,
    pub tool_count: usize,
    #[ts(type = "number")]
    pub tool_tokens: u64,
    #[ts(type = "number | null")]
    pub compaction_tokens: Option<u64>,
    pub messages: ContextMessages,
    #[ts(type = "number")]
    pub estimated_tokens: u64,
    #[ts(type = "number")]
    pub free_tokens: u64,
    #[ts(type = "number")]
    pub model_context_window: u64,
    #[ts(type = "number | null")]
    pub context_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextSection {
    pub kind: ContextSectionKind,
    pub path: Option<String>,
    #[ts(type = "number")]
    pub tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum ContextSectionKind {
    System,
    ModelFamily,
    GlobalInstructions,
    ProjectInstructions,
    Skills,
    Environment,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ContextMessages {
    pub count: usize,
    #[ts(type = "number")]
    pub tokens: u64,
    #[ts(type = "number")]
    pub user: u64,
    #[ts(type = "number")]
    pub assistant: u64,
    #[ts(type = "number")]
    pub tool_results: u64,
    #[ts(type = "number")]
    pub images: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpToolInfo {
    pub name: String,
    pub tool: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpServerStatus {
    pub name: String,
    pub connected: bool,
    pub error: Option<String>,
    pub tools: Vec<McpToolInfo>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct McpListResponse {
    pub data: Vec<McpServerStatus>,
}
