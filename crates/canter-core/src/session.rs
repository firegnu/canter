use crate::{
    CoreError, CoreOptions, RuntimeContext, Subscription,
    context::HistoryEntry,
    journal::{Journal, now},
    runner,
};
use canter_model::{AssistantContent, Message, Model, ToolCall};
use canter_protocol::*;
use canter_store::Store;
use canter_tools::{LocalExecutor, PermissionGate, Tools};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};
use tokio::sync::{Mutex, oneshot, watch};
use tokio_util::sync::CancellationToken;

static IDS: AtomicU64 = AtomicU64::new(0);
pub(crate) fn id(kind: &str) -> String {
    format!("{kind}-{}-{}", now(), IDS.fetch_add(1, Ordering::Relaxed))
}

#[derive(Clone)]
pub struct Session {
    pub(crate) shared: Arc<Shared>,
    _lifetime: Arc<Lifetime>,
}
struct Lifetime(CancellationToken);
impl Drop for Lifetime {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
pub(crate) struct Shared {
    pub agent_parent: Arc<std::sync::RwLock<Option<std::sync::Weak<Shared>>>>,
    pub agent_host: std::sync::OnceLock<Arc<dyn crate::AgentHost>>,
    pub clients: Arc<std::sync::Mutex<crate::clients::Clients>>,
    pub hook_tasks: Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    pub input_waiters: AtomicUsize,
    pub state: Arc<Mutex<State>>,
    pub runtime: std::sync::RwLock<Arc<crate::configuration::Runtime>>,
    pub configuration: std::sync::OnceLock<Arc<dyn crate::ConfigurationSource>>,
    pub configuration_warnings: std::sync::OnceLock<tokio::sync::mpsc::UnboundedSender<String>>,
    pub lifetime: CancellationToken,
}
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct AgentPolicy {
    pub approval: PermissionMode,
    pub readonly: bool,
}

pub(crate) struct State {
    pub agent_forwarding: bool,
    pub suppressed_agent_turns: std::collections::HashSet<String>,
    pub acknowledged_agent_results: std::collections::HashSet<String>,
    pub agent_children: Vec<std::sync::Weak<Shared>>,
    pub permission_policy: watch::Sender<AgentPolicy>,
    pub inherited_policy: Option<watch::Receiver<AgentPolicy>>,
    pub fork_notice: Option<Message>,
    /// A non-interactive turn leaves automation paused until the next input.
    pub automatic_turns_paused: bool,
    pub lsp_delivery: crate::lsp::Delivery,
    pub goal_running: bool,
    pub goal_deadline: Option<tokio::time::Instant>,
    pub goal_summary: Option<(String, bool)>,
    directory_home: Option<std::path::PathBuf>,
    pub background_events: crate::background::PendingEvents,
    pub journal: Journal,
    pub prefix: Vec<Message>,
    pub prefix_segments: Option<Vec<crate::prompt::Segment>>,
    pub skills: Vec<Skill>,
    pub invoked_skills: Vec<crate::skill_invocation::Invocation>,
    pub used_files: Vec<(String, String)>,
    pub restored_file_notices: std::collections::BTreeSet<usize>,
    pub queued_skills: HashMap<String, Vec<crate::skill_invocation::Snapshot>>,
    pub tool_definitions: Vec<canter_model::ToolDefinition>,
    pub prefix_sections: Vec<ContextSection>,
    pub summary_index: Option<usize>,
    pub history: Vec<Message>,
    pub plan_mode_notices: Vec<Message>,
    pub tool_images: Vec<canter_model::UserContent>,
    /// Recent user inputs and tool-origin attachments, sharing the retention budget.
    pub user_messages: Vec<Message>,
    pub gate: PermissionGate,
    pub active: Option<Active>,
    pub btw: Option<crate::btw::ActiveBtw>,
    pub queue: VecDeque<QueuedInput>,
    pub steers: Vec<QueuedInput>,
    pub requests: HashMap<String, oneshot::Sender<ApprovalResolution>>,
    pub resolved: HashMap<String, u64>,
    pub closing: bool,
    pub hook_session_ended: bool,
    pub measured_context: Option<(u64, u64)>,
    pub context_budget_notices: std::collections::BTreeMap<usize, u8>,
    pub auto_compactions_without_progress: u8,
    pub rewind: crate::rewind::Checkpoints,
    plan_tool_calls: u8,
}
pub(crate) struct Active {
    pub pending_prompt: Option<(Vec<UserInput>, Vec<crate::skill_invocation::Snapshot>)>,
    pub prompt_blocked: bool,
    pub configuration_lease: Option<Arc<dyn Send + Sync>>,
    pub non_interactive: bool,
    pub approval_policy: Option<PermissionMode>,
    pub goal_continuation: bool,
    pub auto_wake: bool,
    pub plan_handoff: bool,
    pub turn: Turn,
    pub manual_compaction: bool,
    pub user_shell: Option<SessionShellCommandParams>,
    pub model_started: bool,
    pub retracted: bool,
    pub cancel: CancellationToken,
    pub done: watch::Receiver<Option<Result<u64, CoreError>>>,
}
impl State {
    pub(crate) fn readonly_agent(&self) -> bool {
        self.journal
            .snapshot
            .parent
            .as_ref()
            .is_some_and(|parent| parent.agent_type != AgentType::General)
    }

    pub(crate) fn allows_agent_tool(&self, name: &str, tools: &Tools) -> bool {
        if self.journal.snapshot.parent.is_none() {
            return true;
        }
        if name == "ask_user" || crate::agent_tools::is_tool(name) {
            return false;
        }
        if !self.readonly_agent() {
            return true;
        }
        name != "submit_plan"
            && (matches!(name, "shell" | "update_plan") || tools.allowed_in_plan_mode(name, "{}"))
    }
    pub(crate) fn non_interactive(&self) -> bool {
        self.active
            .as_ref()
            .is_some_and(|active| active.non_interactive)
    }

    pub(crate) fn approval_policy(&self) -> PermissionMode {
        let policy = self
            .inherited_policy
            .as_ref()
            .map(|policy| *policy.borrow())
            .unwrap_or_else(|| {
                let approval = self
                    .active
                    .as_ref()
                    .and_then(|active| active.approval_policy)
                    .unwrap_or(self.journal.snapshot.settings.approval_policy);
                AgentPolicy {
                    approval,
                    readonly: self.journal.snapshot.settings.plan_mode
                        || self
                            .active
                            .as_ref()
                            .is_some_and(|active| active.turn.review.is_some()),
                }
            });
        self.permission_policy.send_if_modified(|current| {
            let changed = *current != policy;
            *current = policy;
            changed
        });
        policy.approval
    }

    pub(crate) fn agent_requires_readonly(&self) -> bool {
        self.readonly_agent()
            || self
                .inherited_policy
                .as_ref()
                .is_some_and(|policy| policy.borrow().readonly)
    }

    pub(crate) async fn resolve_without_user(
        &mut self,
        request: PendingRequest,
        resolution: ApprovalResolution,
    ) -> Result<(), CoreError> {
        let request_id = request.id.clone();
        let events = self
            .journal
            .append(vec![
                EventPayload::RequestOpened { request },
                EventPayload::RequestResolved {
                    request_id: request_id.clone(),
                    resolution,
                    client_name: Some("non-interactive".into()),
                },
            ])
            .await?;
        self.resolved.insert(request_id, events[1].seq);
        Ok(())
    }
    fn new(journal: Journal) -> Self {
        Self {
            agent_children: Vec::new(),
            agent_forwarding: false,
            suppressed_agent_turns: Default::default(),
            acknowledged_agent_results: Default::default(),
            permission_policy: watch::channel(AgentPolicy {
                approval: journal.snapshot.settings.approval_policy,
                readonly: journal.snapshot.settings.plan_mode,
            })
            .0,
            inherited_policy: None,
            fork_notice: None,
            automatic_turns_paused: false,
            lsp_delivery: Default::default(),
            goal_running: false,
            goal_deadline: None,
            goal_summary: None,
            directory_home: None,
            background_events: Default::default(),
            journal,
            prefix: Vec::new(),
            prefix_segments: None,
            skills: Vec::new(),
            invoked_skills: Vec::new(),
            used_files: Vec::new(),
            restored_file_notices: Default::default(),
            queued_skills: HashMap::new(),
            tool_definitions: Vec::new(),
            prefix_sections: Vec::new(),
            summary_index: None,
            history: Vec::new(),
            plan_mode_notices: Vec::new(),
            tool_images: Vec::new(),
            user_messages: Vec::new(),
            gate: PermissionGate::default(),
            active: None,
            btw: None,
            queue: VecDeque::new(),
            steers: Vec::new(),
            requests: HashMap::new(),
            resolved: HashMap::new(),
            closing: false,
            hook_session_ended: false,
            measured_context: None,
            context_budget_notices: Default::default(),
            auto_compactions_without_progress: 0,
            rewind: Default::default(),
            plan_tool_calls: 0,
        }
    }
    async fn initialize_prefix(
        &mut self,
        home: Option<std::path::PathBuf>,
        canter_home: Option<std::path::PathBuf>,
        tools: &Tools,
        fresh_fork: bool,
    ) -> Result<(), CoreError> {
        self.directory_home = home.clone();
        tools.configure_shell(
            self.journal.snapshot.settings.approval_policy,
            &self.journal.snapshot.settings.additional_directories,
        );
        if let Some(lsp) = tools.lsp() {
            lsp.configure(
                self.journal
                    .snapshot
                    .settings
                    .additional_directories
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect(),
                if self.journal.snapshot.settings.plan_mode {
                    canter_tools::lsp::Round::Plan
                } else {
                    canter_tools::lsp::Round::Modify
                },
            );
        }
        self.gate
            .set_additional_directories(&self.journal.snapshot.settings.additional_directories);
        self.define_tools(tools);
        let scratch = tools.scratch_directory();
        let (mut segments, warnings, skills) = crate::context::prefix(
            self.journal.snapshot.settings.cwd.clone(),
            home,
            canter_home,
            self.tool_definitions.clone(),
            scratch
                .as_ref()
                .and_then(|scratch| scratch.path())
                .map(|path| (path, tools.sandbox_enabled())),
        )
        .await?;
        if !self
            .journal
            .snapshot
            .settings
            .additional_directories
            .is_empty()
            && let Some(environment) = segments
                .iter_mut()
                .find(|segment| segment.kind == crate::prompt::SegmentKind::Environment)
        {
            environment.text.push_str(&format!(
                "\nAdditional directories: {}\n",
                serde_json::to_string(&self.journal.snapshot.settings.additional_directories)
                    .expect("directory list")
            ));
        }
        if let Some(catalog) = tools.mcp_catalog() {
            segments.push(crate::prompt::Segment {
                kind: crate::prompt::SegmentKind::McpCatalog,
                path: None,
                text: catalog,
            });
        }
        if self.journal.snapshot.parent.is_some()
            && let Some(core) = segments
                .iter_mut()
                .find(|segment| segment.kind == crate::prompt::SegmentKind::Core)
        {
            core.text.push_str(crate::agent_tools::PROMPT);
        }
        if !fresh_fork {
            segments.extend(crate::memory::segments(tools).await);
        }
        let (prefix, sections) = crate::prompt::chat_completions(&segments);
        self.skills = skills;
        self.prefix_sections = sections;
        if fresh_fork {
            if tools
                .memory()
                .is_none_or(|memory| memory.roots().ok().flatten().is_none())
            {
                let mut saved = self.prefix_segments.clone().unwrap_or_default();
                if saved.iter().any(crate::memory::is_memory) {
                    saved.retain(|segment| !crate::memory::is_memory(segment));
                    self.set_memory_prefix(saved).await?;
                }
            }
            if let Some(saved) = &self.prefix_segments {
                self.prefix_sections = crate::prompt::chat_completions(saved).1;
            }
        } else if self.prefix != prefix || self.prefix_segments.as_ref() != Some(&segments) {
            self.journal
                .record(HistoryEntry::Context {
                    messages: prefix.clone(),
                    segments: Some(segments.clone()),
                })
                .await?;
            self.prefix = prefix;
            self.prefix_segments = Some(segments);
        }
        if !warnings.is_empty() {
            self.journal
                .append(
                    warnings
                        .into_iter()
                        .map(|message| EventPayload::Warning { message })
                        .collect(),
                )
                .await?;
        }
        Ok(())
    }
    fn define_tools(&mut self, tools: &Tools) {
        self.tool_definitions = canter_tools::Tool::ALL
            .into_iter()
            .filter(|tool| {
                tools.find(tool.name()).is_some()
                    || (self.journal.snapshot.parent.is_some()
                        && *tool == canter_tools::Tool::UpdatePlan)
            })
            .map(|tool| {
                let def = tools.definition(tool);
                canter_model::ToolDefinition {
                    name: def.name.into(),
                    description: def.description.into(),
                    parameters: def.parameters,
                }
            })
            .chain(crate::goal::definitions())
            .chain(crate::agent_tools::definitions())
            .chain(
                canter_tools::Tool::MCP
                    .into_iter()
                    .filter(|tool| tools.find(tool.name()).is_some())
                    .map(|tool| {
                        let def = tool.definition();
                        canter_model::ToolDefinition {
                            name: def.name.into(),
                            description: def.description.into(),
                            parameters: def.parameters,
                        }
                    }),
            )
            .chain(
                tools
                    .mcp_tools()
                    .iter()
                    .filter(|_| tools.find("mcp_search").is_none())
                    .map(|tool| canter_model::ToolDefinition {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    }),
            )
            .filter(|definition| self.allows_agent_tool(&definition.name, tools))
            .collect();
    }

    pub(crate) async fn refresh_tools(&mut self, tools: &Tools) -> Result<(), CoreError> {
        self.define_tools(tools);
        let mut segments = self.prefix_segments.clone().unwrap_or_default();
        if let Some(core) = segments
            .iter_mut()
            .find(|segment| segment.kind == crate::prompt::SegmentKind::Core)
        {
            core.text = crate::prompt::core(&self.tool_definitions);
            if self.journal.snapshot.parent.is_some() {
                core.text.push_str(crate::agent_tools::PROMPT);
            }
        }
        segments.retain(|segment| segment.kind != crate::prompt::SegmentKind::McpCatalog);
        if let Some(text) = tools.mcp_catalog() {
            segments.push(crate::prompt::Segment {
                kind: crate::prompt::SegmentKind::McpCatalog,
                path: None,
                text,
            });
        }
        let (prefix, sections) = crate::prompt::chat_completions(&segments);
        if self.prefix != prefix {
            self.journal
                .record(HistoryEntry::Context {
                    messages: prefix.clone(),
                    segments: Some(segments.clone()),
                })
                .await?;
            self.prefix = prefix;
            self.prefix_segments = Some(segments);
            self.prefix_sections = sections;
            self.measured_context = None;
        }
        Ok(())
    }

    pub async fn message(&mut self, message: Message) -> Result<(), CoreError> {
        self.journal
            .record(HistoryEntry::Message {
                message: message.clone(),
                user_input: false,
                skill_input: None,
            })
            .await?;
        self.history.push(message);
        crate::context::flush_tool_images(
            &mut self.history,
            &mut self.user_messages,
            &mut self.tool_images,
        );
        crate::context::flush_plan_mode_notices(&mut self.history, &mut self.plan_mode_notices);
        Ok(())
    }
    pub async fn tool_result(
        &mut self,
        call: &ToolCall,
        mut text: String,
        succeeded: bool,
        output_size: canter_tools::OutputSize,
        item_id: Option<&str>,
    ) -> Result<(), CoreError> {
        if call.name == "update_plan" {
            if succeeded {
                self.plan_tool_calls = 0;
            }
        } else if !self.journal.snapshot.settings.plan_mode
            && self.journal.snapshot.settings.plan_enabled != Some(false)
            && self.journal.snapshot.current_plan().is_some_and(|plan| {
                plan.iter().any(|step| {
                    matches!(
                        step.status,
                        PlanStepStatus::Pending | PlanStepStatus::InProgress
                    )
                })
            })
        {
            self.plan_tool_calls += 1;
            if self.plan_tool_calls == 10 {
                // Append only to this new model result, never to the UI item or user input.
                text.push_str("\n\n[Plan reminder] If any plan steps are complete, call update_plan to mark them completed before continuing.");
                self.plan_tool_calls = 0;
            }
        } else {
            self.plan_tool_calls = 0;
        }
        if let Some(pending) = &self.journal.telemetry {
            pending.lock().expect("telemetry queue").events.push(
                EventPayload::TelemetryToolResult {
                    turn_id: self
                        .active
                        .as_ref()
                        .expect("tool belongs to turn")
                        .turn
                        .id
                        .clone(),
                    tool_call_id: call.id.clone(),
                    item_id: item_id.map(Into::into),
                    estimated: true,
                    result_tokens: crate::context::estimate(&text),
                    truncated: output_size.truncated,
                    original_tokens: output_size.original_bytes.map(|bytes| bytes.div_ceil(3)),
                    source_file_bytes: output_size.source_file_bytes,
                },
            );
        }
        self.message(Message::ToolResult {
            tool_call_id: call.id.clone(),
            text,
        })
        .await
    }
    pub(crate) async fn user_input(
        &mut self,
        input: &[UserInput],
        turn_id: &str,
        skills: Vec<crate::skill_invocation::Snapshot>,
    ) -> Result<(), CoreError> {
        let message = crate::skill_invocation::message(input, &skills, true);
        let user_message = crate::skill_invocation::message(input, &skills, false);
        let skill_input = (!skills.is_empty()).then(|| crate::skill_invocation::Invocation {
            turn_id: turn_id.into(),
            skills,
            user_message: user_message.clone(),
        });
        self.journal
            .record(HistoryEntry::Message {
                message: message.clone(),
                user_input: true,
                skill_input: skill_input.clone(),
            })
            .await?;
        self.user_messages.push(user_message);
        self.invoked_skills.extend(skill_input);
        self.history.push(message);
        Ok(())
    }
    pub async fn complete_missing_tools(&mut self, explanation: &str) -> Result<(), CoreError> {
        let Some(index) = self
            .history
            .iter()
            .rposition(|message| matches!(message, Message::Assistant { .. }))
        else {
            return Ok(());
        };
        let Message::Assistant { content } = &self.history[index] else {
            unreachable!()
        };
        let missing: Vec<_> = content.iter().filter_map(|part| {
            let AssistantContent::ToolCall(call) = part else { return None; };
            (!self.history[index + 1..].iter().any(|message| matches!(message, Message::ToolResult { tool_call_id, .. } if tool_call_id == &call.id))).then(|| call.id.clone())
        }).collect();
        for tool_call_id in missing {
            self.message(Message::ToolResult {
                tool_call_id,
                text: explanation.into(),
            })
            .await?;
        }
        Ok(())
    }
    pub async fn discard_queue(&mut self, reason: InputRemovalReason) -> Result<(), CoreError> {
        let input_ids = self
            .journal
            .snapshot
            .queued_inputs
            .iter()
            .map(|input| input.id.clone())
            .collect::<Vec<_>>();
        if !input_ids.is_empty() {
            self.journal
                .append(vec![EventPayload::InputRemoved {
                    input_ids,
                    reason,
                    turn_id: None,
                }])
                .await?;
        }
        self.queue.clear();
        self.steers.clear();
        self.queued_skills.clear();
        Ok(())
    }
    pub async fn consume_steering(
        &mut self,
        turn_id: &str,
        inputs: Vec<QueuedInput>,
    ) -> Result<(), CoreError> {
        if inputs.is_empty() {
            return Ok(());
        }
        let mut payloads = Vec::new();
        for input in &inputs {
            let skills = self.queued_skills.remove(&input.id).unwrap_or_default();
            self.user_input(&input.input, turn_id, skills).await?;
            payloads.push(EventPayload::ItemCompleted {
                turn_id: turn_id.into(),
                item: Item::UserMessage {
                    id: id("user"),
                    content: input.input.clone(),
                },
            });
        }
        payloads.push(EventPayload::InputRemoved {
            input_ids: inputs.iter().map(|input| input.id.clone()).collect(),
            reason: if inputs.len() > 1 {
                InputRemovalReason::Merged
            } else {
                InputRemovalReason::Steered
            },
            turn_id: Some(turn_id.into()),
        });
        self.journal.append(payloads).await?;
        self.steers
            .retain(|input| !inputs.iter().any(|used| used.id == input.id));
        Ok(())
    }
    pub async fn cancel_requests(&mut self) -> Result<(), CoreError> {
        let payloads = self
            .journal
            .snapshot
            .pending_requests
            .iter()
            .map(|request| EventPayload::RequestResolved {
                request_id: request.id.clone(),
                resolution: ApprovalResolution::Cancelled,
                client_name: None,
            })
            .collect();
        for event in self.journal.append(payloads).await? {
            if let EventPayload::RequestResolved { request_id, .. } = event.payload {
                self.requests.remove(&request_id);
                self.resolved.insert(request_id, event.seq);
            }
        }
        Ok(())
    }
    pub fn available(&self) -> Result<(), CoreError> {
        if self.journal.failed {
            return Err(CoreError::internal());
        }
        if self.closing || self.journal.snapshot.status == SessionStatus::Closed {
            return Err(CoreError::new(ErrorCode::SessionClosed, "session closed"));
        }
        if self.btw.as_ref().is_some_and(|answer| answer.handoff) {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "A handoff is in progress; wait for it to finish or cancel it",
            ));
        }
        Ok(())
    }
}

impl Session {
    async fn scratch_warning(&self) -> Result<(), CoreError> {
        if let Some(scratch) = self.shared.runtime().tools.scratch_directory()
            && let Some(warning) = scratch.warning()
        {
            self.opening_warning(warning.to_owned()).await?;
        }
        Ok(())
    }

    async fn memory_warning(&self) -> Result<(), CoreError> {
        if let Some(memory) = self.shared.runtime().tools.memory()
            && let Some(warning) = memory.take_warning()
        {
            self.opening_warning(warning).await?;
        }
        Ok(())
    }

    /// Record an opening warning before returning to a client that may immediately
    /// start a turn. The caller retains the snapshot cursor from before this event.
    pub async fn opening_warning(&self, message: String) -> Result<(), CoreError> {
        self.shared
            .state
            .lock()
            .await
            .journal
            .append(vec![EventPayload::Warning { message }])
            .await?;
        Ok(())
    }

    async fn session_start_hook(&self, source: &str) {
        let state = self.shared.state.lock().await;
        crate::hooks::notify(
            &self.shared,
            &state,
            crate::hooks::Event::SessionStart,
            serde_json::json!({"source":source}),
            self.shared.lifetime.child_token(),
        );
    }

    /// Recheck trust when resuming, including a failed lookup. Granting trust
    /// never starts LSP by itself; the next file change drives startup.
    pub async fn set_folder_trust(&self, trusted: bool) -> Result<(), CoreError> {
        let session = self.clone();
        owned(async move { session.set_folder_trust_inner(trusted).await }).await
    }

    async fn set_folder_trust_inner(&self, trusted: bool) -> Result<(), CoreError> {
        let mut state = self.shared.state.lock().await;
        if self.shared.runtime().context.restricted_mode != trusted {
            return Ok(());
        }
        let mut runtime = (*self.shared.runtime()).clone();
        runtime.context.restricted_mode = !trusted;
        let memory = runtime.tools.memory();
        let update = async {
            if let Some(memory) = &memory {
                let opening = memory.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(error) = opening.set_enabled(trusted) {
                        opening.disable_with_warning(error.to_string());
                    }
                })
                .await
                .map_err(|_| CoreError::internal())?;
                state.refresh_memory(&runtime.tools).await?;
                if let Some(message) = memory.take_warning() {
                    state
                        .journal
                        .append(vec![EventPayload::Warning { message }])
                        .await?;
                }
            }
            if !trusted {
                if let Some(lsp) = runtime.tools.lsp() {
                    lsp.close().await;
                }
                runtime.tools = runtime.tools.without_lsp();
                state.lsp_delivery = Default::default();
                let stopped = state
                    .journal
                    .snapshot
                    .language_servers
                    .iter()
                    .cloned()
                    .map(|mut server| {
                        server.state = canter_protocol::LanguageServerState::Stopped;
                        server.reason = Some("Folder is not trusted".into());
                        server.shared_references = 0;
                        server.rss_bytes = None;
                        server.updated_at = crate::journal::now();
                        EventPayload::LanguageServerUpdated { server }
                    })
                    .collect();
                state.journal.append(stopped).await?;
            } else if let Some(service) = &runtime.context.lsp {
                runtime.tools = runtime.tools.with_lsp(
                    service.session_with_access(
                        canter_tools::lsp::FileContext {
                            cwd: state.journal.snapshot.settings.cwd.clone().into(),
                            additional_directories: state
                                .journal
                                .snapshot
                                .settings
                                .additional_directories
                                .iter()
                                .map(Into::into)
                                .collect(),
                            environment: runtime.context.environment.clone(),
                            round: if state.journal.snapshot.settings.plan_mode {
                                canter_tools::lsp::Round::Plan
                            } else if state
                                .active
                                .as_ref()
                                .is_some_and(|active| active.turn.review.is_some())
                            {
                                canter_tools::lsp::Round::Review
                            } else {
                                canter_tools::lsp::Round::Modify
                            },
                        },
                        runtime.context.protected_paths.clone(),
                        runtime.context.lsp_file_access.clone(),
                    ),
                );
            }
            let mut settings = state.journal.snapshot.settings.clone();
            settings.restricted_mode = Some(!trusted);
            state
                .journal
                .append(vec![EventPayload::SessionUpdated { settings }])
                .await?;
            Ok::<_, CoreError>(runtime)
        }
        .await;
        let runtime = match update {
            Ok(runtime) => runtime,
            Err(error) => {
                if let Some(memory) = memory {
                    memory
                        .set_enabled(false)
                        .expect("disabling memory does not access the filesystem");
                    // The journal may no longer accept a corrective record. Remove memory
                    // from the live prefix; a later open regenerates its persisted prefix.
                    if let Some(segments) = state.prefix_segments.as_mut() {
                        segments.retain(|segment| !crate::memory::is_memory(segment));
                        let (prefix, sections) = crate::prompt::chat_completions(segments);
                        state.prefix = prefix;
                        state.prefix_sections = sections;
                        state.measured_context = None;
                    }
                }
                return Err(error);
            }
        };
        *self.shared.runtime.write().unwrap() = Arc::new(runtime);
        Ok(())
    }

    pub async fn open(
        store: Arc<Store>,
        id: String,
        runtime: RuntimeContext,
        model: Arc<dyn Model>,
        options: CoreOptions,
    ) -> Result<(Self, SessionReadResponse), CoreError> {
        owned(Self::open_inner(store, id, runtime, model, options, false)).await
    }
    pub(crate) async fn open_inner(
        store: Arc<Store>,
        id: String,
        mut runtime: RuntimeContext,
        model: Arc<dyn Model>,
        mut options: CoreOptions,
        fresh_fork: bool,
    ) -> Result<(Self, SessionReadResponse), CoreError> {
        validate_options(&options)?;
        let owned_store = store.clone();
        let (writer, entries, events) = tokio::task::spawn_blocking(move || {
            let writer = owned_store.open(&id)?;
            let entries = owned_store.model_history::<HistoryEntry>(&id)?;
            let events = owned_store.events_after(&id, 0)?;
            Ok::<_, CoreError>((writer, entries, events))
        })
        .await
        .map_err(|_| CoreError::internal())??;
        if writer.snapshot().parent.is_some() {
            runtime.memory_home = None;
            options.sandbox = writer
                .snapshot()
                .settings
                .sandbox_enabled
                .unwrap_or(options.sandbox);
            runtime.restricted_mode = writer
                .snapshot()
                .settings
                .restricted_mode
                .unwrap_or(runtime.restricted_mode);
        }
        let home = runtime
            .environment
            .get("HOME")
            .map(std::path::PathBuf::from);
        let canter_home = crate::context::canter_home(&runtime.environment);
        let tools = executor(
            writer.snapshot().settings.cwd.clone(),
            store.session_dir(&writer.snapshot().id)?,
            runtime.clone(),
            &options,
        )
        .await?;
        let mut state = State::new(Journal::new(store, writer, options.telemetry));
        state.fork_notice = entries.iter().rev().find_map(|entry| match entry {
            HistoryEntry::ForkNotice { message } => Some(message.clone()),
            _ => None,
        });
        if let Some(lsp) = tools.lsp() {
            lsp.restore_server_ids(&state.journal.snapshot.language_servers);
        }
        crate::usage::Ledger::replay(&events, &options.prices)
            .fill_costs(&mut state.journal.snapshot);
        let restored = crate::context::restore(entries.clone(), &mut state.rewind);
        state.acknowledged_agent_results = restored.acknowledged_agent_results;
        state.prefix = restored.prefix;
        state.invoked_skills = restored.invoked_skills;
        state.restored_file_notices = restored.restored_file_notices;
        state.prefix_segments = restored.segments;
        state.history = restored.history;
        state.lsp_delivery.notices = restored.diagnostic_notices;
        state.context_budget_notices = restored.context_budget_notices;
        state.plan_mode_notices = restored.plan_mode_notices;
        state.tool_images = restored.tool_images;
        state.user_messages = restored.users;
        state.summary_index = restored.summary_index;
        state.background_events.all = restored.background_cards;
        if fresh_fork {
            let path = tools
                .scratch_directory()
                .and_then(|scratch| scratch.path())
                .ok_or_else(|| {
                    CoreError::new(
                        ErrorCode::Internal,
                        "Could not create the fork's scratch directory",
                    )
                })?;
            let message = Message::User {
                text: format!(
                    "This session was forked from another session. Its scratch directory is now {}; use it instead of the scratch directory named earlier.",
                    path.display()
                ),
            };
            state.fork_notice = Some(message);
            state.restate_fork_notice().await?;
        }
        let recovered = state
            .journal
            .snapshot
            .language_servers
            .iter()
            .cloned()
            .map(|mut server| {
                if server.state != LanguageServerState::Stopped {
                    server.state = LanguageServerState::Reconnecting;
                    server.reason = Some("LSP server is not running after session restore".into());
                }
                server.rss_bytes = None;
                server.shared_references = 0;
                server.updated_at = now();
                EventPayload::LanguageServerUpdated { server }
            })
            .collect::<Vec<_>>();
        state.requeue_background_cards();
        for entry in &entries {
            let recovery = match entry {
                HistoryEntry::Rewind { rewind } => Some((rewind.event_seq, rewind.event())),
                HistoryEntry::Retract { turn_id, event_seq } => Some((
                    *event_seq,
                    EventPayload::TurnRetracted {
                        turn_id: turn_id.clone(),
                    },
                )),
                _ => None,
            };
            if let Some((seq, event)) = recovery
                && seq > state.journal.snapshot.last_event_seq
            {
                state.journal.append(vec![event]).await?;
            }
        }
        if !recovered.is_empty() {
            state.journal.append(recovered).await?;
        }
        state
            .initialize_prefix(home, canter_home, &tools, fresh_fork)
            .await?;
        if let Some(lsp) = tools.lsp() {
            let cwd = std::path::Path::new(&state.journal.snapshot.settings.cwd);
            let files = state
                .journal
                .snapshot
                .turns
                .iter()
                .flat_map(|turn| &turn.items)
                .filter_map(|item| match item {
                    Item::FileChange {
                        changes,
                        status: ItemStatus::Completed,
                        ..
                    } => Some(changes),
                    _ => None,
                })
                .flatten()
                .map(|change| cwd.join(&change.path))
                .collect::<Vec<_>>();
            lsp.restore_subscriptions(&state.journal.snapshot.language_servers, &files);
        }
        // Both switches come from this server and executor, not from the saved session.
        let sandbox_enabled = Some(tools.sandbox_enabled());
        if state.journal.snapshot.settings.plan_enabled != Some(options.plan_enabled)
            || state.journal.snapshot.settings.sandbox_enabled != sandbox_enabled
            || state.journal.snapshot.settings.restricted_mode != Some(runtime.restricted_mode)
        {
            let mut settings = state.journal.snapshot.settings.clone();
            settings.plan_enabled = Some(options.plan_enabled);
            settings.sandbox_enabled = sandbox_enabled;
            settings.restricted_mode = Some(runtime.restricted_mode);
            state
                .journal
                .append(vec![EventPayload::SessionUpdated { settings }])
                .await?;
        }
        for event in events {
            if let EventPayload::RequestResolved { request_id, .. } = event.payload {
                state.resolved.insert(request_id, event.seq);
            }
        }
        state
            .complete_missing_tools("No result: the server stopped and this turn was interrupted")
            .await?;
        state.recover_background().await?;
        state.restate_goal().await?;
        state.cancel_requests().await?;
        let stale = state
            .journal
            .snapshot
            .agent_requests
            .iter()
            .map(|request| EventPayload::AgentRequestResolved {
                agent_id: request.agent_id.clone(),
                request_id: request.request.id.clone(),
                resolution: ApprovalResolution::Cancelled,
                client_name: None,
            })
            .collect();
        state.journal.append(stale).await?;
        let unfinished = state
            .journal
            .snapshot
            .turns
            .iter()
            .filter(|turn| turn.status == TurnStatus::InProgress)
            .cloned()
            .collect::<Vec<_>>();
        if !unfinished.is_empty() {
            state
                .message(Message::User {
                    text: "[The server stopped and the previous turn was interrupted; continue from the results so far.]".into(),
                })
                .await?;
        }
        let mut payloads = Vec::new();
        for turn in unfinished {
            payloads.extend(interrupted_turn(turn));
        }
        state.discard_queue(InputRemovalReason::Interrupted).await?;
        payloads.push(EventPayload::SessionStatus {
            status: SessionStatus::Idle,
            close_reason: None,
        });
        state.journal.append(payloads).await?;
        let response = SessionReadResponse {
            session: state.journal.snapshot.clone(),
            event_seq: state.journal.snapshot.last_event_seq,
        };
        let session = Self::from_state(state, tools, model, options, runtime);
        // Keep this warning after the opening client's snapshot cursor.
        session.scratch_warning().await?;
        session.memory_warning().await?;
        session
            .session_start_hook(if fresh_fork { "startup" } else { "resume" })
            .await;
        Ok((session, response))
    }

    pub async fn create(
        store: Arc<Store>,
        id: String,
        settings: SessionSettings,
        runtime: RuntimeContext,
        model: Arc<dyn Model>,
        options: CoreOptions,
    ) -> Result<(Self, SessionCreateResponse), CoreError> {
        owned(Self::create_inner(
            store, id, settings, runtime, model, options, None,
        ))
        .await
    }
    pub(crate) async fn create_inner(
        store: Arc<Store>,
        id: String,
        mut settings: SessionSettings,
        mut runtime: RuntimeContext,
        model: Arc<dyn Model>,
        options: CoreOptions,
        parent: Option<SessionParent>,
    ) -> Result<(Self, SessionCreateResponse), CoreError> {
        validate_options(&options)?;
        if settings.model.trim().is_empty()
            || settings.approval_policy == PermissionMode::Unknown
            || settings.reasoning_effort == ReasoningEffort::Unknown
        {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "invalid session settings",
            ));
        }
        if parent.is_some() {
            runtime.memory_home = None;
        }
        let cwd = settings.cwd.clone();
        let home = runtime
            .environment
            .get("HOME")
            .map(std::path::PathBuf::from);
        let canter_home = crate::context::canter_home(&runtime.environment);
        settings.additional_directories = crate::directories::append(
            settings.cwd.clone(),
            home.clone(),
            Vec::new(),
            settings.additional_directories,
        )
        .await?;
        settings.sandbox_enabled = Some(options.sandbox);
        settings.restricted_mode = Some(runtime.restricted_mode);
        settings.plan_enabled = Some(options.plan_enabled);
        let owned_store = store.clone();
        let writer = tokio::task::spawn_blocking(move || match parent {
            Some(parent) => owned_store.create_child(&id, settings, parent, now()),
            None => owned_store.create(&id, settings, now()),
        })
        .await
        .map_err(|_| CoreError::internal())??;
        let tools = executor(
            cwd,
            store.session_dir(&writer.snapshot().id)?,
            runtime.clone(),
            &options,
        )
        .await?;
        let mut journal = Journal::new(store, writer, options.telemetry);
        journal
            .append(vec![EventPayload::SessionStatus {
                status: SessionStatus::Idle,
                close_reason: None,
            }])
            .await?;
        let mut state = State::new(journal);
        state
            .initialize_prefix(home, canter_home, &tools, false)
            .await?;
        let response = SessionCreateResponse {
            session: state.journal.snapshot.clone(),
            event_seq: state.journal.snapshot.last_event_seq,
        };
        let session = Self::from_state(state, tools, model, options, runtime);
        session.scratch_warning().await?;
        session.memory_warning().await?;
        session.session_start_hook("startup").await;
        Ok((session, response))
    }

    fn from_state(
        mut state: State,
        tools: Tools,
        model: Arc<dyn Model>,
        options: CoreOptions,
        context: RuntimeContext,
    ) -> Self {
        if let Some(path) = &options.command_rules_path {
            state.gate = std::mem::take(&mut state.gate).with_rules_file(path.clone());
        }
        tools
            .background()
            .restore(&state.journal.snapshot.background_commands);
        let lifetime = CancellationToken::new();
        let shared = Arc::new(Shared {
            agent_parent: Default::default(),
            agent_host: Default::default(),
            clients: Default::default(),
            hook_tasks: Default::default(),
            input_waiters: AtomicUsize::new(0),
            state: Arc::new(Mutex::new(state)),
            runtime: std::sync::RwLock::new(Arc::new(crate::configuration::Runtime {
                revision: None,
                model,
                tools,
                options,
                context,
                retired_tools: Vec::new(),
            })),
            configuration: Default::default(),
            configuration_warnings: Default::default(),
            lifetime: lifetime.clone(),
        });
        crate::background::watch(&shared);
        crate::lsp::watch(&shared);
        Self {
            shared,
            _lifetime: Arc::new(Lifetime(lifetime)),
        }
    }

    pub async fn input(
        &self,
        params: SessionInputParams,
    ) -> Result<SessionInputResponse, CoreError> {
        let session = self.clone();
        session.shared.input_waiters.fetch_add(1, Ordering::SeqCst);
        owned(async move {
            let _waiting = InputWaiting(session.shared.clone());
            session.input_inner(params).await
        })
        .await
    }
    async fn input_inner(
        &self,
        mut params: SessionInputParams,
    ) -> Result<SessionInputResponse, CoreError> {
        if params.mode == InputMode::Unknown
            || params.approval_policy == Some(PermissionMode::Unknown)
            || params.input.is_empty()
            || params
                .input
                .iter()
                .any(|input| matches!(input, UserInput::Unknown))
        {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "input must contain supported text, images or skills",
            ));
        }
        let loaded = if params
            .input
            .iter()
            .any(|part| matches!(part, UserInput::LocalImage { .. }))
        {
            let cwd = self
                .shared
                .state
                .lock()
                .await
                .journal
                .snapshot
                .settings
                .cwd
                .clone();
            Some(crate::images::load(cwd, std::mem::take(&mut params.input)).await?)
        } else {
            None
        };
        let mut state = self.shared.state.lock().await;
        state.available()?;
        if state.active.is_some()
            && (state.non_interactive()
                || params.non_interactive
                || params.approval_policy.is_some())
        {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "input with turn-scoped settings requires an idle session",
            ));
        }
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.cancel.is_cancelled())
        {
            return Err(CoreError::new(ErrorCode::Conflict, "turn is stopping"));
        }
        let imported = if let Some(loaded) = loaded {
            let model = &state.journal.snapshot.settings.model;
            if state.active.is_some() && !self.shared.runtime().options.image_models.contains(model)
            {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest,
                    &format!("{model} does not accept images"),
                ));
            }
            let (input, imported) = crate::images::import(
                state.journal.store.clone(),
                state.journal.snapshot.id.clone(),
                loaded,
            )
            .await?;
            params.input = input;
            Some(imported)
        } else {
            None
        };
        let skills = crate::skill_invocation::load(&params.input, &state.skills).await?;
        state.auto_compactions_without_progress = 0;
        state.background_events.user_input();
        if state
            .journal
            .snapshot
            .goal
            .as_ref()
            .is_some_and(|g| g.status == GoalStatus::Active)
        {
            state.goal_running = true;
            state.goal_deadline = None;
        }
        if state.active.is_some() {
            let input = QueuedInput {
                review: params.review.clone(),
                id: id("input"),
                input: params.input,
                client_name: params.client_name,
            };
            let events = state
                .journal
                .append(vec![EventPayload::InputQueued {
                    input: input.clone(),
                }])
                .await?;
            let turn_id = if params.mode == InputMode::Steer
                && params.review.is_none()
                && state
                    .active
                    .as_ref()
                    .expect("active turn")
                    .user_shell
                    .is_none()
                && !state
                    .active
                    .as_ref()
                    .expect("active turn")
                    .manual_compaction
            {
                state.queued_skills.insert(input.id.clone(), skills);
                state.steers.push(input);
                state.active.as_ref().map(|active| active.turn.id.clone())
            } else {
                state.queued_skills.insert(input.id.clone(), skills);
                state.queue.push_back(input);
                None
            };
            if let Some(imported) = imported {
                imported.keep();
            }
            return Ok(SessionInputResponse {
                event_seq: events[0].seq,
                turn_id,
            });
        }
        let (response, done) = self.shared.start(&mut state, params, None, skills).await?;
        if let Some(imported) = imported {
            imported.keep();
        }
        let shared = self.shared.clone();
        tokio::spawn(async move {
            runner::run(shared, done).await;
        });
        Ok(response)
    }

    pub async fn subscribe(&self, after: u64) -> Result<Subscription, CoreError> {
        let state = self.shared.state.lock().await;
        if after > state.journal.snapshot.last_event_seq {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "event cursor is ahead of session",
            ));
        }
        let receiver = state.journal.events.as_ref().map_or_else(
            || tokio::sync::broadcast::channel(1).1,
            |sender| sender.subscribe(),
        );
        let mut subscription = Subscription {
            disk_reader: state
                .journal
                .store
                .event_reader(&state.journal.snapshot.id)?,
            receiver,
            committed: state.journal.committed.subscribe(),
            pending: VecDeque::new(),
            cursor: after,
            ended: false,
        };
        drop(state);
        subscription.catch_up().await?;
        Ok(subscription)
    }

    /// Skills discovered when this session was created or opened; never rescans.
    pub async fn skills(&self) -> SkillListResponse {
        SkillListResponse {
            data: self.shared.state.lock().await.skills.clone(),
        }
    }

    /// What model commands may touch now, worked out on each call and never stored.
    /// The session lock orders it after settings updates, which reconfigure the shell.
    pub async fn sandbox(&self) -> canter_protocol::SessionSandboxResponse {
        let _state = self.shared.state.lock().await;
        self.shared.runtime().tools.sandbox_scope()
    }

    pub async fn snapshot(&self) -> SessionReadResponse {
        let state = self.shared.state.lock().await;
        SessionReadResponse {
            session: state.journal.snapshot.clone(),
            event_seq: state.journal.snapshot.last_event_seq,
        }
    }

    /// Distinguish clones of this open session from a later reopen of the same id.
    pub fn same_instance(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.shared, &other.shared)
    }

    pub async fn interrupt(
        &self,
        params: SessionInterruptParams,
    ) -> Result<SessionInterruptResponse, CoreError> {
        let session = self.clone();
        owned(async move { session.interrupt_inner(params).await }).await
    }
    async fn interrupt_inner(
        &self,
        params: SessionInterruptParams,
    ) -> Result<SessionInterruptResponse, CoreError> {
        let (done, retracted, children) = {
            let mut state = self.shared.state.lock().await;
            state.available()?;
            let active = state
                .active
                .as_mut()
                .filter(|active| active.turn.id == params.turn_id)
                .ok_or_else(|| CoreError::new(ErrorCode::TurnMismatch, "turn is not active"))?;
            if params.retract == Some(true)
                && !active.cancel.is_cancelled()
                && !active.manual_compaction
                && active.user_shell.is_none()
                && !active.auto_wake
                && !active.goal_continuation
                && !active.model_started
            {
                active.retracted = true;
            }
            active.cancel.cancel();
            let result = (active.done.clone(), active.retracted);
            if state
                .journal
                .snapshot
                .goal
                .as_ref()
                .is_some_and(|g| g.status == GoalStatus::Active)
            {
                state.goal_status(GoalStatus::Paused).await?;
            }
            state.goal_summary = None;
            state.background_events.interrupt();
            let children = state.interrupt_agents().await;
            (result.0, result.1, children)
        };
        crate::agents::finish_agent_stops(children).await?;
        Ok(SessionInterruptResponse {
            event_seq: wait_done(done).await?,
            retracted: params.retract.map(|_| retracted),
        })
    }

    pub async fn resolve(
        &self,
        request_id: &str,
        params: RequestResolveParams,
    ) -> Result<RequestResolveResponse, CoreError> {
        let session = self.clone();
        let request_id = request_id.to_owned();
        owned(async move { session.resolve_inner(&request_id, params).await }).await
    }
    async fn resolve_inner(
        &self,
        request_id: &str,
        params: RequestResolveParams,
    ) -> Result<RequestResolveResponse, CoreError> {
        let mut state = self.shared.state.lock().await;
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.cancel.is_cancelled())
        {
            state.cancel_requests().await?;
        }
        if let Some(&event_seq) = state.resolved.get(request_id) {
            return Ok(RequestResolveResponse {
                event_seq,
                already_resolved: true,
            });
        }
        state.available()?;
        let request = state
            .journal
            .snapshot
            .pending_requests
            .iter()
            .find(|request| request.id == request_id)
            .ok_or_else(|| CoreError::new(ErrorCode::RequestNotFound, "request not found"))?;
        let amendment = if params.decision == Some(ApprovalDecision::AcceptWithExecpolicyAmendment)
        {
            let RequestKind::Approval(approval) = &request.kind else {
                return Err(CoreError::new(
                    ErrorCode::InvalidDecision,
                    "invalid command rule answer",
                ));
            };
            if params.execpolicy_amendment.is_none()
                || params.execpolicy_amendment != approval.proposed_execpolicy_amendment
            {
                return Err(CoreError::new(
                    ErrorCode::InvalidDecision,
                    "answer must match the proposed command prefixes",
                ));
            }
            params.execpolicy_amendment.clone()
        } else {
            if params.execpolicy_amendment.is_some() {
                return Err(CoreError::new(
                    ErrorCode::InvalidDecision,
                    "command prefixes require an amendment decision",
                ));
            }
            None
        };
        let valid_feedback = if params.decision == Some(ApprovalDecision::DeclineWithFeedback) {
            params
                .feedback
                .as_ref()
                .is_some_and(|text| !text.trim().is_empty())
        } else {
            params.feedback.is_none()
        };
        if !valid_feedback {
            return Err(CoreError::new(
                ErrorCode::InvalidDecision,
                "invalid approval feedback",
            ));
        }
        let plan_answer = params.plan;
        if plan_answer.is_some()
            && (params.decision.is_some() || params.answer.is_some() || params.feedback.is_some())
        {
            return Err(CoreError::new(
                ErrorCode::InvalidDecision,
                "answer must match the request type",
            ));
        }
        let resolution = if let Some(answer) = plan_answer {
            if !matches!(request.kind, RequestKind::PlanApproval(_))
                || !state.journal.snapshot.settings.plan_mode
                || answer.decision == PlanDecision::Unknown
                || answer.approval_policy == PermissionMode::Unknown
                || (answer.feedback.is_some()
                    && (answer.decision != PlanDecision::KeepPlanning
                        || answer
                            .feedback
                            .as_ref()
                            .is_some_and(|text| text.trim().is_empty())))
            {
                return Err(CoreError::new(
                    ErrorCode::InvalidDecision,
                    "invalid plan answer",
                ));
            }
            ApprovalResolution::Plan { answer }
        } else {
            match (&request.kind, params.decision, &params.answer) {
                (
                    RequestKind::Approval(approval) | RequestKind::SandboxApproval(approval),
                    Some(decision),
                    None,
                ) => {
                    if decision == ApprovalDecision::Unknown
                        || !approval.available_decisions.contains(&decision)
                    {
                        return Err(CoreError::new(
                            ErrorCode::InvalidDecision,
                            "invalid approval decision",
                        ));
                    }
                    ApprovalResolution::Answered {
                        decision,
                        feedback: params.feedback,
                    }
                }
                (RequestKind::UserInput(question), None, Some(answer)) => {
                    let valid = match answer {
                        UserInputAnswer::Option { label } => {
                            !label.trim().is_empty()
                                && question.options.iter().any(|option| option.label == *label)
                        }
                        UserInputAnswer::Text { text } => {
                            !text.trim().is_empty() && !text.contains(['\n', '\r'])
                        }
                        UserInputAnswer::Unknown => false,
                    };
                    if !valid {
                        return Err(CoreError::new(
                            ErrorCode::InvalidRequest,
                            "invalid user input answer",
                        ));
                    }
                    ApprovalResolution::UserInput {
                        answer: answer.clone(),
                    }
                }
                _ => {
                    return Err(CoreError::new(
                        ErrorCode::InvalidRequest,
                        "answer must match the request type",
                    ));
                }
            }
        };
        if let ApprovalResolution::Plan { answer } = &resolution
            && answer.decision != PlanDecision::KeepPlanning
        {
            // The accepted decision and mode transition share the session lock.
            let mut settings = state.journal.snapshot.settings.clone();
            settings.plan_mode = false;
            settings.pre_plan_approval_policy = None;
            settings.approval_policy = answer.approval_policy;
            state.plan_mode_notice(crate::plan_mode::APPROVED).await?;
            state
                .journal
                .append(vec![EventPayload::SessionUpdated { settings }])
                .await?;
            self.shared.runtime().tools.configure_shell(
                state.approval_policy(),
                &state.journal.snapshot.settings.additional_directories,
            );
            if answer.decision == PlanDecision::ClearContext {
                state
                    .active
                    .as_mut()
                    .expect("plan request has an active turn")
                    .plan_handoff = true;
            }
        }
        let saved_rules = if let Some(prefixes) = amendment {
            Some(state.gate.save_rules(&prefixes).map_err(|_| {
                CoreError::new(
                    ErrorCode::InvalidRequest,
                    "Could not save command rules; the command is still awaiting approval.",
                )
            })?)
        } else {
            None
        };
        let mut payloads = vec![
            EventPayload::RequestResolved {
                request_id: request_id.into(),
                resolution: resolution.clone(),
                client_name: Some(params.client_name),
            },
            EventPayload::SessionStatus {
                status: SessionStatus::Working,
                close_reason: None,
            },
        ];
        if let Some(path) = saved_rules {
            payloads.push(EventPayload::Warning {
                message: format!("Saved command allow rules to {}.", path.display()),
            });
        }
        let events = state.journal.append(payloads).await?;
        state.resolved.insert(request_id.into(), events[0].seq);
        if let Some(sender) = state.requests.remove(request_id) {
            let _ = sender.send(resolution);
        }
        if params.decision == Some(ApprovalDecision::Cancel)
            && let Some(active) = &state.active
        {
            active.cancel.cancel();
        }
        Ok(RequestResolveResponse {
            event_seq: events[0].seq,
            already_resolved: false,
        })
    }

    pub async fn update(
        &self,
        params: SessionUpdateParams,
    ) -> Result<SessionUpdateResponse, CoreError> {
        let session = self.clone();
        owned(async move { session.update_inner(params).await }).await
    }
    async fn update_inner(
        &self,
        params: SessionUpdateParams,
    ) -> Result<SessionUpdateResponse, CoreError> {
        let mut state = self.shared.state.lock().await;
        state.available()?;
        // Automatic names and their undo must not overwrite a manual rename
        // that arrived after the terminal's snapshot.
        if params.expected_name.as_ref().is_some_and(|expected| {
            state
                .journal
                .snapshot
                .settings
                .name
                .as_deref()
                .unwrap_or("")
                != expected
        }) {
            return Ok(SessionUpdateResponse {
                event_seq: state.journal.snapshot.last_event_seq,
            });
        }
        if params.approval_policy == Some(PermissionMode::Unknown)
            || params.reasoning_effort == Some(ReasoningEffort::Unknown)
            || params
                .model
                .as_ref()
                .is_some_and(|model| model.trim().is_empty())
        {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "invalid session settings",
            ));
        }
        let mut settings = state.journal.snapshot.settings.clone();
        settings.additional_directories = crate::directories::append(
            settings.cwd.clone(),
            state.directory_home.clone(),
            settings.additional_directories,
            params.add_directories,
        )
        .await?;
        let directories_changed = settings.additional_directories
            != state.journal.snapshot.settings.additional_directories;
        if let Some(plan_mode) = params.plan_mode {
            state.set_plan_mode(&mut settings, plan_mode).await?;
        }
        if let Some(name) = params.name {
            settings.name = (!name.is_empty()).then_some(name);
        }
        if let Some(model) = params.model {
            settings.model = model;
        }
        if let Some(policy) = params.approval_policy {
            settings.approval_policy = policy;
            if settings.plan_mode && params.plan_mode.is_none() {
                settings.pre_plan_approval_policy = Some(policy);
            }
        }
        if let Some(effort) = params.reasoning_effort {
            settings.reasoning_effort = effort;
        } else if settings.reasoning_effort == ReasoningEffort::Unknown {
            settings.reasoning_effort = ReasoningEffort::High;
        }
        let events = state
            .journal
            .append(vec![EventPayload::SessionUpdated { settings }])
            .await?;
        self.shared.runtime().tools.configure_shell(
            state.approval_policy(),
            &state.journal.snapshot.settings.additional_directories,
        );
        if directories_changed {
            let directories = state
                .journal
                .snapshot
                .settings
                .additional_directories
                .clone();
            state.gate.set_additional_directories(&directories);
            state.restate_directories().await?;
        }
        if let Some(lsp) = self.shared.runtime().tools.lsp() {
            lsp.configure(
                state
                    .journal
                    .snapshot
                    .settings
                    .additional_directories
                    .iter()
                    .map(std::path::PathBuf::from)
                    .collect(),
                if state.journal.snapshot.settings.plan_mode {
                    canter_tools::lsp::Round::Plan
                } else if state
                    .active
                    .as_ref()
                    .is_some_and(|active| active.turn.review.is_some())
                {
                    canter_tools::lsp::Round::Review
                } else {
                    canter_tools::lsp::Round::Modify
                },
            );
        }
        Ok(SessionUpdateResponse {
            event_seq: events[0].seq,
        })
    }

    /// A manual compaction is a standalone turn; reject it while another turn runs.
    pub async fn compact(&self) -> Result<SessionCompactResponse, CoreError> {
        let session = self.clone();
        owned(async move { session.compact_inner().await }).await
    }
    async fn compact_inner(&self) -> Result<SessionCompactResponse, CoreError> {
        let mut state = self.shared.state.lock().await;
        state.available()?;
        if state.active.is_some() {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "cannot compact while a turn is active",
            ));
        }
        let (response, done) = self
            .shared
            .start(
                &mut state,
                SessionInputParams {
                    non_interactive: false,
                    approval_policy: None,
                    review: None,
                    input: Vec::new(),
                    mode: InputMode::Queue,
                    client_name: "compact".into(),
                },
                None,
                Vec::new(),
            )
            .await?;
        state
            .active
            .as_mut()
            .expect("started compaction")
            .manual_compaction = true;
        let shared = self.shared.clone();
        tokio::spawn(async move {
            runner::run(shared, done).await;
        });
        Ok(SessionCompactResponse {
            event_seq: response.event_seq,
            turn_id: response.turn_id,
        })
    }

    /// An explicit client close. Returns Conflict if shutdown already closed the session.
    pub async fn close(&self) -> Result<SessionCloseResponse, CoreError> {
        self.close_with_reason(SessionCloseReason::ClientRequest)
            .await
    }

    /// Close for server shutdown so watching clients can resume after restart.
    pub async fn close_for_shutdown(&self) -> Result<SessionCloseResponse, CoreError> {
        self.close_with_reason(SessionCloseReason::ServerShutdown)
            .await
    }

    /// Atomically exclude clients and new work before the server moves this
    /// session. The server must hold its registry lock through the move.
    pub async fn close_for_trash(&self) -> Result<SessionCloseResponse, CoreError> {
        let session = self.clone();
        owned(async move {
            session
                .close_inner(SessionCloseReason::ClientRequest, true)
                .await
        })
        .await
    }

    async fn close_with_reason(
        &self,
        reason: SessionCloseReason,
    ) -> Result<SessionCloseResponse, CoreError> {
        let session = self.clone();
        owned(async move { session.close_inner(reason, false).await }).await
    }
    pub(crate) fn check_trashable(&self, state: &State) -> Result<(), CoreError> {
        state.available()?;
        if self.shared.clients.lock().unwrap().is_attached() {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot delete this session while a terminal is attached; leave it in every terminal first.",
            ));
        }
        // Unlike SessionEnd(client_exit), deletion must preserve a
        // suspended turn: waiting for approval/an answer is still work.
        if state.journal.snapshot.status == SessionStatus::WaitingApproval
            || !state.requests.is_empty()
        {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot delete this session while it is waiting for approval or an answer.",
            ));
        }
        if state.active.is_some() || !state.queue.is_empty() || state.btw.is_some() {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot delete this session while a turn, queued input, or side question is still running.",
            ));
        }
        if state.goal_deadline.is_some() {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot delete this session while a goal is scheduled to continue; pause the goal first.",
            ));
        }
        // Closing would kill every running command, including user
        // shell commands that never wake the model.
        if state
            .journal
            .snapshot
            .background_commands
            .iter()
            .any(|command| command.running)
        {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot delete this session while a background command is still running; open it and run /stop first.",
            ));
        }
        if !state.automatic_turns_paused
            && state
                .background_events
                .has_pending_wake(self.shared.next_options().auto_wake_limit)
        {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot delete this session while a background command has a pending wake.",
            ));
        }
        Ok(())
    }

    async fn close_inner(
        &self,
        reason: SessionCloseReason,
        for_trash: bool,
    ) -> Result<SessionCloseResponse, CoreError> {
        let (done, btw_done, children) = {
            let mut state = self.shared.state.lock().await;
            if for_trash {
                self.check_trashable(&state)?;
            }
            state.closing = true;
            if let Some(turn) = state.active.as_ref().map(|active| active.turn.id.clone()) {
                state.suppressed_agent_turns.insert(turn);
            }
            let done = state.active.as_ref().map(|active| {
                active.cancel.cancel();
                active.done.clone()
            });
            let btw_done = state.btw.as_ref().map(|btw| {
                btw.cancel.cancel();
                btw.done.clone()
            });
            let children = state.interrupt_agents().await;
            (done, btw_done, children)
        };
        crate::agents::finish_agent_stops(children).await?;
        let mut stopped = if let Some(done) = done {
            wait_done(done).await.map(|_| ())
        } else {
            Ok(())
        };
        if let Some(done) = btw_done {
            stopped = stopped.and(wait_done(done).await.map(|_| ()));
        }
        self.shared.runtime().tools.background().stop_all().await;
        self.shared.runtime().tools.close_shell();
        if let Some(lsp) = self.shared.runtime().tools.lsp() {
            lsp.close().await;
        }
        if !self.shared.state.lock().await.journal.failed {
            self.shared.sync_background().await?;
        }
        let mut state = self.shared.state.lock().await;
        if stopped.is_err() || state.journal.failed {
            state.journal.release().await?;
            return Err(CoreError::internal());
        }
        if !state.hook_session_ended && state.journal.snapshot.status != SessionStatus::Closed {
            state.hook_session_ended = true;
            crate::hooks::notify(
                &self.shared,
                &state,
                crate::hooks::Event::SessionEnd,
                serde_json::json!({"reason": if reason == SessionCloseReason::ServerShutdown { "server_shutdown" } else { "client_request" }}),
                self.shared.lifetime.child_token(),
            );
        }
        // A previous client_exit may still be running, along with later turn
        // notifications. Closing always drains them, even without a new End.
        drop(state);
        crate::hooks::drain(&self.shared).await;
        state = self.shared.state.lock().await;
        if reason == SessionCloseReason::ClientRequest
            && state.journal.snapshot.status == SessionStatus::Closed
            && state.journal.snapshot.close_reason != Some(SessionCloseReason::ClientRequest)
        {
            return Err(CoreError::new(ErrorCode::Conflict, "server is stopping"));
        }
        if state.journal.snapshot.status != SessionStatus::Closed {
            state.discard_queue(InputRemovalReason::Closed).await?;
            state
                .journal
                .append(vec![EventPayload::SessionStatus {
                    status: SessionStatus::Closed,
                    close_reason: Some(reason),
                }])
                .await?;
            state.journal.release().await?;
        }
        Ok(SessionCloseResponse {
            event_seq: state.journal.snapshot.last_event_seq,
        })
    }
}

// A disconnected HTTP caller may drop its future. Once accepted, a mutation
// must finish its disk + in-memory transition, including any tool cleanup.
pub(crate) async fn owned<T: Send + 'static>(
    operation: impl std::future::Future<Output = Result<T, CoreError>> + Send + 'static,
) -> Result<T, CoreError> {
    tokio::spawn(operation)
        .await
        .map_err(|_| CoreError::internal())?
}
async fn executor(
    cwd: String,
    session_directory: std::path::PathBuf,
    runtime: RuntimeContext,
    options: &CoreOptions,
) -> Result<Tools, CoreError> {
    let options = options.clone();
    let lsp = runtime
        .lsp
        .as_ref()
        .filter(|_| !runtime.restricted_mode)
        .map(|service| {
            service.session_with_access(
                canter_tools::lsp::FileContext {
                    cwd: cwd.clone().into(),
                    additional_directories: Vec::new(),
                    environment: runtime.environment.clone(),
                    round: canter_tools::lsp::Round::Modify,
                },
                runtime.protected_paths.clone(),
                runtime.lsp_file_access.clone(),
            )
        });
    tokio::task::spawn_blocking(move || {
        let memory = runtime.memory_home.map(|home| {
            let memory = Arc::new(canter_tools::memory::Memory::new(home, cwd.clone().into()));
            if let Err(error) = memory.set_enabled(!runtime.restricted_mode) {
                memory.disable_with_warning(error.to_string());
            }
            memory
        });
        let scratch = Arc::new(canter_tools::ScratchDirectory::for_session(
            &session_directory,
        ));
        LocalExecutor::with_scratch_directory(
            cwd.into(),
            runtime.environment,
            runtime.protected_paths,
            scratch,
        )
        .map(|executor| {
            let tools = Tools::new(Arc::new(
                executor.with_memory(memory).with_sandbox(options.sandbox),
            ))
            .with_telemetry(options.telemetry)
            .with_plan_enabled(options.plan_enabled)
            .with_web_search(options.web_search)
            .with_mcp(runtime.mcp_tools)
            .with_mcp_tool_search(options.mcp_tool_search)
            .with_web_fetch_timeout(options.web_fetch_timeout);
            match lsp {
                Some(lsp) => tools.with_lsp(lsp),
                None => tools,
            }
        })
        .map_err(|_| CoreError::new(ErrorCode::InvalidRequest, "invalid execution context"))
    })
    .await
    .map_err(|_| CoreError::internal())?
}

pub(crate) type Done = watch::Sender<Option<Result<u64, CoreError>>>;
pub(crate) async fn wait_done(
    mut done: watch::Receiver<Option<Result<u64, CoreError>>>,
) -> Result<u64, CoreError> {
    loop {
        if let Some(result) = done.borrow().clone() {
            return result;
        }
        done.changed().await.map_err(|_| CoreError::internal())?;
    }
}
impl Shared {
    /// Serializes the first actual model output with the retract decision.
    /// A losing stream must discard its buffered output before writing anything.
    pub async fn accept_model_output(&self) -> bool {
        let mut state = self.state.lock().await;
        let active = state.active.as_mut().expect("active turn");
        if active.retracted {
            return false;
        }
        active.model_started = true;
        true
    }

    pub async fn start(
        &self,
        state: &mut State,
        params: SessionInputParams,
        input_id: Option<String>,
        skills: Vec<crate::skill_invocation::Snapshot>,
    ) -> Result<(SessionInputResponse, Done), CoreError> {
        let configuration_lease = self.configure(state).await?;
        if params
            .input
            .iter()
            .any(|input| matches!(input, UserInput::LocalImage { .. }))
            && !self
                .runtime()
                .options
                .image_models
                .contains(&state.journal.snapshot.settings.model)
        {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                &format!(
                    "{} does not accept images",
                    state.journal.snapshot.settings.model
                ),
            ));
        }
        let check_prompt = !params.input.is_empty()
            && !self.runtime().context.restricted_mode
            && self
                .runtime()
                .options
                .hooks
                .has(crate::hooks::Event::UserPromptSubmit);
        let turn = Turn {
            review: params.review,
            id: id("turn"),
            items: Vec::new(),
            status: TurnStatus::InProgress,
            error: None,
            input: if check_prompt {
                Vec::new()
            } else {
                params.input.clone()
            },
            client_name: params.client_name,
            usage: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
        };
        state
            .journal
            .record(HistoryEntry::TurnBoundary {
                turn_id: turn.id.clone(),
            })
            .await?;
        state.rewind.boundary(
            turn.id.clone(),
            state.history.len(),
            state.user_messages.len(),
            state.invoked_skills.len(),
        );
        if !params.input.is_empty() && !check_prompt {
            state
                .user_input(&params.input, &turn.id, skills.clone())
                .await?;
        }
        let mut payloads = vec![
            EventPayload::TurnStarted {
                turn: turn.clone(),
                input_id,
            },
            EventPayload::SessionStatus {
                status: SessionStatus::Working,
                close_reason: None,
            },
        ];
        if !params.input.is_empty() && !check_prompt {
            payloads.push(EventPayload::ItemCompleted {
                turn_id: turn.id.clone(),
                item: Item::UserMessage {
                    id: id("user"),
                    content: params.input.clone(),
                },
            });
        }
        let events = state.journal.append(payloads).await?;
        let (done, receiver) = watch::channel(None);
        state.active = Some(Active {
            pending_prompt: check_prompt.then_some((params.input, skills)),
            prompt_blocked: false,
            configuration_lease,
            non_interactive: params.non_interactive,
            approval_policy: params.approval_policy,
            goal_continuation: false,
            auto_wake: false,
            plan_handoff: false,
            turn: turn.clone(),
            manual_compaction: false,
            user_shell: None,
            model_started: false,
            retracted: false,
            cancel: self.lifetime.child_token(),
            done: receiver,
        });
        state.automatic_turns_paused = params.non_interactive;
        self.runtime().tools.configure_shell(
            state.approval_policy(),
            &state.journal.snapshot.settings.additional_directories,
        );
        Ok((
            SessionInputResponse {
                event_seq: events[0].seq,
                turn_id: Some(turn.id),
            },
            done,
        ))
    }
}
struct InputWaiting(Arc<Shared>);
impl Drop for InputWaiting {
    fn drop(&mut self) {
        self.0.input_waiters.fetch_sub(1, Ordering::SeqCst);
    }
}
fn validate_options(options: &CoreOptions) -> Result<(), CoreError> {
    if options.max_output_tokens == 0
        || options.context_window
            <= u64::from(options.max_output_tokens) + crate::compaction::RESERVE
    {
        return Err(CoreError::new(
            ErrorCode::InvalidRequest,
            "context window must exceed output limit plus 16384 tokens",
        ));
    }
    Ok(())
}

#[cfg(test)]
#[path = "decline_feedback_tests.rs"]
mod decline_feedback_tests;

pub(crate) fn interrupted_turn(mut turn: Turn) -> Vec<EventPayload> {
    let mut payloads = Vec::new();
    for mut item in std::mem::take(&mut turn.items) {
        match &mut item {
            Item::PlanProposal { status, .. }
            | Item::Plan { status, .. }
            | Item::McpToolCall { status, .. }
            | Item::McpToolSearch { status, .. }
            | Item::WebFetch { status, .. }
            | Item::WebSearch { status, .. }
            | Item::CommandExecution { status, .. }
            | Item::FileChange { status, .. }
            | Item::FileRead { status, .. }
            | Item::UserInput { status, .. }
            | Item::CollabAgentToolCall { status, .. }
                if *status == ItemStatus::InProgress =>
            {
                *status = ItemStatus::Failed;
                payloads.push(EventPayload::ItemCompleted {
                    turn_id: turn.id.clone(),
                    item,
                });
            }
            _ => {}
        }
    }
    turn.status = TurnStatus::Interrupted;
    // Replay fills the times from event timestamps; keep them out of the event.
    turn.started_at = None;
    turn.completed_at = None;
    turn.duration_ms = None;
    payloads.push(EventPayload::TurnCompleted { turn });
    payloads
}
