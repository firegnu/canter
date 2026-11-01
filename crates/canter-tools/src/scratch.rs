use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Shared by every executor used during one open session, including retired ones.
pub struct ScratchDirectory {
    directory: Mutex<Option<tempfile::TempDir>>,
    path: Option<PathBuf>,
    warning: Option<String>,
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
            path = path.canonicalize()?.join(&name);
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
            let path = directory.path().canonicalize()?;
            Ok((directory, path))
        });
        match result {
            Ok((directory, path)) => Self {
                directory: Mutex::new(Some(directory)),
                path: Some(path),
                warning: None,
            },
            Err(error) => Self {
                directory: Mutex::new(None),
                path: None,
                warning: Some(format!(
                    "Scratch directory unavailable at {}: {error}",
                    path.display()
                )),
            },
        }
    }

    pub fn path(&self) -> Option<PathBuf> {
        self.directory
            .lock()
            .unwrap()
            .as_ref()
            .and(self.path.as_ref())
            .cloned()
    }

    pub fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub fn close(&self) {
        self.directory.lock().unwrap().take();
    }
}
