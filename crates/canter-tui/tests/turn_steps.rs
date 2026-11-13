//! How the steps of a turn look, laid out the Codex way: `●` before the
//! answer, `✓`, `✗` or `•` before every tool card, thinking as
//! `✻ Thought for 2s · <title>` or `∴ <the thought>`, reads in a row merged into `✓ Explored`, commands as `Run` / `Running` / `Ran` with
//! their output after `  └ `, headings and bold in colours of their own,
//! paths in card titles coloured instead of underlined, and the turn's end as
//! `Worked for 31s · 11:13`.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{self, Background, Depth, Theme};
use canter_tui::{App, Ui};
use chrono::TimeZone;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const SESSION: &str = "s1";
/// A fixed moment, in Unix ms.
const T0: i64 = 1_800_000_000_000;
const WIDTH: u16 = 80;
const HEIGHT: u16 = 40;
/// Rows below the conversation: status line, input box (3) and info rows (2).
const BELOW: usize = 6;

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
    now: i64,
}

impl Screen {
    fn new(width: u16) -> Self {
        Self::themed(Theme::default(), width)
    }

    fn themed(theme: Theme, width: u16) -> Self {
        theme::set(theme);
        let app = App::new(SessionSnapshot::empty(SESSION), None, T0);
        let ui = Ui::new(TestBackend::new(width, HEIGHT), app).unwrap();
        let mut screen = Self {
            ui,
            seq: 0,
            now: T0,
        };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen.event(json!({ "type": "turn.started", "turn": turn("inProgress") }));
        screen.event(json!({ "type": "session.status", "status": "working" }));
        screen
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

    fn started(&mut self, item: Value) {
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": item }));
    }

    fn completed(&mut self, item: Value) {
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": item }));
    }

    fn say(&mut self, id: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": id, "text": text });
        self.started(message.clone());
        self.completed(message);
    }

    fn read(&mut self, id: &str, path: &str, status: &str) {
        let item = json!({
            "type": "fileRead", "id": id, "path": path, "startLine": 1,
            "content": "one\ntwo\n", "status": status,
        });
        if status == "inProgress" {
            self.started(item);
        } else {
            self.completed(item);
        }
    }

    fn finish_turn(&mut self) {
        self.event(json!({ "type": "turn.completed", "turn": turn("completed") }));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    fn ctrl_o(&mut self) {
        self.ui.app_mut().handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            self.now,
        );
    }

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(self.now).unwrap();
        self.ui.backend().buffer()
    }

    /// The conversation's rows, above the status line, without the prompt,
    /// the blank rows and the running command's own row.
    fn rows(&mut self) -> Vec<String> {
        let buffer = self.draw();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect();
        rows[..rows.len() - BELOW]
            .iter()
            .filter(|row| {
                !row.is_empty() && !row.starts_with('›') && !row.starts_with("   Running ")
            })
            .cloned()
            .collect()
    }
}

fn turn(status: &str) -> Value {
    json!({
        "id": "t1", "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "do it" }],
        "clientName": "canter-tui", "usage": null,
    })
}

fn command(id: &str, command: &str, output: &str, status: &str, exit: Value, ms: Value) -> Value {
    json!({
        "type": "commandExecution", "id": id, "command": command, "cwd": "/work",
        "status": status, "aggregatedOutput": output, "exitCode": exit, "durationMs": ms,
    })
}

fn numbered(count: usize) -> String {
    (1..=count).map(|n| format!("out {n}\n")).collect()
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
fn cells<'a>(buffer: &'a Buffer, text: &str) -> Vec<&'a Cell> {
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

fn cell<'a>(buffer: &'a Buffer, text: &str) -> &'a Cell {
    cells(buffer, text)[0]
}

/// The cells of `cells` that are not blank.
fn letters<'a>(cells: &[&'a Cell]) -> Vec<&'a Cell> {
    cells
        .iter()
        .copied()
        .filter(|cell| !cell.symbol().trim().is_empty())
        .collect()
}

fn clock(ms: i64) -> String {
    chrono::Local
        .timestamp_opt(ms / 1000, 0)
        .unwrap()
        .format("%H:%M")
        .to_string()
}

// §1

/// The answer starts with a bold `● `, its later rows indent 2; tool cards
/// start with `✓` once they worked, `✗` once they failed, `•` while they
/// run; a command's mark and verb in the colour of how it went.
/// The prompt keeps `›`.
#[test]
fn the_answer_and_tool_cards_start_with_a_bullet() {
    let mut screen = Screen::new(WIDTH);
    screen.say("m1", "First row.\nSecond row.");
    screen.completed(command(
        "ok",
        "cargo build",
        "",
        "completed",
        json!(0),
        json!(10),
    ));
    screen.completed(command(
        "bad",
        "cargo test",
        "",
        "failed",
        json!(1),
        json!(10),
    ));
    screen.started(command(
        "live",
        "sleep 9",
        "",
        "inProgress",
        Value::Null,
        Value::Null,
    ));
    assert_eq!(
        screen.rows(),
        [
            "● First row.",
            "  Second row.",
            "✓ Ran cargo build",
            "✗ Ran cargo test · exit 1",
            "• Running sleep 9",
            "      Ctrl+B to run in background",
        ]
    );
    let buffer = screen.draw();
    assert_eq!(cell(buffer, "✓ Ran cargo build").fg, Color::Green);
    assert_eq!(cell(buffer, "✗ Ran cargo test").fg, Color::Red);
    for cell in letters(&cells(buffer, "✓ Ran")) {
        assert_eq!(
            cell.fg,
            Color::Green,
            "a command that worked: mark and verb green"
        );
    }
    assert!(
        cell(buffer, "Ran cargo build")
            .modifier
            .contains(Modifier::BOLD)
    );
    let failed = cells(buffer, "✗ Ran cargo test");
    for cell in letters(&failed[..5]) {
        assert_eq!(
            cell.fg,
            Color::Red,
            "a command that failed: mark and verb red"
        );
    }
    let said = cell(buffer, "● First row.");
    assert!(
        said.modifier.contains(Modifier::BOLD),
        "the answer's `●` is bold"
    );
    assert_eq!(said.fg, Color::Reset, "in the default colour");
    let live = cell(buffer, "• Running");
    assert!(live.modifier.contains(Modifier::DIM));
    assert_ne!(live.fg, Color::Green);
    assert_ne!(live.fg, Color::Red);
    assert_eq!(cell(buffer, "› do it").symbol(), "›");
}

// §2

/// While thinking: `✻ Thinking…` and the latest line; then folded to
/// `✻ Thought for 2s · ` and the start of the thought, the label in the
/// secondary grey while it runs and purple once done, the text fainter and
/// italic, from column 0.
#[test]
fn thinking_shows_its_latest_line_then_folds_with_its_time() {
    let mut screen = Screen::new(WIDTH);
    screen.started(json!({ "type": "reasoning", "id": "r1", "summary": [], "content": [] }));
    for delta in ["Look at the tests.", "\nThen the code."] {
        screen.event(json!({
            "type": "item.delta", "turnId": "t1", "itemId": "r1",
            "delta": { "type": "reasoningContent", "contentIndex": 0, "delta": delta },
        }));
        screen.now += 2000;
    }
    assert_eq!(screen.rows(), ["✻ Thinking… Then the code."]);
    let buffer = screen.draw();
    let label = cell(buffer, "✻ Thinking");
    assert!(label.modifier.contains(Modifier::DIM));
    assert!(!label.modifier.contains(Modifier::ITALIC));
    let text = cell(buffer, "Then the code.");
    assert!(text.modifier.contains(Modifier::ITALIC));
    assert_eq!(text.fg, Color::DarkGray);

    screen.completed(json!({
        "type": "reasoning", "id": "r1", "summary": [],
        "content": ["Look at the tests.\nThen the code."],
    }));
    // A raw thought has no title to show: folded, it is the thought itself
    // after `∴`, grey and upright, with no label.
    assert_eq!(screen.rows(), ["∴ Look at the tests.", "  Then the code."]);
    let buffer = screen.draw();
    for text in ["∴", "Look at the tests.", "Then the code."] {
        let cell = cell(buffer, text);
        assert!(cell.modifier.contains(Modifier::DIM), "{text}");
        assert!(!cell.modifier.contains(Modifier::ITALIC), "{text}");
    }

    // Ctrl+O: the heading keeps `✻`, the whole thought is under it.
    screen.ctrl_o();
    let rows = screen.rows();
    assert_eq!(rows[0], "✻ Thought for 2s", "{rows:?}");
    assert!(rows.iter().any(|row| row.ends_with("Then the code.")));
}

#[test]
fn folded_thinking_hides_the_bold_markers_of_a_gpt_summary_title() {
    // GPT summaries arrive as reasoning content whose first line is a bold title.
    let mut screen = Screen::new(WIDTH);
    screen.started(json!({ "type": "reasoning", "id": "r1", "summary": [], "content": [] }));
    screen.now += 2000;
    screen.event(json!({
        "type": "item.delta", "turnId": "t1", "itemId": "r1",
        "delta": {
            "type": "reasoningContent", "contentIndex": 0,
            "delta": "**Reviewing concurrency risks**",
        },
    }));
    assert_eq!(screen.rows(), ["✻ Thinking… Reviewing concurrency risks"]);
    screen.completed(json!({
        "type": "reasoning", "id": "r1", "summary": [],
        "content": ["**Reviewing concurrency risks**\n\nTwo savers can race."],
    }));
    assert_eq!(
        screen.rows(),
        ["✻ Thought for 2s · Reviewing concurrency risks…"]
    );
}

#[test]
fn thinking_without_summary_keeps_one_line_when_folded_or_expanded() {
    let mut screen = Screen::new(WIDTH);
    let item = json!({ "type": "reasoning", "id": "r1", "summary": [], "content": [] });
    screen.started(item.clone());
    assert_eq!(screen.rows(), ["✻ Thinking…"]);
    screen.now += 3000;
    screen.event(json!({
        "type": "item.delta", "turnId": "t1", "itemId": "r1",
        "delta": { "type": "reasoningContent", "contentIndex": 0, "delta": "" },
    }));
    screen.completed(item);
    for _ in 0..2 {
        assert_eq!(screen.rows(), ["✻ Thought for 3s · no summary"]);
        let buffer = screen.draw();
        assert_eq!(cell(buffer, "✻ Thought for 3s").fg, Color::Magenta);
        let text = cell(buffer, "no summary");
        assert_eq!(text.fg, Color::DarkGray);
        assert!(text.modifier.contains(Modifier::DIM | Modifier::ITALIC));
        screen.ctrl_o();
    }
}

/// A raw thought wider than the screen wraps under its first word instead
/// of being cut; markdown shows as it does expanded, without its backticks.
#[test]
fn a_folded_raw_thought_wraps_instead_of_being_cut() {
    let mut screen = Screen::new(40);
    screen.completed(json!({
        "type": "reasoning", "id": "r1", "summary": [],
        "content": ["A long thought about `main_pane` that runs well past the edge."],
    }));
    let rows = screen.rows();
    assert_eq!(
        rows,
        [
            "∴ A long thought about main_pane that",
            "  runs well past the edge."
        ],
        "{rows:?}"
    );
    assert!(rows.iter().all(|row| row.width() <= 40));
}

/// A raw thought longer than three rows shows the first three, blank lines
/// not counted, then how many more rows Ctrl+O shows, as other folded cards
/// say it.
#[test]
fn a_long_folded_raw_thought_shows_three_rows_and_how_many_more() {
    let mut screen = Screen::new(WIDTH);
    screen.completed(json!({
        "type": "reasoning", "id": "r1", "summary": [],
        "content": ["Both exist.\n\nNow, implement.\n\nFiles:\n\n1. statusline.rs\n2. app.rs\n3. chrome.rs"],
    }));
    assert_eq!(
        screen.rows(),
        [
            "∴ Both exist.",
            "  Now, implement.",
            "  Files:",
            "   ⋯ 3 more lines · Ctrl+O to expand",
        ]
    );
    screen.ctrl_o();
    let rows = screen.rows();
    assert_eq!(rows[0], "✻ Thought", "{rows:?}");
    assert!(
        rows.iter().any(|row| row.ends_with("chrome.rs")),
        "{rows:?}"
    );
}

// §3

/// Reads in a row are one block, growing as each read ends: `✓ Explored`,
/// then `  └ Read ` and the file names. The mark and `Explored` are cyan,
/// `Read` is blue, the names in the body colour without an underline, the
/// commas dim.
#[test]
fn reads_in_a_row_are_one_explored_block_that_grows() {
    let mut screen = Screen::new(WIDTH);
    screen.read("r1", "/work/src/app.rs", "inProgress");
    assert_eq!(screen.rows(), Vec::<String>::new());
    screen.read("r1", "/work/src/app.rs", "completed");
    assert_eq!(screen.rows(), ["✓ Explored", "  └ Read app.rs"]);
    screen.read("r2", "/work/README.md", "completed");
    screen.read("r3", "/work/src/cells.rs", "completed");
    assert_eq!(
        screen.rows(),
        ["✓ Explored", "  └ Read app.rs, README.md, cells.rs"]
    );
    let buffer = screen.draw();
    assert!(cell(buffer, "Explored").modifier.contains(Modifier::BOLD));
    // `✓` says it worked, green on every card; the title says what kind.
    assert_eq!(cell(buffer, "✓ Explored").fg, Color::Green);
    for cell in letters(&cells(buffer, "Explored")) {
        assert_eq!(cell.fg, Color::Cyan, "the title is cyan");
    }
    assert_eq!(cell(buffer, "Read app.rs").fg, Color::Blue);
    for name in ["app.rs", "README.md", "cells.rs"] {
        for cell in cells(buffer, name) {
            assert_eq!(cell.fg, Color::Reset, "{name}");
            assert!(!cell.modifier.contains(Modifier::UNDERLINED), "{name}");
        }
    }
    assert!(cell(buffer, ", README").modifier.contains(Modifier::DIM));
}

/// Something else between reads ends the block; a failed read is a row of
/// its own; two files of the same name show enough of their paths.
#[test]
fn reads_split_by_other_steps_failures_on_their_own_and_same_names_told_apart() {
    let mut screen = Screen::new(WIDTH);
    screen.read("r1", "/work/src/a/mod.rs", "completed");
    screen.read("r2", "/work/src/b/mod.rs", "completed");
    screen.completed(json!({
        "type": "reasoning", "id": "t", "summary": [], "content": ["Now the lib."],
    }));
    screen.read("r5", "/work/src/lib.rs", "completed");
    screen.completed(command(
        "c1",
        "cat notes.txt",
        "",
        "completed",
        json!(0),
        json!(10),
    ));
    screen.read("r3", "/work/missing.rs", "failed");
    screen.read("r4", "/work/notes.txt", "completed");
    assert_eq!(
        screen.rows(),
        [
            "✓ Explored",
            "  └ Read a/mod.rs, b/mod.rs",
            "∴ Now the lib.",
            "✓ Explored",
            "  └ Read lib.rs",
            "✓ Ran cat notes.txt",
            "✗ Read missing.rs · failed",
            "✓ Explored",
            "  └ Read notes.txt",
        ]
    );
    let buffer = screen.draw();
    assert_eq!(cell(buffer, "✗ Read missing.rs").fg, Color::Red);
}

/// A long list wraps under the first name; Ctrl+O shows a row per file
/// with its whole path and the lines read.
#[test]
fn a_long_list_wraps_under_the_names_and_ctrl_o_shows_every_file() {
    let mut screen = Screen::new(40);
    for (n, name) in ["alpha", "bravo", "charlie", "delta", "echo"]
        .iter()
        .enumerate()
    {
        screen.read(
            &format!("r{n}"),
            &format!("/work/src/{name}.rs"),
            "completed",
        );
    }
    assert_eq!(
        screen.rows(),
        [
            "✓ Explored",
            "  └ Read alpha.rs, bravo.rs, charlie.rs,",
            "         delta.rs, echo.rs",
        ]
    );
    screen.ctrl_o();
    assert_eq!(
        screen.rows(),
        [
            "✓ Explored",
            "  └ Read src/alpha.rs  lines 1–2",
            "    Read src/bravo.rs  lines 1–2",
            "    Read src/charlie.rs  lines 1–2",
            "    Read src/delta.rs  lines 1–2",
            "    Read src/echo.rs  lines 1–2",
        ]
    );
}

// §4

/// `Run` while waiting for approval, `Running` while it runs (its latest
/// lines), `Ran` once done (its first lines and `+ N lines`).
#[test]
fn a_command_is_run_then_running_then_ran() {
    let mut screen = Screen::new(WIDTH);
    let waiting = command(
        "c1",
        "cargo test",
        "",
        "inProgress",
        Value::Null,
        Value::Null,
    );
    screen.started(waiting);
    screen.event(json!({ "type": "request.opened", "request": {
        "id": "q1", "turnId": "t1", "createdAt": 0,
        "kind": {
            "type": "approval", "itemId": "c1", "tool": "shell",
            "arguments": { "command": "cargo test" },
            "availableDecisions": ["accept", "decline"],
        },
    }}));
    screen.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    assert_eq!(screen.rows()[0], "• Run cargo test");
    screen.event(json!({ "type": "request.resolved", "requestId": "q1",
        "resolution": { "type": "answered", "decision": "accept" }, "clientName": "canter-tui" }));
    screen.event(json!({ "type": "session.status", "status": "working" }));
    screen.event(json!({
        "type": "item.delta", "turnId": "t1", "itemId": "c1",
        "delta": { "type": "commandExecution", "delta": numbered(7) },
    }));
    let rows = screen.rows();
    assert_eq!(rows[0], "• Running cargo test", "{rows:?}");
    assert_eq!(
        rows[1], "  └  ⋯ 2 lines above · Ctrl+O to expand",
        "{rows:?}"
    );
    assert_eq!(
        rows[2..],
        [
            "    out 3",
            "    out 4",
            "    out 5",
            "    out 6",
            "    out 7",
            "      Ctrl+B to run in background",
        ]
    );

    screen.completed(command(
        "c1",
        "cargo test",
        &numbered(7),
        "completed",
        json!(0),
        json!(12_300),
    ));
    assert_eq!(
        screen.rows(),
        [
            "✓ Ran cargo test · 12.3s",
            "  └ out 1",
            "    out 2",
            "    out 3",
            "     ⋯ 4 more lines · Ctrl+O to expand",
        ]
    );
    let buffer = screen.draw();
    assert!(cell(buffer, "12.3s").modifier.contains(Modifier::DIM));
    screen.ctrl_o();
    let rows = screen.rows();
    assert_eq!(rows.len(), 8, "{rows:?}");
    assert_eq!(rows[7], "    out 7");
}

/// Failed: the last lines after ` ⋯ N lines above `, `exit N` red and bold.
/// One line more is ` ⋯ 1 more line `; no output, no `└`; under 1s no time.
#[test]
fn failures_keep_their_last_lines_and_the_status_follows_the_rules() {
    let mut screen = Screen::new(WIDTH);
    screen.completed(command(
        "c1",
        "make",
        &numbered(12),
        "failed",
        json!(2),
        json!(800),
    ));
    screen.completed(command(
        "c2",
        "ls",
        &numbered(4),
        "completed",
        json!(0),
        json!(800),
    ));
    screen.completed(command("c3", "true", "", "completed", json!(0), json!(800)));
    let mut expected = vec![
        "✗ Ran make · exit 2".to_owned(),
        "  └  ⋯ 2 lines above · Ctrl+O to expand".to_owned(),
    ];
    expected.extend((3..=12).map(|n| format!("    out {n}")));
    expected.extend(
        [
            "✓ Ran ls",
            "  └ out 1",
            "    out 2",
            "    out 3",
            "     ⋯ 1 more line · Ctrl+O to expand",
            "✓ Ran true",
        ]
        .map(str::to_owned),
    );
    assert_eq!(screen.rows(), expected);
    let buffer = screen.draw();
    assert_eq!(cell(buffer, "exit 2").fg, Color::Red);
    assert!(cell(buffer, "exit 2").modifier.contains(Modifier::BOLD));
    assert!(cell(buffer, "└").modifier.contains(Modifier::DIM));
}

/// A command too long for one row wraps under itself and its status goes
/// on the next row; other cards' status follows their title.
#[test]
fn a_wrapped_command_puts_its_status_on_the_next_row() {
    let mut screen = Screen::new(40);
    screen.completed(command(
        "c1",
        "cargo test --workspace --all-targets --no-fail-fast",
        "",
        "failed",
        json!(101),
        json!(1500),
    ));
    screen.completed(json!({
        "type": "webFetch", "id": "f1", "url": "https://example.com/a",
        "result": "", "status": "failed",
    }));
    assert_eq!(
        screen.rows(),
        [
            "✗ Ran cargo test --workspace",
            "      --all-targets --no-fail-fast",
            "      exit 101 · 1.5s",
            "✗ Fetch https://example.com/a · failed",
        ]
    );
}

// §5

/// `##` headings have a colour of their own, apart from inline code, body
/// text and bold; bold is red, and only bold without colours.
#[test]
fn headings_and_bold_have_their_own_colours() {
    for background in [Background::Dark, Background::Light] {
        let mut screen = Screen::themed(
            Theme {
                depth: Depth::Ansi16,
                background,
            },
            WIDTH,
        );
        screen.say("m1", "## Plan\n\nRun `cargo test` with **care** now.");
        let buffer = screen.draw();
        let heading = cell(buffer, "Plan");
        let code = cell(buffer, "cargo test");
        let bold = cell(buffer, "care");
        let body = cell(buffer, "now.");
        assert!(heading.modifier.contains(Modifier::BOLD));
        assert_ne!(heading.fg, Color::Reset);
        for other in [code.fg, bold.fg, body.fg] {
            assert_ne!(heading.fg, other, "{background:?}");
        }
        assert_eq!(bold.fg, Color::Red);
        assert!(bold.modifier.contains(Modifier::BOLD));
    }
    let mut screen = Screen::themed(
        Theme {
            depth: Depth::None,
            background: Background::Dark,
        },
        WIDTH,
    );
    screen.say("m1", "Use **care** now.");
    let buffer = screen.draw();
    let bold = cell(buffer, "care");
    assert_eq!(bold.fg, Color::Reset);
    assert!(bold.modifier.contains(Modifier::BOLD));
}

// §6

/// A path in a card title is coloured, not underlined; a web address in the
/// answer stays underlined; without colours the path is underlined.
#[test]
fn paths_in_titles_are_coloured_and_addresses_underlined() {
    let edit = json!({
        "type": "fileChange", "id": "e1", "status": "completed",
        "changes": [{ "path": "/work/src/lib.rs", "kind": { "type": "add" }, "diff": "x\n" }],
    });
    let mut screen = Screen::new(WIDTH);
    screen.completed(edit.clone());
    screen.say("m1", "See https://example.com/docs for more.");
    let buffer = screen.draw();
    for cell in cells(buffer, "src/lib.rs") {
        assert_eq!(cell.fg, Color::Blue);
        assert!(!cell.modifier.contains(Modifier::UNDERLINED));
    }
    for cell in cells(buffer, "https://example.com/docs") {
        assert!(cell.modifier.contains(Modifier::UNDERLINED));
    }

    let mut screen = Screen::themed(
        Theme {
            depth: Depth::None,
            background: Background::Dark,
        },
        WIDTH,
    );
    screen.completed(edit);
    let buffer = screen.draw();
    for cell in cells(buffer, "src/lib.rs") {
        assert_eq!(cell.fg, Color::Reset);
        assert!(cell.modifier.contains(Modifier::UNDERLINED));
    }
}

/// An edit that worked has its mark and verb orange, as the working verb,
/// for the background; orange 166 of the 256 with the background unknown;
/// yellow with 16 colours (P2-195).
#[test]
fn an_edit_is_amber_after_a_green_check() {
    for (depth, background, amber) in [
        (
            Depth::TrueColor,
            Background::Dark,
            Color::Rgb(0xe0, 0xa0, 0x40),
        ),
        (
            Depth::TrueColor,
            Background::Light,
            Color::Rgb(0xb4, 0x5f, 0x06),
        ),
        (Depth::Ansi256, Background::Unknown, Color::Indexed(172)),
        (Depth::Ansi16, Background::Dark, Color::Yellow),
    ] {
        let mut screen = Screen::themed(Theme { depth, background }, WIDTH);
        screen.completed(edit("completed"));
        let buffer = screen.draw();
        // An orange `✓` read as an error (user, 11-13): it is green.
        assert_eq!(cell(buffer, "✓ Edit").fg, Color::Green);
        for cell in letters(&cells(buffer, "Edit")) {
            assert_eq!(cell.fg, amber, "{depth:?} {background:?}");
        }
        assert!(cell(buffer, "Edit").modifier.contains(Modifier::BOLD));
    }
}

/// A count of nothing (`−0`, `+0`) is grey, not red or green.
#[test]
fn a_zero_count_on_an_edit_is_grey() {
    let mut screen = Screen::new(WIDTH);
    screen.completed(json!({
        "type": "fileChange", "id": "e1", "status": "completed",
        "changes": [{
            "path": "/work/src/lib.rs", "kind": { "type": "update", "movePath": null },
            "diff": "@@ -1 +1,2 @@\n old\n+new\n",
        }],
    }));
    let buffer = screen.draw();
    assert_eq!(cell(buffer, "+1").fg, Color::Green);
    let zero = cell(buffer, "−0");
    assert_ne!(zero.fg, Color::Red);
    assert!(zero.modifier.contains(Modifier::DIM));
}

/// Without colours the cards' titles are only bold, finished thinking's
/// label grey, and the answer still starts with `●` (P2-195).
#[test]
fn without_colours_titles_are_only_bold() {
    let mut screen = Screen::themed(
        Theme {
            depth: Depth::None,
            background: Background::Dark,
        },
        WIDTH,
    );
    screen.completed(json!({
        "type": "reasoning", "id": "r1", "summary": [], "content": ["Look first."],
    }));
    screen.read("f1", "/work/src/app.rs", "completed");
    screen.completed(edit("completed"));
    screen.completed(command(
        "ok",
        "cargo build",
        "",
        "completed",
        json!(0),
        json!(10),
    ));
    screen.completed(command(
        "bad",
        "cargo test",
        "",
        "failed",
        json!(1),
        json!(10),
    ));
    screen.say("m1", "Done.");
    let rows = screen.rows();
    assert!(rows.contains(&"● Done.".to_owned()), "{rows:#?}");
    let buffer = screen.draw();
    for title in ["Explored", "Edit", "Ran cargo build", "Ran cargo test"] {
        let cell = cell(buffer, title);
        assert_eq!(cell.fg, Color::Reset, "{title}");
        assert!(cell.modifier.contains(Modifier::BOLD), "{title}");
    }
    for mark in [
        "✓ Explored",
        "✓ Edit",
        "✓ Ran cargo build",
        "✗ Ran cargo test",
    ] {
        assert_eq!(cell(buffer, mark).fg, Color::Reset, "{mark}");
    }
    let label = cell(buffer, "∴ Look first.");
    assert_eq!(label.fg, Color::Reset);
    assert!(label.modifier.contains(Modifier::DIM));
    assert!(cell(buffer, "● Done.").modifier.contains(Modifier::BOLD));
}

/// An edit of `src/lib.rs`.
fn edit(status: &str) -> Value {
    json!({
        "type": "fileChange", "id": "e1", "status": status,
        "changes": [{
            "path": "/work/src/lib.rs", "kind": { "type": "update", "movePath": null },
            "diff": "@@ -1 +1 @@\n-old\n+new\n",
        }],
    })
}

// §7

/// The turn ends with `Worked for 31s · HH:MM`, dim, indented 2, with no
/// line; a reopened session shows the time the turn ended then.
#[test]
fn the_turn_ends_with_its_time_and_the_clock() {
    let mut screen = Screen::new(WIDTH);
    screen.say("m1", "Done.");
    screen.now += 31_000;
    screen.finish_turn();
    let rows = screen.rows();
    let footer = format!("  Worked for 31s · {}", clock(T0 + 31_000));
    assert_eq!(rows.last(), Some(&footer), "{rows:?}");
    assert!(!rows.iter().any(|row| row.contains('─')), "{rows:?}");
    let buffer = screen.draw();
    assert!(cell(buffer, "Worked for").modifier.contains(Modifier::DIM));

    // Reopened a day later: the clock is still the one the turn ended at.
    let ended = T0 / 1000 + 31;
    let mut snapshot = SessionSnapshot::empty(SESSION);
    snapshot.turns.push(
        serde_json::from_value(json!({
            "id": "t1", "status": "completed", "error": null, "usage": null,
            "input": [{ "type": "text", "text": "do it" }], "clientName": "canter-tui",
            "items": [{ "type": "agentMessage", "id": "m1", "text": "Done." }],
            "startedAt": ended - 31, "completedAt": ended, "durationMs": 31_000,
        }))
        .unwrap(),
    );
    let later = T0 + 86_400_000 + 3_600_000;
    let app = App::new(snapshot, None, later);
    let mut ui = Ui::new(TestBackend::new(WIDTH, HEIGHT), app).unwrap();
    ui.draw(later).unwrap();
    let buffer = ui.backend().buffer();
    let shown: Vec<String> = (0..buffer.area.height)
        .map(|y| row(buffer, y).trim_end().to_owned())
        .collect();
    assert!(
        shown.contains(&footer),
        "{footer:?} is not on screen:\n{}",
        shown.join("\n")
    );
}
