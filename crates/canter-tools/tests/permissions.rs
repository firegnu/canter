use canter_protocol::{ApprovalDecision, PermissionMode};
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, Tools};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn file_changes_outside_the_working_directory_require_approval() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().join("main");
    std::fs::create_dir(&cwd).unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(cwd, BTreeMap::new(), vec![]).unwrap(),
    ));
    let gate = PermissionGate::default();
    let cancel = CancellationToken::new();
    for (path, ask) in [("new.txt", false), ("../outside.txt", true)] {
        let args = serde_json::json!({"path":path,"content":"new"}).to_string();
        let call = tools
            .prepare(Tool::Write, "write", &args, &cancel)
            .await
            .unwrap();
        assert_eq!(
            matches!(
                gate.evaluate(&call, PermissionMode::AskForExecution),
                PermissionCheck::Ask(_)
            ),
            ask,
            "{path}"
        );
    }
}

#[tokio::test]
async fn directory_boundary_resolves_paths_and_keeps_modes_and_tool_grants() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    for dir in ["main", "service", "outside"] {
        std::fs::create_dir(root.join(dir)).unwrap();
        std::fs::write(root.join(dir).join("file"), "old").unwrap();
    }
    std::os::unix::fs::symlink(root.join("outside"), root.join("main/link")).unwrap();
    std::os::unix::fs::symlink(root.join("service"), root.join("main/service-link")).unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(root.join("main"), BTreeMap::new(), vec![]).unwrap(),
    ));
    let mut gate = PermissionGate::default();
    gate.set_additional_directories(&[root.join("service").display().to_string()]);
    let cancel = CancellationToken::new();
    for (path, outside) in [
        ("file", false),
        ("../service/file", false),
        ("service-link/file", false),
        ("../outside/file", true),
        ("../main/../outside/file", true),
        ("link/file", true),
        ("link/../outside/file", true),
    ] {
        for tool in [Tool::Write, Tool::Edit] {
            let args = match tool {
                Tool::Write => serde_json::json!({"path":path,"content":"new"}),
                _ => serde_json::json!({"path":path,"edits":[{"oldText":"old","newText":"new"}]}),
            }
            .to_string();
            let call = tools.prepare(tool, "change", &args, &cancel).await.unwrap();
            for (mode, ask) in [
                (PermissionMode::AskForExecution, outside),
                (PermissionMode::AlwaysAsk, true),
                (PermissionMode::AllowAll, false),
            ] {
                assert_eq!(
                    matches!(gate.evaluate(&call, mode), PermissionCheck::Ask(_)),
                    ask,
                    "{tool:?} {path} {mode:?}"
                );
            }
            assert!(matches!(
                gate.evaluate_plan(&call, PermissionMode::AllowAll),
                PermissionCheck::Deny { .. }
            ));
            assert!(matches!(
                gate.evaluate_review(&call),
                PermissionCheck::Deny { .. }
            ));
        }
        let read = tools
            .prepare(
                Tool::Read,
                "read",
                &serde_json::json!({"path":path}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        assert!(matches!(
            gate.evaluate(&read, PermissionMode::AskForExecution),
            PermissionCheck::Execute(_)
        ));
    }
    for (path, outside) in [
        ("new/child/file", false),
        ("../service/new/child/file", false),
        ("link/new/child/file", true),
    ] {
        let call = tools
            .prepare(
                Tool::Write,
                "new",
                &serde_json::json!({"path":path,"content":"new"}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        assert_eq!(
            matches!(
                gate.evaluate(&call, PermissionMode::AskForExecution),
                PermissionCheck::Ask(_)
            ),
            outside,
            "{path}"
        );
    }
    let call = tools
        .prepare(
            Tool::Write,
            "grant",
            r#"{"path":"../outside/new","content":"new"}"#,
            &cancel,
        )
        .await
        .unwrap();
    gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
        .unwrap();
    let another = tools
        .prepare(
            Tool::Write,
            "later",
            r#"{"path":"../elsewhere","content":"new"}"#,
            &cancel,
        )
        .await
        .unwrap();
    for mode in [PermissionMode::AskForExecution, PermissionMode::AlwaysAsk] {
        assert!(matches!(
            gate.evaluate(&another, mode),
            PermissionCheck::Execute(_)
        ));
    }
    let edit = tools
        .prepare(
            Tool::Edit,
            "edit",
            r#"{"path":"../outside/file","edits":[{"oldText":"old","newText":"new"}]}"#,
            &cancel,
        )
        .await
        .unwrap();
    assert!(matches!(
        gate.evaluate(&edit, PermissionMode::AskForExecution),
        PermissionCheck::Ask(_)
    ));
    assert!(matches!(
        gate.evaluate_plan(&another, PermissionMode::AllowAll),
        PermissionCheck::Deny { .. }
    ));
    assert!(matches!(
        gate.evaluate_review(&another),
        PermissionCheck::Deny { .. }
    ));
}

#[tokio::test]
async fn readonly_shell_allowlist_applies_in_all_asking_modes() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            dir.path().into(),
            BTreeMap::from([
                ("HOME".into(), dir.path().to_str().unwrap().into()),
                ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ]),
            vec![],
        )
        .unwrap(),
    ));
    let gate = PermissionGate::default();
    let cancel = CancellationToken::new();
    for (commands, ask) in [
        (
            &[
                "ls",
                "ls -lah src",
                "cat -- README.md",
                "cat 'file with spaces'",
                "cat file\\ with\\ spaces",
                "head -n 20 README.md",
                "tail -c100 file",
                "wc -l file",
                "grep -nE 'foo|bar' file",
                "rg --files src",
                "rg -n -g '*.rs' needle src",
                "rg --glob='*.rs' needle src",
                "pwd",
                "git status --short",
                "git diff",
                "git diff --cached --stat",
                "git log --oneline -n 5",
                "git show HEAD:README.md",
                "ls && cat README.md",
                "grep needle file || cat fallback",
                "cat file | head -n 5; wc -l file",
                "cat a\" b\"' c'",
            ][..],
            false,
        ),
        (
            &[
                "rm file",
                "touch file",
                "npm install",
                "cargo build",
                "git commit -m x",
                "git checkout main",
                "sed -i '' file",
                "find . -delete",
                "find . -exec sh -c bad \\;",
                "rg --pre=sh needle",
                "rg --pre sh needle",
                "rg --hostname-bin=sh needle",
                "git diff --output=out",
                "git diff --output out",
                "git diff -U --output=out",
                "git diff --unified --output=out",
                "git log --pretty --output=out",
                "git show --format --output=out",
                "git log --output=out",
                "git show --ext-diff",
                "git -c alias.status=bad status",
                "git --paginate diff",
                "git diff --textconv",
                "git diff --no-index a b",
                "ls --unknown-option",
                "ls && rm file",
                "ls || rm file",
                "ls; rm file",
                "cat file | sh",
                "cat file > out",
                "cat file >> out",
                "cat file 2> out",
                "cat file < in",
                "cat file 2>&1",
                "cat <<EOF\ntext\nEOF",
                "ls &",
                "(ls)",
                "{ ls; }",
                "cat $(pwd)/file",
                "cat `pwd`/file",
                "cat <(ls)",
                "cat $HOME/file",
                "cat \"$HOME/file\"",
                "cat *.rs",
                "X=1 ls",
                "env ls",
                "sh -c ls",
                "./ls",
                "/tmp/ls",
                "ls\nrm file",
                "ls # comment",
                "ls &&",
                "ls ;; cat file",
                "cat 'unterminated",
                "cat file\\",
                "ls |& cat",
                "cd \"$SOMEWHERE\" && cat file",
            ][..],
            true,
        ),
    ] {
        for command in commands {
            let raw = serde_json::json!({"command":command}).to_string();
            let call = tools
                .prepare(Tool::Shell, "shell", &raw, &cancel)
                .await
                .unwrap();
            for mode in [
                PermissionMode::AlwaysAsk,
                PermissionMode::AskForExecution,
                PermissionMode::Unknown,
            ] {
                assert_eq!(
                    matches!(gate.evaluate(&call, mode), PermissionCheck::Ask(_)),
                    ask,
                    "{command}: {mode:?}"
                );
            }
            assert!(
                matches!(
                    gate.evaluate(&call, PermissionMode::AllowAll),
                    PermissionCheck::Execute(_)
                ),
                "{command}"
            );
        }
    }
}

#[tokio::test]
async fn readonly_shell_still_asks_for_literal_protected_paths() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let cwd = home.join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let private = home.join(".canter");
    let custom = dir.path().join("custom-state");
    let config = cwd.join("settings.json");
    let environment = BTreeMap::from([("HOME".into(), home.to_string_lossy().into_owned())]);
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            cwd.clone(),
            environment,
            vec![private.clone(), custom.clone(), config],
        )
        .unwrap(),
    ));
    let cancel = CancellationToken::new();
    let mut gate = PermissionGate::default();
    let commands = [
        "cat ~/.canter/config.json".to_owned(),
        "cat ../.canter/config.json".to_owned(),
        "cat ../repo/../.canter/config.json".to_owned(),
        "cat '../.canter/config.json'".to_owned(),
        "cat ..\\/.canter/config.json".to_owned(),
        "ls ../.canter".to_owned(),
        "cat settings.json".to_owned(),
        "cat ./settings.json".to_owned(),
        "ls && cat ../.canter/config.json".to_owned(),
        "rg -f../.canter/config.json needle".to_owned(),
        "grep -nf../.canter/config.json file".to_owned(),
        "rg --file=../.canter/config.json needle".to_owned(),
        "grep --file ../.canter/config.json file".to_owned(),
        format!("cat {}/config.json", private.display()),
        format!("cat '{}/config.json'", custom.display()),
    ];
    for command in commands {
        let raw = serde_json::json!({"command": command}).to_string();
        let call = tools
            .prepare(Tool::Shell, "private", &raw, &cancel)
            .await
            .unwrap();
        for mode in [
            PermissionMode::AlwaysAsk,
            PermissionMode::AskForExecution,
            PermissionMode::Unknown,
        ] {
            assert!(
                matches!(gate.evaluate(&call, mode), PermissionCheck::Ask(_)),
                "{command}: {mode:?}"
            );
        }
        assert!(matches!(
            gate.evaluate(&call, PermissionMode::AllowAll),
            PermissionCheck::Execute(_)
        ));
    }
    for command in [
        "cat README.md",
        "cat ../.canter-other/file",
        "cat settings.json.backup",
        "cat ~/notes",
    ] {
        let call = tools
            .prepare(
                Tool::Shell,
                "public",
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
    let call = tools
        .prepare(
            Tool::Shell,
            "private",
            r#"{"command":"cat ~/.canter/config.json"}"#,
            &cancel,
        )
        .await
        .unwrap();
    gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
        .unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));
}

#[tokio::test]
async fn readonly_shell_checks_implicit_cwd_and_requires_a_known_home_for_tilde() {
    let dir = tempfile::tempdir().unwrap();
    let protected = dir.path().join("private");
    std::fs::create_dir(&protected).unwrap();
    let cancel = CancellationToken::new();
    for (cwd, home, command) in [
        (protected.clone(), None, "ls"),
        (protected.clone(), None, "rg --files"),
        (protected.clone(), None, "cat config.json"),
        (dir.path().into(), None, "cat ~/.canter/config.json"),
        (dir.path().into(), Some("relative"), "cat ~/file"),
        (dir.path().into(), None, "cat ~somebody/file"),
    ] {
        let environment = home
            .map(|home| BTreeMap::from([("HOME".into(), home.into())]))
            .unwrap_or_default();
        let tools = Tools::new(Arc::new(
            LocalExecutor::new(cwd, environment, vec![protected.clone()]).unwrap(),
        ));
        let call = tools
            .prepare(
                Tool::Shell,
                "private",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        assert!(
            matches!(
                PermissionGate::default().evaluate(&call, PermissionMode::AlwaysAsk),
                PermissionCheck::Ask(_)
            ),
            "{command}"
        );
    }
}

#[tokio::test]
async fn auto_approved_shell_preserves_command_items_and_output() {
    use canter_protocol::{Item, ItemStatus};
    use canter_tools::ToolOutcome;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "repository contents\n").unwrap();
    let environment = BTreeMap::from([
        ("HOME".into(), dir.path().to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]);
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), environment, vec![]).unwrap(),
    ));
    let cancel = CancellationToken::new();
    let raw = r#"{"command":"cat README.md"}"#;
    let call = tools
        .prepare(Tool::Shell, "read-repo", raw, &cancel)
        .await
        .unwrap();
    assert_eq!(call.raw_arguments(), raw);
    assert!(
        matches!(call.started_item(), Item::CommandExecution { command, cwd, status: ItemStatus::InProgress, .. } if command == "cat README.md" && cwd == dir.path().to_string_lossy())
    );
    let gate = PermissionGate::default();
    let PermissionCheck::Execute(permit) = gate.evaluate(&call, PermissionMode::AlwaysAsk) else {
        panic!("reading the repository must not need approval");
    };
    let mut streamed = String::new();
    let result = tools
        .execute(permit, &cancel, &mut |chunk| streamed.push_str(chunk))
        .await;
    assert_eq!(result.outcome, ToolOutcome::Completed);
    assert_eq!(streamed, "repository contents\n");
    assert!(result.text.contains("repository contents\n"));
    assert!(
        matches!(result.item, Item::CommandExecution { id, command, status: ItemStatus::Completed, aggregated_output, exit_code: Some(0), .. } if id == "read-repo" && command == "cat README.md" && aggregated_output == streamed)
    );
    let write = tools
        .prepare(Tool::Shell, "write", r#"{"command":"touch file"}"#, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        gate.evaluate(&write, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
}

#[tokio::test]
async fn permission_modes_use_tool_risk_and_unknown_means_always_ask() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file"), "original\n").unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
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
        (
            Tool::Shell,
            r#"{"command":"printf 'complete command' > file"}"#,
        ),
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
                            ApprovalDecision::DeclineWithFeedback,
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
                PermissionCheck::Execute(_) => assert!(!ask, "{tool:?} {mode:?} must ask"),
                other => panic!("unexpected permission decision: {other:?}"),
            }
        }
    }
}

#[tokio::test]
async fn approval_answers_and_grants_are_scoped_to_one_session_and_one_tool() {
    let dir = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ));
    let cancel = CancellationToken::new();
    let shell = tools
        .prepare(
            Tool::Shell,
            "s1",
            r#"{"command":"printf first > first"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let another_shell = tools
        .prepare(
            Tool::Shell,
            "s2",
            r#"{"command":"printf different > different"}"#,
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
        first_session.resolve(&shell, ApprovalDecision::Decline, None),
        Ok(PermissionCheck::Deny {
            cancel_turn: false,
            feedback: None
        })
    );
    for (decision, feedback) in [
        (ApprovalDecision::Decline, None),
        (
            ApprovalDecision::DeclineWithFeedback,
            Some("read instead".to_owned()),
        ),
    ] {
        assert_eq!(
            first_session.resolve(&shell, decision, Some("read instead".into())),
            Ok(PermissionCheck::Deny {
                cancel_turn: false,
                feedback
            })
        );
    }
    assert_eq!(
        first_session.resolve(&shell, ApprovalDecision::Cancel, None),
        Ok(PermissionCheck::Deny {
            cancel_turn: true,
            feedback: None
        })
    );
    assert!(
        first_session
            .resolve(&shell, ApprovalDecision::Unknown, None)
            .is_err()
    );
    assert!(matches!(
        first_session.resolve(&shell, ApprovalDecision::Accept, None),
        Ok(PermissionCheck::Execute(_))
    ));
    assert!(matches!(
        first_session.evaluate(&another_shell, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    assert!(matches!(
        first_session.resolve(&shell, ApprovalDecision::AcceptForSession, None),
        Ok(PermissionCheck::Execute(_))
    ));
    assert!(matches!(
        first_session.evaluate(&another_shell, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));
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
