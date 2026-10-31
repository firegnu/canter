use canter_protocol::*;
use canter_store::Store;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::PermissionsExt,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ModelEntry {
    role: String,
    call_id: Option<String>,
    raw_arguments: Option<String>,
    content: String,
}

#[test]
fn private_typed_model_history_survives_reopen_and_a_torn_last_line() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::new(temp.path()).unwrap();
    let mut writer = store
        .create(
            "s1",
            SessionSettings {
                non_interactive: false,
                tags: Default::default(),
                additional_directories: Vec::new(),
                plan_mode: false,
                pre_plan_approval_policy: None,
                plan_enabled: None,
                sandbox_enabled: None,
                cwd: "/work".into(),
                name: None,
                model: "fake".into(),
                reasoning_effort: Default::default(),
                approval_policy: PermissionMode::AlwaysAsk,
            },
            1,
        )
        .unwrap();
    let entries = vec![
        ModelEntry {
            role: "assistant".into(),
            call_id: Some("call-fixture".into()),
            raw_arguments: Some("{invalid JSON passed to unknown_tool".into()),
            content: "model-only fixture".into(),
        },
        ModelEntry {
            role: "tool".into(),
            call_id: Some("call-fixture".into()),
            raw_arguments: None,
            content: "exact truncated result\nsecond line".into(),
        },
    ];
    for entry in &entries {
        writer.append_model_history(entry).unwrap();
    }
    assert_eq!(store.model_history::<ModelEntry>("s1").unwrap(), entries);
    assert_eq!(writer.snapshot().last_event_seq, 1);
    assert!(
        !serde_json::to_string(writer.snapshot())
            .unwrap()
            .contains("model-only fixture")
    );
    drop(writer);
    let path = temp.path().join("s1/model-history.jsonl");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"role\":\"\xe4\xb8")
        .unwrap();
    assert_eq!(store.model_history::<ModelEntry>("s1").unwrap(), entries);
    let mut writer = store.open("s1").unwrap();
    writer.append_model_history(&entries[0]).unwrap();
    drop(writer);
    let mut expected = entries.clone();
    expected.push(entries[0].clone());
    assert_eq!(store.model_history::<ModelEntry>("s1").unwrap(), expected);
    assert!(
        !fs::read_to_string(temp.path().join("s1/events.jsonl"))
            .unwrap()
            .contains("model-only fixture")
    );
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"invalid complete line\n")
        .unwrap();
    assert!(store.model_history::<ModelEntry>("s1").is_err());
}
