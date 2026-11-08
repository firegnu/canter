//! Conversation sessions independent of HTTP and terminal clients.
mod agent_tools;
mod agents;
mod background;
mod btw;
mod clients;
mod compaction;
#[cfg(test)]
mod compaction_tests;
mod configuration;
mod context;
mod directories;
mod fork;
mod goal;
pub mod hooks;
mod images;
mod journal;
mod long_run;
mod lsp;
mod memory;
mod plan_mode;
mod prompt;
mod response;
mod restore_files;
mod rewind;
pub mod roles;
mod runner;
mod running;
mod session;
mod skill_invocation;
mod skills;
pub mod stats;
mod suggestions;
mod telemetry;
pub mod usage;
mod user_shell;

pub use agent_tools::{AgentCommand, AgentHost, AgentToolOutput};
pub use agents::SessionMaintenance;
pub use agents::recover_agent_sessions;
pub use btw::BtwStream;
use canter_protocol::ErrorCode;
pub use canter_tools::mcp;
pub use clients::ClientConnection;
pub use configuration::{Configuration, ConfigurationSource};
pub use fork::ForkSnapshot;
pub use journal::Subscription;
pub use session::Session;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Default)]
pub struct RuntimeContext {
    /// This opening disables the sandbox even when server configuration enables it.
    pub sandbox_disabled: bool,
    /// Server-owned state directory; embedded callers opt in explicitly.
    pub memory_home: Option<PathBuf>,
    pub restricted_mode: bool,
    pub lsp_file_access: Option<canter_tools::lsp::FileAccess>,
    pub lsp: Option<std::sync::Arc<canter_tools::lsp::LspService>>,
    pub environment: BTreeMap<String, String>,
    pub protected_paths: Vec<PathBuf>,
    pub mcp_tools: Vec<mcp::McpTool>,
}

#[derive(Clone, Debug)]
pub struct ModelLimits {
    pub context_window: u64,
    pub auto_compact_tokens: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct ModelReasoning {
    pub supported: Vec<canter_protocol::ReasoningEffort>,
    pub default: canter_protocol::ReasoningEffort,
}

impl Default for ModelReasoning {
    fn default() -> Self {
        Self {
            supported: canter_protocol::ReasoningEffort::LEGACY.to_vec(),
            default: canter_protocol::ReasoningEffort::High,
        }
    }
}

impl ModelReasoning {
    pub(crate) fn reset_unsupported(
        &self,
        settings: &mut canter_protocol::SessionSettings,
    ) -> Option<String> {
        let previous = match settings.reasoning_effort {
            canter_protocol::ReasoningEffort::Unknown => canter_protocol::ReasoningEffort::High,
            effort => effort,
        };
        if self.supported.contains(&previous) {
            return None;
        }
        settings.reasoning_effort = self.default;
        Some(format!(
            "Thinking effort changed from {} to {} for {}.",
            previous.as_str(),
            self.default.as_str(),
            settings.model
        ))
    }

    pub(crate) fn validate(
        &self,
        model: &str,
        effort: canter_protocol::ReasoningEffort,
    ) -> Result<(), CoreError> {
        if effort != canter_protocol::ReasoningEffort::Unknown && self.supported.contains(&effort) {
            return Ok(());
        }
        Err(CoreError::new(
            ErrorCode::InvalidRequest,
            &format!(
                "Thinking effort '{}' is not supported by model '{}'. Supported efforts: {}.",
                effort.as_str(),
                model,
                self.supported
                    .iter()
                    .map(|effort| effort.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ))
    }
}

#[derive(Clone, Debug)]
pub struct CoreOptions {
    /// Server-owned directory; embedded callers opt in with an isolated path.
    pub agent_roles_directory: Option<PathBuf>,
    pub agent_role_providers: std::collections::BTreeSet<String>,
    /// Embedded callers opt in; config.json enables suggestions by default.
    pub prompt_suggestions: bool,
    pub hooks: hooks::Hooks,
    pub telemetry: bool,
    /// Embedded callers opt in explicitly; the server's config.json defaults to true.
    pub sandbox: bool,
    /// Global server-owned command rules; embedders may leave this disabled.
    pub command_rules_path: Option<PathBuf>,
    pub auto_wake_limit: u32,
    pub goal_budget_usd: f64,
    /// Goal continuation questions wait five minutes; injectable for local replay tests.
    pub goal_question_timeout: std::time::Duration,
    pub plan_enabled: bool,
    pub mcp_tool_search: bool,
    pub context_window: u64,
    pub max_output_tokens: u32,
    /// Compact automatically past this many context tokens instead of the
    /// window's own line, when lower than it.
    pub auto_compact_tokens: Option<u64>,
    pub image_models: Vec<String>,
    /// Models whose verified catalog entry supports the freeform patch tool.
    pub apply_patch_models: std::collections::BTreeSet<String>,
    /// Per-model limits; names absent here retain the legacy global limits.
    pub model_limits: std::collections::BTreeMap<String, ModelLimits>,
    pub model_reasoning: BTreeMap<String, ModelReasoning>,
    pub prices: usage::PriceBook,
    /// Providers whose configured auth charges requests to the ChatGPT plan.
    pub chatgpt_plan_providers: std::collections::BTreeSet<String>,
    /// Clock at the model-request boundary; replaceable for deterministic replay tests.
    pub request_time: fn() -> i64,
    /// Total time for one web_fetch (30 seconds); replaceable so tests need not wait.
    pub web_fetch_timeout: std::time::Duration,
    /// Optional search service, fixed for the duration of a turn; contains no public credentials.
    pub web_search: Option<canter_tools::web_search::WebSearch>,
    /// Wait after a file change for language-server diagnostics (500 ms); replaceable for tests.
    pub lsp_inline_wait: std::time::Duration,
    /// How long a file change's diagnostics stay deliverable (60 seconds); replaceable so tests need not wait.
    pub lsp_delivery_window: std::time::Duration,
}
impl Default for CoreOptions {
    fn default() -> Self {
        Self {
            agent_roles_directory: None,
            agent_role_providers: Default::default(),
            prompt_suggestions: false,
            hooks: Default::default(),
            telemetry: true,
            sandbox: false,
            command_rules_path: None,
            auto_wake_limit: 10,
            goal_budget_usd: 5.0,
            goal_question_timeout: std::time::Duration::from_secs(300),
            plan_enabled: true,
            mcp_tool_search: true,
            context_window: 1_048_576,
            max_output_tokens: 65_536,
            auto_compact_tokens: None,
            image_models: vec!["deepseek-flash".into()],
            apply_patch_models: Default::default(),
            model_limits: Default::default(),
            model_reasoning: Default::default(),
            prices: usage::builtin_prices(),
            chatgpt_plan_providers: Default::default(),
            request_time: journal::now,
            web_fetch_timeout: std::time::Duration::from_secs(30),
            web_search: None,
            lsp_inline_wait: std::time::Duration::from_millis(500),
            lsp_delivery_window: std::time::Duration::from_secs(60),
        }
    }
}

impl CoreOptions {
    pub fn reasoning(&self, model: &str) -> ModelReasoning {
        self.model_reasoning.get(model).cloned().unwrap_or_default()
    }

    pub fn limits(&self, model: &str) -> ModelLimits {
        self.model_limits
            .get(model)
            .cloned()
            .unwrap_or(ModelLimits {
                context_window: self.context_window,
                auto_compact_tokens: self.auto_compact_tokens,
            })
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
