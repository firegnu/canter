//! The `canter` command. `canter serve` runs the server; everything else opens
//! the terminal client, starting a server in the background when none runs.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use canter_protocol::{PermissionMode, SessionUpdateParams};
use canter_tui::client::Client;
use canter_tui::{CLIENT_NAME, Connected, Connector, Launch, Open};

pub const USAGE: &str = "\
用法：
  canter [选项] [首句]        在当前目录开新会话；没有服务端就在后台起一个
  canter [选项] --resume [会话 id]
                              接着聊；不写 id 就从会话列表里选
  canter serve [--replace]    在前台跑服务端

选项：
  -m, --model <模型>  用这个模型（默认用服务端配置里的）
  --yolo              权限模式用全放行：跑命令、改文件都不问
  -h, --help          显示这段说明
";

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Serve { replace: bool },
    Chat(ChatArgs),
    Help,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ChatArgs {
    /// Sent as the new session's first message.
    pub first: Option<String>,
    pub yolo: bool,
    pub model: Option<String>,
    /// `--resume`, with the session id when one was given.
    pub resume: Option<Option<String>>,
}

/// Parses the arguments after the program name.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let args = args
        .into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|arg| format!("参数不是 UTF-8：{}", arg.to_string_lossy()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if args.first().map(String::as_str) == Some("serve") {
        return match &args[1..] {
            [] => Ok(Command::Serve { replace: false }),
            [replace] if replace == "--replace" => Ok(Command::Serve { replace: true }),
            _ => Err("serve 只接受 --replace".into()),
        };
    }
    let mut chat = ChatArgs::default();
    let mut words = Vec::new();
    let mut args = args.into_iter().peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => words.extend(args.by_ref()),
            "-h" | "--help" => return Ok(Command::Help),
            "--yolo" => chat.yolo = true,
            "-m" | "--model" => chat.model = Some(args.next().ok_or("-m 后面要写模型名")?),
            "--resume" => chat.resume = Some(args.next_if(|next| !next.starts_with('-'))),
            option if option.starts_with('-') && option.len() > 1 => {
                return Err(format!("不认识的选项：{option}"));
            }
            _ => words.push(arg),
        }
    }
    let first = words.join(" ");
    if !first.trim().is_empty() {
        if chat.resume.is_some() {
            return Err("--resume 不能带首句；接上以后再说".into());
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
            executable: std::env::current_exe().map_err(|_| "找不到 canter 程序自己的路径")?,
            cwd: std::env::current_dir().map_err(|_| "读不到当前目录")?,
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
            .ok_or("找不到服务端")?;
        connected(connection)
    }
}

fn connected(connection: canter_server::Connection) -> Result<Connected, String> {
    let client =
        Client::new(&connection.registration.socket_path).map_err(|error| error.to_string())?;
    Ok(Connected {
        client,
        warning: connection.version_warning,
    })
}

/// Finds the server, starting one in the background when none runs, and
/// says which session to open.
pub async fn prepare(args: ChatArgs, context: Context) -> Result<Launch<ServerConnector>, String> {
    let cwd = context
        .cwd
        .to_str()
        .ok_or("当前目录的路径不是 UTF-8")?
        .to_owned();
    let connection = canter_server::ensure_server(&context.state_dir, &context.executable)
        .await
        .map_err(|error| error.to_string())?;
    let yolo = args.yolo.then_some(PermissionMode::AllowAll);
    let open = match args.resume {
        Some(id) => Open::Resume {
            id,
            cwd,
            update: SessionUpdateParams {
                name: None,
                model: args.model,
                approval_policy: yolo,
            },
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
    })
}
