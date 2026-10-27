//! The info-row branch read from `.git`, and a bounded read-only git status
//! query for `/files`.

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

/// What `/files` shows of git.
pub(crate) enum Status {
    /// The branch, and each entry's two-letter code and path as shown.
    Repository {
        branch: String,
        entries: Vec<(String, String)>,
    },
    /// Why there is no status, such as `not a git repository`.
    Unavailable(&'static str),
}

/// The branch in the first line of `git status --branch`, with its upstream
/// and how far apart they are in git's words: `main...origin/main [ahead 1]`
/// is `main  origin/main · ahead 1`; `No commits yet on main` is `main`.
fn branch_of(header: &str) -> String {
    let header = header.trim_start_matches("## ");
    let header = header
        .strip_prefix("No commits yet on ")
        .or_else(|| header.strip_prefix("Initial commit on "))
        .unwrap_or(header);
    let Some((branch, upstream)) = header.split_once("...") else {
        return header.split(" [").next().unwrap_or(header).to_owned();
    };
    match upstream.split_once(" [") {
        Some((upstream, apart)) => {
            format!("{branch}  {upstream} · {}", apart.trim_end_matches(']'))
        }
        None => format!("{branch}  {upstream}"),
    }
}

/// Read-only status, with all git work bounded by one timeout. Dropping an
/// unfinished output future kills and reaps its child through tokio.
pub(crate) async fn status(cwd: &str, home: Option<&str>) -> Status {
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let root = git_output(cwd, &["rev-parse", "--show-toplevel"]).await?;
        if !root.status.success() {
            return Ok(Status::Unavailable(
                if String::from_utf8_lossy(&root.stderr).contains("not a git repository") {
                    "not a git repository"
                } else {
                    "could not read repository status"
                },
            ));
        }
        let root = String::from_utf8_lossy(&root.stdout);
        let root = Path::new(root.strip_suffix('\n').unwrap_or(&root));
        // git resolves directory symlinks (notably /var -> /private/var on macOS).
        let display_cwd = tokio::fs::canonicalize(cwd).await?;
        let display_home = match home {
            Some(home) => tokio::fs::canonicalize(home).await.ok(),
            None => None,
        };
        let mut arguments = filter_overrides(cwd).await?;
        arguments.extend(
            [
                "status",
                "--porcelain=v1",
                "-z",
                "--branch",
                "--untracked-files=all",
                "--no-renames",
            ]
            .map(str::to_owned),
        );
        let output = git_output(
            cwd,
            &arguments.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .await?;
        if !output.status.success() {
            return Ok(Status::Unavailable("could not read repository status"));
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let mut entries = text.split('\0').filter(|entry| !entry.is_empty());
        let branch = branch_of(entries.next().unwrap_or_default());
        let mut changes = Vec::new();
        for entry in entries {
            if let Some(path) = entry.get(3..) {
                changes.push((
                    entry[..2].to_owned(),
                    crate::files::display_path(
                        &root.join(path),
                        &display_cwd.to_string_lossy(),
                        display_home.as_ref().and_then(|home| home.to_str()),
                    ),
                ));
            }
        }
        Ok::<_, std::io::Error>(Status::Repository {
            branch,
            entries: changes,
        })
    })
    .await;
    match result {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => Status::Unavailable("could not run git status"),
        Err(_) => Status::Unavailable("status timed out"),
    }
}

// Status may hash worktree contents through a configured clean/process filter.
// Disable these in the superproject and in status's recursive submodule queries.
pub(crate) async fn filter_overrides(cwd: &str) -> std::io::Result<Vec<String>> {
    const KEYS: &str = r"^filter\..*\.(clean|smudge|process|required)$";
    let local = git_output(
        cwd,
        &["config", "--null", "--name-only", "--get-regexp", KEYS],
    )
    .await?;
    if !local.status.success() && local.status.code() != Some(1) {
        return Err(std::io::Error::other("could not inspect git filters"));
    }
    // Fixed command: no repository-provided strings are interpolated into a shell.
    let modules = git_output(cwd, &["submodule", "--quiet", "foreach", "--recursive",
        r#"git config --null --name-only --get-regexp '^filter\..*\.(clean|smudge|process|required)$' || test "$?" = 1"#,
    ]).await?;
    if !modules.status.success() {
        return Err(std::io::Error::other("could not inspect submodule filters"));
    }
    let mut arguments = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for key in local
        .stdout
        .split(|&byte| byte == 0)
        .chain(modules.stdout.split(|&byte| byte == 0))
        .filter(|key| !key.is_empty())
    {
        let key = std::str::from_utf8(key).map_err(std::io::Error::other)?;
        if seen.insert(key) {
            arguments.push("-c".into());
            arguments.push(format!(
                "{key}={}",
                if key.ends_with(".required") {
                    "false"
                } else {
                    ""
                }
            ));
        }
    }
    Ok(arguments)
}

async fn git_output(cwd: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    command(cwd).args(args).output().await
}

pub(crate) fn command(cwd: &str) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("git");
    command
        .current_dir(cwd)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("LC_ALL", "C")
        .args([
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "status.relativePaths=false",
        ])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    command
}
