//! How long a turn works and how many tokens it uses: the status line's verb
//! in a colour of its own and bold, its time and output tokens in the plain
//! text colour, with a blank row above it and the plan step and the tip
//! hung under it after `⎿`; and once the turn ends, a grey
//! `Worked for 21m 45s · ↑ 1.27M (98% cached) · ↓ 26.2k · HH:MM` row.

use std::collections::BTreeMap;

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{App, Ui};
use chrono::TimeZone;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
/// A fixed moment, in Unix ms, at the start of a sweep of the verb's light,
/// so the light is off the verb at every whole two seconds after it.
const T0: i64 = 1_800_000_000_000;
const HEIGHT: u16 = 30;

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    now: i64,
    turn: String,
    _home: tempfile::TempDir,
}

impl Screen {
    fn new(width: u16) -> Self {
        Self::themed(Theme::default(), width)
    }

    /// With tips on, under an empty `CANTER_HOME`.
    fn themed(theme: Theme, width: u16) -> Self {
        theme::set(theme);
        let home = tempfile::tempdir().unwrap();
        let env = BTreeMap::from([(
            "CANTER_HOME".to_owned(),
            home.path().to_str().unwrap().to_owned(),
        )]);
        let app = App::new(SessionSnapshot::empty(SESSION), None, T0).with_tips(&env);
        Self::with_app(app, width, home)
    }

    fn with_app(app: App, width: u16, home: tempfile::TempDir) -> Self {
        let ui = Ui::new(TestBackend::new(width, HEIGHT), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            now: T0,
            turn: String::new(),
            _home: home,
        };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen
    }

    fn wait(&mut self, ms: i64) {
        self.now += ms;
    }

    fn event(&mut self, payload: Value) {
        self.seq += 1;
        let mut event = json!({ "seq": self.seq, "sessionId": SESSION, "timestamp": self.now });
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        let event: Event = serde_json::from_value(event).unwrap();
        self.ui.app_mut().apply_event(&event, self.now);
    }

    fn turn_json(&self, status: &str, error: Value, usage: Value) -> Value {
        json!({
            "id": self.turn, "items": [], "status": status, "error": error,
            "input": [{ "type": "text", "text": format!("prompt {}", self.turn) }],
            "clientName": "canter-tui", "usage": usage,
        })
    }

    fn start_turn(&mut self, id: &str) {
        self.turn = id.to_owned();
        let turn = self.turn_json("inProgress", Value::Null, Value::Null);
        self.event(json!({ "type": "turn.started", "turn": turn }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    /// Ends the turn with the usage the server kept for it.
    fn end_turn(&mut self, status: &str, error: Value, usage: Value) {
        let turn = self.turn_json(status, error, usage);
        self.event(json!({ "type": "turn.completed", "turn": turn }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    fn say(&mut self, id: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": id, "text": text });
        self.event(json!({ "type": "item.started", "turnId": self.turn, "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": self.turn, "item": message }));
    }

    fn plan(&mut self, steps: Value) {
        self.event(json!({
            "type": "item.completed", "turnId": self.turn,
            "item": { "type": "plan", "id": "p1", "status": "completed",
                      "explanation": "Progress", "plan": steps },
        }));
    }

    /// A request of this session done, as `session.usage` (or
    /// `session.btwUsage`) says it.
    fn used(&mut self, kind: &str, last: Value) {
        self.event(json!({ "type": kind, "tokenUsage": {
            "total": last, "last": last, "contextTokens": 0, "modelContextWindow": 1_000_000,
        }}));
    }

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(self.now).unwrap();
        self.ui.backend().buffer()
    }

    fn rows(&mut self) -> Vec<String> {
        let buffer = self.draw();
        (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect()
    }

    /// Where the status line is.
    fn status_at(&mut self) -> usize {
        let rows = self.rows();
        rows.iter()
            .position(|row| row.contains("Esc to interrupt"))
            .unwrap_or_else(|| panic!("no status line:\n{}", rows.join("\n")))
    }

    fn status(&mut self) -> String {
        let at = self.status_at();
        self.rows()[at].clone()
    }
}

fn row(buffer: &Buffer, y: u16) -> String {
    let mut row = String::new();
    let mut x = 0;
    while x < buffer.area.width {
        let symbol = buffer[(x, y)].symbol();
        row.push_str(symbol);
        x += symbol.width().max(1) as u16;
    }
    row
}

/// The cells of the first row containing `text`, from where it starts.
fn cells_of<'a>(buffer: &'a Buffer, text: &str) -> Vec<&'a Cell> {
    for y in 0..buffer.area.height {
        let row = row(buffer, y);
        if let Some(byte) = row.find(text) {
            let x = row[..byte].width() as u16;
            let end = x + text.width() as u16;
            return (x..end).map(|x| &buffer[(x, y)]).collect();
        }
    }
    panic!("{text:?} is not on screen");
}

fn letters<'a>(cells: &[&'a Cell]) -> Vec<&'a Cell> {
    cells
        .iter()
        .copied()
        .filter(|cell| cell.symbol() != " " && !cell.symbol().is_empty())
        .collect()
}

fn counts(input: u64, cached: u64, output: u64) -> Value {
    json!({
        "inputTokens": input, "cachedInputTokens": cached, "outputTokens": output,
        "reasoningOutputTokens": output / 2, "totalTokens": input + output,
    })
}

fn usage(input: u64, cached: u64, output: u64) -> Value {
    json!({
        "total": counts(input, cached, output), "last": counts(input, cached, output),
        "contextTokens": 0, "modelContextWindow": 1_000_000,
    })
}

/// `HH:MM` in local time at Unix milliseconds `ms`.
fn clock(ms: i64) -> String {
    chrono::Local
        .timestamp_opt(ms.div_euclid(1000), 0)
        .unwrap()
        .format("%H:%M")
        .to_string()
}

const DARK: Theme = Theme {
    depth: Depth::TrueColor,
    background: Background::Dark,
};
const LIGHT: Theme = Theme {
    depth: Depth::TrueColor,
    background: Background::Light,
};

/// The spinner and the verb (with its `…`) are bold in a colour of their
/// own, on a dark or a light background or with 16 colours; the time is in
/// the plain text colour, the keys are dim.
#[test]
fn the_working_verb_stands_out_and_the_time_is_plain() {
    let ansi256 = Theme {
        depth: Depth::Ansi256,
        background: Background::Unknown,
    };
    for theme in [DARK, LIGHT, ansi256, Theme::default()] {
        let mut screen = Screen::themed(theme, 100);
        screen.start_turn("t1");
        screen.wait(284_000);
        let status = screen.status();
        assert!(
            status.contains("Galloping… · 4m 44s · Esc to interrupt"),
            "{theme:?}: {status:?}"
        );
        let buffer = screen.draw().clone();
        let verb = letters(&cells_of(&buffer, "Galloping…"));
        let colour = verb[0].fg;
        assert_ne!(colour, Color::Reset, "{theme:?}");
        for cell in &verb {
            assert!(
                cell.modifier.contains(Modifier::BOLD),
                "{theme:?}: {cell:?}"
            );
            assert!(
                !cell.modifier.contains(Modifier::DIM),
                "{theme:?}: {cell:?}"
            );
            assert_eq!(cell.fg, colour, "{theme:?}: the light is off the verb");
        }
        // The spinner, right before the verb.
        let at = status.find("Galloping").unwrap();
        let spinner = status[..at].trim().to_owned();
        let spinner = letters(&cells_of(&buffer, &spinner));
        assert_eq!(spinner[0].fg, colour, "{theme:?}");
        assert!(spinner[0].modifier.contains(Modifier::BOLD), "{theme:?}");
        if let Color::Rgb(r, g, b) = colour {
            assert!(r > g && g > b, "{theme:?}: orange, not {colour:?}");
        }
        for cell in letters(&cells_of(&buffer, "4m 44s")) {
            assert!(
                !cell.modifier.contains(Modifier::DIM),
                "{theme:?}: {cell:?}"
            );
            assert_eq!(cell.fg, Color::Reset, "{theme:?}: {cell:?}");
        }
        for cell in letters(&cells_of(&buffer, "Esc to interrupt")) {
            assert!(cell.modifier.contains(Modifier::DIM), "{theme:?}: {cell:?}");
        }

        // A second on, the light is on the verb: brighter there.
        screen.wait(1_000);
        let buffer = screen.draw().clone();
        let verb = letters(&cells_of(&buffer, "Galloping…"));
        assert!(verb.iter().any(|cell| cell.fg != colour), "{theme:?}");
        assert!(
            verb.iter()
                .all(|cell| cell.modifier.contains(Modifier::BOLD)),
            "{theme:?}"
        );
    }
}

/// `NO_COLOR`: the verb is only bold, and nothing sweeps over it.
#[test]
fn without_colours_the_verb_is_only_bold() {
    let mut screen = Screen::themed(
        Theme {
            depth: Depth::None,
            background: Background::Unknown,
        },
        100,
    );
    screen.start_turn("t1");
    screen.wait(1_000);
    let buffer = screen.draw().clone();
    for cell in letters(&cells_of(&buffer, "Galloping…")) {
        assert!(cell.modifier.contains(Modifier::BOLD), "{cell:?}");
        assert_eq!(cell.fg, Color::Reset, "{cell:?}");
    }
    for cell in letters(&cells_of(&buffer, "1s")) {
        assert!(!cell.modifier.contains(Modifier::DIM), "{cell:?}");
    }
}

/// The turn's tokens so far, sent and received, show after its time once a
/// request of this session is done, each request adding to them; a `/btw`
/// request does not. When the window is too narrow the sent ones go first,
/// then the received ones, before the time does.
#[test]
fn the_status_line_counts_the_turns_tokens() {
    let mut screen = Screen::new(100);
    screen.start_turn("t1");
    screen.wait(2_000);
    let status = screen.status();
    assert!(
        status.contains("Galloping… · 2s · Esc to interrupt"),
        "no usage yet: {status:?}"
    );
    screen.used("session.usage", counts(30_000, 20_000, 26_200));
    let status = screen.status();
    assert!(
        status.contains("Galloping… · 2s · ↑ 30k · ↓ 26.2k tokens · Esc to interrupt"),
        "{status:?}"
    );
    screen.used("session.btwUsage", counts(5_000, 0, 4_000));
    screen.used("session.usage", counts(31_000, 30_000, 1_000));
    let status = screen.status();
    assert!(
        status.contains("· 2s · ↑ 61k · ↓ 27.2k tokens ·"),
        "{status:?}"
    );

    let prefix = status.find(" · 2s").unwrap() + " · 2s".len();
    let status_row = |screen: &mut Screen| {
        let rows = screen.rows();
        rows.iter()
            .find(|row| row.contains("Galloping…"))
            .unwrap_or_else(|| panic!("{}", rows.join("\n")))
            .clone()
    };
    let width = (status[..prefix].width() + " · ↓ 27.2k tokens".width()) as u16;
    screen.ui.backend_mut().resize(width, HEIGHT);
    let status = status_row(&mut screen);
    assert!(status.contains("· 2s · ↓ 27.2k tokens"), "{status:?}");
    assert!(!status.contains('↑'), "{status:?}");

    let width = status[..prefix].width() as u16 + 4;
    screen.ui.backend_mut().resize(width, HEIGHT);
    let status = status_row(&mut screen);
    assert!(status.contains("· 2s"), "{status:?}");
    assert!(!status.contains('↓'), "{status:?}");
}

/// A finished turn says how long it worked, the input tokens of all its
/// requests with how many of them were cached, and its output tokens: the
/// numbers the server kept for the turn. Grey, as `✻ Thought for 2s`.
#[test]
fn a_finished_turn_says_how_long_it_worked_and_what_it_used() {
    let mut screen = Screen::new(100);
    screen.start_turn("t1");
    screen.wait(185_400);
    screen.say("m1", "First answer.");
    let rows = screen.rows();
    assert!(
        !rows.iter().any(|row| row.contains("Worked for")),
        "{rows:?}"
    );
    screen.end_turn(
        "completed",
        Value::Null,
        usage(1_270_000, 1_245_000, 26_200),
    );
    let expected = format!(
        "  Worked for 3m 05s · ↑ 1.27M (98% cached) · ↓ 26.2k · {}",
        clock(T0 + 185_400)
    );
    let rows = screen.rows();
    assert!(rows.contains(&expected), "{expected:?} in {rows:#?}");
    let buffer = screen.draw().clone();
    for cell in letters(&cells_of(&buffer, "Worked for 3m 05s · ↑ 1.27M")) {
        assert!(cell.modifier.contains(Modifier::DIM), "{cell:?}");
    }
}

/// Interrupted and failed turns keep their row, and get the same
/// `Worked for …` row after it.
#[test]
fn an_interrupted_or_failed_turn_says_what_it_used_too() {
    let mut screen = Screen::new(100);
    screen.start_turn("t1");
    screen.wait(12_300);
    screen.say("m1", "Working on it.");
    screen.end_turn("interrupted", Value::Null, usage(1_500, 0, 950));
    let ended = T0 + 12_300;
    screen.wait(1_000);
    screen.start_turn("t2");
    screen.wait(61_000);
    screen.end_turn(
        "failed",
        json!({ "message": "boom", "additionalDetails": null }),
        usage(40_000, 30_000, 1_000),
    );
    let rows = screen.rows();
    let at = |text: &str| {
        rows.iter()
            .position(|row| row == text)
            .unwrap_or_else(|| panic!("{text:?} in {rows:#?}"))
    };
    let interrupted = at("  ■ Turn interrupted");
    assert_eq!(
        rows[interrupted + 1],
        format!(
            "  Worked for 12s · ↑ 1.5k (0% cached) · ↓ 950 · {}",
            clock(ended)
        )
    );
    let failed = at("  ✗ Turn failed: boom");
    assert_eq!(
        rows[failed + 1],
        format!(
            "  Worked for 1m 01s · ↑ 40k (75% cached) · ↓ 1k · {}",
            clock(ended + 62_000)
        )
    );
}

/// A turn from before usage was kept says how long it worked only; one
/// from before turns were timed has no such row.
#[test]
fn old_turns_say_what_they_know() {
    let mut screen = Screen::new(100);
    screen.start_turn("t1");
    screen.wait(4_000);
    screen.end_turn("completed", Value::Null, Value::Null);
    let rows = screen.rows();
    let expected = format!("  Worked for 4s · {}", clock(T0 + 4_000));
    assert!(rows.contains(&expected), "{expected:?} in {rows:#?}");

    let old: SessionSnapshot = serde_json::from_value(json!({
        "id": SESSION,
        "settings": { "cwd": "/work", "name": null, "model": "m", "approvalPolicy": "alwaysAsk" },
        "status": "idle", "createdAt": 1_000, "updatedAt": 5_000,
        "turns": [{
            "id": "t0", "status": "completed", "error": null,
            "items": [{ "type": "agentMessage", "id": "m0", "text": "Old answer." }],
            "input": [{ "type": "text", "text": "old prompt" }],
            "clientName": "canter-tui", "usage": usage(1_000, 0, 10),
        }],
        "pendingRequests": [], "queuedInputs": [], "tokenUsage": null, "error": null,
        "lastEventSeq": 9,
    }))
    .unwrap();
    theme::set(Theme::default());
    let mut screen = Screen::with_app(App::new(old, None, T0), 100, tempfile::tempdir().unwrap());
    let rows = screen.rows();
    assert!(
        rows.iter().any(|row| row.contains("Old answer.")),
        "{rows:#?}"
    );
    assert!(
        !rows.iter().any(|row| row.contains("Worked for")),
        "{rows:#?}"
    );
}

/// While working, a blank row sets the status line off from the
/// conversation, and the tip hangs under it after `⎿`; a plan's steps do
/// not. When idle the rows are as before.
#[test]
fn the_status_line_stands_apart_with_the_tip_under_it_and_no_plan() {
    let mut screen = Screen::new(100);
    screen.start_turn("t1");
    // More than the screen holds, so the conversation runs down to the
    // rows under it.
    for n in 0..HEIGHT {
        screen.say(&format!("n{n}"), &format!("Note {n}."));
    }
    screen.say("m1", "Working on it.");
    let at = screen.status_at();
    let rows = screen.rows();
    assert_eq!(rows[at - 1], "", "{rows:#?}");
    assert_eq!(rows[at - 2], "● Working on it.", "{rows:#?}");
    assert!(rows[at + 1].starts_with("   ⎿ Tip: "), "{rows:#?}");

    screen.plan(json!([
        { "step": "Read", "status": "completed" },
        { "step": "Implement", "status": "inProgress" },
        { "step": "Verify", "status": "pending" },
    ]));
    let at = screen.status_at();
    let rows = screen.rows();
    assert_eq!(rows[at - 1], "", "{rows:#?}");
    assert!(rows[at + 1].starts_with("   ⎿ Tip: "), "{rows:#?}");
    assert!(rows[at + 2].starts_with('╭'), "{rows:#?}");

    screen.end_turn("completed", Value::Null, Value::Null);
    let rows = screen.rows();
    let input = rows.iter().position(|row| row.starts_with('╭')).unwrap();
    assert_eq!(rows[input - 1], "", "the status line, empty when idle");
    assert!(
        rows[input - 2].starts_with("  Worked for"),
        "no blank row above it: {rows:#?}"
    );
}
