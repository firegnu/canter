//! The welcome header at the top of the conversation: a pixel pony on the
//! left (16 × 8 pixels, two to a cell), three columns, then four rows of
//! text: `canter` and the version line, the session's directory, and two
//! rows of keys. It scrolls with the conversation, is drawn again after a
//! rewind, copies without the pony and is searched without it. Narrow
//! windows show the text alone, with fewer keys.

use std::sync::mpsc;
use std::time::Duration;

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 1_000_000;
const WIDTH: u16 = 100;
const HEIGHT: u16 = 20;
/// A fixed version line, in place of this build's commit.
const VERSION: &str = "0.1.0 · 37489fe";
const HOME: &str = "/home/dev";
const CWD: &str = "/home/dev/projects/canter";
/// Where the text starts: the pony's 16 columns and 3 more.
const TEXT: usize = 19;
const WAIT: Duration = Duration::from_secs(5);

/// The pony, from the task: `.` is empty.
const PIXELS: [&str; 8] = [
    "..........b..b..",
    "........mmbbbbb.",
    ".......mmbwebbbb",
    ".......mbbeebbbb",
    ".mm....mbbbbbpcc",
    "m..bbbbbbbbbbcc.",
    "...bbbbbbbbb....",
    "...m.m...m.m....",
];

const KEYS: [&str; 2] = [
    "/help commands · @ files · Ctrl+F find · Ctrl+O more",
    "Shift+Tab permissions · Esc interrupt · Ctrl+C quit",
];

/// The colour a pixel is drawn in, for `depth`; `None` for an empty one.
fn paint(pixel: u8, depth: Depth) -> Option<Color> {
    if pixel == b'.' {
        return None;
    }
    let (rgb, index) = match pixel {
        b'b' => ((0xc0, 0x70, 0x3f), 173),
        b'm' => ((0x5b, 0x32, 0x20), 94),
        b'e' => ((0x1e, 0x1a, 0x18), 234),
        b'w' => ((0xff, 0xff, 0xff), 231),
        b'c' => ((0xf1, 0xd2, 0xa6), 223),
        b'p' => ((0xe8, 0x9a, 0x8c), 211),
        _ => panic!("unknown pixel {}", pixel as char),
    };
    let eye = matches!(pixel, b'e' | b'w');
    match depth {
        Depth::TrueColor => Some(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        Depth::Ansi256 => Some(Color::Indexed(index)),
        _ if eye => None,
        Depth::Ansi16 => Some(Color::Cyan),
        Depth::None => Some(Color::Reset),
    }
}

/// What the cell at column `x` of pony row `y` holds: its symbol, text
/// colour and background. A cell is two pixels: the upper one in the text
/// colour (`▀`), the lower one in the background when they differ.
fn expected_cell(x: usize, y: usize, depth: Depth) -> (&'static str, Color, Color) {
    let top = paint(PIXELS[2 * y].as_bytes()[x], depth);
    let bottom = paint(PIXELS[2 * y + 1].as_bytes()[x], depth);
    match (top, bottom) {
        (None, None) => (" ", Color::Reset, Color::Reset),
        (Some(top), None) => ("▀", top, Color::Reset),
        (None, Some(bottom)) => ("▄", bottom, Color::Reset),
        (Some(top), Some(bottom)) if top == bottom => ("█", top, Color::Reset),
        (Some(top), Some(bottom)) => ("▀", top, bottom),
    }
}

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    copied: mpsc::Receiver<String>,
}

impl Screen {
    fn new() -> Self {
        Self::with(WIDTH, HEIGHT, VERSION, CWD)
    }

    fn with(width: u16, height: u16, version: &str, cwd: &str) -> Self {
        let (sender, copied) = mpsc::channel();
        let app = App::new(SessionSnapshot::empty(SESSION), Some(HOME.into()), NOW)
            .with_welcome(version)
            .with_clipboard_writer(move |text: &str| {
                let _ = sender.send(text.to_owned());
                Ok(())
            });
        let ui = Ui::new(TestBackend::new(width, height), app).unwrap();
        let mut screen = Self { ui, seq: 0, copied };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": cwd, "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
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

    /// A finished turn: the prompt, then an answer of `lines` lines.
    fn turn(&mut self, id: &str, prompt: &str, lines: usize) {
        self.event(json!({ "type": "turn.started", "turn": turn_json(id, prompt, "inProgress") }));
        let text: Vec<String> = (1..=lines).map(|n| format!("{id} line {n}")).collect();
        let message =
            json!({ "type": "agentMessage", "id": format!("{id}-m"), "text": text.join("\n") });
        self.event(json!({ "type": "item.started", "turnId": id, "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": id, "item": message }));
        self.event(json!({ "type": "turn.completed", "turn": turn_json(id, prompt, "completed") }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    fn draw(&mut self) {
        self.ui.draw(NOW).unwrap();
    }

    fn buffer(&self) -> &Buffer {
        self.ui.backend().buffer()
    }

    fn lines(&self) -> Vec<String> {
        (0..self.buffer().area.height)
            .map(|y| row(self.buffer(), y).trim_end().to_owned())
            .collect()
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// Row `y` from column `x` on, trailing spaces trimmed.
    fn from(&self, x: usize, y: u16) -> String {
        row(self.buffer(), y)
            .chars()
            .skip(x)
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, modifiers), NOW);
    }

    fn mouse(&mut self, kind: MouseEventKind, (column, row): (u16, u16)) {
        let event = MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        self.ui.app_mut().handle_mouse_at(event, NOW);
    }

    /// The status line: the row above the input box.
    fn status(&self) -> String {
        let height = self.buffer().area.height;
        row(self.buffer(), height - 6).trim().to_owned()
    }
}

fn turn_json(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": text }],
        "clientName": "canter-tui", "usage": null,
    })
}

/// A buffer row; cells hidden under a wide character are skipped.
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

/// The four rows of text right of the pony on a wide screen.
fn expected_text() -> [String; 4] {
    [
        format!("canter {VERSION}"),
        "~/projects/canter".to_owned(),
        KEYS[0].to_owned(),
        KEYS[1].to_owned(),
    ]
}

#[test]
fn a_new_session_opens_with_the_pony_and_four_rows_of_text() {
    let depths = [Depth::TrueColor, Depth::Ansi256, Depth::Ansi16, Depth::None];
    for depth in depths {
        for background in [Background::Dark, Background::Light] {
            theme::set(Theme { depth, background });
            let screen = Screen::new();
            let buffer = screen.buffer();
            let at = format!("{depth:?} on {background:?}:\n{}", screen.text());
            for y in 0..4 {
                for x in 0..16 {
                    let cell = &buffer[(x as u16, y as u16)];
                    assert_eq!(
                        (cell.symbol(), cell.fg, cell.bg),
                        expected_cell(x, y, depth),
                        "cell ({x}, {y}), {at}"
                    );
                }
                for x in 16..TEXT {
                    assert_eq!(buffer[(x as u16, y as u16)].symbol(), " ", "{at}");
                }
                assert_eq!(screen.from(TEXT, y as u16), expected_text()[y], "{at}");
            }
            // The title: `canter` bold, the version plain; the rest dim.
            let title = &buffer[(TEXT as u16, 0)];
            assert!(title.modifier.contains(Modifier::BOLD), "{at}");
            assert!(!title.modifier.contains(Modifier::DIM), "{at}");
            let version = &buffer[(TEXT as u16 + 7, 0)];
            assert!(!version.modifier.contains(Modifier::BOLD), "{at}");
            assert!(!version.modifier.contains(Modifier::DIM), "{at}");
            for y in 1..4 {
                let cell = &buffer[(TEXT as u16, y)];
                assert!(cell.modifier.contains(Modifier::DIM), "row {y}, {at}");
            }
            // Nothing else in the conversation yet.
            assert_eq!(screen.lines()[4], "", "{at}");
        }
    }
    theme::set(Theme::default());
}

#[test]
fn in_one_colour_the_pony_reads_as_the_sketch() {
    theme::set(Theme {
        depth: Depth::Ansi16,
        background: Background::Dark,
    });
    let screen = Screen::new();
    let pony: Vec<String> = (0..4)
        .map(|y| row(screen.buffer(), y).chars().take(16).collect())
        .collect();
    assert_eq!(
        pony,
        [
            "        ▄▄█▄▄█▄ ",
            "       ███  ████",
            "▄▀▀▄▄▄▄████████▀",
            "   █▀█▀▀▀█▀█    ",
        ]
    );
    theme::set(Theme {
        depth: Depth::TrueColor,
        background: Background::Dark,
    });
    let screen = Screen::new();
    // The eye's highlight above the eye: `▀` in white on the eye's colour.
    let eye = &screen.buffer()[(10, 1)];
    assert_eq!(
        (eye.symbol(), eye.fg, eye.bg),
        (
            "▀",
            Color::Rgb(0xff, 0xff, 0xff),
            Color::Rgb(0x1e, 0x1a, 0x18)
        )
    );
    theme::set(Theme::default());
}

#[test]
fn with_256_colours_the_eye_stands_out_from_the_body_on_any_background() {
    for background in [Background::Dark, Background::Light] {
        theme::set(Theme {
            depth: Depth::Ansi256,
            background,
        });
        let screen = Screen::new();
        // Pony row 1, column 11: eye above eye; column 12: body above body.
        let eye = screen.buffer()[(11, 1)].fg;
        let body = screen.buffer()[(12, 1)].fg;
        assert_eq!(eye, Color::Indexed(234));
        assert_eq!(body, Color::Indexed(173));
        assert_ne!(eye, body);
    }
    theme::set(Theme::default());
}

#[test]
fn without_a_commit_the_title_has_the_version_only() {
    let screen = Screen::with(WIDTH, HEIGHT, "0.1.0", CWD);
    assert_eq!(screen.from(TEXT, 0), "canter 0.1.0");
}

#[test]
fn a_narrow_window_shows_the_text_alone_with_fewer_keys() {
    // The pony and the title need 19 + 22 columns.
    let screen = Screen::with(40, HEIGHT, VERSION, CWD);
    let lines = screen.lines();
    assert_eq!(lines[0], format!("canter {VERSION}"), "{}", screen.text());
    assert_eq!(lines[1], "~/projects/canter");
    assert_eq!(lines[2], "/help commands · @ files · Ctrl+F find");
    assert_eq!(lines[3], "Shift+Tab permissions · Esc interrupt");
    assert_eq!(lines[4], "");
    assert!(!screen.text().contains('▀') && !screen.text().contains('█'));

    // Just wide enough for the pony: the keys that fit beside it.
    let screen = Screen::with(41, HEIGHT, VERSION, CWD);
    assert_eq!(screen.from(TEXT, 0), format!("canter {VERSION}"));
    assert_eq!(screen.from(TEXT, 2), "/help commands");
    assert_eq!(screen.from(TEXT, 3), "Shift+Tab permissions");
    assert_eq!(screen.lines()[4], "");
}

#[test]
fn a_long_directory_is_cut_in_the_middle() {
    let cwd = "/home/dev/work/a/very/long/path/to/some/deeply/nested/project";
    let screen = Screen::with(60, HEIGHT, VERSION, cwd);
    let path = screen.from(TEXT, 1);
    assert_eq!(path.width(), 60 - TEXT, "{}", screen.text());
    assert!(path.starts_with("~/work/a/very"), "{path}");
    assert!(path.ends_with("nested/project"), "{path}");
    assert!(path.contains('…'), "{path}");
    assert_eq!(screen.lines()[4], "", "one row, not wrapped");
}

#[test]
fn a_resumed_session_has_the_header_at_the_top_of_the_conversation() {
    let mut screen = Screen::new();
    for turn in 1..=3 {
        screen.turn(&format!("t{turn}"), &format!("question {turn}"), 10);
    }
    screen.draw();
    assert!(
        !screen.text().contains("Shift+Tab permissions"),
        "following the latest output:\n{}",
        screen.text()
    );
    for _ in 0..10 {
        screen.key(KeyCode::PageUp, KeyModifiers::NONE);
    }
    screen.draw();
    for (y, text) in expected_text().iter().enumerate() {
        assert_eq!(&screen.from(TEXT, y as u16), text, "{}", screen.text());
    }
    // A blank row, then the first prompt.
    assert_eq!(screen.lines()[4], "");
    assert_eq!(screen.lines()[5], "› question 1");
}

#[test]
fn a_rewind_keeps_the_header_at_the_top() {
    let mut screen = Screen::new();
    screen.turn("t1", "kept", 1);
    screen.turn("t2", "removed", 1);
    let mut record = turn_json("rewind", "", "completed");
    record["input"] = json!([]);
    screen.event(json!({
        "type": "session.rewound", "beforeTurnId": "t2",
        "rewindConversation": true, "record": record,
    }));
    screen.draw();
    assert!(!screen.text().contains("removed"), "{}", screen.text());
    assert_eq!(screen.from(TEXT, 0), format!("canter {VERSION}"));
    assert_eq!(screen.lines()[5], "› kept");
}

#[test]
fn dragging_over_the_header_copies_its_text_without_the_pony() {
    let mut screen = Screen::new();
    screen.mouse(MouseEventKind::Down(MouseButton::Left), (0, 0));
    screen.mouse(MouseEventKind::Drag(MouseButton::Left), (90, 3));
    screen.mouse(MouseEventKind::Up(MouseButton::Left), (90, 3));
    let copied = screen.copied.recv_timeout(WAIT).expect("a copy");
    assert_eq!(copied, expected_text().join("\n"));
}

#[test]
fn searching_finds_the_text_of_the_header_but_not_the_pony() {
    let mut screen = Screen::new();
    screen.key(KeyCode::Char('f'), KeyModifiers::CONTROL);
    for pony in ["█", "▀", "▄"] {
        for c in pony.chars() {
            screen.key(KeyCode::Char(c), KeyModifiers::NONE);
        }
        screen.draw();
        assert!(
            screen.status().contains("No matches"),
            "{pony}: {}",
            screen.status()
        );
        screen.key(KeyCode::Char('c'), KeyModifiers::CONTROL);
    }
    for c in "ctrl+f find".chars() {
        screen.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    screen.draw();
    assert!(
        screen.status().starts_with("Find · 1/1"),
        "{}",
        screen.status()
    );
    let found = &screen.buffer()[(TEXT as u16 + 27, 2)];
    assert_eq!(found.symbol(), "C");
    assert!(found.modifier.contains(Modifier::REVERSED));
}
