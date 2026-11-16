//! The original TUI file walk, isolated in a worker so ignore's process-wide
//! HOME, XDG and git config discovery uses this session's environment.
use canter_protocol::{WorkspaceEntriesBatch, WorkspaceEntry};
use std::{
    io::{self, Write},
    path::Path,
};

pub const WORKER_ARGUMENT: &str = "--internal-workspace-files";
const LIMIT: usize = 10_000;
const BATCH: usize = 256;
const SKIPPED: [&str; 4] = [".git", ".hg", ".svn", ".jj"];

pub fn worker_main() -> std::process::ExitCode {
    let result = (|| -> io::Result<()> {
        let directories: Vec<String> = serde_json::from_reader(io::stdin().lock())?;
        let mut output = io::stdout().lock();
        scan(&directories, |batch| {
            serde_json::to_writer(&mut output, &batch)?;
            output.write_all(b"\n")?;
            output.flush()
        })
    })();
    if result.is_ok() {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::FAILURE
    }
}

pub(crate) fn scan(
    directories: &[String],
    mut send: impl FnMut(WorkspaceEntriesBatch) -> io::Result<()>,
) -> io::Result<()> {
    let mut truncated = false;
    for (index, root) in directories.iter().enumerate() {
        walk(Path::new(root), index != 0, &mut |entries, cut| {
            truncated |= cut;
            send(WorkspaceEntriesBatch {
                entries,
                truncated,
                done: false,
                error: None,
            })
        })?;
    }
    send(WorkspaceEntriesBatch {
        entries: Vec::new(),
        truncated,
        done: true,
        error: None,
    })
}

fn entry(root: &Path, entry: &ignore::DirEntry, absolute: bool) -> Option<WorkspaceEntry> {
    let relative = entry.path().strip_prefix(root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    let path = if absolute { entry.path() } else { relative }.to_str()?;
    if path.is_empty() || path.contains(|c: char| c == '"' || c.is_control()) {
        return None;
    }
    let dir = entry.file_type().is_some_and(|kind| kind.is_dir());
    let label = if dir {
        format!("{path}/")
    } else {
        path.to_owned()
    };
    Some(WorkspaceEntry { label, dir })
}

fn walk(
    root: &Path,
    absolute: bool,
    send: &mut impl FnMut(Vec<WorkspaceEntry>, bool) -> io::Result<()>,
) -> io::Result<()> {
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .ignore(false)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .parents(true)
        .follow_links(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| SKIPPED.contains(&name))
        })
        .build();
    let mut batch = Vec::new();
    let mut count = 0;
    for found in walker {
        let Some(entry) = found.ok().and_then(|found| entry(root, &found, absolute)) else {
            continue;
        };
        if count == LIMIT {
            send(batch, true)?;
            return Ok(());
        }
        batch.push(entry);
        count += 1;
        if batch.len() == BATCH {
            send(std::mem::take(&mut batch), false)?;
        }
    }
    send(batch, false)
}
