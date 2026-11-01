use super::*;

fn create(store: &Store, id: &str, timestamp: i64) -> SessionWriter {
    let mut settings = SessionSnapshot::empty(id).settings;
    settings.cwd = "/work".into();
    settings.name = Some(id.into());
    store.create(id, settings, timestamp).unwrap()
}

#[test]
fn unchanged_list_reads_no_history() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let writer = create(&store, "s1", 1);
    assert_eq!(store.list().unwrap(), vec![writer.snapshot().summary()]);
    READ_BYTES.with(|bytes| bytes.set(0));
    assert_eq!(store.list().unwrap(), vec![writer.snapshot().summary()]);
    assert_eq!(READ_BYTES.with(|bytes| bytes.get()), 0);
}

#[test]
fn repeated_catch_up_reads_only_new_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let mut writer = create(&store, "s1", 1);
    let mut reader = store.event_reader("s1").unwrap();
    assert_eq!(
        reader.read_after(0, 1).unwrap(),
        store.events_after("s1", 0).unwrap()
    );
    let path = temp.path().join("s1/events.jsonl");
    for seq in 2..=4 {
        let before = fs::metadata(&path).unwrap().len();
        let event = writer
            .append(
                EventPayload::Warning {
                    message: "new".into(),
                },
                seq,
            )
            .unwrap();
        let added = fs::metadata(&path).unwrap().len() - before;
        READ_BYTES.with(|bytes| bytes.set(0));
        assert_eq!(
            reader.read_after(seq as u64 - 1, seq as u64).unwrap(),
            vec![event]
        );
        assert_eq!(READ_BYTES.with(|bytes| bytes.get()), added);
    }
}

// The original list implementation: replay every file, with no cache.
fn original_list(store: &Store) -> Vec<SessionSummary> {
    let mut sessions = Vec::new();
    for entry in fs::read_dir(&store.root).unwrap() {
        let entry = entry.unwrap();
        if !entry.file_type().unwrap().is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let Ok(dir) = store.session_dir(&id) else {
            continue;
        };
        let Ok(file) = File::open(dir.join("events.jsonl")) else {
            continue;
        };
        let Ok((snapshot, _)) = replay(&file, &id) else {
            continue;
        };
        if snapshot.last_event_seq > 0 {
            sessions.push(snapshot.summary());
        }
    }
    sessions.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.cmp(&b.id))
    });
    sessions
}

fn warning() -> EventPayload {
    EventPayload::Warning {
        message: "new".into(),
    }
}

#[test]
fn cached_list_matches_full_replay_for_changes_bad_empty_and_removed_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let mut a = create(&store, "a", 10);
    drop(create(&store, "b", 10));
    for id in [
        "bad-json",
        "wrong-id",
        "gap",
        "bad-reducer",
        "empty",
        "partial",
    ] {
        drop(create(&store, id, 1));
        let path = temp.path().join(id).join("events.jsonl");
        let bytes = match id {
            "bad-json" => b"bad JSON\n".to_vec(),
            "empty" => vec![],
            "partial" => b"{\"unfinished\":\"\xe4\xb8".to_vec(),
            _ => {
                let event = Event {
                    session_id: if id == "wrong-id" { "other" } else { id }.into(),
                    seq: if id == "gap" { 2 } else { 1 },
                    timestamp: 1,
                    payload: if id == "bad-reducer" {
                        EventPayload::TurnRetracted {
                            turn_id: "missing".into(),
                        }
                    } else {
                        warning()
                    },
                };
                let mut bytes = serde_json::to_vec(&event).unwrap();
                bytes.push(b'\n');
                bytes
            }
        };
        fs::write(path, bytes).unwrap();
    }
    assert_eq!(store.list().unwrap(), original_list(&store));
    READ_BYTES.with(|bytes| bytes.set(0));
    assert_eq!(
        store
            .list()
            .unwrap()
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(
        READ_BYTES.with(|bytes| bytes.get()),
        0,
        "bad and empty histories are unchanged too"
    );
    a.append(
        EventPayload::SessionStatus {
            status: canter_protocol::SessionStatus::Working,
            close_reason: None,
        },
        20,
    )
    .unwrap();
    let mut settings = a.snapshot().settings.clone();
    settings.cwd = "/changed".into();
    settings.name = Some("renamed".into());
    a.append(EventPayload::SessionUpdated { settings }, 21)
        .unwrap();
    assert_eq!(store.list().unwrap(), original_list(&store));
    drop(create(&store, "new", 30));
    assert_eq!(store.list().unwrap(), original_list(&store));
    fs::remove_dir_all(temp.path().join("b")).unwrap();
    assert_eq!(store.list().unwrap(), original_list(&store));
    assert!(!store.summaries.lock().unwrap().contains_key("b"));
    // Repaired histories must stop being negatively cached.
    let repaired = Event {
        session_id: "bad-json".into(),
        seq: 1,
        timestamp: 40,
        payload: warning(),
    };
    let mut bytes = serde_json::to_vec(&repaired).unwrap();
    bytes.push(b'\n');
    fs::write(temp.path().join("bad-json/events.jsonl"), bytes).unwrap();
    assert_eq!(store.list().unwrap(), original_list(&store));
    let restarted = Store::new(temp.path()).unwrap();
    READ_BYTES.with(|bytes| bytes.set(0));
    assert_eq!(restarted.list().unwrap(), store.list().unwrap());
    assert!(READ_BYTES.with(|bytes| bytes.get()) > 0);
}

#[test]
fn readers_invalidate_replaced_rewritten_and_truncated_files() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    drop(create(&store, "s1", 1));
    let path = temp.path().join("s1/events.jsonl");
    let mut reader = store.event_reader("s1").unwrap();
    let original = reader.read_after(0, u64::MAX).unwrap();
    let summary = store.list().unwrap();
    let old_time = fs::metadata(&path).unwrap().modified().unwrap();
    let mut changed = original[0].clone();
    changed.timestamp = 2;
    let mut bytes = serde_json::to_vec(&changed).unwrap();
    bytes.push(b'\n');
    assert_eq!(bytes.len() as u64, fs::metadata(&path).unwrap().len());
    fs::write(&path, &bytes).unwrap();
    File::open(&path).unwrap().set_modified(old_time).unwrap();
    assert_ne!(
        store.list().unwrap(),
        summary,
        "ctime invalidates a same-size rewrite with restored mtime"
    );
    assert_eq!(store.list().unwrap(), original_list(&store));
    assert!(reader.read_after(1, 1).unwrap().is_empty());
    changed.timestamp = 3;
    let replacement = path.with_extension("replacement");
    let mut bytes = serde_json::to_vec(&changed).unwrap();
    bytes.push(b'\n');
    fs::write(&replacement, bytes).unwrap();
    fs::rename(replacement, &path).unwrap();
    assert_eq!(store.list().unwrap(), original_list(&store));
    assert_eq!(
        reader.read_after(0, 1).unwrap(),
        store.events_after("s1", 0).unwrap()
    );
    fs::write(&path, b"").unwrap();
    assert!(store.list().unwrap().is_empty());
    assert!(reader.read_after(1, 1).unwrap().is_empty());
    assert_eq!(reader.offset, 0);
}

#[test]
fn catch_up_preserves_unpublished_events_torn_tails_and_validation_errors() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let mut writer = create(&store, "s1", 1);
    let mut reader = store.event_reader("s1").unwrap();
    writer
        .append_batch((2..=4).map(|time| (warning(), time)))
        .unwrap();
    let all = store.events_after("s1", 0).unwrap();
    assert_eq!(reader.read_after(0, 2).unwrap(), all[..2]);
    assert_eq!(reader.read_after(2, 4).unwrap(), all[2..]);
    let path = temp.path().join("s1/events.jsonl");
    let intact = fs::metadata(&path).unwrap().len();
    let event = Event {
        session_id: "s1".into(),
        seq: 5,
        timestamp: 5,
        payload: warning(),
    };
    let mut line = serde_json::to_vec(&event).unwrap();
    let mut out = OpenOptions::new().append(true).open(&path).unwrap();
    out.write_all(&line).unwrap();
    assert!(reader.read_after(4, 5).unwrap().is_empty());
    assert_eq!(reader.offset, intact);
    out.write_all(b"\n").unwrap();
    assert_eq!(
        reader.read_after(4, 5).unwrap(),
        store.events_after("s1", 4).unwrap()
    );
    let committed = fs::metadata(&path).unwrap().len();
    for (id, seq) in [("wrong", 6), ("s1", 7)] {
        let invalid = Event {
            session_id: id.into(),
            seq,
            timestamp: 6,
            payload: warning(),
        };
        line = serde_json::to_vec(&invalid).unwrap();
        line.push(b'\n');
        out.write_all(&line).unwrap();
        assert_eq!(
            reader.read_after(5, 6).unwrap_err().to_string(),
            store.events_after("s1", 5).unwrap_err().to_string()
        );
        assert_eq!(reader.offset, committed);
        out.set_len(committed).unwrap();
    }
    out.write_all(b"bad JSON\n").unwrap();
    assert!(reader.read_after(5, 6).is_err());
    out.set_len(committed).unwrap();
    assert!(reader.read_after(5, 6).unwrap().is_empty());
    // Reopening a writer truncates an incomplete tail, preserving the checkpoint.
    out.write_all(b"{\"partial\":\"\xe4\xb8").unwrap();
    assert!(reader.read_after(5, 6).unwrap().is_empty());
    drop(writer);
    let mut writer = store.open("s1").unwrap();
    let event = writer.append(warning(), 6).unwrap();
    assert_eq!(reader.read_after(5, 6).unwrap(), vec![event]);
}

#[test]
fn concurrent_list_calls_and_appends_converge_to_full_replay() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let mut writer = create(&store, "s1", 1);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            let store = &store;
            scope.spawn(move || {
                for _ in 0..20 {
                    let list = store.list().unwrap();
                    assert_eq!(list.len(), 1);
                    assert_eq!(list[0].updated_at as u64, list[0].last_event_seq);
                }
            });
        }
        for time in 2..=10 {
            writer.append(warning(), time).unwrap();
        }
    });
    assert_eq!(store.list().unwrap(), original_list(&store));
}

#[test]
fn concurrent_lists_replay_each_changed_history_only_once() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let mut writer = create(&store, "s1", 1);
    writer
        .append(
            EventPayload::Warning {
                message: "x".repeat(2 * 1024 * 1024),
            },
            2,
        )
        .unwrap();
    let path = temp.path().join("s1/events.jsonl");
    for round in 0..2 {
        if round > 0 {
            writer.append(warning(), 3).unwrap();
        }
        let expected = writer.snapshot().summary();
        let start = std::sync::Barrier::new(8);
        let bytes: u64 = std::thread::scope(|scope| {
            let readers: Vec<_> = (0..8)
                .map(|_| {
                    let (store, expected, start) = (&store, &expected, &start);
                    scope.spawn(move || {
                        READ_BYTES.with(|bytes| bytes.set(0));
                        start.wait();
                        assert_eq!(store.list().unwrap(), vec![expected.clone()]);
                        READ_BYTES.with(|bytes| bytes.get())
                    })
                })
                .collect();
            readers
                .into_iter()
                .map(|reader| reader.join().unwrap())
                .sum()
        });
        assert_eq!(
            bytes,
            fs::metadata(&path).unwrap().len(),
            "cold and invalidated caches must each replay only once"
        );
    }
}
