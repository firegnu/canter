use canter_protocol::PermissionMode;
use canter_tools::{LocalExecutor, PermissionCheck, PermissionGate, Tool, ToolOutcome, Tools};
use serde_json::json;
use std::{collections::BTreeMap, path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;

fn tools_with(root: &Path, protected: Vec<std::path::PathBuf>, rules: Option<&Path>) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(
            root.into(),
            BTreeMap::from([("HOME".into(), root.to_string_lossy().into_owned())]),
            protected,
        )
        .unwrap(),
    ))
    .with_file_rules(rules.map(Path::to_path_buf))
}

fn tools(root: &Path) -> Tools {
    tools_with(root, Vec::new(), None)
}

async fn search(tools: &Tools, arguments: serde_json::Value) -> canter_tools::ToolOutput {
    let call = tools
        .prepare(
            Tool::from_name("ast_grep").unwrap(),
            "ast",
            &arguments.to_string(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    tools
        .execute(
            match PermissionGate::default().evaluate(&call, PermissionMode::AllowAll) {
                PermissionCheck::Execute(permit) => permit,
                other => panic!("ast_grep must be read-only: {other:?}"),
            },
            &CancellationToken::new(),
            &mut |_| {},
        )
        .await
}

fn item(output: &canter_tools::ToolOutput) -> serde_json::Value {
    serde_json::to_value(&output.item).unwrap()
}

fn write(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[tokio::test]
async fn finds_calls_by_structure_in_every_supported_language() {
    let root = tempfile::tempdir().unwrap();
    write(
        root.path(),
        "sample.rs",
        "fn main() {\n    // value.unwrap()\n    let text = \"value.unwrap()\";\n    let value = Some(1);\n    value.unwrap();\n}\n",
    );
    write(
        root.path(),
        "sample.py",
        "# value.unwrap()\ntext = \"value.unwrap()\"\nvalue.unwrap()\n",
    );
    write(
        root.path(),
        "sample.ts",
        "// value.unwrap()\nconst text = \"value.unwrap()\";\nvalue.unwrap();\n",
    );
    write(
        root.path(),
        "sample.tsx",
        "// value.unwrap()\nconst view = <p title=\"value.unwrap()\">x</p>;\nvalue.unwrap();\n",
    );
    write(
        root.path(),
        "sample.js",
        "// value.unwrap()\nconst text = \"value.unwrap()\";\nvalue.unwrap();\n",
    );
    write(
        root.path(),
        "sample.mjs",
        "// value.unwrap()\nvalue.unwrap();\n",
    );
    let output = search(&tools(root.path()), json!({"pattern":"$X.unwrap()"})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert!(output.text.contains("6 matches found."), "{}", output.text);
    for (path, line) in [
        ("sample.rs", 5),
        ("sample.py", 3),
        ("sample.ts", 3),
        ("sample.tsx", 3),
        ("sample.js", 3),
        ("sample.mjs", 2),
    ] {
        assert!(
            output
                .text
                .lines()
                .any(|row| row.starts_with(&format!("{path}:{line}:"))),
            "{path}:{line} missing from {}",
            output.text
        );
    }
    for noise in [
        "// value.unwrap()",
        "# value.unwrap()",
        "\"value.unwrap()\"",
        "title=\"value.unwrap()\"",
    ] {
        assert!(!output.text.contains(noise), "{noise}: {}", output.text);
    }
    let item = item(&output);
    assert_eq!(item["type"], "astGrep");
    assert_eq!(item["totalMatches"], 6);
    assert_eq!(item["matches"].as_array().unwrap().len(), 6);
    assert_eq!(item["matches"][0]["path"], "sample.js");
    assert_eq!(item["matches"][0]["line"], 3);
}

#[tokio::test]
async fn lang_restricts_files_and_path_accepts_one_file() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "one.rs", "value.unwrap();\n");
    write(root.path(), "one.py", "value.unwrap()\n");
    write(root.path(), "one.tsx", "value.unwrap();\n");
    let tools = tools(root.path());
    let python = search(&tools, json!({"pattern":"$X.unwrap()","lang":"python"})).await;
    assert!(python.text.contains("one.py:1:1"), "{}", python.text);
    assert!(!python.text.contains("one.rs"), "{}", python.text);
    assert!(!python.text.contains("one.tsx"), "{}", python.text);
    let file = search(
        &tools,
        json!({"pattern":"$X.unwrap()","path":"one.rs","lang":"rust"}),
    )
    .await;
    assert!(file.text.contains("one.rs:1:1"), "{}", file.text);
    assert!(!file.text.contains("one.py"), "{}", file.text);
    let unknown = tools
        .prepare(
            Tool::from_name("ast_grep").unwrap(),
            "ast",
            &json!({"pattern":"$X.unwrap()","lang":"ruby"}).to_string(),
            &CancellationToken::new(),
        )
        .await
        .err()
        .unwrap();
    assert!(unknown.text.contains("lang must be"), "{}", unknown.text);
}

#[tokio::test]
async fn invalid_patterns_fail_before_approval_with_the_parser_reason() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "one.rs", "value.unwrap();\n");
    let tools = tools(root.path());
    for (arguments, expected) in [
        (
            json!({"pattern":"let x = 1; let y = 2;","lang":"rust"}),
            "Invalid ast-grep pattern",
        ),
        (json!({"pattern":"   "}), "non-empty"),
        (
            json!({"pattern":"$X.unwrap()","extra":true}),
            "Invalid arguments",
        ),
    ] {
        let error = tools
            .prepare(
                Tool::from_name("ast_grep").unwrap(),
                "ast",
                &arguments.to_string(),
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.outcome, ToolOutcome::Failed);
        assert!(error.text.contains(expected), "{}", error.text);
        assert_eq!(item(&error)["type"], "astGrep");
    }
}

#[tokio::test]
async fn patterns_that_tree_sitter_recovers_with_errors_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "one.tsx", "const value = 123;\n");
    let tools = tools(root.path());
    for arguments in [
        json!({"pattern":"123+","lang":"tsx"}),
        json!({"pattern":"123+"}),
    ] {
        let error = tools
            .prepare(
                Tool::from_name("ast_grep").unwrap(),
                "ast",
                &arguments.to_string(),
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.outcome, ToolOutcome::Failed);
        assert!(
            error.text.contains("Invalid ast-grep pattern"),
            "{} {}",
            arguments,
            error.text
        );
    }
}

#[tokio::test]
async fn an_explicitly_approved_file_is_searched_even_when_a_prompt_rule_matches() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "asked.rs", "value.unwrap();\n");
    let rules = root.path().join("rules.json");
    std::fs::write(
        &rules,
        json!({"rules":[],"fileRules":[{"pattern":"asked.rs","decision":"prompt"}]}).to_string(),
    )
    .unwrap();
    let tools = tools_with(root.path(), Vec::new(), Some(&rules));
    let call = tools
        .prepare(
            Tool::from_name("ast_grep").unwrap(),
            "ast",
            &json!({"pattern":"$X.unwrap()","path":"asked.rs"}).to_string(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let permit = match PermissionGate::default()
        .with_rules_file(rules)
        .evaluate(&call, PermissionMode::AllowAll)
    {
        PermissionCheck::Execute(permit) => permit,
        other => panic!("an approved single file must be searchable: {other:?}"),
    };
    let output = tools
        .execute(permit, &CancellationToken::new(), &mut |_| {})
        .await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert!(output.text.contains("asked.rs:1:1"), "{}", output.text);
}

#[tokio::test]
async fn non_object_arguments_fail_and_a_null_path_uses_the_session_directory() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "one.rs", "value.unwrap();\n");
    let tools = tools(root.path());
    for arguments in [json!(["$X.unwrap()"]), json!("$X.unwrap()")] {
        let error = tools
            .prepare(
                Tool::from_name("ast_grep").unwrap(),
                "ast",
                &arguments.to_string(),
                &CancellationToken::new(),
            )
            .await
            .err()
            .unwrap();
        assert_eq!(error.outcome, ToolOutcome::Failed);
        assert!(error.text.contains("Invalid arguments"), "{}", error.text);
    }
    let call = tools
        .prepare(
            Tool::from_name("ast_grep").unwrap(),
            "ast",
            &json!({"pattern":"$X.unwrap()","path":null}).to_string(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let resolved = std::fs::canonicalize(root.path()).unwrap();
    assert_eq!(call.resolved_path(), Some(resolved.as_path()));
    let output = tools
        .execute(
            match PermissionGate::default().evaluate(&call, PermissionMode::AllowAll) {
                PermissionCheck::Execute(permit) => permit,
                other => panic!("{other:?}"),
            },
            &CancellationToken::new(),
            &mut |_| {},
        )
        .await;
    assert!(output.text.contains("one.rs:1:1"), "{}", output.text);
}

#[tokio::test]
async fn matches_page_at_fifty_and_report_the_total_and_next_offset() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "many.rs", &"value.unwrap();\n".repeat(55));
    let tools = tools(root.path());
    let first = search(&tools, json!({"pattern":"$X.unwrap()"})).await;
    assert_eq!(item(&first)["matches"].as_array().unwrap().len(), 50);
    assert_eq!(item(&first)["totalMatches"], 55);
    assert!(
        first
            .text
            .contains("55 matches total; showing 50 from offset 0. Next page: offset=50."),
        "{}",
        first.text
    );
    let second = search(&tools, json!({"pattern":"$X.unwrap()","offset":50})).await;
    assert_eq!(item(&second)["matches"].as_array().unwrap().len(), 5);
    assert!(second.text.contains("55 matches found."), "{}", second.text);
    assert!(
        second
            .text
            .lines()
            .any(|line| line.starts_with("many.rs:51:")),
        "{}",
        second.text
    );
    let beyond = search(&tools, json!({"pattern":"$X.unwrap()","offset":99})).await;
    assert!(
        beyond
            .text
            .contains("offset 99 is beyond the 55 matches found."),
        "{}",
        beyond.text
    );
}

#[tokio::test]
async fn directories_follow_gitignore_and_skip_binaries_and_unknown_files() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), ".gitignore", "ignored.rs\ngenerated/\n");
    write(root.path(), "kept.rs", "value.unwrap();\n");
    write(root.path(), "ignored.rs", "value.unwrap();\n");
    write(root.path(), "generated/code.rs", "value.unwrap();\n");
    write(root.path(), "notes.txt", "value.unwrap();\n");
    std::fs::write(root.path().join("data.rs"), b"value.unwrap();\0\n").unwrap();
    let output = search(&tools(root.path()), json!({"pattern":"$X.unwrap()"})).await;
    assert!(output.text.contains("kept.rs:1:1"), "{}", output.text);
    assert!(!output.text.contains("ignored.rs"), "{}", output.text);
    assert!(!output.text.contains("generated"), "{}", output.text);
    assert!(!output.text.contains("notes.txt"), "{}", output.text);
    assert!(!output.text.contains("data.rs"), "{}", output.text);
    assert!(output.text.contains("1 match found."), "{}", output.text);
}

#[tokio::test]
async fn file_rules_and_canter_private_files_are_skipped_inside_a_directory() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "public.rs", "value.unwrap();\n");
    write(root.path(), "blocked.rs", "value.unwrap();\n");
    write(root.path(), "asked.rs", "value.unwrap();\n");
    write(
        root.path(),
        "private/secret.rs",
        "SECRET_MARKER.unwrap();\n",
    );
    std::os::unix::fs::symlink(
        root.path().join("private/secret.rs"),
        root.path().join("linked.rs"),
    )
    .unwrap();
    write(root.path(), "linked-to-public.rs", "");
    std::fs::remove_file(root.path().join("linked-to-public.rs")).unwrap();
    std::os::unix::fs::symlink(
        root.path().join("public.rs"),
        root.path().join("linked-to-public.rs"),
    )
    .unwrap();
    let rules = root.path().join("rules.json");
    std::fs::write(
        &rules,
        json!({"rules":[],"fileRules":[
            {"pattern":"blocked.rs","decision":"forbidden"},
            {"pattern":"asked.rs","decision":"prompt"}
        ]})
        .to_string(),
    )
    .unwrap();
    let tools = tools_with(root.path(), vec![root.path().join("private")], Some(&rules));
    let output = search(&tools, json!({"pattern":"$X.unwrap()"})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert!(!output.text.contains("blocked.rs"), "{}", output.text);
    assert!(!output.text.contains("asked.rs"), "{}", output.text);
    assert!(
        !output.text.contains("private/secret.rs"),
        "{}",
        output.text
    );
    assert!(!output.text.contains("SECRET_MARKER"), "{}", output.text);
    assert!(!output.text.contains("linked.rs"), "{}", output.text);
    assert!(output.text.contains("public.rs:1:1"), "{}", output.text);
    assert!(
        output.text.contains("linked-to-public.rs:1:1"),
        "{}",
        output.text
    );
    assert!(
        output
            .text
            .contains("Skipped 4 files that file rules block"),
        "{}",
        output.text
    );
    // The gate still checks the search path itself, exactly like read.
    let call = tools
        .prepare(
            Tool::from_name("ast_grep").unwrap(),
            "ast",
            &json!({"pattern":"$X.unwrap()","path":"blocked.rs"}).to_string(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        PermissionGate::default()
            .with_rules_file(rules.clone())
            .evaluate(&call, PermissionMode::AllowAll),
        PermissionCheck::Forbidden { .. }
    ));
}

#[tokio::test]
async fn unreadable_file_rules_fail_instead_of_searching_everything() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "one.rs", "value.unwrap();\n");
    let rules = root.path().join("rules.json");
    std::fs::create_dir(&rules).unwrap();
    let tools = tools_with(root.path(), Vec::new(), Some(&rules));
    let output = search(&tools, json!({"pattern":"$X.unwrap()"})).await;
    assert_eq!(output.outcome, ToolOutcome::Failed);
    assert!(
        output.text.contains("could not read the file rules"),
        "{}",
        output.text
    );
    assert_eq!(item(&output)["status"], "failed");
    assert!(
        item(&output)["error"]
            .as_str()
            .unwrap()
            .contains("No files")
    );
}

#[tokio::test]
async fn session_directory_through_a_symlink_still_shows_relative_paths() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "real/sample.rs", "value.unwrap();\n");
    std::os::unix::fs::symlink(root.path().join("real"), root.path().join("linked-cwd")).unwrap();
    // The session directory keeps the symlink spelling; the executor walks the
    // real directory, so matches must still be shown relative to the session.
    let output = search(
        &tools(&root.path().join("linked-cwd")),
        json!({ "pattern": "$X.unwrap()" }),
    )
    .await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert!(output.text.contains("sample.rs:1:1"), "{}", output.text);
    assert!(!output.text.contains("linked-cwd"), "{}", output.text);
    assert_eq!(item(&output)["matches"][0]["path"], "sample.rs");
}

#[tokio::test]
async fn plan_and_review_rounds_can_search_with_file_rules() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "kept.rs", "value.unwrap();\n");
    write(root.path(), ".env", "value.unwrap();\n");
    let rules = root.path().join("rules.json");
    std::fs::write(
        &rules,
        json!({"rules":[],"fileRules":[{"pattern":".env","decision":"forbidden"}]}).to_string(),
    )
    .unwrap();
    let gate = PermissionGate::default().with_rules_file(rules);
    let tools = tools(root.path());
    let directory = tools
        .prepare(
            Tool::from_name("ast_grep").unwrap(),
            "ast",
            &json!({"pattern":"$X.unwrap()"}).to_string(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let forbidden = tools
        .prepare(
            Tool::from_name("ast_grep").unwrap(),
            "ast",
            &json!({"pattern":"$X.unwrap()","path":".env"}).to_string(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    for mode in [
        PermissionMode::AlwaysAsk,
        PermissionMode::AskForExecution,
        PermissionMode::AllowAll,
    ] {
        assert!(matches!(
            gate.evaluate_plan(&directory, mode),
            PermissionCheck::Execute(_)
        ));
        assert!(matches!(
            gate.evaluate_review(&directory, mode),
            PermissionCheck::Execute(_)
        ));
        assert!(matches!(
            gate.evaluate(&directory, mode),
            PermissionCheck::Execute(_)
        ));
        assert!(matches!(
            gate.evaluate_plan(&forbidden, mode),
            PermissionCheck::Forbidden { .. }
        ));
        assert!(matches!(
            gate.evaluate_review(&forbidden, mode),
            PermissionCheck::Forbidden { .. }
        ));
    }
}

#[tokio::test]
async fn unparsed_files_are_reported_instead_of_looking_like_no_matches() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "broken.rs", "fn main( { value.unwrap();\n");
    let output = search(&tools(root.path()), json!({"pattern":"$X.unwrap()"})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed);
    assert!(
        output.text.contains("No matches for `$X.unwrap()`."),
        "{}",
        output.text
    );
    assert!(
        output
            .text
            .contains("1 file had syntax errors and was not searched."),
        "{}",
        output.text
    );
}

#[tokio::test]
async fn languages_where_the_pattern_does_not_parse_are_reported() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "one.rs", "fn main() { let value = 1; }\n");
    write(root.path(), "one.py", "value = 1\n");
    let output = search(&tools(root.path()), json!({"pattern":"let $A = $B;"})).await;
    assert_eq!(output.outcome, ToolOutcome::Completed, "{}", output.text);
    assert!(output.text.contains("one.rs:1:13"), "{}", output.text);
    assert!(
        output
            .text
            .contains("1 Python file not searched because the pattern does not parse as Python."),
        "{}",
        output.text
    );
}

#[tokio::test]
async fn cancellation_stops_before_reading_the_next_file() {
    let root = tempfile::tempdir().unwrap();
    write(root.path(), "one.rs", "value.unwrap();\n");
    let tools = tools(root.path());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let call = tools
        .prepare(
            Tool::from_name("ast_grep").unwrap(),
            "ast",
            &json!({"pattern":"$X.unwrap()"}).to_string(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let output = tools
        .execute(
            match PermissionGate::default().evaluate(&call, PermissionMode::AllowAll) {
                PermissionCheck::Execute(permit) => permit,
                other => panic!("{other:?}"),
            },
            &cancel,
            &mut |_| {},
        )
        .await;
    assert_eq!(output.outcome, ToolOutcome::Cancelled);
    assert_eq!(item(&output)["status"], "failed");
}
