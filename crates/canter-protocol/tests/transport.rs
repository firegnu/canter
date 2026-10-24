use canter_protocol::*;
use serde_json::json;

#[test]
fn queued_input_is_visible_in_snapshots_until_its_turn_starts() {
    let queued: Event = serde_json::from_value(json!({"seq":1,"sessionId":"s1","timestamp":1,
        "type":"input.queued", "input":{"id":"q1","input":[{"type":"text","text":"next task"}],"clientName":"terminal-b"}})).unwrap();
    let snapshot = apply_event(&SessionSnapshot::empty("s1"), &queued).unwrap();
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap()["queuedInputs"][0]["id"],
        "q1"
    );
    let started: Event = serde_json::from_value(json!({"seq":2,"sessionId":"s1","timestamp":2,
        "type":"turn.started","inputId":"q1", "turn":{"id":"t1","items":[],"status":"inProgress","error":null,
        "input":[{"type":"text","text":"next task"}],"clientName":"terminal-b","usage":null}})).unwrap();
    let snapshot = apply_event(&snapshot, &started).unwrap();
    assert_eq!(
        serde_json::to_value(snapshot).unwrap()["queuedInputs"],
        json!([])
    );
}

#[test]
fn sse_frames_and_resume_cursor_follow_the_shared_contract() {
    let event = Event {
        seq: 42,
        session_id: "s1".into(),
        timestamp: 9,
        payload: EventPayload::Warning {
            message: "line one\nline two".into(),
        },
    };
    let frame = sse_frame(&event).unwrap();
    assert!(frame.starts_with("id: 42\nevent: message\ndata: "));
    assert!(frame.ends_with("\n\n"));
    assert_eq!(
        frame.lines().count(),
        4,
        "embedded newlines must remain escaped in JSON"
    );
    let data = frame
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    assert_eq!(serde_json::from_str::<Event>(data).unwrap(), event);
    assert_eq!(resume_after(4, Some("42")).unwrap(), 42);
    assert_eq!(resume_after(4, None).unwrap(), 4);
    for invalid in ["", "-1", "abc", "18446744073709551616"] {
        assert_eq!(
            resume_after(4, Some(invalid)).unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }
    assert_eq!(
        serde_json::to_value(ErrorResponse {
            code: ErrorCode::InvalidDecision,
            message: "unsupported answer".into()
        })
        .unwrap(),
        json!({"code":"invalid_decision","message":"unsupported answer"})
    );
}
