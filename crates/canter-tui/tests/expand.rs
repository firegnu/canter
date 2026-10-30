//! Ctrl+O shows everything folded in the conversation in place (thinking,
//! long command output, long diffs) and folds it again; and the three forms
//! of thinking: in progress, folded and shown in full.

use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::style::Modifier;
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

const NOW: i64 = 0;
const SESSION: &str = "s1";
const WIDTH: u16 = 60;
/// Rows below the conversation: status line, input box (3) and info rows (2).
const BELOW: usize = 6;
const DETAILS: &str = "Details shown · Ctrl+O to fold";

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

    fn finish_turn(&mut self) {
        self.event(json!({ "type": "turn.completed", "turn": {
            "id": "t1", "items": [], "status": "completed", "error": null,
            "input": [{ "type": "text", "text": "do it" }],
            "clientName": "canter-tui", "usage": null,
        }}));
        self.event(json!({ "type": "session.status", "status": "idle" }));
    }

    fn say(&mut self, item: &str, text: &str) {
        let message = json!({ "type": "agentMessage", "id": item, "text": text });
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": message }));
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": message }));
    }

    fn start_thinking(&mut self, item: &str) {
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": {
            "type": "reasoning", "id": item, "summary": [], "content": [],
        }}));
    }

    fn think(&mut self, item: &str, delta: &str) {
        self.event(json!({
            "type": "item.delta", "turnId": "t1", "itemId": item,
            "delta": { "type": "reasoningContent", "contentIndex": 0, "delta": delta },
        }));
    }

    fn thought(&mut self, item: &str, summary: &[&str], content: &[&str]) {
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
            "type": "reasoning", "id": item, "summary": summary, "content": content,
        }}));
    }

    fn command(&mut self, item: &str, command: &str, output: &str, status: &str) {
        let done = status != "inProgress";
        let item = json!({
            "type": "commandExecution", "id": item, "command": command, "cwd": "/work",
            "status": status, "aggregatedOutput": output,
            "exitCode": if done { json!(0) } else { Value::Null },
            "durationMs": if done { json!(1200) } else { Value::Null },
        });
        let kind = if done {
            "item.completed"
        } else {
            "item.started"
        };
        self.event(json!({ "type": kind, "turnId": "t1", "item": item }));
    }

    fn changed(&mut self, item: &str, path: &str, diff: &str) {
        self.event(json!({ "type": "item.completed", "turnId": "t1", "item": {
            "type": "fileChange", "id": item, "status": "completed",
            "changes": [{ "path": path, "kind": { "type": "update", "movePath": null }, "diff": diff }],
        }}));
    }

    /// The model wants to run `command` and asks first: the approval box opens.
    fn ask(&mut self, command: &str) {
        let item = json!({
            "type": "commandExecution", "id": "ask", "command": command, "cwd": "/work",
            "status": "inProgress", "aggregatedOutput": "", "exitCode": null, "durationMs": null,
        });
        self.event(json!({ "type": "item.started", "turnId": "t1", "item": item }));
        self.event(json!({ "type": "request.opened", "request": {
            "id": "r1", "turnId": "t1", "createdAt": NOW,
            "kind": {
                "type": "approval", "itemId": "ask", "tool": "shell",
                "arguments": { "command": command },
                "availableDecisions": ["accept", "acceptForSession", "decline", "cancel"],
            },
        }}));
        self.event(json!({ "type": "session.status", "status": "waiting_approval" }));
    }

    fn ctrl_o(&mut self) -> Option<Action> {
        self.ui.app_mut().handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            NOW,
        )
    }

    fn key(&mut self, code: KeyCode) -> Option<Action> {
        self.ui
            .app_mut()
            .handle_key(KeyEvent::new(code, KeyModifiers::NONE), NOW)
    }

    fn wheel(&mut self, kind: MouseEventKind) {
        self.ui.app_mut().handle_mouse(MouseEvent {
            kind,
            column: 10,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
    }

    fn draw(&mut self) -> &Buffer {
        self.ui.draw(NOW).unwrap();
        self.ui.backend().buffer()
    }

    fn lines(&mut self) -> Vec<String> {
        let buffer = self.draw();
        (0..buffer.area.height)
            .map(|y| row(buffer, y).trim_end().to_owned())
            .collect()
    }

    /// The conversation's rows, above the status line.
    fn conversation(&mut self) -> Vec<String> {
        let lines = self.lines();
        lines[..lines.len() - BELOW].to_vec()
    }

    fn status(&mut self) -> String {
        let lines = self.lines();
        lines[lines.len() - BELOW].clone()
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

/// Every non-blank cell of `text` is grey and italic.
fn grey_italic(buffer: &Buffer, text: &str) -> bool {
    cells_of(buffer, text)
        .iter()
        .filter(|cell| cell.symbol() != " " && !cell.symbol().is_empty())
        .all(|cell| {
            cell.modifier.contains(Modifier::DIM) && cell.modifier.contains(Modifier::ITALIC)
        })
}

fn numbered(count: usize, word: &str) -> String {
    (1..=count).map(|n| format!("{word} {n}\n")).collect()
}

/// While thinking, one grey italic row shows the latest line written; when
/// it is too long, only its end, after `…`.
#[test]
fn thinking_in_progress_shows_the_latest_line_on_one_row() {
    let mut screen = Screen::new(20);
    screen.start_thinking("r1");
    screen.draw();
    assert!(
        screen.conversation().contains(&"  Thinking…".to_owned()),
        "{:?}",
        screen.conversation()
    );

    screen.think("r1", "first I read the file.\n\nthen I change ");
    screen.think("r1", "the install steps");
    let conversation = screen.conversation();
    assert!(
        conversation.contains(&"  Thinking… then I change the install steps".to_owned()),
        "{conversation:?}"
    );
    assert!(!conversation.iter().any(|row| row.contains("first I read")));
    let buffer = screen.draw();
    assert!(grey_italic(
        buffer,
        "Thinking… then I change the install steps"
    ));

    let long = " and then I run cargo build to check that it still compiles fine";
    screen.think("r1", long);
    let conversation = screen.conversation();
    let row = conversation
        .iter()
        .find(|row| row.starts_with("  Thinking… "))
        .unwrap_or_else(|| panic!("{conversation:?}"));
    assert!(row.starts_with("  Thinking… …"), "{row:?}");
    assert!(row.ends_with("that it still compiles fine"), "{row:?}");
    assert_eq!(row.width(), usize::from(WIDTH), "{row:?}");
    assert_eq!(
        conversation
            .iter()
            .filter(|row| row.contains("Thinking"))
            .count(),
        1
    );
}

/// Finished thinking folds to `Thought: <first line>`, ending with `…` when
/// more follows. A summary is used when there is one, its leading
/// `**title**` without the stars.
#[test]
fn finished_thinking_folds_to_its_first_line() {
    let mut screen = Screen::new(20);
    screen.thought("r1", &[], &["Read the README first.\n\nThen edit it."]);
    screen.thought("r2", &[], &["Only one line."]);
    screen.thought(
        "r3",
        &["**Planning the change**\n\nI need to look at the README."],
        &["raw thoughts"],
    );
    let conversation = screen.conversation();
    for expected in [
        "  Thought: Read the README first.…",
        "  Thought: Only one line.",
        "  Thought: Planning the change…",
    ] {
        assert!(
            conversation.contains(&expected.to_owned()),
            "{expected:?} in {conversation:?}"
        );
    }
    assert!(!conversation.iter().any(|row| row.contains("Then edit")));
    assert!(!conversation.iter().any(|row| row.contains("raw thoughts")));
    let buffer = screen.draw();
    assert!(grey_italic(buffer, "Thought: Read the README first.…"));
}

/// Ctrl+O shows thinking in full: `Thought`, then the text as markdown,
/// grey and italic, each row after a grey `┊`. Ctrl+O again folds it.
#[test]
fn ctrl_o_shows_thinking_in_full() {
    let mut screen = Screen::new(24);
    screen.thought(
        "r1",
        &[],
        &["Read the README first.\n\n- check the install steps\n- run `cargo build`"],
    );
    screen.say("m1", "Done.");
    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    let at = conversation
        .iter()
        .position(|row| row == "  Thought")
        .unwrap_or_else(|| panic!("{conversation:?}"));
    let body: Vec<&String> = conversation[at + 1..]
        .iter()
        .take_while(|row| row.starts_with("  ┊"))
        .collect();
    let text: Vec<&str> = body.iter().map(|row| row[5..].trim()).collect();
    assert!(text.contains(&"Read the README first."), "{body:?}");
    assert!(text.contains(&"• check the install steps"), "{body:?}");
    assert!(text.contains(&"• run cargo build"), "markdown: {body:?}");
    assert!(
        !conversation.iter().any(|row| row.contains("Thought:")),
        "{conversation:?}"
    );
    assert!(
        conversation[at + 1 + body.len()..]
            .iter()
            .any(|row| row == "  Done.")
    );

    let buffer = screen.draw();
    assert!(grey_italic(buffer, "Thought"));
    assert!(grey_italic(buffer, "Read the README first."));
    assert!(grey_italic(buffer, "check the install steps"));
    let bar = cells_of(buffer, "┊")[0];
    assert!(bar.modifier.contains(Modifier::DIM), "the ┊ is grey");

    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    assert!(
        conversation.contains(&"  Thought: Read the README first.…".to_owned()),
        "{conversation:?}"
    );
    assert!(!conversation.iter().any(|row| row.contains('┊')));
}

/// Ctrl+O shows all of a long command's output, finished or running, and all
/// of a long diff in the history; the status line says so, also when idle.
/// Ctrl+O again folds them.
#[test]
fn ctrl_o_shows_long_output_and_diffs_in_full_and_folds_them_again() {
    let mut screen = Screen::new(80);
    screen.command("c1", "cargo test", &numbered(20, "done"), "completed");
    let edit = format!(
        "--- a/a.txt\n+++ b/a.txt\n@@ -1,10 +1,10 @@\n{}{}",
        numbered(10, "-old"),
        numbered(10, "+new")
    );
    screen.changed("e1", "a.txt", &edit);
    screen.command("c2", "cargo build", &numbered(10, "busy"), "inProgress");
    let folded = screen.conversation();
    assert!(!folded.iter().any(|row| row.ends_with("done 1")));
    assert!(!folded.iter().any(|row| row.ends_with("busy 1")));
    assert!(folded.iter().any(|row| row.contains("more lines")));
    assert!(!screen.status().contains(DETAILS));

    assert_eq!(screen.ctrl_o(), None);
    let shown = screen.conversation();
    for n in 1..=20 {
        assert!(
            shown.iter().any(|row| row == &format!("    ┃ done {n}")),
            "done {n}: {shown:#?}"
        );
    }
    for n in 1..=10 {
        assert!(shown.iter().any(|row| row == &format!("    ┃ busy {n}")));
        assert!(shown.iter().any(|row| row.ends_with(&format!("+ new {n}"))));
        assert!(shown.iter().any(|row| row.ends_with(&format!("- old {n}"))));
    }
    assert!(
        !shown.iter().any(|row| row.contains("more lines")),
        "{shown:#?}"
    );
    assert!(!shown.iter().any(|row| row.contains("showing the last")));
    // A narrow status row keeps the complete background shortcut.
    assert_eq!(
        screen.status(),
        " ⠋ Running cargo build · 0s ·… · ctrl+b to run in background"
    );

    screen.finish_turn();
    assert_eq!(screen.status(), format!(" {DETAILS}"), "idle too");

    assert_eq!(screen.ctrl_o(), None);
    // Folded as before; the turn has ended, so the file it changed and its
    // time follow now.
    let mut refolded = folded.clone();
    refolded.retain(|row| !row.contains("ctrl+b to run in background"));
    refolded.push(String::new());
    let end = refolded.iter().rposition(|row| !row.is_empty()).unwrap() + 1;
    refolded[end + 1] = "  ● Changed 1 file  +10 −10".to_owned();
    refolded[end + 2] = "      a.txt  +10 −10".to_owned();
    refolded[end + 3] = "  ─ Worked for <1s ─".to_owned();
    assert_eq!(screen.conversation(), refolded);
    assert_eq!(screen.status(), "");
}

/// Scrolled back, Ctrl+O keeps the item at the top of the view at the top.
/// Following the latest output, the view stays at the bottom.
#[test]
fn ctrl_o_keeps_the_top_item_in_place() {
    let mut screen = Screen::new(20);
    screen.command("c1", "cargo test", &numbered(30, "out"), "completed");
    screen.thought("r1", &[], &[&numbered(10, "idea")]);
    for n in 1..=40 {
        screen.say(&format!("m{n}"), &format!("line {n}"));
    }
    screen.draw();

    // Following: the latest output stays right above the status line.
    assert_eq!(screen.ctrl_o(), None);
    assert_eq!(screen.conversation().last().unwrap(), "  line 40");
    assert_eq!(screen.ctrl_o(), None);
    assert_eq!(screen.conversation().last().unwrap(), "  line 40");

    // Scrolled back to a message after the long items.
    for _ in 0..5 {
        screen.wheel(MouseEventKind::ScrollUp);
    }
    // The prompt, scrolled out of view, shows on the top row, above the
    // conversation.
    let conversation = screen.conversation();
    assert_eq!(conversation[0], "› do it");
    let top = conversation[1].clone();
    assert!(top.starts_with("  line "), "{top:?}");
    for _ in 0..2 {
        assert_eq!(screen.ctrl_o(), None);
        let conversation = screen.conversation();
        assert_eq!(conversation[1], top, "{conversation:#?}");
    }

    // Scrolled back into the command's output: the command stays on top.
    for _ in 0..30 {
        screen.key(KeyCode::PageUp);
    }
    screen.wheel(MouseEventKind::ScrollDown);
    let conversation = screen.conversation();
    assert_eq!(conversation[0], "› do it");
    assert!(conversation[1].starts_with("    ┃"), "{conversation:#?}");
    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    assert_eq!(conversation[0], "› do it");
    assert!(
        conversation[1].starts_with("  ● Run cargo test"),
        "{conversation:#?}"
    );
    assert_eq!(conversation[2], "    ┃ out 1");
    assert_eq!(screen.ctrl_o(), None);
    let conversation = screen.conversation();
    assert_eq!(conversation[0], "› do it");
    assert!(
        conversation[1].starts_with("  ● Run cargo test"),
        "{conversation:#?}"
    );
}

/// With the approval box open, Ctrl+O still switches the conversation; the
/// box is the same, and nothing is answered.
#[test]
fn ctrl_o_works_with_the_approval_box_open() {
    let mut screen = Screen::new(40);
    screen.command("c1", "cargo test", &numbered(8, "out"), "completed");
    screen.ask("cargo build --release");
    let lines = screen.lines();
    let box_top = lines
        .iter()
        .position(|row| row.starts_with('╭'))
        .expect("the approval box");
    let approval_box = lines[box_top..].to_vec();
    assert!(!lines[..box_top].iter().any(|row| row == "    ┃ out 1"));

    assert_eq!(screen.ctrl_o(), None);
    let lines = screen.lines();
    let box_top_after = lines
        .iter()
        .position(|row| row.starts_with('╭'))
        .expect("the approval box");
    assert_eq!(lines[box_top_after..], approval_box[..], "{lines:#?}");
    assert!(
        lines[..box_top_after]
            .iter()
            .any(|row| row == "    ┃ out 1")
    );
    // The status line, cut at this width.
    assert!(
        lines[box_top_after - 1].contains(" · Details shown · Ctrl+O to"),
        "{lines:#?}"
    );

    assert_eq!(screen.ctrl_o(), None);
    let lines = screen.lines();
    assert!(!lines.iter().any(|row| row == "    ┃ out 1"));
    assert_eq!(lines[box_top..], approval_box[..]);
}
