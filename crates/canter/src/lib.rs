//! The `canter` command. `canter serve` runs the server; everything else opens
//! the terminal client, starting a server in the background when none runs.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use canter_protocol::{PermissionMode, SessionUpdateParams};
use canter_tui::client::Client;
use canter_tui::{CLIENT_NAME, Connected, Connector, Launch, Open};

#[cfg(test)]
mod version;

/// This build's version line, worked out by `build.rs`: `0.1.0 · 37489fe`,
/// `0.1.0 · 37489fe-dirty`, or only `0.1.0` when it was built without git.
pub const VERSION: &str = env!("CANTER_VERSION");

pub const USAGE: &str = "\
Usage:
  canter [options] [prompt]   start a new session in the current directory;
                              starts a server in the background if none runs
  canter [options] --resume [session id]
                              resume a session; without an id, pick from a list
  canter [options] -c, --continue
                              resume the latest session in the current directory
  canter serve [--replace]    run the server in the foreground
  canter usage                show daily token usage and cost by model

Options:
  -m, --model <model>  use this model (default: the server's config)
  --yolo               use permission mode Allow all (never ask)
  -V, --version        show the version and commit
  -h, --help           show this help
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Serve { replace: bool },
    Usage,
    Chat(ChatArgs),
    Help,
    Version,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ChatArgs {
    /// Sent as the new session's first message.
    pub first: Option<String>,
    pub yolo: bool,
    pub model: Option<String>,
    /// `--resume`, with the session id when one was given.
    pub resume: Option<Option<String>>,
    /// `-c` / `--continue`: this directory's latest session.
    pub continue_latest: bool,
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| format!("argument is not UTF-8: {}", arg.to_string_lossy()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if args.first().map(String::as_str) == Some("usage") {
        return if args.len() == 1 {
            Ok(Command::Usage)
        } else {
            Err("usage does not accept arguments".into())
        };
    }
    if args.first().map(String::as_str) == Some("serve") {
        return match &args[1..] {
            [] => Ok(Command::Serve { replace: false }),
            [replace] if replace == "--replace" => Ok(Command::Serve { replace: true }),
            _ => Err("serve only accepts --replace".into()),
        };
    }
    let mut chat = ChatArgs::default();
    let mut words = Vec::new();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => words.extend(args.by_ref()),
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "--yolo" => chat.yolo = true,
            "-m" | "--model" => chat.model = Some(args.next().ok_or("-m needs a model name")?),
            "--resume" => chat.resume = Some(args.next_if(|next| !next.starts_with('-'))),
            "-c" | "--continue" => chat.continue_latest = true,
            option if option.starts_with('-') && option.len() > 1 => {
                return Err(format!("unknown option: {option}"));
            }
            _ => words.push(arg),
        }
    }
    if chat.continue_latest && chat.resume.is_some() {
        return Err("use either --continue or --resume".into());
    }
    let first = words.join(" ");
    if !first.trim().is_empty() {
        if chat.resume.is_some() {
            return Err("--resume does not take a prompt; send it after resuming".into());
        }
        if chat.continue_latest {
            return Err("--continue does not take a prompt; send it after resuming".into());
        }
        chat.first = Some(first);
    }
    Ok(Command::Chat(chat))
}

/// Where this terminal is and what it sends to the server.
pub struct Context {
    /// canter's state directory (`~/.canter` or `CANTER_HOME`).
    pub state_dir: PathBuf,
    /// This program, started as `canter serve` when no server runs.
    pub executable: PathBuf,
    /// A new session works here.
    pub cwd: PathBuf,
    /// The terminal's environment, given to the session's commands.
    pub env: BTreeMap<String, String>,
    pub client_name: String,
}

impl Context {
    pub fn current() -> Result<Self, String> {
        Ok(Self {
            state_dir: canter_server::state_directory().map_err(|error| error.to_string())?,
            executable: std::env::current_exe()
                .map_err(|_| "cannot find the canter executable's own path")?,
            cwd: std::env::current_dir().map_err(|_| "cannot read the current directory")?,
            // Variables that are not UTF-8 cannot travel as JSON strings.
            env: std::env::vars_os()
                .filter_map(|(key, value)| {
                    Some((key.into_string().ok()?, value.into_string().ok()?))
                })
                .collect(),
            client_name: client_name(),
        })
    }
}

/// `canter-tui ttys003`: tells this terminal apart from the others on the same session.
fn client_name() -> String {
    let terminal = nix::unistd::ttyname(std::io::stdin())
        .ok()
        .and_then(|path| Some(path.file_name()?.to_string_lossy().into_owned()))
        .unwrap_or_else(|| format!("pid {}", std::process::id()));
    format!("{CLIENT_NAME} {terminal}")
}

/// Finds the server again by reading its registration in the state directory.
/// It does not start one: whoever stopped the server starts the next.
pub struct ServerConnector {
    state_dir: PathBuf,
}

impl Connector for ServerConnector {
    async fn connect(&self) -> Result<Connected, String> {
        let connection = canter_server::discover(&self.state_dir)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("no server found")?;
        connected(connection)
    }
}

fn connected(connection: canter_server::Connection) -> Result<Connected, String> {
    let client =
        Client::new(&connection.registration.socket_path).map_err(|error| error.to_string())?;
    Ok(Connected {
        client,
        warning: connection.version_warning,
        server: Some(connection.registration.instance),
    })
}

pub async fn usage(context: Context) -> Result<String, String> {
    let connection = canter_server::ensure_server(&context.state_dir, &context.executable)
        .await
        .map_err(|error| error.to_string())?;
    if let Some(warning) = &connection.version_warning {
        eprintln!("{warning}");
    }
    let report = connected(connection)?
        .client
        .usage(None)
        .await
        .map_err(|error| error.to_string())?;
    Ok(canter_tui::usage::format_report(&report).join("\n") + "\n")
}

/// Finds the server, starting one in the background when none runs, and
/// says which session to open.
pub async fn prepare(args: ChatArgs, context: Context) -> Result<Launch<ServerConnector>, String> {
    let cwd = context
        .cwd
        .to_str()
        .ok_or("the current directory's path is not UTF-8")?
        .to_owned();
    let connection = canter_server::ensure_server(&context.state_dir, &context.executable)
        .await
        .map_err(|error| error.to_string())?;
    let yolo = args.yolo.then_some(PermissionMode::AllowAll);
    let update = |model| SessionUpdateParams {
        plan_mode: None,
        name: None,
        model,
        reasoning_effort: None,
        approval_policy: yolo,
    };
    let open = match args.resume {
        Some(id) => Open::Resume {
            id,
            cwd,
            update: update(args.model),
        },
        None if args.continue_latest => Open::Continue {
            cwd,
            update: update(args.model),
        },
        None => Open::New {
            cwd,
            model: args.model.unwrap_or_default(),
            approval_policy: yolo.unwrap_or(PermissionMode::AlwaysAsk),
            first: args.first,
        },
    };
    Ok(Launch {
        connected: connected(connection)?,
        connector: ServerConnector {
            state_dir: context.state_dir,
        },
        open,
        home: context.env.get("HOME").cloned(),
        env: context.env,
        client_name: context.client_name,
        version: VERSION.to_owned(),
    })
}
