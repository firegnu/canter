//! Searching the conversation: Ctrl+F opens `Find:` in place of the input
//! box, every change of the words searches again from the newest, the match
//! is reversed and the others underlined, the status line counts them; Up
//! and Down (Ctrl+P / Ctrl+N) go to older and newer matches, Enter stays
//! there, Esc goes back. Only what is on screen is searched, not what is
//! folded, and case does not matter.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::Modifier;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
const NOW: i64 = 1_000_000;
const WIDTH: u16 = 60;
const HEIGHT: u16 = 20;
/// Rows under the conversation: the status line, the input box (3) and the
/// info rows (2).
const BELOW: u16 = 6;
const HINTS: &str = "↑ older · ↓ newer · Enter stay · Esc back";
const FOLDED: &str = "Ctrl+O to search folded parts";

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new() -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let ui = Ui::new(TestBackend::new(WIDTH, HEIGHT), app).unwrap();
        let mut screen = Self { ui, seq: 0 };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
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
    }

    fn start_turn(&mut self, turn: &str, text: &str) {
        self.event(json!({ "type": "turn.started", "turn": turn_json(turn, text, "inProgress") }));
        self.event(json!({ "type": "session.status", "status": "working" }));
    }

    fn finish_turn(&mut self, turn: &str, text: &str) {
        self.event(json!({ "type": "turn.completed", "turn": turn_json(turn, text, "completed") }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    fn say(&mut self, turn: &str, item: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": item, "text": text });
        self.event(json!({ "type": "item.started", "turnId": turn, "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": turn, "item": message }));
    }

    /// A finished turn: 40 lines, `README` on lines 5, 20 and 38, then an
    /// answer that names it once more: four matches, the last one at the
    /// bottom of the screen.
    fn readme_turn() -> Self {
        let mut screen = Self::new();
        screen.start_turn("t1", "show the lines");
        let lines: Vec<String> = (1..=40)
            .map(|n| match n {
                5 | 20 | 38 => format!("line {n} README"),
                _ => format!("line {n}"),
            })
            .collect();
        screen.say("t1", "m1", &lines.join("\n"));
        screen.say("t1", "m2", "Next, update the README.");
        screen.finish_turn("t1", "show the lines");
        screen.draw();
        screen
    }

    fn draw(&mut self) {
        self.ui.draw(NOW).unwrap();
    }

    fn buffer(&self) -> &Buffer {
        self.ui.backend().buffer()
    }

    fn lines(&self) -> Vec<String> {
        (0..self.buffer().area.height)
            .map(|y| row(self.buffer(), y).trim_end().to_owned())
            .collect()
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// The conversation rows on screen.
    fn conversation(&self) -> Vec<String> {
        self.lines()[..usize::from(HEIGHT - BELOW)].to_vec()
    }

    fn status(&self) -> String {
        row(self.buffer(), HEIGHT - BELOW).trim().to_owned()
    }

    /// The input box's first row, without its borders.
    fn input(&self) -> String {
        row(self.buffer(), HEIGHT - BELOW + 2)
            .trim_start_matches('│')
            .trim()
            .trim_end_matches('│')
            .trim()
            .to_owned()
    }

    /// The row and column where `text` first starts.
    fn find(&self, text: &str) -> (u16, u16) {
        let buffer = self.buffer();
        for y in 0..buffer.area.height {
            let row = row(buffer, y);
            if let Some(byte) = row.find(text) {
                return (row[..byte].width() as u16, y);
            }
        }
        panic!("{text:?} is not on screen:\n{}", self.text());
    }

    /// For each row with cells that have `modifier`, the row, the first
    /// such cell and their text.
    fn styled(&self, modifier: Modifier) -> Vec<(u16, u16, String)> {
        let buffer = self.buffer();
        (0..buffer.area.height)
            .filter_map(|y| {
                let mut text = String::new();
                let mut first = None;
                let mut x = 0;
                while x < buffer.area.width {
                    let cell = &buffer[(x, y)];
                    if cell.modifier.contains(modifier) {
                        first.get_or_insert(x);
                        text.push_str(cell.symbol());
                    }
                    x += cell.symbol().width().max(1) as u16;
                }
                first.map(|x| (y, x, text))
            })
            .collect()
    }

    fn reversed(&self) -> Vec<(u16, u16, String)> {
        self.styled(Modifier::REVERSED)
    }

    fn underlined(&self) -> Vec<(u16, u16, String)> {
        self.styled(Modifier::UNDERLINED)
    }

    /// The text of the reversed match and the row it is on.
    fn current(&self) -> (String, String) {
        let reversed = self.reversed();
        assert_eq!(reversed.len(), 1, "one match reversed:\n{}", self.text());
        let (y, _, text) = &reversed[0];
        (text.clone(), self.lines()[usize::from(*y)].clone())
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.key_with(code, KeyModifiers::NONE)
    }

    fn key_with(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, modifiers), NOW)
    }

    fn ctrl(&mut self, c: char) -> Option<Action> {
        self.key_with(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(&mut self, text: &str) {
        for c in text.chars() {
            self.key(KeyCode::Char(c));
        }
    }

    /// Ctrl+F, types `query` and draws.
    fn search(&mut self, query: &str) {
        self.ctrl('f');
        self.type_text(query);
        self.draw();
    }

    /// Presses `code` and draws.
    fn press(&mut self, code: KeyCode) {
        self.key(code);
        self.draw();
    }
}

fn turn_json(id: &str, text: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": text }],
        "clientName": "canter-tui", "usage": null,
    })
}

/// A buffer row; cells hidden under a wide character are skipped.
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

fn find_status(at: usize, count: usize) -> String {
    format!("Find · {at}/{count} · {HINTS}")
}

/// The sketch in the task: Ctrl+F turns the input box into `Find:`; typing
/// jumps to the newest match, reversed, with the others on screen
/// underlined and the matches counted.
#[test]
fn typing_finds_the_newest_match_and_counts_them() {
    let mut screen = Screen::readme_turn();
    screen.ctrl('f');
    screen.draw();
    assert_eq!(screen.input(), "Find:", "{}", screen.text());
    assert!(
        screen.reversed().is_empty(),
        "nothing yet:\n{}",
        screen.text()
    );
    assert!(screen.underlined().is_empty(), "{}", screen.text());

    screen.type_text("readme");
    screen.draw();
    assert_eq!(screen.input(), "Find: readme");
    assert_eq!(screen.status(), find_status(4, 4), "{}", screen.text());
    let (text, line) = screen.current();
    assert_eq!(text, "README");
    assert!(line.contains("Next, update the README."), "{line}");
    let answer = screen.find("Next, update");
    assert_eq!(
        screen.reversed()[0].1,
        answer.0 + "Next, update the ".width() as u16
    );
    let underlined = screen.underlined();
    assert_eq!(underlined.len(), 1, "{}", screen.text());
    let (y, _, text) = &underlined[0];
    assert_eq!(text, "README");
    assert!(screen.lines()[usize::from(*y)].contains("line 38 README"));
}

/// Up or Ctrl+P goes to an older match, scrolling it into view; Down or
/// Ctrl+N to a newer one. At either end it stays.
#[test]
fn up_and_down_go_through_the_matches_and_stop_at_the_ends() {
    let mut screen = Screen::readme_turn();
    screen.search("readme");
    screen.press(KeyCode::Up);
    assert_eq!(screen.status(), find_status(3, 4));
    assert!(screen.current().1.contains("line 38 README"));

    screen.ctrl('p');
    screen.draw();
    assert_eq!(screen.status(), find_status(2, 4));
    assert!(
        screen.current().1.contains("line 20 README"),
        "{}",
        screen.text()
    );

    screen.press(KeyCode::Up);
    assert_eq!(screen.status(), find_status(1, 4));
    assert!(screen.current().1.contains("line 5 README"));
    screen.press(KeyCode::Up);
    assert_eq!(screen.status(), find_status(1, 4), "stays at the oldest");
    assert!(screen.current().1.contains("line 5 README"));

    screen.ctrl('n');
    screen.draw();
    assert_eq!(screen.status(), find_status(2, 4));
    assert!(screen.current().1.contains("line 20 README"));
    screen.press(KeyCode::Down);
    screen.press(KeyCode::Down);
    assert_eq!(screen.status(), find_status(4, 4));
    assert!(screen.current().1.contains("Next, update the README."));
    screen.press(KeyCode::Down);
    assert_eq!(screen.status(), find_status(4, 4), "stays at the newest");
}

/// Enter closes the search and stays at the match: the highlight goes, the
/// view does not move, and new output does not pull it down.
#[test]
fn enter_stays_at_the_match() {
    let mut screen = Screen::readme_turn();
    screen.search("readme");
    screen.press(KeyCode::Up);
    screen.press(KeyCode::Up);
    screen.press(KeyCode::Up);
    let at = screen.find("line 5 README");
    let rows = screen.conversation();

    screen.press(KeyCode::Enter);
    assert!(!screen.status().contains("Find"), "{}", screen.text());
    assert!(screen.input().starts_with('›'), "{}", screen.text());
    assert!(screen.reversed().is_empty(), "{}", screen.text());
    assert!(screen.underlined().is_empty(), "{}", screen.text());
    assert_eq!(screen.find("line 5 README"), at);
    // The rows stay; the hint under them now offers End too.
    let last = rows.len() - 1;
    assert_eq!(screen.conversation()[..last], rows[..last]);

    screen.start_turn("t2", "more");
    screen.say("t2", "m3", "new answer");
    screen.draw();
    assert_eq!(screen.find("line 5 README"), at, "{}", screen.text());
}

/// Esc goes back where the view was before the search: following the
/// latest output, it is at the bottom and follows it again; scrolled back,
/// it is on the same rows as before.
#[test]
fn esc_goes_back_to_where_the_view_was() {
    let mut screen = Screen::readme_turn();
    let bottom = screen.conversation();
    screen.search("readme");
    screen.press(KeyCode::Up);
    screen.press(KeyCode::Up);
    assert_ne!(screen.conversation(), bottom, "the view moved");
    screen.press(KeyCode::Esc);
    assert!(!screen.status().contains("Find"), "{}", screen.text());
    assert_eq!(screen.conversation(), bottom);
    screen.start_turn("t2", "more");
    screen.say("t2", "m3", "new answer");
    screen.draw();
    assert!(
        screen.text().contains("new answer"),
        "follows the latest output again:\n{}",
        screen.text()
    );

    let mut screen = Screen::readme_turn();
    screen.press(KeyCode::PageUp);
    let back = screen.conversation();
    assert!(back.iter().any(|row| row.contains("line 20 README")));
    screen.search("readme");
    assert!(
        screen.current().1.contains("Next, update"),
        "{}",
        screen.text()
    );
    assert_ne!(screen.conversation(), back);
    screen.press(KeyCode::Esc);
    assert_eq!(screen.conversation(), back, "{}", screen.text());
}

/// Case does not matter, either way.
#[test]
fn case_does_not_matter() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "check");
    screen.say("t1", "m1", "Check the ReadMe file.");
    screen.draw();
    for query in ["README", "readme"] {
        screen.search(query);
        assert_eq!(screen.status(), find_status(1, 1), "{query}");
        assert_eq!(screen.current().0, "ReadMe", "{query}");
        screen.press(KeyCode::Esc);
    }
}

/// No match: the status line says so, and while things are folded, that
/// Ctrl+O shows them for the search.
#[test]
fn no_match_says_so_and_how_to_search_folded_parts() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "check");
    screen.say("t1", "m1", "Nothing to see.");
    screen.draw();
    screen.search("missing");
    assert_eq!(screen.status(), format!("Find · No matches · {FOLDED}"));
    assert!(screen.reversed().is_empty());

    screen.ctrl('o');
    screen.draw();
    assert_eq!(screen.status(), "Find · No matches", "{}", screen.text());
}

/// Folded parts (the rest of a thought, the lines a long output leaves
/// out) are not searched; after Ctrl+O they are.
#[test]
fn folded_parts_are_found_only_after_ctrl_o() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "think");
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "reasoning", "id": "r1", "summary": [],
        "content": ["First I look around.\nThe hidden plan is here."],
    }}));
    let output: Vec<String> = (1..=20)
        .map(|n| match n {
            2 => "buried output".to_owned(),
            _ => format!("out {n}"),
        })
        .collect();
    screen.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
        "type": "commandExecution", "id": "c1", "command": "make", "cwd": "/work",
        "status": "completed", "aggregatedOutput": output.join("\n"),
        "exitCode": 0, "durationMs": 1200,
    }}));
    screen.finish_turn("t1", "think");
    screen.draw();
    assert!(screen.text().contains("First I look around."));
    assert!(!screen.text().contains("hidden plan"), "{}", screen.text());
    assert!(!screen.text().contains("buried"), "{}", screen.text());

    for (query, found) in [("hidden plan", "hidden plan"), ("BURIED", "buried")] {
        screen.search(query);
        assert_eq!(
            screen.status(),
            format!("Find · No matches · {FOLDED}"),
            "{query}"
        );
        screen.ctrl('o');
        screen.draw();
        assert_eq!(
            screen.status(),
            find_status(1, 1),
            "{query}:\n{}",
            screen.text()
        );
        assert_eq!(screen.current().0, found);
        screen.ctrl('o');
        screen.press(KeyCode::Esc);
    }
}

/// The draft in the input box stays while searching and is back after.
#[test]
fn the_draft_is_kept() {
    let mut screen = Screen::readme_turn();
    screen.type_text("my draft");
    screen.draw();
    assert_eq!(screen.input(), "› my draft");
    for close in [KeyCode::Esc, KeyCode::Enter] {
        screen.search("readme");
        assert_eq!(screen.input(), "Find: readme");
        assert_eq!(screen.key(close), None, "nothing is sent");
        screen.draw();
        assert_eq!(screen.input(), "› my draft", "{close:?}");
    }
}

/// While the answer grows, new matches count, the view is not pulled down
/// and the match stays on its text.
#[test]
fn the_match_stays_while_the_answer_grows() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "stream");
    let first: Vec<String> = (1..=12).map(|n| format!("alpha {n}")).collect();
    screen.event(json!({ "type": "item.started", "turnId": "t1",
        "item": { "type": "agentMessage", "id": "m1", "text": first.join("\n") + "\n" } }));
    screen.draw();
    screen.search("alpha");
    assert_eq!(screen.status(), find_status(12, 12), "{}", screen.text());
    screen.press(KeyCode::Up);
    screen.press(KeyCode::Up);
    assert_eq!(screen.status(), find_status(10, 12));
    assert_eq!(screen.current().1.trim(), "alpha 10");
    let rows = screen.conversation();
    let at = screen.reversed()[0].clone();

    for n in 13..=30 {
        screen.event(
            json!({ "type": "item.delta", "turnId": "t1", "itemId": "m1",
            "delta": { "type": "agentMessage", "delta": format!("alpha {n}\n") } }),
        );
        screen.draw();
    }
    assert_eq!(screen.status(), find_status(10, 30), "{}", screen.text());
    assert_eq!(screen.reversed(), [at], "{}", screen.text());
    // Not pulled down: the rows stay, the last one now says there is more.
    let now = screen.conversation();
    assert_eq!(now[..rows.len() - 1], rows[..rows.len() - 1]);
    assert!(now.last().unwrap().contains("New output below"), "{now:?}");
    screen.press(KeyCode::Down);
    assert_eq!(screen.current().1.trim(), "alpha 11");
}

/// Wide characters and emoji made of several characters: the highlight is
/// on their cells.
#[test]
fn wide_characters_and_emoji_are_highlighted_in_their_cells() {
    let mut screen = Screen::new();
    screen.start_turn("t1", "看看");
    screen.say("t1", "m1", "测试中文搜索：中文和 👩\u{200d}💻 都要找到。");
    screen.draw();

    screen.search("中文");
    assert_eq!(screen.status(), find_status(2, 2), "{}", screen.text());
    let line = screen.find("测试中文搜索");
    let second = line.0 + "测试中文搜索：".width() as u16;
    assert_eq!(screen.reversed(), [(line.1, second, "中文".to_owned())]);
    let first = line.0 + "测试".width() as u16;
    let underlined = screen.underlined();
    assert!(
        underlined.contains(&(line.1, first, "中文".to_owned())),
        "{underlined:?}"
    );
    screen.press(KeyCode::Esc);

    screen.search("👩\u{200d}💻");
    assert_eq!(screen.status(), find_status(1, 1), "{}", screen.text());
    let emoji = line.0 + "测试中文搜索：中文和 ".width() as u16;
    assert_eq!(
        screen.reversed(),
        [(line.1, emoji, "👩\u{200d}💻".to_owned())]
    );
}
