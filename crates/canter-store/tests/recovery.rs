use std::fs;
use std::os::unix::fs::PermissionsExt;

use canter_protocol::*;
use canter_store::Store;
use serde_json::json;

fn settings() -> SessionSettings {
    SessionSettings {
        cwd: "/isolated/workspace".into(),
        name: Some("demo".into()),
        model: "fake-model".into(),
        approval_policy: PermissionMode::AlwaysAsk,
    }
}

#[test]
fn stored_session_reopens_identically_and_continues_sequence() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path().join("sessions")).unwrap();
    let mut writer = store.create("s1", settings(), 100).unwrap();
    assert!(
        !temp.path().join("sessions/s1/private.json").exists(),
        "session environment must never be persisted"
    );
    let payloads = json!([
        {"type":"session.status", "status":"working"},
        {"type":"turn.started", "turn":{"id":"t1", "items":[], "status":"inProgress", "error":null,
            "input":[{"type":"text","text":"hello"}], "clientName":"terminal-a", "usage":null}},
        {"type":"item.completed", "turnId":"t1", "item":{"type":"userMessage","id":"u1", "content":[{"type":"text","text":"hello"}]}},
        {"type":"item.started", "turnId":"t1", "item":{"type":"agentMessage","id":"a1", "text":""}},
        {"type":"item.delta", "turnId":"t1", "itemId":"a1", "delta":{"type":"agentMessage","delta":"hello back"}},
        {"type":"item.started", "turnId":"t1", "item":{"type":"reasoning","id":"r1", "summary":[],"content":[]}},
        {"type":"item.delta", "turnId":"t1", "itemId":"r1", "delta":{"type":"reasoningSummary","summaryIndex":0,"delta":"summary"}},
        {"type":"item.delta", "turnId":"t1", "itemId":"r1", "delta":{"type":"reasoningContent","contentIndex":0,"delta":"thought"}},
        {"type":"item.started", "turnId":"t1", "item":{"type":"commandExecution","id":"c1", "command":"printf hello", "cwd":"/tool/dir", "status":"inProgress", "aggregatedOutput":"", "exitCode":null, "durationMs":null}},
        {"type":"item.delta", "turnId":"t1", "itemId":"c1", "delta":{"type":"commandExecution","delta":"hello"}},
        {"type":"item.completed", "turnId":"t1", "item":{"type":"commandExecution","id":"c1", "command":"printf hello", "cwd":"/tool/dir", "status":"completed", "aggregatedOutput":"hello", "exitCode":0, "durationMs":12}},
        {"type":"item.completed", "turnId":"t1", "item":{"type":"fileChange","id":"f1", "changes":[{"path":"file.txt", "kind":{"type":"update","movePath":null}, "diff":"-old\n+new"}], "status":"completed"}},
        {"type":"item.completed", "turnId":"t1", "item":{"type":"fileRead","id":"read1", "path":"file.txt", "startLine":1,"content":"new", "status":"completed"}},
        {"type":"item.completed", "turnId":"t1", "item":{"type":"contextCompaction","id":"compact1", "summary":"conversation so far"}},
        {"type":"session.updated", "settings":{"cwd":"/isolated/workspace", "name":"renamed", "model":"fake-model-2", "approvalPolicy":"askForExecution"}},
        {"type":"request.opened", "request":{"id":"q1","turnId":"t1", "createdAt":116,"kind":{"type":"approval","itemId":"c2","tool":"shell", "arguments":{"command":"pwd"},"availableDecisions":["accept","acceptForSession","decline","cancel"]}}},
        {"type":"session.status", "status":"waiting_approval"},
        {"type":"session.usage", "tokenUsage":{"total":{"inputTokens":10,"cachedInputTokens":2,"outputTokens":5,"reasoningOutputTokens":1,"totalTokens":15}, "last":{"inputTokens":10,"cachedInputTokens":2,"outputTokens":5,"reasoningOutputTokens":1,"totalTokens":15},"contextTokens":20,"modelContextWindow":1000}}
    ]);
    for (index, payload) in payloads.as_array().unwrap().iter().enumerate() {
        writer
            .append(
                serde_json::from_value(payload.clone()).unwrap(),
                101 + index as i64,
            )
            .unwrap();
    }
    let expected = writer.snapshot().clone();
    assert_eq!(expected.settings.name.as_deref(), Some("renamed"));
    assert_eq!(expected.settings.model, "fake-model-2");
    assert_eq!(expected.status, SessionStatus::WaitingApproval);
    assert_eq!(expected.turns[0].items.len(), 7);
    assert_eq!(
        expected.turns[0].items[1],
        Item::AgentMessage {
            id: "a1".into(),
            text: "hello back".into()
        }
    );
    assert_eq!(
        expected.turns[0].items[2],
        Item::Reasoning {
            id: "r1".into(),
            summary: vec!["summary".into()],
            content: vec!["thought".into()]
        }
    );
    assert!(
        matches!(&expected.turns[0].items[3], Item::CommandExecution { aggregated_output, exit_code:Some(0), duration_ms:Some(12), .. } if aggregated_output == "hello")
    );
    let RequestKind::Approval(approval) = &expected.pending_requests[0].kind else {
        panic!("expected approval")
    };
    assert_eq!(approval.arguments, json!({"command":"pwd"}));
    assert_eq!(expected.token_usage.as_ref().unwrap().context_tokens, 20);
    assert_eq!(expected.last_event_seq, 19);
    drop(writer);

    let mut reopened = store.open("s1").unwrap();
    assert_eq!(reopened.snapshot(), &expected);
    assert_eq!(store.list().unwrap(), vec![expected.summary()]);
    let mut completed_turn = expected.turns[0].clone();
    completed_turn.status = TurnStatus::Completed;
    completed_turn.usage = expected.token_usage.clone();
    reopened
        .append(
            EventPayload::RequestResolved {
                request_id: "q1".into(),
                resolution: ApprovalResolution::Cancelled,
                client_name: None,
            },
            120,
        )
        .unwrap();
    let event = reopened
        .append(
            EventPayload::TurnCompleted {
                turn: completed_turn,
            },
            121,
        )
        .unwrap();
    assert_eq!(event.seq, 21);
    reopened
        .append(
            EventPayload::SessionStatus {
                status: SessionStatus::Idle,
            },
            122,
        )
        .unwrap();
    assert_eq!(reopened.snapshot().turns[0].status, TurnStatus::Completed);
    assert!(reopened.snapshot().pending_requests.is_empty());
    let completed = reopened.snapshot().clone();
    drop(reopened);
    assert_eq!(store.open("s1").unwrap().snapshot(), &completed);

    let dir = temp.path().join("sessions/s1");
    for (path, mode) in [
        (dir.clone(), 0o700),
        (dir.join("model-history.jsonl"), 0o600),
        (dir.join("events.jsonl"), 0o600),
    ] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            mode
        );
    }
    let events = fs::read_to_string(dir.join("events.jsonl")).unwrap();
    assert!(!events.contains("private-fixture"));
    assert!(events.contains("/isolated/workspace"));
}
