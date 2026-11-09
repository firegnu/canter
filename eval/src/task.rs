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
    /// The main repository name only; its path lives in the eval state
    /// directory. A task without a main repository leaves `repo` and `base`
    /// out and runs its session in the directory that holds the further
    /// repositories' worktrees.
    #[serde(default)]
    pub repo: Option<String>,
    /// The commit the main repository's worktree starts from, as accepted by
    /// `git rev-parse`.
    #[serde(default)]
    pub base: Option<String>,
    /// Further repositories, each with its own start commit and setup commands.
    #[serde(default)]
    pub repos: Vec<TaskRepo>,
    /// The prompt of a one-session task; a multi-session task uses `sessions`.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Several sessions in a row in the same worktree, each with its own prompt.
    #[serde(default)]
    pub sessions: Vec<Session>,
    #[serde(default)]
    pub auto_compact_tokens: Option<u64>,
    #[serde(default)]
    pub reasoning: Option<String>,
    /// Shell commands run in the new worktree before the session starts.
    #[serde(default)]
    pub setup: Vec<String>,
    /// Send a line of chat after counted commits; for tasks whose work is
    /// marked by commits.
    #[serde(default)]
    pub steers: Vec<Steer>,
    /// Send a line of chat at a moment of one session's run; the entry's
    /// `after` says which moment.
    #[serde(default, rename = "chat")]
    pub chats: Vec<Chat>,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskRepo {
    /// The `repos` key in the local configuration.
    pub name: String,
    /// The commit this repository's worktree starts from.
    pub base: String,
    /// Shell commands run in this repository's new worktree before the run.
    #[serde(default)]
    pub setup: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub prompt: String,
    /// The repository this session runs in; defaults to the task's `repo`, or,
    /// without one, to the directory holding the repositories' worktrees.
    #[serde(default)]
    pub repo: Option<String>,
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
pub struct Chat {
    pub after: ChatAfter,
    pub text: String,
}

/// When a chat line is sent: after a turn of the main session, or after a
/// sub-agent report woke it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChatAfter {
    /// After the previous turn ends: the opening turn for the first entry, the
    /// turn the previous entry started for the others.
    Turn,
    /// After a sub-agent report woke the main session. Not sent when every
    /// sub-agent has already come back by then.
    Report,
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
    /// The repository whose worktree the command runs in; defaults to the
    /// task's `repo`, or, without one, to the directory holding the
    /// repositories' worktrees.
    #[serde(default)]
    pub repo: Option<String>,
}

/// One repository a task uses: its `repos` key, start commit and setup commands.
#[derive(Debug)]
pub struct Repo<'a> {
    pub name: &'a str,
    pub base: &'a str,
    pub setup: &'a [String],
}

/// A name that can be appended to a worktree path.
fn path_safe(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
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

    /// How many sessions one run of this task has: one unless `sessions` lists more.
    pub fn session_count(&self) -> usize {
        if self.sessions.is_empty() {
            1
        } else {
            self.sessions.len()
        }
    }

    /// The prompt of session `index` (one-based).
    pub fn prompt(&self, index: usize) -> Result<&str> {
        if self.sessions.is_empty() {
            if index == 1 {
                return self
                    .prompt
                    .as_deref()
                    .ok_or_else(|| format!("task {} has no prompt", self.id));
            }
            return Err(format!("task {} has only one session", self.id));
        }
        self.sessions
            .get(index - 1)
            .map(|session| session.prompt.as_str())
            .ok_or_else(|| format!("task {} has no session {index}", self.id))
    }

    /// The repository session `index` (one-based) runs in; `None` means the
    /// task has no main repository and the session runs between the others'
    /// worktrees.
    pub fn session_repo(&self, index: usize) -> Result<Option<&str>> {
        if self.sessions.is_empty() {
            if index == 1 {
                return Ok(self.repo.as_deref());
            }
            return Err(format!("task {} has only one session", self.id));
        }
        self.sessions
            .get(index - 1)
            .map(|session| session.repo.as_deref().or(self.repo.as_deref()))
            .ok_or_else(|| format!("task {} has no session {index}", self.id))
    }

    /// The repository a check runs in; `None` means the directory holding the
    /// repositories' worktrees (a task without a main repository).
    pub fn check_repo<'a>(&'a self, check: &'a Check) -> Option<&'a str> {
        check.repo.as_deref().or(self.repo.as_deref())
    }

    /// Every repository the task uses, the main one first, with the start
    /// commit and setup commands of each.
    pub fn repositories(&self) -> Vec<Repo<'_>> {
        let mut repos = Vec::new();
        if let (Some(name), Some(base)) = (self.repo.as_deref(), self.base.as_deref()) {
            repos.push(Repo {
                name,
                base,
                setup: &self.setup,
            });
        }
        repos.extend(self.repos.iter().map(|repo| Repo {
            name: &repo.name,
            base: &repo.base,
            setup: &repo.setup,
        }));
        repos
    }

    fn validate(&self) -> Result<()> {
        if self.id.is_empty() || self.name.is_empty() {
            return Err("a task needs id and name".into());
        }
        match (&self.repo, &self.base) {
            (Some(repo), Some(base)) if !repo.trim().is_empty() && !base.trim().is_empty() => {}
            (None, None) => {
                if self.repos.is_empty() {
                    return Err(format!(
                        "task {} has no repo and base and no [[repos]]; it needs at least one repository",
                        self.id
                    ));
                }
            }
            _ => {
                return Err(format!(
                    "task {} writes repo and base together or leaves both out",
                    self.id
                ));
            }
        }
        let mut names = std::collections::BTreeSet::new();
        for repo in &self.repos {
            if repo.name.is_empty() || repo.base.trim().is_empty() {
                return Err(format!(
                    "task {} has a repository without a name or base commit",
                    self.id
                ));
            }
            if self.repo.as_deref() == Some(repo.name.as_str()) {
                return Err(format!(
                    "task {} lists {} both as its repo and under [[repos]]",
                    self.id, repo.name
                ));
            }
            if !path_safe(&repo.name) {
                return Err(format!(
                    "repository name {:?} cannot be used in a worktree path",
                    repo.name
                ));
            }
            if !names.insert(repo.name.as_str()) {
                return Err(format!("task {} repeats repository {}", self.id, repo.name));
            }
        }
        if self.sessions.is_empty() {
            if self
                .prompt
                .as_deref()
                .is_none_or(|prompt| prompt.trim().is_empty())
            {
                return Err(format!("task {} has an empty prompt", self.id));
            }
        } else {
            if self.prompt.is_some() {
                return Err(format!(
                    "task {} uses both prompt and sessions; write one prompt per session or one prompt",
                    self.id
                ));
            }
            for (index, session) in self.sessions.iter().enumerate() {
                if session.prompt.trim().is_empty() {
                    return Err(format!(
                        "task {} has an empty prompt in session {}",
                        self.id,
                        index + 1
                    ));
                }
                if let Some(name) = &session.repo
                    && Some(name.as_str()) != self.repo.as_deref()
                    && !names.contains(name.as_str())
                {
                    return Err(format!(
                        "task {} session {} runs in {name}, which is not listed under [[repos]]",
                        self.id,
                        index + 1
                    ));
                }
            }
        }
        if self.auto_compact_tokens == Some(0) {
            return Err(format!(
                "task {} has auto_compact_tokens 0; it must be greater than 0",
                self.id
            ));
        }
        if !self.steers.is_empty() && !self.chats.is_empty() {
            return Err(format!(
                "task {} uses both [[steers]] and [[chat]]; a task sends one kind of chat",
                self.id
            ));
        }
        if !self.chats.is_empty() && self.session_count() > 1 {
            return Err(format!(
                "task {} uses [[chat]], which runs one session; write [[steers]] for a multi-session task",
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
        for chat in &self.chats {
            if chat.text.trim().is_empty() {
                return Err(format!("task {} has a chat with no text", self.id));
            }
        }
        let mut ids = std::collections::BTreeSet::new();
        for check in &self.checks {
            if check.command.trim().is_empty() {
                return Err(format!("check {} has an empty command", check.id));
            }
            if let Some(name) = &check.repo
                && Some(name.as_str()) != self.repo.as_deref()
                && !names.contains(name.as_str())
            {
                return Err(format!(
                    "check {} runs in {name}, which is not listed under [[repos]]",
                    check.id
                ));
            }
            if !ids.insert(check.id.as_str()) {
                return Err(format!("task {} repeats check id {}", self.id, check.id));
            }
        }
        Ok(())
    }
}

/// Replace `{repo:<name>}` in a prompt or chat line with that repository's
/// worktree path, so the model sees the same absolute paths every run.
pub fn render(text: &str, state: &crate::paths::State) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{repo:") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "{repo:".len()..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("{text:?} has an unclosed repository placeholder"))?;
        let name = &after[..end];
        let repo = state.repo_by_name(name).map_err(|_| {
            format!(
                "the placeholder {{repo:{name}}} names a repository this run has no worktree for"
            )
        })?;
        out.push_str(&repo.worktree);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}
