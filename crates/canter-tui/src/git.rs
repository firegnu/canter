//! The git branch of the session's directory for the info rows, read straight
//! from `.git` rather than by running git.

use std::fs;
use std::path::Path;

/// The branch checked out in the repository holding `dir`, or the short
/// commit when HEAD is detached. `None` outside a repository, for a relative
/// `dir`, or when it cannot be read.
pub(crate) fn branch(dir: &str) -> Option<String> {
    let dir = Path::new(dir);
    if !dir.is_absolute() {
        return None;
    }
    let dot_git = dir
        .ancestors()
        .map(|dir| dir.join(".git"))
        .find(|path| path.exists())?;
    let git_dir = if dot_git.is_file() {
        // A worktree: `gitdir: <path>`, relative to the directory holding the file.
        let text = fs::read_to_string(&dot_git).ok()?;
        let path = text.strip_prefix("gitdir:")?.trim();
        dot_git.parent()?.join(path)
    } else {
        dot_git
    };
    let head = fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    match head.strip_prefix("ref:") {
        Some(name) => {
            let name = name.trim();
            Some(name.strip_prefix("refs/heads/").unwrap_or(name).to_owned())
        }
        None => head
            .get(..7)
            .filter(|_| head.chars().all(|c| c.is_ascii_hexdigit()))
            .map(str::to_owned),
    }
}
