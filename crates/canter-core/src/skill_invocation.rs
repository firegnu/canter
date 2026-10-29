use crate::CoreError;
use canter_model::Message;
use canter_protocol::{ErrorCode, Skill, UserInput};
use serde::{Deserialize, Serialize};
use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, path::Path};

/// The exact instructions read when the input was accepted, never re-read on compaction.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub name: String,
    pub text: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Invocation {
    pub turn_id: String,
    pub skills: Vec<Snapshot>,
    pub user_message: Message,
}

pub(crate) async fn load(
    input: &[UserInput],
    available: &[Skill],
) -> Result<Vec<Snapshot>, CoreError> {
    let selected = input
        .iter()
        .filter_map(|part| match part {
            UserInput::Skill { name } => Some(
                available
                    .iter()
                    .find(|skill| skill.name == *name)
                    .cloned()
                    .ok_or_else(|| {
                        CoreError::new(
                            ErrorCode::InvalidRequest,
                            &format!("Skill not available in this session: {name}"),
                        )
                    }),
            ),
            _ => None,
        })
        .collect::<Result<Vec<_>, _>>()?;
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    tokio::task::spawn_blocking(move || {
        selected.into_iter().map(|skill| {
            let read = || -> std::io::Result<String> {
                let mut file = OpenOptions::new()
                    .read(true)
                    .custom_flags(nix::libc::O_NONBLOCK)
                    .open(&skill.path)?;
                if !file.metadata()?.is_file() {
                    return Err(std::io::Error::other("SKILL.md must be a regular file"));
                }
                let mut body = String::new();
                file.read_to_string(&mut body)?;
                Ok(body)
            };
            let body = read().map_err(|error| CoreError::new(
                ErrorCode::InvalidRequest,
                &format!("Cannot read skill {} ({}): {error}", skill.name, skill.path),
            ))?;
            let directory = Path::new(&skill.path).parent().expect("discovered SKILL.md directory");
            Ok(Snapshot {
                text: format!(
                    "<skill>\n<name>{}</name>\n<path>{}</path>\nBase directory for this skill: {}\n{body}\n</skill>",
                    skill.name, skill.path, directory.display(),
                ),
                name: skill.name,
            })
        }).collect()
    }).await.map_err(|_| CoreError::internal())?
}

pub(crate) fn message(input: &[UserInput], skills: &[Snapshot], instructions: bool) -> Message {
    let input = input
        .iter()
        .map(|part| match part {
            UserInput::Skill { name } => UserInput::Text {
                text: if instructions {
                    skills
                        .iter()
                        .find(|skill| skill.name == *name)
                        .expect("validated skill input")
                        .text
                        .clone()
                } else {
                    format!("/{name}")
                },
            },
            part => part.clone(),
        })
        .collect::<Vec<_>>();
    crate::images::message(&input)
}

pub(crate) fn latest(invocations: &[Invocation]) -> Vec<Message> {
    let mut skills = std::collections::BTreeMap::new();
    for invocation in invocations {
        for skill in &invocation.skills {
            skills.insert(&skill.name, &skill.text);
        }
    }
    skills
        .into_values()
        .map(|text| Message::User { text: text.clone() })
        .collect()
}

impl crate::session::State {
    pub(crate) async fn restate_skills(&mut self) -> Result<(), CoreError> {
        for message in latest(&self.invoked_skills) {
            self.journal
                .record(crate::context::HistoryEntry::SkillNotice {
                    message: message.clone(),
                })
                .await?;
            self.plan_mode_notices.push(message);
        }
        crate::context::flush_plan_mode_notices(&mut self.history, &mut self.plan_mode_notices);
        Ok(())
    }
}
