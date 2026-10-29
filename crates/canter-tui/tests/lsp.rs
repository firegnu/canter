//! LSP diagnostics in the conversation (P2-135): a line under the file
//! change card, the entries with Ctrl+O, a separate `LSP update` for
//! late results. The export is checked in `export.rs`.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Color;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new() -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, 0);
        let ui = Ui::new(TestBackend::new(90, 40), app).unwrap();
        let mut screen = Self { ui, seq: 0 };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen.event(json!({ "type": "turn.started", "turn": {
            "id": "t1", "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": "do it" }],
            "clientName": "canter-tui", "usage": null,
        }}));
        screen.event(json!({ "type": "session.status", "status": "working" }));
        screen
    }

    fn event(&mut self, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": 0 });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, 0);
    }

    fn completed(&mut self, item: Value) {
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": item }));
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": item }));
    }

    fn edit(&mut self, id: &str, path: &str) {
        self.completed(json!({
            "type": "fileChange", "id": id, "status": "completed",
            "changes": [{ "path": path, "kind": { "type": "update", "movePath": null },
                          "diff": "@@ -1 +1 @@\n-old\n+new\n" }],
        }));
    }

    fn message(&mut self, id: &str, text: &str) {
        self.completed(json!({ "type": "agentMessage", "id": id, "text": text }));
    }

    fn ctrl_o(&mut self) {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL), 0);
    }

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(0).unwrap();
        self.ui.backend().buffer()
    }

    fn rows(&mut self) -> Vec<String> {
        let buffer = self.draw();
        (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect()
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

/// The colour of the first cell of `text` on screen.
fn colour_of(buffer: &Buffer, text: &str) -> Color {
    for y in 0..buffer.area.height {
        let row = row(buffer, y);
        if let Some(byte) = row.find(text) {
            return buffer[(row[..byte].width() as u16, y)].fg;
        }
    }
    panic!("{text:?} is not on screen");
}

fn at(rows: &[String], text: &str) -> usize {
    rows.iter()
        .position(|row| row.contains(text))
        .unwrap_or_else(|| panic!("{text:?} in {rows:#?}"))
}

fn file(path: &str, freshness: &str, diagnostics: Value) -> Value {
    json!({
        "batchId": 1, "path": path, "serverId": "lsp-1", "source": "push",
        "collectedAt": 1, "documentVersion": 2, "freshness": freshness,
        "incomplete": false, "diagnostics": diagnostics,
    })
}

fn entry(
    severity: &str,
    line: u32,
    character: u32,
    source: Value,
    code: Value,
    message: &str,
) -> Value {
    let mut entry = json!({
        "severity": severity,
        "range": { "start": { "line": line, "character": character },
                   "end": { "line": line, "character": character + 1 } },
        "message": message,
    });
    if !source.is_null() {
        entry["source"] = source;
    }
    if !code.is_null() {
        entry["code"] = code;
    }
    entry
}

fn diagnostics(
    id: &str,
    source: &str,
    status: &str,
    files: Value,
    counts: Option<(u32, u32)>,
    delivery: Option<&str>,
) -> Value {
    let total = counts.map_or(3, |(e, w)| e + w);
    let mut item = json!({
        "type": "diagnostics", "id": id, "sourceItemId": source, "status": status,
        "files": files, "totalCount": total, "shownCount": 1, "omittedCount": 0,
    });
    if let Some((errors, warnings)) = counts {
        item["errorCount"] = json!(errors);
        item["warningCount"] = json!(warnings);
    }
    if let Some(delivery) = delivery {
        item["delivery"] = json!(delivery);
    }
    item
}

/// `LSP: 2 errors, 1 warning` right under the card, the count in the failure
/// colour; Ctrl+O lists the entries one-based.
#[test]
fn the_card_gets_one_line_of_diagnostics_and_ctrl_o_lists_them() {
    let mut screen = Screen::new();
    screen.edit("e1", "/work/src/main.rs");
    let entries = json!([
        entry(
            "error",
            2,
            4,
            json!("rustc"),
            json!("E0308"),
            "mismatched types"
        ),
        entry("warning", 9, 0, Value::Null, Value::Null, "unused variable"),
    ]);
    screen.completed(diagnostics(
        "d1",
        "e1",
        "available",
        json!([file("src/main.rs", "current", entries)]),
        Some((2, 1)),
        Some("inline"),
    ));
    let rows = screen.rows();
    let card = at(&rows, "Edit src/main.rs");
    let line = at(&rows, "LSP: 2 errors, 1 warning");
    assert_eq!(rows[line], "    LSP: 2 errors, 1 warning");
    assert!(line > card);
    assert!(
        rows[card..line].iter().all(|row| !row.is_empty()),
        "{rows:#?}"
    );
    let buffer = screen.draw();
    let red = colour_of(buffer, "2 errors");
    assert_ne!(red, colour_of(buffer, "LSP:"));
    screen.ctrl_o();
    let rows = screen.rows();
    assert!(
        rows.iter()
            .any(|row| row.trim() == "src/main.rs:3:5 error [rustc/E0308] mismatched types"),
        "{rows:#?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.trim() == "src/main.rs:10:1 warning unused variable"),
        "{rows:#?}"
    );
}

/// The statuses and freshness the task names, each under its own card.
#[test]
fn pending_unavailable_and_freshness_read_as_written() {
    let mut screen = Screen::new();
    let status = |path: &str, reason: &str| {
        json!([{ "batchId": 0, "path": path, "serverId": "", "source": "status",
                 "freshness": "stale", "incomplete": true, "reason": reason, "diagnostics": [] }])
    };
    screen.edit("e1", "/work/a.rs");
    screen.completed(diagnostics(
        "d1",
        "e1",
        "pending",
        status("/work/a.rs", "pending"),
        None,
        Some("inline"),
    ));
    screen.edit("e2", "/work/b.rs");
    screen.completed(diagnostics(
        "d2",
        "e2",
        "unavailable",
        status("/work/b.rs", "rust-analyzer is not installed"),
        None,
        Some("inline"),
    ));
    screen.edit("e3", "/work/c.rs");
    screen.completed(diagnostics(
        "d3",
        "e3",
        "available",
        json!([file("c.rs", "stale", json!([]))]),
        Some((0, 0)),
        Some("inline"),
    ));
    screen.edit("e4", "/work/d.rs");
    screen.completed(diagnostics(
        "d4",
        "e4",
        "available",
        json!([file("d.rs", "versionUnconfirmed", json!([]))]),
        Some((0, 1)),
        Some("inline"),
    ));
    screen.edit("e5", "/work/e.rs");
    screen.completed(diagnostics(
        "d5",
        "e5",
        "available",
        json!([file("e.rs", "deleted", json!([]))]),
        Some((0, 0)),
        Some("inline"),
    ));
    let rows = screen.rows();
    for line in [
        "    LSP: pending",
        "    LSP: unavailable",
        "    LSP: no errors or warnings · may be stale",
        "    LSP: 1 warning · version unconfirmed",
        "    LSP: file deleted",
    ] {
        assert!(rows.iter().any(|row| row == line), "{line:?} in {rows:#?}");
    }
    screen.ctrl_o();
    let rows = screen.rows();
    assert!(
        rows.iter()
            .any(|row| row.trim() == "rust-analyzer is not installed"),
        "{rows:#?}"
    );
}

/// Late results are their own entry; an item saved before the counts
/// existed gives only the total, and one saved before `delivery` existed
/// is placed by where it is.
#[test]
fn late_results_are_a_separate_entry_and_old_items_still_show() {
    let mut screen = Screen::new();
    screen.edit("e1", "/work/src/main.rs");
    screen.completed(diagnostics(
        "d1",
        "e1",
        "available",
        json!([file("src/main.rs", "current", json!([]))]),
        None,
        None,
    ));
    screen.message("m1", "Done editing.");
    screen.completed(diagnostics(
        "d2",
        "e1",
        "available",
        json!([file(
            "src/main.rs",
            "current",
            json!([entry("error", 0, 0, Value::Null, json!("E1"), "bad")])
        )]),
        Some((1, 0)),
        Some("late"),
    ));
    let rows = screen.rows();
    assert!(
        rows.iter()
            .any(|row| row == "    LSP: 3 errors or warnings"),
        "{rows:#?}"
    );
    let late = at(&rows, "LSP update: src/main.rs · 1 error");
    assert_eq!(rows[late], "  LSP update: src/main.rs · 1 error");
    assert!(rows[late - 1].is_empty(), "{rows:#?}");
}

/// Without a diagnostics item the card is exactly as before.
#[test]
fn a_card_without_diagnostics_is_unchanged() {
    let mut screen = Screen::new();
    screen.edit("e1", "/work/src/main.rs");
    let rows = screen.rows();
    assert!(!rows.iter().any(|row| row.contains("LSP")), "{rows:#?}");
}
