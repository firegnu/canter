use crate::{
    CoreError, Session,
    context::HistoryEntry,
    session::{Shared, State, owned},
};
use canter_model::Message;
use canter_protocol::*;
use std::sync::{Arc, Weak};

impl State {
    async fn background_update(&mut self, command: BackgroundCommand) -> Result<(), CoreError> {
        if !command.running {
            let message = Message::User {
                text: format!(
                    "[Background command {} finished: {}. Exit code: {}. Use shell_output with id={} to read the remaining output.]",
                    command.id,
                    command.command,
                    command.exit_code.map_or_else(
                        || "unknown (signal or server shutdown)".into(),
                        |code| code.to_string()
                    ),
                    command.id,
                ),
            };
            self.journal
                .record(HistoryEntry::BackgroundNotice {
                    message: message.clone(),
                })
                .await?;
            self.plan_mode_notices.push(message);
            crate::context::flush_plan_mode_notices(&mut self.history, &mut self.plan_mode_notices);
        }
        self.journal
            .append(vec![EventPayload::BackgroundCommandUpdated { command }])
            .await?;
        Ok(())
    }
    pub(crate) async fn recover_background(&mut self) -> Result<(), CoreError> {
        for mut command in self.journal.snapshot.background_commands.clone() {
            if command.running {
                command.running = false;
                command.completed_at = Some(crate::journal::now());
                command.exit_code = None;
                self.background_update(command).await?;
            }
        }
        Ok(())
    }
}

impl Shared {
    pub(crate) async fn sync_background(&self) -> Result<(), CoreError> {
        let mut state = self.state.lock().await;
        for command in self.tools.background().list() {
            if !state
                .journal
                .snapshot
                .background_commands
                .iter()
                .any(|old| old == &command)
            {
                state.background_update(command).await?;
            }
        }
        Ok(())
    }
}

pub(crate) fn watch(shared: &Arc<Shared>) {
    let mut changes = shared.tools.background().subscribe();
    let lifetime = shared.lifetime.clone();
    let shared: Weak<Shared> = Arc::downgrade(shared);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = lifetime.cancelled() => break,
                changed = changes.changed() => if changed.is_err() { break; },
            }
            let Some(shared) = shared.upgrade() else {
                break;
            };
            if shared.state.lock().await.closing {
                break;
            }
            if shared.sync_background().await.is_err() {
                break;
            }
        }
    });
}

impl Session {
    pub async fn background_commands(&self) -> Result<BackgroundCommandListResponse, CoreError> {
        self.shared.sync_background().await?;
        Ok(BackgroundCommandListResponse {
            data: self
                .shared
                .state
                .lock()
                .await
                .journal
                .snapshot
                .background_commands
                .clone(),
        })
    }
    pub async fn background_output(&self, id: u64) -> Result<BackgroundCommandOutput, CoreError> {
        match self.shared.tools.background().output(id, false) {
            Ok(output) => Ok(output),
            Err(_) => {
                let state = self.shared.state.lock().await;
                let command = state
                    .journal
                    .snapshot
                    .background_commands
                    .iter()
                    .find(|entry| entry.id == id)
                    .ok_or_else(|| {
                        CoreError::new(ErrorCode::InvalidRequest, "unknown background command")
                    })?
                    .clone();
                Ok(BackgroundCommandOutput {
                    command,
                    output: "Output unavailable after server restart.".into(),
                    skipped_bytes: 0,
                })
            }
        }
    }
    pub async fn stop_background(
        &self,
        id: Option<u64>,
    ) -> Result<BackgroundCommandListResponse, CoreError> {
        let session = self.clone();
        owned(async move {
            if let Some(id) = id {
                session
                    .shared
                    .tools
                    .background()
                    .stop(id)
                    .await
                    .map_err(|message| CoreError::new(ErrorCode::InvalidRequest, &message))?;
            } else {
                session.shared.tools.background().stop_all().await;
            }
            session.background_commands().await
        })
        .await
    }
    pub async fn shell_background(
        &self,
        params: ShellBackgroundParams,
    ) -> Result<ShellBackgroundResponse, CoreError> {
        let session = self.clone();
        owned(async move {
            let command_id = session
                .shared
                .tools
                .background()
                .promote(&params.item_id)
                .await
                .map_err(|message| CoreError::new(ErrorCode::Conflict, &message))?;
            session.shared.sync_background().await?;
            Ok(ShellBackgroundResponse {
                command_id,
                event_seq: session
                    .shared
                    .state
                    .lock()
                    .await
                    .journal
                    .snapshot
                    .last_event_seq,
            })
        })
        .await
    }
}
