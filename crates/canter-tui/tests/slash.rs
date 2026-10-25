//! Slash commands in the input box on ratatui's TestBackend: the command list
//! above the input box, picking and completing a command, what each command
//! asks the caller to do, and telling commands from messages.

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
const COMMANDS: [(&str, &str); 6] = [
    ("/compact", "Summarize the conversation to free up context"),
    ("/exit", "Quit canter"),
    ("/help", "Show available commands"),
    ("/mode", "Change the permission mode"),
    ("/new", "Start a new session"),
    ("/resume", "Switch to another session"),
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
    assert!(shown.contains("test-model · Always ask"), "{shown}");
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
