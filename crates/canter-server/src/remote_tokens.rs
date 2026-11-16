//! Local token administration. Only SHA-256 digests are persisted.
use std::{
    collections::HashSet,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    digest,
    rand::{SecureRandom, SystemRandom},
};
use serde::{Deserialize, Serialize};

use crate::ServerError;

const MAX_BYTES: u64 = 1_048_576;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Record {
    pub name: String,
    pub sha256: String,
    pub created: String,
}

pub(crate) fn hash(token: &str) -> String {
    digest::digest(&digest::SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.chars().any(char::is_control)
        && name.trim() == name
}

pub(crate) fn read(home: &Path) -> Result<Vec<Record>, ServerError> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(home.join("remote-tokens.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(ServerError("cannot read remote-tokens.json".into())),
    };
    let metadata = file
        .metadata()
        .map_err(|_| ServerError("cannot inspect remote-tokens.json".into()))?;
    if !metadata.is_file()
        || metadata.uid() != nix::unistd::Uid::effective().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(ServerError(
            "remote-tokens.json must be a regular file owned by this user with permissions 0600"
                .into(),
        ));
    }
    let mut contents = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|_| ServerError("cannot read remote-tokens.json".into()))?;
    let invalid = || ServerError("remote-tokens.json is invalid".into());
    if contents.len() as u64 > MAX_BYTES {
        return Err(invalid());
    }
    let records: Vec<Record> = serde_json::from_slice(&contents).map_err(|_| invalid())?;
    let mut names = HashSet::new();
    let mut hashes = HashSet::new();
    for record in &records {
        if !valid_name(&record.name)
            || !names.insert(&record.name)
            || record.sha256.len() != 64
            || !record
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !hashes.insert(&record.sha256)
            || chrono::DateTime::parse_from_rfc3339(&record.created).is_err()
        {
            return Err(invalid());
        }
    }
    Ok(records)
}

fn lock(home: &Path) -> Result<File, ServerError> {
    crate::lifecycle::private_dir(home)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(home.join("remote-tokens.lock"))
        .map_err(|_| ServerError("cannot open remote token lock".into()))?;
    file.try_lock()
        .map_err(|_| ServerError("another remote token command is running; try again".into()))?;
    Ok(file)
}

fn save(home: &Path, records: &[Record]) -> Result<(), ServerError> {
    let failed = || ServerError("cannot save remote-tokens.json".into());
    let mut file = tempfile::NamedTempFile::new_in(home).map_err(|_| failed())?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| failed())?;
    serde_json::to_writer_pretty(&mut file, records).map_err(|_| failed())?;
    file.write_all(b"\n").map_err(|_| failed())?;
    if file.as_file().metadata().map_err(|_| failed())?.len() > MAX_BYTES {
        return Err(ServerError(
            "remote-tokens.json exceeds the 1 MiB limit; no changes saved".into(),
        ));
    }
    file.as_file().sync_all().map_err(|_| failed())?;
    file.persist(home.join("remote-tokens.json"))
        .map_err(|_| failed())?;
    File::open(home)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| failed())
}

/// Generate a 256-bit token, returned only after its digest is safely saved.
pub fn add(home: &Path, name: &str) -> Result<String, ServerError> {
    if !valid_name(name) {
        return Err(ServerError(
            "client name must be 1–128 bytes without control characters or surrounding whitespace"
                .into(),
        ));
    }
    let _lock = lock(home)?;
    let mut records = read(home)?;
    if records.iter().any(|record| record.name == name) {
        return Err(ServerError(
            "a token with this client name already exists".into(),
        ));
    }
    let mut random = [0; 32];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| ServerError("cannot generate a remote token".into()))?;
    let token = URL_SAFE_NO_PAD.encode(random);
    records.push(Record {
        name: name.into(),
        sha256: hash(&token),
        created: chrono::Utc::now().to_rfc3339(),
    });
    save(home, &records)?;
    Ok(token)
}

/// List client names and creation times; never return tokens or digests.
pub fn list(home: &Path) -> Result<Vec<(String, String)>, ServerError> {
    Ok(read(home)?
        .into_iter()
        .map(|record| (record.name, record.created))
        .collect())
}

pub fn remove(home: &Path, name: &str) -> Result<(), ServerError> {
    let _lock = lock(home)?;
    let mut records = read(home)?;
    let before = records.len();
    records.retain(|record| record.name != name);
    if records.len() == before {
        return Err(ServerError("no token with this client name exists".into()));
    }
    save(home, &records)
}
