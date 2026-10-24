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
    /// Kill the original process group, including descendants, even after its leader exits.
    async fn kill(&mut self) -> io::Result<()>;
}

#[async_trait]
pub trait Executor: Send + Sync {
    fn cwd(&self) -> &Path;
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
}

impl LocalExecutor {
    pub fn new(cwd: PathBuf, environment: BTreeMap<String, String>) -> io::Result<Self> {
        if !cwd.is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session cwd must be absolute",
            ));
        }
        Ok(Self { cwd, environment })
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

    async fn open_file(&self, path: &Path, cancel: &CancellationToken) -> io::Result<FileReader> {
        check_cancel(cancel)?;
        let file = tokio::fs::File::open(self.resolve(path)).await?;
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
        let path = self.resolve(path);
        let original = original.map(ToOwned::to_owned);
        let content = content.to_vec();
        let cancel = cancel.clone();
        // Do not select away from this worker: it must finish before the caller observes completion.
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            check_cancel(&cancel)?;
            let path = match std::fs::canonicalize(&path) {
                Ok(path) => path,
                Err(e) if e.kind() == io::ErrorKind::NotFound => path,
                Err(e) => return Err(e),
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
            check_cancel(&cancel)?;
            let parent = path
                .parent()
                .ok_or_else(|| io::Error::other("file has no parent"))?;
            std::fs::create_dir_all(parent)?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
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
}

impl Drop for LocalProcess {
    fn drop(&mut self) {
        let _ = self.kill_group();
    }
}
