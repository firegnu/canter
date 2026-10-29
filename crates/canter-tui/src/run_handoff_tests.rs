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

#[tokio::test]
async fn clear_context_plan_resolves_then_starts_a_new_session_with_selected_permissions() {
    let mut test = Harness::new().await;
    test.start("planning", "Plan the change", None);
    test.event(serde_json::from_value(json!({"type":"request.opened","request":{
        "id":"proposal","turnId":"planning","createdAt":0,
        "kind":{"type":"planApproval","itemId":"p1","plan":"Build and verify.","approvalPolicy":"alwaysAsk"}
    }})).unwrap());
    test.ui.app_mut().handle_paste("Existing draft");
    let mut finished = test.ui.app().snapshot().turns[0].clone();
    finished.status = canter_protocol::TurnStatus::Completed;
    let mut fresh = SessionSnapshot::empty("implementation");
    fresh.status = SessionStatus::Working;
    fresh.settings.cwd = test.ui.app().snapshot().settings.cwd.clone();
    fresh.settings.approval_policy = PermissionMode::AskForExecution;
    let answer = canter_protocol::PlanAnswer {
        decision: canter_protocol::PlanDecision::ClearContext,
        approval_policy: PermissionMode::AskForExecution,
        feedback: None,
    };
    let Harness {
        chat, ui, requests, ..
    } = &mut test;
    let (receiver, ()) = tokio::join!(
        chat.resolve_plan(ui, "proposal", "# Build\n\nImplement and verify.", answer),
        async {
            let resolve = next(requests).await;
            assert!(resolve.path.ends_with("/requests/proposal"));
            assert_eq!(resolve.body["plan"]["decision"], "clearContext");
            assert_eq!(resolve.body["plan"]["approvalPolicy"], "askForExecution");
            resolve.json(json!({"eventSeq":8,"alreadyResolved":false}));
            let events = next(requests).await;
            assert_eq!(
                events.path, "/v1/events",
                "wait for the original turn and recover its withdrawn input before creating a session"
            );
            let payloads = [
                json!({"type":"input.queued","input":{"id":"q1","input":[{"type":"text","text":"Queued follow-up"}],"clientName":"other terminal","review":null}}),
                json!({"type":"input.queued","input":{"id":"q2","input":[{"type":"text","text":"Steering correction"}],"clientName":"other terminal","review":null}}),
                json!({"type":"input.removed","inputIds":["q1","q2"],"reason":"interrupted","turnId":null}),
                json!({"type":"turn.completed","turn":finished}),
            ];
            let data = payloads
                .into_iter()
                .enumerate()
                .map(|(i, mut payload)| {
                    let fields = payload.as_object_mut().unwrap();
                    fields.insert("seq".into(), json!(9 + i));
                    fields.insert("sessionId".into(), json!("test"));
                    fields.insert("timestamp".into(), json!(0));
                    format!("data: {payload}\n\n")
                })
                .collect::<String>();
            assert!(events.reply.send(Response::new(Body::from(data))).is_ok());
            let create = next(requests).await;
            assert_eq!(create.path, "/v1/sessions");
            assert_eq!(create.body["cwd"], fresh.settings.cwd);
            assert_eq!(create.body["approvalPolicy"], "askForExecution");
            let text = create.body["input"][0]["text"].as_str().unwrap();
            assert!(text.contains("# Build\n\nImplement and verify."));
            assert!(text.contains("Implement this plan"));
            assert!(text.contains("update_plan"));
            assert!(!text.contains("Queued follow-up"));
            assert!(!text.contains("Steering correction"));
            create.json(json!({"session":fresh,"eventSeq":1}));
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
            let skills = next(requests).await;
            assert_eq!(skills.path, "/v1/sessions/implementation/skills");
            skills.json(json!({"data":[]}));
        }
    );
    assert!(receiver.is_some());
    assert_eq!(test.chat.session, "implementation");
    assert!(!test.ui.app().snapshot().settings.plan_mode);
    assert_eq!(
        test.ui.app().editor_draft(),
        "Queued follow-up\nSteering correction\nExisting draft"
    );
    test.ui.draw(now_ms()).unwrap();
    assert!(text(&test.ui).contains("2 inputs withdrawn to draft"));
    assert!(
        test.requests.try_recv().is_err(),
        "withdrawn input is not automatically sent"
    );
    test.chat.pump.abort();
    test.server.abort();
}

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
            let skills = next(requests).await;
            assert_eq!(skills.path, "/v1/sessions/original/skills");
            skills.json(json!({"data":[]}));
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
