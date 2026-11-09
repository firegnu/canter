//! The session facts a task's checks read: every turn of the main session and
//! who opened it, the tools each turn called, all replies, what each
//! sub-agent did, and the chat lines `steer` sent. `grade` writes them to
//! `session-data.json` in the run directory for tasks that use `[[chat]]`.

use std::{collections::BTreeMap, fs, path::Path};

use canter_protocol::{
    AgentStatus, CollabAgentTool, Event, EventPayload, ExecCommandSource, Item, ItemStatus,
    StatsResponse, StatsSubAgent, Turn,
};
use serde::{Deserialize, Serialize};

use crate::Result;

/// What `steer` sent, skipped or timed out on for one run; `steers.json`.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    /// `after_commits` steers already sent, by zero-based index.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sent: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chats: Vec<ChatRecord>,
    #[serde(default)]
    pub timed_out: bool,
}

/// One `[[chat]]` line: sent at a time, or skipped with the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRecord {
    /// One-based position in the task's `[[chat]]` list.
    pub index: usize,
    /// When the input request was accepted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<i64>,
    /// The turn that input started, once the session's events name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Why the line was not sent; never set together with `sentAt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

impl ChatRecord {
    pub fn is_resolved(&self) -> bool {
        self.sent_at.is_some() || self.skipped.is_some()
    }
}

pub fn progress(home: &crate::paths::Home, round: &str, run: &str) -> Result<Progress> {
    let path = home.steer_progress_path(round, run);
    match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Progress::default()),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionData {
    pub session_id: String,
    pub cwd: String,
    pub turns: Vec<TurnRow>,
    /// Completed agent messages from the main session, in event order.
    #[serde(default)]
    pub replies: Vec<String>,
    /// The text of the last completed agent message, if there was one.
    pub final_reply: Option<String>,
    pub sub_agents: Vec<SubAgentRow>,
    pub chats: Vec<ChatRecord>,
    pub timed_out: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRow {
    /// One-based, in the order the turns started.
    pub index: usize,
    pub id: String,
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    /// `endedAt - startedAt`; absent until the turn completes.
    pub duration_ms: Option<u64>,
    pub opened_by: Opener,
    /// How many times each tool ran in this turn, by tool name.
    pub tools: BTreeMap<String, usize>,
    /// How many spawn calls returned an agent with an id: a failed spawn is a
    /// tool call but did not start a sub-agent.
    pub spawned_agents: usize,
}

/// What started a turn of the main session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Opener {
    /// The run's opening prompt.
    Prompt,
    /// The nth `[[chat]]` line.
    Steer { index: usize },
    /// A sub-agent report woke the session.
    Report,
    /// Anything else, such as a compaction turn; `clientName` says what.
    Other { client_name: String },
}

/// One sub-agent session the main session started.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubAgentRow {
    pub id: String,
    pub name: Option<String>,
    pub agent_type: String,
    pub status: String,
    pub started_at: Option<i64>,
    /// The last `turn.completed`, excluding events after the report.
    pub ended_at: Option<i64>,
    /// From the first event to the last completed turn; absent without one.
    pub duration_ms: Option<u64>,
    pub request_count: usize,
    pub blocked_commands: usize,
    /// Blocked commands that pass a short flag containing `r` to `rg`; `-r`
    /// and combinations such as `-rn` count.
    pub blocked_rg_replace: usize,
    pub cached_input_tokens: Option<u64>,
    pub uncached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub cost_unknown: bool,
}

/// Read the session's events and the run's chat progress into one document.
pub fn build(
    home: &crate::paths::Home,
    state: &crate::paths::State,
    stats: &StatsResponse,
) -> Result<SessionData> {
    let events = read_events(&home.session_events(&stats.session_id))?;
    let progress = progress(home, &state.round, &state.run)?;
    let task = state.task()?;
    // The lines as the session saw them, for matching a turn to its line when
    // `steer` had no turn id to record.
    let texts = task
        .chats
        .iter()
        .map(|chat| crate::task::render(&chat.text, state))
        .collect::<Result<Vec<_>>>()?;
    let turns = turns(&events, &progress.chats, &texts);
    let replies = replies(&events);
    Ok(SessionData {
        session_id: stats.session_id.clone(),
        cwd: stats.overview.cwd.clone(),
        turns,
        final_reply: replies.last().cloned(),
        replies,
        sub_agents: sub_agents(home, &stats.sub_agents)?,
        chats: progress.chats,
        timed_out: progress.timed_out,
    })
}

pub fn read_events(path: &Path) -> Result<Vec<Event>> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(parse_events(&text))
}

/// The events in a session's event log; a line that is not an event (a partial
/// write at the end, say) is skipped.
pub fn parse_events(text: &str) -> Vec<Event> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Event>(line).ok())
        .collect()
}

fn turns(events: &[Event], chats: &[ChatRecord], texts: &[String]) -> Vec<TurnRow> {
    let mut turns: Vec<TurnRow> = Vec::new();
    for event in events {
        match &event.payload {
            EventPayload::TurnStarted { turn, .. } => {
                let index = turns.len() + 1;
                turns.push(TurnRow {
                    index,
                    id: turn.id.clone(),
                    started_at: Some(event.timestamp),
                    ended_at: None,
                    duration_ms: None,
                    opened_by: opener(index, turn, chats, texts),
                    tools: BTreeMap::new(),
                    spawned_agents: 0,
                });
            }
            EventPayload::TurnCompleted { turn } => {
                if let Some(row) = turns.iter_mut().find(|row| row.id == turn.id) {
                    row.ended_at = Some(event.timestamp);
                    row.duration_ms = row
                        .started_at
                        .map(|start| u64::try_from(event.timestamp - start).unwrap_or(0));
                }
            }
            EventPayload::ItemCompleted { turn_id, item } => {
                if let Some(name) = tool_name(item)
                    && let Some(row) = turns.iter_mut().find(|row| row.id == *turn_id)
                {
                    *row.tools.entry(name).or_insert(0) += 1;
                    if let Item::CollabAgentToolCall {
                        tool: CollabAgentTool::SpawnAgent,
                        agents,
                        ..
                    } = item
                    {
                        row.spawned_agents +=
                            agents.iter().filter(|agent| !agent.id.is_empty()).count();
                    }
                }
            }
            _ => {}
        }
    }
    turns
}

fn opener(index: usize, turn: &Turn, chats: &[ChatRecord], texts: &[String]) -> Opener {
    if index == 1 {
        return Opener::Prompt;
    }
    if let Some(chat) = chats
        .iter()
        .find(|chat| chat.turn_id.as_deref() == Some(turn.id.as_str()))
        .or_else(|| {
            let text = input_text(turn);
            chats.iter().find(|chat| {
                chat.sent_at.is_some()
                    && texts.get(chat.index.saturating_sub(1)) == Some(&text)
                    && !text.is_empty()
            })
        })
    {
        return Opener::Steer { index: chat.index };
    }
    if turn.client_name == "background" && turn.input.is_empty() {
        return Opener::Report;
    }
    Opener::Other {
        client_name: turn.client_name.clone(),
    }
}

fn input_text(turn: &Turn) -> String {
    turn.input
        .iter()
        .filter_map(|part| match part {
            canter_protocol::UserInput::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn replies(events: &[Event]) -> Vec<String> {
    let mut replies = Vec::new();
    for event in events {
        if let EventPayload::ItemCompleted { item, .. } = &event.payload
            && let Item::AgentMessage { text, status, .. } = item
            && status.is_none_or(|status| status == ItemStatus::Completed)
        {
            replies.push(text.clone());
        }
    }
    replies
}

/// The tool a completed item stands for, by the names the tools report.
fn tool_name(item: &Item) -> Option<String> {
    Some(match item {
        Item::FileRead { .. } => "read".into(),
        Item::AstGrep { .. } => "ast_grep".into(),
        Item::FileChange { .. } => "write/edit".into(),
        Item::CommandExecution { source, .. } => match source {
            ExecCommandSource::Agent => "shell".into(),
            ExecCommandSource::UserShell => "user_shell".into(),
            ExecCommandSource::Unknown => "shell_unknown".into(),
        },
        Item::WebFetch { .. } => "web_fetch".into(),
        Item::WebSearch { .. } => "web_search".into(),
        Item::McpToolCall { server, tool, .. } => format!("mcp__{server}__{tool}"),
        Item::McpToolSearch { .. } => "mcp_search".into(),
        Item::Plan { .. } => "update_plan".into(),
        Item::PlanProposal { .. } => "submit_plan".into(),
        Item::UserInput { .. } => "ask_user".into(),
        Item::BackgroundCommandAction { action, .. } => match action.as_str() {
            "monitor" => "monitor".into(),
            other => format!("shell_{other}"),
        },
        Item::CollabAgentToolCall { tool, .. } => match tool {
            canter_protocol::CollabAgentTool::SpawnAgent => "spawn_agent".into(),
            canter_protocol::CollabAgentTool::Wait => "wait_agent".into(),
            canter_protocol::CollabAgentTool::SendInput => "send_input".into(),
            canter_protocol::CollabAgentTool::CloseAgent => "close_agent".into(),
            canter_protocol::CollabAgentTool::Unknown => "unknown_agent_tool".into(),
        },
        _ => return None,
    })
}

/// One row per sub-agent from the main session's stats; each child's events
/// give its start, last completed turn and blocked commands.
pub fn sub_agents(home: &crate::paths::Home, list: &[StatsSubAgent]) -> Result<Vec<SubAgentRow>> {
    let mut rows = Vec::with_capacity(list.len());
    for child in list {
        let events = read_events(&home.session_events(&child.id))
            .map_err(|error| format!("the sub-agent {} has no events: {error}", child.id))?;
        let started_at = events.first().map(|event| event.timestamp);
        let ended_at = events
            .iter()
            .rev()
            .find(|event| matches!(event.payload, EventPayload::TurnCompleted { .. }))
            .map(|event| event.timestamp);
        let mut blocked_commands = 0;
        let mut blocked_rg_replace = 0;
        for event in &events {
            if let EventPayload::ItemCompleted {
                item:
                    Item::CommandExecution {
                        command,
                        status: ItemStatus::Declined | ItemStatus::Blocked,
                        ..
                    },
                ..
            } = &event.payload
            {
                blocked_commands += 1;
                if rg_with_replace(command) {
                    blocked_rg_replace += 1;
                }
            }
        }
        let usage = child.usage.as_ref();
        rows.push(SubAgentRow {
            id: child.id.clone(),
            name: child.name.clone(),
            agent_type: child.agent_type.as_str().to_string(),
            status: status_str(child.status).to_string(),
            started_at,
            ended_at,
            duration_ms: started_at
                .zip(ended_at)
                .map(|(start, end)| u64::try_from(end - start).unwrap_or(0)),
            request_count: child.request_count,
            blocked_commands,
            blocked_rg_replace,
            cached_input_tokens: usage.map(|usage| usage.cached_input_tokens),
            uncached_input_tokens: usage.map(|usage| usage.uncached_input_tokens),
            output_tokens: usage.map(|usage| usage.output_tokens),
            cost_usd: usage.map(|usage| usage.cost.usd),
            cost_unknown: usage.is_none_or(|usage| usage.cost.unknown),
        });
    }
    Ok(rows)
}

fn status_str(status: AgentStatus) -> &'static str {
    match status {
        AgentStatus::Running => "running",
        AgentStatus::Completed => "completed",
        AgentStatus::Failed => "failed",
        AgentStatus::Closed => "closed",
        AgentStatus::Interrupted => "interrupted",
        AgentStatus::Unknown => "unknown",
    }
}

/// Whether a shell command calls `rg` with a short flag that contains `r`
/// (the replace flag), including combinations such as `-rn`.
pub fn rg_with_replace(command: &str) -> bool {
    for segment in command.split(['&', '|', ';', '\n', '(', ')', '`']) {
        let words: Vec<&str> = segment.split_whitespace().collect();
        for (index, word) in words.iter().enumerate() {
            if *word != "rg" && !word.ends_with("/rg") {
                continue;
            }
            for flag in &words[index + 1..] {
                let Some(flags) = flag.strip_prefix('-') else {
                    continue;
                };
                if flags.is_empty() || flags.starts_with('-') {
                    continue;
                }
                if flags.contains('r') {
                    return true;
                }
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_turns_their_openers_and_tools() {
        let events = [
            r#"{"seq":1,"sessionId":"s","timestamp":1000,"type":"turn.started","turn":{"id":"t1","items":[],"status":"inProgress","error":null,"input":[{"type":"text","text":"调研"}],"clientName":""},"inputId":null}"#,
            r#"{"seq":2,"sessionId":"s","timestamp":2000,"type":"item.completed","turnId":"t1","item":{"type":"collabAgentToolCall","id":"sp","tool":"spawnAgent","status":"completed","agents":[{"id":"c1","name":"probe","status":"running"}]}}"#,
            r#"{"seq":3,"sessionId":"s","timestamp":3000,"type":"item.completed","turnId":"t1","item":{"type":"commandExecution","id":"c","command":"ls","cwd":"/w","status":"completed","aggregatedOutput":"","exitCode":0,"durationMs":1}}"#,
            r#"{"seq":4,"sessionId":"s","timestamp":15000,"type":"turn.completed","turn":{"id":"t1","items":[],"status":"completed","error":null,"input":[],"clientName":""}}"#,
            r#"{"seq":5,"sessionId":"s","timestamp":20000,"type":"turn.started","turn":{"id":"t2","items":[],"status":"inProgress","error":null,"input":[{"type":"text","text":"好的，谢谢"}],"clientName":"canter-eval"},"inputId":null}"#,
            r#"{"seq":6,"sessionId":"s","timestamp":21000,"type":"turn.started","turn":{"id":"t3","items":[],"status":"inProgress","error":null,"input":[],"clientName":"background"},"inputId":null}"#,
            r#"{"seq":7,"sessionId":"s","timestamp":22000,"type":"turn.completed","turn":{"id":"t2","items":[],"status":"completed","error":null,"input":[],"clientName":"canter-eval"}}"#,
        ]
        .join("\n");
        let chats = vec![ChatRecord {
            index: 1,
            sent_at: Some(19_000),
            turn_id: Some("t2".into()),
            skipped: None,
        }];
        let turns = turns(&parse_events(&events), &chats, &["好的，谢谢".to_string()]);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[0].opened_by, Opener::Prompt);
        assert_eq!(turns[0].duration_ms, Some(14_000));
        assert_eq!(turns[0].tools["spawn_agent"], 1);
        assert_eq!(turns[0].tools["shell"], 1);
        assert_eq!(turns[1].opened_by, Opener::Steer { index: 1 });
        assert_eq!(turns[1].duration_ms, Some(2_000));
        assert_eq!(turns[2].opened_by, Opener::Report);
        assert_eq!(turns[2].ended_at, None);
        assert_eq!(turns[2].duration_ms, None);
    }

    #[test]
    fn matches_a_chat_turn_by_its_text_when_no_turn_id_was_recorded() {
        let events = [
            r#"{"seq":1,"sessionId":"s","timestamp":1000,"type":"turn.started","turn":{"id":"t1","items":[],"status":"inProgress","error":null,"input":[{"type":"text","text":"调研"}],"clientName":""},"inputId":null}"#,
            r#"{"seq":2,"sessionId":"s","timestamp":5000,"type":"turn.started","turn":{"id":"t2","items":[],"status":"inProgress","error":null,"input":[{"type":"text","text":"第二句"}],"clientName":"canter-eval"},"inputId":null}"#,
        ]
        .join("\n");
        let chats = vec![
            ChatRecord {
                index: 1,
                sent_at: Some(4_000),
                turn_id: None,
                skipped: None,
            },
            ChatRecord {
                index: 2,
                sent_at: Some(4_500),
                turn_id: None,
                skipped: None,
            },
        ];
        let turns = turns(
            &parse_events(&events),
            &chats,
            &["第一句".to_string(), "第二句".to_string()],
        );
        assert_eq!(turns[1].opened_by, Opener::Steer { index: 2 });
    }

    #[test]
    fn finds_completed_replies_in_order() {
        let events = [
            r#"{"seq":1,"sessionId":"s","timestamp":1,"type":"item.completed","turnId":"t","item":{"type":"agentMessage","id":"a1","text":"first","status":"completed"}}"#,
            r#"{"seq":2,"sessionId":"s","timestamp":2,"type":"item.completed","turnId":"t","item":{"type":"agentMessage","id":"a2","text":"second","status":"completed"}}"#,
            r#"{"seq":3,"sessionId":"s","timestamp":3,"type":"item.started","turnId":"t","item":{"type":"agentMessage","id":"a3","text":"later","status":"inProgress"}}"#,
        ]
        .join("\n");
        assert_eq!(replies(&parse_events(&events)), vec!["first", "second"]);
    }

    #[test]
    fn counts_only_spawns_that_returned_an_agent() {
        let events = [
            r#"{"seq":1,"sessionId":"s","timestamp":1000,"type":"turn.started","turn":{"id":"t1","items":[],"status":"inProgress","error":null,"input":[{"type":"text","text":"调研"}],"clientName":""},"inputId":null}"#,
            r#"{"seq":2,"sessionId":"s","timestamp":2000,"type":"item.completed","turnId":"t1","item":{"type":"collabAgentToolCall","id":"sp1","tool":"spawnAgent","status":"completed","error":"bad role","agents":[{"id":"","name":"","status":null}]}}"#,
            r#"{"seq":3,"sessionId":"s","timestamp":3000,"type":"item.completed","turnId":"t1","item":{"type":"collabAgentToolCall","id":"sp2","tool":"spawnAgent","status":"completed","agents":[{"id":"c1","name":"probe","status":"running"}]}}"#,
            r#"{"seq":4,"sessionId":"s","timestamp":4000,"type":"turn.completed","turn":{"id":"t1","items":[],"status":"completed","error":null,"input":[],"clientName":""}}"#,
        ]
        .join("\n");
        let turns = turns(&parse_events(&events), &[], &[]);
        assert_eq!(turns[0].tools["spawn_agent"], 2);
        assert_eq!(turns[0].spawned_agents, 1);
    }

    #[test]
    fn recognises_rg_replace_flags() {
        for command in [
            "rg -r foo src",
            "rg -rn 'foo' src",
            "rg -nr foo",
            "timeout 5 rg -n -r foo .",
            "cd /tmp && /opt/homebrew/bin/rg -r x y",
        ] {
            assert!(rg_with_replace(command), "{command}");
        }
        for command in [
            "rg -n foo src",
            "rg --replace foo src",
            "grep -rn foo src",
            "rg -l foo",
        ] {
            assert!(!rg_with_replace(command), "{command}");
        }
    }
}
