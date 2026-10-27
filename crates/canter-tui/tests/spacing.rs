//! A blank row between the parts of the conversation (reads in a row stay
//! together), and the status line naming what the turn is doing.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
const WIDTH: u16 = 60;
/// Rows below the conversation: status line, input box (3) and info rows (2).
const BELOW: usize = 6;
const ESC: &str = "Esc to interrupt";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    /// The turn items go to.
    turn: String,
}

impl Screen {
    fn new(width: u16, height: u16) -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let ui = Ui::new(TestBackend::new(width, height), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            turn: String::new(),
        };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen.start_turn("t1", "do it");
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

    fn start_turn(&mut self, id: &str, text: &str) {
        self.turn = id.to_owned();
        self.event(json!({ "type": "turn.started", "turn": {
            "id": id, "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": text }],
            "clientName": "canter-tui", "usage": null,
        }}));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn finish_turn(&mut self, id: &str, text: &str) {
        self.event(json!({ "type": "turn.completed", "turn": {
            "id": id, "items": [], "status": "completed", "error": null,
            "input": [{ "type": "text", "text": text }],
            "clientName": "canter-tui", "usage": null,
        }}));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    fn started(&mut self, item: Value) {
        self.event(json!({ "type": "item.started", "turnId": self.turn, "item": item }));
    }

    fn completed(&mut self, item: Value) {
        self.event(json!({ "type": "item.completed", "turnId": self.turn, "item": item }));
    }

    fn say(&mut self, id: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": id, "text": text });
        self.started(message.clone());
        self.completed(message);
    }

    fn thought(&mut self, id: &str, text: &str) {
        self.completed(json!({
            "type": "reasoning", "id": id, "summary": [], "content": [text],
        }));
    }

    fn read(&mut self, id: &str, path: &str, status: &str) {
        let item = json!({
            "type": "fileRead", "id": id, "path": path, "startLine": 1,
            "content": "one\ntwo\n", "status": status,
        });
        if status == "inProgress" {
            self.started(item);
        } else {
            self.completed(item);
        }
    }

    fn command(&mut self, id: &str, command: &str, status: &str) {
        let done = status != "inProgress";
        let item = json!({
            "type": "commandExecution", "id": id, "command": command, "cwd": "/work",
            "status": status, "aggregatedOutput": if done { "ok\n" } else { "" },
            "exitCode": if done { json!(0) } else { Value::Null },
            "durationMs": if done { json!(1200) } else { Value::Null },
        });
        if done {
            self.completed(item);
        } else {
            self.started(item);
        }
    }

    fn edit(&mut self, id: &str, status: &str) {
        let item = json!({
            "type": "fileChange", "id": id, "status": status,
            "changes": [{
                "path": "a.txt", "kind": { "type": "update", "movePath": null },
                "diff": "@@ -1 +1 @@\n-old\n+new\n",
            }],
        });
        if status == "inProgress" {
            self.started(item);
        } else {
            self.completed(item);
        }
    }

    fn key(&mut self, code: KeyCode) {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), NOW);
    }

    fn lines(&mut self) -> Vec<String> {
        self.ui.draw(NOW).unwrap();
        let buffer = self.ui.backend().buffer();
        (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect()
    }

    /// The conversation's rows, above the status line, without the blank
    /// rows at the bottom.
    fn conversation(&mut self) -> Vec<String> {
        let lines = self.lines();
        let mut rows = lines[..lines.len() - BELOW].to_vec();
        while rows.last().is_some_and(|row| row.is_empty()) {
            rows.pop();
        }
        rows
    }

    fn status(&mut self) -> String {
        let lines = self.lines();
        lines[lines.len() - BELOW].clone()
    }
}

fn row(buffer: &Buffer, y: u16) -> String {
    let mut row = String::new();
    let mut x = 0;
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        row.push_str(symbol);
        x += symbol.width().max(1) as u16;
    }
    row
}

/// Each row reduced to its first few words, `""` for a blank row.
fn outline(rows: &[String]) -> Vec<String> {
    rows.iter()
        .map(|row| row.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .collect()
}

#[test]
fn a_blank_row_separates_messages_cards_and_thinking_but_not_reads_in_a_row() {
    let mut screen = Screen::new(WIDTH, 40);
    screen.thought("r1", "Look at the files first.");
    screen.say("m1", "Reading two files.");
    screen.read("f1", "a.txt", "completed");
    screen.read("f2", "b.txt", "completed");
    screen.edit("e1", "completed");
    screen.command("c1", "cargo build", "completed");
    screen.say("m2", "Done.");
    screen.finish_turn("t1", "do it");
    screen.start_turn("t2", "thanks");
    screen.say("m3", "You're welcome.");
    screen.finish_turn("t2", "thanks");

    assert_eq!(
        outline(&screen.conversation()),
        [
            "› do it",
            "",
            "Thought: Look at",
            "",
            "Reading two files.",
            "",
            "◇ Read a.txt",
            "◇ Read b.txt",
            "",
            "● Edit a.txt",
            "1 - old",
            "1 + new",
            "",
            "● Run cargo",
            "┃ ok",
            "",
            "Done.",
            "",
            "─ Worked for",
            "",
            "› thanks",
            "",
            "You're welcome.",
            "",
            "─ Worked for",
        ]
    );
}

#[test]
fn notices_a_turn_end_and_queued_messages_are_blocks_set_apart() {
    let mut screen = Screen::new(WIDTH, 30);
    screen.say("m1", "Before.");
    screen.event(json!({ "type": "warning", "message": "First warning" }));
    screen.event(json!({ "type": "warning", "message": "Second warning" }));
    screen.say("m2", "After.");
    for (id, text) in [("q1", "one"), ("q2", "two")] {
        screen.event(json!({ "type": "input.queued", "input": {
            "id": id, "input": [{ "type": "text", "text": text }], "clientName": "canter-tui",
        }}));
    }
    assert_eq!(
        outline(&screen.conversation()),
        [
            "› do it",
            "",
            "Before.",
            "",
            "! First warning",
            "! Second warning",
            "",
            "After.",
            "",
            "· Queued: one",
            "· Queued: two",
        ]
    );

    let mut screen = Screen::new(WIDTH, 30);
    screen.say("m1", "Stopping.");
    screen.event(json!({ "type": "turn.completed", "turn": {
        "id": "t1", "items": [], "status": "interrupted", "error": null,
        "input": [{ "type": "text", "text": "do it" }],
        "clientName": "canter-tui", "usage": null,
    }}));
    screen.event(json!({ "type": "session.status", "status": "idle" }));
    assert_eq!(
        outline(&screen.conversation()),
        ["› do it", "", "Stopping.", "", "■ Turn interrupted"]
    );
}

#[test]
fn blank_rows_stay_put_when_a_part_finishes() {
    let mut screen = Screen::new(WIDTH, 30);
    screen.say("m1", "Reading.");
    screen.read("f1", "a.txt", "inProgress");
    let reading = screen.conversation();
    screen.read("f1", "a.txt", "completed");
    screen.read("f2", "b.txt", "inProgress");
    let second = screen.conversation();
    screen.read("f2", "b.txt", "completed");
    let message = json!({ "type": "agentMessage", "id": "m2", "text": "All read." });
    screen.started(message.clone());
    let writing = screen.conversation();
    screen.completed(message);
    let written = screen.conversation();

    assert_eq!(
        outline(&reading),
        ["› do it", "", "Reading.", "", "◇ Read a.txt"]
    );
    assert_eq!(outline(&second[..reading.len()]), outline(&reading));
    assert_eq!(outline(&second[reading.len()..]), ["◇ Read b.txt"]);
    assert_eq!(outline(&writing[..second.len()]), outline(&second));
    assert_eq!(outline(&writing[second.len()..]), ["", "All read."]);
    assert_eq!(written, writing, "nothing moves when the message finishes");
}

#[test]
fn the_status_line_names_what_the_turn_is_doing() {
    let mut screen = Screen::new(WIDTH, 20);
    assert_eq!(screen.status(), " ⠋ Working · 0s · Esc to interrupt");

    screen.started(json!({ "type": "reasoning", "id": "r1", "summary": [], "content": [] }));
    assert_eq!(screen.status(), " ⠋ Thinking · 0s · Esc to interrupt");

    screen.started(json!({ "type": "agentMessage", "id": "m1", "text": "Hi" }));
    assert_eq!(screen.status(), " ⠋ Writing · 0s · Esc to interrupt");

    screen.read("f1", "a.txt", "inProgress");
    assert_eq!(screen.status(), " ⠋ Reading · 0s · Esc to interrupt");
    screen.read("f1", "a.txt", "completed");
    assert_eq!(screen.status(), " ⠋ Working · 0s · Esc to interrupt");

    screen.edit("e1", "inProgress");
    assert_eq!(screen.status(), " ⠋ Editing · 0s · Esc to interrupt");
    screen.edit("e1", "completed");

    screen.started(json!({
        "type": "mcpToolCall", "id": "p1", "server": "github", "tool": "search",
        "arguments": {}, "result": "", "status": "inProgress",
    }));
    assert_eq!(screen.status(), " ⠋ Calling · 0s · Esc to interrupt");
    screen.completed(json!({
        "type": "mcpToolCall", "id": "p1", "server": "github", "tool": "search",
        "arguments": {}, "result": "ok", "status": "completed",
    }));

    screen.started(json!({
        "type": "webFetch", "id": "w1", "url": "https://example.com/",
        "result": "", "status": "inProgress",
    }));
    assert_eq!(screen.status(), " ⠋ Fetching · 0s · Esc to interrupt");
    screen.completed(json!({
        "type": "webFetch", "id": "w1", "url": "https://example.com/",
        "result": "ok", "status": "completed",
    }));

    screen.command("c1", "cargo build\ncargo test", "inProgress");
    assert_eq!(
        screen.status(),
        " ⠋ Running cargo build … · 0s · Esc to interrupt"
    );
    screen.command("c1", "cargo build\ncargo test", "completed");

    // A long command is cut so that the time and Esc still show.
    let long = format!("cargo test {}", "--package canter-tui ".repeat(5));
    screen.command("c2", &long, "inProgress");
    let status = screen.status();
    assert!(
        status.starts_with(" ⠋ Running cargo test --package")
            && status.ends_with("… · 0s · Esc to interrupt"),
        "{status:?}"
    );
    assert_eq!(status.width(), usize::from(WIDTH));
}

#[test]
fn with_details_shown_both_hints_show_and_a_narrow_window_cuts_the_command_first() {
    let mut screen = Screen::new(100, 20);
    screen.command("c1", "cargo build", "inProgress");
    screen.ui.app_mut().handle_key(
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        NOW,
    );
    assert_eq!(
        screen.status(),
        " ⠋ Running cargo build · 0s · Esc to interrupt · Details shown · Ctrl+O to fold"
    );

    let long = format!("cargo test {}", "--package canter-tui ".repeat(5));
    screen.command("c1", "cargo build", "completed");
    screen.command("c2", &long, "inProgress");
    let status = screen.status();
    assert!(
        status.starts_with(" ⠋ Running cargo test --package")
            && status.ends_with("… · 0s · Esc to interrupt · Details shown · Ctrl+O to fold"),
        "{status:?}"
    );
    assert_eq!(status.width(), 100);

    // Too narrow for the shortest command kept: the Ctrl+O hint is left out.
    screen.ui.backend_mut().resize(50, 20);
    assert_eq!(
        screen.status(),
        " ⠋ Running cargo test … · 0s · Esc to interrupt"
    );
}

#[test]
fn waiting_for_approval_does_not_offer_esc() {
    let mut screen = Screen::new(WIDTH, 30);
    screen.command("c1", "rm -rf target", "inProgress");
    screen.event(json!({ "type": "request.opened", "request": {
        "id": "q1", "turnId": "t1", "createdAt": NOW,
        "kind": {
            "type": "approval", "itemId": "c1", "tool": "shell",
            "arguments": { "command": "rm -rf target" },
            "availableDecisions": ["accept", "decline", "cancel"],
        },
    }}));
    screen.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    let lines = screen.lines();
    let status = lines
        .iter()
        .find(|line| line.starts_with(" ⏸"))
        .expect("status line");
    assert_eq!(status, " ⏸ Waiting for approval · 0s");
    assert!(!lines.iter().any(|line| line.contains(ESC)));
}

#[test]
fn scrolled_back_a_new_word_in_the_status_line_is_not_new_output() {
    let mut screen = Screen::new(WIDTH, 14);
    for i in 0..10 {
        screen.say(&format!("m{i}"), &format!("Message {i}."));
    }
    let message = json!({ "type": "agentMessage", "id": "last", "text": "Last one." });
    screen.started(message.clone());
    screen.key(KeyCode::PageUp);
    assert!(screen.status().starts_with(" ⠋ Writing"));
    let before = screen.conversation();

    screen.completed(message);
    assert!(screen.status().starts_with(" ⠋ Working"));
    let lines = screen.lines();
    assert!(
        !lines.iter().any(|line| line.contains("New output below")),
        "{lines:#?}"
    );
    assert_eq!(screen.conversation(), before);
}
