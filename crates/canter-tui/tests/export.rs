//! `/export`: alone it offers a file with the default name or the clipboard;
//! with a path it writes there. The conversation says where the Markdown
//! went and how many lines, or why not. The clipboard is a fake here, and
//! files go to a temporary session directory.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{Action, App, Export, Ui};
use chrono::TimeZone;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use tempfile::TempDir;

const SESSION: &str = "s1";
const NOW: i64 = 1_800_000_000_000;
const WAIT: Duration = Duration::from_secs(5);

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    dir: TempDir,
    copied: mpsc::Receiver<String>,
}

impl Screen {
    /// A session named `跑测试` in a temporary directory; the fake clipboard
    /// answers with `clipboard`.
    fn new(clipboard: Result<(), String>) -> Self {
        let (sender, copied) = mpsc::channel();
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW).with_clipboard_writer(
            move |text: &str| {
                let _ = sender.send(text.to_owned());
                clipboard.clone()
            },
        );
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(80, 24), app).unwrap(),
            seq: 0,
            dir: tempfile::tempdir().unwrap(),
            copied,
        };
        let cwd = screen.dir.path().display().to_string();
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": cwd, "name": "跑测试", "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen
    }

    /// One finished turn: a question, a command and an answer.
    fn with_turn(clipboard: Result<(), String>) -> Self {
        let mut screen = Self::new(clipboard);
        let turn = |status: &str| {
            json!({
                "id": "t1", "items": [], "status": status, "error": null,
                "input": [{ "type": "text", "text": "跑一下测试" }],
                "clientName": "canter-tui", "usage": null,
            })
        };
        screen.event(json!({ "type": "turn.started", "turn": turn("inProgress") }));
        let command = json!({
            "type": "commandExecution", "id": "c1", "command": "cargo test",
            "cwd": "/work", "status": "completed", "aggregatedOutput": "test result: ok\n",
            "exitCode": 0, "durationMs": 10,
        });
        screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": command }));
        let answer = json!({ "type": "agentMessage", "id": "m1", "text": "测试都过了。" });
        screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": answer }));
        screen.event(json!({ "type": "turn.completed", "turn": turn("completed") }));
        screen.event(json!({ "type": "session.status", "status": "idle" }));
        screen.draw();
        screen
    }

    fn event(&mut self, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": NOW });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, NOW);
    }

    fn draw(&mut self) {
        self.ui.draw(NOW).unwrap();
    }

    fn text(&self) -> String {
        let buffer = self.ui.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), NOW)
    }

    fn enter(&mut self, text: &str) -> Option<Action> {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
        self.key(KeyCode::Enter)
    }

    /// What the run loop does with an action.
    fn export(&mut self, to: Export) {
        self.ui.app_mut().export(to, NOW);
        self.draw();
    }

    /// Draws until the screen shows `text`.
    fn wait_for(&mut self, text: &str) {
        let start = Instant::now();
        while !self.text().contains(text) {
            assert!(
                start.elapsed() < WAIT,
                "never showed {text:?}:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(5));
            self.draw();
        }
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(name)
    }
}

/// `canter-跑测试-<local time>.md`, the default name at [`NOW`].
fn default_name() -> String {
    let time = chrono::Local.timestamp_millis_opt(NOW).unwrap();
    format!("canter-跑测试-{}.md", time.format("%Y%m%d-%H%M%S"))
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// `/export` alone offers the two places; Esc closes the choice and nothing
/// is written; Enter on the first writes the default file in the session's
/// directory, and the conversation says where and how many lines. A second
/// export at the same time leaves that file alone and says so.
#[test]
fn export_alone_offers_a_file_or_the_clipboard_and_never_overwrites() {
    let mut screen = Screen::with_turn(Ok(()));
    assert_eq!(screen.enter("/export"), None);
    screen.draw();
    let shown = screen.text();
    assert!(shown.contains("Export conversation"), "{shown}");
    assert!(shown.contains("Save to file"), "{shown}");
    assert!(shown.contains("Copy to clipboard"), "{shown}");
    assert_eq!(screen.key(KeyCode::Esc), None);
    screen.draw();
    assert!(!screen.text().contains("Save to file"), "{}", screen.text());
    assert!(files(screen.dir.path()).is_empty());

    screen.enter("/export");
    screen.draw();
    let action = screen.key(KeyCode::Enter);
    assert_eq!(action, Some(Action::Export(Export::File(None))));
    screen.export(Export::File(None));
    let name = default_name();
    assert_eq!(files(screen.dir.path()), std::slice::from_ref(&name));
    let path = screen.path(&name);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.starts_with("# 跑测试\n"), "{written}");
    assert!(written.contains("\n## User\n\n跑一下测试\n"), "{written}");
    assert!(
        written.contains("**Tool** · Ran `cargo test` · ok, exit 0\n\n```\ntest result: ok\n```\n"),
        "{written}"
    );
    assert!(written.contains("\n测试都过了。\n"), "{written}");
    let lines = written.lines().count();
    // The path after it wraps onto the next rows.
    screen.wait_for(&format!("✓ Exported {lines} lines to"));
    assert!(screen.text().contains(".md"), "{}", screen.text());

    std::fs::write(&path, "mine").unwrap();
    screen.export(Export::File(None));
    screen.wait_for("already exists; not overwritten");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine");
}

/// `/export <path>` writes there at once, relative to the session's
/// directory; a directory that is not there is an error, shown, not a crash.
#[test]
fn export_with_a_path_writes_there() {
    let mut screen = Screen::with_turn(Ok(()));
    let action = screen.enter("/export notes.md");
    assert_eq!(
        action,
        Some(Action::Export(Export::File(Some("notes.md".into()))))
    );
    screen.export(Export::File(Some("notes.md".into())));
    let written = std::fs::read_to_string(screen.path("notes.md")).unwrap();
    assert!(written.contains("测试都过了。"), "{written}");
    screen.wait_for("Exported ");

    screen.export(Export::File(Some("missing/notes.md".into())));
    screen.wait_for("Export failed: cannot create");
}

/// The second choice puts the same Markdown on the clipboard; the
/// conversation says how many lines, or why it could not.
#[test]
fn export_to_the_clipboard_says_how_it_went() {
    let mut screen = Screen::with_turn(Ok(()));
    screen.enter("/export");
    screen.draw();
    screen.key(KeyCode::Down);
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::Export(Export::Clipboard))
    );
    screen.export(Export::Clipboard);
    let copied = screen.copied.recv_timeout(WAIT).unwrap();
    assert!(copied.starts_with("# 跑测试\n"), "{copied}");
    assert!(copied.contains("测试都过了。"), "{copied}");
    let lines = copied.lines().count();
    screen.wait_for(&format!(
        "Copied {lines} lines of Markdown to the clipboard"
    ));
    assert!(files(screen.dir.path()).is_empty());

    let mut screen = Screen::with_turn(Err("no clipboard here".into()));
    screen.export(Export::Clipboard);
    screen.wait_for("Export failed: no clipboard here");
}

/// Before anything is said there is nothing to export: the conversation
/// says so and no file is written.
#[test]
fn an_empty_session_has_nothing_to_export() {
    let mut screen = Screen::new(Ok(()));
    screen.draw();
    screen.export(Export::File(None));
    screen.wait_for("Export failed: nothing to export yet");
    assert!(files(screen.dir.path()).is_empty());
}
