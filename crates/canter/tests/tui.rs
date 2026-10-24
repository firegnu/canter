//! The terminal client against a real `canter serve` and a fake model, going
//! through the same path as the `canter` command: argument parsing, finding
//! (or starting) the server, opening the session, the screen. Screens are
//! ratatui's TestBackend; each test has its own temporary state directory,
//! socket and working directories, and stops the servers it started by PID.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Json, Router, extract::State, routing::post};
use canter::{Command, Context};
use canter_protocol::{SessionSnapshot, SessionStatus, TurnStatus};
use canter_tui::client::Client;
use canter_tui::{FrameSync, InlineTerminal, RunError, run_on};
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use unicode_width::UnicodeWidthStr;

const WIDTH: u16 = 100;
const HEIGHT: u16 = 30;
const WAIT: Duration = Duration::from_secs(20);
/// What the shell tool writes when it runs with a session's environment.
const WRITE_ENV: &str = r#"printf '%s' "$CANTER_TEST_ENV" > env.txt"#;

/// One user's machine: a state directory with a config pointing at a fake
/// model, a home, a project directory, and maybe a running server.
struct World {
    root: TempDir,
    model: JoinHandle<()>,
    server: Option<Child>,
}

impl World {
    /// `replies` are the fake model's answers, one per model request.
    async fn new(replies: Vec<String>) -> Self {
        // A short absolute path keeps the socket within macOS's length limit.
        let root = tempfile::Builder::new()
            .prefix("canter-tui-")
            .tempdir_in("/tmp")
            .unwrap();
        for dir in ["state", "home", "work"] {
            fs::create_dir(root.path().join(dir)).unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let script = Arc::new(Mutex::new(VecDeque::from(replies)));
        let app = Router::new()
            .route("/chat/completions", post(fake_model))
            .with_state(script);
        let model = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let config = root.path().join("state/config.json");
        fs::write(
            &config,
            serde_json::to_vec(&json!({
                "base_url": base_url, "model": "fake-model",
                "context_window": 1048576, "api_key": "test-only-canter-key"
            }))
            .unwrap(),
        )
        .unwrap();
        fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            root,
            model,
            server: None,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.path().join(name)
    }

    /// Starts `canter serve` the way a user would by hand.
    async fn start_server(&mut self) {
        let log = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path("server.log"))
            .unwrap();
        let child = tokio::process::Command::new(env!("CARGO_BIN_EXE_canter"))
            .arg("serve")
            .env_clear()
            .env("CANTER_HOME", self.path("state"))
            .env("HOME", self.path("home"))
            .current_dir(self.path("work"))
            .stdin(std::process::Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        self.server = Some(child);
        let state = self.path("state");
        tokio::time::timeout(WAIT, async {
            while !matches!(canter_server::discover(&state).await, Ok(Some(_))) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("server ready; log:\n{}", self.log()));
    }

    /// Stops the server this test started, by its PID.
    async fn stop_server(&mut self, signal: Signal) {
        let mut child = self.server.take().expect("a server this test started");
        kill(Pid::from_raw(child.id().unwrap() as i32), signal).unwrap();
        tokio::time::timeout(WAIT, child.wait())
            .await
            .expect("server exits")
            .unwrap();
    }

    fn log(&self) -> String {
        fs::read_to_string(self.path("server.log")).unwrap_or_default()
    }

    /// A terminal in `cwd`. Its environment holds a value the shell tool can
    /// write out, to see which terminal's environment a command ran with.
    fn context(&self, client_name: &str, cwd: &Path) -> Context {
        Context {
            state_dir: self.path("state"),
            executable: PathBuf::from(env!("CARGO_BIN_EXE_canter")),
            cwd: cwd.to_owned(),
            env: BTreeMap::from([
                ("HOME".to_owned(), self.path("home").display().to_string()),
                ("CANTER_TEST_ENV".to_owned(), env_value(client_name)),
            ]),
            client_name: client_name.to_owned(),
        }
    }

    /// Types `canter <args>` in a new terminal in the project directory.
    async fn open(&self, args: &[&str], client_name: &str) -> Terminal {
        Terminal::start(args, self.context(client_name, &self.path("work"))).await
    }

    async fn snapshot(&self, id: &str) -> SessionSnapshot {
        let client = Client::new(&self.path("state/server.sock")).unwrap();
        client.read_session(id).await.unwrap().session
    }

    async fn only_session(&self) -> String {
        let client = Client::new(&self.path("state/server.sock")).unwrap();
        let sessions = client.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        sessions[0].id.clone()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.model.abort();
    }
}

fn env_value(client_name: &str) -> String {
    format!("environment of {client_name}")
}

/// A terminal running the client, with a screen the test can look at.
struct Terminal {
    screen: Arc<Mutex<TestBackend>>,
    keys: mpsc::UnboundedSender<TermEvent>,
    task: JoinHandle<Result<Option<String>, RunError>>,
}

impl Terminal {
    async fn start(args: &[&str], context: Context) -> Self {
        let Command::Chat(args) = canter::parse(args.iter().map(OsString::from)).unwrap() else {
            panic!("not a chat command");
        };
        let launch = canter::prepare(args, context).await.unwrap();
        let screen = Arc::new(Mutex::new(TestBackend::new(WIDTH, HEIGHT)));
        let terminal = InlineTerminal::new(Screen(screen.clone())).unwrap();
        let (keys, receiver) = mpsc::unbounded_channel();
        let task = tokio::spawn(run_on(terminal, receiver, launch));
        Self { screen, keys, task }
    }

    fn press(&self, code: KeyCode) {
        self.press_with(code, KeyModifiers::NONE);
    }

    fn press_with(&self, code: KeyCode, modifiers: KeyModifiers) {
        let _ = self
            .keys
            .send(TermEvent::Key(KeyEvent::new(code, modifiers)));
    }

    fn type_line(&self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c));
        }
        self.press(KeyCode::Enter);
    }

    /// The scrollback, then the screen.
    fn text(&self) -> String {
        let backend = self.screen.lock().unwrap();
        rows(backend.scrollback()) + &rows(backend.buffer())
    }

    /// Only what is on the screen now.
    fn screen(&self) -> String {
        rows(self.screen.lock().unwrap().buffer())
    }

    /// The screen's status line: the row above the input or approval box.
    /// Before the first frame there is none.
    fn status(&self) -> String {
        let screen = self.screen();
        let lines: Vec<&str> = screen.lines().collect();
        match lines.iter().rposition(|line| line.starts_with('╭')) {
            Some(top) if top > 0 => lines[top - 1].trim().to_owned(),
            _ => "（还没画出来）".to_owned(),
        }
    }

    async fn wait_idle(&self) {
        self.wait_until("idle", |terminal| terminal.status().is_empty())
            .await;
    }

    async fn wait_until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let waited = tokio::time::timeout(WAIT, async {
            while !done(self) {
                assert!(!self.task.is_finished(), "the client stopped");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if waited.is_err() {
            panic!("waited for {what}; the terminal shows:\n{}", self.text());
        }
    }

    async fn wait_for(&self, text: &str) {
        self.wait_until(text, |terminal| terminal.text().contains(text))
            .await;
    }

    /// Ctrl+C on an empty input quits; returns the session id the client had.
    async fn quit(self) -> String {
        self.press_with(KeyCode::Char('c'), KeyModifiers::CONTROL);
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("client quits")
            .unwrap()
            .unwrap()
            .expect("a session was open")
    }
}

/// The test's handle on a TestBackend that the client draws on.
struct Screen(Arc<Mutex<TestBackend>>);

impl Backend for Screen {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.0.lock().unwrap().draw(content)
    }
    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.0.lock().unwrap().append_lines(n)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.lock().unwrap().hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.0.lock().unwrap().show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.0.lock().unwrap().get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.0.lock().unwrap().set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.0.lock().unwrap().clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.0.lock().unwrap().clear_region(clear_type)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        self.0.lock().unwrap().size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.0.lock().unwrap().window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.lock().unwrap().flush()
    }
}

impl FrameSync for Screen {}

/// The buffer as text, one row per line, trailing spaces trimmed. Cells
/// hidden under a wide character are skipped.
fn rows(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut row = String::new();
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            row.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        out += row.trim_end();
        out.push('\n');
    }
    out
}

async fn fake_model(
    State(script): State<Arc<Mutex<VecDeque<String>>>>,
    Json(_body): Json<Value>,
) -> impl axum::response::IntoResponse {
    let reply = script
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected model request");
    ([("content-type", "text/event-stream")], reply)
}

fn text_reply(text: &str) -> String {
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"content":text},"finish_reason":"stop"}]})
    )
}

fn shell_reply(command: &str) -> String {
    let arguments = json!({ "command": command }).to_string();
    format!(
        "data: {}\n\ndata: [DONE]\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{
            "index":0,"id":"call-shell","type":"function",
            "function":{"name":"shell","arguments":arguments}
        }]},"finish_reason":"tool_calls"}]})
    )
}

/// 「这样我随时可以在任何terminal中启动。」: with no server running, `canter`
/// in some directory starts one in the background, opens a session there
/// with this terminal's environment, and talks.
#[tokio::test]
async fn canter_opens_in_any_directory_and_starts_the_server() {
    let world = World::new(vec![shell_reply(WRITE_ENV), text_reply("写好了")]).await;
    let anywhere = world.path("somewhere/else/deep");
    fs::create_dir_all(&anywhere).unwrap();
    assert!(!world.path("state/server.json").exists(), "no server yet");

    let terminal = Terminal::start(&[], world.context("canter-tui a", &anywhere)).await;
    // The info line: default model, asking each time, this directory.
    terminal
        .wait_for("fake-model · 每次问 · /tmp/canter-tui-")
        .await;
    assert!(terminal.screen().contains("somewhere/else/deep"));
    let registration: Value =
        serde_json::from_slice(&fs::read(world.path("state/server.json")).unwrap()).unwrap();
    let daemon = Pid::from_raw(registration["pid"].as_u64().unwrap() as i32);
    assert_ne!(daemon.as_raw() as u32, std::process::id());

    terminal.type_line("把环境写出来");
    terminal.wait_for("要跑这条命令吗？").await;
    assert!(terminal.screen().contains(WRITE_ENV), "the whole command");
    terminal.press(KeyCode::Enter);
    terminal.wait_for("写好了").await;
    assert!(terminal.text().contains("你选了：允许这一次"));
    assert_eq!(
        fs::read_to_string(anywhere.join("env.txt")).unwrap(),
        env_value("canter-tui a"),
        "the command ran in that directory with this terminal's environment"
    );
    let id = terminal.quit().await;
    let snapshot = world.snapshot(&id).await;
    assert_eq!(snapshot.settings.cwd, anywhere.display().to_string());
    assert_eq!(snapshot.settings.name.as_deref(), Some("把环境写出来"));

    // The background server is this test's to stop.
    kill(daemon, Signal::SIGTERM).unwrap();
    tokio::time::timeout(WAIT, async {
        while kill(daemon, None) != Err(Errno::ESRCH) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("background server exits");
}

/// Two terminals on one session see the same conversation, each message
/// marked with the terminal it came from.
#[tokio::test]
async fn two_terminals_share_one_session() {
    let mut world = World::new(vec![text_reply("回答一"), text_reply("回答二")]).await;
    world.start_server().await;
    let a = world.open(&["第一句"], "canter-tui a").await;
    a.wait_for("回答一").await;
    let id = world.only_session().await;
    let b = world.open(&["--resume", &id], "canter-tui b").await;
    // B first shows what the session already has.
    b.wait_for("回答一").await;
    assert!(b.text().contains("› 第一句  （来自 canter-tui a）"));

    b.type_line("第二句");
    for terminal in [&a, &b] {
        terminal.wait_for("回答二").await;
    }
    assert!(a.text().contains("› 第二句  （来自 canter-tui b）"));
    assert!(
        b.text().contains("› 第二句\n"),
        "own messages are not marked"
    );
    assert!(a.text().contains("› 第一句\n"));
    // Apart from the marks, both show the same conversation.
    let conversation = |terminal: &Terminal| {
        let text = terminal
            .text()
            .replace("  （来自 canter-tui a）", "")
            .replace("  （来自 canter-tui b）", "");
        let start = text.find("› 第一句").unwrap();
        let end = text.find("回答二").unwrap();
        text[start..end].to_owned()
    };
    assert_eq!(conversation(&a), conversation(&b));

    assert_eq!(a.quit().await, id);
    assert_eq!(b.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}

/// After the server restarts, the client reconnects by itself and carries on
/// from where its stream broke: nothing lost, nothing twice.
#[tokio::test]
async fn client_reconnects_after_the_server_restarts() {
    let mut world = World::new(vec![text_reply("回答一"), text_reply("回答二")]).await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.type_line("第一句");
    a.wait_for("回答一").await;

    world.stop_server(Signal::SIGTERM).await;
    a.wait_until("重连中", |a| a.status().contains("重连中"))
        .await;
    world.start_server().await;
    a.wait_until("reconnected", |a| !a.status().contains("重连中"))
        .await;
    a.type_line("第二句");
    a.wait_for("回答二").await;
    a.wait_idle().await;
    let id = world.only_session().await;
    let snapshot = world.snapshot(&id).await;
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert_eq!(snapshot.turns.len(), 2);

    // A fresh terminal on the session shows exactly what A shows.
    let fresh = world.open(&["--resume", &id], "canter-tui a").await;
    fresh.wait_for("回答二").await;
    fresh.wait_idle().await;
    assert_eq!(a.text().trim_end(), fresh.text().trim_end());
    assert!(!a.text().contains("对不上"));

    assert_eq!(a.quit().await, id);
    assert_eq!(fresh.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}

/// Two terminals answer the same approval at once: the first answer counts;
/// the other box closes and says who answered.
#[tokio::test]
async fn the_later_answer_to_an_approval_closes_and_names_the_first() {
    let mut world = World::new(vec![shell_reply(WRITE_ENV), text_reply("做完了")]).await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.wait_idle().await;
    // B opens the session from the list: this directory's sessions come first.
    let b = world.open(&["--resume"], "canter-tui b").await;
    b.wait_for("接着哪个会话？").await;
    b.press(KeyCode::Enter);
    b.wait_idle().await;

    a.type_line("跑一下");
    for terminal in [&a, &b] {
        terminal.wait_for("要跑这条命令吗？").await;
    }
    // A allows (option 1), B declines (key 3), at the same moment.
    a.press(KeyCode::Enter);
    b.press(KeyCode::Char('3'));
    for terminal in [&a, &b] {
        terminal
            .wait_until("the box to close", |t| {
                !t.screen().contains("要跑这条命令吗？")
            })
            .await;
        terminal.wait_for("做完了").await;
    }

    let id = world.only_session().await;
    assert!(world.snapshot(&id).await.pending_requests.is_empty());
    let (winner, loser, label) = if a.text().contains("你选了：允许这一次") {
        (&a, &b, "canter-tui a 答了：允许这一次")
    } else {
        (&b, &a, "canter-tui b 答了：拒绝")
    };
    assert!(
        loser.text().contains(label),
        "the later terminal names who answered:\n{}",
        loser.text()
    );
    assert!(loser.text().contains("你的回答晚了一步，没有生效"));
    assert!(!winner.text().contains("晚了一步"));
    assert!(!winner.text().contains(" 答了："));

    a.quit().await;
    b.quit().await;
    world.stop_server(Signal::SIGTERM).await;
}

/// The server dies while the box is open. After reconnecting, the box and the
/// session's state agree with the server: the request was cancelled, the turn
/// interrupted, the session idle. The resumed session runs commands with this
/// terminal's environment.
#[tokio::test]
async fn approval_box_matches_the_server_after_a_restart() {
    let mut world = World::new(vec![
        shell_reply("echo never"),
        shell_reply(WRITE_ENV),
        text_reply("跑完了"),
    ])
    .await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.type_line("跑一下");
    a.wait_for("要跑这条命令吗？").await;
    assert!(a.status().contains("等你批准"));

    world.stop_server(Signal::SIGKILL).await;
    a.wait_until("重连中", |a| a.status().contains("重连中"))
        .await;
    world.start_server().await;
    a.wait_until("the box to close", |a| {
        !a.screen().contains("要跑这条命令吗？") && !a.status().contains("重连中")
    })
    .await;
    a.wait_for("批准请求取消了").await;
    a.wait_for("这一轮被打断了").await;
    assert_eq!(a.status(), "", "idle");

    let id = world.only_session().await;
    let snapshot = world.snapshot(&id).await;
    assert!(snapshot.pending_requests.is_empty());
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert_eq!(snapshot.turns[0].status, TurnStatus::Interrupted);

    a.type_line("再跑一下");
    a.wait_for("要跑这条命令吗？").await;
    a.press(KeyCode::Char('1'));
    a.wait_for("跑完了").await;
    a.wait_idle().await;
    assert_eq!(
        fs::read_to_string(world.path("work/env.txt")).unwrap(),
        env_value("canter-tui a"),
        "the reconnected session has this terminal's environment"
    );
    let snapshot = world.snapshot(&id).await;
    assert!(matches!(snapshot.turns[1].status, TurnStatus::Completed));

    a.quit().await;
    world.stop_server(Signal::SIGTERM).await;
}
