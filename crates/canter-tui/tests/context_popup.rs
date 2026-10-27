//! `/context`: the result in a box above the input box, not in the
//! conversation. Esc or Enter closes it and leaves the conversation as it
//! was; the draft stays. In a short window the section rows scroll with
//! Up and Down.

use canter_protocol::{ContextResponse, Event, SessionSnapshot};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const WIDTH: u16 = 80;
const TITLE: &str = " Context  ctx 12% 120k/1M (last request)";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new(height: u16) -> Self {
        let app = App::new(
            SessionSnapshot::empty(SESSION),
            Some("/home/test".into()),
            0,
        );
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(WIDTH, height), app).unwrap(),
            seq: 0,
        };
        screen.event(json!({
            "type": "session.updated",
            "settings": { "cwd": "/work", "name": null, "model": "test-model",
                          "approvalPolicy": "alwaysAsk" },
        }));
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

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    /// `/context` and Enter, then the answer from the server.
    fn open(&mut self) {
        self.type_text("/context");
        assert_eq!(self.key(KeyCode::Enter), Some(Action::Context));
        self.ui.app_mut().show_context(&report());
        self.draw();
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

    /// The rows from the title down to the input box; empty while closed.
    fn popup(&self) -> Vec<String> {
        let rows = self.rows();
        match rows.iter().position(|row| row == TITLE) {
            Some(start) => rows[start..self.box_top()].to_vec(),
            None => Vec::new(),
        }
    }

    /// The input box's first row, without the border.
    fn input(&self) -> String {
        self.rows()[self.box_top() + 1]
            .trim_matches('│')
            .trim()
            .to_owned()
    }
}

/// The picture the user saw, plus a compaction summary.
fn report() -> ContextResponse {
    serde_json::from_value(json!({
        "prefix": [
            { "kind": "system", "path": null, "tokens": 2300 },
            { "kind": "globalInstructions", "path": "/home/test/.agents/AGENTS.md", "tokens": 1100 },
            { "kind": "projectInstructions", "path": "/work/AGENTS.md", "tokens": 3400 },
            { "kind": "environment", "path": null, "tokens": 80 },
        ],
        "skillCount": 0, "toolCount": 6, "toolTokens": 2900, "compactionTokens": 1500,
        "messages": { "count": 42, "tokens": 110_000, "user": 8000, "assistant": 31_000,
                      "toolResults": 70_000, "images": 1000 },
        "estimatedTokens": 120_000, "freeTokens": 880_000,
        "modelContextWindow": 1_000_000, "contextTokens": 120_000,
    }))
    .unwrap()
}

const SECTIONS: [&str; 8] = [
    "   System prompt                2k",
    "   Global AGENTS.md             1k  ~/.agents/AGENTS.md",
    "   Project AGENTS.md            3k  /work/AGENTS.md",
    "   Environment                  80",
    "   Tools (6)                    3k",
    "   Compaction summary           2k",
    "   Messages (42)              110k  user 8k · assistant 31k · tool results 70k …",
    "   Free                       880k",
];

const NOTES: [&str; 4] = [
    "",
    " Estimated by size; the first line is what the model reported.",
    " Currently loaded context; queued input and future tool results are not included.",
    " Esc close",
];

fn shown(sections: &[&str]) -> Vec<String> {
    [TITLE, ""]
        .iter()
        .chain(sections)
        .chain(&NOTES[..])
        .map(|row| row.to_string())
        .collect()
}

#[test]
fn context_opens_above_the_input_box_and_not_in_the_conversation() {
    let mut screen = Screen::new(30);
    screen.open();
    let mut expected = shown(&SECTIONS);
    // The second note and the messages row are wider than the window.
    expected[12] =
        " Currently loaded context; queued input and future tool results are not include…".into();
    assert_eq!(screen.popup(), expected);
    let rows = screen.rows();
    let start = rows.iter().position(|row| row == TITLE).unwrap();
    assert!(
        rows[..start]
            .iter()
            .all(|row| !row.contains("System prompt")),
        "{rows:#?}"
    );
}

#[test]
fn esc_or_enter_closes_it_and_the_conversation_is_as_before() {
    for close in [KeyCode::Esc, KeyCode::Enter] {
        let mut screen = Screen::new(30);
        let before = screen.rows();
        screen.open();
        assert!(!screen.popup().is_empty(), "open");
        assert_eq!(screen.key(close), None, "{close:?}");
        assert!(screen.popup().is_empty(), "{close:?} closes it");
        assert_eq!(screen.rows(), before, "{close:?}");
    }
}

#[test]
fn the_draft_stays_while_it_is_open_and_after() {
    let mut screen = Screen::new(30);
    screen.type_text("/context");
    assert_eq!(screen.key(KeyCode::Enter), Some(Action::Context));
    // Typed while the server answers.
    screen.type_text("half typed");
    screen.ui.app_mut().show_context(&report());
    screen.draw();
    assert_eq!(screen.input(), "› half typed");
    // Keys go to the box while it is open.
    screen.type_text("xy");
    assert_eq!(screen.input(), "› half typed");
    assert_eq!(screen.key(KeyCode::Esc), None);
    assert!(screen.popup().is_empty());
    screen.type_text(" more");
    assert_eq!(screen.input(), "› half typed more");
}

#[test]
fn in_a_short_window_the_sections_scroll_and_the_rest_stays() {
    // 11 rows above the input box: 5 for the sections.
    let mut screen = Screen::new(17);
    screen.open();
    let popup = screen.popup();
    let check = |popup: Vec<String>, sections: &[&str]| {
        let expected = shown(sections);
        assert_eq!(popup[..7], expected[..7]);
        assert_eq!(popup[7], NOTES[0]);
        assert!(popup[8].starts_with(" Estimated by size"), "{popup:#?}");
        assert!(
            popup[9].starts_with(" Currently loaded context"),
            "{popup:#?}"
        );
        assert_eq!(popup[10], NOTES[3]);
        assert_eq!(popup.len(), 11);
    };
    check(
        popup,
        &[
            SECTIONS[0],
            SECTIONS[1],
            SECTIONS[2],
            SECTIONS[3],
            "   ↓ 4 more",
        ],
    );
    screen.key(KeyCode::Down);
    check(
        screen.popup(),
        &[
            "   ↑ 1 more",
            SECTIONS[1],
            SECTIONS[2],
            SECTIONS[3],
            "   ↓ 4 more",
        ],
    );
    screen.key(KeyCode::Down);
    screen.key(KeyCode::Down);
    check(
        screen.popup(),
        &[
            "   ↑ 3 more",
            SECTIONS[3],
            SECTIONS[4],
            SECTIONS[5],
            "   ↓ 2 more",
        ],
    );
    // At the bottom the last row takes the place of the count below.
    for _ in 0..5 {
        screen.key(KeyCode::Down);
    }
    check(
        screen.popup(),
        &[
            "   ↑ 4 more",
            SECTIONS[4],
            SECTIONS[5],
            SECTIONS[6],
            SECTIONS[7],
        ],
    );
    screen.key(KeyCode::Up);
    check(
        screen.popup(),
        &[
            "   ↑ 3 more",
            SECTIONS[3],
            SECTIONS[4],
            SECTIONS[5],
            "   ↓ 2 more",
        ],
    );
    for _ in 0..5 {
        screen.key(KeyCode::Up);
    }
    check(
        screen.popup(),
        &[
            SECTIONS[0],
            SECTIONS[1],
            SECTIONS[2],
            SECTIONS[3],
            "   ↓ 4 more",
        ],
    );
}
