//! `/statusline`: which items the status bar shows, in which order and on
//! which row, saved in `tui.json`. Homes, `CANTER_HOME` and git repositories
//! are temporary directories, never the user's.

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use canter_protocol::{Event, PermissionMode, SessionSnapshot, SessionStatus, SessionUpdateParams};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use tempfile::TempDir;
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const WIDTH: u16 = 80;
const HEIGHT: u16 = 30;
const TITLE: &str = " Configure status line";

/// A temporary home with `CANTER_HOME` in it (or not), and the session's
/// directory `~/src/canter`, a git repository on `main`.
struct Home {
    root: TempDir,
    env: BTreeMap<String, String>,
    cwd: PathBuf,
    model: &'static str,
}

impl Home {
    fn new() -> Self {
        let home = Self::without_canter_home();
        let canter = home.root.path().join("canter-home");
        std::fs::create_dir(&canter).unwrap();
        let mut env = home.env.clone();
        env.insert("CANTER_HOME".into(), canter.to_str().unwrap().into());
        Self { env, ..home }
    }

    /// Only `HOME`: settings go in `~/.canter`.
    fn without_canter_home() -> Self {
        let root = TempDir::new().unwrap();
        let cwd = root.path().join("src").join("canter");
        std::fs::create_dir_all(&cwd).unwrap();
        git(&cwd, &["init", "-q", "-b", "main"]);
        let env = BTreeMap::from([("HOME".into(), root.path().to_str().unwrap().into())]);
        Self {
            root,
            env,
            cwd,
            model: "test-model",
        }
    }

    fn tui_json(&self) -> PathBuf {
        match self.env.get("CANTER_HOME") {
            Some(dir) => Path::new(dir).join("tui.json"),
            None => self.root.path().join(".canter").join("tui.json"),
        }
    }

    /// A terminal opened on the session, with the status command `script`
    /// as `config.json` would give it.
    fn open(&self, script: Option<&str>) -> Screen {
        let mut snapshot = SessionSnapshot::empty(SESSION);
        snapshot.settings.cwd = self.cwd.to_str().unwrap().to_owned();
        snapshot.settings.model = self.model.to_owned();
        snapshot.settings.name = Some("会话名字".into());
        snapshot.status = SessionStatus::Idle;
        let app = App::new(snapshot, Some(self.root.path().to_str().unwrap().into()), 0)
            .with_welcome("0.1.0 · a3b8fe7")
            .with_status_line(script.map(str::to_owned), self.env.clone());
        let mut screen = Screen {
            ui: Ui::new(TestBackend::new(WIDTH, HEIGHT), app).unwrap(),
            seq: 0,
            now: 0,
        };
        screen.draw();
        screen
    }
}

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    now: i64,
}

impl Screen {
    fn event(&mut self, at: i64, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": at });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, at);
    }

    /// One answer at 32.5 tokens a second; then cost, context and cache
    /// hits as in the picture the user saw.
    fn work(&mut self) {
        let turn = |status: &str| {
            json!({
                "id": "t1", "items": [], "status": status, "error": null,
                "input": [{ "type": "text", "text": "hi" }],
                "clientName": "canter-tui", "usage": null,
            })
        };
        let message = json!({ "type": "agentMessage", "id": "m1", "text": "abc" });
        self.event(
            0,
            json!({ "type": "turn.started", "turn": turn("inProgress") }),
        );
        self.event(
            0,
            json!({ "type": "item.started", "turnId": "t1",
                    "item": { "type": "agentMessage", "id": "m1", "text": "" } }),
        );
        self.event(
            1000,
            json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
                    "delta": { "type": "agentMessage", "delta": "abc" } }),
        );
        let counts = |input: u64, cached: u64| {
            json!({ "inputTokens": input, "cachedInputTokens": cached, "outputTokens": 65,
                    "reasoningOutputTokens": 0, "totalTokens": input + 65 })
        };
        self.event(
            3000,
            json!({ "type": "session.usage", "tokenUsage": {
                "total": counts(1000, 950), "last": counts(100, 98),
                "contextTokens": 120_000, "modelContextWindow": 1_000_000,
                "cost": { "usd": 0.12, "unknown": false },
            }}),
        );
        self.event(
            3000,
            json!({ "type": "item.completed", "turnId": "t1", "item": message }),
        );
        self.event(
            3000,
            json!({ "type": "turn.completed", "turn": turn("completed") }),
        );
        self.now = 10_000;
        self.draw();
    }

    fn draw(&mut self) {
        self.ui.draw(self.now).unwrap();
    }

    fn resize(&mut self, height: u16) {
        self.ui.backend_mut().resize(WIDTH, height);
        self.draw();
    }

    fn press(&mut self, key: KeyEvent) -> Option<Action> {
        let action = self.ui.app_mut().handle_key(key, self.now);
        self.draw();
        action
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.press(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn keys(&mut self, codes: &[KeyCode]) {
        for &code in codes {
            self.key(code);
        }
    }

    /// Types `text` in the input box and presses Enter.
    fn enter(&mut self, text: &str) -> Option<Action> {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
        self.key(KeyCode::Enter)
    }

    fn rows(&self) -> Vec<String> {
        let buffer = self.ui.backend().buffer();
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

    fn box_top(&self) -> usize {
        self.rows()
            .iter()
            .rposition(|row| row.starts_with('╭'))
            .expect("input box")
    }

    /// The two rows at the bottom.
    fn bar(&self) -> [String; 2] {
        let rows = self.rows();
        let n = rows.len();
        [rows[n - 2].clone(), rows[n - 1].clone()]
    }

    /// The rows of the picker above the input box; empty while it is closed.
    fn picker(&self) -> Vec<String> {
        let rows = self.rows();
        match rows.iter().position(|row| row == TITLE) {
            Some(start) => rows[start..self.box_top()].to_vec(),
            None => Vec::new(),
        }
    }

    /// The status line, right above the picker or the input box.
    fn status(&self) -> String {
        let rows = self.rows();
        let below = rows
            .iter()
            .position(|row| row == TITLE)
            .unwrap_or_else(|| self.box_top());
        rows[below - 1].clone()
    }

    fn preview(&self) -> [String; 2] {
        let picker = self.picker();
        let at = picker.iter().position(|row| row == " Preview").unwrap();
        [picker[at + 1].clone(), picker[at + 2].clone()]
    }

    async fn wait_for(&mut self, first: &str) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            self.draw();
            if self.bar()[0] == first {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "bar: {:?}",
                self.bar()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

/// Runs git in `dir` without the user's or the system's git config.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

const DEFAULT_BAR: [&str; 2] = [
    " deepseek-v4-pro · think high · Always ask · $0.12",
    " ~/src/canter (main) · █░░░░░░░░░ 120k/1M (12%) · cache 95% (last 98%)",
];

#[test]
fn statusline_shows_every_item_with_its_value_and_a_preview() {
    let home = Home {
        model: "deepseek-v4-pro",
        ..Home::new()
    };
    let mut screen = home.open(None);
    screen.work();
    assert_eq!(screen.bar(), DEFAULT_BAR);
    assert_eq!(screen.enter("/statusline"), None);
    assert_eq!(
        screen.picker(),
        [
            TITLE,
            " Space toggle · ←/→ move · Enter save · Esc cancel",
            "",
            " › [x] model        deepseek-v4-pro",
            "   [x] thinking     think high",
            "   [x] permission   Always ask",
            "   [x] cost         $0.12",
            "   ─── line 2 ───",
            "   [x] directory    ~/src/canter",
            "   [x] branch       (main)",
            "   [x] diff",
            "   [x] context      █░░░░░░░░░ 120k/1M (12%)",
            "   [x] cache        cache 95% (last 98%)",
            "   [ ] tps          32.5 tok/s",
            "   [ ] version      0.1.0 · a3b8fe7",
            "   [ ] session      会话名字",
            "",
            " Preview",
            "  deepseek-v4-pro · think high · Always ask · $0.12",
            "  ~/src/canter (main) · █░░░░░░░░░ 120k/1M (12%) · cache 95% (last 98%)",
        ]
    );
    // Nothing is saved or changed yet.
    assert_eq!(screen.bar(), DEFAULT_BAR);
    assert!(!home.tui_json().exists());
}

#[test]
fn space_and_arrows_change_the_preview_as_they_go() {
    let home = Home {
        model: "deepseek-v4-pro",
        ..Home::new()
    };
    let mut screen = home.open(None);
    screen.work();
    screen.enter("/statusline");

    screen.key(KeyCode::Char(' '));
    assert_eq!(screen.picker()[3], " › [ ] model        deepseek-v4-pro");
    assert_eq!(
        screen.preview(),
        [
            "  think high · Always ask · $0.12",
            "  ~/src/canter (main) · █░░░░░░░░░ 120k/1M (12%) · cache 95% (last 98%)",
        ]
    );

    // Back on, then one place later.
    screen.keys(&[KeyCode::Char(' '), KeyCode::Right]);
    assert_eq!(screen.picker()[3], "   [x] thinking     think high");
    assert_eq!(screen.picker()[4], " › [x] model        deepseek-v4-pro");
    assert_eq!(
        screen.preview()[0],
        "  think high · deepseek-v4-pro · Always ask · $0.12"
    );

    // The line 2 row moves like an item: one place up puts cost on line 2.
    screen.keys(&[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Left]);
    assert_eq!(screen.picker()[6], " › ─── line 2 ───");
    assert_eq!(
        screen.preview(),
        [
            "  think high · deepseek-v4-pro · Always ask",
            "  $0.12 · ~/src/canter (main) · █░░░░░░░░░ 120k/1M (12%) · cache 95% (last 98%)",
        ]
    );

    // tps checked, and moved up to after the branch.
    screen.keys(&[KeyCode::Down; 7]);
    assert_eq!(screen.picker()[13], " › [ ] tps          32.5 tok/s");
    screen.keys(&[
        KeyCode::Char(' '),
        KeyCode::Left,
        KeyCode::Left,
        KeyCode::Left,
    ]);
    assert_eq!(screen.picker()[10], " › [x] tps          32.5 tok/s");
    let second = &screen.preview()[1];
    assert!(
        second.starts_with("  $0.12 · ~/src/canter (main) · 32.5 tok/s · █░░░░░░░░░ 120k/1M (12%)"),
        "{second}"
    );

    // At the end, the line leaves line 2 empty.
    screen.keys(&[KeyCode::Up; 4]);
    assert_eq!(screen.picker()[6], " › ─── line 2 ───");
    screen.keys(&[KeyCode::Right; 11]);
    assert_eq!(screen.picker()[15], " › ─── line 2 ───");
    let [first, second] = screen.preview();
    assert!(
        first.starts_with(
            "  think high · deepseek-v4-pro · Always ask · $0.12 · ~/src/canter (main)"
        ),
        "{first}"
    );
    assert_eq!(second, "");

    // None of it is saved or shown below yet.
    assert_eq!(screen.bar(), DEFAULT_BAR);
}

#[test]
fn enter_saves_tui_json_0600_and_the_bar_changes_at_once() {
    for home in [Home::new(), Home::without_canter_home()] {
        let config = home.tui_json().with_file_name("config.json");
        let mut screen = home.open(None);
        assert_eq!(
            screen.bar(),
            [
                " test-model · think high · Always ask",
                " ~/src/canter (main)",
            ]
        );
        screen.enter("/statusline");
        // Model off; version on, moved to the top of line 1.
        screen.key(KeyCode::Char(' '));
        screen.keys(&[KeyCode::Down; 11]);
        screen.key(KeyCode::Char(' '));
        screen.keys(&[KeyCode::Left; 11]);
        assert_eq!(screen.key(KeyCode::Enter), None);

        assert!(screen.picker().is_empty(), "closed");
        assert_eq!(
            screen.bar(),
            [
                " 0.1.0 · a3b8fe7 · think high · Always ask",
                " ~/src/canter (main)",
            ]
        );
        let path = home.tui_json();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", path.display());
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            saved.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["statusLine"]
        );
        // No half-written file left beside it, and config.json untouched.
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(names, ["tui.json"]);
        assert!(!config.exists());
    }
}

#[test]
fn a_terminal_opened_later_reads_the_same_settings_back() {
    let home = Home::new();
    let mut screen = home.open(None);
    screen.enter("/statusline");
    // Cost to line 2 and tps hidden but moved; session on after the line.
    screen.keys(&[KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Right]);
    screen.keys(&[KeyCode::Down; 8]);
    screen.keys(&[KeyCode::Char(' '), KeyCode::Up, KeyCode::Up, KeyCode::Left]);
    screen.key(KeyCode::Enter);
    let bar = screen.bar();
    assert_eq!(
        bar,
        [
            " test-model · think high · Always ask",
            " ~/src/canter (main) · 会话名字",
        ]
    );
    screen.enter("/statusline");
    let picker = screen.picker();
    assert_eq!(picker[7], "   [x] cost");
    assert_eq!(picker[12], "   [ ] tps");
    assert_eq!(picker[15], "   [x] session      会话名字");

    let mut again = home.open(None);
    assert_eq!(again.bar(), bar);
    again.enter("/statusline");
    assert_eq!(again.picker(), picker);
}

#[test]
fn esc_closes_without_saving() {
    let home = Home::new();
    let mut screen = home.open(None);
    let bar = screen.bar();
    screen.enter("/statusline");
    screen.keys(&[KeyCode::Char(' '), KeyCode::Right]);
    assert_eq!(screen.key(KeyCode::Esc), None);
    assert!(screen.picker().is_empty(), "closed");
    assert_eq!(screen.bar(), bar);
    assert!(!home.tui_json().exists());
    screen.enter("/statusline");
    assert_eq!(screen.picker()[3], " › [x] model        test-model");
}

#[test]
fn an_unreadable_tui_json_means_the_defaults_and_unknown_items_are_skipped() {
    let default = [
        " test-model · think high · Always ask",
        " ~/src/canter (main)",
    ];
    let unreadable: [&dyn Fn(&Path); 3] = [
        &|path| std::fs::write(path, "{").unwrap(),
        &|path| std::fs::write(path, r#"{"statusLine":{"items":"model"}}"#).unwrap(),
        &|path| std::fs::create_dir(path).unwrap(),
    ];
    for write in unreadable {
        let home = Home::new();
        write(&home.tui_json());
        let mut screen = home.open(None);
        assert_eq!(screen.bar(), default);
        assert!(screen.status().contains("tui.json"), "{}", screen.status());
        // Said once, for a moment.
        screen.now = 10_000;
        screen.draw();
        assert!(!screen.status().contains("tui.json"), "{}", screen.status());
        screen.enter("/statusline");
        assert_eq!(screen.picker()[3], " › [x] model        test-model");
    }

    let home = Home::new();
    std::fs::write(
        home.tui_json(),
        json!({ "statusLine": { "items": [
            { "name": "bogus", "show": true }, 42, { "name": "model", "show": true },
            { "name": "line2" }, { "name": "directory", "show": true },
        ]}})
        .to_string(),
    )
    .unwrap();
    let screen = home.open(None);
    assert_eq!(screen.bar(), [" test-model", " ~/src/canter"]);
    assert!(!screen.status().contains("tui.json"), "{}", screen.status());
}

#[tokio::test]
async fn with_a_script_a_switch_on_top_chooses_the_script_or_the_items() {
    let home = Home::new();
    let script = "cat >/dev/null; echo run >> runs.txt; printf custom";
    std::fs::write(home.cwd.join("status.sh"), script).unwrap();
    let config = home.tui_json().with_file_name("config.json");
    let config_text = r#"{"statusLine":"sh ./status.sh","model":"keep"}"#;
    std::fs::write(&config, config_text).unwrap();
    let runs = || {
        std::fs::read_to_string(home.cwd.join("runs.txt")).map_or(0, |text| text.lines().count())
    };

    let mut screen = home.open(Some("sh ./status.sh"));
    screen.wait_for("custom").await;
    screen.enter("/statusline");
    let picker = screen.picker();
    assert_eq!(picker[3], " › [x] Use statusLine script");
    assert!(picker[4].starts_with("   ───"), "{picker:?}");
    assert_eq!(picker[5], "   [x] model        test-model");
    assert!(
        screen.preview()[0].contains("statusLine script"),
        "{picker:?}"
    );

    // Off: the items below show, and the script stops running.
    screen.key(KeyCode::Char(' '));
    assert_eq!(screen.picker()[3], " › [ ] Use statusLine script");
    assert_eq!(
        screen.preview()[0],
        "  test-model · think high · Always ask"
    );
    screen.key(KeyCode::Enter);
    assert_eq!(screen.bar()[0], " test-model · think high · Always ask");
    let ran = runs();
    for status in ["working", "idle"].repeat(20) {
        // Changes the script would run for.
        screen.event(0, json!({ "type": "session.status", "status": status }));
        screen.draw();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(screen.bar()[0], " test-model · think high · Always ask");
    assert_eq!(runs(), ran, "the script does not run while off");

    // A terminal opened later keeps it off.
    let mut again = home.open(Some("sh ./status.sh"));
    for _ in 0..40 {
        again.draw();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(again.bar()[0], " test-model · think high · Always ask");
    assert_eq!(runs(), ran);

    // On again: back to the script.
    again.enter("/statusline");
    assert_eq!(again.picker()[3], " › [ ] Use statusLine script");
    again.keys(&[KeyCode::Char(' '), KeyCode::Enter]);
    again.wait_for("custom").await;
    assert_eq!(std::fs::read_to_string(&config).unwrap(), config_text);
}

#[test]
fn with_permission_hidden_shift_tab_says_the_new_mode() {
    let home = Home::new();
    let mut screen = home.open(None);
    screen.enter("/statusline");
    screen.keys(&[
        KeyCode::Down,
        KeyCode::Down,
        KeyCode::Char(' '),
        KeyCode::Enter,
    ]);
    assert_eq!(screen.bar()[0], " test-model · think high");
    let before = screen.status();
    assert_eq!(
        screen.press(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
        Some(Action::Update(SessionUpdateParams {
            approval_policy: Some(PermissionMode::AskForExecution),
            ..Default::default()
        }))
    );
    assert_eq!(screen.status(), " Mode: Ask for execution");
    screen.now += 2000;
    screen.draw();
    assert_eq!(screen.status(), before);
}

#[test]
fn in_a_short_window_the_list_follows_the_cursor_and_the_preview_stays() {
    let home = Home::new();
    let mut screen = home.open(None);
    // 12 rows above the input box: 5 for the list.
    screen.resize(18);
    screen.enter("/statusline");
    let preview = [
        "",
        " Preview",
        "  test-model · think high · Always ask",
        "  ~/src/canter (main)",
    ];
    let shown = |list: &[&str]| -> Vec<String> {
        let head = [
            TITLE,
            " Space toggle · ←/→ move · Enter save · Esc cancel",
            "",
        ];
        head.iter()
            .chain(list)
            .chain(&preview)
            .map(|row| row.to_string())
            .collect()
    };
    assert_eq!(
        screen.picker(),
        shown(&[
            " › [x] model        test-model",
            "   [x] thinking     think high",
            "   [x] permission   Always ask",
            "   [x] cost",
            "   ↓ 9 more",
        ])
    );

    screen.keys(&[KeyCode::Down; 4]);
    assert_eq!(
        screen.picker(),
        shown(&[
            "   ↑ 2 more",
            "   [x] permission   Always ask",
            "   [x] cost",
            " › ─── line 2 ───",
            "   ↓ 8 more",
        ])
    );

    // Moving the line along keeps it in view; the preview follows.
    screen.keys(&[KeyCode::Right; 2]);
    let picker = screen.picker();
    assert_eq!(
        picker[3..8],
        [
            "   ↑ 4 more",
            "   [x] directory    ~/src/canter",
            "   [x] branch       (main)",
            " › ─── line 2 ───",
            "   ↓ 6 more",
        ]
    );
    assert_eq!(
        picker[8..],
        [
            "",
            " Preview",
            "  test-model · think high · Always ask · ~/src/canter (main)",
            "",
        ]
    );
    screen.keys(&[KeyCode::Left; 2]);

    screen.keys(&[KeyCode::Down; 10]);
    assert_eq!(
        screen.picker(),
        shown(&[
            "   ↑ 9 more",
            "   [x] cache",
            "   [ ] tps",
            "   [ ] version      0.1.0 · a3b8fe7",
            " › [ ] session      会话名字",
        ])
    );

    // Fewer than three rows left for the list: the preview folds away.
    screen.resize(15);
    assert_eq!(
        screen.picker(),
        [
            TITLE,
            " Space toggle · ←/→ move · Enter save · Esc cancel",
            "",
            "   ↑ 8 more",
            "   [x] context",
            "   [x] cache",
            "   [ ] tps",
            "   [ ] version      0.1.0 · a3b8fe7",
            " › [ ] session      会话名字",
        ]
    );
}

#[test]
fn a_tui_json_saved_with_compact_and_before_diff_gets_diff_after_the_branch_until_turned_off() {
    let home = Home::new();
    // As P2-113 saved it: `compact` after `context`, no `diff` yet.
    let saved = |branch: bool| {
        json!({ "statusLine": { "useScript": true, "items": [
            { "name": "model", "show": true }, { "name": "thinking", "show": true },
            { "name": "permission", "show": true }, { "name": "cost", "show": true },
            { "name": "line2" }, { "name": "directory", "show": true },
            { "name": "branch", "show": branch }, { "name": "context", "show": true },
            { "name": "compact", "show": true },
            { "name": "cache", "show": true }, { "name": "tps", "show": false },
            { "name": "version", "show": false }, { "name": "session", "show": false },
        ]}})
        .to_string()
    };
    for branch in [true, false] {
        std::fs::write(home.tui_json(), saved(branch)).unwrap();
        let mut screen = home.open(None);
        assert!(!screen.status().contains("tui.json"), "{}", screen.status());
        screen.work();
        screen.event(
            3000,
            json!({ "type": "session.usage", "tokenUsage": {
                "total": {"inputTokens":1000,"cachedInputTokens":950,"outputTokens":65,
                    "reasoningOutputTokens":0,"totalTokens":1065},
                "last": {"inputTokens":100,"cachedInputTokens":98,"outputTokens":65,
                    "reasoningOutputTokens":0,"totalTokens":165},
                "contextTokens": 120_000, "modelContextWindow": 1_000_000,
                "autoCompactTokens": 400_000,
                "cost": { "usd": 0.12, "unknown": false },
            }}),
        );
        screen.draw();
        let shown = if branch { " (main)" } else { "" };
        assert_eq!(
            screen.bar()[1],
            format!(" ~/src/canter{shown} · ███░░░░░░░ 120k/400k (30%) · cache 95% (last 98%)")
        );
        screen.enter("/statusline");
        let mark = if branch { 'x' } else { ' ' };
        let picker = screen.picker();
        assert_eq!(
            picker[9..13],
            [
                format!("   [{mark}] branch       (main)"),
                "   [x] diff".to_owned(),
                "   [x] context      ███░░░░░░░ 120k/400k (30%)".to_owned(),
                "   [x] cache        cache 95% (last 98%)".to_owned(),
            ]
        );
        assert!(
            !picker.iter().any(|row| row.contains("compact")),
            "{picker:#?}"
        );
    }

    // Turned off and saved, it stays off.
    let mut screen = home.open(None);
    screen.enter("/statusline");
    screen.keys(&[KeyCode::Down; 7]);
    assert_eq!(screen.picker()[10], " › [x] diff");
    screen.keys(&[KeyCode::Char(' '), KeyCode::Enter]);
    let mut again = home.open(None);
    again.enter("/statusline");
    assert_eq!(again.picker()[10], "   [ ] diff");
}

#[test]
fn hidden_items_keep_their_colours_in_the_list() {
    let home = Home::new();
    let mut screen = home.open(None);
    screen.work();
    screen.enter("/statusline");
    let rows = screen.rows();
    let buffer = screen.ui.backend().buffer();
    // `text` in the list row of item `name`.
    let style = |name: &str, text: &str| {
        let label = format!("[ ] {name}");
        let y = rows.iter().position(|row| row.contains(&label)).unwrap();
        let at = rows[y].rfind(text).unwrap();
        let x = rows[y][..at].width() as u16;
        let style = buffer[(x, y as u16)].style();
        (style.fg, style.add_modifier.contains(Modifier::DIM))
    };
    assert_eq!(style("tps", "32.5"), (Some(Color::Cyan), false));
    assert_eq!(style("tps", " tok/s"), (Some(Color::Reset), true));
    assert_eq!(
        style("version", "0.1.0 · a3b8fe7"),
        (Some(Color::Reset), true)
    );
    assert_eq!(style("session", "会话名字"), (Some(Color::Reset), true));
}
