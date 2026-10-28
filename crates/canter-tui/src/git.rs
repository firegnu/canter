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

/// Bytes kept of one command's output, and of the diffs in all.
pub(crate) const LIMIT: usize = 256 * 1024;
/// Bytes kept of the untracked file list.
pub(crate) const LIST_LIMIT: usize = 32 * 1024;

/// What a command printed, at most [`LIMIT`] bytes of it.
pub(crate) struct Output {
    pub text: String,
    pub truncated: bool,
}

/// The uncommitted changes of a repository, as git printed them.
pub(crate) struct Local {
    /// The repository's top directory, which the diffs' paths are relative to.
    pub root: String,
    /// [`filter_overrides`] for the repository.
    pub filters: Vec<String>,
    pub staged: Output,
    pub unstaged: Output,
    /// `git ls-files --others --exclude-standard`, one path a line.
    pub untracked: Output,
}

/// The staged and unstaged diffs and the untracked files of the repository
/// holding `cwd`, without running programs the repository configures. An
/// error is `<program>: <why>`, as [`run`] gives it.
pub(crate) async fn local(cwd: &str) -> Result<Local, String> {
    let root = run(command(cwd).args(["rev-parse", "--show-toplevel"])).await?;
    let root = root
        .text
        .strip_suffix('\n')
        .unwrap_or(&root.text)
        .to_owned();
    let filters = filter_overrides(&root).await.map_err(|e| e.to_string())?;
    let staged = run(&mut diff(&root, &filters, true, false)).await?;
    let unstaged = run(&mut diff(&root, &filters, false, false)).await?;
    let untracked =
        run(command(&root).args(["ls-files", "--others", "--exclude-standard"])).await?;
    Ok(Local {
        root,
        filters,
        staged,
        unstaged,
        untracked,
    })
}

/// `git diff` in `root` with `filters` ([`filter_overrides`]): no external
/// diff, textconv or colour; `cached` for the staged changes, `stat` for
/// `--stat`.
pub(crate) fn diff(
    root: &str,
    filters: &[String],
    cached: bool,
    stat: bool,
) -> tokio::process::Command {
    let mut command = diff_options(root, filters);
    if cached {
        command.arg("--cached");
    }
    if stat {
        command.arg("--stat");
    }
    command.arg("--");
    command
}

/// `git diff` and its options as [`diff`] runs it, before what to compare.
fn diff_options(root: &str, filters: &[String]) -> tokio::process::Command {
    let mut command = command(root);
    command.env("GIT_NO_LAZY_FETCH", "1").args(filters).args([
        "-c",
        "diff.submodule=short",
        "-c",
        "diff.mnemonicPrefix=false",
        "-c",
        "diff.relative=false",
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--no-renames",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        "--ignore-submodules=none",
    ]);
    command
}

/// The lines added and removed by the uncommitted changes of the repository
/// holding `cwd`, staged or not, against HEAD (`git diff HEAD --numstat`);
/// before the first commit, the staged ones. Binary files count no lines and
/// untracked files are left out. Run as [`local`] runs git, read-only and
/// within 2 seconds; `None` outside a repository, when git fails or is late.
pub(crate) async fn changed_lines(cwd: &str) -> Option<(u64, u64)> {
    let count = async {
        let root = run(command(cwd).args(["rev-parse", "--show-toplevel"]))
            .await
            .ok()?;
        let root = root.text.strip_suffix('\n').unwrap_or(&root.text);
        let filters = filter_overrides(root).await.ok()?;
        let head = command(root)
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .output()
            .await
            .ok()?;
        let mut diff = diff_options(root, &filters);
        diff.arg("--numstat")
            .arg(if head.status.success() {
                "HEAD"
            } else {
                "--cached"
            })
            .arg("--");
        let output = run(&mut diff).await.ok()?;
        // A count of only the start would be wrong.
        if output.truncated {
            return None;
        }
        let mut lines = (0, 0);
        for line in output.text.lines() {
            // `<added>\t<removed>\t<path>`; `-\t-\t<path>` for a binary file.
            let mut fields = line.split('\t');
            let added = fields.next()?;
            let removed = fields.next()?;
            if added != "-" {
                lines.0 += added.parse::<u64>().ok()?;
                lines.1 += removed.parse::<u64>().ok()?;
            }
        }
        Some(lines)
    };
    tokio::time::timeout(std::time::Duration::from_secs(2), count)
        .await
        .ok()?
}

/// The longest start of `text` at most `limit` bytes long.
pub(crate) fn prefix(text: &str, limit: usize) -> &str {
    let mut end = text.len().min(limit);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

async fn read_bounded(mut reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Output> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; 8192];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let keep = count.min(LIMIT.saturating_sub(bytes.len()));
        bytes.extend_from_slice(&buffer[..keep]);
        truncated |= keep < count;
    }
    Ok(Output {
        text: String::from_utf8_lossy(&bytes).into_owned(),
        truncated,
    })
}

/// Runs `command` (git or gh) to the end, keeping at most [`LIMIT`] bytes
/// of what it prints. It failing is `<program>: <its error output>`.
pub(crate) async fn run(command: &mut tokio::process::Command) -> Result<Output, String> {
    use std::process::Stdio;
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let result = async {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let (stdout, stderr, status) = tokio::join!(
            read_bounded(child.stdout.take().unwrap()),
            read_bounded(child.stderr.take().unwrap()),
            child.wait(),
        );
        Ok::<_, std::io::Error>((stdout?, stderr?, status?))
    }
    .await
    .map_err(|e| format!("{program}: {e}"))?;
    if !result.2.success() {
        return Err(format!("{program}: {}", result.1.text.trim()));
    }
    Ok(result.0)
}
