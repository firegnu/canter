//! The two-row status bar at the bottom of the screen (P2-243): model,
//! thinking level, permission mode and sandbox on the first row, with context
//! use, cache hits and cost on its right; directory, git branch and lines
//! changed on the second. Git repositories are made in temporary
//! directories, never the user's.

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
        Self::of(app(cwd, home, mode))
    }

    /// As `canter` opens it: the lines changed in git are counted too.
    fn with_git_diff(cwd: &Path, home: &Path) -> Self {
        Self::of(app(cwd, home, PermissionMode::AlwaysAsk).with_git_diff())
    }

    fn of(app: App) -> Self {
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

fn app(cwd: &Path, home: &Path, mode: PermissionMode) -> App {
    let mut snapshot = SessionSnapshot::empty(SESSION);
    snapshot.settings.cwd = cwd.to_str().unwrap().to_owned();
    snapshot.settings.model = "deepseek-flash".to_owned();
    snapshot.settings.approval_policy = mode;
    App::new(snapshot, Some(home.to_str().unwrap().to_owned()), NOW)
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

/// The first row of the default items with `deepseek-flash` in Always ask.
const LEFT: &str = " deepseek-flash high   Always ask";

/// `left`, then `right` ending a column before the edge of `width`.
fn spread(left: &str, right: &str, width: u16) -> String {
    let pad = usize::from(width) - 1 - left.width() - right.width();
    format!("{left}{}{right}", " ".repeat(pad))
}

#[test]
fn two_rows_with_usage_in_a_git_repository() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage(Some(1_000_000));
    assert_eq!(
        screen.bar(),
        [
            spread(LEFT, "████░░░░░░  42%   cache  92%", WIDTH),
            " ~/canter  main".to_owned(),
        ]
    );
}

/// The rows as the user chose them (P2-243), in a wide window: the
/// provider dim before the model, Allow all and the sandbox off in red.
#[test]
fn a_wide_window_shows_every_item_where_it_goes() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AllowAll);
    let mut settings = sandbox_settings(&dir, "allowAll", false, false);
    settings["settings"]["model"] = json!("opencode-go/deepseek-v4.1-flash");
    screen.event(settings);
    screen.usage_with_cache(232_000, 400_000, 980, 0.12);
    screen.resize(120);
    let left = " opencode-go/deepseek-v4.1-flash high   Allow all · Sandbox off";
    assert_eq!(
        screen.bar(),
        [
            spread(left, "██████░░░░  58%   cache  98%   $0.12", 120),
            " ~/canter  main".to_owned(),
        ]
    );
    let grey = (Some(Color::Reset), true);
    let plain = (Some(Color::Reset), false);
    let red = (Some(Color::Red), false);
    assert_eq!(screen.style_of(0, "opencode-go/"), grey);
    assert_eq!(screen.style_of(0, "deepseek-v4.1-flash"), plain);
    assert_eq!(screen.style_of(0, "high"), grey);
    assert_eq!(screen.style_of(0, "Allow all"), red);
    assert_eq!(screen.style_of(0, "Sandbox off"), red);
    assert_eq!(screen.style_of(0, "██████"), (Some(Color::Yellow), false));
    assert_eq!(screen.style_of(0, " 58%"), plain);
    assert_eq!(screen.style_of(0, "cache"), grey);
    assert_eq!(screen.style_of(0, " 98%"), plain);
    assert_eq!(screen.style_of(0, "$0.12"), plain);
}

/// Too narrow for all of it: cache goes first, then the cost, then the
/// context, each whole; then the first row is cut at its end.
#[test]
fn a_narrow_window_drops_the_items_on_the_right_whole_from_the_right() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage_with_line(420_000, 1_000_000, Some(1_000_000));
    for (width, right) in [
        (73, "████░░░░░░  42%   cache  92%   $0.12"),
        (72, "████░░░░░░  42%   $0.12"),
        (60, "████░░░░░░  42%   $0.12"),
        (59, "████░░░░░░  42%"),
        (52, "████░░░░░░  42%"),
    ] {
        screen.resize(width);
        assert_eq!(screen.bar()[0], spread(LEFT, right, width), "width {width}");
    }
    screen.resize(51);
    assert_eq!(screen.bar()[0], LEFT);
    screen.resize(24);
    assert_eq!(
        screen.bar(),
        [
            " deepseek-flash high   …".to_owned(),
            " ~/canter  main".to_owned(),
        ]
    );
}

#[test]
fn session_cost_is_on_the_right_after_cache() {
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
            spread(
                LEFT,
                &format!("░░░░░░░░░░   0%   cache  90%   {expected}"),
                WIDTH
            )
        );
    }
}

#[test]
fn allow_all_is_red_and_nothing_else_is() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AllowAll);
    screen.usage(Some(1_000_000));
    let [first, second] = screen.bar();
    assert_eq!(
        first,
        spread(
            " deepseek-flash high   Allow all",
            "████░░░░░░  42%   cache  92%",
            WIDTH
        )
    );
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
fn outside_a_git_repository_the_directory_is_alone() {
    let home = TempDir::new().unwrap();
    let dir = home.path().join("plain");
    std::fs::create_dir(&dir).unwrap();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage(Some(1_000_000));
    assert_eq!(screen.bar()[1], " ~/plain");
}

#[test]
fn a_detached_head_shows_the_short_commit() {
    let (home, dir) = repo();
    commit(&dir);
    git(&dir, &["checkout", "-q", "--detach"]);
    let sha = git(&dir, &["rev-parse", "HEAD"]);
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    assert_eq!(screen.bar()[1], format!(" ~/canter  {}", &sha[..7]));
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
    assert_eq!(screen.bar()[1], " ~/wt/src  feature/x");
}

#[test]
fn without_usage_or_window_there_is_no_context() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    assert_eq!(
        screen.bar(),
        [LEFT.to_owned(), " ~/canter  main".to_owned()],
        "no usage yet"
    );
    screen.usage(None);
    assert_eq!(
        screen.bar()[0],
        spread(LEFT, "cache  92%", WIDTH),
        "window unknown"
    );
}

#[test]
fn the_branch_is_read_again_when_a_turn_ends() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.turn("inProgress");
    assert_eq!(screen.bar()[1], " ~/canter  main");

    git(&dir, &["symbolic-ref", "HEAD", "refs/heads/other"]);
    assert_eq!(
        screen.bar()[1],
        " ~/canter  main",
        "not read again every frame"
    );
    screen.turn("completed");
    assert_eq!(screen.bar()[1], " ~/canter  other");
}

impl Screen {
    /// `used` of a `window` context, with the compaction line at `line` when
    /// the server sends one; cost and cache hits as in [`Screen::usage`].
    fn usage_with_line(&mut self, used: u64, window: u64, line: Option<u64>) {
        self.usage_of(used, window, line, 924, 0.12);
    }

    /// `used` of a context compacted at `line`, `cached` of 1000 input
    /// tokens from the cache, and `usd` spent.
    fn usage_with_cache(&mut self, used: u64, line: u64, cached: u64, usd: f64) {
        self.usage_of(used, 1_000_000, Some(line), cached, usd);
    }

    fn usage_of(&mut self, used: u64, window: u64, line: Option<u64>, cached: u64, usd: f64) {
        let counts = |input: u64, cached: u64| {
            json!({
                "inputTokens": input, "cachedInputTokens": cached,
                "outputTokens": 0, "reasoningOutputTokens": 0, "totalTokens": input,
            })
        };
        let mut usage = json!({
            "total": counts(1000, cached),
            "last": counts(100, 99),
            "contextTokens": used, "modelContextWindow": window,
            "cost": { "usd": usd, "unknown": false },
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

    /// Every row of the screen drawn at `now`.
    fn screen_at(&mut self, now: i64) -> Vec<String> {
        self.ui.draw(now).unwrap();
        rows(self.ui.backend().buffer())
    }
}

#[test]
fn context_is_a_bar_that_is_full_at_the_compaction_line() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    // Ten cells and the percent of the line in use, both rounded, the
    // percent in four columns; past the line the bar stays full and the
    // percent says how far past.
    for (used, expected) in [
        (0, "░░░░░░░░░░   0%"),
        (120_000, "███░░░░░░░  30%"),
        (184_000, "█████░░░░░  46%"),
        (400_000, "██████████ 100%"),
        (420_000, "██████████ 105%"),
    ] {
        screen.usage_with_line(used, 1_000_000, Some(400_000));
        assert_eq!(
            screen.bar()[0],
            spread(LEFT, &format!("{expected}   cache  92%   $0.12"), WIDTH),
            "{used} used"
        );
    }
    // An older server sends no line: full is the window.
    screen.usage_with_line(420_000, 1_000_000, None);
    assert_eq!(
        screen.bar()[0],
        spread(LEFT, "████░░░░░░  42%   cache  92%   $0.12", WIDTH)
    );
}

#[test]
fn only_what_needs_attention_has_a_colour_and_the_bar_turns_with_the_room_left() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage_with_line(400_000, 1_000_000, Some(1_000_000));
    let colour = |color| (Some(color), false);
    let grey = (Some(Color::Reset), true);
    let plain = (Some(Color::Reset), false);
    assert_eq!(screen.style_of(0, "deepseek-flash"), plain);
    assert_eq!(screen.style_of(0, "high"), grey);
    assert_eq!(screen.style_of(0, "Always ask"), grey);
    assert_eq!(screen.style_of(0, "░░░░░░"), grey);
    assert_eq!(screen.style_of(0, " 40%"), plain);
    assert_eq!(screen.style_of(0, "cache"), grey);
    assert_eq!(screen.style_of(0, " 92%"), plain);
    assert_eq!(screen.style_of(0, "$0.12"), plain);
    assert_eq!(screen.style_of(1, "~/canter"), plain);
    assert_eq!(screen.style_of(1, "main"), colour(Color::Green));

    // The cells in use by the room left before the line.
    for (used, cells, color) in [
        (400_000, "████", Color::Green),
        (700_000, "███████", Color::Yellow),
        (900_000, "█████████", Color::Red),
    ] {
        screen.usage_with_line(used, 1_000_000, Some(1_000_000));
        assert_eq!(screen.style_of(0, cells), colour(color), "{used}");
        assert_eq!(screen.style_of(0, "░"), grey, "{used}");
    }
    // Without a line, by the room left in the window.
    screen.usage_with_line(420_000, 450_000, None);
    assert_eq!(screen.style_of(0, "█████████"), colour(Color::Red));
    screen.usage_with_line(420_000, 1_000_000, None);
    assert_eq!(screen.style_of(0, "████"), colour(Color::Green));

    // NO_COLOR: no colours, the cells and the branch grey; the cells still
    // tell how much.
    theme::set(Theme {
        depth: Depth::None,
        ..Theme::default()
    });
    screen.usage_with_line(900_000, 1_000_000, Some(1_000_000));
    assert_eq!(
        screen.bar()[0],
        spread(LEFT, "█████████░  90%   cache  92%   $0.12", WIDTH)
    );
    for (row, text) in [(0, "█████████"), (1, "main")] {
        assert_eq!(screen.style_of(row, text), grey, "{text}");
    }
    theme::set(Theme::default());
}

impl Screen {
    /// Draws until the second row is `expected`, letting git run meanwhile.
    async fn wait_for_second_row(&mut self, expected: &str) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if self.bar()[1] == expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{:?}",
                self.bar()[1]
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

#[tokio::test]
async fn the_branch_is_followed_by_the_lines_changed_since_the_last_commit() {
    let (home, dir) = repo();
    std::fs::write(dir.join("a.txt"), "1\n2\n3\n4\n5\n").unwrap();
    std::fs::write(dir.join("b.txt"), "x\ny\nz\n").unwrap();
    std::fs::write(dir.join("image.bin"), b"\0\x01\x02binary\0").unwrap();
    git(&dir, &["add", "."]);
    commit(&dir);
    let mut screen = Screen::with_git_diff(&dir, home.path());
    // Nothing changed: nothing after the branch.
    screen.wait_for_second_row(" ~/canter  main").await;

    // Not staged: one line changed and two added. Staged: one line changed.
    std::fs::write(dir.join("a.txt"), "1\nTWO\n3\n4\n5\n6\n7\n").unwrap();
    std::fs::write(dir.join("b.txt"), "x\nY\nz\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    // Neither a binary file nor an untracked one counts lines.
    std::fs::write(dir.join("image.bin"), b"\0\x03\x04other\0\n\n").unwrap();
    std::fs::write(dir.join("new.txt"), "a\nb\nc\n").unwrap();
    screen.turn("completed");
    screen.wait_for_second_row(" ~/canter  main  +4 −2").await;

    let colour = |color| (Some(color), false);
    assert_eq!(screen.style_of(1, "+4"), colour(Color::Green));
    assert_eq!(screen.style_of(1, "−2"), colour(Color::Red));
}

#[tokio::test]
async fn before_the_first_commit_the_staged_lines_count() {
    let (home, dir) = repo();
    std::fs::write(dir.join("a.txt"), "1\n2\n3\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    std::fs::write(dir.join("a.txt"), "1\n2\n3\n4\n").unwrap();
    let mut screen = Screen::with_git_diff(&dir, home.path());
    screen.wait_for_second_row(" ~/canter  main  +3 −0").await;
}

/// Numbers that change their number of digits move nothing else: not the
/// first row's left part, not the context and cache, not the directory.
#[tokio::test]
async fn numbers_changing_width_move_neither_the_left_nor_the_directory() {
    let (home, dir) = repo();
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "."]);
    commit(&dir);
    std::fs::write(dir.join("a.txt"), "y\n").unwrap();
    let mut screen = Screen::with_git_diff(&dir, home.path());
    screen.usage_with_line(36_000, 1_000_000, Some(400_000));
    screen.wait_for_second_row(" ~/canter  main  +1 −1").await;
    let first = screen.bar()[0].clone();
    let cache = |row: &str| row[..row.find("cache").unwrap()].width();
    for used in [232_000, 400_000, 4_000_000] {
        screen.usage_with_line(used, 1_000_000, Some(400_000));
        let [now, second] = screen.bar();
        assert!(now.starts_with(&format!("{LEFT}  ")), "{now:?}");
        assert_eq!(cache(&now), cache(&first), "{now:?}");
        assert_eq!(now.width(), first.width(), "{now:?}");
        assert_eq!(second, " ~/canter  main  +1 −1");
    }
    // A longer cost moves the right items left, never the left part.
    screen.usage_of(232_000, 1_000_000, Some(400_000), 924, 10.12);
    assert!(screen.bar()[0].starts_with(&format!("{LEFT}  ")));
    // More digits in the lines changed: the directory and branch stay.
    std::fs::write(dir.join("a.txt"), "y\n".repeat(1000)).unwrap();
    screen.turn("completed");
    screen
        .wait_for_second_row(" ~/canter  main  +1000 −1")
        .await;
}

/// The speed is on the line saying what the turn does, while the model
/// writes and when `/statusline` shows it: in whole tokens a second, three
/// columns wide, so what follows stays put.
#[test]
fn the_speed_is_on_the_working_line_while_the_model_writes() {
    let (home, dir) = repo();
    let canter_home = home.path().join("canter-home");
    std::fs::create_dir(&canter_home).unwrap();
    std::fs::write(
        canter_home.join("tui.json"),
        json!({"statusLine": {"items": [{"name": "tps", "show": true}]}}).to_string(),
    )
    .unwrap();
    let env = std::collections::BTreeMap::from([(
        "CANTER_HOME".to_owned(),
        canter_home.to_str().unwrap().to_owned(),
    )]);
    let app = app(&dir, home.path(), PermissionMode::AlwaysAsk).with_status_line(None, env);
    let mut screen = Screen::of(app);
    screen.turn("inProgress");
    screen.event(json!({"type": "session.status", "status": "working"}));
    screen.event(json!({"type": "item.started", "turnId": "t1",
        "item": {"type": "agentMessage", "id": "m1", "text": ""}}));
    let working = |rows: &[String]| {
        rows.iter()
            .find(|row| row.contains("Esc to interrupt"))
            .cloned()
            .unwrap_or_else(|| panic!("{rows:#?}"))
    };
    assert!(!working(&screen.screen_at(1000)).contains("tok/s"));
    // 177 bytes: 59 tokens, from the first text on.
    screen.event(json!({"type": "item.delta", "turnId": "t1", "itemId": "m1",
        "delta": {"type": "agentMessage", "delta": "a".repeat(177)}}));
    let mut info = None;
    for (now, speed) in [(371, "159"), (1000, " 59"), (6_500, "  9")] {
        let rows = screen.screen_at(now);
        let line = working(&rows);
        assert!(
            line.contains(&format!(" · {speed} tok/s · Esc to interrupt")),
            "{line:?}"
        );
        let bottom = rows[rows.len() - 2..].to_vec();
        assert_eq!(*info.get_or_insert(bottom.clone()), bottom, "{now}");
    }
    // The response done, it goes.
    screen.event(json!({"type": "item.completed", "turnId": "t1",
        "item": {"type": "agentMessage", "id": "m1", "text": "a".repeat(177)}}));
    assert!(!working(&screen.screen_at(20_000)).contains("tok/s"));
}

/// The speed is not shown until `/statusline` turns it on.
#[test]
fn the_speed_is_off_by_default() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.turn("inProgress");
    screen.event(json!({"type": "session.status", "status": "working"}));
    screen.event(json!({"type": "item.started", "turnId": "t1",
        "item": {"type": "agentMessage", "id": "m1", "text": ""}}));
    screen.event(json!({"type": "item.delta", "turnId": "t1", "itemId": "m1",
        "delta": {"type": "agentMessage", "delta": "a".repeat(177)}}));
    let rows = screen.screen_at(1000);
    assert!(
        rows.iter().any(|row| row.contains("Esc to interrupt")),
        "{rows:#?}"
    );
    assert!(!rows.iter().any(|row| row.contains("tok/s")), "{rows:#?}");
}

/// In Plan the permission item also names the mode `/plan` leaves Plan for,
/// in that mode's colour; `/mode` in Plan changes it at once.
#[test]
fn plan_names_the_mode_it_leaves_for() {
    use canter_tui::Action;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    let colour = |color| (Some(color), false);
    let grey = (Some(Color::Reset), true);
    let settings = |policy: &str, plan: bool| {
        let mut settings = json!({
            "cwd": dir.to_str().unwrap(), "name": null, "model": "deepseek-flash",
            "approvalPolicy": policy, "planMode": plan,
        });
        if plan {
            settings["prePlanApprovalPolicy"] = json!(policy);
        }
        json!({ "type": "session.updated", "settings": settings })
    };

    screen.event(settings("alwaysAsk", true));
    assert_eq!(
        screen.bar()[0],
        " deepseek-flash high   Plan (then Always ask)"
    );
    assert_eq!(screen.style_of(0, "Plan"), colour(Color::Magenta));
    assert_eq!(screen.style_of(0, " (then "), grey);
    assert_eq!(screen.style_of(0, "Always ask"), grey);
    assert_eq!(screen.style_of(0, ")"), grey);

    // `/mode allow-all` in Plan, and the update the server sends back.
    for c in "/mode allow-all".chars() {
        screen.ui.draw(NOW).unwrap();
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        assert_eq!(screen.ui.app_mut().handle_key(key, NOW), None);
    }
    screen.ui.draw(NOW).unwrap();
    let enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    assert!(matches!(
        screen.ui.app_mut().handle_key(enter, NOW),
        Some(Action::Update(params))
            if params.approval_policy == Some(PermissionMode::AllowAll) && params.plan_mode.is_none()
    ));
    screen.event(settings("allowAll", true));
    assert_eq!(
        screen.bar()[0],
        " deepseek-flash high   Plan (then Allow all)"
    );
    assert_eq!(screen.style_of(0, "Plan"), colour(Color::Magenta));
    assert_eq!(screen.style_of(0, "Allow all"), colour(Color::Red));
    assert_eq!(screen.style_of(0, ")"), grey);

    // Out of Plan, the mode alone as before.
    screen.event(settings("allowAll", false));
    assert_eq!(screen.bar()[0], " deepseek-flash high   Allow all");
    assert_eq!(screen.style_of(0, "Allow all"), colour(Color::Red));
}

/// `session.updated` with `sandboxEnabled`, as a server that reports it sends.
fn sandbox_settings(dir: &Path, policy: &str, plan: bool, sandbox: bool) -> Value {
    let mut settings = json!({
        "cwd": dir.to_str().unwrap(), "name": null, "model": "deepseek-flash",
        "approvalPolicy": policy, "planMode": plan, "sandboxEnabled": sandbox,
    });
    if plan {
        settings["prePlanApprovalPolicy"] = json!(policy);
    }
    json!({ "type": "session.updated", "settings": settings })
}

#[test]
fn the_sandbox_state_follows_the_mode_red_when_off_and_absent_from_older_servers() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AskForExecution);
    // An older server does not say: nothing, never `Sandbox on`.
    assert_eq!(screen.bar()[0], " deepseek-flash high   Ask for execution");
    let grey = (Some(Color::Reset), true);
    screen.event(sandbox_settings(&dir, "askForExecution", false, true));
    assert_eq!(
        screen.bar()[0],
        " deepseek-flash high   Ask for execution · Sandbox on"
    );
    assert_eq!(screen.style_of(0, "Sandbox on"), grey);
    screen.event(sandbox_settings(&dir, "allowAll", false, false));
    assert_eq!(
        screen.bar()[0],
        " deepseek-flash high   Allow all · Sandbox off"
    );
    assert_eq!(screen.style_of(0, "Sandbox off"), (Some(Color::Red), false));
    screen.event(sandbox_settings(&dir, "alwaysAsk", true, true));
    assert_eq!(
        screen.bar()[0],
        " deepseek-flash high   Plan (then Always ask) · Sandbox on"
    );
    // Cost goes on the right.
    screen.event(sandbox_settings(&dir, "alwaysAsk", false, true));
    screen.event(json!({
        "type": "session.usage",
        "tokenUsage": {
            "total": {"inputTokens": 1, "cachedInputTokens": 0, "outputTokens": 0, "reasoningOutputTokens": 0, "totalTokens": 1},
            "last": {"inputTokens": 1, "cachedInputTokens": 0, "outputTokens": 0, "reasoningOutputTokens": 0, "totalTokens": 1},
            "contextTokens": 1, "modelContextWindow": null,
            "cost": {"usd": 0.31, "unknown": false},
        },
    }));
    assert_eq!(
        screen.bar()[0],
        spread(
            " deepseek-flash high   Always ask · Sandbox on",
            "cache   0%   $0.31",
            WIDTH
        )
    );
}

#[test]
fn a_narrow_window_leaves_the_sandbox_state_out_rather_than_cut_it() {
    use canter_protocol::SessionUpdateParams;
    use canter_tui::Action;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AskForExecution);
    screen.event(sandbox_settings(&dir, "askForExecution", false, true));
    let full = " deepseek-flash high   Ask for execution · Sandbox on";
    screen.resize(full.width() as u16);
    assert_eq!(screen.bar()[0], full);
    for width in [full.width() as u16 - 1, 40] {
        screen.resize(width);
        assert_eq!(
            screen.bar()[0],
            " deepseek-flash high   Ask for execution",
            "width {width}"
        );
    }
    // The mode is still all there, so Shift+Tab still switches it.
    screen.ui.draw(NOW).unwrap();
    assert_eq!(
        screen
            .ui
            .app_mut()
            .handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), NOW),
        Some(Action::Update(SessionUpdateParams {
            approval_policy: Some(PermissionMode::AllowAll),
            ..Default::default()
        }))
    );
}

/// `~/work/<30 a>/src/canter`, a git repository on `main` with one commit
/// and one line changed since.
fn long_repo() -> (TempDir, std::path::PathBuf) {
    let home = TempDir::new().unwrap();
    let dir = home
        .path()
        .join("work")
        .join("a".repeat(30))
        .join("src")
        .join("canter");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "."]);
    commit(&dir);
    std::fs::write(dir.join("a.txt"), "y\n").unwrap();
    (home, dir)
}

/// How wide the directory can be depends only on the window and the
/// branch: the lines changed show only when they fit after it. When it does
/// not fit it loses directories in the middle, as the welcome header shows
/// it, down to its last part; only past that is the row cut at its end.
#[tokio::test]
async fn a_long_directory_is_shortened_in_the_middle_before_the_row_is_cut() {
    let (home, dir) = long_repo();
    let mut screen = Screen::with_git_diff(&dir, home.path());
    let path = format!(" ~/work/{}/src/canter", "a".repeat(30));
    let full = format!("{path}  main  +1 −1");
    screen.resize(full.width() as u16);
    screen.wait_for_second_row(&full).await;
    // One column less: the lines changed go; the directory stays whole.
    screen.resize(full.width() as u16 - 1);
    assert_eq!(screen.bar()[1], format!("{path}  main"));
    let exact = format!("{path}  main").width() as u16;
    screen.resize(exact);
    assert_eq!(screen.bar()[1], format!("{path}  main"));
    // Then a directory at a time.
    screen.resize(exact - 1);
    assert_eq!(
        screen.bar()[1],
        format!(" ~/…/{}/src/canter  main", "a".repeat(30))
    );
    screen.resize(20);
    assert_eq!(screen.bar()[1], " ~/…/canter  main");
    // Not even the last part leaves room: the row is cut at its end.
    screen.resize(12);
    let second = screen.bar()[1].clone();
    assert!(
        second.starts_with(" …/canter") && second.ends_with('…'),
        "{second:?}"
    );
    assert_eq!(second.width(), 12);
}

/// A `tui.json` saved when the order was the user's: only which items show
/// counts, those not listed hidden; they go where they always go now.
#[test]
fn a_tui_json_saved_with_its_own_order_keeps_only_what_shows() {
    let (home, dir) = repo();
    let canter_home = home.path().join("canter-home");
    std::fs::create_dir(&canter_home).unwrap();
    std::fs::write(
        canter_home.join("tui.json"),
        json!({"statusLine": {"items": [
            {"name": "directory"}, {"name": "branch"}, {"name": "model"}, {"name": "line2"},
            {"name": "context"}, {"name": "cache", "show": false},
        ]}})
        .to_string(),
    )
    .unwrap();
    let env = std::collections::BTreeMap::from([(
        "CANTER_HOME".to_owned(),
        canter_home.to_str().unwrap().to_owned(),
    )]);
    let app = app(&dir, home.path(), PermissionMode::AlwaysAsk).with_status_line(None, env);
    let mut screen = Screen::of(app);
    screen.usage(Some(1_000_000));
    assert_eq!(
        screen.bar(),
        [
            spread(" deepseek-flash", "████░░░░░░  42%", WIDTH),
            " ~/canter  main".to_owned(),
        ]
    );
}

#[test]
fn session_cost_distinguishes_plan_only_mixed_and_mixed_unknown() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    for (usd, unknown, expected) in [
        (0.0, false, "ChatGPT plan"),
        (0.12, false, "$0.12 + ChatGPT plan"),
        (0.12, true, "$0.12+? + ChatGPT plan"),
        (0.0, true, "$0.00+? + ChatGPT plan"),
    ] {
        let counts = json!({"inputTokens":100,"cachedInputTokens":90,"outputTokens":10,
            "reasoningOutputTokens":0,"totalTokens":110});
        screen.event(json!({"type":"session.usage","tokenUsage":{
            "total":counts,"last":counts,"contextTokens":110,"modelContextWindow":1000000,
            "cost":{"usd":usd,"unknown":unknown,"chatgptPlan":true}}}));
        assert!(
            screen.bar()[0].ends_with(expected),
            "{} did not end with {expected}",
            screen.bar()[0]
        );
    }
}
