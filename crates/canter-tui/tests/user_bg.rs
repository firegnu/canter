//! Sent messages on a background of their own: the whole width of the
//! conversation, one row of it above and below, then an ordinary blank row.
//! Only when the terminal's background is known and it shows 256 colours or
//! more; queued messages never. A copy leaves the background rows and the
//! spaces after the text out; a selection and the current search match are
//! drawn over the background.

use std::sync::mpsc;
use std::time::Duration;

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 1_000_000;
const WIDTH: u16 = 60;
const HEIGHT: u16 = 24;
const WAIT: Duration = Duration::from_secs(5);

const PROMPT: &str = "把 README 里的安装步骤改成 cargo install\n另外顺手把版本号也改一下";

const DARK: Theme = Theme {
    depth: Depth::TrueColor,
    background: Background::Dark,
};
const DARK_BG: Color = Color::Rgb(0x2d, 0x2e, 0x33);

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    copied: mpsc::Receiver<String>,
}

impl Screen {
    fn new(theme: Theme, width: u16) -> Self {
        theme::set(theme);
        let (sender, copied) = mpsc::channel();
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW).with_clipboard_writer(
            move |text: &str| {
                let _ = sender.send(text.to_owned());
                Ok(())
            },
        );
        let ui = Ui::new(TestBackend::new(width, HEIGHT), app).unwrap();
        let mut screen = Self { ui, seq: 0, copied };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen
    }

    /// A finished turn answering `first`, then a turn sending [`PROMPT`]
    /// that reads a file; the second turn is still working.
    fn conversation(theme: Theme, width: u16) -> Self {
        let mut screen = Self::new(theme, width);
        screen.event(
            json!({ "type": "turn.started", "turn": turn_json("t1", "first", "inProgress") }),
        );
        screen.say("t1", "m1", "先看一下 README 现在怎么写的。");
        screen.event(
            json!({ "type": "turn.completed", "turn": turn_json("t1", "first", "completed") }),
        );
        screen.event(
            json!({ "type": "turn.started", "turn": turn_json("t2", PROMPT, "inProgress") }),
        );
        screen.event(json!({ "type": "session.status", "status": "working" }));
        screen.event(json!({ "type": "item.completed", "turnId": "t2", "item": {
            "type": "fileRead", "id": "r1", "path": "/work/README.md", "startLine": 1,
            "content": "a\nb\n", "status": "completed",
        }}));
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

    fn say(&mut self, turn: &str, item: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": item, "text": text });
        self.event(json!({ "type": "item.started", "turnId": turn, "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": turn, "item": message }));
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

    /// The first row showing `text`, and the column it starts in.
    fn find(&self, text: &str) -> (u16, u16) {
        for (y, line) in self.lines().iter().enumerate() {
            if let Some(byte) = line.find(text) {
                return (line[..byte].width() as u16, y as u16);
            }
        }
        panic!("{text:?} is not on screen:\n{}", self.text());
    }

    /// Each cell of row `y`, without those hidden under a wide character:
    /// the terminal draws the character and its background over both.
    fn cells(&self, y: u16) -> Vec<&Cell> {
        let buffer = self.buffer();
        let mut cells = Vec::new();
        let mut x = 0;
        while x < buffer.area.width {
            let cell = &buffer[(x, y)];
            cells.push(cell);
            x += cell.symbol().width().max(1) as u16;
        }
        cells
    }

    /// Every cell of row `y` is on `color`.
    fn assert_filled(&self, y: u16, color: Color) {
        for (x, cell) in self.cells(y).into_iter().enumerate() {
            assert_eq!(
                cell.bg,
                color,
                "row {y} column {x} ({:?}):\n{}",
                cell.symbol(),
                self.text()
            );
        }
    }

    /// No cell of row `y` has a background.
    fn assert_plain(&self, y: u16) {
        self.assert_filled(y, Color::Reset);
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

    fn select(&mut self, from: (u16, u16), to: (u16, u16)) {
        self.mouse(MouseEventKind::Down(MouseButton::Left), from);
        self.mouse(MouseEventKind::Drag(MouseButton::Left), to);
        self.mouse(MouseEventKind::Up(MouseButton::Left), to);
    }

    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, modifiers), NOW);
    }

    fn clipboard(&self) -> String {
        self.copied
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("nothing was copied:\n{}", self.text()))
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

/// The sketch in the task: an ordinary blank row, a row of background, the
/// message on the background to the edge of the window, a row of
/// background, an ordinary blank row; `›` and the text keep their colours.
#[test]
fn a_sent_message_is_on_its_background_with_a_row_of_it_above_and_below() {
    for (theme, color) in [
        (DARK, DARK_BG),
        (
            Theme {
                depth: Depth::TrueColor,
                background: Background::Light,
            },
            Color::Rgb(0xe9, 0xe9, 0xe5),
        ),
        (
            Theme {
                depth: Depth::Ansi256,
                background: Background::Dark,
            },
            Color::Indexed(236),
        ),
        (
            Theme {
                depth: Depth::Ansi256,
                background: Background::Light,
            },
            Color::Indexed(254),
        ),
    ] {
        let screen = Screen::conversation(theme, WIDTH);
        let lines = screen.lines();
        let (_, first) = screen.find("› 把 README");
        let (_, second) = screen.find("另外顺手");
        assert_eq!(second, first + 1, "{}", screen.text());
        let answer = screen.find("Worked for").1;
        let read = screen.find("Read README.md").1;
        assert_eq!(
            lines[usize::from(answer)..=usize::from(read)],
            [
                "  ─ Worked for <1s ─",
                "",
                "",
                "› 把 README 里的安装步骤改成 cargo install",
                "  另外顺手把版本号也改一下",
                "",
                "",
                "  ◇ Read README.md  lines 1–2",
            ],
            "{theme:?}"
        );
        screen.assert_plain(answer + 1);
        screen.assert_filled(first - 1, color);
        screen.assert_filled(first, color);
        screen.assert_filled(second, color);
        screen.assert_filled(second + 1, color);
        screen.assert_plain(second + 2);
        screen.assert_plain(read);

        let mark = screen.cells(first)[0];
        assert_eq!(mark.symbol(), "›");
        assert_eq!(mark.fg, Color::Cyan, "{theme:?}");
        assert!(mark.modifier.contains(Modifier::BOLD));
        assert_eq!(screen.cells(first)[2].fg, Color::Reset, "{theme:?}");
    }
}

/// With 16 colours, an unknown background or `NO_COLOR` the message is drawn
/// as before: a blank row above and below, no background.
#[test]
fn no_background_with_16_colours_or_an_unknown_background() {
    for theme in [
        Theme {
            depth: Depth::Ansi16,
            background: Background::Dark,
        },
        Theme {
            depth: Depth::TrueColor,
            background: Background::Unknown,
        },
        Theme {
            depth: Depth::None,
            background: Background::Light,
        },
    ] {
        let screen = Screen::conversation(theme, WIDTH);
        let lines = screen.lines();
        let answer = screen.find("Worked for").1;
        let read = screen.find("Read README.md").1;
        assert_eq!(
            lines[usize::from(answer)..=usize::from(read)],
            [
                "  ─ Worked for <1s ─",
                "",
                "› 把 README 里的安装步骤改成 cargo install",
                "  另外顺手把版本号也改一下",
                "",
                "  ◇ Read README.md  lines 1–2",
            ],
            "{theme:?}"
        );
        for y in 0..HEIGHT {
            for cell in screen.cells(y) {
                assert_eq!(cell.bg, Color::Reset, "{theme:?}:\n{}", screen.text());
            }
        }
    }
}

/// A queued message is not sent yet: no background.
#[test]
fn a_queued_message_has_no_background() {
    let mut screen = Screen::conversation(DARK, WIDTH);
    screen.event(json!({ "type": "input.queued", "input": {
        "id": "q1", "input": [{ "type": "text", "text": "later please" }],
        "clientName": "canter-tui",
    }}));
    screen.draw();
    let queued = screen.find("Queued: later please").1;
    screen.assert_plain(queued);
    screen.assert_plain(queued - 1);
}

/// A long line wraps under the first row's text, each row on the background
/// to the edge.
#[test]
fn wrapped_rows_line_up_under_the_text_and_are_all_on_the_background() {
    let screen = Screen::conversation(DARK, 24);
    let lines = screen.lines();
    let (_, first) = screen.find("› 把");
    // The last row of the message: the one before the row of background.
    let last = (first..HEIGHT)
        .find(|&y| lines[usize::from(y) + 1].is_empty())
        .unwrap();
    assert!(last > first + 2, "it wraps:\n{}", screen.text());
    for y in first + 1..=last {
        let line = &lines[usize::from(y)];
        assert!(
            line.starts_with("  ") && !line[2..].starts_with(' '),
            "row {y} starts under the text:\n{}",
            screen.text()
        );
    }
    for y in first - 1..=last + 1 {
        screen.assert_filled(y, DARK_BG);
    }
    screen.assert_plain(first - 2);
    screen.assert_plain(last + 2);
}

/// A review turn's title is a sent message too.
#[test]
fn a_review_title_is_on_the_background() {
    let mut screen = Screen::new(DARK, WIDTH);
    let mut turn = turn_json("t1", "review", "inProgress");
    turn["review"] = json!("Review: current changes");
    screen.event(json!({ "type": "turn.started", "turn": turn }));
    screen.say("t1", "m1", "Looks fine.");
    screen.draw();
    let title = screen.find("Review: current changes").1;
    for y in title - 1..=title + 1 {
        screen.assert_filled(y, DARK_BG);
    }
    let lines = screen.lines();
    assert_eq!(lines[usize::from(title) + 2], "", "{}", screen.text());
    screen.assert_plain(title + 2);
    assert!(lines[usize::from(title) + 3].contains("Looks fine."));
}

/// Dragging from the row above the message to the row below copies the
/// message's lines only: not the background rows, not the spaces after the
/// text. The selection is reversed over the background.
#[test]
fn a_copy_leaves_out_the_background_rows_and_spaces() {
    let mut screen = Screen::conversation(DARK, WIDTH);
    let (_, first) = screen.find("› 把");
    let second = first + 1;
    screen.select((0, first - 1), (WIDTH - 1, second + 1));
    screen.draw();
    assert_eq!(
        screen.clipboard(),
        "把 README 里的安装步骤改成 cargo install\n另外顺手把版本号也改一下"
    );
    for y in [first, second] {
        let cells = screen.cells(y);
        let reversed: Vec<&&Cell> = cells
            .iter()
            .filter(|cell| cell.modifier.contains(Modifier::REVERSED))
            .collect();
        assert!(!reversed.is_empty(), "{}", screen.text());
        for cell in reversed {
            assert_eq!(cell.bg, Color::Reset, "reversed over the background");
        }
        // The spaces after the text stay on the background, not selected.
        let last = cells.last().unwrap();
        assert!(!last.modifier.contains(Modifier::REVERSED));
        assert_eq!(last.bg, DARK_BG);
    }
    screen.assert_filled(first - 1, DARK_BG);
    screen.assert_filled(second + 1, DARK_BG);
}

/// The current match is reversed over the background; the other matches
/// are underlined on it.
#[test]
fn search_matches_show_on_the_background() {
    let mut screen = Screen::new(DARK, WIDTH);
    screen.event(json!({ "type": "turn.started",
        "turn": turn_json("t1", "fix cargo\nthen cargo again", "inProgress") }));
    screen.draw();
    screen.key(KeyCode::Char('f'), KeyModifiers::CONTROL);
    for c in "cargo".chars() {
        screen.key(KeyCode::Char(c), KeyModifiers::NONE);
    }
    screen.draw();
    let current = screen.find("then cargo").1;
    let other = screen.find("fix cargo").1;
    let reversed: Vec<&Cell> = screen
        .cells(current)
        .into_iter()
        .filter(|cell| cell.modifier.contains(Modifier::REVERSED))
        .collect();
    assert_eq!(
        reversed
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>(),
        "cargo",
        "{}",
        screen.text()
    );
    for cell in reversed {
        assert_eq!(cell.bg, Color::Reset, "reversed over the background");
    }
    let underlined: Vec<&Cell> = screen
        .cells(other)
        .into_iter()
        .filter(|cell| cell.modifier.contains(Modifier::UNDERLINED))
        .collect();
    assert_eq!(underlined.len(), "cargo".len(), "{}", screen.text());
    for cell in underlined {
        assert_eq!(cell.bg, DARK_BG);
    }
    let last = *screen.cells(current).last().unwrap();
    assert_eq!(last.bg, DARK_BG);
}
