//! Prepare one run: eval configuration, empty memories, a fresh worktree, and
//! the commands the orchestrator runs next.

use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use crate::{Result, paths::State};

#[derive(Debug)]
pub struct PrepareOptions {
    pub home: crate::paths::Home,
    pub task_dir: PathBuf,
    pub task: String,
    pub round: String,
    pub run: String,
    pub model: Option<String>,
    pub source_config: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Prepared {
    pub state: State,
    pub task: crate::task::Task,
    pub memories_cleared: usize,
}

impl Prepared {
    /// What the orchestrator sees; no configuration values are included.
    pub fn summary(&self, home: &crate::paths::Home) -> String {
        let eval_home = home.root.display().to_string();
        let mut out = String::new();
        out.push_str(&format!("task: {}\n", self.task.name));
        out.push_str(&format!(
            "repo: {} @ {}\n",
            self.state.repo_name, self.state.base_commit
        ));
        out.push_str(&format!("worktree: {}\n", self.state.worktree));
        out.push_str(&format!("model: {}\n", self.state.model));
        out.push_str(&format!(
            "auto compact: {}\n",
            self.state
                .auto_compact_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "model default".into())
        ));
        out.push_str(&format!("memories cleared: {}\n", self.memories_cleared));
        out.push_str(&format!("eval home: {eval_home}\n\n"));
        out.push_str("next:\n");
        out.push_str(&format!(
            "  1. start the eval server: CANTER_HOME={} canter serve --replace\n",
            quote(&eval_home)
        ));
        out.push_str(&format!(
            "  2. trust the worktree: canter-eval trust --eval-home {} --round {} --run {}\n",
            quote(&eval_home),
            self.state.round,
            self.state.run
        ));
        out.push_str(&format!(
            "  3. start the run: {}\n",
            self.corral_command(&eval_home)
        ));
        out.push_str(&format!(
            "  4. in another terminal: canter-eval steer --eval-home {} --round {} --run {}\n",
            quote(&eval_home),
            self.state.round,
            self.state.run
        ));
        out
    }

    pub fn corral_command(&self, eval_home: &str) -> String {
        format!(
            "corral start {} --unique --cwd {} --env {} --prompt {} -- canter --yolo --tag {} --session-file {}",
            quote(&format!("eval/{}", self.state.run)),
            quote(&self.state.worktree),
            quote(&format!("CANTER_HOME={eval_home}")),
            quote(&self.task.prompt),
            quote(&format!("eval={}", self.state.round)),
            quote(&self.state.session_file),
        )
    }
}

pub fn prepare(options: &PrepareOptions) -> Result<Prepared> {
    crate::paths::check_label("round", &options.round)?;
    crate::paths::check_label("run", &options.run)?;
    let task_path = options.task_dir.join(format!("{}.toml", options.task));
    let task = crate::task::Task::load(&task_path)?;
    let local = crate::paths::Local::load(&options.home)?;
    let model = options.model.clone().unwrap_or_else(|| local.model.clone());
    let source = match &options.source_config {
        Some(path) => crate::paths::expand(path)?,
        None => local.source_config()?,
    };
    let repo = local.repo(&task.repo)?;
    if !repo.is_dir() {
        return Err(format!(
            "repository for {} does not exist: {}",
            task.repo,
            repo.display()
        ));
    }

    let run_dir = options.home.run_dir(&options.round, &options.run);
    if run_dir.exists() {
        return Err(format!(
            "run directory already exists: {}\nremove it with `canter-eval clean` or pick another run name",
            run_dir.display()
        ));
    }
    options.home.ensure()?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&run_dir)
        .map_err(|error| format!("cannot create {}: {error}", run_dir.display()))?;

    crate::config::write(
        &options.home.config_path(),
        &source,
        &model,
        task.auto_compact_tokens,
        task.reasoning.as_deref(),
    )?;
    let memories_cleared = clear_memories(&options.home)?;

    let base_commit = git(
        &repo,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", task.base),
        ],
    )?;
    let base_commit = base_commit.trim().to_string();
    let worktree = run_dir.join("worktree");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "--detach",
            worktree.to_str().ok_or("the worktree path is not UTF-8")?,
            &base_commit,
        ],
    )?;

    let setup_env = vec![
        ("REPO".to_string(), repo.display().to_string()),
        ("WORKTREE".to_string(), worktree.display().to_string()),
        ("BASE".to_string(), base_commit.clone()),
    ];
    for (index, command) in task.setup.iter().enumerate() {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&worktree)
            .envs(setup_env.clone())
            .output()
            .map_err(|error| format!("cannot run setup command {}: {error}", index + 1))?;
        if !output.status.success() {
            return Err(format!(
                "setup command {} failed with {}: {}\n{}",
                index + 1,
                output.status,
                command,
                tail(&String::from_utf8_lossy(&output.stderr))
            ));
        }
    }

    let state = State {
        task: task.id.clone(),
        task_path: task_path
            .canonicalize()
            .unwrap_or_else(|_| task_path.clone())
            .display()
            .to_string(),
        round: options.round.clone(),
        run: options.run.clone(),
        repo: repo.display().to_string(),
        repo_name: task.repo.clone(),
        base_commit,
        worktree: worktree.display().to_string(),
        session_file: run_dir.join("session.json").display().to_string(),
        model,
        auto_compact_tokens: task.auto_compact_tokens,
        created_at: crate::now_ms(),
    };
    state.save(&options.home)?;
    Ok(Prepared {
        state,
        task,
        memories_cleared,
    })
}

/// Empty the global and every project memory of the eval state directory.
fn clear_memories(home: &crate::paths::Home) -> Result<usize> {
    let mut cleared = 0;
    let global = home.root.join("memory");
    cleared += clear_dir(&global)?;
    let projects = home.root.join("projects");
    let entries = match fs::read_dir(&projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(cleared),
        Err(error) => return Err(format!("cannot read {}: {error}", projects.display())),
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read {}: {error}", projects.display()))?;
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            cleared += clear_dir(&entry.path().join("memory"))?;
        }
    }
    Ok(cleared)
}

fn clear_dir(path: &Path) -> Result<usize> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("cannot read {}: {error}", entry.path().display()))?;
        if metadata.is_dir() {
            fs::remove_dir_all(entry.path())
                .map_err(|error| format!("cannot remove {}: {error}", entry.path().display()))?;
        } else {
            fs::remove_file(entry.path())
                .map_err(|error| format!("cannot remove {}: {error}", entry.path().display()))?;
        }
        removed += 1;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot protect {}: {error}", path.display()))?;
    Ok(removed)
}

pub fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|error| format!("cannot run git: {error}"))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    Err(format!(
        "git {} in {} failed with {}: {}",
        args.join(" "),
        repo.display(),
        output.status,
        tail(&String::from_utf8_lossy(&output.stderr))
    ))
}

pub fn tail(text: &str) -> String {
    let text = text.trim_end();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(10);
    let joined = lines[start..].join("\n");
    let chars: Vec<char> = joined.chars().collect();
    let start = chars.len().saturating_sub(2000);
    chars[start..].iter().collect()
}

fn quote(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=+".contains(&byte))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}
