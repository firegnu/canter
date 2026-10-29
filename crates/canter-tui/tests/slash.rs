//! Slash commands in the input box on ratatui's TestBackend: the command list
//! above the input box, picking and completing a command, what each command
//! asks the caller to do, and telling commands from messages. Also Shift+Tab,
//! which cycles the permission modes `/mode` sets.

use canter_protocol::{Event, PermissionMode, SessionSnapshot, SessionUpdateParams};
use canter_tui::{ANSWER_GUARD_MS, Action, App, GoalCommand, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;

#[test]
fn goal_command_dispatches_and_goal_state_is_visible() {
    let mut screen = Screen::new();
    assert!(
        matches!(screen.enter("/goal --budget 3.5 Finish this task"), Some(Action::Goal(GoalCommand::Set(params))) if params.objective == "Finish this task" && params.budget_usd == Some(3.5))
    );
    screen.event(json!({"type":"session.goal.updated","goal":{"id":"g1","objective":"Finish this task","status":"active","budgetUsd":5.0,"cost":{"usd":1.2,"unknown":false},"createdAt":0,"updatedAt":0}}));
    screen.draw();
    assert!(
        screen.text().contains("goal $1.20/$5.00"),
        "{}",
        screen.text()
    );
    assert_eq!(screen.enter("/goal"), Some(Action::Goal(GoalCommand::Read)));
    for (action, command) in [
        ("pause", GoalCommand::Pause),
        ("resume", GoalCommand::Resume),
        ("clear", GoalCommand::Clear),
    ] {
        assert_eq!(
            screen.enter(&format!("/goal {action}")),
            Some(Action::Goal(command))
        );
    }
    for command in [
        "/goal --budget -1 task",
        "/goal --budget NaN task",
        "/goal --budget 1",
        "/goal --budget",
    ] {
        assert!(screen.enter(command).is_none());
    }
}

#[test]
fn goal_escape_between_turns_still_pauses() {
    let mut screen = Screen::new();
    screen.event(json!({"type":"session.goal.updated","goal":{"id":"g1","objective":"Continue","status":"active","budgetUsd":5.0,"cost":{"usd":0.0,"unknown":false},"createdAt":0,"updatedAt":0}}));
    screen.start_turn("t1");
    screen.draw();
    screen.event(json!({"type":"turn.completed","turn":turn_json("t1", "completed")}));
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Goal(GoalCommand::Pause))
    );
}

#[test]
fn add_dir_sends_the_path_to_the_server_and_requires_an_argument() {
    let mut screen = Screen::new();
    assert!(
        matches!(screen.enter("/add-dir ../another repo"), Some(Action::Update(params)) if params.add_directories == ["../another repo"])
    );
    assert!(screen.enter("/add-dir").is_none());
    screen.draw();
    assert!(screen.text().contains("/add-dir <path>"));
}

#[test]
fn plan_command_and_four_position_mode_cycle() {
    let mut screen = Screen::new();
    assert!(
        matches!(screen.enter("/plan"), Some(Action::Update(params)) if params.plan_mode == Some(true)),
        "/plan must enter Plan mode"
    );
    screen.event(json!({"type":"session.updated","settings":{
        "cwd":"/tmp","name":null,"model":"fake","approvalPolicy":"allowAll","planMode":true,"prePlanApprovalPolicy":"allowAll"
    }}));
    screen.draw();
    assert!(screen.text().contains("Plan"));
    assert!(
        matches!(screen.enter("/plan"), Some(Action::Update(params)) if params.plan_mode == Some(false) && params.approval_policy.is_none())
    );
}

#[test]
fn rewind_command_dispatches_and_event_removes_rendered_turns() {
    let mut screen = Screen::new();
    assert!(
        screen.enter("/rewind").is_some(),
        "rewind must dispatch a command"
    );
    for (id, text) in [("t1", "retained prompt"), ("t2", "removed prompt")] {
        let mut turn = turn_json(id, "completed");
        turn["input"] = json!([{"type":"text","text":text}]);
        screen.event(json!({"type":"turn.started","turn":turn}));
    }
    screen.draw();
    assert!(screen.text().contains("removed prompt"));
    let mut record = turn_json("rewind", "completed");
    record["input"] = json!([]);
    record["items"] = json!([{"type":"agentMessage","id":"record","text":"Restored a.txt"}]);
    screen.event(json!({"type":"session.rewound","beforeTurnId":"t2","rewindConversation":true,"record":record}));
    screen.draw();
    assert!(!screen.text().contains("removed prompt"));
    assert!(screen.text().contains("retained prompt"));
    assert!(screen.text().contains("Restored a.txt"));
    screen
        .ui
        .app_mut()
        .rewind_draft(&[canter_protocol::UserInput::Text {
            text: "removed prompt".into(),
        }]);
    screen.draw();
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::Send("removed prompt".into()))
    );
}

#[test]
fn web_fetch_shows_url_domain_approval_result_length_and_interrupts_on_escape() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    let url = "http://localhost:12345/docs?view=full#section";
    screen.event(json!({"type":"item.started","turnId":"t1","item":{
        "type":"webFetch","id":"web","url":url,"result":"","status":"inProgress"
    }}));
    screen.draw();
    assert!(screen.text().contains(&format!("Fetch {url}")));
    assert!(screen.text().contains("running"));
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Interrupt {
            turn_id: "t1".into()
        })
    );
    screen.event(json!({"type":"request.opened","request":{
        "id":"web-approval","turnId":"t1","createdAt":NOW,"kind":{"type":"approval","itemId":"web","tool":"web_fetch","arguments":{"url":url},"availableDecisions":["accept","acceptForSession","decline","cancel"]}
    }}));
    screen.event(json!({"type":"session.status","status":"waiting_approval"}));
    screen.draw();
    let text = screen.text();
    assert!(text.contains("Fetch this URL?"), "{text}");
    assert!(text.contains(url), "{text}");
    assert!(text.contains("this host and port"), "{text}");
    assert!(!text.contains("Later web_fetch calls in this session won't ask"));
    screen.event(json!({"type":"request.resolved","requestId":"web-approval","resolution":{"type":"answered","decision":"acceptForSession"},"clientName":"test"}));
    screen.event(json!({"type":"session.status","status":"working"}));
    screen.event(json!({"type":"item.completed","turnId":"t1","item":{
        "type":"webFetch","id":"web","url":url,"result":"fetched text","status":"completed"
    }}));
    screen.draw();
    let text = screen.text();
    assert!(text.contains("12 B"), "{text}");
    assert!(
        !text.contains("fetched text"),
        "the page is for the model: {text}"
    );
    assert!(text.contains("this host and port"), "{text}");
    assert!(!text.contains("don't ask again for this tool"), "{text}");
}

#[test]
fn mcp_command_dispatches_instead_of_sending_a_message() {
    let mut screen = Screen::new();
    assert!(matches!(screen.enter("/mcp"), Some(Action::Mcp)));
}

#[test]
fn web_fetch_displays_the_resolved_url_and_specific_grant_authority() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    let url = "http://bücher.example";
    let resolved = "http://xn--bcher-kva.example/";
    screen.event(json!({"type":"item.started","turnId":"t1","item":{
        "type":"webFetch","id":"web","url":url,"resolvedUrl":resolved,"result":"","status":"inProgress"
    }}));
    screen.draw();
    assert!(screen.text().contains(resolved), "{}", screen.text());
    screen.event(json!({"type":"request.opened","request":{
        "id":"web-approval","turnId":"t1","createdAt":NOW,"kind":{"type":"approval","itemId":"web","tool":"web_fetch","arguments":{"url":url,"resolvedUrl":resolved},"availableDecisions":["accept","acceptForSession","decline","cancel"]}
    }}));
    screen.event(json!({"type":"session.status","status":"waiting_approval"}));
    screen.draw();
    let text = screen.text();
    assert!(text.contains(url) && text.contains(resolved), "{text}");
    assert!(
        text.contains("Later fetches to xn--bcher-kva.example:80"),
        "{text}"
    );
}

#[test]
fn review_command_dispatches_and_hides_assembled_prompt() {
    let mut screen = Screen::new();
    assert!(
        screen.enter("/review").is_some(),
        "review must dispatch a command"
    );
    let mut turn = turn_json("review", "inProgress");
    turn["review"] = json!("Review: PR #123 Fix tests");
    turn["input"] = json!([{"type":"text","text":"PRIVATE ASSEMBLED REVIEW PROMPT\ndiff --git"}]);
    screen.event(json!({"type":"turn.started", "turn":turn}));
    screen.draw();
    assert!(screen.text().contains("Review: PR #123 Fix tests"));
    assert!(!screen.text().contains("PRIVATE ASSEMBLED"));
}

/// Every command, in the list's order, with what the list says about it.
const COMMANDS: [(&str, &str); 22] = [
    ("/compact", "Summarize the conversation to free up context"),
    ("/context", "Show context contents and token estimates"),
    ("/copy", "Copy the last answer, or code block N of it"),
    ("/diff", "Show uncommitted git changes"),
    ("/exit", "Quit canter"),
    ("/export", "Save or copy the conversation as Markdown"),
    ("/files", "List session files and git status"),
    ("/help", "Show available commands"),
    ("/mcp", "Show MCP connections and tools"),
    ("/mode", "Change the permission mode"),
    ("/model", "Change the model"),
    ("/new", "Start a new session"),
    ("/plan", "Enter or leave read-only Plan mode"),
    ("/resume", "Switch to another session"),
    ("/skills", "List this session's skills"),
    ("/think", "Change thinking effort (none, low, high, max)"),
    ("/usage", "Show this session's token usage and cost"),
    (
        "/btw",
        "Ask one question without adding it to the conversation",
    ),
    ("/handoff", "Draft a handoff note in a new session"),
    (
        "/review",
        "Review uncommitted changes or a PR number/URL (read-only)",
    ),
    ("/rewind", "Rewind conversation, files, or both"),
    ("/statusline", "Choose and order what the status line shows"),
];

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    /// The time frames are drawn and keys pressed at.
    now: i64,
}

impl Screen {
    fn new() -> Self {
        Self::sized(WIDTH, HEIGHT)
    }

    fn sized(width: u16, height: u16) -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let ui = Ui::new(TestBackend::new(width, height), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            now: NOW,
        };
        screen.settings("alwaysAsk");
        screen
    }

    fn settings(&mut self, mode: &str) {
        self.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model", "approvalPolicy": mode,
            },
        }));
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

    fn start_turn(&mut self, turn: &str) {
        self.event(json!({ "type": "turn.started", "turn": turn_json(turn, "inProgress") }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    /// The model wants to run `command` and asks first: the approval box opens.
    fn ask(&mut self, turn: &str, command: &str) {
        self.event(json!({
            "type": "item.started", "turnId": turn,
            "item": command_json(command, "inProgress"),
        }));
        self.event(json!({
            "type": "request.opened",
            "request": {
                "id": "r1", "turnId": turn, "createdAt": NOW,
                "kind": {
                    "type": "approval", "itemId": "c1", "tool": "shell",
                    "arguments": { "command": command },
                    "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
                },
            },
        }));
        self.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    }

    /// Another terminal allows the request, and the command runs.
    fn allowed_elsewhere(&mut self, turn: &str, command: &str) {
        self.event(json!({
            "type": "request.resolved", "requestId": "r1",
            "resolution": { "type": "answered", "decision": "accept" },
            "clientName": "canter-tui b",
        }));
        self.event(json!({ "type": "session.status", "status": "working" }));
        self.event(json!({
            "type": "item.completed", "turnId": turn, "item": command_json(command, "completed"),
        }));
    }

    fn wait(&mut self, ms: i64) {
        self.now += ms;
    }

    fn draw(&mut self) {
        self.ui.draw(self.now).unwrap();
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), self.now)
    }

    /// Shift+Tab, as crossterm reports it.
    fn shift_tab(&mut self) -> Option<Action> {
        self.ui.app_mut().handle_key(
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
            self.now,
        )
    }

    /// A question from the model: the question box opens.
    fn question(&mut self, turn: &str) {
        self.event(json!({
            "type": "request.opened",
            "request": {
                "id": "r1", "turnId": turn, "createdAt": NOW,
                "kind": {
                    "type": "userInput", "itemId": "q1", "question": "Pick storage",
                    "options": [
                        { "label": "SQLite", "description": "Local files" },
                        { "label": "PostgreSQL", "description": "Shared server" },
                    ],
                },
            },
        }));
        self.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    }

    /// Another terminal answers the question.
    fn answered_elsewhere(&mut self) {
        self.event(json!({
            "type": "request.resolved", "requestId": "r1",
            "resolution": { "type": "userInput", "answer": { "type": "option", "label": "SQLite" } },
            "clientName": "canter-tui b",
        }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    /// Types `text` without pressing Enter; typing alone asks for nothing.
    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            assert_eq!(self.key(KeyCode::Char(c)), None, "typing {c:?}");
        }
    }

    /// Types `text` and presses Enter on the screen that shows it.
    fn enter(&mut self, text: &str) -> Option<Action> {
        self.type_text(text);
        self.draw();
        self.key(KeyCode::Enter)
    }

    fn lines(&self) -> Vec<String> {
        rows(self.ui.backend().buffer())
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// The rows right above the input box: where the command list shows.
    fn above_box(&self, count: usize) -> Vec<String> {
        let lines = self.lines();
        let top = lines
            .iter()
            .rposition(|row| row.starts_with('╭'))
            .unwrap_or_else(|| panic!("no input box:\n{}", self.text()));
        lines[top.saturating_sub(count)..top].to_vec()
    }

    /// The first of the two info rows at the bottom: model, thinking and
    /// permission mode.
    fn mode_row(&self) -> String {
        let lines = self.lines();
        lines[lines.len() - 2].clone()
    }

    /// The input box's first row, without the border.
    fn input(&self) -> String {
        let lines = self.lines();
        let top = lines.iter().rposition(|row| row.starts_with('╭')).unwrap();
        lines[top + 1].trim_matches('│').trim().to_owned()
    }

    /// The command list is not on screen.
    fn no_list(&self) -> bool {
        COMMANDS
            .iter()
            .all(|(_, about)| !self.text().contains(about))
    }
}

fn turn_json(id: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "跑一下" }],
        "clientName": "canter-tui", "usage": null,
    })
}

fn command_json(command: &str, status: &str) -> Value {
    json!({
        "type": "commandExecution", "id": "c1", "command": command, "cwd": "/work",
        "status": status, "aggregatedOutput": "", "exitCode": null, "durationMs": null,
    })
}

fn set_mode(mode: PermissionMode) -> Option<Action> {
    Some(Action::Update(SessionUpdateParams {
        approval_policy: Some(mode),
        ..SessionUpdateParams::default()
    }))
}

/// The buffer's rows, trailing spaces trimmed. Cells hidden under a wide
/// character are skipped.
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

#[test]
fn skills_show_names_by_source_with_manual_marker_and_descriptions_below() {
    for width in [90, 40] {
        let mut screen = Screen::sized(width, HEIGHT);
        assert_eq!(screen.enter("/skills"), Some(Action::ListSkills));
        let skills = serde_json::from_value(json!([
            {"name":"manual", "description":"Review all interface states", "source":"global", "path":"/home/.agents/skills/manual/SKILL.md", "modelInvocation":false},
            {"name":"tdd", "description":"Write a failing test before changing implementation and verify the result", "source":"project", "path":"/work/.agents/skills/tdd/SKILL.md", "modelInvocation":true}
        ])).unwrap();
        screen.ui.app_mut().show_skills(skills);
        screen.draw();
        let lines = screen.lines();
        let global = lines
            .iter()
            .position(|line| line == " Global  ~/.agents/skills")
            .unwrap_or_else(|| panic!("{}", screen.text()));
        assert_eq!(lines[global + 1], "   manual   not for model");
        assert_eq!(lines[global + 2], "     Review all interface states");
        assert_eq!(lines[global + 3], "");
        assert_eq!(lines[global + 4], " Project  .agents/skills");
        assert_eq!(lines[global + 5], "   tdd");
        if width == 40 {
            assert_eq!(lines[global + 6], "     Write a failing test before");
            assert_eq!(
                lines[global + 7],
                "     changing implementation and verify…"
            );
            assert!(!screen.text().contains("the result"));
        } else {
            assert_eq!(
                lines[global + 6],
                "     Write a failing test before changing implementation and verify the result"
            );
        }
    }
}

#[test]
fn skills_empty_screen_explains_both_directories() {
    let mut screen = Screen::new();
    assert_eq!(screen.enter("/skills"), Some(Action::ListSkills));
    screen.ui.app_mut().show_skills(Vec::new());
    screen.draw();
    let shown = screen.text();
    assert!(shown.contains("No skills in this session."), "{shown}");
    assert!(
        shown.contains("~/.agents/skills/<name>/SKILL.md"),
        "{shown}"
    );
    assert!(
        shown.contains("<project root>/.agents/skills/<name>/SKILL.md"),
        "{shown}"
    );
    assert!(shown.contains("reopen the session"), "{shown}");
}

/// A `/` as the first character lists every command with a line about it,
/// right above the input box, the first one selected. What follows the `/`
/// narrows the list; deleting the `/` closes it.
#[test]
fn a_slash_lists_the_commands_above_the_input_box() {
    // Leave room for the command list, including /plan.
    let mut screen = Screen::sized(WIDTH, HEIGHT + 4);
    screen.type_text("/");
    screen.draw();
    let list = screen.above_box(COMMANDS.len());
    let shown = screen.text();
    for (row, (name, about)) in list.iter().zip(COMMANDS) {
        assert!(
            row.contains(name) && row.contains(about),
            "{name} on {row:?}:\n{shown}"
        );
    }
    assert!(list[0].starts_with(" › /compact"), "{shown}");
    assert!(list[1].starts_with("   /context"), "{shown}");

    screen.type_text("e");
    screen.draw();
    let shown = screen.text();
    let left = screen.above_box(2);
    assert!(
        left[0].starts_with(" › /exit") && left[1].starts_with("   /export"),
        "only /exit and /export are left:\n{shown}"
    );
    assert!(!shown.contains("/compact"), "{shown}");

    assert_eq!(screen.key(KeyCode::Backspace), None);
    assert_eq!(screen.key(KeyCode::Backspace), None);
    screen.draw();
    assert!(screen.no_list(), "{}", screen.text());
}

/// Up and Down move the selection; Tab, or Enter on a name not typed in
/// full, completes the input to the selected command; Enter then runs it.
#[test]
fn up_down_tab_and_enter_work_on_the_list() {
    // The whole list, as above.
    let mut screen = Screen::sized(WIDTH, HEIGHT + 4);
    screen.type_text("/");
    screen.draw();
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Up), None);
    screen.draw();
    let shown = screen.text();
    assert!(
        screen.above_box(COMMANDS.len())[1].starts_with(" › /context"),
        "{shown}"
    );

    assert_eq!(screen.key(KeyCode::Tab), None);
    screen.draw();
    assert_eq!(screen.input(), "› /context", "{}", screen.text());
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Context));

    // Enter on `/` alone completes the command selected, like Tab.
    let mut screen = Screen::new();
    screen.type_text("/");
    screen.draw();
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Enter), None);
    screen.draw();
    assert_eq!(screen.input(), "› /context", "{}", screen.text());
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Context));
}

/// The user's decision: Enter runs a command only when its whole name is
/// typed. `/` alone or the start of a name only completes the command
/// selected (`/compact` comes first), so a quick `/` and Enter does not
/// compact the session; the next Enter does.
#[test]
fn enter_runs_only_a_whole_command_name() {
    let mut screen = Screen::new();
    assert_eq!(screen.enter("/"), None, "/ and Enter runs nothing");
    screen.draw();
    assert_eq!(screen.input(), "› /compact", "{}", screen.text());
    assert!(screen.no_list(), "{}", screen.text());

    let mut screen = Screen::new();
    assert_eq!(screen.enter("/co"), None, "/co and Enter runs nothing");
    screen.draw();
    assert_eq!(screen.input(), "› /compact", "{}", screen.text());
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Compact));
}

/// While the list is open, Esc closes only the list; with the list closed,
/// Esc interrupts the turn as before.
#[test]
fn esc_closes_the_list_before_it_interrupts() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.type_text("/");
    screen.draw();
    assert!(!screen.no_list());
    assert_eq!(screen.key(KeyCode::Esc), None, "Esc only closes the list");
    screen.draw();
    assert!(screen.no_list(), "{}", screen.text());
    assert_eq!(screen.input(), "› /", "the input stays");
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Interrupt {
            turn_id: "t1".into()
        })
    );
}

/// Each command, typed in full, asks the caller for its action; the input box
/// is empty afterwards.
#[test]
fn each_command_asks_for_its_action() {
    for (text, action) in [
        ("/exit", Some(Action::Quit)),
        ("/new", Some(Action::NewSession)),
        ("/resume", Some(Action::PickSession)),
        ("/compact", Some(Action::Compact)),
        ("/usage", Some(Action::Usage)),
        ("/context", Some(Action::Context)),
        ("/files", Some(Action::Files)),
        ("/mode allow-all", set_mode(PermissionMode::AllowAll)),
        ("/mode always-ask", set_mode(PermissionMode::AlwaysAsk)),
        (
            "/mode ask-for-execution",
            set_mode(PermissionMode::AskForExecution),
        ),
    ] {
        let mut screen = Screen::new();
        assert_eq!(screen.enter(text), action, "{text}");
        screen.draw();
        assert!(screen.input().contains("Type a message"), "{text}");
        assert!(screen.no_list(), "{text}");
    }
}

#[test]
fn usage_displays_models_total_last_turn_and_unknown_costs() {
    let mut screen = Screen::new();
    assert_eq!(screen.enter("/usage"), Some(Action::Usage));
    let report = serde_json::from_value(json!({
        "data":[
            {"model":"deepseek-flash","cachedInputTokens":800,"uncachedInputTokens":200,"outputTokens":100,"cost":{"usd":0.001,"unknown":false}},
            {"model":"unpriced","cachedInputTokens":20,"uncachedInputTokens":30,"outputTokens":40,"cost":{"usd":0.0,"unknown":true}}
        ],
        "total":{"cachedInputTokens":820,"uncachedInputTokens":230,"outputTokens":140,"cost":{"usd":0.001,"unknown":true}},
        "lastTurnCost":{"usd":0.12,"unknown":false}
    })).unwrap();
    screen.ui.app_mut().show_usage(&report);
    screen.draw();
    let rows = screen
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("  "))
        .map(|line| {
            line.split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            vec![
                "Model", "Cache", "hit", "Cache", "miss", "Output", "Cost", "(USD)"
            ],
            vec!["deepseek-flash", "800", "200", "100", "<$0.01"],
            vec!["unpriced", "20", "30", "40", "unknown"],
            vec!["Total", "820", "230", "140", "<$0.01+?"],
            vec!["Last", "turn:", "$0.12"],
        ]
    );
}

/// /help opens the Help box above the input box with every command, prints
/// nothing in the conversation, and sends nothing.
#[test]
fn help_shows_the_commands_in_a_box() {
    // The shortcut sections come first: tall enough for the whole box.
    let mut screen = Screen::sized(WIDTH, 100);
    assert_eq!(screen.enter("/help"), None);
    screen.draw();
    let shown = screen.text();
    let box_top = screen.lines().iter().rposition(|row| row.starts_with('╭'));
    let title = screen.lines().iter().position(|row| row == " Help");
    assert!(title.is_some(), "{shown}");
    for (name, about) in COMMANDS {
        let row = screen
            .lines()
            .iter()
            .position(|row| row.contains(name) && row.contains(about));
        assert!(
            row.is_some_and(|row| Some(row) > title && Some(row) < box_top),
            "{name} in the box:\n{shown}"
        );
    }
    assert!(screen.input().contains("Type a message"), "{shown}");
}

/// A first word that starts with `/` and has no other `/` is a command; an
/// unknown one is not sent, the conversation says so, and the draft stays.
#[test]
fn an_unknown_command_is_not_sent() {
    for text in ["/foo", "/foo bar"] {
        let mut screen = Screen::new();
        assert_eq!(screen.enter(text), None, "{text}");
        screen.draw();
        let shown = screen.text();
        assert!(shown.contains("Unknown command: /foo"), "{text}:\n{shown}");
        assert_eq!(screen.input(), format!("› {text}"), "{shown}");
    }
}

/// A first word with another `/` in it, like a path, is a message: no list,
/// no unknown command, and Enter sends it.
#[test]
fn a_path_is_sent_as_a_message() {
    let mut screen = Screen::new();
    screen.type_text("/new");
    screen.draw();
    assert!(!screen.no_list(), "/new could still be a command");
    screen.type_text("/");
    screen.draw();
    assert!(screen.no_list(), "{}", screen.text());
    assert_eq!(
        screen.enter("notes.md 看看这个"),
        Some(Action::Send("/new/notes.md 看看这个".into()))
    );

    let mut screen = Screen::new();
    assert_eq!(
        screen.enter("/tmp/a.txt 看看这个"),
        Some(Action::Send("/tmp/a.txt 看看这个".into()))
    );
    screen.draw();
    assert!(!screen.text().contains("Unknown command"));
}

/// /mode alone offers the three modes, the current one marked and selected;
/// Enter picks the one selected, Esc picks none.
#[test]
fn mode_alone_offers_the_three_modes() {
    let mut screen = Screen::new();
    screen.settings("askForExecution");
    assert_eq!(screen.enter("/mode"), None);
    screen.draw();
    let shown = screen.text();
    let choices = screen.above_box(3);
    assert!(choices[0].contains("always-ask"), "{shown}");
    assert!(
        choices[1].starts_with(" › ask-for-execution (current)"),
        "{shown}"
    );
    assert!(choices[2].contains("allow-all"), "{shown}");
    assert_eq!(shown.matches("(current)").count(), 1, "{shown}");

    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(
        screen.key(KeyCode::Enter),
        set_mode(PermissionMode::AllowAll)
    );
    screen.draw();
    assert!(!screen.text().contains("allow-all"), "the choice closed");

    assert_eq!(screen.enter("/mode"), None);
    screen.draw();
    assert_eq!(screen.key(KeyCode::Esc), None);
    screen.draw();
    assert!(!screen.text().contains("always-ask"), "{}", screen.text());
    assert_eq!(screen.key(KeyCode::Enter), None, "nothing to choose");
}

/// /mode with a name it does not know changes nothing and says so.
#[test]
fn mode_with_an_unknown_name_only_says_so() {
    let mut screen = Screen::new();
    assert_eq!(screen.enter("/mode yolo"), None);
    screen.draw();
    let shown = screen.text();
    assert!(shown.contains("Unknown mode: yolo"), "{shown}");
    assert!(
        shown.contains("test-model · think high · Always ask"),
        "{shown}"
    );
}

/// While the approval box is open, `/` does not open the list.
#[test]
fn no_list_while_the_approval_box_is_open() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.type_text("/");
    screen.draw();
    assert!(!screen.no_list(), "the input box lists the commands");
    assert_eq!(screen.key(KeyCode::Backspace), None);
    screen.ask("t1", "cargo test");
    screen.draw();
    screen.wait(ANSWER_GUARD_MS);
    screen.draw();
    assert_eq!(screen.key(KeyCode::Char('/')), None);
    screen.draw();
    let shown = screen.text();
    assert!(shown.contains("Run this command?"), "{shown}");
    assert!(screen.no_list(), "{shown}");
}

/// P1-14: right after the approval box gives the input box back, Enter is
/// meant for the box, so it does not run the command in the draft either.
#[test]
fn enter_right_after_the_box_closes_does_not_run_a_command() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.draw();
    screen.type_text("/exit");
    screen.ask("t1", "printf A > first-proof.txt");
    screen.draw();
    screen.wait(ANSWER_GUARD_MS);
    screen.draw();

    screen.allowed_elsewhere("t1", "printf A > first-proof.txt");
    screen.draw();
    assert_eq!(screen.input(), "› /exit", "{}", screen.text());
    assert_eq!(
        screen.key(KeyCode::Enter),
        None,
        "Enter was meant for the box"
    );

    screen.wait(ANSWER_GUARD_MS);
    screen.draw();
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Quit));
}

/// Cross review 1: keys act on the frame on screen when they came in. Keys
/// pressed while the input box showed `/mode` and queued before the next
/// frame do not move or confirm the choice that `/mode` opened.
#[test]
fn keys_queued_before_the_mode_choice_shows_do_not_choose() {
    let mut screen = Screen::new();
    screen.type_text("/mode");
    screen.draw();
    for code in [KeyCode::Enter, KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        assert_eq!(screen.key(code), None, "{code:?} before the choice showed");
    }
    screen.draw();
    let shown = screen.text();
    assert!(shown.contains("Permission mode"), "{shown}");
    assert!(
        screen.above_box(3)[0].starts_with(" › always-ask (current)"),
        "the Downs were dropped:\n{shown}"
    );
    // On screen now: keys choose.
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(
        screen.key(KeyCode::Enter),
        set_mode(PermissionMode::AllowAll)
    );
}

/// Cross review 1: in a window too short to show even one mode, keys neither
/// move nor confirm the choice; Esc still closes it.
#[test]
fn a_mode_choice_with_no_room_takes_no_choice() {
    let mut screen = Screen::sized(80, 5);
    assert_eq!(screen.enter("/mode"), None);
    screen.draw();
    let shown = screen.text();
    assert!(!shown.contains("always-ask"), "no room:\n{shown}");
    for code in [KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
        assert_eq!(screen.key(code), None, "{code:?} on a choice not shown");
        screen.draw();
    }
    assert_eq!(screen.key(KeyCode::Esc), None);
    assert_eq!(
        screen.enter("hi"),
        Some(Action::Send("hi".into())),
        "the choice is closed"
    );
}

/// Cross review 2: the first word is found after leading blanks, so a
/// command typed after a space is still a command, never a message. A
/// message keeps its text as typed.
#[test]
fn a_command_after_leading_blanks_is_still_a_command() {
    let mut screen = Screen::new();
    assert_eq!(
        screen.enter(" /mode allow-all"),
        set_mode(PermissionMode::AllowAll)
    );

    let mut screen = Screen::new();
    assert_eq!(screen.enter("  /foo"), None);
    screen.draw();
    assert!(screen.text().contains("Unknown command: /foo"));
    assert_eq!(screen.input(), "›   /foo", "the draft stays");

    let mut screen = Screen::new();
    assert_eq!(
        screen.enter(" /tmp/a.txt 看看这个"),
        Some(Action::Send(" /tmp/a.txt 看看这个".into()))
    );
}

/// Cross review 3: a command that takes no arguments, followed by more text
/// (also pasted text after a newline), is not run and not sent; the
/// conversation says why and the draft stays.
#[test]
fn text_after_a_command_without_arguments_is_refused() {
    for name in [
        "compact", "context", "files", "exit", "help", "new", "resume",
    ] {
        let mut screen = Screen::new();
        assert_eq!(screen.enter(&format!("/{name} now")), None, "/{name}");
        screen.draw();
        let shown = screen.text();
        assert!(
            shown.contains(&format!("/{name} takes no arguments")),
            "{shown}"
        );
        assert_eq!(screen.input(), format!("› /{name} now"), "{shown}");
    }

    let mut screen = Screen::new();
    screen
        .ui
        .app_mut()
        .handle_paste("/compact\nThis is copied documentation, not a request to compact.");
    screen.draw();
    assert_eq!(screen.key(KeyCode::Enter), None, "the paste is not run");
    screen.draw();
    assert!(screen.text().contains("/compact takes no arguments"));
    assert_eq!(screen.input(), "› /compact");
}

/// Cross review 4: after Enter completes `/` to `/compact`, a second Enter
/// pressed before the completion is on screen was meant for what was there
/// before, so it does not compact. Enter once it shows does.
#[test]
fn enter_before_the_completion_shows_does_not_run_it() {
    let mut screen = Screen::new();
    screen.type_text("/");
    screen.draw();
    assert_eq!(screen.key(KeyCode::Enter), None, "completes");
    assert_eq!(
        screen.key(KeyCode::Enter),
        None,
        "the completion is not on screen yet"
    );
    screen.draw();
    assert_eq!(screen.input(), "› /compact", "{}", screen.text());
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Compact));
}

/// Cross review 5: the modes say that read-only commands run without
/// asking, in full at 80 columns.
#[test]
fn the_modes_say_read_only_commands_do_not_ask() {
    for mode in ["alwaysAsk", "askForExecution", "allowAll"] {
        let mut screen = Screen::new();
        screen.settings(mode);
        assert_eq!(screen.enter("/mode"), None);
        screen.draw();
        let choices = screen.above_box(3);
        let shown = screen.text();
        assert!(
            choices[0].ends_with("Ask before edits and non-read-only commands"),
            "{mode}:\n{shown}"
        );
        assert!(
            choices[1].ends_with("Edit freely; ask before non-read-only commands"),
            "{mode}:\n{shown}"
        );
        assert!(choices[2].ends_with("Never ask"), "{mode}:\n{shown}");
    }
}

/// Cross review 2: the list and its completion also find the first word
/// after leading blanks.
#[test]
fn the_list_shows_after_leading_blanks() {
    let mut screen = Screen::new();
    screen.type_text(" /co");
    screen.draw();
    assert!(
        screen.above_box(3)[0].starts_with(" › /compact"),
        "{}",
        screen.text()
    );
    assert_eq!(screen.key(KeyCode::Enter), None, "completes");
    screen.draw();
    assert_eq!(screen.input(), "› /compact");
}

/// Shift+Tab moves on from the mode on screen when it was pressed: until the
/// change shows, more presses ask for the same mode, never two steps on.
#[test]
fn shift_tab_on_an_unchanged_screen_moves_one_mode() {
    let mut screen = Screen::new();
    screen.draw();
    let next = set_mode(PermissionMode::AskForExecution);
    assert_eq!(screen.shift_tab(), next);
    assert_eq!(screen.shift_tab(), next, "pressed on the same frame");
    screen.draw();
    assert_eq!(screen.shift_tab(), next, "the change has not come back yet");
    screen.settings("askForExecution");
    assert_eq!(screen.shift_tab(), next, "the change is not drawn yet");

    screen.draw();
    let shown = screen.text();
    assert!(
        shown.contains("test-model · think high · Ask for execution"),
        "{shown}"
    );
    assert_eq!(screen.shift_tab(), set_mode(PermissionMode::AllowAll));
}

/// Something that happens on a screen, such as a box opening.
type Step = fn(&mut Screen);

/// Shift+Tab changes the mode only from the input box: not while the
/// approval box, the question box, a choice or the command list is open.
#[test]
fn shift_tab_does_nothing_while_a_box_a_choice_or_the_list_is_open() {
    let opens: [(&str, Step); 4] = [
        ("Run this command?", |screen| screen.ask("t1", "cargo test")),
        ("Pick storage", |screen| screen.question("t1")),
        ("Permission mode", |screen| {
            assert_eq!(screen.enter("/mode"), None)
        }),
        ("Summarize the conversation", |screen| screen.type_text("/")),
    ];
    for (open_shows, open) in opens {
        let mut screen = Screen::new();
        screen.start_turn("t1");
        screen.draw();
        assert_eq!(
            screen.shift_tab(),
            set_mode(PermissionMode::AskForExecution),
            "the input box, before {open_shows:?}"
        );
        open(&mut screen);
        screen.draw();
        // Past the boxes' guard: only being open keeps the mode.
        screen.wait(ANSWER_GUARD_MS);
        screen.draw();
        let shown = screen.text();
        assert!(shown.contains(open_shows), "{shown}");
        assert_eq!(screen.shift_tab(), None, "{open_shows:?}:\n{shown}");
    }
}

/// P1-14: for 300 ms after the approval or question box gives the input box
/// back, Shift+Tab was meant for the box and changes nothing; after that it does.
#[test]
fn shift_tab_right_after_a_box_closes_does_nothing() {
    let boxes: [(&str, Step, Step); 2] = [
        (
            "approval",
            |screen| screen.ask("t1", "cargo test"),
            |screen| screen.allowed_elsewhere("t1", "cargo test"),
        ),
        (
            "question",
            |screen| screen.question("t1"),
            Screen::answered_elsewhere,
        ),
    ];
    for (what, open, close) in boxes {
        let mut screen = Screen::new();
        screen.start_turn("t1");
        open(&mut screen);
        screen.draw();
        screen.wait(ANSWER_GUARD_MS);
        screen.draw();
        close(&mut screen);
        screen.draw();
        assert!(
            screen.input().contains("Type a message"),
            "{what}:\n{}",
            screen.text()
        );
        assert_eq!(screen.shift_tab(), None, "the {what} box just closed");

        screen.wait(ANSWER_GUARD_MS);
        screen.draw();
        assert_eq!(
            screen.shift_tab(),
            set_mode(PermissionMode::AskForExecution),
            "after the {what} box's guard"
        );
    }
}

/// While the model answers, Shift+Tab changes the mode too.
#[test]
fn shift_tab_works_while_the_model_answers() {
    let mut screen = Screen::new();
    screen.start_turn("t1");
    screen.draw();
    assert!(screen.text().contains("Working"), "{}", screen.text());
    assert_eq!(
        screen.shift_tab(),
        set_mode(PermissionMode::AskForExecution)
    );
}

/// Cross review 1: while the first info row is too narrow to show the mode,
/// Shift+Tab changes nothing, also after a change came back and was drawn
/// still cut off: it never moves on to Allow all from modes not seen.
#[test]
fn shift_tab_does_nothing_while_the_mode_is_cut_off() {
    let mut screen = Screen::sized(24, 12);
    screen.draw();
    let info = screen.mode_row();
    assert!(info.starts_with(" test-model · think"), "{}", screen.text());
    assert!(!info.contains("Always ask"), "the mode is cut off: {info}");
    assert_eq!(screen.shift_tab(), None, "no mode on screen: {info}");

    screen.settings("askForExecution");
    screen.draw();
    assert_eq!(screen.mode_row(), info, "still cut off");
    assert_eq!(screen.shift_tab(), None, "not on to Allow all");
}

/// Cross review 1: the mode is in the first of the two info rows. In a
/// window with room for that row only, Shift+Tab works; in one too short
/// for it, the mode is not on screen and Shift+Tab changes nothing.
#[test]
fn shift_tab_needs_the_first_info_row_drawn() {
    let mut screen = Screen::sized(80, 5);
    screen.draw();
    let shown = screen.text();
    assert!(
        screen.lines()[4].contains("test-model · think high · Always ask"),
        "{shown}"
    );
    assert!(!shown.contains("/work"), "the second row is cut: {shown}");
    assert_eq!(
        screen.shift_tab(),
        set_mode(PermissionMode::AskForExecution)
    );

    let mut screen = Screen::sized(80, 4);
    screen.draw();
    let shown = screen.text();
    assert!(!shown.contains("Always ask"), "{shown}");
    assert_eq!(screen.shift_tab(), None, "{shown}");
}

/// Cross review 2: Shift+Tab pressed on a frame that showed a choice or the
/// command list changes nothing, even when Esc pressed before it on the
/// same frame already closed them. On the next frame it works.
#[test]
fn shift_tab_on_a_frame_with_a_choice_or_the_list_does_nothing() {
    let opens: [(&str, Step); 2] = [
        ("Thinking effort", |screen| {
            assert_eq!(screen.enter("/think"), None)
        }),
        ("Summarize the conversation", |screen| screen.type_text("/")),
    ];
    for (open_shows, open) in opens {
        let mut screen = Screen::new();
        screen.draw();
        open(&mut screen);
        screen.draw();
        let shown = screen.text();
        assert!(shown.contains(open_shows), "{shown}");
        assert_eq!(screen.key(KeyCode::Esc), None);
        assert_eq!(
            screen.shift_tab(),
            None,
            "{open_shows:?} still on screen:\n{shown}"
        );

        screen.draw();
        assert!(!screen.text().contains(open_shows), "{}", screen.text());
        assert_eq!(
            screen.shift_tab(),
            set_mode(PermissionMode::AskForExecution),
            "{open_shows:?} closed on screen"
        );
    }
}

#[tokio::test]
async fn files_lists_deduplicated_history_and_isolated_git_status() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let repo = home.join("project");
    let cwd = repo.join("src");
    std::fs::create_dir_all(&cwd).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(cwd.join("tracked.txt"), "original").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "initial"]);
    std::fs::write(cwd.join("tracked.txt"), "modified").unwrap();
    std::fs::write(cwd.join("untracked space.txt"), "new").unwrap();
    // A configured fsmonitor must never be executed by /files.
    git(&["config", "core.fsmonitor", "touch forbidden-marker"]);
    git(&[
        "config",
        "filter.probe.clean",
        "touch forbidden-filter; cat",
    ]);
    std::fs::write(repo.join(".gitattributes"), "*.txt filter=probe\n").unwrap();
    let mut snapshot = SessionSnapshot::empty(SESSION);
    snapshot.settings.cwd = cwd.to_string_lossy().into_owned();
    let mut screen = Screen::sized(130, 32);
    screen.ui = Ui::new(
        TestBackend::new(130, 32),
        App::new(snapshot, Some(home.to_string_lossy().into_owned()), NOW),
    )
    .unwrap();
    screen.seq = 0;
    screen.start_turn("t1");
    for (id, path) in [
        ("r1", cwd.join("tracked.txt")),
        ("r2", std::path::PathBuf::from("tracked.txt")),
        ("r3", home.join("other.txt")),
    ] {
        screen.event(json!({"type":"item.completed","turnId":"t1","item":{"type":"fileRead","id":id,"path":path,"startLine":1,"content":"read","status":"completed"}}));
    }
    for (id, kind) in [("w1", "add"), ("w2", "update"), ("w3", "update")] {
        screen.event(json!({"type":"item.completed","turnId":"t1","item":{"type":"fileChange","id":id,"changes":[{"path":cwd.join("created.txt"),"kind":{"type":kind},"diff":""}],"status":"completed"}}));
    }
    screen.event(json!({"type":"item.completed","turnId":"t1","item":{"type":"fileChange","id":"delete","changes":[{"path":cwd.join("gone.txt"),"kind":{"type":"delete"},"diff":""}],"status":"completed"}}));
    screen.ui.app_mut().show_files().await;
    screen.draw();
    let shown = screen.text();
    assert!(
        shown.contains(" Files  read 2 · changed 2 · git 3"),
        "{shown}"
    );
    let shown = &shown[shown.find(" Files  read").unwrap()..];
    assert!(shown.contains("Changed by canter"), "{shown}");
    assert_eq!(shown.matches("~/other.txt").count(), 1, "{shown}");
    assert_eq!(shown.matches("created.txt").count(), 1, "{shown}");
    assert!(shown.contains("added / modified   created.txt"), "{shown}");
    assert!(shown.contains("deleted            gone.txt"), "{shown}");
    assert!(shown.find("tracked.txt").unwrap() < shown.find("~/other.txt").unwrap());
    assert!(shown.contains("Git status · main"), "{shown}");
    assert!(shown.contains("modified    tracked.txt"), "{shown}");
    assert!(shown.contains("untracked   untracked space.txt"), "{shown}");
    assert!(
        shown.contains("Edits made through shell commands"),
        "{shown}"
    );
    assert!(!repo.join("forbidden-marker").exists());
    assert!(!repo.join("forbidden-filter").exists());
    screen.ui.backend_mut().resize(40, 32);
    screen.draw();
    assert!(
        screen
            .lines()
            .iter()
            .any(|line| line.contains("Edits made") && line.ends_with('…')),
        "{}",
        screen.text()
    );

    let plain = temp.path().join("plain");
    std::fs::create_dir(&plain).unwrap();
    let mut snapshot = SessionSnapshot::empty("empty");
    snapshot.settings.cwd = plain.to_string_lossy().into_owned();
    let mut screen = Screen::sized(80, HEIGHT);
    screen.ui = Ui::new(TestBackend::new(80, HEIGHT), App::new(snapshot, None, NOW)).unwrap();
    screen.ui.app_mut().show_files().await;
    screen.draw();
    let shown = screen.text();
    assert_eq!(shown.matches("(none)").count(), 2, "{shown}");
    assert!(shown.contains("not a git repository"), "{shown}");
}

#[test]
fn context_lists_categories_measured_usage_and_truncates_narrow_rows() {
    let report = serde_json::from_value(json!({
        "prefix":[
            {"kind":"system","path":null,"tokens":3000},
            {"kind":"modelFamily","path":null,"tokens":100},
            {"kind":"globalInstructions","path":"/home/test/.canter/AGENTS.md","tokens":1000},
            {"kind":"skills","path":null,"tokens":1000},
            {"kind":"projectInstructions","path":"/project/AGENTS.md","tokens":2000},
            {"kind":"environment","path":null,"tokens":200}
        ],
        "skillCount":5,"toolCount":9,"toolTokens":4000,"compactionTokens":12000,
        "messages":{"count":34,"tokens":395000,"user":20000,"assistant":80000,"toolResults":290000,"images":5000},
        "estimatedTokens":418300,"freeTokens":581700,"modelContextWindow":1000000,"contextTokens":420000
    })).unwrap();
    for width in [140, 40] {
        let mut screen = Screen::sized(width, 30);
        screen.ui.app_mut().show_context(&report);
        screen.draw();
        let shown = screen.text();
        for label in [
            "System prompt",
            "Model notes",
            "Global AGENTS.md",
            "Project AGENTS.md",
            "Skills (5)",
            "Environment",
            "Tools (9)",
            "Compaction summary",
            "Messages (34)",
            "Free",
        ] {
            assert!(shown.contains(label), "{label}: {shown}");
        }
        if width == 140 {
            assert!(shown.contains("ctx 42% 420k/1M (last request)"), "{shown}");
            assert!(
                shown.contains("user 20k · assistant 80k · tool results 290k · images 5k"),
                "{shown}"
            );
            assert!(shown.contains("582k"));
        } else {
            assert!(
                screen
                    .lines()
                    .iter()
                    .any(|line| line.contains("Messages (34)") && line.ends_with('…')),
                "{shown}"
            );
        }
    }
    let mut report = report;
    report.context_tokens = None;
    report.compaction_tokens = None;
    let mut screen = Screen::new();
    screen.ui.app_mut().show_context(&report);
    screen.draw();
    let shown = screen.text();
    assert!(shown.contains("Context  no request yet"), "{shown}");
    assert!(!shown.contains("Compaction summary"));
}

#[test]
fn handoff_rejects_empty_busy_and_btw_sessions_without_an_action() {
    let mut screen = Screen::new();
    assert_eq!(screen.enter("/handoff"), None);
    screen.draw();
    assert!(screen.text().contains("No conversation to hand off"));
    screen.ui.app_mut().handle_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        screen.now,
    );
    screen.start_turn("main");
    assert_eq!(screen.enter("/handoff continue"), None);
    screen.draw();
    assert!(screen.text().contains("Wait for this turn to finish"));
    screen.ui.app_mut().handle_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        screen.now,
    );
    screen.event(json!({"type":"turn.completed","turn":turn_json("main", "completed")}));
    screen.event(json!({"type":"session.status","status":"idle"}));
    assert!(matches!(
        screen.enter("/btw question"),
        Some(Action::Btw { .. })
    ));
    assert_eq!(screen.enter("/handoff continue"), None);
    screen.draw();
    assert!(
        screen
            .text()
            .contains("A btw answer is already in progress")
    );
}

#[test]
fn handoff_escape_on_generating_frame_cancels_even_when_note_just_finished() {
    let mut screen = Screen::new();
    screen.start_turn("main");
    screen.event(json!({"type":"turn.completed","turn":turn_json("main", "completed")}));
    screen.event(json!({"type":"session.status","status":"idle"}));
    let Some(Action::Handoff { id, .. }) = screen.enter("/handoff") else {
        panic!("handoff action");
    };
    screen.ui.app_mut().btw_event(
        id,
        canter_protocol::BtwEvent::Delta {
            delta: "private note".into(),
        },
    );
    screen.draw();
    assert!(screen.text().contains("Writing the handoff note"));
    assert!(!screen.text().contains("private note"));
    screen
        .ui
        .app_mut()
        .btw_event(id, canter_protocol::BtwEvent::Completed);
    assert_eq!(screen.key(KeyCode::Esc), Some(Action::CancelBtw { id }));
    screen
        .ui
        .app_mut()
        .btw_event(id, canter_protocol::BtwEvent::Completed);
    screen.draw();
    assert!(screen.text().contains("handoff  Stopped"));
    assert!(matches!(
        screen.enter("/handoff again"),
        Some(Action::Handoff { .. })
    ));
}

#[test]
fn btw_esc_stops_the_private_answer_before_the_main_turn() {
    let mut screen = Screen::new();
    screen.start_turn("main");
    screen.event(json!({"type":"item.started","turnId":"main","item":{"type":"agentMessage","id":"streaming","text":"main output\n".repeat(30)}}));
    screen.draw();
    let start = screen.enter("/btw explain this");
    assert!(start.is_some(), "/btw must start a private answer");
    assert!(!matches!(start, Some(Action::Send(_))));
    screen.draw();
    assert!(screen.text().contains("› /btw explain this"));
    assert!(screen.text().contains("btw  Waiting"));
    let first = screen.key(KeyCode::Esc);
    assert!(first.is_some());
    assert_ne!(
        first,
        Some(Action::Interrupt {
            turn_id: "main".into()
        })
    );
    screen.draw();
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Interrupt {
            turn_id: "main".into()
        })
    );
    assert_eq!(screen.ui.app().snapshot().turns[0].items.len(), 1);
}

#[test]
fn btw_streams_markdown_locally_and_rejects_empty_or_overlapping_questions() {
    use canter_protocol::BtwEvent;
    use ratatui::style::{Color, Modifier};
    let mut screen = Screen::new();
    let snapshot = screen.ui.app().snapshot().clone();
    assert_eq!(screen.enter("/btw"), None);
    screen.draw();
    assert!(screen.text().contains("Usage: /btw <question>"));
    screen.ui.app_mut().handle_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        NOW,
    );
    let Some(Action::Btw { id, question }) = screen.enter("/btw  explain `x`  ") else {
        panic!("btw action")
    };
    assert_eq!(question, " explain `x`  ");
    assert_eq!(screen.enter("/btw another"), None);
    screen.draw();
    assert!(screen.text().contains("already in progress"));
    screen.ui.app_mut().handle_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        NOW,
    );
    for delta in ["**bold", " reply**\n- `code`"] {
        screen.ui.app_mut().btw_event(
            id,
            BtwEvent::Delta {
                delta: delta.into(),
            },
        );
        screen.draw();
    }
    assert!(screen.text().contains("btw  bold reply"));
    assert!(!screen.text().contains("**bold reply**"));
    let buffer = screen.ui.backend().buffer();
    let pos = (0..HEIGHT)
        .flat_map(|y| (0..WIDTH).map(move |x| (x, y)))
        .find(|&(x, y)| {
            buffer[(x, y)].symbol() == "b" && buffer[(x, y)].modifier.contains(Modifier::BOLD)
        })
        .unwrap();
    assert_eq!(buffer[pos].fg, Color::DarkGray);
    assert!(buffer[pos].modifier.contains(Modifier::DIM));
    screen.ui.app_mut().btw_event(
        id,
        BtwEvent::Error {
            message: "connection lost".into(),
        },
    );
    screen.draw();
    assert!(screen.text().contains("Error: connection lost"));
    assert_eq!(screen.ui.app().snapshot(), &snapshot);
    let Some(Action::Btw { id: next, .. }) = screen.enter("/btw retry") else {
        panic!("retry")
    };
    screen.ui.app_mut().btw_event(
        id,
        BtwEvent::Delta {
            delta: "stale".into(),
        },
    );
    screen.ui.app_mut().btw_event(
        next,
        BtwEvent::Delta {
            delta: "new answer".into(),
        },
    );
    screen.ui.app_mut().btw_event(next, BtwEvent::Completed);
    screen.draw();
    assert!(screen.text().contains("new answer"));
    assert!(!screen.text().contains("stale"));
    let mut reopened = Ui::new(
        TestBackend::new(WIDTH, HEIGHT),
        App::new(snapshot, None, NOW),
    )
    .unwrap();
    reopened.draw(NOW).unwrap();
    assert!(
        !rows(reopened.backend().buffer())
            .join("\n")
            .contains("new answer")
    );
}

#[test]
fn btw_escape_closes_choices_and_command_list_before_cancelling_the_answer() {
    for command in ["/think", "/mode", "/"] {
        let mut screen = Screen::new();
        screen.start_turn("main");
        let Some(Action::Btw { id, .. }) = screen.enter("/btw explain this") else {
            panic!("btw action");
        };
        if command == "/" {
            screen.type_text(command);
        } else {
            assert_eq!(screen.enter(command), None);
        }
        screen.draw();
        assert_eq!(screen.key(KeyCode::Esc), None, "close {command} only");
        screen.draw();
        assert!(screen.no_list());
        assert!(!screen.text().contains("↑↓ select"));
        assert!(screen.text().contains("btw  Waiting"));
        assert_eq!(screen.key(KeyCode::Esc), Some(Action::CancelBtw { id }));
    }
}

#[test]
fn btw_escape_still_precedes_an_approval_box() {
    let mut screen = Screen::new();
    screen.start_turn("main");
    let Some(Action::Btw { id, .. }) = screen.enter("/btw explain this") else {
        panic!("btw action");
    };
    screen.ask("main", "sleep 60");
    screen.draw();
    assert_eq!(screen.key(KeyCode::Esc), Some(Action::CancelBtw { id }));
    screen.draw();
    assert_eq!(screen.ui.app().snapshot().pending_requests.len(), 1);
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Interrupt {
            turn_id: "main".into()
        })
    );
}

#[test]
fn btw_escape_from_an_answering_frame_does_not_interrupt_after_completion() {
    for approval in [false, true] {
        let mut screen = Screen::new();
        screen.start_turn("main");
        let Some(Action::Btw { id, .. }) = screen.enter("/btw explain this") else {
            panic!("btw action");
        };
        if approval {
            screen.ask("main", "sleep 60");
        }
        screen.draw();
        screen
            .ui
            .app_mut()
            .btw_event(id, canter_protocol::BtwEvent::Completed);
        assert_eq!(
            screen.key(KeyCode::Esc),
            None,
            "the key's frame still shows btw answering"
        );
        screen.draw();
        assert_eq!(
            screen.key(KeyCode::Esc),
            Some(Action::Interrupt {
                turn_id: "main".into()
            })
        );
    }
}

#[test]
fn btw_streaming_keeps_later_turns_notices_and_footers_when_cached() {
    let mut screen = Screen::sized(80, 40);
    screen.start_turn("earlier");
    screen.event(json!({"type":"item.completed","turnId":"earlier","item":{"type":"agentMessage","id":"before","text":"Earlier **answer**"}}));
    screen.event(json!({"type":"turn.completed","turn":turn_json("earlier", "completed")}));
    screen.event(json!({"type":"session.status","status":"idle"}));
    screen.draw();
    let Some(Action::Btw { id, .. }) = screen.enter("/btw side question") else {
        panic!("btw action");
    };
    screen.draw();
    assert!(screen.text().contains("btw  Waiting"));
    screen.ui.app_mut().btw_event(
        id,
        canter_protocol::BtwEvent::Delta {
            delta: "First **part**".into(),
        },
    );
    screen.ui.app_mut().warning("Notice after btw");
    screen.start_turn("later");
    screen.event(json!({"type":"item.completed","turnId":"later","item":{"type":"agentMessage","id":"after","text":"Later answer"}}));
    screen.event(json!({"type":"turn.completed","turn":turn_json("later", "interrupted")}));
    screen.event(json!({"type":"session.status","status":"idle"}));
    screen.ui.app_mut().btw_event(
        id,
        canter_protocol::BtwEvent::Delta {
            delta: " and last part".into(),
        },
    );
    screen.draw();
    let wide = screen.text();
    let ordered = [
        "Earlier answer",
        "btw  First part and last part",
        "Notice after btw",
        "Later answer",
        "Turn interrupted",
    ];
    let positions: Vec<_> = ordered
        .iter()
        .map(|text| {
            assert_eq!(wide.matches(text).count(), 1, "{wide}");
            wide.find(text).unwrap()
        })
        .collect();
    assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{wide}");
    screen.ui.backend_mut().resize(40, 40);
    screen.draw();
    let narrow = screen.text();
    screen
        .ui
        .app_mut()
        .btw_event(id, canter_protocol::BtwEvent::Completed);
    screen.draw();
    assert_eq!(
        screen.text(),
        narrow,
        "moving the completed btw into history preserves the display"
    );
    screen.ui.backend_mut().resize(80, 40);
    screen.draw();
    assert_eq!(screen.text(), wide);
}

#[test]
fn mcp_status_and_approval_show_server_tool_arguments_and_escape_interrupts() {
    let mut screen = Screen::new();
    let report=serde_json::from_value(json!({"data":[
        {"name":"local","connected":true,"error":null,"tools":[{"name":"mcp__local__echo","tool":"echo"}],"warnings":[]},
        {"name":"remote","connected":false,"error":"connection timed out","tools":[],"warnings":[]}
    ]})).unwrap();
    screen.ui.app_mut().show_mcp(&report);
    screen.draw();
    let text = screen.text();
    for expected in [
        " ● local    connected · 1 tool\n   echo\n",
        " ○ remote   disconnected\n   connection timed out\n",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    // The result is in a box above the input box, which takes the keys.
    assert_eq!(screen.key(KeyCode::Esc), None);
    screen.draw();
    assert!(!screen.text().contains("remote"));
    screen.start_turn("t1");
    screen.event(json!({"type":"item.started","turnId":"t1","item":{
        "type":"mcpToolCall","id":"m1","server":"local","tool":"echo","arguments":{"text":"hello"},"result":"","status":"inProgress"
    }}));
    screen.draw();
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Interrupt {
            turn_id: "t1".into()
        })
    );
    screen.event(json!({"type":"request.opened","request":{
        "id":"r1","turnId":"t1","createdAt":NOW,"kind":{"type":"approval","itemId":"m1","tool":"mcp__local__echo","arguments":{"text":"hello"},"availableDecisions":["accept","acceptForSession","decline","cancel"]}
    }}));
    screen.event(json!({"type":"session.status","status":"waiting_approval"}));
    screen.draw();
    assert!(screen.text().contains("Allow MCP local / echo?"));
    assert!(screen.text().contains("hello"));
}
