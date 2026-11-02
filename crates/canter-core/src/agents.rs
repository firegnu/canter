use crate::{CoreError, Session};
use canter_protocol::*;

pub(crate) struct AgentStop {
    shared: std::sync::Arc<crate::session::Shared>,
    done: Option<tokio::sync::watch::Receiver<Option<Result<u64, CoreError>>>>,
}

impl crate::session::State {
    async fn running_agents(&self, except: Option<&str>) -> usize {
        let mut count = 0;
        for child in self
            .agent_children
            .iter()
            .filter_map(std::sync::Weak::upgrade)
        {
            let state = child.state.lock().await;
            if except == Some(state.journal.snapshot.id.as_str()) {
                continue;
            }
            if state.active.is_some()
                || state
                    .journal
                    .snapshot
                    .agent
                    .as_ref()
                    .is_some_and(|agent| agent.status == AgentStatus::Running)
                || state
                    .journal
                    .snapshot
                    .background_commands
                    .iter()
                    .any(|command| command.running)
                || (!state.automatic_turns_paused
                    && state
                        .background_events
                        .has_pending_wake(child.next_options().auto_wake_limit))
            {
                count += 1;
            }
        }
        count
    }
    /// Called with the parent locked; only acquires descendant state locks.
    pub(crate) async fn interrupt_agents(&mut self) -> Vec<AgentStop> {
        let mut stops = Vec::new();
        for child in self
            .agent_children
            .iter()
            .filter_map(std::sync::Weak::upgrade)
        {
            let mut state = child.state.lock().await;
            let active = state.active.as_ref().map(|active| {
                active.cancel.cancel();
                (active.turn.id.clone(), active.done.clone())
            });
            if let Some((turn, _)) = &active {
                state.suppressed_agent_turns.insert(turn.clone());
            }
            state.automatic_turns_paused = true;
            state.background_events.interrupt();
            drop(state);
            stops.push(AgentStop {
                shared: child,
                done: active.map(|(_, done)| done),
            });
        }
        stops
    }
}

pub(crate) async fn finish_agent_stops(stops: Vec<AgentStop>) -> Result<(), CoreError> {
    let mut result = Ok(());
    for stop in stops {
        if let Some(done) = stop.done
            && let Err(error) = crate::session::wait_done(done).await
        {
            result = Err(error);
        }
        stop.shared.runtime().tools.background().stop_all().await;
        if let Err(error) = stop.shared.sync_background().await {
            result = Err(error);
        }
    }
    result
}

/// Startup recovery only, after acquiring the daemon lease. Does not open
/// runtimes or require saved environment variables.
pub fn recover_agent_sessions(store: &canter_store::Store) -> Result<(), CoreError> {
    for summary in store.list_all()?.into_iter().filter(|summary| {
        summary.parent.is_some() && summary.agent_status == Some(AgentStatus::Running)
    }) {
        if let Err(error) = recover_agent_session(store, &summary.id) {
            eprintln!(
                "Warning: could not recover sub-agent {}: {error}",
                summary.id
            );
        }
    }
    Ok(())
}

fn recover_agent_session(store: &canter_store::Store, id: &str) -> Result<(), CoreError> {
    let mut writer = store.open(id)?;
    if !writer
        .snapshot()
        .agent
        .as_ref()
        .is_some_and(|agent| agent.status == AgentStatus::Running)
    {
        return Ok(());
    }
    let snapshot = writer.snapshot().clone();
    let timestamp = crate::journal::now();
    for turn in snapshot
        .turns
        .into_iter()
        .filter(|turn| turn.status == TurnStatus::InProgress)
    {
        for event in crate::session::interrupted_turn(turn) {
            writer.append(event, timestamp)?;
        }
    }
    for request in snapshot.pending_requests {
        writer.append(
            EventPayload::RequestResolved {
                request_id: request.id,
                resolution: ApprovalResolution::Cancelled,
                client_name: None,
            },
            timestamp,
        )?;
    }
    if !snapshot.queued_inputs.is_empty() {
        writer.append(
            EventPayload::InputRemoved {
                input_ids: snapshot
                    .queued_inputs
                    .into_iter()
                    .map(|input| input.id)
                    .collect(),
                reason: InputRemovalReason::Interrupted,
                turn_id: None,
            },
            timestamp,
        )?;
    }
    for mut command in snapshot
        .background_commands
        .into_iter()
        .filter(|command| command.running)
    {
        command.running = false;
        command.completed_at = Some(timestamp);
        writer.append(
            EventPayload::BackgroundCommandUpdated { command },
            timestamp,
        )?;
    }
    writer.append(
        EventPayload::SessionStatus {
            status: SessionStatus::Closed,
            close_reason: Some(SessionCloseReason::ServerShutdown),
        },
        timestamp,
    )?;
    Ok(())
}

/// Keeps inputs, automatic turns and client attachment out during a family
/// operation. Hosts acquire these in ancestor-before-descendant order.
pub struct SessionMaintenance {
    session: Session,
    state: tokio::sync::OwnedMutexGuard<crate::session::State>,
}

impl SessionMaintenance {
    pub async fn rewind(
        &mut self,
        params: SessionRewindParams,
    ) -> Result<SessionRewindResponse, CoreError> {
        self.session.rewind_locked(&mut self.state, params).await
    }
    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.state.journal.snapshot
    }

    pub fn ensure_stopped(&self) -> Result<(), CoreError> {
        let state = &self.state;
        state.available()?;
        if state.active.is_some()
            || state.btw.is_some()
            || !state.queue.is_empty()
            || !state.requests.is_empty()
            || state.goal_deadline.is_some()
            || state
                .journal
                .snapshot
                .background_commands
                .iter()
                .any(|command| command.running)
            || (!state.automatic_turns_paused
                && state
                    .background_events
                    .has_pending_wake(self.session.shared.next_options().auto_wake_limit))
        {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "A child session still has work running; stop it first",
            ));
        }
        Ok(())
    }

    pub fn ensure_trashable(&self) -> Result<(), CoreError> {
        self.session.check_trashable(&self.state)
    }

    /// After every family member has passed preflight, reserve a close before
    /// dropping the guards. The host must then finish `Session::close`.
    pub fn reserve_close(&mut self) {
        self.state.closing = true;
    }
}

impl Session {
    /// A dormant parent has no turn override; use its last persisted policy.
    pub async fn restore_agent_policy(&self) -> Result<(), CoreError> {
        let state = self.shared.state.lock().await;
        let Some(source) = state.journal.snapshot.parent.clone() else {
            return Ok(());
        };
        let store = state.journal.store.clone();
        drop(state);
        let policy = tokio::task::spawn_blocking(move || {
            store.events_after(&source.session_id, 0).map(|events| {
                events
                    .into_iter()
                    .rev()
                    .find_map(|event| match event.payload {
                        EventPayload::SessionUpdated { settings } => Some(settings.approval_policy),
                        _ => None,
                    })
                    .unwrap_or(PermissionMode::AlwaysAsk)
            })
        })
        .await
        .map_err(|_| CoreError::internal())??;
        self.shared.state.lock().await.inherited_policy =
            Some(tokio::sync::watch::channel(policy).1);
        Ok(())
    }
    /// Restore a child using the parent's in-memory environment. The host checks
    /// ownership and holds parent then child operation locks before calling.
    pub async fn open_agent(&self, id: String) -> Result<Session, CoreError> {
        let runtime = self.shared.runtime();
        let store = self.shared.state.lock().await.journal.store.clone();
        let (child, _) = Self::open(
            store,
            id,
            runtime.context.clone(),
            runtime.model.clone(),
            runtime.options.clone(),
        )
        .await?;
        if let Some(source) = self.shared.configuration.get() {
            child.set_configuration_source(source.clone(), runtime.revision);
        }
        self.attach_agent(&child).await?;
        Ok(child)
    }

    pub async fn attach_agent(&self, child: &Session) -> Result<(), CoreError> {
        let mut parent = self.shared.state.lock().await;
        let mut state = child.shared.state.lock().await;
        if state
            .journal
            .snapshot
            .parent
            .as_ref()
            .is_none_or(|source| source.session_id != parent.journal.snapshot.id)
        {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "Agent does not belong to this session",
            ));
        }
        parent.approval_policy();
        state.inherited_policy = Some(parent.permission_policy.subscribe());
        if !parent
            .agent_children
            .iter()
            .any(|entry| entry.ptr_eq(&std::sync::Arc::downgrade(&child.shared)))
        {
            parent
                .agent_children
                .push(std::sync::Arc::downgrade(&child.shared));
        }
        for request in &state.journal.snapshot.pending_requests {
            if !parent
                .journal
                .snapshot
                .agent_requests
                .iter()
                .any(|forwarded| {
                    forwarded.agent_id == state.journal.snapshot.id
                        && forwarded.request.id == request.id
                })
            {
                parent
                    .journal
                    .append(vec![EventPayload::AgentRequestOpened {
                        forwarded: forwarded_request(&state.journal.snapshot, request.clone()),
                    }])
                    .await?;
            }
        }
        drop(state);
        drop(parent);
        watch_agent(self, child).await
    }

    pub async fn send_agent_input(
        &self,
        child: &Session,
        message: String,
    ) -> Result<SessionInputResponse, CoreError> {
        let parent = self.shared.state.lock().await;
        parent.available()?;
        if parent
            .active
            .as_ref()
            .is_some_and(|active| active.cancel.is_cancelled())
            || parent.goal_summary.is_some()
        {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "The parent turn is stopping",
            ));
        }
        let snapshot = child.snapshot().await.session;
        if snapshot
            .parent
            .as_ref()
            .is_none_or(|source| source.session_id != parent.journal.snapshot.id)
        {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "Agent does not belong to this session",
            ));
        }
        if snapshot
            .parent
            .as_ref()
            .is_some_and(|source| source.agent_type == AgentType::General)
            && (parent.journal.snapshot.settings.plan_mode
                || parent
                    .active
                    .as_ref()
                    .is_some_and(|active| active.turn.review.is_some()))
        {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "Plan mode and review rounds allow only explorer agents",
            ));
        }
        if snapshot
            .agent
            .as_ref()
            .is_none_or(|agent| agent.status != AgentStatus::Running)
            && parent.running_agents(Some(&snapshot.id)).await >= 4
        {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "At most four sub-agents may run at once",
            ));
        }
        child
            .input(SessionInputParams {
                input: vec![UserInput::Text { text: message }],
                client_name: "agent".into(),
                mode: InputMode::Queue,
                review: None,
                non_interactive: parent.non_interactive(),
                approval_policy: None,
            })
            .await
    }

    pub async fn maintenance(&self) -> SessionMaintenance {
        SessionMaintenance {
            session: self.clone(),
            state: self.shared.state.clone().lock_owned().await,
        }
    }
    /// Create a fresh child and start its first turn. The host holds the parent's
    /// operation lock and registers the returned Session before releasing it,
    /// exactly as for ordinary create/resume. Continue with `input`, stop with
    /// `close`; neither requires a separate child-specific execution path.
    pub async fn spawn_agent(
        &self,
        id: String,
        task: String,
    ) -> Result<(Session, SessionCreateResponse), CoreError> {
        self.spawn_agent_of_type(id, task, AgentType::General).await
    }

    pub async fn spawn_agent_of_type(
        &self,
        id: String,
        task: String,
        kind: AgentType,
    ) -> Result<(Session, SessionCreateResponse), CoreError> {
        let parent = self.clone();
        crate::session::owned(async move {
            if task.trim().is_empty() {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest,
                    "agent task must not be empty",
                ));
            }
            let mut state = parent.shared.state.lock().await;
            state.available()?;
            if state.active.as_ref().is_some_and(|active| active.cancel.is_cancelled())
                || state.goal_summary.is_some()
            {
                return Err(CoreError::new(
                    ErrorCode::Conflict, "The parent turn is stopping",
                ));
            }
            if state.journal.snapshot.parent.is_some() {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest, "Sub-agents cannot spawn other agents",
                ));
            }
            if kind == AgentType::Unknown
                || (kind == AgentType::General
                    && (state.journal.snapshot.settings.plan_mode
                        || state.active.as_ref().is_some_and(|active| active.turn.review.is_some())))
            {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest, "Plan mode and review rounds allow only explorer agents",
                ));
            }
            state.agent_children.retain(|child| child.strong_count() > 0);
            if state.running_agents(None).await >= 4 {
                return Err(CoreError::new(ErrorCode::Conflict, "At most four sub-agents may run at once; wait for one to finish or close it first"));
            }
            let turn = state
                .journal
                .snapshot
                .turns
                .iter()
                .rev()
                .find(|turn| !matches!(turn.client_name.as_str(), "rewind" | "compact" | "userShell"))
                .ok_or_else(|| {
                    CoreError::new(ErrorCode::InvalidRequest, "parent session has no turn")
                })?;
            let source = SessionParent {
                agent_type: kind,
                session_id: state.journal.snapshot.id.clone(),
                turn_id: turn.id.clone(),
                task: task.clone(),
            };
            let runtime = parent.shared.runtime();
            let mut settings = state.journal.snapshot.settings.clone();
            let suffix = id.chars().rev().take(6).collect::<String>();
            settings.name = Some(format!("agent-{}", suffix.chars().rev().collect::<String>()));
            settings.approval_policy = state.approval_policy();
            let (child, _) = Self::create_inner(
                state.journal.store.clone(),
                id,
                settings,
                runtime.context.clone(),
                runtime.model.clone(),
                runtime.options.clone(),
                Some(source),
            )
            .await?;
            state.agent_children.push(std::sync::Arc::downgrade(&child.shared));
            child.shared.state.lock().await.inherited_policy = Some(state.permission_policy.subscribe());
            watch_agent(&parent, &child).await?;
            child
                .input(SessionInputParams {
                    input: vec![UserInput::Text { text: task }],
                    client_name: "agent".into(),
                    mode: InputMode::Queue,
                    review: None,
                    non_interactive: state.non_interactive(),
                    approval_policy: None,
                })
                .await?;
            // Keep the parent's service generation alive for the child's first
            // turn; subsequent turns use ordinary server configuration reload.
            if let Some(active) = child.shared.state.lock().await.active.as_mut() {
                active.configuration_lease = Some(
                    state
                        .active
                        .as_ref()
                        .and_then(|active| active.configuration_lease.clone())
                        .unwrap_or_else(|| std::sync::Arc::new(())),
                );
            }
            if let Some(source) = parent.shared.configuration.get() {
                child.set_configuration_source(source.clone(), runtime.revision);
            }
            let response = child.snapshot().await;
            Ok((
                child,
                SessionCreateResponse {
                    session: response.session,
                    event_seq: response.event_seq,
                },
            ))
        })
        .await
    }
}

async fn watch_agent(parent: &Session, child: &Session) -> Result<(), CoreError> {
    *child.shared.agent_parent.write().unwrap() = Some(std::sync::Arc::downgrade(&parent.shared));
    {
        let mut state = child.shared.state.lock().await;
        if state.agent_forwarding {
            return Ok(());
        }
        state.agent_forwarding = true;
    }
    let mut snapshot = child.snapshot().await.session;
    let mut events = child.subscribe(snapshot.last_event_seq).await?;
    let parent_link = child.shared.agent_parent.clone();
    let child = std::sync::Arc::downgrade(&child.shared);
    tokio::spawn(async move {
        while let Ok(Some(event)) = events.recv().await {
            if apply_event_mut(&mut snapshot, &event).is_err() {
                break;
            }
            let Some(parent) = parent_link
                .read()
                .unwrap()
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
            else {
                // A child may remain open while its parent is closed. Keep
                // following the child so attachment can replace the weak link.
                continue;
            };
            let payload = match event.payload {
                EventPayload::RequestOpened { request } => Some(EventPayload::AgentRequestOpened {
                    forwarded: forwarded_request(&snapshot, request),
                }),
                EventPayload::RequestResolved {
                    request_id,
                    resolution,
                    client_name,
                } => Some(EventPayload::AgentRequestResolved {
                    agent_id: snapshot.id.clone(),
                    request_id,
                    resolution,
                    client_name,
                }),
                EventPayload::TurnCompleted { turn }
                    if !matches!(
                        turn.client_name.as_str(),
                        "compact" | "userShell" | "rewind"
                    ) =>
                {
                    let mut state = parent.state.lock().await;
                    let suppressed = match child.upgrade() {
                        Some(child) => child
                            .state
                            .lock()
                            .await
                            .suppressed_agent_turns
                            .contains(&turn.id),
                        None => true,
                    };
                    if !suppressed
                        && !state.closing
                        && let Some(agent) = &snapshot.agent
                    {
                        let status = match agent.status {
                            AgentStatus::Completed => "completed",
                            AgentStatus::Failed => "failed",
                            AgentStatus::Interrupted => "interrupted",
                            _ => continue,
                        };
                        let text = format!(
                            "[Sub-agent {} ({}) {status}]\n{}",
                            snapshot.id,
                            snapshot.settings.name.as_deref().unwrap_or(&snapshot.id),
                            canter_tools::truncate_model_output(
                                agent.result.as_deref().unwrap_or("No final reply.")
                            )
                        );
                        if state.agent_notice(text).await.is_err() {
                            break;
                        }
                    }
                    None
                }
                _ => None,
            };
            if let Some(payload) = payload {
                let mut state = parent.state.lock().await;
                if state.closing {
                    continue;
                }
                if state.journal.append(vec![payload]).await.is_err() {
                    break;
                }
            }
        }
        if let Some(child) = child.upgrade() {
            child.state.lock().await.agent_forwarding = false;
        }
    });
    Ok(())
}

fn forwarded_request(snapshot: &SessionSnapshot, request: PendingRequest) -> AgentRequest {
    let item_id = match &request.kind {
        RequestKind::Approval(approval) | RequestKind::SandboxApproval(approval) => {
            Some(approval.item_id.as_str())
        }
        RequestKind::PlanApproval(approval) => Some(approval.item_id.as_str()),
        _ => None,
    };
    let item = snapshot
        .turns
        .iter()
        .find(|turn| turn.id == request.turn_id)
        .and_then(|turn| turn.items.iter().find(|item| item.id() == item_id))
        .cloned();
    AgentRequest {
        agent_id: snapshot.id.clone(),
        agent_name: snapshot
            .settings
            .name
            .clone()
            .unwrap_or_else(|| snapshot.id.clone()),
        request,
        cwd: snapshot.settings.cwd.clone(),
        item,
    }
}
