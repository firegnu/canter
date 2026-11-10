//! Linux command isolation. Reviewed Codex rust-v0.163.0,
//! codex-rs/linux-sandbox/src/{bwrap,seccomp}.rs; no code copied.
//! Mounts enforce file access; network namespaces and seccomp restrict networking.
//! Landlock is used ONLY for abstract Unix socket scoping (ABI 6).
use super::{Resources, Sandbox, Scope};
use nix::libc;
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
};

pub const HELP: &str = "Linux sandbox could not start. Install bubblewrap and enable unprivileged user namespaces, or set \"sandbox\": false in config.json or use --yolo.";

#[cfg(test)]
#[path = "sandbox_linux_tests.rs"]
mod tests;

fn bwrap() -> io::Result<PathBuf> {
    if !cfg!(target_arch = "x86_64") {
        return Err(io::Error::other("Linux sandbox currently requires x86_64."));
    }
    #[cfg(test)]
    if let Some(launcher) = tests::BWRAP.with(|value| value.borrow().clone()) {
        return launcher.ok_or_else(|| io::Error::other(HELP));
    }
    for directory in ["/usr/bin", "/bin", "/usr/local/bin"] {
        let executable = Path::new(directory).join("bwrap");
        if fs::metadata(&executable)
            .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            && trusted_launcher_path(&executable, 40).unwrap_or(false)
        {
            return executable.canonicalize();
        }
    }
    Err(io::Error::other(HELP))
}

fn trusted_launcher_path(path: &Path, links_left: usize) -> io::Result<bool> {
    if links_left == 0 {
        return Ok(false);
    }
    for ancestor in path.ancestors() {
        let metadata = fs::symlink_metadata(ancestor)?;
        if metadata.uid() != 0 {
            return Ok(false);
        }
        if metadata.is_symlink() {
            // Symlink mode bits do not control writes; its parent and every
            // component of the target must be trusted, including intermediate links.
            let target = fs::read_link(ancestor)?;
            let target = ancestor.parent().unwrap_or(Path::new("/")).join(target);
            if !trusted_launcher_path(&target, links_left - 1)? {
                return Ok(false);
            }
        } else if metadata.permissions().mode() & 0o022 != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn socket_roots(environment: &BTreeMap<String, String>) -> io::Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    for path in ["/run", "/var/run", "/tmp"]
        .into_iter()
        .chain(environment.get("XDG_RUNTIME_DIR").map(String::as_str))
    {
        if !Path::new(path).is_absolute() {
            return Err(io::Error::other(
                "sandbox requires an absolute XDG_RUNTIME_DIR",
            ));
        }
        match Path::new(path).canonicalize() {
            Ok(path) if !roots.iter().any(|parent| path.starts_with(parent)) => roots.push(path),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(roots)
}

enum Mount {
    Write,
    Read,
    Empty,
}

fn overlaps_kernel_mount(path: &Path) -> bool {
    ["/proc", "/sys", "/dev"]
        .iter()
        .any(|special| path.starts_with(special) || Path::new(special).starts_with(path))
}

pub(super) fn command(
    sandbox: &Sandbox,
    cwd: &Path,
    environment: &BTreeMap<String, String>,
    scope: &Scope,
    resources: &mut Resources,
) -> io::Result<tokio::process::Command> {
    let mut command = base_command(environment, scope.network)?;
    let memory = sandbox.memory_roots();
    let mut writable = vec![
        cwd.canonicalize()?,
        sandbox.cargo.clone(),
        sandbox.npm.clone(),
    ];
    writable.extend(sandbox.scratch.resolved_path());
    for path in scope
        .additional_directories
        .iter()
        .chain(&scope.write_paths)
    {
        writable.push(path.canonicalize()?);
    }
    // Metadata outside a linked worktree is writable too, as on macOS.
    for root in
        std::iter::once(cwd).chain(scope.additional_directories.iter().map(PathBuf::as_path))
    {
        writable.extend(git_metadata(root)?);
    }
    if let Some(memory) = &memory {
        writable.extend([memory.global.clone(), memory.project.clone()]);
    }
    // A later bind over these mounts would expose the host process/device view.
    // Refuse these layouts instead of silently broadening the sandbox.
    for path in &writable {
        if overlaps_kernel_mount(path) {
            return Err(io::Error::other(
                "Linux sandbox cannot use /, /proc, /sys or /dev as writable roots.",
            ));
        }
    }
    let mut exclusions = sandbox.unreadable.clone();
    exclusions.extend([sandbox.cargo.clone(), sandbox.npm.clone()]);
    exclusions.extend(sandbox.scratch.resolved_path());
    let mut git = Vec::new();
    let mut pointers = Vec::new();
    for root in std::iter::once(cwd).chain(
        scope
            .additional_directories
            .iter()
            .chain(&scope.write_paths)
            .map(PathBuf::as_path),
    ) {
        find_git(root, &exclusions, &mut git, &mut pointers)?;
    }
    git.sort();
    git.dedup();
    let mut mounts = BTreeMap::new();
    for path in &writable {
        mounts.insert(path.clone(), Mount::Write);
    }
    // These are namespace-local empty directories, never the host's writable /tmp.
    for path in socket_roots(environment)? {
        mounts.insert(path, Mount::Empty);
    }
    let mut private = crate::executor::prepare_protected_paths(sandbox.unreadable.clone())?;
    private.sort_by_key(|path| path.components().count());
    let mut hidden: Vec<PathBuf> = Vec::new();
    for path in private {
        if hidden.iter().any(|parent| path.starts_with(parent)) {
            continue;
        }
        // An explicit write root cannot reopen credentials. Only memory has
        // the existing, narrow exception inside the state directory.
        mounts.retain(|candidate, _| {
            !candidate.starts_with(&path)
                || memory
                    .as_ref()
                    .is_some_and(|roots| candidate == &roots.global || candidate == &roots.project)
        });
        mounts.insert(path.clone(), Mount::Empty);
        hidden.push(path);
    }
    for path in &sandbox.readonly {
        mounts.retain(|candidate, _| !candidate.starts_with(path));
        mounts.insert(path.clone(), Mount::Read);
    }
    for directory in git {
        for path in super::git_protected_paths(&directory)? {
            for path in crate::executor::prepare_protected_paths(vec![path])? {
                mounts.insert(path, Mount::Read);
            }
        }
    }
    for pointer in pointers {
        mounts.insert(pointer, Mount::Read);
    }
    if scope.network {
        // Ubuntu's resolver file lives below /run. Reopen only that file.
        if let Ok(resolver) = Path::new("/etc/resolv.conf").canonicalize() {
            if hidden.iter().any(|path| resolver.starts_with(path)) {
                return Err(io::Error::other("DNS resolver overlaps a protected path"));
            }
            mounts.insert(resolver, Mount::Read);
        }
    }
    // Read-only does not mean unreadable. Never reopen a denied directory
    // through a cache symlink or Git metadata pointer. Fail closed on a
    // conflicting layout instead of changing the permission scope.
    let socket_roots = socket_roots(environment)?;
    let resolver = scope
        .network
        .then(|| Path::new("/etc/resolv.conf").canonicalize().ok())
        .flatten();
    // An original cache below hidden /tmp needs no extra mount unless a write
    // root reopened its parent. Keep it hidden rather than reopening it read-only.
    mounts.retain(|path, mount| {
        !matches!(mount, Mount::Read)
            || !socket_roots.iter().any(|masked| path.starts_with(masked))
            || writable.iter().any(|root| path.starts_with(root))
            || resolver.as_ref() == Some(path)
    });
    for (path, mount) in &mounts {
        if !matches!(mount, Mount::Read) {
            continue;
        }
        let source = crate::executor::resolve_target(path, &[])?;
        if overlaps_kernel_mount(&source)
            || hidden.iter().any(|denied| source.starts_with(denied))
            || (socket_roots.iter().any(|masked| source.starts_with(masked))
                && !writable.iter().any(|root| source.starts_with(root))
                && resolver.as_ref() != Some(&source))
        {
            return Err(io::Error::other(
                "Sandbox read-only mount conflicts with a protected or hidden path.",
            ));
        }
    }
    // Pin writable ancestors as mountpoints too: a protected leaf alone does
    // not prevent its parent from being renamed along with that leaf mount.
    let protected: Vec<_> = mounts
        .iter()
        .filter(|(_, mount)| !matches!(mount, Mount::Write))
        .map(|(path, _)| path.clone())
        .collect();
    for path in protected {
        for ancestor in path.ancestors().skip(1) {
            let inherited = ancestor.ancestors().find_map(|path| mounts.get(path));
            if matches!(inherited, Some(Mount::Write)) {
                mounts.entry(ancestor.to_path_buf()).or_insert(Mount::Write);
            }
        }
    }
    let mut mounts: Vec<_> = mounts.into_iter().collect();
    mounts.sort_by(|(a, _), (b, _)| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then(a.cmp(b))
    });
    let mut empty = Vec::new();
    let mut applied_writes = Vec::new();
    for (path, mount) in mounts {
        let under_empty = empty
            .iter()
            .any(|parent: &PathBuf| path.starts_with(parent));
        let under_write = applied_writes
            .iter()
            .any(|parent: &PathBuf| path.starts_with(parent));
        if !path.exists() && !under_write {
            // Absent paths outside writable roots cannot be created anyway.
            continue;
        }
        // Missing mountpoints beneath a real writable directory require
        // temporary host placeholders. Keep leases across concurrent commands.
        if under_write {
            resources
                .mounts
                .prepare(&path, matches!(mount, Mount::Empty))?;
        }
        match mount {
            Mount::Write => {
                command.arg("--bind").arg(&path).arg(&path);
                applied_writes.push(path);
            }
            Mount::Read => {
                if under_empty || path.exists() {
                    command.arg("--ro-bind").arg(&path).arg(&path);
                }
            }
            Mount::Empty => {
                let permissions = if hidden.contains(&path) {
                    if memory.as_ref().is_some_and(|roots| path == roots.home) {
                        "711"
                    } else {
                        "000"
                    }
                } else {
                    "755"
                };
                command.args(["--perms", permissions]);
                command.arg("--tmpfs").arg(&path);
                empty.push(path);
            }
        }
    }
    // Defer until bwrap has created mountpoints for children under these roots.
    for path in &empty {
        command.arg("--remount-ro").arg(path);
    }
    command
        .args(["--", "/bin/sh"])
        .env("CARGO_HOME", &sandbox.cargo)
        .env("npm_config_cache", &sandbox.npm)
        .env_remove("NPM_CONFIG_CACHE");
    Ok(command)
}

fn find_git(
    root: &Path,
    excluded: &[PathBuf],
    directories: &mut Vec<PathBuf>,
    pointers: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if !scan(root, fs::metadata(root))?.is_some_and(|metadata| metadata.is_dir()) {
        // A single-file grant does not authorize or require walking its siblings.
        return Ok(());
    }
    let metadata = git_metadata(root)?;
    if !metadata.is_empty() {
        for ancestor in root.ancestors() {
            let path = ancestor.join(".git");
            if path.is_file() || (path.is_symlink() && path.exists()) {
                pointers.push(path);
                break;
            }
        }
        directories.extend(metadata);
    }
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Some(entries) = scan(&directory, fs::read_dir(&directory))? else {
            continue;
        };
        for entry in entries {
            let Some(entry) = scan(&directory, entry)? else {
                continue;
            };
            let path = entry.path();
            if excluded.iter().any(|parent| path.starts_with(parent))
                || ["/proc", "/sys", "/dev", "/run"]
                    .iter()
                    .any(|parent| path == Path::new(parent))
            {
                continue;
            }
            let Some(kind) = scan(&path, entry.file_type())? else {
                continue;
            };
            if entry.file_name().eq_ignore_ascii_case(".git") {
                if kind.is_dir() {
                    if scan(&path, fs::read_dir(&path))?.is_some() {
                        directories.extend(scan(&path, path.canonicalize())?);
                    }
                } else if scan_git_pointer(&path, fs::metadata(&path))?.is_some() {
                    let metadata = git_metadata(path.parent().unwrap())?;
                    if !metadata.is_empty() {
                        directories.extend(metadata);
                        pointers.push(path);
                    }
                }
            } else if kind.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

/// Missing or unusable pointers describe no existing repository to protect.
fn git_metadata(root: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(root) = scan(root, root.canonicalize())? else {
        return Ok(Vec::new());
    };
    for ancestor in root.ancestors() {
        let pointer = ancestor.join(".git");
        let Some(metadata) = scan_git_pointer(&pointer, fs::metadata(&pointer))? else {
            continue;
        };
        let target = if metadata.is_dir() {
            pointer
        } else if metadata.is_file() {
            let Some(text) = scan_git_pointer(&pointer, fs::read(&pointer))? else {
                return Ok(Vec::new());
            };
            let Some(target) = text.trim_ascii().strip_prefix(b"gitdir: ") else {
                return Ok(Vec::new());
            };
            ancestor.join(OsStr::from_bytes(target))
        } else {
            return Ok(Vec::new());
        };
        let Some(directory) = scan_git_pointer(&target, target.canonicalize())? else {
            return Ok(Vec::new());
        };
        if scan_git_pointer(&directory, fs::read_dir(&directory))?.is_none() {
            return Ok(Vec::new());
        }
        let mut directories = vec![directory.clone()];
        let common = directory.join("commondir");
        // A missing commondir is a normal standalone repository.
        match fs::read(&common) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            result => {
                let Some(text) = scan_git_pointer(&common, result)? else {
                    return Ok(Vec::new());
                };
                let target = directory.join(OsStr::from_bytes(text.trim_ascii()));
                let Some(common) = scan_git_pointer(&target, target.canonicalize())? else {
                    return Ok(Vec::new());
                };
                if scan_git_pointer(&common, fs::read_dir(&common))?.is_none() {
                    return Ok(Vec::new());
                }
                directories.push(common);
            }
        }
        return Ok(directories);
    }
    Ok(Vec::new())
}

fn scan_git_pointer<T>(path: &Path, result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Err(error)
            if error.kind() == io::ErrorKind::NotADirectory
                || error.raw_os_error() == Some(libc::ELOOP) =>
        {
            Ok(None)
        }
        result => scan(path, result),
    }
}

fn scan<T>(path: &Path, result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            if error.kind() == io::ErrorKind::PermissionDenied {
                // SAFETY: geteuid has no preconditions or memory effects.
                let uid = unsafe { libc::geteuid() };
                let foreign = path
                    .ancestors()
                    .find_map(|parent| fs::metadata(parent).ok())
                    .is_some_and(|metadata| metadata.uid() != uid);
                #[cfg(test)]
                let foreign = foreign
                    || tests::FOREIGN_DIRECTORY
                        .with(|value| value.borrow().as_deref() == Some(path));
                if foreign {
                    return Ok(None);
                }
            }
            Err(io::Error::new(
                error.kind(),
                format!("Cannot inspect {}: {error}", path.display()),
            ))
        }
    }
}

fn base_command(
    environment: &BTreeMap<String, String>,
    network: bool,
) -> io::Result<tokio::process::Command> {
    let mut command = tokio::process::Command::new(bwrap()?);
    command.env_clear().envs(environment).args([
        "--unshare-user",
        "--unshare-pid",
        "--unshare-ipc",
        "--die-with-parent",
        "--cap-drop",
        "ALL",
        "--ro-bind",
        "/",
        "/",
        "--dev",
        "/dev",
        "--proc",
        "/proc",
    ]);
    if !network {
        command.arg("--unshare-net");
    }
    let filter = seccomp_file(network)?;
    command.arg("--seccomp").arg(filter.as_raw_fd().to_string());
    let scope = if network {
        abstract_socket_scope()?
    } else {
        None
    };
    // SAFETY: only async-signal-safe syscalls between fork and exec; ownership of
    // the filter and optional ruleset is captured until spawn has finished.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(filter.as_raw_fd(), libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            if let Some(scope) = &scope
                && (libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    || libc::syscall(libc::SYS_landlock_restrict_self, scope.as_raw_fd(), 0) != 0)
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(command)
}

/// Probe namespace setup and the same seccomp/Landlock setup without file writes.
pub async fn probe() -> io::Result<bool> {
    let environment = std::env::vars().collect();
    let mut command = base_command(&environment, true)?;
    command
        .args(["--unshare-net", "--", "/usr/bin/true"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let child = crate::process::spawn_probe(command)?;
    let output = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait_with_output())
        .await
        .map_err(|_| io::Error::other(HELP))??;
    if !output.status.success() {
        return Err(io::Error::other(HELP));
    }
    Ok(abstract_socket_scope()?.is_some())
}

fn abstract_socket_scope() -> io::Result<Option<OwnedFd>> {
    // Query before sending ABI 6 fields: older kernels can reject the larger
    // structure with E2BIG rather than reporting an unknown scope.
    // SAFETY: VERSION takes a null pointer and zero size, with no memory access.
    let abi = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0_usize,
            1_u32, // LANDLOCK_CREATE_RULESET_VERSION
        )
    };
    if abi < 0 {
        let error = io::Error::last_os_error();
        return if matches!(
            error.raw_os_error(),
            Some(libc::ENOSYS | libc::EOPNOTSUPP | libc::EINVAL)
        ) {
            Ok(None)
        } else {
            Err(error)
        };
    }
    if abi < 6 {
        return Ok(None);
    }
    // linux/landlock.h: handled_access_fs, handled_access_net, scoped (ABI 6).
    let attributes = [0_u64, 0, 1]; // LANDLOCK_SCOPE_ABSTRACT_UNIX_SOCKET only
    // SAFETY: the kernel reads the fixed three-u64 ruleset structure.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            attributes.as_ptr(),
            std::mem::size_of_val(&attributes),
            0,
        )
    };
    if fd >= 0 {
        // SAFETY: successful syscall returned a new owned descriptor.
        return Ok(Some(unsafe { OwnedFd::from_raw_fd(fd as i32) }));
    }
    Err(io::Error::last_os_error())
}

fn seccomp_file(network: bool) -> io::Result<File> {
    fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }
    // Classic BPF: LD W ABS=0x20, JEQ=0x15, JSET=0x45, RET=0x06.
    let mut filter = vec![
        instruction(0x20, 0, 0, 4),          // seccomp_data.arch
        instruction(0x15, 1, 0, 0xc000003e), // AUDIT_ARCH_X86_64
        instruction(0x06, 0, 0, 0x80000000), // SECCOMP_RET_KILL_PROCESS
        instruction(0x20, 0, 0, 0),          // seccomp_data.nr
        instruction(0x45, 0, 1, 0x40000000), // reject the x32 ABI too
        instruction(0x06, 0, 0, 0x80000000),
    ];
    let denied = 0x00050000 | libc::EPERM as u32; // SECCOMP_RET_ERRNO
    for number in [
        libc::SYS_io_uring_setup,
        libc::SYS_io_uring_enter,
        libc::SYS_io_uring_register,
        libc::SYS_ptrace,
        libc::SYS_process_vm_readv,
        libc::SYS_process_vm_writev,
        libc::SYS_link,
        libc::SYS_linkat,
    ] {
        filter.push(instruction(0x15, 0, 1, number as u32));
        filter.push(instruction(0x06, 0, 0, denied));
    }
    let families: &[i32] = if network {
        &[libc::AF_UNIX, libc::AF_INET, libc::AF_INET6]
    } else {
        &[libc::AF_UNIX]
    };
    filter.push(instruction(0x15, 1, 0, libc::SYS_socket as u32));
    filter.push(instruction(
        0x15,
        0,
        (2 * families.len() + 2) as u8,
        libc::SYS_socketpair as u32,
    ));
    filter.push(instruction(0x20, 0, 0, 16)); // low word of args[0] (domain)
    for family in families {
        filter.push(instruction(0x15, 0, 1, *family as u32));
        filter.push(instruction(0x06, 0, 0, 0x7fff0000)); // SECCOMP_RET_ALLOW
    }
    filter.push(instruction(0x06, 0, 0, denied));
    filter.push(instruction(0x06, 0, 0, 0x7fff0000));
    // The file is anonymous and CLOEXEC; only bwrap receives it.
    // SAFETY: memfd_create returns a new anonymous, close-on-exec descriptor;
    // no filesystem writes are needed, including during doctor probes.
    let fd = unsafe {
        libc::syscall(
            libc::SYS_memfd_create,
            c"canter-seccomp".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful syscall transferred ownership of this descriptor.
    let mut file = unsafe { File::from_raw_fd(fd as i32) };
    // SAFETY: sock_filter is the C ABI's fully initialized 8-byte BPF instruction.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            filter.as_ptr().cast::<u8>(),
            filter.len() * std::mem::size_of::<libc::sock_filter>(),
        )
    };
    file.write_all(bytes)?;
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    // SAFETY: this is our owned memfd; sealing makes inherited copies read-only.
    if unsafe {
        libc::fcntl(
            file.as_raw_fd(),
            libc::F_ADD_SEALS,
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok(file)
}

struct Target {
    references: usize,
    device: u64,
    inode: u64,
    directory: bool,
    contents: &'static [u8],
}
static TARGETS: LazyLock<Mutex<BTreeMap<PathBuf, Target>>> = LazyLock::new(Default::default);

#[derive(Default)]
pub(crate) struct MountTargets(Vec<PathBuf>);

impl MountTargets {
    fn prepare(&mut self, path: &Path, directory: bool) -> io::Result<()> {
        let mut targets = TARGETS.lock().unwrap();
        if let Some(target) = targets.get_mut(path) {
            target.references += 1;
            self.0.push(path.to_path_buf());
            return Ok(());
        }
        if path.exists() {
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("mount has no parent"))?;
        if !parent.is_dir() {
            return Err(io::Error::other("sandbox mount parent does not exist"));
        }
        // A neutral commondir preserves normal Git discovery while mounted.
        let contents: &'static [u8] = if path.file_name().is_some_and(|name| name == "commondir") {
            b".\n"
        } else {
            b""
        };
        if directory || path.file_name().is_some_and(|name| name == "hooks") {
            fs::create_dir(path)?;
        } else {
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(path)?
                .write_all(contents)?;
        }
        let metadata = fs::symlink_metadata(path)?;
        targets.insert(
            path.to_path_buf(),
            Target {
                references: 1,
                device: metadata.dev(),
                inode: metadata.ino(),
                directory: metadata.is_dir(),
                contents,
            },
        );
        self.0.push(path.to_path_buf());
        Ok(())
    }
}

impl Drop for MountTargets {
    fn drop(&mut self) {
        let mut targets = TARGETS.lock().unwrap();
        for path in self.0.iter().rev() {
            let Some(target) = targets.get_mut(path) else {
                continue;
            };
            target.references -= 1;
            if target.references != 0 {
                continue;
            }
            if fs::symlink_metadata(path).is_ok_and(|metadata| {
                metadata.dev() == target.device && metadata.ino() == target.inode
            }) {
                if target.directory {
                    let _ = fs::remove_dir(path); // only if still empty
                } else if fs::read(path).is_ok_and(|contents| contents == target.contents) {
                    let _ = fs::remove_file(path);
                }
            }
            targets.remove(path);
        }
    }
}
