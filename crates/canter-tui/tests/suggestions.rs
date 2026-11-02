use canter_protocol::{Event, SessionSnapshot, SessionStatus};
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use serde_json::json;

fn screen() -> Ui<TestBackend> {
    let mut snapshot = SessionSnapshot::empty("suggestions");
    snapshot.status = SessionStatus::Idle;
    Ui::new(TestBackend::new(100, 24), App::new(snapshot, None, 0)).unwrap()
}
fn suggest(ui: &mut Ui<TestBackend>, turn: Option<&str>) {
    let event: Event = serde_json::from_value(json!({"seq":0,"timestamp":0,"sessionId":"suggestions","type":"session.promptSuggestion","turnId":turn,"text":"add parser tests"})).unwrap();
    ui.app_mut().apply_event(&event, 1000);
    ui.draw(1000).unwrap();
}
fn key(ui: &mut Ui<TestBackend>, code: KeyCode) -> Option<Action> {
    let action = ui
        .app_mut()
        .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 1000);
    ui.draw(1000).unwrap();
    action
}
fn text(ui: &mut Ui<TestBackend>) -> String {
    ui.draw(1000).unwrap();
    ui.backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn gray_suggestion_requires_tab_or_right_before_enter_and_can_be_edited() {
    for accept in [KeyCode::Tab, KeyCode::Right] {
        let mut ui = screen();
        suggest(&mut ui, None);
        assert!(text(&mut ui).contains("add parser tests"));
        assert!(
            ui.backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "a"
                    && cell.modifier.contains(ratatui::style::Modifier::DIM))
        );
        assert_eq!(key(&mut ui, KeyCode::Enter), None);
        assert_eq!(key(&mut ui, accept), None);
        key(&mut ui, KeyCode::Char('!'));
        assert_eq!(
            key(&mut ui, KeyCode::Enter),
            Some(Action::Send("add parser tests!".into()))
        );
    }
}

#[test]
fn typing_paste_and_escape_discard_suggestions_including_late_arrivals() {
    for action in ["type", "paste", "escape"] {
        let mut ui = screen();
        suggest(&mut ui, None);
        match action {
            "type" => {
                key(&mut ui, KeyCode::Char('x'));
                key(&mut ui, KeyCode::Backspace);
            }
            "paste" => {
                ui.app_mut().handle_paste("x");
                key(&mut ui, KeyCode::Backspace);
            }
            _ => {
                key(&mut ui, KeyCode::Esc);
            }
        }
        suggest(&mut ui, None);
        assert!(!text(&mut ui).contains("add parser tests"), "{action}");
        assert_eq!(key(&mut ui, KeyCode::Tab), None);
        assert_eq!(key(&mut ui, KeyCode::Enter), None);
    }
}

#[test]
fn nonempty_input_keeps_arrow_movement_and_slash_completion() {
    let mut ui = screen();
    ui.app_mut().handle_paste("ab");
    suggest(&mut ui, None);
    assert!(!text(&mut ui).contains("add parser tests"));
    key(&mut ui, KeyCode::Left);
    key(&mut ui, KeyCode::Right);
    key(&mut ui, KeyCode::Char('c'));
    assert_eq!(
        key(&mut ui, KeyCode::Enter),
        Some(Action::Send("abc".into()))
    );
    let mut ui = screen();
    suggest(&mut ui, None);
    ui.app_mut().handle_paste("/comp");
    ui.draw(1000).unwrap();
    assert_eq!(key(&mut ui, KeyCode::Tab), None);
    assert!(!text(&mut ui).contains("add parser tests"));
    assert!(text(&mut ui).contains("/compact"));
}

#[test]
fn new_turn_clears_old_suggestions_and_rejects_stale_turn_ids() {
    let mut ui = screen();
    suggest(&mut ui, None);
    let turn = json!({"id":"new","input":[],"items":[],"status":"inProgress","clientName":"test","error":null,"usage":null});
    for (seq, payload) in [
        json!({"type":"turn.started","turn":turn}),
        json!({"type":"session.status","status":"working"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut event = json!({"seq":seq+1,"timestamp":0,"sessionId":"suggestions"});
        event
            .as_object_mut()
            .unwrap()
            .extend(payload.as_object().unwrap().clone());
        ui.app_mut()
            .apply_event(&serde_json::from_value(event).unwrap(), 1000);
    }
    suggest(&mut ui, None);
    assert!(!text(&mut ui).contains("add parser tests"));
    ui.app_mut().handle_paste("steer this");
    ui.draw(1000).unwrap();
    assert!(matches!(
        key(&mut ui, KeyCode::Tab),
        Some(Action::Steer { .. })
    ));

    let mut snapshot = SessionSnapshot::empty("suggestions");
    snapshot.status = SessionStatus::Idle;
    snapshot.turns.push(serde_json::from_value(json!({"id":"latest","input":[],"items":[],"status":"completed","clientName":"test","error":null,"usage":null})).unwrap());
    let mut ui = Ui::new(TestBackend::new(100, 24), App::new(snapshot, None, 0)).unwrap();
    suggest(&mut ui, Some("older"));
    assert!(!text(&mut ui).contains("add parser tests"));
    suggest(&mut ui, Some("latest"));
    assert!(text(&mut ui).contains("add parser tests"));
}

#[test]
fn question_dialog_hides_suggestions_and_reduced_notice_is_english() {
    let mut ui = screen();
    let event = serde_json::from_value(json!({"seq":1,"timestamp":0,"sessionId":"suggestions","type":"request.opened","request":{"id":"question","turnId":"turn","createdAt":0,"kind":{"type":"userInput","itemId":"ask","question":"Continue?","options":[{"label":"yes","description":"Proceed"},{"label":"no","description":"Stop"}]}}})).unwrap();
    ui.app_mut().apply_event(&event, 1000);
    suggest(&mut ui, None);
    assert!(!text(&mut ui).contains("add parser tests"));
    let event = serde_json::from_value(json!({"seq":0,"timestamp":0,"sessionId":"suggestions","type":"session.promptSuggestionsReduced"})).unwrap();
    ui.app_mut().apply_event(&event, 1000);
    assert!(text(&mut ui).contains("Showing fewer prompt suggestions"));
}
