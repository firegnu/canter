use canter_protocol::*;
use serde_json::{Value, json};

fn apply(snapshot: &mut SessionSnapshot, payload: Value) {
    let mut value = json!({"seq": snapshot.last_event_seq + 1,
        "sessionId": snapshot.id, "timestamp": 1000 + snapshot.last_event_seq});
    value
        .as_object_mut()
        .unwrap()
        .extend(payload.as_object().unwrap().clone());
    apply_event_mut(snapshot, &serde_json::from_value(value).unwrap()).unwrap();
}

fn turn(id: &str, status: &str) -> Value {
    json!({"id":id,"status":status,"input":[],"clientName":"agent","items":[],"error":null,"usage":null})
}

#[test]
fn child_metadata_and_latest_result_survive_replay_and_serialization() {
    let mut snapshot = SessionSnapshot::empty("child");
    apply(
        &mut snapshot,
        json!({"type":"session.parent","parent":{
        "sessionId":"parent","turnId":"parent-turn","task":"Inspect the parser"}}),
    );
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap()["parent"]["task"],
        "Inspect the parser"
    );
    apply(
        &mut snapshot,
        json!({"type":"turn.started","turn":turn("t1", "inProgress")}),
    );
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap()["agent"]["status"],
        "running"
    );
    for (id, text) in [("comment", "Looking"), ("final", "Parser is sound")] {
        apply(
            &mut snapshot,
            json!({"type":"item.completed","turnId":"t1",
            "item":{"type":"agentMessage","id":id,"text":text}}),
        );
    }
    apply(
        &mut snapshot,
        json!({"type":"turn.completed","turn":turn("t1", "completed")}),
    );
    let saved = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        saved["agent"],
        json!({"status":"completed","result":"Parser is sound"})
    );
    assert_eq!(
        serde_json::from_value::<SessionSnapshot>(saved).unwrap(),
        snapshot
    );
    apply(
        &mut snapshot,
        json!({"type":"turn.started","turn":turn("t2", "inProgress")}),
    );
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap()["agent"],
        json!({"status":"running","result":null})
    );
}

#[test]
fn child_states_follow_failed_interrupted_closed_and_new_turns() {
    let mut snapshot = SessionSnapshot::empty("child");
    apply(
        &mut snapshot,
        json!({"type":"session.parent","parent":{"sessionId":"parent","turnId":"t","task":"Inspect"}}),
    );
    for (id, status, expected) in [
        ("failed", "failed", "failed"),
        ("stopped", "interrupted", "interrupted"),
    ] {
        apply(
            &mut snapshot,
            json!({"type":"turn.started","turn":turn(id,"inProgress")}),
        );
        apply(
            &mut snapshot,
            json!({"type":"turn.completed","turn":turn(id,status)}),
        );
        assert_eq!(
            serde_json::to_value(&snapshot).unwrap()["agent"]["status"],
            expected
        );
    }
    apply(
        &mut snapshot,
        json!({"type":"session.status","status":"closed","closeReason":"clientRequest"}),
    );
    assert_eq!(snapshot.agent.as_ref().unwrap().status, AgentStatus::Closed);
    apply(
        &mut snapshot,
        json!({"type":"session.status","status":"idle"}),
    );
    assert_eq!(snapshot.agent.as_ref().unwrap().status, AgentStatus::Closed);
    apply(
        &mut snapshot,
        json!({"type":"turn.started","turn":turn("next","inProgress")}),
    );
    assert_eq!(
        snapshot.agent.as_ref().unwrap().status,
        AgentStatus::Running
    );
    apply(
        &mut snapshot,
        json!({"type":"session.status","status":"closed","closeReason":"serverShutdown"}),
    );
    assert_eq!(
        snapshot.agent.as_ref().unwrap().status,
        AgentStatus::Interrupted
    );
}

#[test]
fn old_snapshots_and_summaries_remain_ordinary_sessions() {
    let snapshot = SessionSnapshot::empty("legacy");
    let mut saved = serde_json::to_value(&snapshot).unwrap();
    saved.as_object_mut().unwrap().remove("parent");
    saved.as_object_mut().unwrap().remove("agent");
    let restored: SessionSnapshot = serde_json::from_value(saved).unwrap();
    assert!(restored.parent.is_none() && restored.agent.is_none());
    let summary: SessionSummary =
        serde_json::from_value(serde_json::to_value(restored.summary()).unwrap()).unwrap();
    assert!(summary.parent.is_none());
}
