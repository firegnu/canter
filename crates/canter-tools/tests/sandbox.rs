#![cfg(target_os = "macos")]

mod common;
use canter_protocol::Item;
use canter_tools::{LocalExecutor, Tool, ToolOutput, Tools};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

struct Fixture {
    root: tempfile::TempDir,
    cwd: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let cwd = root.path().join("project");
        let home = root.path().join("home");
        let state = root.path().join("state");
        for path in [&cwd, &home, &state] {
            std::fs::create_dir(path).unwrap();
        }
        Self {
            root,
            cwd,
            home,
            state,
        }
    }

    fn tools(&self, enabled: bool) -> Tools {
        self.tools_at(&self.cwd, enabled)
    }

    fn tools_at(&self, cwd: &std::path::Path, enabled: bool) -> Tools {
        let tools = Tools::new(Arc::new(
            LocalExecutor::new(
                cwd.into(),
                BTreeMap::from([
                    ("HOME".into(), self.home.to_string_lossy().into_owned()),
                    (
                        "CANTER_HOME".into(),
                        self.state.to_string_lossy().into_owned(),
                    ),
                    ("PATH".into(), "/usr/bin:/bin".into()),
                ]),
                vec![self.state.clone()],
            )
            .unwrap()
            .with_sandbox(enabled),
        ));
        tools.configure_shell(canter_protocol::PermissionMode::AllowAll, &[]);
        tools
    }

    fn git(&self, args: &[&str]) {
        let result = std::process::Command::new("/usr/bin/git")
            .args(args)
            .current_dir(&self.cwd)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn git_command(&self) -> String {
        // Resolve outside Seatbelt: /usr/bin/git may refresh xcrun's system cache.
        let result = std::process::Command::new("/usr/bin/xcrun")
            .args(["--find", "git"])
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        shell_quote(String::from_utf8(result.stdout).unwrap().trim())
    }
}

#[tokio::test]
async fn sandbox_ask_for_execution_runs_ordinary_foreground_and_background_without_approval() {
    use canter_protocol::PermissionMode;
    use canter_tools::{PermissionCheck, PermissionGate};
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    tools.configure_shell(PermissionMode::AskForExecution, &[]);
    for background in [false, true] {
        let call = tools
            .prepare(
                Tool::Shell,
                "inside",
                &serde_json::json!({
                    "command": "printf inside > inside.txt", "background": background
                })
                .to_string(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(matches!(
            PermissionGate::default().evaluate(&call, PermissionMode::AskForExecution),
            PermissionCheck::Execute(_)
        ));
        assert!(matches!(
            PermissionGate::default().evaluate(&call, PermissionMode::AlwaysAsk),
            PermissionCheck::Ask(_)
        ));
    }
}

#[tokio::test]
async fn sandbox_network_request_needs_once_approval_and_does_not_carry_over() {
    use canter_protocol::{ApprovalDecision, PermissionMode};
    use canter_tools::{PermissionCheck, PermissionGate};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    tools.configure_shell(PermissionMode::AskForExecution, &[]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let command = format!("/usr/bin/curl --noproxy '*' --max-time 2 -fsS http://{address}/");
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "network",
            &serde_json::json!({
                "command": command, "sandboxRequest": {"network": true},
                "justification": "Read the local test server."
            })
            .to_string(),
            &cancel,
        )
        .await
        .unwrap();
    let mut gate = PermissionGate::default();
    for mode in [PermissionMode::AlwaysAsk, PermissionMode::AskForExecution] {
        let PermissionCheck::Ask(request) = gate.evaluate(&call, mode) else {
            panic!("network needs approval")
        };
        assert!(
            !request
                .available_decisions
                .contains(&ApprovalDecision::AcceptForSession)
        );
        assert!(
            !request
                .available_decisions
                .contains(&ApprovalDecision::AcceptWithExecpolicyAmendment)
        );
    }
    assert!(matches!(
        gate.resolve(&call, ApprovalDecision::Decline, None)
            .unwrap(),
        PermissionCheck::Deny { .. }
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), listener.accept())
            .await
            .is_err()
    );
    let PermissionCheck::Execute(permit) =
        gate.resolve(&call, ApprovalDecision::Accept, None).unwrap()
    else {
        panic!("approved")
    };
    let service = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlocal")
            .await
            .unwrap();
    };
    let mut on_output = |_: &str| {};
    let (output, served) = tokio::join!(tools.execute(permit, &cancel, &mut on_output), async {
        tokio::time::timeout(std::time::Duration::from_secs(4), service).await
    });
    assert_eq!(code(&output), Some(0), "{}", output.text);
    served.unwrap();
    assert!(output.text.contains("local"));
    let next = run(&tools, &command).await;
    assert_ne!(code(&next), Some(0), "{}", next.text);
}

#[tokio::test]
async fn sandbox_write_request_opens_only_that_call_and_allow_all_refuses_it() {
    use canter_protocol::{ApprovalDecision, PermissionMode};
    use canter_tools::{PermissionCheck, PermissionGate};
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    tools.configure_shell(PermissionMode::AskForExecution, &[]);
    let outside = fixture.root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let target = outside.join("new");
    let command = format!(
        "mkdir -p {} && printf approved > {}/result",
        shell_quote(target.to_str().unwrap()),
        shell_quote(target.to_str().unwrap())
    );
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "write-request",
            &serde_json::json!({
                "command": command, "sandboxRequest": {"writePaths": ["../outside/new"]},
                "justification": "Write the requested test output."
            })
            .to_string(),
            &cancel,
        )
        .await
        .unwrap();
    let mut gate = PermissionGate::default();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AskForExecution),
        PermissionCheck::Ask(_)
    ));
    let PermissionCheck::Execute(permit) =
        gate.resolve(&call, ApprovalDecision::Accept, None).unwrap()
    else {
        panic!("approved")
    };
    let output = tools.execute(permit, &cancel, &mut |_| {}).await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert_eq!(
        std::fs::read_to_string(target.join("result")).unwrap(),
        "approved"
    );
    let next = run(
        &tools,
        &format!(
            "printf changed > {}/result",
            shell_quote(target.to_str().unwrap())
        ),
    )
    .await;
    assert_ne!(code(&next), Some(0), "{}", next.text);
    assert!(next.text.contains("The sandbox blocked access outside the permitted paths or network scope. If the command really needs it, retry it with a sandbox request and a justification, or ask the user to run it with !."), "{}", next.text);
    assert_eq!(
        std::fs::read_to_string(target.join("result")).unwrap(),
        "approved"
    );
    let PermissionCheck::Forbidden { reason } = gate.evaluate(&call, PermissionMode::AllowAll)
    else {
        panic!("allowAll cannot grant write scope")
    };
    assert_eq!(
        reason,
        "Blocked: writing outside the sandbox needs approval, and Allow all mode never asks. If the user really wants it, they can run it themselves with !."
    );
}

#[tokio::test]
async fn sandbox_requests_cannot_unlock_private_paths_or_git_configuration() {
    use canter_protocol::{ApprovalDecision, PermissionMode};
    use canter_tools::{PermissionCheck, PermissionGate};
    let fixture = Fixture::new();
    fixture.git(&["init", "-q"]);
    for name in [".ssh", ".aws", ".gnupg", ".canter"] {
        std::fs::create_dir(fixture.home.join(name)).unwrap();
    }
    let tools = fixture.tools(true);
    let paths = [
        fixture.home.join(".ssh"),
        fixture.home.join(".aws/new"),
        fixture.home.join(".gnupg"),
        fixture.home.join(".canter"),
        fixture.home.clone(),
        fixture.state.clone(),
        fixture.root.path().to_owned(),
        fixture.cwd.join(".git/config"),
        fixture.cwd.join(".git/hooks/new-hook"),
    ];
    for path in paths {
        let call = tools
            .prepare(
                Tool::Shell,
                "protected",
                &serde_json::json!({
                    "command": "printf harmless", "sandboxRequest": {"writePaths": [path]},
                    "justification": "Write test output."
                })
                .to_string(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let mut gate = PermissionGate::default();
        for mode in [
            PermissionMode::AlwaysAsk,
            PermissionMode::AskForExecution,
            PermissionMode::AllowAll,
        ] {
            assert!(
                matches!(
                    gate.evaluate(&call, mode),
                    PermissionCheck::Forbidden { .. }
                ),
                "{path:?}"
            );
        }
        assert!(
            matches!(
                gate.resolve(&call, ApprovalDecision::Accept, None).unwrap(),
                PermissionCheck::Forbidden { .. }
            ),
            "{path:?}"
        );
    }
}

#[tokio::test]
async fn sandbox_requests_keep_rules_dangerous_commands_and_readonly_restrictions() {
    use canter_protocol::{ApprovalDecision, PermissionMode};
    use canter_tools::{PermissionCheck, PermissionGate};
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let cancel = CancellationToken::new();
    let rules = fixture.state.join("rules.json");
    let mut gate = PermissionGate::default().with_rules_file(rules.clone());
    let ordinary = tools
        .prepare(
            Tool::Shell,
            "ordinary",
            r#"{"command":"touch result"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let PermissionCheck::Ask(_) = gate.evaluate(&ordinary, PermissionMode::AlwaysAsk) else {
        panic!("ordinary proposal")
    };
    gate.resolve(&ordinary, ApprovalDecision::AcceptForSession, None)
        .unwrap();
    let request = tools.prepare(Tool::Shell, "request", r#"{"command":"touch result","sandboxRequest":{"network":true},"justification":"Use network for this command."}"#, &cancel).await.unwrap();
    for decision in ["allow", "prompt", "forbidden"] {
        std::fs::write(
            &rules,
            serde_json::json!({"rules":[{"pattern":["touch"],"decision":decision}]}).to_string(),
        )
        .unwrap();
        let check = gate.evaluate(&request, PermissionMode::AskForExecution);
        if decision == "forbidden" {
            assert!(matches!(check, PermissionCheck::Forbidden { .. }));
        } else {
            let PermissionCheck::Ask(approval) = check else {
                panic!("rules and grants cannot skip a request")
            };
            assert!(approval.session_prefixes.is_none());
            assert!(approval.proposed_execpolicy_amendment.is_none());
            assert!(
                gate.resolve(&request, ApprovalDecision::AcceptForSession, None)
                    .is_err()
            );
            assert!(
                gate.resolve(
                    &request,
                    ApprovalDecision::AcceptWithExecpolicyAmendment,
                    None
                )
                .is_err()
            );
        }
        let ordinary_check = gate.evaluate(&ordinary, PermissionMode::AskForExecution);
        assert!(match decision {
            "allow" => matches!(ordinary_check, PermissionCheck::Execute(_)),
            "prompt" => matches!(ordinary_check, PermissionCheck::Ask(_)),
            _ => matches!(ordinary_check, PermissionCheck::Forbidden { .. }),
        });
    }
    std::fs::write(&rules, r#"{"rules":[]}"#).unwrap();
    assert!(matches!(
        gate.evaluate(&request, PermissionMode::AllowAll),
        PermissionCheck::Execute(_)
    ));
    assert!(matches!(
        gate.evaluate_plan(&request, PermissionMode::AskForExecution),
        PermissionCheck::Deny { .. }
    ));
    assert!(matches!(
        gate.evaluate_review(&request),
        PermissionCheck::Deny { .. }
    ));
    // Only inspect this deletion command's permission; it is never executed.
    for extra in [
        serde_json::json!({}),
        serde_json::json!({"sandboxRequest":{"network":true},"justification":"Test the approval decision."}),
    ] {
        let mut arguments = serde_json::json!({"command":"rmdir ."});
        arguments
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        let dangerous = tools
            .prepare(Tool::Shell, "dangerous", &arguments.to_string(), &cancel)
            .await
            .unwrap();
        let PermissionCheck::Ask(approval) =
            gate.evaluate(&dangerous, PermissionMode::AskForExecution)
        else {
            panic!("critical deletion still asks")
        };
        assert_eq!(
            approval.available_decisions,
            vec![
                ApprovalDecision::Accept,
                ApprovalDecision::Decline,
                ApprovalDecision::DeclineWithFeedback,
                ApprovalDecision::Cancel
            ]
        );
        assert!(matches!(
            gate.evaluate(&dangerous, PermissionMode::AllowAll),
            PermissionCheck::Forbidden { .. }
        ));
    }
    let readonly = tools.prepare(Tool::Shell, "readonly", r#"{"command":"pwd","sandboxRequest":{"network":true},"justification":"Use network for this command."}"#, &cancel).await.unwrap();
    assert!(matches!(
        gate.evaluate(&readonly, PermissionMode::AskForExecution),
        PermissionCheck::Ask(_)
    ));
    assert!(matches!(
        gate.evaluate(&readonly, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
}

#[tokio::test]
async fn sandbox_request_paths_are_canonical_and_disabled_modes_stay_unchanged() {
    use canter_protocol::PermissionMode;
    use canter_tools::{PermissionCheck, PermissionGate};
    let fixture = Fixture::new();
    let outside = fixture.root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, fixture.cwd.join("output-link")).unwrap();
    let tools = fixture.tools(true);
    let call = tools.prepare(Tool::Shell, "normalized", r#"{"command":"printf test","sandboxRequest":{"writePaths":["output-link/sub/../new/file"]},"justification":"Write test output."}"#, &CancellationToken::new()).await.unwrap();
    let PermissionCheck::Ask(approval) =
        PermissionGate::default().evaluate(&call, PermissionMode::AskForExecution)
    else {
        panic!("approval")
    };
    assert_eq!(
        approval.arguments["sandboxRequest"]["writePaths"],
        serde_json::json!([outside.canonicalize().unwrap().join("new/file")])
    );
    let disabled = fixture.tools(false);
    let call = disabled
        .prepare(
            Tool::Shell,
            "ordinary",
            r#"{"command":"touch result"}"#,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let gate = PermissionGate::default();
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AskForExecution),
        PermissionCheck::Ask(_)
    ));
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Execute(_)
    ));
    assert!(disabled.prepare(Tool::Shell, "unsupported", r#"{"command":"pwd","sandboxRequest":{"network":true},"justification":"Use network."}"#, &CancellationToken::new()).await.is_err());
    assert!(
        disabled
            .prepare(
                Tool::Shell,
                "disabled-field",
                r#"{"command":"pwd","justification":null}"#,
                &CancellationToken::new()
            )
            .await
            .is_err(),
        "disabled sandbox must keep rejecting the new parameter even when null"
    );
}

#[tokio::test]
async fn sandbox_approved_background_and_promoted_children_keep_the_request_until_exit() {
    use canter_protocol::{ApprovalDecision, PermissionMode};
    use canter_tools::{PermissionCheck, PermissionGate};
    use std::time::Duration;
    for promote in [false, true] {
        let fixture = Fixture::new();
        std::fs::create_dir(fixture.root.path().join("outside")).unwrap();
        let tools = fixture.tools(true);
        tools.configure_shell(PermissionMode::AskForExecution, &[]);
        let cancel = CancellationToken::new();
        let call = tools.prepare(Tool::Shell, "approved-background", &serde_json::json!({
            "command":"printf ready; while [ ! -e release ]; do sleep 0.01; done; /bin/sh -c 'printf approved > ../outside/result'",
            "background":!promote, "sandboxRequest":{"writePaths":["../outside/result"]}, "justification":"Write output when the job finishes."
        }).to_string(), &cancel).await.unwrap();
        let mut gate = PermissionGate::default();
        assert!(matches!(
            gate.evaluate(&call, PermissionMode::AskForExecution),
            PermissionCheck::Ask(_)
        ));
        let PermissionCheck::Execute(permit) =
            gate.resolve(&call, ApprovalDecision::Accept, None).unwrap()
        else {
            panic!("approved")
        };
        let ready = tokio::sync::Notify::new();
        let mut on_output = |_: &str| ready.notify_one();
        let execute = tools.execute(permit, &cancel, &mut on_output);
        let promotion = async {
            if promote {
                ready.notified().await;
                assert_eq!(
                    tools
                        .background()
                        .promote("approved-background")
                        .await
                        .unwrap(),
                    Some(1)
                );
            }
        };
        let (initial, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(execute, promotion)
        })
        .await
        .unwrap();
        assert!(
            initial.text.contains("Background command 1: running"),
            "{}",
            initial.text
        );
        // Reconfiguring future commands must not remove the approved child's scope.
        tools.configure_shell(PermissionMode::AskForExecution, &[]);
        let next = run(&tools, "printf unapproved > ../outside/result").await;
        assert_ne!(code(&next), Some(0), "{}", next.text);
        assert!(!fixture.root.path().join("outside/result").exists());
        std::fs::write(fixture.cwd.join("release"), "").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while tools.background().list()[0].running {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let output = tools.background().output(1, false).unwrap();
        assert_eq!(output.command.exit_code, Some(0), "{output:?}");
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("outside/result")).unwrap(),
            "approved"
        );
        tools.background().stop_all().await;
    }
}

#[tokio::test]
async fn sandbox_git_commits_in_repositories_and_worktrees_but_locks_config_and_hooks() {
    let fixture = Fixture::new();
    fixture.git(&["init", "-q"]);
    let tools = fixture.tools(true);
    let git = fixture.git_command();
    let commit = format!(
        "printf content > tracked; {git} add tracked && {git} -c user.name=Test -c user.email=test@example.invalid commit -qm initial"
    );
    let first = run(&tools, &commit).await;
    assert_eq!(code(&first), Some(0), "{}", first.text);
    for path in [".git/config", ".git/hooks/pre-commit"] {
        let write = run(&tools, &format!("printf changed > {path}")).await;
        assert_ne!(code(&write), Some(0), "{}", write.text);
    }
    fixture.git(&["worktree", "add", "-q", "-b", "topic/linked", "../linked"]);
    fixture.git(&["pack-refs", "--all", "--prune"]);
    let worktree_config = fixture.cwd.join(".git/worktrees/linked/config.worktree");
    std::fs::write(&worktree_config, "[user]\nname = Linked\n").unwrap();
    let write = run(
        &tools,
        "printf changed > .git/worktrees/linked/config.worktree",
    )
    .await;
    assert_ne!(code(&write), Some(0), "{}", write.text);
    assert_eq!(
        std::fs::read_to_string(&worktree_config).unwrap(),
        "[user]\nname = Linked\n"
    );
    let linked = fixture.tools_at(&fixture.root.path().join("linked"), true);
    let output = run(&linked, &format!("printf more >> tracked; {git} add tracked && {git} -c user.name=Test -c user.email=test@example.invalid commit -qm linked")).await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(
        !output.text.contains("Operation not permitted"),
        "{}",
        output.text
    );
    for path in ["../project/.git/config", "../project/.git/hooks/pre-commit"] {
        let write = run(&linked, &format!("printf changed > {path}")).await;
        assert_ne!(code(&write), Some(0), "{}", write.text);
    }
}

#[tokio::test]
async fn sandbox_network_allows_allow_all_and_blocks_both_asking_modes() {
    use canter_protocol::PermissionMode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let command = format!("/usr/bin/curl --noproxy '*' --max-time 2 -fsS http://{address}/");
    let service = async {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 1024];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nlocal")
            .await
            .unwrap();
    };
    let (output, served) = tokio::join!(run(&tools, &command), async {
        tokio::time::timeout(std::time::Duration::from_secs(4), service).await
    });
    assert_eq!(code(&output), Some(0), "{}", output.text);
    served.unwrap();
    assert!(output.text.contains("local"));
    for mode in [PermissionMode::AlwaysAsk, PermissionMode::AskForExecution] {
        tools.configure_shell(mode, &[]);
        let output = run(&tools, &command).await;
        assert_ne!(code(&output), Some(0), "{mode:?}: {}", output.text);
    }
    tools.configure_shell(
        PermissionMode::AllowAll,
        &[fixture.root.path().to_string_lossy().into_owned()],
    );
    let socket = fixture.state.join("control.sock");
    let unix = tokio::net::UnixListener::bind(&socket).unwrap();
    let output = run(&tools, "/usr/bin/curl --max-time 1 --unix-socket \"$CANTER_HOME/control.sock\" -fsS http://localhost/").await;
    assert_ne!(code(&output), Some(0), "{}", output.text);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), unix.accept())
            .await
            .is_err()
    );
}

async fn run(tools: &Tools, command: &str) -> ToolOutput {
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "sandbox",
            &serde_json::json!({"command":command}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    tools
        .execute(common::allow(&call), &cancel, &mut |_| {})
        .await
}

fn code(output: &ToolOutput) -> Option<i32> {
    match output.item {
        Item::CommandExecution { exit_code, .. } => exit_code,
        _ => panic!("expected a command"),
    }
}

// Invoked by the socket regression inside Seatbelt; never sends a DNS query.
#[test]
#[ignore = "connector invoked inside Seatbelt by sandbox_mdns_socket_follows_network_permission"]
fn mdns_connect_only() -> std::io::Result<()> {
    drop(std::os::unix::net::UnixStream::connect(
        "/private/var/run/mDNSResponder",
    )?);
    Ok(())
}

#[tokio::test]
async fn sandbox_mdns_socket_follows_network_permission() {
    use canter_protocol::PermissionMode;
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let executable = std::env::current_exe().unwrap();
    let command = format!(
        "{} --ignored --exact mdns_connect_only --nocapture",
        shell_quote(&executable.to_string_lossy())
    );
    for mode in [
        PermissionMode::AllowAll,
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
    ] {
        tools.configure_shell(mode, &[]);
        let output = run(&tools, &command).await;
        assert_eq!(
            code(&output) == Some(0),
            mode == PermissionMode::AllowAll,
            "{mode:?}: {}",
            output.text
        );
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[tokio::test]
async fn sandbox_clones_local_repository_into_temporary_directory() {
    let source = Fixture::new();
    source.git(&["init", "-q"]);
    std::fs::write(source.cwd.join("tracked"), "local source").unwrap();
    source.git(&["add", "tracked"]);
    source.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "initial",
    ]);
    let fixture = Fixture::new();
    let output = run(
        &fixture.tools(true),
        &format!(
            "git clone -q {} \"$TMPDIR/clone\" && cat \"$TMPDIR/clone/tracked\"",
            shell_quote(&source.cwd.to_string_lossy())
        ),
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(output.text.contains("local source"));
}

#[tokio::test]
async fn sandbox_builds_local_cargo_git_dependency_in_cache() {
    check_local_cargo_git_dependency(false).await;
}

#[tokio::test]
async fn sandbox_builds_local_cargo_git_dependency_when_cwd_is_home() {
    check_local_cargo_git_dependency(true).await;
}

async fn check_local_cargo_git_dependency(home_is_cwd: bool) {
    let source = Fixture::new();
    source.git(&["init", "-q"]);
    std::fs::write(
        source.cwd.join("Cargo.toml"),
        "[package]\nname = 'local_dep'\nversion = '0.1.0'\nedition = '2021'\n[lib]\npath = 'lib.rs'\n",
    )
    .unwrap();
    std::fs::write(source.cwd.join("lib.rs"), "pub fn answer() -> u32 { 42 }\n").unwrap();
    source.git(&["add", "Cargo.toml", "lib.rs"]);
    source.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "initial",
    ]);
    let fixture = Fixture::new();
    let cwd = if home_is_cwd {
        &fixture.home
    } else {
        &fixture.cwd
    };
    std::fs::write(
        cwd.join("Cargo.toml"),
        format!(
            "[package]\nname = 'local_app'\nversion = '0.1.0'\nedition = '2021'\n[lib]\npath = 'lib.rs'\n[dependencies]\nlocal_dep = {{ git = 'file://{}' }}\n",
            source.cwd.canonicalize().unwrap().display()
        ),
    )
    .unwrap();
    std::fs::write(
        cwd.join("lib.rs"),
        "#[test] fn dependency_works() { assert_eq!(local_dep::answer(), 42); }\n",
    )
    .unwrap();
    // Locate the installed toolchain read-only before entering the isolated HOME.
    // Direct binaries avoid asking rustup to configure/download a toolchain there.
    let sysroot = std::process::Command::new("rustc")
        .args(["--print", "sysroot"])
        .output()
        .unwrap();
    assert!(sysroot.status.success());
    let bin = PathBuf::from(String::from_utf8(sysroot.stdout).unwrap().trim()).join("bin");
    let tools = fixture.tools_at(cwd, true);
    // No IP network: the only dependency comes from the local file:// repository.
    tools.configure_shell(canter_protocol::PermissionMode::AlwaysAsk, &[]);
    let output = run(
        &tools,
        &format!(
            "RUSTC={} {} test --lib",
            shell_quote(&bin.join("rustc").to_string_lossy()),
            shell_quote(&bin.join("cargo").to_string_lossy())
        ),
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(output.text.contains("1 passed"), "{}", output.text);
    assert!(
        fixture
            .home
            .join("Library/Caches/canter/cargo/git/checkouts")
            .is_dir()
    );
}

#[tokio::test]
async fn sandbox_protects_submodule_metadata_and_allows_submodule_commits() {
    let source = Fixture::new();
    source.git(&["init", "-q"]);
    std::fs::write(source.cwd.join("tracked"), "initial").unwrap();
    source.git(&["add", "tracked"]);
    source.git(&[
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-qm",
        "initial",
    ]);
    let fixture = Fixture::new();
    fixture.git(&["init", "-q"]);
    fixture.git(&[
        "-c",
        "protocol.file.allow=always",
        "submodule",
        "add",
        "-q",
        source.cwd.to_str().unwrap(),
        "nested/child",
    ]);
    let parent = fixture.tools(true);
    let child = fixture.tools_at(&fixture.cwd.join("nested/child"), true);
    for name in [
        "config",
        "config.worktree",
        "hooks/pre-commit",
        "commondir",
        "gitdir",
    ] {
        let path = fixture.cwd.join(".git/modules/nested/child").join(name);
        let original = std::fs::read(&path).ok();
        for tools in [&parent, &child] {
            let output = run(
                tools,
                &format!("printf changed > {}", shell_quote(&path.to_string_lossy())),
            )
            .await;
            assert_ne!(code(&output), Some(0), "{name}: {}", output.text);
            assert_eq!(std::fs::read(&path).ok(), original, "{name}");
        }
    }
    let git = fixture.git_command();
    let output = run(
        &child,
        &format!("printf changed >> tracked && {git} add tracked && {git} -c user.name=Test -c user.email=test@example.invalid commit -qm child"),
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(
        !output.text.contains("Operation not permitted"),
        "{}",
        output.text
    );
}

#[tokio::test]
async fn sandbox_does_not_annotate_ordinary_nonzero_exits() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    for command in [
        "exit 1",
        "printf value | grep missing",
        "printf 'test failed\\n' >&2; exit 101",
    ] {
        let output = run(&tools, command).await;
        assert_ne!(code(&output), Some(0), "{}", output.text);
        assert!(
            !output.text.contains("The sandbox blocks"),
            "{}",
            output.text
        );
    }
}

#[tokio::test]
async fn sandbox_allows_writes_to_standard_output_devices() {
    let fixture = Fixture::new();
    let output = run(
        &fixture.tools(true),
        "printf stdout > /dev/stdout && printf stderr > /dev/stderr && printf fd > /dev/fd/1",
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    for expected in ["stdout", "stderr", "fd"] {
        assert!(output.text.contains(expected), "{}", output.text);
    }
}

#[tokio::test]
async fn sandbox_allows_project_writes_but_rejects_outside_writes() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let inside = run(&tools, "printf inside > result; cat result").await;
    assert_eq!(code(&inside), Some(0), "{}", inside.text);
    assert!(inside.text.contains("inside"));
    let outside = run(&tools, "printf outside > ../outside").await;
    assert_ne!(code(&outside), Some(0), "{}", outside.text);
    assert!(!fixture.root.path().join("outside").exists());
    assert!(outside.text.contains("sandbox"), "{}", outside.text);
    assert!(outside.text.contains("!"));
    assert!(
        matches!(&outside.item, Item::CommandExecution { aggregated_output, .. } if aggregated_output.contains("Operation not permitted") && !aggregated_output.contains("sandbox"))
    );
    let continued = run(&tools, "printf outside > ../outside; printf continued").await;
    assert_eq!(code(&continued), Some(0), "{}", continued.text);
    assert!(continued.text.contains("continued"));
    assert!(continued.text.contains("sandbox"), "{}", continued.text);
    assert!(!fixture.root.path().join("outside").exists());
}

#[tokio::test]
async fn sandbox_user_commands_and_disabled_sandbox_keep_original_execution() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let output = tools
        .user_shell(
            "user".into(),
            &canter_protocol::SessionShellCommandParams {
                command: "printf user > ../outside; printf '%s' \"${CARGO_HOME-unset}\"".into(),
                timeout_ms: None,
            },
            &CancellationToken::new(),
            &mut |_| {},
        )
        .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert_eq!(output.text, "unset");
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("outside")).unwrap(),
        "user"
    );
    let disabled = fixture.tools(false);
    let output = run(
        &disabled,
        "printf disabled > ../outside; printf '%s' \"${CARGO_HOME-unset}\"",
    )
    .await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(output.text.starts_with("unset"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("outside")).unwrap(),
        "disabled"
    );
}

#[tokio::test]
async fn sandbox_setup_and_policy_failures_never_run_the_command() {
    let fixture = Fixture::new();
    // A plain file prevents creation of the sandbox cache, without using real HOME.
    std::fs::write(fixture.home.join("Library"), "not a directory").unwrap();
    let tools = fixture.tools(true);
    let output = run(&tools, "printf ran > marker").await;
    assert_eq!(code(&output), None);
    assert!(
        output.text.contains("Could not start sandbox"),
        "{}",
        output.text
    );
    assert!(!fixture.cwd.join("marker").exists());
    std::fs::remove_file(fixture.home.join("Library")).unwrap();
    let tools = fixture.tools(true);
    tools.configure_shell(
        canter_protocol::PermissionMode::AllowAll,
        &[fixture
            .root
            .path()
            .join("missing")
            .to_string_lossy()
            .into_owned()],
    );
    let output = run(&tools, "printf ran > marker").await;
    assert_eq!(code(&output), None);
    assert!(
        output.text.contains("Could not start sandbox"),
        "{}",
        output.text
    );
    assert!(!fixture.cwd.join("marker").exists());
}

#[tokio::test]
async fn sandbox_paths_include_add_dir_private_temp_and_persistent_shared_caches() {
    let fixture = Fixture::new();
    let extra = fixture.root.path().join("extra");
    std::fs::create_dir(&extra).unwrap();
    let tools = fixture.tools(true);
    tools.configure_shell(
        canter_protocol::PermissionMode::AllowAll,
        &[extra.to_string_lossy().into_owned()],
    );
    let output = run(&tools, "printf extra > ../extra/result; printf temp > \"${TMPDIR:?}/result\"; printf cargo > \"${CARGO_HOME:?}/result\"; printf npm > \"${npm_config_cache:?}/result\"; printf '%s\\n' \"$TMPDIR\" \"$CARGO_HOME\" \"$npm_config_cache\"").await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    let paths = output
        .text
        .lines()
        .take(3)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    assert_eq!(
        std::fs::read_to_string(extra.join("result")).unwrap(),
        "extra"
    );
    assert_eq!(
        std::fs::read_to_string(paths[0].join("result")).unwrap(),
        "temp"
    );
    assert!(!paths[0].starts_with(fixture.state.canonicalize().unwrap()));
    let cache = fixture
        .home
        .join("Library/Caches/canter")
        .canonicalize()
        .unwrap();
    assert!(paths[1].starts_with(&cache));
    assert!(paths[2].starts_with(&cache));
    let second = fixture.tools(true);
    let again = run(
        &second,
        "cat \"$CARGO_HOME/result\" \"$npm_config_cache/result\"; printf '\\n%s' \"$TMPDIR\"",
    )
    .await;
    assert_eq!(code(&again), Some(0), "{}", again.text);
    assert!(again.text.starts_with("cargonpm\n"));
    assert_ne!(
        again.text.lines().nth(1).unwrap(),
        paths[0].to_str().unwrap()
    );
    tools.close_shell();
    assert!(!paths[0].exists());
    assert!(paths[1].join("result").exists());
    assert!(paths[2].join("result").exists());
}

#[tokio::test]
async fn sandbox_temp_supports_zsh_heredocs_and_cargo_cache_lock_reads() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let output = run(&tools, "/bin/zsh -fc 'cat <<EOF\nheredoc\nEOF' && printf lock > \"$CARGO_HOME/.cargo-build-lock\" && cat \"$CARGO_HOME/.cargo-build-lock\"").await;
    assert_eq!(code(&output), Some(0), "{}", output.text);
    assert!(output.text.contains("heredoc\nlock"), "{}", output.text);
    let output = run(&tools, "printf '%s' \"$TMPPREFIX\"").await;
    assert!(
        output.text.starts_with("/private/tmp/canter-shell-"),
        "{}",
        output.text
    );
}

#[tokio::test]
async fn sandbox_background_and_promoted_children_keep_their_starting_scope() {
    use std::time::Duration;
    for promote in [false, true] {
        let fixture = Fixture::new();
        let extra = fixture.root.path().join("later");
        std::fs::create_dir(&extra).unwrap();
        let tools = fixture.tools(true);
        let cancel = CancellationToken::new();
        let call = tools.prepare(Tool::Shell, "background", &serde_json::json!({
            "command":"printf ready; while [ ! -e release ]; do sleep 0.01; done; /bin/sh -c 'printf child > ../later/result'",
            "background":!promote
        }).to_string(), &cancel).await.unwrap();
        let ready = tokio::sync::Notify::new();
        let mut on_output = |_: &str| ready.notify_one();
        let execute = tools.execute(common::allow(&call), &cancel, &mut on_output);
        let promotion = async {
            if promote {
                ready.notified().await;
                assert_eq!(
                    tools.background().promote("background").await.unwrap(),
                    Some(1)
                );
            }
        };
        let (initial, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(execute, promotion)
        })
        .await
        .unwrap();
        assert!(
            initial.text.contains("Background command 1: running"),
            "{}",
            initial.text
        );
        tools.configure_shell(
            canter_protocol::PermissionMode::AllowAll,
            &[extra.to_string_lossy().into_owned()],
        );
        std::fs::write(fixture.cwd.join("release"), "").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while tools.background().list()[0].running {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let output = tools.background().output(1, false).unwrap();
        assert_eq!(output.command.exit_code, Some(1), "{output:?}");
        assert!(output.output.contains("Operation not permitted"));
        assert!(output.output.contains("sandbox"));
        assert!(!extra.join("result").exists());
        let next = run(&tools, "printf next > ../later/result").await;
        assert_eq!(code(&next), Some(0), "{}", next.text);
        assert_eq!(
            std::fs::read_to_string(extra.join("result")).unwrap(),
            "next"
        );
        tools.background().stop_all().await;
    }
}

#[tokio::test]
async fn sandbox_cannot_read_or_write_sensitive_and_state_directories() {
    let fixture = Fixture::new();
    for directory in [".ssh", ".aws", ".gnupg", ".canter"] {
        std::fs::create_dir(fixture.home.join(directory)).unwrap();
        std::fs::write(
            fixture.home.join(directory).join("secret"),
            "test-only-secret",
        )
        .unwrap();
    }
    std::fs::write(fixture.state.join("secret"), "test-only-secret").unwrap();
    let tools = fixture.tools(true);
    // Sensitive denies still apply when a user adds their home as a writable root.
    tools.configure_shell(
        canter_protocol::PermissionMode::AllowAll,
        &[fixture.home.to_string_lossy().into_owned()],
    );
    for path in [
        "$HOME/.ssh",
        "$HOME/.aws",
        "$HOME/.gnupg",
        "$HOME/.canter",
        "$CANTER_HOME",
    ] {
        let read = run(&tools, &format!("cat \"{path}/secret\"")).await;
        assert_ne!(code(&read), Some(0), "{}", read.text);
        assert!(!read.text.contains("test-only-secret"));
        let write = run(&tools, &format!("printf changed > \"{path}/secret\"")).await;
        assert_ne!(code(&write), Some(0), "{}", write.text);
    }
    for path in [
        fixture.home.join(".ssh"),
        fixture.home.join(".aws"),
        fixture.home.join(".gnupg"),
        fixture.home.join(".canter"),
        fixture.state.clone(),
    ] {
        assert_eq!(
            std::fs::read_to_string(path.join("secret")).unwrap(),
            "test-only-secret"
        );
    }
}

#[tokio::test]
async fn sandbox_keeps_original_package_caches_readonly_even_with_home_added() {
    let fixture = Fixture::new();
    for name in [".cargo", ".npm"] {
        std::fs::create_dir(fixture.home.join(name)).unwrap();
        std::fs::write(fixture.home.join(name).join("original"), "unchanged").unwrap();
    }
    let tools = fixture.tools(true);
    tools.configure_shell(
        canter_protocol::PermissionMode::AllowAll,
        &[fixture.home.to_string_lossy().into_owned()],
    );
    for name in [".cargo", ".npm"] {
        let output = run(
            &tools,
            &format!("printf changed > \"$HOME/{name}/original\""),
        )
        .await;
        assert_ne!(code(&output), Some(0), "{}", output.text);
        assert_eq!(
            std::fs::read_to_string(fixture.home.join(name).join("original")).unwrap(),
            "unchanged"
        );
    }
}
