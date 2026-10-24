use canter_protocol::{Item, ItemStatus};
use canter_tools::{LocalExecutor, Tool, ToolOutcome, Tools};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

fn tools(dir: &tempfile::TempDir) -> Tools {
    let environment = BTreeMap::from([
        ("HOME".into(), dir.path().to_string_lossy().into_owned()),
        ("TMPDIR".into(), dir.path().to_string_lossy().into_owned()),
        ("PATH".into(), "/usr/bin:/bin".into()),
        ("CLIENT_VALUE".into(), "session environment".into()),
    ]);
    Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), environment).unwrap(),
    ))
}

async fn assert_process_tree_stops(cancelled: bool) {
    use std::time::Duration;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("grandchild.sh"), "printf '%s\\n' \"$$\" > grandchild.pid\nprintf 'ready\\n'\nwhile :; do /bin/sleep 1; done\n").unwrap();
    std::fs::write(
        dir.path().join("child.sh"),
        "/bin/sh grandchild.sh &\nprintf '%s\\n' \"$$\" > child.pid\nwait\n",
    )
    .unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let command = "printf '%s\\n' \"$$\" > root.pid; /bin/sh child.sh & wait";
    let raw = serde_json::json!({"command":command,"timeoutMs":if cancelled {60_000} else {1000}})
        .to_string();
    let call = tools
        .prepare(Tool::Shell, "tree", &raw, &cancel)
        .await
        .unwrap();
    let mut streamed = String::new();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tools.execute(call, &cancel, &mut |chunk| {
            streamed.push_str(chunk);
            if cancelled && streamed.contains("ready") {
                cancel.cancel();
            }
        }),
    )
    .await
    .expect("shell must finish on its own timeout or cancellation");
    assert!(
        streamed.contains("ready"),
        "grandchild must have started before termination"
    );
    assert_eq!(
        result.outcome,
        if cancelled {
            ToolOutcome::Cancelled
        } else {
            ToolOutcome::TimedOut
        }
    );
    for file in ["root.pid", "child.pid", "grandchild.pid"] {
        let pid: i32 = std::fs::read_to_string(dir.path().join(file))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = nix::unistd::Pid::from_raw(pid);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match nix::sys::signal::kill(pid, None) {
                    Err(nix::errno::Errno::ESRCH) => break,
                    Ok(()) => tokio::time::sleep(Duration::from_millis(20)).await,
                    Err(error) => panic!("cannot inspect {file}: {error}"),
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{file} with pid {pid} is still alive"));
    }
}

#[tokio::test]
async fn timeout_kills_children_and_grandchildren() {
    assert_process_tree_stops(false).await;
}

#[tokio::test]
async fn cancellation_kills_children_and_grandchildren() {
    assert_process_tree_stops(true).await;
}

#[tokio::test]
async fn shell_preserves_split_utf8_and_truncates_only_the_model_result() {
    let dir = tempfile::tempdir().unwrap();
    let large = format!("{}终", "🙂".repeat(10_000));
    std::fs::write(dir.path().join("large"), &large).unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let command = r"printf '\344'; /bin/sleep 0.05; printf '\270\255'; cat large";
    let call = tools
        .prepare(
            Tool::Shell,
            "utf8",
            &serde_json::json!({"command":command}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    let mut streamed = String::new();
    let result = tools
        .execute(call, &cancel, &mut |chunk| streamed.push_str(chunk))
        .await;
    assert_eq!(result.outcome, ToolOutcome::Completed);
    assert!(
        streamed == format!("中{large}"),
        "streamed UTF-8 differs from command output"
    );
    assert!(
        matches!(result.item, Item::CommandExecution {aggregated_output, ..} if aggregated_output == streamed)
    );
    assert_eq!(
        result.text,
        format!(
            "中{}\n[truncated 7240 bytes]\n{}终\nExit code: Some(0)",
            "🙂".repeat(4095),
            "🙂".repeat(4095)
        )
    );
}

#[tokio::test]
async fn shell_uses_session_directory_and_environment_and_streams_both_outputs() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("input"), "文件\n").unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let command = "printf '%s\\n' \"$CLIENT_VALUE\"; cat input; printf 'stderr\\n' >&2; if [ \"${CARGO_MANIFEST_DIR+x}\" = x ]; then exit 9; fi; exit 7";
    let raw = serde_json::json!({"command":command}).to_string();
    let call = tools
        .prepare(Tool::Shell, "s1", &raw, &cancel)
        .await
        .unwrap();
    let mut streamed = String::new();
    let result = tools
        .execute(call, &cancel, &mut |chunk| streamed.push_str(chunk))
        .await;
    assert_eq!(result.outcome, ToolOutcome::Failed);
    assert!(streamed.contains("session environment\n"));
    assert!(streamed.contains("文件\n"));
    assert!(streamed.contains("stderr\n"));
    match result.item {
        Item::CommandExecution {
            aggregated_output,
            exit_code,
            command: actual,
            cwd,
            status,
            duration_ms,
            ..
        } => {
            assert_eq!(aggregated_output, streamed);
            assert_eq!(exit_code, Some(7));
            assert_eq!(status, ItemStatus::Failed);
            assert_eq!(actual, command);
            assert_eq!(cwd, dir.path().to_string_lossy());
            assert!(duration_ms.is_some());
        }
        item => panic!("unexpected item {item:?}"),
    }
}
