//! The usage of the turn in progress, filled by the reducer from the
//! session's own `session.usage` events, so a client can show it while the
//! turn runs; `session.btwUsage` is not the turn's, and the server's usage on
//! `turn.completed` replaces it.

use canter_protocol::*;
use serde_json::{Value, json};

const SESSION: &str = "s1";

fn event(seq: u64, payload: Value) -> Event {
    let mut event = json!({ "seq": seq, "sessionId": SESSION, "timestamp": 1_000 * seq as i64 });
    event
        .as_object_mut()
        .unwrap()
        .extend(payload.as_object().unwrap().clone());
    serde_json::from_value(event).unwrap()
}

fn turn(id: &str, status: &str, usage: Value) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "do it" }],
        "clientName": "terminal", "usage": usage,
    })
}

fn counts(input: u64, cached: u64, output: u64) -> Value {
    json!({
        "inputTokens": input, "cachedInputTokens": cached, "outputTokens": output,
        "reasoningOutputTokens": 0, "totalTokens": input + output,
    })
}

/// A `session.usage` (or `session.btwUsage`) event whose last request used
/// these tokens; `total` is the whole session's so far.
fn usage(kind: &str, total: Value, last: Value) -> Value {
    json!({ "type": kind, "tokenUsage": {
        "total": total, "last": last, "contextTokens": 0, "modelContextWindow": 1_000_000,
    }})
}

fn replay(events: &[Event]) -> SessionSnapshot {
    let mut snapshot = SessionSnapshot::empty(SESSION);
    for event in events {
        apply_event_mut(&mut snapshot, event).unwrap();
    }
    snapshot
}

fn turn_total(snapshot: &SessionSnapshot, index: usize) -> Option<TokenUsageBreakdown> {
    snapshot.turns[index]
        .usage
        .as_ref()
        .map(|usage| usage.total.clone())
}

fn breakdown(value: Value) -> TokenUsageBreakdown {
    serde_json::from_value(value).unwrap()
}

#[test]
fn a_running_turn_adds_up_its_own_requests_only() {
    let completed = json!({
        "total": counts(3_000, 2_500, 70), "last": counts(2_000, 1_900, 50),
        "contextTokens": 0, "modelContextWindow": 1_000_000,
    });
    let events: Vec<Event> = [
        json!({ "type": "turn.started", "turn": turn("t1", "inProgress", Value::Null) }),
        usage(
            "session.usage",
            counts(1_000, 600, 20),
            counts(1_000, 600, 20),
        ),
        // A `/btw` question while the turn runs is not the turn's.
        usage(
            "session.btwUsage",
            counts(1_500, 600, 25),
            counts(500, 0, 5),
        ),
        usage(
            "session.usage",
            counts(3_500, 2_500, 75),
            counts(2_000, 1_900, 50),
        ),
        json!({ "type": "turn.completed", "turn": turn("t1", "completed", completed.clone()) }),
        // Usage outside a turn, such as `/compact` while idle, is not a turn's.
        usage(
            "session.usage",
            counts(4_500, 2_500, 85),
            counts(1_000, 0, 10),
        ),
    ]
    .into_iter()
    .zip(1..)
    .map(|(payload, seq)| event(seq, payload))
    .collect();

    assert_eq!(turn_total(&replay(&events[..1]), 0), None, "no usage yet");
    assert_eq!(
        turn_total(&replay(&events[..2]), 0),
        Some(breakdown(counts(1_000, 600, 20)))
    );
    assert_eq!(
        turn_total(&replay(&events[..3]), 0),
        Some(breakdown(counts(1_000, 600, 20))),
        "not the /btw request"
    );
    assert_eq!(
        turn_total(&replay(&events[..4]), 0),
        Some(breakdown(counts(3_000, 2_500, 70)))
    );
    let whole = replay(&events);
    assert_eq!(
        whole.turns[0].usage,
        Some(serde_json::from_value(completed).unwrap()),
        "the server's usage on turn.completed"
    );

    // Read part way through, as after a reconnect: the same in the end.
    for split in 1..events.len() {
        let first = replay(&events[..split]);
        let mut read: SessionSnapshot =
            serde_json::from_str(&serde_json::to_string(&first).unwrap()).unwrap();
        for event in &events[split..] {
            apply_event_mut(&mut read, event).unwrap();
        }
        assert_eq!(read, whole, "split before event {}", split + 1);
    }
}
