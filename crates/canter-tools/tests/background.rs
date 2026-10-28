mod common;
use canter_protocol::PermissionMode;
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, Tools};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn tools(dir: &tempfile::TempDir) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(
            dir.path().into(),
            BTreeMap::from([
                ("HOME".into(), dir.path().to_string_lossy().into_owned()),
                (
                    "CANTER_HOME".into(),
                    dir.path().join("state").to_string_lossy().into_owned(),
                ),
                ("PATH".into(), "/usr/bin:/bin".into()),
            ]),
            vec![],
        )
        .unwrap(),
    ))
}

#[tokio::test]
async fn background_shell_returns_while_process_keeps_running_and_still_needs_approval() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Shell,
            "service",
            r#"{"command":"printf ready; exec sleep 30","background":true,"timeoutMs":1}"#,
            &cancel,
        )
        .await
        .expect("shell must accept background");
    assert!(matches!(
        PermissionGate::default().evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Ask(_)
    ));
    let output = tokio::time::timeout(
        Duration::from_secs(5),
        tools.execute(common::allow(&call), &cancel, &mut |_| {}),
    )
    .await
    .unwrap();
    assert!(
        output.text.contains("Background command 1"),
        "{}",
        output.text
    );
    assert!(output.text.contains("running"), "{}", output.text);
    assert!(output.text.contains("ready"));
    let stop = tools
        .prepare_named("shell_stop", "stop".into(), r#"{"id":1}"#, &cancel)
        .await
        .unwrap();
    let stopped = tools
        .execute(common::allow(&stop), &cancel, &mut |_| {})
        .await;
    assert!(stopped.text.contains("exited"), "{}", stopped.text);
}

async fn call(tools: &Tools, name: &str, args: serde_json::Value) -> canter_tools::ToolOutput {
    let cancel = CancellationToken::new();
    let call = tools
        .prepare_named(name, name.into(), &args.to_string(), &cancel)
        .await
        .unwrap();
    tools
        .execute(common::allow(&call), &cancel, &mut |_| {})
        .await
}

#[tokio::test]
async fn completed_background_output_is_read_once_and_ui_reads_do_not_consume_it() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let initial = call(&tools, "shell", serde_json::json!({"command":"printf ready; while [ ! -e release ]; do sleep 0.01; done; printf final; exit 7", "background":true})).await;
    assert!(initial.text.contains("ready"));
    assert!(
        !call(&tools, "shell_output", serde_json::json!({"id":1}))
            .await
            .text
            .contains("ready")
    );
    std::fs::write(dir.path().join("release"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while tools.background().list()[0].running {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        tools
            .background()
            .output(1, false)
            .unwrap()
            .output
            .contains("readyfinal")
    );
    let last = call(&tools, "shell_output", serde_json::json!({"id":1})).await;
    assert!(
        last.text.contains("final"),
        "remaining output was lost: {}",
        last.text
    );
    assert!(last.text.contains("exit code: 7"));
    assert!(
        !call(&tools, "shell_output", serde_json::json!({"id":1}))
            .await
            .text
            .contains("final")
    );
}

#[tokio::test]
async fn foreground_can_move_to_background_without_turn_cancellation_stopping_it() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let prepared = tools
        .prepare(
            Tool::Shell,
            "foreground",
            r#"{"command":"printf ready; exec sleep 30"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let ready = tokio::sync::Notify::new();
    let mut stream = |_: &str| ready.notify_one();
    let execute = tools.execute(common::allow(&prepared), &cancel, &mut stream);
    let promote = async {
        ready.notified().await;
        assert_eq!(tools.background().promote("stale").await.unwrap(), None);
        assert_eq!(
            tools.background().promote("foreground").await.unwrap(),
            Some(1)
        );
        cancel.cancel();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(execute, promote)
    })
    .await
    .unwrap();
    assert!(result.text.contains("Background command 1: running"));
    assert!(result.text.contains("ready"));
    assert!(tools.background().list()[0].running);
    tools.background().stop_all().await;
}

#[tokio::test]
async fn concurrent_promotions_both_return_while_the_command_is_running() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let prepared = tools
        .prepare(
            Tool::Shell,
            "foreground",
            r#"{"command":"printf ready; exec sleep 30"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let ready = tokio::sync::Notify::new();
    let mut stream = |_: &str| ready.notify_one();
    let execute = tools.execute(common::allow(&prepared), &cancel, &mut stream);
    let promote = async {
        ready.notified().await;
        let replies = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                tools.background().promote("foreground"),
                tools.background().promote("foreground")
            )
        })
        .await;
        let running = tools.background().list()[0].running;
        tools.background().stop_all().await;
        assert!(running);
        let (first, second) = replies.expect("both promotion requests must return before exit");
        assert_eq!(first.unwrap(), Some(1));
        assert!(matches!(second.unwrap(), Some(1) | None));
    };
    tokio::join!(execute, promote);
}

#[tokio::test]
async fn background_result_completes_the_tool_call_while_the_process_runs() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let output = call(
        &tools,
        "shell",
        serde_json::json!({"background":true,"command":"exec sleep 30"}),
    )
    .await;
    let running = tools.background().list()[0].running;
    tools.background().stop_all().await;
    assert!(running);
    assert!(matches!(
        output.item,
        canter_protocol::Item::CommandExecution {
            status: canter_protocol::ItemStatus::Completed,
            exit_code: None,
            ..
        }
    ));
}

#[tokio::test]
async fn background_spawn_failure_does_not_register_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    std::fs::remove_dir(dir.path()).unwrap();
    let output = call(
        &tools,
        "shell",
        serde_json::json!({"background":true,"command":"printf unreachable"}),
    )
    .await;
    assert_eq!(output.outcome, canter_tools::ToolOutcome::Failed);
    assert!(
        tools.background().list().is_empty(),
        "failed starts must not produce completion notices"
    );
    std::fs::create_dir(dir.path()).unwrap();
    let next = call(
        &tools,
        "shell",
        serde_json::json!({"background":true,"command":"printf ready"}),
    )
    .await;
    assert!(next.text.contains("Background command 1:"));
}

#[tokio::test]
async fn completed_output_keeps_only_the_last_eight_completions() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    for id in 1..=9 {
        call(
            &tools,
            "shell",
            serde_json::json!({"background":true,"command":format!("printf output-{id}")}),
        )
        .await;
    }
    let old = tools.background().output(1, false).unwrap();
    assert_eq!(old.command.exit_code, Some(0));
    assert!(old.output.contains("Output discarded"), "{}", old.output);
    for id in 2..=9 {
        assert_eq!(
            tools.background().output(id, false).unwrap().output,
            format!("output-{id}")
        );
    }
    let model = call(&tools, "shell_output", serde_json::json!({"id":1})).await;
    assert!(model.text.contains("Output discarded"));
    assert_eq!(tools.background().list().len(), 9);
}

#[tokio::test]
async fn background_retains_one_megabyte_and_model_reads_only_last_32k_of_new_output() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    call(&tools, "shell", serde_json::json!({"background":true,"command":"printf ready; while [ ! -e release ]; do sleep 0.01; done; head -c 1200000 /dev/zero | tr '\\0' x; printf 終わり"})).await;
    std::fs::write(dir.path().join("release"), "").unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while tools.background().list()[0].running {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let ui = tools.background().output(1, false).unwrap();
    assert_eq!(ui.output.len(), 1024 * 1024);
    assert_eq!(ui.skipped_bytes, 1_200_014 - 1024 * 1024);
    assert!(ui.output.ends_with("終わり"));
    let new = tools.background().output(1, true).unwrap();
    assert_eq!(new.output.len(), 32 * 1024);
    assert_eq!(new.skipped_bytes, 1_200_009 - 32 * 1024);
    assert!(new.output.ends_with("終わり"));
    assert!(
        tools
            .background()
            .output(1, true)
            .unwrap()
            .output
            .is_empty()
    );
}

fn process_gone(pid: i32) -> bool {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) == Err(nix::errno::Errno::ESRCH)
}
async fn gone(pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !process_gone(pid) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned child process must be gone");
}

#[tokio::test]
async fn stopping_or_leader_exit_cleans_up_the_whole_group_and_immediate_exit_has_output() {
    for finish in ["wait", "exit 7"] {
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(&dir);
        let output = call(&tools, "shell", serde_json::json!({"background":true,"command":format!("sleep 30 & echo $!; {finish}")})).await;
        let pid: i32 = tools
            .background()
            .output(1, false)
            .unwrap()
            .output
            .trim()
            .parse()
            .unwrap();
        if finish == "wait" {
            assert!(output.text.contains("running"));
            let (a, b) = tokio::join!(tools.background().stop(1), tools.background().stop(1));
            assert!(!a.unwrap().command.running);
            assert!(!b.unwrap().command.running);
        } else {
            assert!(output.text.contains("exit code: 7"));
        }
        gone(pid).await;
    }
}

#[tokio::test]
async fn ninth_background_is_rejected_before_spawn_and_dropping_session_cleans_up() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    for _ in 0..8 {
        call(
            &tools,
            "shell",
            serde_json::json!({"background":true,"command":"echo $$; exec sleep 30"}),
        )
        .await;
    }
    let ninth = call(
        &tools,
        "shell",
        serde_json::json!({"background":true,"command":"touch ninth; exec sleep 30"}),
    )
    .await;
    assert!(ninth.text.contains("At most 8"));
    assert!(!dir.path().join("ninth").exists());
    assert_eq!(tools.background().list().len(), 8);
    let cancel = CancellationToken::new();
    let foreground = tools
        .prepare(
            Tool::Shell,
            "full",
            r#"{"command":"printf still-foreground; exec sleep 30"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let ready = tokio::sync::Notify::new();
    let mut stream = |_: &str| ready.notify_one();
    let execute = tools.execute(common::allow(&foreground), &cancel, &mut stream);
    let promote = async {
        ready.notified().await;
        assert!(
            tools
                .background()
                .promote("full")
                .await
                .unwrap_err()
                .contains("At most 8")
        );
        cancel.cancel();
    };
    let (output, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(execute, promote)
    })
    .await
    .unwrap();
    assert_eq!(output.outcome, canter_tools::ToolOutcome::Cancelled);
    assert!(output.text.contains("still-foreground"));
    let pids: Vec<i32> = (1..=8)
        .map(|id| {
            tools
                .background()
                .output(id, false)
                .unwrap()
                .output
                .trim()
                .parse()
                .unwrap()
        })
        .collect();
    drop(tools);
    for pid in pids {
        gone(pid).await;
    }
}

#[tokio::test]
async fn promotion_racing_exit_finishes_once_and_never_leaves_a_running_entry() {
    for _ in 0..4 {
        let dir = tempfile::tempdir().unwrap();
        let tools = tools(&dir);
        let cancel = CancellationToken::new();
        let foreground = tools.prepare(Tool::Shell, "race", r#"{"command":"printf ready; while [ ! -e release ]; do sleep 0.01; done; printf final"}"#, &cancel).await.unwrap();
        let ready = tokio::sync::Notify::new();
        let mut stream = |_: &str| ready.notify_one();
        let execute = tools.execute(common::allow(&foreground), &cancel, &mut stream);
        let promote = async {
            ready.notified().await;
            std::fs::write(dir.path().join("release"), "").unwrap();
            tools.background().promote("race").await.unwrap()
        };
        let (output, id) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(execute, promote)
        })
        .await
        .unwrap();
        assert_eq!(output.outcome, canter_tools::ToolOutcome::Completed);
        if let Some(id) = id {
            tokio::time::timeout(Duration::from_secs(5), async {
                while tools
                    .background()
                    .output(id, false)
                    .unwrap()
                    .command
                    .running
                {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(
                tools
                    .background()
                    .output(id, false)
                    .unwrap()
                    .command
                    .exit_code,
                Some(0)
            );
        } else {
            assert!(output.text.contains("Exit code: 0"));
        }
    }
}

#[tokio::test]
async fn stop_racing_natural_exit_is_idempotent_and_keeps_final_output() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    call(&tools, "shell", serde_json::json!({"background":true,"command":"printf ready; while [ ! -e release ]; do sleep 0.01; done; printf final"})).await;
    std::fs::write(dir.path().join("release"), "").unwrap();
    let (first, second) = tokio::join!(tools.background().stop(1), tools.background().stop(1));
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    assert!(!first.command.running);
    assert!(first.output.starts_with("ready"));
    assert_eq!(tools.background().stop(1).await.unwrap(), first);
}

#[test]
fn trailing_single_ampersand_is_removed_but_and_and_is_not() {
    assert_eq!(
        canter_tools::split_background_command("npm run dev &  \n"),
        ("npm run dev", true)
    );
    assert_eq!(
        canter_tools::split_background_command("a && b"),
        ("a && b", false)
    );
    assert_eq!(
        canter_tools::split_background_command("a &&"),
        ("a &&", false)
    );
}

#[tokio::test]
async fn plan_mode_keeps_shell_whitelist_and_allows_background_management() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    for (command, allowed) in [("git status", true), ("touch forbidden", false)] {
        let args = serde_json::json!({"command":command,"background":true}).to_string();
        assert_eq!(tools.allowed_in_plan_mode("shell", &args), allowed);
        let call = tools
            .prepare(Tool::Shell, "plan", &args, &cancel)
            .await
            .unwrap();
        assert_eq!(
            matches!(
                PermissionGate::default().evaluate_plan(&call, PermissionMode::AllowAll),
                PermissionCheck::Execute(_)
            ),
            allowed
        );
    }
    for tool in ["shell_output", "shell_stop"] {
        assert!(tools.allowed_in_plan_mode(tool, r#"{"id":1}"#));
        let call = tools
            .prepare_named(tool, "manage".into(), r#"{"id":1}"#, &cancel)
            .await
            .unwrap();
        assert!(matches!(
            PermissionGate::default().evaluate_plan(&call, PermissionMode::AlwaysAsk),
            PermissionCheck::Execute(_)
        ));
    }
    assert!(!dir.path().join("forbidden").exists());
}
