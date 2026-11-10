#![cfg(target_os = "linux")]

mod common;

use canter_tools::{LocalExecutor, Tool, ToolOutcome, Tools};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn unavailable_bubblewrap_explains_opt_out_and_never_runs_the_command() {
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
    let output = tools
        .execute(common::allow(&call), &cancel, &mut |_| {})
        .await;
    assert_eq!(output.outcome, ToolOutcome::Failed);
    println!("{}", output.text.trim());
    assert!(
        output.text.contains(canter_tools::LINUX_SANDBOX_HELP),
        "{}",
        output.text
    );
    assert!(!root.path().join("command-ran").exists());
}

#[tokio::test]
async fn failed_namespace_setup_preserves_the_reason_and_explains_opt_out() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    for name in ["home", "project", "bin"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
    }
    let helper = root.path().join("bin/bwrap");
    std::fs::write(&helper, "#!/bin/sh\nprintf '%s\\n' 'bwrap: Creating new namespace failed: Operation not permitted' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            root.path().join("project"),
            BTreeMap::from([
                (
                    "HOME".into(),
                    root.path().join("home").display().to_string(),
                ),
                ("PATH".into(), root.path().join("bin").display().to_string()),
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
    let output = tools
        .execute(common::allow(&call), &cancel, &mut |_| {})
        .await;
    assert_eq!(output.outcome, ToolOutcome::Failed);
    assert!(
        output.text.contains("Creating new namespace failed"),
        "{}",
        output.text
    );
    assert!(
        output.text.contains(canter_tools::LINUX_SANDBOX_HELP),
        "{}",
        output.text
    );
    assert!(!root.path().join("project/command-ran").exists());
}
