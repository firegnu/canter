//! Long paths on one row lose whole directories from the middle: the first
//! part and the file name stay, and what follows the path on its row
//! (`lines 1–2`, the line counts, `failed`) stays too. A click on a path
//! drawn shorter still opens the whole path. The program that opens links
//! is a fake here: tests never start an editor.

use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{App, LinkSystem, Ui};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 1_000_000;
const HEIGHT: u16 = 30;
/// Rows under the conversation: the status line, the input box (3) and the
/// info rows (2).
const BELOW: u16 = 6;
/// A click opens a link only once this long (ms) has passed without a second.
const MULTI_CLICK_MS: i64 = 400;
const WAIT: Duration = Duration::from_secs(5);
const APP: &str = "crates/canter-tui/src/app.rs";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    now: i64,
    started: mpsc::Receiver<(String, Vec<String>)>,
}

impl Screen {
    /// A screen `width` columns wide whose session works in `cwd`; links
    /// open as on macOS.
    fn new(cwd: &str, width: u16) -> Self {
        let (start, started) = mpsc::channel();
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW).with_link_opener(
            LinkSystem::MacOs,
            move |command: Command| {
                let args = command
                    .get_args()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                let program = command.get_program().to_string_lossy().into_owned();
                let _ = start.send((program, args));
                Ok(())
            },
        );
        let ui = Ui::new(TestBackend::new(width, HEIGHT), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            now: NOW,
            started,
        };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": cwd, "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen.event(json!({ "type": "turn.started", "turn": {
            "id": "t1", "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": "look it up" }],
            "clientName": "canter-tui", "usage": null,
        }}));
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

    fn completed(&mut self, item: Value) {
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": item }));
    }

    fn read(&mut self, id: &str, path: &str, status: &str) {
        self.completed(json!({
            "type": "fileRead", "id": id, "path": path, "startLine": 1,
            "content": "one\ntwo\n", "status": status,
        }));
    }

    fn change(&mut self, id: &str, path: &str, kind: Value) {
        self.completed(json!({
            "type": "fileChange", "id": id, "status": "completed",
            "changes": [{ "path": path, "kind": kind, "diff": "@@ -1 +1 @@\n-old\n+new\n" }],
        }));
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

    fn status(&self) -> String {
        row(self.buffer(), HEIGHT - BELOW).trim().to_owned()
    }

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

    /// Each run of underlined cells in the conversation, in order.
    fn underlined(&self) -> Vec<String> {
        let buffer = self.buffer();
        let mut runs = Vec::new();
        for y in 0..HEIGHT - BELOW {
            let mut run = String::new();
            let mut x = 0;
            while x < buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.modifier.contains(Modifier::UNDERLINED) {
                    run.push_str(cell.symbol());
                } else if !run.is_empty() {
                    runs.push(std::mem::take(&mut run));
                }
                x += cell.symbol().width().max(1) as u16;
            }
            if !run.is_empty() {
                runs.push(run);
            }
        }
        runs
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

    /// Clicks at `at`, then lets the double click time pass.
    fn click_and_wait(&mut self, at: (u16, u16)) {
        self.mouse(MouseEventKind::Down(MouseButton::Left), at);
        self.mouse(MouseEventKind::Up(MouseButton::Left), at);
        self.now += MULTI_CLICK_MS + 1;
        self.draw();
    }

    fn opened(&self) -> (String, Vec<String>) {
        self.started
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("nothing was opened:\n{}", self.text()))
    }

    /// Draws until the status line shows `text`.
    fn wait_status(&mut self, text: &str) {
        let start = Instant::now();
        loop {
            self.draw();
            if self.status().contains(text) {
                return;
            }
            assert!(start.elapsed() < WAIT, "no {text:?}:\n{}", self.text());
            std::thread::sleep(Duration::from_millis(5));
        }
    }
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

/// In a narrow window a read card keeps the file name and `lines 1–2`: the
/// directories in the middle go, more of them the narrower it is.
#[test]
fn a_read_card_keeps_the_file_name_and_its_lines() {
    let mut screen = Screen::new("/work", 40);
    screen.read("r1", APP, "completed");
    screen.read("r2", "crates/canter-tui/src/cells.rs", "failed");
    screen.draw();
    let text = screen.text();
    assert!(
        text.contains("  ◇ Read crates/…/src/app.rs  lines 1–2\n"),
        "{text}"
    );
    assert!(
        text.contains("  ✗ Read crates/…/src/cells.rs  failed\n"),
        "{text}"
    );

    let mut screen = Screen::new("/work", 30);
    screen.read("r1", APP, "completed");
    screen.draw();
    let text = screen.text();
    assert!(text.contains("  ◇ Read …/app.rs  lines 1–2\n"), "{text}");
}

/// A moved file's card shares the room between its two paths; the line
/// counts stay.
#[test]
fn a_moved_files_card_shortens_both_paths() {
    let mut screen = Screen::new("/work", 60);
    screen.change(
        "e1",
        "crates/canter-tui/src/old_name.rs",
        json!({ "type": "update", "movePath": "crates/canter-tui/src/new_name.rs" }),
    );
    screen.draw();
    let text = screen.text();
    assert!(
        text.contains("  ● Edit crates/…/old_name.rs → crates/…/new_name.rs  +1 −1\n"),
        "{text}"
    );
}

/// The link is on the path as drawn, shorter, and a click on it opens the
/// whole path; the status line says which, shortened the same way.
#[test]
fn a_shortened_path_is_a_link_to_the_whole_path() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join(APP);
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "fn main() {}\n").unwrap();
    let mut screen = Screen::new(&dir.path().to_string_lossy(), 40);
    screen.read("r1", APP, "completed");
    screen.draw();
    assert_eq!(
        screen.underlined(),
        ["crates/…/src/app.rs"],
        "{}",
        screen.text()
    );

    let at = screen.find("…/src");
    screen.click_and_wait(at);
    let whole = file.to_string_lossy().into_owned();
    assert_eq!(
        screen.opened(),
        ("open".to_owned(), vec!["-t".to_owned(), whole.clone()])
    );
    screen.wait_status("Opened");
    let status = screen.status();
    assert!(status.starts_with("↗ Opened /"), "{status}");
    assert!(
        status.ends_with("/app.rs") && status.contains("/…/"),
        "{status}"
    );
    assert!(status.width() <= 40, "{status}");
    assert!(!whole.contains('…'));
}
