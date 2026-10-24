mod common;
use canter_protocol::{Item, ItemStatus};
use canter_tools::{LocalExecutor, Tool, ToolOutcome, Tools};
use common::allow;
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

fn tools(dir: &tempfile::TempDir) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ))
}

async fn invoke(tools: &Tools, tool: Tool, args: serde_json::Value) -> canter_tools::ToolOutput {
    let cancel = CancellationToken::new();
    match tools
        .prepare(tool, "file", &args.to_string(), &cancel)
        .await
    {
        Ok(call) => tools.execute(allow(&call), &cancel, &mut |_| {}).await,
        Err(error) => error,
    }
}

#[tokio::test]
async fn new_file_permissions_follow_umask_and_existing_modes_are_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    std::fs::write(dir.path().join("ordinary"), "ordinary").unwrap();
    let result = invoke(
        &tools,
        Tool::Write,
        serde_json::json!({"path":"created", "content":"created"}),
    )
    .await;
    assert_eq!(result.outcome, ToolOutcome::Completed);
    let mode = |name: &str| {
        std::fs::metadata(dir.path().join(name))
            .unwrap()
            .permissions()
            .mode()
            & 0o777
    };
    assert_eq!(mode("created"), mode("ordinary"));
    std::fs::set_permissions(
        dir.path().join("created"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let result = invoke(
        &tools,
        Tool::Edit,
        serde_json::json!({"path":"created", "edits":[{"oldText":"created","newText":"edited"}]}),
    )
    .await;
    assert_eq!(result.outcome, ToolOutcome::Completed);
    assert_eq!(mode("created"), 0o755);
}

#[tokio::test]
async fn unsafe_file_dangling_symlink_is_rejected_without_replacing_it() {
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("missing/target", dir.path().join("link")).unwrap();
    let result = invoke(
        &tools(&dir),
        Tool::Write,
        serde_json::json!({"path":"link","content":"new"}),
    )
    .await;
    assert_eq!(result.outcome, ToolOutcome::Failed);
    assert!(
        result.text.contains("dangling symbolic link"),
        "{}",
        result.text
    );
    assert!(
        std::fs::symlink_metadata(dir.path().join("link"))
            .unwrap()
            .is_symlink()
    );
    assert!(!dir.path().join("missing").exists());
}

#[tokio::test]
async fn unsafe_file_readonly_files_are_not_overwritten() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("readonly");
    std::fs::write(&path, "original").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    for (tool, args) in [
        (
            Tool::Write,
            serde_json::json!({"path":"readonly","content":"new"}),
        ),
        (
            Tool::Edit,
            serde_json::json!({"path":"readonly","edits":[{"oldText":"original","newText":"new"}]}),
        ),
    ] {
        let result = invoke(&tools(&dir), tool, args).await;
        assert_eq!(result.outcome, ToolOutcome::Failed);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o444
        );
    }
}

#[tokio::test]
async fn approval_shows_resolved_target_for_symlinks_parent_segments_and_new_files() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("real")).unwrap();
    std::fs::write(dir.path().join("real/file"), "original").unwrap();
    std::os::unix::fs::symlink("real", dir.path().join("alias")).unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    for path in ["alias/file", "real/../real/file", "alias/new/deep/file"] {
        let args = serde_json::json!({"path":path,"content":"changed"});
        let call = tools
            .prepare(Tool::Write, "resolved", &args.to_string(), &cancel)
            .await
            .unwrap();
        let expected = if path.ends_with("deep/file") {
            dir.path()
                .canonicalize()
                .unwrap()
                .join("real/new/deep/file")
        } else {
            dir.path().join("real/file").canonicalize().unwrap()
        };
        assert_eq!(call.approval_arguments()["path"], path);
        assert_eq!(
            call.approval_arguments()["resolvedPath"],
            expected.to_string_lossy().as_ref()
        );
    }
}

#[tokio::test]
async fn protected_paths_reject_reads_writes_edits_and_symlink_aliases() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("state")).unwrap();
    std::fs::write(dir.path().join("state/private.json"), "private fixture").unwrap();
    std::fs::write(dir.path().join("config"), "private fixture").unwrap();
    std::os::unix::fs::symlink("state", dir.path().join("alias")).unwrap();
    std::os::unix::fs::symlink("config", dir.path().join("config-alias")).unwrap();
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(
            dir.path().into(),
            BTreeMap::new(),
            vec![
                dir.path().join("state"),
                dir.path().join("config"),
                dir.path().join("future-state"),
            ],
        )
        .unwrap(),
    ));
    for path in [
        "state/private.json",
        "alias/private.json",
        "state/../state/private.json",
        "config-alias",
        "future-state/new",
    ] {
        for (tool, args) in [
            (Tool::Read, serde_json::json!({"path":path})),
            (
                Tool::Write,
                serde_json::json!({"path":path,"content":"changed"}),
            ),
            (
                Tool::Edit,
                serde_json::json!({"path":path,"edits":[{"oldText":"private fixture","newText":"changed"}]}),
            ),
        ] {
            let result = invoke(&tools, tool, args).await;
            assert_eq!(result.outcome, ToolOutcome::Failed, "{tool:?} {path}");
            assert!(
                result.text.contains("这是 canter 的私有文件"),
                "{}",
                result.text
            );
            assert!(!result.text.contains("private fixture"));
        }
    }
    assert_eq!(
        std::fs::read_to_string(dir.path().join("state/private.json")).unwrap(),
        "private fixture"
    );
    assert!(!dir.path().join("future-state").exists());
    let allowed = invoke(
        &tools,
        Tool::Write,
        serde_json::json!({"path":"state-other/file","content":"allowed"}),
    )
    .await;
    assert_eq!(allowed.outcome, ToolOutcome::Completed);
}

#[tokio::test]
async fn lifecycle_items_preserve_paths_diffs_and_failure_context() {
    use canter_protocol::PatchChangeKind;
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Write,
            "new",
            r#"{"path":"new","content":"created"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let started = call.started_item();
    assert!(
        matches!(&started, Item::FileChange {changes, status:ItemStatus::InProgress, ..} if changes[0].path == "new" && changes[0].kind == PatchChangeKind::Add && changes[0].diff.contains("+created"))
    );
    assert!(
        matches!(call.declined_output().item, Item::FileChange {changes, status:ItemStatus::Declined, ..} if changes[0].diff.contains("+created"))
    );
    std::fs::write(dir.path().join("new"), "external edit").unwrap();
    let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert!(
        matches!(result.item, Item::FileChange {changes, status:ItemStatus::Failed, ..} if changes[0].path == "new" && changes[0].diff.contains("+created"))
    );
    let read = invoke(&tools, Tool::Read, serde_json::json!({"path":"missing"})).await;
    assert!(
        matches!(read.item, Item::FileRead {path, content, status:ItemStatus::Failed, ..} if path == "missing" && content.is_empty())
    );
    let shell = invoke(
        &tools,
        Tool::Shell,
        serde_json::json!({"command":"echo no", "timeoutMs":0}),
    )
    .await;
    assert!(
        matches!(shell.item, Item::CommandExecution {cwd, command, ..} if cwd == dir.path().to_string_lossy() && command == "echo no")
    );
    let edit = invoke(
        &tools,
        Tool::Edit,
        serde_json::json!({"path":"new","edits":[{"oldText":"absent","newText":"x"}]}),
    )
    .await;
    assert!(
        matches!(edit.item, Item::FileChange {changes, status:ItemStatus::Failed, ..} if changes[0].path == "new")
    );
    for (tool, raw) in [
        (Tool::Read, r#"{"path":"new"}"#),
        (Tool::Shell, r#"{"command":"echo hello"}"#),
    ] {
        let call = tools.prepare(tool, "started", raw, &cancel).await.unwrap();
        match call.started_item() {
            Item::FileRead {
                content,
                status: ItemStatus::InProgress,
                ..
            } => assert!(content.is_empty()),
            Item::CommandExecution {
                cwd,
                status: ItemStatus::InProgress,
                ..
            } => assert_eq!(cwd, dir.path().to_string_lossy()),
            _ => panic!("incorrect started item"),
        }
        assert_eq!(call.declined_output().outcome, ToolOutcome::Declined);
    }
}

#[tokio::test]
async fn retargeting_a_symlink_after_approval_cannot_change_the_execution_target() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("first"), "same contents").unwrap();
    std::fs::write(dir.path().join("second"), "same contents").unwrap();
    std::os::unix::fs::symlink("first", dir.path().join("link")).unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Write,
            "target",
            r#"{"path":"link","content":"modified"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let permit = allow(&call);
    std::fs::remove_file(dir.path().join("link")).unwrap();
    std::os::unix::fs::symlink("second", dir.path().join("link")).unwrap();
    let result = tools.execute(permit, &cancel, &mut |_| {}).await;
    assert_eq!(result.outcome, ToolOutcome::Failed);
    assert!(result.text.contains("target changed"));
    for path in ["first", "second"] {
        assert_eq!(
            std::fs::read_to_string(dir.path().join(path)).unwrap(),
            "same contents"
        );
    }
}

#[tokio::test]
async fn a_call_cannot_execute_in_a_different_session_executor() {
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let first_tools = tools(&first);
    let second_tools = tools(&second);
    let cancel = CancellationToken::new();
    let call = first_tools
        .prepare(
            Tool::Shell,
            "foreign",
            r#"{"command":"echo wrong > marker"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let result = second_tools
        .execute(allow(&call), &cancel, &mut |_| {})
        .await;
    assert_eq!(result.outcome, ToolOutcome::Failed);
    assert!(result.text.contains("different executor"));
    assert!(!first.path().join("marker").exists());
    assert!(!second.path().join("marker").exists());
}

#[tokio::test]
async fn large_file_changes_return_a_summary_and_bounded_client_diff() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let content = format!("{}\n", "new data ".repeat(100)).repeat(2000);
    let result = invoke(
        &tools,
        Tool::Write,
        serde_json::json!({"path":"large","content":content}),
    )
    .await;
    assert_eq!(result.outcome, ToolOutcome::Completed);
    assert!(
        result.text.len() < 1024,
        "model should receive a summary, not the whole diff"
    );
    assert!(result.text.contains("2000 lines added"));
    let Item::FileChange { changes, .. } = result.item else {
        panic!("wrong item")
    };
    assert!(changes[0].diff.len() <= 1024 * 1024);
    assert!(changes[0].diff.contains("[truncated "));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("large")).unwrap(),
        content
    );
}

#[tokio::test(flavor = "current_thread")]
async fn expensive_diff_does_not_block_runtime_or_prevent_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let before: String = (0..25_000).map(|n| format!("old line {n}\n")).collect();
    let after: String = (0..25_000).map(|n| format!("new line {n}\n")).collect();
    std::fs::write(dir.path().join("large"), &before).unwrap();
    let raw = serde_json::json!({"path":"large","content":after}).to_string();
    let cancel = CancellationToken::new();
    let started = std::time::Instant::now();
    let prepare = tools.prepare(Tool::Write, "diff", &raw, &cancel);
    let cancel_later = async {
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(prepare, cancel_later);
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("synchronous diff blocked cancellation"),
    };
    assert_eq!(error.outcome, ToolOutcome::Cancelled);
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("large")).unwrap(),
        before
    );
}

#[tokio::test]
async fn read_beyond_eof_reports_total_lines() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    for (contents, total) in [("first\nsecond", 2), ("", 0)] {
        std::fs::write(dir.path().join("short"), contents).unwrap();
        let result = invoke(
            &tools,
            Tool::Read,
            serde_json::json!({"path":"short","startLine":8}),
        )
        .await;
        assert_eq!(result.outcome, ToolOutcome::Completed);
        assert!(result.text.contains("beyond end of file"));
        assert!(
            result.text.contains(&format!("{total} lines")),
            "{}",
            result.text
        );
    }
}

#[tokio::test]
async fn write_creates_parents_and_returns_the_approved_diff() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let raw = r#"{ "path": "nested/notes", "content": "hello\n世界\n" }"#;
    let call = tools
        .prepare(Tool::Write, "w1", raw, &cancel)
        .await
        .unwrap();
    assert_eq!(call.raw_arguments(), raw);
    let diff = call.approval_arguments()["diff"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(diff.contains("+世界"));
    assert!(!dir.path().join("nested").exists());
    let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert_eq!(result.outcome, ToolOutcome::Completed, "{}", result.text);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("nested/notes")).unwrap(),
        "hello\n世界\n"
    );
    assert!(
        matches!(result.item, Item::FileChange { changes, status:ItemStatus::Completed, .. } if changes[0].diff == diff)
    );
    let call = tools
        .prepare(
            Tool::Write,
            "w2",
            r#"{"path":"nested/notes","content":"replacement\n"}"#,
            &cancel,
        )
        .await
        .unwrap();
    let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert_eq!(result.outcome, ToolOutcome::Completed);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("nested/notes")).unwrap(),
        "replacement\n"
    );
}

#[tokio::test]
async fn read_returns_numbered_requested_lines_and_a_final_item() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("notes"), "one\n二\nthree\nfour\n").unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(
            Tool::Read,
            "r1",
            r#"{"path":"notes","startLine":2,"lineCount":2}"#,
            &cancel,
        )
        .await
        .unwrap();
    let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert_eq!(result.outcome, ToolOutcome::Completed, "{}", result.text);
    assert!(result.text.contains("2: 二\n3: three\n"), "{}", result.text);
    assert!(result.text.contains("startLine=4"), "{}", result.text);
    assert!(
        matches!(result.item, Item::FileRead { id, path, start_line:2, status:ItemStatus::Completed, .. } if id == "r1" && path == "notes")
    );
}

#[tokio::test]
async fn edit_validates_all_replacements_against_the_original_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes");
    std::fs::write(&path, "alpha beta gamma\nrepeat repeat\naaa\n").unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    let raw = r#"{"path":"notes","edits":[{"oldText":"alpha","newText":"beta"},{"oldText":"beta","newText":"BETA"}]}"#;
    let call = tools.prepare(Tool::Edit, "e1", raw, &cancel).await.unwrap();
    let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert_eq!(result.outcome, ToolOutcome::Completed, "{}", result.text);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "beta BETA gamma\nrepeat repeat\naaa\n"
    );
    let original = std::fs::read(&path).unwrap();
    for (needle, problem) in [
        ("missing", "not found"),
        ("repeat", "multiple"),
        ("aa", "multiple"),
    ] {
        let raw = serde_json::json!({"path":"notes","edits":[
            {"oldText":"gamma","newText":"must not appear"},
            {"oldText":needle,"newText":"wrong"}
        ]})
        .to_string();
        let error = match tools.prepare(Tool::Edit, "e2", &raw, &cancel).await {
            Err(error) => error,
            Ok(_) => panic!("invalid edit should be rejected"),
        };
        assert!(error.text.contains("edit 2"), "{}", error.text);
        assert!(error.text.contains(problem), "{}", error.text);
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}

#[tokio::test]
async fn invalid_json_or_missing_fields_never_mutate_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("notes");
    std::fs::write(&path, "unchanged").unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    for tool in [Tool::Read, Tool::Write, Tool::Edit, Tool::Shell] {
        for raw in [
            r#"{"path":"notes","content":"changed","command":"touch marker""#,
            "{}",
        ] {
            let error = match tools.prepare(tool, "bad", raw, &cancel).await {
                Err(error) => error,
                Ok(_) => panic!("invalid arguments must be rejected"),
            };
            assert_eq!(error.outcome, ToolOutcome::Failed);
            assert!(error.text.contains("Invalid"), "{}", error.text);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "unchanged");
            assert!(!dir.path().join("marker").exists());
        }
    }
}

#[tokio::test]
async fn read_caps_lines_and_bytes_and_rejects_binary() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let cancel = CancellationToken::new();
    for (name, bytes) in [
        ("lines", "line\n".repeat(2_005).into_bytes()),
        (
            "bytes",
            format!("{}\n", "界".repeat(500)).repeat(100).into_bytes(),
        ),
        ("nul", b"binary\0data".to_vec()),
        ("non-utf8", vec![0xff, 0xfe, 0x00]),
    ] {
        std::fs::write(dir.path().join(name), bytes).unwrap();
        let raw = serde_json::json!({"path":name,"lineCount":5000}).to_string();
        let call = tools
            .prepare(Tool::Read, "read", &raw, &cancel)
            .await
            .unwrap();
        let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
        match name {
            "lines" => {
                assert_eq!(result.outcome, ToolOutcome::Completed);
                assert!(result.text.contains("2000: line\n"));
                assert!(!result.text.contains("2001: line"));
                assert!(result.text.contains("startLine=2001"));
            }
            "bytes" => {
                assert_eq!(result.outcome, ToolOutcome::Completed);
                let (content, notice) = result.text.split_once("\nByte limit reached:").unwrap();
                assert!(content.len() <= canter_tools::MAX_READ_BYTES);
                assert!(notice.contains("startLine="));
                assert!(!result.text.contains('\u{fffd}'));
            }
            _ => {
                assert_eq!(result.outcome, ToolOutcome::Failed);
                assert!(result.text.contains("binary"));
            }
        }
    }
}

#[tokio::test]
async fn every_tool_honors_cancellation_before_execution() {
    let dir = tempfile::tempdir().unwrap();
    let tools = tools(&dir);
    let path = dir.path().join("notes");
    std::fs::write(&path, "unchanged").unwrap();
    for (tool, raw) in [
        (Tool::Read, r#"{"path":"notes"}"#),
        (Tool::Write, r#"{"path":"notes","content":"changed"}"#),
        (
            Tool::Edit,
            r#"{"path":"notes","edits":[{"oldText":"unchanged","newText":"changed"}]}"#,
        ),
        (Tool::Shell, r#"{"command":"touch marker"}"#),
    ] {
        let cancel = CancellationToken::new();
        let call = tools.prepare(tool, "cancel", raw, &cancel).await.unwrap();
        cancel.cancel();
        let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
        assert_eq!(result.outcome, ToolOutcome::Cancelled, "{}", result.text);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "unchanged");
        assert!(!dir.path().join("marker").exists());
    }
}
