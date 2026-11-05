use canter_protocol::{ApprovalDecision, Item, PatchChangeKind, PermissionMode};
use canter_tools::{
    CommandHandle, Executor, FileReader, LocalExecutor, PermissionCheck, PermissionGate,
    PreparedCall, Tool, ToolOutcome, Tools,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

fn tools(dir: &tempfile::TempDir) -> Tools {
    Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
    ))
    .with_apply_patch(true)
}

fn allow(call: &PreparedCall) -> canter_tools::ExecutionPermit<'_> {
    match PermissionGate::default().evaluate(call, PermissionMode::AllowAll) {
        PermissionCheck::Execute(permit) => permit,
        _ => panic!("allowAll must issue a permit"),
    }
}

async fn invoke(tools: &Tools, patch: &str) -> canter_tools::ToolOutput {
    let cancel = CancellationToken::new();
    match tools
        .prepare(Tool::ApplyPatch, "patch", patch, &cancel)
        .await
    {
        Ok(call) => tools.execute(allow(&call), &cancel, &mut |_| {}).await,
        Err(error) => error,
    }
}

#[tokio::test]
async fn codex_scenario_fixtures_match_the_expected_files() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/apply_patch");
    for entry in std::fs::read_dir(root).unwrap() {
        let directory = entry.unwrap().path();
        if !directory.is_dir() {
            continue;
        }
        let temp = tempfile::tempdir().unwrap();
        copy_files(&directory.join("input"), temp.path());
        let patch = std::fs::read_to_string(directory.join("patch.txt")).unwrap();
        invoke(&tools(&temp), &patch).await;
        assert_eq!(
            snapshot(temp.path()),
            snapshot(&directory.join("expected")),
            "{}",
            directory.display()
        );
    }
}

#[tokio::test]
async fn one_call_prepares_all_diffs_and_rejects_bad_patches_before_writing() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("update.txt"), "old\n").unwrap();
    std::fs::write(temp.path().join("delete.txt"), "gone\n").unwrap();
    std::fs::write(temp.path().join("old.txt"), "old\n").unwrap();
    let patch = "*** Begin Patch\n*** Add File: nested/new.txt\n+new\n*** Update File: update.txt\n@@\n-old\n+new\n*** Delete File: delete.txt\n*** Update File: old.txt\n*** Move to: moved.txt\n@@\n-old\n+new\n*** End Patch";
    let tools = tools(&temp);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::ApplyPatch, "all", patch, &cancel)
        .await
        .unwrap();
    assert_eq!(call.file_checkpoints().len(), 5);
    assert_eq!(
        call.approval_arguments()["changes"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    let Item::FileChange { changes, .. } = call.started_item() else {
        panic!("file change")
    };
    assert!(changes[2].kind == PatchChangeKind::Delete);
    assert!(
        matches!(&changes[3].kind, PatchChangeKind::Update { move_path:Some(path) } if path == "moved.txt")
    );
    match PermissionGate::default().evaluate(&call, PermissionMode::AlwaysAsk) {
        PermissionCheck::Ask(request) => {
            assert_eq!(request.arguments["changes"].as_array().unwrap().len(), 4)
        }
        _ => panic!("one approval for the whole patch"),
    }
    let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert_eq!(result.outcome, ToolOutcome::Completed, "{}", result.text);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("nested/new.txt")).unwrap(),
        "new\n"
    );
    assert!(!temp.path().join("delete.txt").exists());
    assert!(!temp.path().join("old.txt").exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("moved.txt")).unwrap(),
        "new\n"
    );

    std::fs::write(temp.path().join("untouched.txt"), "same\n").unwrap();
    for invalid in [
        "not a patch",
        "*** Begin Patch\n*** Update File: untouched.txt\n@@\n-absent\n+new\n*** End Patch",
        "*** Begin Patch\n*** Add File: duplicate.txt\n+one\n*** Add File: duplicate.txt\n+two\n*** End Patch",
        "*** Begin Patch\n*** Environment ID: remote\n*** Add File: remote.txt\n+no\n*** End Patch",
    ] {
        let result = invoke(&tools, invalid).await;
        assert_eq!(result.outcome, ToolOutcome::Failed);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("untouched.txt")).unwrap(),
            "same\n"
        );
        assert!(!temp.path().join("duplicate.txt").exists());
        assert!(!temp.path().join("remote.txt").exists());
    }
}

#[tokio::test]
async fn protected_paths_both_move_ends_and_new_parents_are_checked() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("state")).unwrap();
    std::fs::write(temp.path().join("state/private"), "private fixture").unwrap();
    std::fs::write(temp.path().join("source"), "source\n").unwrap();
    let protected = temp.path().join("state");
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(temp.path().into(), BTreeMap::new(), vec![protected]).unwrap(),
    ))
    .with_apply_patch(true);
    for patch in [
        "*** Begin Patch\n*** Delete File: state/private\n*** End Patch",
        "*** Begin Patch\n*** Update File: state/private\n*** Move to: moved\n@@\n-private fixture\n+changed\n*** End Patch",
        "*** Begin Patch\n*** Update File: source\n*** Move to: state/moved\n@@\n-source\n+changed\n*** End Patch",
        "*** Begin Patch\n*** Add File: state/new/file\n+no\n*** End Patch",
    ] {
        let result = invoke(&tools, patch).await;
        assert_eq!(result.outcome, ToolOutcome::Failed, "{}", result.text);
        assert!(!result.text.contains("private fixture"));
    }
    assert_eq!(
        std::fs::read_to_string(temp.path().join("state/private")).unwrap(),
        "private fixture"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("source")).unwrap(),
        "source\n"
    );
}

#[tokio::test]
async fn outside_workspace_and_plan_mode_follow_existing_write_rules() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let patch = format!(
        "*** Begin Patch\n*** Add File: {}/new.txt\n+no\n*** End Patch",
        outside.path().display()
    );
    let tools = tools(&workspace);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::ApplyPatch, "outside", &patch, &cancel)
        .await
        .unwrap();
    assert!(matches!(
        PermissionGate::default().evaluate(&call, PermissionMode::AskForExecution),
        PermissionCheck::Ask(_)
    ));
    assert!(!tools.allowed_in_plan_mode("apply_patch", &patch));
    assert!(tools.find("write").is_none() && tools.find("edit").is_none());
    assert!(tools.find("apply_patch").is_some());
    assert!(!outside.path().join("new.txt").exists());
}

#[tokio::test]
async fn forbidden_rule_after_prompt_rule_blocks_the_whole_patch_and_accept() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("README.md"), "old\n").unwrap();
    std::fs::create_dir(temp.path().join("secrets")).unwrap();
    let rules = temp.path().join("rules.json");
    std::fs::write(
        rules,
        r#"{"rules":[],"fileRules":[
            {"pattern":"*.md","decision":"prompt"},
            {"pattern":"secrets/**","decision":"forbidden"}
        ]}"#,
    )
    .unwrap();
    let tools = tools(&temp);
    let patch = "*** Begin Patch\n*** Update File: README.md\n@@\n-old\n+new\n*** Add File: secrets/x\n+secret\n*** End Patch";
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::ApplyPatch, "rules", patch, &cancel)
        .await
        .unwrap();
    let mut gate = PermissionGate::default().with_rules_file(temp.path().join("rules.json"));
    assert!(matches!(
        gate.evaluate(&call, PermissionMode::AlwaysAsk),
        PermissionCheck::Forbidden { .. }
    ));
    assert!(matches!(
        gate.resolve(&call, ApprovalDecision::Accept, None),
        Ok(PermissionCheck::Forbidden { .. })
    ));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("README.md")).unwrap(),
        "old\n"
    );
    assert!(!temp.path().join("secrets/x").exists());
}

#[tokio::test]
async fn approved_content_change_aborts_every_write() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("first"), "first\n").unwrap();
    std::fs::write(temp.path().join("second"), "second\n").unwrap();
    let patch = "*** Begin Patch\n*** Update File: first\n@@\n-first\n+changed\n*** Update File: second\n@@\n-second\n+changed\n*** End Patch";
    let tools = tools(&temp);
    let cancel = CancellationToken::new();
    let call = tools
        .prepare(Tool::ApplyPatch, "changed", patch, &cancel)
        .await
        .unwrap();
    std::fs::write(temp.path().join("second"), "external\n").unwrap();
    let result = tools.execute(allow(&call), &cancel, &mut |_| {}).await;
    assert_eq!(result.outcome, ToolOutcome::Failed);
    assert!(result.text.contains("file changed since preparation"));
    assert_eq!(
        std::fs::read_to_string(temp.path().join("first")).unwrap(),
        "first\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("second")).unwrap(),
        "external\n"
    );
}

#[tokio::test]
async fn second_write_failure_reports_committed_and_uncommitted_files() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("first"), "first\n").unwrap();
    std::fs::write(temp.path().join("second"), "second\n").unwrap();
    let executor = Arc::new(FailSecondWrite::new(&temp));
    let tools = Tools::new(executor.clone()).with_apply_patch(true);
    let patch = "*** Begin Patch\n*** Update File: first\n@@\n-first\n+changed\n*** Update File: second\n@@\n-second\n+changed\n*** End Patch";
    let result = invoke(&tools, patch).await;
    assert_eq!(result.outcome, ToolOutcome::Failed);
    assert!(
        result.text.contains("Applied and left in place:"),
        "{}",
        result.text
    );
    assert!(result.text.contains("first"), "{}", result.text);
    assert!(result.text.contains("Not applied:"), "{}", result.text);
    assert!(result.text.contains("second"), "{}", result.text);
    assert_eq!(
        std::fs::read_to_string(temp.path().join("first")).unwrap(),
        "changed\n"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("second")).unwrap(),
        "second\n"
    );
}

struct FailSecondWrite {
    local: LocalExecutor,
}

impl FailSecondWrite {
    fn new(dir: &tempfile::TempDir) -> Self {
        Self {
            local: LocalExecutor::new(dir.path().into(), BTreeMap::new(), vec![]).unwrap(),
        }
    }
}

#[async_trait::async_trait]
impl Executor for FailSecondWrite {
    fn cwd(&self) -> &Path {
        self.local.cwd()
    }

    async fn resolve_path(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> std::io::Result<PathBuf> {
        self.local.resolve_path(path, cancel).await
    }

    async fn resolve_write_path(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> std::io::Result<PathBuf> {
        self.local.resolve_write_path(path, cancel).await
    }

    async fn open_file(
        &self,
        path: &Path,
        cancel: &CancellationToken,
    ) -> std::io::Result<FileReader> {
        self.local.open_file(path, cancel).await
    }

    async fn write_file(
        &self,
        path: &Path,
        original: Option<&[u8]>,
        content: &[u8],
        cancel: &CancellationToken,
    ) -> std::io::Result<()> {
        if path.file_name().is_some_and(|name| name == "second") {
            return Err(std::io::Error::other("injected second write failure"));
        }
        self.local.write_file(path, original, content, cancel).await
    }

    async fn delete_file(
        &self,
        path: &Path,
        original: &[u8],
        cancel: &CancellationToken,
    ) -> std::io::Result<()> {
        self.local.delete_file(path, original, cancel).await
    }

    async fn spawn(
        &self,
        _command: &str,
        _cancel: &CancellationToken,
    ) -> std::io::Result<CommandHandle> {
        Err(std::io::Error::other("not used"))
    }
}

fn copy_files(source: &Path, destination: &Path) {
    let Ok(entries) = std::fs::read_dir(source) else {
        return;
    };
    for entry in entries {
        let source = entry.unwrap().path();
        let destination = destination.join(source.strip_prefix(source.parent().unwrap()).unwrap());
        if source.is_dir() {
            std::fs::create_dir_all(&destination).unwrap();
            copy_files(&source, &destination);
        } else {
            std::fs::copy(source, destination).unwrap();
        }
    }
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut files = BTreeMap::new();
    collect_files(root, root, &mut files);
    files
}

fn collect_files(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_files(root, &path, files);
        } else {
            files.insert(
                path.strip_prefix(root).unwrap().into(),
                std::fs::read(path).unwrap(),
            );
        }
    }
}
