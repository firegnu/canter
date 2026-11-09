//! The eval state directory's two memory layers: emptying them before a
//! session, listing the `.md` files they hold, and recognising memory paths in
//! session events.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Component, Path},
};

use crate::Result;

/// Empty the global memory and every project's memory, and nothing else.
pub fn clear(home: &crate::paths::Home) -> Result<usize> {
    let mut cleared = 0;
    cleared += clear_dir(&home.root.join("memory"))?;
    let projects = home.root.join("projects");
    let entries = match fs::read_dir(&projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(cleared),
        Err(error) => return Err(format!("cannot read {}: {error}", projects.display())),
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read {}: {error}", projects.display()))?;
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            cleared += clear_dir(&entry.path().join("memory"))?;
        }
    }
    Ok(cleared)
}

fn clear_dir(path: &Path) -> Result<usize> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("cannot read {}: {error}", path.display())),
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let metadata = entry
            .metadata()
            .map_err(|error| format!("cannot read {}: {error}", entry.path().display()))?;
        if metadata.is_dir() {
            fs::remove_dir_all(entry.path())
                .map_err(|error| format!("cannot remove {}: {error}", entry.path().display()))?;
        } else {
            fs::remove_file(entry.path())
                .map_err(|error| format!("cannot remove {}: {error}", entry.path().display()))?;
        }
        removed += 1;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot protect {}: {error}", path.display()))?;
    Ok(removed)
}

/// The `.md` files in both layers, named relative to the eval state directory.
/// The listing is sorted and does not follow symlinks, like canter's index.
pub fn files(home: &crate::paths::Home) -> Result<Vec<String>> {
    let mut files: Vec<String> = names(&home.root.join("memory"))?
        .into_iter()
        .map(|name| format!("memory/{name}"))
        .collect();
    let projects = home.root.join("projects");
    let entries = match fs::read_dir(&projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            files.sort();
            return Ok(files);
        }
        Err(error) => return Err(format!("cannot read {}: {error}", projects.display())),
    };
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read {}: {error}", projects.display()))?;
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let project = entry.file_name().to_string_lossy().into_owned();
        for name in names(&entry.path().join("memory"))? {
            files.push(format!("projects/{project}/memory/{name}"));
        }
    }
    files.sort();
    Ok(files)
}

fn names(directory: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", directory.display())),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read {}: {error}", directory.display()))?;
        if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
            continue;
        }
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "md")
        {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Whether `path` is a `.md` file in `memory/` or `projects/<key>/memory/`.
pub fn is_memory_md(home: &crate::paths::Home, path: &Path) -> bool {
    if path.extension().is_none_or(|extension| extension != "md") {
        return false;
    }
    let Some(relative) = crate::paths::relative_to(path, &home.root) else {
        return false;
    };
    let parts: Vec<Component> = relative.components().collect();
    match parts.as_slice() {
        [Component::Normal(memory), Component::Normal(_file)] => *memory == "memory",
        [
            Component::Normal(projects),
            Component::Normal(_key),
            Component::Normal(memory),
            Component::Normal(_file),
        ] => *projects == "projects" && *memory == "memory",
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(root: &Path) -> crate::paths::Home {
        crate::paths::Home {
            root: root.to_path_buf(),
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn lists_only_direct_md_files_in_the_two_layers() {
        let temp = tempfile::tempdir().unwrap();
        let home = home(temp.path());
        write(&home.root.join("memory/a.md"), "# a\n");
        write(&home.root.join("memory/notes.txt"), "no\n");
        write(&home.root.join("memory/nested/b.md"), "# b\n");
        write(&home.root.join("projects/git-abc/memory/c.md"), "# c\n");
        write(&home.root.join("projects/git-abc/other.md"), "# no\n");
        assert_eq!(
            files(&home).unwrap(),
            ["memory/a.md", "projects/git-abc/memory/c.md"]
        );
    }

    #[test]
    fn recognises_memory_files_only_in_the_two_layers() {
        let temp = tempfile::tempdir().unwrap();
        let home = home(temp.path());
        assert!(is_memory_md(&home, &home.root.join("memory/a.md")));
        assert!(is_memory_md(
            &home,
            &home.root.join("projects/git-abc/memory/c.md")
        ));
        assert!(!is_memory_md(&home, &home.root.join("memory/a.txt")));
        assert!(!is_memory_md(&home, &home.root.join("sessions/a.md")));
        assert!(!is_memory_md(
            &home,
            &home.root.join("projects/git-abc/other/c.md")
        ));
        assert!(!is_memory_md(&home, Path::new("/elsewhere/memory/a.md")));
    }

    #[test]
    fn clear_empties_the_two_layers_and_leaves_neighbours() {
        let temp = tempfile::tempdir().unwrap();
        let home = home(temp.path());
        write(&home.root.join("memory/a.md"), "# a\n");
        write(&home.root.join("projects/git-abc/memory/c.md"), "# c\n");
        write(&home.root.join("projects/git-abc/README"), "keep\n");
        write(&home.root.join("config.json"), "{}");
        assert_eq!(clear(&home).unwrap(), 2);
        assert_eq!(fs::read_dir(home.root.join("memory")).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(home.root.join("projects/git-abc/memory"))
                .unwrap()
                .count(),
            0
        );
        assert!(home.root.join("projects/git-abc/README").exists());
        assert!(home.root.join("config.json").exists());
    }
}
