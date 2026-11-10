#![cfg(target_os = "linux")]

mod common;

use canter_protocol::PermissionMode;
use canter_tools::{LocalExecutor, Tool, ToolOutcome, Tools};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn unavailable_sandbox_explains_opt_out_and_never_runs_tools() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("existing"), "original").unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(root.path().into(), BTreeMap::new(), vec![])
            .unwrap()
            .with_sandbox(true),
    ));
    tools.configure_shell(PermissionMode::AllowAll, &[]);
    let cancel = CancellationToken::new();
    for (tool, args) in [
        (Tool::Shell, json!({"command":"touch command-ran"})),
        (Tool::Write, json!({"path":"created", "content":"new"})),
        (
            Tool::Edit,
            json!({"path":"existing", "edits":[{"oldText":"original", "newText":"changed"}]}),
        ),
        (Tool::Read, json!({"path":"existing"})),
    ] {
        let output = match tools
            .prepare(tool, "blocked", &args.to_string(), &cancel)
            .await
        {
            Ok(call) => {
                tools
                    .execute(common::allow(&call), &cancel, &mut |_| {})
                    .await
            }
            Err(output) => output,
        };
        assert_eq!(output.outcome, ToolOutcome::Failed);
        println!("{}", output.text.trim());
        assert!(
            output
                .text
                .contains("Sandbox is not available on Linux yet"),
            "{}",
            output.text
        );
        assert!(
            output.text.contains("\"sandbox\": false"),
            "{}",
            output.text
        );
        assert!(output.text.contains("--yolo"), "{}", output.text);
    }
    assert!(!root.path().join("command-ran").exists());
    assert!(!root.path().join("created").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("existing")).unwrap(),
        "original"
    );
}
