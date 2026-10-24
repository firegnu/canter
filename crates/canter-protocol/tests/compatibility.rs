use canter_protocol::{Event, EventPayload, Item, SessionSnapshot, SessionStatus, apply_event};
use serde_json::json;

#[test]
fn unknown_events_and_items_can_be_skipped() {
    let event: Event = serde_json::from_value(json!({
        "seq": 1, "sessionId": "s1", "timestamp": 100,
        "type": "future.event", "extra": { "anything": true }
    }))
    .expect("a future event must not break the event stream");
    assert_eq!(event.payload, EventPayload::Unknown);
    let snapshot = apply_event(&SessionSnapshot::empty("s1"), &event).unwrap();
    assert_eq!(snapshot.last_event_seq, 1);
    assert!(snapshot.turns.is_empty());

    let event: Event = serde_json::from_value(json!({
        "seq": 2, "sessionId": "s1", "timestamp": 101,
        "type": "item.started", "turnId": "t1",
        "item": { "type": "futureTool", "id": "i1", "extra": [1, 2] }
    }))
    .expect("a future item must not break its enclosing event");
    assert!(matches!(
        event.payload,
        EventPayload::ItemStarted {
            item: Item::Unknown,
            ..
        }
    ));
    let snapshot = apply_event(&snapshot, &event).unwrap();
    assert!(snapshot.turns.is_empty());
    let snapshot = apply_event(
        &snapshot,
        &Event {
            seq: 3,
            session_id: "s1".into(),
            timestamp: 102,
            payload: EventPayload::SessionStatus {
                status: SessionStatus::Idle,
            },
        },
    )
    .unwrap();
    assert_eq!(snapshot.last_event_seq, 3);
    assert_eq!(snapshot.status, SessionStatus::Idle);

    let item: Item = serde_json::from_value(json!({
        "type": "agentMessage", "id": "i2", "text": "hello", "futureField": 42
    }))
    .unwrap();
    assert_eq!(
        item,
        Item::AgentMessage {
            id: "i2".into(),
            text: "hello".into()
        }
    );
}

#[test]
fn unknown_enum_values_inside_known_messages_are_tolerated() {
    use canter_protocol::*;
    fn unknown<T: serde::de::DeserializeOwned + serde::Serialize>(
        value: serde_json::Value,
        expected: serde_json::Value,
    ) {
        let decoded: T = serde_json::from_value(value)
            .expect("a new enum value must not break its containing message");
        assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
    }
    unknown::<SessionStatus>(json!("waiting_answer"), json!("unknown"));
    unknown::<TurnStatus>(json!("futureStatus"), json!("unknown"));
    unknown::<ItemStatus>(json!("futureStatus"), json!("unknown"));
    unknown::<PermissionMode>(json!("futurePolicy"), json!("unknown"));
    unknown::<ApprovalDecision>(json!("futureDecision"), json!("unknown"));
    unknown::<InputMode>(json!("futureMode"), json!("unknown"));
    unknown::<ApprovalResolution>(
        json!({"type":"futureResolution","extra":1}),
        json!({"type":"unknown"}),
    );
    unknown::<PatchChangeKind>(
        json!({"type":"futureChange","extra":1}),
        json!({"type":"unknown"}),
    );
    unknown::<UserInput>(
        json!({"type":"image","url":"fixture://image"}),
        json!({"type":"unknown"}),
    );
    let event: Event = serde_json::from_value(json!({"seq":1,"sessionId":"s1","timestamp":1,"type":"session.status","status":"waiting_answer"})).unwrap();
    assert_eq!(
        serde_json::to_value(apply_event(&SessionSnapshot::empty("s1"), &event).unwrap()).unwrap()
            ["status"],
        "unknown"
    );
}

#[test]
fn an_unknown_pending_request_can_be_read_and_resolved_by_id() {
    use canter_protocol::*;
    let mut value = serde_json::to_value(SessionSnapshot::empty("s1")).unwrap();
    value["pendingRequests"] = json!([{"id":"future-request","turnId":"t1","createdAt":1,
        "kind":{"type":"question","questions":["future question"]}}]);
    let snapshot: SessionSnapshot =
        serde_json::from_value(value).expect("unknown request content must not break a snapshot");
    assert_eq!(snapshot.pending_requests[0].id, "future-request");
    let resolved = apply_event(
        &snapshot,
        &Event {
            seq: 1,
            session_id: "s1".into(),
            timestamp: 2,
            payload: EventPayload::RequestResolved {
                request_id: "future-request".into(),
                resolution: ApprovalResolution::Cancelled,
                client_name: None,
            },
        },
    )
    .unwrap();
    assert!(resolved.pending_requests.is_empty());
}
