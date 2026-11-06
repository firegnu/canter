//! A server-owned preference, separate from handwritten configuration and sessions.
use canter_core::CoreOptions;
use canter_protocol::{ReasoningEffort, SessionSettings};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

#[derive(Clone, Deserialize, Serialize)]
struct Selection {
    model: String,
    reasoning_effort: ReasoningEffort,
    config_model: String,
}

#[derive(Default)]
pub(crate) struct LastModel(Option<Selection>);

impl LastModel {
    pub async fn load(home: &Path, configured: &str) -> Self {
        let path = home.join("last_model.json");
        let read = tokio::task::spawn_blocking(move || match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::from),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        })
        .await;
        let mut selection = match read {
            Ok(Ok(selection)) => Self(selection),
            _ => {
                eprintln!("Could not read last_model.json; using the config.json default.");
                Self::default()
            }
        };
        if selection
            .0
            .as_ref()
            .is_some_and(|value| value.config_model != configured)
        {
            selection.clear(home).await;
        }
        selection
    }

    pub fn resolve(
        &self,
        configured: String,
        models: &[String],
        options: &CoreOptions,
    ) -> (String, ReasoningEffort) {
        if let Some(selection) = &self.0
            && models.contains(&selection.model)
        {
            let reasoning = options.reasoning(&selection.model);
            let effort = if reasoning.supported.contains(&selection.reasoning_effort) {
                selection.reasoning_effort
            } else {
                reasoning.default
            };
            return (selection.model.clone(), effort);
        }
        let effort = options.reasoning(&configured).default;
        (configured, effort)
    }

    pub async fn remember(&mut self, home: &Path, configured: String, settings: &SessionSettings) {
        self.0 = Some(Selection {
            model: settings.model.clone(),
            reasoning_effort: settings.reasoning_effort,
            config_model: configured,
        });
        self.save(home).await;
    }

    pub async fn clear(&mut self, home: &Path) {
        if self.0.take().is_some() {
            self.save(home).await;
        }
    }

    async fn save(&self, home: &Path) {
        let home = home.to_path_buf();
        let selection = self.0.clone();
        let saved = tokio::task::spawn_blocking(move || -> io::Result<()> {
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(home.join("last_model.lock"))?;
            lock.lock()?;
            let path = home.join("last_model.json");
            if let Some(selection) = selection {
                let mut file = tempfile::NamedTempFile::new_in(&home)?;
                file.as_file()
                    .set_permissions(fs::Permissions::from_mode(0o600))?;
                serde_json::to_writer_pretty(&mut file, &selection)?;
                file.write_all(b"\n")?;
                file.as_file().sync_all()?;
                file.persist(path)?;
            } else if let Err(error) = fs::remove_file(path)
                && error.kind() != io::ErrorKind::NotFound
            {
                return Err(error);
            }
            Ok(())
        })
        .await;
        if !matches!(saved, Ok(Ok(()))) {
            eprintln!("Could not save last_model.json; keeping the selection in memory only.");
        }
    }
}
