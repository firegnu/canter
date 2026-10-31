//! Ctrl+G: the draft in `$VISUAL` / `$EDITOR` and back (P2-31). A small
//! `sh` script stands in for the editor; the environment is the test's own.

use super::*;
use crate::fake::{FakeServer, Pace, parse_script};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::{ClearType, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use std::sync::Mutex;

const WAIT: Duration = Duration::from_secs(10);

fn lines(n: usize) -> String {
    (1..=n)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// An editor script in a directory of its own, where `$OUT` points.
struct Editor {
    dir: tempfile::TempDir,
    /// Where the editor's file goes.
    temp: tempfile::TempDir,
}

impl Editor {
    fn new(script: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let body = script.replace("$OUT", dir.path().to_str().unwrap());
        std::fs::write(dir.path().join("editor.sh"), body).unwrap();
        Self {
            dir,
            temp: tempfile::tempdir().unwrap(),
        }
    }

    /// `VISUAL` running the script, quoted as in a shell.
    fn env(&self) -> BTreeMap<String, String> {
        let script = self.dir.path().join("editor.sh");
        BTreeMap::from([(
            "VISUAL".to_owned(),
            format!("sh '{}'", script.to_str().unwrap()),
        )])
    }

    fn out(&self, name: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(name)).unwrap()
    }

    async fn run(&self, ui: &mut Ui<TestBackend>) {
        self.run_with(ui, &self.env()).await;
    }

    async fn run_with(&self, ui: &mut Ui<TestBackend>, env: &BTreeMap<String, String>) {
        let (_sender, input) = input();
        edit_draft(ui, &input, env, self.temp.path()).await.unwrap();
        ui.draw(0).unwrap();
        assert!(
            std::fs::read_dir(self.temp.path())
                .unwrap()
                .next()
                .is_none(),
            "the editor's file and directory are gone"
        );
    }
}

/// The input box with `look `, an image and an 11-line paste, folded.
fn drafted() -> Ui<TestBackend> {
    let mut snapshot = SessionSnapshot::empty("editor-test");
    snapshot.settings.model = "deepseek-flash".into();
    let app = App::new(snapshot, None, 0).with_clipboard_reader(|path| {
        std::fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
        Ok(true)
    });
    let mut ui = Ui::new(TestBackend::new(60, 30), app).unwrap();
    ui.app_mut().handle_paste("look ");
    press(&mut ui, KeyCode::Char('v'), KeyModifiers::CONTROL);
    ui.app_mut().handle_paste(&lines(11));
    ui.draw(0).unwrap();
    let shown = screen(&ui);
    assert!(
        shown.contains("look [Image #1 screenshot.png][Pasted #1 · 11 lines]"),
        "{shown}"
    );
    ui
}

fn press(ui: &mut Ui<TestBackend>, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    ui.app_mut().handle_key(KeyEvent::new(code, modifiers), 0)
}

/// Types `|` where the cursor is, then sends the draft.
fn send(ui: &mut Ui<TestBackend>) -> Option<Action> {
    press(ui, KeyCode::Char('|'), KeyModifiers::NONE);
    press(ui, KeyCode::Enter, KeyModifiers::NONE)
}

fn screen(ui: &Ui<TestBackend>) -> String {
    let buffer = ui.backend().buffer();
    buffer
        .content
        .chunks(usize::from(buffer.area.width))
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn without_visual_or_editor_the_status_line_says_so() {
    let mut ui = drafted();
    let editor = Editor::new(r#": > "$OUT/ran""#);
    let env = BTreeMap::from([("HOME".to_owned(), "/nowhere".to_owned())]);
    editor.run_with(&mut ui, &env).await;
    let shown = screen(&ui);
    assert!(
        shown.contains("Set $VISUAL or $EDITOR to use an external editor"),
        "{shown}"
    );
    assert!(!editor.dir.path().join("ran").exists());
    assert!(shown.contains("[Pasted #1 · 11 lines]"), "{shown}");
}

#[tokio::test]
async fn visual_is_split_into_the_program_and_its_arguments() {
    let mut ui = drafted();
    let editor = Editor::new(r#"printf '%s\n' "$@" > "$OUT/args""#);
    let script = editor.dir.path().join("editor.sh");
    let script = script.to_str().unwrap();
    let env = BTreeMap::from([
        (
            "VISUAL".to_owned(),
            format!(r#"sh '{script}' --wait "two words""#),
        ),
        ("EDITOR".to_owned(), "false".to_owned()),
    ]);
    editor.run_with(&mut ui, &env).await;
    let args = editor.out("args");
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(args[..2], ["--wait", "two words"]);
    assert!(args[2].ends_with("/message.md"), "{args:?}");
    assert_eq!(args.len(), 3);
}

#[tokio::test]
async fn the_editor_gets_pastes_in_full_and_image_markers_as_they_are() {
    let mut ui = drafted();
    let editor = Editor::new(r#"cp "$1" "$OUT/seen""#);
    editor.run(&mut ui).await;
    assert_eq!(
        editor.out("seen"),
        format!("look [Image #1 screenshot.png]{}", lines(11))
    );
}

#[tokio::test]
async fn what_the_editor_saved_is_the_draft_with_the_cursor_at_its_end() {
    // The image's marker stays, so does the image; the paste is in full now.
    let mut ui = drafted();
    let editor =
        Editor::new(r#"printf 'new [Image #1 screenshot.png] text\n%s\n' "$(sed 1d "$1")" > "$1""#);
    editor.run(&mut ui).await;
    let shown = screen(&ui);
    assert!(!shown.contains("[Pasted"), "{shown}");
    assert!(shown.contains("line 5"), "{shown}");
    let Some(Action::SendImages { text, input }) = send(&mut ui) else {
        panic!("the image must stay attached");
    };
    let rest = (2..=11).map(|i| format!("\nline {i}")).collect::<String>();
    assert_eq!(text, format!("new [Image #1 screenshot.png] text{rest}|"));
    assert!(
        matches!(&input[1], UserInput::LocalImage { .. }),
        "{input:?}"
    );

    // Its marker gone, the image goes too: written back, it is only words.
    let mut ui = drafted();
    let editor = Editor::new(r#"printf 'only words\n' > "$1""#);
    editor.run(&mut ui).await;
    let editor = Editor::new(r#"printf '%s [Image #1 screenshot.png]' "$(cat "$1")" > "$1""#);
    editor.run(&mut ui).await;
    assert_eq!(
        send(&mut ui),
        Some(Action::Send("only words [Image #1 screenshot.png]|".into()))
    );

    // Emptied, the draft is empty.
    let mut ui = drafted();
    let editor = Editor::new(r#": > "$1""#);
    editor.run(&mut ui).await;
    assert_eq!(
        press(&mut ui, KeyCode::Enter, KeyModifiers::NONE),
        None,
        "nothing to send"
    );
    assert!(ui.app().editor_draft().is_empty());
}

#[tokio::test]
async fn a_failed_editor_leaves_the_draft_and_says_why() {
    for (script, why) in [
        (
            r#"printf 'half done' > "$1"; exit 1"#,
            "Editor exited with code 1; draft unchanged",
        ),
        (r#"rm "$1""#, "Could not read the edited message"),
    ] {
        let mut ui = drafted();
        let editor = Editor::new(script);
        editor.run(&mut ui).await;
        let shown = screen(&ui);
        assert!(shown.contains(why), "{shown}");
        assert!(
            shown.contains("look [Image #1 screenshot.png][Pasted #1 · 11 lines]"),
            "{shown}"
        );
    }

    let mut ui = drafted();
    let editor = Editor::new("");
    let missing = editor.dir.path().join("no-such-editor");
    let env = BTreeMap::from([("EDITOR".to_owned(), missing.to_str().unwrap().to_owned())]);
    editor.run_with(&mut ui, &env).await;
    let shown = screen(&ui);
    assert!(shown.contains("Could not start"), "{shown}");
    assert!(shown.contains("[Pasted #1 · 11 lines]"), "{shown}");
}

/// A test backend whose screen the test reads while canter runs on it, and
/// which logs handing the terminal over and back.
#[derive(Clone)]
struct Shared {
    backend: Arc<Mutex<TestBackend>>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Shared {
    fn screen(&self) -> String {
        let backend = self.backend.lock().unwrap();
        let buffer = backend.buffer();
        buffer
            .content
            .chunks(usize::from(buffer.area.width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn log(&self) -> Vec<&'static str> {
        self.log.lock().unwrap().clone()
    }
}

impl FrameSync for Shared {
    fn suspend(&mut self) -> Result<(), Self::Error> {
        self.log.lock().unwrap().push("suspend");
        Ok(())
    }
    fn resume(&mut self) -> Result<(), Self::Error> {
        self.log.lock().unwrap().push("resume");
        Ok(())
    }
}

impl Backend for Shared {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.backend.lock().unwrap().draw(content)
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
        if clear_type == ClearType::All {
            self.log.lock().unwrap().push("clear");
        }
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

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let waited = tokio::time::timeout(WAIT, async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(waited.is_ok(), "{what}");
}

#[tokio::test]
async fn events_while_the_editor_is_open_show_after_it_closes() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let socket = dir.path().join("canter.sock");
    let script = format!(
        "{}\n{}\n",
        serde_json::json!({
            "seq": 1, "sessionId": "s1", "timestamp": 0, "type": "session.updated",
            "settings": { "cwd": project, "name": null, "model": "m", "approvalPolicy": "alwaysAsk" },
        }),
        serde_json::json!({
            "seq": 2, "sessionId": "s1", "timestamp": 0,
            "type": "session.status", "status": "idle",
        }),
    );
    let _server = FakeServer::start(&socket, parse_script(&script).unwrap(), 2, Pace::Instant)
        .await
        .unwrap();
    // Says it is open, waits to be told to close, then saves a new draft.
    let editor = Editor::new(
        r#": > "$OUT/open"
while [ ! -e "$OUT/close" ]; do sleep 0.02; done
printf 'written in the editor\n' > "$1"
"#,
    );
    let home = dir.path().to_str().unwrap().to_owned();
    let mut env = editor.env();
    env.insert("HOME".into(), home.clone());
    env.insert("CANTER_HOME".into(), format!("{home}/.canter"));
    let launch = Launch {
        connector: SocketConnector(socket.clone()),
        connected: Connected {
            client: Client::new(&socket).unwrap(),
            warning: None,
            server: None,
        },
        open: Open::Resume {
            id: Some("s1".to_owned()),
            cwd: project.to_str().unwrap().to_owned(),
            update: SessionUpdateParams::default(),
        },
        env,
        client_name: "canter-tui test".to_owned(),
        home: Some(home),
        version: "test".to_owned(),
    };
    let terminal = Shared {
        backend: Arc::new(Mutex::new(TestBackend::new(80, 24))),
        log: Arc::default(),
    };
    let (keys, input) = input();
    let task = tokio::spawn(run_on(
        Terminal::new(terminal.clone()).unwrap(),
        input,
        launch,
    ));
    until("the screen is up", || keys.frames() > 0).await;
    keys.send(TermEvent::Key(KeyEvent::new(
        KeyCode::Char('g'),
        KeyModifiers::CONTROL,
    )));
    let open = editor.dir.path().join("open");
    until("the editor opens", || open.exists()).await;
    assert_eq!(terminal.log(), ["suspend"]);
    let frames = keys.frames();

    // Another terminal sends a message; its whole turn happens meanwhile.
    let other = Client::new(&socket).unwrap();
    let params = SessionInputParams {
        non_interactive: false,
        approval_policy: None,
        review: None,
        input: vec![UserInput::Text {
            text: "asked from elsewhere".into(),
        }],
        mode: InputMode::Queue,
        client_name: "another terminal".into(),
    };
    other.send_input("s1", &params).await.unwrap();
    // Time for the events to reach this terminal while the editor is open.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(keys.frames(), frames, "nothing drawn under the editor");
    std::fs::write(editor.dir.path().join("close"), "").unwrap();

    until("the reply shows after the editor", || {
        terminal.screen().contains("Received: asked from")
    })
    .await;
    let shown = terminal.screen();
    assert!(shown.contains("written in the editor"), "{shown}");
    assert_eq!(terminal.log(), ["suspend", "resume", "clear"]);
    assert!(!task.is_finished());
    task.abort();
}
