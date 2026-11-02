use crate::{CoreError, Session};
use canter_protocol::*;

/// Startup recovery only, after acquiring the daemon lease. Does not open
/// runtimes or require saved environment variables.
pub fn recover_agent_sessions(store: &canter_store::Store) -> Result<(), CoreError> {
    for summary in store
        .list_all()?
        .into_iter()
        .filter(|summary| summary.parent.is_some())
    {
        let mut writer = store.open(&summary.id)?;
        if !writer
            .snapshot()
            .agent
            .as_ref()
            .is_some_and(|agent| agent.status == AgentStatus::Running)
        {
            continue;
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
    }
    Ok(())
}

/// Keeps inputs, automatic turns and client attachment out during a family
/// operation. Hosts acquire these in ancestor-before-descendant order.
pub struct SessionMaintenance {
    session: Session,
    state: tokio::sync::OwnedMutexGuard<crate::session::State>,
}

impl SessionMaintenance {
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
        let parent = self.clone();
        crate::session::owned(async move {
            if task.trim().is_empty() {
                return Err(CoreError::new(
                    ErrorCode::InvalidRequest,
                    "agent task must not be empty",
                ));
            }
            let state = parent.shared.state.lock().await;
            state.available()?;
            let turn = state
                .journal
                .snapshot
                .turns
                .iter()
                .rev()
                .find(|turn| turn.client_name != "rewind")
                .ok_or_else(|| {
                    CoreError::new(ErrorCode::InvalidRequest, "parent session has no turn")
                })?;
            let source = SessionParent {
                session_id: state.journal.snapshot.id.clone(),
                turn_id: turn.id.clone(),
                task: task.clone(),
            };
            let runtime = parent.shared.runtime();
            let mut settings = state.journal.snapshot.settings.clone();
            settings.name = None;
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
            child
                .input(SessionInputParams {
                    input: vec![UserInput::Text { text: task }],
                    client_name: "agent".into(),
                    mode: InputMode::Queue,
                    review: None,
                    non_interactive: false,
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
