//! Synchronous, Unix session storage. Callers choose an isolated root directory.
//! Keep a `SessionWriter` alive for the duration of a writable session.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use canter_protocol::{
    Event, EventPayload, ReduceError, SessionSettings, SessionSnapshot, SessionSummary, apply_event,
};
use serde::{Deserialize, Serialize};

/// Local execution context. Deliberately has no Debug, protocol schema, or TS export.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrivateSessionConfig {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
}

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
        private: PrivateSessionConfig,
        timestamp: i64,
    ) -> Result<SessionWriter, StoreError> {
        let dir = self.session_dir(id)?;
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("events.jsonl"))?;
        lock(&file)?;
        let mut config = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(dir.join("private.json"))?;
        serde_json::to_writer(&mut config, &private)?;
        config.sync_all()?;
        let mut writer = SessionWriter {
            file,
            snapshot: SessionSnapshot::empty(id),
            private,
            write_failed: false,
        };
        writer.append(EventPayload::SessionUpdated { settings }, timestamp)?;
        File::open(&dir)?.sync_all()?;
        File::open(&self.root)?.sync_all()?;
        Ok(writer)
    }

    pub fn open(&self, id: &str) -> Result<SessionWriter, StoreError> {
        let dir = self.session_dir(id)?;
        let file = OpenOptions::new()
            .read(true)
            .append(true)
            .open(dir.join("events.jsonl"))?;
        lock(&file)?;
        let private = serde_json::from_reader(File::open(dir.join("private.json"))?)?;
        let (snapshot, committed_bytes) = replay(&file, id)?;
        if file.metadata()?.len() != committed_bytes {
            // Only an unterminated tail is discarded, with the writer lock held.
            // Keep the same inode so the lock continues to protect future appends.
            file.set_len(committed_bytes)?;
            file.sync_data()?;
        }
        Ok(SessionWriter {
            file,
            snapshot,
            private,
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
            let dir = self.session_dir(&id)?;
            let file = match File::open(dir.join("events.jsonl")) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let (snapshot, _) = replay(&file, &id)?;
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
}

fn lock(file: &File) -> Result<(), StoreError> {
    file.try_lock().map_err(|error| match error {
        fs::TryLockError::WouldBlock => StoreError::Locked,
        fs::TryLockError::Error(error) => StoreError::Io(error),
    })
}

fn replay(file: &File, id: &str) -> Result<(SessionSnapshot, u64), StoreError> {
    let mut snapshot = SessionSnapshot::empty(id);
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut committed_bytes = 0;
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 || line.last() != Some(&b'\n') {
            break;
        }
        let event: Event = serde_json::from_slice(&line)?;
        let expected = snapshot
            .last_event_seq
            .checked_add(1)
            .ok_or(ReduceError::SequenceExhausted)?;
        if event.seq != expected {
            return Err(ReduceError::Sequence {
                expected,
                actual: event.seq,
            }
            .into());
        }
        snapshot = apply_event(&snapshot, &event)?;
        committed_bytes += read as u64;
    }
    Ok((snapshot, committed_bytes))
}

pub struct SessionWriter {
    file: File,
    snapshot: SessionSnapshot,
    private: PrivateSessionConfig,
    write_failed: bool,
}

impl SessionWriter {
    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn private_config(&self) -> &PrivateSessionConfig {
        &self.private
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
        let next = apply_event(&self.snapshot, &event)?;
        let mut line = serde_json::to_vec(&event)?;
        line.push(b'\n');
        if let Err(error) = self
            .file
            .write_all(&line)
            .and_then(|()| self.file.sync_data())
        {
            self.write_failed = true;
            return Err(error.into());
        }
        self.snapshot = next;
        Ok(event)
    }
}
