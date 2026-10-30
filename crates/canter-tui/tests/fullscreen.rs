//! The full-screen layout on ratatui's TestBackend: the conversation from the
//! top of the screen, the status line, the input box and the info line pinned
//! to the bottom, scrolling back with the mouse wheel and PgUp/PgDn, and
//! rewrapping when the window changes size.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
const WIDTH: u16 = 60;
const HEIGHT: u16 = 20;
/// Rows below the conversation: status line, input box (3) and info rows (2).
const BELOW: usize = 6;
/// The hint while scrolled back, with nothing new below and with new output.
const JUMP: &str = " ↓ Jump to bottom · click or End";
const NEW: &str = " ↓ New output below · click or End";

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

    /// The model wants to run `command` and asks first: the approval box opens.
    fn ask(&mut self, turn: &str, command: &str) {
        let item = json!({
            "type": "commandExecution", "id": "c1", "command": command, "cwd": "/work",
            "status": "inProgress", "aggregatedOutput": "", "exitCode": null, "durationMs": null,
        });
        self.event(json!({ "type": "item.started", "turnId": turn, "item": item }));
        self.event(json!({
            "type": "request.opened",
            "request": {
                "id": "r1", "turnId": turn, "createdAt": NOW,
                "kind": {
                    "type": "approval", "itemId": "c1", "tool": "shell",
                    "arguments": { "command": command },
                    "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
                },
            },
        }));
        self.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    }

    /// A turn still in progress with one message whose lines are `line 1`
    /// to `line {count}`, one row each.
    fn long_turn(count: usize) -> Self {
        let mut screen = Self::new();
        screen.start_turn("t1", "show many lines");
        let lines: Vec<String> = (1..=count).map(|n| format!("line {n}")).collect();
        screen.say("t1", "m0", &lines.join("\n"));
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

    /// A left click on row `row`.
    fn click(&mut self, row: u16) {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            self.ui.app_mut().handle_mouse(MouseEvent {
                kind,
                column: 5,
                row,
                modifiers: KeyModifiers::NONE,
            });
        }
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
        if lines[end - 1].starts_with(" ↓ ") {
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
/// sits right above the two info rows at the bottom, blank rows between.
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
        lines[last - 1..],
        [" test-model · think high · Always ask", " /work"],
        "the info rows are the bottom rows:\n{shown}"
    );
    assert!(lines[last - 4].starts_with('╭'), "{shown}");
    assert!(lines[last - 3].starts_with("│ › Type a message"), "{shown}");
    assert!(
        lines[last - 2].starts_with('╰'),
        "the input box is right above the info rows:\n{shown}"
    );
    assert_eq!(lines[4], "  ─ Worked for <1s ─", "{shown}");
    assert!(
        lines[5..last - 4].iter().all(String::is_empty),
        "blank between:\n{shown}"
    );
}

#[test]
fn info_line_shows_total_and_last_cache_hit_rates_only_with_input_usage() {
    let mut screen = Screen::new();
    screen.resize(100, HEIGHT);
    screen.draw();
    let last = usize::from(HEIGHT) - 1;
    let base = " /work";
    assert_eq!(screen.lines()[last], base);

    for (total_input, total_cached, last_input, last_cached, suffix) in [
        (1000, 924, 100, 99, " · cache 92% (last 99%)"),
        (3, 2, 3, 1, " · cache 67% (last 33%)"),
        (100, 0, 10, 0, " · cache 0% (last 0%)"),
        (100, 92, 0, 0, " · cache 92%"),
        (0, 0, 100, 99, " · cache (last 99%)"),
        (0, 0, 0, 0, ""),
    ] {
        let counts = |input, cached| {
            json!({
                "inputTokens": input, "cachedInputTokens": cached,
                "outputTokens": 0, "reasoningOutputTokens": 0, "totalTokens": input,
            })
        };
        screen.event(json!({
            "type": "session.usage",
            "tokenUsage": {
                "total": counts(total_input, total_cached),
                "last": counts(last_input, last_cached),
                "contextTokens": 0, "modelContextWindow": null,
            },
        }));
        screen.draw();
        assert_eq!(screen.lines()[last], format!("{base}{suffix}"));
        // The percentages cyan, the words around them dim.
        let buffer = screen.ui.backend().buffer();
        for x in base.width() as u16..(base.width() + suffix.width()) as u16 {
            let cell = &buffer[(x, HEIGHT - 1)];
            let number = cell
                .symbol()
                .chars()
                .all(|c| c.is_ascii_digit() || c == '%');
            if number {
                assert_eq!(cell.style().fg, Some(Color::Cyan), "column {x}");
            } else {
                assert_eq!(cell.style().fg, Some(Color::Reset), "column {x}");
                assert!(
                    cell.style().add_modifier.contains(Modifier::DIM),
                    "column {x}"
                );
            }
        }
    }
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

/// After scrolling up with the wheel, new output does not move the view; the
/// hint above the status line says there is more below. Scrolling back down
/// to the bottom follows the latest output again.
#[test]
fn the_wheel_scrolls_back_and_new_output_does_not_pull_the_view_down() {
    let mut screen = Screen::long_turn(40);
    let area = usize::from(HEIGHT) - BELOW;
    screen.wheel(MouseEventKind::ScrollUp);
    screen.draw();
    // The prompt, scrolled out of view, shows on the top row.
    assert_eq!(screen.lines()[0], "› show many lines");
    assert_eq!(
        screen.lines()[1],
        format!("  line {}", 40 - area + 1 - 3),
        "one wheel step scrolls three rows"
    );
    let before = screen.conversation();

    screen.say("t1", "m41", "line 41");
    screen.draw();
    let shown = screen_text(&screen.lines());
    assert_eq!(
        screen.above_status(),
        NEW,
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
    assert!(!shown.contains("↓"), "{shown}");

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
    // The prompt, scrolled out of view, shows on the top row.
    assert_eq!(screen.lines()[0], "› show many lines");
    assert_eq!(screen.lines()[1], format!("  line {}", 40 - 2 * area + 1));
    assert_eq!(screen.above_status(), JUMP);

    screen.say("t1", "m41", "line 41");
    screen.draw();
    assert_eq!(screen.above_status(), NEW);

    // PgDn scrolls back a page, not past the end.
    assert_eq!(screen.key(KeyCode::PageUp), None);
    assert_eq!(screen.key(KeyCode::PageDown), None);
    screen.draw();
    assert_eq!(screen.above_status(), NEW, "still scrolled up");

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
    assert!(!shown.contains("↓"), "{shown}");
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
        let worked = ["─", "Worked", "for", "<1s", "─"];
        let expected: Vec<&str> = question
            .split(' ')
            .chain(message.split(' '))
            .chain(worked)
            .collect();
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

/// While the approval box is open, End does not go to the bottom, so the
/// hint only tells to click.
#[test]
fn with_the_approval_box_open_the_hint_does_not_mention_end() {
    let mut screen = Screen::long_turn(40);
    screen.wheel(MouseEventKind::ScrollUp);
    screen.draw();

    screen.ask("t1", "cargo test");
    screen.draw();
    let lines = screen.lines();
    let shown = screen_text(&lines);
    assert!(shown.contains("Run this command?"), "{shown}");
    let hint = lines
        .iter()
        .find(|row| row.contains("New output below"))
        .unwrap_or_else(|| panic!("no hint:\n{shown}"));
    assert_eq!(hint, " ↓ New output below · click", "{shown}");
}

/// Scrolled back with nothing new below: the row above the status line
/// offers to jump to the bottom, and says how.
#[test]
fn scrolled_back_without_new_output_the_hint_offers_to_jump() {
    let mut screen = Screen::long_turn(40);
    assert!(!screen_text(&screen.lines()).contains("↓"), "following");
    screen.wheel(MouseEventKind::ScrollUp);
    screen.draw();
    let shown = screen_text(&screen.lines());
    assert_eq!(screen.above_status(), JUMP, "{shown}");
}

/// Scrolled back and new output comes: the hint says so, with the same way
/// to jump.
#[test]
fn scrolled_back_with_new_output_the_hint_says_so() {
    let mut screen = Screen::long_turn(40);
    screen.wheel(MouseEventKind::ScrollUp);
    screen.draw();
    screen.say("t1", "m41", "line 41");
    screen.draw();
    let shown = screen_text(&screen.lines());
    assert_eq!(screen.above_status(), NEW, "{shown}");
}

/// A click on the hint goes to the bottom and follows the latest output
/// again; it neither selects nor copies.
#[test]
fn clicking_the_hint_jumps_to_the_bottom_and_follows() {
    let mut screen = Screen::long_turn(40);
    screen.key(KeyCode::PageUp);
    screen.draw();
    screen.say("t1", "m41", "line 41");
    screen.draw();
    assert_eq!(screen.above_status(), NEW);

    screen.click(hint_row());
    screen.draw();
    let shown = screen_text(&screen.lines());
    assert_eq!(screen.above_status(), "  line 41", "{shown}");
    assert!(!shown.contains("↓"), "{shown}");

    screen.say("t1", "m42", "line 42");
    screen.draw();
    assert_eq!(screen.above_status(), "  line 42");
}

/// End with nothing typed goes to the bottom and follows again.
#[test]
fn end_with_an_empty_input_jumps_to_the_bottom() {
    let mut screen = Screen::long_turn(40);
    screen.key(KeyCode::PageUp);
    screen.draw();
    assert_eq!(screen.above_status(), JUMP);

    assert_eq!(screen.key(KeyCode::End), None);
    screen.draw();
    let shown = screen_text(&screen.lines());
    assert_eq!(screen.above_status(), "  line 40", "{shown}");

    screen.say("t1", "m41", "line 41");
    screen.draw();
    assert_eq!(screen.above_status(), "  line 41");
}

/// End with words typed only moves the cursor to the end of the line.
#[test]
fn end_with_words_typed_moves_the_cursor_only() {
    let mut screen = Screen::long_turn(40);
    screen.key(KeyCode::PageUp);
    for c in "ab".chars() {
        screen.key(KeyCode::Char(c));
    }
    screen.key(KeyCode::Home);
    screen.draw();
    let before = screen.conversation();

    assert_eq!(screen.key(KeyCode::End), None);
    screen.key(KeyCode::Char('c'));
    screen.draw();
    let shown = screen_text(&screen.lines());
    assert_eq!(screen.above_status(), JUMP, "still scrolled back:\n{shown}");
    assert_eq!(screen.conversation(), before, "{shown}");
    assert!(
        shown.contains("│ › abc"),
        "the cursor went to the end:\n{shown}"
    );
}

/// The hint row while scrolled back.
fn hint_row() -> u16 {
    HEIGHT - BELOW as u16 - 1
}
