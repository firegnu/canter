//! Installs embedded skills without starting or contacting the server.
use std::{fs, io, io::Write, path::PathBuf};

const BUNDLED: [(&str, &str); 4] = [
    ("docx", include_str!("../skills/docx/SKILL.md")),
    ("pptx", include_str!("../skills/pptx/SKILL.md")),
    (
        "web-testing",
        include_str!("../skills/web-testing/SKILL.md"),
    ),
    ("xlsx", include_str!("../skills/xlsx/SKILL.md")),
];

pub fn install(force: bool) -> io::Result<String> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| io::Error::other("HOME is not set"))?;
    let root = PathBuf::from(home).join(".agents/skills");
    fs::create_dir_all(&root)?;
    let mut output = String::new();
    for (name, contents) in BUNDLED {
        let directory = root.join(name);
        let action = match fs::create_dir(&directory) {
            Ok(()) => "Installed",
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !force {
                    output.push_str(&format!(
                        "Skipped {name} (already exists; use --force to overwrite)\n"
                    ));
                    continue;
                }
                // Do not follow an existing skill directory symlink when overwriting.
                if !fs::symlink_metadata(&directory)?.is_dir() {
                    return Err(io::Error::other(format!(
                        "cannot overwrite {}: not a directory",
                        directory.display()
                    )));
                }
                "Overwritten"
            }
            Err(error) => return Err(error),
        };
        let mut file = tempfile::NamedTempFile::new_in(&directory)?;
        file.write_all(contents.as_bytes())?;
        file.persist(directory.join("SKILL.md"))?;
        output.push_str(&format!("{action} {name}\n"));
    }
    output.push_str("Available in new sessions.\n");
    Ok(output)
}
