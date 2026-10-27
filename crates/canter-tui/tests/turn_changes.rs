//! The files a turn changed, listed at its end above `Worked for`: the
//! count and lines added and removed in all, then a row for each file.
//! Only canter's file-changing tools that worked count.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const WIDTH: u16 = 60;
/// Rows below the conversation: status line, input box (3) and info rows (2).
const BELOW: usize = 6;
const T0: i64 = 1_800_000_000_000;

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    now: i64,
    turn: String,
    items: usize,
}

impl Screen {
    fn new() -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, T0);
        let ui = Ui::new(TestBackend::new(WIDTH, 60), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            now: T0,
            turn: String::new(),
            items: 0,
        };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen
    }

    fn event(&mut self, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": self.now });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, self.now);
    }

    fn turn(&self, status: &str) -> Value {
        json!({
            "id": self.turn, "items": [], "status": status, "error": null,
            "input": [{ "type": "text", "text": format!("prompt {}", self.turn) }],
            "clientName": "canter-tui", "usage": null,
        })
    }

    fn start_turn(&mut self, id: &str) {
        self.turn = id.to_owned();
        let turn = self.turn("inProgress");
        self.event(json!({ "type": "turn.started", "turn": turn }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn end_turn(&mut self, status: &str) {
        self.now += 2_000;
        let turn = self.turn(status);
        self.event(json!({ "type": "turn.completed", "turn": turn }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    /// A file-changing tool's item with `changes`, ended with `status`.
    fn change(&mut self, status: &str, changes: Value) {
        self.items += 1;
        let item = json!({
            "type": "fileChange", "id": format!("e{}", self.items),
            "status": status, "changes": changes,
        });
        self.event(json!({ "type": "item.completed", "turnId": self.turn, "item": item }));
    }

    fn command(&mut self) {
        self.items += 1;
        let item = json!({
            "type": "commandExecution", "id": format!("c{}", self.items),
            "command": "sed -i s/a/b/ a.txt", "cwd": "/work", "status": "completed",
            "aggregatedOutput": "", "exitCode": 0, "durationMs": 10,
        });
        self.event(json!({ "type": "item.completed", "turnId": self.turn, "item": item }));
    }

    fn say(&mut self, text: &str) {
        self.items += 1;
        let item =
            json!({ "type": "agentMessage", "id": format!("m{}", self.items), "text": text });
        self.event(json!({ "type": "item.completed", "turnId": self.turn, "item": item }));
    }

    fn ctrl_o(&mut self) {
        self.ui.app_mut().handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            self.now,
        );
    }

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(self.now).unwrap();
        self.ui.backend().buffer()
    }

    fn conversation(&mut self) -> Vec<String> {
        let buffer = self.draw();
        let lines: Vec<String> = (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect();
        lines[..lines.len() - BELOW].to_vec()
    }

    /// The rows from `  ● Changed` up to and including the turn's last row.
    fn summary(&mut self) -> Vec<String> {
        let rows = self.conversation();
        let start = rows
            .iter()
            .position(|row| row.starts_with("  ● Changed"))
            .unwrap_or_else(|| panic!("no summary: {rows:#?}"));
        let end = rows[start..]
            .iter()
            .position(|row| {
                row.starts_with("  ─ Worked") || row.starts_with("  ■") || row.starts_with("  ✗")
            })
            .unwrap_or_else(|| panic!("no footer: {rows:#?}"));
        rows[start..=start + end].to_vec()
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

/// The cells of the first row starting with `start`, from where `text` is in it.
fn cells_of<'a>(buffer: &'a Buffer, start: &str, text: &str) -> Vec<&'a Cell> {
    for y in 0..buffer.area.height {
        let row = row(buffer, y);
        if row.starts_with(start)
            && let Some(at) = row.find(text)
        {
            let x = row[..at].width() as u16;
            return (x..x + text.width() as u16)
                .map(|x| &buffer[(x, y)])
                .collect();
        }
    }
    panic!("{text:?} not in a row starting {start:?}");
}

fn update(path: &str, added: usize, removed: usize) -> Value {
    let mut diff = format!("--- a/{path}\n+++ b/{path}\n@@ -1,{removed} +1,{added} @@\n");
    for n in 0..removed {
        diff += &format!("-old {n}\n");
    }
    for n in 0..added {
        diff += &format!("+new {n}\n");
    }
    json!({ "path": path, "kind": { "type": "update", "movePath": null }, "diff": diff })
}

fn add(path: &str, lines: usize) -> Value {
    let diff: String = (0..lines).map(|n| format!("line {n}\n")).collect();
    json!({ "path": path, "kind": { "type": "add" }, "diff": diff })
}

fn delete(path: &str, lines: usize) -> Value {
    let diff: String = (0..lines).map(|n| format!("line {n}\n")).collect();
    json!({ "path": path, "kind": { "type": "delete" }, "diff": diff })
}

/// Three files: the count and totals, a row each with the counts in one
/// column, no `−0`; `+` green, `−` red, the rest grey.
#[test]
fn three_changed_files_are_listed_above_worked_for() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.change(
        "completed",
        json!([
            update("/work/src/app.rs", 30, 8),
            update("src/input.rs", 8, 4)
        ]),
    );
    screen.change("completed", json!([update("/work/README.md", 2, 0)]));
    screen.end_turn("completed");
    assert_eq!(
        screen.summary(),
        [
            "  ● Changed 3 files  +40 −12",
            "      src/app.rs    +30 −8",
            "      src/input.rs   +8 −4",
            "      README.md      +2",
            "  ─ Worked for 2s ─",
        ]
    );
    let buffer = screen.draw();
    for cell in cells_of(buffer, "  ● Changed", "+40") {
        assert_eq!(cell.fg, Color::Green);
    }
    for cell in cells_of(buffer, "  ● Changed", "−12") {
        assert_eq!(cell.fg, Color::Red);
    }
    for cell in cells_of(buffer, "      src/app.rs", "−8") {
        assert_eq!(cell.fg, Color::Red);
    }
    for text in ["●", "Changed 3 files"] {
        for cell in cells_of(buffer, "  ● Changed", text) {
            assert!(cell.modifier.contains(Modifier::DIM), "{text:?} is grey");
        }
    }
    for cell in cells_of(buffer, "      src/app.rs", "src/app.rs") {
        assert!(cell.modifier.contains(Modifier::DIM), "the path is grey");
    }
}

/// A file changed twice is one row, its lines added up, where it was first
/// changed; one file is `1 file`.
#[test]
fn a_file_changed_twice_is_one_row() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.change("completed", json!([update("b.rs", 1, 1)]));
    screen.change("completed", json!([update("a.rs", 2, 0)]));
    screen.change("completed", json!([update("/work/b.rs", 3, 2)]));
    screen.end_turn("completed");
    assert_eq!(
        screen.summary(),
        [
            "  ● Changed 2 files  +6 −3",
            "      b.rs  +4 −3",
            "      a.rs  +2",
            "  ─ Worked for 2s ─",
        ]
    );

    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.change("completed", json!([update("a.rs", 1, 0)]));
    screen.change("completed", json!([update("a.rs", 1, 0)]));
    screen.end_turn("completed");
    assert_eq!(screen.summary()[0], "  ● Changed 1 file  +2");
}

/// A new file says `new`, a deleted one `deleted`, a moved one is written
/// as its card writes it.
#[test]
fn new_deleted_and_moved_files() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.change("completed", json!([add("new.rs", 6)]));
    screen.change("completed", json!([delete("old.rs", 4)]));
    screen.change(
        "completed",
        json!([{ "path": "a.rs", "kind": { "type": "update", "movePath": "b.rs" },
            "diff": "@@ -1 +1 @@\n-x\n+y\n" }]),
    );
    screen.change("completed", json!([update("b.rs", 1, 0)]));
    screen.end_turn("completed");
    assert_eq!(
        screen.summary(),
        [
            "  ● Changed 3 files  +8 −5",
            "      new.rs       +6     new",
            "      old.rs          −4  deleted",
            "      a.rs → b.rs  +2 −1",
            "  ─ Worked for 2s ─",
        ]
    );
}

/// Failed and declined changes and shell commands do not count; a turn that
/// changed nothing has no list, and its end is as before.
#[test]
fn failed_declined_and_shell_changes_do_not_count() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.change("failed", json!([update("failed.rs", 1, 0)]));
    screen.change("declined", json!([update("declined.rs", 1, 0)]));
    screen.command();
    screen.change("completed", json!([update("kept.rs", 1, 0)]));
    screen.end_turn("completed");
    assert_eq!(
        screen.summary(),
        [
            "  ● Changed 1 file  +1",
            "      kept.rs  +1",
            "  ─ Worked for 2s ─",
        ]
    );

    for changes in [false, true] {
        let mut screen = Screen::new();
        screen.start_turn("t1");
        screen.say("nothing to change");
        if changes {
            screen.change("failed", json!([update("failed.rs", 1, 0)]));
            screen.command();
        }
        screen.end_turn("completed");
        let rows = screen.conversation();
        assert!(!rows.iter().any(|row| row.contains("Changed")), "{rows:#?}");
        assert!(
            rows.iter().any(|row| row == "  ─ Worked for 2s ─"),
            "{rows:#?}"
        );
    }
}

/// Over 8 files, the first 8 and `… and N more`.
#[test]
fn more_than_eight_files() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    let changes: Vec<Value> = (1..=11)
        .map(|n| update(&format!("f{n}.rs"), 1, 0))
        .collect();
    screen.change("completed", Value::Array(changes));
    screen.end_turn("completed");
    let summary = screen.summary();
    assert_eq!(summary[0], "  ● Changed 11 files  +11");
    assert_eq!(summary[1], "      f1.rs  +1");
    assert_eq!(summary[8], "      f8.rs  +1");
    assert_eq!(summary[9], "      … and 3 more");
    assert_eq!(summary[10], "  ─ Worked for 2s ─");
    assert_eq!(summary.len(), 11);
}

/// An interrupted or failed turn lists what it changed too; a turn still
/// going does not, and Ctrl+O changes nothing in the list.
#[test]
fn interrupted_and_failed_turns_list_and_running_ones_do_not() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.change("completed", json!([update("a.rs", 1, 1)]));
    let rows = screen.conversation();
    assert!(!rows.iter().any(|row| row.contains("Changed")), "{rows:#?}");
    screen.end_turn("interrupted");
    assert_eq!(
        screen.summary(),
        [
            "  ● Changed 1 file  +1 −1",
            "      a.rs  +1 −1",
            "  ■ Turn interrupted · 2s",
        ]
    );
    let folded = screen.summary();
    screen.ctrl_o();
    assert_eq!(screen.summary(), folded);

    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.change("completed", json!([update("a.rs", 1, 0)]));
    screen.end_turn("failed");
    assert_eq!(
        screen.summary(),
        [
            "  ● Changed 1 file  +1",
            "      a.rs  +1",
            "  ✗ Turn failed: no reason given · 2s",
        ]
    );
}
