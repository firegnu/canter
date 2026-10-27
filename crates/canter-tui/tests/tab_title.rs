//! The terminal tab's title as `run_on` writes it: the session's title while
//! it runs, then an empty title and no tab mark once canter quits, by Ctrl+C
//! or on SIGTERM. The fake server serves the session; the environment is the
//! test's own, never the terminal running the tests.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use canter_protocol::SessionUpdateParams;
use canter_tui::client::Client;
use canter_tui::fake::{FakeServer, Pace, parse_script};
use canter_tui::{Connected, FrameSync, Launch, Open, SocketConnector, input, run_on};
use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};
use nix::sys::signal::{Signal, raise};
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};

const WAIT: Duration = Duration::from_secs(10);

/// SIGTERM reaches every test in this process; one test at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn quitting_clears_the_title_and_the_tab_mark() {
    let _one = ONE_AT_A_TIME.lock().await;
    let canter = Canter::start().await;
    canter.wait_for_title().await;
    canter.keys.send(TermEvent::Key(KeyEvent::new(
        KeyCode::Char('c'),
        KeyModifiers::CONTROL,
    )));
    canter.finished_with_clear_tab().await;
}

#[tokio::test]
async fn sigterm_clears_the_title_and_the_tab_mark() {
    let _one = ONE_AT_A_TIME.lock().await;
    let canter = Canter::start().await;
    // The title is written after the first frame, by then canter listens for SIGTERM.
    canter.wait_for_title().await;
    raise(Signal::SIGTERM).unwrap();
    canter.finished_with_clear_tab().await;
}

struct Canter {
    keys: canter_tui::InputSender,
    written: Arc<Mutex<Vec<String>>>,
    task: tokio::task::JoinHandle<Result<Option<String>, canter_tui::RunError>>,
    _server: FakeServer,
    _dir: tempfile::TempDir,
}

impl Canter {
    /// canter in Ghostty itself, on a session in `<temp>/project`.
    async fn start() -> Self {
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
        let env = BTreeMap::from([
            ("HOME".to_owned(), home.clone()),
            ("CANTER_HOME".to_owned(), format!("{home}/.canter")),
            ("TERM_PROGRAM".to_owned(), "ghostty".to_owned()),
            ("TERM".to_owned(), "xterm-ghostty".to_owned()),
        ]);
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
        let terminal = ratatui::Terminal::new(Screen {
            backend: TestBackend::new(80, 24),
            written: written.clone(),
        })
        .unwrap();
        let (keys, input) = input();
        let task = tokio::spawn(run_on(terminal, input, launch));
        Self {
            keys,
            written,
            task,
            _server: server,
            _dir: dir,
        }
    }

    fn written(&self) -> String {
        self.written.lock().unwrap().concat()
    }

    async fn wait_for_title(&self) {
        let shown = tokio::time::timeout(WAIT, async {
            while !self.written().contains("\x1b]0;canter · project\x07") {
                assert!(!self.task.is_finished(), "canter stopped");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(shown.is_ok(), "no title; written: {:?}", self.written());
    }

    async fn finished_with_clear_tab(self) {
        let written = self.written.clone();
        let result = tokio::time::timeout(WAIT, self.task)
            .await
            .expect("canter quits")
            .unwrap();
        assert_eq!(result.unwrap().as_deref(), Some("s1"));
        let written = written.lock().unwrap().concat();
        assert!(
            written.ends_with("\x1b]0;\x07\x1b]9;4;0\x07"),
            "last written: {written:?}"
        );
    }
}

/// A test backend that keeps what canter writes between frames.
struct Screen {
    backend: TestBackend,
    written: Arc<Mutex<Vec<String>>>,
}

impl FrameSync for Screen {
    fn write_sequence(&mut self, sequence: &str) -> Result<(), Self::Error> {
        self.written.lock().unwrap().push(sequence.to_owned());
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
