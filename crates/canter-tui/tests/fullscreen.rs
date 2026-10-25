//! The full-screen layout on ratatui's TestBackend: the conversation from the
//! top of the screen, the status line, the input box and the info line pinned
//! to the bottom, scrolling back with the mouse wheel and PgUp/PgDn, and
//! rewrapping when the window changes size.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
const WIDTH: u16 = 60;
const HEIGHT: u16 = 20;
/// Rows below the conversation: status line, input box (3) and info line.
const BELOW: usize = 5;
const HINT: &str = "↓ New output below · scroll down or press PgDn";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new() -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let ui = Ui::new(TestBackend::new(WIDTH, HEIGHT), app).unwrap();
        let mut screen = Self { ui, seq: 0 };
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

    fn start_turn(&mut self, turn: &str, text: &str) {
        self.event(json!({ "type": "turn.started", "turn": turn_json(turn, text, "inProgress") }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn finish_turn(&mut self, turn: &str, text: &str) {
        self.event(json!({ "type": "turn.completed", "turn": turn_json(turn, text, "completed") }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    /// The model writes `text` as a finished message.
    fn say(&mut self, turn: &str, item: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": item, "text": text });
        self.event(json!({ "type": "item.started", "turnId": turn, "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": turn, "item": message }));
    }

    /// A turn still in progress whose messages are `line 1` to `line {count}`.
    fn long_turn(count: usize) -> Self {
        let mut screen = Self::new();
        screen.start_turn("t1", "show many lines");
        for n in 1..=count {
            screen.say("t1", &format!("m{n}"), &format!("line {n}"));
        }
        screen.draw();
        screen
    }

    fn draw(&mut self) {
        self.ui.draw(NOW).unwrap();
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), NOW)
    }

    fn wheel(&mut self, kind: MouseEventKind) {
        self.ui.app_mut().handle_mouse(MouseEvent {
            kind,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
    }

    fn resize(&mut self, width: u16, height: u16) {
        self.ui.backend_mut().resize(width, height);
    }

    fn lines(&self) -> Vec<String> {
        rows(self.ui.backend().buffer())
    }

    /// The conversation's rows: everything above the status line, and above
    /// the hint when it shows.
    fn conversation(&self) -> Vec<String> {
        let lines = self.lines();
        let mut end = lines.len() - BELOW;
        if lines[end - 1].contains(HINT) {
            end -= 1;
        }
        lines[..end].to_vec()
    }

    /// The row just above the status line.
    fn above_status(&self) -> String {
        let lines = self.lines();
        lines[lines.len() - BELOW - 1].clone()
    }
}

fn turn_json(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": text }],
        "clientName": "canter-tui", "usage": null,
    })
}

/// The buffer's rows, trailing spaces trimmed. Cells hidden under a wide
/// character are skipped.
fn rows(buffer: &Buffer) -> Vec<String> {
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

fn screen_text(lines: &[String]) -> String {
    lines.join("\n")
}

/// Less than a screen: the conversation starts on the top row, the input box
/// sits right above the info line on the bottom row, blank rows between.
#[test]
fn a_short_conversation_starts_at_the_top_with_the_input_box_at_the_bottom() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "first question");
    screen.say("t1", "m1", "short answer");
    screen.finish_turn("t1", "first question");
    screen.draw();
    let lines = screen.lines();
    let shown = screen_text(&lines);
    let last = usize::from(HEIGHT) - 1;

    assert_eq!(lines[0], "› first question", "from the top:\n{shown}");
    assert_eq!(lines[2], "  short answer", "{shown}");
    assert_eq!(
        lines[last], " test-model · Always ask · /work",
        "the info line is the bottom row:\n{shown}"
    );
    assert!(lines[last - 3].starts_with('╭'), "{shown}");
    assert!(lines[last - 2].starts_with("│ › Type a message"), "{shown}");
    assert!(
        lines[last - 1].starts_with('╰'),
        "the input box is right above the info line:\n{shown}"
    );
    assert!(
        lines[3..last - 3].iter().all(String::is_empty),
        "blank between:\n{shown}"
    );
}

/// More than a screen: the latest output is the row right above the status
/// line, and the top rows are the ones that do not fit.
#[test]
fn a_long_conversation_follows_the_latest_output() {
    let mut screen = Screen::long_turn(40);
    let shown = screen_text(&screen.lines());
    assert_eq!(screen.above_status(), "  line 40", "{shown}");
    assert!(
        screen.lines()[usize::from(HEIGHT) - BELOW].starts_with(" ⠋ Working"),
        "{shown}"
    );
    let area = usize::from(HEIGHT) - BELOW;
    assert_eq!(
        screen.lines()[0],
        format!("  line {}", 40 - area + 1),
        "{shown}"
    );

    // New output scrolls the conversation along.
    screen.say("t1", "m41", "line 41");
    screen.draw();
    assert_eq!(screen.above_status(), "  line 41");
}

/// After scrolling up with the wheel, new output does not move the view; a
/// hint above the status line says there is more below. Scrolling back down
/// to the bottom follows the latest output again.
#[test]
fn the_wheel_scrolls_back_and_new_output_does_not_pull_the_view_down() {
    let mut screen = Screen::long_turn(40);
    screen.wheel(MouseEventKind::ScrollUp);
    screen.draw();
    assert_eq!(
        screen.above_status(),
        "  line 37",
        "one wheel step scrolls three rows"
    );
    let before = screen.conversation();

    screen.say("t1", "m41", "line 41");
    screen.draw();
    let shown = screen_text(&screen.lines());
    assert_eq!(
        screen.above_status().trim(),
        HINT,
        "the hint is right above the status line:\n{shown}"
    );
    let after = screen.conversation();
    assert_eq!(
        after[..],
        before[..after.len()],
        "the view stayed:\n{shown}"
    );
    assert!(!shown.contains("line 41"), "{shown}");

    for _ in 0..3 {
        screen.wheel(MouseEventKind::ScrollDown);
        screen.draw();
    }
    let shown = screen_text(&screen.lines());
    assert_eq!(screen.above_status(), "  line 41", "{shown}");
    assert!(!shown.contains(HINT), "{shown}");

    // Following again: more output comes into view.
    screen.say("t1", "m42", "line 42");
    screen.draw();
    assert_eq!(screen.above_status(), "  line 42");
}

/// PgUp scrolls a page; new output shows the hint; sending a message follows
/// the latest output again.
#[test]
fn page_up_then_sending_a_message_follows_again() {
    let mut screen = Screen::long_turn(40);
    let area = usize::from(HEIGHT) - BELOW;
    assert_eq!(screen.key(KeyCode::PageUp), None);
    screen.draw();
    assert_eq!(screen.above_status(), format!("  line {}", 40 - area));

    screen.say("t1", "m41", "line 41");
    screen.draw();
    assert_eq!(screen.above_status().trim(), HINT);

    // PgDn scrolls back a page, not past the end.
    assert_eq!(screen.key(KeyCode::PageUp), None);
    assert_eq!(screen.key(KeyCode::PageDown), None);
    screen.draw();
    assert_eq!(screen.above_status().trim(), HINT, "still scrolled up");

    for c in "next".chars() {
        assert_eq!(screen.key(KeyCode::Char(c)), None);
    }
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::Send("next".into()))
    );
    screen.draw();
    let shown = screen_text(&screen.lines());
    assert_eq!(screen.above_status(), "  line 41", "{shown}");
    assert!(!shown.contains(HINT), "{shown}");
}

/// When the window changes size, the whole conversation wraps again at the
/// new width, including what was already finished.
#[test]
fn resizing_rewraps_the_whole_conversation() {
    let words: Vec<String> = (1..=25).map(|n| format!("word{n:02}")).collect();
    let message = words.join(" ");
    let question = format!("please repeat {message}");
    let mut screen = Screen::new();
    screen.start_turn("t1", &question);
    screen.say("t1", "m1", &message);
    screen.finish_turn("t1", &question);
    screen.draw();

    for width in [40u16, 100, 60] {
        screen.resize(width, HEIGHT);
        screen.draw();
        let conversation = screen.conversation();
        let shown = screen_text(&conversation);
        let text: Vec<&str> = conversation
            .iter()
            .flat_map(|row| row.split_whitespace())
            .filter(|word| *word != "›")
            .collect();
        let expected: Vec<&str> = question.split(' ').chain(message.split(' ')).collect();
        assert_eq!(text, expected, "width {width}, every word once:\n{shown}");
        // Each row is full: the next word would not have fitted.
        for pair in conversation.windows(2) {
            let (row, next) = (&pair[0], &pair[1]);
            if let Some(word) = next.split_whitespace().next()
                && !row.is_empty()
                && word != "›"
            {
                assert!(
                    row.width() + 1 + word.width() > usize::from(width),
                    "width {width}: {word} fits on {row:?}:\n{shown}"
                );
            }
        }
    }
}
