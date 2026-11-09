//! Record the end of one session of a run: the worktree HEAD, the shell
//! commands the session ran, and the memory files it left behind.

use std::fs;
use std::path::Path;

use canter_protocol::{Event, EventPayload, Item};
use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySnapshot {
    pub files: Vec<String>,
}

#[derive(Debug)]
pub struct Finished {
    pub head: String,
    pub commands: usize,
    pub memory_files: usize,
}

pub fn finish(home: &crate::paths::Home, round: &str, run: &str, index: usize) -> Result<Finished> {
    let state = crate::paths::State::load(home, round, run)?;
    let count = state.session_count.max(1);
    if index == 0 || index > count {
        return Err(format!(
            "--session must be between 1 and {count} for this run"
        ));
    }
    let session_path = crate::paths::session_id_file(home, &state, index);
    let session_id = crate::paths::read_session_id(&session_path)?.ok_or_else(|| {
        format!(
            "session {index} has no id yet; run `canter-eval steer --session {index}` while it is open and writes {}",
            session_path.display()
        )
    })?;
    let dir = home.session_dir(round, run, index);
    if !dir.is_dir() {
        return Err(format!(
            "session {index} was not prepared: {} is missing",
            dir.display()
        ));
    }
    let head_path = home.session_head_path(round, run, index);
    if head_path.exists() {
        return Err(format!(
            "session {index} is already recorded in {}\nremove that file to record it again",
            head_path.display()
        ));
    }
    let events_path = home.session_events(&session_id);
    let events = fs::read_to_string(&events_path).map_err(|error| {
        format!("cannot read the events of {session_id}: {error}\nthe eval home may be wrong")
    })?;
    // The session's own repository: each repository has its own worktree.
    let repo = state.session_repo(index)?;
    let head = crate::prepare::git(Path::new(&repo.worktree), &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let commands = commands_from_events(&events);
    let memory = MemorySnapshot {
        files: crate::memory::files(home)?,
    };
    crate::paths::write_private(&head_path, format!("{head}\n").as_bytes())?;
    let mut text = String::new();
    for command in &commands {
        text.push_str(command);
        text.push('\n');
    }
    crate::paths::write_private(
        &home.session_commands_path(round, run, index),
        text.as_bytes(),
    )?;
    crate::paths::write_private(
        &home.session_memory_path(round, run, index),
        serde_json::to_vec_pretty(&memory)
            .map_err(|error| format!("cannot encode the memory snapshot: {error}"))?
            .as_slice(),
    )?;
    Ok(Finished {
        head,
        commands: commands.len(),
        memory_files: memory.files.len(),
    })
}

/// Each completed `commandExecution` item's command, one line per command.
/// Newlines inside a command become `; ` so command boundaries stay visible;
/// the commands are not parsed any further.
pub fn commands_from_events(text: &str) -> Vec<String> {
    let mut commands = Vec::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Event>(line) else {
            continue;
        };
        if let EventPayload::ItemCompleted {
            item: Item::CommandExecution { command, .. },
            ..
        } = event.payload
        {
            commands.push(
                command
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .replace('\n', "; "),
            );
        }
    }
    commands
}
