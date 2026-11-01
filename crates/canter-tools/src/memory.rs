//! File memories owned by canter, shared by sessions but scoped to one project.
use canter_protocol::{MemoryEntry, MemoryScope};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, BufRead, Read},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::RwLock,
};

/// Shared with retired executors so a trust change revokes every file-tool exception.
pub struct Memory {
    home: PathBuf,
    cwd: PathBuf,
    roots: RwLock<Option<MemoryRoots>>,
}

#[derive(Clone)]
pub struct MemoryRoots {
    pub(crate) home: PathBuf,
    pub global: PathBuf,
    pub project: PathBuf,
}

impl Memory {
    pub fn new(home: PathBuf, cwd: PathBuf) -> Self {
        Self {
            home,
            cwd,
            roots: RwLock::new(None),
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> io::Result<()> {
        let mut roots = self.roots.write().unwrap();
        *roots = None;
        if enabled {
            *roots = Some(MemoryRoots::open(&self.home, &self.cwd)?);
        }
        Ok(())
    }

    pub fn roots(&self) -> io::Result<Option<MemoryRoots>> {
        let roots = self.roots.read().unwrap();
        if let Some(roots) = roots.as_ref() {
            roots.validate()?;
        }
        Ok(roots.clone())
    }

    pub fn list(&self) -> io::Result<Vec<MemoryEntry>> {
        let roots = self.roots()?.ok_or_else(disabled)?;
        let mut entries = Vec::new();
        for (scope, directory) in [
            (MemoryScope::Global, &roots.global),
            (MemoryScope::Project, &roots.project),
        ] {
            let mut paths = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
            paths.sort_by_key(|entry| entry.file_name());
            for entry in paths {
                let path = entry.path();
                if path.extension().is_none_or(|extension| extension != "md")
                    || !entry.file_type()?.is_file()
                {
                    continue;
                }
                let Some(file) = entry.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                // Read only the header; the body is loaded by the model's normal read tool.
                let input = fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
                    .open(&path)?;
                if !input.metadata()?.is_file() {
                    continue;
                }
                let mut reader = io::BufReader::new(input.take(64 * 1024));
                let mut first = String::new();
                let mut second = String::new();
                let header = reader
                    .read_line(&mut first)
                    .and_then(|_| reader.read_line(&mut second));
                let name = first
                    .trim()
                    .strip_prefix("# ")
                    .map(str::trim)
                    .filter(|name| !name.is_empty());
                let valid = header.is_ok() && name.is_some() && !second.trim().is_empty();
                entries.push(MemoryEntry {
                    scope,
                    file: file.clone(),
                    name: if valid { name.unwrap().into() } else { file },
                    description: if valid {
                        second.trim().into()
                    } else {
                        String::new()
                    },
                    path: path.to_string_lossy().into_owned(),
                });
            }
        }
        Ok(entries)
    }

    pub fn index(&self) -> io::Result<String> {
        let entries = self.list()?;
        let mut layers = Vec::new();
        for (scope, title) in [
            (MemoryScope::Global, "Global memories:\n"),
            (MemoryScope::Project, "Project memories:\n"),
        ] {
            let entries: Vec<_> = entries
                .iter()
                .filter(|entry| entry.scope == scope && !entry.description.is_empty())
                .collect();
            if entries.is_empty() {
                continue;
            }
            let mut text = title.to_owned();
            for (i, entry) in entries.iter().enumerate() {
                let row = format!(
                    "- {{\"name\":{},\"description\":{},\"path\":{}}}\n",
                    quote(&entry.name),
                    quote(&entry.description),
                    quote(&entry.path)
                );
                let remaining = entries.len() - i - 1;
                let notice = if remaining > 0 {
                    truncated(remaining)
                } else {
                    String::new()
                };
                if i + 2 + usize::from(remaining > 0) > 200
                    || text.len() + row.len() + notice.len() > 25_000
                {
                    text.push_str(&truncated(entries.len() - i));
                    break;
                }
                text.push_str(&row);
            }
            layers.push(text.trim_end().to_owned());
        }
        Ok(layers.join("\n\n"))
    }

    pub fn delete(&self, scope: MemoryScope, file: &str) -> io::Result<()> {
        let roots = self.roots()?.ok_or_else(disabled)?;
        let name = Path::new(file);
        if file.contains('\0')
            || name.file_name() != Some(std::ffi::OsStr::new(file))
            || name.extension().is_none_or(|extension| extension != "md")
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Expected a single .md filename",
            ));
        }
        let path = match scope {
            MemoryScope::Global => roots.global,
            MemoryScope::Project => roots.project,
        }
        .join(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() => fs::remove_file(path),
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Memory must be a regular Markdown file",
            )),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }
}

fn truncated(count: usize) -> String {
    format!(
        "Index truncated: {count} more memories are not shown. Use /memory to view the full list.\n"
    )
}

pub fn quote(text: &str) -> String {
    serde_json::to_string(text)
        .expect("string serialization")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

fn disabled() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Memory is unavailable in restricted mode",
    )
}

impl MemoryRoots {
    fn open(home: &Path, cwd: &Path) -> io::Result<Self> {
        let home = home.canonicalize()?;
        let cwd = cwd.canonicalize()?;
        // The sandbox already resolves .git pointers and a worktree's commondir.
        // Its last directory is the shared Git directory; no git process or PATH is needed.
        let (kind, identity) = match crate::sandbox::git_metadata(&cwd)?.pop() {
            Some(common) => ("git", common),
            None => ("dir", cwd),
        };
        let key = format!(
            "{kind}-{:x}",
            Sha256::digest(identity.as_os_str().as_encoded_bytes())
        );
        let global = home.join("memory");
        let projects = home.join("projects");
        let project = projects.join(key);
        let owner = fs::metadata(&home)?.uid();
        for directory in [&global, &projects, &project, &project.join("memory")] {
            match fs::DirBuilder::new().mode(0o700).create(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            let metadata = fs::symlink_metadata(directory)?;
            if !metadata.is_dir() || metadata.uid() != owner {
                return Err(io::Error::other(
                    "Memory path must be a directory owned by this user",
                ));
            }
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            home,
            global,
            project: project.join("memory"),
        })
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        for directory in [&self.global, &self.project] {
            for path in directory
                .ancestors()
                .take_while(|path| path.starts_with(&self.home))
            {
                if !fs::symlink_metadata(path)?.is_dir() || path.canonicalize()? != path {
                    return Err(io::Error::other(
                        "Memory directory changed; reopen the session",
                    ));
                }
            }
        }
        Ok(())
    }

    pub fn contains(&self, path: &Path) -> bool {
        path.starts_with(&self.global) || path.starts_with(&self.project)
    }

    pub(crate) fn excepts(&self, protected: &Path, path: &Path, ancestors: bool) -> bool {
        protected == self.home
            && (self.contains(path)
                || (ancestors
                    && [&self.global, &self.project]
                        .iter()
                        .any(|root| root.starts_with(path))))
    }

    pub(crate) fn check_component(&self, path: &Path) -> io::Result<()> {
        if self.contains(path)
            && fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_symlink())
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Memory files must not be symbolic links",
            ));
        }
        Ok(())
    }
}
