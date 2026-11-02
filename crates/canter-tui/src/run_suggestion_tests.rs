use super::{retract_tests::Harness, *};
use canter_protocol::{EventPayload, sse_frame};
use crossterm::event::KeyCode;
use serde_json::json;

#[tokio::test]
async fn accepted_and_edited_suggestions_mark_only_their_input() {
    for accept in [KeyCode::Tab, KeyCode::Right] {
        let mut test = Harness::new().await;
        test.event(EventPayload::SessionPromptSuggestion {
            turn_id: None,
            text: "add tests".into(),
        });
        test.key(accept);
        test.key(KeyCode::Char('!'));
        test.key(KeyCode::Enter);
        let request = test.request().await;
        assert_eq!(request.body["fromSuggestion"], true);
        assert_eq!(request.body["input"][0]["text"], "add tests!");
        request.json(json!({"eventSeq":1,"turnId":null}));
        test.message().await;
        test.ui.app_mut().handle_paste("different input");
        test.ui.draw(0).unwrap();
        test.key(KeyCode::Enter);
        let request = test.request().await;
        assert!(request.body.get("fromSuggestion").is_none());
        request.json(json!({"eventSeq":2,"turnId":null}));
        test.message().await;
        test.stop().await;
    }
}

#[tokio::test]
async fn live_suggestions_do_not_advance_the_sse_resume_cursor() {
    let mut test = Harness::new().await;
    let client = test.chat.client.clone();
    let subscribe = tokio::spawn(async move { client.subscribe("test", 7).await.unwrap() });
    let request = test.request().await;
    let suggestion = Event {
        seq: 0,
        session_id: "test".into(),
        timestamp: 0,
        payload: EventPayload::SessionPromptSuggestion {
            turn_id: None,
            text: "add tests".into(),
        },
    };
    let durable = Event {
        seq: 8,
        session_id: "test".into(),
        timestamp: 0,
        payload: EventPayload::Warning {
            message: "saved event".into(),
        },
    };
    let frame = sse_frame(&suggestion).unwrap();
    assert!(!frame.contains("id:"));
    assert!(
        request
            .reply
            .send(
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(format!(
                        "{frame}{}",
                        sse_frame(&durable).unwrap()
                    )))
                    .unwrap()
            )
            .is_ok()
    );
    let mut events = subscribe.await.unwrap();
    assert_eq!(events.next().await.unwrap().unwrap(), suggestion);
    assert_eq!(events.last_seq(), 7);
    assert_eq!(events.next().await.unwrap().unwrap(), durable);
    assert_eq!(events.last_seq(), 8);
    test.stop().await;
}
