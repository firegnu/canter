//! Conversation sessions independent of HTTP and terminal clients.
mod background;
mod btw;
mod compaction;
mod context;
mod directories;
mod goal;
mod images;
mod journal;
mod plan_mode;
mod prompt;
mod response;
mod rewind;
mod runner;
mod session;
mod skill_invocation;
mod skills;
pub mod usage;
mod user_shell;

pub use btw::BtwStream;
use canter_protocol::ErrorCode;
pub use canter_tools::mcp;
pub use journal::Subscription;
pub use session::Session;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Default)]
pub struct RuntimeContext {
    pub lsp: Option<std::sync::Arc<canter_tools::lsp::LspService>>,
    pub environment: BTreeMap<String, String>,
    pub protected_paths: Vec<PathBuf>,
    pub mcp_tools: Vec<mcp::McpTool>,
}

#[derive(Clone, Debug)]
pub struct CoreOptions {
    /// Global server-owned command rules; embedders may leave this disabled.
    pub command_rules_path: Option<PathBuf>,
    pub auto_wake_limit: u32,
    pub goal_budget_usd: f64,
    /// Goal continuation questions wait five minutes; injectable for local replay tests.
    pub goal_question_timeout: std::time::Duration,
    pub plan_enabled: bool,
    pub context_window: u64,
    pub max_output_tokens: u32,
    /// Compact automatically past this many context tokens instead of the
    /// window's own line, when lower than it.
    pub auto_compact_tokens: Option<u64>,
    pub image_models: Vec<String>,
    pub prices: usage::PriceBook,
    /// Clock at the model-request boundary; replaceable for deterministic replay tests.
    pub request_time: fn() -> i64,
    /// Total time for one web_fetch (30 seconds); replaceable so tests need not wait.
    pub web_fetch_timeout: std::time::Duration,
}
impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            command_rules_path: None,
            auto_wake_limit: 10,
            goal_budget_usd: 5.0,
            goal_question_timeout: std::time::Duration::from_secs(300),
            plan_enabled: true,
            context_window: 1_048_576,
            max_output_tokens: 65_536,
            auto_compact_tokens: None,
            image_models: vec!["deepseek-flash".into()],
            prices: usage::builtin_prices(),
            request_time: journal::now,
            web_fetch_timeout: std::time::Duration::from_secs(30),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: String,
}
impl CoreError {
    pub(crate) fn new(code: ErrorCode, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
    pub(crate) fn internal() -> Self {
        Self::new(ErrorCode::Internal, "session operation failed")
    }
}
impl From<canter_store::StoreError> for CoreError {
    fn from(error: canter_store::StoreError) -> Self {
        use canter_store::StoreError;
        match error {
            StoreError::InvalidId => Self::new(ErrorCode::InvalidRequest, "invalid session id"),
            StoreError::Locked => Self::new(ErrorCode::Conflict, "session already open"),
            StoreError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::new(ErrorCode::SessionNotFound, "session not found")
            }
            StoreError::Io(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                Self::new(ErrorCode::Conflict, "session already exists")
            }
            _ => Self::internal(),
        }
    }
}
