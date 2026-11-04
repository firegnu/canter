//! The only module that serializes credentials. No secret-bearing type is
//! generally serializable or printable, including the on-disk representations.
use super::{AuthError, ErrorKind, Record, RecordInfo, RecordStatus, TokenSet};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub(super) struct Store {
    dir: PathBuf,
    _lock: File,
}

fn failure(_: impl Sized) -> AuthError {
    ErrorKind::Storage.into()
}

fn check(metadata: &fs::Metadata, directory: bool) -> Result<(), AuthError> {
    // geteuid has no pointer arguments or failure mode.
    let uid = unsafe { nix::libc::geteuid() };
    let kind_ok = if directory {
        metadata.is_dir()
    } else {
        metadata.is_file()
    };
    if !kind_ok
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o077 != 0
        || (!directory && metadata.nlink() != 1)
    {
        return Err(ErrorKind::Storage.into());
    }
    Ok(())
}

fn directory(path: &Path) -> Result<(), AuthError> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(failure(e)),
    }
    check(&fs::symlink_metadata(path).map_err(failure)?, true)
}

fn open(path: &Path) -> Result<File, AuthError> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path)
        .map_err(failure)?;
    check(&file.metadata().map_err(failure)?, false)?;
    Ok(file)
}

impl Store {
    pub fn load_records(&self, host: &str) -> Result<Vec<Record>, AuthError> {
        #[derive(serde::Deserialize)]
        struct Stored {
            version: u32,
            ext_agent_host_id: String,
            record: RecordInfo,
            tokens: Option<TokenSet>,
        }
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.dir).map_err(failure)? {
            let entry = entry.map_err(failure)?;
            let name = entry.file_name();
            let name = name.to_str().ok_or(ErrorKind::Storage)?;
            if name == "host.json" || name == "auth.lock" || name.starts_with(".tmp") {
                continue;
            }
            check(&fs::symlink_metadata(entry.path()).map_err(failure)?, false)?;
            let id = name.strip_suffix(".json").ok_or(ErrorKind::Storage)?;
            let uuid = uuid::Uuid::parse_str(id).map_err(failure)?;
            if uuid.get_version_num() != 4 || uuid.to_string() != id {
                return Err(ErrorKind::Storage.into());
            }
            let stored: Stored = serde_json::from_value(self.read(name)?).map_err(failure)?;
            if stored.version != 1
                || stored.ext_agent_host_id != host
                || stored.record.id != id
                || stored.record.subject.is_empty()
                || stored.record.client_id.is_empty()
                || stored.record.client_id == "dynamic_agent_client"
                || records
                    .iter()
                    .any(|record: &Record| record.info.client_id == stored.record.client_id)
                || stored.tokens.as_ref().is_some_and(|tokens| {
                    tokens.access.0.is_empty()
                        || tokens.received_at.checked_add(tokens.expires_in).is_none()
                        || tokens.status() != stored.record.status
                })
                || (stored.tokens.is_none()
                    && !matches!(
                        stored.record.status,
                        RecordStatus::NeedsLogin | RecordStatus::SignedOut
                    ))
            {
                return Err(ErrorKind::Storage.into());
            }
            records.push(Record {
                info: stored.record,
                tokens: stored.tokens,
            });
        }
        Ok(records)
    }

    pub fn save_record(&self, record: &Record, host: &str) -> Result<(), AuthError> {
        use serde_json::json;
        let info = &record.info;
        let status = match info.status {
            RecordStatus::LoggedIn => "logged_in",
            RecordStatus::PlanNotAuthorized => "plan_not_authorized",
            RecordStatus::NeedsLogin => "needs_login",
            RecordStatus::SignedOut => "signed_out",
        };
        let tokens = record.tokens.as_ref().map(|tokens| json!({
            "access": tokens.access.0, "refresh": tokens.refresh.as_ref().map(|token| &token.0),
            "id": tokens.id.as_ref().map(|token| &token.0), "scopes": tokens.scopes,
            "received_at": tokens.received_at, "expires_in": tokens.expires_in, "earliest_refresh_at": tokens.earliest_refresh_at,
        }));
        self.write(&format!("{}.json", info.id), &json!({
            "version":1, "ext_agent_host_id":host,
            "record": {"id":info.id,"issuer":info.issuer,"subject":info.subject,"client_id":info.client_id,"email":info.email,"status":status},
            "tokens":tokens,
        }))
    }

    pub fn open(home: &Path) -> Result<(Self, String), AuthError> {
        directory(home)?;
        directory(&home.join("auth"))?;
        let dir = home.join("auth/chatgpt");
        directory(&dir)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
            .open(dir.join("auth.lock"))
            .map_err(failure)?;
        check(&lock.metadata().map_err(failure)?, false)?;
        lock.try_lock().map_err(failure)?;
        let store = Self { dir, _lock: lock };
        let host = match fs::symlink_metadata(store.dir.join("host.json")) {
            Ok(_) => {
                let value = store.read("host.json")?;
                let host = value["ext_agent_host_id"]
                    .as_str()
                    .ok_or(ErrorKind::Storage)?;
                let id = uuid::Uuid::parse_str(host).map_err(failure)?;
                if id.get_version_num() != 4 || id.urn().to_string() != host {
                    return Err(ErrorKind::Storage.into());
                }
                host.to_owned()
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let host = uuid::Uuid::new_v4().urn().to_string();
                store.write("host.json", &serde_json::json!({"ext_agent_host_id": host}))?;
                host
            }
            Err(e) => return Err(failure(e)),
        };
        Ok((store, host))
    }

    fn read(&self, name: &str) -> Result<serde_json::Value, AuthError> {
        let mut bytes = Vec::new();
        open(&self.dir.join(name))?
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(failure)?;
        if bytes.len() > 1024 * 1024 {
            return Err(ErrorKind::Storage.into());
        }
        serde_json::from_slice(&bytes).map_err(failure)
    }

    fn write(&self, name: &str, value: &serde_json::Value) -> Result<(), AuthError> {
        check(&fs::symlink_metadata(&self.dir).map_err(failure)?, true)?;
        let target = self.dir.join(name);
        match fs::symlink_metadata(&target) {
            Ok(metadata) => check(&metadata, false)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(failure(e)),
        }
        // tempfile creates Unix files with 0600 before the first byte is written.
        let mut temp = tempfile::NamedTempFile::new_in(&self.dir).map_err(failure)?;
        check(&temp.as_file().metadata().map_err(failure)?, false)?;
        serde_json::to_writer(&mut temp, value).map_err(failure)?;
        temp.write_all(b"\n")
            .and_then(|()| temp.as_file().sync_all())
            .map_err(failure)?;
        temp.persist(target).map_err(failure)?;
        File::open(&self.dir)
            .and_then(|file| file.sync_all())
            .map_err(failure)
    }
}
