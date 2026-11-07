//! Tool cards: `• Verb object` with the status two columns after it,
//! output after `    ┃ `, a command's under `  └ `. A command wraps under
//! where it starts and is coloured as Bash. MCP calls keep the first lines
//! of their result, web fetches show only notes, failed commands keep their
//! last lines in the default colour, and Ctrl+O shows at most 500 lines of
//! one item's output.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::theme::{Background, Depth, Theme};
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

/// The row starting with `title` and its output rows after it: after
/// `    ┃`, or a command's after `  └` and then four spaces.
fn card(conversation: &[String], title: &str) -> Vec<String> {
    let at = conversation
        .iter()
        .position(|row| row.starts_with(title))
        .unwrap_or_else(|| panic!("{title:?} in {conversation:#?}"));
    let mut rows = vec![conversation[at].clone()];
    let branch = conversation
        .get(at + 1)
        .is_some_and(|row| row.starts_with("  └ "));
    rows.extend(
        conversation[at + 1..]
            .iter()
            .enumerate()
            .take_while(|(index, row)| {
                row.starts_with("    ┃") || (branch && (*index == 0 || row.starts_with("    ")))
            })
            .map(|(_, row)| row.clone()),
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
    let rows = card(&conversation, "• Call github/search_issues  {\"limit\"");
    assert_eq!(
        rows,
        [
            "• Call github/search_issues  {\"limit\":3,\"query\":\"diff render\"}",
            "    ┃ issue 1",
            "    ┃ issue 2",
            "    ┃ issue 3",
            "    ┃ … 4 more lines · Ctrl+O",
        ]
    );
    let long_row = conversation
        .iter()
        .find(|row| row.starts_with("• Call github/search_issues  {\"query\":\"xxx"))
        .unwrap_or_else(|| panic!("{conversation:#?}"));
    assert_eq!(long_row.width(), usize::from(WIDTH), "cut to fit");

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let rows = card(&conversation, "• Call github/search_issues  {\"limit\"");
    let expected: Vec<String> = (1..=7).map(|n| format!("    ┃ issue {n}")).collect();
    assert_eq!(rows[1..], expected[..]);
}

/// A web fetch that brought a page back is one row: `• Fetch <url>` and the
/// size after it. The page itself shows only with Ctrl+O.
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
    let rows = card(&conversation, "• Fetch https://docs.example/page");
    assert_eq!(rows.len(), 1, "{conversation:#?}");
    assert_eq!(rows[0], "• Fetch https://docs.example/page  18.4 KB");
    let small = card(&conversation, "• Fetch https://docs.example/small");
    assert_eq!(small, ["• Fetch https://docs.example/small  12 B"]);
    assert!(!conversation.iter().any(|row| row.contains("Docs")));

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let rows = card(&conversation, "• Fetch https://docs.example/page");
    assert_eq!(rows[1], "    ┃ # Docs");
    let small = card(&conversation, "• Fetch https://docs.example/small");
    assert_eq!(small[1..], ["    ┃ hello world"]);
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
    let redirect = card(&conversation, "• Fetch https://example.com/old");
    assert!(redirect[0].ends_with("  redirect"), "{redirect:?}");
    assert_eq!(
        redirect[1..],
        ["    ┃ Redirected to https://example.org/new"]
    );
    assert_eq!(
        card(&conversation, "• Fetch https://example.com/a.png"),
        [
            "• Fetch https://example.com/a.png",
            "    ┃ [Content omitted: image/png, 20480 bytes]",
        ]
    );
    assert_eq!(
        card(&conversation, "• Fetch https://example.com/big"),
        [
            "• Fetch https://example.com/big",
            "    ┃ Download exceeds 10485760 bytes; content omitted.",
        ]
    );
}

/// A failed fetch: a red `•`, a red `failed` after it, and the reason below in red.
#[test]
fn failed_web_fetch_shows_its_reason_in_red() {
    let mut screen = Screen::new(30);
    let reason = "web_fetch request failed (connection or TLS error)";
    screen.fetch("w1", "http://localhost:9/", reason, "failed");
    let conversation = screen.conversation();
    let rows = card(&conversation, "• Fetch http://localhost:9/");
    assert_eq!(rows[0], "• Fetch http://localhost:9/  failed");
    assert_eq!(rows[1..], [format!("    ┃ {reason}")]);
    let buffer = screen.draw();
    assert_eq!(cells_of(buffer, "• Fetch")[0].fg, Color::Red);
    for text in ["failed", reason] {
        for cell in letters(&cells_of(buffer, text)) {
            assert_eq!(cell.fg, Color::Red, "{text:?}");
        }
    }
}

#[test]
fn web_search_card_shows_query_count_and_expanded_titles_and_urls() {
    let mut screen = Screen::new(35);
    screen.completed(json!({"type":"webSearch","id":"s1","query":"Rust docs","status":"completed","results":[
        {"title":"Rust book","url":"https://doc.rust-lang.org/book/","summary":"Model-only summary"},
        {"title":"Reference","url":"https://doc.rust-lang.org/reference/","summary":"Language details"}
    ]}));
    let rows = screen.conversation();
    let rows = card(&rows, "• Search Rust docs");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].ends_with("2 results"), "{rows:?}");
    assert_eq!(screen.ctrl_o(), None);
    let text = screen.conversation().join("\n");
    for part in [
        "Rust book",
        "Reference",
        "https://doc.rust-lang.org/book/",
        "https://doc.rust-lang.org/reference/",
    ] {
        assert!(text.contains(part), "{text}");
    }
    assert!(!text.contains("Model-only summary"));
}

#[test]
fn web_search_empty_and_failed_cards_show_english_status_and_error() {
    let mut screen = Screen::new(35);
    screen.completed(json!({"type":"webSearch","id":"empty","query":"No matches","status":"completed","results":[]}));
    assert!(screen.conversation().join("\n").contains("0 results"));
    let reason = "web_search has insufficient balance (402); top up the account";
    screen.completed(json!({"type":"webSearch","id":"failed","query":"Paid search","status":"failed","results":[],"error":reason}));
    let text = screen.conversation().join("\n");
    assert!(
        text.contains("Search Paid search") && text.contains("failed") && text.contains(reason),
        "{text}"
    );
    for cell in letters(&cells_of(screen.draw(), reason)) {
        assert_eq!(cell.fg, Color::Red);
    }
}

#[test]
fn ast_grep_card_shows_pattern_page_count_and_expanded_matches() {
    let mut screen = Screen::new(45);
    screen.completed(json!({"type":"astGrep","id":"a1","pattern":"$X.unwrap()","totalMatches":42,"status":"completed","matches":[
        {"path":"src/main.rs","line":12,"column":5,"text":"value.unwrap()"},
        {"path":"src/lib.rs","line":3,"column":9,"text":"x.unwrap()\n  .unwrap()"}
    ]}));
    let rows = screen.conversation();
    let rows = card(&rows, "• AST search $X.unwrap()");
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].ends_with("2 matches"), "{rows:?}");
    assert_eq!(screen.ctrl_o(), None);
    let text = screen.conversation().join("\n");
    for part in [
        "src/main.rs:12:5",
        "value.unwrap()",
        "src/lib.rs:3:9",
        ".unwrap()",
    ] {
        assert!(text.contains(part), "{text}");
    }
}

/// A failed command keeps its last 10 lines, in the default colour, after
/// `… N lines above (Ctrl+O to expand)`; one that worked keeps its first 3,
/// grey, then `+ N lines (Ctrl+O to expand)`. A folded diff's line still
/// says `· Ctrl+O`.
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
    let failed = card(&conversation, "• Ran cargo clippy");
    let mut expected = vec!["  └ … 5 lines above (Ctrl+O to expand)".to_owned()];
    expected.extend((6..=15).map(|n| format!("    error {n}")));
    assert_eq!(failed[1..], expected[..]);
    let worked = card(&conversation, "• Ran cargo test");
    assert_eq!(
        worked[1..],
        [
            "  └ ok 1",
            "    ok 2",
            "    ok 3",
            "    + 5 lines (Ctrl+O to expand)",
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
    for cell in letters(&cells_of(buffer, "ok 3")) {
        assert_eq!(cell.fg, Color::DarkGray, "grey");
    }
}

/// A search that found nothing is not a failure: `• Ran rg …` with a grey
/// `•` and `No matches`, no `exit 1`. Only a whole `rg` or `grep` call
/// with exit code 1 and no output counts: another command's exit code 1, a
/// search that printed something, and a search combined with other commands
/// keep the red `•` and `exit 1`.
#[test]
fn a_search_that_finds_nothing_says_no_matches_in_grey() {
    let mut screen = Screen::new(40);
    screen.command("c1", "rg missing src", "", 1);
    screen.command("c2", "grep -rn missing .", "", 1);
    screen.command("c3", "rg \"a|b\"", "", 1);
    screen.command("c4", "diff a.txt b.txt", "1c1\n< a\n---\n> b\n", 1);
    screen.command("c5", "rg missing | wc -l", "", 1);
    screen.command("c6", "rg missing && echo done", "", 1);
    screen.command("c7", "rg missing src", "src/a.rs:1:missing\n", 1);
    let conversation = screen.conversation();

    for title in [
        "• Ran rg missing src  ",
        "• Ran grep -rn missing .  ",
        "• Ran rg \"a|b\"  ",
    ] {
        let rows = card(&conversation, title);
        assert_eq!(rows.len(), 1, "{title:?}: {conversation:#?}");
        assert_eq!(rows[0], format!("{title}No matches · 1.2s"));
    }
    for title in [
        "• Ran diff a.txt b.txt  ",
        "• Ran rg missing | wc -l  ",
        "• Ran rg missing && echo done  ",
        "• Ran rg missing src  exit",
    ] {
        let rows = card(&conversation, title);
        assert!(rows[0].ends_with("  exit 1 · 1.2s"), "{title:?}: {rows:?}");
    }

    let buffer = screen.draw();
    assert_ne!(
        cells_of(buffer, "• Ran rg missing src  No")[0].fg,
        Color::Red
    );
    assert_eq!(cells_of(buffer, "• Ran diff")[0].fg, Color::Red);
    for cell in letters(&cells_of(buffer, "No matches")) {
        assert!(cell.modifier.contains(Modifier::DIM), "grey");
        assert_eq!(cell.fg, Color::Reset);
    }
    for cell in letters(&cells_of(buffer, "exit 1")) {
        assert_eq!(cell.fg, Color::Red);
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
    assert!(!folded.iter().any(|row| row.contains("\\ …")));
    assert!(folded.iter().any(|row| row == "        --release"));
    assert!(!folded.iter().any(|row| row == "    small 20"));

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let at = conversation
        .iter()
        .position(|row| row == "• Ran cargo build \\")
        .unwrap_or_else(|| panic!("{conversation:#?}"));
    // A command of several rows has its status on a row of its own.
    assert_eq!(
        conversation[at + 1..at + 4],
        ["        --release", "      ls", "      1.2s"]
    );
    let small = card(&conversation[at + 3..], "      1.2s");
    let mut expected = vec!["  └ small 1".to_owned()];
    expected.extend((2..=20).map(|n| format!("    small {n}")));
    assert_eq!(small[1..], expected[..]);

    let long = card(&conversation, "• Ran cargo test");
    let mut expected = vec!["  └ line 1".to_owned()];
    expected.extend((2..=250).map(|n| format!("    line {n}")));
    expected.push("    … 700 lines not shown".into());
    expected.extend((951..=1200).map(|n| format!("    line {n}")));
    assert_eq!(long[1..], expected[..]);
    assert!(
        !conversation.iter().any(|row| row.contains("Ctrl+O")),
        "{conversation:#?}"
    );
}

/// A file that could not be read is a row of its own: a red `•` and a red
/// `failed`, its verb red too; one that was read goes under `• Explored`,
/// its `•` and title cyan.
#[test]
fn failed_file_read_shows_a_red_mark() {
    let mut screen = Screen::new(30);
    for (id, path, status) in [
        ("f1", "/work/src/theme.rs", "failed"),
        ("f2", "/work/src/cells.rs", "completed"),
    ] {
        screen.completed(json!({
            "type": "fileRead", "id": id, "path": path, "startLine": 1,
            "content": "one\ntwo\n", "status": status,
        }));
    }
    let conversation = screen.conversation();
    assert_eq!(
        conversation[2..7],
        [
            "• Read src/theme.rs  failed",
            "",
            "• Explored",
            "  └ Read cells.rs",
            "",
        ]
    );
    let buffer = screen.draw();
    assert_eq!(cells_of(buffer, "• Read src/theme.rs")[0].fg, Color::Red);
    for cell in letters(&cells_of(buffer, "failed")) {
        assert_eq!(cell.fg, Color::Red);
    }
    for cell in letters(&cells_of(buffer, "• Read src/theme.rs")[..6]) {
        assert_eq!(cell.fg, Color::Red, "the mark and verb are red");
    }
    for cell in letters(&cells_of(buffer, "• Explored")) {
        assert_eq!(cell.fg, Color::Cyan, "the mark and title are cyan");
    }
}

/// `echo arg01 arg02 …` up to `arg{count}`.
fn args(count: usize) -> String {
    let mut words = vec!["echo".to_owned()];
    words.extend((1..=count).map(|n| format!("arg{n:02}")));
    words.join(" ")
}

/// A row of a command after the title: `arg{from}` to `arg{to}` under the
/// command's start.
fn arg_row(from: usize, to: usize) -> String {
    let words: Vec<String> = (from..=to).map(|n| format!("arg{n:02}")).collect();
    format!("      {}", words.join(" "))
}

/// A long command wraps by words, its rows under where it starts, the status
/// on a row of its own under it. Folded it takes at most 5 rows and then
/// says how many more there are; Ctrl+O shows all of it.
#[test]
fn a_long_command_wraps_under_its_start_and_folds_past_five_rows() {
    let mut screen = Screen::new(40);
    screen.command("c1", &args(80), "done\n", 0);
    let conversation = screen.conversation();
    let at = conversation
        .iter()
        .position(|row| row.starts_with("• Ran echo"))
        .unwrap_or_else(|| panic!("{conversation:#?}"));
    let first = format!("• Ran {}", args(11));
    assert_eq!(
        conversation[at..at + 9],
        [
            first.clone(),
            arg_row(12, 23),
            arg_row(24, 35),
            arg_row(36, 47),
            arg_row(48, 59),
            "      … 2 more lines · Ctrl+O".to_owned(),
            "      1.2s".to_owned(),
            "  └ done".to_owned(),
            String::new(),
        ]
    );

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let at = conversation.iter().position(|row| row == &first).unwrap();
    assert_eq!(
        conversation[at..at + 10],
        [
            first.clone(),
            arg_row(12, 23),
            arg_row(24, 35),
            arg_row(36, 47),
            arg_row(48, 59),
            arg_row(60, 71),
            arg_row(72, 80),
            "      1.2s".to_owned(),
            "  └ done".to_owned(),
            String::new(),
        ]
    );
}

/// Each line of a command of several lines wraps the same way, and the rows
/// wrapping adds count toward the 5.
#[test]
fn each_line_of_a_command_wraps_and_counts_toward_the_fold() {
    let mut screen = Screen::new(40);
    let command = format!(
        "cat <<'EOF'\n{}\nEOF\nwc -l notes.txt\nls\nrm notes.txt",
        args(20)
    );
    screen.command("c1", &command, "", 0);
    let conversation = screen.conversation();
    let at = conversation
        .iter()
        .position(|row| row.starts_with("• Ran cat <<'EOF'"))
        .unwrap_or_else(|| panic!("{conversation:#?}"));
    assert_eq!(conversation[at], "• Ran cat <<'EOF'");
    let first_args: Vec<String> = (1..=11).map(|n| format!("arg{n:02}")).collect();
    assert_eq!(
        conversation[at + 1..at + 7],
        [
            format!("      echo {}", first_args.join(" ")),
            arg_row(12, 20),
            "      EOF".to_owned(),
            "      wc -l notes.txt".to_owned(),
            "      … 2 more lines · Ctrl+O".to_owned(),
            "      1.2s".to_owned(),
        ]
    );

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let at = conversation
        .iter()
        .position(|row| row.starts_with("• Ran cat <<'EOF'"))
        .unwrap();
    assert_eq!(
        conversation[at + 4..at + 8],
        [
            "      wc -l notes.txt",
            "      ls",
            "      rm notes.txt",
            "      1.2s"
        ]
    );
}

/// The command in a card is coloured as Bash, like the approval box's.
#[test]
fn the_command_is_coloured_as_bash() {
    let mut screen = Screen::new(30);
    screen.command("c1", "grep -rn \"pub struct\" src | head -5", "", 0);
    let buffer = screen.draw();
    for cell in letters(&cells_of(buffer, "\"pub struct\"")) {
        assert_ne!(cell.fg, Color::Reset, "a string has a colour");
    }
    for cell in letters(&cells_of(buffer, "Ran ")) {
        assert!(cell.modifier.contains(Modifier::BOLD));
    }
}

/// The programs a command runs stand out (P2-189): the first word, and the
/// first after `;` and `|`, blue and bold. Arguments, quotes and operators
/// keep their Bash colours. The user's `!` commands too.
#[test]
fn programs_in_a_command_are_blue_and_bold() {
    let mut screen = Screen::new(30);
    screen.command("c1", "rg --files -g '!node_modules'", "", 0);
    screen.command("c2", "git status --short; echo \"---\"", "", 0);
    screen.command("c3", "npm test 2>&1 | tail -25", "", 0);
    screen.completed(json!({
        "type": "commandExecution", "id": "u1", "command": "cargo fmt", "cwd": "/work",
        "source": "userShell", "status": "completed", "aggregatedOutput": "",
        "exitCode": 0, "durationMs": 0,
    }));
    let buffer = screen.draw();
    for text in [
        "rg --files",
        "git status",
        "echo \"---\"",
        "npm test",
        "tail -25",
        "cargo fmt",
    ] {
        let program = text.split(' ').next().unwrap();
        for cell in &cells_of(buffer, text)[..program.len()] {
            assert_eq!(cell.fg, Color::Blue, "{text:?}");
            assert!(cell.modifier.contains(Modifier::BOLD), "{text:?}");
        }
        for cell in letters(&cells_of(buffer, text)[program.len()..]) {
            assert_ne!(cell.fg, Color::Blue, "{text:?}");
            assert!(!cell.modifier.contains(Modifier::BOLD), "{text:?}");
        }
    }
    // A quoted argument keeps its colour as a string.
    for cell in letters(&cells_of(buffer, "'!node_modules'")) {
        assert_ne!(cell.fg, Color::Reset, "a string has a colour");
        assert_ne!(cell.fg, Color::Blue);
    }
    for text in ["--files", "status --short", "2>&1 | ", "-25"] {
        for cell in letters(&cells_of(buffer, text)) {
            assert_ne!(cell.fg, Color::Blue, "{text:?}");
            assert!(!cell.modifier.contains(Modifier::BOLD), "{text:?}");
        }
    }
}

/// Without colours (`NO_COLOR`) the programs are bold only.
#[test]
fn without_colours_programs_are_bold_only() {
    canter_tui::theme::set(Theme {
        depth: Depth::None,
        background: Background::Dark,
    });
    let mut screen = Screen::new(30);
    screen.command("c1", "git log | head -5", "", 0);
    let buffer = screen.draw();
    for text in ["git", "head"] {
        for cell in cells_of(buffer, &format!("{text} "))
            .iter()
            .take(text.len())
        {
            assert_eq!(cell.fg, Color::Reset, "{text:?}");
            assert!(cell.modifier.contains(Modifier::BOLD), "{text:?}");
        }
    }
    for cell in letters(&cells_of(buffer, "log | ")) {
        assert!(!cell.modifier.contains(Modifier::BOLD));
    }
}

/// A command's output is a grey foreground (P2-189), not the terminal's
/// dim, while it runs, once it worked and with Ctrl+O; a failed command's
/// output keeps the default colour.
#[test]
fn command_output_is_grey_unless_the_command_failed() {
    let mut screen = Screen::new(40);
    screen.command("c1", "cargo test", "all passed\n", 0);
    screen.command("c2", "cargo clippy", "an error\n", 101);
    screen.event(json!({ "type": "item.started", "turnId": "t1", "item": {
        "type": "commandExecution", "id": "c3", "command": "sleep 60", "cwd": "/work",
        "status": "inProgress", "aggregatedOutput": "still going\n",
    }}));
    let grey = |screen: &mut Screen| {
        let buffer = screen.draw();
        for text in ["all passed", "still going"] {
            for cell in letters(&cells_of(buffer, text)) {
                assert_eq!(cell.fg, Color::DarkGray, "{text:?}");
                assert!(!cell.modifier.contains(Modifier::DIM), "{text:?}");
            }
        }
        for cell in letters(&cells_of(buffer, "an error")) {
            assert_eq!(cell.fg, Color::Reset, "the default colour");
            assert!(!cell.modifier.contains(Modifier::DIM));
        }
    };
    grey(&mut screen);
    assert_eq!(screen.ctrl_o(), None);
    grey(&mut screen);
}

/// Other cards' output follows a thick bar: grey on a card that worked, is
/// running or was declined, red on one that failed. A command's output
/// hangs under a grey `└` either way. The text keeps its colours.
#[test]
fn the_output_bar_is_red_when_the_card_failed() {
    let mut screen = Screen::new(60);
    screen.command("c1", "cargo test", "fine\n", 0);
    screen.command("c2", "cargo clippy", &numbered(15, "broken"), 101);
    screen.mcp("m1", json!({}), "mcp worked\n", "completed");
    screen.mcp("m2", json!({}), "mcp broke\n", "failed");
    screen.fetch("w1", "http://localhost:9/", "fetch broke", "failed");
    let buffer = screen.draw();
    let bar = cells_of(buffer, "┃ mcp worked")[0];
    assert_eq!(bar.symbol(), "┃");
    assert!(bar.modifier.contains(Modifier::DIM), "grey");
    assert_ne!(bar.fg, Color::Red);
    for text in ["┃ mcp broke", "┃ fetch broke"] {
        let bar = cells_of(buffer, text)[0];
        assert_eq!(bar.symbol(), "┃");
        assert_eq!(bar.fg, Color::Red, "{text:?}");
    }
    for text in ["└ fine", "└ … 5 lines above"] {
        let branch = cells_of(buffer, text)[0];
        assert_eq!(branch.symbol(), "└");
        assert!(branch.modifier.contains(Modifier::DIM), "{text:?} grey");
        assert_ne!(branch.fg, Color::Red, "{text:?}");
    }
    for cell in letters(&cells_of(buffer, "… 5 lines above (Ctrl+O to expand)")) {
        assert!(cell.modifier.contains(Modifier::DIM), "the note stays grey");
    }
}

#[test]
fn mcp_search_card_shows_query_and_tool_names() {
    let mut screen = Screen::new(24);
    screen.completed(json!({"type":"mcpToolSearch","id":"search","query":"issue","tools":["mcp__github__search_issues","mcp__gitlab__issues"],"status":"completed"}));
    let text = screen.conversation().join("\n");
    assert!(text.contains("Searched MCP tools \"issue\""), "{text}");
    assert!(text.contains("2 found"), "{text}");
    assert!(text.contains("mcp__github__search_issues"), "{text}");
    assert!(text.contains("mcp__gitlab__issues"), "{text}");
}

/// With the sandbox on, what runs outside it says so after its status in grey:
/// the user's `!` commands (also one still running, as `!cmd &` is), MCP
/// tools and web_fetch; never the model's own commands, and nothing at all
/// when the sandbox is off or the server does not say.
#[test]
fn cards_outside_the_sandbox_say_so_only_while_it_is_on() {
    for sandbox in [Some(true), Some(false), None] {
        let mut screen = Screen::new(30);
        let mut settings = json!({
            "cwd": "/work", "name": null, "model": "test-model", "approvalPolicy": "alwaysAsk",
        });
        if let Some(enabled) = sandbox {
            settings["sandboxEnabled"] = json!(enabled);
        }
        screen.event(json!({ "type": "session.updated", "settings": settings }));
        screen.command("model", "cargo test", "ok", 0);
        screen.completed(json!({
            "type": "commandExecution", "id": "user", "command": "git push", "cwd": "/work",
            "source": "userShell", "status": "completed", "aggregatedOutput": "done",
            "exitCode": 0, "durationMs": 1200,
        }));
        screen.event(json!({ "type": "item.started", "turnId": "t1", "item": {
            "type": "commandExecution", "id": "background", "command": "sleep 60", "cwd": "/work",
            "source": "userShell", "status": "inProgress", "aggregatedOutput": "",
        }}));
        screen.mcp("mcp", json!({}), "found", "completed");
        screen.fetch("fetch", "https://example.com/", "page", "completed");
        let rows = screen.conversation();
        let card = |start: &str| {
            rows.iter()
                .find(|row| row.contains(start))
                .unwrap_or_else(|| panic!("{start:?} in {rows:#?}"))
                .clone()
        };
        let marked = [
            "! git push",
            "! sleep 60",
            "Call github/search_issues",
            "Fetch https://example.com/",
        ];
        if sandbox == Some(true) {
            for start in marked {
                assert!(
                    card(start).ends_with("not sandboxed"),
                    "{start:?}: {rows:#?}"
                );
            }
            assert!(card("! git push").ends_with("Ran ! git push  1.2s · not sandboxed"));
            assert!(card("! sleep 60").ends_with("Running ! sleep 60  not sandboxed"));
            assert!(card("Fetch https://example.com/").ends_with("4 B · not sandboxed"));
            assert!(!card("Ran cargo test").contains("sandboxed"));
            // Grey like the rest of the status on the right.
            let buffer = screen.draw();
            let y = rows
                .iter()
                .position(|row| row.contains("! git push"))
                .unwrap() as u16;
            let x = (card("! git push").width() - "not sandboxed".len()) as u16;
            let cell = &buffer[(x, y)];
            assert!(cell.modifier.contains(Modifier::DIM), "{cell:?}");
        } else {
            assert!(
                rows.iter().all(|row| !row.contains("sandboxed")),
                "{rows:#?}"
            );
        }
    }
}
