//! Model-facing collaboration, implemented independently after reading Codex v1.
use crate::{CoreError, session::Shared};
use canter_model::{ToolCall, ToolDefinition};
use canter_protocol::{AgentType, ErrorCode};
use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(crate) const PROMPT: &str = "\n\nYou are a sub-agent working on a task assigned by the parent agent. Your final reply will be delivered to the parent. Do not ask the user questions; include unresolved questions or blockers in your final reply. You have your own checklist and scratch directory. Do not read or write memories.";

#[derive(Debug, Deserialize)]
#[serde(tag = "tool", rename_all = "snake_case", deny_unknown_fields)]
pub enum AgentCommand {
    SpawnAgent {
        task: String,
        #[serde(rename = "type")]
        kind: AgentType,
    },
    WaitAgent {
        ids: Vec<String>,
        #[serde(rename = "timeoutMs", default = "wait_timeout")]
        timeout_ms: u64,
    },
    SendInput {
        id: String,
        message: String,
    },
    CloseAgent {
        id: String,
    },
}
fn wait_timeout() -> u64 {
    30_000
}

/// The host owns the session registry and serializes family operations.
pub trait AgentHost: Send + Sync {
    fn execute(
        &self,
        parent: String,
        command: AgentCommand,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<Value, CoreError>>;
}

pub(crate) fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent" | "wait_agent" | "send_input" | "close_agent"
    )
}

pub(crate) fn definitions() -> [ToolDefinition; 4] {
    [
        ToolDefinition { name: "spawn_agent".into(), description: "Start a sub-agent with a fresh conversation and your current model and reasoning effort; returns its id and short name immediately. Use type explorer for read-only code lookup or research to save your main context; you may start explorers on your own. Use type general only when the user or applicable AGENTS.md explicitly requests sub-agents, delegation, or parallel agent work. Requests to investigate carefully or analyze deeply do not authorize general sub-agents. Before spawning, make your own plan; do work blocking your next step yourself and delegate independent work. Give a general agent explicit ownership of files or modules, and tell it that others share the repository: do not revert others' edits and accommodate their changes. At most four agents may run at once; children cannot spawn agents. Plan mode and review rounds allow only explorers.".into(), parameters: json!({"type":"object","properties":{"task":{"type":"string","minLength":1},"type":{"type":"string","enum":["explorer","general"]}},"required":["task","type"],"additionalProperties":false}) },
        ToolDefinition { name: "wait_agent".into(), description: "Wait until any listed sub-agent completes, fails, closes, or is interrupted, or the timeout expires. Returns every listed agent's current status and its final reply when available. Default timeout is 30000 ms, maximum 600000 ms. In a non-interactive exec turn, use this tool to receive results before ending your turn.".into(), parameters: json!({"type":"object","properties":{"ids":{"type":"array","minItems":1,"items":{"type":"string"}},"timeoutMs":{"type":"integer","minimum":1,"maximum":600000,"default":30000}},"required":["ids"],"additionalProperties":false}) },
        ToolDefinition { name: "send_input".into(), description: "Send a follow-up task or clarification to an existing sub-agent. In interactive sessions, queues input while it is running; starts another turn when it is idle or interrupted. In canter exec, first use wait_agent until the running sub-agent stops, then send_input. Use this to continue an interrupted agent; there is no resume_agent tool.".into(), parameters: json!({"type":"object","properties":{"id":{"type":"string"},"message":{"type":"string","minLength":1}},"required":["id","message"],"additionalProperties":false}) },
        ToolDefinition { name: "close_agent".into(), description: "Close a sub-agent, cancelling its active turn and stopping its background commands. A completed final reply is retained.".into(), parameters: json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}) },
    ]
}

impl crate::Session {
    pub fn set_agent_host(&self, host: Arc<dyn AgentHost>) {
        let _ = self.shared.agent_host.set(host);
    }
}

pub(crate) async fn execute(
    shared: &Shared,
    call: &ToolCall,
    cancel: &CancellationToken,
) -> Result<bool, CoreError> {
    if !is_tool(&call.name) {
        return Ok(false);
    }
    if let Some(reason) = crate::hooks::run(
        shared,
        crate::hooks::Event::PreToolUse,
        crate::hooks::tool_fields(call),
        cancel,
    )
    .await?
    {
        shared
            .state
            .lock()
            .await
            .tool_result(
                call,
                format!("PreToolUse hook blocked: {reason}"),
                false,
                Default::default(),
                None,
            )
            .await?;
        return Ok(true);
    }
    if cancel.is_cancelled() {
        return Ok(true);
    }
    let parent = shared.state.lock().await.journal.snapshot.id.clone();
    let result = async {
        let mut args: Value = serde_json::from_str(&call.arguments).map_err(|_| {
            CoreError::new(ErrorCode::InvalidRequest, "Invalid agent tool arguments")
        })?;
        let object = args.as_object_mut().ok_or_else(|| {
            CoreError::new(
                ErrorCode::InvalidRequest,
                "Agent tool arguments must be an object",
            )
        })?;
        object.insert("tool".into(), json!(call.name));
        let command = serde_json::from_value(args).map_err(|error| {
            CoreError::new(
                ErrorCode::InvalidRequest,
                &format!("Invalid agent tool arguments: {error}"),
            )
        })?;
        let host = shared.agent_host.get().ok_or_else(|| {
            CoreError::new(
                ErrorCode::InvalidRequest,
                "Sub-agent tools require a session host",
            )
        })?;
        host.execute(parent, command, cancel.clone()).await
    }
    .await;
    let success = result.is_ok();
    let mut text = result
        .map(|value| value.to_string())
        .unwrap_or_else(|error| error.message);
    let mut fields = crate::hooks::tool_fields(call);
    fields["tool_response"] = json!(text);
    if let Some(reason) =
        crate::hooks::run(shared, crate::hooks::Event::PostToolUse, fields, cancel).await?
    {
        text.push_str(&format!("\n\nPostToolUse hook: {reason}"));
    }
    shared
        .state
        .lock()
        .await
        .tool_result(call, text, success, Default::default(), None)
        .await?;
    Ok(true)
}
