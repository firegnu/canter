//! Selecting and copying from the conversation: dragging highlights the text
//! and copies it on release, without the marks, borders, line numbers and
//! indents around it; double and triple clicks; `/copy`. The clipboard is a
//! fake here: tests never touch the real one, and nothing goes to a terminal.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 1_000_000;
const WIDTH: u16 = 60;
const HEIGHT: u16 = 20;
/// Rows under the conversation: the status line, the input box (3) and the
/// info rows (2).
const BELOW: u16 = 6;
/// How long the screen waits on the fake clipboard's thread.
const WAIT: Duration = Duration::from_secs(5);

/// A diff like the task's sketch: three lines replaced by one.
const README_DIFF: &str = "--- a/README.md
+++ b/README.md
@@ -13,5 +13,3 @@
 ```sh
-git clone https://github.com/firegnu/canter
-cd canter
-cargo build --release
+cargo install --path crates/canter
 ```
";

/// One paragraph, longer than a row.
const DONE: &str = "Done. The README now tells you to install canter with cargo \
                    install from the crates directory.";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    /// When frames are drawn and the mouse and keys are used.
    now: i64,
    /// What the fake clipboard was given.
    copied: mpsc::Receiver<String>,
}

impl Screen {
    fn new() -> Self {
        Self::sized(WIDTH, HEIGHT)
    }

    fn sized(width: u16, height: u16) -> Self {
        let (sender, copied) = mpsc::channel();
        Self::with_writer(width, height, copied, move |text: &str| {
            let _ = sender.send(text.to_owned());
            Ok(())
        })
    }

    fn with_writer(
        width: u16,
        height: u16,
        copied: mpsc::Receiver<String>,
        writer: impl FnMut(&str) -> Result<(), String> + Send + 'static,
    ) -> Self {
        let app =
            App::new(SessionSnapshot::empty(SESSION), None, NOW).with_clipboard_writer(writer);
        let ui = Ui::new(TestBackend::new(width, height), app).unwrap();
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

    /// A finished edit of `README.md` with [`README_DIFF`].
    fn edit_readme(&mut self, turn: &str) {
        self.event(json!({ "type": "item.completed", "turnId": turn, "item": {
            "type": "fileChange", "id": "e1", "status": "completed",
            "changes": [{
                "path": "/work/README.md",
                "kind": { "type": "update", "movePath": null },
                "diff": README_DIFF,
            }],
        }}));
    }

    /// A finished turn that edits the README and then says [`DONE`].
    fn readme_turn() -> Self {
        let mut screen = Self::new();
        screen.start_turn("t1", "update the readme");
        screen.edit_readme("t1");
        screen.say("t1", "m1", DONE);
        screen.finish_turn("t1", "update the readme");
        screen.draw();
        screen
    }

    /// A turn with one finished message of `line 1` to `line {count}`.
    fn long_turn(count: usize) -> Self {
        let mut screen = Self::new();
        screen.start_turn("t1", "show many lines");
        let lines: Vec<String> = (1..=count).map(|n| format!("line {n}")).collect();
        screen.say("t1", "m1", &lines.join("\n"));
        screen.draw();
        screen
    }

    fn draw(&mut self) {
        self.ui.draw(self.now).unwrap();
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

    /// The status line: the row above the input box.
    fn status(&self) -> String {
        let height = self.buffer().area.height;
        row(self.buffer(), height - BELOW).trim().to_owned()
    }

    /// The input box's first row, without its borders.
    fn input(&self) -> String {
        let height = self.buffer().area.height;
        row(self.buffer(), height - BELOW + 2)
            .trim_start_matches('│')
            .trim()
            .trim_end_matches('│')
            .trim()
            .to_owned()
    }

    /// The cell where `text` first starts, and its row.
    fn find(&self, text: &str) -> (u16, u16) {
        let buffer = self.buffer();
        for y in 0..buffer.area.height {
            let row = row(buffer, y);
            if let Some(byte) = row.find(text) {
                return (row[..byte].width() as u16, y);
            }
        }
        panic!("{text:?} is not on screen:\n{}", self.text());
    }

    /// The cell of the last character of `text`, found as [`Screen::find`].
    fn find_end(&self, text: &str) -> (u16, u16) {
        let (x, y) = self.find(text);
        let last = text.chars().last().unwrap().to_string();
        (x + (text.width() - last.width()) as u16, y)
    }

    /// The highlighted text on each row that has some, and the row.
    fn highlighted(&self) -> Vec<(u16, String)> {
        let buffer = self.buffer();
        (0..buffer.area.height)
            .filter_map(|y| {
                let mut text = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let cell = &buffer[(x, y)];
                    if cell.modifier.contains(Modifier::REVERSED) {
                        text.push_str(cell.symbol());
                    }
                    x += cell.symbol().width().max(1) as u16;
                }
                (!text.is_empty()).then_some((y, text))
            })
            .collect()
    }

    fn highlighted_text(&self) -> Vec<String> {
        self.highlighted()
            .into_iter()
            .map(|(_, text)| text)
            .collect()
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

    fn press(&mut self, at: (u16, u16)) {
        self.mouse(MouseEventKind::Down(MouseButton::Left), at);
    }

    fn drag(&mut self, at: (u16, u16)) {
        self.mouse(MouseEventKind::Drag(MouseButton::Left), at);
    }

    fn release(&mut self, at: (u16, u16)) {
        self.mouse(MouseEventKind::Up(MouseButton::Left), at);
    }

    fn click(&mut self, at: (u16, u16)) {
        self.press(at);
        self.release(at);
    }

    /// Drags from one cell to another and lets go.
    fn select(&mut self, from: (u16, u16), to: (u16, u16)) {
        self.press(from);
        self.drag(to);
        self.release(to);
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.key_with(code, KeyModifiers::NONE)
    }

    fn key_with(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, modifiers), self.now)
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    fn enter(&mut self, text: &str) -> Option<Action> {
        self.type_text(text);
        self.key(KeyCode::Enter)
    }

    /// What the fake clipboard was given next.
    fn clipboard(&self) -> String {
        self.copied
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("nothing was copied:\n{}", self.text()))
    }

    /// Nothing more was given to the fake clipboard.
    fn nothing_copied(&self) {
        if let Ok(text) = self.copied.recv_timeout(Duration::from_millis(200)) {
            panic!("copied {text:?} unexpectedly");
        }
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

/// The sketch in the task: dragging over a file change's removed lines, on
/// to the answer below it, highlights only the text and copies it without
/// the card's mark, the line numbers, the `+`/`-`, the indents or the row
/// breaks the narrow window made.
#[test]
fn dragging_over_a_card_and_an_answer_copies_only_their_text() {
    let mut screen = Screen::readme_turn();
    let from = screen.find("git clone");
    let last_row = screen
        .lines()
        .iter()
        .rposition(|line| line.starts_with("  ") && line.trim_end().ends_with("directory."))
        .unwrap_or_else(|| panic!("the answer's end:\n{}", screen.text()));
    let to = screen.find_end("directory.");
    assert_eq!(to.1 as usize, last_row);
    assert_ne!(
        screen.find("Done.").1,
        to.1,
        "the answer takes more than one row:\n{}",
        screen.text()
    );

    screen.press(from);
    screen.drag((to.0, to.1 - 1));
    screen.draw();
    assert!(
        !screen.highlighted().is_empty(),
        "highlighted while dragging:\n{}",
        screen.text()
    );
    screen.drag(to);
    screen.release(to);
    screen.draw();

    let highlighted = screen.highlighted_text();
    assert_eq!(
        highlighted[..5],
        [
            "git clone https://github.com/firegnu/canter",
            "cd canter",
            "cargo build --release",
            "cargo install --path crates/canter",
            "```",
        ],
        "only the text is highlighted, no numbers or marks:\n{}",
        screen.text()
    );
    assert!(
        highlighted[5].starts_with("Done. The README"),
        "{highlighted:?}"
    );
    assert!(highlighted.last().unwrap().ends_with("directory."));

    assert_eq!(
        screen.clipboard(),
        "git clone https://github.com/firegnu/canter\n\
         cd canter\n\
         cargo build --release\n\
         cargo install --path crates/canter\n\
         ```\n\
         \n\
         Done. The README now tells you to install canter with cargo install from the \
         crates directory."
    );
    screen.wait_status("✓ Copied 7 lines · 210 chars");
}

/// Part of a row copies only that part; a card's title loses its mark and
/// the status on the right.
#[test]
fn part_of_a_row_and_a_card_title_copy_only_their_text() {
    let mut screen = Screen::readme_turn();
    let from = screen.find("clone");
    let to = screen.find_end("github.com");
    screen.select(from, to);
    screen.draw();
    assert_eq!(screen.highlighted_text(), ["clone https://github.com"]);
    assert_eq!(screen.clipboard(), "clone https://github.com");

    let mut screen = Screen::new();
    screen.start_turn("t1", "test it");
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "commandExecution", "id": "c1", "command": "cargo test", "cwd": "/work",
        "status": "completed", "aggregatedOutput": "running 3 tests\n  ok\n",
        "exitCode": 0, "durationMs": 1200,
    }}));
    screen.draw();
    let title = screen.find("● Run cargo test");
    assert!(screen.lines()[title.1 as usize].ends_with("exit 0 · 1.2s"));
    let end = screen.find_end("  ok");
    screen.select(title, end);
    screen.draw();
    assert_eq!(
        screen.clipboard(),
        "Run cargo test\nrunning 3 tests\n  ok",
        "no mark, no status, no `│`; the output's own indent stays"
    );
}

/// Dragging to the bottom edge of the conversation scrolls it, also while
/// the pointer stays there, and the selection grows past the first screen.
#[test]
fn dragging_to_the_bottom_edge_scrolls_and_extends_the_selection() {
    let mut screen = Screen::long_turn(40);
    // To the top: `› show many lines`, a blank row, then `line 1` on.
    for _ in 0..20 {
        screen.mouse(MouseEventKind::ScrollUp, (10, 5));
    }
    screen.draw();
    assert!(
        screen.lines()[0].contains("show many lines"),
        "{}",
        screen.text()
    );
    let bottom = HEIGHT - BELOW - 1;
    assert_eq!(screen.lines()[bottom as usize].trim(), "line 12");

    screen.press(screen.find("line 3"));
    screen.drag((20, 8));
    screen.drag((20, bottom));
    screen.draw();
    screen.now += 50;
    screen.draw();
    screen.now += 50;
    screen.draw();
    assert_eq!(
        screen.lines()[bottom as usize].trim(),
        "line 15",
        "three rows on:\n{}",
        screen.text()
    );
    screen.release((20, bottom));
    let copied = screen.clipboard();
    let expected: Vec<String> = (3..=15).map(|n| format!("line {n}")).collect();
    assert_eq!(copied, expected.join("\n"));
}

/// The highlight is on the text, not the screen: after the wheel moves the
/// conversation, the same words are highlighted on other rows.
#[test]
fn the_highlight_stays_on_the_same_text_when_scrolled() {
    let mut screen = Screen::long_turn(40);
    for _ in 0..20 {
        screen.mouse(MouseEventKind::ScrollUp, (10, 5));
    }
    screen.draw();
    let from = screen.find("line 5");
    let to = screen.find_end("line 6");
    screen.select(from, to);
    screen.draw();
    let before = screen.highlighted();
    assert_eq!(
        before
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        ["line 5", "line 6"]
    );

    screen.mouse(MouseEventKind::ScrollDown, (10, 5));
    screen.draw();
    let after = screen.highlighted();
    assert_eq!(
        after
            .iter()
            .map(|(_, text)| text.as_str())
            .collect::<Vec<_>>(),
        ["line 5", "line 6"],
        "{}",
        screen.text()
    );
    assert_eq!(after[0].0 + 3, before[0].0, "three rows up");
}

/// A double click selects a word (paths and flags count as one), a triple
/// click the whole row on screen.
#[test]
fn double_click_selects_a_word_and_triple_click_a_row() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "how");
    screen.say(
        "t1",
        "m1",
        "Run cargo build --release in crates/canter now.",
    );
    screen.draw();

    let at = screen.find("argo");
    screen.click(at);
    screen.click(at);
    screen.draw();
    assert_eq!(screen.highlighted_text(), ["cargo"]);
    assert_eq!(screen.clipboard(), "cargo");

    screen.now += 1000;
    let at = screen.find("release");
    screen.click(at);
    screen.click(at);
    assert_eq!(screen.clipboard(), "--release");

    screen.now += 1000;
    let at = screen.find("canter now");
    screen.click(at);
    screen.click(at);
    assert_eq!(screen.clipboard(), "crates/canter");
    screen.click(at);
    screen.draw();
    assert_eq!(
        screen.highlighted_text(),
        ["Run cargo build --release in crates/canter now."]
    );
    assert_eq!(
        screen.clipboard(),
        "Run cargo build --release in crates/canter now."
    );

    // Clicks far apart in time are single clicks: nothing more is copied.
    screen.now += 1000;
    screen.click(at);
    screen.now += 1000;
    screen.click(at);
    screen.nothing_copied();
}

/// Esc, typing and a click elsewhere take the highlight away. Esc then
/// only does that: the turn goes on until the next Esc.
#[test]
fn esc_typing_or_a_click_clears_the_highlight() {
    let mut screen = Screen::long_turn(5);
    let select = |screen: &mut Screen| {
        let from = screen.find("line 2");
        let to = screen.find_end("line 3");
        screen.select(from, to);
        screen.draw();
        assert_eq!(screen.highlighted_text(), ["line 2", "line 3"]);
        screen.clipboard();
    };

    select(&mut screen);
    assert_eq!(screen.key(KeyCode::Esc), None, "Esc clears, not interrupts");
    screen.draw();
    assert!(screen.highlighted().is_empty(), "{}", screen.text());
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Interrupt {
            turn_id: "t1".into()
        })
    );

    select(&mut screen);
    screen.type_text("x");
    screen.draw();
    assert!(screen.highlighted().is_empty());
    assert_eq!(screen.input(), "› x");

    select(&mut screen);
    screen.click((5, HEIGHT - BELOW + 2));
    screen.draw();
    assert!(screen.highlighted().is_empty());
    screen.nothing_copied();
}

/// After a copy the status line says so for about two seconds, then shows
/// what it showed before.
#[test]
fn the_status_line_says_what_was_copied_for_two_seconds() {
    let mut screen = Screen::long_turn(5);
    screen.draw();
    let before = screen.status();
    let from = screen.find("line 1");
    let to = screen.find_end("line 2");
    screen.select(from, to);
    screen.wait_status("✓ Copied 2 lines · 13 chars");
    assert_eq!(screen.status(), "✓ Copied 2 lines · 13 chars");
    screen.now += 1900;
    screen.draw();
    assert_eq!(screen.status(), "✓ Copied 2 lines · 13 chars");
    screen.now += 200;
    screen.draw();
    assert!(before.contains("Working · 0s"), "{before}");
    assert!(
        screen.status().contains("Working · 2s") && !screen.status().contains("Copied"),
        "{}",
        screen.text()
    );
}

/// `/copy` copies the last answer as written (markdown), `/copy N` the code
/// of its Nth code block without the fences. Without an answer, or with too
/// few blocks, the conversation says so and the input stays.
#[test]
fn slash_copy_copies_the_last_answer_or_one_of_its_code_blocks() {
    let mut screen = Screen::new();
    assert_eq!(screen.enter("/copy"), None);
    screen.draw();
    assert!(
        screen.text().contains("Nothing to copy"),
        "{}",
        screen.text()
    );
    assert_eq!(screen.input(), "› /copy");
    screen.nothing_copied();

    let answer = "Two blocks:\n\n```sh\ncargo build\n```\n\nand\n\n```rust\nfn main() {}\n```\n";
    screen.key_with(KeyCode::Char('c'), KeyModifiers::CONTROL);
    screen.start_turn("t1", "show code");
    screen.say("t1", "m0", "An earlier answer.");
    screen.say("t1", "m1", answer);
    screen.finish_turn("t1", "show code");

    assert_eq!(screen.enter("/copy"), None);
    assert_eq!(screen.clipboard(), answer);
    screen.wait_status("✓ Copied 11 lines · ");
    screen.draw();
    assert!(
        screen.input().contains("Type a message"),
        "{}",
        screen.text()
    );

    assert_eq!(screen.enter("/copy 2"), None);
    assert_eq!(screen.clipboard(), "fn main() {}");
    screen.wait_status("✓ Copied 1 line · 12 chars");

    assert_eq!(screen.enter("/copy 1"), None);
    assert_eq!(screen.clipboard(), "cargo build");

    assert_eq!(screen.enter("/copy 3"), None);
    screen.draw();
    assert!(
        screen.text().contains("The last answer has 2 code blocks"),
        "{}",
        screen.text()
    );
    assert_eq!(screen.input(), "› /copy 3");
    screen.nothing_copied();
}

/// `/copy` is in the command list and in `/help`.
#[test]
fn slash_copy_is_listed() {
    let mut screen = Screen::sized(80, 30);
    screen.type_text("/cop");
    screen.draw();
    assert!(
        screen.text().contains("/copy") && screen.text().contains("Copy the last answer"),
        "{}",
        screen.text()
    );
    screen.key_with(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(screen.enter("/help"), None);
    screen.draw();
    let help = screen
        .lines()
        .into_iter()
        .find(|line| line.contains("/copy"))
        .unwrap_or_else(|| panic!("/help:\n{}", screen.text()));
    assert!(help.contains("Copy the last answer"), "{help}");
}

/// A selection that starts or ends on half of a wide character takes the
/// whole character.
#[test]
fn half_of_a_wide_character_selects_all_of_it() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "wide characters");
    screen.say("t1", "m1", "中文复制测试 done");
    screen.draw();
    let (x, y) = screen.find("文");
    // The right half of 文 to the left half of 测.
    screen.select((x + 1, y), (x + 6, y));
    screen.draw();
    assert_eq!(screen.highlighted_text(), ["文复制测"]);
    assert_eq!(screen.clipboard(), "文复制测");
}

/// While the answer is still being written, the highlight stays on the
/// words selected as more rows come and the conversation moves up.
#[test]
fn the_highlight_follows_the_text_while_the_answer_grows() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "stream");
    screen.event(json!({ "type": "item.started", "turnId": "t1",
        "item": { "type": "agentMessage", "id": "m1", "text": "alpha beta\n" } }));
    screen.draw();
    let at = screen.find("alpha");
    screen.click(at);
    screen.click(at);
    assert_eq!(screen.clipboard(), "alpha");
    screen.draw();
    assert_eq!(screen.highlighted(), [(at.1, "alpha".to_owned())]);

    for n in 0..30 {
        screen.event(
            json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": { "type": "agentMessage", "delta": format!("more {n}\n") } }),
        );
    }
    screen.draw();
    assert!(screen.text().contains("more 29"), "{}", screen.text());
    // `alpha` has scrolled off the top: nothing else lights up.
    assert!(screen.highlighted().is_empty(), "{}", screen.text());
    screen.mouse(MouseEventKind::ScrollUp, (10, 5));
    for _ in 0..20 {
        screen.mouse(MouseEventKind::ScrollUp, (10, 5));
    }
    screen.draw();
    let (_, y) = screen.find("alpha");
    assert_eq!(screen.highlighted(), [(y, "alpha".to_owned())]);
}

/// A new width or Ctrl+O wraps the conversation again: the selection goes.
#[test]
fn a_new_width_or_ctrl_o_clears_the_selection() {
    let mut screen = Screen::readme_turn();
    let select = |screen: &mut Screen| {
        let from = screen.find("cd canter");
        let to = screen.find_end("--release");
        screen.select(from, to);
        screen.draw();
        assert_eq!(
            screen.highlighted_text(),
            ["cd canter", "cargo build --release"]
        );
        screen.clipboard();
    };

    select(&mut screen);
    screen.key_with(KeyCode::Char('o'), KeyModifiers::CONTROL);
    screen.draw();
    assert!(screen.highlighted().is_empty(), "{}", screen.text());
    screen.key_with(KeyCode::Char('o'), KeyModifiers::CONTROL);
    screen.draw();

    select(&mut screen);
    screen.ui.backend_mut().resize(70, HEIGHT);
    screen.draw();
    assert!(screen.highlighted().is_empty(), "{}", screen.text());
}

/// The input box, the status line and the approval box are not selected
/// from: a drag there highlights and copies nothing.
#[test]
fn no_selecting_outside_the_conversation() {
    let mut screen = Screen::new();
    screen.type_text("some draft text");
    screen.start_turn("t1", "run it");
    screen.draw();
    let input = screen.find("some draft");
    screen.select(input, (input.0 + 8, input.1));
    let status = (2, HEIGHT - BELOW);
    screen.select(status, (20, status.1));
    screen.draw();
    assert!(screen.highlighted().is_empty(), "{}", screen.text());
    assert_eq!(screen.input(), "› some draft text");

    screen.event(json!({ "type": "item.started", "turnId": "t1", "item": {
        "type": "commandExecution", "id": "c1", "command": "rm -rf build", "cwd": "/work",
        "status": "inProgress", "aggregatedOutput": "", "exitCode": null, "durationMs": null,
    }}));
    screen.event(json!({ "type": "request.opened", "request": {
        "id": "r1", "turnId": "t1", "createdAt": NOW,
        "kind": {
            "type": "approval", "itemId": "c1", "tool": "shell",
            "arguments": { "command": "rm -rf build" },
            "availableDecisions": ["accept", "decline"],
        },
    }}));
    screen.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    screen.draw();
    let lines = screen.lines();
    let command = lines
        .iter()
        .rposition(|line| line.contains("rm -rf build"))
        .unwrap() as u16;
    assert!(
        lines[command as usize].starts_with('│'),
        "in the box:\n{}",
        screen.text()
    );
    screen.select((2, command), (30, command));
    screen.draw();
    assert!(screen.highlighted().is_empty(), "{}", screen.text());
    screen.nothing_copied();
}

/// When the clipboard refuses, the status line says why, in red.
#[test]
fn a_failed_copy_says_why_in_red() {
    let (_, copied) = mpsc::channel();
    let mut screen = Screen::with_writer(WIDTH, HEIGHT, copied, |_| {
        Err("no display to copy to".into())
    });
    screen.start_turn("t1", "say");
    screen.say("t1", "m1", "some words");
    screen.draw();
    let from = screen.find("some");
    screen.select(from, screen.find_end("words"));
    screen.wait_status("✗ Copy failed: no display to copy to");
    let (x, y) = screen.find("Copy failed");
    assert_eq!(screen.buffer()[(x, y)].fg, Color::Red);
}

/// A clipboard that does not answer never holds up the screen: keys and
/// frames go on, and after a while the status line says the copy failed.
#[test]
fn a_slow_clipboard_does_not_hold_up_the_screen() {
    let (unblock, blocked) = mpsc::channel::<()>();
    let (_, copied) = mpsc::channel();
    let mut screen = Screen::with_writer(WIDTH, HEIGHT, copied, move |_| {
        let _ = blocked.recv();
        Ok(())
    });
    screen.start_turn("t1", "say");
    screen.say("t1", "m1", "some words");
    screen.draw();
    let from = screen.find("some");
    let started = Instant::now();
    screen.select(from, screen.find_end("words"));
    screen.draw();
    screen.type_text("still typing");
    screen.draw();
    assert_eq!(screen.input(), "› still typing");
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(!screen.status().contains("Copied"), "{}", screen.text());

    screen.now += 3000;
    screen.draw();
    assert!(
        screen.status().starts_with("✗ Copy failed: "),
        "{}",
        screen.text()
    );
    drop(unblock);
}

/// A word the stream pushes on to the next row keeps its highlight.
#[test]
fn a_word_the_stream_wraps_again_stays_highlighted() {
    let mut screen = Screen::sized(20, 20);
    screen.start_turn("t1", "stream");
    screen.event(json!({ "type": "item.started", "turnId": "t1",
        "item": { "type": "agentMessage", "id": "m1", "text": "hello abcdef" } }));
    screen.draw();
    let from = screen.find("abcdef");
    screen.select(from, screen.find_end("abcdef"));
    assert_eq!(screen.clipboard(), "abcdef");

    screen.event(
        json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
        "delta": { "type": "agentMessage", "delta": "ghijklmn" } }),
    );
    screen.draw();
    assert_eq!(screen.highlighted_text(), ["abcdef"], "{}", screen.text());
}

/// A running command shows its last 5 lines: a new line moves them up, and
/// letting go where the drag ended still copies the line selected.
#[test]
fn new_command_output_does_not_move_the_selection_to_another_line() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "run");
    screen.event(json!({ "type": "item.started", "turnId": "t1", "item": {
        "type": "commandExecution", "id": "c1", "command": "build", "cwd": "/work",
        "status": "inProgress", "aggregatedOutput": "line1\nline2\nline3\nline4\nline5\nline6\n",
        "exitCode": null, "durationMs": null,
    }}));
    screen.draw();
    let from = screen.find("line3");
    let to = screen.find_end("line3");
    screen.press(from);
    screen.drag(to);
    screen.draw();
    assert_eq!(screen.highlighted_text(), ["line3"]);

    screen.event(
        json!({ "type": "item.delta", "turnId": "t1", "itemId": "c1",
        "delta": { "type": "commandExecution", "delta": "line7\n" } }),
    );
    screen.draw();
    assert_eq!(screen.highlighted_text(), ["line3"], "{}", screen.text());
    screen.release(to);
    assert_eq!(screen.clipboard(), "line3", "{}", screen.text());
}

/// A command that ends folds to its last 3 lines and an answer comes after
/// it: the selection stays on its line and never runs into the answer.
#[test]
fn a_command_folding_as_it_ends_keeps_the_selection_on_its_line() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "run");
    let command = |status: &str| {
        json!({
            "type": "commandExecution", "id": "c1", "command": "build", "cwd": "/work",
            "status": status, "aggregatedOutput": "line1\nline2\nline3\nline4\nline5\nline6\n",
            "exitCode": null, "durationMs": null,
        })
    };
    screen.event(json!({ "type": "item.started", "turnId": "t1", "item": command("inProgress") }));
    screen.draw();
    let from = screen.find("line6");
    let to = screen.find_end("line6");
    screen.press(from);
    screen.drag(to);

    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": command("completed") }));
    screen.say("t1", "m1", "abcdefghijklmnop");
    screen.draw();
    assert_eq!(screen.highlighted_text(), ["line6"], "{}", screen.text());
    screen.release(to);
    assert_eq!(screen.clipboard(), "line6", "{}", screen.text());
}

/// A queued message that leaves the queue takes its selection with it: the
/// next queued message, now where it was, is not copied instead.
#[test]
fn a_queued_message_leaving_the_queue_does_not_copy_the_next_one() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "busy");
    for (id, text) in [("q1", "alpha"), ("q2", "bravo")] {
        screen.event(json!({ "type": "input.queued", "input": {
            "id": id, "input": [{ "type": "text", "text": text }], "clientName": "canter-tui",
        }}));
    }
    screen.draw();
    let from = screen.find("alpha");
    let to = screen.find_end("alpha");
    screen.press(from);
    screen.drag(to);

    screen.finish_turn("t1", "busy");
    screen.event(json!({ "type": "turn.started", "inputId": "q1",
        "turn": turn_json("t2", "alpha", "inProgress") }));
    screen.draw();
    assert!(
        !screen.highlighted_text().contains(&"bravo".to_owned()),
        "{}",
        screen.text()
    );
    screen.release(to);
    screen.nothing_copied();
}

/// An emoji made of several characters is drawn as one: the text after it
/// copies as selected.
#[test]
fn text_after_a_joined_emoji_copies_as_selected() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "say");
    screen.say("t1", "m1", "👩\u{200d}💻 done");
    screen.draw();
    let from = screen.find("done");
    screen.select(from, screen.find_end("done"));
    assert_eq!(screen.clipboard(), "done", "{}", screen.text());
    screen.draw();
    assert_eq!(screen.highlighted_text(), ["done"]);
}

/// A letter written with a combining accent is one letter of the word a
/// double click takes.
#[test]
fn a_double_click_takes_a_word_with_a_combining_accent_whole() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "say");
    screen.say("t1", "m1", "e\u{301}clair au cafe\u{301}");
    screen.draw();
    let at = screen.find("e\u{301}");
    screen.click(at);
    screen.click(at);
    assert_eq!(screen.clipboard(), "e\u{301}clair");
    let at = screen.find("cafe");
    screen.click(at);
    screen.click(at);
    assert_eq!(screen.clipboard(), "cafe\u{301}");
}

/// A card title with emoji, close to the status on the right (the rest of
/// the command wraps below): dragging over the whole row copies and
/// highlights the title, not the status.
#[test]
fn a_card_title_with_emoji_copies_without_the_status() {
    let mut screen = Screen::sized(40, 20);
    screen.start_turn("t1", "run");
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "commandExecution", "id": "c1", "command": "echo 👩\u{200d}💻 👩\u{200d}💻 abcdefghijkl",
        "cwd": "/work", "status": "completed", "aggregatedOutput": "", "exitCode": 0,
        "durationMs": null,
    }}));
    screen.draw();
    let at = screen.find("● Run");
    let row = screen.lines()[usize::from(at.1)].clone();
    assert!(row.ends_with("  exit 0"), "{}", screen.text());
    assert_eq!(
        screen.lines()[usize::from(at.1) + 1],
        "        abcdefghijkl"
    );
    let title = row.trim_start().trim_start_matches("● ");
    let title = title.trim_end_matches("exit 0").trim_end().to_owned();
    screen.select(at, (39, at.1));
    assert_eq!(screen.clipboard(), title, "{}", screen.text());
    screen.draw();
    assert_eq!(screen.highlighted_text(), [title]);
}

/// A changed line drawn on a background, with emoji in it: the highlight
/// ends where its text does, not on the background after it.
#[test]
fn a_changed_line_with_emoji_highlights_only_its_text() {
    theme::set(Theme {
        depth: Depth::TrueColor,
        background: Background::Dark,
    });
    let mut screen = Screen::new();
    screen.start_turn("t1", "edit");
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "fileChange", "id": "e1", "status": "completed",
        "changes": [{
            "path": "/work/greet.sh",
            "kind": { "type": "update", "movePath": null },
            "diff": "--- a/greet.sh\n+++ b/greet.sh\n@@ -1 +1 @@\n-echo hi\n+echo 👩\u{200d}💻 👩\u{200d}💻 hi\n",
        }],
    }}));
    screen.draw();
    let line = "echo 👩\u{200d}💻 👩\u{200d}💻 hi";
    let from = screen.find(line);
    screen.select(from, (WIDTH - 1, from.1));
    assert_eq!(screen.clipboard(), line);
    screen.draw();
    assert_eq!(screen.highlighted_text(), [line], "{}", screen.text());
}

/// With code coloured and a gutter of its own, dragging over two hunks
/// still copies only the code: no numbers, signs, or the `⋯` row naming
/// the second hunk's function.
#[test]
fn dragging_over_coloured_hunks_copies_only_the_code() {
    theme::set(Theme {
        depth: Depth::TrueColor,
        background: Background::Dark,
    });
    let mut screen = Screen::new();
    screen.start_turn("t1", "edit");
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "fileChange", "id": "e1", "status": "completed",
        "changes": [{
            "path": "/work/src/lib.rs",
            "kind": { "type": "update", "movePath": null },
            "diff": "--- a/src/lib.rs\n+++ b/src/lib.rs\n\
                     @@ -1,2 +1,2 @@\n fn one() {\n-    let a = 1;\n+    let a = 2;\n\
                     @@ -20,2 +20,2 @@ fn two() {\n-    let b = \"x\";\n+    let b = \"y\";\n",
        }],
    }}));
    screen.draw();
    assert!(screen.text().contains("⋯ fn two() {"), "{}", screen.text());
    let from = screen.find("fn one");
    let to = screen.find_end("let b = \"y\";");
    screen.select(from, to);
    assert_eq!(
        screen.clipboard(),
        "fn one() {\n    let a = 1;\n    let a = 2;\n    let b = \"x\";\n    let b = \"y\";"
    );
}

/// A command that wraps copies as it was written: its rows join again where
/// wrapping broke them, and its own new lines stay.
#[test]
fn a_wrapped_command_copies_as_one_line() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "run");
    let words: Vec<String> = (1..=12).map(|n| format!("arg{n:02}")).collect();
    let line = format!("echo {}", words.join(" "));
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "commandExecution", "id": "c1", "command": format!("{line}\nls -la"),
        "cwd": "/work", "status": "completed", "aggregatedOutput": "", "exitCode": 0,
        "durationMs": 1200,
    }}));
    screen.draw();
    let title = screen.find("● Run echo arg01");
    assert_eq!(
        screen.lines()[usize::from(title.1) + 1],
        "        arg06 arg07 arg08 arg09 arg10 arg11 arg12"
    );
    let end = screen.find_end("ls -la");
    screen.select(title, end);
    assert_eq!(screen.clipboard(), format!("Run {line}\nls -la"));
}
