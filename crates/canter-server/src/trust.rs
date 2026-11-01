//! Server-owned, positive-only folder trust. Never consult project configuration.
use canter_protocol::FolderTrustResponse;
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    os::unix::ffi::OsStrExt,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

fn repository_root(directory: &Path) -> Option<PathBuf> {
    let checkout = directory
        .ancestors()
        .find(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())?;
    Some(worktree_main(checkout).unwrap_or_else(|| checkout.to_path_buf()))
}

// Read only small, regular Git metadata files; never accept a .git symlink as
// proof that an unrelated checkout belongs to a trusted repository.
fn metadata_target(file: &Path, prefix: &[u8]) -> Option<PathBuf> {
    const LIMIT: u64 = 64 * 1024;
    let metadata = fs::symlink_metadata(file).ok()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return None;
    }
    let mut contents = Vec::new();
    fs::File::open(file)
        .ok()?
        .take(LIMIT + 1)
        .read_to_end(&mut contents)
        .ok()?;
    if contents.len() as u64 > LIMIT {
        return None;
    }
    let target = contents.trim_ascii().strip_prefix(prefix)?.trim_ascii();
    if target.is_empty() {
        return None;
    }
    Some(file.parent()?.join(std::ffi::OsStr::from_bytes(target)))
}

fn worktree_main(checkout: &Path) -> Option<PathBuf> {
    let gitdir = metadata_target(&checkout.join(".git"), b"gitdir:")?;
    if !fs::symlink_metadata(&gitdir).ok()?.is_dir() {
        return None;
    }
    let gitdir = gitdir.canonicalize().ok()?;
    let worktrees = gitdir.parent()?;
    if worktrees.file_name()? != "worktrees" {
        return None;
    }
    let common = worktrees.parent()?;
    let backlink = metadata_target(&gitdir.join("gitdir"), b"")?;
    let linked_common = metadata_target(&gitdir.join("commondir"), b"")?;
    // Compare checkout directories, not .git files which could be substituted.
    if backlink.file_name()? != ".git"
        || backlink.parent()?.canonicalize().ok()? != checkout
        || linked_common.canonicalize().ok()? != common
    {
        return None;
    }
    let main = common.parent()?;
    let dot_git = main.join(".git");
    let main_gitdir = if fs::symlink_metadata(&dot_git).ok()?.is_dir() {
        dot_git
    } else {
        metadata_target(&dot_git, b"gitdir:")?
    };
    (main_gitdir.canonicalize().ok()? == common).then(|| main.to_path_buf())
}

fn root(directory: &Path) -> io::Result<PathBuf> {
    if !directory.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an absolute folder path",
        ));
    }
    if !directory.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Directory does not exist: {}", directory.display()),
        ));
    }
    let directory = directory.canonicalize()?;
    Ok(repository_root(&directory).unwrap_or(directory))
}

fn read(home: &Path) -> io::Result<BTreeSet<PathBuf>> {
    let path = home.join("trusted_folders.json");
    let result = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::from),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error),
    };
    result.map_err(|error| io::Error::new(error.kind(), format!("{}: {error}", path.display())))
}

pub(crate) fn query(home: &Path, cwd: &Path) -> io::Result<FolderTrustResponse> {
    let path = match root(cwd) {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(FolderTrustResponse {
                path: cwd.to_string_lossy().into_owned(),
                trusted: false,
                warning: Some(format!(
                    "{error}. Opening in restricted mode; LSP and hooks are disabled."
                )),
            });
        }
        Err(error) => return Err(error),
    };
    let (trusted, warning) = match read(home) {
        Ok(records) => (records.contains(&path), None),
        Err(error) => (
            false,
            Some(format!(
                "Could not read folder trust from {error}. Opening in restricted mode; LSP and hooks are disabled."
            )),
        ),
    };
    Ok(FolderTrustResponse {
        path: path.to_string_lossy().into_owned(),
        trusted,
        warning,
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
        warning: None,
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
