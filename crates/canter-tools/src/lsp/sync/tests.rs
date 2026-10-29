use super::*;
use crate::lsp::test_support::*;
use serde_json::json;

fn setup(mode: &str) -> (tempfile::TempDir, Arc<LspService>, FileContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("project.json"), "{}").unwrap();
    let service = LspService::new(
        serde_json::from_value(json!({"enabled":true,"servers":{"fake":{
            "command":executable(),"extensions":{"fake":"fixture"},"root_markers":["project.json"]
        }}}))
        .unwrap(),
    )
    .unwrap();
    let context = FileContext {
        cwd: dir.path().into(),
        additional_directories: Vec::new(),
        environment: environment(dir.path(), mode),
        round: Round::Modify,
    };
    (dir, service, context)
}
#[tokio::test]
async fn successful_change_synchronizes_and_receives_diagnostics_after_wait_timeout() {
    let (dir, service, context) = setup("slow_sync");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad 😀\n").unwrap();
    let ticket = session.changed(&path);
    let timed_out = ticket.wait(Duration::from_millis(1)).await;
    let result = ticket.wait(Duration::from_secs(5)).await;
    service.shutdown().await;
    assert_eq!(timed_out.unwrap_err().kind, ErrorKind::Timeout);
    let result = result.unwrap();
    assert_eq!(result.version, Some(1));
    assert_eq!(result.diagnostics[0]["message"], "fixture error");
    let starts = std::fs::read_to_string(dir.path().join("starts")).unwrap();
    assert_eq!(starts.lines().count(), 1);
    for pid in starts.lines() {
        assert!(gone(pid.parse().unwrap()));
    }
}

#[tokio::test]
async fn shared_versions_empty_results_and_deletion_replace_previous_diagnostics() {
    let (dir, service, context) = setup("sync");
    let other = FileContext {
        cwd: context.cwd.clone(),
        additional_directories: vec![],
        environment: context.environment.clone(),
        round: Round::Modify,
    };
    let a = service.session(context, vec![]);
    let b = service.session(other, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad 😀\n").unwrap();
    let first = a.changed(&path).wait(Duration::from_secs(5)).await.unwrap();
    std::fs::write(&path, "good\n").unwrap();
    let second = b.changed(&path).wait(Duration::from_secs(5)).await.unwrap();
    std::fs::remove_file(&path).unwrap();
    let deleted = a.changed(&path).wait(Duration::from_secs(5)).await;
    service.shutdown().await;
    assert_eq!(first.generation, second.generation);
    assert_eq!(second.version, Some(2));
    assert!(second.diagnostics.is_empty());
    assert_eq!(deleted.unwrap().freshness, Freshness::Deleted);
    let pid = std::fs::read_to_string(dir.path().join("starts")).unwrap();
    let log = std::fs::read_to_string(dir.path().join(format!("{}.log", pid.trim()))).unwrap();
    assert!(log.contains("textDocument/didClose"));
    assert!(log.contains("textDocument/didSave"));
    assert!(log.contains(r#""end":{"character":0,"line":1}"#));
}

#[tokio::test]
async fn diagnostics_cache_only_contains_current_interested_files() {
    let (dir, service, context) = setup("sync");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let before = session.diagnostics().await;
    std::fs::write(&path, "good").unwrap();
    let after = session.diagnostics().await;
    service.shutdown().await;
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].diagnostics.len(), 1);
    assert!(
        after.is_empty(),
        "disk changed before its watcher event arrived"
    );
}

#[tokio::test]
async fn watcher_synchronizes_external_changes_and_ignores_build_outputs() {
    let (dir, service, context) = setup("sync");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let mut batches = session.subscribe();
    std::fs::write(&path, "good").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), batches.recv()).await;
    service.shutdown().await;
    let batch = result
        .expect("external changes must produce diagnostics")
        .unwrap();
    assert!(batch.diagnostics.is_empty());
    assert_eq!(batch.version, Some(2));
}

#[tokio::test]
async fn pull_diagnostics_are_associated_with_the_requested_document_version() {
    let (dir, service, context) = setup("sync_pull");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    let result = session.changed(&path).wait(Duration::from_secs(5)).await;
    service.shutdown().await;
    let batch = result.unwrap();
    assert_eq!(batch.source, "pull:fixture");
    assert_eq!(batch.freshness, Freshness::Current);
    assert_eq!(batch.version, Some(1));
    assert_eq!(batch.diagnostics.len(), 1);
}

#[tokio::test]
async fn file_tools_notify_only_after_success_and_restore_notifies_too() {
    use crate::{LocalExecutor, PermissionGate, Tool, Tools};
    use canter_protocol::PermissionMode;
    let (dir, service, context) = setup("sync");
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), context.environment.clone(), vec![]).unwrap(),
    ))
    .with_lsp(service.session(context, vec![]));
    let cancel = CancellationToken::new();
    let gate = PermissionGate::default();
    let path = canonical(dir.path()).join("main.fake");
    let failed = tools
        .prepare(
            Tool::Write,
            "failed",
            &json!({"path":"main.fake","content":"never written"}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    std::fs::write(&path, "external modification").unwrap();
    let crate::PermissionCheck::Execute(permit) = gate.evaluate(&failed, PermissionMode::AllowAll)
    else {
        panic!("expected permit");
    };
    let failed = tools.execute(permit, &cancel, &mut |_| {}).await;
    assert_eq!(failed.outcome, crate::ToolOutcome::Failed);
    assert!(failed.change_ticket.is_none());
    assert!(!dir.path().join("starts").exists());
    let prepared = tools
        .prepare(
            Tool::Write,
            "write",
            &json!({"path":"main.fake","content":"bad"}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    let crate::PermissionCheck::Execute(permit) =
        gate.evaluate(&prepared, PermissionMode::AllowAll)
    else {
        panic!("expected permit");
    };
    let output = tools.execute(permit, &cancel, &mut |_| {}).await;
    assert_eq!(output.outcome, crate::ToolOutcome::Completed);
    let first = output
        .change_ticket
        .unwrap()
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(first.version, Some(1));
    let prepared = tools
        .prepare(
            Tool::Edit,
            "edit",
            &json!({"path":"main.fake","edits":[{"oldText":"bad","newText":"good"}]}).to_string(),
            &cancel,
        )
        .await
        .unwrap();
    let crate::PermissionCheck::Execute(permit) =
        gate.evaluate(&prepared, PermissionMode::AllowAll)
    else {
        panic!("expected permit");
    };
    let output = tools.execute(permit, &cancel, &mut |_| {}).await;
    let edited = output
        .change_ticket
        .unwrap()
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    assert!(edited.diagnostics.is_empty());
    let mut stream = tools.lsp().unwrap().subscribe();
    tools.restore_file(&path, Some(b"bad")).await.unwrap();
    let restored = next_matching(&mut stream, |b| {
        b.freshness == Freshness::Current && !b.diagnostics.is_empty()
    })
    .await;
    assert!(restored.version > edited.version);
    tools.restore_file(&path, None).await.unwrap();
    next_matching(&mut stream, |b| b.freshness == Freshness::Deleted).await;
    service.shutdown().await;
}

async fn next_matching(
    stream: &mut DiagnosticSubscription,
    mut matches: impl FnMut(&DiagnosticBatch) -> bool,
) -> DiagnosticBatch {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let batch = stream.recv().await.unwrap();
            if matches(&batch) {
                return batch;
            }
        }
    })
    .await
    .expect("diagnostic batch timed out")
}
fn clone_context(context: &FileContext) -> FileContext {
    context.clone()
}
async fn publish(instance: &Instance, path: &Path, version: Option<i64>, diagnostics: Value) {
    let mut params = json!({"uri":uri(&canonical(path)).unwrap(),"diagnostics":diagnostics});
    if let Some(version) = version {
        params["version"] = json!(version);
    }
    instance
        .request(
            "fixture/publish",
            params,
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn unversioned_empty_push_is_unconfirmed_and_old_versions_are_discarded() {
    let (dir, service, context) = setup("sync_unversioned");
    let session = service.session(clone_context(&context), vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    let first = session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(first.freshness, Freshness::VersionUnconfirmed);
    std::fs::write(&path, "good").unwrap();
    let second = session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(second.freshness, Freshness::VersionUnconfirmed);
    assert!(second.diagnostics.is_empty());
    let instance = service
        .for_file(&path, &context, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    publish(&instance, &path, Some(1), json!([{"message":"obsolete"}])).await;
    publish(&instance, &path, Some(2), json!([{"message":"current"}])).await;
    let mut stream = session.subscribe();
    publish(&instance, &path, Some(1), json!([])).await;
    publish(&instance, &path, Some(2), json!([{"message":"barrier"}])).await;
    let batch = next_matching(&mut stream, |b| {
        b.diagnostics
            .first()
            .is_some_and(|d| d["message"] == "barrier")
    })
    .await;
    assert_eq!(batch.freshness, Freshness::Current);
    assert_eq!(
        session
            .diagnostics()
            .await
            .iter()
            .filter(|b| b.source == "push")
            .count(),
        1
    );
    service.shutdown().await;
}
#[tokio::test]
async fn watcher_handles_checkout_batches_rename_delete_and_configuration_restart() {
    let (dir, service, context) = setup("sync");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    let first = session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let mut stream = session.subscribe();
    let moved = dir.path().join("renamed.fake");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::write(dir.path().join("new.fake"), "bad").unwrap();
    next_matching(&mut stream, |b| {
        b.path == canonical(&moved) && b.freshness == Freshness::Current
    })
    .await;
    assert!(
        !session
            .diagnostics()
            .await
            .iter()
            .any(|b| b.path == canonical(&path) && b.freshness == Freshness::Current)
    );
    std::fs::write(dir.path().join("project.json"), "{\"changed\":true}").unwrap();
    let new = next_matching(&mut stream, |b| {
        b.path == canonical(&moved)
            && b.generation != first.generation
            && b.freshness == Freshness::Current
    })
    .await;
    assert!(new.generation > first.generation);
    assert!(
        session
            .diagnostics()
            .await
            .iter()
            .filter(|b| b.freshness == Freshness::Current)
            .all(|b| b.generation == new.generation)
    );
    service.shutdown().await;
}
#[tokio::test]
async fn pure_shell_does_not_start_and_active_shell_finishes_synchronize() {
    use crate::{LocalExecutor, Tools};
    let (dir, service, context) = setup("sync");
    let session = service.session(context.clone(), vec![]);
    let tools = Tools::new(Arc::new(
        LocalExecutor::new(dir.path().into(), context.environment, vec![]).unwrap(),
    ))
    .with_lsp(session.clone());
    let cancel = CancellationToken::new();
    let params = |command: &str| canter_protocol::SessionShellCommandParams {
        command: command.into(),
        timeout_ms: None,
    };
    let output = tools
        .user_shell(
            "shell1".into(),
            &params("printf bad > main.fake"),
            &cancel,
            &mut |_| {},
        )
        .await;
    assert_eq!(output.outcome, crate::ToolOutcome::Completed);
    assert!(!dir.path().join("starts").exists());
    let path = dir.path().join("main.fake");
    session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let mut stream = session.subscribe();
    tools
        .user_shell(
            "shell2".into(),
            &params("printf good > main.fake"),
            &cancel,
            &mut |_| {},
        )
        .await;
    next_matching(&mut stream, |b| {
        b.freshness == Freshness::Current && b.diagnostics.is_empty()
    })
    .await;
    tools
        .user_shell(
            "shell3".into(),
            &params("/bin/sleep 1; printf bad > main.fake &"),
            &cancel,
            &mut |_| {},
        )
        .await;
    next_matching(&mut stream, |b| {
        b.freshness == Freshness::Current && !b.diagnostics.is_empty()
    })
    .await;
    tools.background().stop_all().await;
    service.shutdown().await;
}
#[tokio::test]
async fn diagnostic_file_and_session_limits_are_visible_to_subscribers() {
    let (dir, service, context) = setup("sync");
    let session = service.session(context.clone(), vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let instance = service
        .for_file(&path, &context, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    let mut stream = session.subscribe();
    publish(
        &instance,
        &path,
        Some(1),
        json!([{"message":"x".repeat(FILE_DIAGNOSTICS_BYTES)}]),
    )
    .await;
    let batch = next_matching(&mut stream, |b| b.incomplete).await;
    assert!(batch.diagnostics.is_empty());
    assert!(batch.reason.unwrap().contains("byte limit"));
    let mut slow = session.subscribe();
    for i in 0..6 {
        let path = dir.path().join(format!("file{i}.fake"));
        std::fs::write(&path, "bad").unwrap();
        let changed = session
            .changed(&path)
            .wait(Duration::from_secs(5))
            .await
            .unwrap();
        publish(
            &instance,
            &path,
            changed.version,
            json!([{"message":"x".repeat(220_000)}]),
        )
        .await;
        next_matching(&mut stream, |b| {
            b.path == canonical(&path)
                && b.diagnostics
                    .first()
                    .is_some_and(|d| d["message"].as_str().is_some_and(|m| m.len() == 220_000))
        })
        .await;
    }
    assert!(slow.recv().await.unwrap().incomplete);
    let cached = session.diagnostics().await;
    assert!(cached.iter().any(|b| b.incomplete));
    assert!(
        cached
            .iter()
            .map(|b| serde_json::to_vec(&b.diagnostics).unwrap().len())
            .sum::<usize>()
            <= CACHE_BYTES
    );
    service.shutdown().await;
}
#[tokio::test]
async fn ignored_and_protected_paths_are_not_synchronized() {
    let (dir, service, context) = setup("sync");
    std::fs::create_dir(dir.path().join("target")).unwrap();
    std::fs::create_dir(dir.path().join("state")).unwrap();
    let session = service.session(context, vec![dir.path().join("state")]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    for folder in ["target", "state"] {
        let ignored = dir.path().join(folder).join("ignored.fake");
        std::fs::write(&ignored, "bad").unwrap();
        assert_eq!(
            session
                .changed(&ignored)
                .wait(Duration::from_secs(5))
                .await
                .unwrap_err()
                .kind,
            ErrorKind::Unsupported
        );
    }
    session.reconcile().await;
    let pid = std::fs::read_to_string(dir.path().join("starts")).unwrap();
    service.shutdown().await;
    let log = std::fs::read_to_string(dir.path().join(format!("{}.log", pid.trim()))).unwrap();
    assert!(!log.contains("ignored.fake"));
}

#[tokio::test]
async fn changing_document_cancels_pull_and_ignores_its_late_response() {
    let (dir, service, context) = setup("sync_pull_delay");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    let old = session.changed(&path);
    until(|| {
        std::fs::read_to_string(dir.path().join("starts"))
            .ok()
            .is_some_and(|starts| {
                std::fs::read_to_string(dir.path().join(format!("{}.log", starts.trim())))
                    .unwrap_or_default()
                    .contains("textDocument/diagnostic")
            })
    })
    .await;
    std::fs::write(&path, "good").unwrap();
    let result = session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(result.version, Some(2));
    assert!(result.diagnostics.is_empty());
    assert_eq!(
        old.wait(Duration::from_secs(1)).await.unwrap_err().kind,
        ErrorKind::Cancelled
    );
    service.shutdown().await;
    let pid = std::fs::read_to_string(dir.path().join("starts")).unwrap();
    assert!(
        std::fs::read_to_string(dir.path().join(format!("{}.log", pid.trim())))
            .unwrap()
            .contains("$/cancelRequest")
    );
}
#[tokio::test]
async fn pull_refresh_handles_unchanged_result_ids() {
    let (dir, service, context) = setup("sync_pull");
    let session = service.session(context.clone(), vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    let first = session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let mut stream = session.subscribe();
    let instance = service
        .for_file(&path, &context, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    instance
        .request(
            "fixture/refresh",
            json!({}),
            Duration::from_secs(5),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let refreshed = next_matching(&mut stream, |b| b.source == "pull:fixture").await;
    assert_eq!(refreshed.diagnostics, first.diagnostics);
    assert!(refreshed.id > first.id);
    service.shutdown().await;
}
#[tokio::test]
async fn registered_watch_rules_notify_non_source_files() {
    let (dir, service, context) = setup("sync_registered");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let pid = std::fs::read_to_string(dir.path().join("starts")).unwrap();
    let log = dir.path().join(format!("{}.log", pid.trim()));
    until(|| {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("\"id\":\"registration\",\"jsonrpc\":\"2.0\",\"result\":null")
    })
    .await;
    std::fs::write(dir.path().join("settings.cfg"), "changed").unwrap();
    until(|| {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .contains("settings.cfg")
    })
    .await;
    service.shutdown().await;
    assert!(
        std::fs::read_to_string(log)
            .unwrap()
            .contains("workspace/didChangeWatchedFiles")
    );
}
#[tokio::test]
async fn watcher_failure_reports_stale_and_rebuilds_with_a_new_generation() {
    let (dir, service, context) = setup("sync");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    let first = session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let mut stream = session.subscribe();
    let workspace = session.0.state.lock().unwrap().workspaces[&first.generation].clone();
    // Inject the OS watch backend's error at its callback boundary.
    workspace
        .watcher
        .event(Err(notify::Error::generic("fixture watcher failure")));
    let stale = next_matching(&mut stream, |b| b.freshness == Freshness::Stale).await;
    assert!(stale.incomplete);
    let recovered = next_matching(&mut stream, |b| {
        b.generation != first.generation && b.freshness == Freshness::Current
    })
    .await;
    assert_eq!(recovered.diagnostics.len(), 1);
    service.shutdown().await;
}
#[tokio::test]
async fn document_limit_closes_least_recently_used_and_reopen_increases_version() {
    let (dir, service, context) = setup("sync");
    let session = service.session(context, vec![]);
    let mut first_version = 0;
    for i in 0..65 {
        let path = dir.path().join(format!("file{i:02}.fake"));
        std::fs::write(&path, "bad").unwrap();
        let batch = session
            .changed(&path)
            .wait(Duration::from_secs(5))
            .await
            .unwrap();
        if i == 0 {
            first_version = batch.version.unwrap();
        }
    }
    assert!(
        session
            .diagnostics()
            .await
            .iter()
            .any(|b| b.incomplete && b.path.ends_with("file00.fake"))
    );
    let path = dir.path().join("file00.fake");
    std::fs::write(&path, "good").unwrap();
    let reopened = session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    assert!(reopened.version.unwrap() > first_version);
    service.shutdown().await;
    let pid = std::fs::read_to_string(dir.path().join("starts")).unwrap();
    let log = std::fs::read_to_string(dir.path().join(format!("{}.log", pid.trim()))).unwrap();
    let mut opened = BTreeSet::new();
    for line in log.lines().filter(|l| l.starts_with('{')) {
        let message: Value = serde_json::from_str(line).unwrap();
        match message["method"].as_str() {
            Some("textDocument/didOpen") => {
                opened.insert(
                    message["params"]["textDocument"]["uri"]
                        .as_str()
                        .unwrap()
                        .to_owned(),
                );
            }
            Some("textDocument/didClose") => {
                opened.remove(message["params"]["textDocument"]["uri"].as_str().unwrap());
            }
            _ => {}
        }
        assert!(opened.len() <= 64);
    }
}
#[tokio::test]
async fn dropping_the_session_releases_the_server_subscription() {
    let (dir, service, context) = setup("sync");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    let pid: u32 = std::fs::read_to_string(dir.path().join("starts"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    drop(session);
    until(|| gone(pid)).await;
    service.shutdown().await;
}

#[tokio::test]
async fn simultaneous_sessions_serialize_versions_and_deliver_latest_disk_content() {
    let (dir, service, context) = setup("sync");
    let a = service.session(context.clone(), vec![]);
    let b = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "initial").unwrap();
    a.changed(&path).wait(Duration::from_secs(5)).await.unwrap();
    let (left, right) = tokio::join!(
        async {
            tokio::fs::write(&path, "bad A").await.unwrap();
            a.changed(&path).wait(Duration::from_secs(5)).await
        },
        async {
            tokio::fs::write(&path, "good B").await.unwrap();
            b.changed(&path).wait(Duration::from_secs(5)).await
        }
    );
    for result in [left, right] {
        if let Err(error) = result {
            assert_eq!(error.kind, ErrorKind::Cancelled);
        }
    }
    std::fs::write(&path, "final").unwrap();
    let final_batch = a.changed(&path).wait(Duration::from_secs(5)).await.unwrap();
    assert!(final_batch.diagnostics.is_empty());
    let pid = std::fs::read_to_string(dir.path().join("starts")).unwrap();
    service.shutdown().await;
    let log = std::fs::read_to_string(dir.path().join(format!("{}.log", pid.trim()))).unwrap();
    let versions: Vec<_> = log
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|message| message["params"]["textDocument"]["version"].as_i64())
        .collect();
    assert!(versions.windows(2).all(|pair| pair[0] < pair[1]));
}

#[tokio::test]
async fn plan_and_review_pause_synchronization_and_resume_reconciles() {
    let (dir, service, context) = setup("sync");
    let session = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    session.set_round(Round::Plan);
    assert_eq!(
        session
            .changed(&path)
            .wait(Duration::from_secs(1))
            .await
            .unwrap_err()
            .kind,
        ErrorKind::Unsupported
    );
    assert!(!dir.path().join("starts").exists());
    session.set_round(Round::Modify);
    session
        .changed(&path)
        .wait(Duration::from_secs(5))
        .await
        .unwrap();
    session.set_round(Round::Review);
    std::fs::write(&path, "good").unwrap();
    session.reconcile().await;
    let pid = std::fs::read_to_string(dir.path().join("starts")).unwrap();
    let log = std::fs::read_to_string(dir.path().join(format!("{}.log", pid.trim()))).unwrap();
    assert!(!log.contains("textDocument/didChange"));
    let mut stream = session.subscribe();
    session.configure(vec![], Round::Modify);
    let batch = next_matching(&mut stream, |b| {
        b.freshness == Freshness::Current && b.diagnostics.is_empty()
    })
    .await;
    assert_eq!(batch.version, Some(2));
    service.shutdown().await;
}

#[tokio::test]
async fn stopping_one_session_subscription_preserves_a_shared_server_and_retry_resynchronizes() {
    let (dir, service, context) = setup("sync");
    let a = service.session(context.clone(), vec![]);
    let b = service.session(context, vec![]);
    let path = dir.path().join("main.fake");
    std::fs::write(&path, "bad").unwrap();
    a.changed(&path).wait(Duration::from_secs(5)).await.unwrap();
    b.changed(&path).wait(Duration::from_secs(5)).await.unwrap();
    let id = a.language_servers()[0].id.clone();
    assert_eq!(a.language_servers()[0].shared_references, 2);
    a.stop_server(&id).await.unwrap();
    assert!(a.diagnostics().await.is_empty());
    assert_eq!(
        b.language_servers()[0].state,
        canter_protocol::LanguageServerState::Running
    );
    assert_eq!(b.language_servers()[0].shared_references, 1);
    assert!(a.changed(&path).wait(Duration::from_secs(1)).await.is_err());
    a.retry_server(&id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !a.diagnostics().await.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(a.language_servers()[0].id, id);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("starts"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    a.close().await;
    b.close().await;
    service.shutdown().await;
}
