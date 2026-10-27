//! `/files`, `/mcp` and `/skills`: the result in a box above the input box,
//! as `/context`'s, not in the conversation; in sections, coloured.

use std::path::Path;

use canter_protocol::{Event, McpListResponse, SessionSnapshot, Skill};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new(width: u16, height: u16, cwd: &str, home: Option<&str>) -> Self {
        let mut snapshot = SessionSnapshot::empty(SESSION);
        snapshot.settings.cwd = cwd.to_owned();
        let app = App::new(snapshot, home.map(str::to_owned), 0);
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(width, height), app).unwrap(),
            seq: 0,
        };
        // Something in the conversation that must stay as it is.
        let turn = |status: &str| {
            json!({ "id": "t1", "items": [], "status": status, "error": null,
                    "input": [{ "type": "text", "text": "hello there" }],
                    "clientName": "canter-tui", "usage": null })
        };
        screen.event(json!({ "type": "turn.started", "turn": turn("inProgress") }));
        screen.event(json!({ "type": "item.completed", "turnId": "t1",
            "item": { "type": "agentMessage", "id": "m1", "text": "General Kenobi" } }));
        screen.event(json!({ "type": "turn.completed", "turn": turn("completed") }));
        screen.draw();
        screen
    }

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

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        let action = self
            .ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 0);
        self.draw();
        action
    }

    /// Types `command` and Enter.
    fn enter(&mut self, command: &str) -> Option<Action> {
        for c in command.chars() {
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

    /// The rows from the one starting with `title` down to the input box;
    /// empty while closed.
    fn popup(&self, title: &str) -> Vec<String> {
        let rows = self.rows();
        match rows.iter().position(|row| row.starts_with(title)) {
            Some(start) => rows[start..self.box_top()].to_vec(),
            None => Vec::new(),
        }
    }

    /// The colour and the modifiers of the first cell of `text` on the first
    /// row holding it.
    fn style(&self, text: &str) -> (Color, Modifier) {
        self.style_on(text, text)
    }

    /// The same, for `text` on the first row holding `row`.
    fn style_on(&self, row: &str, text: &str) -> (Color, Modifier) {
        let buffer = self.ui.backend().buffer();
        let rows = self.rows();
        let y = rows
            .iter()
            .position(|line| line.contains(row) && line.contains(text))
            .unwrap_or_else(|| panic!("{text}: {rows:#?}"));
        let x = rows[y][..rows[y].find(text).unwrap()].width();
        let cell = &buffer[(x as u16, y as u16)];
        (cell.fg, cell.modifier)
    }
}

fn git(dir: &Path, home: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
        .output()
        .unwrap()
}

/// A repository on `main` in the middle of a merge, with each kind of
/// status: unstaged, partly staged, conflicted, staged, deleted, untracked.
fn repository(repo: &Path, home: &Path) {
    let ok = |args: &[&str]| {
        let output = git(repo, home, args);
        assert!(output.status.success(), "{args:?}: {output:?}");
    };
    std::fs::create_dir_all(repo).unwrap();
    ok(&["init", "-q", "-b", "main"]);
    for name in [
        "README.md",
        "both.txt",
        "conflict.txt",
        "gone.txt",
        "old.js",
    ] {
        std::fs::write(repo.join(name), "base").unwrap();
    }
    ok(&["add", "."]);
    ok(&["commit", "-qm", "initial"]);
    ok(&["checkout", "-qb", "other"]);
    std::fs::write(repo.join("conflict.txt"), "theirs").unwrap();
    ok(&["commit", "-qam", "theirs"]);
    ok(&["checkout", "-q", "main"]);
    std::fs::write(repo.join("conflict.txt"), "ours").unwrap();
    ok(&["commit", "-qam", "ours"]);
    assert!(!git(repo, home, &["merge", "-q", "other"]).status.success());
    std::fs::write(repo.join("README.md"), "changed").unwrap();
    std::fs::write(repo.join("both.txt"), "staged").unwrap();
    ok(&["add", "both.txt"]);
    std::fs::write(repo.join("both.txt"), "and changed again").unwrap();
    std::fs::create_dir(repo.join("docs")).unwrap();
    std::fs::write(repo.join("docs/new.md"), "new").unwrap();
    ok(&["add", "docs/new.md"]);
    ok(&["rm", "-q", "gone.txt"]);
    std::fs::remove_file(repo.join("old.js")).unwrap();
    std::fs::write(repo.join("notes.txt"), "untracked").unwrap();
}

#[tokio::test]
async fn files_shows_sections_with_git_words_and_staged_marks() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = home.join("project");
    repository(&repo, &home);
    let mut screen = Screen::new(
        80,
        40,
        &repo.to_string_lossy(),
        Some(&home.to_string_lossy()),
    );
    let turn = |status: &str| {
        json!({ "id": "t2", "items": [], "status": status, "error": null,
                "input": [], "clientName": "canter-tui", "usage": null })
    };
    screen.event(json!({ "type": "turn.started", "turn": turn("inProgress") }));
    for (id, path) in [
        ("r1", Path::new("src/app.js").to_path_buf()),
        ("r2", home.join("other.txt")),
        ("r3", repo.join("src/app.js")),
    ] {
        screen.event(json!({"type":"item.completed","turnId":"t2","item":{"type":"fileRead","id":id,"path":path,"startLine":1,"content":"read","status":"completed"}}));
    }
    for (id, path, kind) in [
        ("w1", "created.txt", "add"),
        ("w2", "created.txt", "update"),
        ("w3", "removed.txt", "delete"),
    ] {
        screen.event(json!({"type":"item.completed","turnId":"t2","item":{"type":"fileChange","id":id,"changes":[{"path":repo.join(path),"kind":{"type":kind},"diff":""}],"status":"completed"}}));
    }
    screen.event(json!({ "type": "turn.completed", "turn": turn("completed") }));
    screen.draw();
    assert_eq!(screen.enter("/files"), Some(Action::Files));
    screen.ui.app_mut().show_files().await;
    screen.draw();
    assert_eq!(
        screen.popup(" Files"),
        [
            " Files  read 2 · changed 2 · git 7",
            "",
            " Read",
            "   src/app.js",
            "   ~/other.txt",
            "",
            " Changed by canter",
            "   added / modified   created.txt",
            "   deleted            removed.txt",
            "",
            " Git status · main",
            "   modified    README.md",
            "   modified    both.txt      partly staged",
            "   conflict    conflict.txt",
            "   added       docs/new.md   staged",
            "   deleted     gone.txt      staged",
            "   deleted     old.js",
            "   untracked   notes.txt",
            "",
            " Edits made through shell commands only show up under Git status.",
            " Esc close",
        ]
    );
    let rows = screen.rows();
    let title = rows
        .iter()
        .position(|row| row.starts_with(" Files"))
        .unwrap();
    assert!(
        rows[..title].iter().all(|row| !row.contains("Git status")),
        "{rows:#?}"
    );
    let bold = |(_, modifier): (Color, Modifier)| modifier.contains(Modifier::BOLD);
    let dim = |(_, modifier): (Color, Modifier)| modifier.contains(Modifier::DIM);
    assert!(bold(screen.style("Files")));
    assert!(dim(screen.style("read 2")));
    assert!(bold(screen.style("Changed by canter")));
    assert!(bold(screen.style("Git status")));
    assert_eq!(screen.style("modified    README").0, Color::Yellow);
    assert_eq!(screen.style("added       docs").0, Color::Green);
    assert_eq!(screen.style("deleted     gone").0, Color::Red);
    assert_eq!(screen.style("conflict").0, Color::Red);
    assert!(bold(screen.style("conflict")));
    assert!(dim(screen.style("untracked")));
    assert!(dim(screen.style("partly staged")));
    assert!(dim(screen.style_on("docs/new.md", "staged")));
    assert!(dim(screen.style("Edits made")));
    assert!(dim(screen.style("Esc close")));
}

#[tokio::test]
async fn files_outside_a_repository_says_so_and_none_for_empty_sections() {
    let temp = tempfile::tempdir().unwrap();
    let mut screen = Screen::new(80, 30, &temp.path().to_string_lossy(), None);
    assert_eq!(screen.enter("/files"), Some(Action::Files));
    screen.ui.app_mut().show_files().await;
    screen.draw();
    assert_eq!(
        screen.popup(" Files"),
        [
            " Files  read 0 · changed 0 · git 0",
            "",
            " Read",
            "   (none)",
            "",
            " Changed by canter",
            "   (none)",
            "",
            " Git status",
            "   (not a git repository)",
            "",
            " Edits made through shell commands only show up under Git status.",
            " Esc close",
        ]
    );
}

const LONG: &str = "a-very-long-tool-name-that-does-not-fit-on-one-row-of-the-box-at-all";

fn mcp_report() -> McpListResponse {
    let tools = |server: &str, names: &[&str]| -> Value {
        names
            .iter()
            .map(|tool| json!({ "name": format!("mcp__{server}__{tool}"), "tool": tool }))
            .collect()
    };
    serde_json::from_value(json!({ "data": [
        { "name": "context7", "connected": true, "error": null, "warnings": [],
          "tools": tools("context7", &["query-docs", "resolve-library-id"]) },
        { "name": "everything", "connected": true, "error": null,
          "warnings": ["Tool skipped: bad schema"],
          "tools": tools("everything", &["echo", "get-annotated-message", "get-env",
              "get-header-params", "get-resource-links", "get-sum", LONG, "zip"]) },
        { "name": "github", "connected": false, "error": "spawn npx ENOENT",
          "warnings": [], "tools": [] },
    ] }))
    .unwrap()
}

#[test]
fn mcp_shows_each_service_with_its_tools_laid_out_in_rows() {
    let mut screen = Screen::new(60, 30, "/work", None);
    assert_eq!(screen.enter("/mcp"), Some(Action::Mcp));
    screen.ui.app_mut().show_mcp(&mcp_report());
    screen.draw();
    let long = format!("   {}…", &LONG[..56]);
    assert_eq!(
        screen.popup(" MCP"),
        [
            " MCP  3 services · 10 tools",
            "",
            " ● context7     connected · 2 tools",
            "   query-docs  resolve-library-id",
            "",
            " ● everything   connected · 8 tools",
            "   echo  get-annotated-message  get-env  get-header-params",
            "   get-resource-links  get-sum",
            &long,
            "   zip",
            "   Tool skipped: bad schema",
            "",
            " ○ github       disconnected",
            "   spawn npx ENOENT",
            "",
            " Tools are called as mcp__<service>__<tool>.",
            " Esc close",
        ]
    );
    assert!(
        !screen
            .rows()
            .iter()
            .any(|row| row.contains("mcp__context7"))
    );
    let dim = |(_, modifier): (Color, Modifier)| modifier.contains(Modifier::DIM);
    assert_eq!(screen.style("● context7").0, Color::Green);
    assert!(screen.style("context7 ").1.contains(Modifier::BOLD));
    assert!(dim(screen.style("connected · 2")));
    assert_eq!(screen.style("○").0, Color::Red);
    assert_eq!(screen.style("disconnected").0, Color::Red);
    assert!(dim(screen.style("Tool skipped")));
    assert!(dim(screen.style("Tools are called")));
}

#[test]
fn skills_shows_global_and_project_sections_with_descriptions_cut_at_two_rows() {
    let mut screen = Screen::new(60, 30, "/work", None);
    assert_eq!(screen.enter("/skills"), Some(Action::ListSkills));
    let skills: Vec<Skill> = serde_json::from_value(json!([
        { "name": "corral", "source": "global", "modelInvocation": true,
          "path": "/home/test/.agents/skills/corral/SKILL.md",
          "description": "Start a separate agent session with corral, hand it a question and bring back its reply when it is finished, then close it." },
        { "name": "handoff", "source": "global", "modelInvocation": true,
          "path": "/home/test/.agents/skills/handoff/SKILL.md",
          "description": "记录当前会话的关键信息，为新会话做准备" },
        { "name": "release", "source": "project", "modelInvocation": false,
          "path": "/work/.agents/skills/release/SKILL.md",
          "description": "Build and install a release." },
    ]))
    .unwrap();
    screen.ui.app_mut().show_skills(skills);
    screen.draw();
    assert_eq!(
        screen.popup(" Skills"),
        [
            " Skills  3 · global 2 · project 1",
            "",
            " Global  ~/.agents/skills",
            "   corral",
            "     Start a separate agent session with corral, hand it a",
            "     question and bring back its reply when it is finished,…",
            "   handoff",
            "     记录当前会话的关键信息，为新会话做准备",
            "",
            " Project  .agents/skills",
            "   release   not for model",
            "     Build and install a release.",
            "",
            " Esc close",
        ]
    );
    let dim = |(_, modifier): (Color, Modifier)| modifier.contains(Modifier::DIM);
    assert!(screen.style("Global").1.contains(Modifier::BOLD));
    assert!(dim(screen.style("~/.agents/skills")));
    assert_eq!(screen.style("corral").0, Color::Cyan);
    assert!(dim(screen.style("Start a separate")));
    assert!(dim(screen.style("not for model")));
}

#[tokio::test]
async fn after_esc_the_conversation_has_no_rows_of_the_three() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_string_lossy().into_owned();
    for (command, title) in [
        ("/files", " Files"),
        ("/mcp", " MCP"),
        ("/skills", " Skills"),
    ] {
        let mut screen = Screen::new(60, 30, &cwd, None);
        let before = screen.rows();
        screen.enter(command);
        match command {
            "/files" => screen.ui.app_mut().show_files().await,
            "/mcp" => screen.ui.app_mut().show_mcp(&mcp_report()),
            _ => screen.ui.app_mut().show_skills(Vec::new()),
        }
        screen.draw();
        assert!(!screen.popup(title).is_empty(), "{command} opens");
        assert_eq!(screen.key(KeyCode::Esc), None, "{command}");
        assert!(screen.popup(title).is_empty(), "{command} closes");
        assert_eq!(screen.rows(), before, "{command}");
    }
}

#[tokio::test]
async fn files_git_status_names_the_upstream_and_how_far_ahead() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let origin = temp.path().join("origin");
    let ok = |dir: &Path, args: &[&str]| {
        let output = git(dir, &home, args);
        assert!(output.status.success(), "{args:?}: {output:?}");
    };
    std::fs::create_dir_all(&origin).unwrap();
    ok(&origin, &["init", "-q", "-b", "main"]);
    std::fs::write(origin.join("README.md"), "base").unwrap();
    ok(&origin, &["add", "."]);
    ok(&origin, &["commit", "-qm", "initial"]);
    let repo = temp.path().join("project");
    ok(
        temp.path(),
        &["clone", "-q", &origin.to_string_lossy(), "project"],
    );
    std::fs::write(repo.join("README.md"), "ahead").unwrap();
    ok(&repo, &["commit", "-qam", "ahead"]);
    let mut screen = Screen::new(80, 30, &repo.to_string_lossy(), None);
    assert_eq!(screen.enter("/files"), Some(Action::Files));
    screen.ui.app_mut().show_files().await;
    screen.draw();
    let popup = screen.popup(" Files");
    assert!(
        popup
            .iter()
            .any(|row| row == " Git status · main  origin/main · ahead 1"),
        "{popup:#?}"
    );
    assert!(
        screen
            .style_on("Git status", "origin/main")
            .1
            .contains(Modifier::DIM)
    );
}
