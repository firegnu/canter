use crate::{LocalExecutor, Tool, ToolOutcome, Tools};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

thread_local! {
    pub(super) static BWRAP: std::cell::RefCell<Option<Option<std::path::PathBuf>>> = const { std::cell::RefCell::new(None) };
    pub(super) static FOREIGN_DIRECTORY: std::cell::RefCell<Option<std::path::PathBuf>> = const { std::cell::RefCell::new(None) };
}

struct Overrides;

impl Drop for Overrides {
    fn drop(&mut self) {
        BWRAP.with(|value| *value.borrow_mut() = None);
        FOREIGN_DIRECTORY.with(|value| *value.borrow_mut() = None);
    }
}

fn allow(call: &crate::PreparedCall) -> crate::ExecutionPermit<'_> {
    match crate::PermissionGate::default().evaluate(call, canter_protocol::PermissionMode::AllowAll)
    {
        crate::PermissionCheck::Execute(permit) => permit,
        _ => panic!("allowAll must issue a permit"),
    }
}

#[tokio::test]
async fn foreign_unreadable_project_directory_does_not_block_shell() {
    use std::os::unix::fs::PermissionsExt;
    let _overrides = Overrides;
    let root = tempfile::tempdir().unwrap();
    for name in ["home", "project", "project/database"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    let blocked = root.path().join("project/database");
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        std::fs::read_dir(&blocked).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    // Unprivileged tests cannot chown to another user. Keep the real EACCES
    // from this fixture, substituting only its ownership classification.
    FOREIGN_DIRECTORY.with(|value| *value.borrow_mut() = Some(blocked.clone()));
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            root.path().join("project"),
            BTreeMap::from([
                (
                    "HOME".into(),
                    root.path().join("home").display().to_string(),
                ),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ]),
            vec![],
        )
        .unwrap()
        .with_sandbox(true),
    ));
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "foreign-directory",
            r#"{"command":"printf ran > result"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let output = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    std::fs::set_permissions(&blocked, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert_eq!(
        std::fs::read_to_string(root.path().join("project/result")).unwrap(),
        "ran"
    );
}

#[tokio::test]
async fn unavailable_bubblewrap_explains_opt_out_and_never_runs_the_command() {
    let _overrides = Overrides;
    BWRAP.with(|value| *value.borrow_mut() = Some(None));
    let root = tempfile::tempdir().unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            root.path().into(),
            BTreeMap::from([
                ("HOME".into(), root.path().display().to_string()),
                ("PATH".into(), String::new()),
            ]),
            vec![],
        )
        .unwrap()
        .with_sandbox(true),
    ));
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "blocked",
            &json!({"command":"touch command-ran"}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    let output = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert_eq!(output.outcome, ToolOutcome::Failed);
    println!("{}", output.text.trim());
    assert!(
        output.text.contains(crate::LINUX_SANDBOX_HELP),
        "{}",
        output.text
    );
    assert!(!root.path().join("command-ran").exists());
}

#[tokio::test]
async fn failed_namespace_setup_preserves_the_reason_and_explains_opt_out() {
    let _overrides = Overrides;
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    for name in ["home", "project", "bin"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    let helper = root.path().join("bin/bwrap");
    std::fs::write(&helper, "#!/bin/sh\nprintf '%s\\n' 'bwrap: Creating new namespace failed: Operation not permitted' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    BWRAP.with(|value| *value.borrow_mut() = Some(Some(helper)));
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            root.path().join("project"),
            BTreeMap::from([
                (
                    "HOME".into(),
                    root.path().join("home").display().to_string(),
                ),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ]),
            vec![],
        )
        .unwrap()
        .with_sandbox(true),
    ));
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "namespace",
            r#"{"command":"touch command-ran"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let output = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert_eq!(output.outcome, ToolOutcome::Failed);
    assert!(
        output.text.contains("Creating new namespace failed"),
        "{}",
        output.text
    );
    assert!(
        output.text.contains(crate::LINUX_SANDBOX_HELP),
        "{}",
        output.text
    );
    assert!(!root.path().join("project/command-ran").exists());
}
