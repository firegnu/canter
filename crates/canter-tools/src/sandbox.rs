//! macOS command isolation. Policy shape informed by Codex rust-v0.166.0,
//! codex-rs/sandboxing/src/seatbelt.rs and seatbelt_base_policy.sbpl; no code copied.
//! Also reviewed sandbox-runtime (srt) 0.0.80, dist/sandbox/macos-sandbox-utils.js
//! (generateSandboxProfile, macGetMandatoryDenyEntries, wrapCommandWithSandboxMacOS).
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(crate) struct Scope {
    pub network: bool,
    pub additional_directories: Vec<PathBuf>,
}

pub(crate) struct Sandbox {
    temporary: Option<tempfile::TempDir>,
    cargo: PathBuf,
    npm: PathBuf,
    unreadable: Vec<PathBuf>,
    readonly: Vec<PathBuf>,
}

impl Sandbox {
    pub fn new(environment: &BTreeMap<String, String>, protected: &[PathBuf]) -> io::Result<Self> {
        if !cfg!(target_os = "macos") {
            return Err(io::Error::other("Seatbelt requires macOS"));
        }
        let home = environment
            .get("HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| io::Error::other("sandbox requires an absolute session HOME"))?;
        let mut unreadable = protected.to_vec();
        for name in [".ssh", ".aws", ".gnupg", ".canter"] {
            unreadable.push(home.join(name));
        }
        if let Some(state) = environment.get("CANTER_HOME") {
            let state = PathBuf::from(state);
            if !state.is_absolute() {
                return Err(io::Error::other("sandbox requires an absolute CANTER_HOME"));
            }
            unreadable.push(state);
        }
        let unreadable = crate::executor::prepare_protected_paths(unreadable)?;
        let readonly =
            crate::executor::prepare_protected_paths(vec![home.join(".cargo"), home.join(".npm")])?;
        let mut protected = unreadable.clone();
        protected.extend(readonly.iter().cloned());
        let cache =
            crate::executor::resolve_target(&home.join("Library/Caches/canter"), &protected)?;
        let cargo = cache.join("cargo");
        let npm = cache.join("npm");
        for path in [&cargo, &npm] {
            crate::executor::resolve_target(path, &protected)?;
            std::fs::create_dir_all(path)?;
        }
        // Only this private child is writable, never the system temporary root.
        let temporary_root = crate::executor::resolve_target(Path::new("/tmp"), &protected)?;
        let temporary = tempfile::Builder::new()
            .prefix("canter-shell-")
            .tempdir_in(temporary_root)?;
        crate::executor::resolve_target(temporary.path(), &protected)?;
        Ok(Self {
            temporary: Some(temporary),
            cargo: cargo.canonicalize()?,
            npm: npm.canonicalize()?,
            unreadable,
            readonly,
        })
    }

    pub fn close(&mut self) {
        self.temporary.take();
    }

    pub fn command(
        &self,
        cwd: &Path,
        environment: &BTreeMap<String, String>,
        scope: &Scope,
    ) -> io::Result<tokio::process::Command> {
        let temporary = self
            .temporary
            .as_ref()
            .ok_or_else(|| io::Error::other("sandbox session has closed"))?
            .path()
            .canonicalize()?;
        let mut project_roots = vec![cwd.canonicalize()?];
        for directory in &scope.additional_directories {
            project_roots.push(directory.canonicalize()?);
        }
        let mut git_directories = Vec::new();
        for root in
            std::iter::once(cwd).chain(scope.additional_directories.iter().map(PathBuf::as_path))
        {
            git_directories.extend(git_metadata(root)?);
        }
        git_directories.sort();
        git_directories.dedup();
        let mut parameters = Parameters::default();
        let mut profile = String::from(
            "(version 1)\n(deny default)\n(deny file-link)\n\
         (allow process-exec process-fork)\n\
         (allow signal (target same-sandbox))\n\
         (allow process-info* (target same-sandbox))\n\
         (allow sysctl-read)\n\
         (allow mach-lookup (global-name \"com.apple.system.opendirectoryd.libinfo\"))\n\
         (allow file-read*)\n\
         (allow system-socket (socket-domain AF_UNIX))\n\
         (allow file-write-data (literal \"/dev/null\") (literal \"/dev/stdout\")\n\
           (literal \"/dev/stderr\") (regex #\"^/dev/fd/[0-9]+$\"))\n",
        );
        for path in project_roots
            .iter()
            .chain([&temporary, &self.cargo, &self.npm])
        {
            let parameter = parameters.path(path)?;
            profile.push_str(&format!("(allow file-write* (subpath {parameter}))\n"));
            profile.push_str(&format!(
                "(allow network-bind (local unix-socket (subpath {parameter})))\n"
            ));
            profile.push_str(&format!(
                "(allow network-outbound (remote unix-socket (subpath {parameter})))\n"
            ));
        }
        let protected_names = ["config", "config.worktree", "hooks", "commondir", "gitdir"]
            .map(case_insensitive_name)
            .join("|");
        for directory in &git_directories {
            profile.push_str(&format!(
                "(allow file-write* (subpath {}))\n",
                parameters.path(directory)?
            ));
        }
        for directory in &git_directories {
            for path in git_protected_paths(directory)? {
                // Include real targets when metadata uses links.
                for path in crate::executor::prepare_protected_paths(vec![path])? {
                    profile.push_str(&format!(
                        "(deny file-write* (subpath {}) {})\n",
                        parameters.path(&path)?,
                        parameters.case_insensitive_subpath(&path)?
                    ));
                    protect_ancestors(&mut profile, &mut parameters, &path)?;
                }
            }
            let parameter = parameters.path(directory)?;
            for (container, depth) in [("worktrees", "[^/]+"), ("modules", ".+")] {
                profile.push_str(&format!(
                    "(deny file-write* (require-all (subpath {parameter}) (regex #\"/{}/{depth}/({protected_names})(/|$)\")))\n",
                    case_insensitive_name(container)
                ));
                protect_ancestors(&mut profile, &mut parameters, &directory.join(container))?;
            }
        }
        // Protect new project repositories, without blocking dependency checkouts
        // in the private temporary directory or disposable download caches.
        let dot_git = case_insensitive_name(".git");
        let mut exclusions = String::new();
        for directory in [&temporary, &self.cargo, &self.npm] {
            exclusions.push_str(&format!(
                " (require-not (subpath {}))",
                parameters.path(directory)?
            ));
        }
        for root in &project_roots {
            let parameter = parameters.path(root)?;
            profile.push_str(&format!(
                "(deny file-write* (require-all (subpath {parameter}) (regex #\"(^|/){dot_git}/({protected_names})(/|$)\"){exclusions}))\n\
                 (deny file-write-data file-write-unlink (require-all (subpath {parameter}) (regex #\"(^|/){dot_git}$\"){exclusions}))\n"
            ));
        }
        if scope.network {
            profile.push_str(
                r#"
(allow network-outbound (remote ip "*:*"))
(allow network-outbound (remote unix-socket (path-literal "/private/var/run/mDNSResponder")))
(allow network-inbound network-bind (local ip "*:*"))
(allow system-socket (require-all (socket-domain AF_SYSTEM) (socket-protocol 2)))
(allow mach-lookup
  (global-name "com.apple.bsd.dirhelper")
  (global-name "com.apple.dnssd.service")
  (global-name "com.apple.system.opendirectoryd.membership")
  (global-name "com.apple.SecurityServer")
  (global-name "com.apple.securityd.xpc")
  (global-name "com.apple.networkd")
  (global-name "com.apple.ocspd")
  (global-name "com.apple.trustd.agent")
  (global-name "com.apple.SystemConfiguration.DNSConfiguration")
  (global-name "com.apple.SystemConfiguration.configd"))
"#,
            );
        }
        for path in &self.unreadable {
            let parameter = parameters.path(path)?;
            let insensitive = parameters.case_insensitive_subpath(path)?;
            profile.push_str(&format!(
                "(deny file-read* file-write* (subpath {parameter}) {insensitive})\n"
            ));
            profile.push_str(&format!(
                "(deny network-outbound (remote unix-socket (subpath {parameter})) (remote unix-socket {insensitive}))\n"
            ));
            profile.push_str(&format!(
                "(deny network-bind (local unix-socket (subpath {parameter})) (local unix-socket {insensitive}))\n"
            ));
            protect_ancestors(&mut profile, &mut parameters, path)?;
        }
        for path in &self.readonly {
            profile.push_str(&format!(
                "(deny file-write* (subpath {}))\n",
                parameters.path(path)?
            ));
            protect_ancestors(&mut profile, &mut parameters, path)?;
        }
        // These macOS operations mutate files even through read-only descriptors.
        // See Codex seatbelt.rs, F_MAKECOMPRESSED and F_TRANSFEREXTENTS.
        profile.push_str("(deny system-fcntl (fcntl-command 80 110))\n");
        let mut command = tokio::process::Command::new("/usr/bin/sandbox-exec");
        command.args(["-p", &profile]);
        for (value, key) in &parameters.0 {
            let mut parameter = std::ffi::OsString::from(format!("{key}="));
            parameter.push(value);
            command.arg("-D").arg(parameter);
        }
        command
            .args(["--", "/bin/sh"])
            .env_clear()
            .envs(environment)
            .env("TMPDIR", &temporary)
            .env("TMPPREFIX", temporary.join("zsh"))
            .env("CARGO_HOME", &self.cargo)
            .env("npm_config_cache", &self.npm)
            .env_remove("NPM_CONFIG_CACHE");
        Ok(command)
    }
}

// Match both ASCII cases, escaping punctuation in configured directory names.
fn case_insensitive_name(name: &str) -> String {
    name.chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' => format!(
                "[{}{}]",
                character.to_ascii_lowercase(),
                character.to_ascii_uppercase()
            ),
            '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\' => {
                format!("\\{character}")
            }
            _ => character.to_string(),
        })
        .collect()
}

fn git_protected_paths(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let mut directories = vec![directory.to_path_buf()];
    match std::fs::read_dir(directory.join("worktrees")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    directories.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut pending: Vec<_> = directories
        .iter()
        .map(|path| path.join("modules"))
        .collect();
    let mut protected: Vec<_> = directories
        .into_iter()
        .flat_map(|directory| {
            ["config", "config.worktree", "hooks", "commondir", "gitdir"]
                .map(|name| directory.join(name))
        })
        .collect();
    while let Some(container) = pending.pop() {
        let entries = match std::fs::read_dir(&container) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let path = entry.path();
            if path.join("HEAD").try_exists()? {
                // A submodule's object database is not another name container.
                // Recurse only through its own worktrees/modules metadata.
                protected.extend(git_protected_paths(&path)?);
            } else {
                pending.push(path);
            }
        }
    }
    Ok(protected)
}

fn git_metadata(root: &Path) -> io::Result<Vec<PathBuf>> {
    let root = root.canonicalize()?;
    for ancestor in root.ancestors() {
        let dot_git = ancestor.join(".git");
        let metadata = match std::fs::metadata(&dot_git) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let directory = if metadata.is_dir() {
            dot_git.canonicalize()?
        } else {
            let pointer = std::fs::read_to_string(&dot_git)?;
            let target = pointer
                .trim()
                .strip_prefix("gitdir: ")
                .ok_or_else(|| io::Error::other("invalid .git pointer"))?;
            ancestor.join(target).canonicalize()?
        };
        let mut directories = vec![directory.clone()];
        match std::fs::read_to_string(directory.join("commondir")) {
            Ok(common) => directories.push(directory.join(common.trim()).canonicalize()?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        return Ok(directories);
    }
    Ok(Vec::new())
}

fn protect_ancestors(
    profile: &mut String,
    parameters: &mut Parameters,
    path: &Path,
) -> io::Result<()> {
    for ancestor in path.ancestors() {
        profile.push_str(&format!(
            "(deny file-write-unlink (literal {}))\n",
            parameters.path(ancestor)?
        ));
    }
    Ok(())
}

#[derive(Default)]
struct Parameters(BTreeMap<String, String>);

impl Parameters {
    fn path(&mut self, path: &Path) -> io::Result<String> {
        let text = path
            .to_str()
            .ok_or_else(|| io::Error::other("sandbox path is not UTF-8"))?;
        if !path.is_absolute() || text.len() >= 1024 {
            return Err(io::Error::other(
                "sandbox path must be absolute and shorter than 1024 bytes",
            ));
        }
        // Paths are separate argv values, never interpolated into SBPL literals
        // that can exceed Seatbelt's 1025-byte limit.
        Ok(self.value(text))
    }

    fn value(&mut self, value: &str) -> String {
        let next = format!("VALUE_{}", self.0.len());
        let key = self.0.entry(value.into()).or_insert(next);
        format!("(param \"{key}\")")
    }

    fn case_insensitive_subpath(&mut self, path: &Path) -> io::Result<String> {
        let Some(parent) = path.parent() else {
            return Ok(format!("(subpath {})", self.path(path)?));
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| io::Error::other("sandbox path has no UTF-8 filename"))?;
        let separator = if parent == Path::new("/") { "" } else { "/" };
        let suffix = self.value(&format!("{separator}{}(/|$)", case_insensitive_name(name)));
        // Anchor at the actual parent so only its direct child name is folded.
        // Both pieces use argv parameters, including names with punctuation.
        let parent = self.path(parent)?;
        Ok(format!(
            "(regex (string-append \"^\" (regex-quote {parent}) {suffix}))"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_protected_paths_include_nested_submodule_metadata() {
        let temporary = tempfile::tempdir_in("/tmp").unwrap();
        let git = temporary.path().join(".git");
        let child = git.join("modules/nested/child");
        let grandchild = child.join("modules/deeper/grandchild");
        let worktree = child.join("worktrees/linked");
        let worktree_child = worktree.join("modules/linked-child");
        let directories = [&git, &child, &grandchild, &worktree, &worktree_child];
        for directory in directories {
            std::fs::create_dir_all(directory).unwrap();
            std::fs::write(directory.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        }
        let protected = git_protected_paths(&git).unwrap();
        for directory in directories {
            for name in ["config", "config.worktree", "hooks", "commondir", "gitdir"] {
                let path = directory.join(name);
                assert!(
                    protected.contains(&path),
                    "{} was not protected",
                    path.display()
                );
            }
        }
    }
}
