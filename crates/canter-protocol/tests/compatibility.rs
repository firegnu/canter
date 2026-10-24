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
