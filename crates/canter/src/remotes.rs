//! Client-owned named connections. Tokens never appear in diagnostics or lists.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};

use serde::{Deserialize, Serialize};

const LIMIT: u64 = 1_048_576;

#[derive(Clone, Deserialize, Serialize)]
pub struct Remote {
    pub name: String,
    pub url: String,
    pub token: String,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

pub fn read(home: &Path) -> Result<Vec<Remote>, String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(home.join("remotes.json"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("cannot read remotes.json".into()),
    };
    let meta = file.metadata().map_err(|_| "cannot inspect remotes.json")?;
    if !meta.is_file()
        || meta.uid() != nix::unistd::Uid::effective().as_raw()
        || meta.permissions().mode() & 0o777 != 0o600
    {
        return Err(
            "remotes.json must be a regular file owned by this user with permissions 0600".into(),
        );
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read remotes.json")?;
    if bytes.len() as u64 > LIMIT {
        return Err("remotes.json exceeds the 1 MiB limit".into());
    }
    let records: Vec<Remote> =
        serde_json::from_slice(&bytes).map_err(|_| "remotes.json is invalid")?;
    let mut names = std::collections::HashSet::new();
    for record in &records {
        if !valid_name(&record.name) || !names.insert(&record.name) || !valid_token(&record.token) {
            return Err("remotes.json is invalid".into());
        }
        canter_tui::client::remote_url(&record.url).map_err(|error| error.to_string())?;
    }
    Ok(records)
}

pub fn find(home: &Path, name: &str) -> Result<Remote, String> {
    read(home)?
        .into_iter()
        .find(|remote| remote.name == name)
        .ok_or_else(|| format!("Unknown server '{name}'; use canter remote add"))
}

impl Remote {
    pub async fn connect(
        &self,
    ) -> Result<(canter_tui::Connected, canter_protocol::InfoResponse), canter_tui::ConnectError>
    {
        let client = canter_tui::client::Client::remote(&self.name, &self.url, &self.token)?;
        let info = tokio::time::timeout(std::time::Duration::from_secs(10), client.info())
            .await
            .map_err(|_| format!("Server '{}' did not respond", self.name))?
            .map_err(|error| {
                let mut error = canter_tui::ConnectError::from(error);
                error.message = format!("Cannot connect to server '{}': {error}", self.name);
                error
            })?;
        let client = client.with_server_version(info.server_version.clone());
        if info.protocol_version != canter_protocol::PROTOCOL_VERSION {
            return Err(canter_tui::ConnectError {
                message: client.upgrade_hint(""),
                retryable: false,
            });
        }
        let warning = (info.server_version != crate::VERSION).then(|| client.upgrade_hint(""));
        Ok((
            canter_tui::Connected {
                client,
                warning,
                server: info.instance.clone(),
            },
            info,
        ))
    }
}

fn valid_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 8192 && token.bytes().all(|b| b.is_ascii_graphic())
}

/// Checks what can be checked before the token is read, so a bad name or URL
/// fails before anyone pastes a secret.
pub fn check_new(home: &Path, name: &str, url: &str) -> Result<String, String> {
    if !valid_name(name) {
        return Err(
            "server name must be 1–128 bytes without control characters or surrounding whitespace"
                .into(),
        );
    }
    let url = canter_tui::client::remote_url(url)
        .map_err(|error| error.to_string())?
        .to_string();
    if read(home)?.iter().any(|r| r.name == name) {
        return Err("a server with this name already exists".into());
    }
    Ok(url)
}

pub fn add(home: &Path, name: String, url: String, input: impl Read) -> Result<(), String> {
    let url = check_new(home, &name, &url)?;
    let mut token = String::new();
    input
        .take(8195)
        .read_to_string(&mut token)
        .map_err(|_| "cannot read token from stdin")?;
    if token.len() > 8194 {
        return Err("stdin token is too long".into());
    }
    let token = token.trim_end_matches(['\r', '\n']).to_owned();
    if !valid_token(&token) {
        return Err("stdin must contain one non-empty token (at most 8192 bytes)".into());
    }
    update(home, |records| {
        if records.iter().any(|r| r.name == name) {
            return Err("a server with this name already exists".into());
        }
        records.push(Remote { name, url, token });
        Ok(())
    })
}

pub fn remove(home: &Path, name: &str) -> Result<(), String> {
    update(home, |records| {
        let before = records.len();
        records.retain(|r| r.name != name);
        if records.len() == before {
            return Err("no server with this name exists".into());
        }
        Ok(())
    })
}

fn update(
    home: &Path,
    edit: impl FnOnce(&mut Vec<Remote>) -> Result<(), String>,
) -> Result<(), String> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home)
        .map_err(|_| "cannot create client state directory")?;
    let meta = fs::symlink_metadata(home).map_err(|_| "cannot inspect client state directory")?;
    if !meta.is_dir() || meta.uid() != nix::unistd::Uid::effective().as_raw() {
        return Err("client state directory must be owned by this user".into());
    }
    fs::set_permissions(home, fs::Permissions::from_mode(0o700))
        .map_err(|_| "cannot make client state directory private")?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(home.join("remotes.lock"))
        .map_err(|_| "cannot open remotes lock")?;
    lock.try_lock()
        .map_err(|_| "another remote command is running; try again")?;
    let mut records = read(home)?;
    edit(&mut records)?;
    let bytes = serde_json::to_vec_pretty(&records).map_err(|_| "cannot encode remotes.json")?;
    if bytes.len() as u64 + 1 > LIMIT {
        return Err("remotes.json exceeds the 1 MiB limit; no changes saved".into());
    }
    let save = || -> std::io::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(home)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(home.join("remotes.json"))?;
        File::open(home)?.sync_all()
    };
    save().map_err(|_| "cannot save remotes.json".into())
}
