// Permission checks only: never pass these prepared calls to an executor.
use canter_protocol::{ApprovalDecision, PermissionMode};
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, PreparedCall, Tool, Tools};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    cwd: PathBuf,
    additional: PathBuf,
    rules: PathBuf,
    tools: Tools,
}

#[tokio::test]
async fn unknown_targets_and_cd_locations_require_the_critical_path_check() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    for (command, target) in [
        ("rm -rf \"$DIR\"/*", "\"$DIR\"/*"),
        ("rm -rf ${DIR}", "${DIR}"),
        ("rm -rf $(cat list)", "$(cat list)"),
        ("rm -rf `cat list`", "`cat list`"),
        ("cd / && rm -rf usr", "usr"),
        ("cd /; rm -rf *", "*"),
        ("cd /\nrm -rf *", "*"),
        ("cd \"$DIR\" && rm -rf target", "target"),
        ("cd ~/work && rm -rf repo", "repo"),
        ("cd build; rm -rf *", "*"),
        ("cd build || rm -rf *", "*"),
        ("bash -c 'cd / && rm -rf usr'", "usr"),
    ] {
        let call = fixture.call(command, false).await;
        let PermissionCheck::Forbidden { reason } = gate.evaluate(&call, PermissionMode::AllowAll)
        else {
            panic!("not blocked: {command}");
        };
        assert!(
            reason.contains(&format!("({target})")),
            "{command}: {reason}"
        );
        assert_single_approval(&gate, &call);
    }
    let call = fixture.call("rm -rf 'unfinished", false).await;
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Forbidden { .. }
    ));
    // && selects cd's successful location; ; and || retain the failure location above.
    let call = fixture.call("cd build && rm -rf *", false).await;
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Execute(_)
    ));
}

#[tokio::test]
async fn critical_removal_spellings_are_recognized_in_every_shell_segment() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    for command in [
        "rm .",
        "rm ~/*",
        "rmdir .",
        "rm -rf .",
        "rm -fr .",
        "rm -r -f .",
        "rm -Rf .",
        "rm --recursive --force .",
        "rm . -rf",
        "rm -- .",
        "/bin/rm -rf .",
        "/usr/bin/rm -rf .",
        "X=1 rm -rf .",
        "sudo rm -rf .",
        "sudo -u root rm -rf .",
        "env rm -rf .",
        "env -i X=1 rm -rf .",
        "timeout 10 rm -rf .",
        "time rm -rf .",
        "nice rm -rf .",
        "nice -n 10 rm -rf .",
        "nohup rm -rf .",
        "command rm -rf .",
        "exec rm -rf .",
        "xargs rm -rf .",
        "xargs -0 rm -rf .",
        "xargs rm -rf",
        "if true; then rm -rf .; fi",
        "for entry in a b; do rm -rf .; done",
        "! rm -rf .",
        "pwd; rm -rf .",
        "pwd && rm -rf .",
        "pwd || rm -rf .",
        "pwd | rm -rf .",
        "pwd\nrm -rf .",
        "echo $(pwd); rm -rf .",
        "echo ${NAME}; rm -rf .",
        "(pwd); rm -rf .",
        "{ pwd; }; rm -rf .",
        "(rm -rf .)",
        "{ rm -rf .; }",
        "echo $(rm -rf .)",
        "bash -c 'rm -rf .'",
        "sh -c 'rm -rf .'",
        "zsh -c 'rm -rf .'",
        "bash -lc 'pwd; rm -rf .'",
        "rm -rf . > output",
    ] {
        for background in [false, true] {
            let call = fixture.call(command, background).await;
            let PermissionCheck::Forbidden { reason } =
                gate.evaluate(&call, PermissionMode::AllowAll)
            else {
                panic!("not blocked: {command}, background={background}");
            };
            assert!(
                reason.starts_with("Blocked: rm/rmdir on a critical path ("),
                "{reason}"
            );
            assert!(reason.ends_with("canter refuses this in allowAll mode. If the user really wants it, they can run it themselves with !."), "{reason}");
            assert_single_approval(&gate, &call);
        }
    }
}

impl Fixture {
    fn new() -> Self {
        // Keep critical roots outside /tmp so the /tmp cleanup cases below
        // cannot also match the separate ancestor-of-a-critical-path rule.
        let temp = tempfile::tempdir_in("/var/tmp").unwrap();
        let home = temp.path().join("home");
        let cwd = home.join("work/repo");
        let additional = temp.path().join("extra/project");
        let state = temp.path().join("state");
        for path in [&cwd, &additional, &state] {
            std::fs::create_dir_all(path).unwrap();
        }
        let tools = Tools::new(Arc::new(
            LocalExecutor::new(
                cwd.clone(),
                BTreeMap::from([
                    ("HOME".into(), home.to_str().unwrap().into()),
                    ("CANTER_HOME".into(), state.to_str().unwrap().into()),
                ]),
                vec![],
            )
            .unwrap(),
        ));
        Self {
            _temp: temp,
            home,
            cwd,
            additional,
            rules: state.join("rules.json"),
            tools,
        }
    }

    fn gate(&self) -> PermissionGate {
        let mut gate = PermissionGate::default().with_rules_file(self.rules.clone());
        gate.set_additional_directories(&[self.additional.to_str().unwrap().into()]);
        gate
    }

    async fn call(&self, command: &str, background: bool) -> PreparedCall {
        self.tools
            .prepare(
                Tool::Shell,
                "rm-guard",
                &serde_json::json!({"command":command,"background":background}).to_string(),
                &CancellationToken::new(),
            )
            .await
            .unwrap()
    }
}

fn assert_single_approval(gate: &PermissionGate, call: &PreparedCall) {
    for mode in [PermissionMode::AlwaysAsk, PermissionMode::AskForExecution] {
        let PermissionCheck::Ask(request) = gate.evaluate(call, mode) else {
            panic!("critical removal must ask in {mode:?}");
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
        assert!(request.session_prefixes.is_none());
        assert!(request.proposed_execpolicy_amendment.is_none());
    }
}

#[tokio::test]
async fn critical_removals_ignore_rules_and_session_prefixes_but_allow_single_approval() {
    let fixture = Fixture::new();
    let mut gate = fixture.gate();
    std::fs::write(&fixture.rules, r#"{"rules":[{"pattern":["rm"],"decision":"allow"},{"pattern":["rmdir"],"decision":"allow"}]}"#).unwrap();
    // Model an already-approved prefix through the gate's existing prefix API.
    // This only records a grant/returns a permit: no call is executed.
    let safe = fixture.call("rm file.txt", false).await;
    assert!(matches!(
        gate.resolve_with_prefixes(
            &safe,
            ApprovalDecision::AcceptForSession,
            None,
            Some(&[vec!["rm".into()]])
        ),
        Ok(PermissionCheck::Execute(_))
    ));
    for command in ["rm -rf .", "rmdir .", "rm -rf \"$DIR\"/*"] {
        let call = fixture.call(command, false).await;
        assert_single_approval(&gate, &call);
        assert!(matches!(
            gate.resolve(&call, ApprovalDecision::Accept, None),
            Ok(PermissionCheck::Execute(_))
        ));
        for decision in [
            ApprovalDecision::AcceptForSession,
            ApprovalDecision::AcceptWithExecpolicyAmendment,
        ] {
            assert!(
                gate.resolve(&call, decision, None).is_err(),
                "{command}: {decision:?}"
            );
        }
    }
    std::fs::write(&fixture.rules, r#"{"rules":[]}"#).unwrap();
    assert!(matches!(
        gate.evaluate(&safe, PermissionMode::AlwaysAsk),
        PermissionCheck::Execute(_)
    ));
    let critical = fixture.call("rm -rf .", true).await;
    assert_single_approval(&gate, &critical);
    assert!(matches!(
        gate.evaluate(&critical, PermissionMode::AllowAll),
        PermissionCheck::Forbidden { .. }
    ));
}

#[tokio::test]
async fn ordinary_project_removals_keep_existing_permission_behavior() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    for command in [
        "rm -rf target",
        "rm -rf node_modules",
        "rm -rf ./build",
        "rm file.txt",
    ] {
        for background in [false, true] {
            let call = fixture.call(command, background).await;
            assert!(
                matches!(
                    gate.evaluate(&call, PermissionMode::AllowAll),
                    PermissionCheck::Execute(_)
                ),
                "{command}"
            );
            assert_single_approval(&gate, &call);
        }
    }
    std::fs::write(
        &fixture.rules,
        r#"{"rules":[{"pattern":["rm"],"decision":"allow"}]}"#,
    )
    .unwrap();
    for command in [
        "rm -rf target",
        "rm -rf node_modules",
        "rm -rf ./build",
        "rm file.txt",
    ] {
        let call = fixture.call(command, false).await;
        for mode in [
            PermissionMode::AllowAll,
            PermissionMode::AlwaysAsk,
            PermissionMode::AskForExecution,
        ] {
            assert!(
                matches!(gate.evaluate(&call, mode), PermissionCheck::Execute(_)),
                "{command}: {mode:?}"
            );
        }
    }
}

#[tokio::test]
async fn selective_globs_keep_existing_permission_behavior() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    for rules in [
        r#"{"rules":[]}"#,
        r#"{"rules":[{"pattern":["rm"],"decision":"allow"}]}"#,
    ] {
        std::fs::write(&fixture.rules, rules).unwrap();
        for command in [
            "rm -f *.log",
            "rm -f ./*.log",
            "rm -f *.o",
            "rm -rf *.egg-info",
            "rm -rf dist build *.egg-info",
            "rm -rf build-*",
            "rm -rf /tmp/foo-*",
            "rm -f /tmp/*.log",
        ] {
            for background in [false, true] {
                let call = fixture.call(command, background).await;
                assert!(
                    matches!(
                        gate.evaluate(&call, PermissionMode::AllowAll),
                        PermissionCheck::Execute(_)
                    ),
                    "{command}, background={background}, rules={rules}"
                );
                // Main already asks about globs even with a hand-written allow rule.
                assert_single_approval(&gate, &call);
            }
        }
    }
}

#[tokio::test]
async fn whole_layer_globs_and_possible_critical_children_still_require_approval() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    for command in [
        "rm -rf *",
        "rm -rf ./*",
        "rm -rf .*",
        "rm -rf **",
        "rm -rf ~/*",
        "rm -rf /*",
        "rm -rf /usr/*",
        "rm -rf ../*",
        "cd / && rm -rf *",
        "rm -rf /U*",
        "rm -rf ../re*",
    ] {
        for background in [false, true] {
            let call = fixture.call(command, background).await;
            let PermissionCheck::Forbidden { reason } =
                gate.evaluate(&call, PermissionMode::AllowAll)
            else {
                panic!("not blocked: {command}, background={background}");
            };
            assert!(
                reason.starts_with("Blocked: rm/rmdir on a critical path ("),
                "{reason}"
            );
            assert_single_approval(&gate, &call);
        }
    }
}

#[tokio::test]
async fn forbidden_rules_and_readonly_modes_still_take_priority() {
    let fixture = Fixture::new();
    let mut gate = fixture.gate();
    let call = fixture.call("rm -rf .", false).await;
    std::fs::write(
        &fixture.rules,
        r#"{"rules":[{"pattern":["rm"],"decision":"forbidden"}]}"#,
    )
    .unwrap();
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
    ] {
        assert_eq!(
            gate.evaluate(&call, mode),
            PermissionCheck::Forbidden {
                reason: "Command forbidden by rules.json".into()
            }
        );
        assert!(matches!(
            gate.evaluate_plan(&call, mode),
            PermissionCheck::Deny { .. }
        ));
    }
    assert!(matches!(
        gate.evaluate_review(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Deny { .. }
    ));
    assert!(matches!(
        gate.resolve(&call, ApprovalDecision::Accept, None),
        Ok(PermissionCheck::Forbidden { .. })
    ));
    // A malformed rules file must not turn an allowAll block into an approval.
    std::fs::write(&fixture.rules, "invalid JSON").unwrap();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Forbidden { .. }
    ));
    assert_single_approval(&gate, &call);
}

#[tokio::test]
async fn critical_paths_are_blocked_in_allow_all_with_model_explanation() {
    let fixture = Fixture::new();
    let gate = fixture.gate();
    for target in [
        "/".to_owned(),
        "/usr".into(),
        "/etc".into(),
        "/Users".into(),
        fixture.home.display().to_string(),
        fixture.cwd.display().to_string(),
        fixture.cwd.parent().unwrap().display().to_string(),
        fixture.additional.display().to_string(),
        fixture.additional.parent().unwrap().display().to_string(),
        "~".into(),
        "~/*".into(),
        ".".into(),
        "..".into(),
        "./build/../../".into(),
        ".///".into(),
        "*".into(),
        "./*".into(),
        format!("{}/*", fixture.additional.display()),
    ] {
        for background in [false, true] {
            let call = fixture.call(&format!("rm -rf {target}"), background).await;
            assert_eq!(
                gate.evaluate(&call, PermissionMode::AllowAll),
                PermissionCheck::Forbidden {
                    reason: format!(
                        "Blocked: rm/rmdir on a critical path ({target}). canter refuses this in allowAll mode. If the user really wants it, they can run it themselves with !."
                    )
                },
                "{target}, background={background}"
            );
            assert_single_approval(&gate, &call);
        }
    }
}
