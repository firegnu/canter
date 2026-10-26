use std::{
    fs::{self, File, OpenOptions, TryLockError},
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::ServerError;
use serde::{Deserialize, Serialize};

pub const UPGRADE_GRACE: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub pid: u32,
    pub version: String,
    pub protocol_version: u32,
    pub socket_path: PathBuf,
    /// Distinguishes a new daemon even if the OS reuses a PID.
    pub instance: String,
}

pub(crate) fn private_dir(path: &Path) -> Result<(), ServerError> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .and_then(|()| fs::set_permissions(path, fs::Permissions::from_mode(0o700)))
        .map_err(|_| ServerError("cannot create the private state directory".into()))
}

pub(crate) fn private_file(path: &Path) -> Result<File, ServerError> {
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| ServerError("cannot open the server state file".into()))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| ServerError("cannot set the state file permissions".into()))?;
    Ok(file)
}

pub(crate) fn read_registration(home: &Path) -> Result<Option<Registration>, ServerError> {
    match fs::read(home.join("server.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| ServerError("the server registration file is invalid".into())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ServerError(
            "cannot read the server registration file".into(),
        )),
    }
}

pub(crate) fn server_running(home: &Path) -> Result<bool, ServerError> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(home.join("server.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(ServerError("cannot check the server file lock".into())),
    };
    match file.try_lock_shared() {
        Ok(()) => Ok(false), // Dropping the file immediately releases this probe's lock.
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(_) => Err(ServerError("cannot check the server file lock".into())),
    }
}

fn write_json(home: &Path, name: &str, value: &impl Serialize) -> Result<(), ServerError> {
    let mut file = tempfile::NamedTempFile::new_in(home)
        .map_err(|_| ServerError("cannot write the registration file".into()))?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| ServerError("cannot set the registration file permissions".into()))?;
    serde_json::to_writer(&mut file, value)
        .map_err(|_| ServerError("cannot write the registration file".into()))?;
    file.write_all(b"\n")
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| ServerError("cannot sync the registration file".into()))?;
    file.persist(home.join(name))
        .map_err(|_| ServerError("cannot publish the registration file".into()))?;
    Ok(())
}

pub(crate) struct Lease {
    pub home: PathBuf,
    pub registration: Registration,
    _lock: File,
}

impl Lease {
    // Runs on a blocking thread, including bounded waiting for an explicit replacement.
    pub fn acquire(home: PathBuf, replace: bool) -> Result<Self, ServerError> {
        private_dir(&home)?;
        let home =
            fs::canonicalize(home).map_err(|_| ServerError("invalid state directory".into()))?;
        let lock = private_file(&home.join("server.lock"))?;
        let acquired = loop {
            match lock.try_lock() {
                Err(TryLockError::WouldBlock) => match lock.try_lock_shared() {
                    // Only discovery probes hold shared locks. Let them finish,
                    // then retry exclusive ownership without mistaking them for a daemon.
                    Ok(()) => {
                        lock.unlock().map_err(|_| {
                            ServerError("cannot release the server probe lock".into())
                        })?;
                        std::thread::yield_now();
                    }
                    Err(error) => break Err(error),
                },
                result => break result,
            }
        };
        match acquired {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) if replace => {
                let replacement = private_file(&home.join("replace.lock"))?;
                replacement
                    .try_lock()
                    .map_err(|_| ServerError("another upgrade is in progress".into()))?;
                // Reject unusable replacement settings before asking a healthy daemon to exit.
                crate::config::Config::load(&home)?.model_client()?;
                let previous = read_registration(&home)?.ok_or(ServerError(
                    "the old server has not finished starting; try again later".into(),
                ))?;
                // A private filesystem request avoids signalling a stale/reused PID or adding an HTTP command.
                write_json(&home, "replace.json", &previous)?;
                let deadline = Instant::now() + UPGRADE_GRACE + Duration::from_secs(15);
                loop {
                    match lock.try_lock() {
                        Ok(()) => break,
                        Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        _ => {
                            return Err(ServerError(
                                "timed out waiting for the old server to exit; it was not killed"
                                    .into(),
                            ));
                        }
                    }
                }
            }
            Err(TryLockError::WouldBlock) => {
                return Err(ServerError(
                    "a server is already running; to upgrade, run canter serve --replace".into(),
                ));
            }
            Err(_) => return Err(ServerError("cannot take the server file lock".into())),
        }
        let registration = Registration {
            pid: std::process::id(),
            version: crate::VERSION.into(),
            protocol_version: canter_protocol::PROTOCOL_VERSION,
            socket_path: home.join("server.sock"),
            instance: crate::new_id(),
        };
        let lease = Self {
            home,
            registration,
            _lock: lock,
        };
        for name in ["server.json", "server.sock", "replace.json"] {
            match fs::remove_file(lease.home.join(name)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => {
                    return Err(ServerError(
                        "cannot clean up the old server's registration file or socket".into(),
                    ));
                }
            }
        }
        Ok(lease)
    }

    pub fn publish(&self) -> Result<(), ServerError> {
        write_json(&self.home, "server.json", &self.registration)
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // The lock is still held, so a successor cannot have published these files yet.
        for name in ["server.json", "server.sock", "replace.json"] {
            let _ = fs::remove_file(self.home.join(name));
        }
    }
}

pub(crate) fn replacement_requested(home: &Path, current: &Registration) -> bool {
    fs::read(home.join("replace.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Registration>(&bytes).ok())
        .is_some_and(|request| request == *current)
}

pub(crate) fn cleanup_creating(root: &Path) -> Result<(), ServerError> {
    for entry in
        fs::read_dir(root).map_err(|_| ServerError("cannot read the sessions directory".into()))?
    {
        let entry = entry.map_err(|_| ServerError("cannot read the sessions directory".into()))?;
        if !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".creating-")
            || !entry.file_type().is_ok_and(|ty| ty.is_dir())
        {
            continue;
        }
        let path = entry.path().join("events.jsonl");
        if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            continue;
        }
        let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
            continue;
        };
        if file.try_lock().is_ok() {
            fs::remove_dir_all(entry.path()).map_err(|_| {
                ServerError("cannot clean up an unfinished session directory".into())
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Barrier,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn discovery_probes_do_not_reject_server_startup() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        private_file(&root.path().join("server.lock")).unwrap();
        let probing = AtomicBool::new(true);
        let ready = Barrier::new(9);
        let result = std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    ready.wait();
                    while probing.load(Ordering::Relaxed) {
                        server_running(root.path()).unwrap();
                    }
                });
            }
            ready.wait();
            // Exercise the same probe and acquisition used by discover / serve,
            // without HTTP or process startup diluting the lock contention.
            let result = (0..1_000)
                .try_for_each(|_| Lease::acquire(root.path().to_path_buf(), false).map(drop));
            probing.store(false, Ordering::Relaxed);
            result
        });
        result.expect("discovery alone must not report a server is already running");
    }
}
