use canter_protocol::*;
use serde_json::json;

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
