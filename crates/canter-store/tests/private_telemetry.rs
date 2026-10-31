use canter_protocol::*;
use canter_store::Store;
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
};

fn record() -> ModelRequestTelemetry {
    serde_json::from_value(json!({
        "requestId":"private-request", "purpose":"btw", "startedAt":100, "durationMs":20,
        "settings":{"model":"fake", "approvalPolicy":"allowAll", "planMode":false},
        "context":{"estimated":true,"systemTokens":1,"toolDefinitionTokens":2,"conversationTokens":3,"toolResultTokens":0}, "toolCallIds":[]
    })).unwrap()
}

#[test]
fn private_telemetry_does_not_parse_old_lines_or_poison_the_writer() {
    for corrupt in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::new(temp.path()).unwrap();
        let mut writer = store
            .create("s", SessionSnapshot::empty("s").settings, 1)
            .unwrap();
        let path = temp.path().join("s/private-telemetry.jsonl");
        if corrupt {
            fs::write(&path, b"\xfffuture-format\nincomplete").unwrap();
            writer.append_private_telemetry(&[record()]).unwrap();
            let bytes = fs::read(&path).unwrap();
            assert!(bytes.starts_with(b"\xfffuture-format\n{"));
            assert!(!bytes.windows(10).any(|part| part == b"incomplete"));
        } else {
            fs::create_dir(&path).unwrap();
            assert!(writer.append_private_telemetry(&[record()]).is_err());
            writer
                .append(
                    EventPayload::SessionStatus {
                        status: SessionStatus::Idle,
                        close_reason: None,
                    },
                    2,
                )
                .unwrap();
            fs::remove_dir(&path).unwrap();
            writer.append_private_telemetry(&[record()]).unwrap();
        }
        writer
            .append(
                EventPayload::SessionStatus {
                    status: SessionStatus::Idle,
                    close_reason: None,
                },
                3,
            )
            .unwrap();
    }
}

#[test]
fn private_telemetry_keeps_snapshots_order_and_events_unchanged_across_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let settings: SessionSettings = serde_json::from_value(json!({
        "cwd":"/fixture", "model":"fake", "approvalPolicy":"allowAll"
    }))
    .unwrap();
    let mut writer = store.create("original", settings.clone(), 10).unwrap();
    drop(store.create("newer", settings, 20).unwrap());
    let before = writer.snapshot().clone();
    let list = store.list().unwrap();
    let events = store.events_after("original", 0).unwrap();
    let path = temp.path().join("original/private-telemetry.jsonl");
    writer.append_private_telemetry(&[]).unwrap();
    assert!(!path.exists());
    let record: ModelRequestTelemetry = serde_json::from_value(json!({
        "requestId":"private-request", "purpose":"handoff", "startedAt":100,
        "durationMs":20, "settings":{"model":"fake", "approvalPolicy":"allowAll", "planMode":false},
        "context":{"estimated":true,"systemTokens":1,"toolDefinitionTokens":2,"conversationTokens":3,"toolResultTokens":0},
        "toolCallIds":[]
    })).unwrap();
    writer
        .append_private_telemetry(std::slice::from_ref(&record))
        .unwrap();
    assert_eq!(writer.snapshot(), &before);
    assert_eq!(store.list().unwrap(), list);
    assert_eq!(store.events_after("original", 0).unwrap(), events);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    drop(writer);
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"requestId\":\"")
        .unwrap();
    let mut writer = store.open("original").unwrap();
    assert_eq!(writer.snapshot(), &before);
    writer
        .append_private_telemetry(std::slice::from_ref(&record))
        .unwrap();
    assert_eq!(store.list().unwrap(), list);
    let records: Vec<ModelRequestTelemetry> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records, vec![record.clone(), record]);
    let next = writer
        .append(
            EventPayload::SessionStatus {
                status: SessionStatus::Idle,
                close_reason: None,
            },
            30,
        )
        .unwrap();
    assert_eq!(next.seq, before.last_event_seq + 1);
}
