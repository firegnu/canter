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
#[cfg(target_os = "macos")]
async fn memory_failed_trust_upgrade_does_not_leave_directory_exceptions_enabled() {
    use std::os::{fd::AsRawFd, unix::fs::MetadataExt};
    struct RestoreWriter {
        target: i32,
        backup: i32,
    }
    impl Drop for RestoreWriter {
        fn drop(&mut self) {
            // Restore only this fixture's descriptor, including when an assertion panics.
            unsafe {
                nix::libc::dup2(self.backup, self.target);
                nix::libc::close(self.backup);
            }
        }
    }
    for log in ["model-history.jsonl", "events.jsonl"] {
        let fixture = Fixture::new();
        let session = fixture
            .create("restricted", ScriptModel::new(Vec::new()), true)
            .await;
        session.set_folder_trust(false).await.unwrap();
        let path = fixture.store.session_dir("restricted").unwrap().join(log);
        let metadata = std::fs::metadata(&path).unwrap();
        // Fault the public storage boundary without adding production test hooks:
        // the already-open writer gets a read-only descriptor for the same isolated file.
        let target = std::fs::read_dir("/dev/fd")
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let fd: i32 = entry.file_name().to_str()?.parse().ok()?;
                let mut opened = std::mem::MaybeUninit::<nix::libc::stat>::uninit();
                if unsafe { nix::libc::fstat(fd, opened.as_mut_ptr()) } != 0 {
                    return None;
                }
                let opened = unsafe { opened.assume_init() };
                let flags = unsafe { nix::libc::fcntl(fd, nix::libc::F_GETFL) };
                (opened.st_dev as u64 == metadata.dev()
                    && opened.st_ino == metadata.ino()
                    && flags >= 0
                    && flags & nix::libc::O_ACCMODE != nix::libc::O_RDONLY)
                    .then_some(fd)
            })
            .next()
            .expect("the session log has an open writer");
        let backup = unsafe { nix::libc::dup(target) };
        assert!(backup >= 0);
        let restore = RestoreWriter { target, backup };
        let readonly = std::fs::File::open(&path).unwrap();
        assert_eq!(
            unsafe { nix::libc::dup2(readonly.as_raw_fd(), target) },
            target
        );
        let result = session.set_folder_trust(true).await;
        drop(restore);
        assert!(result.is_err(), "a journal write must actually fail");
        if log == "events.jsonl" {
            let records = fixture
                .store
                .model_history::<serde_json::Value>("restricted")
                .unwrap();
            assert!(
                records
                    .iter()
                    .rev()
                    .find(|entry| entry["type"] == "context")
                    .unwrap()["messages"][0]["text"]
                    .as_str()
                    .unwrap()
                    .contains("<memory_instructions>"),
                "the prefix write must have succeeded before SessionUpdated failed"
            );
        }
        assert_eq!(
            session.snapshot().await.session.settings.restricted_mode,
            Some(true)
        );
        assert!(session.memories().await.is_err());
        assert!(
            !session
                .context()
                .await
                .prefix
                .iter()
                .any(|section| section.kind == ContextSectionKind::Memory)
        );
        let home = fixture.home.canonicalize().unwrap();
        assert!(
            !session
                .sandbox()
                .await
                .writable
                .iter()
                .any(|path| std::path::Path::new(path).starts_with(&home)),
            "{log}: failed upgrade must revoke both memory exceptions"
        );
        session.set_folder_trust(false).await.unwrap();
        assert!(
            !session
                .sandbox()
                .await
                .writable
                .iter()
                .any(|path| std::path::Path::new(path).starts_with(&home)),
            "restricted reopen must not keep stale exceptions"
        );
        assert!(
            session.close().await.is_err(),
            "the failed journal requires reopening"
        );
    }
}

#[tokio::test]
async fn memory_trust_upgrade_warns_and_continues_when_memory_is_unavailable() {
    use std::os::unix::fs::PermissionsExt;
    for unreadable_file in [true, false] {
        let fixture = Fixture::new();
        let session = fixture
            .create("restricted", ScriptModel::new(Vec::new()), true)
            .await;
        session.set_folder_trust(false).await.unwrap();
        let root = fixture.home.join("memory");
        let file = root.join("unreadable.md");
        if unreadable_file {
            std::fs::write(&file, "# Preference\nA lasting preference.\n\nDetails.").unwrap();
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        } else {
            std::fs::remove_dir(&root).unwrap();
            std::fs::write(&root, "Not a directory.").unwrap();
        }
        let cursor = session.snapshot().await.event_seq;
        session
            .set_folder_trust(true)
            .await
            .expect("memory failure must not block a trust upgrade");
        assert_eq!(
            session.snapshot().await.session.settings.restricted_mode,
            Some(false)
        );
        assert!(fixture.store.events_after("restricted", cursor).unwrap().iter().any(|event| matches!(&event.payload, EventPayload::Warning { message } if message.to_lowercase().contains("memory"))), "the opening client must receive a memory warning");
        if unreadable_file {
            assert_eq!(
                session.memories().await.unwrap().data[0].name,
                "unreadable.md"
            );
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        } else {
            assert!(session.memories().await.is_err());
            assert!(
                !session
                    .context()
                    .await
                    .prefix
                    .iter()
                    .any(|section| section.kind == ContextSectionKind::Memory)
            );
            let home = fixture.home.canonicalize().unwrap();
            assert!(
                !session
                    .sandbox()
                    .await
                    .writable
                    .iter()
                    .any(|path| std::path::Path::new(path).starts_with(&home))
            );
            std::fs::remove_file(&root).unwrap();
        }
        session.set_folder_trust(false).await.unwrap();
        session.set_folder_trust(true).await.unwrap();
        assert!(
            session.memories().await.is_ok(),
            "the next trusted open retries memory"
        );
        session.close().await.unwrap();
    }
}
