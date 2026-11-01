//! Read-only deployment status, taken under the runtime state lock.

use crate::Session;
use canter_protocol::*;

impl Session {
    /// Report work without cloning the conversation or loading a saved session.
    pub async fn running_work(&self) -> Option<RunningTurn> {
        let state = self.shared.state.lock().await;
        let snapshot = &state.journal.snapshot;
        let turn = snapshot
            .turns
            .last()
            .filter(|turn| turn.status == TurnStatus::InProgress);
        let activity = if let Some(turn) = turn {
            match snapshot
                .pending_requests
                .iter()
                .find(|request| request.turn_id == turn.id)
            {
                Some(request) => match &request.kind {
                    RequestKind::UserInput(_) => "Waiting for an answer".into(),
                    RequestKind::Approval(approval) | RequestKind::SandboxApproval(approval) => {
                        format!("Waiting for approval: {}", approval.tool)
                    }
                    RequestKind::PlanApproval(_) => "Waiting for plan approval".into(),
                    RequestKind::Unknown => "Waiting for a response".into(),
                },
                None => turn_activity(turn),
            }
        } else if !state.closing
            && !state.background_events.draining
            && !state.automatic_turns_paused
            && (state.goal_running || state.goal_deadline.is_some())
            && snapshot
                .goal
                .as_ref()
                .is_some_and(|goal| goal.status == GoalStatus::Active)
        {
            "Continuing a goal".into()
        } else if let Some(command) = snapshot
            .background_commands
            .iter()
            .find(|command| command.running)
        {
            let preview = command
                .command
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(60)
                .collect::<String>();
            format!("Running background command #{}: {preview}", command.id)
        } else if !state.automatic_turns_paused
            && state
                .background_events
                .has_pending_wake(self.shared.next_options().auto_wake_limit)
        {
            "Waiting for a background command to wake the model".into()
        } else {
            return None;
        };
        Some(RunningTurn {
            id: snapshot.id.clone(),
            name: snapshot.settings.name.clone(),
            turn_id: turn.map(|turn| turn.id.clone()),
            activity,
        })
    }
}

fn turn_activity(turn: &Turn) -> String {
    let work = turn
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            Item::CommandExecution {
                status: ItemStatus::InProgress,
                ..
            } => Some("Running a shell command"),
            Item::FileChange {
                status: ItemStatus::InProgress,
                ..
            } => Some("Changing files"),
            Item::FileRead {
                status: ItemStatus::InProgress,
                ..
            } => Some("Reading a file"),
            Item::WebSearch {
                status: ItemStatus::InProgress,
                ..
            } => Some("Searching the web"),
            Item::WebFetch {
                status: ItemStatus::InProgress,
                ..
            } => Some("Fetching a web page"),
            Item::McpToolCall {
                status: ItemStatus::InProgress,
                ..
            } => Some("Running an MCP tool"),
            _ => None,
        })
        .unwrap_or("Running a turn");
    let prompt = turn.input.iter().find_map(|input| match input {
        UserInput::Text { text } => Some(text),
        _ => None,
    });
    match prompt {
        Some(text) => format!(
            "{work}: {}",
            text.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(120)
                .collect::<String>()
        ),
        None => work.into(),
    }
}
