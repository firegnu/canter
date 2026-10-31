//! The `canter` command. Interactive and non-interactive clients share the
//! server, starting it in the background when none runs.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use canter_protocol::{PermissionMode, SessionUpdateParams};
use canter_tui::client::Client;
use canter_tui::{CLIENT_NAME, Connected, Connector, Launch, Open};

pub mod exec;
pub mod mcp;
pub mod stats;

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
  canter serve               run the server in the foreground
  canter serve --replace     replace it in the background and exit when ready
  canter usage               show daily token usage and cost by model
  canter exec [options] [prompt]  run one turn without interaction
  canter exec --resume <id> [prompt]
  canter exec --last [prompt]    continue this directory's latest session
                              omit prompt or use - to read stdin;
                              --json prints events and a final result as JSONL
  canter stats [--all] [--json]  list recent sessions (all with --all)
  canter stats <number|id> [--json|--html]
  canter stats --last [--json|--html]  inspect the latest session
  canter mcp add <name> [--env K=V]... -- <command> [args...]
  canter mcp add <name> --url <url> [--header K=V]...
  canter mcp list            list configured MCP services (values hidden)
  canter mcp remove <name>   remove an MCP service
  canter mcp login <name>    authorize a remote MCP service in the browser
  canter mcp logout <name>   delete its local OAuth credentials

Options:
  -m, --model <model>  use this model (default: the server's config)
  --add-dir <path>      add a directory to the session (repeatable)
  -r, --resume [id]     resume a session; without an id, pick from a list
  --tag <key=value>     label a new session (repeatable; last value wins)
  --yolo               use permission mode Allow all (never ask); commands still run in the sandbox
  -V, --version        show the version and commit
  -h, --help           show this help
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Serve { replace: bool, foreground: bool },
    Usage,
    Stats(stats::Args),
    Exec(exec::Args),
    Mcp(mcp::Command),
    Chat(ChatArgs),
    Help,
    Version,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ChatArgs {
    pub tags: BTreeMap<String, String>,
    pub additional_directories: Vec<String>,
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
    if args.first().map(String::as_str) == Some("mcp") {
        return mcp::parse(&args[1..]).map(Command::Mcp);
    }
    if args.first().map(String::as_str) == Some("exec") {
        if matches!(args.get(1).map(String::as_str), Some("-h" | "--help")) {
            return Ok(Command::Help);
        }
        return exec::parse(&args[1..]).map(Command::Exec);
    }
    if args.first().map(String::as_str) == Some("stats") {
        return stats::parse(&args[1..]).map(Command::Stats);
    }
    if args.first().map(String::as_str) == Some("usage") {
        return if args.len() == 1 {
            Ok(Command::Usage)
        } else {
            Err("usage does not accept arguments".into())
        };
    }
    if args.first().map(String::as_str) == Some("serve") {
        return match &args[1..] {
            [] => Ok(Command::Serve {
                replace: false,
                foreground: true,
            }),
            [replace] if replace == "--replace" => Ok(Command::Serve {
                replace: true,
                foreground: false,
            }),
            [replace, foreground] if replace == "--replace" && foreground == "--foreground" => {
                Ok(Command::Serve {
                    replace: true,
                    foreground: true,
                })
            }
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
            "--tag" => {
                let tag = args.next().ok_or("--tag needs key=value")?;
                let (key, value) = tag.split_once('=').ok_or("--tag needs key=value")?;
                if key.trim().is_empty() {
                    return Err("--tag needs a non-empty key".into());
                }
                chat.tags.insert(key.into(), value.into());
            }
            "-m" | "--model" => chat.model = Some(args.next().ok_or("-m needs a model name")?),
            "-r" | "--resume" => chat.resume = Some(args.next_if(|next| !next.starts_with('-'))),
            "-c" | "--continue" => chat.continue_latest = true,
            "--add-dir" => chat.additional_directories.push(
                args.next_if(|next| !next.starts_with('-'))
                    .ok_or("--add-dir needs a path")?,
            ),
            option if option.starts_with('-') && option.len() > 1 => {
                return Err(format!("unknown option: {option}"));
            }
            _ => words.push(arg),
        }
    }
    if chat.continue_latest && chat.resume.is_some() {
        return Err("use either --continue or --resume".into());
    }
    if !chat.tags.is_empty() && (chat.continue_latest || chat.resume.is_some()) {
        return Err("--tag is only supported for new sessions".into());
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
        non_interactive: None,
        add_directories: args.additional_directories.clone(),
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
            tags: args.tags,
            additional_directories: args.additional_directories,
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
