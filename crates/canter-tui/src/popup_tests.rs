//! Every list and report above the input box (P2-187): its first row is its
//! title, ` Name` in bold, then maybe ` · ` and a dim detail; its last row
//! is its keys, dim, and no row before it has keys.

use canter_protocol::SessionSnapshot;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use serde_json::{Value, json};

use crate::theme::{self, Theme};
use crate::{App, Ui};

const WIDTH: u16 = 100;

struct Screen {
    ui: Ui<TestBackend>,
    /// The status line, the row the lists open under.
    status: String,
}

impl Screen {
    fn new() -> Self {
        theme::set(Theme::default());
        let mut snapshot = SessionSnapshot::empty("s1");
        snapshot.settings.cwd = "/work".into();
        let app = App::new(snapshot, None, 0);
        let mut ui = Ui::new(TestBackend::new(WIDTH, 140), app).unwrap();
        ui.draw(0).unwrap();
        let mut screen = Self {
            ui,
            status: String::new(),
        };
        let rows = screen.rows();
        screen.status = rows[box_top(&rows) - 1].clone();
        screen
    }

    fn app(&mut self) -> &mut App {
        self.ui.app_mut()
    }

    fn key(&mut self, code: KeyCode) {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 0);
        self.ui.draw(0).unwrap();
    }

    /// Types `command` and Enter, drawing after each key.
    fn enter(&mut self, command: &str) {
        for c in command.chars() {
            self.key(KeyCode::Char(c));
        }
        self.key(KeyCode::Enter);
    }

    fn rows(&self) -> Vec<String> {
        let buffer = self.ui.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    /// Checks the list or report open above the input box: `name` is its
    /// title, the last row its keys.
    fn check(&mut self, name: &str) {
        self.ui.draw(0).unwrap();
        let rows = self.rows();
        let shown = rows.join("\n");
        let top = box_top(&rows);
        let status = rows[..top]
            .iter()
            .rposition(|row| *row == self.status)
            .unwrap_or_else(|| panic!("{name}: no status line:\n{shown}"));
        let popup = &rows[status + 1..top];
        assert!(popup.len() >= 2, "{name}: nothing open:\n{shown}");
        let (first, last) = (status + 1, top - 1);
        let title = &rows[first];
        let buffer = self.ui.backend().buffer();
        let cell = |x: usize, y: usize| &buffer[(x as u16, y as u16)];
        // ` Name`, bold, then nothing or ` · ` and a dim detail.
        assert!(
            title.starts_with(&format!(" {name}")),
            "{name}: first row {title:?}:\n{shown}"
        );
        assert!(
            cell(1, first).modifier.contains(Modifier::BOLD),
            "{name}: title not bold:\n{shown}"
        );
        let rest = &title[1 + name.len()..];
        if !rest.is_empty() {
            assert!(rest.starts_with(" · "), "{name}: {title:?}");
            let dot = 1 + name.chars().count() + 1;
            assert!(
                cell(dot, first).modifier.contains(Modifier::DIM),
                "{name}: detail not dim:\n{shown}"
            );
        }
        // Keys on the last row only, all dim.
        assert!(
            rows[last].contains("Esc"),
            "{name}: last row {:?}:\n{shown}",
            rows[last]
        );
        for x in 0..usize::from(WIDTH) {
            let cell = cell(x, last);
            assert!(
                cell.symbol() == " " || cell.modifier.contains(Modifier::DIM),
                "{name}: keys not dim:\n{shown}"
            );
        }
        if name != "Help" {
            for row in &popup[..popup.len() - 1] {
                assert!(!row.contains("Esc"), "{name}: keys on {row:?}:\n{shown}");
            }
        }
    }
}

fn box_top(rows: &[String]) -> usize {
    rows.iter().rposition(|row| row.starts_with('╭')).unwrap()
}

fn from<T: serde::de::DeserializeOwned>(value: Value) -> T {
    serde_json::from_value(value).unwrap()
}

#[test]
fn choices_have_the_title_first_and_the_keys_last() {
    for (command, name) in [
        ("/mode", "Permission mode"),
        ("/think", "Thinking effort"),
        ("/export", "Export conversation"),
        ("/theme", "Theme"),
        ("/statusline", "Configure status line"),
        ("/help", "Help"),
    ] {
        let mut screen = Screen::new();
        screen.enter(command);
        screen.check(name);
    }

    let mut screen = Screen::new();
    let models = from(json!([
        { "id": "a", "model": "a", "displayName": "Model A", "isDefault": true },
        { "id": "b", "model": "b", "displayName": "Model B", "isDefault": false },
    ]));
    crate::commands::choose_model(screen.app(), models);
    screen.check("Model");

    let mut screen = Screen::new();
    let turns = from(json!({ "data": [
        { "turnId": "t1", "input": [{ "type": "text", "text": "hello" }], "fileCount": 1 },
    ] }));
    crate::rewind::choose_turn(screen.app(), turns);
    screen.check("Rewind before which message?");

    let mut screen = Screen::new();
    let command = json!({ "id": 1, "itemId": "c", "command": "npm run dev", "startedAt": 0,
                          "completedAt": null, "running": true, "exitCode": null });
    screen
        .app()
        .show_background(&from(json!({ "data": [command] })), 0);
    screen.check("Background commands");
    screen.app().show_background_output(&from(
        json!({ "command": command, "output": "ready", "skippedBytes": 0 }),
    ));
    screen.check("Background #1");
    screen
        .app()
        .show_background(&from(json!({ "data": [] })), 0);
    screen.check("Background commands");

    let mut screen = Screen::new();
    let agent = |id: &str, status: &str| {
        json!({ "id": id, "task": "Inspect", "status": status, "result": null,
                "createdAt": 0, "updatedAt": 0, "name": format!("agent-{id}"),
                "agentType": "explorer" })
    };
    screen.app().show_agents(from(json!([
        agent("a1", "running"),
        agent("a2", "completed")
    ])));
    screen.check("Sub-agents");
    screen.app().show_agents(Vec::new());
    screen.check("Sub-agents");
}

#[test]
fn reports_have_the_title_first_and_the_keys_last() {
    let mut screen = Screen::new();
    screen.app().show_context(&from(json!({
        "prefix": [{ "kind": "system", "path": null, "tokens": 2300 }],
        "skillCount": 0, "toolCount": 6, "toolTokens": 2900,
        "messages": { "count": 4, "tokens": 1000, "user": 100, "assistant": 300,
                      "toolResults": 600, "images": 0 },
        "estimatedTokens": 6200, "freeTokens": 990_000,
        "modelContextWindow": 1_000_000, "contextTokens": 6200,
    })));
    screen.check("Context");

    screen.app().show_usage(&from(json!({
        "data": [{ "model": "m", "cachedInputTokens": 340_000, "uncachedInputTokens": 1,
                   "outputTokens": 2, "cost": { "usd": 0.5, "unknown": false } }],
        "total": { "cachedInputTokens": 340_000, "uncachedInputTokens": 1,
                   "outputTokens": 2, "cost": { "usd": 0.5, "unknown": false } },
    })));
    screen.check("Usage");

    screen.app().show_skills(from(json!([
        { "name": "example", "description": "Inspect", "path": "/s/SKILL.md",
          "source": "project", "modelInvocation": true },
    ])));
    screen.check("Skills");

    screen.app().show_mcp(&from(json!({ "data": [
        { "name": "docs", "connected": true, "error": null, "warnings": [],
          "tools": [{ "name": "mcp__docs__query", "tool": "query" }] },
    ] })));
    screen.check("MCP");

    screen.app().show_sandbox(&Default::default());
    screen.check("Sandbox");
    screen.app().show_sandbox(&from(json!({
        "enabled": true, "writable": ["/work"], "readOnly": [], "noAccess": [], "network": false,
    })));
    screen.check("Sandbox");

    screen.app().show_goal(None);
    screen.check("Goal");

    screen.app().show_command_rules(Ok(from(json!({
        "path": "/rules.json", "data": [], "fileRules": [],
    }))));
    screen.check("Permissions");

    screen.app().show_memories(Ok(from(json!({ "data": [] }))));
    screen.check("Memory");

    screen.app().show_language_servers(from(json!([
        { "id": "l1", "name": "rust-analyzer", "root": "/work", "state": "running",
          "sharedReferences": 0, "updatedAt": 1 },
    ])));
    screen.check("LSP");
    screen.key(KeyCode::Enter);
    screen.check("LSP");
    screen.app().show_language_servers(Vec::new());
    screen.check("LSP");
}

#[tokio::test]
async fn diff_and_files_have_the_title_first_and_the_keys_last() {
    let root = tempfile::tempdir().unwrap();
    let mut screen = Screen::new();
    let changes = crate::uncommitted::collect(root.path().to_str().unwrap()).await;
    screen.app().show_diff(changes);
    screen.check("Diff");
    screen.app().show_files().await;
    screen.check("Files");
}
