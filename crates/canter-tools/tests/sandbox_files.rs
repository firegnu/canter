#![cfg(target_os = "macos")]

use canter_protocol::{ApprovalDecision, PermissionMode};
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, ToolOutcome, Tools};
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

const SENSITIVE: &str = "This path is protected by the sandbox: credential directories such as ~/.ssh can't be read or changed. If the user really needs it, they can do it themselves.";
const READONLY: &str = "Blocked: the sandbox protects this path (Git config and hooks, or the real Cargo and npm directories). If the user really wants this change, they can make it themselves.";
const OUTSIDE: &str = "Blocked: writing outside the working directory and added directories needs approval, and Allow all mode never asks. If the user really wants it, they can add the directory with /add-dir or make the change themselves.";
const MODES: [PermissionMode; 3] = [
    PermissionMode::AlwaysAsk,
    PermissionMode::AskForExecution,
    PermissionMode::AllowAll,
];

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir_in("/tmp").unwrap());
        for directory in ["project", "home", "state", "added", "outside"] {
            std::fs::create_dir(fixture.path(directory)).unwrap();
        }
        fixture
    }

    fn path(&self, name: &str) -> std::path::PathBuf {
        self.0.path().join(name)
    }

    fn file(&self, name: &str) -> std::path::PathBuf {
        let path = self.path(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "original fixture").unwrap();
        path
    }

    fn tools(&self, sandbox: bool) -> Tools {
        Tools::new(Arc::new(
            LocalExecutor::new(
                self.path("project"),
                BTreeMap::from([
                    ("HOME".into(), self.path("home").display().to_string()),
                    (
                        "CANTER_HOME".into(),
                        self.path("state").display().to_string(),
                    ),
                ]),
                vec![self.path("server-state")],
            )
            .unwrap()
            .with_sandbox(sandbox),
        ))
    }
}

#[tokio::test]
async fn sandbox_git_metadata_and_original_caches_can_be_read_but_never_changed() {
    let fixture = Fixture::new();
    let mut paths: Vec<_> = [
        "home/.cargo/config.toml",
        "home/.npm/cache",
        "project/.git/config",
        "project/.git/config.worktree",
        "project/.git/hooks/pre-commit",
        "project/.git/worktrees/linked/config.worktree",
        "project/.git/worktrees/linked/gitdir",
        "project/.git/worktrees/linked/commondir",
        "project/.git/modules/nested/child/config",
        "project/.git/modules/nested/child/hooks/pre-commit",
        "project/.git/modules/nested/child/gitdir",
        "project/.git/modules/nested/child/commondir",
        "project/.git/modules/nested/child/modules/grandchild/config",
        "project/.git/modules/nested/child/worktrees/linked/config.worktree",
    ]
    .map(|name| fixture.file(name))
    .into();
    // Ordinary worktree and submodule metadata layouts, entirely inside the fixture.
    for name in [
        "project/.git/modules/nested/child",
        "project/.git/modules/nested/child/modules/grandchild",
    ] {
        fixture.file(&format!("{name}/HEAD"));
    }
    let pointer = fixture.path("added/.git");
    std::fs::write(
        &pointer,
        format!(
            "gitdir: {}\n",
            fixture.path("metadata/worktrees/linked").display()
        ),
    )
    .unwrap();
    std::fs::create_dir_all(fixture.path("metadata/worktrees/linked")).unwrap();
    std::fs::write(
        fixture.path("metadata/worktrees/linked/commondir"),
        "../..\n",
    )
    .unwrap();
    paths.extend([
        fixture.file("metadata/config"),
        fixture.file("metadata/hooks/pre-commit"),
        fixture.file("metadata/worktrees/linked/config.worktree"),
        fixture.file("metadata/worktrees/linked/gitdir"),
        fixture.path("metadata/worktrees/linked/commondir"),
        pointer,
    ]);
    std::os::unix::fs::symlink(
        fixture.path("project/.git/config"),
        fixture.path("project/git-config-link"),
    )
    .unwrap();
    paths.push(fixture.path("project/git-config-link"));
    let tools = fixture.tools(true);
    let cancel = CancellationToken::new();
    for mode in MODES {
        tools.configure_shell(mode, &[fixture.path("added").display().to_string()]);
        for path in &paths {
            let original = std::fs::read(path).unwrap();
            let read = tools
                .prepare(Tool::Read, "read", &arguments(Tool::Read, path), &cancel)
                .await
                .unwrap();
            let gate = PermissionGate::default();
            let PermissionCheck::Execute(permit) = gate.evaluate(&read, mode) else {
                panic!("read must not ask")
            };
            let output = tools.execute(permit, &cancel, &mut |_| {}).await;
            assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
            assert!(
                output
                    .text
                    .contains(std::str::from_utf8(&original).unwrap().trim())
            );
            for tool in [Tool::Write, Tool::Edit] {
                let error = tools
                    .prepare(tool, "change", &arguments(tool, path), &cancel)
                    .await
                    .err()
                    .unwrap_or_else(|| panic!("{mode:?} {tool:?} {} was allowed", path.display()));
                assert_eq!(error.text, READONLY, "{}", path.display());
                assert_eq!(std::fs::read(path).unwrap(), original);
            }
        }
    }
}

#[tokio::test]
async fn sandbox_outside_changes_require_once_approval_even_with_a_session_grant() {
    let cancel = CancellationToken::new();
    for mode in MODES {
        for tool in [Tool::Write, Tool::Edit] {
            let fixture = Fixture::new();
            let tools = fixture.tools(true);
            let directories = [fixture
                .path("added")
                .canonicalize()
                .unwrap()
                .display()
                .to_string()];
            tools.configure_shell(mode, &directories);
            let mut gate = PermissionGate::default();
            gate.set_additional_directories(&directories);
            std::os::unix::fs::symlink(fixture.path("added"), fixture.path("project/added-link"))
                .unwrap();
            for name in [
                "project/inside",
                "added/inside",
                "project/added-link/inside",
            ] {
                let path = fixture.file(name);
                let call = tools
                    .prepare(tool, "inside", &arguments(tool, &path), &cancel)
                    .await
                    .unwrap();
                let permit = match gate.evaluate(&call, mode) {
                    PermissionCheck::Execute(permit) => {
                        assert_ne!(mode, PermissionMode::AlwaysAsk);
                        permit
                    }
                    PermissionCheck::Ask(request) => {
                        assert_eq!(mode, PermissionMode::AlwaysAsk);
                        assert!(
                            request
                                .available_decisions
                                .contains(&ApprovalDecision::AcceptForSession)
                        );
                        let PermissionCheck::Execute(permit) =
                            gate.resolve(&call, ApprovalDecision::Accept, None).unwrap()
                        else {
                            panic!("approval must execute")
                        };
                        permit
                    }
                    other => panic!("{other:?}"),
                };
                assert_eq!(
                    tools.execute(permit, &cancel, &mut |_| {}).await.outcome,
                    ToolOutcome::Completed
                );
                assert_eq!(std::fs::read_to_string(path).unwrap(), "changed fixture");
            }
            let inside = fixture.file("project/grant");
            let call = tools
                .prepare(tool, "grant", &arguments(tool, &inside), &cancel)
                .await
                .unwrap();
            gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
                .unwrap();

            std::os::unix::fs::symlink(
                fixture.path("outside"),
                fixture.path("project/outside-link"),
            )
            .unwrap();
            for name in [
                "outside/first",
                "outside/second",
                "project/outside-link/linked",
                "home/.zshrc",
            ] {
                let path = fixture.file(name);
                let call = tools
                    .prepare(tool, "outside", &arguments(tool, &path), &cancel)
                    .await
                    .unwrap();
                assert!(matches!(
                    gate.evaluate_plan(&call, mode),
                    PermissionCheck::Deny { .. }
                ));
                assert!(matches!(
                    gate.evaluate_review(&call),
                    PermissionCheck::Deny { .. }
                ));
                match gate.evaluate(&call, mode) {
                    PermissionCheck::Forbidden { reason } => {
                        assert_eq!(mode, PermissionMode::AllowAll);
                        assert_eq!(reason, OUTSIDE);
                        assert_eq!(std::fs::read_to_string(path).unwrap(), "original fixture");
                    }
                    PermissionCheck::Ask(request) => {
                        assert_ne!(mode, PermissionMode::AllowAll);
                        assert_eq!(
                            request.available_decisions,
                            [
                                ApprovalDecision::Accept,
                                ApprovalDecision::Decline,
                                ApprovalDecision::DeclineWithFeedback,
                                ApprovalDecision::Cancel
                            ]
                        );
                        assert!(
                            gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
                                .is_err()
                        );
                        let PermissionCheck::Execute(permit) =
                            gate.resolve(&call, ApprovalDecision::Accept, None).unwrap()
                        else {
                            panic!("once approval must execute")
                        };
                        assert_eq!(
                            tools.execute(permit, &cancel, &mut |_| {}).await.outcome,
                            ToolOutcome::Completed
                        );
                        assert_eq!(std::fs::read_to_string(path).unwrap(), "changed fixture");
                    }
                    other => panic!("{mode:?} {tool:?} {name}: {other:?}"),
                }
            }
            let outside = fixture.file("outside/read");
            let read = tools
                .prepare(
                    Tool::Read,
                    "read",
                    &arguments(Tool::Read, &outside),
                    &cancel,
                )
                .await
                .unwrap();
            let PermissionCheck::Execute(permit) = gate.evaluate(&read, mode) else {
                panic!("read outside must not ask")
            };
            let output = tools.execute(permit, &cancel, &mut |_| {}).await;
            assert_eq!(output.outcome, ToolOutcome::Completed);
            assert!(output.text.contains("original fixture"));
        }
    }
}

#[tokio::test]
async fn sandbox_new_files_use_the_real_nearest_parent_directory() {
    let fixture = Fixture::new();
    std::os::unix::fs::symlink(
        fixture.path("outside"),
        fixture.path("project/outside-link"),
    )
    .unwrap();
    std::os::unix::fs::symlink(fixture.path("added"), fixture.path("project/added-link")).unwrap();
    let tools = fixture.tools(true);
    let directories = [fixture
        .path("added")
        .canonicalize()
        .unwrap()
        .display()
        .to_string()];
    tools.configure_shell(PermissionMode::AllowAll, &directories);
    let mut gate = PermissionGate::default();
    gate.set_additional_directories(&directories);
    let cancel = CancellationToken::new();
    for (name, outside) in [
        ("new/deep/file", false),
        ("added-link/new/deep/file", false),
        ("outside-link/new/deep/file", true),
        ("../outside/new/deep/file", true),
        ("../project/../outside/new/deep/file", true),
    ] {
        let call = tools
            .prepare(
                Tool::Write,
                "new",
                &arguments(Tool::Write, Path::new(name)),
                &cancel,
            )
            .await
            .unwrap();
        match gate.evaluate(&call, PermissionMode::AllowAll) {
            PermissionCheck::Execute(permit) => {
                assert!(!outside, "{name}");
                assert_eq!(
                    tools.execute(permit, &cancel, &mut |_| {}).await.outcome,
                    ToolOutcome::Completed
                );
                assert_eq!(
                    std::fs::read_to_string(fixture.path("project").join(name)).unwrap(),
                    "changed fixture"
                );
            }
            PermissionCheck::Forbidden { reason } => {
                assert!(outside, "{name}");
                assert_eq!(reason, OUTSIDE);
                assert!(!fixture.path("outside/new").exists());
            }
            other => panic!("{other:?}"),
        }
    }
}

#[tokio::test]
async fn disabled_sandbox_preserves_file_access_modes_and_outside_session_approval() {
    let cancel = CancellationToken::new();
    for mode in MODES {
        for tool in [Tool::Read, Tool::Write, Tool::Edit] {
            let fixture = Fixture::new();
            let tools = fixture.tools(false);
            tools.configure_shell(mode, &[]);
            for name in [
                "project/file",
                "outside/file",
                "home/.ssh/key",
                "home/.aws/config",
                "home/.gnupg/key",
                "home/.cargo/config.toml",
                "home/.npm/cache",
                "project/.git/config",
                "project/.git/hooks/pre-commit",
            ] {
                let path = fixture.file(name);
                let call = tools
                    .prepare(tool, "legacy", &arguments(tool, &path), &cancel)
                    .await
                    .unwrap();
                let mut gate = PermissionGate::default();
                let should_ask = tool != Tool::Read
                    && match mode {
                        PermissionMode::AlwaysAsk => true,
                        PermissionMode::AskForExecution => !name.starts_with("project/"),
                        PermissionMode::AllowAll => false,
                        _ => unreachable!(),
                    };
                let permit = match gate.evaluate(&call, mode) {
                    PermissionCheck::Ask(request) => {
                        assert!(should_ask);
                        assert!(
                            request
                                .available_decisions
                                .contains(&ApprovalDecision::AcceptForSession)
                        );
                        let PermissionCheck::Execute(permit) = gate
                            .resolve(&call, ApprovalDecision::AcceptForSession, None)
                            .unwrap()
                        else {
                            panic!("session approval must execute")
                        };
                        permit
                    }
                    PermissionCheck::Execute(permit) => {
                        assert!(!should_ask);
                        permit
                    }
                    other => panic!("{other:?}"),
                };
                let output = tools.execute(permit, &cancel, &mut |_| {}).await;
                assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
                assert_eq!(
                    std::fs::read_to_string(path).unwrap(),
                    if tool == Tool::Read {
                        "original fixture"
                    } else {
                        "changed fixture"
                    }
                );
                if should_ask {
                    let later = fixture.file("outside/later");
                    let later = tools
                        .prepare(tool, "later", &arguments(tool, &later), &cancel)
                        .await
                        .unwrap();
                    assert!(matches!(
                        gate.evaluate(&later, mode),
                        PermissionCheck::Execute(_)
                    ));
                }
            }
            let state = fixture.file("server-state/config.json");
            let error = tools
                .prepare(tool, "private", &arguments(tool, &state), &cancel)
                .await
                .err()
                .unwrap();
            assert_eq!(error.text, "this file is private to canter");
        }
    }
}

fn arguments(tool: Tool, path: &Path) -> String {
    match tool {
        Tool::Read => serde_json::json!({"path": path}),
        Tool::Write => serde_json::json!({"path": path, "content": "changed fixture"}),
        Tool::Edit => serde_json::json!({"path": path, "edits": [{"oldText": "original", "newText": "changed"}]}),
        _ => unreachable!(),
    }
    .to_string()
}

#[tokio::test]
async fn sandbox_sensitive_files_are_rejected_before_read_or_diff_in_every_mode() {
    let fixture = Fixture::new();
    let paths: Vec<_> = [
        "home/.ssh/key",
        "home/.aws/config",
        "home/.gnupg/key",
        "home/.canter/config.json",
        "state/config.json",
        "server-state/config.json",
    ]
    .map(|name| fixture.file(name))
    .into();
    std::os::unix::fs::symlink(fixture.path("home/.ssh"), fixture.path("project/ssh-link"))
        .unwrap();
    let tools = fixture.tools(true);
    let cancel = CancellationToken::new();
    for mode in MODES {
        tools.configure_shell(mode, &[]);
        for tool in [Tool::Read, Tool::Write, Tool::Edit] {
            // Even an existing tool-wide session grant cannot unlock a hard boundary.
            let ordinary = fixture.file("project/ordinary");
            let call = tools
                .prepare(tool, "grant", &arguments(tool, &ordinary), &cancel)
                .await
                .unwrap();
            let mut gate = PermissionGate::default();
            gate.resolve(&call, ApprovalDecision::AcceptForSession, None)
                .unwrap();
            for path in paths.iter().chain([&fixture.path("project/ssh-link/key")]) {
                let result = tools
                    .prepare(tool, "protected", &arguments(tool, path), &cancel)
                    .await;
                let error = result
                    .err()
                    .unwrap_or_else(|| panic!("{mode:?} {tool:?} {} was allowed", path.display()));
                assert_eq!(error.outcome, ToolOutcome::Failed);
                assert_eq!(
                    error.text,
                    if path.starts_with(fixture.path("server-state")) {
                        "this file is private to canter"
                    } else {
                        SENSITIVE
                    }
                );
                assert!(!format!("{:?}", error.item).contains("original fixture"));
                assert_eq!(std::fs::read_to_string(path).unwrap(), "original fixture");
            }
        }
    }
}
