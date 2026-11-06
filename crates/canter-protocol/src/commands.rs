//! Params describe JSON bodies, except GET params (query strings). Path values
//! are separate so private creation data is never part of a session snapshot.

use crate::{
    ApprovalDecision, Event, PermissionMode, ReasoningEffort, SessionSnapshot, SessionSummary,
    UserInput,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoalSetParams {
    pub objective: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub budget_usd: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct GoalResponse {
    pub goal: Option<crate::SessionGoal>,
    #[ts(type = "number")]
    pub event_seq: u64,
}

pub mod routes {
    pub const ACCOUNT_READ: &str = "/v1/account/read";
    pub const ACCOUNT_LOGIN_START: &str = "/v1/account/login/start";
    pub const ACCOUNT_LOGIN_WAIT: &str = "/v1/account/login/wait";
    pub const ACCOUNT_LOGIN_CANCEL: &str = "/v1/account/login/cancel";
    pub const ACCOUNT_SELECT: &str = "/v1/account/select";
    pub const ACCOUNT_BACKGROUND: &str = "/v1/account/background";
    pub const ACCOUNT_LOGOUT: &str = "/v1/account/logout";
    pub const MANAGED_CLIENT: &str = "/v1/managedClients/{id}";
    pub const MANAGED_CLIENT_HEADER: &str = "x-canter-managed-client";
    pub const MEMORIES: &str = "/v1/sessions/{id}/memories";
    pub const MEMORY_DELETE: &str = "/v1/sessions/{id}/memories/delete";
    pub const INFO: &str = "/v1/info";
    pub const DOCTOR: &str = "/v1/doctor";
    pub const RUNNING_TURNS: &str = "/v1/runningTurns";
    pub const FOLDER_TRUST: &str = "/v1/folderTrust";
    pub const COMMAND_RULES: &str = "/v1/commandRules";
    pub const COMMAND_RULE_DELETE: &str = "/v1/commandRules/delete";
    pub const FILE_RULE_DELETE: &str = "/v1/fileRules/delete";
    pub const MODELS: &str = "/v1/models";
    pub const USAGE: &str = "/v1/usage";
    pub const STATS: &str = "/v1/stats";
    pub const SESSION_STATS: &str = "/v1/sessions/{id}/stats";
    pub const SESSION_USAGE: &str = "/v1/sessions/{id}/usage";
    pub const SESSIONS: &str = "/v1/sessions";
    pub const SESSION: &str = "/v1/sessions/{id}";
    pub const SESSION_TRASH: &str = "/v1/sessions/{id}/trash";
    pub const SESSION_FORK: &str = "/v1/sessions/{id}/fork";
    pub const SESSION_AGENTS: &str = "/v1/sessions/{id}/agents";
    /// P2-179: POST, no body; returns the stopped child's `AgentSummary`.
    pub const SESSION_AGENT_STOP: &str = "/v1/sessions/{id}/agents/{agentId}/stop";
    pub const GOAL: &str = "/v1/sessions/{id}/goal";
    pub const GOAL_PAUSE: &str = "/v1/sessions/{id}/goal/pause";
    pub const GOAL_RESUME: &str = "/v1/sessions/{id}/goal/resume";
    pub const SKILLS: &str = "/v1/sessions/{id}/skills";
    pub const ROLES: &str = "/v1/sessions/{id}/roles";
    pub const MCP: &str = "/v1/mcp";
    pub const CONTEXT: &str = "/v1/sessions/{id}/context";
    pub const SANDBOX: &str = "/v1/sessions/{id}/sandbox";
    pub const BTW: &str = "/v1/sessions/{id}/btw";
    pub const HANDOFF: &str = "/v1/sessions/{id}/handoff";
    pub const RESUME: &str = "/v1/sessions/{id}/resume";
    pub const INPUT: &str = "/v1/sessions/{id}/input";
    pub const SHELL_COMMAND: &str = "/v1/sessions/{id}/shellCommand";
    pub const SHELL_BACKGROUND: &str = "/v1/sessions/{id}/shellBackground";
    pub const BACKGROUND_COMMANDS: &str = "/v1/sessions/{id}/backgroundCommands";
    pub const LANGUAGE_SERVERS: &str = "/v1/sessions/{id}/languageServers";
    pub const LANGUAGE_SERVER_STOP: &str = "/v1/sessions/{id}/languageServers/{serverId}/stop";
    pub const LANGUAGE_SERVER_RETRY: &str = "/v1/sessions/{id}/languageServers/{serverId}/retry";
    pub const DIAGNOSTICS: &str = "/v1/sessions/{id}/diagnostics";
    pub const BACKGROUND_OUTPUT: &str = "/v1/sessions/{id}/backgroundCommands/{commandId}";
    pub const BACKGROUND_STOP: &str = "/v1/sessions/{id}/backgroundCommands/{commandId}/stop";
    pub const BACKGROUND_STOP_ALL: &str = "/v1/sessions/{id}/backgroundCommands/stop";
    pub const INTERRUPT: &str = "/v1/sessions/{id}/interrupt";
    pub const REQUEST: &str = "/v1/sessions/{id}/requests/{rid}";
    pub const COMPACT: &str = "/v1/sessions/{id}/compact";
    pub const REWIND: &str = "/v1/sessions/{id}/rewind";
    pub const REWIND_PREVIEW: &str = "/v1/sessions/{id}/rewind/preview";
    pub const EVENTS: &str = "/v1/events";
}

/// GET query or POST body. POST records trust; restricted opens make no write.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct FolderTrustParams {
    pub cwd: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct FolderTrustResponse {
    /// Canonical repository root, or the folder itself outside a repository.
    pub path: String,
    pub trusted: bool,
    /// A failed lookup opens restricted instead of blocking the session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct SessionPath {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct BackgroundCommandListResponse {
    pub data: Vec<crate::BackgroundCommand>,
}

/// A decision in the server's command rules file. Unknown values are invalid.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum CommandRuleDecision {
    Allow,
    Prompt,
    Forbidden,
}

/// Identifies a command rule by its complete word prefix and decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct CommandRule {
    pub pattern: Vec<String>,
    pub decision: CommandRuleDecision,
}

/// File rules never grant access; unknown decisions are invalid.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, TS,
)]
#[serde(rename_all = "lowercase")]
pub enum FileRuleDecision {
    Prompt,
    Forbidden,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(deny_unknown_fields)]
pub struct FileRule {
    pub pattern: String,
    pub decision: FileRuleDecision,
}

/// Global command rules, read from the server's file at `path`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct CommandRuleListResponse {
    pub path: String,
    pub data: Vec<CommandRule>,
    #[serde(default, rename = "fileRules", skip_serializing_if = "Vec::is_empty")]
    #[ts(optional = nullable)]
    pub file_rules: Vec<FileRule>,
}

/// Result of deleting every exact match. Zero means the rule no longer exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct CommandRuleDeleteResponse {
    pub removed: usize,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ShellBackgroundParams {
    pub item_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ShellBackgroundResponse {
    #[ts(type = "number | null")]
    pub command_id: Option<u64>,
    #[ts(type = "number")]
    pub event_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionRewindParams {
    pub before_turn_id: String,
    pub rewind_files: bool,
    pub rewind_conversation: bool,
    #[serde(default)]
    pub confirmed_external_changes: Vec<String>,
    /// Preview's sequence; required when executing, omitted when requesting a preview.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub last_event_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RewindTurn {
    pub turn_id: String,
    pub input: Vec<UserInput>,
    pub file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct RewindListResponse {
    pub data: Vec<RewindTurn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RewindFile {
    pub path: String,
    pub delete: bool,
    pub external_change: bool,
    pub external_change_unknown: bool,
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct RewindPreviewResponse {
    pub files: Vec<RewindFile>,
    #[serde(rename = "lastEventSeq")]
    #[ts(type = "number")]
    pub last_event_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionRewindResponse {
    #[ts(type = "number")]
    pub event_seq: u64,
    pub input: Vec<UserInput>,
}

/// An ephemeral question. Its response stream is private to this HTTP caller.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct SessionBtwParams {
    pub question: String,
}

/// Generate a private handoff note from an idle session's current context.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct SessionHandoffParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub direction: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum BtwEvent {
    /// Replaces the answer after an attempt is discarded or an item is confirmed.
    Replace {
        text: String,
        interrupted: bool,
    },
    Retrying {
        message: String,
    },
    Delta {
        delta: String,
    },
    Completed,
    Cancelled,
    Error {
        message: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct RequestPath {
    pub id: String,
    pub rid: String,
}

macro_rules! empty_params {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
        pub struct $name {}
    )+};
}

empty_params!(
    InfoParams,
    ModelListParams,
    SessionListParams,
    SessionReadParams,
    SkillListParams,
    SessionCompactParams,
    SessionCloseParams
);

/// GET /v1/info. Each session carries its own replay cursor; there is no global sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct InfoResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub features: Option<Vec<String>>,
    pub server_version: String,
    pub protocol_version: u32,
    pub sessions: Vec<SessionSummary>,
}

/// GET /v1/models. The configured catalog is returned in full.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelListResponse {
    pub data: Vec<Model>,
    pub next_cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub catalogs: Option<Vec<ModelCatalogStatus>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogStatus {
    pub provider: String,
    pub stale: bool,
    pub refresh_failed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub fetched_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    pub model: String,
    pub display_name: String,
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub input_modalities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub supported_reasoning_efforts: Option<Vec<ReasoningEffort>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub default_reasoning_effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub missing_capabilities: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from_catalog: Option<bool>,
}

/// GET /v1/sessions/{id}/skills. Frozen when the session is opened.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct SkillListResponse {
    pub data: Vec<Skill>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: String,
    pub source: SkillSource,
    pub model_invocation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum SkillSource {
    Global,
    Project,
    #[serde(other)]
    Unknown,
}

/// GET /v1/sessions. Each summary carries its own lastEventSeq.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionListResponse {
    pub data: Vec<SessionSummary>,
}

/// GET /v1/runningTurns. Live work, including scheduled automatic continuation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct RunningTurnsResponse {
    pub data: Vec<RunningTurn>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RunningTurn {
    pub id: String,
    pub name: Option<String>,
    pub turn_id: Option<String>,
    pub activity: String,
}

/// POST /v1/sessions. Do not log this body: env may contain credentials.
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields = nullable)]
pub struct SessionCreateParams {
    /// Disable the sandbox for this opening, regardless of server configuration.
    #[serde(default, skip_serializing_if = "crate::types::is_false")]
    #[ts(optional = nullable)]
    pub sandbox_disabled: bool,
    /// Restrict this opening even if the server has a positive trust record.
    #[serde(default, skip_serializing_if = "crate::types::is_false")]
    #[ts(optional = nullable)]
    pub restricted_mode: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional = nullable)]
    pub tags: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional = nullable)]
    pub additional_directories: Vec<String>,
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub model: String,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub approval_policy: PermissionMode,
    pub input: Option<Vec<UserInput>>,
    pub name: Option<String>,
    pub client_name: String,
}

macro_rules! session_response {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            pub session: SessionSnapshot,
            #[ts(type = "number")]
            pub event_seq: u64,
        }
    )+};
}
session_response!(
    SessionCreateResponse,
    SessionReadResponse,
    SessionResumeResponse
);

/// POST /v1/sessions/{id}/resume. Never log this body or replace an open session's environment.
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct SessionResumeParams {
    /// False or omitted follows server configuration, not saved session state.
    #[serde(
        default,
        rename = "sandboxDisabled",
        skip_serializing_if = "crate::types::is_false"
    )]
    #[ts(optional = nullable)]
    pub sandbox_disabled: bool,
    #[serde(
        default,
        rename = "restrictedMode",
        skip_serializing_if = "crate::types::is_false"
    )]
    #[ts(optional = nullable)]
    pub restricted_mode: bool,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub enum InputMode {
    #[default]
    Queue,
    Steer,
    #[serde(other)]
    Unknown,
}

/// POST /v1/sessions/{id}/input. Idle starts a turn; mode applies when busy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionInputParams {
    /// Accepted via Tab / Right, including a subsequently edited suggestion.
    #[serde(default, skip_serializing_if = "crate::types::is_false")]
    #[ts(optional = nullable)]
    pub from_suggestion: bool,
    /// No client will answer requests in this turn. Accepted only while idle.
    #[serde(default, skip_serializing_if = "crate::types::is_false")]
    #[ts(optional = nullable)]
    pub non_interactive: bool,
    /// Override permissions for this turn only; never changes session settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub approval_policy: Option<PermissionMode>,
    /// Present for a read-only review turn; the single-line display title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub review: Option<String>,
    pub input: Vec<UserInput>,
    #[serde(default)]
    pub mode: InputMode,
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionInputResponse {
    #[ts(type = "number")]
    pub event_seq: u64,
    /// None while queued for a future turn.
    pub turn_id: Option<String>,
}

/// Run a user command as its own turn, only while the session is idle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionShellCommandParams {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionShellCommandResponse {
    #[ts(type = "number")]
    pub event_seq: u64,
    pub turn_id: String,
}

/// POST /v1/sessions/{id}/interrupt, with the target turn as a precondition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionInterruptParams {
    pub turn_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retract: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionInterruptResponse {
    #[ts(type = "number")]
    pub event_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub retracted: Option<bool>,
}

/// POST /v1/sessions/{id}/requests/{rid}.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestResolveParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub execpolicy_amendment: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plan: Option<crate::PlanAnswer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub decision: Option<ApprovalDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub feedback: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub answer: Option<crate::UserInputAnswer>,
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestResolveResponse {
    /// The original resolution's sequence when alreadyResolved is true.
    #[ts(type = "number")]
    pub event_seq: u64,
    pub already_resolved: bool,
}

/// PATCH /v1/sessions/{id}. Omitted/null scalar fields retain their current values.
/// Omitted/empty addDirectories appends nothing.
/// The resulting complete public settings are emitted in session.updated.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields = nullable)]
pub struct SessionUpdateParams {
    /// Compare before applying the update; empty means unnamed. Omitted is unconditional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_name: Option<String>,
    /// Append directories; never replaces or removes existing directories.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional = nullable)]
    pub add_directories: Vec<String>,
    pub plan_mode: Option<bool>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub approval_policy: Option<PermissionMode>,
}

macro_rules! command_response {
    ($($name:ident),+ $(,)?) => {$(
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            #[ts(type = "number")]
            pub event_seq: u64,
        }
    )+};
}
command_response!(SessionUpdateResponse, SessionCloseResponse);

/// Accepted compaction; completion is reported by the referenced turn's events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionCompactResponse {
    #[ts(type = "number")]
    pub event_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub turn_id: Option<String>,
}

/// GET /v1/events?session={id}&after={seq}. Zero requests the complete history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct EventsParams {
    pub session: String,
    #[serde(default)]
    #[ts(type = "number")]
    pub after: u64,
    /// Observe without joining the session's client lifecycle.
    #[serde(default, skip_serializing_if = "crate::types::is_false")]
    #[ts(optional = nullable)]
    pub observe: bool,
}

/// One SSE data frame, encoded as the event itself (not a JSON array).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(transparent)]
pub struct EventsResponse(pub Event);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct ErrorResponse {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    SessionNotFound,
    TurnMismatch,
    RequestNotFound,
    InvalidDecision,
    SessionClosed,
    SessionNotResumed,
    Conflict,
    Internal,
    #[serde(other)]
    Unknown,
}

pub const SSE_EVENT_NAME: &str = "message";

pub fn sse_frame(event: &Event) -> Result<String, serde_json::Error> {
    if event.payload.is_ephemeral() {
        return Ok(format!(
            "event: {SSE_EVENT_NAME}\ndata: {}\n\n",
            serde_json::to_string(event)?
        ));
    }
    Ok(format!(
        "id: {}\nevent: {SSE_EVENT_NAME}\ndata: {}\n\n",
        event.seq,
        serde_json::to_string(event)?
    ))
}

pub fn resume_after(after: u64, last_event_id: Option<&str>) -> Result<u64, ErrorResponse> {
    let Some(id) = last_event_id else {
        return Ok(after);
    };
    let invalid = || ErrorResponse {
        code: ErrorCode::InvalidRequest,
        message: "Last-Event-ID must be an unsigned decimal event sequence".into(),
    };
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    id.parse().map_err(|_| invalid())
}
