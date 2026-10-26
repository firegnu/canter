//! Syntax colours on ratatui's TestBackend: code blocks in the model's text
//! by their language, and the command in the approval box as Bash. Colours
//! are the terminal's 16, read from each cell's foreground.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Depth, Theme};
use canter_tui::{App, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::Color;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const HEIGHT: u16 = 30;

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new(width: u16) -> Self {
        Self::with_theme(width, Theme::default())
    }

    fn with_theme(width: u16, look: Theme) -> Self {
        theme::set(look);
        let app = App::new(SessionSnapshot::empty(SESSION), None, 0);
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(width, HEIGHT), app).unwrap(),
            seq: 0,
        };
        screen.event(json!({ "type": "turn.started", "turn": {
            "id": "t1", "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": "show some code" }],
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

    /// The model writes `text` as a finished message.
    fn say(&mut self, text: &str) {
        let message = json!({ "type": "agentMessage", "id": "m1", "text": text });
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": message }));
    }

    /// The model asks to run `command`.
    fn ask(&mut self, command: &str) {
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": {
            "type": "commandExecution", "id": "c1", "command": command, "cwd": "/work",
            "aggregatedOutput": "", "exitCode": null, "status": "inProgress",
        }}));
        self.event(json!({ "type": "request.opened", "request": {
            "id": "r1", "turnId": "t1", "createdAt": 0,
            "kind": {
                "type": "approval", "itemId": "c1", "tool": "shell",
                "arguments": { "command": command },
                "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
            },
        }}));
        self.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    }

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(0).unwrap();
        self.ui.backend().buffer()
    }

    fn lines(&mut self) -> Vec<String> {
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

/// The column where `text` first shows from row `from` down, and its row.
fn find_from(buffer: &Buffer, from: u16, text: &str) -> (u16, u16) {
    for y in from..buffer.area.height {
        let row = row(buffer, y);
        if let Some(byte) = row.find(text) {
            return (row[..byte].width() as u16, y);
        }
    }
    let rows: Vec<String> = (0..buffer.area.height).map(|y| row(buffer, y)).collect();
    panic!("{text:?} is not on screen:\n{}", rows.join("\n"));
}

fn find(buffer: &Buffer, text: &str) -> (u16, u16) {
    find_from(buffer, 0, text)
}

/// The cell where `text` first starts.
fn cell<'a>(buffer: &'a Buffer, text: &str) -> &'a Cell {
    &buffer[find(buffer, text)]
}

/// The foreground of every non-space cell on the row where `text` starts.
fn row_colors(buffer: &Buffer, text: &str) -> Vec<Color> {
    let (_, y) = find(buffer, text);
    (0..buffer.area.width)
        .map(|x| &buffer[(x, y)])
        .filter(|cell| !cell.symbol().trim().is_empty())
        .map(|cell| cell.fg)
        .collect()
}

/// The foreground of each non-space cell of `text`, where it first shows
/// from row `from` down.
fn text_colors(buffer: &Buffer, from: u16, text: &str) -> Vec<Color> {
    let (x, y) = find_from(buffer, from, text);
    (x..x + text.width() as u16)
        .map(|x| &buffer[(x, y)])
        .filter(|cell| !cell.symbol().trim().is_empty())
        .map(|cell| cell.fg)
        .collect()
}

/// One of the terminal's 16 colours, or its default.
fn ansi16(color: Color) -> bool {
    !matches!(color, Color::Rgb(..) | Color::Indexed(_))
}

const RUST: &str = "把常量改大一点：

```rust
const FINISHED_TAIL: usize = 4;
fn tail(n: u32) -> u32 { n }
```
";

#[test]
fn a_rust_block_colors_keywords_types_and_numbers_from_the_16_colours() {
    let mut screen = Screen::new(60);
    screen.say(RUST);
    let lines = screen.lines();
    assert!(
        lines
            .iter()
            .any(|line| line == "    const FINISHED_TAIL: usize = 4;"),
        "{}",
        lines.join("\n")
    );
    assert!(!lines.iter().any(|line| line.contains("```")));
    let buffer = screen.draw();
    assert_eq!(cell(buffer, "const").fg, Color::Magenta);
    assert_eq!(cell(buffer, "usize").fg, Color::Magenta);
    assert_eq!(cell(buffer, "4;").fg, Color::Yellow);
    assert_eq!(cell(buffer, "fn tail").fg, Color::Magenta);
    assert_eq!(cell(buffer, ": usize").fg, Color::Reset);
    for text in ["const", "fn tail"] {
        let colors = row_colors(buffer, text);
        assert!(colors.iter().all(|&c| ansi16(c)), "{colors:?}");
        assert!(colors.contains(&Color::Reset), "{colors:?}");
    }
}

#[test]
fn golang_and_a_block_starting_with_a_bash_shebang_are_recognised() {
    let mut screen = Screen::new(60);
    screen.say("```golang\nfunc main() { x := 42 }\n```\n");
    let buffer = screen.draw();
    assert_eq!(cell(buffer, "func").fg, Color::Magenta);
    assert_eq!(cell(buffer, "42").fg, Color::Yellow);
    assert_eq!(cell(buffer, "x :=").fg, Color::Reset);

    let mut screen = Screen::new(60);
    screen.say("```\n#!/bin/bash\nif true; then echo \"hi\"; fi\n```\n");
    let buffer = screen.draw();
    assert_eq!(cell(buffer, "#!/bin/bash").fg, Color::Green);
    assert_eq!(cell(buffer, "if true").fg, Color::Magenta);
    assert_eq!(cell(buffer, "\"hi\"").fg, Color::Green);
}

#[test]
fn a_block_without_or_with_an_unknown_language_stays_all_cyan() {
    for text in [
        "装的时候：\n\n```\ncargo install --path crates/canter\n```\n",
        "```nosuchlanguage\ncargo install --path crates/canter\n```\n",
    ] {
        let mut screen = Screen::new(60);
        screen.say(text);
        let colors = row_colors(screen.draw(), "cargo install");
        assert!(
            colors.iter().all(|&c| c == Color::Cyan),
            "{text}: {colors:?}"
        );
    }
}

#[test]
fn a_block_over_the_limits_stays_all_cyan() {
    let long_line = format!(
        "```rust\nlet x = 1; // {}\nlet y = 2;\n```\n",
        "a".repeat(5000)
    );
    let many_lines = format!(
        "```rust\n{}let y = 2;\n```\n",
        "let x = 1;\n".repeat(10_001)
    );
    let many_bytes = format!(
        "```rust\n{}let y = 2;\n```\n",
        format!("let x = 1; // {}\n", "a".repeat(90)).repeat(5_400)
    );
    for (name, text) in [
        ("a line over 4 KiB", long_line),
        ("over 10 000 lines", many_lines),
        ("over 512 KB", many_bytes),
    ] {
        let mut screen = Screen::new(60);
        screen.say(&text);
        let colors = row_colors(screen.draw(), "let y");
        assert!(
            colors.iter().all(|&c| c == Color::Cyan),
            "{name}: {colors:?}"
        );
    }
}

#[test]
fn no_color_draws_code_without_colours() {
    let mut screen = Screen::with_theme(
        60,
        Theme {
            depth: Depth::None,
            ..Theme::default()
        },
    );
    screen.say(RUST);
    let buffer = screen.draw();
    for text in ["const", "fn tail"] {
        let colors = row_colors(buffer, text);
        assert!(colors.iter().all(|&c| c == Color::Reset), "{colors:?}");
    }
}

/// A command with a pipe, a quoted string, a tab, an escape character and a
/// line long enough to wrap.
const COMMAND: &str = "cd /work && cargo test --all-targets 2>&1 | tee \"log.txt\" | grep -v warning\nprintf 'a\tb\\n'\necho \u{1b}[31m done";

#[test]
fn the_command_in_the_approval_box_is_colored_and_its_text_is_unchanged() {
    let mut screen = Screen::new(50);
    screen.ask(COMMAND);
    let lines = screen.lines();
    let at = lines
        .iter()
        .position(|line| line.contains("Run this command?"))
        .unwrap_or_else(|| panic!("{}", lines.join("\n")));
    assert_eq!(
        lines[at + 1..at + 7],
        [
            "│ cd /work && cargo test --all-targets 2>&1 |    │",
            "│ tee \"log.txt\" | grep -v warning                │",
            "│ printf 'a   b\\n'                               │",
            "│ echo \\x1b[31m done                             │",
            "│ Directory: /work                               │",
            "│                                                │",
        ],
        "{}",
        lines.join("\n")
    );
    // The card above the box shows the command too, uncoloured: look in the box.
    let top = at as u16;
    let buffer = screen.draw();
    let fg = |text: &str| buffer[find_from(buffer, top, text)].fg;
    assert_eq!(fg("&&"), Color::Magenta);
    assert_eq!(fg("\"log.txt\""), Color::Green);
    assert_eq!(fg("'a   b"), Color::Green);
    assert_eq!(fg("cargo test"), Color::Reset);
    assert_eq!(fg("/work &&"), Color::Reset);
    for text in [
        "cd /work && cargo test --all-targets 2>&1 |",
        "tee \"log.txt\" | grep -v warning",
        "printf 'a   b\\n'",
        "echo \\x1b[31m done",
    ] {
        let colors = text_colors(buffer, top, text);
        assert!(colors.iter().all(|&c| ansi16(c)), "{text}: {colors:?}");
        assert!(
            colors.iter().any(|&c| c != Color::Reset),
            "{text}: {colors:?}"
        );
    }
    let colors = text_colors(buffer, top, "Directory: /work");
    assert!(colors.iter().all(|&c| c == Color::Reset), "{colors:?}");
}

/// Code whose colours depend on earlier lines: a comment and a string that
/// span lines.
const STREAMED: &str = "先看这段：

```rust
/* a comment
   that goes on */
let s = \"one
two\";
const N: usize = 4;
```

后面还有一句。
";

/// `STREAMED` drawn at once as a finished message, on its own thread so that
/// nothing the streaming test drew is remembered.
fn drawn_at_once() -> Buffer {
    std::thread::spawn(|| {
        let mut screen = Screen::new(60);
        screen.say(STREAMED);
        screen.draw().clone()
    })
    .join()
    .unwrap()
}

#[test]
fn a_block_still_streaming_draws_and_ends_like_one_drawn_at_once() {
    let mut screen = Screen::new(60);
    screen.event(json!({ "type": "item.started", "turnId": "t1", "item": {
        "type": "agentMessage", "id": "m1", "text": "",
    }}));
    let mut start = 0;
    let mut seen_open = false;
    let mut cuts: Vec<usize> = STREAMED.char_indices().map(|(i, _)| i).collect();
    cuts.push(STREAMED.len());
    for &cut in cuts.iter().skip(1) {
        let chunk = &STREAMED[start..cut];
        start = cut;
        screen.event(
            json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": { "type": "agentMessage", "delta": chunk } }),
        );
        let sent = &STREAMED[..cut];
        let buffer = screen.draw();
        // Inside the block, before its closing fence: earlier lines keep their colours.
        if sent.matches("```").count() == 1 && sent.contains("const N: usize = 4;\n") {
            assert_eq!(cell(buffer, "that goes on").fg, Color::Green);
            assert_eq!(cell(buffer, "const").fg, Color::Magenta);
            seen_open = true;
        }
    }
    assert!(seen_open);
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "agentMessage", "id": "m1", "text": STREAMED,
    }}));
    let streamed = screen.draw().clone();
    assert_eq!(cell(&streamed, "that goes on").fg, Color::Green);
    assert_eq!(cell(&streamed, "two\"").fg, Color::Green);
    assert_eq!(cell(&streamed, "const").fg, Color::Magenta);
    assert_eq!(streamed, drawn_at_once());
}
