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
            .map(git_name_pattern)
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
                        "(deny file-write* (subpath {}))\n",
                        parameters.path(&path)?
                    ));
                    protect_ancestors(&mut profile, &mut parameters, &path)?;
                }
            }
            let parameter = parameters.path(directory)?;
            for (container, depth) in [("worktrees", "[^/]+"), ("modules", ".+")] {
                profile.push_str(&format!(
                    "(deny file-write* (require-all (subpath {parameter}) (regex #\"/{}/{depth}/({protected_names})(/|$)\")))\n",
                    git_name_pattern(container)
                ));
                protect_ancestors(&mut profile, &mut parameters, &directory.join(container))?;
            }
        }
        // Protect new project repositories, without blocking dependency checkouts
        // in the private temporary directory or disposable download caches.
        let dot_git = git_name_pattern(".git");
        for root in &project_roots {
            let parameter = parameters.path(root)?;
            profile.push_str(&format!(
                "(deny file-write* (require-all (subpath {parameter}) (regex #\"(^|/){dot_git}/({protected_names})(/|$)\")))\n\
                 (deny file-write-data file-write-unlink (require-all (subpath {parameter}) (regex #\"(^|/){dot_git}$\")))\n"
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
            profile.push_str(&format!(
                "(deny file-read* file-write* (subpath {parameter}))\n"
            ));
            profile.push_str(&format!(
                "(deny network-outbound (remote unix-socket (subpath {parameter})))\n"
            ));
            profile.push_str(&format!(
                "(deny network-bind (local unix-socket (subpath {parameter})))\n"
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
        for (path, key) in &parameters.0 {
            let mut parameter = std::ffi::OsString::from(format!("{key}="));
            parameter.push(path);
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

// Match both ASCII cases of the fixed Git metadata names used above.
fn git_name_pattern(name: &str) -> String {
    name.chars()
        .map(|character| match character {
            '.' => "\\.".to_owned(),
            _ => format!("[{character}{}]", character.to_ascii_uppercase()),
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
    Ok(directories
        .into_iter()
        .flat_map(|directory| {
            ["config", "config.worktree", "hooks", "commondir", "gitdir"]
                .map(|name| directory.join(name))
        })
        .collect())
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
struct Parameters(BTreeMap<PathBuf, String>);

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
        let next = format!("PATH_{}", self.0.len());
        let key = self.0.entry(path.into()).or_insert(next);
        // Paths are separate argv values, never interpolated into SBPL strings or
        // combined into a regex that can exceed Seatbelt's 1025-byte literal limit.
        Ok(format!("(param \"{key}\")"))
    }
}
