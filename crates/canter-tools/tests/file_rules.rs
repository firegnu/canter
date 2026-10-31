use canter_protocol::{ApprovalDecision, PermissionMode};
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, PreparedCall, Tool, Tools};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

fn tools(root: &Path) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(
            root.into(),
            BTreeMap::from([("HOME".into(), root.to_string_lossy().into_owned())]),
            vec![],
        )
        .unwrap(),
    ))
}

async fn prepare(tools: &Tools, tool: Tool, path: &str) -> PreparedCall {
    let args = match tool {
        Tool::Read => json!({"path":path}),
        Tool::Write => json!({"path":path,"content":"new fake value"}),
        Tool::Edit => json!({"path":path,"edits":[{"oldText":"fake","newText":"new fake"}]}),
        _ => unreachable!(),
    };
    tools
        .prepare(tool, "file", &args.to_string(), &CancellationToken::new())
        .await
        .unwrap()
}

const MODES: [PermissionMode; 3] = [
    PermissionMode::AlwaysAsk,
    PermissionMode::AskForExecution,
    PermissionMode::AllowAll,
];

#[tokio::test]
async fn forbidden_file_rules_block_read_write_and_edit_in_every_mode() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("nested")).unwrap();
    std::fs::write(root.path().join("nested/.env"), "fake").unwrap();
    let rules = root.path().join("rules.json");
    std::fs::write(
        &rules,
        json!({"rules":[],"fileRules":[{"pattern":".env","decision":"forbidden"}]}).to_string(),
    )
    .unwrap();
    let gate = PermissionGate::default().with_rules_file(rules);
    let tools = tools(root.path());
    for tool in [Tool::Read, Tool::Write, Tool::Edit] {
        let call = prepare(&tools, tool, "nested/.env").await;
        for mode in MODES {
            let PermissionCheck::Forbidden { reason } = gate.evaluate(&call, mode) else {
                panic!("{tool:?} must be forbidden in {mode:?}");
            };
            assert!(
                reason.contains("user") && reason.contains("Do not"),
                "{reason}"
            );
        }
    }
}

#[tokio::test]
async fn prompt_file_rules_require_single_approval_despite_existing_session_grants() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("settings.env"), "fake").unwrap();
    let rules = root.path().join("rules.json");
    let tools = tools(root.path());
    for tool in [Tool::Read, Tool::Write, Tool::Edit] {
        std::fs::write(&rules, r#"{"rules":[]}"#).unwrap();
        let mut gate = PermissionGate::default().with_rules_file(rules.clone());
        let call = prepare(&tools, tool, "settings.env").await;
        gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
            .unwrap();
        std::fs::write(
            &rules,
            json!({"rules":[],"fileRules":[{"pattern":"*.env","decision":"prompt"}]}).to_string(),
        )
        .unwrap();
        for mode in [PermissionMode::AlwaysAsk, PermissionMode::AskForExecution] {
            let PermissionCheck::Ask(request) = gate.evaluate(&call, mode) else {
                panic!("{tool:?} must ask in {mode:?}");
            };
            assert_eq!(
                request.available_decisions,
                vec![
                    ApprovalDecision::Accept,
                    ApprovalDecision::Decline,
                    ApprovalDecision::DeclineWithFeedback,
                    ApprovalDecision::Cancel,
                ]
            );
            assert!(matches!(
                gate.resolve(&call, ApprovalDecision::Accept, None),
                Ok(PermissionCheck::Execute(_))
            ));
            assert!(matches!(
                gate.evaluate(&call, mode),
                PermissionCheck::Ask(_)
            ));
        }
    }
}

#[tokio::test]
async fn file_rules_also_apply_to_read_in_plan_and_review() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".env"), "fake").unwrap();
    let rules = root.path().join("rules.json");
    let gate = PermissionGate::default().with_rules_file(rules.clone());
    let tools = tools(root.path());
    let call = prepare(&tools, Tool::Read, ".env").await;
    for decision in ["forbidden", "prompt"] {
        std::fs::write(
            &rules,
            json!({"rules":[],"fileRules":[{"pattern":".env","decision":decision}]}).to_string(),
        )
        .unwrap();
        for mode in MODES {
            for check in [
                gate.evaluate_plan(&call, mode),
                gate.evaluate_review(&call, mode),
            ] {
                match decision {
                    "forbidden" => assert!(matches!(check, PermissionCheck::Forbidden { .. })),
                    _ if mode == PermissionMode::AllowAll => {
                        assert!(matches!(check, PermissionCheck::Execute(_)))
                    }
                    _ => assert!(matches!(check, PermissionCheck::Ask(_))),
                }
            }
        }
    }
}

#[tokio::test]
async fn broken_rules_cannot_release_a_forbidden_file_in_allow_all() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".env"), "fake").unwrap();
    let rules = root.path().join("rules.json");
    let gate = PermissionGate::default().with_rules_file(rules.clone());
    let tools = tools(root.path());
    let call = prepare(&tools, Tool::Read, ".env").await;
    let valid =
        json!({"rules":[],"fileRules":[{"pattern":".env","decision":"forbidden"}]}).to_string();
    std::fs::write(&rules, &valid).unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Forbidden { .. }
    ));
    for broken in [
        format!("{valid} unfinished"),
        json!({"rules":[{"pattern":[],"decision":"allow"}],"fileRules":[{"pattern":".env","decision":"forbidden"}]}).to_string(),
        json!({"rules":[],"fileRules":[{"pattern":"[","decision":"forbidden"}]}).to_string(),
    ] {
        std::fs::write(&rules, broken).unwrap();
        for gate in [&gate, &PermissionGate::default().with_rules_file(rules.clone())] {
            assert!(matches!(gate.evaluate(&call, PermissionMode::AllowAll), PermissionCheck::Forbidden { .. }));
            for mode in [PermissionMode::AlwaysAsk, PermissionMode::AskForExecution] {
                let PermissionCheck::Ask(request) = gate.evaluate(&call, mode) else { panic!("broken rules must ask"); };
                assert!(!request.available_decisions.contains(&ApprovalDecision::AcceptForSession));
            }
        }
    }
    assert!(gate.take_rules_warning().is_some());
    assert!(gate.take_rules_warning().is_none());
    std::fs::remove_file(&rules).unwrap();
    std::fs::create_dir(&rules).unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Forbidden { .. }
    ));
    std::fs::remove_dir(&rules).unwrap();
    std::fs::write(&rules, r#"{"rules":[]}"#).unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Execute(_)
    ));
}

#[tokio::test]
async fn prompt_file_rules_allow_all_without_approval_including_image_reads() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("private.png"), "fake image fixture").unwrap();
    let rules = root.path().join("rules.json");
    std::fs::write(
        &rules,
        json!({"rules":[],"fileRules":[{"pattern":"*.png","decision":"prompt"}]}).to_string(),
    )
    .unwrap();
    let gate = PermissionGate::default().with_rules_file(rules);
    let tools = tools(root.path());
    for tool in [Tool::Read, Tool::Write, Tool::Edit] {
        let call = prepare(&tools, tool, "private.png").await;
        assert!(matches!(
            gate.evaluate(&call, PermissionMode::AllowAll),
            PermissionCheck::Execute(_)
        ));
    }
}

#[tokio::test]
async fn missing_or_empty_file_rules_preserve_existing_file_permissions() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".env"), "fake").unwrap();
    let rules = root.path().join("rules.json");
    let tools = tools(root.path());
    for contents in [
        None,
        Some(r#"{"rules":[]}"#),
        Some(r#"{"rules":[],"fileRules":[]}"#),
    ] {
        if let Some(contents) = contents {
            std::fs::write(&rules, contents).unwrap();
        }
        let gate = PermissionGate::default().with_rules_file(rules.clone());
        for tool in [Tool::Read, Tool::Write, Tool::Edit] {
            let call = prepare(&tools, tool, ".env").await;
            for mode in MODES {
                assert_eq!(
                    gate.evaluate(&call, mode),
                    PermissionGate::default().evaluate(&call, mode)
                );
            }
        }
    }
}

#[tokio::test]
async fn symlinks_to_ruled_files_match_names_and_resolved_paths() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("secrets/nested")).unwrap();
    std::fs::write(root.path().join("secrets/nested/.env.local"), "fake").unwrap();
    std::os::unix::fs::symlink(
        root.path().join("secrets/nested/.env.local"),
        root.path().join("linked"),
    )
    .unwrap();
    let rules = root.path().join("rules.json");
    let gate = PermissionGate::default().with_rules_file(rules.clone());
    let tools = tools(root.path());
    for pattern in [
        ".env.*".to_owned(),
        "./secrets/**".into(),
        "secrets/**/*.local".into(),
        "~/secrets/**/.env.*".into(),
        root.path()
            .join("secrets/nested/.env.local")
            .display()
            .to_string(),
    ] {
        std::fs::write(
            &rules,
            json!({"rules":[],"fileRules":[{"pattern":pattern,"decision":"forbidden"}]})
                .to_string(),
        )
        .unwrap();
        for tool in [Tool::Read, Tool::Write, Tool::Edit] {
            let call = prepare(&tools, tool, "linked").await;
            assert!(
                matches!(
                    gate.evaluate(&call, PermissionMode::AllowAll),
                    PermissionCheck::Forbidden { .. }
                ),
                "{pattern}"
            );
        }
    }
}

#[tokio::test]
async fn forbidden_wins_over_prompt_regardless_of_rule_order() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join(".env"), "fake").unwrap();
    let rules = root.path().join("rules.json");
    let tools = tools(root.path());
    let mut gate = PermissionGate::default().with_rules_file(rules.clone());
    let call = prepare(&tools, Tool::Read, ".env").await;
    for decisions in [["prompt", "forbidden"], ["forbidden", "prompt"]] {
        std::fs::write(
            &rules,
            json!({"rules":[],"fileRules":[
                {"pattern":".env","decision":decisions[0]},
                {"pattern":"./.env","decision":decisions[1]},
            ]})
            .to_string(),
        )
        .unwrap();
        for mode in MODES {
            assert!(matches!(
                gate.evaluate(&call, mode),
                PermissionCheck::Forbidden { .. }
            ));
        }
        assert!(matches!(
            gate.resolve(&call, ApprovalDecision::Accept, None),
            Ok(PermissionCheck::Forbidden { .. })
        ));
    }
}
