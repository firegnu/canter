//! Esc twice in the session `/handoff` opened, before anything was sent,
//! goes back to the session the note came from (P2-86).

use super::retract_tests::{Harness, Request};
use super::*;
use axum::{body::Body, response::Response};
use canter_protocol::Turn;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

const HINT: &str = "Esc again to return to the original session";
const NOTE: &str = "handoff note to edit";

fn text(ui: &Ui<TestBackend>) -> String {
    ui.backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn key(ui: &mut Ui<TestBackend>, code: KeyCode, at: i64) -> Option<Action> {
    let action = ui
        .app_mut()
        .handle_key(KeyEvent::new(code, KeyModifiers::NONE), at);
    ui.draw(at).unwrap();
    action
}

/// A new, empty session on screen; with `handoff`, as `/handoff` leaves it.
fn new_session(handoff: bool) -> Ui<TestBackend> {
    let mut snapshot = SessionSnapshot::empty("new");
    snapshot.status = SessionStatus::Idle;
    let mut ui = Ui::new(TestBackend::new(90, 28), App::new(snapshot, None, 0)).unwrap();
    if handoff {
        ui.app_mut().handoff_draft("original", NOTE);
    }
    ui.draw(0).unwrap();
    ui
}

async fn next(requests: &mut mpsc::UnboundedReceiver<Request>) -> Request {
    tokio::time::timeout(Duration::from_secs(5), requests.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn esc_twice_returns_to_the_original_session_and_shows_its_conversation() {
    let mut test = Harness::new().await;
    test.ui.app_mut().handoff_draft("original", NOTE);
    test.ui.draw(0).unwrap();
    assert_eq!(key(&mut test.ui, KeyCode::Esc, 1_000), None);
    let Some(action) = key(&mut test.ui, KeyCode::Esc, 2_500) else {
        panic!("the second Esc goes back");
    };
    let mut original = SessionSnapshot::empty("original");
    original.status = SessionStatus::Idle;
    original.last_event_seq = 4;
    let turn: Turn = serde_json::from_value(json!({
        "id":"t1","input":[{"type":"text","text":"question in the original"}],
        "items":[{"type":"agentMessage","id":"a1","text":"answer in the original"}],
        "status":"completed","clientName":"test","error":null,"usage":null,"review":null,
    }))
    .unwrap();
    original.turns.push(turn);
    let Harness {
        chat, ui, requests, ..
    } = &mut test;
    let (receiver, ()) = tokio::join!(
        async {
            match action {
                Action::ReturnFromHandoff(source) => chat.return_from_handoff(ui, &source).await,
                other => panic!("unexpected {other:?}"),
            }
        },
        async {
            let resume = next(requests).await;
            assert_eq!(resume.path, "/v1/sessions/original/resume");
            resume.json(json!({"session":original,"eventSeq":4}));
            let events = next(requests).await;
            assert_eq!(events.path, "/v1/events");
            let open = futures_util::stream::pending::<Result<axum::body::Bytes, io::Error>>();
            assert!(
                events
                    .reply
                    .send(Response::new(Body::from_stream(open)))
                    .is_ok()
            );
            next(requests)
                .await
                .json(json!({"data":[],"nextCursor":null}));
        }
    );
    assert!(receiver.is_some());
    assert_eq!(test.chat.session, "original");
    test.ui.draw(3_000).unwrap();
    let shown = text(&test.ui);
    assert!(shown.contains("question in the original"), "{shown}");
    assert!(shown.contains("answer in the original"), "{shown}");
    assert!(!shown.contains(NOTE), "{shown}");
    test.chat.pump.abort();
    test.server.abort();
}

#[test]
fn one_esc_only_says_to_press_again() {
    let mut ui = new_session(true);
    assert_eq!(key(&mut ui, KeyCode::Esc, 1_000), None);
    let shown = text(&ui);
    assert!(shown.contains(HINT), "{shown}");
    assert!(shown.contains(NOTE), "{shown}");
    assert_eq!(ui.app().snapshot().id, "new");
    // The hint goes away like the other status line notes.
    ui.draw(3_500).unwrap();
    assert!(!text(&ui).contains(HINT));
}

#[test]
fn esc_more_than_two_seconds_later_starts_over() {
    let mut ui = new_session(true);
    assert_eq!(key(&mut ui, KeyCode::Esc, 1_000), None);
    assert_eq!(key(&mut ui, KeyCode::Esc, 3_001), None);
    assert!(text(&ui).contains(HINT));
    assert!(text(&ui).contains(NOTE));
    assert!(matches!(
        key(&mut ui, KeyCode::Esc, 4_000),
        Some(Action::ReturnFromHandoff(source)) if source == "original"
    ));
}

#[test]
fn after_sending_esc_twice_stays() {
    let mut ui = new_session(true);
    assert_eq!(
        key(&mut ui, KeyCode::Enter, 1_000),
        Some(Action::Send(NOTE.into()))
    );
    assert_eq!(key(&mut ui, KeyCode::Esc, 2_000), None);
    assert_eq!(key(&mut ui, KeyCode::Esc, 2_500), None);
    assert!(!text(&ui).contains(HINT));
}

#[test]
fn esc_with_a_popup_open_only_closes_it() {
    let mut ui = new_session(true);
    ui.app_mut().open_help();
    ui.draw(500).unwrap();
    assert!(text(&ui).contains("Help"));
    assert_eq!(key(&mut ui, KeyCode::Esc, 1_000), None);
    let shown = text(&ui);
    assert!(!shown.contains(" Help"), "{shown}");
    assert!(!shown.contains(HINT), "{shown}");
    // That Esc was the popup's: this one is the first.
    assert_eq!(key(&mut ui, KeyCode::Esc, 1_500), None);
    assert!(text(&ui).contains(HINT));
    assert!(matches!(
        key(&mut ui, KeyCode::Esc, 2_000),
        Some(Action::ReturnFromHandoff(_))
    ));
}

#[test]
fn a_session_from_new_stays_on_esc() {
    let mut ui = new_session(false);
    assert_eq!(key(&mut ui, KeyCode::Esc, 1_000), None);
    assert_eq!(key(&mut ui, KeyCode::Esc, 1_500), None);
    assert!(!text(&ui).contains(HINT));
}
