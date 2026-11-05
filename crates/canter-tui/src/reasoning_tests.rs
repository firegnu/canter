use crate::{Action, App, Ui};
use canter_protocol::{Event, EventPayload, ReasoningEffort, SessionSnapshot};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use serde_json::json;

fn app() -> App {
    let mut snapshot = SessionSnapshot::empty("reasoning");
    snapshot.settings.model = "configured/solver".into();
    snapshot.settings.reasoning_effort = ReasoningEffort::Medium;
    let mut app = App::new(snapshot, None, 0);
    app.set_models(&serde_json::from_value::<Vec<_>>(json!([{
        "id":"configured/solver","model":"configured/solver","displayName":"solver","isDefault":true,
        "supportedReasoningEfforts":["low","medium","high","xhigh","max"],
        "defaultReasoningEffort":"medium"
    }])).unwrap());
    app
}

fn enter(app: &mut App, command: &str) -> Option<Action> {
    for key in command.chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE), 0);
    }
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0)
}

fn screen(ui: &mut Ui<TestBackend>) -> String {
    ui.draw(0).unwrap();
    let buffer = ui.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn think_chooser_uses_current_model_efforts_and_selects_the_effective_value() {
    let mut ui = Ui::new(TestBackend::new(100, 30), app()).unwrap();
    assert!(enter(ui.app_mut(), "/think").is_none());
    let shown = screen(&mut ui);
    assert!(shown.contains("medium (current)"), "{shown}");
    assert!(shown.contains("xhigh"), "{shown}");
    assert!(!shown.contains("none"), "{shown}");
    assert!(
        matches!(enter(ui.app_mut(), ""), Some(Action::Update(params)) if params.reasoning_effort == Some(ReasoningEffort::Medium))
    );
}

#[test]
fn think_accepts_supported_new_efforts_and_explains_model_specific_options() {
    assert!(
        matches!(enter(&mut app(), "/think xhigh"), Some(Action::Update(params)) if params.reasoning_effort == Some(ReasoningEffort::Xhigh))
    );
    let mut ui = Ui::new(TestBackend::new(120, 30), app()).unwrap();
    assert!(enter(ui.app_mut(), "/think none").is_none());
    let shown = screen(&mut ui);
    assert!(
        shown.contains("use low, medium, high, xhigh, max"),
        "{shown}"
    );
}

#[test]
fn status_and_switch_notice_display_server_confirmed_reasoning() {
    let mut ui = Ui::new(TestBackend::new(100, 30), app()).unwrap();
    assert!(screen(&mut ui).contains("think medium"));
    let mut settings = ui.app().snapshot().settings.clone();
    settings.reasoning_effort = ReasoningEffort::Xhigh;
    ui.app_mut().apply_event(
        &Event {
            seq: 1,
            session_id: "reasoning".into(),
            timestamp: 0,
            payload: EventPayload::SessionUpdated { settings },
        },
        0,
    );
    assert!(screen(&mut ui).contains("think xhigh"));
    let mut settings = ui.app().snapshot().settings.clone();
    settings.model = "legacy".into();
    settings.reasoning_effort = ReasoningEffort::High;
    ui.app_mut().apply_event(
        &Event {
            seq: 2,
            session_id: "reasoning".into(),
            timestamp: 0,
            payload: EventPayload::SessionUpdated { settings },
        },
        0,
    );
    ui.app_mut().apply_event(
        &Event {
            seq: 3,
            session_id: "reasoning".into(),
            timestamp: 0,
            payload: EventPayload::Warning {
                message: "Thinking effort changed from xhigh to high for legacy.".into(),
            },
        },
        0,
    );
    let shown = screen(&mut ui);
    assert!(shown.contains("think high"), "{shown}");
    assert!(
        shown.contains("Thinking effort changed from xhigh to high for legacy."),
        "{shown}"
    );
    enter(ui.app_mut(), "/think");
    let shown = screen(&mut ui);
    assert!(
        shown.contains("none") && shown.contains("high (current)"),
        "{shown}"
    );
}

#[test]
fn unverified_model_offers_only_medium_and_explains_the_fallback() {
    let mut model_app = app();
    model_app.set_models(&serde_json::from_value::<Vec<_>>(json!([{
        "id":"configured/solver","model":"configured/solver","displayName":"Solver","isDefault":true,
        "supportedReasoningEfforts":["medium"],"defaultReasoningEffort":"medium",
        "verified":false,"missingCapabilities":["reasoningEffort"]
    }])).unwrap());
    let mut ui = Ui::new(TestBackend::new(120, 30), model_app).unwrap();
    assert!(enter(ui.app_mut(), "/think").is_none());
    let shown = screen(&mut ui);
    assert!(shown.contains("medium (current)"), "{shown}");
    assert!(shown.contains("unverified for this model"), "{shown}");
    assert!(!shown.contains("high"), "{shown}");
    let mut command = app();
    command.set_models(ui.app().models());
    assert!(enter(&mut command, "/think high").is_none());
}
