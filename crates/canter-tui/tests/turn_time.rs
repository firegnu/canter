//! How long a turn worked, after it ends, and how long thinking took, from
//! the times the reducer fills in from the events' timestamps.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::Modifier;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const WIDTH: u16 = 60;
/// Rows below the conversation: status line, input box (3) and info rows (2).
const BELOW: usize = 6;
/// Unix milliseconds when the first turn starts.
const T0: i64 = 1_800_000_000_000;

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    /// The time of the next event.
    now: i64,
    turn: String,
}

impl Screen {
    fn new() -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, T0);
        let ui = Ui::new(TestBackend::new(WIDTH, 40), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            now: T0,
            turn: String::new(),
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

    /// Moves the clock on by `ms` for the events that follow.
    fn wait(&mut self, ms: i64) {
        self.now += ms;
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

    fn turn(&self, status: &str, error: Value) -> Value {
        json!({
            "id": self.turn, "items": [], "status": status, "error": error,
            "input": [{ "type": "text", "text": format!("prompt {}", self.turn) }],
            "clientName": "canter-tui", "usage": null,
        })
    }

    fn start_turn(&mut self, id: &str) {
        self.turn = id.to_owned();
        let turn = self.turn("inProgress", Value::Null);
        self.event(json!({ "type": "turn.started", "turn": turn }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn end_turn(&mut self, status: &str, error: Value) {
        let turn = self.turn(status, error);
        self.event(json!({ "type": "turn.completed", "turn": turn }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    fn say(&mut self, id: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": id, "text": text });
        self.event(json!({ "type": "item.started", "turnId": self.turn, "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": self.turn, "item": message }));
    }

    fn start_thinking(&mut self, id: &str) {
        self.event(
            json!({ "type": "item.started", "turnId": self.turn, "item": {
                "type": "reasoning", "id": id, "summary": [], "content": [],
            }}),
        );
    }

    fn think(&mut self, id: &str, delta: &str) {
        self.event(json!({
            "type": "item.delta", "turnId": self.turn, "itemId": id,
            "delta": { "type": "reasoningContent", "contentIndex": 0, "delta": delta },
        }));
    }

    fn thought(&mut self, id: &str, text: &str) {
        self.event(
            json!({ "type": "item.completed", "turnId": self.turn, "item": {
                "type": "reasoning", "id": id, "summary": [], "content": [text],
            }}),
        );
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

    /// The conversation's rows, above the status line.
    fn conversation(&mut self) -> Vec<String> {
        let buffer = self.draw();
        let lines: Vec<String> = (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect();
        lines[..lines.len() - BELOW].to_vec()
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

/// The cells of the first row containing `text`, from where it starts.
fn cells_of<'a>(buffer: &'a Buffer, text: &str) -> Vec<&'a Cell> {
    for y in 0..buffer.area.height {
        let row = row(buffer, y);
        if let Some(byte) = row.find(text) {
            let x = row[..byte].width() as u16;
            let end = x + text.width() as u16;
            return (x..end).map(|x| &buffer[(x, y)]).collect();
        }
    }
    panic!("{text:?} is not on screen");
}

/// Every non-blank cell of `text` is grey (dim).
fn grey(buffer: &Buffer, text: &str) -> bool {
    cells_of(buffer, text)
        .iter()
        .filter(|cell| cell.symbol() != " " && !cell.symbol().is_empty())
        .all(|cell| cell.modifier.contains(Modifier::DIM))
}

/// The row right after the last row containing `text`, skipping blank rows.
fn after<'a>(conversation: &'a [String], text: &str) -> &'a str {
    let at = conversation
        .iter()
        .rposition(|row| row.contains(text))
        .unwrap_or_else(|| panic!("{text:?} in {conversation:?}"));
    conversation[at + 1..]
        .iter()
        .find(|row| !row.is_empty())
        .map_or("", String::as_str)
}

/// A turn that ended normally gets a grey `─ Worked for … ─` row after its
/// last item, timed as the status line times a turn, `<1s` under a second.
/// A turn still running has none: the status line counts.
#[test]
fn a_finished_turn_ends_with_how_long_it_worked() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.wait(185_400);
    screen.say("m1", "First answer.");
    let running = screen.conversation();
    assert!(
        !running.iter().any(|row| row.contains("Worked for")),
        "{running:?}"
    );
    screen.end_turn("completed", Value::Null);
    let conversation = screen.conversation();
    assert_eq!(
        after(&conversation, "First answer."),
        "  ─ Worked for 3m 05s ─",
        "{conversation:?}"
    );
    assert!(grey(screen.draw(), "─ Worked for 3m 05s ─"));

    screen.wait(1_000);
    screen.start_turn("t2");
    screen.wait(400);
    screen.say("m2", "Second answer.");
    screen.end_turn("completed", Value::Null);
    screen.wait(1_000);
    screen.start_turn("t3");
    screen.wait(12_999);
    screen.say("m3", "Third answer.");
    screen.end_turn("completed", Value::Null);
    let conversation = screen.conversation();
    assert_eq!(
        after(&conversation, "Second answer."),
        "  ─ Worked for <1s ─",
        "{conversation:?}"
    );
    assert_eq!(
        after(&conversation, "Third answer."),
        "  ─ Worked for 12s ─",
        "{conversation:?}"
    );
}

/// An interrupted or failed turn keeps its row and adds ` · <time>` to it.
#[test]
fn an_interrupted_or_failed_turn_says_how_long_it_ran() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.wait(12_300);
    screen.say("m1", "Working on it.");
    screen.end_turn("interrupted", Value::Null);
    screen.wait(1_000);
    screen.start_turn("t2");
    screen.wait(61_000);
    screen.end_turn(
        "failed",
        json!({ "message": "boom", "additionalDetails": null }),
    );
    let conversation = screen.conversation();
    assert!(
        conversation.contains(&"  ■ Turn interrupted · 12s".to_owned()),
        "{conversation:?}"
    );
    assert!(
        conversation.contains(&"  ✗ Turn failed: boom · 1m 01s".to_owned()),
        "{conversation:?}"
    );
    assert!(!conversation.iter().any(|row| row.contains("Worked for")));
    assert!(grey(screen.draw(), "■ Turn interrupted · 12s"));
}

/// Finished thinking says how long it took, up to its last line:
/// `Thought for 4s: …` folded, `Thought for 4s` shown in full. Thinking
/// still going, and thinking with no known time, look as before.
#[test]
fn thinking_says_how_long_it_thought() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.wait(100);
    screen.start_thinking("r1");
    screen.think("r1", "Look at the README.");
    screen.wait(4_000);
    screen.think("r1", "\n\nThen edit it.");
    let thinking = screen.conversation();
    assert!(
        thinking.contains(&"  Thinking… Then edit it.".to_owned()),
        "{thinking:?}"
    );
    screen.wait(3_000);
    screen.say("m1", "Done.");
    screen.wait(2_000);
    screen.thought("r1", "Look at the README.\n\nThen edit it.");
    screen.thought("r2", "No start was seen for this one.");
    screen.end_turn("completed", Value::Null);
    let conversation = screen.conversation();
    for expected in [
        "  Thought for 4s: Look at the README.…",
        "  Thought: No start was seen for this one.",
    ] {
        assert!(
            conversation.contains(&expected.to_owned()),
            "{expected:?} in {conversation:?}"
        );
    }
    assert!(grey(screen.draw(), "Thought for 4s: Look at the README.…"));

    screen.ctrl_o();
    let details = screen.conversation();
    assert!(
        details.contains(&"  Thought for 4s".to_owned()),
        "{details:?}"
    );
    assert!(details.contains(&"  Thought".to_owned()), "{details:?}");
}
