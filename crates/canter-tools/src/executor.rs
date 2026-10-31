use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, RwLock},
};

use async_trait::async_trait;
use tokio::io::AsyncRead;
use tokio_util::sync::CancellationToken;

pub type FileReader = Box<dyn AsyncRead + Unpin + Send>;

pub struct CommandHandle {
    pub sandboxed: bool,
    pub sandbox_approval_allowed: bool,
    pub stdout: FileReader,
    pub stderr: FileReader,
    pub process: Box<dyn CommandProcess>,
}

#[async_trait]
pub trait CommandProcess: Send {
    async fn wait(&mut self) -> io::Result<std::process::ExitStatus>;
    /// Request graceful shutdown of members of the original group with SIGTERM.
    async fn terminate(&mut self) -> io::Result<()>;
    /// Kill the original process group, even after its leader exits. Processes that
    /// moved to another group or session are outside this cleanup boundary.
    async fn kill(&mut self) -> io::Result<()>;
}

#[async_trait]
pub trait Executor: Send + Sync {
    fn cwd(&self) -> &Path;
    /// Session HOME for user-supplied file rules; never the server process HOME.
    fn home(&self) -> Option<&Path> {
        None
    }
    fn sandbox_enabled(&self) -> bool {
        false
    }
    fn normalize_sandbox_request(
        &self,
        _request: &mut canter_protocol::SandboxRequest,
    ) -> io::Result<()> {
        Err(io::Error::other(
            "sandbox requests are not supported by this executor",
        ))
    }
    fn check_sandbox_request(&self, _request: &canter_protocol::SandboxRequest) -> io::Result<()> {
        Err(io::Error::other(
            "sandbox requests are not supported by this executor",
        ))
    }
    /// What model commands may touch now; disabled unless this executor sandboxes them.
    fn sandbox_scope(&self) -> canter_protocol::SessionSandboxResponse {
        canter_protocol::SessionSandboxResponse::default()
    }
    fn configure_shell(&self, _mode: canter_protocol::PermissionMode, _directories: &[String]) {}
    fn close_shell(&self) {}
    /// Run the shared command checks without executing shell or resolving symlinks.
    fn dangerous_shell_reason(&self, command: &str, additional: &[PathBuf]) -> Option<String> {
        crate::dangerous::check(command, self.cwd(), &BTreeMap::new(), additional)
    }
    /// Opt in only when the command and the executor's private paths have been checked.
    fn is_readonly_shell(&self, _command: &str) -> bool {
        false
    }
    /// Check every segment with the whitelist's cwd and protected-path handling.
    fn is_allowed_shell(&self, command: &str, _allowed_prefix: &dyn Fn(&[String]) -> bool) -> bool {
        self.is_readonly_shell(command)
    }
    async fn resolve_path(&self, path: &Path, cancel: &CancellationToken) -> io::Result<PathBuf>;
    /// Validate a file change before reading its original bytes for a diff.
    async fn resolve_write_path(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> io::Result<PathBuf> {
        self.resolve_path(path, cancel).await
    }
    async fn open_file(&self, path: &Path, cancel: &CancellationToken) -> io::Result<FileReader>;
    /// Optional best-effort source size for a successfully opened, truncated read.
    /// Stream-backed executors may not know it; never read extra content to find out.
    async fn file_size(&self, _path: &Path, _cancel: &CancellationToken) -> Option<u64> {
        None
    }
    /// Replace a file only if its bytes still match the approved snapshot (None means absent).
    /// Cancellation before commit leaves the original file intact; await this future to completion.
    async fn write_file(
        &self,
        path: &Path,
        original: Option<&[u8]>,
        content: &[u8],
        cancel: &CancellationToken,
    ) -> io::Result<()>;
    async fn spawn(&self, command: &str, cancel: &CancellationToken) -> io::Result<CommandHandle>;
    async fn spawn_with_sandbox_request(
        &self,
        _command: &str,
        _request: &canter_protocol::SandboxRequest,
        _cancel: &CancellationToken,
    ) -> io::Result<CommandHandle> {
        Err(io::Error::other(
            "sandbox requests are not supported by this executor",
        ))
    }
    /// Only explicit user input uses this path; model tools always call spawn.
    async fn spawn_user(
        &self,
        command: &str,
        cancel: &CancellationToken,
    ) -> io::Result<CommandHandle> {
        self.spawn(command, cancel).await
    }
}

/// One instance per session. The environment is private and never implements Debug.
pub struct LocalExecutor {
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    protected_paths: Vec<PathBuf>,
    shell_protected_paths: Vec<PathBuf>,
    sandbox: Option<Arc<Mutex<Result<crate::sandbox::Sandbox, String>>>>,
    shell_scope: RwLock<crate::sandbox::Scope>,
}

impl LocalExecutor {
    pub fn with_sandbox(mut self, enabled: bool) -> Self {
        self.sandbox = enabled.then(|| {
            Arc::new(Mutex::new(
                crate::sandbox::Sandbox::new(&self.environment, &self.shell_protected_paths)
                    .map_err(|error| error.to_string()),
            ))
        });
        self
    }

    pub fn new(
        cwd: PathBuf,
        environment: BTreeMap<String, String>,
        protected_paths: Vec<PathBuf>,
    ) -> io::Result<Self> {
        if !cwd.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session cwd must be absolute",
            ));
        }
        if protected_paths.iter().any(|path| !path.is_absolute()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "protected paths must be absolute",
            ));
        }
        // Keep the spellings supplied by the server as well as the resolved file-tool paths.
        let mut shell_protected_paths = protected_paths.clone();
        let protected_paths = prepare_protected_paths(protected_paths)?;
        shell_protected_paths.extend(protected_paths.iter().cloned());
        Ok(Self {
            cwd,
            environment,
            protected_paths,
            shell_protected_paths,
            sandbox: None,
            shell_scope: RwLock::default(),
        })
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        self.cwd.join(path)
    }

    fn launch(
        &self,
        command: &str,
        cancel: &CancellationToken,
        sandboxed: bool,
        request: Option<&canter_protocol::SandboxRequest>,
    ) -> io::Result<CommandHandle> {
        use std::process::Stdio;
        check_cancel(cancel)?;
        let mut sandbox_approval_allowed = false;
        let mut launch = if let Some(sandbox) = self.sandbox.as_ref().filter(|_| sandboxed) {
            let sandbox = sandbox.lock().unwrap();
            let sandbox = sandbox
                .as_ref()
                .map_err(|error| io::Error::other(format!("Could not start sandbox: {error}")))?;
            let mut scope = self.shell_scope.read().unwrap().clone();
            sandbox_approval_allowed = scope.approval_allowed;
            if let Some(request) = request {
                sandbox.check_request(&self.cwd, &scope, request)?;
                scope.network |= request.network;
                scope.write_paths = request.write_paths.iter().map(PathBuf::from).collect();
            }
            sandbox
                .command(&self.cwd, &self.environment, &scope)
                .map_err(|error| io::Error::other(format!("Could not start sandbox: {error}")))?
        } else {
            let mut launch = tokio::process::Command::new("/bin/sh");
            launch.env_clear().envs(&self.environment);
            launch
        };
        launch
            .args(["-c", command])
            .current_dir(&self.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        let mut process = crate::process::spawn(launch, Some(cancel)).map_err(|error| {
            if sandboxed {
                io::Error::other(format!("Could not start sandbox: {error}"))
            } else {
                error
            }
        })?;
        let stdout = process
            .child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing stdout pipe"))?;
        let stderr = process
            .child
            .stderr
            .take()
            .ok_or_else(|| io::Error::other("missing stderr pipe"))?;
        Ok(CommandHandle {
            sandboxed,
            sandbox_approval_allowed,
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
            process: Box::new(process),
        })
    }
}

pub(crate) fn check_cancel(cancel: &CancellationToken) -> io::Result<()> {
    if cancel.is_cancelled() {
        Err(io::Error::new(io::ErrorKind::Interrupted, "tool cancelled"))
    } else {
        Ok(())
    }
}

#[async_trait]
impl Executor for LocalExecutor {
    fn cwd(&self) -> &Path {
        &self.cwd
    }

    fn home(&self) -> Option<&Path> {
        self.environment.get("HOME").map(Path::new)
    }

    fn sandbox_enabled(&self) -> bool {
        self.sandbox.is_some()
    }

    fn normalize_sandbox_request(
        &self,
        request: &mut canter_protocol::SandboxRequest,
    ) -> io::Result<()> {
        for path in &mut request.write_paths {
            if path.is_empty() || path.contains('\0') {
                return Err(io::Error::other(
                    "writePaths must contain nonempty paths without NUL bytes",
                ));
            }
            let expanded = if path == "~" || path.starts_with("~/") {
                PathBuf::from(
                    self.environment
                        .get("HOME")
                        .ok_or_else(|| io::Error::other("session HOME is not set"))?,
                )
                .join(path.strip_prefix("~/").unwrap_or(""))
            } else {
                self.cwd.join(&*path)
            };
            *path = resolve_target(&expanded, &[])?
                .to_str()
                .ok_or_else(|| io::Error::other("sandbox path is not UTF-8"))?
                .to_owned();
        }
        request.write_paths.sort();
        request.write_paths.dedup();
        Ok(())
    }

    fn check_sandbox_request(&self, request: &canter_protocol::SandboxRequest) -> io::Result<()> {
        let sandbox = self
            .sandbox
            .as_ref()
            .ok_or_else(|| io::Error::other("sandbox is disabled"))?
            .lock()
            .unwrap();
        sandbox
            .as_ref()
            .map_err(|error| io::Error::other(error.clone()))?
            .check_request(&self.cwd, &self.shell_scope.read().unwrap(), request)
    }

    fn configure_shell(&self, mode: canter_protocol::PermissionMode, directories: &[String]) {
        *self.shell_scope.write().unwrap() = crate::sandbox::Scope {
            network: mode == canter_protocol::PermissionMode::AllowAll,
            approval_allowed: mode != canter_protocol::PermissionMode::AllowAll,
            additional_directories: directories.iter().map(PathBuf::from).collect(),
            write_paths: Vec::new(),
        };
    }

    fn sandbox_scope(&self) -> canter_protocol::SessionSandboxResponse {
        let Some(sandbox) = &self.sandbox else {
            return canter_protocol::SessionSandboxResponse::default();
        };
        let text = |path: &PathBuf| path.to_string_lossy().into_owned();
        let scope = self.shell_scope.read().unwrap();
        let mut response = canter_protocol::SessionSandboxResponse {
            enabled: true,
            writable: std::iter::once(&self.cwd)
                .chain(&scope.additional_directories)
                .map(text)
                .collect(),
            network: scope.network,
            ..Default::default()
        };
        // A sandbox that could not start still blocks every command; only its paths are unknown.
        if let Ok(sandbox) = sandbox.lock().unwrap().as_ref() {
            let (writable, readonly, unreadable) = sandbox.listing();
            response.writable.extend(writable.iter().map(text));
            response.read_only = readonly.iter().map(text).collect();
            response.no_access = unreadable.iter().map(text).collect();
        }
        response
    }

    fn close_shell(&self) {
        if let Some(sandbox) = &self.sandbox
            && let Ok(sandbox) = sandbox.lock().unwrap().as_mut()
        {
            sandbox.close();
        }
    }

    fn dangerous_shell_reason(&self, command: &str, additional: &[PathBuf]) -> Option<String> {
        crate::dangerous::check(command, &self.cwd, &self.environment, additional)
    }

    fn is_allowed_shell(&self, command: &str, allowed_prefix: &dyn Fn(&[String]) -> bool) -> bool {
        crate::readonly::is_allowed(
            command,
            &self.cwd,
            &self.environment,
            &self.shell_protected_paths,
            allowed_prefix,
        )
    }

    fn is_readonly_shell(&self, command: &str) -> bool {
        crate::readonly::is_readonly(
            command,
            &self.cwd,
            &self.environment,
            &self.shell_protected_paths,
        )
    }

    async fn resolve_path(&self, path: &Path, cancel: &CancellationToken) -> io::Result<PathBuf> {
        check_cancel(cancel)?;
        let path = self.resolve(path);
        let protected_paths = self.protected_paths.clone();
        let sandbox = self.sandbox.clone();
        let resolved = tokio::task::spawn_blocking(move || {
            let resolved = resolve_target(&path, &protected_paths)?;
            check_protected(&resolved, &protected_paths)?;
            if let Some(sandbox) = sandbox {
                return resolve_sandbox_read(
                    sandbox
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map_err(|error| io::Error::other(error.clone()))?,
                    &path,
                );
            }
            Ok::<_, io::Error>(resolved)
        })
        .await
        .map_err(io::Error::other)??;
        check_cancel(cancel)?;
        Ok(resolved)
    }

    async fn open_file(&self, path: &Path, cancel: &CancellationToken) -> io::Result<FileReader> {
        check_cancel(cancel)?;
        let file = tokio::fs::File::open(self.resolve_path(path, cancel).await?).await?;
        check_cancel(cancel)?;
        Ok(Box::new(file))
    }

    async fn file_size(&self, path: &Path, cancel: &CancellationToken) -> Option<u64> {
        if cancel.is_cancelled() {
            return None;
        }
        let metadata = tokio::fs::metadata(self.resolve(path)).await.ok()?;
        metadata.is_file().then_some(metadata.len())
    }

    async fn resolve_write_path(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> io::Result<PathBuf> {
        let resolved = self.resolve_path(path, cancel).await?;
        let Some(sandbox) = self.sandbox.clone() else {
            return Ok(resolved);
        };
        let path = self.resolve(path);
        let cwd = self.cwd.clone();
        let scope = self.shell_scope.read().unwrap().clone();
        let resolved = tokio::task::spawn_blocking(move || {
            resolve_sandbox_write(
                sandbox
                    .lock()
                    .unwrap()
                    .as_ref()
                    .map_err(|error| io::Error::other(error.clone()))?,
                &cwd,
                &scope,
                &path,
            )
        })
        .await
        .map_err(io::Error::other)??;
        check_cancel(cancel)?;
        Ok(resolved)
    }

    async fn write_file(
        &self,
        path: &Path,
        original: Option<&[u8]>,
        content: &[u8],
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        let path = self.resolve_write_path(path, cancel).await?;
        let original = original.map(ToOwned::to_owned);
        let content = content.to_vec();
        let cancel = cancel.clone();
        let protected_paths = self.protected_paths.clone();
        let sandbox = self.sandbox.clone();
        let cwd = self.cwd.clone();
        let scope = self.shell_scope.read().unwrap().clone();
        // Do not select away from this worker: it must finish before the caller observes completion.
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            check_cancel(&cancel)?;
            let path = resolve_target(&path, &protected_paths)?;
            check_protected(&path, &protected_paths)?;
            let path = if let Some(sandbox) = sandbox {
                resolve_sandbox_write(
                    sandbox
                        .lock()
                        .unwrap()
                        .as_ref()
                        .map_err(|error| io::Error::other(error.clone()))?,
                    &cwd,
                    &scope,
                    &path,
                )?
            } else {
                path
            };
            let current = match std::fs::read(&path) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == io::ErrorKind::NotFound => None,
                Err(e) => return Err(e),
            };
            if current != original {
                return Err(io::Error::other(
                    "file changed since preparation; read it again and prepare a new call",
                ));
            }
            if current.is_some() {
                if std::fs::metadata(&path)?.permissions().readonly() {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "file is not writable",
                    ));
                }
                // Opening without truncate checks effective permissions and ACLs.
                std::fs::OpenOptions::new().write(true).open(&path)?;
            }
            check_cancel(&cancel)?;
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("file has no parent"))?;
            std::fs::create_dir_all(parent)?;
            let mut temp = tempfile::Builder::new()
                .permissions(std::fs::Permissions::from_mode(0o666))
                .tempfile_in(parent)?;
            if let Ok(metadata) = std::fs::metadata(&path) {
                temp.as_file().set_permissions(metadata.permissions())?;
            }
            for chunk in content.chunks(64 * 1024) {
                check_cancel(&cancel)?;
                temp.write_all(chunk)?;
            }
            temp.flush()?;
            check_cancel(&cancel)?;
            temp.persist(path).map_err(|e| e.error)?;
            Ok(())
        })
        .await
        .map_err(io::Error::other)?
    }

    async fn spawn(&self, command: &str, cancel: &CancellationToken) -> io::Result<CommandHandle> {
        self.launch(command, cancel, self.sandbox.is_some(), None)
    }

    async fn spawn_with_sandbox_request(
        &self,
        command: &str,
        request: &canter_protocol::SandboxRequest,
        cancel: &CancellationToken,
    ) -> io::Result<CommandHandle> {
        if !self.sandbox_enabled() {
            return Err(io::Error::other(
                "sandbox requests require an enabled sandbox",
            ));
        }
        self.launch(command, cancel, true, Some(request))
    }

    async fn spawn_user(
        &self,
        command: &str,
        cancel: &CancellationToken,
    ) -> io::Result<CommandHandle> {
        self.launch(command, cancel, false, None)
    }
}

fn resolve_sandbox_read(sandbox: &crate::sandbox::Sandbox, path: &Path) -> io::Result<PathBuf> {
    let unreadable = prepare_protected_paths(sandbox.unreadable_paths().to_vec())?;
    let check = |path: &Path| {
        if unreadable
            .iter()
            .any(|protected| crate::sandbox::within_private(path, protected))
        {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "This path is protected by the sandbox: credential directories such as ~/.ssh can't be read or changed. If the user really needs it, they can do it themselves.",
            ))
        } else {
            Ok(())
        }
    };
    let resolved = resolve_target_checked(path, check)?;
    check(&resolved)?;
    Ok(resolved)
}

fn resolve_sandbox_write(
    sandbox: &crate::sandbox::Sandbox,
    cwd: &Path,
    scope: &crate::sandbox::Scope,
    path: &Path,
) -> io::Result<PathBuf> {
    let resolved = resolve_sandbox_read(sandbox, path)?;
    let mut readonly = prepare_protected_paths(sandbox.readonly_paths().to_vec())?;
    readonly.extend(crate::sandbox::git_protected_roots(
        std::iter::once(cwd)
            .chain(scope.additional_directories.iter().map(PathBuf::as_path))
            .chain(resolved.parent()),
    )?);
    let check = |path: &Path| {
        if readonly
            .iter()
            .any(|protected| crate::sandbox::within_private(path, protected))
            || crate::sandbox::is_git_protected(path)
            || path
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case(".git"))
        {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Blocked: the sandbox protects this path (Git config and hooks, or the real Cargo and npm directories). If the user really wants this change, they can make it themselves.",
            ))
        } else {
            Ok(())
        }
    };
    // Check the requested spelling too, including Git metadata pointers.
    // Only the final .git component is a file target; parent .git directories are writable.
    check(path)?;
    check(&resolved)?;
    Ok(resolved)
}

// Resolve each existing prefix before processing `..`; unlike lexical normalization,
// this preserves the meaning of a parent segment following a symlink. Missing tails
// are allowed for write, but dangling symlinks are never replaced by ordinary files.
pub(crate) fn resolve_target(path: &Path, protected_paths: &[PathBuf]) -> io::Result<PathBuf> {
    resolve_target_checked(path, |path| check_protected(path, protected_paths))
}

fn resolve_target_checked(
    path: &Path,
    check: impl Fn(&Path) -> io::Result<()>,
) -> io::Result<PathBuf> {
    use std::path::Component;
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            component => {
                resolved.push(component.as_os_str());
                // Check link names before following them, including protected dangling links.
                check(&resolved)?;
                match std::fs::canonicalize(&resolved) {
                    Ok(path) => resolved = path,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        if std::fs::symlink_metadata(&resolved).is_ok_and(|m| m.is_symlink()) {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "dangling symbolic link: target does not exist",
                            ));
                        }
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    Ok(resolved)
}

pub(crate) fn prepare_protected_paths(mut pending: Vec<PathBuf>) -> io::Result<Vec<PathBuf>> {
    let mut protected = Vec::new();
    while let Some(path) = pending.pop() {
        let path = match (path.parent(), path.file_name()) {
            (Some(parent), Some(name)) => resolve_target(parent, &[])?.join(name),
            _ => resolve_target(&path, &[])?,
        };
        if protected.contains(&path) {
            continue;
        }
        // Keep the link name even if it is subsequently replaced with an ordinary file.
        protected.push(path.clone());
        match std::fs::read_link(&path) {
            Ok(target) => pending.push(path.parent().unwrap_or(Path::new("/")).join(target)),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::InvalidInput | io::ErrorKind::NotFound
                ) =>
            {
                protected.push(resolve_target(&path, &[])?);
            }
            Err(error) => return Err(error),
        }
    }
    protected.sort();
    protected.dedup();
    Ok(protected)
}

fn check_protected(path: &Path, protected_paths: &[PathBuf]) -> io::Result<()> {
    for protected in protected_paths {
        if path.starts_with(protected) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "this file is private to canter",
            ));
        }
    }
    Ok(())
}
