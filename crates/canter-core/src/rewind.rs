use crate::{
    CoreError, Session,
    context::HistoryEntry,
    session::{State, id, owned},
};
use canter_protocol::*;
use canter_store::Store;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::Path};

const MAX_FILE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct FileChange {
    id: String,
    turn_id: String,
    path: String,
    before: Option<String>,
    after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    after_bytes: Option<u64>,
    unavailable: Option<String>,
    #[serde(skip)]
    inactive: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Rewind {
    pub before_turn_id: String,
    pub conversation: bool,
    pub event_seq: u64,
    record: Turn,
    restored: Vec<String>,
}
impl Rewind {
    pub fn event(&self) -> EventPayload {
        EventPayload::SessionRewound {
            before_turn_id: self.before_turn_id.clone(),
            rewind_conversation: self.conversation,
            record: self.record.clone(),
        }
    }
}

#[derive(Clone)]
struct Boundary {
    turn_id: String,
    messages: usize,
    users: usize,
    skills: usize,
    changes: usize,
}

#[derive(Default)]
pub(crate) struct Checkpoints {
    boundaries: Vec<Boundary>,
    changes: Vec<FileChange>,
}
impl Checkpoints {
    pub fn boundary(&mut self, turn_id: String, messages: usize, users: usize, skills: usize) {
        self.boundaries.push(Boundary {
            turn_id,
            messages,
            users,
            skills,
            changes: self.changes.len(),
        });
    }
    pub fn clear_boundaries(&mut self) {
        self.boundaries.clear();
    }
    pub fn retract(&mut self, turn_id: &str) -> Option<(usize, usize, usize)> {
        let index = self.boundaries.iter().position(|b| b.turn_id == turn_id)?;
        let boundary = &self.boundaries[index];
        let lengths = (boundary.messages, boundary.users, boundary.skills);
        self.boundaries.truncate(index);
        Some(lengths)
    }
    pub fn record_file(&mut self, checkpoint: FileChange) {
        self.changes.push(checkpoint);
    }
    pub fn abort(&mut self, id: &str) {
        if let Some(change) = self.changes.iter_mut().find(|change| change.id == id) {
            change.inactive = true;
        }
    }
    pub fn apply(&mut self, rewind: &Rewind) -> Option<(usize, usize, usize)> {
        let index = self
            .boundaries
            .iter()
            .position(|b| b.turn_id == rewind.before_turn_id)?;
        let boundary = self.boundaries[index].clone();
        for change in &mut self.changes[boundary.changes..] {
            if rewind.restored.contains(&change.path) {
                change.inactive = true;
            }
        }
        if rewind.conversation {
            self.boundaries.truncate(index);
            Some((boundary.messages, boundary.users, boundary.skills))
        } else {
            None
        }
    }
    fn files(&self, boundary: &Boundary) -> BTreeMap<String, (FileChange, FileChange)> {
        let mut files = BTreeMap::new();
        for change in self.changes[boundary.changes..]
            .iter()
            .filter(|change| !change.inactive)
        {
            files
                .entry(change.path.clone())
                .and_modify(|pair: &mut (FileChange, FileChange)| pair.1 = change.clone())
                .or_insert_with(|| (change.clone(), change.clone()));
        }
        files
    }
}

impl State {
    pub async fn checkpoint_file(
        &mut self,
        turn_id: &str,
        call: &canter_tools::PreparedCall,
    ) -> Result<Option<String>, CoreError> {
        let Some(file) = call.file_checkpoint() else {
            return Ok(None);
        };
        let mut checkpoint = FileChange {
            id: id("change"),
            turn_id: turn_id.into(),
            path: file.path.to_string_lossy().into_owned(),
            before: None,
            after: None,
            after_bytes: None,
            unavailable: None,
            inactive: false,
        };
        if file.path.to_str().is_none() {
            checkpoint.unavailable = Some("file path cannot be recorded as UTF-8".into());
        } else {
            if file
                .original
                .is_some_and(|bytes| bytes.len() > MAX_FILE_BYTES)
            {
                checkpoint.unavailable =
                    Some("original file exceeds the 1 MiB checkpoint limit".into());
            }
            let store = self.journal.store.clone();
            let session = self.journal.snapshot.id.clone();
            let original = file
                .original
                .filter(|bytes| bytes.len() <= MAX_FILE_BYTES)
                .map(ToOwned::to_owned);
            let content = if file.content.len() > MAX_FILE_BYTES {
                checkpoint.after_bytes = Some(file.content.len() as u64);
                None
            } else {
                Some(file.content.to_vec())
            };
            let (before, after) = tokio::task::spawn_blocking(move || {
                Ok::<_, CoreError>((
                    original
                        .as_deref()
                        .map(|bytes| store.save_rewind(&session, bytes))
                        .transpose()?,
                    content
                        .as_deref()
                        .map(|bytes| store.save_rewind(&session, bytes))
                        .transpose()?,
                ))
            })
            .await
            .map_err(|_| CoreError::internal())??;
            checkpoint.before = before;
            checkpoint.after = after;
        }
        self.journal
            .record(HistoryEntry::FileCheckpoint {
                checkpoint: checkpoint.clone(),
            })
            .await?;
        let id = checkpoint.id.clone();
        self.rewind.changes.push(checkpoint);
        Ok(Some(id))
    }
    pub async fn abort_checkpoint(&mut self, id: String) -> Result<(), CoreError> {
        self.journal
            .record(HistoryEntry::FileAborted { id: id.clone() })
            .await?;
        self.rewind.abort(&id);
        Ok(())
    }
    fn rewind_boundary(&self, params: &SessionRewindParams) -> Result<Boundary, CoreError> {
        self.available()?;
        if self.active.is_some() || self.btw.is_some() || !self.queue.is_empty() {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot rewind while a turn is active; press Esc to stop it first",
            ));
        }
        if !params.rewind_files && !params.rewind_conversation {
            return Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "choose files, conversation, or both",
            ));
        }
        self.rewind
            .boundaries
            .iter()
            .find(|b| b.turn_id == params.before_turn_id)
            .filter(|_| {
                self.journal
                    .snapshot
                    .turns
                    .iter()
                    .any(|turn| turn.id == params.before_turn_id && rewind_input(turn).is_some())
            })
            .cloned()
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::TurnMismatch,
                    "turn is unavailable or predates the latest compaction",
                )
            })
    }
}

fn rewind_input(turn: &Turn) -> Option<Vec<UserInput>> {
    if !turn.input.is_empty() {
        return Some(turn.input.clone());
    }
    turn.items.iter().find_map(|item| match item {
        Item::BackgroundEvent {
            command_id: Some(_),
            ..
        } => Some(Vec::new()),
        Item::CommandExecution {
            command,
            source: ExecCommandSource::UserShell,
            ..
        } => Some(vec![UserInput::Text {
            text: format!("!{command}"),
        }]),
        _ => None,
    })
}

fn read_bounded(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}
fn backup(store: &Store, session: &str, name: &str) -> Result<Vec<u8>, String> {
    let path = store
        .rewind_path(session, name)
        .map_err(|error| error.to_string())?;
    let bytes = read_bounded(&path).map_err(|error| format!("checkpoint unavailable: {error}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("checkpoint exceeds 1 MiB".into());
    }
    Ok(bytes)
}

struct PlannedFile {
    preview: RewindFile,
    before: Option<Vec<u8>>,
}
async fn plan(state: &State, boundary: &Boundary) -> Result<Vec<PlannedFile>, CoreError> {
    let files = state.rewind.files(boundary);
    let store = state.journal.store.clone();
    let session = state.journal.snapshot.id.clone();
    tokio::task::spawn_blocking(move || {
        files
            .into_iter()
            .map(|(path, (first, last))| {
                let mut preview = RewindFile {
                    path: path.clone(),
                    delete: first.before.is_none() && first.unavailable.is_none(),
                    external_change: false,
                    external_change_unknown: false,
                    unavailable: first.unavailable.clone(),
                };
                let mut before = None;
                if preview.unavailable.is_none() {
                    let result = (|| -> Result<_, String> {
                        let before = first
                            .before
                            .as_deref()
                            .map(|name| backup(&store, &session, name))
                            .transpose()?;
                        if last
                            .after_bytes
                            .is_some_and(|bytes| bytes > MAX_FILE_BYTES as u64)
                        {
                            return Ok((before, false, true));
                        }
                        let expected = backup(
                            &store,
                            &session,
                            last.after
                                .as_deref()
                                .ok_or("missing written-content checkpoint")?,
                        )?;
                        let current = match read_bounded(Path::new(&path)) {
                            Ok(bytes) => Some(bytes),
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                            Err(error) => return Err(error.to_string()),
                        };
                        Ok((
                            before,
                            current.as_deref() != Some(expected.as_slice()),
                            false,
                        ))
                    })();
                    match result {
                        Ok((original, external, unknown)) => {
                            before = original;
                            preview.external_change = external;
                            preview.external_change_unknown = unknown;
                        }
                        Err(error) => preview.unavailable = Some(error),
                    }
                }
                PlannedFile { preview, before }
            })
            .collect()
    })
    .await
    .map_err(|_| CoreError::internal())
}

impl Session {
    pub async fn rewind_list(&self) -> Result<RewindListResponse, CoreError> {
        let state = self.shared.state.lock().await;
        state.available()?;
        if state.active.is_some() {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot rewind while a turn is active; press Esc to stop it first",
            ));
        }
        let data = state
            .journal
            .snapshot
            .turns
            .iter()
            .rev()
            .filter_map(|turn| {
                let boundary = state
                    .rewind
                    .boundaries
                    .iter()
                    .find(|b| b.turn_id == turn.id)?;
                Some(RewindTurn {
                    turn_id: turn.id.clone(),
                    input: rewind_input(turn)?,
                    file_count: state.rewind.files(boundary).len(),
                })
            })
            .collect();
        Ok(RewindListResponse { data })
    }
    pub async fn rewind_preview(
        &self,
        params: SessionRewindParams,
    ) -> Result<RewindPreviewResponse, CoreError> {
        let state = self.shared.state.lock().await;
        let boundary = state.rewind_boundary(&params)?;
        let files = if params.rewind_files {
            plan(&state, &boundary)
                .await?
                .into_iter()
                .map(|file| file.preview)
                .collect()
        } else {
            Vec::new()
        };
        Ok(RewindPreviewResponse {
            files,
            last_event_seq: state.journal.snapshot.last_event_seq,
        })
    }
    pub async fn rewind(
        &self,
        params: SessionRewindParams,
    ) -> Result<SessionRewindResponse, CoreError> {
        let session = self.clone();
        owned(async move { session.rewind_inner(params).await }).await
    }
    async fn rewind_inner(
        &self,
        params: SessionRewindParams,
    ) -> Result<SessionRewindResponse, CoreError> {
        let mut state = self.shared.state.lock().await;
        state.available()?;
        if params.last_event_seq != Some(state.journal.snapshot.last_event_seq) {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Session changed since preview; preview and confirm again",
            ));
        }
        let boundary = state.rewind_boundary(&params)?;
        let files = if params.rewind_files {
            plan(&state, &boundary).await?
        } else {
            Vec::new()
        };
        if files.iter().any(|file| {
            (file.preview.external_change || file.preview.external_change_unknown)
                && !params
                    .confirmed_external_changes
                    .contains(&file.preview.path)
        }) {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Files changed outside canter or cannot be compared; preview and confirm these paths before rewinding",
            ));
        }
        let input = if params.rewind_conversation {
            rewind_input(
                state
                    .journal
                    .snapshot
                    .turns
                    .iter()
                    .find(|turn| turn.id == params.before_turn_id)
                    .expect("validated turn"),
            )
            .expect("validated rewind input")
        } else {
            Vec::new()
        };
        let mut report = vec![format!(
            "Rewind before {}: {}.",
            params.before_turn_id,
            if params.rewind_files && params.rewind_conversation {
                "conversation and files"
            } else if params.rewind_files {
                "files only"
            } else {
                "conversation only"
            }
        )];
        let mut restored = Vec::new();
        for file in files {
            let error = if let Some(reason) = file.preview.unavailable {
                Some(reason)
            } else {
                self.shared
                    .tools
                    .restore_file(Path::new(&file.preview.path), file.before.as_deref())
                    .await
                    .err()
                    .map(|error| error.to_string())
            };
            if let Some(reason) = error {
                report.push(format!("Could not restore {}: {reason}", file.preview.path));
            } else {
                report.push(format!(
                    "{} {}",
                    if file.before.is_some() {
                        "Restored"
                    } else {
                        "Deleted"
                    },
                    file.preview.path
                ));
                restored.push(file.preview.path);
            }
        }
        let record = Turn {
            id: id("rewind"),
            input: Vec::new(),
            client_name: "rewind".into(),
            review: None,
            status: TurnStatus::Completed,
            error: None,
            usage: None,
            started_at: None,
            completed_at: None,
            duration_ms: None,
            items: vec![Item::AgentMessage {
                status: None,
                id: id("rewind-record"),
                text: report.join("\n"),
            }],
        };
        let rewind = Rewind {
            before_turn_id: params.before_turn_id,
            conversation: params.rewind_conversation,
            event_seq: state.journal.snapshot.last_event_seq + 1,
            record,
            restored,
        };
        state
            .journal
            .record(HistoryEntry::Rewind {
                rewind: Box::new(rewind.clone()),
            })
            .await?;
        state.journal.append(vec![rewind.event()]).await?;
        let retained = state.journal.snapshot.turns.clone();
        state.lsp_delivery.rewind(&retained);
        if let Some((messages, users, skills)) = state.rewind.apply(&rewind) {
            let removed = state.history.split_off(messages);
            state.lsp_delivery.notices.retain(|index| *index < messages);
            state.user_messages.truncate(users);
            state.invoked_skills.truncate(skills);
            state.measured_context = None;
            state.restate_plan_mode().await?;
            state.restate_goal().await?;
            state.restate_directories().await?;
            state.restate_background_notices(removed).await?;
        }
        Ok(SessionRewindResponse {
            event_seq: rewind.event_seq,
            input,
        })
    }
}
