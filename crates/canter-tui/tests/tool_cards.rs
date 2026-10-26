//! Tool cards: `● Verb object` with the status on the right, output after
//! `    │ `. MCP calls keep the first lines of their result, web fetches show
//! only notes, failed commands keep more lines in the default colour, and
//! Ctrl+O shows at most 500 lines of one item's output.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::{Color, Modifier};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
const WIDTH: u16 = 80;
/// Rows below the conversation: status line, input box (3) and info rows (2).
const BELOW: usize = 6;

struct Screen {
    ui: Ui<TestBackend>,
    seq: u64,
}

impl Screen {
    fn new(height: u16) -> Self {
        let app = App::new(SessionSnapshot::empty(SESSION), None, NOW);
        let ui = Ui::new(TestBackend::new(WIDTH, height), app).unwrap();
        let mut screen = Self { ui, seq: 0 };
        screen.event(json!({
            "type": "session.updated",
            "settings": {
                "cwd": "/work", "name": null, "model": "test-model",
                "approvalPolicy": "alwaysAsk",
            },
        }));
        screen.event(json!({ "type": "turn.started", "turn": {
            "id": "t1", "items": [], "status": "inProgress", "error": null,
            "input": [{ "type": "text", "text": "do it" }],
            "clientName": "canter-tui", "usage": null,
        }}));
        screen.event(json!({ "type": "session.status", "status": "working" }));
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

    fn completed(&mut self, item: Value) {
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": item }));
    }

    fn command(&mut self, id: &str, command: &str, output: &str, exit_code: i32) {
        self.completed(json!({
            "type": "commandExecution", "id": id, "command": command, "cwd": "/work",
            "status": if exit_code == 0 { "completed" } else { "failed" },
            "aggregatedOutput": output, "exitCode": exit_code, "durationMs": 1200,
        }));
    }

    fn mcp(&mut self, id: &str, arguments: Value, result: &str, status: &str) {
        self.completed(json!({
            "type": "mcpToolCall", "id": id, "server": "github", "tool": "search_issues",
            "arguments": arguments, "result": result, "status": status,
        }));
    }

    fn fetch(&mut self, id: &str, url: &str, result: &str, status: &str) {
        self.completed(json!({
            "type": "webFetch", "id": id, "url": url, "result": result, "status": status,
        }));
    }

    fn ctrl_o(&mut self) -> Option<Action> {
        self.ui.app_mut().handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            NOW,
        )
    }

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(NOW).unwrap();
        self.ui.backend().buffer()
    }

    /// The conversation's rows, above the status line.
    fn conversation(&mut self) -> Vec<String> {
        let buffer = self.draw();
        let rows: Vec<String> = (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect();
        rows[..rows.len() - BELOW].to_vec()
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
        .filter(|cell| !cell.symbol().trim().is_empty())
        .collect()
}

fn numbered(count: usize, word: &str) -> String {
    (1..=count).map(|n| format!("{word} {n}\n")).collect()
}

/// The rows from the one starting with `title` to the next blank row.
fn card(conversation: &[String], title: &str) -> Vec<String> {
    let at = conversation
        .iter()
        .position(|row| row.starts_with(title))
        .unwrap_or_else(|| panic!("{title:?} in {conversation:#?}"));
    let mut rows = vec![conversation[at].clone()];
    rows.extend(
        conversation[at + 1..]
            .iter()
            .take_while(|row| row.starts_with("    │"))
            .cloned(),
    );
    rows
}

/// `● Call server/tool` and the arguments on one row, the first 3 lines of
/// the result, then `… N more lines · Ctrl+O`; Ctrl+O shows all of it.
#[test]
fn mcp_card_shows_arguments_and_the_first_lines_of_its_result() {
    let mut screen = Screen::new(40);
    screen.mcp(
        "m1",
        json!({ "query": "diff render", "limit": 3 }),
        &numbered(7, "issue"),
        "completed",
    );
    let long = "x".repeat(200);
    screen.mcp("m2", json!({ "query": long }), "one\n", "completed");
    let conversation = screen.conversation();
    let rows = card(&conversation, "  ● Call github/search_issues  {\"limit\"");
    assert_eq!(
        rows,
        [
            "  ● Call github/search_issues  {\"limit\":3,\"query\":\"diff render\"}",
            "    │ issue 1",
            "    │ issue 2",
            "    │ issue 3",
            "    │ … 4 more lines · Ctrl+O",
        ]
    );
    let long_row = conversation
        .iter()
        .find(|row| row.starts_with("  ● Call github/search_issues  {\"query\":\"xxx"))
        .unwrap_or_else(|| panic!("{conversation:#?}"));
    assert_eq!(long_row.width(), usize::from(WIDTH), "cut to fit");

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let rows = card(&conversation, "  ● Call github/search_issues  {\"limit\"");
    let expected: Vec<String> = (1..=7).map(|n| format!("    │ issue {n}")).collect();
    assert_eq!(rows[1..], expected[..]);
}

/// A web fetch that brought a page back is one row: `● Fetch <url>` and the
/// size on the right. The page itself shows only with Ctrl+O.
#[test]
fn web_fetch_shows_only_its_title_and_size() {
    let mut screen = Screen::new(40);
    let page = format!("# Docs\n{}", "a".repeat(18_841 - 7));
    screen.fetch("w1", "https://docs.example/page", &page, "completed");
    screen.fetch(
        "w2",
        "https://docs.example/small",
        "hello world\n",
        "completed",
    );
    let conversation = screen.conversation();
    let rows = card(&conversation, "  ● Fetch https://docs.example/page");
    assert_eq!(rows.len(), 1, "{conversation:#?}");
    assert!(rows[0].ends_with("  18.4 KB"), "{rows:?}");
    assert_eq!(rows[0].width(), usize::from(WIDTH));
    let small = card(&conversation, "  ● Fetch https://docs.example/small");
    assert_eq!(small.len(), 1);
    assert!(small[0].ends_with("  12 B"), "{small:?}");
    assert!(!conversation.iter().any(|row| row.contains("Docs")));

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let rows = card(&conversation, "  ● Fetch https://docs.example/page");
    assert_eq!(rows[1], "    │ # Docs");
    let small = card(&conversation, "  ● Fetch https://docs.example/small");
    assert_eq!(small[1..], ["    │ hello world"]);
}

/// A redirect to another host says `redirect` on the right and shows the
/// line; content left out (images, PDFs, too big to download) shows its line.
#[test]
fn web_fetch_shows_redirects_and_omitted_content_as_their_line() {
    let mut screen = Screen::new(40);
    screen.fetch(
        "w1",
        "https://example.com/old",
        "Redirected to https://example.org/new",
        "completed",
    );
    screen.fetch(
        "w2",
        "https://example.com/a.png",
        "[Content omitted: image/png, 20480 bytes]",
        "completed",
    );
    screen.fetch(
        "w3",
        "https://example.com/big",
        "Download exceeds 10485760 bytes; content omitted.",
        "completed",
    );
    let conversation = screen.conversation();
    let redirect = card(&conversation, "  ● Fetch https://example.com/old");
    assert!(redirect[0].ends_with("  redirect"), "{redirect:?}");
    assert_eq!(
        redirect[1..],
        ["    │ Redirected to https://example.org/new"]
    );
    assert_eq!(
        card(&conversation, "  ● Fetch https://example.com/a.png"),
        [
            "  ● Fetch https://example.com/a.png",
            "    │ [Content omitted: image/png, 20480 bytes]",
        ]
    );
    assert_eq!(
        card(&conversation, "  ● Fetch https://example.com/big"),
        [
            "  ● Fetch https://example.com/big",
            "    │ Download exceeds 10485760 bytes; content omitted.",
        ]
    );
}

/// A failed fetch: `✗`, a red `failed` on the right, and the reason below in red.
#[test]
fn failed_web_fetch_shows_its_reason_in_red() {
    let mut screen = Screen::new(30);
    let reason = "web_fetch request failed (connection or TLS error)";
    screen.fetch("w1", "http://localhost:9/", reason, "failed");
    let conversation = screen.conversation();
    let rows = card(&conversation, "  ✗ Fetch http://localhost:9/");
    assert!(rows[0].ends_with("  failed"), "{rows:?}");
    assert_eq!(rows[1..], [format!("    │ {reason}")]);
    let buffer = screen.draw();
    for text in ["✗", "failed", reason] {
        for cell in letters(&cells_of(buffer, text)) {
            assert_eq!(cell.fg, Color::Red, "{text:?}");
        }
    }
}

/// A failed command keeps its last 10 lines, in the default colour; one that
/// worked keeps 3, grey. The line for the rest says `· Ctrl+O`, as does a
/// folded diff's.
#[test]
fn failed_command_keeps_ten_lines_in_the_default_colour() {
    let mut screen = Screen::new(50);
    screen.command("c1", "cargo clippy", &numbered(15, "error"), 101);
    screen.command("c2", "cargo test", &numbered(8, "ok"), 0);
    screen.completed(json!({
        "type": "fileChange", "id": "e1", "status": "completed",
        "changes": [{ "path": "a.txt", "kind": { "type": "update", "movePath": null },
            "diff": format!("--- a/a.txt\n+++ b/a.txt\n@@ -1,0 +1,20 @@\n{}", numbered(20, "+new")) }],
    }));
    let conversation = screen.conversation();
    let failed = card(&conversation, "  ✗ Run cargo clippy");
    let mut expected = vec!["    │ … 5 more lines · Ctrl+O".to_owned()];
    expected.extend((6..=15).map(|n| format!("    │ error {n}")));
    assert_eq!(failed[1..], expected[..]);
    let worked = card(&conversation, "  ● Run cargo test");
    assert_eq!(
        worked[1..],
        [
            "    │ … 5 more lines · Ctrl+O",
            "    │ ok 6",
            "    │ ok 7",
            "    │ ok 8",
        ]
    );
    assert!(
        conversation
            .iter()
            .any(|row| row.trim() == "… 8 more lines · Ctrl+O"),
        "the diff's fold: {conversation:#?}"
    );

    let buffer = screen.draw();
    for cell in letters(&cells_of(buffer, "error 15")) {
        assert!(!cell.modifier.contains(Modifier::DIM), "not grey");
        assert_eq!(cell.fg, Color::Reset, "the default colour");
    }
    for cell in letters(&cells_of(buffer, "ok 8")) {
        assert!(cell.modifier.contains(Modifier::DIM), "grey");
    }
}

/// With Ctrl+O, a command shows all its lines and all its output; over 500
/// lines, the first and last 250 and one row between saying how many are
/// left out. No `· Ctrl+O` rows are left.
#[test]
fn ctrl_o_shows_whole_commands_and_at_most_500_lines_of_output() {
    let mut screen = Screen::new(600);
    screen.command(
        "c1",
        "cargo build \\\n  --release\nls",
        &numbered(20, "small"),
        0,
    );
    screen.command("c2", "cargo test", &numbered(1200, "line"), 0);
    let folded = screen.conversation();
    assert!(
        folded
            .iter()
            .any(|row| row.starts_with("  ● Run cargo build \\ …"))
    );
    assert!(!folded.iter().any(|row| row.contains("--release")));

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let small = card(&conversation, "  ● Run cargo build \\");
    assert!(small[0].ends_with("exit 0 · 1.2s"), "{small:?}");
    let at = conversation
        .iter()
        .position(|row| row == &small[0])
        .unwrap();
    assert_eq!(conversation[at + 1], "          --release");
    assert_eq!(conversation[at + 2], "        ls");
    let small = card(&conversation[at + 2..], "        ls");
    let expected: Vec<String> = (1..=20).map(|n| format!("    │ small {n}")).collect();
    assert_eq!(small[1..], expected[..]);

    let long = card(&conversation, "  ● Run cargo test");
    let mut expected: Vec<String> = (1..=250).map(|n| format!("    │ line {n}")).collect();
    expected.push("    │ … 700 lines not shown".into());
    expected.extend((951..=1200).map(|n| format!("    │ line {n}")));
    assert_eq!(long[1..], expected[..]);
    assert!(
        !conversation
            .iter()
            .any(|row| row.contains("Ctrl+O") && row.contains("more lines")),
        "{conversation:#?}"
    );
}
