use crate::{CoreError, context::HistoryEntry, session::State};
use canter_model::Message;
use canter_protocol::ErrorCode;
use std::path::{Path, PathBuf};

/// Validate the whole append before changing the session. Existing roots are
/// already canonical; do not remove them when a parent directory is added.
pub(crate) async fn append(
    cwd: String,
    home: Option<PathBuf>,
    mut directories: Vec<String>,
    additions: Vec<String>,
) -> Result<Vec<String>, CoreError> {
    if additions.is_empty() {
        return Ok(directories);
    }
    tokio::task::spawn_blocking(move || {
        let cwd = Path::new(&cwd).canonicalize().map_err(|error| {
            CoreError::new(
                ErrorCode::InvalidRequest,
                &format!("Cannot resolve working directory: {error}"),
            )
        })?;
        for input in additions {
            let invalid = |reason: String| {
                CoreError::new(
                    ErrorCode::InvalidRequest,
                    &format!("Cannot add directory {input:?}: {reason}"),
                )
            };
            if input.is_empty() {
                return Err(invalid("path is empty".into()));
            }
            let path = if input == "~" || input.starts_with("~/") {
                let home = home
                    .as_ref()
                    .ok_or_else(|| invalid("session HOME is not set".into()))?;
                cwd.join(home).join(input.strip_prefix("~/").unwrap_or(""))
            } else {
                cwd.join(&input)
            };
            let path = path
                .canonicalize()
                .map_err(|error| invalid(error.to_string()))?;
            if !path.is_dir() {
                return Err(invalid("not a directory".into()));
            }
            if path.starts_with(&cwd) || directories.iter().any(|dir| path.starts_with(dir)) {
                continue;
            }
            directories.push(
                path.to_str()
                    .ok_or_else(|| invalid("path is not UTF-8".into()))?
                    .to_owned(),
            );
        }
        Ok(directories)
    })
    .await
    .map_err(|_| CoreError::internal())?
}

impl State {
    pub(crate) async fn restate_directories(&mut self) -> Result<(), CoreError> {
        let directories = &self.journal.snapshot.settings.additional_directories;
        if directories.is_empty() {
            return Ok(());
        }
        let message = Message::System {
            text: format!(
                "[Additional directories]\nThese directories are available in this session alongside the working directory:\n{}",
                serde_json::to_string(directories).expect("directory list")
            ),
        };
        self.journal
            .record(HistoryEntry::DirectoriesNotice {
                message: message.clone(),
            })
            .await?;
        self.plan_mode_notices.push(message);
        crate::context::flush_plan_mode_notices(&mut self.history, &mut self.plan_mode_notices);
        Ok(())
    }
}
