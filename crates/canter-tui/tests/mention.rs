//! `@` file mentions in the input box on ratatui's TestBackend: the list of
//! files and directories under the session's directory, filtered and ranked
//! as you type, and completing the selected path into the input box.

use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
/// The most files and directories the list takes from one directory.
const CAP: usize = 10_000;
const TRUNCATED: &str = "Only the first 10000 files and directories are listed";

/// The walk finds git's global config and ignore file through these
/// variables, in this process. Every test points them at one temporary home
/// before any walk starts, so the user's own git config is never read; its
/// global ignore file ignores `global.txt`.
fn isolate_home() {
    static HOME: OnceLock<tempfile::TempDir> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().unwrap();
        write(home.path(), ".gitconfig", "");
        write(home.path(), ".config/git/ignore", "global.txt\n");
        // SAFETY: the first test runs this while the others wait for it,
        // before any of them starts a walk; nothing else reads the
        // environment meanwhile.
        unsafe {
            std::env::set_var("HOME", home.path());
            std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
            std::env::set_var("GIT_CONFIG_GLOBAL", home.path().join(".gitconfig"));
            std::env::set_var("GIT_CONFIG_SYSTEM", "/dev/null");
        }
        home
    });
}

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new(cwd: &Path) -> Self {
        isolate_home();
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let ui = Ui::new(TestBackend::new(80, 30), app).unwrap();
        let mut screen = Self { ui, seq: 0 };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": cwd.to_str().unwrap(), "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen.event(json!({ "type": "session.status", "status": "idle" }));
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
        self.ui.draw(NOW).unwrap();
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), NOW)
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            assert_eq!(self.key(KeyCode::Char(c)), None, "typing {c:?}");
        }
    }

    /// Draws until `done` holds for the screen; the files are found in the
    /// background, a few frames after the list opens.
    fn draw_until(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            self.draw();
            if done(self) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "never showed {what}:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn lines(&self) -> Vec<String> {
        rows(self.ui.backend().buffer())
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// The rows between the status line and the input box: where the list
    /// shows. The status line is the first row after the conversation; with
    /// nothing in the conversation, the list rows are the non-empty rows
    /// right above the box.
    fn list(&self) -> Vec<String> {
        let lines = self.lines();
        let top = lines
            .iter()
            .rposition(|row| row.starts_with('╭'))
            .unwrap_or_else(|| panic!("no input box:\n{}", self.text()));
        let mut list: Vec<String> = lines[..top]
            .iter()
            .rev()
            .take_while(|row| !row.is_empty())
            .cloned()
            .collect();
        list.reverse();
        list
    }

    /// The listed paths, in order, the selected one first in its row.
    fn paths(&self) -> Vec<String> {
        self.list()
            .iter()
            .filter(|row| row.starts_with(" › ") || row.starts_with("   "))
            .map(|row| {
                row[row.char_indices().nth(3).unwrap().0..]
                    .trim()
                    .to_owned()
            })
            .collect()
    }

    /// The input box's first row, without the border.
    fn input(&self) -> String {
        let lines = self.lines();
        let top = lines.iter().rposition(|row| row.starts_with('╭')).unwrap();
        lines[top + 1].trim_matches('│').trim().to_owned()
    }
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
            row.trim_end().to_owned()
        })
        .collect()
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A few levels of files in a git repository (a `.git` directory is enough
/// for the ignore rules), with a `.gitignore`, `.git/info/exclude` and a
/// file the global ignore file ignores.
fn repository() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(root, ".git/HEAD", "ref: refs/heads/main\n");
    write(root, ".git/info/exclude", "secret.txt\n");
    write(root, ".gitignore", "target/\n*.log\n");
    write(root, "src/main.rs", "");
    write(root, "src/lib/deep.rs", "");
    write(root, "target/out.bin", "");
    write(root, "build.log", "");
    write(root, "secret.txt", "");
    write(root, "global.txt", "");
    write(root, "zz.txt", "");
    dir
}

/// Files for filtering and ranking `app`.
fn ranked() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for path in [
        "src/app.rs",
        "apps/web/index.html",
        "docs/a_p_p.md",
        "happy.txt",
        "lib/mapper.rs",
        "xa/yp/zp.txt",
        "unrelated.txt",
        "my notes.txt",
    ] {
        write(root, path, "");
    }
    dir
}

/// `@` alone lists the files and directories under the session's directory,
/// without what `.gitignore`, `.git/info/exclude` or the global ignore
/// file ignores and without `.git`.
#[test]
fn an_at_lists_the_files_git_does_not_ignore() {
    let dir = repository();
    let mut screen = Screen::new(dir.path());
    screen.type_text("@");
    // The directory is walked in name order, so once `zz.txt`, the last
    // name, shows, everything that would be listed is.
    screen.draw_until("zz.txt", |screen| {
        screen.paths().iter().any(|path| path == "zz.txt")
    });
    let mut paths = screen.paths();
    let shown = screen.text();
    assert!(
        screen.list()[0].starts_with(" › "),
        "the first is selected:\n{shown}"
    );
    paths.sort();
    assert_eq!(
        paths,
        [
            ".gitignore",
            "src/",
            "src/lib/",
            "src/lib/deep.rs",
            "src/main.rs",
            "zz.txt"
        ],
        "{shown}"
    );
    assert_eq!(screen.input(), "› @", "{shown}");
}

/// What follows the `@` filters, ignoring case: the letters in order
/// anywhere in the path. Matches within the file name come first, then the
/// fewer pieces the letters are in, then a name that starts with them, then
/// the shorter path.
#[test]
fn typing_after_the_at_filters_and_ranks() {
    let dir = ranked();
    let mut screen = Screen::new(dir.path());
    screen.type_text("look at @ApP");
    screen.draw_until("xa/yp/zp.txt", |screen| {
        screen.paths().iter().any(|path| path == "xa/yp/zp.txt")
    });
    assert_eq!(
        screen.paths(),
        [
            "apps/",
            "src/app.rs",
            "happy.txt",
            "lib/mapper.rs",
            "docs/a_p_p.md",
            "apps/web/",
            "apps/web/index.html",
            "xa/yp/zp.txt",
        ],
        "{}",
        screen.text()
    );
}

/// Tab or Enter puts `@path ` in place of the `@` word; a directory gets `/`
/// and no space, and the list goes on inside it. The message sent is that
/// text. A path with a space is in double quotes.
#[test]
fn tab_and_enter_complete_the_path() {
    let dir = ranked();
    let mut screen = Screen::new(dir.path());
    screen.type_text("see @sr");
    screen.draw_until("src/", |screen| {
        screen.paths().first().is_some_and(|path| path == "src/")
    });
    assert_eq!(screen.key(KeyCode::Tab), None);
    screen.draw_until("inside src/", |screen| {
        screen
            .paths()
            .first()
            .is_some_and(|path| path == "src/app.rs")
    });
    assert_eq!(screen.input(), "› see @src/", "{}", screen.text());
    assert_eq!(screen.key(KeyCode::Enter), None, "Enter only completes");
    screen.draw();
    assert_eq!(screen.input(), "› see @src/app.rs", "{}", screen.text());
    assert!(screen.paths().is_empty(), "{}", screen.text());
    screen.type_text("please");
    screen.draw();
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::Send("see @src/app.rs please".into()))
    );

    let mut screen = Screen::new(dir.path());
    screen.type_text("@notes");
    screen.draw_until("my notes.txt", |screen| {
        screen
            .paths()
            .first()
            .is_some_and(|path| path == "my notes.txt")
    });
    assert_eq!(screen.key(KeyCode::Tab), None);
    screen.draw();
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::Send("@\"my notes.txt\" ".into()))
    );
}

/// While the list is open, Enter does not send; Up and Down select.
#[test]
fn enter_while_the_list_is_open_does_not_send() {
    let dir = ranked();
    let mut screen = Screen::new(dir.path());
    screen.type_text("@ApP");
    screen.draw_until("the list", |screen| screen.paths().len() == 8);
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Up), None);
    screen.draw();
    assert!(
        screen.list()[1].starts_with(" › src/app.rs"),
        "{}",
        screen.text()
    );
    assert_eq!(screen.key(KeyCode::Enter), None, "Enter does not send");
    screen.draw();
    assert_eq!(screen.input(), "› @src/app.rs", "{}", screen.text());
}

/// Esc closes the list and keeps what is typed; Enter then sends it.
#[test]
fn esc_closes_the_list_and_keeps_the_input() {
    let dir = ranked();
    let mut screen = Screen::new(dir.path());
    screen.type_text("@happ");
    screen.draw_until("happy.txt", |screen| !screen.paths().is_empty());
    assert_eq!(screen.key(KeyCode::Esc), None);
    screen.draw();
    assert!(screen.paths().is_empty(), "{}", screen.text());
    assert_eq!(screen.input(), "› @happ", "{}", screen.text());
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::Send("@happ".into()))
    );
}

/// Moving the cursor off the `@` word or deleting the `@` closes the list.
#[test]
fn leaving_the_word_or_deleting_the_at_closes_the_list() {
    let dir = ranked();
    let mut screen = Screen::new(dir.path());
    screen.type_text("x @happ");
    screen.draw_until("happy.txt", |screen| !screen.paths().is_empty());
    assert_eq!(screen.key(KeyCode::Home), None);
    screen.draw();
    assert!(screen.paths().is_empty(), "{}", screen.text());
    assert_eq!(screen.key(KeyCode::End), None);
    screen.draw_until("happy.txt again", |screen| !screen.paths().is_empty());
    for _ in 0..5 {
        assert_eq!(screen.key(KeyCode::Backspace), None);
    }
    screen.draw();
    assert!(screen.paths().is_empty(), "{}", screen.text());
}

/// `@` works in the text after `/btw`, and the command list stays closed.
#[test]
fn an_at_after_a_command_lists_files() {
    let dir = ranked();
    let mut screen = Screen::new(dir.path());
    screen.type_text("/btw what is @happ");
    screen.draw_until("happy.txt", |screen| {
        screen
            .paths()
            .first()
            .is_some_and(|path| path == "happy.txt")
    });
    assert!(
        !screen.text().contains("Ask one question"),
        "{}",
        screen.text()
    );
    assert_eq!(screen.key(KeyCode::Tab), None);
    screen.draw();
    assert_eq!(
        screen.input(),
        "› /btw what is @happy.txt",
        "{}",
        screen.text()
    );
}

/// Past the cap, the walk stops and the list says so at its bottom. The
/// files are found in the background: the frame that opens the list is
/// drawn before they are.
#[test]
fn past_the_cap_the_list_says_so_without_holding_up_the_screen() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("many")).unwrap();
    for n in 0..CAP + 50 {
        std::fs::write(dir.path().join(format!("many/f{n:05}.txt")), "").unwrap();
    }
    let mut screen = Screen::new(dir.path());
    screen.type_text("@");
    screen.draw();
    assert!(!screen.text().contains(TRUNCATED), "{}", screen.text());
    screen.draw_until("the note", |screen| {
        screen
            .list()
            .last()
            .is_some_and(|row| row.contains(TRUNCATED))
    });
    assert!(!screen.paths().is_empty(), "{}", screen.text());
}
