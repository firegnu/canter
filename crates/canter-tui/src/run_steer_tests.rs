//! Tab while working steers the turn in progress; Enter queues as before
//! (P2-97, user's choice: Enter queues, Tab steers).

use super::retract_tests::Harness;
use super::*;
use canter_protocol::{
    EventPayload, InputRemovalReason, Item, QueuedInput, Turn, TurnStatus, UserInput,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde_json::json;

const HINT: &str = "Tab to steer";

fn shown(ui: &Ui<TestBackend>) -> String {
    let buffer = ui.backend().buffer();
    let width = usize::from(buffer.area.width);
    buffer
        .content
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn text(value: &str) -> Vec<UserInput> {
    vec![UserInput::Text { text: value.into() }]
}

fn queued(test: &mut Harness, id: &str, value: &str) {
    test.event(EventPayload::InputQueued {
        input: QueuedInput {
            review: None,
            id: id.into(),
            input: text(value),
            client_name: "test".into(),
        },
    });
}

fn next_seq(test: &Harness) -> u64 {
    test.ui.app().snapshot().last_event_seq + 1
}

/// Types `value`, presses `code` and answers the input request with
/// `turn_id`; `event_first` sends `input.queued` before the reply.
async fn send(
    test: &mut Harness,
    value: &str,
    code: KeyCode,
    turn_id: Option<&str>,
    event_first: bool,
) -> serde_json::Value {
    test.ui.app_mut().handle_paste(value);
    test.ui.draw(0).unwrap();
    test.key(code);
    let request = test.request().await;
    assert_eq!(request.path, "/v1/sessions/test/input");
    let body = request.body.clone();
    let seq = next_seq(test);
    if event_first {
        queued(test, "q1", value);
    }
    request.json(json!({"eventSeq": seq, "turnId": turn_id}));
    test.message().await;
    if !event_first {
        queued(test, "q1", value);
    }
    test.ui.draw(0).unwrap();
    body
}

#[tokio::test]
async fn tab_while_working_steers_and_shows_steering_until_the_turn_takes_it() {
    for event_first in [true, false] {
        let mut test = Harness::new().await;
        test.start("turn", "first", None);
        let body = send(
            &mut test,
            "look at the tests too",
            KeyCode::Tab,
            Some("turn"),
            event_first,
        )
        .await;
        assert_eq!(body["mode"], "steer");
        assert_eq!(body["input"], json!(text("look at the tests too")));
        let screen = shown(&test.ui);
        assert!(
            screen.contains("↳ Steering: look at the tests too"),
            "{screen}"
        );
        assert!(!screen.contains("Queued"), "{screen}");

        // The core takes it into this turn at the next model request.
        test.event(EventPayload::ItemCompleted {
            turn_id: "turn".into(),
            item: Item::UserMessage {
                id: "user-2".into(),
                content: text("look at the tests too"),
            },
        });
        test.event(EventPayload::InputRemoved {
            input_ids: vec!["q1".into()],
            reason: InputRemovalReason::Steered,
            turn_id: Some("turn".into()),
        });
        let screen = shown(&test.ui);
        assert!(!screen.contains("Steering"), "{screen}");
        assert!(screen.contains("› look at the tests too"), "{screen}");
        test.stop().await;
    }
}

#[tokio::test]
async fn enter_while_working_still_queues() {
    let mut test = Harness::new().await;
    test.start("turn", "first", None);
    let body = send(&mut test, "then do this", KeyCode::Enter, None, true).await;
    assert_eq!(body["mode"], "queue");
    let screen = shown(&test.ui);
    assert!(screen.contains("· Queued: then do this"), "{screen}");
    assert!(!screen.contains("Steering"), "{screen}");
    test.stop().await;
}

#[tokio::test]
async fn a_steer_the_server_queued_shows_as_queued() {
    // E.g. the turn ended just then, or it is a review turn.
    for event_first in [true, false] {
        let mut test = Harness::new().await;
        test.start("turn", "first", None);
        let body = send(&mut test, "too late", KeyCode::Tab, None, event_first).await;
        assert_eq!(body["mode"], "steer");
        let screen = shown(&test.ui);
        assert!(screen.contains("· Queued: too late"), "{screen}");
        assert!(!screen.contains("Steering"), "{screen}");
        test.stop().await;
    }
}

#[tokio::test]
async fn steering_shows_before_queued_messages() {
    let mut test = Harness::new().await;
    test.start("turn", "first", None);
    queued(&mut test, "q0", "queued earlier");
    send(&mut test, "steer now", KeyCode::Tab, Some("turn"), true).await;
    let screen = shown(&test.ui);
    let steering = screen.find("↳ Steering: steer now").expect(&screen);
    let queued = screen.find("· Queued: queued earlier").expect(&screen);
    assert!(steering < queued, "{screen}");
    test.stop().await;
}

#[tokio::test]
async fn tab_with_images_steers() {
    let mut test = Harness::new().await;
    test.start("turn", "first", None);
    let snapshot = test.ui.app().snapshot().clone();
    *test.ui.app_mut() = App::new(snapshot, None, 0).with_clipboard_reader(|path| {
        std::fs::write(path, b"\x89PNG\r\n\x1a\n").unwrap();
        Ok(true)
    });
    test.ui.app_mut().handle_paste("see ");
    test.ui
        .app_mut()
        .handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL), 0);
    test.ui.draw(0).unwrap();
    test.key(KeyCode::Tab);
    let request = test.request().await;
    assert_eq!(request.body["mode"], "steer");
    assert!(
        request.body["input"]
            .as_array()
            .unwrap()
            .iter()
            .any(|part| part["type"] == "localImage"),
        "{}",
        request.body
    );
    request.json(json!({"eventSeq": next_seq(&test), "turnId": "turn"}));
    test.message().await;
    test.stop().await;
}

#[tokio::test]
async fn tab_does_not_steer_commands_or_when_idle() {
    let mut test = Harness::new().await;
    // Idle: nothing, the draft stays.
    test.ui.app_mut().handle_paste("not working yet");
    test.ui.draw(0).unwrap();
    assert_eq!(press(&mut test, KeyCode::Tab), None);
    assert_eq!(draft(&test.ui), "not working yet");
    assert!(!shown(&test.ui).contains(HINT));
    test.ui.app_mut().handle_key(ctrl('c'), 0);

    test.start("turn", "first", None);
    for typed in ["/model x", "! ls", "   "] {
        test.ui.app_mut().handle_paste(typed);
        test.ui.draw(0).unwrap();
        assert_eq!(press(&mut test, KeyCode::Tab), None, "{typed}");
        assert_eq!(draft(&test.ui), typed.trim_end());
        assert!(!shown(&test.ui).contains(HINT), "{typed}");
        test.ui.app_mut().handle_key(ctrl('c'), 0);
    }
    assert!(test.requests.try_recv().is_err());
    test.stop().await;
}

#[tokio::test]
async fn tab_in_the_command_list_still_completes() {
    let mut test = Harness::new().await;
    test.start("turn", "first", None);
    test.ui.app_mut().handle_paste("/comp");
    test.ui.draw(0).unwrap();
    assert!(!shown(&test.ui).contains(HINT));
    assert_eq!(press(&mut test, KeyCode::Tab), None);
    assert_eq!(draft(&test.ui), "/compact");
    assert!(test.requests.try_recv().is_err());
    test.stop().await;
}

#[tokio::test]
async fn the_hint_shows_only_while_working_with_a_message_typed() {
    let mut test = Harness::new().await;
    test.start("turn", "first", None);
    assert!(!shown(&test.ui).contains(HINT));
    test.ui.app_mut().handle_paste("more");
    test.ui.draw(0).unwrap();
    let screen = shown(&test.ui);
    assert!(
        screen.contains("Esc to interrupt · Tab to steer"),
        "{screen}"
    );

    // The turn ends: idle, no hint.
    let turn: Turn = serde_json::from_value(json!({
        "id":"turn","input":[{"type":"text","text":"first"}],"items":[],
        "status":"completed","clientName":"test","error":null,"usage":null,
    }))
    .unwrap();
    assert_eq!(turn.status, TurnStatus::Completed);
    test.event(EventPayload::TurnCompleted { turn });
    test.event(EventPayload::SessionStatus {
        status: SessionStatus::Idle,
        close_reason: None,
    });
    assert!(!shown(&test.ui).contains(HINT));
    test.stop().await;
}

#[tokio::test]
async fn a_failed_steer_puts_the_draft_back_and_is_in_the_history() {
    use axum::response::IntoResponse;
    let mut test = Harness::new().await;
    test.start("turn", "first", None);
    test.ui.app_mut().handle_paste("steer me");
    test.ui.draw(0).unwrap();
    test.key(KeyCode::Tab);
    test.ui.draw(0).unwrap();
    assert_eq!(draft(&test.ui), "");
    let request = test.request().await;
    assert_eq!(request.body["mode"], "steer");
    let response = (
        axum::http::StatusCode::CONFLICT,
        axum::Json(json!({"code":"conflict","message":"offline"})),
    )
        .into_response();
    assert!(request.reply.send(response).is_ok());
    test.message().await;
    assert_eq!(draft(&test.ui), "steer me");
    assert!(shown(&test.ui).contains("✗ Not sent:"));
    // Recorded like Enter: ↑ in the empty box brings it back.
    test.ui.app_mut().handle_key(ctrl('c'), 0);
    test.key(KeyCode::Up);
    test.ui.draw(0).unwrap();
    assert_eq!(draft(&test.ui), "steer me");
    test.stop().await;
}

#[tokio::test]
async fn steered_message_in_a_reopened_session_is_drawn_like_a_sent_one() {
    let dark = theme::Theme {
        depth: theme::Depth::TrueColor,
        background: theme::Background::Dark,
    };
    theme::set(dark);
    let snapshot: SessionSnapshot = serde_json::from_value(json!({
        "id":"test","settings":{"cwd":"/work","name":null,"model":"m","approvalPolicy":"alwaysAsk"},
        "status":"idle","createdAt":0,"updatedAt":0,"pendingRequests":[],"queuedInputs":[],
        "tokenUsage":null,"error":null,"lastEventSeq":9,
        "turns":[{
            "id":"turn","input":[{"type":"text","text":"first"}],"clientName":"test",
            "status":"completed","error":null,"usage":null,
            "items":[
                {"type":"userMessage","id":"u1","content":[{"type":"text","text":"first"}]},
                {"type":"agentMessage","id":"a1","text":"working on it"},
                {"type":"userMessage","id":"u2","content":[{"type":"text","text":"steered in"}]},
                {"type":"agentMessage","id":"a2","text":"done"},
            ],
        }],
    }))
    .unwrap();
    let mut ui = Ui::new(TestBackend::new(60, 24), App::new(snapshot, None, 0)).unwrap();
    ui.draw(0).unwrap();
    let screen = shown(&ui);
    let rows: Vec<&str> = screen.lines().collect();
    let row = rows
        .iter()
        .position(|row| row.contains("› steered in"))
        .expect(&screen);
    let answer = rows
        .iter()
        .position(|row| row.contains("working on it"))
        .unwrap();
    let done = rows.iter().position(|row| row.contains("done")).unwrap();
    assert!(answer < row && row < done, "{screen}");
    let buffer = ui.backend().buffer();
    let x = rows[row].chars().position(|c| c == 's').unwrap() as u16;
    assert_eq!(
        buffer[(x, row as u16)].bg,
        Color::Rgb(0x2d, 0x2e, 0x33),
        "{screen}"
    );
    theme::set(theme::Theme::default());
}

#[tokio::test]
async fn steering_marks_carry_over_to_the_session_shown_again() {
    let mut test = Harness::new().await;
    test.start("turn", "first", None);
    send(&mut test, "keep me", KeyCode::Tab, Some("turn"), true).await;
    let snapshot = test.ui.app().snapshot().clone();
    let app = App::new(snapshot, None, 0).with_steering_from(test.ui.app());
    *test.ui.app_mut() = app;
    test.ui.draw(0).unwrap();
    let screen = shown(&test.ui);
    assert!(screen.contains("↳ Steering: keep me"), "{screen}");
    test.stop().await;
}

fn press(test: &mut Harness, code: KeyCode) -> Option<Action> {
    let action = test
        .ui
        .app_mut()
        .handle_key(KeyEvent::new(code, KeyModifiers::NONE), 0);
    test.ui.draw(0).unwrap();
    action
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// What the input box shows: the text after its `› ` (the last on screen).
fn draft(ui: &Ui<TestBackend>) -> String {
    let screen = shown(ui);
    let row = screen.lines().rfind(|row| row.contains("› ")).unwrap_or("");
    let after = row.split_once("› ").map_or("", |(_, after)| after);
    let after = after.trim_end_matches([' ', '│', '┃']).trim_end();
    if after.starts_with("Type a message") {
        return String::new();
    }
    after.to_owned()
}
