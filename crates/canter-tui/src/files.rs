use crate::approval::shorten_home;
use canter_protocol::{Item, ItemStatus, PatchChangeKind, SessionSnapshot};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

pub(crate) fn display_path(path: &Path, cwd: &str, home: Option<&str>) -> String {
    match path.strip_prefix(cwd) {
        Ok(relative) => relative.to_string_lossy().into_owned(),
        Err(_) => shorten_home(&path.to_string_lossy(), home),
    }
}

pub(crate) async fn lines(snapshot: &SessionSnapshot, home: Option<&str>) -> Vec<String> {
    let cwd = &snapshot.settings.cwd;
    let mut read = Vec::new();
    let mut seen = HashSet::new();
    let mut changed: Vec<(PathBuf, Vec<&str>)> = Vec::new();
    let mut positions = HashMap::new();
    for item in snapshot.turns.iter().flat_map(|turn| &turn.items) {
        match item {
            Item::FileRead {
                path,
                status: ItemStatus::Completed,
                ..
            } => {
                let path = Path::new(cwd).join(path);
                if seen.insert(path.clone()) {
                    read.push(path);
                }
            }
            Item::FileChange {
                changes,
                status: ItemStatus::Completed,
                ..
            } => {
                for change in changes {
                    let path = Path::new(cwd).join(&change.path);
                    let kind = match change.kind {
                        PatchChangeKind::Add => "新增",
                        PatchChangeKind::Update { .. } => "修改",
                        PatchChangeKind::Delete => "删除",
                        PatchChangeKind::Unknown => "unknown",
                    };
                    let index = *positions.entry(path.clone()).or_insert_with(|| {
                        changed.push((path, Vec::new()));
                        changed.len() - 1
                    });
                    if !changed[index].1.contains(&kind) {
                        changed[index].1.push(kind);
                    }
                }
            }
            _ => {}
        }
    }
    let mut lines = vec!["Read files".into()];
    if read.is_empty() {
        lines.push("  none".into());
    }
    lines.extend(
        read.iter()
            .map(|path| format!("  {}", display_path(path, cwd, home))),
    );
    lines.push("Changed files".into());
    if changed.is_empty() {
        lines.push("  none".into());
    }
    lines.extend(changed.iter().map(|(path, kinds)| {
        format!("  {}  {}", kinds.join(" / "), display_path(path, cwd, home))
    }));
    lines.extend(crate::git::status(cwd, home).await);
    lines.push("Shell edits are not tracked in Changed files; see Git status.".into());
    lines
}
