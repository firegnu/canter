use canter_protocol::{ApprovalDecision, PermissionMode};
use canter_tools::{Executor, LocalExecutor, PermissionCheck, PermissionGate, Tool, Tools};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn cd_home_before_newline_preserves_path_checks_and_unresolved_tilde_asks() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let cwd = home.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let executor = Arc::new(
        LocalExecutor::new(
            cwd,
            BTreeMap::from([("HOME".into(), home.to_string_lossy().into_owned())]),
            vec![home.join("private")],
        )
        .unwrap(),
    );
    assert!(executor.is_allowed_shell("cd ~\npwd", &|_| false));
    let tools = Tools::new(executor.clone());
    let gate = PermissionGate::default();
    let cancel = CancellationToken::new();
    for command in ["cd ~\ncat private/config.json", "cd '~' && pwd"] {
        assert!(!executor.is_allowed_shell(command, &|_| false), "{command}");
        let call = tools
            .prepare(
                Tool::Shell,
                "home",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        for mode in [PermissionMode::AlwaysAsk, PermissionMode::AskForExecution] {
            let PermissionCheck::Ask(request) = gate.evaluate(&call, mode) else {
                panic!("{command}: {mode:?}")
            };
            assert!(request.session_prefixes.is_none());
            assert!(request.proposed_execpolicy_amendment.is_none());
        }
    }
}

#[tokio::test]
async fn proposals_exclude_command_wrappers_and_keep_relative_programs_session_only() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let gate = PermissionGate::default().with_rules_file(file.clone());
    let cancel = CancellationToken::new();
    let cases = [
        ("uv run script.py", "uv run", false, false),
        ("npm exec tsc", "npm exec", false, false),
        ("pnpm exec tsc", "pnpm exec", false, false),
        ("yarn exec tsc", "yarn exec", false, false),
        ("pnpm dlx prettier", "pnpm dlx", false, false),
        ("yarn dlx prettier", "yarn dlx", false, false),
        ("rustup run stable cargo test", "rustup run", false, false),
        ("docker run alpine", "docker run", false, false),
        ("docker exec worker ls", "docker exec", false, false),
        ("kubectl exec pod -- ls", "kubectl exec", false, false),
        ("find --help", "find", false, false),
        ("sed --help", "sed", false, false),
        ("/usr/bin/find", "/usr/bin/find", false, false),
        ("/usr/bin/sed", "/usr/bin/sed", false, false),
        ("./build.sh", "./build.sh", true, false),
        ("../build.sh", "../build.sh", true, false),
        ("scripts/build.sh", "scripts/build.sh", true, false),
        ("/opt/bin/build.sh", "/opt/bin/build.sh", true, true),
        ("cargo test", "cargo test", true, true),
    ];
    for (command, prefix, session, permanent) in cases {
        let prefixes = vec![prefix.split_whitespace().map(String::from).collect()];
        let call = tools
            .prepare(
                Tool::Shell,
                "proposal",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        let PermissionCheck::Ask(request) = gate.evaluate(&call, PermissionMode::AlwaysAsk) else {
            panic!("{command}")
        };
        assert_eq!(
            request.session_prefixes,
            session.then(|| prefixes.clone()),
            "{command}"
        );
        assert_eq!(
            request.proposed_execpolicy_amendment,
            permanent.then(|| prefixes.clone()),
            "{command}"
        );
        for (decision, expected) in [
            (ApprovalDecision::AcceptForSession, session),
            (ApprovalDecision::AcceptWithExecpolicyAmendment, permanent),
        ] {
            assert_eq!(
                request.available_decisions.contains(&decision),
                expected,
                "{command}"
            );
        }
        if !permanent {
            assert!(gate.save_rules(&prefixes).is_err(), "{command}");
            assert!(!file.exists());
        }
    }
    // Suggestion restrictions do not change the user's own file rules.
    for (command, prefix, _, _) in cases {
        std::fs::write(
            &file,
            serde_json::json!({"rules":[{
                "pattern":prefix.split_whitespace().collect::<Vec<_>>(), "decision":"allow"
            }]})
            .to_string(),
        )
        .unwrap();
        let call = tools
            .prepare(
                Tool::Shell,
                "manual-rule",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        assert!(
            matches!(
                gate.evaluate(&call, PermissionMode::AlwaysAsk),
                PermissionCheck::Execute(_)
            ),
            "{command}"
        );
    }
}

#[tokio::test]
async fn session_approval_only_grants_the_command_prefix() {
    let temp = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::Shell, "first", r#"{"command":"cargo test"}"#, &cancel)
        .await
        .unwrap();
    let mut gate = PermissionGate::default();
    gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
        .unwrap();
    for (command, allowed) in [
        ("cargo test --release", true),
        ("cargo testx", false),
        ("cargo build", false),
    ] {
        let next = tools
            .prepare(
                Tool::Shell,
                "later",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(
            matches!(
                gate.evaluate(&next, PermissionMode::AlwaysAsk),
                PermissionCheck::Execute(_)
            ),
            allowed,
            "{command}"
        );
    }
}

#[tokio::test]
async fn file_rules_match_words_and_take_the_strictest_decision_in_every_mode() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    std::fs::write(
        &file,
        r#"{"rules":[
        {"pattern":["cargo"],"decision":"allow"},
        {"pattern":["cargo","test"],"decision":"prompt"},
        {"pattern":["cargo","test","--release"],"decision":"forbidden"}
    ]}"#,
    )
    .unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let gate = PermissionGate::default().with_rules_file(file);
    let cancel = CancellationToken::new();
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
    ] {
        for (command, expected) in [
            ("cargo test --release", "deny"),
            (
                "cargo test",
                if mode == PermissionMode::AllowAll {
                    "allow"
                } else {
                    "ask"
                },
            ),
            ("cargo testx", "allow"),
        ] {
            let call = tools
                .prepare(
                    Tool::Shell,
                    "item",
                    &serde_json::json!({"command": command}).to_string(),
                    &cancel,
                )
                .await
                .unwrap();
            let check = gate.evaluate(&call, mode);
            let actual = match check {
                PermissionCheck::Execute(_) => "allow",
                PermissionCheck::Ask(_) => "ask",
                _ => "deny",
            };
            assert_eq!(actual, expected, "{command} {mode:?}");
        }
    }
}

#[tokio::test]
async fn rules_reload_and_a_broken_file_requires_single_approval_even_in_allow_all() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let gate = PermissionGate::default().with_rules_file(file.clone());
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::Shell, "item", r#"{"command":"pwd"}"#, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));
    for contents in [
        "broken",
        r#"{"rules":[{"pattern":[],"decision":"allow"}]}"#,
        r#"{"rules":[{"pattern":["pwd"],"decision":"unknown"}]}"#,
    ] {
        std::fs::write(&file, contents).unwrap();
        for mode in [
            PermissionMode::AlwaysAsk,
            PermissionMode::AskForExecution,
            PermissionMode::AllowAll,
        ] {
            let PermissionCheck::Ask(request) = gate.evaluate(&call, mode) else {
                panic!("broken rules must ask in {mode:?}")
            };
            assert!(
                !request
                    .available_decisions
                    .contains(&ApprovalDecision::AcceptForSession)
            );
        }
    }
    std::fs::write(
        &file,
        r#"{"rules":[{"pattern":["pwd"],"decision":"prompt"}]}"#,
    )
    .unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Execute(_)
    ));
    std::fs::write(&file, r#"{"rules":[]}"#).unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));
}

#[tokio::test]
async fn compound_commands_require_every_segment_and_unknown_syntax_still_asks() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    std::fs::write(
        &file,
        r#"{"rules":[
        {"pattern":["cargo","test"],"decision":"allow"},
        {"pattern":["cargo","publish"],"decision":"forbidden"},
        {"pattern":["git","status"],"decision":"prompt"}
    ]}"#,
    )
    .unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let gate = PermissionGate::default().with_rules_file(file);
    let cancel = CancellationToken::new();
    for (command, expected) in [
        ("cargo test && pwd", "allow"),
        ("pwd || cargo test", "allow"),
        ("cargo test; pwd", "allow"),
        ("cargo test | cat", "allow"),
        ("cargo test\npwd", "allow"),
        ("cargo test & pwd", "allow"),
        ("cargo test |& cat", "allow"),
        ("cargo test && cargo build", "ask"),
        ("cargo test && git status", "ask"),
        ("cargo test && cargo publish", "deny"),
        ("cargo test \"$TEST\"", "ask"),
        ("cargo test $(pwd)", "ask"),
        ("cargo test `pwd`", "ask"),
        ("cargo publish \"$PACKAGE\"", "deny"),
        ("printf \"$NAME\"; cargo publish", "deny"),
        ("git \"status\"", "ask"),
        ("/usr/bin/cargo test", "ask"),
        ("cargo te\\st", "allow"),
        ("   ", "ask"),
    ] {
        let call = tools
            .prepare(
                Tool::Shell,
                "item",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        let actual = match gate.evaluate(&call, PermissionMode::AlwaysAsk) {
            PermissionCheck::Execute(_) => "allow",
            PermissionCheck::Ask(_) => "ask",
            _ => "deny",
        };
        assert_eq!(actual, expected, "{command}");
    }
}

#[tokio::test]
async fn proposals_show_only_needed_prefixes_and_never_offer_broad_or_prompt_rules() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    std::fs::write(
        &file,
        r#"{"rules":[{"pattern":["git","status"],"decision":"prompt"}]}"#,
    )
    .unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let gate = PermissionGate::default().with_rules_file(file);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "item",
            r#"{"command":"pwd && cargo test && git \"commit\" -m done"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let PermissionCheck::Ask(request) = gate.evaluate(&call, PermissionMode::AlwaysAsk) else {
        panic!()
    };
    let prefixes = vec![
        vec!["cargo".to_string(), "test".into()],
        vec!["git".into(), "commit".into()],
    ];
    assert_eq!(
        request.proposed_execpolicy_amendment,
        Some(prefixes.clone())
    );
    assert_eq!(request.session_prefixes, Some(prefixes));
    assert!(
        request
            .available_decisions
            .contains(&ApprovalDecision::AcceptWithExecpolicyAmendment)
    );
    for command in [
        "bash -c pwd",
        "/bin/zsh -c pwd",
        "python3 script.py",
        "node script.js",
        "env cargo test",
        "sudo cargo test",
        "git",
        "npm run test",
        "git -C repo commit",
        "cargo +nightly test",
        "git status",
    ] {
        let call = tools
            .prepare(
                Tool::Shell,
                "item",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        let PermissionCheck::Ask(request) = gate.evaluate(&call, PermissionMode::AlwaysAsk) else {
            panic!("{command}")
        };
        assert!(request.proposed_execpolicy_amendment.is_none(), "{command}");
        assert!(request.session_prefixes.is_none(), "{command}");
        assert!(
            !request
                .available_decisions
                .contains(&ApprovalDecision::AcceptForSession),
            "{command}"
        );
    }
}

#[tokio::test]
async fn protected_paths_and_readonly_modes_take_priority_and_warnings_recover() {
    let temp = tempfile::tempdir().unwrap();
    let private = temp.path().join("private");
    std::fs::create_dir(&private).unwrap();
    let file = private.join("rules.json");
    std::fs::write(&file, r#"{"rules":[{"pattern":["cat"],"decision":"allow"},{"pattern":["touch"],"decision":"allow"}]}"#).unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![private.clone()]).unwrap(),
    ));
    let mut gate = PermissionGate::default().with_rules_file(file.clone());
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "private",
            r#"{"command":"cat private/rules.json"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let PermissionCheck::Ask(request) = gate.evaluate(&call, PermissionMode::AlwaysAsk) else {
        panic!()
    };
    assert!(request.session_prefixes.is_none());
    assert!(
        gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
            .is_err()
    );
    assert!(matches!(
        gate.resolve(&call, ApprovalDecision::Accept, None),
        Ok(PermissionCheck::Execute(_))
    ));
    let write = tools
        .prepare(
            Tool::Shell,
            "write",
            r#"{"command":"touch marker","background":true}"#,
            &cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        gate.evaluate(&write, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));
    assert!(matches!(
        gate.evaluate_plan(&write, PermissionMode::AllowAll),
        PermissionCheck::Deny { .. }
    ));
    assert!(matches!(
        gate.evaluate_review(&write, PermissionMode::AlwaysAsk),
        PermissionCheck::Deny { .. }
    ));
    std::fs::remove_file(&file).unwrap();
    std::fs::create_dir(&file).unwrap();
    for index in 0..2 {
        assert!(matches!(
            gate.evaluate(&write, PermissionMode::AllowAll),
            PermissionCheck::Ask(_)
        ));
        assert_eq!(gate.take_rules_warning().is_some(), index == 0);
    }
    std::fs::remove_dir(&file).unwrap();
    assert!(matches!(
        gate.evaluate(&write, PermissionMode::AllowAll),
        PermissionCheck::Execute(_)
    ));
    assert!(gate.take_rules_warning().is_none());
    std::fs::write(&file, "invalid").unwrap();
    assert!(matches!(
        gate.evaluate(&write, PermissionMode::AllowAll),
        PermissionCheck::Ask(_)
    ));
    assert!(
        gate.take_rules_warning()
            .unwrap()
            .contains(&file.display().to_string())
    );
}

#[tokio::test]
async fn absolute_executables_match_only_the_exact_program_word_and_prompt_overrides_session() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let mut gate = PermissionGate::default().with_rules_file(file.clone());
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "item",
            r#"{"command":"/usr/bin/git \"commit\" -m done"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let PermissionCheck::Ask(request) = gate.evaluate(&call, PermissionMode::AlwaysAsk) else {
        panic!()
    };
    assert_eq!(
        request.session_prefixes,
        Some(vec![vec!["/usr/bin/git".into(), "commit".into()]])
    );
    gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
        .unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));
    let bare = tools
        .prepare(
            Tool::Shell,
            "bare",
            r#"{"command":"git commit -m done"}"#,
            &cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        gate.evaluate(&bare, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    std::fs::write(
        &file,
        r#"{"rules":[{"pattern":["/usr/bin/git","commit"],"decision":"prompt"}]}"#,
    )
    .unwrap();
    let PermissionCheck::Ask(request) = gate.evaluate(&call, PermissionMode::AlwaysAsk) else {
        panic!()
    };
    assert!(request.session_prefixes.is_none());
    assert!(request.proposed_execpolicy_amendment.is_none());
    assert!(
        tools
            .prepare(Tool::Shell, "empty", r#"{"command":""}"#, &cancel)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn saving_multiple_prefixes_preserves_other_rules_and_serializes_session_writes() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    std::fs::write(
        &file,
        r#"{"rules":[{"pattern":["cargo","publish"],"decision":"forbidden"}]}"#,
    )
    .unwrap();
    let first = PermissionGate::default().with_rules_file(file.clone());
    let second = PermissionGate::default().with_rules_file(file.clone());
    std::thread::scope(|scope| {
        scope.spawn(|| {
            first
                .save_rules(&[
                    vec!["cargo".into(), "test".into()],
                    vec!["git".into(), "commit".into()],
                ])
                .unwrap()
        });
        scope.spawn(|| {
            second
                .save_rules(&[vec!["cargo".into(), "test".into()]])
                .unwrap()
        });
    });
    let rules: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(rules["rules"].as_array().unwrap().len(), 3);
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let cancel = CancellationToken::new();
    for command in [
        "cargo test --release",
        "git commit -m done",
        "cargo publish",
    ] {
        let call = tools
            .prepare(
                Tool::Shell,
                "item",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        if command == "cargo publish" {
            assert!(matches!(
                first.evaluate(&call, PermissionMode::AllowAll),
                PermissionCheck::Forbidden { .. }
            ));
        } else {
            assert!(matches!(
                first.evaluate(&call, PermissionMode::AlwaysAsk),
                PermissionCheck::Execute(_)
            ));
        }
    }
}

#[tokio::test]
async fn a_pending_session_approval_keeps_its_scope_if_another_session_already_saved_the_rule() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("rules.json");
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let mut gate = PermissionGate::default().with_rules_file(file.clone());
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::Shell, "item", r#"{"command":"cargo test"}"#, &cancel)
        .await
        .unwrap();
    let PermissionCheck::Ask(request) = gate.evaluate(&call, PermissionMode::AlwaysAsk) else {
        panic!()
    };
    std::fs::write(
        &file,
        r#"{"rules":[{"pattern":["cargo","test"],"decision":"allow"}]}"#,
    )
    .unwrap();
    assert!(matches!(
        gate.resolve_with_prefixes(
            &call,
            ApprovalDecision::AcceptForSession,
            None,
            request.session_prefixes.as_deref()
        ),
        Ok(PermissionCheck::Execute(_))
    ));
    std::fs::remove_file(file).unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));
}
