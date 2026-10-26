//! The two shimmers: a light sweeping over the working verb on the status
//! line, and a cyan light over selected text once it is copied; and the
//! screen redrawn more often only while the copy's light sweeps. The
//! clipboard is a fake here, and time is what the tests say it is.

use std::time::{Duration, Instant};

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
/// A whole number of two-second sweeps: the verb's light is off its left end.
const NOW: i64 = 1_000_000;
const WIDTH: u16 = 60;
const HEIGHT: u16 = 20;
/// Rows under the conversation: the status line, the input box (3) and the
/// info rows (2).
const BELOW: u16 = 6;
const WAIT: Duration = Duration::from_secs(5);
const NORMAL: Duration = Duration::from_millis(100);
const FAST: Duration = Duration::from_millis(33);

const DARK: Theme = Theme {
    depth: Depth::TrueColor,
    background: Background::Dark,
};
const LIGHT: Theme = Theme {
    depth: Depth::TrueColor,
    background: Background::Light,
};
const SIXTEEN: Theme = Theme {
    depth: Depth::Ansi16,
    background: Background::Unknown,
};
const NO_COLOR: Theme = Theme {
    depth: Depth::None,
    background: Background::Unknown,
};

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    now: i64,
}

impl Screen {
    /// Draws for `theme` (per thread: each test has its own); the clipboard
    /// takes everything.
    fn new(theme: Theme) -> Self {
        Self::with_writer(theme, |_| Ok(()))
    }

    fn with_writer(
        theme: Theme,
        writer: impl FnMut(&str) -> Result<(), String> + Send + 'static,
    ) -> Self {
        theme::set(theme);
        let app =
            App::new(SessionSnapshot::empty(SESSION), None, NOW).with_clipboard_writer(writer);
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(WIDTH, HEIGHT), app).unwrap(),
            seq: 0,
            now: NOW,
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

    fn start_turn(&mut self) {
        self.event(json!({ "type": "turn.started", "turn": turn_json("inProgress") }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn start_item(&mut self, item: Value) {
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": item }));
    }

    /// A turn in progress with one finished message of `line 1` to `line 5`.
    fn lines_turn(theme: Theme) -> Self {
        let mut screen = Self::new(theme);
        screen.start_turn();
        screen.say();
        screen.draw();
        screen
    }

    fn say(&mut self) {
        let lines: Vec<String> = (1..=5).map(|n| format!("line {n}")).collect();
        let message = json!({ "type": "agentMessage", "id": "m1", "text": lines.join("\n") });
        self.start_item(message.clone());
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": message }));
    }

    fn draw(&mut self) {
        self.ui.draw(self.now).unwrap();
    }

    fn draw_at(&mut self, now: i64) -> &Buffer {
        self.now = now;
        self.draw();
        self.buffer()
    }

    fn buffer(&self) -> &Buffer {
        self.ui.backend().buffer()
    }

    fn text(&self) -> String {
        (0..HEIGHT)
            .map(|y| row(self.buffer(), y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn status_y(&self) -> u16 {
        HEIGHT - BELOW
    }

    fn status(&self) -> String {
        row(self.buffer(), self.status_y()).trim().to_owned()
    }

    /// The status line's cells from where `text` starts, one per character.
    fn status_cells(&self, text: &str) -> Vec<Cell> {
        let y = self.status_y();
        let line = row(self.buffer(), y);
        let start = line
            .find(text)
            .unwrap_or_else(|| panic!("{text:?} is not on the status line: {line:?}"));
        let x = line[..start].width() as u16;
        (0..text.chars().count() as u16)
            .map(|i| self.buffer()[(x + i, y)].clone())
            .collect()
    }

    /// The cell where `text` first starts.
    fn find(&self, text: &str) -> (u16, u16) {
        for y in 0..HEIGHT {
            let line = row(self.buffer(), y);
            if let Some(byte) = line.find(text) {
                return (line[..byte].width() as u16, y);
            }
        }
        panic!("{text:?} is not on screen:\n{}", self.text());
    }

    fn mouse(&mut self, kind: MouseEventKind, (column, row): (u16, u16)) {
        let event = MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        self.ui.app_mut().handle_mouse_at(event, self.now);
    }

    /// Drags from the start of `line 2` to the end of `line 3` and lets go.
    fn select_lines(&mut self) -> [(u16, u16); 2] {
        let from = self.find("line 2");
        let (x, y) = self.find("line 3");
        let to = (x + 5, y);
        self.mouse(MouseEventKind::Down(MouseButton::Left), from);
        self.mouse(MouseEventKind::Drag(MouseButton::Left), to);
        self.mouse(MouseEventKind::Up(MouseButton::Left), to);
        [from, (x, y)]
    }

    /// Draws, at the same time, until the status line shows `text`.
    fn wait_status(&mut self, text: &str) {
        let start = Instant::now();
        loop {
            self.draw();
            if self.status().contains(text) {
                return;
            }
            assert!(
                start.elapsed() < WAIT,
                "the status line never showed {text:?}:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The six cells of `line 2` and of `line 3`, starting at `rows`.
    fn selected(&self, rows: [(u16, u16); 2]) -> [Vec<Cell>; 2] {
        rows.map(|(x, y)| (x..x + 6).map(|x| self.buffer()[(x, y)].clone()).collect())
    }

    fn redraw_every(&self) -> Duration {
        self.ui.app().redraw_every(self.now)
    }
}

fn turn_json(status: &str) -> Value {
    json!({
        "id": "t1", "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "go" }],
        "clientName": "canter-tui", "usage": null,
    })
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

fn reasoning() -> Value {
    json!({ "type": "reasoning", "id": "r1", "summary": [], "content": [] })
}

fn rgb(cell: &Cell) -> (u8, u8, u8) {
    match cell.fg {
        Color::Rgb(r, g, b) => (r, g, b),
        other => panic!(
            "{:?} is drawn in {other:?}, not a true colour",
            cell.symbol()
        ),
    }
}

fn bold(cells: &[Cell]) -> String {
    cells
        .iter()
        .filter(|cell| cell.modifier.contains(Modifier::BOLD))
        .map(Cell::symbol)
        .collect()
}

/// Selected and drawn as the plain highlight: reversed, no colours of its own.
fn plain_highlight(cell: &Cell) -> bool {
    cell.modifier.contains(Modifier::REVERSED) && cell.fg == Color::Reset && cell.bg == Color::Reset
}

/// How far a background is from the copy's cyan; `None` without one.
fn off_cyan(cell: &Cell, cyan: (i32, i32, i32)) -> Option<i32> {
    match cell.bg {
        Color::Rgb(r, g, b) => Some(
            (i32::from(r) - cyan.0).abs()
                + (i32::from(g) - cyan.1).abs()
                + (i32::from(b) - cyan.2).abs(),
        ),
        _ => None,
    }
}

/// On a dark true-colour terminal the verb is grey, and a light sweeps it
/// left to right every two seconds, by the time: one second in, it is on
/// the word's middle letter, white and bold there, bold either side.
#[test]
fn the_working_verb_shimmers_on_a_dark_terminal() {
    let mut screen = Screen::new(DARK);
    screen.start_turn();
    screen.start_item(reasoning());

    screen.draw_at(NOW);
    let cells = screen.status_cells("Thinking");
    for cell in &cells {
        assert_eq!(rgb(cell), (0x7f, 0x7b, 0x75), "grey away from the light");
    }
    assert_eq!(bold(&cells), "");

    screen.draw_at(NOW + 1000);
    let cells = screen.status_cells("Thinking");
    assert_eq!(rgb(&cells[4]), (249, 248, 248), "the light is on the k");
    assert_eq!(bold(&cells), "inkin");
    let light = |cell: &Cell| {
        let (r, g, b) = rgb(cell);
        u32::from(r) + u32::from(g) + u32::from(b)
    };
    assert!(light(&cells[3]) < light(&cells[4]) && light(&cells[2]) < light(&cells[3]));
    assert!(light(&cells[0]) < light(&cells[2]));
    // The rest of the line is as before.
    assert!(
        screen
            .status()
            .ends_with("Thinking · 1s · Esc to interrupt"),
        "{}",
        screen.status()
    );
    for cell in screen.status_cells(" · 1s · Esc to interrupt") {
        assert_eq!(cell.fg, Color::Reset);
        assert_eq!(cell.modifier, Modifier::DIM);
    }

    // Another sweep, the same place.
    screen.draw_at(NOW + 3000);
    assert_eq!(bold(&screen.status_cells("Thinking")), "inkin");
}

/// On a light terminal the light is black.
#[test]
fn the_working_verb_shimmers_on_a_light_terminal() {
    let mut screen = Screen::new(LIGHT);
    screen.start_turn();
    screen.start_item(reasoning());
    screen.draw_at(NOW);
    for cell in screen.status_cells("Thinking") {
        assert_eq!(rgb(&cell), (0x8a, 0x85, 0x7e));
    }
    screen.draw_at(NOW + 1000);
    let cells = screen.status_cells("Thinking");
    assert_eq!(rgb(&cells[4]), (7, 7, 6));
    assert_eq!(bold(&cells), "inkin");
}

/// With 16 colours the light is three steps: dim away from it, normal
/// near it, bold in its middle.
#[test]
fn with_16_colours_the_verb_shimmers_in_three_steps() {
    let mut screen = Screen::new(SIXTEEN);
    screen.start_turn();
    screen.start_item(reasoning());
    screen.draw_at(NOW);
    for cell in screen.status_cells("Thinking") {
        assert_eq!((cell.fg, cell.modifier), (Color::Reset, Modifier::DIM));
    }
    screen.draw_at(NOW + 1000);
    let cells = screen.status_cells("Thinking");
    let steps: Vec<Modifier> = cells.iter().map(|cell| cell.modifier).collect();
    let (dim, normal, bold) = (Modifier::DIM, Modifier::empty(), Modifier::BOLD);
    assert_eq!(steps, [dim, normal, bold, bold, bold, bold, bold, normal]);
    assert!(cells.iter().all(|cell| cell.fg == Color::Reset));
}

/// Only the verb shimmers: a running command after it stays as it was.
#[test]
fn a_running_command_does_not_shimmer() {
    for theme in [DARK, SIXTEEN] {
        let mut screen = Screen::new(theme);
        screen.start_turn();
        screen.start_item(json!({
            "type": "commandExecution", "id": "c1", "command": "cargo test", "cwd": "/work",
            "status": "inProgress", "aggregatedOutput": "", "exitCode": null, "durationMs": null,
        }));
        screen.draw_at(NOW + 1000);
        assert!(
            !bold(&screen.status_cells("Running")).is_empty(),
            "{theme:?}"
        );
        for cell in screen.status_cells(" cargo test") {
            assert_eq!(
                (cell.fg, cell.modifier),
                (Color::Reset, Modifier::empty()),
                "{theme:?}"
            );
        }
    }
}

/// Without colours, and on any line other than the working one, nothing
/// sweeps: the same at any time.
#[test]
fn nothing_shimmers_without_colours_or_while_not_working() {
    let mut screen = Screen::new(NO_COLOR);
    screen.start_turn();
    screen.start_item(reasoning());
    for now in [NOW, NOW + 1000] {
        screen.draw_at(now);
        for cell in screen.status_cells("Thinking") {
            assert_eq!((cell.fg, cell.modifier), (Color::Reset, Modifier::empty()));
        }
    }

    let mut screen = Screen::new(DARK);
    screen.start_turn();
    screen.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    let looks = |screen: &mut Screen, now| {
        screen.draw_at(now);
        screen.status_cells("Waiting for approval")
    };
    assert_eq!(looks(&mut screen, NOW), looks(&mut screen, NOW + 1000));

    screen.event(json!({ "type": "turn.completed", "turn": turn_json("completed") }));
    screen.event(json!({ "type": "session.status", "status": "idle" }));
    screen.draw_at(NOW + 1000);
    assert_eq!(screen.status(), "");
    assert_eq!(screen.redraw_every(), NORMAL);
}

/// The status line's light does not draw the screen more often.
#[test]
fn the_verb_shimmers_at_the_usual_ten_frames_a_second() {
    let mut screen = Screen::new(DARK);
    screen.start_turn();
    screen.start_item(reasoning());
    for now in [NOW, NOW + 500, NOW + 1000] {
        screen.draw_at(now);
        assert_eq!(screen.redraw_every(), NORMAL);
    }
}

/// On a dark true-colour terminal, once the copy worked a cyan light runs
/// left to right over the highlight for half a second, the lower row a
/// little behind; then the plain highlight. Meanwhile the screen is drawn
/// about thirty times a second, then ten again.
#[test]
fn a_copy_sweeps_a_cyan_light_over_the_highlight() {
    let mut screen = Screen::lines_turn(DARK);
    assert_eq!(screen.redraw_every(), NORMAL);
    let rows = screen.select_lines();
    screen.wait_status("✓ Copied 2 lines · 13 chars");
    let start = screen.now;
    for cells in screen.selected(rows) {
        assert!(cells.iter().all(plain_highlight), "not yet: {cells:?}");
    }
    assert_eq!(screen.redraw_every(), FAST);

    screen.draw_at(start + 250);
    assert_eq!(screen.redraw_every(), FAST);
    let cyan = (0x56, 0xc2, 0xcf);
    let peaks = screen.selected(rows).map(|cells| {
        let lit: Vec<(usize, i32)> = cells
            .iter()
            .enumerate()
            .filter_map(|(x, cell)| off_cyan(cell, cyan).map(|off| (x, off)))
            .collect();
        assert!(!lit.is_empty(), "no light: {cells:?}");
        for (x, _) in &lit {
            let cell = &cells[*x];
            assert!(!cell.modifier.contains(Modifier::REVERSED));
            assert_eq!(
                cell.fg,
                Color::Rgb(0x1c, 0x1d, 0x20),
                "dark text on the light"
            );
        }
        lit.iter().min_by_key(|(_, off)| *off).unwrap().0
    });
    assert!(peaks[1] < peaks[0], "the lower row lags: {peaks:?}");
    let [first, _] = screen.selected(rows);
    let brightest = off_cyan(&first[peaks[0]], cyan).unwrap();
    assert!(brightest < 40, "nearly cyan in its middle: {brightest}");

    screen.draw_at(start + 500);
    for cells in screen.selected(rows) {
        assert!(cells.iter().all(plain_highlight), "over: {cells:?}");
    }
    assert_eq!(screen.redraw_every(), NORMAL);
}

/// On a light terminal the light goes from the highlight's dark to a deep
/// cyan, under light text.
#[test]
fn on_a_light_terminal_the_copy_light_is_deep_cyan() {
    let mut screen = Screen::lines_turn(LIGHT);
    let rows = screen.select_lines();
    screen.wait_status("✓ Copied");
    let start = screen.now;
    screen.draw_at(start + 250);
    let [first, _] = screen.selected(rows);
    let lit: Vec<&Cell> = first
        .iter()
        .filter(|cell| off_cyan(cell, (0x0f, 0x8a, 0x99)).is_some())
        .collect();
    assert!(!lit.is_empty(), "{first:?}");
    let nearest = lit
        .iter()
        .filter_map(|cell| off_cyan(cell, (0x0f, 0x8a, 0x99)))
        .min()
        .unwrap();
    assert!(nearest < 40, "{nearest}");
    for cell in lit {
        assert_eq!(cell.fg, Color::Rgb(0xfb, 0xfb, 0xf9));
    }
}

/// With 16 colours the light is a band four cells wide of plain cyan.
#[test]
fn with_16_colours_the_copy_light_is_a_cyan_band() {
    let mut screen = Screen::lines_turn(SIXTEEN);
    let rows = screen.select_lines();
    screen.wait_status("✓ Copied");
    let start = screen.now;
    screen.draw_at(start + 250);
    let bands = screen.selected(rows).map(|cells| {
        let band: Vec<usize> = cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| {
                cell.fg == Color::Cyan && cell.modifier.contains(Modifier::REVERSED)
            })
            .map(|(x, _)| x)
            .collect();
        for (x, cell) in cells.iter().enumerate() {
            if !band.contains(&x) {
                assert!(plain_highlight(cell), "{cell:?}");
            }
        }
        band
    });
    assert_eq!(bands[0].len(), 4, "{bands:?}");
    assert_eq!(bands[1].len(), 4, "{bands:?}");
    assert!(bands[0].windows(2).all(|pair| pair[1] == pair[0] + 1));
    assert!(bands[1][0] < bands[0][0], "the lower row lags: {bands:?}");

    screen.draw_at(start + 500);
    for cells in screen.selected(rows) {
        assert!(cells.iter().all(plain_highlight));
    }
}

/// A copy that failed does not sweep, nor does any without colours; and
/// the screen is drawn as often as usual.
#[test]
fn a_failed_copy_or_no_colours_sweeps_nothing() {
    for (theme, worked) in [(DARK, false), (SIXTEEN, false), (NO_COLOR, true)] {
        let mut screen = Screen::with_writer(theme, move |_| {
            if worked {
                Ok(())
            } else {
                Err("no display".into())
            }
        });
        screen.start_turn();
        screen.say();
        screen.draw();
        let rows = screen.select_lines();
        screen.wait_status(if worked { "Copied" } else { "Copy failed" });
        let start = screen.now;
        for now in [start, start + 100, start + 250, start + 400] {
            screen.draw_at(now);
            for cells in screen.selected(rows) {
                assert!(cells.iter().all(plain_highlight), "{theme:?}: {cells:?}");
            }
            assert_eq!(screen.redraw_every(), NORMAL, "{theme:?}");
        }
    }
}

/// Esc in the middle of the sweep takes the highlight and its light away,
/// and the screen goes back to ten frames a second.
#[test]
fn clearing_the_selection_ends_the_sweep() {
    let mut screen = Screen::lines_turn(DARK);
    let rows = screen.select_lines();
    screen.wait_status("✓ Copied");
    let start = screen.now;
    screen.draw_at(start + 100);
    screen
        .ui
        .app_mut()
        .handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), start + 100);
    screen.draw_at(start + 200);
    for cells in screen.selected(rows) {
        for cell in cells {
            assert_eq!(cell.bg, Color::Reset);
            assert!(!cell.modifier.contains(Modifier::REVERSED));
        }
    }
    assert_eq!(screen.redraw_every(), NORMAL);
}

/// `/copy` has no selection: nothing sweeps.
#[test]
fn slash_copy_does_not_sweep() {
    let mut screen = Screen::lines_turn(DARK);
    screen.event(json!({ "type": "turn.completed", "turn": turn_json("completed") }));
    screen.event(json!({ "type": "session.status", "status": "idle" }));
    for c in "/copy".chars() {
        screen
            .ui
            .app_mut()
            .handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), NOW);
    }
    screen
        .ui
        .app_mut()
        .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), NOW);
    screen.wait_status("✓ Copied");
    assert_eq!(screen.redraw_every(), NORMAL);
    screen.draw_at(NOW + 250);
    let buffer = screen.buffer();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            assert_ne!(buffer[(x, y)].fg, Color::Rgb(0x1c, 0x1d, 0x20));
        }
    }
}
