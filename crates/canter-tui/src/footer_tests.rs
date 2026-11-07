//! The sub-agents row, the background commands row and the goal row under
//! the info rows (P2-240, P2-243): ↓ from the input box selects the first
//! two, Enter opens their list under the info rows, the input box stays,
//! grey, and Esc closes it.

use super::*;
use crate::Ui;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use serde_json::json;

const BACKGROUND: &str = "▸ 1 background commands";
const AGENTS: &str = "● 1 sub-agent running · 2 done";
const WIDTH: u16 = 100;

/// A row under the info rows as [`footer`] gives it: `text`, then `hint`
/// on the right, a column before the edge.
fn row(text: &str, hint: &str) -> String {
    let pad = usize::from(WIDTH) - 2 - text.chars().count() - hint.chars().count();
    format!("{text}{}{hint}", " ".repeat(pad))
}

/// The row as it shows when not selected.
fn shown(text: &str) -> String {
    row(text, "↓ view")
}

/// The row as it shows while ↓ has it selected.
fn selected(text: &str) -> String {
    row(text, "Enter open")
}

/// Both rows, neither selected.
fn both() -> [String; 2] {
    [shown(AGENTS), shown(BACKGROUND)]
}

fn agent(id: &str, status: &str) -> AgentSummary {
    serde_json::from_value(json!({
        "id": id, "task": "Inspect", "status": status, "result": null,
        "createdAt": 0, "updatedAt": 0, "name": format!("agent-{id}"), "agentType": "explorer",
    }))
    .unwrap()
}

fn agents() -> Vec<AgentSummary> {
    vec![
        agent("a", "running"),
        agent("b", "completed"),
        agent("c", "completed"),
    ]
}

fn command() -> serde_json::Value {
    json!({"id":1,"itemId":"c","command":"npm run dev","startedAt":0,"completedAt":null,"running":true,"exitCode":null})
}

/// A screen with a running background command and three sub-agents.
fn screen() -> Ui<TestBackend> {
    let mut snapshot = SessionSnapshot::empty("s1");
    snapshot.status = SessionStatus::Idle;
    snapshot
        .background_commands
        .push(serde_json::from_value(command()).unwrap());
    let mut app = App::new(snapshot, None, 0);
    app.set_agents(agents());
    let mut ui = Ui::new(TestBackend::new(WIDTH, 30), app).unwrap();
    ui.draw(0).unwrap();
    ui
}

fn rows(ui: &Ui<TestBackend>) -> Vec<String> {
    let buffer = ui.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// The input box's top and bottom rows: the first box on screen.
fn input_box(ui: &Ui<TestBackend>) -> (usize, usize) {
    let rows = rows(ui);
    let top = rows.iter().position(|row| row.starts_with('╭')).unwrap();
    let bottom = top
        + rows[top..]
            .iter()
            .position(|row| row.starts_with('╰'))
            .unwrap();
    (top, bottom)
}

fn press(ui: &mut Ui<TestBackend>, code: KeyCode) -> Option<Action> {
    press_with(ui, code, KeyModifiers::NONE)
}

fn press_with(ui: &mut Ui<TestBackend>, code: KeyCode, modifiers: KeyModifiers) -> Option<Action> {
    let action = ui
        .app_mut()
        .handle_key(KeyEvent::new(code, modifiers), 10_000);
    ui.draw(0).unwrap();
    action
}

/// The rows under the two info rows.
fn footer(ui: &Ui<TestBackend>) -> Vec<String> {
    let (_, bottom) = input_box(ui);
    rows(ui)[bottom + 1 + INFO_ROWS as usize..]
        .iter()
        .map(|row| row.trim().to_owned())
        .collect()
}

fn reversed(ui: &Ui<TestBackend>, text: &str) -> bool {
    let rows = rows(ui);
    let y = rows.iter().position(|row| row.contains(text)).unwrap();
    let x = crate::text::str_width(&rows[y][..rows[y].find(text).unwrap()]);
    ui.backend().buffer()[(x as u16, y as u16)]
        .modifier
        .contains(Modifier::REVERSED)
}

#[test]
fn the_rows_sit_under_the_info_rows_and_not_above_the_input_box() {
    let ui = screen();
    let rows = rows(&ui);
    let shown = rows.join("\n");
    let (top, _) = input_box(&ui);
    assert_eq!(footer(&ui), both(), "{shown}");
    for row in &rows[..top] {
        assert!(!row.contains("background commands"), "{shown}");
        assert!(!row.contains("sub-agent"), "{shown}");
    }
    assert!(!shown.contains("click or /agents"), "{shown}");
    // The hint is dim, a column before the edge.
    let y = rows.iter().position(|row| row.contains(AGENTS)).unwrap();
    let x = rows[y].chars().count() - "↓ view".chars().count();
    assert_eq!(x, usize::from(WIDTH) - 1 - "↓ view".chars().count());
    assert!(
        ui.backend().buffer()[(x as u16, y as u16)]
            .modifier
            .contains(Modifier::DIM)
    );
}

#[test]
fn without_sub_agents_commands_or_a_goal_there_are_no_rows() {
    let mut app = App::new(SessionSnapshot::empty("s1"), None, 0);
    app.snapshot.status = SessionStatus::Idle;
    let mut ui = Ui::new(TestBackend::new(WIDTH, 30), app).unwrap();
    ui.draw(0).unwrap();
    let rows = rows(&ui);
    let (_, bottom) = input_box(&ui);
    assert_eq!(rows.len(), bottom + 1 + usize::from(INFO_ROWS), "{rows:#?}");
}

#[test]
fn down_selects_the_rows_and_up_goes_back_to_the_input_box() {
    let mut ui = screen();
    assert_eq!(press(&mut ui, KeyCode::Down), None);
    assert_eq!(
        footer(&ui),
        [selected(AGENTS), shown(BACKGROUND)],
        "{:?}",
        footer(&ui)
    );
    assert!(reversed(&ui, "1 sub-agent"));
    assert!(!reversed(&ui, "Enter open"));

    assert_eq!(press(&mut ui, KeyCode::Down), None);
    assert_eq!(footer(&ui), [shown(AGENTS), selected(BACKGROUND)]);
    assert!(reversed(&ui, "1 background"));
    assert!(!reversed(&ui, "1 sub-agent"));

    assert_eq!(press(&mut ui, KeyCode::Up), None);
    assert!(reversed(&ui, "1 sub-agent"));
    assert_eq!(press(&mut ui, KeyCode::Up), None);
    assert_eq!(footer(&ui), both());
    assert!(!reversed(&ui, "1 sub-agent"));

    // Enter opens the row selected; ↓ on the last row too.
    press(&mut ui, KeyCode::Down);
    assert_eq!(press(&mut ui, KeyCode::Enter), Some(Action::AgentList));
    assert_eq!(footer(&ui), both());
    press(&mut ui, KeyCode::Down);
    press(&mut ui, KeyCode::Down);
    assert_eq!(press(&mut ui, KeyCode::Down), Some(Action::BackgroundList));
    press(&mut ui, KeyCode::Down);
    press(&mut ui, KeyCode::Down);
    assert_eq!(press(&mut ui, KeyCode::Enter), Some(Action::BackgroundList));
}

#[test]
fn typing_or_esc_leaves_the_row() {
    let mut ui = screen();
    press(&mut ui, KeyCode::Down);
    assert_eq!(press(&mut ui, KeyCode::Char('h')), None);
    assert_eq!(ui.app().input.text(), "h");
    assert_eq!(footer(&ui), both());

    press(&mut ui, KeyCode::Backspace);
    press(&mut ui, KeyCode::Down);
    assert!(reversed(&ui, "1 sub-agent"));
    assert_eq!(press(&mut ui, KeyCode::Esc), None);
    assert_eq!(footer(&ui), both());
}

#[test]
fn down_keeps_moving_the_cursor_going_through_history_and_the_lists() {
    let mut ui = screen();
    // A cursor above the last row goes down first.
    press(&mut ui, KeyCode::Char('a'));
    press_with(&mut ui, KeyCode::Char('j'), KeyModifiers::CONTROL);
    press(&mut ui, KeyCode::Char('b'));
    press(&mut ui, KeyCode::Up);
    assert_eq!(ui.app().input.cursor(), 1);
    press(&mut ui, KeyCode::Down);
    assert_eq!(ui.app().input.cursor(), 3);
    assert_eq!(footer(&ui), both());
    press(&mut ui, KeyCode::Down);
    assert!(reversed(&ui, "1 sub-agent"));
    press(&mut ui, KeyCode::Up);

    // A message recalled goes back to the newest first.
    ui.app_mut().input.clear();
    for c in "first".chars() {
        press(&mut ui, KeyCode::Char(c));
    }
    ui.app_mut().record_input(None);
    ui.app_mut().input.clear();
    ui.draw(0).unwrap();
    press(&mut ui, KeyCode::Up);
    assert_eq!(ui.app().input.text(), "first");
    press(&mut ui, KeyCode::Down);
    assert_eq!(ui.app().input.text(), "");
    assert_eq!(footer(&ui), both());
    press(&mut ui, KeyCode::Down);
    assert!(reversed(&ui, "1 sub-agent"));
    press(&mut ui, KeyCode::Up);

    // The command list takes ↓.
    press(&mut ui, KeyCode::Char('/'));
    assert!(ui.app().command_list().is_some());
    press(&mut ui, KeyCode::Down);
    assert_eq!(ui.app().command_list().unwrap().1, 1);
    assert_eq!(footer(&ui), both());
}

/// The input box is grey: its border, its prompt (cyan otherwise) and its
/// text dim.
fn grey(ui: &Ui<TestBackend>) -> bool {
    let (top, _) = input_box(ui);
    let buffer = ui.backend().buffer();
    let cell = |x: u16, y: usize| &buffer[(x, y as u16)];
    let dim = |x: u16, y: usize| cell(x, y).modifier.contains(Modifier::DIM);
    dim(0, top)
        && dim(2, top + 1)
        && cell(2, top + 1).fg != ratatui::style::Color::Cyan
        && (cell(4, top + 1).symbol() == " " || dim(4, top + 1))
}

#[test]
fn the_sub_agents_list_opens_under_the_info_rows_and_esc_keeps_the_draft() {
    let mut ui = screen();
    assert!(!grey(&ui));
    press(&mut ui, KeyCode::Char('h'));
    press(&mut ui, KeyCode::Char('i'));
    press(&mut ui, KeyCode::Down);
    assert_eq!(press(&mut ui, KeyCode::Enter), Some(Action::AgentList));
    // What the main loop does with it.
    ui.app_mut().show_agents(agents());
    ui.draw(0).unwrap();
    let shown_rows = rows(&ui).join("\n");
    let (top, _) = input_box(&ui);
    assert!(rows(&ui)[top + 1].contains("› hi"), "{shown_rows}");
    assert!(grey(&ui), "{shown_rows}");
    let below = footer(&ui);
    assert_eq!(below[0], shown(BACKGROUND), "{shown_rows}");
    assert!(
        below[1].starts_with("╭─ Sub-agents · 1 running · 2 done ─"),
        "{shown_rows}"
    );
    assert!(
        below.iter().any(|row| row.contains("├ ● agent-a")),
        "{shown_rows}"
    );
    assert!(
        below
            .last()
            .unwrap()
            .contains("↑↓ select · Enter open · s stop · Esc close"),
        "{shown_rows}"
    );
    // Keys go to the list.
    press(&mut ui, KeyCode::Char('x'));
    assert_eq!(ui.app().input.text(), "hi");

    assert_eq!(press(&mut ui, KeyCode::Esc), None);
    assert_eq!(ui.app().input.text(), "hi");
    assert!(!grey(&ui));
    assert_eq!(footer(&ui), both());
}

#[test]
fn the_ps_list_opens_under_the_info_rows() {
    let mut ui = screen();
    press(&mut ui, KeyCode::Down);
    press(&mut ui, KeyCode::Down);
    assert_eq!(press(&mut ui, KeyCode::Enter), Some(Action::BackgroundList));
    let list = serde_json::from_value(json!({ "data": [command()] })).unwrap();
    ui.app_mut().show_background(&list, 5000);
    ui.draw(0).unwrap();
    let shown_rows = rows(&ui).join("\n");
    assert!(grey(&ui), "{shown_rows}");
    let below = footer(&ui);
    assert_eq!(below[0], shown(AGENTS), "{shown_rows}");
    assert_eq!(below[1], shown("▾ 1 background commands"), "{shown_rows}");
    assert!(
        below[2].starts_with("╭─ Background commands"),
        "{shown_rows}"
    );
    assert!(
        below.iter().any(|row| row.contains("#1 npm run dev")),
        "{shown_rows}"
    );

    // The command's log opens there too.
    assert_eq!(
        press(&mut ui, KeyCode::Enter),
        Some(Action::BackgroundOutput(1))
    );
    let output = serde_json::from_value(
        json!({"command": command(), "output": "server ready", "skippedBytes": 0}),
    )
    .unwrap();
    ui.app_mut().show_background_output(&output);
    ui.draw(0).unwrap();
    let shown_rows = rows(&ui).join("\n");
    let below = footer(&ui);
    assert!(below[2].starts_with("╭─ Background #1"), "{shown_rows}");
    assert!(
        below.iter().any(|row| row.contains("server ready")),
        "{shown_rows}"
    );
    assert!(grey(&ui), "{shown_rows}");
    assert_eq!(press(&mut ui, KeyCode::Esc), None);
    assert!(!grey(&ui));
    assert_eq!(footer(&ui), both());
}

#[test]
fn other_panels_still_take_the_input_box_place() {
    let mut ui = screen();
    ui.app_mut().show_sandbox(&Default::default());
    ui.draw(0).unwrap();
    let shown_rows = rows(&ui).join("\n");
    let (top, _) = input_box(&ui);
    assert!(rows(&ui)[top].starts_with("╭─ Sandbox"), "{shown_rows}");
    assert_eq!(footer(&ui), both(), "{shown_rows}");
}

fn goal(status: &str, budget: f64) -> canter_protocol::SessionGoal {
    serde_json::from_value(json!({
        "id": "g1", "objective": "Finish", "status": status, "budgetUsd": budget,
        "cost": {"usd": 0.12, "unknown": false}, "createdAt": 0, "updatedAt": 0,
    }))
    .unwrap()
}

/// The goal has a row of its own, last; ↓ does not select it, and the
/// first info row no longer starts with it.
#[test]
fn the_goal_is_a_row_of_its_own_that_down_skips() {
    let mut ui = screen();
    ui.app_mut().snapshot.goal = Some(goal("active", 5.0));
    ui.draw(0).unwrap();
    let all = rows(&ui);
    let shown_rows = all.join("\n");
    assert_eq!(
        footer(&ui),
        [
            shown(AGENTS),
            shown(BACKGROUND),
            "◎ goal $0.12 / $5.00".to_owned()
        ],
        "{shown_rows}"
    );
    let (_, bottom) = input_box(&ui);
    assert!(!all[bottom + 1].contains("goal"), "{shown_rows}");
    let y = all.iter().position(|row| row.contains("◎ goal")).unwrap();
    let buffer = ui.backend().buffer();
    assert!(buffer[(1, y as u16)].modifier.contains(Modifier::DIM));
    assert!(!buffer[(3, y as u16)].modifier.contains(Modifier::DIM));

    press(&mut ui, KeyCode::Down);
    press(&mut ui, KeyCode::Down);
    assert!(reversed(&ui, "1 background"));
    assert!(!reversed(&ui, "◎ goal"));
    assert_eq!(press(&mut ui, KeyCode::Down), Some(Action::BackgroundList));

    // Not active: its state after `goal`; no budget: unlimited.
    ui.app_mut().snapshot.goal = Some(goal("paused", 0.0));
    ui.draw(0).unwrap();
    assert_eq!(footer(&ui)[2], "◎ goal paused $0.12 / unlimited");
}
