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
        .map_err(|_| ServerError("无法创建私有状态目录"))
}

pub(crate) fn private_file(path: &Path) -> Result<File, ServerError> {
    let file = OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| ServerError("无法打开服务端状态文件"))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| ServerError("无法设置状态文件权限"))?;
    Ok(file)
}

pub(crate) fn read_registration(home: &Path) -> Result<Option<Registration>, ServerError> {
    match fs::read(home.join("server.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| ServerError("服务端登记文件无效")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(ServerError("无法读取服务端登记文件")),
    }
}

fn write_json(home: &Path, name: &str, value: &impl Serialize) -> Result<(), ServerError> {
    let mut file =
        tempfile::NamedTempFile::new_in(home).map_err(|_| ServerError("无法写登记文件"))?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|_| ServerError("无法设置登记文件权限"))?;
    serde_json::to_writer(&mut file, value).map_err(|_| ServerError("无法写登记文件"))?;
    file.write_all(b"\n")
        .and_then(|()| file.as_file().sync_all())
        .map_err(|_| ServerError("无法同步登记文件"))?;
    file.persist(home.join(name))
        .map_err(|_| ServerError("无法发布登记文件"))?;
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
        let home = fs::canonicalize(home).map_err(|_| ServerError("状态目录无效"))?;
        let lock = private_file(&home.join("server.lock"))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) if replace => {
                let replacement = private_file(&home.join("replace.lock"))?;
                replacement
                    .try_lock()
                    .map_err(|_| ServerError("另一个升级正在进行"))?;
                // Reject unusable replacement settings before asking a healthy daemon to exit.
                crate::config::Config::load(&home)?.model_client()?;
                let previous = read_registration(&home)?
                    .ok_or(ServerError("旧服务端尚未完成启动，请稍后重试"))?;
                // A private filesystem request avoids signalling a stale/reused PID or adding an HTTP command.
                write_json(&home, "replace.json", &previous)?;
                let deadline = Instant::now() + UPGRADE_GRACE + Duration::from_secs(15);
                loop {
                    match lock.try_lock() {
                        Ok(()) => break,
                        Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(100));
                        }
                        _ => return Err(ServerError("等待旧服务端退出超时，未强制终止进程")),
                    }
                }
            }
            Err(TryLockError::WouldBlock) => {
                return Err(ServerError(
                    "已经有一个服务端在运行；要升级请运行 canter serve --replace",
                ));
            }
            Err(_) => return Err(ServerError("无法取得服务端文件锁")),
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
                Err(_) => return Err(ServerError("无法清理旧服务端登记文件或 socket")),
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
    for entry in fs::read_dir(root).map_err(|_| ServerError("无法读取会话目录"))? {
        let entry = entry.map_err(|_| ServerError("无法读取会话目录"))?;
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
            fs::remove_dir_all(entry.path())
                .map_err(|_| ServerError("无法清理未完成的会话目录"))?;
        }
    }
    Ok(())
}
