mod common;
use canter_protocol::{Item, ItemStatus};
use canter_tools::{LocalExecutor, Tool, ToolOutcome, Tools};
use common::allow;
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
        LocalExecutor::new(dir.path().into(), environment, vec![]).unwrap(),
    ))
}

// The command intentionally escapes our process group. Always clean up its recorded PID,
// including when a regression makes the outer timeout or an assertion fail.
struct EscapedProcess(std::path::PathBuf);
impl Drop for EscapedProcess {
    fn drop(&mut self) {
        if let Ok(text) = std::fs::read_to_string(&self.0)
            && let Ok(pid) = text.trim().parse::<i32>()
        {
            let _ = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }
}

#[tokio::test]
async fn escaped_output_holders_cannot_block_exit_timeout_or_cancellation() {
    use std::time::Duration;
    for mode in [
        "cancel_during_drain",
        "timeout_during_drain",
        "cancel_during_drain_failed",
        "timeout_during_drain_failed",
        "exit",
        "cancel",
        "timeout",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let _escaped = EscapedProcess(dir.path().join("escaped.pid"));
        let tools = tools(&dir);
        let cancel = CancellationToken::new();
        let command = format!(
            "set -m; /bin/sleep 30 & echo $! > escaped.pid; echo started; {}",
            if mode == "cancel" || mode == "timeout" {
                "/bin/sleep 30"
            } else if mode.ends_with("failed") {
                "exit 7"
            } else {
                ":"
            }
        );
        let raw = serde_json::json!({"command":command,"timeoutMs":if mode.starts_with("timeout") {300} else {10_000}}).to_string();
        let call = tools
            .prepare(Tool::Shell, "escaped", &raw, &cancel)
            .await
            .unwrap();
        let mut streamed = String::new();
        let mut on_output = |chunk: &str| streamed.push_str(chunk);
        let execute = tools.execute(allow(&call), &cancel, &mut on_output);
        let cancel_later = async {
            if mode.starts_with("cancel") {
                tokio::time::sleep(Duration::from_millis(300)).await;
                cancel.cancel();
            }
        };
        let (result, ()) = tokio::join!(
            tokio::time::timeout(Duration::from_secs(5), execute),
            cancel_later
        );
        let result = result.expect("escaped output pipe must not block the tool forever");
        assert!(
            result
                .text
                .contains("Background processes still hold output pipes"),
            "{mode}: {}",
            result.text
        );
        assert!(
            matches!(&result.item, Item::CommandExecution {aggregated_output, ..} if aggregated_output.contains("Background processes still hold output pipes"))
        );
        if mode.contains("during_drain") {
            let (code, status) = if mode.ends_with("failed") {
                (7, ItemStatus::Failed)
            } else {
                (0, ItemStatus::Completed)
            };
            assert!(
                result.text.contains(&format!("Exit code: {code}")),
                "{mode}: {}",
                result.text
            );
            assert!(result.text.contains(if mode.starts_with("cancel") {
                "Output reading was cancelled"
            } else {
                "Output timeout reached"
            }));
            assert!(
                !result.text.contains("Command timed out")
                    && !result.text.contains("Command cancelled")
            );
            assert!(
                matches!(&result.item, Item::CommandExecution {exit_code, status:actual_status, ..} if *exit_code == Some(code) && *actual_status == status)
            );
        }
        assert_eq!(
            result.outcome,
            if mode.ends_with("failed") {
                ToolOutcome::Failed
            } else if mode.contains("during_drain") {
                ToolOutcome::Completed
            } else if mode == "cancel" {
                ToolOutcome::Cancelled
            } else if mode == "timeout" {
                ToolOutcome::TimedOut
            } else {
                ToolOutcome::Completed
            },
            "{mode}"
        );
    }
}

#[tokio::test]
async fn cancellation_and_original_timeout_remain_active_after_the_other_trigger() {
    use std::time::{Duration, Instant};
    for cancel_first in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let _escaped = EscapedProcess(dir.path().join("escaped.pid"));
        let tools = tools(&dir);
        let cancel = CancellationToken::new();
        let raw = serde_json::json!({"command":"set -m; /bin/sleep 30 & echo $! > escaped.pid; /bin/sleep 30", "timeoutMs":if cancel_first {600} else {300}}).to_string();
        let call = tools
            .prepare(Tool::Shell, "drain", &raw, &cancel)
            .await
            .unwrap();
        let mut sink = |_: &str| {};
        let start = Instant::now();
        let (result, ()) = tokio::join!(tools.execute(allow(&call), &cancel, &mut sink), async {
            tokio::time::sleep(Duration::from_millis(if cancel_first { 300 } else { 600 })).await;
            cancel.cancel();
        });
        assert!(
            start.elapsed() < Duration::from_millis(1500),
            "second trigger was ignored during drain"
        );
        assert_eq!(
            result.outcome,
            if cancel_first {
                ToolOutcome::Cancelled
            } else {
                ToolOutcome::TimedOut
            }
        );
    }
}

#[tokio::test]
async fn client_output_and_deltas_are_bounded_with_utf8_head_and_tail() {
    let dir = tempfile::tempdir().unwrap();
    let large = format!("开始{}结尾", "🙂".repeat(600_000));
    std::fs::write(dir.path().join("large"), &large).unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::Shell, "large", r#"{"command":"cat large"}"#, &cancel)
        .await
        .unwrap();
    let mut streamed = String::new();
    let result = tools
        .execute(allow(&call), &cancel, &mut |chunk| streamed.push_str(chunk))
        .await;
    let Item::CommandExecution {
        aggregated_output, ..
    } = result.item
    else {
        panic!("wrong item")
    };
    assert!(
        aggregated_output.len() <= 1024 * 1024,
        "client output exceeds 1 MiB"
    );
    assert!(streamed.len() <= 1024 * 1024, "deltas exceed 1 MiB");
    assert!(large.starts_with(&streamed));
    assert!(aggregated_output.starts_with("开始"));
    assert!(aggregated_output.ends_with("结尾"));
    let (head, rest) = aggregated_output.split_once("\n[truncated ").unwrap();
    let (removed, tail) = rest.split_once(" bytes]\n").unwrap();
    assert_eq!(
        head.len() + tail.len() + removed.parse::<usize>().unwrap(),
        large.len()
    );
    assert!(!aggregated_output.contains('\u{fffd}'));
    assert!(result.text.len() < 34 * 1024);
}

#[tokio::test]
async fn cancellation_allows_term_cleanup_then_kills_uncooperative_processes() {
    for cooperative in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(&dir);
        let cancel = CancellationToken::new();
        let trap = if cooperative {
            "trap 'echo cleaned > cleanup; exit 0' TERM"
        } else {
            "trap '' TERM"
        };
        // Stay in shell builtins so a fork racing SIGTERM cannot defer the trap.
        let command = format!("{trap}; echo $$ > root.pid; echo ready; while :; do :; done");
        let call = tools
            .prepare(
                Tool::Shell,
                "term",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        let mut output = String::new();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tools.execute(allow(&call), &cancel, &mut |chunk| {
                output.push_str(chunk);
                if output.contains("ready") {
                    cancel.cancel();
                }
            }),
        )
        .await
        .unwrap();
        assert_eq!(result.outcome, ToolOutcome::Cancelled);
        assert_eq!(
            dir.path().join("cleanup").exists(),
            cooperative,
            "TERM trap must get a chance to clean up"
        );
        let pid = std::fs::read_to_string(dir.path().join("root.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH)
        );
    }
}

#[tokio::test]
async fn shell_reports_exit_status_and_signals() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    for (command, expected) in [
        ("exit 0", "Exit code: 0"),
        ("kill -KILL $$", "Killed by signal 9"),
    ] {
        let call = tools
            .prepare(
                Tool::Shell,
                "status",
                &serde_json::json!({"command":command}).to_string(),
                &cancel,
            )
            .await
            .unwrap();
        let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
        assert!(result.text.contains(expected), "{}", result.text);
    }
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
        tools.execute(allow(&call), &cancel, &mut |chunk| {
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
        .execute(allow(&call), &cancel, &mut |chunk| streamed.push_str(chunk))
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
            "中{}\n[truncated 7240 bytes]\n{}终\nExit code: 0",
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
        .execute(allow(&call), &cancel, &mut |chunk| streamed.push_str(chunk))
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
