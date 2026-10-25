//! Slash commands in the input box on ratatui's TestBackend: the command list
//! above the input box, picking and completing a command, what each command
//! asks the caller to do, and telling commands from messages. Also Shift+Tab,
//! which cycles the permission modes `/mode` sets.

use canter_protocol::{Event, PermissionMode, SessionSnapshot, SessionUpdateParams};
use canter_tui::{ANSWER_GUARD_MS, Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
const WIDTH: u16 = 80;
const HEIGHT: u16 = 24;

/// Every command, in the list's order, with what the list says about it.
const COMMANDS: [(&str, &str); 10] = [
    ("/compact", "Summarize the conversation to free up context"),
    ("/exit", "Quit canter"),
    ("/help", "Show available commands"),
    ("/mode", "Change the permission mode"),
    ("/model", "Change the model"),
    ("/new", "Start a new session"),
    ("/resume", "Switch to another session"),
    ("/skills", "List this session's skills"),
    ("/think", "Change thinking effort (none, low, high, max)"),
    ("/usage", "Show this session's token usage and cost"),
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
fn skills_show_names_sources_and_manual_marker_in_single_truncated_rows() {
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
        let manual = lines
            .iter()
            .position(|line| line.contains("manual [global] [not for model]"))
            .unwrap();
        let tdd = &lines[manual + 1];
        assert!(
            tdd.contains("tdd [project]  Write a failing"),
            "{}",
            screen.text()
        );
        if width == 40 {
            assert!(lines[manual].ends_with('…'));
            assert!(tdd.ends_with('…'));
            assert!(!screen.text().contains("changing implementation"));
        } else {
            assert!(lines[manual].contains("Review all interface states"));
            assert!(tdd.contains("verify the result"));
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
    let mut screen = Screen::new();
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
    assert!(list[1].starts_with("   /exit"), "{shown}");

    screen.type_text("e");
    screen.draw();
    let shown = screen.text();
    assert!(
        screen.above_box(1)[0].starts_with(" › /exit"),
        "only /exit is left:\n{shown}"
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
    let mut screen = Screen::new();
    screen.type_text("/");
    screen.draw();
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Up), None);
    screen.draw();
    let shown = screen.text();
    assert!(
        screen.above_box(COMMANDS.len())[1].starts_with(" › /exit"),
        "{shown}"
    );

    assert_eq!(screen.key(KeyCode::Tab), None);
    screen.draw();
    assert_eq!(screen.input(), "› /exit", "{}", screen.text());
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Quit));

    // Enter on `/` alone completes the command selected, like Tab.
    let mut screen = Screen::new();
    screen.type_text("/");
    screen.draw();
    assert_eq!(screen.key(KeyCode::Down), None);
    assert_eq!(screen.key(KeyCode::Enter), None);
    screen.draw();
    assert_eq!(screen.input(), "› /exit", "{}", screen.text());
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Quit));
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

/// /help shows the commands in the conversation, and sends nothing.
#[test]
fn help_shows_the_commands_in_the_conversation() {
    let mut screen = Screen::new();
    assert_eq!(screen.enter("/help"), None);
    screen.draw();
    let shown = screen.text();
    let box_top = screen.lines().iter().rposition(|row| row.starts_with('╭'));
    for (name, about) in COMMANDS {
        let row = screen
            .lines()
            .iter()
            .position(|row| row.contains(name) && row.contains(about));
        assert!(
            row.is_some_and(|row| Some(row) < box_top),
            "{name} in the conversation:\n{shown}"
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
    for name in ["compact", "exit", "help", "new", "resume"] {
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
        screen.above_box(1)[0].starts_with(" › /compact"),
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
