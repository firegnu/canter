use super::{retract_tests::Harness, *};
use axum::response::IntoResponse;
use canter_protocol::{EventPayload, GoalSetParams};
use crossterm::event::KeyCode;
use serde_json::{Value, json};

fn goal() -> Value {
    json!({"id":"g1","objective":"Task","status":"active","budgetUsd":5,"cost":{"usd":0,"unknown":false},"createdAt":0,"updatedAt":0})
}

async fn accept_info(test: &mut Harness) {
    let request = test.request().await;
    assert_eq!(request.path, "/v1/info");
    request.json(
        json!({"features":["goals"],"serverVersion":"test","protocolVersion":1,"sessions":[]}),
    );
}

#[tokio::test]
async fn goal_escape_before_set_response_pauses_after_creation() {
    let mut test = Harness::new().await;
    test.chat.act(
        test.ui.app_mut(),
        Action::Goal(crate::GoalCommand::Set(GoalSetParams {
            objective: "Task".into(),
            budget_usd: None,
        })),
    );
    accept_info(&mut test).await;
    let setting = test.request().await;
    assert_eq!(setting.path, "/v1/sessions/test/goal");
    assert_eq!(setting.body, json!({"objective":"Task"}));
    test.key(KeyCode::Esc);
    setting.json(json!({"goal":goal(),"eventSeq":1}));
    test.message().await;
    accept_info(&mut test).await;
    let pause = test.request().await;
    assert_eq!(pause.path, "/v1/sessions/test/goal/pause");
    let mut paused = goal();
    paused["status"] = json!("paused");
    pause.json(json!({"goal":paused,"eventSeq":2}));
    test.message().await;
    test.stop().await;
}

#[tokio::test]
async fn goal_escape_that_loses_turn_race_still_sends_pause() {
    let mut test = Harness::new().await;
    test.event(EventPayload::SessionGoalUpdated {
        goal: Some(serde_json::from_value(goal()).unwrap()),
    });
    test.start("t1", "Task", None);
    test.key(KeyCode::Esc);
    let interrupt = test.request().await;
    assert_eq!(interrupt.path, "/v1/sessions/test/interrupt");
    assert!(
        interrupt
            .reply
            .send(
                (
                    axum::http::StatusCode::CONFLICT,
                    axum::Json(json!({"code":"turn_mismatch","message":"turn is not active"}))
                )
                    .into_response()
            )
            .is_ok()
    );
    let pause = test.request().await;
    assert_eq!(pause.path, "/v1/sessions/test/goal/pause");
    let mut paused = goal();
    paused["status"] = json!("paused");
    pause.json(json!({"goal":paused,"eventSeq":4}));
    test.message().await;
    test.stop().await;
}
