//! The two-row status bar at the bottom of the screen: model, thinking level
//! and permission mode on the first row; directory with its git branch,
//! context use and cache hit rates on the second. Git repositories are made
//! in temporary directories, never the user's.

use std::path::Path;
use std::process::Command;

use canter_protocol::{Event, PermissionMode, SessionSnapshot};
use canter_tui::theme::{self, Depth, Theme};
use canter_tui::{App, Ui};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use tempfile::TempDir;
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
const WIDTH: u16 = 80;
const HEIGHT: u16 = 12;

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    /// A session in `cwd`, paths under `home` shown as `~`.
    fn new(cwd: &Path, home: &Path, mode: PermissionMode) -> Self {
        let mut snapshot = SessionSnapshot::empty(SESSION);
        snapshot.settings.cwd = cwd.to_str().unwrap().to_owned();
        snapshot.settings.model = "deepseek-flash".to_owned();
        snapshot.settings.approval_policy = mode;
        let app = App::new(snapshot, Some(home.to_str().unwrap().to_owned()), NOW);
        let ui = Ui::new(TestBackend::new(WIDTH, HEIGHT), app).unwrap();
        Self { ui, seq: 0 }
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

    /// 420k of a 1M context in use; cache hits 92% in total, 99% last time.
    fn usage(&mut self, window: Option<u64>) {
        let counts = |input: u64, cached: u64| {
            json!({
                "inputTokens": input, "cachedInputTokens": cached,
                "outputTokens": 0, "reasoningOutputTokens": 0, "totalTokens": input,
            })
        };
        self.event(json!({
            "type": "session.usage",
            "tokenUsage": {
                "total": counts(1000, 924),
                "last": counts(100, 99),
                "contextTokens": 420_000, "modelContextWindow": window,
            },
        }));
    }

    fn turn(&mut self, status: &str) {
        let turn = json!({
            "id": "t1", "items": [], "status": status, "error": null,
            "input": [{ "type": "text", "text": "hi" }],
            "clientName": "canter-tui", "usage": null,
        });
        if status == "inProgress" {
            self.event(json!({ "type": "turn.started", "turn": turn }));
        } else {
            self.event(json!({ "type": "turn.completed", "turn": turn }));
        }
    }

    fn resize(&mut self, width: u16) {
        self.ui.backend_mut().resize(width, HEIGHT);
    }

    /// The two bottom rows, after drawing.
    fn bar(&mut self) -> [String; 2] {
        self.ui.draw(NOW).unwrap();
        let rows = rows(self.ui.backend().buffer());
        let n = rows.len();
        [rows[n - 2].clone(), rows[n - 1].clone()]
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

/// Runs git in `dir` without the user's or the system's git config.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A home directory with `canter`, a git repository on `main`, in it.
fn repo() -> (TempDir, std::path::PathBuf) {
    let home = TempDir::new().unwrap();
    let dir = home.path().join("canter");
    std::fs::create_dir(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    (home, dir)
}

fn commit(dir: &Path) {
    git(dir, &["commit", "-q", "--allow-empty", "-m", "first"]);
}

#[test]
fn two_rows_with_usage_in_a_git_repository() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage(Some(1_000_000));
    assert_eq!(
        screen.bar(),
        [
            " deepseek-flash · think high · Always ask".to_owned(),
            " ~/canter (main) · ctx 42% 420k/1M · cache 92% (last 99%)".to_owned(),
        ]
    );
}

#[test]
fn session_cost_is_shown_after_the_permission_mode() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    assert!(!screen.bar()[0].contains('$'));
    for (usd, unknown, expected) in [
        (0.12, false, "$0.12"),
        (0.12, true, "$0.12+?"),
        (0.001, false, "<$0.01"),
        (0.0, true, "$0.00+?"),
        (0.0, false, "$0.00"),
    ] {
        let counts = json!({"inputTokens":100,"cachedInputTokens":90,"outputTokens":10,
            "reasoningOutputTokens":0,"totalTokens":110});
        screen.event(json!({"type":"session.usage","tokenUsage":{
            "total":counts,"last":counts,"contextTokens":110,"modelContextWindow":1000000,
            "cost":{"usd":usd,"unknown":unknown}}}));
        assert_eq!(
            screen.bar()[0],
            format!(" deepseek-flash · think high · Always ask · {expected}")
        );
    }
}

#[test]
fn allow_all_is_red_and_nothing_else_is() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AllowAll);
    screen.usage(Some(1_000_000));
    let [first, second] = screen.bar();
    assert_eq!(first, " deepseek-flash · think high · Allow all");
    let buffer = screen.ui.backend().buffer();
    let y = HEIGHT - 2;
    let start = first[..first.find("Allow all").unwrap()].width() as u16;
    for x in 0..first.width() as u16 {
        let style = buffer[(x, y)].style();
        let in_mode = x >= start && x < start + "Allow all".len() as u16;
        if in_mode {
            assert_eq!(style.fg, Some(Color::Red), "column {x}");
        } else {
            assert_ne!(style.fg, Some(Color::Red), "column {x}");
        }
    }
    for x in 0..second.width() as u16 {
        let style = buffer[(x, HEIGHT - 1)].style();
        assert_ne!(style.fg, Some(Color::Red), "column {x}");
    }
}

#[test]
fn outside_a_git_repository_there_are_no_brackets() {
    let home = TempDir::new().unwrap();
    let dir = home.path().join("plain");
    std::fs::create_dir(&dir).unwrap();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage(Some(1_000_000));
    assert_eq!(
        screen.bar()[1],
        " ~/plain · ctx 42% 420k/1M · cache 92% (last 99%)"
    );
}

#[test]
fn a_detached_head_shows_the_short_commit() {
    let (home, dir) = repo();
    commit(&dir);
    git(&dir, &["checkout", "-q", "--detach"]);
    let sha = git(&dir, &["rev-parse", "HEAD"]);
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    assert_eq!(screen.bar()[1], format!(" ~/canter ({})", &sha[..7]));
}

#[test]
fn a_worktree_shows_its_own_branch() {
    let (home, dir) = repo();
    commit(&dir);
    let worktree = home.path().join("wt");
    git(
        &dir,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature/x",
            worktree.to_str().unwrap(),
        ],
    );
    assert!(worktree.join(".git").is_file());
    // A directory inside the worktree finds it too.
    let inner = worktree.join("src");
    std::fs::create_dir(&inner).unwrap();
    let mut screen = Screen::new(&inner, home.path(), PermissionMode::AlwaysAsk);
    assert_eq!(screen.bar()[1], " ~/wt/src (feature/x)");
}

#[test]
fn without_usage_or_window_there_is_no_ctx() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    assert_eq!(screen.bar()[1], " ~/canter (main)", "no usage yet");
    screen.usage(None);
    assert_eq!(
        screen.bar()[1],
        " ~/canter (main) · cache 92% (last 99%)",
        "window unknown"
    );
}

#[test]
fn a_narrow_window_cuts_each_row_on_its_own() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage(Some(1_000_000));
    screen.resize(24);
    assert_eq!(
        screen.bar(),
        [
            " deepseek-flash · think…".to_owned(),
            " ~/canter (main) · ctx …".to_owned(),
        ]
    );
}

#[test]
fn the_branch_is_read_again_when_a_turn_ends() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.turn("inProgress");
    assert_eq!(screen.bar()[1], " ~/canter (main)");

    git(&dir, &["symbolic-ref", "HEAD", "refs/heads/other"]);
    assert_eq!(
        screen.bar()[1],
        " ~/canter (main)",
        "not read again every frame"
    );
    screen.turn("completed");
    assert_eq!(screen.bar()[1], " ~/canter (other)");
}

impl Screen {
    /// `used` of a `window` context, with the compaction line at `line` when
    /// the server sends one; cost and cache hits as in [`Screen::usage`].
    fn usage_with_line(&mut self, used: u64, window: u64, line: Option<u64>) {
        let counts = |input: u64, cached: u64| {
            json!({
                "inputTokens": input, "cachedInputTokens": cached,
                "outputTokens": 0, "reasoningOutputTokens": 0, "totalTokens": input,
            })
        };
        let mut usage = json!({
            "total": counts(1000, 924),
            "last": counts(100, 99),
            "contextTokens": used, "modelContextWindow": window,
            "cost": { "usd": 0.12, "unknown": false },
        });
        if let Some(line) = line {
            usage["autoCompactTokens"] = json!(line);
        }
        self.event(json!({ "type": "session.usage", "tokenUsage": usage }));
    }

    /// The text colour of `text` on bottom row `row` (0 or 1), and whether it
    /// is dim; the same in every column of it.
    fn style_of(&mut self, row: usize, text: &str) -> (Option<Color>, bool) {
        let line = self.bar()[row].clone();
        let at = line
            .find(text)
            .unwrap_or_else(|| panic!("{text:?} in {line:?}"));
        let start = line[..at].width() as u16;
        let y = HEIGHT - 2 + row as u16;
        let buffer = self.ui.backend().buffer();
        let styles: Vec<_> = (start..start + text.width() as u16)
            .map(|x| {
                let style = buffer[(x, y)].style();
                (style.fg, style.add_modifier.contains(Modifier::DIM))
            })
            .collect();
        assert!(
            styles.windows(2).all(|pair| pair[0] == pair[1]),
            "{text:?}: {styles:?}"
        );
        styles[0]
    }
}

#[test]
fn compact_says_how_much_is_left_before_automatic_compaction() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    // 420k in use: (line − 420k) ÷ line, rounded down; 0% at or over the line.
    for (line, left) in [
        (800_000, "47%"),
        (600_000, "30%"),
        (420_000, "0%"),
        (400_000, "0%"),
    ] {
        screen.usage_with_line(420_000, 1_000_000, Some(line));
        assert_eq!(
            screen.bar()[1],
            format!(
                " ~/canter (main) · ctx 42% 420k/1M · {left} to compact · cache 92% (last 99%)"
            ),
            "line {line}"
        );
    }
    // An older server sends no line: no compact item.
    screen.usage_with_line(420_000, 1_000_000, None);
    assert_eq!(
        screen.bar()[1],
        " ~/canter (main) · ctx 42% 420k/1M · cache 92% (last 99%)"
    );
}

#[test]
fn each_item_has_its_colour_and_the_percentages_turn_with_the_room_left() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage_with_line(420_000, 1_000_000, Some(1_050_000));
    let colour = |color| (Some(color), false);
    let grey = (Some(Color::Reset), true);
    assert_eq!(screen.style_of(0, "deepseek-flash"), colour(Color::Cyan));
    assert_eq!(screen.style_of(0, "think high"), colour(Color::Magenta));
    assert_eq!(screen.style_of(0, "Always ask"), grey);
    assert_eq!(screen.style_of(0, "$0.12"), colour(Color::Yellow));
    assert_eq!(screen.style_of(0, " · "), grey);
    assert_eq!(screen.style_of(1, "~/canter"), colour(Color::Blue));
    assert_eq!(screen.style_of(1, "(main)"), colour(Color::Green));
    assert_eq!(screen.style_of(1, "ctx "), grey);
    assert_eq!(screen.style_of(1, " 420k/1M"), grey);
    assert_eq!(screen.style_of(1, " to compact"), grey);
    assert_eq!(screen.style_of(1, "cache "), grey);
    assert_eq!(screen.style_of(1, "92%"), colour(Color::Cyan));
    assert_eq!(screen.style_of(1, " (last "), grey);
    assert_eq!(screen.style_of(1, "99%"), colour(Color::Cyan));

    // ctx and compact numbers by the room left before the line.
    for (line, left, color) in [
        (1_050_000, "60%", Color::Green),
        (600_000, "30%", Color::Yellow),
        (466_700, "10%", Color::Red),
    ] {
        screen.usage_with_line(420_000, 1_000_000, Some(line));
        assert_eq!(screen.style_of(1, "42%"), colour(color), "line {line}");
        assert_eq!(screen.style_of(1, left), colour(color), "line {line}");
    }
    // Without a line, ctx's number by the room left in the window.
    screen.usage_with_line(420_000, 450_000, None);
    assert_eq!(screen.style_of(1, "93%"), colour(Color::Red));
    screen.usage_with_line(420_000, 1_000_000, None);
    assert_eq!(screen.style_of(1, "42%"), colour(Color::Green));

    // NO_COLOR: grey as before, no colours.
    theme::set(Theme {
        depth: Depth::None,
        ..Theme::default()
    });
    screen.usage_with_line(420_000, 1_000_000, Some(466_700));
    for (row, text) in [
        (0, "deepseek-flash"),
        (0, "$0.12"),
        (1, "(main)"),
        (1, "42%"),
        (1, "10%"),
        (1, "92%"),
    ] {
        assert_eq!(screen.style_of(row, text), grey, "{text}");
    }
    theme::set(Theme::default());
}
