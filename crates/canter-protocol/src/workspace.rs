//! Read-only queries of an open session's server-side workspace.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub branch: Option<String>,
    #[ts(type = "[number, number] | null")]
    pub changed_lines: Option<(u64, u64)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct WorkspaceOutput {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct WorkspaceDiff {
    pub root: String,
    pub staged: WorkspaceOutput,
    pub unstaged: WorkspaceOutput,
    pub untracked: WorkspaceOutput,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WorkspaceStatus {
    Repository {
        branch: String,
        entries: Vec<(String, String)>,
    },
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceReviewParams {
    #[serde(default)]
    pub target: String,
    pub client_name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct WorkspaceEntry {
    /// Relative to cwd; entries in additional directories use absolute paths.
    /// Directories end in `/`, as in the terminal's existing completion list.
    pub label: String,
    pub dir: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
pub struct WorkspaceEntriesBatch {
    pub entries: Vec<WorkspaceEntry>,
    pub truncated: bool,
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<String>,
}
