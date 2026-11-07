//! `/statusline`: which items the status bar shows, saved in `tui.json`;
//! where each goes is fixed (P2-245). Homes, `CANTER_HOME` and git repositories
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
const KEYS: &str = " Space toggle · Enter save · Esc cancel";

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

    /// The info rows at the bottom, under the input box or the picker in
    /// its place.
    fn bar(&self) -> Vec<String> {
        let rows = self.rows();
        let bottom = rows.iter().rposition(|row| row.starts_with('╰')).unwrap();
        rows[bottom + 1..].to_vec()
    }

    /// The rows of the picker in the input box's place, title to keys; empty
    /// while it is closed.
    fn picker(&self) -> Vec<String> {
        let picker = panel(&self.rows());
        if picker.first().is_some_and(|title| title == TITLE) {
            picker
        } else {
            Vec::new()
        }
    }

    /// The status line, right above the picker or the input box.
    fn status(&self) -> String {
        self.rows()[self.box_top() - 1].clone()
    }

    /// The rows under ` Preview`, down to the keys.
    fn preview(&self) -> Vec<String> {
        let picker = self.picker();
        let at = picker.iter().position(|row| row == " Preview").unwrap();
        picker[at + 1..picker.len() - 1].to_vec()
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

/// The first row after [`Screen::work`].
const FIRST: &str = " ◆ deepseek-v4-pro · ◐ think high · Always ask · $0.12";
/// The second row after [`Screen::work`].
const USAGE: &str = " ●○○○○○○○○○ 120k/1M ( 12% of window) · ⟳ cache  95% (last  98%)";
const DIRECTORY: &str = " ~/src/canter ⑂ main";

fn default_bar() -> [&'static str; 3] {
    [FIRST, USAGE, DIRECTORY]
}

#[test]
fn statusline_shows_every_item_with_its_value_and_a_preview() {
    let home = Home {
        model: "deepseek-v4-pro",
        ..Home::new()
    };
    let mut screen = home.open(None);
    screen.work();
    assert_eq!(screen.bar(), default_bar());
    assert_eq!(screen.enter("/statusline"), None);
    assert_eq!(
        screen.picker(),
        [
            TITLE,
            "",
            " › [x] model        ◆ deepseek-v4-pro",
            "   [x] thinking     ◐ think high",
            "   [x] permission   Always ask",
            "   [x] cost         $0.12",
            "   [x] context      ●○○○○○○○○○ 120k/1M ( 12% of window)",
            "   [x] cache        ⟳ cache  95% (last  98%)",
            "   [ ] tps          ↯  33 tok/s",
            "   [ ] version      0.1.0 · a3b8fe7",
            "   [ ] session      会话名字",
            "   [x] directory    ~/src/canter",
            "   [x] branch       ⑂ main",
            "   [x] diff",
            "",
            " Preview",
            FIRST,
            USAGE,
            DIRECTORY,
            KEYS,
        ]
    );
    // Nothing is saved or changed yet.
    assert_eq!(screen.bar(), default_bar());
    assert!(!home.tui_json().exists());
}

/// Space changes the preview at once; where each item goes is fixed, so
/// Left and Right move nothing.
#[test]
fn space_changes_the_preview_as_it_goes_and_nothing_moves() {
    let home = Home {
        model: "deepseek-v4-pro",
        ..Home::new()
    };
    let mut screen = home.open(None);
    screen.work();
    screen.enter("/statusline");

    screen.key(KeyCode::Char(' '));
    assert_eq!(screen.picker()[2], " › [ ] model        ◆ deepseek-v4-pro");
    assert_eq!(
        screen.preview(),
        [" ◐ think high · Always ask · $0.12", USAGE, DIRECTORY]
    );

    // Back on; Right and Left leave the list as it is.
    screen.keys(&[KeyCode::Char(' '), KeyCode::Right, KeyCode::Left]);
    assert_eq!(screen.picker()[2], " › [x] model        ◆ deepseek-v4-pro");
    assert_eq!(screen.picker()[3], "   [x] thinking     ◐ think high");
    assert_eq!(screen.preview(), default_bar());

    // The session name at the end of the second row.
    screen.keys(&[KeyCode::Down; 8]);
    assert_eq!(screen.picker()[10], " › [ ] session      会话名字");
    screen.key(KeyCode::Char(' '));
    assert_eq!(screen.preview()[1], format!("{USAGE} · 会话名字"));

    // The speed before it; in the panel, narrower than the window, the
    // session name then no longer fits and goes, whole.
    screen.keys(&[KeyCode::Up, KeyCode::Up, KeyCode::Char(' ')]);
    assert_eq!(screen.picker()[8], " › [x] tps          ↯  33 tok/s");
    assert_eq!(screen.preview()[1], format!("{USAGE} · ↯  33 tok/s"));

    // None of it is saved or shown below yet.
    assert_eq!(screen.bar(), default_bar());
}

#[test]
fn enter_saves_tui_json_0600_and_the_bar_changes_at_once() {
    for home in [Home::new(), Home::without_canter_home()] {
        let config = home.tui_json().with_file_name("config.json");
        let mut screen = home.open(None);
        assert_eq!(
            screen.bar(),
            [" ◆ test-model · ◐ think high · Always ask", DIRECTORY]
        );
        screen.enter("/statusline");
        // Model off; version on, on the second row.
        screen.key(KeyCode::Char(' '));
        screen.keys(&[KeyCode::Down; 7]);
        screen.key(KeyCode::Char(' '));
        assert_eq!(screen.key(KeyCode::Enter), None);

        assert!(screen.picker().is_empty(), "closed");
        assert_eq!(
            screen.bar(),
            [" ◐ think high · Always ask", " 0.1.0 · a3b8fe7", DIRECTORY]
        );
        let path = home.tui_json();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{}", path.display());
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            saved.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["statusLine"]
        );
        // The line 2 entry is still written where the rows broke, for a
        // terminal from before to read.
        assert_eq!(saved["statusLine"]["items"][4], json!({ "name": "line2" }));
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
    // Thinking off and the session name on.
    screen.keys(&[KeyCode::Down, KeyCode::Char(' ')]);
    screen.keys(&[KeyCode::Down; 7]);
    screen.keys(&[KeyCode::Char(' '), KeyCode::Enter]);
    let bar = screen.bar();
    assert_eq!(bar, [" ◆ test-model · Always ask", " 会话名字", DIRECTORY]);
    screen.enter("/statusline");
    let picker = screen.picker();
    assert_eq!(picker[3], "   [ ] thinking     ◐ think high");
    assert_eq!(picker[10], "   [x] session      会话名字");

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
    screen.keys(&[KeyCode::Char(' '), KeyCode::Down, KeyCode::Char(' ')]);
    assert_eq!(screen.key(KeyCode::Esc), None);
    assert!(screen.picker().is_empty(), "closed");
    assert_eq!(screen.bar(), bar);
    assert!(!home.tui_json().exists());
    screen.enter("/statusline");
    assert_eq!(screen.picker()[2], " › [x] model        ◆ test-model");
}

#[test]
fn an_unreadable_tui_json_means_the_defaults_and_unknown_items_are_skipped() {
    let default = [" ◆ test-model · ◐ think high · Always ask", DIRECTORY];
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
        assert_eq!(screen.picker()[2], " › [x] model        ◆ test-model");
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
    assert_eq!(screen.bar(), [" ◆ test-model", " ~/src/canter"]);
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
    let first = " ◆ test-model · ◐ think high · Always ask";

    let mut screen = home.open(Some("sh ./status.sh"));
    screen.wait_for("custom").await;
    screen.enter("/statusline");
    let picker = screen.picker();
    assert_eq!(picker[2], " › [x] Use statusLine script");
    assert!(picker[3].starts_with("   ───"), "{picker:?}");
    assert_eq!(picker[4], "   [x] model        ◆ test-model");
    assert!(
        screen.preview()[0].contains("statusLine script"),
        "{picker:?}"
    );

    // Off: the items below show, and the script stops running.
    screen.key(KeyCode::Char(' '));
    assert_eq!(screen.picker()[2], " › [ ] Use statusLine script");
    assert_eq!(screen.preview()[0], first);
    screen.key(KeyCode::Enter);
    assert_eq!(screen.bar()[0], first);
    let ran = runs();
    for status in ["working", "idle"].repeat(20) {
        // Changes the script would run for.
        screen.event(0, json!({ "type": "session.status", "status": status }));
        screen.draw();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(screen.bar()[0], first);
    assert_eq!(runs(), ran, "the script does not run while off");

    // A terminal opened later keeps it off.
    let mut again = home.open(Some("sh ./status.sh"));
    for _ in 0..40 {
        again.draw();
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(again.bar()[0], first);
    assert_eq!(runs(), ran);

    // On again: back to the script.
    again.enter("/statusline");
    assert_eq!(again.picker()[2], " › [ ] Use statusLine script");
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
    assert_eq!(screen.bar()[0], " ◆ test-model · ◐ think high");
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
    // 12 rows for the box in the input box's place: 5 for the list.
    screen.resize(15);
    screen.enter("/statusline");
    let preview = [
        "",
        " Preview",
        " ◆ test-model · ◐ think high · Always ask",
        DIRECTORY,
        KEYS,
    ];
    let shown = |list: &[&str]| -> Vec<String> {
        let head = [TITLE, ""];
        head.iter()
            .chain(list)
            .chain(&preview)
            .map(|row| row.to_string())
            .collect()
    };
    assert_eq!(
        screen.picker(),
        shown(&[
            " › [x] model        ◆ test-model",
            "   [x] thinking     ◐ think high",
            "   [x] permission   Always ask",
            "   [x] cost",
            "   ↓ 8 more",
        ])
    );

    screen.keys(&[KeyCode::Down; 4]);
    assert_eq!(
        screen.picker(),
        shown(&[
            "   ↑ 2 more",
            "   [x] permission   Always ask",
            "   [x] cost",
            " › [x] context",
            "   ↓ 7 more",
        ])
    );

    screen.keys(&[KeyCode::Down; 10]);
    assert_eq!(
        screen.picker(),
        shown(&[
            "   ↑ 8 more",
            "   [ ] session      会话名字",
            "   [x] directory    ~/src/canter",
            "   [x] branch       ⑂ main",
            " › [x] diff",
        ])
    );

    // Fewer than three rows left for the list: the preview folds away.
    screen.resize(12);
    assert_eq!(
        screen.picker(),
        [
            TITLE,
            "",
            "   ↑ 7 more",
            "   [ ] version      0.1.0 · a3b8fe7",
            "   [ ] session      会话名字",
            "   [x] directory    ~/src/canter",
            "   [x] branch       ⑂ main",
            " › [x] diff",
            KEYS,
        ]
    );
}

#[test]
fn a_tui_json_saved_with_compact_and_before_diff_gets_diff_until_turned_off() {
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
        let directory = if branch { DIRECTORY } else { " ~/src/canter" };
        assert_eq!(
            screen.bar(),
            [
                " ◆ test-model · ◐ think high · Always ask · $0.12",
                " ●●●○○○○○○○ 120k/400k ( 30% to compact) · ⟳ cache  95% (last  98%)",
                directory,
            ]
        );
        screen.enter("/statusline");
        let mark = if branch { 'x' } else { ' ' };
        let picker = screen.picker();
        assert_eq!(
            picker[6..8],
            [
                "   [x] context      ●●●○○○○○○○ 120k/400k ( 30% to compact)",
                "   [x] cache        ⟳ cache  95% (last  98%)",
            ]
        );
        assert_eq!(
            picker[12..14],
            [
                format!("   [{mark}] branch       ⑂ main"),
                "   [x] diff".to_owned(),
            ]
        );
        assert!(
            !picker.iter().any(|row| row.contains("] compact")),
            "{picker:#?}"
        );
    }

    // Turned off and saved, it stays off.
    let mut screen = home.open(None);
    screen.enter("/statusline");
    screen.keys(&[KeyCode::Down; 11]);
    assert_eq!(screen.picker()[13], " › [x] diff");
    screen.keys(&[KeyCode::Char(' '), KeyCode::Enter]);
    let mut again = home.open(None);
    again.enter("/statusline");
    assert_eq!(again.picker()[13], "   [ ] diff");
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
    assert_eq!(style("tps", "33"), (Some(Color::Cyan), false));
    assert_eq!(style("tps", " tok/s"), (Some(Color::Reset), true));
    assert_eq!(
        style("version", "0.1.0 · a3b8fe7"),
        (Some(Color::Reset), true)
    );
    assert_eq!(style("session", "会话名字"), (Some(Color::Reset), true));
}

/// The panel in the input box's place, without its border: the title from
/// the top border, the rows in it, then the keys from the bottom border;
/// empty when the box at the bottom is the input box.
fn panel(rows: &[String]) -> Vec<String> {
    let Some(top) = rows.iter().rposition(|row| row.starts_with("╭─ ")) else {
        return Vec::new();
    };
    if rows[top + 1..].iter().any(|row| row.starts_with('╭')) {
        return Vec::new();
    }
    let last = rows.iter().rposition(|row| row.starts_with('╰')).unwrap();
    let border = |row: &str| row.trim_end_matches(['─', '╮', '╯']).trim_end().to_owned();
    let mut panel = vec![border(&rows[top]["╭─".len()..])];
    panel.extend(rows[top + 1..last].iter().map(|row| {
        let row = row.strip_prefix('│').unwrap_or(row);
        row.strip_suffix('│').unwrap_or(row).trim_end().to_owned()
    }));
    panel.push(border(rows[last].trim_start_matches(['╰', '─'])));
    panel
}
