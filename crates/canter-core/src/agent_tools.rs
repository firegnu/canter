//! Model-facing collaboration, implemented independently after reading Codex v1.
use crate::{CoreError, session::Shared};
use canter_model::{ToolCall, ToolDefinition};
use canter_protocol::{
    AgentDepth, AgentStatus, AgentType, CollabAgent, CollabAgentTool, ErrorCode, EventPayload,
    Item, ItemStatus,
};
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
        #[serde(default)]
        name: Option<String>,
        #[serde(rename = "type")]
        kind: AgentType,
        /// P2-255: never [`AgentDepth::Unknown`] once parsed.
        depth: AgentDepth,
    },
    WaitAgent {
        ids: Vec<String>,
        #[serde(rename = "timeoutMs", default = "wait_timeout")]
        timeout_ms: u64,
        /// Set by the core for interactive turns, never by model arguments.
        #[serde(skip)]
        no_wait: bool,
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

/// What a sub-agent tool returns.
pub struct AgentToolOutput {
    /// The result the model gets, as JSON.
    pub value: Value,
    /// The agents the call named or started, as its card shows them after.
    pub agents: Vec<CollabAgent>,
}

/// The host owns the session registry and serializes family operations.
pub trait AgentHost: Send + Sync {
    fn execute(
        &self,
        parent: String,
        command: AgentCommand,
        cancel: CancellationToken,
    ) -> BoxFuture<'_, Result<AgentToolOutput, CoreError>>;
    /// The parent's children with these ids as last known, for the card of a
    /// call still running. An id that is not one of them keeps its id as name.
    fn describe(&self, parent: String, ids: Vec<String>) -> BoxFuture<'_, Vec<CollabAgent>>;
}

pub(crate) fn is_tool(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent" | "wait_agent" | "send_input" | "close_agent"
    )
}

pub(crate) fn definitions() -> [ToolDefinition; 4] {
    [
        ToolDefinition { name: "spawn_agent".into(), description: "Start a sub-agent with a fresh conversation and your current model and reasoning effort; returns its id and short name immediately. Use type explorer for read-only code lookup or research to save your main context; you may start explorers on your own. When research splits into independent parts that each need many files read, such as comparing how several projects or modules handle something, start one explorer per part in the same reply so they run at the same time. Give each one only its part of the user's question without widening the scope, and set depth instead of writing it into the task: quick (find the main places), medium (the default: answer each question with file:line evidence, then stop), or thorough (only when the user asks for exhaustive coverage). Then tell the user what each one is checking and end your reply instead of calling wait_agent, so the user can keep talking; their reports arrive on their own and wake you to combine them. When a report wakes you while other sub-agents you started are still running, tell the user in a sentence what arrived and end your turn; combine them when the last one arrives. Never say a report has arrived before you have seen it. Don't research those parts yourself meanwhile. When a few reads answer the question, do it yourself. Use type general only when the user or applicable AGENTS.md explicitly requests sub-agents, delegation, or parallel agent work. Requests to investigate carefully or analyze deeply do not authorize general sub-agents. Before spawning, make your own plan; do work blocking your next step yourself and delegate independent work. Give a general agent explicit ownership of files or modules, and tell it that others share the repository: do not revert others' edits and accommodate their changes. At most four agents may run at once; children cannot spawn agents. Plan mode and review rounds allow only explorers.".into(), parameters: json!({"type":"object","properties":{"task":{"type":"string","minLength":1},"type":{"type":"string","enum":["explorer","general"]},"depth":{"type":"string","enum":["quick","medium","thorough"]},"name":{"type":"string","description":"A short name of 3 to 5 English words describing what this sub-agent does, shown to the user in lists and cards."}},"required":["task","type","depth","name"],"additionalProperties":false}) },
        ToolDefinition { name: "wait_agent".into(), description: "Returns every listed sub-agent's current status and its final reply when available. In an interactive session with auto wake enabled, user-started and report-woken turns return at once without waiting. If sub-agents are still running, reply to the user and end your turn; their reports arrive on their own and wake you. With auto wake disabled or in an automatic goal continuation, wait until any listed sub-agent completes, fails, closes, or is interrupted, or the timeout expires. Default timeout is 30000 ms, maximum 600000 ms. In a non-interactive exec turn, use this tool to receive results before ending your turn.".into(), parameters: json!({"type":"object","properties":{"ids":{"type":"array","minItems":1,"items":{"type":"string"}},"timeoutMs":{"type":"integer","minimum":1,"maximum":600000,"default":30000}},"required":["ids"],"additionalProperties":false}) },
        ToolDefinition { name: "send_input".into(), description: "Send a follow-up task or clarification to an existing sub-agent. In interactive sessions, queues input while it is running; starts another turn when it is idle or interrupted. In canter exec, first use wait_agent until the running sub-agent stops, then send_input. Use this to continue an interrupted agent; there is no resume_agent tool.".into(), parameters: json!({"type":"object","properties":{"id":{"type":"string"},"message":{"type":"string","minLength":1}},"required":["id","message"],"additionalProperties":false}) },
        ToolDefinition { name: "close_agent".into(), description: "Close a sub-agent, cancelling its active turn and stopping its background commands. A completed final reply is retained.".into(), parameters: json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}) },
    ]
}

pub(crate) fn definitions_for_roles(roles: &[crate::roles::Role]) -> [ToolDefinition; 4] {
    let mut definitions = definitions();
    let available: Vec<_> = roles
        .iter()
        .filter(|role| role.summary.error.is_none())
        .collect();
    if available.is_empty() {
        return definitions;
    }
    let spawn = &mut definitions[0];
    spawn.description = "Start a sub-agent with a fresh conversation; returns its id and short name immediately. Choose the role by its description, or use the role explicitly named by the user. Read-only roles may be started on your own. When research splits into independent parts that each need many files read, such as comparing how several projects or modules handle something, start one read-only agent per part in the same reply so they run at the same time. Give each one only its part of the user's question without widening the scope, and set depth instead of writing it into the task: quick (find the main places), medium (the default: answer each question with file:line evidence, then stop), or thorough (only when the user asks for exhaustive coverage). Then tell the user what each one is checking and end your reply instead of calling wait_agent, so the user can keep talking; their reports arrive on their own and wake you to combine them. When a report wakes you while other sub-agents you started are still running, tell the user in a sentence what arrived and end your turn; combine them when the last one arrives. Never say a report has arrived before you have seen it. Don't research those parts yourself meanwhile. When a few reads answer the question, do it yourself. Roles that can edit require explicit sub-agent, delegation, or parallel-work instructions from the user or applicable AGENTS.md, including standing instructions such as delegating tests to a test-writer. Requests to investigate carefully or analyze deeply do not authorize editing roles. Make your own plan first; do work blocking your next step yourself and delegate independent work. Give editing agents explicit file or module ownership and tell them that others share the repository: do not revert others' edits. At most four agents may run at once; children cannot spawn agents. Plan mode and review rounds allow only read-only roles. Roles inherit your model and effort unless configured otherwise. Available roles:\n- explorer (read-only): Read-only code lookup or research.\n- general (can edit): Independent work, including file changes, when explicitly delegated.".into();
    let names = spawn.parameters["properties"]["type"]["enum"]
        .as_array_mut()
        .expect("role enum");
    for role in available {
        names.push(json!(role.summary.name));
        spawn.description.push_str(&format!(
            "\n- {} ({}): {}",
            role.summary.name,
            if role.summary.read_only {
                "read-only"
            } else {
                "can edit"
            },
            role.summary
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    definitions
}

impl crate::Session {
    pub fn set_agent_host(&self, host: Arc<dyn AgentHost>) {
        let _ = self.shared.agent_host.set(host);
    }
}

fn parse(call: &ToolCall) -> Result<AgentCommand, CoreError> {
    let mut args: Value = serde_json::from_str(&call.arguments)
        .map_err(|_| CoreError::new(ErrorCode::InvalidRequest, "Invalid agent tool arguments"))?;
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
    if let AgentCommand::SpawnAgent {
        depth: AgentDepth::Unknown,
        ..
    } = command
    {
        return Err(CoreError::new(
            ErrorCode::InvalidRequest,
            "Invalid agent tool arguments: depth must be quick, medium, or thorough",
        ));
    }
    Ok(command)
}

/// P2-255: the task a read-only child gets: the model's, then a sentence on
/// how deep to look. A child that can edit gets the model's task alone.
pub(crate) fn with_depth(task: &str, depth: AgentDepth) -> String {
    let said = match depth {
        AgentDepth::Quick => "Depth: quick. Find the main places, report them, and stop.",
        AgentDepth::Medium => {
            "Depth: medium. Answer each question with file:line evidence, then stop."
        }
        AgentDepth::Thorough => {
            "Depth: thorough. Cover everything the task asks for before you report."
        }
        AgentDepth::Unknown => return task.to_owned(),
    };
    format!("{task}\n\n{said}")
}

/// The card shown for a call: the tool, the task or message, and the agents
/// it names with what is known of them when it starts.
async fn card(
    host: Option<&Arc<dyn AgentHost>>,
    parent: &str,
    name: &str,
    command: &Result<AgentCommand, CoreError>,
) -> Item {
    let (tool, prompt, ids) = match command {
        Ok(AgentCommand::SpawnAgent { task, .. }) => {
            (CollabAgentTool::SpawnAgent, Some(task.clone()), Vec::new())
        }
        Ok(AgentCommand::WaitAgent { ids, .. }) => (CollabAgentTool::Wait, None, ids.clone()),
        Ok(AgentCommand::SendInput { id, message }) => (
            CollabAgentTool::SendInput,
            Some(message.clone()),
            vec![id.clone()],
        ),
        Ok(AgentCommand::CloseAgent { id }) => {
            (CollabAgentTool::CloseAgent, None, vec![id.clone()])
        }
        Err(_) => (
            match name {
                "spawn_agent" => CollabAgentTool::SpawnAgent,
                "wait_agent" => CollabAgentTool::Wait,
                "send_input" => CollabAgentTool::SendInput,
                _ => CollabAgentTool::CloseAgent,
            },
            None,
            Vec::new(),
        ),
    };
    let agents = match (host, command) {
        // Not started yet: only its type is known.
        (_, Ok(AgentCommand::SpawnAgent { kind, .. })) => vec![CollabAgent {
            id: String::new(),
            name: String::new(),
            agent_type: kind.clone(),
            status: None,
            result: None,
        }],
        (Some(host), _) if !ids.is_empty() => host.describe(parent.into(), ids).await,
        _ => Vec::new(),
    };
    Item::CollabAgentToolCall {
        id: crate::session::id("agent-tool"),
        tool,
        status: ItemStatus::InProgress,
        prompt,
        agents,
        error: None,
        started_at: Some(crate::journal::now()),
        duration_ms: None,
    }
}

const INTERACTIVE_WAIT: &str = "Not waiting: sub-agents are still running and their reports will wake you when they finish; reply to the user now and end this turn.";

pub(crate) async fn execute(
    shared: &Shared,
    turn_id: &str,
    call: &ToolCall,
    cancel: &CancellationToken,
) -> Result<bool, CoreError> {
    if !is_tool(&call.name) {
        return Ok(false);
    }
    let parent = shared.state.lock().await.journal.snapshot.id.clone();
    let mut command = parse(call);
    let host = shared.agent_host.get();
    let mut item = card(host, &parent, &call.name, &command).await;
    let item_id = item.id().unwrap_or_default().to_owned();
    shared
        .state
        .lock()
        .await
        .journal
        .append(vec![EventPayload::ItemStarted {
            turn_id: turn_id.into(),
            item: item.clone(),
        }])
        .await?;
    let finish = |item: &mut Item,
                  done: ItemStatus,
                  found: Option<Vec<CollabAgent>>,
                  why: Option<String>| {
        if let Item::CollabAgentToolCall {
            status,
            agents,
            error,
            started_at,
            duration_ms,
            ..
        } = item
        {
            *status = done;
            if let Some(found) = found {
                *agents = found;
            }
            *error = why;
            *duration_ms = started_at.map(|start| {
                u64::try_from(crate::journal::now().saturating_sub(start)).unwrap_or(0)
            });
        }
    };
    if let Some(reason) = crate::hooks::run(
        shared,
        crate::hooks::Event::PreToolUse,
        crate::hooks::tool_fields(call),
        cancel,
    )
    .await?
    {
        let text = format!("PreToolUse hook blocked: {reason}");
        finish(&mut item, ItemStatus::Failed, None, Some(text.clone()));
        let mut state = shared.state.lock().await;
        state
            .journal
            .append(vec![EventPayload::ItemCompleted {
                turn_id: turn_id.into(),
                item,
            }])
            .await?;
        state
            .tool_result(call, text, false, Default::default(), Some(&item_id))
            .await?;
        return Ok(true);
    }
    if cancel.is_cancelled() {
        finish(&mut item, ItemStatus::Interrupted, None, None);
        shared
            .state
            .lock()
            .await
            .journal
            .append(vec![EventPayload::ItemCompleted {
                turn_id: turn_id.into(),
                item,
            }])
            .await?;
        return Ok(true);
    }
    // User and report-woken turns must leave the user free to talk (P2-270).
    // Exec and disabled auto-wake need the wait; goal continuations would spin.
    let interactive_wait = call.name == "wait_agent" && {
        let state = shared.state.lock().await;
        shared.runtime().options.auto_wake_limit > 0
            && state
                .active
                .as_ref()
                .is_some_and(|active| !active.non_interactive && !active.goal_continuation)
    };
    if let Ok(AgentCommand::WaitAgent { no_wait, .. }) = &mut command {
        *no_wait = interactive_wait;
    }
    let result = async {
        let host = host.ok_or_else(|| {
            CoreError::new(
                ErrorCode::InvalidRequest,
                "Sub-agent tools require a session host",
            )
        })?;
        host.execute(parent, command?, cancel.clone()).await
    }
    .await;
    let success = result.is_ok();
    let (mut text, found, error) = match result {
        Ok(output)
            if interactive_wait
                && output
                    .agents
                    .iter()
                    .any(|agent| agent.status == Some(AgentStatus::Running)) =>
        {
            (
                format!("{}\n\n{INTERACTIVE_WAIT}", output.value),
                Some(output.agents),
                None,
            )
        }
        Ok(output) => (output.value.to_string(), Some(output.agents), None),
        Err(error) => (error.message.clone(), None, Some(error.message)),
    };
    // canter exec does not wake the parent when a child finishes (DESIGN, P2-178).
    if success && call.name == "spawn_agent" && shared.state.lock().await.non_interactive() {
        text.push_str("\n\nThis run is non-interactive: sub-agent reports will not wake you. Call wait_agent before your final reply.");
    }
    let mut fields = crate::hooks::tool_fields(call);
    fields["tool_response"] = json!(text);
    if let Some(reason) =
        crate::hooks::run(shared, crate::hooks::Event::PostToolUse, fields, cancel).await?
    {
        text.push_str(&format!("\n\nPostToolUse hook: {reason}"));
    }
    let status = if success {
        ItemStatus::Completed
    } else if cancel.is_cancelled() {
        ItemStatus::Interrupted
    } else {
        ItemStatus::Failed
    };
    finish(&mut item, status, found, error);
    let mut state = shared.state.lock().await;
    state
        .journal
        .append(vec![EventPayload::ItemCompleted {
            turn_id: turn_id.into(),
            item,
        }])
        .await?;
    state
        .tool_result(call, text, success, Default::default(), Some(&item_id))
        .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use canter_protocol::AgentRole;

    const SPLIT: &str = "When research splits into independent parts that each need many files read, such as comparing how several projects or modules handle something,";
    const SMALL: &str = "When a few reads answer the question, do it yourself.";
    const SCOPE: &str = "Give each one only its part of the user's question without widening the scope, and set depth instead of writing it into the task: quick (find the main places), medium (the default: answer each question with file:line evidence, then stop), or thorough (only when the user asks for exhaustive coverage). Then tell the user what each one is checking and end your reply instead of calling wait_agent, so the user can keep talking; their reports arrive on their own and wake you to combine them. When a report wakes you while other sub-agents you started are still running, tell the user in a sentence what arrived and end your turn; combine them when the last one arrives. Never say a report has arrived before you have seen it. Don't research those parts yourself meanwhile.";

    #[test]
    fn spawn_description_says_when_to_run_read_only_agents_in_parallel() {
        let builtin = definitions()[0].description.clone();
        let reviewer = crate::roles::Role {
            summary: AgentRole {
                name: "reviewer".into(),
                description: "Reviews code.".into(),
                model: None,
                effort: None,
                read_only: true,
                built_in: false,
                error: None,
            },
            tools: None,
            prompt: String::new(),
        };
        let with_roles = definitions_for_roles(&[reviewer])[0].description.clone();
        for text in [builtin, with_roles] {
            assert!(text.contains(SPLIT), "{text}");
            assert!(text.contains(SMALL), "{text}");
            assert!(text.contains(SCOPE), "{text}");
            assert!(!text.contains("say how deep to look"), "{text}");
        }
    }

    fn spawn(arguments: Value) -> Result<AgentCommand, CoreError> {
        parse(&ToolCall {
            id: "call".into(),
            name: "spawn_agent".into(),
            arguments: arguments.to_string(),
        })
    }

    /// P2-255: every spawn names how deep to look, whatever the type.
    #[test]
    fn spawn_agent_requires_one_of_three_depths() {
        let reviewer = crate::roles::Role {
            summary: AgentRole {
                name: "reviewer".into(),
                description: "Reviews code.".into(),
                model: None,
                effort: None,
                read_only: true,
                built_in: false,
                error: None,
            },
            tools: None,
            prompt: String::new(),
        };
        for spawn in [definitions(), definitions_for_roles(&[reviewer])]
            .map(|definitions| definitions[0].parameters.clone())
        {
            assert_eq!(spawn["required"], json!(["task", "type", "depth", "name"]));
            assert_eq!(
                spawn["properties"]["depth"],
                json!({"type":"string","enum":["quick","medium","thorough"]})
            );
        }
        for kind in ["explorer", "general", "reviewer"] {
            let missing = spawn(json!({"task":"Inspect","type":kind,"name":"Inspect"}))
                .expect_err("depth is required");
            assert!(missing.message.contains("depth"), "{}", missing.message);
            for (name, depth) in [
                ("quick", AgentDepth::Quick),
                ("medium", AgentDepth::Medium),
                ("thorough", AgentDepth::Thorough),
            ] {
                let Ok(AgentCommand::SpawnAgent { depth: parsed, .. }) =
                    spawn(json!({"task":"Inspect","type":kind,"depth":name}))
                else {
                    panic!("{kind} {name}");
                };
                assert_eq!(parsed, depth);
            }
            let other = spawn(json!({"task":"Inspect","type":kind,"depth":"deep"}))
                .expect_err("only the three depths");
            assert!(other.message.contains("depth"), "{}", other.message);
        }
    }

    /// The child's task: the model's, then what the depth asks for.
    #[test]
    fn the_depth_is_said_after_the_task() {
        for (depth, said) in [
            (AgentDepth::Quick, "Depth: quick."),
            (AgentDepth::Medium, "Depth: medium."),
            (AgentDepth::Thorough, "Depth: thorough."),
        ] {
            let task = with_depth("Inspect the parser", depth);
            let (before, after) = task.split_once("\n\n").unwrap();
            assert_eq!(before, "Inspect the parser");
            assert!(after.starts_with(said), "{after}");
        }
        assert!(with_depth("Inspect", AgentDepth::Medium).contains("file:line evidence"));
    }

    #[test]
    fn wait_description_explains_interactive_returns_and_blocking_exceptions() {
        let wait = definitions()
            .into_iter()
            .find(|tool| tool.name == "wait_agent")
            .unwrap();
        assert!(
            wait.description.contains("In an interactive session with auto wake enabled, user-started and report-woken turns return at once without waiting. If sub-agents are still running, reply to the user and end your turn; their reports arrive on their own and wake you."),
            "{}",
            wait.description
        );
        assert!(
            wait.description.contains("With auto wake disabled or in an automatic goal continuation, wait until any listed sub-agent completes, fails, closes, or is interrupted, or the timeout expires."),
            "{}",
            wait.description
        );
        assert!(wait.description.contains("In a non-interactive exec turn, use this tool to receive results before ending your turn."));
    }
}
