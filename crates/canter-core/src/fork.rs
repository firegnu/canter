//! Read-only inheritance. Existing records are projected into a new session;
//! only the fork's environment notice adds a private history record kind.
use crate::{
    CoreError, CoreOptions, RuntimeContext, Session, context::HistoryEntry, session::owned,
};
use canter_model::{Message, Model};
use canter_protocol::*;
use canter_store::Store;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    sync::Arc,
};

pub struct ForkSnapshot {
    pub settings: SessionSettings,
    events: Vec<(EventPayload, i64)>,
    history: Vec<HistoryEntry>,
    images: BTreeMap<String, Vec<u8>>,
    created_at: i64,
}

pub(crate) fn is_notice(message: &Message) -> bool {
    matches!(message, Message::User { text } if text.starts_with("This session was forked from another session. Its scratch directory is now "))
}

impl crate::session::State {
    pub(crate) async fn restate_fork_notice(&mut self) -> Result<(), CoreError> {
        if let Some(message) = self.fork_notice.clone() {
            self.journal
                .record(HistoryEntry::ForkNotice {
                    message: message.clone(),
                })
                .await?;
            self.plan_mode_notices.push(message);
            crate::context::flush_plan_mode_notices(&mut self.history, &mut self.plan_mode_notices);
        }
        Ok(())
    }
}

impl Session {
    /// Hold Core's state lock through both disk reads, including cancellation.
    /// The HTTP caller also holds the source's session operation lock.
    pub async fn fork_snapshot(&self) -> Result<ForkSnapshot, CoreError> {
        let state = self.shared.state.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            ForkSnapshot::read(&state.journal.store, &state.journal.snapshot.id)
        })
        .await
        .map_err(|_| CoreError::internal())?
    }
}

impl ForkSnapshot {
    /// For an unopened source; callers must exclude concurrent open/trash.
    pub fn read(store: &Store, id: &str) -> Result<Self, CoreError> {
        let events = store.events_after(id, 0)?;
        let first = events
            .first()
            .ok_or_else(|| CoreError::new(ErrorCode::SessionNotFound, "session not found"))?;
        let created_at = first.timestamp;
        let settings = events
            .iter()
            .rev()
            .find_map(|event| match &event.payload {
                EventPayload::SessionUpdated { settings } => Some(settings.clone()),
                _ => None,
            })
            .ok_or_else(CoreError::internal)?;
        let ended: BTreeSet<_> = events
            .iter()
            .filter_map(|event| match &event.payload {
                EventPayload::TurnCompleted { turn } => Some(turn.id.as_str()),
                EventPayload::TurnRetracted { turn_id } => Some(turn_id.as_str()),
                _ => None,
            })
            .collect();
        let entries = store.model_history::<HistoryEntry>(id)?;
        let unfinished = events.iter().find_map(|event| match &event.payload {
            EventPayload::TurnStarted { turn, .. } if !ended.contains(turn.id.as_str()) => {
                Some(turn.id.as_str())
            }
            _ => None,
        });
        if let Some(turn) = unfinished
            && !entries.iter().any(
                |entry| matches!(entry, HistoryEntry::TurnBoundary { turn_id } if turn_id == turn),
            )
        {
            return Err(CoreError::new(
                ErrorCode::Conflict,
                "Cannot locate the unfinished turn's saved history boundary",
            ));
        }
        let mut history = Vec::new();
        let mut boundaries: Vec<(String, usize)> = Vec::new();
        let mut context = None;
        for entry in entries {
            let rewind_to = match &entry {
                HistoryEntry::Rewind { rewind } if rewind.conversation => {
                    Some(rewind.before_turn_id.as_str())
                }
                HistoryEntry::Retract { turn_id, .. } => Some(turn_id.as_str()),
                _ => None,
            };
            if let Some(turn) = rewind_to {
                if let Some(index) = boundaries.iter().position(|(id, _)| id == turn) {
                    history.truncate(boundaries[index].1);
                    boundaries.truncate(index);
                }
                continue;
            }
            match entry {
                HistoryEntry::TurnBoundary { turn_id } => {
                    if !ended.contains(turn_id.as_str()) {
                        break;
                    }
                    boundaries.push((turn_id, history.len()));
                }
                HistoryEntry::Context { .. } => context = Some(entry),
                HistoryEntry::FileCheckpoint { .. }
                | HistoryEntry::FileAborted { .. }
                | HistoryEntry::Rewind { .. }
                | HistoryEntry::Retract { .. }
                | HistoryEntry::CompactionRequest { .. }
                | HistoryEntry::Unknown => {}
                HistoryEntry::Compaction { .. } => {
                    boundaries.clear();
                    history.push(entry);
                }
                HistoryEntry::BackgroundNotice { message, .. } => {
                    history.push(HistoryEntry::BackgroundNotice {
                        message,
                        event_id: None,
                        command_id: None,
                        visible: false,
                    })
                }
                entry => history.push(entry),
            }
        }
        if let Some(context) = context {
            history.insert(0, context);
        }
        let mut copied = Vec::new();
        let mut requests = BTreeSet::new();
        for event in &events {
            if matches!(&event.payload, EventPayload::TurnStarted { turn, .. } if !ended.contains(turn.id.as_str()))
            {
                break;
            }
            let payload = match &event.payload {
                EventPayload::TurnStarted { turn, .. } => {
                    let mut turn = turn.clone();
                    turn.usage = None;
                    EventPayload::TurnStarted {
                        turn,
                        input_id: None,
                    }
                }
                EventPayload::TurnCompleted { turn } => {
                    let mut turn = turn.clone();
                    turn.usage = None;
                    EventPayload::TurnCompleted { turn }
                }
                EventPayload::RequestOpened { request } => {
                    requests.insert(request.id.clone());
                    event.payload.clone()
                }
                EventPayload::RequestResolved { request_id, .. }
                    if requests.contains(request_id) =>
                {
                    event.payload.clone()
                }
                EventPayload::ItemStarted { .. }
                | EventPayload::ItemDelta { .. }
                | EventPayload::ItemCompleted { .. }
                | EventPayload::SessionRewound { .. }
                | EventPayload::TurnRetracted { .. }
                | EventPayload::Warning { .. }
                | EventPayload::Error { .. } => event.payload.clone(),
                _ => continue,
            };
            copied.push((payload, event.timestamp));
        }
        let mut names = BTreeSet::new();
        image_names(
            &serde_json::to_value(&copied).map_err(|_| CoreError::internal())?,
            &mut names,
        );
        image_names(
            &serde_json::to_value(&history).map_err(|_| CoreError::internal())?,
            &mut names,
        );
        let mut images = BTreeMap::new();
        for name in names {
            let path = store.image_path(id, &name)?;
            match std::fs::read(path) {
                Ok(bytes) => {
                    images.insert(name, bytes);
                }
                // Preserve the ordinary "image unavailable" behavior for old missing copies.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(CoreError::internal()),
            }
        }
        Ok(Self {
            settings,
            events: copied,
            history,
            images,
            created_at,
        })
    }

    pub async fn create(
        mut self,
        store: Arc<Store>,
        id: String,
        runtime: RuntimeContext,
        model: Arc<dyn Model>,
        options: CoreOptions,
    ) -> Result<(Session, SessionCreateResponse), CoreError> {
        owned(async move {
            self.settings.name = self
                .settings
                .name
                .filter(|name| !name.is_empty())
                .map(|name| format!("{name} (fork)"));
            let directory = store.session_dir(&id)?;
            let saved_store = store.clone();
            let saved_id = id.clone();
            tokio::task::spawn_blocking(move || {
                let mut writer = saved_store.create(&saved_id, self.settings, self.created_at)?;
                let result = (|| {
                    writer.append_batch(self.events)?;
                    for entry in self.history {
                        writer.append_model_history(&entry)?;
                    }
                    for (name, bytes) in self.images {
                        let mut file = std::fs::OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .mode(0o600)
                            .open(saved_store.image_path(&saved_id, &name)?)?;
                        file.write_all(&bytes)?;
                        file.sync_all()?;
                    }
                    Ok::<_, canter_store::StoreError>(())
                })();
                drop(writer);
                if result.is_err() {
                    let _ = std::fs::remove_dir_all(saved_store.session_dir(&saved_id)?);
                }
                result.map_err(CoreError::from)
            })
            .await
            .map_err(|_| CoreError::internal())??;
            match Session::open_inner(store, id, runtime, model, options, true).await {
                Ok((session, response)) => Ok((
                    session,
                    SessionCreateResponse {
                        session: response.session,
                        event_seq: response.event_seq,
                    },
                )),
                Err(error) => {
                    tokio::task::spawn_blocking(move || std::fs::remove_dir_all(directory))
                        .await
                        .map_err(|_| CoreError::internal())?
                        .map_err(|_| CoreError::internal())?;
                    Err(error)
                }
            }
        })
        .await
    }
}

fn image_names(value: &serde_json::Value, names: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(fields) => {
            if matches!(
                fields.get("type").and_then(|value| value.as_str()),
                Some("image" | "localImage")
            ) && let Some(path) = fields.get("path").and_then(|value| value.as_str())
            {
                names.insert(path.into());
            }
            for value in fields.values() {
                image_names(value, names);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                image_names(value, names);
            }
        }
        _ => {}
    }
}
