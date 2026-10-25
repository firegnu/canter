use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Entries in the private model history, separate from client-facing events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    System {
        text: String,
    },
    User {
        text: String,
    },
    /// Ordered text and images. Text-only history keeps its original encoding.
    UserParts {
        content: Vec<UserContent>,
    },
    Assistant {
        content: Vec<AssistantContent>,
    },
    ToolResult {
        tool_call_id: String,
        text: String,
    },
    /// Skip when building a request after reading history from a newer version.
    /// Serialization fails rather than silently discarding the original payload.
    #[serde(other, skip_serializing)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserContent {
    Text {
        text: String,
    },
    Image {
        /// Filename relative to the owning session directory.
        path: String,
        name: String,
        /// Filled from the session copy for each request; never stored in history.
        #[serde(skip)]
        data_url: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantContent {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    ToolCall(ToolCall),
    /// Read-only compatibility marker; serializing it is an error.
    #[serde(other, skip_serializing)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Exact model output, including whitespace and invalid JSON.
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub options: RequestOptions,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestOptions {
    /// None leaves the provider's default unchanged.
    pub thinking: Option<bool>,
    pub reasoning_effort: Option<String>,
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub cache_read_tokens: Option<u64>,
    pub cache_miss_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCall(ToolCall),
    Usage(Usage),
    /// Successful transport completion. Inspect the reason before executing tools.
    Finished(FinishReason),
}
