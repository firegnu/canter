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
    pub input_waiters: AtomicUsize,
    pub state: Mutex<State>,
    pub model: Arc<dyn Model>,
    pub tools: Tools,
    pub options: CoreOptions,
    pub lifetime: CancellationToken,
}
pub(crate) struct State {
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
    pub measured_context: Option<(u64, u64)>,
    pub rewind: crate::rewind::Checkpoints,
    plan_tool_calls: u8,
}
pub(crate) struct Active {
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
    fn new(journal: Journal) -> Self {
        Self {
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
            measured_context: None,
            rewind: Default::default(),
            plan_tool_calls: 0,
        }
    }
    async fn initialize_prefix(
        &mut self,
        home: Option<std::path::PathBuf>,
        canter_home: Option<std::path::PathBuf>,
        tools: &Tools,
    ) -> Result<(), CoreError> {
        self.directory_home = home.clone();
        self.gate
            .set_additional_directories(&self.journal.snapshot.settings.additional_directories);
        self.tool_definitions = canter_tools::Tool::ALL
            .into_iter()
            .filter(|tool| tools.find(tool.name()).is_some())
            .map(|tool| {
                let def = tool.definition();
                canter_model::ToolDefinition {
                    name: def.name.into(),
                    description: def.description.into(),
                    parameters: def.parameters,
                }
            })
            .chain(crate::goal::definitions())
            .chain(
                tools
                    .mcp_tools()
                    .iter()
                    .map(|tool| canter_model::ToolDefinition {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        parameters: tool.parameters.clone(),
                    }),
            )
            .collect();
        let (mut segments, warnings, skills) = crate::context::prefix(
            self.journal.snapshot.settings.cwd.clone(),
            home,
            canter_home,
            self.tool_definitions.clone(),
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
        let (prefix, sections) = crate::prompt::chat_completions(&segments);
        self.skills = skills;
        self.prefix_sections = sections;
        if self.prefix != prefix || self.prefix_segments.as_ref() != Some(&segments) {
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
        self.message(Message::ToolResult {
            tool_call_id: call.id.clone(),
            text,
        })
        .await
    }
    async fn user_input(
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
    pub async fn consume_steering(&mut self, turn_id: &str) -> Result<(), CoreError> {
        let inputs = self.steers.clone();
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
        self.steers.clear();
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
    pub async fn open(
        store: Arc<Store>,
        id: String,
        runtime: RuntimeContext,
        model: Arc<dyn Model>,
        options: CoreOptions,
    ) -> Result<(Self, SessionReadResponse), CoreError> {
        owned(Self::open_inner(store, id, runtime, model, options)).await
    }
    async fn open_inner(
        store: Arc<Store>,
        id: String,
        runtime: RuntimeContext,
        model: Arc<dyn Model>,
        options: CoreOptions,
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
        let home = runtime
            .environment
            .get("HOME")
            .map(std::path::PathBuf::from);
        let canter_home = crate::context::canter_home(&runtime.environment);
        let tools = executor(
            writer.snapshot().settings.cwd.clone(),
            runtime,
            options.web_fetch_timeout,
            options.plan_enabled,
        )
        .await?;
        let mut state = State::new(Journal::new(store, writer));
        crate::usage::Ledger::replay(&events, &options.prices)
            .fill_costs(&mut state.journal.snapshot);
        let restored = crate::context::restore(entries.clone(), &mut state.rewind);
        state.prefix = restored.prefix;
        state.invoked_skills = restored.invoked_skills;
        state.prefix_segments = restored.segments;
        state.history = restored.history;
        state.plan_mode_notices = restored.plan_mode_notices;
        state.tool_images = restored.tool_images;
        state.user_messages = restored.users;
        state.summary_index = restored.summary_index;
        state.background_events.all = restored.background_cards;
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
        state.initialize_prefix(home, canter_home, &tools).await?;
        if state.journal.snapshot.settings.plan_enabled != Some(options.plan_enabled) {
            let mut settings = state.journal.snapshot.settings.clone();
            settings.plan_enabled = Some(options.plan_enabled);
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
        for mut turn in unfinished {
            for mut item in std::mem::take(&mut turn.items) {
                match &mut item {
                    Item::PlanProposal { status, .. }
                    | Item::Plan { status, .. }
                    | Item::McpToolCall { status, .. }
                    | Item::WebFetch { status, .. }
                    | Item::CommandExecution { status, .. }
                    | Item::FileChange { status, .. }
                    | Item::FileRead { status, .. }
                    | Item::UserInput { status, .. }
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
        Ok((Self::from_state(state, tools, model, options), response))
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
            store, id, settings, runtime, model, options,
        ))
        .await
    }
    async fn create_inner(
        store: Arc<Store>,
        id: String,
        mut settings: SessionSettings,
        runtime: RuntimeContext,
        model: Arc<dyn Model>,
        options: CoreOptions,
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
        let cwd = settings.cwd.clone();
        let home = runtime
            .environment
            .get("HOME")
            .map(std::path::PathBuf::from);
        let canter_home = crate::context::canter_home(&runtime.environment);
        let tools = executor(
            cwd,
            runtime,
            options.web_fetch_timeout,
            options.plan_enabled,
        )
        .await?;
        settings.additional_directories = crate::directories::append(
            settings.cwd.clone(),
            home.clone(),
            Vec::new(),
            settings.additional_directories,
        )
        .await?;
        settings.plan_enabled = Some(options.plan_enabled);
        let owned_store = store.clone();
        let writer = tokio::task::spawn_blocking(move || owned_store.create(&id, settings, now()))
            .await
            .map_err(|_| CoreError::internal())??;
        let mut journal = Journal::new(store, writer);
        journal
            .append(vec![EventPayload::SessionStatus {
                status: SessionStatus::Idle,
                close_reason: None,
            }])
            .await?;
        let mut state = State::new(journal);
        state.initialize_prefix(home, canter_home, &tools).await?;
        let response = SessionCreateResponse {
            session: state.journal.snapshot.clone(),
            event_seq: state.journal.snapshot.last_event_seq,
        };
        Ok((Self::from_state(state, tools, model, options), response))
    }

    fn from_state(
        mut state: State,
        tools: Tools,
        model: Arc<dyn Model>,
        options: CoreOptions,
    ) -> Self {
        if let Some(path) = &options.command_rules_path {
            state.gate = std::mem::take(&mut state.gate).with_rules_file(path.clone());
        }
        tools
            .background()
            .restore(&state.journal.snapshot.background_commands);
        let lifetime = CancellationToken::new();
        let shared = Arc::new(Shared {
            input_waiters: AtomicUsize::new(0),
            state: Mutex::new(state),
            model,
            tools,
            options,
            lifetime: lifetime.clone(),
        });
        crate::background::watch(&shared);
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
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.cancel.is_cancelled())
        {
            return Err(CoreError::new(ErrorCode::Conflict, "turn is stopping"));
        }
        let imported = if let Some(loaded) = loaded {
            let model = &state.journal.snapshot.settings.model;
            if !self.shared.options.image_models.contains(model) {
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
            store: state.journal.store.clone(),
            id: state.journal.snapshot.id.clone(),
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

    pub async fn snapshot(&self) -> SessionReadResponse {
        let state = self.shared.state.lock().await;
        SessionReadResponse {
            session: state.journal.snapshot.clone(),
            event_seq: state.journal.snapshot.last_event_seq,
        }
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
        let (done, retracted) = {
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
            result
        };
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
                (RequestKind::Approval(approval), Some(decision), None) => {
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

    async fn close_with_reason(
        &self,
        reason: SessionCloseReason,
    ) -> Result<SessionCloseResponse, CoreError> {
        let session = self.clone();
        owned(async move { session.close_inner(reason).await }).await
    }
    async fn close_inner(
        &self,
        reason: SessionCloseReason,
    ) -> Result<SessionCloseResponse, CoreError> {
        let (done, btw_done) = {
            let mut state = self.shared.state.lock().await;
            state.closing = true;
            let done = state.active.as_ref().map(|active| {
                active.cancel.cancel();
                active.done.clone()
            });
            let btw_done = state.btw.as_ref().map(|btw| {
                btw.cancel.cancel();
                btw.done.clone()
            });
            (done, btw_done)
        };
        let mut stopped = if let Some(done) = done {
            wait_done(done).await.map(|_| ())
        } else {
            Ok(())
        };
        if let Some(done) = btw_done {
            stopped = stopped.and(wait_done(done).await.map(|_| ()));
        }
        self.shared.tools.background().stop_all().await;
        if !self.shared.state.lock().await.journal.failed {
            self.shared.sync_background().await?;
        }
        let mut state = self.shared.state.lock().await;
        if stopped.is_err() || state.journal.failed {
            state.journal.release().await?;
            return Err(CoreError::internal());
        }
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
    runtime: RuntimeContext,
    web_fetch_timeout: std::time::Duration,
    plan_enabled: bool,
) -> Result<Tools, CoreError> {
    tokio::task::spawn_blocking(move || {
        LocalExecutor::new(cwd.into(), runtime.environment, runtime.protected_paths)
            .map(|executor| {
                Tools::new(Arc::new(executor))
                    .with_plan_enabled(plan_enabled)
                    .with_mcp(runtime.mcp_tools)
                    .with_web_fetch_timeout(web_fetch_timeout)
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
        let turn = Turn {
            review: params.review,
            id: id("turn"),
            items: Vec::new(),
            status: TurnStatus::InProgress,
            error: None,
            input: params.input.clone(),
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
        if !params.input.is_empty() {
            state.user_input(&params.input, &turn.id, skills).await?;
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
        if !params.input.is_empty() {
            payloads.push(EventPayload::ItemCompleted {
                turn_id: turn.id.clone(),
                item: Item::UserMessage {
                    id: id("user"),
                    content: params.input,
                },
            });
        }
        let events = state.journal.append(payloads).await?;
        let (done, receiver) = watch::channel(None);
        state.active = Some(Active {
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
