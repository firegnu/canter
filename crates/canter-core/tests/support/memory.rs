use super::*;

struct Fixture {
    _temp: tempfile::TempDir,
    cwd: std::path::PathBuf,
    home: std::path::PathBuf,
    store: Arc<Store>,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let cwd = temp.path().join("project");
        let home = temp.path().join("state");
        std::fs::create_dir(&cwd).unwrap();
        let store = Arc::new(Store::new(home.join("sessions")).unwrap());
        Self {
            _temp: temp,
            cwd,
            home,
            store,
        }
    }
    fn runtime(&self, restricted: bool) -> RuntimeContext {
        RuntimeContext {
            memory_home: Some(self.home.clone()),
            restricted_mode: restricted,
            environment: BTreeMap::from([
                ("HOME".into(), self._temp.path().display().to_string()),
                ("CANTER_HOME".into(), self.home.display().to_string()),
            ]),
            protected_paths: vec![self.home.clone()],
            ..Default::default()
        }
    }
    async fn create(&self, id: &str, model: Arc<ScriptModel>, sandbox: bool) -> Session {
        let mut settings = settings(&self.cwd);
        settings.approval_policy = PermissionMode::AlwaysAsk;
        Session::create(
            self.store.clone(),
            id.into(),
            settings,
            self.runtime(false),
            model,
            CoreOptions {
                sandbox,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .0
    }
}

async fn send(session: &Session, message: &str) -> Vec<Event> {
    let mut events = session
        .subscribe(session.snapshot().await.event_seq)
        .await
        .unwrap();
    session.input(input(message)).await.unwrap();
    finished(&mut events).await
}

fn prefix(model: &ScriptModel, index: usize) -> String {
    serde_json::to_string(&model.requests.lock().unwrap()[index].messages[0]).unwrap()
}

#[tokio::test]
async fn memory_model_writes_and_edits_without_approval_and_only_new_openings_update_the_prefix() {
    for sandbox in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.home.join("memory/testing.md");
        let model = ScriptModel::new(vec![
            vec![call("remember", "write", &serde_json::json!({"path":path,"content":"# Testing\nUse fixtures.\n\nA lasting preference."}).to_string()), ModelEvent::Finished(FinishReason::ToolCalls)],
            vec![call("correct", "edit", &serde_json::json!({"path":path,"edits":[{"oldText":"Use fixtures.","newText":"Use isolated fixtures."}]}).to_string()), ModelEvent::Finished(FinishReason::ToolCalls)],
            vec![ModelEvent::Finished(FinishReason::Stop)],
            vec![ModelEvent::Finished(FinishReason::Stop)],
            vec![ModelEvent::Finished(FinishReason::Stop)],
            vec![ModelEvent::Finished(FinishReason::Stop)],
        ]);
        let session = fixture.create("source", model.clone(), sandbox).await;
        let events = send(
            &session,
            "Remember my testing preference and its correction.",
        )
        .await;
        assert!(
            !events
                .iter()
                .any(|event| matches!(event.payload, EventPayload::RequestOpened { .. }))
        );
        let changed = events
            .iter()
            .filter(|event| {
                matches!(
                    &event.payload,
                    EventPayload::ItemCompleted {
                        item: Item::FileChange {
                            status: ItemStatus::Completed,
                            ..
                        },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(changed, 2, "both existing write/edit cards must be present");
        assert_eq!(
            session.memories().await.unwrap().data[0].description,
            "Use isolated fixtures."
        );
        let before = prefix(&model, 0);
        assert!(before.contains("memory_instructions"));
        assert!(!before.contains("memory_index"));
        assert_eq!(before, prefix(&model, 1));
        assert_eq!(before, prefix(&model, 2));
        session
            .delete_memory(MemoryDeleteParams {
                scope: MemoryScope::Global,
                file: "testing.md".into(),
            })
            .await
            .unwrap();
        send(&session, "Continue after forgetting.").await;
        assert_eq!(before, prefix(&model, 3));
        std::fs::write(&path, "# Testing\nNew opening preference.\n\nDetails.").unwrap();
        let new = fixture.create("new", model.clone(), sandbox).await;
        send(&new, "What should you remember?").await;
        assert!(prefix(&model, 4).contains("New opening preference."));
        new.close().await.unwrap();
        session.close().await.unwrap();
        let resumed = Session::open(
            fixture.store.clone(),
            "source".into(),
            fixture.runtime(false),
            model.clone(),
            CoreOptions {
                sandbox,
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .0;
        send(&resumed, "Continue.").await;
        assert!(prefix(&model, 5).contains("New opening preference."));
        resumed.close().await.unwrap();
    }
}

#[tokio::test]
async fn memory_fork_keeps_the_snapshot_and_trust_changes_remove_or_reload_only_memory_segments() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.home.join("memory")).unwrap();
    let path = fixture.home.join("memory/testing.md");
    std::fs::write(&path, "# Testing\nOpening preference.\n\nDetails.").unwrap();
    let model = ScriptModel::new(
        (0..5)
            .map(|_| vec![ModelEvent::Finished(FinishReason::Stop)])
            .collect(),
    );
    let source = fixture.create("source", model.clone(), false).await;
    send(&source, "First.").await;
    std::fs::write(&path, "# Testing\nLater preference.\n\nDetails.").unwrap();
    let fork = source
        .fork_snapshot()
        .await
        .unwrap()
        .create(
            fixture.store.clone(),
            "fork".into(),
            fixture.runtime(false),
            model.clone(),
            CoreOptions::default(),
        )
        .await
        .unwrap()
        .0;
    send(&fork, "Fork.").await;
    assert_eq!(prefix(&model, 0), prefix(&model, 1));
    let restricted = source
        .fork_snapshot()
        .await
        .unwrap()
        .create(
            fixture.store.clone(),
            "restricted".into(),
            fixture.runtime(true),
            model.clone(),
            CoreOptions::default(),
        )
        .await
        .unwrap()
        .0;
    send(&restricted, "Restricted fork.").await;
    assert!(!prefix(&model, 2).contains("memory_instructions"));
    assert!(!prefix(&model, 2).contains("Opening preference."));
    assert!(restricted.memories().await.is_err());
    source.set_folder_trust(false).await.unwrap();
    send(&source, "Restricted resume.").await;
    assert!(!prefix(&model, 3).contains("memory_index"));
    source.set_folder_trust(true).await.unwrap();
    send(&source, "Trusted resume.").await;
    assert!(prefix(&model, 4).contains("Later preference."));
    for session in [fork, restricted, source] {
        session.close().await.unwrap();
    }
}

#[tokio::test]
async fn memory_failed_trust_upgrade_does_not_leave_directory_exceptions_enabled() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let model = ScriptModel::new(Vec::new());
    let session = fixture.create("restricted", model, true).await;
    session.set_folder_trust(false).await.unwrap();
    let path = fixture.home.join("memory/unreadable.md");
    std::fs::write(&path, "# Preference\nA lasting preference.\n\nDetails.").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    let result = session.set_folder_trust(true).await;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        result.is_err(),
        "unreadable memory prevents loading the new prefix"
    );
    assert_eq!(
        session.snapshot().await.session.settings.restricted_mode,
        Some(true)
    );
    assert!(session.memories().await.is_err());
    let memory_root = path.parent().unwrap().canonicalize().unwrap();
    assert!(
        !session
            .sandbox()
            .await
            .writable
            .contains(&memory_root.display().to_string()),
        "failed upgrade must revoke its memory exception"
    );
    session.set_folder_trust(true).await.unwrap();
    assert_eq!(session.memories().await.unwrap().data.len(), 1);
    session.close().await.unwrap();
}
