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

pub mod routes {
    pub const INFO: &str = "/v1/info";
    pub const MODELS: &str = "/v1/models";
    pub const USAGE: &str = "/v1/usage";
    pub const SESSION_USAGE: &str = "/v1/sessions/{id}/usage";
    pub const SESSIONS: &str = "/v1/sessions";
    pub const SESSION: &str = "/v1/sessions/{id}";
    pub const SKILLS: &str = "/v1/sessions/{id}/skills";
    pub const MCP: &str = "/v1/mcp";
    pub const CONTEXT: &str = "/v1/sessions/{id}/context";
    pub const BTW: &str = "/v1/sessions/{id}/btw";
    pub const HANDOFF: &str = "/v1/sessions/{id}/handoff";
    pub const RESUME: &str = "/v1/sessions/{id}/resume";
    pub const INPUT: &str = "/v1/sessions/{id}/input";
    pub const INTERRUPT: &str = "/v1/sessions/{id}/interrupt";
    pub const REQUEST: &str = "/v1/sessions/{id}/requests/{rid}";
    pub const COMPACT: &str = "/v1/sessions/{id}/compact";
    pub const EVENTS: &str = "/v1/events";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct SessionPath {
    pub id: String,
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

/// POST /v1/sessions. Do not log this body: env may contain credentials.
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields = nullable)]
pub struct SessionCreateParams {
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

/// POST /v1/sessions/{id}/interrupt, with the target turn as a precondition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct SessionInterruptParams {
    pub turn_id: String,
}

/// POST /v1/sessions/{id}/requests/{rid}.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct RequestResolveParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub decision: Option<ApprovalDecision>,
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

/// PATCH /v1/sessions/{id}. Omitted/null fields retain their current values.
/// The resulting complete public settings are emitted in session.updated.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields = nullable)]
pub struct SessionUpdateParams {
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
command_response!(
    SessionInterruptResponse,
    SessionUpdateResponse,
    SessionCloseResponse
);

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
