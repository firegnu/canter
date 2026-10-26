//! Links in the conversation: web addresses in answers, and the paths and
//! addresses in tool card titles, are underlined; a click that does not
//! move, with no second press within the double click time, opens one.
//! Over SSH a click copies it instead, and on Linux a path. The program that
//! opens links is a fake here, as is the clipboard: tests never start a
//! browser, an editor or a file manager, and never touch the real clipboard.
//! The screen opens links as on macOS unless a test says otherwise.

use std::path::Path;
use std::process::Command;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{Action, App, LinkSystem, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 1_000_000;
const WIDTH: u16 = 100;
const HEIGHT: u16 = 30;
/// Rows under the conversation: the status line, the input box (3) and the
/// info rows (2).
const BELOW: u16 = 6;
/// Presses this close together (ms) are a double click; a click opens a
/// link only after it.
const MULTI_CLICK_MS: i64 = 400;
/// How long the screen waits on the fake clipboard's thread.
const WAIT: Duration = Duration::from_secs(5);

/// What the fake opener was asked to start: the program and its arguments.
type Started = (String, Vec<String>);

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    /// When frames are drawn and the mouse is used.
    now: i64,
    /// The number of the frame on screen.
    frame: u64,
    started: mpsc::Receiver<Started>,
    copied: mpsc::Receiver<String>,
}

impl Screen {
    /// A screen whose session works in `cwd`, on macOS.
    fn new(cwd: &str) -> Self {
        Self::with(cwd, LinkSystem::MacOs, Ok(()))
    }

    /// Links open as on `system`; `result` is what starting a program gives.
    fn with(cwd: &str, system: LinkSystem, result: Result<(), String>) -> Self {
        let (start, started) = mpsc::channel();
        Self::launching(cwd, system, started, move |command: Command| {
            let _ = start.send(started_by(&command));
            result.clone()
        })
    }

    /// Links open as on `system`, with `launch`; `started` hears from it.
    fn launching(
        cwd: &str,
        system: LinkSystem,
        started: mpsc::Receiver<Started>,
        launch: impl FnMut(Command) -> Result<(), String> + Send + 'static,
    ) -> Self {
        let (copy, copied) = mpsc::channel();
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW)
            .with_clipboard_writer(move |text: &str| {
                let _ = copy.send(text.to_owned());
                Ok(())
            })
            .with_link_opener(system, launch);
        let ui = Ui::new(TestBackend::new(WIDTH, HEIGHT), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            now: NOW,
            frame: 0,
            started,
            copied,
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

    /// The model writes `text` as a finished message.
    fn say(&mut self, id: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": id, "text": text });
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": message }));
        self.completed(message);
    }

    fn read(&mut self, id: &str, path: &str) {
        self.completed(json!({
            "type": "fileRead", "id": id, "path": path, "startLine": 1,
            "content": "one\ntwo\n", "status": "completed",
        }));
    }

    fn change(&mut self, id: &str, path: &str, kind: Value) {
        self.completed(json!({
            "type": "fileChange", "id": id, "status": "completed",
            "changes": [{ "path": path, "kind": kind, "diff": "@@ -1 +1 @@\n-old\n+new\n" }],
        }));
    }

    fn fetch(&mut self, id: &str, url: &str) {
        self.completed(json!({
            "type": "webFetch", "id": id, "url": url, "result": "page", "status": "completed",
        }));
    }

    fn draw(&mut self) {
        self.ui.draw(self.now).unwrap();
        self.frame += 1;
    }

    /// Makes the terminal `width` columns wide, from the next frame on.
    fn resize(&mut self, width: u16) {
        self.ui.backend_mut().resize(width, HEIGHT);
    }

    /// Lets `ms` pass and draws.
    fn wait(&mut self, ms: i64) {
        self.now += ms;
        self.draw();
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

    /// The cell where `text` first starts.
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

    /// The mouse, come in while frame `frame` was on screen.
    fn mouse_on(&mut self, kind: MouseEventKind, (column, row): (u16, u16), frame: u64) {
        let event = MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        self.ui.app_mut().handle_mouse_on(event, frame, self.now);
    }

    fn press(&mut self, at: (u16, u16)) {
        self.mouse(MouseEventKind::Down(MouseButton::Left), at);
    }

    fn release(&mut self, at: (u16, u16)) {
        self.mouse(MouseEventKind::Up(MouseButton::Left), at);
    }

    fn click(&mut self, at: (u16, u16)) {
        self.press(at);
        self.release(at);
    }

    /// Clicks where `text` starts, then lets the double click time pass.
    fn click_and_wait(&mut self, text: &str) {
        let at = self.find(text);
        self.click(at);
        self.wait(MULTI_CLICK_MS + 1);
    }

    fn ctrl(&mut self, c: char) {
        self.ui.app_mut().handle_key(
            KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
            self.now,
        );
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), self.now)
    }

    /// What the fake opener was asked to start since the last look. It
    /// starts on a thread of its own.
    fn opened(&self) -> Started {
        self.started
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("nothing was opened:\n{}", self.text()))
    }

    fn nothing_opened(&self) {
        if let Ok(started) = self.started.recv_timeout(Duration::from_millis(200)) {
            panic!("opened {started:?} unexpectedly:\n{}", self.text());
        }
    }

    /// What the fake clipboard was given next.
    fn clipboard(&self) -> String {
        self.copied
            .recv_timeout(WAIT)
            .unwrap_or_else(|_| panic!("nothing was copied:\n{}", self.text()))
    }

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

/// The program `command` starts and its arguments.
fn started_by(command: &Command) -> Started {
    let program = command.get_program().to_string_lossy().into_owned();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    (program, args)
}

/// What opens a web address on macOS: the default browser.
fn browser(url: &str) -> Started {
    ("open".to_owned(), vec![url.to_owned()])
}

/// What opens a file on macOS: the default text editor.
fn editor(path: &Path) -> Started {
    let path = path.to_string_lossy().into_owned();
    ("open".to_owned(), vec!["-t".to_owned(), path])
}

/// What shows a directory on macOS: the Finder, with it selected.
fn finder(path: &Path) -> Started {
    let path = path.to_string_lossy().into_owned();
    ("open".to_owned(), vec!["-R".to_owned(), path])
}

/// A session directory of its own, with `src/main.rs`, `README.md` and `docs/`.
fn workdir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::create_dir_all(dir.path().join("docs")).unwrap();
    std::fs::write(dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(dir.path().join("README.md"), "# readme\n").unwrap();
    dir
}

fn cwd(dir: &tempfile::TempDir) -> String {
    dir.path().to_string_lossy().into_owned()
}

/// The sketch in the task: the addresses in an answer are underlined, not
/// the text or punctuation around them; a click opens one in the browser,
/// once the double click time has passed, and the status line says so.
#[test]
fn a_click_on_an_address_in_an_answer_opens_it() {
    let mut screen = Screen::new("/work");
    screen.say(
        "m1",
        "装好以后看官方文档 https://doc.rust-lang.org/book/ ，\n\
         或者 Rust 官网 (https://www.rust-lang.org)。",
    );
    screen.draw();
    assert_eq!(
        screen.underlined(),
        [
            "https://doc.rust-lang.org/book/",
            "https://www.rust-lang.org"
        ],
        "{}",
        screen.text()
    );

    let at = screen.find("rust-lang.org/book");
    screen.click(at);
    screen.wait(MULTI_CLICK_MS);
    screen.nothing_opened();
    assert!(!screen.status().contains("Opened"), "{}", screen.status());
    screen.wait(1);
    assert_eq!(screen.opened(), browser("https://doc.rust-lang.org/book/"));
    screen.wait_status("Opened");
    assert_eq!(screen.status(), "↗ Opened https://doc.rust-lang.org/book/");

    screen.now += 1000;
    screen.click_and_wait("www.rust-lang");
    assert_eq!(screen.opened(), browser("https://www.rust-lang.org"));
    screen.nothing_opened();
}

/// A markdown link shows as `text (address)`: both open the address.
#[test]
fn a_markdown_links_text_and_address_open_the_address() {
    let mut screen = Screen::new("/work");
    screen.say(
        "m1",
        "See [the book](https://doc.rust-lang.org/book/) or <https://crates.io/>.",
    );
    screen.draw();
    assert!(
        screen
            .text()
            .contains("See the book (https://doc.rust-lang.org/book/) or https://crates.io/."),
        "{}",
        screen.text()
    );
    assert_eq!(
        screen.underlined(),
        [
            "the book",
            "https://doc.rust-lang.org/book/",
            "https://crates.io/"
        ]
    );

    screen.click_and_wait("book (");
    assert_eq!(screen.opened(), browser("https://doc.rust-lang.org/book/"));
    screen.now += 1000;
    screen.click_and_wait("doc.rust-lang");
    assert_eq!(screen.opened(), browser("https://doc.rust-lang.org/book/"));
    screen.now += 1000;
    screen.click_and_wait("crates.io");
    assert_eq!(screen.opened(), browser("https://crates.io/"));
}

/// Paths in the titles of read, edit and new-file cards, and the new path of
/// a moved file, are underlined and open in the text editor, as absolute
/// paths from the session's directory. The mark, the verb and the status
/// are not links, nor is a deleted file or the old path of a moved one.
#[test]
fn paths_in_card_titles_open_as_absolute_paths() {
    let dir = workdir();
    let root = dir.path();
    std::fs::write(root.join("new.rs"), "").unwrap();
    std::fs::write(root.join("moved.rs"), "").unwrap();
    let mut screen = Screen::new(&cwd(&dir));
    screen.read("r1", "src/main.rs");
    let readme = root.join("README.md");
    screen.change(
        "e1",
        &readme.to_string_lossy(),
        json!({ "type": "update", "movePath": null }),
    );
    screen.change("e2", "new.rs", json!({ "type": "add" }));
    screen.change(
        "e3",
        "old.rs",
        json!({ "type": "update", "movePath": "moved.rs" }),
    );
    screen.change("e4", "gone.rs", json!({ "type": "delete" }));
    screen.draw();
    let text = screen.text();
    for title in [
        "◇ Read src/main.rs  lines 1–2",
        "● Edit README.md  +1 −1",
        "● Add new.rs  +1",
        "● Edit old.rs → moved.rs  +1 −1",
        "● Delete gone.rs  −1",
    ] {
        assert!(text.contains(title), "{title:?}:\n{text}");
    }
    assert_eq!(
        screen.underlined(),
        ["src/main.rs", "README.md", "new.rs", "moved.rs"]
    );

    screen.click_and_wait("src/main.rs");
    assert_eq!(screen.opened(), editor(&root.join("src/main.rs")));
    screen.wait_status("Opened");
    assert_eq!(
        screen.status(),
        format!("↗ Opened {}", root.join("src/main.rs").display())
    );
    for (text, path) in [
        ("README.md", readme),
        ("new.rs", root.join("new.rs")),
        ("moved.rs", root.join("moved.rs")),
    ] {
        screen.now += 1000;
        screen.click_and_wait(text);
        assert_eq!(screen.opened(), editor(&path), "{text}");
    }
    for text in ["Read", "◇", "lines 1–2", "+1 −1", "gone.rs", "old.rs"] {
        screen.now += 1000;
        screen.click_and_wait(text);
        screen.nothing_opened();
    }
}

/// A web fetch card's address opens in the browser.
#[test]
fn a_fetch_cards_address_opens_in_the_browser() {
    let mut screen = Screen::new("/work");
    screen.fetch("w1", "https://docs.example/page?q=1");
    screen.draw();
    assert_eq!(screen.underlined(), ["https://docs.example/page?q=1"]);
    screen.click_and_wait("docs.example");
    assert_eq!(screen.opened(), browser("https://docs.example/page?q=1"));
}

/// A directory is shown in the Finder; a path that is not there is not
/// opened, and the status line says so in red for about five seconds.
#[test]
fn a_directory_is_shown_in_the_finder_and_a_missing_path_is_not_opened() {
    let dir = workdir();
    let mut screen = Screen::new(&cwd(&dir));
    screen.read("r1", "docs");
    screen.read("r2", "missing.rs");
    screen.draw();

    screen.click_and_wait("docs");
    assert_eq!(screen.opened(), finder(&dir.path().join("docs")));

    screen.now += 1000;
    screen.click_and_wait("missing.rs");
    screen.wait_status("No such file");
    screen.nothing_opened();
    let missing = dir.path().join("missing.rs");
    assert_eq!(
        screen.status(),
        format!("✗ No such file: {}", missing.display())
    );
    let (x, y) = screen.find("No such file");
    assert_eq!(
        screen.buffer()[(x, y)].fg,
        ratatui::style::Color::Red,
        "in red"
    );
    screen.wait(4999);
    assert!(
        screen.status().contains("No such file"),
        "{}",
        screen.status()
    );
    screen.wait(1);
    assert!(
        !screen.status().contains("No such file"),
        "{}",
        screen.status()
    );
}

/// Only http and https: other schemes are neither underlined nor opened,
/// whether written out or as a markdown link's destination; nor are paths
/// in the answer's text, or addresses in a code block or in shell output.
#[test]
fn other_schemes_paths_in_text_and_shell_output_are_not_links() {
    let mut screen = Screen::new("/work");
    screen.say(
        "m1",
        "file:///etc/passwd javascript:alert(1) vscode://file/x ftp://a.example/f\n\
         [run](javascript:alert(1)) [hosts](file:///etc/hosts) [app](zed://open) \
         [doc](docs/x.md)\n\
         See src/main.rs and /etc/hosts.\n\
         xhttps://joined.example\n\n\
         ```\ncurl https://in-code.example/install.sh\n```",
    );
    screen.completed(json!({
        "type": "commandExecution", "id": "c1", "command": "echo https://shell.example",
        "cwd": "/work", "status": "completed", "aggregatedOutput": "https://shell.example\n",
        "exitCode": 0, "durationMs": 10,
    }));
    screen.draw();
    assert!(
        screen.text().contains("run (javascript:alert(1))"),
        "{}",
        screen.text()
    );
    assert_eq!(
        screen.underlined(),
        Vec::<String>::new(),
        "{}",
        screen.text()
    );
    for text in [
        "file:///etc",
        "javascript:alert",
        "vscode://",
        "ftp://",
        "run (",
        "hosts (",
        "app (",
        "doc (",
        "src/main.rs",
        "joined.example",
        "in-code.example",
        "shell.example",
    ] {
        screen.now += 1000;
        screen.click_and_wait(text);
        screen.nothing_opened();
    }
}

/// Punctuation after an address is not part of it, and neither is a
/// closing bracket without its opening one; brackets in pairs are.
#[test]
fn punctuation_after_an_address_is_left_out_and_brackets_in_pairs_kept() {
    let mut screen = Screen::new("/work");
    screen.say(
        "m1",
        "a https://a.example/x. b https://b.example/y, c (https://c.example/z)\n\
         d https://en.wikipedia.org/wiki/Rust_(language)。e https://e.example/q?x=1;\n\
         f \"https://f.example/\"! g https://g.example/中文，h [https://h.example/](i)\n\
         j https://j.example/a_b_(c)_d) k https://k.example:8080/p?q=[1]&r=2!",
    );
    screen.draw();
    assert_eq!(
        screen.underlined(),
        [
            "https://a.example/x",
            "https://b.example/y",
            "https://c.example/z",
            "https://en.wikipedia.org/wiki/Rust_(language)",
            "https://e.example/q?x=1",
            "https://f.example/",
            "https://g.example/中文",
            "https://j.example/a_b_(c)_d",
            "https://k.example:8080/p?q=[1]&r=2",
        ],
        "{}",
        screen.text()
    );
    screen.click_and_wait("wikipedia");
    assert_eq!(
        screen.opened(),
        browser("https://en.wikipedia.org/wiki/Rust_(language)")
    );
}

/// A double click on an address takes the word and a triple click the row,
/// as anywhere else; neither opens it.
#[test]
fn a_double_or_triple_click_on_an_address_selects_and_does_not_open() {
    let mut screen = Screen::new("/work");
    screen.say("m1", "Read https://docs.example/guide first.");
    screen.draw();
    let at = screen.find("docs.example");
    screen.click(at);
    screen.now += 100;
    screen.click(at);
    assert_eq!(screen.clipboard(), "//docs.example/guide");
    screen.wait(MULTI_CLICK_MS + 500);
    screen.nothing_opened();

    // Each copy is waited for before the next press: the clipboard thread
    // writes only the latest of the copies piled up.
    screen.now += 1000;
    screen.click(at);
    screen.now += 100;
    screen.click(at);
    assert_eq!(screen.clipboard(), "//docs.example/guide");
    screen.now += 100;
    screen.click(at);
    assert_eq!(screen.clipboard(), "Read https://docs.example/guide first.");
    screen.wait(MULTI_CLICK_MS + 500);
    screen.nothing_opened();
}

/// A drag that starts on an address selects text, as anywhere else.
#[test]
fn a_drag_from_an_address_selects_text() {
    let mut screen = Screen::new("/work");
    screen.say("m1", "Read https://docs.example/guide first.");
    screen.draw();
    let from = screen.find("docs.example");
    let to = screen.find("first");
    screen.press(from);
    screen.mouse(MouseEventKind::Drag(MouseButton::Left), to);
    screen.release(to);
    assert_eq!(screen.clipboard(), "docs.example/guide f");
    screen.wait(MULTI_CLICK_MS + 500);
    screen.nothing_opened();

    // Dragging away and back to the cell pressed is still a drag.
    screen.now += 1000;
    screen.press(from);
    screen.mouse(MouseEventKind::Drag(MouseButton::Left), to);
    screen.mouse(MouseEventKind::Drag(MouseButton::Left), from);
    screen.release(from);
    assert_eq!(screen.clipboard(), "d");
    screen.wait(MULTI_CLICK_MS + 500);
    screen.nothing_opened();
}

/// Over SSH a browser or editor would open on the wrong machine: a click
/// copies the address or the absolute path instead.
#[test]
fn over_ssh_a_click_copies_the_link() {
    let dir = workdir();
    let mut screen = Screen::with(&cwd(&dir), LinkSystem::Remote, Ok(()));
    screen.say("m1", "See https://docs.example/guide now.");
    screen.read("r1", "src/main.rs");
    screen.draw();

    screen.click_and_wait("docs.example");
    assert_eq!(screen.clipboard(), "https://docs.example/guide");
    screen.wait_status("✓ Link copied");
    screen.nothing_opened();

    screen.now += 3000;
    screen.click_and_wait("src/main.rs");
    assert_eq!(
        screen.clipboard(),
        dir.path().join("src/main.rs").to_string_lossy()
    );
    screen.nothing_opened();
}

/// The note that a link opened goes after about two seconds; a long one is
/// cut in the middle. When the program cannot start, the status line says
/// why in red.
#[test]
fn the_note_goes_after_two_seconds_and_a_failure_shows_in_red() {
    let long = format!("https://docs.example/{}/end", "a".repeat(100));
    let mut screen = Screen::new("/work");
    screen.say(
        "m1",
        &format!("Short https://docs.example/guide and {long}"),
    );
    screen.draw();
    screen.click_and_wait("docs.example/guide");
    screen.opened();
    screen.wait_status("Opened");
    assert_eq!(screen.status(), "↗ Opened https://docs.example/guide");
    screen.wait(1999);
    assert_eq!(screen.status(), "↗ Opened https://docs.example/guide");
    screen.wait(1);
    assert!(!screen.status().contains("Opened"), "{}", screen.status());

    screen.click_and_wait("docs.example/aaa");
    assert_eq!(screen.opened(), browser(&long));
    screen.wait_status("Opened");
    let status = screen.status();
    assert!(
        status.starts_with("↗ Opened https://docs.example/aaa") && status.ends_with("aaa/end"),
        "{status}"
    );
    assert!(status.contains('…'), "{status}");
    assert!(status.width() <= usize::from(WIDTH), "{status}");

    // The rest of the address, on the row it wrapped onto, is the same link.
    let first = screen.find("docs.example/aaa").1;
    let rest = screen.find("aaa/end");
    assert!(rest.1 > first, "the address wraps:\n{}", screen.text());
    assert_eq!(screen.underlined()[1..].concat(), long);
    screen.now += 1000;
    screen.click(rest);
    screen.wait(MULTI_CLICK_MS + 1);
    assert_eq!(screen.opened(), browser(&long));

    let mut screen = Screen::with("/work", LinkSystem::MacOs, Err("no such program".into()));
    screen.say("m1", "Short https://docs.example/guide");
    screen.draw();
    screen.click_and_wait("docs.example");
    screen.opened();
    screen.wait_status("Could not open");
    assert_eq!(
        screen.status(),
        "✗ Could not open https://docs.example/guide: no such program"
    );
    let (x, y) = screen.find("Could not open");
    assert_eq!(screen.buffer()[(x, y)].fg, ratatui::style::Color::Red);
    screen.wait(4999);
    assert!(screen.status().contains("Could not open"));
    screen.wait(1);
    assert!(!screen.status().contains("Could not open"));
}

/// Characters a shell would act on go to the opener as they are, in one
/// argument: nothing goes through a shell.
#[test]
fn an_address_goes_to_the_opener_as_one_argument_as_it_is() {
    let odd = "https://example.com/a b\"c'd;e$(touch x)|f&g";
    let mut screen = Screen::new("/work");
    screen.say("m1", &format!("[odd](<{odd}>)"));
    screen.fetch("w1", "https://example.com/one\nrm -rf ~");
    screen.draw();
    screen.click_and_wait("odd");
    assert_eq!(screen.opened(), browser(odd));

    screen.now += 1000;
    screen.click_and_wait("example.com/one");
    assert_eq!(
        screen.opened(),
        browser("https://example.com/one\nrm -rf ~")
    );
}

/// A path that starts with `-` goes as an absolute path, never as an option.
#[test]
fn a_path_starting_with_a_dash_is_not_taken_as_an_option() {
    let dir = workdir();
    std::fs::write(dir.path().join("-rf"), "").unwrap();
    let mut screen = Screen::new(&cwd(&dir));
    screen.read("r1", "-rf");
    screen.draw();
    screen.click_and_wait("-rf");
    let (_, args) = screen.opened();
    let path = dir.path().join("-rf").to_string_lossy().into_owned();
    assert_eq!(args.last(), Some(&path));
    assert!(
        args.iter().all(|arg| arg == "-t" || !arg.starts_with('-')),
        "{args:?}"
    );
}

/// A symbolic link to a file elsewhere opens as a text file, by its own path.
#[test]
fn a_symlink_to_a_file_elsewhere_opens_as_text() {
    let dir = workdir();
    let elsewhere = tempfile::tempdir().unwrap();
    let target = elsewhere.path().join("script.sh");
    std::fs::write(&target, "#!/bin/sh\necho hi\n").unwrap();
    std::os::unix::fs::symlink(&target, dir.path().join("link.sh")).unwrap();
    let mut screen = Screen::new(&cwd(&dir));
    screen.read("r1", "link.sh");
    screen.draw();
    screen.click_and_wait("link.sh");
    assert_eq!(screen.opened(), editor(&dir.path().join("link.sh")));
}

/// While the answer still grows, the address clicked is the one opened,
/// though new rows come in between the press, the release and the open.
#[test]
fn while_the_answer_grows_the_address_clicked_opens() {
    let mut screen = Screen::new("/work");
    let message = |text: &str| json!({ "type": "agentMessage", "id": "m1", "text": text });
    screen.event(json!({ "type": "item.started", "turnId": "t1",
        "item": message("one https://one.example/\ntwo https://two.example/\n") }));
    screen.draw();
    let at = screen.find("two.example");
    screen.press(at);
    for n in 0..30 {
        screen.event(
            json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": { "type": "agentMessage",
                       "delta": format!("more https://more{n}.example/\n") } }),
        );
    }
    screen.draw();
    screen.release(at);
    for n in 30..60 {
        screen.event(
            json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": { "type": "agentMessage",
                       "delta": format!("more https://more{n}.example/\n") } }),
        );
    }
    screen.wait(MULTI_CLICK_MS + 1);
    assert_eq!(screen.opened(), browser("https://two.example/"));
    screen.nothing_opened();
}

/// While searching, an underline marks a match: links are not underlined
/// and do not open then. Once the search closes they are again.
#[test]
fn while_searching_links_are_not_underlined_or_opened() {
    let mut screen = Screen::new("/work");
    screen.say("m1", "Read https://docs.example/guide and the guide.");
    screen.draw();
    assert_eq!(screen.underlined(), ["https://docs.example/guide"]);

    screen.ctrl('f');
    for c in "guide".chars() {
        screen.key(KeyCode::Char(c));
    }
    screen.draw();
    assert_eq!(screen.underlined(), ["guide"], "the older match only");
    screen.click_and_wait("docs.example");
    screen.nothing_opened();

    screen.key(KeyCode::Esc);
    screen.draw();
    assert_eq!(screen.underlined(), ["https://docs.example/guide"]);
    screen.now += 1000;
    screen.click_and_wait("docs.example");
    assert_eq!(screen.opened(), browser("https://docs.example/guide"));
}

/// A press anywhere before the double click time is over is a second press:
/// the link clicked first does not open.
#[test]
fn a_second_press_before_the_open_cancels_it() {
    let mut screen = Screen::new("/work");
    screen.say("m1", "See https://docs.example/guide now.");
    screen.draw();
    let link = screen.find("docs.example");
    screen.click(link);
    screen.now += 200;
    screen.click(screen.find("See"));
    screen.wait(MULTI_CLICK_MS + 500);
    screen.nothing_opened();
    screen.nothing_copied();
}

// Rework 1 (cross review, round 1).

/// On macOS a directory is shown in the Finder (`open -R`), never opened:
/// an application bundle is a directory too, and opening it would start it.
/// So is one a symbolic link or a moved file's new path points to.
#[test]
fn an_app_bundle_is_shown_in_the_finder_not_started() {
    let dir = workdir();
    let app = dir.path().join("Tool.app");
    std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
    std::os::unix::fs::symlink(&app, dir.path().join("tool-link")).unwrap();
    let mut screen = Screen::new(&cwd(&dir));
    screen.read("r1", "Tool.app");
    screen.change(
        "e1",
        "old.txt",
        json!({ "type": "update", "movePath": "tool-link" }),
    );
    screen.draw();

    screen.click_and_wait("Tool.app");
    assert_eq!(screen.opened(), finder(&app));
    screen.now += 1000;
    screen.click_and_wait("tool-link");
    assert_eq!(screen.opened(), finder(&dir.path().join("tool-link")));
}

/// On Linux an address opens with `xdg-open`, but a path is copied: there
/// is no program there sure to show a file as text, and `xdg-open` picks
/// one by the file's type (a `.desktop` file can start a program).
#[test]
fn on_linux_an_address_opens_and_a_path_is_copied() {
    let dir = workdir();
    std::fs::write(dir.path().join("tool.desktop"), "[Desktop Entry]\n").unwrap();
    std::fs::create_dir(dir.path().join("folder")).unwrap();
    let mut screen = Screen::with(&cwd(&dir), LinkSystem::Linux, Ok(()));
    screen.say("m1", "See https://docs.example/guide now.");
    screen.read("r1", "tool.desktop");
    screen.read("r2", "folder");
    screen.read("r3", "missing.rs");
    screen.draw();

    screen.click_and_wait("docs.example");
    assert_eq!(
        screen.opened(),
        (
            "xdg-open".to_owned(),
            vec!["https://docs.example/guide".to_owned()]
        )
    );
    screen.wait_status("↗ Opened https://docs.example/guide");

    for path in ["tool.desktop", "folder", "missing.rs"] {
        screen.now += 3000;
        screen.click_and_wait(path);
        assert_eq!(
            screen.clipboard(),
            dir.path().join(path).to_string_lossy(),
            "{path}"
        );
        screen.wait_status("✓ Path copied");
        screen.nothing_opened();
    }
}

/// A click is on what was drawn: an address that grew since the frame on
/// screen opens as it was drawn, not as it is now.
#[test]
fn an_address_opens_as_drawn_before_the_answer_grew() {
    let mut screen = Screen::new("/work");
    let message =
        json!({ "type": "agentMessage", "id": "m1", "text": "See https://docs.example/gu" });
    screen.event(json!({ "type": "item.started", "turnId": "t1", "item": message }));
    screen.draw();
    assert_eq!(screen.underlined(), ["https://docs.example/gu"]);
    screen.event(
        json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
        "delta": { "type": "agentMessage", "delta": "ide/more and more" } }),
    );

    let at = screen.find("docs.example");
    screen.click(at);
    screen.wait(MULTI_CLICK_MS + 1);
    assert_eq!(screen.opened(), browser("https://docs.example/gu"));
    screen.nothing_opened();
}

/// The wheel moves the conversation under the pointer before the next
/// frame shows it: a press or a release after it opens nothing. Once the
/// frame is drawn, a click opens again.
#[test]
fn after_the_wheel_turns_a_click_opens_nothing_until_drawn() {
    let mut screen = Screen::new("/work");
    let items: String = (0..40)
        .map(|n| format!("- item {n} https://l{n}.example/\n"))
        .collect();
    screen.say("m1", &items);
    screen.draw();
    let at = screen.find("l35.example");

    screen.mouse(MouseEventKind::ScrollUp, at);
    screen.click(at);
    screen.wait(MULTI_CLICK_MS + 1);
    screen.nothing_opened();

    screen.now += 1000;
    let at = screen.find("l30.example");
    screen.press(at);
    screen.mouse(MouseEventKind::ScrollDown, at);
    screen.release(at);
    screen.wait(MULTI_CLICK_MS + 1);
    screen.nothing_opened();

    screen.now += 1000;
    screen.draw();
    screen.click_and_wait("l35.example");
    assert_eq!(screen.opened(), browser("https://l35.example/"));
}

/// A new width wraps the text again: a click that came in on the frame
/// before is not taken as one on the new frame, nor does a click whose
/// press and release are on frames of two widths open.
#[test]
fn across_a_change_of_width_a_click_opens_nothing() {
    let long = format!("https://one.example/{}", "a".repeat(70));
    let mut screen = Screen::new("/work");
    screen.say("m1", &format!("- {long}\n- https://two.example/"));
    screen.draw();
    let frame = screen.frame;
    let at = screen.find("two.example");

    screen.resize(80);
    screen.draw();
    assert_ne!(screen.find("two.example"), at, "{}", screen.text());
    screen.mouse_on(MouseEventKind::Down(MouseButton::Left), at, frame);
    screen.mouse_on(MouseEventKind::Up(MouseButton::Left), at, frame);
    screen.wait(MULTI_CLICK_MS + 1);
    screen.nothing_opened();

    screen.now += 1000;
    screen.draw();
    let at = screen.find("two.example");
    screen.press(at);
    screen.resize(WIDTH);
    screen.draw();
    screen.release(at);
    screen.wait(MULTI_CLICK_MS + 1);
    screen.nothing_opened();

    screen.now += 1000;
    screen.click_and_wait("two.example");
    assert_eq!(screen.opened(), browser("https://two.example/"));
}

/// A click is on the frame that was on screen when it came in, though a
/// later one is drawn by the time it is handled.
#[test]
fn a_click_is_on_the_frame_on_screen_when_it_came_in() {
    let mut screen = Screen::new("/work");
    screen.completed(json!({
        "type": "reasoning", "id": "r0", "summary": [],
        "content": ["one\n\ntwo\n\nthree"],
    }));
    screen.say("m1", "See https://docs.example/guide now.");
    screen.draw();
    let frame = screen.frame;
    let at = screen.find("docs.example");
    screen.ctrl('o');
    screen.draw();
    assert_ne!(screen.find("docs.example"), at, "{}", screen.text());

    screen.mouse_on(MouseEventKind::Down(MouseButton::Left), at, frame);
    screen.mouse_on(MouseEventKind::Up(MouseButton::Left), at, frame);
    screen.wait(MULTI_CLICK_MS + 1);
    assert_eq!(screen.opened(), browser("https://docs.example/guide"));

    // The same cell on the frame on screen now is not on the link.
    screen.now += 1000;
    screen.click(at);
    screen.wait(MULTI_CLICK_MS + 1);
    screen.nothing_opened();
}

/// Esc, opening the search or switching sessions while a click waits for
/// the double click time drops it: nothing opens.
#[test]
fn esc_the_search_or_a_session_switch_drops_a_click_waiting_to_open() {
    let mut screen = Screen::new("/work");
    screen.say("m1", "See https://docs.example/guide now.");
    screen.draw();
    let at = screen.find("docs.example");

    screen.click(at);
    screen.key(KeyCode::Esc);
    screen.wait(MULTI_CLICK_MS + 1);
    screen.nothing_opened();

    screen.now += 1000;
    screen.draw();
    screen.click(at);
    screen.ctrl('f');
    screen.wait(MULTI_CLICK_MS + 1);
    screen.nothing_opened();
    screen.key(KeyCode::Esc);

    for command in ["/new", "/resume"] {
        screen.now += 1000;
        screen.draw();
        screen.click(at);
        screen.ui.app_mut().handle_paste(command);
        let action = screen.key(KeyCode::Enter);
        assert!(
            matches!(action, Some(Action::NewSession | Action::PickSession)),
            "{command}"
        );
        screen.wait(MULTI_CLICK_MS + 1);
        screen.nothing_opened();
    }

    screen.now += 1000;
    screen.draw();
    screen.click_and_wait("docs.example");
    assert_eq!(screen.opened(), browser("https://docs.example/guide"));
}

/// The program starts on a thread of its own: the screen goes on drawing
/// while it starts, and says it opened once it has.
#[test]
fn the_screen_does_not_wait_for_the_program_to_start() {
    let (start, started) = mpsc::channel();
    let (go, wait) = mpsc::channel::<()>();
    let mut screen = Screen::launching("/work", LinkSystem::MacOs, started, move |command| {
        let _ = start.send(started_by(&command));
        let _ = wait.recv_timeout(WAIT);
        Ok(())
    });
    screen.say("m1", "See https://docs.example/guide now.");
    screen.draw();
    screen.click_and_wait("docs.example");
    assert_eq!(screen.opened(), browser("https://docs.example/guide"));
    screen.wait(10);
    assert!(!screen.status().contains("Opened"), "{}", screen.status());
    go.send(()).unwrap();
    screen.wait_status("↗ Opened https://docs.example/guide");
}

/// Only a file or a directory opens, symbolic links followed: a device, a
/// socket and the like are not given to the editor.
#[test]
fn only_a_file_or_a_directory_opens() {
    let dir = workdir();
    std::os::unix::fs::symlink("/dev/null", dir.path().join("null-link")).unwrap();
    let socket = dir.path().join("s.sock");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let mut screen = Screen::new(&cwd(&dir));
    screen.read("r1", "null-link");
    screen.read("r2", "s.sock");
    screen.draw();
    for path in ["null-link", "s.sock"] {
        screen.now += 6000;
        screen.click_and_wait(path);
        let shown = format!("✗ Not a file: {}", dir.path().join(path).display());
        screen.wait_status(&shown);
        assert_eq!(screen.status(), shown);
        let (x, y) = screen.find("Not a file");
        assert_eq!(screen.buffer()[(x, y)].fg, ratatui::style::Color::Red);
        screen.nothing_opened();
    }
}
