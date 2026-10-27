use canter_protocol::*;
use serde_json::json;

#[test]
fn decline_feedback_round_trips_without_losing_the_users_words() {
    let params = json!({
        "decision": "declineWithFeedback", "feedback": "  改用只读命令\n不要写文件  ",
        "clientName": "terminal-a"
    });
    let decoded: RequestResolveParams = serde_json::from_value(params.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), params);

    let event = json!({
        "seq": 1, "sessionId": "s1", "timestamp": 20,
        "type": "request.resolved", "requestId": "r1",
        "resolution": {"type": "answered", "decision": "declineWithFeedback",
            "feedback": "  改用只读命令\n不要写文件  "}, "clientName": "terminal-a"
    });
    let decoded: Event = serde_json::from_value(event.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), event);
}

#[test]
fn legacy_resolution_and_unknown_decisions_remain_readable() {
    let event = json!({
        "seq": 1, "sessionId": "s1", "timestamp": 20,
        "type": "request.resolved", "requestId": "r1",
        "resolution": {"type": "answered", "decision": "decline"}, "clientName": null
    });
    let decoded: Event = serde_json::from_value(event.clone()).unwrap();
    assert_eq!(serde_json::to_value(decoded).unwrap(), event);
    assert_eq!(
        serde_json::from_value::<ApprovalDecision>(json!("futureDecision")).unwrap(),
        ApprovalDecision::Unknown
    );
}

#[test]
fn approval_feedback_follows_items_when_rewinding_or_retracting() {
    let turn = |id: &str| -> Turn {
        serde_json::from_value(json!({
            "id": id, "items": [], "status": "completed", "error": null,
            "input": [], "clientName": "terminal", "usage": null
        }))
        .unwrap()
    };
    let mut initial = SessionSnapshot::empty("s1");
    for (turn_id, item_id, feedback) in
        [("t1", "i1", "keep this"), ("t2", "i2", "remove with item")]
    {
        let mut turn = turn(turn_id);
        turn.items.push(
            serde_json::from_value(json!({
                "type": "commandExecution", "id": item_id, "command": "touch file", "cwd": "/work",
                "status": "declined", "aggregatedOutput": "", "exitCode": null, "durationMs": null
            }))
            .unwrap(),
        );
        initial.turns.push(turn);
        initial
            .approval_feedback
            .insert(item_id.into(), feedback.into());
    }
    for (payload, removes_item) in [
        (
            EventPayload::SessionRewound {
                before_turn_id: "t2".into(),
                rewind_conversation: true,
                record: turn("record"),
            },
            true,
        ),
        (
            EventPayload::SessionRewound {
                before_turn_id: "t2".into(),
                rewind_conversation: false,
                record: turn("record"),
            },
            false,
        ),
        (
            EventPayload::TurnRetracted {
                turn_id: "t2".into(),
            },
            true,
        ),
    ] {
        let event = Event {
            seq: 1,
            session_id: "s1".into(),
            timestamp: 20,
            payload,
        };
        let snapshot = apply_event(&initial, &event).unwrap();
        let expected = if removes_item {
            [("i1".to_owned(), "keep this".to_owned())].into()
        } else {
            initial.approval_feedback.clone()
        };
        assert_eq!(snapshot.approval_feedback, expected, "{:?}", event.payload);
        assert_eq!(
            snapshot.turns.iter().any(|turn| turn.id == "t2"),
            !removes_item
        );
    }
}

#[test]
fn resolving_an_already_processed_approval_is_idempotent() {
    let mut initial = SessionSnapshot::empty("s1");
    initial.pending_requests.push(PendingRequest {
        id: "r1".into(),
        turn_id: "t1".into(),
        created_at: 10,
        kind: RequestKind::Approval(ApprovalRequest {
            item_id: "i1".into(),
            tool: "shell".into(),
            arguments: json!({"command": "pwd"}),
            available_decisions: vec![
                ApprovalDecision::Accept,
                ApprovalDecision::AcceptForSession,
                ApprovalDecision::Decline,
                ApprovalDecision::Cancel,
            ],
        }),
    });
    let event = Event {
        seq: 1,
        session_id: "s1".into(),
        timestamp: 20,
        payload: EventPayload::RequestResolved {
            request_id: "r1".into(),
            resolution: ApprovalResolution::Answered {
                decision: ApprovalDecision::Accept,
                feedback: None,
            },
            client_name: Some("terminal-a".into()),
        },
    };
    let resolved = apply_event(&initial, &event).unwrap();
    assert!(resolved.pending_requests.is_empty());
    assert_eq!(resolved.last_event_seq, 1);
    assert_eq!(apply_event(&resolved, &event).unwrap(), resolved);
    let late_answer = Event {
        seq: 2,
        timestamp: 30,
        payload: EventPayload::RequestResolved {
            request_id: "r1".into(),
            resolution: ApprovalResolution::Answered {
                decision: ApprovalDecision::Decline,
                feedback: None,
            },
            client_name: Some("terminal-b".into()),
        },
        ..event
    };
    let mut expected = resolved.clone();
    expected.last_event_seq = 2;
    expected.updated_at = 30;
    assert_eq!(apply_event(&resolved, &late_answer).unwrap(), expected);
}
