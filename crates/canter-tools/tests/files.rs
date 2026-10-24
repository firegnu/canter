use canter_protocol::{Item, ItemStatus};
use canter_tools::{LocalExecutor, Tool, ToolOutcome, Tools};
use std::{collections::BTreeMap, sync::Arc};
use tokio_util::sync::CancellationToken;

fn tools(dir: &tempfile::TempDir) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new()).unwrap(),
    ))
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
    let result = tools.execute(call, &cancel, &mut |_| {}).await;
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
    let result = tools.execute(call, &cancel, &mut |_| {}).await;
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
    let result = tools.execute(call, &cancel, &mut |_| {}).await;
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
    let result = tools.execute(call, &cancel, &mut |_| {}).await;
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
        let result = tools.execute(call, &cancel, &mut |_| {}).await;
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
        let result = tools.execute(call, &cancel, &mut |_| {}).await;
        assert_eq!(result.outcome, ToolOutcome::Cancelled, "{}", result.text);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "unchanged");
        assert!(!dir.path().join("marker").exists());
    }
}
