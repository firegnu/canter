use canter_protocol::*;
use canter_store::{SessionWriter, Store};
use std::{
    fs::{self, OpenOptions},
    io::Write,
};

fn create(store: &Store, id: &str) -> SessionWriter {
    store
        .create(
            id,
            SessionSettings {
                cwd: "/work/project".into(),
                name: Some(id.into()),
                model: "fake".into(),
                reasoning_effort: Default::default(),
                approval_policy: PermissionMode::AlwaysAsk,
            },
            1,
        )
        .unwrap()
}
fn status(status: SessionStatus) -> EventPayload {
    EventPayload::SessionStatus {
        status,
        close_reason: None,
    }
}

#[test]
fn readers_can_resume_without_a_writer_lock_and_ignore_only_the_torn_tail() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let mut writer = create(&store, "s1");
    writer.append(status(SessionStatus::Idle), 2).unwrap();
    writer.append(status(SessionStatus::Working), 3).unwrap();
    let events = store.events_after("s1", 1).unwrap();
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![2, 3]
    );
    assert!(store.events_after("s1", 3).unwrap().is_empty());
    assert!(store.events_after("s1", 100).unwrap().is_empty());
    OpenOptions::new()
        .append(true)
        .open(temp.path().join("s1/events.jsonl"))
        .unwrap()
        .write_all(b"{\"seq\":4,\"partial\":\"\xe4\xb8")
        .unwrap();
    assert_eq!(store.events_after("s1", 1).unwrap(), events);
    drop(writer);
    assert_eq!(store.open("s1").unwrap().snapshot().last_event_seq, 3);
    OpenOptions::new()
        .append(true)
        .open(temp.path().join("s1/events.jsonl"))
        .unwrap()
        .write_all(b"not JSON\n")
        .unwrap();
    assert!(
        store.events_after("s1", 1).is_err(),
        "complete bad lines must not be skipped"
    );
}

#[test]
fn a_batch_is_prevalidated_and_commits_contiguous_events_together() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let mut writer = create(&store, "s1");
    let before = writer.snapshot().clone();
    let path = temp.path().join("s1/events.jsonl");
    let disk_before = fs::read(&path).unwrap();
    assert!(
        writer
            .append_batch([
                (status(SessionStatus::Idle), 2),
                (
                    EventPayload::ItemStarted {
                        turn_id: "missing".into(),
                        item: Item::AgentMessage {
                            id: "a1".into(),
                            text: "text".into()
                        }
                    },
                    3
                ),
            ])
            .is_err()
    );
    assert_eq!(writer.snapshot(), &before);
    assert_eq!(fs::read(&path).unwrap(), disk_before);
    let events = writer
        .append_batch([
            (
                EventPayload::TurnStarted {
                    turn: Turn {
                        id: "t1".into(),
                        items: Vec::new(),
                        status: TurnStatus::InProgress,
                        error: None,
                        input: Vec::new(),
                        client_name: "client".into(),
                        usage: None,
                    },
                    input_id: None,
                },
                4,
            ),
            (
                EventPayload::ItemStarted {
                    turn_id: "t1".into(),
                    item: Item::AgentMessage {
                        id: "a1".into(),
                        text: String::new(),
                    },
                },
                5,
            ),
            (
                EventPayload::ItemDelta {
                    turn_id: "t1".into(),
                    item_id: "a1".into(),
                    delta: ItemDelta::AgentMessage {
                        delta: "batched".into(),
                    },
                },
                6,
            ),
        ])
        .unwrap();
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![2, 3, 4]
    );
    assert_eq!(store.events_after("s1", 1).unwrap(), events);
    assert!(writer.append_batch([]).unwrap().is_empty());
    let expected = writer.snapshot().clone();
    drop(writer);
    assert_eq!(store.open("s1").unwrap().snapshot(), &expected);
    assert!(
        matches!(&expected.turns[0].items[0], Item::AgentMessage {text, ..} if text == "batched")
    );
}

#[test]
fn listing_skips_broken_sessions_and_unrelated_directory_names() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let healthy = create(&store, "healthy");
    let expected = healthy.snapshot().summary();
    drop(create(&store, "broken"));
    OpenOptions::new()
        .append(true)
        .open(temp.path().join("broken/events.jsonl"))
        .unwrap()
        .write_all(b"broken JSON\n")
        .unwrap();
    for name in [".tmp-backup", "s1.bak", "incomplete"] {
        fs::create_dir(temp.path().join(name)).unwrap();
    }
    assert_eq!(store.list().unwrap(), vec![expected]);
}

#[test]
fn a_session_without_a_committed_first_event_is_not_openable() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    drop(create(&store, "s1"));
    for tail in [b"".as_slice(), b"{\"seq\":1"] {
        fs::write(temp.path().join("s1/events.jsonl"), tail).unwrap();
        assert!(
            matches!(store.open("s1"), Err(canter_store::StoreError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
        );
        assert!(store.list().unwrap().is_empty());
    }
}
