//! Coordinate related ordinary sessions under ancestor-first operation locks.
use super::*;

impl App {
    /// Server-side entry point for the next phase's model tools. Creation and
    /// registration belong to one accepted operation even if its caller leaves.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "P2-178 will expose the model tools")
    )]
    pub(crate) async fn spawn_agent(
        self: &Arc<Self>,
        parent_id: String,
        task: String,
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
            let (child, response) = parent.spawn_agent(id.clone(), task).await?;
            app.sessions.lock().await.insert(id, child);
            app.registry_changed.notify_waiters();
            Ok(response)
        })
        .await
        .map_err(|_| ApiError::internal())?
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

    async fn guards(
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
        let snapshot = session.snapshot().await.session;
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
        let response = session.rewind(params).await?;
        for (index, guard) in &mut guards {
            if removed.contains(&self.0[*index].id) {
                guard.reserve_close();
            }
        }
        drop(guards);
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
