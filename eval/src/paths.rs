//! The eval state directory and the files that describe one run.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Component, Path, PathBuf},
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
        let home = env::var_os("HOME").map(PathBuf::from);
        let daily = env::var_os("CANTER_HOME").map(PathBuf::from);
        Self::resolve_with(flag, home.as_deref(), daily.as_deref())
    }

    /// The environment is passed in so the guards can be tested without
    /// changing process-wide variables.
    #[doc(hidden)]
    pub fn resolve_with(
        flag: Option<PathBuf>,
        home: Option<&Path>,
        daily: Option<&Path>,
    ) -> Result<Self> {
        let root = match flag {
            Some(path) => path,
            None => match env::var_os("CANTER_EVAL_HOME") {
                Some(value) => PathBuf::from(value),
                None => {
                    let home = home.ok_or("HOME is not set")?;
                    home.join(".canter-eval")
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
        let eval = Self { root };
        eval.refuse_daily_state(home, daily)?;
        Ok(eval)
    }

    /// Never point eval state at the user's home or at canter's own state
    /// directory (or a parent of it).
    fn refuse_daily_state(&self, home: Option<&Path>, daily: Option<&Path>) -> Result<()> {
        if let Some(home) = home
            && same(&self.root, home)
        {
            return Err(format!(
                "refusing to use {} as the eval state directory: it is the user's home directory",
                self.root.display()
            ));
        }
        for candidate in daily_state_dirs(home, daily) {
            if within(&self.root, &candidate) {
                return Err(format!(
                    "refusing to use {} as the eval state directory: it is inside canter's state directory {}",
                    self.root.display(),
                    candidate.display()
                ));
            }
            if let Some(parent) = candidate.parent()
                && same(&self.root, parent)
            {
                return Err(format!(
                    "refusing to use {} as the eval state directory: it is a parent of canter's state directory {}",
                    self.root.display(),
                    candidate.display()
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

    pub fn server_lock_path(&self) -> PathBuf {
        self.root.join("server.lock")
    }

    pub fn session_events(&self, id: &str) -> PathBuf {
        self.root.join("sessions").join(id).join("events.jsonl")
    }

    /// One run's per-session records: id, end HEAD, shell commands, memory snapshot.
    pub fn session_dir(&self, round: &str, run: &str, index: usize) -> PathBuf {
        self.run_dir(round, run)
            .join("sessions")
            .join(index.to_string())
    }

    pub fn session_id_path(&self, round: &str, run: &str, index: usize) -> PathBuf {
        self.session_dir(round, run, index).join("session.json")
    }

    pub fn session_head_path(&self, round: &str, run: &str, index: usize) -> PathBuf {
        self.session_dir(round, run, index).join("head")
    }

    pub fn session_commands_path(&self, round: &str, run: &str, index: usize) -> PathBuf {
        self.session_dir(round, run, index).join("commands.txt")
    }

    pub fn session_memory_path(&self, round: &str, run: &str, index: usize) -> PathBuf {
        self.session_dir(round, run, index).join("memory.json")
    }

    /// What `steer` has sent, skipped or timed out on for this run.
    pub fn steer_progress_path(&self, round: &str, run: &str) -> PathBuf {
        self.run_dir(round, run).join("steers.json")
    }

    /// Session ids already recorded for this run: one file per session, plus
    /// the run-level `session.json` that first-version runs wrote.
    pub fn recorded_session_ids(&self, round: &str, run: &str) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        let sessions = self.run_dir(round, run).join("sessions");
        if let Ok(entries) = fs::read_dir(&sessions) {
            for entry in entries.flatten() {
                if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                if let Some(id) = read_session_id(&entry.path().join("session.json"))? {
                    ids.push(id);
                }
            }
        }
        if let Some(id) = read_session_id(&self.run_dir(round, run).join("session.json"))? {
            ids.push(id);
        }
        Ok(ids)
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
    /// Where run worktrees live; defaults to `$HOME/.canter-eval-worktrees`.
    /// Must stay outside both the eval state directory and canter's own one,
    /// because canter refuses to read or write inside `CANTER_HOME`.
    #[serde(default)]
    pub worktrees: Option<String>,
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

    pub fn worktree_root(&self, home: &Home) -> Result<PathBuf> {
        let value = match &self.worktrees {
            Some(value) => value.clone(),
            None => {
                let home_dir = env::var_os("HOME").ok_or("HOME is not set")?;
                PathBuf::from(home_dir)
                    .join(".canter-eval-worktrees")
                    .display()
                    .to_string()
            }
        };
        let root = expand(Path::new(&value))?;
        if !root.is_absolute() {
            return Err(format!("worktrees in local.json must be absolute: {value}"));
        }
        let home_dir = env::var_os("HOME").map(PathBuf::from);
        if let Some(home_dir) = &home_dir
            && same(&root, home_dir)
        {
            return Err(format!(
                "refusing to put worktrees in {}: it is the user's home directory",
                root.display()
            ));
        }
        if within(&root, &home.root) {
            return Err(format!(
                "refusing to put worktrees in {}: it is inside the eval state directory {}",
                root.display(),
                home.root.display()
            ));
        }
        let daily = env::var_os("CANTER_HOME").map(PathBuf::from);
        for candidate in daily_state_dirs(home_dir.as_deref(), daily.as_deref()) {
            if within(&root, &candidate) {
                return Err(format!(
                    "refusing to put worktrees in {}: it is inside canter's state directory {}",
                    root.display(),
                    candidate.display()
                ));
            }
        }
        Ok(root)
    }

    pub fn worktree_path(&self, home: &Home, round: &str, run: &str) -> Result<PathBuf> {
        Ok(self.worktree_root(home)?.join(round).join(run))
    }

    /// The directory a run without a main repository runs its session in: the
    /// one that holds the further repositories' worktrees. Below a fixed
    /// segment that no run or repository tag can be, then by run. A run with a
    /// main repository keeps `<round>/<run>`, so the two layouts cannot meet:
    /// the segment holds `@`, which `check_label` refuses in a run name.
    pub fn session_dir_path(&self, home: &Home, round: &str, run: &str) -> Result<PathBuf> {
        Ok(self
            .worktree_root(home)?
            .join(round)
            .join(REPOS_SEGMENT)
            .join(run))
    }

    /// Where a further repository's worktree goes: below the directory a
    /// session without a main repository runs in, by repository name.
    pub fn other_worktree_path(
        &self,
        home: &Home,
        round: &str,
        run: &str,
        repo: &str,
    ) -> Result<PathBuf> {
        Ok(self.session_dir_path(home, round, run)?.join(repo))
    }
}

/// The directory segment that holds the worktrees of a run's further
/// repositories. `@` cannot appear in a run or repository tag (`check_label`
/// only allows ASCII letters, digits, `-`, `_`, `.` and `+`), so a main
/// worktree path can never be inside it and the other way round.
pub const REPOS_SEGMENT: &str = "@repos";

/// Whether `worktree` is a path this run would have made: its own worktree
/// `<round>/<run>`, or a further repository's
/// `<round>/@repos/<run>/<repository>`. A path recorded by an older layout, or
/// by another run, is not this run's to delete even if it exists.
pub fn owned_worktree(root: &Path, round: &str, run: &str, worktree: &Path) -> bool {
    let Some(relative) = relative_to(worktree, root) else {
        return false;
    };
    let parts: Vec<Component> = relative.components().collect();
    let round = OsStr::new(round);
    let run = OsStr::new(run);
    match parts.as_slice() {
        [Component::Normal(path_round), Component::Normal(path_run)] => {
            *path_round == round && *path_run == run
        }
        [
            Component::Normal(path_round),
            Component::Normal(repos),
            Component::Normal(path_run),
            Component::Normal(_repo),
        ] => *path_round == round && *repos == OsStr::new(REPOS_SEGMENT) && *path_run == run,
        _ => false,
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
    /// The main repository's checkout; empty when the task has none.
    pub repo: String,
    /// The main repository's `repos` key; empty when the task has none.
    pub repo_name: String,
    /// The main repository's start commit; empty when the task has none.
    pub base_commit: String,
    /// The main repository's worktree, or, without a main repository, the
    /// directory the session runs in.
    pub worktree: String,
    pub session_file: String,
    pub model: String,
    pub auto_compact_tokens: Option<u64>,
    pub created_at: i64,
    /// Sessions in this run; absent in state files written before multi-session tasks.
    #[serde(default = "one_session")]
    pub session_count: usize,
    /// Whether memories are emptied before every session or only once at the start.
    #[serde(default)]
    pub memory_mode: Option<MemoryMode>,
    /// The worktrees made from the task's further repositories; empty in state
    /// files written before multi-repository tasks. The main repository's
    /// worktree and base commit are the fields above.
    #[serde(default)]
    pub repos: Vec<StateRepo>,
}

/// One repository of a run: the checkout it came from and its worktree.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StateRepo {
    /// The `repos` key in the local configuration.
    pub name: String,
    pub repo: String,
    pub base_commit: String,
    pub worktree: String,
}

fn one_session() -> usize {
    1
}

/// What `prepare --memory` chooses: memories are kept between sessions or emptied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryMode {
    Keep,
    Clear,
}

impl MemoryMode {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "keep" => Ok(Self::Keep),
            "clear" => Ok(Self::Clear),
            other => Err(format!("--memory must be keep or clear, not {other:?}")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Clear => "clear",
        }
    }
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

    /// Whether the task has a main repository. A task without one leaves the
    /// run-level repository fields empty and runs its session in `worktree`,
    /// the directory that holds the further repositories' worktrees.
    pub fn has_main_repo(&self) -> bool {
        !self.repo_name.is_empty()
    }

    /// The main repository, described by the run-level fields.
    pub fn main_repo(&self) -> Option<StateRepo> {
        self.has_main_repo().then(|| StateRepo {
            name: self.repo_name.clone(),
            repo: self.repo.clone(),
            base_commit: self.base_commit.clone(),
            worktree: self.worktree.clone(),
        })
    }

    /// Every repository this run made a worktree for, the main one first.
    pub fn all_repos(&self) -> Vec<StateRepo> {
        let mut repos = Vec::new();
        let main_name = self.main_repo().map(|main| {
            repos.push(main.clone());
            main.name
        });
        repos.extend(
            self.repos
                .iter()
                .filter(|repo| main_name.as_ref() != Some(&repo.name))
                .cloned(),
        );
        repos
    }

    /// The worktree of one repository of this run.
    pub fn repo_by_name(&self, name: &str) -> Result<StateRepo> {
        self.all_repos()
            .into_iter()
            .find(|repo| repo.name == name)
            .ok_or_else(|| {
                format!(
                    "run {} has no worktree for repository {name}\nprepared before the task used it? prepare the run again",
                    self.run
                )
            })
    }

    /// The repository session `index` (one-based) runs in; `None` when the task
    /// has no main repository and the session runs between the others.
    pub fn session_checkout(&self, index: usize) -> Result<Option<StateRepo>> {
        match self.task()?.session_repo(index)? {
            Some(name) => Ok(Some(self.repo_by_name(name)?)),
            None => Ok(None),
        }
    }

    /// The directory session `index` (one-based) works in.
    pub fn session_cwd(&self, index: usize) -> Result<PathBuf> {
        Ok(self.session_checkout(index)?.map_or_else(
            || PathBuf::from(&self.worktree),
            |repo| PathBuf::from(repo.worktree),
        ))
    }

    /// The repository a check runs in; `None` means the task's own session
    /// directory.
    pub fn check_checkout(
        &self,
        task: &crate::task::Task,
        check: &crate::task::Check,
    ) -> Result<Option<StateRepo>> {
        match task.check_repo(check) {
            Some(name) => Ok(Some(self.repo_by_name(name)?)),
            None if self.has_main_repo() => Ok(self.main_repo()),
            None => Ok(None),
        }
    }
}

/// Where one session's id lives. First-version state files wrote the id of
/// their only session at the run level; keep using that file for session 1.
pub fn session_id_file(home: &Home, state: &State, index: usize) -> PathBuf {
    let path = home.session_id_path(&state.round, &state.run, index);
    if index == 1 && state.session_count <= 1 {
        let legacy = Path::new(&state.session_file);
        if legacy != path {
            return legacy.to_path_buf();
        }
    }
    path
}

/// The session id in a `session.json`, or `None` when the file is not there.
pub fn read_session_id(path: &Path) -> Result<Option<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid: {error}", path.display()))?;
    Ok(value
        .get("sessionId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string))
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

/// canter's own state directories: `$HOME/.canter` and any `CANTER_HOME`.
fn daily_state_dirs(home: Option<&Path>, daily: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = home {
        candidates.push(home.join(".canter"));
    }
    if let Some(daily) = daily {
        candidates.push(daily.to_path_buf());
    }
    candidates
}

/// `path` relative to `dir`, resolving the existing parts of both so a
/// canonicalised state directory still matches the paths canter reports.
pub fn relative_to(path: &Path, dir: &Path) -> Option<PathBuf> {
    compare_path(path)
        .strip_prefix(compare_path(dir))
        .ok()
        .map(Path::to_path_buf)
}

/// Whether `path` is `dir` or below it, comparing resolved paths when they
/// exist and falling back to `.`/`..` normalization for paths not yet created.
pub fn within(path: &Path, dir: &Path) -> bool {
    let path = compare_path(path);
    let dir = compare_path(dir);
    path.starts_with(&dir)
}

fn same(left: &Path, right: &Path) -> bool {
    compare_path(left) == compare_path(right)
}

fn compare_path(path: &Path) -> PathBuf {
    // Resolve the longest existing prefix so a not-yet-created path still
    // shares the resolved form of its existing parents (macOS /tmp and /var
    // are symlinks).
    let mut existing = path.to_path_buf();
    let mut suffix = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            let mut out = canonical;
            for part in suffix.iter().rev() {
                out.push(part);
            }
            return out;
        }
        let Some(name) = existing.file_name().map(|name| name.to_os_string()) else {
            break;
        };
        suffix.push(name);
        existing.pop();
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            component => out.push(component.as_os_str()),
        }
    }
    out
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
