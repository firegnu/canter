//! Every list and report in the input box's place (P2-187, P2-238): its
//! title is on the top border, ` Name` in bold, then maybe ` · ` and a dim
//! detail; its keys are on the bottom border, dim, and no row before it has
//! keys. `/agents` and `/ps` open the same way under the info rows (P2-240).

use canter_protocol::SessionSnapshot;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use serde_json::{Value, json};

use crate::theme::{self, Theme};
use crate::{App, Ui};

const WIDTH: u16 = 100;

#[test]
fn roles_command_is_registered() {
    let command = crate::commands::find("roles").expect("/roles should be available");
    assert_eq!(command.about, "List built-in and custom sub-agent roles");
    let mut screen = Screen::new();
    screen.app().show_roles(&from(json!({"directory":"/isolated/state/agents","data":[
        {"name":"explorer","description":"Read-only lookup.","readOnly":true,"builtIn":true},
        {"name":"general","description":"Independent work.","readOnly":false,"builtIn":true},
        {"name":"reviewer","description":"Review recent changes.","model":"solver/worker","readOnly":true,"builtIn":false},
        {"name":"broken","description":"Unavailable role.","readOnly":false,"builtIn":false,"error":"Unknown tool: absent"}
    ]})));
    screen.check("Roles");
    let rows = screen.rows();
    let shown = rows.join("\n");
    for text in [
        "4 · 3 available · 1 unavailable",
        "Built-in",
        "Custom",
        "~/.canter/agents",
        "inherits model",
        "solver/worker",
        "read-only",
        "can edit",
        "Review recent changes.",
        "Unknown tool: absent",
    ] {
        assert!(shown.contains(text), "missing {text}: {shown}");
    }
    let row = rows.iter().position(|row| row.contains("broken")).unwrap();
    let column = rows[row].find("broken").unwrap();
    assert_eq!(
        screen.ui.backend().buffer()[(column as u16, row as u16)].fg,
        theme::failure().fg.unwrap()
    );
    screen.app().show_roles(&from(
        json!({"directory":"/isolated/state/agents","data":[]}),
    ));
    screen.check("Roles");
    assert!(
        screen
            .rows()
            .join("\n")
            .contains("Add roles as ~/.canter/agents/<name>.md")
    );
}

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

    /// Checks the panel open in the input box's place: `name` is its
    /// title, on the top border; its keys are on the bottom border.
    fn check(&mut self, name: &str) {
        self.check_at(name, false);
    }

    /// Checks `/agents` or `/ps` open under the two info rows (P2-240),
    /// the input box still under the status line.
    fn check_under_info(&mut self, name: &str) {
        self.check_at(name, true);
    }

    fn check_at(&mut self, name: &str, under_info: bool) {
        self.ui.draw(0).unwrap();
        let rows = self.rows();
        let shown = rows.join("\n");
        let top = box_top(&rows);
        if under_info {
            let input = rows.iter().position(|row| row.starts_with('╭')).unwrap();
            assert_eq!(
                rows[input - 1],
                self.status,
                "{name}: input box not under the status line:\n{shown}"
            );
            let input_bottom = input
                + rows[input..]
                    .iter()
                    .position(|row| row.starts_with('╰'))
                    .unwrap();
            assert_eq!(
                top,
                input_bottom + 3,
                "{name}: not under the info rows:\n{shown}"
            );
        } else {
            assert_eq!(
                rows[top - 1],
                self.status,
                "{name}: not under the status line:\n{shown}"
            );
        }
        let last = box_bottom(&rows);
        let popup = &rows[top..=last];
        let (first, title) = (top, &rows[top]);
        let buffer = self.ui.backend().buffer();
        let cell = |x: usize, y: usize| &buffer[(x as u16, y as u16)];
        // `╭─`, ` Name`, bold, then nothing or ` · ` and a dim detail.
        let title = title
            .strip_prefix("╭─")
            .unwrap_or_else(|| panic!("{name}: nothing open:\n{shown}"));
        assert!(
            title.starts_with(&format!(" {name}")),
            "{name}: first row {title:?}:\n{shown}"
        );
        assert!(
            cell(3, first).modifier.contains(Modifier::BOLD),
            "{name}: title not bold:\n{shown}"
        );
        let rest = &title[1 + name.len()..];
        if !rest.starts_with(" ─") {
            assert!(rest.starts_with(" · "), "{name}: {title:?}");
            let dot = 2 + 1 + name.chars().count() + 1;
            assert!(
                cell(dot, first).modifier.contains(Modifier::DIM),
                "{name}: detail not dim:\n{shown}"
            );
        }
        // Keys on the bottom border only, all dim.
        assert!(
            rows[last].contains("Esc") && rows[last].ends_with(" ─╯"),
            "{name}: last row {:?}:\n{shown}",
            rows[last]
        );
        for x in 0..usize::from(WIDTH) {
            let cell = cell(x, last);
            assert!(
                matches!(cell.symbol(), " " | "─" | "╰" | "╯")
                    || cell.modifier.contains(Modifier::DIM),
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

fn box_bottom(rows: &[String]) -> usize {
    rows.iter().rposition(|row| row.starts_with('╰')).unwrap()
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
    screen.check_under_info("Background commands");
    screen.app().show_background_output(&from(
        json!({ "command": command, "output": "ready", "skippedBytes": 0 }),
    ));
    screen.check_under_info("Background #1");
    screen
        .app()
        .show_background(&from(json!({ "data": [] })), 0);
    screen.check_under_info("Background commands");

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
    screen.check_under_info("Sub-agents");
    screen.app().show_agents(Vec::new());
    screen.check_under_info("Sub-agents");
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

impl Screen {
    /// Presses `code` and does what the terminal does with a `/model` step
    /// it returns; returns any other action.
    fn press(&mut self, code: KeyCode) -> Option<crate::Action> {
        let action = self
            .ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 0);
        let action = match action {
            Some(crate::Action::ModelProviders(selected)) => {
                crate::commands::choose_provider(self.app(), selected);
                None
            }
            Some(crate::Action::ProviderModels(provider)) => {
                crate::commands::choose_provider_model(self.app(), provider);
                None
            }
            other => other,
        };
        self.ui.draw(0).unwrap();
        action
    }

    /// The rows of the panel open in the input box's place, without its
    /// border: the title, the rows in it, then the keys.
    fn popup(&mut self) -> Vec<String> {
        self.ui.draw(0).unwrap();
        let rows = self.rows();
        let (top, last) = (box_top(&rows), box_bottom(&rows));
        let Some(title) = rows[top]
            .strip_prefix("╭─")
            .filter(|title| title.starts_with(' '))
        else {
            return Vec::new();
        };
        let border = |row: &str| row.trim_end_matches(['─', '╮', '╯']).trim_end().to_owned();
        let mut popup = vec![border(title)];
        popup.extend(rows[top + 1..last].iter().map(|row| {
            let row = row.strip_prefix('│').unwrap_or(row);
            row.strip_suffix('│').unwrap_or(row).trim_end().to_owned()
        }));
        popup.push(border(rows[last].trim_start_matches(['╰', '─'])));
        popup
    }
}

fn model(id: &str, provider: Option<&str>, window: u64, image: bool) -> Value {
    let kinds = if image {
        json!(["text", "image"])
    } else {
        json!(["text"])
    };
    let mut model = json!({ "id": id, "model": id, "displayName": id, "isDefault": false,
                            "contextWindow": window, "inputModalities": kinds });
    if let Some(provider) = provider {
        model["provider"] = json!(provider);
    }
    model
}

/// P2-201: with several providers `/model` lists them first, then the
/// models of the one chosen; Esc goes back, then closes.
#[test]
fn model_choice_goes_through_the_provider_first() {
    let models = || {
        from::<Vec<canter_protocol::Model>>(json!([
            model("fake-model", None, 1_048_576, false),
            model("two/small", Some("two"), 100_000, false),
            model("one/vendor/model", Some("one"), 200_000, true),
            model("two/big", Some("two"), 1_048_576, true),
        ]))
    };
    let mut screen = Screen::new();
    screen.app().apply_event(
        &from(
            json!({ "seq": 1, "sessionId": "s1", "timestamp": 0, "type": "session.updated",
                      "settings": { "cwd": "/work", "name": null, "model": "one/vendor/model",
                                    "approvalPolicy": "alwaysAsk" } }),
        ),
        0,
    );
    crate::commands::choose_model(screen.app(), models());
    screen.check("Provider");
    let keys = " ↑↓ select · Enter choose · Esc cancel";
    assert_eq!(
        screen.popup(),
        [
            " Provider",
            "   default         1 model",
            " › one (current)   1 model",
            "   two             2 models",
            keys,
        ]
    );

    assert_eq!(screen.press(KeyCode::Down), None);
    assert_eq!(screen.press(KeyCode::Enter), None);
    screen.check("Model");
    assert_eq!(
        screen.popup(),
        [
            " Model · two",
            " › big     1049k context · images",
            "   small   100k context",
            keys,
        ]
    );
    // Esc: back to the providers, on the one left.
    assert_eq!(screen.press(KeyCode::Esc), None);
    assert_eq!(screen.popup()[3], " › two             2 models");
    assert_eq!(screen.press(KeyCode::Up), None);
    assert_eq!(screen.press(KeyCode::Up), None);
    assert_eq!(screen.press(KeyCode::Enter), None);
    assert_eq!(
        screen.popup(),
        [" Model · default", " › fake-model   1049k context", keys]
    );
    assert_eq!(screen.press(KeyCode::Esc), None);
    assert_eq!(screen.press(KeyCode::Esc), None);
    assert!(screen.popup().is_empty(), "{:?}", screen.rows());

    // The model in use is marked and selected in its provider's list.
    crate::commands::choose_model(screen.app(), models());
    screen.check("Provider");
    assert_eq!(screen.press(KeyCode::Enter), None);
    assert_eq!(
        screen.popup()[..2],
        [
            " Model · one",
            " › vendor/model (current)   200k context · images"
        ]
    );
    assert_eq!(screen.press(KeyCode::Esc), None);
    assert_eq!(screen.press(KeyCode::Down), None);
    assert_eq!(screen.press(KeyCode::Enter), None);
    assert_eq!(screen.press(KeyCode::Down), None);
    assert_eq!(
        screen.press(KeyCode::Enter),
        Some(crate::Action::Update(
            canter_protocol::SessionUpdateParams {
                model: Some("two/small".into()),
                remember_selection: true,
                ..Default::default()
            }
        ))
    );
    assert!(screen.popup().is_empty());
}

/// Models from one provider only: straight to its models, and Esc closes.
/// Without providers the list is as it was before P2-201.
#[test]
fn model_choice_with_one_provider_skips_the_provider_list() {
    let mut screen = Screen::new();
    crate::commands::choose_model(
        screen.app(),
        from(json!([
            model("go/b", Some("go"), 200_000, false),
            model("go/a", Some("go"), 1_048_576, true),
        ])),
    );
    screen.check("Model");
    assert_eq!(
        screen.popup()[..3],
        [
            " Model · go",
            " › a   1049k context · images",
            "   b   200k context"
        ]
    );
    assert_eq!(screen.press(KeyCode::Esc), None);
    assert!(screen.popup().is_empty());

    crate::commands::choose_model(
        screen.app(),
        from(json!([
            model("deepseek-v4-pro", None, 1_048_576, false),
            model("deepseek-flash", None, 1_048_576, true),
        ])),
    );
    assert_eq!(
        screen.popup()[..3],
        [" Model", " › deepseek-v4-pro", "   deepseek-flash"]
    );
    assert_eq!(screen.press(KeyCode::Esc), None);
    assert!(screen.popup().is_empty());
}

#[test]
fn account_catalog_preserves_order_marks_unverified_and_warns_once() {
    let mut first = model("chatgpt/zed", Some("chatgpt"), 128000, false);
    first["displayName"] = json!("Zed");
    first["fromCatalog"] = json!(true);
    first["verified"] = json!(false);
    first["missingCapabilities"] = json!(["contextWindow", "reasoningEffort", "imageInput"]);
    let mut second = model("chatgpt/alpha", Some("chatgpt"), 200000, true);
    second["displayName"] = json!("Alpha");
    second["fromCatalog"] = json!(true);
    second["verified"] = json!(true);
    let mut screen = Screen::new();
    screen.app().set_model_list(from(json!({
        "data":[first, second],"nextCursor":null,
        "catalogs":[{"provider":"chatgpt","stale":false,"refreshFailed":true}]
    })));
    let models = screen.app().models().to_vec();
    crate::commands::choose_model(screen.app(), models);
    let popup = screen.popup();
    assert_eq!(popup[0], " Model · chatgpt · Could not load ChatGPT models");
    assert!(popup[1].contains("Zed (unverified)"), "{popup:?}");
    assert!(popup[2].contains("Alpha"), "{popup:?}");
    let warning = screen.app().unverified_warning("chatgpt/zed").unwrap();
    assert_eq!(
        warning,
        "Model details for Zed are unverified: using a 128000-token context window, medium thinking effort, and no image input."
    );
    assert!(screen.app().unverified_warning("chatgpt/zed").is_none());
    assert!(screen.app().unverified_warning("chatgpt/alpha").is_none());
}

/// P2-238: a panel open takes the input box's place, in a rounded box in
/// the accent colour, its title on the top border and its keys on the
/// bottom one; Esc brings the input box and the draft back. The lists for
/// what is typed stay above the input box, without a box.
#[test]
fn panels_take_the_input_box_place() {
    let mut screen = Screen::new();
    for c in "draft".chars() {
        screen.key(KeyCode::Char(c));
    }
    let accent = theme::accent().fg.unwrap();
    let panel = |screen: &mut Screen, title: &str, keys: &str| {
        let rows = screen.rows();
        let shown = rows.join("\n");
        let tops: Vec<usize> = (0..rows.len())
            .filter(|&y| rows[y].starts_with('╭'))
            .collect();
        assert_eq!(tops.len(), 1, "one box only:\n{shown}");
        let top = tops[0];
        let bottom = (top..rows.len())
            .find(|&y| rows[y].starts_with('╰'))
            .unwrap_or_else(|| panic!("no bottom border:\n{shown}"));
        assert!(
            rows[top].starts_with(&format!("╭─ {title}")) && rows[top].ends_with("─╮"),
            "title on the top border: {:?}\n{shown}",
            rows[top]
        );
        assert!(
            rows[bottom].ends_with(&format!(" {keys} ─╯")),
            "keys on the bottom border: {:?}\n{shown}",
            rows[bottom]
        );
        for row in &rows[top + 1..bottom] {
            assert!(
                row.starts_with('│') && row.ends_with('│'),
                "{row:?}\n{shown}"
            );
        }
        let buffer = screen.ui.backend().buffer();
        for (x, y) in [(0, top), (0, bottom), (WIDTH - 1, top + 1)] {
            assert_eq!(buffer[(x, y as u16)].fg, accent, "border colour:\n{shown}");
        }
        assert!(!shown.contains("draft"), "input box shown:\n{shown}");
        // The info rows stay under it.
        assert_eq!(bottom + 3, rows.len(), "{shown}");
    };
    let input = |screen: &mut Screen, draft: &str| {
        let rows = screen.rows();
        let shown = rows.join("\n");
        let top = box_top(&rows);
        let row = rows[top + 1]
            .strip_suffix('│')
            .unwrap_or_else(|| panic!("{shown}"));
        assert_eq!(row.trim_end(), format!("│ › {draft}"), "{shown}");
        let buffer = screen.ui.backend().buffer();
        assert_ne!(buffer[(0, top as u16)].fg, accent, "{shown}");
        assert!(!shown.contains("Esc close"), "{shown}");
    };

    screen.app().show_mcp(&from(json!({ "data": [
        { "name": "docs", "connected": true, "error": null, "warnings": [],
          "tools": [{ "name": "mcp__docs__query", "tool": "query" }] },
    ] })));
    screen.ui.draw(0).unwrap();
    panel(&mut screen, "MCP · 1 service · 1 tool", "Esc close");
    assert!(screen.rows().join("\n").contains("│ ● docs"));
    screen.key(KeyCode::Esc);
    input(&mut screen, "draft");

    crate::commands::choose_model(
        screen.app(),
        from(json!([
            model("go/b", Some("go"), 200_000, false),
            model("go/a", Some("go"), 1_048_576, true),
        ])),
    );
    screen.ui.draw(0).unwrap();
    panel(
        &mut screen,
        "Model · go",
        "↑↓ select · Enter choose · Esc cancel",
    );
    assert!(screen.rows().join("\n").contains("│ › a   1049k context"));
    screen.key(KeyCode::Esc);
    input(&mut screen, "draft");

    // The command list stays above the input box, without a box.
    for _ in 0.."draft".len() {
        screen.key(KeyCode::Backspace);
    }
    screen.key(KeyCode::Char('/'));
    let rows = screen.rows();
    let shown = rows.join("\n");
    let top = box_top(&rows);
    assert_eq!(rows.iter().filter(|row| row.starts_with('╭')).count(), 1);
    assert!(rows[top + 1].starts_with("│ › /"), "{shown}");
    assert!(
        rows[..top].iter().any(|row| row.contains("/add-dir")),
        "{shown}"
    );
    assert!(!rows[..top].iter().any(|row| row.contains('│')), "{shown}");
}
