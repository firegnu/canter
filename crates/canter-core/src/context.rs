use crate::{
    CoreError,
    prompt::{self, Segment, SegmentKind},
};
use canter_model::{AssistantContent, Message, ToolDefinition};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::PathBuf};

const MAX_INSTRUCTION_BYTES: u64 = 64 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum HistoryEntry {
    TurnBoundary {
        turn_id: String,
    },
    FileCheckpoint {
        checkpoint: crate::rewind::FileChange,
    },
    FileAborted {
        id: String,
    },
    Rewind {
        rewind: Box<crate::rewind::Rewind>,
    },
    Retract {
        turn_id: String,
        event_seq: u64,
    },
    Context {
        messages: Vec<Message>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        segments: Option<Vec<Segment>>,
    },
    Message {
        message: Message,
        #[serde(default)]
        user_input: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skill_input: Option<crate::skill_invocation::Invocation>,
    },
    /// Durable attachments waiting for every tool result in the batch.
    ToolImages {
        content: Vec<canter_model::UserContent>,
    },
    /// A hidden mode notice is queued until the current tool batch is complete.
    PlanModeNotice {
        message: Message,
    },
    SkillNotice {
        message: Message,
    },
    GoalNotice {
        message: Message,
    },
    DirectoriesNotice {
        message: Message,
    },
    BackgroundNotice {
        message: Message,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        event_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        command_id: Option<u64>,
        #[serde(default)]
        visible: bool,
    },
    DiagnosticsNotice {
        message: Message,
    },
    CompactionRequest {
        messages: Vec<Message>,
    },
    Compaction {
        summary: String,
        messages: Vec<Message>,
        #[serde(default)]
        user_messages: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        user_inputs: Vec<Message>,
    },
    #[serde(other, skip_serializing)]
    Unknown,
}

pub(crate) fn known(message: Message) -> Option<Message> {
    match message {
        Message::Unknown => None,
        Message::Assistant { mut content } => {
            content.retain(|part| !matches!(part, AssistantContent::Unknown));
            if content.is_empty() {
                None
            } else {
                Some(Message::Assistant { content })
            }
        }
        message => Some(message),
    }
}

pub(crate) struct Restored {
    pub diagnostic_notices: std::collections::BTreeSet<usize>,
    pub invoked_skills: Vec<crate::skill_invocation::Invocation>,
    pub background_cards: BTreeMap<String, canter_protocol::Item>,
    pub prefix: Vec<Message>,
    pub segments: Option<Vec<Segment>>,
    pub history: Vec<Message>,
    pub users: Vec<Message>,
    pub summary_index: Option<usize>,
    pub plan_mode_notices: Vec<Message>,
    pub tool_images: Vec<canter_model::UserContent>,
}

pub(crate) fn restore(
    entries: Vec<HistoryEntry>,
    rewind: &mut crate::rewind::Checkpoints,
) -> Restored {
    let mut prefix = Vec::new();
    let mut invoked_skills = Vec::new();
    let mut segments = None;
    let mut history = Vec::new();
    let mut users = Vec::new();
    let mut summary_index = None;
    let mut plan_mode_notices = Vec::new();
    let mut tool_images = Vec::new();
    let mut background_cards = BTreeMap::new();
    let mut diagnostic_notices = std::collections::BTreeSet::new();
    for entry in entries {
        match entry {
            HistoryEntry::DiagnosticsNotice { message } => {
                diagnostic_notices.insert(history.len());
                history.push(message);
            }
            HistoryEntry::ToolImages { content } => tool_images.extend(content),
            HistoryEntry::PlanModeNotice { message }
            | HistoryEntry::SkillNotice { message }
            | HistoryEntry::GoalNotice { message }
            | HistoryEntry::DirectoriesNotice { message } => plan_mode_notices.push(message),
            HistoryEntry::BackgroundNotice {
                message,
                event_id,
                command_id,
                visible,
            } => {
                if visible && let (Some(id), Message::User { text }) = (event_id, &message) {
                    background_cards.insert(
                        id.clone(),
                        canter_protocol::Item::BackgroundEvent {
                            id,
                            command_id,
                            text: crate::background::notice_text(text).into(),
                        },
                    );
                }
                plan_mode_notices.push(message);
            }
            HistoryEntry::TurnBoundary { turn_id } => {
                rewind.boundary(turn_id, history.len(), users.len(), invoked_skills.len())
            }
            HistoryEntry::Rewind { rewind: entry } => {
                if let Some((messages, user_count, skills)) = rewind.apply(&entry) {
                    tool_images.clear();
                    history.truncate(messages);
                    diagnostic_notices.retain(|index| *index < messages);
                    users.truncate(user_count);
                    invoked_skills.truncate(skills);
                }
            }
            HistoryEntry::Retract { turn_id, .. } => {
                if let Some((messages, user_count, skills)) = rewind.retract(&turn_id) {
                    tool_images.clear();
                    history.truncate(messages);
                    diagnostic_notices.retain(|index| *index < messages);
                    users.truncate(user_count);
                    invoked_skills.truncate(skills);
                }
            }
            HistoryEntry::FileCheckpoint { checkpoint } => rewind.record_file(checkpoint),
            HistoryEntry::FileAborted { id } => rewind.abort(&id),
            HistoryEntry::Context {
                messages,
                segments: saved,
            } => {
                prefix = messages;
                segments = saved;
            }
            HistoryEntry::Message {
                message,
                user_input,
                skill_input,
            } => {
                if user_input && matches!(message, Message::User { .. } | Message::UserParts { .. })
                {
                    users.push(
                        skill_input
                            .as_ref()
                            .map_or_else(|| message.clone(), |input| input.user_message.clone()),
                    );
                }
                invoked_skills.extend(skill_input);
                history.extend(known(message));
            }
            HistoryEntry::Compaction {
                messages,
                user_messages,
                user_inputs,
                ..
            } => {
                rewind.clear_boundaries();
                tool_images.clear();
                history = messages.into_iter().filter_map(known).collect();
                diagnostic_notices.clear();
                summary_index = history.len().checked_sub(1);
                users = if user_inputs.is_empty() {
                    user_messages
                        .into_iter()
                        .map(|text| Message::User { text })
                        .collect()
                } else {
                    user_inputs.into_iter().filter_map(known).collect()
                };
            }
            HistoryEntry::CompactionRequest { .. } => {}
            HistoryEntry::Unknown => {}
        }
        flush_tool_images(&mut history, &mut users, &mut tool_images);
        flush_plan_mode_notices(&mut history, &mut plan_mode_notices);
    }
    Restored {
        diagnostic_notices,
        invoked_skills,
        background_cards,
        prefix,
        segments,
        history,
        users,
        summary_index,
        plan_mode_notices,
        tool_images,
    }
}

pub(crate) fn flush_tool_images(
    history: &mut Vec<Message>,
    retained: &mut Vec<Message>,
    pending: &mut Vec<canter_model::UserContent>,
) {
    if pending.is_empty() {
        return;
    }
    // Use the same batch boundary as other deferred notices, but put attachments first.
    let mut messages = vec![Message::ToolImages {
        content: pending.clone(),
    }];
    flush_plan_mode_notices(history, &mut messages);
    if messages.is_empty() {
        retained.push(Message::ToolImages {
            content: std::mem::take(pending),
        });
    }
}

/// Never separate an assistant's tool calls from their results. The notice record
/// itself is durable; replay applies this same rule without rewriting old entries.
pub(crate) fn flush_plan_mode_notices(history: &mut Vec<Message>, notices: &mut Vec<Message>) {
    if notices.is_empty() {
        return;
    }
    if let Some(index) = history.iter().rposition(|message| matches!(message, Message::Assistant { .. }))
        && let Message::Assistant { content } = &history[index]
        && content.iter().any(|part| match part {
            AssistantContent::ToolCall(call) => !history[index + 1..].iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, .. } if tool_call_id == &call.id)),
            _ => false,
        })
    {
        return;
    }
    history.append(notices);
}

pub(crate) fn estimate(text: &str) -> u64 {
    (text.len() as u64).div_ceil(3)
}

pub(crate) fn request_tokens(request: &canter_model::Request) -> u64 {
    // Byte heuristic including role/JSON/tool-definition overhead.
    // Provider-reported input usage, when available, is also used by the loop.
    request.messages.iter().map(message_tokens).sum::<u64>()
        + estimate(&serde_json::to_string(&request.tools).expect("tool definitions"))
}

pub(crate) fn message_tokens(message: &Message) -> u64 {
    if let Message::UserParts { content } | Message::ToolImages { content } = message {
        return 8 + content
            .iter()
            .map(|part| match part {
                canter_model::UserContent::Image { .. } => 1024,
                canter_model::UserContent::Text { .. } => {
                    estimate(&serde_json::to_string(part).expect("text part"))
                }
            })
            .sum::<u64>();
    }
    estimate(&serde_json::to_string(message).expect("known message")) + 8
}

/// The global AGENTS.md lives in canter's own directory: `CANTER_HOME`, or else
/// `~/.canter`, from the session environment; only an absolute path.
pub(crate) fn canter_home(environment: &BTreeMap<String, String>) -> Option<PathBuf> {
    let dir = match environment.get("CANTER_HOME") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(environment.get("HOME")?).join(".canter"),
    };
    dir.is_absolute().then_some(dir)
}

pub(crate) async fn prefix(
    cwd: String,
    home: Option<PathBuf>,
    canter_home: Option<PathBuf>,
    tools: Vec<ToolDefinition>,
) -> Result<(Vec<Segment>, Vec<String>, Vec<canter_protocol::Skill>), CoreError> {
    tokio::task::spawn_blocking(move || {
        let cwd = PathBuf::from(cwd);
        let home = home.map(|home| cwd.join(home));
        let git_root = cwd.ancestors().find(|path| path.join(".git").exists());
        let root = git_root.unwrap_or(&cwd);
        let mut paths: Vec<_> = cwd
            .ancestors()
            .take_while(|path| path.starts_with(root))
            .collect();
        paths.reverse();
        let opened = chrono::Local::now().fixed_offset();
        let mut segments = vec![Segment {
            kind: SegmentKind::Core,
            path: None,
            text: prompt::core(&tools),
        }];
        let mut warnings = Vec::new();
        let global = canter_home.map(|dir| ("Global", dir.join("AGENTS.md")));
        let project = paths
            .into_iter()
            .map(|dir| ("Project", dir.join("AGENTS.md")));
        for (scope, path) in global.into_iter().chain(project) {
            let instructions = (|| -> std::io::Result<String> {
                let mut bytes = Vec::new();
                std::fs::File::open(&path)?
                    .take(MAX_INSTRUCTION_BYTES + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > MAX_INSTRUCTION_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "exceeds the 64 KiB file limit",
                    ));
                }
                String::from_utf8(bytes).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "not valid UTF-8")
                })
            })();
            match instructions {
                Ok(instructions) if !instructions.trim().is_empty() => {
                    segments.push(Segment {
                        kind: if scope == "Global" {
                            SegmentKind::GlobalInstructions
                        } else {
                            SegmentKind::ProjectInstructions
                        },
                        path: Some(path.to_string_lossy().into_owned()),
                        text: instructions,
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => warnings.push(format!(
                    "Skipped {} instructions {}: {error}",
                    scope.to_ascii_lowercase(),
                    path.display()
                )),
            }
        }
        let skills = crate::skills::discover(home, root, &mut warnings);
        let mut skill_text = String::new();
        crate::skills::append_prompt(&mut skill_text, &skills);
        segments.push(Segment {
            kind: SegmentKind::Skills,
            path: None,
            text: skill_text,
        });
        segments.push(Segment {
            kind: SegmentKind::Environment,
            path: None,
            text: prompt::environment(
                &cwd,
                git_root,
                std::env::consts::OS,
                std::env::consts::ARCH,
                opened,
            ),
        });
        (
            prompt::ordered(&segments).cloned().collect(),
            warnings,
            skills,
        )
    })
    .await
    .map_err(|_| CoreError::internal())
}

pub(crate) fn request(
    state: &crate::session::State,
    max_output_tokens: u32,
) -> canter_model::Request {
    use canter_model::{Request, RequestOptions};
    let effort = match state.journal.snapshot.settings.reasoning_effort {
        canter_protocol::ReasoningEffort::Unknown => canter_protocol::ReasoningEffort::High,
        effort => effort,
    };
    Request {
        model: state.journal.snapshot.settings.model.clone(),
        messages: state.prefix.iter().chain(&state.history).cloned().collect(),
        tools: state.tool_definitions.clone(),
        options: RequestOptions {
            max_retries: None,
            thinking: Some(effort != canter_protocol::ReasoningEffort::None),
            reasoning_effort: (effort != canter_protocol::ReasoningEffort::None)
                .then(|| effort.as_str().to_owned()),
            max_output_tokens: Some(max_output_tokens),
        },
    }
}

impl crate::Session {
    pub async fn context(&self) -> canter_protocol::ContextResponse {
        use canter_model::UserContent;
        use canter_protocol::{ContextMessages, ContextResponse};
        let state = self.shared.state.lock().await;
        let request = request(&state, self.shared.options.max_output_tokens);
        let mut messages = ContextMessages::default();
        let mut compaction_tokens = None;
        for (index, message) in state.history.iter().enumerate() {
            let tokens = message_tokens(message);
            if state.summary_index == Some(index) {
                compaction_tokens = Some(tokens);
                continue;
            }
            messages.count += 1;
            messages.tokens += tokens;
            match message {
                Message::User { .. } => messages.user += tokens,
                Message::UserParts { content } | Message::ToolImages { content } => {
                    let images = content
                        .iter()
                        .filter(|part| matches!(part, UserContent::Image { .. }))
                        .count() as u64
                        * 1024;
                    messages.images += images;
                    if matches!(message, Message::ToolImages { .. }) {
                        messages.tool_results += tokens - images;
                    } else {
                        messages.user += tokens - images;
                    }
                }
                Message::ToolResult { .. } => messages.tool_results += tokens,
                _ => messages.assistant += tokens,
            }
        }
        let estimated_tokens = request_tokens(&request);
        ContextResponse {
            prefix: state.prefix_sections.clone(),
            skill_count: state
                .skills
                .iter()
                .filter(|skill| skill.model_invocation)
                .count(),
            tool_count: request.tools.len(),
            tool_tokens: estimate(
                &serde_json::to_string(&request.tools).expect("tool definitions"),
            ),
            compaction_tokens,
            messages,
            estimated_tokens,
            free_tokens: self
                .shared
                .options
                .context_window
                .saturating_sub(estimated_tokens),
            model_context_window: self.shared.options.context_window,
            context_tokens: state
                .journal
                .snapshot
                .token_usage
                .as_ref()
                .filter(|usage| usage.model_context_window.is_some())
                .map(|usage| usage.context_tokens),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canter_model::{FinishReason, Model, ModelEvent, ModelStream, Request};
    use canter_protocol::{
        InputMode, PermissionMode, ReasoningEffort, SessionInputParams, SessionSettings, UserInput,
    };
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    struct Reply;
    impl Model for Reply {
        fn stream(&self, _: Request, _: CancellationToken) -> ModelStream<'_> {
            Box::pin(futures_util::stream::iter([
                Ok(ModelEvent::TextDelta {
                    item_id: "text".into(),
                    delta: "saved summary".into(),
                }),
                Ok(ModelEvent::Finished(FinishReason::Stop)),
            ]))
        }
    }

    #[tokio::test]
    async fn report_uses_existing_estimators_and_preserves_the_request_after_compaction() {
        let temp = tempfile::tempdir().unwrap();
        let image = temp.path().join("image.png");
        std::fs::write(&image, b"\x89PNG\r\n\x1a\n").unwrap();
        let (session, _) = crate::Session::create(
            Arc::new(canter_store::Store::new(temp.path().join("state")).unwrap()),
            "context".into(),
            SessionSettings {
                additional_directories: Vec::new(),
                plan_mode: false,
                pre_plan_approval_policy: None,
                plan_enabled: None,
                cwd: temp.path().to_string_lossy().into_owned(),
                name: None,
                model: "deepseek-flash".into(),
                reasoning_effort: ReasoningEffort::High,
                approval_policy: PermissionMode::AllowAll,
            },
            crate::RuntimeContext {
                environment: [("HOME".into(), temp.path().to_string_lossy().into_owned())].into(),
                ..Default::default()
            },
            Arc::new(Reply),
            crate::CoreOptions::default(),
        )
        .await
        .unwrap();
        let mut events = session.subscribe(0).await.unwrap();
        session
            .input(SessionInputParams {
                review: None,
                input: vec![
                    UserInput::Text {
                        text: "inspect image".into(),
                    },
                    UserInput::LocalImage {
                        path: image.to_string_lossy().into_owned(),
                        name: None,
                    },
                ],
                mode: InputMode::Queue,
                client_name: "test".into(),
            })
            .await
            .unwrap();
        for compact in [false, true] {
            if compact {
                session.compact().await.unwrap();
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while let Some(event) = events.recv().await.unwrap() {
                    if matches!(
                        event.payload,
                        canter_protocol::EventPayload::SessionStatus {
                            status: canter_protocol::SessionStatus::Idle,
                            ..
                        }
                    ) && session.shared.state.lock().await.active.is_none()
                    {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            let before = {
                let state = session.shared.state.lock().await;
                request(&state, session.shared.options.max_output_tokens)
            };
            let snapshot = session.snapshot().await;
            let report = session.context().await;
            let after = {
                let state = session.shared.state.lock().await;
                request(&state, session.shared.options.max_output_tokens)
            };
            assert_eq!(before, after);
            assert_eq!(snapshot, session.snapshot().await);
            assert_eq!(report.estimated_tokens, request_tokens(&before));
            assert_eq!(
                report.tool_tokens,
                estimate(&serde_json::to_string(&before.tools).unwrap())
            );
            assert_eq!(report.messages.images, 1024);
            assert_eq!(report.compaction_tokens.is_some(), compact);
            assert_eq!(
                report.estimated_tokens,
                report.prefix.iter().map(|part| part.tokens).sum::<u64>()
                    + report.tool_tokens
                    + report.compaction_tokens.unwrap_or(0)
                    + report.messages.tokens
            );
            let state = session.shared.state.lock().await;
            for (index, message) in state.history.iter().enumerate() {
                if state.summary_index == Some(index) {
                    assert_eq!(report.compaction_tokens, Some(message_tokens(message)));
                }
            }
        }
    }
}
