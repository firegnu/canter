//! The eval state directory and the files that describe one run.

use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::Result;

pub const DEFAULT_TASKS: &str = "eval/tasks";

/// The eval state directory, separate from canter's own `~/.canter`.
#[derive(Clone, Debug)]
pub struct Home {
    pub root: PathBuf,
}

impl Home {
    pub fn resolve(flag: Option<PathBuf>) -> Result<Self> {
        let root = match flag {
            Some(path) => path,
            None => match env::var_os("CANTER_EVAL_HOME") {
                Some(value) => PathBuf::from(value),
                None => {
                    let home = env::var_os("HOME").ok_or("HOME is not set")?;
                    PathBuf::from(home).join(".canter-eval")
                }
            },
        };
        let root = expand(&root)?;
        if !root.is_absolute() {
            return Err(format!(
                "the eval state directory must be an absolute path: {}",
                root.display()
            ));
        }
        let home = Self { root };
        home.refuse_daily_state()?;
        Ok(home)
    }

    /// Never point eval state at the directory canter itself uses.
    fn refuse_daily_state(&self) -> Result<()> {
        let root = same_path(&self.root);
        let mut candidates = Vec::new();
        if let Some(home) = env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".canter"));
        }
        if let Some(configured) = env::var_os("CANTER_HOME") {
            candidates.push(PathBuf::from(configured));
        }
        for candidate in candidates {
            if same_path(&candidate) == root {
                return Err(format!(
                    "refusing to use {} as the eval state directory: it is canter's own state directory",
                    self.root.display()
                ));
            }
        }
        Ok(())
    }

    pub fn ensure(&self) -> Result<()> {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.root)
            .and_then(|()| fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700)))
            .map_err(|error| format!("cannot create {}: {error}", self.root.display()))
    }

    pub fn local_path(&self) -> PathBuf {
        self.root.join("local.json")
    }

    pub fn config_path(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn server_path(&self) -> PathBuf {
        self.root.join("server.json")
    }

    pub fn session_events(&self, id: &str) -> PathBuf {
        self.root.join("sessions").join(id).join("events.jsonl")
    }

    pub fn round_dir(&self, round: &str) -> PathBuf {
        self.root.join("runs").join(round)
    }

    pub fn run_dir(&self, round: &str, run: &str) -> PathBuf {
        self.round_dir(round).join(run)
    }
}

/// A local-only configuration; `local.json` in the eval state directory.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Local {
    /// Task name to the absolute path of its repository.
    pub repos: BTreeMap<String, String>,
    /// The one model this eval home uses, as `provider/model`.
    pub model: String,
    /// Defaults to `$HOME/.canter/config.json`.
    #[serde(default)]
    pub source_config: Option<String>,
    /// The canter executable used by `collect`; defaults to `canter` on PATH.
    #[serde(default)]
    pub canter: Option<String>,
}

impl Local {
    pub fn load(home: &Home) -> Result<Self> {
        let path = home.local_path();
        let text = fs::read_to_string(&path).map_err(|error| {
            format!(
                "cannot read {}: {error}\n\
                 create it first; see eval/README.md for the format",
                path.display()
            )
        })?;
        serde_json::from_str(&text).map_err(|error| {
            format!(
                "{} is not valid local configuration: {error}",
                path.display()
            )
        })
    }

    pub fn repo(&self, task: &str) -> Result<PathBuf> {
        let value = self
            .repos
            .get(task)
            .ok_or_else(|| format!("repos in {} has no path for task {task}", "local.json"))?;
        let path = expand(Path::new(value))?;
        if !path.is_absolute() {
            return Err(format!(
                "the repo path for task {task} must be absolute: {value}"
            ));
        }
        Ok(path)
    }

    pub fn source_config(&self) -> Result<PathBuf> {
        match &self.source_config {
            Some(value) => {
                let path = expand(Path::new(value))?;
                if !path.is_absolute() {
                    return Err(format!(
                        "sourceConfig in local.json must be absolute: {value}"
                    ));
                }
                Ok(path)
            }
            None => {
                let home = env::var_os("HOME").ok_or("HOME is not set")?;
                Ok(PathBuf::from(home).join(".canter").join("config.json"))
            }
        }
    }

    pub fn canter(&self) -> String {
        self.canter.clone().unwrap_or_else(|| "canter".into())
    }
}

/// What one prepared run needs later; `state.json` inside the run directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub task: String,
    pub task_path: String,
    pub round: String,
    pub run: String,
    pub repo: String,
    pub repo_name: String,
    pub base_commit: String,
    pub worktree: String,
    pub session_file: String,
    pub model: String,
    pub auto_compact_tokens: Option<u64>,
    pub created_at: i64,
}

impl State {
    pub fn load(home: &Home, round: &str, run: &str) -> Result<Self> {
        let path = home.run_dir(round, run).join("state.json");
        let text = fs::read_to_string(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        serde_json::from_str(&text)
            .map_err(|error| format!("{} is not valid: {error}", path.display()))
    }

    pub fn save(&self, home: &Home) -> Result<()> {
        let path = home.run_dir(&self.round, &self.run).join("state.json");
        write_private(
            &path,
            serde_json::to_vec_pretty(self)
                .map_err(|error| format!("cannot encode state: {error}"))?
                .as_slice(),
        )
    }

    pub fn task(&self) -> Result<crate::task::Task> {
        crate::task::Task::load(Path::new(&self.task_path))
    }

    pub fn worktree(&self) -> &Path {
        Path::new(&self.worktree)
    }
}

pub fn expand(path: &Path) -> Result<PathBuf> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", path.display()))?;
    if let Some(rest) = text.strip_prefix("~/") {
        let home = env::var_os("HOME").ok_or("HOME is not set")?;
        return Ok(PathBuf::from(home).join(rest));
    }
    Ok(path.to_path_buf())
}

fn same_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

pub fn check_label(kind: &str, value: &str) -> Result<()> {
    let ok = !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.+".contains(&byte));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "{kind} must use ASCII letters, digits, - _ . or +: {value:?}"
        ))
    }
}

/// Create a 0600 file inside a 0700 directory, replacing it atomically.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .and_then(|()| fs::set_permissions(parent, fs::Permissions::from_mode(0o700)))
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        format!(
            "cannot create a temporary file in {}: {error}",
            parent.display()
        )
    })?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|error| format!("cannot protect {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.as_file()
        .sync_all()
        .map_err(|error| format!("cannot sync {}: {error}", path.display()))?;
    file.persist(path)
        .map_err(|error| format!("cannot replace {}: {}", path.display(), error.error))?;
    Ok(())
}

pub fn read_private_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    serde_json::from_str(&text).map_err(|error| format!("{} is not valid: {error}", path.display()))
}
