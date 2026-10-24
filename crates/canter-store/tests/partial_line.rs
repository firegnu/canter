use canter_protocol::*;
use canter_store::{PrivateSessionConfig, Store};
use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::Write;

#[test]
fn an_incomplete_last_line_is_discarded_before_further_appends() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let writer = store
        .create(
            "s1",
            SessionSettings {
                name: Some("intact".into()),
                model: "fake".into(),
                approval_policy: PermissionMode::AllowAll,
            },
            PrivateSessionConfig {
                cwd: temp.path().into(),
                env: BTreeMap::new(),
            },
            10,
        )
        .unwrap();
    let expected = writer.snapshot().clone();
    drop(writer);
    OpenOptions::new().append(true).open(temp.path().join("s1/events.jsonl")).unwrap()
        .write_all(b"{\"seq\":2,\"sessionId\":\"s1\",\"type\":\"session.status\",\"unfinished\":\"\xe4\xb8").unwrap();
    let mut writer = store
        .open("s1")
        .expect("a torn last line must not hide intact history");
    assert_eq!(writer.snapshot(), &expected);
    let event = writer
        .append(
            EventPayload::SessionStatus {
                status: SessionStatus::Idle,
            },
            12,
        )
        .unwrap();
    assert_eq!(event.seq, 2);
    let expected = writer.snapshot().clone();
    drop(writer);
    assert_eq!(store.open("s1").unwrap().snapshot(), &expected);
}
