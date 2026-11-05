//! Coordinate related ordinary sessions under ancestor-first operation locks.
use super::*;

pub(super) struct Host(pub std::sync::Weak<App>);
impl canter_core::AgentHost for Host {
    fn execute(
        &self,
        parent: String,
        command: canter_core::AgentCommand,
        cancel: CancellationToken,
    ) -> futures_util::future::BoxFuture<'_, Result<canter_core::AgentToolOutput, CoreError>> {
        Box::pin(async move {
            let app = self.0.upgrade().ok_or_else(|| CoreError {
                code: ErrorCode::Conflict,
                message: "Session host stopped".into(),
            })?;
            if cancel.is_cancelled() {
                return Err(CoreError {
                    code: ErrorCode::Conflict,
                    message: "Turn interrupted".into(),
                });
            }
            #[cfg(test)]
            app.pauses.hit(&parent, "agent_execute").await;
            let result: Result<canter_core::AgentToolOutput, ApiError> = async {
                match command {
                canter_core::AgentCommand::SpawnAgent { task, kind, name } => {
                    let response = app.spawn_agent_of_type(parent, task, kind, name).await?;
                    Ok(canter_core::AgentToolOutput {
                        value: serde_json::json!({"id":response.session.id,"name":response.session.settings.name}),
                        agents: vec![collab_agent(&response.session, false)],
                    })
                }
                canter_core::AgentCommand::WaitAgent { ids, timeout_ms } => {
                    app.wait_agents(&parent, &ids, timeout_ms, &cancel).await
                }
                canter_core::AgentCommand::SendInput { id, message } => {
                    if id == parent {
                        return Err(ApiError::new(ErrorCode::InvalidRequest, "Expected a child agent id"));
                    }
                    let _parent_operation = app.session_locks.lock(&parent).await;
                    let child_operation = app.session_locks.lock(&id).await;
                    app.agent_snapshot(&parent, &id, child_operation).await?;
                    let parent_session = app.opened_session(&parent).await.ok_or_else(|| {
                        ApiError::new(ErrorCode::SessionNotResumed, "Resume the parent session first")
                    })?;
                    let child = match app.opened_session(&id).await {
                        Some(child) if child.snapshot().await.session.status != SessionStatus::Closed => child,
                        _ => {
                            let child = parent_session.open_agent(id.clone()).await?;
                            app.register_session(id.clone(), child.clone()).await?;
                            child
                        }
                    };
                    let response = parent_session.send_agent_input(&child, message).await?;
                    Ok(canter_core::AgentToolOutput {
                        value: serde_json::json!({"id":id,"turnId":response.turn_id}),
                        agents: vec![collab_agent(&child.snapshot().await.session, false)],
                    })
                }
                canter_core::AgentCommand::CloseAgent { id } => {
                    if id == parent {
                        return Err(ApiError::new(ErrorCode::InvalidRequest, "Expected a child agent id"));
                    }
                    let _parent_operation = app.session_locks.lock(&parent).await;
                    let child_operation = app.session_locks.lock(&id).await;
                    let snapshot = app.agent_snapshot(&parent, &id, child_operation.clone()).await?;
                    if snapshot.status != SessionStatus::Closed {
                        let child = match app.opened_session(&id).await {
                            Some(child) => child,
                            None => {
                                let parent = app.opened_session(&parent).await.ok_or_else(|| {
                                    ApiError::new(ErrorCode::SessionNotResumed, "Resume the parent session first")
                                })?;
                                parent.open_agent(id.clone()).await?
                            }
                        };
                        child.close().await?;
                    }
                    app.sessions.lock().await.remove(&id);
                    app.registry_changed.notify_waiters();
                    let snapshot = app.agent_snapshot(&parent, &id, child_operation).await?;
                    Ok(canter_core::AgentToolOutput {
                        value: agent_value(&snapshot),
                        agents: vec![collab_agent(&snapshot, false)],
                    })
                }
                }
            }.await;
            result.map_err(|error| CoreError {
                code: error.0.code,
                message: error.0.message,
            })
        })
    }

    fn describe(
        &self,
        parent: String,
        ids: Vec<String>,
    ) -> futures_util::future::BoxFuture<'_, Vec<CollabAgent>> {
        Box::pin(async move {
            let Some(app) = self.0.upgrade() else {
                return ids.into_iter().map(unknown_agent).collect();
            };
            let mut agents = Vec::new();
            for id in ids {
                let operation = app.session_locks.lock(&id).await;
                agents.push(match app.agent_snapshot(&parent, &id, operation).await {
                    Ok(snapshot) => collab_agent(&snapshot, false),
                    Err(_) => unknown_agent(id),
                });
            }
            agents
        })
    }
}

impl App {
    async fn agent_snapshot(
        &self,
        parent: &str,
        id: &str,
        operation: Arc<SessionGuard>,
    ) -> Result<SessionSnapshot, ApiError> {
        let snapshot = match self.opened_session(id).await {
            Some(session) => session.snapshot().await.session,
            None => self.disk_snapshot(id, operation).await?.session,
        };
        if snapshot
            .parent
            .as_ref()
            .is_none_or(|source| source.session_id != parent)
        {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "Agent does not belong to this session",
            ));
        }
        Ok(snapshot)
    }

    async fn wait_agents(
        &self,
        parent: &str,
        ids: &[String],
        timeout_ms: u64,
        cancel: &CancellationToken,
    ) -> Result<canter_core::AgentToolOutput, ApiError> {
        use futures_util::{StreamExt, stream::FuturesUnordered};
        if ids.is_empty() || ids.iter().any(|id| id == parent) || timeout_ms == 0 {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "wait_agent requires child ids and a positive timeoutMs",
            ));
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms.min(600_000));
        loop {
            let parent_operation = self.session_locks.lock(parent).await;
            let mut data = Vec::new();
            let mut agents = Vec::new();
            let mut completed = Vec::new();
            let mut terminal = false;
            let mut changes = FuturesUnordered::new();
            for id in ids {
                let operation = self.session_locks.lock(id).await;
                let snapshot = self.agent_snapshot(parent, id, operation).await?;
                let stopped = snapshot
                    .agent
                    .as_ref()
                    .is_none_or(|agent| agent.status != AgentStatus::Running);
                terminal |= stopped;
                if let Some(session) = self.opened_session(id).await {
                    let mut events = session.subscribe(snapshot.last_event_seq).await?;
                    changes.push(async move {
                        while let Some(event) = events.recv().await? {
                            if matches!(
                                event.payload,
                                EventPayload::TurnCompleted { .. }
                                    | EventPayload::SessionStatus {
                                        status: SessionStatus::Closed,
                                        ..
                                    }
                            ) {
                                break;
                            }
                        }
                        Ok::<_, CoreError>(())
                    });
                }
                data.push(agent_value(&snapshot));
                agents.push(collab_agent(&snapshot, true));
                if stopped {
                    completed.push(snapshot);
                }
            }
            if terminal || tokio::time::Instant::now() >= deadline || changes.is_empty() {
                if let Some(parent) = self.opened_session(parent).await {
                    for snapshot in &completed {
                        parent.acknowledge_agent_result(snapshot).await?;
                    }
                }
                return Ok(canter_core::AgentToolOutput {
                    value: serde_json::json!({"data":data}),
                    agents,
                });
            }
            drop(parent_operation);
            tokio::select! {
                _ = cancel.cancelled() => return Err(ApiError::new(ErrorCode::Conflict, "Wait interrupted")),
                _ = tokio::time::sleep_until(deadline) => {},
                changed = changes.next() => { if let Some(result) = changed { result?; } },
            }
        }
    }
    #[cfg(test)]
    pub(crate) async fn spawn_agent(
        self: &Arc<Self>,
        parent_id: String,
        task: String,
    ) -> Result<SessionCreateResponse, ApiError> {
        self.spawn_agent_of_type(parent_id, task, AgentType::General, None)
            .await
    }

    pub(crate) async fn spawn_agent_of_type(
        self: &Arc<Self>,
        parent_id: String,
        task: String,
        kind: AgentType,
        name: Option<String>,
    ) -> Result<SessionCreateResponse, ApiError> {
        let app = self.clone();
        tokio::spawn(async move {
            let draining = app.draining.read().await;
            if *draining {
                return Err(ApiError::new(ErrorCode::Conflict, "server is stopping"));
            }
            let operation = app.session_locks.lock(&parent_id).await;
            let parent = match app.opened_session(&parent_id).await {
                Some(parent) => parent,
                None => {
                    app.disk_snapshot(&parent_id, operation.clone()).await?;
                    return Err(ApiError::new(
                        ErrorCode::SessionNotResumed,
                        "resume the parent session first",
                    ));
                }
            };
            let id = crate::new_id();
            let _child_operation = app.session_locks.lock(&id).await;
            let (child, response) = parent
                .spawn_agent_of_type(id.clone(), task, kind, name)
                .await?;
            app.register_session(id, child).await?;
            Ok(response)
        })
        .await
        .map_err(|_| ApiError::internal())?
    }

    /// P2-179: the user stops child `id` of `parent`. Not running: nothing
    /// changes. Returns the child's list entry after.
    pub(super) async fn stop_agent(
        &self,
        parent: &str,
        id: &str,
    ) -> Result<AgentSummary, ApiError> {
        let _parent_operation = self.session_locks.lock(parent).await;
        let child_operation = self.session_locks.lock(id).await;
        self.agent_snapshot(parent, id, child_operation.clone())
            .await?;
        let parent_session = self.opened_session(parent).await.ok_or_else(|| {
            ApiError::new(
                ErrorCode::SessionNotResumed,
                "Resume the parent session first",
            )
        })?;
        // A child that is not open has no turn running.
        if let Some(child) = self.opened_session(id).await {
            parent_session.stop_agent(&child).await?;
        }
        let snapshot = self.agent_snapshot(parent, id, child_operation).await?;
        AgentSummary::from_snapshot(&snapshot).ok_or_else(ApiError::internal)
    }
}

fn agent_value(snapshot: &SessionSnapshot) -> serde_json::Value {
    serde_json::json!({"id":snapshot.id,"name":snapshot.settings.name,"status":snapshot.agent.as_ref().map(|agent| agent.status),"result":snapshot.agent.as_ref().and_then(|agent| agent.result.as_deref()).map(canter_tools::truncate_model_output)})
}

/// A child as a sub-agent tool card shows it; `with_result` adds the final
/// reply as given to the model.
fn collab_agent(snapshot: &SessionSnapshot, with_result: bool) -> CollabAgent {
    CollabAgent {
        id: snapshot.id.clone(),
        name: snapshot
            .settings
            .name
            .clone()
            .unwrap_or_else(|| snapshot.id.clone()),
        agent_type: snapshot
            .parent
            .as_ref()
            .map(|parent| parent.agent_type)
            .unwrap_or_default(),
        status: snapshot.agent.as_ref().map(|agent| agent.status),
        result: snapshot
            .agent
            .as_ref()
            .and_then(|agent| agent.result.as_deref())
            .filter(|_| with_result)
            .map(canter_tools::truncate_model_output),
    }
}

/// An id that is not a child of the parent: known by its id alone.
fn unknown_agent(id: String) -> CollabAgent {
    CollabAgent {
        name: id.clone(),
        id,
        agent_type: AgentType::default(),
        status: None,
        result: None,
    }
}

struct Member {
    id: String,
    operation: Arc<SessionGuard>,
    session: Option<Session>,
}

pub(super) struct Family(Vec<Member>);

impl Family {
    pub(super) async fn lock(
        app: &App,
        id: &str,
        operation: Arc<SessionGuard>,
    ) -> Result<Self, ApiError> {
        let mut members = vec![Member {
            id: id.into(),
            operation,
            session: app.opened_session(id).await,
        }];
        let mut index = 0;
        while index < members.len() {
            let store = app.store.clone();
            let all = tokio::task::spawn_blocking(move || store.list_all())
                .await
                .map_err(|_| ApiError::internal())?
                .map_err(CoreError::from)?;
            // Read each generation after locking its parent: concurrent child
            // creation cannot appear behind the traversal.
            for child in all
                .into_iter()
                .filter(|child| {
                    child
                        .parent
                        .as_ref()
                        .is_some_and(|parent| parent.session_id == members[index].id)
                })
                .collect::<Vec<_>>()
            {
                if members.iter().any(|member| member.id == child.id) {
                    continue;
                }
                let operation = app.session_locks.lock(&child.id).await;
                let session = app.opened_session(&child.id).await;
                // A direct deletion may have completed while we waited.
                if !app
                    .store
                    .session_dir(&child.id)
                    .map_err(CoreError::from)?
                    .exists()
                {
                    continue;
                }
                members.push(Member {
                    id: child.id,
                    operation,
                    session,
                });
            }
            index += 1;
        }
        Ok(Self(members))
    }

    pub(super) fn len(&self) -> usize {
        self.0.len()
    }

    pub(super) fn ids(&self) -> Vec<String> {
        self.0.iter().map(|member| member.id.clone()).collect()
    }

    pub(super) fn has_opened_session(&self) -> bool {
        self.0.iter().any(|member| member.session.is_some())
    }

    pub(super) async fn guards(
        &self,
        app: &App,
    ) -> Result<Vec<(usize, canter_core::SessionMaintenance)>, ApiError> {
        let mut guards = Vec::new();
        for (index, member) in self.0.iter().enumerate() {
            if let Some(session) = &member.session {
                guards.push((index, session.maintenance().await));
            } else {
                app.disk_snapshot(&member.id, member.operation.clone())
                    .await?;
            }
        }
        Ok(guards)
    }

    pub(super) async fn trash(self, app: &App) -> Result<(), ApiError> {
        let mut guards = self.guards(app).await?;
        for (_, guard) in &guards {
            guard.ensure_trashable()?;
        }
        let ids = self
            .0
            .iter()
            .map(|member| member.id.clone())
            .collect::<Vec<_>>();
        prepare_trash(app, &ids).await?;
        for (_, guard) in &mut guards {
            guard.reserve_close();
        }
        drop(guards);
        self.move_to_trash(app, &ids).await
    }

    pub(super) async fn rewind(
        self,
        app: &App,
        params: SessionRewindParams,
    ) -> Result<SessionRewindResponse, ApiError> {
        let parent = &self.0[0];
        let session = parent.session.as_ref().ok_or_else(|| {
            ApiError::new(ErrorCode::SessionNotResumed, "resume the session first")
        })?;
        let mut parent_guard = session.maintenance().await;
        let snapshot = parent_guard.snapshot();
        let removed_turns = if params.rewind_conversation {
            snapshot
                .turns
                .iter()
                .skip_while(|turn| turn.id != params.before_turn_id)
                .map(|turn| turn.id.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let mut guards = Vec::new();
        let mut removed = Vec::new();
        for (index, child) in self.0.iter().enumerate().skip(1) {
            let source = if let Some(session) = &child.session {
                let guard = session.maintenance().await;
                guard.ensure_stopped()?;
                let source = guard.snapshot().parent.clone();
                guards.push((index, guard));
                source
            } else {
                app.disk_snapshot(&child.id, child.operation.clone())
                    .await?
                    .session
                    .parent
            };
            if source.as_ref().is_some_and(|source| {
                (source.session_id == parent.id && removed_turns.contains(&source.turn_id))
                    || removed.contains(&source.session_id)
            }) {
                removed.push(child.id.clone());
            }
        }
        for (index, guard) in &guards {
            if removed.contains(&self.0[*index].id) {
                guard.ensure_trashable()?;
            }
        }
        if !removed.is_empty() {
            prepare_trash(app, &removed).await?;
        }
        // Core checks preview sequence, boundaries and file conflicts under its
        // parent state lock. Until it succeeds no child has been changed.
        let response = parent_guard.rewind(params).await?;
        for (index, guard) in &mut guards {
            if removed.contains(&self.0[*index].id) {
                guard.reserve_close();
            }
        }
        drop(guards);
        drop(parent_guard);
        self.move_to_trash(app, &removed).await?;
        Ok(response)
    }

    async fn move_to_trash(&self, app: &App, ids: &[String]) -> Result<(), ApiError> {
        for member in self
            .0
            .iter()
            .rev()
            .filter(|member| ids.contains(&member.id))
        {
            if let Some(session) = &member.session {
                session.close().await?;
            }
            app.sessions.lock().await.remove(&member.id);
            let source = app.store.session_dir(&member.id).map_err(CoreError::from)?;
            let target = app.home.join("trash").join(&member.id);
            tokio::task::spawn_blocking(move || std::fs::rename(source, target))
                .await
                .map_err(|_| ApiError::internal())?
                .map_err(|error| CoreError::from(canter_store::StoreError::from(error)))?;
        }
        app.registry_changed.notify_waiters();
        Ok(())
    }
}

async fn prepare_trash(app: &App, ids: &[String]) -> Result<(), ApiError> {
    let trash = app.home.join("trash");
    let ids = ids.to_vec();
    tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        for id in ids {
            match std::fs::symlink_metadata(trash.join(id)) {
                Ok(_) => return Err(ApiError::new(ErrorCode::Conflict, "The trash destination already exists; move or remove it manually before deleting this session.")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
                Err(_) => return Err(ApiError::internal()),
            }
        }
        std::fs::DirBuilder::new().mode(0o700).recursive(true).create(&trash).map_err(|_| ApiError::internal())?;
        std::fs::set_permissions(&trash, std::fs::Permissions::from_mode(0o700)).map_err(|_| ApiError::internal())?;
        Ok(())
    }).await.map_err(|_| ApiError::internal())?
}
