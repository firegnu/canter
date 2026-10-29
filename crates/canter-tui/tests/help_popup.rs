//! `?` in the empty input box and `/help`: the shortcuts and the commands in
//! a box above the input box, as `/context`'s, not in the conversation.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{ANSWER_GUARD_MS, Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 0;

/// Every command, in the command list's order, with its line.
const COMMANDS: [(&str, &str); 28] = [
    ("/compact", "Summarize the conversation to free up context"),
    ("/context", "Show context contents and token estimates"),
    ("/copy", "Copy the last answer, or code block N of it"),
    ("/diff", "Show uncommitted git changes"),
    ("/exit", "Quit canter"),
    ("/export", "Save or copy the conversation as Markdown"),
    ("/files", "List session files and git status"),
    ("/help", "Show available commands"),
    ("/lsp", "Show language servers"),
    ("/mcp", "Show MCP connections and tools"),
    ("/mode", "Change the permission mode"),
    ("/model", "Change the model"),
    ("/new", "Start a new session"),
    ("/plan", "Enter or leave read-only Plan mode"),
    ("/resume", "Switch to another session"),
    ("/skills", "List this session's skills"),
    ("/think", "Change thinking effort (none, low, high, max)"),
    ("/usage", "Show this session's token usage and cost"),
    (
        "/btw",
        "Ask one question without adding it to the conversation",
    ),
    ("/handoff", "Draft a handoff note in a new session"),
    (
        "/review",
        "Review uncommitted changes or a PR number/URL (read-only)",
    ),
    ("/rewind", "Rewind conversation, files, or both"),
    ("/statusline", "Choose and order what the status line shows"),
    ("/ps", "List background commands and inspect or stop one"),
    ("/stop", "Stop all background commands in this session"),
    ("/theme", "Choose the code colour theme"),
    ("/add-dir", "Add a directory to this session"),
    (
        "/goal",
        "Set or view a goal; pause, resume, clear, or --budget USD",
    ),
];

const SECTIONS: [&str; 5] = [
    "Input",
    "Conversation",
    "Approvals and questions",
    "Find",
    "Commands",
];

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    now: i64,
}

impl Screen {
    fn new(width: u16, height: u16) -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(width, height), app).unwrap(),
            seq: 0,
            now: NOW,
        };
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
        self.ui.draw(self.now).unwrap();
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        let action = self
            .ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), self.now);
        self.draw();
        action
    }

    /// Types `text` and Enter.
    fn enter(&mut self, text: &str) -> Option<Action> {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
        self.key(KeyCode::Enter)
    }

    fn start_turn(&mut self, turn: &str) {
        self.event(json!({ "type": "turn.started", "turn": turn_json(turn, "inProgress") }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn rows(&self) -> Vec<String> {
        let buffer = self.ui.backend().buffer();
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
        self.rows().join("\n")
    }

    fn box_top(&self) -> usize {
        self.rows()
            .iter()
            .rposition(|row| row.starts_with('╭'))
            .expect("input box")
    }

    /// The rows from the ` Help` title down to the input box; empty while
    /// closed.
    fn help(&self) -> Vec<String> {
        let rows = self.rows();
        match rows.iter().position(|row| row == " Help") {
            Some(start) => rows[start..self.box_top()].to_vec(),
            None => Vec::new(),
        }
    }

    fn input(&self) -> String {
        let rows = self.rows();
        rows[self.box_top() + 1]
            .trim_start_matches('│')
            .trim_end_matches('│')
            .trim()
            .to_owned()
    }
}

fn turn_json(id: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "hello" }],
        "clientName": "canter-tui", "usage": null,
    })
}

/// The rows of `section`, from its title to the blank row after it.
fn section<'a>(help: &'a [String], title: &str) -> &'a [String] {
    let start = help
        .iter()
        .position(|row| row.trim() == title)
        .unwrap_or_else(|| panic!("no {title} in:\n{}", help.join("\n")));
    let len = help[start + 1..]
        .iter()
        .position(String::is_empty)
        .unwrap_or(help.len() - start - 1);
    &help[start + 1..start + 1 + len]
}

#[test]
fn a_question_mark_in_the_empty_input_box_opens_the_help() {
    let mut screen = Screen::new(80, 30);
    assert_eq!(screen.key(KeyCode::Char('?')), None);
    let help = screen.help();
    assert!(!help.is_empty(), "{}", screen.text());
    assert_eq!(help.last().map(String::as_str), Some(" Esc close"));
    // Not typed: the input box is still empty.
    assert!(
        screen.input().contains("Type a message"),
        "{}",
        screen.text()
    );
    // Esc closes it, as /context's.
    screen.key(KeyCode::Esc);
    assert!(screen.help().is_empty(), "{}", screen.text());
}

#[test]
fn the_help_opens_while_a_turn_runs() {
    let mut screen = Screen::new(80, 30);
    screen.start_turn("t1");
    screen.draw();
    assert_eq!(screen.key(KeyCode::Char('?')), None);
    assert!(!screen.help().is_empty(), "{}", screen.text());
}

#[test]
fn a_question_mark_after_text_is_typed() {
    let mut screen = Screen::new(80, 30);
    screen.key(KeyCode::Char('a'));
    screen.key(KeyCode::Char('?'));
    assert!(screen.help().is_empty(), "{}", screen.text());
    assert_eq!(screen.input(), "› a?");
}

#[test]
fn a_question_mark_with_the_command_list_open_is_typed() {
    let mut screen = Screen::new(80, 30);
    screen.key(KeyCode::Char('/'));
    assert!(screen.text().contains("/compact"), "{}", screen.text());
    screen.key(KeyCode::Char('?'));
    assert!(screen.help().is_empty(), "{}", screen.text());
    assert_eq!(screen.input(), "› /?");
}

#[test]
fn a_question_mark_with_the_approval_box_open_does_not_open_the_help() {
    let mut screen = Screen::new(80, 30);
    screen.start_turn("t1");
    screen.event(json!({
        "type": "item.started", "turnId": "t1",
        "item": { "type": "commandExecution", "id": "c1", "command": "ls", "cwd": "/work",
                  "status": "inProgress", "aggregatedOutput": "", "exitCode": null,
                  "durationMs": null },
    }));
    screen.event(json!({
        "type": "request.opened",
        "request": {
            "id": "r1", "turnId": "t1", "createdAt": NOW,
            "kind": { "type": "approval", "itemId": "c1", "tool": "shell",
                      "arguments": { "command": "ls" },
                      "availableDecisions": ["accept", "decline", "cancel"] },
        },
    }));
    screen.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    screen.draw();
    assert!(screen.text().contains("Allow once"), "{}", screen.text());
    screen.now += ANSWER_GUARD_MS;
    assert_eq!(screen.key(KeyCode::Char('?')), None);
    assert!(screen.help().is_empty(), "{}", screen.text());
    assert!(screen.text().contains("Allow once"), "{}", screen.text());
}

#[test]
fn slash_help_opens_the_same_box_and_prints_nothing() {
    let mut screen = Screen::new(80, 30);
    let before: Vec<String> = screen.rows()[..screen.box_top()].to_vec();
    assert_eq!(screen.enter("/help"), None);
    let help = screen.help();
    assert!(!help.is_empty(), "{}", screen.text());
    // The same box as `?` opens.
    screen.key(KeyCode::Esc);
    screen.key(KeyCode::Char('?'));
    assert_eq!(screen.help(), help);
    screen.key(KeyCode::Esc);
    // Nothing was added to the conversation.
    assert_eq!(screen.rows()[..screen.box_top()].to_vec(), before);
    assert!(
        !screen.text().contains("Show available commands"),
        "{}",
        screen.text()
    );
}

#[test]
fn the_help_has_the_sections_and_every_command() {
    let mut screen = Screen::new(100, 140);
    screen.key(KeyCode::Char('?'));
    let help = screen.help();
    assert!(!help.iter().any(|row| row.contains("more")), "{help:#?}");
    let titles: Vec<usize> = SECTIONS
        .iter()
        .map(|title| {
            help.iter()
                .position(|row| row.trim() == *title)
                .unwrap_or_else(|| panic!("no {title} in:\n{}", help.join("\n")))
        })
        .collect();
    assert!(titles.is_sorted(), "{titles:?}");
    for title in &SECTIONS[..4] {
        assert!(!section(&help, title).is_empty(), "{title}");
    }
    let commands = section(&help, "Commands");
    assert_eq!(commands.len(), COMMANDS.len(), "{commands:#?}");
    for (row, (name, about)) in commands.iter().zip(COMMANDS) {
        let words: Vec<&str> = row
            .trim()
            .split("  ")
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .collect();
        assert_eq!(words, [name.trim(), about.trim()], "{row}");
    }
    // What each key does starts in one column in the shortcut sections, and
    // what each command does in one of its own.
    let column = |row: &String| {
        let about = row.trim().split("  ").last().unwrap().trim();
        row[..row.rfind(about).unwrap()].width()
    };
    let first = column(&section(&help, "Input")[0]);
    for title in &SECTIONS[..4] {
        for row in section(&help, title) {
            assert_eq!(column(row), first, "{row}");
        }
    }
    let first = column(&commands[0]);
    for row in commands {
        assert_eq!(column(row), first, "{row}");
    }
}

#[test]
fn the_help_lists_the_shortcuts() {
    let mut screen = Screen::new(100, 140);
    screen.key(KeyCode::Char('?'));
    let help = screen.help();
    let keys = |title| -> Vec<String> {
        section(&help, title)
            .iter()
            .map(|row| row.trim().split("  ").next().unwrap().to_owned())
            .collect()
    };
    for (title, wanted) in [
        (
            "Input",
            &[
                "Enter",
                "Shift+Enter / Ctrl+J",
                "Ctrl+V",
                "/",
                "@",
                "Tab",
                "Alt+Backspace / Ctrl+W",
                "Ctrl+A / Home",
                "Ctrl+E / End",
                "Ctrl+K",
                "Ctrl+U",
                "Ctrl+C",
                "Ctrl+D",
                "?",
            ][..],
        ),
        (
            "Conversation",
            &["Esc", "PgUp / PgDn", "End", "Ctrl+O", "Ctrl+F", "Shift+Tab"][..],
        ),
        ("Find", &["Enter", "Esc", "Ctrl+C"][..]),
    ] {
        let shown = keys(title);
        for key in wanted {
            assert!(
                shown.iter().any(|shown| shown == key),
                "{key} in {title}: {shown:#?}"
            );
        }
    }
    let text = help.join("\n");
    for words in ["Pasted", "Drag", "Click"] {
        assert!(text.contains(words), "{words}:\n{text}");
    }
}

#[test]
fn a_short_window_scrolls_the_help() {
    let mut screen = Screen::new(80, 30);
    screen.key(KeyCode::Char('?'));
    let help = screen.help();
    assert!(
        help.iter()
            .any(|row| row.contains("↓") && row.contains("more")),
        "{help:#?}"
    );
    for _ in 0..200 {
        screen.key(KeyCode::Down);
    }
    let help = screen.help();
    assert!(
        help.iter().any(|row| row.contains("/statusline")),
        "{help:#?}"
    );
}
