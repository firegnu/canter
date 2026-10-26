//! The version line `canter --version` prints and the welcome header shows
//! after `canter`: `0.1.0 · 37489fe`, the commit followed by `-dirty` when
//! the working tree has changes not committed, or only `0.1.0` when git
//! cannot tell (not a git repository, or no git). The build script
//! (`build.rs`) works it out once; this file is shared with it.

use std::ffi::OsStr;
use std::path::Path;
use std::process::Command;

/// `version`, then the commit when there is one.
pub fn line(version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("{version} · {commit}"),
        None => version.to_owned(),
    }
}

/// The short commit of the git repository `dir` is in, with `-dirty` when
/// tracked files have changes not committed. `None` when `git` (the
/// program) cannot run or `dir` is not in a repository.
pub fn commit(git: &OsStr, dir: &Path) -> Option<String> {
    let hash = run(git, dir, &["rev-parse", "--short", "HEAD"])?;
    // `--no-optional-locks`: only looks, never rewrites the index.
    let changes = run(
        git,
        dir,
        &[
            "--no-optional-locks",
            "status",
            "--porcelain",
            "--untracked-files=no",
        ],
    )?;
    Some(if changes.is_empty() {
        hash
    } else {
        format!("{hash}-dirty")
    })
}

/// What `git args` prints in `dir`, trimmed; `None` when it fails.
pub fn run(git: &OsStr, dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new(git)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const GIT: &str = "git";

    /// Runs git in `dir` without the user's or the system's settings.
    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new(GIT)
            .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn the_line_has_the_commit_only_when_there_is_one() {
        assert_eq!(line("0.1.0", Some("37489fe")), "0.1.0 · 37489fe");
        assert_eq!(
            line("0.1.0", Some("37489fe-dirty")),
            "0.1.0 · 37489fe-dirty"
        );
        assert_eq!(line("0.1.0", None), "0.1.0");
    }

    #[test]
    fn no_commit_outside_a_repository_or_without_git() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(commit(GIT.as_ref(), dir.path()), None);
        let missing = dir.path().join("no-such-git");
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"));
        assert_eq!(commit(missing.as_os_str(), repo), None);
    }

    #[test]
    fn the_commit_is_marked_dirty_while_tracked_files_change() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        git(dir, &["init", "--quiet"]);
        fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(dir, &["add", "a.txt"]);
        git(dir, &["commit", "--quiet", "-m", "one"]);
        let clean = commit(GIT.as_ref(), dir).expect("a commit");
        assert!(
            clean.len() >= 7 && clean.chars().all(|c| c.is_ascii_hexdigit()),
            "{clean}"
        );
        // Files git does not track do not count.
        fs::write(dir.join("new.txt"), "new\n").unwrap();
        assert_eq!(commit(GIT.as_ref(), dir).as_deref(), Some(clean.as_str()));
        fs::write(dir.join("a.txt"), "two\n").unwrap();
        assert_eq!(commit(GIT.as_ref(), dir), Some(format!("{clean}-dirty")));
    }
}
