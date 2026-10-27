//! `/theme`: the colours of code and diffs, chosen from a list with a live
//! preview and saved in `tui.json`. Homes and `CANTER_HOME` are temporary
//! directories, never the user's.

use std::collections::BTreeMap;
use std::path::PathBuf;

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::style::Color;
use serde_json::{Value, json};
use tempfile::TempDir;
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const WIDE: u16 = 120;
const HEIGHT: u16 = 40;
const TITLE: &str = " Theme · code and diff colours";

/// The 32 themes two-face comes with, by the names Codex gives them.
const BUILT_IN: [&str; 32] = [
    "1337",
    "ansi",
    "base16",
    "base16-256",
    "base16-eighties-dark",
    "base16-mocha-dark",
    "base16-ocean-dark",
    "base16-ocean-light",
    "catppuccin-frappe",
    "catppuccin-latte",
    "catppuccin-macchiato",
    "catppuccin-mocha",
    "coldark-cold",
    "coldark-dark",
    "dark-neon",
    "dracula",
    "github",
    "gruvbox-dark",
    "gruvbox-light",
    "inspired-github",
    "monokai-extended",
    "monokai-extended-bright",
    "monokai-extended-light",
    "monokai-extended-origin",
    "nord",
    "one-half-dark",
    "one-half-light",
    "solarized-dark",
    "solarized-light",
    "sublime-snazzy",
    "two-dark",
    "zenburn",
];

const DARK_TRUE: Theme = Theme {
    depth: Depth::TrueColor,
    background: Background::Dark,
};

/// A theme file with every keyword red and other text dark blue.
const MINE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>name</key><string>Mine</string>
<key>settings</key><array>
<dict><key>settings</key><dict><key>foreground</key><string>#112233</string></dict></dict>
<dict><key>scope</key><string>keyword, storage</string>
<key>settings</key><dict><key>foreground</key><string>#FF0000</string></dict></dict>
</array></dict></plist>
"#;

/// A temporary home with `CANTER_HOME` in it.
struct Home {
    _root: TempDir,
    canter: PathBuf,
    env: BTreeMap<String, String>,
}

impl Home {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let canter = root.path().join("canter-home");
        std::fs::create_dir(&canter).unwrap();
        let env = BTreeMap::from([
            ("HOME".into(), root.path().to_str().unwrap().into()),
            ("CANTER_HOME".into(), canter.to_str().unwrap().into()),
        ]);
        Self {
            _root: root,
            canter,
            env,
        }
    }

    fn tui_json(&self) -> PathBuf {
        self.canter.join("tui.json")
    }

    fn read_tui_json(&self) -> Value {
        serde_json::from_str(&std::fs::read_to_string(self.tui_json()).unwrap()).unwrap()
    }

    fn write_tui_json(&self, value: &Value) {
        std::fs::write(self.tui_json(), value.to_string()).unwrap();
    }

    fn theme_file(&self, name: &str, text: &str) {
        let dir = self.canter.join("themes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{name}.tmTheme")), text).unwrap();
    }

    /// A terminal `width` wide drawn for `look`, opened on a session whose
    /// answer has a Rust code block.
    fn open(&self, look: Theme, width: u16) -> Screen {
        theme::set(look);
        let app = App::new(SessionSnapshot::empty(SESSION), None, 0)
            .with_status_line(None, self.env.clone());
        let mut screen = Screen {
            ui: Ui::new(TestBackend::new(width, HEIGHT), app).unwrap(),
            seq: 0,
        };
        let turn = |status: &str| {
            json!({
                "id": "t1", "items": [], "status": status, "error": null,
                "input": [{ "type": "text", "text": "show some code" }],
                "clientName": "canter-tui", "usage": null,
            })
        };
        let message = json!({ "type": "agentMessage", "id": "m1",
            "text": "Here:\n\n```rust\nfn main() { let answer = 42; }\n```\n" });
        screen.event(json!({ "type": "turn.started", "turn": turn("inProgress") }));
        screen.event(json!({ "type": "item.started", "turnId": "t1", "item": message }));
        screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": message }));
        screen.event(json!({ "type": "turn.completed", "turn": turn("completed") }));
        screen.draw();
        screen
    }
}

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn event(&mut self, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": 0 });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, 0);
    }

    fn draw(&mut self) {
        self.ui.draw(0).unwrap();
    }

    fn key(&mut self, code: KeyCode) {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 0);
        self.draw();
    }

    fn enter(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
        self.key(KeyCode::Enter);
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

    /// The rows of the picker, from its title to the input box.
    fn picker(&self) -> Vec<String> {
        let rows = self.rows();
        let Some(start) = rows.iter().position(|row| row == TITLE) else {
            return Vec::new();
        };
        let end = rows
            .iter()
            .rposition(|row| row.starts_with('╭'))
            .expect("input box");
        rows[start..end].to_vec()
    }

    /// The selected row of the picker: its name and what follows it, up
    /// to the preview, with the spaces lining the tags up as two.
    fn selected(&self) -> String {
        let picker = self.picker();
        let row = picker
            .iter()
            .find_map(|row| row.strip_prefix(" › "))
            .unwrap_or_else(|| panic!("no row selected: {picker:#?}"));
        let row = row.split('│').next().unwrap().trim();
        row.split("  ")
            .filter(|part| !part.is_empty())
            .map(str::trim)
            .collect::<Vec<_>>()
            .join("  ")
    }

    /// Only the name of the selected row.
    fn selected_name(&self) -> String {
        let selected = self.selected();
        selected.split("  ").next().unwrap().to_owned()
    }

    /// The colours of the code block's `fn` and `42` in the conversation.
    fn code_colours(&self) -> [Option<Color>; 2] {
        let rows = self.rows();
        let y = rows
            .iter()
            .position(|row| row.contains("fn main() { let answer = 42; }"))
            .expect("the code block");
        let buffer = self.ui.backend().buffer();
        let x = |needle: &str| rows[y].find(needle).unwrap() as u16;
        let fg = |x: u16| match buffer[(x, y as u16)].fg {
            Color::Reset => None,
            color => Some(color),
        };
        [fg(x("fn")), fg(x("42"))]
    }

    /// Moves down to the row named `name`.
    fn down_to(&mut self, name: &str) {
        for _ in 0..60 {
            if self.selected_name() == name {
                return;
            }
            self.key(KeyCode::Down);
        }
        panic!("no row {name}");
    }
}

fn is_rgb(color: Option<Color>) -> bool {
    matches!(color, Some(Color::Rgb(..)))
}

#[test]
fn the_list_is_terminal_auto_the_32_built_in_then_the_files() {
    let home = Home::new();
    home.theme_file("my-theme", MINE);
    home.theme_file("dracula", MINE);
    let mut screen = home.open(DARK_TRUE, WIDE);
    screen.enter("/theme");
    assert_eq!(screen.picker()[0], TITLE, "{:#?}", screen.rows());
    assert_eq!(screen.selected(), "terminal  (current)");
    let mut names = vec![screen.selected_name()];
    for _ in 0..40 {
        screen.key(KeyCode::Down);
        let name = screen.selected_name();
        if names.last() == Some(&name) {
            break;
        }
        names.push(name);
    }
    let mut expected = vec!["terminal".to_owned(), "auto · catppuccin-mocha".to_owned()];
    expected.extend(BUILT_IN.iter().map(|name| (*name).to_owned()));
    expected.push("my-theme".to_owned());
    assert_eq!(names, expected);
    assert!(
        screen.selected().ends_with("(custom)"),
        "{}",
        screen.selected()
    );
}

#[test]
fn moving_previews_esc_restores_and_enter_saves_only_the_theme() {
    let home = Home::new();
    let before = json!({
        "statusLine": { "items": [{ "name": "model", "show": true }], "useScript": false },
        "somethingElse": [1, 2],
    });
    home.write_tui_json(&before);
    let mut screen = home.open(DARK_TRUE, WIDE);
    let terminal = screen.code_colours();
    assert!(!is_rgb(terminal[0]), "{terminal:?}");

    screen.enter("/theme");
    screen.down_to("dracula");
    let dracula = screen.code_colours();
    assert!(is_rgb(dracula[0]) && is_rgb(dracula[1]), "{dracula:?}");
    screen.down_to("github");
    let github = screen.code_colours();
    assert!(is_rgb(github[0]), "{github:?}");
    assert_ne!(github, dracula);

    screen.key(KeyCode::Esc);
    assert!(screen.picker().is_empty());
    assert_eq!(screen.code_colours(), terminal);
    assert_eq!(home.read_tui_json(), before);

    screen.enter("/theme");
    assert_eq!(screen.selected_name(), "terminal");
    screen.down_to("dracula");
    screen.key(KeyCode::Enter);
    assert!(screen.picker().is_empty());
    assert_eq!(screen.code_colours(), dracula);
    let mut after = before.clone();
    after["theme"] = json!("dracula");
    assert_eq!(home.read_tui_json(), after);

    // Saving the status line later keeps the theme.
    screen.enter("/statusline");
    screen.key(KeyCode::Enter);
    assert_eq!(home.read_tui_json()["theme"], json!("dracula"));
}

#[test]
fn a_terminal_opened_later_draws_the_saved_theme() {
    let home = Home::new();
    let mut first = home.open(DARK_TRUE, WIDE);
    first.enter("/theme");
    first.down_to("dracula");
    let dracula = first.code_colours();
    first.key(KeyCode::Enter);
    drop(first);

    let mut later = home.open(DARK_TRUE, WIDE);
    assert_eq!(later.code_colours(), dracula);
    later.enter("/theme");
    assert_eq!(later.selected(), "dracula  (current)");
}

#[test]
fn auto_picks_mocha_on_dark_and_latte_on_light() {
    for (background, label, name) in [
        (
            Background::Dark,
            "auto · catppuccin-mocha",
            "catppuccin-mocha",
        ),
        (
            Background::Unknown,
            "auto · catppuccin-mocha",
            "catppuccin-mocha",
        ),
        (
            Background::Light,
            "auto · catppuccin-latte",
            "catppuccin-latte",
        ),
    ] {
        let home = Home::new();
        let look = Theme {
            depth: Depth::TrueColor,
            background,
        };
        let mut screen = home.open(look, WIDE);
        screen.enter("/theme");
        screen.down_to(name);
        let wanted = screen.code_colours();
        screen.key(KeyCode::Esc);
        screen.enter("/theme");
        screen.down_to(label);
        assert_eq!(screen.code_colours(), wanted, "{background:?}");
        screen.key(KeyCode::Enter);
        assert_eq!(home.read_tui_json()["theme"], json!("auto"));
        let later = home.open(look, WIDE);
        assert_eq!(later.code_colours(), wanted, "{background:?}");
    }
}

#[test]
fn theme_files_are_listed_once_and_a_broken_one_changes_nothing() {
    let home = Home::new();
    home.theme_file("my-theme", MINE);
    home.theme_file("dracula", MINE);
    home.theme_file("broken", "not a theme");
    let mut screen = home.open(DARK_TRUE, WIDE);
    screen.enter("/theme");

    screen.down_to("dracula");
    let dracula = screen.code_colours();
    assert_eq!(screen.selected(), "dracula", "the built-in, not the file");
    screen.key(KeyCode::Down);
    assert_ne!(screen.selected_name(), "dracula");

    screen.down_to("broken");
    let shown = screen.code_colours();
    assert!(
        screen.selected().contains("unreadable"),
        "{}",
        screen.selected()
    );
    screen.key(KeyCode::Enter);
    assert!(!home.tui_json().exists(), "a broken file is not saved");

    screen.down_to("my-theme");
    assert_eq!(
        screen.code_colours(),
        [
            Some(Color::Rgb(0xff, 0, 0)),
            Some(Color::Rgb(0x11, 0x22, 0x33))
        ]
    );
    assert_ne!(shown, screen.code_colours());
    assert_ne!(dracula, screen.code_colours());
    screen.key(KeyCode::Enter);
    assert_eq!(home.read_tui_json()["theme"], json!("my-theme"));
}

#[test]
fn a_saved_theme_not_found_draws_terminal_and_says_so() {
    let terminal = Home::new().open(DARK_TRUE, WIDE).code_colours();
    for (name, file) in [("nope", None), ("broken", Some("not a theme"))] {
        let home = Home::new();
        if let Some(text) = file {
            home.theme_file(name, text);
        }
        let saved = json!({ "theme": name });
        home.write_tui_json(&saved);
        let mut screen = home.open(DARK_TRUE, WIDE);
        let warning = format!("Theme {name} not found; using terminal");
        assert!(
            screen.rows().iter().any(|row| row.contains(&warning)),
            "{:#?}",
            screen.rows()
        );
        assert_eq!(screen.code_colours(), terminal);
        screen.enter("/theme");
        assert_eq!(screen.selected_name(), "terminal");
        screen.key(KeyCode::Esc);
        assert_eq!(home.read_tui_json(), saved);
    }
}

#[test]
fn with_16_colours_only_terminal_applies() {
    let sixteen = Theme {
        depth: Depth::Ansi16,
        background: Background::Dark,
    };
    let home = Home::new();
    let mut screen = home.open(sixteen, WIDE);
    let terminal = screen.code_colours();
    screen.enter("/theme");
    assert_eq!(
        screen.picker()[1],
        " This terminal shows 16 colours; only terminal applies"
    );
    screen.down_to("dracula");
    assert_eq!(screen.code_colours(), terminal);
    screen.key(KeyCode::Enter);
    assert_eq!(home.read_tui_json()["theme"], json!("dracula"));
    assert_eq!(screen.code_colours(), terminal);
    let later = home.open(sixteen, WIDE);
    assert_eq!(later.code_colours(), terminal);
}
