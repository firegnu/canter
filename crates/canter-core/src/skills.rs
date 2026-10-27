use canter_protocol::{Skill, SkillSource};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
};

pub(crate) fn discover(
    home: Option<PathBuf>,
    root: &Path,
    warnings: &mut Vec<String>,
) -> Vec<Skill> {
    let mut skills = BTreeMap::new();
    let directories = home
        .map(|home| (home.join(".agents/skills"), SkillSource::Global))
        .into_iter()
        .chain([(root.join(".agents/skills"), SkillSource::Project)]);
    for (directory, source) in directories {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                warn(warnings, &directory, error);
                continue;
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            match entry {
                Ok(entry) => paths.push(entry.path()),
                Err(error) => warn(warnings, &directory, error),
            }
        }
        paths.sort();
        for directory in paths {
            match fs::metadata(&directory) {
                Ok(metadata) if !metadata.is_dir() => continue,
                Err(error) => {
                    warn(warnings, &directory, error);
                    continue;
                }
                _ => {}
            }
            let path = directory.join("SKILL.md");
            match read_header(&path, source) {
                Ok(skill) => {
                    skills.insert(skill.name.clone(), skill);
                }
                Err(error) => warn(warnings, &path, error),
            }
        }
    }
    skills.into_values().collect()
}

fn warn(warnings: &mut Vec<String>, path: &Path, error: io::Error) {
    warnings.push(format!("Skipped skill {}: {error}", path.display()));
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_header(path: &Path, source: SkillSource) -> io::Result<Skill> {
    // A one-byte buffer prevents read-ahead into the skill body.
    let mut reader = BufReader::with_capacity(1, File::open(path)?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    if line.trim_end() != "---" {
        return Err(invalid("missing opening --- front matter delimiter"));
    }
    let mut name = None;
    let mut description = None;
    let mut model_invocation = true;
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(invalid("missing closing --- front matter delimiter"));
        }
        if line.trim_end() == "---" {
            break;
        }
        // Only top-level fields matter; nested metadata is ignored.
        if line.starts_with(char::is_whitespace) {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        match key {
            "name" => name = Some(scalar(value)?),
            "description" => description = Some(scalar(value)?),
            "disable-model-invocation" => match value.trim() {
                "true" => model_invocation = false,
                "false" => model_invocation = true,
                _ => return Err(invalid("disable-model-invocation must be true or false")),
            },
            _ => {}
        }
    }
    Ok(Skill {
        name: name.ok_or_else(|| invalid("missing name"))?,
        description: description.ok_or_else(|| invalid("missing description"))?,
        path: path.to_string_lossy().into_owned(),
        source,
        model_invocation,
    })
}

// The installed skills use single-line plain or double-quoted text.
fn scalar(value: &str) -> io::Result<String> {
    let value = value.trim();
    let text: String = if value.starts_with('"') {
        serde_json::from_str(value).map_err(|_| invalid("invalid double-quoted text"))?
    } else {
        value.to_owned()
    };
    if text.trim().is_empty() || text.contains(['\n', '\r']) {
        return Err(invalid(
            "name and description must be nonempty single-line text",
        ));
    }
    Ok(text)
}

pub(crate) fn append_prompt(text: &mut String, skills: &[Skill]) {
    let mut available = skills
        .iter()
        .filter(|skill| skill.model_invocation)
        .peekable();
    if available.peek().is_none() {
        return;
    }
    text.push_str("When a task matches one of these skills, read its SKILL.md in full first, then follow its instructions.\n");
    for skill in available {
        text.push_str(&format!(
            "- {}: {} (SKILL.md: {})\n",
            skill.name, skill.description, skill.path
        ));
    }
}
