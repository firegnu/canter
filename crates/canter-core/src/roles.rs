//! Global Markdown roles. Independently implemented; see P2-223 for references.
use crate::{CoreError, CoreOptions, Session};
use canter_protocol::{AgentRole, AgentRoleListResponse, AgentType, ErrorCode, ReasoningEffort};
use canter_tools::{Tool, Tools};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read, path::Path};
use yaml_rust2::{Yaml, YamlLoader};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Role {
    pub summary: AgentRole,
    pub tools: Option<Vec<String>>,
    pub prompt: String,
}

/// Immutable at spawn, including an omitted `tools` resolved against the child model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    pub name: String,
    pub tools: Vec<String>,
    pub prompt: String,
    pub model: String,
    pub effort: ReasoningEffort,
    pub read_only: bool,
}

pub(crate) fn explorer_tool(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "shell"
            | "web_fetch"
            | "web_search"
            | "update_plan"
            | "shell_output"
            | "shell_stop"
            | "monitor"
            | "mcp_search"
    )
}

pub(crate) fn general_tools(tools: &Tools) -> Vec<String> {
    Tool::ALL
        .into_iter()
        .chain(Tool::APPLY_PATCH)
        .chain(Tool::MCP)
        .filter(|tool| {
            *tool != Tool::AskUser
                && (*tool == Tool::UpdatePlan || tools.find(tool.name()).is_some())
        })
        .map(|tool| tool.name().to_owned())
        .chain(crate::goal::definitions().into_iter().map(|tool| tool.name))
        .chain(
            tools
                .mcp_tools()
                .iter()
                .filter(|_| tools.find("mcp_search").is_none())
                .map(|tool| tool.name.clone()),
        )
        .collect()
}

fn known_tool(name: &str, tools: &Tools) -> bool {
    name != "ask_user"
        && (Tool::from_name(name).is_some()
            || crate::goal::definitions()
                .iter()
                .any(|tool| tool.name == name)
            || tools.mcp_tools().iter().any(|tool| tool.name == name))
}

pub(crate) async fn load(options: &CoreOptions, tools: &Tools) -> Result<Vec<Role>, CoreError> {
    let Some(directory) = options.agent_roles_directory.clone() else {
        return Ok(Vec::new());
    };
    let mut roles = tokio::task::spawn_blocking(move || read_directory(&directory))
        .await
        .map_err(|_| CoreError::internal())?;
    let mut counts = BTreeMap::new();
    for role in &roles {
        *counts.entry(role.summary.name.clone()).or_insert(0) += 1;
    }
    for role in &mut roles {
        let error = if ["explorer", "general", "unknown"].contains(&role.summary.name.as_str()) {
            Some("Role name is reserved".into())
        } else if counts[&role.summary.name] > 1 {
            Some("Duplicate role name".into())
        } else if let Some(name) = role
            .tools
            .as_ref()
            .and_then(|names| names.iter().find(|name| !known_tool(name, tools)))
        {
            Some(format!("Unknown or forbidden tool: {name}"))
        } else if let Some(model) = &role.summary.model {
            let provider = model
                .split_once('/')
                .map(|(provider, _)| provider)
                .unwrap_or_default();
            if !options.agent_role_providers.contains(provider) {
                Some(format!("Provider is not configured: {provider}"))
            } else if !options.model_limits.contains_key(model) {
                Some(format!("Model is not configured: {model}"))
            } else {
                None
            }
        } else {
            None
        };
        if role.summary.error.is_none() {
            role.summary.error = error;
        }
    }
    Ok(roles)
}

fn unavailable(name: String, error: &str) -> Role {
    Role {
        summary: AgentRole {
            name,
            description: String::new(),
            model: None,
            effort: None,
            read_only: false,
            built_in: false,
            error: Some(error.into()),
        },
        tools: None,
        prompt: String::new(),
    }
}

fn read_directory(directory: &Path) -> Vec<Role> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => {
            return vec![unavailable(
                "agents/".into(),
                "Could not read the roles directory",
            )];
        }
    };
    let mut paths = Vec::new();
    let mut roles = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry)
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "md") =>
            {
                paths.push(entry.path())
            }
            Ok(_) => {}
            Err(_) => roles.push(unavailable(
                "agents/".into(),
                "Could not read a roles directory entry",
            )),
        }
    }
    paths.sort();
    for path in paths {
        let fallback = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let parsed = (|| {
            // Do not block the server on a FIFO/device masquerading as a role file.
            let metadata = std::fs::metadata(&path).map_err(|_| "Could not read role file")?;
            if !metadata.is_file() {
                return Err("Role must be a regular file");
            }
            let mut text = String::new();
            std::fs::File::open(&path)
                .map_err(|_| "Could not read role file")?
                .take(65_537)
                .read_to_string(&mut text)
                .map_err(|_| "Could not read role file as UTF-8")?;
            if text.len() > 65_536 {
                return Err("Role file exceeds 64 KiB");
            }
            parse(&text)
        })();
        roles.push(parsed.unwrap_or_else(|error| unavailable(fallback, error)));
    }
    roles
}

fn parse(text: &str) -> Result<Role, &'static str> {
    let text = text.replace("\r\n", "\n");
    let rest = text
        .strip_prefix("---\n")
        .ok_or("Expected YAML front matter")?;
    let (header, prompt) = rest
        .split_once("\n---")
        .filter(|(_, body)| body.is_empty() || body.starts_with('\n'))
        .ok_or("Unclosed YAML front matter")?;
    let documents = YamlLoader::load_from_str(header).map_err(|_| "Invalid YAML front matter")?;
    let [header] = documents.as_slice() else {
        return Err("Expected one YAML mapping");
    };
    let mapping = header.as_hash().ok_or("Expected one YAML mapping")?;
    if mapping.keys().any(|key| {
        !matches!(
            key.as_str(),
            Some("name" | "description" | "tools" | "model" | "effort")
        )
    }) {
        return Err(
            "Unknown role field; only name, description, tools, model and effort are allowed",
        );
    }
    let name = scalar(&header["name"])?;
    if name.len() > 64
        || !name.as_bytes()[0].is_ascii_lowercase()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(
            "Role name must start with a lowercase letter and contain 1-64 lowercase letters, digits or hyphens",
        );
    }
    let description = scalar(&header["description"])?;
    let model = optional(&header["model"])?;
    if model.as_ref().is_some_and(|model| {
        model.split_once('/').is_none_or(|(provider, model)| {
            provider.is_empty()
                || model.is_empty()
                || !provider
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                || model.chars().any(char::is_whitespace)
        })
    }) {
        return Err("Model must be provider/model");
    }
    let effort = optional(&header["effort"])?
        .map(|value| {
            serde_json::from_value::<ReasoningEffort>(serde_json::Value::String(value))
                .ok()
                .filter(|effort| *effort != ReasoningEffort::Unknown)
                .ok_or("Unknown thinking effort")
        })
        .transpose()?;
    let tools = optional(&header["tools"])?.map(|text| {
        text.split(',')
            .map(|name| name.trim().to_owned())
            .collect::<Vec<_>>()
    });
    let read_only = tools
        .as_ref()
        .is_some_and(|names| names.iter().all(|name| explorer_tool(name)));
    Ok(Role {
        summary: AgentRole {
            name,
            description,
            model,
            effort,
            read_only,
            built_in: false,
            error: None,
        },
        tools,
        prompt: prompt.trim_start_matches('\n').to_owned(),
    })
}

fn scalar(value: &Yaml) -> Result<String, &'static str> {
    value
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(|text| text.trim().to_owned())
        .ok_or("Role fields must contain nonempty text")
}
fn optional(value: &Yaml) -> Result<Option<String>, &'static str> {
    if *value == Yaml::BadValue {
        Ok(None)
    } else {
        scalar(value).map(Some)
    }
}

pub(crate) fn builtins() -> Vec<AgentRole> {
    [
        ("explorer", "Read-only code lookup or research.", true),
        (
            "general",
            "Independent work, including file changes, when explicitly delegated.",
            false,
        ),
    ]
    .into_iter()
    .map(|(name, description, read_only)| AgentRole {
        name: name.into(),
        description: description.into(),
        model: None,
        effort: None,
        read_only,
        built_in: true,
        error: None,
    })
    .collect()
}

impl Session {
    pub async fn roles(&self) -> Result<AgentRoleListResponse, CoreError> {
        let state = self.shared.state.lock().await;
        let runtime = self.shared.runtime();
        let options = if state.active.is_some() {
            runtime.options.clone()
        } else {
            self.shared.next_options()
        };
        let roles = if state.active.is_some() && state.journal.snapshot.parent.is_none() {
            state.roles.clone()
        } else {
            load(&options, &runtime.tools).await?
        };
        Ok(AgentRoleListResponse {
            data: builtins()
                .into_iter()
                .chain(roles.into_iter().map(|role| role.summary))
                .collect(),
            directory: options
                .agent_roles_directory
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "~/.canter/agents".into()),
        })
    }
}

impl crate::session::State {
    pub(crate) async fn refresh_roles(
        &mut self,
        options: &CoreOptions,
        tools: &Tools,
    ) -> Result<(), CoreError> {
        if self.journal.snapshot.parent.is_some() {
            return Ok(());
        }
        let roles = load(options, tools).await?;
        if self.roles != roles {
            self.roles = roles;
            self.refresh_tools(tools).await?;
        }
        Ok(())
    }

    pub(crate) fn role(&self, kind: &AgentType) -> Result<Option<Role>, CoreError> {
        match kind {
            AgentType::Explorer | AgentType::General => Ok(None),
            AgentType::Custom(name) => self
                .roles
                .iter()
                .find(|role| role.summary.name == *name)
                .ok_or_else(|| {
                    CoreError::new(
                        ErrorCode::InvalidRequest,
                        "Role is not available in this turn",
                    )
                })
                .and_then(|role| match &role.summary.error {
                    Some(error) => Err(CoreError::new(ErrorCode::InvalidRequest, error)),
                    None => Ok(Some(role.clone())),
                }),
            AgentType::Unknown => Err(CoreError::new(
                ErrorCode::InvalidRequest,
                "Unknown agent role",
            )),
        }
    }

    pub(crate) fn append_role_prompt(&self, prompt: &mut String) {
        if self.journal.snapshot.parent.is_some() {
            prompt.push_str(crate::agent_tools::PROMPT);
            if let Some(role) = &self.agent_role
                && !role.prompt.is_empty()
            {
                prompt.push_str("\n\n");
                prompt.push_str(&role.prompt);
            }
        }
    }
}
