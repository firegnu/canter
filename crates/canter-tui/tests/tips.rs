//! The tip under the status line while the model works: a new one each
//! turn, none while idle or with a box open, none when it does not fit, and
//! none when `tui.json` says `"tips": false`.

use std::collections::BTreeMap;

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 0;

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    _home: tempfile::TempDir,
}

impl Screen {
    /// A screen with tips on, under a `CANTER_HOME` holding `tui_json`.
    fn new(width: u16, tui_json: Option<Value>) -> Self {
        let home = tempfile::tempdir().unwrap();
        if let Some(settings) = tui_json {
            std::fs::write(home.path().join("tui.json"), settings.to_string()).unwrap();
        }
        let env = BTreeMap::from([(
            "CANTER_HOME".to_owned(),
            home.path().to_str().unwrap().to_owned(),
        )]);
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW).with_tips(&env);
        let mut screen = Self {
            ui: Ui::new(TestBackend::new(width, 30), app).unwrap(),
            seq: 0,
            _home: home,
        };
        screen.draw();
        screen
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
        self.draw();
    }

    fn draw(&mut self) {
        self.ui.draw(NOW).unwrap();
    }

    fn key(&mut self, code: KeyCode) {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), NOW);
        self.draw();
    }

    fn start_turn(&mut self, turn: &str) {
        self.event(json!({ "type": "turn.started", "turn": turn_json(turn, "inProgress") }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn end_turn(&mut self, turn: &str) {
        self.event(json!({ "type": "turn.completed", "turn": turn_json(turn, "completed") }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
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

    /// The tip on screen, checked to be right under the status line.
    fn tip(&self) -> Option<String> {
        let rows = self.rows();
        let at = rows.iter().position(|row| row.starts_with(" Tip: "))?;
        assert!(
            rows[at - 1].contains("Esc to interrupt"),
            "not under the status line:\n{}",
            rows.join("\n")
        );
        Some(rows[at].clone())
    }
}

fn turn_json(id: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "do it" }],
        "clientName": "canter-tui", "usage": null,
    })
}

#[test]
fn a_tip_shows_while_working_and_not_while_idle() {
    let mut screen = Screen::new(100, None);
    assert_eq!(screen.tip(), None, "not when the session opens");
    screen.start_turn("t1");
    assert!(screen.tip().is_some());
    screen.end_turn("t1");
    assert_eq!(screen.tip(), None, "not once the turn ends");
}

#[test]
fn not_while_a_box_is_open() {
    let mut screen = Screen::new(100, None);
    screen.start_turn("t1");
    assert!(screen.tip().is_some());
    screen.key(KeyCode::Char('?'));
    assert!(screen.rows().iter().any(|row| row.contains("Help")));
    assert_eq!(screen.tip(), None);
    screen.key(KeyCode::Esc);
    assert!(screen.tip().is_some(), "back when the box closes");
}

#[test]
fn the_same_tip_all_turn_and_another_next_turn() {
    let mut screen = Screen::new(100, None);
    screen.start_turn("t1");
    let first = screen.tip().unwrap();
    screen.event(json!({
        "type": "item.started", "turnId": "t1",
        "item": { "type": "agentMessage", "id": "m1", "text": "" },
    }));
    screen.event(json!({
        "type": "item.delta", "turnId": "t1", "itemId": "m1",
        "delta": { "type": "agentMessage", "delta": "Working on it" },
    }));
    assert_eq!(screen.tip().as_ref(), Some(&first));
    screen.end_turn("t1");
    screen.start_turn("t2");
    let second = screen.tip().unwrap();
    assert_ne!(second, first);
}

#[test]
fn not_when_the_whole_tip_does_not_fit() {
    let mut screen = Screen::new(100, None);
    screen.start_turn("t1");
    let tip = screen.tip().unwrap();
    screen.ui.backend_mut().resize(tip.width() as u16 - 1, 30);
    screen.draw();
    assert_eq!(screen.tip(), None, "not cut, not wrapped");
    screen.ui.backend_mut().resize(tip.width() as u16, 30);
    screen.draw();
    assert_eq!(screen.tip(), Some(tip));
}

#[test]
fn not_when_tui_json_turns_tips_off() {
    let mut screen = Screen::new(100, Some(json!({ "tips": false })));
    screen.start_turn("t1");
    assert!(
        screen
            .rows()
            .iter()
            .any(|row| row.contains("Esc to interrupt"))
    );
    assert_eq!(screen.tip(), None);
}
