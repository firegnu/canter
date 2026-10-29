//! Shared local process ownership for shell and LSP.
use crate::CommandProcess;
use async_trait::async_trait;
use std::io;

// Covers pipe creation, guardian attachment, spawn and temporary FD release.
static SPAWN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub(crate) fn spawn_probe(
    mut command: tokio::process::Command,
) -> io::Result<tokio::process::Child> {
    let _spawn = SPAWN_LOCK.lock().unwrap();
    command.kill_on_drop(true).spawn()
}

pub(crate) fn spawn(
    mut command: tokio::process::Command,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> io::Result<LocalProcess> {
    let _spawn = SPAWN_LOCK.lock().unwrap();
    if let Some(cancel) = cancel {
        crate::executor::check_cancel(cancel)?;
    }
    command.process_group(0).kill_on_drop(true);
    let guardian = crate::guardian::attach(&mut command)?;
    let child = command.spawn()?;
    drop(command);
    let group = child.id().map(|id| nix::unistd::Pid::from_raw(id as i32));
    Ok(LocalProcess {
        child,
        group,
        guardian,
    })
}

pub(crate) struct LocalProcess {
    pub(crate) child: tokio::process::Child,
    group: Option<nix::unistd::Pid>,
    guardian: std::io::PipeWriter,
}

impl LocalProcess {
    fn kill_group(&mut self) -> io::Result<()> {
        if let Some(group) = self.group {
            match nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {
                    self.group = None;
                    // Disarm only after the original group has been cleaned up.
                    let _ = std::io::Write::write_all(&mut self.guardian, b"\n");
                }
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
