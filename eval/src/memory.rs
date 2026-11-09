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
/// Symbolic links in the layer's own path are refused; links inside a memory
/// directory are removed as links and never followed.
pub fn clear(home: &crate::paths::Home) -> Result<usize> {
    let mut cleared = 0;
    let global = home.root.join("memory");
    if layer_dir(&home.root, &global)? {
        cleared += clear_dir(&global)?;
    }
    let projects = home.root.join("projects");
    if !layer_dir(&home.root, &projects)? {
        return Ok(cleared);
    }
    for entry in project_dirs(&projects)? {
        let memory = entry.path().join("memory");
        if layer_dir(&home.root, &memory)? {
            cleared += clear_dir(&memory)?;
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
        let kind = entry
            .file_type()
            .map_err(|error| format!("cannot read {}: {error}", entry.path().display()))?;
        // `file_type` does not follow links: a linked entry is removed as the
        // link itself, and a real directory is removed with its contents.
        if kind.is_dir() {
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
/// The listing is sorted and does not follow symlinks, like canter's index: a
/// layer's own path may not be a link, and linked entries are not listed.
pub fn files(home: &crate::paths::Home) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let global = home.root.join("memory");
    if layer_dir(&home.root, &global)? {
        files.extend(
            names(&global)?
                .into_iter()
                .map(|name| format!("memory/{name}")),
        );
    }
    let projects = home.root.join("projects");
    if !layer_dir(&home.root, &projects)? {
        files.sort();
        return Ok(files);
    }
    for entry in project_dirs(&projects)? {
        let directory = entry.path().join("memory");
        if !layer_dir(&home.root, &directory)? {
            continue;
        }
        let project = entry.file_name().to_string_lossy().into_owned();
        for name in names(&directory)? {
            files.push(format!("projects/{project}/memory/{name}"));
        }
    }
    files.sort();
    Ok(files)
}

/// Project directories below `projects/`, refusing entries that are symbolic
/// links instead of following them.
fn project_dirs(projects: &Path) -> Result<Vec<fs::DirEntry>> {
    let entries = match fs::read_dir(projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("cannot read {}: {error}", projects.display())),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("cannot read {}: {error}", projects.display()))?;
        let kind = entry
            .file_type()
            .map_err(|error| format!("cannot read {}: {error}", entry.path().display()))?;
        if kind.is_symlink() {
            return Err(format!(
                "refusing to touch {}: it is a symbolic link",
                entry.path().display()
            ));
        }
        if kind.is_dir() {
            dirs.push(entry);
        }
    }
    Ok(dirs)
}

/// Whether `directory` exists without any component below `root` being a
/// symbolic link. Missing directories are fine and report `false`.
fn layer_dir(root: &Path, directory: &Path) -> Result<bool> {
    let relative = directory.strip_prefix(root).map_err(|_| {
        format!(
            "refusing to touch {}: it is outside {}",
            directory.display(),
            root.display()
        )
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to touch {}: it is a symbolic link",
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(format!("cannot read {}: {error}", current.display()));
            }
        }
    }
    Ok(true)
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

    fn link(target: &Path, path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, path).unwrap();
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
    fn clear_refuses_linked_layer_dirs_and_leaves_the_targets() {
        fn both_fail(home: &crate::paths::Home) {
            let clear_error = clear(home).unwrap_err();
            assert!(clear_error.contains("symbolic link"), "{clear_error}");
            let files_error = files(home).unwrap_err();
            assert!(files_error.contains("symbolic link"), "{files_error}");
        }

        let temp = tempfile::tempdir().unwrap();
        let home = home(&temp.path().join("eval-home"));
        let outside = temp.path().join("outside");
        write(&outside.join("a.md"), "# outside\n");
        fs::create_dir_all(&home.root).unwrap();

        link(&outside, &home.root.join("memory"));
        both_fail(&home);
        assert!(outside.join("a.md").exists());
        fs::remove_file(home.root.join("memory")).unwrap();

        link(&outside, &home.root.join("projects"));
        both_fail(&home);
        assert!(outside.join("a.md").exists());
        fs::remove_file(home.root.join("projects")).unwrap();

        let memory = home.root.join("projects/git-abc/memory");
        fs::create_dir_all(memory.parent().unwrap()).unwrap();
        link(&outside, &memory);
        both_fail(&home);
        assert!(outside.join("a.md").exists());
        fs::remove_file(&memory).unwrap();

        let project = home.root.join("projects/git-link");
        link(&outside, &project);
        both_fail(&home);
        assert!(outside.join("a.md").exists());
    }

    #[test]
    fn linked_entries_are_listed_as_nothing_and_removed_as_links() {
        let temp = tempfile::tempdir().unwrap();
        let home = home(&temp.path().join("eval-home"));
        let outside = temp.path().join("outside");
        write(&outside.join("a.md"), "# outside\n");
        let memory = home.root.join("memory");
        fs::create_dir_all(&memory).unwrap();
        link(&outside.join("a.md"), &memory.join("link.md"));
        link(&outside, &memory.join("link-dir"));

        assert!(files(&home).unwrap().is_empty());
        assert_eq!(clear(&home).unwrap(), 2);
        assert_eq!(fs::read_dir(&memory).unwrap().count(), 0);
        assert!(outside.join("a.md").exists());
        assert!(outside.is_dir());
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
