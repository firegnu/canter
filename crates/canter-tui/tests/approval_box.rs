//! The approval box on ratatui's TestBackend, fed protocol events in the
//! client loop's order: events that arrived are applied, then the keys pressed
//! meanwhile, and only then is the next frame drawn. A key must act on what
//! the last frame showed.

use canter_protocol::{ApprovalDecision, Event, SessionSnapshot};
use canter_tui::{ANSWER_GUARD_MS, Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    /// The time frames are drawn and keys pressed at.
    now: i64,
}

impl Screen {
    fn new(width: u16, height: u16) -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let ui = Ui::new(TestBackend::new(width, height), app).unwrap();
        Self {
            ui,
            seq: 0,
            now: NOW,
        }
    }

    fn wait(&mut self, ms: i64) {
        self.now += ms;
    }

    /// Like a user who reads the box before answering: the answer guard is over.
    fn read_the_box(&mut self) {
        self.wait(ANSWER_GUARD_MS);
    }

    /// An event from the server; the screen is not redrawn yet.
    fn event(&mut self, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": NOW });
        let fields = payload.as_object().unwrap().clone();
        event.as_object_mut().unwrap().extend(fields);
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, NOW);
    }

    fn draw(&mut self) {
        self.ui.draw(self.now).unwrap();
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), self.now)
    }

    fn screen(&self) -> String {
        rows(self.ui.backend().buffer())
    }

    /// Whether the first row showing `text` is drawn dim where `text` starts.
    fn dim(&self, text: &str) -> bool {
        let buffer = self.ui.backend().buffer();
        let y = self
            .screen()
            .lines()
            .position(|row| row.contains(text))
            .unwrap_or_else(|| panic!("{text} is not on screen")) as u16;
        let first = text.chars().next().unwrap().to_string();
        let x = (0..buffer.area.width)
            .find(|&x| buffer[(x, y)].symbol() == first)
            .unwrap();
        buffer[(x, y)].modifier.contains(Modifier::DIM)
    }

    fn start_turn(&mut self, turn: &str) {
        self.event(json!({ "type": "turn.started", "turn": turn_json(turn, "inProgress") }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn finish_turn(&mut self, turn: &str) {
        self.event(json!({ "type": "turn.completed", "turn": turn_json(turn, "completed") }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    /// The model wants to run `command` and asks first.
    fn ask(&mut self, request: &str, turn: &str, command: &str) {
        self.ask_with_decisions(
            request,
            turn,
            command,
            &[
                "accept",
                "acceptForSession",
                "decline",
                "declineWithFeedback",
                "cancel",
            ],
        );
    }

    fn ask_with_decisions(&mut self, request: &str, turn: &str, command: &str, decisions: &[&str]) {
        let item = format!("item-{request}");
        self.event(json!({
            "type": "item.started", "turnId": turn,
            "item": command_json(&item, command, "inProgress"),
        }));
        self.event(json!({
            "type": "request.opened",
            "request": {
                "id": request, "turnId": turn, "createdAt": NOW,
                "kind": {
                    "type": "approval", "itemId": item, "tool": "shell",
                    "arguments": { "command": command },
                    "availableDecisions": decisions,
                },
            },
        }));
        self.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    }

    /// Another terminal allows `request`, and the command runs.
    fn allowed_elsewhere(&mut self, request: &str, turn: &str, command: &str) {
        self.event(json!({
            "type": "request.resolved", "requestId": request,
            "resolution": { "type": "answered", "decision": "accept" },
            "clientName": "canter-tui b",
        }));
        self.event(json!({ "type": "session.status", "status": "working" }));
        self.event(json!({
            "type": "item.completed", "turnId": turn,
            "item": command_json(&format!("item-{request}"), command, "completed"),
        }));
    }
}

fn turn_json(id: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "跑一下" }],
        "clientName": "canter-tui", "usage": null,
    })
}

fn command_json(id: &str, command: &str, status: &str) -> Value {
    json!({
        "type": "commandExecution", "id": id, "command": command, "cwd": "/work",
        "status": status, "aggregatedOutput": "", "exitCode": null, "durationMs": null,
    })
}

fn resolve(request: &str, decision: ApprovalDecision) -> Option<Action> {
    Some(Action::Resolve {
        request_id: request.to_owned(),
        decision,
        feedback: None,
    })
}

fn question(screen: &mut Screen, request: &str) {
    question_with_text(screen, request, "Pick storage");
}

fn question_with_text(screen: &mut Screen, request: &str, text: &str) {
    screen.event(json!({"type":"request.opened","request":{
        "id":request,"turnId":"t1","createdAt":NOW,"kind":{
            "type":"userInput","itemId":format!("item-{request}"),
            "question":text, "options":[
                {"label":"SQLite","description":"Local files"},
                {"label":"PostgreSQL","description":"Shared server"}
            ]
        }
    }}));
    screen.event(json!({"type":"session.status","status":"waiting_approval"}));
}

fn question_resolved(screen: &mut Screen, request: &str) {
    screen.event(json!({"type":"request.resolved","requestId":request,
        "resolution":{"type":"userInput","answer":{"type":"option","label":"PostgreSQL"}},
        "clientName":"another-terminal"
    }));
}

#[test]
fn question_keys_follow_the_visible_frame_and_both_300ms_guards() {
    let mut screen = Screen::new(100, 30);
    screen.start_turn("t1");
    screen.draw();
    question(&mut screen, "q1");
    assert_eq!(screen.key(KeyCode::Enter), None, "not drawn yet");
    screen.draw();
    assert!(screen.screen().contains("2. PostgreSQL"));
    assert!(screen.dim("PostgreSQL"));
    for key in [KeyCode::Enter, KeyCode::Char('2'), KeyCode::Down] {
        screen.wait(ANSWER_GUARD_MS - 1);
        assert_eq!(screen.key(key), None, "guard extends on every key");
    }
    screen.read_the_box();
    screen.draw();
    assert!(!screen.dim("PostgreSQL"));
    // q2 arrived while the frame still shows q1. An old Enter cannot answer q2.
    question_resolved(&mut screen, "q1");
    question(&mut screen, "q2");
    assert_eq!(screen.key(KeyCode::Enter), None);
    screen.draw();
    assert_eq!(
        screen.key(KeyCode::Char('2')),
        None,
        "new box starts a guard"
    );
    screen.read_the_box();
    assert_eq!(
        screen.key(KeyCode::Char('2')),
        Some(Action::ResolveUserInput {
            request_id: "q2".into(),
            answer: canter_protocol::UserInputAnswer::Option {
                label: "PostgreSQL".into()
            },
        })
    );
    screen.draw();
    assert!(!screen.screen().contains("─ Ask user"));
    assert_eq!(screen.key(KeyCode::Char('稿')), None);
    screen.wait(ANSWER_GUARD_MS - 1);
    assert_eq!(
        screen.key(KeyCode::Enter),
        None,
        "own answer protects the draft"
    );
    screen.draw();
    assert!(screen.dim("稿"));
    screen.read_the_box();
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Send("稿".into())));
}

#[test]
fn question_custom_paste_appends_a_single_line_and_preserves_the_message_draft() {
    let mut screen = Screen::new(100, 30);
    screen.start_turn("t1");
    screen.draw();
    screen.ui.app_mut().handle_paste("草稿\n第二行");
    question(&mut screen, "q1");
    screen.draw();
    // Outside custom input, paste still goes to the message draft.
    screen.ui.app_mut().handle_paste("（选项时）");
    screen.read_the_box();
    assert_eq!(screen.key(KeyCode::Char('3')), None);
    screen.draw();
    for c in "Use ".chars() {
        assert_eq!(screen.key(KeyCode::Char(c)), None);
    }
    screen
        .ui
        .app_mut()
        .handle_paste("plain\nfiles\r\nfor\rnow.");
    screen.draw();
    assert!(screen.screen().contains("› Use plain files for now.▏"));
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::ResolveUserInput {
            request_id: "q1".into(),
            answer: canter_protocol::UserInputAnswer::Text {
                text: "Use plain files for now.".into(),
            },
        })
    );
    screen.draw();
    // A retained custom cursor must not catch paste after this terminal answers.
    screen.ui.app_mut().handle_paste("（答完后）");
    screen.read_the_box();
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::Send("草稿\n第二行（选项时）（答完后）".into()))
    );
}

#[test]
fn question_empty_custom_answer_can_return_to_the_options() {
    for back in [KeyCode::Backspace, KeyCode::Up] {
        let mut screen = Screen::new(100, 30);
        screen.start_turn("t1");
        question(&mut screen, "q1");
        screen.draw();
        screen.read_the_box();
        assert_eq!(screen.key(KeyCode::Char('3')), None);
        screen.draw();
        assert_eq!(screen.key(KeyCode::Char('x')), None);
        assert_eq!(screen.key(KeyCode::Up), None);
        screen.draw();
        assert!(screen.screen().contains("› x▏"), "Up keeps non-empty text");
        assert_eq!(screen.key(KeyCode::Backspace), None);
        screen.draw();
        assert!(
            screen.screen().contains("Write your own:"),
            "deleting the last character stays in the editor"
        );
        assert_eq!(screen.key(back), None);
        // These keys still belong to the custom-input frame, not the options.
        assert_eq!(screen.key(KeyCode::Char('1')), None);
        assert_eq!(screen.key(KeyCode::Enter), None);
        screen.draw();
        assert!(
            screen.screen().contains("3. Write your own"),
            "{back:?} returns to the options"
        );
        assert_eq!(
            screen.key(KeyCode::Char('2')),
            Some(Action::ResolveUserInput {
                request_id: "q1".into(),
                answer: canter_protocol::UserInputAnswer::Option {
                    label: "PostgreSQL".into()
                },
            })
        );
    }
}

#[test]
fn question_custom_answer_keeps_page_scroll_escape_and_ctrl_c() {
    let mut screen = Screen::new(100, 30);
    screen.start_turn("t1");
    question_with_text(
        &mut screen,
        "q1",
        &format!("Start of question. {}", "Question details. ".repeat(200)),
    );
    screen.draw();
    screen.read_the_box();
    assert_eq!(screen.key(KeyCode::Char('3')), None);
    screen.draw();
    assert_eq!(screen.key(KeyCode::Char('x')), None);
    screen.draw();
    let before = screen.screen();
    assert!(before.contains("Start of question."));
    assert_eq!(screen.key(KeyCode::PageDown), None);
    screen.draw();
    assert!(!screen.screen().contains("Start of question."));
    assert!(screen.screen().contains("› x▏"));
    assert_eq!(screen.key(KeyCode::PageUp), None);
    screen.draw();
    assert_eq!(screen.screen(), before);
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Interrupt {
            turn_id: "t1".into()
        })
    );
    assert_eq!(
        screen.ui.app_mut().handle_key(
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            screen.now
        ),
        Some(Action::Quit)
    );
}

#[test]
fn question_custom_answer_accepts_digits_only_after_its_frame_is_shown() {
    let mut screen = Screen::new(100, 30);
    screen.start_turn("t1");
    question(&mut screen, "q1");
    screen.draw();
    screen.read_the_box();
    assert_eq!(screen.key(KeyCode::Char('3')), None);
    assert_eq!(screen.key(KeyCode::Char('1')), None);
    assert_eq!(screen.key(KeyCode::Enter), None);
    screen.draw();
    assert!(screen.screen().contains("Write your own:"));
    for c in "2 x".chars() {
        assert_eq!(screen.key(KeyCode::Char(c)), None);
    }
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::ResolveUserInput {
            request_id: "q1".into(),
            answer: canter_protocol::UserInputAnswer::Text { text: "2 x".into() },
        })
    );
}

/// The buffer as text, one row per line, trailing spaces trimmed.
fn rows(buffer: &Buffer) -> String {
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut row = String::new();
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            row.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        out += row.trim_end();
        out.push('\n');
    }
    out
}

/// Another terminal answers request A and the model asks B before this
/// screen redraws: keys pressed while A was on screen must not answer B.
#[test]
fn keys_meant_for_an_answered_request_do_not_answer_the_next() {
    let mut screen = Screen::new(80, 30);
    screen.start_turn("t1");
    screen.ask("r1", "t1", "printf A > first-proof.txt");
    screen.draw();
    assert!(screen.screen().contains("printf A > first-proof.txt"));

    screen.allowed_elsewhere("r1", "t1", "printf A > first-proof.txt");
    screen.ask("r2", "t1", "printf B > second-proof.txt");
    assert_eq!(screen.key(KeyCode::Enter), None, "Enter was meant for A");
    assert_eq!(screen.key(KeyCode::Char('1')), None);

    screen.draw();
    assert!(screen.screen().contains("printf B > second-proof.txt"));
    screen.read_the_box();
    assert_eq!(
        screen.key(KeyCode::Enter),
        resolve("r2", ApprovalDecision::Accept)
    );
}

/// The user's decision after review round 2: the terminal may still show
/// what was there before for a moment after a box is drawn. So for
/// ANSWER_GUARD_MS after a box's answers come on screen, answer keys are
/// dropped, not kept for later, and every key in that time pushes the end
/// back. The answers show dim until then.
#[test]
fn answer_keys_right_after_the_box_appears_are_dropped() {
    let mut screen = Screen::new(80, 30);
    screen.start_turn("t1");
    screen.ask("r1", "t1", "printf A > first-proof.txt");
    screen.draw();
    assert!(screen.dim("1. Allow once"), "A's answers are not taken yet");
    screen.read_the_box();
    screen.draw();
    assert!(!screen.dim("1. Allow once"));

    // Another terminal answers A, and B comes on screen.
    screen.allowed_elsewhere("r1", "t1", "printf A > first-proof.txt");
    screen.ask("r2", "t1", "printf B > second-proof.txt");
    screen.draw();
    assert!(screen.screen().contains("printf B > second-proof.txt"));
    assert!(screen.dim("1. Allow once"), "B's answers are not taken yet");
    for code in [KeyCode::Down, KeyCode::Char('2'), KeyCode::Enter] {
        screen.wait(50);
        assert_eq!(screen.key(code), None, "{code:?} right after B appeared");
    }
    // The guard now ends ANSWER_GUARD_MS after the last of those keys.
    screen.wait(ANSWER_GUARD_MS - 1);
    screen.draw();
    assert!(screen.dim("1. Allow once"));
    assert_eq!(
        screen.key(KeyCode::Enter),
        None,
        "the keys meanwhile pushed the guard back"
    );

    screen.read_the_box();
    screen.draw();
    assert!(!screen.dim("1. Allow once"));
    // Down was dropped, not kept: option 1 is still the one selected.
    assert_eq!(
        screen.key(KeyCode::Enter),
        resolve("r2", ApprovalDecision::Accept)
    );
}

/// Keys typed into the input box before the approval box was drawn belong to
/// the input box, not to the approval.
#[test]
fn keys_typed_before_the_box_appears_do_not_answer_it() {
    let mut screen = Screen::new(80, 30);
    screen.start_turn("t1");
    screen.draw();

    screen.ask("r1", "t1", "rm -rf build");
    for code in [KeyCode::Char('1'), KeyCode::Enter] {
        let action = screen.key(code);
        assert!(
            !matches!(action, Some(Action::Resolve { .. })),
            "{code:?} answered a box that was not on screen: {action:?}"
        );
    }

    screen.draw();
    assert!(screen.screen().contains("rm -rf build"));
    screen.read_the_box();
    assert_eq!(
        screen.key(KeyCode::Char('3')),
        resolve("r1", ApprovalDecision::Decline)
    );
}

/// Esc stops the turn the screen showed, not one that started since.
#[test]
fn esc_interrupts_only_the_turn_on_screen() {
    let mut screen = Screen::new(80, 30);
    screen.start_turn("t1");
    screen.draw();

    screen.finish_turn("t1");
    screen.start_turn("t2");
    assert_eq!(screen.key(KeyCode::Esc), None, "t2 was not on screen yet");

    screen.draw();
    assert_eq!(
        screen.key(KeyCode::Esc),
        Some(Action::Interrupt {
            turn_id: "t2".into()
        })
    );
}

/// What 「本会话都允许」 grants is on screen before anything is chosen, so
/// pressing 2 as soon as the box takes answers is an informed choice.
#[test]
fn what_allowing_for_the_session_means_shows_before_choosing() {
    let mut screen = Screen::new(120, 30);
    screen.start_turn("t1");
    screen.ask("r1", "t1", "cargo test");
    screen.draw();
    let shown = screen.screen();
    assert!(shown.contains("› 1. Allow once"), "{shown}");
    assert!(
        shown.contains(
            "All later commands in this session run without asking, even after switching back to Always ask."
        ),
        "the note shows with option 1 selected:\n{shown}"
    );
    screen.read_the_box();
    assert_eq!(
        screen.key(KeyCode::Char('2')),
        resolve("r1", ApprovalDecision::AcceptForSession)
    );
}

/// In a small window every answer the box accepts is on screen, and the body
/// scrolls in the rows left over. When even the answers do not fit, the box
/// says the window is too small and accepts none.
#[test]
fn a_small_window_never_hides_an_answer_it_accepts() {
    let long: Vec<String> = (1..=30).map(|n| format!("echo line-{n}")).collect();
    let long = long.join("\n");
    for command in ["cargo test", long.as_str()] {
        for height in 6..=30 {
            let mut screen = Screen::new(80, height);
            screen.start_turn("t1");
            screen.ask("r1", "t1", command);
            screen.draw();
            let shown = screen.screen();
            if shown.contains("Window too small") {
                assert!(height < 20, "height {height} is enough:\n{shown}");
                for code in [
                    KeyCode::Enter,
                    KeyCode::Char('1'),
                    KeyCode::Char('2'),
                    KeyCode::Char('3'),
                    KeyCode::Char('4'),
                ] {
                    assert_eq!(screen.key(code), None, "height {height}: {code:?}");
                }
                continue;
            }
            for answer in [
                "1. Allow once",
                "2. Allow for this session",
                "3. Decline",
                "4. Decline and say what to do instead",
                "5. Decline and stop",
            ] {
                assert!(
                    shown.contains(answer),
                    "height {height}: {answer} is cut off:\n{shown}"
                );
            }
            let first = command.lines().next().unwrap();
            assert!(
                shown.contains(first),
                "height {height}: no room for the command:\n{shown}"
            );
        }
    }
}

/// The other way round: another terminal answers the box and the input box
/// comes back with a draft in it. An Enter right then was meant for the box,
/// so for ANSWER_GUARD_MS it does not send the draft, and every key in that
/// time pushes the end back. Typing still works; the draft shows dim until
/// Enter sends it again.
#[test]
fn enter_right_after_the_box_closes_does_not_send_the_draft() {
    let mut screen = Screen::new(80, 30);
    screen.start_turn("t1");
    screen.draw();
    for c in "草稿".chars() {
        assert_eq!(screen.key(KeyCode::Char(c)), None);
    }
    screen.ask("r1", "t1", "printf A > first-proof.txt");
    screen.draw();
    screen.read_the_box();
    screen.draw();
    assert!(!screen.screen().contains("草稿"));

    // Another terminal answers, and the input box comes back.
    screen.allowed_elsewhere("r1", "t1", "printf A > first-proof.txt");
    screen.draw();
    assert!(screen.screen().contains("› 草稿"));
    assert!(screen.dim("草稿"), "Enter does not send the draft yet");
    assert_eq!(
        screen.key(KeyCode::Enter),
        None,
        "Enter was meant for the box"
    );
    screen.wait(50);
    assert_eq!(screen.key(KeyCode::Char('!')), None);
    // The guard now ends ANSWER_GUARD_MS after the last key.
    screen.wait(ANSWER_GUARD_MS - 1);
    screen.draw();
    assert!(
        screen.screen().contains("› 草稿!"),
        "typing works meanwhile"
    );
    assert!(screen.dim("草稿"));
    assert_eq!(
        screen.key(KeyCode::Enter),
        None,
        "the keys meanwhile pushed the guard back"
    );

    screen.wait(ANSWER_GUARD_MS);
    screen.draw();
    assert!(!screen.dim("草稿"));
    assert_eq!(
        screen.key(KeyCode::Enter),
        Some(Action::Send("草稿!".into()))
    );
}

/// Answering the box in this terminal brings the input box back the same way,
/// with the same guard: a second Enter right after the answer does not send
/// the draft.
#[test]
fn enter_right_after_answering_does_not_send_the_draft() {
    let mut screen = Screen::new(80, 30);
    screen.start_turn("t1");
    screen.draw();
    screen.key(KeyCode::Char('稿'));
    screen.ask("r1", "t1", "printf A > first-proof.txt");
    screen.draw();
    screen.read_the_box();
    assert_eq!(
        screen.key(KeyCode::Enter),
        resolve("r1", ApprovalDecision::Accept)
    );
    screen.draw();
    assert!(screen.screen().contains("› 稿"));
    assert!(screen.dim("稿"));
    assert_eq!(screen.key(KeyCode::Enter), None, "a second Enter");

    screen.read_the_box();
    screen.draw();
    assert!(!screen.dim("稿"));
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Send("稿".into())));
}

#[test]
fn decline_feedback_is_fourth_and_sends_typed_and_pasted_text() {
    for enter in [false, true] {
        let mut screen = Screen::new(100, 30);
        screen.start_turn("t1");
        screen.draw();
        screen.ui.app_mut().handle_paste("message draft");
        screen.ask("r1", "t1", "touch must-not-exist");
        screen.draw();
        let shown = screen.screen();
        assert!(shown.contains("3. Decline"));
        assert!(shown.contains("4. Decline and say what to do instead"));
        assert!(shown.contains("5. Decline and stop"));
        assert_eq!(screen.key(KeyCode::Char('4')), None, "300 ms guard");
        screen.draw();
        assert!(!screen.screen().contains("What to do instead:"));
        screen.read_the_box();
        if enter {
            for _ in 0..3 {
                assert_eq!(screen.key(KeyCode::Down), None);
            }
            assert_eq!(screen.key(KeyCode::Enter), None);
        } else {
            assert_eq!(screen.key(KeyCode::Char('4')), None);
        }
        assert_eq!(screen.key(KeyCode::Char('1')), None);
        assert_eq!(screen.key(KeyCode::Enter), None);
        screen.draw();
        assert!(screen.screen().contains("What to do instead:"));
        assert_eq!(screen.key(KeyCode::Enter), None, "empty input is not sent");
        for c in "Try ".chars() {
            assert_eq!(screen.key(KeyCode::Char(c)), None);
        }
        screen
            .ui
            .app_mut()
            .handle_paste("read\nonly\r\nplease\rnow");
        screen.draw();
        assert!(screen.screen().contains("› Try read only please now▏"));
        assert_eq!(
            screen.key(KeyCode::Enter),
            Some(Action::Resolve {
                request_id: "r1".into(),
                decision: ApprovalDecision::DeclineWithFeedback,
                feedback: Some("Try read only please now".into()),
            })
        );
        screen.draw();
        assert_eq!(
            screen.key(KeyCode::Enter),
            None,
            "draft is guarded after answering"
        );
        screen.ui.app_mut().handle_paste(" kept");
        screen.read_the_box();
        assert_eq!(
            screen.key(KeyCode::Enter),
            Some(Action::Send("message draft kept".into()))
        );
    }
}

#[test]
fn decline_feedback_empty_input_returns_to_options_and_escape_interrupts() {
    for back in [KeyCode::Backspace, KeyCode::Up] {
        let mut screen = Screen::new(100, 30);
        screen.start_turn("t1");
        screen.ask("r1", "t1", "touch must-not-exist");
        screen.draw();
        screen.read_the_box();
        assert_eq!(screen.key(KeyCode::Char('4')), None);
        screen.draw();
        assert_eq!(screen.key(KeyCode::Char('x')), None);
        assert_eq!(screen.key(KeyCode::Up), None);
        screen.draw();
        assert!(screen.screen().contains("› x▏"));
        assert_eq!(screen.key(KeyCode::Backspace), None);
        screen.draw();
        assert!(screen.screen().contains("What to do instead:"));
        assert_eq!(screen.key(back), None);
        assert_eq!(screen.key(KeyCode::Char('1')), None);
        assert_eq!(screen.key(KeyCode::Enter), None);
        screen.draw();
        assert!(
            screen
                .screen()
                .contains("4. Decline and say what to do instead")
        );
        assert_eq!(screen.key(KeyCode::Char('4')), None);
        screen.draw();
        assert_eq!(
            screen.key(KeyCode::Esc),
            Some(Action::Interrupt {
                turn_id: "t1".into()
            })
        );
    }
}

#[test]
fn decline_feedback_is_hidden_when_the_server_does_not_offer_it() {
    let mut screen = Screen::new(100, 30);
    screen.start_turn("t1");
    screen.ask_with_decisions(
        "r1",
        "t1",
        "touch must-not-exist",
        &["accept", "acceptForSession", "decline", "cancel"],
    );
    screen.draw();
    assert!(!screen.screen().contains("say what to do instead"));
    assert!(screen.screen().contains("4. Decline and stop"));
    screen.read_the_box();
    assert_eq!(
        screen.key(KeyCode::Char('4')),
        resolve("r1", ApprovalDecision::Cancel)
    );
}

#[test]
fn decline_feedback_shows_under_the_card_live_and_from_a_snapshot() {
    let mut screen = Screen::new(100, 30);
    screen.start_turn("t1");
    screen.ask("r1", "t1", "touch must-not-exist");
    screen.event(json!({
        "type":"request.resolved", "requestId":"r1", "clientName":"other terminal",
        "resolution":{"type":"answered", "decision":"declineWithFeedback", "feedback":"Try \u{001b}[31mreading\u{001b}[0m\u{0007} instead"}
    }));
    screen.event(json!({"type":"item.completed", "turnId":"t1",
        "item":command_json("item-r1", "touch must-not-exist", "declined")}));
    screen.finish_turn("t1");
    screen.draw();
    for reopened in [false, true] {
        if reopened {
            let serialized = serde_json::to_value(screen.ui.app().snapshot()).unwrap();
            let snapshot = serde_json::from_value(serialized).unwrap();
            screen.ui = Ui::new(TestBackend::new(100, 30), App::new(snapshot, None, NOW)).unwrap();
            screen.draw();
        }
        let shown = screen.screen();
        let lines: Vec<_> = shown.lines().collect();
        let card = lines
            .iter()
            .position(|line| line.contains("declined"))
            .unwrap();
        assert!(lines[card + 1].contains("Try reading instead"), "{shown}");
        assert!(screen.dim("Try reading instead"));
        assert!(!shown.contains("[31m"));
    }
}
