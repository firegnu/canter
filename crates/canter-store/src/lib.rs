//! Synchronous, Unix session storage. Callers choose an isolated root directory.
//! Keep a `SessionWriter` alive for the duration of a writable session.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use canter_protocol::{
    Event, EventPayload, ModelRequestTelemetry, ReduceError, SessionSettings, SessionSnapshot,
    SessionSummary, apply_event_mut, validate_event,
};
use serde::{Serialize, de::DeserializeOwned};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("invalid session id (use 1–128 ASCII letters, digits, hyphens or underscores)")]
    InvalidId,
    #[error("session is already open for writing")]
    Locked,
    #[error("storage I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("invalid stored JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid event history: {0}")]
    Replay(#[from] ReduceError),
    #[error("an earlier write failed; close and reopen the session before writing again")]
    WriteFailed,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    /// Creates the caller-selected root with mode 0700, or tightens its permissions.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let root = root.into();
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        Ok(Self { root })
    }

    pub fn create(
        &self,
        id: &str,
        settings: SessionSettings,
        timestamp: i64,
    ) -> Result<SessionWriter, StoreError> {
        self.create_with_sync(id, settings, timestamp, |path| File::open(path)?.sync_all())
    }

    fn create_with_sync(
        &self,
        id: &str,
        settings: SessionSettings,
        timestamp: i64,
        mut sync: impl FnMut(&std::path::Path) -> io::Result<()>,
    ) -> Result<SessionWriter, StoreError> {
        let destination = self.session_dir(id)?;
        if destination.try_exists()? {
            return Err(
                io::Error::new(io::ErrorKind::AlreadyExists, "session already exists").into(),
            );
        }
        let staging = tempfile::Builder::new()
            .prefix(".creating-")
            .tempdir_in(&self.root)?;
        let dir = staging.path();
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("events.jsonl"))?;
        lock(&file)?;
        let model_history = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("model-history.jsonl"))?;
        model_history.sync_all()?;
        let mut writer = SessionWriter {
            file,
            model_history,
            session_dir: destination.clone(),
            private_telemetry: None,
            snapshot: SessionSnapshot::empty(id),
            write_failed: false,
        };
        writer.append(EventPayload::SessionUpdated { settings }, timestamp)?;
        sync(dir)?;
        // Published session directories are nonempty: a competing create's
        // rename cannot overwrite a successfully published session.
        fs::rename(dir, &destination)?;
        if let Err(error) = sync(&self.root) {
            fs::remove_dir_all(&destination)?;
            return Err(error.into());
        }
        Ok(writer)
    }

    pub fn open(&self, id: &str) -> Result<SessionWriter, StoreError> {
        let dir = self.session_dir(id)?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;
        lock(&file)?;
        let (snapshot, committed_bytes) = replay(&file, id)?;
        if snapshot.last_event_seq == 0 {
            return Err(
                io::Error::new(io::ErrorKind::NotFound, "session has no committed events").into(),
            );
        }
        let model_history = OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .mode(0o600)
            .open(dir.join("model-history.jsonl"))?;
        let model_bytes = read_jsonl(&model_history, |_: serde_json::Value| Ok(()))?;
        truncate_tail(&file, committed_bytes)?;
        truncate_tail(&model_history, model_bytes)?;
        Ok(SessionWriter {
            file,
            model_history,
            session_dir: dir,
            private_telemetry: None,
            snapshot,
            write_failed: false,
        })
    }

    pub fn list(&self) -> Result<Vec<SessionSummary>, StoreError> {
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(dir) = self.session_dir(&id) else {
                continue;
            };
            let Ok(file) = File::open(dir.join("events.jsonl")) else {
                continue;
            };
            let Ok((snapshot, _)) = replay(&file, &id) else {
                continue;
            };
            if snapshot.last_event_seq > 0 {
                sessions.push(snapshot.summary());
            }
        }
        sessions.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(sessions)
    }

    /// Read committed events strictly after `seq`, without acquiring the writer lock.
    pub fn events_after(&self, id: &str, seq: u64) -> Result<Vec<Event>, StoreError> {
        let file = File::open(self.session_dir(id)?.join("events.jsonl"))?;
        let mut events = Vec::new();
        read_events(&file, id, |event| {
            if event.seq > seq {
                events.push(event);
            }
            Ok(())
        })?;
        Ok(events)
    }

    /// Read private model history using the caller's model-layer type.
    pub fn model_history<T: DeserializeOwned>(&self, id: &str) -> Result<Vec<T>, StoreError> {
        let file = File::open(self.session_dir(id)?.join("model-history.jsonl"))?;
        let mut entries = Vec::new();
        read_jsonl(&file, |entry| {
            entries.push(entry);
            Ok(())
        })?;
        Ok(entries)
    }

    /// Read optional private telemetry without a writer lock or repairing the file.
    /// Bound the read to its initial size; ignore damaged and uncommitted lines.
    pub fn private_telemetry(&self, id: &str) -> Result<Vec<ModelRequestTelemetry>, StoreError> {
        use std::io::Read;
        let file = match File::open(self.session_dir(id)?.join("private-telemetry.jsonl")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let length = file.metadata()?.len();
        let mut reader = BufReader::new(file.take(length));
        let mut line = Vec::new();
        let mut records = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 || line.last() != Some(&b'\n') {
                break;
            }
            if let Ok(record) = serde_json::from_slice(&line) {
                records.push(record);
            }
        }
        Ok(records)
    }

    fn session_dir(&self, id: &str) -> Result<PathBuf, StoreError> {
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(StoreError::InvalidId);
        }
        Ok(self.root.join(id))
    }

    /// Resolve a saved rewind filename within its owning session directory.
    pub fn rewind_path(&self, id: &str, name: &str) -> Result<PathBuf, StoreError> {
        if !name.starts_with("rewind-")
            || std::path::Path::new(name)
                .file_name()
                .and_then(|s| s.to_str())
                != Some(name)
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "invalid rewind filename").into(),
            );
        }
        Ok(self.session_dir(id)?.join(name))
    }

    /// Save a private file checkpoint before the tool may change the workspace.
    pub fn save_rewind(&self, id: &str, bytes: &[u8]) -> Result<String, StoreError> {
        let dir = self.session_dir(id)?;
        let mut file = tempfile::Builder::new()
            .prefix("rewind-")
            .tempfile_in(&dir)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        let (_, path) = file.keep().map_err(|error| error.error)?;
        File::open(dir)?.sync_all()?;
        Ok(path
            .file_name()
            .expect("temporary filename")
            .to_string_lossy()
            .into_owned())
    }

    /// Resolve a saved image filename within its owning session directory.
    pub fn image_path(&self, id: &str, name: &str) -> Result<PathBuf, StoreError> {
        let mut components = std::path::Path::new(name).components();
        if !name.starts_with("image-")
            || !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "invalid image filename").into(),
            );
        }
        Ok(self.session_dir(id)?.join(name))
    }

    /// Publish original attachment bytes before their referencing events/history.
    pub fn save_image(
        &self,
        id: &str,
        bytes: &[u8],
        extension: &str,
    ) -> Result<PathBuf, StoreError> {
        let dir = self.session_dir(id)?;
        let mut file = tempfile::Builder::new()
            .prefix("image-")
            .suffix(&format!(".{extension}"))
            .tempfile_in(&dir)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        File::open(dir)?.sync_all()?;
        let (_, path) = file.keep().map_err(|error| error.error)?;
        Ok(path)
    }
}

fn lock(file: &File) -> Result<(), StoreError> {
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => StoreError::Locked,
        fs::TryLockError::Error(error) => StoreError::Io(error),
    })
}

fn truncate_tail(file: &File, committed_bytes: u64) -> Result<(), StoreError> {
    if file.metadata()?.len() != committed_bytes {
        // Keep the locked inode; discard only an unterminated final line.
        file.set_len(committed_bytes)?;
        file.sync_data()?;
    }
    Ok(())
}

/// Telemetry is append-only metadata: old records need not match this version's schema.
fn private_committed_bytes(file: &File) -> io::Result<u64> {
    let mut end = file.metadata()?.len();
    let mut buffer = [0; 8192];
    while end > 0 {
        let length = end.min(buffer.len() as u64) as usize;
        let start = end - length as u64;
        file.read_exact_at(&mut buffer[..length], start)?;
        if let Some(index) = buffer[..length].iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + index as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

fn replay(file: &File, id: &str) -> Result<(SessionSnapshot, u64), StoreError> {
    let mut snapshot = SessionSnapshot::empty(id);
    let committed_bytes = read_events(file, id, |event| {
        apply_event_mut(&mut snapshot, &event)?;
        Ok(())
    })?;
    Ok((snapshot, committed_bytes))
}

fn read_events(
    file: &File,
    id: &str,
    mut visit: impl FnMut(Event) -> Result<(), StoreError>,
) -> Result<u64, StoreError> {
    let mut last_seq: u64 = 0;
    read_jsonl(file, |event: Event| {
        if event.session_id != id {
            return Err(ReduceError::WrongSession.into());
        }
        let expected = last_seq
            .checked_add(1)
            .ok_or(ReduceError::SequenceExhausted)?;
        if event.seq != expected {
            return Err(ReduceError::Sequence {
                expected,
                actual: event.seq,
            }
            .into());
        }
        last_seq = event.seq;
        visit(event)
    })
}

fn read_jsonl<T: DeserializeOwned>(
    file: &File,
    mut visit: impl FnMut(T) -> Result<(), StoreError>,
) -> Result<u64, StoreError> {
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut committed_bytes = 0;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        visit(serde_json::from_slice(&line)?)?;
        committed_bytes += read as u64;
    }
    Ok(committed_bytes)
}

pub struct SessionWriter {
    file: File,
    model_history: File,
    session_dir: PathBuf,
    private_telemetry: Option<File>,
    snapshot: SessionSnapshot,
    write_failed: bool,
}

impl SessionWriter {
    /// Private request metadata is durable but never advances the public event
    /// cursor or session activity time. The events file lock also owns this file.
    pub fn append_private_telemetry(
        &mut self,
        records: &[ModelRequestTelemetry],
    ) -> Result<(), StoreError> {
        if self.write_failed {
            return Err(StoreError::WriteFailed);
        }
        if records.is_empty() {
            return Ok(());
        }
        let result = (|| {
            let opened = self.private_telemetry.is_none();
            if self.private_telemetry.is_none() {
                let file = OpenOptions::new()
                    .read(true)
                    .append(true)
                    .create(true)
                    .mode(0o600)
                    .open(self.session_dir.join("private-telemetry.jsonl"))?;
                let committed = private_committed_bytes(&file)?;
                truncate_tail(&file, committed)?;
                self.private_telemetry = Some(file);
            }
            let mut lines = Vec::new();
            for record in records {
                serde_json::to_writer(&mut lines, record)?;
                lines.push(b'\n');
            }
            let file = self.private_telemetry.as_mut().expect("opened above");
            file.write_all(&lines)?;
            file.sync_data()?;
            if opened {
                File::open(&self.session_dir)?.sync_all()?;
            }
            Ok(())
        })();
        if result.is_err() {
            // Only this metadata batch is lost; reopen on the next attempt to
            // repair a torn tail without poisoning the main session writer.
            self.private_telemetry = None;
        }
        result
    }

    /// Append one model-layer record, without producing a public event.
    pub fn append_model_history<T: Serialize>(&mut self, entry: &T) -> Result<(), StoreError> {
        if self.write_failed {
            return Err(StoreError::WriteFailed);
        }
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        if let Err(error) = self
            .model_history
            .write_all(&line)
            .and_then(|()| self.model_history.sync_data())
        {
            self.write_failed = true;
            return Err(error.into());
        }
        Ok(())
    }

    pub fn append_batch(
        &mut self,
        entries: impl IntoIterator<Item = (EventPayload, i64)>,
    ) -> Result<Vec<Event>, StoreError> {
        if self.write_failed {
            return Err(StoreError::WriteFailed);
        }
        let mut entries = entries.into_iter().peekable();
        if entries.peek().is_none() {
            return Ok(Vec::new());
        }
        // One staging copy for the entire batch, never one per delta.
        let mut next = self.snapshot.clone();
        let mut events = Vec::new();
        let mut lines = Vec::new();
        for (payload, timestamp) in entries {
            let event = Event {
                seq: next
                    .last_event_seq
                    .checked_add(1)
                    .ok_or(ReduceError::SequenceExhausted)?,
                session_id: next.id.clone(),
                timestamp,
                payload,
            };
            apply_event_mut(&mut next, &event)?;
            serde_json::to_writer(&mut lines, &event)?;
            lines.push(b'\n');
            events.push(event);
        }
        self.write_events(&lines)?;
        self.snapshot = next;
        Ok(events)
    }

    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    /// Assigns the next sequence, durably appends one line, then updates memory.
    /// After an I/O error this handle refuses further writes; reopen to recover.
    pub fn append(&mut self, payload: EventPayload, timestamp: i64) -> Result<Event, StoreError> {
        if self.write_failed {
            return Err(StoreError::WriteFailed);
        }
        let seq = self
            .snapshot
            .last_event_seq
            .checked_add(1)
            .ok_or(ReduceError::SequenceExhausted)?;
        let event = Event {
            seq,
            session_id: self.snapshot.id.clone(),
            timestamp,
            payload,
        };
        validate_event(&self.snapshot, &event)?;
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        self.write_events(&line)?;
        apply_event_mut(&mut self.snapshot, &event)?;
        Ok(event)
    }

    fn write_events(&mut self, bytes: &[u8]) -> Result<(), StoreError> {
        if let Err(error) = self
            .file
            .write_all(bytes)
            .and_then(|()| self.file.sync_data())
        {
            self.write_failed = true;
            return Err(error.into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod creation_tests {
    use super::*;

    #[test]
    fn failed_creation_cleans_up_before_and_after_publication() {
        for fail_at in 1..=2 {
            let temp = tempfile::tempdir().unwrap();
            let store = Store::new(temp.path()).unwrap();
            let settings = SessionSettings {
                tags: Default::default(),
                additional_directories: Vec::new(),
                plan_mode: false,
                pre_plan_approval_policy: None,
                plan_enabled: None,
                sandbox_enabled: None,
                cwd: "/work".into(),
                name: None,
                model: "fake".into(),
                reasoning_effort: Default::default(),
                approval_policy: canter_protocol::PermissionMode::AlwaysAsk,
            };
            let mut syncs = 0;
            let result = store.create_with_sync("s1", settings.clone(), 1, |path| {
                syncs += 1;
                assert_eq!(
                    temp.path().join("s1").exists(),
                    path == temp.path(),
                    "publish only after all session files are durable"
                );
                if syncs == fail_at {
                    return Err(io::Error::other("injected filesystem sync failure"));
                }
                File::open(path)?.sync_all()
            });
            assert!(result.is_err());
            assert!(
                !temp.path().join("s1").exists(),
                "a failed creation must not reserve its id"
            );
            assert_eq!(
                fs::read_dir(temp.path()).unwrap().count(),
                0,
                "temporary creation data must be removed too"
            );
            assert!(store.create("s1", settings, 2).is_ok());
        }
    }
}
