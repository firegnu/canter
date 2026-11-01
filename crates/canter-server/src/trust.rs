//! Server-owned, positive-only folder trust. Never consult project configuration.
use canter_protocol::FolderTrustResponse;
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

fn repository_root(directory: &Path) -> Option<PathBuf> {
    directory
        .ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

fn root(directory: &Path) -> io::Result<PathBuf> {
    if !directory.is_absolute() || !directory.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an absolute folder path",
        ));
    }
    let directory = directory.canonicalize()?;
    Ok(repository_root(&directory).unwrap_or(directory))
}

fn read(home: &Path) -> io::Result<BTreeSet<PathBuf>> {
    match fs::read(home.join("trusted_folders.json")) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error),
    }
}

pub(crate) fn query(home: &Path, cwd: &Path) -> io::Result<FolderTrustResponse> {
    let path = root(cwd)?;
    let trusted = read(home)?.contains(&path);
    Ok(FolderTrustResponse {
        path: path.to_string_lossy().into_owned(),
        trusted,
    })
}

pub(crate) fn grant(home: &Path, cwd: &Path) -> io::Result<FolderTrustResponse> {
    let path = root(cwd)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(home.join("trusted_folders.lock"))?;
    lock.lock()?;
    let mut trusted = read(home)?;
    if trusted.insert(path.clone()) {
        let mut file = tempfile::NamedTempFile::new_in(home)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
        serde_json::to_writer_pretty(&mut file, &trusted)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(home.join("trusted_folders.json"))?;
    }
    Ok(FolderTrustResponse {
        path: path.to_string_lossy().into_owned(),
        trusted: true,
    })
}

pub(crate) fn file_access(home: PathBuf) -> canter_tools::lsp::FileAccess {
    std::sync::Arc::new(move |file, context| {
        let Some(parent) = file.parent().and_then(|parent| parent.canonicalize().ok()) else {
            return false;
        };
        let boundary = std::iter::once(&context.cwd)
            .chain(&context.additional_directories)
            .filter_map(|directory| directory.canonicalize().ok())
            .filter(|directory| parent.starts_with(directory))
            .max_by_key(|directory| directory.components().count());
        let Some(boundary) = boundary else {
            return false;
        };
        let root = repository_root(&parent).unwrap_or(boundary);
        read(&home).is_ok_and(|trusted| trusted.contains(&root))
    })
}
