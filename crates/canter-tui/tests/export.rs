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

#[test]
fn retry_and_interrupted_reply_remain_visible_after_snapshot_reload_and_export() {
    let mut screen = Screen::with_turn(Ok(()));
    let mut turn = json!({"id":"t2","items":[],"status":"inProgress","error":null,"input":[],"clientName":"test","usage":null});
    screen.event(json!({"type":"turn.started","turn":turn}));
    for item in [
        json!({"type":"agentMessage","id":"partial","text":"partial reply","status":"interrupted"}),
        json!({"type":"modelRetry","id":"retry","error":{"message":"server error 503","additionalDetails":null},"willRetry":true,"attempt":3,"maxRetries":10,"delaySeconds":4.0}),
    ] {
        screen.event(json!({"type":"item.completed","turnId":"t2","item":item}));
    }
    turn["status"] = json!("completed");
    screen.event(json!({"type":"turn.completed","turn":turn}));
    screen.draw();
    assert!(screen.text().contains("Interrupted"), "{}", screen.text());
    assert!(
        screen
            .text()
            .contains("Retried 3 times after server error 503"),
        "{}",
        screen.text()
    );
    let stored = serde_json::to_value(screen.ui.app().snapshot()).unwrap();
    let restored: SessionSnapshot = serde_json::from_value(stored).unwrap();
    let mut other = Ui::new(TestBackend::new(80, 24), App::new(restored, None, NOW)).unwrap();
    other.draw(NOW).unwrap();
    assert_eq!(other.backend().buffer(), screen.ui.backend().buffer());
    screen.export(Export::File(Some("retry.md".into())));
    screen.wait_for("retry.md");
    let text = std::fs::read_to_string(screen.path("retry.md")).unwrap();
    assert!(text.contains("partial reply") && text.contains("_Interrupted_"));
    assert!(text.contains("Retried 3 times after server error 503"));
}

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
    // The temporary path has a different length on each OS; the sentence may wrap.
    screen.wait_for("not overwritten");
    let words = screen
        .text()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(words.contains("already exists; not overwritten"), "{words}");
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

#[test]
fn mcp_search_card_survives_snapshot_reload_and_export() {
    let mut screen = Screen::with_turn(Ok(()));
    let mut turn = json!({"id":"t2","items":[],"status":"inProgress","error":null,"input":[],"clientName":"test","usage":null});
    screen.event(json!({"type":"turn.started","turn":turn}));
    screen.event(json!({"type":"item.completed","turnId":"t2","item":{
        "type":"mcpToolSearch","id":"search","query":"issue","tools":["mcp__github__issues"],"status":"completed"
    }}));
    turn["status"] = json!("completed");
    screen.event(json!({"type":"turn.completed","turn":turn}));
    screen.draw();
    assert!(screen.text().contains("Searched MCP tools \"issue\""));
    assert!(screen.text().contains("mcp__github__issues"));
    let stored = serde_json::to_value(screen.ui.app().snapshot()).unwrap();
    let restored: SessionSnapshot = serde_json::from_value(stored).unwrap();
    let mut other = Ui::new(TestBackend::new(80, 24), App::new(restored, None, NOW)).unwrap();
    other.draw(NOW).unwrap();
    assert_eq!(other.backend().buffer(), screen.ui.backend().buffer());
    screen.export(Export::File(Some("mcp.md".into())));
    screen.wait_for("mcp.md");
    let text = std::fs::read_to_string(screen.path("mcp.md")).unwrap();
    assert!(text.contains("Searched MCP tools `issue` · 1 found"));
    assert!(text.contains("mcp__github__issues"));
}

fn retry_json(id: &str, message: &str, attempt: u32, delay: f64) -> Value {
    json!({"type":"modelRetry","id":id,"error":{"message":message,"additionalDetails":null},
        "willRetry":true,"attempt":attempt,"maxRetries":10,"delaySeconds":delay})
}

impl Screen {
    /// The rows that mention a retry.
    fn retry_rows(&self) -> Vec<String> {
        self.text()
            .lines()
            .filter(|row| row.contains("etr"))
            .map(|row| row.trim().to_owned())
            .collect()
    }
}

/// Retries in a row are one row that changes while they go on and then says
/// how many there were; exported the same. Text saved by older versions is
/// shown as it was, also as one row.
#[test]
fn retries_in_a_row_are_one_row_then_a_dim_summary() {
    let mut screen = Screen::with_turn(Ok(()));
    let mut turn = json!({"id":"t2","items":[],"status":"inProgress","error":null,"input":[],"clientName":"test","usage":null});
    screen.event(json!({"type":"turn.started","turn":turn}));
    for (attempt, delay) in [(1, 0.5), (2, 1.2), (3, 2.0), (4, 3.9)] {
        let item = retry_json(
            &format!("r{attempt}"),
            "server error (HTTP 500)",
            attempt,
            delay,
        );
        screen.event(json!({"type":"item.started","turnId":"t2","item":item}));
        screen.event(json!({"type":"item.completed","turnId":"t2","item":item}));
        screen.draw();
        let seconds = delay.ceil() as u64;
        assert_eq!(
            screen.retry_rows(),
            [format!(
                "Server error (HTTP 500) · retrying in {seconds}s ({attempt}/10)"
            )],
            "{}",
            screen.text()
        );
    }
    let answer = json!({"type":"agentMessage","id":"m2","text":"好了。"});
    screen.event(json!({"type":"item.completed","turnId":"t2","item":answer}));
    screen.draw();
    assert_eq!(
        screen.retry_rows(),
        ["Retried 4 times after server error (HTTP 500)"]
    );
    let buffer = screen.ui.backend().buffer();
    let y = screen
        .text()
        .lines()
        .position(|row| row.contains("Retried 4 times"))
        .unwrap() as u16;
    assert!(
        buffer[(2, y)]
            .modifier
            .contains(ratatui::style::Modifier::DIM)
    );
    turn["status"] = json!("completed");
    screen.event(json!({"type":"turn.completed","turn":turn}));

    // An older session's text, as it was saved; one retry alone says once.
    let mut old = json!({"id":"t3","items":[],"status":"inProgress","error":null,"input":[],"clientName":"test","usage":null});
    screen.event(json!({"type":"turn.started","turn":old}));
    for attempt in [1, 2] {
        let item = retry_json(
            &format!("o{attempt}"),
            "model request failed: Server (HTTP status: Some(500))",
            attempt,
            1.0,
        );
        screen.event(json!({"type":"item.completed","turnId":"t3","item":item}));
    }
    screen.event(json!({"type":"item.completed","turnId":"t3","item":{"type":"agentMessage","id":"m3","text":"嗯。"}}));
    let item = retry_json("o3", "network error", 1, 1.0);
    screen.event(json!({"type":"item.completed","turnId":"t3","item":item}));
    old["status"] = json!("completed");
    screen.event(json!({"type":"turn.completed","turn":old}));
    screen.draw();
    let shown = [
        "Retried 4 times after server error (HTTP 500)",
        "Retried 2 times after model request failed: Server (HTTP status: Some(500))",
        "Retried once after network error",
    ];
    assert_eq!(screen.retry_rows(), shown, "{}", screen.text());

    screen.export(Export::File(Some("retries.md".into())));
    screen.wait_for("retries.md");
    let text = std::fs::read_to_string(screen.path("retries.md")).unwrap();
    for line in shown {
        assert_eq!(text.matches(line).count(), 1, "{text}");
    }
    assert!(!text.contains("retrying in"), "{text}");
}
