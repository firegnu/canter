//! Run the task's checks in the worktree and keep a short tail of each result.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub id: String,
    pub description: String,
    pub record: bool,
    /// None for a record check.
    pub passed: Option<bool>,
    /// The number a record check prints.
    pub value: Option<i64>,
    pub output_tail: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckFile {
    pub ran_at: i64,
    pub results: Vec<CheckResult>,
}

pub fn grade(
    home: &crate::paths::Home,
    round: &str,
    run: &str,
    out: &mut impl std::io::Write,
) -> Result<CheckFile> {
    let state = crate::paths::State::load(home, round, run)?;
    let task = state.task()?;
    // Single-session checks can inspect replies and tools even without chat.
    if task.session_count() == 1 {
        match write_session_data(home, &state) {
            Ok(path) => {
                writeln!(out, "session data: {}", path.display())
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => {
                writeln!(out, "note: session data was not written: {error}")
                    .map_err(|error| error.to_string())?;
            }
        }
    }
    let mut results = Vec::with_capacity(task.checks.len());
    for check in &task.checks {
        // A check runs in the worktree of its own repository, with that
        // repository's start commit and path, or in the session's directory
        // when the task has no main repository.
        let target = state.check_checkout(&task, check)?;
        let worktree = target.as_ref().map_or_else(
            || state.worktree().to_path_buf(),
            |repo| PathBuf::from(&repo.worktree),
        );
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&check.command)
            .current_dir(&worktree)
            .env("RUN_DIR", home.run_dir(round, run));
        if let Some(repo) = &target {
            command
                .env("BASE", &repo.base_commit)
                .env("REPO", &repo.repo)
                .env("WORKTREE", &repo.worktree);
        }
        let output = command
            .output()
            .map_err(|error| format!("cannot run check {}: {error}", check.id))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        if !stderr.trim().is_empty() {
            if !text.ends_with('\n') && !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&stderr);
        }
        let output_tail = tail(&text);
        let (passed, value) = if check.record {
            (None, number(&output.stdout))
        } else {
            (Some(output.status.success()), None)
        };
        let line = match (passed, value) {
            (Some(true), _) => "pass".to_string(),
            (Some(false), _) => "FAIL".to_string(),
            (None, Some(value)) => format!("record {value}"),
            (None, None) => "record ?".to_string(),
        };
        writeln!(out, "[{line}] {} — {}", check.id, check.description)
            .map_err(|error| error.to_string())?;
        if passed == Some(false) || (check.record && value.is_none()) {
            for line in output_tail.lines() {
                writeln!(out, "    {line}").map_err(|error| error.to_string())?;
            }
        }
        results.push(CheckResult {
            id: check.id.clone(),
            description: check.description.clone(),
            record: check.record,
            passed,
            value,
            output_tail,
        });
    }
    let file = CheckFile {
        ran_at: crate::now_ms(),
        results,
    };
    crate::paths::write_private(
        &home.run_dir(round, run).join("checks.json"),
        serde_json::to_vec_pretty(&file)
            .map_err(|error| format!("cannot encode the check results: {error}"))?
            .as_slice(),
    )?;
    Ok(file)
}

fn write_session_data(home: &crate::paths::Home, state: &crate::paths::State) -> Result<PathBuf> {
    let session_path = crate::paths::session_id_file(home, state, 1);
    let session_id = crate::paths::read_session_id(&session_path)?.ok_or_else(|| {
        format!(
            "session 1 has no id yet: {} is missing\nrun `canter-eval steer` while the session is open",
            session_path.display()
        )
    })?;
    let local = crate::paths::Local::load(home)?;
    let text = crate::server::run_canter(
        &crate::server::canter_binary(&local),
        &["stats", &session_id, "--json"],
        home,
    )?;
    let stats: canter_protocol::StatsResponse = serde_json::from_str(&text)
        .map_err(|error| format!("the stats JSON of {session_id} is not valid: {error}"))?;
    let data = crate::session_data::build(home, state, &stats)?;
    let path = home
        .run_dir(&state.round, &state.run)
        .join("session-data.json");
    crate::paths::write_private(
        &path,
        serde_json::to_vec_pretty(&data)
            .map_err(|error| format!("cannot encode the session data: {error}"))?
            .as_slice(),
    )?;
    Ok(path)
}

fn number(stdout: &[u8]) -> Option<i64> {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.trim().parse().ok())
}

fn tail(text: &str) -> String {
    let text = text.trim_end();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(20);
    let joined = lines[start..].join("\n");
    let chars: Vec<char> = joined.chars().collect();
    let start = chars.len().saturating_sub(2000);
    chars[start..].iter().collect()
}

pub fn load(home: &crate::paths::Home, round: &str, run: &str) -> Result<CheckFile> {
    let path = home.run_dir(round, run).join("checks.json");
    let text = fs::read_to_string(&path).map_err(|error| {
        format!(
            "cannot read {}: {error}\nrun `canter-eval grade` first",
            path.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|error| format!("{} is not valid: {error}", path.display()))
}

pub fn worktree_head(state: &crate::paths::State) -> Result<String> {
    let output = crate::prepare::git(state.worktree(), &["rev-parse", "HEAD"])?;
    Ok(output.trim().to_string())
}

pub fn ensure_worktree(state: &crate::paths::State) -> Result<()> {
    if !Path::new(&state.worktree).is_dir() {
        return Err(format!(
            "the worktree is gone: {}\nprepare the run again or clean it",
            state.worktree
        ));
    }
    Ok(())
}
