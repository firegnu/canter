//! The status bar at the bottom of the screen (P2-245): model, thinking
//! level, permission mode, sandbox and cost on the first row; context use,
//! cache hits, speed, version and session name on the second; directory,
//! git branch and lines changed on the third. Each item between ` · `, all
//! on the left. Git repositories are made in temporary directories, never
//! the user's.

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
        self.event_at(NOW, payload);
    }

    fn event_at(&mut self, at: i64, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": at });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, at);
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

    /// The info rows under the input box, after drawing.
    fn bar(&mut self) -> Vec<String> {
        self.bar_at(NOW)
    }

    fn bar_at(&mut self, now: i64) -> Vec<String> {
        self.ui.draw(now).unwrap();
        let rows = rows(self.ui.backend().buffer());
        rows[self.info_top(&rows)..].to_vec()
    }

    /// The first info row: right under the input box.
    fn info_top(&self, rows: &[String]) -> usize {
        rows.iter().rposition(|row| row.starts_with('╰')).unwrap() + 1
    }
}

fn app(cwd: &Path, home: &Path, mode: PermissionMode) -> App {
    let mut snapshot = SessionSnapshot::empty(SESSION);
    snapshot.settings.cwd = cwd.to_str().unwrap().to_owned();
    snapshot.settings.model = "deepseek-flash".to_owned();
    snapshot.settings.approval_policy = mode;
    App::new(snapshot, Some(home.to_str().unwrap().to_owned()), NOW)
}

/// [`app`] with `tui.json` in a `CANTER_HOME` of its own under `home`
/// showing `items` as well as the defaults.
fn app_showing(cwd: &Path, home: &Path, items: &[&str]) -> App {
    let canter_home = home.join("canter-home");
    std::fs::create_dir_all(&canter_home).unwrap();
    let mut listed: Vec<Value> = [
        "model",
        "thinking",
        "permission",
        "cost",
        "line2",
        "directory",
        "branch",
        "diff",
        "context",
        "cache",
    ]
    .iter()
    .map(|name| json!({ "name": name }))
    .collect();
    listed.extend(
        items
            .iter()
            .map(|name| json!({ "name": name, "show": true })),
    );
    std::fs::write(
        canter_home.join("tui.json"),
        json!({ "statusLine": { "items": listed } }).to_string(),
    )
    .unwrap();
    let env = std::collections::BTreeMap::from([(
        "CANTER_HOME".to_owned(),
        canter_home.to_str().unwrap().to_owned(),
    )]);
    app(cwd, home, PermissionMode::AlwaysAsk).with_status_line(None, env)
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

/// The text colour of some text on the rows, whether it is dim and whether
/// it is bold.
type Look = (Option<Color>, bool, bool);

/// The words around the items.
const GREY: Look = (Some(Color::Reset), true, false);
/// In the terminal's own colour.
const PLAIN: Look = (Some(Color::Reset), false, false);
/// In the terminal's own colour, bold.
const BOLD: Look = (Some(Color::Reset), false, true);

fn colour(color: Color) -> Look {
    (Some(color), false, false)
}

fn bold(color: Color) -> Look {
    (Some(color), false, true)
}

/// The first row of the default items with `deepseek-flash` in Always ask.
const FIRST: &str = " ◆ deepseek-flash · ◐ think high · Always ask";
/// The second row of the default items after [`Screen::usage`] with a 1M
/// window.
const USAGE: &str = " ●●●●○○○○○○ 420k/1M ( 42% of window) · ⟳ cache  92% (last  99%)";

#[test]
fn three_rows_with_usage_in_a_git_repository() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage(Some(1_000_000));
    assert_eq!(screen.bar(), [FIRST, USAGE, " ~/canter ⑂ main"]);
}

/// The rows as the user chose them (P2-245), in a wide window, with every
/// item the rows had before P2-243 shown: each in its colour, the words
/// around it grey.
#[tokio::test]
async fn a_wide_window_shows_every_item_where_it_goes() {
    let (home, dir) = repo();
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "."]);
    commit(&dir);
    std::fs::write(dir.join("a.txt"), "y\n").unwrap();
    let app = app_showing(&dir, home.path(), &["tps", "version", "session"])
        .with_welcome("0.1.0 · 88aca80")
        .with_git_diff();
    let mut screen = Screen::of(app);
    let mut settings = sandbox_settings(&dir, "allowAll", false, false);
    settings["settings"]["model"] = json!("opencode-go/deepseek-v4.1-flash");
    settings["settings"]["name"] = json!("astgrep");
    screen.event(settings);
    screen.usage_with_cache(232_000, 400_000, 980, 0.12);
    // 177 bytes, 59 tokens, over a second.
    screen.output(&[(0, 90), (1000, 87)]);
    screen.resize(120);
    screen.wait_for_last_row(" ~/canter ⑂ main · +1 −1").await;
    assert_eq!(
        screen.bar(),
        [
            " ◆ opencode-go/deepseek-v4.1-flash · ◐ think high · ▲ Allow all · ▲ Sandbox off · $0.12",
            " ●●●●●●○○○○ 232k/400k ( 58% to compact) · ⟳ cache  98% (last  99%) · ↯  59 tok/s · 0.1.0 · 88aca80 · astgrep",
            " ~/canter ⑂ main · +1 −1",
        ]
    );
    for (row, text, style) in [
        (0, "◆", colour(Color::Cyan)),
        (0, "opencode-go/deepseek-v4.1-flash", BOLD),
        (0, "◐", colour(Color::Magenta)),
        (0, "think high", PLAIN),
        (0, "▲ Allow all", colour(Color::Red)),
        (0, "▲ Sandbox off", colour(Color::Red)),
        (0, "$", colour(Color::Yellow)),
        (0, "0.12", BOLD),
        (0, " · ", GREY),
        (1, "○○○○", GREY),
        (1, "232k/400k", BOLD),
        (1, " 58%", bold(Color::Yellow)),
        (1, " to compact) · ", GREY),
        (1, "⟳", colour(Color::Cyan)),
        (1, "cache ", GREY),
        (1, " 98%", bold(Color::Cyan)),
        (1, " (last ", GREY),
        (1, " 99%", bold(Color::Cyan)),
        (1, "↯", colour(Color::Yellow)),
        (1, " 59", bold(Color::Cyan)),
        (1, " tok/s · 0.1.0 · 88aca80 · astgrep", GREY),
        (2, "~/canter", colour(Color::Blue)),
        (2, "⑂ main", colour(Color::Green)),
        (2, "+1", colour(Color::Green)),
        (2, "−1", colour(Color::Red)),
    ] {
        assert_eq!(screen.style_of(row, text), style, "{text}");
    }
    // Five cells green, the sixth, past half the room, yellow.
    let mut cells = vec![Some(Color::Green); 5];
    cells.push(Some(Color::Yellow));
    assert_eq!(screen.cells(), cells);
}

/// Too narrow for a row: its items go whole from its end, each row on its
/// own; the first keeps at least the start of its first item, the second
/// is not drawn when not even its first item fits.
#[test]
fn a_narrow_window_drops_items_whole_from_the_end_of_each_row() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AskForExecution);
    screen.event(sandbox_settings(&dir, "askForExecution", false, true));
    screen.usage_with_line(420_000, 1_000_000, Some(1_000_000));
    let first = [
        " ◆ deepseek-flash · ◐ think high · Ask for execution · Sandbox on · $0.12",
        " ◆ deepseek-flash · ◐ think high · Ask for execution · Sandbox on",
        " ◆ deepseek-flash · ◐ think high · Ask for execution",
        " ◆ deepseek-flash · ◐ think high",
        " ◆ deepseek-flash",
    ];
    for (width, row) in [
        (73, 0),
        (72, 1),
        (65, 1),
        (64, 2),
        (52, 2),
        (51, 3),
        (32, 3),
        (31, 4),
        (17, 4),
    ] {
        screen.resize(width);
        assert_eq!(screen.bar()[0], first[row], "width {width}");
    }
    screen.resize(16);
    assert_eq!(screen.bar()[0], " ◆ deepseek-fla…");

    let usage = " ●●●●○○○○○○ 420k/1M ( 42% to compact)";
    for (width, row) in [
        (64, format!("{usage} · ⟳ cache  92% (last  99%)")),
        (63, usage.to_owned()),
        (37, usage.to_owned()),
    ] {
        screen.resize(width);
        assert_eq!(screen.bar()[1], row, "width {width}");
    }
    screen.resize(36);
    assert_eq!(
        screen.bar(),
        [" ◆ deepseek-flash · ◐ think high", " ~/canter ⑂ main"]
    );
}

#[test]
fn session_cost_is_last_on_the_first_row() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    assert_eq!(screen.bar()[0], FIRST);
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
        assert_eq!(screen.bar()[0], format!("{FIRST} · {expected}"));
    }
}

#[test]
fn allow_all_is_red_and_nothing_else_is() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AllowAll);
    screen.usage(Some(1_000_000));
    let bar = screen.bar();
    assert_eq!(bar[0], " ◆ deepseek-flash · ◐ think high · ▲ Allow all");
    let top = HEIGHT as usize - bar.len();
    let buffer = screen.ui.backend().buffer();
    let start = bar[0][..bar[0].find("▲ Allow all").unwrap()].width() as u16;
    for (at, row) in bar.iter().enumerate() {
        for x in 0..row.width() as u16 {
            let style = buffer[(x, (top + at) as u16)].style();
            let in_mode = at == 0 && x >= start && x < start + "▲ Allow all".width() as u16;
            if in_mode {
                assert_eq!(style.fg, Some(Color::Red), "column {x}");
            } else {
                assert_ne!(style.fg, Some(Color::Red), "row {at} column {x}");
            }
        }
    }
}

#[test]
fn outside_a_git_repository_there_are_no_brackets() {
    let home = TempDir::new().unwrap();
    let dir = home.path().join("plain");
    std::fs::create_dir(&dir).unwrap();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage(Some(1_000_000));
    assert_eq!(screen.bar()[2], " ~/plain");
}

#[test]
fn a_detached_head_shows_the_short_commit() {
    let (home, dir) = repo();
    commit(&dir);
    git(&dir, &["checkout", "-q", "--detach"]);
    let sha = git(&dir, &["rev-parse", "HEAD"]);
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    assert_eq!(screen.bar()[1], format!(" ~/canter ⑂ {}", &sha[..7]));
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
    assert_eq!(screen.bar()[1], " ~/wt/src ⑂ feature/x");
}

/// Without usage, and with the version and session name hidden, the second
/// row has nothing to show and is not drawn.
#[test]
fn without_usage_or_window_there_is_no_context() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    assert_eq!(screen.bar(), [FIRST, " ~/canter ⑂ main"], "no usage yet");
    screen.usage(None);
    assert_eq!(
        screen.bar(),
        [FIRST, " ⟳ cache  92% (last  99%)", " ~/canter ⑂ main"],
        "window unknown"
    );
}

/// With the version shown, the second row has it before any usage.
#[test]
fn the_version_shows_on_the_second_row_before_any_usage() {
    let (home, dir) = repo();
    let app = app_showing(&dir, home.path(), &["version"]).with_welcome("0.1.0 · 88aca80");
    let mut screen = Screen::of(app);
    assert_eq!(
        screen.bar(),
        [FIRST, " 0.1.0 · 88aca80", " ~/canter ⑂ main"]
    );
}

#[test]
fn the_branch_is_read_again_when_a_turn_ends() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.turn("inProgress");
    assert_eq!(screen.last_row(), " ~/canter ⑂ main");

    git(&dir, &["symbolic-ref", "HEAD", "refs/heads/other"]);
    assert_eq!(
        screen.last_row(),
        " ~/canter ⑂ main",
        "not read again every frame"
    );
    screen.turn("completed");
    assert_eq!(screen.last_row(), " ~/canter ⑂ other");
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

    /// A response written as `(at, bytes)` chunks of text, in a turn of
    /// its own.
    fn output(&mut self, chunks: &[(i64, usize)]) {
        let turn = format!("t{}", self.seq);
        self.event(json!({ "type": "turn.started", "turn": {
            "id": turn, "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": "hi" }],
            "clientName": "canter-tui", "usage": null,
        }}));
        for &(at, bytes) in chunks {
            self.event_at(
                at,
                json!({"type": "item.delta", "turnId": turn, "itemId": "m1",
                    "delta": {"type": "agentMessage", "delta": "a".repeat(bytes)}}),
            );
        }
    }

    /// The colour of each `●` of the context bar, from the first.
    fn cells(&mut self) -> Vec<Option<Color>> {
        self.ui.draw(NOW).unwrap();
        let rows = rows(self.ui.backend().buffer());
        let y = (self.info_top(&rows) + 1) as u16;
        let buffer = self.ui.backend().buffer();
        (0..buffer.area.width)
            .filter(|&x| buffer[(x, y)].symbol() == "●")
            .map(|x| buffer[(x, y)].style().fg)
            .collect()
    }

    /// The look of `text` on info row `row`; the same in every column of it.
    fn style_of(&mut self, row: usize, text: &str) -> Look {
        self.ui.draw(NOW).unwrap();
        let rows = rows(self.ui.backend().buffer());
        let y = self.info_top(&rows) + row;
        let line = rows[y].clone();
        let at = line
            .find(text)
            .unwrap_or_else(|| panic!("{text:?} in {line:?}"));
        let start = line[..at].width() as u16;
        let buffer = self.ui.backend().buffer();
        let styles: Vec<_> = (start..start + text.width() as u16)
            .map(|x| {
                let style = buffer[(x, y as u16)].style();
                (
                    style.fg,
                    style.add_modifier.contains(Modifier::DIM),
                    style.add_modifier.contains(Modifier::BOLD),
                )
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

    /// The directory's row.
    fn last_row(&mut self) -> String {
        self.bar().pop().unwrap()
    }
}

#[test]
fn context_is_a_bar_that_is_full_at_the_compaction_line() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    // Ten cells and the percent of the line in use, both rounded; past the
    // line the bar stays full and the percent says how far past. The tokens
    // used and the percent each in four columns.
    for (used, expected) in [
        (0, "○○○○○○○○○○    0/400k (  0% to compact)"),
        (120_000, "●●●○○○○○○○ 120k/400k ( 30% to compact)"),
        (184_000, "●●●●●○○○○○ 184k/400k ( 46% to compact)"),
        (400_000, "●●●●●●●●●● 400k/400k (100% to compact)"),
        (420_000, "●●●●●●●●●● 420k/400k (105% to compact)"),
    ] {
        screen.usage_with_line(used, 1_000_000, Some(400_000));
        assert_eq!(
            screen.bar()[1],
            format!(" {expected} · ⟳ cache  92% (last  99%)"),
            "{used} used"
        );
    }
    // An older server sends no line: full is the window.
    screen.usage_with_line(420_000, 1_000_000, None);
    assert_eq!(screen.bar()[1], USAGE);
}

#[test]
fn each_item_has_its_colour_and_the_bar_turns_with_the_room_left() {
    let (home, dir) = repo();
    let mut screen = Screen::new(&dir, home.path(), PermissionMode::AlwaysAsk);
    screen.usage_with_line(400_000, 1_000_000, Some(1_000_000));
    // Marks in their colour; the model and the amount bold in the
    // terminal's own colour, the thinking level in it; no mark before a
    // mode that is not dangerous (P2-246).
    for (row, text, look) in [
        (0, "◆", colour(Color::Cyan)),
        (0, "deepseek-flash", BOLD),
        (0, "◐", colour(Color::Magenta)),
        (0, "think high", PLAIN),
        (0, " · Always ask · ", GREY),
        (0, "$", colour(Color::Yellow)),
        (0, "0.12", BOLD),
        (1, "○○○○○○", GREY),
        (1, "400k/1M", BOLD),
        (1, " 40%", bold(Color::Green)),
        (1, " to compact) · ", GREY),
        (1, "⟳", colour(Color::Cyan)),
        (1, "cache ", GREY),
        (1, " 92%", bold(Color::Cyan)),
        (1, " (last ", GREY),
        (1, " 99%", bold(Color::Cyan)),
        (2, "~/canter", colour(Color::Blue)),
        (2, "⑂ main", colour(Color::Green)),
    ] {
        assert_eq!(screen.style_of(row, text), look, "{text}");
    }
    assert!(!screen.bar()[0].contains('▲'));

    // Each cell in use by the room left where it ends: half the bar green,
    // the next three yellow, the last two red; the percent by the room
    // left before the line.
    let (green, yellow, red) = (Some(Color::Green), Some(Color::Yellow), Some(Color::Red));
    for (used, cells, percent, color) in [
        (400_000, vec![green; 4], " 40%", Color::Green),
        (
            700_000,
            [vec![green; 5], vec![yellow; 2]].concat(),
            " 70%",
            Color::Yellow,
        ),
        (
            900_000,
            [vec![green; 5], vec![yellow; 3], vec![red]].concat(),
            " 90%",
            Color::Red,
        ),
        (
            1_050_000,
            [vec![green; 5], vec![yellow; 3], vec![red; 2]].concat(),
            "105%",
            Color::Red,
        ),
    ] {
        screen.usage_with_line(used, 1_000_000, Some(1_000_000));
        assert_eq!(screen.cells(), cells, "{used}");
        assert_eq!(screen.style_of(1, percent), bold(color), "{used}");
        if used < 1_000_000 {
            assert_eq!(screen.style_of(1, "○"), GREY, "{used}");
        }
    }
    // Without a line, by the room left in the window.
    screen.usage_with_line(420_000, 450_000, None);
    assert_eq!(
        screen.cells(),
        [vec![green; 5], vec![yellow; 3], vec![red]].concat()
    );
    assert_eq!(screen.style_of(1, " 93%"), bold(Color::Red));
    screen.usage_with_line(420_000, 1_000_000, None);
    assert_eq!(screen.cells(), vec![green; 4]);
    assert_eq!(screen.style_of(1, " 42%"), bold(Color::Green));

    // NO_COLOR: grey as before, the numbers bold; the cells still tell how
    // much.
    theme::set(Theme {
        depth: Depth::None,
        ..Theme::default()
    });
    screen.usage_with_line(900_000, 1_000_000, Some(1_000_000));
    assert_eq!(
        screen.bar()[1],
        " ●●●●●●●●●○ 900k/1M ( 90% to compact) · ⟳ cache  92% (last  99%)"
    );
    for (row, text, look) in [
        (0, "◆", GREY),
        (0, "deepseek-flash", BOLD),
        (0, "◐", GREY),
        (0, "think high", PLAIN),
        (0, "$", GREY),
        (0, "0.12", BOLD),
        (1, "●●●●●●●●●○", GREY),
        (1, " 90%", BOLD),
        (1, "⟳", GREY),
        (1, " 92%", BOLD),
        (2, "~/canter", GREY),
        (2, "⑂ main", GREY),
    ] {
        assert_eq!(screen.style_of(row, text), look, "{text}");
    }
    theme::set(Theme::default());
}

impl Screen {
    /// Draws until the directory's row is `expected`, letting git run
    /// meanwhile.
    async fn wait_for_last_row(&mut self, expected: &str) {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if self.last_row() == expected {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "{:?}",
                self.last_row()
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
    screen.wait_for_last_row(" ~/canter ⑂ main").await;

    // Not staged: one line changed and two added. Staged: one line changed.
    std::fs::write(dir.join("a.txt"), "1\nTWO\n3\n4\n5\n6\n7\n").unwrap();
    std::fs::write(dir.join("b.txt"), "x\nY\nz\n").unwrap();
    git(&dir, &["add", "b.txt"]);
    // Neither a binary file nor an untracked one counts lines.
    std::fs::write(dir.join("image.bin"), b"\0\x03\x04other\0\n\n").unwrap();
    std::fs::write(dir.join("new.txt"), "a\nb\nc\n").unwrap();
    screen.turn("completed");
    screen.wait_for_last_row(" ~/canter ⑂ main · +4 −2").await;

    let row = screen.bar().len() - 1;
    assert_eq!(screen.style_of(row, "+4"), colour(Color::Green));
    assert_eq!(screen.style_of(row, "−2"), colour(Color::Red));
}

#[tokio::test]
async fn before_the_first_commit_the_staged_lines_count() {
    let (home, dir) = repo();
    std::fs::write(dir.join("a.txt"), "1\n2\n3\n").unwrap();
    git(&dir, &["add", "a.txt"]);
    std::fs::write(dir.join("a.txt"), "1\n2\n3\n4\n").unwrap();
    let mut screen = Screen::with_git_diff(&dir, home.path());
    screen.wait_for_last_row(" ~/canter ⑂ main · +3 −0").await;
    // The count that is 0 grey, the other in its colour (P2-246).
    let row = screen.bar().len() - 1;
    assert_eq!(screen.style_of(row, "+3"), colour(Color::Green));
    assert_eq!(screen.style_of(row, "−0"), GREY);
}

/// Numbers on the second row that change their number of digits move
/// nothing after them on it (P2-245), and nothing on the other rows.
#[tokio::test]
async fn numbers_changing_width_move_nothing_after_them() {
    let (home, dir) = repo();
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    git(&dir, &["add", "."]);
    commit(&dir);
    std::fs::write(dir.join("a.txt"), "y\n").unwrap();
    let app = app_showing(&dir, home.path(), &["tps", "version"])
        .with_welcome("0.1.0 · 88aca80")
        .with_git_diff();
    let mut screen = Screen::of(app);
    screen.resize(120);
    screen.usage_with_cache(36_000, 400_000, 90, 0.12);
    screen.wait_for_last_row(" ~/canter ⑂ main · +1 −1").await;
    let column = |row: &str, text: &str| row[..row.find(text).unwrap()].width();
    let mut seen: Option<(Vec<String>, [usize; 3])> = None;
    // Context 36k (9%), 232k (58%), 400k (100%), 420k (105%); cache 9%,
    // 98%, 100%; speed 9, 59, 159 tokens a second.
    for (at, (used, cached, tokens)) in [
        (36_000, 90, 9),
        (232_000, 980, 59),
        (400_000, 1000, 159),
        (420_000, 980, 9),
    ]
    .into_iter()
    .enumerate()
    {
        let start = 10_000 * at as i64;
        screen.usage_with_cache(used, 400_000, cached, 0.12);
        screen.output(&[(start, 3), (start + 1000, 3 * tokens - 3)]);
        let bar = screen.bar_at(start + 1000);
        let columns = [
            column(&bar[1], "· ⟳ cache"),
            column(&bar[1], "tok/s"),
            column(&bar[1], "0.1.0"),
        ];
        assert!(bar[1].contains(&format!("{tokens:>3} tok/s")), "{bar:?}");
        let (first, columns_first) = seen.get_or_insert((bar.clone(), columns));
        assert_eq!(columns, *columns_first, "{bar:?}");
        assert_eq!(bar[0], first[0]);
        assert_eq!(bar[2], " ~/canter ⑂ main · +1 −1");
    }
}

/// The speed is on the second row when `/statusline` shows it, while the
/// model writes and after, in whole tokens a second; not on the line
/// saying what the turn does.
#[test]
fn the_speed_is_on_the_second_row_not_the_working_line() {
    let (home, dir) = repo();
    let mut screen = Screen::of(app_showing(&dir, home.path(), &["tps"]));
    screen.turn("inProgress");
    screen.event(json!({"type": "session.status", "status": "working"}));
    screen.event(json!({"type": "item.started", "turnId": "t1",
        "item": {"type": "agentMessage", "id": "m1", "text": ""}}));
    screen.event_at(
        0,
        json!({"type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": {"type": "agentMessage", "delta": "a".repeat(90)}}),
    );
    screen.event_at(
        1000,
        json!({"type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": {"type": "agentMessage", "delta": "a".repeat(87)}}),
    );
    let rows = screen.screen_at(1000);
    let working = rows
        .iter()
        .find(|row| row.contains("Esc to interrupt"))
        .unwrap_or_else(|| panic!("{rows:#?}"));
    assert!(!working.contains("tok/s"), "{working:?}");
    assert_eq!(screen.bar_at(1000)[1], " ↯  59 tok/s");
    // The model waits: the speed stays.
    assert_eq!(screen.bar_at(9000)[1], " ↯  59 tok/s");
    // Done, it stays at the last response's.
    screen.event_at(
        9000,
        json!({"type": "item.completed", "turnId": "t1",
            "item": {"type": "agentMessage", "id": "m1", "text": "a".repeat(177)}}),
    );
    screen.event_at(9000, json!({"type": "session.status", "status": "idle"}));
    assert!(screen.bar_at(20_000)[1].ends_with(" tok/s"));
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
    screen.event_at(
        1000,
        json!({"type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": {"type": "agentMessage", "delta": "a".repeat(177)}}),
    );
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
        " ◆ deepseek-flash · ◐ think high · Plan (then Always ask)"
    );
    assert_eq!(screen.style_of(0, "Plan"), colour(Color::Magenta));
    assert_eq!(screen.style_of(0, " (then "), GREY);
    assert_eq!(screen.style_of(0, "Always ask"), GREY);
    assert_eq!(screen.style_of(0, ")"), GREY);

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
        " ◆ deepseek-flash · ◐ think high · Plan (then Allow all)"
    );
    assert_eq!(screen.style_of(0, "Plan"), colour(Color::Magenta));
    assert_eq!(screen.style_of(0, "Allow all"), colour(Color::Red));
    assert_eq!(screen.style_of(0, ")"), GREY);

    // Out of Plan, the mode alone as before.
    screen.event(settings("allowAll", false));
    assert_eq!(
        screen.bar()[0],
        " ◆ deepseek-flash · ◐ think high · ▲ Allow all"
    );
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
    assert_eq!(
        screen.bar()[0],
        " ◆ deepseek-flash · ◐ think high · Ask for execution"
    );
    screen.event(sandbox_settings(&dir, "askForExecution", false, true));
    assert_eq!(
        screen.bar()[0],
        " ◆ deepseek-flash · ◐ think high · Ask for execution · Sandbox on"
    );
    assert_eq!(screen.style_of(0, "Sandbox on"), GREY);
    screen.event(sandbox_settings(&dir, "allowAll", false, false));
    assert_eq!(
        screen.bar()[0],
        " ◆ deepseek-flash · ◐ think high · ▲ Allow all · ▲ Sandbox off"
    );
    assert_eq!(screen.style_of(0, "▲ Sandbox off"), colour(Color::Red));
    assert_eq!(screen.style_of(0, "▲ Allow all"), colour(Color::Red));
    screen.event(sandbox_settings(&dir, "alwaysAsk", true, true));
    assert_eq!(
        screen.bar()[0],
        " ◆ deepseek-flash · ◐ think high · Plan (then Always ask) · Sandbox on"
    );
    // The cost after it.
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
        " ◆ deepseek-flash · ◐ think high · Always ask · Sandbox on · $0.31"
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
    let full = " ◆ deepseek-flash · ◐ think high · Ask for execution · Sandbox on";
    let mode = " ◆ deepseek-flash · ◐ think high · Ask for execution";
    screen.resize(full.width() as u16);
    assert_eq!(screen.bar()[0], full);
    for width in [full.width() as u16 - 1, mode.width() as u16] {
        screen.resize(width);
        assert_eq!(screen.bar()[0], mode, "width {width}");
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
    let full = format!("{path} ⑂ main · +1 −1");
    screen.resize(full.width() as u16);
    screen.wait_for_last_row(&full).await;
    // One column less: the lines changed go; the directory stays whole.
    screen.resize(full.width() as u16 - 1);
    assert_eq!(screen.last_row(), format!("{path} ⑂ main"));
    let exact = format!("{path} ⑂ main").width() as u16;
    screen.resize(exact);
    assert_eq!(screen.last_row(), format!("{path} ⑂ main"));
    // Then a directory at a time.
    screen.resize(exact - 1);
    assert_eq!(
        screen.last_row(),
        format!(" ~/…/{}/src/canter ⑂ main", "a".repeat(30))
    );
    screen.resize(20);
    assert_eq!(screen.last_row(), " ~/…/canter ⑂ main");
    // Not even the last part leaves room: the row is cut at its end.
    screen.resize(12);
    let last = screen.last_row();
    assert!(
        last.starts_with(" …/canter") && last.ends_with('…'),
        "{last:?}"
    );
    assert_eq!(last.width(), 12);
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
            " ◆ deepseek-flash",
            " ●●●●○○○○○○ 420k/1M ( 42% of window)",
            " ~/canter ⑂ main",
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
        assert_eq!(screen.bar()[0], format!("{FIRST} · {expected}"));
    }
}
