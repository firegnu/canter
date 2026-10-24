use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

// No git subprocess and no cache: repository/user config can change between calls.
pub(super) fn configuration_is_safe(cwd: &Path, environment: &BTreeMap<String, String>) -> bool {
    check_configuration(cwd, environment).unwrap_or(false)
}

fn check_configuration(cwd: &Path, environment: &BTreeMap<String, String>) -> io::Result<bool> {
    // These overrides change the config sources or inject config directly. Do not
    // guess their semantics or accidentally inspect the server's own environment.
    if environment.keys().any(|key| {
        matches!(
            key.as_str(),
            "GIT_DIR" | "GIT_COMMON_DIR" | "GIT_WORK_TREE" | "GIT_CONFIG"
        ) || key.starts_with("GIT_CONFIG_") && key != "GIT_CONFIG_NOSYSTEM"
    }) {
        return Ok(false);
    }
    let Some(home) = environment
        .get("HOME")
        .map(Path::new)
        .filter(|path| path.is_absolute())
    else {
        return Ok(false);
    };
    let xdg = match environment
        .get("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
    {
        Some(value) if Path::new(value).is_absolute() => PathBuf::from(value),
        Some(_) => return Ok(false),
        None => home.join(".config"),
    };
    for path in [home.join(".gitconfig"), xdg.join("git/config")] {
        if !config_file_is_safe(&path)? {
            return Ok(false);
        }
    }
    let Some(gitdir) = discover_gitdir(cwd)? else {
        return Ok(true);
    };
    let common = match read_optional(&gitdir.join("commondir"))? {
        Some(value) => reference(&gitdir, &value)?.canonicalize()?,
        None => gitdir.clone(),
    };
    // Check config.worktree even when the extension is not currently enabled.
    for path in [
        common.join("config"),
        gitdir.join("config"),
        gitdir.join("config.worktree"),
    ] {
        if !config_file_is_safe(&path)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn discover_gitdir(cwd: &Path) -> io::Result<Option<PathBuf>> {
    let cwd = cwd.canonicalize()?;
    for directory in cwd.ancestors() {
        let marker = directory.join(".git");
        if let Some(metadata) = metadata_optional(&marker)? {
            if metadata.is_dir() {
                return Ok(Some(marker.canonicalize()?));
            }
            let value =
                read_optional(&marker)?.ok_or_else(|| io::Error::other("gitdir disappeared"))?;
            let value = value
                .strip_prefix("gitdir: ")
                .ok_or_else(|| io::Error::other("unknown gitdir format"))?;
            let target = reference(directory, value)?.canonicalize()?;
            if !target.is_dir() {
                return Err(io::Error::other("gitdir is not a directory"));
            }
            return Ok(Some(target));
        }
        // Bare repositories and commands whose cwd is already the git directory.
        if metadata_optional(&directory.join("HEAD"))?.is_some_and(|meta| meta.is_file())
            && metadata_optional(&directory.join("objects"))?.is_some_and(|meta| meta.is_dir())
            && metadata_optional(&directory.join("refs"))?.is_some_and(|meta| meta.is_dir())
        {
            return Ok(Some(directory.into()));
        }
    }
    Ok(None)
}

fn metadata_optional(path: &Path) -> io::Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(_) => fs::metadata(path).map(Some), // Dangling links and unreadable targets fail closed.
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn reference(base: &Path, text: &str) -> io::Result<PathBuf> {
    let value = text.trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.trim() != value || value.contains(['\r', '\n', '\0']) {
        return Err(io::Error::other("unknown git directory reference"));
    }
    Ok(base.join(value))
}

fn read_optional(path: &Path) -> io::Result<Option<String>> {
    const LIMIT: u64 = 1024 * 1024;
    let file = match fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(error)
            if error.kind() == io::ErrorKind::NotFound && metadata_optional(path)?.is_none() =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > LIMIT {
        return Err(io::Error::other("unsupported git config file"));
    }
    let mut contents = String::new();
    file.take(LIMIT + 1).read_to_string(&mut contents)?;
    if contents.len() as u64 > LIMIT {
        return Err(io::Error::other("git config exceeds limit"));
    }
    Ok(Some(contents))
}

fn config_file_is_safe(path: &Path) -> io::Result<bool> {
    let Some(contents) = read_optional(path)? else {
        return Ok(true);
    };
    let mut section = String::new();
    for line in contents.trim_start_matches('\u{feff}').lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(['#', ';']) {
            continue;
        }
        // Continuations/escapes and malformed config need a full Git parser.
        if line.contains(['\\', '\0']) {
            return Ok(false);
        }
        if let Some(header) = line.strip_prefix('[') {
            let Some((header, suffix)) = header.rsplit_once(']') else {
                return Ok(false);
            };
            let suffix = suffix.trim();
            if !suffix.is_empty() && !suffix.starts_with(['#', ';']) {
                return Ok(false);
            }
            let mut parts = header.trim().splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or("");
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.'))
            {
                return Ok(false);
            }
            if let Some(subsection) = parts.next() {
                let subsection = subsection.trim();
                let Some(value) = subsection
                    .strip_prefix('"')
                    .and_then(|value| value.strip_suffix('"'))
                else {
                    return Ok(false);
                };
                if value.contains('"') {
                    return Ok(false);
                }
            }
            section = name.split('.').next().unwrap_or("").to_ascii_lowercase();
            if matches!(section.as_str(), "include" | "includeif") {
                return Ok(false);
            }
            continue;
        }
        if section.is_empty() || !line.as_bytes()[0].is_ascii_alphabetic() {
            return Ok(false);
        }
        let end = line
            .find(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
            .unwrap_or(line.len());
        let key = line[..end].to_ascii_lowercase();
        let value = line[end..].trim_start();
        if !value.is_empty() && !value.starts_with(['=', '#', ';']) {
            return Ok(false);
        }
        if value.chars().filter(|ch| *ch == '"').count() % 2 != 0 {
            return Ok(false);
        }
        if matches!(key.as_str(), "fsmonitor" | "textconv" | "signingkey")
            || key.contains("gpg")
            || section == "gpg"
            || section == "diff" && matches!(key.as_str(), "external" | "command")
            || section == "filter" && matches!(key.as_str(), "clean" | "smudge" | "process")
            || section == "log" && key == "showsignature"
            || (section == "pretty" || section == "format" && key == "pretty")
                && value.contains("%G")
            || section == "remote" && key == "promisor"
            || section == "extensions" && key == "partialclone"
        {
            return Ok(false);
        }
    }
    Ok(true)
}
