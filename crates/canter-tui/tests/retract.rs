use canter_protocol::*;
use canter_tui::{Action, App, Ui};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use serde_json::json;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn text(value: &str) -> Vec<UserInput> {
    vec![UserInput::Text { text: value.into() }]
}
fn ui() -> Ui<TestBackend> {
    let mut snapshot = SessionSnapshot::empty("test");
    snapshot.settings.model = "deepseek-flash".into();
    Ui::new(TestBackend::new(90, 28), App::new(snapshot, None, 0)).unwrap()
}
fn event(ui: &mut Ui<TestBackend>, payload: EventPayload) {
    let event = Event {
        seq: ui.app().snapshot().last_event_seq + 1,
        session_id: "test".into(),
        timestamp: 0,
        payload,
    };
    ui.app_mut().apply_event(&event, 0);
    ui.draw(0).unwrap();
}
fn start(ui: &mut Ui<TestBackend>, input: Vec<UserInput>) {
    let turn = serde_json::from_value(json!({
        "id":"turn", "input":input, "items":[], "clientName":"test",
        "status":"inProgress", "error":null, "usage":null,
    }))
    .unwrap();
    event(
        ui,
        EventPayload::TurnStarted {
            turn,
            input_id: None,
        },
    );
    event(
        ui,
        EventPayload::ItemCompleted {
            turn_id: "turn".into(),
            item: Item::UserMessage {
                id: "user".into(),
                content: input,
            },
        },
    );
    event(
        ui,
        EventPayload::SessionStatus {
            status: SessionStatus::Working,
            close_reason: None,
        },
    );
}
fn queued(ui: &mut Ui<TestBackend>, id: &str, input: Vec<UserInput>) {
    event(
        ui,
        EventPayload::InputQueued {
            input: QueuedInput {
                id: id.into(),
                input,
                client_name: "test".into(),
                review: None,
            },
        },
    );
}
fn retract(ui: &mut Ui<TestBackend>) {
    event(
        ui,
        EventPayload::InputRemoved {
            input_ids: vec!["q1".into(), "q2".into()],
            reason: InputRemovalReason::Interrupted,
            turn_id: None,
        },
    );
    event(
        ui,
        EventPayload::TurnRetracted {
            turn_id: "turn".into(),
        },
    );
    event(
        ui,
        EventPayload::SessionStatus {
            status: SessionStatus::Idle,
            close_reason: None,
        },
    );
}
fn screen(ui: &Ui<TestBackend>) -> String {
    ui.backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn retract_restores_multiline_queued_inputs_before_the_draft_only_on_initiator() {
    for draft in ["", "original draft"] {
        let mut initiator = ui();
        let mut observer = ui();
        for ui in [&mut initiator, &mut observer] {
            start(ui, text("mistyped\nwhole message"));
            queued(ui, "q1", text("queued one\nsecond line"));
            queued(ui, "q2", text("queued two"));
        }
        initiator.app_mut().handle_paste(draft);
        assert_eq!(
            initiator.app_mut().handle_key(key(KeyCode::Esc), 0),
            Some(Action::Interrupt {
                turn_id: "turn".into()
            })
        );
        initiator.app_mut().interrupt_finished("turn", true);
        for ui in [&mut initiator, &mut observer] {
            retract(ui);
            assert!(ui.app().snapshot().turns.is_empty());
            assert!(!screen(ui).contains("Turn interrupted"));
        }
        let suffix = if draft.is_empty() {
            String::new()
        } else {
            format!("\n{draft}")
        };
        initiator.app_mut().handle_key(key(KeyCode::Char('!')), 0);
        assert_eq!(
            initiator.app_mut().handle_key(key(KeyCode::Enter), 0),
            Some(Action::Send(format!(
                "mistyped\nwhole message\nqueued one\nsecond line\nqueued two{suffix}!"
            )))
        );
        assert_eq!(observer.app_mut().handle_key(key(KeyCode::Enter), 0), None);
        assert!(!screen(&observer).contains("mistyped"));
    }
}

#[test]
fn retract_restores_original_image_paths_even_when_send_replies_arrive_last() {
    for replies_last in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (1..=3)
            .map(|n| {
                let path = temp.path().join(format!("picture{n}.png"));
                std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
                path.to_string_lossy().into_owned()
            })
            .collect();
        let image_input = |message: &str, path: &str| {
            vec![
                UserInput::Text {
                    text: message.into(),
                },
                UserInput::LocalImage {
                    path: path.into(),
                    name: Some("picture.png".into()),
                },
            ]
        };
        let original = image_input("first\nline", &paths[0]);
        let queue = image_input("queue one", &paths[1]);
        let mut ui = ui();
        ui.app_mut().input_sending();
        ui.app_mut().input_sending();
        ui.app_mut().input_sending();
        if !replies_last {
            ui.app_mut().input_sent(1, original.clone());
            ui.app_mut().input_sent(4, queue.clone());
            ui.app_mut().input_sent(5, text("queue two"));
        }
        start(
            &mut ui,
            image_input("first\nline", "image-server-first.png"),
        );
        queued(
            &mut ui,
            "q1",
            image_input("queue one", "image-server-queued.png"),
        );
        queued(&mut ui, "q2", text("queue two"));
        ui.app_mut().handle_paste("draft");
        ui.app_mut().handle_paste(&paths[2]);
        assert!(matches!(
            ui.app_mut().handle_key(key(KeyCode::Esc), 0),
            Some(Action::Interrupt { .. })
        ));
        retract(&mut ui);
        // Retraction events alone must never restore another client's input.
        ui.app_mut().interrupt_finished("turn", true);
        if replies_last {
            ui.app_mut().input_sent(5, text("queue two"));
            ui.app_mut().input_sent(4, queue.clone());
            ui.app_mut().input_sent(1, original.clone());
        }
        ui.draw(0).unwrap();
        assert!(!screen(&ui).contains("Turn interrupted"));
        let Some(Action::SendImages { input, .. }) =
            ui.app_mut().handle_key(key(KeyCode::Enter), 0)
        else {
            panic!("retracted attachments must remain real attachments");
        };
        let actual_paths: Vec<_> = input
            .iter()
            .filter_map(|part| match part {
                UserInput::LocalImage { path, .. } => Some(path.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(actual_paths, paths);
        let contents: String = input
            .iter()
            .filter_map(|part| match part {
                UserInput::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(contents, "first\nline\nqueue one\nqueue two\ndraft");
    }
}

#[test]
fn rejected_retract_keeps_the_interrupted_turn_and_original_draft() {
    let mut ui = ui();
    start(&mut ui, text("keep in conversation"));
    queued(&mut ui, "q1", text("ordinary discard"));
    event(
        &mut ui,
        EventPayload::ItemStarted {
            turn_id: "turn".into(),
            item: Item::AgentMessage {
                id: "answer".into(),
                text: "already speaking".into(),
            },
        },
    );
    ui.app_mut().handle_paste("original draft");
    ui.app_mut().handle_key(key(KeyCode::Esc), 0);
    ui.app_mut().interrupt_finished("turn", false);
    event(
        &mut ui,
        EventPayload::InputRemoved {
            input_ids: vec!["q1".into()],
            reason: InputRemovalReason::Interrupted,
            turn_id: None,
        },
    );
    let mut turn = ui.app().snapshot().turns[0].clone();
    turn.status = TurnStatus::Interrupted;
    event(&mut ui, EventPayload::TurnCompleted { turn });
    event(
        &mut ui,
        EventPayload::SessionStatus {
            status: SessionStatus::Idle,
            close_reason: None,
        },
    );
    assert!(screen(&ui).contains("keep in conversation"));
    assert!(screen(&ui).contains("Turn interrupted"));
    assert_eq!(
        ui.app_mut().handle_key(key(KeyCode::Enter), 0),
        Some(Action::Send("original draft".into()))
    );
}
