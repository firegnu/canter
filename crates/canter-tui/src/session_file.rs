//! Optional, atomic publication of this terminal's current session.
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
};

use canter_protocol::SessionSnapshot;

pub struct SessionFile {
    path: PathBuf,
    state_dir: PathBuf,
    current: Option<(String, String)>,
}

impl SessionFile {
    /// Check the destination before taking over the terminal, without publishing
    /// anything until a session exists. Relative paths are resolved by the caller.
    pub fn new(path: PathBuf, state_dir: PathBuf) -> Result<Self, String> {
        let file = Self {
            path,
            state_dir,
            current: None,
        };
        let check = || -> io::Result<()> {
            match fs::symlink_metadata(&file.path) {
                Ok(meta) if meta.is_dir() => {
                    return Err(io::Error::other("the destination is a directory"));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            file.temporary()?;
            Ok(())
        };
        check().map_err(|error| file.error(error))?;
        Ok(file)
    }

    fn temporary(&self) -> io::Result<tempfile::NamedTempFile> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("the destination has no parent directory"))?;
        let file = tempfile::NamedTempFile::new_in(parent)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }

    fn error(&self, error: io::Error) -> String {
        format!(
            "Could not write session file {}: {error}",
            self.path.display()
        )
        .replace(['\r', '\n'], " ")
    }

    pub(crate) fn publish(&mut self, snapshot: &SessionSnapshot) -> Result<(), String> {
        // Keep the new identity even if publishing fails, for the next retry.
        self.current = Some((snapshot.id.clone(), snapshot.settings.cwd.clone()));
        self.rewrite()
    }

    pub(crate) fn rewrite(&self) -> Result<(), String> {
        let Some((session, cwd)) = &self.current else {
            return Ok(());
        };
        let write = || -> io::Result<()> {
            let mut file = self.temporary()?;
            serde_json::to_writer(
                &mut file,
                &serde_json::json!({
                    "sessionId": session, "cwd": cwd, "stateDir": self.state_dir,
                }),
            )?;
            file.write_all(b"\n")?;
            file.as_file().sync_all()?;
            file.persist(&self.path).map_err(|error| error.error)?;
            Ok(())
        };
        write().map_err(|error| self.error(error))
    }
}
