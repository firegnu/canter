use canter_protocol::{Skill, SkillSource};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, BufRead, BufReader},
    path::{Path, PathBuf},
};
use yaml_rust2::{Yaml, YamlLoader};

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
    let mut header = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err(invalid("missing closing --- front matter delimiter"));
        }
        if line.trim_end() == "---" {
            break;
        }
        header.push_str(&line);
    }
    let documents = YamlLoader::load_from_str(&header)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let [header] = documents.as_slice() else {
        return Err(invalid("front matter must contain one YAML mapping"));
    };
    let name = scalar(&header["name"])?;
    if name.contains(['\n', '\r']) {
        return Err(invalid("name must be single-line text"));
    }
    let description = scalar(&header["description"])?;
    let model_invocation = match &header["disable-model-invocation"] {
        Yaml::BadValue => true,
        Yaml::Boolean(disabled) => !disabled,
        _ => return Err(invalid("disable-model-invocation must be true or false")),
    };
    Ok(Skill {
        name,
        description,
        path: path.to_string_lossy().into_owned(),
        source,
        model_invocation,
    })
}

fn scalar(value: &Yaml) -> io::Result<String> {
    value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid("name and description must be nonempty text"))
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
            skill.name,
            skill
                .description
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
            skill.path
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_descriptions_reach_prompt_on_one_line() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SKILL.md");
        for (yaml, expected, prompt_description) in [
            (
                ">\n  Read files\n  carefully\n",
                "Read files carefully\n",
                "Read files carefully",
            ),
            (
                "|\n  Read files\n  carefully\n",
                "Read files\ncarefully\n",
                "Read files carefully",
            ),
            (
                ">-\n  Read files\n  carefully\n",
                "Read files carefully",
                "Read files carefully",
            ),
            (
                "|+\n  Read files\n    carefully\n\n",
                "Read files\n  carefully\n\n",
                "Read files carefully",
            ),
            (
                "'Read user''s files'\n",
                "Read user's files",
                "Read user's files",
            ),
            ("\"Read files\"\n", "Read files", "Read files"),
            ("Read files\n", "Read files", "Read files"),
        ] {
            fs::write(
                &path,
                format!(
                    "---\nname: example\ndescription: {yaml}---\nBody stays out of the prompt."
                ),
            )
            .unwrap();
            let skill = read_header(&path, SkillSource::Project).unwrap();
            assert_eq!(skill.description, expected, "description: {yaml}");
            let mut prompt = String::new();
            append_prompt(&mut prompt, &[skill]);
            assert_eq!(prompt.lines().count(), 2, "description: {yaml}");
            assert_eq!(
                prompt.lines().nth(1).unwrap(),
                format!(
                    "- example: {prompt_description} (SKILL.md: {})",
                    path.display()
                )
            );
        }
    }
}
