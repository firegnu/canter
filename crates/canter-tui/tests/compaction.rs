//! Compacting the context, automatic or `/compact`: while it runs the status
//! line says `Compacting context` with the light sweeping it, and the
//! conversation says `Compacting context…` without one; then the
//! conversation says it compacted, or that it did not finish.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{App, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::Modifier;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
/// A whole number of two-second sweeps: the light is off the verb's left end.
const NOW: i64 = 1_000_000;
const WIDTH: u16 = 80;
const HEIGHT: u16 = 20;
/// Rows under the conversation: the status line, the input box (3) and the
/// info rows (2).
const BELOW: u16 = 6;
const COMPACTING: &str = "  ── Compacting context… ──";
const COMPACTED: &str = "  ── Context compacted ──";
const NOT_FINISHED: &str = "  ── Compaction did not finish; the original context is kept ──";
/// What the core puts in the summary when compaction did not finish.
const KEPT: &str = "Compaction did not finish; the original context is kept.";

const DARK: Theme = Theme {
    depth: Depth::TrueColor,
    background: Background::Dark,
};

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new() -> Self {
        theme::set(DARK);
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(WIDTH, HEIGHT), app).unwrap(),
            seq: 0,
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
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": NOW });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, NOW);
    }

    /// A turn in progress that has started compacting.
    fn compacting() -> Self {
        let mut screen = Self::new();
        screen.event(json!({ "type": "turn.started", "turn": turn("inProgress") }));
        screen.event(json!({ "type": "session.status", "status": "working" }));
        screen.event(json!({
            "type": "item.started", "turnId": "t1", "item": compaction(""),
        }));
        screen
    }

    fn complete(&mut self, summary: &str) {
        self.event(json!({
            "type": "item.completed", "turnId": "t1", "item": compaction(summary),
        }));
    }

    fn end_turn(&mut self, status: &str) {
        self.event(json!({ "type": "turn.completed", "turn": turn(status) }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    fn draw_at(&mut self, now: i64) {
        self.ui.draw(now).unwrap();
    }

    fn rows(&self) -> Vec<String> {
        (0..HEIGHT).map(|y| row(self.buffer(), y)).collect()
    }

    fn text(&self) -> String {
        self.rows().join("\n")
    }

    /// The conversation's rows, without trailing spaces.
    fn conversation(&self) -> Vec<String> {
        self.rows()[..usize::from(HEIGHT - BELOW)]
            .iter()
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    fn status(&self) -> String {
        row(self.buffer(), HEIGHT - BELOW).trim().to_owned()
    }

    fn buffer(&self) -> &Buffer {
        self.ui.backend().buffer()
    }

    /// The cells of `text` where it first starts on row `y`.
    fn cells(&self, y: u16, text: &str) -> Vec<Cell> {
        let line = row(self.buffer(), y);
        let start = line
            .find(text)
            .unwrap_or_else(|| panic!("{text:?} is not on row {y}: {line:?}"));
        let x = line[..start].width() as u16;
        (0..text.chars().count() as u16)
            .map(|i| self.buffer()[(x + i, y)].clone())
            .collect()
    }

    fn row_of(&self, text: &str) -> u16 {
        self.rows()
            .iter()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text:?} is not on screen:\n{}", self.text())) as u16
    }
}

fn turn(status: &str) -> Value {
    json!({
        "id": "t1", "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "go" }],
        "clientName": "canter-tui", "usage": null,
    })
}

fn compaction(summary: &str) -> Value {
    json!({ "type": "contextCompaction", "id": "k1", "summary": summary })
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

fn bold(cells: &[Cell]) -> String {
    cells
        .iter()
        .filter(|cell| cell.modifier.contains(Modifier::BOLD))
        .map(Cell::symbol)
        .collect()
}

/// While compacting, the status line says `Compacting context`, the light
/// sweeping both words like it sweeps `Thinking`; the conversation says
/// `Compacting context…`, the same at any time, and not that it compacted.
#[test]
fn while_compacting_both_lines_say_so_and_only_the_status_line_shimmers() {
    let mut screen = Screen::compacting();

    screen.draw_at(NOW + 1000);
    assert!(
        screen
            .status()
            .ends_with("Compacting context · 1s · Esc to interrupt"),
        "{}",
        screen.text()
    );
    assert!(
        screen.conversation().iter().any(|row| row == COMPACTING),
        "{}",
        screen.text()
    );
    assert!(!screen.text().contains("Context compacted"));

    // The light is in the middle of the verb: on the end of one word and
    // the start of the other.
    let status = HEIGHT - BELOW;
    let lit = bold(&screen.cells(status, "Compacting context"));
    assert!(lit.contains('g') && lit.contains('c'), "{lit:?}");
    // Away from the light, none of it is.
    screen.draw_at(NOW);
    assert_eq!(bold(&screen.cells(status, "Compacting context")), "");

    let y = screen.row_of(COMPACTING.trim());
    let before = screen.cells(y, COMPACTING.trim());
    for now in [NOW + 500, NOW + 1000, NOW + 1500] {
        screen.draw_at(now);
        assert_eq!(screen.cells(y, COMPACTING.trim()), before, "at {now}");
    }
    assert_eq!(bold(&before), "");
}

/// Once compacted, the conversation says so; an automatic compaction goes
/// on in the same turn, and the status line goes back to what it does next.
#[test]
fn once_compacted_the_conversation_says_so_and_the_turn_goes_on() {
    let mut screen = Screen::compacting();
    screen.complete("the summary");
    screen.event(json!({
        "type": "item.started", "turnId": "t1",
        "item": { "type": "reasoning", "id": "r1", "summary": [], "content": [] },
    }));
    screen.draw_at(NOW + 1000);
    assert!(
        screen
            .status()
            .ends_with("Thinking · 1s · Esc to interrupt"),
        "{}",
        screen.text()
    );
    let conversation = screen.conversation();
    assert!(
        conversation.iter().any(|row| row == COMPACTED),
        "{}",
        screen.text()
    );
    assert!(!screen.text().contains("Compacting"), "{}", screen.text());

    // `/compact` is a turn of its own that ends there.
    let mut screen = Screen::compacting();
    screen.complete("the summary");
    screen.end_turn("completed");
    screen.draw_at(NOW + 1000);
    assert!(
        screen.conversation().iter().any(|row| row == COMPACTED),
        "{}",
        screen.text()
    );
    assert!(!screen.text().contains("Compacting"), "{}", screen.text());
}

/// A compaction that did not finish, interrupted with Esc or failed, says
/// so and never that it compacted; so does one whose turn ended before it
/// was completed.
#[test]
fn a_compaction_that_did_not_finish_says_so() {
    for (summary, status) in [
        (Some(KEPT), "failed"),
        (Some(KEPT), "interrupted"),
        (None, "interrupted"),
    ] {
        let mut screen = Screen::compacting();
        if let Some(summary) = summary {
            screen.complete(summary);
        }
        screen.end_turn(status);
        screen.draw_at(NOW + 1000);
        let text = screen.text();
        assert!(
            screen.conversation().iter().any(|row| row == NOT_FINISHED),
            "{summary:?} {status}:\n{text}"
        );
        assert!(!text.contains("Context compacted"), "{status}:\n{text}");
        assert!(!text.contains("Compacting"), "{status}:\n{text}");
    }
}
