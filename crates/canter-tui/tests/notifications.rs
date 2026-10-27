//! Notifications as `run_on` writes them (P2-27): when a turn is done while
//! the window is not in front, an OSC 9 notification in Ghostty itself and a
//! bell elsewhere; nothing while it is in front. Focus comes and goes as fake
//! terminal events; the fake server answers each message with a finished
//! turn. The environment is the test's own, never the terminal running the
//! tests.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use canter_protocol::SessionUpdateParams;
use canter_tui::client::Client;
use canter_tui::fake::{FakeServer, Pace, parse_script};
use canter_tui::{Connected, FrameSync, Launch, Open, SocketConnector, input, run_on};
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

const WAIT: Duration = Duration::from_secs(10);

const GHOSTTY: [(&str, &str); 2] = [("TERM_PROGRAM", "ghostty"), ("TERM", "xterm-ghostty")];
const ANSWER: &str = "This is a fake server; it only replays scripted events. Received: hi";

#[tokio::test]
async fn ghostty_gets_an_osc_9_with_the_answer_when_the_window_is_behind() {
    let canter = Canter::start(&GHOSTTY, None).await;
    canter.keys.send(TermEvent::FocusLost);
    canter.say("hi").await;
    canter.wait_for(&format!("\x1b]9;{ANSWER}\x07")).await;
    assert!(!canter.bells(), "{:?}", canter.written());
    canter.quit().await;
}

#[tokio::test]
async fn other_terminals_ring_the_bell() {
    let canter = Canter::start(&[("TERM_PROGRAM", "Apple_Terminal")], None).await;
    canter.keys.send(TermEvent::FocusLost);
    canter.say("hi").await;
    canter.wait_for_bell().await;
    assert!(
        !canter.written().contains("\x1b]9;"),
        "{:?}",
        canter.written()
    );
    canter.quit().await;
}

#[tokio::test]
async fn nothing_while_the_window_is_in_front() {
    let canter = Canter::start(&GHOSTTY, None).await;
    canter.keys.send(TermEvent::FocusLost);
    canter.keys.send(TermEvent::FocusGained);
    canter.say("hi").await;
    canter.wait_for_answer().await;
    assert!(!canter.notified(), "{:?}", canter.written());
    canter.quit().await;
}

#[tokio::test]
async fn nothing_when_the_terminal_never_reports_focus() {
    let canter = Canter::start(&GHOSTTY, None).await;
    canter.say("hi").await;
    canter.wait_for_answer().await;
    assert!(!canter.notified(), "{:?}", canter.written());
    canter.quit().await;
}

#[tokio::test]
async fn always_notifies_even_in_front() {
    let settings = r#"{"notificationCondition":"always"}"#;
    let canter = Canter::start(&GHOSTTY, Some(settings)).await;
    canter.say("hi").await;
    canter.wait_for(&format!("\x1b]9;{ANSWER}\x07")).await;
    canter.quit().await;
}

#[tokio::test]
async fn the_bell_when_asked_for_even_in_ghostty() {
    let settings = r#"{"notificationMethod":"bel"}"#;
    let canter = Canter::start(&GHOSTTY, Some(settings)).await;
    canter.keys.send(TermEvent::FocusLost);
    canter.say("hi").await;
    canter.wait_for_bell().await;
    assert!(!canter.notified_osc9(), "{:?}", canter.written());
    canter.quit().await;
}

#[tokio::test]
async fn turned_off_or_without_turn_complete_nothing() {
    for settings in [
        r#"{"notifications":false}"#,
        r#"{"notifications":["approval-requested","question"]}"#,
    ] {
        let canter = Canter::start(&GHOSTTY, Some(settings)).await;
        canter.keys.send(TermEvent::FocusLost);
        canter.say("hi").await;
        canter.wait_for_answer().await;
        assert!(!canter.notified(), "{settings}: {:?}", canter.written());
        canter.quit().await;
    }
}

#[tokio::test]
async fn a_value_not_known_is_said_once_and_the_default_used() {
    let settings = r#"{"notificationMethod":"popup"}"#;
    let canter = Canter::start(&GHOSTTY, Some(settings)).await;
    canter.keys.send(TermEvent::FocusLost);
    canter.say("hi").await;
    canter.wait_for(&format!("\x1b]9;{ANSWER}\x07")).await;
    let shown = canter.screen();
    assert_eq!(
        shown.matches("notificationMethod").count(),
        1,
        "said once: {shown}"
    );
    canter.quit().await;
}

struct Canter {
    keys: canter_tui::InputSender,
    written: Arc<Mutex<Vec<String>>>,
    screen: Arc<Mutex<String>>,
    task: tokio::task::JoinHandle<Result<Option<String>, canter_tui::RunError>>,
    _server: FakeServer,
    _dir: tempfile::TempDir,
}

impl Canter {
    /// canter in the terminal `term`, on an idle session in
    /// `<temp>/project`, with `tui` as `tui.json`.
    async fn start(term: &[(&str, &str)], tui: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let cwd = project.to_str().unwrap();
        let socket = dir.path().join("canter.sock");
        let script = format!(
            "{}\n{}\n",
            serde_json::json!({
                "seq": 1, "sessionId": "s1", "timestamp": 0, "type": "session.updated",
                "settings": { "cwd": cwd, "name": null, "model": "m", "approvalPolicy": "alwaysAsk" },
            }),
            serde_json::json!({
                "seq": 2, "sessionId": "s1", "timestamp": 0,
                "type": "session.status", "status": "idle",
            }),
        );
        let server = FakeServer::start(&socket, parse_script(&script).unwrap(), 2, Pace::Instant)
            .await
            .unwrap();
        let home = dir.path().to_str().unwrap().to_owned();
        let canter_home = format!("{home}/.canter");
        if let Some(tui) = tui {
            std::fs::create_dir(&canter_home).unwrap();
            std::fs::write(format!("{canter_home}/tui.json"), tui).unwrap();
        }
        let mut env = BTreeMap::from([
            ("HOME".to_owned(), home.clone()),
            ("CANTER_HOME".to_owned(), canter_home),
        ]);
        for (key, value) in term {
            env.insert((*key).to_owned(), (*value).to_owned());
        }
        let launch = Launch {
            connector: SocketConnector(socket.clone()),
            connected: Connected {
                client: Client::new(&socket).unwrap(),
                warning: None,
                server: None,
            },
            open: Open::Resume {
                id: Some("s1".to_owned()),
                cwd: cwd.to_owned(),
                update: SessionUpdateParams::default(),
            },
            env,
            client_name: "canter-tui test".to_owned(),
            home: Some(home),
            version: "test".to_owned(),
        };
        let written = Arc::new(Mutex::new(Vec::new()));
        let screen = Arc::new(Mutex::new(String::new()));
        let terminal = ratatui::Terminal::new(Screen {
            backend: TestBackend::new(100, 30),
            written: written.clone(),
            screen: screen.clone(),
        })
        .unwrap();
        let (keys, input) = input();
        let task = tokio::spawn(run_on(terminal, input, launch));
        let canter = Self {
            keys,
            written,
            screen,
            task,
            _server: server,
            _dir: dir,
        };
        canter.wait_for("canter · project\x07").await;
        canter
    }

    fn written(&self) -> String {
        self.written.lock().unwrap().concat()
    }

    fn screen(&self) -> String {
        self.screen.lock().unwrap().clone()
    }

    /// Writes not part of an OSC sequence: a bell on its own.
    fn bells(&self) -> bool {
        self.written
            .lock()
            .unwrap()
            .iter()
            .any(|sequence| sequence == "\x07")
    }

    fn notified_osc9(&self) -> bool {
        self.written
            .lock()
            .unwrap()
            .iter()
            .any(|sequence| sequence.starts_with("\x1b]9;") && !sequence.starts_with("\x1b]9;4;"))
    }

    fn notified(&self) -> bool {
        self.bells() || self.notified_osc9()
    }

    async fn say(&self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
        self.key(KeyCode::Enter);
    }

    fn key(&self, code: KeyCode) {
        self.keys
            .send(TermEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
    }

    async fn wait_until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        let found = tokio::time::timeout(WAIT, async {
            while !done(self) {
                assert!(!self.task.is_finished(), "canter stopped");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(found.is_ok(), "no {what}; written: {:?}", self.written());
    }

    async fn wait_for(&self, sequence: &str) {
        self.wait_until(sequence, |canter| canter.written().contains(sequence))
            .await;
    }

    async fn wait_for_bell(&self) {
        self.wait_until("bell", Self::bells).await;
    }

    /// The finished turn's mark in the title: everything for it is written.
    async fn wait_for_answer(&self) {
        self.wait_for("\x1b]0;✓ canter · project\x07").await;
        // One more round of the loop, where a notification would be written.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    async fn quit(self) {
        self.keys.send(TermEvent::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        let result = tokio::time::timeout(WAIT, self.task)
            .await
            .expect("canter quits")
            .unwrap();
        assert_eq!(result.unwrap().as_deref(), Some("s1"));
    }
}

/// A test backend that keeps what canter writes between frames, and the
/// last frame's text.
struct Screen {
    backend: TestBackend,
    written: Arc<Mutex<Vec<String>>>,
    screen: Arc<Mutex<String>>,
}

impl FrameSync for Screen {
    fn write_sequence(&mut self, sequence: &str) -> Result<(), Self::Error> {
        self.written.lock().unwrap().push(sequence.to_owned());
        Ok(())
    }
    fn end_frame(&mut self) -> Result<(), Self::Error> {
        let buffer = self.backend.buffer();
        let text = buffer
            .content
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n");
        *self.screen.lock().unwrap() = text;
        Ok(())
    }
}

impl Backend for Screen {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.backend.draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.backend.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.backend.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.backend.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.backend.set_cursor_position(position)
    }
    fn clear(&mut self) -> Result<(), Self::Error> {
        self.backend.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.backend.clear_region(clear_type)
    }
    fn size(&self) -> Result<Size, Self::Error> {
        self.backend.size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.backend.window_size()
    }
    fn flush(&mut self) -> Result<(), Self::Error> {
        self.backend.flush()
    }
}
