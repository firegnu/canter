use canter_protocol::PermissionMode;
use canter_tools::{
    LocalExecutor, PermissionCheck, PermissionGate, Tool, ToolOutcome, Tools, memory::Memory,
};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _temp: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    cwd: PathBuf,
    memory: Arc<Memory>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let home = temp.path().join("home");
        let state = temp.path().join("state");
        let cwd = temp.path().join("project");
        for path in [&home, &state, &cwd] {
            std::fs::create_dir(path).unwrap();
        }
        let memory = Arc::new(Memory::new(state.clone(), cwd.clone()));
        memory.set_enabled(true).unwrap();
        Self {
            _temp: temp,
            home,
            state,
            cwd,
            memory,
        }
    }
    fn tools(&self, sandbox: bool) -> Tools {
        Tools::new(Arc::new(
            LocalExecutor::new(
                self.cwd.clone(),
                BTreeMap::from([
                    ("HOME".into(), self.home.to_string_lossy().into_owned()),
                    (
                        "CANTER_HOME".into(),
                        self.state.to_string_lossy().into_owned(),
                    ),
                    ("PATH".into(), "/usr/bin:/bin".into()),
                ]),
                vec![self.state.clone(), self.state.join("config.json")],
            )
            .unwrap()
            .with_memory(Some(self.memory.clone()))
            .with_sandbox(sandbox),
        ))
    }
}

#[tokio::test]
async fn memory_write_and_edit_are_allowed_in_both_roots_and_all_permission_modes() {
    for sandbox in [false, true] {
        let fixture = Fixture::new();
        let tools = fixture.tools(sandbox);
        let roots = fixture.memory.roots().unwrap().unwrap();
        for root in [&roots.global, &roots.project] {
            for mode in [
                PermissionMode::AlwaysAsk,
                PermissionMode::AskForExecution,
                PermissionMode::AllowAll,
            ] {
                let path = root.join("preference.md");
                let cancel = CancellationToken::new();
                for (tool, args) in [
                    (
                        Tool::Write,
                        serde_json::json!({"path":path,"content":"# Preference\nUse fixtures.\n\nDetails."}),
                    ),
                    (
                        Tool::Edit,
                        serde_json::json!({"path":path,"edits":[{"oldText":"Use fixtures.","newText":"Use isolated fixtures."}]}),
                    ),
                ] {
                    let call = tools
                        .prepare(tool, "memory", &args.to_string(), &cancel)
                        .await
                        .expect(
                            "memory files must be accessible inside the protected state directory",
                        );
                    let gate = PermissionGate::default();
                    assert!(matches!(
                        gate.evaluate_plan(&call, mode),
                        PermissionCheck::Deny { .. }
                    ));
                    assert!(matches!(
                        gate.evaluate_review(&call, mode),
                        PermissionCheck::Deny { .. }
                    ));
                    let PermissionCheck::Execute(permit) = gate.evaluate(&call, mode) else {
                        panic!("memory should not require approval")
                    };
                    assert_eq!(
                        tools.execute(permit, &cancel, &mut |_| {}).await.outcome,
                        ToolOutcome::Completed
                    );
                }
            }
        }
        fixture.memory.set_enabled(false).unwrap();
        assert!(
            tools
                .prepare(
                    Tool::Read,
                    "restricted",
                    &serde_json::json!({"path":roots.global.join("preference.md")}).to_string(),
                    &CancellationToken::new()
                )
                .await
                .is_err()
        );
        tools.close_shell();
    }
}

#[tokio::test]
async fn memory_sandbox_allows_the_two_roots_and_preserves_other_state_and_file_rules() {
    let fixture = Fixture::new();
    let tools = fixture.tools(true);
    let roots = fixture.memory.roots().unwrap().unwrap();
    let cancel = CancellationToken::new();
    let gate = PermissionGate::default();
    for root in [&roots.global, &roots.project] {
        let path = root.join("shell.md");
        let call = tools.prepare(Tool::Shell, "memory-shell", &serde_json::json!({"command":format!("printf '# Shell\\nA remembered preference.\\n\\nDetails.\\n' > '{}'; cat '{}'", path.display(), path.display())}).to_string(), &cancel).await.unwrap();
        let PermissionCheck::Execute(permit) = gate.evaluate(&call, PermissionMode::AllowAll)
        else {
            panic!("sandboxed shell")
        };
        let output = tools.execute(permit, &cancel, &mut |_| {}).await;
        assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
        assert!(
            output.text.contains("A remembered preference."),
            "{}",
            output.text
        );
    }
    let other = fixture.state.join("projects/other/memory");
    std::fs::create_dir_all(&other).unwrap();
    for path in [
        fixture.state.join("config.json"),
        other.join("other.md"),
        fixture.home.join(".ssh/key"),
    ] {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "isolated private fixture").unwrap();
        assert!(
            tools
                .prepare(
                    Tool::Read,
                    "private",
                    &serde_json::json!({"path":path}).to_string(),
                    &cancel
                )
                .await
                .is_err()
        );
        let call = tools
            .prepare(
                Tool::Shell,
                "private-shell",
                &serde_json::json!({"command":format!("cat '{}'", path.display())}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        let PermissionCheck::Execute(permit) = gate.evaluate(&call, PermissionMode::AllowAll)
        else {
            panic!("sandboxed shell")
        };
        let output = tools.execute(permit, &cancel, &mut |_| {}).await;
        assert!(!output.text.contains("isolated private fixture"));
        assert_ne!(output.outcome, ToolOutcome::Completed);
    }
    let rules = fixture.state.join("rules.json");
    std::fs::write(
        &rules,
        r#"{"fileRules":[{"pattern":"shell.md","decision":"forbidden"}]}"#,
    )
    .unwrap();
    let call = tools
        .prepare(
            Tool::Write,
            "rules",
            &serde_json::json!({"path":roots.global.join("shell.md"),"content":"replacement"})
                .to_string(),
            &cancel,
        )
        .await
        .unwrap();
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
    ] {
        assert!(matches!(
            PermissionGate::default()
                .with_rules_file(rules.clone())
                .evaluate(&call, mode),
            PermissionCheck::Forbidden { .. }
        ));
    }
    tools.close_shell();
}

#[test]
fn memory_indexes_are_sorted_bounded_and_only_include_headers() {
    let fixture = Fixture::new();
    let roots = fixture.memory.roots().unwrap().unwrap();
    use std::os::unix::fs::PermissionsExt;
    for root in [&roots.global, &roots.project] {
        assert_eq!(
            std::fs::metadata(root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    assert_eq!(fixture.memory.index().unwrap(), "");
    for index in 0..201 {
        std::fs::write(
            roots.global.join(format!("{index:03}.md")),
            format!("# Preference {index}\nA useful summary.\n\nBODY MUST NOT BE INDEXED"),
        )
        .unwrap();
    }
    let index = fixture.memory.index().unwrap();
    assert_eq!(index.lines().count(), 200);
    assert!(index.ends_with(
        "Index truncated: 3 more memories are not shown. Use /memory to view the full list."
    ));
    assert!(index.find("Preference 0\"").unwrap() < index.find("Preference 1\"").unwrap());
    assert!(!index.contains("BODY MUST NOT BE INDEXED"));
    assert_eq!(fixture.memory.list().unwrap().len(), 201);
    for entry in fixture.memory.list().unwrap() {
        fixture.memory.delete(entry.scope, &entry.file).unwrap();
    }
    std::fs::write(
        roots.global.join("large.md"),
        format!("# Large\n{}\n\nBody.", "界".repeat(9_000)),
    )
    .unwrap();
    std::fs::write(
        roots.project.join("special.md"),
        "# <Preference>\nA & B.\n\nBody.",
    )
    .unwrap();
    let index = fixture.memory.index().unwrap();
    assert!(index.len() < 25_000);
    assert!(index.contains("Index truncated: 1 more memories"));
    assert!(index.contains("\\u003cPreference\\u003e"));
    assert!(index.contains("A \\u0026 B."));
    std::fs::write(roots.project.join("invalid.md"), "Missing header").unwrap();
    assert!(
        fixture
            .memory
            .list()
            .unwrap()
            .iter()
            .any(|entry| entry.file == "invalid.md")
    );
    assert!(!fixture.memory.index().unwrap().contains("invalid.md"));
}

#[test]
fn memory_project_identity_is_shared_by_git_worktrees_and_separate_for_other_directories() {
    let fixture = Fixture::new();
    let git = |cwd: &std::path::Path, args: &[&str]| {
        let output = std::process::Command::new("git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &fixture.home)
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&fixture.cwd, &["init", "-q"]);
    git(
        &fixture.cwd,
        &[
            "-c",
            "user.name=Memory test",
            "-c",
            "user.email=memory@example.invalid",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    );
    let worktree = fixture._temp.path().join("worktree");
    git(
        &fixture.cwd,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "memory-test",
            worktree.to_str().unwrap(),
        ],
    );
    fixture.memory.set_enabled(true).unwrap();
    let linked = Memory::new(fixture.state.clone(), worktree.clone());
    linked.set_enabled(true).unwrap();
    let roots = fixture.memory.roots().unwrap().unwrap();
    assert_eq!(roots.project, linked.roots().unwrap().unwrap().project);
    let unrelated = fixture._temp.path().join("unrelated");
    std::fs::create_dir(&unrelated).unwrap();
    let separate = Memory::new(fixture.state.clone(), unrelated);
    separate.set_enabled(true).unwrap();
    assert_ne!(roots.project, separate.roots().unwrap().unwrap().project);
    assert_eq!(roots.global, separate.roots().unwrap().unwrap().global);
    std::fs::write(
        roots.project.join("convention.md"),
        "# Convention\nUse the project convention.\n\nDetails.",
    )
    .unwrap();
    assert_eq!(linked.list().unwrap().len(), 1);
    assert!(separate.list().unwrap().is_empty());
}

#[test]
fn disabled_memory_does_not_create_directories() {
    let temp = tempfile::tempdir_in("/tmp").unwrap();
    let memory = Memory::new(
        temp.path().join("missing-state"),
        temp.path().join("missing-project"),
    );
    memory.set_enabled(false).unwrap();
    assert!(memory.roots().unwrap().is_none());
    assert!(memory.list().is_err());
    assert!(!temp.path().join("missing-state").exists());
}

#[tokio::test]
async fn missing_memory_directory_does_not_block_project_files_or_shell() {
    for sandbox in [false, true] {
        for global in [false, true] {
            let fixture = Fixture::new();
            let tools = fixture.tools(sandbox);
            let roots = fixture.memory.roots().unwrap().unwrap();
            let path = fixture.cwd.join("project.txt");
            std::fs::write(&path, "project content").unwrap();
            let cancel = CancellationToken::new();
            let gate = PermissionGate::default();
            let pending_write = tools
                .prepare(
                    Tool::Write,
                    "before-removal",
                    &serde_json::json!({"path":path,"content":"updated project"}).to_string(),
                    &cancel,
                )
                .await
                .unwrap();
            std::fs::remove_dir(if global {
                &roots.global
            } else {
                &roots.project
            })
            .unwrap();
            let read = tools
                .prepare(
                    Tool::Read,
                    "after-removal",
                    &serde_json::json!({"path":path}).to_string(),
                    &cancel,
                )
                .await
                .expect("missing memories must not block reading project files");
            for call in [&read, &pending_write] {
                let PermissionCheck::Execute(permit) =
                    gate.evaluate(call, PermissionMode::AllowAll)
                else {
                    panic!("project file should be allowed")
                };
                let output = tools.execute(permit, &cancel, &mut |_| {}).await;
                assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
            }
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated project");
            let write = tools
                .prepare(
                    Tool::Write,
                    "after-removal",
                    &serde_json::json!({"path":path,"content":"second update"}).to_string(),
                    &cancel,
                )
                .await
                .unwrap();
            let PermissionCheck::Execute(permit) = gate.evaluate(&write, PermissionMode::AllowAll)
            else {
                panic!("project write should be allowed")
            };
            assert_eq!(
                tools.execute(permit, &cancel, &mut |_| {}).await.outcome,
                ToolOutcome::Completed
            );
            let shell = tools
                .prepare(
                    Tool::Shell,
                    "shell-after-removal",
                    r#"{"command":"printf 'shell content' > shell.txt; cat shell.txt"}"#,
                    &cancel,
                )
                .await
                .unwrap();
            let PermissionCheck::Execute(permit) = gate.evaluate(&shell, PermissionMode::AllowAll)
            else {
                panic!("shell should be allowed")
            };
            let output = tools.execute(permit, &cancel, &mut |_| {}).await;
            assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
            assert!(output.text.contains("shell content"));
            for root in [&roots.global, &roots.project] {
                assert!(tools.prepare(Tool::Write, "memory-after-removal", &serde_json::json!({"path":root.join("new.md"),"content":"# Name\nDescription\n\nBody"}).to_string(), &cancel).await.is_err(), "both memory exceptions must be unavailable");
            }
            tools.close_shell();
        }
    }
}
