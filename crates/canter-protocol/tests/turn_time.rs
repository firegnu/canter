//! Turn and thinking times, filled by the reducer from the events' own
//! timestamps: a turn's `startedAt`, `completedAt` (Unix seconds) and
//! `durationMs`; a thinking item's `durationMs`, up to its last delta.

use canter_protocol::*;
use serde_json::{Value, json};

const SESSION: &str = "s1";
/// Unix milliseconds of the turn's start.
const T0: i64 = 1_800_000_000_250;

fn event(seq: u64, timestamp: i64, payload: Value) -> Event {
    let mut event = json!({ "seq": seq, "sessionId": SESSION, "timestamp": timestamp });
    event
        .as_object_mut()
        .unwrap()
        .extend(payload.as_object().unwrap().clone());
    serde_json::from_value(event).unwrap()
}

/// Events at the given timestamps, numbered from `first`.
fn numbered(first: u64, events: Vec<(i64, Value)>) -> Vec<Event> {
    events
        .into_iter()
        .zip(first..)
        .map(|((timestamp, payload), seq)| event(seq, timestamp, payload))
        .collect()
}

fn turn(id: &str, status: &str) -> Value {
    json!({
        "id": id, "items": [], "status": status, "error": null,
        "input": [{ "type": "text", "text": "do it" }],
        "clientName": "terminal", "usage": null,
    })
}

fn thinking(id: &str, content: &[&str]) -> Value {
    json!({ "type": "reasoning", "id": id, "summary": [], "content": content })
}

fn think(item: &str, delta: &str) -> Value {
    json!({
        "type": "item.delta", "turnId": "t1", "itemId": item,
        "delta": { "type": "reasoningContent", "contentIndex": 0, "delta": delta },
    })
}

/// A turn that thinks for 4 s, answers, and ends 12.345 s after it started;
/// then a warning after the turn.
fn session() -> Vec<Event> {
    let message = |text: &str| json!({ "type": "agentMessage", "id": "m1", "text": text });
    numbered(
        1,
        vec![
            (
                T0 - 5_000,
                json!({ "type": "session.updated", "settings": {
                    "cwd": "/work", "name": null, "model": "m", "approvalPolicy": "alwaysAsk",
                }}),
            ),
            (T0, json!({ "type": "turn.started", "turn": turn("t1", "inProgress") })),
            (T0, json!({ "type": "session.status", "status": "working" })),
            (
                T0 + 100,
                json!({ "type": "item.started", "turnId": "t1", "item": thinking("r1", &[]) }),
            ),
            (T0 + 100, think("r1", "Look")),
            (T0 + 2_100, think("r1", " at")),
            (T0 + 4_100, think("r1", " it.")),
            (
                T0 + 4_120,
                json!({ "type": "item.started", "turnId": "t1", "item": message("") }),
            ),
            (
                T0 + 6_000,
                json!({
                    "type": "item.delta", "turnId": "t1", "itemId": "m1",
                    "delta": { "type": "agentMessage", "delta": "Done." },
                }),
            ),
            (
                T0 + 9_000,
                json!({ "type": "item.completed", "turnId": "t1", "item": message("Done.") }),
            ),
            (
                T0 + 9_000,
                json!({
                    "type": "item.completed", "turnId": "t1",
                    "item": thinking("r1", &["Look at it."]),
                }),
            ),
            (
                T0 + 12_345,
                json!({ "type": "turn.completed", "turn": turn("t1", "completed") }),
            ),
            (T0 + 12_345, json!({ "type": "session.status", "status": "idle" })),
            (T0 + 20_000, json!({ "type": "warning", "message": "later" })),
        ],
    )
}

fn replay(snapshot: &mut SessionSnapshot, events: &[Event]) {
    for event in events {
        apply_event_mut(snapshot, event).unwrap();
    }
}

fn wire(snapshot: &SessionSnapshot) -> Value {
    serde_json::to_value(snapshot).unwrap()
}

fn assert_times(snapshot: &SessionSnapshot) {
    let turn = &wire(snapshot)["turns"][0];
    assert_eq!(turn["startedAt"], json!(1_800_000_000), "{turn}");
    assert_eq!(turn["completedAt"], json!(1_800_000_012), "{turn}");
    assert_eq!(turn["durationMs"], json!(12_345), "{turn}");
    let thought = &turn["items"][0];
    assert_eq!(thought["type"], "reasoning");
    assert_eq!(
        thought["durationMs"],
        json!(4_000),
        "thinking ends at its last delta, not at its item.completed: {thought}"
    );
    assert_eq!(turn["items"][1].get("durationMs"), None, "{turn}");
}

#[test]
fn turn_and_thinking_times_come_from_the_event_timestamps() {
    let mut snapshot = SessionSnapshot::empty(SESSION);
    replay(&mut snapshot, &session());
    assert_times(&snapshot);
}

/// A clock set back between two events adds nothing for that step.
#[test]
fn a_clock_set_back_counts_as_no_time() {
    let mut snapshot = SessionSnapshot::empty(SESSION);
    let events = numbered(
        1,
        vec![
            (10_000, json!({ "type": "turn.started", "turn": turn("t1", "inProgress") })),
            (
                10_000,
                json!({ "type": "item.started", "turnId": "t1", "item": thinking("r1", &[]) }),
            ),
            (13_000, think("r1", "a")),
            (12_000, think("r1", "b")),
            (14_000, think("r1", "c")),
            (
                15_000,
                json!({ "type": "turn.completed", "turn": turn("t1", "completed") }),
            ),
        ],
    );
    replay(&mut snapshot, &events);
    let turn = &wire(&snapshot)["turns"][0];
    assert_eq!(turn["durationMs"], json!(6_000), "{turn}");
    assert_eq!(turn["items"][0]["durationMs"], json!(5_000), "{turn}");
}

/// The snapshot a client reads part way through (over the wire), then the
/// rest of the events, gives the same snapshot as one pass: as after a
/// reconnect, or opening a session while a turn runs.
#[test]
fn replaying_in_two_parts_matches_one_pass() {
    let events = session();
    let mut whole = SessionSnapshot::empty(SESSION);
    replay(&mut whole, &events);
    assert_times(&whole);
    for split in 1..events.len() {
        let mut first = SessionSnapshot::empty(SESSION);
        replay(&mut first, &events[..split]);
        let mut read: SessionSnapshot =
            serde_json::from_str(&serde_json::to_string(&first).unwrap()).unwrap();
        replay(&mut read, &events[split..]);
        assert_eq!(read, whole, "split before event {}", split + 1);
    }
}

/// A snapshot from before these fields reads as before, and writes back
/// unchanged; its turns' times stay unknown, while a new turn gets them.
#[test]
fn old_snapshots_without_times_still_read() {
    let old = json!({
        "id": SESSION,
        "settings": {
            "cwd": "/work", "name": null, "model": "m",
            "reasoningEffort": "high", "approvalPolicy": "alwaysAsk",
        },
        "status": "working",
        "createdAt": 1_000,
        "updatedAt": 5_000,
        "turns": [
            {
                "id": "t0", "status": "completed", "error": null,
                "items": [{ "type": "reasoning", "id": "r0", "summary": [], "content": ["old"] }],
                "input": [], "clientName": "terminal", "usage": null,
            },
            {
                "id": "t1", "status": "inProgress", "error": null,
                "items": [{ "type": "reasoning", "id": "r1", "summary": [], "content": ["so"] }],
                "input": [], "clientName": "terminal", "usage": null,
            },
        ],
        "pendingRequests": [],
        "queuedInputs": [],
        "tokenUsage": null,
        "error": null,
        "lastEventSeq": 9,
    });
    let mut snapshot: SessionSnapshot = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(wire(&snapshot), old);

    let events = numbered(
        10,
        vec![
            (6_000, think("r1", " on")),
            (
                7_000,
                json!({ "type": "turn.completed", "turn": turn("t1", "completed") }),
            ),
            (8_000, json!({ "type": "turn.started", "turn": turn("t2", "inProgress") })),
            (
                9_500,
                json!({ "type": "turn.completed", "turn": turn("t2", "completed") }),
            ),
        ],
    );
    replay(&mut snapshot, &events);
    let turns = wire(&snapshot)["turns"].clone();
    for old_turn in [&turns[0], &turns[1]] {
        assert_eq!(old_turn.get("startedAt"), None, "{old_turn}");
        assert_eq!(old_turn.get("durationMs"), None, "{old_turn}");
        assert_eq!(old_turn["items"][0].get("durationMs"), None, "{old_turn}");
    }
    assert_eq!(turns[2]["startedAt"], json!(8), "{}", turns[2]);
    assert_eq!(turns[2]["completedAt"], json!(9), "{}", turns[2]);
    assert_eq!(turns[2]["durationMs"], json!(1_500), "{}", turns[2]);
}
