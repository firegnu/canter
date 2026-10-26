//! Colours by purpose: `NO_COLOR`, a light background, the failed card's
//! cross, and how the colour depth and background are worked out.

use std::collections::HashMap;

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{App, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    /// Draws for `theme`; the theme is per thread, and each test has its own.
    fn new(theme: Theme) -> Self {
        theme::set(theme);
        let app = App::new(SessionSnapshot::empty(SESSION), None, 0);
        Self {
            ui: Ui::new(TestBackend::new(80, 30), app).unwrap(),
            seq: 0,
        }
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

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(0).unwrap();
        self.ui.backend().buffer()
    }

    /// A turn with a command that worked, one that failed, a file change
    /// that failed, a warning, and a command waiting for approval.
    fn busy_turn(&mut self) {
        self.event(json!({ "type": "turn.started", "turn": {
            "id": "t1", "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": "build it" }],
            "clientName": "canter-tui", "usage": null,
        }}));
        self.event(json!({ "type": "session.status", "status": "working" }));
        self.event(json!({ "type": "item.completed", "turnId": "t1",
            "item": command("ok", "cargo build", "completed", Some(0)) }));
        self.event(json!({ "type": "item.completed", "turnId": "t1",
            "item": command("bad", "cargo test", "failed", Some(1)) }));
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
            "type": "fileChange", "id": "write", "status": "failed",
            "changes": [{ "path": "/work/notes.txt", "kind": { "type": "add" }, "diff": "hello\n" }],
        }}));
        self.event(json!({ "type": "warning", "message": "Server version differs" }));
        self.event(json!({ "type": "item.started", "turnId": "t1",
            "item": command("ask", "rm -rf target", "inProgress", None) }));
        self.event(json!({ "type": "request.opened", "request": {
            "id": "r1", "turnId": "t1", "createdAt": 0,
            "kind": {
                "type": "approval", "itemId": "ask", "tool": "shell",
                "arguments": { "command": "rm -rf target" },
                "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
            },
        }}));
        self.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    }
}

fn command(id: &str, command: &str, status: &str, exit_code: Option<i32>) -> Value {
    json!({
        "type": "commandExecution", "id": id, "command": command, "cwd": "/work",
        "status": status, "aggregatedOutput": "", "exitCode": exit_code, "durationMs": null,
    })
}

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
            row
        })
        .collect()
}

/// The cell where `text` starts on the first row (from `from`) showing it.
fn find<'a>(buffer: &'a Buffer, text: &str, from: u16) -> &'a Cell {
    let rows = rows(buffer);
    let y = (from..buffer.area.height)
        .find(|&y| rows[usize::from(y)].contains(text))
        .unwrap_or_else(|| panic!("{text} is not on screen:\n{}", rows.join("\n")));
    let first = text.chars().next().unwrap().to_string();
    let row = &rows[usize::from(y)];
    let column = row[..row.find(text).unwrap()].width();
    let x = (column as u16..buffer.area.width)
        .find(|&x| buffer[(x, y)].symbol() == first)
        .unwrap();
    &buffer[(x, y)]
}

fn bold(cell: &Cell) -> bool {
    cell.modifier.contains(Modifier::BOLD)
}

#[test]
fn no_color_draws_no_colours_but_keeps_bold() {
    let mut screen = Screen::new(Theme {
        depth: Depth::None,
        background: Background::Dark,
    });
    screen.busy_turn();
    let buffer = screen.draw();
    for cell in buffer.content() {
        assert_eq!(cell.fg, Color::Reset, "{:?} has a colour", cell.symbol());
        assert_eq!(
            cell.bg,
            Color::Reset,
            "{:?} has a background",
            cell.symbol()
        );
    }
    assert!(
        bold(find(buffer, "› build it", 0)),
        "the prompt mark stays bold"
    );
    assert!(
        bold(find(buffer, "Run cargo build", 0)),
        "the verb stays bold"
    );
}

#[test]
fn on_a_light_background_warnings_are_bold_default_text_and_the_border_stays_yellow() {
    let mut screen = Screen::new(Theme {
        depth: Depth::Ansi16,
        background: Background::Light,
    });
    screen.busy_turn();
    let buffer = screen.draw();
    for text in [
        "! Server version differs",
        "Waiting for approval",
        "⏸ Waiting for approval",
    ] {
        let cell = find(buffer, text, 0);
        assert_eq!(cell.fg, Color::Reset, "{text} keeps the default colour");
        assert!(bold(cell), "{text} is bold");
    }
    let corner = find(buffer, "╭", 0);
    assert_eq!(
        corner.fg,
        Color::Yellow,
        "the approval box border is yellow"
    );
}

#[test]
fn failed_tool_cards_are_marked_with_a_red_cross() {
    let mut screen = Screen::new(Theme::default());
    screen.busy_turn();
    let buffer = screen.draw();
    for text in ["✗ Run cargo test", "✗ Add /work/notes.txt"] {
        let cell = find(buffer, text, 0);
        assert_eq!(cell.fg, Color::Red, "{text} is red");
    }
    let worked = find(buffer, "● Run cargo build", 0);
    assert_eq!(
        worked.fg,
        Color::Green,
        "a command that worked keeps its dot"
    );
}

/// Works out the theme from `vars`; `answer` is what the terminal says.
fn detect(vars: &[(&str, &str)], answer: Option<Background>) -> (Theme, bool) {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    let mut asked = false;
    let theme = Theme::from_env(
        |name| vars.get(name).cloned(),
        || {
            asked = true;
            answer
        },
    );
    (theme, asked)
}

#[test]
fn canter_theme_wins_without_asking_the_terminal() {
    for (value, expected) in [("dark", Background::Dark), ("light", Background::Light)] {
        let (theme, asked) = detect(
            &[("CANTER_THEME", value), ("COLORFGBG", "0;15")],
            Some(Background::Dark),
        );
        assert_eq!(theme.background, expected, "CANTER_THEME={value}");
        assert!(!asked, "CANTER_THEME={value} needs no question");
    }
    let (theme, asked) = detect(&[("CANTER_THEME", "auto")], Some(Background::Light));
    assert_eq!(
        theme.background,
        Background::Light,
        "other values are ignored"
    );
    assert!(asked);
}

#[test]
fn the_terminal_answer_comes_before_colorfgbg() {
    let (theme, _) = detect(&[("COLORFGBG", "15;0")], Some(Background::Light));
    assert_eq!(theme.background, Background::Light);
}

#[test]
fn colorfgbg_is_used_when_the_terminal_does_not_answer() {
    for (value, expected) in [
        ("15;0", Background::Dark),
        ("0;15", Background::Light),
        ("0;7", Background::Light),
        ("15;8", Background::Dark),
        ("0;default;15", Background::Light),
        ("15;default", Background::Unknown),
        ("", Background::Unknown),
    ] {
        let (theme, _) = detect(&[("COLORFGBG", value)], None);
        assert_eq!(theme.background, expected, "COLORFGBG={value:?}");
    }
    let (theme, _) = detect(&[], None);
    assert_eq!(theme.background, Background::Unknown, "nothing to go by");
}

#[test]
fn colour_depth_follows_colorterm_term_and_no_color() {
    for (vars, expected) in [
        (vec![("COLORTERM", "truecolor")], Depth::TrueColor),
        (vec![("COLORTERM", "24bit")], Depth::TrueColor),
        (
            vec![("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
            Depth::TrueColor,
        ),
        (vec![("TERM", "xterm-256color")], Depth::Ansi256),
        (vec![("TERM", "tmux-256color")], Depth::Ansi256),
        (vec![("COLORTERM", "yes"), ("TERM", "xterm")], Depth::Ansi16),
        (vec![("TERM", "xterm")], Depth::Ansi16),
        (vec![], Depth::Ansi16),
        (
            vec![("NO_COLOR", "1"), ("COLORTERM", "truecolor")],
            Depth::None,
        ),
        (
            vec![("NO_COLOR", ""), ("TERM", "xterm-256color")],
            Depth::Ansi256,
        ),
    ] {
        let (theme, _) = detect(&vars, None);
        assert_eq!(theme.depth, expected, "{vars:?}");
    }
}
