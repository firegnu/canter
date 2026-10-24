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
                    "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
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
    })
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
                "4. Decline and stop",
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
