use canter_protocol::{Event, SessionSnapshot};
use canter_tui::{App, Export, Ui};
use ratatui::{
    backend::TestBackend,
    style::{Color, Modifier},
};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

struct Screen {
    ui: Ui<TestBackend>,
}
impl Screen {
    fn new() -> Self {
        let mut screen = Self {
            ui: Ui::new(
                TestBackend::new(70, 32),
                App::new(SessionSnapshot::empty("s1"), None, 0),
            )
            .unwrap(),
        };
        screen.event(json!({"type":"session.updated","settings":{"cwd":"/work","model":"fake","name":null,"approvalPolicy":"alwaysAsk"}}));
        screen.event(json!({"type":"turn.started","turn":{"id":"t1","items":[],"input":[],"clientName":"test","status":"inProgress","error":null,"usage":null}}));
        screen.event(json!({"type":"session.status","status":"working"}));
        screen
    }
    fn event(&mut self, mut value: Value) {
        value["seq"] = json!(self.ui.app().snapshot().last_event_seq + 1);
        value["sessionId"] = json!("s1");
        value["timestamp"] = json!(0);
        let event: Event = serde_json::from_value(value).unwrap();
        self.ui.app_mut().apply_event(&event, 0);
    }
    fn plan(&mut self, id: &str, plan: Value) {
        self.event(json!({"type":"item.completed","turnId":"t1","item":{"type":"plan","id":id,"status":"completed","explanation":"Progress","plan":plan}}));
    }
    fn lines(&mut self) -> Vec<String> {
        self.ui.draw(0).unwrap();
        let buffer = self.ui.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                let mut line = String::new();
                let mut x = 0;
                while x < buffer.area.width {
                    let text = buffer[(x, y)].symbol();
                    line.push_str(text);
                    x += text.width().max(1) as u16;
                }
                line.trim_end().to_owned()
            })
            .collect()
    }
    fn pinned(&mut self) -> Vec<String> {
        let lines = self.lines();
        let status = lines
            .iter()
            .position(|line| line.contains("Esc to interrupt"))
            .unwrap();
        let input = lines.iter().position(|line| line.starts_with('╭')).unwrap();
        lines[status + 1..input].to_vec()
    }
}

fn steps() -> Value {
    json!([
        {"step":"Read","status":"completed"},
        {"step":"Implement","status":"inProgress"},
        {"step":"Verify","status":"pending"}
    ])
}

#[test]
fn plan_cards_pin_unfinished_steps_and_collapse_after_completion() {
    let mut screen = Screen::new();
    assert!(screen.pinned().is_empty());
    screen.plan("p1", steps());
    let lines = screen.lines();
    assert!(
        lines
            .iter()
            .any(|line| line == "  ● Updated Plan · 1/3 complete"),
        "{lines:#?}"
    );
    assert_eq!(screen.pinned(), ["  □ Implement", "  □ Verify"]);
    let read = lines
        .iter()
        .position(|line| line.contains("✔ Read"))
        .unwrap() as u16;
    let active = lines
        .iter()
        .position(|line| line.contains("□ Implement"))
        .unwrap() as u16;
    let buffer = screen.ui.backend().buffer();
    assert!(buffer[(6, read)].modifier.contains(Modifier::CROSSED_OUT));
    assert!(buffer[(6, read)].modifier.contains(Modifier::DIM));
    assert!(buffer[(6, active)].modifier.contains(Modifier::BOLD));
    assert_eq!(buffer[(6, active)].fg, Color::Cyan);
    let snapshot = screen.ui.app().snapshot().clone();
    let mut other = Screen {
        ui: Ui::new(TestBackend::new(70, 32), App::new(snapshot, None, 0)).unwrap(),
    };
    assert_eq!(
        other.lines(),
        screen.lines(),
        "a newly attached terminal sees the same checklist"
    );
    screen.plan("p2", json!([{"step":"Report","status":"completed"}]));
    assert!(screen.pinned().is_empty());
    let text = screen.lines().join("\n");
    assert!(text.contains("Updated Plan · 1/3 complete"));
    assert!(text.contains("Updated Plan · 1/1 complete"));
}

#[test]
fn pinned_plan_uses_at_most_five_rows_with_active_first_and_overflow_count() {
    let mut screen = Screen::new();
    let mut plan: Vec<Value> = (1..=7)
        .map(|n| json!({"step":format!("Step {n}"),"status":"pending"}))
        .collect();
    plan[6]["status"] = json!("inProgress");
    screen.plan("p1", json!(plan));
    assert_eq!(
        screen.pinned(),
        [
            "  □ Step 7",
            "  □ Step 1",
            "  □ Step 2",
            "  □ Step 3",
            "  … and 3 more"
        ]
    );
    screen.ui.backend_mut().resize(20, 12);
    screen.plan(
        "p2",
        json!([{"step":"这是一条很长很长需要显示但不能挤掉输入框的步骤","status":"inProgress"}]),
    );
    let lines = screen.lines();
    assert!(
        lines.iter().any(|line| line.starts_with("│ › ")),
        "{lines:#?}"
    );
    let input = lines.iter().position(|line| line.starts_with('╭')).unwrap();
    assert!(lines[input - 1].starts_with("  □ "), "{lines:#?}");
}

#[test]
fn a_single_plan_row_keeps_the_active_step_and_overflow_count() {
    let mut screen = Screen::new();
    screen.ui.backend_mut().resize(40, 9);
    screen.plan("p1", json!([
        {"step":"Later","status":"pending"},
        {"step":"Implement the very long active step without hiding the count","status":"inProgress"},
        {"step":"Verify","status":"pending"}
    ]));
    let pinned = screen.pinned();
    assert_eq!(pinned.len(), 1, "{pinned:#?}");
    assert!(pinned[0].starts_with("  □ Implement"), "{pinned:#?}");
    assert!(pinned[0].ends_with(" (+2 more)"), "{pinned:#?}");
}

#[test]
fn a_tall_approval_with_a_plan_leaves_room_for_conversation_and_scroll_hint() {
    let mut screen = Screen::new();
    screen.ui.backend_mut().resize(60, 20);
    screen.plan("p1", steps());
    let command = (0..61)
        .map(|n| format!("echo {n}"))
        .collect::<Vec<_>>()
        .join("\n");
    screen.event(json!({"type":"request.opened","request":{
        "id":"r1","turnId":"t1","createdAt":0,"kind":{
            "type":"approval","itemId":"shell1","tool":"shell",
            "arguments":{"command":command},"availableDecisions":["accept","decline","cancel"]
        }
    }}));
    screen.event(json!({"type":"session.status","status":"waiting_approval"}));
    screen.lines();
    screen.ui.app_mut().handle_mouse_at(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: crossterm::event::KeyModifiers::NONE,
        },
        0,
    );
    let lines = screen.lines();
    let hint = lines
        .iter()
        .position(|line| line.contains("Jump to bottom"))
        .expect("scroll hint");
    assert!(
        hint > 0,
        "conversation must keep at least one row: {lines:#?}"
    );
    assert!(!lines[0].trim().is_empty(), "{lines:#?}");
}

#[test]
fn export_preserves_each_plan_and_disabling_hides_cards_and_pinned_steps() {
    let mut screen = Screen::new();
    screen.plan("p1", steps());
    screen.lines(); // Exercise the cached history when the switch changes.
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("export.md");
    screen
        .ui
        .app_mut()
        .export(Export::File(Some(path.to_string_lossy().into_owned())), 0);
    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("Updated Plan · 1/3 complete"), "{text}");
    assert!(text.contains("- [x] ~~Read~~"), "{text}");
    assert!(text.contains("- [ ] **Implement**"), "{text}");
    assert!(text.contains("- [ ] Verify"), "{text}");
    screen.ui.backend_mut().resize(70, 12);
    screen.lines();
    screen.ui.app_mut().handle_key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::PageUp,
            crossterm::event::KeyModifiers::NONE,
        ),
        0,
    );
    screen.lines();
    let mut settings = serde_json::to_value(&screen.ui.app().snapshot().settings).unwrap();
    settings["planEnabled"] = json!(false);
    screen.event(json!({"type":"session.updated","settings":settings}));
    assert!(screen.pinned().is_empty());
    let text = screen.lines().join("\n");
    assert!(!text.contains("Updated Plan"), "{text}");
    assert!(!text.contains("Implement"), "{text}");
}

#[test]
fn proposals_fold_expand_replay_and_export_the_full_plan_and_decision() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for (decision, label) in [
        ("implement", "Plan approved"),
        ("clearContext", "Plan approved · clear context"),
        ("keepPlanning", "Keep planning"),
    ] {
        let mut screen = Screen::new();
        let plan = (0..14)
            .map(|n| format!("- Implementation step {n}\n"))
            .collect::<String>();
        screen.event(json!({"type":"item.completed","turnId":"t1","item":{
            "type":"planProposal","id":"proposal","plan":plan,"status":"completed",
            "answer":{"decision":decision,"approvalPolicy":"askForExecution","feedback":"User feedback"}
        }}));
        let folded = screen.lines();
        assert!(folded.join("\n").contains(label));
        assert!(!folded.join("\n").contains("Implementation step 13"));
        assert!(folded.join("\n").contains("Ctrl+O"));
        let saved = serde_json::to_value(screen.ui.app().snapshot()).unwrap();
        let mut resumed = Screen {
            ui: Ui::new(
                TestBackend::new(70, 32),
                App::new(serde_json::from_value(saved).unwrap(), None, 0),
            )
            .unwrap(),
        };
        assert_eq!(resumed.lines(), folded);
        resumed.ui.app_mut().handle_key(
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
            1000,
        );
        assert!(
            resumed
                .lines()
                .join("\n")
                .contains("Implementation step 13")
        );
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("plan.md");
        resumed.ui.app_mut().export(
            Export::File(Some(path.to_string_lossy().into_owned())),
            1000,
        );
        let exported = std::fs::read_to_string(path).unwrap();
        assert!(exported.contains(&plan));
        assert!(exported.contains(label));
        assert!(exported.contains("User feedback"));
    }
}

#[test]
fn plan_blocked_tool_cards_are_marked_in_the_conversation() {
    let mut screen = Screen::new();
    for item in [
        json!({"type":"fileChange","id":"w","changes":[{"path":"file.rs","kind":{"type":"unknown"},"diff":""}],"status":"blocked"}),
        json!({"type":"commandExecution","id":"s","command":"touch file","cwd":"/work","aggregatedOutput":"","exitCode":null,"durationMs":null,"status":"blocked"}),
        json!({"type":"mcpToolCall","id":"m","server":"remote","tool":"echo","arguments":{},"result":"","status":"blocked"}),
        json!({"type":"plan","id":"p","plan":[],"status":"blocked"}),
    ] {
        screen.event(json!({"type":"item.completed","turnId":"t1","item":item}));
    }
    let text = screen.lines().join("\n");
    assert_eq!(text.matches("blocked (Plan mode)").count(), 4, "{text}");
}
