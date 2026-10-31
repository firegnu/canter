//! Global command and file rules. Paths and HOME come from the server/session.
use canter_protocol::{FileRule, FileRuleDecision};
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

pub(crate) use canter_protocol::{CommandRule as Rule, CommandRuleDecision as Decision};

/// Reads the global file; a missing file has no rules, a broken file is an error.
pub fn list(path: &Path) -> io::Result<canter_protocol::CommandRuleListResponse> {
    let rules = Rules::read(path)?;
    Ok(canter_protocol::CommandRuleListResponse {
        path: path.to_string_lossy().into_owned(),
        data: rules.rules,
        file_rules: rules.file_rules,
    })
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rules {
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default, rename = "fileRules", skip_serializing_if = "Vec::is_empty")]
    pub file_rules: Vec<FileRule>,
}

impl Rules {
    pub fn read(path: &Path) -> io::Result<Self> {
        let Some(bytes) = read_bytes(path)? else {
            return Ok(Self::default());
        };
        Self::parse(&bytes)
    }

    pub fn read_for_file_tools(path: &Path) -> io::Result<Self> {
        let Some(bytes) = read_bytes(path)? else {
            return Ok(Self::default());
        };
        // A readable command-only file cannot opt file tools into restrictions,
        // even if its JSON is unfinished. Also recognize decoded JSON field names.
        if !bytes
            .windows(b"fileRules".len())
            .any(|part| part == b"fileRules")
            && !serde_json::from_slice::<serde_json::Value>(&bytes)
                .is_ok_and(|value| value.get("fileRules").is_some())
        {
            return Ok(Self::default());
        }
        Self::parse(&bytes)
    }

    fn parse(bytes: &[u8]) -> io::Result<Self> {
        let rules: Self = serde_json::from_slice(bytes).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid command rules JSON: {error}"),
            )
        })?;
        for (index, rule) in rules.rules.iter().enumerate() {
            if rule.pattern.is_empty()
                || rule
                    .pattern
                    .iter()
                    .any(|word| word.is_empty() || word.contains('\0'))
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("rules[{index}]: empty or invalid command prefix"),
                ));
            }
        }
        for (index, rule) in rules.file_rules.iter().enumerate() {
            file_glob(&rule.pattern).map_err(|error| file_rule_error(index, rule, error))?;
        }
        Ok(rules)
    }

    pub fn decision(&self, words: &[String]) -> Option<Decision> {
        self.rules
            .iter()
            .filter(|rule| words.starts_with(&rule.pattern))
            .map(|rule| rule.decision)
            .max()
    }

    pub fn file_decision(
        &self,
        requested: &Path,
        path: &Path,
        cwd: &Path,
        home: Option<&Path>,
    ) -> io::Result<Option<FileRuleDecision>> {
        let mut decision = None;
        for (index, rule) in self.file_rules.iter().enumerate() {
            if file_matches(&rule.pattern, requested, path, cwd, home)
                .map_err(|error| file_rule_error(index, rule, error))?
            {
                decision = decision.max(Some(rule.decision));
            }
        }
        Ok(decision)
    }
}

fn file_rule_error(index: usize, rule: &FileRule, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!("fileRules[{index}] pattern {:?}: {error}", rule.pattern),
    )
}

fn read_bytes(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            // A dangling link is a broken rules file, not an absent one.
            if fs::symlink_metadata(path).is_ok() {
                return Err(error);
            }
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn file_glob(pattern: &str) -> io::Result<globset::GlobMatcher> {
    let tail = pattern
        .find(['*', '?', '[', '{', '\\'])
        .map(|at| &pattern[at..])
        .unwrap_or_default();
    let error = if pattern.is_empty() {
        Some("file pattern must not be empty")
    } else if pattern.contains('\0') {
        Some("file pattern must not contain NUL bytes")
    } else if pattern.ends_with('/') {
        Some("file pattern must not end with '/'")
    } else if tail.split('/').any(|part| matches!(part, "." | "..")) {
        Some("file glob must not contain '.' or '..' path components")
    } else {
        None
    };
    if let Some(error) = error {
        return Err(io::Error::new(io::ErrorKind::InvalidData, error));
    }
    globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .backslash_escape(true)
        .build()
        .map(|glob| glob.compile_matcher())
        .map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid file glob: {error}"),
            )
        })
}

fn file_matches(
    pattern: &str,
    requested: &Path,
    path: &Path,
    cwd: &Path,
    home: Option<&Path>,
) -> io::Result<bool> {
    if !pattern.contains('/') {
        let glob = file_glob(pattern)?;
        return Ok([requested, path]
            .iter()
            .any(|path| path.file_name().is_some_and(|name| glob.is_match(name))));
    }
    let (base, pattern) = match pattern.strip_prefix("~/") {
        Some(pattern) => (
            home.filter(|path| path.is_absolute())
                .ok_or_else(|| io::Error::other("file rules require an absolute session HOME"))?,
            pattern,
        ),
        None => (cwd, pattern),
    };
    // Resolve the literal prefix exactly as file tools do, including symlinks and
    // nonexistent new files. Only the remaining glob is compared as a pattern.
    let split = pattern
        .find(['*', '?', '[', '{', '\\'])
        .map(|at| pattern[..at].rfind('/').map_or(0, |slash| slash + 1))
        .unwrap_or(pattern.len());
    let root = crate::executor::resolve_target(&base.join(&pattern[..split]), &[])?;
    if split == pattern.len() {
        return Ok(path.starts_with(root));
    }
    let glob = file_glob(&pattern[split..])?;
    Ok(path
        .strip_prefix(root)
        .is_ok_and(|relative| glob.is_match(relative)))
}

// Reference only: Codex rust-v0.163.0 core/src/exec_policy.rs,
// BANNED_PREFIX_SUGGESTIONS. We also suppress shell builtins and common wrappers.
pub(crate) fn suggested_prefix(words: &[String]) -> Option<Vec<String>> {
    let program = Path::new(words.first()?).file_name()?.to_str()?;
    if matches!(
        program,
        "bash"
            | "sh"
            | "zsh"
            | "dash"
            | "fish"
            | "ksh"
            | "cmd"
            | "cmd.exe"
            | "powershell"
            | "powershell.exe"
            | "pwsh"
            | "Rscript"
            | "julia"
            | "lua"
            | "node"
            | "nodejs"
            | "osascript"
            | "perl"
            | "php"
            | "py"
            | "pyw"
            | "pypy"
            | "pypy3"
            | "python"
            | "python3"
            | "pythonw"
            | "ruby"
            | "bun"
            | "deno"
            | "env"
            | "sudo"
            | "doas"
            | "command"
            | "exec"
            | "eval"
            | "source"
            | "."
            | "xargs"
            | "nice"
            | "nohup"
            | "timeout"
            | "time"
            | "rm"
            | "find"
            | "sed"
    ) || program.strip_prefix("python").is_some_and(|tail| {
        !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit() || c == '.')
    }) {
        return None;
    }
    let has_subcommand = matches!(
        program,
        "git"
            | "cargo"
            | "go"
            | "gh"
            | "docker"
            | "podman"
            | "kubectl"
            | "npm"
            | "pnpm"
            | "yarn"
            | "uv"
            | "rustup"
    );
    if has_subcommand {
        let subcommand = words.get(1)?;
        if subcommand.is_empty()
            || subcommand.starts_with(['-', '+'])
            || matches!(
                (program, subcommand.as_str()),
                ("npm" | "pnpm" | "yarn", "run" | "exec")
                    | ("pnpm" | "yarn", "dlx")
                    | ("uv" | "rustup", "run")
                    | ("docker", "run" | "exec")
                    | ("kubectl", "exec")
            )
        {
            return None;
        }
        return Some(words[..2].to_vec());
    }
    // Known commands without subcommands, or a bare executable. Unknown argument
    // conventions get no suggestion rather than mistaking an operand for a subcommand.
    if words.len() == 1
        || matches!(
            program,
            "touch"
                | "mkdir"
                | "cp"
                | "mv"
                | "install"
                | "make"
                | "cmake"
                | "ctest"
                | "rg"
                | "grep"
                | "cat"
                | "head"
                | "tail"
                | "wc"
                | "ls"
                | "sort"
                | "uniq"
                | "pwd"
                | "echo"
                | "printf"
        )
    {
        return Some(words[..1].to_vec());
    }
    None
}

pub(crate) fn can_persist_prefix(prefix: &[String]) -> bool {
    let Some(program) = prefix.first() else {
        return false;
    };
    (Path::new(program).is_absolute() || !program.contains('/'))
        && suggested_prefix(prefix).as_deref() == Some(prefix)
}

// One server process owns the global file. Serialize read/append/replace across sessions.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn append(path: &Path, prefixes: &[Vec<String>]) -> io::Result<()> {
    let _lock = WRITE_LOCK.lock().unwrap();
    let mut rules = Rules::read(path)?;
    if prefixes.is_empty()
        || prefixes.iter().any(|prefix| {
            !can_persist_prefix(prefix)
                || matches!(
                    rules.decision(prefix),
                    Some(Decision::Prompt | Decision::Forbidden)
                )
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command rule proposal is no longer valid",
        ));
    }
    for prefix in prefixes {
        if !rules
            .rules
            .iter()
            .any(|rule| rule.pattern == *prefix && rule.decision == Decision::Allow)
        {
            rules.rules.push(Rule {
                pattern: prefix.clone(),
                decision: Decision::Allow,
            });
        }
    }
    write(path, &rules)
}

/// Removes every exact match while holding the same lock as permanent approvals.
/// A missing rule is a no-op; an unreadable or invalid file is never replaced.
pub fn delete(path: &Path, rule: &canter_protocol::CommandRule) -> io::Result<usize> {
    let _lock = WRITE_LOCK.lock().unwrap();
    let mut rules = Rules::read(path)?;
    let before = rules.rules.len();
    rules.rules.retain(|candidate| candidate != rule);
    let removed = before - rules.rules.len();
    if removed > 0 {
        write(path, &rules)?;
    }
    Ok(removed)
}

/// File deletions share the command writer's lock and preserve command rules.
pub fn delete_file(path: &Path, rule: &FileRule) -> io::Result<usize> {
    let _lock = WRITE_LOCK.lock().unwrap();
    let mut rules = Rules::read(path)?;
    let before = rules.file_rules.len();
    rules.file_rules.retain(|candidate| candidate != rule);
    let removed = before - rules.file_rules.len();
    if removed > 0 {
        write(path, &rules)?;
    }
    Ok(removed)
}

fn write(path: &Path, rules: &Rules) -> io::Result<()> {
    use std::{io::Write, os::unix::fs::PermissionsExt};
    let mut file = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| io::Error::other("missing rules directory"))?,
    )?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut file, rules).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
