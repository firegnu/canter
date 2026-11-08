use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Most command output one session keeps in files here, across every call.
pub(crate) const MAX_SESSION_SAVED_OUTPUT_BYTES: u64 = 256 * 1024 * 1024;

/// Shared by every executor used during one open session, including retired ones.
pub struct ScratchDirectory {
    directory: Mutex<Option<tempfile::TempDir>>,
    path: Option<PathBuf>,
    resolved: Option<PathBuf>,
    warning: Option<String>,
    saved_output: Mutex<u64>,
}

impl ScratchDirectory {
    /// Call only while holding the store's exclusive lock for this session.
    /// The canonical storage path separates identical IDs in independent stores.
    pub fn for_session(session_directory: &Path) -> Self {
        let mut path = PathBuf::from("/tmp");
        let result = (|| {
            let session_directory = session_directory.canonicalize()?;
            let owner = std::fs::metadata(&session_directory)?.uid();
            let digest = format!(
                "{:x}",
                Sha256::digest(session_directory.as_os_str().as_encoded_bytes())
            );
            let name = format!("canter-shell-{owner}-{}", &digest[..16]);
            path = Path::new("/tmp").join(&name);
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if !metadata.is_dir()
                        || metadata.uid() != owner
                        || metadata.permissions().mode() & 0o777 != 0o700
                    {
                        return Err(io::Error::other(
                            "existing scratch path is not a private directory owned by this user",
                        ));
                    }
                    std::fs::remove_dir_all(&path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            tempfile::Builder::new()
                .prefix(&name)
                .rand_bytes(0)
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in("/tmp")
        })();
        Self::from_result(&path, result)
    }

    pub(crate) fn temporary() -> Self {
        Self::from_result(
            Path::new("/tmp"),
            tempfile::Builder::new()
                .prefix("canter-shell-")
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in("/tmp"),
        )
    }

    fn from_result(path: &Path, result: io::Result<tempfile::TempDir>) -> Self {
        let result = result.and_then(|directory| {
            let path = directory.path().to_path_buf();
            let resolved = path.canonicalize()?;
            Ok((directory, path, resolved))
        });
        match result {
            Ok((directory, path, resolved)) => Self {
                directory: Mutex::new(Some(directory)),
                path: Some(path),
                resolved: Some(resolved),
                warning: None,
                saved_output: Mutex::new(0),
            },
            Err(error) => Self {
                directory: Mutex::new(None),
                path: None,
                resolved: None,
                warning: Some(format!(
                    "Scratch directory unavailable at {}: {error}",
                    path.display()
                )),
                saved_output: Mutex::new(0),
            },
        }
    }

    /// The spelling given to commands and the model: `/tmp/...`, which follows
    /// the system link to the real directory exactly as a user's terminal does.
    pub fn path(&self) -> Option<PathBuf> {
        self.directory
            .lock()
            .unwrap()
            .as_ref()
            .and(self.path.as_ref())
            .cloned()
    }

    /// The canonical directory behind [`Self::path`]; sandbox and permission
    /// checks compare resolved file paths against this one.
    pub(crate) fn resolved_path(&self) -> Option<PathBuf> {
        self.directory
            .lock()
            .unwrap()
            .as_ref()
            .and(self.resolved.as_ref())
            .cloned()
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn close(&self) {
        self.directory.lock().unwrap().take();
    }

    /// Charge saved command output against the session's total; false when it is full.
    pub(crate) fn claim_saved_output(&self, bytes: u64) -> bool {
        let mut saved = self.saved_output.lock().unwrap();
        if saved.saturating_add(bytes) > MAX_SESSION_SAVED_OUTPUT_BYTES {
            return false;
        }
        *saved += bytes;
        true
    }
}
