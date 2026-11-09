//! Task files: what one eval task asks a canter session to do and how it is
//! judged. Task files are committed; local paths never appear in them.

use std::{fs, path::Path};

use serde::Deserialize;

use crate::Result;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    /// Stable short name; also the `repos` key in the local configuration.
    pub id: String,
    pub name: String,
    /// The repository name only; its path lives in the eval state directory.
    pub repo: String,
    /// The commit the worktree starts from, as accepted by `git rev-parse`.
    pub base: String,
    pub prompt: String,
    #[serde(default)]
    pub auto_compact_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning: Option<String>,
    /// Shell commands run in the new worktree before the session starts.
    #[serde(default)]
    pub setup: Vec<String>,
    #[serde(default)]
    pub steers: Vec<Steer>,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Steer {
    /// Send after this many commits whose subject starts with `ritual: `.
    pub after_commits: u32,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub id: String,
    pub description: String,
    /// A shell command run in the worktree; exit 0 passes.
    pub command: String,
    /// Record the number the command prints instead of passing or failing.
    #[serde(default)]
    pub record: bool,
}

impl Task {
    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let task: Self = toml::from_str(&text)
            .map_err(|error| format!("{} is not a valid task file: {error}", path.display()))?;
        task.validate()?;
        Ok(task)
    }

    fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.name.is_empty() || self.repo.is_empty() {
            return Err("a task needs id, name and repo".into());
        }
        if self.prompt.trim().is_empty() {
            return Err(format!("task {} has an empty prompt", self.id));
        }
        if self.auto_compact_tokens == Some(0) {
            return Err(format!(
                "task {} has auto_compact_tokens 0; it must be greater than 0",
                self.id
            ));
        }
        for steer in &self.steers {
            if steer.after_commits == 0 || steer.text.trim().is_empty() {
                return Err(format!(
                    "task {} has a steer with no commit count or text",
                    self.id
                ));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        for check in &self.checks {
            if check.command.trim().is_empty() {
                return Err(format!("check {} has an empty command", check.id));
            }
            if !ids.insert(check.id.as_str()) {
                return Err(format!("task {} repeats check id {}", self.id, check.id));
            }
        }
        Ok(())
    }
}
