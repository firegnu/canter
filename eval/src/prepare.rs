//! Prepare one run: eval configuration, empty memories, a fresh worktree for
//! every repository the task uses, and the commands the orchestrator runs
//! next. Later sessions of a multi-session run are prepared one at a time in
//! the worktree of their own repository.

use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
};

use crate::{
    Result,
    paths::{Home, MemoryMode, State, StateRepo},
};

#[derive(Debug)]
pub struct PrepareOptions {
    pub home: Home,
    pub task_dir: PathBuf,
    /// The task of session 1; later sessions read it from the run's state.json.
    pub task: Option<String>,
    pub round: String,
    pub run: String,
    /// One-based session of the task; defaults to the first.
    pub session: usize,
    /// `None` keeps the run's mode, or clears for the first session.
    pub memory: Option<MemoryMode>,
    pub model: Option<String>,
    pub source_config: Option<PathBuf>,
}

#[derive(Debug)]
pub struct Prepared {
    pub state: State,
    pub task: crate::task::Task,
    pub session: usize,
    pub prompt: String,
    /// The repository this session runs in; `None` when the task has no main
    /// repository and the session runs between the others' worktrees.
    pub repo: Option<StateRepo>,
    /// The directory this session works in.
    pub cwd: PathBuf,
    pub memories_cleared: usize,
}

impl Prepared {
    /// What the orchestrator sees; no configuration values are included.
    pub fn summary(&self, home: &Home) -> String {
        let eval_home = home.root.display().to_string();
        let session_count = self.state.session_count.max(1);
        let mode = self.state.memory_mode.unwrap_or(MemoryMode::Clear);
        let repos = self.state.all_repos();
        let mut out = String::new();
        out.push_str(&format!("task: {}\n", self.task.name));
        for repo in &repos {
            out.push_str(&format!("repo: {} @ {}\n", repo.name, repo.base_commit));
            out.push_str(&format!("worktree: {}\n", repo.worktree));
        }
        if repos.len() > 1 {
            if let Some(repo) = &self.repo {
                out.push_str(&format!("session repo: {}\n", repo.name));
            } else {
                out.push_str(&format!("session cwd: {}\n", self.cwd.display()));
            }
        }
        out.push_str(&format!("model: {}\n", self.state.model));
        out.push_str(&format!(
            "auto compact: {}\n",
            self.state
                .auto_compact_tokens
                .map(|tokens| tokens.to_string())
                .unwrap_or_else(|| "model default".into())
        ));
        out.push_str(&format!("memory: {}\n", mode.as_str()));
        if session_count > 1 {
            out.push_str(&format!("session: {} of {}\n", self.session, session_count));
        }
        out.push_str(&format!("memories cleared: {}\n", self.memories_cleared));
        out.push_str(&format!("eval home: {eval_home}\n\n"));
        out.push_str("next:\n");
        if session_count == 1 {
            out.push_str(&format!(
                "  1. start the eval server: CANTER_HOME={} canter serve --replace\n",
                quote(&eval_home)
            ));
            out.push_str(&format!(
                "  2. trust the worktrees: canter-eval trust --eval-home {} --round {} --run {}\n",
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
            return out;
        }
        let mut step = 1;
        if self.session == 1 {
            out.push_str(&format!(
                "  {step}. start the eval server: CANTER_HOME={} canter serve --replace\n",
                quote(&eval_home)
            ));
            step += 1;
            out.push_str(&format!(
                "  {step}. trust the worktrees: canter-eval trust --eval-home {} --round {} --run {}\n",
                quote(&eval_home),
                self.state.round,
                self.state.run
            ));
            step += 1;
        }
        let run = &self.state.run;
        out.push_str(&format!(
            "  {step}. start session {} of {}: {}\n",
            self.session,
            session_count,
            self.corral_command(&eval_home)
        ));
        step += 1;
        out.push_str(&format!(
            "  {step}. in another terminal: canter-eval steer --eval-home {} --round {} --run {} --session {}\n",
            quote(&eval_home),
            self.state.round,
            run,
            self.session
        ));
        step += 1;
        out.push_str(&format!("  {step}. wait for it: corral wait eval/{run}\n"));
        step += 1;
        out.push_str(&format!("  {step}. stop it: corral stop eval/{run}\n"));
        step += 1;
        out.push_str(&format!(
            "  {step}. record it: canter-eval finish --eval-home {} --round {} --run {} --session {}\n",
            quote(&eval_home),
            self.state.round,
            run,
            self.session
        ));
        step += 1;
        if self.session < session_count {
            out.push_str(&format!(
                "  {step}. next: canter-eval prepare --eval-home {} --round {} --run {} --session {}\n",
                quote(&eval_home),
                self.state.round,
                run,
                self.session + 1
            ));
        } else {
            out.push_str(&format!(
                "  {step}. grade it: canter-eval grade --eval-home {} --round {} --run {}, then collect\n",
                quote(&eval_home),
                self.state.round,
                run
            ));
        }
        out
    }

    pub fn corral_command(&self, eval_home: &str) -> String {
        // A multi-session run starts several sessions under the same name; a
        // `--unique` suffix would not match the wait and stop commands.
        let unique = if self.state.session_count > 1 {
            ""
        } else {
            " --unique"
        };
        format!(
            "corral start eval/{}{unique} --cwd {} --env {} --env CAIRN_DISABLE=1 --prompt {} -- canter --yolo --tag {}",
            self.state.run,
            quote(&self.cwd.display().to_string()),
            quote(&format!("CANTER_HOME={eval_home}")),
            quote(&self.prompt),
            quote(&format!("eval={}", self.state.round)),
        )
    }
}

pub fn prepare(options: &PrepareOptions) -> Result<Prepared> {
    crate::paths::check_label("round", &options.round)?;
    crate::paths::check_label("run", &options.run)?;
    if options.session == 0 {
        return Err("--session starts at 1".into());
    }
    if options.session == 1 {
        let name = options.task.as_deref().ok_or(
            "--task is required for session 1; later sessions read it from the run's state.json",
        )?;
        let task_path = options.task_dir.join(format!("{name}.toml"));
        let task = crate::task::Task::load(&task_path)?;
        if options.session > task.session_count() {
            return Err(format!(
                "task {} has {} sessions; --session {} is past the end",
                task.id,
                task.session_count(),
                options.session
            ));
        }
        return prepare_first(options, &task, &task_path);
    }
    let state = State::load(&options.home, &options.round, &options.run)
        .map_err(|error| format!("{error}\nprepare session 1 of this run first"))?;
    if let Some(name) = &options.task
        && *name != state.task
    {
        return Err(format!(
            "run {} is for task {}; leave --task out for a later session",
            state.run, state.task
        ));
    }
    let task = state.task()?;
    if options.session > task.session_count() {
        return Err(format!(
            "task {} has {} sessions; --session {} is past the end",
            task.id,
            task.session_count(),
            options.session
        ));
    }
    prepare_next(options, &task, &state)
}

fn prepare_first(
    options: &PrepareOptions,
    task: &crate::task::Task,
    task_path: &Path,
) -> Result<Prepared> {
    let local = crate::paths::Local::load(&options.home)?;
    let model = options.model.clone().unwrap_or_else(|| local.model.clone());
    let source = match &options.source_config {
        Some(path) => crate::paths::expand(path)?,
        None => local.source_config()?,
    };
    let memory = options.memory.unwrap_or(MemoryMode::Clear);
    // Every repository has to be there before anything is written or cleared.
    let mut planned = Vec::new();
    for repo_setup in task.repositories() {
        let repo = local.repo(repo_setup.name)?;
        if !repo.is_dir() {
            return Err(format!(
                "repository for {} does not exist: {}",
                repo_setup.name,
                repo.display()
            ));
        }
        planned.push((repo_setup, repo));
    }

    let run_dir = options.home.run_dir(&options.round, &options.run);
    if run_dir.exists() {
        return Err(format!(
            "run directory already exists: {}\nremove it with `canter-eval clean` or pick another run name",
            run_dir.display()
        ));
    }
    // The session's directory: the main repository keeps the run's own worktree
    // path; a task without one runs between the further repositories' worktrees.
    let session_dir = match task.repo.as_deref() {
        Some(_) => local.worktree_path(&options.home, &options.round, &options.run)?,
        None => local.session_dir_path(&options.home, &options.round, &options.run)?,
    };
    if session_dir.exists() {
        return Err(format!(
            "worktree already exists: {}\nremove it or pick another run name",
            session_dir.display()
        ));
    }
    if let Some(parent) = session_dir.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .and_then(|()| fs::set_permissions(parent, fs::Permissions::from_mode(0o700)))
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    if task.repo.is_none() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&session_dir)
            .and_then(|()| fs::set_permissions(&session_dir, fs::Permissions::from_mode(0o700)))
            .map_err(|error| format!("cannot create {}: {error}", session_dir.display()))?;
    }
    options.home.ensure()?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(options.home.session_dir(&options.round, &options.run, 1))
        .map_err(|error| {
            format!(
                "cannot create {}: {error}",
                options
                    .home
                    .session_dir(&options.round, &options.run, 1)
                    .display()
            )
        })?;

    crate::config::write(
        &options.home.config_path(),
        &source,
        &model,
        task.auto_compact_tokens,
        task.reasoning.as_deref(),
    )?;
    let memories_cleared = crate::memory::clear(&options.home)?;

    // Every repository the task uses gets a worktree and its own setup run.
    let mut repos = Vec::new();
    let mut main = None;
    for (repo_setup, repo) in planned {
        let worktree = if Some(repo_setup.name) == task.repo.as_deref() {
            session_dir.clone()
        } else {
            local.other_worktree_path(
                &options.home,
                &options.round,
                &options.run,
                repo_setup.name,
            )?
        };
        if worktree != session_dir && worktree.exists() {
            return Err(format!(
                "worktree already exists: {}\nremove it or pick another run name",
                worktree.display()
            ));
        }
        if let Some(parent) = worktree.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .and_then(|()| fs::set_permissions(parent, fs::Permissions::from_mode(0o700)))
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let base_commit = git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                &format!("{}^{{commit}}", repo_setup.base),
            ],
        )?;
        let base_commit = base_commit.trim().to_string();
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
        run_setup(&repo, &worktree, &base_commit, repo_setup.setup)?;
        let row = StateRepo {
            name: repo_setup.name.to_string(),
            repo: repo.display().to_string(),
            base_commit,
            worktree: worktree.display().to_string(),
        };
        if Some(row.name.as_str()) == task.repo.as_deref() {
            main = Some(row);
        } else {
            repos.push(row);
        }
    }
    if task.repo.is_some() && main.is_none() {
        return Err("the task's main repository has no worktree".into());
    }
    // Without a main repository the session's directory is the run-level one.
    let (repo, repo_name, base_commit, worktree) = match main {
        Some(main) => (main.repo, main.name, main.base_commit, main.worktree),
        None => (
            String::new(),
            String::new(),
            String::new(),
            session_dir.display().to_string(),
        ),
    };

    let state = State {
        task: task.id.clone(),
        task_path: task_path
            .canonicalize()
            .unwrap_or_else(|_| task_path.to_path_buf())
            .display()
            .to_string(),
        round: options.round.clone(),
        run: options.run.clone(),
        repo,
        repo_name,
        base_commit,
        worktree,
        session_file: options
            .home
            .session_id_path(&options.round, &options.run, 1)
            .display()
            .to_string(),
        model,
        auto_compact_tokens: task.auto_compact_tokens,
        created_at: crate::now_ms(),
        session_count: task.session_count(),
        memory_mode: Some(memory),
        repos,
    };
    state.save(&options.home)?;
    let repo = state.session_checkout(1)?;
    let cwd = state.session_cwd(1)?;
    let prompt = crate::task::render(task.prompt(1)?, &state)?;
    Ok(Prepared {
        state,
        task: task.clone(),
        session: 1,
        prompt,
        repo,
        cwd,
        memories_cleared,
    })
}

/// Run a repository's setup commands in its new worktree.
fn run_setup(repo: &Path, worktree: &Path, base_commit: &str, setup: &[String]) -> Result<()> {
    let env = vec![
        ("REPO".to_string(), repo.display().to_string()),
        ("WORKTREE".to_string(), worktree.display().to_string()),
        ("BASE".to_string(), base_commit.to_string()),
    ];
    for (index, command) in setup.iter().enumerate() {
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(worktree)
            .envs(env.clone())
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
    Ok(())
}

fn prepare_next(
    options: &PrepareOptions,
    task: &crate::task::Task,
    state: &State,
) -> Result<Prepared> {
    if state.session_count != task.session_count() {
        return Err(format!(
            "task {} now has {} sessions; this run was prepared with {}",
            task.id,
            task.session_count(),
            state.session_count
        ));
    }
    if options.model.is_some() || options.source_config.is_some() {
        return Err("the model is fixed when session 1 is prepared".into());
    }
    let mode = match (options.memory, state.memory_mode) {
        (Some(given), Some(run)) if given != run => {
            return Err(format!(
                "this run uses --memory {}; prepare the next session with {} or leave it out",
                run.as_str(),
                run.as_str()
            ));
        }
        (Some(given), _) => given,
        (None, Some(run)) => run,
        (None, None) => MemoryMode::Clear,
    };
    let cwd = state.session_cwd(options.session)?;
    if !cwd.is_dir() {
        return Err(format!(
            "the worktree is gone: {}\nprepare the run again or clean it",
            cwd.display()
        ));
    }
    let previous =
        options
            .home
            .session_head_path(&options.round, &options.run, options.session - 1);
    if !previous.is_file() {
        return Err(format!(
            "session {} is not recorded yet: {} is missing\nrun `canter-eval finish --session {}` when it stops",
            options.session - 1,
            previous.display(),
            options.session - 1
        ));
    }
    let session_dir = options
        .home
        .session_dir(&options.round, &options.run, options.session);
    if session_dir.exists() {
        return Err(format!(
            "session {} was already prepared: {}\nremove that directory to prepare it again",
            options.session,
            session_dir.display()
        ));
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&session_dir)
        .map_err(|error| format!("cannot create {}: {error}", session_dir.display()))?;
    let memories_cleared = if mode == MemoryMode::Clear {
        crate::memory::clear(&options.home)?
    } else {
        0
    };
    Ok(Prepared {
        state: state.clone(),
        task: task.clone(),
        session: options.session,
        prompt: crate::task::render(task.prompt(options.session)?, state)?,
        repo: state.session_checkout(options.session)?,
        cwd,
        memories_cleared,
    })
}

/// Remove every worktree this run made, but only ones below the configured
/// worktree root, never the root itself, and only paths this run would have
/// made: a path another run took over is not this run's to delete.
pub fn clean(home: &crate::paths::Home, state: &crate::paths::State) -> Result<()> {
    let local = crate::paths::Local::load(home)?;
    let root = local.worktree_root(home)?;
    let mut worktrees: Vec<StateRepo> = Vec::new();
    for repo in state.all_repos() {
        let worktree = Path::new(&repo.worktree);
        let inside = within_no_self(worktree, &root);
        if !inside {
            return Err(format!(
                "refusing to remove {}: it is not under the worktree root {}",
                worktree.display(),
                root.display()
            ));
        }
        if !crate::paths::owned_worktree(&root, &state.round, &state.run, worktree) {
            return Err(format!(
                "refusing to remove {}: it is not a worktree run {} would have made",
                worktree.display(),
                state.run
            ));
        }
        if worktrees.iter().any(|seen| seen.worktree == repo.worktree) {
            continue;
        }
        worktrees.push(repo);
    }
    for repo in worktrees {
        let worktree = Path::new(&repo.worktree);
        if worktree.exists() {
            git(
                Path::new(&repo.repo),
                &["worktree", "remove", "--force", &repo.worktree],
            )?;
        } else {
            // A worktree removed by hand leaves stale metadata in the repository.
            let _ = git(Path::new(&repo.repo), &["worktree", "prune"]);
        }
    }
    // A task without a main repository has its session's directory between the
    // removed worktrees; remove it when nothing else is left in it.
    if !state.has_main_repo() {
        let dir = Path::new(&state.worktree);
        let expected = local.session_dir_path(home, &state.round, &state.run)?;
        if !within_no_self(dir, &root) || *dir != expected {
            return Err(format!(
                "refusing to remove {}: it is not the directory run {} would have made",
                dir.display(),
                state.run
            ));
        }
        if dir.exists() {
            match fs::remove_dir(dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => {
                    eprintln!(
                        "note: {} still holds files this run did not make; left it",
                        dir.display()
                    );
                }
                Err(error) => {
                    return Err(format!("cannot remove {}: {error}", dir.display()));
                }
            }
        }
    }
    Ok(())
}

/// Whether `path` is below `dir`, not `dir` itself.
fn within_no_self(path: &Path, dir: &Path) -> bool {
    crate::paths::within(path, dir) && !crate::paths::within(dir, path)
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
