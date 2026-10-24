use canter_protocol::{ApprovalDecision, PermissionMode};
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, Tools};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn permission_modes_use_tool_risk_and_unknown_means_always_ask() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file"), "original\n").unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new()).unwrap(),
    ));
    let cancel = CancellationToken::new();
    let gate = PermissionGate::default();
    for (tool, raw) in [
        (Tool::Read, r#"{"path":"file"}"#),
        (Tool::Write, r#"{"path":"file","content":"replacement\n"}"#),
        (
            Tool::Edit,
            r#"{"path":"file","edits":[{"oldText":"original","newText":"edited"}]}"#,
        ),
        (Tool::Shell, r#"{"command":"printf 'complete command'"}"#),
    ] {
        let call = tools.prepare(tool, "item", raw, &cancel).await.unwrap();
        for (mode, ask) in [
            (PermissionMode::AlwaysAsk, tool != Tool::Read),
            (PermissionMode::AskForExecution, tool == Tool::Shell),
            (PermissionMode::AllowAll, false),
            (PermissionMode::Unknown, tool != Tool::Read),
        ] {
            match gate.evaluate(&call, mode) {
                PermissionCheck::Ask(request) => {
                    assert!(ask, "{tool:?} {mode:?}");
                    assert_eq!(request.item_id, "item");
                    assert_eq!(request.tool, tool.name());
                    assert_eq!(request.arguments, *call.approval_arguments());
                    assert_eq!(
                        request.available_decisions,
                        vec![
                            ApprovalDecision::Accept,
                            ApprovalDecision::AcceptForSession,
                            ApprovalDecision::Decline,
                            ApprovalDecision::Cancel
                        ]
                    );
                    if tool == Tool::Write || tool == Tool::Edit {
                        assert!(
                            request.arguments["diff"]
                                .as_str()
                                .unwrap()
                                .contains("-original")
                        );
                    }
                }
                PermissionCheck::Execute => assert!(!ask, "{tool:?} {mode:?} must ask"),
                other => panic!("unexpected permission decision: {other:?}"),
            }
        }
    }
}

#[tokio::test]
async fn approval_answers_and_grants_are_scoped_to_one_session_and_one_tool() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new()).unwrap(),
    ));
    let cancel = CancellationToken::new();
    let shell = tools
        .prepare(Tool::Shell, "s1", r#"{"command":"printf first"}"#, &cancel)
        .await
        .unwrap();
    let another_shell = tools
        .prepare(
            Tool::Shell,
            "s2",
            r#"{"command":"printf different"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let write = tools
        .prepare(
            Tool::Write,
            "w1",
            r#"{"path":"file","content":"text"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let mut first_session = PermissionGate::default();
    assert_eq!(
        first_session.resolve(&shell, ApprovalDecision::Decline),
        Ok(PermissionCheck::Deny { cancel_turn: false })
    );
    assert_eq!(
        first_session.resolve(&shell, ApprovalDecision::Cancel),
        Ok(PermissionCheck::Deny { cancel_turn: true })
    );
    assert!(
        first_session
            .resolve(&shell, ApprovalDecision::Unknown)
            .is_err()
    );
    assert_eq!(
        first_session.resolve(&shell, ApprovalDecision::Accept),
        Ok(PermissionCheck::Execute)
    );
    assert!(matches!(
        first_session.evaluate(&another_shell, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    assert_eq!(
        first_session.resolve(&shell, ApprovalDecision::AcceptForSession),
        Ok(PermissionCheck::Execute)
    );
    assert_eq!(
        first_session.evaluate(&another_shell, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute
    );
    assert!(matches!(
        first_session.evaluate(&write, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    let other_session = PermissionGate::default();
    assert!(matches!(
        other_session.evaluate(&another_shell, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    drop(first_session);
    let restarted_session = PermissionGate::default();
    assert!(matches!(
        restarted_session.evaluate(&shell, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
}
