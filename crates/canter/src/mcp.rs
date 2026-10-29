//! Local MCP management: never starts or contacts the canter daemon.
use canter_tools::mcp::ServerConfig;
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

const RESTART: &str = "canter serve --replace 后新开的会话生效\n";

#[derive(PartialEq, Eq)]
pub enum Command {
    Add { name: String, config: Value },
    List,
    Remove(String),
    Login(String),
    Logout(String),
}
// CLI arguments may contain credentials.
impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpCommand [values hidden]")
    }
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    match args {
        [action] if action == "list" => Ok(Command::List),
        [action, name] if action == "remove" => Ok(Command::Remove(name.clone())),
        [action, name] if action == "login" => Ok(Command::Login(name.clone())),
        [action, name] if action == "logout" => Ok(Command::Logout(name.clone())),
        [action, name, rest @ ..] if action == "add" => {
            if name.is_empty()
                || name.len() > 48
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            {
                return Err(
                    "MCP name must be 1–48 ASCII letters, digits, underscores or hyphens".into(),
                );
            }
            let mut url = None;
            let mut env = BTreeMap::new();
            let mut headers = BTreeMap::new();
            let mut command = None;
            let mut rest = rest.iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--url" if url.is_none() => {
                        url = Some(rest.next().ok_or("--url needs a URL")?.clone())
                    }
                    "--env" | "--header" => {
                        let pair = rest.next().ok_or("--env/--header needs K=V")?;
                        let (key, value) =
                            pair.split_once('=').ok_or("--env/--header needs K=V")?;
                        let key = if arg == "--header" {
                            key.to_ascii_lowercase()
                        } else {
                            key.to_owned()
                        };
                        let map = if arg == "--env" {
                            &mut env
                        } else {
                            &mut headers
                        };
                        if key.is_empty() || map.insert(key, value.to_owned()).is_some() {
                            return Err("empty or duplicate MCP environment/header name".into());
                        }
                    }
                    "--" => {
                        command = Some(rest.by_ref().cloned().collect::<Vec<_>>());
                        break;
                    }
                    _ => return Err("invalid mcp add arguments; see canter --help".into()),
                }
            }
            let config = match (url, command) {
                (Some(url), None) if env.is_empty() => {
                    json!({"transport":"http", "url":url, "headers":headers})
                }
                (None, Some(command)) if !command.is_empty() && headers.is_empty() => {
                    json!({"transport":"stdio", "command":command[0], "args":command[1..], "env":env})
                }
                _ => return Err("use --url with --header, or -- <command> with --env".into()),
            };
            validate(&config)?;
            Ok(Command::Add {
                name: name.clone(),
                config,
            })
        }
        _ => Err("invalid mcp command; see canter --help".into()),
    }
}

fn validate(value: &Value) -> Result<ServerConfig, String> {
    let config: ServerConfig =
        serde_json::from_value(value.clone()).map_err(|_| "invalid MCP configuration")?;
    config.validate()?;
    Ok(config)
}

pub async fn run(command: Command, home: &Path) -> Result<String, String> {
    // Serialize local read/modify/write commands, using a separate inode from
    // config.json because the latter is atomically replaced.
    fs::create_dir_all(home).map_err(|_| "cannot create canter state directory")?;
    fs::set_permissions(home, fs::Permissions::from_mode(0o700))
        .map_err(|_| "cannot secure canter state directory")?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(home.join("config.lock"))
        .map_err(|_| "cannot open config.lock")?;
    lock.lock().map_err(|_| "cannot lock config.json")?;
    let path = home.join("config.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "{}\n".into(),
        Err(_) => return Err("cannot read config.json".into()),
    };
    let fields: BTreeMap<String, &RawValue> =
        serde_json::from_str(&text).map_err(|_| "config.json is not a JSON object")?;
    let raw = fields.get("mcp_servers");
    let mut servers: BTreeMap<String, Value> = match raw {
        Some(raw) => {
            serde_json::from_str(raw.get()).map_err(|_| "invalid mcp_servers in config.json")?
        }
        None => BTreeMap::new(),
    };
    match command {
        Command::Login(name) => {
            let config = validate(servers.get(&name).ok_or("MCP service not found")?)?;
            let ServerConfig::Http { url, headers, .. } = config else {
                return Err("OAuth login requires an HTTP MCP service".into());
            };
            drop(lock);
            canter_tools::mcp::oauth::login(home, &name, &url, &headers).await?;
            return Ok(format!("MCP login successful.\n{RESTART}"));
        }
        Command::Logout(name) => {
            drop(lock);
            canter_tools::mcp::oauth::logout(home, &name).await?;
            return Ok("MCP credentials removed.\n".into());
        }
        Command::List => {
            let mut output = String::new();
            for (name, config) in servers {
                let transport = match validate(&config)? {
                    ServerConfig::Stdio { .. } => "stdio",
                    ServerConfig::Http { .. } => "http",
                };
                output.push_str(&format!(
                    "{}\t{transport}\n",
                    name.replace(char::is_control, "")
                ));
            }
            return Ok(if output.is_empty() {
                "No MCP services configured.\n".into()
            } else {
                output
            });
        }
        Command::Add { name, config } => {
            if servers.contains_key(&name) {
                return Err("MCP name already exists; remove it first".into());
            }
            servers.insert(name, config);
        }
        Command::Remove(name) => {
            if servers.remove(&name).is_none() {
                return Err("MCP service not found".into());
            }
        }
    }
    let replacement =
        serde_json::to_string_pretty(&servers).map_err(|_| "cannot encode MCP configuration")?;
    let mut updated = text.clone();
    if let Some(raw) = raw {
        let start = raw.get().as_ptr() as usize - text.as_ptr() as usize;
        updated.replace_range(start..start + raw.get().len(), &replacement);
    } else {
        let end = text.rfind('}').ok_or("invalid config.json")?;
        updated.insert_str(
            end,
            &format!(
                "{}\n  \"mcp_servers\": {replacement}\n",
                if fields.is_empty() { "" } else { "," }
            ),
        );
    }
    let mut file =
        tempfile::NamedTempFile::new_in(home).map_err(|_| "cannot create config temporary file")?;
    file.write_all(updated.as_bytes())
        .and_then(|_| file.as_file().sync_all())
        .map_err(|_| "cannot write config.json")?;
    file.persist(&path)
        .map_err(|_| "cannot replace config.json")?;
    Ok(RESTART.into())
}
