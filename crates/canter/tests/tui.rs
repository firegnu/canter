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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::{Json, Router, extract::State, routing::post};
use canter::{Command, Context};
use canter_protocol::{
    Item, PermissionMode, SessionCloseReason, SessionSnapshot, SessionStatus, TurnStatus,
};
use canter_tui::client::Client;
use canter_tui::{ANSWER_GUARD_MS, FrameSync, InputSender, RunError, run_on};
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

    /// Another client closes the session: `DELETE /v1/sessions/{id}`.
    async fn close_session(&self, id: &str) {
        let http = reqwest::Client::builder()
            .unix_socket(self.path("state/server.sock"))
            .build()
            .unwrap();
        let response = http
            .delete(format!("http://localhost/v1/sessions/{id}"))
            .send()
            .await
            .unwrap();
        assert!(response.status().is_success(), "{response:?}");
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
    keys: InputSender,
    frames: Frames,
    task: JoinHandle<Result<Option<String>, RunError>>,
}

impl Terminal {
    async fn start(args: &[&str], context: Context) -> Self {
        let Command::Chat(args) = canter::parse(args.iter().map(OsString::from)).unwrap() else {
            panic!("not a chat command");
        };
        let launch = canter::prepare(args, context).await.unwrap();
        let (keys, input) = canter_tui::input();
        let frames = Frames::default();
        let terminal = ratatui::Terminal::new(Screen {
            backend: Arc::new(Mutex::new(TestBackend::new(WIDTH, HEIGHT))),
            keys: keys.clone(),
            frames: frames.clone(),
        })
        .unwrap();
        let task = tokio::spawn(run_on(terminal, input, launch));
        Self { keys, frames, task }
    }

    /// Sends `key` while the first frame showing `text` is drawn, before that
    /// frame is on screen. Returns whether it has been sent.
    fn press_while_drawing(&self, text: &str, key: KeyCode) -> Arc<AtomicBool> {
        let sent = Arc::new(AtomicBool::new(false));
        *self.frames.trap.lock().unwrap() = Some(Trap {
            text: text.to_owned(),
            key: TermEvent::Key(KeyEvent::new(key, KeyModifiers::NONE)),
            sent: sent.clone(),
        });
        sent
    }

    /// Waits until the client has put `count` more frames on screen.
    async fn frames_later(&self, count: u64) {
        let target = self.frames.done.load(Ordering::SeqCst) + count;
        self.wait_until("frames", |terminal| {
            terminal.frames.done.load(Ordering::SeqCst) >= target
        })
        .await;
    }

    async fn press(&self, code: KeyCode) {
        self.press_with(code, KeyModifiers::NONE).await;
    }

    /// Like a user, presses a key on what the test has seen: first waits
    /// until the client counts the frames the test saw as on screen.
    async fn press_with(&self, code: KeyCode, modifiers: KeyModifiers) {
        let seen = self.frames.done.load(Ordering::SeqCst);
        self.wait_until("the frame to be on screen", |terminal| {
            terminal.keys.frames() >= seen
        })
        .await;
        self.keys
            .send(TermEvent::Key(KeyEvent::new(code, modifiers)));
    }

    async fn type_line(&self, text: &str) {
        for c in text.chars() {
            self.press(KeyCode::Char(c)).await;
        }
        self.press(KeyCode::Enter).await;
    }

    /// The screen as of the last frame on screen.
    fn screen(&self) -> String {
        self.frames.visible.lock().unwrap().clone()
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
            panic!("waited for {what}; the terminal shows:\n{}", self.screen());
        }
    }

    async fn wait_for(&self, text: &str) {
        self.wait_until(text, |terminal| terminal.screen().contains(text))
            .await;
    }

    /// Ctrl+C on an empty input quits; returns the session id the client had.
    async fn quit(self) -> String {
        self.press_with(KeyCode::Char('c'), KeyModifiers::CONTROL)
            .await;
        self.finished().await
    }

    /// Waits for the client to stop; returns the session id it had, the one
    /// `canter` prints how to resume.
    async fn finished(self) -> String {
        tokio::time::timeout(WAIT, self.task)
            .await
            .expect("client quits")
            .unwrap()
            .unwrap()
            .expect("a session was open")
    }

    /// The bottom row: model, permission mode and directory.
    fn info(&self) -> String {
        self.screen()
            .lines()
            .last()
            .unwrap_or_default()
            .trim()
            .to_owned()
    }
}

/// The client draws on a TestBackend. Like a terminal with synchronized
/// output, a frame is on screen only once `end_frame` is done; the test sees
/// only frames on screen.
struct Screen {
    backend: Arc<Mutex<TestBackend>>,
    keys: InputSender,
    frames: Frames,
}

/// What is on screen, how many frames the client finished, and a key to send
/// while a frame is drawn.
#[derive(Clone, Default)]
struct Frames {
    visible: Arc<Mutex<String>>,
    done: Arc<AtomicU64>,
    trap: Arc<Mutex<Option<Trap>>>,
}

/// Send `key` while the frame showing `text` is drawn but not yet on screen.
struct Trap {
    text: String,
    key: TermEvent,
    sent: Arc<AtomicBool>,
}

impl FrameSync for Screen {
    fn end_frame(&mut self) -> Result<(), Self::Error> {
        let screen = rows(self.backend.lock().unwrap().buffer());
        let mut trap = self.frames.trap.lock().unwrap();
        if trap
            .as_ref()
            .is_some_and(|trap| screen.contains(&trap.text))
        {
            let trap = trap.take().unwrap();
            self.keys.send(trap.key);
            trap.sent.store(true, Ordering::SeqCst);
        }
        drop(trap);
        *self.frames.visible.lock().unwrap() = screen;
        self.frames.done.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl Backend for Screen {
    type Error = std::convert::Infallible;

    /// Like a terminal, a wide character also covers the cell after it.
    /// TestBackend alone would keep what was there, and ratatui, which
    /// counts on the terminal clearing it, never redraws that cell.
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let mut backend = self.backend.lock().unwrap();
        let width = backend.size()?.width;
        let blank = Cell::default();
        for (x, y, cell) in content {
            backend.draw(std::iter::once((x, y, cell)))?;
            if cell.symbol().width() > 1 && x + 1 < width {
                backend.draw(std::iter::once((x + 1, y, &blank)))?;
            }
        }
        Ok(())
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.backend.lock().unwrap().hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.backend.lock().unwrap().show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.backend.lock().unwrap().get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.backend.lock().unwrap().set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.backend.lock().unwrap().clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.backend.lock().unwrap().clear_region(clear_type)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        self.backend.lock().unwrap().size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.backend.lock().unwrap().window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.backend.lock().unwrap().flush()
    }
}

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

/// Like a user who reads an approval box before answering it: waits until
/// the box takes answers, ANSWER_GUARD_MS after it appeared.
async fn read_the_box() {
    tokio::time::sleep(Duration::from_millis(ANSWER_GUARD_MS as u64 + 200)).await;
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
        .wait_for("fake-model · Always ask · /tmp/canter-tui-")
        .await;
    assert!(terminal.screen().contains("somewhere/else/deep"));
    let registration: Value =
        serde_json::from_slice(&fs::read(world.path("state/server.json")).unwrap()).unwrap();
    let daemon = Pid::from_raw(registration["pid"].as_u64().unwrap() as i32);
    assert_ne!(daemon.as_raw() as u32, std::process::id());

    terminal.type_line("把环境写出来").await;
    terminal.wait_for("Run this command?").await;
    assert!(terminal.screen().contains(WRITE_ENV), "the whole command");
    read_the_box().await;
    terminal.press(KeyCode::Enter).await;
    terminal.wait_for("写好了").await;
    assert!(terminal.screen().contains("You chose: Allow once"));
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
    assert!(b.screen().contains("› 第一句  (from canter-tui a)"));

    b.type_line("第二句").await;
    for terminal in [&a, &b] {
        terminal.wait_for("回答二").await;
    }
    assert!(a.screen().contains("› 第二句  (from canter-tui b)"));
    assert!(
        b.screen().contains("› 第二句\n"),
        "own messages are not marked"
    );
    assert!(a.screen().contains("› 第一句\n"));
    // Apart from the marks, both show the same conversation.
    let conversation = |terminal: &Terminal| {
        let text = terminal
            .screen()
            .replace("  (from canter-tui a)", "")
            .replace("  (from canter-tui b)", "");
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
    reconnect_after_restart(false).await;
}

#[tokio::test]
async fn client_reconnects_to_a_legacy_session_without_a_close_reason() {
    reconnect_after_restart(true).await;
}

async fn reconnect_after_restart(legacy: bool) {
    let mut world = World::new(vec![text_reply("回答一"), text_reply("回答二")]).await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.type_line("第一句").await;
    a.wait_for("回答一").await;
    let id = world.only_session().await;

    world.stop_server(Signal::SIGTERM).await;
    a.wait_until("Reconnecting", |a| a.status().contains("Reconnecting"))
        .await;
    let path = world.path(&format!("state/sessions/{id}/events.jsonl"));
    let mut events: Vec<Value> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let closed = events.last().unwrap();
    assert_eq!(closed["type"], "session.status");
    assert_eq!(closed["status"], "closed");
    assert_eq!(closed["closeReason"], "serverShutdown");
    if legacy {
        // Reproduce a pre-closeReason event log while its server is stopped.
        let lines = events
            .iter_mut()
            .map(|event| {
                event.as_object_mut().unwrap().remove("closeReason");
                serde_json::to_string(event).unwrap()
            })
            .collect::<Vec<_>>();
        fs::write(path, lines.join("\n") + "\n").unwrap();
    }
    world.start_server().await;
    a.wait_until("reconnected", |a| !a.status().contains("Reconnecting"))
        .await;
    a.type_line("第二句").await;
    a.wait_for("回答二").await;
    a.wait_idle().await;
    let snapshot = world.snapshot(&id).await;
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert_eq!(snapshot.close_reason, None);
    assert_eq!(snapshot.turns.len(), 2);

    // A fresh terminal on the session shows exactly what A shows.
    let fresh = world.open(&["--resume", &id], "canter-tui a").await;
    fresh.wait_for("回答二").await;
    fresh.wait_idle().await;
    assert_eq!(a.screen().trim_end(), fresh.screen().trim_end());
    assert!(!a.screen().contains("does not fit"));

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
    b.wait_for("Which session to resume?").await;
    b.press(KeyCode::Enter).await;
    b.wait_idle().await;

    a.type_line("跑一下").await;
    for terminal in [&a, &b] {
        terminal.wait_for("Run this command?").await;
    }
    // A allows (option 1), B declines (key 3), at the same moment.
    read_the_box().await;
    a.press(KeyCode::Enter).await;
    b.press(KeyCode::Char('3')).await;
    for terminal in [&a, &b] {
        terminal
            .wait_until("the box to close", |t| {
                !t.screen().contains("Run this command?")
            })
            .await;
        terminal.wait_for("做完了").await;
    }

    let id = world.only_session().await;
    assert!(world.snapshot(&id).await.pending_requests.is_empty());
    let (winner, loser, label) = if a.screen().contains("You chose: Allow once") {
        (&a, &b, "canter-tui a answered: Allow once")
    } else {
        (&b, &a, "canter-tui b answered: Decline")
    };
    assert!(
        loser.screen().contains(label),
        "the later terminal names who answered:\n{}",
        loser.screen()
    );
    assert!(
        loser
            .screen()
            .contains("Your answer came too late and was ignored")
    );
    assert!(!winner.screen().contains("too late"));
    assert!(!winner.screen().contains(" answered: "));

    a.quit().await;
    b.quit().await;
    world.stop_server(Signal::SIGTERM).await;
}

/// The box shows exactly what will run: a carriage return in the command does
/// not hide what comes before it, it shows as `\r`.
#[tokio::test]
async fn approval_box_shows_control_characters_in_the_command() {
    let hidden = "printf review-proof > approval-proof.txt #\recho safe";
    let mut world = World::new(vec![shell_reply(hidden), text_reply("跑完了")]).await;
    world.start_server().await;
    let a = world.open(&["跑一下"], "canter-tui a").await;
    a.wait_for("Run this command?").await;
    assert!(
        a.screen()
            .contains(r"printf review-proof > approval-proof.txt #\recho safe"),
        "the whole command:\n{}",
        a.screen()
    );

    read_the_box().await;
    a.press(KeyCode::Enter).await;
    a.wait_for("跑完了").await;
    assert_eq!(
        fs::read_to_string(world.path("work/approval-proof.txt")).unwrap(),
        "review-proof",
        "what ran is what the box showed"
    );
    a.quit().await;
    world.stop_server(Signal::SIGTERM).await;
}

/// Another client closes the session on the same server. The terminal
/// watching it does not open it again and says it is closed; opening it on
/// purpose with `--resume` still works, and the watching terminal follows.
#[tokio::test]
async fn a_session_closed_by_another_client_stays_closed() {
    let mut world = World::new(vec![text_reply("回答一"), text_reply("回答二")]).await;
    world.start_server().await;
    let a = world.open(&["第一句"], "canter-tui a").await;
    a.wait_for("回答一").await;
    a.wait_idle().await;
    let id = world.only_session().await;

    world.close_session(&id).await;
    // Long enough for the watching terminal to reconnect a few times over.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        world.snapshot(&id).await.status,
        SessionStatus::Closed,
        "the watching terminal opened the session again"
    );
    a.wait_until("Session closed", |a| a.status().contains("Session closed"))
        .await;

    let b = world.open(&["--resume", &id], "canter-tui b").await;
    b.wait_idle().await;
    a.wait_idle().await;
    b.type_line("第二句").await;
    a.wait_for("回答二").await;
    assert!(a.screen().contains("› 第二句  (from canter-tui b)"));

    assert_eq!(a.quit().await, id);
    assert_eq!(b.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}

/// A watching terminal must not undo an explicit close when the server restarts.
#[tokio::test]
async fn a_session_closed_by_another_client_stays_closed_after_restart() {
    let mut world = World::new(vec![text_reply("回答一"), text_reply("回答二")]).await;
    world.start_server().await;
    let a = world.open(&["第一句"], "canter-tui a").await;
    a.wait_for("回答一").await;
    a.wait_idle().await;
    let id = world.only_session().await;

    world.close_session(&id).await;
    assert_eq!(
        world.snapshot(&id).await.close_reason,
        Some(SessionCloseReason::ClientRequest)
    );
    a.wait_until("Session closed", |a| a.status().contains("Session closed"))
        .await;
    world.stop_server(Signal::SIGTERM).await;
    a.wait_until("Reconnecting", |a| a.status().contains("Reconnecting"))
        .await;
    world.start_server().await;
    a.wait_until("reconnected", |a| !a.status().contains("Reconnecting"))
        .await;
    assert_eq!(
        world.snapshot(&id).await.status,
        SessionStatus::Closed,
        "the watching terminal reopened a session closed by another client"
    );
    assert_eq!(
        world.snapshot(&id).await.close_reason,
        Some(SessionCloseReason::ClientRequest),
        "the reason survives replay and the later server shutdown"
    );
    a.wait_until("Session closed", |a| a.status().contains("Session closed"))
        .await;

    let b = world.open(&["--resume", &id], "canter-tui b").await;
    b.wait_idle().await;
    a.wait_idle().await;
    assert_eq!(world.snapshot(&id).await.close_reason, None);
    b.type_line("第二句").await;
    a.wait_for("回答二").await;
    assert_eq!(a.quit().await, id);
    assert_eq!(b.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}

/// Enter comes in while the approval box's first frame is drawn but not yet
/// on screen: it was pressed on the input box still showing, so it does not
/// answer the approval. Enter pressed once the box is on screen does.
#[tokio::test]
async fn a_key_that_comes_in_while_the_box_is_drawn_does_not_answer_it() {
    let mut world = World::new(vec![
        shell_reply("printf B > second-proof.txt"),
        text_reply("跑完了"),
    ])
    .await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.wait_idle().await;
    let sent = a.press_while_drawing("Run this command?", KeyCode::Enter);
    a.type_line("跑一下").await;
    a.wait_until("Enter sent mid-frame", |_| sent.load(Ordering::SeqCst))
        .await;
    a.frames_later(2).await;
    // Give a wrong answer every chance to land.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let id = world.only_session().await;
    assert_eq!(
        world.snapshot(&id).await.pending_requests.len(),
        1,
        "the Enter answered a box that was not on screen yet"
    );
    assert!(!world.path("work/second-proof.txt").exists());

    a.press(KeyCode::Enter).await;
    a.wait_for("跑完了").await;
    assert_eq!(
        fs::read_to_string(world.path("work/second-proof.txt")).unwrap(),
        "B"
    );
    a.quit().await;
    world.stop_server(Signal::SIGTERM).await;
}

/// The user's decision after review round 2: a terminal can still show the
/// input box for a moment after the client has sent the approval box, so an
/// Enter that comes in just after the box's frame was sent may not be meant
/// for it. For a short guard after the box appears, answer keys are dropped,
/// not kept for later; Enter pressed after that answers it.
#[tokio::test]
async fn enter_right_after_the_box_appears_does_not_answer_it() {
    let mut world = World::new(vec![
        shell_reply("printf B > second-proof.txt"),
        text_reply("跑完了"),
    ])
    .await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.wait_idle().await;
    a.type_line("跑一下").await;
    a.wait_for("Run this command?").await;
    // Comes in after the box's frame, so the frame check lets it through.
    a.press(KeyCode::Enter).await;
    // Give a wrong answer every chance to land, also after the guard.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let id = world.only_session().await;
    assert_eq!(
        world.snapshot(&id).await.pending_requests.len(),
        1,
        "the Enter right after the box appeared answered it"
    );
    assert!(!world.path("work/second-proof.txt").exists());
    assert!(a.screen().contains("Run this command?"));

    a.press(KeyCode::Enter).await;
    a.wait_for("跑完了").await;
    assert_eq!(
        fs::read_to_string(world.path("work/second-proof.txt")).unwrap(),
        "B"
    );
    a.quit().await;
    world.stop_server(Signal::SIGTERM).await;
}

/// The other way round: terminal A answers the box, and terminal B, which
/// has a draft in its input box, gets the input box back. An Enter B presses
/// right then was meant for the box, so it does not send the draft; Enter
/// pressed after the guard does.
#[tokio::test]
async fn enter_right_after_another_terminal_answers_does_not_send_the_draft() {
    let mut world = World::new(vec![
        shell_reply("printf A > first-proof.txt"),
        text_reply("跑完了"),
        text_reply("收到草稿"),
    ])
    .await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.wait_idle().await;
    let b = world.open(&["--resume"], "canter-tui b").await;
    b.wait_for("Which session to resume?").await;
    b.press(KeyCode::Enter).await;
    b.wait_idle().await;
    for c in "草稿".chars() {
        b.press(KeyCode::Char(c)).await;
    }
    b.wait_until("the draft", |b| b.screen().contains("› 草稿"))
        .await;

    a.type_line("跑一下").await;
    for terminal in [&a, &b] {
        terminal.wait_for("Run this command?").await;
    }
    read_the_box().await;
    a.press(KeyCode::Enter).await;
    b.wait_until("the input box", |b| b.screen().contains("› 草稿"))
        .await;
    // Comes in after the input box's frame, so the frame check lets it through.
    b.press(KeyCode::Enter).await;
    a.wait_for("跑完了").await;
    // Give a wrong send every chance to land, also after the guard.
    tokio::time::sleep(Duration::from_secs(1)).await;
    let id = world.only_session().await;
    let snapshot = world.snapshot(&id).await;
    assert_eq!(
        (snapshot.turns.len(), snapshot.queued_inputs.len()),
        (1, 0),
        "the Enter right after the box closed sent the draft"
    );
    assert!(b.screen().contains("› 草稿"));

    b.press(KeyCode::Enter).await;
    b.wait_for("收到草稿").await;
    a.wait_for("收到草稿").await;
    assert_eq!(world.snapshot(&id).await.turns.len(), 2);
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
        shell_reply("echo never > approval-proof.txt"),
        shell_reply(WRITE_ENV),
        text_reply("跑完了"),
    ])
    .await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.type_line("跑一下").await;
    a.wait_for("Run this command?").await;
    assert!(a.status().contains("Waiting for approval"));

    world.stop_server(Signal::SIGKILL).await;
    a.wait_until("Reconnecting", |a| a.status().contains("Reconnecting"))
        .await;
    world.start_server().await;
    a.wait_until("the box to close", |a| {
        !a.screen().contains("Run this command?") && !a.status().contains("Reconnecting")
    })
    .await;
    a.wait_for("Approval request cancelled").await;
    a.wait_for("Turn interrupted").await;
    assert_eq!(a.status(), "", "idle");

    let id = world.only_session().await;
    let snapshot = world.snapshot(&id).await;
    assert!(snapshot.pending_requests.is_empty());
    assert_eq!(snapshot.status, SessionStatus::Idle);
    assert_eq!(snapshot.turns[0].status, TurnStatus::Interrupted);

    assert!(!world.path("work/approval-proof.txt").exists());

    // The input box came back when the box closed: Enter sends once the
    // guard is over, like a user who sees the box gone before typing on.
    read_the_box().await;
    a.type_line("再跑一下").await;
    a.wait_for("Run this command?").await;
    read_the_box().await;
    a.press(KeyCode::Char('1')).await;
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

/// `/exit` quits like Ctrl+D on an empty input: the session stays on the
/// server, and `canter` prints how to resume it.
#[tokio::test]
async fn slash_exit_quits_and_keeps_the_session() {
    let mut world = World::new(vec![text_reply("回答一")]).await;
    world.start_server().await;
    let a = world.open(&["第一句"], "canter-tui a").await;
    a.wait_for("回答一").await;
    a.wait_idle().await;
    let id = world.only_session().await;

    a.type_line("/exit").await;
    assert_eq!(a.finished().await, id);
    assert_eq!(
        world.snapshot(&id).await.status,
        SessionStatus::Idle,
        "the session stays on the server"
    );
    world.stop_server(Signal::SIGTERM).await;
}

/// `/new` opens a new session in the same terminal, with the directory,
/// model and permission mode of the one before. The old session stays open;
/// the screen shows the new one, and quitting names it.
#[tokio::test]
async fn slash_new_opens_a_new_session_in_this_terminal() {
    let mut world = World::new(vec![text_reply("回答一"), text_reply("回答二")]).await;
    world.start_server().await;
    let a = world
        .open(&["--yolo", "-m", "other-model", "第一句"], "canter-tui a")
        .await;
    a.wait_for("回答一").await;
    a.wait_idle().await;
    let old = world.only_session().await;

    a.type_line("/new").await;
    a.wait_until("the new session", |a| !a.screen().contains("第一句"))
        .await;
    assert!(
        a.info()
            .starts_with("other-model · Allow all · /tmp/canter-tui-"),
        "{}",
        a.screen()
    );
    let client = Client::new(&world.path("state/server.sock")).unwrap();
    let sessions = client.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 2);
    let new = sessions.iter().find(|s| s.id != old).unwrap().id.clone();
    let settings = world.snapshot(&new).await.settings;
    assert_eq!(settings.cwd, world.path("work").display().to_string());
    assert_eq!(settings.model, "other-model");
    assert_eq!(settings.approval_policy, PermissionMode::AllowAll);
    assert_eq!(
        world.snapshot(&old).await.status,
        SessionStatus::Idle,
        "the old session stays open"
    );

    a.type_line("第二句").await;
    a.wait_for("回答二").await;
    assert_eq!(world.snapshot(&new).await.turns.len(), 1);
    assert_eq!(world.snapshot(&old).await.turns.len(), 1);
    assert_eq!(a.quit().await, new, "quitting names the session on screen");
    world.stop_server(Signal::SIGTERM).await;
}

/// `/resume` shows the session list; the session picked replaces the one on
/// screen, and the conversation goes on there.
#[tokio::test]
async fn slash_resume_switches_to_the_session_picked() {
    let mut world = World::new(vec![
        text_reply("回答一"),
        text_reply("回答二"),
        text_reply("回答三"),
    ])
    .await;
    world.start_server().await;
    let b = world.open(&["第一句"], "canter-tui b").await;
    b.wait_for("回答一").await;
    b.wait_idle().await;
    let first = world.only_session().await;
    b.quit().await;
    let a = world.open(&["第二句"], "canter-tui a").await;
    a.wait_for("回答二").await;
    a.wait_idle().await;

    a.type_line("/resume").await;
    a.wait_for("Which session to resume?").await;
    // Most recently updated first: the session on screen, then the first one.
    a.press(KeyCode::Down).await;
    a.press(KeyCode::Enter).await;
    a.wait_for("回答一").await;
    a.wait_idle().await;
    assert!(
        !a.screen().contains("第二句"),
        "only the session switched to:\n{}",
        a.screen()
    );

    a.type_line("第三句").await;
    a.wait_for("回答三").await;
    assert_eq!(world.snapshot(&first).await.turns.len(), 2);
    assert_eq!(
        a.quit().await,
        first,
        "quitting names the session on screen"
    );
    world.stop_server(Signal::SIGTERM).await;
}

/// Esc on the `/resume` list goes back to the session on screen.
#[tokio::test]
async fn slash_resume_esc_goes_back_to_the_same_session() {
    let mut world = World::new(vec![text_reply("回答一"), text_reply("回答二")]).await;
    world.start_server().await;
    let a = world.open(&["第一句"], "canter-tui a").await;
    a.wait_for("回答一").await;
    a.wait_idle().await;
    let id = world.only_session().await;

    a.type_line("/resume").await;
    a.wait_for("Which session to resume?").await;
    a.press(KeyCode::Esc).await;
    a.wait_until("the conversation again", |a| {
        !a.screen().contains("Which session to resume?") && a.screen().contains("回答一")
    })
    .await;
    a.type_line("第二句").await;
    a.wait_for("回答二").await;
    assert_eq!(world.snapshot(&id).await.turns.len(), 2);
    assert_eq!(a.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}

/// `/compact` compacts the session; the screen shows it the way it shows
/// any compaction.
#[tokio::test]
async fn slash_compact_compacts_the_session() {
    let mut world = World::new(vec![
        text_reply("回答一"),
        text_reply("SUMMARY-OF-THE-CHAT"),
    ])
    .await;
    world.start_server().await;
    let a = world.open(&["第一句"], "canter-tui a").await;
    a.wait_for("回答一").await;
    a.wait_idle().await;
    let id = world.only_session().await;

    a.type_line("/compact").await;
    a.wait_for("Context compacted").await;
    a.wait_idle().await;
    let snapshot = world.snapshot(&id).await;
    assert_eq!(snapshot.turns.len(), 2);
    assert_eq!(snapshot.turns[1].status, TurnStatus::Completed);
    assert!(
        snapshot.turns[1].items.iter().any(|item| matches!(
            item,
            Item::ContextCompaction { summary, .. } if summary == "SUMMARY-OF-THE-CHAT"
        )),
        "{:?}",
        snapshot.turns[1]
    );
    assert_eq!(a.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}

/// The server refuses to compact while the model is answering: the screen
/// shows its error as a warning and carries on.
#[tokio::test]
async fn slash_compact_during_an_answer_shows_the_server_error() {
    let mut world = World::new(vec![shell_reply("sleep 5"), text_reply("睡好了")]).await;
    world.start_server().await;
    let a = world.open(&["--yolo", "睡一会"], "canter-tui a").await;
    a.wait_until("the command running", |a| {
        a.screen().contains("sleep 5") && a.status().contains("Working")
    })
    .await;

    a.type_line("/compact").await;
    a.wait_for("cannot compact while a turn is active").await;
    a.wait_for("睡好了").await;
    a.wait_idle().await;
    let id = world.only_session().await;
    assert_eq!(
        world.snapshot(&id).await.turns.len(),
        1,
        "nothing compacted"
    );
    assert_eq!(a.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}

/// `/mode <mode>` changes the permission mode, `/mode` alone offers the three
/// modes, and an unknown name changes nothing. The info line follows.
#[tokio::test]
async fn slash_mode_changes_the_permission_mode() {
    let mut world = World::new(Vec::new()).await;
    world.start_server().await;
    let a = world.open(&[], "canter-tui a").await;
    a.wait_idle().await;
    assert!(
        a.info().starts_with("fake-model · Always ask"),
        "{}",
        a.info()
    );
    let id = world.only_session().await;
    let mode = async || world.snapshot(&id).await.settings.approval_policy;

    a.type_line("/mode allow-all").await;
    a.wait_until("Allow all", |a| {
        a.info().starts_with("fake-model · Allow all")
    })
    .await;
    assert_eq!(mode().await, PermissionMode::AllowAll);

    a.type_line("/mode").await;
    a.wait_for("(current)").await;
    a.press(KeyCode::Up).await;
    a.press(KeyCode::Up).await;
    a.press(KeyCode::Enter).await;
    a.wait_until("Always ask", |a| {
        a.info().starts_with("fake-model · Always ask")
    })
    .await;
    assert_eq!(mode().await, PermissionMode::AlwaysAsk);

    a.type_line("/mode yolo").await;
    a.wait_for("Unknown mode: yolo").await;
    assert_eq!(mode().await, PermissionMode::AlwaysAsk);
    assert!(a.info().starts_with("fake-model · Always ask"));
    // The first Ctrl+C clears the draft left in the input box.
    a.press_with(KeyCode::Char('c'), KeyModifiers::CONTROL)
        .await;
    assert_eq!(a.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}

/// The user's decision: `/` and Enter, or `/co` and Enter, only complete the
/// input to `/compact`; nothing is compacted and the model gets no request.
/// Enter on the whole name compacts.
#[tokio::test]
async fn slash_and_enter_only_completes_compact() {
    let mut world = World::new(vec![
        text_reply("回答一"),
        text_reply("SUMMARY-OF-THE-CHAT"),
    ])
    .await;
    world.start_server().await;
    let a = world.open(&["第一句"], "canter-tui a").await;
    a.wait_for("回答一").await;
    a.wait_idle().await;
    let id = world.only_session().await;
    let turns = async || world.snapshot(&id).await.turns.len();

    a.type_line("/").await;
    a.wait_for("│ › /compact").await;
    // Give a wrong compaction every chance to start.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(turns().await, 1, "/ and Enter compacted");
    a.press_with(KeyCode::Char('c'), KeyModifiers::CONTROL)
        .await;
    a.wait_for("Type a message").await;

    a.type_line("/co").await;
    a.wait_for("│ › /compact").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(turns().await, 1, "/co and Enter compacted");

    a.press(KeyCode::Enter).await;
    a.wait_for("Context compacted").await;
    a.wait_idle().await;
    assert_eq!(turns().await, 2);
    assert_eq!(a.quit().await, id);
    world.stop_server(Signal::SIGTERM).await;
}
