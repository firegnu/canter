use super::retract_tests::Harness;
use super::*;
use canter_protocol::{EventPayload, InputRemovalReason, TurnStatus};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

async fn capability(test: &mut Harness, supported: bool) {
    let request = test.request().await;
    assert_eq!(request.path, "/v1/info");
    request.json(
        json!({"serverVersion":"test","protocolVersion":1,"sessions":[],
        "features":if supported { vec!["imageData"] } else { vec![] }}),
    );
}

#[tokio::test]
async fn image_data_missing_capability_or_size_errors_restore_draft_without_upload() {
    for (supported, size, count, expected) in [
        (false, 8, 1, "canter serve --replace"),
        (
            true,
            canter_protocol::MAX_IMAGE_BYTES + 1,
            1,
            "image exceeds the 8 MiB limit",
        ),
        (
            true,
            canter_protocol::MAX_IMAGE_BYTES,
            9,
            "combined images exceed the 64 MiB limit",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shot.png");
        std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(size)
            .unwrap();
        let mut test = Harness::new().await;
        let paths = vec![path.to_str().unwrap(); count].join(" ");
        test.ui.app_mut().handle_paste(&paths);
        test.key(KeyCode::Enter);
        capability(&mut test, supported).await;
        test.message().await;
        let shown: String = test
            .ui
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            shown
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .contains(expected),
            "{shown}"
        );
        assert!(
            test.requests.try_recv().is_err(),
            "an invalid upload must not be sent"
        );
        assert!(
            matches!(
                test.ui
                    .app_mut()
                    .handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), 0),
                Some(Action::SendImages { .. })
            ),
            "the original draft must be editable"
        );
        test.stop().await;
    }
}

#[tokio::test]
async fn image_data_retracted_queue_is_edited_and_uploaded_again_as_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("shot.png");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
    let mut test = Harness::new().await;
    test.start("turn", "hold", None);
    test.ui.app_mut().handle_paste(path.to_str().unwrap());
    test.key(KeyCode::Enter);
    capability(&mut test, true).await;
    let request = test.request().await;
    let wire = request.body["input"].clone();
    assert_eq!(
        wire,
        json!([{"type":"image","url":"data:image/png;base64,iVBORw0KGgo=","name":"shot.png"}])
    );
    let seq = test.ui.app().snapshot().last_event_seq + 1;
    test.event(EventPayload::InputQueued { input: serde_json::from_value(json!({
        "id":"q","input":[{"type":"localImage","path":"image-copy.png","name":"shot.png"}],"clientName":"test"
    })).unwrap() });
    request.json(json!({"eventSeq":seq,"turnId":null}));
    test.message().await;
    test.key(KeyCode::Esc);
    let interrupt = test.request().await;
    assert_eq!(interrupt.path, "/v1/sessions/test/interrupt");
    test.event(EventPayload::InputRemoved {
        input_ids: vec!["q".into()],
        reason: InputRemovalReason::Interrupted,
        turn_id: None,
    });
    let mut turn = test.ui.app().snapshot().turns[0].clone();
    turn.status = TurnStatus::Interrupted;
    test.event(EventPayload::TurnCompleted { turn });
    test.event(EventPayload::SessionStatus {
        status: SessionStatus::Idle,
        close_reason: None,
    });
    interrupt.json(json!({"eventSeq":test.ui.app().snapshot().last_event_seq,"retracted":false}));
    test.message().await;
    test.ui.app_mut().handle_paste(" edited");
    test.key(KeyCode::Enter);
    capability(&mut test, true).await;
    let request = test.request().await;
    assert_eq!(request.body["input"][0], wire[0]);
    assert_eq!(
        request.body["input"][1],
        json!({"type":"text","text":" edited"})
    );
    request.json(json!({"eventSeq":test.ui.app().snapshot().last_event_seq+1,"turnId":"next"}));
    test.message().await;
    test.stop().await;
}
