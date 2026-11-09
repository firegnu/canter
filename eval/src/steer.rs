//! Send the task's steers after the counted commits appear.

use std::{fs, time::Duration};

use serde::{Deserialize, Serialize};

use crate::{Result, paths::State};

const POLL: Duration = Duration::from_secs(2);

#[derive(Debug, Default, Deserialize, Serialize)]
struct Progress {
    sent: Vec<usize>,
}

pub struct SteerOptions {
    pub home: crate::paths::Home,
    pub round: String,
    pub run: String,
    /// One-based session of a multi-session task.
    pub session: usize,
    pub poll: Duration,
}

impl SteerOptions {
    pub fn new(home: crate::paths::Home, round: String, run: String, session: usize) -> Self {
        Self {
            home,
            round,
            run,
            session,
            poll: POLL,
        }
    }
}

pub async fn steer(options: SteerOptions) -> Result<()> {
    let state = State::load(&options.home, &options.round, &options.run)?;
    let task = state.task()?;
    let session_count = state.session_count.max(1);
    if options.session == 0 || options.session > session_count {
        return Err(format!(
            "--session must be between 1 and {session_count} for this run"
        ));
    }
    let registration = crate::server::registration(&options.home)?;
    let session = wait_for_session(
        &options.home,
        &registration,
        &state,
        options.session,
        options.poll,
    )
    .await?;
    if task.steers.is_empty() {
        println!("task {} defines no steers", task.id);
        return Ok(());
    }
    let progress_path = options
        .home
        .run_dir(&options.round, &options.run)
        .join("steers.json");
    let mut progress = match fs::read_to_string(&progress_path) {
        Ok(text) => serde_json::from_str::<Progress>(&text)
            .map_err(|error| format!("{} is not valid: {error}", progress_path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Progress::default(),
        Err(error) => {
            return Err(format!("cannot read {}: {error}", progress_path.display()));
        }
    };

    for (index, steer) in task.steers.iter().enumerate() {
        if progress.sent.contains(&index) {
            continue;
        }
        let count = wait_for_commits(&state, steer.after_commits, options.poll).await?;
        println!(
            "steer {}: {} ritual: commits ({}, wanted {})",
            index + 1,
            count,
            state.run,
            steer.after_commits
        );
        let registration = crate::server::registration(&options.home)?;
        let (status, body) = crate::server::steer(&registration, &session, &steer.text).await?;
        if !(200..300).contains(&status) {
            return Err(format!(
                "the eval server rejected steer {} for {session}: HTTP {status} {body}",
                index + 1
            ));
        }
        progress.sent.push(index);
        crate::paths::write_private(
            &progress_path,
            serde_json::to_vec_pretty(&progress)
                .map_err(|error| format!("cannot encode steer progress: {error}"))?
                .as_slice(),
        )?;
        println!("steer {} sent", index + 1);
    }
    println!("all {} steers sent", task.steers.len());
    Ok(())
}

async fn wait_for_commits(state: &State, wanted: u32, poll: Duration) -> Result<u32> {
    let mut last = None;
    loop {
        let count = ritual_commits(state)?;
        if count >= wanted {
            return Ok(count);
        }
        if last != Some(count) {
            println!("waiting for ritual: commits: {count}/{wanted}");
            last = Some(count);
        }
        tokio::time::sleep(poll).await;
    }
}

/// Commits after the base commit whose subject starts with `ritual: `.
pub fn ritual_commits(state: &State) -> Result<u32> {
    let output = crate::prepare::git(
        state.worktree(),
        &[
            "log",
            "--reverse",
            "--format=%s",
            &format!("{}..HEAD", state.base_commit),
        ],
    )?;
    let count = output
        .lines()
        .filter(|line| line.starts_with("ritual: "))
        .count();
    u32::try_from(count).map_err(|_| "too many commits".into())
}

/// The session id from this session's `session.json`, or else the new session
/// the eval server lists in the run's worktree, saved for `finish` and `collect`.
async fn wait_for_session(
    home: &crate::paths::Home,
    registration: &crate::server::Registration,
    state: &State,
    index: usize,
    poll: Duration,
) -> Result<String> {
    let path = home.session_id_path(&state.round, &state.run, index);
    loop {
        if let Some(session) = crate::paths::read_session_id(&path)? {
            return Ok(session);
        }
        let exclude = home.recorded_session_ids(&state.round, &state.run)?;
        if let Some(session) =
            crate::server::find_session(registration, state.worktree(), &exclude).await?
        {
            let body = serde_json::json!({ "sessionId": session });
            crate::paths::write_private(&path, body.to_string().as_bytes())?;
            println!("session {session}");
            return Ok(session);
        }
        println!("waiting for a session in {}", state.worktree.as_str());
        tokio::time::sleep(poll).await;
    }
}
