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

fn interrupted(ui: &mut Ui<TestBackend>, turn_id: &str) {
    event(
        ui,
        EventPayload::InputRemoved {
            input_ids: vec!["q1".into()],
            reason: InputRemovalReason::Interrupted,
            turn_id: None,
        },
    );
    let mut turn = ui
        .app()
        .snapshot()
        .turns
        .iter()
        .find(|turn| turn.id == turn_id)
        .unwrap()
        .clone();
    turn.status = TurnStatus::Interrupted;
    event(ui, EventPayload::TurnCompleted { turn });
    event(
        ui,
        EventPayload::SessionStatus {
            status: SessionStatus::Idle,
            close_reason: None,
        },
    );
}

#[test]
fn interrupt_after_the_model_spoke_restores_queued_inputs_but_keeps_the_turn() {
    for reply_first in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let picture = temp.path().join("picture.png");
        std::fs::write(&picture, b"\x89PNG\r\n\x1a\n").unwrap();
        let picture = picture.to_string_lossy().into_owned();
        let queue = vec![
            UserInput::Text {
                text: "queued one\nsecond line".into(),
            },
            UserInput::LocalImage {
                path: picture.clone(),
                name: Some("picture.png".into()),
            },
        ];
        let mut ui = ui();
        start(&mut ui, text("keep in conversation"));
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
        ui.app_mut().input_sending();
        ui.app_mut().input_sent(5, queue);
        queued(
            &mut ui,
            "q1",
            vec![
                UserInput::Text {
                    text: "queued one\nsecond line".into(),
                },
                UserInput::LocalImage {
                    path: "image-server-queued.png".into(),
                    name: Some("picture.png".into()),
                },
            ],
        );
        ui.app_mut().handle_paste("original draft");
        assert!(matches!(
            ui.app_mut().handle_key(key(KeyCode::Esc), 0),
            Some(Action::Interrupt { .. })
        ));
        if reply_first {
            ui.app_mut().interrupt_finished("turn", true);
        }
        interrupted(&mut ui, "turn");
        if !reply_first {
            ui.app_mut().interrupt_finished("turn", true);
        }
        ui.draw(0).unwrap();
        assert!(screen(&ui).contains("keep in conversation"));
        assert!(screen(&ui).contains("Turn interrupted"));
        let Some(Action::SendImages { input, .. }) =
            ui.app_mut().handle_key(key(KeyCode::Enter), 0)
        else {
            panic!("the queued image must come back as an attachment");
        };
        let contents: String = input
            .iter()
            .filter_map(|part| match part {
                UserInput::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(contents, "queued one\nsecond line\noriginal draft");
        assert!(
            input
                .iter()
                .any(|part| matches!(part, UserInput::LocalImage { path, .. } if *path == picture))
        );
    }
}

#[test]
fn interrupting_a_user_shell_turn_restores_queued_inputs() {
    let mut ui = ui();
    let turn = serde_json::from_value(json!({
        "id":"shell", "input":[], "items":[], "clientName":"userShell",
        "status":"inProgress", "error":null, "usage":null,
    }))
    .unwrap();
    event(
        &mut ui,
        EventPayload::TurnStarted {
            turn,
            input_id: None,
        },
    );
    event(
        &mut ui,
        EventPayload::ItemStarted {
            turn_id: "shell".into(),
            item: serde_json::from_value(json!({
                "type":"commandExecution", "id":"command", "command":"sleep 100",
                "source":"userShell", "cwd":"/work", "status":"inProgress",
                "aggregatedOutput":"", "exitCode":null, "durationMs":null,
            }))
            .unwrap(),
        },
    );
    event(
        &mut ui,
        EventPayload::SessionStatus {
            status: SessionStatus::Working,
            close_reason: None,
        },
    );
    queued(&mut ui, "q1", text("queued behind the command"));
    ui.app_mut().handle_paste("draft");
    assert_eq!(
        ui.app_mut().handle_key(key(KeyCode::Esc), 0),
        Some(Action::Interrupt {
            turn_id: "shell".into()
        })
    );
    ui.app_mut().interrupt_finished("shell", true);
    interrupted(&mut ui, "shell");
    assert_eq!(
        ui.app_mut().handle_key(key(KeyCode::Enter), 0),
        Some(Action::Send("queued behind the command\ndraft".into()))
    );
}

#[test]
fn interrupt_from_another_terminal_does_not_restore_queued_inputs() {
    // `false`: this terminal pressed Esc too, but the server answered that
    // the turn was no longer running, so the other terminal stopped it.
    for pressed_esc in [false, true] {
        let mut ui = ui();
        start(&mut ui, text("someone else stops this"));
        queued(&mut ui, "q1", text("queued input"));
        ui.app_mut().handle_paste("own draft");
        if pressed_esc {
            ui.app_mut().handle_key(key(KeyCode::Esc), 0);
        }
        interrupted(&mut ui, "turn");
        if pressed_esc {
            ui.app_mut().interrupt_finished("turn", false);
        }
        assert_eq!(
            ui.app_mut().handle_key(key(KeyCode::Enter), 0),
            Some(Action::Send("own draft".into()))
        );
    }
}

#[test]
fn retract_keeps_notices_before_the_turn_and_removes_notices_inside_it() {
    for initiating in [false, true] {
        let mut ui = ui();
        let previous: Turn = serde_json::from_value(json!({
            "id":"previous", "input":text("earlier input"), "clientName":"test",
            "status":"completed", "error":null, "usage":null,
            "items":[{"type":"userMessage","id":"previous-user","content":text("earlier input")}]
        }))
        .unwrap();
        event(
            &mut ui,
            EventPayload::TurnStarted {
                turn: previous,
                input_id: None,
            },
        );
        ui.app_mut().warning("keep the previous report");
        ui.draw(0).unwrap();
        start(&mut ui, text("withdraw this"));
        ui.app_mut().warning("remove the in-turn warning");
        ui.draw(0).unwrap();
        assert!(screen(&ui).contains("remove the in-turn warning"));
        if initiating {
            ui.app_mut().handle_key(key(KeyCode::Esc), 0);
            ui.app_mut().interrupt_finished("turn", true);
        }
        retract(&mut ui);
        assert!(screen(&ui).contains("earlier input"));
        assert!(screen(&ui).contains("keep the previous report"));
        assert!(!screen(&ui).contains("remove the in-turn warning"));
    }
}

#[test]
fn retract_on_another_terminal_keeps_btw_streaming_and_cancellable() {
    for cancel in [false, true] {
        let mut ui = ui();
        ui.app_mut().handle_paste("/btw explain this");
        let Some(Action::Btw { id, .. }) = ui.app_mut().handle_key(key(KeyCode::Enter), 0) else {
            panic!("btw action");
        };
        ui.app_mut().btw_event(
            id,
            BtwEvent::Delta {
                delta: "PARTIAL".into(),
            },
        );
        ui.draw(0).unwrap();
        assert!(screen(&ui).contains("PARTIAL"));

        start(&mut ui, text("withdraw from another terminal"));
        retract(&mut ui);
        ui.app_mut().btw_event(
            id,
            BtwEvent::Delta {
                delta: " REST".into(),
            },
        );
        ui.draw(0).unwrap();
        assert!(screen(&ui).contains("PARTIAL REST"));
        assert!(!screen(&ui).contains("withdraw from another terminal"));
        if cancel {
            assert_eq!(
                ui.app_mut().handle_key(key(KeyCode::Esc), 0),
                Some(Action::CancelBtw { id })
            );
        } else {
            ui.app_mut().btw_event(id, BtwEvent::Completed);
        }
        ui.draw(0).unwrap();
        assert!(screen(&ui).contains("PARTIAL REST"));
        assert_eq!(screen(&ui).contains("Stopped"), cancel);
        ui.app_mut().handle_paste("/btw next question");
        assert!(matches!(
            ui.app_mut().handle_key(key(KeyCode::Enter), 0),
            Some(Action::Btw { .. })
        ));
    }
}

#[test]
fn retract_does_not_restore_a_queued_review_as_an_ordinary_prompt() {
    let mut ui = ui();
    start(&mut ui, text("ordinary input"));
    event(
        &mut ui,
        EventPayload::InputQueued {
            input: QueuedInput {
                id: "q1".into(),
                input: text("generated review instructions and diff"),
                client_name: "test".into(),
                review: Some("Review changes".into()),
            },
        },
    );
    queued(&mut ui, "q2", text("ordinary queue"));
    ui.app_mut().handle_key(key(KeyCode::Esc), 0);
    ui.app_mut().interrupt_finished("turn", true);
    retract(&mut ui);
    assert_eq!(
        ui.app_mut().handle_key(key(KeyCode::Enter), 0),
        Some(Action::Send("ordinary input\nordinary queue".into()))
    );
}

#[test]
fn images_without_local_paths_become_text_and_request_repasting_in_the_status_line() {
    for resumed in [false, true] {
        let mut ui = ui();
        let image = |name: &str| {
            vec![UserInput::LocalImage {
                path: format!("image-stored-{name}"),
                name: Some(name.into()),
            }]
        };
        start(&mut ui, image("first.png"));
        queued(&mut ui, "q1", image("queued.png"));
        if resumed {
            let snapshot = ui.app().snapshot().clone();
            ui = Ui::new(TestBackend::new(90, 28), App::new(snapshot, None, 0)).unwrap();
            ui.draw(0).unwrap();
        }
        ui.app_mut().handle_key(key(KeyCode::Esc), 0);
        ui.app_mut().interrupt_finished("turn", true);
        retract(&mut ui);
        let status: String = (0..90)
            .map(|x| ui.backend().buffer()[(x, 21)].symbol())
            .collect();
        assert!(
            status.contains("Paste images again with Ctrl+V"),
            "{status}"
        );
        assert_eq!(
            ui.app_mut().handle_key(key(KeyCode::Enter), 0),
            Some(Action::Send(
                "[image: first.png]\n[image: queued.png]".into()
            ))
        );
    }
}
