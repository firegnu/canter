use canter_protocol::{Event, EventPayload, Item, SessionSnapshot, SessionStatus, apply_event};
use serde_json::json;

#[test]
fn retry_items_are_skipped_by_legacy_readers_and_missing_status_stays_normal() {
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    enum LegacyItem {
        AgentMessage {
            text: String,
        },
        #[serde(other)]
        Unknown,
    }
    let retry = json!({"type":"modelRetry","id":"r1","error":{"message":"server error 503","additionalDetails":null},"willRetry":true,"attempt":3,"maxRetries":10,"delaySeconds":4.0});
    assert!(matches!(
        serde_json::from_value::<LegacyItem>(retry.clone()).unwrap(),
        LegacyItem::Unknown
    ));
    let parsed: Item = serde_json::from_value(retry.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), retry);
    let old = json!({"type":"agentMessage","id":"m1","text":"old answer"});
    let parsed: Item = serde_json::from_value(old.clone()).unwrap();
    assert!(matches!(parsed, Item::AgentMessage { status: None, .. }));
    assert_eq!(serde_json::to_value(parsed).unwrap(), old);
    let old_thinking = json!({"type":"reasoning","id":"r1","summary":[],"content":["old thought"]});
    assert!(matches!(
        serde_json::from_value::<Item>(old_thinking).unwrap(),
        Item::Reasoning { status: None, .. }
    ));
    let mut interrupted = old;
    interrupted["status"] = json!("interrupted");
    assert!(
        matches!(serde_json::from_value::<LegacyItem>(interrupted).unwrap(), LegacyItem::AgentMessage { text } if text == "old answer")
    );
}

#[test]
fn plan_fields_are_optional_and_legacy_clients_skip_plan_items() {
    // A minimal legacy view, using the protocol's existing unknown-item rule.
    #[derive(serde::Deserialize)]
    #[serde(tag = "type", rename_all = "camelCase")]
    enum LegacyItem {
        AgentMessage {
            text: String,
        },
        #[serde(other)]
        Unknown,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct LegacySettings {
        model: String,
    }
    let settings = json!({"cwd":"/work","name":null,"model":"fake","approvalPolicy":"alwaysAsk"});
    let old: canter_protocol::SessionSettings = serde_json::from_value(settings.clone()).unwrap();
    assert_eq!(old.plan_enabled, None);
    assert_eq!(serde_json::to_value(old).unwrap().get("planEnabled"), None);
    let mut new = settings;
    new["planEnabled"] = json!(false);
    assert_eq!(
        serde_json::from_value::<LegacySettings>(new).unwrap().model,
        "fake"
    );
    let value = json!({"type":"plan","id":"p1","plan":[{"step":"Read","status":"inProgress"}],"status":"completed"});
    assert!(matches!(
        serde_json::from_value::<LegacyItem>(value.clone()).unwrap(),
        LegacyItem::Unknown
    ));
    let parsed: Item = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), value);
    let following: LegacyItem =
        serde_json::from_value(json!({"type":"agentMessage","id":"m1","text":"still readable"}))
            .unwrap();
    assert!(matches!(following, LegacyItem::AgentMessage { text } if text == "still readable"));
    assert_eq!(
        serde_json::from_value::<canter_protocol::PlanStepStatus>(json!("futureStatus")).unwrap(),
        canter_protocol::PlanStepStatus::Unknown
    );
}

#[test]
fn command_source_defaults_to_agent_and_user_shell_round_trips() {
    use canter_protocol::ExecCommandSource;
    let old = json!({"type":"commandExecution","id":"command","command":"git status",
        "cwd":"/work","status":"completed","aggregatedOutput":"clean","exitCode":0,"durationMs":1});
    let item: Item = serde_json::from_value(old.clone()).unwrap();
    assert!(matches!(
        item,
        Item::CommandExecution {
            source: ExecCommandSource::Agent,
            ..
        }
    ));
    assert_eq!(serde_json::to_value(item).unwrap(), old);
    for (source, expected) in [
        ("agent", ExecCommandSource::Agent),
        ("userShell", ExecCommandSource::UserShell),
        ("futureSource", ExecCommandSource::Unknown),
    ] {
        let mut value = old.clone();
        value["source"] = json!(source);
        let item: Item = serde_json::from_value(value.clone()).unwrap();
        assert!(matches!(item, Item::CommandExecution { source, .. } if source == expected));
        if expected == ExecCommandSource::UserShell {
            assert_eq!(serde_json::to_value(item).unwrap(), value);
        }
    }
}

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
                close_reason: None,
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
            status: None,
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
    unknown::<SessionCloseReason>(json!("futureReason"), json!("unknown"));
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

#[test]
fn close_reasons_replay_and_legacy_events_and_snapshots_remain_readable() {
    let mut snapshot = SessionSnapshot::empty("s1");
    for (seq, status, reason, expected) in [
        (1, "closed", Some("clientRequest"), Some("clientRequest")),
        (2, "idle", None, None),
        (3, "closed", Some("serverShutdown"), Some("serverShutdown")),
        (4, "closed", Some("futureReason"), Some("unknown")),
        (5, "closed", None, None),
    ] {
        let mut value = json!({
            "seq": seq, "sessionId": "s1", "timestamp": seq,
            "type": "session.status", "status": status
        });
        if let Some(reason) = reason {
            value["closeReason"] = json!(reason);
        }
        let event: Event = serde_json::from_value(value).unwrap();
        snapshot = apply_event(&snapshot, &event).unwrap();
        let encoded = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(
            encoded.get("closeReason"),
            expected.map(|reason| json!(reason)).as_ref()
        );
        assert_eq!(encoded["status"], status);
        assert_eq!(
            serde_json::from_value::<SessionSnapshot>(encoded).unwrap(),
            snapshot
        );
        if reason.is_none() {
            assert!(
                serde_json::to_value(event)
                    .unwrap()
                    .get("closeReason")
                    .is_none()
            );
        }
    }
}
