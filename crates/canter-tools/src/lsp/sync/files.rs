use super::*;
use std::{
    fs::File,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::ffi::OsStrExt,
    },
    path::Component,
};

/// Walk already-authorized absolute paths by directory descriptors. A path
/// replaced by a symlink cannot redirect a watcher read after its scope check.
pub(super) fn open_regular(path: &Path) -> std::io::Result<File> {
    if !path.is_absolute() {
        return Err(std::io::Error::other("LSP file path must be absolute"));
    }
    let mut current = File::open("/")?;
    let mut components = path
        .components()
        .filter(|c| !matches!(c, Component::RootDir))
        .peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::other("Unsupported LSP file path"));
        };
        let name = std::ffi::CString::new(name.as_bytes())?;
        let flags = nix::libc::O_RDONLY
            | nix::libc::O_CLOEXEC
            | nix::libc::O_NOFOLLOW
            | if components.peek().is_some() {
                nix::libc::O_DIRECTORY
            } else {
                nix::libc::O_NONBLOCK
            };
        // SAFETY: current owns a live directory fd and name is NUL-terminated.
        let fd = unsafe { nix::libc::openat(current.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: openat returned a fresh owned descriptor.
        current = unsafe { File::from_raw_fd(fd) };
    }
    if !current.metadata()?.is_file() {
        return Err(std::io::Error::other("LSP document is not a regular file"));
    }
    Ok(current)
}
pub(super) async fn read_text(path: &Path) -> Result<String, LspError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let mut bytes = Vec::new();
        let file = open_regular(&path)
            .map_err(|_| LspError::new(ErrorKind::Unavailable, "Cannot read LSP document"))?;
        file.take(MAX_FILE as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| LspError::new(ErrorKind::Unavailable, "Cannot read LSP document"))?;
        if bytes.len() > MAX_FILE {
            return Err(LspError::new(
                ErrorKind::ResourceLimited,
                "LSP document exceeds the byte limit; diagnostics are incomplete",
            ));
        }
        String::from_utf8(bytes)
            .map_err(|_| LspError::new(ErrorKind::Unsupported, "LSP document is not UTF-8"))
    })
    .await
    .map_err(|_| LspError::new(ErrorKind::Unavailable, "Cannot read LSP document"))?
}
