use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use tokio::io::AsyncRead;
use tokio_util::sync::CancellationToken;

pub type FileReader = Box<dyn AsyncRead + Unpin + Send>;

pub struct CommandHandle {
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
    /// Opt in only when the command and the executor's private paths have been checked.
    fn is_readonly_shell(&self, _command: &str) -> bool {
        false
    }
    async fn resolve_path(&self, path: &Path, cancel: &CancellationToken) -> io::Result<PathBuf>;
    async fn open_file(&self, path: &Path, cancel: &CancellationToken) -> io::Result<FileReader>;
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
}

/// One instance per session. The environment is private and never implements Debug.
pub struct LocalExecutor {
    cwd: PathBuf,
    environment: BTreeMap<String, String>,
    protected_paths: Vec<PathBuf>,
    shell_protected_paths: Vec<PathBuf>,
}

impl LocalExecutor {
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
        })
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        self.cwd.join(path)
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
        let resolved = tokio::task::spawn_blocking(move || {
            let resolved = resolve_target(&path, &protected_paths)?;
            check_protected(&resolved, &protected_paths)?;
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

    async fn write_file(
        &self,
        path: &Path,
        original: Option<&[u8]>,
        content: &[u8],
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        let path = self.resolve_path(path, cancel).await?;
        let original = original.map(ToOwned::to_owned);
        let content = content.to_vec();
        let cancel = cancel.clone();
        let protected_paths = self.protected_paths.clone();
        // Do not select away from this worker: it must finish before the caller observes completion.
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            use std::os::unix::fs::PermissionsExt;
            check_cancel(&cancel)?;
            let path = resolve_target(&path, &protected_paths)?;
            check_protected(&path, &protected_paths)?;
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
        use std::process::Stdio;
        check_cancel(cancel)?;
        let child = tokio::process::Command::new("/bin/sh")
            .args(["-c", command])
            .current_dir(&self.cwd)
            .env_clear()
            .envs(&self.environment)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let group = child.id().map(|id| nix::unistd::Pid::from_raw(id as i32));
        let mut process = LocalProcess { child, group };
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
            stdout: Box::new(stdout),
            stderr: Box::new(stderr),
            process: Box::new(process),
        })
    }
}

// Resolve each existing prefix before processing `..`; unlike lexical normalization,
// this preserves the meaning of a parent segment following a symlink. Missing tails
// are allowed for write, but dangling symlinks are never replaced by ordinary files.
fn resolve_target(path: &Path, protected_paths: &[PathBuf]) -> io::Result<PathBuf> {
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
                check_protected(&resolved, protected_paths)?;
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

fn prepare_protected_paths(mut pending: Vec<PathBuf>) -> io::Result<Vec<PathBuf>> {
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
                "这是 canter 的私有文件",
            ));
        }
    }
    Ok(())
}

struct LocalProcess {
    child: tokio::process::Child,
    group: Option<nix::unistd::Pid>,
}

impl LocalProcess {
    fn kill_group(&mut self) -> io::Result<()> {
        if let Some(group) = self.group {
            match nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => self.group = None,
                Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
            }
        }
        Ok(())
    }
}

#[async_trait]
impl CommandProcess for LocalProcess {
    async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
    async fn kill(&mut self) -> io::Result<()> {
        self.kill_group()
    }
    async fn terminate(&mut self) -> io::Result<()> {
        if let Some(group) = self.group {
            match nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGTERM) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(error) => return Err(io::Error::from_raw_os_error(error as i32)),
            }
        }
        Ok(())
    }
}

impl Drop for LocalProcess {
    fn drop(&mut self) {
        let _ = self.kill_group();
    }
}
