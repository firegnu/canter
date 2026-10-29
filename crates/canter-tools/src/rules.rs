//! Global command-prefix rules. The server supplies the path; no process HOME is read here.
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Decision {
    Allow,
    Prompt,
    Forbidden,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rule {
    pub pattern: Vec<String>,
    pub decision: Decision,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rules {
    pub rules: Vec<Rule>,
}

impl Rules {
    pub fn read(path: &Path) -> io::Result<Self> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A dangling link is a broken rules file, not an absent one.
                if fs::symlink_metadata(path).is_ok() {
                    return Err(error);
                }
                return Ok(Self::default());
            }
            Err(error) => return Err(error),
        };
        let rules: Self = serde_json::from_slice(&bytes).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid command rules JSON")
        })?;
        if rules.rules.iter().any(|rule| {
            rule.pattern.is_empty()
                || rule
                    .pattern
                    .iter()
                    .any(|word| word.is_empty() || word.contains('\0'))
        }) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "empty or invalid command prefix",
            ));
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
            || matches!(program, "npm" | "pnpm" | "yarn") && subcommand == "run"
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
                | "find"
                | "sed"
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

// One server process owns the global file. Serialize read/append/replace across sessions.
static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn append(path: &Path, prefixes: &[Vec<String>]) -> io::Result<()> {
    use std::{io::Write, os::unix::fs::PermissionsExt};
    let _lock = WRITE_LOCK.lock().unwrap();
    let mut rules = Rules::read(path)?;
    if prefixes.is_empty()
        || prefixes.iter().any(|prefix| {
            suggested_prefix(prefix).as_ref() != Some(prefix)
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
    let mut file = tempfile::NamedTempFile::new_in(
        path.parent()
            .ok_or_else(|| io::Error::other("missing rules directory"))?,
    )?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer_pretty(&mut file, &rules).map_err(io::Error::other)?;
    file.write_all(b"\n")?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}
