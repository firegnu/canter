use std::collections::BTreeMap;

use canter_protocol::SessionSnapshot;
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

fn ui() -> (tempfile::TempDir, Ui<TestBackend>) {
    let home = tempfile::tempdir().unwrap();
    let entries = [
        ("/work", "review src\nplease", 1),
        ("/work", "review only", 2),
        ("/work", "SRC review newest", 3),
        ("/elsewhere", "review src elsewhere", 4),
        ("/work", "review src\nplease", 5),
    ];
    let data = entries
        .map(|(cwd, text, ts)| {
            serde_json::json!({"cwd":cwd,"text":text,"ts":ts}).to_string() + "\n"
        })
        .concat();
    std::fs::write(home.path().join("input_history.jsonl"), data).unwrap();
    let env = BTreeMap::from([
        ("HOME".into(), home.path().display().to_string()),
        ("CANTER_HOME".into(), home.path().display().to_string()),
    ]);
    let mut snapshot = SessionSnapshot::empty("history-search");
    snapshot.settings.cwd = "/work".into();
    snapshot.settings.model = "fake-model".into();
    let mut ui = Ui::new(
        TestBackend::new(120, 30),
        App::new(snapshot, None, 0).with_input_history(&env),
    )
    .unwrap();
    ui.draw(0).unwrap();
    (home, ui)
}

fn key(ui: &mut Ui<TestBackend>, code: KeyCode) -> Option<Action> {
    let action = ui.app_mut().handle_key(KeyEvent::from(code), 0);
    ui.draw(0).unwrap();
    action
}

fn ctrl_r(ui: &mut Ui<TestBackend>) {
    assert_eq!(
        ui.app_mut()
            .handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL), 0),
        None
    );
    ui.draw(0).unwrap();
}

fn screen(ui: &Ui<TestBackend>) -> String {
    ui.backend()
        .buffer()
        .content
        .chunks(120)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ctrl_r_opens_history_above_the_input_and_cycles_newest_first() {
    let (_home, mut ui) = ui();
    ctrl_r(&mut ui);
    let shown = screen(&ui);
    assert!(
        shown.contains("Search history")
            && shown.contains("this directory")
            && shown.contains("3 matches"),
        "{shown}"
    );
    assert!(
        shown.contains("review src please") && shown.contains("search: "),
        "{shown}"
    );
    ctrl_r(&mut ui);
    assert_eq!(key(&mut ui, KeyCode::Enter), None);
    assert_eq!(
        key(&mut ui, KeyCode::Enter),
        Some(Action::Send("SRC review newest".into()))
    );
}

#[test]
fn history_filter_requires_every_word_without_case_or_order_and_shows_no_matches() {
    let (_home, mut ui) = ui();
    ctrl_r(&mut ui);
    ui.app_mut().handle_paste("sRc REVIEW");
    ui.draw(0).unwrap();
    let shown = screen(&ui);
    assert!(
        shown.contains("2 matches") && !shown.contains("review only"),
        "{shown}"
    );
    key(&mut ui, KeyCode::Char('z'));
    let shown = screen(&ui);
    assert!(shown.contains("No matching history"), "{shown}");
}

#[test]
fn tab_searches_all_directories_and_enter_only_puts_selection_in_box() {
    let (_home, mut ui) = ui();
    ui.app_mut().handle_paste("draft");
    ctrl_r(&mut ui);
    ui.app_mut().handle_paste("elsewhere");
    key(&mut ui, KeyCode::Tab);
    let shown = screen(&ui);
    assert!(
        shown.contains("all directories") && shown.contains("1 match"),
        "{shown}"
    );
    assert_eq!(key(&mut ui, KeyCode::Enter), None);
    assert!(!screen(&ui).contains("Search history"));
    key(&mut ui, KeyCode::Char('!'));
    assert_eq!(
        key(&mut ui, KeyCode::Enter),
        Some(Action::Send("review src elsewhere!".into()))
    );
}

#[test]
fn escape_restores_the_complete_draft_and_its_cursor() {
    let (_home, mut ui) = ui();
    ui.app_mut().handle_paste("draft\nline");
    key(&mut ui, KeyCode::Left);
    ctrl_r(&mut ui);
    ui.app_mut().handle_paste("review");
    key(&mut ui, KeyCode::Down);
    key(&mut ui, KeyCode::Esc);
    assert!(!screen(&ui).contains("Search history"));
    key(&mut ui, KeyCode::Char('|'));
    assert_eq!(
        key(&mut ui, KeyCode::Enter),
        Some(Action::Send("draft\nlin|e".into()))
    );
}

#[test]
fn ctrl_r_does_not_open_over_another_popup() {
    let (_home, mut ui) = ui();
    key(&mut ui, KeyCode::Char('?'));
    ctrl_r(&mut ui);
    assert!(!screen(&ui).contains("Search history"));
    key(&mut ui, KeyCode::Esc);
    key(&mut ui, KeyCode::Char('/'));
    ctrl_r(&mut ui);
    assert!(!screen(&ui).contains("Search history"));
    for kind in [
        serde_json::json!({"type":"approval","itemId":"tool","tool":"write","arguments":"{}","availableDecisions":["accept","cancel"]}),
        serde_json::json!({"type":"userInput","itemId":"tool","question":"Choose","options":[{"label":"A","description":"First"}]}),
    ] {
        let (_home, mut ui) = self::ui();
        for (seq, payload) in [
            serde_json::json!({"type":"turn.started","turn":{"id":"t","items":[],"status":"inProgress","error":null,"input":[],"clientName":"test","usage":null}}),
            serde_json::json!({"type":"request.opened","request":{"id":"pending","turnId":"t","createdAt":0,"kind":kind}}),
        ].into_iter().enumerate() {
            let mut event = serde_json::json!({"seq":seq + 1,"sessionId":"history-search","timestamp":0});
            event.as_object_mut().unwrap().extend(payload.as_object().unwrap().clone());
            ui.app_mut().apply_event(&serde_json::from_value(event).unwrap(), 0);
        }
        ui.draw(0).unwrap();
        let before = screen(&ui);
        ctrl_r(&mut ui);
        assert_eq!(screen(&ui), before);
        assert!(!screen(&ui).contains("Search history"));
    }
}
