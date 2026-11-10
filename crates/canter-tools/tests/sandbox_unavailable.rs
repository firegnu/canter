#![cfg(target_os = "linux")]

mod common;

use canter_tools::{LocalExecutor, Tool, ToolOutcome, Tools};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn system_bubblewrap_does_not_require_the_sessions_path() {
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
            &json!({"command":"printf ran > command-ran"}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    let output = tools
        .execute(common::allow(&call), &cancel, &mut |_| {})
        .await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert_eq!(
        std::fs::read_to_string(root.path().join("command-ran")).unwrap(),
        "ran"
    );
    canter_tools::probe_linux_sandbox().await.unwrap();
}
