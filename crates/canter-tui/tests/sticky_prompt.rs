//! Scrolled back past the prompt the rows in view answer, that prompt shows
//! on the top row, dim, cut to fit with `…`. A click on it puts the view on
//! the prompt and hides the row until the view scrolls again. It is not
//! part of the conversation: a selection does not take it.

use std::sync::mpsc;
use std::time::Duration;

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{App, Ui};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 1_000_000;
const WIDTH: u16 = 60;
const HEIGHT: u16 = 20;
/// Rows under the conversation: the status line, the input box (3) and the
/// info rows (2).
const BELOW: u16 = 6;
const WAIT: Duration = Duration::from_secs(5);

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    /// When frames are drawn and the mouse is used; each use moves it on,
    /// so no two clicks make a double click.
    now: i64,
    copied: mpsc::Receiver<String>,
}

impl Screen {
    fn new(height: u16, welcome: bool) -> Self {
        let (sender, copied) = mpsc::channel();
        let mut app = App::new(SessionSnapshot::empty(SESSION), None, NOW).with_clipboard_writer(
            move |text: &str| {
                let _ = sender.send(text.to_owned());
                Ok(())
            },
        );
        if welcome {
            app = app.with_welcome("0.1.0 · 37489fe");
        }
        let ui = Ui::new(TestBackend::new(WIDTH, height), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            now: NOW,
            copied,
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

    /// Two finished turns: `first question` answered with `a1 line 1` to
    /// `a1 line 30`, then `second question` with `a2 line 1` to
    /// `a2 line 30`, one row each.
    fn two_turns() -> Self {
        let mut screen = Self::new(HEIGHT, false);
        screen.turn("t1", "first question", "a1", 30);
        screen.turn("t2", "second question", "a2", 30);
        screen.draw();
        screen
    }

    fn turn(&mut self, id: &str, prompt: &str, answer: &str, lines: usize) {
        self.event(json!({ "type": "turn.started", "turn": turn_json(id, prompt, "inProgress") }));
        let text: Vec<String> = (1..=lines).map(|n| format!("{answer} line {n}")).collect();
        let message =
            json!({ "type": "agentMessage", "id": format!("m-{id}"), "text": text.join("\n") });
        self.event(json!({ "type": "item.started", "turnId": id, "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": id, "item": message }));
        self.event(json!({ "type": "turn.completed", "turn": turn_json(id, prompt, "completed") }));
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
        self.ui.draw(self.now).unwrap();
    }

    fn mouse(&mut self, kind: MouseEventKind, (column, row): (u16, u16)) {
        self.now += 1_000;
        self.ui.app_mut().handle_mouse_at(
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
            self.now,
        );
        self.draw();
    }

    fn wheel_up(&mut self, steps: usize) {
        for _ in 0..steps {
            self.mouse(MouseEventKind::ScrollUp, (10, 5));
        }
    }

    fn wheel_down(&mut self, steps: usize) {
        for _ in 0..steps {
            self.mouse(MouseEventKind::ScrollDown, (10, 5));
        }
    }

    fn click(&mut self, at: (u16, u16)) {
        self.mouse(MouseEventKind::Down(MouseButton::Left), at);
        self.mouse(MouseEventKind::Up(MouseButton::Left), at);
    }

    fn buffer(&self) -> &Buffer {
        self.ui.backend().buffer()
    }

    fn lines(&self) -> Vec<String> {
        let buffer = self.buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut row = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    row.push_str(symbol);
                    x += symbol.width().max(1) as u16;
                }
                row.trim_end().to_owned()
            })
            .collect()
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// The prompt row on top, when it shows: a dim `›` on the top row. A
    /// prompt in the conversation has a bold one.
    fn sticky(&self) -> Option<String> {
        let cell = &self.buffer()[(0, 0)];
        (cell.symbol() == "›" && cell.modifier.contains(Modifier::DIM))
            .then(|| self.lines()[0].clone())
    }

    /// The row above the status line: the hint while scrolled back.
    fn above_status(&self) -> String {
        let lines = self.lines();
        lines[lines.len() - usize::from(BELOW) - 1].clone()
    }
}

fn turn_json(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": text }],
        "clientName": "canter-tui", "usage": null,
    })
}

/// Scrolled back into the middle of an answer, its prompt shows on the top
/// row, dim, and the conversation goes on under it; further back, in the
/// answer before, that one's prompt.
#[test]
fn scrolled_back_into_an_answer_its_prompt_shows_on_top() {
    let mut screen = Screen::two_turns();
    screen.wheel_up(3);
    assert_eq!(
        screen.sticky().as_deref(),
        Some("› second question"),
        "{}",
        screen.text()
    );
    let row = &screen.buffer()[(2, 0)];
    assert!(row.modifier.contains(Modifier::DIM), "dim like other hints");
    assert!(
        screen.lines()[1].starts_with("  a2 line "),
        "{}",
        screen.text()
    );

    // Back into the first answer.
    screen.wheel_up(12);
    assert!(
        screen.lines()[1].starts_with("  a1 line "),
        "{}",
        screen.text()
    );
    assert_eq!(
        screen.sticky().as_deref(),
        Some("› first question"),
        "{}",
        screen.text()
    );
}

/// Following the latest output, nothing shows on top, though the prompt
/// is long out of view.
#[test]
fn following_the_latest_output_shows_no_prompt_on_top() {
    let screen = Screen::two_turns();
    assert_eq!(screen.sticky(), None, "{}", screen.text());
    assert!(
        screen.lines()[0].starts_with("  a2 line "),
        "{}",
        screen.text()
    );
}

/// While a row of the prompt is still on screen, nothing shows on top,
/// even with its first row out of view.
#[test]
fn a_prompt_still_on_screen_does_not_show_on_top() {
    let mut screen = Screen::new(HEIGHT, false);
    screen.turn(
        "t1",
        "long question\nmore 2\nmore 3\nmore 4\nmore 5",
        "a1",
        30,
    );
    screen.draw();
    // To the top, then one wheel step (three rows) down.
    screen.wheel_up(20);
    assert!(
        screen.lines()[0].starts_with("› long question"),
        "{}",
        screen.text()
    );
    assert_eq!(screen.sticky(), None);
    screen.wheel_down(1);
    assert_eq!(screen.lines()[0], "  more 4", "{}", screen.text());
    assert_eq!(screen.sticky(), None, "{}", screen.text());

    // One more step and all of it is out of view.
    screen.wheel_down(1);
    assert_eq!(
        screen.sticky().as_deref(),
        Some("› long question"),
        "{}",
        screen.text()
    );
}

/// Over the welcome header, before any prompt, nothing shows on top.
#[test]
fn over_the_welcome_header_no_prompt_shows_on_top() {
    let mut screen = Screen::new(HEIGHT, true);
    screen.turn("t1", "first question", "a1", 30);
    screen.draw();
    screen.wheel_up(20);
    assert!(
        screen.text().contains("canter 0.1.0"),
        "the welcome header:\n{}",
        screen.text()
    );
    assert_eq!(screen.sticky(), None);
    // Into the header: rows of it are above the view, the prompt is below.
    screen.wheel_down(1);
    assert!(!screen.lines()[0].contains("first question"));
    assert_eq!(screen.sticky(), None, "{}", screen.text());
}

/// A prompt wider than the window is cut to fit, by display width, with
/// `…` at the end.
#[test]
fn a_long_prompt_is_cut_with_an_ellipsis() {
    let long = "解释".repeat(40);
    let mut screen = Screen::new(HEIGHT, false);
    screen.turn("t1", &long, "a1", 30);
    screen.draw();
    screen.wheel_up(3);
    let sticky = screen
        .sticky()
        .unwrap_or_else(|| panic!("the prompt on top:\n{}", screen.text()));
    // `› `, then as many characters as leave a column for `…`.
    let kept: String = long.chars().take(28).collect();
    assert_eq!(sticky, format!("› {kept}…"));
    assert!(sticky.width() <= usize::from(WIDTH));
}

/// A click on the prompt on top puts the view on that prompt, still
/// scrolled back, and hides the row; scrolling shows it again.
#[test]
fn clicking_the_prompt_on_top_scrolls_back_to_it_until_the_view_scrolls() {
    let mut screen = Screen::two_turns();
    screen.wheel_up(3);
    assert_eq!(screen.sticky().as_deref(), Some("› second question"));

    screen.click((5, 0));
    assert_eq!(screen.sticky(), None, "{}", screen.text());
    assert_eq!(
        screen.lines()[0],
        "› second question",
        "the prompt on the top row:\n{}",
        screen.text()
    );
    assert_eq!(
        screen.above_status(),
        " ↓ Jump to bottom · click or End",
        "still scrolled back"
    );

    // Another frame, and the row stays hidden.
    screen.draw();
    assert_eq!(screen.sticky(), None);

    // The wheel scrolls: the prompt is out of view and shows on top again.
    screen.wheel_down(1);
    assert_eq!(
        screen.sticky().as_deref(),
        Some("› second question"),
        "{}",
        screen.text()
    );
}

/// Dragging over the conversation under the prompt on top copies the rows
/// dragged over, not the prompt.
#[test]
fn a_selection_does_not_take_the_prompt_on_top() {
    let mut screen = Screen::two_turns();
    screen.wheel_up(3);
    assert_eq!(screen.sticky().as_deref(), Some("› second question"));
    let lines = screen.lines();
    let expected: Vec<&str> = lines[1..=3].iter().map(|line| line.trim()).collect();

    screen.mouse(MouseEventKind::Down(MouseButton::Left), (0, 1));
    screen.mouse(MouseEventKind::Drag(MouseButton::Left), (20, 2));
    screen.mouse(MouseEventKind::Drag(MouseButton::Left), (59, 3));
    screen.mouse(MouseEventKind::Up(MouseButton::Left), (59, 3));
    let copied = screen
        .copied
        .recv_timeout(WAIT)
        .unwrap_or_else(|_| panic!("nothing was copied:\n{}", screen.text()));
    assert_eq!(copied, expected.join("\n"));
    assert!(!copied.contains("question"));
}

/// When the window leaves the conversation two rows or fewer, nothing
/// shows on top; with three, it does.
#[test]
fn a_short_window_shows_no_prompt_on_top() {
    // The conversation's rows with the hint under them: 9 rows leave 3, the
    // hint takes 1.
    let mut screen = Screen::new(BELOW + 3, false);
    screen.turn("t1", "first question", "a1", 30);
    screen.draw();
    screen.wheel_up(3);
    assert_eq!(screen.sticky(), None, "{}", screen.text());
    assert!(screen.lines()[0].starts_with("  a1 line "));

    let mut screen = Screen::new(BELOW + 4, false);
    screen.turn("t1", "first question", "a1", 30);
    screen.draw();
    screen.wheel_up(3);
    assert_eq!(
        screen.sticky().as_deref(),
        Some("› first question"),
        "{}",
        screen.text()
    );
}
