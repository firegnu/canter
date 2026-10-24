use crate::{
    CoreError, CoreOptions, RuntimeContext, Subscription,
    context::HistoryEntry,
    journal::{Journal, now},
    runner,
};
use canter_model::{AssistantContent, Message, Model};
use canter_protocol::*;
use canter_store::Store;
use canter_tools::{LocalExecutor, PermissionGate, Tools};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
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
    pub state: Mutex<State>,
    pub model: Arc<dyn Model>,
    pub tools: Tools,
    pub options: CoreOptions,
    pub lifetime: CancellationToken,
}
pub(crate) struct State {
    pub journal: Journal,
    pub prefix: Vec<Message>,
    pub history: Vec<Message>,
    pub user_messages: Vec<String>,
    pub gate: PermissionGate,
    pub active: Option<Active>,
    pub queue: VecDeque<QueuedInput>,
    pub steers: Vec<QueuedInput>,
    pub approvals: HashMap<String, oneshot::Sender<ApprovalDecision>>,
    pub resolved: HashMap<String, u64>,
    pub closing: bool,
    pub measured_context: Option<(u64, u64)>,
}
pub(crate) struct Active {
    pub turn: Turn,
    pub cancel: CancellationToken,
    pub done: watch::Receiver<Option<Result<u64, CoreError>>>,
}
impl State {
    fn new(journal: Journal) -> Self {
        Self {
            journal,
            prefix: Vec::new(),
            history: Vec::new(),
            user_messages: Vec::new(),
            gate: PermissionGate::default(),
            active: None,
            queue: VecDeque::new(),
            steers: Vec::new(),
            approvals: HashMap::new(),
            resolved: HashMap::new(),
            closing: false,
            measured_context: None,
        }
    }
    pub async fn message(&mut self, message: Message) -> Result<(), CoreError> {
        self.journal
            .record(HistoryEntry::Message {
                message: message.clone(),
                user_input: false,
            })
            .await?;
        self.history.push(message);
        Ok(())
    }
    async fn user_input(&mut self, text: String) -> Result<(), CoreError> {
        let message = Message::User { text: text.clone() };
        self.journal
            .record(HistoryEntry::Message {
                message: message.clone(),
                user_input: true,
            })
            .await?;
        self.history.push(message);
        self.user_messages.push(text);
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
        Ok(())
    }
    pub async fn consume_steering(&mut self, turn_id: &str) -> Result<(), CoreError> {
        let inputs = self.steers.clone();
        if inputs.is_empty() {
            return Ok(());
        }
        let mut payloads = Vec::new();
        for input in &inputs {
            self.user_input(input_text(&input.input)).await?;
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
                self.approvals.remove(&request_id);
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
        let tools = executor(writer.snapshot().settings.cwd.clone(), runtime).await?;
        let mut state = State::new(Journal::new(store, writer));
        (state.prefix, state.history, state.user_messages) = crate::context::restore(entries);
        for event in events {
            if let EventPayload::RequestResolved { request_id, .. } = event.payload {
                state.resolved.insert(request_id, event.seq);
            }
        }
        state
            .complete_missing_tools("没有结果：服务端停止，本轮被打断")
            .await?;
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
                    text: "[服务端停止，上一轮被打断；请根据已有结果继续。]".into(),
                })
                .await?;
        }
        let mut payloads = Vec::new();
        for mut turn in unfinished {
            for mut item in std::mem::take(&mut turn.items) {
                match &mut item {
                    Item::CommandExecution { status, .. }
                    | Item::FileChange { status, .. }
                    | Item::FileRead { status, .. }
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
            payloads.push(EventPayload::TurnCompleted { turn });
        }
        state.discard_queue(InputRemovalReason::Interrupted).await?;
        payloads.push(EventPayload::SessionStatus {
            status: SessionStatus::Idle,
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
        settings: SessionSettings,
        runtime: RuntimeContext,
        model: Arc<dyn Model>,
        options: CoreOptions,
    ) -> Result<(Self, SessionCreateResponse), CoreError> {
        validate_options(&options)?;
        if settings.model.trim().is_empty() || settings.approval_policy == PermissionMode::Unknown {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "invalid session settings",
            ));
        }
        let cwd = settings.cwd.clone();
        let tools = executor(cwd, runtime).await?;
        let owned_store = store.clone();
        let writer = tokio::task::spawn_blocking(move || owned_store.create(&id, settings, now()))
            .await
            .map_err(|_| CoreError::internal())??;
        let mut journal = Journal::new(store, writer);
        journal
            .append(vec![EventPayload::SessionStatus {
                status: SessionStatus::Idle,
            }])
            .await?;
        let response = SessionCreateResponse {
            session: journal.snapshot.clone(),
            event_seq: journal.snapshot.last_event_seq,
        };
        Ok((
            Self::from_state(State::new(journal), tools, model, options),
            response,
        ))
    }

    fn from_state(state: State, tools: Tools, model: Arc<dyn Model>, options: CoreOptions) -> Self {
        let lifetime = CancellationToken::new();
        let shared = Arc::new(Shared {
            state: Mutex::new(state),
            model,
            tools,
            options,
            lifetime: lifetime.clone(),
        });
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
        owned(async move { session.input_inner(params).await }).await
    }
    async fn input_inner(
        &self,
        params: SessionInputParams,
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
                "input must contain supported text",
            ));
        }
        let mut state = self.shared.state.lock().await;
        state.available()?;
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.cancel.is_cancelled())
        {
            return Err(CoreError::new(ErrorCode::Conflict, "turn is stopping"));
        }
        if state.active.is_some() {
            let input = QueuedInput {
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
            let turn_id = if params.mode == InputMode::Steer {
                state.steers.push(input);
                state.active.as_ref().map(|active| active.turn.id.clone())
            } else {
                state.queue.push_back(input);
                None
            };
            return Ok(SessionInputResponse {
                event_seq: events[0].seq,
                turn_id,
            });
        }
        let (response, done) = self.shared.start(&mut state, params, None).await?;
        let shared = self.shared.clone();
        tokio::spawn(async move {
            runner::run(shared, done, false).await;
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
        let done = {
            let state = self.shared.state.lock().await;
            state.available()?;
            let active = state
                .active
                .as_ref()
                .filter(|active| active.turn.id == params.turn_id)
                .ok_or_else(|| CoreError::new(ErrorCode::TurnMismatch, "turn is not active"))?;
            active.cancel.cancel();
            active.done.clone()
        };
        Ok(SessionInterruptResponse {
            event_seq: wait_done(done).await?,
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
            .ok_or_else(|| {
                CoreError::new(ErrorCode::RequestNotFound, "approval request not found")
            })?;
        if !matches!(&request.kind, RequestKind::Approval(approval) if params.decision != ApprovalDecision::Unknown && approval.available_decisions.contains(&params.decision))
        {
            return Err(CoreError::new(
                ErrorCode::InvalidDecision,
                "invalid approval decision",
            ));
        }
        let events = state
            .journal
            .append(vec![
                EventPayload::RequestResolved {
                    request_id: request_id.into(),
                    resolution: ApprovalResolution::Answered {
                        decision: params.decision,
                    },
                    client_name: Some(params.client_name),
                },
                EventPayload::SessionStatus {
                    status: SessionStatus::Working,
                },
            ])
            .await?;
        state.resolved.insert(request_id.into(), events[0].seq);
        if let Some(sender) = state.approvals.remove(request_id) {
            let _ = sender.send(params.decision);
        }
        if params.decision == ApprovalDecision::Cancel
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
        if let Some(name) = params.name {
            settings.name = Some(name);
        }
        if let Some(model) = params.model {
            settings.model = model;
        }
        if let Some(policy) = params.approval_policy {
            settings.approval_policy = policy;
        }
        let events = state
            .journal
            .append(vec![EventPayload::SessionUpdated { settings }])
            .await?;
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
                    input: Vec::new(),
                    mode: InputMode::Queue,
                    client_name: "compact".into(),
                },
                None,
            )
            .await?;
        let receiver = state
            .active
            .as_ref()
            .expect("started compaction")
            .done
            .clone();
        let shared = self.shared.clone();
        tokio::spawn(async move {
            runner::run(shared, done, true).await;
        });
        drop(state);
        let event_seq = wait_done(receiver).await?;
        let state = self.shared.state.lock().await;
        let turn = state
            .journal
            .snapshot
            .turns
            .iter()
            .find(|turn| Some(&turn.id) == response.turn_id.as_ref())
            .expect("compaction turn");
        if turn.status != TurnStatus::Completed {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "compaction did not complete",
            ));
        }
        Ok(SessionCompactResponse { event_seq })
    }

    pub async fn close(&self) -> Result<SessionCloseResponse, CoreError> {
        let session = self.clone();
        owned(async move { session.close_inner().await }).await
    }
    async fn close_inner(&self) -> Result<SessionCloseResponse, CoreError> {
        let done = {
            let mut state = self.shared.state.lock().await;
            state.closing = true;
            state.active.as_ref().map(|active| {
                active.cancel.cancel();
                active.done.clone()
            })
        };
        let stopped = if let Some(done) = done {
            wait_done(done).await.map(|_| ())
        } else {
            Ok(())
        };
        let mut state = self.shared.state.lock().await;
        if stopped.is_err() || state.journal.failed {
            state.journal.release().await?;
            return Err(CoreError::internal());
        }
        if state.journal.snapshot.status != SessionStatus::Closed {
            state.discard_queue(InputRemovalReason::Closed).await?;
            state
                .journal
                .append(vec![EventPayload::SessionStatus {
                    status: SessionStatus::Closed,
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
async fn owned<T: Send + 'static>(
    operation: impl std::future::Future<Output = Result<T, CoreError>> + Send + 'static,
) -> Result<T, CoreError> {
    tokio::spawn(operation)
        .await
        .map_err(|_| CoreError::internal())?
}
async fn executor(cwd: String, runtime: RuntimeContext) -> Result<Tools, CoreError> {
    tokio::task::spawn_blocking(move || {
        LocalExecutor::new(cwd.into(), runtime.environment, runtime.protected_paths)
            .map(|executor| Tools::new(Arc::new(executor)))
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
    pub async fn start(
        &self,
        state: &mut State,
        params: SessionInputParams,
        input_id: Option<String>,
    ) -> Result<(SessionInputResponse, Done), CoreError> {
        let turn = Turn {
            id: id("turn"),
            items: Vec::new(),
            status: TurnStatus::InProgress,
            error: None,
            input: params.input.clone(),
            client_name: params.client_name,
            usage: None,
        };
        let text = input_text(&params.input);
        if !params.input.is_empty() {
            state.user_input(text).await?;
        }
        let mut payloads = vec![
            EventPayload::TurnStarted {
                turn: turn.clone(),
                input_id,
            },
            EventPayload::SessionStatus {
                status: SessionStatus::Working,
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
            turn: turn.clone(),
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
fn input_text(input: &[UserInput]) -> String {
    input
        .iter()
        .filter_map(|input| match input {
            UserInput::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
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
