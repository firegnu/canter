//! The `canter` command. Interactive and non-interactive clients share the
//! server, starting it in the background when none runs.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use canter_protocol::{PermissionMode, SessionUpdateParams};
use canter_tui::client::Client;
use canter_tui::{CLIENT_NAME, Connected, Connector, Launch, Open};

pub mod auth;
pub mod deployment;
pub mod doctor;
pub mod exec;
pub mod mcp;
pub mod remote;
pub mod remotes;
pub mod service;
pub mod skills;
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
  canter service install     start once at login; save this terminal's PATH
  canter service uninstall   remove login startup; keep the running server
  canter upgrade [directory] [--now]  build, back up, install and replace the server
  canter rollback [--now]    restore the latest backup and replace the server
                              waits for running turns; --now waits at most 60s
  canter usage               show daily token usage and cost by model
  canter doctor              check configuration, sign-in, providers, MCP and sandbox
  canter skills install [--force]  install bundled skills; --force overwrites
  canter auth                sign in, select an account, or sign out (chatgpt)
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
  canter remote token add <name>     issue a token locally; print it once
  canter remote token list           list client names and creation times
  canter remote token remove <name>  revoke a client's token
  canter remote add <name> <url>     save a server; read its token from stdin
  canter remote list                 list server names and URLs (no tokens)
  canter remote remove <name>        remove a saved server

Options:
  --server <name>       connect to a saved remote (chat, exec, usage, stats, doctor)
  --cwd <path>          absolute server directory; required for new remote sessions
  -m, --model <model>  use this model (default: the server's config)
  --add-dir <path>      add a directory to the session (repeatable)
  --session-file <path> write the current session and server location as JSON
  -r, --resume [id]     resume a session; without an id, pick from a list
  --tag <key=value>     label a new session (repeatable; last value wins)
  --close-on-exit       close every session opened by this terminal when it exits
  --yolo               use permission mode Allow all (never ask) and disable the sandbox regardless of config
  -V, --version        show the version and commit
  -h, --help           show this help

In a session:
  /memory             view, edit, and delete global and project memories
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    WithServer { name: String, command: Box<Command> },
    Serve { replace: bool, foreground: bool },
    Usage,
    Doctor,
    Auth(auth::Command),
    Stats(stats::Args),
    Exec(exec::Args),
    Mcp(mcp::Command),
    Remote(remote::Command),
    Service(service::Command),
    Skills { force: bool },
    Deployment(deployment::Command),
    Chat(ChatArgs),
    Help,
    Version,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ChatArgs {
    /// Absolute directory on the selected server.
    pub cwd: Option<String>,
    pub close_on_exit: bool,
    pub session_file: Option<PathBuf>,
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
    // A named server is explicit on every invocation. Leave the local parser
    // unchanged, including everything after the end-of-options marker.
    let mut words = Vec::new();
    let mut server = None;
    let mut remaining = args.into_iter();
    while let Some(word) = remaining.next() {
        if word == "--" {
            words.push(word);
            words.extend(remaining);
            break;
        }
        if word == "--server" {
            if server.is_some() {
                return Err("--server may only be given once".into());
            }
            server = Some(
                remaining
                    .next()
                    .filter(|name| !name.is_empty() && !name.starts_with('-'))
                    .ok_or("--server needs a name")?,
            );
        } else {
            words.push(word);
        }
    }
    if let Some(name) = server {
        if matches!(
            words.first().map(String::as_str),
            Some(
                "serve" | "upgrade" | "rollback" | "service" | "mcp" | "skills" | "auth" | "remote"
            )
        ) {
            return Err("Run this on the server machine".into());
        }
        if words
            .iter()
            .take_while(|word| word.as_str() != "--")
            .any(|word| word == "--session-file")
        {
            return Err("--session-file is only available on the local server".into());
        }
        return Ok(Command::WithServer {
            name,
            command: Box::new(parse_words(words)?),
        });
    }
    parse_words(words)
}

fn parse_words(args: Vec<String>) -> Result<Command, String> {
    if matches!(
        args.first().map(String::as_str),
        Some("upgrade" | "rollback")
    ) {
        if matches!(args.get(1).map(String::as_str), Some("--help" | "-h")) {
            return Ok(Command::Help);
        }
        return deployment::parse(&args).map(Command::Deployment);
    }
    if args.first().map(String::as_str) == Some("service") {
        return match args[1..]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["install"] => Ok(Command::Service(service::Command::Install)),
            ["uninstall"] => Ok(Command::Service(service::Command::Uninstall)),
            ["--help" | "-h"] => Ok(Command::Help),
            _ => Err("use canter service install or canter service uninstall".into()),
        };
    }
    if args.first().map(String::as_str) == Some("mcp") {
        return mcp::parse(&args[1..]).map(Command::Mcp);
    }
    if args.first().map(String::as_str) == Some("remote") {
        return remote::parse(&args[1..]).map(Command::Remote);
    }
    if args.first().map(String::as_str) == Some("skills") {
        return match args[1..]
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["install"] => Ok(Command::Skills { force: false }),
            ["install", "--force"] => Ok(Command::Skills { force: true }),
            ["--help" | "-h"] => Ok(Command::Help),
            _ => Err("use canter skills install [--force]".into()),
        };
    }
    if args.first().map(String::as_str) == Some("auth") {
        return auth::parse(&args[1..]).map(Command::Auth);
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
    if args.first().map(String::as_str) == Some("doctor") {
        return if args.len() == 1 {
            Ok(Command::Doctor)
        } else {
            Err("doctor does not accept arguments".into())
        };
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
            "--cwd" => {
                chat.cwd = Some(
                    args.next_if(|next| !next.starts_with('-'))
                        .ok_or("--cwd needs a path")?,
                )
            }
            "--close-on-exit" => chat.close_on_exit = true,
            "--session-file" => {
                chat.session_file = Some(
                    args.next_if(|next| !next.starts_with('-'))
                        .ok_or("--session-file needs a path")?
                        .into(),
                );
            }
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
    pub server: Option<String>,
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
    pub async fn connect(&self) -> Result<Connected, String> {
        if let Some(name) = &self.server {
            return remotes::find(&self.state_dir, name)?
                .connect()
                .await
                .map(|(connected, _)| connected)
                .map_err(|error| error.to_string());
        }
        connected(
            canter_server::ensure_server(&self.state_dir, &self.executable)
                .await
                .map_err(|error| error.to_string())?,
        )
    }

    fn session_cwd(&self, args: &ChatArgs) -> Result<String, String> {
        if self.server.is_some() {
            if let Some(cwd) = &args.cwd {
                if !std::path::Path::new(cwd).is_absolute() {
                    return Err("--cwd must be an absolute path on the server machine".into());
                }
                return Ok(cwd.clone());
            }
            if args.resume.is_some() || args.continue_latest {
                return Ok(String::new());
            }
            return Err(
                "Remote sessions require --cwd <absolute path on the server machine>".into(),
            );
        }
        if args.cwd.is_some() {
            return Err("--cwd requires --server".into());
        }
        self.cwd
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| "the current directory's path is not UTF-8".into())
    }

    pub fn current() -> Result<Self, String> {
        Ok(Self {
            server: None,
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
    sandbox_disabled: bool,
    remote: Option<remotes::Remote>,
    remote_cwd: Option<String>,
}

impl Connector for ServerConnector {
    async fn connect(&self) -> Result<Connected, canter_tui::ConnectError> {
        if let Some(remote) = &self.remote {
            let (mut connected, _) = remote.connect().await?;
            connected.client = connected
                .client
                .with_sandbox_disabled(self.sandbox_disabled);
            connected.client.remote_cwd = self.remote_cwd.clone();
            return Ok(connected);
        }
        let connection = canter_server::discover(&self.state_dir)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "no server found".to_owned())?;
        let mut connected = connected(connection)?;
        connected.client = connected
            .client
            .with_sandbox_disabled(self.sandbox_disabled);
        Ok(connected)
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
    let connection = context.connect().await?;
    if let Some(warning) = &connection.warning {
        eprintln!("{warning}");
    }
    let report = connection
        .client
        .usage(None)
        .await
        .map_err(|error| error.to_string())?;
    Ok(canter_tui::usage::format_report(&report).join("\n") + "\n")
}

/// Finds the server, starting one in the background when none runs, and
/// says which session to open.
pub async fn prepare(args: ChatArgs, context: Context) -> Result<Launch<ServerConnector>, String> {
    let cwd = context.session_cwd(&args)?;
    if context.server.is_some() && args.session_file.is_some() {
        return Err("--session-file is only available on the local server".into());
    }
    let remote = context
        .server
        .as_deref()
        .map(|name| remotes::find(&context.state_dir, name))
        .transpose()?;
    let session_file = args
        .session_file
        .map(|path| {
            canter_tui::session_file::SessionFile::new(
                context.cwd.join(path),
                context.state_dir.clone(),
            )
        })
        .transpose()?;
    let (mut connected, home) = if let Some(remote) = &remote {
        let (connected, info) = remote.connect().await.map_err(|error| error.to_string())?;
        (connected, info.home)
    } else {
        (context.connect().await?, context.env.get("HOME").cloned())
    };
    connected.client.remote_cwd = args.cwd.clone();
    let yolo = args.yolo.then_some(PermissionMode::AllowAll);
    let update = |model| SessionUpdateParams {
        remember_selection: false,
        expected_name: None,
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
    connected.client = connected.client.with_sandbox_disabled(args.yolo);
    if args.close_on_exit {
        connected.client = connected
            .client
            .with_close_on_exit()
            .map_err(|error| error.to_string())?;
    }
    Ok(Launch {
        session_file,
        connected,
        connector: ServerConnector {
            remote,
            remote_cwd: args.cwd,
            state_dir: context.state_dir,
            sandbox_disabled: args.yolo,
        },
        open,
        home,
        env: context.env,
        client_name: context.client_name,
        version: VERSION.to_owned(),
    })
}
